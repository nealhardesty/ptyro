//! The local wall clock for the status bar and timestamps, honouring `TZ` (WIN3): libc's
//! `localtime_r` through `ptyro-pty`.

/// The local hour and minute now.
pub fn local_hm() -> Option<(u8, u8)> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    local_hm_at(i64::try_from(now).ok()?)
}

/// A moment (ms since the epoch) moved by the local offset from UTC, so that what formats a
/// UTC date and time (`ptyro_core::kb::card::minute`) says the local ones. As it is when the
/// offset can't be had.
pub fn local_ms(ms: u64) -> u64 {
    let offset = i64::try_from(ms / 1000).ok().and_then(ptyro_pty::local_offset_at).unwrap_or(0);
    ms.saturating_add_signed(offset.saturating_mul(1000))
}

/// The local hour and minute of a moment (seconds since the epoch).
pub fn local_hm_at(secs: i64) -> Option<(u8, u8)> {
    ptyro_pty::local_time_hm_at(secs)
}
