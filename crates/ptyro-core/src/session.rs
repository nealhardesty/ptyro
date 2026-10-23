//! Session descriptions (SPEC §7).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ClientId, SessionId, TermSize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// Adapter-backed coding agent (M4). Until adapters exist, agents run as `command`.
    Agent,
    Shell,
    Command,
    Script,
}

/// Process lifecycle (SPEC §7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ProcessState {
    Starting,
    Running,
    Exited {
        #[serde(default)]
        code: Option<i32>,
        #[serde(default)]
        signal: Option<i32>,
    },
    Failed {
        error: String,
    },
    /// Not running, but resumable: the agent's conversation continues on resume (SPEC §7.4).
    Dormant,
}

impl ProcessState {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Starting => "starting".into(),
            Self::Running => "running".into(),
            Self::Exited { code: Some(c), .. } => format!("exited {c}"),
            Self::Exited { signal: Some(s), .. } => format!("killed sig{s}"),
            Self::Exited { .. } => "exited".into(),
            Self::Failed { .. } => "failed".into(),
            Self::Dormant => "dormant".into(),
        }
    }
}

/// What the program last reported over OSC 7501 (SPEC §9.2). Raw; the status engine (M4)
/// fuses it with hooks into the real agent status.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    /// Shortest unambiguous handle at the time of the reply.
    pub handle: String,
    pub name: String,
    pub kind: SessionKind,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub state: ProcessState,
    pub size: TermSize,
    #[serde(default)]
    pub pid: Option<u32>,
    pub created_ms: u64,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    /// Number of clients currently subscribed.
    #[serde(default)]
    pub viewers: u32,
    #[serde(default)]
    pub controller: Option<ClientId>,
    #[serde(default)]
    pub title: Option<String>,
    /// Name of the foreground process (e.g. `vim` inside a shell), when known.
    #[serde(default)]
    pub foreground: Option<String>,
    /// Current directory: reported by the shell (OSC 7), else probed from the foreground process.
    #[serde(default)]
    pub live_cwd: Option<PathBuf>,
    #[serde(default)]
    pub program_status: Option<ProgramStatus>,
    #[serde(default)]
    pub progress: Option<Progress>,
    /// Exit status of the last shell command (OSC 133;D shell integration).
    #[serde(default)]
    pub last_command_exit: Option<i32>,
    /// The profile it runs under, fixed for its lifetime (SPEC §10.10).
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Worktree id, when it runs in a known worktree.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Project id.
    #[serde(default)]
    pub project: Option<String>,
    /// Home workspace name (SPEC §9.6).
    #[serde(default)]
    pub workspace: Option<String>,
    /// Agent name for `kind = agent` (`claude`, `codex`, …).
    #[serde(default)]
    pub agent: Option<String>,
    /// For `kind = script`: `setup`, `archive`, `after_turn` or `run:<name>`.
    #[serde(default)]
    pub script: Option<String>,
    /// TCP ports its processes listen on (SPEC §10.7).
    #[serde(default)]
    pub ports: Vec<u16>,
    /// The agent status engine's view (SPEC §9).
    #[serde(default)]
    pub status: Option<crate::status::StatusRecord>,
    /// Set when it wants you (SPEC §9.4).
    #[serde(default)]
    pub attention: Option<crate::status::Attention>,
    /// What it's doing right now ("Bash: cargo test").
    #[serde(default)]
    pub activity: Option<String>,
    /// The agent's own conversation id (resume).
    #[serde(default)]
    pub conversation: Option<String>,
    /// Running subagents or teammates.
    #[serde(default)]
    pub children: Vec<crate::status::Child>,
    /// Started with arguments that skip permission prompts.
    #[serde(default)]
    pub dangerous: bool,
    /// A prompt (`new --prompt`) waits to be typed once the agent is ready.
    #[serde(default)]
    pub prompt_pending: bool,
    /// When ptyro last typed a prompt into it: `wait --for idle` waits for a status newer than
    /// this, so it never mistakes the idle before the prompt for the end of the turn.
    #[serde(default)]
    pub last_prompt_ms: Option<u64>,
    /// Completed turns (working → idle or done).
    #[serde(default)]
    pub turns: u32,
    /// When it last changed its screen (`wait --stall`).
    #[serde(default)]
    pub last_output_ms: Option<u64>,
    /// Verified-done (M7.9): the check its turns must pass, and how the last one went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
    /// The run and step it works for (M7.1), `r-7f3a2c/impl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Its permission prompt is held for an exact answer from any client (M9.5): `answer`
    /// reaches the agent as a decision, not keystrokes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// A structured session's protocol (`stream-json`, `app-server`, `acp`, `opencode`; M10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<String>,
    /// What it was asked to do: the first prompt typed into it, trimmed (UX1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The agent's last message, once its turn ended, trimmed (UX1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
    /// A shell's command, running or last finished (OSC 133 `C`/`D`; UX1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<ShellCommand>,
}

/// A shell's current or last command, from OSC 133 marks (`ptyro shell-init`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCommand {
    /// The command line, when the shell sends it (`133;C;cmdline=…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub started_ms: u64,
    /// Unset while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
}

/// At most `max` characters of `text`'s first non-empty lines joined by spaces, `…` when cut.
pub fn brief(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut s: String = joined.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// A session's verified-done check (M7.9): a turn counts as done only once `check` passes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verify {
    /// What runs (`scripts.run.test` resolved to its command).
    pub check: String,
    pub max_bounces: u32,
    /// Bounces since the last prompt ptyro typed.
    pub bounces: u32,
    pub state: VerifyState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Evidence>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyState {
    /// No turn has ended since it was set (or since the last prompt).
    #[default]
    Pending,
    /// The check is running.
    Checking,
    /// The last turn's check passed: done (verified).
    Passed,
    /// It failed and the agent was sent back to fix it.
    Bounced,
    /// It failed and the bounces ran out: done (claimed), not verified.
    Failed,
}

impl VerifyState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Checking => "checking",
            Self::Passed => "passed",
            Self::Bounced => "bounced",
            Self::Failed => "failed",
        }
    }
}

/// What a check found, kept as evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub command: String,
    pub exit_code: i32,
    /// The output's last lines.
    pub tail: String,
    /// The worktree's tree (HEAD plus uncommitted work) it ran against.
    #[serde(default)]
    pub tree: Option<String>,
    pub at_ms: u64,
    pub duration_ms: u64,
}

/// A status newer than the last prompt ptyro typed (and none waiting to be typed).
fn fresh(s: &SessionInfo) -> bool {
    !s.prompt_pending
        && s.status.as_ref().is_some_and(|r| r.since_ms >= s.last_prompt_ms.unwrap_or(0))
}

/// Has `s` reached `cond`: `working`, `blocked`, `idle`, `done`, `errored`, `exit` or
/// `any-attention`? `idle` and `done` mean "the turn ptyro last typed has ended", so the idle
/// before a prompt never counts (SPEC §12.2, M5).
pub fn reached(s: &SessionInfo, cond: &str) -> bool {
    use crate::status::AgentStatus;
    let st = s.status.as_ref().map(|r| r.status);
    match cond {
        "exit" => !s.state.is_live(),
        "working" => st == Some(AgentStatus::Working),
        "blocked" => st == Some(AgentStatus::Blocked),
        "idle" => matches!(st, Some(AgentStatus::Idle | AgentStatus::Done)) && fresh(s),
        "done" => st == Some(AgentStatus::Done) && fresh(s),
        "errored" => st == Some(AgentStatus::Errored) && fresh(s),
        "any-attention" => {
            st == Some(AgentStatus::Blocked)
                || s.attention.as_ref().is_some_and(|a| a.since_ms >= s.last_prompt_ms.unwrap_or(0))
        }
        _ => false,
    }
}

fn default_profile() -> String {
    crate::model::DEFAULT_PROFILE.into()
}

/// OSC 9;4 progress (SPEC §5.7): state 1 normal, 2 error, 3 indeterminate, 4 paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub state: u8,
    #[serde(default)]
    pub percent: Option<u8>,
}

/// A session as remembered by the store, including ones from earlier daemon runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub name: String,
    pub kind: SessionKind,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub state: ProcessState,
    pub created_ms: u64,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    /// Still known to the running daemon.
    pub current: bool,
}

/// Where a new session runs and what it runs (SPEC §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Empty (and no `agent`): the user's `$SHELL` (kind `shell`).
    #[serde(default)]
    pub argv: Vec<String>,
    /// An agent by name, optionally with a profile: `claude`, `claude@work` (kind `agent`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Ignored when `worktree` is set (the session runs in the worktree).
    pub cwd: PathBuf,
    /// A worktree address (`brisk-rook`, `myrepo/@root`, an id suffix).
    #[serde(default)]
    pub worktree: Option<String>,
    /// Explicit profile; `None` follows SPEC §10.10's resolution order.
    #[serde(default)]
    pub profile: Option<String>,
    /// Home workspace; default: the worktree's project's first workspace, else `main`.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The user agreed to run under a profile other than the worktree's or the project's pin.
    #[serde(default)]
    pub override_pin: bool,
    /// The user agreed to a profile guard's question (another profile claims this repo).
    #[serde(default)]
    pub confirmed: bool,
    /// An initial prompt, typed into the agent once it's ready (SPEC §8.2).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Extra variables from the requesting client (SPEC §5.1 `client_env`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub size: Option<TermSize>,
    /// Start even when `review.max_unreviewed` worktrees wait for review (M6.9).
    #[serde(default)]
    pub unqueued: bool,
    /// Verified-done: its turns count as done only once this check passes (M7.9).
    #[serde(default)]
    pub verify: Option<String>,
    /// Make it a lead agent with this policy (JSON; `{}` for the defaults), given `ptyro mcp`
    /// (M7.6).
    #[serde(default)]
    pub lead: Option<serde_json::Value>,
    /// Drive the agent over its structured protocol instead of its terminal UI (M10).
    #[serde(default)]
    pub structured: bool,
    /// Give the agent `ptyro mcp` (a change set's lead, M11; `lead` implies it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mcp: bool,
}
