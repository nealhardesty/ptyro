use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use super::*;
use crate::catalog::Via;

/// An in-process machine: what's installed, what's on `PATH`, and each step's fate.
#[derive(Default)]
struct TestHost {
    platform: Option<Platform>,
    programs: BTreeSet<String>,
    installed: Mutex<BTreeMap<String, String>>,
    other: BTreeMap<String, Via>,
    candidates: BTreeMap<String, String>,
    elevation: Option<Elevation>,
    elevate: bool,
    fail: BTreeSet<String>,
    calls: Mutex<Vec<String>>,
    /// Logged in (BOOT4): a status command for these exits 0.
    logged_in: Mutex<BTreeSet<String>>,
    /// Logins that fail (exit 1, still logged out).
    login_fails: BTreeSet<String>,
    /// Newest-version replies by item id (BOOT5); none: offline for it.
    replies: BTreeMap<String, String>,
    /// Each newest-version source asked, by item id.
    asked: Mutex<Vec<String>>,
    /// A step's error, by item id (instead of the default failure).
    errors: BTreeMap<String, String>,
    /// Copies installed the recommended way beside the `other` one (BOOT7's moves), by id.
    new: Mutex<BTreeMap<String, String>>,
    /// `other` copies removed.
    removed: Mutex<BTreeSet<String>>,
    /// Removals that fail.
    uninstall_fails: BTreeSet<String>,
    /// A third installer's copy, by id (after the new one).
    third: BTreeMap<String, Via>,
    /// A copy nothing classifies (Nix's store), by id (last).
    unknown: BTreeSet<String>,
    /// A second copy in the vendor's place that prints no version, after the old one (BOOT7).
    vendor_mute: BTreeSet<String>,
    /// A removal takes the new copy with it (a shared bin folder).
    uninstall_takes_new: bool,
    /// Every install after the first fails.
    fail_after_first: bool,
}

impl TestHost {
    fn on(platform: Platform) -> Self {
        Self { platform: Some(platform), elevate: true, ..Self::default() }
    }
    fn with(mut self, programs: &[&str]) -> Self {
        self.programs.extend(programs.iter().map(|p| (*p).to_owned()));
        self
    }
    fn have(self, id: &str, v: &str) -> Self {
        self.installed.lock().unwrap().insert(id.into(), v.into());
        self
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Host for TestHost {
    fn platform(&self) -> Platform {
        self.platform.unwrap()
    }
    fn has(&self, p: &str) -> bool {
        self.programs.contains(p)
    }
    fn installed(&self, item: &Item) -> Option<Installed> {
        self.copies(item).into_iter().next()
    }
    fn copies(&self, item: &Item) -> Vec<Installed> {
        let mut out = Vec::new();
        if let Some(v) = self.installed.lock().unwrap().get(&item.id).cloned()
            && !self.removed.lock().unwrap().contains(&item.id)
        {
            let via = self.other.get(&item.id).copied();
            let prefix = (via == Some(Via::Npm)).then(|| "/home/me/.npm-global".to_owned());
            let path = Some("/home/me/.npm-global/bin/x".to_owned());
            let file = path.clone();
            let real = path.clone();
            out.push(Installed {
                version: Some(v),
                path,
                via,
                system: false,
                prefix,
                store: None,
                file,
                real,
            });
        }
        if self.vendor_mute.contains(&item.id) {
            let m = item.method(self.platform(), |_| true).unwrap();
            let place = m.places.iter().find_map(|p| expand_place(p)).unwrap_or_default();
            let file = Some(format!("{place}/old/{}", item.program));
            out.push(Installed {
                path: file.clone(),
                real: file.clone(),
                file,
                ..Default::default()
            });
        }
        if let Some(v) = self.new.lock().unwrap().get(&item.id).cloned() {
            // Where the vendor's installer puts it (the table's `places`).
            let m = item.method(self.platform(), |_| true).unwrap();
            let place = m.places.iter().find_map(|p| expand_place(p)).unwrap_or_default();
            let file = Some(format!("{place}/{}", item.program));
            let path = Some("~/.local/bin/x".into());
            out.push(Installed {
                version: Some(v),
                path,
                via: None,
                system: false,
                prefix: None,
                store: None,
                real: file.clone(),
                file,
            });
        }
        if let Some(t) = self.third.get(&item.id) {
            let path = Some("/opt/homebrew/bin/x".into());
            let v = Some("0.1.0".to_owned());
            let (file, real) = (path.clone(), path.clone());
            out.push(Installed {
                version: v,
                path,
                via: Some(*t),
                system: false,
                prefix: None,
                store: None,
                file,
                real,
            });
        }
        if self.unknown.contains(&item.id) {
            let path = Some("/nix/store/abc-codex/bin/codex".into());
            let v = Some("0.1.0".to_owned());
            let (file, real) = (path.clone(), path.clone());
            out.push(Installed {
                version: v,
                path,
                via: None,
                system: false,
                prefix: None,
                store: None,
                file,
                real,
            });
        }
        out
    }
    fn uninstall(
        &self,
        item: &Item,
        method: &Method,
        command: &str,
        line: &mut dyn FnMut(&str),
    ) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("uninstall {} {}", item.id, method.via.name()));
        line(command);
        if self.uninstall_fails.contains(&item.id) {
            return Err("it exited with 1: EACCES".into());
        }
        self.removed.lock().unwrap().insert(item.id.clone());
        if self.uninstall_takes_new {
            self.new.lock().unwrap().remove(&item.id);
        }
        Ok(())
    }
    fn candidate(&self, via: Via) -> Option<String> {
        self.candidates.get(via.name()).cloned()
    }
    fn elevation(&self) -> Elevation {
        self.elevation.unwrap_or(Elevation::Ready)
    }
    fn elevate(&self) -> bool {
        self.calls.lock().unwrap().push("elevate".into());
        self.elevate
    }
    fn run(&self, step: &Step<'_>, line: &mut dyn FnMut(&str)) -> Result<(), String> {
        let id = &step.item.id;
        self.calls.lock().unwrap().push(format!("{id} {}", step.method.via.name()));
        line(&format!("installing {id}"));
        let again = self
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == &format!("{id} {}", step.method.via.name()))
            .count()
            > 1;
        if self.fail.contains(id) || (self.fail_after_first && again) {
            return Err(self
                .errors
                .get(id)
                .cloned()
                .unwrap_or_else(|| "it exited with 1: E: no network".into()));
        }
        if self.other.get(id).is_some_and(|v| *v != step.method.via) {
            self.new.lock().unwrap().insert(id.clone(), "26.13.0".into());
        } else {
            self.installed.lock().unwrap().insert(id.clone(), "26.13.0".into());
        }
        Ok(())
    }
    fn tools_used(&self) -> bool {
        self.calls().iter().any(|c| c.ends_with("node-dist"))
    }
    fn status(&self, argv: &[String]) -> Option<bool> {
        self.calls.lock().unwrap().push(format!("status {}", argv.join(" ")));
        let item = catalog::items()
            .iter()
            .find(|i| i.login.as_ref().and_then(|l| l.check.as_ref()).is_some_and(|c| c == argv))?;
        Some(self.logged_in.lock().unwrap().contains(&item.id))
    }
    fn interact(&self, argv: &[String]) -> Result<Option<String>, String> {
        self.calls.lock().unwrap().push(format!("login {}", argv.join(" ")));
        let item = catalog::items()
            .iter()
            .find(|i| i.login.as_ref().is_some_and(|l| l.start == argv))
            .ok_or("no such login")?;
        if self.login_fails.contains(&item.id) {
            return Ok(Some("it exited with 1".into()));
        }
        self.logged_in.lock().unwrap().insert(item.id.clone());
        Ok(None)
    }
    fn latest_reply(&self, url: &str) -> Result<String, String> {
        let item = catalog::items()
            .iter()
            .find(|i| {
                i.latest
                    .as_ref()
                    .is_some_and(|l| l.url(self.platform(), catalog::Arch::host()) == url)
            })
            .ok_or("no such source")?;
        self.asked.lock().unwrap().push(item.id.clone());
        self.replies.get(&item.id).cloned().ok_or_else(|| "offline".to_owned())
    }
}

fn picks(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_owned()).collect()
}

fn run_all(plan: &Plan, host: &TestHost, admin: Admin) -> (Report, Vec<Event>) {
    let mut events = Vec::new();
    let r = run(plan, host, admin, &mut |e| events.push(e));
    (r, events)
}

#[test]
fn a_fresh_machine_gets_the_defaults_in_order() {
    let host = TestHost::on(Platform::Linux).with(&["apt-get", "sudo", "curl"]);
    let s = survey(&host, &BTreeMap::new());
    assert!(s.states.values().all(|st| *st == State::Missing));
    assert_eq!(defaults(&s), ["claude", "codex", "git", "gh"]);
    let p = plan(&s, &defaults(&s));
    let steps: Vec<&str> = p.steps().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(steps, ["claude", "codex", "git", "gh"]);
    // Codex needs no Node.js (BOOT0): nothing else is ticked.
    assert!(!p.entry("node").unwrap().ticked);
    assert_eq!(
        p.admin().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["git", "gh"],
        "apt needs sudo"
    );
    let claude = p.entry("claude").unwrap().action.clone().unwrap();
    assert_eq!(claude.command, "curl -fsSL https://claude.ai/install.sh | bash");
    assert!(!claude.admin);
}

#[test]
fn installed_items_are_not_ticked_and_need_nothing() {
    let host = TestHost::on(Platform::Linux).with(&["apt-get"]).have("git", "2.43.0");
    let s = survey(&host, &BTreeMap::new());
    assert_eq!(defaults(&s), ["claude", "codex", "gh"]);
    let p = plan(&s, &picks(&["git"]));
    let git = p.entry("git").unwrap();
    assert!(git.ticked && git.action.is_none() && git.blocked.is_none());
    assert_eq!(git.state.version(), Some("2.43.0"));
    assert!(p.steps().is_empty());
}

#[test]
fn node_ticked_on_its_own_comes_from_apt() {
    let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    h.candidates.insert("apt".into(), "18.19.1".into());
    // Nothing names a minimum: apt's, whatever it has.
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["node"]));
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::Apt);
}

#[test]
fn no_way_here_says_why() {
    // A Linux with neither apt nor dnf: git has no other method.
    let h = TestHost::on(Platform::Linux);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["git"]));
    let git = p.entry("git").unwrap();
    assert!(git.action.is_none());
    assert!(git.blocked.as_deref().unwrap().contains("`apt-get`"), "{:?}", git.blocked);
    assert!(p.steps().is_empty());
}

#[test]
fn states_other_and_outdated() {
    let mut h = TestHost::on(Platform::Linux).have("claude", "2.1.300").have("codex", "0.160.0");
    h.other.insert("claude".into(), Via::Npm);
    let latest: BTreeMap<String, String> = [("codex".into(), "0.167.0".into())].into();
    let s = survey(&h, &latest);
    assert!(matches!(s.states["claude"], State::Other { via: Via::Npm, .. }));
    assert!(matches!(&s.states["codex"], State::Outdated { latest, .. } if latest == "0.167.0"));
    let p = plan(&s, &picks(&["claude", "codex"]));
    // BOOT7: ticked, it moves to the recommended installer.
    let claude = p.entry("claude").unwrap();
    let a = claude.action.clone().expect("the move");
    assert!(a.moving && !a.update && a.via == Via::Vendor, "{a:?}");
    assert_eq!(
        a.command,
        "curl -fsSL https://claude.ai/install.sh | bash\nnpm uninstall -g --prefix '/home/me/.npm-global' @anthropic-ai/claude-code"
    );
    let codex = p.entry("codex").unwrap().action.clone().unwrap();
    assert!(codex.update);
    assert_eq!(codex.command, "curl -fsSL https://chatgpt.com/codex/install.sh | sh");
}

#[test]
fn a_run_reports_each_step_and_a_failure_leaves_the_rest() {
    let mut h = TestHost::on(Platform::Linux);
    h.fail.insert("node".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["node", "claude"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["claude"]);
    assert_eq!(r.failed, ["node"]);
    assert!(!r.ok());
    assert!(events.iter().any(
        |e| matches!(e, Event::Line { id, text } if id == "claude" && text == "installing claude")
    ));
    let fix = events.iter().find_map(|e| match e {
        Event::Failed { id, fix, .. } if id == "node" => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("ptyro install node"));
    assert!(matches!(events.last(), Some(Event::Finished { .. })));
    // Claude was installed and has a login: the logins' turn (BOOT4) names it.
    assert!(events.iter().any(|e| matches!(e, Event::Logins { ids } if ids == &["claude"])));
    assert_eq!(h.calls(), ["claude vendor", "node node-dist"]);
}

#[test]
fn admin_is_asked_once_and_only_where_allowed() {
    let plan_on =
        |h: &TestHost| plan(&survey(h, &BTreeMap::new()), &picks(&["git", "gh", "claude"]));
    // A terminal: asked once for both.
    let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    h.elevation = Some(Elevation::Prompt);
    let (r, events) = run_all(&plan_on(&h), &h, Admin::Ask);
    assert_eq!(h.calls().iter().filter(|c| *c == "elevate").count(), 1);
    assert_eq!(h.calls()[0], "elevate", "before the first step");
    assert!(matches!(&events[0], Event::Elevating { ids } if ids == &["git", "gh"]));
    assert!(r.ok());
    // Refused: those two are skipped with their command, the rest installs.
    let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    h.elevation = Some(Elevation::Prompt);
    h.elevate = false;
    let (r, _) = run_all(&plan_on(&h), &h, Admin::Ask);
    assert_eq!((r.done.as_slice(), r.skipped.len()), (&["claude".to_owned()][..], 2));
    // --yes: never prompts; reported with the command.
    let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    h.elevation = Some(Elevation::Prompt);
    let (r, events) = run_all(&plan_on(&h), &h, Admin::Silent);
    assert!(!h.calls().contains(&"elevate".to_owned()));
    assert_eq!(r.skipped, ["git", "gh"]);
    let fix = events.iter().find_map(|e| match e {
        Event::Skipped { id, fix, .. } if id == "git" => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("sudo apt-get install -y git"));
    // --yes where no prompt is needed (root, cached sudo): it runs.
    let h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    let (r, _) = run_all(&plan_on(&h), &h, Admin::Silent);
    assert!(r.ok());
    // The daemon: never, even when it could.
    let h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
    let (r, events) = run_all(&plan_on(&h), &h, Admin::Never);
    assert_eq!(r.skipped, ["git", "gh"]);
    let fix = events.iter().find_map(|e| match e {
        Event::Skipped { id, fix, .. } if id == "gh" => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("on the computer itself: `ptyro install gh`"));
}

#[test]
fn nodejs_orgs_build_marks_the_tools_folder_used() {
    let h = TestHost::on(Platform::Linux);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["node"]));
    // No apt, no dnf: nodejs.org's build.
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::NodeDist);
    let (r, _) = run_all(&p, &h, Admin::Ask);
    assert!(r.ok() && r.tools);
    assert_eq!(r.done, ["node"]);
}

#[test]
fn names_resolve_as_people_say_them() {
    let id = |n: &str| resolve(n).map(|i| i.id.as_str());
    assert_eq!(id("opencode"), Ok("opencode"));
    assert_eq!(id("OpenCode"), Ok("opencode"));
    assert!(id("agy").is_err(), "no longer one setup installs (2026-11-17)");
    assert_eq!(id("github-cli"), Ok("gh"));
    assert_eq!(id("Claude Code"), Ok("claude"));
    assert_eq!(id("nodejs"), Ok("node"));
    assert_eq!(id("Node.js"), Ok("node"));
    let e = resolve("vim").unwrap_err();
    assert!(e.contains("opencode") && e.contains("gh"), "{e}");
}

#[test]
fn package_versions_read() {
    let apt =
        "nodejs:\n  Installed: (none)\n  Candidate: 18.19.1+dfsg-6ubuntu5\n  Version table:\n";
    assert_eq!(host::package_version(apt, "Candidate").as_deref(), Some("18.19.1"));
    let epoch = "  Candidate: 1:22.11.0-1nodesource1\n";
    assert_eq!(host::package_version(epoch, "Candidate").as_deref(), Some("22.11.0"));
    assert_eq!(host::package_version("  Candidate: (none)\n", "Candidate"), None);
    let dnf =
        "Name         : nodejs\nEpoch        : 1\nVersion      : 22.11.0\nRelease      : 1.fc41\n";
    assert_eq!(host::package_version(dnf, "Version").as_deref(), Some("22.11.0"));
}

#[test]
fn the_plan_travels_as_json() {
    let h = TestHost::on(Platform::Linux).with(&["apt-get"]).have("git", "2.43.0");
    let p = plan(&survey(&h, &BTreeMap::new()), &defaults(&survey(&h, &BTreeMap::new())));
    let j = serde_json::to_value(&p).unwrap();
    let git = j["entries"].as_array().unwrap().iter().find(|e| e["id"] == "git").unwrap();
    assert_eq!(git["state"], "installed");
    assert_eq!(git["version"], "2.43.0");
    let back: Plan = serde_json::from_value(j).unwrap();
    assert_eq!(back, p);
    let e = Event::Line { id: "gh".into(), text: "x".into() };
    assert_eq!(serde_json::to_value(&e).unwrap()["event"], "line");
}

#[test]
fn path_dirs_go_first_once() {
    let paths = ptyro_core::paths::Paths::resolve(|k| {
        (k == "PTYRO_HOME").then(|| std::env::temp_dir().join("ptyro-setup-test"))
    })
    .unwrap();
    let dirs = path_dirs(&paths);
    assert_eq!(dirs.len(), 2);
    assert!(dirs.iter().all(|d| d.starts_with(paths.tools())));
    let base =
        std::env::join_paths([std::path::PathBuf::from("/usr/bin"), dirs[0].clone()]).unwrap();
    let joined = path_with(&dirs, &base);
    let parts: Vec<_> = std::env::split_paths(&joined).collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[..2], dirs[..]);
}

#[test]
fn a_blocked_item_is_reported_and_counts_as_not_installed() {
    // Linux without apt or dnf: git and gh have no way here; Claude and Codex install.
    let h = TestHost::on(Platform::Linux);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["claude", "git", "gh"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["claude"]);
    assert_eq!(r.blocked, ["git", "gh"]);
    assert!(!r.ok(), "something asked for didn't install");
    let (reason, fix) = events
        .iter()
        .find_map(|e| match e {
            Event::Blocked { id, reason, fix } if id == "git" => {
                Some((reason.clone(), fix.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert!(reason.contains("`apt-get` or `dnf`"), "{reason}");
    assert!(fix.contains("`ptyro install git`"), "{fix}");
    // A blocked item never runs.
    assert!(h.calls().iter().all(|c| !c.starts_with("git") && !c.starts_with("gh")));
}

/// An installer that leaves a background process holding its output doesn't hang the step.
#[cfg(unix)]
#[test]
fn a_step_ends_when_its_program_does_even_with_a_child_left_holding_the_pipe() {
    let start = std::time::Instant::now();
    let mut lines = Vec::new();
    let argv: Vec<String> =
        ["sh", "-c", "sleep 30 & echo installed"].iter().map(|s| (*s).to_owned()).collect();
    let r = host::stream(&argv, &[], &mut |l| lines.push(l.to_owned()));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(lines, ["installed"]);
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "{:?}", start.elapsed());
    let argv: Vec<String> =
        ["sh", "-c", "sleep 30 & exit 3"].iter().map(|s| (*s).to_owned()).collect();
    assert!(host::stream(&argv, &[], &mut |_| {}).unwrap_err().contains("exited with 3"));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_login_is_decided_by_its_status_commands_exit_code_alone() {
    let item = |id: &str| catalog::item(id).unwrap();
    let h = TestHost::on(Platform::Linux).have("gh", "2.57.0").have("opencode", "1.19.5");
    assert_eq!(login::status(&h, item("gh")), login::LoginState::LoggedOut);
    assert_eq!(login::status(&h, item("claude")), login::LoginState::Missing);
    // No status command documented: it can't be confirmed, and nothing is run to find out.
    assert_eq!(login::status(&h, item("opencode")), login::LoginState::Unconfirmed);
    assert_eq!(h.calls(), ["status gh auth status"]);
    h.logged_in.lock().unwrap().insert("gh".into());
    assert_eq!(login::status(&h, item("gh")), login::LoginState::LoggedIn);

    // A list of them: the agents and gh, with what each runs and what confirms it.
    let rows = login::rows(&h, &[]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "opencode", "gh"]);
    let gh = rows.iter().find(|r| r.id == "gh").unwrap();
    assert_eq!(gh.command, "gh auth login --web");
    assert_eq!(gh.check.as_deref(), Some("gh auth status"));
    let only = login::rows(&h, &picks(&["gh", "git"]));
    assert_eq!(only.len(), 1, "git has no login");
}

#[test]
fn logging_in_runs_the_tools_own_flow_then_asks_its_status() {
    let item = |id: &str| catalog::item(id).unwrap();
    let mut h = TestHost::on(Platform::Linux)
        .have("codex", "0.162.1")
        .have("gh", "2.57.0")
        .have("opencode", "1.19.5");
    h.login_fails.insert("gh".into());
    h.login_fails.insert("opencode".into());
    let done = login::log_in(&h, item("codex")).unwrap();
    assert_eq!((done.state, done.ended), (login::LoginState::LoggedIn, None));
    let gh = login::log_in(&h, item("gh")).unwrap();
    assert_eq!(gh.state, login::LoginState::LoggedOut);
    assert_eq!(gh.ended.as_deref(), Some("it exited with 1"));
    // An agent with no status command, quit with Ctrl+C: how it ended, nothing more known.
    let oc = login::log_in(&h, item("opencode")).unwrap();
    assert_eq!(oc.state, login::LoginState::Unconfirmed);
    assert!(oc.ended.is_some());
    assert_eq!(
        h.calls(),
        [
            "login codex login",
            "status codex login status",
            "login gh auth login --web",
            "status gh auth status",
            "login opencode auth login",
        ]
    );
}

#[test]
fn the_logins_after_a_run_are_what_it_installed_and_what_was_picked() {
    let h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]).have("gh", "2.57.0");
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["claude", "git", "gh"]));
    let (report, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(report.done, ["claude", "git"]);
    // git has no login; gh was picked and is here already.
    assert_eq!(login::after(&p, &report), ["claude", "gh"]);
    assert!(events.iter().any(|e| matches!(e, Event::Logins { ids } if ids == &["claude", "gh"])));
}

/// The web's lines (BOOT3): each entry's words as the TUI says them, the method in the line.
#[test]
fn the_checklists_lines_say_what_the_tui_does() {
    use super::checklist::{cell, lines};
    let linux =
        TestHost::on(Platform::Linux).with(&["apt-get", "sudo", "curl"]).have("claude", "2.1.3");
    let here = survey(&linux, &BTreeMap::new());
    let one = plan(&here, &ids(&["opencode", "git"]));
    let ls = lines(&one, &here);
    assert_eq!(ls.len(), one.entries.len());
    let of =
        |ls: &[super::checklist::Line], id: &str| ls.iter().find(|l| l.id == id).unwrap().clone();
    let opencode = of(&ls, "opencode");
    assert!(opencode.about.ends_with("then log in"), "{opencode:?}");
    // Unticked: the method it would use, from what is on PATH.
    assert!(of(&ls, "gh").about.contains("apt · asks for admin"), "{:?}", of(&ls, "gh"));
    assert_eq!(of(&ls, "gh").how, "");
    let claude = of(&ls, "claude");
    assert_eq!((claude.how.as_str(), claude.state.as_str()), ("✓ 2.1.3", "installed 2.1.3"));
    assert_eq!(of(&ls, "git").how, "apt (admin)");
    assert_eq!(cell(one.entry("git")), "[x] apt (admin)");
    // It travels as JSON and reads back.
    let back: Vec<super::checklist::Line> =
        serde_json::from_value(serde_json::to_value(&ls).unwrap()).unwrap();
    assert_eq!(back, ls);
}

// ── BOOT5: out of date, offered (`crate::updates`) ──────────────────────────────────────────

fn updates_cfg(check: &str, ignore: &[&str]) -> ptyro_core::config::Updates {
    ptyro_core::config::Updates {
        check: if check == "off" {
            ptyro_core::config::UpdateCheck::Off
        } else {
            ptyro_core::config::UpdateCheck::Daily
        },
        ignore: ignore.iter().map(|s| (*s).to_owned()).collect(),
        ..ptyro_core::config::Updates::default()
    }
}

#[test]
fn a_newer_version_is_found_remembered_and_offered_with_its_update() {
    use crate::updates;
    let dir = tempfile::tempdir().unwrap();
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300").have("codex", "0.170.0");
    host.replies.insert("claude".into(), "2.1.400\n".into());
    host.replies.insert("codex".into(), r#"{"tag_name":"rust-v0.170.0"}"#.into());
    let v = updates::check(&host, dir.path(), 1_000);
    assert_eq!(v.checked, 1_000);
    assert_eq!(v.latest["claude"], "2.1.400");
    // The other (OpenCode) couldn't be reached: kept, quietly.
    assert!(v.failed.contains_key("opencode"), "{:?}", v.failed);
    assert_eq!(v.outdated.len(), 1, "Codex is the newest already: {:?}", v.outdated);
    let o = &v.outdated[0];
    assert_eq!(
        (o.id.as_str(), o.version.as_str(), o.latest.as_str()),
        ("claude", "2.1.300", "2.1.400")
    );
    assert_eq!(o.command.as_deref(), Some("claude update"));
    assert_eq!(
        o.finding(),
        "Claude Code 2.1.400 is out (you have 2.1.300): `ptyro update claude` updates it (it runs `claude update`)"
    );
    // Remembered: the file holds it.
    assert_eq!(updates::load(dir.path()), v);
    // At most once a day: not due until a day has passed, and due if the clock went back.
    assert!(!updates::due(&v, 1_000 + updates::DAY_MS - 1));
    assert!(updates::due(&v, 1_000 + updates::DAY_MS));
    assert!(updates::due(&v, 999));
    assert!(updates::due(&updates::Versions::default(), 5));
    // Offline the next day: the time moves on, the versions found before stay.
    host.replies.clear();
    let v2 = updates::check(&host, dir.path(), 1_000 + updates::DAY_MS);
    assert_eq!(v2.latest["claude"], "2.1.400");
    assert_eq!(v2.outdated, v.outdated);
    assert!(v2.failed.contains_key("claude"));
    assert_eq!(v2.checked, 1_000 + updates::DAY_MS);
}

#[test]
fn an_agent_installed_another_way_updates_that_way() {
    use crate::updates;
    let mut host = TestHost::on(Platform::Linux).with(&["npm"]).have("codex", "0.162.1");
    host.other.insert("codex".into(), Via::Npm);
    let latest = BTreeMap::from([("codex".to_owned(), "0.170.0".to_owned())]);
    let s = survey(&host, &latest);
    assert!(
        matches!(&s.states["codex"], State::Other { via: Via::Npm, latest: Some(l), .. } if l == "0.170.0"),
        "{:?}",
        s.states["codex"]
    );
    let out = updates::outdated(&s);
    assert_eq!(out[0].command.as_deref(), Some("npm install -g @openai/codex@latest"));
    assert_eq!(out[0].via, Some(Via::Npm));
    // Kept the way it is (`updates.keep_installer`, BOOT7): npm's own update.
    let mut kept = s.clone();
    kept.keep.insert("codex".into());
    let p = plan(&kept, &picks(&["codex"]));
    let a = p.entry("codex").unwrap().action.clone().expect("an update");
    assert!(a.update && a.via == Via::Npm && !a.moving, "{a:?}");
    assert!(p.entry("codex").unwrap().moves.is_none(), "kept: no move offered");
    let (r, _) = run_all(&p, &host, Admin::Ask);
    assert_eq!(r.done, ["codex"]);
    assert_eq!(host.calls(), ["codex npm"]);
    // Not outdated and kept: still "other", with nothing to do.
    let mut s = survey(&host, &BTreeMap::new());
    assert!(matches!(&s.states["codex"], State::Other { latest: None, .. }));
    s.keep.insert("codex".into());
    assert!(plan(&s, &picks(&["codex"])).entry("codex").unwrap().action.is_none());
}

#[test]
fn updates_ignore_skips_an_agent_or_one_version_and_off_offers_nothing() {
    use crate::updates;
    let dir = tempfile::tempdir().unwrap();
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300").have("codex", "0.162.1");
    host.replies.insert("claude".into(), "2.1.400".into());
    host.replies.insert("codex".into(), r#"{"tag_name":"rust-v0.170.0"}"#.into());
    let v = updates::check(&host, dir.path(), 1);
    let ids = |cfg: &ptyro_core::config::Updates| -> Vec<String> {
        updates::offered(&v, cfg).iter().map(|o| o.id.clone()).collect()
    };
    assert_eq!(ids(&updates_cfg("daily", &[])), ["claude", "codex"]);
    assert_eq!(ids(&updates_cfg("daily", &["codex"])), ["claude"]);
    assert_eq!(ids(&updates_cfg("daily", &["codex@0.170.0"])), ["claude"]);
    // Another version of it is offered again.
    assert_eq!(ids(&updates_cfg("daily", &["codex@0.169.0"])), ["claude", "codex"]);
    // Names as people say them.
    assert_eq!(ids(&updates_cfg("daily", &["Claude Code", "CODEX"])), Vec::<String>::new());
    assert_eq!(ids(&updates_cfg("off", &[])), Vec::<String>::new());
    // What checklists plan with: the remembered versions less the skipped; none when off.
    let known = updates::known(dir.path(), &updates_cfg("daily", &["codex@0.170.0"]));
    assert_eq!(known.keys().collect::<Vec<_>>(), ["claude"]);
    assert!(updates::known(dir.path(), &updates_cfg("off", &[])).is_empty());
}

#[test]
fn the_start_up_note_says_each_new_version_once() {
    use crate::updates;
    let dir = tempfile::tempdir().unwrap();
    let cfg = updates_cfg("daily", &[]);
    // Nothing remembered yet: nothing said, and nothing written.
    assert!(updates::note(dir.path(), &cfg).is_empty());
    assert!(!updates::path(dir.path()).exists());
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300");
    host.replies.insert("claude".into(), "2.1.400".into());
    updates::check(&host, dir.path(), 1);
    let said = updates::note(dir.path(), &cfg);
    assert_eq!(
        updates::note_line(&said).as_deref(),
        Some("Claude Code 2.1.400 is out (you have 2.1.300)")
    );
    // Said once: not again at the next start.
    assert!(updates::note(dir.path(), &cfg).is_empty());
    // A newer one still is a new version: said again, once.
    host.replies.insert("claude".into(), "2.1.401".into());
    updates::check(&host, dir.path(), 2);
    assert_eq!(updates::note(dir.path(), &cfg).len(), 1);
    assert!(updates::note(dir.path(), &cfg).is_empty());
    // Updated meanwhile: refreshed without the network, nothing is offered.
    host.installed.lock().unwrap().insert("claude".into(), "2.1.401".into());
    let asked = host.asked.lock().unwrap().len();
    let v = updates::refresh(&host, dir.path(), 3);
    assert!(v.outdated.is_empty());
    assert_eq!(host.asked.lock().unwrap().len(), asked, "refresh asks no source");
    // Off: nothing said.
    host.installed.lock().unwrap().insert("claude".into(), "2.1.300".into());
    updates::refresh(&host, dir.path(), 4);
    assert!(updates::note(dir.path(), &updates_cfg("off", &[])).is_empty());
}

#[test]
fn a_program_running_while_its_update_replaces_it_says_why_and_how_to_retry() {
    // A running program's file is busy (ETXTBSY).
    let latest = BTreeMap::from([("claude".to_owned(), "2.1.400".to_owned())]);
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300");
    host.fail.insert("claude".into());
    host.errors.insert("claude".into(), "it exited with 1: Text file busy".into());
    let p = plan(&survey(&host, &latest), &picks(&["claude"]));
    let (r, events) = run_all(&p, &host, Admin::Ask);
    assert_eq!(r.failed, ["claude"]);
    let fix = events
        .iter()
        .find_map(|e| match e {
            Event::Failed { fix, .. } => Some(fix.clone()),
            _ => None,
        })
        .unwrap();
    assert!(fix.contains("its file couldn't be replaced while it runs"), "{fix}");
    assert!(fix.contains("then try the update again (`ptyro update claude`)"), "{fix}");
    assert!(events.iter().any(|e| matches!(e, Event::Failed { in_use: true, .. })));
    assert!(in_use("npm ERR! code EBUSY"));
    assert!(!in_use("it exited with 1: E: no network"));
}

#[test]
fn a_look_that_began_before_an_update_never_writes_the_old_version_back() {
    use crate::updates;
    let dir = tempfile::tempdir().unwrap();
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300");
    host.replies.insert("claude".into(), "2.1.400".into());
    updates::check(&host, dir.path(), 1);
    assert_eq!(updates::load(dir.path()).outdated.len(), 1);
    // A look beside the installs begins: it counts the installs, and sees 2.1.300.
    let since = updates::generation(dir.path());
    // Meanwhile an update takes the lock (counted), updates, and refreshes under it.
    updates::started(dir.path());
    host.installed.lock().unwrap().insert("claude".into(), "2.1.400".into());
    updates::refresh(&host, dir.path(), 2);
    assert!(updates::load(dir.path()).outdated.is_empty());
    // The look lands late, with what it saw before: dropped by the count; its newest versions
    // are still kept.
    host.installed.lock().unwrap().insert("claude".into(), "2.1.300".into());
    host.replies.insert("claude".into(), "2.1.401".into());
    let looked = updates::lookup(&host);
    let (v, _) = updates::remember(&host, dir.path(), 3, since, Some(looked), false);
    assert!(v.outdated.is_empty(), "{:?}", v.outdated);
    assert_eq!(v.surveyed, 2);
    assert_eq!(v.latest["claude"], "2.1.401");
    assert_eq!(v.checked, 3);
    // A look with the count as it is now writes what it saw.
    let since = updates::generation(dir.path());
    let (v, _) = updates::remember(&host, dir.path(), 4, since, None, false);
    assert_eq!(v.outdated.len(), 1);
}

#[test]
fn an_agent_installed_another_way_without_an_update_command_is_not_offered_one() {
    // `outdated` and `plan` choose the same method (`setup::other_update`): an `others` row
    // with no `update` gives neither a command nor a step.
    let item = catalog::item("codex").unwrap();
    for m in item.others.iter() {
        assert!(m.update.is_some(), "every row today has one: {:?}", m.via);
    }
    assert!(other_update(item, Platform::Linux, Via::Npm).is_some());
    assert!(other_update(item, Platform::Linux, Via::Apt).is_none());
}

#[test]
fn the_refresh_asks_only_the_agents() {
    let host = TestHost::on(Platform::Linux).have("claude", "2.1.300").have("git", "2.43.0");
    let s = survey_agents(&host, &BTreeMap::new());
    assert!(s.states.contains_key("claude"));
    assert!(!s.states.contains_key("git"), "no tool asked");
    assert!(s.candidates.is_empty());
}

// ── BOOT7: moves to the recommended installer ──────────────────────────────────────────────

fn npm_codex() -> TestHost {
    let mut h = TestHost::on(Platform::Linux).with(&["npm"]).have("codex", "0.162.1");
    h.other.insert("codex".into(), Via::Npm);
    h
}

#[test]
fn an_agent_installed_another_way_is_offered_the_move_and_ticking_it_moves() {
    let h = npm_codex();
    let s = survey(&h, &BTreeMap::new());
    let p = plan(&s, &[]);
    let e = p.entry("codex").unwrap();
    let mv = e.moves.clone().expect("offered");
    assert_eq!((mv.from, mv.to), (Via::Npm, Via::Vendor));
    assert_eq!(mv.uninstall, "npm uninstall -g --prefix '/home/me/.npm-global' @openai/codex");
    assert!(!mv.admin);
    assert_eq!(mv.keeps, ["~/.codex"]);
    assert!(e.action.is_none(), "not ticked: nothing runs");
    // A row of the checklist (not "already here"), with the offer in its words.
    assert!(!checklist::here_already(e));
    assert!(checklist::rows(&p).iter().any(|r| r.id == "codex"));
    let line = checklist::about(&p, e, &|_| true);
    assert!(line.contains("installed with npm · move to the recommended installer?"), "{line}");
    // Ticked: installed the vendor's way, checked, then npm's copy removed.
    let p = plan(&s, &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["codex"], "{events:?}");
    assert_eq!(h.calls(), ["codex vendor", "uninstall codex npm"]);
    let starts: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Started { command, .. } => Some(command.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [
            "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
            "npm uninstall -g --prefix '/home/me/.npm-global' @openai/codex"
        ]
    );
    // Now the recommended one: installed, nothing offered.
    let s = survey(&h, &BTreeMap::new());
    assert!(matches!(s.states["codex"], State::Installed { .. }), "{:?}", s.states["codex"]);
    assert!(plan(&s, &[]).entry("codex").unwrap().moves.is_none());
}

#[test]
fn a_new_install_that_fails_leaves_the_old_one_as_it_was() {
    let mut h = npm_codex();
    h.fail.insert("codex".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.failed, ["codex"]);
    assert_eq!(h.calls(), ["codex vendor"], "nothing removed");
    let fix = events.iter().find_map(|e| match e {
        Event::Failed { fix, .. } => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("nothing was removed: Codex through npm is still installed"));
    let s = survey(&h, &BTreeMap::new());
    assert!(
        matches!(&s.states["codex"], State::Other { via: Via::Npm, version: Some(v), .. } if v == "0.162.1")
    );
}

#[test]
fn a_removal_that_fails_says_the_command_that_finishes_it_and_a_retry_only_removes() {
    let mut h = npm_codex();
    h.uninstall_fails.insert("codex".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.failed, ["codex"]);
    let (error, fix) = events
        .iter()
        .find_map(|e| match e {
            Event::Failed { error, fix, .. } => Some((error.clone(), fix.clone())),
            _ => None,
        })
        .unwrap();
    assert!(
        error.starts_with("removing npm's copy")
            && error.ends_with("through the vendor's installer works (~/.local/bin/x)"),
        "{error}"
    );
    assert!(
        fix.contains("`npm uninstall -g --prefix '/home/me/.npm-global' @openai/codex`"),
        "{fix}"
    );
    // Tried again once it can be removed: the new copy is here, so only the removal runs.
    h.uninstall_fails.clear();
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, _) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["codex"]);
    assert_eq!(h.calls(), ["codex vendor", "uninstall codex npm", "uninstall codex npm"]);
}

#[test]
fn a_kept_agent_is_not_offered_the_move_and_a_system_copy_needs_admin() {
    let h = npm_codex();
    let mut s = survey(&h, &BTreeMap::new());
    s.keep.insert("codex".into());
    let p = plan(&s, &picks(&["codex"]));
    let e = p.entry("codex").unwrap();
    assert!(e.moves.is_none() && e.action.is_none());
    assert!(checklist::here_already(e));
    // In an administrator's folder (a system npm prefix): removing it is sudo's, and the whole
    // move is an administrator's step (TUI and CLI only; the web shows the command).
    let item = catalog::item("codex").unwrap();
    let st = State::Other {
        version: Some("0.162.1".into()),
        via: Via::Npm,
        path: None,
        latest: None,
        system: true,
        prefix: Some("/usr".into()),
        store: None,
    };
    let mv = offer(item, Platform::Linux, &st, &BTreeSet::new(), &|_| true).unwrap();
    assert!(mv.admin);
    assert_eq!(mv.uninstall, "sudo npm uninstall -g --prefix '/usr' @openai/codex");
    let mut s = survey(&h, &BTreeMap::new());
    s.states.insert("codex".into(), st);
    let p = plan(&s, &picks(&["codex"]));
    assert!(p.entry("codex").unwrap().action.as_ref().unwrap().admin);
    let (r, _) = run_all(&p, &h, Admin::Never);
    assert_eq!(r.skipped, ["codex"], "never over the network");
    // The findings: offered unless kept.
    let v = crate::updates::Versions {
        movable: crate::updates::movable(&survey(&h, &BTreeMap::new())),
        ..Default::default()
    };
    assert_eq!(v.movable.len(), 1);
    assert!(
        v.movable[0].finding().contains("`ptyro install codex` moves it to the vendor's installer")
    );
    let mut cfg = updates_cfg("daily", &[]);
    assert!(!v.movable[0].kept(&cfg));
    cfg.keep_installer = vec!["Codex".into()];
    assert!(v.movable[0].kept(&cfg));
}

#[test]
fn an_outdated_agent_installed_another_way_offers_both_until_kept() {
    let mut h = npm_codex();
    h.replies.insert("codex".into(), r#"{"tag_name":"rust-v0.170.0"}"#.into());
    let dir = tempfile::tempdir().unwrap();
    let v = crate::updates::check(&h, dir.path(), 1);
    let o = &crate::updates::offered(&v, &updates_cfg("daily", &[]))[0];
    assert_eq!(o.move_to, Some(Via::Vendor));
    assert!(
        o.finding()
            .contains("; or `ptyro install codex` moves it from npm to the vendor's installer"),
        "{}",
        o.finding()
    );
    assert_eq!(
        crate::updates::note_line(std::slice::from_ref(o)).unwrap(),
        "Codex 0.170.0 is out (you have 0.162.1) (installed with npm; the move to the vendor's installer is offered too)"
    );
    let mut cfg = updates_cfg("daily", &[]);
    cfg.keep_installer = vec!["codex".into()];
    let o = &crate::updates::offered(&v, &cfg)[0];
    assert_eq!(o.move_to, None);
    assert!(!o.finding().contains("moves it"));
}

#[test]
fn a_homebrew_keg_is_homebrews_even_with_node_modules_inside_and_npm_has_its_prefix() {
    use super::host::{installed_via, npm_prefix};
    use std::path::Path;
    let keg = Path::new(
        "/opt/homebrew/Cellar/some-cli/0.33.0/libexec/lib/node_modules/@acme/some-cli/dist/index.js",
    );
    assert_eq!(installed_via(keg), Some(Via::Brew));
    let cask = Path::new("/opt/homebrew/Caskroom/codex/0.162.1/codex");
    assert_eq!(installed_via(cask), Some(Via::Brew));
    // npm with Homebrew's Node.js: npm's.
    let npm = Path::new("/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js");
    assert_eq!(installed_via(npm), Some(Via::Npm));
    assert_eq!(npm_prefix(npm).as_deref(), Some("/opt/homebrew"));
    let nvm = Path::new(
        "/home/me/.nvm/versions/node/v22.1.0/lib/node_modules/@openai/codex/bin/codex.js",
    );
    assert_eq!(npm_prefix(nvm).as_deref(), Some("/home/me/.nvm/versions/node/v22.1.0"));
    // Bun's global folder isn't npm's to remove from.
    let bun = Path::new("/home/me/.bun/install/global/node_modules/@openai/codex/bin/codex.js");
    assert_eq!(npm_prefix(bun), None);
}

#[test]
fn no_move_is_offered_without_a_prefix_or_for_someone_elses_homebrew() {
    let item = catalog::item("codex").unwrap();
    let st = |via, system, prefix: Option<&str>| State::Other {
        version: None,
        via,
        path: None,
        latest: None,
        system,
        prefix: prefix.map(str::to_owned),
        store: None,
    };
    let none = BTreeSet::new();
    assert!(offer(item, Platform::Linux, &st(Via::Npm, false, None), &none, &|_| true).is_none());
    assert!(offer(item, Platform::Macos, &st(Via::Brew, true, None), &none, &|_| true).is_none());
    // A backslash is taken as it is inside single quotes.
    let back =
        offer(item, Platform::Linux, &st(Via::Npm, false, Some(r"/t/npm\prefix")), &none, &|_| {
            true
        })
        .unwrap();
    assert_eq!(back.uninstall, r"npm uninstall -g --prefix '/t/npm\prefix' @openai/codex");
    let mine = offer(item, Platform::Macos, &st(Via::Brew, false, None), &none, &|_| true).unwrap();
    assert_eq!(mine.uninstall, "brew uninstall --cask codex");
}

#[test]
fn a_third_installers_copy_never_counts_as_the_new_one() {
    let mut h = npm_codex();
    // A copy from Homebrew answering already: not the vendor's, so the vendor's still installs.
    h.new.lock().unwrap().clear();
    h.third.insert("codex".into(), Via::Brew);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["codex"], "{events:?}");
    assert_eq!(h.calls(), ["codex vendor", "uninstall codex npm"]);
}

#[test]
fn a_removal_that_takes_the_new_copy_too_installs_it_again_or_says_none_runs() {
    let mut h = npm_codex();
    h.uninstall_takes_new = true;
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["codex"], "{events:?}");
    assert_eq!(h.calls(), ["codex vendor", "uninstall codex npm", "codex vendor"], "put back");
    // Put back fails: never Done with nothing to run.
    let mut h = npm_codex();
    h.uninstall_takes_new = true;
    h.fail_after_first = true;
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.failed, ["codex"]);
    let error = events
        .iter()
        .find_map(|e| match e {
            Event::Failed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap();
    assert!(error.contains("no Codex runs now"), "{error}");
}

#[test]
fn an_unknown_installers_copy_never_counts_and_an_unknown_vendor_place_offers_no_move() {
    // Nix's codex beside npm's: no package manager's, but not in the vendor's places either.
    let mut h = npm_codex();
    h.unknown.insert("codex".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["codex"], "{events:?}");
    assert_eq!(
        h.calls(),
        ["codex vendor", "uninstall codex npm"],
        "the vendor's installer ran first"
    );
    // A vendor installer whose place isn't known: no move, and why.
    let mut item = catalog::item("codex").unwrap().clone();
    for m in &mut item.methods {
        m.places.clear();
    }
    let st = State::Other {
        version: None,
        via: Via::Npm,
        path: None,
        latest: None,
        system: false,
        prefix: Some("/home/me/.npm-global".into()),
        store: None,
    };
    assert!(offer(&item, Platform::Linux, &st, &BTreeSet::new(), &|_| true).is_none());
    let why = not_offered(&item, Platform::Linux, &st, &|_| true).unwrap();
    assert!(why.contains("isn't known"), "{why}");
    // A copy is the vendor's only by its place.
    let m = catalog::item("codex").unwrap().method(Platform::Linux, |_| true).unwrap();
    let at =
        |f: &str| Installed { file: Some(f.into()), real: Some(f.into()), ..Default::default() };
    let home = expand_place("~").unwrap();
    assert!(installed_by(&at(&format!("{home}/.codex/packages/standalone/current/codex")), m));
    assert!(!installed_by(&at("/nix/store/abc-codex/bin/codex"), m));
    assert!(
        !installed_by(&at(&format!("{home}/.local/bin/codex")), m),
        "a wrapper in ~/.local/bin isn't"
    );
}

#[test]
fn npm_prefixes_are_above_lib_node_modules_and_never_inside_a_tool_store() {
    use super::host::npm_prefix;
    use std::path::Path;
    let lin = Path::new("/home/me/.npm-global/lib/node_modules/@openai/codex/bin/codex.js");
    assert_eq!(npm_prefix(lin).as_deref(), Some("/home/me/.npm-global"));
    let mise = Path::new(
        "/home/me/.local/share/mise/installs/npm-openai-codex/0.162.1/lib/node_modules/@openai/codex/bin/codex.js",
    );
    assert_eq!(npm_prefix(mise), None);
}

#[test]
fn a_vendor_place_follows_its_installers_variables() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |v: &str| pairs.iter().find(|(k, _)| *k == v).map(|(_, x)| (*x).to_owned())
    };
    let set = env(&[("HOME", "/home/me"), ("CODEX_HOME", "/data/codex")]);
    assert_eq!(
        expand_place_with("%CODEX_HOME%/packages/standalone", &set).as_deref(),
        Some("/data/codex/packages/standalone")
    );
    // Unset: that place is skipped, and the installer's default is the next one.
    let unset = env(&[("HOME", "/home/me")]);
    assert_eq!(expand_place_with("%CODEX_HOME%/packages/standalone", &unset), None);
    assert_eq!(
        expand_place_with("~/.codex/packages/standalone", &unset).as_deref(),
        Some("/home/me/.codex/packages/standalone")
    );
    assert_eq!(
        expand_place_with("~/.local/share/claude", &set).as_deref(),
        Some("/home/me/.local/share/claude")
    );
    // The table names CODEX_HOME before the default, on every platform.
    for p in Platform::ALL {
        let m = catalog::item("codex").unwrap().method(p, |_| true).unwrap();
        assert_eq!(m.places.first().map(String::as_str), Some("%CODEX_HOME%/packages/standalone"));
    }
}

#[test]
fn the_vendors_own_copy_not_answering_is_repaired_never_deleted() {
    let mut h = npm_codex();
    h.vendor_mute.insert("codex".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["codex"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.failed, ["codex"], "{events:?}");
    let (error, fix) = events
        .iter()
        .find_map(|e| match e {
            Event::Failed { error, fix, .. } => Some((error.clone(), fix.clone())),
            _ => None,
        })
        .unwrap();
    assert!(error.contains("doesn't answer `codex --version`"), "{error}");
    assert!(
        fix.starts_with("repair it by installing it again: `ptyro install codex`, or run `"),
        "{fix}"
    );
    assert!(!fix.contains("rm ") && !fix.contains("Remove-Item"), "{fix}");
    assert_eq!(fix.matches("again").count(), 1, "said once: {fix}");
}

#[test]
fn npm_under_mise_or_asdf_node_is_a_prefix_and_their_own_stores_name_their_command() {
    use super::host::{npm_prefix, npm_store};
    use std::path::Path;
    let mise_node = Path::new(
        "/home/me/.local/share/mise/installs/node/22.1.0/lib/node_modules/@openai/codex/bin/codex.js",
    );
    assert_eq!(
        npm_prefix(mise_node).as_deref(),
        Some("/home/me/.local/share/mise/installs/node/22.1.0")
    );
    let asdf_node = Path::new(
        "/home/me/.asdf/installs/nodejs/22.1.0/lib/node_modules/@openai/codex/bin/codex.js",
    );
    assert_eq!(npm_prefix(asdf_node).as_deref(), Some("/home/me/.asdf/installs/nodejs/22.1.0"));
    let mise_npm = Path::new(
        "/home/me/.local/share/mise/installs/npm-openai-codex/0.162.1/lib/node_modules/@openai/codex/bin/codex.js",
    );
    assert_eq!(npm_store(mise_npm), Some("mise"));
    let item = catalog::item("codex").unwrap();
    let st = |prefix: Option<&str>, store: Option<&str>| State::Other {
        version: None,
        via: Via::Npm,
        path: None,
        latest: None,
        system: false,
        prefix: prefix.map(str::to_owned),
        store: store.map(str::to_owned),
    };
    let why = not_offered(item, Platform::Linux, &st(None, Some("mise")), &|_| true).unwrap();
    assert!(why.contains("remove it with `mise uninstall npm:@openai/codex`"), "{why}");
    let why = not_offered(item, Platform::Linux, &st(None, Some("bun")), &|_| true).unwrap();
    assert!(why.contains("`bun remove -g @openai/codex`"), "{why}");
    // A prefix that can't be put in a shell command: said so, not "no known way".
    let why =
        not_offered(item, Platform::Linux, &st(Some("/home/o'neil/.npm-global"), None), &|_| true)
            .unwrap();
    assert!(why.contains("has a character ptyro won't put in a shell command"), "{why}");
}

#[test]
fn an_npm_leftover_is_deleted_by_the_link_path_finds() {
    let cmd = super::moving::delete_command("/home/me/.npm-global/bin/codex");
    assert_eq!(cmd, "`rm '/home/me/.npm-global/bin/codex'`");
}

/// A new ptyro release is announced on the desktop once (SPEC §24.8), not with `notify` off.
#[test]
fn a_new_ptyro_release_is_announced_once() {
    use crate::updates;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = updates_cfg("daily", &[]);
    assert!(updates::announce(dir.path(), &cfg).is_none(), "nothing remembered yet");
    let row = |latest: &str| {
        serde_json::json!({ "id": "ptyro", "name": "ptyro", "program": "ptyro",
            "version": "0.0.68", "latest": latest, "doc": "" })
    };
    let write = |latest: &str| {
        let v = serde_json::json!({ "checked": 1, "outdated": [row(latest)] });
        std::fs::write(updates::path(dir.path()), v.to_string()).unwrap();
    };
    write("0.0.69");
    cfg.notify = false;
    assert!(updates::announce(dir.path(), &cfg).is_none(), "off says nothing");
    cfg.notify = true;
    assert_eq!(updates::announce(dir.path(), &cfg).map(|o| o.latest).as_deref(), Some("0.0.69"));
    assert!(updates::announce(dir.path(), &cfg).is_none(), "said once");
    // The next release is said again, once; one ignored isn't.
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(updates::path(dir.path())).unwrap()).unwrap();
    v["outdated"] = serde_json::json!([row("0.0.70")]);
    std::fs::write(updates::path(dir.path()), v.to_string()).unwrap();
    assert!(updates::announce(dir.path(), &updates_cfg("daily", &["ptyro@0.0.70"])).is_none());
    assert!(updates::announce(dir.path(), &cfg).is_some());
}
