use proptest::prelude::*;
use ptyro_core::chats::{MARK_OFF, MARK_ON, parse_kb_query, parse_query};
use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::{self, Body, Gate, Never, Plan, Source, Stamp};
use ptyro_core::kb::redact::Redactor;

use super::*;

const M: &str = "test-model";

fn plan(source: Source, stamp: Stamp) -> Plan {
    let never = Never::default();
    let passed = ingest::gate(&Gate::open(&never)).unwrap();
    ingest::plan(passed, source, stamp, &Redactor::default()).unwrap()
}

fn stamp(writer: Writer, profile: Option<&str>, project: Option<&str>) -> Stamp {
    Stamp { project: project.map(str::to_owned), ..Stamp::new(writer, profile.map(str::to_owned)) }
}

/// An item of `kind` with `parts`, all of it.
async fn put(db: &KbDb, kind: ItemKind, uri: &str, parts: Vec<Part>, stamp: Stamp) -> String {
    let mut s = Source::new(kind, uri, Body::Parts(parts));
    s.title = Some(format!("title of {uri}"));
    db.write(plan(s, stamp)).await.unwrap().item
}

fn part(kind: ChunkKind, text: &str, at: u64) -> Part {
    Part::new(kind, text, at)
}

fn fts(q: &str) -> String {
    parse_kb_query(q, 0).unwrap().fts
}

async fn by_words(db: &KbDb, q: &str, f: KbFilter) -> Vec<KbHit> {
    db.search_words(fts(q), f, 10).await.unwrap()
}

fn uris(hits: &[KbHit]) -> Vec<&str> {
    hits.iter().map(|h| h.uri.as_deref().unwrap().rsplit('/').next().unwrap()).collect()
}

fn unit(x: f32, y: f32) -> Vec<f32> {
    let n = (x * x + y * y).sqrt();
    vec![x / n, y / n]
}

/// Give every prose chunk without one a vector: `(1, 0)` when its text has "cat", else
/// `(0, 1)`.
async fn embed(db: &KbDb) {
    let todo = db.vec_todo(M.into(), (0, i64::MAX), 1000).await.unwrap();
    let rows = todo
        .into_iter()
        .map(|t| {
            let v = if t.text.contains("cat") { unit(1.0, 0.05) } else { unit(0.0, 1.0) };
            (t.seq, t.text, ptyro_meaning::quantize(&v))
        })
        .collect();
    db.put_vecs(M.into(), rows).await.unwrap();
}

async fn near(db: &KbDb, f: KbFilter) -> Vec<KbHit> {
    db.search_meaning(unit(1.0, 0.0), M.into(), String::new(), 0.5, f, 10).await.unwrap()
}

/// Two profiles, every kind of item.
async fn corpus(db: &KbDb) -> Vec<String> {
    let work = || stamp(Writer::Agent, Some("work"), Some("p1"));
    let mut ids = Vec::new();
    ids.push(
        put(
            db,
            ItemKind::Session,
            "session://claude/s-work",
            vec![
                part(ChunkKind::Card, "task: the retry policy for the cat feeder", 100)
                    .anchor("card"),
                part(ChunkKind::User, "why does the retry back off so slowly", 110).anchor("u1"),
                part(ChunkKind::Assistant, "the backoff doubles; cap the retry at five", 120)
                    .anchor("a1"),
                part(ChunkKind::Tool, "Edit src/retry.rs", 130).anchor("t1"),
            ],
            Stamp {
                agent: Some("claude".into()),
                session: Some("ptyro-s1".into()),
                workspace: Some("w1".into()),
                ..work()
            },
        )
        .await,
    );
    ids.push(
        put(
            db,
            ItemKind::Session,
            "session://codex/s-home",
            vec![part(ChunkKind::User, "retry the cat photo upload", 200).anchor("u1")],
            Stamp {
                agent: Some("codex".into()),
                workspace: Some("w2".into()),
                ..stamp(Writer::Agent, Some("home"), Some("p2"))
            },
        )
        .await,
    );
    ids.push(
        put(
            db,
            ItemKind::File,
            "file:///repo/docs/retry.md",
            vec![
                part(ChunkKind::Path, "docs/retry.md docs retry md", 300),
                part(ChunkKind::Text, "Retries use exponential backoff.", 300)
                    .heading("retry.md › Policy"),
                part(ChunkKind::Comment, "retry once more, then give up", 300).heading("L12"),
                part(ChunkKind::Code, "fn retry(n: u32) { backoff(n) }", 300),
            ],
            Stamp { root: None, ..stamp(Writer::File, None, Some("p1")) },
        )
        .await,
    );
    let mut note = Source::new(
        ItemKind::Note,
        "note://work/_global/retry",
        Body::Parts(vec![part(
            ChunkKind::Text,
            "Decision: retry three times, the cat agreed.",
            400,
        )]),
    );
    note.title = Some("Retry decision".into());
    ids.push(db.write(plan(note, stamp(Writer::User, Some("work"), None))).await.unwrap().item);
    let mut commit = Source::new(
        ItemKind::Commit,
        "commit://r1/abc123",
        Body::Parts(vec![
            part(ChunkKind::Message, "fix(retry): cap the backoff", 500),
            part(ChunkKind::Path, "src/retry.rs", 500),
        ]),
    );
    commit.files = vec!["src/retry.rs".into()];
    ids.push(db.write(plan(commit, stamp(Writer::File, None, Some("p1")))).await.unwrap().item);
    ids.push(
        put(
            db,
            ItemKind::Shell,
            "shell://sh1/2026-11-01",
            vec![
                part(ChunkKind::Command, "cargo test retry", 600),
                part(ChunkKind::Output, "test retry ... ok", 600),
            ],
            stamp(Writer::User, Some("work"), Some("p1")),
        )
        .await,
    );
    ids
}

#[tokio::test]
async fn words_find_rank_and_mark() {
    let db = KbDb::open_in_memory().unwrap();
    corpus(&db).await;
    let hits = by_words(&db, "exponential backoff", KbFilter::default()).await;
    assert_eq!(uris(&hits), ["retry.md"]);
    let h = &hits[0];
    assert_eq!((h.kind, h.part.as_str(), h.how.as_str()), (HitKind::Doc, "text", "words"));
    assert_eq!(
        h.snippet,
        format!("Retries use {MARK_ON}exponential{MARK_OFF} {MARK_ON}backoff{MARK_OFF}.")
    );
    assert_eq!(h.heading.as_deref(), Some("retry.md › Policy"));
    assert_eq!(h.addr, format!("k:{}/1", h.item));
    assert_eq!((h.idx, h.at_ms, h.project.as_deref()), (Some(1), 300, Some("p1")));
    assert_eq!(
        (h.title.as_deref(), h.writer),
        (Some("title of file:///repo/docs/retry.md"), Some(Writer::File))
    );
    // The porter stemmer: "retries" finds "retry". Every kind of chunk is found by words.
    let all = db.search_words(fts("retries"), KbFilter::default(), 50).await.unwrap();
    let kinds: HashSet<&str> = all.iter().map(|h| h.part.as_str()).collect();
    for k in [
        "card",
        "user",
        "assistant",
        "tool",
        "path",
        "text",
        "comment",
        "code",
        "message",
        "command",
        "output",
    ] {
        assert!(kinds.contains(k), "{k} in {kinds:?}");
    }
    // A match in the heading counts twice one in the text: the section named for the word
    // comes before a longer text that only says it.
    let policy = by_words(&db, "policy", KbFilter::default()).await;
    assert_eq!(uris(&policy), ["retry.md", "s-work"]);
    // Left-out words, phrases and prefixes are the query's.
    assert_eq!(uris(&by_words(&db, "cat -feeder -agreed", KbFilter::default()).await), ["s-home"]);
    assert_eq!(by_words(&db, "\"back off\"", KbFilter::default()).await.len(), 1);
    assert_eq!(by_words(&db, "expon*", KbFilter::default()).await.len(), 1);
    // Nothing asked, nothing found; and the limit is kept.
    assert!(db.search_words(String::new(), KbFilter::default(), 10).await.unwrap().is_empty());
    assert_eq!(db.search_words(fts("retry"), KbFilter::default(), 3).await.unwrap().len(), 3);
}

#[tokio::test]
async fn every_filter_applies_to_words_and_to_meaning() {
    let db = KbDb::open_in_memory().unwrap();
    corpus(&db).await;
    embed(&db).await;
    // The "cat" chunks: the work session's card, the home session's message, the note.
    assert_eq!(uris(&near(&db, KbFilter::default()).await).len(), 3);
    let both = async |f: KbFilter| {
        let mut w: Vec<String> =
            uris(&by_words(&db, "cat", f.clone()).await).iter().map(|s| s.to_string()).collect();
        let mut m: Vec<String> = uris(&near(&db, f).await).iter().map(|s| s.to_string()).collect();
        w.sort();
        m.sort();
        assert_eq!(w, m, "words and meaning agree");
        w
    };
    let f = KbFilter::default;
    assert_eq!(both(f()).await, ["retry", "s-home", "s-work"]);
    // The wall: a profile sees its own and what is nobody's, never another's.
    assert_eq!(both(KbFilter { profile: Some("work".into()), ..f() }).await, ["retry", "s-work"]);
    assert_eq!(both(KbFilter { profile: Some("home".into()), ..f() }).await, ["s-home"]);
    assert!(both(KbFilter { profile: Some("other".into()), ..f() }).await.is_empty());
    let shared = by_words(&db, "exponential", KbFilter { profile: Some("home".into()), ..f() });
    assert_eq!(uris(&shared.await), ["retry.md"], "a document of no profile is everyone's");
    // A workspace: its own, and nothing else.
    assert_eq!(both(KbFilter { workspace: Some("w1".into()), ..f() }).await, ["s-work"]);
    assert_eq!(both(KbFilter { project: Some("p2".into()), ..f() }).await, ["s-home"]);
    assert_eq!(both(KbFilter { kinds: vec![HitKind::Note], ..f() }).await, ["retry"]);
    assert_eq!(
        both(KbFilter { kinds: vec![HitKind::Session, HitKind::Chat], ..f() }).await,
        ["s-home", "s-work"]
    );
    assert!(both(KbFilter { kinds: vec![HitKind::Chat], ..f() }).await.is_empty());
    assert!(both(KbFilter { kinds: vec![HitKind::Doc], ..f() }).await.is_empty());
    assert_eq!(both(KbFilter { agent: Some("codex".into()), ..f() }).await, ["s-home"]);
    assert_eq!(both(KbFilter { writer: Some(Writer::User), ..f() }).await, ["retry"]);
    assert_eq!(both(KbFilter { since_ms: Some(150), ..f() }).await, ["retry", "s-home"]);
    assert_eq!(both(KbFilter { before_ms: Some(150), ..f() }).await, ["s-work"]);
    assert_eq!(
        both(KbFilter { since_ms: Some(150), before_ms: Some(300), ..f() }).await,
        ["s-home"]
    );
    // An agent is not shown its own session.
    assert_eq!(
        both(KbFilter { not_session: Some("ptyro-s1".into()), ..f() }).await,
        ["retry", "s-home"]
    );
    // Kinds by words: a kind is its item's kind and, in a file, the chunk's.
    let kind = async |k: HitKind| {
        let hits = by_words(&db, "retry", KbFilter { kinds: vec![k], ..f() }).await;
        assert!(hits.iter().all(|h| h.kind == k), "{k}: {hits:?}");
        hits.len()
    };
    assert_eq!(kind(HitKind::Session).await, 5);
    assert_eq!((kind(HitKind::Doc).await, kind(HitKind::Comment).await), (1, 1));
    assert_eq!((kind(HitKind::Code).await, kind(HitKind::Path).await), (1, 1));
    assert_eq!((kind(HitKind::Commit).await, kind(HitKind::Command).await), (2, 2));
    assert_eq!(kind(HitKind::Note).await, 1);
    // A path part: the file's own, or one a commit (or a session) touched.
    let file = |p: &str| KbFilter { file: Some(p.into()), ..f() };
    let hits = by_words(&db, "retry", file("docs/retry")).await;
    assert!(
        hits.len() == 4
            && hits.iter().all(|h| h.uri.as_deref() == Some("file:///repo/docs/retry.md"))
    );
    let hits = by_words(&db, "retry", file("src/retry.rs")).await;
    assert!(hits.len() == 2 && hits.iter().all(|h| h.kind == HitKind::Commit));
    // A left-out word rules a chunk out of the meaning's list too.
    let not = db
        .search_meaning(unit(1.0, 0.0), M.into(), "\"feeder\" OR \"agreed\"".into(), 0.5, f(), 10)
        .await
        .unwrap();
    assert_eq!(uris(&not), ["s-home"]);
    // Meaning's hit: the opening as its snippet, the floor, another model's vectors.
    let hits = near(&db, KbFilter { kinds: vec![HitKind::Note], ..f() }).await;
    assert_eq!(
        (hits[0].snippet.as_str(), hits[0].how.as_str()),
        ("Decision: retry three times, the cat agreed.", "meaning")
    );
    assert_eq!((hits[0].kind, hits[0].writer), (HitKind::Note, Some(Writer::User)));
    let none = db.search_meaning(unit(1.0, 0.0), "other".into(), String::new(), 0.5, f(), 10);
    assert!(none.await.unwrap().is_empty());
    let all = db.search_meaning(unit(1.0, 0.0), M.into(), String::new(), -1.0, f(), 100);
    assert_eq!(all.await.unwrap().len(), 8, "every prose chunk has a vector");
}

#[tokio::test]
async fn only_prose_gets_a_vector_and_a_vector_follows_its_chunk() {
    let db = KbDb::open_in_memory().unwrap();
    let ids = corpus(&db).await;
    // (The constant the SQL uses is the kinds' own rule.)
    let prose: Vec<String> =
        ChunkKind::ALL.iter().filter(|k| k.prose()).map(|k| format!("'{k}'")).collect();
    assert_eq!(PROSE, prose.join(", "));
    assert_eq!(db.vec_counts(M.into()).await.unwrap(), (0, 8));
    let todo = db.vec_todo(M.into(), (0, i64::MAX), 3).await.unwrap();
    assert_eq!(todo.len(), 3);
    assert!(todo[0].seq > todo[1].seq, "newest first");
    assert_eq!(
        todo[0].text, "fix(retry): cap the backoff",
        "a command has none: the commit's message"
    );
    // A pass walks down, between where the last pass ended and the last one it got.
    let rest = db.vec_todo(M.into(), (0, todo[2].seq), 100).await.unwrap();
    assert_eq!(rest.len(), 5);
    assert!(
        db.vec_todo(M.into(), (db.vec_top().await.unwrap(), i64::MAX), 9).await.unwrap().is_empty()
    );
    // One item's, in its order: what an ingest embeds at once.
    let of = db.vec_todo_of(ids[0].clone(), M.into()).await.unwrap();
    assert_eq!(of.len(), 3, "the card and two messages; a tool's title has none");
    assert!(of[0].text.starts_with("task:"));
    embed(&db).await;
    assert_eq!(db.vec_counts(M.into()).await.unwrap(), (8, 8));
    assert!(db.vec_todo_of(ids[0].clone(), M.into()).await.unwrap().is_empty());
    assert_eq!(db.status().await.unwrap().vectors, 8);

    // A vector of text the chunk no longer has is dropped, and one for a chunk that is gone.
    let stale = vec![
        (of[0].seq, "old words".to_owned(), vec![0u8; 6]),
        (9_999, "x".to_owned(), vec![0u8; 6]),
    ];
    db.put_vecs("m2".into(), stale).await.unwrap();
    assert_eq!(db.vec_counts("m2".into()).await.unwrap().0, 0);
    // A chunk whose text changes loses its vector, and is embedded again.
    let again = vec![
        part(ChunkKind::Card, "task: the retry policy for the dog feeder", 100).anchor("card"),
        part(ChunkKind::User, "one more", 140).anchor("u2"),
    ];
    let mut s = Source::new(ItemKind::Session, "session://claude/s-work", Body::Parts(again));
    s.append = true;
    let home =
        Stamp { session: Some("ptyro-s1".into()), ..stamp(Writer::Agent, Some("home"), None) };
    db.write(plan(s, home)).await.unwrap();
    let of = db.vec_todo_of(ids[0].clone(), M.into()).await.unwrap();
    assert_eq!(
        of.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["task: the retry policy for the dog feeder", "one more"]
    );
    // The item changed hands (another profile; an appended piece keeps the project it had):
    // the vectors it kept went with it, so the wall holds in the pass itself.
    let whose: Vec<(Option<String>, Option<String>)> = db
        .call({
            let item = ids[0].clone();
            move |c| {
                let mut st = c.prepare("SELECT profile, project FROM chunk_vec WHERE item = ?1")?;
                Ok(st
                    .query_map([item], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            }
        })
        .await
        .unwrap();
    assert_eq!(whose.len(), 2);
    assert!(whose.iter().all(|w| w == &(Some("home".into()), Some("p1".into()))), "{whose:?}");
    // Vectors of a model ptyro no longer uses go when asked.
    assert_eq!(db.drop_other_vecs(M.into()).await.unwrap(), 0);
    assert_eq!(db.drop_other_vecs("new".into()).await.unwrap(), 7);
}

#[tokio::test]
async fn a_forgotten_item_is_never_found_nor_its_vectors() {
    let db = KbDb::open_in_memory().unwrap();
    let ids = corpus(&db).await;
    embed(&db).await;
    let note = ids[3].clone();
    let f = KbFilter::default;
    assert_eq!(by_words(&db, "decision", f()).await.len(), 1);
    // The tombstone is written first, and the chunks go after it in batches: from the
    // tombstone on, nothing of the item is found, by words or by meaning, filtered or not.
    db.call({
        let note = note.clone();
        move |c| Ok(c.execute("UPDATE item SET forgotten_at = 5 WHERE id = ?1", [note])?)
    })
    .await
    .unwrap();
    assert!(by_words(&db, "decision", f()).await.is_empty());
    assert!(
        by_words(&db, "decision", KbFilter { profile: Some("work".into()), ..f() })
            .await
            .is_empty()
    );
    assert_eq!(uris(&near(&db, f()).await), ["s-work", "s-home"]);
    assert_eq!(uris(&near(&db, KbFilter { kinds: vec![HitKind::Note], ..f() }).await), [""; 0]);
    // No vector is made for it meanwhile.
    db.call(|c| {
        Ok(c.execute("DELETE FROM chunk_vec WHERE kind = 'text' AND profile = 'work'", [])?)
    })
    .await
    .unwrap();
    embed(&db).await;
    assert_eq!(db.vec_counts(M.into()).await.unwrap(), (7, 8));
    // Forgotten for good: its chunks, its words and its vectors are gone.
    assert_eq!(db.forget(note).await.unwrap(), Some(1));
    assert!(by_words(&db, "decision", f()).await.is_empty());
    assert_eq!(db.vec_counts(M.into()).await.unwrap(), (7, 7));
    assert_eq!(near(&db, f()).await.len(), 2);
    // Another item's forgetting takes its vectors with it.
    db.forget(ids[0].clone()).await.unwrap();
    assert_eq!(uris(&near(&db, f()).await), ["s-home"]);
    assert_eq!(db.status().await.unwrap().vectors, 4);
}

#[tokio::test]
async fn the_wall_holds_whatever_a_vectors_own_columns_say() {
    let db = KbDb::open_in_memory().unwrap();
    corpus(&db).await;
    embed(&db).await;
    // A vector's copy of its item's profile is wrong (nothing writes one so; a damaged
    // file might hold one): the row is checked against the item before it is returned.
    db.call(|c| Ok(c.execute("UPDATE chunk_vec SET profile = 'work', project = 'p1'", [])?))
        .await
        .unwrap();
    let work = KbFilter { profile: Some("work".into()), ..KbFilter::default() };
    assert_eq!(uris(&near(&db, work).await), ["s-work", "retry"]);
    let p1 = KbFilter { project: Some("p1".into()), ..KbFilter::default() };
    assert_eq!(uris(&near(&db, p1).await), ["s-work"]);
}

#[tokio::test]
async fn a_word_most_chunks_hold_is_ranked_among_its_newest() {
    let db = KbDb::open_in_memory().unwrap();
    // Forty chunks say "common"; the oldest says it most, and would rank first.
    let mut parts = vec![part(ChunkKind::Text, "common common common common", 1)];
    parts.extend((1..40).map(|i| {
        part(ChunkKind::Text, &format!("common words number{i} and more of them"), 1 + i)
    }));
    put(&db, ItemKind::File, "file:///a.md", parts, stamp(Writer::File, None, None)).await;
    let top = |cap: usize| {
        let db = db.clone();
        async move {
            db.call(move |c| words(c, "\"common\"", &KbFilter::default(), 5, cap)).await.unwrap()
        }
    };
    // Under the cap: the best of all of them.
    let all = top(40).await;
    assert_eq!((all.len(), all[0].idx), (5, Some(0)));
    assert_eq!(top(1000).await[0].idx, Some(0));
    // Over it: the best of the newest ten, and never an older one.
    let newest = top(10).await;
    assert_eq!(newest.len(), 5);
    assert!(
        newest.iter().all(|h| h.idx >= Some(30)),
        "{:?}",
        newest.iter().map(|h| h.idx).collect::<Vec<_>>()
    );
    // The same with a filter joined in.
    let f = KbFilter { kinds: vec![HitKind::Doc], ..KbFilter::default() };
    let filtered = db.call(move |c| words(c, "\"common\"", &f, 5, 10)).await.unwrap();
    assert!(filtered.len() == 5 && filtered.iter().all(|h| h.idx >= Some(30)));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Whatever is typed, the expression made of it is one FTS5 takes: a search answers, it
    /// never fails on the query's syntax. (Both parsers, with every character that means
    /// something to FTS5 or to them made likely.)
    #[test]
    fn no_typed_text_is_an_fts5_syntax_error(
        q in proptest::collection::vec(
            prop_oneof![
                "[a-z]{1,6}",
                Just("\"".to_owned()), Just("*".to_owned()), Just("-".to_owned()),
                Just(" ".to_owned()), Just("(".to_owned()), Just(")".to_owned()),
                Just(":".to_owned()), Just("^".to_owned()), Just("+".to_owned()),
                Just("AND".to_owned()), Just("OR".to_owned()), Just("NOT".to_owned()),
                Just("NEAR(".to_owned()), Just("\u{0}".to_owned()), Just("\u{2}".to_owned()),
                Just("text:".to_owned()), Just("{".to_owned()), Just("}".to_owned()),
                Just("kind:".to_owned()), Just("by:words".to_owned()), Just("'".to_owned()),
                Just(",".to_owned()), Just("é".to_owned()), Just("\n".to_owned()),
                any::<char>().prop_map(String::from),
            ],
            0..12,
        ),
    ) {
        let q: String = q.concat();
        let db = db_for_fuzz();
        for parsed in [parse_kb_query(&q, 0), parse_query(&q, 0)] {
            // (What a parser refuses, it refuses in words: a filter with a bad value.)
            let Ok(p) = parsed else { continue };
            let found = db.call_blocking({
                let (fts, not) = (p.fts.clone(), p.not.clone());
                move |c| {
                    words(c, &fts, &KbFilter::default(), 5, 3)?;
                    words(c, &fts, &KbFilter { profile: Some("p".into()), ..KbFilter::default() }, 5, 1000)?;
                    // (The words left out, as the meaning's pass asks for them.)
                    meaning(c, &[1.0, 0.0], M, &not, -1.0, &KbFilter::default(), 5)
                }
            });
            prop_assert!(found.is_ok(), "{q:?} → {:?} / {:?}: {:?}", p.fts, p.not, found.err());
        }
    }
}

fn db_for_fuzz() -> KbDb {
    static DB: std::sync::OnceLock<KbDb> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let db = KbDb::open_in_memory().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            corpus(&db).await;
            embed(&db).await;
        });
        db
    })
    .clone()
}

/// A term the tokenizer makes nothing of (`&&`, `->`) is no word to look for: FTS5 leaves a
/// phrase with no token out of the query, so what the other words find is still found.
#[tokio::test]
async fn a_term_with_no_word_in_it_does_not_empty_the_search() {
    let db = KbDb::open_in_memory().unwrap();
    put(
        &db,
        ItemKind::File,
        "file:///langs.md",
        vec![part(ChunkKind::Text, "we moved the parser from C++ to rust && kept the tests", 1)],
        stamp(Writer::File, None, None),
    )
    .await;
    for q in ["C++ && rust", "rust &&", "&& rust", "rust -> tests", "rust -&&", "\"&&\" rust"] {
        assert_eq!(by_words(&db, q, KbFilter::default()).await.len(), 1, "{q:?} → {:?}", fts(q));
    }
    // Nothing but such terms: nothing is found, and it is no error.
    assert!(by_words(&db, "&& ++", KbFilter::default()).await.is_empty());
}
