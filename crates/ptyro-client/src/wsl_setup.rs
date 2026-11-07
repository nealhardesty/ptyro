//! Setup's WSL side, from Windows (ROADMAP BOOT8, SPEC §24.3): whether WSL is usable (`wsl.exe`,
//! a distro, and ptyro in it), what is installed there, and installing there. WSL is the user's
//! to bring: nothing here installs WSL, a distro, or ptyro inside one; it says how instead.
//!
//! The distro's own ptyro does the work, reached through `wsl.exe` the way `wsl://` reaches its
//! daemon ([`crate::remote::wsl_exe`], the same places on `PATH`): `ptyro setup --survey` for what
//! is there, and `ptyro install --json` for a run, whose JSON events come back here as they
//! happen. The CLI (`ptyro setup` on Windows) and a native Windows daemon (for the web) both use
//! it, so the two can't drift.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::Duration;

use ptyro_agents::setup::login::LoginRow;
use ptyro_agents::setup::{Event, Plan, Report, Side, Survey, WslStatus};

use crate::remote;

/// What the README installs ptyro with (the release tarball, checked against `SHA256SUMS`).
pub const INSTALL: &str =
    "curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh";

/// Distros that are some tool's own, never where you work.
const NOT_YOURS: &[&str] =
    &["docker-desktop", "docker-desktop-data", "rancher-desktop", "podman-machine"];

/// `wsl -d <distro> -- sh -c "<install line>"`: putting ptyro in a distro, for a fix. Double
/// quotes, which cmd.exe and PowerShell both keep together (the line has no `$`).
pub fn install_line(distro: &str) -> String {
    let d = if distro.is_empty() { String::new() } else { format!(" -d {distro}") };
    format!("wsl{d} -- sh -c \"{INSTALL}\"")
}

/// The distros worth asking, in WSL's order (the default first): tools' own left out, and
/// anything that isn't a distro's name (what `wsl -l -q` says when there are none, or a
/// `--install` from its help).
pub fn candidates(all: &[String]) -> Vec<String> {
    all.iter()
        .filter(|d| !NOT_YOURS.iter().any(|n| d.to_ascii_lowercase().starts_with(n)))
        .filter(|d| !d.starts_with('-'))
        .filter(|d| d.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)))
        .cloned()
        .collect()
}

/// Is WSL usable for setup: `wsl.exe` answers, it has a distro, and ptyro there answers
/// `--version`. `prefer` is the distro the user's daemon is in (`client.default_remote`); it
/// is asked first, then the others in WSL's order.
pub async fn probe(prefer: Option<&str>) -> WslStatus {
    let Some((listed, all)) = remote::distros_listed().await else {
        return WslStatus::Unusable {
            reason: "WSL isn't installed (wsl.exe didn't answer)".into(),
            fix: format!(
                "WSL is yours to set up: `wsl --install` in an administrator terminal, then ptyro in it ({}), then set up again",
                install_line("")
            ),
        };
    };
    let mut names = candidates(&all);
    if names.is_empty() {
        // Windows 11's inbox wsl.exe answers even without WSL, failing with its own message.
        let reason =
            if listed { "WSL has no Linux distro" } else { "WSL isn't installed or has no distro" };
        return WslStatus::Unusable {
            reason: reason.into(),
            fix: format!(
                "`wsl --install` adds one (Ubuntu); then put ptyro in it ({}) and set up again",
                install_line("")
            ),
        };
    }
    if let Some(p) = prefer.filter(|p| !p.is_empty())
        && let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(p))
    {
        let p = names.remove(i);
        names.insert(0, p);
    }
    for d in &names {
        if let Some(version) = remote::version_in(d).await {
            return WslStatus::Usable { distro: d.clone(), version };
        }
    }
    let first = &names[0];
    WslStatus::Unusable {
        reason: format!("ptyro isn't installed in {first} (WSL)"),
        fix: format!("put it there: `{}`, then set up again", install_line(first)),
    }
}

/// `wsl.exe` arguments running the distro's ptyro with `args` (the same `PATH` as `proxy`; the
/// arguments go in as `"$@"`, never into the script).
pub fn ptyro_args(distro: &str, args: &[String]) -> Result<Vec<String>, String> {
    let bin = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
    let mut out = remote::wsl_args(distro, &bin).map_err(|e| e.to_string())?;
    let script = out.iter().position(|a| a == "-c").map(|i| i + 1).ok_or("no script")?;
    out[script] = out[script].replace(" proxy", " \"$@\"");
    out.extend(args.iter().cloned());
    Ok(out)
}

/// The last few lines of what a program said, for an error.
fn tail(text: &str) -> String {
    let text = text.replace('\0', "");
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
}

/// What is installed in `distro`: its ptyro's `setup --survey`. `Err`: why WSL isn't usable
/// after all, with the fix.
pub async fn survey(distro: &str) -> Result<Survey, WslStatus> {
    let unusable = |reason: String, fix: String| WslStatus::Unusable { reason, fix };
    let args = ptyro_args(distro, &["setup".into(), "--survey".into()])
        .map_err(|e| unusable(e, "check the distro's name in `wsl -l`".into()))?;
    let out = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(remote::wsl_exe())
            .args(&args)
            .env("WSL_UTF8", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        unusable(
            format!("ptyro in {distro} (WSL) didn't answer within two minutes"),
            format!("check that `wsl -d {distro} -- ptyro --version` answers, then set up again"),
        )
    })?
    .map_err(|e| {
        unusable(format!("couldn't run wsl.exe: {e}"), "check that WSL works: `wsl -l`".into())
    })?;
    let text = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<Survey>(text.trim()) {
        Ok(s) if out.status.success() => Ok(s),
        _ => {
            let said = tail(&String::from_utf8_lossy(&out.stderr));
            Err(unusable(
                format!(
                    "ptyro in {distro} (WSL) couldn't say what's installed{}: it may be older than setup",
                    if said.is_empty() { String::new() } else { format!(" ({said})") }
                ),
                format!("update it: `{}`, then set up again", install_line(distro)),
            ))
        }
    }
}

/// How the distro's ptyro may get administrator rights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admin {
    /// sudo asks for the password once, in this terminal (through `wsl.exe`): the CLI.
    Ask,
    /// `--yes`: only without a prompt.
    Silent,
    /// Never (a native Windows daemon, for the web): the step shows its command instead.
    Never,
}

/// Install what `plan` ticks for WSL's side in `distro`, through its ptyro (`ptyro install
/// --json`), each event to `on` as it comes. Ends with [`Event::Finished`] always: if the
/// distro's ptyro stops early, what it didn't get to is reported failed, with the fix.
pub fn run(distro: &str, plan: &Plan, admin: Admin, on: &mut dyn FnMut(Event)) -> Report {
    let side = plan.side(Side::Wsl);
    let picks: Vec<String> =
        side.entries.iter().filter(|e| e.picked).map(|e| e.id.clone()).collect();
    let mut args = vec!["install".to_owned(), "--json".to_owned()];
    args.push(
        match admin {
            Admin::Ask => "--admin=ask",
            Admin::Silent => "--admin=silent",
            Admin::Never => "--admin=never",
        }
        .into(),
    );
    args.extend(picks.iter().cloned());
    let shown = format!("wsl -d {distro} -- ptyro install {}", picks.join(" "));
    let mut seen: Vec<String> = Vec::new();
    let mut report: Option<Report> = None;
    let mut forward = |ev: Event, seen: &mut Vec<String>, report: &mut Option<Report>| {
        match &ev {
            Event::Done { id, .. }
            | Event::Failed { id, .. }
            | Event::Skipped { id, .. }
            | Event::Blocked { id, .. } => seen.push(id.clone()),
            Event::Finished { report: r } => *report = Some(r.clone()),
            _ => {}
        }
        on(ev);
    };
    let ended: Result<(), String> = (|| {
        let argv = ptyro_args(distro, &args)?;
        let mut cmd = Command::new(remote::wsl_exe());
        cmd.args(&argv).env("WSL_UTF8", "1").stdout(Stdio::piped());
        match admin {
            // sudo's prompt and the questions after a run are the person's, in this terminal.
            Admin::Ask => cmd.stdin(Stdio::inherit()).stderr(Stdio::inherit()),
            Admin::Silent => cmd.stdin(Stdio::null()).stderr(Stdio::inherit()),
            Admin::Never => cmd.stdin(Stdio::null()).stderr(Stdio::piped()),
        };
        #[cfg(windows)]
        if admin == Admin::Never {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| format!("couldn't run wsl.exe: {e}"))?;
        let errs = child.stderr.take().map(|mut e| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.read_to_string(&mut s);
                s
            })
        });
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Ok(ev) = serde_json::from_str::<Event>(line.trim()) {
                    forward(ev, &mut seen, &mut report);
                }
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let said = errs.and_then(|h| h.join().ok()).map(|s| tail(&s)).unwrap_or_default();
        if status.success() || report.is_some() {
            return Ok(());
        }
        let how =
            status.code().map_or("it was stopped".to_owned(), |c| format!("it exited with {c}"));
        Err(if said.is_empty() { how } else { format!("{how}: {said}") })
    })();
    if let Some(r) = report {
        return r;
    }
    // It ended without its report: what it didn't get to failed, with why.
    let mut r = Report::default();
    for e in side.steps() {
        if seen.contains(&e.id) {
            continue;
        }
        let error = match &ended {
            Err(why) => format!("ptyro in {distro} (WSL) didn't finish: {why}"),
            Ok(()) => format!("ptyro in {distro} (WSL) ended without installing it"),
        };
        forward(
            Event::Failed {
                id: e.id.clone(),
                error,
                fix: format!("run it there to see why: `{shown}`"),
                in_use: false,
            },
            &mut seen,
            &mut None,
        );
        r.failed.push(e.id.clone());
    }
    forward(Event::Finished { report: r.clone() }, &mut seen, &mut None);
    r
}

/// `ptyro <args>` in `distro`, in this terminal: a login there (BOOT4), whose sign-in is the
/// person's to see. Its exit code.
pub fn interact(distro: &str, args: &[String]) -> Result<i32, String> {
    let argv = ptyro_args(distro, args)?;
    let status = Command::new(remote::wsl_exe())
        .args(&argv)
        .env("WSL_UTF8", "1")
        .status()
        .map_err(|e| format!("couldn't run wsl.exe: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

/// Where each login stands in `distro` (`ptyro login --check --json`), each row said to be WSL's.
pub fn logins(distro: &str, ids: &[String]) -> Result<Vec<LoginRow>, String> {
    let mut args = vec!["login".to_owned(), "--check".to_owned(), "--json".to_owned()];
    args.extend(ids.iter().cloned());
    let argv = ptyro_args(distro, &args)?;
    let mut cmd = Command::new(remote::wsl_exe());
    cmd.args(&argv).env("WSL_UTF8", "1").stdin(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("couldn't run wsl.exe: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows: Vec<LoginRow> = serde_json::from_str(text.trim()).map_err(|_| {
        let said = tail(&String::from_utf8_lossy(&out.stderr));
        format!(
            "ptyro in {distro} (WSL) couldn't say{}: it may be older than logins; update it: `{}`",
            if said.is_empty() { String::new() } else { format!(" ({said})") },
            install_line(distro)
        )
    })?;
    for r in &mut rows {
        r.side = Side::Wsl;
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn only_real_distros_are_asked() {
        assert_eq!(
            candidates(&s(&[
                "Ubuntu",
                "docker-desktop",
                "Windows Subsystem for Linux has no installed distributions.",
                "Debian-12",
                "--install",
            ])),
            ["Ubuntu", "Debian-12"]
        );
    }

    #[test]
    fn the_distros_ptyro_runs_with_its_arguments_outside_the_script() {
        let a = ptyro_args("Ubuntu", &s(&["install", "--json", "a'b"])).unwrap();
        assert_eq!(&a[..3], ["-d", "Ubuntu", "--exec"]);
        let script = &a[a.iter().position(|x| x == "-c").unwrap() + 1];
        assert!(script.ends_with("exec \"$0\" \"$@\""), "{script}");
        assert!(!script.contains("a'b"));
        assert_eq!(a.last().unwrap(), "a'b");
        assert!(ptyro_args("-bad", &[]).is_err());
        assert_eq!(install_line("Ubuntu"), format!("wsl -d Ubuntu -- sh -c \"{INSTALL}\""));
        assert!(!install_line("").contains('\''), "cmd.exe doesn't quote with '");
    }
}
