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
