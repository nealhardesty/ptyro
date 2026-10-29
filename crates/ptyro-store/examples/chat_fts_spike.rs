//! CHAT0 spike (ROADMAP CHAT0, SPEC §22.4, §22.10): is FTS5 in the bundled SQLite, does the
//! `porter unicode61 remove_diacritics 2` tokenizer behave as §22.10 needs, and what does an index
//! over `NODES` chat nodes cost in size and time? Run with `make spike-chat-fts`
//! (`NODES=100000` by default; `NODES=500000 make spike-chat-fts` for the long case).
//!
//! The schema is the one of §22.4 (external-content `node_fts`, kept in step by triggers), so a
//! pass here is a pass for CHAT1.1.

use std::time::Instant;

use rusqlite::{Connection, params};

const SCHEMA: &str = "
CREATE TABLE chat (id TEXT PRIMARY KEY, workspace TEXT NOT NULL, title TEXT, agent TEXT NOT NULL,
                   profile TEXT NOT NULL, created_at INTEGER);
CREATE TABLE node (
  seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
  chat TEXT NOT NULL REFERENCES chat(id) ON DELETE CASCADE, parent TEXT, kind TEXT NOT NULL,
  body JSON NOT NULL, text TEXT, at_ms INTEGER NOT NULL, conv TEXT, origin TEXT NOT NULL, deleted_at INTEGER);
CREATE INDEX node_by_chat ON node(chat, at_ms);
CREATE INDEX node_by_parent ON node(parent);
CREATE VIRTUAL TABLE node_fts USING fts5(text, content='node', content_rowid='seq',
                                         tokenize='porter unicode61 remove_diacritics 2');
CREATE TRIGGER node_ai AFTER INSERT ON node WHEN new.text IS NOT NULL AND new.deleted_at IS NULL BEGIN
  INSERT INTO node_fts(rowid, text) VALUES (new.seq, new.text);
END;
CREATE TRIGGER node_ad AFTER DELETE ON node WHEN old.text IS NOT NULL AND old.deleted_at IS NULL BEGIN
  INSERT INTO node_fts(node_fts, rowid, text) VALUES ('delete', old.seq, old.text);
END;
CREATE TRIGGER node_au AFTER UPDATE OF text, deleted_at ON node BEGIN
  INSERT INTO node_fts(node_fts, rowid, text) SELECT 'delete', old.seq, old.text
    WHERE old.text IS NOT NULL AND old.deleted_at IS NULL;
  INSERT INTO node_fts(rowid, text) SELECT new.seq, new.text
    WHERE new.text IS NOT NULL AND new.deleted_at IS NULL;
END;";

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A made-up vocabulary of `n` words with a Zipf-like draw, so common words are common and most
/// words are rare, as in prose.
struct Vocab(Vec<String>);
impl Vocab {
    fn new(n: usize, rng: &mut Rng) -> Self {
        const SYL: &[&str] = &[
            "ka", "lo", "mi", "ter", "an", "re", "po", "sin", "dal", "ve", "or", "tu", "ban",
            "cle", "dis", "ex", "fra", "gen", "hy", "ing", "jo", "ku", "lem", "mon", "nu", "ob",
            "pre", "qui", "ras", "sto", "tion",
        ];
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let k = 1 + rng.below(3) as usize;
            v.push((0..k).map(|_| SYL[rng.below(SYL.len() as u64) as usize]).collect::<String>());
        }
        Vocab(v)
    }
    fn word(&self, rng: &mut Rng) -> &str {
        // squaring a uniform draw skews it to the front of the list
        let u = rng.below(1_000_000) as f64 / 1_000_000.0;
        &self.0[((u * u * u) * self.0.len() as f64) as usize]
    }
    fn text(&self, rng: &mut Rng, words: u64) -> String {
        (0..words).map(|_| self.word(rng)).collect::<Vec<_>>().join(" ")
    }
}

struct Report(u32);
impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        println!("  {} {what}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.0 += 1;
        }
    }
}

fn hits(db: &Connection, q: &str) -> rusqlite::Result<i64> {
    db.query_row("SELECT count(*) FROM node_fts WHERE node_fts MATCH ?1", [q], |r| r.get(0))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nodes: u64 = std::env::var("NODES").ok().and_then(|n| n.parse().ok()).unwrap_or(100_000);
    let mut rep = Report(0);
    let dir = std::env::temp_dir().join(format!("ptyro-chat-fts-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("chat.db");
    let db = Connection::open(&path)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;

    println!("sqlite {}", rusqlite::version());
    let opts: Vec<String> = db
        .prepare("PRAGMA compile_options")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    println!(
        "  compile options with FTS/UNICODE/THREAD: {:?}",
        opts.iter()
            .filter(|o| o.contains("FTS") || o.contains("UNICODE") || o.contains("THREADSAFE"))
            .collect::<Vec<_>>()
    );
    rep.check(opts.iter().any(|o| o == "ENABLE_FTS5"), "ENABLE_FTS5 is compiled in");
    db.execute_batch(SCHEMA)?;
    rep.check(true, "the §22.4 schema, with `porter unicode61 remove_diacritics 2`, creates");

    println!("tokenizer behaviour (§22.10):");
    db.execute("INSERT INTO chat VALUES ('c1','w','t','claude','p',0)", [])?;
    let say = |db: &Connection, id: &str, text: &str| -> rusqlite::Result<usize> {
        db.execute(
            "INSERT INTO node(id, chat, kind, body, text, at_ms, origin) VALUES (?1,'c1','user','{}',?2,0,'m')",
            params![id, text],
        )
    };
    say(&db, "n1", "We were running the Café migration plan, and the retry policy needs backoff")?;
    say(&db, "n2", "Rust lifetimes and borrowing: the borrow checker is strict")?;
    say(&db, "n3", "dinner ideas: pasta with 日本語 mushrooms")?;
    rep.check(hits(&db, "run")? == 1, "stemming: `run` finds `running`");
    rep.check(hits(&db, "cafe")? == 1, "diacritics: `cafe` finds `Café`");
    rep.check(hits(&db, "CAFÉ")? == 1, "case and accents: `CAFÉ` finds `Café`");
    rep.check(hits(&db, "\"retry policy\"")? == 1, "phrase: \"retry policy\"");
    rep.check(hits(&db, "retry policy")? == 1, "AND: `retry policy`");
    rep.check(hits(&db, "migrat*")? == 1, "prefix: `migrat*`");
    rep.check(hits(&db, "borrow NOT migration")? == 1, "NOT: `borrow NOT migration`");
    rep.check(hits(&db, "lifetime")? == 1, "stemming: `lifetime` finds `lifetimes`");
    rep.check(
        hits(&db, "日本語")? == 1,
        "CJK text is indexed as a token (no segmentation: whole run is one token)",
    );
    rep.check(
        hits(&db, "日本")? == 0,
        "…and a part of a CJK run does not match (known limit of unicode61; trigram would)",
    );
    let snip: String = db.query_row(
        "SELECT snippet(node_fts, 0, '[', ']', '…', 6) FROM node_fts WHERE node_fts MATCH 'backoff'",
        [],
        |r| r.get(0),
    )?;
    println!("  snippet: {snip}");
    rep.check(snip.contains("[backoff]"), "snippet() highlights the match");
    let rank: f64 = db.query_row(
        "SELECT bm25(node_fts) FROM node_fts WHERE node_fts MATCH 'borrow'",
        [],
        |r| r.get(0),
    )?;
    rep.check(rank < 0.0, "bm25() ranks (lower, negative, is better)");
    // update / tombstone / delete keep the index in step
    db.execute("UPDATE node SET deleted_at = 1 WHERE id = 'n2'", [])?;
    rep.check(hits(&db, "borrow")? == 0, "a tombstoned node leaves the index");
    db.execute("UPDATE node SET deleted_at = NULL WHERE id = 'n2'", [])?;
    rep.check(hits(&db, "borrow")? == 1, "…and comes back when restored");
    db.execute("UPDATE node SET text = 'edited words' WHERE id = 'n1'", [])?;
    rep.check(hits(&db, "backoff")? == 0 && hits(&db, "edited")? == 1, "an edit re-indexes");
    db.execute("DELETE FROM node WHERE id = 'n3'", [])?;
    rep.check(hits(&db, "dinner")? == 0, "a delete leaves the index");
    db.execute("DELETE FROM chat WHERE id = 'c1'", [])?;
    rep.check(
        hits(&db, "edited")? == 0,
        "ON DELETE CASCADE of a chat drops its nodes from the index",
    );
    db.execute_batch("INSERT INTO node_fts(node_fts) VALUES('integrity-check');")?;
    rep.check(true, "FTS5 integrity-check passes after all that");

    println!("scale: {nodes} nodes in ~{} chats:", nodes / 40);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let vocab = Vocab::new(20_000, &mut rng);
    let chats = (nodes / 40).max(1);
    let t0 = Instant::now();
    {
        let tx = db.unchecked_transaction()?;
        {
            let mut c = tx.prepare("INSERT INTO chat VALUES (?1,?2,?3,?4,?5,?6)")?;
            for i in 0..chats {
                c.execute(params![
                    format!("c{i}"),
                    format!("w{}", i % 3),
                    vocab.text(&mut rng, 4),
                    "claude",
                    format!("p{}", i % 2),
                    i as i64
                ])?;
            }
            let mut n = tx.prepare(
                "INSERT INTO node(id, chat, parent, kind, body, text, at_ms, origin) VALUES (?1,?2,?3,?4,?5,?6,?7,'m')",
            )?;
            let mut total_chars = 0usize;
            for i in 0..nodes {
                let user = i % 2 == 0;
                let words = if user { 8 + rng.below(40) } else { 40 + rng.below(260) };
                let text = vocab.text(&mut rng, words);
                total_chars += text.len();
                // the body is the JSON the chat view renders: about twice the text
                let body = format!(
                    "{{\"kind\":\"{}\",\"text\":\"{text}\",\"meta\":{{\"model\":\"x\",\"tokens\":{words}}}}}",
                    if user { "user" } else { "assistant" }
                );
                n.execute(params![
                    format!("n{i}"),
                    format!("c{}", i / 40),
                    if i % 40 == 0 { None } else { Some(format!("n{}", i - 1)) },
                    if user { "user" } else { "assistant" },
                    body,
                    text,
                    i as i64,
                ])?;
            }
            println!(
                "  text: {:.1} MB over {nodes} nodes (avg {} chars)",
                total_chars as f64 / 1e6,
                total_chars as u64 / nodes
            );
        }
        tx.commit()?;
    }
    let load = t0.elapsed();
    println!(
        "  load with triggers: {:.1} s ({:.0} nodes/s)",
        load.as_secs_f64(),
        nodes as f64 / load.as_secs_f64()
    );
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let size =
        |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
    let (pages, page_size): (i64, i64) = (
        db.query_row("PRAGMA page_count", [], |r| r.get(0))?,
        db.query_row("PRAGMA page_size", [], |r| r.get(0))?,
    );
    println!("  chat.db: {:.1} MB ({pages} pages of {page_size})", size(&path));
    // the index alone: the FTS shadow tables' share
    let fts_bytes: Option<i64> = db
        .query_row("SELECT sum(pgsize) FROM dbstat WHERE name LIKE 'node_fts%'", [], |r| r.get(0))
        .ok();
    match fts_bytes {
        Some(b) => println!("  of which node_fts: {:.1} MB", b as f64 / 1e6),
        None => {
            println!("  (dbstat is not compiled in; the index share is not measured separately)")
        }
    }

    let q = |db: &Connection, label: &str, sql: &str, arg: &str| -> rusqlite::Result<f64> {
        let mut stmt = db.prepare(sql)?;
        let mut times = Vec::new();
        let mut n = 0usize;
        for _ in 0..9 {
            let t = Instant::now();
            n = stmt.query_map([arg], |r| r.get::<_, i64>(0))?.count();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(|a, b| a.total_cmp(b));
        println!("  {label}: median {:.1} ms, worst {:.1} ms, {n} rows", times[4], times[8]);
        Ok(times[4])
    };
    // the words of the draw: the head of the list is common, the tail rare
    let common = vocab.0[0].clone();
    let rare = vocab.0[vocab.0.len() - 1].clone();
    let mid = vocab.0[vocab.0.len() / 20].clone();
    let ranked = "SELECT n.seq FROM node_fts f JOIN node n ON n.seq = f.rowid WHERE node_fts MATCH ?1 ORDER BY bm25(node_fts) LIMIT 20";
    let t_common = q(&db, &format!("common word `{common}`, top 20 by bm25"), ranked, &common)?;
    let t_mid = q(&db, &format!("middling word `{mid}`, top 20 by bm25"), ranked, &mid)?;
    let t_rare = q(&db, &format!("rare word `{rare}`, top 20 by bm25"), ranked, &rare)?;
    let t_phrase = q(&db, "a two-word phrase, top 20", ranked, &format!("\"{mid} {common}\""))?;
    let t_prefix = q(&db, "a prefix, top 20", ranked, &format!("{}*", &mid[..2.min(mid.len())]))?;
    let filtered = "SELECT n.seq FROM node_fts f JOIN node n ON n.seq = f.rowid JOIN chat c ON c.id = n.chat \
                    WHERE node_fts MATCH ?1 AND c.profile = 'p0' AND c.workspace = 'w1' ORDER BY bm25(node_fts) LIMIT 20";
    let t_filter = q(&db, "common word with profile and workspace filters", filtered, &common)?;
    let snippeted = "SELECT length(snippet(node_fts, 0, '[', ']', '…', 12)) FROM node_fts WHERE node_fts MATCH ?1 ORDER BY bm25(node_fts) LIMIT 20";
    let t_snip = q(&db, "common word with snippet(), top 20", snippeted, &common)?;
    rep.check(
        [t_common, t_mid, t_rare, t_phrase, t_prefix, t_filter, t_snip].iter().all(|t| *t < 250.0),
        "every search answers in under 250 ms (a keystroke-fast search box needs about 100)",
    );

    let t = Instant::now();
    db.execute_batch("INSERT INTO node_fts(node_fts) VALUES('optimize');")?;
    println!("  optimize: {:.1} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    db.execute_batch("INSERT INTO node_fts(node_fts) VALUES('rebuild');")?;
    println!(
        "  rebuild (what a migration or `doctor --fix` costs): {:.1} s",
        t.elapsed().as_secs_f64()
    );
    let t = Instant::now();
    db.execute_batch("INSERT INTO node_fts(node_fts) VALUES('integrity-check');")?;
    println!("  integrity-check: {:.1} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let ok: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    println!("  PRAGMA integrity_check: {ok}, {:.1} s", t.elapsed().as_secs_f64());
    rep.check(ok == "ok", "integrity_check ok");

    // a streaming node: one row inserted at the end, then 2 s flushes that replace its text (§22.4)
    let t = Instant::now();
    for i in 0..50 {
        db.execute("UPDATE node SET text = ?1 WHERE id = 'n1'", [vocab.text(&mut rng, 40 + i)])?;
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / 50.0;
    println!("  re-indexing a streaming node (an update through the triggers): {per:.2} ms each");
    rep.check(per < 20.0, "a 2 s flush of a streaming node costs well under 20 ms");

    std::fs::remove_dir_all(&dir)?;
    if rep.0 > 0 {
        return Err(format!("{} check(s) failed", rep.0).into());
    }
    println!("all good");
    Ok(())
}
