//! `[remote] serve = true` (M9.2): the daemon runs `ptyro serve --managed` itself, restarts it
//! when it dies (backing off), stops it when the setting goes off, and kills it before a live
//! upgrade (the next image starts its own). Its output goes to `state/serve.log`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::daemon::Daemon;

#[derive(Default)]
pub struct ServeChild {
    child: Mutex<Option<std::process::Child>>,
}

impl ServeChild {
    pub fn kill(&self) {
        if let Some(mut c) = self.child.lock().expect("serve child").take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Daemon {
    pub(crate) fn start_serve_supervisor(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let want = me.config().remote.serve;
                let state = {
                    let mut c = me.serve_child.child.lock().expect("serve child");
                    match c.as_mut().map(std::process::Child::try_wait) {
                        Some(Ok(Some(status))) => {
                            *c = None;
                            Some(Some(status))
                        }
                        Some(_) => Some(None),
                        None => None,
                    }
                };
                match (want, state) {
                    (true, None) => {
                        if let Err(e) = me.spawn_serve() {
                            tracing::warn!("ptyro serve: {e}");
                        }
                    }
                    (true, Some(Some(status))) => {
                        tracing::warn!(
                            "ptyro serve exited ({status}); restarting in {}s (state/serve.log)",
                            backoff.as_secs()
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        continue;
                    }
                    (true, Some(None)) => backoff = Duration::from_secs(1),
                    (false, Some(_)) => me.serve_child.kill(),
                    (false, None) => {}
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    fn spawn_serve(&self) -> std::io::Result<()> {
        let exe = crate::handoff::running_binary();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.state.join("serve.log"))?;
        let child = std::process::Command::new(exe)
            .args(["serve", "--managed"])
            .envs(self.base_env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        tracing::info!(pid = child.id(), "started ptyro serve");
        *self.serve_child.child.lock().expect("serve child") = Some(child);
        Ok(())
    }
}
