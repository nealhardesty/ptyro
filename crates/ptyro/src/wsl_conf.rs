//! `ptyro daemon keep-alive` (ROADMAP WIN2 "Survival", SPEC §20.10): keep the daemon, and the
//! agents in it, running when no terminal is open.
//!
//! - **Inside WSL** that takes systemd as PID 1 (`[boot] systemd=true` in `/etc/wsl.conf`) and
//!   linger for this user. With both, all three `make spike-wsl` heartbeats lived 9 h 26 min
//!   overnight with every terminal closed (WSL 2.6.2, 2026-11-03). Without systemd, reports say
//!   WSL stops the distro 15–20 s after the last terminal closes \[S\].
//! - **On plain Linux** linger keeps it running after you log out.
//! - **macOS** needs nothing.
//!
//! The doctor's `wsl survival` check says which is missing; its fix is this command. Turning on
//! linger is one `loginctl` call (polkit lets you set your own, so no password). Turning on
//! systemd edits `/etc/wsl.conf` with sudo after asking, and takes one WSL restart.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use ptyro_core::{Error, ErrorCode};

use crate::doctor::Level;

const WSL_CONF: &str = "/etc/wsl.conf";
const LINGER_DIR: &str = "/var/lib/systemd/linger";

/// What keeps the daemon running here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub wsl: bool,
    /// systemd is PID 1.
    pub systemd: bool,
    /// systemd keeps this user's services running with no login session.
    pub linger: bool,
}

impl State {
    pub fn now() -> State {
        State {
            wsl: ptyro_core::wsl::detect().is_some(),
            systemd: systemd_on(),
            linger: user().is_some_and(|u| linger_file(&u).exists()),
        }
    }
}

pub fn systemd_on() -> bool {
    std::fs::read_to_string("/proc/1/comm").is_ok_and(|c| c.trim() == "systemd")
}

/// Where logind records linger: one empty file per user.
fn linger_file(user: &str) -> PathBuf {
    Path::new(LINGER_DIR).join(user)
}

pub fn user() -> Option<String> {
    std::env::var("USER").ok().filter(|u| !u.is_empty()).or_else(|| {
        let out = std::process::Command::new("id").arg("-un").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|u| !u.is_empty())
    })
}

/// The doctor's `wsl survival` finding. The text names `ptyro daemon keep-alive`, which the TUI's
/// health badge runs in a split and the web's Health page opens in a session.
pub fn verdict(s: State) -> (Level, String) {
    if !s.systemd {
        (
            Level::Warn,
            "systemd is off in this distro, so WSL may stop it, and ptyro's agents with it, soon \
             after you close the last terminal. `ptyro daemon keep-alive` turns it on (it asks \
             for your password to edit /etc/wsl.conf, then WSL restarts once)"
                .into(),
        )
    } else if !s.linger {
        (
            Level::Warn,
            "systemd's linger is off for you, so WSL may stop ptyro's daemon, and its agents, \
             after you close the last terminal. `ptyro daemon keep-alive` turns it on"
                .into(),
        )
    } else {
        (
            Level::Ok,
            "systemd and linger are on: ptyro keeps running with every terminal closed \
             (verified for 9 hours on WSL 2.6.2)"
                .into(),
        )
    }
}

/// `/etc/wsl.conf` with `[boot] systemd=true`: an existing `systemd =` line under `[boot]` is
/// replaced, a `[boot]` without one gets it, and a file without `[boot]` gets the section.
/// Everything else is kept as it was.
pub fn with_systemd(conf: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_boot = false;
    let mut boot_at = None;
    let mut done = false;
    for line in conf.lines() {
        let bare = line.split('#').next().unwrap_or("").trim();
        if let Some(s) = bare.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_boot = s.trim().eq_ignore_ascii_case("boot");
            if in_boot && boot_at.is_none() {
                boot_at = Some(out.len());
            }
        } else if in_boot
            && let Some((k, _)) = bare.split_once('=')
            && k.trim().eq_ignore_ascii_case("systemd")
        {
            if !done {
                out.push("systemd=true".into());
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        match boot_at {
            Some(i) => out.insert(i + 1, "systemd=true".into()),
            None => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push("[boot]".into());
                out.push("systemd=true".into());
            }
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Run `argv` as root: directly when we are root, else through sudo (`-n` without a terminal,
/// so it fails at once instead of waiting for a password nobody can type).
fn sudo(argv: &[&str], input: Option<&str>, dry_run: bool) -> Result<(), Error> {
    use std::io::Write;
    let root = rustix::process::geteuid().is_root();
    let mut full: Vec<&str> = Vec::new();
    if !root {
        full.push("sudo");
        if !std::io::stdin().is_terminal() {
            full.push("-n");
        }
    }
    full.extend_from_slice(argv);
    eprintln!("$ {}", full.join(" "));
    if dry_run {
        return Ok(());
    }
    let mut cmd = std::process::Command::new(full[0]);
    cmd.args(&full[1..]);
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null());
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::new(ErrorCode::Unsupported, format!("{}: {e} (is sudo installed?)", full[0]))
    })?;
    if let Some(text) = input
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin.write_all(text.as_bytes()).map_err(|e| Error::internal(format!("{e}")))?;
    }
    let status = child.wait().map_err(|e| Error::internal(format!("{e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::Unsupported,
            format!(
                "`{}` failed ({status}){}",
                full.join(" "),
                if full.contains(&"-n") {
                    ": it needs your password, so run `ptyro daemon keep-alive` in a terminal"
                } else {
                    ""
                }
            ),
        ))
    }
}

/// Turn on linger: `loginctl` first (polkit allows your own), then the same through sudo.
pub fn enable_linger(user: &str, dry_run: bool) -> Result<(), Error> {
    eprintln!("$ loginctl enable-linger {user}");
    let ok = dry_run
        || std::process::Command::new("loginctl")
            .args(["enable-linger", user])
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    if !ok {
        sudo(&["loginctl", "enable-linger", user], None, dry_run)?;
    }
    if dry_run {
        return Ok(());
    }
    if !linger_file(user).exists() {
        return Err(Error::internal(format!(
            "linger still looks off ({} is missing)",
            linger_file(user).display()
        )));
    }
    eprintln!(
        "linger is on for {user}: systemd keeps your services, and ptyro's daemon, running with \
         no terminal open"
    );
    Ok(())
}

/// Turn on systemd in this WSL distro (asks first unless `yes`): `[boot] systemd=true` in
/// `/etc/wsl.conf` (the old file kept as `/etc/wsl.conf.ptyro-backup`), and linger, recorded
/// now so it holds from the first boot with systemd. `Ok(false)` when it wasn't agreed to.
pub fn enable_systemd(user: &str, dry_run: bool, yes: bool) -> Result<bool, Error> {
    let conf = std::fs::read_to_string(WSL_CONF).unwrap_or_default();
    let new = with_systemd(&conf);
    eprintln!(
        "systemd is off in this distro, so WSL may stop it, and ptyro's agents with it, soon \
         after the last terminal closes. Turning it on adds `[boot] systemd=true` to {WSL_CONF} \
         and turns on linger for {user}; WSL then restarts once."
    );
    if !yes && !dry_run && !crate::cli::confirm("Turn systemd on? (sudo asks for your password)") {
        return Ok(false);
    }
    if Path::new(WSL_CONF).exists() {
        sudo(&["cp", "-p", WSL_CONF, "/etc/wsl.conf.ptyro-backup"], None, dry_run)?;
    }
    if dry_run {
        println!("# {WSL_CONF}");
        print!("{new}");
    }
    sudo(&["tee", WSL_CONF], Some(&new), dry_run)?;
    let linger = linger_file(user);
    sudo(&["mkdir", "-p", LINGER_DIR], None, dry_run)?;
    sudo(&["touch", &linger.to_string_lossy()], None, dry_run)?;
    eprintln!("{}", RESTART);
    Ok(true)
}

/// What to do after turning systemd on.
pub const RESTART: &str = "systemd starts the next time WSL does. To restart it now: \
    `ptyro daemon stop --hibernate` (agents come back where they were), then in Windows \
    PowerShell `wsl --shutdown`, and open a WSL terminal again. Every WSL window closes.";

/// `ptyro daemon keep-alive`.
pub fn run(dry_run: bool, yes: bool) -> Result<i32, Error> {
    if cfg!(target_os = "macos") {
        println!(
            "nothing to do: on macOS the daemon keeps running when its terminal closes \
             (`ptyro daemon install-service` also starts it at login)"
        );
        return Ok(0);
    }
    let s = State::now();
    let user =
        user().ok_or_else(|| Error::internal("who am I? neither $USER nor `id -un` says"))?;
    if !s.systemd {
        if !s.wsl {
            println!(
                "systemd isn't running here, so there is nothing for ptyro to turn on: the \
                 daemon keeps running when its terminal closes, until you log out"
            );
            return Ok(0);
        }
        return match enable_systemd(&user, dry_run, yes)? {
            true => Ok(0),
            false => {
                eprintln!("left as it was");
                Ok(1)
            }
        };
    }
    if s.linger {
        println!(
            "already on: systemd runs and linger is on for {user}, so ptyro keeps running with \
             no terminal open{}",
            if s.wsl { " (verified for 9 hours on WSL 2.6.2)" } else { " and after you log out" }
        );
        return Ok(0);
    }
    enable_linger(&user, dry_run)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verdict_names_what_is_missing_and_the_fix() {
        let st = |systemd, linger| State { wsl: true, systemd, linger };
        let (l, t) = verdict(st(false, false));
        assert_eq!(l, Level::Warn);
        assert!(t.contains("systemd is off") && t.contains("`ptyro daemon keep-alive`"), "{t}");
        let (l, t) = verdict(st(false, true));
        assert!(l == Level::Warn && t.contains("systemd is off"), "systemd comes first: {t}");
        let (l, t) = verdict(st(true, false));
        assert!(l == Level::Warn && t.contains("linger is off"), "{t}");
        assert!(t.contains("`ptyro daemon keep-alive`"));
        assert_eq!(verdict(st(true, true)).0, Level::Ok);
    }

    #[test]
    fn turning_systemd_on_dry_runs_every_step_without_asking_or_changing_anything() {
        let before = std::fs::read_to_string(WSL_CONF).ok();
        assert!(enable_systemd("nobody-here", true, false).unwrap(), "a dry run never asks");
        assert_eq!(std::fs::read_to_string(WSL_CONF).ok(), before);
        assert!(!linger_file("nobody-here").exists());
        assert!(enable_linger("nobody-here", true).is_ok());
    }

    #[test]
    fn systemd_goes_into_wsl_conf_and_nothing_else_changes() {
        assert_eq!(with_systemd(""), "[boot]\nsystemd=true\n");
        assert_eq!(
            with_systemd("[user]\ndefault=me"),
            "[user]\ndefault=me\n\n[boot]\nsystemd=true\n",
            "a new section, after a blank line"
        );
        assert_eq!(
            with_systemd("[boot]\nsystemd = false\ncommand = echo hi\n"),
            "[boot]\nsystemd=true\ncommand = echo hi\n",
            "false becomes true in place"
        );
        assert_eq!(
            with_systemd("# mine\n[Boot]\ncommand=x\n[user]\ndefault=me\n"),
            "# mine\n[Boot]\nsystemd=true\ncommand=x\n[user]\ndefault=me\n",
            "a [boot] without the key gets it first"
        );
        assert_eq!(
            with_systemd("[boot]\nsystemd=false\n[boot]\nsystemd=false\n"),
            "[boot]\nsystemd=true\n[boot]\n",
            "a second setting can't turn it off again"
        );
        let mine = "[automount]\nroot = /win/\n[boot]\nsystemd=true\n";
        assert_eq!(with_systemd(mine), mine, "already on: the same file");
        assert_eq!(
            with_systemd("[network]\nsystemd=false\n"),
            "[network]\nsystemd=false\n\n[boot]\nsystemd=true\n",
            "only [boot]'s key counts"
        );
    }
}
