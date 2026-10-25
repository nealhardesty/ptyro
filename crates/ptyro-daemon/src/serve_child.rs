//! `[remote] serve = true` (M9.2, WEB1.9): the daemon runs `ptyro serve --managed` itself, restarts
//! it when it dies (backing off), stops it when the setting goes off, and kills it before a live
//! upgrade (the next image starts its own). Its output goes to `state/serve.log`.
//!
//! The setting is a switch anyone with `full` scope can flip (`Request::Web`): the daemon edits
//! the config file, starts or stops the server at once, and says where it listens. Status
//! (`DaemonStatus.web`) is what the CLI, the doctor and the web app's settings show.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::config::Config;
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{Reply, WebServe};

use crate::daemon::Daemon;

#[derive(Default)]
struct Run {
    child: Option<std::process::Child>,
    /// The `[remote]` settings it was started with: a change starts it again.
    started_with: Option<ptyro_core::config::Remote>,
    since_ms: Option<u64>,
    restarts: u32,
    last_exit: Option<String>,
}

#[derive(Default)]
pub struct ServeChild {
    run: Mutex<Run>,
    /// Wakes the supervisor now (the setting changed), instead of at its next look.
    wake: tokio::sync::Notify,
}

impl ServeChild {
    pub fn kill(&self) {
        let mut r = self.run.lock().expect("serve child");
        if let Some(mut c) = r.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        r.since_ms = None;
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }
}

/// A process is alive (and ours to look at).
fn alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
}

/// The last thing the server said, for "why did it stop".
fn last_words(log: &std::path::Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    match line.char_indices().nth(240) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

impl Daemon {
    pub(crate) fn start_serve_supervisor(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let remote = me.config().remote;
                let want = remote.serve;
                let state = {
                    let mut r = me.serve_child.run.lock().expect("serve child");
                    // `serve` itself is the switch; anything else it was started with is read once.
                    let changed = r.started_with.as_ref().is_some_and(|w| {
                        ptyro_core::config::Remote { serve: remote.serve, ..w.clone() } != remote
                    });
                    if changed && want && r.child.is_some() {
                        tracing::info!("[remote] changed; starting ptyro serve again");
                        if let Some(mut c) = r.child.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        r.since_ms = None;
                        r.started_with = None;
                    }
                    match r.child.as_mut().map(std::process::Child::try_wait) {
                        Some(Ok(Some(status))) => {
                            r.child = None;
                            r.since_ms = None;
                            let why = last_words(&me.paths.state.join("serve.log"));
                            r.last_exit = Some(if why.is_empty() {
                                status.to_string()
                            } else {
                                format!("{status}: {why}")
                            });
                            Some(Some(status))
                        }
                        Some(_) => Some(None),
                        None => None,
                    }
                };
                let mut pause = Duration::from_secs(3);
                match (want, state) {
                    (true, None) => {
                        if let Err(e) = me.spawn_serve() {
                            tracing::warn!("ptyro serve: {e}");
                            me.serve_child.run.lock().expect("serve child").last_exit =
                                Some(e.to_string());
                        }
                    }
                    (true, Some(Some(status))) => {
                        tracing::warn!(
                            "ptyro serve exited ({status}); restarting in {}s (state/serve.log)",
                            backoff.as_secs()
                        );
                        pause = backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        me.serve_child.run.lock().expect("serve child").restarts += 1;
                    }
                    (true, Some(None)) => backoff = Duration::from_secs(1),
                    (false, Some(_)) => {
                        me.serve_child.kill();
                        me.serve_child.run.lock().expect("serve child").last_exit = None;
                    }
                    (false, None) => {}
                }
                // Asked to look now (the setting changed): don't wait out the pause, or a backoff.
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    () = me.serve_child.wake.notified() => backoff = Duration::from_secs(1),
                }
            }
        });
    }

    fn spawn_serve(&self) -> std::io::Result<()> {
        let exe = crate::handoff::running_binary();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.state.join("serve.log"))?;
        // Its record of where it listens belongs to a run that has ended, unless a server
        // someone started by hand is still running (it keeps the port, and the record says so).
        let record = self.paths.state.join("serve.json");
        let live = std::fs::read_to_string(&record)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["pid"].as_u64())
            .and_then(|p| u32::try_from(p).ok())
            .is_some_and(alive);
        if !live {
            let _ = std::fs::remove_file(record);
        }
        let child = std::process::Command::new(exe)
            .args(["serve", "--managed"])
            .envs(self.base_env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        tracing::info!(pid = child.id(), "started ptyro serve");
        let mut r = self.serve_child.run.lock().expect("serve child");
        r.child = Some(child);
        r.started_with = Some(self.config().remote);
        r.since_ms = Some(now_ms());
        Ok(())
    }

    /// How the web app's server is.
    pub fn web_status(&self) -> WebServe {
        let config = self.config();
        let r = self.serve_child.run.lock().expect("serve child");
        let mine = r.child.as_ref().map(std::process::Child::id);
        // The record `ptyro serve` writes when it is listening: its pid, URL and CA.
        let record: serde_json::Value =
            std::fs::read_to_string(self.paths.state.join("serve.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
        let recorded = record["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
        let up = mine.is_some() && recorded == mine;
        let by_hand = recorded.filter(|p| Some(*p) != mine && alive(*p));
        WebServe {
            wanted: config.remote.serve,
            up,
            pid: mine,
            url: if up || by_hand.is_some() {
                record["url"].as_str().map(str::to_owned)
            } else {
                None
            },
            listen: config.remote.listen.clone(),
            ca_fingerprint: record["ca_fingerprint"]
                .as_str()
                .filter(|_| up || by_hand.is_some())
                .map(str::to_owned),
            since_ms: r.since_ms.filter(|_| up),
            restarts: r.restarts,
            last_exit: r.last_exit.clone(),
            by_hand,
            log: self.paths.state.join("serve.log"),
        }
    }

    /// Turn `[remote] serve` on or off in the config file, and bring the server up or down.
    /// `via` is how phones will reach it (`tailscale`, `lan`, `here`); left out, a way is chosen
    /// when none is set yet. Waits a few seconds for it to say where it listens.
    pub(crate) async fn web_set(
        self: &Arc<Self>,
        on: bool,
        via: Option<&str>,
    ) -> Result<Reply, Error> {
        let mut notes = ptyro_core::config::set_serve(&Config::path(&self.paths), on, via)?;
        if let Some(e) = self.reload_config() {
            return Err(Error::invalid(e));
        }
        if on {
            self.serve_child.wake();
            let deadline = std::time::Instant::now() + Duration::from_secs(12);
            while !self.web_status().up && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else {
            self.serve_child.wake();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while self.web_status().pid.is_some() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let web = self.web_status();
        match (on, web.up) {
            (true, true) => {}
            (true, false) => notes.push(match (&web.by_hand, &web.last_exit) {
                (Some(pid), _) => format!(
                    "a `ptyro serve` you started by hand (pid {pid}) is running; stop it (Ctrl-C) and the daemon's takes over"
                ),
                (None, Some(why)) => format!("it didn't start: {why}"),
                (None, None) => "it is starting; give it a moment".into(),
            }),
            (false, _) => {
                if let Some(pid) = web.by_hand {
                    notes.push(format!(
                        "a `ptyro serve` you started by hand (pid {pid}) is still running; Ctrl-C it to stop it"
                    ));
                }
            }
        }
        Ok(Reply::Web { web, notes })
    }
}
