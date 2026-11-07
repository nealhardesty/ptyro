//! Out of date, offered (ROADMAP BOOT5, SPEC §24.7), end to end: the agents' newest versions
//! come from a fixture registry (`PTYRO_TEST_LATEST`: `<dir>/<id>` holds each source's reply, in
//! the shape BOOT0's recorded fixtures have; a missing one is offline), the clock from
//! `PTYRO_TEST_NOW`, and the machine and its updates are BOOT1's fake (`PTYRO_TEST_SETUP`,
//! `ptyro fake-installer`). Nothing reaches the network or installs anything real.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
/// 2026-11-07 00:00 UTC, in ms: the fake clock's "now".
const NOW: u64 = 1_793_923_200_000;
const HOUR: u64 = 3600 * 1000;

/// A fake machine with a fixture registry.
struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(world: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot5-").tempdir().unwrap();
        for d in ["fake", "home", "ptyro", "registry"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("fake/world.json"), world.to_string()).unwrap();
        std::fs::write(dir.path().join("clock"), NOW.to_string()).unwrap();
        Self { dir }
    }

    fn fake(&self) -> PathBuf {
        self.dir.path().join("fake")
    }

    fn registry(&self) -> PathBuf {
        self.dir.path().join("registry")
    }

    fn clock(&self) -> PathBuf {
        self.dir.path().join("clock")
    }

    fn state(&self) -> PathBuf {
        ptyro_testkit::paths_for(&self.dir.path().join("ptyro")).unwrap().state
    }

    /// The registry's reply for `id` (BOOT0's shapes: Claude's is plain text, Codex's a GitHub
    /// release, the others npm's or Antigravity's `{"version"}`).
    fn publish(&self, id: &str, version: &str) {
        let body = match id {
            "claude" => version.to_owned(),
            "codex" => json!({ "tag_name": format!("rust-v{version}") }).to_string(),
            _ => json!({ "version": version }).to_string(),
        };
        std::fs::write(self.registry().join(id), body).unwrap();
    }

    fn config(&self, text: &str) {
        let p = ptyro_testkit::paths_for(&self.dir.path().join("ptyro")).unwrap();
        std::fs::create_dir_all(&p.config).unwrap();
        std::fs::write(p.config.join("config.toml"), text).unwrap();
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("PTYRO_TEST_SETUP", self.fake().display().to_string()),
            ("PTYRO_TEST_LATEST", self.registry().display().to_string()),
            ("PTYRO_TEST_NOW", format!("@{}", self.clock().display())),
        ]
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_HOME", self.dir.path().join("ptyro"))
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_WSL", self.dir.path().join("no-wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for (k, v) in self.env() {
            c.env(k, v);
        }
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
        }
        c
    }

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

    fn lines(&self, file: &Path) -> Vec<String> {
        std::fs::read_to_string(file).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// Each newest-version source asked (looked up all at once, so in no set order).
    fn asked(&self) -> Vec<String> {
        self.lines(&self.registry().join("asked.log"))
    }

    fn calls(&self) -> Vec<String> {
        self.lines(&self.fake().join("calls.log"))
    }

    fn versions(&self) -> Value {
        std::fs::read_to_string(self.state().join("versions.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    }

    /// `doctor --json`'s `updates` checks.
    fn doctor(&self, quick: bool) -> Vec<Value> {
        let args: &[&str] =
            if quick { &["doctor", "--quick", "--json"] } else { &["doctor", "--json"] };
        let o = self.run(args, "");
        let all: Vec<Value> =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        all.into_iter().filter(|c| c["section"] == "updates").collect()
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// Linux with Claude Code 2.1.300 and Codex 0.170.0; an update installs `versions`'.
fn machine() -> Machine {
    let m = Machine::new(json!({
        "platform": "linux",
        "programs": ["curl"],
        "installed": { "claude": "2.1.300", "codex": "0.170.0" },
        "versions": { "claude": "2.1.400", "codex": "0.171.0" },
    }));
    m.publish("claude", "2.1.400");
    m.publish("codex", "0.170.0");
    m
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn doctor_looks_afresh_and_the_quick_one_reads_whats_remembered() {
    let m = machine();
    let checks = m.doctor(false);
    let claude = checks.iter().find(|c| c["name"] == "claude").expect("claude's finding");
    assert_eq!(claude["level"], "warn");
    assert_eq!(
        claude["text"],
        "Claude Code 2.1.400 is out (you have 2.1.300): `ptyro update claude` updates it (it runs `claude update`)"
    );
    assert!(!checks.iter().any(|c| c["name"] == "codex"), "Codex is the newest: {checks:?}");
    // The other agents' sources are offline here: said quietly, as a note.
    let lookup = checks.iter().find(|c| c["name"] == "lookup").unwrap();
    assert_eq!(lookup["level"], "note");
    let asked = m.asked();
    assert_eq!(asked.len(), 5, "every agent's source, once: {asked:?}");
    // Remembered.
    let v = m.versions();
    assert_eq!(v["checked"], NOW);
    assert_eq!(v["latest"]["claude"], "2.1.400");
    // The badge's quick doctor reads what was remembered: the same finding, nothing fetched.
    let quick = m.doctor(true);
    assert_eq!(quick.iter().find(|c| c["name"] == "claude").unwrap()["text"], claude["text"]);
    assert_eq!(m.asked().len(), 5);
    // `update --check` looks afresh, and says so in JSON too.
    let o = m.run(&["update", "--check", "--json"], "");
    let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["outdated"][0]["id"], "claude");
    assert_eq!(doc["outdated"][0]["command"], "claude update");
    assert_eq!(m.asked().len(), 10);
}

#[test]
fn offline_is_quiet_and_off_fetches_nothing() {
    let m = machine();
    for id in ["claude", "codex"] {
        std::fs::remove_file(m.registry().join(id)).unwrap();
    }
    let checks = m.doctor(false);
    assert!(checks.iter().all(|c| c["level"] != "warn" && c["level"] != "problem"), "{checks:?}");
    let lookup = checks.iter().find(|c| c["name"] == "lookup").unwrap();
    assert_eq!(
        lookup["text"],
        "couldn't look for newer versions (offline?): ptyro looks again tomorrow"
    );
    // `ptyro update` offline: nothing to update, said calmly, exit 0.
    let o = m.run(&["update", "--yes"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Couldn't look for newer versions (offline?)"), "{}", text(&o));
    assert!(m.calls().is_empty());
    // Off: nothing is looked up, and nothing offered.
    let m = machine();
    m.config("[updates]\ncheck = \"off\"\n");
    let checks = m.doctor(false);
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0]["name"], "check");
    assert!(checks[0]["text"].as_str().unwrap().starts_with("off (updates.check)"));
    assert!(m.asked().is_empty(), "nothing fetched: {:?}", m.asked());
}

#[test]
fn update_shows_the_command_asks_and_updates_then_nothing_is_offered() {
    let m = machine();
    m.publish("codex", "0.171.0");
    // No: nothing runs.
    let o = m.run(&["update"], "n\n");
    let t = text(&o);
    assert!(t.contains("  Claude Code 2.1.300 → 2.1.400: `claude update`"), "{t}");
    assert!(
        t.contains(
            "  Codex 0.170.0 → 0.171.0: `curl -fsSL https://chatgpt.com/codex/install.sh | sh`"
        ),
        "{t}"
    );
    assert!(t.contains("Update? [Y/n]"), "{t}");
    assert!(t.contains("Nothing updated."), "{t}");
    assert!(m.calls().is_empty());
    // updates.ignore skips Codex (that version); yes updates the rest.
    m.config("[updates]\nignore = [\"codex@0.171.0\"]\n");
    let o = m.run(&["update"], "y\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(m.calls(), ["update claude via vendor admin=false"]);
    assert!(t.contains("▸ Claude Code\n  $ claude update"), "{t}");
    assert!(t.contains("✓ Claude Code 2.1.400"), "{t}");
    assert!(t.contains("1 updated."), "{t}");
    // Remembered as current: the badge, the web and the note offer nothing for Claude now.
    let v = m.versions();
    assert!(v["outdated"].as_array().unwrap().iter().all(|o| o["id"] != "claude"), "{v}");
    let quick = m.doctor(true);
    assert!(!quick.iter().any(|c| c["name"] == "claude" && c["level"] == "warn"), "{quick:?}");
    let codex = quick.iter().find(|c| c["name"] == "codex").unwrap();
    assert_eq!(codex["level"], "note");
    assert!(codex["text"].as_str().unwrap().ends_with("skipped (updates.ignore)"));
    // Named, a skipped one updates all the same; one up to date says so.
    let o = m.run(&["update", "codex", "claude", "--yes"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("✓ Claude Code 2.1.400 is the newest (2.1.400)"), "{t}");
    assert_eq!(m.calls().last().unwrap(), "update codex via vendor admin=false");
    // A tool isn't updated by ptyro; an unknown name is refused.
    let o = m.run(&["update", "git"], "");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("ptyro updates the agents; git updates the way you installed it"));
    let o = m.run(&["update", "vim"], "");
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
}

#[test]
fn an_update_waits_for_a_running_install_and_another_way_updates_its_own_way() {
    let m = Machine::new(json!({
        "platform": "linux",
        "programs": ["curl", "npm"],
        "installed": { "codex": "0.162.1" },
        "other": { "codex": "npm" },
    }));
    m.publish("codex", "0.170.0");
    // One install or update at a time on the machine.
    let held = ptyro_agents::setup::lock::Lock::take(&m.state(), "`ptyro install`").unwrap();
    let o = m.run(&["update", "--yes"], "");
    assert_eq!(o.status.code(), Some(5), "{}", text(&o));
    assert!(text(&o).contains("setup is already running (from `ptyro install` (process "));
    assert!(m.calls().is_empty());
    drop(held);
    // Installed through npm: its update is npm's.
    let o = m.run(&["update", "--yes"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.calls(), ["update codex via npm admin=false"]);
}

#[test]
fn windows_wont_replace_a_running_agent_and_offers_a_retry() {
    let m = Machine::new(json!({
        "platform": "windows-x64",
        "programs": ["curl"],
        "installed": { "claude": "2.1.300" },
        "versions": { "claude": "2.1.400" },
        "outcomes": { "claude": "busy" },
    }));
    m.publish("claude", "2.1.400");
    let o = m.run(&["update", "claude"], "y\ny\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("being used by another process"), "{t}");
    assert!(t.contains("Windows won't replace a program while it runs"), "{t}");
    assert!(t.contains("Try again now? [Y/n]"), "{t}");
    assert!(t.contains("✓ Claude Code 2.1.400"), "{t}");
    assert_eq!(m.calls(), ["update claude via vendor admin=false"; 2]);
    // Said no: it stays as it was, with the fix.
    let m = Machine::new(json!({
        "platform": "windows-x64",
        "installed": { "claude": "2.1.300" },
        "outcomes": { "claude": "busy" },
    }));
    m.publish("claude", "2.1.400");
    let o = m.run(&["update", "claude"], "y\nn\n");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("then try the update again (`ptyro update claude`)"), "{t}");
    assert_eq!(m.calls().len(), 1);
}

/// The daemon looks once a day in the background (never at start-up's moment), remembers it,
/// and between looks only asks the installed versions again; the web's update is its
/// `SetupRun` of the outdated agent, which runs the update and refreshes what's remembered.
#[tokio::test]
async fn the_daemon_looks_once_a_day_and_the_webs_update_runs_through_it() {
    let m = machine();
    let mut env = m.env();
    env.push(("PTYRO_TEST_UPDATES_FIRST_MS", "0".into()));
    env.push(("PTYRO_TEST_UPDATES_EVERY_MS", "100".into()));
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let d = TempDaemon::start_with_env(BIN, &env).unwrap();
    let state = d.paths.state.clone();
    let versions = || -> Value {
        std::fs::read_to_string(state.join("versions.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    wait_for("the first look", || versions()["checked"] == NOW);
    assert_eq!(m.asked().len(), 5);
    // An hour on: it asks the installed versions again (`surveyed` moves), and looks nothing up.
    std::fs::write(m.clock(), (NOW + HOUR).to_string()).unwrap();
    wait_for("an hour's refresh", || versions()["surveyed"] == NOW + HOUR);
    assert_eq!(m.asked().len(), 5, "at most once a day");
    // A day on: it looks again.
    std::fs::write(m.clock(), (NOW + 25 * HOUR).to_string()).unwrap();
    wait_for("the next day's look", || m.asked().len() == 10);
    // The web's checklist plans with what was remembered: Claude Code is out of date there.
    let mut c = d.client().await.unwrap();
    let Reply::SetupPlan { plan, .. } = c
        .request(Request::SetupPlan { picks: Some(vec!["claude".into()]), wsl_picks: None })
        .await
        .unwrap()
    else {
        panic!("not a plan");
    };
    let e = plan.entry("claude").unwrap();
    assert!(
        matches!(&e.state, ptyro_agents::setup::State::Outdated { latest, .. } if latest == "2.1.400")
    );
    assert!(e.action.as_ref().is_some_and(|a| a.update && a.command == "claude update"));
    // The web's Update: a run of it.
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let Reply::SetupStarted { run, .. } = c
        .request(Request::SetupRun { picks: vec!["claude".into()], wsl_picks: None })
        .await
        .unwrap()
    else {
        panic!("not started");
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let report = loop {
        assert!(Instant::now() < deadline, "no report");
        let Ok(Some(ServerMsg::Event { event: DomainEvent::SetupProgress { run: r, event, .. } })) =
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
        else {
            continue;
        };
        assert_eq!(r, run);
        if let ptyro_agents::setup::Event::Finished { report } = event {
            break report;
        }
    };
    assert_eq!(report.done, ["claude"]);
    assert_eq!(m.calls(), ["update claude via vendor admin=false"]);
    // What's remembered follows (just after the report): nothing offered for Claude Code now,
    // and the daemon's hourly look running beside it never writes the old view back.
    wait_for("the refresh after the update", || {
        versions()["outdated"].as_array().is_some_and(|o| o.iter().all(|o| o["id"] != "claude"))
    });
    std::fs::write(m.clock(), (NOW + 26 * HOUR).to_string()).unwrap();
    wait_for("an hour's refresh", || versions()["surveyed"] == NOW + 26 * HOUR);
    let v = versions();
    assert!(v["outdated"].as_array().unwrap().iter().all(|o| o["id"] != "claude"), "{v}");
}

/// An agent in use (the design's call): the update goes ahead, and the sessions still running the
/// old version until they restart are named, before the question and after the update.
#[test]
fn the_sessions_running_an_agent_are_named_before_and_after_its_update() {
    let m = machine();
    let fake_agent = format!("['{BIN}', \"fake-agent\", \"--as\", \"claude\"]");
    let config = format!("[agents.claude]\ncommand = {fake_agent}\n");
    let env = m.env();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
    let work = tempfile::tempdir().unwrap();
    let o = d
        .cmd()
        .args(["new", "-d", "-n", "brisk-rook", "-a", "claude", "-C"])
        .arg(work.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let mut child = d
        .cmd()
        .args(["update", "claude"])
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"y\n").unwrap();
    let o = child.wait_with_output().unwrap();
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("    1 session still runs Claude Code 2.1.300 until restarted: brisk-rook"),
        "{t}"
    );
    assert!(
        t.contains("\n1 session still runs Claude Code 2.1.300 until restarted: brisk-rook"),
        "{t}"
    );
    assert!(t.contains("1 updated."), "{t}");
}

/// A name in `updates.ignore` that isn't an agent (a typo) skips nothing, and `doctor` says so.
#[test]
fn doctor_says_when_the_ignore_list_names_no_agent() {
    let m = machine();
    m.config("[updates]\nignore = [\"codx@0.170.0\", \"codex\"]\n");
    let checks = m.doctor(true);
    let bad: Vec<&Value> = checks.iter().filter(|c| c["name"] == "ignore").collect();
    assert_eq!(bad.len(), 1, "{checks:?}");
    assert!(bad[0]["text"].as_str().unwrap().contains("\"codx@0.170.0\", which isn't an agent"));
    // The finding carries what it is about, for a client to act on (the web's Update).
    let claude = m.doctor(false).into_iter().find(|c| c["name"] == "claude").unwrap();
    assert_eq!(claude["data"]["program"], "claude");
    assert_eq!(claude["data"]["latest"], "2.1.400");
}

/// Only the OS lock says setup is running: a `setup.who` left behind (Ctrl-C, a closed window)
/// naming a live, unrelated process (a reused id; here this test's own) neither refuses an
/// update nor freezes the looks.
#[test]
fn a_stale_note_of_who_holds_setup_blocks_nothing() {
    let m = machine();
    std::fs::create_dir_all(m.state()).unwrap();
    let stale = || {
        std::fs::write(
            m.state().join("setup.who"),
            format!("`ptyro install` (process {})", std::process::id()),
        )
        .unwrap();
    };
    stale();
    let checks = m.doctor(false);
    assert!(checks.iter().any(|c| c["name"] == "claude" && c["level"] == "warn"), "{checks:?}");
    assert_eq!(m.versions()["surveyed"], NOW, "the look wrote");
    stale();
    let o = m.run(&["update", "claude", "--yes"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.calls(), ["update claude via vendor admin=false"]);
}
