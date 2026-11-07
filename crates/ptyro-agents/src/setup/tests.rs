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
        let v = self.installed.lock().unwrap().get(&item.id).cloned()?;
        Some(Installed { version: Some(v), path: None, via: self.other.get(&item.id).copied() })
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
        if self.fail.contains(id) {
            return Err("it exited with 1: E: no network".into());
        }
        self.installed.lock().unwrap().insert(id.clone(), "26.13.0".into());
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
    fn interact(&self, argv: &[String]) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("login {}", argv.join(" ")));
        let item = catalog::items()
            .iter()
            .find(|i| i.login.as_ref().is_some_and(|l| l.start == argv))
            .ok_or("no such login")?;
        if self.login_fails.contains(&item.id) {
            return Err("it exited with 1".into());
        }
        self.logged_in.lock().unwrap().insert(item.id.clone());
        Ok(())
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
    assert!(p.entry("claude").unwrap().action.is_none(), "BOOT7 offers the move");
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
    let mut h = TestHost::on(Platform::Linux).have("codex", "0.162.1").have("gh", "2.57.0");
    h.login_fails.insert("gh".into());
    assert_eq!(login::log_in(&h, item("codex")), Ok(login::LoginState::LoggedIn));
    let err = login::log_in(&h, item("gh")).unwrap_err();
    assert!(err.contains("exited with 1"), "{err}");
    assert_eq!(
        h.calls(),
        [
            "login codex login",
            "status codex login status",
            "login gh auth login --web",
            "status gh auth status"
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
