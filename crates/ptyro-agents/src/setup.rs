//! The plan engine (ROADMAP BOOT1, SPEC §24.2): what is installed, what to install for what the
//! user ticked (and what that needs first), and running it with live output. The CLI's `ptyro
//! setup` and `ptyro install`, the TUI and the daemon (for the web) all use this one engine, so
//! the three can't drift. It needs no daemon: a fresh machine has none yet.
//!
//! The steps:
//! 1. [`survey`]: each row of the version table ([`crate::catalog`]) found or not, with its
//!    version, through a [`Host`] (the real machine, or the fake a test names, [`host`]);
//! 2. [`plan`]: the user's picks, the prerequisites they need ticked too, and per ticked item the
//!    method and command, chosen by the table's order and what is present;
//! 3. [`run`]: each step in order, its output line by line, ✓ or ✗ with the fix; admin steps are
//!    asked for once, and a failure leaves the rest going.
//!
//! Node.js follows the design's rule: the platform's package manager when its Node.js is new
//! enough for what needs it (apt's and dnf's candidate version is read first), else nodejs.org's
//! build in a folder ptyro owns ([`ptyro_core::paths::Paths::tools`]), which goes on `PATH` through
//! `ptyro shell-init` and the daemon's environment ([`path_dirs`]).

mod fake;
mod host;
mod run;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::catalog::{self, Item, Kind, Method, Platform, Version, Via};

pub use fake::installer as fake_installer;
pub use host::{Elevation, Host, Installed, RealHost, Step, host, path_dirs, path_with};
pub use run::{Admin, Event, Report, run};

/// The design's default ticks (its "My calls"): Claude Code, Codex, git and the GitHub CLI. One
/// already installed isn't ticked. (Codex no longer needs Node.js, BOOT0, so Node.js isn't.)
pub const DEFAULTS: &[&str] = &["claude", "codex", "git", "gh"];

/// Where an item goes. One side today; BOOT8 adds WSL's and Windows' on a Windows machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// This computer, where the engine runs.
    #[default]
    Local,
}

/// What an item is on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Missing,
    Installed {
        /// `None`: it runs, but what it printed held no version.
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// A newer version is out (BOOT5 fills in the newest; the engine updates it).
    Outdated {
        version: String,
        latest: String,
        #[serde(default)]
        path: Option<String>,
    },
    /// Installed some way other than the table's recommended one (Claude Code through npm):
    /// BOOT7 offers the move.
    Other {
        #[serde(default)]
        version: Option<String>,
        via: Via,
        #[serde(default)]
        path: Option<String>,
    },
    /// Older than something ticked needs (Node.js 18 for Gemini CLI's 20).
    TooOld {
        version: String,
        min: String,
        #[serde(default)]
        path: Option<String>,
    },
}

impl State {
    /// Present, whatever its version.
    pub fn present(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    /// The installed version, when known.
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Missing => None,
            Self::Installed { version, .. } | Self::Other { version, .. } => version.as_deref(),
            Self::Outdated { version, .. } | Self::TooOld { version, .. } => Some(version),
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Missing => None,
            Self::Installed { path, .. }
            | Self::Other { path, .. }
            | Self::Outdated { path, .. }
            | Self::TooOld { path, .. } => path.as_deref(),
        }
    }

    /// `installed 2.1.319`, `2.1.300, 2.1.319 is out`… for a checklist line.
    pub fn words(&self) -> String {
        let v = |v: Option<&str>| v.map(|v| format!(" {v}")).unwrap_or_default();
        match self {
            Self::Missing => "not installed".into(),
            Self::Installed { version, .. } => format!("installed{}", v(version.as_deref())),
            Self::Outdated { version, latest, .. } => format!("{version}; {latest} is out"),
            Self::Other { version, via, .. } => {
                format!("installed{} through {}", v(version.as_deref()), via.words())
            }
            Self::TooOld { version, min, .. } => format!("{version}, older than the {min} needed"),
        }
    }
}

/// What the engine found: each item's state, the programs that decide methods, and the
/// package managers' Node.js.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Survey {
    pub platform: Platform,
    /// By item id, every row of the table.
    pub states: BTreeMap<String, State>,
    /// The methods' `needs` (and `sudo`, `curl`) that are on `PATH`.
    pub programs: BTreeSet<String>,
    /// The version of Node.js apt or dnf would install (`apt` → `18.19.1`), when it could be read.
    pub candidates: BTreeMap<String, String>,
}

/// Look at the machine: every row of the table through `host`. `latest` holds newest versions
/// by item id (BOOT5's daily check); an older installed one is [`State::Outdated`].
pub fn survey(host: &dyn Host, latest: &BTreeMap<String, String>) -> Survey {
    let platform = host.platform();
    let mut wanted: BTreeSet<String> = ["sudo", "curl"].map(str::to_owned).into();
    for i in catalog::items() {
        wanted.extend(i.methods(platform).filter_map(|m| m.needs.clone()));
    }
    let programs = wanted.into_iter().filter(|p| host.has(p)).collect::<BTreeSet<_>>();
    let mut states = BTreeMap::new();
    for item in catalog::items() {
        let state = match host.installed(item) {
            None => State::Missing,
            Some(found) => classify(item, platform, found, latest.get(&item.id)),
        };
        states.insert(item.id.clone(), state);
    }
    let mut candidates = BTreeMap::new();
    if let Some(node) = catalog::item("node") {
        for m in node.methods(platform).filter(|m| matches!(m.via, Via::Apt | Via::Dnf)) {
            if m.needs.as_ref().is_some_and(|n| programs.contains(n))
                && let Some(v) = host.candidate(m.via)
            {
                candidates.insert(m.via.name().to_owned(), v);
            }
        }
    }
    Survey { platform, states, programs, candidates }
}

fn classify(item: &Item, platform: Platform, found: Installed, latest: Option<&String>) -> State {
    let recommended = item.methods(platform).next().map(|m| m.via);
    if let Some(via) = found.via
        && Some(via) != recommended
        && item.others.iter().any(|m| m.via == via && m.on(platform))
    {
        return State::Other { version: found.version, via, path: found.path };
    }
    if let (Some(v), Some(l)) = (&found.version, latest)
        && let (Some(have), Some(new)) = (Version::parse(v), Version::parse(l))
        && have < new
    {
        return State::Outdated { version: v.clone(), latest: l.clone(), path: found.path };
    }
    State::Installed { version: found.version, path: found.path }
}

/// How a ticked item gets installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub via: Via,
    /// What runs, as a person would type it (nodejs.org's build: what it does, in words).
    pub command: String,
    /// It asks for administrator rights (sudo, a UAC prompt).
    pub admin: bool,
    /// The method's update command rather than its install (an outdated or too old one).
    #[serde(default)]
    pub update: bool,
    /// Ticked items that must be installed first: if one fails, this is skipped.
    #[serde(default)]
    pub after: Vec<String>,
}

/// One line of the checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Beside the name: Gemini CLI's "for Gemini API-key and enterprise accounts…".
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub side: Side,
    #[serde(flatten)]
    pub state: State,
    /// Ticked: picked, or needed by something picked.
    pub ticked: bool,
    /// The user ticked it (not only because something needs it).
    pub picked: bool,
    /// The ticked items that need it: it was ticked for them.
    #[serde(default)]
    pub needed_by: Vec<String>,
    /// What installing it runs; `None` when there's nothing to do or no way here.
    #[serde(default)]
    pub action: Option<Action>,
    /// Ticked, but there's no way to install it here, and why (with the vendor's page).
    #[serde(default)]
    pub blocked: Option<String>,
    /// The vendor's page.
    pub doc: String,
}

impl Entry {
    /// Something to install for it (an action and ticked).
    pub fn will_run(&self) -> bool {
        self.ticked && self.action.is_some()
    }
}

/// The checklist with what installing it would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub platform: Platform,
    /// Every row of the table, agents first, in its order.
    pub entries: Vec<Entry>,
}

impl Plan {
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// What runs, in order: each item after what it needs.
    pub fn steps(&self) -> Vec<&Entry> {
        let mut left: Vec<&Entry> = self.entries.iter().filter(|e| e.will_run()).collect();
        let mut out: Vec<&Entry> = Vec::new();
        while !left.is_empty() {
            let ready = left.iter().position(|e| {
                e.action.as_ref().is_none_or(|a| {
                    a.after.iter().all(|d| out.iter().any(|o| &o.id == d))
                        || a.after.iter().all(|d| !left.iter().any(|l| &l.id == d))
                })
            });
            // A cycle can't come from the table (the tests check), but never loop on one.
            out.push(left.remove(ready.unwrap_or(0)));
        }
        out
    }

    /// The steps that need administrator rights.
    pub fn admin(&self) -> Vec<&Entry> {
        self.steps().into_iter().filter(|e| e.action.as_ref().is_some_and(|a| a.admin)).collect()
    }
}

/// The default ticks that aren't installed yet.
pub fn defaults(survey: &Survey) -> Vec<String> {
    DEFAULTS
        .iter()
        .filter(|id| survey.states.get(**id).is_none_or(|s| !s.present()))
        .map(|id| (*id).to_owned())
        .collect()
}

/// The item a person means: its id (`antigravity`), its command (`agy`) or its name, in any
/// case and without the spaces or dots (`github-cli`, `"Claude Code"`, `nodejs`).
pub fn resolve(name: &str) -> Result<&'static Item, String> {
    let squash = |s: &str| -> String {
        s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase()
    };
    let n = squash(name);
    catalog::items()
        .iter()
        .find(|i| squash(&i.id) == n || squash(&i.program) == n || squash(&i.name) == n)
        .ok_or_else(|| {
            let known: Vec<&str> = catalog::items()
                .iter()
                .map(|i| if i.program != i.id { i.program.as_str() } else { i.id.as_str() })
                .collect();
            format!("{name:?} isn't something ptyro installs (it knows {})", known.join(", "))
        })
}

/// The checklist for `picks` (item ids): each ticked item's method and command, and the
/// prerequisites they need ticked too. Ids the table doesn't have are ignored ([`resolve`] first).
pub fn plan(survey: &Survey, picks: &[String]) -> Plan {
    let platform = survey.platform;
    let picked: BTreeSet<&str> = picks.iter().map(String::as_str).collect();
    let state = |id: &str| survey.states.get(id).cloned().unwrap_or(State::Missing);
    // What each item needs of another: needed id → [(by, min)]. Only required prerequisites
    // tick; recommended ones (git for Codex's PR helpers) are the user's pick.
    let mut needs: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    let mut ticked: BTreeSet<String> = picked.iter().map(|s| (*s).to_owned()).collect();
    let mut queue: Vec<String> = ticked.iter().cloned().collect();
    while let Some(id) = queue.pop() {
        let Some(item) = catalog::item(&id) else { continue };
        for p in item.prereqs(platform).filter(|p| p.required) {
            needs.entry(p.item.clone()).or_default().push((id.clone(), p.min.clone()));
            if !satisfies(&state(&p.item), &needs[&p.item]) && ticked.insert(p.item.clone()) {
                queue.push(p.item.clone());
            }
        }
    }
    // Whether npm will be there: Node.js is, or is going to be installed.
    let node_coming = ticked.contains("node") || state("node").present();
    let has = |p: &str| survey.programs.contains(p) || (p == "npm" && node_coming);
    let mut entries = Vec::new();
    for item in catalog::items() {
        let wants = needs.get(&item.id).cloned().unwrap_or_default();
        let mut st = state(&item.id);
        let min = highest(wants.iter().filter_map(|(_, m)| m.as_deref()));
        if let (Some(min), Some(v)) = (&min, st.version().and_then(Version::parse))
            && st.present()
            && !v.at_least(min)
        {
            st = State::TooOld {
                version: st.version().unwrap_or_default().to_owned(),
                min: min.clone(),
                path: st.path().map(str::to_owned),
            };
        }
        let is_ticked = ticked.contains(&item.id);
        let needed_by: Vec<String> = if is_ticked && !satisfies(&state(&item.id), &wants) {
            wants.iter().map(|(by, _)| by.clone()).collect()
        } else {
            Vec::new()
        };
        let chosen = match &st {
            State::Missing => method_for(item, survey, min.as_deref(), None, &has),
            State::TooOld { version, .. } => {
                method_for(item, survey, min.as_deref(), Version::parse(version), &has)
            }
            State::Outdated { .. } => item.method(platform, has).map(|m| (m, true)),
            State::Installed { .. } | State::Other { .. } => None,
        };
        let after: Vec<String> = item
            .prereqs(platform)
            .filter(|p| p.required && ticked.contains(&p.item))
            .filter(|p| !satisfies(&state(&p.item), needs.get(&p.item).map_or(&[], Vec::as_slice)))
            .map(|p| p.item.clone())
            .collect();
        let action = chosen.and_then(|(m, update)| {
            let command = command_of(m, update)?;
            Some(Action { via: m.via, command, admin: m.admin, update, after: after.clone() })
        });
        let blocked =
            (is_ticked && action.is_none() && matches!(st, State::Missing | State::TooOld { .. }))
                .then(|| why_not(item, platform, &has));
        entries.push(Entry {
            id: item.id.clone(),
            name: item.name.clone(),
            kind: item.kind,
            label: item.label.clone(),
            side: Side::Local,
            state: st,
            ticked: is_ticked,
            picked: picked.contains(item.id.as_str()),
            needed_by,
            action: if is_ticked { action } else { None },
            blocked,
            doc: item.doc.clone(),
        });
    }
    Plan { platform, entries }
}

/// `state` is enough for every `(by, min)` that needs it.
fn satisfies(state: &State, wants: &[(String, Option<String>)]) -> bool {
    match state {
        State::Missing => false,
        // A version that couldn't be read is taken as enough: better than reinstalling.
        s => s
            .version()
            .and_then(Version::parse)
            .is_none_or(|v| wants.iter().filter_map(|(_, m)| m.as_deref()).all(|m| v.at_least(m))),
    }
}

/// The highest of some minimum versions.
fn highest<'a>(mins: impl Iterator<Item = &'a str>) -> Option<String> {
    mins.filter_map(|m| Version::parse(m).map(|v| (v, m)))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, m)| m.to_owned())
}

/// The method for a missing (or too old, `have`) item, and whether it's the update command.
/// Node.js: apt's or dnf's only when the version it would install meets `min`; a package
/// manager that ships the newest (Homebrew, WinGet) as it is; else nodejs.org's build.
fn method_for<'a>(
    item: &'a Item,
    survey: &Survey,
    min: Option<&str>,
    have: Option<Version>,
    has: &dyn Fn(&str) -> bool,
) -> Option<(&'a Method, bool)> {
    let too_old = have.is_some();
    for m in item.methods(survey.platform) {
        if m.needs.as_deref().is_some_and(|n| !has(n)) {
            continue;
        }
        match m.via {
            Via::Apt | Via::Dnf => {
                let cand = survey.candidates.get(m.via.name()).and_then(|c| Version::parse(c));
                let new_enough = match (&cand, min) {
                    (Some(c), Some(min)) => c.at_least(min),
                    (None, Some(_)) => false,
                    (_, None) => true,
                };
                let newer = match (&cand, &have) {
                    (Some(c), Some(h)) => c > h,
                    (None, Some(_)) => false,
                    (_, None) => true,
                };
                if new_enough && newer {
                    // apt's and dnf's `install` brings an old one up to the candidate too.
                    return Some((m, false));
                }
            }
            Via::NodeDist => return Some((m, false)),
            _ => return Some((m, too_old && m.update.is_some())),
        }
    }
    None
}

/// What a step runs, for people.
fn command_of(m: &Method, update: bool) -> Option<String> {
    if m.via == Via::NodeDist {
        return Some(
            "download the newest Node.js LTS from nodejs.org, check it against SHASUMS256.txt and unpack it into ptyro's tools folder (no admin rights)"
                .into(),
        );
    }
    if update { m.update.clone().or_else(|| m.install.clone()) } else { m.install.clone() }
}

/// Why nothing installs `item` here: the programs its methods need.
fn why_not(item: &Item, platform: Platform, has: &dyn Fn(&str) -> bool) -> String {
    let missing: Vec<&str> =
        item.methods(platform).filter_map(|m| m.needs.as_deref()).filter(|n| !has(n)).collect();
    if missing.is_empty() {
        format!("no way to install it on {} is known: see {}", platform.name(), item.doc)
    } else {
        format!(
            "it needs {} first, which isn't installed: see {}",
            missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(" or "),
            item.doc
        )
    }
}
