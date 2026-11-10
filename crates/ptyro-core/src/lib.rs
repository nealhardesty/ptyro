//! Domain types shared by every crate: ids and handles, addressing, the session model, screen and
//! input types, paths, and error codes (SPEC §2, §5.5, §6.5, §7, §14.1).
//!
//! Everything here is plain data. `ptyro-term` and `ptyro-protocol` both build on these types, so
//! neither has to depend on the other (SPEC §3.3 boundary rules).

pub mod cache;
pub mod chat;
pub mod chats;
pub mod config;
pub mod error;
pub mod ids;
pub mod imports;
pub mod input;
pub mod lookup;
pub mod machine;
pub mod model;
pub mod names;
pub mod open;
pub mod os;
pub mod paths;
pub mod project;
pub mod removal;
pub mod scratchpad;
pub mod screen;
pub mod secret;
pub mod selector;
pub mod session;
pub mod settings;
pub mod shell;
pub mod status;
pub mod sync;
pub mod winapp;
pub mod window;
pub mod wsl;

pub use error::{Error, ErrorCode};
pub use ids::{ClientId, SessionId};
pub use screen::TermSize;

/// A `PTYRO_TEST_*` knob that shortens a wait or opens a door for a test. Only debug builds (what
/// the tests run) read them: a release ignores them, whatever the environment says.
pub fn test_knob(name: &str) -> Option<String> {
    if cfg!(debug_assertions) { std::env::var(name).ok() } else { None }
}

/// Milliseconds since the Unix epoch; the timestamp type on the wire.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long ago the timestamp `ms` was: `12s`, `4m`, `3h`, `2d`. One way for every client.
pub fn ago(ms: u64) -> String {
    dur(now_ms().saturating_sub(ms))
}

/// A duration: `12s`, `4m`, `3h`, `2d`.
pub fn dur(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}
