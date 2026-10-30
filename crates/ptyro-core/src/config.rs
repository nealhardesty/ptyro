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
    /// Capture the mouse for click-to-focus, wheel scrollback and border drags.
    pub mouse: bool,
    /// Ask before stopping a session from the TUI.
    pub confirm_kill: bool,
    /// Show listening ports in the sidebar and pane headers.
    pub show_ports: bool,
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
}

/// `[ui] sidebar_detail`: the lines under each session row (what it wants, what it's on, its
/// task and last reply, a shell's command).
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
            mouse: true,
            confirm_kill: true,
            show_ports: true,
            tint_borders: false,
            on_stop: OnStop::Next,
            sidebar_detail: SidebarDetail::Auto,
            shell_style: ShellStyle::Pane,
            shell_exit: ShellExit::Close,
            which_key_delay: 400,
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
}

impl Default for Daemon {
    fn default() -> Self {
        Self {
            scrollback_lines: 5_000,
            frame_interval_ms: 8,
            log_level: "info".into(),
            restore_on_start: RestoreOnStart::Dormant,
            machine_name: None,
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

/// An agent (SPEC §13.2 `[agents.<name>]`). User config only (ADR 15).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentDef {
    /// `claude`, `codex`, `gemini`, `opencode`, `antigravity`, `generic` (default: the name if
    /// it's a known adapter, else `generic`).
    pub adapter: Option<String>,
    pub command: Option<CommandSpec>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The agent `n` offers first.
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
    /// Run the project's `after_turn` script after each turn (default true when trusted).
    pub after_turn: Option<bool>,
    /// Record this agent's sessions (`[recorder]`; default true).
    pub record: Option<bool>,
    /// The structured protocol it speaks with `ptyro new --structured` (M10): `stream-json`,
    /// `app-server`, `acp` or `opencode`. Default from its adapter; `acp` makes any Agent Client
    /// Protocol agent structured.
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
            breakthrough: vec!["blocked".into(), "gate".into()],
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
    /// `inbox` (always), `bell`, `osc` (OSC 9/777 to your terminal), `desktop`, `sinks`.
    pub channels: Vec<String>,
    /// Which attention kinds notify: blocked, errored, done, idle, exited, notification.
    pub on: Vec<String>,
    /// `"22:00-07:00"`: only the inbox (and breakthrough kinds) in this window.
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
            channels: ["inbox", "osc", "bell"].map(String::from).into(),
            on: ["blocked", "errored", "done", "idle", "gate", "run"].map(String::from).into(),
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

/// A user theme: any subset of colour tokens, on top of a built-in `base`.
/// Colours: `#rrggbb`, a 0–255 palette index, or a name (`red`, `bright-blue`, `default`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeDef {
    pub base: Option<String>,
    pub accent: Option<String>,
    pub text: Option<String>,
    pub dim: Option<String>,
    pub border: Option<String>,
    pub border_focus: Option<String>,
    pub bar_fg: Option<String>,
    pub bar_bg: Option<String>,
    pub selection_fg: Option<String>,
    pub selection_bg: Option<String>,
    pub search: Option<String>,
    pub working: Option<String>,
    pub blocked: Option<String>,
    pub idle: Option<String>,
    pub done: Option<String>,
    pub error: Option<String>,
    pub exited: Option<String>,
}

/// Sections parsed but used only by later milestones (SPEC §13.2).
/// Sections a config may hold before the milestone that uses them (none left since M9).
pub const RESERVED_SECTIONS: &[(&str, &str)] = &[];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: Ui,
    pub daemon: Daemon,
    /// Key overrides: `"key" = "action [args]"`, `"global:key" = …`, `"nav:key" = …`; the value
    /// `"none"` unbinds a default.
    pub keys: BTreeMap<String, String>,
    pub themes: BTreeMap<String, ThemeDef>,
    pub profiles: BTreeMap<String, ProfileDef>,
    pub worktrees: Worktrees,
    pub ports: Ports,
    pub discover: Discover,
    /// `default = "nvim"` plus `<name> = { command = [...], inside = bool }`.
    pub editors: BTreeMap<String, EditorEntry>,
    pub compat: Compat,
    pub agents: BTreeMap<String, AgentDef>,
    pub notifications: Notifications,
    pub attention: AttentionConfig,
    pub github: Github,
    pub review: Review,
    pub orchestration: Orchestration,
    pub recorder: Recorder,
    pub remote: Remote,
    pub clipboard: Clipboard,
    pub summaries: Summaries,
    pub scratchpad: Scratchpad,
    pub chat: Chat,
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
    /// else the first installed of claude, codex, gemini and opencode.
    pub agent: String,
    /// The tool preset of new chats: `chat`, `read`, `none`, or one of `[chat.tools]`.
    pub preset: String,
    /// New chats start incognito: nothing of them is written, and they end with their agent.
    pub incognito: bool,
    /// Chats can look up your other chats (`chat_search`, `chat_read`, `chat_list`).
    pub history_tool: bool,
    /// Which chats those tools see: `profile` (the chat's account's), `workspace`, or `all`.
    pub history_scope: String,
    /// An idle chat's agent stops after this long; the next message carries on.
    pub hibernate_after: String,
    /// The most a replayed conversation carries when an agent has to be told it again.
    pub seed_tokens: u32,
    /// `auto`: a short title from a small model after the first answer; `off`: the first line.
    pub title: String,
    /// The titler (reads the prompt on stdin); empty = `[summaries] command`.
    pub title_command: Vec<String>,
    /// The most titles made for each profile in a day.
    pub daily_cap: u32,
    /// Also make tool output searchable (2 KB of it a message).
    pub index_tools: bool,
    /// Meaning search (SPEC §22.10): find messages by what they are about, not only by their
    /// words. Switching it on downloads a 31 MB model once.
    pub meaning: bool,
    /// Keep importing the agents' own histories (Claude Code, Codex and Gemini sessions) as
    /// read-only chats, every 10 minutes (SPEC §22.10). Off: it copies coding history into
    /// `chat.db`, so it is yours to ask for; `ptyro chat import` does it once.
    pub import: bool,
    /// `blocked`: tell you when a chat needs you; `all`: also when an answer arrives while no
    /// screen shows it; `off`.
    pub notify: String,
    /// Delete archived chats after this many days (0 = keep them).
    pub archive_purge_days: u32,
    /// Your own tool presets (`chat`, `read` and `none` are built in; a table with their name
    /// changes them).
    pub tools: BTreeMap<String, ChatTools>,
}

impl Default for Chat {
    fn default() -> Self {
        Self {
            enabled: true,
            recent: 5,
            agent: String::new(),
            preset: "chat".into(),
            incognito: false,
            history_tool: true,
            history_scope: "profile".into(),
            hibernate_after: "2m".into(),
            seed_tokens: 30_000,
            title: "auto".into(),
            title_command: Vec::new(),
            daily_cap: 200,
            index_tools: false,
            meaning: false,
            import: false,
            notify: "blocked".into(),
            archive_purge_days: 0,
            tools: BTreeMap::new(),
        }
    }
}

/// What a chat may use (SPEC §22.5): a tool preset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChatTools {
    /// The agent's own web search and fetch.
    pub web: bool,
    /// Read files in `dirs`.
    pub read: bool,
    /// Folders a chat of this preset may read.
    pub dirs: Vec<String>,
    /// The agent's shell: it runs commands as you (never in a built-in preset; shown with ⚠).
    pub shell: bool,
    /// MCP servers by name, from the agent's own configuration, kept on in the chat.
    pub mcp: Vec<String>,
    /// More tool names to approve without asking.
    pub allow: Vec<String>,
}

impl ChatTools {
    /// One line for people: what a chat with this preset may use.
    pub fn describe(&self) -> String {
        let mut what: Vec<String> = Vec::new();
        if self.shell {
            what.push("⚠ runs shell commands".into());
        }
        if self.web {
            what.push("web search".into());
        }
        if self.read {
            what.push(if self.dirs.is_empty() {
                "reads the chat's folders".into()
            } else {
                format!("reads {} and the chat's folders", self.dirs.join(", "))
            });
        }
        if !self.mcp.is_empty() {
            what.push(format!("MCP: {}", self.mcp.join(", ")));
        }
        if !self.allow.is_empty() {
            what.push(format!("also {}", self.allow.join(", ")));
        }
        if what.is_empty() { "no tools".into() } else { what.join(" · ") }
    }
}

impl Chat {
    /// The built-in presets: `chat` (web search), `read` (also reads `dirs`), `none`.
    pub const BUILTIN: [&'static str; 3] = ["chat", "read", "none"];
    /// `chat find`'s own (its history tools and nothing else): never a preset of yours.
    pub const FIND: &'static str = "find";

    /// A preset by name: yours, else a built-in one.
    pub fn preset(&self, name: &str) -> Option<ChatTools> {
        if let Some(t) = self.tools.get(name) {
            return Some(t.clone());
        }
        match name {
            "chat" => Some(ChatTools { web: true, ..ChatTools::default() }),
            "read" => Some(ChatTools { web: true, read: true, ..ChatTools::default() }),
            "none" => Some(ChatTools::default()),
            _ => None,
        }
    }

    /// Every preset name: the built-in ones, then yours.
    pub fn presets(&self) -> Vec<String> {
        let mut v: Vec<String> = Self::BUILTIN.iter().map(|s| (*s).to_owned()).collect();
        let mine: Vec<String> = self.tools.keys().filter(|k| !v.contains(k)).cloned().collect();
        v.extend(mine);
        v
    }

    fn validate(&self) -> Result<(), String> {
        if self.preset(&self.preset).is_none() {
            return Err(format!(
                "chat.preset: {:?} isn't a preset ({})",
                self.preset,
                self.presets().join(", ")
            ));
        }
        parse_duration(&self.hibernate_after).map_err(|e| format!("chat.hibernate_after: {e}"))?;
        if !matches!(self.title.as_str(), "auto" | "off") {
            return Err(format!("chat.title: {:?} (auto or off)", self.title));
        }
        if !matches!(self.history_scope.as_str(), "profile" | "workspace" | "all") {
            return Err(format!(
                "chat.history_scope: {:?} (profile, workspace or all)",
                self.history_scope
            ));
        }
        if !matches!(self.notify.as_str(), "blocked" | "all" | "off") {
            return Err(format!("chat.notify: {:?} (blocked, all or off)", self.notify));
        }
        if !(1..=100).contains(&self.recent) {
            return Err("chat.recent: 1 to 100".into());
        }
        for (name, t) in &self.tools {
            crate::names::validate(name).map_err(|e| format!("chat.tools.{name}: {e}"))?;
            if name == Self::FIND {
                return Err(format!(
                    "chat.tools.{name}: `{name}` is the name of `chat find`'s own tools (it reads your past chats, nothing else): give this preset another name"
                ));
            }
            if t.shell && Self::BUILTIN.contains(&name.as_str()) {
                return Err(format!(
                    "chat.tools.{name}.shell: the built-in presets never run commands; make a preset of your own for that ([chat.tools.shell] shell = true)"
                ));
            }
            // Gemini names a server's tools `mcp_<server>_<tool>`: a `_` would be ambiguous.
            if let Some(m) = t
                .mcp
                .iter()
                .find(|m| m.is_empty() || !m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            {
                return Err(format!(
                    "chat.tools.{name}.mcp: {m:?} isn't an MCP server name (letters, digits and -)"
                ));
            }
        }
        Ok(())
    }
}

/// The scratchpad (NOTE1, SPEC §13.6): one notes file, opened in your editor in a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scratchpad {
    /// Where the file lives (default: `scratchpad.md` in ptyro's data directory). Point it into a
    /// synced folder or a repository to share it between machines.
    pub path: Option<String>,
    /// How its key shows it: `drawer` (below the pane, like the quick terminal), `popup` (a
    /// window over everything) or `pane` (a pane of its own beside the others).
    pub mode: String,
    /// The editor, as a command line (`"nvim"`, `"hx"`, `"code -w"`); default: `[editors]`
    /// default, else `$VISUAL`, `$EDITOR`, then nvim, vim, nano or vi, whichever is installed.
    pub editor: Option<String>,
    /// How many days of saved copies to keep (the newest 20 always stay).
    pub keep: u64,
    /// Tick an item once the worktree started from it is merged or cleaned up.
    pub tick_on_finish: bool,
}

impl Default for Scratchpad {
    fn default() -> Self {
        Self { path: None, mode: "drawer".into(), editor: None, keep: 30, tick_on_finish: true }
    }
}

/// Turn `[remote] serve` on or off in the config file (`ptyro web`, `ptyro daemon start --serve`,
/// and the daemon's own `Request::Web`; WEB1.9), keeping the rest of the file as it is. When
/// turning it on, `access` (`localhost`, `tailscale` or `open`) says who can reach it; left out,
/// the file's own `[remote] access` stands, else Tailscale when it is up on this machine, else
/// open. A file still written with the old keys (`listen`, `tailscale`, `local`) is moved to
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
            (None, false) => Some(Access::Open),
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
    let out = std::process::Command::new("tailscale")
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
fn tailscale_running() -> bool {
    std::process::Command::new("tailscale")
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
    use std::os::unix::fs::OpenOptionsExt;
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
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
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

impl Config {
    /// Where the scratchpad file is: `[scratchpad] path`, else `scratchpad.md` in `data`.
    pub fn scratchpad_path(&self, data: &std::path::Path) -> PathBuf {
        match &self.scratchpad.path {
            Some(p) if !p.trim().is_empty() => expand_home(p.trim()),
            _ => data.join("scratchpad.md"),
        }
    }
}

impl Scratchpad {
    fn validate(&self) -> Result<(), String> {
        if !["drawer", "popup", "pane"].contains(&self.mode.as_str()) {
            return Err(format!("scratchpad.mode: {:?} (drawer, popup or pane)", self.mode));
        }
        if self.keep == 0 || self.keep > 3650 {
            return Err("scratchpad.keep: 1 to 3650 days".into());
        }
        if self.editor.as_deref().is_some_and(|e| e.split_whitespace().next().is_none()) {
            return Err("scratchpad.editor: must not be empty".into());
        }
        Ok(())
    }
}

/// A one-line summary of what a session is doing, from a small, cheap model (UX1.3, SPEC
/// §11.2h). Off unless you turn it on: it sends a trimmed, redacted view of the session to the
/// model's provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Summaries {
    /// Summarize each agent's turn when it ends or stops to ask (`ptyro summarize` works
    /// either way).
    pub enabled: bool,
    /// At most this many summaries a day for each profile (each is about $0.0007 with Haiku).
    pub daily_cap: u32,
    /// The summarizer: reads the prompt on stdin, prints the summary (or Claude's
    /// `--output-format json`). It runs under the session's profile, so on its account.
    pub command: Vec<String>,
}

impl Default for Summaries {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_cap: 200,
            command: [
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
        }
    }
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

/// Pull requests through `gh` (M6.5, M6.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Github {
    /// Ask `gh` about a worktree's PR this often (never more than once a minute per PR).
    pub poll: String,
    /// New PRs are drafts.
    pub draft: bool,
    /// `squash`, `merge` or `rebase` for `ptyro pr automerge`.
    pub merge_method: String,
    /// Feed new CI failures and review comments of PRs ptyro opened to their agent.
    pub watch: bool,
    /// Raise attention when a watched PR stays failing or unanswered this long.
    pub escalate_after: String,
    /// Lines of a failing check's log sent to the agent.
    pub log_lines: usize,
}

impl Default for Github {
    fn default() -> Self {
        Self {
            poll: "60s".into(),
            draft: true,
            merge_method: "squash".into(),
            watch: true,
            escalate_after: "2h".into(),
            log_lines: 80,
        }
    }
}

/// The review queue (M6.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Review {
    /// Hold new agent sessions while this many worktrees wait for review (0 = never).
    pub max_unreviewed: u32,
    /// Send a "ready for review" digest at most this often ("off" for never).
    pub digest: String,
}

impl Default for Review {
    fn default() -> Self {
        Self { max_unreviewed: 0, digest: "30m".into() }
    }
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

/// Runs, checkpoints, the mailbox, leases and the collision radar (SPEC §15, M7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Orchestration {
    /// Checkpoint every agent turn that changed its worktree (`refs/ptyro/ckpt/…`).
    pub checkpoints: bool,
    /// How long a file lease lasts unless renewed.
    pub lease_ttl: String,
    /// Watch live worktrees of a project for overlapping changes and tell both agents.
    pub radar: bool,
    /// Messages expire after this long undelivered.
    pub message_ttl: String,
}

impl Default for Orchestration {
    fn default() -> Self {
        Self { checkpoints: true, lease_ttl: "30m".into(), radar: true, message_ttl: "24h".into() }
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
    /// Name the branch after the first prompt (until it's pushed; M3.16).
    pub auto_rename_branch: bool,
    /// Archive a worktree when its PR merges and nothing runs in it (UX1.12).
    pub archive_on_merge: bool,
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
            auto_rename_branch: false,
            archive_on_merge: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ports {
    /// `"20000-29999"`
    pub range: String,
    /// Ports per worktree.
    pub count: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self { range: "20000-29999".into(), count: 10 }
    }
}

impl Ports {
    pub fn bounds(&self) -> Result<(u16, u16), String> {
        let (a, b) = self.range.split_once('-').ok_or("expected LOW-HIGH, e.g. 20000-29999")?;
        let lo: u16 = a.trim().parse().map_err(|_| format!("bad port {a:?}"))?;
        let hi: u16 = b.trim().parse().map_err(|_| format!("bad port {b:?}"))?;
        if lo < 1024 || hi < lo {
            return Err("range must be LOW-HIGH with LOW ≥ 1024".into());
        }
        if u32::from(hi - lo) + 1 < u32::from(self.count) {
            return Err(format!("range {} is smaller than count {}", self.range, self.count));
        }
        Ok((lo, hi))
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
    /// argv; `{path}` is replaced by the worktree path.
    pub command: Vec<String>,
    /// Open as a ptyro session instead of a separate window (terminal editors).
    #[serde(default)]
    pub inside: bool,
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
    /// `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`/`CODEX_API_KEY`, `GEMINI_API_KEY`.
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
    pub gemini: Option<Account>,
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
pub const PROFILE_AGENTS: &[&str] = &["claude", "codex", "gemini", "opencode"];

/// Files that hold credentials: never shared between profiles (SPEC §10.10).
pub const CREDENTIAL_FILES: &[&str] =
    &[".credentials.json", ".claude.json", "auth.json", "oauth_creds.json", "credentials.json"];

impl ProfileDef {
    pub fn account(&self, agent: &str) -> Option<&Account> {
        match agent {
            "claude" => self.claude.as_ref(),
            "codex" => self.codex.as_ref(),
            "gemini" => self.gemini.as_ref(),
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
        let config: Self = toml::from_str(text).map_err(|e| e.to_string().trim_end().to_owned())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        self.scratchpad.validate()?;
        self.chat.validate()?;
        if self.ui.prefix.keys().is_empty() {
            return Err("ui.prefix: needs at least one key".into());
        }
        for k in self.ui.prefix.keys() {
            parse_key(k).map_err(|e| format!("ui.prefix: {e}"))?;
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
            ("github.poll", &self.github.poll),
            ("github.escalate_after", &self.github.escalate_after),
            ("review.digest", &self.review.digest),
            ("orchestration.lease_ttl", &self.orchestration.lease_ttl),
            ("orchestration.message_ttl", &self.orchestration.message_ttl),
        ] {
            parse_duration(v).map_err(|e| format!("{k}: {e}"))?;
        }
        if !["squash", "merge", "rebase"].contains(&self.github.merge_method.as_str()) {
            return Err(format!(
                "github.merge_method: {:?} (squash, merge or rebase)",
                self.github.merge_method
            ));
        }
        if let Some(q) = &self.notifications.quiet_hours {
            parse_quiet_hours(q).map_err(|e| format!("notifications.quiet_hours: {e}"))?;
        }
        for c in &self.notifications.channels {
            if !["inbox", "bell", "osc", "desktop", "sinks"].contains(&c.as_str()) {
                return Err(format!(
                    "notifications.channels: unknown channel {c:?} (inbox, bell, osc, desktop, sinks)"
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
            if crate::status::AttentionKind::parse(k).is_none() {
                return Err(format!(
                    "notifications: unknown kind {k:?} (blocked, errored, done, idle, exited, notification, gate, run)"
                ));
            }
        }
        if !(12..=500).contains(&self.ui.sidebar_width) {
            return Err("ui.sidebar_width: must be 12 to 500".into());
        }
        if self.discover.depth > 32 {
            return Err("discover.depth: at most 32".into());
        }
        if self.github.log_lines > 5000 {
            return Err("github.log_lines: at most 5000".into());
        }
        if self.summaries.command.is_empty() {
            return Err("summaries.command: must not be empty".into());
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
            let spec =
                key.strip_prefix("global:").or_else(|| key.strip_prefix("nav:")).unwrap_or(key);
            parse_key(spec).map_err(|e| format!("keys.{key:?}: {e}"))?;
        }
        for (name, p) in &self.profiles {
            if name != "default" {
                crate::names::validate(name).map_err(|e| format!("profiles.{name}: {e}"))?;
            }
            p.validate().map_err(|e| format!("profiles.{name}.{e}"))?;
        }
        self.ports.bounds().map_err(|e| format!("ports.range: {e}"))?;
        if self.ports.count == 0 {
            return Err("ports.count: must be at least 1".into());
        }
        crate::model::check_branch_template(&self.worktrees.branch_template)
            .map_err(|e| format!("worktrees.branch_template: {e}"))?;
        for (name, e) in &self.editors {
            match (name.as_str(), e) {
                ("default", EditorEntry::Name(n))
                    if !self.editors.contains_key(n) && !BUILTIN_EDITORS.contains(&n.as_str()) =>
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
        for (name, t) in &self.themes {
            for (token, value) in t.tokens() {
                if let Some(v) = value {
                    parse_color(v).map_err(|e| format!("themes.{name}.{token}: {e}"))?;
                }
            }
        }
        Ok(())
    }

    /// Reserved sections present in this config (for `config check`).
    pub fn reserved_in_use(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
}

/// Flags that turn off an agent's permission prompts or sandbox on their own (Claude, Codex,
/// Gemini), alone or as `--flag=value`.
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
        // `npx -y @google/gemini-cli`: that `-y` is the package runner's own ("yes, install it").
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

/// Editors available without configuration: terminal ones open inside ptyro.
pub const BUILTIN_EDITORS: &[&str] = &["nvim", "vim", "hx", "code", "zed", "cursor"];

impl Config {
    /// An editor by name: configured, else built in.
    pub fn editor(&self, name: &str) -> Option<EditorDef> {
        if let Some(EditorEntry::Def(d)) = self.editors.get(name) {
            return Some(d.clone());
        }
        let inside = matches!(name, "nvim" | "vim" | "hx");
        BUILTIN_EDITORS
            .contains(&name)
            .then(|| EditorDef { command: vec![name.into(), "{path}".into()], inside })
    }

    /// The scratchpad's editor as argv (`{path}` may stand for the file): `[scratchpad] editor`,
    /// else the default editor of `[editors]` (only a terminal one when `terminal`), else
    /// `$VISUAL` / `$EDITOR`, else the first of nvim, vim, nano and vi that is installed.
    pub fn scratchpad_editor(&self, terminal: bool) -> Option<Vec<String>> {
        let words = |l: &str| l.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        if let Some(e) = &self.scratchpad.editor {
            return Some(words(e));
        }
        if let Some(EditorEntry::Name(n)) = self.editors.get("default")
            && let Some(def) = self.editor(n)
            && (def.inside || !terminal)
        {
            return Some(def.command);
        }
        for var in ["VISUAL", "EDITOR"] {
            if let Ok(v) = std::env::var(var)
                && !v.trim().is_empty()
            {
                return Some(words(&v));
            }
        }
        crate::scratchpad::fallback_editor().map(|e| vec![e])
    }

    /// `editors.default`, else `$VISUAL`/`$EDITOR` if it names a known editor, else `nvim`.
    pub fn default_editor(&self) -> String {
        if let Some(EditorEntry::Name(n)) = self.editors.get("default") {
            return n.clone();
        }
        for var in ["VISUAL", "EDITOR"] {
            if let Ok(v) = std::env::var(var) {
                let base = v.rsplit('/').next().unwrap_or(&v).to_owned();
                if self.editor(&base).is_some() {
                    return base;
                }
            }
        }
        "nvim".into()
    }

    /// Every editor name: configured ones, then built-ins.
    pub fn editor_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.editors.keys().filter(|k| *k != "default").cloned().collect();
        for b in BUILTIN_EDITORS {
            if !v.iter().any(|n| n == b) {
                v.push((*b).into());
            }
        }
        v
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

impl ThemeDef {
    pub fn tokens(&self) -> [(&'static str, &Option<String>); 16] {
        [
            ("accent", &self.accent),
            ("text", &self.text),
            ("dim", &self.dim),
            ("border", &self.border),
            ("border_focus", &self.border_focus),
            ("bar_fg", &self.bar_fg),
            ("bar_bg", &self.bar_bg),
            ("selection_fg", &self.selection_fg),
            ("selection_bg", &self.selection_bg),
            ("search", &self.search),
            ("working", &self.working),
            ("blocked", &self.blocked),
            ("idle", &self.idle),
            ("done", &self.done),
            ("error", &self.error),
            ("exited", &self.exited),
        ]
    }
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
    fn find_is_chat_finds_own_preset() {
        let mut c = Chat::default();
        assert!(c.validate().is_ok());
        c.tools.insert("find".into(), ChatTools { shell: true, ..ChatTools::default() });
        let e = c.validate().unwrap_err();
        assert!(e.contains("chat.tools.find") && e.contains("another name"), "{e}");
    }

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
            ("gemini -y", "-y"),
            ("gemini --yolo", "--yolo"),
            ("gemini --approval-mode yolo", "--approval-mode yolo"),
            ("gemini --approval-mode=YOLO", "--approval-mode=YOLO"),
        ] {
            assert_eq!(dangerous_arg(&argv(bad)).as_deref(), Some(shown), "{bad}");
        }
        for ok in [
            "claude --permission-mode plan",
            "claude --permission-mode",
            "codex -s workspace-write -a on-request",
            "codex -c model=o3",
            "gemini --approval-mode default",
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
        let e = Config::parse("[themes.mine]\naccent = \"#12345\"\n").unwrap_err();
        assert!(e.contains("themes.mine.accent"), "{e}");
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
channels = ["inbox", "desktop", "sinks"]
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

    #[test]
    fn the_spec_profile_example_parses() {
        let c = Config::parse(
            r#"
[profiles.work]
label = "work"
color = "blue"
claude = "~/.claude-work"
codex  = "~/.codex-work"
gemini = { api_key = { command = ["op", "read", "op://Work/gemini/key"] } }
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

[ports]
range = "30000-30099"
count = 5

[editors]
default = "nvim"
code = { command = ["code", "{path}"] }
"#,
        )
        .unwrap();
        let w = &c.profiles["work"];
        assert_eq!(w.claude.as_ref().and_then(Account::dir), Some("~/.claude-work"));
        assert!(matches!(
            w.gemini.as_ref().and_then(Account::def).and_then(|d| d.api_key.as_ref()),
            Some(Secret::Command { .. })
        ));
        assert_eq!(w.guard, Guard::Refuse);
        assert_eq!(c.profiles["personal"].guard, Guard::Confirm);
        assert_eq!(c.worktrees.location, WorktreeLocation::Sibling);
        assert_eq!(c.ports.bounds().unwrap(), (30000, 30099));
        assert_eq!(c.default_editor(), "nvim");
        assert!(c.editor("nvim").unwrap().inside);
        assert!(!c.editor("code").unwrap().inside);
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
        let e = Config::parse("[ports]\nrange = \"10-20\"\n").unwrap_err();
        assert!(e.contains("ports.range"), "{e}");
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
        for ok in ["npx -y @google/gemini-cli", "bunx -y gemini", "pnpm dlx -y gemini-cli"] {
            assert_eq!(dangerous_arg(&argv(ok)), None, "{ok}");
        }
        // Anywhere else it still skips prompts.
        assert_eq!(dangerous_arg(&argv("npx @google/gemini-cli -y")).as_deref(), Some("-y"));
        assert_eq!(dangerous_arg(&argv("gemini -y")).as_deref(), Some("-y"));
        assert!(
            Config::parse("[agents.gemini]\ncommand = [\"npx\", \"-y\", \"@google/gemini-cli\"]\n")
                .is_ok()
        );
    }

    #[test]
    fn durations_and_numbers_are_bounded() {
        assert!(parse_duration("3650d").is_ok());
        assert!(parse_duration("99999999999999d").unwrap_err().contains("too long"));
        assert!(parse_duration("18446744073709551615s").is_err());
        for (bad, key) in [
            ("[orchestration]\nlease_ttl = \"soon\"\n", "orchestration.lease_ttl"),
            ("[orchestration]\nmessage_ttl = \"24 hours\"\n", "orchestration.message_ttl"),
            ("[scratchpad]\nkeep = 300000000000\n", "scratchpad.keep"),
            ("[ui]\nsidebar_width = 60000\n", "ui.sidebar_width"),
            ("[discover]\ndepth = 200\n", "discover.depth"),
            ("[github]\nlog_lines = 100000\n", "github.log_lines"),
            ("[summaries]\ncommand = []\n", "summaries.command"),
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
