//! M4 end-to-end tests: every adapter through the fake agent's adapter modes (hooks, notify,
//! sidecars), the status engine, attention and the inbox, answering prompts, prompt delivery,
//! dormant sessions and resume across daemon restarts, hibernation, usage meters, notifications
//! with signed approve links, after_turn, self reports, and the TUI's `a` / `p` / `i`.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind, Confidence, SignalSource};
use ptyro_protocol::{ClientMsg, Reply, Request};
use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::{Report, Script};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(15);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

/// Every adapter's agent is the fake agent in that adapter's mode.
fn agents_config(extra: &str) -> String {
    let mut c = String::from("[attention]\nmin_work = \"1s\"\n");
    for a in ["claude", "codex", "gemini", "opencode", "antigravity"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n"
        ));
    }
    c.push_str(extra);
    c
}

impl World {
    fn new(extra_config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m4-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(&agents_config(extra_config))).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
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
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn tui(&self, args: &[&str]) -> Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = Script::new(argv)
            .size(110, 24)
            .timeout(8.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

async fn agent(c: &Client, name: &str, agent: &str, prompt: Option<&str>) -> SessionInfo {
    let spec = SpawnSpec {
        agent: Some(agent.into()),
        cwd: std::env::temp_dir(),
        name: Some(name.into()),
        size: Some(TermSize::new(100, 20)),
        prompt: prompt.map(Into::into),
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
            "{name}: never {what}; last: status {:?} attention {:?} state {:?}",
            s.status,
            s.attention,
            s.state
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// The names a selector expands to.
async fn resolve(c: &Client, sel: &str) -> Vec<String> {
    match c.request(Request::Resolve { addr: sel.into() }).await.expect("resolve") {
        Reply::Sessions { sessions } => sessions.into_iter().map(|s| s.name).collect(),
        other => panic!("{other:?}"),
    }
}

fn is(s: &SessionInfo, st: AgentStatus) -> bool {
    s.status.as_ref().is_some_and(|r| r.status == st)
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

async fn inbox(c: &Client) -> Vec<ptyro_core::status::InboxEntry> {
    match c
        .request(Request::Inbox { workspace: None, unread_only: false, limit: None })
        .await
        .unwrap()
    {
        Reply::Inbox { entries } => entries,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn claude_adapter_status_attention_answer_and_usage() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    // The prompt is typed once the agent is ready (SPEC §8.2).
    let s = agent(&c, "cl", "claude", Some("work 1.5 first turn")).await;
    let conv = s.conversation.clone().expect("pre-assigned conversation");
    // The hooks reach the daemon through --plugin-dir → `ptyro hook claude …`.
    let s = wait_for(&c, "cl", "working on a tool", |s| {
        is(s, AgentStatus::Working) && s.activity.is_some()
    })
    .await;
    assert!(matches!(s.status.as_ref().unwrap().source, SignalSource::Hook | SignalSource::Osc));
    assert_eq!(s.activity.as_deref(), Some("Bash: sleep 1.5"));
    // SessionStart named the model, so `ptyro ls` and the pane header can say which one runs.
    assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
    // SessionStart carried the pre-assigned id back.
    let screen = w.d.wait_for_text(&c, "cl", &format!("session={conv}"), WAIT).await.unwrap();
    assert!(screen.contains("resumed=false"));
    // A turn of ≥ min_work raises attention when it ends.
    let s = wait_for(&c, "cl", "idle with attention", |s| {
        is(s, AgentStatus::Idle) && s.attention.is_some()
    })
    .await;
    assert_eq!(s.attention.as_ref().unwrap().kind, AttentionKind::Idle);
    assert!(s.activity.is_none(), "activity clears with the turn");
    // Blocked on a permission: attention says exactly what; the answer goes in as keys.
    send(&c, "cl", "ask npm install").await;
    let s = wait_for(&c, "cl", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    // Status selectors follow the status ptyro shows, here from hooks (not only self-reports).
    assert_eq!(resolve(&c, "@blocked").await, ["cl"]);
    assert_eq!(resolve(&c, "@working").await, Vec::<String>::new());
    let r = s.status.clone().unwrap();
    assert_eq!(
        (r.kind.as_deref(), r.detail.as_deref()),
        (Some("permission"), Some("Bash: npm install"))
    );
    assert_eq!(s.attention.as_ref().unwrap().text, "wants permission: Bash: npm install");
    let e = c
        .request(Request::Answer { addr: "cl".into(), decision: "maybe".into(), expect: None })
        .await
        .unwrap_err();
    assert_eq!(e.code, ptyro_core::ErrorCode::Invalid);
    w.ok(&["answer", "cl", "allow"]);
    w.d.wait_for_text(&c, "cl", "answer: yes", WAIT).await.unwrap();
    let s = wait_for(&c, "cl", "idle again", |s| is(s, AgentStatus::Idle)).await;
    assert_eq!(resolve(&c, "@idle").await, ["cl"]);
    assert_eq!(resolve(&c, "@blocked").await, Vec::<String>::new());
    assert!(
        s.attention.as_ref().is_none_or(|a| a.kind != AttentionKind::Blocked),
        "answering clears it"
    );
    // Answering when nothing is pending is a conflict.
    let e = c
        .request(Request::Answer { addr: "cl".into(), decision: "allow".into(), expect: None })
        .await
        .unwrap_err();
    assert_eq!(e.code, ptyro_core::ErrorCode::Conflict);
    // The inbox has the story, newest first, with context.
    let entries = inbox(&c).await;
    assert!(
        entries.iter().any(|e| e.kind == AttentionKind::Blocked && e.body.contains("npm install")),
        "{entries:?}"
    );
    assert!(entries.iter().any(|e| e.kind == AttentionKind::Idle));
    assert!(w.ok(&["inbox"]).contains("wants permission"));
    // Denying via Esc returns to idle without a Stop hook.
    send(&c, "cl", "ask rm -rf /").await;
    wait_for(&c, "cl", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    w.ok(&["answer", "cl", "deny"]);
    w.d.wait_for_text(&c, "cl", "answer: no", WAIT).await.unwrap();
    wait_for(&c, "cl", "idle after deny", |s| is(s, AgentStatus::Idle)).await;
    // Subagents are children while they run.
    send(&c, "cl", "team").await;
    let s = wait_for(&c, "cl", "children", |s| s.children.len() == 2).await;
    assert!(s.children.iter().all(|c| c.running));
    let entries = inbox(&c).await;
    assert!(
        !entries.iter().any(|e| e.body.contains("teammates")),
        "not in a worktree: no shared-worktree warning"
    );
    // The statusline relay feeds usage meters, and chains to (no) user statusline.
    send(&c, "cl", "usage 85").await;
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Usage { usage } = c.request(Request::Usage).await.unwrap() else { panic!() };
        if let Some(u) = usage.iter().find(|u| u.agent == "claude") {
            assert_eq!(u.profile, "default");
            assert_eq!(u.windows[0].used_percent, 85);
            assert_eq!((u.cost_millis, u.context_percent), (Some(420), Some(33)));
            break;
        }
        assert!(Instant::now() < deadline, "no usage");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(w.ok(&["usage"]).contains("5h 85%"));
    // A rate-limit failure: errored, and the account shows as limited.
    send(&c, "cl", "fail").await;
    let s = wait_for(&c, "cl", "errored", |s| is(s, AgentStatus::Errored)).await;
    assert_eq!(s.status.as_ref().unwrap().kind.as_deref(), Some("rate_limit"));
    let Reply::Usage { usage } = c.request(Request::Usage).await.unwrap() else { panic!() };
    assert!(usage.iter().find(|u| u.agent == "claude").unwrap().limited_until_ms.is_some());
    // `status --explain` tells the story.
    let out = w.ok(&["status", "cl", "--explain"]);
    assert!(out.contains("applied: working → blocked"), "{out}");
    assert!(out.contains("hook (high)"), "{out}");
    // Graceful stop: the fake honours /exit.
    w.ok(&["stop", "cl"]);
    assert!(!get(&c, "cl").await.state.is_live());
}

/// An agent whose first hook is slow (a loaded runner): its start-up output is guessed
/// "working", and the start's idle replaces the guess, so its prompt is typed. The idle used to
/// be dropped as "a session starting can't end a turn": the guess stayed for 5 minutes, and a
/// Codex, which shows no input box ptyro reads, never got its prompt (CI's Windows, m7).
#[tokio::test]
async fn a_slow_session_start_replaces_a_guessed_working() {
    // Its first hook waits for a file (it loads, with output, meanwhile): the guess certainly
    // comes first, whatever the machine's load.
    let gate = tempfile::tempdir().unwrap();
    let go = gate.path().join("go");
    let w =
        World::new(&format!("[agents.codex.env]\nPTYRO_FAKE_START_GATE = '{}'\n", go.display()));
    let c = w.d.client().await.unwrap();
    agent(&c, "cx", "codex", Some("work 0.1 hi")).await;
    wait_for(&c, "cx", "the heuristic's guess", |s| {
        is(s, AgentStatus::Working)
            && s.status.as_ref().is_some_and(|r| r.source == SignalSource::Heuristic)
    })
    .await;
    std::fs::write(&go, "").unwrap();
    let s = wait_for(&c, "cx", "a turn, idle from its hook", |s| {
        s.turns >= 1
            && is(s, AgentStatus::Idle)
            && s.status.as_ref().is_some_and(|r| r.source == SignalSource::Hook)
    })
    .await;
    assert_eq!(s.status.unwrap().confidence, Confidence::High);
    // The start's idle replaced the guess.
    let ev = w.ok(&["events", "-s", "cx", "--json", "-n", "200"]);
    let statuses: Vec<serde_json::Value> = ev
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["kind"] == "status")
        .map(|e| e["detail"].clone())
        .collect();
    assert_eq!(
        (&statuses[0]["source"], &statuses[0]["to"]),
        (&"heuristic".into(), &"working".into()),
        "{ev}"
    );
    assert_eq!(
        (&statuses[1]["source"], &statuses[1]["to"]),
        (&"hook".into(), &"idle".into()),
        "{ev}"
    );
}

/// A Claude whose redraw lags (`PTYRO_FAKE_LAGGY_BOX`, with `extra` fake knobs): its slow hooks
/// leave the status idle while a prompt it took stays in its box and its permission dialog comes,
/// as a dropped Enter would look. `prompt` is typed once it's ready, as `send --wait-ready` and a
/// first prompt are (what ptyro then watches), and the dialog is answered "allow". Returns the
/// session's events.
async fn laggy_claude_asks(extra: &str, prompt: &str) -> String {
    let w =
        World::new(&format!("[agents.claude.env]\nPTYRO_FAKE_HOOK_DELAY_MS = \"1500\"\n{extra}"));
    let c = w.d.client().await.unwrap();
    agent(&c, "lag", "claude", None).await;
    wait_for(&c, "lag", "a reliable idle", |s| {
        is(s, AgentStatus::Idle)
            && s.status.as_ref().is_some_and(|r| r.confidence == Confidence::High)
    })
    .await;
    c.request(Request::SendText {
        addr: "lag".into(),
        text: prompt.into(),
        enter: true,
        wait_ready_ms: Some(30_000),
    })
    .await
    .unwrap();
    wait_for(&c, "lag", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    w.ok(&["answer", "lag", "allow"]);
    let screen = w.d.wait_for_text(&c, "lag", "answer: ", WAIT).await.unwrap();
    assert!(screen.contains("answer: yes"), "nothing but the answer answered it: {screen}");
    // The send is reported, however long the watch takes to end.
    let deadline = Instant::now() + WAIT;
    loop {
        let ev = w.ok(&["events", "-s", "lag", "--json", "-n", "200"]);
        if ev.contains("prompt_unconfirmed") {
            assert!(!ev.contains("prompt_enter_again"), "{ev}");
            return ev;
        }
        assert!(Instant::now() < deadline, "never reported unconfirmed: {ev}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Claude's box never drops an Enter, and Enter on its dialog is "Yes": a prompt that stays in
/// its box while a dialog comes at once is reported, never entered again.
#[tokio::test]
async fn a_claude_prompt_left_in_its_box_is_reported_not_entered_again() {
    // Reported, never entered again (the helper's checks); why it was reported depends on which
    // look saw it first, which a loaded runner moves, so it isn't asserted.
    laggy_claude_asks("PTYRO_FAKE_LAGGY_BOX = \"1\"\n", "ask rm -rf build").await;
}

/// A dialog that comes between ptyro's two looks at the box (one at ~0.5 s, the next at ~1 s).
#[tokio::test]
async fn a_dialog_drawn_between_two_looks_is_reported_not_answered() {
    laggy_claude_asks("PTYRO_FAKE_LAGGY_BOX = \"700\"\n", "ask rm -rf build").await;
}

/// A dialog in words no adapter recognises, with the screen otherwise still: still no Enter.
#[tokio::test]
async fn a_dialog_no_adapter_recognises_is_never_answered() {
    laggy_claude_asks(
        "PTYRO_FAKE_LAGGY_BOX = \"1\"\nPTYRO_FAKE_PLAIN_DIALOG = \"1\"\n",
        "ask rm -rf build",
    )
    .await;
}

/// A prompt that wraps in Claude's box (wider than its 100 columns) is reported too, not lost
/// for not matching the box's first line.
#[tokio::test]
async fn a_wrapped_claude_prompt_left_in_its_box_is_reported() {
    let long = format!("ask rm -rf {}", "build/".repeat(25));
    laggy_claude_asks("PTYRO_FAKE_LAGGY_BOX = \"1\"\n", &long).await;
}

#[tokio::test]
async fn codex_gemini_antigravity_and_opencode_adapters() {
    let w = World::new("");
    // The user's own Codex notify keeps working (chained).
    let codex_home = w.home().join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let chained = w.home().join("chained.txt");
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            "notify = [\"sh\", \"-c\", \"echo \\\"$1\\\" >> {}\", \"x\"]\n",
            ptyro_testkit::program::shell_path(&chained.to_string_lossy())
        ),
    )
    .unwrap();
    let c = w.d.client().await.unwrap();
    for (name, adapter) in
        [("cx", "codex"), ("gm", "gemini"), ("ag", "antigravity"), ("oc", "opencode")]
    {
        let s = agent(&c, name, adapter, None).await;
        w.d.wait_for_text(&c, name, &format!("fake {adapter}"), WAIT).await.unwrap();
        // Its first reliable word (a hook, OSC 7501 or the sidecar) before the prompt: until it
        // comes, its start-up output can be guessed "working" (SPEC §9.3), which the wait for
        // the prompt's working below would take (a slow WSL runner, every time).
        wait_for(&c, name, "a reliable status", |s| {
            s.status.as_ref().is_some_and(|r| r.confidence == Confidence::High)
        })
        .await;
        // Codex and Antigravity learn their conversation id at start; OpenCode with its first event.
        if adapter != "opencode" {
            wait_for(&c, name, "conversation", |s| s.conversation.is_some()).await;
        }
        let _ = s;
        send(&c, name, "work 1.2 ok").await;
        let s = wait_for(&c, name, "working", |s| is(s, AgentStatus::Working)).await;
        let src = s.status.as_ref().unwrap().source;
        let want = if adapter == "opencode" { SignalSource::Sidecar } else { SignalSource::Hook };
        assert!(src == want || src == SignalSource::Osc, "{adapter}: working from {src:?}");
        let s = wait_for(&c, name, "idle + attention", |s| {
            is(s, AgentStatus::Idle) && s.attention.is_some()
        })
        .await;
        if adapter == "opencode" {
            // Its id comes with the sidecar's first SSE event, which can trail the OSC 7501
            // reports that already ended the turn.
            wait_for(&c, name, "opencode conversation", |s| s.conversation.is_some()).await;
        } else {
            assert!(s.conversation.is_some(), "{adapter} conversation");
        }
        // One turn, however many ways it ended (Codex: Stop, notify and OSC 7501 `done`) and
        // whatever its startup looked like.
        wait_for(&c, name, "a turn", |s| s.turns >= 1).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(get(&c, name).await.turns, 1, "{adapter}");
    }
    // Codex: the chained notify program got the turn's JSON.
    let deadline = Instant::now() + WAIT;
    while !std::fs::read_to_string(&chained).is_ok_and(|t| t.contains("agent-turn-complete")) {
        if Instant::now() >= deadline {
            // What there is to go on: the file, and the same line run by hand.
            let by_hand = std::process::Command::new("sh")
                .args([
                    "-c",
                    &format!(
                        "echo \"$1\" >> {}",
                        ptyro_testkit::program::shell_path(&chained.to_string_lossy())
                    ),
                    "x",
                    "by-hand",
                ])
                .output();
            panic!(
                "the user's notify wasn't chained: the file holds {:?}; by hand: {by_hand:?}, then {:?}",
                std::fs::read_to_string(&chained).ok(),
                std::fs::read_to_string(&chained).ok()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Codex's side thread (it writes the title) reports through notify too: ignored, so it
    // neither replaces the reply nor the conversation.
    let before = get(&c, "cx").await;
    c.request(Request::Hook {
        session: before.id.full(),
        adapter: "codex".into(),
        event: "notify".into(),
        payload: serde_json::json!({
            "type": "agent-turn-complete",
            "thread-id": "side-thread",
            "last-assistant-message": "{\"title\":\"Fix it\"}",
        }),
        env: Default::default(),
    })
    .await
    .unwrap();
    let after = get(&c, "cx").await;
    assert_eq!(after.conversation, before.conversation);
    assert_eq!(after.last_reply, before.last_reply);
    // Working, then idle with no turn end (Codex starting up: its own "working", then
    // SessionStart) is not a turn; a turn that ends twice (Stop, then notify) is one.
    let hook = |event: &str, payload: serde_json::Value| Request::Hook {
        session: before.id.full(),
        adapter: "codex".into(),
        event: event.into(),
        payload,
        env: Default::default(),
    };
    let thread = before.conversation.clone().unwrap();
    for (event, payload) in [
        ("PreToolUse", serde_json::json!({ "tool_name": "Bash" })),
        ("SessionStart", serde_json::json!({})),
    ] {
        c.request(hook(event, payload)).await.unwrap();
    }
    assert_eq!(get(&c, "cx").await.turns, before.turns, "startup noise isn't a turn");
    for (event, payload) in [
        ("UserPromptSubmit", serde_json::json!({ "prompt": "again" })),
        ("Stop", serde_json::json!({ "last_assistant_message": "done" })),
        (
            "notify",
            serde_json::json!({ "type": "agent-turn-complete", "thread-id": thread, "last-assistant-message": "done" }),
        ),
    ] {
        c.request(hook(event, payload)).await.unwrap();
    }
    assert_eq!(get(&c, "cx").await.turns, before.turns + 1, "one turn, ended twice");
    // Permission prompts: keys for codex and gemini, the API for OpenCode.
    for name in ["cx", "gm", "oc"] {
        send(&c, name, "ask make deploy").await;
        let s = wait_for(&c, name, "blocked", |s| is(s, AgentStatus::Blocked)).await;
        assert_eq!(s.status.as_ref().unwrap().kind.as_deref(), Some("permission"), "{name}");
        c.request(Request::Answer { addr: name.into(), decision: "allow".into(), expect: None })
            .await
            .unwrap();
        w.d.wait_for_text(&c, name, "answer: yes", WAIT).await.unwrap();
    }
    // Antigravity has no permission hook: OSC 7501 says blocked.
    send(&c, "ag", "ask x").await;
    let s = wait_for(&c, "ag", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    assert_eq!(s.status.as_ref().unwrap().source, SignalSource::Osc);
}

#[tokio::test]
async fn generic_programs_shells_and_self_reports() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    // Any OSC 7501 emitter gets confident status, no adapter needed (R-STA-01).
    let spec = SpawnSpec {
        argv: vec![BIN.into(), "fake-agent".into()],
        cwd: std::env::temp_dir(),
        name: Some("tool".into()),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    w.d.wait_for_text(&c, "tool", "osc7501=yes", WAIT).await.unwrap();
    send(&c, "tool", "work 1.2").await;
    let s = wait_for(&c, "tool", "working", |s| is(s, AgentStatus::Working)).await;
    assert_eq!(s.status.as_ref().unwrap().confidence, Confidence::High);
    // A one-shot tool's `done` is done, not idle.
    let s = wait_for(&c, "tool", "done", |s| is(s, AgentStatus::Done)).await;
    assert_eq!(s.attention.as_ref().unwrap().kind, AttentionKind::Done);
    // A shell with OSC 133: a long command finishing wants you; a short one doesn't.
    let script = r"printf '\033]133;A\007$ \033]133;C\007'; sleep 1.5; printf '\033]133;D;3\007\033]133;A\007$ '; sleep 30";
    let spec = SpawnSpec {
        argv: vec!["sh".into(), "-c".into(), script.into()],
        cwd: std::env::temp_dir(),
        name: Some("sh1".into()),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    let _ = wait_for(&c, "sh1", "a command", |s| s.last_command_exit == Some(3)).await;
    // (It's a command session running sh, not a `shell` kind: OSC 133 there doesn't set status.)
    // Self report from inside (herdr-compatible) and via the CLI.
    w.ok(&["report", "--session", "tool", "--state", "needs_input", "--msg", "pick a branch"]);
    let s = wait_for(&c, "tool", "self-reported blocked", |s| is(s, AgentStatus::Blocked)).await;
    assert_eq!(s.status.as_ref().unwrap().source, SignalSource::Report);
    assert_eq!(s.attention.as_ref().unwrap().text, "has a question: pick a branch");
    assert!(!w.cli(&["report", "--session", "tool", "--state", "sleepy"]).status.success());
    // Focusing a session clears its attention (and marks its inbox entries read).
    c.send(ClientMsg::Focus { session: s.id, terminal_focused: true }).unwrap();
    wait_for(&c, "tool", "attention cleared", |s| s.attention.is_none()).await;
    let entries = inbox(&c).await;
    assert!(
        entries.iter().filter(|e| e.session_name == "tool").all(|e| e.read_ms.is_some()),
        "{entries:?}"
    );
    // …and while watched, nothing new is raised.
    w.ok(&["report", "--session", "tool", "--state", "error", "--msg", "boom"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(get(&c, "tool").await.attention.is_none(), "watched in a focused terminal");
    c.send(ClientMsg::Focus { session: s.id, terminal_focused: false }).unwrap();
    // Let the message leave before blocking this (current-thread) runtime on the CLI.
    tokio::time::sleep(Duration::from_millis(200)).await;
    w.ok(&["report", "--session", "tool", "--state", "blocked", "--msg", "again"]);
    wait_for(&c, "tool", "raised when unfocused", |s| s.attention.is_some()).await;
}

#[tokio::test]
async fn dormant_resume_and_hibernate_across_daemon_restarts() {
    let mut w = World::new("");
    let c = w.d.client().await.unwrap();
    let s = agent(&c, "keeper", "claude", None).await;
    let conv = s.conversation.clone().unwrap();
    w.d.wait_for_text(&c, "keeper", "resumed=false", WAIT).await.unwrap();
    // Hibernate: stops gracefully, stays resumable.
    w.ok(&["hibernate", "keeper"]);
    let s = wait_for(&c, "keeper", "dormant", |s| s.state == ProcessState::Dormant).await;
    assert_eq!(s.conversation.as_deref(), Some(conv.as_str()));
    w.ok(&["resume", "keeper"]);
    let screen = w.d.wait_for_text(&c, "keeper", "resumed=true", WAIT).await.unwrap();
    assert!(screen.contains(&format!("session={conv}")), "same conversation: {screen}");
    let s = get(&c, "keeper").await;
    assert_eq!(s.id, get(&c, "keeper").await.id);
    // A daemon restart: agents hibernate on the way down and come back dormant.
    let codex = agent(&c, "cdx", "codex", None).await;
    let codex_conv = wait_for(&c, "cdx", "conversation", |s| s.conversation.is_some())
        .await
        .conversation
        .unwrap();
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let s = get(&c, "keeper").await;
    assert_eq!(s.state, ProcessState::Dormant, "{s:?}");
    assert_eq!(s.id, codex.id.max(s.id).min(s.id), "ids survive");
    let cx = get(&c, "cdx").await;
    assert_eq!(
        (cx.state.clone(), cx.conversation.as_deref()),
        (ProcessState::Dormant, Some(codex_conv.as_str()))
    );
    // Opening it in a focused terminal resumes it (resume_on_focus).
    c.send(ClientMsg::Focus { session: cx.id, terminal_focused: true }).unwrap();
    let screen = w.d.wait_for_text(&c, "cdx", "resumed=true", WAIT).await.unwrap();
    assert!(screen.contains(&format!("session={codex_conv}")), "{screen}");
    // Removing a dormant one makes it stay gone.
    w.ok(&["rm", "keeper"]);
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    assert!(c.request(Request::Get { addr: "keeper".into() }).await.is_err());
    assert_eq!(get(&c, "cdx").await.state, ProcessState::Dormant);
}

#[tokio::test]
async fn notifications_sinks_mutes_and_signed_links() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("sink.jsonl");
    let cfg = format!(
        "[notifications]\nchannels = [\"inbox\", \"sinks\"]\non = [\"blocked\", \"idle\", \"errored\"]\nsinks = [{{ type = \"command\", command = [\"sh\", \"-c\", \"cat >> {} && echo >> {}\"], on = [\"blocked\", \"idle\"] }}]\ncallback = {{ listen = \"127.0.0.1:{port}\" }}\n",
        ptyro_testkit::program::shell_path(&log.to_string_lossy()),
        ptyro_testkit::program::shell_path(&log.to_string_lossy())
    );
    let w = World::new(&cfg);
    let c = w.d.client().await.unwrap();
    agent(&c, "n1", "claude", None).await;
    w.d.wait_for_text(&c, "n1", "resumed=false", WAIT).await.unwrap();
    send(&c, "n1", "ask terraform apply").await;
    wait_for(&c, "n1", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    // The sink got the notification, with approve/deny links.
    let deadline = Instant::now() + WAIT;
    let note: serde_json::Value = loop {
        if let Some(line) = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .find(|l| l.contains("terraform"))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
        {
            break v;
        }
        assert!(Instant::now() < deadline, "the sink never ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(note["kind"], "blocked");
    assert!(note["title"].as_str().unwrap().contains("n1"));
    let approve = note["approve_url"].as_str().expect("approve link").to_owned();
    assert!(approve.starts_with(&format!("http://127.0.0.1:{port}/a/")));
    let http = |method: &str, url: &str| -> (u16, String) {
        let out = std::process::Command::new("curl")
            .args(["-s", "-o", "-", "-w", "\n%{http_code}", "-X", method, url])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
        (code.trim().parse().unwrap_or(0), body.to_owned())
    };
    if std::process::Command::new("curl").arg("--version").output().is_err() {
        eprintln!("skipped link checks: no curl");
        return;
    }
    // GET only shows a confirmation page (link previews can't approve anything).
    let (code, body) = http("GET", &approve);
    assert_eq!(code, 200);
    assert!(body.contains("Approve") && body.contains("terraform apply"), "{body}");
    assert!(
        get(&c, "n1").await.status.unwrap().status == AgentStatus::Blocked,
        "still blocked after GET"
    );
    // POST approves; the link is single-use; a tampered one is refused.
    let (code, _) = http("POST", &approve);
    assert_eq!(code, 200);
    w.d.wait_for_text(&c, "n1", "answer: yes", WAIT).await.unwrap();
    assert_eq!(http("POST", &approve).0, 410, "single use");
    let tampered = format!("{}x", &approve[..approve.len() - 1]);
    assert_eq!(http("GET", &tampered).0, 410);
    // Muted: the inbox still gets it, the sink doesn't.
    let before = std::fs::read_to_string(&log).unwrap().lines().count();
    w.ok(&["mute", "n1", "--for", "10m"]);
    send(&c, "n1", "ask second").await;
    wait_for(&c, "n1", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), before, "muted");
    assert!(inbox(&c).await.iter().any(|e| e.body.contains("second")), "the inbox still has it");
    // notify test reports per channel.
    let out = w.ok(&["notify", "test"]);
    assert!(out.contains("inbox: added") && out.contains("command: ran"), "{out}");
}

/// The approve/deny links are plain HTTP: like `ptyro serve`, they listen only on loopback or
/// Tailscale unless `insecure = true`.
#[tokio::test]
async fn approve_links_refuse_a_public_address() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let w = World::new(&format!("[notifications]\ncallback = {{ listen = \"0.0.0.0:{port}\" }}\n"));
    let _c = w.d.client().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "it listened on 0.0.0.0");
}

#[tokio::test]
async fn after_turn_runs_trusted_scripts_and_branches_follow_prompts() {
    let w = World::new("[worktrees]\nfetch = false\nauto_rename_branch = true\n");
    let h = w.home();
    w.git(&h, &["init", "-q", "--bare", "r.git"]);
    w.git(&h, &["clone", "-q", "r.git", "r"]);
    let root = h.join("r");
    std::fs::write(
        root.join(".ptyro.toml"),
        "[scripts]\nshell = \"bash\"\nafter_turn = \"echo linted-$PTYRO_WORKTREE_NAME\"\n",
    )
    .unwrap();
    w.git(&root, &["add", "-A"]);
    w.git(&root, &["commit", "-q", "-m", "init"]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["project", "trust", "r", "--yes"]);
    let c = w.d.client().await.unwrap();
    let Reply::WorktreeCreated { info, agent: Some(a), .. } = c
        .request(Request::WorktreeNew {
            spec: ptyro_core::model::WorktreeSpec {
                project: "r".into(),
                name: Some("job".into()),
                agent: Some("claude".into()),
                prompt: Some("work 0.3 Fix the login bug".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
    else {
        panic!("agent should start right away (no setup)")
    };
    // The first prompt names the branch (not pushed yet).
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Worktree { info: wt } =
            c.request(Request::WorktreeGet { addr: info.id.clone() }).await.unwrap()
        else {
            panic!()
        };
        if wt.branch.as_deref() == Some("work-0-3-fix-the-login") {
            break;
        }
        assert!(Instant::now() < deadline, "branch not renamed: {:?}", wt.branch);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The turn ended: after_turn ran in its own script session.
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Sessions { sessions } = c.request(Request::List).await.unwrap() else {
            panic!()
        };
        if let Some(s) = sessions.iter().find(|s| s.script.as_deref() == Some("after_turn")) {
            w.d.wait_for_text(&c, &s.name, "linted-job", WAIT).await.unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "after_turn never ran");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = a;
}

async fn run(s: Script) -> Report {
    ptyro_testkit::headless::run(s).await.expect("headless run")
}

#[tokio::test]
async fn tui_attention_peek_answer_and_inbox() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    agent(&c, "busy", "claude", None).await;
    agent(&c, "asker", "claude", None).await;
    w.d.wait_for_text(&c, "asker", "resumed=false", WAIT).await.unwrap();
    send(&c, "asker", "ask docker system prune").await;
    wait_for(&c, "asker", "blocked", |s| is(s, AgentStatus::Blocked)).await;
    let r = run(w
        .tui(&["attach", "busy"])
        .snapshot(1.4)
        // a: jump to the one that needs us.
        .send(1.6, "\\x11a")
        .snapshot(2.1)
        // p: peek, then y answers.
        .send(2.3, "\\x11p")
        .snapshot(2.9)
        .send(3.1, "y")
        .snapshot(4.0)
        .send(4.2, "\\x11i")
        .snapshot(4.7)
        .send(5.0, "\\e")
        .send(5.3, "\\x11d"))
    .await;
    let start = r.at(1.4);
    assert!(
        start.contains("◉ asker") && start.contains("⚠ permission"),
        "the sidebar shows who's blocked: {start}"
    );
    assert!(start.contains("✉ 1"), "unread inbox count: {start}");
    let jumped = r.at(2.1);
    assert!(
        jumped.contains("asker · fake-agent") || jumped.contains("◉ asker ·"),
        "focused on asker: {jumped}"
    );
    let peek = r.at(2.9);
    assert!(peek.contains("Peek asker") && peek.contains("Do you want to proceed?"), "{peek}");
    assert!(peek.contains("y allow"), "{peek}");
    assert!(r.at(4.0).contains("answer: yes"), "answered from the peek: {}", r.at(4.0));
    let inbox = r.at(4.7);
    assert!(
        inbox.contains("Inbox") && inbox.contains("wants permission: Bash: docker system prune"),
        "{inbox}"
    );
}

#[tokio::test]
async fn osc7501_conformance_and_auto_hibernation() {
    let w = World::new(&format!(
        "[agents.sleepy]\nadapter = \"claude\"\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"claude\"]\nhibernate_after = \"1s\"\n"
    ));
    let c = w.d.client().await.unwrap();
    // A plain program's records: invalid ones discarded, subtrees cleared, the latest speaks.
    let spec = SpawnSpec {
        argv: vec![BIN.into(), "fake-agent".into()],
        cwd: std::env::temp_dir(),
        name: Some("conf".into()),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    w.d.wait_for_text(&c, "conf", "osc7501=yes", WAIT).await.unwrap();
    send(&c, "conf", "conformance").await;
    w.d.wait_for_text(&c, "conf", "conformance sent", WAIT).await.unwrap();
    let s = wait_for(&c, "conf", "lint record", |s| {
        s.status.as_ref().is_some_and(|r| r.detail.as_deref() == Some("linting"))
    })
    .await;
    assert!(is(&s, AgentStatus::Working));
    let raw = s.program_status.unwrap();
    assert_eq!(
        raw.msg.as_deref(),
        Some("linting"),
        "build/* cleared, the invalid and control-character reports discarded"
    );
    let out = w.ok(&["status", "conf", "--explain"]);
    assert!(
        !out.contains("blocked"),
        "the report with a newline in msg was discarded whole: {out}"
    );
    // An idle agent nobody watches hibernates on its own, and stays resumable.
    agent(&c, "zz", "sleepy", None).await;
    w.d.wait_for_text(&c, "zz", "resumed=false", WAIT).await.unwrap();
    let s = wait_for(&c, "zz", "dormant", |s| s.state == ProcessState::Dormant).await;
    assert!(s.conversation.is_some());
    w.ok(&["resume", "zz"]);
    w.d.wait_for_text(&c, "zz", "resumed=true", WAIT).await.unwrap();
}

/// Codex 0.166 sends SessionStart only with the first prompt, after OSC 7501 has said working:
/// its idle doesn't end the turn, so status goes working → idle once, at Stop (WIN8).
#[tokio::test]
async fn a_late_session_start_does_not_end_the_turn() {
    let w = World::new("[agents.codex.env]\nPTYRO_FAKE_LATE_SESSION_START = \"1\"\n");
    let c = w.d.client().await.unwrap();
    agent(&c, "cx", "codex", None).await;
    let out = w.cli(&["send", "cx", "work 1 slow", "--wait-ready", "--wait", "--json"]);
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && o.contains("\"idle\""),
        "{o}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = w.cli(&["events", "-s", "cx", "--json", "-n", "200"]);
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&ev.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let hook = |l: &serde_json::Value, e: &str| l["kind"] == "hook" && l["detail"]["event"] == e;
    let start = lines
        .iter()
        .position(|l| hook(l, "SessionStart"))
        .unwrap_or_else(|| panic!("SessionStart came: {lines:#?}"));
    let stop = lines.iter().position(|l| hook(l, "Stop")).expect("the turn ended");
    assert!(start < stop);
    let idle_between =
        lines[start..stop].iter().any(|l| l["kind"] == "status" && l["detail"]["to"] == "idle");
    assert!(!idle_between, "SessionStart ended the turn: {lines:#?}");
}

/// A prompt wider than the box (100 columns here) whose Enter is lost: it wraps, the box's first
/// line holds its start, and it is still pressed again, once.
#[tokio::test]
async fn a_wrapped_prompt_whose_enter_was_lost_is_sent() {
    let w = World::new(
        "[agents.codex.env]\nPTYRO_FAKE_LATE_SESSION_START = \"1\"\nPTYRO_FAKE_DROP_FIRST_ENTER = \"1\"\n",
    );
    let c = w.d.client().await.unwrap();
    agent(&c, "cx", "codex", None).await;
    let long = format!("work 0.2 {}", "a long prompt ".repeat(10));
    let out = w.cli(&["send", "cx", &long, "--wait-ready", "--wait", "--timeout", "30s", "--json"]);
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && o.contains("\"idle\""),
        "{o}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = String::from_utf8_lossy(&w.cli(&["events", "-s", "cx", "--json", "-n", "200"]).stdout)
        .into_owned();
    assert_eq!(ev.lines().filter(|l| l.contains("prompt_enter_again")).count(), 1, "{ev}");
    assert!(ev.contains("\"Stop\""), "the turn ran: {ev}");
}

/// A prompt whose Enter is lost (Codex 0.166, busy just after it says idle) is still in the
/// composer: ptyro presses Enter again, once, and the turn happens (WIN8).
#[tokio::test]
async fn a_prompt_whose_enter_was_lost_is_sent() {
    let w = World::new(
        "[agents.codex.env]\nPTYRO_FAKE_LATE_SESSION_START = \"1\"\nPTYRO_FAKE_DROP_FIRST_ENTER = \"1\"\n",
    );
    let c = w.d.client().await.unwrap();
    agent(&c, "cx", "codex", None).await;
    let out = w.cli(&[
        "send",
        "cx",
        "work 0.2 hi",
        "--wait-ready",
        "--wait",
        "--timeout",
        "30s",
        "--json",
    ]);
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && o.contains("\"idle\""),
        "{o}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = String::from_utf8_lossy(&w.cli(&["events", "-s", "cx", "--json", "-n", "200"]).stdout)
        .into_owned();
    let again = ev.lines().filter(|l| l.contains("prompt_enter_again")).count();
    assert_eq!(again, 1, "one Enter more, no more: {ev}");
    assert!(ev.contains("\"Stop\""), "the turn ran: {ev}");
    let screen = String::from_utf8_lossy(&w.cli(&["read", "cx"]).stdout).into_owned();
    assert!(!screen.contains("a second prompt while one waits"), "{screen}");
}
