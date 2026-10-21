//! Painting session content into a ratatui buffer (SPEC §11.4): the live screen from its cache,
//! or copy mode's frozen document with selection and search highlights.

use ptyro_core::cache::{Cell, ScreenCache};
use ptyro_core::screen::{Color, Style, StyleFlags};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color as RColor, Modifier, Style as RStyle};

use crate::copy::CopyMode;
use crate::theme::{Depth, Theme};

fn color(c: Color, depth: Depth) -> RColor {
    depth.fit(match c {
        Color::Default => RColor::Reset,
        Color::Indexed { i } => RColor::Indexed(i),
        Color::Rgb { r, g, b } => RColor::Rgb(r, g, b),
    })
}

pub fn style(s: &Style, depth: Depth, dim: bool) -> RStyle {
    let mut m = Modifier::empty();
    for (flag, md) in [
        (StyleFlags::BOLD, Modifier::BOLD),
        (StyleFlags::DIM, Modifier::DIM),
        (StyleFlags::ITALIC, Modifier::ITALIC),
        (StyleFlags::INVERSE, Modifier::REVERSED),
        (StyleFlags::HIDDEN, Modifier::HIDDEN),
        (StyleFlags::STRIKE, Modifier::CROSSED_OUT),
    ] {
        if s.flags.has(flag) {
            m |= md;
        }
    }
    if s.flags.has(StyleFlags::ANY_UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    if dim {
        m |= Modifier::DIM;
    }
    let mut out = RStyle::new().fg(color(s.fg, depth)).bg(color(s.bg, depth)).add_modifier(m);
    if let Some(ul) = s.ul {
        out = out.underline_color(color(ul, depth));
    }
    out
}

/// The top-left cell of the session shown in a viewport of `area`'s size. When the session is
/// bigger than the pane (another client controls its size, SPEC §5.8), pan so the cursor stays
/// in view.
pub fn origin(cache: &ScreenCache, area: Rect) -> (u16, u16) {
    let pan = |len: u16, view: u16, cursor: u16| -> u16 {
        if len <= view || cursor < view { 0 } else { (cursor + 1 - view).min(len - view) }
    };
    (
        pan(cache.size.cols, area.width, cache.cursor.x),
        pan(cache.size.rows, area.height, cache.cursor.y),
    )
}

fn put(buf: &mut Buffer, x: u16, y: u16, cell: &Cell, st: RStyle, last_col: bool) {
    let Some(out) = buf.cell_mut(Position::new(x, y)) else { return };
    if cell.is_spacer() {
        // ratatui skips the trailing half of a wide glyph when diffing.
        out.reset();
        return;
    }
    // A wide glyph cut by the pane edge would bleed outside it. The emulator keeps a tab in the
    // cell it jumped from (copy mode copies it); written out, the outer terminal would jump to
    // *its* tab stop.
    let text = if (cell.wide && last_col) || cell.text.starts_with(char::is_control) {
        " "
    } else {
        &cell.text
    };
    out.set_symbol(text);
    out.set_style(st);
}

/// The live screen. `dim` greys out an exited session's final screen.
pub fn screen(cache: &ScreenCache, buf: &mut Buffer, area: Rect, depth: Depth, dim: bool) {
    let (ox, oy) = origin(cache, area);
    for y in 0..area.height {
        let Some(row) = cache.rows.get((oy + y) as usize) else { break };
        for x in 0..area.width {
            let Some(cell) = row.get((ox + x) as usize) else { break };
            let st = style(&cache.style(cell.style), depth, dim);
            put(buf, area.x + x, area.y + y, cell, st, x + 1 == area.width);
        }
    }
}

pub fn cursor(cache: &ScreenCache, area: Rect) -> Option<Position> {
    let c = cache.cursor;
    let (ox, oy) = origin(cache, area);
    let (x, y) = (c.x.checked_sub(ox)?, c.y.checked_sub(oy)?);
    (c.visible && x < area.width && y < area.height).then(|| Position::new(area.x + x, area.y + y))
}

/// Copy mode: the document at its scroll position, selection, matches, prompt rows and cursor.
pub fn copy(c: &CopyMode, buf: &mut Buffer, area: Rect, theme: &Theme, depth: Depth) {
    let selected = RStyle::new()
        .fg(depth.fit(theme.selection_fg))
        .bg(depth.fit(theme.selection_bg))
        .add_modifier(if theme.monochrome { Modifier::REVERSED } else { Modifier::empty() });
    let found = RStyle::new().fg(RColor::Black).bg(depth.fit(theme.search));
    for y in 0..area.height {
        let r = c.top + y as usize;
        let Some(row) = c.row(r) else { break };
        for x in 0..area.width {
            let col = x as usize;
            let Some(cell) = row.get(col) else { break };
            let mut st = style(&c.style(cell.style), depth, false);
            if let Some(current) = c.match_at(r, col) {
                st = if current {
                    found.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    found
                };
            }
            if c.is_selected(r, col) {
                st = selected;
            }
            if r == c.cursor.row && col == c.cursor.col {
                st = st.add_modifier(Modifier::REVERSED);
            }
            put(buf, area.x + x, area.y + y, cell, st, x + 1 == area.width);
        }
        // Prompt rows get a marker in the first column's background, so jumps are visible.
        if c.is_prompt(r)
            && let Some(cell) = buf.cell_mut(Position::new(area.x, area.y + y))
            && c.selection.is_none()
        {
            cell.set_style(cell.style().add_modifier(Modifier::UNDERLINED));
        }
    }
}

#[cfg(test)]
mod tests {
    use ptyro_core::TermSize;
    use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};

    use super::*;

    #[test]
    fn tab_cells_paint_as_spaces() {
        let size = TermSize::new(20, 2);
        let mut e = AlacrittyEmulator::new(size, 10);
        e.feed(b"a\tb", &mut FeedOutput::default());
        let mut t = FrameTracker::new(&mut e);
        let delta = t.build(&e, None, &mut StyleTable::default());
        let mut cache = ScreenCache::new(size);
        cache.apply(true, t.seq(), &delta);
        assert!(cache.plain_text().starts_with("a\t"), "copy keeps the tab");
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        screen(&cache, &mut buf, area, Depth::True, false);
        let row: String = (0..20).map(|x| buf[(x, 0)].symbol().to_owned()).collect();
        assert_eq!(row.trim_end(), "a       b");
    }
}
