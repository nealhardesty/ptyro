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
//!
//! **Windows (WIN9)** has no `execve`, so the next daemon is another process. The old one parks
//! every reader (each pipe's thread stops, its read cancelled, and what it read goes into the
//! emulator), starts the new binary waiting for its image, copies every live session's handles
//! into it (`DuplicateHandle`: nothing is made inheritable), writes the image with the values the
//! new process has, and waits for it to say it took everything over and listens. Then it exits
//! without closing a thing: the pseudoconsoles, programs and jobs live on in the new process. The
//! pipe's name never goes away (the old daemon's instances last until the new one has its own),
//! so no client starts a daemon of its own meanwhile; the new daemon takes `daemon.lock` once the
//! old has exited. If anything fails, the new process is ended (its copies with it) and the old
//! daemon carries on, as a rollback.

use ptyro_core::sync::MutexExt;
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
#[cfg(unix)]
pub const ENV_ROLLBACK: &str = "PTYRO_HANDOFF_ROLLBACK";
/// The number `daemon.lock` had in the previous image, carried over so the lock is never let go
/// across the exec (H2.7).
#[cfg(unix)]
pub const ENV_LOCK: &str = "PTYRO_HANDOFF_LOCK";
/// Tests: make the new image refuse the handoff once (rollback is then exercised).
pub const ENV_TEST_REFUSE: &str = "PTYRO_TEST_REFUSE_HANDOFF";
/// Windows: the image file the previous daemon writes for the new one (WIN9).
#[cfg(windows)]
pub const ENV_IMAGE: &str = "PTYRO_HANDOFF_IMAGE";
/// Windows: the previous daemon's pid: the new one takes the lock once it has exited.
pub const ENV_PARENT: &str = "PTYRO_HANDOFF_PARENT";

/// Windows: where the new daemon says it took over (it then listens and holds every session).
#[cfg(windows)]
pub fn ack_path(state_dir: &Path, old_pid: u32) -> PathBuf {
    state_dir.join(format!("handoff-{old_pid}.ack"))
}

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
    pids: Mutex<HashMap<u32, oneshot::Sender<Ended>>>,
}

/// How an inherited child ended: (exit code, terminating signal).
pub type Ended = (Option<i32>, Option<i32>);

impl Reaper {
    pub fn watch(&self, pid: u32) -> oneshot::Receiver<Ended> {
        let (tx, rx) = oneshot::channel();
        self.pids.locked().insert(pid, tx);
        rx
    }

    #[cfg(unix)]
    fn sweep(&self) {
        let mut pids = self.pids.locked();
        let done: Vec<(u32, Ended)> = pids
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

    /// Reap on every SIGCHLD, and every few seconds in case one was coalesced away. Windows reaps
    /// nothing: a handed-over program's exit comes from its process handle (WIN9).
    #[cfg(windows)]
    pub fn start(self: &Arc<Self>) {}

    /// Reap on every SIGCHLD, and every few seconds in case one was coalesced away.
    #[cfg(unix)]
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
                if !me.pids.locked().is_empty() {
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

#[cfg(windows)]
impl Daemon {
    /// `ptyro daemon upgrade [--binary B]` on Windows: check, answer, then hand over a moment
    /// later (WIN9). The binary is the one this daemon was started from unless another is named:
    /// an install renames the running `ptyro.exe` aside and puts the new one in its place.
    pub(crate) async fn upgrade(self: &Arc<Self>, binary: Option<PathBuf>) -> Result<Reply, Error> {
        let target = binary.unwrap_or_else(|| crate::started_from().to_path_buf());
        if !target.is_file() {
            return Err(Error::missing("binary", target.display()));
        }
        let probe = target.clone();
        let versions = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || accepts(&probe)),
        )
        .await
        .map_err(|_| {
            Error::new(ErrorCode::Timeout, format!("{} didn't answer in 15 s", target.display()))
        })?
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::new(ErrorCode::Unsupported, e))?;
        if !versions.contains(&IMAGE_VERSION) {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "{} reads handoff images {versions:?}, this daemon writes {IMAGE_VERSION}: restart the daemon instead, `ptyro daemon restart --hibernate` (agents hibernate and resume their conversations; shells and running commands start again)",
                    target.display()
                ),
            ));
        }
        if self.upgrading.swap(true, Ordering::SeqCst) {
            return Err(Error::conflict("an upgrade is already in progress"));
        }
        let probe = target.clone();
        let to = tokio::task::spawn_blocking(move || version_of(&probe)).await.unwrap_or_default();
        let sessions = self.sessions().len() as u32;
        let me = Arc::clone(self);
        let target2 = target.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Err(e) = me.hand_over(&target2).await {
                tracing::error!("upgrade called off: {e}");
                // The fix first: the TUI and the web offer a finding's first `ptyro …` command.
                me.warn(format!(
                    "the last daemon upgrade was called off, and this daemon carries on with every session. `ptyro daemon restart --hibernate` restarts it on the new binary instead (agents resume their conversations; shells and running commands start again). Why: {e}"
                ));
                me.call_off_handover();
            }
        });
        Ok(Reply::Upgrading {
            from: env!("CARGO_PKG_VERSION").into(),
            to,
            binary: target,
            sessions,
        })
    }

    /// Back to normal after a handover that failed: readers read again, connections are taken
    /// again. Said as a rollback (the CLI waits for a new generation and reads `rolled_back`).
    fn call_off_handover(&self) {
        self.upgrading.store(false, Ordering::SeqCst);
        let _ = self.io_pause.send(false);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.rolled_back.store(true, Ordering::SeqCst);
        let _ = self.goodbye.send(None);
    }

    async fn hand_over(self: &Arc<Self>, target: &Path) -> std::io::Result<()> {
        self.wait_for_launches().await?;
        let started = now_ms();
        let _ = self.goodbye.send(Some("the daemon is upgrading; reconnecting".into()));
        self.serve_child.kill();
        let _ = self.io_pause.send(true);
        // A pause cancels each pipe's read (up to 3 s for a stubborn one).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.io.parked.load(Ordering::SeqCst) < self.io.readers.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other(
                    "a session's output didn't stop for the handover, so it would have lost some",
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        if let Some(store) = &self.store {
            let _ = store.flush().await;
        }
        let pid = std::process::id();
        let image_path = self.paths.state.join(format!("handoff-{pid}.img"));
        let ack = ack_path(&self.paths.state, pid);
        let _ = std::fs::remove_file(&image_path);
        let _ = std::fs::remove_file(&ack);
        let foreground = self.foreground.load(Ordering::SeqCst);
        let mut cmd = std::process::Command::new(target);
        cmd.args(["daemon", "start", if foreground { "--foreground" } else { "--spawned" }])
            .env(ENV_IMAGE, &image_path)
            .env(ENV_PARENT, pid.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        ptyro_pty::std_handles_private();
        ptyro_core::os::detach(&mut cmd);
        let mut next = cmd.spawn()?;
        let result = self.write_handover(&mut next, &image_path, &ack, started).await;
        if result.is_err() {
            // Its copies of our handles end with it; ours never moved.
            let _ = next.kill();
            let _ = next.wait();
            let _ = std::fs::remove_file(&image_path);
            let _ = std::fs::remove_file(&ack);
        }
        result
    }

    /// The image, with each live session's handles as `next` has them; then wait for `next` to
    /// say it took over, and go.
    async fn write_handover(
        self: &Arc<Self>,
        next: &mut std::process::Child,
        image_path: &Path,
        ack: &Path,
        started: u64,
    ) -> std::io::Result<()> {
        let all = self.sessions();
        let mut sessions = std::thread::scope(|sc| {
            let handles: Vec<_> = all.iter().map(|s| sc.spawn(move || s.image())).collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(std::io::Error::other("image panicked"))))
                .collect::<std::io::Result<Vec<_>>>()
        })?;
        for (s, img) in all.iter().zip(sessions.iter_mut()) {
            if !s.is_live() {
                continue;
            }
            s.copy_handles_into(next, img)?;
        }
        let image = Image {
            version: IMAGE_VERSION,
            binary: env!("CARGO_PKG_VERSION").into(),
            run_id: self.run_id.clone(),
            started_ms: self.started_ms,
            generation: self.generation.load(Ordering::SeqCst) + 1,
            listener_fd: -1,
            foreground: self.foreground.load(Ordering::SeqCst),
            sessions,
            focused: self.focused_id().map(|i| i.full()),
            sidecars: self
                .sidecars
                .locked()
                .iter()
                .map(|(k, (p, pw))| (k.full(), *p, pw.clone()))
                .collect(),
            written_ms: now_ms(),
            counters: crate::watchdog::counters(),
        };
        let bytes = rmp_serde::to_vec_named(&image).map_err(std::io::Error::other)?;
        // Whole or not at all: the new daemon waits for the name.
        let tmp = image_path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, image_path)?;
        tracing::info!(
            sessions = image.sessions.len(),
            bytes = bytes.len(),
            ms = now_ms().saturating_sub(started),
            pid = next.id(),
            "handing over"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if ack.exists() {
                break;
            }
            if let Some(status) = next.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "the new daemon exited ({status}) before it took over; see `ptyro daemon logs`"
                )));
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other("the new daemon didn't take over within 30 s"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_file(ack);
        tracing::info!(ms = now_ms().saturating_sub(started), "handed over; exiting");
        // Nothing is closed on the way out: every console, program and job is the new daemon's.
        std::process::exit(0)
    }
}

impl Daemon {
    /// A launch that took its claim before `upgrading` was set is still on its way into the
    /// session list (a secret command can take 20 s): an image now would miss it.
    async fn wait_for_launches(&self) -> std::io::Result<()> {
        let wait = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let busy = {
                let c = self.launching.locked();
                !c.0.is_empty() || !c.1.is_empty()
            };
            if !busy {
                return Ok(());
            }
            if std::time::Instant::now() >= wait {
                return Err(std::io::Error::other(
                    "a session is still starting, so the upgrade waited and then gave up; try again in a moment",
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[cfg(unix)]
impl Daemon {
    /// `ptyro daemon upgrade [--binary B]`: check, answer, then hand off a moment later.
    pub(crate) async fn upgrade(self: &Arc<Self>, binary: Option<PathBuf>) -> Result<Reply, Error> {
        let target = binary.unwrap_or_else(running_binary);
        if !target.is_file() {
            return Err(Error::missing("binary", target.display()));
        }
        // Asking the other binary runs it: off the runtime, and not forever.
        let probe = target.clone();
        let versions = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || accepts(&probe)),
        )
        .await
        .map_err(|_| {
            Error::new(ErrorCode::Timeout, format!("{} didn't answer in 15 s", target.display()))
        })?
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::new(ErrorCode::Unsupported, e))?;
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
        // After a rollback this process runs from `daemon.prev` itself: it is already the copy
        // (and writing over a running executable fails, or worse). Otherwise copy beside it and
        // rename, so a crash leaves the old rollback binary whole.
        let already =
            prev.exists() && std::fs::canonicalize(&src).ok() == std::fs::canonicalize(&prev).ok();
        if !already {
            let (from, to_tmp, to_prev) = (src.clone(), prev.with_extension("tmp"), prev.clone());
            let copied = tokio::task::spawn_blocking(move || {
                std::fs::copy(&from, &to_tmp).and_then(|_| std::fs::rename(&to_tmp, &to_prev))
            })
            .await
            .map_err(std::io::Error::other)
            .and_then(|r| r);
            if let Err(e) = copied {
                self.upgrading.store(false, Ordering::SeqCst);
                return Err(Error::internal(format!(
                    "keeping the current binary as {}: {e}",
                    prev.display()
                )));
            }
        }
        let probe = target.clone();
        let to = tokio::task::spawn_blocking(move || version_of(&probe)).await.unwrap_or_default();
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
        // Its terminal would close on the exec if a launch still on its way were left out.
        self.wait_for_launches().await?;
        let started = now_ms();
        let _ = self.goodbye.send(Some("the daemon is upgrading; reconnecting".into()));
        // The managed `ptyro serve` (M9.2) goes; the next image starts its own.
        self.serve_child.kill();
        // So does the hold on WSL (a rollback's supervisor starts it again within 10 s).
        self.wsl_hold.kill();
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
                .locked()
                .iter()
                .map(|(k, (p, pw))| (k.full(), *p, pw.clone()))
                .collect(),
            written_ms: now_ms(),
            counters: crate::watchdog::counters(),
        };
        let bytes = rmp_serde::to_vec_named(&image).map_err(std::io::Error::other)?;
        let fd = ptyro_pty::handoff::image_file(&self.paths.state, &bytes)?;
        let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
        // What the next image keeps goes over one socket (H2.7), so a child spawned meanwhile
        // can't take a PTY master or the lock with it, and the lock is held throughout. Not the
        // listening socket: macOS breaks one that crossed an exec in flight (every connection it
        // then accepts reads EOF after its first message), so it is inherited by number, made so
        // last, just before the exec.
        let lock = crate::LOCK_FD.load(Ordering::SeqCst);
        let mut carried = vec![raw, lock];
        for s in &image.sessions {
            carried.extend(s.master_fd);
            if let Some(f) = s.structured.as_ref().and_then(|st| st.fds) {
                carried.extend(f);
            }
        }
        let sock = ptyro_pty::handoff::carry(&carried)?;
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
            .env(ptyro_pty::handoff::ENV_SOCK, std::os::fd::AsRawFd::as_raw_fd(&sock).to_string())
            .env(ENV_LOCK, lock.to_string())
            .env_remove(ENV_ROLLBACK);
        use std::os::unix::process::CommandExt;
        ptyro_pty::handoff::inheritable_raw(image.listener_fd)?;
        let err = cmd.exec();
        // Still here: the exec failed. Carry on as before (the copies in the socket close with it).
        drop(sock);
        drop(fd);
        Err(err)
    }
}

/// An image's bytes as an [`Image`], or why this binary can't read them.
fn decode(bytes: &[u8]) -> Result<Image, String> {
    let img =
        rmp_serde::from_slice::<Image>(bytes).map_err(|e| format!("unreadable image: {e}"))?;
    if img.version != IMAGE_VERSION {
        return Err(format!("image version {} (this binary reads {IMAGE_VERSION})", img.version));
    }
    Ok(img)
}

/// At startup on Windows: the image the previous daemon writes once it has started us and
/// copied its handles in (WIN9). We wait for it; a refused image ends this process, and the
/// previous daemon, still running, carries on (the rollback).
#[cfg(windows)]
pub fn take_image(_state_dir: &Path) -> Option<Inherited> {
    let path = PathBuf::from(std::env::var_os(ENV_IMAGE)?);
    let parent: u32 = std::env::var(ENV_PARENT).ok()?.parse().ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let bytes = loop {
        if let Ok(b) = std::fs::read(&path) {
            break b;
        }
        if std::time::Instant::now() >= deadline || ptyro_pty::wait_for_exit(parent, Duration::ZERO)
        {
            tracing::error!("no handoff image from pid {parent}; exiting");
            std::process::exit(3);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = std::fs::remove_file(&path);
    let refused = (ptyro_core::test_knob(ENV_TEST_REFUSE).is_some())
        .then(|| "refused for a test (PTYRO_TEST_REFUSE_HANDOFF)".to_owned());
    match refused.map_or_else(|| decode(&bytes), Err) {
        Ok(image) => Some(Inherited { image, rolled_back: false }),
        Err(why) => {
            // The previous daemon sees us exit and carries on with every session.
            tracing::error!("handoff refused ({why}); exiting, the previous daemon carries on");
            std::process::exit(3);
        }
    }
}

/// At startup: an image handed to us by the previous daemon, if any. A refused image re-execs
/// the previous binary (`daemon.prev`), which wrote it; this never returns in that case.
#[cfg(unix)]
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
        .and_then(|b| decode(&b))
        .and_then(|img| {
            if ptyro_core::test_knob(ENV_TEST_REFUSE).is_some() && !rolled_back {
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
                // The older binary inherits by number: what came over the socket goes back to the
                // numbers it had (the lock stays ours until the exec, then is its to take).
                if let Err(e) = ptyro_pty::handoff::restore_carried() {
                    tracing::error!("rollback: carried descriptors: {e}");
                }
                if let Ok(n) = ptyro_pty::handoff::inheritable(&dup) {
                    use std::os::unix::process::CommandExt;
                    let args: Vec<String> = std::env::args().skip(1).collect();
                    let err = std::process::Command::new(&prev)
                        .args(&args)
                        .env(ENV_FD, n.to_string())
                        .env(ENV_ROLLBACK, "1")
                        .env_remove(ptyro_pty::handoff::ENV_SOCK)
                        .env_remove(ENV_LOCK)
                        .exec();
                    tracing::error!("rollback failed: {err}");
                }
            }
            tracing::error!("handoff refused ({why}); starting fresh: the old sessions are lost");
            None
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const FIXTURE: &str = "tests/fixtures/handoff-image-v1.msgpack";

    /// An image like the one the current code writes: one live shell, a focused session, an
    /// OpenCode sidecar. Fixed values, so the bytes are the same on every run.
    fn sample() -> Image {
        let meta = serde_json::json!({
            "name": "build", "kind": "shell", "argv": ["/bin/zsh"], "cwd": "/work",
            "state": {"t": "running"}, "pid": 4242, "client_env": {"TERM": "xterm-256color"},
            "place": {"profile": "default"}, "ports": [3000], "children": [],
            "dangerous": false, "summarizing": false, "prompt_pending": false,
            "turns": 2, "turn_started_ms": 1_700_000_000_000u64,
        });
        let session = SessionImage {
            id: "0190aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee".into(),
            meta: serde_json::from_value(meta).unwrap(),
            status: None,
            activity: Some("tests".into()),
            emu: vec![0x90],
            master_fd: Some(9),
            last_output_ms: 1_700_000_000_500,
            output_bytes: 1234,
            done_means_idle: false,
            stale_after_ms: 30_000,
            engine_clocks: [Some(1), None, Some(3)],
            structured: None,
            win_handles: None,
        };
        Image {
            version: IMAGE_VERSION,
            binary: "0.0.5".into(),
            run_id: "run-fixture".into(),
            started_ms: 1_700_000_000_000,
            generation: 3,
            listener_fd: 4,
            foreground: false,
            focused: Some(session.id.clone()),
            sessions: vec![session],
            sidecars: vec![("0190aaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee".into(), 4096, "pw".into())],
            written_ms: 1_700_000_001_000,
            counters: [10, 20, 1],
        }
    }

    fn fixture_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
    }

    /// `PTYRO_UPDATE_FIXTURES=1 cargo test -p ptyro-daemon handoff` rewrites the fixture. Do
    /// that only together with a new `IMAGE_VERSION` (or a change that older images survive):
    /// the committed file is what the previous release wrote, and an upgrade must read it.
    #[test]
    fn the_committed_image_still_decodes() {
        let path = fixture_path();
        if std::env::var_os("PTYRO_UPDATE_FIXTURES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rmp_serde::to_vec_named(&sample()).unwrap()).unwrap();
        }
        let bytes = std::fs::read(&path).expect("the fixture is committed");
        let img = decode(&bytes).expect("the committed image decodes");
        let want = sample();
        assert_eq!(img.version, want.version);
        assert_eq!(img.generation, 3);
        assert_eq!(img.counters, [10, 20, 1]);
        assert_eq!(img.sidecars, want.sidecars);
        assert_eq!(img.focused, want.focused);
        let (got, want) = (&img.sessions[0], &want.sessions[0]);
        assert_eq!(
            (&got.id, &got.meta.name, got.meta.pid),
            (&want.id, &want.meta.name, Some(4242))
        );
        assert_eq!(got.master_fd, Some(9));
        assert_eq!(got.engine_clocks, [Some(1), None, Some(3)]);
        assert_eq!(got.meta.ports, vec![3000]);
    }

    /// What the code writes now is what the fixture holds: a change to `Image` that alters the
    /// bytes fails here, so it is made on purpose (regenerate, and keep decoding the old one).
    #[test]
    fn the_current_code_writes_the_committed_image() {
        let bytes = std::fs::read(fixture_path()).expect("the fixture is committed");
        assert_eq!(
            rmp_serde::to_vec_named(&sample()).unwrap(),
            bytes,
            "the image format changed: bump IMAGE_VERSION or keep old images readable, then \
             regenerate with PTYRO_UPDATE_FIXTURES=1"
        );
    }

    fn as_value(img: &Image) -> serde_json::Value {
        serde_json::to_value(img).unwrap()
    }

    fn encode(v: &serde_json::Value) -> Vec<u8> {
        rmp_serde::to_vec_named(v).unwrap()
    }

    #[test]
    fn fields_a_newer_ptyro_added_are_ignored() {
        let mut v = as_value(&sample());
        v["from_the_future"] = serde_json::json!({"a": [1, 2]});
        v["sessions"][0]["also_new"] = serde_json::json!(true);
        let img = decode(&encode(&v)).expect("unknown fields are skipped");
        assert_eq!(img.sessions.len(), 1);
    }

    #[test]
    fn fields_an_older_ptyro_never_wrote_take_their_defaults() {
        let mut v = as_value(&sample());
        let obj = v.as_object_mut().unwrap();
        obj.remove("counters");
        let session = obj["sessions"][0].as_object_mut().unwrap().clone();
        let mut session = session;
        for k in ["engine_clocks", "structured"] {
            session.remove(k);
        }
        obj["sessions"][0] = session.into();
        let img = decode(&encode(&v)).expect("optional fields may be missing");
        assert_eq!(img.counters, [0, 0, 0]);
        assert_eq!(img.sessions[0].engine_clocks, [None, None, None]);
        assert!(img.sessions[0].structured.is_none());
    }

    #[test]
    fn an_image_of_another_version_is_refused_with_a_reason() {
        let mut v = as_value(&sample());
        v["version"] = serde_json::json!(IMAGE_VERSION + 1);
        let e = decode(&encode(&v)).err().expect("refused");
        assert!(e.contains("image version") && e.contains("this binary reads"), "{e}");
    }

    #[test]
    fn a_garbage_or_truncated_image_is_refused_not_a_panic() {
        let good = std::fs::read(fixture_path()).unwrap();
        assert!(decode(&[]).is_err());
        assert!(decode(b"not a handoff image").is_err());
        assert!(decode(&[0xff; 64]).is_err());
        for cut in [1, good.len() / 3, good.len() / 2, good.len() - 1] {
            let e = decode(&good[..cut]).err().expect("truncated image refused");
            assert!(e.starts_with("unreadable image"), "{e}");
        }
        // Flipped bytes either still decode or are refused; they never panic.
        for i in (0..good.len()).step_by(7) {
            let mut bad = good.clone();
            bad[i] ^= 0xa5;
            let _ = decode(&bad);
        }
    }
}
