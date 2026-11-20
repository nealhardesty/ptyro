use super::*;

fn line(kind: &str, uuid: &str, text: &str) -> String {
    let content = match kind {
        "user" => json!(text),
        _ => json!([{ "type": "text", "text": text }]),
    };
    format!("{}\n", json!({ "type": kind, "uuid": uuid, "message": { "content": content } }))
}

fn said(p: &Piece) -> Vec<&str> {
    p.transcript.parts.iter().map(|p| p.text.as_str()).collect()
}

#[test]
fn a_transcript_is_read_on_from_its_cursor_and_again_when_it_is_not_the_file_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c1.jsonl");
    let claude = ptyro_agents::adapter("claude");
    let read = |cursor: Option<&Value>| read(claude.as_ref(), &path, cursor, READ_MAX).unwrap();
    let turn_1 = line("user", "u1", "first") + &line("assistant", "a1", "one");
    std::fs::write(&path, &turn_1).unwrap();

    let first = read(None);
    assert_eq!(said(&first), ["first", "one"]);
    assert!(first.whole && !first.more);
    assert_eq!(first.cursor["offset"], turn_1.len());
    assert_eq!(first.cursor["path"], path.to_string_lossy().as_ref());
    assert!(!grew(&first.cursor));

    // It grows: only what is new is read, and a last line with no end yet waits.
    let turn_2 = line("user", "u2", "second") + &line("assistant", "a2", "two");
    let torn = &turn_2[..turn_2.len() - 1];
    std::fs::write(&path, format!("{turn_1}{torn}")).unwrap();
    assert!(grew(&first.cursor));
    let second = read(Some(&first.cursor));
    assert_eq!(said(&second), ["second"]);
    assert!(!second.whole);
    std::fs::write(&path, format!("{turn_1}{turn_2}")).unwrap();
    let third = read(Some(&second.cursor));
    assert_eq!(said(&third), ["two"]);
    assert_eq!(third.cursor["offset"], turn_1.len() + turn_2.len());
    assert!(said(&read(Some(&third.cursor))).is_empty());

    // Cut back (a rewind): shorter than the cursor, so all of it again.
    std::fs::write(&path, &turn_1).unwrap();
    assert!(grew(&third.cursor));
    let rewound = read(Some(&third.cursor));
    assert!(rewound.whole);
    assert_eq!(said(&rewound), ["first", "one"]);

    // Cut back and grown past the cursor again: the cursor no longer stands at a line's end.
    let other =
        line("user", "u3", "another way, said at some length") + &line("assistant", "a3", "three");
    std::fs::write(&path, format!("{turn_1}{other}")).unwrap();
    let regrown = read(Some(&third.cursor));
    assert!(regrown.whole);
    assert_eq!(said(&regrown), ["first", "one", "another way, said at some length", "three"]);

    // Another file in its place (the same bytes, another inode).
    let aside = dir.path().join("new");
    std::fs::copy(&path, &aside).unwrap();
    std::fs::rename(&aside, &path).unwrap();
    #[cfg(unix)]
    {
        assert!(grew(&regrown.cursor));
        assert!(read(Some(&regrown.cursor)).whole);
    }
    assert!(!grew(&json!({ "path": dir.path().join("gone"), "offset": 1 })), "nothing to read");
    assert!(super::read(claude.as_ref(), &dir.path().join("gone"), None, READ_MAX).is_none());
}

#[test]
fn a_long_file_is_read_in_several_reads_and_a_line_longer_than_a_read_is_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c1.jsonl");
    let claude = ptyro_agents::adapter("claude");
    let mut text = String::new();
    for n in 0..5 {
        text += &line("user", &format!("u{n}"), &format!("prompt {n}"));
    }
    text += &line("user", "big", &"x".repeat(2000));
    text += &line("assistant", "a", "the end");
    std::fs::write(&path, &text).unwrap();
    let (mut cursor, mut all, mut reads) = (None, Vec::new(), 0);
    loop {
        let p = read(claude.as_ref(), &path, cursor.as_ref(), 200).unwrap();
        all.extend(p.transcript.parts.iter().map(|p| p.text.clone()));
        reads += 1;
        assert!(reads < 50, "it never ends");
        let more = p.more;
        cursor = Some(p.cursor);
        if !more {
            break;
        }
    }
    assert_eq!(all, ["prompt 0", "prompt 1", "prompt 2", "prompt 3", "prompt 4", "the end"]);
    assert_eq!(cursor.unwrap()["offset"], text.len());

    // One still being written, longer than a read: nothing moves until it ends.
    let open = &text[..text.len() - line("assistant", "a", "the end").len() - 1];
    std::fs::write(&path, open).unwrap();
    let at =
        json!({ "path": path, "offset": open.len() - 2000 - line("user", "big", "").len() + 1 });
    let mut at = at;
    let m = std::fs::metadata(&path).unwrap();
    (at["dev"], at["ino"]) = (json!(file_id(&m).0), json!(file_id(&m).1));
    let p = read(claude.as_ref(), &path, Some(&at), 200).unwrap();
    assert_eq!((p.cursor["offset"].clone(), p.more), (at["offset"].clone(), false));
}

/// An item of `uri` as a read of `path` up to `upto` bytes left it: its turns, its cursor.
async fn kept_until(d: &Arc<Daemon>, uri: &str, path: &Path, upto: usize) {
    let claude = ptyro_agents::adapter("claude");
    let bytes = std::fs::read(path).unwrap();
    let transcript = claude.read_transcript(&bytes[..upto], 0).unwrap();
    let m = std::fs::metadata(path).unwrap();
    let mut source = Source::new(ItemKind::Session, uri, Body::Parts(transcript.parts));
    source.append = true;
    source.cursor =
        Some(json!({ "path": path, "dev": file_id(&m).0, "ino": file_id(&m).1, "offset": upto }));
    let stamp = Stamp::new(Writer::Agent, None);
    let facts = Facts { session: true, ..Facts::default() };
    d.kb_ingest(Prepared { source, stamp, facts }).await.unwrap();
}

async fn texts(d: &Arc<Daemon>, uri: &str) -> Vec<String> {
    let db = d.kb_db().unwrap();
    let item = db.item_by_uri(uri.into()).await.unwrap().unwrap();
    db.chunks(item.id, 0, 0).await.unwrap().into_iter().map(|c| c.text).collect()
}

/// E1: a conversation that was kept, then carried on by a private session, is closed. Once
/// that session is gone, the daemon's start finds the transcript grown past the item's
/// cursor: it must not read the private turns. E8: a conversation with more to read than
/// one read takes is read to its end, piece by piece.
#[tokio::test]
async fn the_catch_up_reads_a_long_transcript_to_its_end_and_never_a_closed_one() {
    let t = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), ptyro_store::Registry::default());
    d.start_kb();
    let first =
        line("user", "u1", "zqbefore asked") + &line("assistant", "a1", "zqbefore answered");
    // More than a read takes (`READ_MAX`, small in tests), so it is several pieces.
    let mut more = String::new();
    for n in 0..120 {
        more += &line(
            "user",
            &format!("u{n}x"),
            &format!("zqlater question {n} {}", "word ".repeat(40)),
        );
        more += &line("assistant", &format!("a{n}x"), &format!("zqlater answer {n}"));
    }
    assert!(more.len() as u64 > 3 * READ_MAX);
    let (open, closed) = (dir.path().join("open.jsonl"), dir.path().join("closed.jsonl"));
    for (uri, path) in [("session://claude/open", &open), ("session://claude/closed", &closed)] {
        std::fs::write(path, &first).unwrap();
        kept_until(&d, uri, path, first.len()).await;
        std::fs::write(path, format!("{first}{more}")).unwrap();
    }
    // (As `kb_keep_out` does when a private session is in that conversation.)
    let db = d.kb_db().unwrap();
    db.keep_out("session://claude/closed".into(), ItemKind::Session).await.unwrap();

    d.kb_catch_up().await;
    d.kb_idle().await;
    let all = texts(&d, "session://claude/open").await;
    assert!(all.iter().any(|t| t == "zqlater answer 119"), "read to its end: {}", all.len());
    assert_eq!(all.iter().filter(|t| t.starts_with("zqlater answer")).count(), 120);
    assert_eq!(
        texts(&d, "session://claude/closed").await,
        ["zqbefore asked", "zqbefore answered"],
        "what was kept stays, and nothing said in private is read"
    );
    // Asked for by name too (a queue rebuilt, a backfill): still nothing.
    assert!(matches!(
        d.kb_item("session://claude/closed").await,
        Err(Refused::Skip(ptyro_core::kb::ingest::Skip::Private))
    ));
}
