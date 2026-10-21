//! Staying up for a week (M8.5): crash reports for every panic, a session whose emulator panics
//! keeps running on a fresh one, and a watchdog that measures the daemon's health for
//! `ptyro daemon status` (memory, fds, tasks, how late the runtime runs).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ptyro_protocol::Metrics;

use crate::daemon::Daemon;

/// Panics so far (any thread).
pub static CRASHES: AtomicU32 = AtomicU32::new(0);
/// Bytes read from all PTYs.
pub static BYTES_READ: AtomicU64 = AtomicU64::new(0);
/// Screen frames sent to clients.
pub static FRAMES_SENT: AtomicU64 = AtomicU64::new(0);
static CRASH_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The running totals, for the handoff image.
pub fn counters() -> [u64; 3] {
    [
        BYTES_READ.load(Ordering::Relaxed),
        FRAMES_SENT.load(Ordering::Relaxed),
        CRASHES.load(Ordering::Relaxed).into(),
    ]
}

/// Carry on from the previous image's totals.
pub fn restore_counters([bytes, frames, crashes]: [u64; 3]) {
    BYTES_READ.fetch_add(bytes, Ordering::Relaxed);
    FRAMES_SENT.fetch_add(frames, Ordering::Relaxed);
    CRASHES.fetch_add(crashes as u32, Ordering::Relaxed);
}

/// Write a crash report; returns its path.
pub fn report(what: &str, detail: &str) -> Option<PathBuf> {
    let dir = CRASH_DIR.get()?;
    let _ = std::fs::create_dir_all(dir);
    let ts = ptyro_core::now_ms();
    let path = dir.join(format!("{ts}-{}.txt", std::process::id()));
    let text = format!(
        "ptyro {} crash report\nwhen: {ts} (unix ms)\nwhat: {what}\nos: {} {}\n\n{detail}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    std::fs::write(&path, text).ok()?;
    // Keep the newest 50.
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for old in files.iter().take(files.len().saturating_sub(50)) {
            let _ = std::fs::remove_file(old);
        }
    }
    Some(path)
}

/// Every panic leaves a report (message, location, backtrace) in `state/crashes/`.
pub fn install_panic_hook(state: &Path) {
    let _ = CRASH_DIR.set(state.join("crashes"));
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        CRASHES.fetch_add(1, Ordering::Relaxed);
        let bt = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current().name().unwrap_or("?").to_owned();
        let detail = format!("thread: {thread}\npanic: {info}\n\nbacktrace:\n{bt}");
        let path = report("panic", &detail);
        tracing::error!(
            "panic on {thread}: {info} (report: {})",
            path.map(|p| p.display().to_string()).unwrap_or_default()
        );
        prev(info);
    }));
}

/// The bytes a crash happened on, for the report: printable as-is, the rest escaped.
pub fn escape(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        match b {
            b'\n' => s.push_str("\\n\n"),
            b'\r' => s.push_str("\\r"),
            0x1b => s.push_str("\\e"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}

/// How late the runtime runs: a 1 s tick, measured.
#[derive(Default)]
pub struct Lag {
    samples: Mutex<VecDeque<u64>>,
}

impl Lag {
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let t = Instant::now();
                tokio::time::sleep(Duration::from_secs(1)).await;
                let late = t.elapsed().saturating_sub(Duration::from_secs(1)).as_millis() as u64;
                let mut s = me.samples.lock().expect("lag");
                s.push_back(late);
                if s.len() > 60 {
                    s.pop_front();
                }
            }
        });
    }

    fn stats(&self) -> (u64, u64) {
        let s = self.samples.lock().expect("lag");
        let mut v: Vec<u64> = s.iter().copied().collect();
        v.sort_unstable();
        let max = v.last().copied().unwrap_or(0);
        let p99 = v.get((v.len() * 99 / 100).min(v.len().saturating_sub(1))).copied().unwrap_or(0);
        (max, p99)
    }
}

fn rss_kib() -> u64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            })
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }
}

fn open_fds() -> u32 {
    let dir = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    std::fs::read_dir(dir).map_or(0, |d| d.count() as u32)
}

impl Daemon {
    pub(crate) fn metrics(&self) -> Metrics {
        let (loop_lag_ms_max, loop_lag_ms_p99) = self.lag.stats();
        let mut sessions = Vec::new();
        let mut total = 0;
        for s in self.sessions() {
            let b = s.memory_estimate();
            total += b;
            sessions.push((s.meta.lock().expect("meta").name.clone(), b));
        }
        sessions.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
        Metrics {
            rss_kib: rss_kib(),
            open_fds: open_fds(),
            tasks_alive: tokio::runtime::Handle::current().metrics().num_alive_tasks() as u32,
            loop_lag_ms_max,
            loop_lag_ms_p99,
            pty_readers: self.io.readers.load(Ordering::Relaxed) as u32,
            bytes_read: BYTES_READ.load(Ordering::Relaxed),
            frames_sent: FRAMES_SENT.load(Ordering::Relaxed),
            crashes: CRASHES.load(Ordering::Relaxed),
            scrollback_bytes: total,
            sessions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_bytes_for_reports() {
        assert_eq!(escape(b"a\x1b[1m\x07\n"), "a\\e[1m\\x07\\n\n");
    }
}
