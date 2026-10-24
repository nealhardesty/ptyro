//! Fuzzy pickers (M2.6, M2.7): the command palette, the session finder and the split picker are
//! all this one widget over different items. Matching is `nucleo-matcher` with smart case.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ptyro_core::SessionId;
use ptyro_core::input::{KeyCode, KeyEvent, Mods};

use crate::keymap::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Action(Command),
    Session(SessionId),
    Theme(String),
    /// Start a new shell session (offered by the split picker).
    NewShell,
    /// Wizard: an agent name (`claude`, `claude@work`), `@shell` or `@command`.
    Agent(String),
    /// Wizard: a new worktree of this project.
    NewWorktree(String),
    Worktree(String),
    /// Wizard: the current directory, no worktree.
    Here,
    Workspace(String),
    NewWorkspace,
    Editor(String),
    Script(String),
    Path(std::path::PathBuf),
    /// Wizard: there's no project here; pick one to add, then carry on (UX1.6).
    AddProject,
    /// Palette: go to this project in the sidebar (UX1.18).
    Project(String),
    /// Palette: run this lifecycle or run script in this worktree.
    RunScript(String, String),
    /// Health: fix finding N (UX1.20).
    Fix(usize),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    pub detail: String,
    /// The key bound to it, shown at the right.
    pub key: Option<String>,
    pub target: Target,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Stay,
    Close,
    Pick(Target),
}

pub struct Palette {
    pub title: String,
    pub query: String,
    items: Vec<Item>,
    /// Indices into `items`, best match first.
    pub filtered: Vec<usize>,
    pub selected: usize,
    matcher: Matcher,
    /// A short choice (finish: `p` a PR, `i` integrate): with nothing typed, an item's one-letter
    /// key picks it. Off, typing always filters (the palette's keys are only shown).
    hotkeys: bool,
}

impl Palette {
    pub fn new(title: impl Into<String>, items: Vec<Item>) -> Self {
        let mut p = Self {
            title: title.into(),
            query: String::new(),
            filtered: (0..items.len()).collect(),
            items,
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
            hotkeys: false,
        };
        p.refilter();
        p
    }

    /// One-letter item keys pick their item (see `hotkeys`).
    pub fn with_hotkeys(mut self) -> Self {
        self.hotkeys = true;
        self
    }

    /// More items, arrived after it opened (a project's scripts, UX1.18); the query applies.
    pub fn extend(&mut self, more: impl IntoIterator<Item = Item>) {
        self.items.extend(more);
        self.refilter();
    }

    pub fn item(&self, i: usize) -> &Item {
        &self.items[self.filtered[i]]
    }

    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let hay = format!("{} {}", it.label, it.detail);
                    pattern.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher).map(|s| (s, i))
                })
                .collect();
            // Best first; stable on ties so the original order (e.g. sidebar order) wins.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn key(&mut self, k: KeyEvent) -> Picked {
        let ctrl = k.mods.has(Mods::CTRL);
        match (k.code, ctrl) {
            (KeyCode::Esc, _) | (KeyCode::Char { c: 'c' | 'g' }, true) => return Picked::Close,
            (KeyCode::Enter, _) => {
                return match self.filtered.get(self.selected) {
                    Some(&i) => Picked::Pick(self.items[i].target.clone()),
                    None => Picked::Stay,
                };
            }
            (KeyCode::Down, _) | (KeyCode::Char { c: 'n' | 'j' }, true) | (KeyCode::Tab, _) => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + 1) % self.filtered.len();
                }
            }
            (KeyCode::Up, _) | (KeyCode::Char { c: 'p' | 'k' }, true) | (KeyCode::BackTab, _) => {
                if !self.filtered.is_empty() {
                    self.selected = (self.selected + self.filtered.len() - 1) % self.filtered.len();
                }
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.refilter();
            }
            (KeyCode::Char { c: 'u' }, true) => {
                self.query.clear();
                self.refilter();
            }
            (KeyCode::Char { c }, false)
                if self.hotkeys
                    && self.query.is_empty()
                    && let Some(i) = self
                        .items
                        .iter()
                        .position(|i| i.key.as_deref() == Some(c.encode_utf8(&mut [0; 4]))) =>
            {
                return Picked::Pick(self.items[i].target.clone());
            }
            (KeyCode::Char { c }, false) => {
                self.query.push(c);
                self.selected = 0;
                self.refilter();
            }
            _ => {}
        }
        Picked::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item {
            label: label.into(),
            detail: String::new(),
            key: None,
            target: Target::Theme(label.into()),
        }
    }

    fn typed(p: &mut Palette, s: &str) {
        for c in s.chars() {
            p.key(KeyEvent::char(c));
        }
    }

    #[test]
    fn a_short_choice_picks_by_its_key_and_the_palette_only_filters() {
        let keyed = |label: &str, k: &str| Item { key: Some(k.into()), ..item(label) };
        let items = || vec![keyed("a pull request", "p"), keyed("integrate", "i")];
        let mut p = Palette::new("Finish", items()).with_hotkeys();
        assert!(
            matches!(p.key(KeyEvent::char('i')), Picked::Pick(Target::Theme(t)) if t == "integrate")
        );
        // Once a filter is typed (starting with a letter that isn't a key), letters filter.
        let mut p = Palette::new("Finish", items()).with_hotkeys();
        typed(&mut p, "rq");
        assert_eq!(p.query, "rq");
        typed(&mut p, "i");
        assert_eq!(p.query, "rqi");
        // Without hotkeys (the palette), a letter always filters.
        let mut p = Palette::new("Palette", items());
        assert!(matches!(p.key(KeyEvent::char('i')), Picked::Stay));
        assert_eq!(p.query, "i");
    }

    #[test]
    fn fuzzy_filters_and_picks_the_best() {
        let mut p = Palette::new(
            "t",
            vec![item("pane.split_right"), item("session.new"), item("pane.zoom")],
        );
        typed(&mut p, "spr");
        assert_eq!(p.item(0).label, "pane.split_right");
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Enter, Mods::NONE)),
            Picked::Pick(Target::Theme("pane.split_right".into()))
        );
        typed(&mut p, "zzzz");
        assert!(p.filtered.is_empty());
        assert_eq!(p.key(KeyEvent::new(KeyCode::Enter, Mods::NONE)), Picked::Stay);
        assert_eq!(p.key(KeyEvent::new(KeyCode::Esc, Mods::NONE)), Picked::Close);
    }

    #[test]
    fn selection_wraps() {
        let mut p = Palette::new("t", vec![item("a"), item("b")]);
        p.key(KeyEvent::new(KeyCode::Up, Mods::NONE));
        assert_eq!(p.selected, 1);
        p.key(KeyEvent::new(KeyCode::Down, Mods::NONE));
        assert_eq!(p.selected, 0);
    }
}
