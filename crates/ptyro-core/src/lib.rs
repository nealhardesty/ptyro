//! Domain types shared by every crate: ids and handles, addressing, the session model, screen and
//! input types, paths, and error codes (SPEC §2, §5.5, §6.5, §7, §14.1).
//!
//! Everything here is plain data. `ptyro-term` and `ptyro-protocol` both build on these types, so
//! neither has to depend on the other (SPEC §3.3 boundary rules).

pub mod cache;
pub mod chat;
pub mod config;
pub mod error;
pub mod ids;
pub mod imports;
pub mod input;
pub mod model;
pub mod names;
pub mod paths;
pub mod project;
pub mod screen;
pub mod secret;
pub mod session;
pub mod status;

pub use error::{Error, ErrorCode};
pub use ids::{ClientId, SessionId};
pub use screen::TermSize;

/// Milliseconds since the Unix epoch; the timestamp type on the wire.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
