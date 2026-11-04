//! WIN5 end to end, on Windows itself: a native daemon over its named pipe, sessions in ConPTY,
//! and every status source (OSC 7501, 133, 9, 777, hooks) arriving through it. CI's `windows`
//! job runs these with the packaged ConPTY beside `ptyro.exe` (`make conpty INTO=…`) and sets
//! `PTYRO_TEST_PACKAGED_CONPTY=1`; without it, the tests that need what only the packaged ConPTY
//! passes on (the query responder) say they're skipped.
#![cfg(windows)]

use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_core::status::{AgentStatus, Confidence, SignalSource};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

/// The packaged ConPTY is beside the test's `ptyro.exe`, so the daemon uses it.
fn packaged() -> bool {
    let dir = std::path::Path::new(BIN).parent().unwrap().to_path_buf();
    let here = dir.join("conpty.dll").is_file() && dir.join("OpenConsole.exe").is_file();
    assert!(
        here || std::env::var_os("PTYRO_TEST_PACKAGED_CONPTY").is_none(),
        "PTYRO_TEST_PACKAGED_CONPTY is set, but no conpty.dll is beside {BIN}: make conpty INTO=…"
    );
    if !here {
        eprintln!("skipped: no packaged ConPTY beside {BIN} (make conpty INTO=…)");
    }
    here
}

/// Every adapter's agent is the fake agent in that adapter's mode (TOML literal strings: the
/// path has backslashes).
fn agents_config() -> String {
    let mut c = String::from("[attention]\nmin_work = \"1s\"\n");
    for a in ["claude", "codex"] {
        c.push_str(&format!("[agents.{a}]\ncommand = ['{BIN}', 'fake-agent', '--as', '{a}']\n"));
    }
    c
}

fn daemon() -> TempDaemon {
    TempDaemon::start_with(BIN, &[], Some(&agents_config())).unwrap()
}

async fn spawn(c: &Client, name: &str, argv: &[&str]) -> SessionInfo {
    let spec = SpawnSpec {
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd: std::env::temp_dir(),
        name: Some(name.into()),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn get(c: &Client, name: &str) -> SessionInfo {
    match c.request(Request::Get { addr: name.into() }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn wait_for(
    c: &Client,
    name: &str,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let s = get(c, name).await;
        if pred(&s) {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: never {what}; last: status {:?} state {:?} exit {:?}",
            s.status,
            s.state,
            s.last_command_exit
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn send(c: &Client, name: &str, text: &str) {
    c.request(Request::SendText {
        addr: name.into(),
        text: text.into(),
        enter: true,
        wait_ready_ms: None,
    })
    .await
    .unwrap();
}

fn is(s: &SessionInfo, st: AgentStatus) -> bool {
    s.status.as_ref().is_some_and(|r| r.status == st)
}

/// PowerShell, writing `script`'s output with escapes spelled `` `e `` (PowerShell 5.1 has no
/// `` `e ``: `[char]27` is built once as `$e`).
fn ps(script: &str) -> Vec<String> {
    let body = format!("$e = [char]27; $b = [char]7; {script}");
    ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", &body]
        .map(String::from)
        .to_vec()
}

#[tokio::test]
async fn a_session_runs_in_conpty_over_the_pipe_and_its_exit_code_is_kept() {
    let d = daemon();
    let c = d.client().await.unwrap();
    let argv = ps(
        "Write-Output ('ptyro=' + $env:PTYRO + ' term=' + $env:TERM + ' in=' + (Get-Location)); exit 7",
    );
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let info = spawn(&c, "ps", &argv).await;
    let text = d.wait_for_text(&c, &info.name, "term=", WAIT).await.unwrap();
    let tmp = std::env::temp_dir().canonicalize().unwrap();
    let tmp = tmp.to_string_lossy();
    let tmp = tmp.trim_start_matches(r"\\?\").trim_end_matches('\\');
    assert!(text.contains("ptyro=1 term=xterm-256color"), "{text}");
    assert!(text.to_lowercase().contains(&tmp.to_lowercase()), "not in {tmp}: {text}");
    let s = wait_for(&c, "ps", "exited", |s| !s.state.is_live()).await;
    assert_eq!(s.state, ProcessState::Exited { code: Some(7), signal: None });
}

#[tokio::test]
async fn osc_7501_and_the_query_responder_work_through_conpty() {
    if !packaged() {
        return;
    }
    let d = daemon();
    let c = d.client().await.unwrap();
    // The fake agent probes for OSC 7501, then DA1: only ptyro's answer to the probe, in order,
    // makes it say yes (the packaged ConPTY passes every query on; Windows' own answers DA1).
    spawn(&c, "tool", &[BIN, "fake-agent"]).await;
    d.wait_for_text(&c, "tool", "osc7501=yes", WAIT).await.unwrap();
    send(&c, "tool", "work 1.2").await;
    let s = wait_for(&c, "tool", "working", |s| is(s, AgentStatus::Working)).await;
    assert_eq!(s.status.as_ref().unwrap().confidence, Confidence::High);
    wait_for(&c, "tool", "done", |s| is(s, AgentStatus::Done)).await;
    send(&c, "tool", "ask Proceed?").await;
    let s = wait_for(&c, "tool", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    assert_eq!(s.program_status.as_ref().and_then(|p| p.kind.as_deref()), Some("question"));
}

#[tokio::test]
async fn osc_133_9_and_777_reach_the_session() {
    let d = daemon();
    let mut c = d.client().await.unwrap();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let script = "[Console]::Out.Write($e + ']133;A' + $b + '> ' + $e + ']133;C' + $b); \
         Start-Sleep -Milliseconds 300; \
         [Console]::Out.Write($e + ']133;D;3' + $b + $e + ']9;nine says hi' + $b + $e + ']777;notify;title;seven says hi' + $b + 'ok'); \
         [Console]::Out.Flush(); Start-Sleep 30";
    let argv = ps(script);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let info = spawn(&c, "marks", &argv).await;
    wait_for(&c, "marks", "a command's exit", |s| s.last_command_exit == Some(3)).await;
    let (mut nine, mut seven) = (false, false);
    let deadline = Instant::now() + WAIT;
    while !(nine && seven) {
        assert!(Instant::now() < deadline, "notifications: OSC 9 {nine}, OSC 777 {seven}");
        if let Ok(Some(ServerMsg::Event { event: DomainEvent::Notification { session, text } })) =
            tokio::time::timeout(Duration::from_millis(200), pushes.recv()).await
            && session == info.id
        {
            nine |= text.contains("nine says hi");
            seven |= text.contains("seven says hi");
        }
    }
}

#[tokio::test]
async fn an_agents_hooks_report_its_status() {
    // Claude runs its hooks through bash, Git Bash on Windows; so does the fake.
    if ptyro_core::os::program_names("sh").iter().all(|n| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .all(|d| !d.join(n).is_file())
    }) {
        assert!(std::env::var_os("CI").is_none(), "CI's Windows runner has Git Bash's sh on PATH");
        eprintln!("skipped: no sh on PATH (Git for Windows)");
        return;
    }
    let d = daemon();
    let c = d.client().await.unwrap();
    let spec = SpawnSpec {
        agent: Some("claude".into()),
        cwd: std::env::temp_dir(),
        name: Some("cl".into()),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    d.wait_for_text(&c, "cl", "fake claude", WAIT).await.unwrap();
    send(&c, "cl", "work 1.2 ok").await;
    let s = wait_for(&c, "cl", "working", |s| is(s, AgentStatus::Working)).await;
    let src = s.status.as_ref().unwrap().source;
    assert!(matches!(src, SignalSource::Hook | SignalSource::Osc), "working from {src:?}");
    let s = wait_for(&c, "cl", "a turn", |s| s.turns >= 1 && is(s, AgentStatus::Idle)).await;
    assert!(s.conversation.is_some(), "the hooks named the conversation");
    send(&c, "cl", "ask make deploy").await;
    let s = wait_for(&c, "cl", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    assert_eq!(s.status.as_ref().unwrap().source, SignalSource::Hook);
}

#[tokio::test]
async fn a_stop_ends_the_whole_tree() {
    let d = daemon();
    let c = d.client().await.unwrap();
    // A program that starts another and both wait: the stop must take both (the Job Object).
    // `-n 6007` marks this test's ping among any others on the machine.
    let argv =
        ps("Start-Process -NoNewWindow ping.exe '-n 6007 127.0.0.1' | Out-Null; Start-Sleep 600");
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    spawn(&c, "tree", &argv).await;
    let pings = || {
        let out = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "@(Get-CimInstance Win32_Process -Filter \"Name='PING.EXE'\" | Where-Object { $_.CommandLine -like '*6007*' }).Count",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().unwrap_or(0)
    };
    let deadline = Instant::now() + WAIT;
    while pings() == 0 {
        assert!(Instant::now() < deadline, "the child ping never started");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let started = Instant::now();
    c.request(Request::Kill { addr: "tree".into(), signal: None }).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
    let deadline = Instant::now() + Duration::from_secs(10);
    while pings() > 0 {
        assert!(Instant::now() < deadline, "the child ping outlived the stop");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
