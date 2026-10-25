//! `ptyro daemon install-service` / `uninstall-service` (M5.7): start the daemon at login with a
//! systemd user unit (Linux) or a launchd agent (macOS).
//!
//! Choices that matter:
//! - The unit records this shell's `PATH` (and `PTYRO_HOME`/XDG overrides): a service manager's
//!   PATH is short, and agents in `~/.local/bin` would otherwise "not exist".
//! - systemd `KillMode=mixed`: `stop` sends SIGTERM to the daemon alone, which stops sessions
//!   gracefully (agents hibernate) before anything is killed.
//! - A daemon that's already running (started on demand) makes the service exit 5; that exit
//!   never triggers a restart loop.
//! - Nothing is started or stopped behind your back: install starts the service only when no
//!   daemon is running, uninstall leaves a running daemon alone.

use std::path::PathBuf;

use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

const UNIT: &str = "ptyro.service";
const LABEL: &str = "dev.ptyro.daemon";

/// Variables the service needs to find the same config, data and agents as this shell.
const CARRY: &[&str] =
    &["PATH", "PTYRO_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"];

fn carried() -> Vec<(&'static str, String)> {
    CARRY.iter().filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))).collect()
}

fn exe() -> Result<String, Error> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| Error::internal(format!("where am I? {e}")))
}

fn systemd_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ptyro_core::config::expand_home("~/.config"));
    base.join("systemd/user").join(UNIT)
}

fn launchd_path() -> PathBuf {
    ptyro_core::config::expand_home("~/Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// systemd quotes: `"…"` with `\` and `"` escaped, `%` doubled (specifiers).
fn sd_quote(s: &str) -> String {
    // (A newline in a value would end the line and start another directive.)
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

pub fn systemd_unit(exe: &str, env: &[(&str, String)]) -> String {
    let mut u = String::from(
        "# Written by `ptyro daemon install-service`; remove with `ptyro daemon uninstall-service`.\n\
         [Unit]\nDescription=ptyro daemon (terminal agent workspace)\nDocumentation=https://github.com/nealhardesty/ptyro\n\n\
         [Service]\nType=simple\n",
    );
    u.push_str(&format!("ExecStart={} daemon start --foreground\n", sd_quote(exe)));
    for (k, v) in env {
        u.push_str(&format!("Environment={}\n", sd_quote(&format!("{k}={v}"))));
    }
    u.push_str(
        "# SIGTERM to the daemon only: it stops sessions gracefully (agents hibernate).\n\
         KillMode=mixed\nTimeoutStopSec=45\nRestart=on-failure\n\
         # 5 = a daemon is already running (started on demand): not a failure to retry.\n\
         RestartPreventExitStatus=5\n\n[Install]\nWantedBy=default.target\n",
    );
    u
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn launchd_plist(exe: &str, env: &[(&str, String)], log: &str) -> String {
    let mut p = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    p.push_str(&format!("  <key>Label</key><string>{LABEL}</string>\n"));
    p.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    for a in [exe, "daemon", "start", "--foreground"] {
        p.push_str(&format!("    <string>{}</string>\n", xml(a)));
    }
    p.push_str("  </array>\n  <key>EnvironmentVariables</key>\n  <dict>\n");
    for (k, v) in env {
        p.push_str(&format!("    <key>{k}</key><string>{}</string>\n", xml(v)));
    }
    p.push_str("  </dict>\n  <key>RunAtLoad</key><true/>\n");
    // Restart after a crash (a fatal signal) only: launchd can't exempt one exit status, and
    // SuccessfulExit=false would rerun "already running" (exit 5) forever.
    p.push_str("  <key>KeepAlive</key>\n  <dict><key>Crashed</key><true/></dict>\n");
    p.push_str("  <key>ProcessType</key><string>Interactive</string>\n");
    p.push_str(&format!(
        "  <key>StandardErrorPath</key><string>{}</string>\n</dict>\n</plist>\n",
        xml(log)
    ));
    p
}

fn sh(argv: &[&str], dry_run: bool) -> Result<(), Error> {
    eprintln!("$ {}", argv.join(" "));
    if dry_run {
        return Ok(());
    }
    let status = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::new(ErrorCode::Unsupported, format!("{}: {e}", argv[0])))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::internal(format!("`{}` failed ({status})", argv.join(" "))))
    }
}

fn write(path: &std::path::Path, text: &str, dry_run: bool) -> Result<(), Error> {
    if dry_run {
        println!("# {}", path.display());
        print!("{text}");
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::internal(format!("{}: {e}", d.display())))?;
    }
    std::fs::write(path, text).map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn daemon_running(ctx: &Ctx) -> bool {
    let lock = ctx.paths.lock_file();
    std::fs::File::open(&lock).is_ok_and(|f| f.try_lock_shared().is_err())
}

/// Where the login service's file is (whether or not it is installed).
pub fn file() -> std::path::PathBuf {
    if cfg!(target_os = "macos") { launchd_path() } else { systemd_path() }
}

/// The login service is installed (`ptyro daemon install-service`).
pub fn installed() -> bool {
    if cfg!(target_os = "macos") { launchd_path().exists() } else { systemd_path().exists() }
}

pub fn install(ctx: &Ctx, dry_run: bool) -> Result<i32, Error> {
    let exe = exe()?;
    let env = carried();
    let running = daemon_running(ctx);
    if cfg!(target_os = "macos") {
        let log = ctx.paths.log_dir().join("launchd.log");
        write(&launchd_path(), &launchd_plist(&exe, &env, &log.to_string_lossy()), dry_run)?;
        let uid = rustix::process::getuid().as_raw().to_string();
        if running {
            eprintln!("a daemon is already running: launchd takes over at your next login");
        } else {
            // Loaded but not running (a stopped job): bootstrap would fail on it, so clear it first.
            let _ = sh(&["launchctl", "bootout", &format!("gui/{uid}/{LABEL}")], dry_run);
            sh(
                &[
                    "launchctl",
                    "bootstrap",
                    &format!("gui/{uid}"),
                    &launchd_path().to_string_lossy(),
                ],
                dry_run,
            )?;
        }
    } else {
        write(&systemd_path(), &systemd_unit(&exe, &env), dry_run)?;
        sh(&["systemctl", "--user", "daemon-reload"], dry_run)?;
        sh(&["systemctl", "--user", "enable", UNIT], dry_run)?;
        if running {
            eprintln!(
                "a daemon is already running (started on demand): systemd takes over at your next login, or now with `ptyro daemon stop --hibernate && systemctl --user start {UNIT}`"
            );
        } else {
            sh(&["systemctl", "--user", "start", UNIT], dry_run)?;
        }
        eprintln!(
            "note: to keep it running after you log out, `loginctl enable-linger $USER` (your choice)"
        );
    }
    Ok(0)
}

pub fn uninstall(ctx: &Ctx, dry_run: bool) -> Result<i32, Error> {
    if cfg!(target_os = "macos") {
        let path = launchd_path();
        let uid = rustix::process::getuid().as_raw().to_string();
        // bootout stops the job, and a daemon launchd started is that job: a running daemon (and
        // the sessions in it) is left alone, as on Linux. Without one it's fine if it isn't loaded.
        if daemon_running(ctx) {
            eprintln!(
                "a daemon is running: it keeps running (launchd forgets the job at your next login); `ptyro daemon stop` stops it"
            );
        } else {
            let _ = sh(&["launchctl", "bootout", &format!("gui/{uid}/{LABEL}")], dry_run);
        }
        remove(&path, dry_run)?;
    } else {
        let path = systemd_path();
        let _ = sh(&["systemctl", "--user", "disable", UNIT], dry_run);
        remove(&path, dry_run)?;
        sh(&["systemctl", "--user", "daemon-reload"], dry_run)?;
        eprintln!("a running daemon keeps running; `ptyro daemon stop` stops it");
    }
    Ok(0)
}

fn remove(path: &std::path::Path, dry_run: bool) -> Result<(), Error> {
    eprintln!("$ rm {}", path.display());
    if dry_run {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("(no service was installed)");
            Ok(())
        }
        Err(e) => Err(Error::internal(format!("{}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newline_in_a_value_cannot_start_a_directive() {
        let u = systemd_unit("/bin/ptyro", &[("PATH", "/a\nExecStartPre=/bin/evil".to_owned())]);
        assert!(!u.lines().any(|l| l.starts_with("ExecStartPre")), "{u}");
        assert!(u.contains("Environment=\"PATH=/a\\nExecStartPre=/bin/evil\""), "{u}");
    }

    #[test]
    fn units_quote_and_carry_the_environment() {
        let env = [
            ("PATH", "/home/me/.local/bin:/usr/bin".to_owned()),
            ("PTYRO_HOME", "/a b/%h".to_owned()),
        ];
        let u = systemd_unit("/opt/my ptyro/ptyro", &env);
        assert!(u.contains("ExecStart=\"/opt/my ptyro/ptyro\" daemon start --foreground"));
        assert!(u.contains("Environment=\"PTYRO_HOME=/a b/%%h\""), "{u}");
        assert!(u.contains("KillMode=mixed") && u.contains("RestartPreventExitStatus=5"));
        let p = launchd_plist("/usr/local/bin/ptyro", &env, "/tmp/l&g");
        assert!(p.contains("<string>/usr/local/bin/ptyro</string>"));
        assert!(p.contains("<key>PATH</key><string>/home/me/.local/bin:/usr/bin</string>"));
        assert!(p.contains("/tmp/l&amp;g") && p.contains("<key>Crashed</key><true/>"));
        assert!(!p.contains("SuccessfulExit"), "exit 5 must not loop");
    }
}
