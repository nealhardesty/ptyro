//! A live upgrade requested while a session is still starting (a profile's secret command can
//! take seconds): the upgrade waits for the launch, the new session is in the new daemon, and no
//! second daemon is left behind. And a rolled-back upgrade does not stop the next one.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str, daemon_env: &[(&str, &str)]) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-race-").tempdir().unwrap();
        let hs = home.path().canonicalize().unwrap().to_string_lossy().into_owned();
        let mut env = vec![("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        env.extend_from_slice(daemon_env);
        let d = TempDaemon::start_with(BIN, &env, Some(extra)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn sessions(&self) -> Vec<Value> {
        self.json(&["ls", "--json"]).as_array().unwrap().clone()
    }

    /// Daemon processes that run on this world's home (the old image would show up here).
    fn daemon_pids(&self) -> Vec<u32> {
        let home = self.d.paths.state.to_string_lossy().into_owned();
        let ps = std::process::Command::new("ps").args(["-axo", "pid=,command="]).output().unwrap();
        String::from_utf8_lossy(&ps.stdout)
            .lines()
            .filter(|l| l.contains(" daemon start") && l.contains(&home))
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect()
    }
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const SLOW: &str = r#"
[profiles.slow]
secrets = { SLOW_TOKEN = { command = ["sh", "-c", "sleep 2; echo tok"] } }
"#;

/// The launch is in flight when the upgrade is asked for. The upgrade waits for it; the session
/// ends up in the new daemon, alive, and the same daemon pid is the only one running.
#[test]
fn an_upgrade_during_a_slow_launch_waits_and_loses_nothing() {
    let w = World::new(SLOW, &[]);
    let before = w.json(&["daemon", "status", "--json"]);
    let launch =
        w.d.cmd()
            .args([
                "new",
                "-d",
                "-n",
                "slow",
                "--profile",
                "slow",
                "--",
                "sh",
                "-c",
                "echo up; sleep 60",
            ])
            .env("HOME", w.home())
            .current_dir(w.home())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    // Let the daemon take the launch's claim (its secret command is running now).
    std::thread::sleep(Duration::from_millis(700));
    let started = Instant::now();
    let up = w.cli(&["daemon", "upgrade", "--json"]);
    let waited = started.elapsed();
    let launched = launch.wait_with_output().unwrap();

    // Either outcome is allowed by the documented contract, but never a success that lost the
    // session the caller was told about.
    let upgraded = up.status.success();
    let created = launched.status.success();
    if upgraded {
        let up: Value = serde_json::from_slice(&up.stdout).unwrap();
        assert_eq!(up["rolled_back"], false, "{up}");
        assert_eq!(up["pid"], before["pid"]);
    } else {
        let msg = String::from_utf8_lossy(&up.stderr).into_owned();
        assert!(msg.contains("still starting"), "an unexplained failure: {msg}");
    }
    let live = w.sessions();
    let slow = live.iter().find(|s| s["name"] == "slow");
    assert_eq!(
        slow.is_some(),
        created,
        "launch {created}, sessions {live:?}; stderr {}",
        String::from_utf8_lossy(&launched.stderr)
    );
    if upgraded {
        assert!(
            created,
            "the upgrade lost the launch: {}",
            String::from_utf8_lossy(&launched.stderr)
        );
        assert!(waited >= Duration::from_millis(900), "the upgrade did not wait: {waited:?}");
        assert_eq!(w.json(&["daemon", "status", "--json"])["generation"], 1);
    }
    if let Some(s) = slow {
        let pid = s["pid"].as_u64().expect("a pid") as u32;
        assert!(alive(pid), "the session's process died");
        let deadline = Instant::now() + WAIT;
        loop {
            let t = String::from_utf8_lossy(&w.cli(&["read", "slow"]).stdout).into_owned();
            if t.contains("up") {
                break;
            }
            assert!(Instant::now() < deadline, "the session never printed: {t}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // One daemon only: the old image is gone.
    let daemons = w.daemon_pids();
    let daemon = before["pid"].as_u64().unwrap() as u32;
    assert!(daemons.iter().all(|p| *p == daemon), "a stray daemon: {daemons:?} (daemon {daemon})");
}

/// A refused handoff rolls back to `daemon.prev`; the daemon then runs from that very file, so
/// the next upgrade must not copy it onto itself, and goes through.
#[test]
fn an_upgrade_after_a_rollback_works() {
    let w = World::new("", &[("PTYRO_TEST_REFUSE_HANDOFF", "1")]);
    assert!(
        w.cli(&["new", "-d", "-n", "keep", "--", "sh", "-c", "echo alive; sleep 60"])
            .status
            .success()
    );
    let pid = w.json(&["daemon", "status", "--json"])["pid"].clone();

    let first = w.cli(&["daemon", "upgrade", "--json"]);
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["rolled_back"], true, "{first}");
    let prev = w.d.paths.state.join("daemon.prev");
    let len = std::fs::metadata(&prev).expect("daemon.prev exists").len();
    assert!(len > 0);

    // A target that does not refuse: the real binary, without the test knob.
    let wrapper = w.home().join("ptyro-new");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nunset PTYRO_TEST_REFUSE_HANDOFF\nexec '{BIN}' \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let second = w.cli(&["daemon", "upgrade", "--binary", wrapper.to_str().unwrap(), "--json"]);
    assert!(
        second.status.success(),
        "the second upgrade: {}{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["rolled_back"], false, "{second}");
    assert_eq!(second["pid"], pid);
    assert_eq!(std::fs::metadata(&prev).unwrap().len(), len, "daemon.prev is still the same file");
    let keep =
        w.sessions().into_iter().find(|s| s["name"] == "keep").expect("the session survived");
    assert!(alive(keep["pid"].as_u64().unwrap() as u32));
}
