//! Setup over the protocol (BOOT1, SPEC §24.2): the checklist and installs on the daemon's
//! machine, for clients that can't run the engine themselves (the web). The same engine as `ptyro
//! setup` (`ptyro_agents::setup`); administrator steps never run here (`Admin::Never`): a
//! password or a UAC prompt never goes through the web, so each is reported with its command.
//! On a Windows daemon with a usable WSL, WSL's distro is a second side (BOOT8): its own ptyro
//! plans and installs there, through `wsl.exe`, with the same rule.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ptyro_agents::catalog::Platform;
use ptyro_agents::setup::{self, Admin, Plan, Side, Survey, WslStatus, checklist};
use ptyro_agents::updates;
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
    updates_cfg: &ptyro_core::config::Updates,
    picks: Option<Vec<String>>,
    wsl_picks: Option<Vec<String>>,
    moves: Option<&[String]>,
) -> Result<(Box<dyn setup::Host + Send>, Plan, Vec<checklist::Line>), Error> {
    let paths = paths.clone();
    // The remembered newest versions (BOOT5): an outdated agent is offered its update, and
    // ticking it runs that. An agent installed another way is offered the move unless
    // `updates.keep_installer` keeps it (BOOT7); a run (`moves`) moves only those it names.
    let cfg = updates_cfg.clone();
    let (host, here) = tokio::task::spawn_blocking(move || {
        let host = setup::host(&paths);
        let survey = updates::survey(&*host, &paths.state, &cfg);
        (host, survey)
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let (status, mut wsl) = wsl_side(here.platform).await;
    let mut here = here;
    if let Some(moves) = moves {
        let keep: Vec<String> = ptyro_agents::catalog::items()
            .iter()
            .filter(|i| i.kind == ptyro_agents::catalog::Kind::Agent && !moves.contains(&i.id))
            .map(|i| i.id.clone())
            .collect();
        here.keep.extend(keep.iter().cloned());
        if let Some(w) = &mut wsl {
            w.keep.extend(keep);
        }
    }
    let picks = picks.unwrap_or_else(|| setup::defaults(&here));
    let mine = setup::plan(&here, &picks);
    let theirs = wsl.as_ref().map(|s| setup::plan(s, &wsl_picks.unwrap_or_default()));
    let plan = Plan::merge(mine, theirs, status);
    let lines = checklist::lines(&plan, &here, wsl.as_ref());
    Ok((host, plan, lines))
}

/// The daily look for newer versions (BOOT5, SPEC §24.7): the first a minute after the daemon
/// starts (never on its start-up path), then every hour it asks whether a day has passed since
/// the last look (`<state>/versions.json`), and looks if so; otherwise it only asks the installed
/// versions again, so an agent that updated itself stops being offered. Nothing while an install
/// runs (the next hour does it), nothing with `updates.check = "off"`, and offline it stays
/// quiet: the failure is remembered, and tried again the next day.
const UPDATES_FIRST: Duration = Duration::from_secs(60);
const UPDATES_EVERY: Duration = Duration::from_secs(3600);

fn knob_ms(name: &str, or: Duration) -> Duration {
    ptyro_core::test_knob(name).and_then(|v| v.parse().ok()).map_or(or, Duration::from_millis)
}

impl Daemon {
    pub(crate) fn start_updates(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(knob_ms("PTYRO_TEST_UPDATES_FIRST_MS", UPDATES_FIRST)).await;
            loop {
                if me.config().updates.check == ptyro_core::config::UpdateCheck::Daily {
                    let (paths, state) = (me.paths.clone(), me.paths.state.clone());
                    let _ = tokio::task::spawn_blocking(move || {
                        let host = setup::host(&paths);
                        let now = updates::now();
                        let due = updates::due(&updates::load(&state), now);
                        let (v, _) = updates::look_beside(&*host, &state, now, due);
                        if due && v.checked == now {
                            tracing::info!(
                                "looked for newer agent versions: {} out of date, {} not reached",
                                v.outdated.len(),
                                v.failed.len()
                            );
                        }
                    })
                    .await;
                }
                tokio::time::sleep(knob_ms("PTYRO_TEST_UPDATES_EVERY_MS", UPDATES_EVERY)).await;
            }
        });
    }

    /// `Updates` (BOOT5): what is out of date here, for a TUI's badge and start-up note, which
    /// show this daemon's machine (the sessions' agents), wherever the TUI runs. A refresh beside
    /// the installs (`updates::look_beside`, no network); `note` also marks the note's said.
    pub(crate) async fn updates(&self, note: bool) -> Result<Reply, Error> {
        let cfg = self.config().updates;
        let (paths, state) = (self.paths.clone(), self.paths.state.clone());
        tokio::task::spawn_blocking(move || {
            if cfg.check == ptyro_core::config::UpdateCheck::Off {
                return Reply::Updates { offered: Vec::new(), note: Vec::new() };
            }
            let host = setup::host(&paths);
            let (v, _) = updates::look_beside(&*host, &state, updates::now(), false);
            let offered = updates::offered(&v, &cfg);
            let note = if note { updates::note(&state, &cfg) } else { Vec::new() };
            Reply::Updates { offered, note }
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))
    }

    /// `SetupPlan`.
    pub(crate) async fn setup_plan(
        &self,
        picks: Option<Vec<String>>,
        wsl_picks: Option<Vec<String>>,
    ) -> Result<Reply, Error> {
        let picks = picks.map(|p| ids(&p)).transpose()?;
        let wsl_picks = wsl_picks.map(|p| ids(&p)).transpose()?;
        let (_, plan, lines) =
            plan_on(&self.paths, &self.config().updates, picks, wsl_picks, None).await?;
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
        moves: Vec<String>,
    ) -> Result<Reply, Error> {
        let picks = ids(&picks)?;
        let moves = ids(&moves)?;
        let wsl_picks = wsl_picks.map(|p| ids(&p)).transpose()?;
        // Another run holding the machine's lock is refused at once; the take itself (which waits
        // out a probe's instant) runs off the runtime's workers.
        if !RUNNING.load(Ordering::Acquire)
            && let Some(by) = setup::lock::Lock::held_by(&self.paths.state)
        {
            return Err(Error::conflict(setup::lock::busy(&by)));
        }
        let state = self.paths.state.clone();
        let running = tokio::task::spawn_blocking(move || Running::take(&state))
            .await
            .map_err(|e| Error::internal(e.to_string()))?
            .map_err(Error::conflict)?;
        let (host, plan, _) =
            plan_on(&self.paths, &self.config().updates, Some(picks), wsl_picks, Some(&moves))
                .await?;
        let run = ptyro_run::short_id("setup", 6);
        if let Ok(mut c) = CURRENT.lock() {
            *c = Some(SetupRunning { run: run.clone(), plan: plan.clone(), events: Vec::new() });
        }
        let events = self.events.clone();
        let (r, p) = (run.clone(), plan.clone());
        let state = self.paths.state.clone();
        let me = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _running = running;
            let mut changed = false;
            for side in p.sides() {
                let mut on = |event: setup::Event| {
                    // What it installed is on the daemon's PATH before anyone hears it ended,
                    // for the sessions and worktrees started next (SPEC §24.2).
                    if matches!(event, setup::Event::Finished { .. }) {
                        me.refresh_path();
                    }
                    record(&events, &r, side, event)
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
                        // A move that failed half way changed what is installed too (BOOT7).
                        let r = setup::run(&part, &*host, Admin::Never, &mut on);
                        changed |= !r.done.is_empty() || !r.failed.is_empty();
                    }
                }
            }
            // What is out of date changed (an update, BOOT5): the Health page and the badge
            // follow. Before the run lets go of the lock, so nothing starts meanwhile.
            if changed {
                updates::refresh(&*host, &state, updates::now());
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
