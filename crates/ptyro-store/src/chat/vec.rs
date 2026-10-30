//! Meaning search's vectors in `chat.db` (SPEC §22.10 tier 3, schema v3 `node_vec`): which
//! messages still need one, writing them, and the brute-force cosine scan behind a search. The
//! vectors come from `ptyro_meaning`; this module only stores and compares them.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::{ChatDb, RawHit, SearchFilter, chat_cols, chat_row, filter_sql};
use crate::Result;

/// A message that has no vector of the current model yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VecTodo {
    /// Its place in the table: the backfill walks down it.
    pub seq: i64,
    pub node: String,
    pub text: String,
}

/// The messages meaning search covers: what keyword search does, but titles (user and
/// assistant text, and tools only with `index_tools`), complete, not deleted. Over `node n`.
const COVERED: &str = "n.text IS NOT NULL AND n.deleted_at IS NULL
    AND (n.kind IN ('user', 'assistant') OR (?2 AND n.kind = 'tool'))
    AND json_extract(n.body, '$.streaming') IS NOT 1
    AND EXISTS (SELECT 1 FROM chat c WHERE c.id = n.chat AND c.deleted_at IS NULL)";

impl ChatDb {
    /// Up to `limit` messages with no vector of `model` whose `seq` is above `above` and below
    /// `below`, newest first: the backfill walks down from the top, `below` the last one it got,
    /// so the whole table is read once, and the recent past is searchable soonest.
    pub async fn vec_todo(
        &self,
        model: String,
        index_tools: bool,
        (above, below): (i64, i64),
        limit: usize,
    ) -> Result<Vec<VecTodo>> {
        self.call(move |db| {
            let sql = format!(
                "SELECT n.seq, n.id, n.text FROM node n WHERE n.seq > ?3 AND n.seq < ?4 AND {COVERED}
                 AND NOT EXISTS (SELECT 1 FROM node_vec v WHERE v.node = n.id AND v.model = ?1)
                 ORDER BY n.seq DESC LIMIT ?5"
            );
            let mut st = db.prepare(&sql)?;
            let rows = st.query_map(params![model, index_tools, above, below, limit as i64], |r| {
                Ok(VecTodo { seq: r.get(0)?, node: r.get(1)?, text: r.get(2)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Where a backfill pass above `above` ends: the newest message's `seq`, lowered to below the
    /// first answer still being written (it is embedded once complete, by the next pass).
    pub async fn vec_top(&self, above: i64) -> Result<i64> {
        self.call(move |db| {
            let top: i64 =
                db.query_row("SELECT coalesce(max(seq), 0) FROM node", [], |r| r.get(0))?;
            let open: Option<i64> = db
                .query_row(
                    "SELECT min(seq) FROM node WHERE seq > ?1 AND kind = 'assistant'
                     AND deleted_at IS NULL AND json_extract(body, '$.streaming') = 1",
                    [above],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            Ok(open.map_or(top, |o| top.min(o - 1)))
        })
        .await
    }

    /// Store vectors (`(node, vec)`); one whose message has gone meanwhile is dropped.
    pub async fn put_vecs(&self, model: String, rows: Vec<(String, Vec<u8>)>) -> Result<()> {
        self.call(move |db| {
            let tx = db.transaction()?;
            {
                let mut st = tx.prepare(
                    "INSERT OR REPLACE INTO node_vec (node, chat, at_ms, model, vec)
                     SELECT id, chat, at_ms, ?2, ?3 FROM node WHERE id = ?1 AND deleted_at IS NULL",
                )?;
                for (node, vec) in rows {
                    st.execute(params![node, model, vec])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// How many covered messages have a vector of `model`, of how many.
    pub async fn vec_counts(&self, model: String, index_tools: bool) -> Result<(u64, u64)> {
        self.call(move |db| {
            let sql = format!(
                "SELECT count(*), count(v.node) FROM node n
                 LEFT JOIN node_vec v ON v.node = n.id AND v.model = ?1 WHERE {COVERED}"
            );
            let (all, done): (i64, i64) =
                db.query_row(&sql, params![model, index_tools], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok((done as u64, all as u64))
        })
        .await
    }

    /// Vectors of any model but `model` (one ptyro no longer uses) go.
    pub async fn drop_other_vecs(&self, model: String) -> Result<usize> {
        self.call(move |db| Ok(db.execute("DELETE FROM node_vec WHERE model IS NOT ?1", [model])?))
            .await
    }

    /// The messages nearest in meaning to the unit vector `q`: the best `q.limit` with a cosine
    /// of at least `min`, under every filter keyword search has. `not` is an FTS5 expression
    /// of the words left out (`-word`): a message with one is not a hit either. Each hit's
    /// snippet is the start of its message, its rank the negated cosine (lower is better).
    pub async fn meaning(
        &self,
        f: SearchFilter,
        q: Vec<f32>,
        model: String,
        not: String,
        min: f32,
    ) -> Result<Vec<RawHit>> {
        self.call(move |db| meaning(db, &f, &q, &model, &not, min)).await
    }
}

/// The scan reads `node_vec` alone (a few hundred bytes a message): the chats the filters allow
/// and the messages a left-out word rules out are looked up first.
fn meaning(
    db: &Connection,
    f: &SearchFilter,
    q: &[f32],
    model: &str,
    not: &str,
    min: f32,
) -> Result<Vec<RawHit>> {
    let k = if f.limit == 0 { 50 } else { f.limit.min(500) };
    let (wh, args) = filter_sql(f);
    let chats: HashSet<String> = {
        let mut st = db.prepare(&format!("SELECT c.id FROM chat c WHERE {wh}"))?;
        let all: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        st.query_map(all.as_slice(), |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    if chats.is_empty() {
        return Ok(Vec::new());
    }
    let ruled_out: HashSet<String> = if !not.is_empty() {
        let mut st = db.prepare(
            "SELECT n.id FROM node_fts JOIN node n ON n.seq = node_fts.rowid WHERE node_fts MATCH ?1",
        )?;
        st.query_map([not], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    } else {
        HashSet::new()
    };
    let since = f.since_ms.map_or(0, |s| s as i64);
    let one = f.chat.as_deref().map(|_| " AND chat = ?3").unwrap_or_default();
    let mut st = db.prepare(&format!(
        "SELECT rowid, chat, node, vec FROM node_vec WHERE model = ?1 AND at_ms >= ?2{one}"
    ))?;
    let mut rows = match &f.chat {
        Some(c) => st.query(params![model, since, c])?,
        None => st.query(params![model, since])?,
    };
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
            let text = |i| r.get_ref(i).ok().and_then(|v| v.as_str().ok()).unwrap_or_default();
            if !chats.contains(text(1)) || (!ruled_out.is_empty() && ruled_out.contains(text(2))) {
                continue;
            }
            let s = r
                .get_ref(3)
                .ok()
                .and_then(|v| v.as_blob().ok())
                .and_then(|b| ptyro_meaning::score(b, q));
            if let (Ok(id), Some(s)) = (r.get::<_, i64>(0), s)
                && s >= min
            {
                return Some((s, id));
            }
        }
    });
    let best = ptyro_meaning::top_k(scored, k);
    if let Some(e) = failed {
        return Err(e.into());
    }
    let mut st = db.prepare(&format!(
        "SELECT {}, n.id, n.kind, n.text, n.at_ms FROM node_vec v JOIN node n ON n.id = v.node
         JOIN chat c ON c.id = n.chat WHERE v.rowid = ?1",
        chat_cols()
    ))?;
    // The hit's columns come after the chat's.
    let k = chat_cols().split(", ").count();
    let mut hits = Vec::with_capacity(best.len());
    for (s, id) in best {
        hits.push(st.query_row([id], |r| {
            Ok(RawHit {
                chat: chat_row(r)?,
                node: Some(r.get(k)?),
                kind: r.get(k + 1)?,
                snippet: ptyro_meaning::opening(&r.get::<_, String>(k + 2)?, 160),
                at_ms: r.get::<_, i64>(k + 3)? as u64,
                rank: -f64::from(s),
            })
        })?);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use ptyro_core::chat::{ChatBody, ToolStatus};
    use ptyro_core::chats::ChatNode;

    use super::super::NewChat;
    use super::*;

    const M: &str = "test-model";
    const ALL: (i64, i64) = (0, i64::MAX);

    async fn db() -> ChatDb {
        let db = ChatDb::open_in_memory().unwrap();
        for (id, profile) in [("c1", "default"), ("c2", "work")] {
            db.create(NewChat {
                id: id.into(),
                workspace: "w1".into(),
                title: None,
                title_src: "auto".into(),
                agent: "claude".into(),
                profile: profile.into(),
                tools: "chat".into(),
                forked_from: None,
                side_of: None,
                folders: Vec::new(),
            })
            .await
            .unwrap();
        }
        db
    }

    fn node(id: &str, body: ChatBody) -> ChatNode {
        ChatNode { id: id.into(), parent: None, at_ms: 5, body }
    }

    fn user(text: &str) -> ChatBody {
        ChatBody::User { text: text.into() }
    }

    fn unit(x: f32, y: f32) -> Vec<f32> {
        let n = (x * x + y * y).sqrt();
        vec![x / n, y / n]
    }

    #[tokio::test]
    async fn todo_put_count_and_scan() {
        let db = db().await;
        let put = |chat: &str, n: ChatNode| db.put_node_now(chat.into(), n, None, false);
        put("c1", node("u1", user("my kitten sleeps"))).await.unwrap();
        put("c1", node("a1", ChatBody::Assistant { text: "half".into(), streaming: true }))
            .await
            .unwrap();
        put("c2", node("u2", user("the dog barks"))).await.unwrap();
        let tool = ChatBody::Tool {
            name: "Read".into(),
            title: "notes.md".into(),
            status: ToolStatus::Done,
            output: None,
            diff: None,
            files: Vec::new(),
        };
        put("c1", node("t1", tool)).await.unwrap();

        // Newest first; a streaming answer waits until it is complete; tools only when indexed.
        let todo = db.vec_todo(M.into(), false, ALL, 10).await.unwrap();
        assert_eq!(todo.iter().map(|t| t.node.as_str()).collect::<Vec<_>>(), ["u2", "u1"]);
        assert_eq!(db.vec_todo(M.into(), true, ALL, 10).await.unwrap().len(), 3);
        assert_eq!(db.vec_counts(M.into(), false).await.unwrap(), (0, 2));
        // A pass walks down: below the last one it got, and above where the last pass ended.
        let below = db.vec_todo(M.into(), false, (0, todo[0].seq), 10).await.unwrap();
        assert_eq!(below.iter().map(|t| t.node.as_str()).collect::<Vec<_>>(), ["u1"]);
        assert!(
            db.vec_todo(M.into(), false, (todo[0].seq, i64::MAX), 10).await.unwrap().is_empty()
        );
        // A pass ends below the answer still streaming (seq 2), so the next one finds it.
        assert_eq!(db.vec_top(0).await.unwrap(), 1);
        assert_eq!(db.vec_top(2).await.unwrap(), 4);

        let q = |v: Vec<f32>| ptyro_meaning::quantize(&v);
        db.put_vecs(
            M.into(),
            vec![
                ("u1".into(), q(unit(1.0, 0.1))),
                ("u2".into(), q(unit(0.0, 1.0))),
                ("gone".into(), q(unit(1.0, 0.0))),
            ],
        )
        .await
        .unwrap();
        assert_eq!(db.vec_counts(M.into(), false).await.unwrap(), (2, 2));
        assert_eq!(db.drop_other_vecs(M.into()).await.unwrap(), 0, "its own model's stay");
        assert!(db.vec_todo(M.into(), false, ALL, 10).await.unwrap().is_empty());

        let f = || SearchFilter { limit: 10, ..SearchFilter::default() };
        let near = |f: SearchFilter, not: &str, min: f32| {
            db.meaning(f, unit(1.0, 0.0), M.into(), not.into(), min)
        };
        let hits = near(f(), "", 0.5).await.unwrap();
        assert_eq!(hits.len(), 1, "the dog is not near the cat");
        assert_eq!((hits[0].node.as_deref(), hits[0].kind.as_str()), (Some("u1"), "user"));
        assert_eq!(hits[0].snippet, "my kitten sleeps");
        assert!(hits[0].rank < -0.99);
        let all = near(f(), "", -1.0).await.unwrap();
        assert_eq!(all.iter().map(|h| h.chat.id.as_str()).collect::<Vec<_>>(), ["c1", "c2"]);
        // Every filter applies: the profile, a word left out, the date, another model.
        let work = SearchFilter { profile: Some("work".into()), ..f() };
        assert_eq!(near(work, "", -1.0).await.unwrap()[0].chat.id, "c2");
        assert_eq!(near(f(), "\"kitten\"", -1.0).await.unwrap().len(), 1);
        let later = SearchFilter { since_ms: Some(6), ..f() };
        assert!(near(later, "", -1.0).await.unwrap().is_empty());
        let other = db.meaning(f(), unit(1.0, 0.0), "x".into(), String::new(), -1.0);
        assert!(other.await.unwrap().is_empty());

        // A streaming answer, once complete, is embedded; a message's vector goes when it changes.
        put("c1", node("a1", ChatBody::Assistant { text: "whole".into(), streaming: false }))
            .await
            .unwrap();
        assert_eq!(db.vec_todo(M.into(), false, ALL, 10).await.unwrap()[0].node, "a1");
        // A message deleted loses its vector at once.
        db.call(|c| Ok(c.execute("UPDATE node SET deleted_at = 1 WHERE id = 'u1'", [])?))
            .await
            .unwrap();
        assert!(near(f(), "", -1.0).await.unwrap().iter().all(|h| h.node.as_deref() != Some("u1")));
        // A deleted chat's messages aren't found; another model's vectors go when asked.
        db.delete("c2".into()).await.unwrap();
        assert!(near(f(), "", -1.0).await.unwrap().iter().all(|h| h.chat.id == "c1"));
        // (Those of the deleted messages and chat went with them.)
        assert_eq!(db.drop_other_vecs("other".into()).await.unwrap(), 0);
    }
}
