//! Logins (ROADMAP BOOT4) end to end, on the fake machine `PTYRO_TEST_SETUP` names: a login
//! runs `ptyro fake-installer login ID` in the terminal (a device code, then logged in, failed,
//! or nothing done, by `world.json`'s `logins`), and a status command is answered from what it
//! wrote. Nothing real is logged in, no credential exists, and the network is never used. The
//! engine's own rules are unit-tested in `ptyro_agents::setup`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const HINT: &str = "`ptyro profile new work`, then `ptyro profile login work claude`";

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    /// A Linux with apt, sudo and curl, merged with `extra`.
    fn new(extra: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot4-").tempdir().unwrap();
        for d in ["fake", "home", "ptyro"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let mut w = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"] });
        w.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        std::fs::write(dir.path().join("fake/world.json"), w.to_string()).unwrap();
        Self { dir }
    }

    fn fake(&self) -> PathBuf {
        self.dir.path().join("fake")
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

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_TEST_SETUP", self.fake())
            .env("PTYRO_HOME", self.dir.path().join("ptyro"))
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_WSL", self.dir.path().join("no-wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
        }
        c
    }

    /// `logins.log`: each login run (`login ID`) and status asked (`status ID`).
    fn logins(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake().join("logins.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn ran(&self) -> Vec<String> {
        self.logins().into_iter().filter(|l| l.starts_with("login ")).collect()
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn setup_walks_through_the_logins_of_what_it_installed_then_names_profiles() {
    let m = Machine::new(json!({ "installed": { "git": "2.43.0" } }));
    // The checklist (Enter installs), then each login in turn, asking nothing more.
    let o = m.run(&["setup", "--agents", "claude", "--tools", "gh,git"], "\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(m.ran(), ["login claude", "login gh"], "git has no login");
    assert!(t.contains("▸ Logging in to Claude Code: `claude auth login`"), "{t}");
    assert!(t.contains("ptyro never sees it"), "{t}");
    // The tool's own sign-in reaches the person's terminal as it is.
    assert!(t.contains("enter the code FAKE-claude"), "{t}");
    assert!(t.contains("✓ Claude Code: logged in"), "{t}");
    assert!(t.contains("▸ Logging in to GitHub CLI: `gh auth login --web`"), "{t}");
    assert!(t.contains("✓ GitHub CLI: logged in"), "{t}");
    // The logins come after the installs, and the profiles last.
    let installed = t.find("2 installed.").unwrap();
    let first = t.find("▸ Logging in").unwrap();
    let hint = t.find(HINT).unwrap_or_else(|| panic!("{t}"));
    assert!(installed < first && first < hint, "{t}");

    // Again: logged in already, so nothing runs, and installing what's here walks its login.
    let o = m.run(&["install", "gh"], "y\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("✓ GitHub CLI is here already"), "{t}");
    assert!(t.contains("✓ GitHub CLI: logged in"), "{t}");
    assert_eq!(m.ran(), ["login claude", "login gh"]);
}

#[test]
fn unattended_setup_says_where_each_login_stands_and_runs_none() {
    let m = Machine::new(json!({ "installed": { "gh": "2.57.0" }, "logged_in": ["gh"] }));
    let o = m.run(&["setup", "--yes", "--agents", "claude,agy", "--tools", "gh"], "");
    let t = text(&o);
    assert!(o.status.success(), "the exit code is the installs': {t}");
    assert!(m.ran().is_empty(), "{:?}", m.logins());
    assert!(t.contains("✗ Claude Code: not logged in: `ptyro login claude`"), "{t}");
    assert!(t.contains("✓ GitHub CLI: logged in"), "{t}");
    assert!(t.contains("? Antigravity: can't be checked"), "{t}");
    assert!(t.contains("`ptyro login` logs them in, in a terminal."), "{t}");
    assert!(t.contains(HINT), "{t}");
    // --json: the logins' turn is an event; nothing is walked or said.
    let m = Machine::new(json!({}));
    let o = m.run(&["setup", "--yes", "--json", "--agents", "codex", "--tools", "none"], "");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.lines().any(|l| l.contains(r#""event":"logins""#) && l.contains("codex")), "{out}");
    assert!(m.ran().is_empty());
}

#[test]
fn ptyro_login_logs_in_each_one_that_isnt_and_a_failure_offers_its_fix() {
    let m = Machine::new(json!({
        "installed": { "claude": "2.1.319", "codex": "0.162.1", "gh": "2.57.0", "antigravity": "1.0.0" },
        "logged_in": ["codex"],
        "logins": { "claude": "fail", "gh": "nothing" },
    }));
    let o = m.run(&["login"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    // Codex is logged in; Antigravity can't be checked, so it isn't started unasked.
    assert_eq!(m.ran(), ["login claude", "login gh"]);
    assert!(t.contains("✓ Codex: logged in"), "{t}");
    assert!(t.contains("✗ Claude Code: not logged in: it exited with 1"), "{t}");
    assert!(
        t.contains("fix: `ptyro login claude` tries again, or run `claude auth login` yourself"),
        "{t}"
    );
    assert!(t.contains("✗ GitHub CLI: still not logged in (`gh auth status` says so)"), "{t}");
    assert!(t.contains("? Antigravity: can't be checked; it signs in when it starts (`ptyro login agy` starts it now)"), "{t}");

    // Named, the one that can't be checked runs too, and says it can't be confirmed.
    let o = m.run(&["login", "agy"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("This starts Antigravity: sign in, then quit it to come back here."), "{t}");
    assert!(t.contains("? Antigravity: done."), "{t}");
    assert_eq!(m.ran().last().map(String::as_str), Some("login antigravity"));

    // Not installed, or no login at all.
    let o = m.run(&["login", "opencode"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o).contains("– OpenCode: not installed: `ptyro install opencode`"),
        "{}",
        text(&o)
    );
    let o = m.run(&["login", "git"], "");
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("git has no login"), "{}", text(&o));
    let o = m.run(&["login", "nope"], "");
    assert_eq!(o.status.code(), Some(3));
}

#[test]
fn check_and_json_only_ask_the_status_commands() {
    let m = Machine::new(json!({
        "installed": { "claude": "2.1.319", "gh": "2.57.0", "gemini": "0.30.0" },
        "logged_in": ["gh"],
    }));
    let o = m.run(&["login", "--check"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "Claude is logged out: {t}");
    assert!(
        t.contains(
            "✗ Claude Code: not logged in: `ptyro login claude` (it runs `claude auth login`)"
        ),
        "{t}"
    );
    assert!(t.contains("✓ GitHub CLI: logged in"), "{t}");
    assert!(t.contains("? Gemini CLI: can't be checked"), "{t}");
    assert!(!t.contains("Codex"), "what isn't installed isn't listed unless named: {t}");
    let o = m.run(&["login", "--json"], "");
    let rows: Value = serde_json::from_slice(&o.stdout).unwrap();
    let state = |id: &str| {
        rows.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap()["state"].clone()
    };
    assert_eq!(state("claude"), "logged_out");
    assert_eq!(state("gh"), "logged_in");
    assert_eq!(state("gemini"), "unconfirmed");
    assert_eq!(state("codex"), "missing");
    assert_eq!(rows[0]["command"], "claude auth login");
    assert_eq!(rows[0]["check"], "claude auth status");
    assert!(m.ran().is_empty(), "{:?}", m.logins());
    // Only the status commands ran: Claude's and gh's (Gemini names none).
    let mut asked = m.logins();
    asked.sort();
    asked.dedup();
    assert_eq!(asked, ["status claude", "status gh"]);
    // Nothing installed: it says how to get them.
    let m = Machine::new(json!({}));
    let o = m.run(&["login"], "");
    assert!(o.status.success());
    assert!(
        text(&o).contains("No agent or GitHub CLI is installed yet: `ptyro setup` installs them.")
    );
}

#[test]
fn on_windows_a_wsl_sides_logins_run_in_that_distro() {
    let dir = tempfile::Builder::new().prefix("ptyro-boot4-wsl-").tempdir().unwrap();
    let d = dir.path();
    for p in ["fake", "home", "ptyro", "wsl/Ubuntu/fake", "wsl/Ubuntu/home", "wsl/Ubuntu/ptyro"] {
        std::fs::create_dir_all(d.join(p)).unwrap();
    }
    std::fs::write(d.join("fake/world.json"), json!({ "platform": "windows-x64", "programs": ["winget"], "installed": { "gh": "2.57.0" } }).to_string()).unwrap();
    std::fs::write(d.join("wsl/distros"), "Ubuntu\n").unwrap();
    let linux = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"], "installed": { "codex": "0.162.1" } });
    std::fs::write(d.join("wsl/Ubuntu/fake/world.json"), linux.to_string()).unwrap();
    let wsl = ptyro_testkit::program::program(
        &d.join("bin"),
        "wsl",
        BIN,
        &format!("exec \"{BIN}\" fake-wsl \"$@\""),
        &[BIN, "fake-wsl"],
        &[],
    );
    let run = |args: &[&str], input: &str| {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_TEST_SETUP", d.join("fake"))
            .env("PTYRO_HOME", d.join("ptyro"))
            .env("HOME", d.join("home"))
            .env("USERPROFILE", d.join("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_WSL", &wsl)
            .env("PTYRO_FAKE_WSL", d.join("wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
        }
        let mut child =
            c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    let log = |p: &str| std::fs::read_to_string(d.join(p)).unwrap_or_default();

    // Where each stands, both sides, each row saying its side.
    let o = run(&["login", "--json", "--where", "both"], "");
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let rows: Value = serde_json::from_slice(&o.stdout).unwrap();
    let side_of = |id: &str, side: &str| {
        rows.as_array().unwrap().iter().find(|r| r["id"] == id && r["side"] == side).cloned()
    };
    assert_eq!(side_of("gh", "windows").unwrap()["state"], "logged_out");
    assert_eq!(side_of("codex", "wsl").unwrap()["state"], "logged_out");
    assert_eq!(side_of("codex", "windows").unwrap()["state"], "missing");

    // Installing into WSL walks WSL's logins in WSL, through wsl.exe, in this terminal.
    let o = run(&["install", "claude", "--where", "wsl"], "y\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    // Setup's list, with what it just installed, and a person at this terminal.
    assert!(
        log("wsl/wsl.log").contains("ptyro login --installed=claude --tty claude"),
        "{}",
        log("wsl/wsl.log")
    );
    assert!(log("wsl/Ubuntu/fake/logins.log").contains("login claude"), "{t}");
    assert!(t.contains("✓ Claude Code: logged in"), "{t}");
    assert!(!log("fake/logins.log").contains("login claude"), "not on Windows' side");
    assert!(t.contains(HINT), "{t}");

    // `ptyro login --where wsl` is WSL's own walk.
    let o = run(&["login", "--where", "wsl"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(log("wsl/Ubuntu/fake/logins.log").contains("login codex"), "{t}");
}

/// The web: `POST /api/host` `login.list` says where each stands, and `login.start` runs
/// `ptyro login <id>` in a session on the daemon, which the web then shows.
#[test]
fn the_web_lists_the_logins_and_runs_one_in_a_session() {
    let m = Machine::new(json!({ "installed": { "gh": "2.57.0" } }));
    let fake = m.fake().to_string_lossy().into_owned();
    let home = m.dir.path().join("home").to_string_lossy().into_owned();
    let d = TempDaemon::start_with_env(
        BIN,
        &[("PTYRO_TEST_SETUP", &fake), ("HOME", &home), ("SHELL", "/bin/sh")],
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut serve = d
        .cmd()
        .args(["serve", "--loopback-http", "--port", &port.to_string()])
        .env("PTYRO_TEST_SETUP", &fake)
        .env("HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let token = {
        let o = d.cmd().args(["pair", "phone", "--scope", "full", "--json"]).output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
        let (_, body) = http(port, "/api/pair", None, &json!({ "code": code }));
        body["token"].as_str().unwrap().to_owned()
    };
    let host = |body: Value| http(port, "/api/host", Some(&token), &body).1;

    let v = host(json!({ "op": "login.list" }));
    let rows = v["reply"]["logins"].as_array().unwrap_or_else(|| panic!("{v}"));
    let gh = rows.iter().find(|r| r["id"] == "gh").unwrap();
    assert_eq!(gh["state"], "logged_out", "{v}");
    assert!(v["reply"]["profiles"].as_str().unwrap().contains("ptyro profile login"), "{v}");

    let v = host(json!({ "op": "login.start", "id": "gh" }));
    let session = v["reply"]["session"].as_str().unwrap_or_else(|| panic!("{v}")).to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let o = d.cmd().args(["read", &session]).output().unwrap();
        let screen = String::from_utf8_lossy(&o.stdout).into_owned();
        if screen.contains("✓ GitHub CLI: logged in") {
            assert!(screen.contains("FAKE-gh"), "the device code shows in the session: {screen}");
            break;
        }
        assert!(Instant::now() < deadline, "the login never finished:\n{screen}");
        std::thread::sleep(Duration::from_millis(200));
    }
    // What the sign-in printed (a code the person may paste) was never recorded to disk.
    let recording = d.paths.data.join("sessions").join(&session);
    assert!(!recording.exists(), "{} was recorded", recording.display());
    let v = host(json!({ "op": "login.list" }));
    let rows = v["reply"]["logins"].as_array().unwrap();
    assert_eq!(rows.iter().find(|r| r["id"] == "gh").unwrap()["state"], "logged_in", "{v}");
    let v = host(json!({ "op": "login.start", "id": "git" }));
    assert_eq!(v["t"], "err", "{v}");
    let _ = serve.kill();
    let _ = serve.wait();
}

/// The web on a Windows daemon with WSL (the final review's M2): `login.list` and `login.start`
/// take `where: "wsl"`, so WSL's logins are listed and run from the page too, in a session
/// running `ptyro login --where wsl <id>` (WSL's own ptyro, through wsl.exe, in that terminal).
#[test]
fn the_web_lists_and_runs_wsls_logins_too() {
    let dir = tempfile::Builder::new().prefix("ptyro-boot4-wweb-").tempdir().unwrap();
    let d = dir.path();
    for p in ["fake", "home", "wsl/Ubuntu/fake", "wsl/Ubuntu/home", "wsl/Ubuntu/ptyro"] {
        std::fs::create_dir_all(d.join(p)).unwrap();
    }
    let win = json!({ "platform": "windows-x64", "programs": ["winget"], "installed": { "gh": "2.57.0" } });
    std::fs::write(d.join("fake/world.json"), win.to_string()).unwrap();
    std::fs::write(d.join("wsl/distros"), "Ubuntu\n").unwrap();
    let linux = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"], "installed": { "codex": "0.162.1" } });
    std::fs::write(d.join("wsl/Ubuntu/fake/world.json"), linux.to_string()).unwrap();
    let wsl = ptyro_testkit::program::program(
        &d.join("bin"),
        "wsl",
        BIN,
        &format!("exec \"{BIN}\" fake-wsl \"$@\""),
        &[BIN, "fake-wsl"],
        &[],
    );
    let s = |p: PathBuf| p.to_string_lossy().into_owned();
    let (fake, home, wsl, fake_wsl) =
        (s(d.join("fake")), s(d.join("home")), s(wsl), s(d.join("wsl")));
    let env = [
        ("PTYRO_TEST_SETUP", fake.as_str()),
        ("HOME", home.as_str()),
        ("SHELL", "/bin/sh"),
        ("PTYRO_WSL", wsl.as_str()),
        ("PTYRO_FAKE_WSL", fake_wsl.as_str()),
    ];
    let daemon = TempDaemon::start_with_env(BIN, &env).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut serve = daemon
        .cmd()
        .args(["serve", "--loopback-http", "--port", &port.to_string()])
        .envs(env)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let token = {
        let o = daemon.cmd().args(["pair", "phone", "--scope", "full", "--json"]).output().unwrap();
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
        let (_, body) = http(port, "/api/pair", None, &json!({ "code": code }));
        body["token"].as_str().unwrap().to_owned()
    };
    let host = |body: Value| http(port, "/api/host", Some(&token), &body).1;

    let v = host(json!({ "op": "login.list", "where": "wsl" }));
    let rows = v["reply"]["logins"].as_array().unwrap_or_else(|| panic!("{v}"));
    let codex = rows.iter().find(|r| r["id"] == "codex").unwrap_or_else(|| panic!("{v}"));
    assert_eq!(codex["state"], "logged_out", "{v}");
    assert_eq!(codex["side"], "wsl", "{v}");
    let v = host(json!({ "op": "login.list", "where": "elsewhere" }));
    assert_eq!(v["t"], "err", "{v}");

    let v = host(json!({ "op": "login.start", "id": "codex", "where": "wsl" }));
    let session = v["reply"]["session"].as_str().unwrap_or_else(|| panic!("{v}")).to_owned();
    let log = |p: &str| std::fs::read_to_string(d.join(p)).unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log("wsl/Ubuntu/fake/logins.log").contains("login codex") {
        let o = daemon.cmd().args(["read", &session]).output().unwrap();
        assert!(
            Instant::now() < deadline,
            "no login in WSL:\n{}",
            String::from_utf8_lossy(&o.stdout)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(!log("fake/logins.log").contains("login codex"), "not on Windows' side");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let v = host(json!({ "op": "login.list", "where": "wsl" }));
        let rows = v["reply"]["logins"].as_array().unwrap_or_else(|| panic!("{v}"));
        if rows.iter().find(|r| r["id"] == "codex").unwrap()["state"] == "logged_in" {
            break;
        }
        assert!(Instant::now() < deadline, "never logged in: {v}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = serve.kill();
    let _ = serve.wait();
}

/// `POST path` with JSON (and a bearer token) → (status, the JSON body).
fn http(port: u16, path: &str, token: Option<&str>, body: &Value) -> (u16, Value) {
    let body = body.to_string();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let json = text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok());
    (code, json.unwrap_or(Value::Null))
}

#[test]
fn one_that_cant_be_checked_starts_when_just_installed_or_named_never_on_every_rerun() {
    // Antigravity here already, Gemini CLI not: setup starts only the one it just installed.
    let m = Machine::new(json!({
        "installed": { "antigravity": "1.0.0", "node": "24.13.0" },
        "programs": ["apt-get", "sudo", "curl"],
    }));
    let o = m.run(&["setup", "--agents", "agy,gemini", "--tools", "none"], "\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(m.ran(), ["login gemini"], "{t}");
    assert!(t.contains("? Antigravity: can't be checked; it signs in when it starts (`ptyro login agy` starts it now)"), "{t}");
    // Again: Gemini CLI is here now, so neither starts.
    let o = m.run(&["setup", "--agents", "agy,gemini", "--tools", "none"], "\n");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.ran(), ["login gemini"], "{}", text(&o));
}

#[test]
fn quitting_one_that_cant_be_checked_is_a_skip_not_a_failure() {
    // Ctrl+C (or however the agent quits) ends it non-zero.
    let m = Machine::new(
        json!({ "installed": { "antigravity": "1.0.0" }, "logins": { "antigravity": "fail" } }),
    );
    let o = m.run(&["login", "agy"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("– Antigravity: skipped (it exited with 1). If it isn't signed in, it asks when it starts"), "{t}");
    assert!(!t.contains("✗"), "{t}");
}

#[test]
fn installed_but_not_found_yet_says_to_open_a_new_terminal() {
    // An installer that reports success but puts the program off this terminal's PATH.
    let m = Machine::new(json!({ "outcomes": { "claude": "quiet" } }));
    let o = m.run(&["setup", "--agents", "claude", "--tools", "none"], "\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("– Claude Code: installed, but this terminal can't find it yet: open a new terminal, then `ptyro login claude`"), "{t}");
    assert!(!t.contains("`ptyro install claude`"), "{t}");
    assert!(!t.contains("No agent or GitHub CLI is installed yet"), "{t}");
    assert!(m.ran().is_empty());
}

#[test]
fn where_refusals_speak_of_logins() {
    let m = Machine::new(json!({}));
    let o = m.run(&["login", "--where", "wsl"], "");
    assert_eq!(o.status.code(), Some(2));
    let t = text(&o);
    assert!(t.contains("an agent can be logged in on WSL's side, Windows', or both"), "{t}");
    assert!(!t.contains("install"), "{t}");
}

/// Ctrl+C during a login is the login's: ptyro carries on with the rest (here the console's
/// Ctrl+C reaching ptyro alone, as Windows' console sends it to every process on it).
#[cfg(unix)]
#[test]
fn ctrl_c_during_a_login_never_ends_ptyro() {
    let wait_for = |f: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "never happened");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let interrupt = |pid: u32| {
        assert!(Command::new("kill").args(["-INT", &pid.to_string()]).status().unwrap().success());
    };

    // This computer's login.
    let m = Machine::new(json!({ "installed": { "gh": "2.57.0" }, "logins": { "gh": "slow" } }));
    let child = m
        .cmd(&["login"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // ptyro's handler is in place before it starts the login (`interrupts_skip`), so once the
    // login has written its line, Ctrl+C is the login's: no wait beyond that condition.
    wait_for(&|| m.ran() == ["login gh"]);
    interrupt(child.id());
    let o = child.wait_with_output().unwrap();
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("✓ GitHub CLI: logged in"), "{t}");

    // WSL's, from Windows: ptyro.exe waits on wsl.exe, and Ctrl+C mustn't end it there either.
    let dir = tempfile::Builder::new().prefix("ptyro-boot4-int-").tempdir().unwrap();
    let d = dir.path();
    for p in ["fake", "home", "ptyro", "wsl/Ubuntu/fake", "wsl/Ubuntu/home", "wsl/Ubuntu/ptyro"] {
        std::fs::create_dir_all(d.join(p)).unwrap();
    }
    std::fs::write(
        d.join("fake/world.json"),
        json!({ "platform": "windows-x64", "programs": ["winget"] }).to_string(),
    )
    .unwrap();
    std::fs::write(d.join("wsl/distros"), "Ubuntu\n").unwrap();
    let linux = json!({ "platform": "linux", "installed": { "codex": "0.162.1" }, "logins": { "codex": "slow" } });
    std::fs::write(d.join("wsl/Ubuntu/fake/world.json"), linux.to_string()).unwrap();
    let wsl = ptyro_testkit::program::program(
        &d.join("bin"),
        "wsl",
        BIN,
        &format!("exec \"{BIN}\" fake-wsl \"$@\""),
        &[BIN, "fake-wsl"],
        &[],
    );
    let mut c = Command::new(BIN);
    c.args(["login", "--where", "wsl"])
        .env("PTYRO_TEST_SETUP", d.join("fake"))
        .env("PTYRO_HOME", d.join("ptyro"))
        .env("HOME", d.join("home"))
        .env("PTYRO_WSL", &wsl)
        .env("PTYRO_FAKE_WSL", d.join("wsl"))
        .env("PTYRO_NO_AUTOSPAWN", "1");
    for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
        c.env_remove(v);
    }
    let child =
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let log = d.join("wsl/Ubuntu/fake/logins.log");
    wait_for(&|| std::fs::read_to_string(&log).unwrap_or_default().contains("login codex"));
    interrupt(child.id());
    let o = child.wait_with_output().unwrap();
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("✓ Codex: logged in"), "{t}");
}
