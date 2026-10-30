//! CHAT2 and CHAT3 end-to-end (SPEC §22.7–§22.10): branches, forks, side chats, incognito,
//! search, the history scope, `chat find` and the history tools, through the CLI against a real
//! daemon whose agents are the fakes (which echo what they are given, so a seed shows).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Output, Stdio};
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
    fn new(chat: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-chat2-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = format!(
            "[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo Titled\"]\n{chat}\n[profiles.work]\nlabel = \"work\"\n"
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

    fn addr_of(&self, title_part: &str) -> String {
        let c = self
            .chats()
            .into_iter()
            .find(|c| c["title"].as_str().is_some_and(|t| t.contains(title_part)))
            .unwrap_or_else(|| panic!("no chat titled {title_part:?}: {:?}", self.chats()));
        format!("c:{}", c["handle"].as_str().unwrap())
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

    fn idle(&self, addr: &str) {
        self.until("idle", || {
            let c = self.show(addr);
            c["chat"]["status"].is_null() || c["chat"]["status"] == "idle"
        });
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

fn ids(chat: &Value, kind: &str) -> Vec<String> {
    chat["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == kind)
        .map(|n| n["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn retry_edit_branches_and_forks() {
    let w = World::new("");
    w.ok(&["chat", "new", "--title", "Plans", "hello"]);
    let addr = w.addr_of("Plans");
    w.idle(&addr);
    let first = w.show(&addr);
    let first_answer = ids(&first, "assistant")[0].clone();
    let hello = ids(&first, "user")[0].clone();

    // Retry: a second answer beside the first; the first stays, as a branch.
    let out = w.ok(&["chat", "retry", &addr]);
    assert!(out.contains("you said: hello"), "{out}");
    let shown = w.show(&addr);
    let second_answer = ids(&shown, "assistant")[0].clone();
    assert_ne!(first_answer, second_answer);
    let sib = &shown["siblings"].as_array().unwrap_or_else(|| panic!("{shown}"))[0];
    assert_eq!((sib["at"].as_u64(), sib["of"].as_u64()), (Some(2), Some(2)), "{shown}");
    let text = w.ok(&["chat", "show", &addr, "--branches"]);
    assert!(text.contains("‹ 2/2 ›"), "{text}");

    // Flip back to the first answer.
    w.ok(&["chat", "branch", &addr, "--to", &first_answer]);
    assert_eq!(ids(&w.show(&addr), "assistant"), std::slice::from_ref(&first_answer));

    // Edit the message: a sibling of it, answered afresh; both stay.
    let out = w.ok(&["chat", "edit", &addr, &hello[hello.len() - 8..], "bonjour"]);
    assert!(out.contains("you said: bonjour"), "{out}");
    let shown = w.show(&addr);
    assert_eq!(texts(&shown, "user"), ["bonjour"]);
    assert_eq!(
        w.json(&["chat", "show", &addr, "--all", "--format", "json"])["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["t"] == "user")
            .count(),
        2
    );
    w.idle(&addr);

    // A fork on the same agent from the end: Claude's native fork (no seed is sent).
    let out = w.ok(&["chat", "fork", &addr, "and then?"]);
    assert!(out.contains("you said: and then?"), "{out}");
    assert!(!out.contains("The conversation so far"), "native: no seed: {out}");
    let fork = w.addr_of("fork of Plans");
    let f = w.show(&fork);
    assert_eq!(texts(&f, "user"), ["bonjour", "and then?"]);
    assert!(f["chat"]["forked"].as_bool().unwrap());
    // The original is as it was.
    assert_eq!(texts(&w.show(&addr), "user"), ["bonjour"]);

    // A fork on another agent, from a message in the middle: seeded with the branch to there.
    let out = w.ok(&[
        "chat",
        "fork",
        &fork,
        "--agent",
        "codex",
        "--at",
        &ids(&f, "assistant")[0],
        "on codex",
    ]);
    assert!(out.contains("The conversation so far") && out.contains("[user] bonjour"), "{out}");
    assert!(!out.contains("and then?"), "only the branch up to the message: {out}");

    // A side chat: asks about the chat, answers, and goes; the chat is unchanged.
    let before = w.chats().len();
    let out = w.ok(&["chat", "fork", &addr, "--side", "what was that about?"]);
    assert!(out.contains("what was that about?"), "{out}");
    w.until("the side chat gone", || w.chats().len() == before);
    assert_eq!(texts(&w.show(&addr), "user"), ["bonjour"]);

    // Kept, it stays as a fork of its own, named as one.
    w.ok(&["chat", "fork", &addr, "--side", "--keep", "and this?"]);
    let forks = w.chats();
    let forks: Vec<_> = forks.iter().filter(|c| c["title"] == "fork of Plans").collect();
    assert_eq!(forks.len(), 2, "the earlier fork and the kept side chat: {forks:?}");
    assert!(forks.iter().all(|c| c["side_of"].is_null()));
    assert_eq!(w.chats().len(), before + 1);
}

#[test]
fn a_side_chat_nobody_follows_goes_and_goes_with_its_chat() {
    let w = World::new("hibernate_after = \"2s\"");
    w.ok(&["chat", "new", "hello"]);
    // (By its handle: its title is written behind the answer.)
    let parent = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    let side_of = |out: &Output| {
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        err.split_whitespace().next().unwrap_or_default().to_owned()
    };
    let gone = |w: &World, addr: &str| !w.cli(&["chat", "show", addr]).status.success();
    // Left by a client that couldn't delete it (here: never opened): it goes after a while.
    // (Made well after the daemon started, so not by the look it takes at start.)
    std::thread::sleep(Duration::from_secs(3));
    let s1 = side_of(&w.cli(&["chat", "fork", &parent, "--side"]));
    assert!(s1.starts_with("c:") && !gone(&w, &s1), "{s1}");
    w.until("the side chat nobody follows is gone", || gone(&w, &s1));
    assert!(!gone(&w, &parent), "its chat stays");
    // Deleting its chat takes it along at once (here nothing else would, for a minute).
    let w = World::new("hibernate_after = \"60s\"");
    w.ok(&["chat", "new", "hello"]);
    // (By its handle: its title is written behind the answer.)
    let parent = format!("c:{}", w.chats()[0]["handle"].as_str().unwrap());
    let s2 = side_of(&w.cli(&["chat", "fork", &parent, "--side"]));
    assert!(!gone(&w, &s2));
    w.ok(&["chat", "rm", &parent]);
    assert!(gone(&w, &s2), "it went with its chat");
}

#[test]
fn incognito_keeps_nothing() {
    let w = World::new("hibernate_after = \"4s\"");
    let out = w.ok(&["chat", "new", "--incognito", "my secret plan"]);
    assert!(out.contains("you said: my secret plan"), "{out}");
    let c = w.chats().into_iter().find(|c| c["incognito"] == true).expect("listed while it lasts");
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    assert_eq!(texts(&w.show(&addr), "assistant"), ["you said: my secret plan"]);
    // A retry replaces the answer (no branches to keep); another agent carries it on, seeded.
    w.ok(&["chat", "retry", &addr]);
    let shown = w.show(&addr);
    assert_eq!(texts(&shown, "user"), ["my secret plan"]);
    assert_eq!(texts(&shown, "assistant"), ["you said: my secret plan"]);
    w.ok(&["chat", "set", &addr, "--agent", "codex"]);
    let out = w.ok(&["chat", "send", &addr, "and on codex?"]);
    assert!(out.contains("[user] my secret plan"), "seeded from memory: {out}");
    // Never searchable, never written.
    assert!(w.json(&["chat", "search", "secret", "--json"]).as_array().unwrap().is_empty());
    let db = w.d.home.path().join("data/chat.db");
    let bytes = std::fs::read(&db).unwrap_or_default();
    assert!(!String::from_utf8_lossy(&bytes).contains("secret plan"), "nothing in chat.db");
    // Nor in ptyro.db (an engine is no session row) or a recording (it is never recorded).
    let data = w.d.home.path().join("data");
    let mut files = vec![data.join("ptyro.db"), data.join("ptyro.db-wal")];
    for d in std::fs::read_dir(data.join("sessions")).into_iter().flatten().flatten() {
        files.extend(std::fs::read_dir(d.path()).into_iter().flatten().flatten().map(|f| f.path()));
    }
    for f in files {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&bytes).contains("secret plan"), "in {}", f.display());
    }
    // It ends with its agent.
    w.until("gone with its agent", || w.chats().iter().all(|c| c["incognito"] != true));
    // `ask` is incognito: nothing left, nothing found.
    w.ok(&["chat", "ask", "another secret"]);
    assert!(w.json(&["chat", "search", "another", "--json"]).as_array().unwrap().is_empty());
}

#[test]
fn search_scopes_find_and_the_history_tools() {
    let w = World::new("");
    w.ok(&[
        "chat",
        "new",
        "--title",
        "Retry design",
        "the retry policy should back off with jitter",
    ]);
    w.ok(&["chat", "new", "--agent", "codex", "--title", "Cache", "caching for the redis layer"]);
    w.ok(&[
        "chat",
        "new",
        "--agent",
        "claude@work",
        "--title",
        "Work notes",
        "quarterly jitter budget at work",
    ]);
    let retry = w.addr_of("Retry design");

    // Words, stems, phrases, prefixes, filters; snippets marked; best first.
    let hits = w.json(&["chat", "search", "jitter", "--json"]);
    let hits = hits.as_array().unwrap();
    assert!(hits.len() >= 2, "{hits:?}");
    assert!(hits[0]["snippet"].as_str().unwrap().contains('\u{2}'));
    let only = w.json(&["chat", "search", "\"back off\"", "--json"]);
    assert!(
        only.as_array().unwrap().iter().all(|h| h["chat"]["title"] == "Retry design"),
        "{only}"
    );
    let codex = w.json(&["chat", "search", "agent:codex", "redi*", "--json"]);
    assert!(
        codex.as_array().unwrap().iter().all(|h| h["chat"]["agent"] == "codex")
            && !codex.as_array().unwrap().is_empty()
    );
    let not = w.json(&["chat", "search", "jitter", "-budget", "--json"]);
    assert!(not.as_array().unwrap().iter().all(|h| h["chat"]["title"] != "Work notes"), "{not}");
    let titles = w.json(&["chat", "search", "design", "--json"]);
    assert!(titles.as_array().unwrap().iter().any(|h| h["kind"] == "title"));
    let text = w.ok(&["chat", "search", "jitter"]);
    assert!(text.contains(&format!("{retry}/")) && text.contains("*jitter*"), "{text}");
    let short = w.cli(&["chat", "search", "ji*"]);
    assert!(String::from_utf8_lossy(&short.stderr).contains("3 letters"));

    // From inside a chat's agent (the fake runs `sh` as a tool): its profile's chats only, and
    // it may not change anything.
    let q = format!("sh {BIN} chat search jitter --json");
    let out = w.ok(&["chat", "send", &retry, &q]);
    assert!(out.contains("Retry design") || out.contains("ran:"), "{out}");
    let shown = w.show(&retry);
    let tool: String = shown["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == "tool")
        .map(|n| n["output"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(!tool.contains("Work notes"), "a default-profile chat never sees a work one: {tool}");
    assert!(!tool.contains("Retry design\""), "nor itself");
    let rm = format!("sh {BIN} chat rm {retry}");
    w.idle(&retry);
    w.ok(&["chat", "send", &retry, &rm]);
    let shown = w.show(&retry);
    let tool: String = shown["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == "tool")
        .map(|n| n["output"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(tool.contains("may search and read chats, not change them"), "{tool}");
    // Nor read one outside its scope, or itself, by name.
    let work = w.addr_of("Work notes");
    w.idle(&retry);
    w.ok(&["chat", "send", &retry, &format!("sh {BIN} chat show {work}")]);
    w.idle(&retry);
    w.ok(&["chat", "send", &retry, &format!("sh {BIN} chat show {retry}")]);
    let shown = w.show(&retry);
    let tool: Vec<String> = shown["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["t"] == "tool")
        .map(|n| n["output"].as_str().unwrap_or("").to_owned())
        .collect();
    let last2 = &tool[tool.len() - 2..];
    assert!(last2.iter().all(|t| t.contains("no chat")), "{last2:?}");
    assert!(!last2.iter().any(|t| t.contains("quarterly")), "{last2:?}");

    // `chat find`: an incognito chat asked the question; nothing of it stays.
    let before = w.chats().len();
    let out = w.ok(&["chat", "find", "what", "did", "we", "decide?"]);
    assert!(out.contains("you said: what did we decide?"), "{out}");
    assert_eq!(w.chats().len(), before);

    // The history tools themselves (`ptyro mcp --chat`), over JSON-RPC.
    let mut mcp =
        w.d.cmd()
            .args(["mcp", "--chat"])
            .env("PTYRO_SESSION_ID", "01testsessionid000000000000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let mut out = BufReader::new(mcp.stdout.take().unwrap());
    let mut ask = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
    );
    assert!(init["result"]["instructions"].as_str().unwrap().contains("chat_search"));
    let list = ask(serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["chat_search", "chat_read", "chat_list"]);
    let found = ask(
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "chat_search", "arguments": { "query": "redis" } } }),
    );
    let hits = &found["result"]["structuredContent"]["hits"];
    let cite = hits[0]["cite"].as_str().unwrap().to_owned();
    assert!(cite.starts_with("c:") && cite.contains('/'), "{found}");
    assert!(hits[0]["snippet"].as_str().unwrap().contains("**redis**"));
    let read = ask(
        serde_json::json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "chat_read", "arguments": { "chat": cite } } }),
    );
    assert!(
        read["result"]["structuredContent"]["messages"].to_string().contains("redis"),
        "{read}"
    );
    drop(stdin);
    let _ = mcp.wait();
}
