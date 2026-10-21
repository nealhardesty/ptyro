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
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::NotFound, format!("no session matches \"{what}\""))
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }
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
