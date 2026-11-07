//! Running a plan: each step in order with its output as it comes, ✓ or ✗ with the fix, and a
//! failure never stopping the rest (only what needs the failed item). Administrator rights are
//! asked for once, before the first step that needs them, and only where the caller allows.

use serde::{Deserialize, Serialize};

use super::host::{Elevation, Host, Step};
use super::{Entry, Plan};
use crate::catalog::{self, Via};

/// Where administrator rights may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admin {
    /// A person at the terminal: sudo asks once for the password (Windows: each installer's
    /// UAC prompt). `ptyro setup`, `ptyro install`.
    Ask,
    /// Only without a prompt (root, sudo's remembered password, an elevated terminal): the
    /// unattended `--yes`. A step that would prompt is reported with its command.
    Silent,
    /// Never: the daemon, for the web. A password never goes through the web (the owner's parity
    /// exception, SPEC §24.2); the step is reported with its command, to run on the machine.
    Never,
}

/// What a run reports as it goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Administrator rights are about to be asked for, once, for these steps.
    Elevating {
        ids: Vec<String>,
    },
    Started {
        id: String,
        name: String,
        command: String,
        admin: bool,
    },
    /// A line of its output.
    Line {
        id: String,
        text: String,
    },
    Done {
        id: String,
        /// The version found afterwards; `None` when it can't be found yet (a new terminal may
        /// be needed).
        #[serde(default)]
        version: Option<String>,
    },
    Failed {
        id: String,
        error: String,
        fix: String,
    },
    /// Not run: what it needs failed, or it needs administrator rights that can't be had here.
    Skipped {
        id: String,
        reason: String,
        fix: String,
    },
    /// Ticked, but nothing installs it here (no package manager its methods need): said first.
    Blocked {
        id: String,
        reason: String,
        fix: String,
    },
    /// After the installs, before the report: the logins (BOOT4). Nothing yet.
    Logins {
        ids: Vec<String>,
    },
    Finished {
        report: Report,
    },
}

/// How a run ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub done: Vec<String>,
    pub failed: Vec<String>,
    pub skipped: Vec<String>,
    /// Ticked but impossible here ([`super::Entry::blocked`]).
    #[serde(default)]
    pub blocked: Vec<String>,
    /// Something went into ptyro's tools folders: new terminals need them on `PATH`
    /// (`ptyro shell-init`'s line; on Windows the user's `PATH`).
    #[serde(default)]
    pub tools: bool,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && self.skipped.is_empty() && self.blocked.is_empty()
    }
}

/// Run `plan`'s steps on `host`, each event to `on`.
pub fn run(plan: &Plan, host: &dyn Host, admin: Admin, on: &mut dyn FnMut(Event)) -> Report {
    let steps = plan.steps();
    let mut report = Report::default();
    let admin_ids: Vec<String> = plan.admin().iter().map(|e| e.id.clone()).collect();
    // Can admin steps run, and if not, why.
    let elevated: Result<(), String> = if admin_ids.is_empty() {
        Ok(())
    } else {
        match (admin, host.elevation()) {
            (Admin::Never, _) => Err(
                "it needs administrator rights, which ptyro never asks for over the network".into(),
            ),
            (_, Elevation::Ready) => Ok(()),
            (_, Elevation::Unavailable) => {
                Err("it needs administrator rights, and there's no sudo here".into())
            }
            (Admin::Silent, Elevation::Prompt) => Err(
                "it needs administrator rights, which would ask for a password (--yes asks nothing)"
                    .into(),
            ),
            (Admin::Ask, Elevation::Prompt) => {
                on(Event::Elevating { ids: admin_ids.clone() });
                if host.elevate() {
                    Ok(())
                } else {
                    Err("administrator rights weren't given".into())
                }
            }
        }
    };
    for e in plan.entries.iter().filter(|e| e.ticked) {
        let Some(why) = &e.blocked else { continue };
        on(Event::Blocked {
            id: e.id.clone(),
            reason: why.clone(),
            fix: format!(
                "install what it needs (or it, from {}), then `ptyro install {}`",
                e.doc,
                program(e)
            ),
        });
        report.blocked.push(e.id.clone());
    }
    for e in steps {
        let Some(action) = &e.action else { continue };
        let not_there: Vec<&String> = action
            .after
            .iter()
            .filter(|d| {
                report.failed.contains(d)
                    || report.skipped.contains(d)
                    || report.blocked.contains(d)
            })
            .collect();
        if let Some(d) = not_there.first() {
            let dn = plan.entry(d).map_or(d.as_str(), |x| x.name.as_str());
            on(Event::Skipped {
                id: e.id.clone(),
                reason: format!("it needs {dn}, which didn't install"),
                fix: format!("once {dn} is installed: `ptyro install {}`", program(e)),
            });
            report.skipped.push(e.id.clone());
            continue;
        }
        if action.admin
            && let Err(why) = &elevated
        {
            on(Event::Skipped { id: e.id.clone(), reason: why.clone(), fix: admin_fix(e, admin) });
            report.skipped.push(e.id.clone());
            continue;
        }
        let Some((item, method)) = catalog::item(&e.id)
            .and_then(|i| Some((i, i.methods(plan.platform).find(|m| m.via == action.via)?)))
        else {
            continue;
        };
        on(Event::Started {
            id: e.id.clone(),
            name: e.name.clone(),
            command: action.command.clone(),
            admin: action.admin,
        });
        let step = Step { item, method, update: action.update };
        let id = e.id.clone();
        let result = host.run(&step, &mut |text| {
            on(Event::Line { id: id.clone(), text: text.to_owned() });
        });
        match result {
            Ok(()) => {
                let version = host.installed(item).and_then(|f| f.version);
                on(Event::Done { id: e.id.clone(), version });
                report.done.push(e.id.clone());
            }
            Err(error) => {
                on(Event::Failed { id: e.id.clone(), error, fix: failure_fix(e, method.via) });
                report.failed.push(e.id.clone());
            }
        }
    }
    login_step(&report.done, on);
    report.tools = host.tools_used();
    on(Event::Finished { report: report.clone() });
    report
}

/// BOOT4 walks through the logins here (each tool's own flow, in a session); for now it says
/// which installed items would be next, and does nothing.
fn login_step(done: &[String], on: &mut dyn FnMut(Event)) {
    let ids: Vec<String> = done
        .iter()
        .filter(|id| catalog::item(id).is_some_and(|i| i.login.is_some()))
        .cloned()
        .collect();
    if !ids.is_empty() {
        on(Event::Logins { ids });
    }
}

/// The name `ptyro install` takes for it (its command: `agy`, `gh`).
fn program(e: &Entry) -> String {
    catalog::item(&e.id).map_or_else(|| e.id.clone(), |i| i.program.clone())
}

fn admin_fix(e: &Entry, admin: Admin) -> String {
    let cmd = e.action.as_ref().map(|a| a.command.as_str()).unwrap_or_default();
    match admin {
        Admin::Never => format!(
            "on the computer itself: `ptyro install {}` (it asks for the password there), or run `{cmd}`",
            program(e)
        ),
        _ => format!("run `{cmd}` in a terminal, or `ptyro install {}` without --yes", program(e)),
    }
}

fn failure_fix(e: &Entry, via: Via) -> String {
    let cmd = e.action.as_ref().map(|a| a.command.as_str()).unwrap_or_default();
    match via {
        Via::NodeDist => format!(
            "check the connection to nodejs.org, then `ptyro install node` again; or install Node.js from {}",
            e.doc
        ),
        _ => format!(
            "`ptyro install {}` tries again; or run it yourself to see why: `{cmd}`; the vendor's page: {}",
            program(e),
            e.doc
        ),
    }
}
