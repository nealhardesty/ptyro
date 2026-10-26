//! End-to-end tests (ROADMAP M1.17): a real daemon in a temporary home, real PTYs, the built-in
//! fake agent. No network, no real agents.

use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::cache::ScreenCache;
use ptyro_core::input::{InputEvent, KeyCode, KeyEvent, Mods};
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_core::{ErrorCode, TermSize};
use ptyro_protocol::{ClientMsg, Reply, Request, ScreenFormat, ServerMsg};
use ptyro_testkit::TempDaemon;
use tokio::sync::mpsc::UnboundedReceiver;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(10);

fn fake_agent() -> Vec<String> {
    vec![BIN.into(), "fake-agent".into()]
}

async fn spawn(c: &Client, argv: Vec<String>, size: TermSize) -> SessionInfo {
    let spec =
        SpawnSpec { argv, cwd: std::env::temp_dir(), size: Some(size), ..SpawnSpec::default() };
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn get(c: &Client, addr: &str) -> SessionInfo {
    match c.request(Request::Get { addr: addr.into() }).await.expect("get") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn screen(c: &Client, addr: &str) -> String {
    match c
        .request(Request::ReadScreen { addr: addr.into(), format: ScreenFormat::Plain })
        .await
        .expect("read")
    {
        Reply::Screen { text } => text,
        other => panic!("{other:?}"),
    }
}

/// A subscribed viewer that applies frames to its own cache, like the TUI does.
struct Viewer {
    client: Client,
    pushes: UnboundedReceiver<ServerMsg>,
    cache: ScreenCache,
    keyframes: usize,
}

impl Viewer {
    async fn attach(d: &TempDaemon, info: &SessionInfo, size: TermSize, control: bool) -> Self {
        let mut client = d.client().await.expect("connect");
        let pushes = client.pushes();
        client.send(ClientMsg::Subscribe { session: info.id, size: Some(size), control }).unwrap();
        Self { client, pushes, cache: ScreenCache::new(info.size), keyframes: 0 }
    }

    /// Apply frames (acking each) until `pred` holds for the cache.
    async fn until(&mut self, what: &str, pred: impl Fn(&ScreenCache) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !pred(&self.cache) {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.pushes.recv()).await {
                Ok(Some(ServerMsg::Screen { frame })) => {
                    if frame.keyframe {
                        self.keyframes += 1;
                    }
                    self.cache.apply(frame.keyframe, frame.seq, &frame.delta);
                    self.client
                        .send(ClientMsg::Ack { session: frame.session, seq: frame.seq })
                        .unwrap();
                }
                Ok(Some(_)) => {}
                _ => panic!(
                    "timed out waiting for {what}; viewer screen:\n{}",
                    self.cache.plain_text()
                ),
            }
        }
    }

    fn type_text(&self, session: &SessionInfo, text: &str) {
        for c in text.chars() {
            let code = if c == '\r' { KeyCode::Enter } else { KeyCode::Char { c } };
            let key = KeyEvent::new(code, Mods::NONE);
            self.client
                .send(ClientMsg::Input { session: session.id, event: InputEvent::Key { key } })
                .unwrap();
        }
    }
}

#[tokio::test]
async fn detach_and_reattach_restores_the_exact_screen() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let info = spawn(&c, fake_agent(), TermSize::new(70, 16)).await;

    let mut a = Viewer::attach(&d, &info, TermSize::new(70, 16), true).await;
    a.until("prompt", |s| s.plain_text().contains("\n>\n")).await;
    a.type_text(&info, "hello from a\r");
    // The echo, then the next prompt: the screen is settled before we detach.
    a.until("echo", |s| s.plain_text().contains("you said: hello from a\n>\n")).await;
    let before = a.cache.plain_text();
    drop(a); // detach

    tokio::time::sleep(Duration::from_millis(200)).await;
    let alive = get(&c, &info.name).await;
    assert_eq!(alive.state, ProcessState::Running, "session must survive the client");
    assert_eq!(alive.viewers, 0);

    let mut b = Viewer::attach(&d, &info, TermSize::new(70, 16), false).await;
    b.until("keyframe", |s| s.seq > 0).await;
    assert_eq!(b.keyframes, 1);
    assert_eq!(b.cache.plain_text(), before, "reattached screen differs");
    assert_eq!(screen(&c, &info.name).await, before);
}

#[tokio::test]
async fn query_responder_and_program_status_work_end_to_end() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let info = spawn(&c, fake_agent(), TermSize::new(80, 12)).await;
    let text = d.wait_for_text(&c, &info.name, "\n>\n", WAIT).await.unwrap();
    assert!(text.contains("osc7501=yes"), "fake agent did not see our OSC 7501 reply:\n{text}");
    c.request(Request::SendText {
        addr: info.name.clone(),
        text: "ask Proceed?".into(),
        enter: true,
        wait_ready_ms: None,
    })
    .await
    .unwrap();
    d.wait_for_text(&c, &info.name, "[y/n]", WAIT).await.unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let s = get(&c, &info.name).await;
        if let Some(p) = &s.program_status
            && p.state == "blocked"
        {
            assert_eq!(p.kind.as_deref(), Some("question"));
            assert_eq!(p.app.as_deref(), Some("fake-agent"));
            break;
        }
        assert!(Instant::now() < deadline, "no blocked status: {:?}", s.program_status);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn child_environment_follows_the_policy() {
    let d = TempDaemon::start_with_env(
        BIN,
        &[
            ("TMUX", "/tmp/tmux-1/default,1,0"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "secret"),
            ("PTYRO_KEEP", "kept"),
        ],
    )
    .unwrap();
    let c = d.client().await.unwrap();
    let argv = ["sh", "-c", "echo T=$TERM X=${TMUX:-none} C=${CLAUDE_CODE_MESSAGING_TOKEN:-none} K=$PTYRO_KEEP S=${PTYRO_SESSION_ID:+set} P=$TERM_PROGRAM; sleep 5"]
        .map(String::from)
        .to_vec();
    let info = spawn(&c, argv, TermSize::new(100, 5)).await;
    let text = d.wait_for_text(&c, &info.name, "P=", WAIT).await.unwrap();
    assert!(text.contains("T=xterm-256color X=none C=none K=kept S=set P=ptyro"), "{text}");
}

#[tokio::test]
async fn children_get_default_sigpipe_and_exit_status_is_recorded() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    // With SIGPIPE ignored (Rust's default for itself), `yes` would spin forever.
    let argv = ["sh", "-c", "yes | head -n 1; echo piped; exit 7"].map(String::from).to_vec();
    let info = spawn(&c, argv, TermSize::new(40, 5)).await;
    let deadline = Instant::now() + WAIT;
    loop {
        let s = get(&c, &info.name).await;
        if let ProcessState::Exited { code, .. } = s.state {
            assert_eq!(code, Some(7));
            break;
        }
        assert!(Instant::now() < deadline, "child never exited (SIGPIPE inherited as ignored?)");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(screen(&c, &info.name).await.contains("piped"));
}

#[tokio::test]
async fn a_client_that_never_acks_does_not_stall_the_session() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let argv = [
        "sh",
        "-c",
        "i=0; while [ $i -lt 20000 ]; do echo line $i; i=$((i+1)); done; echo finished",
    ]
    .map(String::from)
    .to_vec();
    let info = spawn(&c, argv, TermSize::new(40, 10)).await;
    // Subscribe and never read or ack anything.
    let mut lazy = d.client().await.unwrap();
    let _unread = lazy.pushes();
    lazy.send(ClientMsg::Subscribe { session: info.id, size: None, control: false }).unwrap();
    d.wait_for_text(&c, &info.name, "finished", Duration::from_secs(20)).await.unwrap();
}

#[tokio::test]
async fn the_controlling_client_decides_the_size() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let info = spawn(&c, fake_agent(), TermSize::new(80, 20)).await;
    let mut a = Viewer::attach(&d, &info, TermSize::new(90, 21), true).await;
    a.until("a's size", |s| s.size == TermSize::new(90, 21)).await;
    let mut b = Viewer::attach(&d, &info, TermSize::new(120, 30), true).await;
    b.until("b's size", |s| s.size == TermSize::new(120, 30)).await;
    assert_eq!(get(&c, &info.name).await.size, TermSize::new(120, 30));
    // Typing makes a the controller again, and its size wins.
    a.type_text(&info, "size\r");
    a.until("a's size again", |s| {
        s.size == TermSize::new(90, 21) && s.plain_text().contains("size 90x21")
    })
    .await;
}

#[tokio::test]
async fn kill_rename_remove_and_addressing() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let info = spawn(&c, fake_agent(), TermSize::new(60, 10)).await;
    assert_eq!(get(&c, "@last").await.id, info.id);
    assert_eq!(get(&c, &info.handle).await.id, info.id);
    assert_eq!(get(&c, &format!("s:{}", info.handle)).await.id, info.id);
    c.request(Request::Rename { addr: info.name.clone(), name: "renamed".into() }).await.unwrap();
    let err = c.request(Request::Remove { addr: "renamed".into() }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "live sessions can't be removed");
    let killed =
        match c.request(Request::Kill { addr: "renamed".into(), signal: None }).await.unwrap() {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
    assert!(!killed.state.is_live());
    c.request(Request::Remove { addr: "renamed".into() }).await.unwrap();
    let err = c.request(Request::Get { addr: "renamed".into() }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = c
        .request(Request::Rename { addr: "nope".into(), name: "Bad Name".into() })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn an_incompatible_protocol_major_is_rejected() {
    use futures::{SinkExt, StreamExt};
    use ptyro_protocol::{ClientInfo, ClientKind, Encoding, Version, decode, encode, framing};
    let d = TempDaemon::start(BIN).unwrap();
    let stream = tokio::net::UnixStream::connect(&d.paths.socket).await.unwrap();
    let mut framed = tokio_util::codec::Framed::new(stream, framing());
    let hello = ClientMsg::Hello {
        proto: Version { major: 99, minor: 0 },
        client: ClientInfo {
            kind: ClientKind::Test,
            version: "x".into(),
            pid: 1,
            outer: None,
            token: None,
            via: None,
        },
        features: vec![],
    };
    framed.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();
    let reply = framed.next().await.unwrap().unwrap();
    let (msg, enc) = decode::<ServerMsg>(&reply).unwrap();
    assert_eq!(enc, Encoding::Json, "the daemon answers in the client's encoding");
    assert!(matches!(msg, ServerMsg::Reject { .. }), "{msg:?}");
}

#[test]
fn only_one_daemon_per_home_and_stale_sockets_are_recovered() {
    let mut d = TempDaemon::start(BIN).unwrap();
    let second = d.cmd().args(["daemon", "start", "--foreground"]).output().unwrap();
    assert_eq!(second.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    d.crash();
    assert!(d.paths.socket.exists(), "a crash leaves the socket behind");
    // A new client auto-spawns a daemon, which clears the stale socket.
    let out = d.cmd().args(["new", "-d", "--", "sh", "-c", "sleep 30"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ls = d.cmd().arg("ls").output().unwrap();
    assert!(String::from_utf8_lossy(&ls.stdout).contains("running"));
    let stop = d.cmd().args(["daemon", "stop", "--force"]).output().unwrap();
    assert!(stop.status.success());
}

#[test]
fn cli_autospawns_and_maps_errors_to_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(BIN)
            .args(args)
            .env("PTYRO_HOME", home.path())
            .env("PTYRO_BIN", BIN)
            .env_remove("PTYRO_SOCKET")
            .env_remove("PTYRO_SESSION_ID")
            .output()
            .unwrap()
    };
    let ls = run(&["ls"]);
    assert_eq!(ls.status.code(), Some(0), "ls without a daemon is fine");
    assert_eq!(run(&["daemon", "status"]).status.code(), Some(8), "and doesn't start one");

    let new = run(&["new", "-d", "-n", "worker", "--", "sh", "-c", "echo ready; sleep 30"]);
    assert!(new.status.success(), "{}", String::from_utf8_lossy(&new.stderr));
    assert_eq!(run(&["read", "nosuch"]).status.code(), Some(3));
    assert_eq!(
        run(&["new", "-d", "-n", "worker", "--", "true"]).status.code(),
        Some(5),
        "duplicate name"
    );
    assert_eq!(run(&["new", "-d", "-C", "/nonexistent", "--", "true"]).status.code(), Some(2));

    std::thread::sleep(Duration::from_millis(300));
    let read = run(&["read", "worker"]);
    assert!(String::from_utf8_lossy(&read.stdout).contains("ready"));
    let json = run(&["--json-wire", "ls", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v[0]["name"], "worker");

    assert_eq!(run(&["daemon", "stop"]).status.code(), Some(5), "refuses with live sessions");
    assert_eq!(run(&["daemon", "stop", "--force"]).status.code(), Some(0));
    assert_eq!(run(&["daemon", "status"]).status.code(), Some(8));
}

#[test]
fn sigterm_stops_the_daemon_and_its_sessions() {
    let mut d = TempDaemon::start(BIN).unwrap();
    let out = d
        .cmd()
        .args(["new", "-d", "--json", "--", "sh", "-c", "trap '' HUP; sleep 60"])
        .output()
        .unwrap();
    let info: SessionInfo = serde_json::from_slice(&out.stdout).unwrap();
    let pid = info.pid.unwrap();
    d.stop();
    assert!(!d.is_running());
    let gone = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(30));
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap())
            .is_err()
    });
    assert!(gone, "session process {pid} survived daemon shutdown");
    assert!(!d.paths.socket.exists());
}

#[tokio::test]
async fn shell_integration_side_channels_reach_session_info() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let argv = [
        "sh",
        "-c",
        r"printf '\033]7;file://host/tmp/x%%20y\007\033]9;4;1;40\007\033]133;D;3\007ok\n'; sleep 5",
    ]
    .map(String::from)
    .to_vec();
    let info = spawn(&c, argv, TermSize::new(40, 5)).await;
    d.wait_for_text(&c, &info.name, "ok", WAIT).await.unwrap();
    // (The shell's own directory is read on a timer and can briefly stand in for the OSC 7 one.)
    let mut s = get(&c, &info.name).await;
    for _ in 0..100 {
        if s.live_cwd.as_deref() == Some(std::path::Path::new("/tmp/x y")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        s = get(&c, &info.name).await;
    }
    assert_eq!(s.live_cwd.as_deref(), Some(std::path::Path::new("/tmp/x y")));
    assert_eq!(s.progress.map(|p| (p.state, p.percent)), Some((1, Some(40))));
    assert_eq!(s.last_command_exit, Some(3));
}
