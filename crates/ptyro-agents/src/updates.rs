//! Out of date, offered (ROADMAP BOOT5, SPEC §24.7): the newest version of each agent, looked
//! up at most once a day and remembered in `<state>/versions.json`, the agents older than it,
//! and the one-step update each is offered with. Nothing here updates anything: an update is
//! the plan engine's ([`crate::setup`]), run with the person's yes, under the machine's setup
//! lock. Nothing here prints either: a failed lookup (offline, a proxy) is kept quietly and
//! tried again the next day; only `doctor` says so.
//!
//! Who looks: the daemon, in the background, once a day (so the TUI's health badge, the web's
//! Health page and the start-up note all read the one result), and `ptyro doctor` and `ptyro
//! update` afresh. Start-up never touches the network: the TUI asks its daemon, which answers
//! from what the last look remembered.
//!
//! The rows are the version table's agents with a newest-version source ([`sources`]), and
//! ptyro itself (BOOT6, [`crate::own`]): one more source and finding under the id `ptyro`, whose
//! update is ptyro's own download, check and swap rather than the plan engine's.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use ptyro_core::config::{UpdateCheck, Updates};
use serde::{Deserialize, Serialize};

use crate::catalog::{self, Arch, Kind, Latest, Version, Via};
use crate::own;
use crate::setup::{self, Host, State, Survey};

/// How often the newest versions are looked up: at most once a day (the design's goal 8).
pub const DAY_MS: u64 = 24 * 3600 * 1000;

/// The remembered file, in the state directory.
pub const FILE: &str = "versions.json";

/// What `<state>/versions.json` remembers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Versions {
    /// When the newest versions were last looked up (ms since the epoch), whether that worked
    /// or not: a failed look waits a day too.
    pub checked: u64,
    /// The newest version of each agent, by item id, as last found (a failed lookup keeps the
    /// one before).
    pub latest: BTreeMap<String, String>,
    /// What the last lookup couldn't reach, and why (said only by `doctor`).
    pub failed: BTreeMap<String, String>,
    /// When `outdated` was last worked out (installed versions asked again).
    pub surveyed: u64,
    /// The installed agents older than their newest version, as last worked out.
    pub outdated: Vec<Outdated>,
    /// Per agent, the newest version the start-up note has said: it says each new one once.
    pub noted: BTreeMap<String, String>,
    /// Counts installs and updates started ([`started`], from the machine's setup lock): a look
    /// that began before one doesn't write over what that one found after it (its survey may
    /// predate the change).
    pub generation: u64,
}

/// An installed agent with a newer version out, and how it updates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outdated {
    pub id: String,
    pub name: String,
    /// What `ptyro update` takes (its command: `agy`).
    pub program: String,
    pub version: String,
    pub latest: String,
    /// What the update runs, as typed (`claude update`); `None`: no update command is known
    /// here (the vendor's page says how).
    #[serde(default)]
    pub command: Option<String>,
    /// Installed another way than the recommended one (npm, Homebrew): it updates that way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// The update asks for administrator rights.
    #[serde(default)]
    pub admin: bool,
    /// The vendor's page.
    #[serde(default)]
    pub doc: String,
    /// ptyro's own row (BOOT6): the binary the update replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// ptyro's own row: a package manager owns the binary (cargo, a system package), so ptyro
    /// doesn't update it; how it updates instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<String>,
}

impl Outdated {
    /// `Codex 0.170.0 is out (you have 0.162.1)`.
    pub fn line(&self) -> String {
        format!("{} {} is out (you have {})", self.name, self.latest, self.version)
    }

    /// The one-step update: `ptyro update codex`.
    pub fn fix(&self) -> String {
        format!("ptyro update {}", self.program)
    }

    /// The finding as `doctor` (and so the health badge and the web) says it: the line, the
    /// one-step update, and the command it runs.
    pub fn finding(&self) -> String {
        if let Some(path) = &self.path {
            return self.own_finding(path);
        }
        let how = match (&self.command, self.admin) {
            (Some(c), false) => format!(" (it runs `{c}`)"),
            (Some(c), true) => format!(" (it runs `{c}`, as administrator)"),
            (None, _) => format!(" (the vendor's page says how: {})", self.doc),
        };
        format!("{}: `{}` updates it{how}", self.line(), self.fix())
    }
}

impl Outdated {
    /// ptyro's own finding (BOOT6): how the binary at `path` updates.
    fn own_finding(&self, path: &str) -> String {
        let line = self.line();
        let fix = self.fix();
        match (&self.managed, &self.command, self.admin) {
            (Some(m), _, _) => format!(
                "{line}: it was installed with {m}, so update it that way (ptyro never replaces a binary a package manager owns)"
            ),
            (None, Some(c), false) => {
                format!("{line}: `{fix}` updates it (it runs `{c}`, then upgrades the daemon live)")
            }
            (None, _, true) => format!(
                "{line}: `{fix}` in a terminal on the computer updates it (it replaces {path} as administrator, then upgrades the daemon live)"
            ),
            (None, None, false) => format!(
                "{line}: `{fix}` updates it (it downloads the release, checks it against SHA256SUMS, replaces {path} and upgrades the daemon live)"
            ),
        }
    }
}

/// Milliseconds since the epoch: the clock "once a day" is measured by. A debug build's
/// `PTYRO_TEST_NOW` sets it (ms, or `@file` holding ms, so a test can move it under a running
/// daemon).
pub fn now() -> u64 {
    if let Some(v) = ptyro_core::test_knob("PTYRO_TEST_NOW") {
        let text = match v.strip_prefix('@') {
            Some(file) => std::fs::read_to_string(file).unwrap_or_default(),
            None => v,
        };
        if let Ok(ms) = text.trim().parse() {
            return ms;
        }
    }
    ptyro_core::now_ms()
}

/// The file in `state`.
pub fn path(state: &Path) -> PathBuf {
    state.join(FILE)
}

/// What is remembered (nothing yet, or a file that doesn't read: nothing).
pub fn load(state: &Path) -> Versions {
    std::fs::read_to_string(path(state))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Change the file under its lock (`versions.lock`), so the daemon's daily look, a `ptyro
/// update` and the start-up note never undo each other; written whole, then moved into place.
pub fn edit(state: &Path, change: impl FnOnce(&mut Versions)) -> Versions {
    let _ = std::fs::create_dir_all(state);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state.join("versions.lock"));
    // A filesystem that can't lock: changed unguarded, as setup's lock does.
    let held = lock.as_ref().ok().filter(|f| f.lock().is_ok());
    let mut v = load(state);
    change(&mut v);
    if let Ok(text) = serde_json::to_string_pretty(&v) {
        write_whole(state, &text);
    }
    if let Some(f) = held {
        let _ = f.unlock();
    }
    v
}

/// `text` into `versions.json` through a temporary file moved into place (tried twice: Windows'
/// indexer or a virus scanner can hold the file a moment); the temporary file never stays.
fn write_whole(state: &Path, text: &str) {
    let tmp = state.join(format!("{FILE}.{}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()));
    let moved = written.is_ok()
        && (std::fs::rename(&tmp, path(state)).is_ok() || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::rename(&tmp, path(state)).is_ok()
        });
    if !moved {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// An install or update is starting (the machine's setup lock was just taken): a look already
/// going keeps its newest versions but not its view of what's installed.
pub fn started(state: &Path) {
    edit(state, |v| v.generation += 1);
}

/// Where the count of installs stands: read before a look begins, and passed to its write.
pub fn generation(state: &Path) -> u64 {
    load(state).generation
}

/// A look by anyone who doesn't hold the machine's setup lock (the daemon's hourly one,
/// `doctor`, `ptyro update --check`, the web's "look now"): `fresh` looks the newest versions up
/// (the network), else only asks the installed agents' versions again. While an install or
/// update runs it writes nothing (what it would see may be half done) and answers with what is
/// remembered; one that starts while it looks keeps its own view ([`started`]). Also the survey
/// it saw (the agents only).
pub fn look_beside(host: &dyn Host, state: &Path, now: u64, fresh: bool) -> (Versions, Survey) {
    let since = generation(state);
    if setup::lock::Lock::held_by(state).is_some() {
        let v = load(state);
        let survey = setup::survey_agents(host, &v.latest);
        return (v, survey);
    }
    let (found, failed) = if fresh { lookup(host) } else { Default::default() };
    remember(host, state, now, since, fresh.then_some((found, failed)), false)
}

/// The newest versions are a day old (or were never looked up, or the clock went back).
pub fn due(v: &Versions, now: u64) -> bool {
    v.checked == 0 || now < v.checked || now - v.checked >= DAY_MS
}

/// What is looked up: each agent of the version table with a newest-version source (the
/// design: agents and ptyro, not git, the GitHub CLI or Node.js).
pub fn sources() -> Vec<(&'static str, &'static Latest)> {
    catalog::items()
        .iter()
        .filter(|i| i.kind == Kind::Agent)
        .filter_map(|i| Some((i.id.as_str(), i.latest.as_ref()?)))
        .collect()
}

/// Every source [`lookup`] asks: the agents', and ptyro's own releases unless this ptyro is a
/// build in a cargo `target` folder (never offered an update, so never looked up).
pub fn looked_up() -> Vec<(&'static str, &'static Latest)> {
    static OWN: std::sync::OnceLock<Latest> = std::sync::OnceLock::new();
    let mut all = sources();
    if own::Install::here().how != own::How::Build {
        all.push((own::ID, OWN.get_or_init(own::source)));
    }
    all
}

/// Look up every newest version, all at once (each at most a few seconds: a dead network costs
/// one wait, not one per agent): those found, and why each other couldn't be. A debug build's
/// `PTYRO_TEST_LATEST` names a fixture registry instead (`<dir>/<id>` holds the reply; none:
/// offline), so no test ever reaches the network.
pub fn lookup(host: &dyn Host) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let platform = host.platform();
    let registry = ptyro_core::test_knob("PTYRO_TEST_LATEST");
    let replies: Vec<(&str, Result<String, String>)> = std::thread::scope(|scope| {
        let asks: Vec<_> = looked_up()
            .into_iter()
            .map(|(id, latest)| {
                let registry = registry.as_deref();
                let handle = scope.spawn(move || {
                    let url = latest.url(platform, Arch::host());
                    let reply = match registry {
                        Some(dir) => fixture(Path::new(dir), id, &url),
                        None => host.latest_reply(&url),
                    };
                    reply.and_then(|body| latest.parse(&body).map_err(|e| e.to_string()))
                });
                (id, handle)
            })
            .collect();
        asks.into_iter()
            .map(|(id, h)| (id, h.join().unwrap_or_else(|_| Err("the lookup failed".into()))))
            .collect()
    });
    let (mut found, mut failed) = (BTreeMap::new(), BTreeMap::new());
    for (id, reply) in replies {
        match reply {
            Ok(v) => found.insert(id.to_owned(), v),
            Err(why) => failed.insert(id.to_owned(), why),
        };
    }
    (found, failed)
}

/// A fixture registry's reply for `id` (each one asked is logged to `<dir>/asked.log`).
fn fixture(dir: &Path, id: &str, url: &str) -> Result<String, String> {
    if dir.is_dir()
        && let Ok(mut f) =
            std::fs::OpenOptions::new().create(true).append(true).open(dir.join("asked.log"))
    {
        let _ = writeln!(f, "{id}");
    }
    std::fs::read_to_string(dir.join(id))
        .map_err(|_| format!("couldn't reach {url}: the test registry has no {id}"))
}

/// The installed agents older than their newest version, from a survey made with the newest
/// versions ([`setup::survey`]), each with the update the plan engine would run.
pub fn outdated(survey: &Survey) -> Vec<Outdated> {
    let platform = survey.platform;
    let has = |p: &str| survey.programs.contains(p);
    let mut out = Vec::new();
    for item in catalog::items().iter().filter(|i| i.kind == Kind::Agent) {
        let (version, latest, method, via) = match survey.states.get(&item.id) {
            Some(State::Outdated { version, latest, .. }) => {
                (version, latest, item.method(platform, has), None)
            }
            Some(State::Other { version: Some(version), via, latest: Some(latest), .. }) => {
                (version, latest, setup::other_update(item, platform, *via), Some(*via))
            }
            _ => continue,
        };
        out.push(Outdated {
            id: item.id.clone(),
            name: item.name.clone(),
            program: item.program.clone(),
            version: version.clone(),
            latest: latest.clone(),
            command: method.and_then(|m| m.update.clone().or_else(|| m.install.clone())),
            via,
            admin: method.is_some_and(|m| m.admin),
            doc: item.doc.clone(),
            path: None,
            managed: None,
        });
    }
    out
}

/// Look up the newest versions now (the network), see what is installed, and remember both.
/// For whoever holds the machine's setup lock (and the tests): a look beside an install is
/// [`look_beside`]. A failed lookup keeps the version found before.
pub fn check(host: &dyn Host, state: &Path, now: u64) -> Versions {
    let since = generation(state);
    remember(host, state, now, since, Some(lookup(host)), true).0
}

/// Remember newest versions looked up already (`found`, `failed`: [`lookup`], done before the
/// setup lock was taken, so a slow network never holds it), and see what is installed against
/// them; `None`: only see what is installed. `since`: the count of installs read before the
/// survey ([`generation`]): if an install started since, what is installed isn't written (its
/// own refresh says). `full`: every row of the table (to plan with), else the agents only.
pub fn remember(
    host: &dyn Host,
    state: &Path,
    now: u64,
    since: u64,
    looked: Option<(BTreeMap<String, String>, BTreeMap<String, String>)>,
    full: bool,
) -> (Versions, Survey) {
    let mut latest = load(state).latest;
    let failed = looked.as_ref().map(|(found, failed)| {
        latest.extend(found.clone());
        failed.clone()
    });
    let survey =
        if full { setup::survey(host, &latest) } else { setup::survey_agents(host, &latest) };
    let mut outdated = outdated(&survey);
    // ptyro itself (BOOT6): its binary's version against its newest release.
    outdated.extend(own::finding(&own::Install::here(), latest.get(own::ID).map(String::as_str)));
    let v = edit(state, |v| {
        if let Some(failed) = failed {
            v.checked = now;
            v.latest = latest;
            v.failed = failed;
        }
        if v.generation == since {
            v.surveyed = now;
            v.outdated = outdated;
        }
    });
    (v, survey)
}

/// See what is installed again against the remembered newest versions (no network): after an
/// update or an install, while still holding the setup lock, so the badge and the note stop
/// offering what was just done. Beside the installs, it is [`look_beside`] with `fresh` false.
pub fn refresh(host: &dyn Host, state: &Path, now: u64) -> Versions {
    let since = generation(state);
    if load(state).latest.is_empty() {
        return load(state);
    }
    remember(host, state, now, since, None, false).0
}

/// `updates.ignore` skips this one: its name (every version), or its name and this version.
/// Names are as `ptyro install` takes them (`codex`, `agy`, `"Claude Code"`).
pub fn ignored(ignore: &[String], o: &Outdated) -> bool {
    ignore.iter().any(|i| {
        let (name, version) = match i.split_once('@') {
            Some((n, v)) => (n, Some(v)),
            None => (i.as_str(), None),
        };
        let named = if name.trim().eq_ignore_ascii_case(own::ID) {
            o.id == own::ID
        } else {
            setup::resolve(name).is_ok_and(|item| item.id == o.id)
        };
        named
            && version.is_none_or(|v| match (Version::parse(v), Version::parse(&o.latest)) {
                (Some(a), Some(b)) => a == b,
                _ => v.trim() == o.latest,
            })
    })
}

/// What to offer: the outdated agents `updates.ignore` doesn't skip; nothing with
/// `updates.check = "off"`.
pub fn offered<'a>(v: &'a Versions, cfg: &Updates) -> Vec<&'a Outdated> {
    if cfg.check == UpdateCheck::Off {
        return Vec::new();
    }
    v.outdated.iter().filter(|o| !ignored(&cfg.ignore, o)).collect()
}

/// The newest versions to plan with (the setup checklist, `ptyro install`, the web's): the
/// remembered ones less what `updates.ignore` skips, so an outdated agent shows as such there;
/// none with `updates.check = "off"`.
pub fn known(state: &Path, cfg: &Updates) -> BTreeMap<String, String> {
    if cfg.check == UpdateCheck::Off {
        return BTreeMap::new();
    }
    let mut latest = load(state).latest;
    latest.retain(|id, l| {
        let probe = Outdated {
            id: id.clone(),
            name: String::new(),
            program: String::new(),
            version: String::new(),
            latest: l.clone(),
            command: None,
            via: None,
            admin: false,
            doc: String::new(),
            path: None,
            managed: None,
        };
        !ignored(&cfg.ignore, &probe)
    });
    latest
}

/// The start-up note (once per new version): what is offered and not said yet, now marked said.
/// Reads only the file.
pub fn note(state: &Path, cfg: &Updates) -> Vec<Outdated> {
    if cfg.check == UpdateCheck::Off || !path(state).exists() {
        return Vec::new();
    }
    let mut said = Vec::new();
    edit(state, |v| {
        let fresh: Vec<Outdated> = offered(v, cfg)
            .into_iter()
            .filter(|o| v.noted.get(&o.id) != Some(&o.latest))
            .cloned()
            .collect();
        for o in &fresh {
            v.noted.insert(o.id.clone(), o.latest.clone());
        }
        said = fresh;
    });
    said
}

/// The note in a line: `Codex 0.170.0 is out (you have 0.162.1)`, or `2 agents have newer
/// versions: Codex 0.170.0, Claude Code 2.1.400` (with ptyro among them, `2 newer versions are
/// out: …`).
pub fn note_line(said: &[Outdated]) -> Option<String> {
    let what = if said.iter().any(|o| o.id == own::ID) {
        "newer versions are out"
    } else {
        "agents have newer versions"
    };
    match said {
        [] => None,
        [one] => Some(one.line()),
        many => Some(format!(
            "{} {what}: {}",
            many.len(),
            many.iter().map(|o| format!("{} {}", o.name, o.latest)).collect::<Vec<_>>().join(", ")
        )),
    }
}
