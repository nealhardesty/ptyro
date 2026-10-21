//! Client-side screen cache (SPEC §11.4): applies keyframes and deltas, and renders the result
//! as plain text or ANSI. The TUI paints from it, `ptyro read` prints it, tests compare it.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::screen::{Color, CursorState, ScreenDelta, ScreenModes, Style, StyleFlags, TermSize};

/// Style id 0 is always the default style; it is never sent in a style table.
pub const DEFAULT_STYLE: u32 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Cell content; empty for the second half of a wide character.
    pub text: String,
    pub style: u32,
    /// First half of a wide character.
    pub wide: bool,
}

impl Cell {
    pub fn blank() -> Self {
        Self { text: " ".into(), style: DEFAULT_STYLE, wide: false }
    }
    pub fn is_spacer(&self) -> bool {
        self.text.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCache {
    pub size: TermSize,
    pub rows: Vec<Vec<Cell>>,
    pub wrapped: Vec<bool>,
    /// Rows where a shell prompt starts (OSC 133;A).
    pub prompts: Vec<bool>,
    pub styles: HashMap<u32, Style>,
    pub cursor: CursorState,
    pub modes: ScreenModes,
    pub title: Option<String>,
    /// Sequence number of the last applied frame.
    pub seq: u64,
}

impl ScreenCache {
    pub fn new(size: TermSize) -> Self {
        let mut c = Self {
            size,
            rows: Vec::new(),
            wrapped: Vec::new(),
            prompts: Vec::new(),
            styles: HashMap::new(),
            cursor: CursorState::default(),
            modes: ScreenModes::default(),
            title: None,
            seq: 0,
        };
        c.reshape(size);
        c
    }

    fn reshape(&mut self, size: TermSize) {
        self.size = size;
        self.rows.resize_with(size.rows as usize, Vec::new);
        self.wrapped.resize(size.rows as usize, false);
        self.prompts.resize(size.rows as usize, false);
        for row in &mut self.rows {
            row.resize_with(size.cols as usize, Cell::blank);
        }
    }

    pub fn apply(&mut self, keyframe: bool, seq: u64, d: &ScreenDelta) {
        if keyframe {
            self.styles.clear();
            self.rows.clear();
            self.title = None;
        }
        if let Some(size) = d.size {
            self.reshape(size);
        } else if keyframe {
            self.reshape(self.size);
        }
        for def in &d.styles {
            self.styles.insert(def.id, def.style);
        }
        for row in &d.rows {
            let Some(cells) = self.rows.get_mut(row.y as usize) else { continue };
            *cells = row_cells(&row.runs, self.size.cols as usize);
            cells.truncate(self.size.cols as usize);
            self.wrapped[row.y as usize] = row.wrapped;
            self.prompts[row.y as usize] = row.prompt;
        }
        if let Some(c) = d.cursor {
            self.cursor = c;
        }
        if let Some(m) = d.modes {
            self.modes = m;
        }
        if let Some(t) = &d.title {
            self.title = (!t.is_empty()).then(|| t.clone());
        }
        self.seq = seq;
    }

    pub fn style(&self, id: u32) -> Style {
        self.styles.get(&id).copied().unwrap_or_default()
    }

    /// Visible text, one line per row, trailing spaces trimmed; trailing blank rows dropped.
    pub fn plain_text(&self) -> String {
        plain_text(&self.rows)
    }

    /// Visible text with SGR colours and attributes; each line ends with a reset.
    pub fn ansi_text(&self) -> String {
        ansi_text(&self.rows, &self.styles)
    }
}

/// Cells of a row from its runs, padded to `cols` (spacers follow wide cells).
pub fn row_cells(runs: &[crate::screen::Run], cols: usize) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(cols);
    for run in runs {
        for text in run.cells() {
            cells.push(Cell { text: text.to_owned(), style: run.style, wide: run.wide });
            if run.wide {
                cells.push(Cell { text: String::new(), style: run.style, wide: false });
            }
        }
    }
    cells.resize_with(cols.max(cells.len()), Cell::blank);
    cells
}

/// The rows of a scrollback document as cells.
pub fn doc_rows(doc: &crate::screen::ScrollbackDoc) -> Vec<Vec<Cell>> {
    doc.rows.iter().map(|r| row_cells(&r.runs, doc.size.cols as usize)).collect()
}

/// The styles of a scrollback document by id.
pub fn doc_styles(doc: &crate::screen::ScrollbackDoc) -> HashMap<u32, Style> {
    doc.styles.iter().map(|d| (d.id, d.style)).collect()
}

/// Plain text of `rows`: trailing spaces trimmed, trailing blank rows dropped.
pub fn plain_text(rows: &[Vec<Cell>]) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| r.iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_owned())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// `rows` with SGR colours and attributes; each line ends with a reset.
pub fn ansi_text(rows: &[Vec<Cell>], styles: &HashMap<u32, Style>) -> String {
    let style = |id: u32| styles.get(&id).copied().unwrap_or_default();
    let mut out = String::new();
    let last = rows
        .iter()
        .rposition(|r| r.iter().any(|c| c.text.trim() != "" || c.style != DEFAULT_STYLE))
        .map_or(0, |i| i + 1);
    for row in &rows[..last] {
        let mut current = None;
        let end = row
            .iter()
            .rposition(|c| c.text.trim() != "" || style(c.style).bg != Color::Default)
            .map_or(0, |i| i + 1);
        for cell in &row[..end] {
            if current != Some(cell.style) {
                out.push_str(&sgr(&style(cell.style)));
                current = Some(cell.style);
            }
            out.push_str(&cell.text);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// A complete SGR sequence (starting from a reset) that selects `style`.
pub fn sgr(style: &Style) -> String {
    let mut s = String::from("\x1b[0");
    let f = style.flags;
    for (flag, code) in [
        (StyleFlags::BOLD, "1"),
        (StyleFlags::DIM, "2"),
        (StyleFlags::ITALIC, "3"),
        (StyleFlags::INVERSE, "7"),
        (StyleFlags::HIDDEN, "8"),
        (StyleFlags::STRIKE, "9"),
    ] {
        if f.has(flag) {
            let _ = write!(s, ";{code}");
        }
    }
    if f.has(StyleFlags::DOUBLE_UNDERLINE) {
        s.push_str(";4:2");
    } else if f.has(StyleFlags::CURLY_UNDERLINE) {
        s.push_str(";4:3");
    } else if f.has(StyleFlags::DOTTED_UNDERLINE) {
        s.push_str(";4:4");
    } else if f.has(StyleFlags::DASHED_UNDERLINE) {
        s.push_str(";4:5");
    } else if f.has(StyleFlags::UNDERLINE) {
        s.push_str(";4");
    }
    color(&mut s, style.fg, 30, 90, 38);
    color(&mut s, style.bg, 40, 100, 48);
    if let Some(ul) = style.ul {
        match ul {
            Color::Default => {}
            Color::Indexed { i } => {
                let _ = write!(s, ";58:5:{i}");
            }
            Color::Rgb { r, g, b } => {
                let _ = write!(s, ";58:2::{r}:{g}:{b}");
            }
        }
    }
    s.push('m');
    s
}

fn color(s: &mut String, c: Color, base: u8, bright: u8, ext: u8) {
    match c {
        Color::Default => {}
        Color::Indexed { i } if i < 8 => {
            let _ = write!(s, ";{}", base + i);
        }
        Color::Indexed { i } if i < 16 => {
            let _ = write!(s, ";{}", bright + i - 8);
        }
        Color::Indexed { i } => {
            let _ = write!(s, ";{ext};5;{i}");
        }
        Color::Rgb { r, g, b } => {
            let _ = write!(s, ";{ext};2;{r};{g};{b}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{RowUpdate, Run, StyleDef};

    #[test]
    fn applies_rows_and_pads() {
        let mut c = ScreenCache::new(TermSize::new(6, 2));
        let red = Style { fg: Color::Indexed { i: 1 }, ..Style::default() };
        let d = ScreenDelta {
            styles: vec![StyleDef { id: 1, style: red }],
            rows: vec![RowUpdate {
                y: 1,
                runs: vec![
                    Run { style: 1, text: "hi".into(), wide: false, clusters: None },
                    Run { style: 0, text: "漢".into(), wide: true, clusters: None },
                ],
                wrapped: false,
                prompt: false,
            }],
            ..ScreenDelta::default()
        };
        c.apply(true, 1, &d);
        assert_eq!(c.plain_text(), "\nhi漢\n");
        assert!(c.rows[1][3].is_spacer());
        assert_eq!(c.rows[1].len(), 6);
        assert_eq!(c.ansi_text(), "\x1b[0m\n\x1b[0;31mhi\x1b[0m漢\x1b[0m\n");
    }

    #[test]
    fn sgr_codes() {
        let s = Style {
            fg: Color::Indexed { i: 9 },
            bg: Color::Rgb { r: 1, g: 2, b: 3 },
            ul: None,
            flags: StyleFlags(StyleFlags::BOLD | StyleFlags::UNDERLINE),
        };
        assert_eq!(sgr(&s), "\x1b[0;1;4;91;48;2;1;2;3m");
    }
}
