//! The settings schema (ROADMAP CFG1.0, SPEC §13.7): one description for every configuration
//! key, which the CLI (`ptyro config get|set|unset|list`), the TUI's panel, the web's panel, the
//! commented starter file and the docs all read.
//!
//! A [`Setting`] is a key (`ui.theme`, or `agents.*.command` for what every agent has), the panel
//! section it sits in, its [`Kind`] (what can be typed, and so what the panel shows to edit it),
//! one line of help, and when a change takes effect. Defaults aren't repeated here: they come from
//! `Config::default()`, so they can't drift. A test walks the config structs and fails when a
//! field has no entry (a new option can't skip the panel) or an entry has no field.
//!
//! Edits are text to text with `toml_edit` ([`set`], [`unset`], [`add`], [`remove`], [`rename`]):
//! the user's comments, ordering and spelling stay, and the result is checked with the same
//! parser the daemon loads with before anyone writes it.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::project::ProjectConfig;

/// Which file family a setting lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// `~/.config/ptyro/config.toml` (agents, profiles and the like can only be here).
    User,
    /// `<repo>/.ptyro.toml` (shared) and `<repo>/.ptyro.local.toml` (yours).
    Project,
}

/// When a change shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// At once: the daemon and every open TUI reload the file when it changes.
    Live,
    /// For sessions started (or screens attached) from now on; running ones keep theirs.
    Next,
    /// When the daemon starts again.
    Restart,
}

impl Apply {
    pub fn says(self) -> &'static str {
        match self {
            Self::Live => "applies at once",
            Self::Next => "applies to new sessions",
            Self::Restart => "needs a daemon restart",
        }
    }
}

/// A fixed choice, and what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub value: &'static str,
    pub help: &'static str,
}

const fn c(value: &'static str, help: &'static str) -> Choice {
    Choice { value, help }
}

/// Things a setting can pick from that are found at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Themes,
    /// The themes built in: what a theme of yours can start from.
    BuiltinThemes,
    Agents,
    Editors,
    /// Every TUI action (`session.next`…), for key bindings.
    Actions,
    /// The project's run scripts.
    RunScripts,
}

/// What a setting holds, and so how it's typed and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    /// One line of text.
    Text,
    /// A command or other text that may run over several lines in the file (shown on one).
    Script,
    Path,
    Choice {
        choices: &'static [Choice],
    },
    /// One of what exists: themes, agents, editors, actions.
    Dynamic {
        source: Source,
    },
    /// Several of a fixed few.
    Set {
        choices: &'static [Choice],
    },
    /// A list of lines of text.
    List,
    /// A list of key names (`C-q`, `M-1`).
    Keys,
    /// A program and its arguments.
    Argv,
    /// `30s`, `10m`, `2h`, `1d`, or `off`.
    Duration,
    /// `16MiB`, `512KiB`, `2GB`.
    Bytes,
    /// `22:00-07:00`.
    Window,
    /// `#rrggbb`, a 0–255 palette index, or a name.
    Color,
    /// `host:port`.
    Address,
    /// A token: written as it is, or `!program args` to run a program that prints it.
    Secret,
    /// Structured (a table of its own): edited in the file, not field by field.
    Opaque,
}

impl Kind {
    /// What it is, in a word, for lists (`yes/no`, `number`, `one of`…).
    pub fn word(&self) -> &'static str {
        match self {
            Self::Bool => "yes/no",
            Self::Int { .. } => "number",
            Self::Text | Self::Script => "text",
            Self::Path => "path",
            Self::Choice { .. } | Self::Dynamic { .. } => "one of",
            Self::Set { .. } => "several of",
            Self::List | Self::Keys => "list",
            Self::Argv => "command",
            Self::Duration => "duration",
            Self::Bytes => "size",
            Self::Window => "hours",
            Self::Color => "colour",
            Self::Address => "address",
            Self::Secret => "secret",
            Self::Opaque => "table",
        }
    }
}

/// A setting that only means something when a sibling has one of some values (a notification
/// sink's `url` is for `ntfy`; its `webhook` is for `slack` and `discord`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct When {
    pub key: &'static str,
    pub any: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Setting {
    pub key: &'static str,
    pub section: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    pub apply: Apply,
    /// Rarely wanted: the panel hides it until asked.
    pub advanced: bool,
    pub when: Option<When>,
    /// What to show when the setting is empty and its default is "nothing": where the value
    /// then comes from.
    pub otherwise: &'static str,
    /// A value to show when there is no default to show (the starter file, `config list`).
    pub example: &'static str,
    /// What a number counts (`columns`, `days`), shown beside it in the panel.
    pub unit: &'static str,
    /// Other words for it that a search should find ("notify", "sound").
    pub also: &'static str,
}

const fn s(key: &'static str, section: &'static str, kind: Kind, help: &'static str) -> Setting {
    Setting {
        key,
        section,
        kind,
        help,
        apply: Apply::Live,
        advanced: false,
        when: None,
        otherwise: "",
        example: "",
        unit: "",
        also: match kind {
            Kind::Color => "color colour",
            _ => "",
        },
    }
}

impl Setting {
    /// Applies to sessions started from now on.
    const fn new_only(mut self) -> Self {
        self.apply = Apply::Next;
        self
    }
    const fn restart(mut self) -> Self {
        self.apply = Apply::Restart;
        self
    }
    const fn adv(mut self) -> Self {
        self.advanced = true;
        self
    }
    const fn when(mut self, key: &'static str, any: &'static [&'static str]) -> Self {
        self.when = Some(When { key, any });
        self
    }
    const fn eg(mut self, example: &'static str) -> Self {
        self.example = example;
        self
    }
    const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }
    const fn also(mut self, also: &'static str) -> Self {
        self.also = also;
        self
    }
    const fn or(mut self, otherwise: &'static str) -> Self {
        self.otherwise = otherwise;
        self
    }

    /// The setting's key as a list of segments.
    pub fn segments(&self) -> Vec<&'static str> {
        self.key.split('.').collect()
    }
}

/// A group in the panel.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub blurb: &'static str,
    pub scope: Scope,
}

pub const SECTIONS: &[Section] = &[
    Section {
        id: "interface",
        title: "Interface",
        blurb: "How the terminal screen looks and behaves.",
        scope: Scope::User,
    },
    Section {
        id: "keys",
        title: "Key bindings",
        blurb: "Every key: see what it does, change it, unbind it or add your own.",
        scope: Scope::User,
    },
    Section {
        id: "themes",
        title: "Themes",
        blurb: "Pick the colour scheme, or make your own.",
        scope: Scope::User,
    },
    Section {
        id: "agents",
        title: "Agents",
        blurb: "The programs ptyro starts, and how.",
        scope: Scope::User,
    },
    Section {
        id: "profiles",
        title: "Profiles",
        blurb: "Accounts and identities, one per context.",
        scope: Scope::User,
    },
    Section {
        id: "attention",
        title: "Attention",
        blurb: "When a session needs you, and where you hear about it.",
        scope: Scope::User,
    },
    Section {
        id: "notifications",
        title: "Notifications",
        blurb: "How to reach you away from the screen.",
        scope: Scope::User,
    },
    Section {
        id: "worktrees",
        title: "Worktrees and ports",
        blurb: "Where new work goes and how it is named.",
        scope: Scope::User,
    },
    Section {
        id: "github",
        title: "GitHub and review",
        blurb: "Pull requests, checks and the review queue.",
        scope: Scope::User,
    },
    Section {
        id: "orchestration",
        title: "Orchestration",
        blurb: "Several agents at once: save points, file locks, overlap warnings and messages between them.",
        scope: Scope::User,
    },
    Section {
        id: "summaries",
        title: "Summaries",
        blurb: "A line about what each turn did, from a small model.",
        scope: Scope::User,
    },
    Section {
        id: "scratchpad",
        title: "Scratchpad and editors",
        blurb: "The notes file, and the editor that opens things.",
        scope: Scope::User,
    },
    Section {
        id: "daemon",
        title: "Daemon and recording",
        blurb: "The background service, scrollback and recordings.",
        scope: Scope::User,
    },
    Section {
        id: "remote",
        title: "Web app and remote",
        blurb: "Reaching ptyro from a phone or another computer.",
        scope: Scope::User,
    },
    Section {
        id: "project",
        title: "This project",
        blurb: "Set up for one repository.",
        scope: Scope::Project,
    },
    Section {
        id: "scripts",
        title: "Project scripts",
        blurb: "What runs when work starts, finishes and in between.",
        scope: Scope::Project,
    },
    Section {
        id: "prompts",
        title: "Project prompts",
        blurb: "Snippets for new sessions.",
        scope: Scope::Project,
    },
];

pub fn section(id: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.id == id)
}

/// A group of named things made by the person: agents, profiles, themes… The settings of each
/// are the entries whose key starts `<path>.*.`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Collection {
    pub path: &'static str,
    pub section: &'static str,
    /// "agent", "profile": what one is called.
    pub noun: &'static str,
    pub layout: Layout,
    /// A name already in use by the table itself (`editors.default`).
    pub reserved: &'static [&'static str],
    /// Settings written when one is added (a command to run, a type…), so it is valid at once.
    pub starter: &'static [(&'static str, &'static str)],
    /// The heading over its list.
    pub title: &'static str,
    /// What to say while there are none: what they are for, and what already works without one.
    pub empty: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// `[path.<name>]` with the fields as keys.
    Tables,
    /// `path.<name> = value`: the name is the key (key bindings, prompts).
    Map(Kind),
    /// `[[path]]`, one table each, named by position.
    Array,
}

pub const COLLECTIONS: &[Collection] = &[
    Collection {
        path: "keys",
        section: "keys",
        noun: "binding",
        layout: Layout::Map(Kind::Dynamic { source: Source::Actions }),
        reserved: &[],
        starter: &[],
        title: "Key bindings",
        empty: "No key is bound. Add one to make a shortcut.",
    },
    Collection {
        path: "themes",
        section: "themes",
        noun: "theme",
        layout: Layout::Tables,
        reserved: &["ptyro-dark", "ptyro-light", "high-contrast"],
        starter: &[("base", "ptyro-dark")],
        title: "Your themes",
        empty: "None yet. ptyro-dark, ptyro-light and high-contrast are built in; add one to make colours of your own.",
    },
    Collection {
        path: "agents",
        section: "agents",
        noun: "agent",
        layout: Layout::Tables,
        reserved: &[],
        starter: &[],
        title: "Your agents",
        empty: "None added. claude, codex, gemini and opencode work as they are: add one only to change how an agent starts, or to run another program.",
    },
    Collection {
        path: "profiles",
        section: "profiles",
        noun: "profile",
        layout: Layout::Tables,
        reserved: &[],
        starter: &[],
        title: "Profiles",
        empty: "None. One account needs no profile; add one to keep another account's logins and git identity apart.",
    },
    Collection {
        path: "notifications.sinks",
        section: "notifications",
        noun: "sink",
        layout: Layout::Array,
        reserved: &[],
        starter: &[],
        title: "Notification services",
        empty: "None. Add one (ntfy, Slack, Pushover…) to be told when you are away from this screen.",
    },
    Collection {
        path: "editors",
        section: "scratchpad",
        noun: "editor",
        layout: Layout::Tables,
        reserved: &["default"],
        starter: &[],
        title: "Your editors",
        empty: "None added. nvim, vim, hx, code, zed and cursor are built in (whichever is installed); add one to use another.",
    },
    Collection {
        path: "scripts.run",
        section: "scripts",
        noun: "run script",
        layout: Layout::Tables,
        reserved: &[],
        starter: &[],
        title: "Run scripts",
        empty: "None. A run script is a named command, like a dev server or the tests, that you start from a worktree.",
    },
    Collection {
        path: "prompts",
        section: "prompts",
        noun: "prompt",
        layout: Layout::Map(Kind::Script),
        reserved: &[],
        starter: &[],
        title: "Prompts",
        empty: "None. A prompt is saved text a new session can start from.",
    },
];

pub fn collection(path: &str) -> Option<&'static Collection> {
    COLLECTIONS.iter().find(|c| c.path == path)
}

// ── the table ─────────────────────────────────────────────────────────────────────────────

const KINDS: &[Choice] = &[
    c("blocked", "an agent is waiting on a permission or a question"),
    c("errored", "an agent hit an error"),
    c("done", "an agent finished its turn"),
    c("idle", "an agent has been working and stopped"),
    c("exited", "a session's program ended"),
    c("notification", "a program sent its own notification"),
    c("gate", "a run waits for your go-ahead"),
    c("run", "a run ended, or a message for you arrived"),
];
const BREAKTHROUGH: &[Choice] = KINDS;
const CHANNELS: &[Choice] = &[
    c("inbox", "the inbox (always on)"),
    c("bell", "the terminal bell"),
    c("osc", "your terminal's own notification (OSC 9/777)"),
    c("desktop", "a desktop notification"),
    c("sinks", "the services below (ntfy, Slack…)"),
];
const ON_STOP: &[Choice] = &[
    c("next", "show the nearest other session"),
    c("stay", "keep the stopped session's last screen"),
];
const DETAIL: &[Choice] = &[
    c("auto", "every row while they fit, else the ones that matter"),
    c("compact", "one line per row"),
    c("full", "every row, always"),
];
const SHELL_STYLE: &[Choice] = &[
    c(
        "auto",
        "a drawer under a pane of the same project when one is open, else a panel of its own",
    ),
    c("drawer", "always a drawer under the focused pane"),
    c("pane", "always a full-sized panel of its own (the default; C-t asks for the drawer)"),
];
const LOG_LEVEL: &[Choice] = &[
    c("error", "problems only"),
    c("warn", "problems and warnings"),
    c("info", "what the daemon does (the default)"),
    c("debug", "more detail, for a bug report"),
    c("trace", "everything"),
];
const RESTORE: &[Choice] = &[
    c("dormant", "listed, and resumed when you open them"),
    c("resume", "resumed right away, three at a time"),
    c("none", "not restored"),
];
const SCOPE: &[Choice] = &[
    c("current-first", "this workspace first, then the others"),
    c("current", "this workspace only"),
    c("global", "everywhere, most urgent first"),
];
const LOCATION: &[Choice] = &[
    c("data", "in ptyro's data directory"),
    c("sibling", "next to the repository (<repo>.worktrees/)"),
    c("in-repo", "inside it (.ptyro/worktrees/, kept out of git)"),
];
const START_AGENTS: &[Choice] = &[
    c("after_setup", "once the setup script has run"),
    c("immediately", "straight away, while setup runs"),
];
const MERGE: &[Choice] =
    &[c("squash", "one commit"), c("merge", "a merge commit"), c("rebase", "rebase onto the base")];
const PAD_MODE: &[Choice] = &[
    c("drawer", "below the pane, like the quick terminal"),
    c("popup", "a window over everything"),
    c("pane", "a pane of its own beside the others"),
];
const ADAPTERS: &[Choice] = &[
    c("claude", "Claude Code"),
    c("codex", "Codex"),
    c("gemini", "Gemini CLI"),
    c("opencode", "OpenCode"),
    c("antigravity", "Antigravity"),
    c("generic", "any other program"),
];
const PROTOCOLS: &[Choice] = &[
    c("stream-json", "Claude's stream-json"),
    c("app-server", "Codex's app server"),
    c("acp", "the Agent Client Protocol (any agent that speaks it)"),
    c("opencode", "OpenCode's server"),
];
const GUARD: &[Choice] = &[
    c("confirm", "ask before using it on another profile's repository"),
    c("refuse", "never"),
    c("warn", "go ahead and say so"),
];
const SINK_TYPES: &[Choice] = &[
    c("ntfy", "ntfy.sh or your own ntfy"),
    c("pushover", "Pushover"),
    c("slack", "a Slack webhook"),
    c("discord", "a Discord webhook"),
    c("telegram", "a Telegram bot"),
    c("command", "any program, given the notification as JSON on stdin"),
];
const SHARE_MODE: &[Choice] = &[
    c("clone", "copy-on-write clone (a symlink where the file system can't)"),
    c("symlink", "a symlink"),
    c("copy", "clone where possible, else a full copy"),
];
const ACCESS: &[Choice] = &[
    c("localhost", "this computer only: nothing else can reach the web app"),
    c(
        "tailscale",
        "this computer and your Tailscale network (phones and laptops signed in to it), not the Wi-Fi",
    ),
    c("open", "every network interface: the Wi-Fi or LAN too. Devices still need a pairing token"),
];
const RUN_MODE: &[Choice] = &[
    c("concurrent", "run scripts of every worktree can run together"),
    c("exclusive", "starting one stops the other worktrees' run scripts"),
];

const fn dynamic(source: Source) -> Kind {
    Kind::Dynamic { source }
}
const fn int(min: i64, max: i64) -> Kind {
    Kind::Int { min, max }
}
const fn pick(choices: &'static [Choice]) -> Kind {
    Kind::Choice { choices }
}
const fn some(choices: &'static [Choice]) -> Kind {
    Kind::Set { choices }
}

const SINK_URL: &[&str] = &["ntfy"];
const SINK_TOKEN: &[&str] = &["ntfy", "pushover", "telegram"];
const SINK_HOOK: &[&str] = &["slack", "discord"];

pub const SETTINGS: &[Setting] = &[
    // ── Interface ──
    s("ui.theme", "themes", dynamic(Source::Themes), "The terminal screen's colour scheme. Make your own below.").also("color colour dark light appearance"),
    s("ui.prefix", "interface", Kind::Keys, "The keys that start a ptyro command (then a key; `?` lists them all). Any of them works; the first is the one hints show.").also("shortcut hotkey leader modifier keybinding"),
    s("ui.sidebar", "interface", Kind::Bool, "Show the session list down the side (`\\` toggles it)."),
    s("ui.sidebar_width", "interface", int(12, 120), "How wide the sidebar is.").unit("columns"),
    s("ui.sidebar_detail", "interface", pick(DETAIL), "How much each session row says under its first line (its task, what it is on, its last reply)."),
    s("ui.status_bar", "interface", Kind::Bool, "Show the status bar along the bottom."),
    s("ui.clock", "interface", Kind::Bool, "Show the time in the status bar."),
    s("ui.mouse", "interface", Kind::Bool, "Let ptyro take the mouse: click to focus, wheel to scroll, drag borders. Off leaves it to your terminal, so you can select and copy text with it.").also("click scroll select copy"),
    s("ui.confirm_kill", "interface", Kind::Bool, "Ask before stopping a session."),
    s("ui.show_ports", "interface", Kind::Bool, "Show the ports a session listens on, in the sidebar and pane headers."),
    s("ui.tint_borders", "interface", Kind::Bool, "Tint pane borders with the session's profile colour."),
    s("ui.on_stop", "interface", pick(ON_STOP), "What a pane shows after you stop its session."),
    s("ui.which_key_delay", "interface", int(0, 5000), "How long after the prefix key the menu of next keys waits before it pops up. 0 never shows it.").unit("ms").also("hints popup help menu"),
    s("ui.shell_style", "interface", pick(SHELL_STYLE), "How the shell key (`!`) shows a worktree's terminal: as a full-sized panel of its own (the default), or as a drawer under its project's pane. `C-t` always asks for the drawer.").also("terminal split drawer pane panel window shell"),
    s("clipboard.allow_write", "interface", Kind::Bool, "Let programs in sessions copy to your clipboard (OSC 52). Reads are always refused.").also("copy paste"),
    // ── Attention ──
    s("attention.scope", "attention", pick(SCOPE), "Where the next-attention key may go."),
    s("attention.breakthrough", "attention", some(BREAKTHROUGH), "Kinds that always notify you, even in a workspace you muted to a badge."),
    s("attention.stale_after", "attention", Kind::Duration, "If the agent's own hooks go quiet this long, fall back to guessing its status from the screen.").adv(),
    s("attention.min_work", "attention", Kind::Duration, "An agent that stops after working at least this long counts as needing you. Shorter stops are ignored.").adv(),
    s("attention.resume_on_focus", "attention", Kind::Bool, "Opening a dormant session resumes it."),
    // ── Notifications ──
    s("notifications.channels", "notifications", some(CHANNELS), "How ptyro tells you.").also("notify alert sound beep ping bell"),
    s("notifications.on", "notifications", some(KINDS), "Which things notify you.").also("notify alert"),
    s("notifications.quiet_hours", "notifications", Kind::Window, "Between these hours only the inbox is used, like 22:00-07:00 (kinds you set to always notify still do).").or("never quiet").eg("22:00-07:00"),
    s("notifications.coalesce", "notifications", Kind::Duration, "At most one notification per session in this time."),
    s("notifications.desktop_command", "notifications", Kind::Argv, "Your own desktop notifier, as a command. {title} and {body} are replaced.").adv().or("the system's").eg("my-notifier --title {title} {body}"),
    s("notifications.callback.listen", "notifications", Kind::Address, "Where approve/deny links in notifications are answered, like 127.0.0.1:7591. Off unless set.").restart().or("off").eg("127.0.0.1:7591"),
    s("notifications.callback.insecure", "notifications", Kind::Bool, "Allow that address when it isn't loopback or Tailscale. The links are plain HTTP.").restart().adv(),
    s("notifications.callback.public_url", "notifications", Kind::Text, "The address the links point at, if not the one it listens on.").restart().adv().or("http://<listen>"),
    s("notifications.callback.link_ttl", "notifications", Kind::Duration, "How long an approve/deny link stays valid."),
    s("notifications.sinks.*.type", "notifications", pick(SINK_TYPES), "Where it goes."),
    s("notifications.sinks.*.url", "notifications", Kind::Text, "The topic's address, like https://ntfy.sh/my-topic.").when("type", SINK_URL),
    s("notifications.sinks.*.token", "notifications", Kind::Secret, "The access token, or `!program args` to run one that prints it.").when("type", SINK_TOKEN).or("none"),
    s("notifications.sinks.*.user", "notifications", Kind::Secret, "Your Pushover user key.").when("type", &["pushover"]),
    s("notifications.sinks.*.webhook", "notifications", Kind::Secret, "The webhook's address.").when("type", SINK_HOOK),
    s("notifications.sinks.*.chat_id", "notifications", Kind::Text, "The chat to message.").when("type", &["telegram"]),
    s("notifications.sinks.*.command", "notifications", Kind::Argv, "The program to run.").when("type", &["command"]),
    s("notifications.sinks.*.on", "notifications", some(KINDS), "Which kinds go here. None chosen: blocked and errored."),
    // ── Worktrees and ports ──
    s("worktrees.location", "worktrees", pick(LOCATION), "Where new worktrees are made."),
    s("worktrees.branch_template", "worktrees", Kind::Text, "How a new branch is named: {name}, {user}, {date}, {issue}, {slug}."),
    s("worktrees.start_agents", "worktrees", pick(START_AGENTS), "When agents asked for with a new worktree start."),
    s("worktrees.fetch", "worktrees", Kind::Bool, "Fetch the latest base branch before making a worktree, so it starts up to date."),
    s("worktrees.watch", "worktrees", Kind::Bool, "Refresh a repository's status when its files change, not only every few seconds.").adv(),
    s("worktrees.stale_days", "worktrees", int(1, 3650), "`ptyro gc` reports a worktree with no sessions after this long.").unit("days"),
    s("worktrees.auto_rename_branch", "worktrees", Kind::Bool, "Name the branch after the first prompt, until it is pushed."),
    s("worktrees.archive_on_merge", "worktrees", Kind::Bool, "Archive a worktree when its pull request merges and nothing runs in it."),
    s("ports.range", "worktrees", Kind::Text, "The ports handed out to worktrees, LOW-HIGH (LOW at least 1024).").new_only(),
    s("ports.count", "worktrees", int(1, 1000), "Ports for each worktree.").new_only().unit("ports"),
    s("discover.roots", "worktrees", Kind::List, "Folders to look in for git repositories.").or("none"),
    s("discover.depth", "worktrees", int(1, 10), "How deep to look in them.").unit("folders deep"),
    s("discover.auto_register", "worktrees", Kind::Bool, "Make the repository a session runs in a project by itself."),
    s("compat.conductor_env", "worktrees", Kind::Bool, "Also set CONDUCTOR_* variables, so Conductor's setup scripts run unchanged.").adv().new_only(),
    // ── GitHub and review ──
    s("github.poll", "github", Kind::Duration, "How often to ask GitHub (through `gh`) about a worktree's pull request, never more than once a minute for one.").adv(),
    s("github.draft", "github", Kind::Bool, "New pull requests are drafts."),
    s("github.merge_method", "github", pick(MERGE), "How `ptyro pr automerge` merges."),
    s("github.watch", "github", Kind::Bool, "Tell an agent about new CI failures and review comments on the pull requests ptyro opened."),
    s("github.escalate_after", "github", Kind::Duration, "Ask for your attention when a pull request stays failing or unanswered this long."),
    s("github.log_lines", "github", int(1, 2000), "Lines of a failing check's log sent to the agent.").adv().unit("lines"),
    s("review.max_unreviewed", "github", int(0, 100), "Hold new agent sessions while this many worktrees are waiting for your review. 0 never holds.").unit("worktrees"),
    s("review.digest", "github", Kind::Duration, "A digest of what is ready for review, at most this often. `off` for never."),
    // ── Orchestration ──
    s("orchestration.checkpoints", "orchestration", Kind::Bool, "Save a restore point (a checkpoint) after every agent turn that changed its worktree."),
    s("orchestration.lease_ttl", "orchestration", Kind::Duration, "How long an agent's claim on a file lasts unless it renews it.").adv(),
    s("orchestration.radar", "orchestration", Kind::Bool, "Watch a project's worktrees for agents changing the same files, and tell both."),
    s("orchestration.message_ttl", "orchestration", Kind::Duration, "A message between agents is dropped if it was not picked up in this time.").adv(),
    // ── Summaries ──
    s("summaries.enabled", "summaries", Kind::Bool, "A one-line `≈` summary after each agent turn, written by a small model on the session's own account. It sends a trimmed, redacted view of the session to the model's provider."),
    s("summaries.daily_cap", "summaries", int(0, 100_000), "The most summaries made for each profile in a day, to cap the cost (about $0.0007 each with Haiku). 0 makes none.").unit("a day"),
    s("summaries.command", "summaries", Kind::Argv, "The summarizer: reads the prompt on stdin, prints the summary.").adv(),
    // ── Scratchpad and editors ──
    s("scratchpad.mode", "scratchpad", pick(PAD_MODE), "How the scratchpad key shows it."),
    s("scratchpad.editor", "scratchpad", Kind::Text, "The editor for the notes file, as a command line like `nvim` or `code -w`. (Worktrees open in the editor under Editors, below.)").or("your default editor, $VISUAL, $EDITOR, then nvim, vim, nano or vi").eg("nvim"),
    s("scratchpad.path", "scratchpad", Kind::Path, "Where the notes file lives. Point it into a synced folder to share it between machines.").or("scratchpad.md in ptyro's data directory").eg("~/Sync/pad.md"),
    s("scratchpad.keep", "scratchpad", int(1, 3650), "How long saved copies of the notes are kept (the newest 20 always stay).").unit("days"),
    s("scratchpad.tick_on_finish", "scratchpad", Kind::Bool, "Tick an item once the worktree started from it is merged or cleaned up."),
    s("editors.default", "scratchpad", dynamic(Source::Editors), "The editor that opens a worktree (the notes file has its own, above).").or("the first of nvim, vim, hx, code, zed, cursor that is installed"),
    s("editors.*.command", "scratchpad", Kind::Argv, "The program and arguments; {path} is the worktree."),
    s("editors.*.inside", "scratchpad", Kind::Bool, "Open it as a ptyro session, for terminal editors."),
    // ── Daemon and recording ──
    s("daemon.scrollback_lines", "daemon", int(0, 1_000_000), "How many lines of history each session keeps to scroll back through. A very wide session keeps fewer, so one never holds more than 4 million cells (20,000 lines at 200 columns).").new_only().unit("lines"),
    s("daemon.frame_interval_ms", "daemon", int(1, 1000), "The shortest wait between screen updates sent to a client. Lower is smoother and busier.").adv().new_only().unit("ms"),
    s("daemon.log_level", "daemon", pick(LOG_LEVEL), "How much the daemon writes to its log. (PTYRO_LOG, when set, wins.)"),
    s("daemon.restore_on_start", "daemon", pick(RESTORE), "What agent sessions of an earlier daemon become when it starts.").restart(),
    s("daemon.machine_name", "daemon", Kind::Text, "What to call this computer in the web page's title and the TUI, to tell several apart. Letters, digits, spaces; 24 characters at most.").also("hostname computer name machine tab title").or("this computer's name").eg("studio"),
    s("recorder.enabled", "daemon", Kind::Bool, "Record every session's output with timing, for replay and for scrolling back past what is in memory.").new_only(),
    s("recorder.max_bytes", "daemon", Kind::Bytes, "The most one session's recording takes, like 16MiB (at least 256KiB).").new_only(),
    // ── Web app and remote ──
    s("remote.serve", "remote", Kind::Bool, "Run the web app (for your phone or another computer) from the daemon, and keep it running."),
    s("remote.access", "remote", pick(ACCESS), "Who can reach the web app. It is always HTTPS (ptyro makes the certificate; trust it once from /ca.crt on a phone).").also("tailscale tailnet lan wifi network phone listen bind interface localhost open"),
    s("remote.port", "remote", int(1024, 65535), "The port the web app listens on, whoever may reach it. Pairing links and QR codes use it too.").also("listen bind address https"),
    s("remote.public_url", "remote", Kind::Text, "The address QR codes and links point at.").adv().or("worked out from how it is reached").eg("https://ptyro.example.com"),
    s("remote.tls_cert", "remote", Kind::Path, "Your own certificate (a PEM chain, leaf first), instead of the one ptyro makes.").adv().or("one ptyro makes").eg("~/certs/fullchain.pem"),
    s("remote.tls_key", "remote", Kind::Path, "The key for that certificate.").adv().or("one ptyro makes").eg("~/certs/privkey.pem"),
    s("remote.push", "remote", Kind::Bool, "Web Push to paired devices that asked for it."),
    s("remote.push_hosts", "remote", Kind::List, "More push services a device may use besides the browsers' own, like a self-hosted ntfy.").adv().or("none"),
    s("remote.approval_wait", "remote", Kind::Duration, "How long a Claude permission prompt waits for a phone's answer."),
    // ── Agents ──
    s("agents.*.adapter", "agents", pick(ADAPTERS), "Which kind of agent it is.").new_only().or("the name, if it is a known agent, else generic"),
    s("agents.*.command", "agents", Kind::Argv, "The program to run.").new_only().or("the agent's own name"),
    s("agents.*.args", "agents", Kind::List, "Arguments added to every start.").new_only(),
    s("agents.*.env", "agents", Kind::Opaque, "Environment variables set for it (edit in the file).").new_only(),
    s("agents.*.default", "agents", Kind::Bool, "The agent a new session offers first.").new_only(),
    s("agents.*.hibernate_after", "agents", Kind::Duration, "Hibernate after this long idle and unseen.").new_only().or("never"),
    s("agents.*.dangerous", "agents", Kind::Bool, "Allow arguments that skip its permission prompts. Such sessions show a ⚠.").new_only(),
    s("agents.*.trust_worktrees", "agents", Kind::Bool, "Codex: trust new worktrees of projects you trusted.").new_only().adv(),
    s("agents.*.hooks", "agents", Kind::Bool, "Wire ptyro's hooks, plugin or notify into the agent.").new_only().or("on"),
    s("agents.*.usage", "agents", Kind::Bool, "Report rate limits per profile.").new_only().or("on for Claude, off for Codex"),
    s("agents.*.after_turn", "agents", Kind::Bool, "Run the project's after_turn script after each turn.").new_only().or("on when the project is trusted"),
    s("agents.*.record", "agents", Kind::Bool, "Record this agent's sessions.").new_only().or("on"),
    s("agents.*.protocol", "agents", pick(PROTOCOLS), "The structured protocol `ptyro new --structured` speaks with it.").new_only().or("from its adapter"),
    // ── Themes ──
    s("themes.*.base", "themes", dynamic(Source::BuiltinThemes), "A built-in theme to start from.").or("none"),
    s("themes.*.accent", "themes", Kind::Color, "Highlights and the focused item."),
    s("themes.*.text", "themes", Kind::Color, "Ordinary text."),
    s("themes.*.dim", "themes", Kind::Color, "Quiet text."),
    s("themes.*.border", "themes", Kind::Color, "Pane borders."),
    s("themes.*.border_focus", "themes", Kind::Color, "The focused pane's border."),
    s("themes.*.bar_fg", "themes", Kind::Color, "Status bar text."),
    s("themes.*.bar_bg", "themes", Kind::Color, "Status bar background."),
    s("themes.*.selection_fg", "themes", Kind::Color, "Selected text."),
    s("themes.*.selection_bg", "themes", Kind::Color, "Selection background."),
    s("themes.*.search", "themes", Kind::Color, "Search matches."),
    s("themes.*.working", "themes", Kind::Color, "A session that is working."),
    s("themes.*.blocked", "themes", Kind::Color, "A session waiting on you."),
    s("themes.*.idle", "themes", Kind::Color, "A session that is idle."),
    s("themes.*.done", "themes", Kind::Color, "A session that finished."),
    s("themes.*.error", "themes", Kind::Color, "A session that failed."),
    s("themes.*.exited", "themes", Kind::Color, "A session that ended."),
    // ── Profiles ──
    s("profiles.*.label", "profiles", Kind::Text, "A name to show.").new_only().or("the profile's name"),
    s("profiles.*.color", "profiles", Kind::Color, "Its colour on borders and badges.").new_only(),
    s("profiles.*.claude", "profiles", Kind::Path, "Claude's config directory for this profile (an account with credentials is edited in the file).").new_only(),
    s("profiles.*.codex", "profiles", Kind::Path, "Codex's directory for this profile.").new_only(),
    s("profiles.*.gemini", "profiles", Kind::Path, "Gemini's directory for this profile.").new_only(),
    s("profiles.*.opencode", "profiles", Kind::Path, "OpenCode's directory for this profile.").new_only(),
    s("profiles.*.gh", "profiles", Kind::Path, "`gh`'s directory for this profile.").new_only(),
    s("profiles.*.git.name", "profiles", Kind::Text, "The name on its commits.").new_only(),
    s("profiles.*.git.email", "profiles", Kind::Text, "The email on its commits.").new_only(),
    s("profiles.*.git.ssh_key", "profiles", Kind::Path, "The SSH key it uses for git.").new_only(),
    s("profiles.*.git.config", "profiles", Kind::Path, "A whole global git config for it.").new_only().adv(),
    s("profiles.*.env", "profiles", Kind::Opaque, "Environment variables set in its sessions (edit in the file).").new_only(),
    s("profiles.*.env_file", "profiles", Kind::Path, "A file of NAME=value lines to load.").new_only().adv(),
    s("profiles.*.secrets", "profiles", Kind::Opaque, "Secrets resolved at each start (edit in the file).").new_only(),
    s("profiles.*.match.remotes", "profiles", Kind::List, "Remote address globs it claims, like github.com:acme/*.").new_only(),
    s("profiles.*.match.paths", "profiles", Kind::List, "Folder globs it claims, like ~/work/**.").new_only(),
    s("profiles.*.share", "profiles", Kind::Opaque, "Entries symlinked from the agent's default directory (edit in the file).").new_only(),
    s("profiles.*.isolate", "profiles", Kind::Bool, "Remove inherited credentials from its sessions.").new_only().or("on"),
    s("profiles.*.scrub_extra", "profiles", Kind::List, "More environment variables to remove.").new_only().adv(),
    s("profiles.*.guard", "profiles", pick(GUARD), "What to do when a spawn targets another profile's repository.").new_only(),
    s("profiles.*.secret_ttl", "profiles", Kind::Duration, "Remember resolved secrets this long.").new_only().adv().or("resolve at every start"),
    // ── This project (.ptyro.toml) ──
    s("project.name", "project", Kind::Text, "What the project is called.").or("the folder's name"),
    s("project.default_agent", "project", dynamic(Source::Agents), "Which of your agents new sessions here use.").or("your default agent"),
    s("project.base", "project", Kind::Text, "What new worktrees start from, like origin/main.").or("the repository's main branch"),
    s("project.import", "project", Kind::Bool, "Read other tools' committed setup (Conductor, Cursor, Codex…) for scripts this file doesn't set.").or("on"),
    s("worktree.copy", "project", Kind::List, "Untracked files to copy into new worktrees, like .env.").or("none"),
    s("worktree.share", "project", Kind::List, "Heavy folders to share instead of copying, like node_modules.").or("none"),
    s("worktree.share_mode", "project", pick(SHARE_MODE), "How those are shared."),
    s("worktree.start_agents", "project", pick(START_AGENTS), "When agents start in a new worktree here.").or("your worktrees setting"),
    s("worktree.branch_template", "project", Kind::Text, "How branches here are named.").or("your worktrees setting"),
    s("verify.check", "project", dynamic(Source::RunScripts), "A run script that must pass before an agent's turn counts as done.").or("none"),
    s("verify.max_bounces", "project", int(0, 100), "How many times a failing check sends the agent back to fix it before giving up.").or("the default").unit("times"),
    s("verify.auto", "project", Kind::Bool, "Verify every agent session in this project's worktrees, not only recipe steps."),
    // ── Project scripts ──
    s("scripts.setup", "scripts", Kind::Script, "Runs once in each new worktree.").or("none"),
    s("scripts.archive", "scripts", Kind::Script, "Runs before a worktree is archived or removed.").or("none"),
    s("scripts.after_turn", "scripts", Kind::Script, "Runs after each agent turn, like a linter.").or("none"),
    s("scripts.run_mode", "scripts", pick(RUN_MODE), "Whether run scripts of different worktrees may run together."),
    s("scripts.run.*.command", "scripts", Kind::Script, "What it runs; $PTYRO_PORT is this worktree's port."),
    s("scripts.run.*.default", "scripts", Kind::Bool, "The one that starts when none is named."),
];

/// The settings of one file family.
pub fn settings(scope: Scope) -> impl Iterator<Item = &'static Setting> {
    SETTINGS.iter().filter(move |s| section(s.section).is_some_and(|x| x.scope == scope))
}

// ── paths ─────────────────────────────────────────────────────────────────────────────────

/// Whether `key` (concrete: `agents.claude.command`) matches a pattern (`agents.*.command`).
pub fn matches(pattern: &str, key: &str) -> bool {
    let mut p = pattern.split('.');
    let mut k = key.split('.');
    loop {
        match (p.next(), k.next()) {
            (None, None) => return true,
            (Some("*"), Some(seg)) if !seg.is_empty() => {}
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
    }
}

/// The setting a concrete key is.
pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| matches(s.key, key))
}

/// Split a concrete key into the segments of the file: a collection's names are one segment
/// however they're spelled (`keys.global:M-1`, `prompts.fix.it` is not a thing).
pub fn split(key: &str) -> Vec<String> {
    for col in COLLECTIONS.iter().filter(|c| matches!(c.layout, Layout::Map(_))) {
        if let Some(name) = key.strip_prefix(col.path).and_then(|r| r.strip_prefix('.')) {
            return col.path.split('.').map(str::to_owned).chain([name.to_owned()]).collect();
        }
    }
    key.split('.').map(str::to_owned).collect()
}

/// The collection a concrete key belongs to, and the name in it.
pub fn member_of(key: &str) -> Option<(&'static Collection, String)> {
    let seg = split(key);
    COLLECTIONS.iter().find_map(|c| {
        let n = c.path.split('.').count();
        (seg.len() > n && seg[..n].join(".") == c.path).then(|| (c, seg[n].clone()))
    })
}

/// The pattern of a setting for the member `name` of `c`: `agents.*.command` → `agents.claude.command`.
pub fn concrete(setting: &Setting, c: &Collection, name: &str) -> String {
    let mut parts: Vec<&str> = setting.key.split('.').collect();
    parts[c.path.split('.').count()] = name;
    parts.join(".")
}

/// The settings of a collection's members (`agents.*.…`).
pub fn fields_of(c: &Collection) -> impl Iterator<Item = &'static Setting> + '_ {
    SETTINGS
        .iter()
        .filter(move |s| s.key.strip_prefix(c.path).and_then(|r| r.strip_prefix(".*.")).is_some())
}

// ── values ────────────────────────────────────────────────────────────────────────────────

/// Pieces a caller knows that this crate doesn't: the themes built into the TUI, the TUI's
/// actions. Used to offer choices and to check names; the full check is the config's own.
#[derive(Debug, Clone, Default)]
pub struct Lookups {
    pub themes: Vec<String>,
    pub actions: Vec<(String, String)>,
    /// The default key bindings, named as `[keys]` names them (`d`, `nav:j`): `(name, action)`.
    pub keys: Vec<(String, String)>,
}

/// What a [`Source`] offers, from the config and what the caller knows.
pub fn options(
    source: Source,
    config: &Config,
    project: Option<&ProjectConfig>,
    lk: &Lookups,
) -> Vec<(String, String)> {
    let named =
        |names: Vec<String>, what: &str| names.into_iter().map(|n| (n, what.to_owned())).collect();
    match source {
        Source::BuiltinThemes => {
            lk.themes.iter().map(|t| (t.clone(), "built in".to_owned())).collect()
        }
        Source::Themes => {
            let mut v: Vec<(String, String)> =
                lk.themes.iter().map(|t| (t.clone(), "built in".to_owned())).collect();
            v.extend(
                config
                    .themes
                    .keys()
                    .filter(|k| !lk.themes.contains(k))
                    .map(|k| (k.clone(), "yours".to_owned())),
            );
            v
        }
        Source::Agents => {
            let mut names: BTreeSet<String> =
                ["claude", "codex", "gemini", "opencode"].map(String::from).into();
            names.extend(config.agents.keys().cloned());
            named(names.into_iter().collect(), "agent")
        }
        Source::Editors => {
            let mut names: BTreeSet<String> =
                crate::config::BUILTIN_EDITORS.iter().map(|s| (*s).to_owned()).collect();
            names.extend(config.editors.keys().filter(|k| *k != "default").cloned());
            named(names.into_iter().collect(), "editor")
        }
        Source::Actions => lk.actions.clone(),
        Source::RunScripts => named(
            project.map(|p| p.scripts.run.keys().cloned().collect()).unwrap_or_default(),
            "run script",
        ),
    }
}

/// The value of `key` in a serialized config (`serde_json::to_value`), `Null` when it is unset
/// or not there.
pub fn lookup<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    let mut v = root;
    for seg in split(key) {
        v = match v {
            Value::Object(m) => m.get(&seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(v)
}

/// A value as a person reads it: `on`, `30s`, `a, b`, `nvim -w`; `None` when it is empty.
pub fn show(kind: &Kind, v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => if *b { "on" } else { "off" }.to_owned(),
        Value::Number(n) => n.to_string(),
        Value::String(t) => match kind {
            Kind::Secret => "••••".to_owned(),
            _ if t.is_empty() => return None,
            _ => t.clone(),
        },
        Value::Array(a) if a.is_empty() => return None,
        Value::Array(a) => match kind {
            Kind::Argv => a
                .iter()
                .map(|x| shell_quote(x.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => a
                .iter()
                .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
                .collect::<Vec<_>>()
                .join(", "),
        },
        Value::Object(m) if m.is_empty() => return None,
        Value::Object(m) => match kind {
            // A secret that runs a program: the program is not a secret.
            Kind::Secret => m.get("command").and_then(Value::as_array).map(|a| {
                format!(
                    "!{}",
                    a.iter()
                        .map(|x| shell_quote(x.as_str().unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })?,
            _ => format!("{} entr{}", m.len(), if m.len() == 1 { "y" } else { "ies" }),
        },
    })
}

fn shell_quote(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || "-_./=:@%+,~{}".contains(c)) {
        w.to_owned()
    } else {
        format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Split a command line into words, with `"double"` and `'single'` quotes and `\` escapes.
pub fn words(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut open = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    open = true;
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(ch);
                open = true;
            }
            (None, c) if c.is_whitespace() => {
                if open || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    open = false;
                }
            }
            (None, c) => {
                cur.push(c);
                open = true;
            }
        }
    }
    if quote.is_some() {
        return Err("a quote is not closed".into());
    }
    if open || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn truthy(t: &str) -> Result<bool, String> {
    match t.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "y" | "1" | "enable" | "enabled" => Ok(true),
        "false" | "off" | "no" | "n" | "0" | "disable" | "disabled" => Ok(false),
        other => Err(format!("{other:?} isn't yes or no (on, off, true, false)")),
    }
}

fn items(input: &str) -> Result<Vec<String>, String> {
    let t = input.trim();
    if t.starts_with('[') {
        let v: toml_edit::Value = t.parse().map_err(|e| format!("not a list: {e}"))?;
        let a = v.as_array().ok_or("not a list")?;
        return a
            .iter()
            .map(|x| x.as_str().map(str::to_owned).ok_or_else(|| "a list of text".to_owned()))
            .collect();
    }
    Ok(t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned).collect())
}

fn one_of(what: &str, v: &str, choices: &[Choice]) -> Result<String, String> {
    let t = v.trim();
    choices.iter().find(|c| c.value.eq_ignore_ascii_case(t)).map(|c| c.value.to_owned()).ok_or_else(
        || {
            format!(
                "{what}: {t:?} isn't one of {}",
                choices.iter().map(|c| c.value).collect::<Vec<_>>().join(", ")
            )
        },
    )
}

fn array(v: Vec<String>) -> toml_edit::Value {
    let mut a = toml_edit::Array::new();
    for x in v {
        a.push(x);
    }
    toml_edit::Value::Array(a)
}

/// What typing `input` for `setting` means, as the TOML value to write, or why it isn't one.
pub fn parse_input(
    setting: &Setting,
    input: &str,
    lk: &Lookups,
) -> Result<toml_edit::Value, String> {
    let t = input.trim();
    let key = setting.key;
    let need = |what: &str| -> Result<(), String> {
        if t.is_empty() { Err(format!("{key}: {what} (to clear it, unset it)")) } else { Ok(()) }
    };
    Ok(match setting.kind {
        Kind::Bool => truthy(t).map_err(|e| format!("{key}: {e}"))?.into(),
        Kind::Int { min, max } => {
            let n: i64 =
                t.replace('_', "").parse().map_err(|_| format!("{key}: {t:?} isn't a number"))?;
            if n < min || n > max {
                return Err(format!("{key}: {n} is outside {min} to {max}"));
            }
            n.into()
        }
        Kind::Text | Kind::Script | Kind::Path | Kind::Address | Kind::Color | Kind::Window => {
            need("it needs something")?;
            t.into()
        }
        Kind::Choice { choices } => one_of(key, t, choices)?.into(),
        Kind::Dynamic { source } => {
            need("pick one")?;
            // A name is as it is written; the config's own check says whether it exists.
            let _ = (source, lk);
            t.into()
        }
        Kind::Set { choices } => {
            let mut v = Vec::new();
            for x in items(t)? {
                let x = one_of(key, &x, choices)?;
                if !v.contains(&x) {
                    v.push(x);
                }
            }
            array(v)
        }
        Kind::List => array(items(t)?),
        Kind::Keys => {
            let v = items(t)?;
            if v.is_empty() {
                return Err(format!("{key}: needs at least one key"));
            }
            array(v)
        }
        Kind::Argv => {
            let v = if t.starts_with('[') { items(t)? } else { words(t)? };
            if v.is_empty() {
                return Err(format!("{key}: needs a program (to clear it, unset it)"));
            }
            array(v)
        }
        Kind::Duration => {
            need("like 30s, 10m, 2h, 1d or off")?;
            crate::config::parse_duration(t).map_err(|e| format!("{key}: {e}"))?;
            t.into()
        }
        Kind::Bytes => {
            need("like 16MiB")?;
            crate::config::parse_bytes(t).map_err(|e| format!("{key}: {e}"))?;
            t.into()
        }
        Kind::Secret => {
            need("it needs a value")?;
            match t.strip_prefix('!') {
                Some(cmd) => {
                    let argv = words(cmd.trim())?;
                    if argv.is_empty() {
                        return Err(format!("{key}: `!` needs a program after it"));
                    }
                    let mut tbl = toml_edit::InlineTable::new();
                    tbl.insert("command", array(argv));
                    toml_edit::Value::InlineTable(tbl)
                }
                None => t.into(),
            }
        }
        Kind::Opaque => {
            return Err(format!("{key} is a table; edit it in the file (`ptyro config edit`)"));
        }
    })
}

// ── editing ───────────────────────────────────────────────────────────────────────────────

use toml_edit::TableLike;

fn parse_doc(text: &str) -> Result<toml_edit::DocumentMut, String> {
    text.parse().map_err(|e| format!("the file isn't valid TOML: {e}"))
}

/// The table that holds the last segment of `seg`, if the path to it exists. A list of tables
/// is entered by position (`notifications.sinks.0.url`).
fn container<'a>(root: &'a dyn TableLike, seg: &[String]) -> Option<&'a dyn TableLike> {
    let mut cur = root;
    let mut i = 0;
    while i + 1 < seg.len() {
        let item = cur.get(&seg[i])?;
        if let Some(a) = item.as_array_of_tables() {
            cur = a.get(seg.get(i + 1)?.parse::<usize>().ok()?)?;
            i += 2;
        } else {
            cur = item.as_table_like()?;
            i += 1;
        }
    }
    Some(cur)
}

/// [`container`], mutable, and made (as implicit tables, so no empty headers print) when
/// `create` and it isn't there.
fn container_mut<'a>(
    root: &'a mut dyn TableLike,
    seg: &[String],
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>, String> {
    let mut cur = root;
    let mut i = 0;
    while i + 1 < seg.len() {
        let name = &seg[i];
        if !create && !cur.contains_key(name) {
            return Ok(None);
        }
        let item = cur.entry(name).or_insert_with(|| {
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        });
        if item.is_array_of_tables() {
            let idx: usize = seg
                .get(i + 1)
                .and_then(|x| x.parse().ok())
                .ok_or_else(|| format!("{}: a position is needed", seg[..=i].join(".")))?;
            let Some(t) = item.as_array_of_tables_mut().and_then(|a| a.get_mut(idx)) else {
                return Ok(None);
            };
            cur = t;
            i += 2;
        } else {
            cur = item
                .as_table_like_mut()
                .ok_or_else(|| format!("{} isn't a table in the file", seg[..=i].join(".")))?;
            i += 1;
        }
    }
    Ok(Some(cur))
}

fn item_at<'a>(doc: &'a toml_edit::DocumentMut, seg: &[String]) -> Option<&'a toml_edit::Item> {
    container(doc.as_table(), seg)?.get(seg.last()?)
}

/// Put `value` at `seg`, keeping the comment at the end of its line when it replaces one.
fn put(
    doc: &mut toml_edit::DocumentMut,
    seg: &[String],
    mut value: toml_edit::Value,
) -> Result<(), String> {
    let name = seg.last().ok_or("no key")?.clone();
    let table = container_mut(doc.as_table_mut(), seg, true)?.ok_or("no such place in the file")?;
    // Change the value where it stands: the key keeps the comment lines above it, and the value
    // the one after it. (`insert` over an existing key would drop the lines above.)
    match table.get_mut(&name) {
        Some(old) if old.is_table() || old.is_array_of_tables() => {
            return Err(format!("{} is a table in the file; edit it there", seg.join(".")));
        }
        Some(old) => {
            if let Some(v) = old.as_value() {
                *value.decor_mut() = v.decor().clone();
            }
            *old = toml_edit::Item::Value(value);
        }
        None => {
            table.insert(&name, toml_edit::Item::Value(value));
        }
    }
    Ok(())
}

/// What is wrong with `text` as a file of `scope`'s settings, if anything.
pub fn check(scope: Scope, text: &str) -> Result<(), String> {
    match scope {
        Scope::User => Config::parse(text).map(|_| ()),
        Scope::Project => {
            let t = ProjectConfig::parse(text)?;
            // One layer may be a partial override of the other (`[scripts.run.dev] default = true`):
            // what is missing is checked on the merge (`settings::ops`).
            let c: ProjectConfig = match t.try_into::<ProjectConfig>() {
                Ok(c) => c,
                Err(e) if e.to_string().contains("missing field") => return Ok(()),
                Err(e) => return Err(e.to_string().trim_end().to_owned()),
            };
            if c.project.base.as_deref().is_some_and(|b| b.trim_start().starts_with('-')) {
                return Err("project.base: must not start with `-`".into());
            }
            if let Some(t) = &c.worktree.branch_template {
                crate::model::check_branch_template(t)
                    .map_err(|e| format!("worktree.branch_template: {e}"))?;
            }
            Ok(())
        }
    }
}

/// The caller's own check of a whole new file (key bindings and themes, which need the TUI to
/// judge); `&|_| Ok(())` for none.
pub type Extra<'a> = &'a dyn Fn(&str) -> Result<(), String>;

fn finish(scope: Scope, key: &str, new: String, extra: Extra<'_>) -> Result<String, String> {
    check(scope, &new).map_err(|e| format!("{key}: that would make the config invalid: {e}"))?;
    extra(&new).map_err(|e| format!("{key}: {e}"))?;
    Ok(new)
}

/// How many single-character edits turn `a` into `b`.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Why `key` isn't a setting, and the nearest ones.
pub fn unknown(key: &str) -> String {
    let mine: Vec<&str> = key.split('.').collect();
    let mut near: Vec<(usize, String)> = SETTINGS
        .iter()
        .filter_map(|s| {
            // A member's name stands in for the `*`.
            let cand: Vec<&str> = s
                .key
                .split('.')
                .enumerate()
                .map(|(i, seg)| if seg == "*" { mine.get(i).copied().unwrap_or(seg) } else { seg })
                .collect();
            let cand = cand.join(".");
            let d = distance(key, &cand).min(
                distance(
                    mine.last().copied().unwrap_or(key),
                    cand.rsplit('.').next().unwrap_or(&cand),
                ) + 1,
            );
            (d <= 3.max(key.len() / 6)).then_some((d, cand))
        })
        .collect();
    near.sort();
    near.dedup_by(|a, b| a.1 == b.1);
    let near: Vec<String> = near.into_iter().take(3).map(|(_, k)| k).collect();
    if near.is_empty() {
        format!("{key}: no such setting (`ptyro config list` shows them all)")
    } else {
        format!("{key}: no such setting. Did you mean {}?", near.join(", "))
    }
}

/// What `key` is in `scope`: a setting, or a member of a map collection (a binding, a prompt).
enum Target {
    Setting(&'static Setting),
    Map(&'static Collection, Kind),
}

fn target(scope: Scope, key: &str) -> Result<Target, String> {
    let member = member_of(key);
    let setting = find(key);
    let in_section = member.as_ref().map(|(c, _)| c.section).or(setting.map(|s| s.section));
    let Some(sec) = in_section.and_then(section) else { return Err(unknown(key)) };
    if sec.scope != scope {
        return Err(match scope {
            Scope::User => format!(
                "{key} is a project setting: use `--project` (it lives in the repository's .ptyro.toml)"
            ),
            Scope::Project => format!(
                "{key} is a user setting: agents, profiles, keys and the rest can only be set in your own config, never in a repository (SPEC §13.1)"
            ),
        });
    }
    match (member, setting) {
        (Some((c, name)), _) if matches!(c.layout, Layout::Map(_)) && !name.is_empty() => {
            let Layout::Map(kind) = c.layout else { unreachable!("matched above") };
            Ok(Target::Map(c, kind))
        }
        (_, Some(s)) => Ok(Target::Setting(s)),
        _ => Err(unknown(key)),
    }
}

fn map_setting(c: &Collection, kind: Kind) -> Setting {
    s(c.path, c.section, kind, "")
}

/// Set `key` to what `input` means, in the file's `text`. Everything else in the file stays as
/// it is.
pub fn set(
    scope: Scope,
    text: &str,
    key: &str,
    input: &str,
    lk: &Lookups,
    extra: Extra<'_>,
) -> Result<String, String> {
    let value = match target(scope, key)? {
        Target::Map(c, kind) => parse_input(&map_setting(c, kind), input, lk)?,
        Target::Setting(st) => {
            let v = parse_input(st, input, lk)?;
            // An account may be a directory or a table with credentials: never write over the table.
            if matches!(st.kind, Kind::Path)
                && st.key.starts_with("profiles.*.")
                && parse_doc(text)?.as_table().get("profiles").is_some()
                && parse_doc(text)
                    .ok()
                    .and_then(|d| item_at(&d, &split(key)).map(toml_edit::Item::is_table_like))
                    == Some(true)
            {
                return Err(format!(
                    "{key} has credentials set in the file; edit it there (`ptyro config edit`)"
                ));
            }
            v
        }
    };
    let mut doc = parse_doc(text)?;
    put(&mut doc, &split(key), value)?;
    finish(scope, key, doc.to_string(), extra)
}

/// Remove `key` from the file, so the default (or the next layer) applies again.
pub fn unset(scope: Scope, text: &str, key: &str, extra: Extra<'_>) -> Result<String, String> {
    target(scope, key)?;
    let mut doc = parse_doc(text)?;
    let seg = split(key);
    let name = seg.last().ok_or("no key")?.clone();
    // An account with credentials is a table of its own: not removed by a reset.
    if matches!(find(key).map(|s| s.kind), Some(Kind::Path))
        && key.starts_with("profiles.")
        && item_at(&doc, &seg).is_some_and(toml_edit::Item::is_table_like)
    {
        return Err(format!(
            "{key} has credentials set in the file; remove it there (`ptyro config edit`)"
        ));
    }
    let gone = container_mut(doc.as_table_mut(), &seg, false)?.and_then(|t| t.remove(&name));
    if gone.is_none() {
        return Err(format!("{key} isn't set in the file"));
    }
    finish(scope, key, doc.to_string(), extra)
}

/// Whether the file sets `key`.
pub fn is_set(text: &str, key: &str) -> bool {
    parse_doc(text).is_ok_and(|d| item_at(&d, &split(key)).is_some())
}

/// The fields a new member of `path` must have to be valid: a sink's address, an editor's
/// command. `kind` is a sink's type.
pub fn required(path: &str, kind: &str) -> &'static [&'static str] {
    match (path, kind) {
        ("notifications.sinks", "ntfy") => &["url"],
        ("notifications.sinks", "pushover") => &["token", "user"],
        ("notifications.sinks", "slack" | "discord") => &["webhook"],
        ("notifications.sinks", "telegram") => &["token", "chat_id"],
        ("notifications.sinks", "command") => &["command"],
        ("editors" | "scripts.run", _) => &["command"],
        _ => &[],
    }
}

/// Add a member to a collection. `fields` are its settings by short name (`command`, `url`; a
/// sink's `type`); a binding or prompt takes its action or text as `value`. The new text is
/// checked whole, so nothing half-made is written.
pub fn add(
    scope: Scope,
    text: &str,
    path: &str,
    name: &str,
    fields: &[(&str, &str)],
    lk: &Lookups,
    extra: Extra<'_>,
) -> Result<String, String> {
    let col = collection(path).ok_or_else(|| format!("{path} isn't a group of named things"))?;
    if section(col.section).is_some_and(|s| s.scope != scope) {
        return Err(format!("{path} belongs to the other kind of config file"));
    }
    let mut doc = parse_doc(text)?;
    let base: Vec<String> = path.split('.').map(str::to_owned).collect();
    let name = name.trim();
    let field = |k: &str| fields.iter().find(|(f, _)| *f == k).map(|(_, v)| *v);
    match col.layout {
        Layout::Map(kind) => {
            if name.is_empty() {
                return Err(format!("a {} needs a name", col.noun));
            }
            let v = field("value").ok_or_else(|| format!("a {} needs a value", col.noun))?;
            let value = parse_input(&map_setting(col, kind), v, lk)?;
            put(&mut doc, &[base, vec![name.to_owned()]].concat(), value)?;
        }
        Layout::Tables => {
            if name.is_empty() {
                return Err(format!("a {} needs a name", col.noun));
            }
            if col.reserved.contains(&name) {
                return Err(format!("{name:?} is taken by a setting of its own"));
            }
            crate::names::validate(name).map_err(|e| format!("{name:?}: {e}"))?;
            if name.contains('.') {
                return Err(format!("{name:?}: a dot can't be used in a {} name here", col.noun));
            }
            let full = [base, vec![name.to_owned()]].concat();
            if item_at(&doc, &full).is_some() {
                return Err(format!("there is already a {} called {name:?}", col.noun));
            }
            let mut all: Vec<(&str, &str)> = col.starter.to_vec();
            all.extend_from_slice(fields);
            for need in required(path, "") {
                if !all.iter().any(|(k, _)| k == need) {
                    return Err(format!("a {} needs its {need}", col.noun));
                }
            }
            // The header prints even while the table is empty.
            container_mut(doc.as_table_mut(), &[full.clone(), vec!["x".into()]].concat(), true)?;
            if let Some(t) = lookup_mut(&mut doc, &full).and_then(toml_edit::Item::as_table_mut) {
                t.set_implicit(false);
            }
            for (k, v) in all {
                let sk = format!("{path}.{name}.{k}");
                let st = find(&sk).ok_or_else(|| unknown(&sk))?;
                put(
                    &mut doc,
                    &[full.clone(), vec![k.to_owned()]].concat(),
                    parse_input(st, v, lk)?,
                )?;
            }
        }
        Layout::Array => {
            let kind = field("type").ok_or("a sink needs a type")?;
            let kind = one_of("type", kind, SINK_TYPES)?;
            for need in required(path, &kind) {
                if !fields.iter().any(|(k, _)| k == need) {
                    return Err(format!("a {kind} sink needs its {need}"));
                }
            }
            let mut t = toml_edit::Table::new();
            t.insert("type", toml_edit::value(kind));
            for (k, v) in fields.iter().filter(|(k, _)| *k != "type" && *k != "value") {
                let sk = format!("{path}.0.{k}");
                let st = find(&sk).ok_or_else(|| unknown(&sk))?;
                t.insert(k, toml_edit::Item::Value(parse_input(st, v, lk)?));
            }
            let parent =
                container_mut(doc.as_table_mut(), &base, true)?.ok_or("no place for it")?;
            let last = base.last().expect("a path");
            let list = parent
                .entry(last)
                .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
            list.as_array_of_tables_mut()
                .ok_or_else(|| format!("{path} isn't a list of tables in the file"))?
                .push(t);
        }
    }
    finish(scope, path, doc.to_string(), extra)
}

fn lookup_mut<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    seg: &[String],
) -> Option<&'a mut toml_edit::Item> {
    let name = seg.last()?.clone();
    container_mut(doc.as_table_mut(), seg, false).ok()??.get_mut(&name)
}

/// The names in a collection, in the file's order (positions, for a list).
pub fn members(text: &str, path: &str) -> Vec<String> {
    let Some(col) = collection(path) else { return Vec::new() };
    let Ok(doc) = parse_doc(text) else { return Vec::new() };
    let seg: Vec<String> = path.split('.').map(str::to_owned).collect();
    match item_at(&doc, &seg) {
        Some(toml_edit::Item::ArrayOfTables(a)) => (0..a.len()).map(|i| i.to_string()).collect(),
        Some(other) => other
            .as_table_like()
            .map(|t| {
                t.iter()
                    .map(|(k, _)| k.to_owned())
                    .filter(|k| !col.reserved.contains(&k.as_str()))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Remove a member and everything in it.
pub fn remove(
    scope: Scope,
    text: &str,
    path: &str,
    name: &str,
    extra: Extra<'_>,
) -> Result<String, String> {
    let col = collection(path).ok_or_else(|| format!("{path} isn't a group of named things"))?;
    let mut doc = parse_doc(text)?;
    let seg: Vec<String> = path.split('.').map(str::to_owned).collect();
    let item =
        lookup_mut(&mut doc, &seg).ok_or_else(|| format!("there is no {} to remove", col.noun))?;
    let emptied = match item {
        toml_edit::Item::ArrayOfTables(a) => {
            let i: usize = name.parse().map_err(|_| format!("{name:?} isn't a position"))?;
            if i >= a.len() {
                return Err(format!("there is no {} {i}", col.noun));
            }
            a.remove(i);
            a.is_empty()
        }
        other => {
            let t = other
                .as_table_like_mut()
                .ok_or_else(|| format!("{path} isn't a table in the file"))?;
            if t.remove(name).is_none() {
                return Err(format!("there is no {} called {name:?}", col.noun));
            }
            false
        }
    };
    if emptied && let Some((last, up)) = seg.split_last() {
        let _ =
            container_mut(doc.as_table_mut(), &[up.to_vec(), vec![last.clone()]].concat(), false)
                .ok()
                .flatten()
                .map(|t| t.remove(last));
    }
    finish(scope, &format!("{path}.{name}"), doc.to_string(), extra)
}

/// Give a member another name, keeping what is in it.
pub fn rename(
    scope: Scope,
    text: &str,
    path: &str,
    from: &str,
    to: &str,
    extra: Extra<'_>,
) -> Result<String, String> {
    let col = collection(path).ok_or_else(|| format!("{path} isn't a group of named things"))?;
    if matches!(col.layout, Layout::Array) {
        return Err("sinks are ordered, not named".into());
    }
    let to = to.trim();
    if to.is_empty() {
        return Err(format!("a {} needs a name", col.noun));
    }
    if col.reserved.contains(&to) {
        return Err(format!("{to:?} is taken by a setting of its own"));
    }
    if matches!(col.layout, Layout::Tables) {
        crate::names::validate(to).map_err(|e| format!("{to:?}: {e}"))?;
        if to.contains('.') {
            return Err(format!("{to:?}: a dot can't be used in a {} name here", col.noun));
        }
    }
    let mut doc = parse_doc(text)?;
    let seg: Vec<String> = path.split('.').map(str::to_owned).collect();
    let t = lookup_mut(&mut doc, &seg)
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| format!("there is no {} called {from:?}", col.noun))?;
    if t.contains_key(to) {
        return Err(format!("there is already a {} called {to:?}", col.noun));
    }
    let decor = t.get_key_value(from).map(|(k, _)| k.leaf_decor().clone());
    let item = t.remove(from).ok_or_else(|| format!("there is no {} called {from:?}", col.noun))?;
    t.insert(to, item);
    if let (Some(d), Some(mut k)) = (decor, t.key_mut(to)) {
        *k.leaf_decor_mut() = d;
    }
    finish(scope, &format!("{path}.{to}"), doc.to_string(), extra)
}

// ── what the panel shows ──────────────────────────────────────────────────────────────────

/// Where an effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Nothing sets it: the default.
    Default,
    /// Your config file.
    User,
    /// The repository's `.ptyro.toml`.
    Project,
    /// The repository's `.ptyro.local.toml`.
    Local,
}

impl Origin {
    pub fn says(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::User => "your config",
            Self::Project => ".ptyro.toml",
            Self::Local => ".ptyro.local.toml",
        }
    }
}

/// One line of the panel: a concrete key, its value in effect, its default, and where it is from.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub key: String,
    pub setting: &'static Setting,
    pub value: Value,
    pub default: Value,
    pub origin: Origin,
    /// The line in the file that sets it (1-based), for "open the file here".
    pub line: Option<usize>,
    /// The collection and member it belongs to.
    pub member: Option<(String, String)>,
}

fn or_otherwise(setting: &Setting, kind_text: Option<String>) -> String {
    kind_text.unwrap_or_else(|| {
        if setting.otherwise.is_empty() {
            "—".to_owned()
        } else {
            format!("({})", setting.otherwise)
        }
    })
}

fn with_unit(setting: &Setting, shown: String) -> String {
    if setting.unit.is_empty() || shown.is_empty() || !shown.chars().all(|c| c.is_ascii_digit()) {
        shown
    } else {
        format!("{shown} {}", setting.unit)
    }
}

impl Entry {
    /// The value as shown, or what it falls back to, or `—`.
    pub fn shown(&self) -> String {
        or_otherwise(self.setting, show(&self.setting.kind, &self.value))
    }

    /// The default as shown.
    pub fn default_shown(&self) -> String {
        or_otherwise(self.setting, show(&self.setting.kind, &self.default))
    }

    /// [`shown`](Self::shown), with what a number counts after it (`30 columns`).
    pub fn pretty(&self) -> String {
        with_unit(self.setting, self.shown())
    }

    /// [`default_shown`](Self::default_shown), the same way.
    pub fn default_pretty(&self) -> String {
        with_unit(self.setting, self.default_shown())
    }

    /// Differs from what it would be with nothing in the file.
    pub fn changed(&self) -> bool {
        self.origin != Origin::Default && self.value != self.default
    }
}

/// The 1-based line of `key` in a parsed file.
fn line_of(text: &str, doc: &toml_edit::Document<String>, seg: &[String]) -> Option<usize> {
    let t = container(doc.as_table(), seg)?;
    let (key, item) = t.get_key_value(seg.last()?)?;
    let at = key.span().or_else(|| item.span())?.start;
    Some(text[..at.min(text.len())].bytes().filter(|b| *b == b'\n').count() + 1)
}

/// What a fresh member of a collection holds, field by field.
fn member_default(path: &str) -> Value {
    use crate::config::{AgentDef, ProfileDef, ThemeDef};
    match path {
        "agents" => serde_json::to_value(AgentDef::default()),
        "profiles" => serde_json::to_value(ProfileDef::default()),
        "themes" => serde_json::to_value(ThemeDef::default()),
        "editors" => Ok(serde_json::json!({ "command": null, "inside": false })),
        "scripts.run" => Ok(serde_json::json!({ "command": null, "default": false })),
        _ => Ok(Value::Null),
    }
    .unwrap_or_default()
}

/// Every setting of `scope` as the panel lists it: the fixed keys, then each member of every
/// collection with its fields. `root` is the effective config as JSON; `files` are the texts
/// that set things, the one that wins last, each with where it is from.
pub fn entries(scope: Scope, root: &Value, files: &[(Origin, &str)]) -> Vec<Entry> {
    let defaults = match scope {
        Scope::User => serde_json::to_value(Config::default()),
        Scope::Project => serde_json::to_value(ProjectConfig::default()),
    }
    .unwrap_or_default();
    let docs: Vec<(Origin, &str, Option<toml_edit::Document<String>>)> = files
        .iter()
        .map(|(o, t)| (*o, *t, toml_edit::Document::parse((*t).to_owned()).ok()))
        .collect();
    let origin_of = |seg: &[String]| -> (Origin, Option<usize>) {
        let mut found = (Origin::Default, None);
        for (o, text, doc) in &docs {
            if let Some(d) = doc
                && container(d.as_table(), seg)
                    .is_some_and(|t| seg.last().is_some_and(|l| t.contains_key(l)))
            {
                found = (*o, line_of(text, d, seg));
            }
        }
        found
    };
    let mut out = Vec::new();
    let mut push = |key: String, setting: &'static Setting, member: Option<(String, String)>| {
        let value = lookup(root, &key).cloned().unwrap_or(Value::Null);
        let default = match &member {
            Some((path, _)) => {
                let tail = setting.key.strip_prefix(&format!("{path}.*.")).unwrap_or_default();
                lookup(&member_default(path), tail).cloned().unwrap_or(Value::Null)
            }
            None => lookup(&defaults, &key).cloned().unwrap_or(Value::Null),
        };
        let (origin, line) = origin_of(&split(&key));
        out.push(Entry { key, setting, value, default, origin, line, member });
    };
    for st in settings(scope).filter(|s| !s.key.contains('*')) {
        push(st.key.to_owned(), st, None);
    }
    for col in COLLECTIONS.iter().filter(|c| section(c.section).is_some_and(|s| s.scope == scope)) {
        if matches!(col.layout, Layout::Map(_)) {
            continue;
        }
        let names: Vec<String> = match lookup(root, col.path) {
            Some(Value::Object(m)) => {
                m.keys().filter(|k| !col.reserved.contains(&k.as_str())).cloned().collect()
            }
            Some(Value::Array(a)) => (0..a.len()).map(|i| i.to_string()).collect(),
            _ => Vec::new(),
        };
        for name in names {
            for st in fields_of(col) {
                push(concrete(st, col, &name), st, Some((col.path.to_owned(), name.clone())));
            }
        }
    }
    out
}

/// The bindings or prompts of a map collection, as `(name, value)`.
pub fn map_members(root: &Value, path: &str) -> Vec<(String, String)> {
    match lookup(root, path) {
        Some(Value::Object(m)) => {
            m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned())).collect()
        }
        _ => Vec::new(),
    }
}

/// The commented file a first `config edit` starts from: every ordinary setting, grouped, with
/// its default and what it does. Everything is commented out, so the file changes nothing until
/// someone edits a line.
pub fn starter() -> String {
    let defaults = serde_json::to_value(Config::default()).unwrap_or_default();
    let mut out = String::from(
        "# ptyro configuration. Every key is optional, and what is commented out is the default.\n\
         # Changes apply as soon as you save (the daemon and open screens reload this file).\n\
         # `ptyro config list` shows every setting; Settings (the `config.panel` key, `C-q C` by default) edits them without this file.\n",
    );
    let mut tables: Vec<&str> = Vec::new();
    for st in settings(Scope::User)
        .filter(|s| !s.advanced && !s.key.contains('*') && !matches!(s.kind, Kind::Opaque))
    {
        let Some((table, key)) = st.key.rsplit_once('.') else { continue };
        let rendered = match lookup(&defaults, st.key) {
            None | Some(Value::Null) if !st.example.is_empty() => {
                parse_input(st, st.example, &Lookups::default())
                    .map(|v| v.to_string().trim().to_owned())
                    .unwrap_or_default()
            }
            None | Some(Value::Null) => String::new(),
            Some(v) => toml::to_string(&serde_json::json!({ "x": v }))
                .ok()
                .and_then(|t| t.strip_prefix("x = ").map(|r| r.trim_end().to_owned()))
                .unwrap_or_default(),
        };
        if rendered.is_empty() {
            continue;
        }
        if !tables.contains(&table) {
            tables.push(table);
            out.push_str(&format!("\n[{table}]\n"));
        }
        let line = format!("# {key} = {rendered}");
        out.push_str(&format!("{:<44} # {}\n", line.trim_end(), st.help));
    }
    out.push_str("\n[keys]\n# Rebind after-prefix keys: \"key\" = \"action [args]\"; \"none\" unbinds. `C-q ?` lists actions.\n# \"a\" = \"session.next\"\n# \"global:M-1\" = \"session.focus 1\"   # global: works without the prefix\n# \"d\" = \"none\"                    # unbind a default (Settings lists every key)\n");
    out
}

mod keys;
pub use keys::*;
mod ops;
pub use ops::*;

#[cfg(test)]
mod tests;
