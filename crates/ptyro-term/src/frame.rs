//! Row versioning and screen-frame building (SPEC §5.4, §6.5).
//!
//! The tracker keeps one version number per visible row plus one for each scalar (size, cursor,
//! modes, title). A client that has applied everything up to `since` needs exactly the parts whose
//! version is newer, so the daemon keeps no per-client diff state and no history: O(rows) per frame.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use ptyro_core::TermSize;
use ptyro_core::cache::DEFAULT_STYLE;
use ptyro_core::screen::{
    CursorState, RowUpdate, Run, ScreenDelta, ScreenModes, ScrollbackDoc, Style, StyleDef,
};

use crate::emulator::{CellData, Damage, Emulator, MAX_COMBINING};

pub struct FrameTracker {
    seq: u64,
    row_ver: Vec<u64>,
    /// Rows the emulator reported damaged since they were last checked. Checking (hashing the
    /// row's content) is deferred until a frame is built, so a session nobody watches pays only
    /// for bookkeeping, not for reading cells (SPEC §5.4).
    pending: Vec<bool>,
    /// Content hash per row as of its last check. alacritty reports the cursor row (and
    /// sometimes more) as damaged even when nothing changed: damage nominates, the hash decides.
    row_hash: Vec<u64>,
    scratch: Vec<CellData>,
    size_ver: u64,
    cursor_ver: u64,
    modes_ver: u64,
    title_ver: u64,
    size: TermSize,
    cursor: CursorState,
    modes: ScreenModes,
    title: Option<String>,
}

impl FrameTracker {
    pub fn new(emu: &mut dyn Emulator) -> Self {
        let _ = emu.take_damage();
        let size = emu.size();
        let mut scratch = Vec::new();
        let row_hash = (0..size.rows).map(|y| row_hash(emu, y, &mut scratch)).collect();
        Self {
            seq: 1,
            row_ver: vec![1; size.rows as usize],
            pending: vec![false; size.rows as usize],
            row_hash,
            scratch,
            size_ver: 1,
            cursor_ver: 1,
            modes_ver: 1,
            title_ver: 1,
            size,
            cursor: emu.cursor(),
            modes: emu.modes().viewer(),
            title: emu.title().map(str::to_owned),
        }
    }

    /// The version a client is at after applying the latest frame.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Fold the emulator's changes since the last call into versions. Call after every feed,
    /// resize or sync expiry. Returns whether anything a viewer sees *may* have changed (damaged
    /// rows are only confirmed by `build`). Cheap: no cell is read here.
    pub fn update(&mut self, emu: &mut dyn Emulator) -> bool {
        let damage = emu.take_damage();
        let size = emu.size();
        let cursor = emu.cursor();
        let modes = emu.modes().viewer();
        let title = emu.title().map(str::to_owned);

        let resized = size != self.size;
        let mut damaged = false;
        if resized {
            self.pending = vec![true; size.rows as usize];
            // Force every row to count as changed when it is next checked.
            self.row_hash = vec![u64::MAX; size.rows as usize];
            damaged = true;
        } else {
            match damage {
                Damage::All => {
                    self.pending.fill(true);
                    damaged = true;
                }
                // alacritty under-reports in one known case: overwriting a wide char that wrapped
                // also clears the *leading spacer* it left at the end of the previous row, but only
                // the current row is marked damaged. Check the row above each damaged row too.
                // Found by `tests/props.rs`.
                Damage::Rows(rows) => {
                    for y in rows {
                        for r in [y.saturating_sub(1), y] {
                            if let Some(p) = self.pending.get_mut(r as usize) {
                                *p = true;
                                damaged = true;
                            }
                        }
                    }
                }
            }
        }
        let changed =
            damaged || cursor != self.cursor || modes != self.modes || title != self.title;
        if !changed {
            return false;
        }
        self.seq += 1;
        let seq = self.seq;
        if resized {
            self.size = size;
            self.size_ver = seq;
            self.row_ver = vec![seq; size.rows as usize];
        }
        if cursor != self.cursor {
            self.cursor = cursor;
            self.cursor_ver = seq;
        }
        if modes != self.modes {
            self.modes = modes;
            self.modes_ver = seq;
        }
        if title != self.title {
            self.title = title;
            self.title_ver = seq;
        }
        true
    }

    /// Check pending rows: those whose content really changed get the current version.
    fn resolve(&mut self, emu: &dyn Emulator) {
        for y in 0..self.pending.len() {
            if !std::mem::take(&mut self.pending[y]) {
                continue;
            }
            let h = row_hash(emu, y as u16, &mut self.scratch);
            if self.row_hash[y] != h {
                self.row_hash[y] = h;
                self.row_ver[y] = self.seq;
            }
        }
    }

    /// Everything newer than `since`; `None` builds a keyframe (and resets `styles`).
    ///
    /// When `styles` has grown too big (animated truecolour), it is reset here and every row is
    /// sent again with the new ids, so the caller needs nothing special: ids are reused and their
    /// definitions re-sent, which a client applies over the old ones.
    pub fn build(
        &mut self,
        emu: &dyn Emulator,
        since: Option<u64>,
        styles: &mut StyleTable,
    ) -> ScreenDelta {
        self.resolve(emu);
        let newer = |v: u64| since.is_none_or(|s| v > s);
        let all_rows = since.is_none() || styles.too_big();
        if all_rows {
            styles.reset();
        }
        let mut delta = ScreenDelta::default();
        if newer(self.size_ver) {
            delta.size = Some(self.size);
        }
        let mut cells = Vec::new();
        let mut cost = 0;
        for (y, &ver) in self.row_ver.iter().enumerate() {
            if all_rows || newer(ver) {
                let y = y as u16;
                emu.row(y, &mut cells);
                let defs = delta.styles.len();
                let mut runs = runs(&cells, styles, &mut delta.styles);
                cost += runs_cost(&runs) + (delta.styles.len() - defs) * DEF_COST;
                if cost > FRAME_BUDGET {
                    // Only a pathological screen gets here: blank the rest rather than send a
                    // frame the client must drop (and with it the connection).
                    runs.clear();
                }
                let prompt = cells.iter().any(|c| c.prompt);
                delta.rows.push(RowUpdate { y, runs, wrapped: emu.row_wrapped(y), prompt });
            }
        }
        if all_rows {
            styles.settle();
        }
        if newer(self.cursor_ver) {
            delta.cursor = Some(self.cursor);
        }
        if newer(self.modes_ver) {
            delta.modes = Some(self.modes);
        }
        if newer(self.title_ver) {
            delta.title = Some(self.title.clone().unwrap_or_default());
        }
        delta
    }
}

fn row_hash(emu: &dyn Emulator, y: u16, cells: &mut Vec<CellData>) -> u64 {
    emu.row(y, cells);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in cells.iter() {
        (c.c, &c.zerowidth, c.style, c.wide, c.spacer, c.prompt).hash(&mut h);
    }
    emu.row_wrapped(y).hash(&mut h);
    h.finish()
}

/// Rough upper bound on what a frame may carry, well under `MAX_FRAME` (16 MiB) so estimates
/// and encoding overhead (JSON is bigger than MessagePack) still fit.
const FRAME_BUDGET: usize = 8 * 1024 * 1024;
/// Estimated encoded bytes per run (besides its text) and per style definition.
const RUN_COST: usize = 40;
const DEF_COST: usize = 128;

fn runs_cost(runs: &[Run]) -> usize {
    runs.iter().map(|r| RUN_COST + r.text.len() + r.clusters.as_ref().map_or(0, Vec::len)).sum()
}

/// A frozen document of the last `lines` history lines plus the screen (SPEC §5.4). History is
/// cut short (oldest lines first) to keep the document within a frame.
pub fn scrollback_doc(emu: &dyn Emulator, lines: u32) -> ScrollbackDoc {
    let history = (emu.scrollback_len() as u32).min(lines);
    let size = emu.size();
    let mut styles = StyleTable::default();
    let mut doc = ScrollbackDoc { size, history, cursor: emu.cursor(), ..ScrollbackDoc::default() };
    let mut cells = Vec::new();
    // Newest first, so the budget drops the oldest history; the screen itself always goes in.
    let mut cost = 0;
    for line in (-(history as i32)..i32::from(size.rows)).rev() {
        emu.line(line, &mut cells);
        let defs = doc.styles.len();
        let runs = runs(&cells, &mut styles, &mut doc.styles);
        cost += runs_cost(&runs) + (doc.styles.len() - defs) * DEF_COST;
        if line < 0 && cost > FRAME_BUDGET {
            doc.history = (-line - 1) as u32;
            break;
        }
        let prompt = cells.iter().any(|c| c.prompt);
        doc.rows.push(RowUpdate { y: 0, runs, wrapped: emu.line_wrapped(line), prompt });
    }
    doc.rows.reverse();
    doc
}

/// Style ids over this many (and twice what the screen needed at the last reset) make the next
/// frame reset the table.
const MAX_STYLES: usize = 4096;

/// Per-subscription style ids. Append-only between resets; id 0 is the default style.
#[derive(Debug, Default)]
pub struct StyleTable {
    ids: HashMap<Style, u32>,
    /// Ids in use right after the last reset (what one full screen needs).
    settled: usize,
}

impl StyleTable {
    pub fn reset(&mut self) {
        self.ids.clear();
    }

    fn settle(&mut self) {
        self.settled = self.ids.len();
    }

    /// The table has grown enough that re-sending the screen is cheaper than keeping it.
    fn too_big(&self) -> bool {
        self.ids.len() > MAX_STYLES.max(2 * self.settled)
    }

    fn id(&mut self, style: Style, defs: &mut Vec<StyleDef>) -> u32 {
        if style == Style::default() {
            return DEFAULT_STYLE;
        }
        let next = self.ids.len() as u32 + 1;
        *self.ids.entry(style).or_insert_with(|| {
            defs.push(StyleDef { id: next, style });
            next
        })
    }
}

/// Group a row's cells into runs. Trailing default blanks are dropped; the client pads.
fn runs(cells: &[CellData], styles: &mut StyleTable, defs: &mut Vec<StyleDef>) -> Vec<Run> {
    let end = cells
        .iter()
        .rposition(|c| {
            c.spacer || c.c != ' ' || c.zerowidth.is_some() || c.style != Style::default()
        })
        .map_or(0, |i| i + 1);
    let mut out: Vec<Run> = Vec::new();
    let mut lens: Vec<u8> = Vec::new();
    let mut clustered = false;
    let flush = |run: Option<Run>, lens: &mut Vec<u8>, clustered: &mut bool, out: &mut Vec<Run>| {
        if let Some(mut r) = run {
            if *clustered {
                r.clusters = Some(std::mem::take(lens));
            }
            out.push(r);
        }
        lens.clear();
        *clustered = false;
    };
    let mut current: Option<Run> = None;
    for cell in &cells[..end] {
        if cell.spacer {
            continue;
        }
        let style = styles.id(cell.style, defs);
        let same = current.as_ref().is_some_and(|r| r.style == style && r.wide == cell.wide);
        if !same {
            flush(current.take(), &mut lens, &mut clustered, &mut out);
            current = Some(Run { style, text: String::new(), wide: cell.wide, clusters: None });
        }
        let run = current.as_mut().expect("just set");
        run.text.push(cell.c);
        let mut n = 1u8;
        if let Some(zw) = &cell.zerowidth {
            // The emulator already caps these; keep `n` and the text in step regardless.
            let zw = &zw[..zw.len().min(MAX_COMBINING)];
            run.text.extend(zw);
            n += zw.len() as u8;
            clustered = true;
        }
        lens.push(n);
    }
    flush(current, &mut lens, &mut clustered, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{AlacrittyEmulator, FeedOutput};
    use ptyro_core::cache::ScreenCache;

    fn feed(emu: &mut AlacrittyEmulator, t: &mut FrameTracker, bytes: &[u8]) -> bool {
        let mut out = FeedOutput::default();
        emu.feed(bytes, &mut out);
        t.update(emu)
    }

    fn keyframe(emu: &AlacrittyEmulator, t: &mut FrameTracker) -> ScreenCache {
        let mut c = ScreenCache::new(emu.size());
        c.apply(true, t.seq(), &t.build(emu, None, &mut StyleTable::default()));
        c
    }

    #[test]
    fn deltas_only_carry_changed_rows() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 5), 100);
        let mut t = FrameTracker::new(&mut emu);
        assert!(feed(&mut emu, &mut t, b"one\r\ntwo"));
        // A client's `since` always comes from a frame it received, i.e. a build.
        t.build(&emu, None, &mut StyleTable::default());
        let since = t.seq();
        assert!(feed(&mut emu, &mut t, b"\x1b[5;1Hfive"));
        let d = t.build(&emu, Some(since), &mut StyleTable::default());
        let ys: Vec<u16> = d.rows.iter().map(|r| r.y).collect();
        assert!(ys.contains(&4) && !ys.contains(&0), "{ys:?}");
        // Damage without a content change yields an empty delta.
        let since = t.seq();
        feed(&mut emu, &mut t, b"");
        assert!(t.build(&emu, Some(since), &mut StyleTable::default()).is_empty());
    }

    #[test]
    fn combining_and_wide_characters_survive_the_round_trip() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 100);
        let mut t = FrameTracker::new(&mut emu);
        feed(&mut emu, &mut t, "e\u{301}漢x".as_bytes());
        assert_eq!(keyframe(&emu, &mut t).plain_text(), "e\u{301}漢x\n");
    }

    #[test]
    fn clusters_match_text_when_marks_are_capped() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 100);
        let mut t = FrameTracker::new(&mut emu);
        feed(&mut emu, &mut t, format!("a{}b", "\u{301}".repeat(500)).as_bytes());
        let d = t.build(&emu, None, &mut StyleTable::default());
        let run = &d.rows[0].runs[0];
        let lens = run.clusters.as_ref().unwrap();
        assert_eq!(lens, &[1 + MAX_COMBINING as u8, 1]);
        assert_eq!(run.text.chars().count(), lens.iter().map(|&n| n as usize).sum::<usize>());
        assert_eq!(run.cells()[1], "b");
    }

    #[test]
    fn style_table_stays_bounded() {
        // Every frame repaints one cell in a new truecolour: ids must not grow without end.
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let mut t = FrameTracker::new(&mut emu);
        let mut styles = StyleTable::default();
        let mut cache = ScreenCache::new(emu.size());
        cache.apply(true, t.seq(), &t.build(&emu, None, &mut styles));
        feed(&mut emu, &mut t, b"\x1b[2;1Hfixed\x1b[1;31mred");
        let mut full = 0;
        for i in 0..3 * MAX_STYLES as u32 {
            let since = t.seq();
            let rgb = format!("\x1b[H\x1b[38;2;{};{};{}mX", i & 255, (i >> 8) & 255, i >> 16);
            feed(&mut emu, &mut t, rgb.as_bytes());
            let d = t.build(&emu, Some(since), &mut styles);
            if d.rows.len() == 3 {
                full += 1;
            }
            cache.apply(false, t.seq(), &d);
            assert!(styles.ids.len() <= MAX_STYLES + 1);
        }
        assert!(full >= 2, "{full}");
        // The client's picture is still right after ids were reused.
        assert_eq!(cache.plain_text(), "X\nfixedred\n");
        let red = cache.rows[1][5].style;
        assert_eq!(cache.style(red).fg, ptyro_core::screen::Color::Indexed { i: 1 });
    }

    #[test]
    fn scrollback_doc_fits_a_frame() {
        // ~7 KB a row (every cell carries the most combining marks), 1500 rows: over budget.
        let mut emu = AlacrittyEmulator::new(TermSize::new(200, 5), 10_000);
        let mut out = FeedOutput::default();
        let cell = format!("e{}", "\u{301}".repeat(MAX_COMBINING));
        let line = format!("{}\r\n", cell.repeat(200));
        for _ in 0..1500 {
            emu.feed(line.as_bytes(), &mut out);
        }
        let doc = scrollback_doc(&emu, u32::MAX);
        assert!(doc.history < emu.scrollback_len() as u32, "history was cut");
        assert_eq!(doc.rows.len(), doc.history as usize + 5);
        let bytes: usize = doc.rows.iter().map(|r| runs_cost(&r.runs)).sum();
        assert!(bytes <= FRAME_BUDGET, "{bytes}");
        // The newest history is the part kept.
        let text: String = doc.rows[doc.history as usize - 1].runs[0].text.clone();
        assert!(text.starts_with(&cell));
    }

    #[test]
    fn resize_sends_size_and_all_rows() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let mut t = FrameTracker::new(&mut emu);
        let since = t.seq();
        emu.resize(TermSize::new(12, 4));
        assert!(t.update(&mut emu));
        let d = t.build(&emu, Some(since), &mut StyleTable::default());
        assert_eq!(d.size, Some(TermSize::new(12, 4)));
        assert_eq!(d.rows.len(), 4);
    }
}
