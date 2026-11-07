//! `ptyro setup` and `ptyro install` end to end (ROADMAP BOOT1), on the fake machine the debug
//! build's `PTYRO_TEST_SETUP` names: every step runs `ptyro fake-installer` (a real child whose
//! output streams), nothing real is installed or asked for, and the network is never used. The
//! daemon's `SetupPlan` / `SetupRun` (for the web) run the same engine. The plan's own logic is
//! unit-tested in `ptyro_agents::setup`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// A fake machine: a ptyro home, a `$HOME`, and the fake's directory with its `world.json`.
struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(world: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot1-").tempdir().unwrap();
        for d in ["fake", "home", "ptyro"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("fake/world.json"), world.to_string()).unwrap();
        Self { dir }
    }

    fn fake(&self) -> PathBuf {
        self.dir.path().join("fake")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_TEST_SETUP", self.fake())
            .env("PTYRO_HOME", self.dir.path().join("ptyro"))
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("SHELL", "/bin/bash")
            // A Windows machine looks for WSL (BOOT8): never the real one.
            .env("PTYRO_WSL", self.dir.path().join("no-wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
        }
        c
    }

    /// Run with `input` on stdin.
    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn installed(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.fake().join("installed").join(id)).ok()
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// A fresh Linux machine with apt and sudo (no password asked: `ready`).
fn fresh_linux(extra: Value) -> Value {
    let mut w = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"] });
    if let (Some(w), Some(e)) = (w.as_object_mut(), extra.as_object()) {
        w.extend(e.clone());
    }
    w
}

#[test]
fn unattended_setup_installs_the_defaults_in_order() {
    let m = Machine::new(fresh_linux(json!({})));
    let o = m.run(&["setup", "--yes"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(
        m.calls(),
        [
            "install claude via vendor admin=false",
            "install codex via vendor admin=false",
            "install git via apt admin=true",
            "install gh via apt admin=true",
        ]
    );
    for id in ["claude", "codex", "git", "gh"] {
        assert!(m.installed(id).is_some(), "{id}");
    }
    assert!(t.contains("▸ Claude Code\n  $ curl -fsSL https://claude.ai/install.sh | bash"), "{t}");
    assert!(t.contains("  │ fake vendor: install claude"), "{t}");
    assert!(t.contains("✓ Claude Code 1.0.0"), "{t}");
    assert!(t.contains("▸ git (as administrator)"), "{t}");
    assert!(t.contains("4 installed."), "{t}");
    // The logins come next (BOOT4): --yes runs none, and says where each stands.
    assert!(
        t.contains("✗ GitHub CLI: not logged in: `ptyro login gh` (it runs `gh auth login --web`)"),
        "{t}"
    );
    // Again: everything is here.
    let o = m.run(&["setup", "--yes"], "");
    assert!(o.status.success());
    assert!(text(&o).contains("Nothing to install."), "{}", text(&o));
    assert_eq!(m.calls().len(), 4);
}

#[test]
fn a_failure_offers_its_fix_and_the_rest_installs() {
    let m = Machine::new(fresh_linux(json!({ "outcomes": { "codex": "fail" } })));
    let o = m.run(&["setup", "--yes", "--agents", "claude,codex", "--tools", "none"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("✗ Codex: it exited with 1: "), "{t}");
    assert!(t.contains("E: could not reach the server for codex\n"), "{t}");
    assert!(t.contains("fix: `ptyro install codex` tries again"), "{t}");
    assert!(t.contains("✓ Claude Code"), "{t}");
    assert!(t.contains("1 installed, 1 failed"), "{t}");
    assert!(m.installed("codex").is_none() && m.installed("claude").is_some());
    assert!(!m.calls().iter().any(|c| c.contains(" git ")), "--tools none: no tools");
}

#[test]
fn output_streams_live_as_json_lines() {
    let m = Machine::new(fresh_linux(json!({ "outcomes": { "antigravity": "slow" } })));
    let mut child = m
        .cmd(&["install", "agy", "--yes", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let mut steps = Vec::new();
    let mut events = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let v: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if v["event"] == "line" && v["text"].as_str().unwrap().contains("step ") {
            steps.push(start.elapsed());
        }
        events.push(v);
    }
    assert!(child.wait().unwrap().success());
    // Each line arrived as it was printed, 300 ms apart, not all at the end.
    assert_eq!(steps.len(), 3, "{events:?}");
    assert!(steps[2] - steps[0] >= Duration::from_millis(450), "{steps:?}");
    let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(kinds.first(), Some(&"started"));
    assert_eq!(kinds.last(), Some(&"finished"));
    assert!(events.iter().any(|e| e["event"] == "done" && e["id"] == "antigravity"));
}

#[test]
fn admin_steps_ask_once_in_a_terminal_and_never_with_yes() {
    let world = fresh_linux(json!({ "elevation": "prompt" }));
    // --yes: no prompt is possible, so they're reported with their command.
    let m = Machine::new(world.clone());
    let o = m.run(&["setup", "--yes"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(!m.calls().contains(&"elevate".to_owned()), "{:?}", m.calls());
    assert!(t.contains("– git: not installed, it needs administrator rights, which would ask for a password (--yes asks nothing)"), "{t}");
    assert!(t.contains("fix: run `sudo apt-get install -y git` in a terminal"), "{t}");
    assert!(m.installed("claude").is_some() && m.installed("git").is_none());
    // At the terminal (the checklist, then Enter): asked once, before the first step.
    let m = Machine::new(world);
    let o = m.run(&["setup"], "\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    let calls = m.calls();
    assert_eq!(calls.iter().filter(|c| *c == "elevate").count(), 1, "{calls:?}");
    assert_eq!(calls[0], "elevate");
    assert!(
        t.contains(
            "git, GitHub CLI need administrator rights: sudo asks for your password once, here."
        ),
        "{t}"
    );
    // Refused: those steps are skipped, the rest installs.
    let m = Machine::new(fresh_linux(json!({ "elevation": "prompt", "elevate": false })));
    let o = m.run(&["install", "gh", "claude"], "\n");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("administrator rights weren't given"), "{t}");
    assert!(m.installed("claude").is_some() && m.installed("gh").is_none());
}

#[test]
fn the_checklist_ticks_by_number_and_quits_cleanly() {
    let m = Machine::new(fresh_linux(json!({ "installed": { "git": "2.43.0" } })));
    // 3 is Antigravity; 1 unticks Claude Code; Enter installs.
    let o = m.run(&["setup"], "3 1\n\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("   1 [x] Claude Code"), "{t}");
    assert!(t.contains("   3 [x] Antigravity"), "{t}");
    assert!(t.contains("✓ git: installed 2.43.0"), "{t}");
    let installed: Vec<&str> = ["claude", "codex", "antigravity", "gh"]
        .into_iter()
        .filter(|i| m.installed(i).is_some())
        .collect();
    assert_eq!(installed, ["codex", "antigravity", "gh"]);
    // q installs nothing.
    let m = Machine::new(fresh_linux(json!({})));
    let o = m.run(&["setup"], "q\n");
    assert!(o.status.success());
    assert!(text(&o).contains("Nothing installed."));
    assert!(m.calls().is_empty());
    // install asks too: end of input is no.
    let o = m.run(&["install", "agy"], "");
    assert!(text(&o).contains("Install? [Y/n]"), "{}", text(&o));
    assert!(m.calls().is_empty());
}

#[test]
fn node_from_nodejs_org_when_apts_is_too_old_and_its_folder_goes_on_path() {
    let m = Machine::new(fresh_linux(json!({ "candidates": { "apt": "18.19.1" } })));
    // The plan first, as JSON: Node.js ticked for Gemini CLI, from nodejs.org, no admin.
    let o = m.run(&["install", "gemini", "--dry-run", "--json"], "");
    assert!(o.status.success(), "{}", text(&o));
    let plan: Value = serde_json::from_slice(&o.stdout).unwrap();
    let node = plan["entries"].as_array().unwrap().iter().find(|e| e["id"] == "node").unwrap();
    assert_eq!((node["ticked"].as_bool(), node["picked"].as_bool()), (Some(true), Some(false)));
    assert_eq!(node["needed_by"], json!(["gemini"]));
    assert_eq!(node["action"]["via"], "node-dist");
    assert_eq!(node["action"]["admin"], false);
    let o = m.run(&["install", "gemini", "--yes"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(
        m.calls(),
        ["install node via node-dist admin=false", "install gemini via npm admin=false"]
    );
    assert!(t.contains("went into ptyro's tools folder"), "{t}");
    // --yes leaves the rc file (Windows: the user's PATH) alone and says what to add.
    let says = if cfg!(windows) { "add these to your PATH" } else { "add this line to" };
    assert!(t.contains(says), "{t}");
    assert!(!m.home().join(".bashrc").exists());
    // shell-init now puts the folder first on PATH, for every shell.
    let o = m.run(&["shell-init", "bash"], "");
    let init = String::from_utf8_lossy(&o.stdout).into_owned();
    // Its own form for bash: `/c/…` on Windows (Git Bash), where `C:\…` would split at the colon.
    let leaf = m.dir.path().file_name().unwrap().to_string_lossy().into_owned();
    assert!(init.contains(&format!("{leaf}/ptyro/ptyro-tools/node")), "{init}");
    if cfg!(windows) {
        assert!(init.contains("PATH='/"), "{init}");
    }
    assert!(init.contains("case \":$PATH:\""), "{init}");
    assert!(!init.contains(":\\"), "no drive path left for bash: {init}");
}

#[test]
fn names_and_lists_are_checked() {
    let m = Machine::new(fresh_linux(json!({})));
    let o = m.run(&["install", "vim", "--yes"], "");
    assert_eq!(o.status.code(), Some(3));
    assert!(
        text(&o).contains("\"vim\" isn't something ptyro installs (it knows claude, codex, agy"),
        "{}",
        text(&o)
    );
    let o = m.run(&["setup", "--yes", "--agents", "gh"], "");
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("GitHub CLI isn't for --agents: put it in --tools"));
    // Installed already: said, nothing run.
    let m = Machine::new(fresh_linux(json!({ "installed": { "gh": "2.57.0" } })));
    let o = m.run(&["install", "github-cli", "--yes"], "");
    assert!(o.status.success());
    assert!(text(&o).contains("✓ GitHub CLI is here already (installed 2.57.0)"), "{}", text(&o));
    assert!(m.calls().is_empty());
}

/// The daemon's requests (for the web): the same plan; a run's progress as events; admin steps
/// never run there, each reported with the command to run on the computer.
#[tokio::test]
async fn the_daemon_plans_and_installs_without_admin() {
    let m = Machine::new(fresh_linux(json!({})));
    let fake = m.fake().display().to_string();
    let d = TempDaemon::start_with_env(BIN, &[("PTYRO_TEST_SETUP", fake.as_str())]).unwrap();
    let mut c = d.client().await.unwrap();
    let Reply::SetupPlan { plan, .. } =
        c.request(Request::SetupPlan { picks: None, wsl_picks: None }).await.unwrap()
    else {
        panic!("not a plan");
    };
    let steps: Vec<&str> = plan.steps().iter().map(|e| e.id.as_str()).collect();
    assert_eq!(steps, ["claude", "codex", "git", "gh"]);
    let r =
        c.request(Request::SetupPlan { picks: Some(vec!["vim".into()]), wsl_picks: None }).await;
    assert!(r.is_err());
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let Reply::SetupStarted { run, .. } = c
        .request(Request::SetupRun { picks: vec!["agy".into(), "git".into()], wsl_picks: None })
        .await
        .unwrap()
    else {
        panic!("not started");
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = Vec::new();
    let report = loop {
        assert!(Instant::now() < deadline, "no report: {seen:?}");
        let Ok(Some(ServerMsg::Event { event: DomainEvent::SetupProgress { run: r, event, .. } })) =
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
        else {
            continue;
        };
        assert_eq!(r, run);
        if let ptyro_agents::setup::Event::Finished { report } = &event {
            break report.clone();
        }
        seen.push(event);
    };
    assert_eq!(report.done, ["antigravity"]);
    assert_eq!(report.skipped, ["git"]);
    let fix = seen.iter().find_map(|e| match e {
        ptyro_agents::setup::Event::Skipped { fix, .. } => Some(fix.clone()),
        _ => None,
    });
    assert!(fix.unwrap().contains("on the computer itself: `ptyro install git`"));
    assert!(!m.calls().iter().any(|c| c == "elevate" || c.contains(" git ")), "{:?}", m.calls());
    assert!(m.installed("antigravity").is_some());
}

/// The web's checklist (BOOT3, protocol 1.34): the plan comes with each row's line in the TUI's
/// words and the default agent; while a run is going, every plan says so, with its events so
/// far, numbered as the live ones are, and a second run is refused; once it ends, it's gone.
#[tokio::test]
async fn the_daemon_says_a_run_is_going_with_its_progress() {
    let m = Machine::new(fresh_linux(json!({ "outcomes": { "antigravity": "wait" } })));
    let fake = m.fake().display().to_string();
    let d = TempDaemon::start_with_env(BIN, &[("PTYRO_TEST_SETUP", fake.as_str())]).unwrap();
    let mut c = d.client().await.unwrap();
    let plan_req = || Request::SetupPlan { picks: None, wsl_picks: None };
    let Reply::SetupPlan { plan, lines, default_agent, running, held_by } =
        c.request(plan_req()).await.unwrap()
    else {
        panic!("not a plan");
    };
    assert!(running.is_none());
    assert!(held_by.is_none());
    assert_eq!(default_agent, "claude");
    assert_eq!(lines.len(), plan.entries.len());
    let line = |id: &str| lines.iter().find(|l| l.id == id).unwrap().clone();
    let codex = line("codex");
    assert!(codex.about.ends_with("· then log in"), "{codex:?}");
    assert_eq!(codex.state, "not installed");
    assert_eq!(line("git").how, "apt (admin)", "{:?}", line("git"));
    assert!(line("git").about.contains("asks for admin"), "{:?}", line("git"));

    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let Reply::SetupStarted { run, .. } =
        c.request(Request::SetupRun { picks: vec!["agy".into()], wsl_picks: None }).await.unwrap()
    else {
        panic!("not started");
    };
    // Its first line arrives while the installer waits.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut live = Vec::new();
    while !live.iter().any(|(_, e): &(u64, Value)| e["text"] == "fake vendor: waiting for go") {
        assert!(Instant::now() < deadline, "no line: {live:?}");
        if let Ok(Some(ServerMsg::Event {
            event: DomainEvent::SetupProgress { run: r, event, seq, .. },
        })) = tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
        {
            assert_eq!(r, run);
            live.push((seq, serde_json::to_value(&event).unwrap()));
        }
    }
    let seqs: Vec<u64> = live.iter().map(|(s, _)| *s).collect();
    assert_eq!(seqs, (1..=live.len() as u64).collect::<Vec<_>>(), "numbered from 1");
    let Reply::SetupPlan { running: Some(going), .. } = c.request(plan_req()).await.unwrap() else {
        panic!("no run going");
    };
    assert_eq!(going.run, run);
    assert!(going.plan.entry("antigravity").unwrap().will_run());
    let kept: Vec<(u64, Value)> =
        going.events.iter().map(|l| (l.seq, serde_json::to_value(&l.event).unwrap())).collect();
    assert_eq!(kept, live, "the events so far, as they were sent");
    let again =
        c.request(Request::SetupRun { picks: vec!["opencode".into()], wsl_picks: None }).await;
    assert!(
        format!("{again:?}").contains("setup is already running (from the web app"),
        "{again:?}"
    );
    // The machine's lock: `ptyro install` from a terminal on the same machine is refused too,
    // saying where the run came from, and installs nothing.
    let o = d
        .cmd()
        .args(["install", "opencode", "--yes"])
        .env("PTYRO_TEST_SETUP", &fake)
        .env("PTYRO_WSL", m.dir.path().join("no-wsl"))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{said}");
    assert!(
        said.contains("setup is already running (from the web app, through the daemon (process "),
        "{said}"
    );

    std::fs::write(m.fake().join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "no report");
        if let Ok(Some(ServerMsg::Event {
            event:
                DomainEvent::SetupProgress {
                    event: ptyro_agents::setup::Event::Finished { report },
                    ..
                },
        })) = tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
        {
            assert_eq!(report.done, ["antigravity"]);
            break;
        }
    }
    // The run lets go of its place just after its last event.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let Reply::SetupPlan { running, .. } = c.request(plan_req()).await.unwrap() else {
            panic!("not a plan");
        };
        if running.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "still going");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(m.calls().iter().all(|c| !c.contains("opencode")), "{:?}", m.calls());
}

/// One install at a time on a machine, from any client (the machine's setup lock): a second
/// `ptyro install` while one runs says where the first came from and installs nothing; the first
/// carries on, and afterwards the lock is free again.
#[test]
fn two_installs_at_once_the_second_is_refused() {
    let m = Machine::new(fresh_linux(json!({ "outcomes": { "antigravity": "wait" } })));
    let first = m
        .cmd(&["install", "agy", "--yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !m.calls().iter().any(|c| c.starts_with("install antigravity")) {
        assert!(Instant::now() < deadline, "the first never started: {:?}", m.calls());
        std::thread::sleep(Duration::from_millis(50));
    }
    let second = m.run(&["install", "opencode", "--yes"], "");
    let said = text(&second);
    // Asked up front, before the checklist or a question.
    let checklist = m.run(&["setup"], "");
    let shown = text(&checklist);
    // (Written before the asserts: a failing one would leave the first waiting a minute.)
    std::fs::write(m.fake().join("go"), "").unwrap();
    assert!(!second.status.success(), "{said}");
    assert!(said.contains("setup is already running (from `ptyro install` (process "), "{said}");
    assert!(!checklist.status.success(), "{shown}");
    assert!(shown.contains("setup is already running (from `ptyro install` (process "), "{shown}");
    assert!(!shown.contains("Agents"), "no checklist walked: {shown}");
    let first = first.wait_with_output().unwrap();
    assert!(first.status.success(), "{}", text(&first));
    assert!(m.installed("antigravity").is_some());
    assert!(m.installed("opencode").is_none());
    assert!(!m.calls().iter().any(|c| c.contains("opencode")), "{:?}", m.calls());
    let again = m.run(&["install", "opencode", "--yes"], "");
    assert!(again.status.success(), "free again: {}", text(&again));
}

/// The other way round: `ptyro install` in a terminal holds the machine's lock, so the daemon's
/// plan says who (for the web page to say so up front), and its `SetupRun` is refused with it;
/// once that install ends, both are free again.
#[tokio::test]
async fn the_daemon_says_and_refuses_while_a_terminal_installs() {
    let m = Machine::new(fresh_linux(json!({ "outcomes": { "antigravity": "wait" } })));
    let fake = m.fake().display().to_string();
    let d = TempDaemon::start_with_env(BIN, &[("PTYRO_TEST_SETUP", fake.as_str())]).unwrap();
    let first = d
        .cmd()
        .args(["install", "agy", "--yes"])
        .env("PTYRO_TEST_SETUP", &fake)
        .env("PTYRO_WSL", m.dir.path().join("no-wsl"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !m.calls().iter().any(|c| c.starts_with("install antigravity")) {
        assert!(Instant::now() < deadline, "the install never started: {:?}", m.calls());
        std::thread::sleep(Duration::from_millis(50));
    }
    let c = d.client().await.unwrap();
    let plan_req = || Request::SetupPlan { picks: None, wsl_picks: None };
    let Reply::SetupPlan { running, held_by, .. } = c.request(plan_req()).await.unwrap() else {
        panic!("not a plan");
    };
    assert!(running.is_none(), "not the daemon's run");
    let by = held_by.expect("who holds it");
    assert!(by.starts_with("`ptyro install` (process "), "{by}");
    let r = c.request(Request::SetupRun { picks: vec!["opencode".into()], wsl_picks: None }).await;
    std::fs::write(m.fake().join("go"), "").unwrap();
    let said = format!("{r:?}");
    assert!(said.contains("setup is already running (from `ptyro install` (process "), "{said}");
    let first = first.wait_with_output().unwrap();
    assert!(first.status.success(), "{}", text(&first));
    assert!(!m.calls().iter().any(|c| c.contains("opencode")), "{:?}", m.calls());
    let Reply::SetupPlan { held_by, .. } = c.request(plan_req()).await.unwrap() else {
        panic!("not a plan");
    };
    assert!(held_by.is_none(), "free again");
    // A refused run doesn't keep the daemon's own flag: the next one starts.
    let Reply::SetupStarted { .. } = c
        .request(Request::SetupRun { picks: vec!["opencode".into()], wsl_picks: None })
        .await
        .unwrap()
    else {
        panic!("not started");
    };
}

/// At a terminal, the rc file gets ptyro's line (the form `ptyro uninstall` takes out again), and
/// a shell that runs it finds the tools folder first, once.
#[cfg(unix)]
#[test]
fn the_rc_line_is_offered_and_puts_the_tools_on_path() {
    let m = Machine::new(fresh_linux(json!({ "candidates": { "apt": "18.19.1" } })));
    // Install? yes. Add the line? yes.
    let o = m.run(&["install", "gemini"], "\n\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    let rc = std::fs::read_to_string(m.home().join(".bashrc")).unwrap();
    assert!(rc.ends_with("\n# ptyro shell integration, and the tools ptyro setup installed\neval \"$(ptyro shell-init bash)\"\n"), "{rc:?}");
    let tools = m.dir.path().join("ptyro").join("ptyro-tools").join("node").join("bin");
    let init = String::from_utf8(m.run(&["shell-init", "bash"], "").stdout).unwrap();
    let script = format!("PATH=/usr/bin:/bin\n{init}\n{init}\nprintf %s \"$PATH\"");
    let out = Command::new("bash").args(["--norc", "-c", &script]).output().unwrap();
    let path = String::from_utf8(out.stdout).unwrap();
    assert_eq!(path, format!("{}:/usr/bin:/bin", tools.display()), "{init}");
    // Asked again later: the line is there, so it says so and adds nothing.
    let o = m.run(&["install", "node", "--yes"], "");
    assert!(o.status.success());
    assert_eq!(std::fs::read_to_string(m.home().join(".bashrc")).unwrap(), rc);
}

/// Windows: WinGet's Node.js adds its folder only to the stored Path; the next step (Gemini
/// CLI's `npm install -g`) still finds it, because the PATH is refreshed after each step.
#[test]
fn a_folder_an_installer_stored_is_on_path_for_the_next_step() {
    let m = Machine::new(json!({
        "platform": "windows-x64",
        "programs": ["winget"],
        "path_adds": { "node": "Program Files/nodejs" },
        "path_needs": { "gemini": "Program Files/nodejs" },
    }));
    let o = m.run(&["install", "gemini", "--yes"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(
        m.calls(),
        ["install node via winget admin=true", "install gemini via npm admin=false"]
    );
    // Without the folder, the fake's npm isn't found: the check is real.
    let m = Machine::new(json!({
        "platform": "windows-x64",
        "programs": ["winget"],
        "path_needs": { "gemini": "Program Files/nodejs" },
    }));
    let o = m.run(&["install", "gemini", "--yes"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("npm: command not found"), "{}", text(&o));
}

/// Ticked but impossible here (a Linux with neither apt nor dnf): said with its fix, in the
/// summary and the JSON, and the exit says not everything installed.
#[test]
fn what_cant_be_installed_here_is_said_and_fails_the_run() {
    let m = Machine::new(json!({ "platform": "linux", "programs": ["curl"] }));
    let o = m.run(&["setup", "--yes"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("✗ git: can't be installed here: it needs `apt-get` or `dnf` first"), "{t}");
    assert!(t.contains("fix: install what it needs"), "{t}");
    assert!(t.contains("2 installed, 2 can't be installed here"), "{t}");
    assert!(m.installed("claude").is_some());
    // Only what's blocked: still said, still a failure.
    let o = m.run(&["install", "gh", "--yes", "--json"], "");
    assert_eq!(o.status.code(), Some(1));
    let events: Vec<Value> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["event"] == "blocked" && e["id"] == "gh"), "{events:?}");
    let last = events.last().unwrap();
    assert_eq!(last["report"]["blocked"], json!(["gh"]));
}
