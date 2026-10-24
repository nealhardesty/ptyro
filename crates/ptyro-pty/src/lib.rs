//! PTYs and the processes in them (SPEC §5.1): spawn with ptyro's environment policy, resize,
//! signal the whole process group, stop gracefully, and probe the foreground process and cwd.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use pty_process::{OwnedReadPty, OwnedWritePty};
use ptyro_core::TermSize;
pub use rustix::process::Signal;

pub mod handoff;
mod ports;
mod probe;
pub use ports::listening_ports;
pub use probe::{
    cwd_of, exec_env_var, local_datetime, local_time_hm, parent_pid, process_name, session_leader,
};

/// Variables that would make the child talk to the *outer* terminal or multiplexer, or that mark
/// a *parent* Claude Code session (SPEC §5.1, §20.1).
pub const STRIP_ENV: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "STY",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_UNIX_SOCKET",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_LISTEN_ON",
    "ITERM_SESSION_ID",
    "TERM_SESSION_ID",
    "VTE_VERSION",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERMINAL_EMULATOR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_BIN_DIR",
    "WT_SESSION",
    "COLUMNS",
    "LINES",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    // Ours, from an enclosing ptyro session: re-set for the new session where they apply.
    "PTYRO_SESSION_ID",
    "PTYRO_PROFILE",
    "PTYRO_PROJECT",
    "PTYRO_ROOT_PATH",
    "PTYRO_WORKTREE_PATH",
    "PTYRO_WORKTREE_NAME",
    "PTYRO_BRANCH",
    "PTYRO_PORT",
    "PTYRO_PORT_COUNT",
    "PTYRO_SCRATCHPAD",
];

/// Variables a client may pass along with a spawn so sessions see the *client's* SSH agent and
/// display, like tmux's `update-environment` (SPEC §5.1).
pub const CLIENT_ENV: &[&str] =
    &["SSH_AUTH_SOCK", "SSH_CONNECTION", "DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"];

/// Build the child's complete environment from `base` (the daemon's), applying the strip list,
/// then `extra` (client and ptyro variables), then the terminal identity.
pub fn child_env<'a>(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    extra: impl IntoIterator<Item = (&'a str, &'a OsStr)>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> =
        base.into_iter().filter(|(k, _)| !STRIP_ENV.iter().any(|s| OsStr::new(s) == k)).collect();
    let mut set = |k: &str, v: &OsStr| {
        env.retain(|(ek, _)| ek != k);
        env.push((k.into(), v.to_owned()));
    };
    for (k, v) in extra {
        set(k, v);
    }
    set("TERM", OsStr::new("xterm-256color"));
    set("COLORTERM", OsStr::new("truecolor"));
    set("TERM_PROGRAM", OsStr::new("ptyro"));
    set("TERM_PROGRAM_VERSION", OsStr::new(env!("CARGO_PKG_VERSION")));
    set("PTYRO", OsStr::new("1"));
    env
}

pub struct SpawnRequest<'a> {
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub env: Vec<(OsString, OsString)>,
    pub size: TermSize,
}

pub struct PtyChild {
    pub pid: u32,
    pub child: tokio::process::Child,
    pub reader: OwnedReadPty,
    pub writer: OwnedWritePty,
    /// A duplicate of the master fd, kept for `tcgetpgrp` probes after the PTY is split.
    pub master: OwnedFd,
}

pub fn pty_size(s: TermSize) -> pty_process::Size {
    pty_process::Size::new(s.rows, s.cols)
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

/// Whether the program in the PTY has the terminal in raw mode (no line editing): full-screen
/// programs and agents do, a shell waiting at its prompt line or `cat` don't.
pub fn is_raw(master: &OwnedFd) -> Option<bool> {
    let t = rustix::termios::tcgetattr(master).ok()?;
    Some(!t.local_modes.contains(rustix::termios::LocalModes::ICANON))
}

/// The stop sequence (SPEC §5.1): SIGHUP, then SIGTERM after `grace.0`, then SIGKILL after
/// `grace.1`. Returns once the child has exited.
pub async fn terminate(
    pid: u32,
    exited: impl std::future::Future<Output = ()>,
    grace: (Duration, Duration),
) {
    terminate_with(
        pid,
        exited,
        &[(Signal::HUP, grace.0), (Signal::TERM, grace.1), (Signal::KILL, Duration::from_secs(5))],
    )
    .await;
}

/// Send each signal to the group in turn, waiting up to its duration for the exit. Members that
/// ignore a signal can outlive the leader, so after the leader exits the steps go on until the
/// group is empty.
pub async fn terminate_with(
    pid: u32,
    exited: impl std::future::Future<Output = ()>,
    steps: &[(Signal, Duration)],
) {
    tokio::pin!(exited);
    let mut leader_gone = false;
    for &(sig, wait) in steps {
        if signal_group(pid, sig).is_err() {
            // ESRCH: the whole group is already gone.
            return;
        }
        if !leader_gone {
            let start = tokio::time::Instant::now();
            if tokio::time::timeout(wait, &mut exited).await.is_err() {
                continue;
            }
            leader_gone = true;
            if group_gone_within(pid, wait.saturating_sub(start.elapsed())).await {
                return;
            }
        } else if group_gone_within(pid, wait).await {
            return;
        }
    }
}

/// Poll until the group has no members left, for up to `wait`. (Exited members count until
/// they are reaped, which init does for orphans.)
async fn group_gone_within(pid: u32, wait: Duration) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid as i32) else { return true };
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if rustix::process::test_kill_process_group(pid).is_err() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The foreground process group of the PTY (e.g. `vim` started from a shell).
#[allow(unsafe_code)] // tcgetpgrp only reads the borrowed fd
pub fn foreground_pid(master: &OwnedFd) -> Option<u32> {
    // Not rustix's tcgetpgrp: on macOS it answers 0 once the PTY has no foreground group (its
    // program just exited), and rustix makes a `Pid` of that (a panic in debug builds, UB in
    // release; rustix maps 0 to an error on Linux only).
    // SAFETY: `master` is an open fd for the duration of the call.
    let pgrp = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    u32::try_from(pgrp).ok().filter(|p| *p > 0)
}

/// Name and cwd of the PTY's foreground process.
pub fn foreground(master: &OwnedFd) -> Option<(String, Option<PathBuf>)> {
    let pid = foreground_pid(master)?;
    Some((process_name(pid)?, cwd_of(pid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn env_policy_strips_and_sets() {
        let base = vec![
            ("TMUX".into(), "x".into()),
            ("CLAUDE_CODE_MESSAGING_TOKEN".into(), "secret".into()),
            ("HOME".into(), "/h".into()),
            ("TERM".into(), "screen".into()),
        ];
        let env = child_env(base, [("PTYRO_SESSION_ID", OsStr::new("abc"))]);
        let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone());
        assert_eq!(get("TMUX"), None);
        assert_eq!(get("CLAUDE_CODE_MESSAGING_TOKEN"), None);
        assert_eq!(get("HOME"), Some("/h".into()));
        assert_eq!(get("TERM"), Some("xterm-256color".into()));
        assert_eq!(get("PTYRO_SESSION_ID"), Some("abc".into()));
        assert_eq!(env.iter().filter(|(k, _)| k == "TERM").count(), 1);
    }

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
