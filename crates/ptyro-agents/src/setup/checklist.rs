//! The checklist's own rules (BOOT2), shared by the CLI's checklist (`ptyro setup` in a terminal)
//! and the TUI's (the first part of the onboarding screen), so the two tick alike: what each
//! side found ([`Sides`]), what is ticked there ([`Picks`]), which rows can be ticked ([`rows`]),
//! and what ticking one does ([`toggle`]). The plan itself is still [`super::plan`]. What a row
//! says ([`about`], [`why`], [`cell`]) is here too, so the web's checklist (BOOT3, through the
//! daemon's [`lines`]) says what the TUI's does.

use serde::{Deserialize, Serialize};

use super::{Entry, Plan, Side, State, Survey, Where, WslStatus};
use crate::catalog;

/// What each side has ticked (BOOT8): `here` is this computer (Windows itself, on Windows),
/// `wsl` the WSL distro's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picks {
    pub here: Vec<String>,
    pub wsl: Vec<String>,
}

impl Picks {
    pub fn of(&mut self, side: Side) -> &mut Vec<String> {
        if side == Side::Wsl { &mut self.wsl } else { &mut self.here }
    }

    pub fn on(&self, side: Side) -> &[String] {
        if side == Side::Wsl { &self.wsl } else { &self.here }
    }
}

/// What setup found: this computer, and on Windows with a usable WSL, the distro.
#[derive(Debug, Clone)]
pub struct Sides {
    pub here: Survey,
    /// This computer's side: `Windows` on Windows, else `Local`.
    pub side: Side,
    /// WSL's survey, when WSL is usable (and wanted).
    pub wsl: Option<Survey>,
    /// WSL as found (Windows), or that this is WSL; `None` elsewhere.
    pub status: Option<WslStatus>,
}

impl Sides {
    pub fn distro(&self) -> Option<&str> {
        self.status.as_ref().and_then(WslStatus::usable).filter(|_| self.wsl.is_some())
    }

    /// The sides `place` means here, those that can be had.
    pub fn sides(&self, place: Where) -> Vec<Side> {
        place
            .sides(self.side)
            .into_iter()
            .filter(|s| *s != Side::Wsl || self.wsl.is_some())
            .collect()
    }

    pub fn plan(&self, picks: &Picks) -> Plan {
        let mine = super::plan(&self.here, &picks.here);
        let wsl = self.wsl.as_ref().map(|s| super::plan(s, &picks.wsl));
        Plan::merge(mine, wsl, self.status.clone())
    }

    /// The default ticks not installed yet, on `place`'s sides.
    pub fn defaults(&self, place: Where) -> Picks {
        let mut p = Picks::default();
        for side in self.sides(place) {
            let survey = if side == Side::Wsl { self.wsl.as_ref() } else { Some(&self.here) };
            if let Some(s) = survey {
                *p.of(side) = super::defaults(s);
            }
        }
        p
    }

    /// `ids` ticked on `place`'s sides.
    pub fn picks(&self, place: Where, ids: &[String]) -> Picks {
        let mut p = Picks::default();
        for side in self.sides(place) {
            *p.of(side) = ids.to_vec();
        }
        p
    }

    /// `Windows`, `WSL (Ubuntu)`, `this computer`.
    pub fn name(&self, side: Side) -> String {
        match (side, self.distro()) {
            (Side::Wsl, Some(d)) => format!("WSL ({d})"),
            (s, _) => s.words().to_owned(),
        }
    }
}

/// Installed already, as wanted: nothing to tick.
pub fn here_already(e: &Entry) -> bool {
    matches!(e.state, State::Installed { .. } | State::Other { .. })
}

/// The items a person can tick, in the checklist's order: what isn't here (or isn't new
/// enough) on some side. One entry per item (the first side's).
pub fn rows(plan: &Plan) -> Vec<&Entry> {
    let sides = plan.sides();
    let Some(first) = sides.first() else { return Vec::new() };
    plan.entries
        .iter()
        .filter(|e| e.side == *first)
        .filter(|e| {
            sides.iter().any(|s| plan.entry_on(&e.id, *s).is_some_and(|x| !here_already(x)))
        })
        .collect()
}

/// Here already on every side: the checklist's "Already here".
pub fn already(plan: &Plan) -> Vec<&Entry> {
    let sides = plan.sides();
    let Some(first) = sides.first() else { return Vec::new() };
    plan.entries
        .iter()
        .filter(|e| e.side == *first)
        .filter(|e| sides.iter().all(|s| plan.entry_on(&e.id, *s).is_none_or(here_already)))
        .collect()
}

/// Tick or untick item `id` on `place`'s sides: picked there, it's unticked; else ticked where
/// it can be. A prerequisite something ticked needs stays ticked, and what is here already can't
/// be ticked: each says so, in the returned lines.
pub fn toggle(
    sides: &Sides,
    plan: &Plan,
    picks: &mut Picks,
    id: &str,
    place: Where,
) -> Vec<String> {
    let two = sides.wsl.is_some();
    let on = |s: Side| if two { format!(" on {}", sides.name(s)) } else { String::new() };
    let here: Vec<&Entry> =
        sides.sides(place).iter().filter_map(|s| plan.entry_on(id, *s)).collect();
    let mut said = Vec::new();
    if here.iter().any(|x| x.picked) {
        for x in &here {
            picks.of(x.side).retain(|p| p != id);
        }
        return said;
    }
    for x in here {
        if x.ticked {
            said.push(format!(
                "{} is needed by something ticked{}: untick that first",
                x.name,
                on(x.side)
            ));
        } else if here_already(x) {
            said.push(format!("{} is here already{}", x.name, on(x.side)));
        } else {
            picks.of(x.side).push(id.to_owned());
        }
    }
    said
}

/// The default side on Windows: the daemon's distro (`prefer`, from `client.default_remote`)
/// when it is the usable one, else Windows. Another distro with ptyro isn't where the sessions
/// run, so the default stays Windows, with a line saying why.
pub fn default_place(prefer: Option<&str>, status: Option<&WslStatus>) -> (Where, Option<String>) {
    match (prefer, status.and_then(WslStatus::usable)) {
        (Some(p), Some(d)) if p.is_empty() || p.eq_ignore_ascii_case(d) => (Where::Wsl, None),
        (Some(p), _) => (Where::Windows, Some(daemon_elsewhere(p, status))),
        (None, _) => (Where::Windows, None),
    }
}

/// Why the default isn't the daemon's distro `prefer` (`client.default_remote`).
fn daemon_elsewhere(prefer: &str, status: Option<&WslStatus>) -> String {
    match status {
        Some(WslStatus::Usable { distro, .. }) => format!(
            "WSL: your daemon is in {prefer}, which setup can't reach; {distro} has ptyro, so `--where wsl` installs there. By default this sets up Windows only."
        ),
        Some(s) => s.words(),
        None => String::new(),
    }
}

/// `[x]` picked, `[+]` ticked because something picked needs it, `[ ]` not.
pub fn mark(e: &Entry) -> &'static str {
    if e.picked {
        "[x]"
    } else if e.ticked {
        "[+]"
    } else {
        "[ ]"
    }
}

/// One side's cell: `[x] WinGet`, `[+] apt (admin)`, `✓ 2.43.0`, `[ ]`.
pub fn cell(e: Option<&Entry>) -> String {
    let Some(e) = e else { return String::new() };
    let how = how(e);
    if here_already(e) {
        how
    } else if how.is_empty() {
        mark(e).to_owned()
    } else {
        format!("{} {how}", mark(e))
    }
}

/// A cell without its mark (the web's checkbox is the mark): `WinGet (admin)`, `can't`,
/// `✓ 2.43.0`, or nothing.
pub fn how(e: &Entry) -> String {
    if here_already(e) {
        return format!("✓ {}", e.state.version().unwrap_or("installed"));
    }
    match e.action.as_ref().filter(|_| e.ticked) {
        Some(a) => format!("{}{}", a.via.words(), if a.admin { " (admin)" } else { "" }),
        None if e.ticked && e.blocked.is_some() => "can't".to_owned(),
        None => String::new(),
    }
}

/// The line after a row's name: what it is, then how it installs and what comes with it
/// ("Anthropic's coding agent · the vendor's installer · then log in"). With two sides the
/// method is in each side's cell, so it's left out here. `programs`: what the row's side has on
/// `PATH` (for the method an unticked row would use).
pub fn about(plan: &Plan, e: &Entry, programs: &dyn Fn(&str) -> bool, two: bool) -> String {
    let Some(item) = catalog::item(&e.id) else { return String::new() };
    // A label (Gemini CLI's "for Gemini API-key and enterprise accounts…") says more than what
    // it is, so it takes that place.
    let mut parts: Vec<String> = vec![e.label.clone().unwrap_or_else(|| item.about.clone())];
    let platform = plan.platform;
    if !two {
        match (&e.action, &e.blocked) {
            (Some(a), _) => {
                let how = if a.update { "updates through " } else { "" };
                parts.push(format!("{how}{}", a.via.words()));
                if a.admin {
                    parts.push("asks for admin".into());
                }
            }
            (None, Some(b)) => parts.push(format!("can't here: {b}")),
            (None, None) => {
                if let Some(m) = item.method(platform, programs) {
                    parts.push(m.via.words().to_owned());
                    if m.admin {
                        parts.push("asks for admin".into());
                    }
                }
            }
        }
    }
    let needs: Vec<&str> = item
        .prereqs(platform)
        .filter(|p| p.required)
        .filter_map(|p| catalog::item(&p.item).map(|i| i.name.as_str()))
        .collect();
    if !needs.is_empty() {
        parts.push(format!("needs {}", needs.join(", ")));
    }
    if !e.needed_by.is_empty() {
        parts.push(format!("ticked because {} needs it", names(plan, &e.needed_by)));
    }
    if matches!(e.state, State::TooOld { .. } | State::Outdated { .. }) {
        parts.push(e.state.words());
    }
    if item.login.is_some() {
        parts.push("then log in".into());
    }
    parts.join(" · ")
}

/// What a side's cell can't say, on a two-sided checklist: why it can't, what needs it.
pub fn why(plan: &Plan, e: &Entry) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(b) = &e.blocked {
        out.push(format!("can't: {b}"));
    }
    if !e.needed_by.is_empty() {
        out.push(format!("ticked because {} needs it", names(plan, &e.needed_by)));
    }
    out
}

/// Item ids as their names: `Codex, Gemini CLI`.
fn names(plan: &Plan, ids: &[String]) -> String {
    let by: Vec<&str> =
        ids.iter().map(|id| plan.entry(id).map_or(id.as_str(), |x| x.name.as_str())).collect();
    by.join(", ")
}

/// What the checklist says of one entry (one row on one side), for a client that shows the
/// checklist without the catalog: the web's (BOOT3, protocol 1.34), so it says what the TUI's
/// does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    pub side: Side,
    /// The line after the name ([`about`]; without the method when there are two sides).
    pub about: String,
    /// The side's cell without its mark ([`how`]).
    pub how: String,
    /// What the cell can't say ([`why`]).
    #[serde(default)]
    pub why: Vec<String>,
    /// Its state in words (`installed 2.1.3`, `not installed`).
    pub state: String,
}

/// Every entry's [`Line`], in the plan's order; `here` and `wsl` are the sides' surveys (for the
/// method an unticked row would use).
pub fn lines(plan: &Plan, here: &Survey, wsl: Option<&Survey>) -> Vec<Line> {
    let two = plan.sides().len() > 1;
    plan.entries
        .iter()
        .map(|e| {
            let survey = if e.side == Side::Wsl { wsl.unwrap_or(here) } else { here };
            let programs = |p: &str| survey.programs.contains(p);
            Line {
                id: e.id.clone(),
                side: e.side,
                about: about(plan, e, &programs, two),
                how: how(e),
                why: why(plan, e),
                state: e.state.words(),
            }
        })
        .collect()
}
