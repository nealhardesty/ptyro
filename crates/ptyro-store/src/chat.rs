//! `chat.db` (SPEC §22.4): the one database that holds contents, chat text, because history and
//! search are the feature. Apart from `ptyro.db`: its own file, connection and writer thread, so
//! a long message or an index update never delays a status write. Built for sync (§22.15):
//! ULIDs everywhere, an `origin` on every row, append-only nodes with parent links, tombstones.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ptyro_core::chat::ChatBody;
use ptyro_core::chats::{self, ChatInfo, ChatNode, ChatSiblings};
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use tokio::sync::oneshot;

use crate::{Result, StoreError};

mod vec;
pub use vec::VecTodo;

/// Schema history. Never edit a released migration; append a new one.
const MIGRATIONS: &[M<'static>] = &[
    M::up(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value);
    CREATE TABLE chat (
      id TEXT PRIMARY KEY,
      workspace TEXT NOT NULL,
      title TEXT, title_src TEXT,
      pinned INTEGER NOT NULL DEFAULT 0, archived_at INTEGER, deleted_at INTEGER,
      agent TEXT NOT NULL, profile TEXT NOT NULL, tools TEXT NOT NULL,
      leaf TEXT,
      forked_from_chat TEXT, forked_from_node TEXT,
      created_at INTEGER, updated_at INTEGER, last_at INTEGER,
      nodes INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0,
      cost_micro_usd INTEGER NOT NULL DEFAULT 0,
      origin TEXT NOT NULL);
    CREATE INDEX chat_by_workspace ON chat(workspace, last_at);
    CREATE TABLE node (
      seq INTEGER PRIMARY KEY,
      id TEXT NOT NULL UNIQUE,
      chat TEXT NOT NULL REFERENCES chat(id) ON DELETE CASCADE,
      parent TEXT,
      kind TEXT NOT NULL,
      body JSON NOT NULL,
      text TEXT,
      at_ms INTEGER NOT NULL,
      conv TEXT,
      origin TEXT NOT NULL, deleted_at INTEGER);
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
    END;
    CREATE TABLE conv (
      id TEXT PRIMARY KEY,
      chat TEXT NOT NULL REFERENCES chat(id) ON DELETE CASCADE,
      agent TEXT NOT NULL, profile TEXT NOT NULL,
      conversation_id TEXT,
      node TEXT,
      how TEXT NOT NULL,
      started_at INTEGER, ended_at INTEGER);
    CREATE INDEX conv_by_chat ON conv(chat, started_at);",
    ),
    // v2 (CHAT2): a side chat names the chat it reads (SPEC §22.7).
    M::up("ALTER TABLE chat ADD COLUMN side_of TEXT;"),
    // v3 (CHAT5): meaning search, a vector per message when it is switched on (SPEC §22.10):
    // `vec` is `ptyro_meaning::quantize`'s, `model` its `MODEL_ID`. `chat` and `at_ms` are the
    // node's, so a search scans this small table alone. A changed or deleted message loses its
    // vector (a streaming answer's is made again once it is complete).
    M::up(
        "CREATE TABLE node_vec (
      node TEXT PRIMARY KEY REFERENCES node(id) ON DELETE CASCADE,
      chat TEXT NOT NULL,
      at_ms INTEGER NOT NULL,
      model TEXT NOT NULL,
      vec BLOB NOT NULL);
    CREATE INDEX node_vec_by_chat ON node_vec(chat);
    CREATE TRIGGER node_vec_au AFTER UPDATE OF text, deleted_at ON node
      WHEN old.text IS NOT new.text OR new.deleted_at IS NOT NULL BEGIN
      DELETE FROM node_vec WHERE node = new.id;
    END;",
    ),
    // v4 (CHAT4): the folders a chat was given to read, as a JSON array (SPEC §22.5).
    M::up("ALTER TABLE chat ADD COLUMN folders TEXT;"),
    // v5 (CHAT5): chats imported from the agents' own histories (SPEC §22.10). `source` is
    // `claude:<session id>`; `import` remembers how far each session's file was read, and the
    // chat it became, kept after that chat is deleted so a delete is never undone by an import.
    M::up(
        "ALTER TABLE chat ADD COLUMN source TEXT;
    ALTER TABLE chat ADD COLUMN source_dir TEXT;
    CREATE UNIQUE INDEX chat_by_source ON chat(source) WHERE source IS NOT NULL;
    CREATE TABLE import (
      source TEXT PRIMARY KEY,
      chat TEXT NOT NULL,
      path TEXT NOT NULL,
      size INTEGER NOT NULL,
      mtime INTEGER NOT NULL,
      offset INTEGER NOT NULL DEFAULT 0,
      tail TEXT,
      at INTEGER NOT NULL);",
    ),
    // v6 (CHAT5): the tip each import left the chat's shown branch at, so a re-import moves it
    // on only when the person hasn't gone to another branch since.
    M::up("ALTER TABLE import ADD COLUMN leaf TEXT;"),
];

/// A tombstoned chat's rows go at the first compaction this long after its delete (§22.4).
pub const TOMBSTONE_MS: u64 = 24 * 3600 * 1000;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// The chat database: one connection on its own thread.
#[derive(Clone)]
pub struct ChatDb {
    tx: mpsc::Sender<Job>,
    /// This machine's id (`meta.machine_id`), the `origin` of what it writes.
    pub machine_id: String,
    pub path: PathBuf,
}

/// A new chat's row.
#[derive(Debug, Clone)]
pub struct NewChat {
    pub id: String,
    pub workspace: String,
    pub title: Option<String>,
    pub title_src: String,
    pub agent: String,
    pub profile: String,
    pub tools: String,
    pub forked_from: Option<(String, String)>,
    /// A side chat's chat.
    pub side_of: Option<String>,
    /// Folders it may read (with a preset that reads).
    pub folders: Vec<String>,
}

/// One agent conversation that served a chat (§22.4 `conv`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conv {
    pub id: String,
    pub chat: String,
    pub agent: String,
    pub profile: String,
    pub conversation_id: Option<String>,
    /// The last node it has seen.
    pub node: Option<String>,
    /// `new`, `native_fork` or `seed`.
    pub how: String,
    pub ended_ms: Option<u64>,
}

/// How far a session of an agent's history was imported (SPEC §22.10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportState {
    pub chat: String,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    /// How far an append-only file was read (the next import starts there).
    pub offset: u64,
    /// The last node written: the next one's parent.
    pub tail: Option<String>,
    /// The chat it became was deleted: it is never imported again.
    pub deleted: bool,
}

/// One session's new part, to import.
#[derive(Debug, Clone)]
pub struct ImportBatch {
    /// `claude:<session id>`.
    pub source: String,
    /// The chat's id when it is new.
    pub chat: String,
    pub workspace: String,
    /// The agent's own title for it, if any.
    pub title: Option<String>,
    pub agent: String,
    pub profile: String,
    pub tools: String,
    pub source_dir: Option<String>,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub offset: u64,
    /// Nodes, each with its parent (ids made from the source's, so an import is idempotent).
    pub nodes: Vec<ChatNode>,
    /// The session's last node: the shown branch's tip.
    pub leaf: Option<String>,
    /// The node before `offset` (the parent of the next import's first node).
    pub tail: Option<String>,
    pub index_tools: bool,
}

/// What `import_batch` did: the chat (`None`: it was deleted, so nothing), whether it is new,
/// and how many nodes it gained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub chat: Option<String>,
    pub created: bool,
    pub added: usize,
}

/// What `ChatDb::list` returns.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub workspace: Option<String>,
    pub pinned_only: bool,
    /// Archived chats instead of the others.
    pub archived: bool,
    pub limit: usize,
    /// Side chats too (they are listed under their chat, not with the others).
    pub sides: bool,
}

/// `ptyro doctor`'s look at `chat.db`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDoctor {
    pub bytes: u64,
    pub chats: u64,
    pub nodes: u64,
    pub fts5: bool,
    /// `ok`, or what `PRAGMA integrity_check` said.
    pub integrity: String,
}

const CHAT_COLS: &str = "id, workspace, title, title_src, pinned, archived_at, agent, profile, \
                         tools, leaf, forked_from_chat, created_at, last_at, nodes, unread, cost_micro_usd, side_of, folders, \
                         source, source_dir";

fn chat_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatInfo> {
    Ok(ChatInfo {
        id: r.get(0)?,
        handle: String::new(),
        workspace: r.get(1)?,
        machine: None,
        title: r.get(2)?,
        title_src: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        pinned: r.get::<_, i64>(4)? != 0,
        archived: r.get::<_, Option<i64>>(5)?.is_some(),
        incognito: false,
        agent: r.get(6)?,
        profile: r.get(7)?,
        tools: r.get(8)?,
        leaf: r.get(9)?,
        forked: r.get::<_, Option<String>>(10)?.is_some(),
        created_ms: r.get::<_, Option<i64>>(11)?.unwrap_or(0) as u64,
        last_ms: r.get::<_, Option<i64>>(12)?.unwrap_or(0) as u64,
        nodes: r.get::<_, i64>(13)? as u32,
        unread: r.get::<_, i64>(14)? as u32,
        cost_micro_usd: r.get::<_, i64>(15)? as u64,
        status: None,
        attention: None,
        engine: None,
        side_of: r.get(16)?,
        folders: r
            .get::<_, Option<String>>(17)?
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default(),
        best_effort: false,
        shell: false,
        comparing: None,
        source: r.get(18)?,
        source_dir: r.get(19)?,
    })
}

fn node_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatNode> {
    let body: String = r.get(2)?;
    let body = serde_json::from_str(&body).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ChatNode { id: r.get(0)?, parent: r.get(1)?, at_ms: r.get::<_, i64>(3)? as u64, body })
}

/// `None` for no folders, so a chat without any stores nothing.
fn folders_json(f: &[String]) -> Option<String> {
    (!f.is_empty()).then(|| serde_json::to_string(f).unwrap_or_default())
}

fn now() -> i64 {
    ptyro_core::now_ms() as i64
}

impl ChatDb {
    /// Open (or create) `path`, 0600, and bring its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        // Private every time (a restored copy may not be), and before the WAL is made: SQLite
        // gives its -wal and -shm files the main file's mode.
        private(path);
        Self::start(conn, path.to_path_buf())
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?, PathBuf::from(":memory:"))
    }

    fn start(mut conn: Connection, path: PathBuf) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let known = MIGRATIONS.len() as i64;
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found > known {
            return Err(StoreError::TooNew {
                path: path.display().to_string(),
                found,
                known: MIGRATIONS.len(),
            });
        }
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        let machine_id: String = match conn
            .query_row("SELECT value FROM meta WHERE key = 'machine_id'", [], |r| r.get(0))
            .optional()?
        {
            Some(m) => m,
            None => {
                let m = chats::new_id();
                conn.execute("INSERT INTO meta (key, value) VALUES ('machine_id', ?1)", [&m])?;
                m
            }
        };
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("ptyro-chat-db".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|_| StoreError::Closed)?;
        Ok(Self { tx, machine_id, path })
    }

    /// Run `f` on the chat thread and wait for its result.
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

    /// Run `f` on the chat thread without waiting; a failure is logged.
    pub fn spawn(&self, f: impl FnOnce(&mut Connection) -> Result<()> + Send + 'static) {
        let _ = self.tx.send(Box::new(move |c| {
            if let Err(e) = f(c) {
                tracing::warn!("chat.db: {e}");
            }
        }));
    }

    // ── chats ────────────────────────────────────────────────────────────────────────────────

    pub async fn create(&self, c: NewChat) -> Result<()> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let t = now();
            let (fc, fnode) = c.forked_from.unzip();
            db.execute(
                "INSERT INTO chat (id, workspace, title, title_src, agent, profile, tools,
                   forked_from_chat, forked_from_node, created_at, updated_at, last_at, origin, side_of,
                   folders)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10, ?11, ?12, ?13)",
                params![
                    c.id,
                    c.workspace,
                    c.title,
                    c.title_src,
                    c.agent,
                    c.profile,
                    c.tools,
                    fc,
                    fnode,
                    t,
                    origin,
                    c.side_of,
                    folders_json(&c.folders)
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// One chat (a tombstoned one too, when `deleted`).
    pub async fn get(&self, id: String) -> Result<Option<ChatInfo>> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    &format!("SELECT {CHAT_COLS} FROM chat WHERE id = ?1 AND deleted_at IS NULL"),
                    [id],
                    chat_row,
                )
                .optional()?)
        })
        .await
    }

    /// Chats, pinned first, then the most recent.
    pub async fn list(&self, f: ListFilter) -> Result<Vec<ChatInfo>> {
        self.call(move |db| {
            let mut sql = format!("SELECT {CHAT_COLS} FROM chat WHERE deleted_at IS NULL");
            sql.push_str(if f.archived {
                " AND archived_at IS NOT NULL"
            } else {
                " AND archived_at IS NULL"
            });
            if f.pinned_only {
                sql.push_str(" AND pinned = 1");
            }
            if !f.sides {
                sql.push_str(" AND side_of IS NULL");
            }
            if f.workspace.is_some() {
                sql.push_str(" AND workspace = ?1");
            }
            sql.push_str(" ORDER BY pinned DESC, last_at DESC, id DESC");
            if f.limit > 0 {
                sql.push_str(&format!(" LIMIT {}", f.limit));
            }
            let mut st = db.prepare(&sql)?;
            let rows = match &f.workspace {
                Some(w) => st.query_map([w], chat_row)?.collect::<rusqlite::Result<Vec<_>>>()?,
                None => st.query_map([], chat_row)?.collect::<rusqlite::Result<Vec<_>>>()?,
            };
            Ok(rows)
        })
        .await
    }

    /// Every chat id that isn't tombstoned (for handles).
    pub async fn ids(&self) -> Result<Vec<String>> {
        self.call(|db| {
            let mut st = db.prepare("SELECT id FROM chat WHERE deleted_at IS NULL")?;
            Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
    }

    /// A title; `src` = `auto`, `agent` or `user`. An automatic one never replaces a user's.
    pub async fn set_title(&self, id: String, title: Option<String>, src: String) -> Result<bool> {
        self.call(move |db| {
            let n = db.execute(
                "UPDATE chat SET title = ?2, title_src = ?3, updated_at = ?4
                 WHERE id = ?1 AND (?3 = 'user' OR title_src IS NOT 'user')",
                params![id, title, src, now()],
            )?;
            Ok(n > 0)
        })
        .await
    }

    pub async fn set_pinned(&self, id: String, pinned: bool) -> Result<()> {
        self.update(id, "pinned = ?2", pinned as i64).await
    }

    pub async fn set_archived(&self, id: String, archived: bool) -> Result<()> {
        self.update(id, "archived_at = ?2", archived.then(now)).await
    }

    pub async fn set_agent(&self, id: String, agent: String, profile: String) -> Result<()> {
        self.call(move |db| {
            db.execute(
                "UPDATE chat SET agent = ?2, profile = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, agent, profile, now()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn set_tools(&self, id: String, tools: String) -> Result<()> {
        self.update(id, "tools = ?2", tools).await
    }

    pub async fn set_folders(&self, id: String, folders: Vec<String>) -> Result<()> {
        self.update(id, "folders = ?2", folders_json(&folders)).await
    }

    pub async fn set_unread(&self, id: String, unread: u32) -> Result<()> {
        self.update(id, "unread = ?2", unread as i64).await
    }

    pub async fn set_leaf(&self, id: String, leaf: String) -> Result<()> {
        self.update(id, "leaf = ?2", leaf).await
    }

    async fn update(
        &self,
        id: String,
        set: &'static str,
        v: impl rusqlite::ToSql + Send + 'static,
    ) -> Result<()> {
        self.call(move |db| {
            db.execute(
                &format!("UPDATE chat SET {set}, updated_at = ?3 WHERE id = ?1"),
                params![id, v, now()],
            )?;
            Ok(())
        })
        .await
    }

    /// Delete: a tombstone now, the rows at the next compaction a day later (§22.4).
    pub async fn delete(&self, id: String) -> Result<bool> {
        self.call(move |db| {
            let t = now();
            let n = db.execute(
                "UPDATE chat SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                params![id, t],
            )?;
            db.execute(
                "UPDATE node SET deleted_at = ?2 WHERE chat = ?1 AND deleted_at IS NULL",
                params![id, t],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// A removed workspace's chats move to `to` (§22.2).
    pub async fn move_workspace(&self, from: String, to: String) -> Result<usize> {
        self.call(move |db| {
            Ok(db.execute(
                "UPDATE chat SET workspace = ?2, updated_at = ?3 WHERE workspace = ?1",
                params![from, to, now()],
            )?)
        })
        .await
    }

    /// Remove tombstones older than [`TOMBSTONE_MS`], and tombstone archived chats older than
    /// `archive_purge_days` (0 = never). Returns how many chats went, and the ids purged now
    /// (their files and lists are the caller's to clear).
    pub async fn compact(&self, archive_purge_days: u32) -> Result<(usize, Vec<String>)> {
        self.call(move |db| {
            let t = now();
            let mut purged = Vec::new();
            if archive_purge_days > 0 {
                let before = t - i64::from(archive_purge_days) * 86_400_000;
                purged = {
                    let mut st = db.prepare(
                        "SELECT id FROM chat WHERE archived_at IS NOT NULL AND archived_at < ?1 AND deleted_at IS NULL",
                    )?;
                    st.query_map([before], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                for id in &purged {
                    db.execute("UPDATE chat SET deleted_at = ?2 WHERE id = ?1", params![id, t])?;
                    // Out of the index at once, as a delete is.
                    db.execute(
                        "UPDATE node SET deleted_at = ?2 WHERE chat = ?1 AND deleted_at IS NULL",
                        params![id, t],
                    )?;
                }
            }
            let before = t - TOMBSTONE_MS as i64;
            let tx = db.transaction()?;
            // The index drops tombstoned nodes as they are marked; the rows go now.
            let n = tx.execute(
                "DELETE FROM chat WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
                [before],
            )?;
            tx.execute(
                "DELETE FROM node WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
                [before],
            )?;
            tx.commit()?;
            Ok((n, purged))
        })
        .await
    }

    // ── nodes ────────────────────────────────────────────────────────────────────────────────

    /// Write a node: insert it, or (while it streams, and once more when it completes) update
    /// its body. Keeps the chat's counts, leaf, last message time and cost in step.
    pub fn put_node(&self, chat: String, n: ChatNode, conv: Option<String>, index_tools: bool) {
        let origin = self.machine_id.clone();
        self.spawn(move |db| put_node(db, &chat, &n, conv.as_deref(), &origin, index_tools));
    }

    /// The same, waiting for it.
    pub async fn put_node_now(
        &self,
        chat: String,
        n: ChatNode,
        conv: Option<String>,
        index_tools: bool,
    ) -> Result<()> {
        let origin = self.machine_id.clone();
        self.call(move |db| put_node(db, &chat, &n, conv.as_deref(), &origin, index_tools)).await
    }

    /// The branch from the root to `leaf` (the chat's leaf when `None`), oldest first.
    pub async fn branch(&self, chat: String, leaf: Option<String>) -> Result<Vec<ChatNode>> {
        self.call(move |db| branch(db, &chat, leaf.as_deref())).await
    }

    /// Every node of a chat (all branches), oldest first.
    pub async fn nodes(&self, chat: String) -> Result<Vec<ChatNode>> {
        self.call(move |db| {
            let mut st = db.prepare(
                "SELECT id, parent, body, at_ms FROM node WHERE chat = ?1 AND deleted_at IS NULL ORDER BY seq",
            )?;
            Ok(st.query_map([chat], node_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// A daemon that stopped mid-answer left a streaming node: close each such turn with an
    /// `interrupted` end (§22.4). Returns the chats it touched.
    pub async fn close_interrupted(&self) -> Result<Vec<String>> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let open: Vec<(String, ChatNode)> = {
                let mut st = db.prepare(
                    "SELECT chat, id, parent, body, at_ms FROM node
                     WHERE kind = 'assistant' AND deleted_at IS NULL AND json_extract(body, '$.streaming') = 1",
                )?;
                st.query_map([], |r| {
                    let body: String = r.get(3)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        body,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .filter_map(|(chat, id, parent, body, at)| {
                    let body = serde_json::from_str(&body).ok()?;
                    Some((chat, ChatNode { id, parent, at_ms: at as u64, body }))
                })
                .collect()
            };
            let mut touched = Vec::new();
            for (chat, mut n) in open {
                if let ChatBody::Assistant { streaming, .. } = &mut n.body {
                    *streaming = false;
                }
                put_node(db, &chat, &n, None, &origin, false)?;
                let end = ChatNode {
                    id: chats::new_id(),
                    parent: Some(n.id.clone()),
                    at_ms: ptyro_core::now_ms(),
                    body: ChatBody::TurnEnd {
                        stop: "interrupted".into(),
                        cost_micro_usd: None,
                        duration_ms: None,
                        input_tokens: None,
                        output_tokens: None,
                    },
                };
                put_node(db, &chat, &end, None, &origin, false)?;
                touched.push(chat);
            }
            Ok(touched)
        })
        .await
    }

    /// One node of a chat (not a deleted one).
    pub async fn node(&self, chat: String, id: String) -> Result<Option<ChatNode>> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT id, parent, body, at_ms FROM node WHERE chat = ?1 AND id = ?2 AND deleted_at IS NULL",
                    [chat, id],
                    node_row,
                )
                .optional()?)
        })
        .await
    }

    /// A node by its id or the end of it (at least 6 characters, SPEC §22.3).
    pub async fn find_node(&self, chat: String, addr: String) -> Result<Vec<String>> {
        self.call(move |db| {
            let a = addr.trim().to_lowercase();
            let mut st = db.prepare(
                "SELECT id FROM node WHERE chat = ?1 AND deleted_at IS NULL AND (id = ?2 OR (length(?2) >= 6 AND id LIKE '%' || ?2))",
            )?;
            Ok(st.query_map(params![chat, a], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
    }

    /// Where each node of `branch` has siblings (other children of its parent), and which
    /// agent wrote each sibling's branch (the first node at or below it that a conv produced).
    pub async fn siblings(&self, chat: String, branch: Vec<String>) -> Result<Vec<ChatSiblings>> {
        self.call(move |db| sibling_groups(db, &chat, &branch, false)).await
    }

    /// The children of `parent` (answers to a message), each with the agent that wrote it, as
    /// `siblings` names them.
    pub async fn child_agents(
        &self,
        chat: String,
        parent: String,
    ) -> Result<Vec<(String, String)>> {
        self.call(move |db| {
            let first: Option<String> = db
                .query_row(
                    "SELECT id FROM node WHERE chat = ?1 AND parent = ?2 AND deleted_at IS NULL ORDER BY seq LIMIT 1",
                    [&chat, &parent],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(first) = first else { return Ok(Vec::new()) };
            let groups = sibling_groups(db, &chat, &[first], true)?;
            Ok(groups
                .into_iter()
                .next()
                .map(|s| {
                    let n = s.ids.len();
                    let agents = if s.agents.is_empty() { vec![String::new(); n] } else { s.agents };
                    s.ids.into_iter().zip(agents).collect()
                })
                .unwrap_or_default())
        })
        .await
    }

    /// The agent and profile that wrote the branch ending at `leaf`: the newest node on it
    /// that a conv produced (a compare's answer is a branch of the agent that wrote it).
    pub async fn branch_agent(
        &self,
        chat: String,
        leaf: String,
    ) -> Result<Option<(String, String)>> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "WITH RECURSIVE up(id, parent, conv, depth) AS (
                       SELECT id, parent, conv, 0 FROM node WHERE id = ?1 AND chat = ?2
                       UNION ALL
                       SELECT n.id, n.parent, n.conv, up.depth + 1 FROM node n JOIN up ON n.id = up.parent
                     )
                     SELECT conv.agent, conv.profile FROM up JOIN conv ON conv.id = up.conv
                     ORDER BY up.depth LIMIT 1",
                    [&leaf, &chat],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
        })
        .await
    }

    /// Add `nodes` (a branch beside the shown one, each the next's parent, the first's parent
    /// already in the chat) as written by `conv`, which is recorded ended: it is who wrote them,
    /// not a conversation to continue (a compare's other answers, SPEC §22.7). The leaf stays.
    pub async fn put_branch(
        &self,
        chat: String,
        nodes: Vec<ChatNode>,
        conv: Conv,
        index_tools: bool,
    ) -> Result<()> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let tx = db.transaction()?;
            let t = now();
            tx.execute(
                "INSERT INTO conv (id, chat, agent, profile, conversation_id, node, how, started_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    conv.id,
                    chat,
                    conv.agent,
                    conv.profile,
                    conv.conversation_id,
                    nodes.last().map(|n| n.id.clone()),
                    conv.how,
                    t
                ],
            )?;
            for n in &nodes {
                insert_node(&tx, &chat, n, Some(&conv.id), &origin, index_tools)?;
            }
            tx.execute(
                "UPDATE chat SET nodes = nodes + ?2, updated_at = ?3, last_at = ?3 WHERE id = ?1",
                params![chat, nodes.len() as i64, t],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The tip of `node`'s subtree, following the newest child down: what is shown when a
    /// branch is picked at `node`.
    pub async fn tip(&self, chat: String, node: String) -> Result<String> {
        self.call(move |db| {
            let mut at = node;
            for _ in 0..100_000 {
                let next: Option<String> = db
                    .query_row(
                        "SELECT id FROM node WHERE chat = ?1 AND parent = ?2 AND deleted_at IS NULL ORDER BY seq DESC LIMIT 1",
                        [&chat, &at],
                        |r| r.get(0),
                    )
                    .optional()?;
                match next {
                    Some(n) => at = n,
                    None => break,
                }
            }
            Ok(at)
        })
        .await
    }

    /// Copy `nodes` (a branch, oldest first) into chat `to` with new ids, each the next's
    /// parent: a fork (SPEC §22.7). Returns the old → new ids.
    pub async fn copy_branch(
        &self,
        to: String,
        nodes: Vec<ChatNode>,
        index_tools: bool,
    ) -> Result<Vec<(String, String)>> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let mut map = Vec::new();
            let mut parent: Option<String> = None;
            for n in nodes {
                let id = chats::new_id();
                let copy = ChatNode {
                    id: id.clone(),
                    parent: parent.clone(),
                    at_ms: n.at_ms,
                    body: n.body,
                };
                put_node(db, &to, &copy, None, &origin, index_tools)?;
                map.push((n.id, id.clone()));
                parent = Some(id);
            }
            Ok(map)
        })
        .await
    }

    // ── imports (SPEC §22.10: the agents' own histories) ─────────────────────────────────────

    /// Every session imported so far, by source.
    pub async fn import_states(&self) -> Result<std::collections::HashMap<String, ImportState>> {
        self.call(|db| {
            let mut st = db.prepare(
                "SELECT i.source, i.chat, i.path, i.size, i.mtime, i.offset, i.tail,
                        c.id IS NULL OR c.deleted_at IS NOT NULL
                 FROM import i LEFT JOIN chat c ON c.id = i.chat",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    ImportState {
                        chat: r.get(1)?,
                        path: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                        mtime: r.get::<_, i64>(4)? as u64,
                        offset: r.get::<_, i64>(5)? as u64,
                        tail: r.get(6)?,
                        deleted: r.get(7)?,
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// How many of `ids` are nodes already (a dry run's count of what is new).
    pub async fn known_nodes(&self, ids: Vec<String>) -> Result<usize> {
        self.call(move |db| {
            let mut st = db.prepare_cached("SELECT 1 FROM node WHERE id = ?1")?;
            let mut n = 0;
            for id in ids {
                if st.exists([id])? {
                    n += 1;
                }
            }
            Ok(n)
        })
        .await
    }

    /// Import one session's new part in one transaction: the chat (made when it is new), its
    /// nodes (a node already there takes its new body: a tool's result arrived), the shown
    /// branch's tip, the title unless a person gave one, and how far the file was read. A
    /// session whose chat was deleted is left alone.
    pub async fn import_batch(&self, b: ImportBatch) -> Result<ImportResult> {
        let origin = self.machine_id.clone();
        self.call(move |db| {
            let tx = db.transaction()?;
            let t = now();
            let known: Option<(String, bool)> = tx
                .query_row(
                    "SELECT i.chat, c.id IS NULL OR c.deleted_at IS NOT NULL
                     FROM import i LEFT JOIN chat c ON c.id = i.chat WHERE i.source = ?1",
                    [&b.source],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let by_source: Option<String> = tx
                .query_row(
                    "SELECT id FROM chat WHERE source = ?1 AND deleted_at IS NULL",
                    [&b.source],
                    |r| r.get(0),
                )
                .optional()?;
            if matches!(known, Some((_, true))) && by_source.is_none() {
                return Ok(ImportResult { chat: None, created: false, added: 0 });
            }
            let first_at = b.nodes.first().map_or(t, |n| n.at_ms as i64);
            let (chat, created) = match by_source.or(known.map(|k| k.0)) {
                Some(c) => (c, false),
                None => {
                    tx.execute(
                        "INSERT INTO chat (id, workspace, title, title_src, agent, profile, tools,
                           created_at, updated_at, last_at, origin, source, source_dir)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?8, ?10, ?11, ?12)",
                        params![
                            b.chat,
                            b.workspace,
                            b.title,
                            if b.title.is_some() { "agent" } else { "auto" },
                            b.agent,
                            b.profile,
                            b.tools,
                            first_at,
                            t,
                            origin,
                            b.source,
                            b.source_dir
                        ],
                    )?;
                    (b.chat.clone(), true)
                }
            };
            // The shown branch moves on with the session only from where the last import left
            // it (the person may have gone to another branch since).
            let old_tip: Option<Option<String>> = tx
                .query_row("SELECT leaf FROM import WHERE source = ?1", [&b.source], |r| r.get(0))
                .optional()?;
            let shown: Option<String> =
                tx.query_row("SELECT leaf FROM chat WHERE id = ?1", [&chat], |r| r.get(0))?;
            let follow = match (&shown, old_tip) {
                (None, _) | (_, None | Some(None)) => true,
                (Some(s), Some(Some(t))) => *s == t,
            };
            let mut added = 0;
            let mut last_msg: Option<i64> = None;
            for n in &b.nodes {
                if insert_node(&tx, &chat, n, None, &origin, b.index_tools)? {
                    added += 1;
                } else {
                    let body = serde_json::to_string(&n.body)?;
                    let text = chats::search_text(&n.body, b.index_tools);
                    tx.execute(
                        "UPDATE node SET body = ?2, text = ?3 WHERE id = ?1 AND chat = ?4 AND body IS NOT ?2",
                        params![n.id, body, text, chat],
                    )?;
                }
                if matches!(n.body, ChatBody::User { .. } | ChatBody::Assistant { .. }) {
                    last_msg = Some(last_msg.unwrap_or(0).max(n.at_ms as i64));
                }
            }
            tx.execute(
                "UPDATE chat SET
                   nodes = (SELECT count(*) FROM node WHERE chat = ?1 AND deleted_at IS NULL),
                   leaf = COALESCE(?2, leaf),
                   last_at = max(COALESCE(last_at, 0), COALESCE(?3, 0)),
                   title = CASE WHEN ?4 IS NOT NULL AND title_src IS NOT 'user' THEN ?4 ELSE title END,
                   title_src = CASE WHEN ?4 IS NOT NULL AND title_src IS NOT 'user' THEN 'agent' ELSE title_src END,
                   updated_at = ?5
                 WHERE id = ?1",
                params![chat, if follow { b.leaf.clone() } else { None }, last_msg, b.title, t],
            )?;
            tx.execute(
                "INSERT INTO import (source, chat, path, size, mtime, offset, tail, at, leaf)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(source) DO UPDATE SET chat = ?2, path = ?3, size = ?4, mtime = ?5,
                   offset = ?6, tail = COALESCE(?7, tail), at = ?8, leaf = COALESCE(?9, leaf)",
                params![
                    b.source,
                    chat,
                    b.path,
                    b.size as i64,
                    b.mtime as i64,
                    b.offset as i64,
                    b.tail,
                    t,
                    b.leaf
                ],
            )?;
            tx.commit()?;
            Ok(ImportResult { chat: Some(chat), created, added })
        })
        .await
    }

    /// Keep a side chat: it becomes an ordinary fork, listed with the others.
    pub async fn keep_side(&self, id: String) -> Result<()> {
        self.update(id, "side_of = ?2", Option::<String>::None).await
    }

    /// Full-text search (SPEC §22.10): messages ranked by bm25 (a person's words above the
    /// answers'), each with a snippet; then titles that match. `profile`/`workspace` limit it
    /// (a chat's `history_scope`). Deleted chats and nodes are never found.
    pub async fn search(&self, q: SearchFilter) -> Result<Vec<RawHit>> {
        self.call(move |db| search(db, &q)).await
    }

    // ── convs ────────────────────────────────────────────────────────────────────────────────

    pub async fn add_conv(&self, c: Conv) -> Result<()> {
        self.call(move |db| {
            db.execute(
                "INSERT INTO conv (id, chat, agent, profile, conversation_id, node, how, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![c.id, c.chat, c.agent, c.profile, c.conversation_id, c.node, c.how, now()],
            )?;
            Ok(())
        })
        .await
    }

    /// What a conv has reached: its agent conversation id, its last seen node.
    /// A turn ended: where the conv is now. A pending fork whose agent has a conversation of
    /// its own (not the source's any more) is a fork from here on; until then a restart forks
    /// the source again rather than resuming it.
    pub fn update_conv(&self, id: String, conversation_id: Option<String>, node: Option<String>) {
        self.spawn(move |db| {
            db.execute(
                "UPDATE conv SET conversation_id = COALESCE(?2, conversation_id), node = COALESCE(?3, node), \
                 how = CASE WHEN how = 'fork_pending' AND ?2 IS NOT NULL AND ?2 IS NOT conversation_id \
                 THEN 'native_fork' ELSE how END WHERE id = ?1",
                params![id, conversation_id, node],
            )?;
            Ok(())
        });
    }

    /// A conv the agent can no longer continue (its conversation is gone).
    pub fn end_conv(&self, id: String) {
        self.spawn(move |db| {
            db.execute("UPDATE conv SET ended_at = ?2 WHERE id = ?1", params![id, now()])?;
            Ok(())
        });
    }

    /// The chat's current conv: the newest one not ended.
    pub async fn current_conv(&self, chat: String) -> Result<Option<Conv>> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT id, chat, agent, profile, conversation_id, node, how, ended_at FROM conv
                     WHERE chat = ?1 AND ended_at IS NULL ORDER BY started_at DESC, rowid DESC LIMIT 1",
                    [chat],
                    |r| {
                        Ok(Conv {
                            id: r.get(0)?,
                            chat: r.get(1)?,
                            agent: r.get(2)?,
                            profile: r.get(3)?,
                            conversation_id: r.get(4)?,
                            node: r.get(5)?,
                            how: r.get(6)?,
                            ended_ms: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    // ── the file ─────────────────────────────────────────────────────────────────────────────

    pub async fn doctor(&self) -> Result<ChatDoctor> {
        let path = self.path.clone();
        self.call(move |db| {
            let chats: i64 =
                db.query_row("SELECT count(*) FROM chat WHERE deleted_at IS NULL", [], |r| {
                    r.get(0)
                })?;
            let nodes: i64 =
                db.query_row("SELECT count(*) FROM node WHERE deleted_at IS NULL", [], |r| {
                    r.get(0)
                })?;
            let fts5 = db
                .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0)
                == 1;
            let integrity: String =
                db.query_row("PRAGMA quick_check", [], |r| r.get(0)).unwrap_or_default();
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                + std::fs::metadata(path.with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
            Ok(ChatDoctor { bytes, chats: chats as u64, nodes: nodes as u64, fts5, integrity })
        })
        .await
    }

    /// A consistent copy of the file (WAL included) at `to`, private, for `ptyro backup`.
    pub async fn copy_to(&self, to: PathBuf) -> Result<()> {
        self.call(move |db| {
            let _ = std::fs::remove_file(&to);
            db.execute("VACUUM INTO ?1", [to.display().to_string()])?;
            private(&to);
            Ok(())
        })
        .await
    }

    /// The same of a `chat.db` that isn't open here (`ptyro backup` beside a running daemon):
    /// read-only, so nothing is migrated or written, whichever ptyro made it.
    pub fn snapshot(from: &Path, to: &Path) -> Result<()> {
        let db = Connection::open_with_flags(
            from,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        let _ = std::fs::remove_file(to);
        db.execute("VACUUM INTO ?1", [to.display().to_string()])?;
        private(to);
        Ok(())
    }
}

/// What a search looks in.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// An FTS5 expression (`parse_query`'s); empty: titles only.
    pub fts: String,
    /// Plain words, for titles.
    pub words: Vec<String>,
    pub chat: Option<String>,
    /// A workspace id.
    pub workspace: Option<String>,
    pub agent: Option<String>,
    pub profile: Option<String>,
    pub since_ms: Option<u64>,
    pub pinned: bool,
    /// Archived chats too.
    pub archived: bool,
    pub limit: usize,
}

/// A hit before the daemon dresses its chat.
#[derive(Debug, Clone)]
pub struct RawHit {
    pub chat: ChatInfo,
    pub node: Option<String>,
    pub kind: String,
    pub snippet: String,
    pub at_ms: u64,
    pub rank: f64,
}

/// `q`'s filters as SQL over `chat c`, and their arguments (`?1`…).
fn filter_sql(q: &SearchFilter) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut wh = String::from("c.deleted_at IS NULL");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut arg = |wh: &mut String, sql: &str, v: Box<dyn rusqlite::ToSql>| {
        args.push(v);
        wh.push_str(&sql.replace('?', &format!("?{}", args.len())));
    };
    if let Some(c) = &q.chat {
        arg(&mut wh, " AND c.id = ?", Box::new(c.clone()));
    }
    if let Some(w) = &q.workspace {
        arg(&mut wh, " AND c.workspace = ?", Box::new(w.clone()));
    }
    if let Some(a) = &q.agent {
        arg(&mut wh, " AND c.agent = ?", Box::new(a.clone()));
    }
    if let Some(p) = &q.profile {
        arg(&mut wh, " AND c.profile = ?", Box::new(p.clone()));
    }
    if q.pinned {
        wh.push_str(" AND c.pinned = 1");
    }
    if !q.archived {
        wh.push_str(" AND c.archived_at IS NULL");
    }
    (wh, args)
}

/// The chat columns of `chat c`.
fn chat_cols() -> String {
    CHAT_COLS.split(", ").map(|c| format!("c.{}", c.trim())).collect::<Vec<_>>().join(", ")
}

fn search(db: &Connection, q: &SearchFilter) -> Result<Vec<RawHit>> {
    let limit = if q.limit == 0 { 50 } else { q.limit.min(500) } as i64;
    let (wh, args) = filter_sql(q);
    let since = q.since_ms.map_or(0, |s| s as i64);
    let cols = chat_cols();
    let mut hits = Vec::new();
    if !q.fts.is_empty() {
        let n = args.len();
        let sql = format!(
            "SELECT {cols}, n.id, n.kind, snippet(node_fts, 0, char(2), char(3), '…', 16), n.at_ms,
                    bm25(node_fts) * CASE n.kind WHEN 'user' THEN 1.3 ELSE 1.0 END AS r
             FROM node_fts JOIN node n ON n.seq = node_fts.rowid JOIN chat c ON c.id = n.chat
             WHERE node_fts MATCH ?{} AND n.deleted_at IS NULL AND n.at_ms >= ?{} AND {wh}
             ORDER BY r LIMIT ?{}",
            n + 1,
            n + 2,
            n + 3
        );
        let mut st = db.prepare(&sql)?;
        let mut all: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        all.extend([&q.fts as &dyn rusqlite::ToSql, &since, &limit]);
        // The hit's columns come after the chat's.
        let k = CHAT_COLS.split(", ").count();
        let rows = st.query_map(all.as_slice(), |r| {
            Ok(RawHit {
                chat: chat_row(r)?,
                node: Some(r.get(k)?),
                kind: r.get(k + 1)?,
                snippet: r.get(k + 2)?,
                at_ms: r.get::<_, i64>(k + 3)? as u64,
                rank: r.get(k + 4)?,
            })
        })?;
        hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    }
    // Titles: every word in it (titles are short; no index needed).
    if !q.words.is_empty() {
        let mut st =
            db.prepare(&format!("SELECT {cols} FROM chat c WHERE {wh} AND c.title IS NOT NULL"))?;
        let all: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = st.query_map(all.as_slice(), chat_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        for c in rows {
            let title = c.title.clone().unwrap_or_default();
            let low = title.to_lowercase();
            if c.last_ms >= since as u64 && q.words.iter().all(|w| low.contains(&w.to_lowercase()))
            {
                hits.push(RawHit {
                    snippet: mark_words(&title, &q.words),
                    at_ms: c.last_ms,
                    chat: c,
                    node: None,
                    kind: "title".into(),
                    rank: 0.0,
                });
            }
        }
    }
    Ok(hits)
}

/// `title` with each of `words` marked as a snippet's matches are.
fn mark_words(title: &str, words: &[String]) -> String {
    let low = title.to_lowercase();
    // (Lowercasing can change a text's length, `İ`: then no marks rather than wrong ones.)
    if low.len() != title.len() {
        return title.to_owned();
    }
    let mut marks = vec![false; title.len()];
    for w in words.iter().map(|w| w.to_lowercase()).filter(|w| !w.is_empty()) {
        let mut from = 0;
        while let Some(i) = low[from..].find(&w) {
            for m in &mut marks[from + i..from + i + w.len()] {
                *m = true;
            }
            from += i + w.len();
        }
    }
    let mut out = String::new();
    let mut on = false;
    for (i, ch) in title.char_indices() {
        let m = marks.get(i).copied().unwrap_or(false);
        if m != on {
            out.push(if m { chats::MARK_ON } else { chats::MARK_OFF });
            on = m;
        }
        out.push(ch);
    }
    if on {
        out.push(chats::MARK_OFF);
    }
    out
}

/// Insert a node that isn't there yet (a copy or an import), leaving the chat's row alone.
fn insert_node(
    tx: &rusqlite::Transaction<'_>,
    chat: &str,
    n: &ChatNode,
    conv: Option<&str>,
    origin: &str,
    index_tools: bool,
) -> Result<bool> {
    let body = serde_json::to_string(&n.body)?;
    let text = chats::search_text(&n.body, index_tools);
    let added = tx.execute(
        "INSERT OR IGNORE INTO node (id, chat, parent, kind, body, text, at_ms, conv, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            n.id,
            chat,
            n.parent,
            chats::kind(&n.body),
            body,
            text,
            n.at_ms as i64,
            conv,
            origin
        ],
    )?;
    Ok(added > 0)
}

/// Owner-only (0600): chat text is private.
fn private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

fn put_node(
    db: &mut Connection,
    chat: &str,
    n: &ChatNode,
    conv: Option<&str>,
    origin: &str,
    index_tools: bool,
) -> Result<()> {
    let body = serde_json::to_string(&n.body)?;
    let text = chats::search_text(&n.body, index_tools);
    let tx = db.transaction()?;
    let updated = tx.execute(
        "UPDATE node SET body = ?2, text = ?3 WHERE id = ?1 AND body IS NOT ?2",
        params![n.id, body, text],
    )?;
    let exists = updated > 0
        || tx
            .query_row("SELECT 1 FROM node WHERE id = ?1", [&n.id], |_| Ok(()))
            .optional()?
            .is_some();
    // A chat deleted while its answer was still being written takes nothing more (a late write
    // must not come back to life, nor into the index).
    let gone = tx
        .query_row("SELECT deleted_at IS NOT NULL FROM chat WHERE id = ?1", [chat], |r| {
            r.get::<_, bool>(0)
        })
        .optional()?
        .unwrap_or(true);
    if !exists && gone {
        return Ok(());
    }
    if !exists {
        tx.execute(
            "INSERT INTO node (id, chat, parent, kind, body, text, at_ms, conv, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                n.id,
                chat,
                n.parent,
                chats::kind(&n.body),
                body,
                text,
                n.at_ms as i64,
                conv,
                origin
            ],
        )?;
        let message = matches!(n.body, ChatBody::User { .. } | ChatBody::Assistant { .. });
        tx.execute(
            // The leaf moves only when the node extends the shown branch: a late write of a
            // branch left behind (a stopped engine's) never takes the view back to it. A new
            // branch is shown by setting the leaf explicitly (`set_leaf`).
            "UPDATE chat SET leaf = CASE WHEN leaf IS NULL OR leaf IS ?5 THEN ?2 ELSE leaf END,
               nodes = nodes + 1, updated_at = ?3,
               last_at = CASE WHEN ?4 THEN ?3 ELSE last_at END WHERE id = ?1",
            params![chat, n.id, now(), message, n.parent],
        )?;
    }
    if let ChatBody::TurnEnd { cost_micro_usd: Some(c), .. } = &n.body
        && !exists
    {
        tx.execute(
            "UPDATE chat SET cost_micro_usd = cost_micro_usd + ?2 WHERE id = ?1",
            params![chat, *c as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Where each node of `branch` has siblings, with each sibling's agent (`single`: a node with
/// none is listed too, as one of one).
fn sibling_groups(
    db: &Connection,
    chat: &str,
    branch: &[String],
    single: bool,
) -> Result<Vec<ChatSiblings>> {
    let mut st = db.prepare(
        "SELECT id, parent, conv FROM node WHERE chat = ?1 AND deleted_at IS NULL ORDER BY seq",
    )?;
    let all: Vec<(String, Option<String>, Option<String>)> = st
        .query_map([chat], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut st = db.prepare("SELECT id, agent, profile FROM conv WHERE chat = ?1")?;
    let convs: std::collections::HashMap<String, String> = st
        .query_map([chat], |r| {
            let (id, agent, profile): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((id, chats::agent_label(&agent, &profile)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut by_parent: std::collections::HashMap<Option<String>, Vec<String>> =
        std::collections::HashMap::new();
    for (id, parent, _) in &all {
        by_parent.entry(parent.clone()).or_default().push(id.clone());
    }
    let parent_of: std::collections::HashMap<&String, &Option<String>> =
        all.iter().map(|(i, p, _)| (i, p)).collect();
    let conv_of: std::collections::HashMap<&String, &Option<String>> =
        all.iter().map(|(i, _, c)| (i, c)).collect();
    // Down the newest child until a node some conv produced (a notice has none).
    let agent_of = |id: &String| -> String {
        let mut at = id.clone();
        for _ in 0..64 {
            if let Some(Some(c)) = conv_of.get(&at)
                && let Some(a) = convs.get(c)
            {
                return a.clone();
            }
            match by_parent.get(&Some(at.clone())).and_then(|k| k.last()) {
                Some(next) => at = next.clone(),
                None => break,
            }
        }
        String::new()
    };
    Ok(branch
        .iter()
        .filter_map(|n| {
            let ids = by_parent.get(*parent_of.get(n)?)?;
            let at = ids.iter().position(|x| x == n)?;
            if ids.len() < 2 && !single {
                return None;
            }
            let agents: Vec<String> = ids.iter().map(&agent_of).collect();
            Some(ChatSiblings {
                node: n.clone(),
                at: at as u32 + 1,
                of: ids.len() as u32,
                ids: ids.clone(),
                agents: if agents.iter().all(String::is_empty) { Vec::new() } else { agents },
            })
        })
        .collect())
}

fn branch(db: &Connection, chat: &str, leaf: Option<&str>) -> Result<Vec<ChatNode>> {
    let leaf: Option<String> = match leaf {
        Some(l) => Some(l.to_owned()),
        None => db
            .query_row("SELECT leaf FROM chat WHERE id = ?1", [chat], |r| r.get(0))
            .optional()?
            .flatten(),
    };
    let Some(leaf) = leaf else { return Ok(Vec::new()) };
    let mut st = db.prepare(
        "WITH RECURSIVE up(id, parent, body, at_ms, depth) AS (
           SELECT id, parent, body, at_ms, 0 FROM node WHERE id = ?1 AND chat = ?2
           UNION ALL
           SELECT n.id, n.parent, n.body, n.at_ms, up.depth + 1 FROM node n JOIN up ON n.id = up.parent
         )
         SELECT up.id, up.parent, up.body, up.at_ms FROM up
         JOIN node ON node.id = up.id WHERE node.deleted_at IS NULL ORDER BY depth DESC",
    )?;
    Ok(st.query_map([&leaf, chat], node_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, parent: Option<&str>, text: &str) -> ChatNode {
        ChatNode {
            id: id.into(),
            parent: parent.map(str::to_owned),
            at_ms: 1,
            body: ChatBody::User { text: text.into() },
        }
    }

    fn answer(id: &str, parent: &str, text: &str, streaming: bool) -> ChatNode {
        ChatNode {
            id: id.into(),
            parent: Some(parent.into()),
            at_ms: 2,
            body: ChatBody::Assistant { text: text.into(), streaming },
        }
    }

    async fn db_with_chat() -> ChatDb {
        let db = ChatDb::open_in_memory().unwrap();
        db.create(NewChat {
            id: "c1".into(),
            workspace: "w1".into(),
            title: None,
            title_src: "auto".into(),
            agent: "claude".into(),
            profile: "default".into(),
            tools: "chat".into(),
            forked_from: None,
            side_of: None,
            folders: Vec::new(),
        })
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn a_compares_answers_are_labelled_and_picked_by_their_agent() {
        let db = db_with_chat().await;
        let conv = |id: &str, agent: &str, profile: &str| Conv {
            id: id.into(),
            chat: "c1".into(),
            agent: agent.into(),
            profile: profile.into(),
            conversation_id: None,
            node: None,
            how: "new".into(),
            ended_ms: None,
        };
        db.add_conv(conv("k1", "claude", "default")).await.unwrap();
        db.put_node_now("c1".into(), user("u", None, "q"), None, false).await.unwrap();
        db.put_node_now(
            "c1".into(),
            answer("a1", "u", "claude's", false),
            Some("k1".into()),
            false,
        )
        .await
        .unwrap();
        // Codex's answer beside it: the leaf (claude's) stays shown.
        let codex = vec![answer("b1", "u", "codex's", false)];
        db.put_branch("c1".into(), codex, conv("k2", "codex", "work"), false).await.unwrap();
        assert_eq!(db.get("c1".into()).await.unwrap().unwrap().leaf.as_deref(), Some("a1"));
        let sib = db.siblings("c1".into(), vec!["u".into(), "a1".into()]).await.unwrap();
        assert_eq!(sib[0].agents, ["claude", "codex@work"]);
        assert_eq!(sib[0].label(), Some("claude"));
        let kids = db.child_agents("c1".into(), "u".into()).await.unwrap();
        assert_eq!(kids, [("a1".into(), "claude".into()), ("b1".into(), "codex@work".into())]);
        // Who wrote a branch; the compare's conv is ended, so the current one is still claude's.
        let who = db.branch_agent("c1".into(), "b1".into()).await.unwrap();
        assert_eq!(who, Some(("codex".into(), "work".into())));
        assert_eq!(db.current_conv("c1".into()).await.unwrap().unwrap().id, "k1");
        // A retry's siblings by one agent carry no label.
        db.put_node_now("c1".into(), answer("a2", "u", "again", false), Some("k1".into()), false)
            .await
            .unwrap();
        let sib = db.siblings("c1".into(), vec!["a1".into()]).await.unwrap();
        assert_eq!(sib[0].of, 3);
    }

    #[tokio::test]
    async fn an_import_is_idempotent_and_a_deleted_one_stays_deleted() {
        let db = ChatDb::open_in_memory().unwrap();
        let batch = |nodes: Vec<ChatNode>, title: Option<&str>| ImportBatch {
            source: "claude:s1".into(),
            chat: chats::new_id(),
            workspace: "w1".into(),
            title: title.map(str::to_owned),
            agent: "claude".into(),
            profile: "default".into(),
            tools: "chat".into(),
            source_dir: Some("/repo".into()),
            path: "/h/.claude/projects/x/s1.jsonl".into(),
            size: 10,
            mtime: 1,
            offset: 10,
            leaf: nodes.last().map(|n| n.id.clone()),
            tail: nodes.last().map(|n| n.id.clone()),
            nodes,
            index_tools: false,
        };
        let first = vec![user("i1", None, "hello"), answer("i2", "i1", "hi", false)];
        let r = db.import_batch(batch(first.clone(), Some("Greeting"))).await.unwrap();
        assert!(r.created && r.added == 2);
        let chat = r.chat.unwrap();
        let again = db.import_batch(batch(first, None)).await.unwrap();
        assert_eq!(
            (again.chat.as_deref(), again.created, again.added),
            (Some(chat.as_str()), false, 0)
        );
        let more = vec![user("i3", Some("i2"), "more")];
        assert_eq!(db.import_batch(batch(more, None)).await.unwrap().added, 1);
        let c = db.get(chat.clone()).await.unwrap().unwrap();
        assert_eq!(
            (c.nodes, c.leaf.as_deref(), c.title.as_deref()),
            (3, Some("i3"), Some("Greeting"))
        );
        assert_eq!(c.source.as_deref(), Some("claude:s1"));
        // A person who went to another branch stays there; back at the import's tip, it follows.
        let leaf = || async { db.get(chat.clone()).await.unwrap().unwrap().leaf };
        db.set_leaf(chat.clone(), "i2".into()).await.unwrap();
        db.import_batch(batch(vec![user("i4", Some("i3"), "on")], None)).await.unwrap();
        assert_eq!(leaf().await.as_deref(), Some("i2"), "not moved off the person's branch");
        db.set_leaf(chat.clone(), "i4".into()).await.unwrap();
        db.import_batch(batch(vec![user("i5", Some("i4"), "and on")], None)).await.unwrap();
        assert_eq!(leaf().await.as_deref(), Some("i5"));
        // A person's title is never replaced by the agent's.
        db.set_title(chat.clone(), Some("Mine".into()), "user".into()).await.unwrap();
        db.import_batch(batch(vec![], Some("Theirs"))).await.unwrap();
        assert_eq!(db.get(chat.clone()).await.unwrap().unwrap().title.as_deref(), Some("Mine"));
        let st = db.import_states().await.unwrap();
        assert_eq!((st["claude:s1"].offset, st["claude:s1"].deleted), (10, false));
        // Deleted: the next import leaves it alone.
        db.delete(chat.clone()).await.unwrap();
        assert!(db.import_states().await.unwrap()["claude:s1"].deleted);
        let r = db.import_batch(batch(vec![user("i9", None, "x")], None)).await.unwrap();
        assert_eq!(r, ImportResult { chat: None, created: false, added: 0 });
        assert_eq!(db.known_nodes(vec!["i1".into(), "zz".into()]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_chats_folders_are_kept_and_changed() {
        let db = db_with_chat().await;
        assert!(db.get("c1".into()).await.unwrap().unwrap().folders.is_empty());
        db.set_folders("c1".into(), vec!["/notes".into(), "/src".into()]).await.unwrap();
        assert_eq!(db.get("c1".into()).await.unwrap().unwrap().folders, ["/notes", "/src"]);
        db.set_folders("c1".into(), Vec::new()).await.unwrap();
        assert!(db.get("c1".into()).await.unwrap().unwrap().folders.is_empty());
    }

    fn hits(db: &ChatDb, q: &'static str) -> i64 {
        db.call_blocking(move |c| {
            Ok(c.query_row("SELECT count(*) FROM node_fts WHERE node_fts MATCH ?1", [q], |r| {
                r.get(0)
            })?)
        })
        .unwrap()
    }

    #[tokio::test]
    async fn a_chat_its_branches_and_its_index() {
        let db = db_with_chat().await;
        assert_eq!(db.machine_id.len(), 26);
        db.put_node_now("c1".into(), user("u1", None, "how do lifetimes work"), None, false)
            .await
            .unwrap();
        // Streamed in steps, then complete: one row, updated in place.
        db.put_node_now("c1".into(), answer("a1", "u1", "They are", true), None, false)
            .await
            .unwrap();
        db.put_node_now("c1".into(), answer("a1", "u1", "They are regions", false), None, false)
            .await
            .unwrap();
        let c = db.get("c1".into()).await.unwrap().unwrap();
        assert_eq!((c.nodes, c.leaf.as_deref()), (2, Some("a1")));
        assert_eq!(hits(&db, "regions"), 1);
        assert_eq!(hits(&db, "lifetime"), 1, "porter: lifetime finds lifetimes");
        // A retry is a sibling; shown, the leaf moves to it; both branches stay.
        db.put_node_now("c1".into(), answer("a2", "u1", "Scopes", false), None, false)
            .await
            .unwrap();
        db.set_leaf("c1".into(), "a2".into()).await.unwrap();
        let b: Vec<String> =
            db.branch("c1".into(), None).await.unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(b, ["u1", "a2"]);
        let b: Vec<String> = db
            .branch("c1".into(), Some("a1".into()))
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(b, ["u1", "a1"]);
        assert_eq!(db.nodes("c1".into()).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn branches_tips_forks_and_search() {
        let db = db_with_chat().await;
        let put = |n: ChatNode| db.put_node_now("c1".into(), n, None, false);
        put(user("u1", None, "how should the retry policy back off?")).await.unwrap();
        put(answer("a1", "u1", "Exponentially, with jitter.", false)).await.unwrap();
        put(user("u2", Some("a1"), "and for redis?")).await.unwrap();
        // A retry of a1: a sibling. Written off the shown branch, it doesn't move the leaf…
        put(answer("a1b", "u1", "Linearly, capped at a minute.", false)).await.unwrap();
        assert_eq!(db.get("c1".into()).await.unwrap().unwrap().leaf.as_deref(), Some("u2"));
        // …until it is shown.
        db.set_leaf("c1".into(), "a1b".into()).await.unwrap();
        let b: Vec<String> =
            db.branch("c1".into(), None).await.unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(b, ["u1", "a1b"]);
        let sib = db.siblings("c1".into(), b.clone()).await.unwrap();
        assert_eq!(sib.len(), 1);
        assert_eq!((sib[0].node.as_str(), sib[0].at, sib[0].of), ("a1b", 2, 2));
        assert_eq!(sib[0].ids, ["a1", "a1b"]);
        // Back to the first: its subtree's tip is u2.
        assert_eq!(db.tip("c1".into(), "a1".into()).await.unwrap(), "u2");
        assert_eq!(db.find_node("c1".into(), "a1b".into()).await.unwrap().len(), 1);
        // A fork copies a branch with new ids, each the next's parent.
        db.create(NewChat {
            id: "c2".into(),
            workspace: "w1".into(),
            title: Some("fork of it".into()),
            title_src: "auto".into(),
            agent: "codex".into(),
            profile: "work".into(),
            tools: "chat".into(),
            forked_from: Some(("c1".into(), "a1".into())),
            side_of: None,
            folders: Vec::new(),
        })
        .await
        .unwrap();
        let path = db.branch("c1".into(), Some("a1".into())).await.unwrap();
        let map = db.copy_branch("c2".into(), path, false).await.unwrap();
        assert_eq!(map.len(), 2);
        let copied = db.branch("c2".into(), None).await.unwrap();
        assert_eq!(copied[1].parent.as_deref(), Some(copied[0].id.as_str()));
        assert!(copied.iter().all(|n| n.id != "u1" && n.id != "a1"));
        // Search: stems, the profile scope, titles, snippets marked.
        let q = |s: &str, profile: Option<&str>| {
            let p = ptyro_core::chats::parse_query(s, 0).unwrap();
            SearchFilter {
                fts: p.fts,
                words: p.words,
                profile: profile.map(Into::into),
                ..SearchFilter::default()
            }
        };
        let hits = db.search(q("jitters", None)).await.unwrap();
        assert_eq!(hits.iter().filter(|h| h.node.is_some()).count(), 2, "both chats' copies");
        assert!(hits[0].snippet.contains('\u{2}') && hits[0].snippet.contains('\u{3}'));
        let work = db.search(q("jitter", Some("work"))).await.unwrap();
        assert!(work.iter().all(|h| h.chat.profile == "work"), "a profile sees its own only");
        let titled = db.search(q("fork", None)).await.unwrap();
        assert!(titled.iter().any(|h| h.kind == "title" && h.chat.id == "c2"));
        db.delete("c2".into()).await.unwrap();
        assert!(
            db.search(q("jitter", Some("work"))).await.unwrap().is_empty(),
            "deleted: never found"
        );
        assert_eq!(mark_words("İstanbul trip", &["trip".into()]), "İstanbul trip");
    }

    #[tokio::test]
    async fn titles_pins_archive_and_delete() {
        let db = db_with_chat().await;
        assert!(db.set_title("c1".into(), Some("Mine".into()), "user".into()).await.unwrap());
        // An automatic title never replaces a person's.
        assert!(!db.set_title("c1".into(), Some("Auto".into()), "auto".into()).await.unwrap());
        assert_eq!(db.get("c1".into()).await.unwrap().unwrap().title.as_deref(), Some("Mine"));
        db.set_pinned("c1".into(), true).await.unwrap();
        db.set_archived("c1".into(), true).await.unwrap();
        let f = ListFilter { workspace: Some("w1".into()), ..ListFilter::default() };
        assert!(db.list(f.clone()).await.unwrap().is_empty());
        let a = db.list(ListFilter { archived: true, ..f.clone() }).await.unwrap();
        assert!(a[0].pinned && a[0].archived);
        db.put_node_now("c1".into(), user("u1", None, "secret plans"), None, false).await.unwrap();
        assert!(db.delete("c1".into()).await.unwrap());
        assert!(db.get("c1".into()).await.unwrap().is_none());
        assert_eq!(hits(&db, "secret"), 0, "a tombstone leaves the index at once");
        // A fresh tombstone stays until compaction a day later.
        assert_eq!(db.compact(0).await.unwrap().0, 0);
        db.call(|c| {
            c.execute("UPDATE chat SET deleted_at = 1", [])?;
            c.execute("UPDATE node SET deleted_at = 1", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(db.compact(0).await.unwrap().0, 1);
        let n: i64 = db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM node", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 0);
        let d = db.doctor().await.unwrap();
        assert!(d.fts5 && d.integrity == "ok", "{d:?}");
    }

    #[tokio::test]
    async fn a_late_write_after_a_delete_is_dropped_and_copies_are_private() {
        let db = db_with_chat().await;
        db.put_node_now("c1".into(), user("u1", None, "hello"), None, false).await.unwrap();
        db.delete("c1".into()).await.unwrap();
        // The engine's last flush, after the delete: it must not come back, nor be found.
        db.put_node_now("c1".into(), answer("a1", "u1", "secret answer", false), None, false)
            .await
            .unwrap();
        assert_eq!(hits(&db, "secret"), 0);
        let n: i64 = db
            .call(|c| {
                Ok(c.query_row("SELECT count(*) FROM node WHERE id = 'a1'", [], |r| r.get(0))?)
            })
            .await
            .unwrap();
        assert_eq!(n, 0);
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy.db");
        db.copy_to(copy.clone()).await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o600);
        // An archived chat past its days is purged, and named so its files can go.
        let db = db_with_chat().await;
        db.put_node_now("c1".into(), user("u1", None, "old plans"), None, false).await.unwrap();
        db.set_archived("c1".into(), true).await.unwrap();
        db.call(|c| Ok(c.execute("UPDATE chat SET archived_at = 1", [])?)).await.unwrap();
        assert_eq!(db.compact(30).await.unwrap().1, ["c1"]);
        assert_eq!(hits(&db, "plans"), 0, "out of the index at once");
    }

    #[tokio::test]
    async fn a_crash_mid_answer_is_closed_as_interrupted() {
        let db = db_with_chat().await;
        db.put_node_now("c1".into(), user("u1", None, "hi"), None, false).await.unwrap();
        db.put_node_now("c1".into(), answer("a1", "u1", "Hel", true), None, false).await.unwrap();
        assert_eq!(db.close_interrupted().await.unwrap(), ["c1"]);
        let b = db.branch("c1".into(), None).await.unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].body, ChatBody::Assistant { text: "Hel".into(), streaming: false });
        assert!(matches!(&b[2].body, ChatBody::TurnEnd { stop, .. } if stop == "interrupted"));
        assert!(db.close_interrupted().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn convs_and_the_current_one() {
        let db = db_with_chat().await;
        let conv = |id: &str| Conv {
            id: id.into(),
            chat: "c1".into(),
            agent: "claude".into(),
            profile: "default".into(),
            conversation_id: None,
            node: None,
            how: "new".into(),
            ended_ms: None,
        };
        db.add_conv(conv("k1")).await.unwrap();
        db.add_conv(conv("k2")).await.unwrap();
        db.update_conv("k2".into(), Some("sess-9".into()), Some("n5".into()));
        let c = db.current_conv("c1".into()).await.unwrap().unwrap();
        assert_eq!(
            (c.id.as_str(), c.conversation_id.as_deref(), c.node.as_deref()),
            ("k2", Some("sess-9"), Some("n5"))
        );
        db.end_conv("k2".into());
        assert_eq!(db.current_conv("c1".into()).await.unwrap().unwrap().id, "k1");
    }

    #[test]
    fn a_newer_schema_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("chat.db");
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(ChatDb::open(&p), Err(StoreError::TooNew { .. })));
        let db = ChatDb::open(&dir.path().join("new.db")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&db.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
