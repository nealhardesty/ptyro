//! End-to-end checks for what the full-codebase review found in the daemon: a program in one
//! session can't report for another.

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

/// A hook from inside session A counts for A; one that names session B is refused, since it
/// would forge what B's notifications and its sidebar row believe.
#[tokio::test]
async fn a_program_reports_only_for_its_own_session() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let out = d.home.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let home = ptyro_core::os::canonicalize(d.home.path()).unwrap();
    let b = shell(&c, &home, "bee", "sleep 120".into()).await;
    let done = out.join("done");
    let a = shell(
        &c,
        &home,
        "aye",
        format!(
            "PTYRO_SESSION_ID={b} '{BIN}' hook claude Notification </dev/null; \
             '{BIN}' hook claude Notification </dev/null; echo exit=$? > '{o}'; sleep 120",
            o = done.display()
        ),
    )
    .await;
    wait_for(&done, "both hooks sent");
    let hooks = |id: String| {
        let c = &c;
        async move {
            let r = c.request(Request::Events { session: Some(id), limit: None }).await.unwrap();
            let Reply::Events { events } = r else { panic!("{r:?}") };
            events.iter().filter(|e| e.kind == "hook").count()
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while hooks(a.clone()).await == 0 {
        assert!(Instant::now() < deadline, "its own hook never counted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(hooks(b).await, 0, "a hook for another session is refused");
}
