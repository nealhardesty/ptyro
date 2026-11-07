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
        /// An update the program's file couldn't be replaced for, because it is running
        /// (protocol 1.35, BOOT5): a client offers to try again once its sessions end.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        in_use: bool,
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
    /// After the installs, before the report: the items to log in next (BOOT4: what was
    /// installed, or picked and here already, that has a login). The caller walks through them
    /// ([`super::login`]), since a login needs the person's terminal.
    Logins {
        ids: Vec<String>,
    },
    Finished {
        report: Report,
    },
}

/// An event and the side it happened on (BOOT8): what `ptyro setup --json` prints on Windows,
/// where a run can have two sides. A reader that knows only [`Event`] still reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sided {
    pub side: super::Side,
    #[serde(flatten)]
    pub event: Event,
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
    /// Another side's report added to this one (for the exit code and the summary).
    pub fn merge(&mut self, other: &Report) {
        self.done.extend(other.done.iter().cloned());
        self.failed.extend(other.failed.iter().cloned());
        self.skipped.extend(other.skipped.iter().cloned());
        self.blocked.extend(other.blocked.iter().cloned());
        self.tools |= other.tools;
    }

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
        // The recommended methods first; an update of one installed another way is that
        // installer's own (BOOT5).
        let Some((item, method)) = catalog::item(&e.id).and_then(|i| {
            let mut all = i
                .methods(plan.platform)
                .chain(i.others.iter().filter(|m| m.on(plan.platform) && action.update));
            Some((i, all.find(|m| m.via == action.via)?))
        }) else {
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
                let busy = action.update && in_use(&error);
                let fix = failure_fix(e, method.via, &error, plan.platform);
                on(Event::Failed { id: e.id.clone(), error, fix, in_use: busy });
                report.failed.push(e.id.clone());
            }
        }
    }
    let logins = super::login::after(plan, &report);
    if !logins.is_empty() {
        on(Event::Logins { ids: logins });
    }
    report.tools = host.tools_used();
    on(Event::Finished { report: report.clone() });
    report
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

/// The failure is Windows refusing to replace a program that is running (an agent updated while
/// a session runs it): what its error says, in the words Windows, npm and the installers use.
pub fn in_use(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "being used by another process",
        "ebusy",
        "resource busy or locked",
        "text file busy",
        "os error 32",
        "sharing violation",
    ]
    .iter()
    .any(|w| e.contains(w))
}

fn failure_fix(e: &Entry, via: Via, error: &str, platform: crate::catalog::Platform) -> String {
    let cmd = e.action.as_ref().map(|a| a.command.as_str()).unwrap_or_default();
    let update = e.action.as_ref().is_some_and(|a| a.update);
    if update && in_use(error) {
        let why = if platform.is_windows() {
            "Windows won't replace a program while it runs"
        } else {
            "its file couldn't be replaced while it runs"
        };
        return format!(
            "{} is running, and {why}: end the sessions running it (or quit it wherever it runs), then try the update again (`ptyro update {}`)",
            e.name,
            program(e)
        );
    }
    match via {
        _ if update => format!(
            "`ptyro update {}` tries again; or run it yourself to see why: `{cmd}`; the vendor's page: {}",
            program(e),
            e.doc
        ),
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
