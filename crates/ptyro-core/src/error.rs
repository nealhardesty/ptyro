//! Stable error codes shared by the daemon, the wire and the CLI (SPEC §6.6, §12.4).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    Ambiguous,
    Invalid,
    Conflict,
    DirtyWorktree,
    UntrustedProject,
    PermissionDenied,
    Unsupported,
    Timeout,
    Busy,
    /// The request needs the user's explicit confirmation (a profile guard or pin override,
    /// SPEC §10.10); resend it with confirmation once they agree.
    NeedsConfirmation,
    Internal,
    /// Client-side only: the daemon could not be reached.
    DaemonUnavailable,
    /// Client-side only: incompatible protocol major version.
    ProtocolMismatch,
}

impl ErrorCode {
    /// Process exit code for the CLI (SPEC §12.4).
    pub fn exit_code(self) -> i32 {
        match self {
            Self::NotFound => 3,
            Self::Ambiguous => 4,
            Self::Conflict | Self::DirtyWorktree | Self::Busy | Self::NeedsConfirmation => 5,
            Self::UntrustedProject => 6,
            Self::Timeout => 7,
            Self::DaemonUnavailable => 8,
            Self::ProtocolMismatch => 9,
            Self::Invalid => 2,
            Self::PermissionDenied | Self::Unsupported | Self::Internal => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// The way through this refusal, when there is one (protocol 1.38, SPEC §6.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

/// A way through a refusal: the same request again with one thing said. Each client offers it
/// its own way: the CLI names the flag, the TUI and the web show a button with `label`. The
/// message itself names no flag (SPEC §6.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    #[serde(flatten)]
    pub again: Again,
    /// What a button says: "Remove anyway", "Stop and remove".
    pub label: String,
}

/// What the request says the second time. Stable ids on the wire (`"fix": "force"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fix", rename_all = "snake_case")]
pub enum Again {
    /// `force`: go on anyway (discard the work, interrupt the turn, stop what runs).
    Force,
    /// `stop_sessions`: stop the sessions running in it too.
    StopSessions,
    /// `stop`: stop the session first.
    Stop,
    /// `trust`: trust what was read, by its fingerprint.
    Trust { fingerprint: String },
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), fix: None }
    }
    /// Says the way through (see [`Fix`]).
    #[must_use]
    pub fn with_fix(mut self, again: Again, label: &str) -> Self {
        self.fix = Some(Fix { again, label: label.into() });
        self
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }
    /// Nothing of the kind `what` (a session, a run, a device) matches `addr`. Resolvers use
    /// [`crate::lookup::not_found`], which also explains a handle that is too short.
    pub fn missing(what: &str, addr: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("no {what} matches \"{addr}\""))
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}
