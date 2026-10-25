//! Reaching a daemon on another machine (SPEC §16.2, M9.1): `ssh host ptyro proxy` carries the
//! protocol over ssh's stdio, so there are no ports and no new authentication. ssh runs with
//! `BatchMode`: the TUI owns the terminal, so it can't ask for a password (use keys or an agent).

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, Command};
use tokio::task::JoinHandle;

use crate::ClientError;

/// `[ssh://][user@]host[:port]` → (`[user@]host`, port). An IPv6 address is written
/// `[fe80::1]:22` (or bare, with no port): ssh wants it without the brackets.
pub fn parse(target: &str) -> (String, Option<u16>) {
    let t = target.strip_prefix("ssh://").unwrap_or(target).trim_end_matches('/');
    let (user, host) = match t.rsplit_once('@') {
        Some((u, h)) => (format!("{u}@"), h),
        None => (String::new(), t),
    };
    if let Some(rest) = host.strip_prefix('[')
        && let Some((addr, after)) = rest.split_once(']')
    {
        let port = after.strip_prefix(':').and_then(|p| p.parse::<u16>().ok());
        return (format!("{user}{addr}"), port);
    }
    // Two or more colons and no brackets: an IPv6 address, which has no port to split off.
    if host.matches(':').count() == 1
        && let Some((h, port)) = host.rsplit_once(':')
        && let Ok(p) = port.parse::<u16>()
    {
        return (format!("{user}{h}"), Some(p));
    }
    (format!("{user}{host}"), None)
}

/// The host alone, for the audit trail.
pub fn host_of(target: &str) -> String {
    let (dest, _) = parse(target);
    dest.rsplit_once('@').map_or(dest.clone(), |(_, h)| h.to_owned())
}

/// The command run on the far side. Non-interactive ssh often lacks the PATH entries ptyro is
/// installed under, so the usual places come first; `sh -c` keeps it independent of the
/// remote login shell (fish splits `$PATH` differently). The binary goes in as `$0`, quoted for
/// the login shell, so nothing in it is ever part of the script.
pub fn remote_command(bin: &str) -> String {
    let bin = if bin.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        bin.to_owned()
    } else {
        format!("'{}'", bin.replace('\'', "'\\''"))
    };
    format!(
        "sh -c 'PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\" exec \"$0\" proxy' {bin}"
    )
}

/// Start ssh to `target` running `ptyro proxy`. `PTYRO_SSH` replaces the ssh command (e.g.
/// `tailscale ssh`, or a test double); `PTYRO_REMOTE_BIN` names ptyro on the far side.
pub fn ssh(target: &str) -> Result<Child, ClientError> {
    let prog = std::env::var("PTYRO_SSH").unwrap_or_else(|_| "ssh".into());
    let mut words = prog.split_whitespace();
    let exe = words.next().unwrap_or("ssh").to_owned();
    let (dest, port) = parse(target);
    if dest.is_empty() || dest.starts_with('-') {
        return Err(ClientError::Unavailable(format!("bad remote target {target:?}")));
    }
    let bin = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
    let mut cmd = Command::new(&exe);
    cmd.args(words)
        .args(["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15"])
        .args(port.map(|p| vec!["-p".to_owned(), p.to_string()]).unwrap_or_default())
        .arg("--")
        .arg(&dest)
        .arg(remote_command(&bin))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn().map_err(|e| ClientError::Unavailable(format!("cannot run {exe}: {e}")))
}

/// ssh's stderr, kept for the error message (capped).
pub fn collect(mut err: ChildStderr) -> JoinHandle<String> {
    tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = (&mut err).take(16 * 1024).read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// A failed remote handshake, with what ssh said.
pub async fn explain(
    e: ClientError,
    target: &str,
    ssh: &mut Child,
    stderr: Option<JoinHandle<String>>,
) -> ClientError {
    if matches!(e, ClientError::Rejected(_)) {
        return e;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), ssh.wait()).await;
    let _ = ssh.start_kill();
    let said = match stderr {
        Some(h) => tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default(),
        None => String::new(),
    };
    let said: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = said[said.len().saturating_sub(3)..].join(" / ");
    ClientError::Unavailable(format!(
        "{target}: {}{}. Check that `ssh {} ptyro --version` works without a password prompt \
         (keys or ssh-agent; --remote can't ask)",
        e,
        if tail.is_empty() { String::new() } else { format!(" (ssh: {tail})") },
        parse(target).0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(parse("ssh://me@box:2222"), ("me@box".into(), Some(2222)));
        assert_eq!(parse("box"), ("box".into(), None));
        assert_eq!(parse("me@box.tail.ts.net"), ("me@box.tail.ts.net".into(), None));
        assert_eq!(host_of("ssh://me@box:22"), "box");
        // IPv6: brackets with a port, or a bare address with none.
        assert_eq!(parse("[::1]:2222"), ("::1".into(), Some(2222)));
        assert_eq!(parse("ssh://me@[fe80::1]:22"), ("me@fe80::1".into(), Some(22)));
        assert_eq!(parse("me@fe80::1"), ("me@fe80::1".into(), None));
        assert_eq!(parse("[fe80::1]"), ("fe80::1".into(), None));
        assert_eq!(host_of("me@[fe80::1]:22"), "fe80::1");
        assert!(remote_command("ptyro").ends_with("exec \"$0\" proxy' ptyro"));
        assert!(remote_command("/opt/my ptyro").ends_with(" '/opt/my ptyro'"));
    }

    #[test]
    fn remote_bin_cannot_escape_the_script() {
        // The login shell then `sh -c` see exactly one argument, whatever the name holds.
        for bin in ["/opt/it's here/ptyro", "x'; touch /tmp/pwned; '", "a\"b$(id)"] {
            let cmd = remote_command(bin);
            let out = std::process::Command::new("sh")
                .args(["-c", &cmd.replace(" exec ", " printf %s ")])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{bin}proxy"), "{cmd}");
        }
    }

    #[test]
    fn option_like_targets_are_refused() {
        assert!(ssh("-oProxyCommand=evil").is_err());
        assert!(ssh("ssh://-oProxyCommand=evil").is_err());
    }
}
