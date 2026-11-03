//! `/etc/wsl.conf` for `ptyro daemon install-service` inside WSL (ROADMAP WIN2): a systemd user
//! unit needs systemd as PID 1, which WSL starts only with `[boot] systemd=true`. With systemd
//! off, install-service offers to turn it on: it asks, then edits the file through sudo, and
//! WSL restarts once.
//!
//! This is not what keeps ptyro running after the last terminal closes: systemd, linger and a
//! service don't (SPEC §20.10). The daemon's own hold on WSL does (`[daemon] hold_wsl`).

use std::io::IsTerminal;
use std::path::Path;

use ptyro_core::{Error, ErrorCode};

const WSL_CONF: &str = "/etc/wsl.conf";

pub fn systemd_on() -> bool {
    std::fs::read_to_string("/proc/1/comm").is_ok_and(|c| c.trim() == "systemd")
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
    let root = ptyro_core::os::uid() == Some(0);
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
                    ": it needs your password, so run `ptyro daemon install-service` in a terminal"
                } else {
                    ""
                }
            ),
        ))
    }
}

/// Turn on systemd in this WSL distro (asks first): `[boot] systemd=true` in `/etc/wsl.conf`,
/// the old file kept as `/etc/wsl.conf.ptyro-backup`. `Ok(false)` when it wasn't agreed to (or
/// there is no terminal to ask on).
pub fn enable_systemd(dry_run: bool) -> Result<bool, Error> {
    let conf = std::fs::read_to_string(WSL_CONF).unwrap_or_default();
    let new = with_systemd(&conf);
    eprintln!(
        "the login service needs systemd, which is off in this distro. Turning it on adds \
         `[boot] systemd=true` to {WSL_CONF}; WSL then restarts once."
    );
    if !dry_run && !crate::cli::confirm("Turn systemd on? (sudo asks for your password)") {
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
    eprintln!("{RESTART}");
    Ok(true)
}

/// What to do after turning systemd on.
pub const RESTART: &str = "systemd starts the next time WSL does. To restart it now: \
    `ptyro daemon stop --hibernate` (agents come back where they were), then in Windows \
    PowerShell `wsl --shutdown`, and open a WSL terminal again. Every WSL window closes.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turning_systemd_on_dry_runs_every_step_without_changing_anything() {
        let before = std::fs::read_to_string(WSL_CONF).ok();
        assert!(enable_systemd(true).unwrap(), "a dry run never asks");
        assert_eq!(std::fs::read_to_string(WSL_CONF).ok(), before);
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
