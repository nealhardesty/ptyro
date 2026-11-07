//! The TUI's setup checklist (ROADMAP BOOT2) end to end: `ptyro` in a PTY (`ptyro_testkit::
//! headless`), on the fake machine `PTYRO_TEST_SETUP` names (BOOT1). The checklist is the first
//! part of the first run, comes back on its key, and installs in a split that runs the CLI's own
//! `ptyro install … --confirmed`: live output, administrator rights asked once, the logins, the
//! profiles line. Nothing real is installed or logged in, and the network is never used.

use std::path::{Path, PathBuf};

use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::{Script, run};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// A fake Linux with apt, sudo and curl (merged with `extra`), its home, and a daemon whose
/// sessions (the TUI's splits) see the same fake machine.
struct World {
    dir: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot2-").tempdir().unwrap();
        for d in ["fake", "home"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let mut w = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"] });
        w.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        std::fs::write(dir.path().join("fake/world.json"), w.to_string()).unwrap();
        let (fake, home) = (path(&dir.path().join("fake")), path(&dir.path().join("home")));
        let d = TempDaemon::start_with_env(
            BIN,
            &[("PTYRO_TEST_SETUP", &fake), ("HOME", &home), ("SHELL", "/bin/sh")],
        )
        .unwrap();
        Self { dir, d }
    }

    fn fake(&self) -> PathBuf {
        self.dir.path().join("fake")
    }

    /// `ptyro` itself, in a PTY, as a person would start it.
    fn tui(&self) -> Script {
        self.ptyro(&[])
    }

    /// `ptyro ARGS…` in a PTY.
    fn ptyro(&self, args: &[&str]) -> Script {
        let argv = std::iter::once(BIN).chain(args.iter().copied()).map(str::to_owned).collect();
        let mut s = Script::new(argv)
            .size(140, 40)
            .timeout(90.0)
            .env("PTYRO_TEST_SETUP", &path(&self.fake()))
            .env("HOME", &path(&self.dir.path().join("home")))
            .env("SHELL", "/bin/sh");
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }

    fn installed(&self, id: &str) -> bool {
        self.fake().join("installed").join(id).exists()
    }

    fn log(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.fake().join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The first run opens with the checklist: the defaults ticked (git, here already, under
/// Already here); ⏎ installs them in a split, sudo asked once there, then each login and the
/// profiles line, and the first run's config is written as before.
#[tokio::test]
async fn the_first_run_installs_the_ticked_in_a_split_then_walks_the_logins() {
    let w = World::new(json!({ "installed": { "git": "2.43.0" }, "elevation": "prompt" }));
    let r = run(w.tui().send_when("✓ git", "\\r").until("More accounts")).await.unwrap();
    assert!(r.screen.contains("More accounts"), "{}", r.screen);
    for id in ["claude", "codex", "gh"] {
        assert!(w.installed(id), "{id} installed:\n{}", r.screen);
    }
    assert!(!w.installed("git"), "git was here already");
    let calls = w.log("calls.log");
    assert_eq!(calls.iter().filter(|c| *c == "elevate").count(), 1, "admin asked once: {calls:?}");
    let logins: Vec<String> =
        w.log("logins.log").into_iter().filter(|l| l.starts_with("login ")).collect();
    assert_eq!(logins, ["login claude", "login codex", "login gh"], "each login, in turn");
    assert!(w.d.paths.config.join("config.toml").exists(), "the first run wrote its config");
}

/// While the first run's install still runs (Claude Code's installer is slow here), `U`
/// doesn't open a second checklist or start a second install: it shows the split and says
/// setup is still running. One install split runs, to the end, and each item installs once.
#[tokio::test]
async fn a_running_install_is_never_joined_by_a_second() {
    let w = World::new(json!({
        "installed": { "git": "2.43.0" },
        "outcomes": { "claude": "slow" },
    }));
    let r = run(w
        .tui()
        .send_when("✓ git", "\\r")
        .send_when("▸ Claude Code", "\\x11U")
        .until("setup is still running"))
    .await
    .unwrap();
    assert!(r.screen.contains("setup is still running in ptyro-install"), "{}", r.screen);
    assert!(!r.screen.contains("Set up this computer"), "no second checklist:\n{}", r.screen);
    // The split carries on in the daemon (the TUI is gone now): wait for its last login.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !w.log("logins.log").iter().any(|l| l == "login gh") {
        assert!(std::time::Instant::now() < deadline, "the install never finished");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let c = w.d.client().await.unwrap();
    let ptyro_protocol::Reply::Sessions { sessions } =
        c.request(ptyro_protocol::Request::List).await.unwrap()
    else {
        panic!()
    };
    let splits: Vec<&str> = sessions
        .iter()
        .map(|s| s.name.as_str())
        .filter(|n| n.starts_with("ptyro-install"))
        .collect();
    assert_eq!(splits, ["ptyro-install"], "one install split, never a second");
    let installs: Vec<String> =
        w.log("calls.log").into_iter().filter(|c| c.starts_with("install ")).collect();
    for id in ["claude", "codex", "gh"] {
        assert_eq!(
            installs.iter().filter(|c| c.starts_with(&format!("install {id} "))).count(),
            1,
            "{id} installed once: {installs:?}"
        );
    }
}

/// Later, `U` opens the checklist alone (a config exists: no first run). What's here isn't
/// offered; Space ticks Antigravity, ⏎ installs just it, and its sign-in runs since it was just
/// installed. `s` (not now) closes it with nothing installed, saying how to come back.
#[tokio::test]
async fn the_checklist_comes_back_on_its_key_and_installs_what_is_ticked() {
    let all = json!({ "claude": "2.1.319", "codex": "0.162.1", "git": "2.43.0", "gh": "2.57.0" });
    let w = World::new(json!({ "installed": all, "logged_in": ["claude", "codex", "gh"] }));
    w.d.write_config("").unwrap();
    // Not now first: nothing runs.
    let r = run(w
        .tui()
        .send_when("▾ main", "\\x11U")
        .send_when("[ ] Antigravity", "s")
        .until("opens the setup checklist"))
    .await
    .unwrap();
    assert!(r.screen.contains("C-q U opens the setup checklist"), "{}", r.screen);
    assert!(w.log("calls.log").is_empty(), "nothing ran");
    // Then Antigravity: the cursor starts on it, the first row.
    let r = run(w
        .tui()
        .send_when("▾ main", "\\x11U")
        .send_when("[ ] Antigravity", " ")
        .send_when("[x] Antigravity", "\\r")
        .until("More accounts"))
    .await
    .unwrap();
    assert!(w.installed("antigravity"), "{}", r.screen);
    for id in ["claude", "codex", "opencode", "gemini", "node"] {
        assert!(!w.installed(id), "{id} wasn't ticked");
    }
    assert_eq!(w.log("calls.log").iter().filter(|c| c.starts_with("install")).count(), 1);
    assert!(
        w.log("logins.log").contains(&"login antigravity".to_owned()),
        "just installed: its sign-in"
    );
}

/// `ptyro init` offers the same checklist (the CLI's, in its terminal) in its agents step.
#[tokio::test]
async fn init_offers_the_setup_checklist() {
    let w = World::new(json!({}));
    let r = run(w
        .ptyro(&["init"])
        .send_when("open the setup checklist", "y\\r")
        .send_when("Enter to install", "q\\r")
        .until("Nothing installed."))
    .await
    .unwrap();
    assert!(r.screen.contains("[x] Claude Code"), "{}", r.screen);
    assert!(r.screen.contains("Nothing installed."), "{}", r.screen);
}
