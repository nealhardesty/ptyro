//! `kb.db` (SPEC §25.4): the knowledge base. The second database that holds contents, beside
//! `chat.db`: agent sessions, documents, comments, commits, notes and shell commands, cut into
//! chunks that are searched by words (FTS5) and by meaning (`chunk_vec`).
//!
//! Its own file, connection and writer thread, so an ingest never delays a status write or a
//! chat's answer. Everything that is written is a [`Plan`] (§25.5): there is no other
//! way to put text here.
//!
//! **Forgetting really deletes** (§25.12). The connection runs with `secure_delete`, and the
//! words' index with FTS5's own `secure-delete` option (without it a forgotten word stays in
//! the index's old segments). A delete costs about 1.7 ms of the writer's time a chunk for it,
//! so everything that deletes many chunks does it in batches, one writer job each, and other
//! writes go between them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::Plan;
use ptyro_core::kb::{ChunkKind, ItemKind, KbChunk, KbItem, KbRoot, RootKind, RootState, Writer};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use tokio::sync::oneshot;

use crate::migrate::{self, M};
use crate::{Result, StoreError};

mod search;
pub use search::{KbFilter, MATCH_CAP, VecTodo};

/// Schema history. Never edit a released migration; append a new one.
const MIGRATIONS: &[M] = &[
    // v1 (KB1.1): §25.4 as KB0.1's spike proved it at 500k and a million chunks. `chunk_fts`
    // ranks a match in a heading at twice one in the text, and deletes for good.
    M::up(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value);
    CREATE TABLE root (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL UNIQUE,
      kind TEXT NOT NULL,
      path TEXT NOT NULL UNIQUE,
      project TEXT,
      profile TEXT,
      options JSON NOT NULL,
      head TEXT,
      state TEXT NOT NULL,
      error TEXT, added_at INTEGER, synced_at INTEGER);
    CREATE TABLE item (
      id TEXT PRIMARY KEY,
      uri TEXT NOT NULL UNIQUE,
      kind TEXT NOT NULL,
      root TEXT REFERENCES root(id) ON DELETE CASCADE,
      title TEXT, rel TEXT,
      project TEXT, workspace TEXT, worktree TEXT, branch TEXT,
      profile TEXT,
      agent TEXT, model TEXT,
      session TEXT,
      writer TEXT NOT NULL,
      files JSON,
      hash TEXT,
      cursor JSON,
      chunks INTEGER NOT NULL DEFAULT 0,
      started_at INTEGER, ended_at INTEGER, ingested_at INTEGER,
      origin TEXT NOT NULL,
      forgotten_at INTEGER);
    CREATE INDEX item_by_kind ON item(kind, ended_at);
    CREATE INDEX item_by_root ON item(root, rel);
    CREATE INDEX item_by_session ON item(session);
    CREATE TABLE chunk (
      seq INTEGER PRIMARY KEY,
      item TEXT NOT NULL REFERENCES item(id) ON DELETE CASCADE,
      idx INTEGER NOT NULL,
      kind TEXT NOT NULL,
      anchor TEXT,
      heading TEXT,
      text TEXT NOT NULL,
      meta JSON,
      at_ms INTEGER NOT NULL,
      UNIQUE(item, idx));
    CREATE VIRTUAL TABLE chunk_fts USING fts5(text, heading, content='chunk', content_rowid='seq',
                                              tokenize='porter unicode61 remove_diacritics 2');
    CREATE TRIGGER chunk_ai AFTER INSERT ON chunk BEGIN
      INSERT INTO chunk_fts(rowid, text, heading) VALUES (new.seq, new.text, new.heading);
    END;
    CREATE TRIGGER chunk_ad AFTER DELETE ON chunk BEGIN
      INSERT INTO chunk_fts(chunk_fts, rowid, text, heading)
      VALUES ('delete', old.seq, old.text, old.heading);
    END;
    CREATE TRIGGER chunk_au AFTER UPDATE OF text, heading ON chunk BEGIN
      INSERT INTO chunk_fts(chunk_fts, rowid, text, heading)
      VALUES ('delete', old.seq, old.text, old.heading);
      INSERT INTO chunk_fts(rowid, text, heading) VALUES (new.seq, new.text, new.heading);
    END;
    INSERT INTO chunk_fts(chunk_fts, rank) VALUES ('rank', 'bm25(1.0, 2.0)');
    INSERT INTO chunk_fts(chunk_fts, rank) VALUES ('secure-delete', 1);
    CREATE TABLE chunk_vec (
      seq INTEGER PRIMARY KEY REFERENCES chunk(seq) ON DELETE CASCADE,
      item TEXT NOT NULL, kind TEXT NOT NULL, profile TEXT, project TEXT, at_ms INTEGER NOT NULL,
      model TEXT NOT NULL, vec BLOB NOT NULL);
    CREATE TRIGGER chunk_vec_au AFTER UPDATE OF text ON chunk WHEN old.text IS NOT new.text BEGIN
      DELETE FROM chunk_vec WHERE seq = new.seq;
    END;",
    ),
];

/// The cursor of an item nothing more is read into ([`KbDb::keep_out`]).
const CLOSED: &str = r#"{"closed":true}"#;

/// How many chunks one writer job deletes: about a third of a second with FTS5's
/// `secure-delete` on (1.7 ms a chunk at 500k chunks, KB0.1), then other writes get their turn.
pub const DELETE_BATCH: usize = 200;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The knowledge base's database: one connection on its own thread.
#[derive(Clone)]
pub struct KbDb {
    tx: mpsc::Sender<Job>,
    /// This machine's id (`meta.machine_id`, `chat.db`'s when that is open): the `origin` of
    /// what it writes.
    pub machine_id: String,
    pub path: PathBuf,
}

/// What [`KbDb::write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The item's id (also for one that was refused: its tombstone's).
    pub item: String,
    pub outcome: Outcome,
    /// Chunks inserted, rewritten in place, deleted, and left as they were.
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub kept: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A new item.
    New,
    /// An item that was there: its chunks now match the plan.
    Updated,
    /// The plan's hash is the item's: nothing was written (but a moved cursor).
    Unchanged,
    /// The URI was forgotten, or its item is closed ([`KbDb::keep_out`]): nothing was
    /// written, and nothing will be (§25.12).
    Forgotten,
}

/// Which items [`KbDb::list`] returns. Forgotten ones never are.
#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    /// Empty: every kind.
    pub kinds: Vec<ItemKind>,
    pub project: Option<String>,
    /// That profile's items and those of no profile (the wall, §25.9). `None`: everyone's.
    pub profile: Option<String>,
    pub root: Option<String>,
    /// A ptyro session's id.
    pub session: Option<String>,
    /// A worktree's id.
    pub worktree: Option<String>,
    /// Only what this one wrote (you, an agent).
    pub writer: Option<Writer>,
    /// By an item's time: when it ended, else started, else was ingested.
    pub since_ms: Option<u64>,
    /// Items older than this: the next page of a history.
    pub before_ms: Option<u64>,
    /// 0: no limit.
    pub limit: usize,
}

/// Where reading an item stopped ([`KbDb::cursors`]): what a queue is rebuilt from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub item: String,
    pub uri: String,
    /// ptyro's session.
    pub session: Option<String>,
    pub cursor: Option<serde_json::Value>,
}

/// One item of a root ([`KbDb::root_items`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootItem {
    pub id: String,
    pub uri: String,
    pub kind: ItemKind,
    pub cursor: Option<serde_json::Value>,
}

/// What an item's stored chunks say of its session ([`KbDb::session_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFacts {
    /// The start of its first user message.
    pub task: Option<String>,
    /// The start of its last assistant message.
    pub outcome: Option<String>,
    /// Its user messages.
    pub turns: u32,
}

/// Counts and sizes, for `ptyro kb status` ([`KbDb::status`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KbStatus {
    /// The file and its write-ahead log.
    pub bytes: u64,
    /// Items and their chunks by kind, forgotten ones left out.
    pub items: Vec<(ItemKind, u64, u64)>,
    pub vectors: u64,
    pub roots: u64,
    /// Tombstones.
    pub forgotten: u64,
}

/// `ptyro doctor`'s look at `kb.db`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KbDoctor {
    pub status: KbStatus,
    /// The schema's version (`user_version`).
    pub version: i64,
    pub fts5: bool,
    /// Deleted text is zeroed: the connection's `secure_delete` and the words' index's own.
    pub secure_delete: bool,
    /// `ok`, or what `PRAGMA quick_check` said.
    pub integrity: String,
}

const ITEM_COLS: &str = "id, uri, kind, root, title, rel, project, workspace, worktree, branch, \
                         profile, agent, model, session, writer, files, hash, cursor, chunks, \
                         started_at, ended_at, ingested_at, origin, forgotten_at";

/// A stored word that this build doesn't know (a newer ptyro's kind): the row can't be read.
fn word<T>(col: usize, found: &str, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    parse(found).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            format!("unknown word {found:?}").into(),
        )
    })
}

fn json(col: usize, r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<serde_json::Value>> {
    Ok(r.get::<_, Option<String>>(col)?.and_then(|j| serde_json::from_str(&j).ok()))
}

fn ms(col: usize, r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<u64>> {
    Ok(r.get::<_, Option<i64>>(col)?.map(|v| v.max(0) as u64))
}

fn item_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<KbItem> {
    Ok(KbItem {
        id: r.get(0)?,
        uri: r.get(1)?,
        kind: word(2, &r.get::<_, String>(2)?, ItemKind::parse)?,
        root: r.get(3)?,
        title: r.get(4)?,
        rel: r.get(5)?,
        project: r.get(6)?,
        workspace: r.get(7)?,
        worktree: r.get(8)?,
        branch: r.get(9)?,
        profile: r.get(10)?,
        agent: r.get(11)?,
        model: r.get(12)?,
        session: r.get(13)?,
        writer: word(14, &r.get::<_, String>(14)?, Writer::parse)?,
        files: r
            .get::<_, Option<String>>(15)?
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default(),
        hash: r.get(16)?,
        cursor: json(17, r)?,
        chunks: r.get::<_, i64>(18)?.max(0) as u32,
        started_ms: ms(19, r)?,
        ended_ms: ms(20, r)?,
        ingested_ms: ms(21, r)?,
        origin: r.get(22)?,
        forgotten_ms: ms(23, r)?,
    })
}

const CHUNK_COLS: &str = "seq, item, idx, kind, anchor, heading, text, meta, at_ms";

fn chunk_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<KbChunk> {
    Ok(KbChunk {
        seq: r.get(0)?,
        item: r.get(1)?,
        idx: r.get::<_, i64>(2)?.max(0) as u32,
        kind: word(3, &r.get::<_, String>(3)?, ChunkKind::parse)?,
        anchor: r.get(4)?,
        heading: r.get(5)?,
        text: r.get(6)?,
        meta: json(7, r)?,
        at_ms: r.get::<_, i64>(8)?.max(0) as u64,
    })
}

const ROOT_COLS: &str =
    "id, name, kind, path, project, profile, options, head, state, error, added_at, synced_at";

fn root_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<KbRoot> {
    Ok(KbRoot {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: word(2, &r.get::<_, String>(2)?, RootKind::parse)?,
        path: r.get(3)?,
        project: r.get(4)?,
        profile: r.get(5)?,
        options: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        head: r.get(7)?,
        state: word(8, &r.get::<_, String>(8)?, RootState::parse)?,
        error: r.get(9)?,
        added_ms: ms(10, r)?,
        synced_ms: ms(11, r)?,
    })
}

fn now() -> i64 {
    ptyro_core::now_ms() as i64
}

impl KbDb {
    /// Open (or create) `path`, 0600, and bring its schema up to date. `origin`: this machine's
    /// id as `chat.db` has it, so that both files name one machine alike; `None` (no `chat.db`)
    /// keeps the file's own, or makes one.
    pub fn open(path: &Path, origin: Option<&str>) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // A kb.db from an older ptyro is copied beside it first (`kb.db.bak-v<N>`, private): a
        // migration never costs the record. One from a newer ptyro is refused untouched.
        crate::prepare(path, MIGRATIONS.len())?;
        // Made private (SQLite would make it by the umask, readable until the next line).
        let _ = ptyro_core::os::private(std::fs::OpenOptions::new().append(true).create(true))
            .open(path);
        let conn = Connection::open(path)?;
        // Private every time (a restored copy may not be), and before the WAL is made: SQLite
        // gives its -wal and -shm files the main file's mode.
        let _ = ptyro_core::os::make_private(path);
        Self::start(conn, path.to_path_buf(), origin)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?, PathBuf::from(":memory:"), None)
    }

    fn start(mut conn: Connection, path: PathBuf, origin: Option<&str>) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Every transaction here writes, and says so when it begins: one that begins by
        // reading is refused at once when another connection holds the file's write lock
        // (SQLite can't wait for it then), whatever the timeout above.
        conn.set_transaction_behavior(rusqlite::TransactionBehavior::Immediate);
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Freed pages and cells are zeroed (§25.12). Per connection: every one that writes
        // this file must set it, and this is the only one.
        conn.pragma_update(None, "secure_delete", "ON")?;
        let known = MIGRATIONS.len() as i64;
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found > known {
            return Err(StoreError::TooNew {
                path: path.display().to_string(),
                found,
                known: MIGRATIONS.len(),
            });
        }
        migrate::to_latest(&mut conn, MIGRATIONS)?;
        let stored: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'machine_id'", [], |r| r.get(0))
            .optional()?;
        let machine_id = match (origin, stored) {
            (Some(o), Some(s)) if o == s => s,
            (None, Some(s)) => s,
            (o, _) => {
                let m = o.map_or_else(ptyro_core::chats::new_id, str::to_owned);
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('machine_id', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [&m],
                )?;
                m
            }
        };
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("ptyro-kb-db".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|_| StoreError::Closed)?;
        let db = Self { tx, machine_id, path };
        db.finish_forgets();
        Ok(db)
    }

    /// A forget that a crash or a restart cut short left a tombstone with some of its text
    /// (the tombstone is written first, the chunks go in batches): the rest goes now, before
    /// anything else is asked of the file (§25.12).
    fn finish_forgets(&self) {
        let me = self.clone();
        let _ = self.tx.send(Box::new(move |db| {
            let done = (|| -> Result<usize> {
                let n = db.execute(
                    "DELETE FROM chunk WHERE item IN
                       (SELECT id FROM item WHERE forgotten_at IS NOT NULL)",
                    [],
                )?;
                if n > 0 {
                    db.execute(
                        "UPDATE item SET chunks = 0 WHERE forgotten_at IS NOT NULL AND chunks > 0",
                        [],
                    )?;
                    db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
                }
                Ok(n)
            })();
            match done {
                Ok(0) => {}
                Ok(n) => {
                    me.remove_backups();
                    tracing::info!("kb.db: {n} piece(s) of forgotten items deleted at open");
                }
                Err(e) => tracing::warn!("kb.db: finishing what was forgotten: {e}"),
            }
        }));
    }

    /// Run `f` on the writer thread and wait for its result.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// The same, blocking (for code that isn't async: a test, a CLI tool).
    pub fn call_blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.recv().map_err(|_| StoreError::Closed)?
    }

    /// Wait until everything queued so far is written (before a live upgrade's exec: a write is
    /// one transaction, so the file then holds whole items only).
    pub async fn flush(&self) -> Result<()> {
        self.call(|_| Ok(())).await
    }

    // ── the way in ───────────────────────────────────────────────────────────────────────────

    /// Store a plan (§25.5 step 6), in one transaction: the item (made at its URI's first
    /// write) and its chunks.
    ///
    /// - A plan that is **all of its item** (`!append`): the item's chunks become the plan's. A
    ///   chunk the item already has, with the same kind, anchor, heading, text and meta, keeps
    ///   its row, and so its vector, wherever it now stands; the others are deleted and the new
    ///   ones inserted. The same hash as last time writes nothing at all.
    /// - An **appended** plan (a transcript read on from its cursor): its chunks follow the
    ///   item's. Chunks whose anchor the item already has stand in for those, in place (the
    ///   same text is left alone), so a piece read twice, or a session's card sent again with
    ///   each piece, doubles nothing. A piece must hold all the chunks of each of its anchors.
    /// - A **forgotten** URI is never written again.
    pub async fn write(&self, plan: Plan) -> Result<Written> {
        let origin = self.machine_id.clone();
        self.call(move |db| write(db, &plan, &origin)).await
    }

    // ── reading ──────────────────────────────────────────────────────────────────────────────

    /// One item by its id (a tombstone too: see `forgotten_ms`).
    pub async fn item(&self, id: String) -> Result<Option<KbItem>> {
        self.call(move |db| {
            Ok(db
                .query_row(&format!("SELECT {ITEM_COLS} FROM item WHERE id = ?1"), [id], item_row)
                .optional()?)
        })
        .await
    }

    /// One item by its URI (a tombstone too).
    pub async fn item_by_uri(&self, uri: String) -> Result<Option<KbItem>> {
        self.call(move |db| {
            Ok(db
                .query_row(&format!("SELECT {ITEM_COLS} FROM item WHERE uri = ?1"), [uri], item_row)
                .optional()?)
        })
        .await
    }

    /// Every item's id but the forgotten ones (for handles, `k:7f3k`).
    pub async fn ids(&self) -> Result<Vec<String>> {
        self.call(|db| {
            let mut st = db.prepare("SELECT id FROM item WHERE forgotten_at IS NULL")?;
            Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
    }

    /// An item's chunks from index `from` on, at most `limit` (0: all), in order.
    pub async fn chunks(&self, item: String, from: u32, limit: u32) -> Result<Vec<KbChunk>> {
        self.call(move |db| {
            let mut st = db.prepare_cached(&format!(
                "SELECT {CHUNK_COLS} FROM chunk WHERE item = ?1 AND idx >= ?2 ORDER BY idx LIMIT ?3"
            ))?;
            let limit = if limit == 0 { -1 } else { i64::from(limit) };
            Ok(st
                .query_map(params![item, i64::from(from), limit], chunk_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// What a session's card is made of, from the item's stored (so redacted) chunks: the
    /// start of its first user message, the start of its last assistant message, and how many
    /// user messages it holds (SPEC §25.6.1).
    pub async fn session_facts(&self, item: String) -> Result<SessionFacts> {
        self.call(move |db| {
            let one = |sql: &str| -> Result<Option<String>> {
                Ok(db.prepare_cached(sql)?.query_row([&item], |r| r.get(0)).optional()?)
            };
            let task = one(
                "SELECT text FROM chunk WHERE item = ?1 AND kind = 'user' ORDER BY idx LIMIT 1",
            )?;
            // (A long message is several chunks under one anchor: its first one.)
            let last = one("SELECT text FROM chunk WHERE item = ?1 AND kind = 'assistant'
                   AND anchor IS (SELECT anchor FROM chunk WHERE item = ?1 AND kind = 'assistant'
                                  ORDER BY idx DESC LIMIT 1)
                 ORDER BY idx LIMIT 1")?;
            let turns: i64 = db
                .prepare_cached(
                    "SELECT count(DISTINCT COALESCE(anchor, idx)) FROM chunk
                     WHERE item = ?1 AND kind = 'user'",
                )?
                .query_row([&item], |r| r.get(0))?;
            Ok(SessionFacts { task, outcome: last, turns: turns.max(0) as u32 })
        })
        .await
    }

    /// Items, newest first (the history): by kind, project, profile, root, session and time.
    pub async fn list(&self, f: ItemFilter) -> Result<Vec<KbItem>> {
        self.call(move |db| {
            const AT: &str = "COALESCE(ended_at, started_at, ingested_at, 0)";
            let mut sql = format!("SELECT {ITEM_COLS} FROM item WHERE forgotten_at IS NULL");
            let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            let mut arg = |sql: &mut String, cond: &str, v: Box<dyn rusqlite::ToSql>| {
                args.push(v);
                sql.push_str(&cond.replace('?', &format!("?{}", args.len())));
            };
            if !f.kinds.is_empty() {
                // (Their own words, never a caller's text.)
                let kinds: Vec<String> = f.kinds.iter().map(|k| format!("'{k}'")).collect();
                sql.push_str(&format!(" AND kind IN ({})", kinds.join(", ")));
            }
            if let Some(p) = f.project {
                arg(&mut sql, " AND project = ?", Box::new(p));
            }
            if let Some(p) = f.profile {
                arg(&mut sql, " AND (profile = ? OR profile IS NULL)", Box::new(p));
            }
            if let Some(r) = f.root {
                arg(&mut sql, " AND root = ?", Box::new(r));
            }
            if let Some(s) = f.session {
                arg(&mut sql, " AND session = ?", Box::new(s));
            }
            if let Some(w) = f.worktree {
                arg(&mut sql, " AND worktree = ?", Box::new(w));
            }
            if let Some(w) = f.writer {
                // (Its own word, never a caller's text.)
                sql.push_str(&format!(" AND writer = '{w}'"));
            }
            if let Some(t) = f.since_ms {
                arg(&mut sql, &format!(" AND {AT} >= ?"), Box::new(t as i64));
            }
            if let Some(t) = f.before_ms {
                arg(&mut sql, &format!(" AND {AT} < ?"), Box::new(t as i64));
            }
            sql.push_str(&format!(" ORDER BY {AT} DESC, id DESC"));
            if f.limit > 0 {
                sql.push_str(&format!(" LIMIT {}", f.limit));
            }
            let mut st = db.prepare(&sql)?;
            Ok(st
                .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), item_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    // ── cursors and tombstones ───────────────────────────────────────────────────────────────

    /// Where reading stopped for every item of `kind` that isn't forgotten: after a restart
    /// the ingest queue is rebuilt from these (§25.5), so nothing has to cross a live upgrade.
    pub async fn cursors(&self, kind: ItemKind) -> Result<Vec<Cursor>> {
        self.call(move |db| {
            let mut st = db.prepare(
                "SELECT id, uri, session, cursor FROM item WHERE kind = ?1 AND forgotten_at IS NULL",
            )?;
            Ok(st
                .query_map([kind.as_str()], |r| {
                    Ok(Cursor {
                        item: r.get(0)?,
                        uri: r.get(1)?,
                        session: r.get(2)?,
                        cursor: json(3, r)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Move an item's cursor without writing anything else (a read that found nothing new).
    pub async fn set_cursor(&self, uri: String, cursor: Option<serde_json::Value>) -> Result<()> {
        self.call(move |db| {
            db.execute(
                "UPDATE item SET cursor = ?2
                 WHERE uri = ?1 AND forgotten_at IS NULL AND cursor IS NOT ?3",
                params![uri, cursor.map(|c| c.to_string()), CLOSED],
            )?;
            Ok(())
        })
        .await
    }

    /// Nothing is ever read of `uri` from now on (a private session's conversation, §25.7a):
    /// a tombstone when nothing was kept of it; when an item is there (the conversation was
    /// kept before it was carried on in private), that item is **closed**: what it holds
    /// stays, and no write reaches it again, across restarts too.
    pub async fn keep_out(&self, uri: String, kind: ItemKind) -> Result<()> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO item (id, uri, kind, writer, origin, ingested_at, forgotten_at)
                 VALUES (?1, ?2, ?3, 'ptyro', ?4, ?5, ?5) ON CONFLICT(uri) DO NOTHING",
                params![ptyro_core::chats::new_id(), uri, kind.as_str(), origin, now()],
            )?;
            tx.execute(
                "UPDATE item SET cursor = ?2 WHERE uri = ?1 AND forgotten_at IS NULL",
                params![uri, CLOSED],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Whether `uri`'s item is closed ([`KbDb::keep_out`]): the gate's other question.
    pub async fn closed(&self, uri: String) -> Result<bool> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT 1 FROM item WHERE uri = ?1 AND forgotten_at IS NULL AND cursor = ?2",
                    params![uri, CLOSED],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }

    /// The commits that were forgotten under a root that was at `path` and is gone (their
    /// shas): a root made there again has another id, and so other URIs for them (§25.12).
    pub async fn forgotten_commits(&self, path: String) -> Result<Vec<String>> {
        self.call(move |db| {
            let mut st = db.prepare(
                "SELECT uri FROM item
                 WHERE kind = 'commit' AND forgotten_at IS NOT NULL AND root IS NULL AND rel = ?1",
            )?;
            Ok(st
                .query_map([path], |r| r.get::<_, String>(0))?
                .filter_map(|uri| Some(uri.ok()?.rsplit('/').next()?.to_owned()))
                .collect())
        })
        .await
    }

    /// Whether `uri` was forgotten: the gate's question (§25.5 step 1).
    pub async fn tombstoned(&self, uri: String) -> Result<bool> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT 1 FROM item WHERE uri = ?1 AND forgotten_at IS NOT NULL",
                    [uri],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }

    // ── roots ────────────────────────────────────────────────────────────────────────────────

    /// Add a root, or write all of an existing one's row again (by its id).
    pub async fn put_root(&self, root: KbRoot) -> Result<()> {
        self.call(move |db| {
            db.execute(
                "INSERT INTO root (id, name, kind, path, project, profile, options, head, state,
                                   error, added_at, synced_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name, kind = excluded.kind, path = excluded.path,
                   project = excluded.project, profile = excluded.profile,
                   options = excluded.options, head = excluded.head, state = excluded.state,
                   error = excluded.error, added_at = excluded.added_at,
                   synced_at = excluded.synced_at",
                params![
                    root.id,
                    root.name,
                    root.kind.as_str(),
                    root.path,
                    root.project,
                    root.profile,
                    serde_json::to_string(&root.options)?,
                    root.head,
                    root.state.as_str(),
                    root.error,
                    root.added_ms.map(|t| t as i64),
                    root.synced_ms.map(|t| t as i64),
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Every root, by name.
    pub async fn roots(&self) -> Result<Vec<KbRoot>> {
        self.call(|db| {
            let mut st = db.prepare(&format!("SELECT {ROOT_COLS} FROM root ORDER BY name"))?;
            Ok(st.query_map([], root_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// One root, by its id, its name or its path.
    pub async fn root(&self, which: String) -> Result<Option<KbRoot>> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    &format!(
                        "SELECT {ROOT_COLS} FROM root WHERE id = ?1 OR name = ?1 OR path = ?1"
                    ),
                    [which],
                    root_row,
                )
                .optional()?)
        })
        .await
    }

    /// What a root holds, for a pass over it: every item of it that isn't forgotten, with
    /// where reading it stopped.
    pub async fn root_items(&self, root: String) -> Result<Vec<RootItem>> {
        self.call(move |db| {
            let mut st = db.prepare(
                "SELECT id, uri, kind, cursor FROM item WHERE root = ?1 AND forgotten_at IS NULL",
            )?;
            Ok(st
                .query_map([root], |r| {
                    Ok(RootItem {
                        id: r.get(0)?,
                        uri: r.get(1)?,
                        kind: word(2, &r.get::<_, String>(2)?, ItemKind::parse)?,
                        cursor: json(3, r)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// How much each root holds: its id, a kind, that kind's items and their chunks.
    pub async fn root_counts(&self) -> Result<Vec<(String, ItemKind, u64, u64)>> {
        self.call(|db| {
            let mut st = db.prepare(
                "SELECT root, kind, count(*), sum(chunks) FROM item
                 WHERE root IS NOT NULL AND forgotten_at IS NULL GROUP BY root, kind",
            )?;
            Ok(st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        word(1, &r.get::<_, String>(1)?, ItemKind::parse)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Every URI of `kind` the file knows, forgotten ones too: what a backfill must not read
    /// again (`true`: it was forgotten).
    pub async fn uris(&self, kind: ItemKind) -> Result<Vec<(String, bool)>> {
        self.call(move |db| {
            let mut st =
                db.prepare("SELECT uri, forgotten_at IS NOT NULL FROM item WHERE kind = ?1")?;
            Ok(st
                .query_map([kind.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// How a root's sync went: its state, the error it stopped on, the newest commit read
    /// (`None` leaves the head as it is), and now as its sync time when it is `ok`.
    pub async fn set_root_state(
        &self,
        id: String,
        state: RootState,
        error: Option<String>,
        head: Option<String>,
    ) -> Result<()> {
        self.call(move |db| {
            let synced = (state == RootState::Ok).then(now);
            db.execute(
                "UPDATE root SET state = ?2, error = ?3, head = COALESCE(?4, head),
                                 synced_at = COALESCE(?5, synced_at) WHERE id = ?1",
                params![id, state.as_str(), error, head, synced],
            )?;
            Ok(())
        })
        .await
    }

    /// Remove a root and its items, with no tombstones: ingest it again and it is back
    /// (§25.12). **What was forgotten under it stays forgotten**: its tombstones are kept,
    /// no longer the root's; a file's is its path, and a commit's remembers where the
    /// repository was ([`KbDb::forgotten_commits`]). In batches; `progress` hears how many
    /// chunks are gone so far. Returns that number at the end.
    pub async fn remove_root(
        &self,
        id: String,
        mut progress: impl FnMut(usize) + Send,
    ) -> Result<usize> {
        let mut gone = 0;
        loop {
            let root = id.clone();
            let n = self
                .call(move |db| {
                    Ok(db.execute(
                        "DELETE FROM chunk WHERE seq IN (
                           SELECT c.seq FROM chunk c JOIN item i ON i.id = c.item
                           WHERE i.root = ?1 LIMIT ?2)",
                        params![root, DELETE_BATCH as i64],
                    )?)
                })
                .await?;
            gone += n;
            progress(gone);
            if n < DELETE_BATCH {
                break;
            }
        }
        self.call(move |db| {
            let tx = db.transaction()?;
            // (Chunks written meanwhile go with their items, by the foreign key.)
            tx.execute("DELETE FROM item WHERE root = ?1 AND forgotten_at IS NULL", [&id])?;
            tx.execute(
                "UPDATE item SET root = NULL,
                   rel = CASE WHEN kind = 'commit' THEN (SELECT path FROM root WHERE id = ?1)
                              ELSE rel END
                 WHERE root = ?1",
                [&id],
            )?;
            tx.execute("DELETE FROM root WHERE id = ?1", [&id])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(gone)
    }

    // ── deleting ─────────────────────────────────────────────────────────────────────────────

    /// Forget an item (§25.12): its chunks, their index entries and their vectors are deleted
    /// and zeroed, the write-ahead log is checkpointed so that no page of it is left there,
    /// and the copies set aside before a migration (which hold it too) are removed. The item
    /// stays as a tombstone: its URI and when, no text. Nothing writes that URI again.
    ///
    /// The tombstone is written first, so the item is out of every search at once; its chunks
    /// then go in batches. Returns how many, or `None` when there is no such item.
    pub async fn forget(&self, id: String) -> Result<Option<usize>> {
        let item = id.clone();
        let found = self
            .call(move |db| {
                Ok(db.execute(
                    "UPDATE item SET title = NULL, files = NULL, hash = NULL, cursor = NULL,
                                     forgotten_at = COALESCE(forgotten_at, ?2) WHERE id = ?1",
                    params![item, now()],
                )?)
            })
            .await?;
        if found == 0 {
            return Ok(None);
        }
        let gone = self.delete_chunks_of(id.clone()).await?;
        self.call(move |db| {
            db.execute("UPDATE item SET chunks = 0 WHERE id = ?1", [id])?;
            Ok(())
        })
        .await?;
        self.checkpoint().await?;
        self.remove_backups();
        Ok(Some(gone))
    }

    /// A tombstone for a URI that has no item (§25.12): a session nothing was read of yet,
    /// which is never to be kept. An item that is there is left as it is ([`KbDb::forget`]
    /// forgets one). Returns whether a tombstone was made.
    pub async fn tombstone(&self, uri: String, kind: ItemKind) -> Result<bool> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            Ok(db.execute(
                "INSERT INTO item (id, uri, kind, writer, origin, ingested_at, forgotten_at)
                 VALUES (?1, ?2, ?3, 'ptyro', ?4, ?5, ?5) ON CONFLICT(uri) DO NOTHING",
                params![ptyro_core::chats::new_id(), uri, kind.as_str(), origin, now()],
            )? > 0)
        })
        .await
    }

    /// Delete an item and its chunks, with no tombstone (a file that is gone, §25.6.3): the
    /// same URI may be written again. In batches. Returns whether it was there.
    pub async fn delete_item(&self, id: String) -> Result<bool> {
        self.delete_chunks_of(id.clone()).await?;
        self.call(move |db| Ok(db.execute("DELETE FROM item WHERE id = ?1", [id])? > 0)).await
    }

    async fn delete_chunks_of(&self, item: String) -> Result<usize> {
        let mut gone = 0;
        loop {
            let item = item.clone();
            let n = self
                .call(move |db| {
                    Ok(db.execute(
                        "DELETE FROM chunk WHERE seq IN (SELECT seq FROM chunk WHERE item = ?1 LIMIT ?2)",
                        params![item, DELETE_BATCH as i64],
                    )?)
                })
                .await?;
            gone += n;
            if n < DELETE_BATCH {
                return Ok(gone);
            }
        }
    }

    /// Retention (§25.12, `kb.retain_days`): delete the items of `kinds` whose time (ended,
    /// else started, else ingested) is before `before_ms`, with no tombstones; but a closed
    /// one ([`KbDb::keep_out`]) leaves its tombstone, so that it stays unread. Returns how
    /// many items went.
    pub async fn expire(&self, kinds: Vec<ItemKind>, before_ms: u64) -> Result<usize> {
        if kinds.is_empty() {
            return Ok(0);
        }
        let old: Vec<(String, bool)> = self
            .call(move |db| {
                let kinds: Vec<String> = kinds.iter().map(|k| format!("'{k}'")).collect();
                let mut st = db.prepare(&format!(
                    "SELECT id, cursor IS ?2 FROM item WHERE kind IN ({}) AND forgotten_at IS NULL
                       AND COALESCE(ended_at, started_at, ingested_at, 0) < ?1",
                    kinds.join(", ")
                ))?;
                Ok(st
                    .query_map(params![before_ms as i64, CLOSED], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        let n = old.len();
        for (id, closed) in old {
            if closed {
                self.forget(id).await?;
            } else {
                self.delete_item(id).await?;
            }
        }
        if n > 0 {
            self.checkpoint().await?;
        }
        Ok(n)
    }

    /// Retention for one kind of chunk (`kb.shell_output_days`: `output`): delete those older
    /// than `before_ms`; their items stay. Returns how many chunks went.
    pub async fn expire_chunks(&self, kind: ChunkKind, before_ms: u64) -> Result<usize> {
        let mut gone = 0;
        loop {
            let n = self
                .call(move |db| {
                    let tx = db.transaction()?;
                    let old: Vec<(i64, String)> = {
                        let mut st = tx.prepare_cached(
                            "SELECT seq, item FROM chunk WHERE kind = ?1 AND at_ms < ?2 LIMIT ?3",
                        )?;
                        st.query_map(
                            params![kind.as_str(), before_ms as i64, DELETE_BATCH as i64],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )?
                        .collect::<rusqlite::Result<_>>()?
                    };
                    let items: HashSet<&str> = old.iter().map(|(_, i)| i.as_str()).collect();
                    for (seq, _) in &old {
                        tx.prepare_cached("DELETE FROM chunk WHERE seq = ?1")?.execute([seq])?;
                    }
                    for item in items {
                        count_chunks(&tx, item)?;
                    }
                    tx.commit()?;
                    Ok(old.len())
                })
                .await?;
            gone += n;
            if n < DELETE_BATCH {
                break;
            }
        }
        if gone > 0 {
            self.checkpoint().await?;
        }
        Ok(gone)
    }

    /// Move the write-ahead log into the file and empty it: after a delete, so that what was
    /// deleted is in no page of the log either.
    pub async fn checkpoint(&self) -> Result<()> {
        self.call(|db| {
            db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }

    /// The copies set aside before a migration hold what the file held then: after a forget
    /// they go.
    fn remove_backups(&self) {
        let (Some(dir), Some(name)) = (self.path.parent(), self.path.file_name()) else { return };
        let prefix = format!("{}.bak-v", name.to_string_lossy());
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // ── the file ─────────────────────────────────────────────────────────────────────────────

    /// Counts and sizes.
    pub async fn status(&self) -> Result<KbStatus> {
        let path = self.path.clone();
        self.call(move |db| status(db, &path)).await
    }

    pub async fn doctor(&self) -> Result<KbDoctor> {
        let path = self.path.clone();
        self.call(move |db| {
            let fts5 = db
                .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0)
                == 1;
            let pragma: i64 = db.pragma_query_value(None, "secure_delete", |r| r.get(0))?;
            let index: i64 = db
                .query_row("SELECT v FROM chunk_fts_config WHERE k = 'secure-delete'", [], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(0);
            let integrity: String =
                db.query_row("PRAGMA quick_check", [], |r| r.get(0)).unwrap_or_default();
            Ok(KbDoctor {
                status: status(db, &path)?,
                version: db.pragma_query_value(None, "user_version", |r| r.get(0))?,
                fts5,
                secure_delete: pragma == 1 && index == 1,
                integrity,
            })
        })
        .await
    }
}

fn status(db: &Connection, path: &Path) -> Result<KbStatus> {
    let items = {
        let mut st = db.prepare(
            "SELECT kind, count(*), sum(chunks) FROM item WHERE forgotten_at IS NULL
             GROUP BY kind ORDER BY kind",
        )?;
        st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(k, n, c)| Some((ItemKind::parse(&k)?, n as u64, c.unwrap_or(0) as u64)))
        .collect()
    };
    let count =
        |sql: &str| -> Result<u64> { Ok(db.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64) };
    let wal = {
        let mut p = path.as_os_str().to_owned();
        p.push("-wal");
        PathBuf::from(p)
    };
    Ok(KbStatus {
        bytes: std::fs::metadata(path).map_or(0, |m| m.len())
            + std::fs::metadata(wal).map_or(0, |m| m.len()),
        items,
        vectors: count("SELECT count(*) FROM chunk_vec")?,
        roots: count("SELECT count(*) FROM root")?,
        forgotten: count("SELECT count(*) FROM item WHERE forgotten_at IS NOT NULL")?,
    })
}

fn count_chunks(tx: &Connection, item: &str) -> Result<()> {
    tx.prepare_cached(
        "UPDATE item SET chunks = (SELECT count(*) FROM chunk WHERE item = ?1) WHERE id = ?1",
    )?
    .execute([item])?;
    Ok(())
}

// ── writing a plan ──────────────────────────────────────────────────────────────────────────

/// What the item at a plan's URI is now: its id, hash, tombstone and cursor, whose it is
/// (its profile and project), and its root.
type Was = (
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    (Option<String>, Option<String>),
    Option<String>,
);

fn write(db: &mut Connection, p: &Plan, origin: &str) -> Result<Written> {
    let tx = db.transaction()?;
    let found: Option<Was> = tx
        .query_row(
            "SELECT id, hash, forgotten_at, cursor, profile, project, root FROM item WHERE uri = ?1",
            [&p.uri],
            |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, (r.get(4)?, r.get(5)?), r.get(6)?))
            },
        )
        .optional()?;
    // Whose the item was, when it changes hands: its vectors carry a copy (below).
    let mut moved = None;
    let done = |item: String, outcome| Written {
        item,
        outcome,
        added: 0,
        changed: 0,
        removed: 0,
        kept: 0,
    };
    let cursor = p.cursor.as_ref().map(|c| c.to_string());
    let files = (!p.files.is_empty()).then(|| serde_json::to_string(&p.files)).transpose()?;
    let t = now();
    let s = &p.stamp;
    let (id, outcome) = match found {
        Some((id, _, Some(_), ..)) => return Ok(done(id, Outcome::Forgotten)),
        // Closed (`keep_out`): what it holds stays, and nothing more is written to it.
        Some((id, _, None, Some(c), ..)) if c == CLOSED => {
            return Ok(done(id, Outcome::Forgotten));
        }
        // (The same text under another profile, project or root is not the same item: the
        // wall goes by those, so they are written again.)
        Some((id, hash, None, old_cursor, whose, root))
            if !p.append
                && p.hash.is_some()
                && hash == p.hash
                && whose == (s.profile.clone(), s.project.clone())
                && root == s.root =>
        {
            if cursor.is_some() && cursor != old_cursor {
                tx.execute("UPDATE item SET cursor = ?2 WHERE id = ?1", params![id, cursor])?;
                tx.commit()?;
            }
            return Ok(done(id, Outcome::Unchanged));
        }
        Some((id, .., whose, _)) if p.append => {
            moved = Some(whose);
            // More of the same item: what the piece doesn't say stays as it was.
            tx.execute(
                "UPDATE item SET
                   kind = ?2, root = COALESCE(?3, root), title = COALESCE(?4, title),
                   rel = COALESCE(?5, rel), project = COALESCE(?6, project),
                   workspace = COALESCE(?7, workspace), worktree = COALESCE(?8, worktree),
                   branch = COALESCE(?9, branch), profile = ?10, agent = COALESCE(?11, agent),
                   model = COALESCE(?12, model), session = COALESCE(?13, session), writer = ?14,
                   files = COALESCE(?15, files), hash = NULL, cursor = COALESCE(?16, cursor),
                   started_at = CASE WHEN started_at IS NULL OR ?17 < started_at THEN COALESCE(?17, started_at) ELSE started_at END,
                   ended_at = CASE WHEN ended_at IS NULL OR ?18 > ended_at THEN COALESCE(?18, ended_at) ELSE ended_at END,
                   ingested_at = ?19
                 WHERE id = ?1",
                params![
                    id, p.kind.as_str(), s.root, p.title, p.rel, s.project, s.workspace,
                    s.worktree, s.branch, s.profile, s.agent, s.model, s.session,
                    s.writer.as_str(), files, cursor, p.started_ms.map(|t| t as i64),
                    p.ended_ms.map(|t| t as i64), t,
                ],
            )?;
            (id, Outcome::Updated)
        }
        Some((id, .., whose, _)) => {
            moved = Some(whose);
            tx.execute(
                "UPDATE item SET
                   kind = ?2, root = ?3, title = ?4, rel = ?5, project = ?6, workspace = ?7,
                   worktree = ?8, branch = ?9, profile = ?10, agent = ?11, model = ?12,
                   session = ?13, writer = ?14, files = ?15, hash = ?16, cursor = ?17,
                   started_at = ?18, ended_at = ?19, ingested_at = ?20
                 WHERE id = ?1",
                params![
                    id,
                    p.kind.as_str(),
                    s.root,
                    p.title,
                    p.rel,
                    s.project,
                    s.workspace,
                    s.worktree,
                    s.branch,
                    s.profile,
                    s.agent,
                    s.model,
                    s.session,
                    s.writer.as_str(),
                    files,
                    p.hash,
                    cursor,
                    p.started_ms.map(|t| t as i64),
                    p.ended_ms.map(|t| t as i64),
                    t,
                ],
            )?;
            (id, Outcome::Updated)
        }
        None => {
            let id = ptyro_core::chats::new_id();
            tx.execute(
                "INSERT INTO item (id, uri, kind, root, title, rel, project, workspace, worktree,
                                   branch, profile, agent, model, session, writer, files, hash,
                                   cursor, started_at, ended_at, ingested_at, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    id,
                    p.uri,
                    p.kind.as_str(),
                    s.root,
                    p.title,
                    p.rel,
                    s.project,
                    s.workspace,
                    s.worktree,
                    s.branch,
                    s.profile,
                    s.agent,
                    s.model,
                    s.session,
                    s.writer.as_str(),
                    files,
                    p.hash,
                    cursor,
                    p.started_ms.map(|t| t as i64),
                    p.ended_ms.map(|t| t as i64),
                    t,
                    origin,
                ],
            )?;
            (id, Outcome::New)
        }
    };
    let mut w = done(id, outcome);
    // `chunk_vec` carries the item's profile and project so that a search by meaning joins
    // nothing (§25.4): an item that changes hands takes its vectors along.
    if let Some((profile, project)) = moved {
        let now_project =
            if p.append { s.project.clone().or(project.clone()) } else { s.project.clone() };
        if profile != s.profile || project != now_project {
            tx.execute(
                "UPDATE chunk_vec SET profile = ?2, project = ?3
                 WHERE seq IN (SELECT seq FROM chunk WHERE item = ?1)",
                params![w.item, s.profile, now_project],
            )?;
        }
    }
    if p.append {
        append(&tx, &p.chunks, &mut w)?;
    } else {
        replace(&tx, &p.chunks, &mut w)?;
    }
    count_chunks(&tx, &w.item)?;
    tx.commit()?;
    Ok(w)
}

/// A stored chunk's row number and place, and what makes it the same chunk as another.
type Same = (String, Option<String>, Option<String>, String, Option<String>);

fn same(c: &Part) -> Same {
    (
        c.kind.as_str().to_owned(),
        c.anchor.clone(),
        c.heading.clone(),
        c.text.clone(),
        c.meta.as_ref().map(|m| m.to_string()),
    )
}

fn insert(tx: &Connection, item: &str, idx: i64, c: &Part) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO chunk (item, idx, kind, anchor, heading, text, meta, at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        item,
        idx,
        c.kind.as_str(),
        c.anchor,
        c.heading,
        c.text,
        c.meta.as_ref().map(|m| m.to_string()),
        c.at_ms as i64
    ])?;
    Ok(())
}

/// The item's chunks become `chunks`: rows that are the same chunk stay (and move), the rest
/// are deleted and inserted.
fn replace(tx: &Connection, chunks: &[Part], w: &mut Written) -> Result<()> {
    let item = w.item.clone();
    let mut have: HashMap<Same, VecDeque<(i64, i64)>> = HashMap::new();
    {
        let mut st = tx.prepare_cached(
            "SELECT seq, idx, kind, anchor, heading, text, meta FROM chunk WHERE item = ?1 ORDER BY idx",
        )?;
        let rows = st.query_map([&item], |r| {
            Ok(((r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?), (r.get(0)?, r.get(1)?)))
        })?;
        for row in rows {
            let (key, at): (Same, (i64, i64)) = row?;
            have.entry(key).or_default().push_back(at);
        }
    }
    // Which row each new chunk keeps, if any.
    let keep: Vec<Option<(i64, i64)>> = if have.is_empty() {
        vec![None; chunks.len()]
    } else {
        chunks.iter().map(|c| have.get_mut(&same(c)).and_then(VecDeque::pop_front)).collect()
    };
    for (seq, _) in have.into_values().flatten() {
        tx.prepare_cached("DELETE FROM chunk WHERE seq = ?1")?.execute([seq])?;
        w.removed += 1;
    }
    // A kept row that moves waits below zero until the new rows are in, so that no two rows
    // ever hold one place.
    let mut moved = false;
    for (idx, (kept, c)) in keep.iter().zip(chunks).enumerate() {
        let idx = idx as i64;
        if let Some((seq, old)) = kept {
            w.kept += 1;
            if *old != idx {
                tx.prepare_cached("UPDATE chunk SET idx = ?2, at_ms = ?3 WHERE seq = ?1")?
                    .execute(params![seq, -idx - 1, c.at_ms as i64])?;
                moved = true;
            }
        }
    }
    for (idx, (kept, c)) in keep.iter().zip(chunks).enumerate() {
        if kept.is_none() {
            insert(tx, &item, idx as i64, c)?;
            w.added += 1;
        }
    }
    if moved {
        tx.execute("UPDATE chunk SET idx = -idx - 1 WHERE item = ?1 AND idx < 0", [&item])?;
    }
    Ok(())
}

/// `chunks` follow the item's; those whose anchor the item has stand in for its rows.
fn append(tx: &Connection, chunks: &[Part], w: &mut Written) -> Result<()> {
    let item = w.item.clone();
    let anchors: HashSet<&str> = chunks.iter().filter_map(|c| c.anchor.as_deref()).collect();
    // The rows under each anchor of this piece, in order. (One pass over the item's anchors:
    // an index on them would cost every chunk 70 bytes for the sake of a re-read.)
    let mut have: HashMap<String, VecDeque<i64>> = HashMap::new();
    let mut next: i64 = 0;
    {
        let mut st =
            tx.prepare_cached("SELECT seq, idx, anchor FROM chunk WHERE item = ?1 ORDER BY idx")?;
        let rows = st.query_map([&item], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        for row in rows {
            let (seq, idx, anchor) = row?;
            next = next.max(idx + 1);
            if let Some(a) = anchor.filter(|a| anchors.contains(a.as_str())) {
                have.entry(a).or_default().push_back(seq);
            }
        }
    }
    for c in chunks {
        let old = c.anchor.as_ref().and_then(|a| have.get_mut(a)).and_then(VecDeque::pop_front);
        let Some(seq) = old else {
            // (An anchor's later chunks, past the rows it had, go to the end too.)
            insert(tx, &item, next, c)?;
            next += 1;
            w.added += 1;
            continue;
        };
        let stored: Same = tx
            .prepare_cached("SELECT kind, anchor, heading, text, meta FROM chunk WHERE seq = ?1")?
            .query_row([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        let new = same(c);
        if stored == new {
            w.kept += 1;
        } else {
            tx.prepare_cached(
                "UPDATE chunk SET kind = ?2, heading = ?3, text = ?4, meta = ?5, at_ms = ?6 WHERE seq = ?1",
            )?
            .execute(params![seq, new.0, new.2, new.3, new.4, c.at_ms as i64])?;
            w.changed += 1;
        }
    }
    // An anchor that has fewer chunks now loses the rest.
    for seq in have.into_values().flatten() {
        tx.prepare_cached("DELETE FROM chunk WHERE seq = ?1")?.execute([seq])?;
        w.removed += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
