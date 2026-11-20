//! Searching `kb.db` (SPEC §25.8): the list a query's words find and the list its meaning
//! finds, each best first, and the vectors' upkeep. Merging the lists and ranking them is not
//! here (`ptyro_meaning::fuse_lists`, `ptyro_core::kb::search::rank`).
//!
//! Both lists are made the way KB0.1 measured at 500k and a million chunks:
//!
//! - **By words, in two steps.** The index names the best rows (`ORDER BY rank LIMIT`), then
//!   each kept row gets its snippet. One statement with the joins and `snippet()` in it makes
//!   a snippet of every match before it sorts.
//! - **By meaning, one pass over `chunk_vec`** with nothing joined: its own columns carry the
//!   profile, the project, the kind and the time. What only the item knows (its root, its
//!   writer, a path) is looked up once, before the pass.
//!
//! **A forgotten item is never found**: its tombstone is written before its chunks go, and
//! every row is checked against its item before it is returned. That last check is also what
//! holds the wall between profiles, whatever a vector's own columns say.

use std::collections::HashSet;

use ptyro_core::kb::search::{HitKind, KbHit};
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};

use super::{KbDb, word};
use crate::Result;

/// The most matches a search by words ranks (SPEC §25.8). FTS5 scores every match before it
/// can name the best, so a word that a large part of all chunks hold costs by the match: a
/// query with more matches than this is ranked among its newest ones only.
pub const MATCH_CAP: usize = 20_000;

/// What a search may find. Everything empty: all of it but what was forgotten.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KbFilter {
    /// Only these kinds (`HitKind::Chat` is not in this file); empty: every kind.
    pub kinds: Vec<HitKind>,
    /// A project's id.
    pub project: Option<String>,
    /// A root's id.
    pub root: Option<String>,
    /// **The wall** (§25.9): that profile's items and those of no profile. `None`: everyone's.
    pub profile: Option<String>,
    /// Only that workspace's items (its id): `workspace:` in a query, and the wall's
    /// narrower setting.
    pub workspace: Option<String>,
    pub agent: Option<String>,
    pub writer: Option<Writer>,
    /// A part of a path: a file's own, or one a session or a commit touched.
    pub file: Option<String>,
    /// By a chunk's time.
    pub since_ms: Option<u64>,
    pub before_ms: Option<u64>,
    /// A ptyro session whose own item is left out (an agent is not shown what it is saying).
    pub not_session: Option<String>,
}

/// A filter as SQL, and its arguments in order (`?` placeholders, numbered by [`numbered`]).
#[derive(Default)]
struct Cond {
    sql: String,
    args: Vec<Value>,
}

impl Cond {
    fn and(&mut self, sql: &str, args: impl IntoIterator<Item = Value>) {
        self.sql.push_str(" AND ");
        self.sql.push_str(sql);
        self.args.extend(args);
    }
}

/// `?` → `?N`, from `first` on.
fn numbered(sql: &str, first: usize) -> String {
    let mut n = first;
    let mut out = String::with_capacity(sql.len() + 8);
    for ch in sql.chars() {
        out.push(ch);
        if ch == '?' {
            out.push_str(&n.to_string());
            n += 1;
        }
    }
    out
}

/// A list of stored words for `IN (…)`. (Their own words, never a caller's text.)
fn quoted<T: std::fmt::Display>(words: impl IntoIterator<Item = T>) -> String {
    words.into_iter().map(|w| format!("'{w}'")).collect::<Vec<_>>().join(", ")
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

impl KbFilter {
    /// The kinds this file holds, each as its item kind and chunk kinds.
    fn stored(&self) -> Vec<(ItemKind, &'static [ChunkKind])> {
        self.kinds.iter().filter_map(|k| k.stored()).collect()
    }

    /// Kinds were asked for, and none of them is in this file.
    fn nothing_here(&self) -> bool {
        !self.kinds.is_empty() && self.stored().is_empty()
    }

    /// What must hold of the item (`item i`).
    fn item(&self) -> Cond {
        let mut c = Cond::default();
        let kinds = self.stored();
        if !kinds.is_empty() {
            c.and(&format!("i.kind IN ({})", quoted(kinds.iter().map(|k| k.0))), []);
        }
        if let Some(p) = &self.project {
            c.and("i.project = ?", [text(p)]);
        }
        if let Some(r) = &self.root {
            c.and("i.root = ?", [text(r)]);
        }
        if let Some(p) = &self.profile {
            c.and("(i.profile = ? OR i.profile IS NULL)", [text(p)]);
        }
        if let Some(w) = &self.workspace {
            c.and("i.workspace = ?", [text(w)]);
        }
        if let Some(a) = &self.agent {
            c.and("i.agent = ?", [text(a)]);
        }
        if let Some(w) = self.writer {
            c.and(&format!("i.writer = '{w}'"), []);
        }
        if let Some(f) = &self.file {
            c.and(
                "(instr(COALESCE(i.rel, ''), ?) > 0 OR (i.kind = 'file' AND instr(i.uri, ?) > 0)
                  OR instr(COALESCE(i.files, ''), ?) > 0)",
                [text(f), text(f), text(f)],
            );
        }
        if let Some(s) = &self.not_session {
            // (A note it wrote is not what it is saying: it finds its own notes.)
            c.and("(i.session IS NOT ? OR i.kind = 'note')", [text(s)]);
        }
        c
    }

    /// What must hold of the item and of the chunk (`item i`, `chunk c`): everything.
    fn row(&self) -> Cond {
        let mut c = self.item();
        let kinds = self.stored();
        if kinds.iter().any(|(_, chunks)| !chunks.is_empty()) {
            let each: Vec<String> = kinds
                .iter()
                .map(|(item, chunks)| match chunks.is_empty() {
                    true => format!("i.kind = '{item}'"),
                    false => format!("(i.kind = '{item}' AND c.kind IN ({}))", quoted(*chunks)),
                })
                .collect();
            c.and(&format!("({})", each.join(" OR ")), []);
        }
        if let Some(t) = self.since_ms {
            c.and("c.at_ms >= ?", [Value::Integer(t as i64)]);
        }
        if let Some(t) = self.before_ms {
            c.and("c.at_ms < ?", [Value::Integer(t as i64)]);
        }
        c
    }

    /// What `chunk_vec`'s own columns can say of it, so that the pass joins nothing.
    /// `None`: no vector can match (only kinds that have none were asked for).
    fn vec(&self) -> Option<Cond> {
        let mut c = Cond::default();
        let kinds = self.stored();
        if !kinds.is_empty() {
            let mut chunks: Vec<ChunkKind> = Vec::new();
            for (item, of) in &kinds {
                let all: &[ChunkKind] = match item {
                    ItemKind::Session => &[
                        ChunkKind::Card,
                        ChunkKind::User,
                        ChunkKind::Assistant,
                        ChunkKind::Summary,
                    ],
                    ItemKind::Note => &[ChunkKind::Text],
                    ItemKind::Commit => &[ChunkKind::Message],
                    ItemKind::File | ItemKind::Shell => &[],
                };
                let of = if of.is_empty() { all } else { of };
                chunks.extend(of.iter().filter(|k| k.prose()));
            }
            chunks.sort();
            chunks.dedup();
            if chunks.is_empty() {
                return None;
            }
            c.and(&format!("kind IN ({})", quoted(chunks)), []);
        }
        if let Some(p) = &self.project {
            c.and("project = ?", [text(p)]);
        }
        if let Some(p) = &self.profile {
            c.and("(profile = ? OR profile IS NULL)", [text(p)]);
        }
        if let Some(t) = self.since_ms {
            c.and("at_ms >= ?", [Value::Integer(t as i64)]);
        }
        if let Some(t) = self.before_ms {
            c.and("at_ms < ?", [Value::Integer(t as i64)]);
        }
        Some(c)
    }

    /// Whether something only the item knows is asked (so the pass over the vectors needs
    /// the items it may return).
    fn asks_the_item(&self) -> bool {
        !self.kinds.is_empty()
            || self.root.is_some()
            || self.workspace.is_some()
            || self.agent.is_some()
            || self.writer.is_some()
            || self.file.is_some()
            || self.not_session.is_some()
    }
}

/// A chunk with no vector of the current model yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VecTodo {
    pub seq: i64,
    pub text: String,
}

/// A hit's columns, after whatever comes first (the snippet, or the text): `c.seq` and on.
const HIT_COLS: &str = "c.seq, c.item, c.idx, c.kind, c.heading, c.at_ms, i.uri, i.kind, i.title, \
                        i.project, i.workspace, i.profile, i.agent, i.writer";

/// A hit from a row of `<snippet>, HIT_COLS`. Its address is the item's whole id and the
/// chunk's index (`k:<id>/<idx>`; the daemon shortens the id to its handle), its score 0.
fn hit_row(r: &rusqlite::Row<'_>, snippet: String, how: &str) -> rusqlite::Result<KbHit> {
    let item: String = r.get(2)?;
    let idx = r.get::<_, i64>(3)?.max(0) as u32;
    let chunk = word(4, &r.get::<_, String>(4)?, ChunkKind::parse)?;
    let kind = word(8, &r.get::<_, String>(8)?, ItemKind::parse)?;
    Ok(KbHit {
        addr: format!("k:{item}/{idx}"),
        item,
        idx: Some(idx),
        node: None,
        uri: Some(r.get(7)?),
        kind: HitKind::of(kind, chunk),
        part: chunk.as_str().to_owned(),
        title: r.get(9)?,
        heading: r.get(5)?,
        snippet,
        at_ms: r.get::<_, i64>(6)?.max(0) as u64,
        project: r.get(10)?,
        project_name: None,
        workspace_name: None,
        workspace: r.get(11)?,
        profile: r.get(12)?,
        agent: r.get(13)?,
        writer: Some(word(14, &r.get::<_, String>(14)?, Writer::parse)?),
        machine: None,
        how: how.to_owned(),
        score: 0.0,
    })
}

impl KbDb {
    /// The best `limit` chunks by words, best first (bm25, a match in a heading at twice one
    /// in the text): `fts` is an FTS5 expression as `chats::parse_kb_query` makes it. Each
    /// hit's snippet marks the matches. A query with more than [`MATCH_CAP`] matches is
    /// ranked among its newest [`MATCH_CAP`].
    pub async fn search_words(
        &self,
        fts: String,
        filter: KbFilter,
        limit: usize,
    ) -> Result<Vec<KbHit>> {
        self.call(move |db| words(db, &fts, &filter, limit, MATCH_CAP)).await
    }

    /// The chunks nearest in meaning to the unit vector `q`: the best `limit` with a cosine
    /// of at least `min`, best first. `not` is an FTS5 expression of the words left out
    /// (`-word`): a chunk with one is not a hit either. Each hit's snippet is its opening.
    pub async fn search_meaning(
        &self,
        q: Vec<f32>,
        model: String,
        not: String,
        min: f32,
        filter: KbFilter,
        limit: usize,
    ) -> Result<Vec<KbHit>> {
        self.call(move |db| meaning(db, &q, &model, &not, min, &filter, limit)).await
    }

    /// Up to `limit` prose chunks with no vector of `model` whose `seq` is above `above` and
    /// below `below`, newest first (the backfill walks down the table, as chat's does).
    pub async fn vec_todo(
        &self,
        model: String,
        (above, below): (i64, i64),
        limit: usize,
    ) -> Result<Vec<VecTodo>> {
        self.call(move |db| {
            let mut st = db.prepare_cached(&format!(
                "SELECT c.seq, c.text FROM chunk c
                 WHERE c.seq > ?2 AND c.seq < ?3 AND c.kind IN ({PROSE})
                   AND NOT EXISTS (SELECT 1 FROM chunk_vec v WHERE v.seq = c.seq AND v.model = ?1)
                 ORDER BY c.seq DESC LIMIT ?4"
            ))?;
            let rows = st.query_map(rusqlite::params![model, above, below, limit as i64], |r| {
                Ok(VecTodo { seq: r.get(0)?, text: r.get(1)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// One item's prose chunks with no vector of `model` (what an ingest just wrote).
    pub async fn vec_todo_of(&self, item: String, model: String) -> Result<Vec<VecTodo>> {
        self.call(move |db| {
            let mut st = db.prepare_cached(&format!(
                "SELECT c.seq, c.text FROM chunk c
                 WHERE c.item = ?1 AND c.kind IN ({PROSE})
                   AND NOT EXISTS (SELECT 1 FROM chunk_vec v WHERE v.seq = c.seq AND v.model = ?2)
                 ORDER BY c.idx"
            ))?;
            let rows =
                st.query_map([item, model], |r| Ok(VecTodo { seq: r.get(0)?, text: r.get(1)? }))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// The newest chunk's `seq`: where a backfill pass ends.
    pub async fn vec_top(&self) -> Result<i64> {
        self.call(|db| {
            Ok(db.query_row("SELECT coalesce(max(seq), 0) FROM chunk", [], |r| r.get(0))?)
        })
        .await
    }

    /// Store vectors (`(seq, the text embedded, vec)`), each with its item's profile and
    /// project and its chunk's kind and time. One whose chunk has gone or changed meanwhile,
    /// or whose item was forgotten, is dropped.
    pub async fn put_vecs(&self, model: String, rows: Vec<(i64, String, Vec<u8>)>) -> Result<()> {
        self.call(move |db| {
            let tx = db.transaction()?;
            {
                let mut st = tx.prepare_cached(
                    "INSERT OR REPLACE INTO chunk_vec (seq, item, kind, profile, project, at_ms, model, vec)
                     SELECT c.seq, c.item, c.kind, i.profile, i.project, c.at_ms, ?2, ?3
                     FROM chunk c JOIN item i ON i.id = c.item
                     WHERE c.seq = ?1 AND c.text IS ?4 AND i.forgotten_at IS NULL",
                )?;
                for (seq, text, vec) in rows {
                    st.execute(rusqlite::params![seq, model, vec, text])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// How many prose chunks have a vector of `model`, of how many.
    pub async fn vec_counts(&self, model: String) -> Result<(u64, u64)> {
        self.call(move |db| {
            let all: i64 = db.query_row(
                &format!("SELECT count(*) FROM chunk WHERE kind IN ({PROSE})"),
                [],
                |r| r.get(0),
            )?;
            let done: i64 =
                db.query_row("SELECT count(*) FROM chunk_vec WHERE model = ?1", [model], |r| {
                    r.get(0)
                })?;
            Ok((done as u64, all as u64))
        })
        .await
    }

    /// Vectors of any model but `model` (one ptyro no longer uses) go.
    pub async fn drop_other_vecs(&self, model: String) -> Result<usize> {
        self.call(move |db| Ok(db.execute("DELETE FROM chunk_vec WHERE model IS NOT ?1", [model])?))
            .await
    }
}

/// The chunk kinds that get a vector (`ChunkKind::prose`), for `IN (…)`.
const PROSE: &str = "'card', 'user', 'assistant', 'summary', 'text', 'comment', 'message'";

pub(super) fn words(
    db: &Connection,
    fts: &str,
    f: &KbFilter,
    limit: usize,
    cap: usize,
) -> Result<Vec<KbHit>> {
    if fts.is_empty() || limit == 0 || f.nothing_here() {
        return Ok(Vec::new());
    }
    // More matches than the cap: only the newest `cap` are ranked. (Walking the matches
    // newest first without scoring them is cheap; it is the scoring that costs.)
    let newest: Option<i64> = db
        .prepare_cached(
            "SELECT rowid FROM chunk_fts WHERE chunk_fts MATCH ?1 ORDER BY rowid DESC LIMIT 1 OFFSET ?2",
        )?
        .query_row(rusqlite::params![fts, cap as i64], |r| r.get(0))
        .optional()?;
    let cond = f.row();
    // Step 1: the index ranks. A filter joins here, so the best are the best of what may be
    // found.
    let mut args =
        vec![text(fts), Value::Integer(limit as i64), Value::Integer(newest.unwrap_or(-1))];
    let seqs: Vec<i64> = if cond.sql.is_empty() {
        db.prepare_cached(
            "SELECT rowid FROM chunk_fts WHERE chunk_fts MATCH ?1 AND rowid > ?3
             ORDER BY rank LIMIT ?2",
        )?
        .query_map(params_from_iter(&args), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?
    } else {
        args.extend(cond.args.iter().cloned());
        db.prepare_cached(&format!(
            "SELECT chunk_fts.rowid FROM chunk_fts
             JOIN chunk c ON c.seq = chunk_fts.rowid JOIN item i ON i.id = c.item
             WHERE chunk_fts MATCH ?1 AND chunk_fts.rowid > ?3 AND i.forgotten_at IS NULL{}
             ORDER BY chunk_fts.rank LIMIT ?2",
            numbered(&cond.sql, 4)
        ))?
        .query_map(params_from_iter(&args), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?
    };
    // Step 2: a snippet for each row kept, and its item: never a forgotten one's.
    let mut st = db.prepare_cached(&format!(
        "SELECT snippet(chunk_fts, 0, char(2), char(3), '…', 16), {HIT_COLS}
         FROM chunk_fts JOIN chunk c ON c.seq = chunk_fts.rowid JOIN item i ON i.id = c.item
         WHERE chunk_fts MATCH ?1 AND chunk_fts.rowid = ?2 AND i.forgotten_at IS NULL"
    ))?;
    let mut hits = Vec::with_capacity(seqs.len());
    for seq in seqs {
        let hit = st
            .query_row(rusqlite::params![fts, seq], |r| hit_row(r, r.get(0)?, "words"))
            .optional()?;
        hits.extend(hit);
    }
    Ok(hits)
}

fn meaning(
    db: &Connection,
    q: &[f32],
    model: &str,
    not: &str,
    min: f32,
    f: &KbFilter,
    limit: usize,
) -> Result<Vec<KbHit>> {
    let Some(vec) = f.vec() else { return Ok(Vec::new()) };
    if limit == 0 || f.nothing_here() {
        return Ok(Vec::new());
    }
    let ids = |sql: &str, args: &[Value]| -> Result<HashSet<String>> {
        Ok(db
            .prepare(sql)?
            .query_map(params_from_iter(args), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    // What only the item knows, looked up once: the items a hit may be of. With nothing
    // asked of the item, the items a hit may *not* be of: those forgotten whose chunks are
    // still going (usually none).
    let item = f.item();
    let (allowed, gone) = if f.asks_the_item() {
        let sql = format!(
            "SELECT i.id FROM item i WHERE i.forgotten_at IS NULL{}",
            numbered(&item.sql, 1)
        );
        let allowed = ids(&sql, &item.args)?;
        if allowed.is_empty() {
            return Ok(Vec::new());
        }
        (Some(allowed), HashSet::new())
    } else {
        (None, ids("SELECT id FROM item WHERE forgotten_at IS NOT NULL AND chunks > 0", &[])?)
    };
    let ruled_out: HashSet<i64> = if !not.is_empty() {
        db.prepare("SELECT rowid FROM chunk_fts WHERE chunk_fts MATCH ?1")?
            .query_map([not], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        HashSet::new()
    };
    let check_item = allowed.is_some() || !gone.is_empty();
    let mut st = db.prepare_cached(&format!(
        "SELECT seq, vec{} FROM chunk_vec WHERE model = ?1{}",
        if check_item { ", item" } else { "" },
        numbered(&vec.sql, 2)
    ))?;
    let mut args = vec![text(model)];
    args.extend(vec.args.iter().cloned());
    let mut rows = st.query(params_from_iter(&args))?;
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
            if check_item {
                let of = r.get_ref(2).ok().and_then(|v| v.as_str().ok()).unwrap_or_default();
                if allowed.as_ref().is_some_and(|a| !a.contains(of)) || gone.contains(of) {
                    continue;
                }
            }
            let s = r
                .get_ref(1)
                .ok()
                .and_then(|v| v.as_blob().ok())
                .and_then(|b| ptyro_meaning::score(b, q));
            if let (Ok(seq), Some(s)) = (r.get::<_, i64>(0), s)
                && s >= min
                && (ruled_out.is_empty() || !ruled_out.contains(&seq))
            {
                return Some((s, seq));
            }
        }
    });
    let best = ptyro_meaning::top_k(scored, limit);
    drop(rows);
    if let Some(e) = failed {
        return Err(e.into());
    }
    // Each hit's row, checked against its item as it is now: the whole filter again (a
    // vector's own columns are a copy), and never a forgotten one.
    let cond = f.row();
    let mut st = db.prepare_cached(&format!(
        "SELECT c.text, {HIT_COLS} FROM chunk c JOIN item i ON i.id = c.item
         WHERE c.seq = ?1 AND i.forgotten_at IS NULL{}",
        numbered(&cond.sql, 2)
    ))?;
    let mut hits = Vec::with_capacity(best.len());
    for (_, seq) in best {
        let mut args = vec![Value::Integer(seq)];
        args.extend(cond.args.iter().cloned());
        let hit = st
            .query_row(params_from_iter(&args), |r| {
                let text: String = r.get(0)?;
                hit_row(r, ptyro_meaning::opening(&text, 160), "meaning")
            })
            .optional()?;
        hits.extend(hit);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests;
