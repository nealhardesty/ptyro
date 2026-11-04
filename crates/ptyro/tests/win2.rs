//! ROADMAP WIN2, inside WSL only: the daemon holds WSL open while it runs (SPEC §20.10), with a
//! real `wsl.exe` back into this distro, and leaves nothing behind when it stops, when it is
//! killed, or when `[daemon] hold_wsl` goes off. Elsewhere the daemon has no hold at all.

use std::time::{Duration, Instant};

use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// The holder's inner loops for this daemon pid (its marker starts `: ptyro-wsl-hold <pid> `).
fn loops(pid: u32) -> usize {
    let mark = format!(": ptyro-wsl-hold {pid} ");
    std::fs::read_dir("/proc")
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| {
                    std::fs::read(e.path().join("cmdline")).is_ok_and(|c| {
                        let c = String::from_utf8_lossy(&c);
                        c.starts_with("/bin/sh\0-c\0") && c.contains(&mark)
                    })
                })
                .count()
        })
        .unwrap_or(0)
}

fn until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

async fn hold(d: &TempDaemon) -> (u32, Option<ptyro_protocol::WslHold>) {
    let c = d.client().await.unwrap();
    match c.request(Request::Status).await.unwrap() {
        Reply::Status { status } => (status.pid, status.wsl_hold),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_holds_wsl_open_while_it_runs_and_not_a_moment_longer() {
    let mut d = TempDaemon::start_with_env(BIN, &[("PTYRO_WSL_HOLD", "1")]).unwrap();
    let (pid, h) = hold(&d).await;
    let Some(w) = ptyro_core::wsl::detect() else {
        assert_eq!(h, None, "no hold outside WSL");
        eprintln!("skipped: not WSL");
        return;
    };
    if !w.interop || w.distro.is_none() {
        let why = h.unwrap().why_not.unwrap_or_default();
        assert!(why.contains("interop") || why.contains("name"), "{why}");
        eprintln!("skipped: {why}");
        return;
    }
    let start = Instant::now();
    while !hold(&d).await.1.is_some_and(|h| h.holding) {
        assert!(start.elapsed() < Duration::from_secs(20), "never held: {:?}", hold(&d).await.1);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    until("the inner loop", || loops(pid) == 1);

    // Off: the proxy and the loop both go, though the daemon runs on.
    d.write_config("[daemon]\nhold_wsl = false\n").unwrap();
    until("the loop to go when the setting does", || loops(pid) == 0);
    let h = hold(&d).await.1.unwrap();
    assert!(!h.holding && h.why_not.as_deref() == Some("[daemon] hold_wsl is off"), "{h:?}");

    // On again, then stopped: nothing left.
    d.write_config("[daemon]\nhold_wsl = true\n").unwrap();
    until("the hold to come back", || loops(pid) == 1);
    d.stop();
    until("the loop to go with the daemon", || loops(pid) == 0);

    // Killed: Linux kills the proxy, and the loop sees the pid gone.
    d.restart().unwrap();
    let (pid, _) = hold(&d).await;
    until("the hold after a restart", || loops(pid) == 1);
    d.crash();
    until("the loop to go with a killed daemon", || loops(pid) == 0);
}
