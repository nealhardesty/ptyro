//! KB0.1 spike (SPEC §25.8, §25.16): what the knowledge base's search costs over `CHUNKS`
//! chunks (500000 by default) in the schema of §25.4, with a model the size of
//! `potion-base-8M` (30000 tokens × 256, f32), but synthetic, so nothing is downloaded. Run with
//! `make spike-kb`.
//!
//! It fills a throwaway `kb.db` with a realistic mix of kinds (sessions' messages dominate; then
//! documents, comments, paths, commits and commands) over three profiles, twelve projects and
//! two years, with vectors for the prose kinds only, and a second, smaller file standing in for
//! `chat.db`. Then it times, over `QUERIES` queries (61 by default): a search by words, a search
//! by meaning (unfiltered, then filtered by `chunk_vec`'s own columns), §25.8's fallbacks (a
//! faster exact score, newest first with an early stop, a one-bit sketch then rescoring) with
//! their recall, the merged search over both files, an ingest's writer time, forgetting an item
//! with `secure_delete` on, and a WAL checkpoint; and it reports the file's size by part.
//!
//! Since KB1.5 it also times **the search as built**: the same files through `KbDb`'s own
//! `search_words` and `search_meaning`, `ptyro_meaning::fuse_lists` and
//! `ptyro_core::kb::search::rank` (the "as built" rows).
//!
//! The text is made of the synthetic model's words (`w0`…), common ones common, and every item
//! has a topic (16 words that half of its text is drawn from), so that a query about a topic
//! has real neighbours to find, as real text has.

use std::collections::{BinaryHeap, HashMap};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;
use std::time::Instant;

use ptyro_core::chats::parse_kb_query;
use ptyro_core::kb::search::{HitKind, KbHit, rank};
use ptyro_meaning::synthetic::{self, Rng};
use ptyro_meaning::{Found, MIN_SCORE, Model, RRF_K, fuse_lists};
use ptyro_store::kb::{KbDb, KbFilter, MATCH_CAP};
use rusqlite::{Connection, params};

const VOCAB: usize = 30_000;
const DIM: usize = 256;
const MODEL: &str = "spike";
/// A page of hits; each list brings the best `4 × LIMIT` to the merge (§25.8).
const LIMIT: usize = 10;
const LIST: usize = 4 * LIMIT;
/// The sketch's first pass keeps this many for exact rescoring (§25.8).
const RESCORE: usize = 2_000;
/// Fallback (b): the newest vectors scanned before an early stop is allowed, and the cosine
/// that counts as "well above the floor".
const NEWEST: usize = 200_000;
const STRONG: f32 = MIN_SCORE + 0.1;
const TOPICS: usize = 2_000;
const TOPIC_WORDS: usize = 16;
const DAY: i64 = 86_400_000;
/// 2026-11-19, and two years before it.
const NOW: i64 = 1_795_046_400_000;
const SPAN: i64 = 730 * DAY;
const PROFILES: [&str; 3] = ["work", "personal", "client"];
const PROJECTS: usize = 12;

/// §25.4's schema (the parts a search and an ingest touch), plus `chunk_sketch`, which is the
/// spike's: fallback (c)'s one-bit sketches in a narrow table of their own.
const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value);
CREATE TABLE root (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
  project TEXT, profile TEXT, options JSON NOT NULL, head TEXT, state TEXT NOT NULL,
  error TEXT, added_at INTEGER, synced_at INTEGER);
CREATE TABLE item (
  id TEXT PRIMARY KEY, uri TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
  root TEXT REFERENCES root(id) ON DELETE CASCADE,
  title TEXT, rel TEXT, project TEXT, workspace TEXT, worktree TEXT, branch TEXT, profile TEXT,
  agent TEXT, model TEXT, session TEXT, writer TEXT NOT NULL, files JSON, hash TEXT, cursor JSON,
  chunks INTEGER NOT NULL DEFAULT 0, started_at INTEGER, ended_at INTEGER, ingested_at INTEGER,
  origin TEXT NOT NULL, forgotten_at INTEGER);
CREATE INDEX item_by_kind ON item(kind, ended_at);
CREATE INDEX item_by_root ON item(root, rel);
CREATE INDEX item_by_session ON item(session);
CREATE TABLE chunk (
  seq INTEGER PRIMARY KEY, item TEXT NOT NULL REFERENCES item(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL, kind TEXT NOT NULL, anchor TEXT, heading TEXT, text TEXT NOT NULL,
  meta JSON, at_ms INTEGER NOT NULL, UNIQUE(item, idx));
CREATE VIRTUAL TABLE chunk_fts USING fts5(text, heading, content='chunk', content_rowid='seq',
                                          tokenize='porter unicode61 remove_diacritics 2');
CREATE TRIGGER chunk_ai AFTER INSERT ON chunk BEGIN
  INSERT INTO chunk_fts(rowid, text, heading) VALUES (new.seq, new.text, new.heading);
END;
CREATE TRIGGER chunk_ad AFTER DELETE ON chunk BEGIN
  INSERT INTO chunk_fts(chunk_fts, rowid, text, heading)
  VALUES ('delete', old.seq, old.text, old.heading);
END;
INSERT INTO chunk_fts(chunk_fts, rank) VALUES ('rank', 'bm25(1.0, 2.0)');
CREATE TABLE chunk_vec (
  seq INTEGER PRIMARY KEY REFERENCES chunk(seq) ON DELETE CASCADE,
  item TEXT NOT NULL, kind TEXT NOT NULL, profile TEXT, project TEXT, at_ms INTEGER NOT NULL,
  model TEXT NOT NULL, vec BLOB NOT NULL);
CREATE TABLE chunk_sketch (
  seq INTEGER PRIMARY KEY REFERENCES chunk(seq) ON DELETE CASCADE,
  kind TEXT NOT NULL, profile TEXT, project TEXT, at_ms INTEGER NOT NULL, bits BLOB NOT NULL);
";

type R<T> = rusqlite::Result<T>;

fn main() -> R<()> {
    let chunks = env("CHUNKS", 500_000);
    let chat_nodes = env("CHAT_NODES", 100_000);
    let queries = env("QUERIES", 61).max(3);
    let dir = tempfile::tempdir().expect("a temp dir");
    println!(
        "knowledge base search over {chunks} chunks (and {chat_nodes} standing in for chat.db), \
         a {VOCAB}×{DIM} synthetic model, SQLite {}",
        rusqlite::version()
    );

    synthetic::random(&dir.path().join("model"), VOCAB, DIM, 7).expect("model written");
    let model = Model::load(&dir.path().join("model")).expect("model loads");
    let mut data = Data::new();

    // ── Fill ────────────────────────────────────────────────────────────────────────────────
    let kb_path = dir.path().join("kb.db");
    let mut kb = open(&kb_path)?;
    let t = Instant::now();
    let filled = fill(&mut kb, &mut data, &model, chunks, false)?;
    println!(
        "  filled                  {:>8.1} s  ({} items, {} vectors = {:.0}% of the chunks; \
         {:.0} µs to embed one)",
        t.elapsed().as_secs_f64(),
        filled.items,
        filled.vectors,
        100.0 * filled.vectors as f64 / chunks as f64,
        filled.embed_ms * 1000.0 / filled.vectors.max(1) as f64
    );
    let mix: Vec<(String, i64, i64)> = kb
        .prepare(
            "SELECT kind, count(*), avg(length(text)) FROM chunk GROUP BY kind ORDER BY 2 DESC",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, f64>(2)? as i64)))?
        .collect::<R<_>>()?;
    let mix: Vec<String> = mix
        .iter()
        .map(|(k, n, len)| format!("{k} {:.0}% ({len} chars)", 100.0 * *n as f64 / chunks as f64))
        .collect();
    println!("  kinds                   {}", mix.join(", "));
    let mut chat = open(&dir.path().join("chat-standin.db"))?;
    fill(&mut chat, &mut data, &model, chat_nodes, true)?;
    kb.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;

    // ── Size ────────────────────────────────────────────────────────────────────────────────
    sizes(&kb, &kb_path, chunks, filled.vectors)?;

    // ── Words ───────────────────────────────────────────────────────────────────────────────
    println!("search by words (bm25, best {LIST}, with snippets): median / 95th / worst, ms");
    let profile = " AND (i.profile = 'work' OR i.profile IS NULL)";
    let mut qs = Queries { rng: Rng(9), topics: &data.topics };
    let topical: Vec<String> = (0..queries).map(|_| qs.topic_words(2)).collect();
    let rare: Vec<String> = (0..queries).map(|_| qs.rare_words(2)).collect();
    let common: Vec<String> = (0..queries).map(|i| format!("w{}", i % 20)).collect();
    let mixed: Vec<String> =
        (0..queries).map(|i| format!("w{} {}", i % 20, qs.topic_words(1))).collect();
    for (name, list) in [
        ("two words of a topic", &topical),
        ("two rare words", &rare),
        ("a common and a topic word", &mixed),
        ("one of the 20 commonest", &common),
    ] {
        let hits = median_of(list.iter().map(|q| count_matches(&kb, q)));
        for (shape, wh) in
            [(Shape::OneQuery, ""), (Shape::TwoSteps, ""), (Shape::TwoSteps, profile)]
        {
            let times = time_each(list, |q| words(&kb, q, wh, shape).map(|_| ()))?;
            let how = match (shape, wh.is_empty()) {
                (Shape::OneQuery, _) => "one query, as chat's",
                (Shape::TwoSteps, true) => "rank first, then snippets",
                (Shape::TwoSteps, false) => "rank first, one profile",
            };
            row(&format!("{name} ({hits} match), {how}"), &times);
        }
    }

    // ── Meaning ─────────────────────────────────────────────────────────────────────────────
    println!(
        "search by meaning over {} vectors (best {LIST}, floor {MIN_SCORE}, rows fetched): \
         median / 95th / worst, ms",
        filled.vectors
    );
    let texts: Vec<String> = (0..queries).map(|_| qs.topic_words(3)).collect();
    let vecs: Vec<Vec<f32>> = texts.iter().map(|t| model.embed(t)).collect();
    let since = NOW - 30 * DAY;
    let filters = [
        ("unfiltered", String::new()),
        ("one profile (and no profile)", " AND (profile = 'work' OR profile IS NULL)".to_owned()),
        (
            "one profile, one project",
            " AND (profile = 'work' OR profile IS NULL) AND project = 'p01'".to_owned(),
        ),
        ("kind: documents and comments", " AND kind IN ('text', 'comment')".to_owned()),
        ("kind: cards", " AND kind = 'card'".to_owned()),
        ("the last 30 days", format!(" AND at_ms >= {since}")),
    ];
    for (name, wh) in &filters {
        let n: i64 =
            kb.query_row(&format!("SELECT count(*) FROM chunk_vec WHERE 1{wh}"), [], |r| r.get(0))?;
        let times = time_each(&vecs, |q| {
            let best = scan(&kb, wh, q, LIST, MIN_SCORE, ptyro_meaning::score)?;
            fetch(&kb, &best)
        })?;
        row(&format!("exact scan, {name} ({n} vectors)"), &times);
    }
    let above = median_of(vecs.iter().map(|q| count_above(&kb, q).unwrap_or(0)));
    println!("    (a query has {above} chunks above the floor, median)");

    // §25.8's fallbacks, and two that cost nothing to build: a score the compiler can
    // vectorise, and reading the file through the page cache (`mmap_size`).
    println!("the fallbacks: median / 95th / worst, ms");
    for (name, wh) in &filters[..2] {
        let times = time_each(&vecs, |q| {
            let best = scan(&kb, wh, q, LIST, MIN_SCORE, score_lanes)?;
            fetch(&kb, &best)
        })?;
        row(&format!("exact, a faster score (8 lanes), {name}"), &times);
    }
    let mapped = Connection::open(&kb_path)?;
    mapped.pragma_update(None, "mmap_size", 4_i64 << 30)?;
    for (name, wh) in &filters[..2] {
        let times = time_each(&vecs, |q| {
            let best = scan(&mapped, wh, q, LIST, MIN_SCORE, score_lanes)?;
            fetch(&mapped, &best)
        })?;
        row(&format!("exact, faster score, mmap_size, {name}"), &times);
    }
    let times = time_each(&vecs, |q| scan_only(&mapped, q))?;
    row("    (of which: reading the rows, no scoring)", &times);

    // (b) Newest first, stopping when the page is full of strong hits.
    let (mut stopped, mut found, mut wanted) = (0, 0, 0);
    let times = time_each(&vecs, |q| {
        let (best, early) = newest_first(&mapped, q)?;
        stopped += usize::from(early);
        if early {
            let exact = scan(&mapped, "", q, LIMIT, MIN_SCORE, score_lanes)?;
            wanted += exact.len();
            found += exact.iter().filter(|e| best.iter().any(|b| b.1 == e.1)).count();
        }
        fetch(&mapped, &best)
    })?;
    row(&format!("(b) newest {NEWEST} first, stop on {LIMIT} above {STRONG:.1}"), &times);
    println!(
        "    stopped early on {stopped} of {} queries; of the true best {LIMIT} it returned {}",
        vecs.len(),
        percent(found, wanted)
    );

    // (c) A one-bit sketch of each vector, then the best RESCORE rescored exactly.
    for (name, wh) in &filters[..2] {
        let (mut found, mut wanted) = (0, 0);
        let mut times = Vec::new();
        for q in &vecs {
            let t = Instant::now();
            let best = sketch_scan(&mapped, wh, q, LIST, MIN_SCORE)?;
            fetch(&mapped, &best)?;
            times.push(ms(t));
            // Recall is judged without the floor: the ranking is what the sketch could lose.
            let exact = scan(&mapped, wh, q, LIMIT, -1.0, score_lanes)?;
            let got = sketch_scan(&mapped, wh, q, LIMIT, -1.0)?;
            wanted += exact.len();
            found += exact.iter().filter(|e| got.iter().any(|g| g.1 == e.1)).count();
        }
        row(&format!("(c) one-bit sketch, rescore {RESCORE}, {name}"), &times);
        println!("    of the true best {LIMIT} it returned {}", percent(found, wanted));
    }

    // ── Merged ──────────────────────────────────────────────────────────────────────────────
    println!(
        "the merged search (words + meaning in kb.db and in chat.db's stand-in, RRF, a page fetched): median / 95th / worst, ms"
    );
    let pairs: Vec<(String, Vec<f32>)> =
        topical.iter().map(|t| (t.clone(), model.embed(t))).collect();
    let e = Instant::now();
    for (t, _) in &pairs {
        std::hint::black_box(model.embed(t));
    }
    let embed_ms = ms(e) / pairs.len() as f64;
    for (name, plan) in [
        ("as chat does it (one query, the plain score)", Plan::AsChat),
        ("rank first, faster score, mmap_size", Plan::Fast),
        ("rank first, the sketch, mmap_size", Plan::Sketch),
    ] {
        let kbc = if plan == Plan::AsChat { &kb } else { &mapped };
        let times = time_each(&pairs, |(text, q)| merged(kbc, &chat, text, q, plan))?;
        row(name, &times);
    }
    // The worst query there is: one word that a large part of every file has.
    let worst: Vec<(String, Vec<f32>)> =
        common.iter().map(|t| (t.clone(), model.embed(t))).collect();
    for (name, plan) in [
        ("one of the 20 commonest words, as chat does it", Plan::AsChat),
        ("one of the 20 commonest words, rank first, faster score", Plan::Fast),
    ] {
        let kbc = if plan == Plan::AsChat { &kb } else { &mapped };
        let times = time_each(&worst, |(text, q)| merged(kbc, &chat, text, q, plan))?;
        row(name, &times);
    }
    println!(
        "    (plus {:.1} µs to embed the query; the two files are searched one after the other)",
        embed_ms * 1000.0
    );
    drop(mapped);

    // ── As built ────────────────────────────────────────────────────────────────────────────
    as_built(
        (&kb, &kb_path),
        (&chat, &dir.path().join("chat-standin.db")),
        [&topical, &rare, &mixed, &common],
        &vecs,
        &model,
    )?;

    // ── Ingest ──────────────────────────────────────────────────────────────────────────────
    println!("ingest (one transaction an item, 100 chunks each): median / 95th / worst, ms");
    let mut ids = Vec::new();
    let mut times = Vec::new();
    for _ in 0..40 {
        let item = data.item(1.0, Some(Kind::Session), 100);
        let t = Instant::now();
        let tx = kb.transaction()?;
        write_item(&tx, &item, None)?;
        tx.commit()?;
        times.push(ms(t));
        ids.push(item);
    }
    row("the writer's time, rows and index (no vectors)", &times);
    let (mut times, mut written) = (Vec::new(), 0);
    for item in &ids {
        let vecs: Vec<(usize, Vec<f32>)> = item
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| prose(c.kind))
            .map(|(i, c)| (i, model.embed(&c.text)))
            .collect();
        written += vecs.len();
        let t = Instant::now();
        let tx = kb.transaction()?;
        write_vecs(&tx, item, &vecs)?;
        tx.commit()?;
        times.push(ms(t));
    }
    row(&format!("the writer's time for their vectors, later ({} an item)", written / 40), &times);
    let t = Instant::now();
    kb.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    println!("  a checkpoint after those 4000 chunks {:>30.1} ms", ms(t));

    // ── Forget ──────────────────────────────────────────────────────────────────────────────
    println!(
        "forgetting an item of 100 chunks (rows, index entries, vectors): median / 95th / worst, ms"
    );
    let mut ids = ids.into_iter();
    for (name, secure, fts_secure) in [
        ("secure_delete off (for comparison)", false, false),
        ("secure_delete on", true, false),
        ("secure_delete on, and FTS5's own secure-delete", true, true),
    ] {
        kb.pragma_update(None, "secure_delete", secure)?;
        kb.execute(
            "INSERT INTO chunk_fts(chunk_fts, rank) VALUES ('secure-delete', ?1)",
            [i64::from(fts_secure)],
        )?;
        // An item with a word nothing else has, to look for in the file's bytes afterwards.
        let marker = format!("zqforgotten{}", u8::from(secure) + u8::from(fts_secure));
        let mut marked = data.item(1.0, Some(Kind::Session), 100);
        marked.chunks[50].text.push_str(&format!(" {marker} {marker}"));
        let tx = kb.transaction()?;
        write_item(&tx, &marked, None)?;
        tx.commit()?;
        kb.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;

        let mut times = Vec::new();
        for item in ids.by_ref().take(12).chain([marked]) {
            let t = Instant::now();
            forget(&mut kb, &item.id)?;
            times.push(ms(t));
        }
        row(name, &times);
        let t = Instant::now();
        kb.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        let checkpoint = ms(t);
        let left = contains(&kb_path, marker.as_bytes())
            || contains(&dir.path().join("kb.db-wal"), marker.as_bytes());
        println!(
            "    the checkpoint after those 13: {checkpoint:.1} ms; the forgotten word is {} in the file's bytes",
            if left { "STILL" } else { "nowhere" }
        );
    }
    Ok(())
}

// ── The data ────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Session,
    Document,
    Source,
    Commit,
    Shell,
}

/// The share of all chunks each kind of item holds.
const SHARES: [(Kind, f64); 5] = [
    (Kind::Session, 0.68),
    (Kind::Document, 0.10),
    (Kind::Source, 0.10),
    (Kind::Commit, 0.08),
    (Kind::Shell, 0.04),
];

struct Chunk {
    kind: &'static str,
    heading: Option<String>,
    text: String,
    at_ms: i64,
}

struct Item {
    id: String,
    uri: String,
    kind: &'static str,
    title: String,
    profile: Option<&'static str>,
    project: Option<String>,
    chunks: Vec<Chunk>,
}

/// §25.4: only prose gets a vector.
fn prose(kind: &str) -> bool {
    matches!(kind, "card" | "user" | "assistant" | "summary" | "text" | "comment" | "message")
}

struct Data {
    rng: Rng,
    topics: Vec<[usize; TOPIC_WORDS]>,
    items: usize,
    written: [usize; 5],
}

impl Data {
    fn new() -> Self {
        let mut rng = Rng(42);
        let topics = (0..TOPICS)
            .map(|_| std::array::from_fn(|_| 200 + (rng.next_u64() as usize) % (VOCAB - 200)))
            .collect();
        Self { rng, topics, items: 0, written: [0; 5] }
    }

    fn below(&mut self, n: usize) -> usize {
        (self.rng.next_u64() % n as u64) as usize
    }

    fn unit(&mut self) -> f64 {
        (self.rng.next_u64() % 1_000_000) as f64 / 1e6
    }

    /// `n` words: half from the topic, half from the whole vocabulary with common words common.
    fn text(&mut self, topic: usize, n: usize) -> String {
        let mut s = String::with_capacity(n * 7);
        for i in 0..n {
            let w = if self.rng.next_u64() & 1 == 0 {
                let at = self.below(TOPIC_WORDS);
                self.topics[topic][at]
            } else {
                let u = self.unit();
                (u * u * u * VOCAB as f64) as usize
            };
            let _ = write!(s, "{}w{w}", if i == 0 { "" } else { " " });
        }
        s
    }

    fn between(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    /// The next item: the kind furthest behind its share (or `kind`), at `progress` (0…1)
    /// through the two years, with `size` chunks (0: the kind's own spread).
    fn item(&mut self, progress: f64, kind: Option<Kind>, size: usize) -> Item {
        let total: usize = self.written.iter().sum::<usize>().max(1);
        let at = kind.map_or_else(
            || {
                (0..SHARES.len())
                    .min_by(|a, b| {
                        let behind = |i: usize| self.written[i] as f64 / total as f64 / SHARES[i].1;
                        behind(*a).total_cmp(&behind(*b))
                    })
                    .unwrap_or(0)
            },
            |k| SHARES.iter().position(|s| s.0 == k).unwrap_or(0),
        );
        let kind = SHARES[at].0;
        self.items += 1;
        let n = self.items;
        let topic = self.below(TOPICS);
        let project = {
            let u = self.unit();
            (u * u * PROJECTS as f64) as usize
        };
        // A project is pinned to a profile, or to none (every profile may see its files).
        let pin = [None, Some("work"), Some("work"), Some("personal")][project % 4];
        let happened = NOW - SPAN + (progress * SPAN as f64) as i64;
        let anytime = NOW - SPAN + (self.unit() * SPAN as f64) as i64;
        let mut chunks = Vec::new();
        let mut push = |d: &mut Self, kind: &'static str, heading: usize, lo, hi, at_ms| {
            let heading = (heading > 0).then(|| d.text(topic, heading));
            let words = d.between(lo, hi);
            chunks.push(Chunk { kind, heading, text: d.text(topic, words), at_ms });
        };
        let (item_kind, uri, profile, project) = match kind {
            Kind::Session => {
                let count = if size > 0 { size } else { self.between(20, 120) };
                push(self, "card", 0, 80, 200, happened);
                for j in 1..count {
                    let at_ms = happened + j as i64 * 20_000;
                    match self.below(100) {
                        0..12 => push(self, "user", 0, 5, 150, at_ms),
                        12..50 => push(self, "assistant", 0, 20, 250, at_ms),
                        50..99 => push(self, "tool", 0, 3, 12, at_ms),
                        _ => push(self, "summary", 0, 150, 280, at_ms),
                    }
                }
                let profile = PROFILES
                    [[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 2, 2][self.below(20)]];
                let project = (self.below(100) >= 15).then(|| format!("p{project:02}"));
                (
                    "session",
                    format!("session://claude/{n:08x}-5a1e-4c0d-9f3b-{n:012x}"),
                    Some(profile),
                    project,
                )
            }
            Kind::Document | Kind::Source => {
                let doc = kind == Kind::Document;
                push(self, "path", 0, 3, 8, anytime);
                for _ in 0..self.between(2, if doc { 30 } else { 25 }) {
                    if doc {
                        push(self, "text", 5, 50, 200, anytime);
                    } else {
                        push(self, "comment", 8, 6, 60, anytime);
                    }
                }
                let ext = if doc { "md" } else { "rs" };
                let uri = format!(
                    "file:///Users/someone/dev/p{project:02}/src/part{}/file{n}.{ext}",
                    n % 40
                );
                ("file", uri, pin, Some(format!("p{project:02}")))
            }
            Kind::Commit => {
                push(self, "message", 0, 8, 80, happened);
                push(self, "path", 0, 5, 50, happened);
                (
                    "commit",
                    format!("commit://01JROOT{project:019}/{n:040x}"),
                    pin,
                    Some(format!("p{project:02}")),
                )
            }
            Kind::Shell => {
                for j in 0..self.between(5, 40) {
                    push(self, "command", 0, 2, 10, happened + j as i64 * 60_000);
                }
                let profile = PROFILES[self.below(2)];
                ("shell", format!("shell://01JSESSION{n:016}/2026-01-01#{n}"), Some(profile), None)
            }
        };
        self.written[at] += chunks.len();
        Item {
            id: format!("01JB{n:022}"),
            uri,
            kind: item_kind,
            title: self.text(topic, 6),
            profile,
            project,
            chunks,
        }
    }
}

fn open(path: &Path) -> R<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "secure_delete", "ON")?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

struct Filled {
    items: usize,
    vectors: usize,
    embed_ms: f64,
}

fn fill(
    db: &mut Connection,
    data: &mut Data,
    model: &Model,
    chunks: usize,
    chat: bool,
) -> R<Filled> {
    let (mut done, mut out) = (0, Filled { items: 0, vectors: 0, embed_ms: 0.0 });
    while done < chunks {
        let tx = db.transaction()?;
        for _ in 0..200 {
            if done >= chunks {
                break;
            }
            let mut item = data.item(done as f64 / chunks as f64, chat.then_some(Kind::Session), 0);
            item.chunks.truncate(chunks - done);
            let e = Instant::now();
            let vecs: Vec<(usize, Vec<f32>)> = item
                .chunks
                .iter()
                .enumerate()
                .filter(|(_, c)| prose(c.kind))
                .map(|(i, c)| (i, model.embed(&c.text)))
                .collect();
            out.embed_ms += ms(e);
            write_item(&tx, &item, Some(&vecs))?;
            done += item.chunks.len();
            out.items += 1;
            out.vectors += vecs.len();
        }
        tx.commit()?;
    }
    Ok(out)
}

/// One item's rows, as §25.5's step 6 writes them (and its vectors, when the fill gives them).
fn write_item(tx: &Connection, item: &Item, vecs: Option<&[(usize, Vec<f32>)]>) -> R<()> {
    let (first, last) =
        (item.chunks.first().map_or(0, |c| c.at_ms), item.chunks.last().map_or(0, |c| c.at_ms));
    tx.prepare_cached(
        "INSERT INTO item (id, uri, kind, title, rel, project, profile, agent, model, writer, hash,
                           cursor, chunks, started_at, ended_at, ingested_at, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'claude', 'a-model-name', 'agent', ?8,
                 '{\"dev\":16777234,\"ino\":48211034,\"size\":812345,\"offset\":812345}', ?9, ?10,
                 ?11, ?11, '01JMACHINE0000000000000000')",
    )?
    .execute(params![
        item.id,
        item.uri,
        item.kind,
        item.title,
        item.uri.rsplit('/').next(),
        item.project,
        item.profile,
        format!("{:064x}", item.id.len() * 7919 + item.chunks.len()),
        item.chunks.len() as i64,
        first,
        last
    ])?;
    let mut st = tx.prepare_cached(
        "INSERT INTO chunk (item, idx, kind, anchor, heading, text, at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (i, c) in item.chunks.iter().enumerate() {
        let anchor = format!("{:08x}-0000-4000-8000-{i:012x}", item.chunks.len());
        st.execute(params![item.id, i as i64, c.kind, anchor, c.heading, c.text, c.at_ms])?;
    }
    match vecs {
        Some(v) => write_vecs(tx, item, v),
        None => Ok(()),
    }
}

fn write_vecs(tx: &Connection, item: &Item, vecs: &[(usize, Vec<f32>)]) -> R<()> {
    let mut seq = tx.prepare_cached("SELECT seq FROM chunk WHERE item = ?1 AND idx = ?2")?;
    let mut vec = tx.prepare_cached(
        "INSERT INTO chunk_vec (seq, item, kind, profile, project, at_ms, model, vec)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    let mut sketch = tx.prepare_cached(
        "INSERT INTO chunk_sketch (seq, kind, profile, project, at_ms, bits)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (i, v) in vecs {
        let c = &item.chunks[*i];
        let s: i64 = seq.query_row(params![item.id, *i as i64], |r| r.get(0))?;
        let blob = ptyro_meaning::quantize(v);
        vec.execute(params![s, item.id, c.kind, item.profile, item.project, c.at_ms, MODEL, blob])?;
        sketch.execute(params![
            s,
            c.kind,
            item.profile,
            item.project,
            c.at_ms,
            bits(v).to_vec()
        ])?;
    }
    Ok(())
}

/// §25.12: the chunks, their index entries and their vectors go; the item stays as a tombstone.
fn forget(db: &mut Connection, id: &str) -> R<()> {
    let tx = db.transaction()?;
    tx.execute("DELETE FROM chunk WHERE item = ?1", [id])?;
    tx.execute(
        "UPDATE item SET title = NULL, files = NULL, hash = NULL, cursor = NULL, chunks = 0,
                         forgotten_at = ?2 WHERE id = ?1",
        params![id, NOW],
    )?;
    tx.commit()
}

// ── The searches ────────────────────────────────────────────────────────────────────────────

struct Queries<'a> {
    rng: Rng,
    topics: &'a [[usize; TOPIC_WORDS]],
}

impl Queries<'_> {
    fn topic_words(&mut self, n: usize) -> String {
        let topic = &self.topics[(self.rng.next_u64() % TOPICS as u64) as usize];
        let at = (self.rng.next_u64() % TOPIC_WORDS as u64) as usize;
        let words: Vec<String> =
            (0..n).map(|i| format!("w{}", topic[(at + i * 5) % TOPIC_WORDS])).collect();
        words.join(" ")
    }

    fn rare_words(&mut self, n: usize) -> String {
        let words: Vec<String> = (0..n)
            .map(|_| format!("w{}", 2_000 + self.rng.next_u64() % (VOCAB as u64 - 2_000)))
            .collect();
        words.join(" ")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// Chat's (§22.10): the match, the joins, the snippet and the order in one statement.
    OneQuery,
    /// The index ranks first (`ORDER BY rank`), then a snippet for each row that is kept.
    TwoSteps,
}

/// The best `LIST` chunks by words, each with its snippet. `wh` filters on the item (`i.`).
fn words(db: &Connection, query: &str, wh: &str, shape: Shape) -> R<Vec<i64>> {
    if shape == Shape::OneQuery {
        let mut st = db.prepare_cached(&format!(
            "SELECT c.seq, snippet(chunk_fts, 0, char(2), char(3), '…', 16), c.kind, c.at_ms,
                    bm25(chunk_fts, 1.0, 2.0) AS r
             FROM chunk_fts JOIN chunk c ON c.seq = chunk_fts.rowid JOIN item i ON i.id = c.item
             WHERE chunk_fts MATCH ?1 AND i.forgotten_at IS NULL{wh} ORDER BY r LIMIT ?2"
        ))?;
        return st
            .query_map(params![query, LIST as i64], |r| {
                std::hint::black_box(r.get::<_, String>(1)?);
                r.get(0)
            })?
            .collect();
    }
    let seqs: Vec<i64> = if wh.is_empty() {
        db.prepare_cached(
            "SELECT rowid FROM chunk_fts WHERE chunk_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?
        .query_map(params![query, LIST as i64], |r| r.get(0))?
        .collect::<R<_>>()?
    } else {
        db.prepare_cached(&format!(
            "SELECT chunk_fts.rowid FROM chunk_fts
             JOIN chunk c ON c.seq = chunk_fts.rowid JOIN item i ON i.id = c.item
             WHERE chunk_fts MATCH ?1 AND i.forgotten_at IS NULL{wh}
             ORDER BY chunk_fts.rank LIMIT ?2"
        ))?
        .query_map(params![query, LIST as i64], |r| r.get(0))?
        .collect::<R<_>>()?
    };
    let mut st = db.prepare_cached(
        "SELECT snippet(chunk_fts, 0, char(2), char(3), '…', 16), c.kind, c.at_ms, i.title
         FROM chunk_fts JOIN chunk c ON c.seq = chunk_fts.rowid JOIN item i ON i.id = c.item
         WHERE chunk_fts MATCH ?1 AND chunk_fts.rowid = ?2",
    )?;
    for seq in &seqs {
        std::hint::black_box(st.query_row(params![query, seq], |r| r.get::<_, String>(0))?);
    }
    Ok(seqs)
}

fn count_matches(db: &Connection, query: &str) -> i64 {
    db.query_row("SELECT count(*) FROM chunk_fts WHERE chunk_fts MATCH ?1", [query], |r| r.get(0))
        .unwrap_or(0)
}

/// The exact scan, as chat's (`ptyro_store::chat::vec`): every vector `wh` allows, scored, the
/// best `k` of at least `min`. `wh` names `chunk_vec`'s own columns only: no join.
fn scan(
    db: &Connection,
    wh: &str,
    q: &[f32],
    k: usize,
    min: f32,
    score: fn(&[u8], &[f32]) -> Option<f32>,
) -> R<Vec<(f32, i64)>> {
    let mut st =
        db.prepare_cached(&format!("SELECT seq, vec FROM chunk_vec WHERE model = ?1{wh}"))?;
    let mut rows = st.query([MODEL])?;
    let mut failed = None;
    let scored = std::iter::from_fn(|| {
        loop {
            let r = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => return None,
                Err(e) => {
                    failed = Some(e);
                    return None;
                }
            };
            let s = r.get_ref(1).ok().and_then(|v| v.as_blob().ok()).and_then(|b| score(b, q));
            if let (Ok(seq), Some(s)) = (r.get::<_, i64>(0), s)
                && s >= min
            {
                return Some((s, seq));
            }
        }
    });
    let best = ptyro_meaning::top_k(scored, k);
    failed.map_or(Ok(best), Err)
}

/// How many vectors are at least the floor away from nothing: what a query "is about".
fn count_above(db: &Connection, q: &[f32]) -> R<i64> {
    let mut st = db.prepare_cached("SELECT vec FROM chunk_vec WHERE model = ?1")?;
    let mut rows = st.query([MODEL])?;
    let mut n = 0;
    while let Some(r) = rows.next()? {
        let s = score_lanes(r.get_ref(0)?.as_blob().unwrap_or_default(), q);
        n += i64::from(s.is_some_and(|s| s >= MIN_SCORE));
    }
    Ok(n)
}

/// What the scan costs before any arithmetic: stepping every row and touching its blob.
fn scan_only(db: &Connection, _q: &[f32]) -> R<()> {
    let mut st = db.prepare_cached("SELECT seq, vec FROM chunk_vec WHERE model = ?1")?;
    let mut rows = st.query([MODEL])?;
    let mut n = 0usize;
    while let Some(r) = rows.next()? {
        n += r.get_ref(1)?.as_blob().map_or(0, <[u8]>::len);
    }
    std::hint::black_box(n);
    Ok(())
}

/// `ptyro_meaning::score` with eight running sums instead of one: a float sum can't be
/// reordered by the compiler, so the plain loop adds 256 numbers one after the other; eight
/// lanes are one SIMD register. The same cosine but for the last bits.
fn score_lanes(blob: &[u8], q: &[f32]) -> Option<f32> {
    let (scale, bytes) = blob.split_first_chunk::<4>()?;
    if bytes.len() != q.len() {
        return None;
    }
    let mut acc = [0f32; 8];
    let ((b8, b_rest), (q8, q_rest)) = (bytes.as_chunks::<8>(), q.as_chunks::<8>());
    let tail: f32 = b_rest.iter().zip(q_rest).map(|(b, x)| f32::from(*b as i8) * x).sum();
    for (b, x) in b8.iter().zip(q8) {
        for i in 0..8 {
            acc[i] += f32::from(b[i] as i8) * x[i];
        }
    }
    Some((acc.iter().sum::<f32>() + tail) * f32::from_le_bytes(*scale))
}

/// Fallback (b): newest first; after `NEWEST` vectors, stop if the page is already full of
/// hits of at least `STRONG`. Returns the best `LIST` seen and whether it stopped early.
fn newest_first(db: &Connection, q: &[f32]) -> R<(Vec<(f32, i64)>, bool)> {
    let mut st =
        db.prepare_cached("SELECT seq, vec FROM chunk_vec WHERE model = ?1 ORDER BY seq DESC")?;
    let mut rows = st.query([MODEL])?;
    let (mut seen, mut strong, mut early) = (0usize, 0usize, false);
    let mut best: Vec<(f32, i64)> = Vec::new();
    while let Some(r) = rows.next()? {
        seen += 1;
        if let Some(s) = score_lanes(r.get_ref(1)?.as_blob().unwrap_or_default(), q)
            && s >= MIN_SCORE
        {
            strong += usize::from(s >= STRONG);
            best.push((s, r.get(0)?));
        }
        if seen == NEWEST && strong >= LIMIT {
            early = rows.next()?.is_some();
            break;
        }
    }
    Ok((ptyro_meaning::top_k(best, LIST), early))
}

/// A vector's one-bit sketch: the sign of each dimension, 32 bytes for 256.
fn bits(v: &[f32]) -> [u8; DIM / 8] {
    let mut out = [0u8; DIM / 8];
    for (i, x) in v.iter().enumerate().take(DIM) {
        out[i / 8] |= u8::from(*x > 0.0) << (i % 8);
    }
    out
}

/// Fallback (c): the Hamming distance of every sketch to the query's, the nearest `RESCORE`
/// rescored exactly from `chunk_vec`, the best `k` of at least `min`.
fn sketch_scan(db: &Connection, wh: &str, q: &[f32], k: usize, min: f32) -> R<Vec<(f32, i64)>> {
    let qb: [u64; DIM / 64] = {
        let b = bits(q);
        std::array::from_fn(|i| u64::from_le_bytes(std::array::from_fn(|j| b[i * 8 + j])))
    };
    let mut st = db.prepare_cached(&format!("SELECT seq, bits FROM chunk_sketch WHERE 1{wh}"))?;
    let mut rows = st.query([])?;
    let mut near: BinaryHeap<(u32, i64)> = BinaryHeap::with_capacity(RESCORE + 1);
    while let Some(r) = rows.next()? {
        let b = r.get_ref(1)?.as_blob().unwrap_or_default();
        if b.len() != DIM / 8 {
            continue;
        }
        let d: u32 = b
            .as_chunks::<8>()
            .0
            .iter()
            .zip(&qb)
            .map(|(c, q)| (u64::from_le_bytes(*c) ^ q).count_ones())
            .sum();
        if near.len() < RESCORE {
            near.push((d, r.get(0)?));
        } else if near.peek().is_some_and(|worst| d < worst.0) {
            near.pop();
            near.push((d, r.get(0)?));
        }
    }
    let mut seqs: Vec<i64> = near.into_iter().map(|(_, s)| s).collect();
    seqs.sort_unstable();
    let mut st = db.prepare_cached("SELECT vec FROM chunk_vec WHERE seq = ?1")?;
    let mut scored = Vec::with_capacity(seqs.len());
    for seq in seqs {
        let s = st.query_row([seq], |r| {
            Ok(score_lanes(r.get_ref(0)?.as_blob().unwrap_or_default(), q))
        })?;
        if let Some(s) = s
            && s >= min
        {
            scored.push((s, seq));
        }
    }
    Ok(ptyro_meaning::top_k(scored, k))
}

/// The rows of a list's hits, as a search shows them (a hit by meaning shows its opening).
fn fetch(db: &Connection, best: &[(f32, i64)]) -> R<()> {
    let mut st = db.prepare_cached(
        "SELECT c.item, c.kind, c.heading, c.text, c.at_ms, i.title, i.project, i.profile
         FROM chunk c JOIN item i ON i.id = c.item WHERE c.seq = ?1",
    )?;
    for (_, seq) in best {
        let text: String = st.query_row([seq], |r| r.get(3))?;
        std::hint::black_box(ptyro_meaning::opening(&text, 160));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Plan {
    AsChat,
    Fast,
    Sketch,
}

/// §25.8's search: four lists (words and meaning, in each file), fused by rank, a page fetched.
fn merged(kb: &Connection, chat: &Connection, text: &str, q: &[f32], plan: Plan) -> R<()> {
    let shape = if plan == Plan::AsChat { Shape::OneQuery } else { Shape::TwoSteps };
    let mut lists: Vec<Vec<(u8, i64)>> = Vec::new();
    for (file, db) in [(0u8, kb), (1, chat)] {
        lists.push(words(db, text, "", shape)?.into_iter().map(|s| (file, s)).collect());
        let best = match plan {
            Plan::AsChat => scan(db, "", q, LIST, MIN_SCORE, ptyro_meaning::score)?,
            Plan::Fast => scan(db, "", q, LIST, MIN_SCORE, score_lanes)?,
            Plan::Sketch => sketch_scan(db, "", q, LIST, MIN_SCORE)?,
        };
        lists.push(best.into_iter().map(|(_, s)| (file, s)).collect());
    }
    // Reciprocal-rank fusion over every list (`ptyro_meaning::fuse` takes two; KB1 extends it).
    let mut by: HashMap<(u8, i64), f64> = HashMap::new();
    for list in &lists {
        for (rank, key) in list.iter().enumerate() {
            *by.entry(*key).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut all: Vec<((u8, i64), f64)> = by.into_iter().collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for ((file, seq), score) in all.into_iter().take(LIMIT) {
        fetch(if file == 0 { kb } else { chat }, &[(score as f32, seq)])?;
    }
    Ok(())
}

/// KB1.5: the same files searched by the code that ships: `KbDb::search_words` and
/// `search_meaning` (each list with its rows fetched and checked against their items),
/// `fuse_lists` over the four lists and `rank`. One connection a file, its writer's; no
/// `mmap_size`.
fn as_built(
    (kb, kb_path): (&Connection, &Path),
    (chat, chat_path): (&Connection, &Path),
    [topical, rare, mixed, common]: [&Vec<String>; 4],
    vecs: &[Vec<f32>],
    model: &Model,
) -> R<()> {
    // (The spike's schema is migration 1's tables; saying so lets `KbDb` open the files.)
    kb.pragma_update(None, "user_version", 1)?;
    chat.pragma_update(None, "user_version", 1)?;
    let rt = tokio::runtime::Builder::new_current_thread().build().expect("a runtime");
    let kbdb = KbDb::open(kb_path, None).expect("KbDb opens the spike's kb.db");
    let chatdb = KbDb::open(chat_path, None).expect("KbDb opens the stand-in");
    let fts = |q: &str| parse_kb_query(q, 0).expect("a query").fts;
    let timed = |n: usize, f: &mut dyn FnMut(usize)| -> Vec<f64> {
        (0..n)
            .map(|i| {
                let t = Instant::now();
                f(i);
                ms(t)
            })
            .collect()
    };
    let all = KbFilter::default;
    let work = || KbFilter { profile: Some("work".into()), ..KbFilter::default() };
    println!(
        "as built (KB1.5: KbDb's lists, best {LIST}, rows fetched; no more than {MATCH_CAP} \
         matches ranked): median / 95th / worst, ms"
    );
    for (name, list) in [
        ("two words of a topic", topical),
        ("two rare words", rare),
        ("a common and a topic word", mixed),
        ("one of the 20 commonest", common),
    ] {
        let over = list.iter().filter(|q| count_matches(kb, &fts(q)) > MATCH_CAP as i64).count();
        for (how, filter) in [("", all()), (", one profile", work())] {
            let times = timed(list.len(), &mut |i| {
                let hits = rt.block_on(kbdb.search_words(fts(&list[i]), filter.clone(), LIST));
                std::hint::black_box(hits.expect("a search by words"));
            });
            row(&format!("by words, {name}{how} ({over} of {} over the cap)", list.len()), &times);
        }
    }
    let docs = || KbFilter { kinds: vec![HitKind::Doc], ..KbFilter::default() };
    let since = || KbFilter { since_ms: Some((NOW - 30 * DAY) as u64), ..KbFilter::default() };
    for (name, filter) in [
        ("unfiltered", all()),
        ("one profile", work()),
        ("kind:doc (the items looked up first)", docs()),
        ("the last 30 days", since()),
    ] {
        let times = timed(vecs.len(), &mut |i| {
            let hits = rt.block_on(kbdb.search_meaning(
                vecs[i].clone(),
                MODEL.into(),
                String::new(),
                MIN_SCORE,
                filter.clone(),
                LIST,
            ));
            std::hint::black_box(hits.expect("a search by meaning"));
        });
        row(&format!("by meaning, {name}"), &times);
    }
    // The whole search: four lists, fused by rank, ranked, the best LIMIT.
    let merged = |text: &str, filter: &KbFilter| {
        let (f, v) = (fts(text), model.embed(text));
        let mut lists: Vec<(Found, Vec<KbHit>)> = Vec::new();
        for db in [&kbdb, &chatdb] {
            let words = rt.block_on(db.search_words(f.clone(), filter.clone(), LIST));
            lists.push((Found::Words, words.expect("a search by words")));
            let near = rt.block_on(db.search_meaning(
                v.clone(),
                MODEL.into(),
                String::new(),
                MIN_SCORE,
                filter.clone(),
                LIST,
            ));
            lists.push((Found::Meaning, near.expect("a search by meaning")));
        }
        // A hit in the lists: its file, its item, its chunk.
        type Key = (usize, String, u32);
        let key =
            |file: usize, h: &KbHit| -> Key { (file / 2, h.item.clone(), h.idx.unwrap_or(0)) };
        let keys: Vec<(Found, Vec<Key>)> = lists
            .iter()
            .enumerate()
            .map(|(n, (how, l))| (*how, l.iter().map(|h| key(n, h)).collect()))
            .collect();
        let order =
            fuse_lists(&keys.iter().map(|(how, l)| (*how, l.as_slice())).collect::<Vec<_>>());
        let mut by: HashMap<Key, KbHit> = HashMap::new();
        for (n, (_, list)) in lists.into_iter().enumerate() {
            for h in list {
                by.entry(key(n, &h)).or_insert(h);
            }
        }
        let fused: Vec<KbHit> = order
            .into_iter()
            .filter_map(|(k, how, score)| {
                let mut h = by.remove(&k)?;
                h.how = how.as_str().to_owned();
                h.score = score;
                Some(h)
            })
            .collect();
        std::hint::black_box(rank(fused, NOW as u64, None, LIMIT));
    };
    for (name, list, filter) in [
        ("two words of a topic", topical, all()),
        ("two words of a topic, one profile", topical, work()),
        ("one of the 20 commonest words", common, all()),
    ] {
        let times = timed(list.len(), &mut |i| merged(&list[i], &filter));
        row(&format!("the merged search as built, {name}"), &times);
    }
    Ok(())
}

// ── Sizes and printing ──────────────────────────────────────────────────────────────────────

fn sizes(db: &Connection, path: &Path, chunks: usize, vectors: usize) -> R<()> {
    let file = std::fs::metadata(path).map_or(0, |m| m.len());
    println!(
        "size on disk              {:>8.1} MB  ({:.0} B a chunk)",
        file as f64 / 1e6,
        file as f64 / chunks as f64
    );
    let parts: Vec<(String, i64)> = match db
        .prepare("SELECT name, sum(pgsize) FROM dbstat GROUP BY name")
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
    {
        Ok(p) => p,
        Err(e) => {
            println!("  (no dbstat in this SQLite, so no split: {e})");
            return Ok(());
        }
    };
    let sum = |pick: &dyn Fn(&str) -> bool| -> f64 {
        parts.iter().filter(|(n, _)| pick(n)).map(|(_, b)| *b as f64).sum::<f64>() / 1e6
    };
    let text: i64 = db.query_row("SELECT sum(length(text)) FROM chunk", [], |r| r.get(0))?;
    println!(
        "  rows (item, chunk)      {:>8.1} MB  ({:.1} MB of it is the chunks' text)",
        sum(&|n| n == "item" || n == "chunk"),
        text as f64 / 1e6
    );
    println!(
        "  their indexes           {:>8.1} MB",
        sum(&|n| n.starts_with("sqlite_autoindex") || n.starts_with("item_by"))
    );
    println!("  the words' index (FTS5) {:>8.1} MB", sum(&|n| n.starts_with("chunk_fts")));
    println!(
        "  vectors (chunk_vec)     {:>8.1} MB  ({:.0} B a vector, {} of them)",
        sum(&|n| n == "chunk_vec"),
        sum(&|n| n == "chunk_vec") * 1e6 / vectors.max(1) as f64,
        vectors
    );
    println!(
        "  sketches (the spike's)  {:>8.1} MB  ({:.0} B a vector)",
        sum(&|n| n == "chunk_sketch"),
        sum(&|n| n == "chunk_sketch") * 1e6 / vectors.max(1) as f64
    );
    Ok(())
}

fn time_each<T>(items: &[T], mut f: impl FnMut(&T) -> R<()>) -> R<Vec<f64>> {
    let mut times = Vec::with_capacity(items.len());
    for item in items {
        let t = Instant::now();
        f(item)?;
        times.push(ms(t));
    }
    Ok(times)
}

fn row(name: &str, times: &[f64]) {
    let mut t = times.to_vec();
    t.sort_by(f64::total_cmp);
    let at = |p: f64| t[((t.len() as f64 * p).ceil() as usize).clamp(1, t.len()) - 1];
    println!("  {name:<68} {:>8.1} {:>8.1} {:>8.1}", at(0.5), at(0.95), at(1.0));
}

fn median_of(values: impl Iterator<Item = i64>) -> i64 {
    let mut v: Vec<i64> = values.collect();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

fn percent(found: usize, wanted: usize) -> String {
    if wanted == 0 {
        return "nothing (none were wanted)".into();
    }
    format!("{:.1}% ({found} of {wanted})", 100.0 * found as f64 / wanted as f64)
}

/// Whether `needle` is anywhere in the file's bytes.
fn contains(path: &Path, needle: &[u8]) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = vec![0u8; 1 << 20];
    let mut kept = 0;
    loop {
        let Ok(n) = f.read(&mut buf[kept..]) else { return false };
        if n == 0 {
            return false;
        }
        let end = kept + n;
        if buf[..end].windows(needle.len()).any(|w| w == needle) {
            return true;
        }
        kept = needle.len().min(end);
        buf.copy_within(end - kept..end, 0);
    }
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
