//! Setup over the protocol (BOOT1, SPEC §24.2): the checklist and installs on the daemon's
//! machine, for clients that can't run the engine themselves (the web). The same engine as `ptyro
//! setup` (`ptyro_agents::setup`); administrator steps never run here (`Admin::Never`): a
//! password or a UAC prompt never goes through the web, so each is reported with its command.
//! On a Windows daemon with a usable WSL, WSL's distro is a second side (BOOT8): its own ptyro
//! plans and installs there, through `wsl.exe`, with the same rule.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ptyro_agents::catalog::Platform;
use ptyro_agents::setup::{self, Admin, Plan, Side, Survey, WslStatus};
use ptyro_client::wsl_setup;
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

/// WSL as last found, kept a minute: the web re-plans on every tick, and asking WSL's ptyro
/// takes a second or two. A run forgets it (what's installed changed).
static WSL_SEEN: Mutex<Option<(Instant, WslStatus, Option<Survey>)>> = Mutex::new(None);

/// On a Windows daemon, WSL and what its distro has (BOOT8); inside WSL, that this is it.
async fn wsl_side(platform: Platform) -> (Option<WslStatus>, Option<Survey>) {
    if platform == Platform::Wsl {
        let distro = std::env::var("WSL_DISTRO_NAME").ok();
        return (Some(WslStatus::Inside { distro }), None);
    }
    if !platform.is_windows() {
        return (None, None);
    }
    if let Ok(seen) = WSL_SEEN.lock()
        && let Some((at, status, survey)) = seen.as_ref()
        && at.elapsed() < Duration::from_secs(60)
    {
        return (Some(status.clone()), survey.clone());
    }
    let mut status = wsl_setup::probe(None).await;
    let mut survey = None;
    if let Some(d) = status.usable().map(str::to_owned) {
        match wsl_setup::survey(&d).await {
            Ok(s) => survey = Some(s),
            Err(why) => status = why,
        }
    }
    if let Ok(mut seen) = WSL_SEEN.lock() {
        *seen = Some((Instant::now(), status.clone(), survey.clone()));
    }
    (Some(status), survey)
}

fn forget_wsl() {
    if let Ok(mut seen) = WSL_SEEN.lock() {
        *seen = None;
    }
}

/// The plan for `picks` here (the defaults when `None`), and on Windows `wsl_picks` in WSL's
/// distro beside it.
async fn plan_on(
    paths: &Paths,
    picks: Option<Vec<String>>,
    wsl_picks: Option<Vec<String>>,
) -> Result<(Box<dyn setup::Host + Send>, Plan), Error> {
    let paths = paths.clone();
    let (host, here) = tokio::task::spawn_blocking(move || {
        let host = setup::host(&paths);
        let survey = setup::survey(&*host, &BTreeMap::new());
        (host, survey)
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let (status, wsl) = wsl_side(here.platform).await;
    let picks = picks.unwrap_or_else(|| setup::defaults(&here));
    let mine = setup::plan(&here, &picks);
    let theirs = wsl.as_ref().map(|s| setup::plan(s, &wsl_picks.unwrap_or_default()));
    Ok((host, Plan::merge(mine, theirs, status)))
}

impl Daemon {
    /// `SetupPlan`.
    pub(crate) async fn setup_plan(
        &self,
        picks: Option<Vec<String>>,
        wsl_picks: Option<Vec<String>>,
    ) -> Result<Reply, Error> {
        let picks = picks.map(|p| ids(&p)).transpose()?;
        let wsl_picks = wsl_picks.map(|p| ids(&p)).transpose()?;
        let (_, plan) = plan_on(&self.paths, picks, wsl_picks).await?;
        Ok(Reply::SetupPlan { plan })
    }

    /// `SetupRun`: the plan now, its progress as `SetupProgress` events (this computer's side
    /// first, then WSL's, each ending with its own `finished`).
    pub(crate) async fn setup_run(
        self: &Arc<Self>,
        picks: Vec<String>,
        wsl_picks: Option<Vec<String>>,
    ) -> Result<Reply, Error> {
        let picks = ids(&picks)?;
        let wsl_picks = wsl_picks.map(|p| ids(&p)).transpose()?;
        let Some(running) = Running::take() else {
            return Err(Error::conflict("a setup run is going already: wait for it to finish"));
        };
        let (host, plan) = plan_on(&self.paths, Some(picks), wsl_picks).await?;
        let run = ptyro_run::short_id("setup", 6);
        let events = self.events.clone();
        let (r, p) = (run.clone(), plan.clone());
        tokio::task::spawn_blocking(move || {
            let _running = running;
            for side in p.sides() {
                let mut on = |event| {
                    let _ = events.send(DomainEvent::SetupProgress { run: r.clone(), event, side });
                };
                let part = p.side(side);
                if !part.entries.iter().any(|e| e.will_run() || (e.ticked && e.blocked.is_some())) {
                    continue;
                }
                match (side, p.wsl.as_ref().and_then(WslStatus::usable)) {
                    (Side::Wsl, Some(distro)) => {
                        wsl_setup::run(distro, &p, wsl_setup::Admin::Never, &mut on);
                    }
                    _ => {
                        setup::run(&part, &*host, Admin::Never, &mut on);
                    }
                }
            }
            forget_wsl();
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
