//! M2 end-to-end tests: selectors, restart, scrollback, views, history, config reload, and the
//! multi-session TUI itself, driven inside ptyro's own headless emulator.

use std::time::{Duration, Instant};

use ptyro_core::ErrorCode;
use ptyro_core::TermSize;
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::{Report, Script};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(10);

async fn spawn(c: &ptyro_client::Client, name: &str, argv: &[&str]) -> SessionInfo {
    let spec = SpawnSpec {
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd: std::env::temp_dir(),
        name: Some(name.into()),
        size: Some(TermSize::new(80, 12)),
        ..SpawnSpec::default()
    };
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn resolve(c: &ptyro_client::Client, sel: &str) -> Vec<String> {
    match c.request(Request::Resolve { addr: sel.into() }).await.expect("resolve") {
        Reply::Sessions { sessions } => sessions.into_iter().map(|s| s.name).collect(),
        other => panic!("{other:?}"),
    }
}

async fn wait_state(
    c: &ptyro_client::Client,
    name: &str,
    pred: impl Fn(&ProcessState) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(Reply::Session { info }) = c.request(Request::Get { addr: name.into() }).await
            && pred(&info.state)
        {
            return info;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {name}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn selectors_expand_to_the_right_sessions() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    spawn(&c, "build-web", &["sh", "-c", "sleep 30"]).await;
    spawn(&c, "build-api", &["sh", "-c", "sleep 30"]).await;
    let done = spawn(&c, "notes", &["true"]).await;
    wait_state(&c, "notes", |s| !s.is_live()).await;
    assert_eq!(resolve(&c, "@all").await, ["build-web", "build-api", "notes"]);
    assert_eq!(resolve(&c, "@live").await, ["build-web", "build-api"]);
    assert_eq!(resolve(&c, "@exited").await, ["notes"]);
    assert_eq!(resolve(&c, "build-*").await, ["build-web", "build-api"]);
    assert_eq!(
        resolve(&c, "notes,build-a?i").await,
        ["build-api", "notes"],
        "list order, not argument order"
    );
    assert_eq!(resolve(&c, "@working").await, Vec::<String>::new());
    let err = c.request(Request::Resolve { addr: "@nonsense".into() }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    // @focused follows what a TUI reports.
    c.send(ClientMsg::Focus { session: done.id, terminal_focused: true }).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(resolve(&c, "@focused").await, ["notes"]);
    let err = c.request(Request::Get { addr: "build-*".into() }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Ambiguous, "single-target requests refuse ambiguous selectors");
}

#[tokio::test]
async fn restart_runs_the_command_again_in_place() {
    let d = TempDaemon::start(BIN).unwrap();
    let mut c = d.client().await.unwrap();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let first = spawn(&c, "job", &["sh", "-c", "echo run-$$; exit 3"]).await;
    let exited = wait_state(&c, "job", |s| !s.is_live()).await;
    assert_eq!(exited.state, ProcessState::Exited { code: Some(3), signal: None });
    let err = c.request(Request::Restart { addr: "nope".into() }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let again = match c.request(Request::Restart { addr: "job".into() }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(again.id, first.id, "same session");
    assert_ne!(again.pid, first.pid, "new process");
    let deadline = Instant::now() + WAIT;
    let mut saw_restarted = false;
    while Instant::now() < deadline && !saw_restarted {
        if let Ok(Some(ServerMsg::Event { event: DomainEvent::SessionRestarted { info } })) =
            tokio::time::timeout(Duration::from_millis(200), pushes.recv()).await
        {
            saw_restarted = info.id == first.id;
        }
    }
    assert!(saw_restarted);
    // Restarting a live session is refused.
    let live = spawn(&c, "live", &["sh", "-c", "sleep 30"]).await;
    let err = c.request(Request::Restart { addr: live.name }).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn scrollback_documents_carry_history_and_prompt_marks() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let script = r"for i in $(seq 1 300); do echo line $i; done; printf '\033]133;A\007$ \033]133;B\007ls\n'; echo out; sleep 30";
    spawn(&c, "scroll", &["sh", "-c", script]).await;
    d.wait_for_text(&c, "scroll", "out", WAIT).await.unwrap();
    let doc = match c
        .request(Request::ReadScrollback { addr: "scroll".into(), lines: None })
        .await
        .unwrap()
    {
        Reply::Scrollback { doc } => doc,
        other => panic!("{other:?}"),
    };
    let rows = ptyro_core::cache::doc_rows(&doc);
    let text = ptyro_core::cache::plain_text(&rows);
    assert!(doc.history >= 290, "history {}", doc.history);
    assert!(text.starts_with("line 1\n"), "{}", &text[..40]);
    assert!(text.contains("line 300\n$ ls\nout"));
    let prompt_rows: Vec<usize> =
        doc.rows.iter().enumerate().filter(|(_, r)| r.prompt).map(|(i, _)| i).collect();
    assert_eq!(prompt_rows.len(), 1);
    assert!(
        ptyro_core::cache::plain_text(&rows[prompt_rows[0]..=prompt_rows[0]]).starts_with("$ ls")
    );
    let tail = match c
        .request(Request::ReadScrollback { addr: "scroll".into(), lines: Some(5) })
        .await
        .unwrap()
    {
        Reply::Scrollback { doc } => doc,
        other => panic!("{other:?}"),
    };
    assert_eq!(tail.history, 5);
}

#[tokio::test]
async fn views_and_history_survive_a_daemon_restart() {
    let mut d = TempDaemon::start(BIN).unwrap();
    {
        let c = d.client().await.unwrap();
        spawn(&c, "before", &["sh", "-c", "sleep 30"]).await;
        let view = serde_json::json!({"v": 1, "anything": [1, 2, 3]});
        c.request(Request::SaveView { name: "main".into(), view: view.clone() }).await.unwrap();
        match c.request(Request::LoadView { name: "main".into() }).await.unwrap() {
            Reply::View { view: Some(v) } => assert_eq!(v, view),
            other => panic!("{other:?}"),
        }
    }
    d.restart().unwrap();
    let c = d.client().await.unwrap();
    match c.request(Request::LoadView { name: "main".into() }).await.unwrap() {
        Reply::View { view: Some(v) } => assert_eq!(v["anything"], serde_json::json!([1, 2, 3])),
        other => panic!("{other:?}"),
    }
    match c.request(Request::LoadView { name: "other".into() }).await.unwrap() {
        Reply::View { view: None } => {}
        other => panic!("{other:?}"),
    }
    let entries = match c.request(Request::History { limit: None }).await.unwrap() {
        Reply::History { entries } => entries,
        other => panic!("{other:?}"),
    };
    let old = entries.iter().find(|e| e.name == "before").expect("history keeps earlier sessions");
    assert!(!old.current);
    assert!(old.ended_ms.is_some());
    assert!(!old.state.is_live());
}

async fn next_reload(
    pushes: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
) -> Option<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, pushes.recv()).await {
            Ok(Some(ServerMsg::Event { event: DomainEvent::ConfigReloaded { error } })) => {
                return error;
            }
            Ok(Some(_)) => {}
            _ => panic!("no ConfigReloaded event"),
        }
    }
}

#[tokio::test]
async fn the_daemon_reloads_config_and_reports_errors() {
    let d = TempDaemon::start(BIN).unwrap();
    let mut c = d.client().await.unwrap();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    d.write_config("[daemon]\nscrollback_lines = 50\n").unwrap();
    assert_eq!(next_reload(&mut pushes).await, None);
    // New sessions use the new scrollback size.
    spawn(&c, "short", &["sh", "-c", "seq 1 200; sleep 30"]).await;
    d.wait_for_text(&c, "short", "200", WAIT).await.unwrap();
    match c.request(Request::ReadScrollback { addr: "short".into(), lines: None }).await.unwrap() {
        Reply::Scrollback { doc } => assert!(doc.history <= 50, "history {}", doc.history),
        other => panic!("{other:?}"),
    }
    d.write_config("[ui]\nsidebar_widht = 3\n").unwrap();
    let error = next_reload(&mut pushes).await.expect("an error");
    assert!(error.contains("sidebar_widht"), "{error}");
}

#[test]
fn cli_selectors_restart_scrollback_config_and_shell_init() {
    let d = TempDaemon::start(BIN).unwrap();
    let run = |args: &[&str]| d.cmd().args(args).output().unwrap();
    let ok = |args: &[&str]| {
        let o = run(args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    ok(&["new", "-d", "-n", "w1", "--", "sh", "-c", "seq 1 50; echo done; exit 4"]);
    ok(&["new", "-d", "-n", "w2", "--", "true"]);
    ok(&["new", "-d", "-n", "keep", "--", "sh", "-c", "sleep 30"]);
    std::thread::sleep(Duration::from_millis(500));
    let tail = ok(&["read", "w1", "--scrollback", "3"]);
    assert!(tail.contains("done"), "{tail}");
    ok(&["restart", "w1"]);
    std::thread::sleep(Duration::from_millis(300));
    ok(&["rm", "@exited"]);
    let ls = ok(&["ls"]);
    assert!(ls.contains("keep") && !ls.contains("w2"), "{ls}");
    assert_eq!(run(&["kill", "@nothing-matches-this"]).status.code(), Some(2));
    ok(&["kill", "@live"]);
    let all = ok(&["ls", "--all"]);
    assert!(all.contains("keep") && all.contains("w2"), "history lists removed sessions:\n{all}");

    assert!(ok(&["config", "path"]).trim().ends_with("config.toml"));
    assert!(ok(&["config", "check"]).contains("all defaults"));
    d.write_config("[ui]\nprefix = \"C-a\"\n[keys]\n\"y\" = \"pane.zoom\"\n[agents.claude]\nadapter = \"claude\"\n[recorder]\nenabled = true\n").unwrap();
    let check = ok(&["config", "check"]);
    assert!(!check.contains("[recorder] is accepted but not used"), "{check}");
    assert!(ok(&["config", "show"]).contains("prefix = \"C-a\""));
    d.write_config("[keys]\n\"y\" = \"no.such.action\"\n").unwrap();
    let bad = run(&["config", "check"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no.such.action"));
    d.write_config("[ui]\ntheme = \"no-such-theme\"\n").unwrap();
    assert_eq!(run(&["config", "check"]).status.code(), Some(2));

    for shell in ["bash", "zsh", "fish"] {
        let snippet = ok(&["shell-init", shell]);
        assert!(
            snippet.contains("133;A")
                && snippet.contains("]7;file://")
                && snippet.contains("PTYRO"),
            "{shell}"
        );
    }
}

// ── the TUI, inside the headless emulator ──────────────────────────────────

fn tui(d: &TempDaemon, args: &[&str]) -> Script {
    let mut argv = vec![BIN.to_owned()];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    let mut s = Script::new(argv).size(100, 20).timeout(6.0);
    for (k, v) in d.env() {
        s = s.env(&k, &v);
    }
    s
}

async fn run(s: Script) -> Report {
    ptyro_testkit::headless::run(s).await.expect("headless run")
}

#[tokio::test]
async fn tui_splits_and_restores_the_layout_on_reattach() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    spawn(&c, "agent", &[BIN, "fake-agent"]).await;
    spawn(&c, "logs", &["sh", "-c", "for i in $(seq 1 40); do echo log-$i; done; exec cat"]).await;
    let r = run(tui(&d, &["attach", "agent"])
        .send(1.5, "\\x00v")
        .send(1.9, "logs")
        .send(2.3, "\\r")
        .snapshot(3.2)
        .send(3.6, "\\x00d")
        .timeout(5.0))
    .await;
    let split = r.at(3.2);
    assert!(
        split.contains("osc7501=yes") && split.contains("log-40"),
        "both panes visible:\n{split}"
    );
    let r2 = run(tui(&d, &[]).snapshot(1.8).send(2.2, "\\x00d").timeout(4.0)).await;
    let restored = r2.at(1.8);
    assert!(
        restored.contains("osc7501=yes") && restored.contains("log-40"),
        "layout restored:\n{restored}"
    );
}

#[tokio::test]
async fn tui_copy_mode_searches_scrollback_and_yanks() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    spawn(&c, "build", &["sh", "-c", "for i in $(seq 1 200); do echo row $i; done; echo 'ERROR: disk full'; seq 1 30; exec cat"]).await;
    let r = run(tui(&d, &["attach", "build"])
        .send(1.4, "\\x00[")
        .send(1.9, "/error\\r")
        .snapshot(2.4)
        .send(2.6, "V")
        .send(2.8, "y")
        .snapshot(3.2)
        .send(3.4, "\\x00]")
        .snapshot(3.9)
        .send(4.2, "\\x00d"))
    .await;
    let found = r.at(2.4);
    assert!(found.contains("COPY") && found.contains("1/1"), "search found the match:\n{found}");
    assert!(found.contains("ERROR: disk full"));
    assert!(r.at(3.2).contains("copied 16 characters"), "{}", r.at(3.2));
    // The original line has scrolled off; C-Space ] pasted the yank at `cat`'s cursor.
    assert!(!r.at(3.2).lines().take(18).any(|l| l.contains("ERROR")) || r.at(3.2).contains("30"));
    let last = r.at(3.9).lines().rev().find(|l| l.contains('│')).unwrap_or_default().to_owned();
    assert!(last.contains("ERROR: disk full"), "pasted:\n{}", r.at(3.9));
}

#[tokio::test]
async fn tui_palette_finder_sidebar_and_restart() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    spawn(&c, "alpha", &["sh", "-c", "echo I-am-alpha; exec cat"]).await;
    // Exits the first time; after a restart (same session id) it stays up.
    let beta = r#"m=/tmp/ptyro-test-$PTYRO_SESSION_ID; echo I-am-beta; if [ -e $m ]; then rm $m; exec cat; fi; touch $m; exit 5"#;
    spawn(&c, "beta", &["sh", "-c", beta]).await;
    let r = run(tui(&d, &["attach", "alpha"])
        .send(1.4, "\\x00/")
        .send(1.8, "bet")
        .send(2.1, "\\r")
        .snapshot(2.6)
        .send(2.9, "\\r")
        .snapshot(3.6)
        .send(3.9, "\\x00\\t")
        .send(4.2, "k")
        .send(4.4, "\\r")
        .snapshot(4.9)
        .send(5.2, "\\x00:")
        .send(5.5, "pane.split_down")
        .send(5.8, "\\r")
        .send(6.1, "beta")
        .send(6.3, "\\r")
        .snapshot(6.9)
        .send(7.1, "\\x00d")
        .timeout(8.5))
    .await;
    let exited = r.at(2.6);
    assert!(
        exited.contains("I-am-beta") && exited.contains("exited 5") && exited.contains("restart"),
        "{exited}"
    );
    assert!(
        r.at(3.6).contains("I-am-beta") && !r.at(3.6).contains("exited 5"),
        "Enter restarted beta:\n{}",
        r.at(3.6)
    );
    assert!(r.at(4.9).contains("I-am-alpha"), "sidebar Enter showed alpha:\n{}", r.at(4.9));
    let split = r.at(6.9);
    assert!(
        split.contains("──") && split.contains("I-am-alpha") && split.contains("I-am-beta"),
        "palette ran a split (stacked):\n{split}"
    );
}

#[tokio::test]
async fn tui_view_only_and_custom_prefix() {
    let d = TempDaemon::start(BIN).unwrap();
    d.write_config("[ui]\nprefix = \"C-a\"\n").unwrap();
    let c = d.client().await.unwrap();
    spawn(&c, "agent", &[BIN, "fake-agent"]).await;
    let r = run(tui(&d, &["attach", "--view-only", "agent"])
        .send(1.5, "hello\\r")
        .snapshot(2.2)
        .send(2.4, "\\x01?")
        .snapshot(3.0)
        .send(3.2, "\\e\\x01d")
        .timeout(5.0))
    .await;
    assert!(r.at(2.2).contains("VIEW ONLY"));
    assert!(!r.at(2.2).contains("you said"), "view-only input must not reach the session");
    assert!(r.at(3.0).contains("C-a d"), "help shows the configured prefix:\n{}", r.at(3.0));
    let screen = match c
        .request(Request::ReadScreen { addr: "agent".into(), format: Default::default() })
        .await
        .unwrap()
    {
        Reply::Screen { text } => text,
        other => panic!("{other:?}"),
    };
    assert!(!screen.contains("you said"));
}
