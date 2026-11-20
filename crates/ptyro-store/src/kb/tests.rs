use proptest::prelude::*;
use ptyro_core::kb::RootOptions;
use ptyro_core::kb::ingest::{self, Body, Gate, Never, Source, Stamp};
use ptyro_core::kb::redact::Redactor;

use super::*;

fn stamp() -> Stamp {
    Stamp {
        project: Some("p1".into()),
        agent: Some("claude".into()),
        session: Some("s1".into()),
        ..Stamp::new(Writer::Agent, Some("work".into()))
    }
}

fn plan(source: Source) -> Plan {
    let never = Never::default();
    let passed = ingest::gate(&Gate::open(&never)).unwrap();
    ingest::plan(passed, source, stamp(), &Redactor::default()).unwrap()
}

fn doc(uri: &str, text: &str) -> Plan {
    let mut s = Source::new(
        ItemKind::File,
        uri,
        Body::Markdown { name: "doc.md".into(), text: text.into(), at_ms: 10 },
    );
    s.title = Some("A document".into());
    s.rel = Some("doc.md".into());
    plan(s)
}

fn turn(kind: ChunkKind, anchor: &str, text: &str, at: u64) -> Part {
    Part::new(kind, text, at).anchor(anchor)
}

fn piece(uri: &str, parts: Vec<Part>) -> Plan {
    let mut s = Source::new(ItemKind::Session, uri, Body::Parts(parts));
    s.append = true;
    plan(s)
}

async fn texts(db: &KbDb, item: &str) -> Vec<String> {
    db.chunks(item.to_owned(), 0, 0).await.unwrap().into_iter().map(|c| c.text).collect()
}

async fn found(db: &KbDb, word: &'static str) -> usize {
    db.call(move |c| {
        Ok(c.query_row("SELECT count(*) FROM chunk_fts WHERE chunk_fts MATCH ?1", [word], |r| {
            r.get::<_, i64>(0)
        })? as usize)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_new_file_is_private_in_wal_mode_and_keeps_its_machine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.db");
    let db = KbDb::open(&path, Some("machine-from-chat")).unwrap();
    assert_eq!(db.machine_id, "machine-from-chat");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        db.write(doc("file:///a.md", "# A\n\ntext")).await.unwrap();
        for f in ["kb.db", "kb.db-wal"] {
            let mode = std::fs::metadata(dir.path().join(f)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{f}");
        }
    }
    let d = db.doctor().await.unwrap();
    assert_eq!(d.version, MIGRATIONS.len() as i64);
    assert!(d.fts5 && d.secure_delete, "{d:?}");
    assert_eq!(d.integrity, "ok");
    let (mode, fk): (String, i64) = db
        .call(|c| {
            Ok((
                c.pragma_query_value(None, "journal_mode", |r| r.get(0))?,
                c.pragma_query_value(None, "foreign_keys", |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((mode.as_str(), fk), ("wal", 1));
    drop(db);
    // Opened without chat.db's id: its own is kept. With another: that one, from then on.
    assert_eq!(KbDb::open(&path, None).unwrap().machine_id, "machine-from-chat");
    assert_eq!(KbDb::open(&path, Some("m2")).unwrap().machine_id, "m2");
    assert_eq!(KbDb::open(&path, None).unwrap().machine_id, "m2");
}

#[test]
fn a_newer_schema_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.db");
    drop(KbDb::open(&path, None).unwrap());
    let newer = MIGRATIONS.len() as i64 + 3;
    {
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", newer).unwrap();
        c.execute_batch("CREATE TABLE from_the_future (x);").unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let Err(e) = KbDb::open(&path, None) else { panic!("it opened") };
    assert!(matches!(e, StoreError::TooNew { found, .. } if found == newer), "{e}");
    assert!(e.fix("kb.db").unwrap().contains("newer ptyro"));
    assert_eq!(std::fs::read(&path).unwrap(), before, "not a byte changed");
}

#[tokio::test]
async fn a_document_is_written_once_and_a_change_keeps_what_did_not_change() {
    let db = KbDb::open_in_memory().unwrap();
    let v1 = "# One\n\nalpha text\n\n# Two\n\nbravo text\n\n# Three\n\ncharlie text\n";
    let w = db.write(doc("file:///doc.md", v1)).await.unwrap();
    assert_eq!((w.outcome, w.added, w.kept, w.removed), (Outcome::New, 3, 0, 0));
    let item = db.item(w.item.clone()).await.unwrap().unwrap();
    assert_eq!(item.uri, "file:///doc.md");
    assert_eq!((item.kind, item.writer, item.chunks), (ItemKind::File, Writer::Agent, 3));
    assert_eq!((item.profile.as_deref(), item.project.as_deref()), (Some("work"), Some("p1")));
    assert_eq!(item.origin, db.machine_id);
    assert_eq!(db.item_by_uri("file:///doc.md".into()).await.unwrap().unwrap(), item);
    let first = db.chunks(w.item.clone(), 0, 0).await.unwrap();
    assert_eq!(first[1].heading.as_deref(), Some("doc.md › Two"));
    assert_eq!(first[1].anchor.as_deref(), Some("two"));
    assert_eq!((first[1].idx, first[1].kind, first[1].at_ms), (1, ChunkKind::Text, 10));
    // A range.
    let some = db.chunks(w.item.clone(), 1, 1).await.unwrap();
    assert_eq!(some.len(), 1);
    assert_eq!(some[0].text, "bravo text");

    // The same again: nothing is written, but a cursor that moved.
    let mut again = doc("file:///doc.md", v1);
    again.cursor = Some(serde_json::json!({"mtime": 2, "size": 60}));
    let w2 = db.write(again).await.unwrap();
    assert_eq!((w2.outcome, w2.item.as_str()), (Outcome::Unchanged, w.item.as_str()));
    let after = db.item(w.item.clone()).await.unwrap().unwrap();
    assert_eq!(after.cursor, Some(serde_json::json!({"mtime": 2, "size": 60})));
    assert_eq!(after.ingested_ms, item.ingested_ms);
    assert_eq!(db.chunks(w.item.clone(), 0, 0).await.unwrap(), first);

    // A section put in front, one changed, one gone: the unchanged rows stay (their `seq`,
    // which their vectors hang on), at their new places.
    let (one, three) = (first[0].seq, first[2].seq);
    db.call(move |c| {
        for seq in [one, three] {
            c.execute(
                "INSERT INTO chunk_vec (seq, item, kind, at_ms, model, vec) VALUES (?1, 'i', 'text', 0, 'm', x'00')",
                [seq],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let v2 = "# Zero\n\nzulu text\n\n# One\n\nalpha text\n\n# Three\n\ncharlie changed\n";
    let w3 = db.write(doc("file:///doc.md", v2)).await.unwrap();
    assert_eq!((w3.outcome, w3.added, w3.kept, w3.removed), (Outcome::Updated, 2, 1, 2));
    let now = db.chunks(w.item.clone(), 0, 0).await.unwrap();
    assert_eq!(
        now.iter().map(|c| (c.idx, c.text.as_str())).collect::<Vec<_>>(),
        [(0, "zulu text"), (1, "alpha text"), (2, "charlie changed")]
    );
    assert_eq!(now[1].seq, one);
    assert_eq!(db.item(w.item.clone()).await.unwrap().unwrap().chunks, 3);
    // The index follows, and a deleted chunk's vector went with it.
    assert_eq!((found(&db, "bravo").await, found(&db, "zulu").await), (0, 1));
    assert_eq!((found(&db, "charlie").await, found(&db, "three").await), (1, 1));
    let vecs: Vec<i64> = db
        .call(|c| {
            let mut st = c.prepare("SELECT seq FROM chunk_vec")?;
            Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(vecs, [one]);
}

#[tokio::test]
async fn a_transcript_grows_piece_by_piece_and_a_piece_twice_doubles_nothing() {
    let db = KbDb::open_in_memory().unwrap();
    let uri = "session://claude/c1";
    let card = |text: &str| turn(ChunkKind::Card, "card", text, 1);
    let mut first = piece(
        uri,
        vec![
            card("task: fix the test"),
            turn(ChunkKind::User, "u1", "fix the test", 1),
            turn(ChunkKind::Assistant, "a1", "Looking at it.", 2),
            turn(ChunkKind::Tool, "a1", "Read src/lib.rs", 2),
        ],
    );
    first.started_ms = Some(1);
    first.ended_ms = Some(2);
    first.cursor = Some(serde_json::json!({"offset": 100}));
    first.title = Some("fix the test".into());
    let w = db.write(first.clone()).await.unwrap();
    assert_eq!((w.outcome, w.added), (Outcome::New, 4));
    let id = w.item.clone();
    let rows = db.chunks(id.clone(), 0, 0).await.unwrap();

    // The same piece again (a retry, a re-read from the start after a rewind): nothing new.
    let w = db.write(first).await.unwrap();
    assert_eq!((w.outcome, w.added, w.changed, w.kept), (Outcome::Updated, 0, 0, 4));
    assert_eq!(db.chunks(id.clone(), 0, 0).await.unwrap(), rows);

    // The next piece: its turns follow, and the card it sends again is rewritten in place.
    let a1 = rows[2].seq;
    db.call(move |c| {
        for seq in [rows[0].seq, a1] {
            c.execute(
                "INSERT INTO chunk_vec (seq, item, kind, at_ms, model, vec) VALUES (?1, 'i', 'card', 0, 'm', x'00')",
                [seq],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let mut more = piece(
        uri,
        vec![
            card("task: fix the test\noutcome: fixed"),
            turn(ChunkKind::Assistant, "a2", "Fixed: the assertion was wrong.", 9),
        ],
    );
    more.ended_ms = Some(9);
    more.cursor = Some(serde_json::json!({"offset": 250}));
    let w = db.write(more).await.unwrap();
    assert_eq!((w.added, w.changed, w.kept, w.removed), (1, 1, 0, 0));
    assert_eq!(
        texts(&db, &id).await,
        [
            "task: fix the test\noutcome: fixed",
            "fix the test",
            "Looking at it.",
            "Read src/lib.rs",
            "Fixed: the assertion was wrong."
        ]
    );
    let item = db.item(id.clone()).await.unwrap().unwrap();
    assert_eq!((item.started_ms, item.ended_ms, item.chunks), (Some(1), Some(9), 5));
    assert_eq!(item.title.as_deref(), Some("fix the test"), "a piece without one keeps it");
    assert_eq!(item.cursor, Some(serde_json::json!({"offset": 250})));
    assert_eq!(item.hash, None);
    // The rewritten card lost its vector (it is embedded again); the untouched turn kept its.
    let vecs: Vec<i64> = db
        .call(|c| {
            let mut st = c.prepare("SELECT seq FROM chunk_vec")?;
            Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(vecs, [a1]);
    assert_eq!((found(&db, "outcome").await, found(&db, "assertion").await), (1, 1));

    // What the next card is made of: the first prompt, the last answer from its start (a
    // long one is several chunks), the user's messages counted once each.
    let facts = db.session_facts(id.clone()).await.unwrap();
    assert_eq!(facts.task.as_deref(), Some("fix the test"));
    assert_eq!(facts.outcome.as_deref(), Some("Fixed: the assertion was wrong."));
    assert_eq!(facts.turns, 1);
    let long = format!("Done. {}", "It works now. ".repeat(300));
    let w = db
        .write(piece(
            uri,
            vec![
                turn(ChunkKind::User, "u2", &"and the other test? ".repeat(200), 10),
                turn(ChunkKind::Assistant, "a3", &long, 11),
                turn(ChunkKind::Tool, "a3", "Bash: cargo test", 11),
            ],
        ))
        .await
        .unwrap();
    assert!(w.added > 5, "{w:?}");
    let facts = db.session_facts(id.clone()).await.unwrap();
    assert_eq!(facts.task.as_deref(), Some("fix the test"));
    assert!(facts.outcome.as_deref().unwrap().starts_with("Done. It works now."), "{facts:?}");
    assert_eq!(facts.turns, 2);
    assert_eq!(db.session_facts("nobody".into()).await.unwrap(), SessionFacts::default());

    // What a queue is rebuilt from.
    let cursors = db.cursors(ItemKind::Session).await.unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!((cursors[0].uri.as_str(), cursors[0].session.as_deref()), (uri, Some("s1")));
    assert_eq!(cursors[0].cursor, Some(serde_json::json!({"offset": 250})));
    db.set_cursor(uri.into(), Some(serde_json::json!({"offset": 300}))).await.unwrap();
    assert_eq!(
        db.cursors(ItemKind::Session).await.unwrap()[0].cursor,
        Some(serde_json::json!({"offset": 300}))
    );
    assert!(db.cursors(ItemKind::File).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_history_is_listed_newest_first_and_filtered() {
    let db = KbDb::open_in_memory().unwrap();
    for (n, at) in [(1, 100u64), (2, 300), (3, 200)] {
        let mut p =
            piece(&format!("session://claude/c{n}"), vec![turn(ChunkKind::User, "u", "hi", at)]);
        p.ended_ms = Some(at);
        if n == 3 {
            p.stamp.profile = Some("personal".into());
            p.stamp.project = Some("p2".into());
            p.stamp.session = Some("s3".into());
            p.stamp.worktree = Some("w3".into());
        }
        db.write(p).await.unwrap();
    }
    let mut shared = doc("file:///shared.md", "text");
    shared.stamp.profile = None;
    shared.ended_ms = Some(50);
    db.write(shared).await.unwrap();
    let uris = |items: Vec<KbItem>| items.into_iter().map(|i| i.uri).collect::<Vec<_>>();
    let all = db.list(ItemFilter::default()).await.unwrap();
    assert_eq!(
        uris(all),
        ["session://claude/c2", "session://claude/c3", "session://claude/c1", "file:///shared.md"]
    );
    let f = ItemFilter { kinds: vec![ItemKind::Session], limit: 2, ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c2", "session://claude/c3"]);
    // The next page.
    let f =
        ItemFilter { kinds: vec![ItemKind::Session], before_ms: Some(200), ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c1"]);
    // One profile sees its own and what belongs to nobody.
    let f = ItemFilter { profile: Some("personal".into()), ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c3", "file:///shared.md"]);
    let f = ItemFilter { project: Some("p2".into()), since_ms: Some(150), ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c3"]);
    let f = ItemFilter { session: Some("s3".into()), ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c3"]);
    let f = ItemFilter { worktree: Some("w3".into()), ..Default::default() };
    assert_eq!(uris(db.list(f).await.unwrap()), ["session://claude/c3"]);
    assert_eq!(db.ids().await.unwrap().len(), 4);

    let s = db.status().await.unwrap();
    assert_eq!(s.items, [(ItemKind::File, 1, 1), (ItemKind::Session, 3, 3)]);
    assert_eq!((s.vectors, s.roots, s.forgotten), (0, 0, 0));
}

/// Whether `needle` is in any of the knowledge base's files.
fn in_files(dir: &Path, needle: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            let bytes = std::fs::read(e.path()).unwrap_or_default();
            bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// SPEC §25.16: after `forget` and a checkpoint, the item's text is found nowhere in the bytes
/// of `kb.db`, its WAL or its index; and nothing brings it back.
#[tokio::test]
async fn forgetting_leaves_no_byte_of_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kb.db");
    let db = KbDb::open(&path, None).unwrap();
    // Other items around it, so that its pages are not simply the file's last.
    for n in 0..30 {
        let text =
            format!("# Section\n\n{}", format!("ordinary filler number{n} words. ").repeat(60));
        db.write(doc(&format!("file:///other{n}.md"), &text)).await.unwrap();
    }
    let uri = "session://claude/secret";
    // A long session: several batches of deletes. Each kind of place a word can be.
    let mut turns = vec![
        turn(ChunkKind::Card, "card", "task: zqcardxyzzy first", 1),
        turn(ChunkKind::User, "u1", "the zqtextxyzzy plan, said twice: zqtextxyzzy", 1)
            .heading("zqheadxyzzy heading"),
    ];
    for n in 0..(DELETE_BATCH + 50) {
        turns.push(turn(
            ChunkKind::Assistant,
            &format!("a{n}"),
            &format!("step {n} of zqmanyxyzzy and more words"),
            2,
        ));
    }
    let mut p = piece(uri, turns);
    p.title = Some("zqtitlexyzzy".into());
    p.files = vec!["src/zqfilexyzzy.rs".into()];
    p.cursor = Some(serde_json::json!({"offset": 9}));
    let id = db.write(p).await.unwrap().item;
    // A chunk rewritten in place: its old text must go too.
    db.write(piece(uri, vec![turn(ChunkKind::Card, "card", "task: zqnewcardxyzzy second", 3)]))
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    const WORDS: [&str; 7] = [
        "zqcardxyzzy",
        "zqnewcardxyzzy",
        "zqtextxyzzy",
        "zqheadxyzzy",
        "zqmanyxyzzy",
        "zqtitlexyzzy",
        "zqfilexyzzy",
    ];
    // (The test can see them: all but the card's first text, already rewritten and zeroed.)
    for w in &WORDS[1..] {
        assert!(!in_files(dir.path(), w).is_empty(), "{w} was never stored");
    }
    assert!(
        in_files(dir.path(), WORDS[0]).is_empty(),
        "the rewritten card's old text is still there"
    );
    // A copy set aside before a migration holds the text too.
    std::fs::copy(&path, dir.path().join("kb.db.bak-v0")).unwrap();

    assert_eq!(db.forget(id.clone()).await.unwrap(), Some(DELETE_BATCH + 52));
    for w in WORDS {
        assert_eq!(in_files(dir.path(), w), Vec::<String>::new(), "{w} is still on disk");
        assert_eq!(found(&db, w).await, 0);
    }
    assert!(!dir.path().join("kb.db.bak-v0").exists());
    assert_eq!(db.doctor().await.unwrap().integrity, "ok");
    assert!(found(&db, "filler").await > 0, "the others are untouched");

    // The tombstone: the URI and when, no text.
    let tomb = db.item(id.clone()).await.unwrap().unwrap();
    assert!(tomb.forgotten_ms.is_some());
    assert_eq!((tomb.uri.as_str(), tomb.chunks), (uri, 0));
    assert_eq!((tomb.title, tomb.hash, tomb.cursor), (None, None, None));
    assert!(tomb.files.is_empty());
    assert!(db.tombstoned(uri.into()).await.unwrap());
    assert!(!db.tombstoned("file:///other1.md".into()).await.unwrap());
    // It is in no list, has no cursor to carry on from, and a later ingest writes nothing.
    assert!(db.list(ItemFilter::default()).await.unwrap().iter().all(|i| i.id != id));
    assert!(!db.ids().await.unwrap().contains(&id));
    assert!(db.cursors(ItemKind::Session).await.unwrap().is_empty());
    let w = db
        .write(piece(uri, vec![turn(ChunkKind::User, "u9", "zqtextxyzzy again", 5)]))
        .await
        .unwrap();
    assert_eq!((w.outcome, w.item.as_str()), (Outcome::Forgotten, id.as_str()));
    let mut whole = doc(uri, "zqtextxyzzy as a document");
    whole.cursor = Some(serde_json::json!({"x": 1}));
    assert_eq!(db.write(whole).await.unwrap().outcome, Outcome::Forgotten);
    db.set_cursor(uri.into(), Some(serde_json::json!({"offset": 1}))).await.unwrap();
    db.checkpoint().await.unwrap();
    assert!(in_files(dir.path(), "zqtextxyzzy").is_empty());
    assert_eq!(db.item(id.clone()).await.unwrap().unwrap().cursor, None);
    assert_eq!(db.status().await.unwrap().forgotten, 1);
    // A tombstone for a URI nothing was kept of: it is forgotten from then on, counted, in
    // no list, and one that has an item is left alone.
    let never = "session://claude/never-read";
    assert!(db.tombstone(never.into(), ItemKind::Session).await.unwrap());
    assert!(!db.tombstone(never.into(), ItemKind::Session).await.unwrap());
    assert!(db.tombstoned(never.into()).await.unwrap());
    assert!(!db.tombstone("file:///other1.md".into(), ItemKind::File).await.unwrap());
    assert!(!db.tombstoned("file:///other1.md".into()).await.unwrap());
    let stone = db.item_by_uri(never.into()).await.unwrap().unwrap();
    assert!(stone.forgotten_ms.is_some() && stone.chunks == 0);
    assert!(db.list(ItemFilter::default()).await.unwrap().iter().all(|i| i.uri != never));
    assert_eq!(db.status().await.unwrap().forgotten, 2);
    // Forgetting it again, or what isn't there.
    assert_eq!(db.forget(id.clone()).await.unwrap(), Some(0));
    assert_eq!(db.item(id).await.unwrap().unwrap().forgotten_ms, tomb.forgotten_ms);
    assert_eq!(db.forget("nothing".into()).await.unwrap(), None);
}

/// A changed file's old text is deleted as surely as a forgotten item's.
#[tokio::test]
async fn a_replaced_chunk_leaves_no_byte_either() {
    let dir = tempfile::tempdir().unwrap();
    let db = KbDb::open(&dir.path().join("kb.db"), None).unwrap();
    db.write(doc("file:///a.md", "# H\n\nthe zqoldxyzzy text\n\n# Keep\n\nkept")).await.unwrap();
    db.checkpoint().await.unwrap();
    assert!(!in_files(dir.path(), "zqoldxyzzy").is_empty());
    db.write(doc("file:///a.md", "# H\n\nthe new text\n\n# Keep\n\nkept")).await.unwrap();
    db.checkpoint().await.unwrap();
    assert!(in_files(dir.path(), "zqoldxyzzy").is_empty());
}

fn root(id: &str, name: &str, path: &str) -> KbRoot {
    KbRoot {
        id: id.into(),
        name: name.into(),
        kind: RootKind::Project,
        path: path.into(),
        project: Some("p1".into()),
        profile: None,
        options: RootOptions {
            exclude: vec!["*.gen.rs".into()],
            code: Some(true),
            ..Default::default()
        },
        head: None,
        state: RootState::Syncing,
        error: None,
        added_ms: Some(5),
        synced_ms: None,
    }
}

#[tokio::test]
async fn roots_are_kept_and_removed_with_their_items_and_no_tombstones() {
    let db = KbDb::open_in_memory().unwrap();
    let r = root("r1", "ptyro", "/src/ptyro");
    db.put_root(r.clone()).await.unwrap();
    db.put_root(root("r2", "other", "/src/other")).await.unwrap();
    assert_eq!(
        db.roots().await.unwrap().iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["other", "ptyro"]
    );
    for which in ["r1", "ptyro", "/src/ptyro"] {
        assert_eq!(db.root(which.into()).await.unwrap().as_ref(), Some(&r), "{which}");
    }
    assert_eq!(db.root("nope".into()).await.unwrap(), None);
    // A name or a path can't be taken twice.
    assert!(db.put_root(root("r3", "ptyro", "/elsewhere")).await.is_err());
    assert!(db.put_root(root("r3", "third", "/src/ptyro")).await.is_err());
    // Renamed, by its id.
    db.put_root(KbRoot { name: "ptyro-main".into(), ..r.clone() }).await.unwrap();
    db.set_root_state("r1".into(), RootState::Ok, None, Some("abc123".into())).await.unwrap();
    let got = db.root("r1".into()).await.unwrap().unwrap();
    assert_eq!(
        (got.name.as_str(), got.state, got.head.as_deref()),
        ("ptyro-main", RootState::Ok, Some("abc123"))
    );
    assert!(got.synced_ms.is_some());
    db.set_root_state("r1".into(), RootState::Error, Some("unreadable".into()), None)
        .await
        .unwrap();
    let got = db.root("r1".into()).await.unwrap().unwrap();
    assert_eq!(
        (got.state, got.error.as_deref(), got.head.as_deref()),
        (RootState::Error, Some("unreadable"), Some("abc123"))
    );

    // Files under each root, more than a batch of chunks in one.
    let many =
        (0..DELETE_BATCH + 10).map(|n| format!("# S{n}\n\ntext rootword{n}\n")).collect::<String>();
    for (uri, root, text) in [
        ("file:///src/ptyro/a.md", "r1", many.as_str()),
        ("file:///src/ptyro/b.md", "r1", "# B\n\nbtext"),
        ("file:///src/other/c.md", "r2", "# C\n\nctext"),
    ] {
        let mut p = doc(uri, text);
        p.stamp.root = Some(root.into());
        db.write(p).await.unwrap();
    }
    let f = ItemFilter { root: Some("r1".into()), ..Default::default() };
    assert_eq!(db.list(f).await.unwrap().len(), 2);
    assert_eq!(db.status().await.unwrap().roots, 2);
    // What a pass over a root asks: its items, and how much each root holds.
    let mut of: Vec<_> = db.root_items("r1".into()).await.unwrap();
    of.sort_by(|a, b| a.uri.cmp(&b.uri));
    assert_eq!(
        of.iter().map(|i| (i.uri.as_str(), i.kind)).collect::<Vec<_>>(),
        [("file:///src/ptyro/a.md", ItemKind::File), ("file:///src/ptyro/b.md", ItemKind::File)]
    );
    let mut counts = db.root_counts().await.unwrap();
    counts.sort();
    assert_eq!(
        counts,
        [
            ("r1".to_owned(), ItemKind::File, 2, DELETE_BATCH as u64 + 11),
            ("r2".to_owned(), ItemKind::File, 1, 1)
        ]
    );
    let uris = db.uris(ItemKind::File).await.unwrap();
    assert_eq!((uris.len(), uris.iter().any(|(_, forgotten)| *forgotten)), (3, false));
    let mut said = Vec::new();
    let gone = db.remove_root("r1".into(), |n| said.push(n)).await.unwrap();
    assert_eq!(gone, DELETE_BATCH + 11);
    assert_eq!(said, [DELETE_BATCH, DELETE_BATCH + 11]);
    assert_eq!(db.roots().await.unwrap().len(), 1);
    assert_eq!(db.list(ItemFilter::default()).await.unwrap().len(), 1);
    assert_eq!(
        (found(&db, "btext").await, found(&db, "rootword3").await, found(&db, "ctext").await),
        (0, 0, 1)
    );
    // No tombstone: ingest it again and it is back.
    assert!(!db.tombstoned("file:///src/ptyro/b.md".into()).await.unwrap());
    assert_eq!(
        db.write(doc("file:///src/ptyro/b.md", "# B\n\nbtext")).await.unwrap().outcome,
        Outcome::New
    );
}

#[tokio::test]
async fn retention_deletes_by_age_and_kind() {
    let db = KbDb::open_in_memory().unwrap();
    for (uri, kind, at) in [
        ("session://claude/old", ItemKind::Session, 100u64),
        ("session://claude/new", ItemKind::Session, 900),
        ("shell://s1/2026-01-01", ItemKind::Shell, 100),
        ("note://work/_global/old", ItemKind::Note, 100),
    ] {
        let mut s = Source::new(
            kind,
            uri,
            Body::Parts(vec![
                turn(ChunkKind::Command, "c1", "$ make test", at),
                turn(ChunkKind::Output, "c1", "all passed oldoutput", at),
                turn(ChunkKind::Output, "c2", "all passed newoutput", at + 500),
            ]),
        );
        s.append = true;
        s.ended_ms = Some(at);
        db.write(plan(s)).await.unwrap();
    }
    // Sessions and shells older than 500; a note is never aged out.
    let n = db.expire(vec![ItemKind::Session, ItemKind::Shell], 500).await.unwrap();
    assert_eq!(n, 2);
    let left: Vec<String> =
        db.list(ItemFilter::default()).await.unwrap().into_iter().map(|i| i.uri).collect();
    assert_eq!(left, ["session://claude/new", "note://work/_global/old"]);
    assert!(!db.tombstoned("session://claude/old".into()).await.unwrap());
    assert_eq!(db.expire(Vec::new(), u64::MAX).await.unwrap(), 0);
    // Output older than 500, whatever its item: the commands stay.
    assert_eq!(db.expire_chunks(ChunkKind::Output, 500).await.unwrap(), 1);
    // (The newer session's are all after it; the note's first one went.)
    assert_eq!((found(&db, "oldoutput").await, found(&db, "newoutput").await), (1, 2));
    let note = db.item_by_uri("note://work/_global/old".into()).await.unwrap().unwrap();
    assert_eq!(note.chunks, 2);
    assert_eq!(found(&db, "make").await, 2);
    // One item, with no tombstone.
    assert!(db.delete_item(note.id.clone()).await.unwrap());
    assert!(!db.delete_item(note.id).await.unwrap());
    assert_eq!(db.list(ItemFilter::default()).await.unwrap().len(), 1);
}

// ── Idempotence (SPEC §25.16) ───────────────────────────────────────────────────────────────

type Dump = Vec<Vec<Option<String>>>;

/// Every row a source left, but what differs by design: ids, row numbers, when it was written.
fn rows(db: &KbDb) -> (Dump, Dump, i64) {
    db.call_blocking(|c| {
        let dump = |sql: &str| -> Result<Dump> {
            let mut st = c.prepare(sql)?;
            let n = st.column_count();
            let rows = st
                .query_map([], |r| {
                    (0..n)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i).map(|v| match v {
                                rusqlite::types::Value::Null => None,
                                rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                                rusqlite::types::Value::Text(t) => Some(t),
                                other => Some(format!("{other:?}")),
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        };
        Ok((
            dump(
                "SELECT uri, kind, root, title, rel, project, workspace, worktree, branch, profile,
                        agent, model, session, writer, files, hash, cursor, chunks, started_at,
                        ended_at, forgotten_at FROM item ORDER BY uri",
            )?,
            dump(
                "SELECT i.uri, c.idx, c.kind, c.anchor, c.heading, c.text, c.meta, c.at_ms
                 FROM chunk c JOIN item i ON i.id = c.item ORDER BY i.uri, c.idx",
            )?,
            // The words' index agrees with the rows.
            c.query_row(
                "SELECT count(*) FROM chunk_fts WHERE chunk_fts MATCH 'the OR a OR lorem OR w*'",
                [],
                |r| r.get(0),
            )?,
        ))
    })
    .unwrap()
}

/// Text of every length that matters: empty, a line, paragraphs past the limit, a secret.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-z ]{1,40}",
        "(the [a-z]{1,8}[.?!\n ]{1,2}){1,400}",
        "(w[a-z0-9]{0,6} ){0,30}(\n\n|# [A-Z][a-z]{1,6}\n|export API_TOKEN=abc123def456\n)?(lorem [a-z]{1,9}\\. ){0,120}",
    ]
}

/// A transcript: turns in order, each one anchor with one to three parts.
fn turns() -> impl Strategy<Value = Vec<Vec<Part>>> {
    let part = (0usize..4, text(), 0u64..1000);
    prop::collection::vec(prop::collection::vec(part, 1..4), 0..12).prop_map(|turns| {
        turns
            .into_iter()
            .enumerate()
            .map(|(n, parts)| {
                parts
                    .into_iter()
                    .map(|(kind, text, at)| {
                        let kind = [
                            ChunkKind::User,
                            ChunkKind::Assistant,
                            ChunkKind::Tool,
                            ChunkKind::Summary,
                        ][kind];
                        Part::new(kind, text, at).anchor(format!("uuid-{n}"))
                    })
                    .collect()
            })
            .collect()
    })
}

fn session(uri: &str, turns: &[Vec<Part>], upto: usize) -> Plan {
    let mut s = Source::new(ItemKind::Session, uri, Body::Parts(turns.concat()));
    s.append = true;
    s.title = Some("a session".into());
    s.started_ms = Some(1);
    s.ended_ms = Some(upto as u64);
    s.cursor = Some(serde_json::json!({ "offset": upto }));
    plan(s)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// A transcript ingested in pieces at any cut points between its turns, each piece
    /// possibly read twice, gives the same rows as ingested once, and as ingested once twice.
    #[test]
    fn a_transcript_in_pieces_is_the_transcript_at_once(
        turns in turns(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..5),
        twice in prop::collection::vec(any::<bool>(), 6),
    ) {
        let uri = "session://claude/prop";
        let once = KbDb::open_in_memory().unwrap();
        once.call_blocking({
            let p = session(uri, &turns, turns.len());
            let origin = once.machine_id.clone();
            move |c| write(c, &p, &origin)
        }).unwrap();
        let expected = rows(&once);

        let again = session(uri, &turns, turns.len());
        let origin = once.machine_id.clone();
        let w = once.call_blocking(move |c| write(c, &again, &origin)).unwrap();
        prop_assert_eq!((w.added, w.changed, w.removed), (0, 0, 0));
        prop_assert_eq!(&rows(&once), &expected);

        let mut at: Vec<usize> = cuts.iter().map(|i| i.index(turns.len() + 1)).collect();
        at.extend([0, turns.len()]);
        at.sort_unstable();
        at.dedup();
        let pieces = KbDb::open_in_memory().unwrap();
        for (n, pair) in at.windows(2).enumerate() {
            for _ in 0..=usize::from(twice[n % twice.len()]) {
                let p = session(uri, &turns[pair[0]..pair[1]], pair[1]);
                let origin = pieces.machine_id.clone();
                pieces.call_blocking(move |c| write(c, &p, &origin)).unwrap();
            }
        }
        if at.len() < 2 {
            let p = session(uri, &turns, turns.len());
            let origin = pieces.machine_id.clone();
            pieces.call_blocking(move |c| write(c, &p, &origin)).unwrap();
        }
        prop_assert_eq!(&rows(&pieces), &expected);
    }

    /// A document ingested twice is the document once; one that changed is, row for row, what
    /// a new file given only the second text holds.
    #[test]
    fn a_document_again_is_the_document_and_a_change_is_the_new_document(
        a in prop::collection::vec(text(), 0..8),
        b in prop::collection::vec(text(), 0..8),
    ) {
        let uri = "file:///prop.md";
        // (The second shares the first's opening sections, as an edited file does.)
        let b: Vec<String> = a.iter().take(a.len() / 2).chain(&b).chain(a.last()).cloned().collect();
        let (a, b) = (a.join("\n\n# Next\n\n"), b.join("\n\n# Next\n\n"));
        let write_doc = |db: &KbDb, text: &str| {
            let p = doc(uri, text);
            let origin = db.machine_id.clone();
            db.call_blocking(move |c| write(c, &p, &origin)).unwrap()
        };
        let db = KbDb::open_in_memory().unwrap();
        write_doc(&db, &a);
        let first = rows(&db);
        prop_assert_eq!(write_doc(&db, &a).outcome, Outcome::Unchanged);
        prop_assert_eq!(&rows(&db), &first);

        let changed = write_doc(&db, &b);
        let fresh = KbDb::open_in_memory().unwrap();
        write_doc(&fresh, &b);
        prop_assert_eq!(&rows(&db), &rows(&fresh));
        // And back again; what the two texts share was never rewritten.
        prop_assert!(a != b || changed.outcome == Outcome::Unchanged);
        write_doc(&db, &a);
        prop_assert_eq!(&rows(&db), &first);
    }
}
