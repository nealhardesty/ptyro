//! The setup checklist in the TUI (ROADMAP BOOT2, SPEC §24.5): the first part of the onboarding
//! screen (`onboard.rs`), and on its own from `app.setup` (`U`), the palette and the health
//! badge's fixes. Agents, Tools and Already here, each with a line saying what it is and how it
//! installs; prerequisites tick themselves, with the reason shown. It is the CLI's checklist on
//! the same engine (`ptyro_agents::setup`, with `setup::checklist`'s ticking rules): this computer
//! is surveyed here. ⏎ runs `ptyro install … --confirmed` in a split, a real terminal: live
//! output, sudo's prompt, then each tool's own login and the profiles line, exactly as the CLI
//! does them.

use ptyro_agents::catalog::{self, Kind};
use ptyro_agents::setup::checklist;
use ptyro_agents::setup::{self, Entry, Plan, Survey};
use ptyro_core::paths::Paths;

/// What setup found.
#[derive(Debug, Clone)]
pub enum Found {
    /// Looking at what's installed (a few `--version`s).
    Looking,
    Ready(Box<Ready>),
    /// The daemon runs on another computer (`ptyro --remote`): its agents are set up there.
    Elsewhere(String),
    /// Looking failed (it never should: the survey only reads).
    Failed,
}

#[derive(Debug, Clone)]
pub struct Ready {
    pub survey: Survey,
    pub picks: Vec<String>,
    pub plan: Plan,
}

impl Ready {
    fn replan(&mut self) {
        self.plan = setup::plan(&self.survey, &self.picks);
    }
}

/// The checklist's state on screen.
#[derive(Debug, Clone)]
pub struct Setup {
    pub found: Found,
    /// What the last tick said ("Node.js is needed by something ticked: untick that first").
    pub said: Option<String>,
    /// What to tick instead of the defaults (a health fix for one agent), by item id.
    pub want: Option<Vec<String>>,
    /// Setup is running already (the machine's lock, `setup::lock`): who holds it. ⏎ installs
    /// nothing meanwhile, and the checklist says so.
    pub busy: Option<String>,
}

/// The checklist's install, running (BOOT2): one split, so two package managers, sudo prompts or
/// sign-ins never overlap. While it runs, the checklist isn't opened again (its split is shown
/// instead); once it ends, the health badge looks again.
#[derive(Debug, Clone)]
pub struct SetupRun {
    /// The split running now (its session name).
    pub split: String,
    /// Started: its spawn answered, so it is in the session list (until then it is starting).
    pub seen: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self::looking(None)
    }
}

impl Setup {
    pub fn looking(want: Option<Vec<String>>) -> Self {
        Self { found: Found::Looking, said: None, want, busy: None }
    }

    pub fn ready(&self) -> Option<&Ready> {
        match &self.found {
            Found::Ready(r) => Some(r),
            _ => None,
        }
    }

    /// The rows that can be ticked, in order.
    pub fn rows(&self) -> Vec<&Entry> {
        self.ready().map(|r| checklist::rows(&r.plan)).unwrap_or_default()
    }

    /// How many steps ⏎ runs (none while setup runs elsewhere).
    pub fn steps(&self) -> usize {
        if self.busy.is_some() {
            return 0;
        }
        self.ready().map_or(0, |r| r.plan.steps().len())
    }

    /// Look at the machine's setup lock in `state` again; true when someone holds it.
    pub fn check_busy(&mut self, state: &std::path::Path) -> bool {
        self.busy = ptyro_agents::setup::lock::Lock::held_by(state);
        self.busy.is_some()
    }

    /// The survey is in: tick the defaults (or what was asked for).
    pub fn found(&mut self, survey: Survey) {
        let picks = match &self.want {
            Some(ids) => ids.clone(),
            None => setup::defaults(&survey),
        };
        let plan = setup::plan(&survey, &picks);
        self.found = Found::Ready(Box::new(Ready { survey, picks, plan }));
    }

    /// Space on row `i`: ticked or unticked.
    pub fn toggle(&mut self, i: usize) {
        let Found::Ready(r) = &mut self.found else { return };
        let Some(id) = checklist::rows(&r.plan).get(i).map(|e| e.id.clone()) else { return };
        let said = checklist::toggle(&r.plan, &mut r.picks, &id);
        r.replan();
        self.said = (!said.is_empty()).then(|| said.join("; "));
    }

    /// `K` on row `i` (BOOT7): an agent offered the move to the recommended installer is kept the
    /// way it's installed: unticked, and no longer offered. `Ok(Some((id, name, installer)))` to
    /// remember in `updates.keep_installer`; `Err`: why it can't be kept.
    pub fn keep(&mut self, i: usize) -> Result<Option<(String, String, String)>, String> {
        let Found::Ready(r) = &mut self.found else { return Ok(None) };
        let Some(id) = checklist::rows(&r.plan).get(i).map(|e| e.id.clone()) else {
            return Ok(None);
        };
        let Some(e) = r.plan.entry(&id) else { return Ok(None) };
        let Some(m) = e.moves.clone() else {
            return Err(format!(
                "K keeps an agent installed another way (npm, Homebrew) as it is; {} isn't",
                e.name
            ));
        };
        let name = e.name.clone();
        r.survey.keep.insert(id.clone());
        r.picks.retain(|p| *p != id);
        r.replan();
        self.said = None;
        Ok(Some((id, name, m.from.words().to_owned())))
    }

    /// Some row is offered the move (BOOT7): the footer names `K`.
    pub fn offers_moves(&self) -> bool {
        self.ready().is_some_and(|r| r.plan.entries.iter().any(|e| e.moves.is_some()))
    }

    /// What ⏎ runs in a split: `ptyro install … --confirmed` for what is picked. `None`: nothing
    /// is.
    pub fn command(&self) -> Option<Vec<String>> {
        let r = self.ready()?;
        let mut ids: Vec<String> =
            r.plan.entries.iter().filter(|e| e.picked).map(|e| program(&e.id)).collect();
        ids.dedup();
        (!ids.is_empty()).then(|| {
            std::iter::once("install".to_owned())
                .chain(ids)
                .chain(std::iter::once("--confirmed".to_owned()))
                .collect()
        })
    }
}

/// The name `ptyro install` takes for an item (its command: `opencode`, `gh`).
fn program(id: &str) -> String {
    catalog::item(id).map_or_else(|| id.to_owned(), |i| i.program.clone())
}

/// The daemon is on another computer (not this one): its setup is there. `None`: this
/// computer's.
pub fn elsewhere(remote: Option<&str>) -> Option<String> {
    let r = remote?;
    if ptyro_client::is_local(Some(r)) {
        return None;
    }
    Some(r.to_owned())
}

/// Look at this computer, as `ptyro setup` does.
pub async fn look(paths: Paths) -> Option<Survey> {
    tokio::task::spawn_blocking(move || {
        let host = setup::host(&paths);
        // The remembered newest versions (BOOT5): an outdated agent can be ticked to update;
        // `updates.keep_installer` (BOOT7): one installed another way is offered the move
        // unless kept.
        let cfg = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&paths))
            .map(|c| c.updates)
            .unwrap_or_default();
        ptyro_agents::updates::survey(&*host, &paths.state, &cfg)
    })
    .await
    .ok()
}

pub use ptyro_agents::setup::checklist::about;

/// What the survey found on `PATH` (for a method's `needs`).
pub fn programs_of(r: &Ready) -> impl Fn(&str) -> bool + '_ {
    move |p: &str| r.survey.programs.contains(p)
}

/// The two kinds, in the checklist's order and words.
pub const SECTIONS: [(&str, Kind); 2] = [("Agents", Kind::Agent), ("Tools", Kind::Tool)];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ptyro_agents::catalog::Platform;
    use ptyro_agents::setup::State;
    use std::collections::BTreeMap;

    pub(crate) fn survey(
        platform: Platform,
        installed: &[(&str, &str)],
        programs: &[&str],
    ) -> Survey {
        let mut states = BTreeMap::new();
        for i in catalog::items() {
            let st = match installed.iter().find(|(id, _)| *id == i.id) {
                Some((_, v)) => State::Installed { version: Some((*v).into()), path: None },
                None => State::Missing,
            };
            states.insert(i.id.clone(), st);
        }
        Survey {
            platform,
            states,
            programs: programs.iter().map(|p| (*p).to_owned()).collect(),
            candidates: BTreeMap::new(),
            keep: Default::default(),
        }
    }

    /// A Linux with apt, sudo and curl, and `installed`, looked at.
    pub(crate) fn one(installed: &[(&str, &str)]) -> Setup {
        let mut s = Setup::looking(None);
        s.found(survey(Platform::Linux, installed, &["apt-get", "sudo", "curl"]));
        s
    }

    /// BOOT7: Codex through npm is a row offering the move; Space ticks it (⏎ runs `ptyro
    /// install codex`, which moves it); K keeps it the way it is, and it leaves the rows.
    #[test]
    fn an_agent_installed_another_way_can_be_moved_or_kept() {
        let mut here = survey(Platform::Linux, &[("git", "2.43.0"), ("gh", "2.57.0")], &["npm"]);
        here.states.insert(
            "codex".into(),
            State::Other {
                version: Some("0.162.1".into()),
                via: catalog::Via::Npm,
                path: None,
                latest: None,
                system: false,
                prefix: Some("/home/me/.npm-global".into()),
                store: None,
            },
        );
        let mut s = Setup::looking(Some(vec![]));
        s.found(here);
        assert!(s.offers_moves());
        let i = s.rows().iter().position(|e| e.id == "codex").expect("a row");
        s.toggle(i);
        assert_eq!(s.command().unwrap(), ["install", "codex", "--confirmed"]);
        let kept = s.keep(i).unwrap().unwrap();
        assert_eq!(kept, ("codex".into(), "Codex".into(), "npm".into()));
        assert!(!s.rows().iter().any(|e| e.id == "codex"), "kept: here already");
        assert!(s.command().is_none(), "and unticked");
        // K on a row that isn't installed another way says what K is for.
        let i = s.rows().iter().position(|e| e.id == "claude").unwrap();
        assert!(s.keep(i).unwrap_err().contains("K keeps an agent installed another way"));
    }

    #[test]
    fn the_defaults_tick_and_enter_installs_them_with_the_logins() {
        let s = one(&[("git", "2.43.0")]);
        let ticked: Vec<&str> =
            s.rows().iter().filter(|e| e.picked).map(|e| e.id.as_str()).collect();
        assert_eq!(ticked, ["claude", "codex", "gh"], "git is here already");
        assert_eq!(s.command().unwrap(), ["install", "claude", "codex", "gh", "--confirmed"]);
    }

    #[test]
    fn a_daemon_on_another_computer_is_set_up_there() {
        assert_eq!(elsewhere(None), None);
        assert_eq!(elsewhere(Some("local")), None);
        assert_eq!(elsewhere(Some("me@box")).as_deref(), Some("me@box"));
    }
}
