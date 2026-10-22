//! M5 end-to-end tests: the scriptable control plane. A shell script drives a whole task with
//! `ptyro` commands only (the milestone's Done-when); `send`/`wait`/`read`/`events` details;
//! stable exit codes; the `--json` shapes (snapshots); completions; `doctor` with its hook
//! self-test; `install-service`; `init`; other tools' committed config; and the TUI's broadcast
//! and tour.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::Script;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

fn agents_config(extra: &str) -> String {
    let mut c = String::from("[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n");
    for a in ["claude", "codex"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
        ));
    }
    c.push_str(extra);
    c
}

impl World {
    fn new(extra_config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m5-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(&agents_config(extra_config))).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("ptyro {args:?}: {e}\n{out}"))
    }

    fn code(&self, args: &[&str]) -> i32 {
        self.cli(args).status.code().unwrap_or(-1)
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repo cloned from a bare "remote", with `files` committed.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        for (f, text) in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        root
    }

    fn tui(&self, args: &[&str]) -> Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = Script::new(argv)
            .size(110, 24)
            .timeout(8.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }
}

/// The milestone's Done-when: a shell script creates a worktree, starts Claude with a prompt,
/// waits for it to go idle, reads its final screen, runs the tests and removes the worktree,
/// using only `ptyro` commands; meanwhile `ptyro events --follow` sees all of it live.
#[test]
fn done_when_a_shell_script_drives_a_whole_task() {
    let w = World::new("");
    let root = w.repo(
        "app",
        &[(
            ".ptyro.toml",
            "[scripts.run.test]\ncommand = \"test -f README.md && echo tests-pass\"\n",
        )],
    );
    let mut follow =
        w.d.cmd()
            .args(["events", "--follow"])
            .env("HOME", w.home())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
    let script = r#"
set -eu
cd "$REPO"
ptyro project add >/dev/null 2>&1
ptyro project trust --yes >/dev/null 2>&1
ptyro wt new fixit >/dev/null 2>&1
ptyro new -d -n fixer --agent claude --wt app/fixit --prompt "work 0.5 fixed the bug" >/dev/null 2>&1
ptyro wait fixer --for idle --timeout 30s
ptyro read fixer | grep -q "worked: fixed the bug"
ptyro wt run app/fixit test >/dev/null 2>&1
ptyro wait fixit.test --for exit --timeout 30s >/dev/null
ptyro read fixit.test | grep -q tests-pass
ptyro stop fixer >/dev/null 2>&1
ptyro wt rm app/fixit --stop-sessions --force --yes >/dev/null 2>&1
echo all-done
"#;
    let bin_dir = Path::new(BIN).parent().unwrap().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .env("REPO", &root)
        .env("HOME", w.home())
        .env("PATH", format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default()));
    for (k, v) in w.d.env() {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("fixer idle") && stdout.contains("all-done"),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!root.parent().unwrap().join("app.worktrees/fixit").exists());
    let _ = follow.kill();
    let live = follow.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&live.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["event"]["t"].as_str()).collect();
    for k in ["worktree_updated", "session_created", "session_updated", "worktree_removed"] {
        assert!(kinds.contains(&k), "events --follow saw no {k}: {kinds:?}");
    }
    assert!(lines.iter().all(|l| l["at_ms"].as_u64().is_some()));
}

#[test]
fn send_wait_read_and_events() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    w.ok(&["new", "-d", "-n", "a2", "--agent", "codex"]);
    // Ready, typed, turn over: one command.
    let out = w.json(&["send", "a1", "a2", "work 0.3 hi", "--wait-ready", "--wait", "--json"]);
    let states: Vec<&str> =
        out.as_array().unwrap().iter().map(|o| o["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["idle", "idle"], "{out}");
    assert!(out[0]["turns"].as_u64() >= Some(1));
    // A turn that ends blocked says so.
    let blocked = w.ok(&["send", "a1", "ask rm -rf build", "--wait"]);
    assert!(blocked.contains("a1 blocked"), "{blocked}");
    w.ok(&["answer", "a1", "allow"]);
    // --match on the screen, --any across sessions.
    let m = w.ok(&["wait", "a1", "--match", "answer: (yes|no)", "--timeout", "10s"]);
    assert!(m.contains("a1 matched \"answer: yes\""), "{m}");
    let any = w.ok(&["wait", "a1", "a2", "--for", "idle", "--any"]);
    assert!(any.contains("idle"), "{any}");
    // A stalled turn: working but silent.
    w.ok(&["send", "a2", "work 5 slow"]);
    let stall = w.cli(&["wait", "a2", "--for", "idle", "--stall", "1s", "--timeout", "20s"]);
    assert_eq!(stall.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&stall.stdout).contains("a2 stalled"));
    // read --json carries status and turns; --scrollback works with it.
    let r = w.json(&["read", "a1", "--json"]);
    assert_eq!(r["view"], "screen");
    assert!(r["text"].as_str().unwrap().contains("fake claude"));
    assert_eq!(w.json(&["read", "a1", "--scrollback", "--json"])["view"], "scrollback");
    // Persisted events, oldest first, one JSON per line.
    let ev = w.ok(&["events", "-s", "a1", "--json", "-n", "200"]);
    let kinds: Vec<String> = ev
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["kind"].as_str().unwrap().to_owned())
        .collect();
    for k in ["status", "prompt", "answer"] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing from {kinds:?}");
    }
    // broadcast and stop.
    let b = w.cli(&["broadcast", "a1", "a2", "work 0.1 both"]);
    let berr = String::from_utf8_lossy(&b.stderr);
    assert!(
        b.status.success() && berr.contains("sent to a1") && berr.contains("sent to a2"),
        "{berr}"
    );
    w.ok(&["stop", "a1"]);
    let gone = w.json(&["ls", "--json", "--status", "exited"]);
    assert_eq!(gone.as_array().unwrap().len(), 1, "{gone}");
    assert_eq!(gone[0]["name"], "a1", "stop keeps it listed");
}

#[test]
fn exit_codes_are_stable() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "e1", "--", "sleep", "60"]);
    w.ok(&["new", "-d", "-n", "e2", "--", "true"]);
    assert_eq!(w.code(&["frobnicate"]), 2, "usage");
    assert_eq!(w.code(&["read", "nope"]), 3, "not found");
    assert_eq!(w.code(&["read", "e*"]), 4, "ambiguous");
    assert_eq!(w.code(&["wait", "e1", "--for", "blocked", "--timeout", "1s"]), 7, "timeout");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(w.code(&["wait", "e2", "--for", "idle", "--timeout", "5s"]), 5, "exited first");
    assert_eq!(w.code(&["wait", "e2", "--for", "exit"]), 0);
    let mut lonely = w.d.cmd();
    let none = tempfile::tempdir().unwrap();
    lonely.env("PTYRO_HOME", none.path()).args(["read", "x"]);
    assert_eq!(lonely.output().unwrap().status.code(), Some(8), "no daemon");
}

/// The shape of a JSON value: keys and types, values dropped (so snapshots don't churn).
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => match a.first() {
            Some(x) => Value::Array(vec![shape(x)]),
            None => Value::Array(vec![]),
        },
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "bool".into(),
        Value::Null => Value::Null,
    }
}

/// `--json` output is an interface (docs/cli-json.md): its shape is pinned by snapshots.
/// `make snapshots` accepts intended changes.
#[test]
fn json_shapes() {
    let w = World::new("");
    let root = w.repo("proj", &[(".ptyro.toml", "[scripts.run.dev]\ncommand = \"true\"\n")]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["new", "-d", "-n", "j1", "--agent", "claude", "--prompt", "work 0.1 x"]);
    w.ok(&["wait", "j1", "--for", "idle", "--timeout", "20s"]);
    let mut all = serde_json::Map::new();
    for (name, args) in [
        ("ls", vec!["ls", "--json"]),
        ("status", vec!["status", "j1", "--json"]),
        ("read", vec!["read", "j1", "--json"]),
        ("wait", vec!["wait", "j1", "--json"]),
        ("project_ls", vec!["project", "ls", "--json"]),
        ("ws_ls", vec!["ws", "ls", "--json"]),
        ("wt_ls", vec!["wt", "ls", "--json"]),
        ("profile_ls", vec!["profile", "ls", "--json"]),
        ("inbox", vec!["inbox", "--json"]),
        ("daemon_status", vec!["daemon", "status", "--json"]),
        ("integrations", vec!["integrations", "ls", "--json"]),
        ("config_check", vec!["config", "check", "--json"]),
    ] {
        all.insert(name.into(), shape(&w.json(&args)));
    }
    // A status event (kinds differ in their detail).
    let ev: Value = w
        .ok(&["events", "-s", "j1", "--json", "-n", "200"])
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|e| e["kind"] == "status")
        .expect("a status event");
    all.insert("events".into(), shape(&ev));
    insta::assert_json_snapshot!("cli-json", Value::Object(all));
}

#[test]
fn completions_are_live() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "comp-target", "--", "sleep", "30"]);
    for shell in ["bash", "zsh", "fish"] {
        let reg = w.ok(&["completions", shell]);
        assert!(reg.contains("PTYRO_COMPLETE"), "{shell}: {reg}");
    }
    assert_eq!(w.code(&["completions", "tcsh"]), 2);
    let out =
        w.d.cmd()
            .env("PTYRO_COMPLETE", "fish")
            .args(["--", "ptyro", "attach", "comp"])
            .output()
            .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("comp-target"), "live session names complete: {text}");
    let agents =
        w.d.cmd()
            .env("PTYRO_COMPLETE", "fish")
            .args(["--", "ptyro", "new", "--agent", "cl"])
            .output()
            .unwrap();
    assert!(String::from_utf8_lossy(&agents.stdout).contains("claude"));
}

#[test]
fn doctor_self_test_and_service() {
    let w = World::new("");
    let report = w.cli(&["doctor", "--json"]);
    let checks: Value = serde_json::from_slice(&report.stdout).unwrap();
    let find = |s: &str, n: &str| {
        checks.as_array().unwrap().iter().find(|c| c["section"] == s && c["name"] == n).cloned()
    };
    for a in ["claude", "codex", "opencode"] {
        let c = find("integrations", a).unwrap_or_else(|| panic!("no {a} self-test: {checks}"));
        assert_eq!(c["level"], "ok", "{c}");
    }
    assert_eq!(find("daemon", "running").unwrap()["level"], "ok");
    let quick: Value =
        serde_json::from_slice(&w.cli(&["doctor", "--quick", "--json"]).stdout).unwrap();
    assert!(!quick.to_string().contains("self-test"));
    assert!(w.ok(&["profile", "doctor", "--json"]).trim_start().starts_with('['));
    // The service: --dry-run prints the unit; a real install calls systemctl.
    let unit = w.ok(&["daemon", "install-service", "--dry-run"]);
    if cfg!(target_os = "linux") {
        assert!(
            unit.contains("ExecStart=") && unit.contains("daemon start --foreground"),
            "{unit}"
        );
        assert!(unit.contains("Environment=\"PATH="), "{unit}");
        let fake = w.home().join("fakebin");
        std::fs::create_dir_all(&fake).unwrap();
        let log = w.home().join("systemctl.log");
        std::fs::write(
            fake.join("systemctl"),
            format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fake.join("systemctl"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let xdg = w.home().join("xdgconf");
        let path = format!("{}:{}", fake.display(), std::env::var("PATH").unwrap());
        let run = |args: &[&str]| {
            w.d.cmd().args(args).env("PATH", &path).env("XDG_CONFIG_HOME", &xdg).output().unwrap()
        };
        assert!(run(&["daemon", "install-service"]).status.success());
        assert!(xdg.join("systemd/user/ptyro.service").exists());
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("--user daemon-reload") && calls.contains("--user enable ptyro.service"),
            "{calls}"
        );
        assert!(!calls.contains("start"), "a daemon runs already: never started a second: {calls}");
        assert!(run(&["daemon", "uninstall-service"]).status.success());
        assert!(!xdg.join("systemd/user/ptyro.service").exists());
    }
}

#[test]
fn init_detects_and_writes_only_whats_missing() {
    let w = World::new("");
    let h = w.home();
    std::fs::create_dir_all(h.join(".claude-work")).unwrap();
    std::fs::write(
        h.join(".zshrc"),
        "claude-work() {\n  CLAUDE_CONFIG_DIR=\"$HOME/.claude-work\" claude \"$@\"\n}\n",
    )
    .unwrap();
    let dry = w.ok(&["init", "--dry-run"]);
    assert!(
        dry.contains("[profiles.work]") && dry.contains("claude = \"~/.claude-work\""),
        "{dry}"
    );
    let out = w.cli(&["init", "--yes"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let config = std::fs::read_to_string(
        w.d.env()
            .iter()
            .find(|(k, _)| k == "PTYRO_HOME")
            .map(|(_, v)| PathBuf::from(v).join("config/config.toml"))
            .unwrap(),
    )
    .unwrap();
    assert!(config.contains("[agents.claude]"), "kept what was there: {config}");
    assert!(config.contains("[profiles.work]"), "{config}");
    // A second run finds nothing to add.
    let again = w.cli(&["init", "--yes"]);
    assert!(String::from_utf8_lossy(&again.stderr).contains("nothing to add"));
    assert_eq!(w.code(&["init"]), 2, "no terminal and no --yes: refuses to guess");
}

#[test]
fn other_tools_config_becomes_scripts() {
    let w = World::new("");
    let root = w.repo(
        "imp",
        &[
            (".conductor/settings.toml", "[scripts]\nsetup = \"echo setup-from-conductor $CONDUCTOR_WORKSPACE_NAME > SETUP_RAN\"\n[scripts.run.web]\ncommand = \"echo web on $CONDUCTOR_PORT\"\ndefault = true\n"),
            (".cursor/environment.json", "{ \"install\": \"echo never\", \"terminals\": [{\"name\": \"worker\", \"command\": \"echo worker\"}] }"),
        ],
    );
    w.ok(&["project", "add", root.to_str().unwrap()]);
    let explain = w.ok(&["project", "config", "imp", "--explain"]);
    assert!(
        explain.contains("scripts.setup") && explain.contains(".conductor/settings.toml"),
        "{explain}"
    );
    assert!(
        explain.contains("scripts.run.worker") && explain.contains(".cursor/environment.json"),
        "{explain}"
    );
    assert!(
        explain.contains("setup not used"),
        "cursor's install loses to conductor's setup: {explain}"
    );
    let j = w.json(&["project", "config", "imp", "--json"]);
    assert_eq!(j["origin"]["scripts.run.web"], ".conductor/settings.toml");
    // Untrusted until reviewed, like any script.
    let refused = w.cli(&["wt", "new", "one", "--project", "imp", "--json"]);
    assert!(refused.status.success() || refused.status.code() == Some(6));
    w.ok(&["project", "trust", "imp", "--yes"]);
    w.ok(&["wt", "new", "two", "--project", "imp"]);
    let wt = w.json(&["wt", "status", "imp/two", "--json"]);
    let path = PathBuf::from(wt["path"].as_str().unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !path.join("SETUP_RAN").exists() {
        assert!(std::time::Instant::now() < deadline, "the imported setup never ran");
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        std::fs::read_to_string(path.join("SETUP_RAN")).unwrap().trim(),
        "setup-from-conductor two",
        "CONDUCTOR_* aliases set"
    );
}

#[tokio::test]
async fn tui_broadcast_and_tour() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "b1", "--agent", "claude"]);
    w.ok(&["new", "-d", "-n", "b2", "--agent", "claude"]);
    w.ok(&["wait", "b1", "b2", "--for", "idle", "--timeout", "15s"]);
    let r = ptyro_testkit::headless::run(
        w.tui(&["attach", "b1"])
            .snapshot(1.2)
            // Sidebar, mark both, b, type, enter.
            .send(1.4, "\\x11\\t")
            .send(1.7, " ")
            .send(1.9, "j")
            .send(2.1, " ")
            .snapshot(2.4)
            .send(2.6, "b")
            .snapshot(3.0)
            .send(3.2, "work 0.1 broadcasted")
            .send(3.5, "\\r")
            .snapshot(4.6)
            .send(4.8, "\\x11d"),
    )
    .await
    .unwrap();
    assert!(r.at(2.4).matches('◆').count() == 2, "both marked: {}", r.at(2.4));
    assert!(r.at(3.0).contains("Broadcast to b1, b2"), "{}", r.at(3.0));
    for s in ["b1", "b2"] {
        let screen = w.ok(&["read", s]);
        assert!(screen.contains("worked: broadcasted"), "{s}: {screen}");
    }
    let tour = ptyro_testkit::headless::run(
        w.tui(&["tour"])
            .snapshot(1.2)
            .send(1.4, "\\r")
            .snapshot(1.8)
            .send(2.0, "q")
            .snapshot(2.3)
            .send(2.5, "\\x11d"),
    )
    .await
    .unwrap();
    assert!(
        tour.at(1.2).contains("Tour: welcome") && tour.at(1.2).contains("1/6"),
        "{}",
        tour.at(1.2)
    );
    assert!(tour.at(1.8).contains("Tour: sessions"), "{}", tour.at(1.8));
    assert!(!tour.at(2.3).contains("Tour:"), "q leaves the tour");
}
