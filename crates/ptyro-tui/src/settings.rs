//! The settings panel (ROADMAP CFG1.1): every configuration option, found by section or by
//! search, with the value in effect, its default and where it comes from, edited in place by
//! what kind of value it is. It reads the schema in `ptyro_core::settings` and writes the files
//! through the same calls as `ptyro config set`, so the file keeps its comments, a bad value is
//! refused with the reason, and the daemon and every open screen pick the change up as the file
//! changes.
//!
//! The panel owns its state and answers keys with [`Action`]s the app carries out (toast,
//! reload, open an editor), so it can be driven in tests without a terminal.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;

use ptyro_core::config::{Config, key_label};
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::settings::{
    self, Apply, Binding, Collection, Entry, Kind, Layer, Layout, Lookups, Op, Origin, Scope,
    Section, Source, Standing, View,
};

use crate::app::App;
use crate::keymap;

/// What the app does for the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Close,
    /// The user's config changed: read it again (key bindings, theme…).
    Reload,
    Toast(String),
    ToastErr(String),
    /// Show this theme for now (`None`: back to the one in use).
    Theme(Option<String>),
    /// Run a TUI action (`daemon.restart`, `profile.new`).
    Run(&'static str),
    /// Open a file in the editor at a line.
    EditFile(PathBuf, Option<usize>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Sections,
    Items,
}

/// A repository being edited.
#[derive(Debug, Clone)]
pub struct Proj {
    pub name: String,
    pub root: PathBuf,
}

/// A text box with a cursor, edited like a shell line (a settings box; the chat composer, whose
/// lines `C-a`/`C-e`/`C-u`/`C-k` keep to the one the cursor is on).
#[derive(Debug, Clone, Default)]
pub struct LineEdit {
    pub text: String,
    /// In characters.
    pub at: usize,
}

impl LineEdit {
    pub(crate) fn with(text: &str) -> Self {
        Self { text: text.to_owned(), at: text.chars().count() }
    }

    fn byte(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }

    /// Where the cursor's line starts and ends (in characters).
    fn line(&self) -> (usize, usize) {
        let chars: Vec<char> = self.text.chars().collect();
        let at = self.at.min(chars.len());
        let start = chars[..at].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
        let end = chars[at..].iter().position(|c| *c == '\n').map_or(chars.len(), |i| at + i);
        (start, end)
    }

    /// Put `text` in at the cursor, which goes after it.
    pub(crate) fn insert(&mut self, text: &str) {
        let b = self.byte(self.at);
        self.text.insert_str(b, text);
        self.at += text.chars().count();
    }

    /// Returns whether the key edited the text.
    pub(crate) fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.mods.has(Mods::CTRL);
        let len = self.text.chars().count();
        self.at = self.at.min(len);
        let (start, end) = self.line();
        match key.code {
            KeyCode::Char { c: 'u' } if ctrl => {
                let (a, b) = (self.byte(start), self.byte(self.at));
                self.text.replace_range(a..b, "");
                self.at = start;
            }
            KeyCode::Char { c: 'k' } if ctrl => {
                let (a, b) = (self.byte(self.at), self.byte(end));
                self.text.replace_range(a..b, "");
            }
            KeyCode::Char { c: 'w' } if ctrl => {
                let end = self.byte(self.at);
                let head: Vec<char> = self.text[..end].chars().collect();
                let mut i = head.len();
                while i > 0 && head[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !head[i - 1].is_whitespace() {
                    i -= 1;
                }
                let start = self.byte(i);
                self.text.replace_range(start..end, "");
                self.at = i;
            }
            KeyCode::Char { c: 'a' } if ctrl => {
                self.at = start;
                return false;
            }
            KeyCode::Char { c: 'e' } if ctrl => {
                self.at = end;
                return false;
            }
            KeyCode::Char { c } if !ctrl && !key.mods.has(Mods::ALT) => {
                let b = self.byte(self.at);
                self.text.insert(b, c);
                self.at += 1;
            }
            KeyCode::Backspace if self.at > 0 => {
                let (a, b) = (self.byte(self.at - 1), self.byte(self.at));
                self.text.replace_range(a..b, "");
                self.at -= 1;
            }
            KeyCode::Delete if self.at < len => {
                let (a, b) = (self.byte(self.at), self.byte(self.at + 1));
                self.text.replace_range(a..b, "");
            }
            KeyCode::Left => {
                self.at = self.at.saturating_sub(1);
                return false;
            }
            KeyCode::Right => {
                self.at = (self.at + 1).min(len);
                return false;
            }
            KeyCode::Home => {
                self.at = start;
                return false;
            }
            KeyCode::End => {
                self.at = end;
                return false;
            }
            _ => return false,
        }
        true
    }
}

/// What a finished box does.
#[derive(Debug, Clone)]
enum Then {
    /// Set this key to the answer.
    Set(String),
    /// Set a member of a map (a prompt's text) to the answer.
    SetMember {
        path: &'static str,
        name: String,
    },
    /// A step of making a member: the answer is the next field.
    Make(Make),
    Rename {
        path: &'static str,
        from: String,
    },
}

/// The box takes the name of a table member (an agent, a theme…): what a name can't hold is
/// swapped for `_` as it is typed. Map keys (snippets, key bindings) are free text.
fn names_a_table(then: &Then) -> bool {
    let tables =
        |path| settings::collection(path).is_some_and(|c| matches!(c.layout, Layout::Tables));
    match then {
        Then::Rename { path, .. } => tables(path),
        Then::Make(m) => m.asking.is_none() && tables(m.path),
        _ => false,
    }
}

/// A member being made, a question at a time.
#[derive(Debug, Clone)]
struct Make {
    path: &'static str,
    name: String,
    fields: Vec<(String, String)>,
    /// The fields still to ask for.
    todo: Vec<&'static str>,
    /// Which one the open box answers.
    asking: Option<&'static str>,
}

#[derive(Debug, Clone)]
enum Modal {
    Input {
        title: String,
        hint: String,
        edit: LineEdit,
        secret: bool,
        then: Then,
        check: Option<String>,
    },
    Pick(Pick),
    /// Press keys: the next key presses are the answer.
    Capture {
        title: String,
        keys: Vec<String>,
        then: CaptureThen,
    },
    Confirm {
        text: String,
        then: Confirmed,
    },
}

#[derive(Debug, Clone)]
enum CaptureThen {
    /// `ui.prefix`: the keys are the setting.
    Prefix,
    /// A new key binding in a table (`prefix`, `nav`, `global`): the key, then which action.
    Binding(&'static str),
    /// Another key for an existing binding (by its `[keys]` name).
    MoveKey(String),
}

#[derive(Debug, Clone)]
enum Confirmed {
    Remove {
        path: &'static str,
        name: String,
    },
    /// The key is in use. A new binding (`from` is `None`) takes it over, then asks what it runs;
    /// a moved one (`from` is the key it leaves) either swaps with it or takes it over.
    KeyOver {
        from: Option<String>,
        to: String,
        swap: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Pick {
    title: String,
    /// `(value, what it means)`.
    items: Vec<(String, String)>,
    /// Typed to narrow the list (only when `search`).
    query: String,
    search: bool,
    sel: usize,
    /// Several can be on (a set); `None`: one is picked.
    on: Option<BTreeSet<String>>,
    then: PickThen,
}

#[derive(Debug, Clone)]
enum PickThen {
    Set(String),
    Make(Make),
    /// The action for a key binding (by its `[keys]` name).
    BindingAction(String),
    /// What to do with a key binding.
    KeyMenu(String),
}

impl Pick {
    fn shown(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        (0..self.items.len())
            .filter(|i| {
                q.is_empty()
                    || self.items[*i].0.to_lowercase().contains(&q)
                    || self.items[*i].1.to_lowercase().contains(&q)
            })
            .collect()
    }

    fn current(&self) -> Option<&(String, String)> {
        self.shown().get(self.sel).map(|i| &self.items[*i])
    }
}

/// One undoable change.
#[derive(Debug, Clone)]
struct Undo {
    layer: Layer,
    before: String,
    /// The file as the change left it: undo only puts back over exactly that.
    after: String,
    label: String,
}

/// A row of the items pane.
#[derive(Debug, Clone)]
enum Row {
    Title(String),
    Hint(String),
    /// An entry of the current view: `(project?, index)`.
    Setting(bool, usize),
    Member {
        path: &'static str,
        name: String,
        summary: String,
        changed: usize,
    },
    /// A prompt (the other map collection).
    Binding {
        path: &'static str,
        name: String,
        value: String,
    },
    /// A key, whatever it stands at: a default, yours, changed or unbound.
    Key(Binding),
    /// Add a key to a table (`prefix`, `nav`, `global`).
    AddKey(&'static str),
    Add {
        path: &'static str,
        label: String,
    },
}

impl Row {
    fn selectable(&self) -> bool {
        !matches!(self, Self::Title(_) | Self::Hint(_))
    }
}

/// Where things were drawn, for the mouse.
#[derive(Debug, Clone, Default)]
pub struct Hits {
    pub sections: Vec<(ratatui::layout::Rect, usize)>,
    pub rows: Vec<(ratatui::layout::Rect, usize)>,
    pub panel: ratatui::layout::Rect,
}

pub struct Panel {
    user: Layer,
    project: Option<Proj>,
    /// Project changes go to the committed `.ptyro.toml`, not your `.ptyro.local.toml`.
    shared: bool,
    lk: Lookups,
    uview: View,
    pview: Option<View>,
    focus: Pane,
    /// The sections shown, in order; project ones follow the user's.
    sections: Vec<&'static Section>,
    sec: usize,
    cursor: usize,
    top: usize,
    drill: Option<(&'static str, String)>,
    modified_only: bool,
    advanced: bool,
    search: Option<LineEdit>,
    modal: Option<Modal>,
    undo: Vec<Undo>,
    /// Keys changed that need the daemon restarted.
    restart: BTreeSet<String>,
    note: Option<(String, bool)>,
    help: bool,
    pub hits: RefCell<Hits>,
}

/// The checks only the TUI can make of a whole user config: key bindings and themes.
fn semantic(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    let problems = keymap::validate(&config);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    if !crate::theme::exists(&config, &config.ui.theme) {
        return Err(format!(
            "unknown theme {:?} (built in: {}; or define [themes.{}])",
            config.ui.theme,
            crate::theme::BUILTIN.join(", "),
            config.ui.theme
        ));
    }
    Ok(())
}

fn no_extra(_: &str) -> Result<(), String> {
    Ok(())
}

fn lookups() -> Lookups {
    Lookups {
        themes: crate::theme::BUILTIN.iter().map(|s| (*s).to_owned()).collect(),
        actions: keymap::ACTIONS.iter().map(|a| (a.id.to_owned(), a.desc.to_owned())).collect(),
        keys: keymap::default_keys(),
    }
}

impl Panel {
    pub fn new(config_path: PathBuf, project: Option<Proj>) -> Self {
        let user = Layer::User(config_path);
        let lk = lookups();
        let uview = settings::view(&user, &lk);
        let mut p = Self {
            user,
            project,
            shared: false,
            lk,
            uview,
            pview: None,
            focus: Pane::Sections,
            sections: settings::SECTIONS.iter().collect(),
            sec: 0,
            cursor: 0,
            top: 0,
            drill: None,
            modified_only: false,
            advanced: false,
            search: None,
            modal: None,
            undo: Vec::new(),
            restart: BTreeSet::new(),
            note: None,
            help: false,
            hits: RefCell::default(),
        };
        p.refresh();
        p.cursor = p.first_selectable(0);
        p
    }

    /// Open on the section and setting of `key`.
    pub fn at(mut self, key: &str) -> Self {
        self.jump(key);
        self
    }

    fn project_layer(&self) -> Option<Layer> {
        let p = self.project.as_ref()?;
        Some(if self.shared { Layer::Shared(p.root.clone()) } else { Layer::Local(p.root.clone()) })
    }

    /// Read the files again (after a change, or when the daemon says the config changed).
    pub fn refresh(&mut self) {
        self.uview = settings::view(&self.user, &self.lk);
        self.pview = self.project_layer().map(|l| settings::view(&l, &self.lk));
        // A member opened here that has gone (removed by something else, or renumbered): back out.
        if let Some((path, name)) = &self.drill {
            let project = Self::is_project(self.section());
            let there = self
                .view(project)
                .members
                .iter()
                .any(|(p, names)| p == path && names.contains(name));
            if !there {
                self.drill = None;
            }
        }
        self.clamp();
    }

    fn view(&self, project: bool) -> &View {
        if project { self.pview.as_ref().unwrap_or(&self.uview) } else { &self.uview }
    }

    fn entry(&self, project: bool, i: usize) -> &Entry {
        &self.view(project).entries[i]
    }

    pub fn open_modal(&self) -> bool {
        self.modal.is_some()
    }

    // ── rows ──

    fn section(&self) -> &'static Section {
        self.sections[self.sec.min(self.sections.len() - 1)]
    }

    fn is_project(sec: &Section) -> bool {
        sec.scope == Scope::Project
    }

    /// Whether a setting is relevant next to its siblings (a sink's `url` is for `ntfy`).
    fn relevant(&self, project: bool, e: &Entry) -> bool {
        let Some(w) = e.setting.when else { return true };
        let Some((path, name)) = &e.member else { return true };
        let sibling = format!("{path}.{name}.{}", w.key);
        self.view(project)
            .entries
            .iter()
            .find(|x| x.key == sibling)
            .and_then(|x| x.value.as_str())
            .is_some_and(|v| w.any.contains(&v))
    }

    fn rows(&self) -> Vec<Row> {
        if let Some(q) = &self.search {
            return self.search_rows(&q.text);
        }
        let sec = self.section();
        let project = Self::is_project(sec);
        if project && self.project.is_none() {
            return hint_rows(
                "These are a repository's own settings (.ptyro.toml). Open Settings from a session or worktree of a project to edit them.",
            );
        }
        let view = self.view(project);
        let mut rows = Vec::new();
        if let Some((path, name)) = &self.drill {
            let col = settings::collection(path).expect("a collection");
            for (i, e) in view.entries.iter().enumerate() {
                if e.member.as_ref().is_some_and(|(p, n)| p == path && n == name)
                    && self.relevant(project, e)
                    && (self.advanced || !e.setting.advanced)
                {
                    rows.push(Row::Setting(project, i));
                }
            }
            if rows.is_empty() {
                rows.push(Row::Hint(format!("this {} has no settings to show", col.noun)));
            }
            return rows;
        }
        let keep = |e: &Entry| {
            (!self.modified_only || e.changed())
                && (self.advanced || !e.setting.advanced || e.changed())
        };
        for (i, e) in view.entries.iter().enumerate() {
            if e.setting.section == sec.id && e.member.is_none() && keep(e) {
                rows.push(Row::Setting(project, i));
            }
        }
        for col in settings::COLLECTIONS.iter().filter(|c| c.section == sec.id) {
            if !rows.is_empty() {
                rows.push(Row::Title(String::new()));
            }
            rows.push(Row::Title(col.title.to_owned()));
            if col.path == "keys" {
                rows.extend(self.key_rows(view));
                continue;
            }
            match col.layout {
                Layout::Map(_) => {
                    let items = view
                        .maps
                        .iter()
                        .find(|(p, _)| *p == col.path)
                        .map(|(_, m)| m.as_slice())
                        .unwrap_or(&[]);
                    if items.is_empty() {
                        rows.extend(hint_rows(col.empty));
                    }
                    for (k, v) in items {
                        rows.push(Row::Binding {
                            path: col.path,
                            name: k.clone(),
                            value: v.clone(),
                        });
                    }
                }
                _ => {
                    let names = view
                        .members
                        .iter()
                        .find(|(p, _)| *p == col.path)
                        .map(|(_, n)| n.as_slice())
                        .unwrap_or(&[]);
                    if names.is_empty() {
                        rows.extend(hint_rows(col.empty));
                    }
                    for n in names {
                        let fields: Vec<&Entry> = view
                            .entries
                            .iter()
                            .filter(|e| {
                                e.member.as_ref().is_some_and(|(p, m)| p == col.path && m == n)
                            })
                            .collect();
                        rows.push(Row::Member {
                            path: col.path,
                            name: n.clone(),
                            summary: summarize(col, n, &fields),
                            changed: fields.iter().filter(|e| e.changed()).count(),
                        });
                    }
                }
            }
            rows.push(Row::Add {
                path: col.path,
                label: format!("Add {} {}…", article(col.noun), col.noun),
            });
        }
        if rows.is_empty() {
            rows.push(Row::Hint(if self.modified_only {
                "nothing here differs from the default".into()
            } else {
                "nothing in this section".into()
            }));
        }
        rows
    }

    /// Every key, by when it works: after the prefix, in the sidebar, anywhere.
    fn key_rows(&self, view: &View) -> Vec<Row> {
        let prefix = self
            .find_entry("ui.prefix")
            .and_then(|e| {
                e.value
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "C-q".to_owned());
        let mut rows = Vec::new();
        for (table, title) in [
            ("prefix", format!("After the prefix key ({prefix}, then the key)")),
            ("nav", "In the sidebar (while it has focus)".to_owned()),
            (
                "global",
                "Anywhere, without the prefix (a global key takes the key from programs)"
                    .to_owned(),
            ),
        ] {
            rows.push(Row::Title(String::new()));
            rows.push(Row::Title(title));
            rows.extend(
                view.bindings
                    .iter()
                    .filter(|b| {
                        b.table == table && (!self.modified_only || b.standing != Standing::Default)
                    })
                    .map(|b| Row::Key(b.clone())),
            );
            rows.push(Row::AddKey(match table {
                "nav" => "nav",
                "global" => "global",
                _ => "prefix",
            }));
        }
        rows.remove(0);
        rows
    }

    fn search_rows(&self, query: &str) -> Vec<Row> {
        let words: Vec<String> =
            query.to_lowercase().split_whitespace().map(str::to_owned).collect();
        let mut out = Vec::new();
        for (project, view) in
            [(false, &self.uview), (true, self.pview.as_ref().unwrap_or(&self.uview))]
        {
            if project && self.pview.is_none() {
                break;
            }
            for (i, e) in view.entries.iter().enumerate() {
                if words.is_empty() {
                    continue;
                }
                let hay = format!("{} {} {} {}", e.key, e.setting.help, e.shown(), e.setting.also)
                    .to_lowercase();
                if words.iter().all(|w| hay.contains(w)) && self.relevant(project, e) {
                    out.push(Row::Setting(project, i));
                }
            }
            for b in &view.bindings {
                let hay = format!(
                    "keys key binding shortcut {} {} {} {} {}",
                    b.name,
                    b.action,
                    b.help,
                    b.standing.says(),
                    b.when
                )
                .to_lowercase();
                if !words.is_empty() && words.iter().all(|w| hay.contains(w)) {
                    out.push(Row::Key(b.clone()));
                }
            }
            for (path, items) in view.maps.iter().filter(|(p, _)| p != "keys") {
                for (k, v) in items {
                    let hay = format!("{path}.{k} {v}").to_lowercase();
                    if !words.is_empty() && words.iter().all(|w| hay.contains(w)) {
                        out.push(Row::Binding {
                            path: settings::collection(path).map_or("keys", |c| c.path),
                            name: k.clone(),
                            value: v.clone(),
                        });
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(Row::Hint(if words.is_empty() {
                "type to search every setting, its help and its value".into()
            } else {
                "no setting matches: try fewer or different words (esc goes back to the sections)"
                    .into()
            }));
        }
        out
    }

    fn first_selectable(&self, from: usize) -> usize {
        let rows = self.rows();
        (from..rows.len()).find(|i| rows[*i].selectable()).unwrap_or(0)
    }

    fn clamp(&mut self) {
        let rows = self.rows();
        if self.cursor >= rows.len() {
            self.cursor = rows.len().saturating_sub(1);
        }
        if !rows.get(self.cursor).is_some_and(Row::selectable) {
            self.cursor = (0..rows.len())
                .rev()
                .find(|i| rows[*i].selectable() && *i <= self.cursor)
                .or_else(|| (0..rows.len()).find(|i| rows[*i].selectable()))
                .unwrap_or(0);
        }
    }

    fn step(&mut self, down: bool, by: usize) {
        let rows = self.rows();
        let mut i = self.cursor;
        for _ in 0..by {
            let next = if down {
                (i + 1..rows.len()).find(|j| rows[*j].selectable())
            } else {
                (0..i).rev().find(|j| rows[*j].selectable())
            };
            match next {
                Some(n) => i = n,
                None => break,
            }
        }
        self.cursor = i;
    }

    /// Show the section and row of `key`.
    pub fn jump(&mut self, key: &str) {
        self.search = None;
        self.drill = None;
        let found = [(false, &self.uview), (true, self.pview.as_ref().unwrap_or(&self.uview))]
            .into_iter()
            .find_map(|(p, v)| v.entries.iter().position(|e| e.key == key).map(|i| (p, i)));
        if let Some((project, i)) = found {
            let e = self.entry(project, i).clone();
            // (Looked for by name, an advanced setting is shown.)
            self.advanced |= e.setting.advanced;
            if let Some(s) = self.sections.iter().position(|s| s.id == e.setting.section) {
                self.sec = s;
            }
            if let Some((path, name)) = e.member.clone() {
                self.drill = settings::collection(&path).map(|c| (c.path, name));
            }
            self.focus = Pane::Items;
            let rows = self.rows();
            self.cursor = rows
                .iter()
                .position(|r| matches!(r, Row::Setting(p, j) if *p == project && *j == i))
                .unwrap_or(0);
        } else if let Some((c, name)) = settings::member_of(key) {
            if let Some(s) = self.sections.iter().position(|s| s.id == c.section) {
                self.sec = s;
            }
            self.focus = Pane::Items;
            let rows = self.rows();
            self.cursor = rows
                .iter()
                .position(|r| match r {
                    Row::Binding { path, name: n, .. } => *path == c.path && *n == name,
                    Row::Key(b) => {
                        c.path == "keys" && settings::slot(&b.name) == settings::slot(&name)
                    }
                    _ => false,
                })
                .unwrap_or(0);
        }
    }

    // ── changing things ──

    fn layer_of(&self, e: &Entry) -> Layer {
        match e.setting.section {
            s if settings::section(s).is_some_and(|x| x.scope == Scope::Project) => {
                self.project_layer().unwrap_or_else(|| self.user.clone())
            }
            _ => self.user.clone(),
        }
    }

    /// The layer a project value is in now (so resetting removes it where it is).
    fn layer_of_origin(&self, e: &Entry) -> Layer {
        let root = self.project.as_ref().map(|p| p.root.clone());
        match (e.origin, root) {
            (Origin::Project, Some(r)) => Layer::Shared(r),
            (Origin::Local, Some(r)) => Layer::Local(r),
            _ => self.layer_of(e),
        }
    }

    /// The file that has `path.name` (a member of a collection), or where one would go: yours for
    /// user settings; for a repository's, the one of its two files that holds it.
    fn holder(&self, path: &str, name: &str) -> Layer {
        let project = settings::collection(path)
            .and_then(|c| settings::section(c.section))
            .is_some_and(|s| s.scope == Scope::Project);
        if !project {
            return self.user.clone();
        }
        let Some(p) = &self.project else { return self.user.clone() };
        let (local, shared) = (Layer::Local(p.root.clone()), Layer::Shared(p.root.clone()));
        let has = |l: &Layer| {
            let text = l.read().unwrap_or_default();
            settings::members(&text, path).iter().any(|m| m == name)
                || settings::is_set(&text, &format!("{path}.{name}"))
        };
        if has(&local) {
            local
        } else if has(&shared) {
            shared
        } else {
            self.project_layer().unwrap_or_else(|| self.user.clone())
        }
    }

    /// Take a binding or a prompt out.
    fn unbind(&mut self, path: &'static str, name: &str) -> Vec<Action> {
        let layer = self.holder(path, name);
        let what = if path == "keys" { "unbound" } else { "removed" };
        self.change(layer, Op::Unset { key: format!("{path}.{name}") }, &format!("{name} {what}"))
    }

    // ── key bindings ──

    fn key_named(&self, name: &str) -> Option<Binding> {
        let at = settings::slot(name);
        self.uview.bindings.iter().find(|b| !b.fixed && settings::slot(&b.name) == at).cloned()
    }

    /// Put the cursor on the key `name`, wherever the list put it.
    fn focus_key(&mut self, name: &str) {
        let at = settings::slot(name);
        let rows = self.rows();
        if let Some(i) =
            rows.iter().position(|r| matches!(r, Row::Key(b) if settings::slot(&b.name) == at))
        {
            self.cursor = i;
        }
        self.clamp();
    }

    /// One change to bindings, as one undo.
    fn bind(&mut self, changes: Vec<(String, Option<String>)>, label: &str) -> Vec<Action> {
        let layer = self.user.clone();
        self.change(layer, Op::Bind { changes }, label)
    }

    fn key_fixed(&mut self) {
        self.note = Some((
            "1 to 9 show sessions 1 to 9. To change one, bind a key of your own to session.focus and a number"
                .into(),
            false,
        ));
    }

    /// Switch a key off: a default is kept in the file as `none` (reset brings it back); a key of
    /// your own is simply taken out.
    fn key_unbind(&mut self, b: &Binding) -> Vec<Action> {
        if b.fixed {
            self.key_fixed();
            return vec![];
        }
        if b.standing == Standing::Unbound {
            self.note = Some((format!("{} is already unbound: r brings it back", b.key), false));
            return vec![];
        }
        let (value, what) = match b.default {
            Some(_) => (Some("none".to_owned()), "unbound (r brings it back)"),
            None => (None, "removed"),
        };
        let label = format!("{} {what}", b.key);
        let acts = self.bind(vec![(b.name.clone(), value)], &label);
        self.focus_key(&b.name);
        acts
    }

    /// Back to the default (a key of your own has none: it goes).
    fn key_reset(&mut self, b: &Binding) -> Vec<Action> {
        if b.fixed || !b.mine {
            self.note = Some((format!("{} is already the default", b.key), false));
            return vec![];
        }
        let label = match &b.default {
            Some(d) => format!("{} is back to the default ({})", b.key, gist(&self.help_of(d))),
            None => format!("{} removed", b.key),
        };
        let acts = self.bind(vec![(b.name.clone(), None)], &label);
        self.focus_key(&b.name);
        acts
    }

    fn help_of(&self, action: &str) -> String {
        settings::describe(&self.lk, action)
    }

    /// What can be done with a key.
    fn key_menu(&mut self, b: &Binding) -> Vec<Action> {
        if b.fixed {
            self.key_fixed();
            return vec![];
        }
        let mut items: Vec<(String, String)> = Vec::new();
        if b.standing != Standing::Unbound {
            items.push(("key".into(), "Change its key: press the one you want".into()));
        }
        items.push(("action".into(), "Change what it runs".into()));
        match b.standing {
            Standing::Unbound => {}
            Standing::Yours => items.push(("unbind".into(), "Remove this key".into())),
            _ => items.push(("unbind".into(), "Unbind it: the key does nothing".into())),
        }
        if b.mine {
            let label = match &b.default {
                Some(d) => format!("Back to the default: {}", gist(&self.help_of(d))),
                None => "Remove this key".to_owned(),
            };
            if !(b.default.is_none() && b.standing != Standing::Unbound) {
                items.push(("reset".into(), label));
            }
        }
        let shown = if b.standing == Standing::Unbound { "unbound" } else { b.action.as_str() };
        self.modal = Some(Modal::Pick(Pick {
            title: format!("{} {} ({shown}): {}", b.when, b.key, gist(&b.help)),
            items,
            query: String::new(),
            search: false,
            sel: 0,
            on: None,
            then: PickThen::KeyMenu(b.name.clone()),
        }));
        vec![]
    }

    fn key_menu_done(&mut self, name: &str, what: &str) -> Vec<Action> {
        let Some(b) = self.key_named(name) else { return vec![] };
        match what {
            "key" => {
                self.modal = Some(Modal::Capture {
                    title: format!("Press the new key for “{}” ({})", gist(&b.help), b.when),
                    keys: Vec::new(),
                    then: CaptureThen::MoveKey(b.name.clone()),
                });
                vec![]
            }
            "action" => {
                let items = self.action_items();
                let now = b.action.split_whitespace().next().unwrap_or_default();
                self.modal = Some(Modal::Pick(Pick {
                    title: format!("{} {} should run", b.when, b.key),
                    sel: items.iter().position(|(v, _)| v == now).unwrap_or(0),
                    items,
                    query: String::new(),
                    search: true,
                    on: None,
                    then: PickThen::BindingAction(b.name.clone()),
                }));
                vec![]
            }
            "unbind" => self.key_unbind(&b),
            _ => self.key_reset(&b),
        }
    }

    /// Ask for a new key in `table`.
    fn add_key(&mut self, table: &'static str) -> Vec<Action> {
        self.modal = Some(Modal::Capture {
            title: format!("New key binding, {} — press the key", settings::when(table)),
            keys: Vec::new(),
            then: CaptureThen::Binding(table),
        });
        vec![]
    }

    /// The key `label` in `table`, pressed to be a new binding or another key for `from`.
    fn key_pressed(
        &mut self,
        table: &'static str,
        label: &str,
        from: Option<String>,
    ) -> Vec<Action> {
        let to = settings::name_in(table, label);
        let mover = from.as_deref().and_then(|f| self.key_named(f));
        if mover.as_ref().is_some_and(|m| settings::slot(&m.name) == settings::slot(&to)) {
            self.note = Some((format!("{label} is already its key"), false));
            return vec![];
        }
        let owner = settings::owner(&self.uview.bindings, &to).cloned();
        let Some(o) = owner else { return self.key_placed(from, to, false) };
        let swap = mover.is_some() && !o.fixed;
        let what = gist(&o.help);
        let text = match (&mover, swap) {
            (Some(m), true) => format!(
                "{label} already runs “{what}”. Swap them: {label} will run “{}”, and {} will run “{what}”?",
                gist(&m.help),
                m.key
            ),
            (Some(_), false) => format!("{label} already runs “{what}”. Use it for this instead?"),
            (None, _) => {
                format!("{label} already runs “{what}”. Use it for something else instead?")
            }
        };
        self.modal = Some(Modal::Confirm { text, then: Confirmed::KeyOver { from, to, swap } });
        vec![]
    }

    /// Settled: move `from`'s binding onto `to` (swapping with what was there), or, for a new
    /// binding, ask what `to` runs.
    fn key_placed(&mut self, from: Option<String>, to: String, swap: bool) -> Vec<Action> {
        let Some(from) = from else {
            let items = self.action_items();
            self.modal = Some(Modal::Pick(Pick {
                title: format!("{} should run", settings::slot(&to).1),
                items,
                query: String::new(),
                search: true,
                sel: 1,
                on: None,
                then: PickThen::BindingAction(to),
            }));
            return vec![];
        };
        let Some(m) = self.key_named(&from) else { return vec![] };
        let other = settings::owner(&self.uview.bindings, &to).filter(|o| !o.fixed).cloned();
        let mut changes = vec![(to.clone(), Some(m.action.clone()))];
        // The old key: given to what was on the new one, or switched off (a key of your own goes).
        changes.push(match (&other, swap) {
            (Some(o), true) => (m.name.clone(), Some(o.action.clone())),
            _ if m.default.is_some() => (m.name.clone(), Some("none".to_owned())),
            _ => (m.name.clone(), None),
        });
        let label = match (&other, swap) {
            (Some(o), true) => format!(
                "{} and {} swapped ({} ↔ {})",
                m.key,
                settings::slot(&to).1,
                gist(&m.help),
                gist(&o.help)
            ),
            _ => format!("“{}” is now on {}", gist(&m.help), settings::slot(&to).1),
        };
        let acts = self.bind(changes, &label);
        self.focus_key(&to);
        acts
    }

    fn extra_for(layer: &Layer) -> fn(&str) -> Result<(), String> {
        if layer.scope() == Scope::User { semantic } else { no_extra }
    }

    fn change(&mut self, layer: Layer, op: Op, label: &str) -> Vec<Action> {
        let extra = Self::extra_for(&layer);
        match settings::apply(&layer, &op, &self.lk, &extra) {
            Ok(done) => {
                if done.changed {
                    self.undo.push(Undo {
                        layer: layer.clone(),
                        before: done.before.clone(),
                        after: done.after.clone(),
                        label: label.to_owned(),
                    });
                    if self.undo.len() > 50 {
                        self.undo.remove(0);
                    }
                    if done.apply == Apply::Restart {
                        let key = match &op {
                            Op::Set { key, .. } | Op::Unset { key } => key.clone(),
                            Op::Add { path, .. }
                            | Op::Remove { path, .. }
                            | Op::Rename { path, .. } => path.clone(),
                            Op::Bind { .. } => "keys".to_owned(),
                        };
                        self.restart.insert(key);
                    }
                }
                self.refresh();
                let notes = done.notes.join(" · ");
                self.note =
                    Some((if done.changed { format!("{label} — {notes}") } else { notes }, false));
                if layer.scope() == Scope::User && done.changed {
                    vec![Action::Reload]
                } else {
                    vec![]
                }
            }
            Err(e) => {
                self.note = Some((e, true));
                vec![]
            }
        }
    }

    fn set(&mut self, e: &Entry, value: &str) -> Vec<Action> {
        let layer = self.layer_of(e);
        let label = format!("{} = {}", e.key, value.trim());
        self.change(layer, Op::Set { key: e.key.clone(), value: value.to_owned() }, &label)
    }

    fn reset(&mut self, e: &Entry) -> Vec<Action> {
        if e.origin == Origin::Default {
            self.note = Some((format!("{} is already the default", e.key), false));
            return vec![];
        }
        let layer = self.layer_of_origin(e);
        let label = format!("{} reset", e.key);
        self.change(layer, Op::Unset { key: e.key.clone() }, &label)
    }

    fn undo_last(&mut self) -> Vec<Action> {
        let Some(u) = self.undo.pop() else {
            self.note = Some(("nothing to undo".into(), false));
            return vec![];
        };
        let extra = Self::extra_for(&u.layer);
        match settings::restore(&u.layer, &u.before, Some(&u.after), &extra) {
            Ok(()) => {
                self.refresh();
                self.note = Some((format!("undone: {}", u.label), false));
                if u.layer.scope() == Scope::User { vec![Action::Reload] } else { vec![] }
            }
            Err(e) => {
                self.note = Some((format!("can't undo: {e}"), true));
                vec![]
            }
        }
    }

    // ── keys ──

    /// A key press. Returns what the app should do.
    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        if let Some(m) = self.modal.take() {
            return self.modal_key(m, key);
        }
        if self.search.is_some() {
            return self.search_key(key);
        }
        self.note = None;
        match self.focus {
            Pane::Sections => self.sections_key(key),
            Pane::Items => self.items_key(key),
        }
    }

    fn sections_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Esc | KeyCode::Char { c: 'q' } => return vec![Action::Close],
            KeyCode::Up | KeyCode::Char { c: 'k' } => self.move_section(false),
            KeyCode::Down | KeyCode::Char { c: 'j' } => self.move_section(true),
            KeyCode::Home | KeyCode::Char { c: 'g' } => self.set_section(0),
            KeyCode::End | KeyCode::Char { c: 'G' } => self.set_section(self.sections.len() - 1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Tab | KeyCode::Char { c: 'l' } => {
                self.focus = Pane::Items
            }
            _ => return self.common_key(key),
        }
        vec![]
    }

    fn move_section(&mut self, down: bool) {
        let n = self.sections.len();
        self.set_section(if down { (self.sec + 1).min(n - 1) } else { self.sec.saturating_sub(1) });
    }

    fn set_section(&mut self, i: usize) {
        if i != self.sec {
            self.sec = i;
            self.drill = None;
            self.top = 0;
            self.cursor = self.first_selectable(0);
        }
    }

    /// Keys that work in either pane.
    fn common_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Char { c: '/' } => {
                self.search = Some(LineEdit::default());
                self.cursor = 0;
            }
            KeyCode::Char { c: 'm' } => {
                self.modified_only = !self.modified_only;
                self.clamp();
                self.note = Some((
                    if self.modified_only {
                        "showing only what differs from the default"
                    } else {
                        "showing everything"
                    }
                    .into(),
                    false,
                ));
            }
            KeyCode::Char { c: 'A' } => {
                self.advanced = !self.advanced;
                self.clamp();
                self.note = Some((
                    if self.advanced {
                        "showing advanced settings too"
                    } else {
                        "hiding advanced settings"
                    }
                    .into(),
                    false,
                ));
            }
            KeyCode::Char { c: 'u' } => return self.undo_last(),
            KeyCode::Char { c: '?' } => self.help = !self.help,
            KeyCode::Char { c: 's' } => {
                if self.project.is_some() {
                    self.shared = !self.shared;
                    self.refresh();
                    self.note = Some((
                        if self.shared {
                            "repository settings now save to .ptyro.toml, which is committed and shared"
                        } else {
                            "repository settings now save to .ptyro.local.toml, yours alone"
                        }
                        .into(),
                        false,
                    ));
                }
            }
            KeyCode::Char { c: '!' } => {
                if self.restart.is_empty() {
                    self.note = Some(("nothing needs a restart".into(), false));
                } else {
                    self.restart.clear();
                    return vec![Action::Run("daemon.restart")];
                }
            }
            KeyCode::Char { c: 'e' } => {
                let p = self.user.path();
                return vec![Action::EditFile(p, None)];
            }
            _ => {}
        }
        vec![]
    }

    fn items_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let rows = self.rows();
        let row = rows.get(self.cursor).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char { c: 'h' } => {
                if self.drill.take().is_some() {
                    self.cursor = 0;
                    self.clamp();
                } else {
                    self.focus = Pane::Sections;
                }
            }
            KeyCode::Tab | KeyCode::BackTab => self.focus = Pane::Sections,
            KeyCode::Up | KeyCode::Char { c: 'k' } => self.step(false, 1),
            KeyCode::Down | KeyCode::Char { c: 'j' } => self.step(true, 1),
            KeyCode::PageUp => self.step(false, 8),
            KeyCode::PageDown => self.step(true, 8),
            KeyCode::Home | KeyCode::Char { c: 'g' } => {
                self.cursor = self.first_selectable(0);
            }
            KeyCode::End | KeyCode::Char { c: 'G' } => {
                self.cursor = (0..rows.len()).rev().find(|i| rows[*i].selectable()).unwrap_or(0);
            }
            KeyCode::Char { c: ']' } => {
                self.move_section(true);
                self.focus = Pane::Items;
            }
            KeyCode::Char { c: '[' } => {
                self.move_section(false);
                self.focus = Pane::Items;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char { c: 'l' | ' ' } => {
                if let Some(r) = row {
                    return self.activate(r, key.code == KeyCode::Char { c: ' ' });
                }
            }
            KeyCode::Char { c: c @ ('+' | '=' | '-') } => {
                if let Some(Row::Setting(p, i)) = row {
                    return self.nudge(p, i, if c == '-' { -1 } else { 1 });
                }
            }
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char { c: 'r' } => match row {
                Some(Row::Setting(p, i)) => {
                    let e = self.entry(p, i).clone();
                    return self.reset(&e);
                }
                Some(Row::Binding { path, name, .. }) => return self.unbind(path, &name),
                Some(Row::Key(b)) => return self.key_reset(&b),
                Some(Row::Member { path, name, .. }) => return self.ask_remove(path, &name),
                _ => {}
            },
            KeyCode::Char { c: 'd' } => match row {
                Some(Row::Member { path, name, .. }) => return self.ask_remove(path, &name),
                Some(Row::Binding { path, name, .. }) => return self.unbind(path, &name),
                Some(Row::Key(b)) => return self.key_unbind(&b),
                _ => {}
            },
            KeyCode::Char { c: 'R' } => {
                if let Some(Row::Member { path, name, .. }) = row {
                    return self.ask_rename(path, &name);
                }
            }
            KeyCode::Char { c: 'n' } if self.section().id == "keys" => {
                let table = match row {
                    Some(Row::Key(b)) => b.table,
                    Some(Row::AddKey(t)) => t,
                    _ => "prefix",
                };
                return self.add_key(table);
            }
            KeyCode::Char { c: 'n' } => {
                let sec = self.section();
                if let Some(col) = settings::COLLECTIONS.iter().find(|c| c.section == sec.id) {
                    return self.add(col);
                }
            }
            KeyCode::Char { c: 'o' } => {
                if let Some(Row::Setting(p, i)) = row {
                    let e = self.entry(p, i).clone();
                    let layer = self.layer_of_origin(&e);
                    return vec![Action::EditFile(layer.path(), e.line)];
                }
            }
            _ => return self.common_key(key),
        }
        vec![]
    }

    fn ask_remove(&mut self, path: &'static str, name: &str) -> Vec<Action> {
        if path == "profiles" {
            return vec![Action::Run("profile.remove")];
        }
        let col = settings::collection(path).expect("collection");
        let what = if matches!(col.layout, Layout::Array) {
            format!("the {} sink at position {name}", col.noun)
        } else {
            format!("the {} {name:?}", col.noun)
        };
        let in_use = path == "themes"
            && self.find_entry("ui.theme").is_some_and(|e| e.value.as_str() == Some(name));
        self.modal = Some(Modal::Confirm {
            text: format!(
                "Remove {what}, and everything set in it?{}",
                if in_use { " It is the theme in use: the default comes back." } else { "" }
            ),
            then: Confirmed::Remove { path, name: name.to_owned() },
        });
        vec![]
    }

    fn ask_rename(&mut self, path: &'static str, from: &str) -> Vec<Action> {
        if path == "profiles" {
            return vec![Action::Run("profile.rename")];
        }
        let col = settings::collection(path).expect("collection");
        if matches!(col.layout, Layout::Array) {
            self.note = Some(("sinks are ordered, not named".into(), false));
            return vec![];
        }
        self.modal = Some(Modal::Input {
            title: format!("Rename the {} {from:?}", col.noun),
            hint: "letters, digits, - and _".into(),
            edit: LineEdit::with(from),
            secret: false,
            then: Then::Rename { path, from: from.to_owned() },
            check: None,
        });
        vec![]
    }

    fn nudge(&mut self, project: bool, i: usize, by: i64) -> Vec<Action> {
        let e = self.entry(project, i).clone();
        let Kind::Int { min, max } = e.setting.kind else { return vec![] };
        let cur = e.value.as_i64().unwrap_or(min);
        if cur < min || cur > max {
            // Set by hand beyond the usual range: + and - don't pull it back.
            self.note = Some((format!("{} is outside {min} to {max}: type a value", e.key), false));
            return vec![];
        }
        let step = if matches!(by, -1 | 1) && (max - min) > 500 { 10 } else { 1 };
        let to = (cur + by * step).clamp(min, max);
        if to == cur {
            return vec![];
        }
        self.set(&e, &to.to_string())
    }

    fn activate(&mut self, row: Row, space: bool) -> Vec<Action> {
        match row {
            Row::Title(_) | Row::Hint(_) => vec![],
            Row::Member { path, name, .. } => {
                self.drill = Some((path, name));
                self.cursor = 0;
                self.clamp();
                vec![]
            }
            Row::Add { path, .. } => match settings::collection(path) {
                Some(col) => self.add(col),
                None => vec![],
            },
            Row::Key(b) => self.key_menu(&b),
            Row::AddKey(table) => self.add_key(table),
            Row::Binding { path: "prompts", name, value } => {
                self.modal = Some(Modal::Input {
                    title: format!("prompt {name}: the text a new session starts from"),
                    hint: String::new(),
                    edit: LineEdit::with(&value),
                    secret: false,
                    then: Then::SetMember { path: "prompts", name },
                    check: None,
                });
                vec![]
            }
            Row::Binding { path, name, value } => {
                let items = self.action_items();
                self.modal = Some(Modal::Pick(Pick {
                    title: format!("{name} runs"),
                    sel: items.iter().position(|(v, _)| *v == value).unwrap_or(0),
                    items,
                    query: String::new(),
                    search: true,
                    on: None,
                    then: PickThen::Set(format!("{path}.{name}")),
                }));
                vec![]
            }
            Row::Setting(p, i) => {
                let e = self.entry(p, i).clone();
                self.edit(&e, space)
            }
        }
    }

    fn action_items(&self) -> Vec<(String, String)> {
        let mut v = vec![("none".to_owned(), "unbind this key".to_owned())];
        v.extend(self.lk.actions.iter().cloned());
        v
    }

    fn choices_of(&self, kind: Kind, project: bool) -> Vec<(String, String)> {
        match kind {
            Kind::Choice { choices } | Kind::Set { choices } => {
                choices.iter().map(|c| (c.value.to_owned(), c.help.to_owned())).collect()
            }
            Kind::Dynamic { source } => {
                let mut v = self
                    .view(project)
                    .options
                    .iter()
                    .find(|(s, _)| *s == source.name())
                    .map(|(_, o)| o.clone())
                    .unwrap_or_default();
                if source == Source::Actions {
                    v = self.action_items();
                }
                v
            }
            _ => Vec::new(),
        }
    }

    /// Edit one setting by what it is.
    fn edit(&mut self, e: &Entry, space: bool) -> Vec<Action> {
        let st = e.setting;
        let project = settings::section(st.section).is_some_and(|s| s.scope == Scope::Project);
        match st.kind {
            Kind::Bool => {
                // Off → on → (default, when it can be unset): a plain switch for the rest.
                let cur = e.value.as_bool();
                // Unset, it is what the setting says it falls back to ("on" or "off").
                let fallback = e.setting.otherwise.starts_with("on");
                let to = !cur.unwrap_or(fallback);
                let optional = e.default.is_null();
                if optional && cur == Some(true) && e.origin != Origin::Default {
                    return self.set(e, "off");
                }
                if optional && cur == Some(false) && e.origin != Origin::Default {
                    return self.reset(e);
                }
                self.set(e, if to { "on" } else { "off" })
            }
            Kind::Choice { .. } | Kind::Dynamic { .. } => {
                let items = self.choices_of(st.kind, project);
                if items.is_empty() {
                    return self.text_box(e, space);
                }
                let cur = e.value.as_str().unwrap_or_default();
                self.modal = Some(Modal::Pick(Pick {
                    title: format!("{} — {}", e.key, st.help),
                    sel: items.iter().position(|(v, _)| v == cur).unwrap_or(0),
                    search: items.len() > 9,
                    items,
                    query: String::new(),
                    on: None,
                    then: PickThen::Set(e.key.clone()),
                }));
                // The theme shows as you move over it.
                if st.key == "ui.theme" {
                    return self.preview();
                }
                vec![]
            }
            Kind::Set { .. } => {
                let items = self.choices_of(st.kind, project);
                let on: BTreeSet<String> = e
                    .value
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                self.modal = Some(Modal::Pick(Pick {
                    title: format!("{} — space turns one on or off, ⏎ saves", e.key),
                    items,
                    query: String::new(),
                    search: false,
                    sel: 0,
                    on: Some(on),
                    then: PickThen::Set(e.key.clone()),
                }));
                vec![]
            }
            Kind::Keys => {
                self.modal = Some(Modal::Capture {
                    title: format!("{} — press the keys you want", e.key),
                    keys: Vec::new(),
                    then: CaptureThen::Prefix,
                });
                vec![]
            }
            Kind::Opaque => {
                let layer = self.layer_of_origin(e);
                self.note = Some((format!("{} is a table: opening the file at it", e.key), false));
                // Structured values are edited in the file, at their table.
                let line = e.line.or_else(|| {
                    settings::view(&layer, &self.lk)
                        .entries
                        .iter()
                        .find(|x| x.key == e.key)
                        .and_then(|x| x.line)
                });
                vec![Action::EditFile(layer.path(), line)]
            }
            _ => self.text_box(e, space),
        }
    }

    fn text_box(&mut self, e: &Entry, _space: bool) -> Vec<Action> {
        let st = e.setting;
        let current = match (&st.kind, &e.value) {
            (_, serde_json::Value::Null) => String::new(),
            (Kind::Secret, _) => String::new(),
            (k, v) => settings::show(k, v).unwrap_or_default(),
        };
        let hint = match st.kind {
            Kind::Int { min, max } => format!("a number from {min} to {max}"),
            Kind::Duration => "like 30s, 10m, 2h, 1d, or off".into(),
            Kind::Bytes => "like 16MiB, 512KiB, 2GB".into(),
            Kind::Window => "like 22:00-07:00".into(),
            Kind::List => "separate with commas".into(),
            Kind::Argv => "a program and its arguments, quoted like a shell".into(),
            Kind::Color => "#rrggbb, a 0–255 palette index, or a name like bright-blue".into(),
            Kind::Address => "host:port, like 127.0.0.1:7591".into(),
            Kind::Secret => "the token, or !program args to run one that prints it".into(),
            Kind::Path => "a path; ~ is your home".into(),
            _ => String::new(),
        };
        self.modal = Some(Modal::Input {
            title: format!("{} — {}", e.key, st.help),
            hint,
            edit: LineEdit::with(&current),
            secret: matches!(st.kind, Kind::Secret),
            then: Then::Set(e.key.clone()),
            check: None,
        });
        self.recheck();
        vec![]
    }

    /// Check the open box's answer against the real parser, so the reason shows as you type.
    fn recheck(&mut self) {
        let Some(Modal::Input { edit, then: Then::Set(key), .. }) = &self.modal else { return };
        let (key, input) = (key.clone(), edit.text.clone());
        let scope = settings::section(settings::find(&key).map_or("", |s| s.section))
            .map_or(Scope::User, |s| s.scope);
        let layer =
            if scope == Scope::User { Some(self.user.clone()) } else { self.project_layer() };
        let verdict = match layer {
            Some(l) => {
                let text = l.read().unwrap_or_default();
                let extra = Self::extra_for(&l);
                settings::set(scope, &text, &key, &input, &self.lk, &extra).map(|_| ())
            }
            None => Err("no repository".into()),
        };
        if let Some(Modal::Input { check, .. }) = &mut self.modal {
            *check = verdict.err();
        }
    }

    fn preview(&self) -> Vec<Action> {
        if let Some(Modal::Pick(p)) = &self.modal
            && matches!(&p.then, PickThen::Set(k) if k == "ui.theme")
            && let Some((v, _)) = p.current()
        {
            return vec![Action::Theme(Some(v.clone()))];
        }
        vec![]
    }

    fn add(&mut self, col: &'static Collection) -> Vec<Action> {
        match (col.path, col.layout) {
            ("profiles", _) => vec![Action::Run("profile.new")],
            (_, Layout::Map(_)) if col.path == "keys" => self.add_key("prefix"),
            (_, Layout::Array) => {
                let types = [
                    ("ntfy", "ntfy.sh or your own"),
                    ("pushover", "Pushover"),
                    ("slack", "a Slack webhook"),
                    ("discord", "a Discord webhook"),
                    ("telegram", "a Telegram bot"),
                    ("command", "any program, given the notification as JSON"),
                ];
                self.modal = Some(Modal::Pick(Pick {
                    title: "Where should notifications go?".into(),
                    items: types.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect(),
                    query: String::new(),
                    search: false,
                    sel: 0,
                    on: None,
                    then: PickThen::Make(Make {
                        path: col.path,
                        name: String::new(),
                        fields: Vec::new(),
                        todo: Vec::new(),
                        asking: None,
                    }),
                }));
                vec![]
            }
            _ => {
                let make = Make {
                    path: col.path,
                    name: String::new(),
                    fields: Vec::new(),
                    todo: Vec::new(),
                    asking: None,
                };
                self.modal = Some(Modal::Input {
                    title: format!("Name the new {}", col.noun),
                    hint: if matches!(col.layout, Layout::Map(_)) {
                        "its name".into()
                    } else {
                        "letters, digits, - and _".into()
                    },
                    edit: LineEdit::default(),
                    secret: false,
                    then: Then::Make(make),
                    check: None,
                });
                vec![]
            }
        }
    }

    /// A made member's next question, or the making.
    fn make_next(&mut self, mut make: Make) -> Vec<Action> {
        let col = settings::collection(make.path).expect("collection");
        if let Some(field) = make.todo.first().copied() {
            make.todo.remove(0);
            make.asking = Some(field);
            let sk = format!(
                "{}.{}.{field}",
                make.path,
                if matches!(col.layout, Layout::Array) { "0" } else { "x" }
            );
            let st = settings::find(&sk);
            let (secret, hint) = match st.map(|s| s.kind) {
                Some(Kind::Secret) => {
                    (true, "the token, or !program args to run one that prints it".to_owned())
                }
                Some(Kind::Argv) if make.path == "agents" => (
                    false,
                    format!("a program and its arguments, or ⏎ to run it as {:?}", make.name),
                ),
                Some(Kind::Argv) => (false, "a program and its arguments".to_owned()),
                _ => (false, String::new()),
            };
            self.modal = Some(Modal::Input {
                title: format!("{} — {}", field, st.map_or("", |s| s.help)),
                hint,
                edit: LineEdit::default(),
                secret,
                then: Then::Make(make),
                check: None,
            });
            return vec![];
        }
        let layer = settings::section(col.section)
            .filter(|s| s.scope == Scope::Project)
            .and_then(|_| self.project_layer())
            .unwrap_or_else(|| self.user.clone());
        let mut fields = make.fields.clone();
        if matches!(col.layout, Layout::Tables) {
            // (An answer left empty is a question skipped.)
            fields.retain(|(_, v)| !v.trim().is_empty());
        }
        let name = make.name.clone();
        if matches!(col.layout, Layout::Map(_)) {
            // (a prompt: its text came last)
            fields = fields
                .into_iter()
                .map(|(k, v)| if k == "text" { ("value".to_owned(), v) } else { (k, v) })
                .collect();
        }
        let label = format!("added {} {}", col.noun, name);
        let mut acts = self.change(
            layer,
            Op::Add { path: col.path.to_owned(), name: name.clone(), fields },
            &label,
        );
        // A theme just made is the one in use, so editing its colours shows at once.
        if col.path == "themes"
            && self.note.as_ref().is_some_and(|(_, err)| !*err)
            && let Some(e) = self.find_entry("ui.theme")
        {
            acts.extend(self.set(&e, &name));
            if self.note.as_ref().is_some_and(|(_, err)| !*err) {
                self.note = Some((
                    format!(
                        "added theme {name}, now in use: change its colours and watch them change"
                    ),
                    false,
                ));
            }
        }
        if self.note.as_ref().is_some_and(|(_, err)| !*err)
            && !matches!(col.layout, Layout::Map(_) | Layout::Array)
        {
            self.drill = Some((col.path, name));
            self.cursor = 0;
            self.clamp();
        }
        acts
    }

    fn modal_key(&mut self, modal: Modal, key: KeyEvent) -> Vec<Action> {
        match modal {
            Modal::Input { title, hint, mut edit, secret, then, check } => {
                match key.code {
                    KeyCode::Esc => return vec![],
                    // A wrong answer keeps the box open, with what was typed, and says why.
                    KeyCode::Enter if check.is_some() => {
                        self.note = check.clone().map(|c| (c, true));
                        self.modal = Some(Modal::Input { title, hint, edit, secret, then, check });
                        return vec![];
                    }
                    KeyCode::Enter => return self.input_done(edit.text.clone(), then),
                    _ => {}
                }
                let edited = edit.key(key);
                if edited && names_a_table(&then) {
                    edit.text = ptyro_core::names::typed(&edit.text);
                }
                self.modal = Some(Modal::Input { title, hint, edit, secret, then, check });
                if edited {
                    self.recheck();
                }
                vec![]
            }
            Modal::Pick(mut p) => {
                let n = p.shown().len();
                match key.code {
                    KeyCode::Esc => {
                        return if matches!(&p.then, PickThen::Set(k) if k == "ui.theme") {
                            vec![Action::Theme(None)]
                        } else {
                            vec![]
                        };
                    }
                    KeyCode::Up | KeyCode::Char { c: 'k' }
                        if !p.search || key.code == KeyCode::Up =>
                    {
                        p.sel = p.sel.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char { c: 'j' }
                        if !p.search || key.code == KeyCode::Down =>
                    {
                        p.sel = (p.sel + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::PageUp => p.sel = p.sel.saturating_sub(8),
                    KeyCode::PageDown => p.sel = (p.sel + 8).min(n.saturating_sub(1)),
                    KeyCode::Char { c: ' ' } if p.on.is_some() => {
                        if let Some((v, _)) = p.current().cloned()
                            && let Some(on) = &mut p.on
                            && !on.remove(&v)
                        {
                            on.insert(v);
                        }
                    }
                    KeyCode::Enter => return self.pick_done(p),
                    KeyCode::Backspace if p.search => {
                        p.query.pop();
                        p.sel = 0;
                    }
                    KeyCode::Char { c } if p.search && !key.mods.has(Mods::CTRL) => {
                        p.query.push(c);
                        p.sel = 0;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Pick(p));
                self.preview()
            }
            Modal::Capture { title, mut keys, then } => {
                match key.code {
                    KeyCode::Esc => return vec![],
                    KeyCode::Enter if !keys.is_empty() => return self.capture_done(keys, then),
                    KeyCode::Backspace => {
                        keys.pop();
                    }
                    _ => {
                        // Record the key as the config writes it.
                        let k = keymap::normalize(key);
                        let label = key_label(&k);
                        if !keys.contains(&label) {
                            keys.push(label);
                        }
                        // A binding is one key: take it at once.
                        if matches!(then, CaptureThen::Binding(_) | CaptureThen::MoveKey(_)) {
                            return self.capture_done(keys, then);
                        }
                    }
                }
                self.modal = Some(Modal::Capture { title, keys, then });
                vec![]
            }
            Modal::Confirm { text, then } => match key.code {
                KeyCode::Char { c: 'y' | 'Y' } | KeyCode::Enter => match then {
                    Confirmed::Remove { path, name } => {
                        let layer = self.holder(path, &name);
                        self.drill = None;
                        let acts = self.change(
                            layer,
                            Op::Remove { path: path.to_owned(), name: name.clone() },
                            &format!("removed {name}"),
                        );
                        self.clamp();
                        acts
                    }
                    Confirmed::KeyOver { from, to, swap } => self.key_placed(from, to, swap),
                },
                KeyCode::Char { c: 'n' | 'N' } | KeyCode::Esc => vec![],
                _ => {
                    self.modal = Some(Modal::Confirm { text, then });
                    vec![]
                }
            },
        }
    }

    fn input_done(&mut self, answer: String, then: Then) -> Vec<Action> {
        match then {
            Then::Set(key) => {
                let Some(e) = self.find_entry(&key) else { return vec![] };
                self.set(&e, &answer)
            }
            Then::SetMember { path, name } => {
                let layer = self.holder(path, &name);
                self.change(
                    layer,
                    Op::Set { key: format!("{path}.{name}"), value: answer },
                    &format!("{name} changed"),
                )
            }
            Then::Rename { path, from } => {
                let layer = self.holder(path, &from);
                let to = answer.trim().to_owned();
                let acts = self.change(
                    layer,
                    Op::Rename { path: path.to_owned(), from: from.clone(), to: to.clone() },
                    &format!("{from} is now {to}"),
                );
                if self.note.as_ref().is_some_and(|(_, err)| !*err)
                    && self.drill.as_ref().is_some_and(|(p, n)| *p == path && *n == from)
                {
                    self.drill = Some((path, to));
                }
                acts
            }
            Then::Make(mut make) => {
                let col = settings::collection(make.path).expect("collection");
                match make.asking {
                    // The name.
                    None => {
                        make.name = answer.trim().to_owned();
                        if make.name.is_empty() {
                            self.note = Some((format!("a {} needs a name", col.noun), true));
                            return vec![];
                        }
                        make.todo = match (col.layout, make.path) {
                            (Layout::Map(_), _) => vec!["text"],
                            // Which program it runs: optional, it runs as its own name without.
                            (_, "agents") => vec!["command"],
                            _ => settings::required(make.path, "").to_vec(),
                        };
                        // A theme starts as a copy of one that exists: pick which, first.
                        if make.path == "themes" {
                            self.modal = Some(Modal::Pick(Pick {
                                title: format!("Start {} from which theme?", make.name),
                                items: crate::theme::BUILTIN
                                    .iter()
                                    .map(|t| ((*t).to_owned(), "built in".to_owned()))
                                    .collect(),
                                query: String::new(),
                                search: false,
                                sel: 0,
                                on: None,
                                then: PickThen::Make(make),
                            }));
                            return vec![];
                        }
                    }
                    Some(field) => {
                        let f = if field == "text" { "text" } else { field };
                        make.fields.push((f.to_owned(), answer));
                    }
                }
                self.make_next(make)
            }
        }
    }

    fn find_entry(&self, key: &str) -> Option<Entry> {
        [false, true]
            .into_iter()
            .find_map(|p| self.view(p).entries.iter().find(|e| e.key == key).cloned())
    }

    fn pick_done(&mut self, p: Pick) -> Vec<Action> {
        match p.then.clone() {
            PickThen::Set(key) => {
                let Some(e) = self.find_entry(&key) else { return vec![] };
                let value = match &p.on {
                    Some(on) => p
                        .items
                        .iter()
                        .map(|(v, _)| v.clone())
                        .filter(|v| on.contains(v))
                        .collect::<Vec<_>>()
                        .join(","),
                    None => match p.current() {
                        Some((v, _)) => v.clone(),
                        None if key == "ui.theme" => return vec![Action::Theme(None)],
                        None => return vec![],
                    },
                };
                // Nothing chosen from a set is an empty list.
                let value =
                    if p.on.is_some() && value.is_empty() { "[]".to_owned() } else { value };
                let mut acts = self.set(&e, &value);
                // A theme tried on screen goes back when the change wasn't made.
                if key == "ui.theme" && acts.is_empty() {
                    acts.push(Action::Theme(None));
                }
                acts
            }
            PickThen::Make(mut make) => {
                let Some((kind, _)) = p.current().cloned() else { return vec![] };
                if make.path == "themes" {
                    make.fields.push(("base".into(), kind));
                    return self.make_next(make);
                }
                make.fields.push(("type".into(), kind.clone()));
                make.todo = settings::required(make.path, &kind).to_vec();
                self.make_next(make)
            }
            PickThen::BindingAction(name) => {
                let Some((v, _)) = p.current().cloned() else { return vec![] };
                let label = format!("{} runs {v}", settings::slot(&name).1);
                let acts = self.bind(vec![(name.clone(), Some(v))], &label);
                self.focus_key(&name);
                acts
            }
            PickThen::KeyMenu(name) => {
                let Some((what, _)) = p.current().cloned() else { return vec![] };
                self.key_menu_done(&name, &what)
            }
        }
    }

    fn capture_done(&mut self, keys: Vec<String>, then: CaptureThen) -> Vec<Action> {
        match then {
            CaptureThen::Prefix => {
                let Some(e) = self.find_entry("ui.prefix") else { return vec![] };
                self.set(&e, &serde_json::to_string(&keys).unwrap_or_default())
            }
            CaptureThen::Binding(table) => {
                let key = keys.first().cloned().unwrap_or_default();
                self.key_pressed(table, &key, None)
            }
            CaptureThen::MoveKey(from) => {
                let key = keys.first().cloned().unwrap_or_default();
                let table = match settings::slot(&from).0 {
                    "nav" => "nav",
                    "global" => "global",
                    _ => "prefix",
                };
                self.key_pressed(table, &key, Some(from))
            }
        }
    }

    fn search_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(mut q) = self.search.take() else { return vec![] };
        match key.code {
            KeyCode::Esc => return vec![],
            KeyCode::Enter => {
                let rows = {
                    self.search = Some(q.clone());
                    self.rows()
                };
                self.search = None;
                match rows.get(self.cursor) {
                    Some(Row::Setting(p, i)) => {
                        let k = self.entry(*p, *i).key.clone();
                        self.jump(&k);
                    }
                    Some(Row::Binding { path, name, .. }) => {
                        let k = format!("{path}.{name}");
                        self.jump(&k);
                    }
                    Some(Row::Key(b)) => {
                        let k = format!("keys.{}", b.name);
                        self.jump(&k);
                    }
                    _ => {}
                }
                return vec![];
            }
            KeyCode::Up => {
                self.search = Some(q);
                self.step(false, 1);
                return vec![];
            }
            KeyCode::Down => {
                self.search = Some(q);
                self.step(true, 1);
                return vec![];
            }
            _ => {
                if q.key(key) {
                    self.cursor = 0;
                }
            }
        }
        self.search = Some(q);
        self.clamp();
        vec![]
    }

    /// Text pasted into the open box.
    pub fn paste(&mut self, text: &str) {
        let flat = text.replace(['\r', '\n'], " ");
        match (&mut self.modal, &mut self.search) {
            (Some(Modal::Input { edit, then, .. }), _) => {
                for c in flat.chars() {
                    edit.key(KeyEvent::char(c));
                }
                if names_a_table(then) {
                    edit.text = ptyro_core::names::typed(&edit.text);
                }
                self.recheck();
            }
            (Some(Modal::Pick(p)), _) if p.search => {
                p.query.push_str(&flat);
                p.sel = 0;
            }
            (None, Some(q)) => {
                for c in flat.chars() {
                    q.key(KeyEvent::char(c));
                }
            }
            _ => {}
        }
    }

    /// A click or the wheel. `x`, `y` are screen cells.
    pub fn mouse(&mut self, kind: crossterm::event::MouseEventKind, x: u16, y: u16) -> Vec<Action> {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let hits = self.hits.borrow().clone();
        let inside = |r: &ratatui::layout::Rect| {
            x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
        };
        match kind {
            K::Down(MouseButton::Left) => {
                if self.modal.is_some() {
                    return vec![];
                }
                if !inside(&hits.panel) {
                    return vec![Action::Close];
                }
                if let Some((_, s)) = hits.sections.iter().find(|(r, _)| inside(r)) {
                    self.focus = Pane::Sections;
                    self.set_section(*s);
                    self.focus = Pane::Items;
                } else if let Some((_, i)) = hits.rows.iter().find(|(r, _)| inside(r)) {
                    let rows = self.rows();
                    if rows.get(*i).is_some_and(Row::selectable) {
                        let already = self.cursor == *i && self.focus == Pane::Items;
                        self.focus = Pane::Items;
                        self.cursor = *i;
                        if already {
                            return self.activate(rows[*i].clone(), false);
                        }
                    }
                }
            }
            K::ScrollDown if self.modal.is_none() => {
                if self.focus == Pane::Sections && hits.sections.iter().any(|(r, _)| inside(r)) {
                    self.move_section(true);
                } else {
                    self.step(true, 3);
                }
            }
            K::ScrollUp if self.modal.is_none() => {
                if self.focus == Pane::Sections && hits.sections.iter().any(|(r, _)| inside(r)) {
                    self.move_section(false);
                } else {
                    self.step(false, 3);
                }
            }
            _ => {}
        }
        vec![]
    }
}

// ── words ─────────────────────────────────────────────────────────────────────────────────

/// A sentence as hint rows that fit the items pane.
fn hint_rows(text: &str) -> Vec<Row> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= 76 => {
                l.push(' ');
                l.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines.into_iter().map(Row::Hint).collect()
}

/// The first few words of what an action does: “Detach”, “Command palette”.
fn gist(help: &str) -> String {
    let cut = help.find([':', '(', '.']).unwrap_or(help.len());
    let mut g = help[..cut].trim().to_owned();
    if g.is_empty() {
        g = help.to_owned();
    }
    g
}

fn article(noun: &str) -> &'static str {
    if noun.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" }
}

/// A member's one line: what it is, from its main field.
fn summarize(col: &Collection, name: &str, fields: &[&Entry]) -> String {
    let get = |suffix: &str| {
        fields.iter().find(|e| e.key.ends_with(&format!(".{suffix}"))).map(|e| e.shown())
    };
    let some = |s: Option<String>| s.filter(|v| !v.starts_with('(') && v != "—");
    match col.path {
        "agents" => some(get("command"))
            .or_else(|| some(get("adapter")))
            .unwrap_or_else(|| "runs as its name".into()),
        "themes" => some(get("base"))
            .map_or_else(|| "colours of your own".into(), |b| format!("based on {b}")),
        "editors" => some(get("command")).unwrap_or_default(),
        "scripts.run" => some(get("command")).unwrap_or_default(),
        "profiles" => some(get("label")).or_else(|| some(get("git.email"))).unwrap_or_default(),
        "notifications.sinks" => {
            let t = get("type").unwrap_or_default();
            let to = some(get("url"))
                .or_else(|| some(get("chat_id")))
                .or_else(|| some(get("command")))
                .unwrap_or_default();
            format!("{name}: {t} {to}").trim().to_owned()
        }
        _ => String::new(),
    }
}

// ── drawing ───────────────────────────────────────────────────────────────────────────────

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

fn fit(s: &str, width: usize) -> String {
    let mut out = s.to_owned();
    crate::ui::truncate_width(&mut out, width);
    out
}

fn pad(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width { fit(s, width) } else { format!("{s}{}", " ".repeat(width - w)) }
}

/// A colour setting's value as a terminal colour, for its swatch.
fn swatch(v: &serde_json::Value) -> Option<Color> {
    match ptyro_core::config::parse_color(v.as_str()?).ok()? {
        ptyro_core::config::ConfigColor::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
        ptyro_core::config::ConfigColor::Indexed(i) => Some(Color::Indexed(i)),
        _ => None,
    }
}

fn selected_style(app: &App) -> Style {
    let t = &app.theme;
    if t.monochrome {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().bg(app.depth.fit(t.selection_bg)).fg(app.depth.fit(t.selection_fg))
    }
}

pub fn draw(app: &App, p: &Panel, f: &mut Frame<'_>, area: Rect) {
    use crate::ui::fg;
    let t = &app.theme;
    let r = crate::ui::boxed(area, 118, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let scope_label = match &p.project {
        Some(pr) => format!(
            "your config · {}: {}",
            pr.name,
            if p.shared { ".ptyro.toml (shared)" } else { ".ptyro.local.toml (yours)" }
        ),
        None => "your config".into(),
    };
    let title = format!(" Settings — {scope_label} ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(app, t.border_focus))
        .title(Span::styled(title, fg(app, t.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut hits = Hits { panel: r, ..Hits::default() };

    // Columns: sections | items. Below: the selected setting's detail, then the hints.
    let left_w = 24.min(inner.width / 3);
    let detail_h = 5u16;
    let hint_h = 1u16;
    let body_h = inner.height.saturating_sub(detail_h + hint_h + 1);
    let left = Rect { x: inner.x, y: inner.y, width: left_w, height: body_h };
    let right = Rect {
        x: inner.x + left_w + 1,
        y: inner.y,
        width: inner.width.saturating_sub(left_w + 1),
        height: body_h,
    };
    let line_x = Rect { x: inner.x + left_w, y: inner.y, width: 1, height: body_h };
    let bar: Vec<Line<'_>> =
        (0..body_h).map(|_| Line::from(Span::styled("│", fg(app, t.border)))).collect();
    f.render_widget(Paragraph::new(bar), line_x);

    // Sections.
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut shown_repo_title = false;
    for (i, s) in p.sections.iter().enumerate() {
        if s.scope == Scope::Project && !shown_repo_title {
            shown_repo_title = true;
            let name = p.project.as_ref().map_or("no repository".to_owned(), |pr| pr.name.clone());
            lines.push(Line::from(Span::styled(
                fit(&format!("─ {name} "), left_w as usize),
                fg(app, t.dim),
            )));
        }
        let project = s.scope == Scope::Project;
        let view = p.view(project);
        let changed =
            view.entries.iter().filter(|e| e.setting.section == s.id && e.changed()).count();
        let badge = if changed > 0 { format!(" {changed}") } else { String::new() };
        let active = i == p.sec;
        let marker = if active { "▸ " } else { "  " };
        let text = format!("{marker}{}", s.title);
        let room = (left_w as usize).saturating_sub(badge.chars().count());
        let label = pad(&text, room);
        let style = if active && p.focus == Pane::Sections {
            selected_style(app)
        } else if active {
            fg(app, t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        hits.sections.push((
            Rect { x: left.x, y: left.y + lines.len() as u16, width: left_w, height: 1 },
            i,
        ));
        lines.push(Line::from(vec![
            Span::styled(label, style),
            Span::styled(badge, fg(app, t.accent)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), left);

    // Items.
    let rows = p.rows();
    let sec = p.section();
    let heading = if let Some((path, name)) = &p.drill {
        format!("{} › {} › {name}", sec.title, settings::collection(path).map_or("", |c| c.noun))
    } else if p.search.is_some() {
        "Search".to_owned()
    } else {
        format!("{} — {}", sec.title, sec.blurb)
    };
    let mut out: Vec<Line<'_>> = Vec::new();
    if let Some(prob) = &p.uview.problem {
        out.push(Line::from(Span::styled(
            fit(
                &format!("⚠ the config doesn't load, so defaults show: {prob}"),
                right.width as usize,
            ),
            fg(app, t.error),
        )));
    }
    if let Some(q) = &p.search {
        let mut s = q.text.clone();
        s.insert(s.char_indices().nth(q.at).map_or(s.len(), |(i, _)| i), '▏');
        out.push(Line::from(vec![Span::styled("/ ", fg(app, t.accent)), Span::raw(s)]));
    } else {
        out.push(Line::from(Span::styled(
            fit(&heading, right.width as usize),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        )));
    }
    let head_h = out.len();
    let room = (right.height as usize).saturating_sub(head_h);
    // Keep the cursor in view.
    let mut top = p.top.min(rows.len().saturating_sub(1));
    if p.cursor < top {
        top = p.cursor;
    }
    if p.cursor >= top + room {
        top = p.cursor + 1 - room;
    }
    let label_w = rows
        .iter()
        .filter_map(|r| match r {
            Row::Setting(pr, i) => Some(
                label_of(p.entry(*pr, *i), p.drill.is_some(), p.search.is_some()).chars().count(),
            ),
            Row::Member { name, .. } | Row::Binding { name, .. } => Some(name.chars().count()),
            Row::Key(b) => Some(b.key.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(10)
        .clamp(10, 30);
    for (i, row) in rows.iter().enumerate().skip(top).take(room) {
        let y = right.y + out.len() as u16;
        hits.rows.push((Rect { x: right.x, y, width: right.width, height: 1 }, i));
        let is_sel = i == p.cursor && p.focus == Pane::Items;
        let w = right.width as usize;
        let mut spans: Vec<Span<'_>> = Vec::new();
        match row {
            Row::Title(s) => {
                if !s.is_empty() {
                    spans.push(Span::styled(fit(&format!("── {s} "), w), fg(app, t.dim)));
                }
            }
            Row::Hint(s) => spans.push(Span::styled(fit(s, w), fg(app, t.dim))),
            Row::Setting(pr, idx) => {
                let e = p.entry(*pr, *idx);
                let changed = e.changed();
                let mark = if changed { "● " } else { "  " };
                let name = pad(&label_of(e, p.drill.is_some(), p.search.is_some()), label_w + 1);
                let tag = origin_tag(e);
                let val_w = w.saturating_sub(2 + label_w + 1 + tag.chars().count() + 2);
                let mut value = e.pretty();
                if matches!(e.setting.kind, Kind::Bool) {
                    value = if e.value.as_bool().unwrap_or(false) {
                        "[x] on".into()
                    } else if e.value.is_null() {
                        value
                    } else {
                        "[ ] off".into()
                    };
                }
                let sw = swatch(&e.value);
                let vstyle = if changed {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    fg(app, t.dim)
                };
                if is_sel {
                    let mut text = format!("{mark}{name}{}", pad(&value, val_w));
                    text = pad(&text, w.saturating_sub(tag.chars().count() + 1));
                    spans.push(Span::styled(format!("{text} {tag}"), selected_style(app)));
                } else {
                    spans.push(Span::styled(mark, fg(app, t.accent)));
                    spans.push(Span::raw(name));
                    if let Some(c) = sw {
                        spans.push(Span::styled("██ ", Style::new().fg(app.depth.fit(c))));
                    }
                    spans.push(Span::styled(
                        pad(&value, val_w.saturating_sub(if sw.is_some() { 3 } else { 0 })),
                        vstyle,
                    ));
                    spans.push(Span::styled(format!(" {tag}"), fg(app, t.dim)));
                }
            }
            Row::Member { name, summary, changed, .. } => {
                let mark = if *changed > 0 { "● " } else { "  " };
                let text = pad(
                    &format!("{mark}{}{}", pad(name, label_w + 1), summary),
                    w.saturating_sub(2),
                );
                spans.push(Span::styled(
                    format!("{text} ›"),
                    if is_sel { selected_style(app) } else { Style::new() },
                ));
            }
            Row::Binding { name, value, .. } => {
                let text = pad(
                    &format!(
                        "  {}{}",
                        pad(name, label_w + 1),
                        if value == "none" { "unbound".to_owned() } else { format!("→ {value}") }
                    ),
                    w,
                );
                spans.push(Span::styled(
                    text,
                    if is_sel { selected_style(app) } else { Style::new() },
                ));
            }
            Row::Key(b) => {
                let mark = if b.standing == Standing::Default { "  " } else { "● " };
                let name = pad(&b.key, label_w + 1);
                let tag = b.standing.says();
                let what = match (b.standing, p.search.is_some()) {
                    (Standing::Unbound, _) => format!("not bound (was: {})", gist(&b.help)),
                    (_, true) => format!("{}: {}", b.when, gist(&b.help)),
                    _ => b.help.clone(),
                };
                let val_w = w.saturating_sub(2 + label_w + 1 + tag.chars().count() + 2);
                if is_sel {
                    let text = format!("{mark}{name}{}", pad(&what, val_w));
                    let text = pad(&text, w.saturating_sub(tag.chars().count() + 1));
                    spans.push(Span::styled(format!("{text} {tag}"), selected_style(app)));
                } else {
                    let live = matches!(b.standing, Standing::Yours | Standing::Changed);
                    spans.push(Span::styled(mark, fg(app, t.accent)));
                    spans.push(Span::styled(name, Style::new().add_modifier(Modifier::BOLD)));
                    spans.push(Span::styled(
                        pad(&what, val_w),
                        if b.standing == Standing::Unbound { fg(app, t.dim) } else { Style::new() },
                    ));
                    spans.push(Span::styled(
                        format!(" {tag}"),
                        if live { fg(app, t.accent) } else { fg(app, t.dim) },
                    ));
                }
            }
            Row::AddKey(table) => {
                let text = pad(&format!("  + Add a key {}…", settings::when(table)), w);
                spans.push(Span::styled(
                    text,
                    if is_sel { selected_style(app) } else { fg(app, t.accent) },
                ));
            }
            Row::Add { label, .. } => {
                let text = pad(&format!("  + {label}"), w);
                spans.push(Span::styled(
                    text,
                    if is_sel { selected_style(app) } else { fg(app, t.accent) },
                ));
            }
        }
        out.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(out), right);

    // Detail of the selected row.
    let det = Rect { x: inner.x, y: inner.y + body_h, width: inner.width, height: detail_h + 1 };
    let mut d: Vec<Line<'_>> =
        vec![Line::from(Span::styled("─".repeat(inner.width as usize), fg(app, t.border)))];
    let w = inner.width as usize;
    if let Some((msg, err)) = &p.note {
        d.push(Line::from(Span::styled(
            fit(msg, w),
            if *err { fg(app, t.error) } else { fg(app, t.done) },
        )));
    }
    if p.help {
        for l in [
            "↑↓ move · ⏎ or space changes it · r resets it to the default · u undoes the last change",
            "/ searches everything · m only what you changed · A advanced settings · o opens the file at it",
            "Tab switches between sections and settings · [ ] other sections · n adds one · d removes · R renames",
        ] {
            d.push(Line::from(Span::styled(fit(l, w), fg(app, t.dim))));
        }
    } else if let Some(row) = rows.get(p.cursor) {
        detail_lines(app, p, row, w, &mut d);
    }
    if !p.restart.is_empty() {
        d.push(Line::from(Span::styled(
            fit(&format!("⟳ {} needs the daemon restarted: press ! to restart it now (agents come back resumable)", p.restart.iter().cloned().collect::<Vec<_>>().join(", ")), w),
            fg(app, t.blocked),
        )));
    }
    d.truncate(detail_h as usize + 1);
    f.render_widget(Paragraph::new(d), det);

    // Hints.
    let hint = match (&p.modal, p.search.is_some(), p.focus) {
        (Some(_), _, _) => "esc cancels",
        (None, true, _) => "type to search · ↑↓ pick · ⏎ go there · esc cancels",
        (None, false, Pane::Sections) => {
            "↑↓ section · ⏎ open · / search · m changed · ? help · esc closes"
        }
        (None, false, Pane::Items) => {
            "⏎ change · r reset · u undo · / search · m changed · s save target · ? help · esc back"
        }
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(fit(hint, w), fg(app, t.dim)))),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
    *p.hits.borrow_mut() = hits;

    if let Some(m) = &p.modal {
        draw_modal(app, p, m, f, r);
    }
}

/// A setting's name in the list: its key without the section, in words (`sidebar width`).
fn label_of(e: &Entry, drilled: bool, searching: bool) -> String {
    if searching {
        e.key.clone()
    } else if drilled || e.member.is_some() {
        e.key.rsplit_once('.').map_or(e.key.clone(), |(_, k)| k.replace('_', " "))
    } else {
        e.key.split_once('.').map_or(e.key.clone(), |(_, k)| k.replace('_', " "))
    }
}

fn origin_tag(e: &Entry) -> String {
    match e.origin {
        Origin::Default => "default".into(),
        Origin::User => "yours".into(),
        Origin::Project => "shared".into(),
        Origin::Local => "mine".into(),
    }
}

fn detail_lines<'a>(app: &App, p: &Panel, row: &Row, w: usize, d: &mut Vec<Line<'a>>) {
    use crate::ui::fg;
    let t = &app.theme;
    let dim = fg(app, t.dim);
    match row {
        Row::Setting(pr, i) => {
            let e = p.entry(*pr, *i);
            d.push(Line::from(vec![
                Span::styled(e.key.clone(), fg(app, t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  ({})", e.setting.kind.word()), dim),
            ]));
            d.push(Line::from(Span::raw(fit(e.setting.help, w))));
            let from = match (e.origin, e.line) {
                (Origin::Default, _) => "the default".to_owned(),
                (o, Some(l)) => format!("{}, line {l}", o.says()),
                (o, None) => o.says().to_owned(),
            };
            let kind_help = match e.setting.kind {
                Kind::Int { min, max } => format!(" · {min}–{max}, +/- steps it"),
                Kind::Bool => " · space toggles".to_owned(),
                _ => String::new(),
            };
            d.push(Line::from(Span::styled(
                fit(
                    &format!(
                        "default {} · from {from} · {}{kind_help}",
                        e.default_pretty(),
                        e.setting.apply.says()
                    ),
                    w,
                ),
                dim,
            )));
        }
        Row::Member { path, name, .. } => {
            let col = settings::collection(path).expect("collection");
            d.push(Line::from(Span::styled(
                format!("{} {name}", col.noun),
                fg(app, t.accent).add_modifier(Modifier::BOLD),
            )));
            d.push(Line::from(Span::styled(
                "⏎ opens its settings · d removes it · R renames it",
                dim,
            )));
        }
        Row::Binding { path, name, .. } => {
            d.push(Line::from(Span::styled(
                format!("{path}.{name}"),
                fg(app, t.accent).add_modifier(Modifier::BOLD),
            )));
            let hint = if *path == "prompts" {
                "⏎ edits its text · d removes it. A new session starts from it with --prompt @name."
            } else {
                "⏎ picks what it runs · d removes it."
            };
            d.push(Line::from(Span::styled(hint, dim)));
        }
        Row::Key(b) => {
            d.push(Line::from(vec![
                Span::styled(
                    format!("{}  {}", b.key, b.when),
                    fg(app, t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  ({})", b.standing.says()), dim),
            ]));
            let (runs, id) = match b.standing {
                Standing::Unbound => {
                    ("Unbound. It used to run", b.default.as_deref().unwrap_or(""))
                }
                _ => ("Runs", b.action.as_str()),
            };
            d.push(Line::from(Span::raw(fit(&format!("{runs} {id}: {}", b.help), w))));
            d.push(Line::from(Span::styled(
                match (&b.default, b.standing) {
                    (_, Standing::Default) => "the default · r does nothing here".to_owned(),
                    (Some(dflt), _) => format!("the default is {dflt} · r puts it back"),
                    (None, _) => "your own key · r or d removes it".to_owned(),
                },
                dim,
            )));
            d.push(Line::from(Span::styled(
                if b.fixed {
                    "these jump to a session by its number; bind your own key to session.focus N to add more"
                } else {
                    "⏎ for the choices: change its key, change what it runs, unbind it · d unbinds · r resets"
                },
                dim,
            )));
        }
        Row::AddKey(table) => {
            d.push(Line::from(Span::styled(
                format!("Add a key {}", settings::when(table)),
                fg(app, t.accent).add_modifier(Modifier::BOLD),
            )));
            d.push(Line::from(Span::styled(
                "⏎ then press the key, then pick what it runs. If the key is taken, you are asked first.",
                dim,
            )));
        }
        Row::Add { path, .. } => {
            let col = settings::collection(path).expect("collection");
            d.push(Line::from(Span::styled(
                format!("Add {} {}", article(col.noun), col.noun),
                fg(app, t.accent).add_modifier(Modifier::BOLD),
            )));
            if *path == "profiles" {
                d.push(Line::from(Span::styled(
                    "⏎ runs `ptyro profile new`, which also sets up the account folders",
                    dim,
                )));
            }
        }
        _ => {}
    }
}

fn draw_modal(app: &App, _p: &Panel, m: &Modal, f: &mut Frame<'_>, panel: Rect) {
    use crate::ui::fg;
    let t = &app.theme;
    let block = |title: String| {
        Block::default().borders(Borders::ALL).border_style(fg(app, t.accent)).title(Span::styled(
            format!(" {title} "),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        ))
    };
    match m {
        Modal::Input { title, hint, edit, secret, check, .. } => {
            let r = Rect {
                x: panel.x + 4,
                y: panel.y + panel.height / 3,
                width: panel.width.saturating_sub(8),
                height: 6,
            };
            f.render_widget(Clear, r);
            let w = r.width.saturating_sub(4) as usize;
            let shown: String =
                if *secret { "•".repeat(edit.text.chars().count()) } else { edit.text.clone() };
            let (a, b): (String, String) =
                (shown.chars().take(edit.at).collect(), shown.chars().skip(edit.at).collect());
            let (cur, rest) = match b.chars().next() {
                Some(c) => (c.to_string(), b.chars().skip(1).collect::<String>()),
                None => (" ".to_owned(), String::new()),
            };
            let mut lines = vec![
                Line::from(Span::raw(fit(title, w))),
                Line::from(vec![
                    Span::styled("› ", fg(app, t.accent)),
                    Span::raw(a),
                    Span::styled(cur, Style::new().add_modifier(Modifier::REVERSED)),
                    Span::raw(rest),
                ]),
            ];
            lines.push(match check {
                Some(e) => Line::from(Span::styled(fit(&format!("✗ {e}"), w), fg(app, t.error))),
                None if hint.is_empty() => Line::from(Span::styled("✓ ⏎ saves", fg(app, t.done))),
                None => Line::from(Span::styled(
                    fit(&format!("✓ {hint} · ⏎ saves"), w),
                    fg(app, t.done),
                )),
            });
            f.render_widget(Paragraph::new(lines).block(block("Edit".into())), r);
        }
        Modal::Pick(p) => {
            let shown = p.shown();
            let rows = shown.len().min(12) as u16;
            let r = Rect {
                x: panel.x + 6,
                y: panel.y + 3,
                width: panel.width.saturating_sub(12),
                height: rows + 4 + u16::from(p.search),
            };
            f.render_widget(Clear, r);
            let w = r.width.saturating_sub(2) as usize;
            let mut lines: Vec<Line<'_>> = Vec::new();
            lines.push(Line::from(Span::styled(fit(&p.title, w), fg(app, t.dim))));
            if p.search {
                lines.push(Line::from(vec![
                    Span::styled("› ", fg(app, t.accent)),
                    Span::raw(p.query.clone()),
                ]));
            }
            let first = p.sel.saturating_sub(11);
            for (n, idx) in shown.iter().enumerate().skip(first).take(12) {
                let (v, help) = &p.items[*idx];
                let mark = match &p.on {
                    Some(on) => {
                        if on.contains(v) {
                            "[x] "
                        } else {
                            "[ ] "
                        }
                    }
                    None => "",
                };
                let text = pad(&format!(" {mark}{}  {help}", pad(v, 16)), w);
                lines.push(Line::from(Span::styled(
                    text,
                    if n == p.sel { selected_style(app) } else { Style::new() },
                )));
            }
            if shown.is_empty() {
                lines.push(Line::from(Span::styled("  nothing matches", fg(app, t.dim))));
            }
            f.render_widget(Paragraph::new(lines).block(block("Choose".into())), r);
        }
        Modal::Capture { title, keys, .. } => {
            let r = Rect {
                x: panel.x + 6,
                y: panel.y + panel.height / 3,
                width: panel.width.saturating_sub(12),
                height: 6,
            };
            f.render_widget(Clear, r);
            let lines = vec![
                Line::from(Span::raw(title.clone())),
                Line::from(vec![
                    Span::styled("› ", fg(app, t.accent)),
                    Span::styled(
                        if keys.is_empty() { "…".into() } else { keys.join("   ") },
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    "⏎ done · ⌫ takes the last back · esc cancels",
                    fg(app, t.dim),
                )),
            ];
            f.render_widget(Paragraph::new(lines).block(block("Press keys".into())), r);
        }
        Modal::Confirm { text, then } => {
            let r = Rect {
                x: panel.x + 6,
                y: panel.y + panel.height / 3,
                width: panel.width.saturating_sub(12),
                height: 6,
            };
            f.render_widget(Clear, r);
            let (title, answers) = match then {
                Confirmed::Remove { .. } => ("Remove", "y removes it · n keeps it"),
                Confirmed::KeyOver { .. } => ("That key is taken", "y or ⏎ goes ahead · n cancels"),
            };
            let lines = vec![
                Line::from(Span::raw(text.clone())),
                Line::from(Span::styled(answers, fg(app, t.dim))),
            ];
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .block(block(title.into())),
                r,
            );
        }
    }
}

#[cfg(test)]
mod tests;
