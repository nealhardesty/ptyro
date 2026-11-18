//! M10 end-to-end tests: the structured machinery chats run on (SPEC §22.2). A session of your
//! own is no longer started structured (removed 2026-11-17), so it is tested through chats:
//! every protocol's turn, permission request and answer (Claude stream-json, Codex app-server,
//! an ACP agent of the user's, OpenCode's server), and a chat's answer cut off by a live upgrade. A session started
//! structured before the removal resumes in its agent's own terminal UI. Every agent is the fake.

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m10-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[attention]\nmin_work = \"1s\"\n[chat]\ntitle = \"off\"\n[worktrees]\nfetch = false\n",
        );
        for a in ["claude", "codex", "opencode"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        // An agent of the user's that speaks ACP.
        config.push_str(&format!(
            "[agents.acp]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"acp\", \"--acp\"]\nprotocol = \"acp\"\n"
        ));
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("ptyro {args:?}: {e}\n{out}"))
    }

    fn info(&self, s: &str) -> Value {
        self.json(&["status", s, "--json"])["session"].clone()
    }

    /// A chat, as `chat ls` has it.
    fn chat_info(&self, id: &str) -> Value {
        let all = self.json(&["chat", "ls", "--json"]);
        all.as_array().unwrap().iter().find(|c| c["id"] == id).cloned().unwrap_or_default()
    }

    /// What the chat's agent said, oldest first.
    fn answers(&self, addr: &str) -> Vec<String> {
        let shown = self.json(&["chat", "show", addr, "--format", "json"]);
        shown["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["t"] == "assistant")
            .map(|n| n["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// The chat's engine: the hidden structured session it runs on.
    fn engine(&self, id: &str) -> Value {
        let all = self.json(&["ls", "--chats", "--json"]);
        all.as_array().unwrap().iter().find(|s| s["chat"] == id).cloned().unwrap_or_default()
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A new chat with `agent`, answered: its id and address.
fn new_chat(w: &World, agent: &str, first: &str) -> (String, String) {
    let out = w.json(&["chat", "new", "--agent", agent, "--timeout", "60", "--json", first]);
    let id = out["chat"]["id"].as_str().unwrap().to_owned();
    (id, format!("c:{}", out["chat"]["handle"].as_str().unwrap()))
}

/// Every protocol: a turn, a permission request answered (allow, then deny), the engine driven
/// over the agent's protocol with its conversation id.
#[test]
fn every_protocol_answers_a_chat_and_its_permission_requests() {
    let w = World::new();
    for (agent, protocol) in [
        ("claude", "stream-json"),
        ("codex", "app-server"),
        ("acp", "acp"),
        ("opencode", "opencode"),
    ] {
        let (id, addr) = new_chat(&w, agent, "work 0.2 first");
        assert!(w.answers(&addr).iter().any(|a| a == "worked: first"), "{agent}");
        let e = w.engine(&id);
        assert_eq!(e["structured"], protocol, "{agent}: {e:#}");
        assert!(e["conversation"].is_string(), "{agent}: {e:#}");
        for (cmd, decision, word) in [("ask rm one", "allow", "yes"), ("ask rm two", "deny", "no")]
        {
            w.ok(&["chat", "send", &addr, "--no-wait", cmd]);
            w.until(&format!("{agent} asks"), || {
                w.chat_info(&id)["attention"]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("wants permission"))
            });
            w.ok(&["chat", "answer", &addr, decision]);
            w.until(&format!("{agent} answered {word}"), || {
                w.answers(&addr).iter().any(|a| *a == format!("answer: {word}"))
            });
            w.until(&format!("{agent} done"), || w.chat_info(&id)["status"] != "working");
        }
    }
}

/// A live upgrade in the middle of a chat's answer: the engine crosses the exec with its
/// driver (the structured image), then is stopped (the new image has no follower for it, §22.4);
/// the cut-off answer says so, and the next message carries on in the same conversation.
#[test]
fn a_chats_answer_cut_off_by_a_live_upgrade_says_so_and_the_chat_goes_on() {
    let w = World::new();
    let (id, addr) = new_chat(&w, "codex", "work 0.1 before");
    let before = w.engine(&id);
    assert_eq!(before["structured"], "app-server");
    w.ok(&["chat", "send", &addr, "--no-wait", "work 2 across"]);
    w.until("working", || w.engine(&id)["status"]["status"] == "working");
    let up = w.json(&["daemon", "upgrade", "--json"]);
    assert_eq!(up["rolled_back"], false, "{up}");
    w.until("the cut-off answer is closed", || {
        let shown = w.json(&["chat", "show", &addr, "--format", "json"]);
        shown["nodes"].as_array().unwrap().iter().any(|n| {
            n["t"] == "notice"
                && n["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("ptyro stopped before this was answered")
        })
    });
    w.until("idle", || w.chat_info(&id)["status"] != "working");
    let out = w.ok(&["chat", "send", &addr, "after the upgrade"]);
    assert!(out.contains("you said: after the upgrade"), "{out}");
    assert!(!out.contains("The conversation so far"), "a native resume needs no seed: {out}");
    assert_eq!(w.engine(&id)["conversation"], before["conversation"]);
}

/// A session someone started structured before that was removed (2026-11-17) and left dormant:
/// it resumes in its agent's own terminal UI, in the same conversation.
#[test]
fn a_dormant_structured_session_from_before_resumes_in_its_own_ui() {
    let mut w = World::new();
    w.ok(&["new", "-d", "-n", "h1", "--agent", "claude"]);
    w.ok(&["send", "h1", "work 0.1 remember-me", "--wait-ready", "--wait", "--timeout", "60s"]);
    let conv = w.info("h1")["conversation"].as_str().unwrap().to_owned();
    w.ok(&["hibernate", "h1"]);
    w.until("dormant", || w.info("h1")["state"]["t"] == "dormant");
    w.d.stop();
    // As an older ptyro left it: launched over the agent's protocol.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let store = ptyro_store::Store::open(&w.d.paths.data.join("ptyro.db")).unwrap();
        let mut r =
            store.dormant().await.unwrap().into_iter().find(|r| r.name == "h1").expect("h1");
        r.launch.get_or_insert_with(|| serde_json::json!({}))["structured"] = "stream-json".into();
        store.save_session(r);
        store.flush().await.unwrap();
    });
    w.d.restart().unwrap();
    w.ok(&["resume", "h1"]);
    let deadline = Instant::now() + WAIT;
    let screen = loop {
        let t = w.ok(&["read", "h1"]);
        if t.contains("resumed=true") {
            break t;
        }
        assert!(Instant::now() < deadline, "never resumed in its own UI:\n{t}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(screen.contains(&format!("session={conv}")), "{screen}");
    let i = w.info("h1");
    assert!(i["structured"].is_null(), "{i:#}");
    assert_eq!(i["conversation"], conv.as_str());
}
