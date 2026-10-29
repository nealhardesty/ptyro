//! `chat.db` (SPEC §22.4): the one database that holds contents, chat text, because history and
//! search are the feature. Apart from `ptyro.db`: its own file, connection and writer thread, so
//! a long message or an index update never delays a status write. Built for sync (§22.15):
//! ULIDs everywhere, an `origin` on every row, append-only nodes with parent links, tombstones.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ptyro_core::chat::ChatBody;
use ptyro_core::chats::{self, ChatInfo, ChatNode};
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use tokio::sync::oneshot;

use crate::{Result, StoreError};

/// Schema history. Never edit a released migration; append a new one.
const MIGRATIONS: &[M<'static>] = &[M::up(
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
)];

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

/// What `ChatDb::list` returns.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub workspace: Option<String>,
    pub pinned_only: bool,
    /// Archived chats instead of the others.
    pub archived: bool,
    pub limit: usize,
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
                         tools, leaf, forked_from_chat, created_at, last_at, nodes, unread, cost_micro_usd";

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
    })
}

fn node_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatNode> {
    let body: String = r.get(2)?;
    let body = serde_json::from_str(&body).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ChatNode { id: r.get(0)?, parent: r.get(1)?, at_ms: r.get::<_, i64>(3)? as u64, body })
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
        let fresh = !path.exists();
        let conn = Connection::open(path)?;
        if fresh {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
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
                   forked_from_chat, forked_from_node, created_at, updated_at, last_at, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10, ?11)",
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
                    origin
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
            if f.workspace.is_some() {
                sql.push_str(" AND workspace = ?1");
            }
            sql.push_str(" ORDER BY pinned DESC, last_at DESC");
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

    /// Remove tombstones older than [`TOMBSTONE_MS`], and archived chats older than
    /// `archive_purge_days` (0 = never). Returns how many chats went.
    pub async fn compact(&self, archive_purge_days: u32) -> Result<usize> {
        self.call(move |db| {
            let t = now();
            if archive_purge_days > 0 {
                let before = t - i64::from(archive_purge_days) * 86_400_000;
                db.execute(
                    "UPDATE chat SET deleted_at = ?1 WHERE archived_at IS NOT NULL AND archived_at < ?2 AND deleted_at IS NULL",
                    params![t, before],
                )?;
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
            Ok(n)
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
    pub fn update_conv(&self, id: String, conversation_id: Option<String>, node: Option<String>) {
        self.spawn(move |db| {
            db.execute(
                "UPDATE conv SET conversation_id = COALESCE(?2, conversation_id), node = COALESCE(?3, node) WHERE id = ?1",
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
                db.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap_or_default();
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                + std::fs::metadata(path.with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
            Ok(ChatDoctor { bytes, chats: chats as u64, nodes: nodes as u64, fts5, integrity })
        })
        .await
    }

    /// A consistent copy of the file (WAL included) at `to`, for `ptyro backup`.
    pub async fn copy_to(&self, to: PathBuf) -> Result<()> {
        self.call(move |db| {
            let _ = std::fs::remove_file(&to);
            db.execute("VACUUM INTO ?1", [to.display().to_string()])?;
            Ok(())
        })
        .await
    }
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
            "UPDATE chat SET leaf = ?2, nodes = nodes + 1, updated_at = ?3,
               last_at = CASE WHEN ?4 THEN ?3 ELSE last_at END WHERE id = ?1",
            params![chat, n.id, now(), message],
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
        })
        .await
        .unwrap();
        db
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
        // A retry is a sibling; the leaf moves to it; both branches stay.
        db.put_node_now("c1".into(), answer("a2", "u1", "Scopes", false), None, false)
            .await
            .unwrap();
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
        assert_eq!(db.compact(0).await.unwrap(), 0);
        db.call(|c| {
            c.execute("UPDATE chat SET deleted_at = 1", [])?;
            c.execute("UPDATE node SET deleted_at = 1", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(db.compact(0).await.unwrap(), 1);
        let n: i64 = db
            .call(|c| Ok(c.query_row("SELECT count(*) FROM node", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 0);
        let d = db.doctor().await.unwrap();
        assert!(d.fts5 && d.integrity == "ok", "{d:?}");
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
