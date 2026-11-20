//! KB5 end to end (SPEC §25.9): what an agent is offered at a prompt, under a real daemon
//! with fake agents that run the prompt's hook as Claude Code and Codex do and keep its
//! answer where they keep it.
//!
//! - Off unless switched on: with `kb.inject` as it comes, a prompt's hook answers nothing.
//! - On: a prompt that says again what a kept session said is given pointers, in the fence,
//!   within the cap, never the text, and the hook answers within its time; `recall_read`
//!   opens what was pointed at.
//! - Never twice in a session, never its own session, never over the wall between
//!   profiles, never a private session, never for a prompt that shares too little (the
//!   floor) or is too short.
//! - Never out of sight: `ptyro kb` says what was offered and to which session; a pointer
//!   to what was forgotten since is gone from there too.
//! - What was offered is not kept again as something the user or the agent said.
//! - `[agents.X] kb = false` and `kb.tools = false` offer nothing.
#![cfg(unix)]

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::kb::ChunkKind;
use ptyro_core::kb::offer::{FENCE, MAX, MAX_BYTES};
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request};

mod kbkit;
use kbkit::*;

/// The file the fake agents write a line to for every prompt: what its hook gave them.
const ASKED: &str = "asked.jsonl";

fn config(more: &str) -> String {
    format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\ncodex = \"~/.codex-work\"\n[profiles.home]\nclaude = \"~/.claude-home\"\n{more}",
        agents(&format!(", PTYRO_FAKE_CONTEXT = \"{ASKED}\""))
    )
}

/// What a prompt's hook gave the agent, and how long it took (ms).
struct Given {
    context: Option<String>,
    ms: u64,
}

impl Given {
    /// The pointers' lines.
    fn pointers(&self) -> Vec<&str> {
        self.context
            .as_deref()
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("- "))
            .collect()
    }
}

/// Give the session's agent `prompt`, wait for its turn, and say what the prompt's hook
/// gave it.
async fn ask(w: &World, c: &Client, name: &str, prompt: &str) -> Given {
    let file = w.home().join(ASKED);
    let lines = |f: &std::path::Path| -> Vec<serde_json::Value> {
        std::fs::read_to_string(f)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let before = lines(&file).len();
    w.turn(c, name, prompt).await;
    let all = lines(&file);
    assert_eq!(all.len(), before + 1, "{name}: one prompt, one hook");
    let last = &all[before];
    assert_eq!(last["prompt"], prompt);
    Given { context: last["context"].as_str().map(str::to_owned), ms: last["ms"].as_u64().unwrap() }
}

/// Wait until the daemon runs with `kb.inject` as `on`.
async fn inject(w: &World, on: bool) {
    let o = w.cli(&["config", "set", "kb.inject", if on { "true" } else { "false" }]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    w.status(&format!("kb.inject = {on}"), |s| s["inject"] == on).await;
}

const SAID: &str = "the zqupload queue retries with exponential zqbackoff and jitter";
const AGAIN: &str = "how did the zqupload queue do its retries and zqbackoff before";

#[tokio::test(flavor = "multi_thread")]
async fn off_as_it_comes_and_on_pointers_within_the_cap_behind_every_gate() {
    let w = World::new(&config(""));
    let c = w.d.client().await.unwrap();
    // A work session says something worth finding again.
    let a = w.agent_in(&c, "wa", "claude", Some("work")).await;
    let said = format!("session://claude/{}", a.conversation.unwrap());
    w.turn(&c, "wa", SAID).await;
    w.kept(&said, "the turn", |k| k.has(ChunkKind::User, SAID)).await;
    let addr = w.json(&["recall", "--json", "zqbackoff"])[0]["addr"].as_str().unwrap().to_owned();
    let item = addr.split('/').next().unwrap().to_owned();

    // As it comes: nothing is offered, and the status says nothing of it.
    let b = w.agent_in(&c, "wb", "claude", Some("work")).await;
    let asking = format!("session://claude/{}", b.conversation.unwrap());
    assert_eq!(ask(&w, &c, "wb", AGAIN).await.context, None, "off unless switched on");
    let status = w.json(&["kb", "--json"]);
    assert_eq!(status["inject"], false);
    assert_eq!(status["offered"], serde_json::json!([]));
    assert!(!w.out(&["kb"]).contains("offered"));

    // Switched on: the same prompt is given a pointer to the session that said it. (Its own
    // session holds the prompt's words by now, and is never pointed at.)
    inject(&w, true).await;
    assert!(
        w.out(&["kb"])
            .contains("agents are offered pointers at a prompt (`kb.inject`): none since")
    );
    let given = ask(&w, &c, "wb", AGAIN).await;
    let text = given.context.clone().expect("a pointer for a prompt that says it again");
    assert!(
        text.starts_with(&format!("<{FENCE} note=\"")) && text.ends_with(&format!("</{FENCE}>")),
        "{text}"
    );
    assert!(text.contains("They are pointers, not the text"), "{text}");
    assert!(text.len() <= MAX_BYTES, "{} bytes", text.len());
    let pointers = given.pointers();
    assert!(!pointers.is_empty() && pointers.len() <= MAX, "{text}");
    assert_eq!(pointers.len(), 1, "only the other session: {text}");
    assert!(
        pointers[0].starts_with(&format!("- {item}/")) && pointers[0].contains(" · session · "),
        "{text}"
    );
    // Never the text: the pointer's line is an address, a kind, a date and a title.
    assert!(pointers[0].chars().count() < 200, "{text}");
    // Within the time a hook has (Claude's is 5 s; ptyro's relay gives up at 500 ms and the
    // search behind it at 150 ms, after which nothing is offered).
    assert!(given.ms < 5_000, "the hook took {} ms", given.ms);
    // What it points at opens with the tool the agent was given.
    let opened = w
        .mcp(
            &c,
            "wb",
            "recall_read",
            serde_json::json!({ "ref": pointers[0][2..].split(' ').next().unwrap() }),
        )
        .await;
    assert!(opened.contains("zqbackoff"), "{opened}");

    // Never twice in one session.
    assert_eq!(ask(&w, &c, "wb", AGAIN).await.context, None, "offered once already");

    // Never out of sight: the status names the session and what it was given.
    let status = w.json(&["kb", "--json"]);
    assert_eq!(status["inject"], true);
    let offered = status["offered"].as_array().unwrap();
    assert_eq!(offered.len(), 1, "{status:#}");
    assert_eq!(offered[0]["name"], "wb");
    assert_eq!(offered[0]["session"], b.id.full());
    assert_eq!(offered[0]["pointers"].as_array().unwrap().len(), 1);
    assert!(offered[0]["pointers"][0]["addr"].as_str().unwrap().starts_with(&item));
    assert_eq!(offered[0]["pointers"][0]["kind"], "session");
    let lines = w.out(&["kb"]);
    assert!(
        lines.contains("agents are offered pointers at a prompt (`kb.inject`); the last 1:"),
        "{lines}"
    );
    assert!(lines.contains(&format!("ago · wb: {item}/")), "{lines}");

    // What was offered is in the agent's own transcript (where Claude keeps a hook's
    // context), and is not kept again as anything that was said.
    let home = w.home();
    let transcripts: Vec<String> = walk(&home.join(".claude-work"))
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    assert!(
        transcripts
            .iter()
            .any(|t| t.contains("hook_additional_context") && t.contains("They are pointers")),
        "the fake agent kept the hook's context as Claude does"
    );
    let k = w
        .kept(&asking, "the asking session", |k| {
            k.chunks.iter().filter(|c| c.kind == ChunkKind::User).count() == 3
        })
        .await;
    assert!(
        !k.chunks.iter().any(|c| c.text.contains(FENCE) || c.text.contains("pointers")),
        "{:?}",
        k.shown()
    );

    // The floor: a prompt that shares one word, or two, with what is kept is offered
    // nothing, and neither is a short one or a command.
    w.agent_in(&c, "wc", "claude", Some("work")).await;
    for quiet in [
        "is the zqupload service healthy right now",
        "what do zqupload and zqbackoff have in common",
        "zqupload retries zqbackoff",
        "/compact keep the zqupload queue retries and zqbackoff",
    ] {
        assert_eq!(ask(&w, &c, "wc", quiet).await.context, None, "{quiet}");
    }
    // …and the same session is still offered the pointer once a prompt asks enough.
    assert!(ask(&w, &c, "wc", AGAIN).await.context.is_some());

    // The wall: another profile's agent asks the same and is given nothing.
    w.agent_in(&c, "hb", "claude", Some("home")).await;
    assert_eq!(ask(&w, &c, "hb", AGAIN).await.context, None, "over the wall");

    // A private session is offered nothing.
    let spec = SpawnSpec {
        agent: Some("claude".into()),
        profile: Some("work".into()),
        cwd: w.home(),
        name: Some("sp".into()),
        size: Some(TermSize::new(100, 20)),
        private: Some(true),
        ..SpawnSpec::default()
    };
    let Reply::Session { info } = c.request(Request::Spawn { spec }).await.unwrap() else {
        panic!()
    };
    assert!(info.private);
    w.d.wait_for_text(&c, "sp", "resumed=", WAIT).await.unwrap();
    assert_eq!(ask(&w, &c, "sp", AGAIN).await.context, None, "a private session");

    // Codex takes it the same way.
    w.agent_in(&c, "wx", "codex", Some("work")).await;
    let given = ask(&w, &c, "wx", AGAIN).await;
    assert!(
        given.pointers().iter().any(|p| p.starts_with(&format!("- {item}/"))),
        "{:?}",
        given.context
    );
    assert!(given.context.unwrap().len() <= MAX_BYTES);

    // Forgotten: what pointed at it is gone from the trace too, and nothing points at it.
    let o = w.cli(&["kb", "forget", &item, "--yes"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let status = w.json(&["kb", "--json"]);
    let left: Vec<String> = status["offered"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|o| o["pointers"].as_array().unwrap().clone())
        .map(|p| p["addr"].as_str().unwrap().to_owned())
        .collect();
    assert!(!left.iter().any(|a| a.starts_with(&item)), "{left:?}");
    w.agent_in(&c, "wd", "claude", Some("work")).await;
    let given = ask(&w, &c, "wd", AGAIN).await;
    assert!(
        !given.pointers().iter().any(|p| p.starts_with(&format!("- {item}/"))),
        "{:?}",
        given.context
    );

    // Switched off again: nothing more.
    inject(&w, false).await;
    w.agent_in(&c, "we", "claude", Some("work")).await;
    assert_eq!(ask(&w, &c, "we", AGAIN).await.context, None);
}

/// Every file under `dir`.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        match e.path() {
            p if p.is_dir() => out.extend(walk(&p)),
            p => out.push(p),
        }
    }
    out
}

/// The switches that take an agent out: `[agents.X] kb = false` (that agent is given no
/// tools and nothing of it is kept: it is offered nothing either), and `kb.tools = false`
/// (no agent could open a pointer).
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_without_the_tools_is_offered_nothing() {
    let on =
        config("[kb]\ninject = true\n").replace("[agents.codex]\n", "[agents.codex]\nkb = false\n");
    let w = World::new(&on);
    let c = w.d.client().await.unwrap();
    let a = w.agent(&c, "a", "claude").await;
    let said = format!("session://claude/{}", a.conversation.unwrap());
    w.turn(&c, "a", SAID).await;
    w.kept(&said, "the turn", |k| k.has(ChunkKind::User, SAID)).await;
    // On from the start: Claude is offered it, Codex (`kb = false`) is not.
    w.agent(&c, "b", "claude").await;
    assert!(ask(&w, &c, "b", AGAIN).await.context.is_some());
    w.agent(&c, "x", "codex").await;
    assert_eq!(ask(&w, &c, "x", AGAIN).await.context, None, "[agents.codex] kb = false");
    // Without the tools nobody is: a pointer nobody can open is noise. (The second setting
    // is how the test knows the daemon has read the first.)
    assert!(w.cli(&["config", "set", "kb.tools", "false"]).status.success());
    assert!(w.cli(&["config", "set", "kb.shell", "off"]).status.success());
    w.status("the settings were read", |s| s["shell"] == "off").await;
    w.agent(&c, "d", "claude").await;
    assert_eq!(ask(&w, &c, "d", AGAIN).await.context, None, "kb.tools = false");
}
