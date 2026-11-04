//! PTYs and the processes in them (SPEC §5.1): spawn with ptyro's environment policy, resize,
//! signal the whole process group, stop gracefully, and probe the foreground process and cwd.

//!
//! What differs by platform is in `unix` (pty-process, signals and process groups) and `windows`
//! (ConPTY and Job Objects, WIN5; until then it compiles and says so), behind the same functions
//! and types: [`Reader`], [`Writer`], [`Master`], [`Signal`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ptyro_core::TermSize;

pub mod acl;
#[cfg(any(windows, test))]
mod cmdline;
pub mod console;
#[cfg(unix)]
pub mod handoff;
#[cfg(windows)]
pub mod pipe;
mod ports;
mod probe;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
pub mod winproc;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
pub use ports::listening_ports;
pub use probe::{
    cwd_of, exec_env_var, local_datetime, local_time_hm, local_time_hm_at, parent_pid, process_name,
};
#[cfg(not(windows))]
pub use probe::{others_in_group, session_leader};
#[cfg(windows)]
pub use windows::*;

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
    // The IDE bridge of a Claude Code the daemon was started from.
    "CLAUDE_CODE_SSE_PORT",
    "ENABLE_IDE_INTEGRATION",
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

/// Prefixes stripped the same way (`CLAUDE_CODE_IDE_*`). Not `CLAUDE_CODE_*`: a profile's own
/// `CLAUDE_CODE_OAUTH_TOKEN` and the like are set for the session and must survive.
pub const STRIP_ENV_PREFIXES: &[&str] = &["CLAUDE_CODE_IDE_"];

pub use ptyro_core::os::CLIENT_ENV;

/// Build the child's complete environment from `base` (the daemon's), applying the strip list,
/// then `extra` (client and ptyro variables), then the terminal identity.
pub fn child_env<'a>(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    extra: impl IntoIterator<Item = (&'a str, &'a OsStr)>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(k, _)| {
            !STRIP_ENV.iter().any(|s| OsStr::new(s) == k)
                && !STRIP_ENV_PREFIXES.iter().any(|p| k.to_string_lossy().starts_with(p))
        })
        .collect();
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
    pub child: Child,
    pub reader: Reader,
    pub writer: Writer,
    /// A duplicate of the master, kept for the foreground and raw-mode probes after the PTY is
    /// split.
    pub master: Master,
}

/// A command for `argv` (a program and its arguments). On Windows, cmd.exe's command text goes
/// raw (`cmdline::cmd_tail`): Rust's quoting is the MSVC rules, which cmd doesn't follow.
pub fn command(argv: &[String]) -> tokio::process::Command {
    let (prog, args) = argv.split_first().map_or(("", &[][..]), |(p, a)| (p.as_str(), a));
    let mut c = tokio::process::Command::new(prog);
    #[cfg(windows)]
    if let Some(tail) = cmdline::cmd_tail(Path::new(prog), args) {
        c.raw_arg(tail);
        return c;
    }
    c.args(args);
    c
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

/// Poll until the group has no members left besides its leader, for up to `wait`. The leader is
/// out of it: ptyro keeps it a zombie until the rest are gone (`exited_unreaped`), so the group's
/// id can't go to another process while a late signal may still be sent to it (H2.7). (Exited
/// members count until they are reaped, which init does for orphans.)
async fn group_gone_within(pid: u32, wait: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if others_in_group(pid) == 0 {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until process group `pgid` has no member but its (zombie) leader, then return: the
/// leader may be reaped.
pub async fn until_group_empty(pgid: u32) {
    while others_in_group(pgid) > 0 {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Name and cwd of the PTY's foreground process.
pub fn foreground(master: &Master) -> Option<(String, Option<PathBuf>)> {
    let pid = foreground_pid(master)?;
    Some((process_name(pid)?, cwd_of(pid)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_claudes_ide_bridge_is_not_inherited_but_a_profiles_login_is() {
        let base = [
            ("CLAUDE_CODE_SSE_PORT", "1234"),
            ("ENABLE_IDE_INTEGRATION", "true"),
            ("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL", "1"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "profile-token"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let env = child_env(base, []);
        let has = |k: &str| env.iter().any(|(ek, _)| ek == k);
        assert!(!has("CLAUDE_CODE_SSE_PORT") && !has("ENABLE_IDE_INTEGRATION"));
        assert!(!has("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL"));
        assert!(has("CLAUDE_CODE_OAUTH_TOKEN") && has("PATH"));
    }

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
}
