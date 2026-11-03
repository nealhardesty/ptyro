//! Keeping WSL running while the daemon runs (ROADMAP WIN2, SPEC §20.10).
//!
//! WSL stops a distro about 15 s after the last Windows-side `wsl.exe` that uses it exits,
//! whatever runs inside: a systemd service, or a user service with linger, is stopped too
//! (measured on WSL 2.6.2, 2026-11-03; microsoft/WSL#13416). A `wsl.exe` started from inside
//! the distro, back into it, counts as one of those callers. So the daemon starts one as its
//! child, and WSL stays up exactly while ptyro does:
//!
//! `wsl.exe -d <distro> --cd / --exec /bin/sh -c '<watch the daemon's pid>'`
//!
//! - The inner loop ends when the daemon's pid (and its start time, so a reused pid doesn't
//!   count) is gone, which ends the Windows `wsl.exe`.
//! - The `wsl.exe` here is a Linux process (WSL's interop proxy) that outlives its Windows side
//!   and holds the distro by itself, so the daemon kills it on the way out, and Linux kills it if
//!   the daemon dies first (`PR_SET_PDEATHSIG`, from a thread that lives exactly as long as it).
//! - `[daemon] hold_wsl = false` lets WSL stop on its own schedule; `PTYRO_WSL_HOLD=0` does the
//!   same for one daemon (the test suite sets it, so test daemons don't each start one).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::now_ms;
use ptyro_core::sync::MutexExt;
use ptyro_protocol::WslHold;

use crate::daemon::Daemon;

#[derive(Default)]
struct Run {
    /// The holder's pid (the interop proxy, our child) while it runs.
    pid: Option<u32>,
    /// What its command line carries (`marker`), to find its inner loop.
    marker: Option<String>,
    since_ms: Option<u64>,
    last_exit: Option<String>,
    restarts: u32,
}

#[derive(Default)]
pub struct Holder {
    run: Arc<Mutex<Run>>,
    wake: tokio::sync::Notify,
}

impl Holder {
    /// Stop holding: on shutdown, before a live upgrade, or when the setting goes off. Killing
    /// the proxy isn't enough: the Windows side runs until its inner loop ends, and that loop
    /// only ends by itself once the daemon is gone. So the loop goes too.
    pub fn kill(&self) {
        let mut r = self.run.locked();
        let marker = r.marker.take();
        let pids = r.pid.take().into_iter().chain(marker.iter().flat_map(|m| with_marker(m)));
        for pid in pids {
            if let Some(p) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
                let _ = rustix::process::kill_process(p, rustix::process::Signal::KILL);
            }
        }
        r.since_ms = None;
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

/// Why this daemon can't or won't hold WSL; `None` when it should.
pub fn reason_not(
    wanted: bool,
    env: Option<&str>,
    wsl: Option<&ptyro_core::wsl::Wsl>,
) -> Option<String> {
    let Some(w) = wsl else { return Some("not WSL".into()) };
    if env.is_some_and(|v| v == "0") {
        return Some("PTYRO_WSL_HOLD=0".into());
    }
    if !wanted {
        return Some("[daemon] hold_wsl is off".into());
    }
    if !w.interop {
        return Some("interop is off, so wsl.exe can't be started from here".into());
    }
    if w.distro.is_none() {
        return Some(
            "the distro's name is unknown (no WSL_DISTRO_NAME, and `wslpath -w /` said nothing)"
                .into(),
        );
    }
    None
}

/// When this process started, in clock ticks after boot (field 22 of `/proc/<pid>/stat`, read
/// after the last `)` because a name may hold spaces): with the pid, it names this process.
pub fn start_ticks(stat: &str) -> Option<String> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19).map(str::to_owned)
}

/// What names one daemon's holder in `ps`: its pid and start time.
pub fn marker(pid: u32, ticks: &str) -> String {
    format!(": ptyro-wsl-hold {pid} {ticks};")
}

/// The holder's command line (after `wsl.exe`). It runs as `user`, this daemon's, so the daemon
/// may end it (the distro's default user can be another).
pub fn argv(distro: &str, user: &str, pid: u32, ticks: &str) -> Vec<String> {
    // Field 20 after the `)`: the start time, as `start_ticks` reads it.
    let script = format!(
        "{} while [ \"$(sed 's/.*) //' /proc/{pid}/stat 2>/dev/null | cut -d' ' -f20)\" = {ticks} ]; do sleep 5; done",
        marker(pid, ticks)
    );
    ["-d", distro, "-u", user, "--cd", "/", "--exec", "/bin/sh", "-c", &script]
        .map(str::to_owned)
        .to_vec()
}

/// The processes whose command line holds `marker` (the holder's inner loop, and its proxy).
fn with_marker(marker: &str) -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains(marker))
        })
        .collect()
}

/// This process's user name: `/etc/passwd`'s entry for our uid, else `$USER`.
fn user_name(passwd: &str, uid: u32) -> Option<String> {
    passwd
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2].parse() == Ok(uid)).then(|| f[0].to_owned())
        })
        .or_else(|| std::env::var("USER").ok())
        .filter(|u| !u.is_empty())
}

impl Daemon {
    pub(crate) fn wsl_hold_status(&self) -> Option<WslHold> {
        let wsl = ptyro_core::wsl::detect()?;
        let r = self.wsl_hold.run.locked();
        Some(WslHold {
            holding: r.pid.is_some(),
            pid: r.pid,
            since_ms: r.since_ms,
            why_not: reason_not(
                self.config().daemon.hold_wsl,
                std::env::var("PTYRO_WSL_HOLD").ok().as_deref(),
                Some(wsl),
            ),
            last_exit: r.last_exit.clone(),
            restarts: r.restarts,
        })
    }

    /// Hold WSL while the setting says so: start the holder, start it again if it dies
    /// (backing off), stop it when the setting goes off. Nothing at all outside WSL.
    pub(crate) fn start_wsl_hold(self: &Arc<Self>) {
        let Some(wsl) = ptyro_core::wsl::detect() else { return };
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(2);
            loop {
                let env = std::env::var("PTYRO_WSL_HOLD").ok();
                let want =
                    reason_not(me.config().daemon.hold_wsl, env.as_deref(), Some(wsl)).is_none();
                let running = me.wsl_hold.run.locked().pid.is_some();
                let mut pause = Duration::from_secs(10);
                match (want, running) {
                    (true, false) => match spawn(wsl, &me.wsl_hold.run) {
                        Ok(pid) => {
                            tracing::info!(pid, "holding WSL open while the daemon runs");
                            backoff = Duration::from_secs(2);
                        }
                        Err(e) => {
                            tracing::warn!("can't hold WSL open: {e}");
                            me.wsl_hold.run.locked().last_exit = Some(e);
                            pause = backoff;
                            backoff = (backoff * 2).min(Duration::from_secs(300));
                        }
                    },
                    (false, true) => {
                        tracing::info!("no longer holding WSL open");
                        me.wsl_hold.kill();
                    }
                    _ => {}
                }
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    () = me.wsl_hold.wake.notified() => {}
                }
            }
        });
    }
}

/// Start the holder on a thread of its own that waits for it: `PR_SET_PDEATHSIG` fires when the
/// thread that forked it ends, so that thread must live as long as the holder does.
fn spawn(w: &ptyro_core::wsl::Wsl, run: &Arc<Mutex<Run>>) -> Result<u32, String> {
    use std::os::unix::process::CommandExt as _;
    let distro = w.distro.clone().ok_or("the distro's name is unknown")?;
    let me = std::process::id();
    let stat =
        std::fs::read_to_string("/proc/self/stat").map_err(|e| format!("/proc/self/stat: {e}"))?;
    let ticks = start_ticks(&stat).ok_or("can't read this process's start time")?;
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let user = user_name(&passwd, rustix::process::getuid().as_raw()).ok_or("who am I?")?;
    let exe = w.windows_exe("wsl.exe");
    let args = argv(&distro, &user, me, &ticks);
    let mark = marker(me, &ticks);
    let (tx, rx) = std::sync::mpsc::channel();
    let run = Arc::clone(run);
    std::thread::Builder::new()
        .name("wsl-hold".into())
        .spawn(move || {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0);
            ptyro_pty::die_with_parent(&mut cmd);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("{}: {e}", exe.display())));
                    return;
                }
            };
            let pid = child.id();
            {
                let mut r = run.locked();
                r.pid = Some(pid);
                r.marker = Some(mark);
                r.since_ms = Some(now_ms());
            }
            let _ = tx.send(Ok(pid));
            let status = child.wait();
            let mut r = run.locked();
            // Killed on purpose (`Holder::kill` took the pid): nothing to report.
            if r.pid == Some(pid) {
                r.pid = None;
                r.since_ms = None;
                r.restarts += 1;
                r.last_exit = Some(match status {
                    Ok(s) => format!("wsl.exe exited ({s})"),
                    Err(e) => format!("wsl.exe: {e}"),
                });
            }
        })
        .map_err(|e| format!("thread: {e}"))?;
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "wsl.exe didn't start".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsl(interop: bool, distro: Option<&str>) -> ptyro_core::wsl::Wsl {
        ptyro_core::wsl::Wsl {
            version: 2,
            distro: distro.map(str::to_owned),
            interop,
            mount_root: "/mnt/".into(),
        }
    }

    #[test]
    fn it_holds_only_in_wsl_with_interop_a_distro_and_the_setting() {
        let ok = wsl(true, Some("Ubuntu"));
        assert_eq!(reason_not(true, None, Some(&ok)), None);
        assert_eq!(reason_not(true, Some("1"), Some(&ok)), None);
        assert_eq!(reason_not(true, None, None).as_deref(), Some("not WSL"));
        assert!(reason_not(true, Some("0"), Some(&ok)).unwrap().contains("PTYRO_WSL_HOLD"));
        assert!(reason_not(false, None, Some(&ok)).unwrap().contains("hold_wsl is off"));
        assert!(reason_not(true, None, Some(&wsl(false, Some("U")))).unwrap().contains("interop"));
        assert!(reason_not(true, None, Some(&wsl(true, None))).unwrap().contains("name"));
    }

    #[test]
    fn the_holder_watches_this_pid_and_its_start_time() {
        // A name with a space and a `)`: the fields are counted after the last `)`.
        let stat =
            "4386 (pt yro) x) S 1 4386 4386 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 9 0 812345 1 2";
        assert_eq!(start_ticks(stat).as_deref(), Some("812345"));
        assert_eq!(start_ticks("garbage"), None);
        let a = argv("Ubuntu", "me", 4386, "812345");
        assert_eq!(&a[..8], ["-d", "Ubuntu", "-u", "me", "--cd", "/", "--exec", "/bin/sh"]);
        assert_eq!(a[8], "-c");
        assert!(a[9].starts_with(&marker(4386, "812345")), "{}", a[9]);
        assert!(a[9].contains("/proc/4386/stat") && a[9].contains("= 812345 ]"), "{}", a[9]);
    }

    #[test]
    fn the_user_comes_from_passwd_by_uid() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nneal:x:1000:1000::/home/neal:/bin/zsh\n";
        assert_eq!(user_name(passwd, 1000).as_deref(), Some("neal"));
        assert_eq!(user_name(passwd, 0).as_deref(), Some("root"));
    }

    #[test]
    fn the_marker_finds_the_loop_and_only_ours() {
        let mark = marker(999_999, "42");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("{mark} sleep 30")])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(with_marker(&mark), vec![child.id()]);
        assert!(with_marker(&marker(999_999, "43")).is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_watch_loop_reads_the_same_field_it_was_given() {
        // Run the loop's own pipeline on this process: it must print what start_ticks reads.
        let me = std::process::id();
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let Some(ticks) = start_ticks(&stat) else { return }; // not Linux
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("sed 's/.*) //' /proc/{me}/stat | cut -d' ' -f20")])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), ticks);
    }
}
