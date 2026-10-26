//! The key map (SPEC §11.3, M2.5): named actions, default bindings in three tables, config
//! overrides, and the help/palette metadata generated from it.
//!
//! Tables: **prefix** (after the prefix key), **global** (always; use sparingly, they steal keys
//! from programs), **nav** (while the sidebar has focus).

use std::collections::BTreeMap;

use ptyro_core::config::{Config, key_label, parse_key};
use ptyro_core::input::KeyEvent;

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
    a("app.quit", "Quit ptyro: stop every session and the daemon (agents hibernate, resumable)"),
    a(
        "session.new",
        "New task: say what to do; ⏎ starts the default agent on it in a new worktree (C-o: pick)",
    ),
    a("session.command", "New session running a command line (empty: your shell)"),
    a("worktree.new", "New worktree of this project (+ an agent), after its setup script"),
    a("workspace.switch", "Switch workspace (each has its own projects, profile and layout)"),
    a("worktree.open", "Open this worktree in an editor ([editors]; nvim opens here)"),
    a("script.run", "Run the project's default run script in this worktree"),
    a("script.pick", "Pick a lifecycle script to run in this worktree"),
    a("project.add", "Add a git repository to this workspace (discovered under discover.roots)"),
    a("project.trust", "Review and trust this project's scripts"),
    a(
        "project.suggest",
        "What this project needs (a setup script from its lockfile, .env copies), to use",
    ),
    a("worktree.remove", "Remove this worktree (refuses if work would be lost)"),
    a("worktree.retitle", "Change this worktree's title: what it's for, for people to read"),
    a(
        "worktree.finish",
        "Finish this worktree: commit what's left (its agent writes the message), then a PR or integrate",
    ),
    a(
        "worktree.archive",
        "Archive this worktree: its branch and a snapshot of its files are kept, the checkout goes",
    ),
    a("worktree.restore", "Bring an archived worktree back, as it was"),
    a("worktree.gc", "Stale worktrees (merged, gone, unused): pick one to archive it"),
    a(
        "shell.here",
        "The quick terminal: this worktree's shell in a drawer under its project's pane, or in a panel of its own when none is open ([ui] shell_style); again focuses, then hides it",
    ),
    a(
        "shell.drawer",
        "The quick terminal in a drawer below a pane of its project (a panel of its own, with a note, when none is open); again focuses, then hides it",
    ),
    a(
        "shell.pane",
        "The quick terminal in a panel of its own, replacing the pane you are in; again goes back",
    ),
    a(
        "scratchpad.toggle",
        "The scratchpad: your notes file in the editor, in a drawer, popup or pane ([scratchpad] mode); again focuses, then hides it",
    ),
    a(
        "scratchpad.add",
        "Add a line to the scratchpad without opening it, stamped with where you are",
    ),
    a(
        "scratchpad.items",
        "The scratchpad's open items: search them, tick one, start one as a task",
    ),
    a("scratchpad.history", "The scratchpad's saved copies: put one back"),
    a(
        "scratchpad.note",
        "Note this to the scratchpad: the selected session's task, or what you type",
    ),
    a("session.next", "Show the next session in sidebar order (across projects) here"),
    a("session.prev", "Show the previous session in sidebar order (across projects) here"),
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
    a(
        "session.adopt",
        "Adopt the Claude started by hand in this shell: a managed session, same conversation",
    ),
    a("session.hibernate", "Stop an idle agent, keeping it resumable"),
    a("session.explain", "Why is it in this state? The status engine's recent decisions"),
    a(
        "session.summarize",
        "Summarize it: what it's doing, in a line, from a small model ([summaries])",
    ),
    a("session.mute", "Mute this session's notifications for an hour (the inbox still gets them)"),
    a("notify.test", "Send a test notification through every channel and sink"),
    a("session.last", "Show the previously shown session here"),
    a("session.stop", "Stop the session (HUP→TERM→KILL); removes it if it already exited"),
    a("session.kill", "Kill the session's process group now (SIGKILL)"),
    a("session.restart", "Run an exited session's command again"),
    a("session.rename", "Rename the session: its address (C-t in the dialog: its title)"),
    a("session.retitle", "Change the session's title: what it's about, for people to read"),
    a("session.take_control", "Take control: resize the session to this pane"),
    a("focus.toggle", "Move focus between the sidebar and the pane"),
    a("sidebar.toggle", "Show or hide the sidebar"),
    a("toast.act", "Do what the last message offers: undo it, or fix what it reports"),
    a("home", "Home: who needs you, PRs, recent projects and repositories found, in one list"),
    a(
        "app.about",
        "About ptyro: version, repository, where its files live, the daemon and web app, a bug report to copy",
    ),
    a("health", "Mend: what the doctor found (the status bar's ⚕), each with its fix"),
    a("doctor", "Run the full doctor (agents, logins, integrations' self-tests) in a split"),
    a("daemon.logs", "Follow the daemon's log in a split"),
    a("daemon.status", "The daemon's status and health, in a split"),
    a(
        "daemon.restart",
        "Restart the daemon (agents come back dormant, resumable); this TUI reconnects",
    ),
    a("daemon.upgrade", "Switch the daemon to the installed binary, live (sessions keep running)"),
    a(
        "app.backup",
        "Back up ptyro's own state (config, database, scratchpad) to a private .tar.gz, in a split",
    ),
    a(
        "app.uninstall",
        "Uninstall ptyro: shows what goes and what stays, then asks (in a split; it ends this TUI and the daemon)",
    ),
    a("daemon.service", "Start the daemon at login (a systemd user unit or launchd agent)"),
    a("daemon.unservice", "Stop starting the daemon at login"),
    a("integrations", "The agent integrations: what's installed and what each launch gets"),
    a("integrations.install", "Install an agent's opt-in integration (Gemini, Antigravity)"),
    a("integrations.uninstall", "Remove an agent's opt-in integration"),
    a("first.run", "The first-run screen again: agents, accounts and repositories found"),
    a("row.menu", "Everything you can do with this session, worktree or project, with its keys"),
    a("sidebar.wider", "Make the sidebar a column wider (also: drag its edge)"),
    a("sidebar.narrower", "Make the sidebar a column narrower (also: drag its edge)"),
    a("sidebar.zoom", "Everything at once: the sidebar fills the screen, every row open (toggle)"),
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
    a(
        "config.panel",
        "Settings: every option, searchable, edited in place (no file to know about)",
    ),
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
    a(
        "help.tour",
        "The tour: start a task, answer it, review, finish and clean up, with a pretend agent",
    ),
    a(
        "inspector",
        "Inspector: the worktree's diff (comment, commit, PR), commits, checks, ports, events",
    ),
    a("review.queue", "Worktrees ready for review, best first"),
    a("runs", "Runs: recipes and fan-outs, their steps and gates; compare and pick candidates"),
    a("sets", "Change sets: members as swimlanes, gates, issues, the ledger as a timeline"),
    // ── UX1.21: everything else the CLI does, in the TUI ──
    a(
        "session.convert",
        "Switch this agent between its own terminal UI and structured mode, same conversation",
    ),
    a("session.replay", "Play this session's recording back, in a split"),
    a("session.export", "Export this session's recording (asciicast, text or a test fixture)"),
    a("session.verify", "Verified-done: its turns count as done only when a check passes"),
    a(
        "session.message",
        "Send this agent a message through the mailbox (it reads it between turns)",
    ),
    a("session.lead", "Make this agent a lead: it gets ptyro's tools to start and manage workers"),
    a("checkpoint.list", "This session's checkpoints: its worktree after every turn"),
    a("checkpoint.diff", "What changed since a checkpoint"),
    a("checkpoint.restore", "Put the worktree back to a checkpoint (asks first)"),
    a("pr.reply", "Reply on this worktree's PR (marked \"via ptyro\")"),
    a("pr.open", "Go to a PR's worktree and session, or start one on it"),
    a("project.show", "This project: its root, branches, pin and config, in a split"),
    a("project.config", "This project's effective config, and where each value comes from"),
    a(
        "project.remove",
        "Forget this project (its worktrees stay on disk; it won't register on its own again)",
    ),
    a("project.untrust", "Stop trusting this project's scripts"),
    a("project.pin", "Pin this project to a profile (its sessions run as it)"),
    a("project.unpin", "Unpin this project from its profile"),
    a("project.leave", "Take this project out of this workspace (it stays registered)"),
    a("worktree.lock", "Lock this worktree (git worktree lock: nothing prunes it)"),
    a("worktree.unlock", "Unlock this worktree"),
    a(
        "worktree.pair",
        "Show this worktree's work in the main checkout (for your IDE and dev server)",
    ),
    a("worktree.unpair", "Give the main checkout back, your edits carried to the worktree"),
    a("worktree.rename_branch", "Rename this worktree's branch (never once it's pushed)"),
    a("workspace.rename", "Rename this workspace"),
    a("workspace.remove", "Remove this workspace (its projects stay registered)"),
    a("workspace.profile", "Which profile this workspace runs as"),
    a("workspace.notify", "How this workspace notifies you: all, badge or mute"),
    a(
        "workspace.mute",
        "Mute this workspace's notifications for an hour (the inbox still gets them)",
    ),
    a("project.mute", "Mute this project's notifications for an hour (the inbox still gets them)"),
    a("profile.list", "Your profiles: accounts, git identity, SSH key, in a split"),
    a("profile.show", "One profile in full"),
    a("profile.new", "A new profile"),
    a("profile.remove", "Remove a profile"),
    a("profile.rename", "Rename a profile"),
    a("profile.login", "Log a profile in to an agent (or gh), in a split"),
    a("profile.sync", "Copy a profile's agent settings from the default one"),
    a("recipe.list", "Recipes: workflows with steps and gates"),
    a("recipe.show", "One recipe's steps"),
    a("recipe.run", "Run a recipe (its gates wait for you in the runs view)"),
    a("run.fork", "Fork a run from one of its steps"),
    a("leases", "File leases: which agent claims what"),
    a("radar", "Overlapping worktrees and the conflicts they predict"),
    a("sets.new", "A change set: one change across several repositories"),
    a("sets.templates", "Change-set templates"),
    a("set.checkpoints", "This change set's checkpoints (every repository at once)"),
    a("set.restore", "Put every repository of this set back to a checkpoint"),
    a("set.contract", "This set's contract: show, edit, diff or its history"),
    a("set.overlay", "Build this set's members against each other's unpushed code"),
    a("set.commit", "Commit in every member at once"),
    a("set.push", "Push every member"),
    a("set.pr", "Open a PR per member"),
    a("set.land", "Land the set's PRs in dependency order (the merge train)"),
    a("set.add", "Add a repository to this set"),
    a("set.drop", "Drop a member from this set"),
    a("set.remove", "Remove this change set"),
    a("remote.serve", "Serve the web app for your phone (Tailscale or a listen address)"),
    a("remote.web", "The web app's server: is it up, at what address, and what to do next"),
    a("remote.web_on", "Have the daemon run the web app's server, now and whenever it starts"),
    a("remote.web_off", "Stop the daemon running the web app's server (devices stay paired)"),
    a("remote.pair", "Pair a phone: a QR code to scan, in a split"),
    a("devices.list", "Paired devices"),
    a("devices.revoke", "Cut a paired device off now"),
    a("devices.push_test", "Send a test push to your devices"),
    a("config.show", "The effective config, defaults filled in"),
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

pub use ptyro_core::config::normalize_key as normalize;

fn key(spec: &str) -> KeyEvent {
    normalize(parse_key(spec).expect("default key spec"))
}

const DEFAULTS: &[(Table, &str, &str)] = &[
    (Table::Prefix, "?", "help"),
    (Table::Prefix, ":", "palette"),
    (Table::Prefix, "C-p", "palette"),
    (Table::Prefix, "d", "app.detach"),
    (Table::Prefix, "q", "app.detach"),
    (Table::Prefix, "C-d", "app.quit"),
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
    (Table::Prefix, "Z", "sidebar.zoom"),
    (Table::Prefix, "u", "toast.act"),
    (Table::Prefix, ".", "row.menu"),
    (Table::Prefix, "!", "shell.here"),
    (Table::Prefix, ";", "scratchpad.toggle"),
    (Table::Prefix, "'", "scratchpad.add"),
    (Table::Prefix, "\"", "scratchpad.items"),
    (Table::Prefix, "C-t", "shell.drawer"),
    (Table::Prefix, "T", "shell.pane"),
    (Table::Prefix, "H", "home"),
    (Table::Prefix, "M", "health"),
    (Table::Prefix, "I", "app.about"),
    (Table::Prefix, "C", "config.panel"),
    (Table::Prefix, "P", "project.add"),
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
    (Table::Nav, "z", "sidebar.zoom"),
    (Table::Nav, "u", "toast.act"),
    (Table::Nav, ".", "row.menu"),
    (Table::Nav, "!", "shell.here"),
    (Table::Nav, "T", "shell.pane"),
    (Table::Nav, ";", "scratchpad.toggle"),
    (Table::Nav, "'", "scratchpad.add"),
    (Table::Nav, "H", "home"),
    (Table::Nav, "M", "health"),
    (Table::Nav, "I", "app.about"),
    (Table::Nav, "C", "config.panel"),
    (Table::Nav, "F", "worktree.finish"),
    (Table::Nav, "P", "project.add"),
    (Table::Nav, "A", "session.adopt"),
    (Table::Nav, ">", "sidebar.wider"),
    (Table::Nav, "<", "sidebar.narrower"),
    (Table::Nav, "v", "pane.split_right"),
    (Table::Nav, "s", "pane.split_down"),
];

/// The default bindings named as `[keys]` names them (`d`, `nav:j`), for the settings panel and
/// `ptyro config list`: what a key does before any override.
pub fn default_keys() -> Vec<(String, String)> {
    let named = |table: Table, k: &str| match table {
        Table::Prefix => k.to_owned(),
        Table::Nav => format!("nav:{k}"),
        Table::Global => format!("global:{k}"),
    };
    let mut out: Vec<(String, String)> =
        DEFAULTS.iter().map(|(t, k, a)| (named(*t, k), (*a).to_owned())).collect();
    for d in 1..=9 {
        for t in [Table::Prefix, Table::Nav] {
            out.push((named(t, &d.to_string()), format!("session.focus {d}")));
        }
    }
    out
}

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
            // Only the default digit keys fold: `y` bound to `session.focus 2` is its own line.
            if c.action == "session.focus"
                && c.arg.as_deref().is_some_and(|a| a.len() == 1)
                && c.arg.as_deref() == Some(label.as_str())
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

    /// The key bound to `action` in `table` (no prefix shown), if any.
    pub fn key_in(&self, table: Table, action: &str) -> Option<String> {
        self.keys_of(table, action).into_iter().next()
    }

    /// The keys bound to `action` in `table`, the ones to show first first: a single character
    /// (`j`) over a named key (`Down`) or a chord (`C-p`), then by name.
    fn keys_of(&self, table: Table, action: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .bindings
            .iter()
            .filter(|((t, _), (_, c))| *t == table && c.action == action && c.arg.is_none())
            .map(|((_, label), _)| label.clone())
            .collect();
        keys.sort_by(|a, b| {
            (a.chars().count() > 1, a.len(), a).cmp(&(b.chars().count() > 1, b.len(), b))
        });
        keys
    }

    /// The key shown next to an action in the palette (prefix table first).
    pub fn key_for(&self, action: &str) -> Option<String> {
        let prefix = key_label(&self.prefix);
        let find = |t: Table| self.keys_of(t, action).into_iter().next();
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
    use ptyro_core::input::{KeyCode, Mods};

    fn ev(c: char, mods: u8) -> KeyEvent {
        KeyEvent::new(KeyCode::Char { c }, Mods(mods))
    }

    /// `docs/keys.md` is the page people read for keys: every default key after the prefix is in it.
    #[test]
    fn the_keys_page_lists_every_default_key() {
        const DOC: &str = include_str!("../../../docs/keys.md");
        // Arrow keys are described in words, as a group.
        let words = ["Down", "Up", "S-Left", "S-Right", "S-Up", "S-Down", "PageUp"];
        let missing: Vec<&str> = DEFAULTS
            .iter()
            .filter(|(table, key, _)| *table == Table::Prefix && !words.contains(key))
            .filter(|(_, key, _)| !DOC.contains(&format!("`{key}`")))
            .map(|(_, key, _)| *key)
            .collect();
        assert!(
            missing.is_empty(),
            "docs/keys.md never shows these keys after the prefix: {missing:?}"
        );
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

    /// The settings panel lists the defaults from `default_keys`: it must be exactly the key map's.
    #[test]
    fn the_listed_defaults_are_the_keymaps_own() {
        use ptyro_core::settings::{self, Lookups};
        let lk = Lookups {
            themes: Vec::new(),
            actions: ACTIONS.iter().map(|a| (a.id.to_owned(), a.desc.to_owned())).collect(),
            keys: default_keys(),
        };
        let (m, _) = Keymap::new(&Config::default());
        let rows = settings::bindings(&serde_json::Value::Null, &lk);
        // The nine digits are one row per table; everything else is a row of its own.
        assert_eq!(rows.len(), DEFAULTS.len() + 2, "one row per default key");
        for b in rows.iter().filter(|b| !b.fixed) {
            let table = match b.table {
                "nav" => Table::Nav,
                "global" => Table::Global,
                _ => Table::Prefix,
            };
            let k = parse_key(&b.key).unwrap();
            let c = m.lookup(table, k).unwrap_or_else(|| panic!("{} isn't bound", b.name));
            assert_eq!(c.action, b.action.split(' ').next().unwrap(), "{}", b.name);
            assert!(!b.help.is_empty() && !b.help.contains("not an action"), "{}", b.name);
        }
        assert!(rows.iter().any(|b| b.fixed && b.table == "prefix" && b.key == "1…9"));
    }

    #[test]
    fn every_default_names_a_real_action() {
        for (_, _, action) in DEFAULTS {
            assert!(spec(action).is_some(), "{action}");
        }
    }
}
