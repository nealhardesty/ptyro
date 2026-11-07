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

pub mod checklist;
mod fake;
pub(crate) mod host;
pub mod lock;
pub mod login;
mod moving;
mod run;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::catalog::{self, Item, Kind, Method, Platform, Version, Via};

pub use fake::installer as fake_installer;
#[cfg(windows)]
pub use host::registry_path;
pub use host::{
    Elevation, Host, Installed, RealHost, Step, expand_vars, host, path_dirs, path_with,
    prepend_entries, refreshed_path,
};
pub use run::{Admin, Event, Report, Sided, in_use, run};

/// The design's default ticks (its "My calls"): Claude Code, Codex, git and the GitHub CLI. One
/// already installed isn't ticked. (Codex no longer needs Node.js, BOOT0, so Node.js isn't.)
pub const DEFAULTS: &[&str] = &["claude", "codex", "git", "gh"];

/// Where an item goes (BOOT8, SPEC §24.3). On Windows an item can go onto Windows itself, into
/// the WSL distro, or both; everywhere else there is one side, this computer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// This computer, where the engine runs (Linux, macOS, and WSL seen from inside).
    #[default]
    Local,
    /// Windows itself, from Windows' ptyro (the engine running there).
    Windows,
    /// The WSL distro, from Windows: its own ptyro does the work, through `wsl.exe`.
    Wsl,
}

impl Side {
    /// `Windows`, `WSL`, `this computer`.
    pub fn words(self) -> &'static str {
        match self {
            Self::Local => "this computer",
            Self::Windows => "Windows",
            Self::Wsl => "WSL",
        }
    }
}

/// What `--where` asks for (BOOT8): on Windows `windows`, `wsl` or `both`; `local` is this
/// computer everywhere (Windows itself, on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Where {
    Local,
    Windows,
    Wsl,
    Both,
}

impl std::str::FromStr for Where {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" | "here" => Ok(Self::Local),
            "windows" | "win" => Ok(Self::Windows),
            "wsl" | "linux" => Ok(Self::Wsl),
            "both" => Ok(Self::Both),
            _ => Err(format!("--where takes wsl, windows or both (not {s:?})")),
        }
    }
}

impl Where {
    /// The sides it means on a machine whose own side is `here` (`Windows` on Windows, else
    /// `Local`).
    pub fn sides(self, here: Side) -> Vec<Side> {
        match self {
            Self::Local | Self::Windows => vec![here],
            Self::Wsl => vec![Side::Wsl],
            Self::Both => vec![here, Side::Wsl],
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Windows => "windows",
            Self::Wsl => "wsl",
            Self::Both => "both",
        }
    }
}

/// WSL, as setup on Windows found it (BOOT8). WSL is the user's to bring: setup never installs
/// WSL, a distro, or ptyro inside one; without a usable one only the Windows side is offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WslStatus {
    /// `wsl.exe`, a distro, and ptyro in it answering `--version`.
    Usable { distro: String, version: String },
    /// Not usable, why, and how to make it so.
    Unusable { reason: String, fix: String },
    /// The engine runs inside WSL (`distro`): this is the WSL side; Windows' side is set up
    /// from Windows.
    Inside {
        #[serde(default)]
        distro: Option<String>,
    },
}

impl WslStatus {
    pub fn usable(&self) -> Option<&str> {
        match self {
            Self::Usable { distro, .. } => Some(distro),
            _ => None,
        }
    }

    /// One line for a checklist: what WSL is, and what to do about it.
    pub fn words(&self) -> String {
        match self {
            Self::Usable { distro, version } => {
                format!("WSL: {distro}, with {version}: agents and tools can go there too")
            }
            Self::Unusable { reason, fix } => {
                format!("WSL: {reason}, so this sets up Windows only. For WSL too: {fix}")
            }
            Self::Inside { distro } => format!(
                "This is WSL{}: setup here installs into it. For Windows itself, run `ptyro setup --where windows` in a Windows terminal.",
                distro.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
            ),
        }
    }
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
    /// offered the move to the recommended one (BOOT7, [`Entry::moves`]); kept
    /// (`updates.keep_installer`), it updates the way it was installed (BOOT5).
    Other {
        #[serde(default)]
        version: Option<String>,
        via: Via,
        #[serde(default)]
        path: Option<String>,
        /// A newer version is out (BOOT5): its update is that installer's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latest: Option<String>,
        /// It is in a folder only an administrator can change (a system npm prefix): removing
        /// it, after a move, needs administrator rights (BOOT7).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        system: bool,
        /// npm's: the prefix it is in, which its removal acts on (BOOT7); `None` when npm doesn't
        /// manage that folder (Bun's), and then no move is offered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
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
            Self::Other { version, via, latest: None, .. } => {
                format!("installed{} through {}", v(version.as_deref()), via.words())
            }
            Self::Other { version, via, latest: Some(l), .. } => {
                format!("{} through {}; {l} is out", version.as_deref().unwrap_or("?"), via.words())
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
    /// Agents kept the way they were installed (`updates.keep_installer`, BOOT7, as ids): the
    /// move to the recommended installer isn't offered, and ticking one updates it its own way.
    /// The caller fills it from its own side's config ([`crate::updates::survey`]).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub keep: BTreeSet<String>,
    /// The methods' `needs` (and `sudo`, `curl`) that are on `PATH`.
    pub programs: BTreeSet<String>,
    /// The version of Node.js apt or dnf would install (`apt` → `18.19.1`), when it could be read.
    pub candidates: BTreeMap<String, String>,
}

/// Look at the machine: every row of the table through `host`. `latest` holds newest versions
/// by item id (BOOT5's daily check); an older installed one is [`State::Outdated`].
pub fn survey(host: &dyn Host, latest: &BTreeMap<String, String>) -> Survey {
    survey_of(host, latest, &|_| true)
}

/// [`survey`] of the agents only (BOOT5's refreshes, every few minutes: no git, GitHub CLI or
/// Node.js versions asked, no package manager's candidate); other rows are left out.
pub fn survey_agents(host: &dyn Host, latest: &BTreeMap<String, String>) -> Survey {
    survey_of(host, latest, &|i| i.kind == Kind::Agent)
}

fn survey_of(
    host: &dyn Host,
    latest: &BTreeMap<String, String>,
    wanted_item: &dyn Fn(&Item) -> bool,
) -> Survey {
    let platform = host.platform();
    let items: Vec<&Item> = catalog::items().iter().filter(|i| wanted_item(i)).collect();
    let mut wanted: BTreeSet<String> = ["sudo", "curl"].map(str::to_owned).into();
    for i in &items {
        wanted.extend(i.methods(platform).filter_map(|m| m.needs.clone()));
    }
    let programs = wanted.into_iter().filter(|p| host.has(p)).collect::<BTreeSet<_>>();
    let mut states = BTreeMap::new();
    for item in &items {
        let state = match host.installed(item) {
            None => State::Missing,
            Some(found) => classify(item, platform, found, latest.get(&item.id)),
        };
        states.insert(item.id.clone(), state);
    }
    let mut candidates = BTreeMap::new();
    if let Some(node) = items.iter().find(|i| i.id == "node") {
        for m in node.methods(platform).filter(|m| matches!(m.via, Via::Apt | Via::Dnf)) {
            if m.needs.as_ref().is_some_and(|n| programs.contains(n))
                && let Some(v) = host.candidate(m.via)
            {
                candidates.insert(m.via.name().to_owned(), v);
            }
        }
    }
    Survey { platform, states, programs, candidates, keep: BTreeSet::new() }
}

/// The way an agent installed another way (`via`) updates: that installer's `others` row with
/// an `update` (BOOT5). The plan and the findings both use it, so they can't disagree.
pub fn other_update(item: &Item, platform: Platform, via: Via) -> Option<&Method> {
    item.others.iter().find(|m| m.via == via && m.on(platform) && m.update.is_some())
}

fn classify(item: &Item, platform: Platform, found: Installed, latest: Option<&String>) -> State {
    let recommended = item.methods(platform).next().map(|m| m.via);
    let newer = match (&found.version, latest) {
        (Some(v), Some(l)) => Version::parse(v)
            .zip(Version::parse(l))
            .is_some_and(|(have, new)| have < new)
            .then(|| l.clone()),
        _ => None,
    };
    if let Some(via) = found.via
        && Some(via) != recommended
        && item.others.iter().any(|m| m.via == via && m.on(platform))
    {
        return State::Other {
            version: found.version,
            via,
            path: found.path,
            latest: newer,
            system: found.system,
            prefix: found.prefix,
        };
    }
    if let (Some(v), Some(l)) = (&found.version, newer) {
        return State::Outdated { version: v.clone(), latest: l, path: found.path };
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
    /// A move to the recommended installer (BOOT7): install it this way, confirm it runs, then
    /// remove the old copy ([`Entry::moves`] says how).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moving: bool,
}

/// The move an agent installed another way is offered (BOOT7, the design's goal 9): the
/// recommended installer first, the new copy confirmed to run, and only then the old copy removed
/// with its own installer's uninstall. Settings and logins stay where they are (`keeps`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moving {
    /// How it is installed now (`npm`).
    pub from: Via,
    /// The recommended way it moves to (`vendor`).
    pub to: Via,
    /// What removes the old copy, as typed (`npm uninstall -g @openai/codex`; with `sudo` for a
    /// copy in an administrator's folder).
    pub uninstall: String,
    /// Removing the old copy needs administrator rights.
    #[serde(default)]
    pub admin: bool,
    /// Where its settings and logins live, which stay (`~/.codex`).
    #[serde(default)]
    pub keeps: Vec<String>,
}

impl Moving {
    /// `move from npm to the vendor's installer`.
    pub fn words(&self) -> String {
        format!("move from {} to {}", self.from.words(), self.to.words())
    }

    /// `your settings and login (~/.codex) stay`.
    pub fn keeps_words(&self) -> String {
        if self.keeps.is_empty() {
            "your settings and logins stay".into()
        } else {
            format!("your settings and logins ({}) stay", self.keeps.join(", "))
        }
    }
}

/// The move `item` is offered, when it is installed another way (`state`), not kept
/// (`updates.keep_installer`), and both its recommended method here (`has`: what is on `PATH`)
/// and its current installer's uninstall are known.
pub fn offer(
    item: &Item,
    platform: Platform,
    state: &State,
    keep: &BTreeSet<String>,
    has: &dyn Fn(&str) -> bool,
) -> Option<Moving> {
    let State::Other { via, system, prefix, .. } = state else { return None };
    if keep.contains(&item.id) {
        return None;
    }
    // Homebrew refuses to run as root, so a keg that isn't this user's can't be removed here.
    if *via == Via::Brew && *system {
        return None;
    }
    let to = item.method(platform, has).filter(|m| m.via != *via && m.install.is_some())?;
    // The vendor installer's copy must be known by its path, or the move can't tell it apart.
    if to.via == Via::Vendor && to.places.is_empty() {
        return None;
    }
    let old = item.others.iter().find(|m| m.via == *via && m.on(platform))?;
    let plain = old.uninstall.as_deref()?;
    // npm's removal acts on the prefix the old copy is in (another nvm Node.js's, ptyro's own),
    // never npm's prefix of the moment; one npm didn't install (Bun's) isn't offered.
    let plain = if *via == Via::Npm {
        let at = quote(prefix.as_deref()?, platform)?;
        let rest = plain.strip_prefix("npm uninstall -g ")?;
        format!("npm uninstall -g --prefix {at} {rest}")
    } else {
        plain.to_owned()
    };
    // A copy only an administrator can change: sudo removes it (Windows' installers raise
    // their own prompt).
    let admin = *system;
    let uninstall = if admin && !platform.is_windows() { format!("sudo {plain}") } else { plain };
    Some(Moving { from: *via, to: to.via, uninstall, admin, keeps: item.keeps.clone() })
}

/// Why `item`, installed another way (`state`), isn't offered the move (BOOT7, said by `doctor`);
/// `None` when it is, or isn't installed another way, or is kept.
pub fn not_offered(
    item: &Item,
    platform: Platform,
    state: &State,
    has: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let State::Other { via, system, prefix, .. } = state else { return None };
    if offer(item, platform, state, &BTreeSet::new(), has).is_some() {
        return None;
    }
    let to = item.method(platform, has);
    Some(if *via == Via::Brew && *system {
        "its Homebrew isn't yours, and Homebrew won't run as administrator: move it as the Homebrew owner".into()
    } else if to.is_some_and(|m| m.via == Via::Vendor && m.places.is_empty()) {
        format!(
            "where {}'s own installer puts it on {} isn't known, so its copy couldn't be told apart",
            item.name,
            platform.name()
        )
    } else if *via == Via::Npm && prefix.is_none() {
        "it isn't in an npm prefix (Bun's, mise's or another tool's store): remove it with that tool, then `ptyro install` it".into()
    } else if to.is_none() {
        format!("its recommended installer can't run here: see {}", item.doc)
    } else {
        format!("there's no known way to remove {}'s copy", via.words())
    })
}

/// The copy `method` installed: a package manager's by its folder (`via`); a vendor's installer's
/// by being in one of its `places` (any other copy no package manager claims, from Nix, Scoop,
/// mise, pipx or a wrapper script, never counts).
pub fn installed_by(c: &Installed, method: &Method) -> bool {
    if method.via != Via::Vendor {
        return c.via == Some(method.via);
    }
    if c.via.is_some() {
        return false;
    }
    let norm = |p: &str| {
        let n = p.replace('\\', "/");
        let n = n.trim_end_matches('/').to_owned();
        if cfg!(windows) { n.to_ascii_lowercase() } else { n }
    };
    method.places.iter().any(|place| {
        let dir = norm(&expand_place(place));
        [&c.real, &c.file].into_iter().flatten().any(|f| {
            let f = norm(f);
            f == dir || f.starts_with(&format!("{dir}/"))
        })
    })
}

/// A catalog place as a path here: `~` the home, `%VAR%` from the environment.
pub fn expand_place(place: &str) -> String {
    let home = ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|v| std::env::var(v).ok().filter(|h| !h.is_empty()))
        .unwrap_or_default();
    let p =
        place.strip_prefix('~').map_or_else(|| place.to_owned(), |rest| format!("{home}{rest}"));
    expand_vars(&p, |v| std::env::var(v).ok())
}

/// A path as one word for the platform's shell (`sh` or PowerShell): single quotes; `None` for
/// one that can't be (a quote in it).
fn quote(path: &str, platform: Platform) -> Option<String> {
    if path.contains('\'') || (!platform.is_windows() && path.contains('\\')) {
        return None;
    }
    Some(format!("'{path}'"))
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
    /// Installed another way, and offered the move to the recommended installer (BOOT7):
    /// ticking it moves it ([`Action::moving`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moves: Option<Moving>,
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
    /// Every row of the table, agents first, in its order; on Windows with a usable WSL, every
    /// row again for WSL's side (`side: wsl`), after Windows' rows.
    pub entries: Vec<Entry>,
    /// On Windows (and inside WSL): WSL as setup found it (BOOT8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl: Option<WslStatus>,
}

impl Plan {
    /// The entry for `id` (the first side's, when there are two: [`Plan::entry_on`]).
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn entry_on(&self, id: &str, side: Side) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id && e.side == side)
    }

    /// The sides it has rows for, in order.
    pub fn sides(&self) -> Vec<Side> {
        let mut out: Vec<Side> = Vec::new();
        for e in &self.entries {
            if !out.contains(&e.side) {
                out.push(e.side);
            }
        }
        out
    }

    /// Only `side`'s rows (what one engine runs).
    pub fn side(&self, side: Side) -> Plan {
        Plan {
            platform: self.platform,
            entries: self.entries.iter().filter(|e| e.side == side).cloned().collect(),
            wsl: self.wsl.clone(),
        }
    }

    /// Every row said to be `side`'s (WSL's plan, made from its survey, is `Wsl` on Windows).
    pub fn on(mut self, side: Side) -> Plan {
        for e in &mut self.entries {
            e.side = side;
        }
        self
    }

    /// One plan of Windows' rows and WSL's (BOOT8): both sides on one checklist.
    pub fn merge(mine: Plan, wsl: Option<Plan>, status: Option<WslStatus>) -> Plan {
        let mut entries = mine.entries;
        entries.extend(wsl.into_iter().flat_map(|p| p.on(Side::Wsl).entries));
        Plan { platform: mine.platform, entries, wsl: status }
    }

    /// What runs, in order: each item after what it needs (on its own side).
    pub fn steps(&self) -> Vec<&Entry> {
        let mut left: Vec<&Entry> = self.entries.iter().filter(|e| e.will_run()).collect();
        let mut out: Vec<&Entry> = Vec::new();
        while !left.is_empty() {
            let ready = left.iter().position(|e| {
                e.action.as_ref().is_none_or(|a| {
                    a.after.iter().all(|d| out.iter().any(|o| &o.id == d && o.side == e.side))
                        || a.after
                            .iter()
                            .all(|d| !left.iter().any(|l| &l.id == d && l.side == e.side))
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
        let moves = offer(item, platform, &st, &survey.keep, &has);
        let chosen = match &st {
            // Offered the move (BOOT7): the recommended method's install, then the old removed.
            State::Other { .. } if moves.is_some() => {
                item.method(platform, has).map(|m| (m, false))
            }
            State::Missing => method_for(item, survey, min.as_deref(), None, &has),
            State::TooOld { version, .. } => {
                method_for(item, survey, min.as_deref(), Version::parse(version), &has)
            }
            State::Outdated { .. } => item.method(platform, has).map(|m| (m, true)),
            // Out of date, installed another way: that installer's own update (BOOT5).
            State::Other { via, latest: Some(_), .. } => {
                other_update(item, platform, *via).map(|m| (m, true))
            }
            State::Installed { .. } | State::Other { .. } => None,
        };
        let after: Vec<String> = item
            .prereqs(platform)
            .filter(|p| p.required && ticked.contains(&p.item))
            .filter(|p| !satisfies(&state(&p.item), needs.get(&p.item).map_or(&[], Vec::as_slice)))
            .map(|p| p.item.clone())
            .collect();
        let action = chosen.and_then(|(m, update)| {
            let mut command = command_of(m, update)?;
            let mut admin = m.admin;
            if let Some(mv) = &moves {
                command = format!("{command}\n{}", mv.uninstall);
                admin |= mv.admin;
            }
            Some(Action {
                via: m.via,
                command,
                admin,
                update,
                after: after.clone(),
                moving: moves.is_some(),
            })
        });
        let blocked =
            (is_ticked && action.is_none() && matches!(st, State::Missing | State::TooOld { .. }))
                .then(|| why_not(item, platform, &has));
        entries.push(Entry {
            id: item.id.clone(),
            name: item.name.clone(),
            kind: item.kind,
            label: item.label.clone(),
            side: if platform.is_windows() { Side::Windows } else { Side::Local },
            state: st,
            ticked: is_ticked,
            picked: picked.contains(item.id.as_str()),
            needed_by,
            action: if is_ticked { action } else { None },
            moves,
            blocked,
            doc: item.doc.clone(),
        });
    }
    Plan { platform, entries, wsl: None }
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
