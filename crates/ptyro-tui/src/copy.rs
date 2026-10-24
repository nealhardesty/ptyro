//! Copy mode (SPEC §11.3, M2.9): vi-style navigation, search and selection over a frozen
//! scrollback document. Pure state; the app draws it and sends yanks to the clipboard.

use std::collections::HashMap;

use ptyro_core::cache::{Cell, doc_rows, doc_styles};
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::screen::{ScrollbackDoc, Style};

/// The largest count a motion takes (`99999j`), so a long count can't stall or overflow.
const MAX_COUNT: usize = 100_000;

/// A row's text without trailing blanks, for matching rows across documents.
fn row_text(row: &[Cell]) -> String {
    let s: String = row.iter().map(|c| c.text.as_str()).collect();
    s.trim_end().to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelMode {
    Char,
    Line,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Exit,
    /// Copy this text, then leave copy mode.
    Yank(String),
    /// `gg` at the very top: fetch the older lines from the recording (M8.2).
    MoreHistory,
    /// Add this text to the scratchpad (NOTE1.2), then leave copy mode.
    Note(String),
}

#[derive(Debug, Clone)]
pub struct Search {
    pub query: String,
    pub forward: bool,
    /// (row, first col, last col exclusive)
    pub matches: Vec<(usize, usize, usize)>,
}

pub struct CopyMode {
    rows: Vec<Vec<Cell>>,
    styles: HashMap<u32, Style>,
    wrapped: Vec<bool>,
    prompts: Vec<bool>,
    pub history: usize,
    pub cols: usize,
    pub top: usize,
    pub height: usize,
    pub cursor: Pos,
    pub selection: Option<(Pos, SelMode)>,
    pub search: Option<Search>,
    /// Search text being typed (`/` forward, `?` backward).
    pub input: Option<(bool, String)>,
    count: String,
    pending_g: bool,
    /// A short note for the header (e.g. "no more matches").
    pub note: Option<String>,
    /// The recording was asked for older lines already.
    pub deep: bool,
}

impl CopyMode {
    /// The same view over a longer document (older lines above): cursor, view and selection
    /// keep their place.
    pub fn extend(&mut self, doc: &ScrollbackDoc) {
        let mut n = Self::new(doc, self.height);
        // The longer history also holds what was printed since copy mode started, so find where
        // our history now starts rather than trusting the difference in length.
        let most = n.history.saturating_sub(self.history);
        let ours = &self.rows[..self.history.max(1).min(self.rows.len())];
        let added = (0..=most)
            .rev()
            .find(|&o| {
                n.rows.get(o..o + ours.len()).is_some_and(|theirs| {
                    theirs.iter().zip(ours).all(|(a, b)| row_text(a) == row_text(b))
                })
            })
            .unwrap_or(most);
        n.cursor = Pos { row: self.cursor.row + added, col: self.cursor.col };
        n.top = self.top + added;
        n.selection = self.selection.map(|(p, m)| (Pos { row: p.row + added, col: p.col }, m));
        n.deep = true;
        n.note = Some(if added == 0 {
            "no older lines recorded".into()
        } else {
            format!("{added} older lines from the recording")
        });
        *self = n;
    }

    pub fn new(doc: &ScrollbackDoc, height: usize) -> Self {
        let rows = doc_rows(doc);
        let history = doc.history as usize;
        let mut c = Self {
            styles: doc_styles(doc),
            wrapped: doc.rows.iter().map(|r| r.wrapped).collect(),
            prompts: doc.rows.iter().map(|r| r.prompt).collect(),
            history,
            cols: doc.size.cols as usize,
            top: history,
            height: height.max(1),
            cursor: Pos { row: history + doc.cursor.y as usize, col: doc.cursor.x as usize },
            selection: None,
            search: None,
            input: None,
            count: String::new(),
            pending_g: false,
            note: None,
            deep: false,
            rows,
        };
        c.cursor.row = c.cursor.row.min(c.last_row());
        c.follow();
        c
    }

    fn last_row(&self) -> usize {
        self.rows.len().saturating_sub(1)
    }

    pub fn row(&self, r: usize) -> Option<&[Cell]> {
        self.rows.get(r).map(Vec::as_slice)
    }

    pub fn style(&self, id: u32) -> Style {
        self.styles.get(&id).copied().unwrap_or_default()
    }

    pub fn is_prompt(&self, r: usize) -> bool {
        self.prompts.get(r).copied().unwrap_or(false)
    }

    pub fn set_height(&mut self, h: usize) {
        self.height = h.max(1);
        self.follow();
    }

    /// Whether the view shows the live screen's position (scrolling down there exits).
    pub fn at_bottom(&self) -> bool {
        self.top + self.height >= self.rows.len()
    }

    /// Keep the cursor inside the viewport.
    fn follow(&mut self) {
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        } else if self.cursor.row >= self.top + self.height {
            self.top = self.cursor.row + 1 - self.height;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(self.height));
    }

    /// Scroll the view by `lines` (negative = up), dragging the cursor along when it leaves.
    pub fn scroll(&mut self, lines: isize) {
        let max_top = self.rows.len().saturating_sub(self.height);
        self.top = (self.top as isize + lines).clamp(0, max_top as isize) as usize;
        self.cursor.row =
            self.cursor.row.clamp(self.top, (self.top + self.height - 1).min(self.last_row()));
    }

    fn ch(&self, p: Pos) -> char {
        self.rows
            .get(p.row)
            .and_then(|r| r.get(p.col))
            .and_then(|c| c.text.chars().next())
            .unwrap_or(' ')
    }

    fn class(c: char) -> u8 {
        if c.is_whitespace() {
            0
        } else if c.is_alphanumeric() || c == '_' {
            1
        } else {
            2
        }
    }

    fn row_end(&self, row: usize) -> usize {
        self.rows
            .get(row)
            .map_or(0, |r| r.iter().rposition(|c| !c.text.trim().is_empty()).unwrap_or(0))
    }

    fn next_pos(&self, p: Pos) -> Option<Pos> {
        if p.col + 1 < self.cols {
            Some(Pos { col: p.col + 1, ..p })
        } else if p.row < self.last_row() {
            Some(Pos { row: p.row + 1, col: 0 })
        } else {
            None
        }
    }

    fn prev_pos(&self, p: Pos) -> Option<Pos> {
        if p.col > 0 {
            Some(Pos { col: p.col - 1, ..p })
        } else if p.row > 0 {
            Some(Pos { row: p.row - 1, col: self.cols.saturating_sub(1) })
        } else {
            None
        }
    }

    fn word_forward(&self, mut p: Pos) -> Pos {
        let start = Self::class(self.ch(p));
        while let Some(n) = self.next_pos(p) {
            let row_changed = n.row != p.row;
            p = n;
            if row_changed && !self.wrapped.get(p.row.saturating_sub(1)).copied().unwrap_or(false) {
                break;
            }
            if Self::class(self.ch(p)) != start {
                break;
            }
        }
        while Self::class(self.ch(p)) == 0 {
            match self.next_pos(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        p
    }

    fn word_back(&self, mut p: Pos) -> Pos {
        while let Some(n) = self.prev_pos(p) {
            p = n;
            if Self::class(self.ch(p)) != 0 {
                break;
            }
        }
        let class = Self::class(self.ch(p));
        while let Some(n) = self.prev_pos(p) {
            if Self::class(self.ch(n)) != class || n.row != p.row {
                break;
            }
            p = n;
        }
        p
    }

    fn word_end(&self, mut p: Pos) -> Pos {
        if let Some(n) = self.next_pos(p) {
            p = n;
        }
        while Self::class(self.ch(p)) == 0 {
            match self.next_pos(p) {
                Some(n) => p = n,
                None => return p,
            }
        }
        let class = Self::class(self.ch(p));
        while let Some(n) = self.next_pos(p) {
            if Self::class(self.ch(n)) != class || n.row != p.row {
                break;
            }
            p = n;
        }
        p
    }

    fn take_count(&mut self) -> usize {
        let n = self.count.parse().unwrap_or(1).clamp(1, MAX_COUNT);
        self.count.clear();
        n
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        self.note = None;
        if let Some((forward, text)) = &mut self.input {
            match k.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let (forward, query) = (*forward, std::mem::take(text));
                    self.input = None;
                    if !query.is_empty() {
                        self.run_search(query, forward);
                        self.jump_match(forward);
                    }
                }
                KeyCode::Backspace => {
                    if text.pop().is_none() {
                        self.input = None;
                    }
                }
                KeyCode::Char { c } if !k.mods.has(Mods::CTRL) => text.push(c),
                _ => {}
            }
            return Outcome::Stay;
        }
        let ctrl = k.mods.has(Mods::CTRL);
        if let KeyCode::Char { c } = k.code
            && !ctrl
            && (c.is_ascii_digit() && (c != '0' || !self.count.is_empty()))
        {
            if self.count.len() < 6 {
                self.count.push(c);
            }
            return Outcome::Stay;
        }
        let g = std::mem::take(&mut self.pending_g);
        let half = (self.height / 2).max(1);
        match (k.code, ctrl) {
            (KeyCode::Esc, _) if self.selection.is_some() => self.selection = None,
            (KeyCode::Esc, _)
            | (KeyCode::Char { c: 'q' }, false)
            | (KeyCode::Char { c: 'c' }, true) => {
                return Outcome::Exit;
            }
            (KeyCode::Char { c: 'h' }, false) | (KeyCode::Left, _) => {
                let n = self.take_count();
                self.cursor.col = self.cursor.col.saturating_sub(n);
            }
            (KeyCode::Char { c: 'l' }, false) | (KeyCode::Right, _) => {
                let n = self.take_count();
                self.cursor.col = (self.cursor.col + n).min(self.cols.saturating_sub(1));
            }
            (KeyCode::Char { c: 'j' }, false) | (KeyCode::Down, _) => {
                let n = self.take_count();
                self.cursor.row = (self.cursor.row + n).min(self.last_row());
            }
            (KeyCode::Char { c: 'k' }, false) | (KeyCode::Up, _) => {
                let n = self.take_count();
                self.cursor.row = self.cursor.row.saturating_sub(n);
            }
            (KeyCode::Char { c: 'w' | 'W' }, false) => {
                for _ in 0..self.take_count() {
                    self.cursor = self.word_forward(self.cursor);
                }
            }
            (KeyCode::Char { c: 'b' | 'B' }, false) => {
                for _ in 0..self.take_count() {
                    self.cursor = self.word_back(self.cursor);
                }
            }
            (KeyCode::Char { c: 'e' | 'E' }, false) => {
                for _ in 0..self.take_count() {
                    self.cursor = self.word_end(self.cursor);
                }
            }
            (KeyCode::Char { c: '0' }, false) | (KeyCode::Home, _) => self.cursor.col = 0,
            (KeyCode::Char { c: '^' }, false) => {
                self.cursor.col = self.rows[self.cursor.row]
                    .iter()
                    .position(|c| !c.text.trim().is_empty())
                    .unwrap_or(0);
            }
            (KeyCode::Char { c: '$' }, false) | (KeyCode::End, _) => {
                self.cursor.col = self.row_end(self.cursor.row)
            }
            (KeyCode::Char { c: 'g' }, false) if g && self.cursor.row == 0 && !self.deep => {
                self.deep = true;
                self.note = Some("loading older lines from the recording…".into());
                return Outcome::MoreHistory;
            }
            (KeyCode::Char { c: 'g' }, false) if g => self.cursor = Pos { row: 0, col: 0 },
            (KeyCode::Char { c: 'g' }, false) => self.pending_g = true,
            (KeyCode::Char { c: 'G' }, false) => {
                self.cursor.row =
                    if self.count.is_empty() { self.last_row() } else { self.take_count() - 1 }
                        .min(self.last_row());
            }
            (KeyCode::Char { c: 'H' }, false) => self.cursor.row = self.top,
            (KeyCode::Char { c: 'M' }, false) => {
                self.cursor.row = (self.top + self.height / 2).min(self.last_row())
            }
            (KeyCode::Char { c: 'L' }, false) => {
                self.cursor.row = (self.top + self.height - 1).min(self.last_row())
            }
            (KeyCode::Char { c: 'u' }, true) => {
                self.scroll(-(half as isize));
                self.cursor.row = self.cursor.row.saturating_sub(half).max(self.top);
            }
            (KeyCode::Char { c: 'd' }, true) => {
                self.scroll(half as isize);
                self.cursor.row = (self.cursor.row + half).min(self.last_row());
            }
            (KeyCode::Char { c: 'b' }, true) | (KeyCode::PageUp, _) => {
                self.scroll(-(self.height as isize));
            }
            (KeyCode::Char { c: 'f' }, true) | (KeyCode::PageDown, _) => {
                self.scroll(self.height as isize);
            }
            (KeyCode::Char { c: 'y' }, true) => self.scroll(-1),
            (KeyCode::Char { c: 'e' }, true) => self.scroll(1),
            (KeyCode::Char { c: '[' | '{' }, false) => self.jump_prompt(false),
            (KeyCode::Char { c: ']' | '}' }, false) => self.jump_prompt(true),
            (KeyCode::Char { c: 'v' }, false) => self.toggle(SelMode::Char),
            (KeyCode::Char { c: 'V' }, false) => self.toggle(SelMode::Line),
            (KeyCode::Char { c: 'v' }, true) => self.toggle(SelMode::Block),
            (KeyCode::Char { c: 'o' }, false) => {
                if let Some((anchor, mode)) = self.selection {
                    self.selection = Some((self.cursor, mode));
                    self.cursor = anchor;
                }
            }
            (KeyCode::Char { c: 'y' }, false) | (KeyCode::Enter, _) => {
                if self.selection.is_some() {
                    return Outcome::Yank(self.selected_text());
                }
                if k.code == KeyCode::Enter {
                    return Outcome::Exit;
                }
            }
            (KeyCode::Char { c: 'Y' }, false) => {
                let text = self.text_between(
                    Pos { row: self.cursor.row, col: 0 },
                    Pos { row: self.cursor.row, col: self.cols },
                    SelMode::Line,
                );
                return Outcome::Yank(text);
            }
            (KeyCode::Char { c: 'a' }, false) if self.selection.is_some() => {
                return Outcome::Note(self.selected_text());
            }
            (KeyCode::Char { c: '/' }, false) => self.input = Some((true, String::new())),
            (KeyCode::Char { c: '?' }, false) => self.input = Some((false, String::new())),
            (KeyCode::Char { c: 'n' }, false) => {
                let forward = self.search.as_ref().is_none_or(|s| s.forward);
                self.jump_match(forward);
            }
            (KeyCode::Char { c: 'N' }, false) => {
                let forward = self.search.as_ref().is_none_or(|s| s.forward);
                self.jump_match(!forward);
            }
            _ => {}
        }
        self.count.clear();
        self.follow();
        Outcome::Stay
    }

    fn toggle(&mut self, mode: SelMode) {
        self.selection = match self.selection {
            Some((_, m)) if m == mode => None,
            Some((a, _)) => Some((a, mode)),
            None => Some((self.cursor, mode)),
        };
    }

    /// Start (or move) a selection at a document position, e.g. from a mouse drag.
    pub fn select_from(&mut self, anchor: Pos, mode: SelMode) {
        self.selection = Some((anchor, mode));
    }

    pub fn move_cursor(&mut self, p: Pos) {
        self.cursor =
            Pos { row: p.row.min(self.last_row()), col: p.col.min(self.cols.saturating_sub(1)) };
        self.follow();
    }

    fn jump_prompt(&mut self, forward: bool) {
        let n = self.take_count();
        for _ in 0..n {
            let found = if forward {
                (self.cursor.row + 1..self.rows.len()).find(|&r| self.is_prompt(r))
            } else {
                (0..self.cursor.row).rev().find(|&r| self.is_prompt(r))
            };
            match found {
                Some(r) => self.cursor = Pos { row: r, col: 0 },
                None => {
                    self.note = Some(if self.prompts.iter().any(|p| *p) {
                        "no more prompts".into()
                    } else {
                        "no prompt marks: C-q M sets up shell integration".into()
                    });
                    break;
                }
            }
        }
        if forward || self.note.is_none() {
            // Show the prompt near the top, with its output below.
            self.top =
                self.cursor.row.saturating_sub(1).min(self.rows.len().saturating_sub(self.height));
        }
    }

    /// A row's text and, for each char, the column it starts at.
    fn row_text(&self, r: usize) -> (String, Vec<usize>) {
        let mut s = String::new();
        let mut cols = Vec::new();
        for (col, cell) in self.rows[r].iter().enumerate() {
            if cell.is_spacer() {
                continue;
            }
            for c in cell.text.chars() {
                s.push(c);
                cols.push(col);
            }
        }
        (s, cols)
    }

    fn run_search(&mut self, query: String, forward: bool) {
        let smart_lower = !query.chars().any(char::is_uppercase);
        let needle: Vec<char> = if smart_lower {
            query.to_lowercase().chars().collect()
        } else {
            query.chars().collect()
        };
        let mut matches = Vec::new();
        for r in 0..self.rows.len() {
            let (text, cols) = self.row_text(r);
            let hay: Vec<char> = if smart_lower {
                text.to_lowercase().chars().collect()
            } else {
                text.chars().collect()
            };
            if hay.len() != cols.len() {
                continue; // lowercasing changed the length (rare scripts); skip rather than mis-map
            }
            let mut i = 0;
            while needle.len() <= hay.len() && i + needle.len() <= hay.len() {
                if hay[i..i + needle.len()] == needle[..] {
                    let start = cols[i];
                    let end = cols[i + needle.len() - 1] + 1;
                    matches.push((r, start, end));
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        if matches.is_empty() {
            self.note = Some(format!("not found: {query}"));
        }
        self.search = Some(Search { query, forward, matches });
    }

    fn jump_match(&mut self, forward: bool) {
        let Some(s) = &self.search else { return };
        let here = (self.cursor.row, self.cursor.col);
        let next = if forward {
            s.matches.iter().find(|m| (m.0, m.1) > here).or_else(|| s.matches.first())
        } else {
            s.matches.iter().rev().find(|m| (m.0, m.1) < here).or_else(|| s.matches.last())
        };
        if let Some(&(row, col, _)) = next {
            self.cursor = Pos { row, col };
            let n = s.matches.len();
            let i = s.matches.iter().position(|m| m.0 == row && m.1 == col).unwrap_or(0) + 1;
            self.note = Some(format!("{i}/{n} {:?}", s.query));
        }
        self.follow();
    }

    /// Whether (row, col) is inside the selection.
    pub fn is_selected(&self, row: usize, col: usize) -> bool {
        let Some((anchor, mode)) = self.selection else { return false };
        let (a, b) =
            if anchor <= self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
        match mode {
            SelMode::Line => (a.row..=b.row).contains(&row),
            SelMode::Block => {
                let (c0, c1) = (anchor.col.min(self.cursor.col), anchor.col.max(self.cursor.col));
                (a.row..=b.row).contains(&row) && (c0..=c1).contains(&col)
            }
            SelMode::Char => {
                (a.row..=b.row).contains(&row)
                    && (row != a.row || col >= a.col)
                    && (row != b.row || col <= b.col)
            }
        }
    }

    /// `Some(true)` for the match under the cursor, `Some(false)` for other matches.
    pub fn match_at(&self, row: usize, col: usize) -> Option<bool> {
        let s = self.search.as_ref()?;
        s.matches
            .iter()
            .find(|m| m.0 == row && (m.1..m.2).contains(&col))
            .map(|m| m.0 == self.cursor.row && m.1 == self.cursor.col)
    }

    pub fn selected_text(&self) -> String {
        let Some((anchor, mode)) = self.selection else { return String::new() };
        let (a, b) =
            if anchor <= self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
        match mode {
            SelMode::Block => {
                let (c0, c1) = (anchor.col.min(self.cursor.col), anchor.col.max(self.cursor.col));
                (a.row..=b.row)
                    .map(|r| self.cells_text(r, c0, c1 + 1).trim_end().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            m => self.text_between(a, b, m),
        }
    }

    fn cells_text(&self, r: usize, from: usize, to: usize) -> String {
        let Some(row) = self.rows.get(r) else { return String::new() };
        row[from.min(row.len())..to.min(row.len())]
            .iter()
            .filter(|c| !c.is_spacer())
            .map(|c| c.text.as_str())
            .collect()
    }

    /// Text from `a` to `b` inclusive; soft-wrapped rows join without a newline.
    fn text_between(&self, a: Pos, b: Pos, mode: SelMode) -> String {
        let mut out = String::new();
        for r in a.row..=b.row.min(self.last_row()) {
            let (from, to) = match mode {
                SelMode::Line => (0, self.cols),
                _ => (
                    if r == a.row { a.col } else { 0 },
                    if r == b.row { b.col + 1 } else { self.cols },
                ),
            };
            let text = self.cells_text(r, from, to);
            let wraps = self.wrapped.get(r).copied().unwrap_or(false) && r != b.row;
            if wraps {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                if r != b.row || mode == SelMode::Line {
                    out.push('\n');
                }
            }
        }
        if mode == SelMode::Line {
            out.pop();
        }
        out
    }

    /// `[row/total]` for the header.
    pub fn position(&self) -> String {
        format!("{}/{}", self.cursor.row + 1, self.rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::TermSize;
    use ptyro_core::screen::{CursorState, RowUpdate, Run};

    fn doc(lines: &[&str], wrapped: &[usize], prompts: &[usize], history: u32) -> ScrollbackDoc {
        ScrollbackDoc {
            size: TermSize::new(20, 3),
            history,
            rows: lines
                .iter()
                .enumerate()
                .map(|(i, l)| RowUpdate {
                    y: 0,
                    runs: vec![Run { style: 0, text: (*l).into(), wide: false, clusters: None }],
                    wrapped: wrapped.contains(&i),
                    prompt: prompts.contains(&i),
                })
                .collect(),
            styles: vec![],
            cursor: CursorState { x: 0, y: 2, visible: true, ..CursorState::default() },
        }
    }

    fn keys(c: &mut CopyMode, s: &str) -> Outcome {
        let mut last = Outcome::Stay;
        for ch in s.chars() {
            let k = match ch {
                '\n' => KeyEvent::new(KeyCode::Enter, Mods::NONE),
                '\x1b' => KeyEvent::new(KeyCode::Esc, Mods::NONE),
                c => KeyEvent::char(c),
            };
            last = c.key(k);
        }
        last
    }

    #[test]
    fn starts_at_the_live_cursor_and_yanks_words() {
        let d = doc(
            &["$ make", "build ok", "$ echo hello world", "hello world", "$"],
            &[],
            &[0, 2, 4],
            2,
        );
        let mut c = CopyMode::new(&d, 3);
        assert_eq!(c.cursor, Pos { row: 4, col: 0 });
        assert_eq!(c.top, 2);
        assert_eq!(keys(&mut c, "k0ve"), Outcome::Stay);
        assert_eq!(c.selected_text(), "hello");
        assert_eq!(keys(&mut c, "wey"), Outcome::Yank("hello world".into()));
    }

    #[test]
    fn a_sends_the_selection_to_the_scratchpad() {
        let d = doc(&["$ echo hello world", "hello world", "$"], &[], &[0, 2], 1);
        let mut c = CopyMode::new(&d, 3);
        assert_eq!(keys(&mut c, "a"), Outcome::Stay, "nothing selected, nothing to send");
        keys(&mut c, "k0ve");
        assert_eq!(keys(&mut c, "a"), Outcome::Note("hello".into()));
    }

    #[test]
    fn prompt_jumps_and_line_selection() {
        let d = doc(&["$ make", "build ok", "$ echo hi", "hi", "$"], &[], &[0, 2, 4], 2);
        let mut c = CopyMode::new(&d, 3);
        keys(&mut c, "[");
        assert_eq!(c.cursor.row, 2);
        keys(&mut c, "[");
        assert_eq!(c.cursor.row, 0);
        assert_eq!(c.top, 0);
        keys(&mut c, "Vj");
        assert_eq!(keys(&mut c, "y"), Outcome::Yank("$ make\nbuild ok".into()));
    }

    #[test]
    fn search_is_smartcase_and_wraps() {
        let d = doc(&["Error one", "fine", "error two", "ERROR three"], &[], &[], 1);
        let mut c = CopyMode::new(&d, 2);
        keys(&mut c, "gg/error\n");
        assert_eq!(c.search.as_ref().unwrap().matches.len(), 3, "lowercase query ignores case");
        assert_eq!(c.cursor.row, 2);
        keys(&mut c, "n");
        assert_eq!(c.cursor.row, 3);
        keys(&mut c, "n");
        assert_eq!(c.cursor.row, 0, "wraps around");
        keys(&mut c, "/ERROR\n");
        assert_eq!(c.search.as_ref().unwrap().matches.len(), 1, "uppercase makes it exact");
        assert_eq!(c.match_at(3, 0), Some(true));
    }

    #[test]
    fn soft_wrapped_rows_join_and_esc_layers() {
        let d = doc(&["a long line that wra", "ps here", "next"], &[0], &[], 0);
        let mut c = CopyMode::new(&d, 3);
        keys(&mut c, "ggv");
        c.move_cursor(Pos { row: 1, col: 6 });
        assert_eq!(c.selected_text(), "a long line that wraps here");
        assert_eq!(keys(&mut c, "\x1b"), Outcome::Stay, "first Esc clears the selection");
        assert!(c.selection.is_none());
        assert_eq!(keys(&mut c, "\x1b"), Outcome::Exit);
    }

    #[test]
    fn older_history_keeps_the_cursor_on_its_line() {
        let d = doc(&["h1", "h2", "s1", "s2", "s3"], &[], &[], 2);
        let mut c = CopyMode::new(&d, 3);
        keys(&mut c, "gg");
        assert_eq!(c.cursor.row, 0);
        // One older line from the recording, and one printed since copy mode started.
        let deep = doc(&["o1", "h1", "h2", "s1", "s2", "s3", "p1"], &[], &[], 4);
        c.extend(&deep);
        assert_eq!(c.cursor.row, 1, "still on h1");
        assert_eq!(c.note.as_deref(), Some("1 older lines from the recording"));
    }

    #[test]
    fn huge_counts_are_capped() {
        let d = doc(&["abcd", "efgh"], &[], &[], 0);
        let mut c = CopyMode::new(&d, 2);
        keys(&mut c, "gg99999999999999999999999j");
        assert_eq!(c.cursor.row, 1);
        keys(&mut c, "99999999999999999999w");
        assert_eq!(c.cursor.row, 1);
    }

    #[test]
    fn counts_and_block_selection() {
        let d = doc(&["abcd", "efgh", "ijkl", "mnop"], &[], &[], 0);
        let mut c = CopyMode::new(&d, 4);
        keys(&mut c, "gg3j");
        assert_eq!(c.cursor.row, 3);
        let mut c = CopyMode::new(&d, 4);
        keys(&mut c, "gg");
        keys(&mut c, "l");
        c.key(KeyEvent::new(KeyCode::Char { c: 'v' }, Mods(Mods::CTRL)));
        keys(&mut c, "jl");
        assert_eq!(c.selected_text(), "bc\nfg");
    }
}
