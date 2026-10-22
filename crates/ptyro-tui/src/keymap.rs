//! The key map (SPEC §11.3, M2.5): named actions, default bindings in three tables, config
//! overrides, and the help/palette metadata generated from it.
//!
//! Tables: **prefix** (after the prefix key), **global** (always; use sparingly, they steal keys
//! from programs), **nav** (while the sidebar has focus).

use std::collections::BTreeMap;

use ptyro_core::config::{Config, key_label, parse_key};
use ptyro_core::input::{KeyCode, KeyEvent, KeyKind, Mods};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    None,
    Number,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    pub id: &'static str,
    pub desc: &'static str,
    pub arg: Arg,
}

const fn a(id: &'static str, desc: &'static str) -> ActionSpec {
    ActionSpec { id, desc, arg: Arg::None }
}

/// Every action the TUI understands. Palette and help list these.
pub const ACTIONS: &[ActionSpec] = &[
    a("help", "Show key bindings"),
    a("palette", "Command palette: run any action, jump to any session"),
    a("app.detach", "Detach (sessions keep running)"),
    a(
        "session.new",
        "New session: pick an agent, then where (n ⏎ ⏎: the default agent in a new worktree)",
    ),
    a("session.command", "New session running a command line (empty: your shell)"),
    a("worktree.new", "New worktree of this project (+ an agent), after its setup script"),
    a("workspace.switch", "Switch workspace (each has its own projects, profile and layout)"),
    a("worktree.open", "Open this worktree in an editor ([editors]; nvim opens here)"),
    a("script.run", "Run the project's default run script in this worktree"),
    a("script.pick", "Pick a lifecycle script to run in this worktree"),
    a("project.add", "Add a git repository to this workspace (discovered under discover.roots)"),
    a("project.trust", "Review and trust this project's scripts"),
    a("worktree.remove", "Remove this worktree (refuses if work would be lost)"),
    a("session.next", "Show the next session in this pane"),
    a("session.prev", "Show the previous session in this pane"),
    ActionSpec {
        id: "session.focus",
        desc: "Show session N (sidebar order) in this pane",
        arg: Arg::Number,
    },
    a("session.find", "Find a session (fuzzy) and show it here"),
    a("attention.next", "Jump to the session that needs you most (blocked first)"),
    a("inbox", "The inbox: everything that wanted you, per workspace"),
    a("peek", "Peek at a session's screen and what it wants; reply or answer inline"),
    a("session.resume", "Resume a dormant agent into its conversation"),
    a("session.hibernate", "Stop an idle agent, keeping it resumable"),
    a("session.explain", "Why is it in this state? The status engine's recent decisions"),
    a("session.mute", "Mute this session's notifications for an hour (the inbox still gets them)"),
    a("notify.test", "Send a test notification through every channel and sink"),
    a("session.last", "Show the previously shown session here"),
    a("session.stop", "Stop the session (HUP→TERM→KILL); removes it if it already exited"),
    a("session.kill", "Kill the session's process group now (SIGKILL)"),
    a("session.restart", "Run an exited session's command again"),
    a("session.rename", "Rename the session"),
    a("session.take_control", "Take control: resize the session to this pane"),
    a("focus.toggle", "Move focus between the sidebar and the pane"),
    a("sidebar.toggle", "Show or hide the sidebar"),
    a("pane.split_right", "Split: open a session beside this pane"),
    a("pane.split_down", "Split: open a session below this pane"),
    a("pane.close", "Close this pane (the session keeps running)"),
    a("pane.zoom", "Zoom this pane to the whole screen (toggle)"),
    a("pane.next", "Focus the next pane"),
    a("pane.prev", "Focus the previous pane"),
    a("pane.left", "Focus the pane to the left"),
    a("pane.right", "Focus the pane to the right"),
    a("pane.up", "Focus the pane above"),
    a("pane.down", "Focus the pane below"),
    a("pane.wider", "Make this pane wider"),
    a("pane.narrower", "Make this pane narrower"),
    a("pane.taller", "Make this pane taller"),
    a("pane.shorter", "Make this pane shorter"),
    a("copy.enter", "Copy mode: scroll back, search, select and yank"),
    a("copy.page_up", "Copy mode, one page up"),
    a("paste.buffer", "Paste the last yanked text into the session"),
    a("view.redraw", "Redraw everything (fresh keyframes)"),
    a("view.chat", "Structured session: chat view ⇄ terminal view"),
    a("theme.next", "Switch to the next theme"),
    a("config.edit", "Edit the config file in a new session"),
    a("nav.down", "Sidebar: select the next session"),
    a("nav.up", "Sidebar: select the previous session"),
    a("nav.top", "Sidebar: select the first session"),
    a("nav.bottom", "Sidebar: select the last session"),
    a("nav.open", "Sidebar: show the selected session, or fold/unfold a project or worktree"),
    a(
        "nav.select",
        "Sidebar: mark the selected session for broadcast, or fold/unfold a project or worktree",
    ),
    a("broadcast", "Type one prompt into every marked session (each once it's ready)"),
    a("help.tour", "The 60-second tour of ptyro's keys"),
    a(
        "inspector",
        "Inspector: the worktree's diff (comment, commit, PR), commits, checks, ports, events",
    ),
    a("review.queue", "Worktrees ready for review, best first"),
    a("runs", "Runs: recipes and fan-outs, their steps and gates; compare and pick candidates"),
    a("sets", "Change sets: members as swimlanes, gates, issues, the ledger as a timeline"),
    a("nav.collapse", "Sidebar: fold the selected project or worktree"),
    a("nav.expand", "Sidebar: unfold the selected project or worktree"),
];

pub fn spec(id: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|s| s.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Table {
    Prefix,
    Global,
    Nav,
}

impl Table {
    pub fn label(self) -> &'static str {
        match self {
            Self::Prefix => "after the prefix",
            Self::Global => "anywhere",
            Self::Nav => "in the sidebar",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub action: &'static str,
    pub arg: Option<String>,
}

impl Command {
    pub fn number(&self) -> Option<usize> {
        self.arg.as_deref().and_then(|a| a.parse().ok())
    }
}

/// Compare keys the way users think of them: release/repeat/keypad ignored, and for characters
/// the character already says whether Shift was held (`A`, `?`), so Shift is dropped.
pub fn normalize(k: KeyEvent) -> KeyEvent {
    let mut mods = k.mods.0 & !(Mods::META | Mods::HYPER);
    let code = match k.code {
        KeyCode::Char { c } => {
            let c = if mods & Mods::SHIFT != 0 && c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c
            };
            mods &= !Mods::SHIFT;
            KeyCode::Char { c }
        }
        KeyCode::BackTab => {
            mods &= !Mods::SHIFT;
            KeyCode::BackTab
        }
        other => other,
    };
    KeyEvent { code, mods: Mods(mods), kind: KeyKind::Press, keypad: false }
}

fn key(spec: &str) -> KeyEvent {
    normalize(parse_key(spec).expect("default key spec"))
}

const DEFAULTS: &[(Table, &str, &str)] = &[
    (Table::Prefix, "?", "help"),
    (Table::Prefix, ":", "palette"),
    (Table::Prefix, "C-p", "palette"),
    (Table::Prefix, "d", "app.detach"),
    (Table::Prefix, "q", "app.detach"),
    (Table::Prefix, "n", "session.new"),
    (Table::Prefix, "N", "worktree.new"),
    (Table::Prefix, "w", "workspace.switch"),
    (Table::Prefix, "o", "worktree.open"),
    (Table::Prefix, "r", "script.run"),
    (Table::Prefix, "R", "script.pick"),
    (Table::Prefix, "j", "session.next"),
    (Table::Prefix, "k", "session.prev"),
    (Table::Prefix, "Down", "session.next"),
    (Table::Prefix, "Up", "session.prev"),
    (Table::Prefix, "/", "session.find"),
    (Table::Prefix, "a", "attention.next"),
    (Table::Prefix, "i", "inbox"),
    (Table::Prefix, "p", "peek"),
    (Table::Prefix, "b", "broadcast"),
    (Table::Prefix, "D", "inspector"),
    (Table::Prefix, "Q", "review.queue"),
    (Table::Prefix, "O", "runs"),
    (Table::Prefix, "S", "sets"),
    (Table::Prefix, "`", "session.last"),
    (Table::Prefix, "x", "session.stop"),
    (Table::Prefix, "X", "session.kill"),
    (Table::Prefix, "C-r", "session.restart"),
    (Table::Prefix, ",", "session.rename"),
    (Table::Prefix, "=", "session.take_control"),
    (Table::Prefix, "Tab", "focus.toggle"),
    (Table::Prefix, "\\", "sidebar.toggle"),
    (Table::Prefix, "v", "pane.split_right"),
    (Table::Prefix, "s", "pane.split_down"),
    (Table::Prefix, "c", "pane.close"),
    (Table::Prefix, "z", "pane.zoom"),
    (Table::Prefix, "l", "pane.next"),
    (Table::Prefix, "h", "pane.prev"),
    (Table::Prefix, "S-Left", "pane.left"),
    (Table::Prefix, "S-Right", "pane.right"),
    (Table::Prefix, "S-Up", "pane.up"),
    (Table::Prefix, "S-Down", "pane.down"),
    (Table::Prefix, ">", "pane.wider"),
    (Table::Prefix, "<", "pane.narrower"),
    (Table::Prefix, "+", "pane.taller"),
    (Table::Prefix, "-", "pane.shorter"),
    (Table::Prefix, "[", "copy.enter"),
    (Table::Prefix, "PageUp", "copy.page_up"),
    (Table::Prefix, "]", "paste.buffer"),
    (Table::Prefix, "C-l", "view.redraw"),
    (Table::Prefix, "t", "view.chat"),
    (Table::Nav, "j", "nav.down"),
    (Table::Nav, "Down", "nav.down"),
    (Table::Nav, "k", "nav.up"),
    (Table::Nav, "Up", "nav.up"),
    (Table::Nav, "g", "nav.top"),
    (Table::Nav, "G", "nav.bottom"),
    (Table::Nav, "Enter", "nav.open"),
    (Table::Nav, "Space", "nav.select"),
    (Table::Nav, "b", "broadcast"),
    (Table::Nav, "D", "inspector"),
    (Table::Nav, "O", "runs"),
    (Table::Nav, "S", "sets"),
    (Table::Nav, "h", "nav.collapse"),
    (Table::Nav, "Left", "nav.collapse"),
    (Table::Nav, "l", "nav.expand"),
    (Table::Nav, "Right", "nav.expand"),
    (Table::Nav, "N", "worktree.new"),
    (Table::Nav, "w", "workspace.switch"),
    (Table::Nav, "o", "worktree.open"),
    (Table::Nav, "r", "script.run"),
    (Table::Nav, "Tab", "focus.toggle"),
    (Table::Nav, "Esc", "focus.toggle"),
    (Table::Nav, "n", "session.new"),
    (Table::Nav, "x", "session.stop"),
    (Table::Nav, "X", "session.kill"),
    (Table::Nav, "R", "session.restart"),
    (Table::Nav, ",", "session.rename"),
    (Table::Nav, "/", "session.find"),
    (Table::Nav, "a", "attention.next"),
    (Table::Nav, "i", "inbox"),
    (Table::Nav, "p", "peek"),
    (Table::Nav, "e", "session.explain"),
    (Table::Nav, "?", "help"),
    (Table::Nav, ":", "palette"),
    (Table::Nav, "\\", "sidebar.toggle"),
    (Table::Nav, "v", "pane.split_right"),
    (Table::Nav, "s", "pane.split_down"),
];

#[derive(Debug, Clone)]
pub struct Keymap {
    /// The first prefix key: the one hints show.
    pub prefix: KeyEvent,
    /// Every prefix key, `prefix` first.
    pub prefixes: Vec<KeyEvent>,
    bindings: BTreeMap<(Table, String), (KeyEvent, Command)>,
}

fn slot(table: Table, k: &KeyEvent) -> (Table, String) {
    (table, key_label(k))
}

impl Keymap {
    /// Defaults plus `[keys]` overrides; problems are returned, never fatal.
    pub fn new(config: &Config) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut prefixes = Vec::new();
        for k in config.ui.prefix.keys() {
            match parse_key(k) {
                Ok(k) => prefixes.push(normalize(k)),
                Err(e) => errors.push(format!("ui.prefix: {e}")),
            }
        }
        if prefixes.is_empty() {
            prefixes.push(key("C-q"));
        }
        let mut map = Self { prefix: prefixes[0], prefixes, bindings: BTreeMap::new() };
        for (table, k, action) in DEFAULTS {
            let k = key(k);
            map.bindings.insert(slot(*table, &k), (k, Command { action, arg: None }));
        }
        for d in 1..=9u8 {
            let k = normalize(KeyEvent::char(char::from(b'0' + d)));
            map.bindings.insert(
                slot(Table::Prefix, &k),
                (k, Command { action: "session.focus", arg: Some(d.to_string()) }),
            );
            map.bindings.insert(
                slot(Table::Nav, &k),
                (k, Command { action: "session.focus", arg: Some(d.to_string()) }),
            );
        }
        for (spec_key, value) in &config.keys {
            let (table, key_spec) = if let Some(s) = spec_key.strip_prefix("global:") {
                (Table::Global, s)
            } else if let Some(s) = spec_key.strip_prefix("nav:") {
                (Table::Nav, s)
            } else {
                (Table::Prefix, spec_key.as_str())
            };
            let k = match parse_key(key_spec) {
                Ok(k) => normalize(k),
                Err(e) => {
                    errors.push(format!("keys.{spec_key:?}: {e}"));
                    continue;
                }
            };
            let value = value.trim();
            if value == "none" {
                map.bindings.remove(&slot(table, &k));
                continue;
            }
            let (name, arg) = match value.split_once(char::is_whitespace) {
                Some((n, a)) => (n, Some(a.trim().to_owned())),
                None => (value, None),
            };
            match spec(name) {
                Some(s) => {
                    if s.arg == Arg::Number && arg.as_deref().and_then(|a| a.parse::<usize>().ok()).is_none() {
                        errors.push(format!("keys.{spec_key:?}: {name} needs a number, e.g. \"{name} 1\""));
                        continue;
                    }
                    map.bindings.insert(slot(table, &k), (k, Command { action: s.id, arg }));
                }
                None => errors.push(format!(
                    "keys.{spec_key:?}: unknown action {name:?} (see `{} ?` or the palette for the list)",
                    key_label(&map.prefix)
                )),
            }
        }
        (map, errors)
    }

    pub fn is_prefix(&self, k: KeyEvent) -> bool {
        self.prefixes.contains(&normalize(k))
    }

    pub fn lookup(&self, table: Table, k: KeyEvent) -> Option<&Command> {
        self.bindings.get(&slot(table, &normalize(k))).map(|(_, c)| c)
    }

    /// All bindings for the help overlay: grouped by table, in the order of [`ACTIONS`], with
    /// `session.focus 1`…`9` folded into one `1…9` entry.
    pub fn bindings(&self) -> Vec<(Table, String, Command)> {
        let order = |a: &str| ACTIONS.iter().position(|s| s.id == a).unwrap_or(usize::MAX);
        let mut out: Vec<(Table, String, Command)> = Vec::new();
        for ((t, label), (_, c)) in &self.bindings {
            if c.action == "session.focus"
                && c.arg.as_deref().is_some_and(|a| a.len() == 1)
                && label.len() == 1
            {
                if c.arg.as_deref() == Some("1") {
                    out.push((
                        *t,
                        "1…9".into(),
                        Command { action: c.action, arg: Some("N".into()) },
                    ));
                }
                continue;
            }
            out.push((*t, label.clone(), c.clone()));
        }
        out.sort_by(|a, b| (a.0, order(a.2.action), &a.1).cmp(&(b.0, order(b.2.action), &b.1)));
        out
    }

    /// The key shown next to an action in the palette (prefix table first).
    pub fn key_for(&self, action: &str) -> Option<String> {
        let prefix = key_label(&self.prefix);
        let find = |t: Table| {
            self.bindings
                .iter()
                .filter(|((table, _), (_, c))| *table == t && c.action == action && c.arg.is_none())
                .map(|((_, label), _)| label.clone())
                .next()
        };
        find(Table::Global).or_else(|| find(Table::Prefix).map(|k| format!("{prefix} {k}")))
    }
}

/// Problems in `[keys]` / `ui.prefix`, for `ptyro config check`.
pub fn validate(config: &Config) -> Vec<String> {
    Keymap::new(config).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(c: char, mods: u8) -> KeyEvent {
        KeyEvent::new(KeyCode::Char { c }, Mods(mods))
    }

    #[test]
    fn defaults_and_shifted_characters() {
        let (m, errors) = Keymap::new(&Config::default());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(m.is_prefix(ev('q', Mods::CTRL)));
        assert!(m.is_prefix(ev(' ', Mods::CTRL)));
        assert_eq!(m.lookup(Table::Prefix, ev('d', 0)).unwrap().action, "app.detach");
        // Shift reported separately (kitty) or folded into the character (legacy) both match `X`.
        assert_eq!(m.lookup(Table::Prefix, ev('x', Mods::SHIFT)).unwrap().action, "session.kill");
        assert_eq!(m.lookup(Table::Prefix, ev('X', 0)).unwrap().action, "session.kill");
        assert_eq!(m.lookup(Table::Prefix, ev('?', Mods::SHIFT)).unwrap().action, "help");
        assert_eq!(m.lookup(Table::Prefix, ev('3', 0)).unwrap().number(), Some(3));
        assert_eq!(m.key_for("app.detach").as_deref(), Some("C-q d"));
    }

    #[test]
    fn overrides_unbind_and_report_errors() {
        let c = Config::parse(
            "[ui]\nprefix = \"C-a\"\n[keys]\n\"d\" = \"none\"\n\"global:M-1\" = \"session.focus 1\"\n\"y\" = \"bogus\"\n\"u\" = \"session.focus\"\n",
        )
        .unwrap();
        let (m, errors) = Keymap::new(&c);
        assert!(m.is_prefix(ev('a', Mods::CTRL)));
        assert!(!m.is_prefix(ev('q', Mods::CTRL)), "a configured prefix replaces both defaults");
        assert!(m.lookup(Table::Prefix, ev('d', 0)).is_none());
        assert_eq!(m.lookup(Table::Global, ev('1', Mods::ALT)).unwrap().number(), Some(1));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("bogus") || errors[1].contains("bogus"));
    }

    #[test]
    fn every_default_names_a_real_action() {
        for (_, _, action) in DEFAULTS {
            assert!(spec(action).is_some(), "{action}");
        }
    }
}
