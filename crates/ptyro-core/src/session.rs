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
    /// The machine it lives on, when that is another one this daemon mirrors (MM1.B, SPEC
    /// §16.4b); `None`: here. Its names then carry the machine (`laptop2:work`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
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
    /// What it's about, for people to read (UX1.24): `--title`, `ptyro retitle`, else its first
    /// prompt; else the program's window title. The `name` is its address.
    #[serde(default)]
    pub title: Option<String>,
    /// The window title the program set (OSC 0/2), whatever `title` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
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
    /// The model the agent runs, as it names it (`claude-opus-5-5`); [`model_label`] shortens it
    /// for display. Live only: a dormant session shows none until its agent speaks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The chat it serves, when it is a chat's engine (SPEC §22.2): hidden from the session
    /// tree and from `ptyro ls` (`ls --chats` shows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
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
    /// Completed turns: the agent's own turn ends, else working → idle or done (SPEC §9.3b).
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
    /// The adapter of an agent started by hand in this shell session, while it's in the
    /// foreground (UX1.4): its status then comes from its screen, at low confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_agent: Option<String>,
    /// What it's doing, in a line, from a small model (`[summaries]`, UX1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    /// Its worktree's (or directory's) quick terminal (UX1.25): one shell, reused, and quiet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick: bool,
    /// The scratchpad's editor (NOTE1.1): one, in a pane or drawer of its own, kept out of the
    /// sidebar.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pad: bool,
    /// It belongs to its workspace's SHELLS, not to a worktree (SPEC §23.2): never placed under
    /// one, whatever its directory.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loose: bool,
    /// Its number in its workspace's SHELLS (SPEC §23.3): `Alt-3`, `%3`. Stays until it goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
    /// What it is watched for (SPEC §23.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<Watch>,
    /// It runs in a scratch directory of its own, which goes with it (SPEC §23.8).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scratch: bool,
    /// Lost in a daemon restart and offered again in its number (SPEC §23.10): ⏎ starts it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,
}

/// A SHELLS shell's label (SPEC §23.4), the same in the TUI, the CLI and the web: its title,
/// else an agent's or a command's name, else what runs in the shell and where (`htop · ~/dl`),
/// else where (`~/dl`, `…/tmp` outside `home`).
pub fn shell_label(s: &SessionInfo, home: Option<&std::path::Path>) -> String {
    if let Some(t) = s.title.as_deref().filter(|t| s.window_title.as_deref() != Some(*t)) {
        return t.to_owned();
    }
    if s.kind != SessionKind::Shell {
        return s.name.clone();
    }
    let dir = s.live_cwd.as_deref().unwrap_or(&s.cwd);
    // A Windows daemon's directory reads with `\`, wherever it's shown (WIN7).
    let sep = if dir.to_string_lossy().contains('\\') { '\\' } else { '/' };
    let dir = match home.and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{sep}{}", rest.display()),
        None => {
            let whole = dir.to_string_lossy();
            match whole.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next() {
                Some(last) if !last.is_empty() && last != whole => format!("…{sep}{last}"),
                _ => whole.into_owned(),
            }
        }
    };
    let shell = s.argv.first().map(|a| program_name(a));
    match s.foreground.as_deref().map(|f| f.trim_start_matches('-')) {
        Some(fg) if Some(fg) != shell.as_deref() => format!("{fg} · {dir}"),
        _ => dir,
    }
}

/// A program's name from its path, as the process list names it (`foreground`): its last part
/// (by either separator: a Windows daemon's sessions are shown everywhere), a login shell's `-`
/// dropped; for a Windows path (with `\`, or a runnable extension) also without the extension
/// and lowercase, as WIN7 names a Windows process (`C:\…\pwsh.exe` is `pwsh`). The same table is
/// in `web/test/shells.mjs`.
pub fn program_name(argv0: &str) -> String {
    let last = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0).trim_start_matches('-');
    let lower = last.to_ascii_lowercase();
    let ext = [".exe", ".cmd", ".bat", ".com"].iter().find_map(|e| lower.strip_suffix(e));
    match ext {
        Some(stem) => stem.to_owned(),
        None if argv0.contains('\\') => lower,
        None => last.to_owned(),
    }
}

/// What a SHELLS shell is watched for (SPEC §23.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Watch {
    /// When the running command ends, however short. Fires once.
    Done,
    /// When new output arrives while nobody looks (tmux's `monitor-activity`). Stays.
    Output,
    /// When it has printed nothing for `ms` (tmux's `monitor-silence`). Stays.
    Silence { ms: u64 },
    /// When a line on its screen matches `re`. Fires once.
    Match { re: String },
}

impl Watch {
    /// `done`, `output`, `silence 30s`, `match ERROR`: how people write and read it.
    pub fn label(&self) -> String {
        match self {
            Self::Done => "done".into(),
            Self::Output => "output".into(),
            Self::Silence { ms } => format!("silence {}s", ms / 1000),
            Self::Match { re } => format!("match {re}"),
        }
    }

    /// The opposite of [`label`](Self::label): `done`, `output`, `silence 30s`, `match RE`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (word, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let rest = rest.trim();
        match word {
            "done" => Ok(Self::Done),
            "output" => Ok(Self::Output),
            "silence" => {
                let d = crate::config::parse_duration(if rest.is_empty() { "30s" } else { rest })?;
                if d.is_zero() {
                    return Err("silence needs a time, like 30s".into());
                }
                Ok(Self::Silence { ms: d.as_millis() as u64 })
            }
            "match" if !rest.is_empty() => Ok(Self::Match { re: rest.to_owned() }),
            "match" => Err("match needs a pattern, like `match ERROR|listening`".into()),
            _ => {
                Err(format!("{word:?}: watch for done, output, silence <time> or match <pattern>"))
            }
        }
    }

    /// Fires once and then clears (`done`, `match`), as against staying on (`output`, `silence`).
    pub fn once(&self) -> bool {
        matches!(self, Self::Done | Self::Match { .. })
    }
}

/// A session's summary (UX1.3): `text` was made after turn `turn`, at `at_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub text: String,
    pub turn: u32,
    pub at_ms: u64,
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

/// A model's id shortened for a title or a column: `claude-opus-5-5` → `opus 5.5`,
/// `claude-3-5-sonnet-20241022` → `sonnet 3.5`, `claude-opus-5-5[1m]` → `opus 5.5 1m`, and
/// `anthropic/claude-sonnet-4-5` (OpenCode names a provider first) → `sonnet 4.5`. Any other
/// vendor's id (`gpt-5.6-sol`, `gemini-2.5-pro`) is already short, so it stays as it is.
/// The web client has the same function (`modelLabel`, web/src/screen.ts).
pub fn model_label(id: &str) -> String {
    let id = id.trim();
    let id = id.rsplit('/').next().unwrap_or(id);
    let (id, tag) = id.split_once('[').map_or((id, ""), |(a, b)| (a, b.trim_end_matches(']')));
    let label = claude_label(id).unwrap_or_else(|| id.to_owned());
    if tag.is_empty() { label } else { format!("{label} {tag}") }
}

fn claude_label(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.strip_prefix("claude-")?.split('-').collect();
    let at = parts.iter().position(|p| matches!(*p, "opus" | "sonnet" | "haiku"))?;
    // `opus-5-5` (family first) or `3-5-sonnet` (version first); a date (`20241022`) isn't one.
    let version = |ps: &[&str]| {
        ps.iter()
            .take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
            .copied()
            .collect::<Vec<_>>()
            .join(".")
    };
    let v = if at == 0 { version(&parts[1..]) } else { version(&parts[..at]) };
    Some(if v.is_empty() { parts[at].to_owned() } else { format!("{} {v}", parts[at]) })
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
    /// Resume this conversation of the agent instead of starting one (UX1.4: adopting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// Its title, for people to read (UX1.24); default: the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Make it the quick terminal of where it starts (UX1.25; `QuickShell` reuses it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick: bool,
    /// Make it the scratchpad's editor (NOTE1.1; `ScratchPane` reuses it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pad: bool,
    /// Quick-new (UX3.2): start what was last started in its workspace (`agent`, `argv` and
    /// `profile` come from that), else the default agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
    /// A client's tool (an editor, a `ptyro …` command's output): not remembered for `again`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tool: bool,
    /// A command line, as typed (with no `argv` or `agent`): the daemon runs it through its own
    /// default shell (`$SHELL -c` on Unix), so the client needn't know which shell that is
    /// (protocol 1.27, WIN4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A shell in its workspace's SHELLS (SPEC §23): never placed under the worktree its
    /// directory is in, and given a number.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loose: bool,
    /// The number it asks for in SHELLS (`--slot`, `Alt-N` on an empty one); taken is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u32>,
    /// Start it in a new, empty scratch directory of its own (SPEC §23.8); implies `loose`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scratch: bool,
}

/// What a workspace last started (UX3.2), so `again` can start another of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastLaunch {
    /// An agent (`claude`, `claude@work`); `None` with empty `argv`: a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// A command's argv (no `agent`), or an agent's extra arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
}

impl LastLaunch {
    /// For people: `claude`, `claude@work`, `shell`, `htop`.
    pub fn label(&self) -> String {
        match (&self.agent, self.argv.first()) {
            (Some(a), _) => match &self.profile {
                Some(p) if !a.contains('@') => format!("{a}@{p}"),
                _ => a.clone(),
            },
            (None, None) => "shell".into(),
            (None, Some(p)) => p.rsplit('/').next().unwrap_or(p).to_owned(),
        }
    }
}

/// How much of the session list to show (SPEC §11.2c, the sidebar's filter): everything, only
/// what is running, or only what is busy right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Show {
    #[default]
    All,
    Running,
    Active,
}

impl Show {
    pub const EVERY: [Show; 3] = [Self::All, Self::Running, Self::Active];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Running => "running",
            Self::Active => "active",
        }
    }

    /// `all` → `running` → `active` → `all`.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Active,
            Self::Active => Self::All,
        }
    }

    pub fn keeps(self, s: &SessionInfo) -> bool {
        match self {
            Self::All => true,
            Self::Running => s.is_running(),
            Self::Active => s.is_active(),
        }
    }
}

impl SessionInfo {
    /// A session of its own: not a worktree's quick terminal, not the scratchpad's editor.
    pub fn is_real(&self) -> bool {
        !self.quick && !self.pad
    }

    /// A real session whose process is alive (not exited, failed or hibernated).
    pub fn is_running(&self) -> bool {
        self.is_real() && self.state.is_live()
    }

    /// Running and busy now: an agent that is starting, working or waiting for you (blocked), or
    /// a shell with a command running (OSC 133). An agent between turns, a shell at its prompt
    /// and a command session with no status are running, not active.
    pub fn is_active(&self) -> bool {
        use crate::status::AgentStatus as A;
        if !self.is_running() {
            return false;
        }
        match &self.status {
            Some(r) => matches!(r.status, A::Working | A::Blocked | A::Starting),
            None => self.command.as_ref().is_some_and(|c| c.ended_ms.is_none()),
        }
    }
}

#[cfg(test)]
mod model_label_tests {
    use super::model_label;

    /// A Windows daemon's shell, as `ptyro list --json` printed one (WIN7): its idle PowerShell
    /// isn't named as though something ran in it, and its directory reads with `\`.
    #[test]
    fn a_windows_shell_is_labelled_by_where_it_is() {
        let s = |argv0: &str, fg: &str, cwd: &str| -> super::SessionInfo {
            serde_json::from_value(serde_json::json!({
                "id": "01m6mxc13w7wz153srvrxtcsg5", "handle": "csg5", "name": "s1", "kind": "shell",
                "argv": [argv0], "cwd": cwd, "live_cwd": cwd, "state": { "t": "running" },
                "size": { "cols": 80, "rows": 24 }, "created_ms": 0, "viewers": 0,
                "foreground": fg,
            }))
            .unwrap()
        };
        let label = |x: &super::SessionInfo, home: &str| {
            super::shell_label(x, Some(std::path::Path::new(home)))
        };
        let pwsh = r"C:\Program Files\PowerShell\7\pwsh.exe";
        assert_eq!(label(&s(pwsh, "pwsh", r"D:\src\app"), r"C:\Users\me"), r"…\app");
        assert_eq!(label(&s(pwsh, "htop", r"D:\src\app"), r"C:\Users\me"), r"htop · …\app");
        assert_eq!(label(&s("/bin/zsh", "zsh", "/home/me/dl"), "/home/me"), "~/dl", "as before");
        assert_eq!(label(&s("/bin/zsh", "zsh", "/tmp"), "/home/me"), "…/tmp");
    }

    /// The same table as `web/test/shells.mjs`: change both together.
    #[test]
    fn programs_are_named_as_the_process_list_names_them() {
        use super::program_name;
        for (argv0, name) in [
            ("/bin/zsh", "zsh"),
            ("-zsh", "zsh"),
            ("bash", "bash"),
            ("/opt/My Tools/Fish", "Fish"),
            (r"C:\Program Files\PowerShell\7\pwsh.exe", "pwsh"),
            (r"C:\Windows\System32\WindowsPowerShell\v1.0\PowerShell.EXE", "powershell"),
            (r"C:\Windows\System32\cmd.exe", "cmd"),
            (r"C:\Program Files\Git\bin\bash", "bash"),
            ("pwsh.exe", "pwsh"),
        ] {
            assert_eq!(program_name(argv0), name, "{argv0}");
        }
    }

    #[test]
    fn models_are_named_short() {
        for (id, label) in [
            ("claude-opus-5-5", "opus 5.5"),
            ("claude-sonnet-4-5-20250929", "sonnet 4.5"),
            ("claude-3-5-sonnet-20241022", "sonnet 3.5"),
            ("claude-opus-4-1", "opus 4.1"),
            ("claude-haiku-5-5", "haiku 5.5"),
            ("claude-opus-5-5[1m]", "opus 5.5 1m"),
            ("anthropic/claude-sonnet-5-5", "sonnet 5.5"),
            ("opus", "opus"),
            ("gpt-5.6-sol", "gpt-5.6-sol"),
            ("openai/gpt-5.5", "gpt-5.5"),
            ("gemini-2.5-pro", "gemini-2.5-pro"),
            ("claude-fable-5-1", "claude-fable-5-1"),
            ("  ", ""),
        ] {
            assert_eq!(model_label(id), label, "{id}");
        }
    }
}
