//! The local wall clock for the status bar and timestamps, honouring `TZ` (WIN3): libc's
//! `localtime_r` through `ptyro-pty`.

/// The local hour and minute now.
pub fn local_hm() -> Option<(u8, u8)> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    local_hm_at(i64::try_from(now).ok()?)
}

/// The local hour and minute of a moment (seconds since the epoch).
pub fn local_hm_at(secs: i64) -> Option<(u8, u8)> {
    ptyro_pty::local_time_hm_at(secs)
}
