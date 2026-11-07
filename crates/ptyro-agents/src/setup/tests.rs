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
            out.push(Installed { version: Some(v), path, via, system: false, prefix });
        }
        if let Some(v) = self.new.lock().unwrap().get(&item.id).cloned() {
            let path = Some("~/.local/bin/x".into());
            out.push(Installed { version: Some(v), path, via: None, system: false, prefix: None });
        }
        if let Some(t) = self.third.get(&item.id) {
            let path = Some("/opt/homebrew/bin/x".into());
            let v = Some("0.1.0".to_owned());
            out.push(Installed { version: v, path, via: Some(*t), system: false, prefix: None });
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
fn a_prerequisite_is_ticked_and_goes_first() {
    // Gemini CLI needs Node.js 20; a Mac with Homebrew.
    let host = TestHost::on(Platform::Macos).with(&["brew"]);
    let p = plan(&survey(&host, &BTreeMap::new()), &picks(&["gemini"]));
    let node = p.entry("node").unwrap();
    assert!(node.ticked && !node.picked);
    assert_eq!(node.needed_by, ["gemini"]);
    assert_eq!(node.action.as_ref().unwrap().command, "brew install node");
    let gemini = p.entry("gemini").unwrap().action.clone().unwrap();
    assert_eq!(gemini.after, ["node"]);
    assert_eq!(gemini.command, "npm install -g @google/gemini-cli", "npm will be there");
    let order: Vec<&str> = p.steps().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(order, ["node", "gemini"]);
}

#[test]
fn node_from_apt_only_when_new_enough() {
    let s = |cand: &str| {
        let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "sudo"]);
        h.candidates.insert("apt".into(), cand.into());
        survey(&h, &BTreeMap::new())
    };
    // Ubuntu 24.04's 18.x is too old for Gemini CLI: nodejs.org's build, no admin.
    let p = plan(&s("18.19.1"), &picks(&["gemini"]));
    let node = p.entry("node").unwrap().action.clone().unwrap();
    assert_eq!(node.via, Via::NodeDist);
    assert!(!node.admin);
    // A new enough one: apt's.
    let p = plan(&s("22.11.0"), &picks(&["gemini"]));
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::Apt);
    // apt's candidate unknown: not trusted to be new enough.
    let h = TestHost::on(Platform::Linux).with(&["apt-get"]);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["gemini"]));
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::NodeDist);
    // Ticked on its own (nothing names a minimum): apt's, whatever it has.
    let p = plan(&s("18.19.1"), &picks(&["node"]));
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::Apt);
}

#[test]
fn an_old_node_is_too_old_and_replaced_by_nodejs_orgs() {
    let mut h = TestHost::on(Platform::Linux).with(&["apt-get", "npm"]).have("node", "v18.19.1");
    h.candidates.insert("apt".into(), "18.19.1".into());
    let s = survey(&h, &BTreeMap::new());
    let p = plan(&s, &picks(&["gemini"]));
    let node = p.entry("node").unwrap();
    assert!(
        matches!(&node.state, State::TooOld { min, .. } if min == "20.0.0"),
        "{:?}",
        node.state
    );
    assert!(node.ticked);
    assert_eq!(node.action.as_ref().unwrap().via, Via::NodeDist);
    // New enough: nothing to do for it.
    let h = TestHost::on(Platform::Linux).with(&["npm"]).have("node", "v24.13.0");
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["gemini"]));
    let node = p.entry("node").unwrap();
    assert!(!node.ticked && node.action.is_none());
    assert!(p.entry("gemini").unwrap().action.as_ref().unwrap().after.is_empty());
}

#[test]
fn no_way_here_says_why() {
    // Windows without WinGet: git has no other method.
    let h = TestHost::on(Platform::WindowsX64);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["git"]));
    let git = p.entry("git").unwrap();
    assert!(git.action.is_none());
    assert!(git.blocked.as_deref().unwrap().contains("`winget`"), "{:?}", git.blocked);
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
    let mut h = TestHost::on(Platform::Macos).with(&["brew"]);
    h.fail.insert("node".into());
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["gemini", "claude"]));
    let (r, events) = run_all(&p, &h, Admin::Ask);
    assert_eq!(r.done, ["claude"]);
    assert_eq!(r.failed, ["node"]);
    assert_eq!(r.skipped, ["gemini"], "it needs the Node.js that failed");
    assert!(!r.ok());
    assert!(events.iter().any(
        |e| matches!(e, Event::Line { id, text } if id == "claude" && text == "installing claude")
    ));
    let fix = events.iter().find_map(|e| match e {
        Event::Failed { id, fix, .. } if id == "node" => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("ptyro install node"));
    let why = events.iter().find_map(|e| match e {
        Event::Skipped { id, reason, .. } if id == "gemini" => Some(reason.clone()),
        _ => None,
    });
    assert!(why.unwrap().contains("Node.js"));
    assert!(matches!(events.last(), Some(Event::Finished { .. })));
    // Claude was installed and has a login: the logins' turn (BOOT4) names it.
    assert!(events.iter().any(|e| matches!(e, Event::Logins { ids } if ids == &["claude"])));
    assert_eq!(h.calls(), ["claude vendor", "node brew"]);
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
    let h = TestHost::on(Platform::WindowsArm64);
    let p = plan(&survey(&h, &BTreeMap::new()), &picks(&["opencode"]));
    // OpenCode on Windows needs Node.js; no WinGet: nodejs.org's build.
    assert_eq!(p.entry("node").unwrap().action.as_ref().unwrap().via, Via::NodeDist);
    let (r, _) = run_all(&p, &h, Admin::Ask);
    assert!(r.ok() && r.tools);
    assert_eq!(r.done, ["node", "opencode"]);
}

#[test]
fn names_resolve_as_people_say_them() {
    let id = |n: &str| resolve(n).map(|i| i.id.as_str());
    assert_eq!(id("agy"), Ok("antigravity"));
    assert_eq!(id("Antigravity"), Ok("antigravity"));
    assert_eq!(id("github-cli"), Ok("gh"));
    assert_eq!(id("Claude Code"), Ok("claude"));
    assert_eq!(id("nodejs"), Ok("node"));
    assert_eq!(id("Node.js"), Ok("node"));
    let e = resolve("vim").unwrap_err();
    assert!(e.contains("agy") && e.contains("gh"), "{e}");
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

#[test]
fn a_refreshed_path_keeps_what_it_had_and_adds_what_installers_stored() {
    let tools = vec![std::path::PathBuf::from("/t/node/bin")];
    let cur = std::env::join_paths(["/usr/bin", "/t/node/bin", "/bin"]).unwrap();
    let fresh = [std::path::PathBuf::from("/opt/nodejs"), std::path::PathBuf::from("/usr/bin")];
    let out = refreshed_path(&cur, &fresh, &tools);
    let parts: Vec<_> = std::env::split_paths(&out).collect();
    assert_eq!(
        parts,
        ["/t/node/bin", "/usr/bin", "/bin", "/opt/nodejs"].map(std::path::PathBuf::from)
    );
}

#[test]
fn stored_paths_expand_and_prepend_without_touching_the_rest() {
    let var = |k: &str| (k == "USERPROFILE").then(|| r"C:\Users\me".to_owned());
    assert_eq!(
        expand_vars(r"%USERPROFILE%\bin;%NOPE%\x;50%;C:\y", var),
        r"C:\Users\me\bin;%NOPE%\x;50%;C:\y"
    );
    let raw = r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;;C:\Tools\;c:\pt\node";
    let out = prepend_entries(raw, &[r"C:\pt\node".into(), r"C:\pt\npm".into()]);
    assert_eq!(
        out,
        r"C:\pt\node;C:\pt\npm;%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;C:\Tools\"
    );
    // Again: unchanged.
    assert_eq!(prepend_entries(&out, &[r"C:\pt\node".into(), r"C:\pt\npm".into()]), out);
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
fn windows_rows_say_windows_and_wsls_plan_joins_them_as_a_second_side() {
    let win = TestHost::on(Platform::WindowsX64).with(&["winget"]);
    let linux = TestHost::on(Platform::Linux).with(&["apt-get", "sudo", "curl"]);
    let mine = plan(&survey(&win, &BTreeMap::new()), &ids(&["gemini"]));
    assert!(mine.entries.iter().all(|e| e.side == Side::Windows));
    let theirs = plan(&survey(&linux, &BTreeMap::new()), &ids(&["gemini", "gh"]));
    assert!(theirs.entries.iter().all(|e| e.side == Side::Local));
    let status = WslStatus::Usable { distro: "Ubuntu".into(), version: "ptyro 0.0.63".into() };
    let both = Plan::merge(mine, Some(theirs), Some(status.clone()));
    assert_eq!(both.sides(), [Side::Windows, Side::Wsl]);
    // Each side in order, each item after what it needs on its own side.
    let steps: Vec<(Side, &str)> = both.steps().iter().map(|e| (e.side, e.id.as_str())).collect();
    assert_eq!(
        steps,
        [
            (Side::Windows, "node"),
            (Side::Windows, "gemini"),
            (Side::Wsl, "gh"),
            (Side::Wsl, "node"),
            (Side::Wsl, "gemini"),
        ]
    );
    let wsl = both.side(Side::Wsl);
    assert!(wsl.entries.iter().all(|e| e.side == Side::Wsl));
    assert_eq!(wsl.entry_on("gh", Side::Wsl).unwrap().action.as_ref().unwrap().via, Via::Apt);
    assert!(both.entry_on("gh", Side::Windows).is_some_and(|e| !e.ticked));
    // The JSON carries the sides and WSL's state, and reads back.
    let j = serde_json::to_value(&both).unwrap();
    assert_eq!(
        j["wsl"],
        serde_json::json!({"state": "usable", "distro": "Ubuntu", "version": "ptyro 0.0.63"})
    );
    let back: Plan = serde_json::from_value(j).unwrap();
    assert_eq!(back, both);
    // A plan from before BOOT8 (no `wsl`, no `side`) still reads.
    let old = serde_json::json!({"platform": "linux", "entries": []});
    assert_eq!(serde_json::from_value::<Plan>(old).unwrap().wsl, None);
}

#[test]
fn where_means_sides_and_events_carry_theirs() {
    assert_eq!("both".parse::<Where>().unwrap().sides(Side::Windows), [Side::Windows, Side::Wsl]);
    assert_eq!("WSL".parse::<Where>().unwrap().sides(Side::Windows), [Side::Wsl]);
    assert_eq!("windows".parse::<Where>().unwrap().sides(Side::Windows), [Side::Windows]);
    assert_eq!("local".parse::<Where>().unwrap().sides(Side::Local), [Side::Local]);
    assert!("mars".parse::<Where>().unwrap_err().contains("wsl, windows or both"));
    let ev = Sided { side: Side::Wsl, event: Event::Done { id: "gh".into(), version: None } };
    let j = serde_json::to_value(&ev).unwrap();
    assert_eq!(j, serde_json::json!({"side": "wsl", "event": "done", "id": "gh", "version": null}));
    // A reader that knows only events reads it too.
    assert_eq!(serde_json::from_value::<Event>(j.clone()).unwrap(), ev.event);
    assert_eq!(serde_json::from_value::<Sided>(j).unwrap(), ev);
    let mut r = Report { done: ids(&["claude"]), ..Report::default() };
    r.merge(&Report { failed: ids(&["gh"]), tools: true, ..Report::default() });
    assert!(!r.ok() && r.tools && r.done == ["claude"]);
    let no = WslStatus::Unusable {
        reason: "WSL has no Linux distro".into(),
        fix: "`wsl --install`".into(),
    };
    assert_eq!(
        no.words(),
        "WSL: WSL has no Linux distro, so this sets up Windows only. For WSL too: `wsl --install`"
    );
    assert_eq!(no.usable(), None);
}

#[test]
fn a_login_is_decided_by_its_status_commands_exit_code_alone() {
    let item = |id: &str| catalog::item(id).unwrap();
    let h = TestHost::on(Platform::Linux).have("gh", "2.57.0").have("antigravity", "1.0.0");
    assert_eq!(login::status(&h, item("gh")), login::LoginState::LoggedOut);
    assert_eq!(login::status(&h, item("claude")), login::LoginState::Missing);
    // No status command documented: it can't be confirmed, and nothing is run to find out.
    assert_eq!(login::status(&h, item("antigravity")), login::LoginState::Unconfirmed);
    assert_eq!(h.calls(), ["status gh auth status"]);
    h.logged_in.lock().unwrap().insert("gh".into());
    assert_eq!(login::status(&h, item("gh")), login::LoginState::LoggedIn);

    // A list of them: the agents and gh, with what each runs and what confirms it.
    let rows = login::rows(&h, &[]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["claude", "codex", "antigravity", "opencode", "gemini", "gh"]);
    let gh = rows.iter().find(|r| r.id == "gh").unwrap();
    assert_eq!(gh.command, "gh auth login --web");
    assert_eq!(gh.check.as_deref(), Some("gh auth status"));
    assert_eq!(gh.side, Side::Local);
    let only = login::rows(&h, &picks(&["gh", "git"]));
    assert_eq!(only.len(), 1, "git has no login");
}

#[test]
fn logging_in_runs_the_tools_own_flow_then_asks_its_status() {
    let item = |id: &str| catalog::item(id).unwrap();
    let mut h = TestHost::on(Platform::Linux)
        .have("codex", "0.162.1")
        .have("gh", "2.57.0")
        .have("antigravity", "1.0.0");
    h.login_fails.insert("gh".into());
    h.login_fails.insert("antigravity".into());
    let done = login::log_in(&h, item("codex")).unwrap();
    assert_eq!((done.state, done.ended), (login::LoginState::LoggedIn, None));
    let gh = login::log_in(&h, item("gh")).unwrap();
    assert_eq!(gh.state, login::LoginState::LoggedOut);
    assert_eq!(gh.ended.as_deref(), Some("it exited with 1"));
    // An agent with no status command, quit with Ctrl+C: how it ended, nothing more known.
    let agy = login::log_in(&h, item("antigravity")).unwrap();
    assert_eq!(agy.state, login::LoginState::Unconfirmed);
    assert!(agy.ended.is_some());
    assert_eq!(
        h.calls(),
        [
            "login codex login",
            "status codex login status",
            "login gh auth login --web",
            "status gh auth status",
            "login agy",
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

/// The web's lines (BOOT3): each entry's words as the TUI says them. One side: the method in
/// the line; two sides: the method in each side's cell, what a cell can't say beside it.
#[test]
fn the_checklists_lines_say_what_the_tui_does() {
    use super::checklist::{cell, lines};
    let linux =
        TestHost::on(Platform::Linux).with(&["apt-get", "sudo", "curl"]).have("claude", "2.1.3");
    let here = survey(&linux, &BTreeMap::new());
    let one = plan(&here, &ids(&["gemini"]));
    let ls = lines(&one, &here, None);
    assert_eq!(ls.len(), one.entries.len());
    let of = |ls: &[super::checklist::Line], id: &str, side: Side| {
        ls.iter().find(|l| l.id == id && l.side == side).unwrap().clone()
    };
    let gemini = of(&ls, "gemini", Side::Local);
    assert!(
        gemini.about.contains("needs Node.js") && gemini.about.ends_with("then log in"),
        "{gemini:?}"
    );
    assert_eq!(gemini.how, "npm");
    assert!(of(&ls, "node", Side::Local).about.contains("ticked because Gemini CLI needs it"));
    // Unticked: the method it would use, from what is on PATH.
    assert!(
        of(&ls, "gh", Side::Local).about.contains("apt · asks for admin"),
        "{:?}",
        of(&ls, "gh", Side::Local)
    );
    assert_eq!(of(&ls, "gh", Side::Local).how, "");
    let claude = of(&ls, "claude", Side::Local);
    assert_eq!((claude.how.as_str(), claude.state.as_str()), ("✓ 2.1.3", "installed 2.1.3"));

    let win = TestHost::on(Platform::WindowsX64).with(&["winget"]);
    let mine_s = survey(&win, &BTreeMap::new());
    let status = WslStatus::Usable { distro: "Ubuntu".into(), version: "ptyro 0.0.63".into() };
    let both = Plan::merge(
        plan(&mine_s, &ids(&["git"])),
        Some(plan(&here, &ids(&["gemini"]))),
        Some(status),
    );
    let ls = lines(&both, &mine_s, Some(&here));
    let git = of(&ls, "git", Side::Windows);
    assert!(!git.about.contains("WinGet"), "the method is the cell's: {git:?}");
    assert_eq!(git.how, "WinGet (admin)");
    assert_eq!(cell(both.entry_on("git", Side::Windows)), "[x] WinGet (admin)");
    let node = of(&ls, "node", Side::Wsl);
    assert_eq!(node.why, ["ticked because Gemini CLI needs it"]);
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
        keep_installer: Vec::new(),
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
    // The others (Antigravity, OpenCode, Gemini CLI) couldn't be reached: kept, quietly.
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
fn windows_refusing_to_replace_a_running_program_says_why_and_how_to_retry() {
    let mut host = TestHost::on(Platform::WindowsX64).have("claude", "2.1.300");
    host.fail.insert("claude".into());
    host.errors.insert(
        "claude".into(),
        "it exited with 1: The process cannot access the file because it is being used by another process.".into(),
    );
    let latest = BTreeMap::from([("claude".to_owned(), "2.1.400".to_owned())]);
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
    assert!(fix.contains("Windows won't replace a program while it runs"), "{fix}");
    assert!(fix.contains("then try the update again (`ptyro update claude`)"), "{fix}");
    assert!(events.iter().any(|e| matches!(e, Event::Failed { in_use: true, .. })));
    // Elsewhere a running program's file is busy too (ETXTBSY), without blaming Windows.
    let mut host = TestHost::on(Platform::Linux).have("claude", "2.1.300");
    host.fail.insert("claude".into());
    host.errors.insert("claude".into(), "it exited with 1: Text file busy".into());
    let p = plan(&survey(&host, &latest), &picks(&["claude"]));
    let (_, events) = run_all(&p, &host, Admin::Ask);
    let fix = events
        .iter()
        .find_map(|e| match e {
            Event::Failed { fix, .. } => Some(fix.clone()),
            _ => None,
        })
        .unwrap();
    assert!(fix.contains("its file couldn't be replaced while it runs"), "{fix}");
    assert!(!fix.contains("Windows"), "{fix}");
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
    assert!(other_update(item, Platform::Linux, Via::Winget).is_none());
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
    let line = checklist::about(&p, e, &|_| true, false);
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
    assert!(error.contains("works (~/.local/bin/x), but removing npm's copy"), "{error}");
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
        "/opt/homebrew/Cellar/gemini-cli/0.33.0/libexec/lib/node_modules/@google/gemini-cli/dist/index.js",
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
    let win = Path::new(r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js");
    assert_eq!(npm_prefix(win).as_deref(), Some(r"C:\Users\me\AppData\Roaming\npm"));
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
    };
    let none = BTreeSet::new();
    assert!(offer(item, Platform::Linux, &st(Via::Npm, false, None), &none, &|_| true).is_none());
    assert!(offer(item, Platform::Macos, &st(Via::Brew, true, None), &none, &|_| true).is_none());
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
