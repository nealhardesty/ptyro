//! Setup over the protocol (BOOT1, SPEC §24.2): the checklist and installs on the daemon's
//! machine, for clients that can't run the engine themselves (the web). The same engine as `ptyro
//! setup` (`ptyro_agents::setup`); administrator steps never run here (`Admin::Never`): a
//! password or a UAC prompt never goes through the web, so each is reported with its command.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ptyro_agents::setup::{self, Admin, Plan};
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{DomainEvent, Reply};

use crate::daemon::Daemon;

/// One setup run at a time: two package managers at once fight over their locks.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Holds the one run's place; let go however its holder ends, a panic included, so a later run
/// is never refused for one that is gone.
struct Running;

impl Running {
    fn take() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

/// Item ids for what a client named (`agy`, `github-cli`).
fn ids(names: &[String]) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for n in names {
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if !out.contains(&item.id) {
            out.push(item.id.clone());
        }
    }
    Ok(out)
}

fn plan_on(paths: &Paths, picks: Option<Vec<String>>) -> (Box<dyn setup::Host + Send>, Plan) {
    let host = setup::host(paths);
    let survey = setup::survey(&*host, &BTreeMap::new());
    let picks = picks.unwrap_or_else(|| setup::defaults(&survey));
    let plan = setup::plan(&survey, &picks);
    (host, plan)
}

impl Daemon {
    /// `SetupPlan`.
    pub(crate) async fn setup_plan(&self, picks: Option<Vec<String>>) -> Result<Reply, Error> {
        let picks = picks.map(|p| ids(&p)).transpose()?;
        let paths = self.paths.clone();
        let (_, plan) = tokio::task::spawn_blocking(move || plan_on(&paths, picks))
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok(Reply::SetupPlan { plan })
    }

    /// `SetupRun`: the plan now, its progress as `SetupProgress` events.
    pub(crate) async fn setup_run(self: &Arc<Self>, picks: Vec<String>) -> Result<Reply, Error> {
        let picks = ids(&picks)?;
        let Some(running) = Running::take() else {
            return Err(Error::conflict("a setup run is going already: wait for it to finish"));
        };
        let paths = self.paths.clone();
        let (host, plan) = tokio::task::spawn_blocking(move || plan_on(&paths, Some(picks)))
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        let run = ptyro_run::short_id("setup", 6);
        let events = self.events.clone();
        let (r, p) = (run.clone(), plan.clone());
        tokio::task::spawn_blocking(move || {
            let _running = running;
            setup::run(&p, &*host, Admin::Never, &mut |event| {
                let _ = events.send(DomainEvent::SetupProgress { run: r.clone(), event });
            });
        });
        Ok(Reply::SetupStarted { run, plan })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_guard_lets_go_even_after_a_panic() {
        let held = Running::take().expect("free at first");
        assert!(Running::take().is_none(), "one at a time");
        let r = std::thread::spawn(move || {
            let _held = held;
            panic!("a run that panicked");
        })
        .join();
        assert!(r.is_err());
        assert!(Running::take().is_some(), "free again after the panic");
    }
}
