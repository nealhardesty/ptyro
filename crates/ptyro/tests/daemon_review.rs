//! End-to-end checks for what the full-codebase review found in the daemon: a program in one
//! session can't report for another, a recording is found by session id and never by path.

use std::time::{Duration, Instant};

use ptyro_core::TermSize;
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

async fn shell(
    c: &ptyro_client::Client,
    cwd: &std::path::Path,
    name: &str,
    script: String,
) -> String {
    let spec = SpawnSpec {
        cwd: cwd.to_path_buf(),
        name: Some(name.into()),
        argv: vec!["sh".into(), "-c".into(), script],
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    let Reply::Session { info } = c.request(Request::Spawn { spec }).await.unwrap() else {
        panic!("a session")
    };
    info.id.full()
}

fn wait_for(path: &std::path::Path, what: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(t) = std::fs::read_to_string(path)
            && t.contains("exit=")
        {
            return t;
        }
        assert!(Instant::now() < deadline, "{what}: nothing in {}", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `ptyro report` from inside session A is A's own word; for session B it is refused, since it
/// would forge what B's lead, its gates and the review queue believe.
#[tokio::test]
async fn a_program_reports_only_for_its_own_session() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let out = d.home.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let home = d.home.path().canonicalize().unwrap();
    let b = shell(&c, &home, "bee", "sleep 120".into()).await;
    let (o1, o2) = (out.join("other"), out.join("own"));
    shell(
        &c,
        &home,
        "aye",
        format!(
            "'{BIN}' report --state working --session {b} > '{o1}' 2>&1; echo exit=$? >> '{o1}'; \
             '{BIN}' report --state working > '{o2}' 2>&1; echo exit=$? >> '{o2}'; sleep 120",
            o1 = o1.display(),
            o2 = o2.display()
        ),
    )
    .await;
    let other = wait_for(&o1, "reporting for another session");
    assert!(
        other.contains("can only report for its own session") && !other.contains("exit=0"),
        "{other}"
    );
    let own = wait_for(&o2, "reporting for its own session");
    assert!(own.contains("exit=0"), "its own word is taken: {own}");
}

/// A recording is looked up by a session's id; an address that is a path reads no directory.
#[tokio::test]
async fn a_recording_is_found_by_id_and_never_by_path() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    // A directory outside `sessions/` that holds what looks like a recording.
    let other = d.paths.data.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("pty.log"), b"not yours").unwrap();
    for addr in ["../elsewhere", "../../elsewhere", "/etc", "a/b", ""] {
        let e = c.request(Request::Replay { addr: addr.into() }).await.unwrap_err();
        assert!(e.to_string().contains("no recording"), "{addr:?}: {e}");
        let e = c
            .request(Request::Export { addr: addr.into(), format: "text".into() })
            .await
            .unwrap_err();
        assert!(e.to_string().contains("no recording"), "{addr:?}: {e}");
    }
}
