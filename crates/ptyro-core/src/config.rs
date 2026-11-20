//! User configuration (SPEC §13): `~/.config/ptyro/config.toml`, or `$PTYRO_CONFIG`.
//!
//! Every key is optional. Typos inside known sections are errors with line and column; sections
//! that later milestones will use (`[agents]`, `[notifications]`, …) are accepted now and reported
//! by `ptyro config check` as "not used yet", so a config written from the SPEC never breaks.
//!
//! Profiles (SPEC §10.10) live here and only here: a repository's `.ptyro.toml` can never define
//! or select one (ADR 15).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::input::{KeyCode, KeyEvent, KeyKind, Mods};
use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// The prefix key or keys (SPEC §11.2), e.g. `C-q`, or `["C-q", "C-Space"]`.
    pub prefix: Prefix,
    pub theme: String,
    pub sidebar: bool,
    pub sidebar_width: u16,
    pub status_bar: bool,
    pub clock: bool,
    /// The daemon's computer's load average in the status bar (and the web's header).
    pub load: bool,
    /// Capture the mouse for click-to-focus, wheel scrollback and border drags.
    pub mouse: bool,
    /// Ask before stopping, killing, or stopping and removing a session (the TUI and the web).
    pub confirm_kill: bool,
    /// Tint pane borders with the session's profile colour (SPEC §10.10).
    pub tint_borders: bool,
    /// What a pane shows after you stop its session from the TUI (UX1.23).
    pub on_stop: OnStop,
    /// How much a sidebar row says under its first line (UX1.2).
    pub sidebar_detail: SidebarDetail,
    /// What happens to a shell that exits: removed, or kept as an exited row. Agents stay.
    pub shell_exit: ShellExit,
    /// What the shell key (`!`) shows: a drawer when the project is open, else a panel (UX2.3).
    pub shell_style: ShellStyle,
    /// Milliseconds after the prefix before the menu of next keys appears (UX1.14); 0: never.
    pub which_key_delay: u64,
    /// Windows Terminal's progress ring on the tab and taskbar (OSC 9;4, WIN2): how it looks while
    /// a session needs you, has an error, or works. The first of those three that applies and
    /// isn't `off` shows.
    pub progress_blocked: ProgressLook,
    pub progress_errored: ProgressLook,
    pub progress_working: ProgressLook,
}

/// `[ui] progress_*`: one of the looks OSC 9;4 offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgressLook {
    /// Nothing shown for this state.
    Off,
    /// Spinning: busy, no amount.
    Busy,
    Green,
    Yellow,
    Red,
}

impl ProgressLook {
    /// OSC 9;4's state and percent: green is "progress" (1), red "error" (2), busy
    /// "indeterminate" (3), yellow "paused" (4); a full ring for the coloured ones.
    pub fn osc(self) -> (u8, u8) {
        match self {
            ProgressLook::Off => (0, 0),
            ProgressLook::Green => (1, 100),
            ProgressLook::Red => (2, 100),
            ProgressLook::Busy => (3, 0),
            ProgressLook::Yellow => (4, 100),
        }
    }
}

/// `[ui] sidebar_detail`: the lines under each session row (what it wants, what it's on, its
/// last reply, a shell's command).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarDetail {
    /// Every row while they all fit; else the selected, focused, blocked and attention rows.
    #[default]
    Auto,
    /// One line per row.
    Compact,
    /// Every row, always.
    Full,
}

/// `[ui] shell_style`: how the shell key (`shell.here`) shows a worktree's quick terminal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellStyle {
    /// A drawer under a pane of the same project when one is open, else a panel of its own.
    Auto,
    /// Always a drawer under the focused pane.
    Drawer,
    /// Always a panel of its own, full sized (the default since UX3.1: only `C-t` asks for the
    /// drawer).
    #[default]
    Pane,
}

/// What becomes of a shell when it exits, you having typed `exit` (`[ui] shell_exit`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellExit {
    /// It disappears from the sidebar and its pane, as if stopped with `x` (agents are kept).
    #[default]
    Close,
    /// It stays in the sidebar as exited, with its last screen, until you remove it.
    Keep,
}

/// What a pane does once you stop the session it shows (`[ui] on_stop`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnStop {
    /// Show the nearest other session (the stopped one stays in the sidebar).
    #[default]
    Next,
    /// Keep showing the stopped session's last screen.
    Stay,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            prefix: Prefix::Keys(vec!["C-q".into(), "C-Space".into()]),
            theme: "ptyro-dark".into(),
            sidebar: true,
            sidebar_width: 30,
            status_bar: true,
            clock: true,
            load: true,
            mouse: true,
            confirm_kill: true,
            tint_borders: false,
            on_stop: OnStop::Next,
            sidebar_detail: SidebarDetail::Auto,
            shell_style: ShellStyle::Pane,
            shell_exit: ShellExit::Close,
            which_key_delay: 400,
            progress_blocked: ProgressLook::Yellow,
            progress_errored: ProgressLook::Red,
            progress_working: ProgressLook::Busy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    pub scrollback_lines: usize,
    pub frame_interval_ms: u64,
    pub log_level: String,
    /// What agent sessions of an earlier daemon become (SPEC §7.5).
    pub restore_on_start: RestoreOnStart,
    /// What to call this machine in the web page's title and the TUI (default: its host name).
    pub machine_name: Option<String>,
    /// Inside WSL, keep the distro running while the daemon runs, so closing the last terminal
    /// doesn't stop the agents (SPEC §20.10). Does nothing elsewhere.
    pub hold_wsl: bool,
}

impl Default for Daemon {
    fn default() -> Self {
        Self {
            scrollback_lines: 5_000,
            frame_interval_ms: 8,
            log_level: "info".into(),
            restore_on_start: RestoreOnStart::Dormant,
            machine_name: None,
            hold_wsl: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreOnStart {
    /// Listed, resumed when you open them.
    #[default]
    Dormant,
    /// Resumed right away (staggered, 3 at a time).
    Resume,
    /// Not restored.
    None,
}

/// The prefix (SPEC §11.2): one key (`"C-q"`) or several (`["C-q", "C-Space"]`), any of which
/// starts a command. The first is the one hints show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Prefix {
    Key(String),
    Keys(Vec<String>),
}

impl Prefix {
    pub fn keys(&self) -> &[String] {
        match self {
            Self::Key(k) => std::slice::from_ref(k),
            Self::Keys(v) => v,
        }
    }
}

/// A command: a string (`"claude"`) or an argv (`["claude", "--model", "opus"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandSpec {
    Line(String),
    Argv(Vec<String>),
}

impl CommandSpec {
    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::Line(s) => vec![s.clone()],
            Self::Argv(v) => v.clone(),
        }
    }
}

/// The agents ptyro knows without a `[agents.<name>]` table.
pub const BUILTIN_AGENTS: &[&str] = &["claude", "codex", "opencode"];

/// Built-in agents that were removed (2026-11-17, the pare-down), with their names for people.
/// An `[agents.<name>]` table that gives its own `command` still runs as an agent of your own
/// (the generic adapter); one without, or a default agent that names it, is reported with its
/// fix, which `ptyro doctor` applies.
pub const REMOVED_AGENTS: &[(&str, &str)] =
    &[("gemini", "Gemini CLI"), ("antigravity", "Antigravity")];

/// The removed built-in `name` is, as `(name, its name for people)`, when nothing of yours
/// (`[agents.<name>] command`) makes it an agent of your own.
pub fn removed_agent<'a>(agents: &Agents, name: &'a str) -> Option<(&'a str, &'static str)> {
    let (_, label) = REMOVED_AGENTS.iter().find(|(n, _)| *n == name)?;
    agents.get(name).is_none_or(|d| d.command.is_none()).then_some((name, *label))
}

/// `[agents]` (SPEC §13.2): `default = "<agent>"`, the agent new sessions use when nothing else
/// says (`claude` when unset), plus one `[agents.<name>]` table each. It reads as the map of those
/// tables; `default` is the one key that isn't one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Agents {
    pub default: Option<String>,
    defs: BTreeMap<String, AgentDef>,
}

impl std::ops::Deref for Agents {
    type Target = BTreeMap<String, AgentDef>;
    fn deref(&self) -> &Self::Target {
        &self.defs
    }
}

impl std::ops::DerefMut for Agents {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.defs
    }
}

impl<'a> IntoIterator for &'a Agents {
    type Item = (&'a String, &'a AgentDef);
    type IntoIter = std::collections::btree_map::Iter<'a, String, AgentDef>;
    fn into_iter(self) -> Self::IntoIter {
        self.defs.iter()
    }
}

impl Serialize for Agents {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        if let Some(d) = &self.default {
            m.serialize_entry("default", d)?;
        }
        for (k, v) in &self.defs {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Agents {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Agents;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("`default = \"<agent>\"` and `[agents.<name>]` tables")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Agents, A::Error> {
                let mut out = Agents::default();
                while let Some(k) = m.next_key::<String>()? {
                    if k == "default" {
                        out.default = Some(m.next_value()?);
                    } else {
                        let v = m.next_value()?;
                        out.defs.insert(k, v);
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_map(V)
    }
}

/// An agent (SPEC §13.2 `[agents.<name>]`). User config only (ADR 15).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentDef {
    /// `claude`, `codex`, `opencode`, `generic` (default: the name if it's a known adapter, else
    /// `generic`).
    pub adapter: Option<String>,
    pub command: Option<CommandSpec>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The older way to name the default agent; `[agents] default` wins over it.
    pub default: bool,
    /// Hibernate after this long idle and unseen (`"2h"`; default off).
    pub hibernate_after: Option<String>,
    /// Required for args that skip permission prompts; such sessions show a ⚠ badge.
    pub dangerous: bool,
    /// Codex: trust new worktrees of projects you trusted (SPEC §8.4).
    pub trust_worktrees: bool,
    /// Wire ptyro's hooks / plugin / notify into the agent (default true).
    pub hooks: Option<bool>,
    /// Report rate limits per profile (Claude statusline relay; Codex app-server). Default on
    /// for Claude, off for Codex.
    pub usage: Option<bool>,
    /// Record this agent's sessions (`[recorder]`; default true).
    pub record: Option<bool>,
    /// `false`: none of its sessions are kept in the knowledge base, and it gets no tools to
    /// search it (SPEC §25.13; default true).
    pub kb: Option<bool>,
    /// The structured protocol a chat speaks with it (M10, SPEC §22): `stream-json`,
    /// `app-server`, `acp` or `opencode`. Default from its adapter; `acp` lets any Agent Client
    /// Protocol agent chat.
    pub protocol: Option<String>,
}

impl AgentDef {
    pub fn hooks(&self) -> bool {
        self.hooks.unwrap_or(true)
    }
}

/// Where `a` (next attention) may go (SPEC §9.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttentionScope {
    #[default]
    CurrentFirst,
    Current,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttentionConfig {
    pub scope: AttentionScope,
    /// Kinds that notify even in `notify = "badge"` workspaces.
    pub breakthrough: Vec<String>,
    /// Lower-confidence sources apply after high ones were quiet this long (SPEC §9.2).
    pub stale_after: String,
    /// An `Idle` after working at least this long raises attention.
    pub min_work: String,
    /// Opening a dormant session resumes it.
    pub resume_on_focus: bool,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            scope: AttentionScope::CurrentFirst,
            breakthrough: vec!["blocked".into()],
            stale_after: "5m".into(),
            min_work: "10s".into(),
            resume_on_focus: true,
        }
    }
}

/// An external notification sink (M4.22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SinkDef {
    /// ntfy.sh or a self-hosted ntfy: `url = "https://ntfy.sh/my-topic"`.
    Ntfy {
        url: String,
        #[serde(default)]
        token: Option<Secret>,
        #[serde(default)]
        on: Vec<String>,
    },
    Pushover {
        token: Secret,
        user: Secret,
        #[serde(default)]
        on: Vec<String>,
    },
    Slack {
        webhook: Secret,
        #[serde(default)]
        on: Vec<String>,
    },
    Discord {
        webhook: Secret,
        #[serde(default)]
        on: Vec<String>,
    },
    Telegram {
        token: Secret,
        chat_id: String,
        #[serde(default)]
        on: Vec<String>,
    },
    /// Any program: gets the notification as JSON on stdin.
    Command {
        command: Vec<String>,
        #[serde(default)]
        on: Vec<String>,
    },
}

impl SinkDef {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ntfy { .. } => "ntfy",
            Self::Pushover { .. } => "pushover",
            Self::Slack { .. } => "slack",
            Self::Discord { .. } => "discord",
            Self::Telegram { .. } => "telegram",
            Self::Command { .. } => "command",
        }
    }

    /// Which attention kinds it gets (empty: `blocked` and `errored`).
    pub fn on(&self) -> &[String] {
        match self {
            Self::Ntfy { on, .. }
            | Self::Pushover { on, .. }
            | Self::Slack { on, .. }
            | Self::Discord { on, .. }
            | Self::Telegram { on, .. }
            | Self::Command { on, .. } => on,
        }
    }
}

/// The approve/deny callback listener (M4.22): off unless `listen` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Callback {
    /// `127.0.0.1:7591`, or a tailnet address. Others (`0.0.0.0`, a LAN address) need
    /// `insecure`: the links are plain HTTP.
    pub listen: Option<String>,
    /// Allow `listen` on an address that isn't loopback or Tailscale.
    pub insecure: bool,
    /// The URL links point at (default `http://<listen>`).
    pub public_url: Option<String>,
    /// How long an approve/deny link stays valid.
    pub link_ttl: String,
}

impl Default for Callback {
    fn default() -> Self {
        Self { listen: None, insecure: false, public_url: None, link_ttl: "15m".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    /// `bell`, `osc` (OSC 9/777 to your terminal), `desktop`, `sinks`.
    pub channels: Vec<String>,
    /// Which attention kinds notify: blocked, errored, done, idle, exited, notification.
    pub on: Vec<String>,
    /// `"22:00-07:00"`: nothing notifies in this window (breakthrough kinds still do).
    pub quiet_hours: Option<String>,
    /// At most one notification per session this often.
    pub coalesce: String,
    /// Desktop notifier override (an argv, run without a shell); `{title}` and `{body}` are
    /// replaced with agent-controlled text, so never pass them through `sh -c`.
    pub desktop_command: Option<Vec<String>>,
    pub sinks: Vec<SinkDef>,
    pub callback: Callback,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            channels: ["osc", "bell"].map(String::from).into(),
            on: ["blocked", "errored", "done", "idle"].map(String::from).into(),
            quiet_hours: None,
            coalesce: "10s".into(),
            desktop_command: None,
            sinks: Vec::new(),
            callback: Callback::default(),
        }
    }
}

/// An address links and QR codes point at: `http://` or `https://`, a host, nothing else odd.
fn check_url(u: &str) -> Result<(), String> {
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(|| format!("{u:?} needs http:// or https:// in front (like https://{u})"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.starts_with(':') {
        return Err(format!("{u:?} has no host (like https://ptyro.example.com)"));
    }
    if u.chars().any(char::is_whitespace) {
        return Err(format!("{u:?} has a space in it"));
    }
    Ok(())
}

/// `"22:00-07:00"` → minutes since midnight (start, end); the window may wrap midnight.
pub fn parse_quiet_hours(s: &str) -> Result<(u16, u16), String> {
    let (a, b) = s.split_once('-').ok_or("expected HH:MM-HH:MM")?;
    let hm = |t: &str| -> Result<u16, String> {
        let (h, m) = t.trim().split_once(':').ok_or_else(|| format!("bad time {t:?}"))?;
        let (h, m): (u16, u16) = (
            h.parse().map_err(|_| format!("bad hour {h:?}"))?,
            m.parse().map_err(|_| format!("bad minute {m:?}"))?,
        );
        if h > 23 || m > 59 {
            return Err(format!("bad time {t:?}"));
        }
        Ok(h * 60 + m)
    };
    Ok((hm(a)?, hm(b)?))
}

/// Whether `now` (minutes since midnight) falls in the quiet window.
pub fn in_quiet_hours(window: (u16, u16), now: u16) -> bool {
    let (a, b) = window;
    if a <= b { now >= a && now < b } else { now >= a || now < b }
}

/// Sections parsed but used only by later milestones (SPEC §13.2).
/// Sections a config may hold before the milestone that uses them (none left since M9).
pub const RESERVED_SECTIONS: &[(&str, &str)] = &[];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: Ui,
    pub daemon: Daemon,
    /// Key overrides: `"key" = "action [args]"`, `"global:key" = …`, `"nav:key" = …` (also
    /// `chat:` in a chat and `chatlist:` in the chat list); the value
    /// `"none"` unbinds a default.
    pub keys: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, ProfileDef>,
    pub worktrees: Worktrees,
    pub discover: Discover,
    /// `default = "nvim"` plus `<name> = { command = [...], inside = bool }`.
    pub editors: BTreeMap<String, EditorEntry>,
    pub compat: Compat,
    /// `default = "claude"` plus `[agents.<name>]` tables.
    pub agents: Agents,
    pub notifications: Notifications,
    pub attention: AttentionConfig,
    pub orchestration: Orchestration,
    pub recorder: Recorder,
    pub remote: Remote,
    pub clipboard: Clipboard,
    pub shell: Shell,
    pub chat: Chat,
    pub kb: Kb,
    pub client: Client,
    pub updates: Updates,
}

/// The knowledge base (SPEC §25.13): everything that happens in ptyro, searchable in one
/// place, kept in `kb.db`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Kb {
    /// Off: nothing is ingested and nothing is offered; what is stored stays.
    pub enabled: bool,
    /// More patterns to redact before anything is stored (regexes), beside the built-in rules
    /// for credentials (§25.7).
    pub redact: Vec<String>,
    /// More paths never to read (globs), beside the built-in list of key and credential files.
    pub exclude: Vec<String>,
    /// Meaning search (SPEC §25.8, §22.10), for the knowledge base and for chats: find things
    /// by what they are about, not only by their words. Switching it on downloads a 31 MB
    /// model once. `[chat] meaning` is its old name, and switches it on too.
    pub meaning: bool,
    /// Agent sessions are kept, from the agents' own transcripts (§25.6.1).
    pub sessions: bool,
    /// New sessions start private: nothing of them is kept (§25.7a).
    pub private: bool,
    /// Agents are given its tools (`recall`, `recall_read`, and `remember`; §25.9).
    pub tools: bool,
    /// Agents may write notes (the `remember` tool, §25.6.4).
    pub remember: bool,
    /// What an agent may see of it (the wall, §25.9): `profile` (its own account's, and what is
    /// nobody's), `workspace` (of those, its workspace's) or `all`. `[chat] history_scope` is
    /// its old name.
    pub scope: String,
    /// Sessions older than this many days are deleted, once a day (0: never; §25.12).
    pub retain_days: u32,
    /// The largest file read (`1MiB`); a larger one is named in `ptyro kb status`.
    pub max_file: String,
}

impl Default for Kb {
    fn default() -> Self {
        Self {
            enabled: true,
            redact: Vec::new(),
            exclude: Vec::new(),
            meaning: false,
            sessions: true,
            private: false,
            tools: true,
            remember: true,
            scope: "profile".into(),
            retain_days: 0,
            max_file: "1MiB".into(),
        }
    }
}

/// The words `kb.scope` takes.
pub const KB_SCOPES: [&str; 3] = ["profile", "workspace", "all"];

impl Kb {
    fn validate(&self) -> Result<(), String> {
        for p in &self.redact {
            crate::kb::redact::pattern(p).map_err(|e| format!("kb.redact: {e}"))?;
        }
        if !KB_SCOPES.contains(&self.scope.as_str()) {
            return Err(format!("kb.scope: {:?} (profile, workspace or all)", self.scope));
        }
        match parse_bytes(&self.max_file) {
            Ok(0) => Err("kb.max_file: more than 0 (like 1MiB)".into()),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("kb.max_file: {e}")),
        }
    }

    /// `kb.max_file` in bytes.
    pub fn max_file_bytes(&self) -> u64 {
        parse_bytes(&self.max_file).unwrap_or(crate::kb::ingest::MAX_FILE)
    }
}

/// Newer versions of your agents (BOOT5, SPEC §24.7): looked for at most once a day, offered,
/// never installed without you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Updates {
    /// `daily`: the daemon looks for newer versions once a day (`doctor` looks afresh); `off`:
    /// nothing is fetched, and nothing is offered.
    pub check: UpdateCheck,
    /// Updates not to offer: an agent (`codex`, every version) or one version (`codex@0.170.0`).
    pub ignore: Vec<String>,
    /// Agents kept the way they were installed (BOOT7: `codex`): the move to the vendor's
    /// recommended installer isn't offered again, and they update the way they're installed.
    pub keep_installer: Vec<String>,
    /// A desktop notification once per new ptyro release (SPEC §24.8), outside quiet hours.
    pub notify: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check: UpdateCheck::default(),
            ignore: Vec::new(),
            keep_installer: Vec::new(),
            notify: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateCheck {
    #[default]
    Daily,
    Off,
}

impl Updates {
    fn validate(&self) -> Result<(), String> {
        for i in &self.ignore {
            let (name, version) = match i.split_once('@') {
                Some((n, v)) => (n, Some(v)),
                None => (i.as_str(), None),
            };
            if name.trim().is_empty()
                || i.contains(char::is_whitespace)
                || version.is_some_and(|v| v.trim().is_empty())
            {
                return Err(format!(
                    "updates.ignore: {i:?} (an agent, like \"codex\", or one version of it, like \"codex@0.170.0\")"
                ));
            }
        }
        for k in &self.keep_installer {
            if k.trim().is_empty() || k.contains(char::is_whitespace) || k.contains('@') {
                return Err(format!(
                    "updates.keep_installer: {k:?} (an agent's name, like \"codex\")"
                ));
            }
        }
        Ok(())
    }
}

/// This ptyro as a client: where its daemon is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Client {
    /// The daemon to use when `--remote` and `PTYRO_REMOTE` don't say: an ssh host
    /// (`[ssh://][user@]host[:port]`), or `local`: this computer's own daemon. (`wsl://…`, which
    /// Windows' own ptyro used, was removed on 2026-11-18: `ptyro doctor` takes it out.)
    pub default_remote: Option<String>,
}

impl Client {
    fn validate(&self) -> Result<(), String> {
        match self.default_remote.as_deref().map(str::trim) {
            Some(r) if r.is_empty() || r.contains(char::is_whitespace) || r.starts_with('-') => {
                Err(format!("client.default_remote: {r:?} (an ssh host, or local)"))
            }
            _ => Ok(()),
        }
    }
}

/// Chats (SPEC §22.13): plain conversations with your agents, kept in `chat.db`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chat {
    /// Off hides the CHATS section, the keys, the CLI group and the web page; chats stay on disk.
    pub enabled: bool,
    /// Chats shown under CHATS in the sidebar, after the pinned ones.
    pub recent: u32,
    /// The agent for new chats (`claude`, `codex@work`…); "" = the workspace's default agent,
    /// else the first installed of claude, codex and opencode.
    pub agent: String,
    /// New chats start incognito: nothing of them is written, and they end with their agent.
    pub incognito: bool,
    /// Chats can look up your other chats (`chat_search`, `chat_read`, `chat_list`).
    pub history_tool: bool,
    /// The old name of `[kb] scope` (SPEC §25.13): read from a file that still has it and has
    /// no `[kb] scope`, and folded into that one when the file is parsed. Nothing else reads it.
    #[serde(skip_serializing)]
    pub history_scope: Option<String>,
    /// An idle chat's agent stops after this long; the next message carries on.
    pub hibernate_after: String,
    /// The most a replayed conversation carries when an agent has to be told it again.
    pub seed_tokens: u32,
    /// `auto`: a short title from a small model after the first answer; `off`: the first line.
    pub title: String,
    /// The titler: reads the prompt on stdin, prints the title (or Claude's
    /// `--output-format json`), under the chat's profile. Empty: titles are the first line.
    pub title_command: Vec<String>,
    /// The most titles made for each profile in a day.
    pub daily_cap: u32,
    /// Also make tool output searchable (2 KB of it a message).
    pub index_tools: bool,
    /// The old name of `[kb] meaning` (SPEC §25.13): read from a file that still has it, and
    /// folded into that one when the file is parsed. Nothing else reads it.
    #[serde(skip_serializing)]
    pub meaning: bool,
    /// `blocked`: tell you when a chat needs you; `all`: also when an answer arrives while no
    /// screen shows it; `off`.
    pub notify: String,
    /// Delete archived chats after this many days (0 = keep them).
    pub archive_purge_days: u32,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            enabled: true,
            recent: 5,
            agent: String::new(),
            incognito: false,
            history_tool: true,
            history_scope: None,
            hibernate_after: "2m".into(),
            seed_tokens: 30_000,
            title: "auto".into(),
            title_command: [
                "claude",
                "-p",
                "--model",
                "haiku",
                "--output-format",
                "json",
                "--strict-mcp-config",
                "--tools",
                "",
            ]
            .map(str::to_owned)
            .to_vec(),
            daily_cap: 200,
            index_tools: false,
            meaning: false,
            notify: "blocked".into(),
            archive_purge_days: 0,
        }
    }
}

/// What a chat may use (SPEC §22.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatTools {
    /// The agent's own web search and fetch.
    pub web: bool,
    /// Read files in the chat's folders.
    pub read: bool,
}

impl Chat {
    /// `chat find`'s tools (its history tools and nothing else).
    pub const FIND: &'static str = "find";

    /// What a chat may use, by the `tools` its row keeps: web search, and its folders when it
    /// has some. That is `chat` (what a chat keeps now), or `read`, the other built-in preset
    /// that had web search before presets were removed (2026-11-17). Any other name (`none`, a
    /// preset of your own, which may have had no web search) keeps none, so no chat gains web
    /// search on an upgrade; and `find` has only what its launch adds.
    pub fn tools_for(tools: &str, has_folders: bool) -> ChatTools {
        match tools {
            "chat" | "read" => ChatTools { web: true, read: has_folders },
            _ => ChatTools::default(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        parse_duration(&self.hibernate_after).map_err(|e| format!("chat.hibernate_after: {e}"))?;
        if !matches!(self.title.as_str(), "auto" | "off") {
            return Err(format!("chat.title: {:?} (auto or off)", self.title));
        }
        if let Some(s) = &self.history_scope
            && !KB_SCOPES.contains(&s.as_str())
        {
            return Err(format!("chat.history_scope: {s:?} (profile, workspace or all)"));
        }
        if !matches!(self.notify.as_str(), "blocked" | "all" | "off") {
            return Err(format!("chat.notify: {:?} (blocked, all or off)", self.notify));
        }
        if !(1..=100).contains(&self.recent) {
            return Err("chat.recent: 1 to 100".into());
        }
        Ok(())
    }
}

/// Turn `[remote] serve` on or off in the config file (`ptyro web`, and the daemon's own
/// `Request::Web`; WEB1.9), keeping the rest of the file as it is. When turning it on, `access`
/// (`localhost`, `tailscale` or `open`) says who can reach it; left out, the file's own
/// `[remote] access` stands, else Tailscale when it is up on this machine, else this computer
/// only (the default, so nothing is written and a Tailscale set up later is taken next time). A file still written with the old keys (`listen`, `tailscale`, `local`) is moved to
/// `access` and `port` on the way. Returns what that chose, for the person to read.
pub fn set_serve(path: &Path, on: bool, access: Option<&str>) -> Result<Vec<String>, crate::Error> {
    // Only a file that isn't there is empty: one that can't be read (permissions, not UTF-8) must
    // not be replaced by a near-empty config.
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(crate::Error::internal(format!(
                "{}: {e} (it is left as it is)",
                path.display()
            )));
        }
    };
    let (migrated, mut notes) = match migrate_remote(&text) {
        Some((t, notes)) => (t, notes),
        None => (text.clone(), Vec::new()),
    };
    let mut doc: toml_edit::DocumentMut =
        migrated.parse().map_err(|e| crate::Error::invalid(format!("{}: {e}", path.display())))?;
    let remote = doc.entry("remote").or_insert(toml_edit::table());
    // (`remote = { … }` is a table too.)
    let Some(remote) = remote.as_table_like_mut() else {
        return Err(crate::Error::invalid(format!("{}: [remote] is not a table", path.display())));
    };
    // Said before anything is written: a word that isn't one leaves the file as it is.
    let asked = access
        .map(str::parse::<Access>)
        .transpose()
        .map_err(|e| crate::Error::invalid(format!("who can reach it? {e}")))?;
    remote.insert("serve", toml_edit::value(on));
    if on {
        let pick = match (asked, remote.contains_key("access")) {
            (Some(a), _) => Some(a),
            (None, true) => None,
            (None, false) if tailscale_running() => Some(Access::Tailscale),
            (None, false) => {
                notes.push(format!(
                    "reachable from {} (Tailscale isn't running here), over HTTPS; for a phone, set up Tailscale and `ptyro web on` again, or `ptyro web on --access open`",
                    Access::Localhost.blurb()
                ));
                None
            }
        };
        if let Some(pick) = pick {
            remote.insert("access", toml_edit::value(pick.as_str()));
            notes.push(format!(
                "reachable from {} (`[remote] access = \"{pick}\"`), over HTTPS{}",
                pick.blurb(),
                if pick == Access::Localhost {
                    "; for a phone, `ptyro web on --access tailscale` or `--access open`"
                } else {
                    "; the first time, trust ptyro's certificate from /ca.crt"
                }
            ));
        }
    }
    let new = doc.to_string();
    Config::parse(&new)
        .map_err(|e| crate::Error::invalid(format!("the config would be invalid: {e}")))?;
    if new != text {
        write_private(path, &new)?;
    }
    Ok(notes)
}

/// This machine's name on the tailnet if Tailscale can issue a certificate for it, else why not,
/// worded so the person can fix it.
pub fn tailnet_cert_domain() -> Result<String, String> {
    let out = crate::os::tailscale()
        .args(["status", "--json"])
        .output()
        .map_err(|_| "Tailscale isn't installed".to_owned())?;
    let status: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "Tailscale isn't running on this computer".to_owned())?;
    tailnet_cert_domain_from(&status)
}

/// [`tailnet_cert_domain`] from `tailscale status --json`.
pub fn tailnet_cert_domain_from(status: &serde_json::Value) -> Result<String, String> {
    if status["BackendState"] != "Running" {
        return Err("Tailscale isn't connected on this computer".into());
    }
    let own = status["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_lowercase();
    status["CertDomains"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str())
        .map(str::to_lowercase)
        .find(|d| !own.is_empty() && *d == own)
        .ok_or_else(|| {
            "your tailnet has no HTTPS certificates yet: turn on \"HTTPS Certificates\" at \
             https://login.tailscale.com/admin/dns"
                .to_owned()
        })
}

/// Tailscale is installed and connected on this machine.
pub(crate) fn tailscale_running() -> bool {
    crate::os::tailscale()
        .args(["status", "--json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .is_some_and(|v| v["BackendState"] == "Running")
}

/// Write a file only its owner reads (a config can hold keys), replacing it whole: to a temp
/// file of its own name beside it, synced, then renamed over it, so a crash leaves the old file
/// or the new one and two writers never share a temp file. A symlinked file (dotfiles managers)
/// stays a symlink: the file it points at is what is replaced.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<(), crate::Error> {
    use std::io::Write;
    let io = |p: &Path, e: std::io::Error| crate::Error::internal(format!("{}: {e}", p.display()));
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = target.with_extension(format!(
        "toml.{}.{}.tmp",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let write = || -> Result<(), crate::Error> {
        let mut f = crate::os::private(std::fs::OpenOptions::new().write(true).create_new(true))
            .open(&tmp)
            .map_err(|e| io(&tmp, e))?;
        f.write_all(text.as_bytes()).map_err(|e| io(&tmp, e))?;
        f.sync_all().map_err(|e| io(&tmp, e))?;
        std::fs::rename(&tmp, &target).map_err(|e| io(&target, e))
    };
    let r = write();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// OSC 52 from programs in sessions (SPEC §5.7, §17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clipboard {
    /// A program's clipboard writes reach your terminal's clipboard (at most 1 MiB; reads are
    /// always denied).
    pub allow_write: bool,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { allow_write: true }
    }
}

/// Shell sessions (SPEC §8.7, ROADMAP WIN1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    /// The shell new shell sessions run, like `["pwsh", "-Login"]`; empty = your login shell
    /// (`$SHELL -l`).
    pub command: Vec<String>,
    /// Add ptyro's shell integration to the bash, zsh, fish and PowerShell sessions it starts:
    /// prompt marks, working/idle, the last command and its exit code, the directory. Off, they
    /// have none (unless the rc file line `ptyro setup` adds gives them).
    pub integration: bool,
    /// Each workspace's SHELLS section in the sidebar (SPEC §23.6): always, only once it has a
    /// shell, or never (the keys still work).
    pub section: ShellsSection,
    /// A command in a SHELLS shell that ran at least this long tells you when it ends, when
    /// you weren't looking (SPEC §23.9); `off` never.
    pub notify_after: String,
    /// After the daemon restarts (not an upgrade), what becomes of SHELLS (SPEC §23.10).
    pub restore: ShellsRestore,
    /// `Alt-1`…`Alt-9` and `Alt-0` show SHELLS shells 1–10 with no prefix (SPEC §23.6). Off
    /// leaves them to the program in the pane; `C-q e N` still works.
    pub alt_digits: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            integration: true,
            section: ShellsSection::Always,
            notify_after: "30s".into(),
            restore: ShellsRestore::Offer,
            alt_digits: true,
        }
    }
}

impl Shell {
    /// `notify_after` as a duration; `None` when it is off (or unreadable: `config check` says).
    pub fn notify_after(&self) -> Option<std::time::Duration> {
        parse_duration(&self.notify_after).ok().filter(|d| !d.is_zero())
    }
}

/// When a workspace's SHELLS section shows in the sidebar (`[shell] section`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellsSection {
    /// Always, with `＋ new shell` when it has none.
    #[default]
    Always,
    /// Only once the workspace has a shell there.
    WhenUsed,
    /// Never (the keys and commands still work).
    Never,
}

/// What becomes of SHELLS after a daemon restart (`[shell] restore`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellsRestore {
    /// Each comes back as a row in its number that ⏎ starts again in its last directory.
    #[default]
    Offer,
    /// Shells start again by themselves; commands are only offered.
    Start,
    /// They're gone.
    Forget,
}

/// The recorder (SPEC §7.6, M8.2): every session's output, with timing, under
/// `data/sessions/<id>/pty.log` (files 0600). Replay, export, and scrollback deeper than memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recorder {
    pub enabled: bool,
    /// Per session, both ring segments together (`16MiB`, `512KiB`, `1GiB`, or bytes).
    pub max_bytes: String,
}

impl Default for Recorder {
    fn default() -> Self {
        Self { enabled: true, max_bytes: "16MiB".into() }
    }
}

impl Recorder {
    pub fn max_bytes(&self) -> Result<u64, String> {
        parse_bytes(&self.max_bytes)
    }
}

/// Remote access (SPEC §16, M9): `ptyro serve`, paired devices, Web Push, remote approvals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Remote {
    /// Who can reach the web app: `localhost` (this computer only), `tailscale` (this computer and
    /// your tailnet) or `open` (every network interface). Always HTTPS, on `port`.
    pub access: Access,
    /// The port the web app listens on, whichever `access` says.
    pub port: u16,
    /// The address QR codes and links point at (default: this machine's name or address for
    /// `access`, with `port`).
    pub public_url: Option<String>,
    /// The daemon runs `ptyro serve` itself (and restarts it).
    pub serve: bool,
    /// Your own certificate (a PEM chain, leaf first) and its key, instead of the one ptyro
    /// makes: phones that trust its issuer need nothing installed (`--cert`, `--key`).
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    /// How long Claude's permission prompt waits for a phone's answer (its own dialog stays
    /// open the whole time; the first answer wins).
    pub approval_wait: String,
    /// Web Push to paired devices that asked for it, for the attention kinds in
    /// `notifications.on`.
    pub push: bool,
    /// More push services a device may subscribe with, besides the browsers' own (a
    /// self-hosted UnifiedPush or ntfy server: `push_hosts = ["ntfy.example.com"]`).
    pub push_hosts: Vec<String>,
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            access: Access::Localhost,
            port: DEFAULT_PORT,
            public_url: None,
            serve: false,
            tls_cert: None,
            tls_key: None,
            approval_wait: "10m".into(),
            push: true,
            push_hosts: Vec::new(),
        }
    }
}

/// The web app's port unless `[remote] port` says otherwise.
pub const DEFAULT_PORT: u16 = 7091;

/// Who can reach the web app (`[remote] access`). It is always HTTPS; this only says on which
/// network interfaces it listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// This computer only (127.0.0.1).
    #[default]
    Localhost,
    /// This computer and your tailnet: loopback and this machine's Tailscale addresses, nothing
    /// else, so the Wi-Fi never sees it.
    Tailscale,
    /// Every network interface (0.0.0.0): the LAN, and anything else that can reach this
    /// computer. Devices still need a pairing token.
    Open,
}

impl Access {
    pub const ALL: [Access; 3] = [Access::Localhost, Access::Tailscale, Access::Open];

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Localhost => "localhost",
            Access::Tailscale => "tailscale",
            Access::Open => "open",
        }
    }

    /// One line a person decides by.
    pub fn blurb(self) -> &'static str {
        match self {
            Access::Localhost => "this computer only",
            Access::Tailscale => "this computer and your Tailscale network",
            Access::Open => {
                "every network interface: the Wi-Fi or LAN, and anything else that reaches this computer"
            }
        }
    }
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Access {
    type Err = String;
    /// `localhost`, `tailscale`, `open`; the words of the old `--via` (`here`, `lan`) still work.
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "localhost" | "here" => Ok(Access::Localhost),
            "tailscale" => Ok(Access::Tailscale),
            "open" | "lan" => Ok(Access::Open),
            other => Err(format!("{other:?}: localhost, tailscale or open")),
        }
    }
}

/// The keys `[remote]` had before `access` and `port` (they were `listen`, `tailscale`, `local`,
/// `insecure` and `trusted_proxy`: five settings that had to agree). A file that still has them
/// is read as what they meant, and `ptyro config check` says so.
const LEGACY_REMOTE_KEYS: [&str; 5] = ["listen", "tailscale", "local", "insecure", "trusted_proxy"];

/// The titler an older config chose with the retired `[summaries] command` (perhaps a model of
/// the person's own rather than Claude), while `[chat] title_command` chooses none: before
/// 2026-11-17 chats were titled by it when `title_command` was unset or empty, and they still
/// are, until `ptyro doctor` moves it into `title_command`. `None` when the file doesn't set it.
pub fn old_titler(text: &str) -> Option<Vec<String>> {
    let t: toml::Table = toml::from_str(text).ok()?;
    let argv = t.get("summaries")?.get("command")?.as_array()?;
    let argv: Vec<String> =
        argv.iter().map(|v| v.as_str().map(str::to_owned)).collect::<Option<_>>()?;
    let chosen = t
        .get("chat")
        .and_then(|c| c.get("title_command"))
        .and_then(toml::Value::as_array)
        .is_some_and(|a| !a.is_empty());
    (!chosen).then_some(argv)
}

/// A config's text with the old `[remote]` keys turned into `access` and `port` (unless the file
/// already has them), and a note for each key that was dropped. `None` when it has none.
pub fn migrate_remote(text: &str) -> Option<(String, Vec<String>)> {
    let mut doc: toml_edit::DocumentMut = text.parse().ok()?;
    let remote = doc.get_mut("remote")?.as_table_like_mut()?;
    if !LEGACY_REMOTE_KEYS.iter().any(|k| remote.contains_key(k)) {
        return None;
    }
    let flag = |r: &dyn toml_edit::TableLike, k: &str| {
        r.get(k).and_then(toml_edit::Item::as_bool).unwrap_or(false)
    };
    let listen = remote.get("listen").and_then(|v| v.as_str()).map(str::to_owned);
    let mut notes = Vec::new();
    if !remote.contains_key("access") {
        let addr = listen.as_deref().and_then(|l| l.parse::<std::net::SocketAddr>().ok());
        let access = if flag(remote, "local") || addr.is_some_and(|a| a.ip().is_unspecified()) {
            Access::Open
        } else if flag(remote, "tailscale") || addr.is_some_and(|a| !a.ip().is_loopback()) {
            Access::Tailscale
        } else {
            Access::Localhost
        };
        remote.insert("access", toml_edit::value(access.as_str()));
        notes.push(format!("remote.access = \"{access}\" (from the old keys)"));
    }
    if !remote.contains_key("port")
        && let Some(port) =
            listen.as_deref().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse::<u16>().ok())
        && port != DEFAULT_PORT
    {
        remote.insert("port", toml_edit::value(i64::from(port)));
        notes.push(format!("remote.port = {port} (from `listen`)"));
    }
    for k in LEGACY_REMOTE_KEYS {
        if remote.remove(k).is_some() {
            notes.push(format!(
                "remote.{k} is gone: {}",
                match k {
                    "insecure" | "trusted_proxy" => "the web app is always HTTPS now",
                    _ => "`access` and `port` say it now",
                }
            ));
        }
    }
    Some((doc.to_string(), notes))
}

/// Where a plain-HTTP listener (`ptyro serve`, the approve/deny links) may listen without an
/// explicit `insecure`: loopback, or Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48), which
/// WireGuard encrypts. Anywhere else, what it carries would cross the network in clear.
pub fn safe_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            v.is_loopback() || (v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v) => v.is_loopback() || v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// `16MiB`, `512KiB`, `2GB`, `4096`.
pub fn parse_bytes(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n.parse().map_err(|_| format!("bad size {s:?} (e.g. 16MiB)"))?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return Err(format!("bad size unit in {s:?} (KiB, MiB, GiB)")),
    };
    n.checked_mul(mult).ok_or_else(|| format!("size {s:?} is too large"))
}

/// Checkpoints (SPEC §15.3). The table keeps its name from when it also held the orchestration
/// settings (runs, mail, leases, the radar; removed 2026-11-17, see `retired.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Orchestration {
    /// Checkpoint every agent turn that changed its worktree (`refs/ptyro/ckpt/…`).
    pub checkpoints: bool,
}

impl Default for Orchestration {
    fn default() -> Self {
        Self { checkpoints: true }
    }
}

impl Config {
    pub fn checkpoints(&self) -> bool {
        self.orchestration.checkpoints
    }
}

/// Where new worktrees go (SPEC §10.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeLocation {
    /// `<data>/worktrees/<project>/<name>`
    #[default]
    Data,
    /// `<repo>/../<repo>.worktrees/<name>`
    Sibling,
    /// `<repo>/.ptyro/worktrees/<name>` (excluded via `.git/info/exclude`)
    InRepo,
}

/// When agents asked for with a new worktree start (SPEC §10.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartAgents {
    #[default]
    AfterSetup,
    Immediately,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Worktrees {
    pub location: WorktreeLocation,
    /// `{name}`, `{user}`, `{date}`, `{issue}`, `{slug}` (SPEC §10.3).
    pub branch_template: String,
    pub start_agents: StartAgents,
    /// `git fetch` the base before creating a worktree.
    pub fetch: bool,
    /// Refresh repository status on file changes (else every few seconds).
    pub watch: bool,
    /// `ptyro gc` reports worktrees without sessions for this many days.
    pub stale_days: u32,
}

impl Default for Worktrees {
    fn default() -> Self {
        Self {
            location: WorktreeLocation::Data,
            branch_template: "{name}".into(),
            start_agents: StartAgents::AfterSetup,
            fetch: true,
            watch: true,
            stale_days: 14,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Discover {
    /// Where to look for git repositories (SPEC §10.1).
    pub roots: Vec<String>,
    pub depth: u8,
    /// Register the repository a session runs in when it isn't a project yet (UX1.5).
    pub auto_register: bool,
}

impl Default for Discover {
    fn default() -> Self {
        Self {
            roots: ["~/src", "~/dev", "~/code", "~/projects"].map(String::from).into(),
            depth: 3,
            auto_register: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Compat {
    /// Also export `CONDUCTOR_*` variables so existing setup scripts run unchanged (SPEC §10.6).
    pub conductor_env: bool,
}

/// An `[editors]` entry: `default = "<name>"`, or an editor definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EditorEntry {
    Name(String),
    Def(EditorDef),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDef {
    /// argv; `{path}` is replaced by the worktree path, `{winpath}` by the same path as Windows
    /// programs name it inside WSL (`\\wsl.localhost\Ubuntu\home\me\src`).
    pub command: Vec<String>,
    /// Open as a ptyro session instead of a separate window (terminal editors).
    #[serde(default)]
    pub inside: bool,
}

impl EditorDef {
    /// The command line opening `path`: `{path}` and `{winpath}` filled in, or the path added at
    /// the end when the command names neither.
    pub fn argv(&self, path: &Path) -> Vec<String> {
        let p = path.to_string_lossy();
        let win = crate::wsl::detect().and_then(|w| w.windows_path(path));
        let win = win.as_deref().unwrap_or(&p);
        let mut argv: Vec<String> = self
            .command
            .iter()
            .map(|a| a.replace("{path}", &p).replace("{winpath}", win))
            .collect();
        if !self.command.iter().any(|a| a.contains("{path}") || a.contains("{winpath}")) {
            argv.push(p.into_owned());
        }
        argv
    }
}

/// What a profile does when a spawn targets a repository another profile claims (SPEC §10.10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guard {
    #[default]
    Confirm,
    Refuse,
    Warn,
}

/// A secret: a literal (discouraged), or a command whose stdout is the value, run at each spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Secret {
    Literal(String),
    Command { command: Vec<String> },
}

/// An agent (or `gh`) account: a config directory, or a table with a directory and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Account {
    Dir(String),
    Def(AccountDef),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountDef {
    pub dir: Option<String>,
    /// `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`/`CODEX_API_KEY`.
    pub api_key: Option<Secret>,
    /// `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) or `GH_TOKEN`.
    pub token: Option<Secret>,
}

impl Account {
    pub fn dir(&self) -> Option<&str> {
        match self {
            Self::Dir(d) => Some(d),
            Self::Def(d) => d.dir.as_deref(),
        }
    }

    pub fn def(&self) -> Option<&AccountDef> {
        match self {
            Self::Dir(_) => None,
            Self::Def(d) => Some(d),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
    /// → `GIT_SSH_COMMAND="ssh -i <key> -o IdentitiesOnly=yes"`
    pub ssh_key: Option<String>,
    /// A whole global git config for this profile → `GIT_CONFIG_GLOBAL`.
    pub config: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MatchRules {
    /// Remote URL globs: `github.com:acme/*` (ssh and https spellings both match).
    pub remotes: Vec<String>,
    /// Path globs: `~/work/**`.
    pub paths: Vec<String>,
}

impl MatchRules {
    pub fn is_empty(&self) -> bool {
        self.remotes.is_empty() && self.paths.is_empty()
    }
}

/// One identity bundle (SPEC §10.10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileDef {
    pub label: Option<String>,
    pub color: Option<String>,
    pub claude: Option<Account>,
    pub codex: Option<Account>,
    pub opencode: Option<Account>,
    pub git: Option<GitIdentity>,
    pub gh: Option<Account>,
    pub env: BTreeMap<String, String>,
    pub env_file: Option<String>,
    pub secrets: BTreeMap<String, Secret>,
    #[serde(rename = "match")]
    pub matches: MatchRules,
    /// Entries symlinked from the agent's default dir: `{ claude = ["CLAUDE.md", "skills"] }`.
    pub share: BTreeMap<String, Vec<String>>,
    /// Remove inherited credentials. Default: true (also for `default` once any profile exists).
    pub isolate: Option<bool>,
    pub scrub_extra: Vec<String>,
    pub guard: Guard,
    /// Cache resolved secrets this long (`"10m"`); default: resolve at every spawn.
    pub secret_ttl: Option<String>,
}

/// Agents a profile can carry an account for, with their `share` defaults directory.
pub const PROFILE_AGENTS: &[&str] = &["claude", "codex", "opencode"];

/// Files that hold credentials: never shared between profiles (SPEC §10.10).
pub const CREDENTIAL_FILES: &[&str] =
    &[".credentials.json", ".claude.json", "auth.json", "oauth_creds.json", "credentials.json"];

impl ProfileDef {
    pub fn account(&self, agent: &str) -> Option<&Account> {
        match agent {
            "claude" => self.claude.as_ref(),
            "codex" => self.codex.as_ref(),
            "opencode" => self.opencode.as_ref(),
            "gh" => self.gh.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

impl Config {
    /// `$PTYRO_CONFIG`, else `<config dir>/config.toml`.
    pub fn path(paths: &Paths) -> PathBuf {
        std::env::var_os("PTYRO_CONFIG")
            .filter(|v| !v.is_empty())
            .map_or_else(|| paths.config.join("config.toml"), PathBuf::from)
    }

    /// Load and validate; a missing file means all defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Io { path: path.into(), source }),
        };
        Self::parse(&text).map_err(|message| ConfigError::Invalid { path: path.into(), message })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let migrated = migrate_remote(text);
        let text = migrated.as_ref().map_or(text, |(t, _)| t.as_str());
        // Keys a removed feature had are ignored here, and reported by `config check` and the
        // doctor; any other key it doesn't know is still an error.
        let titler = old_titler(text);
        let stripped = crate::retired::strip(text);
        let text = stripped.as_ref().map_or(text, |(t, _)| t.as_str());
        let mut config: Self =
            toml::from_str(text).map_err(|e| e.to_string().trim_end().to_owned())?;
        if let Some(argv) = titler {
            config.chat.title_command = argv;
        }
        // `[chat] meaning` is `[kb] meaning`'s old name: either switches it on.
        config.kb.meaning |= config.chat.meaning;
        config.validate()?;
        // `[chat] history_scope` is `[kb] scope`'s: it holds while the file has no `[kb] scope`.
        if let Some(old) = config.chat.history_scope.take()
            && !toml::from_str::<toml::Table>(text)
                .is_ok_and(|t| t.get("kb").is_some_and(|kb| kb.get("scope").is_some()))
        {
            config.kb.scope = old;
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        self.client.validate()?;
        self.chat.validate()?;
        self.kb.validate()?;
        self.updates.validate()?;
        parse_duration(&self.shell.notify_after).map_err(|e| format!("shell.notify_after: {e}"))?;
        if self.ui.prefix.keys().is_empty() {
            return Err("ui.prefix: needs at least one key".into());
        }
        for k in self.ui.prefix.keys() {
            parse_key(k).map_err(|e| format!("ui.prefix: {e}"))?;
        }
        // A removed built-in (2026-11-17) loads: using it says why it can't run, and `ptyro
        // doctor` switches the default.
        if let Some(name) = self.agents.default.as_deref().filter(|d| !d.is_empty())
            && !self.agents.contains_key(name)
            && !BUILTIN_AGENTS.contains(&name)
            && removed_agent(&self.agents, name).is_none()
        {
            let mut known: Vec<&str> = BUILTIN_AGENTS.to_vec();
            known.extend(self.agents.keys().map(String::as_str));
            return Err(format!(
                "agents.default: no agent named {name:?} (known: {})",
                known.join(", ")
            ));
        }
        for (name, a) in &self.agents {
            crate::names::validate(name).map_err(|e| format!("agents.{name}: {e}"))?;
            if let Some(h) = &a.hibernate_after {
                parse_duration(h).map_err(|e| format!("agents.{name}.hibernate_after: {e}"))?;
            }
            if matches!(&a.command, Some(CommandSpec::Argv(v)) if v.is_empty()) {
                return Err(format!("agents.{name}.command: must not be empty"));
            }
            // A one-string command is a program name, but check its words too: better a false
            // alarm than a missed `--yolo`.
            let mut argv: Vec<String> = match &a.command {
                Some(CommandSpec::Argv(v)) => v.clone(),
                Some(CommandSpec::Line(l)) => l.split_whitespace().map(str::to_owned).collect(),
                None => Vec::new(),
            };
            argv.extend(a.args.iter().cloned());
            if !a.dangerous
                && let Some(bad) = dangerous_arg(&argv)
            {
                return Err(format!(
                    "agents.{name}: {bad} skips permission prompts; set `dangerous = true` to allow it (such sessions show ⚠)"
                ));
            }
        }
        for (k, v) in [
            ("attention.stale_after", &self.attention.stale_after),
            ("attention.min_work", &self.attention.min_work),
            ("notifications.coalesce", &self.notifications.coalesce),
            ("notifications.callback.link_ttl", &self.notifications.callback.link_ttl),
        ] {
            parse_duration(v).map_err(|e| format!("{k}: {e}"))?;
        }
        if let Some(q) = &self.notifications.quiet_hours {
            parse_quiet_hours(q).map_err(|e| format!("notifications.quiet_hours: {e}"))?;
        }
        for c in &self.notifications.channels {
            // `inbox` went with the inbox (2026-11-17): a list that still names it is read as
            // if it didn't.
            if !["inbox", "bell", "osc", "desktop", "sinks"].contains(&c.as_str()) {
                return Err(format!(
                    "notifications.channels: unknown channel {c:?} (bell, osc, desktop, sinks)"
                ));
            }
        }
        let kinds = self
            .notifications
            .on
            .iter()
            .chain(&self.attention.breakthrough)
            .chain(self.notifications.sinks.iter().flat_map(|s| s.on()));
        for k in kinds {
            // `gate` and `run` went with runs (2026-11-17): a list that still names them is
            // read as if it didn't.
            if crate::status::AttentionKind::parse(k).is_none()
                && !["gate", "run"].contains(&k.as_str())
            {
                return Err(format!(
                    "notifications: unknown kind {k:?} (blocked, errored, done, idle, exited, notification)"
                ));
            }
        }
        if !(12..=500).contains(&self.ui.sidebar_width) {
            return Err("ui.sidebar_width: must be 12 to 500".into());
        }
        if self.discover.depth > 32 {
            return Err("discover.depth: at most 32".into());
        }
        if self.notifications.desktop_command.as_ref().is_some_and(Vec::is_empty) {
            return Err("notifications.desktop_command: must not be empty (leave it out)".into());
        }
        for (i, s) in self.notifications.sinks.iter().enumerate() {
            if matches!(s, SinkDef::Command { command, .. } if command.is_empty()) {
                return Err(format!("notifications.sinks[{i}].command: must not be empty"));
            }
        }
        if !(1..=1000).contains(&self.daemon.frame_interval_ms) {
            return Err("daemon.frame_interval_ms: must be 1–1000".into());
        }
        for (name, a) in &self.agents {
            if let Some(p) = &a.protocol
                && !["stream-json", "app-server", "acp", "opencode"].contains(&p.as_str())
            {
                return Err(format!(
                    "agents.{name}.protocol: {p:?} (stream-json, app-server, acp, opencode)"
                ));
            }
        }
        parse_duration(&self.remote.approval_wait)
            .map_err(|e| format!("remote.approval_wait: {e}"))?;
        if self.remote.port < 1024 {
            return Err("remote.port: 1024 to 65535 (a lower port needs root)".into());
        }
        if let Some(l) = &self.notifications.callback.listen {
            l.parse::<std::net::SocketAddr>().map_err(|e| {
                format!("notifications.callback.listen: {e} (an address like 127.0.0.1:7591)")
            })?;
        }
        for (k, u) in [
            ("remote.public_url", &self.remote.public_url),
            ("notifications.callback.public_url", &self.notifications.callback.public_url),
        ] {
            if let Some(u) = u {
                check_url(u).map_err(|e| format!("{k}: {e}"))?;
            }
        }
        // Each ring half starts with a keyframe (≈ 60 KB at 200×60), so it must hold one.
        if self.recorder.max_bytes().map_err(|e| format!("recorder.max_bytes: {e}"))? < 256 << 10 {
            return Err("recorder.max_bytes: at least 256KiB".into());
        }
        if !["error", "warn", "info", "debug", "trace"]
            .contains(&self.daemon.log_level.trim().to_ascii_lowercase().as_str())
        {
            return Err(format!(
                "daemon.log_level: {:?} (error, warn, info, debug or trace)",
                self.daemon.log_level
            ));
        }
        if self.daemon.scrollback_lines > 1_000_000 {
            return Err("daemon.scrollback_lines: at most 1000000".into());
        }
        for key in self.keys.keys() {
            let spec = ["global:", "nav:", "chat:", "chatlist:"]
                .iter()
                .find_map(|t| key.strip_prefix(t))
                .unwrap_or(key);
            parse_key(spec).map_err(|e| format!("keys.{key:?}: {e}"))?;
        }
        for (name, p) in &self.profiles {
            if name != "default" {
                crate::names::validate(name).map_err(|e| format!("profiles.{name}: {e}"))?;
            }
            p.validate().map_err(|e| format!("profiles.{name}.{e}"))?;
        }
        crate::model::check_branch_template(&self.worktrees.branch_template)
            .map_err(|e| format!("worktrees.branch_template: {e}"))?;
        for (name, e) in &self.editors {
            match (name.as_str(), e) {
                ("default", EditorEntry::Name(n))
                    if !self.editors.contains_key(n)
                        && !BUILTIN_EDITORS.contains(&n.as_str())
                        && !FILE_MANAGERS.contains(&n.as_str())
                        && !TERMINAL_APPS.contains(&n.as_str()) =>
                {
                    return Err(format!("editors.default: no editor named {n:?}"));
                }
                ("default", EditorEntry::Name(_)) => {}
                (_, EditorEntry::Name(_)) => {
                    return Err(format!("editors.{name}: expected {{ command = [...] }}"));
                }
                (_, EditorEntry::Def(d)) if d.command.is_empty() => {
                    return Err(format!("editors.{name}.command: must not be empty"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Reserved sections present in this config (for `config check`).
    pub fn reserved_in_use(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

/// Flags that turn off an agent's permission prompts or sandbox on their own (Claude's, Codex's
/// and other agents' common ones), alone or as `--flag=value`.
const DANGEROUS_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--full-auto",
    "--yolo",
    "-y",
];

/// Options whose value does it: the option's names, and the values (compared without case,
/// quotes or spaces).
const DANGEROUS_VALUES: &[(&[&str], &[&str])] = &[
    (&["--permission-mode"], &["bypasspermissions"]),
    (&["--approval-mode"], &["yolo"]),
    (&["--sandbox", "-s"], &["danger-full-access"]),
    (&["--ask-for-approval", "-a"], &["never"]),
    (&["--config", "-c"], &["approval_policy=never", "sandbox_mode=danger-full-access"]),
];

/// The first argument that turns off an agent's permission prompts or sandbox (it needs
/// `dangerous = true`), as written: `--yolo`, `--permission-mode bypassPermissions`,
/// `--sandbox=danger-full-access`, `-sdanger-full-access`, `-c approval_policy="never"`…
pub fn dangerous_arg(argv: &[String]) -> Option<String> {
    let norm = |v: &str| v.chars().filter(|c| !matches!(c, '"' | '\'' | ' ')).collect::<String>();
    for (i, arg) in argv.iter().enumerate() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f, Some(v)),
            _ => (arg.as_str(), None),
        };
        // `npx -y @acme/agent`: that `-y` is the package runner's own ("yes, install it").
        let runner_flag = flag == "-y"
            && i > 0
            && matches!(
                argv[i - 1].rsplit('/').next().unwrap_or(""),
                "npx" | "bunx" | "pnpx" | "uvx" | "dlx" | "exec"
            );
        if DANGEROUS_FLAGS.contains(&flag) && !runner_flag {
            return Some(arg.clone());
        }
        for (names, bad) in DANGEROUS_VALUES {
            let (value, shown) = if names.contains(&flag) {
                match inline {
                    Some(v) => (v, arg.clone()),
                    None => match argv.get(i + 1) {
                        Some(v) => (v.as_str(), format!("{arg} {v}")),
                        None => continue,
                    },
                }
            } else if let Some(v) = names
                .iter()
                .filter(|n| n.len() == 2)
                .find_map(|n| arg.strip_prefix(n).filter(|v| !v.is_empty()))
            {
                // `-sdanger-full-access`, `-anever`
                (v, arg.clone())
            } else {
                continue;
            };
            if bad.contains(&norm(value).to_ascii_lowercase().as_str()) {
                return Some(shown);
            }
        }
    }
    None
}

/// Editors available without configuration, in the order they're offered: terminal ones open
/// inside ptyro, the rest in their own window; the last is the file manager. Each is offered only
/// where it is installed ([`Config::editor_names`]).
pub const BUILTIN_EDITORS: &[&str] = &["nvim", "vim", "hx", "code", "zed", "cursor", FILE_MANAGER];

/// This computer's file manager, by its own name. Any of `finder`, `explorer` and `files` opens
/// it, so a config shared between computers keeps working.
pub const FILE_MANAGER: &str = if cfg!(target_os = "macos") { "finder" } else { "files" };
const FILE_MANAGERS: [&str; 3] = ["finder", "explorer", "files"];

/// Terminal apps that open a window of their own in the worktree, offered after the editors and
/// the file manager, each only where it is installed (macOS and Linux for now).
pub const TERMINAL_APPS: &[&str] = &[
    "terminal",
    "iterm",
    "ghostty",
    "wezterm",
    "kitty",
    "alacritty",
    "gnome-terminal",
    "kgx",
    "konsole",
    "xfce4-terminal",
    "foot",
];

/// A terminal app's command line opening a window in `{path}`, if it is installed here.
fn terminal_app(name: &str, here: &Installed) -> Option<Vec<String>> {
    let argv = |a: &[&str]| Some(a.iter().map(|&w| w.to_owned()).collect());
    let mac = cfg!(target_os = "macos");
    match name {
        // These open a folder they're given (their Info.plist takes folders, SPEC §20).
        "terminal" | "iterm" | "ghostty" if mac => {
            let app = match name {
                "terminal" => "Terminal",
                "iterm" => "iTerm",
                _ => "Ghostty",
            };
            argv(&["open", "-a", &here.app(app)?.to_string_lossy(), "{path}"])
        }
        "wezterm" | "kitty" | "alacritty" => {
            let flags: &[&str] = match name {
                "wezterm" => &["start", "--cwd", "{path}"],
                "kitty" => &["--directory", "{path}"],
                _ => &["--working-directory", "{path}"],
            };
            let prog = if here.program(name) {
                name.to_owned()
            } else {
                // The program inside the app, on macOS.
                let app = match name {
                    "wezterm" => "WezTerm",
                    "kitty" => "kitty",
                    _ => "Alacritty",
                };
                let p = here.app(app)?.join("Contents/MacOS").join(name);
                crate::os::is_executable(&p).then(|| p.to_string_lossy().into_owned())?
            };
            let mut v = vec![prog];
            v.extend(flags.iter().map(|&f| f.to_owned()));
            Some(v)
        }
        _ if mac || !here.program(name) => None,
        "ghostty" | "gnome-terminal" | "kgx" | "xfce4-terminal" | "foot" => {
            argv(&[name, "--working-directory={path}"])
        }
        "konsole" => argv(&["konsole", "--workdir", "{path}"]),
        _ => None,
    }
}

/// The app a built-in editor also comes as on macOS, where its command is often not on `PATH`
/// (VS Code's `code` is a step of its own to install).
fn mac_app(name: &str) -> Option<&'static str> {
    match name {
        "code" => Some("Visual Studio Code"),
        "zed" => Some("Zed"),
        "cursor" => Some("Cursor"),
        _ => None,
    }
}

/// Where this computer's programs are: `PATH`, on macOS the Applications folders, and inside WSL
/// Windows' own programs.
pub struct Installed {
    path: std::ffi::OsString,
    apps: Vec<PathBuf>,
    wsl: Option<crate::wsl::Wsl>,
    /// Inside WSL, the Windows user's home (`/mnt/c/Users/me`), where VS Code installs itself.
    windows_home: Option<PathBuf>,
}

/// Whether `prog` is an executable in one of `path`'s directories, remembered for a minute.
/// The settings panel asks for every editor each time it redraws its choices, and inside WSL
/// `PATH` ends with Windows' own directories, on a slow mount: there that was seconds a call.
/// The answer is per `PATH`, so a caller with another one (a test) asks afresh.
fn on_path(path: &std::ffi::OsStr, prog: &str) -> bool {
    use crate::sync::MutexExt;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    type Seen = (std::ffi::OsString, Instant, HashMap<String, bool>);
    static SEEN: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
    const FOR: Duration = Duration::from_secs(60);
    {
        let seen = SEEN.locked();
        if let Some((p, at, found)) = seen.as_ref()
            && p == path
            && at.elapsed() < FOR
            && let Some(&f) = found.get(prog)
        {
            return f;
        }
    }
    let names = crate::os::program_names(prog);
    let found = std::env::split_paths(path)
        .any(|d| names.iter().any(|n| crate::os::is_executable(&d.join(n))));
    let mut seen = SEEN.locked();
    match seen.as_mut() {
        Some((p, at, map)) if p == path && at.elapsed() < FOR => {
            map.insert(prog.to_owned(), found);
        }
        _ => {
            *seen =
                Some((path.to_owned(), Instant::now(), HashMap::from([(prog.to_owned(), found)])))
        }
    }
    found
}

impl Installed {
    pub fn here() -> Self {
        let mut apps = Vec::new();
        if cfg!(target_os = "macos") {
            apps.push(PathBuf::from("/Applications"));
            apps.push(PathBuf::from("/System/Applications/Utilities"));
            if let Some(home) = std::env::var_os("HOME") {
                apps.push(PathBuf::from(home).join("Applications"));
            }
        }
        let wsl = crate::wsl::detect();
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            apps,
            windows_home: wsl.and_then(|w| w.windows_home()).map(Path::to_path_buf),
            wsl: wsl.cloned(),
        }
    }

    fn program(&self, prog: &str) -> bool {
        if prog.contains('/') {
            return crate::os::is_executable(Path::new(prog));
        }
        on_path(&self.path, prog)
    }

    fn app(&self, name: &str) -> Option<PathBuf> {
        self.apps.iter().map(|d| d.join(format!("{name}.app"))).find(|p| p.is_dir())
    }

    /// Inside WSL, a Windows editor's command for WSL (`…/Microsoft VS Code/bin/code`), from
    /// `PATH` or where its installer puts it.
    fn windows_editor(&self, name: &str) -> Option<String> {
        if self.program(name) {
            return Some(name.to_owned());
        }
        let home = self.windows_home.as_deref();
        let wsl = self.wsl.as_ref()?;
        let mut places = Vec::new();
        match name {
            "code" => {
                places.extend(
                    home.map(|h| h.join("AppData/Local/Programs/Microsoft VS Code/bin/code")),
                );
                places.push(
                    Path::new(&wsl.mount_root).join("c/Program Files/Microsoft VS Code/bin/code"),
                );
            }
            "cursor" => places.extend(
                home.map(|h| h.join("AppData/Local/Programs/cursor/resources/app/bin/cursor")),
            ),
            _ => {}
        }
        places
            .into_iter()
            .find(|p| crate::os::is_executable(p))
            .map(|p| p.to_string_lossy().into_owned())
    }
}

impl Config {
    /// An editor by name, as this computer runs it: configured, else built in. `None` when it
    /// isn't installed: its program is on no `PATH` (nor, on macOS, an app in Applications).
    pub fn editor(&self, name: &str) -> Option<EditorDef> {
        self.editor_in(name, &Installed::here())
    }

    fn editor_in(&self, name: &str, here: &Installed) -> Option<EditorDef> {
        if let Some(EditorEntry::Def(d)) = self.editors.get(name) {
            return d.command.first().is_some_and(|p| here.program(p)).then(|| d.clone());
        }
        let def = |command: &[&str], inside| {
            Some(EditorDef { command: command.iter().map(|&a| a.to_owned()).collect(), inside })
        };
        match name {
            "nvim" | "vim" | "hx" if here.program(name) => def(&[name, "{path}"], true),
            // VS Code's and Cursor's WSL script opens the folder through their WSL extension only
            // when WSL_DISTRO_NAME is set, and a daemon run by systemd doesn't have it.
            "code" | "cursor" if here.wsl.is_some() => {
                let prog = here.windows_editor(name)?;
                match here.wsl.as_ref().and_then(|w| w.distro.as_deref()) {
                    Some(d) => {
                        def(&["env", &format!("WSL_DISTRO_NAME={d}"), &prog, "{path}"], false)
                    }
                    None => def(&[&prog, "{path}"], false),
                }
            }
            "code" | "zed" | "cursor" if here.program(name) => def(&[name, "{path}"], false),
            "code" | "zed" | "cursor" => {
                let app = here.app(mac_app(name)?)?;
                def(&["open", "-a", &app.to_string_lossy(), "{path}"], false)
            }
            n if TERMINAL_APPS.contains(&n) => {
                Some(EditorDef { command: terminal_app(n, here)?, inside: false })
            }
            n if FILE_MANAGERS.contains(&n) => {
                if cfg!(target_os = "macos") {
                    def(&["open", "{path}"], false)
                } else if let Some(w) = here.wsl.as_ref().filter(|w| w.interop) {
                    // Windows' own file manager, not a Linux one WSL rarely has.
                    def(&[&w.windows_exe("explorer.exe").to_string_lossy(), "{winpath}"], false)
                } else if here.program("xdg-open") {
                    def(&["xdg-open", "{path}"], false)
                } else if here.program("gio") {
                    def(&["gio", "open", "{path}"], false)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// `editors.default`, else `$VISUAL`/`$EDITOR` if it names an editor, else the first
    /// installed one; only one that is installed (`nvim` when none is, for the error to name).
    pub fn default_editor(&self) -> String {
        self.default_editor_in(&Installed::here())
    }

    fn default_editor_in(&self, here: &Installed) -> String {
        let mut want = Vec::new();
        if let Some(EditorEntry::Name(n)) = self.editors.get("default") {
            want.push(n.clone());
        }
        for var in ["VISUAL", "EDITOR"] {
            if let Ok(v) = std::env::var(var) {
                want.push(v.rsplit('/').next().unwrap_or(&v).to_owned());
            }
        }
        want.into_iter()
            .find(|n| self.editor_in(n, here).is_some())
            .or_else(|| self.editor_names_in(here).into_iter().next())
            .unwrap_or_else(|| "nvim".into())
    }

    /// The editors installed here: configured ones, then built-ins, then terminal apps.
    pub fn editor_names(&self) -> Vec<String> {
        self.editor_names_in(&Installed::here())
    }

    fn editor_names_in(&self, here: &Installed) -> Vec<String> {
        let mut v: Vec<String> = self.editors.keys().filter(|k| *k != "default").cloned().collect();
        for b in BUILTIN_EDITORS.iter().chain(TERMINAL_APPS) {
            if !v.iter().any(|n| n == b) {
                v.push((*b).into());
            }
        }
        v.retain(|n| self.editor_in(n, here).is_some());
        v
    }

    /// Why `name` can't open a worktree here, and what can.
    pub fn no_editor(&self, name: &str) -> String {
        let have = self.editor_names().join(", ");
        let known = self.editors.contains_key(name)
            || BUILTIN_EDITORS.contains(&name)
            || FILE_MANAGERS.contains(&name)
            || TERMINAL_APPS.contains(&name);
        if !known {
            return format!("no editor {name:?} (installed: {have})");
        }
        let apps = if cfg!(target_os = "macos") { " or in Applications" } else { "" };
        format!("{name} isn't installed here: not on PATH{apps} (installed: {have})")
    }
}

impl ProfileDef {
    fn validate(&self) -> Result<(), String> {
        if let Some(c) = &self.color {
            parse_color(c).map_err(|e| format!("color: {e}"))?;
        }
        for (var, s) in &self.secrets {
            if let Secret::Command { command } = s
                && command.is_empty()
            {
                return Err(format!("secrets.{var}: command must not be empty"));
            }
        }
        for (agent, entries) in &self.share {
            if !PROFILE_AGENTS.contains(&agent.as_str()) {
                return Err(format!("share.{agent}: not an agent (use one of {PROFILE_AGENTS:?})"));
            }
            for e in entries {
                // As the filesystem reads it: `.credentials.json/`, `x/.` and `.Credentials.json`
                // (a case-insensitive disk) are the same file.
                let leaf = e.split('/').rfind(|c| !c.is_empty() && *c != ".").unwrap_or("");
                if leaf.is_empty() {
                    return Err(format!(
                        "share.{agent}: {e:?} must name something in the agent's dir"
                    ));
                }
                if CREDENTIAL_FILES.iter().any(|c| c.eq_ignore_ascii_case(leaf)) {
                    return Err(format!(
                        "share.{agent}: {e:?} holds credentials and is never shared"
                    ));
                }
                if e.contains("..") || e.starts_with('/') {
                    return Err(format!(
                        "share.{agent}: {e:?} must be a path inside the agent's dir"
                    ));
                }
            }
        }
        if let Some(ttl) = &self.secret_ttl {
            parse_duration(ttl).map_err(|e| format!("secret_ttl: {e}"))?;
        }
        Ok(())
    }
}

/// `"90s"`, `"10m"`, `"2h"`, `"1d"`; `"off"`/`"0"` is zero.
pub fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    if s == "off" || s == "0" {
        return Ok(std::time::Duration::ZERO);
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: u64 = n.parse().map_err(|_| format!("bad duration {s:?} (e.g. 30s, 10m, 2h)"))?;
    let mult: u64 = match unit {
        "s" | "" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("bad duration unit in {s:?} (use s, m, h or d)")),
    };
    // Ten years is longer than anything here means; the cap keeps millisecond arithmetic honest.
    const LONGEST: u64 = 10 * 365 * 86_400;
    let secs = n
        .checked_mul(mult)
        .filter(|s| *s <= LONGEST)
        .ok_or_else(|| format!("duration {s:?} is too long (at most 10 years)"))?;
    Ok(std::time::Duration::from_secs(secs))
}

/// `~` and `~/…` → the home directory.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            // Never a relative path (it would land in whatever directory the daemon is in).
            let home = std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
                .or_else(|| etcetera::home_dir().ok())
                .unwrap_or_else(|| PathBuf::from("/"));
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(p),
    }
}

/// Where fish, PowerShell (off Windows) and systemd read their config: `$XDG_CONFIG_HOME`, or
/// `<home>/.config` when it is unset (or relative, which the XDG spec says to ignore).
pub fn config_home(home: &Path) -> PathBuf {
    config_home_in(home, std::env::var_os("XDG_CONFIG_HOME").as_deref().map(Path::new))
}

/// [`config_home`] with the variable's value given.
pub fn config_home_in(home: &Path, xdg: Option<&Path>) -> PathBuf {
    xdg.filter(|x| x.is_absolute()).map_or_else(|| home.join(".config"), Path::to_path_buf)
}

/// A colour from config: `#rrggbb`, `0`–`255`, or a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub fn parse_color(s: &str) -> Result<ConfigColor, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(v) = u32::from_str_radix(hex, 16)
        {
            return Ok(ConfigColor::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
        }
        return Err(format!("bad colour {s:?} (use #rrggbb)"));
    }
    if let Ok(i) = s.parse::<u8>() {
        return Ok(ConfigColor::Indexed(i));
    }
    const NAMES: [&str; 8] =
        ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    let lower = s.to_ascii_lowercase();
    if lower == "default" || lower == "reset" {
        return Ok(ConfigColor::Default);
    }
    let (bright, base) = match lower.strip_prefix("bright-") {
        Some(b) => (true, b),
        None => (false, lower.as_str()),
    };
    match NAMES.iter().position(|n| *n == base) {
        Some(i) => Ok(ConfigColor::Indexed(i as u8 + if bright { 8 } else { 0 })),
        None => Err(format!("unknown colour {s:?}")),
    }
}

/// Parse a key spec: `x`, `?`, `Enter`, `Space`, `Tab`, `S-Tab`, `C-a`, `M-1`, `C-S-Left`, `F5`.
/// Modifiers: `C-` ctrl, `M-`/`A-` alt, `S-` shift, `D-` super.
pub fn parse_key(spec: &str) -> Result<KeyEvent, String> {
    let mut mods = 0u8;
    let mut rest = spec.trim();
    if rest.is_empty() {
        return Err("empty key".into());
    }
    while let Some((m, tail)) = rest.split_once('-') {
        if tail.is_empty() {
            break; // a bare "-" key, or "C--"
        }
        let bit = match m {
            "C" | "Ctrl" | "ctrl" => Mods::CTRL,
            "M" | "A" | "Alt" | "alt" | "Meta" => Mods::ALT,
            "S" | "Shift" | "shift" => Mods::SHIFT,
            "D" | "Super" | "super" | "Cmd" => Mods::SUPER,
            _ => break,
        };
        mods |= bit;
        rest = tail;
    }
    let code = match rest {
        "Space" | "space" => KeyCode::Char { c: ' ' },
        "Enter" | "Return" | "CR" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "BTab" | "BackTab" => KeyCode::BackTab,
        "Esc" | "Escape" => KeyCode::Esc,
        "BS" | "Backspace" => KeyCode::Backspace,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" | "PgUp" => KeyCode::PageUp,
        "PageDown" | "PgDn" => KeyCode::PageDown,
        "Insert" | "Ins" => KeyCode::Insert,
        "Delete" | "Del" => KeyCode::Delete,
        f if f.len() > 1 && f.starts_with('F') && f[1..].parse::<u8>().is_ok() => {
            let n: u8 = f[1..].parse().unwrap_or(0);
            if !(1..=24).contains(&n) {
                return Err(format!("no such key {f:?}"));
            }
            KeyCode::F { n }
        }
        one if one.chars().count() == 1 => KeyCode::Char { c: one.chars().next().unwrap_or(' ') },
        other => return Err(format!("unknown key {other:?} in {spec:?}")),
    };
    Ok(KeyEvent::new(code, Mods(mods)))
}

/// The canonical spelling of a key, for help screens.
pub fn key_label(k: &KeyEvent) -> String {
    let mut s = String::new();
    for (bit, p) in
        [(Mods::CTRL, "C-"), (Mods::ALT, "M-"), (Mods::SUPER, "D-"), (Mods::SHIFT, "S-")]
    {
        if k.mods.has(bit) {
            s.push_str(p);
        }
    }
    match k.code {
        KeyCode::Char { c: ' ' } => s.push_str("Space"),
        KeyCode::Char { c } => s.push(c),
        KeyCode::F { n } => s.push_str(&format!("F{n}")),
        other => s.push_str(&format!("{other:?}")),
    }
    s
}

/// Compare keys the way users think of them: release/repeat/keypad ignored, and for characters
/// the character already says whether Shift was held (`A`, `?`), so Shift is dropped.
pub fn normalize_key(k: KeyEvent) -> KeyEvent {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_knowledge_base_is_on_and_a_bad_pattern_is_refused() {
        let c = Config::parse("").unwrap();
        assert!(c.kb.enabled && c.kb.redact.is_empty() && c.kb.exclude.is_empty());
        let c = Config::parse(
            "[kb]\nenabled = false\nredact = ['ACME-\\d+']\nexclude = ['private/**']\n",
        )
        .unwrap();
        assert!(!c.kb.enabled);
        assert_eq!((c.kb.redact.len(), c.kb.exclude.len()), (1, 1));
        let e = Config::parse("[kb]\nredact = ['(open']\n").unwrap_err();
        assert!(e.starts_with("kb.redact: \"(open\" is not a pattern"), "{e}");
        // Meaning search is one switch, off until asked for; its old name still switches it on.
        assert!(!c.kb.meaning);
        // The rest of the Knowledge section (KB1.11).
        assert!(c.kb.sessions && c.kb.tools && c.kb.retain_days == 0);
        assert_eq!((c.kb.scope.as_str(), c.kb.max_file_bytes()), ("profile", 1 << 20));
        let c = Config::parse(
            "[kb]\nsessions = false\ntools = false\nscope = 'all'\nretain_days = 90\nmax_file = '64KiB'\n[agents.codex]\nkb = false\n",
        )
        .unwrap();
        assert!(!c.kb.sessions && !c.kb.tools);
        assert_eq!(
            (c.kb.scope.as_str(), c.kb.retain_days, c.kb.max_file_bytes()),
            ("all", 90, 64 << 10)
        );
        assert_eq!(c.agents.get("codex").unwrap().kb, Some(false));
        assert!(Config::parse("[kb]\nscope = 'mine'\n").unwrap_err().starts_with("kb.scope: "));
        assert!(
            Config::parse("[kb]\nmax_file = 'big'\n").unwrap_err().starts_with("kb.max_file: ")
        );
        assert!(Config::parse("[kb]\nmax_file = '0'\n").unwrap_err().starts_with("kb.max_file: "));
        // `[chat] history_scope` is `[kb] scope`'s old name: it holds until the file has the new one.
        assert_eq!(Config::parse("[chat]\nhistory_scope = 'all'\n").unwrap().kb.scope, "all");
        let both = "[chat]\nhistory_scope = 'all'\n[kb]\nscope = 'profile'\n";
        assert_eq!(Config::parse(both).unwrap().kb.scope, "profile");
        assert!(
            Config::parse("[chat]\nhistory_scope = 'x'\n")
                .unwrap_err()
                .starts_with("chat.history_scope: ")
        );
        assert!(Config::parse("[kb]\nmeaning = true\n").unwrap().kb.meaning);
        assert!(Config::parse("[chat]\nmeaning = true\n").unwrap().kb.meaning);
    }

    #[test]
    fn what_a_chat_may_use() {
        assert!(Chat::default().validate().is_ok());
        let web = ChatTools { web: true, read: false };
        assert_eq!(Chat::tools_for("chat", false), web);
        assert_eq!(Chat::tools_for("chat", true), ChatTools { web: true, read: true });
        // A row from before presets went: `read` is the same chat now; `none` and a preset of
        // your own (which may have had no web) stay without tools, and `find` has only its
        // history tools.
        assert_eq!(Chat::tools_for("read", false), web);
        assert_eq!(Chat::tools_for("mine", true), ChatTools::default());
        assert_eq!(Chat::tools_for("none", true), ChatTools::default());
        assert_eq!(Chat::tools_for(Chat::FIND, false), ChatTools::default());
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn serving_the_web_app_is_one_setting_that_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let write = |t: &str| std::fs::write(&path, t).unwrap();
        let read = || std::fs::read_to_string(&path).unwrap();

        // No file yet: one is made, private, with the setting and a way to be reached.
        let notes = set_serve(&path, true, Some("open")).unwrap();
        assert!(notes[0].contains("every network interface"), "{notes:?}");
        let c = Config::load(&path).unwrap();
        assert!(c.remote.serve && c.remote.access == Access::Open);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        // Comments and other sections stay; a way already chosen is kept, not replaced.
        write("# mine\n[ui]\nprefix = \"C-a\" # why not\n[remote]\naccess = \"tailscale\"\n");
        let notes = set_serve(&path, true, None).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let t = read();
        assert!(
            t.contains("# mine") && t.contains("# why not") && t.contains("serve = true"),
            "{t}"
        );
        assert_eq!(Config::load(&path).unwrap().remote.access, Access::Tailscale);

        // Off changes nothing else; asking for one says it, and the old words still work.
        set_serve(&path, false, None).unwrap();
        assert!(!Config::load(&path).unwrap().remote.serve);
        assert_eq!(Config::load(&path).unwrap().remote.access, Access::Tailscale);
        set_serve(&path, true, Some("here")).unwrap();
        let c = Config::load(&path).unwrap();
        assert!(c.remote.serve && c.remote.access == Access::Localhost);
        set_serve(&path, true, Some("lan")).unwrap();
        assert_eq!(Config::load(&path).unwrap().remote.access, Access::Open);

        // A way we don't know is refused before anything is written.
        let before = read();
        assert!(set_serve(&path, true, Some("pigeon")).is_err());
        assert_eq!(read(), before);
    }

    #[test]
    fn dangerous_args_are_caught_in_every_spelling() {
        let argv = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        for (bad, shown) in [
            ("claude --dangerously-skip-permissions", "--dangerously-skip-permissions"),
            ("claude --allow-dangerously-skip-permissions", "--allow-dangerously-skip-permissions"),
            ("claude --permission-mode bypassPermissions", "--permission-mode bypassPermissions"),
            ("claude --permission-mode=bypassPermissions", "--permission-mode=bypassPermissions"),
            (
                "codex --dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-approvals-and-sandbox",
            ),
            ("codex --sandbox danger-full-access", "--sandbox danger-full-access"),
            ("codex --sandbox=danger-full-access", "--sandbox=danger-full-access"),
            ("codex -s danger-full-access", "-s danger-full-access"),
            ("codex -sdanger-full-access", "-sdanger-full-access"),
            ("codex -a never", "-a never"),
            ("codex --ask-for-approval=never", "--ask-for-approval=never"),
            ("codex -c approval_policy=\"never\"", "-c approval_policy=\"never\""),
            (
                "codex --config=sandbox_mode=danger-full-access",
                "--config=sandbox_mode=danger-full-access",
            ),
            ("codex --full-auto", "--full-auto"),
            ("my-agent -y", "-y"),
            ("my-agent --yolo", "--yolo"),
            ("my-agent --approval-mode yolo", "--approval-mode yolo"),
            ("my-agent --approval-mode=YOLO", "--approval-mode=YOLO"),
        ] {
            assert_eq!(dangerous_arg(&argv(bad)).as_deref(), Some(shown), "{bad}");
        }
        for ok in [
            "claude --permission-mode plan",
            "claude --permission-mode",
            "codex -s workspace-write -a on-request",
            "codex -c model=o3",
            "my-agent --approval-mode default",
            "aider --yes-always-not",
            "my-agent --sandbox",
        ] {
            assert_eq!(dangerous_arg(&argv(ok)), None, "{ok}");
        }
    }

    #[test]
    fn safe_addresses() {
        for ok in ["127.0.0.1", "::1", "100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
            assert!(safe_address(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["0.0.0.0", "192.168.1.5", "100.128.0.1", "10.0.0.1", "::"] {
            assert!(!safe_address(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn empty_config_is_all_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn typos_are_precise_errors() {
        let e = Config::parse("[ui]\nsidebar_widht = 3\n").unwrap_err();
        assert!(e.contains("sidebar_widht") && e.contains("line 2"), "{e}");
        let e = Config::parse("[ui]\nprefix = \"C-Spaec\"\n").unwrap_err();
        assert!(e.contains("ui.prefix"), "{e}");
        let e = Config::parse("[ui]\nprefix = [\"C-q\", \"C-Spaec\"]\n").unwrap_err();
        assert!(e.contains("ui.prefix"), "{e}");
        let e = Config::parse("[ui]\nprefix = []\n").unwrap_err();
        assert!(e.contains("ui.prefix"), "{e}");
    }

    #[test]
    fn reserved_sections_are_accepted() {
        let c = Config::parse("[recorder]\nenabled = true\n[remote]\nserve = true\n").unwrap();
        assert!(c.reserved_in_use().is_empty());
        assert!(c.remote.serve);
        assert_eq!((c.remote.access, c.remote.port), (Access::Localhost, 7091));
        assert!(c.recorder.enabled);
        assert_eq!(c.recorder.max_bytes().unwrap(), 16 << 20);
    }

    #[test]
    fn agents_notifications_attention() {
        let c = Config::parse(
            r#"
[daemon]
restore_on_start = "resume"
[agents.claude]
adapter = "claude"
command = "claude"
default = true
hibernate_after = "2h"
[agents.aider]
command = ["aider", "--no-auto-commits"]
[notifications]
channels = ["desktop", "sinks"]
quiet_hours = "22:00-07:00"
sinks = [{ type = "ntfy", url = "https://ntfy.sh/x", on = ["blocked"] }, { type = "command", command = ["notify-me"] }]
callback = { listen = "127.0.0.1:7591" }
[attention]
scope = "global"
"#,
        )
        .unwrap();
        assert_eq!(c.daemon.restore_on_start, RestoreOnStart::Resume);
        assert_eq!(
            c.agents["aider"].command.as_ref().unwrap().argv(),
            ["aider", "--no-auto-commits"]
        );
        assert_eq!(c.notifications.sinks.len(), 2);
        assert_eq!(c.attention.scope, AttentionScope::Global);
        for cmd in [
            r#"command = "claude --dangerously-skip-permissions""#,
            r#"args = ["--permission-mode", "bypassPermissions"]"#,
            r#"command = ["codex", "-s", "danger-full-access"]"#,
        ] {
            let e = Config::parse(&format!("[agents.c]\n{cmd}\n")).unwrap_err();
            assert!(e.contains("dangerous = true"), "{cmd}: {e}");
        }
        let e =
            Config::parse("[agents.c]\nargs = [\"--dangerously-skip-permissions\"]\n").unwrap_err();
        assert!(e.contains("dangerous = true"), "{e}");
        assert!(
            Config::parse(
                "[agents.c]\nargs = [\"--dangerously-skip-permissions\"]\ndangerous = true\n"
            )
            .is_ok()
        );
        assert!(Config::parse("[notifications]\non = [\"sleepy\"]\n").is_err());
        assert!(Config::parse("[notifications]\nquiet_hours = \"25:00-07:00\"\n").is_err());
        let q = parse_quiet_hours("22:00-07:00").unwrap();
        assert!(in_quiet_hours(q, 23 * 60) && in_quiet_hours(q, 60) && !in_quiet_hours(q, 12 * 60));
    }

    #[test]
    fn agents_default_names_an_agent() {
        let c =
            Config::parse("[agents]\ndefault = \"codex\"\n[agents.aider]\ncommand = \"aider\"\n")
                .unwrap();
        assert_eq!(c.agents.default.as_deref(), Some("codex"));
        assert_eq!(c.agents.len(), 1, "default isn't an agent table");
        assert!(
            Config::parse("[agents]\ndefault = \"aider\"\n[agents.aider]\ncommand = \"aider\"\n")
                .is_ok()
        );
        let e = Config::parse("[agents]\ndefault = \"nope\"\n").unwrap_err();
        assert!(e.contains("agents.default") && e.contains("claude"), "{e}");
        // Typos inside an agent's table are still named.
        let e = Config::parse("[agents.aider]\ncomand = \"aider\"\n").unwrap_err();
        assert!(e.contains("comand"), "{e}");
        // It round-trips.
        let back: Agents = toml::from_str(&toml::to_string(&c.agents).unwrap()).unwrap();
        assert_eq!(back, c.agents);
    }

    #[test]
    fn addresses_and_urls_are_checked() {
        let bad = |t: &str| Config::parse(t).unwrap_err();
        let ok = |t: &str| Config::parse(t).unwrap();
        assert!(
            bad("[remote]\npublic_url = \"ptyro.example.com\"\n")
                .contains("https://ptyro.example.com")
        );
        assert!(bad("[remote]\npublic_url = \"https://\"\n").contains("no host"));
        assert!(bad("[remote]\npublic_url = \"https://a b\"\n").contains("space"));
        ok("[remote]\npublic_url = \"https://ptyro.example.com:8443/\"\n");
        let cb = |v: &str| format!("[notifications.callback]\n{v}\n");
        assert!(bad(&cb("listen = \"7591\"")).starts_with("notifications.callback.listen:"));
        assert!(
            bad(&cb("public_url = \"ftp://x\"")).starts_with("notifications.callback.public_url:")
        );
        ok(&cb("listen = \"127.0.0.1:7591\"\npublic_url = \"http://100.64.0.1:7591\""));
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn the_spec_profile_example_parses() {
        let c = Config::parse(
            r#"
[profiles.work]
label = "work"
color = "blue"
claude = "~/.claude-work"
codex  = { dir = "~/.codex-work", api_key = { command = ["op", "read", "op://Work/openai/key"] } }
git    = { name = "Your Name", email = "you@acme.example", ssh_key = "~/.ssh/id_work" }
gh     = "~/.config/gh-work"
env    = { AWS_PROFILE = "acme" }
match  = { remotes = ["github.com:acme/*", "github.com/acme/*"], paths = ["~/work/**"] }
share  = { claude = ["CLAUDE.md", "skills", "agents", "commands"] }
guard = "refuse"

[profiles.personal]
label = "me"
color = "green"
claude = "~/.claude"
match  = { remotes = ["github.com:you/*"] }

[worktrees]
location = "sibling"
branch_template = "{user}/{name}"

[editors]
default = "nvim"
code = { command = ["code", "{path}"] }
"#,
        )
        .unwrap();
        let w = &c.profiles["work"];
        assert_eq!(w.claude.as_ref().and_then(Account::dir), Some("~/.claude-work"));
        assert!(matches!(
            w.codex.as_ref().and_then(Account::def).and_then(|d| d.api_key.as_ref()),
            Some(Secret::Command { .. })
        ));
        assert_eq!(w.guard, Guard::Refuse);
        assert_eq!(c.profiles["personal"].guard, Guard::Confirm);
        assert_eq!(c.worktrees.location, WorktreeLocation::Sibling);
        let (_t, here) = installed(&["nvim", "code"], &[]);
        assert_eq!(c.default_editor_in(&here), "nvim");
        assert!(c.editor_in("nvim", &here).unwrap().inside);
        assert!(!c.editor_in("code", &here).unwrap().inside);
    }

    /// Inside WSL (WIN2): the file manager is Windows' Explorer on the Windows form of the path,
    /// and VS Code is found where its installer puts it, told which distro it is in.
    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn inside_wsl_the_editors_are_windows_ones() {
        use std::os::unix::fs::PermissionsExt;
        let (t, mut here) = installed(&[], &[]);
        let wsl =
            crate::wsl::from_facts("6.6-microsoft-standard-WSL2", Some("Ubuntu"), true, "", "")
                .unwrap();
        here.wsl = Some(wsl.clone());
        let home = t.path().join("Users/me");
        let code = home.join("AppData/Local/Programs/Microsoft VS Code/bin/code");
        std::fs::create_dir_all(code.parent().unwrap()).unwrap();
        std::fs::write(&code, "").unwrap();
        std::fs::set_permissions(&code, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = Config::default();
        assert_eq!(c.editor_in("code", &here), None, "no Windows home known: not found");
        here.windows_home = Some(home);
        let def = c.editor_in("code", &here).unwrap();
        assert_eq!(
            def.command,
            ["env", "WSL_DISTRO_NAME=Ubuntu", &code.to_string_lossy(), "{path}"]
        );
        assert!(!def.inside);
        assert_eq!(c.editor_in("cursor", &here), None, "not installed");
        // WSL is Linux: on macOS (where these facts are only pretend) `open` comes first.
        if !cfg!(target_os = "macos") {
            let files = c.editor_in("files", &here).unwrap();
            assert!(files.command[0].ends_with("explorer.exe"), "{:?}", files.command);
            assert_eq!(files.command[1], "{winpath}");
            // Without interop, Windows programs can't start: no file manager unless Linux has one.
            here.wsl = Some(crate::wsl::Wsl { interop: false, ..wsl });
            assert_eq!(c.editor_in("files", &here), None);
        }
    }

    #[test]
    fn an_editor_command_gets_the_path_once() {
        let def = |c: &[&str]| EditorDef {
            command: c.iter().map(|&a| a.into()).collect(),
            inside: false,
        };
        let p = Path::new("/home/me/src");
        assert_eq!(def(&["hx", "{path}"]).argv(p), ["hx", "/home/me/src"]);
        assert_eq!(def(&["zed"]).argv(p), ["zed", "/home/me/src"]);
        let win = def(&["explorer.exe", "{winpath}"]).argv(p);
        assert_eq!(win.len(), 2, "{win:?}");
        // Inside WSL the Windows form; elsewhere (or with no distro known) the path itself.
        let want = crate::wsl::detect()
            .and_then(|w| w.windows_path(p))
            .unwrap_or_else(|| "/home/me/src".into());
        assert_eq!(win[1], want);
    }

    /// A `PATH` of these programs, and an Applications folder of these apps (gone with the
    /// `TempDir`).
    #[cfg(unix)] // execute bits
    fn installed(programs: &[&str], apps: &[&str]) -> (tempfile::TempDir, Installed) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir(dir.join("bin")).unwrap();
        for p in programs {
            let f = dir.join("bin").join(p);
            std::fs::write(&f, "").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for a in apps {
            std::fs::create_dir_all(dir.join(format!("{a}.app"))).unwrap();
        }
        (
            tmp,
            Installed {
                path: dir.join("bin").into(),
                apps: vec![dir],
                wsl: None,
                windows_home: None,
            },
        )
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn only_installed_editors_are_offered() {
        let c = Config::parse(
            "[editors]\ndefault = \"nvim\"\nmine = { command = [\"/bin/sh\"] }\ngone = { command = [\"nope\"] }\n",
        )
        .unwrap();
        let (_t, here) = installed(&["hx", "code", "xdg-open"], &["Zed"]);
        assert_eq!(c.editor_names_in(&here), ["mine", "hx", "code", "zed", FILE_MANAGER]);
        assert!(c.editor_in("gone", &here).is_none(), "its program is on no PATH");
        assert!(c.editor_in("cursor", &here).is_none());
        // Not on PATH, but an app (macOS): opened as the app.
        let zed = c.editor_in("zed", &here).unwrap();
        assert_eq!(zed.command[..2], ["open", "-a"]);
        assert!(zed.command[2].ends_with("Zed.app") && zed.command[3] == "{path}", "{zed:?}");
        // The default isn't installed: the first one that is.
        assert_eq!(c.default_editor_in(&here), "mine");
        let (_n, none) = installed(&[], &[]);
        let bare = Config::default();
        assert_eq!(
            bare.default_editor_in(&none),
            bare.editor_names_in(&none).first().map_or("nvim", |n| n.as_str())
        );
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn terminal_apps_open_a_window_in_the_worktree() {
        let c = Config::parse("[editors]\ndefault = \"iterm\"\n").unwrap();
        let (t, here) = installed(&["kitty", "wezterm", "xdg-open"], &["iTerm", "Alacritty"]);
        let cmd = |n: &str| c.editor_in(n, &here).map(|d| d.command);
        assert_eq!(cmd("kitty").unwrap(), ["kitty", "--directory", "{path}"]);
        assert_eq!(cmd("wezterm").unwrap(), ["wezterm", "start", "--cwd", "{path}"]);
        assert!(c.editor_in("kitty", &here).is_some_and(|d| !d.inside), "a window of its own");
        // Not on PATH: the program inside the app (macOS), once it is there.
        assert_eq!(cmd("alacritty"), None);
        let bin = t.path().join("Alacritty.app/Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("alacritty"), "").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(bin.join("alacritty"), perm).unwrap();
        let a = cmd("alacritty").unwrap();
        assert!(a[0].ends_with("Alacritty.app/Contents/MacOS/alacritty"), "{a:?}");
        assert_eq!(a[1..], ["--working-directory", "{path}"]);
        // After the editors and the file manager.
        let names = c.editor_names_in(&here);
        let at = |n: &str| names.iter().position(|x| x == n);
        assert!(at(FILE_MANAGER) < at("kitty"), "{names:?}");
        if cfg!(target_os = "macos") {
            let i = cmd("iterm").unwrap();
            assert_eq!(i[..2], ["open", "-a"]);
            assert!(i[2].ends_with("iTerm.app") && i[3] == "{path}", "{i:?}");
            assert_eq!(cmd("ghostty"), None, "no Ghostty.app here");
            assert_eq!(c.default_editor_in(&here), "iterm");
        } else {
            assert_eq!(cmd("iterm"), None, "macOS only");
            let (_l, linux) = installed(&["gnome-terminal", "konsole", "ghostty"], &[]);
            let cmd = |n: &str| c.editor_in(n, &linux).map(|d| d.command);
            assert_eq!(
                cmd("gnome-terminal").unwrap(),
                ["gnome-terminal", "--working-directory={path}"]
            );
            assert_eq!(cmd("ghostty").unwrap(), ["ghostty", "--working-directory={path}"]);
            assert_eq!(cmd("konsole").unwrap(), ["konsole", "--workdir", "{path}"]);
            assert_eq!(cmd("foot"), None);
        }
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn the_file_manager_answers_to_every_name() {
        let (_t, here) = installed(&["xdg-open"], &[]);
        let c = Config::parse("[editors]\ndefault = \"finder\"\n").unwrap();
        let fm = c.editor_in(FILE_MANAGER, &here).unwrap();
        assert!(!fm.inside);
        for n in ["finder", "explorer", "files"] {
            assert_eq!(c.editor_in(n, &here), Some(fm.clone()), "{n}");
        }
        assert_eq!(c.default_editor_in(&here), "finder");
        if cfg!(target_os = "linux") {
            assert_eq!(fm.command, ["xdg-open", "{path}"]);
            let (_g, gio) = installed(&["gio"], &[]);
            assert_eq!(c.editor_in("files", &gio).unwrap().command, ["gio", "open", "{path}"]);
            let (_n, none) = installed(&[], &[]);
            assert!(c.editor_in("files", &none).is_none());
        }
        assert!(Config::parse("[editors]\ndefault = \"thing\"\n").is_err());
    }

    #[test]
    fn profile_mistakes_are_reported() {
        let e = Config::parse("[profiles.work]\nshare = { claude = [\".credentials.json\"] }\n")
            .unwrap_err();
        assert!(e.contains("credentials"), "{e}");
        let e = Config::parse("[profiles.Work]\n").unwrap_err();
        assert!(e.contains("profiles.Work"), "{e}");
        let e = Config::parse("[profiles.w]\nsecret_ttl = \"5 minutes\"\n").unwrap_err();
        assert!(e.contains("secret_ttl"), "{e}");
        let e = Config::parse("[worktrees]\nbranch_template = \"x/{nmae}\"\n").unwrap_err();
        assert!(e.contains("nmae"), "{e}");
        assert_eq!(parse_duration("10m").unwrap().as_secs(), 600);
    }

    #[test]
    fn huge_sizes_and_durations_are_errors_not_overflows() {
        assert!(parse_duration("999999999999999999d").unwrap_err().contains("too long"));
        assert_eq!(parse_duration("2d").unwrap().as_secs(), 172_800);
        assert!(parse_bytes("99999999999999GiB").unwrap_err().contains("too large"));
        assert_eq!(parse_bytes("16MiB").unwrap(), 16 << 20);
    }

    #[test]
    fn key_specs() {
        let k = parse_key("C-Space").unwrap();
        assert_eq!((k.code, k.mods), (KeyCode::Char { c: ' ' }, Mods(Mods::CTRL)));
        assert_eq!(parse_key("M-1").unwrap().mods, Mods(Mods::ALT));
        assert_eq!(parse_key("C-S-Left").unwrap().mods, Mods(Mods::CTRL | Mods::SHIFT));
        assert_eq!(parse_key("-").unwrap().code, KeyCode::Char { c: '-' });
        assert_eq!(parse_key("F12").unwrap().code, KeyCode::F { n: 12 });
        assert!(parse_key("Hyper-x").is_err());
        assert_eq!(key_label(&parse_key("C-a").unwrap()), "C-a");
    }

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000").unwrap(), ConfigColor::Rgb(255, 128, 0));
        assert_eq!(parse_color("bright-blue").unwrap(), ConfigColor::Indexed(12));
        assert_eq!(parse_color("245").unwrap(), ConfigColor::Indexed(245));
        assert!(parse_color("teal-ish").is_err());
    }
}

#[cfg(test)]
mod hardening {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn a_package_runners_yes_flag_is_not_the_agents() {
        for ok in ["npx -y @acme/agent", "bunx -y agent", "pnpm dlx -y agent-cli"] {
            assert_eq!(dangerous_arg(&argv(ok)), None, "{ok}");
        }
        // Anywhere else it still skips prompts.
        assert_eq!(dangerous_arg(&argv("npx @acme/agent -y")).as_deref(), Some("-y"));
        assert_eq!(dangerous_arg(&argv("agent -y")).as_deref(), Some("-y"));
        assert!(
            Config::parse("[agents.mine]\ncommand = [\"npx\", \"-y\", \"@acme/agent\"]\n").is_ok()
        );
    }

    #[test]
    fn durations_and_numbers_are_bounded() {
        assert!(parse_duration("3650d").is_ok());
        assert!(parse_duration("99999999999999d").unwrap_err().contains("too long"));
        assert!(parse_duration("18446744073709551615s").is_err());
        for (bad, key) in [
            ("[ui]\nsidebar_width = 60000\n", "ui.sidebar_width"),
            ("[discover]\ndepth = 200\n", "discover.depth"),
            ("[notifications]\ndesktop_command = []\n", "notifications.desktop_command"),
            ("[[notifications.sinks]]\ntype = \"command\"\ncommand = []\n", "command"),
        ] {
            let e = Config::parse(bad).unwrap_err();
            assert!(e.contains(key), "{bad}: {e}");
        }
    }

    #[test]
    fn colours_take_only_hex_digits_and_home_is_never_relative() {
        assert!(parse_color("#+fffff").is_err());
        assert!(parse_color("#12345g").is_err());
        assert_eq!(parse_color("#ff0080").unwrap(), ConfigColor::Rgb(255, 0, 128));
        assert!(expand_home("~/x").is_absolute());
    }

    #[cfg(unix)] // Unix paths (the agents' config home is the daemon's)
    #[test]
    fn config_home_follows_xdg_unless_it_is_relative() {
        let home = Path::new("/home/me");
        assert_eq!(config_home_in(home, None), Path::new("/home/me/.config"));
        assert_eq!(config_home_in(home, Some(Path::new("/x/conf"))), Path::new("/x/conf"));
        assert_eq!(config_home_in(home, Some(Path::new("conf"))), Path::new("/home/me/.config"));
        assert_eq!(config_home_in(home, Some(Path::new(""))), Path::new("/home/me/.config"));
    }

    #[test]
    fn sharing_a_credential_file_is_refused_however_it_is_spelled() {
        for spelling in [
            ".credentials.json",
            ".credentials.json/",
            "./.credentials.json",
            "sub/.Credentials.json",
            ".credentials.json/.",
        ] {
            let text = format!("[profiles.work.share]\nclaude = [{spelling:?}]\n");
            let e = Config::parse(&text).unwrap_err();
            assert!(e.contains("credentials"), "{spelling}: {e}");
        }
        assert!(Config::parse("[profiles.work.share]\nclaude = [\".\"]\n").is_err());
        assert!(
            Config::parse("[profiles.work.share]\nclaude = [\"CLAUDE.md\", \"skills\"]\n").is_ok()
        );
    }

    #[test]
    fn the_old_remote_keys_are_read_as_access_and_port() {
        let r = |t: &str| Config::parse(t).unwrap().remote;
        // Nothing set: this computer only, on the default port.
        assert_eq!((r("").access, r("").port), (Access::Localhost, 7091));
        // `tailscale serve` in front, or a tailnet address: the tailnet.
        assert_eq!(r("[remote]\ntailscale = true\n").access, Access::Tailscale);
        assert_eq!(r("[remote]\nlisten = \"100.64.0.7:7091\"\n").access, Access::Tailscale);
        // Every interface: open, and `listen`'s port stays.
        let o = r("[remote]\nlocal = true\nlisten = \"127.0.0.1:8123\"\n");
        assert_eq!((o.access, o.port), (Access::Open, 8123));
        assert_eq!(r("[remote]\nlisten = \"0.0.0.0:7091\"\n").access, Access::Open);
        // `access` written out wins over the old keys; plain HTTP is not an option any more.
        let w = r("[remote]\naccess = \"localhost\"\nlocal = true\ninsecure = true\n");
        assert_eq!(w.access, Access::Localhost);
        // The file is moved to the new keys when ptyro rewrites it.
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        std::fs::write(
            &path,
            "[remote]\ntailscale = true\nlocal = true\nlisten = \"127.0.0.1:7091\"\n",
        )
        .unwrap();
        let notes = set_serve(&path, true, None).unwrap();
        let t = std::fs::read_to_string(&path).unwrap();
        assert!(
            t.contains("access = \"open\"") && !t.contains("tailscale") && !t.contains("listen"),
            "{t}"
        );
        assert!(notes.iter().any(|n| n.contains("remote.local is gone")), "{notes:?}");
    }

    #[test]
    fn the_port_and_access_are_checked() {
        assert!(Config::parse("[remote]\nport = 80\n").unwrap_err().contains("remote.port"));
        assert!(Config::parse("[remote]\naccess = \"everyone\"\n").is_err());
        assert_eq!("LAN".parse::<Access>(), Ok(Access::Open));
        assert!("everyone".parse::<Access>().unwrap_err().contains("localhost, tailscale or open"));
    }

    #[test]
    fn web_on_edits_an_inline_table_and_leaves_an_unreadable_file_alone() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        std::fs::write(&path, "# mine\nremote = { port = 8443 }\n").unwrap();
        set_serve(&path, true, Some("here")).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# mine")
                && after.contains("serve = true")
                && after.contains("port = 8443"),
            "{after}"
        );
        // Not UTF-8: it is not "empty", so nothing replaces it.
        let bytes = b"# caf\xe9\n[ui]\nmouse = false\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(set_serve(&path, true, Some("here")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A missing file is made.
        let fresh = d.path().join("new/config.toml");
        set_serve(&fresh, true, Some("here")).unwrap();
        assert!(std::fs::read_to_string(&fresh).unwrap().contains("serve = true"));
    }

    #[cfg(unix)] // Unix execute bits, modes and symlinks
    #[test]
    fn writing_a_config_keeps_a_symlink_and_never_shares_a_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("dotfiles/ptyro.toml");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "[ui]\nmouse = false\n").unwrap();
        let link = d.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_private(&link, "[ui]\nmouse = true\n").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "[ui]\nmouse = true\n");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        // Two writers at once: each gets its own temp file and the file is one of the two.
        let a = std::thread::spawn({
            let l = link.clone();
            move || (0..50).for_each(|_| write_private(&l, "[ui]\nmouse = false\n").unwrap())
        });
        (0..50).for_each(|_| write_private(&link, "[ui]\nmouse = true\n").unwrap());
        a.join().unwrap();
        let text = std::fs::read_to_string(&real).unwrap();
        assert!(Config::parse(&text).is_ok(), "{text:?}");
        let left: Vec<_> = std::fs::read_dir(real.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp files left: {left:?}");
    }
}

#[cfg(test)]
mod tailnet_cert {
    use super::*;

    fn status(backend: &str, domains: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "BackendState": backend,
            "Self": { "DNSName": "box.tail1.ts.net." },
            "CertDomains": domains,
        })
    }

    #[test]
    fn a_certificate_is_offered_only_for_this_machines_own_name() {
        assert_eq!(
            tailnet_cert_domain_from(&status("Running", &["Box.tail1.ts.net"])).as_deref(),
            Ok("box.tail1.ts.net")
        );
        let off = tailnet_cert_domain_from(&status("Running", &[])).unwrap_err();
        assert!(off.contains("HTTPS Certificates") && off.contains("admin/dns"), "{off}");
        let other = tailnet_cert_domain_from(&status("Running", &["else.tail1.ts.net"]));
        assert!(other.is_err());
        let down = tailnet_cert_domain_from(&status("Stopped", &["box.tail1.ts.net"]));
        assert!(down.unwrap_err().contains("isn't connected"));
    }
}
