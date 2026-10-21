//! M8 end-to-end tests: hardening. The Done-when (a daemon upgrade during active agent work is
//! invisible to the agents: same pid, same sessions, the turn's Stop hook lands on the new daemon,
//! a waiting script never notices), rollback when the new binary refuses, the recorder (export,
//! replay, history deeper than the in-memory scrollback, dormant screens after a restart), frame
//! compression and the health metrics.

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_client::{Client, ConnectOptions};
use ptyro_protocol::{ClientKind, Reply, Request, ScreenFormat};
use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);
const TICKER: &str = "i=0; while :; do i=$((i+1)); echo tick $i; sleep 0.1; done";

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        Self::with_env(extra, &[])
    }

    fn with_env(extra: &str, daemon_env: &[(&str, &str)]) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m8-").tempdir().unwrap();
        let hs = home.path().canonicalize().unwrap().to_string_lossy().into_owned();
        let mut env = vec![("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        env.extend_from_slice(daemon_env);
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n{extra}"
        );
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
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

    fn status(&self) -> Value {
        self.json(&["daemon", "status", "--json"])
    }

    fn wait_screen(&self, s: &str, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let t = self.ok(&["read", s]);
            if t.contains(needle) {
                return t;
            }
            assert!(Instant::now() < deadline, "{s} never showed {needle:?}:\n{t}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The highest `tick N` on the ticker's screen.
    fn ticks(&self, s: &str) -> u64 {
        self.ok(&["read", s])
            .lines()
            .filter_map(|l| l.strip_prefix("tick ")?.trim().parse().ok())
            .max()
            .unwrap_or(0)
    }

    fn ids(&self) -> Vec<(String, String, String)> {
        let mut v: Vec<_> = self
            .json(&["ls", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let f = |k: &str| s[k].as_str().unwrap_or_default().to_owned();
                (f("name"), f("id"), s["state"]["t"].as_str().unwrap_or_default().to_owned())
            })
            .collect();
        v.sort();
        v
    }
}

/// The Done-when: an upgrade in the middle of an agent's turn. The daemon keeps its pid, every
/// session keeps its id, process and screen; the turn's Stop hook reaches the new daemon, so the
/// agent goes idle as usual; a script blocked in `ptyro wait` reconnects and sees it; a plain
/// program keeps printing; a child that exits after the upgrade has its exit code captured.
#[test]
fn done_when_an_upgrade_mid_turn_is_invisible() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    w.ok(&["new", "-d", "-n", "tick", "--", "sh", "-c", TICKER]);
    w.ok(&["new", "-d", "-n", "ex", "--", "sh", "-c", "echo ready; read x; exit $x"]);
    w.wait_screen("ex", "ready");
    w.ok(&["send", "a1", "work 3 during the upgrade", "--wait-ready"]);
    w.wait_screen("a1", "work 3");
    let waiter =
        w.d.cmd()
            .args(["wait", "a1", "--for", "idle", "--timeout", "40s"])
            .env("HOME", w.home())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    let before = w.status();
    let ids = w.ids();
    let ticks = w.ticks("tick");
    assert_eq!(before["generation"], 0);

    let up = w.json(&["daemon", "upgrade", "--json"]);
    assert_eq!(up["rolled_back"], false, "{up}");
    assert_eq!(up["pid"], before["pid"], "re-exec keeps the pid");
    assert_eq!(up["sessions"], 3);

    let after = w.status();
    assert_eq!(after["pid"], before["pid"]);
    assert_eq!(after["generation"], 1, "{after:#}");
    assert_eq!(after["started_ms"], before["started_ms"], "the daemon's identity carries over");
    assert!(after["upgraded_ms"].as_u64().unwrap_or(0) > 0);
    assert_eq!(w.ids(), ids, "the same sessions, ids and states");
    // The agent's screen came across, and its turn finishes on the new daemon.
    assert!(w.ok(&["read", "a1"]).contains("fake claude"));
    let out = waiter.wait_with_output().unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("idle"),
        "wait across the upgrade: {:?} {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let st = w.json(&["status", "a1", "--json"]);
    assert!(st["session"]["turns"].as_u64() >= Some(1), "{st:#}");
    w.wait_screen("a1", "worked");
    // The plain program never stopped.
    let deadline = Instant::now() + WAIT;
    while w.ticks("tick") <= ticks + 5 {
        assert!(Instant::now() < deadline, "the ticker stopped at {}", w.ticks("tick"));
        std::thread::sleep(Duration::from_millis(100));
    }
    // Input reaches an adopted child, and its exit is reaped with the code.
    w.ok(&["send", "ex", "4"]);
    let deadline = Instant::now() + WAIT;
    loop {
        let s = w.json(&["status", "ex", "--json"])["session"].clone();
        if s["state"]["t"] == "exited" {
            assert_eq!(s["state"]["code"], 4, "{s:#}");
            break;
        }
        assert!(Instant::now() < deadline, "ex never exited: {s:#}");
        std::thread::sleep(Duration::from_millis(100));
    }
    // A second upgrade works from an upgraded daemon.
    let up2 = w.json(&["daemon", "upgrade", "--json"]);
    assert_eq!(up2["pid"], before["pid"]);
    assert_eq!(w.status()["generation"], 2);
}

/// A new binary that refuses the image hands it back: the previous binary takes over again
/// (still the same pid), every session survives, and the command says it rolled back.
#[test]
fn a_refused_handoff_rolls_back() {
    let w = World::with_env("", &[("PTYRO_TEST_REFUSE_HANDOFF", "1")]);
    w.ok(&["new", "-d", "-n", "tick", "--", "sh", "-c", TICKER]);
    w.wait_screen("tick", "tick 3");
    let pid = w.status()["pid"].clone();
    let out = w.cli(&["daemon", "upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let up: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(up["rolled_back"], true, "{up}");
    let st = w.status();
    assert_eq!(st["pid"], pid);
    assert_eq!(st["rolled_back"], true);
    let t = w.ticks("tick");
    let deadline = Instant::now() + WAIT;
    while w.ticks("tick") <= t + 3 {
        assert!(Instant::now() < deadline, "the ticker stopped after the rollback");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The recorder: every byte a session printed, beyond the in-memory scrollback, exported as
/// asciicast or text, replayed, and used to show a dormant session's last screen after a restart.
#[test]
fn the_recorder_keeps_everything() {
    let w = World::new("[daemon]\nscrollback_lines = 200\n");
    w.ok(&["new", "-d", "-n", "s", "--", "sh", "-c", "seq 1 3000; echo all-done; sleep 600"]);
    w.wait_screen("s", "all-done");
    // In memory: only the last 200 lines. Asking for more reads the recording.
    let short = w.ok(&["read", "s", "--scrollback", "100"]);
    assert!(!short.lines().any(|l| l == "1"), "{short}");
    let deep = w.ok(&["read", "s", "--scrollback", "5000"]);
    let lines: Vec<&str> = deep.lines().collect();
    assert!(
        lines.contains(&"1") && lines.contains(&"1500") && lines.contains(&"3000"),
        "deep history"
    );

    let text = w.ok(&["export", "s", "--format", "text"]);
    assert!(
        text.starts_with("1\n2\n3\n") && text.contains("3000\nall-done"),
        "{}",
        &text[..80.min(text.len())]
    );
    let cast = w.home().join("s.cast");
    w.ok(&["export", "s", "-o", cast.to_str().unwrap()]);
    let cast = std::fs::read_to_string(cast).unwrap();
    let header: Value = serde_json::from_str(cast.lines().next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    assert!(header["width"].as_u64() > Some(0));
    let ev: Value = serde_json::from_str(cast.lines().nth(1).unwrap()).unwrap();
    assert_eq!(ev[1], "o");
    let replay = w.ok(&["replay", "s", "--speed", "1000", "--max-idle", "1s"]);
    assert!(replay.contains("all-done"));

    // Recordings are private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sessions = w.d.paths.data.join("sessions");
        let dir = std::fs::read_dir(&sessions).unwrap().flatten().next().unwrap().path();
        let log = dir.join("pty.log");
        assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }

    // A restart ends the processes; a dormant agent still shows its last screen.
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    w.ok(&["send", "a1", "work 0.1 remember-me", "--wait-ready", "--wait"]);
    let mut w = w;
    w.d.restart().unwrap();
    let a1 = w.json(&["status", "a1", "--json"]);
    assert_eq!(a1["session"]["state"]["t"], "dormant", "{a1:#}");
    let screen = w.ok(&["read", "a1"]);
    assert!(screen.contains("remember-me"), "dormant screen from the recording:\n{screen}");
}

/// A session can opt out of recording.
#[test]
fn recording_can_be_turned_off() {
    let w = World::new("[recorder]\nenabled = false\n");
    w.ok(&["new", "-d", "-n", "q", "--", "sh", "-c", "echo quiet; sleep 600"]);
    w.wait_screen("q", "quiet");
    let out = w.cli(&["export", "q", "--format", "text"]);
    assert!(!out.status.success(), "nothing recorded, so nothing to export");
}

/// zstd frames when the client asks (`PTYRO_COMPRESS=1`), and the daemon's health in status.
#[tokio::test]
async fn compressed_frames_and_health_metrics() {
    let w = World::new("");
    w.ok(&[
        "new",
        "-d",
        "-n",
        "big",
        "--size",
        "200x50",
        "--",
        "sh",
        "-c",
        "seq 1 100000 | paste -sd ' ' - | fold -w 199; sleep 600",
    ]);
    let mut opts = ConnectOptions::new(w.d.paths.clone(), ClientKind::Test);
    opts.autospawn = false;
    opts.compress = true;
    let c = Client::connect(&opts).await.unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let r = c
            .request(Request::ReadScreen { addr: "big".into(), format: ScreenFormat::Plain })
            .await
            .unwrap();
        let Reply::Screen { text } = r else { panic!("{r:?}") };
        if text.contains("100000") {
            break;
        }
        assert!(Instant::now() < deadline, "{text}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let st = w.status();
    let m = &st["metrics"];
    assert!(m["rss_kib"].as_u64() > Some(1000), "{st:#}");
    assert!(m["open_fds"].as_u64() > Some(3));
    assert!(m["bytes_read"].as_u64() > Some(500_000));
    assert_eq!(m["crashes"], 0);
    assert_eq!(m["pty_readers"], 1);
    let text = w.ok(&["daemon", "status"]);
    assert!(text.contains("rss") || text.contains("RSS") || text.contains("memory"), "{text}");
}

/// A request on a connection that is already gone fails at once (it used to wait forever when
/// the connection dropped before the request was made: the soak caught a `send` hung for good
/// across an upgrade).
#[tokio::test]
async fn a_request_after_the_daemon_left_fails_fast() {
    let mut w = World::new("");
    let c = w.d.client().await.unwrap();
    w.d.crash();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = tokio::time::timeout(Duration::from_secs(5), c.request(Request::Status)).await;
    assert!(matches!(r, Ok(Err(_))), "{r:?}");
}

/// A `send --wait-ready --wait` that is waiting inside the daemon (the agent is blocked on a
/// question) when an upgrade happens picks up on the new daemon: once the question is answered,
/// the prompt is typed exactly once and the command reports the end of that turn.
#[test]
fn a_send_waiting_for_ready_survives_an_upgrade() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "a2", "--agent", "claude"]);
    let blocked = w.ok(&["send", "a2", "ask rm -rf build", "--wait-ready", "--wait"]);
    assert!(blocked.contains("a2 blocked"), "{blocked}");
    let send =
        w.d.cmd()
            .args([
                "send",
                "a2",
                "work 0.5 second-prompt",
                "--wait-ready",
                "60s",
                "--wait",
                "--timeout",
                "60s",
            ])
            .env("HOME", w.home())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    w.ok(&["daemon", "upgrade"]);
    // The hook's "blocked" outranks the output the redraw nudge causes, as before the upgrade.
    std::thread::sleep(Duration::from_millis(500));
    let st = w.json(&["status", "a2", "--json"])["session"]["status"].clone();
    assert_eq!(
        (st["status"].as_str(), st["source"].as_str()),
        (Some("blocked"), Some("hook")),
        "{st}"
    );
    w.ok(&["answer", "a2", "allow"]);
    let out = send.wait_with_output().unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("a2 idle"),
        "{:?} {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = w.wait_screen("a2", "worked: second-prompt");
    let all = w.ok(&["read", "a2", "--scrollback", "500"]);
    assert_eq!(all.matches("work 0.5 second-prompt").count(), 1, "typed once:\n{all}");
    assert!(text.contains("worked: second-prompt"));
}
