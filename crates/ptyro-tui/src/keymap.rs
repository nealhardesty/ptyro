//! The key map (SPEC §11.3, M2.5): named actions, default bindings in three tables, config
//! overrides, and the help/palette metadata generated from it.
//!
//! Tables: **prefix** (after the prefix key), **global** (always; use sparingly, they steal keys
//! from programs), **nav** (while the sidebar has focus), **chat** (in a chat, before its
//! composer takes the key) and **chatlist** (in the chat list).

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
    a(
        "prefix.send",
        "Send the prefix key itself to the program (a ptyro inside ptyro gets its prefix)",
    ),
    a("app.quit", "Quit ptyro: stop every session and the daemon (agents hibernate, resumable)"),
    a(
        "session.new",
        "New task: say what to do; ⏎ starts the default agent on it in a new worktree (C-o: pick)",
    ),
    a(
        "session.again",
        "Quick new: another of the agent this worktree last started (claude the first time), here, no questions",
    ),
    a("session.command", "New session running a command line (empty: your shell)"),
    a("worktree.new", "New worktree of this project (+ an agent), after its setup script"),
    a("workspace.switch", "Switch workspace (each has its own projects, profile and layout)"),
    a(
        "worktree.open",
        "Open this worktree in an installed editor, the file manager, a terminal app, or a new shell (!)",
    ),
    a("script.run", "Run the project's default run script in this worktree"),
    a("script.pick", "Pick a lifecycle script to run in this worktree"),
    a("project.add", "Add a git repository to this workspace (discovered under discover.roots)"),
    a("project.trust", "Review and trust this project's scripts"),
    a("worktree.remove", "Remove this worktree (refuses if work would be lost)"),
    a("worktree.gc", "Stale worktrees (merged, gone, unused): pick one to remove it"),
    a("shell.new", "New shell: another one of its own in this worktree, full sized"),
    a(
        "machine.add",
        "Mirror another machine's workspaces here: ssh://host, or the code its machine.pair printed",
    ),
    a("machine.pair", "A code for another machine to mirror this one (machine.add there)"),
    a("machine.list", "Other machines mirrored here: their state and workspaces"),
    a("machine.retry", "Try this workspace's machine's link again now"),
    a(
        "machine.update",
        "Have this workspace's machine update its own ptyro: it downloads and checks the newest release there and upgrades its daemon live (its sessions carry on); asks first",
    ),
    a("machine.hide", "Hide this workspace of another machine here (nothing changes there)"),
    a("machine.unhide", "Show a hidden workspace of another machine again"),
    a("machine.remove", "Stop mirroring this workspace's machine (nothing changes there)"),
    a(
        "shell.here",
        "The quick terminal: this worktree's one shell, as [ui] shell_style says (a full-sized panel, or a drawer while its project is open); again focuses, then hides it",
    ),
    a(
        "shell.drawer",
        "The quick terminal in a drawer below a pane of its project (a panel of its own, with a note, when none is open); again focuses, then hides it",
    ),
    a(
        "shell.pane",
        "The quick terminal in a panel of its own, replacing the pane you are in; again goes back",
    ),
    a("session.next", "Show the next session in sidebar order (across projects) here"),
    a("session.prev", "Show the previous session in sidebar order (across projects) here"),
    ActionSpec {
        id: "session.focus",
        desc: "Show session N (sidebar order) in this pane",
        arg: Arg::Number,
    },
    a("session.find", "Find a session (fuzzy) and show it here"),
    ActionSpec {
        id: "shells.go",
        desc: "Show shell N of this workspace's SHELLS (an empty number starts a shell there)",
        arg: Arg::Number,
    },
    a(
        "shells.new",
        "A new shell in this workspace's SHELLS, in the pane's directory, full sized (tmux's c)",
    ),
    a(
        "shells.menu",
        "The shells menu: this workspace's SHELLS by number; a digit shows one, c makes one",
    ),
    a(
        "shells.scratch",
        "A scratch shell in SHELLS: a new, empty directory of its own that goes with it",
    ),
    a(
        "shells.command",
        "A new shell with every option: a command (htop, ssh prod), where, its number, name, title",
    ),
    a("shells.move", "Give this shell another number in SHELLS (one already there swaps)"),
    a(
        "shells.watch",
        "Watch this shell: when its command ends, new output, silence, or a line that matches",
    ),
    a(
        "shells.loose",
        "Move this session to its workspace's SHELLS, with a number: it outlives its worktree",
    ),
    a("shells.adopt", "Move this shell from SHELLS into the worktree its directory is in"),
    a(
        "shells.break",
        "Break this pane out of its SHELLS window to a number of its own (tmux's break-pane)",
    ),
    a("shells.join", "Join this shell into another SHELLS window as a pane (tmux's join-pane)"),
    a("attention.next", "Jump to the session that needs you most (blocked first)"),
    a("peek", "Peek at a session's screen and what it wants; reply or answer inline"),
    a("session.resume", "Resume a dormant agent into its conversation"),
    a("session.hibernate", "Stop an idle agent, keeping it resumable"),
    a("session.explain", "Why is it in this state? The status engine's recent decisions"),
    a(
        "session.private",
        "Make this session private: what the knowledge base kept of it is deleted for good, and nothing more of it is kept (asks first; it can't be switched off again)",
    ),
    a(
        "session.mute",
        "Mute this session's notifications for an hour (its row still shows when it wants you)",
    ),
    a("session.tab", "Open this session in a new tab of your terminal (tmux, Windows Terminal)"),
    a("session.pane", "Open this session in a new pane beside this one (tmux, Windows Terminal)"),
    a("notify.test", "Send a test notification through every channel and sink"),
    a(
        "session.last",
        "Back and forth: a chat and the session under it, else the last session here",
    ),
    a(
        "session.stop",
        "Stop the session, kept listed: an agent's turn is interrupted and it gets a moment to exit, then HUP, TERM, KILL. On an ended one: remove it (a dormant one asks first)",
    ),
    a("session.kill", "Kill the session now (SIGKILL, no chance to clean up), kept listed"),
    a(
        "session.remove",
        "Remove the session from the list: an ended one (a dormant one asks first: its conversation is forgotten), or stop a running one first",
    ),
    a("session.restart", "Run an exited session's command again"),
    a(
        "session.rename",
        "Rename the session: its name, its address (C-t in the dialog: its title, what people read)",
    ),
    a("session.take_control", "Take control: resize the session to this pane"),
    a("focus.toggle", "Move focus between the sidebar and the pane"),
    a("sidebar.toggle", "Show or hide the sidebar"),
    a("sidebar.filter", "Sidebar: all sessions → only the running ones → only the active ones"),
    a("sidebar.show_all", "Sidebar: list every session"),
    a("sidebar.show_running", "Sidebar: list only the sessions that are running (a live process)"),
    a(
        "sidebar.show_active",
        "Sidebar: list only the active sessions (an agent working or waiting for you, a shell running a command)",
    ),
    a("toast.act", "Do what the last message offers: undo it, or fix what it reports"),
    a("home", "Home: who needs you, recent projects and repositories found, in one list"),
    a(
        "app.about",
        "About ptyro: version, repository, where its files live, the daemon and web app, a bug report to copy",
    ),
    a("health", "Mend: what the doctor found (the status bar's ⚕), each with its fix"),
    a("doctor", "Run the full doctor (agents, logins, the hooks' self-tests) in a split"),
    a("daemon.logs", "Follow the daemon's log in a split"),
    a("daemon.status", "The daemon's status and health, in a split"),
    a(
        "daemon.restart",
        "Restart the daemon: running sessions are stopped; agents hibernate and come back resumable",
    ),
    a("daemon.upgrade", "Switch the daemon to the installed binary, live (sessions keep running)"),
    a(
        "app.uninstall",
        "Uninstall ptyro: shows what goes and what stays, then asks (in a split; it ends this TUI and the daemon)",
    ),
    a("daemon.service", "Start the daemon at login (a systemd user unit, or a launchd agent)"),
    a("daemon.unservice", "Stop starting the daemon at login"),
    a(
        "first.run",
        "Set ptyro up, all of it: agents and tools to install, accounts and repositories found",
    ),
    a(
        "app.setup",
        "Set up agents and tools: the checklist (Claude Code, Codex, OpenCode, git, GitHub CLI, Node.js); ⏎ installs in a split, then the logins",
    ),
    a("app.install", "Install an agent or tool by name, with what it needs first, in a split"),
    a(
        "app.update",
        "Update your agents, and ptyro itself, when a newer version is out (looked for once a day), in a split: it shows each update's command and asks first; ptyro's own is checked against its SHA256SUMS, then the daemon upgrades live",
    ),
    a(
        "app.login",
        "Log in to the agents and the GitHub CLI, one account each: each tool's own sign-in, in a split",
    ),
    a("row.menu", "Everything you can do with this session, worktree or project, with its keys"),
    a("sidebar.wider", "Make the sidebar a column wider (also: drag its edge)"),
    a("sidebar.narrower", "Make the sidebar a column narrower (also: drag its edge)"),
    a("sidebar.zoom", "Everything at once: the sidebar fills the screen, every row open (toggle)"),
    a(
        "sidebar.fold_all",
        "Sidebar: fold everything (workspaces, projects, worktrees, chats); again: unfold it as it was",
    ),
    a(
        "pane.split_right",
        "Split beside this pane: ⏎ a clone of its session, a new shell, or any session",
    ),
    a(
        "pane.split_down",
        "Split below this pane: ⏎ a clone of its session, a new shell, or any session",
    ),
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
    a(
        "config.panel",
        "Settings: every option, searchable, edited in place (no file to know about)",
    ),
    a("config.edit", "Edit the config file in a new session"),
    a("nav.down", "Sidebar: select the next session"),
    a("nav.up", "Sidebar: select the previous session"),
    a("nav.top", "Sidebar: select the first session"),
    a("nav.bottom", "Sidebar: select the last session"),
    a(
        "nav.open",
        "Sidebar: show the selected session, or fold/unfold a workspace, project or worktree",
    ),
    a(
        "nav.select",
        "Sidebar: mark the selected session for broadcast, or fold/unfold the selected row",
    ),
    a("broadcast", "Type one prompt into every marked session (each once it's ready)"),
    a(
        "chat.new",
        "New chat with your agent, in this workspace: type, ⏎ sends (C-o picks the agent and a folder)",
    ),
    a(
        "chat.list",
        "Chats: this workspace's (Tab: every one), searchable; open, pin, archive, delete",
    ),
    a("chat.open", "Open this chat"),
    a("chat.rename", "Rename this chat: its title, what people read (empty clears it)"),
    a("chat.autotitle", "A new automatic title for this chat, made from the conversation"),
    a("chat.pin", "Pin this chat to the top of CHATS, or unpin it"),
    a(
        "chat.archive",
        "Archive this chat (the chat list's A shows archived ones; again brings it back)",
    ),
    a("chat.delete", "Delete this chat (asks first)"),
    a(
        "chat.agent",
        "Switch this chat to another agent: the next message carries the conversation over (a new chat: pick its agent and folder)",
    ),
    a("chat.folder", "A folder this chat may read, or none"),
    a("chat.task", "Make it a task: an agent in a new worktree carries out the chat's last answer"),
    a("chat.export", "Write this chat out as Markdown"),
    a("chat.stop", "Stop the answer being written"),
    a("chat.retry", "Ask again: a new answer replaces the last one"),
    a("chat.edit", "Say your last message differently: it and its answer are replaced"),
    a(
        "chat.incognito",
        "New incognito chat: nothing of it is kept or searchable, it ends with its agent",
    ),
    a(
        "chat.search",
        "Search every chat's messages, on every machine (words, \"phrases\", prefix*, -word, agent:, since:)",
    ),
    a("chat.find", "Ask your chats a question: an agent searches them and answers, citing where"),
    a("chat.find_in", "Find in this chat (again: the next match)"),
    a("chat.cite", "Open a citation (c:7f3k/…) of this chat's answers"),
    a("chat.allow", "Answer the chat's permission card: allow it once"),
    a("chat.always", "Answer the chat's permission card: always allow it"),
    a("chat.deny", "Answer the chat's permission card: deny it"),
    a("chat.page_up", "Scroll the chat a page up"),
    a("chat.page_down", "Scroll the chat a page down"),
    a("chat.list_search", "The chat list: type into its search box"),
    a("chat.list_all", "The chat list: this workspace's chats, or every workspace's"),
    a("chat.list_archived", "The chat list: the archived chats instead (again: the others)"),
    a(
        "chat.list_state",
        "The chat list: only the chats that need you, then the working, unread, pinned ones, then all",
    ),
    // ── UX1.21: everything else the CLI does, in the TUI ──
    a("session.events", "What happened in this session: its last events, in a split"),
    a("checkpoint.list", "This session's checkpoints: its worktree after every turn"),
    a("checkpoint.diff", "What changed since a checkpoint"),
    a("checkpoint.restore", "Put the worktree back to a checkpoint (asks first)"),
    a("project.show", "This project: its root, branches, pin and config, in a split"),
    a("project.config", "This project's effective config, and where each value comes from"),
    a(
        "project.remove",
        "Forget this project (its worktrees stay on disk; it won't register on its own again)",
    ),
    a("project.untrust", "Stop trusting this project's scripts"),
    a("project.rename", "Rename this project: its slug, the address of its worktrees and sessions"),
    a(
        "project.move",
        "Its repository moved: point this project at where it is now (keeps its trust, pin, workspaces and worktrees)",
    ),
    a("project.pin", "Pin this project to a profile (its sessions run as it)"),
    a("project.unpin", "Unpin this project from its profile"),
    a("project.leave", "Take this project out of this workspace (it stays registered)"),
    a("project.workspaces", "Which workspaces this project is in: tick them"),
    a(
        "worktree.rename",
        "Rename this worktree: its name, the address (branch and directory stay); C-t: its title, what people read (the main checkout has only a title)",
    ),
    a("workspace.rename", "Rename this workspace"),
    a(
        "workspace.remove",
        "Remove this workspace (its sessions, chats and only-here projects move to main)",
    ),
    a("workspace.profile", "Which profile this workspace runs as"),
    a("workspace.notify", "How this workspace notifies you: all, badge or mute"),
    a(
        "workspace.mute",
        "Mute this workspace's notifications for an hour (rows still show what wants you)",
    ),
    a(
        "project.mute",
        "Mute this project's notifications for an hour (rows still show what wants you)",
    ),
    a("profile.list", "Your profiles: accounts, git identity, SSH key, in a split"),
    a("profile.show", "One profile in full"),
    a("profile.new", "A new profile"),
    a("profile.remove", "Remove a profile"),
    a("profile.rename", "Rename a profile"),
    a("profile.login", "Log a profile in to an agent (or gh), in a split"),
    a("profile.sync", "Copy a profile's agent settings from the default one"),
    a("remote.web", "The web app's server: is it up, at what address, and what to do next"),
    a("remote.web_on", "Have the daemon run the web app's server, now and whenever it starts"),
    a("remote.web_off", "Stop the daemon running the web app's server (devices stay paired)"),
    a("remote.pair", "Pair a phone: a QR code to scan, in a split"),
    a(
        "remote.pair.browser",
        "Open the web app in this computer's browser, paired (inside WSL: the Windows browser)",
    ),
    a("devices.list", "Paired devices"),
    a("devices.revoke", "Cut a paired device off now"),
    a("devices.push_test", "Send a test push to your devices"),
    a("config.show", "The effective config, defaults filled in"),
    a(
        "kb.open",
        "The Library: everything ptyro kept of your work (agent sessions) and your chats, searched and read in one place; with nothing typed, the history by day",
    ),
    a(
        "kb.search",
        "Search what was kept, and your chats, in the Library (words, \"a phrase\", prefix*, kind:session, project:, since:7d, agent:, by:meaning)",
    ),
    a("kb.recent", "The Library's history: what was kept, newest first, by day"),
    a(
        "kb.history",
        "What was kept of this session, worktree or project: the Library, with only that",
    ),
    a(
        "kb.show",
        "Open what the Library found (the selected row; else asks for an address like k:7f3k/12): the item in the reader, at that piece",
    ),
    a(
        "kb.forget",
        "Forget something kept (the Library's selected row; else asks for an address): its text is deleted for good and never kept again; asks first",
    ),
    a(
        "kb.status",
        "The knowledge base in the Library: on or off, what it holds, what was not kept and why",
    ),
    a("kb.enable", "Switch the knowledge base on: agent sessions are kept from then on"),
    a(
        "kb.notes",
        "The Library's notes: what you and your agents wrote down, by project (n writes one, ⏎ edits one in your editor, x deletes it)",
    ),
    a(
        "kb.note",
        "Write a note (a decision, a fact, a how-to): its text or your editor, its title, the project it is about, whose agents may see it, its tags",
    ),
    a(
        "kb.sync",
        "Look again now: the notes folder is read (what you wrote, changed or deleted there by hand is found), and every project and folder is looked at for files that changed",
    ),
    a(
        "kb.sources",
        "The Library's Sources: what is read into the knowledge base (every project, every folder or file given to it), what each holds, how it stands and how far a read of it is",
    ),
    a(
        "kb.ingest",
        "Read a folder or a file into the knowledge base and keep reading it (a form with every option): its documents, its files' names, its source comments, its commits",
    ),
    a(
        "kb.backfill",
        "Read what your agents kept before ptyro did: every conversation in Claude Code's and Codex's own folders that isn't kept yet (it counts them first, and asks)",
    ),
    a(
        "kb.remove",
        "Stop reading the folder or file selected in the Library's Sources, and drop what was kept of it (your files aren't touched); asks first",
    ),
    a("nav.collapse", "Sidebar: fold the selected workspace, project or worktree"),
    a("nav.expand", "Sidebar: unfold the selected workspace, project or worktree"),
];

pub fn spec(id: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|s| s.id == id)
}

/// Actions that were renamed (2026-11-17): a `[keys]` line written with the old name still runs
/// the action under its new one.
const RENAMED: &[(&str, &str)] = &[
    ("chat.retitle", "chat.rename"),
    ("session.retitle", "session.rename"),
    ("worktree.retitle", "worktree.rename"),
];

/// The action a `[keys]` line names: by its id, or by the name it had before a rename.
fn bound_action(name: &str) -> Option<&'static ActionSpec> {
    let name = RENAMED.iter().find(|(old, _)| *old == name).map_or(name, |(_, new)| new);
    spec(name)
}

/// A `[keys]` name as a `ptyro config` command line takes it: `keys.C-.` (everything after
/// `keys.` is the name), in quotes when the shell would otherwise split or change it.
pub fn key_setting(name: &str) -> String {
    let setting = format!("keys.{name}");
    if name.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.+,=@%/^".contains(c)) {
        setting
    } else if !name.contains('\'') {
        format!("'{setting}'")
    } else {
        format!("\"{setting}\"")
    }
}

/// A command line as a shell splits it: words between spaces, quotes taken off (`'keys.C- '`
/// is one word). What the TUI and `ptyro doctor --fix` run of a command a message names.
pub fn shell_words(line: &str) -> Vec<String> {
    let (mut words, mut word, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => (quote, any) = (Some(c), true),
            (None, c) if c.is_whitespace() => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            (None, c) => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Table {
    Prefix,
    Global,
    Nav,
    Chat,
    ChatList,
}

impl Table {
    pub fn label(self) -> &'static str {
        match self {
            Self::Prefix => "after the prefix",
            Self::Global => "anywhere",
            Self::Nav => "in the sidebar",
            Self::Chat => "in a chat",
            Self::ChatList => "in the chat list",
        }
    }

    /// What `[keys]` names put before a key of this table (`nav:j`); none after the prefix.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Prefix => "",
            Self::Global => "global:",
            Self::Nav => "nav:",
            Self::Chat => "chat:",
            Self::ChatList => "chatlist:",
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
    (Table::Prefix, "q", "prefix.send"),
    (Table::Prefix, "C-d", "app.quit"),
    (Table::Prefix, "n", "session.new"),
    (Table::Prefix, "N", "worktree.new"),
    (Table::Prefix, "C-n", "session.again"),
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
    (Table::Prefix, "p", "peek"),
    (Table::Prefix, "b", "broadcast"),
    (Table::Prefix, "m", "chat.new"),
    (Table::Prefix, "g", "chat.list"),
    (Table::Prefix, "C-g", "chat.search"),
    (Table::Prefix, "`", "session.last"),
    (Table::Prefix, "x", "session.stop"),
    (Table::Prefix, "X", "session.kill"),
    (Table::Prefix, "C-r", "session.restart"),
    (Table::Prefix, ",", "session.rename"),
    (Table::Prefix, "=", "session.take_control"),
    (Table::Prefix, "Tab", "focus.toggle"),
    // A prefix pressed twice: the quickest way to the sidebar (a prefix you set gets it too).
    (Table::Prefix, "C-q", "focus.toggle"),
    (Table::Prefix, "C-Space", "focus.toggle"),
    (Table::Prefix, "\\", "sidebar.toggle"),
    (Table::Prefix, "Z", "sidebar.zoom"),
    (Table::Prefix, "C-z", "sidebar.fold_all"),
    (Table::Prefix, "f", "sidebar.filter"),
    (Table::Prefix, "u", "toast.act"),
    (Table::Prefix, ".", "row.menu"),
    (Table::Prefix, "!", "shell.new"),
    (Table::Prefix, "C-t", "shell.drawer"),
    (Table::Prefix, "T", "shell.pane"),
    (Table::Prefix, "H", "home"),
    (Table::Prefix, "G", "worktree.gc"),
    (Table::Prefix, "M", "health"),
    (Table::Prefix, "I", "app.about"),
    (Table::Prefix, "L", "kb.open"),
    (Table::Prefix, "U", "app.setup"),
    (Table::Prefix, "^", "app.update"),
    (Table::Prefix, "C", "config.panel"),
    (Table::Prefix, "P", "project.add"),
    (Table::Prefix, "v", "pane.split_right"),
    (Table::Prefix, "s", "pane.split_down"),
    (Table::Prefix, "c", "pane.close"),
    (Table::Prefix, "C-c", "shells.new"),
    (Table::Prefix, "e", "shells.menu"),
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
    (Table::Nav, "j", "nav.down"),
    (Table::Nav, "Down", "nav.down"),
    (Table::Nav, "k", "nav.up"),
    (Table::Nav, "Up", "nav.up"),
    (Table::Nav, "g", "nav.top"),
    (Table::Nav, "G", "nav.bottom"),
    (Table::Nav, "Enter", "nav.open"),
    (Table::Nav, "Space", "nav.select"),
    (Table::Nav, "b", "broadcast"),
    (Table::Nav, "m", "chat.new"),
    (Table::Nav, "*", "chat.pin"),
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
    (Table::Nav, "C-n", "session.again"),
    (Table::Nav, "x", "session.stop"),
    (Table::Nav, "X", "session.kill"),
    (Table::Nav, "R", "session.restart"),
    (Table::Nav, ",", "session.rename"),
    (Table::Nav, "/", "session.find"),
    (Table::Nav, "a", "attention.next"),
    (Table::Nav, "p", "peek"),
    (Table::Nav, "e", "session.explain"),
    (Table::Nav, "?", "help"),
    (Table::Nav, ":", "palette"),
    (Table::Nav, "\\", "sidebar.toggle"),
    (Table::Nav, "z", "sidebar.zoom"),
    (Table::Nav, "Z", "sidebar.fold_all"),
    (Table::Nav, "f", "sidebar.filter"),
    (Table::Nav, "u", "toast.act"),
    (Table::Nav, ".", "row.menu"),
    (Table::Nav, "!", "shell.new"),
    (Table::Nav, "C-t", "shell.drawer"),
    (Table::Nav, "c", "shells.new"),
    (Table::Nav, "T", "shell.pane"),
    (Table::Nav, "H", "home"),
    (Table::Nav, "M", "health"),
    (Table::Nav, "I", "app.about"),
    (Table::Nav, "L", "kb.open"),
    (Table::Nav, "U", "app.setup"),
    (Table::Nav, "^", "app.update"),
    (Table::Nav, "C", "config.panel"),
    (Table::Nav, "P", "project.add"),
    (Table::Nav, ">", "sidebar.wider"),
    (Table::Nav, "<", "sidebar.narrower"),
    (Table::Nav, "v", "pane.split_right"),
    (Table::Nav, "s", "pane.split_down"),
    // In a chat, before its composer takes the key; a key whose action doesn't apply (a
    // permission letter with no card) is typed instead.
    (Table::Chat, "C-f", "chat.find_in"),
    (Table::Chat, "C-o", "chat.agent"),
    (Table::Chat, "Tab", "chat.incognito"),
    (Table::Chat, "C-i", "chat.incognito"),
    (Table::Chat, "a", "chat.allow"),
    (Table::Chat, "A", "chat.always"),
    (Table::Chat, "d", "chat.deny"),
    (Table::Chat, "C-c", "chat.stop"),
    (Table::Chat, "PageUp", "chat.page_up"),
    (Table::Chat, "PageDown", "chat.page_down"),
    (Table::ChatList, "/", "chat.list_search"),
    (Table::ChatList, "m", "chat.new"),
    (Table::ChatList, "n", "chat.new"),
    (Table::ChatList, "*", "chat.pin"),
    (Table::ChatList, "x", "chat.archive"),
    (Table::ChatList, "X", "chat.delete"),
    (Table::ChatList, ",", "chat.rename"),
    (Table::ChatList, "e", "chat.export"),
    (Table::ChatList, "A", "chat.list_archived"),
    (Table::ChatList, "s", "chat.list_state"),
    (Table::ChatList, "Tab", "chat.list_all"),
    (Table::ChatList, ".", "row.menu"),
];

/// The default bindings named as `[keys]` names them (`d`, `nav:j`), for the settings panel and
/// `ptyro config list`: what a key does before any override.
pub fn default_keys() -> Vec<(String, String)> {
    let named = |table: Table, k: &str| format!("{}{k}", table.tag());
    let mut out: Vec<(String, String)> =
        DEFAULTS.iter().map(|(t, k, a)| (named(*t, k), (*a).to_owned())).collect();
    for d in 1..=9 {
        for t in [Table::Prefix, Table::Nav] {
            out.push((named(t, &d.to_string()), format!("session.focus {d}")));
        }
    }
    for (k, n) in shell_digits() {
        out.push((named(Table::Global, &k), format!("shells.go {n}")));
    }
    out
}

/// `M-1`…`M-9` and `M-0`: shells 1–10 of SHELLS (SPEC §23.6), with no prefix.
pub fn shell_digits() -> Vec<(String, u8)> {
    (1..=10u8).map(|n| (format!("M-{}", n % 10), n)).collect()
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
        for k in map.prefixes.clone() {
            map.bindings
                .entry(slot(Table::Prefix, &k))
                .or_insert((k, Command { action: "focus.toggle", arg: None }));
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
        for (k, n) in shell_digits().into_iter().filter(|_| config.shell.alt_digits) {
            let k = key(&k);
            map.bindings.insert(
                slot(Table::Global, &k),
                (k, Command { action: "shells.go", arg: Some(n.to_string()) }),
            );
        }
        for (spec_key, value) in &config.keys {
            let (table, key_spec) = [Table::Global, Table::Nav, Table::Chat, Table::ChatList]
                .into_iter()
                .find_map(|t| spec_key.strip_prefix(t.tag()).map(|s| (t, s)))
                .unwrap_or((Table::Prefix, spec_key.as_str()));
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
            match bound_action(name) {
                Some(s) => {
                    if s.arg == Arg::Number && arg.as_deref().and_then(|a| a.parse::<usize>().ok()).is_none() {
                        errors.push(format!("keys.{spec_key:?}: {name} needs a number, e.g. \"{name} 1\""));
                        continue;
                    }
                    map.bindings.insert(slot(table, &k), (k, Command { action: s.id, arg }));
                }
                None => errors.push(format!(
                    "keys.{spec_key:?}: unknown action {name:?}: ptyro no longer has it, or it is misspelt: `ptyro config unset {}` removes the line (`ptyro doctor` finds every such key; `{} ?` or the palette lists the actions)",
                    key_setting(spec_key),
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
            // `M-1`…`M-0` → `shells.go 1`…`10`: one `M-1…9` line, the defaults only.
            if c.action == "shells.go"
                && shell_digits()
                    .iter()
                    .any(|(k, n)| k == label && c.arg.as_deref() == Some(n.to_string().as_str()))
            {
                if c.arg.as_deref() == Some("1") {
                    out.push((
                        *t,
                        "M-1…9".into(),
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
    /// (`j`) over a named key (`Down`) or a chord (`C-p`), then by name; a prefix key last, since
    /// `C-q C-q` reads worse in a hint than `C-q Tab`.
    fn keys_of(&self, table: Table, action: &str) -> Vec<String> {
        let prefixes: Vec<String> = self.prefixes.iter().map(key_label).collect();
        let mut keys: Vec<String> = self
            .bindings
            .iter()
            .filter(|((t, _), (_, c))| *t == table && c.action == action && c.arg.is_none())
            .map(|((_, label), _)| label.clone())
            .collect();
        let rank = |k: &String| (prefixes.contains(k), k.chars().count() > 1, k.len());
        keys.sort_by(|a, b| (rank(a), a).cmp(&(rank(b), b)));
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

/// The `[keys]` lines that bind an action ptyro doesn't have (one a release removed, or a
/// misspelling): each key as the file names it, with that action. `ptyro doctor` reports them.
pub fn stale_bindings(config: &Config) -> Vec<(String, String)> {
    config
        .keys
        .iter()
        .filter_map(|(k, v)| {
            let name = v.split_whitespace().next()?;
            (name != "none" && bound_action(name).is_none()).then(|| (k.clone(), name.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::input::{KeyCode, Mods};

    fn ev(c: char, mods: u8) -> KeyEvent {
        KeyEvent::new(KeyCode::Char { c }, Mods(mods))
    }

    /// `[keys]` lines bound to actions the TUI doesn't have (a release removed them, or a typo)
    /// are found for `ptyro doctor`, and the TUI's message names the line that removes each. One
    /// that names an action by the name it had before a rename still runs it.
    #[test]
    fn keys_bound_to_actions_that_are_gone_are_found() {
        let c = Config::parse(
            "[keys]\n\"S\" = \"sets\"\n\"nav:j\" = \"nav.down\"\n\"x\" = \"none\"\n\"T\" = \"help.tour\"\n\"Y\" = \"theme.next\"\n\"W\" = \"remote.serve\"\n\"C-.\" = \"app.backup\"\n\"chat:e\" = \"chat.retitle\"\n\"nav:E\" = \"worktree.retitle\"\n",
        )
        .unwrap();
        let names: Vec<String> = stale_bindings(&c).into_iter().map(|(_, a)| a).collect();
        assert_eq!(names, ["app.backup", "sets", "help.tour", "remote.serve", "theme.next"]);
        let errors = validate(&c);
        assert_eq!(errors.len(), 5, "{errors:?}");
        assert!(
            errors.iter().all(|e| e.contains("`ptyro doctor` finds every such key")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("`ptyro config unset keys.C-.` removes the line")),
            "{errors:?}"
        );
        let (map, _) = Keymap::new(&c);
        let renamed = map.lookup(Table::Chat, key("e")).map(|c| c.action);
        assert_eq!(renamed, Some("chat.rename"), "an old name still runs its action");
        // The fix as a command line, and back: a name the shell would split is quoted.
        assert_eq!(key_setting("nav:j"), "keys.nav:j");
        assert_eq!(key_setting("C- "), "'keys.C- '");
        assert_eq!(key_setting("'"), "\"keys.'\"");
        for name in ["C-.", "C- ", "'", "\"", "nav:j"] {
            let line = format!("config unset {}", key_setting(name));
            assert_eq!(shell_words(&line), ["config", "unset", &format!("keys.{name}")], "{line}");
        }
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
    fn a_prefix_twice_is_the_sidebar_and_q_sends_the_prefix() {
        let (m, _) = Keymap::new(&Config::default());
        for k in [ev('q', Mods::CTRL), ev(' ', Mods::CTRL), KeyEvent::new(KeyCode::Tab, Mods(0))] {
            assert_eq!(m.lookup(Table::Prefix, k).unwrap().action, "focus.toggle", "{k:?}");
        }
        assert_eq!(m.lookup(Table::Prefix, ev('q', 0)).unwrap().action, "prefix.send");
        assert_eq!(m.key_for("app.detach").as_deref(), Some("C-q d"), "d alone detaches");
        // Hints show `C-q Tab`, not `C-q C-q`.
        assert_eq!(m.key_for("focus.toggle").as_deref(), Some("C-q Tab"));
        // A prefix you set does it too, and `[keys]` can give it back to the program.
        let c = Config::parse("[ui]\nprefix = \"C-]\"\n").unwrap();
        let (m, _) = Keymap::new(&c);
        assert_eq!(m.lookup(Table::Prefix, ev(']', Mods::CTRL)).unwrap().action, "focus.toggle");
        let c = Config::parse("[keys]\n\"C-q\" = \"prefix.send\"\n").unwrap();
        let (m, errors) = Keymap::new(&c);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(m.lookup(Table::Prefix, ev('q', Mods::CTRL)).unwrap().action, "prefix.send");
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
            actions: ACTIONS.iter().map(|a| (a.id.to_owned(), a.desc.to_owned())).collect(),
            keys: default_keys(),
        };
        let (m, _) = Keymap::new(&Config::default());
        let rows = settings::bindings(&serde_json::Value::Null, &lk);
        // The nine digits are one row per table; everything else is a row of its own.
        // The nine digits are one row per table, the ten Alt digits one more.
        assert_eq!(rows.len(), DEFAULTS.len() + 3, "one row per default key");
        for b in rows.iter().filter(|b| !b.fixed) {
            let table = match b.table {
                "nav" => Table::Nav,
                "global" => Table::Global,
                "chat" => Table::Chat,
                "chatlist" => Table::ChatList,
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
    fn a_chat_and_the_chat_list_have_tables_of_their_own() {
        let (m, _) = Keymap::new(&Config::default());
        assert_eq!(m.lookup(Table::Chat, ev('f', Mods::CTRL)).unwrap().action, "chat.find_in");
        assert_eq!(m.lookup(Table::ChatList, ev('x', 0)).unwrap().action, "chat.archive");
        assert!(m.lookup(Table::Prefix, ev('f', Mods::CTRL)).is_none(), "nothing leaks out");
        // Parsed: the file's check reads past the table's name.
        let c = Config::parse("[keys]\n\"chat:C-f\" = \"none\"\n\"chatlist:z\" = \"chat.pin\"\n")
            .unwrap();
        let (m, errors) = Keymap::new(&c);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(m.lookup(Table::Chat, ev('f', Mods::CTRL)).is_none());
        assert_eq!(m.lookup(Table::ChatList, ev('z', 0)).unwrap().action, "chat.pin");
        assert!(default_keys().iter().any(|(k, a)| k == "chatlist:x" && a == "chat.archive"));
    }

    #[test]
    fn every_default_names_a_real_action() {
        for (_, _, action) in DEFAULTS {
            assert!(spec(action).is_some(), "{action}");
        }
    }
}
