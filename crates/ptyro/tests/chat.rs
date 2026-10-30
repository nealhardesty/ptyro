//! CHAT1 end-to-end (SPEC §22, ROADMAP CHAT1): chats through the CLI against a real daemon whose
//! agents are the fakes. A chat is made and answered with no setup; its engine is hidden from
//! `ls`; it carries on after a daemon restart (natively, the same conversation); switching its
//! agent replays the conversation as a seed; titles, pin, archive, export and delete; the
//! launch line of each preset.

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
    /// `chat`: more `[chat]` lines.
    fn new(chat: &str) -> Self {
        Self::with(chat, "")
    }

    /// `extra`: more config at the end (agents).
    fn with(chat: &str, extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-chat-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n",
        );
        for a in ["claude", "codex", "gemini", "opencode"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        config.insert_str(config.find("[agents").unwrap(), &format!("{chat}\n"));
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

    fn chats(&self) -> Vec<Value> {
        self.json(&["chat", "ls", "--json"]).as_array().cloned().unwrap_or_default()
    }

    fn chat(&self, addr: &str) -> Value {
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
fn a_chat_is_answered_kept_and_carried_on_after_a_restart() {
    let mut w = World::new("");
    // No setup: the first installed agent answers, and the answer streams to stdout.
    let out = w.ok(&["chat", "hello there"]);
    assert!(out.contains("you said: hello there"), "{out}");
    let list = w.chats();
    assert_eq!(list.len(), 1);
    let c = &list[0];
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    assert_eq!((c["agent"].as_str(), c["tools"].as_str()), (Some("claude"), Some("chat")));
    assert_eq!(c["workspace"], "main");
    // The titler's answer, cleaned up.
    w.until("a title", || w.chats()[0]["title"] == "A fake title");
    // Its engine is a session, hidden from `ls` and shown by `ls --chats`.
    let ls = w.json(&["ls", "--json"]);
    assert!(ls.as_array().unwrap().is_empty(), "{ls}");
    let engines = w.json(&["ls", "--chats", "--json"]);
    let e = &engines.as_array().unwrap()[0];
    assert_eq!(e["chat"], c["id"]);
    let argv: Vec<&str> = e["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    // With the history tools (the default), our MCP server instead of `--safe-mode` (which drops
    // it), the user's customizations kept out by naming no setting sources.
    for want in [
        "--setting-sources",
        "--disable-slash-commands",
        "--strict-mcp-config",
        "--mcp-config",
        "--system-prompt",
    ] {
        assert!(argv.contains(&want), "{argv:?}");
    }
    // The MCP servers are in a file only its owner reads, never in the argv any client (or
    // `ps`) sees: a server's definition can carry its tokens.
    let cfg = argv[argv.iter().position(|a| *a == "--mcp-config").unwrap() + 1];
    assert!(cfg.ends_with("/mcp.json"), "{cfg}");
    assert!(!argv.iter().any(|a| a.contains("mcpServers") || a.contains("PTYRO_SOCKET")));
    assert!(std::fs::read_to_string(cfg).unwrap().contains("mcpServers"));
    assert!(argv.windows(2).any(|p| p == ["--tools", "WebSearch,WebFetch"]), "{argv:?}");
    let cwd = e["cwd"].as_str().unwrap().to_owned();
    assert!(cwd.ends_with(&format!("chat/{}", c["id"].as_str().unwrap())), "{cwd}");

    // A second message goes to the same engine; the record is user, answer, user, answer.
    let out = w.ok(&["chat", "send", &addr, "second one"]);
    assert!(out.contains("you said: second one"), "{out}");
    let shown = w.chat(&addr);
    assert_eq!(texts(&shown, "user"), ["hello there", "second one"]);
    assert_eq!(texts(&shown, "assistant"), ["you said: hello there", "you said: second one"]);

    // After a restart the chat is still there and carries on natively: no seed is sent.
    w.d.restart().unwrap();
    let out = w.ok(&["chat", "send", &addr, "after the restart"]);
    assert!(out.contains("you said: after the restart"), "{out}");
    assert!(!out.contains("The conversation so far"), "a native resume needs no seed: {out}");
    let shown = w.chat(&addr);
    assert_eq!(texts(&shown, "user").len(), 3);
    assert_eq!(shown["chat"]["nodes"].as_u64(), shown["nodes"].as_array().map(|a| a.len() as u64));

    // Another agent: the next message carries the conversation over as a seed.
    w.ok(&["chat", "set", &addr, "--agent", "codex"]);
    let out = w.ok(&["chat", "send", &addr, "now on codex"]);
    assert!(out.contains("The conversation so far, saved by ptyro"), "{out}");
    assert!(out.contains("[user] hello there") && out.contains("now on codex"), "{out}");
    let shown = w.chat(&addr);
    assert!(texts(&shown, "notice").iter().any(|t| t.contains("continued with codex")), "{shown}");
    let codex = w.json(&["ls", "--chats", "--json"]);
    let codex = &codex.as_array().unwrap()[0];
    assert_eq!(codex["agent"], "codex");
}

#[test]
fn a_stop_before_a_resumed_chat_answers_is_a_stop_not_a_resend() {
    let mut w = World::new("");
    w.ok(&["chat", "new", "hello"]);
    let addr = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    // After a restart the next message resumes the agent's conversation; stopped before it
    // says a word, that is the end of it (it was taken for a failed resume, and sent again).
    w.d.restart().unwrap();
    w.ok(&["chat", "send", "--no-wait", &addr, "pause 4"]);
    w.until("answering", || w.chats()[0]["status"] == "working");
    w.ok(&["chat", "stop", &addr]);
    w.until("stopped", || w.chats()[0]["status"] != "working");
    std::thread::sleep(Duration::from_secs(5));
    let shown = w.chat(&addr);
    assert_eq!(texts(&shown, "user"), ["hello", "pause 4"], "{shown}");
    assert!(!texts(&shown, "assistant").iter().any(|t| t == "paused"), "sent again: {shown}");
    assert!(!texts(&shown, "notice").iter().any(|t| t.contains("couldn't")), "{shown}");
}

#[test]
fn an_agent_that_wont_stop_is_ended_and_the_chat_goes_on() {
    let w = World::new("");
    w.ok(&["chat", "new", "--agent", "codex", "--no-wait", "work 40 slowly"]);
    let addr = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    w.until("answering", || w.chats()[0]["status"] == "working");
    // The fake doesn't hear an interrupt mid-step: ended after the grace, and said so.
    w.ok(&["chat", "stop", &addr]);
    w.until("ended", || {
        texts(&w.chat(&addr), "notice").iter().any(|t| t.contains("didn't stop when asked"))
    });
    assert_ne!(w.chats()[0]["status"], "working");
    // Not busy for good: the next message is answered.
    let out = w.ok(&["chat", "send", &addr, "and now?"]);
    assert!(out.contains("and now?"), "{out}");
}

#[test]
fn titles_pins_archive_export_and_delete() {
    let w = World::new("");
    w.ok(&["chat", "new", "--title", "Dinner ideas", "--no-wait", "what should I cook"]);
    let c = w.chats()[0].clone();
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    assert_eq!(c["title"], "Dinner ideas");
    // By title, by handle, by @last.
    w.until("the answer", || texts(&w.chat("dinner"), "assistant").len() == 1);
    // A person's title is never replaced by an automatic one.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(w.chats()[0]["title"], "Dinner ideas");
    w.ok(&["chat", "retitle", "@last", "Cooking"]);
    assert_eq!(w.chats()[0]["title"], "Cooking");
    w.ok(&["chat", "pin", &addr]);
    assert_eq!(w.chats()[0]["pinned"], true);
    let md = w.ok(&["chat", "export", &addr]);
    assert!(md.starts_with("# Cooking") && md.contains("**You:** what should I cook"), "{md}");
    w.ok(&["chat", "archive", &addr]);
    assert!(w.chats().is_empty());
    let archived = w.json(&["chat", "ls", "--archived", "--json"]);
    assert_eq!(archived.as_array().unwrap().len(), 1);
    w.ok(&["chat", "rm", &addr]);
    let archived = w.json(&["chat", "ls", "--archived", "--json"]);
    assert!(archived.as_array().unwrap().is_empty());
    let out = w.cli(&["chat", "show", &addr]);
    assert!(!out.status.success());
}

#[test]
fn ask_answers_once_and_keeps_nothing() {
    let w = World::new("");
    let out = w.ok(&["chat", "ask", "what", "is", "2+2"]);
    assert_eq!(out.trim(), "you said: what is 2+2");
    assert!(w.chats().is_empty(), "ask keeps nothing");
    let out = w.ok(&["chat", "ask", "--keep", "keep", "this"]);
    assert!(out.contains("you said: keep this"));
    assert_eq!(w.chats().len(), 1);
}

#[test]
fn incognito_by_default_still_keeps_a_chat_asked_to() {
    let w = World::new("incognito = true");
    w.ok(&["chat", "new", "--no-wait", "forget me"]);
    w.ok(&["chat", "new", "--keep", "--no-wait", "keep me"]);
    w.ok(&["chat", "ask", "--keep", "and keep this"]);
    let mut kept: Vec<(bool, String)> = w
        .chats()
        .iter()
        .map(|c| (c["incognito"] == true, c["title"].as_str().unwrap_or_default().to_owned()))
        .collect();
    kept.sort();
    assert_eq!(kept.iter().filter(|(i, _)| *i).count(), 1, "only the first: {kept:?}");
    assert_eq!(kept.iter().filter(|(i, _)| !*i).count(), 2, "--keep wins: {kept:?}");
}

#[test]
fn a_permission_card_is_answered_and_chats_can_be_switched_off() {
    let w = World::new("");
    w.ok(&["chat", "new", "--no-wait", "ask rm -rf /tmp/nothing"]);
    let addr = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    w.until("a permission request", || {
        w.chats()[0]["attention"]["text"].as_str().is_some_and(|t| t.contains("wants permission"))
    });
    // The inbox says what happened, never what it asked: a chat's words stay in chat.db.
    let inbox = w.json(&["inbox", "--json"]);
    let bodies: Vec<&str> =
        inbox.as_array().unwrap().iter().filter_map(|e| e["body"].as_str()).collect();
    assert_eq!(bodies, ["the chat asks for permission"], "{inbox}");
    w.ok(&["chat", "answer", &addr, "deny"]);
    w.until("the answer", || texts(&w.chat(&addr), "assistant").iter().any(|t| t == "answer: no"));
    assert!(w.chats()[0]["attention"].is_null());
    // An incognito chat's card is no inbox entry at all.
    w.ok(&["chat", "new", "--incognito", "--no-wait", "ask rm -rf /tmp/secretplan"]);
    w.until("its permission request", || {
        w.chats().iter().any(|c| {
            c["incognito"] == true
                && c["attention"]["text"].as_str().is_some_and(|t| t.contains("wants permission"))
        })
    });
    assert_eq!(w.json(&["inbox", "--json"]).as_array().unwrap().len(), 1, "no new entry");
    let data = w.d.home.path().join("data");
    for f in [data.join("ptyro.db"), data.join("ptyro.db-wal")] {
        let bytes = String::from_utf8_lossy(&std::fs::read(&f).unwrap_or_default()).into_owned();
        assert!(!bytes.contains("rm -rf"), "{} holds a chat's words", f.display());
    }
}

#[test]
fn chats_switched_off_say_how_to_switch_them_on() {
    let w = World::new("enabled = false");
    let out = w.cli(&["chat", "ls"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("chat.enabled true"));
}

#[test]
fn one_answer_at_a_time_and_an_agent_that_wont_start_says_so() {
    let w = World::new("");
    w.ok(&["chat", "new", "--no-wait", "work 2 slowly"]);
    let addr = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    w.until("it is answering", || w.chats()[0]["status"] == "working");
    // A message mid-answer is refused (it would interleave with the answer), not lost.
    let out = w.cli(&["chat", "send", &addr, "and another"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("still answering"));
    w.until("the answer", || texts(&w.chat(&addr), "assistant").len() == 1);
    w.until("idle again", || w.chats()[0]["status"] == "idle");
    // Then it goes, and `send` prints this message's answer, not the one before.
    let out = w.ok(&["chat", "send", &addr, "next"]);
    assert_eq!(out.trim(), "you said: next");
}

#[test]
fn an_agent_that_stops_before_answering_is_said_and_ask_keeps_nothing() {
    let w = World::with(
        "",
        "[agents.broken]\ncommand = [\"/bin/sh\", \"-c\", \"exit 3\"]\nprotocol = \"acp\"\n",
    );
    let out = w.cli(&["chat", "ask", "--agent", "broken", "hello?"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("stopped before it answered"), "{out:?}");
    assert!(w.chats().is_empty(), "ask keeps nothing, even when it fails");
}
