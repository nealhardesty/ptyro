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
use ptyro_agents::setup::{self, Admin, Plan, Side, Survey, WslStatus, checklist};
use ptyro_client::wsl_setup;
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{DomainEvent, Reply, SetupLogged, SetupRunning};

use crate::daemon::Daemon;

/// One setup run at a time: two package managers at once fight over their locks.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The run going now and its events so far (BOOT3), for `SetupPlan`'s `running`: a page that
/// didn't start it (another device, or reloaded) shows it instead of offering a second.
static CURRENT: Mutex<Option<SetupRunning>> = Mutex::new(None);

/// How many of a run's events `CURRENT` keeps (the latest; a page shows 400 lines).
const KEPT: usize = 500;

/// Holds the one run's place; let go however its holder ends, a panic included, so a later run
/// is never refused for one that is gone. Besides this daemon's own flag it holds the machine's
/// setup lock (`setup::lock`), which `ptyro install` and `ptyro setup` take too: one install at a
/// time whoever starts it.
struct Running {
    _lock: setup::lock::Lock,
}

impl Running {
    fn take(state: &std::path::Path) -> Result<Self, String> {
        if RUNNING.swap(true, Ordering::AcqRel) {
            return Err(
                "setup is already running (from the web app): wait for it to finish".to_owned()
            );
        }
        match setup::lock::Lock::take(state, "the web app, through the daemon") {
            Ok(lock) => Ok(Self { _lock: lock }),
            Err(why) => {
                RUNNING.store(false, Ordering::Release);
                Err(why)
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut c) = CURRENT.lock() {
            *c = None;
        }
        RUNNING.store(false, Ordering::Release);
    }
}

/// The run going now, as `SetupPlan` shows it.
fn current() -> Option<SetupRunning> {
    CURRENT.lock().ok().and_then(|c| c.clone())
}

/// Keep one of the run's events, numbered, and tell every watcher; under `CURRENT`'s lock, so a
/// `SetupPlan` either holds an event or comes before it is sent.
fn record(
    events: &tokio::sync::broadcast::Sender<DomainEvent>,
    run: &str,
    side: Side,
    event: setup::Event,
) {
    let Ok(mut c) = CURRENT.lock() else { return };
    let seq = match c.as_mut().filter(|c| c.run == run) {
        Some(c) => {
            let seq = c.events.last().map_or(1, |l| l.seq + 1);
            c.events.push(SetupLogged { seq, side, event: event.clone() });
            if c.events.len() > KEPT {
                c.events.remove(0);
            }
            seq
        }
        None => 0,
    };
    let _ = events.send(DomainEvent::SetupProgress { run: run.to_owned(), event, side, seq });
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
) -> Result<(Box<dyn setup::Host + Send>, Plan, Vec<checklist::Line>), Error> {
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
    let plan = Plan::merge(mine, theirs, status);
    let lines = checklist::lines(&plan, &here, wsl.as_ref());
    Ok((host, plan, lines))
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
        let (_, plan, lines) = plan_on(&self.paths, picks, wsl_picks).await?;
        let default_agent = ptyro_agents::default_agent(&self.config());
        let running = current();
        // Someone else's install (a terminal's, the TUI's): the page says so instead of offering
        // one that would be refused. (While this daemon's own run is starting, before `CURRENT`
        // names it, this says it is the web app's.)
        let held_by =
            if running.is_none() { setup::lock::Lock::held_by(&self.paths.state) } else { None };
        Ok(Reply::SetupPlan { plan, lines, default_agent, running, held_by })
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
        let running = Running::take(&self.paths.state).map_err(Error::conflict)?;
        let (host, plan, _) = plan_on(&self.paths, Some(picks), wsl_picks).await?;
        let run = ptyro_run::short_id("setup", 6);
        if let Ok(mut c) = CURRENT.lock() {
            *c = Some(SetupRunning { run: run.clone(), plan: plan.clone(), events: Vec::new() });
        }
        let events = self.events.clone();
        let (r, p) = (run.clone(), plan.clone());
        tokio::task::spawn_blocking(move || {
            let _running = running;
            for side in p.sides() {
                let mut on = |event| record(&events, &r, side, event);
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
        let dir = tempfile::tempdir().unwrap();
        let held = Running::take(dir.path()).expect("free at first");
        assert!(Running::take(dir.path()).is_err(), "one at a time");
        let r = std::thread::spawn(move || {
            let _held = held;
            panic!("a run that panicked");
        })
        .join();
        assert!(r.is_err());
        assert!(Running::take(dir.path()).is_ok(), "free again after the panic");
    }
}
