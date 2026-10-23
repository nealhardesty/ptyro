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

use crate::input::{KeyCode, KeyEvent, Mods};
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
}

impl Default for Daemon {
    fn default() -> Self {
        Self {
            scrollback_lines: 5_000,
            frame_interval_ms: 8,
            log_level: "info".into(),
            restore_on_start: RestoreOnStart::Dormant,
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
    /// Where `ptyro serve` listens. Loopback and Tailscale addresses only, unless `insecure`.
    pub listen: String,
    /// The address QR codes and links point at (default: this machine's Tailscale HTTPS name
    /// when `tailscale serve` fronts ptyro, else `http://<listen>`).
    pub public_url: Option<String>,
    /// The daemon runs `ptyro serve` itself (and restarts it).
    pub serve: bool,
    /// Allow a plain-HTTP listener on other addresses (tokens would cross the network in clear).
    pub insecure: bool,
    /// How long Claude's permission prompt waits for a phone's answer (its own dialog stays
    /// open the whole time; the first answer wins).
    pub approval_wait: String,
    /// Web Push to paired devices that asked for it, for the attention kinds in
    /// `notifications.on`.
    pub push: bool,
    /// More push services a device may subscribe with, besides the browsers' own (a
    /// self-hosted UnifiedPush or ntfy server: `push_hosts = ["ntfy.example.com"]`).
    pub push_hosts: Vec<String>,
    /// Believe `X-Forwarded-For` (and `Tailscale-User-Login`) from a loopback peer: set it when a
    /// reverse proxy on this machine fronts `ptyro serve`. Automatic when `tailscale serve` does.
    pub trusted_proxy: bool,
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:7091".into(),
            public_url: None,
            serve: false,
            insecure: false,
            approval_wait: "10m".into(),
            push: true,
            push_hosts: Vec::new(),
            trusted_proxy: false,
        }
    }
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
}

impl Default for Worktrees {
    fn default() -> Self {
        Self {
            location: WorktreeLocation::Data,
            branch_template: "ptyro/{name}".into(),
            start_agents: StartAgents::AfterSetup,
            fetch: true,
            watch: true,
            stale_days: 14,
            auto_rename_branch: false,
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
        let config: Self = toml::from_str(text).map_err(|e| e.to_string().trim_end().to_owned())?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
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
        if self.ui.sidebar_width < 12 {
            return Err("ui.sidebar_width: must be at least 12".into());
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
        self.remote
            .listen
            .parse::<std::net::SocketAddr>()
            .map_err(|e| format!("remote.listen: {e} (an address like 127.0.0.1:7091)"))?;
        // Each ring half starts with a keyframe (≈ 60 KB at 200×60), so it must hold one.
        if self.recorder.max_bytes().map_err(|e| format!("recorder.max_bytes: {e}"))? < 256 << 10 {
            return Err("recorder.max_bytes: at least 256KiB".into());
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
        if DANGEROUS_FLAGS.contains(&flag) {
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
                let leaf = e.rsplit('/').next().unwrap_or(e);
                if CREDENTIAL_FILES.contains(&leaf) {
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
    let secs = n.checked_mul(mult).ok_or_else(|| format!("duration {s:?} is too long"))?;
    Ok(std::time::Duration::from_secs(secs))
}

/// `~` and `~/…` → the home directory.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(c.remote.listen, "127.0.0.1:7091");
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
