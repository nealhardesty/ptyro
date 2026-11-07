//! Logins (ROADMAP BOOT4, SPEC §24.4): one account per agent and the GitHub CLI, through each
//! tool's own flow, confirmed by the tool's own status command.
//!
//! ptyro never reads, parses or stores a credential. A login runs in the person's terminal (or a
//! session, for the web), where its browser page or device code shows; whether it worked is
//! decided by the exit code of the status command the table names ([`crate::catalog::Login`]),
//! run with its output thrown away, so not even an account name passes through ptyro. A tool the
//! vendor documents no status command for is [`LoginState::Unconfirmed`]: it signs in when it
//! starts.

use serde::{Deserialize, Serialize};

use super::host::Host;
use super::{Plan, Report, Side};
use crate::catalog::{self, Item};

/// Where an item's login stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    /// Its status command exited 0.
    LoggedIn,
    /// Its status command exited otherwise (or couldn't run).
    LoggedOut,
    /// The vendor documents no status command: it asks when it starts, if it needs to.
    Unconfirmed,
    /// The item isn't installed.
    Missing,
}

/// One item's login, for a list (`ptyro login --check --json`, the web).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub side: Side,
    pub state: LoginState,
    /// What logging in runs, as typed (`gh auth login --web`).
    pub command: String,
    /// What confirms it (`gh auth status`); `None`: nothing documented.
    #[serde(default)]
    pub check: Option<String>,
    /// The table's note: why it can't be confirmed, or what the flow asks.
    #[serde(default)]
    pub note: Option<String>,
}

/// Where `item`'s login stands on `host`.
pub fn status(host: &dyn Host, item: &Item) -> LoginState {
    let Some(login) = &item.login else { return LoginState::Unconfirmed };
    if host.installed(item).is_none() {
        return LoginState::Missing;
    }
    match &login.check {
        None => LoginState::Unconfirmed,
        Some(check) => match host.status(check) {
            Some(true) => LoginState::LoggedIn,
            _ => LoginState::LoggedOut,
        },
    }
}

/// The logins of `ids` (those with a login; every one with an empty list).
pub fn rows(host: &dyn Host, ids: &[String]) -> Vec<LoginRow> {
    let side = if host.platform().is_windows() { Side::Windows } else { Side::Local };
    catalog::items()
        .iter()
        .filter(|i| ids.is_empty() || ids.contains(&i.id))
        .filter_map(|i| {
            let l = i.login.as_ref()?;
            Some(LoginRow {
                id: i.id.clone(),
                name: i.name.clone(),
                side,
                state: status(host, i),
                command: l.start.join(" "),
                check: l.check.as_ref().map(|c| c.join(" ")),
                note: l.note.clone(),
            })
        })
        .collect()
}

/// How a login ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Its status afterwards.
    pub state: LoginState,
    /// How it ended when it didn't exit 0 (`it exited with 1`, `it was stopped`): a failure,
    /// Ctrl+C, or an agent quit that way.
    pub ended: Option<String>,
}

/// Run `item`'s own login in the person's terminal, then ask its status command. `Err`: it
/// couldn't start.
pub fn log_in(host: &dyn Host, item: &Item) -> Result<Outcome, String> {
    let login = item.login.as_ref().ok_or("it has no login")?;
    let ended = host.interact(&login.start)?;
    Ok(Outcome { state: status(host, item), ended })
}

/// The logins to walk through after a run: what it installed, and what was picked and is here
/// already, that has a login, in the plan's order.
pub fn after(plan: &Plan, report: &Report) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in &plan.entries {
        let here = report.done.contains(&e.id) || (e.picked && e.state.present());
        if here && catalog::item(&e.id).is_some_and(|i| i.login.is_some()) && !out.contains(&e.id) {
            out.push(e.id.clone());
        }
    }
    out
}

/// What setup ends with: how to add more accounts (the design's goal 5).
pub const PROFILES_HINT: &str = "More accounts (work and personal, say) are profiles, each with its own logins: `ptyro profile new work`, then `ptyro profile login work claude`.";
