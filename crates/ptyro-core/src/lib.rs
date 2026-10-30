//! Domain types shared by every crate: ids and handles, addressing, the session model, screen and
//! input types, paths, and error codes (SPEC §2, §5.5, §6.5, §7, §14.1).
//!
//! Everything here is plain data. `ptyro-term` and `ptyro-protocol` both build on these types, so
//! neither has to depend on the other (SPEC §3.3 boundary rules).

// Here, not in the binary: every ptyro crate builds on this one, so this is said first, before a
// Unix-only dependency fails in a way that hides it.
#[cfg(not(unix))]
compile_error!(
    "ptyro runs on Linux and macOS (Windows through WSL2); see the Requirements section of README.md"
);

pub mod cache;
pub mod chat;
pub mod chats;
pub mod config;
pub mod error;
pub mod ids;
pub mod imports;
pub mod input;
pub mod machine;
pub mod model;
pub mod names;
pub mod paths;
pub mod project;
pub mod scratchpad;
pub mod screen;
pub mod secret;
pub mod session;
pub mod settings;
pub mod status;
pub mod sync;

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
