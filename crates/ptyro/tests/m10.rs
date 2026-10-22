//! M10 end-to-end tests: structured sessions. The Done-when (fan-out workers run as structured
//! sessions with full tool-call visibility in the compare view, and any of them can be opened in
//! its native TUI to take over by hand, and back), every protocol's turn, permission request and
//! answer (Claude stream-json, Codex app-server, Gemini ACP, OpenCode's server), the chat stream
//! and the line editor, a live upgrade in the middle of a structured turn, hibernate/resume,
//! an agent that fails to start, and a phone reading a transcript. Every agent is the fake.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_core::input::{InputEvent, KeyCode, KeyEvent, Mods};
use ptyro_protocol::{ClientMsg, ServerMsg};
use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m10-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config =
            String::from("[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n");
        for a in ["claude", "codex", "gemini", "opencode"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        config.push_str(extra);
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

    fn info(&self, s: &str) -> Value {
        self.json(&["status", s, "--json"])["session"].clone()
    }

    fn status(&self, s: &str) -> String {
        self.info(s)["status"]["status"].as_str().unwrap_or_default().to_owned()
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn chat(&self, s: &str) -> Vec<Value> {
        self.json(&["read", s, "--chat", "--json"]).as_array().cloned().unwrap_or_default()
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn project(&self, name: &str) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("f.txt"), "one\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.ok(&["project", "add", root.to_str().unwrap()]);
        root
    }
}

/// The Done-when: a fan-out of structured workers; the compare view shows every candidate's tool
/// calls; one is opened in its native TUI (same conversation) and taken over by hand, then
/// switched back.
#[test]
fn done_when_structured_fanout_with_tool_visibility_and_takeover() {
    let w = World::new("");
    w.project("app");
    let out = w.ok(&[
        "fanout",
        "--agents",
        "claude,codex,gemini",
        "--structured",
        "-p",
        "sh echo $PTYRO_STEP_ID > answer.txt",
        "--project",
        "app",
        "--test",
        "grep -q claude answer.txt",
        "--name",
        "ans",
    ]);
    let run = out.lines().next().unwrap().trim().to_owned();
    w.until("the fan-out", || {
        let r = w.json(&["run", "show", &run, "--json"]);
        assert_ne!(r["state"]["status"], "failed", "{r:#}");
        r["state"]["status"] == "succeeded"
    });
    let sessions = w.json(&["ls", "--json"]);
    for name in ["ans-claude", "ans-codex", "ans-gemini"] {
        let s =
            sessions.as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| {
                panic!("{name}: {sessions:#}\n{:#}", w.json(&["run", "show", &run, "--json"]))
            });
        assert!(s["structured"].is_string(), "{name} runs structured: {s:#}");
    }
    let doc = w.json(&["compare", &run, "--json"]);
    let cands = doc["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 3);
    for c in cands {
        let t = &c["tools"];
        assert!(t["calls"].as_u64() >= Some(1), "{c:#}");
        assert!(
            t["commands"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("echo")),
            "{c:#}"
        );
    }
    let claude = cands.iter().find(|c| c["step"] == "claude").unwrap();
    assert_eq!(claude["rank"], 1, "only claude's answer passes the test: {doc:#}");
    assert_eq!(claude["tools"]["cost_micro_usd"], 1200, "Claude reports its cost");
    assert_eq!(claude["last_message"], "ran: echo $PTYRO_STEP_ID > answer.txt");
    let text = w.ok(&["compare", &run]);
    assert!(text.contains("tools: 1 call (Bash 1)"), "{text}");

    // Take one over by hand: the same conversation, in the agent's own TUI.
    let conv = w.info("ans-claude")["conversation"].as_str().unwrap().to_owned();
    w.ok(&["convert", "ans-claude", "--to", "tui"]);
    let deadline = Instant::now() + WAIT;
    let screen = loop {
        let t = w.ok(&["read", "ans-claude"]);
        if t.contains("resumed=true") {
            break t;
        }
        assert!(Instant::now() < deadline, "never resumed in the TUI:\n{t}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(screen.contains(&format!("session={conv}")), "{screen}");
    let i = w.info("ans-claude");
    assert!(i["structured"].is_null(), "{i:#}");
    assert_eq!(i["conversation"], conv.as_str());
    w.ok(&["send", "ans-claude", "work 0.1 by hand", "--wait-ready", "--wait"]);
    // And back to structured, still the same conversation.
    w.ok(&["convert", "ans-claude", "--to", "structured"]);
    w.until("structured again", || w.info("ans-claude")["structured"] == "stream-json");
    assert_eq!(w.info("ans-claude")["conversation"], conv.as_str());
    w.ok(&["send", "ans-claude", "hello again", "--wait-ready", "--wait"]);
    assert!(
        w.chat("ans-claude")
            .iter()
            .any(|i| i["t"] == "assistant" && i["text"] == "you said: hello again")
    );
    // Candidates are alternatives, also once the run is over: the radar doesn't warn them about
    // each other (that mail would run as a turn ahead of the prompt above).
    let mail = w.json(&["msg", "ls", "--json"]);
    assert!(!mail.as_array().unwrap().iter().any(|m| m["kind"] == "collision"), "{mail:#}");
    // Converting needs the mode to differ.
    let again = w.cli(&["convert", "ans-claude", "--to", "structured"]);
    assert_eq!(again.status.code(), Some(5), "{}", String::from_utf8_lossy(&again.stderr));
}

/// Every protocol: a turn, a permission request answered (allow, then deny), the transcript,
/// the status from the protocol, the conversation id.
#[test]
fn every_protocol_turns_and_permissions() {
    let w = World::new("");
    for (agent, protocol) in [
        ("claude", "stream-json"),
        ("codex", "app-server"),
        ("gemini", "acp"),
        ("opencode", "opencode"),
    ] {
        let name = format!("s-{agent}");
        w.ok(&["new", "-d", "-n", &name, "--agent", agent, "--structured"]);
        let out = w.ok(&["send", &name, "work 0.2 first", "--wait-ready", "--wait"]);
        assert!(out.contains("idle"), "{agent}: {out}");
        let i = w.info(&name);
        assert_eq!(i["structured"], protocol);
        assert_eq!(i["status"]["source"], "protocol", "{agent}: {i:#}");
        assert!(i["conversation"].is_string(), "{agent}: {i:#}");
        assert!(i["turns"].as_u64() >= Some(1), "{agent}: {i:#}");
        for (cmd, decision, word) in [("ask rm one", "allow", "yes"), ("ask rm two", "deny", "no")]
        {
            w.ok(&["send", &name, cmd, "--wait-ready"]);
            w.until(&format!("{agent} blocked"), || w.status(&name) == "blocked");
            w.ok(&["answer", &name, decision]);
            w.until(&format!("{agent} answered {word}"), || {
                w.chat(&name)
                    .iter()
                    .any(|i| i["t"] == "assistant" && i["text"] == format!("answer: {word}"))
            });
            w.until(&format!("{agent} idle"), || w.status(&name) == "idle");
        }
        let chat = w.chat(&name);
        let kinds: Vec<&str> = chat.iter().map(|i| i["t"].as_str().unwrap()).collect();
        for k in ["user", "assistant", "tool", "permission", "turn_end"] {
            assert!(kinds.contains(&k), "{agent}: {k} missing from {kinds:?}");
        }
        assert!(
            chat.iter().filter(|i| i["t"] == "permission").all(|p| p["decided"].is_string()),
            "{agent}: {chat:#?}"
        );
        // The terminal view shows it too.
        let screen = w.ok(&["read", &name, "--scrollback", "200"]);
        assert!(
            screen.contains("› work 0.2 first")
                && screen.contains("worked: first")
                && screen.contains("wants permission"),
            "{agent}:\n{screen}"
        );
    }
    // An agent with no structured mode says so.
    let out = w.cli(&["new", "-d", "-n", "nope", "--structured", "--", "sh"]);
    assert!(!out.status.success());
    w.ok(&["new", "-d", "-n", "x", "--agent", "claude"]);
    let out = w.cli(&["read", "x", "--chat"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a structured session"));
}

/// A subscriber gets the transcript as it grows, the draft as it's typed, and keys answer an
/// open permission request.
#[tokio::test]
async fn the_chat_stream_and_the_line_editor() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "c1", "--agent", "claude", "--structured"]);
    let id = w.info("c1")["id"].as_str().unwrap().to_owned();
    let mut c = w.d.client().await.unwrap();
    let pushes = c.pushes();
    let sid = id.parse().unwrap();
    c.send(ClientMsg::Subscribe { session: sid, size: None, control: true }).unwrap();
    let key = |code: KeyCode, mods: Mods| ClientMsg::Input {
        session: sid,
        event: InputEvent::Key { key: KeyEvent::new(code, mods) },
    };
    for ch in "work 0.3 streamed".chars() {
        c.send(key(KeyCode::Char { c: ch }, Mods::NONE)).unwrap();
    }
    // The draft arrives with the chat messages.
    let mut st = Stream { pushes, items: Vec::new(), draft: String::new(), streaming: false };
    st.until(|s| s.draft == "work 0.3 streamed").await;
    c.send(key(KeyCode::Enter, Mods::NONE)).unwrap();
    st.until(|s| s.draft.is_empty() && s.items.iter().any(|i| i["t"] == "turn_end")).await;
    assert!(st.streaming, "text streamed in pieces before it was final");
    assert!(
        st.items.iter().any(|i| i["t"] == "assistant"
            && i["text"] == "worked: streamed"
            && i["streaming"] == false),
        "{:#?}",
        st.items
    );
    assert!(
        st.items
            .iter()
            .any(|i| i["t"] == "tool" && i["status"] == "done" && i["title"] == "sleep 0.3"),
        "{:#?}",
        st.items
    );
    // A permission request, answered with a key.
    for ch in "ask rm -rf build".chars() {
        c.send(key(KeyCode::Char { c: ch }, Mods::NONE)).unwrap();
    }
    c.send(key(KeyCode::Enter, Mods::NONE)).unwrap();
    st.until(|s| s.items.iter().any(|i| i["t"] == "permission" && i["decided"].is_null())).await;
    c.send(key(KeyCode::Char { c: 'd' }, Mods::NONE)).unwrap();
    st.until(|s| s.items.iter().any(|i| i["t"] == "assistant" && i["text"] == "answer: no")).await;
    assert!(st.items.iter().any(|i| i["t"] == "permission" && i["decided"] == "deny"));
    // A late subscriber gets everything at once.
    let mut c2 = w.d.client().await.unwrap();
    let mut p2 = c2.pushes();
    c2.send(ClientMsg::Subscribe { session: sid, size: None, control: false }).unwrap();
    let first = loop {
        if let ServerMsg::Chat { reset: true, items, .. } =
            tokio::time::timeout(WAIT, p2.recv()).await.unwrap().unwrap()
        {
            break items;
        }
    };
    assert!(first.len() >= 8, "{first:#?}");
}

/// What a chat subscriber has seen.
struct Stream {
    pushes: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    items: Vec<Value>,
    draft: String,
    streaming: bool,
}

impl Stream {
    async fn until(&mut self, want: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !want(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.pushes.recv()).await {
                Ok(Some(ServerMsg::Chat { items, draft, reset, .. })) => {
                    if reset {
                        self.items.clear();
                    }
                    for it in items {
                        let v = serde_json::to_value(&it).unwrap();
                        if v["t"] == "assistant" && v["streaming"] == true {
                            self.streaming = true;
                        }
                        match self.items.iter().position(|x| x["id"] == v["id"]) {
                            Some(i) => self.items[i] = v,
                            None => self.items.push(v),
                        }
                    }
                    self.draft = draft;
                }
                Ok(Some(_)) => {}
                _ => panic!("timed out; items {:#?}, draft {:?}", self.items, self.draft),
            }
        }
    }
}

/// A live upgrade in the middle of a structured turn: the driver parks between lines, its state
/// and the pipes cross the exec, and the turn finishes on the new image with the transcript whole.
#[test]
fn a_structured_turn_survives_a_live_upgrade() {
    let w = World::new("");
    w.ok(&["new", "-d", "-n", "u1", "--agent", "codex", "--structured"]);
    w.ok(&["send", "u1", "work 0.1 before", "--wait-ready", "--wait"]);
    let before = w.info("u1");
    w.ok(&["send", "u1", "work 2 across", "--wait-ready"]);
    w.until("working", || w.status("u1") == "working");
    let up = w.json(&["daemon", "upgrade", "--json"]);
    assert_eq!(up["rolled_back"], false, "{up}");
    w.until("the turn finished on the new image", || {
        w.chat("u1").iter().any(|i| i["t"] == "assistant" && i["text"] == "worked: across")
    });
    let after = w.info("u1");
    assert_eq!(after["pid"], before["pid"], "the same agent process");
    assert_eq!(after["conversation"], before["conversation"]);
    assert_eq!(after["structured"], "app-server");
    let chat = w.chat("u1");
    assert!(
        chat.iter().any(|i| i["t"] == "assistant" && i["text"] == "worked: before"),
        "the transcript came across: {chat:#?}"
    );
    w.until("idle", || w.status("u1") == "idle");
    w.ok(&["send", "u1", "after the upgrade", "--wait-ready", "--wait"]);
    assert!(w.chat("u1").iter().any(|i| i["text"] == "you said: after the upgrade"));
}

/// Hibernate and resume keep a session structured and its conversation; an agent that fails at
/// start shows why; a phone reads the transcript.
#[test]
fn resume_failures_and_remote_reading() {
    let w = World::new(
        "[agents.broken]\ncommand = [\"sh\", \"-c\", \"echo 'error: not logged in (run broken login)' >&2; exit 3\"]\nprotocol = \"acp\"\n",
    );
    w.ok(&["new", "-d", "-n", "h1", "--agent", "claude", "--structured"]);
    w.ok(&["send", "h1", "remember this", "--wait-ready", "--wait"]);
    let conv = w.info("h1")["conversation"].as_str().unwrap().to_owned();
    w.ok(&["hibernate", "h1"]);
    w.until("dormant", || w.info("h1")["state"]["t"] == "dormant");
    w.ok(&["resume", "h1"]);
    w.until("structured again", || {
        w.info("h1")["structured"] == "stream-json" && w.info("h1")["state"]["t"] == "running"
    });
    assert_eq!(w.info("h1")["conversation"], conv.as_str());
    w.ok(&["send", "h1", "still here", "--wait-ready", "--wait"]);

    w.ok(&["new", "-d", "-n", "b1", "--agent", "broken", "--structured"]);
    w.until("the failure is in the transcript", || {
        w.chat("b1").iter().any(|i| {
            i["t"] == "notice" && i["text"].as_str().unwrap_or("").contains("not logged in")
        })
    });

    // A paired phone (read scope) reads the transcript.
    let v = w.json(&["pair", "phone", "--scope", "read", "--json"]);
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        // The code is traded for the token (what `ptyro serve` does for the web app).
        let local = w.d.client().await.unwrap();
        let claim = ptyro_protocol::Request::DeviceClaim { code, via: None };
        let ptyro_protocol::Reply::Paired { token, .. } = local.request(claim).await.unwrap()
        else {
            panic!("not paired")
        };
        let mut opts =
            ptyro_client::ConnectOptions::new(w.d.paths.clone(), ptyro_protocol::ClientKind::Web);
        opts.autospawn = false;
        opts.token = Some(token);
        let c = ptyro_client::Client::connect(&opts).await.unwrap();
        let r = c.request(ptyro_protocol::Request::ChatGet { addr: "h1".into() }).await.unwrap();
        let ptyro_protocol::Reply::Transcript { items } = r else { panic!("{r:?}") };
        assert!(items.iter().any(
            |i| matches!(&i.body, ptyro_core::chat::ChatBody::User { text } if text == "still here")
        ));
        let typed = c
            .request(ptyro_protocol::Request::SendText {
                addr: "h1".into(),
                text: "x".into(),
                enter: true,
                wait_ready_ms: None,
            })
            .await;
        assert!(typed.is_err(), "read scope can't type");
    });
}
