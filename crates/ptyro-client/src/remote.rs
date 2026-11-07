//! Reaching a daemon on another machine (SPEC §16.2, M9.1): `ssh host ptyro proxy` carries the
//! protocol over ssh's stdio, so there are no ports and no new authentication. ssh runs with
//! `BatchMode`: the TUI owns the terminal, so it can't ask for a password (use keys or an agent).
//!
//! `wsl://<distro>` (WIN3) is the same over `wsl.exe`: Windows' ptyro reaching the daemon in a
//! WSL distro (`wsl://` alone is the default distro). It works from inside WSL too, through
//! interop, which is how it is tested.

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

/// The command run on a Windows host (WIN11), whose ssh server hands it to `cmd.exe` (its
/// default) or PowerShell: both run `ptyro proxy` as it is, from the `PATH` install.ps1 set, and
/// neither has `sh`. A binary with a space is quoted the way `cmd.exe` takes it (PowerShell
/// would need `&`, which `cmd.exe` refuses: give such a host a path without spaces).
pub fn windows_command(bin: &str) -> String {
    if bin.chars().all(|c| c.is_ascii_alphanumeric() || "\\/:._-".contains(c)) {
        format!("{bin} proxy")
    } else {
        format!("\"{}\" proxy", bin.replace('"', ""))
    }
}

/// Asked of a host when ptyro couldn't start there: `cmd.exe` prints `Windows_NT` for `%OS%`,
/// PowerShell for `$env:OS`, and a POSIX shell neither. No locale changes it.
pub const OS_PROBE: &str = "echo %OS% $env:OS";

/// The ssh command (`PTYRO_SSH`, e.g. `tailscale ssh` or a test double; else `ssh`) to `target`,
/// running `remote`.
fn ssh_command(target: &str, remote: &str) -> Result<Command, ClientError> {
    let prog = std::env::var("PTYRO_SSH").unwrap_or_else(|_| "ssh".into());
    let mut words = prog.split_whitespace();
    let exe = words.next().unwrap_or("ssh").to_owned();
    let (dest, port) = parse(target);
    if dest.is_empty() || dest.starts_with('-') {
        return Err(ClientError::Unavailable(format!("bad remote target {target:?}")));
    }
    let mut cmd = Command::new(&exe);
    cmd.args(words)
        .args(["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15"])
        .args(port.map(|p| vec!["-p".to_owned(), p.to_string()]).unwrap_or_default())
        .arg("--")
        .arg(&dest)
        .arg(remote)
        .kill_on_drop(true);
    Ok(cmd)
}

/// Whether `target` is a Windows host (asked with [`OS_PROBE`], within 15 s).
pub async fn is_windows(target: &str) -> bool {
    let Ok(mut cmd) = ssh_command(target, OS_PROBE) else { return false };
    cmd.stdin(Stdio::null()).stderr(Stdio::null());
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output()).await;
    matches!(out, Ok(Ok(o)) if String::from_utf8_lossy(&o.stdout).contains("Windows_NT"))
}

/// Where the client remembers which ssh hosts are Windows, so it starts the right command
/// there the first time: one `[user@]host[:port]` a line.
fn hosts_file(state: &std::path::Path) -> std::path::PathBuf {
    state.join("ssh-windows-hosts")
}

fn host_key(target: &str) -> String {
    match parse(target) {
        (dest, Some(p)) => format!("{dest}:{p}"),
        (dest, None) => dest,
    }
}

/// `target` was found to be a Windows host.
pub fn windows_hint(state: &std::path::Path, target: &str) -> bool {
    let key = host_key(target);
    std::fs::read_to_string(hosts_file(state)).is_ok_and(|t| t.lines().any(|l| l == key))
}

/// Remember (or forget) that `target` is a Windows host. Best effort.
pub fn set_windows_hint(state: &std::path::Path, target: &str, windows: bool) {
    let key = host_key(target);
    let file = hosts_file(state);
    let mut hosts: Vec<String> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty() && *l != key)
        .map(str::to_owned)
        .collect();
    if windows {
        hosts.push(key);
    }
    let _ = std::fs::create_dir_all(state);
    let _ = std::fs::write(&file, hosts.iter().map(|h| format!("{h}\n")).collect::<String>());
}

/// Start ssh to `target` running `ptyro proxy`: through `sh` on a POSIX host, as it is on a
/// Windows one. `PTYRO_REMOTE_BIN` names ptyro on the far side.
pub fn ssh(target: &str, windows: bool) -> Result<Child, ClientError> {
    let bin = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
    let remote = if windows { windows_command(&bin) } else { remote_command(&bin) };
    let mut cmd = ssh_command(target, &remote)?;
    let exe = cmd.as_std().get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.spawn().map_err(|e| ClientError::Unavailable(format!("cannot run {exe}: {e}")))
}

/// The distro of a `wsl://` target: `wsl://Ubuntu` is `Some("Ubuntu")`, `wsl://` is `Some("")`
/// (the default distro); anything else is `None`.
pub fn wsl_distro(target: &str) -> Option<&str> {
    target.strip_prefix("wsl://").map(|d| d.trim_end_matches('/'))
}

/// What runs inside the distro: `sh` with the places ptyro is installed under put first on
/// `PATH` (as over ssh), then `ptyro proxy`. The binary goes in as `$0`, never into the script.
pub fn proxy_argv(bin: &str) -> Vec<String> {
    [
        "sh",
        "-c",
        "PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\" exec \"$0\" proxy",
        bin,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `wsl.exe`: by name on Windows; inside WSL where Windows keeps it (its folder may not be on
/// `PATH`); `PTYRO_WSL` replaces it (a test double).
pub fn wsl_exe() -> String {
    if let Ok(w) = std::env::var("PTYRO_WSL") {
        return w;
    }
    match ptyro_core::wsl::detect() {
        Some(w) => w.windows_exe("wsl.exe").to_string_lossy().into_owned(),
        None => "wsl.exe".into(),
    }
}

/// The arguments for `wsl.exe` that run `ptyro proxy` in `distro` (empty: the default one).
pub fn wsl_args(distro: &str, bin: &str) -> Result<Vec<String>, ClientError> {
    // A distro's name is letters, digits, `.`, `_` and `-` (WSL's own rule); nothing option-like.
    if distro.starts_with('-')
        || !distro.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return Err(ClientError::Unavailable(format!("bad WSL distro name {distro:?}")));
    }
    let mut args = Vec::new();
    if !distro.is_empty() {
        args.extend(["-d".to_owned(), distro.to_owned()]);
    }
    args.push("--exec".into());
    args.extend(proxy_argv(bin));
    Ok(args)
}

/// `wsl.exe` with `args`, its messages in UTF-8, run in the background: on Windows with no
/// window of its own (`ptyro_core::os::no_window`, SPEC §20.12). For probes and surveys whose
/// output is read, never a terminal's.
pub fn hidden_wsl<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Command {
    let mut cmd = std::process::Command::new(wsl_exe());
    cmd.args(args).env("WSL_UTF8", "1");
    ptyro_core::os::no_window(&mut cmd);
    Command::from(cmd)
}

/// Start `ptyro proxy` in a WSL distro through `wsl.exe`.
pub fn wsl(distro: &str) -> Result<Child, ClientError> {
    let exe = wsl_exe();
    let bin = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
    let mut cmd = std::process::Command::new(&exe);
    // wsl.exe's own messages ("There is no distribution…") in UTF-8, not UTF-16.
    cmd.args(wsl_args(distro, &bin)?).env("WSL_UTF8", "1");
    // The bridge's bytes are the protocol, on pipes: no window of its own (SPEC §20.12).
    ptyro_core::os::no_window(&mut cmd);
    Command::from(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ClientError::Unavailable(format!("cannot run {exe}: {e} (is WSL installed?)")))
}

/// A failed handshake through `wsl.exe`, with what it (or ptyro in the distro) said.
pub async fn explain_wsl(
    e: ClientError,
    distro: &str,
    child: &mut Child,
    stderr: Option<JoinHandle<String>>,
) -> ClientError {
    if matches!(e, ClientError::Rejected(_)) {
        return e;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = child.start_kill();
    let said = match stderr {
        Some(h) => tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default(),
        None => String::new(),
    };
    // Older wsl.exe ignores WSL_UTF8 and writes UTF-16: drop its NULs.
    let said = said.replace('\0', "");
    let said: Vec<&str> = said.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = said[said.len().saturating_sub(2)..].join(" / ");
    // wsl.exe says "there is no distribution" on stdout, where the protocol was expected, so the
    // error above can be a framing one: ask it which distros there are instead.
    if !distro.is_empty()
        && let Some(names) = distros().await
        && !names.iter().any(|n| n.eq_ignore_ascii_case(distro))
    {
        return ClientError::Unavailable(format!(
            "wsl://{distro}: there is no WSL distro named {distro}{}",
            if names.is_empty() {
                " (WSL has none: `wsl --install`)".to_owned()
            } else {
                format!("; yours: {}", names.join(", "))
            }
        ));
    }
    let d = if distro.is_empty() { String::new() } else { format!(" -d {distro}") };
    ClientError::Unavailable(format!(
        "wsl://{distro}: {e}{}. Check that `wsl{d} -- ptyro --version` works: ptyro must be \
         installed in that distro (its README has the one-line install)",
        if tail.is_empty() { String::new() } else { format!(" (wsl: {tail})") },
    ))
}

/// `ptyro --version` in a WSL distro (`ptyro 0.0.48`), found where `proxy` looks; `None` when it
/// isn't there or the distro doesn't answer within 20 s (the first start of a distro is slow).
pub async fn version_in(distro: &str) -> Option<String> {
    let bin = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
    let mut args = wsl_args(distro, &bin).ok()?;
    // The same script as `proxy`, asking for the version instead.
    let script = args.iter().position(|a| a == "-c").map(|i| i + 1)?;
    args[script] = args[script].replace(" proxy", " --version");
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        hidden_wsl(&args).stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && v.starts_with("ptyro ")).then_some(v)
}

/// The distros WSL has (`wsl.exe -l -q`); `None` when it can't say.
pub async fn distros() -> Option<Vec<String>> {
    distros_listed().await.map(|(_, d)| d)
}

/// [`distros`], and whether `wsl -l -q` succeeded (it fails, with its own message as the lines,
/// when WSL isn't installed or has no distro).
pub async fn distros_listed() -> Option<(bool, Vec<String>)> {
    let out = tokio::time::timeout(Duration::from_secs(5), hidden_wsl(&["-l", "-q"]).output())
        .await
        .ok()?
        .ok()?;
    Some((out.status.success(), parse_distros(&out.stdout)))
}

/// `wsl.exe -l -q`'s output: one name a line (CRLF), UTF-8 with `WSL_UTF8`, UTF-16 without.
pub fn parse_distros(out: &[u8]) -> Vec<String> {
    let text = if out.len() >= 2 && out.iter().skip(1).step_by(2).all(|&b| b == 0) {
        let units: Vec<u16> =
            out.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(out).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}'))
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Start `argv` with its stdin and stdout as the protocol stream (MM1.B's bridge).
pub fn spawn(argv: &[String]) -> Result<Child, ClientError> {
    let Some((exe, args)) = argv.split_first() else {
        return Err(ClientError::Unavailable("no command to reach the daemon with".into()));
    };
    Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ClientError::Unavailable(format!("cannot run {exe}: {e}")))
}

/// A failed handshake through a command, with the last lines it wrote to stderr (its reason).
pub async fn explain_command(
    e: ClientError,
    via: &str,
    child: &mut Child,
    stderr: Option<JoinHandle<String>>,
) -> ClientError {
    if matches!(e, ClientError::Rejected(_)) {
        return e;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let _ = child.start_kill();
    let said = match stderr {
        Some(h) => tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default(),
        None => String::new(),
    };
    let said: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
    match said.last() {
        Some(last) => ClientError::Unavailable(format!("{via}: {last}")),
        None => ClientError::Unavailable(format!("{via}: {e}")),
    }
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

    /// A Windows host runs `ptyro proxy` itself (cmd.exe and PowerShell alike; neither has
    /// `sh`); which hosts are Windows is remembered by host and port.
    #[test]
    fn windows_hosts_get_a_command_cmd_and_powershell_run() {
        assert_eq!(windows_command("ptyro"), "ptyro proxy");
        assert_eq!(windows_command(r"C:\Tools\ptyro.exe"), r"C:\Tools\ptyro.exe proxy");
        assert_eq!(windows_command(r"C:\My Tools\ptyro.exe"), r#""C:\My Tools\ptyro.exe" proxy"#);
        assert!(!windows_command("ptyro").contains("sh -c"));
        let d = tempfile::tempdir().unwrap();
        assert!(!windows_hint(d.path(), "me@win:2222"));
        set_windows_hint(d.path(), "ssh://me@win:2222", true);
        set_windows_hint(d.path(), "other", true);
        assert!(windows_hint(d.path(), "me@win:2222"));
        assert!(!windows_hint(d.path(), "me@win"), "another port is another host");
        set_windows_hint(d.path(), "me@win:2222", false);
        assert!(!windows_hint(d.path(), "me@win:2222"));
        assert!(windows_hint(d.path(), "other"));
    }

    #[cfg(unix)] // runs sh
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
    fn wsl_targets_name_a_distro_or_the_default() {
        assert_eq!(wsl_distro("wsl://Ubuntu"), Some("Ubuntu"));
        assert_eq!(wsl_distro("wsl://"), Some(""));
        assert_eq!(wsl_distro("wsl://Ubuntu-24.04/"), Some("Ubuntu-24.04"));
        assert_eq!(wsl_distro("me@box"), None);
        let a = wsl_args("Ubuntu", "ptyro").unwrap();
        assert_eq!(&a[..4], ["-d", "Ubuntu", "--exec", "sh"]);
        assert_eq!(a.last().map(String::as_str), Some("ptyro"));
        assert_eq!(wsl_args("", "ptyro").unwrap()[0], "--exec", "the default distro");
        for bad in ["-d", "--exec", "a b", "x;y", "Ub$untu"] {
            assert!(wsl_args(bad, "ptyro").is_err(), "{bad}");
        }
    }

    #[test]
    fn wsl_lists_its_distros_in_either_encoding() {
        assert_eq!(parse_distros(b"Ubuntu\r\ndocker-desktop\r\n"), ["Ubuntu", "docker-desktop"]);
        let utf16: Vec<u8> =
            "Ubuntu\r\nDebian\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(parse_distros(&utf16), ["Ubuntu", "Debian"]);
        assert!(parse_distros(b"").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_proxy_script_runs_the_binary_it_is_given() {
        let a = proxy_argv("/opt/it's here/ptyro");
        let out = std::process::Command::new(&a[0])
            .arg(&a[1])
            .arg(a[2].replace(" exec ", " printf %s "))
            .arg(&a[3])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/opt/it's here/ptyroproxy");
    }

    #[test]
    fn option_like_targets_are_refused() {
        assert!(ssh("-oProxyCommand=evil", false).is_err());
        assert!(ssh("ssh://-oProxyCommand=evil", true).is_err());
    }
}
