use std::io::Write;
use std::time::{Duration, SystemTime};

use ptyro_core::kb::{ChunkKind, KbItem};
use ptyro_core::model::{WorktreeInfo, WorktreeState};
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::ItemFilter;

use super::super::requests::tests::{ask, daemon};
use super::super::search::tests::set_config;
use super::*;

const A: &str = "1428ccdc-2336-4067-abd0-ac2ef1665b1f";
const B: &str = "6992c0ad-dafa-428e-8e80-8de66de55326";
const C: &str = "01a1f5f6-b278-76d1-8a83-cfe1291c7e0d";
const CHAT: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const PRIVATE: &str = "aaaaaaaa-0000-4000-8000-000000000002";
const HERE: &str = "aaaaaaaa-0000-4000-8000-000000000003";
const OLD: &str = "aaaaaaaa-0000-4000-8000-000000000004";

fn fixture(rel: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ptyro-agents/tests/fixtures").join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn put(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A Claude Code conversation of one turn that ran in `cwd`, with a line that is not
/// conversation and holds an account's email.
fn claude_turn(id: &str, cwd: &Path, asked: &str, answered: &str) -> Vec<u8> {
    let cwd = cwd.to_string_lossy();
    let lines = [
        serde_json::json!({"type": "api-request", "sessionId": id, "email": "someone@example.com"}),
        serde_json::json!({"type": "user", "uuid": format!("u-{id}"), "sessionId": id, "cwd": cwd,
            "timestamp": "2026-11-18T10:00:00.000Z", "message": {"role": "user", "content": asked}}),
        serde_json::json!({"type": "assistant", "uuid": format!("a-{id}"), "sessionId": id, "cwd": cwd,
            "timestamp": "2026-11-18T10:00:05.000Z",
            "message": {"role": "assistant", "model": "claude-test", "content": [{"type": "text", "text": answered}]}}),
    ];
    lines.iter().flat_map(|l| format!("{l}\n").into_bytes()).collect()
}

async fn history(d: &Arc<Daemon>, agent: Option<&str>, dry_run: bool) -> KbHistoryPlan {
    let req = Request::KbHistory {
        agent: agent.map(str::to_owned),
        // (Never the default profile here: its folders are the real `~/.claude` and `~/.codex`.)
        profile: Some("work".into()),
        since_ms: None,
        dry_run,
        machine: None,
    };
    let plan = match ask(d, req).await.unwrap() {
        Reply::KbHistory { plan } => plan,
        r => panic!("{r:?}"),
    };
    d.kb_idle().await;
    plan
}

async fn sessions(d: &Arc<Daemon>) -> Vec<KbItem> {
    let f = ItemFilter { kinds: vec![ItemKind::Session], ..ItemFilter::default() };
    let mut items = d.kb_db().unwrap().list(f).await.unwrap();
    items.sort_by(|a, b| a.uri.cmp(&b.uri));
    items
}

#[tokio::test]
async fn what_the_agents_kept_is_counted_then_read_once() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    d.start_kb();
    let project = ptyro_core::os::canonicalize(t.path()).unwrap();
    d.registry.locked().worktrees.push(WorktreeInfo {
        id: "wt1".into(),
        machine: None,
        project: "p1".into(),
        project_slug: "feeder".into(),
        name: "@root".into(),
        path: project.clone(),
        branch: None,
        base: None,
        is_root: true,
        profile: None,
        state: WorktreeState::Ready,
        created_ms: 0,
        removed_ms: None,
        last_used_ms: None,
        status: None,
        locked: false,
        title: None,
    });
    // Where profile `work` keeps its agents' folders.
    let claude = d.paths.data.join("profiles/work/claude");
    let codex = d.paths.data.join("profiles/work/codex");
    let at = |id: &str| claude.join("projects/-home-u-proj").join(format!("{id}.jsonl"));
    put(&at(A), &fixture("claude-2.1.329/transcripts/plain.jsonl"));
    put(&at(B), &fixture("claude-2.1.329/transcripts/tools.jsonl"));
    put(
        &at(HERE),
        &claude_turn(HERE, &project.join("src"), "why does the hopper jam", "the belt slips"),
    );
    // A sub-agent's file, a chat's engine, a private session's, and one too old to keep.
    put(
        &claude.join("projects/-home-u-proj").join(B).join("subagents/agent-1.jsonl"),
        &fixture("claude-2.1.329/transcripts/plain.jsonl"),
    );
    let engine = d.paths.data.join("chat/c1");
    std::fs::create_dir_all(&engine).unwrap();
    put(&at(CHAT), &claude_turn(CHAT, &engine, "a chat's question", "a chat's answer"));
    put(&at(PRIVATE), &claude_turn(PRIVATE, &project, "a private question", "a private answer"));
    put(&at(OLD), &claude_turn(OLD, &project, "an old question", "an old answer"));
    let long_ago = SystemTime::now() - Duration::from_secs(40 * 86_400);
    std::fs::File::options().write(true).open(at(OLD)).unwrap().set_modified(long_ago).unwrap();
    put(
        &codex.join(format!("sessions/2026/11/19/rollout-2026-11-19T22-39-07-{C}.jsonl")),
        &fixture("codex-0.167.1/transcripts/plain.jsonl"),
    );
    let db = d.kb_db().unwrap();
    db.tombstone(format!("session://claude/{PRIVATE}"), ItemKind::Session).await.unwrap();
    set_config(&d, "[kb]\nprojects = false\nretain_days = 30\n");

    // Counted first, and with `--dry-run` nothing more: no file is opened.
    let plan = history(&d, None, true).await;
    assert_eq!(
        (plan.found, plan.known, plan.old, plan.chats, plan.new, plan.reading),
        (7, 1, 1, 0, 5, false),
        "{plan:?}"
    );
    assert_eq!(
        plan.places
            .iter()
            .map(|p| (p.agent.as_str(), p.profile.as_str(), p.found, p.new))
            .collect::<Vec<_>>(),
        [("claude", "work", 6, 4), ("codex", "work", 1, 1)]
    );
    assert!(plan.places[0].dir.ends_with("profiles/work/claude"));
    assert!(sessions(&d).await.is_empty());
    assert_eq!(history(&d, Some("codex"), true).await.found, 1);
    let e = ask(
        &d,
        Request::KbHistory {
            agent: Some("opencode".into()),
            profile: None,
            since_ms: None,
            dry_run: true,
            machine: None,
        },
    )
    .await
    .unwrap_err();
    assert!(e.message.contains("claude and codex"), "{}", e.message);

    // Read: each conversation is an item written by `import`, under the profile whose folder
    // held it; the chat's engine is passed over once its folder is known.
    let plan = history(&d, None, false).await;
    assert!(plan.reading && plan.new == 5, "{plan:?}");
    let kept = sessions(&d).await;
    assert_eq!(
        kept.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
        [
            format!("session://claude/{A}"),
            format!("session://claude/{B}"),
            format!("session://claude/{HERE}"),
            format!("session://codex/{C}"),
        ]
    );
    for i in &kept {
        assert_eq!(
            (i.writer, i.profile.as_deref(), i.session.as_deref()),
            (Writer::Import, Some("work"), None),
            "{}",
            i.uri
        );
        assert!(i.cursor.is_some() && i.started_ms.is_some(), "{}", i.uri);
    }
    assert_eq!(kept[0].agent.as_deref(), Some("claude"));
    assert_eq!(kept[3].agent.as_deref(), Some("codex"));
    assert_eq!((kept[0].project.as_deref(), kept[2].project.as_deref()), (None, Some("p1")));
    assert_eq!(kept[0].title.as_deref(), Some("Reply with only the word OK."));
    let chunks = db.chunks(kept[0].id.clone(), 0, 0).await.unwrap();
    assert_eq!(
        chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChunkKind::Card, ChunkKind::User, ChunkKind::Assistant]
    );
    assert!(chunks[0].text.contains("claude"), "{}", chunks[0].text);
    let here = db.chunks(kept[2].id.clone(), 0, 0).await.unwrap();
    assert!(here[0].text.contains("feeder"), "{}", here[0].text);
    assert_eq!(here[1].text, "why does the hopper jam");
    // Never a line that is not conversation.
    for i in &kept {
        for c in db.chunks(i.id.clone(), 0, 0).await.unwrap() {
            assert!(!c.text.contains("example.com"), "{}", c.text);
        }
    }
    let tools = db.chunks(kept[1].id.clone(), 0, 0).await.unwrap();
    assert!(tools.iter().any(|c| c.kind == ChunkKind::Tool) && kept[1].files.len() == 1);

    // Again: everything is known (the chat's engine is still passed over), nothing is read.
    let plan = history(&d, None, false).await;
    assert_eq!((plan.found, plan.known, plan.old, plan.new), (7, 5, 1, 1), "{plan:?}");
    assert_eq!(sessions(&d).await.len(), 4);
    // What was forgotten stays out, whatever is asked.
    db.forget(kept[0].id.clone()).await.unwrap();
    history(&d, None, false).await;
    assert_eq!(sessions(&d).await.len(), 3);

    // A conversation carried on by hand is read on at the daemon's next start.
    let mut f = std::fs::File::options().append(true).open(at(HERE)).unwrap();
    f.write_all(&claude_turn(&format!("{HERE}-2"), &project, "and the motor", "it hums")[..])
        .unwrap();
    drop(f);
    d.kb_catch_up().await;
    d.kb_idle().await;
    let on = db.chunks(kept[2].id.clone(), 0, 0).await.unwrap();
    assert!(
        on.iter().any(|c| c.text == "and the motor"),
        "{:?}",
        on.iter().map(|c| &c.text).collect::<Vec<_>>()
    );
    assert_eq!(sessions(&d).await[1].writer, Writer::Import);

    // An agent kept out of the knowledge base, and sessions switched off.
    set_config(&d, "[kb]\nprojects = false\n[agents.codex]\nkb = false\n");
    let plan = history(&d, None, true).await;
    assert_eq!(plan.places.len(), 1, "{plan:?}");
    // (Without `retain_days` the old one would be read.)
    assert_eq!((plan.old, plan.new), (0, 2), "{plan:?}");
    set_config(&d, "[kb]\nprojects = false\nsessions = false\n");
    let e = ask(
        &d,
        Request::KbHistory {
            agent: None,
            profile: Some("work".into()),
            since_ms: None,
            dry_run: true,
            machine: None,
        },
    )
    .await
    .unwrap_err();
    assert!(e.message.contains("kb.sessions"), "{}", e.message);
}

#[tokio::test]
async fn since_and_a_project_that_keeps_itself_out() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    d.start_kb();
    let project = ptyro_core::os::canonicalize(t.path()).unwrap();
    d.registry.locked().worktrees.push(WorktreeInfo {
        id: "wt1".into(),
        machine: None,
        project: "p1".into(),
        project_slug: "feeder".into(),
        name: "@root".into(),
        path: project.clone(),
        branch: None,
        base: None,
        is_root: true,
        profile: None,
        state: WorktreeState::Ready,
        created_ms: 0,
        removed_ms: None,
        last_used_ms: None,
        status: None,
        locked: false,
        title: None,
    });
    let claude = d.paths.data.join("profiles/work/claude");
    let at = |id: &str| claude.join("projects/-x").join(format!("{id}.jsonl"));
    put(&at(A), &claude_turn(A, Path::new("/somewhere/else"), "elsewhere", "fine"));
    put(&at(HERE), &claude_turn(HERE, &project, "in the project", "kept out"));
    put(&at(OLD), &claude_turn(OLD, &project, "last week", "old"));
    let week = SystemTime::now() - Duration::from_secs(7 * 86_400);
    std::fs::File::options().write(true).open(at(OLD)).unwrap().set_modified(week).unwrap();
    std::fs::write(project.join(".ptyro.toml"), "[knowledge]\nenabled = false\n").unwrap();

    let since = ptyro_core::now_ms() - 86_400_000;
    let req = Request::KbHistory {
        agent: None,
        profile: Some("work".into()),
        since_ms: Some(since),
        dry_run: false,
        machine: None,
    };
    let Reply::KbHistory { plan } = ask(&d, req).await.unwrap() else { panic!() };
    assert_eq!((plan.found, plan.old, plan.new), (3, 1, 2), "{plan:?}");
    d.kb_idle().await;
    // The one that ran in the project is refused by the gate, with why.
    let kept = sessions(&d).await;
    assert_eq!(
        kept.iter().map(|i| i.uri.clone()).collect::<Vec<_>>(),
        [format!("session://claude/{A}")]
    );
    let skipped = d.kb_skipped();
    assert!(
        skipped.iter().any(|s| s.uri.ends_with(HERE) && s.why.contains("project")),
        "{skipped:?}"
    );
}
