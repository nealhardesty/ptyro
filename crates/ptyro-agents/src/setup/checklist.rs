//! The checklist's own rules (BOOT2), shared by the CLI's checklist (`ptyro setup` in a terminal)
//! and the TUI's (the first part of the onboarding screen), so the two tick alike: what each
//! side found ([`Sides`]), what is ticked there ([`Picks`]), which rows can be ticked ([`rows`]),
//! and what ticking one does ([`toggle`]). The plan itself is still [`super::plan`].

use super::{Entry, Plan, Side, State, Survey, Where, WslStatus};

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
    if here_already(e) {
        return format!("✓ {}", e.state.version().unwrap_or("installed"));
    }
    match e.action.as_ref().filter(|_| e.ticked) {
        Some(a) => {
            format!("{} {}{}", mark(e), a.via.words(), if a.admin { " (admin)" } else { "" })
        }
        None if e.ticked && e.blocked.is_some() => format!("{} can't", mark(e)),
        None => mark(e).to_owned(),
    }
}
