//! The agent status model (SPEC §9): what an agent is doing, how sure ptyro is, where it heard
//! it, and the attention and inbox entries it raises. Plain data; the reducer lives in the daemon.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// No signal yet.
    Unknown,
    /// Spawned, not yet ready for input.
    Starting,
    Working,
    /// Waiting on the user: `kind` says for what (permission, question, auth).
    Blocked,
    /// Turn finished, waiting for the next prompt.
    Idle,
    /// Finished and reported complete (orchestration, OSC 7501 `done` from non-agents).
    Done,
    /// Stopped by an error: `kind` says which (rate_limit, auth, overloaded, …).
    Errored,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Starting => "starting",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Errored => "error",
        }
    }

    /// Parse a state name from any status vocabulary: ptyro's, OSC 7501's, tuios's
    /// (`needs_input`, `errored`, `none`), herdr's (`blocked`, `done`), iTerm2's.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "unknown" | "none" => Self::Unknown,
            "starting" | "start" => Self::Starting,
            "working" | "busy" | "running" | "active" | "thinking" => Self::Working,
            "blocked" | "needs_input" | "needs-input" | "waiting" | "input" | "permission" => {
                Self::Blocked
            }
            "idle" | "ready" => Self::Idle,
            "done" | "complete" | "completed" | "finished" => Self::Done,
            "error" | "errored" | "failed" | "failure" => Self::Errored,
            _ => return None,
        })
    }
}

/// How much to trust a status (SPEC §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Where a signal came from, in SPEC §9.2's priority order (first = strongest within a tie).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    Process,
    /// A structured session's own protocol (M10): the agent tells us exactly.
    Protocol,
    Hook,
    /// A structured side channel the daemon holds (OpenCode SSE, Codex app-server).
    Sidecar,
    Osc,
    /// `ptyro report` from inside the session (self report; herdr-compatible).
    Report,
    Title,
    /// A dialog recognised on screen (trust, login) before any hook spoke.
    Screen,
    Notification,
    Heuristic,
}

impl SignalSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Protocol => "protocol",
            Self::Hook => "hook",
            Self::Sidecar => "sidecar",
            Self::Osc => "osc7501",
            Self::Report => "report",
            Self::Title => "title",
            Self::Screen => "screen",
            Self::Notification => "notification",
            Self::Heuristic => "heuristic",
        }
    }
}

/// A session's current agent status (SPEC §9.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusRecord {
    pub status: AgentStatus,
    /// Blocked: permission | question | auth. Errored: rate_limit | auth | overloaded | ….
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// What exactly: "Bash: npm install", the OSC 7501 `msg`, an error text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub confidence: Confidence,
    pub source: SignalSource,
    pub since_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
}

impl StatusRecord {
    pub fn new(status: AgentStatus, confidence: Confidence, source: SignalSource) -> Self {
        Self {
            status,
            kind: None,
            detail: None,
            confidence,
            source,
            since_ms: crate::now_ms(),
            progress: None,
        }
    }

    /// `blocked (permission): Bash: npm install`
    pub fn describe(&self) -> String {
        let mut s = self.status.label().to_owned();
        if let Some(k) = &self.kind {
            s.push_str(&format!(" ({k})"));
        }
        if let Some(d) = &self.detail {
            s.push_str(&format!(": {d}"));
        }
        s
    }
}

/// A normalized signal, the status engine's only input. Adapters turn hooks, OSC 7501, titles
/// and side channels into these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Signal {
    Status {
        status: AgentStatus,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        progress: Option<u8>,
    },
    /// What the agent is doing right now ("Bash: cargo test"); doesn't change the status.
    Activity { text: String },
    /// The agent's own conversation id (for resume).
    Conversation { id: String },
    /// The first prompt the user typed (branch names, inbox context).
    Prompt { text: String },
    /// A turn ended normally (Stop, `agent-turn-complete`, 7501 `done`).
    TurnEnd,
    /// A subagent or teammate started or finished (Claude agent teams).
    Child {
        id: String,
        #[serde(default)]
        kind: Option<String>,
        running: bool,
    },
    /// The agent asked for the user's attention in words.
    Notify { text: String },
    /// The agent's last message of a turn (Claude `Stop`, Codex `agent-turn-complete`).
    Reply { text: String },
    /// Claude's messaging socket (learned from the hook environment; SPEC §20.1).
    Messaging { socket: String },
}

impl Signal {
    pub fn status(status: AgentStatus) -> Self {
        Self::Status { status, kind: None, detail: None, progress: None }
    }

    pub fn blocked(kind: &str, detail: Option<String>) -> Self {
        Self::Status {
            status: AgentStatus::Blocked,
            kind: Some(kind.into()),
            detail,
            progress: None,
        }
    }
}

/// A signal plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced {
    pub signal: Signal,
    pub source: SignalSource,
    pub confidence: Confidence,
}

/// Why a session wants you (SPEC §9.4), in jump order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    Blocked,
    Errored,
    Done,
    Idle,
    Exited,
    Notification,
    /// A run's gate waits for an answer (M7.5).
    Gate,
    /// A run ended (M7.1), or a message for you arrived (M7.7).
    Run,
}

impl AttentionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Errored => "errored",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Exited => "exited",
            Self::Notification => "notification",
            Self::Gate => "gate",
            Self::Run => "run",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "blocked" => Self::Blocked,
            "errored" | "error" => Self::Errored,
            "done" => Self::Done,
            "idle" => Self::Idle,
            "exited" => Self::Exited,
            "notification" => Self::Notification,
            "gate" => Self::Gate,
            "run" => Self::Run,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub kind: AttentionKind,
    pub since_ms: u64,
    /// "wants permission: Bash(npm install)".
    pub text: String,
}

/// An inbox entry (SPEC §9.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    pub id: i64,
    pub session: String,
    pub session_name: String,
    pub at_ms: u64,
    pub kind: AttentionKind,
    /// "\[work\] brisk-rook (myrepo)".
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub read_ms: Option<u64>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// A rate-limit window (Claude `five_hour`/`seven_day`, Codex primary/secondary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// `5h`, `7d`.
    pub name: String,
    pub used_percent: u8,
    #[serde(default)]
    pub resets_at_ms: Option<u64>,
}

/// One profile's usage of one agent (M4.17).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub profile: String,
    pub agent: String,
    pub windows: Vec<UsageWindow>,
    /// Session cost in thousandths of a dollar (Claude's `cost.total_cost_usd`).
    #[serde(default)]
    pub cost_millis: Option<u64>,
    #[serde(default)]
    pub context_percent: Option<u8>,
    /// Rate-limited until this time.
    #[serde(default)]
    pub limited_until_ms: Option<u64>,
    pub at_ms: u64,
}

impl Usage {
    /// One line for people: `5h 42% (resets in 2h10m) · 7d 18% · context 31% · session $1.20`,
    /// with `RATE-LIMITED for 1h02m` first while it is.
    pub fn summary(&self, now: u64) -> String {
        let until = |t: u64| {
            let m = t.saturating_sub(now) / 60_000;
            if m >= 60 { format!("{}h{:02}m", m / 60, m % 60) } else { format!("{m}m") }
        };
        let mut v = Vec::new();
        if let Some(t) = self.limited_until_ms.filter(|t| *t > now) {
            v.push(format!("RATE-LIMITED for {}", until(t)));
        }
        for w in &self.windows {
            let reset = w
                .resets_at_ms
                .filter(|t| *t > now)
                .map(|t| format!(" (resets in {})", until(t)))
                .unwrap_or_default();
            v.push(format!("{} {}%{reset}", w.name, w.used_percent));
        }
        if let Some(c) = self.context_percent {
            v.push(format!("context {c}%"));
        }
        if let Some(c) = self.cost_millis {
            v.push(format!("session ${:.2}", c as f64 / 1000.0));
        }
        if v.is_empty() {
            v.push("no figures reported yet".into());
        }
        v.join(" · ")
    }
}

/// A subagent or teammate of a session (Claude agent teams, M4.18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usage_line_says_the_limits_and_when_they_reset() {
        let u = Usage {
            profile: "work".into(),
            agent: "claude".into(),
            windows: vec![
                UsageWindow {
                    name: "5h".into(),
                    used_percent: 42,
                    resets_at_ms: Some(130 * 60_000),
                },
                UsageWindow { name: "7d".into(), used_percent: 18, resets_at_ms: None },
            ],
            cost_millis: Some(1200),
            context_percent: None,
            limited_until_ms: Some(62 * 60_000),
            at_ms: 0,
        };
        assert_eq!(
            u.summary(0),
            "RATE-LIMITED for 1h02m · 5h 42% (resets in 2h10m) · 7d 18% · session $1.20"
        );
        assert_eq!(Usage::default().summary(0), "no figures reported yet");
    }

    #[test]
    fn vocabularies_map() {
        assert_eq!(AgentStatus::parse("needs_input"), Some(AgentStatus::Blocked));
        assert_eq!(AgentStatus::parse("errored"), Some(AgentStatus::Errored));
        assert_eq!(AgentStatus::parse("none"), Some(AgentStatus::Unknown));
        assert_eq!(AgentStatus::parse("busy"), Some(AgentStatus::Working));
        assert_eq!(AgentStatus::parse("sleepy"), None);
        assert!(
            AttentionKind::Blocked < AttentionKind::Errored
                && AttentionKind::Idle < AttentionKind::Exited
        );
    }
}
