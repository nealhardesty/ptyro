//! WIN9 end to end, on Windows itself: a live upgrade of a native daemon hands every session to
//! the new one (SPEC §4.5). There is no `execve` there, so the new daemon is another process: the
//! daemon's pid changes, and every session's program, screen and status carry on. A program keeps
//! printing through the upgrade, input reaches one waiting for it and its exit code comes back, an
//! agent's turn finishes on the new daemon (its hooks reach it), and a second upgrade works from
//! the first. A refused handover leaves the old daemon running with every session. CI's `windows`
//! job runs these (`make test-windows`).
#![cfg(windows)]

use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(40);
/// PowerShell printing a numbered line every 100 ms, forever.
const TICKER: &str =
    "$i = 0; while ($true) { $i++; Write-Output \"tick $i\"; Start-Sleep -Milliseconds 100 }";
/// PowerShell that says it's ready, then exits with the number typed into it.
const WAITER: &str = "Write-Output ready; $x = Read-Host; exit [int]$x";

fn agents_config() -> String {
    let mut c = String::from("[attention]\nmin_work = \"1s\"\n");
    c.push_str(&format!("[agents.claude]\ncommand = ['{BIN}', 'fake-agent', '--as', 'claude']\n"));
    c
}

fn daemon(env: &[(&str, &str)]) -> TempDaemon {
    TempDaemon::start_with(BIN, env, Some(&agents_config())).unwrap()
}

fn cli(d: &TempDaemon, args: &[&str]) -> Output {
    d.cmd().args(args).output().unwrap()
}

fn ok(d: &TempDaemon, args: &[&str]) -> String {
    let out = cli(d, args);
    assert!(
        out.status.success(),
        "ptyro {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(d: &TempDaemon, args: &[&str]) -> Value {
    serde_json::from_str(&ok(d, args)).unwrap_or_else(|e| panic!("{args:?}: {e}"))
}

fn status(d: &TempDaemon) -> Value {
    json(d, &["daemon", "status", "--json"])
}

fn ps(script: &str) -> Vec<&str> {
    vec!["--", "powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script]
}

fn new(d: &TempDaemon, name: &str, argv: &[&str]) {
    let mut a = vec!["new", "-d", "-n", name];
    a.extend_from_slice(argv);
    ok(d, &a);
}

fn wait_screen(d: &TempDaemon, name: &str, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let s = ok(d, &["read", name]);
        if s.contains(needle) {
            return s;
        }
        assert!(Instant::now() < deadline, "{name}: no {needle:?} in\n{s}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The highest `tick N` on the ticker's screen.
fn ticks(d: &TempDaemon, name: &str) -> u64 {
    ok(d, &["read", name])
        .split("tick ")
        .skip(1)
        .filter_map(|t| t.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Every session as (name, id, program pid, state).
fn sessions(d: &TempDaemon) -> Vec<(String, String, String, String)> {
    let v = json(d, &["ls", "--json"]);
    let mut out: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap_or_default().to_owned(),
                s["id"].as_str().unwrap_or_default().to_owned(),
                s["pid"].to_string(),
                s["state"]["t"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn an_upgrade_hands_every_session_over() {
    let d = daemon(&[]);
    new(&d, "tick", &ps(TICKER));
    new(&d, "ex", &ps(WAITER));
    ok(&d, &["new", "-d", "-n", "a1", "--agent", "claude"]);
    wait_screen(&d, "tick", "tick 3");
    wait_screen(&d, "ex", "ready");
    ok(&d, &["send", "a1", "work 4 during the upgrade", "--wait-ready"]);
    wait_screen(&d, "a1", "work 4");
    let before = status(&d);
    let held = sessions(&d);
    let t = ticks(&d, "tick");
    assert_eq!(before["generation"], 0);

    let up = json(&d, &["daemon", "upgrade", "--json"]);
    assert_eq!(up["rolled_back"], false, "{up}");
    assert_ne!(up["pid"], before["pid"], "the next daemon is another process on Windows");
    assert_eq!(up["sessions"], 3, "{up}");

    let after = status(&d);
    assert_eq!(after["generation"], 1, "{after:#}");
    assert_eq!(after["started_ms"], before["started_ms"], "the daemon's identity carries over");
    assert_eq!(sessions(&d), held, "the same sessions, ids, programs (pids) and states");
    // The ticker never stopped, and what it printed through the upgrade is on its screen.
    let deadline = Instant::now() + WAIT;
    while ticks(&d, "tick") <= t + 10 {
        assert!(Instant::now() < deadline, "the ticker stopped at {}", ticks(&d, "tick"));
        std::thread::sleep(Duration::from_millis(100));
    }
    // The agent's turn finishes on the new daemon: its Stop hook reached it.
    let w = ok(&d, &["wait", "a1", "--for", "idle", "--timeout", "40s"]);
    assert!(w.contains("idle"), "{w}");
    wait_screen(&d, "a1", "worked");
    // Typing reaches a handed-over program, and its exit code comes back through its handle.
    ok(&d, &["send", "ex", "4"]);
    let deadline = Instant::now() + WAIT;
    loop {
        let s = json(&d, &["status", "ex", "--json"])["session"].clone();
        if s["state"]["t"] == "exited" {
            assert_eq!(s["state"]["code"], 4, "{s:#}");
            break;
        }
        assert!(Instant::now() < deadline, "ex never exited: {s:#}");
        std::thread::sleep(Duration::from_millis(100));
    }
    // A new session starts on the new daemon, and a second upgrade works from the first.
    new(&d, "late", &ps("Write-Output started-after"));
    wait_screen(&d, "late", "started-after");
    let up2 = json(&d, &["daemon", "upgrade", "--json"]);
    assert_eq!(up2["rolled_back"], false, "{up2}");
    assert_eq!(status(&d)["generation"], 2);
    let t2 = ticks(&d, "tick");
    let deadline = Instant::now() + WAIT;
    while ticks(&d, "tick") <= t2 + 5 {
        assert!(Instant::now() < deadline, "the ticker stopped after the second upgrade");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A new binary that refuses the image exits; the old daemon carries on with every session,
/// and the command says the upgrade was called off.
#[test]
fn a_refused_handover_leaves_the_daemon_running() {
    let d = daemon(&[("PTYRO_TEST_REFUSE_HANDOFF", "1")]);
    new(&d, "tick", &ps(TICKER));
    wait_screen(&d, "tick", "tick 3");
    let pid = status(&d)["pid"].clone();
    let out = cli(&d, &["daemon", "upgrade", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let up: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(up["rolled_back"], true, "{up}");
    let st = status(&d);
    assert_eq!(st["pid"], pid, "the same daemon carries on");
    assert_eq!(st["rolled_back"], true);
    let t = ticks(&d, "tick");
    let deadline = Instant::now() + WAIT;
    while ticks(&d, "tick") <= t + 5 {
        assert!(Instant::now() < deadline, "the ticker stopped after the called-off upgrade");
        std::thread::sleep(Duration::from_millis(100));
    }
    // And a new session still starts: the daemon serves as before.
    new(&d, "after", &ps("Write-Output still-serving"));
    wait_screen(&d, "after", "still-serving");
}
