//! `ptyro daemon install-service` / `uninstall-service` (M5.7): start the daemon at login with a
//! systemd user unit (Linux), a launchd agent (macOS), or on Windows a per-user logon task in the
//! Task Scheduler (WIN10; never a Windows Service, which runs in session 0 without the user's
//! profile, credentials or desktop, all of which agents need). The task runs the windowless
//! `ptyrow.exe daemon start --spawned`, so no console window opens at login.
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
//! - Inside WSL (WIN2) with systemd off, install enables the unit by hand and offers to turn
//!   systemd on (`crate::wsl_conf`). What keeps the daemon running after the last terminal
//!   closes is its own hold on WSL (`[daemon] hold_wsl`), not the service.

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
    ptyro_core::config::config_home(&ptyro_core::config::expand_home("~"))
        .join("systemd/user")
        .join(UNIT)
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

/// XML text: `&`, `<`, `>` and `"` escaped (a user name or a path may hold them).
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
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

/// The Windows logon task, in a Task Scheduler folder of ptyro's own.
pub const TASK: &str = "\\ptyro\\daemon";

/// [`TASK`], or the test's own (`PTYRO_TEST_SERVICE_TASK`, debug builds), which is never started:
/// a test must not leave a daemon of the real home running.
pub fn task() -> String {
    ptyro_core::test_knob("PTYRO_TEST_SERVICE_TASK").unwrap_or_else(|| TASK.to_owned())
}

/// The logon task (Task Scheduler's XML): when `user` logs on, run `ptyrow` (no window) with
/// `args` to start the daemon, as that user with their interactive login and no more than their own rights.
/// Defined this way (not `schtasks /SC ONLOGON`, whose trigger is anyone's and needs an
/// administrator), it needs none. No time limit, on battery too; a second start is ignored.
pub fn task_xml(user: &str, ptyrow: &str, args: &str) -> String {
    let (user, ptyrow, args, task) = (xml(user), xml(ptyrow), xml(args), xml(&task()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts the ptyro daemon when you log on (ptyro daemon install-service; remove with ptyro daemon uninstall-service).</Description>
    <URI>{task}</URI>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{ptyrow}</Command>
      <Arguments>{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// `schtasks.exe` with `args`, quietly; whether it succeeded, and what it said.
fn schtasks(args: &[&str]) -> Result<(bool, String), Error> {
    let out = std::process::Command::new("schtasks.exe")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::new(ErrorCode::Unsupported, format!("schtasks.exe: {e}")))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok((out.status.success(), said))
}

/// Where the login service is, for people: its file, or on Windows its task.
pub fn file() -> std::path::PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("Task Scheduler {}", task()))
    } else if cfg!(target_os = "macos") {
        launchd_path()
    } else {
        systemd_path()
    }
}

/// The login service is installed (`ptyro daemon install-service`).
pub fn installed() -> bool {
    if cfg!(windows) {
        schtasks(&["/Query", "/TN", &task()]).is_ok_and(|(ok, _)| ok)
    } else if cfg!(target_os = "macos") {
        launchd_path().exists()
    } else {
        systemd_path().exists()
    }
}

/// Windows: register the logon task (`ptyrow.exe` beside this ptyro.exe runs the daemon without
/// a window), and start it now when no daemon runs.
fn install_windows(ctx: &Ctx, dry_run: bool) -> Result<i32, Error> {
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("where am I? {e}")))?;
    let exe = ptyro_core::os::plain(&exe);
    let ptyrow = exe.with_file_name("ptyrow.exe");
    if !ptyrow.is_file() && !dry_run {
        return Err(Error::new(
            ErrorCode::Unsupported,
            format!(
                "{} isn't there: it starts the daemon at login without a window. Install ptyro again (`irm https://github.com/nealhardesty/ptyro/releases/latest/download/install.ps1 | iex`), or build it (`make build-windows`)",
                ptyrow.display()
            ),
        ));
    }
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) if !d.is_empty() => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => return Err(Error::internal("who am I? USERNAME isn't set")),
    };
    // The daemon this computer uses: WSL's (`client.default_remote = "wsl://…"`), which the
    // task boots and starts, or the native one.
    let wsl = ctx.remote.as_deref().filter(|r| ptyro_client::remote::wsl_distro(r).is_some());
    let args = match wsl {
        Some(r) => format!("--remote {r} daemon start"),
        None => "daemon start --spawned".to_owned(),
    };
    let text = task_xml(&user, &ptyrow.to_string_lossy(), &args);
    let task = task();
    let file = ctx.paths.state.join("ptyro-logon-task.xml");
    eprintln!("$ schtasks /Create /TN {task} /XML {} /F", file.display());
    if dry_run {
        print!("{text}");
        return Ok(0);
    }
    // Task Scheduler reads UTF-16 (with its byte order mark), as the XML says.
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let _ = std::fs::create_dir_all(&ctx.paths.state);
    std::fs::write(&file, bytes)
        .map_err(|e| Error::internal(format!("{}: {e}", file.display())))?;
    let made = schtasks(&["/Create", "/TN", &task, "/XML", &file.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&file);
    match made? {
        (true, _) => match wsl {
            Some(r) => eprintln!(
                "the daemon in WSL ({r}) starts when you log on, WSL with it (Task Scheduler {task})"
            ),
            None => eprintln!("the daemon starts when you log on (Task Scheduler {task})"),
        },
        (false, said) => {
            return Err(Error::internal(format!("Task Scheduler refused the logon task: {said}")));
        }
    }
    for k in ["PTYRO_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        let here = std::env::var(k).ok().filter(|v| !v.is_empty());
        let user = user_env(k);
        if here.is_some() && here != user {
            eprintln!(
                "note: {k} is set in this terminal only; the daemon started at logon gets your user's environment. `setx {k} \"{}\"` keeps it for that daemon too",
                here.unwrap_or_default()
            );
        }
    }
    if task != TASK {
        eprintln!("(a test's task: not started)");
    } else if wsl.is_none() && daemon_running(ctx) {
        eprintln!("a daemon is already running: the task takes over at your next logon");
    } else if let (false, said) = schtasks(&["/Run", "/TN", &task])? {
        eprintln!("note: the task didn't start now ({said}); it does at your next logon");
    } else {
        eprintln!("started it now");
    }
    Ok(0)
}

/// The user's own environment variable `k` (what a program started at logon gets), on Windows.
fn user_env(k: &str) -> Option<String> {
    #[cfg(windows)]
    return ptyro_pty::winreg::get_string("Environment", k).filter(|v| !v.is_empty());
    #[cfg(not(windows))]
    {
        let _ = k;
        None
    }
}

/// Windows: the logon task goes; a running daemon keeps running.
fn uninstall_windows(dry_run: bool) -> Result<i32, Error> {
    let task = task();
    eprintln!("$ schtasks /Delete /TN {task} /F");
    if dry_run {
        return Ok(0);
    }
    match schtasks(&["/Delete", "/TN", &task, "/F"])? {
        (true, _) => eprintln!("the daemon no longer starts at logon"),
        (false, _) if !installed() => eprintln!("(no logon task was installed)"),
        (false, said) => {
            return Err(Error::internal(format!("Task Scheduler didn't remove {task}: {said}")));
        }
    }
    remove_task_folder(&task);
    eprintln!("a running daemon keeps running; `ptyro daemon stop` stops it");
    Ok(0)
}

/// The task's Task Scheduler folder (`\ptyro`), when nothing else is in it: `schtasks` can't
/// remove a folder, Task Scheduler's own API can (and refuses one that isn't empty).
fn remove_task_folder(task: &str) {
    let Some((folder, _)) = task.rsplit_once('\\').filter(|(f, _)| !f.is_empty()) else { return };
    let _ = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$s = New-Object -ComObject Schedule.Service; $s.Connect(); \
             $s.GetFolder('\\').DeleteFolder($env:PTYRO_TASK_FOLDER.TrimStart('\\'), 0)",
        ])
        .env("PTYRO_TASK_FOLDER", folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn install(ctx: &Ctx, dry_run: bool) -> Result<i32, Error> {
    if cfg!(windows) {
        return install_windows(ctx, dry_run);
    }
    let exe = exe()?;
    let env = carried();
    let running = daemon_running(ctx);
    if cfg!(target_os = "macos") {
        let log = ctx.paths.log_dir().join("launchd.log");
        write(&launchd_path(), &launchd_plist(&exe, &env, &log.to_string_lossy()), dry_run)?;
        let uid = ptyro_core::os::uid().unwrap_or(0).to_string();
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
        let wsl = ptyro_core::wsl::detect().is_some();
        write(&systemd_path(), &systemd_unit(&exe, &env), dry_run)?;
        if wsl && !crate::wsl_conf::systemd_on() {
            // No systemd to ask yet (WIN2): enable the unit by hand, with the link `systemctl
            // enable` makes, so it starts on the first boot with systemd, and offer to turn it on.
            link_wanted(dry_run)?;
            if !crate::wsl_conf::enable_systemd(dry_run)? {
                eprintln!(
                    "the service is installed, and starts once systemd is on in this distro: \
                     run `ptyro daemon install-service` in a terminal to turn it on"
                );
            }
            return Ok(0);
        }
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

/// `<config>/systemd/user/default.target.wants/ptyro.service`: what `systemctl --user enable`
/// links, made by hand where systemd isn't running yet.
fn wanted_link() -> PathBuf {
    systemd_path().with_file_name("default.target.wants").join(UNIT)
}

fn link_wanted(dry_run: bool) -> Result<(), Error> {
    let link = wanted_link();
    eprintln!("$ ln -sf {} {}", systemd_path().display(), link.display());
    if dry_run {
        return Ok(());
    }
    let fail = |e: std::io::Error| Error::internal(format!("{}: {e}", link.display()));
    if let Some(d) = link.parent() {
        std::fs::create_dir_all(d).map_err(fail)?;
    }
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    return std::os::unix::fs::symlink(systemd_path(), &link).map_err(fail);
    #[cfg(not(unix))]
    Err(Error::new(ErrorCode::Unsupported, "systemd's links are Linux's"))
}

pub fn uninstall(ctx: &Ctx, dry_run: bool) -> Result<i32, Error> {
    if cfg!(windows) {
        return uninstall_windows(dry_run);
    }
    if cfg!(target_os = "macos") {
        let path = launchd_path();
        let uid = ptyro_core::os::uid().unwrap_or(0).to_string();
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
        let systemd = crate::wsl_conf::systemd_on();
        if systemd {
            let _ = sh(&["systemctl", "--user", "disable", UNIT], dry_run);
        } else if !dry_run {
            // Enabled by hand before systemd ran (WSL): the link is all there is to undo.
            let _ = std::fs::remove_file(wanted_link());
        }
        remove(&path, dry_run)?;
        if systemd {
            sh(&["systemctl", "--user", "daemon-reload"], dry_run)?;
        }
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

    /// The logon task runs ptyrow (no window) as this user, at logon, with no more than their
    /// rights and no time limit; a name or path with XML's specials can't change its shape.
    #[test]
    fn the_windows_logon_task_is_this_users_and_windowless() {
        let x = task_xml(
            "WORK\\Ann & <Bo>",
            r"C:\Users\Ann & Bo\ptyro\ptyrow.exe",
            "daemon start --spawned",
        );
        assert!(x.contains("<UserId>WORK\\Ann &amp; &lt;Bo&gt;</UserId>"), "{x}");
        assert!(x.contains(r"<Command>C:\Users\Ann &amp; Bo\ptyro\ptyrow.exe</Command>"), "{x}");
        assert!(x.contains("<Arguments>daemon start --spawned</Arguments>"));
        assert!(x.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(x.contains("<LogonTrigger>") && !x.contains("<BootTrigger>"));
        assert_eq!(x.matches("<UserId>").count(), 2, "the trigger and the principal: this user");
        assert!(x.contains(&format!("<URI>{TASK}</URI>")));
        // WSL's daemon: the task boots the distro and starts it there.
        let x = task_xml("me", r"C:\p\ptyrow.exe", "--remote wsl://Ubuntu daemon start");
        assert!(x.contains("<Arguments>--remote wsl://Ubuntu daemon start</Arguments>"), "{x}");
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
