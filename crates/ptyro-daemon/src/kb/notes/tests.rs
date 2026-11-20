use ptyro_core::ErrorCode;
use ptyro_core::kb::search::KbListed;
use ptyro_core::kb::{ItemKind, Writer};
use ptyro_protocol::{Reply, Request};

use super::super::requests::tests::{ask, daemon, search, session};
use super::super::search::KbScope;
use super::*;

fn note(text: &str) -> Request {
    note_in(text, None, None)
}

fn note_in(text: &str, project: Option<&str>, profile: Option<&str>) -> Request {
    Request::KbNote {
        text: text.into(),
        title: None,
        project: project.map(str::to_owned),
        profile: profile.map(str::to_owned),
        tags: Vec::new(),
        replaces: None,
        addr: None,
    }
}

async fn noted(d: &Arc<Daemon>, req: Request) -> (KbListed, PathBuf) {
    match ask(d, req).await.unwrap() {
        Reply::KbNoted { item, path } => (item, PathBuf::from(path)),
        r => panic!("{r:?}"),
    }
}

async fn notes(d: &Arc<Daemon>, project: Option<&str>, writer: Option<Writer>) -> Vec<String> {
    let req = Request::KbNotes { project: project.map(str::to_owned), writer };
    match ask(d, req).await.unwrap() {
        Reply::KbItems { items } => items.into_iter().map(|i| i.item.uri).collect(),
        r => panic!("{r:?}"),
    }
}

async fn first_hit(d: &Arc<Daemon>, query: &str) -> Option<String> {
    match ask(d, search(query)).await.unwrap() {
        Reply::KbHits { hits, .. } => hits.into_iter().next().and_then(|h| h.uri),
        r => panic!("{r:?}"),
    }
}

/// A file's time moves on even where the clock is coarse.
fn touch_later(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(later).unwrap();
}

#[tokio::test]
async fn a_note_is_a_file_in_its_folder_and_is_found_before_a_session() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let now = now_ms();
    d.kb_ingest(session("session://claude/c1", "the hopper jams when the belt slips", now))
        .await
        .unwrap();

    // Yours, about a project, for one profile's agents, with tags.
    let req = Request::KbNote {
        text: "The hopper jams because the belt slips: tighten it first.".into(),
        title: Some("Hopper jams".into()),
        project: Some("feeder".into()),
        profile: Some("work".into()),
        tags: vec!["gotcha".into(), " #two words ".into()],
        replaces: None,
        addr: None,
    };
    let (item, path) = noted(&d, req).await;
    assert_eq!(path, d.kb_notes_dir().join("work/feeder/hopper-jams.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nby: you\ntags: [gotcha, two-words]\ncreated: 20"), "{text}");
    assert!(
        text.ends_with(
            "---\n\n# Hopper jams\n\nThe hopper jams because the belt slips: tighten it first.\n"
        ),
        "{text}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&path), mode(path.parent().unwrap())), (0o600, 0o700));
    }
    let i = &item.item;
    assert_eq!((i.kind, i.uri.as_str()), (ItemKind::Note, "note://work/feeder/hopper-jams"));
    assert_eq!((i.title.as_deref(), i.writer), (Some("Hopper jams"), Writer::User));
    assert_eq!((i.profile.as_deref(), i.project.as_deref()), (Some("work"), Some("p1")));
    assert_eq!((item.project_name.as_deref(), i.agent.as_deref()), (Some("feeder"), None));
    assert!(item.addr.starts_with("k:"), "{}", item.addr);

    // What someone wrote ranks first; its tags are found, its header is not text.
    assert_eq!(first_hit(&d, "hopper jams belt").await.as_deref(), Some(i.uri.as_str()));
    assert_eq!(first_hit(&d, "gotcha").await.as_deref(), Some(i.uri.as_str()));
    assert_eq!(first_hit(&d, "created").await, None);
    let read =
        Request::KbRead { addr: item.addr.clone(), before: None, after: None, profile: None };
    let Reply::KbDoc { chunks, .. } = ask(&d, read).await.unwrap() else { panic!() };
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading.as_deref(), Some("Hopper jams"));
    assert!(chunks[0].text.ends_with("tighten it first.\n\ntags: gotcha, two-words"));

    // A second one of the same title gets a name of its own; one about nothing is global
    // and everyone's.
    let (again, _) =
        noted(&d, note_in("# Hopper jams\n\nagain", Some("feeder"), Some("work"))).await;
    assert_eq!(again.item.uri, "note://work/feeder/hopper-jams-2");
    let (global, path) = noted(&d, note("we deploy with make ship")).await;
    assert_eq!(global.item.uri, "note://_all/_global/we-deploy-with-make-ship");
    assert_eq!((global.item.profile.as_deref(), global.item.project.as_deref()), (None, None));
    assert!(path.ends_with("_all/_global/we-deploy-with-make-ship.md"));

    // The list: all, a project's, the ones about none, by who wrote them.
    assert_eq!(notes(&d, None, None).await.len(), 3);
    assert_eq!(notes(&d, Some("feeder"), None).await.len(), 2);
    assert_eq!(notes(&d, Some("_global"), None).await, std::slice::from_ref(&global.item.uri));
    assert_eq!(notes(&d, None, Some(Writer::User)).await.len(), 3);
    assert!(notes(&d, None, Some(Writer::Agent)).await.is_empty());

    // What can't be: no text, a profile or a project that isn't.
    for (req, said) in [
        (note("  "), "needs its text"),
        (note_in("x", None, Some("nobody")), "nobody"),
        (note_in("x", Some("nothing"), None), "nothing"),
    ] {
        let e = ask(&d, req).await.unwrap_err();
        assert!(e.message.contains(said), "{}", e.message);
    }
    // A status names the folder.
    let Reply::KbStatus { status } = ask(&d, Request::KbStatus).await.unwrap() else { panic!() };
    assert_eq!(status.notes, d.kb_notes_dir().display().to_string());
}

#[tokio::test]
async fn what_is_written_changed_or_deleted_by_hand_is_found() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    // A file with no header, made by an editor: yours, about the project its folder names.
    let dir = d.kb_notes_dir().join("work").join("feeder");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Belt tension.md");
    std::fs::write(&path, "# Belt tension\n\nForty newtons, measured cold.\n").unwrap();
    // One an agent wrote, as its header says; one under `_old/`, which is not a note.
    std::fs::write(
        dir.join("motor.md"),
        "---\nby: codex\nsession: 01jb0000000000000000000s01\ntags: [motor]\n---\nThe motor is a NEMA 23.\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("_old")).unwrap();
    std::fs::write(dir.join("_old").join("gone.md"), "superseded zqoldword").unwrap();
    std::fs::write(dir.join(".hidden.md"), "zqhiddenword").unwrap();
    std::fs::write(dir.join("notes.txt"), "zqtxtword").unwrap();

    assert_eq!(d.kb_notes_sync().await.unwrap(), (2, 2, 0));
    assert_eq!(d.kb_notes_sync().await.unwrap(), (2, 0, 0), "nothing changed: nothing read");
    let db = d.kb_db().unwrap();
    let belt = db.item_by_uri("note://work/feeder/Belt tension".into()).await.unwrap().unwrap();
    assert_eq!((belt.writer, belt.title.as_deref()), (Writer::User, Some("Belt tension")));
    assert_eq!((belt.profile.as_deref(), belt.project.as_deref()), (Some("work"), Some("p1")));
    let motor = db.item_by_uri("note://work/feeder/motor".into()).await.unwrap().unwrap();
    assert_eq!((motor.writer, motor.agent.as_deref()), (Writer::Agent, Some("codex")));
    assert_eq!(motor.session.as_deref(), Some("01jb0000000000000000000s01"));
    assert_eq!(first_hit(&d, "forty newtons").await.as_deref(), Some(belt.uri.as_str()));
    for never in ["zqoldword", "zqhiddenword", "zqtxtword"] {
        assert_eq!(first_hit(&d, never).await, None, "{never}");
    }
    assert_eq!(notes(&d, None, Some(Writer::Agent)).await, std::slice::from_ref(&motor.uri));

    // Edited in an editor: the new words are found, the old ones are not, the item is the same.
    touch_later(&path, "# Belt tension\n\nFifty-five newtons, measured warm.\n");
    assert_eq!(d.kb_notes_sync().await.unwrap(), (2, 1, 0));
    assert_eq!(first_hit(&d, "fifty-five newtons warm").await.as_deref(), Some(belt.uri.as_str()));
    assert_eq!(first_hit(&d, "forty cold").await, None);
    let after = db.item_by_uri(belt.uri.clone()).await.unwrap().unwrap();
    assert_eq!((after.id.as_str(), after.started_ms), (belt.id.as_str(), belt.started_ms));

    // Deleted by hand: its item goes, and no tombstone keeps the name from being used again.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(d.kb_notes_sync().await.unwrap(), (1, 0, 1));
    assert!(db.item_by_uri(belt.uri.clone()).await.unwrap().is_none());
    assert_eq!(first_hit(&d, "newtons").await, None);
    std::fs::write(&path, "# Belt tension\n\nback again\n").unwrap();
    assert_eq!(d.kb_notes_sync().await.unwrap(), (2, 1, 0));

    // `KbSync` is the same reading, asked for.
    let Reply::KbSynced { notes: n, changed, removed } = ask(&d, Request::KbSync).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((n, changed, removed), (2, 0, 0));
}

#[tokio::test]
async fn a_note_is_read_edited_replaced_and_forgotten_with_its_file() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let (item, path) = noted(&d, note("# Retry policy\n\nThree tries, then give up.")).await;

    // Its file as it is, for an editor.
    let read = |addr: &str| Request::KbNoteRead { addr: addr.into() };
    let Reply::KbNoteText { text, path: p, item: same } = ask(&d, read(&item.addr)).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (p.as_str(), same.item.id.as_str()),
        (path.to_str().unwrap(), item.item.id.as_str())
    );
    assert!(text.contains("by: you") && text.ends_with("Three tries, then give up.\n"));

    // Written again whole: the header that was sent is the header.
    let edited = text.replace("Three tries", "Five tries");
    let (after, _) = noted(
        &d,
        Request::KbNote {
            addr: Some(item.addr.clone()),
            text: edited.clone(),
            title: None,
            project: None,
            profile: None,
            tags: Vec::new(),
            replaces: None,
        },
    )
    .await;
    assert_eq!(after.item.id, item.item.id);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    assert_eq!(first_hit(&d, "five tries").await.as_deref(), Some(item.item.uri.as_str()));
    assert_eq!(first_hit(&d, "three tries").await, None);

    // Replaced: the new one says what it replaces; the old file is kept aside, out of search.
    let (new, new_path) = noted(
        &d,
        Request::KbNote {
            text: "# Retry policy\n\nSeven tries with backoff.".into(),
            replaces: Some(item.addr.clone()),
            title: None,
            project: None,
            profile: None,
            tags: Vec::new(),
            addr: None,
        },
    )
    .await;
    assert_eq!(new.item.uri, "note://_all/_global/retry-policy-2");
    let text = std::fs::read_to_string(&new_path).unwrap();
    assert!(text.contains("replaces: note://_all/_global/retry-policy\n"), "{text}");
    assert!(!path.exists());
    let aside = path.parent().unwrap().join("_old").join("retry-policy.md");
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), edited);
    assert_eq!(first_hit(&d, "five tries").await, None);
    assert_eq!(notes(&d, None, None).await, std::slice::from_ref(&new.item.uri));
    assert_eq!(d.kb_notes_sync().await.unwrap(), (1, 0, 0), "what is aside is not a note");
    // Only a note is read as one, or replaced by one.
    d.kb_ingest(session("session://claude/c1", "the hopper", now_ms())).await.unwrap();
    let db = d.kb_db().unwrap();
    let s = db.item_by_uri("session://claude/c1".into()).await.unwrap().unwrap();
    assert!(ask(&d, read(&s.id)).await.unwrap_err().message.contains("not a note"));

    // Forgotten: the question says the file goes; then it has, and the item with it.
    let forget = |confirmed| Request::KbForget {
        addrs: vec![new.addr.clone()],
        query: None,
        confirmed,
        session: None,
    };
    let e = ask(&d, forget(false)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(
        e.message.starts_with("Forget 1 item: note “Retry policy”? A note's file is deleted"),
        "{}",
        e.message
    );
    assert!(new_path.exists());
    assert!(matches!(ask(&d, forget(true)).await.unwrap(), Reply::KbForgotten { items: 1, .. }));
    assert!(!new_path.exists() && aside.exists());
    assert!(db.item_by_uri(new.item.uri.clone()).await.unwrap().is_none());
    assert_eq!(first_hit(&d, "seven tries backoff").await, None);
}

#[tokio::test]
async fn the_wall_holds_for_notes_and_a_refused_file_is_named_once() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    let of = |profile: &str, text: &str| note_in(text, None, Some(profile));
    let (work, _) = noted(&d, of("work", "the work vpn is wireguard")).await;
    let (home, _) = noted(&d, of("home", "the home printer is a brother")).await;
    let (all, _) = noted(&d, note("everyone deploys with make ship")).await;

    // A model of `home` sees its own and nobody's; never `work`'s.
    let scope = KbScope::of("profile", "home", None);
    let seen: Vec<String> =
        d.kb_notes(None, None, Some(&scope)).await.unwrap().into_iter().map(|i| i.uri).collect();
    assert_eq!(seen.len(), 2);
    assert!(seen.contains(&home.item.uri) && seen.contains(&all.item.uri), "{seen:?}");
    assert!(d.kb_note_read(&home.addr, Some(&scope)).await.is_ok());
    let e = d.kb_note_read(&work.addr, Some(&scope)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    // A note its own session wrote is found by it (its session's item never is).
    let mut own = KbScope::of("profile", "work", None);
    own.session = Some("01jb0000000000000000000s01".into());
    let dir = d.kb_notes_dir().join("work").join("_global");
    std::fs::write(
        dir.join("mine.md"),
        "---\nby: claude\nsession: 01jb0000000000000000000s01\n---\nzqownnote\n",
    )
    .unwrap();
    d.kb_notes_sync().await.unwrap();
    let mine: Vec<String> =
        d.kb_notes(None, None, Some(&own)).await.unwrap().into_iter().map(|i| i.uri).collect();
    assert!(mine.contains(&"note://work/_global/mine".to_owned()), "{mine:?}");

    // A keystore saved as a note, and a folder of a profile that isn't: not kept, said why,
    // and not read again until they change.
    let keys: String = (0..30).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect();
    std::fs::write(dir.join("keys.md"), keys).unwrap();
    let stray = d.kb_notes_dir().join("nobody").join("_global");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("x.md"), "zqstray").unwrap();
    let before = d.kb_notes_sync().await.unwrap();
    assert_eq!((before.1, before.2), (0, 0));
    let skipped = d.kb_skipped();
    let why = |uri: &str| skipped.iter().find(|s| s.uri == uri).map(|s| s.why.clone()).unwrap();
    assert!(why("note://work/_global/keys").contains("look like credentials"));
    assert!(
        why("note://nobody/_global/x").contains("nobody"),
        "{}",
        why("note://nobody/_global/x")
    );
    let at = skipped.iter().map(|s| s.at_ms).max();
    tokio::time::sleep(Duration::from_millis(5)).await;
    d.kb_notes_sync().await.unwrap();
    assert_eq!(d.kb_skipped().iter().map(|s| s.at_ms).max(), at, "not read again");

    // A note that was kept and is then filled with keys: what it held is no longer found.
    let path = d.kb_notes_dir().join("_all/_global/everyone-deploys-with-make-ship.md");
    assert_eq!(first_hit(&d, "everyone deploys").await.as_deref(), Some(all.item.uri.as_str()));
    let keys: String = (0..30).map(|i| format!("API_TOKEN=xyz{i}def456\n")).collect();
    touch_later(&path, &format!("everyone deploys\n{keys}"));
    d.kb_notes_sync().await.unwrap();
    assert_eq!(first_hit(&d, "everyone deploys").await, None);
    assert!(path.exists(), "the file is yours: it stays");
    assert!(d.kb_skipped().iter().any(|s| s.uri == all.item.uri));
}
