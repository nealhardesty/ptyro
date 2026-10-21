//! Live upgrade by re-exec (SPEC §4.5, M8.1): replace the daemon binary without killing a
//! session.
//!
//! `ptyro daemon upgrade`:
//! 1. checks the new binary runs and speaks this image version;
//! 2. keeps the running binary as `state/daemon.prev` (the rollback);
//! 3. tells clients goodbye ("upgrading"), parks every PTY reader (output waits in the kernel),
//!    flushes the store;
//! 4. writes the handoff image (sessions, emulators, ids) to an anonymous file;
//! 5. clears close-on-exec on that file, the listening socket and every PTY master;
//! 6. `execve`s the new binary with `PTYRO_HANDOFF_FD`.
//!
//! Same pid, so the children stay ours: `waitpid` still works, through [`Reaper`], since the new
//! runtime didn't spawn them. The listening socket never closes, so clients and hooks queue in
//! its backlog for the few milliseconds it takes. If the new binary refuses the image (another
//! version), it re-execs `daemon.prev`, which reads its own image.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::Reply;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::daemon::Daemon;
use crate::session::SessionImage;

/// The handoff image format. A new binary accepts the versions it lists in
/// `ptyro daemon handoff-version`; anything else is refused (and rolled back).
pub const IMAGE_VERSION: u32 = 1;
pub const ENV_FD: &str = "PTYRO_HANDOFF_FD";
pub const ENV_ROLLBACK: &str = "PTYRO_HANDOFF_ROLLBACK";
/// Tests: make the new image refuse the handoff once (rollback is then exercised).
pub const ENV_TEST_REFUSE: &str = "PTYRO_TEST_REFUSE_HANDOFF";

#[derive(Serialize, Deserialize)]
pub struct Image {
    pub version: u32,
    /// The binary that wrote it.
    pub binary: String,
    pub run_id: String,
    pub started_ms: u64,
    /// Upgrades so far (this one included).
    pub generation: u32,
    pub listener_fd: i32,
    pub foreground: bool,
    pub sessions: Vec<SessionImage>,
    pub focused: Option<String>,
    /// OpenCode servers: session → (port, password).
    pub sidecars: Vec<(String, u16, String)>,
    pub written_ms: u64,
    /// The watchdog's running totals (bytes read, frames sent, crashes), so they span upgrades.
    #[serde(default)]
    pub counters: [u64; 3],
}

/// What the new image got from the old one.
pub struct Inherited {
    pub image: Image,
    pub rolled_back: bool,
}

/// Children we didn't spawn in this image (inherited across the re-exec): reaped on SIGCHLD.
#[derive(Default)]
pub struct Reaper {
    pids: Mutex<HashMap<u32, oneshot::Sender<ptyro_pty::handoff::Ended>>>,
}

impl Reaper {
    pub fn watch(&self, pid: u32) -> oneshot::Receiver<ptyro_pty::handoff::Ended> {
        let (tx, rx) = oneshot::channel();
        self.pids.lock().expect("reaper").insert(pid, tx);
        rx
    }

    fn sweep(&self) {
        let mut pids = self.pids.lock().expect("reaper");
        let done: Vec<(u32, ptyro_pty::handoff::Ended)> = pids
            .keys()
            .filter_map(|&p| match ptyro_pty::handoff::try_reap(p) {
                Ok(Some(e)) => Some((p, e)),
                Ok(None) => None,
                // ECHILD: not ours any more (shouldn't happen); report it as gone.
                Err(_) => Some((p, (None, None))),
            })
            .collect();
        for (p, e) in done {
            if let Some(tx) = pids.remove(&p) {
                let _ = tx.send(e);
            }
        }
    }

    /// Reap on every SIGCHLD, and every few seconds in case one was coalesced away.
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut sig =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).ok();
            loop {
                match &mut sig {
                    Some(s) => {
                        let _ = tokio::time::timeout(Duration::from_secs(5), s.recv()).await;
                    }
                    None => tokio::time::sleep(Duration::from_secs(1)).await,
                }
                if !me.pids.lock().expect("reaper").is_empty() {
                    me.sweep();
                }
            }
        });
    }
}

/// The binary that is running now (on Linux even after it was replaced on disk).
pub(crate) fn running_binary() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ptyro"));
    let s = p.to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(orig) => PathBuf::from(orig),
        None => p,
    }
}

/// `binary daemon handoff-version`: the image versions it accepts.
fn accepts(binary: &Path) -> Result<Vec<u32>, String> {
    let out = std::process::Command::new(binary)
        .args(["daemon", "handoff-version"])
        .env_remove(ENV_FD)
        .output()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} can't take a handoff (too old for live upgrades?): {}",
            binary.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect())
}

fn version_of(binary: &Path) -> String {
    std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("ptyro ").to_owned())
        .unwrap_or_default()
}

impl Daemon {
    /// `ptyro daemon upgrade [--binary B]`: check, answer, then hand off a moment later.
    pub(crate) async fn upgrade(self: &Arc<Self>, binary: Option<PathBuf>) -> Result<Reply, Error> {
        let target = binary.unwrap_or_else(running_binary);
        if !target.is_file() {
            return Err(Error::missing("binary", target.display()));
        }
        let versions = accepts(&target).map_err(|e| Error::new(ErrorCode::Unsupported, e))?;
        if !versions.contains(&IMAGE_VERSION) {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "{} reads handoff images {versions:?}, this daemon writes {IMAGE_VERSION}: stop and start the daemon instead (agents hibernate and resume)",
                    target.display()
                ),
            ));
        }
        if self.upgrading.swap(true, Ordering::SeqCst) {
            return Err(Error::conflict("an upgrade is already in progress"));
        }
        // The rollback: the binary running now. /proc/self/exe reads it even if it was replaced.
        let prev = self.paths.state.join("daemon.prev");
        let src = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            running_binary()
        };
        if let Err(e) = std::fs::copy(&src, &prev) {
            self.upgrading.store(false, Ordering::SeqCst);
            return Err(Error::internal(format!(
                "keeping the current binary as {}: {e}",
                prev.display()
            )));
        }
        let to = version_of(&target);
        let sessions = self.sessions().len() as u32;
        let me = Arc::clone(self);
        let target2 = target.clone();
        tokio::spawn(async move {
            // Let the reply reach the client first.
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Err(e) = me.hand_off(&target2).await {
                tracing::error!("upgrade aborted: {e}");
                me.warn(format!("the last daemon upgrade failed and was called off: {e}"));
                me.call_off_upgrade();
            }
        });
        Ok(Reply::Upgrading {
            from: env!("CARGO_PKG_VERSION").into(),
            to,
            binary: target,
            sessions,
        })
    }

    /// Undo what a handoff did before it failed: descriptors private again (they'd leak into
    /// every child), readers back, the accept loop and new connections back to normal.
    fn call_off_upgrade(&self) {
        for s in self.sessions() {
            s.master_private();
        }
        let listener = self.listener_fd.load(Ordering::SeqCst);
        if listener >= 0 {
            let _ = ptyro_pty::handoff::private_raw(listener);
        }
        self.upgrading.store(false, Ordering::SeqCst);
        let _ = self.io_pause.send(false);
        // Wakes the accept loop (it watches `goodbye`) now that `upgrading` is false.
        let _ = self.goodbye.send(None);
    }

    async fn hand_off(self: &Arc<Self>, target: &Path) -> std::io::Result<()> {
        let started = now_ms();
        let _ = self.goodbye.send(Some("the daemon is upgrading; reconnecting".into()));
        // The managed `ptyro serve` (M9.2) goes; the next image starts its own.
        self.serve_child.kill();
        // Park the readers: every byte read is in an emulator, the rest waits in the kernel.
        let _ = self.io_pause.send(true);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while self.io.parked.load(Ordering::SeqCst) < self.io.readers.load(Ordering::SeqCst)
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Writers flush what's queued (keys, query replies).
        tokio::time::sleep(Duration::from_millis(30)).await;
        if let Some(store) = &self.store {
            let _ = store.flush().await;
        }
        // One image per core at once: each is mostly serializing (and compressing) cells.
        let all = self.sessions();
        let sessions = std::thread::scope(|sc| {
            let handles: Vec<_> = all.iter().map(|s| sc.spawn(move || s.image())).collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(std::io::Error::other("image panicked"))))
                .collect::<std::io::Result<Vec<_>>>()
        })?;
        let image = Image {
            version: IMAGE_VERSION,
            binary: env!("CARGO_PKG_VERSION").into(),
            run_id: self.run_id.clone(),
            started_ms: self.started_ms,
            generation: self.generation.load(Ordering::SeqCst) + 1,
            listener_fd: self.listener_fd.load(Ordering::SeqCst),
            foreground: self.foreground.load(Ordering::SeqCst),
            sessions,
            focused: self.focused_id().map(|i| i.full()),
            sidecars: self
                .sidecars
                .lock()
                .expect("sidecars")
                .iter()
                .map(|(k, (p, pw))| (k.full(), *p, pw.clone()))
                .collect(),
            written_ms: now_ms(),
            counters: crate::watchdog::counters(),
        };
        let bytes = rmp_serde::to_vec_named(&image).map_err(std::io::Error::other)?;
        let fd = ptyro_pty::handoff::image_file(&self.paths.state, &bytes)?;
        let raw = ptyro_pty::handoff::inheritable(&fd)?;
        let listener = image.listener_fd;
        ptyro_pty::handoff::inheritable_raw(listener)?;
        tracing::info!(
            sessions = image.sessions.len(),
            bytes = bytes.len(),
            ms = now_ms().saturating_sub(started),
            "handing off to {}",
            target.display()
        );
        let mut cmd = std::process::Command::new(target);
        cmd.args(["daemon", "start", if image.foreground { "--foreground" } else { "--spawned" }])
            .env(ENV_FD, raw.to_string())
            .env_remove(ENV_ROLLBACK);
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        // Still here: the exec failed. Carry on as before.
        drop(fd);
        Err(err)
    }
}

/// At startup: an image handed to us by the previous daemon, if any. A refused image re-execs
/// the previous binary (`daemon.prev`), which wrote it; this never returns in that case.
pub fn take_image(state_dir: &Path) -> Option<Inherited> {
    let raw: i32 = std::env::var(ENV_FD).ok()?.parse().ok()?;
    let rolled_back = std::env::var_os(ENV_ROLLBACK).is_some();
    let fd = match ptyro_pty::handoff::adopt(raw) {
        Ok(fd) => fd,
        Err(e) => {
            tracing::error!("handoff image fd {raw}: {e}; starting fresh");
            return None;
        }
    };
    let dup = rustix::io::fcntl_dupfd_cloexec(&fd, 0).ok();
    let parsed = ptyro_pty::handoff::read_image(fd)
        .map_err(|e| e.to_string())
        .and_then(|b| rmp_serde::from_slice::<Image>(&b).map_err(|e| e.to_string()))
        .and_then(|img| {
            if img.version != IMAGE_VERSION {
                Err(format!("image version {} (this binary reads {IMAGE_VERSION})", img.version))
            } else if std::env::var_os(ENV_TEST_REFUSE).is_some() && !rolled_back {
                Err("refused for a test (PTYRO_TEST_REFUSE_HANDOFF)".into())
            } else {
                Ok(img)
            }
        });
    match parsed {
        Ok(image) => Some(Inherited { image, rolled_back }),
        Err(why) => {
            let prev = state_dir.join("daemon.prev");
            if !rolled_back
                && prev.is_file()
                && let Some(dup) = dup
            {
                tracing::error!("handoff refused ({why}); rolling back to {}", prev.display());
                if let Ok(n) = ptyro_pty::handoff::inheritable(&dup) {
                    use std::os::unix::process::CommandExt;
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let err = std::process::Command::new(&prev)
                        .args(&args)
                        .env(ENV_FD, n.to_string())
                        .env(ENV_ROLLBACK, "1")
                        .exec();
                    tracing::error!("rollback failed: {err}");
                }
            }
            tracing::error!("handoff refused ({why}); starting fresh: the old sessions are lost");
            None
        }
    }
}
