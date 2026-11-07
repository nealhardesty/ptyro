//! The setup checklist in the TUI (ROADMAP BOOT2, SPEC §24.5): the first part of the onboarding
//! screen (`onboard.rs`), and on its own from `app.setup` (`U`), the palette and the health
//! badge's fixes. Agents, Tools and Already here, each with a line saying what it is and how it
//! installs; prerequisites tick themselves, with the reason shown. It is the CLI's checklist on
//! the same engine (`ptyro_agents::setup`, with `setup::checklist`'s ticking rules): this computer
//! is surveyed here, and on Windows WSL's distro through its own ptyro (BOOT8). ⏎ runs `ptyro
//! install … --confirmed` in a split, a real terminal: live output, sudo's prompt (or Windows'
//! UAC), then each tool's own login and the profiles line, exactly as the CLI does them.

use std::collections::BTreeMap;

use ptyro_agents::catalog::{self, Kind, Platform};
use ptyro_agents::setup::checklist::{self, Picks, Sides, cell};
use ptyro_agents::setup::{self, Entry, Plan, Side, Where, WslStatus};
use ptyro_client::wsl_setup;
use ptyro_core::paths::Paths;

/// One split to run: the ptyro to run (`None`: the daemon's), and its words.
pub type Split = (Option<String>, Vec<String>);

/// What setup found.
#[derive(Debug, Clone)]
pub enum Found {
    /// Looking at what's installed (a few `--version`s, and WSL's ptyro on Windows).
    Looking,
    Ready(Box<Ready>),
    /// The daemon runs on another computer (`ptyro --remote`): its agents are set up there.
    Elsewhere(String),
    /// Looking failed (it never should: the survey only reads).
    Failed,
}

#[derive(Debug, Clone)]
pub struct Ready {
    pub sides: Sides,
    /// Where a tick goes by default: the daemon's side (BOOT8).
    pub place: Where,
    pub picks: Picks,
    pub plan: Plan,
    /// Why the default side isn't the daemon's distro, when it isn't.
    pub note: Option<String>,
}

impl Ready {
    fn replan(&mut self) {
        self.plan = self.sides.plan(&self.picks);
    }
}

/// The checklist's state on screen.
#[derive(Debug, Clone)]
pub struct Setup {
    pub found: Found,
    /// The side column the cursor is on, with two sides (Windows, WSL).
    pub col: usize,
    /// What the last tick said ("Node.js is needed by something ticked: untick that first").
    pub said: Option<String>,
    /// What to tick instead of the defaults (a health fix for one agent), by item id.
    pub want: Option<Vec<String>>,
    /// The side to tick on instead of the default (a health fix's `--where`).
    pub want_place: Option<Where>,
}

/// The checklist's install, running (BOOT2): one split at a time, so two package managers, sudo
/// prompts or sign-ins never overlap. While it runs, the checklist isn't opened again (its
/// split is shown instead); once the last one ends, the health badge looks again.
#[derive(Debug, Clone)]
pub struct SetupRun {
    /// The split running now (its session name).
    pub split: String,
    /// Started: its spawn answered, so it is in the session list (until then it is starting).
    pub seen: bool,
    /// What runs after it, in order (Windows' side, then WSL's).
    pub queue: Vec<Split>,
}

impl Default for Setup {
    fn default() -> Self {
        Self::looking(None)
    }
}

impl Setup {
    pub fn looking(want: Option<Vec<String>>) -> Self {
        Self { found: Found::Looking, col: 0, said: None, want, want_place: None }
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

    /// How many steps ⏎ runs.
    pub fn steps(&self) -> usize {
        self.ready().map_or(0, |r| r.plan.steps().len())
    }

    /// The survey is in: tick the defaults (or what was asked for) on the default side.
    pub fn found(&mut self, sides: Sides, place: Where, note: Option<String>) {
        let place = match self.want_place {
            Some(p) if !sides.sides(p).is_empty() => p,
            _ => place,
        };
        let picks = match &self.want {
            Some(ids) => sides.picks(place, ids),
            None => sides.defaults(place),
        };
        let plan = sides.plan(&picks);
        let first = sides.sides(place).first().copied();
        self.col = plan.sides().iter().position(|s| Some(*s) == first).unwrap_or(0);
        self.found = Found::Ready(Box::new(Ready { sides, place, picks, plan, note }));
    }

    /// Space on row `i`: ticked or unticked on the side the cursor is on.
    pub fn toggle(&mut self, i: usize) {
        let col = self.col;
        let Found::Ready(r) = &mut self.found else { return };
        let Some(id) = checklist::rows(&r.plan).get(i).map(|e| e.id.clone()) else { return };
        let side = r.plan.sides().get(col).copied().unwrap_or(r.sides.side);
        let place = if side == Side::Wsl { Where::Wsl } else { Where::Local };
        let said = checklist::toggle(&r.sides, &r.plan, &mut r.picks, &id, place);
        r.replan();
        self.said = (!said.is_empty()).then(|| said.join("; "));
    }

    /// ←/→: the other side's column (two sides only).
    pub fn side_step(&mut self, by: isize) {
        let n = self.ready().map_or(1, |r| r.plan.sides().len());
        if n > 1 {
            self.col = (self.col as isize + by).rem_euclid(n as isize) as usize;
        }
    }

    /// What ⏎ runs, one split each, one after the other: `(the ptyro to run, None for the daemon's; its words)`.
    /// This computer's picks (Windows' on Windows) and WSL's go as `ptyro install … --confirmed`
    /// with `--where` as `cli_actions::where_runs` says (BOOT8); the same picks on both sides
    /// with this computer's daemon are one `--where both`. Err: Windows' ptyro can't be reached
    /// from WSL's daemon.
    pub fn commands(
        &self,
        wsl_daemon: bool,
        exe_in_wsl: Option<&str>,
    ) -> Result<Vec<Split>, String> {
        let Some(r) = self.ready() else { return Ok(Vec::new()) };
        let picked = |side: Side| -> Vec<String> {
            let mut ids: Vec<String> = r
                .plan
                .entries
                .iter()
                .filter(|e| e.side == side && e.picked)
                .map(|e| program(&e.id))
                .collect();
            ids.dedup();
            ids
        };
        let words = |ids: &[String], extra: Vec<String>| -> Vec<String> {
            std::iter::once("install".to_owned())
                .chain(ids.iter().cloned())
                .chain(std::iter::once("--confirmed".to_owned()))
                .chain(extra)
                .collect()
        };
        if !r.sides.here.platform.is_windows() {
            let ids = picked(Side::Local);
            return Ok(if ids.is_empty() { Vec::new() } else { vec![(None, words(&ids, vec![]))] });
        }
        let (win, wsl) = (picked(Side::Windows), picked(Side::Wsl));
        let mut out = Vec::new();
        let mut each: Vec<(&str, Vec<String>)> = Vec::new();
        if !wsl_daemon && !win.is_empty() && win == wsl {
            each.push(("both", win));
        } else {
            each.extend(
                [("windows", win), ("wsl", wsl)].into_iter().filter(|(_, i)| !i.is_empty()),
            );
        }
        for (choice, ids) in each {
            let runs = crate::cli_actions::where_runs(choice, wsl_daemon, exe_in_wsl);
            if runs.is_empty() {
                return Err("this ptyro.exe isn't where WSL can reach it: run `ptyro setup --where windows` in a Windows terminal".into());
            }
            out.extend(runs.into_iter().map(|(bin, extra)| (bin, words(&ids, extra))));
        }
        Ok(out)
    }
}

/// The name `ptyro install` takes for an item (its command: `agy`, `gh`).
fn program(id: &str) -> String {
    catalog::item(id).map_or_else(|| id.to_owned(), |i| i.program.clone())
}

/// The daemon is on another computer (not this one, nor this Windows' WSL): its setup is
/// there. `None`: this computer's.
pub fn elsewhere(remote: Option<&str>, windows: bool) -> Option<String> {
    let r = remote?;
    if ptyro_client::is_local(Some(r)) || (windows && r.starts_with("wsl://")) {
        return None;
    }
    Some(r.to_owned())
}

/// Look at this computer (and on Windows at WSL, through its ptyro), as `ptyro setup` does.
/// `remote` is the daemon this TUI uses: its WSL distro is the default side.
pub async fn look(paths: Paths, remote: Option<String>) -> Option<(Sides, Where, Option<String>)> {
    let here = tokio::task::spawn_blocking(move || {
        let host = setup::host(&paths);
        setup::survey(&*host, &BTreeMap::new())
    })
    .await
    .ok()?;
    let platform = here.platform;
    if !platform.is_windows() {
        let status = (platform == Platform::Wsl)
            .then(|| WslStatus::Inside { distro: std::env::var("WSL_DISTRO_NAME").ok() });
        return Some((Sides { here, side: Side::Local, wsl: None, status }, Where::Local, None));
    }
    let prefer = remote.as_deref().and_then(ptyro_client::remote::wsl_distro).map(str::to_owned);
    let mut status = wsl_setup::probe(prefer.as_deref()).await;
    let mut wsl = None;
    if let Some(d) = status.usable().map(str::to_owned) {
        match wsl_setup::survey(&d).await {
            Ok(s) => wsl = Some(s),
            Err(why) => status = why,
        }
    }
    let sides = Sides { here, side: Side::Windows, wsl, status: Some(status) };
    let (place, note) = checklist::default_place(prefer.as_deref(), sides.status.as_ref());
    let place = if sides.sides(place).is_empty() { Where::Windows } else { place };
    Some((sides, place, note.filter(|n| !n.is_empty())))
}

pub use ptyro_agents::setup::checklist::about;

/// One row's cells, per side, for a two-sided checklist.
pub fn cells(plan: &Plan, id: &str) -> Vec<String> {
    plan.sides().iter().map(|s| cell(plan.entry_on(id, *s))).collect()
}

/// The column heads: `Windows`, `WSL (Ubuntu)`.
pub fn heads(r: &Ready) -> Vec<String> {
    r.plan.sides().iter().map(|s| r.sides.name(*s)).collect()
}

/// What the side's survey found on `PATH` (for a method's `needs`).
pub fn programs_of<'a>(r: &'a Ready, side: Side) -> impl Fn(&str) -> bool + 'a {
    let survey = if side == Side::Wsl { r.sides.wsl.as_ref() } else { Some(&r.sides.here) };
    move |p: &str| survey.is_some_and(|s| s.programs.contains(p))
}

/// The two kinds, in the checklist's order and words.
pub const SECTIONS: [(&str, Kind); 2] = [("Agents", Kind::Agent), ("Tools", Kind::Tool)];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ptyro_agents::setup::{State, Survey};

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
        }
    }

    /// A Linux with apt, sudo and curl, and `installed`, looked at.
    pub(crate) fn one(installed: &[(&str, &str)]) -> Setup {
        let mut s = Setup::looking(None);
        let sides = Sides {
            here: survey(Platform::Linux, installed, &["apt-get", "sudo", "curl"]),
            side: Side::Local,
            wsl: None,
            status: None,
        };
        s.found(sides, Where::Local, None);
        s
    }

    #[test]
    fn the_defaults_tick_and_enter_installs_them_with_the_logins() {
        let s = one(&[("git", "2.43.0")]);
        let ticked: Vec<&str> =
            s.rows().iter().filter(|e| e.picked).map(|e| e.id.as_str()).collect();
        assert_eq!(ticked, ["claude", "codex", "gh"], "git is here already");
        let w = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            s.commands(false, None).unwrap(),
            [(None, w(&["install", "claude", "codex", "gh", "--confirmed"]))]
        );
    }

    #[test]
    fn a_prerequisite_ticks_itself_says_why_and_stays() {
        let mut s = one(&[]);
        let at = |s: &Setup, id: &str| s.rows().iter().position(|e| e.id == id).unwrap();
        s.toggle(at(&s, "gemini"));
        let node = s.rows()[at(&s, "node")].clone();
        assert!(node.ticked && !node.picked, "Node.js ticked for Gemini CLI");
        let r = s.ready().unwrap();
        let line = about(&r.plan, &node, &programs_of(r, Side::Local), false);
        assert!(line.contains("ticked because Gemini CLI needs it"), "{line}");
        s.toggle(at(&s, "node"));
        assert!(s.said.as_deref().unwrap().contains("untick that first"), "{:?}", s.said);
        assert!(s.rows()[at(&s, "node")].ticked);
        // Unticking Gemini CLI lets Node.js go.
        s.toggle(at(&s, "gemini"));
        assert!(!s.rows()[at(&s, "node")].ticked);
    }

    /// Windows with a usable WSL (Ubuntu), git on Windows, looked at; the defaults on Windows.
    pub(crate) fn two() -> Setup {
        let mut s = Setup::looking(None);
        let wsl = survey(Platform::Wsl, &[], &["apt-get", "sudo", "curl"]);
        let sides = Sides {
            here: survey(Platform::WindowsX64, &[("git", "2.51.0")], &["winget", "curl"]),
            side: Side::Windows,
            wsl: Some(wsl),
            status: Some(WslStatus::Usable { distro: "Ubuntu".into(), version: "0.0.63".into() }),
        };
        s.found(sides, Where::Windows, None);
        s
    }

    /// Windows with no usable WSL (its reason and the fix's long line), and a default-side note.
    pub(crate) fn unusable() -> Setup {
        let mut s = Setup::looking(None);
        let sides = Sides {
            here: survey(Platform::WindowsX64, &[], &["winget", "curl"]),
            side: Side::Windows,
            wsl: None,
            status: Some(WslStatus::Unusable {
                reason: "WSL isn't installed or has no distro".into(),
                fix: "`wsl --install`, then ptyro in it: `wsl -d Ubuntu -- sh -c \"curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh\"`".into(),
            }),
        };
        let note = "WSL: your daemon is in Debian, which setup can't reach; Ubuntu has ptyro, so `--where wsl` installs there. By default this sets up Windows only.";
        s.found(sides, Where::Windows, Some(note.into()));
        s
    }

    #[test]
    fn a_wanted_side_ticks_there() {
        let mut s = Setup::looking(Some(vec!["gh".into()]));
        s.want_place = Some(Where::Wsl);
        let t = two();
        let Found::Ready(r) = t.found else { panic!() };
        s.found(r.sides, Where::Windows, None);
        let r = s.ready().unwrap();
        assert_eq!(r.picks.wsl, ["gh"]);
        assert!(r.picks.here.is_empty());
    }

    #[test]
    fn two_sides_tick_per_column_and_run_where_each_goes() {
        let mut s = Setup::looking(None);
        let wsl = survey(Platform::Wsl, &[], &["apt-get", "sudo", "curl"]);
        let sides = Sides {
            here: survey(Platform::WindowsX64, &[("git", "2.51.0")], &["winget", "curl"]),
            side: Side::Windows,
            wsl: Some(wsl),
            status: Some(WslStatus::Usable { distro: "Ubuntu".into(), version: "0.0.63".into() }),
        };
        s.found(sides, Where::Windows, None);
        assert_eq!(s.col, 0, "Windows' column first");
        let w = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // Untick everything on Windows but Claude Code, then tick it on WSL's side too.
        for id in ["codex", "gh"] {
            let i = s.rows().iter().position(|e| e.id == id).unwrap();
            s.toggle(i);
        }
        s.side_step(1);
        s.toggle(0);
        let r = s.ready().unwrap();
        assert_eq!(cells(&r.plan, "claude")[1], "[x] the vendor's installer");
        assert_eq!(heads(r), ["Windows", "WSL (Ubuntu)"]);
        // The same on both, with this computer's daemon: one split, `--where both`.
        assert_eq!(
            s.commands(false, None).unwrap(),
            [(None, w(&["install", "claude", "--confirmed", "--where", "both"]))]
        );
        // With WSL's daemon: WSL's own ptyro for its side, this ptyro.exe through interop.
        let exe = "/mnt/c/ptyro.exe";
        assert_eq!(
            s.commands(true, Some(exe)).unwrap(),
            [
                (
                    Some(exe.to_owned()),
                    w(&["install", "claude", "--confirmed", "--where", "windows", "--tty"])
                ),
                (None, w(&["install", "claude", "--confirmed"])),
            ]
        );
        assert!(s.commands(true, None).is_err(), "Windows' ptyro out of WSL's reach");
    }

    #[test]
    fn a_daemon_on_another_computer_is_set_up_there() {
        assert_eq!(elsewhere(None, false), None);
        assert_eq!(elsewhere(Some("local"), false), None);
        assert_eq!(elsewhere(Some("wsl://Ubuntu"), true), None);
        assert_eq!(elsewhere(Some("me@box"), true).as_deref(), Some("me@box"));
    }
}
