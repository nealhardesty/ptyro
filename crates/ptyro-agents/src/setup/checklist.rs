//! The checklist's own rules (BOOT2), shared by the CLI's checklist (`ptyro setup` in a terminal)
//! and the TUI's (the first part of the onboarding screen), so the two tick alike: which rows can
//! be ticked ([`rows`]), and what ticking one does ([`toggle`]). The plan itself is still
//! [`super::plan`]. What a row says ([`about`], [`cell`]) is here too, so the web's checklist
//! (BOOT3, through the daemon's [`lines`]) says what the TUI's does.

use serde::{Deserialize, Serialize};

use super::{Entry, Plan, State, Survey};
use crate::catalog;

/// Every agent kept the way it's installed, but `except` (BOOT7): nothing moves unless asked
/// for.
pub fn keep_all(survey: &mut Survey, except: &[String]) {
    survey.keep.extend(
        catalog::items()
            .iter()
            .filter(|i| i.kind == catalog::Kind::Agent && !except.contains(&i.id))
            .map(|i| i.id.clone()),
    );
}

/// Installed already, as wanted: nothing to tick. One installed another way and offered the move
/// to the recommended installer (BOOT7) is a row to tick instead.
pub fn here_already(e: &Entry) -> bool {
    match e.state {
        State::Installed { .. } => true,
        State::Other { .. } => e.moves.is_none(),
        _ => false,
    }
}

/// The items a person can tick, in the checklist's order: what isn't here (or isn't new
/// enough).
pub fn rows(plan: &Plan) -> Vec<&Entry> {
    plan.entries.iter().filter(|e| !here_already(e)).collect()
}

/// Here already: the checklist's "Already here".
pub fn already(plan: &Plan) -> Vec<&Entry> {
    plan.entries.iter().filter(|e| here_already(e)).collect()
}

/// Tick or untick item `id`: picked, it's unticked; else ticked when it can be. A prerequisite
/// something ticked needs stays ticked, and what is here already can't be ticked: each says so,
/// in the returned lines.
pub fn toggle(plan: &Plan, picks: &mut Vec<String>, id: &str) -> Vec<String> {
    let Some(x) = plan.entry(id) else { return Vec::new() };
    if x.picked {
        picks.retain(|p| p != id);
        Vec::new()
    } else if x.ticked {
        vec![format!("{} is needed by something ticked: untick that first", x.name)]
    } else if here_already(x) {
        vec![format!("{} is here already", x.name)]
    } else {
        picks.push(id.to_owned());
        Vec::new()
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

/// A row's cell: `[x] Homebrew`, `[+] apt (admin)`, `✓ 2.43.0`, `[ ]`.
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

/// A cell without its mark (the web's checkbox is the mark): `apt (admin)`, `can't`,
/// `✓ 2.43.0`, or nothing.
pub fn how(e: &Entry) -> String {
    if here_already(e) {
        return format!("✓ {}", e.state.version().unwrap_or("installed"));
    }
    match e.action.as_ref().filter(|_| e.ticked) {
        Some(a) if a.moving => {
            format!("move to {}{}", a.via.words(), if a.admin { " (admin)" } else { "" })
        }
        Some(a) => format!("{}{}", a.via.words(), if a.admin { " (admin)" } else { "" }),
        None if e.ticked && e.blocked.is_some() => "can't".to_owned(),
        None => e.moves.as_ref().map(|m| format!("{}: move?", m.from.words())).unwrap_or_default(),
    }
}

/// The line after a row's name: what it is, then how it installs and what comes with it
/// ("Anthropic's coding agent · the vendor's installer · then log in"). `programs`: what this
/// computer has on `PATH` (for the method an unticked row would use).
pub fn about(plan: &Plan, e: &Entry, programs: &dyn Fn(&str) -> bool) -> String {
    let Some(item) = catalog::item(&e.id) else { return String::new() };
    // A label ("for API-key accounts") says more than what
    // it is, so it takes that place.
    let mut parts: Vec<String> = vec![e.label.clone().unwrap_or_else(|| item.about.clone())];
    let platform = plan.platform;
    // Installed another way (BOOT7): the move is the offer.
    if let Some(m) = &e.moves {
        parts.push(format!("installed with {}", m.from.words()));
        if e.action.as_ref().is_some_and(|a| a.moving) {
            parts.push(format!(
                "moves to {}, then removes {}'s copy",
                m.to.words(),
                m.from.words()
            ));
            if m.admin {
                parts.push("asks for admin".into());
            }
            parts.push(m.keeps_words());
        } else {
            parts.push("move to the recommended installer?".into());
        }
        if let State::Other { latest: Some(l), .. } = &e.state {
            parts.push(format!("{l} is out"));
        }
        return parts.join(" · ");
    }
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

/// Item ids as their names: `Codex, OpenCode`.
fn names(plan: &Plan, ids: &[String]) -> String {
    let by: Vec<&str> =
        ids.iter().map(|id| plan.entry(id).map_or(id.as_str(), |x| x.name.as_str())).collect();
    by.join(", ")
}

/// What the checklist says of one entry, for a client that shows the checklist without the
/// catalog: the web's (BOOT3, protocol 1.34), so it says what the TUI's does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    /// The line after the name ([`about`]).
    pub about: String,
    /// The cell without its mark ([`how`]).
    pub how: String,
    /// Its state in words (`installed 2.1.3`, `not installed`).
    pub state: String,
}

/// Every entry's [`Line`], in the plan's order; `survey` is this computer's (for the method an
/// unticked row would use).
pub fn lines(plan: &Plan, survey: &Survey) -> Vec<Line> {
    let programs = |p: &str| survey.programs.contains(p);
    plan.entries
        .iter()
        .map(|e| Line {
            id: e.id.clone(),
            about: about(plan, e, &programs),
            how: how(e),
            state: e.state.words(),
        })
        .collect()
}
