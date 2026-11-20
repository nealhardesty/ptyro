use std::sync::Arc;

use ptyro_core::ErrorCode;
use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::{Skip, Stamp};
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use ptyro_core::model::{ProjectInfo, WorkspaceInfo};
use ptyro_protocol::{Reply, Request};
use ptyro_store::Registry;
use ptyro_store::kb::ItemFilter;

use super::super::search::tests::{prepared, set_config};
use super::super::{Facts, Prepared, Refused};
use crate::daemon::Daemon;
use crate::guard::Caller;

const DAY: u64 = 86_400_000;

fn registry(root: &std::path::Path) -> Registry {
    let workspace = WorkspaceInfo {
        id: "w1".into(),
        machine: None,
        name: "main".into(),
        profile: "work".into(),
        notify: "all".into(),
        projects: vec!["p1".into()],
        created_ms: 0,
    };
    Registry {
        projects: vec![ProjectInfo {
            id: "p1".into(),
            machine: None,
            slug: "feeder".into(),
            root: root.to_path_buf(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        }],
        workspaces: vec![workspace],
        ..Registry::default()
    }
}

/// A session of `claude` under `work` in project `p1`, said at `at_ms`.
fn session(uri: &str, said: &str, at_ms: u64) -> Prepared {
    let mut p = prepared(
        ItemKind::Session,
        uri,
        said,
        vec![
            Part::new(ChunkKind::Card, format!("task: {said}"), at_ms).anchor("card"),
            Part::new(ChunkKind::User, said, at_ms).anchor("u1"),
            Part::new(ChunkKind::Assistant, format!("done: {said}"), at_ms).anchor("a1"),
        ],
        Stamp {
            project: Some("p1".into()),
            workspace: Some("w1".into()),
            agent: Some("claude".into()),
            session: Some(format!("ptyro-{uri}")),
            ..Stamp::new(Writer::Agent, Some("work".into()))
        },
    );
    p.source.append = true;
    (p.source.started_ms, p.source.ended_ms) = (Some(at_ms), Some(at_ms));
    p.facts = Facts { session: true, ..Facts::default() };
    p
}

/// A document of nobody's.
fn doc(uri: &str) -> Prepared {
    prepared(
        ItemKind::File,
        uri,
        "a document",
        vec![Part::new(ChunkKind::Text, "a switch test document", 1)],
        Stamp::new(Writer::File, None),
    )
}

fn daemon(t: &tempfile::TempDir, config: &str) -> Arc<Daemon> {
    let d = crate::daemon::test_daemon(t.path(), registry(t.path()));
    set_config(&d, config);
    d
}

async fn ask(d: &Daemon, req: Request) -> Result<Reply, ptyro_core::Error> {
    d.handle_kb(req, Caller::default()).await
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
    }
}

async fn uris(d: &Daemon, query: &str) -> Vec<String> {
    match ask(d, search(query)).await.unwrap() {
        Reply::KbHits { hits, .. } => hits.into_iter().filter_map(|h| h.uri).collect(),
        r => panic!("{r:?}"),
    }
}

#[tokio::test]
async fn the_requests_search_read_and_list_with_names_and_addresses() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let now = ptyro_core::now_ms();
    d.kb_ingest(session("session://claude/c1", "the hopper jams every morning", now))
        .await
        .unwrap();
    d.kb_ingest(session("session://claude/c2", "the hopper motor hums", now - DAY)).await.unwrap();

    let Reply::KbHits { hits, words_only } = ask(&d, search("hopper jams")).await.unwrap() else {
        panic!()
    };
    assert!(words_only.is_some_and(|w| w.contains("meaning search is off")));
    let h = &hits[0];
    assert_eq!(h.uri.as_deref(), Some("session://claude/c1"));
    // The ids, and what they are called here.
    assert_eq!((h.project.as_deref(), h.workspace.as_deref()), (Some("p1"), Some("w1")));
    assert_eq!(
        (h.project_name.as_deref(), h.workspace_name.as_deref()),
        (Some("feeder"), Some("main"))
    );
    assert!(h.addr.starts_with("k:") && h.addr.contains('/'), "{}", h.addr);

    // `here`: a project by its id, its name or a path in it; a place that is none is nothing.
    for here in ["p1", "feeder", &t.path().display().to_string(), "/nowhere/at/all"] {
        let mut req = search("hopper");
        if let Request::KbSearch { here: h, .. } = &mut req {
            *h = Some(here.to_owned());
        }
        assert!(
            matches!(ask(&d, req).await.unwrap(), Reply::KbHits { hits, .. } if hits.len() == 2)
        );
    }

    // A piece with two neighbours each side unless told; an item, all of it.
    let read = |addr: &str, before, after| Request::KbRead { addr: addr.into(), before, after };
    let Reply::KbDoc { item, chunks, at, stale } =
        ask(&d, read(&h.addr, None, None)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((at, stale, chunks.len()), (h.idx, false, 3));
    assert_eq!(
        (item.item.uri.as_str(), item.project_name.as_deref()),
        ("session://claude/c1", Some("feeder"))
    );
    assert!(h.addr.starts_with(&format!("{}/", item.addr)), "{} of {}", h.addr, item.addr);
    let Reply::KbDoc { chunks, at, .. } = ask(&d, read(&item.addr, None, None)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((at, chunks.len()), (None, 3));
    let Reply::KbDoc { chunks, .. } =
        ask(&d, read(&format!("{}/1", item.addr), Some(0), Some(0))).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ["the hopper jams every morning"]
    );
    let e = ask(&d, read("c:7f3k/01JB", None, None)).await.unwrap_err();
    assert!(e.message.contains("ptyro chat show"), "{}", e.message);

    // The history, newest first, paged.
    let recent = |before_ms, limit| Request::KbRecent {
        kinds: vec![ItemKind::Session],
        project: Some("feeder".into()),
        before_ms,
        limit,
    };
    let Reply::KbItems { items } = ask(&d, recent(None, Some(1))).await.unwrap() else { panic!() };
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].item.uri.as_str(), items[0].addr.as_str()),
        ("session://claude/c1", item.addr.as_str())
    );
    let Reply::KbItems { items } = ask(&d, recent(items[0].item.ended_ms, None)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        items.iter().map(|i| i.item.uri.as_str()).collect::<Vec<_>>(),
        ["session://claude/c2"]
    );
}

#[tokio::test]
async fn forgetting_asks_first_then_deletes_for_good() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let now = ptyro_core::now_ms();
    for (uri, said) in [
        ("session://claude/c1", "the zebra crossing plan"),
        ("session://claude/c2", "the zebra stripes paint"),
        ("session://claude/c3", "an unrelated giraffe"),
    ] {
        d.kb_ingest(session(uri, said, now)).await.unwrap();
    }
    let forget = |addrs: &[&str], query: Option<&str>, confirmed| Request::KbForget {
        session: None,
        addrs: addrs.iter().map(|a| (*a).to_owned()).collect(),
        query: query.map(str::to_owned),
        confirmed,
    };
    // Nothing named.
    assert_eq!(ask(&d, forget(&[], None, true)).await.unwrap_err().code, ErrorCode::Invalid);
    assert_eq!(
        ask(&d, forget(&["k:zzzz"], None, true)).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    let e = ask(&d, forget(&["c:7f3k"], None, true)).await.unwrap_err();
    assert!(e.message.contains("ptyro chat rm"), "{}", e.message);
    let e = ask(&d, forget(&[], Some("walrus"), true)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);

    // Not agreed to: the question says what would go, and nothing does.
    let e = ask(&d, forget(&[], Some("zebra"), false)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(e.message.starts_with("Forget 2 items: "), "{}", e.message);
    assert!(
        e.message.contains("“the zebra crossing plan”") && e.message.contains("for good"),
        "{}",
        e.message
    );
    assert_eq!(uris(&d, "zebra").await.len(), 2);

    // Agreed to: both are gone from every answer, their URIs are never kept again, and the
    // third is as it was.
    let Reply::KbForgotten { items, chunks } =
        ask(&d, forget(&[], Some("zebra"), true)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((items, chunks), (2, 6));
    assert!(uris(&d, "zebra").await.is_empty());
    assert!(matches!(
        d.kb_ingest(session("session://claude/c1", "the zebra is back", now)).await,
        Err(Refused::Skip(Skip::Forgotten))
    ));
    assert!(uris(&d, "zebra").await.is_empty());
    // One by its address.
    let Reply::KbItems { items } =
        ask(&d, Request::KbRecent { kinds: vec![], project: None, before_ms: None, limit: None })
            .await
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(items.len(), 1);
    let e = ask(&d, forget(&[&items[0].addr], None, false)).await.unwrap_err();
    assert!(
        e.message.starts_with("Forget 1 item: session “an unrelated giraffe”?"),
        "{}",
        e.message
    );
    assert!(matches!(
        ask(&d, forget(&[&items[0].addr, &items[0].item.id], None, true)).await.unwrap(),
        Reply::KbForgotten { items: 1, chunks: 3 }
    ));
    let Reply::KbStatus { status } = ask(&d, Request::KbStatus).await.unwrap() else { panic!() };
    assert_eq!((status.forgotten, status.kinds.len()), (3, 0));
}

#[tokio::test]
async fn each_switch_keeps_a_session_out_and_the_status_says_why() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let now = ptyro_core::now_ms();
    let kept = |d: &Arc<Daemon>, uri: &str, facts: Facts| {
        let d = Arc::clone(d);
        let mut p =
            if uri.starts_with("file:") { doc(uri) } else { session(uri, "a switch test", now) };
        p.facts = facts;
        async move { d.kb_ingest(p).await.map(|_| ()) }
    };
    let session_facts = || Facts { session: true, ..Facts::default() };
    kept(&d, "session://claude/on", session_facts()).await.unwrap();

    // `kb.sessions = false`: no session is kept; a document still is.
    set_config(&d, "[kb]\nsessions = false\n");
    assert!(matches!(
        kept(&d, "session://claude/s-off", session_facts()).await,
        Err(Refused::Skip(Skip::SessionsOff))
    ));
    kept(&d, "file:///doc.md", Facts::default()).await.unwrap();

    // `[agents.claude] kb = false`: that agent's, by its name (`claude@work` is claude).
    set_config(&d, "[agents.claude]\nkb = false\n");
    assert!(d.kb_agent_off(Some("claude")) && d.kb_agent_off(Some("claude@work")));
    assert!(!d.kb_agent_off(Some("codex")) && !d.kb_agent_off(None));
    let facts = Facts { agent_off: d.kb_agent_off(Some("claude")), ..session_facts() };
    assert!(matches!(
        kept(&d, "session://claude/a-off", facts).await,
        Err(Refused::Skip(Skip::AgentOff))
    ));

    // The project's own switch, in its `.ptyro.toml`; a repository can't switch it on.
    set_config(&d, "");
    assert!(!d.kb_project_off(Some("p1"), None) && !d.kb_project_off(None, None));
    std::fs::write(t.path().join(".ptyro.toml"), "[knowledge]\nenabled = false\n").unwrap();
    assert!(d.kb_project_off(Some("p1"), None) && !d.kb_project_off(Some("p9"), None));
    let facts = Facts { project_off: true, ..session_facts() };
    assert!(matches!(
        kept(&d, "session://claude/p-off", facts).await,
        Err(Refused::Skip(Skip::ProjectOff))
    ));
    std::fs::write(t.path().join(".ptyro.toml"), "[knowledge]\nenabled = true\n").unwrap();
    assert!(!d.kb_project_off(Some("p1"), None));
    let e = ptyro_core::project::ProjectConfig::load(t.path(), t.path());
    assert!(e.is_ok());
    std::fs::write(t.path().join(".ptyro.toml"), "[kb]\nenabled = true\n").unwrap();
    let e = ptyro_core::project::ProjectConfig::load(t.path(), t.path()).unwrap_err();
    assert!(e.contains("[knowledge] enabled = false"), "{e}");

    // `kb.max_file`: the gate's limit is the setting's.
    set_config(&d, "[kb]\nmax_file = '1KiB'\n");
    let facts = Facts { bytes: Some(1025), ..Facts::default() };
    assert!(matches!(
        kept(&d, "file:///big.md", facts).await,
        Err(Refused::Skip(Skip::TooBig { bytes: 1025, max: 1024 }))
    ));
    let facts = Facts { bytes: Some(1024), ..Facts::default() };
    kept(&d, "file:///fits.md", facts).await.unwrap();

    // Only what was let through is there.
    let db = d.kb_db().unwrap();
    let mut have: Vec<String> =
        db.list(ItemFilter::default()).await.unwrap().into_iter().map(|i| i.uri).collect();
    have.sort();
    assert_eq!(have, ["file:///doc.md", "file:///fits.md", "session://claude/on"]);

    // The status: on, what it holds; off, that it is, and nothing is opened or searched.
    set_config(&d, "[kb]\nretain_days = 30\n");
    let Reply::KbStatus { status } = ask(&d, Request::KbStatus).await.unwrap() else { panic!() };
    assert!(status.enabled && status.sessions && status.error.is_none());
    assert_eq!((status.retain_days, status.queued, status.forgotten), (30, 0, 0));
    assert!(status.path.ends_with("kb.db") && status.bytes > 0);
    assert_eq!(
        status.kinds.iter().map(|k| (k.kind, k.items, k.chunks)).collect::<Vec<_>>(),
        [(ItemKind::File, 2, 2), (ItemKind::Session, 1, 3)]
    );
    assert!(status.meaning.is_some_and(|m| m.contains("off")));
    set_config(&d, "[kb]\nenabled = false\n");
    let Reply::KbStatus { status } = ask(&d, Request::KbStatus).await.unwrap() else { panic!() };
    assert!(!status.enabled && status.kinds.is_empty() && status.bytes > 0);
    let e = ask(&d, search("switch")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(e.message.contains("the knowledge base is off"), "{}", e.message);
}

#[tokio::test]
async fn retention_deletes_old_sessions_and_nothing_else() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let now = ptyro_core::now_ms();
    d.kb_ingest(session("session://claude/old", "an old quokka session", now - 40 * DAY))
        .await
        .unwrap();
    d.kb_ingest(session("session://claude/new", "a new quokka session", now - DAY)).await.unwrap();
    // A document of the same age: current by being read again, never aged out.
    let mut doc = prepared(
        ItemKind::File,
        "file:///old.md",
        "old.md",
        vec![Part::new(ChunkKind::Text, "an old quokka document", now - 400 * DAY)],
        Stamp::new(Writer::File, None),
    );
    (doc.source.started_ms, doc.source.ended_ms) = (Some(now - 400 * DAY), Some(now - 400 * DAY));
    d.kb_ingest(doc).await.unwrap();

    // 0: for good.
    assert_eq!((d.kb_retain_cut(), d.kb_retention_pass().await), (None, 0));
    assert_eq!(uris(&d, "quokka").await.len(), 3);
    // Off: nothing is touched while it is.
    set_config(&d, "[kb]\nenabled = false\nretain_days = 30\n");
    assert_eq!(d.kb_retention_pass().await, 0);
    set_config(&d, "[kb]\nretain_days = 30\n");
    assert_eq!(d.kb_retention_pass().await, 1);
    let mut left = uris(&d, "quokka").await;
    left.sort();
    assert_eq!(left, ["file:///old.md", "session://claude/new"]);
    assert_eq!(d.kb_retention_pass().await, 0);
    // No tombstone: a session that is carried on is kept again.
    d.kb_ingest(session("session://claude/old", "an old quokka session", now)).await.unwrap();
    assert_eq!(uris(&d, "quokka").await.len(), 3);
}
