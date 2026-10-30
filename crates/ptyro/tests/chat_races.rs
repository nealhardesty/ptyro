//! Chats under changes that race and lives that outlast a look (SPEC §22.6–§22.9, §22.14),
//! against a real daemon whose agents are the fakes: a change of agent waits for the answer, a
//! cut-off turn isn't sent again, a question from a resumed agent alerts and opening the chat
//! clears it, an incognito chat lasts while it is read, and a chat followed by one address stops
//! by another.

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_protocol::{ClientMsg, Reply, Request};
use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    /// `chat`: more `[chat]` lines.
    fn new(chat: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-races-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = format!(
            "[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo Titled\"]\n{chat}\n"
        );
        for a in ["claude", "codex"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
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

    fn chats(&self) -> Vec<Value> {
        self.json(&["chat", "ls", "--json"]).as_array().cloned().unwrap_or_default()
    }

    /// The only chat (an incognito one too).
    fn the_chat(&self) -> Value {
        let all = self.chats();
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].clone()
    }

    fn show(&self, addr: &str) -> Value {
        self.json(&["chat", "show", addr, "--format", "json"])
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn texts(chat: &Value, kind: &str) -> Vec<String> {
    chat["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == kind)
        .map(|n| n["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn a_change_of_agent_waits_for_the_answer() {
    let w = World::new("");
    w.ok(&["chat", "new", "--no-wait", "work 30 slowly"]);
    let addr = format!("c:{}", w.the_chat()["handle"].as_str().unwrap());
    w.until("answering", || w.the_chat()["status"] == "working");
    // Like a retry, a change that needs a new agent is refused mid-answer, saying what to do.
    for args in [
        vec!["chat", "set", addr.as_str(), "--agent", "codex"],
        vec!["chat", "set", addr.as_str(), "--preset", "none"],
    ] {
        let out = w.cli(&args);
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("still answering") && err.contains("stop it first"), "{err}");
        assert!(!err.contains("ptyro chat stop"), "it offers the fix, not a command: {err}");
    }
    assert_eq!(w.the_chat()["agent"], "claude", "nothing changed");
    assert_eq!(w.the_chat()["status"], "working", "the answer goes on");
    // A pin is the person's at any time.
    w.ok(&["chat", "pin", &addr]);
}

#[test]
fn a_turn_cut_off_by_a_restart_is_not_sent_again() {
    let mut w = World::new("");
    w.ok(&["chat", "new", "hello"]);
    let addr = format!("c:{}", w.the_chat()["handle"].as_str().unwrap());
    w.ok(&["chat", "send", "--no-wait", &addr, "work 30 slowly"]);
    w.until("answering", || w.the_chat()["status"] == "working");
    w.d.restart().unwrap();
    // Its conversation had the cut-off message already: carried on natively, nothing seeded.
    let out = w.ok(&["chat", "send", &addr, "next"]);
    assert!(out.contains("you said: next"), "{out}");
    assert!(!out.contains("The conversation so far"), "sent again: {out}");
}

#[test]
fn a_resumed_agents_question_alerts_and_opening_the_chat_clears_it() {
    let w = World::new("hibernate_after = \"1s\"");
    w.ok(&["chat", "new", "hello"]);
    let addr = format!("c:{}", w.the_chat()["handle"].as_str().unwrap());
    w.until("asleep", || w.the_chat()["status"].is_null());
    // Resumed, its first word is a question: that is news, though a resume's errors aren't.
    w.ok(&["chat", "send", "--no-wait", &addr, "ask touch /tmp/x"]);
    w.until("the question alerts", || w.the_chat()["attention"]["kind"] == "blocked");
    // Opened: seen, so its alert goes (the question stays to answer).
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut c = w.d.client().await.unwrap();
        let mut pushes = c.pushes();
        c.send(ClientMsg::ChatSubscribe { chat: addr.clone() }).unwrap();
        let _ = tokio::time::timeout(WAIT, pushes.recv()).await;
        let deadline = Instant::now() + WAIT;
        while !w.the_chat()["attention"].is_null() {
            assert!(Instant::now() < deadline, "the alert stayed: {}", w.the_chat());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(w.the_chat()["status"], "blocked");
        c.send(ClientMsg::ChatUnsubscribe { chat: addr.clone() }).unwrap();
    });
}

#[test]
fn an_incognito_chat_lasts_while_it_is_read() {
    let w = World::new("hibernate_after = \"1s\"");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut c = w.d.client().await.unwrap();
        let _pushes = c.pushes();
        let info = match c
            .request(Request::ChatNew {
                workspace: None,
                agent: None,
                preset: None,
                title: None,
                prompt: None,
                incognito: Some(true),
                folders: Vec::new(),
                compare: Vec::new(),
                confirmed: false,
            })
            .await
            .unwrap()
        {
            Reply::Chat { info } => info,
            other => panic!("{other:?}"),
        };
        c.send(ClientMsg::ChatSubscribe { chat: info.id.clone() }).unwrap();
        c.request(Request::ChatSend { chat: info.id.clone(), text: "my secret".into() })
            .await
            .unwrap();
        let addr = format!("c:{}", info.handle);
        let deadline = Instant::now() + WAIT;
        while texts(&w.show(&addr), "assistant") != ["you said: my secret"] {
            assert!(Instant::now() < deadline, "no answer");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Its agent sleeps; the chat stays while someone reads it, and carries on, seeded.
        let deadline = Instant::now() + WAIT;
        while !w.the_chat()["status"].is_null() {
            assert!(Instant::now() < deadline, "its agent never slept");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(w.the_chat()["id"], info.id.as_str(), "it stays while it is read");
        c.request(Request::ChatSend { chat: info.id.clone(), text: "and then?".into() })
            .await
            .unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let shown = w.show(&addr);
            if texts(&shown, "assistant").get(1).is_some_and(|t| t.contains("[user] my secret")) {
                break;
            }
            assert!(Instant::now() < deadline, "not seeded from memory: {shown}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Closed: it ends (a moment later, in case the reader comes back).
        c.send(ClientMsg::ChatUnsubscribe { chat: info.id.clone() }).unwrap();
        let deadline = Instant::now() + WAIT;
        while !w.chats().is_empty() {
            assert!(Instant::now() < deadline, "it never ended");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
}

#[test]
fn a_chat_followed_by_its_handle_stops_by_its_id() {
    let w = World::new("");
    w.ok(&["chat", "new", "hello"]);
    let chat = w.the_chat();
    let (id, handle) = (chat["id"].as_str().unwrap(), chat["handle"].as_str().unwrap());
    let addr = format!("c:{handle}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut c = w.d.client().await.unwrap();
        let mut pushes = c.pushes();
        c.send(ClientMsg::ChatSubscribe { chat: addr.clone() }).unwrap();
        let _ = tokio::time::timeout(WAIT, pushes.recv()).await;
        // Followed: an answer is read as it comes, so it isn't unread.
        w.ok(&["chat", "send", "--no-wait", &addr, "seen"]);
        let deadline = Instant::now() + WAIT;
        while !texts(&w.show(&addr), "assistant").iter().any(|t| t == "you said: seen") {
            assert!(Instant::now() < deadline, "no answer");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(w.the_chat()["unread"], 0);
        // Stopped by its id: nobody follows it now, so the next answer is unread.
        c.send(ClientMsg::ChatUnsubscribe { chat: id.to_owned() }).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        w.ok(&["chat", "send", "--no-wait", &addr, "unseen"]);
        let deadline = Instant::now() + WAIT;
        while w.the_chat()["unread"] != 1 {
            assert!(Instant::now() < deadline, "still followed: {}", w.the_chat());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(c);
    });
}

#[test]
fn a_deleted_chat_leaves_no_agent_behind() {
    let w = World::new("");
    w.ok(&["chat", "new", "hello"]);
    let chat = w.the_chat();
    let addr = format!("c:{}", chat["handle"].as_str().unwrap());
    let dir = w.d.paths.data.join("chat").join(chat["id"].as_str().unwrap());
    // Deleted just after a message: whatever engine the message starts is stopped with it.
    w.ok(&["chat", "send", "--no-wait", &addr, "work 30 slowly"]);
    w.ok(&["chat", "rm", &addr]);
    w.until("no engine", || {
        w.json(&["ls", "--chats", "--json"]).as_array().is_none_or(|a| a.is_empty())
    });
    std::thread::sleep(Duration::from_millis(500));
    assert!(!dir.exists(), "its folder went: {}", dir.display());
    assert!(w.chats().is_empty());
}

#[test]
fn pinned_and_unread_address_a_chat() {
    let w = World::new("");
    w.ok(&["chat", "new", "--title", "First", "one"]);
    w.ok(&["chat", "new", "--title", "Second", "two"]);
    let addr = |title: &str| {
        let c = w.chats().into_iter().find(|c| c["title"] == title).unwrap();
        format!("c:{}", c["handle"].as_str().unwrap())
    };
    let none = w.cli(&["chat", "show", "@pinned"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no chat is pinned"), "{none:?}");
    w.ok(&["chat", "pin", &addr("Second")]);
    assert_eq!(w.show("@pinned")["chat"]["title"], "Second");
    // Two pinned: which, it asks, naming both.
    w.ok(&["chat", "pin", &addr("First")]);
    let two = w.cli(&["chat", "show", "@pinned"]);
    let err = String::from_utf8_lossy(&two.stderr);
    assert!(err.contains("matches 2 chats") && err.contains("First"), "{err}");
    let unread = w.cli(&["chat", "show", "@unread"]);
    assert!(String::from_utf8_lossy(&unread.stderr).contains("haven't read"), "{unread:?}");
}

#[test]
fn a_stopped_engines_last_words_stay_off_the_new_branch() {
    // A retry stops the engine; what it wrote to stderr is shown as it dies (200 ms on), and
    // went under the new message, the shown branch, as a red notice.
    let w = World::new("");
    w.ok(&["chat", "new", "noisy hello"]);
    let addr = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    w.ok(&["chat", "retry", &addr]);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let shown = w.json(&["chat", "show", &addr, "--format", "json"]);
    let notices: Vec<&str> = shown["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == "notice")
        .filter_map(|n| n["text"].as_str())
        .collect();
    assert!(!notices.iter().any(|t| t.contains("noisy:")), "{notices:?}");
}
