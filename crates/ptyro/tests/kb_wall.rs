//! The wall between profiles (SPEC §25.9), asked from inside an agent's session by every way
//! in, over a real daemon with the fake agents. What a session may see is decided by where its
//! process runs, never by what it says of itself or by which request it sends:
//!
//! - a process in a session that calls itself another machine's link is still that session;
//! - a chat is read, searched and listed behind the same wall as the knowledge base, by the
//!   tools (`recall_read` of a chat's address) and by the commands;
//! - `KbStatus` names nothing another profile's sources were refused for;
//! - a note an agent writes has no secret in its file's name, its address or the tool's answer.

use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::kb::ChunkKind;
use ptyro_core::kb::RootSpec;
use ptyro_protocol::{Reply, Request};

mod kbkit;
use kbkit::*;

/// Two profiles, each with its fake agents; `work` has a secret.
fn profiles() -> String {
    format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\n[profiles.work.secrets]\nDB_PASS = 'hunter2-value'\n[profiles.home]\nclaude = \"~/.claude-home\"\n",
        agents("")
    )
}

impl World {
    /// A request sent from inside the session `name`, by a client that calls itself `kind`:
    /// `ok: <reply as JSON>` or `error: <message>`.
    async fn asks(&self, c: &Client, name: &str, kind: &str, req: &Request) -> String {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = self.home().join(format!("as-{n}.txt"));
        let json = serde_json::to_string(req).unwrap();
        self.type_line(c, name, &format!("as {kind} {} {json}", file.display())).await;
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(said) = std::fs::read_to_string(&file) {
                return said;
            }
            assert!(Instant::now() < deadline, "{name}: {json} never answered");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

fn search(query: &str) -> Request {
    Request::KbSearch {
        query: query.into(),
        project: None,
        kinds: Vec::new(),
        since_ms: None,
        before_ms: None,
        limit: None,
        here: None,
        machines: false,
        machine: None,
        profile: None,
    }
}

fn note(text: &str) -> Request {
    Request::KbNote {
        text: text.into(),
        title: None,
        project: None,
        profile: None,
        tags: Vec::new(),
        replaces: None,
        addr: None,
    }
}

/// A3, A4: `kind` in a client's hello is the client's own word. A process in an agent's
/// session that says it is a machine's link is answered as that session: behind the wall,
/// writing as the agent, refused what a person decides, and told nothing of other profiles'
/// refusals.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_calls_itself_a_machine_is_still_behind_the_wall() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    let b = w.agent_in(&c, "hb", "claude", Some("home")).await;
    let home = format!("session://claude/{}", b.conversation.unwrap());
    w.turn(&c, "hb", "edit other.txt zqhomeword").await;
    w.kept(&home, "the home turn", |k| k.has(ChunkKind::User, "edit other.txt zqhomeword")).await;
    // A source of the home profile that was refused: the status names it, to you.
    let dir = w.d.home.path().join("data/kb/notes/home/_global");
    std::fs::create_dir_all(&dir).unwrap();
    let keys: String = (0..30).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect();
    std::fs::write(dir.join("zqkeys.md"), keys).unwrap();
    w.skipped("note://home/_global/zqkeys", "credentials").await;

    w.agent_in(&c, "wa", "claude", Some("work")).await;
    for kind in ["cli", "machine"] {
        // Search and read: nothing of the other profile.
        let said = w.asks(&c, "wa", kind, &search("zqhomeword")).await;
        assert!(said.starts_with("ok: ") && said.contains("\"hits\":[]"), "{kind}: {said}");
        assert!(!said.contains("other.txt"), "{kind}: {said}");
        // The status: no other profile's refusals, and what was offered to others is not said.
        let said = w.asks(&c, "wa", kind, &Request::KbStatus).await;
        assert!(said.starts_with("ok: ") && !said.contains("zqkeys"), "{kind}: {said}");
        // What is read and dropped is a person's to say.
        let spec = RootSpec { path: w.home().display().to_string(), ..RootSpec::default() };
        let said = w.asks(&c, "wa", kind, &Request::KbIngest { spec, machine: None }).await;
        assert!(said.starts_with("error: an agent's session doesn't add"), "{kind}: {said}");
        // A note is the agent's, under its profile, redacted.
        let text = format!("zq{kind}note pw hunter2-value");
        let said = w.asks(&c, "wa", kind, &note(&text)).await;
        assert!(said.starts_with("ok: ") && !said.contains("hunter2-value"), "{kind}: {said}");
        let file =
            w.d.home
                .path()
                .join(format!("data/kb/notes/work/_global/zq{kind}note-pw-secret-db-pass.md"));
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{kind}: {e}: {said}"));
        assert!(text.starts_with("---\nby: claude"), "{kind}: {text}");
        assert!(text.contains("note pw <secret:DB_PASS>"), "{kind}: {text}");
    }
    assert!(!w.d.home.path().join("data/kb/notes/_all").exists(), "no note as `you`");
    // You still see it all.
    let found = w.json(&["recall", "--json", "zqhomeword"]);
    assert!(!found.as_array().unwrap().is_empty(), "{found}");
}

/// A2, C1: a secret in a note's first line is in no file name, no address, no heading and
/// not in what the tool answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_notes_title_is_redacted_before_it_names_the_file() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    w.agent_in(&c, "wa", "claude", Some("work")).await;
    let text = "hunter2-value opens staging\n\nzqstaging: rotate it monthly.";
    let said = w.mcp(&c, "wa", "remember", serde_json::json!({ "text": text })).await;
    assert!(said.starts_with("noted as k:"), "{said}");
    assert!(!said.contains("hunter2"), "{said}");
    let dir = w.d.home.path().join("data/kb/notes/work/_global");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["secret-db-pass-opens-staging.md"]);
    let found = w.json(&["recall", "--json", "zqstaging"]);
    assert_eq!(found[0]["uri"], "note://work/_global/secret-db-pass-opens-staging");
    assert!(!found.to_string().contains("hunter2"), "{found}");
    assert!(!w.bytes_hold("hunter2-value"), "the secret is in kb.db");
}

/// A1: a chat is the user's and the chat agent's; an agent's session in a terminal reads,
/// searches and lists chats behind the same wall as the knowledge base, by its tools and by
/// the commands.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_agent_reads_chats_only_within_its_wall() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    // A chat of the default profile.
    let chat = w.cli(&["chat", "new", "zqchatword is the plan for the weekend"]);
    assert!(chat.status.success(), "{}", String::from_utf8_lossy(&chat.stderr));
    let all = Request::ChatList {
        workspace: None,
        all: true,
        pinned: false,
        archived: false,
        limit: None,
    };
    let Reply::Chats { chats, .. } = c.request(all.clone()).await.unwrap() else { panic!() };
    let addr = chats[0].addr();
    let read = Request::ChatRead { chat: addr.clone(), from: None, limit: None };
    let find = Request::ChatSearch {
        query: "zqchatword".into(),
        chat: None,
        workspace: None,
        agent: None,
        pinned: false,
        archived: true,
        limit: None,
        machines: false,
        profile: None,
    };
    let export = Request::ChatExport { chat: addr.clone(), format: String::new() };

    // A session under another profile: the chat is not there, whichever way it asks.
    w.agent_in(&c, "wa", "claude", Some("work")).await;
    let said = w.mcp(&c, "wa", "recall_read", serde_json::json!({ "ref": addr })).await;
    assert!(said.starts_with("error: ") && !said.contains("zqchatword"), "{said}");
    for tool in ["chat_read", "chat_search", "chat_list"] {
        let args = serde_json::json!({ "chat": addr, "query": "zqchatword" });
        let said = w.mcp(&c, "wa", tool, args).await;
        assert!(!said.contains("zqchatword") && !said.contains("weekend"), "{tool}: {said}");
    }
    for kind in ["cli", "machine"] {
        for req in [&read, &export] {
            let said = w.asks(&c, "wa", kind, req).await;
            assert!(said.starts_with("error: no chat"), "{kind} {req:?}: {said}");
        }
        for req in [&find, &all] {
            let said = w.asks(&c, "wa", kind, req).await;
            assert!(said.starts_with("ok: ") && !said.contains("zqchatword"), "{kind}: {said}");
            assert!(!said.contains("weekend") && !said.contains(&chats[0].id), "{kind}: {said}");
        }
    }
    w.turn(&c, "wa", &format!("sh {BIN} chat show {addr} 2>&1 | grep -c zqchatword")).await;
    let work = format!("session://claude/{}", w.info(&c, "wa").await.conversation.unwrap());
    w.kept(&work, "the command", |k| k.has(ChunkKind::Assistant, "ran: 0")).await;

    // A session under the chat's own profile reads it.
    w.agent_in(&c, "da", "claude", None).await;
    let said = w.mcp(&c, "da", "recall_read", serde_json::json!({ "ref": addr })).await;
    assert!(said.contains("zqchatword is the plan"), "{said}");
    let said = w.asks(&c, "da", "cli", &read).await;
    assert!(said.starts_with("ok: ") && said.contains("zqchatword"), "{said}");
    // And you read it as before.
    assert!(w.out(&["chat", "show", &addr]).contains("zqchatword"));
}
