use std::sync::Arc;

use ptyro_core::ErrorCode;
use ptyro_core::chat::ChatBody;
use ptyro_core::chats::{ChatNode, MARK_OFF, MARK_ON};
use ptyro_core::kb::ChunkKind;
use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::{Body, Source, Stamp};
use ptyro_store::Registry;
use ptyro_store::chat::NewChat;

use super::super::{Facts, Prepared};
use super::*;

/// The config `toml`, with the two profiles the tests' items are of.
pub(crate) fn set_config(d: &Daemon, toml: &str) {
    let toml = format!("{toml}\n[profiles.work]\n[profiles.home]\n");
    *d.config.write().unwrap() = ptyro_core::config::Config::parse(&toml).unwrap();
}

/// A model where the real one would be downloaded to: nothing leaves the machine.
pub(crate) fn put_model(d: &Daemon, tokens: &[(&str, &[f32])]) {
    let dir = d.paths.data.join("models").join(ptyro_meaning::NAME);
    ptyro_meaning::synthetic::write(&dir, tokens).unwrap();
}

/// An item of `parts`, all of it, as a reader would hand it over.
pub(crate) fn prepared(
    kind: ItemKind,
    uri: &str,
    title: &str,
    parts: Vec<Part>,
    stamp: Stamp,
) -> Prepared {
    let mut source = Source::new(kind, uri, Body::Parts(parts));
    source.title = Some(title.to_owned());
    Prepared { source, stamp, facts: Facts::default() }
}

/// A chat with one message of each of `said` (user, then assistant, in turn).
pub(crate) async fn chat(
    d: &Daemon,
    id: &str,
    profile: &str,
    workspace: &str,
    said: &[&str],
    at_ms: u64,
) {
    let db = d.chats.db().unwrap();
    db.create(NewChat {
        id: id.into(),
        workspace: workspace.into(),
        title: None,
        title_src: "auto".into(),
        agent: "claude".into(),
        profile: profile.into(),
        tools: "chat".into(),
        folders: Vec::new(),
    })
    .await
    .unwrap();
    let mut parent = None;
    for (i, text) in said.iter().enumerate() {
        let node = format!("{id}-n{i}");
        let body = match i % 2 {
            0 => ChatBody::User { text: (*text).into() },
            _ => ChatBody::Assistant { text: (*text).into(), streaming: false },
        };
        let n = ChatNode {
            id: node.clone(),
            parent: parent.replace(node),
            at_ms: at_ms + i as u64,
            body,
        };
        db.put_node_now(id.into(), n, None, false).await.unwrap();
    }
}

/// Every chat message gets its vector (what the backfill task does, at once).
pub(crate) async fn embed_chats(d: &Daemon) {
    let db = d.chats.db().unwrap();
    let model = d.meaning_model().await.unwrap();
    let todo = db.vec_todo(MODEL_ID.into(), false, (0, i64::MAX), 10_000).await.unwrap();
    let rows = todo
        .into_iter()
        .map(|t| {
            let v = ptyro_meaning::quantize(&model.embed(&t.text));
            (t.node, t.text, v)
        })
        .collect();
    db.put_vecs(MODEL_ID.into(), rows).await.unwrap();
}

fn stamp(writer: Writer, profile: Option<&str>) -> Stamp {
    Stamp::new(writer, profile.map(str::to_owned))
}

fn ask(query: &str) -> KbSearch {
    KbSearch { query: query.into(), ..KbSearch::default() }
}

/// What was found, as `uri` or `chat:<id>/<node>`, in order.
fn found(hits: &KbHits) -> Vec<String> {
    hits.hits
        .iter()
        .map(|h| match &h.uri {
            Some(u) => u.clone(),
            None => format!("chat:{}/{}", h.item, h.node.as_deref().unwrap_or("title")),
        })
        .collect()
}

/// A work session, a home note, a document of nobody's; a work chat and a home chat. The
/// model knows `cat` and `kitten` as near, `dog` as far.
async fn daemon(t: &tempfile::TempDir) -> Arc<Daemon> {
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    put_model(&d, &[("cat", &[1.0, 0.0]), ("kitten", &[0.95, 0.05]), ("dog", &[0.0, 1.0])]);
    set_config(&d, "[kb]\nmeaning = true\n");
    // (Not `start_kb`: its pass over the notes folder drops a note that has no file, as the
    // one below, whenever it happens to run.)
    let now = now_ms();
    let session = prepared(
        ItemKind::Session,
        "session://claude/work",
        "Feeding the cat",
        vec![
            Part::new(ChunkKind::Card, "task: feed the cat on a schedule", now).anchor("card"),
            Part::new(ChunkKind::User, "the cat feeder jams every morning", now).anchor("u1"),
            Part::new(ChunkKind::Assistant, "the hopper sticks; the cat is fine", now).anchor("a1"),
        ],
        Stamp {
            session: Some("ptyro-s1".into()),
            workspace: Some("w1".into()),
            project: Some("p1".into()),
            agent: Some("claude".into()),
            ..stamp(Writer::Agent, Some("work"))
        },
    );
    let note = prepared(
        ItemKind::Note,
        "note://home/_global/vet",
        "Vet",
        vec![Part::new(ChunkKind::Text, "the cat goes to the vet in March", now)],
        stamp(Writer::User, Some("home")),
    );
    let doc = prepared(
        ItemKind::File,
        "file:///docs/pets.md",
        "pets.md",
        vec![
            Part::new(ChunkKind::Text, "A dog needs walking twice a day.", now)
                .heading("pets.md › Dogs"),
            Part::new(ChunkKind::Text, "A cat needs a clean box.", now).heading("pets.md › Cats"),
        ],
        stamp(Writer::File, None),
    );
    for p in [session, note, doc] {
        d.kb_ingest(p).await.unwrap();
    }
    chat(&d, "chat-work", "work", "w1", &["is the cat asleep", "the cat is asleep"], now).await;
    chat(&d, "chat-home", "home", "w2", &["where is my cat"], now).await;
    embed_chats(&d).await;
    d
}

#[tokio::test]
async fn one_search_over_both_files_ranked_and_addressed() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t).await;
    // What an ingest writes has its vectors at once: the embed step.
    let db = d.kb_db().unwrap();
    assert_eq!(db.vec_counts(MODEL_ID.into()).await.unwrap(), (6, 6));

    let hits = d.kb_search(ask("cat"), None).await.unwrap();
    assert_eq!(hits.words_only, None);
    let all = found(&hits);
    // The note you wrote first, then the session's card; both files are in the answer, at
    // most two hits of one item or one chat, and neighbouring chunks as one hit.
    assert_eq!(all[0], "note://home/_global/vet");
    assert_eq!(all[1], "session://claude/work");
    assert_eq!(all.iter().filter(|u| u.as_str() == "session://claude/work").count(), 1);
    assert_eq!(all.iter().filter(|u| u.starts_with("chat:chat-work/")).count(), 2);
    assert!(all.contains(&"chat:chat-home/chat-home-n0".to_owned()), "{all:?}");
    assert!(all.contains(&"file:///docs/pets.md".to_owned()), "{all:?}");
    assert_eq!(all.len(), 6, "{all:?}");
    let card = &hits.hits[1];
    assert_eq!(
        (card.kind, card.part.as_str(), card.how.as_str()),
        (HitKind::Session, "card", "both")
    );
    assert!(
        card.snippet.matches(MARK_ON).count() == 3 && card.snippet.contains(" … "),
        "{:?}",
        card.snippet
    );
    assert_eq!(card.snippet.matches(MARK_OFF).count(), 3);
    assert!(hits.hits.windows(2).all(|p| p[0].score >= p[1].score));
    // Addresses: an item's handle and the chunk, a chat's handle and the message; each opens.
    assert!(card.addr.starts_with("k:") && card.addr.ends_with("/0"), "{}", card.addr);
    assert!(card.addr.len() < 12, "a handle, not the id: {}", card.addr);
    let opened = d.kb_read(&card.addr, 0, 1, None).await.unwrap();
    assert_eq!(
        (opened.item.uri.as_str(), opened.at, opened.chunks.len()),
        ("session://claude/work", Some(0), 2)
    );
    let c = hits.hits.iter().find(|h| h.kind == HitKind::Chat).unwrap();
    assert!(c.addr.starts_with("c:") && c.addr.contains("/chat-"), "{}", c.addr);
    assert_eq!((c.profile.is_some(), c.writer, c.uri.as_deref()), (true, None, None));

    // By meaning alone: no word of "kitten" is anywhere, and the cats are found (`≈`).
    let near = d.kb_search(ask("kitten"), None).await.unwrap();
    assert!(near.hits.len() >= 5 && near.hits.iter().all(|h| h.how == "meaning"), "{near:?}");
    assert!(!found(&near).iter().any(|u| u.contains("Dogs")));
    assert!(d.kb_search(ask("kitten by:words"), None).await.unwrap().hits.is_empty());
    let dogs = d.kb_search(ask("dog by:meaning"), None).await.unwrap();
    assert_eq!(dogs.hits[0].heading.as_deref(), Some("pets.md › Dogs"));
    assert_eq!(dogs.hits[0].how, "meaning");

    // The query's filters, and a caller's.
    let only = async |q: &str| found(&d.kb_search(ask(q), None).await.unwrap());
    assert_eq!(only("cat kind:note").await, ["note://home/_global/vet"]);
    assert!(only("cat kind:chat").await.iter().all(|u| u.starts_with("chat:")));
    assert_eq!(only("cat kind:chat").await.len(), 3);
    assert!(
        !only("cat kind:session,doc")
            .await
            .iter()
            .any(|u| u.starts_with("chat:") || u.starts_with("note:"))
    );
    assert_eq!(only("cat wrote:you").await, ["note://home/_global/vet"]);
    assert_eq!(only("cat file:pets").await, ["file:///docs/pets.md"]);
    assert_eq!(only("cat agent:claude -asleep -where").await, ["session://claude/work"]);
    assert!(only("cat before:1h").await.is_empty());
    assert_eq!(only("cat since:1h").await.len(), 6);
    let narrowed = KbSearch { kinds: vec![HitKind::Note, HitKind::Doc], ..ask("cat kind:doc") };
    assert_eq!(found(&d.kb_search(narrowed, None).await.unwrap()), ["file:///docs/pets.md"]);
    let none = KbSearch { kinds: vec![HitKind::Note], ..ask("cat kind:doc") };
    assert!(d.kb_search(none, None).await.unwrap().hits.is_empty());
    let two = KbSearch { limit: 2, ..ask("cat") };
    assert_eq!(d.kb_search(two, None).await.unwrap().hits.len(), 2);
    // Where you are lifts its hits over the note; it filters nothing.
    let here = KbSearch { here: Some("p1".into()), ..ask("cat by:words") };
    let here = d.kb_search(here, None).await.unwrap();
    assert_eq!(found(&here)[0], "session://claude/work");
    assert_eq!(here.hits.len(), 6);
    // What there isn't is said, not searched for.
    let e = d.kb_search(ask("cat project:nope"), None).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    assert!(d.kb_search(ask("cat root:nope"), None).await.is_err());
    assert!(
        d.kb_search(ask("cat machine:laptop2"), None)
            .await
            .unwrap_err()
            .message
            .contains("this machine only")
    );
}

#[tokio::test]
async fn the_wall_and_what_is_forgotten() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t).await;
    let see = async |q: &str, s: &KbScope| found(&d.kb_search(ask(q), Some(s)).await.unwrap());
    // A work session's model: its profile's, and what is nobody's. Never home's, in either
    // file, by words or by meaning, whatever the query filters.
    let work = KbScope::of("profile", "work", Some("w1"));
    for q in [
        "cat",
        "kitten",
        "cat by:words",
        "cat kind:note",
        "cat kind:chat",
        "vet",
        "cat wrote:you",
        "cat since:1d",
    ] {
        let got = see(q, &work).await;
        assert!(!got.iter().any(|u| u.contains("home")), "{q}: {got:?}");
    }
    let got = see("cat", &work).await;
    assert!(got.contains(&"file:///docs/pets.md".to_owned()) && got.len() == 4, "{got:?}");
    let home = KbScope::of("profile", "home", None);
    let got = see("cat", &home).await;
    assert_eq!(got.iter().filter(|u| u.contains("work")).count(), 0, "{got:?}");
    assert_eq!(got.len(), 3);
    // `workspace` narrows to the caller's workspace; `all` takes the wall down.
    let got = see("cat", &KbScope::of("workspace", "work", Some("w1"))).await;
    assert!(got.len() == 3 && got.iter().all(|u| u.contains("work")), "{got:?}");
    assert_eq!(see("cat", &KbScope::of("all", "work", Some("w1"))).await.len(), 6);
    // Its own session, and its own chat, are not what it is looking for.
    let own = KbScope {
        session: Some("ptyro-s1".into()),
        chat: Some("chat-work".into()),
        ..work.clone()
    };
    assert_eq!(see("cat", &own).await, ["file:///docs/pets.md"]);
    // Behind the wall an address doesn't open, and the history doesn't list.
    let note = d.kb_search(ask("vet"), None).await.unwrap().hits[0].addr.clone();
    assert_eq!(d.kb_read(&note, 0, 0, None).await.unwrap().item.uri, "note://home/_global/vet");
    assert_eq!(d.kb_read(&note, 0, 0, Some(&work)).await.unwrap_err().code, ErrorCode::NotFound);
    let recent = d.kb_recent(Vec::new(), None, Of::default(), None, 0, Some(&work)).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert!(recent.iter().all(|i| i.profile.as_deref() != Some("home")));
    assert_eq!(
        d.kb_recent(Vec::new(), None, Of::default(), None, 0, Some(&own)).await.unwrap().len(),
        1
    );

    // Forgotten: found by nobody, by words or by meaning, and it doesn't open.
    let db = d.kb_db().unwrap();
    let item = db.item_by_uri("session://claude/work".into()).await.unwrap().unwrap();
    db.forget(item.id.clone()).await.unwrap();
    for q in ["cat", "kitten", "feeder", "cat kind:session", "cat by:meaning"] {
        let got = found(&d.kb_search(ask(q), None).await.unwrap());
        assert!(!got.iter().any(|u| u.starts_with("session:")), "{q}: {got:?}");
    }
    assert!(d.kb_read(&format!("k:{}", item.id), 0, 0, None).await.is_err());
    assert_eq!(d.kb_recent(Vec::new(), None, Of::default(), None, 0, None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn without_the_model_it_is_by_words_and_says_so() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t).await;
    // Off: by words, and the answer says why. `by:words` asked for it, so nothing is said.
    set_config(&d, "");
    let hits = d.kb_search(ask("cat"), None).await.unwrap();
    assert!(hits.words_only.as_deref().unwrap().contains("meaning search is off"), "{hits:?}");
    assert!(hits.hits.len() == 6 && hits.hits.iter().all(|h| h.how == "words"));
    assert!(d.kb_search(ask("kitten"), None).await.unwrap().hits.is_empty());
    assert_eq!(d.kb_search(ask("cat by:words"), None).await.unwrap().words_only, None);
    let e = d.kb_search(ask("cat by:meaning"), None).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    // Its old name switches it on too.
    set_config(&d, "[chat]\nmeaning = true\n");
    assert_eq!(d.kb_search(ask("kitten"), None).await.unwrap().hits.len(), 6);
    // On, and the model not here yet.
    std::fs::remove_dir_all(d.paths.data.join("models")).unwrap();
    let hits = d.kb_search(ask("cat"), None).await.unwrap();
    assert!(hits.words_only.as_deref().unwrap().contains("isn't here yet"), "{hits:?}");
    assert_eq!(hits.hits.len(), 6);
    assert_eq!(d.kb_search(ask("cat by:meaning"), None).await.unwrap_err().code, ErrorCode::Busy);
    // No words: the newest of what the filters let through, each item once.
    let newest = d.kb_search(ask("kind:note"), None).await.unwrap().hits;
    assert!(
        !newest.is_empty() && newest.iter().all(|h| h.how == "newest" && h.kind == HitKind::Note)
    );
    assert!(newest.iter().all(|h| h.addr.starts_with("k:")), "{newest:?}");
    let all = d.kb_search(ask("*"), None).await.unwrap().hits;
    assert_eq!(all.len(), d.kb_search(ask(""), None).await.unwrap().hits.len());
    assert!(all.windows(2).all(|w| w[0].at_ms >= w[1].at_ms), "newest first: {all:?}");
    // What a search can't be made of.
    for q in ["by:meaning", "-cat", "ca*", "cat kind:memo", "cat since:whenever"] {
        let e = d.kb_search(ask(q), None).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{q}: {e:?}");
    }
    // Chats off: the knowledge base alone. The knowledge base off: it says how to switch it on.
    set_config(&d, "[chat]\nenabled = false\n");
    let got = found(&d.kb_search(ask("cat"), None).await.unwrap());
    assert!(got.len() == 3 && !got.iter().any(|u| u.starts_with("chat:")), "{got:?}");
    set_config(&d, "[kb]\nenabled = false\n");
    let e = d.kb_search(ask("cat"), None).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(d.kb_read("k:abcd", 0, 0, None).await.is_err());
}

#[tokio::test]
async fn an_address_opens_with_its_neighbours_and_the_history_pages() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    d.start_kb();
    let parts: Vec<Part> =
        (0..10).map(|i| Part::new(ChunkKind::Text, format!("piece {i}"), 10 + i)).collect();
    for (uri, at) in [("file:///a.md", 100u64), ("file:///b.md", 200), ("file:///c.md", 300)] {
        let mut p = prepared(ItemKind::File, uri, uri, parts.clone(), stamp(Writer::File, None));
        p.source.ended_ms = Some(at);
        d.kb_ingest(p).await.unwrap();
    }
    let db = d.kb_db().unwrap();
    let a = db.item_by_uri("file:///a.md".into()).await.unwrap().unwrap();
    let texts = |doc: &KbDoc| doc.chunks.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
    // The chunk with its neighbours each side; at the item's start there are none before.
    let doc = d.kb_read(&format!("k:{}/5", a.id), 2, 1, None).await.unwrap();
    assert_eq!(texts(&doc), ["piece 3", "piece 4", "piece 5", "piece 6"]);
    assert_eq!((doc.at, doc.stale), (Some(5), false));
    assert_eq!(texts(&d.kb_read(&format!("K:{}/0", a.id), 2, 0, None).await.unwrap()), ["piece 0"]);
    // The item alone: from its start; all of it when asked for that much.
    let doc = d.kb_read(&a.id, 0, 2, None).await.unwrap();
    assert_eq!((texts(&doc).len(), doc.at), (3, None));
    assert_eq!(d.kb_read(&a.id, 0, u32::MAX, None).await.unwrap().chunks.len(), 10);
    // A chunk the item no longer has: its start, and it says so.
    let doc = d.kb_read(&format!("k:{}/40", a.id), 2, 1, None).await.unwrap();
    assert_eq!(
        (texts(&doc), doc.at, doc.stale),
        (vec!["piece 0".to_owned(), "piece 1".to_owned()], None, true)
    );
    // By its handle; what isn't an address says what one is.
    let handle = &a.id[a.id.len() - 6..];
    assert_eq!(d.kb_read(&format!("k:{handle}/1"), 0, 0, None).await.unwrap().item.id, a.id);
    assert_eq!(d.kb_read("k:zzzzzz", 0, 0, None).await.unwrap_err().code, ErrorCode::NotFound);
    assert!(
        d.kb_read("k:zzzzzz/x", 0, 0, None).await.unwrap_err().message.contains("piece's number")
    );

    // The history: newest first, a page at a time, by kind.
    let uris = |items: &[KbItem]| items.iter().map(|i| i.uri.clone()).collect::<Vec<_>>();
    let page = d.kb_recent(Vec::new(), None, Of::default(), None, 2, None).await.unwrap();
    assert_eq!(uris(&page), ["file:///c.md", "file:///b.md"]);
    let next =
        d.kb_recent(Vec::new(), None, Of::default(), page[1].ended_ms, 2, None).await.unwrap();
    assert_eq!(uris(&next), ["file:///a.md"]);
    assert!(
        d.kb_recent(vec![ItemKind::Session], None, Of::default(), None, 0, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        d.kb_recent(vec![ItemKind::File], None, Of::default(), None, 0, None).await.unwrap().len(),
        3
    );
}

/// A5: `kb.scope = "workspace"` for a caller whose workspace this machine doesn't know (it
/// was renamed or removed under the session) is a wall around nothing, never its whole
/// profile.
#[test]
fn a_workspace_scope_with_no_workspace_sees_nothing() {
    use ptyro_core::kb::KbItem;
    let item = |workspace: Option<&str>| {
        let mut i: KbItem = serde_json::from_value(serde_json::json!({
            "id": "i", "uri": "file:///a.md", "kind": "file", "writer": "file", "origin": "m",
            "chunks": 0,
        }))
        .unwrap();
        i.profile = Some("work".into());
        i.workspace = workspace.map(str::to_owned);
        i
    };
    let known = KbScope::of("workspace", "work", Some("w1"));
    assert!(known.sees(&item(Some("w1"))) && !known.sees(&item(Some("w2"))));
    let unknown = KbScope::of("workspace", "work", None);
    assert!(unknown.workspace.is_some(), "it is a wall: nothing is asked of other machines");
    for w in [Some("w1"), Some("w2"), None] {
        assert!(!unknown.sees(&item(w)), "{w:?}");
    }
    // The other settings never had a workspace.
    assert_eq!(KbScope::of("profile", "work", Some("w1")).workspace, None);
    assert_eq!(KbScope::of("all", "work", None), KbScope::default());
}
