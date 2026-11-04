//! Unix: PTYs through pty-process, signals to process groups, `waitid` and termios probes.

use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::Duration;

use ptyro_core::TermSize;
pub use rustix::process::Signal;

use crate::{PtyChild, SpawnRequest};

/// The PTY's read half.
pub type Reader = pty_process::OwnedReadPty;
/// The PTY's write half (it also resizes).
pub type Writer = pty_process::OwnedWritePty;
/// A duplicate of the PTY's master fd, for the probes.
pub type Master = OwnedFd;

fn pty_size(s: TermSize) -> pty_process::Size {
    pty_process::Size::new(s.rows, s.cols)
}

/// Tell the PTY (and so its program, by `SIGWINCH`) its new size.
pub fn resize(writer: &Writer, size: TermSize) -> io::Result<()> {
    writer.resize(pty_size(size)).map_err(io::Error::other)
}

/// Spawn `argv` as the session leader of a fresh PTY (its own session and process group).
pub fn spawn(req: SpawnRequest<'_>) -> io::Result<PtyChild> {
    let Some((prog, args)) = req.argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    let (pty, pts) = pty_process::open().map_err(io::Error::other)?;
    pty.resize(pty_size(req.size)).map_err(io::Error::other)?;
    let master = rustix::io::fcntl_dupfd_cloexec(pty.as_fd(), 0)?;
    let cmd = pty_process::Command::new(prog)
        .args(args)
        .env_clear()
        .envs(req.env)
        .current_dir(req.cwd)
        .kill_on_drop(false);
    let child = cmd.spawn(pts).map_err(|e| match e {
        pty_process::Error::Io(e) => io::Error::new(e.kind(), format!("{prog}: {e}")),
        other => io::Error::other(other),
    })?;
    let pid = child.id().ok_or_else(|| io::Error::other("child exited immediately"))?;
    let (reader, writer) = pty.into_split();
    Ok(PtyChild { pid, child, reader, writer, master })
}

/// Signal the child's whole process group (the child is its group leader), so tool subprocesses
/// such as dev servers and test runners go with it.
pub fn signal_group(pid: u32, sig: Signal) -> io::Result<()> {
    let pid = rustix::process::Pid::from_raw(pid as i32)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad pid"))?;
    Ok(rustix::process::kill_process_group(pid, sig)?)
}

/// Have Linux kill `cmd`'s process when the thread that starts it ends (`PR_SET_PDEATHSIG`):
/// the caller keeps that thread alive exactly as long as it wants the child. Nothing elsewhere.
#[allow(unsafe_code)] // pre_exec runs one async-signal-safe prctl between fork and exec
pub fn die_with_parent(cmd: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure only calls prctl, which is async-signal-safe, and allocates nothing.
        unsafe {
            cmd.pre_exec(|| {
                rustix::process::set_parent_process_death_signal(Some(Signal::KILL))
                    .map_err(io::Error::from)
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cmd;
}

/// Whether the program in the PTY has the terminal in raw mode (no line editing): full-screen
/// programs and agents do, a shell waiting at its prompt line or `cat` don't.
pub fn is_raw(master: &Master) -> Option<bool> {
    let t = rustix::termios::tcgetattr(master).ok()?;
    Some(!t.local_modes.contains(rustix::termios::LocalModes::ICANON))
}

/// Wait for our child `pid` to exit, leaving it a zombie: its pid, and so its process group's
/// id, can't be given to another process until it is reaped (H2.7: a stop sequence may signal
/// the group after the leader is gone; a recycled id would make that someone else's group).
/// Returns how it ended: (exit code, terminating signal). Reap it with the `Child` afterwards.
pub async fn exited_unreaped(pid: u32) -> io::Result<(Option<i32>, Option<i32>)> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    let p = rustix::process::Pid::from_raw(pid as i32)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "pid 0"))?;
    let mut child = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    loop {
        let opts = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
        if let Some(st) = waitid(WaitId::Pid(p), opts)? {
            return Ok((st.exit_status(), st.terminating_signal()));
        }
        // Signals coalesce, and one may have come before we listened: look again now and then.
        let _ = tokio::time::timeout(Duration::from_secs(1), child.recv()).await;
    }
}

/// The foreground process group of the PTY (e.g. `vim` started from a shell).
#[allow(unsafe_code)] // tcgetpgrp only reads the borrowed fd
pub fn foreground_pid(master: &Master) -> Option<u32> {
    // Not rustix's tcgetpgrp: on macOS it answers 0 once the PTY has no foreground group (its
    // program just exited), and rustix makes a `Pid` of that (a panic in debug builds, UB in
    // release; rustix maps 0 to an error on Linux only).
    // SAFETY: `master` is an open fd for the duration of the call.
    let pgrp = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    u32::try_from(pgrp).ok().filter(|p| *p > 0)
}

/// How a child that was waited for ended: (exit code, terminating signal).
pub fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    (status.code(), status.signal())
}

/// Start `cmd` in a process group of its own, so stopping the group reaches what it starts.
pub fn own_group(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd.process_group(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::exec_env_var;
    use crate::{
        child_env, foreground, others_in_group, session_leader, terminate, until_group_empty,
    };
    use std::ffi::OsStr;
    use std::path::Path;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn spawn_reads_output_with_env_and_size() {
        let argv: Vec<String> = ["sh", "-c", "printf '%s %s ' \"$TERM\" \"$PTYRO\"; stty size"]
            .map(String::from)
            .into();
        let env = child_env(std::env::vars_os(), []);
        let mut c = spawn(SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env,
            size: TermSize::new(77, 9),
        })
        .unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n @ 1..) = c.reader.read(&mut buf).await {
            out.extend_from_slice(&buf[..n]);
        }
        let status = c.child.wait().await.unwrap();
        assert!(status.success());
        assert_eq!(String::from_utf8_lossy(&out).trim(), "xterm-256color 1 9 77");
    }

    #[tokio::test]
    async fn terminate_kills_the_whole_group() {
        // The shell ignores HUP and TERM, and its background child would outlive a plain kill.
        let argv: Vec<String> =
            ["sh", "-c", "trap '' HUP TERM; sleep 100 & sleep 100"].map(String::from).into();
        let env = child_env(std::env::vars_os(), []);
        let mut c = spawn(SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env,
            size: TermSize::new(20, 5),
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pid = c.pid;
        let start = std::time::Instant::now();
        let wait = async {
            let _ = c.child.wait().await;
        };
        terminate(pid, wait, (Duration::from_millis(200), Duration::from_millis(200))).await;
        assert!(start.elapsed() < Duration::from_secs(3));
        // Orphaned members may linger as zombies until init reaps them; signalling a group of
        // zombies still succeeds, so give the reaper a moment.
        let gone = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            signal_group(pid, Signal::KILL).is_err()
        });
        assert!(gone, "group should be gone");
    }

    #[tokio::test]
    async fn an_exit_is_seen_without_reaping_until_the_group_is_empty() {
        // A group of its own, no terminal (whose teardown at the leader's exit would take its
        // members with it): the leader exits at once, its background child lives on.
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "sleep 30 & exit 3"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), exited_unreaped(pid)).await;
        assert_eq!(ended.unwrap().unwrap(), (Some(3), None), "how it ended");
        // Not reaped: it is still there to wait on, so its pid (the group's id) is held. (macOS
        // won't signal a group of zombies, so `kill(-pgid, 0)` is no test of that.)
        let raw = rustix::process::Pid::from_raw(pid as i32).unwrap();
        let held = rustix::process::waitid(
            rustix::process::WaitId::Pid(raw),
            rustix::process::WaitIdOptions::EXITED
                | rustix::process::WaitIdOptions::NOWAIT
                | rustix::process::WaitIdOptions::NOHANG,
        );
        assert!(matches!(held, Ok(Some(_))), "the leader is a zombie still: {held:?}");
        assert!(others_in_group(pid) >= 1, "the background sleep is in the group");
        let early = tokio::time::timeout(Duration::from_millis(300), until_group_empty(pid)).await;
        assert!(early.is_err(), "not empty while the sleep runs");
        let _ = signal_group(pid, Signal::KILL);
        tokio::time::timeout(Duration::from_secs(10), until_group_empty(pid)).await.unwrap();
        assert_eq!(others_in_group(pid), 0);
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(3), "reaped afterwards, the same status");
    }

    #[tokio::test]
    async fn terminate_outlives_the_leader() {
        // The leader dies on HUP; its child ignores HUP and must still be stopped (by TERM).
        let argv: Vec<String> =
            ["sh", "-c", "(trap '' HUP; exec sleep 100) & wait"].map(String::from).into();
        let env = child_env(std::env::vars_os(), []);
        let mut c = spawn(SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env,
            size: TermSize::new(20, 5),
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pid = c.pid;
        let wait = async {
            let _ = c.child.wait().await;
        };
        terminate(pid, wait, (Duration::from_secs(2), Duration::from_secs(2))).await;
        let raw = rustix::process::Pid::from_raw(pid as i32).unwrap();
        let alive = rustix::process::test_kill_process_group(raw).is_ok();
        let _ = signal_group(pid, Signal::KILL);
        assert!(!alive, "terminate returned with the group still alive");
    }

    #[tokio::test]
    async fn probes_place_a_reparented_descendant() {
        // A grandchild started in the background and orphaned (its shell exits) still names the
        // session leader and carries the session's variables (who's calling, M7.6).
        let argv: Vec<String> =
            ["sh", "-c", "sh -c 'sleep 5 & echo $!'; sleep 5"].map(String::from).into();
        let env = child_env(std::env::vars_os(), [("PTYRO_SESSION_ID", OsStr::new("probe"))]);
        let mut c = spawn(SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env,
            size: TermSize::new(20, 5),
        })
        .unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        while !out.contains(&b'\n') {
            let n = c.reader.read(&mut buf).await.unwrap();
            assert!(n > 0);
            out.extend_from_slice(&buf[..n]);
        }
        let grandchild: u32 = String::from_utf8_lossy(&out).trim().parse().unwrap();
        // Past its exec (mid-exec, the kernel shows an empty environment).
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(session_leader(c.pid), Some(c.pid));
        assert_eq!(session_leader(grandchild), Some(c.pid));
        assert_ne!(session_leader(std::process::id()), Some(c.pid));
        #[cfg(target_os = "linux")]
        assert_eq!(exec_env_var(grandchild, "PTYRO_SESSION_ID").as_deref(), Some("probe"));
        let _ = signal_group(c.pid, Signal::KILL);
    }

    #[tokio::test]
    async fn is_raw_follows_the_program() {
        let argv: Vec<String> =
            ["sh", "-c", "sleep 0.5; stty raw -echo; sleep 5"].map(String::from).into();
        let env = child_env(std::env::vars_os(), []);
        let c = spawn(SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env,
            size: TermSize::new(20, 5),
        })
        .unwrap();
        assert_eq!(is_raw(&c.master), Some(false));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while is_raw(&c.master) != Some(true) {
            assert!(tokio::time::Instant::now() < deadline, "never went raw");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = signal_group(c.pid, Signal::KILL);
    }

    /// Claude Code runs as `…/versions/2.1.309` behind a `claude` symlink: the name shown is
    /// argv[0]'s, not the version the kernel reports. Linux only: macOS kills a copied system
    /// binary (it loses its signature), and the probe's parts have unit tests there.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_program_named_like_a_version_shows_its_argv0() {
        let dir = tempfile::tempdir().unwrap();
        // The real file has the version's name (the kernel names a process after the resolved
        // file); `claude` is a symlink to it, as the installer sets it up.
        let exe = dir.path().join("9.8.7");
        let sleep = ["/bin/sleep", "/usr/bin/sleep"].into_iter().find(|p| Path::new(p).exists());
        std::fs::copy(sleep.unwrap(), &exe).unwrap();
        let link = dir.path().join("claude");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        let script = format!("exec {} 3", link.display());
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), script];
        let env = child_env(std::env::vars_os(), []);
        let c =
            spawn(SpawnRequest { argv: &argv, cwd: dir.path(), env, size: TermSize::new(20, 5) })
                .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let name = foreground(&c.master).map(|(n, _)| n);
            if name.as_deref() == Some("claude") {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "shown as {name:?}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let _ = signal_group(c.pid, Signal::KILL);
    }

    #[tokio::test]
    async fn probes_report_foreground_and_cwd() {
        let argv: Vec<String> = ["sh", "-c", "sleep 2"].map(String::from).into();
        let env = child_env(std::env::vars_os(), []);
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let c = spawn(SpawnRequest { argv: &argv, cwd: &dir, env, size: TermSize::new(20, 5) })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (name, cwd) = foreground(&c.master).expect("foreground");
        assert!(name == "sleep" || name == "sh", "{name}");
        assert_eq!(cwd.and_then(|p| p.canonicalize().ok()), Some(dir));
        let _ = signal_group(c.pid, Signal::KILL);
    }
}
