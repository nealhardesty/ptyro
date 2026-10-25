//! The store (SPEC §14.2): SQLite (bundled), WAL, migrations embedded, one OS thread owning the
//! connection. Async callers send closures and await a oneshot; fire-and-forget writes never wait.
//! Only metadata lives here, never terminal contents.

use std::path::Path;
use std::sync::mpsc;

use ptyro_core::model::{PortBlock, ProjectInfo, WorkspaceInfo, WorktreeInfo};
use ptyro_core::session::{ProcessState, SessionKind};
use ptyro_core::status::{AttentionKind, InboxEntry};
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

mod runs;
pub use runs::{LeaseRow, MessageRow, RunRow};

/// Schema history. Never edit a released migration; append a new one.
const MIGRATIONS: &[M<'static>] = &[
    M::up(
        "CREATE TABLE session (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        argv TEXT NOT NULL,          -- JSON array
        cwd TEXT NOT NULL,
        state TEXT NOT NULL,         -- JSON ProcessState
        pid INTEGER,
        daemon_run TEXT NOT NULL,    -- which daemon instance ran it
        created_at INTEGER NOT NULL,
        ended_at INTEGER
    );
    CREATE INDEX session_by_created ON session(created_at);
    CREATE TABLE session_event (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
        at INTEGER NOT NULL,
        kind TEXT NOT NULL,          -- process | status | rename | restart
        detail TEXT                  -- JSON
    );
    CREATE INDEX session_event_by_session ON session_event(session_id, at);
    CREATE TABLE kv (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,         -- JSON
        updated_at INTEGER NOT NULL
    );",
    ),
    // v2 (M3.1): workspaces, projects, worktrees, trust; sessions learn their place and profile.
    M::up(
        "CREATE TABLE workspace (
        id TEXT PRIMARY KEY,
        name TEXT UNIQUE NOT NULL,
        profile TEXT NOT NULL DEFAULT 'default',   -- a name in user config
        notify TEXT NOT NULL DEFAULT 'all',        -- all | badge | mute
        quiet_hours TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE project (
        id TEXT PRIMARY KEY,
        slug TEXT UNIQUE NOT NULL,
        root_path TEXT UNIQUE NOT NULL,
        default_branch TEXT,
        remote_url TEXT,
        profile TEXT,                              -- the pin, nullable
        created_at INTEGER NOT NULL,
        last_used_at INTEGER
    );
    CREATE TABLE workspace_project (
        workspace_id TEXT NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
        project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        PRIMARY KEY (workspace_id, project_id)
    );
    CREATE TABLE worktree (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        path TEXT NOT NULL,
        branch TEXT,
        base TEXT,
        is_root INTEGER NOT NULL DEFAULT 0,
        profile TEXT,                              -- fixed at creation; NULL for @root
        state TEXT NOT NULL,                       -- creating | ready | missing | removed | failed
        port_base INTEGER,
        port_count INTEGER,
        created_at INTEGER NOT NULL,
        removed_at INTEGER,
        last_used_at INTEGER
    );
    -- Removed worktrees stay as history, so names are unique among the others only.
    CREATE UNIQUE INDEX worktree_live_name ON worktree(project_id, name) WHERE state != 'removed';
    CREATE TABLE trust (
        project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
        scope TEXT NOT NULL,                       -- scripts
        fingerprint TEXT NOT NULL,
        approved_at INTEGER NOT NULL,
        PRIMARY KEY (project_id, scope)
    );
    ALTER TABLE session ADD COLUMN profile TEXT NOT NULL DEFAULT 'default';
    ALTER TABLE session ADD COLUMN worktree_id TEXT;
    ALTER TABLE session ADD COLUMN project_id TEXT;
    ALTER TABLE session ADD COLUMN workspace TEXT;
    ALTER TABLE session ADD COLUMN agent TEXT;
    ALTER TABLE session ADD COLUMN script TEXT;
    ALTER TABLE session ADD COLUMN launch TEXT;   -- JSON: argv and env diff, secrets as <secret:NAME>",
    ),
    // v3 (M4): conversation ids for resume, and the inbox (SPEC §9.5).
    M::up(
        "ALTER TABLE session ADD COLUMN conversation_id TEXT;
    CREATE TABLE inbox (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL,
        session_name TEXT NOT NULL,
        at INTEGER NOT NULL,
        kind TEXT NOT NULL,          -- blocked | errored | done | idle | exited | notification
        title TEXT NOT NULL,
        body TEXT NOT NULL,
        read_at INTEGER,
        workspace TEXT,
        profile TEXT
    );
    CREATE INDEX inbox_by_at ON inbox(at);",
    ),
    // v4 (M7): runs, event-sourced (an append-only log + a projection, written together), the
    // message ledger, and file leases.
    M::up(
        "CREATE TABLE run (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,          -- recipe | fanout | lead
        name TEXT NOT NULL,
        recipe_src TEXT,             -- pinned: the source the run executes
        recipe_sha TEXT,
        status TEXT NOT NULL,        -- running | waiting | succeeded | failed | cancelled
        state TEXT NOT NULL,         -- JSON projection of the events
        project_id TEXT,
        lead_session TEXT,
        forked_from TEXT,            -- run id
        fork_step TEXT,
        created_at INTEGER NOT NULL,
        ended_at INTEGER
    );
    CREATE INDEX run_by_created ON run(created_at);
    CREATE TABLE run_event (
        run_id TEXT NOT NULL REFERENCES run(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        at INTEGER NOT NULL,
        kind TEXT NOT NULL,
        step_id TEXT,
        payload TEXT NOT NULL,       -- JSON RunEvent
        PRIMARY KEY (run_id, seq)
    );
    CREATE TABLE run_message (
        id TEXT PRIMARY KEY,         -- ULID
        run_id TEXT,
        at INTEGER NOT NULL,
        sender TEXT NOT NULL,        -- a session id, @user or @ptyro
        recipient TEXT NOT NULL,     -- a session id, @user or @lead
        address TEXT NOT NULL,       -- as written: @codex, @idle, api…
        kind TEXT NOT NULL,          -- dispatch | status | question | escalation | worker_done | note | collision
        body TEXT NOT NULL,
        in_reply_to TEXT,
        priority TEXT NOT NULL DEFAULT 'normal',
        hop INTEGER NOT NULL DEFAULT 0,
        expires_at INTEGER,
        delivered_at INTEGER,
        seen_at INTEGER,
        acked_at INTEGER
    );
    CREATE INDEX run_message_by_recipient ON run_message(recipient, at);
    CREATE TABLE lease (
        id TEXT PRIMARY KEY,
        holder TEXT NOT NULL,        -- session id
        pattern TEXT NOT NULL,       -- path or glob, relative to the worktree
        project_id TEXT,
        exclusive INTEGER NOT NULL,
        reason TEXT,
        expires_at INTEGER NOT NULL,
        created_at INTEGER NOT NULL
    );",
    ),
    // UX1.24: a title, for people to read, beside the name.
    M::up(
        "ALTER TABLE session ADD COLUMN title TEXT;
    ALTER TABLE worktree ADD COLUMN title TEXT;",
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the store thread has stopped")]
    Closed,
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Job>,
}

/// A session as remembered across daemon restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub name: String,
    pub kind: SessionKind,
    pub argv: Vec<String>,
    pub cwd: String,
    pub state: ProcessState,
    pub pid: Option<u32>,
    pub daemon_run: String,
    pub created_ms: u64,
    pub ended_ms: Option<u64>,
    pub profile: String,
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub workspace: Option<String>,
    pub agent: Option<String>,
    pub script: Option<String>,
    /// The resolved launch: argv and env diff, secrets as `<secret:NAME>` (SPEC §14.2).
    pub launch: Option<serde_json::Value>,
    /// The agent's conversation id (resume, SPEC §7.4).
    pub conversation: Option<String>,
    /// Its title (UX1.24).
    #[serde(default)]
    pub title: Option<String>,
}

/// A trust approval (SPEC §10.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRow {
    pub project: String,
    pub scope: String,
    pub fingerprint: String,
    pub approved_ms: u64,
}

/// Everything the daemon keeps in memory about workspaces, projects and worktrees.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub workspaces: Vec<WorkspaceInfo>,
    pub projects: Vec<ProjectInfo>,
    /// Not removed ones only.
    pub worktrees: Vec<WorktreeInfo>,
    pub trust: Vec<TrustRow>,
}

const SESSION_COLS: &str = "id, name, kind, argv, cwd, state, pid, daemon_run, created_at, ended_at, \
                            profile, worktree_id, project_id, workspace, agent, script, launch, conversation_id, title";

/// A `session` row, as selected by [`SESSION_COLS`].
fn session_row(r: &rusqlite::Row<'_>) -> Result<SessionRecord> {
    let launch: Option<String> = r.get(16)?;
    Ok(SessionRecord {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: serde_json::from_str(&r.get::<_, String>(2)?)?,
        argv: serde_json::from_str(&r.get::<_, String>(3)?)?,
        cwd: r.get(4)?,
        state: serde_json::from_str(&r.get::<_, String>(5)?)?,
        pid: r.get(6)?,
        daemon_run: r.get(7)?,
        created_ms: r.get::<_, i64>(8)? as u64,
        ended_ms: opt_ms(r.get(9)?),
        profile: r.get(10)?,
        worktree: r.get(11)?,
        project: r.get(12)?,
        workspace: r.get(13)?,
        agent: r.get(14)?,
        script: r.get(15)?,
        launch: launch.map(|l| serde_json::from_str(&l)).transpose()?,
        conversation: r.get(17)?,
        title: r.get(18)?,
    })
}

fn opt_ms(v: Option<i64>) -> Option<u64> {
    v.map(|v| v as u64)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::start(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> Result<Self> {
        // First: the pragmas below take locks too, and a second opener (a daemon starting in a
        // race, `sqlite3`, a backup) should wait for them, not fail the store for good.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Migrations::from_slice(MIGRATIONS).to_latest(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("ptyro-store".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|_| StoreError::Closed)?;
        Ok(Self { tx })
    }

    /// Run `f` on the store thread and wait for its result.
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

    /// Run `f` on the store thread without waiting; failures are logged by the caller's `on_err`.
    pub fn spawn(&self, f: impl FnOnce(&mut Connection) -> Result<()> + Send + 'static) {
        let _ = self.tx.send(Box::new(move |c| {
            if let Err(e) = f(c) {
                // (An autospawned daemon's stderr is /dev/null: the log is where this is read.)
                tracing::warn!("ptyro store: {e}");
            }
        }));
    }

    pub fn save_session(&self, r: SessionRecord) {
        self.spawn(move |c| {
            c.execute(
                "INSERT INTO session (id, name, kind, argv, cwd, state, pid, daemon_run, created_at, ended_at,
                                      profile, worktree_id, project_id, workspace, agent, script, launch, conversation_id, title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, state = ?6, pid = ?7, ended_at = ?10, daemon_run = ?8,
                                               launch = COALESCE(?17, launch), argv = ?4, profile = ?11,
                                               conversation_id = COALESCE(?18, conversation_id), title = ?19,
                                               worktree_id = ?12, project_id = ?13",
                params![
                    r.id,
                    r.name,
                    serde_json::to_string(&r.kind)?,
                    serde_json::to_string(&r.argv)?,
                    r.cwd,
                    serde_json::to_string(&r.state)?,
                    r.pid,
                    r.daemon_run,
                    r.created_ms as i64,
                    r.ended_ms.map(|m| m as i64),
                    r.profile,
                    r.worktree,
                    r.project,
                    r.workspace,
                    r.agent,
                    r.script,
                    r.launch.as_ref().map(serde_json::Value::to_string),
                    r.conversation,
                    r.title,
                ],
            )?;
            Ok(())
        });
    }

    pub fn event(&self, session: String, kind: &'static str, detail: serde_json::Value) {
        let at = ptyro_core::now_ms() as i64;
        self.spawn(move |c| {
            c.execute(
                "INSERT INTO session_event (session_id, at, kind, detail) VALUES (?1, ?2, ?3, ?4)",
                params![session, at, kind, detail.to_string()],
            )?;
            Ok(())
        });
    }

    /// The newest `limit` events (of one session, or all), oldest first:
    /// (session, at, kind, detail).
    pub async fn events(
        &self,
        session: Option<String>,
        limit: usize,
    ) -> Result<Vec<(String, u64, String, serde_json::Value)>> {
        self.call(move |c| {
            let mut st = c.prepare(
                "SELECT session_id, at, kind, detail FROM session_event
                 WHERE ?1 IS NULL OR session_id = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2",
            )?;
            let mut rows: Vec<(String, u64, String, serde_json::Value)> = st
                .query_map(params![session, limit as i64], |r| {
                    let detail: Option<String> = r.get(3)?;
                    Ok((
                        r.get(0)?,
                        r.get::<_, i64>(1)? as u64,
                        r.get(2)?,
                        detail.and_then(|d| serde_json::from_str(&d).ok()).unwrap_or_default(),
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await
    }

    pub fn forget_session(&self, id: String) {
        self.spawn(move |c| {
            c.execute("DELETE FROM session WHERE id = ?1", params![id])?;
            Ok(())
        });
    }

    /// Sessions that were live when an earlier daemon stopped (a crash, a reboot) and can't be
    /// resumed are gone: record that, so history doesn't claim they still run. Resumable agent
    /// sessions (a conversation id) become dormant instead; [`dormant`](Self::dormant) lists them.
    pub async fn close_orphans(&self, current_run: String) -> Result<usize> {
        self.call(move |c| {
            let now = ptyro_core::now_ms() as i64;
            let lost = serde_json::to_string(&ProcessState::Failed { error: "daemon stopped".into() })?;
            let dormant = serde_json::to_string(&ProcessState::Dormant)?;
            c.execute(
                "UPDATE session SET state = ?1, pid = NULL WHERE ended_at IS NULL AND daemon_run != ?2
                   AND conversation_id IS NOT NULL AND agent IS NOT NULL",
                params![dormant, current_run],
            )?;
            Ok(c.execute(
                "UPDATE session SET state = ?1, ended_at = ?2 WHERE ended_at IS NULL AND daemon_run != ?3 AND state != ?4",
                params![lost, now, current_run, dormant],
            )?)
        })
        .await
    }

    /// Dormant sessions to bring back (oldest first).
    pub async fn dormant(&self) -> Result<Vec<SessionRecord>> {
        let dormant = serde_json::to_string(&ProcessState::Dormant)?;
        self.call(move |c| {
            let mut st = c.prepare(&format!(
                "SELECT {SESSION_COLS} FROM session WHERE state = ?1 ORDER BY created_at ASC"
            ))?;
            let mut rows = st.query(params![dormant])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                match session_row(r) {
                    Ok(rec) => out.push(rec),
                    Err(e) => tracing::warn!("skipping an unreadable session record: {e}"),
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn inbox_add(&self, e: InboxEntry) -> Result<i64> {
        self.call(move |c| {
            c.execute(
                "INSERT INTO inbox (session_id, session_name, at, kind, title, body, read_at, workspace, profile)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    e.session,
                    e.session_name,
                    e.at_ms as i64,
                    e.kind.label(),
                    e.title,
                    e.body,
                    e.read_ms.map(|m| m as i64),
                    e.workspace,
                    e.profile
                ],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
    }

    /// Newest first.
    pub async fn inbox(
        &self,
        workspace: Option<String>,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<InboxEntry>> {
        self.call(move |c| {
            let mut st = c.prepare(
                "SELECT id, session_id, session_name, at, kind, title, body, read_at, workspace, profile FROM inbox
                 WHERE (?1 IS NULL OR workspace = ?1) AND (?2 = 0 OR read_at IS NULL)
                 ORDER BY at DESC, id DESC LIMIT ?3",
            )?;
            let mut rows = st.query(params![workspace, i64::from(unread_only), limit as i64])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                let kind: String = r.get(4)?;
                out.push(InboxEntry {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    session_name: r.get(2)?,
                    at_ms: r.get::<_, i64>(3)? as u64,
                    kind: AttentionKind::parse(&kind).unwrap_or(AttentionKind::Notification),
                    title: r.get(5)?,
                    body: r.get(6)?,
                    read_ms: opt_ms(r.get(7)?),
                    workspace: r.get(8)?,
                    profile: r.get(9)?,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Mark entries read (or unread): by id, every entry of a session, or everything.
    pub async fn inbox_mark(
        &self,
        ids: Vec<i64>,
        session: Option<String>,
        all: bool,
        read: bool,
    ) -> Result<usize> {
        self.call(move |c| {
            let at: Option<i64> = read.then(|| ptyro_core::now_ms() as i64);
            let mut n = 0;
            if all {
                n += c.execute(
                    "UPDATE inbox SET read_at = ?1 WHERE (?2 IS NULL OR session_id = ?2)",
                    params![at, session],
                )?;
            }
            for id in ids {
                n += c.execute("UPDATE inbox SET read_at = ?1 WHERE id = ?2", params![at, id])?;
            }
            Ok(n)
        })
        .await
    }

    /// Newest first. A row this build can't read (written by a newer one, or damaged) is
    /// skipped and logged, not allowed to hide every other session.
    pub async fn history(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        self.call(move |c| {
            let mut st = c.prepare(&format!(
                "SELECT {SESSION_COLS} FROM session ORDER BY created_at DESC LIMIT ?1"
            ))?;
            let mut rows = st.query(params![limit as i64])?;
            let mut out = Vec::new();
            while let Some(r) = rows.next()? {
                match session_row(r) {
                    Ok(rec) => out.push(rec),
                    Err(e) => tracing::warn!("skipping an unreadable session record: {e}"),
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn kv_get(&self, key: String) -> Result<Option<serde_json::Value>> {
        self.call(move |c| {
            let v: Option<String> = c
                .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
                .optional()?;
            Ok(v.map(|s| serde_json::from_str(&s)).transpose()?)
        })
        .await
    }

    pub async fn kv_set(&self, key: String, value: serde_json::Value) -> Result<()> {
        self.call(move |c| {
            c.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
                params![key, value.to_string(), ptyro_core::now_ms() as i64],
            )?;
            Ok(())
        })
        .await
    }

    /// A consistent copy of the database at `path` (SQLite's `VACUUM INTO`), safe while the daemon
    /// is writing it: the copy is one moment of the data, never half of two. `path` must not exist.
    pub async fn backup_to(&self, path: std::path::PathBuf) -> Result<()> {
        self.call(move |c| {
            c.execute("VACUUM INTO ?1", params![path.to_string_lossy().into_owned()])?;
            Ok(())
        })
        .await
    }

    /// Retention (SPEC §14.2): events older than 90 days, ended sessions older than 30 days.
    pub async fn prune(&self) -> Result<()> {
        self.call(|c| {
            let now = ptyro_core::now_ms() as i64;
            c.execute("DELETE FROM session_event WHERE at < ?1", params![now - 90 * 86_400_000])?;
            c.execute(
                "DELETE FROM inbox WHERE read_at IS NOT NULL AND read_at < ?1",
                params![now - 30 * 86_400_000],
            )?;
            c.execute(
                "DELETE FROM session WHERE ended_at IS NOT NULL AND ended_at < ?1",
                params![now - 30 * 86_400_000],
            )?;
            Ok(())
        })
        .await
    }

    /// Workspaces (with members in order), projects, live worktrees and trust.
    pub async fn load_registry(&self) -> Result<Registry> {
        self.call(|c| {
            let mut reg = Registry::default();
            let mut st = c.prepare("SELECT id, name, profile, notify, created_at FROM workspace ORDER BY created_at")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                reg.workspaces.push(WorkspaceInfo {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    profile: r.get(2)?,
                    notify: r.get(3)?,
                    projects: Vec::new(),
                    created_ms: r.get::<_, i64>(4)? as u64,
                });
            }
            let mut st = c.prepare("SELECT workspace_id, project_id FROM workspace_project ORDER BY position")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let (ws, p): (String, String) = (r.get(0)?, r.get(1)?);
                if let Some(w) = reg.workspaces.iter_mut().find(|w| w.id == ws) {
                    w.projects.push(p);
                }
            }
            let mut st = c.prepare(
                "SELECT id, slug, root_path, default_branch, remote_url, profile, created_at, last_used_at
                 FROM project ORDER BY created_at",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                reg.projects.push(ProjectInfo {
                    id: r.get(0)?,
                    slug: r.get(1)?,
                    root: r.get::<_, String>(2)?.into(),
                    default_branch: r.get(3)?,
                    remote_url: r.get(4)?,
                    pin: r.get(5)?,
                    created_ms: r.get::<_, i64>(6)? as u64,
                    last_used_ms: opt_ms(r.get(7)?),
                });
            }
            let mut st = c.prepare(
                "SELECT w.id, w.project_id, p.slug, w.name, w.path, w.branch, w.base, w.is_root, w.profile, w.state,
                        w.port_base, w.port_count, w.created_at, w.removed_at, w.last_used_at, w.title
                 FROM worktree w JOIN project p ON p.id = w.project_id
                 WHERE w.state != 'removed' ORDER BY w.is_root DESC, w.created_at",
            )?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let state: String = r.get(9)?;
                let port_base: Option<i64> = r.get(10)?;
                let port_count: Option<i64> = r.get(11)?;
                reg.worktrees.push(WorktreeInfo {
                    id: r.get(0)?,
                    project: r.get(1)?,
                    project_slug: r.get(2)?,
                    name: r.get(3)?,
                    path: r.get::<_, String>(4)?.into(),
                    branch: r.get(5)?,
                    base: r.get(6)?,
                    is_root: r.get::<_, i64>(7)? != 0,
                    profile: r.get(8)?,
                    state: serde_json::from_value(serde_json::Value::String(state))?,
                    ports: port_base.zip(port_count).map(|(b, n)| PortBlock { base: b as u16, count: n as u16 }),
                    created_ms: r.get::<_, i64>(12)? as u64,
                    removed_ms: opt_ms(r.get(13)?),
                    last_used_ms: opt_ms(r.get(14)?),
                    status: None,
                    locked: false,
                    pr: None,
                    title: r.get(15)?,
                    integrated: None,
                });
            }
            let mut st = c.prepare("SELECT project_id, scope, fingerprint, approved_at FROM trust")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                reg.trust.push(TrustRow {
                    project: r.get(0)?,
                    scope: r.get(1)?,
                    fingerprint: r.get(2)?,
                    approved_ms: r.get::<_, i64>(3)? as u64,
                });
            }
            Ok(reg)
        })
        .await
    }

    /// Insert or update a workspace and its member list.
    pub fn save_workspace(&self, w: WorkspaceInfo) {
        self.spawn(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO workspace (id, name, profile, notify, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, profile = ?3, notify = ?4, updated_at = ?6",
                params![w.id, w.name, w.profile, w.notify, w.created_ms as i64, ptyro_core::now_ms() as i64],
            )?;
            tx.execute("DELETE FROM workspace_project WHERE workspace_id = ?1", params![w.id])?;
            for (i, p) in w.projects.iter().enumerate() {
                tx.execute(
                    "INSERT INTO workspace_project (workspace_id, project_id, position) VALUES (?1, ?2, ?3)",
                    params![w.id, p, i as i64],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }

    pub fn delete_workspace(&self, id: String) {
        self.spawn(move |c| {
            c.execute("DELETE FROM workspace WHERE id = ?1", params![id])?;
            Ok(())
        });
    }

    pub fn save_project(&self, p: ProjectInfo) {
        self.spawn(move |c| {
            c.execute(
                "INSERT INTO project (id, slug, root_path, default_branch, remote_url, profile, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET slug = ?2, root_path = ?3, default_branch = ?4, remote_url = ?5,
                                               profile = ?6, last_used_at = ?8",
                params![
                    p.id,
                    p.slug,
                    p.root.to_string_lossy(),
                    p.default_branch,
                    p.remote_url,
                    p.pin,
                    p.created_ms as i64,
                    p.last_used_ms.map(|m| m as i64)
                ],
            )?;
            Ok(())
        });
    }

    /// Forget a project; its worktree rows and trust go with it (history stays in `session`).
    pub fn delete_project(&self, id: String) {
        self.spawn(move |c| {
            c.execute("DELETE FROM project WHERE id = ?1", params![id])?;
            Ok(())
        });
    }

    pub fn save_worktree(&self, w: WorktreeInfo) {
        self.spawn(move |c| {
            c.execute(
                "INSERT INTO worktree (id, project_id, name, path, branch, base, is_root, profile, state,
                                       port_base, port_count, created_at, removed_at, last_used_at, title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(id) DO UPDATE SET name = ?3, path = ?4, branch = ?5, base = ?6, profile = ?8, state = ?9,
                                               port_base = ?10, port_count = ?11, removed_at = ?13, last_used_at = ?14,
                                               title = ?15",
                params![
                    w.id,
                    w.project,
                    w.name,
                    w.path.to_string_lossy(),
                    w.branch,
                    w.base,
                    i64::from(w.is_root),
                    w.profile,
                    w.state.label(),
                    w.ports.map(|p| i64::from(p.base)),
                    w.ports.map(|p| i64::from(p.count)),
                    w.created_ms as i64,
                    w.removed_ms.map(|m| m as i64),
                    w.last_used_ms.map(|m| m as i64),
                    w.title,
                ],
            )?;
            Ok(())
        });
    }

    pub fn save_trust(&self, t: TrustRow) {
        self.spawn(move |c| {
            c.execute(
                "INSERT INTO trust (project_id, scope, fingerprint, approved_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, scope) DO UPDATE SET fingerprint = ?3, approved_at = ?4",
                params![t.project, t.scope, t.fingerprint, t.approved_ms as i64],
            )?;
            Ok(())
        });
    }

    pub fn delete_trust(&self, project: String, scope: String) {
        self.spawn(move |c| {
            c.execute(
                "DELETE FROM trust WHERE project_id = ?1 AND scope = ?2",
                params![project, scope],
            )?;
            Ok(())
        });
    }

    /// `ptyro profile rename`: every reference to `old` becomes `new` (SPEC §10.10).
    pub async fn rename_profile(&self, old: String, new: String) -> Result<usize> {
        self.call(move |c| {
            let tx = c.transaction()?;
            let mut n = 0;
            for sql in [
                "UPDATE workspace SET profile = ?2 WHERE profile = ?1",
                "UPDATE project SET profile = ?2 WHERE profile = ?1",
                "UPDATE worktree SET profile = ?2 WHERE profile = ?1",
                "UPDATE session SET profile = ?2 WHERE profile = ?1",
            ] {
                n += tx.execute(sql, params![old, new])?;
            }
            tx.commit()?;
            Ok(n)
        })
        .await
    }

    /// Every session id the store remembers.
    pub async fn session_ids(&self) -> Result<std::collections::HashSet<String>> {
        self.call(|c| {
            let mut st = c.prepare("SELECT id FROM session")?;
            Ok(st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Wait until every queued write has been applied.
    pub async fn flush(&self) -> Result<()> {
        self.call(|_| Ok(())).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::model::WorktreeState;

    #[test]
    fn migrations_are_valid() {
        Migrations::from_slice(MIGRATIONS).validate().unwrap();
    }

    fn record(id: &str, run: &str) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            name: format!("n-{id}"),
            kind: SessionKind::Shell,
            argv: vec!["sh".into()],
            cwd: "/".into(),
            state: ProcessState::Running,
            pid: Some(42),
            daemon_run: run.into(),
            created_ms: ptyro_core::now_ms(),
            ended_ms: None,
            profile: "default".into(),
            worktree: None,
            project: None,
            workspace: None,
            agent: None,
            script: None,
            launch: Some(serde_json::json!({"argv": ["sh"]})),
            conversation: None,
            title: Some(format!("the title of {id}")),
        }
    }

    /// A session row this build can't decode (a state a newer build wrote) hides nothing else,
    /// and dormant sessions are found however many newer rows there are.
    #[tokio::test]
    async fn one_unreadable_row_does_not_hide_the_rest() {
        let s = Store::open_in_memory().unwrap();
        let mut dormant = record("sleeper", "old");
        dormant.agent = Some("claude".into());
        dormant.conversation = Some("conv-9".into());
        dormant.state = ProcessState::Dormant;
        dormant.created_ms = 1;
        s.save_session(dormant);
        s.save_session(record("fine", "run"));
        s.call(|c| {
            c.execute(
                "INSERT INTO session (id, name, kind, argv, cwd, state, daemon_run, created_at, profile)
                 VALUES ('from-the-future', 'x', '\"shell\"', '[]', '/', '{\"t\":\"a_new_state\"}', 'run', 5, 'default')",
                [],
            )?;
            // Plenty of newer rows after the dormant one: the old `history(10_000)` limit.
            for n in 0..30 {
                c.execute(
                    "INSERT INTO session (id, name, kind, argv, cwd, state, daemon_run, created_at, profile)
                     VALUES (?1, ?1, '\"shell\"', '[]', '/', '{\"t\":\"exited\",\"code\":0}', 'run', ?2, 'default')",
                    params![format!("e{n}"), 100 + n],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let h = s.history(1000).await.unwrap();
        assert!(h.iter().any(|r| r.id == "fine") && h.iter().all(|r| r.id != "from-the-future"));
        let d = s.dormant().await.unwrap();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].id, "sleeper");
    }

    /// A run event with a sequence number already stored is skipped, not a reason to lose the
    /// projection that came with it.
    #[tokio::test]
    async fn a_repeated_run_event_does_not_abort_the_transaction() {
        let s = Store::open_in_memory().unwrap();
        let row = RunRow {
            id: "r-dup".into(),
            kind: "recipe".into(),
            name: "n".into(),
            recipe_src: None,
            recipe_sha: None,
            status: "running".into(),
            state: serde_json::json!({}),
            project: None,
            lead: None,
            forked_from: None,
            fork_step: None,
            created_ms: 1,
            ended_ms: None,
        };
        let ev = |seq: u64| (seq, 1, "note".to_owned(), None, serde_json::json!({"e": "note"}));
        s.run_append(row.clone(), vec![ev(1), ev(2)]);
        let mut done = row;
        done.status = "succeeded".into();
        s.run_append(done, vec![ev(2), ev(3)]);
        assert_eq!(s.run("r-dup".into()).await.unwrap().unwrap().status, "succeeded");
        assert_eq!(s.run_events("r-dup".into()).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn dormant_and_inbox() {
        let s = Store::open_in_memory().unwrap();
        let mut r = record("agent", "old");
        r.agent = Some("claude".into());
        r.conversation = Some("conv-1".into());
        s.save_session(r);
        s.save_session(record("plain", "old"));
        s.close_orphans("new".into()).await.unwrap();
        let d = s.dormant().await.unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].conversation.as_deref(), Some("conv-1"));
        let h = s.history(10).await.unwrap();
        assert!(matches!(
            h.iter().find(|x| x.id == "plain").unwrap().state,
            ProcessState::Failed { .. }
        ));
        let e = InboxEntry {
            id: 0,
            session: "agent".into(),
            session_name: "claude".into(),
            at_ms: 5,
            kind: AttentionKind::Blocked,
            title: "[work] claude (app)".into(),
            body: "wants permission: Bash(ls)".into(),
            read_ms: None,
            workspace: Some("main".into()),
            profile: Some("work".into()),
        };
        let id = s.inbox_add(e.clone()).await.unwrap();
        s.inbox_add(InboxEntry { at_ms: 6, workspace: Some("side".into()), ..e.clone() })
            .await
            .unwrap();
        assert_eq!(s.inbox(None, false, 10).await.unwrap().len(), 2);
        assert_eq!(s.inbox(Some("main".into()), false, 10).await.unwrap()[0].id, id);
        s.inbox_mark(vec![id], None, false, true).await.unwrap();
        assert_eq!(s.inbox(None, true, 10).await.unwrap().len(), 1);
        s.inbox_mark(vec![], Some("agent".into()), true, true).await.unwrap();
        assert!(s.inbox(None, true, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn registry_round_trips() {
        let s = Store::open_in_memory().unwrap();
        let p = ProjectInfo {
            id: "p1".into(),
            slug: "repo".into(),
            root: "/r".into(),
            default_branch: Some("main".into()),
            remote_url: None,
            pin: Some("work".into()),
            created_ms: 1,
            last_used_ms: None,
        };
        s.save_project(p.clone());
        let ws = WorkspaceInfo {
            id: "w1".into(),
            name: "main".into(),
            profile: "default".into(),
            notify: "all".into(),
            projects: vec!["p1".into()],
            created_ms: 1,
        };
        s.save_workspace(ws.clone());
        let mut wt = WorktreeInfo {
            id: "t1".into(),
            project: "p1".into(),
            project_slug: "repo".into(),
            name: "brisk-rook".into(),
            path: "/w/brisk-rook".into(),
            branch: Some("ptyro/brisk-rook".into()),
            base: Some("origin/main".into()),
            is_root: false,
            profile: Some("work".into()),
            state: WorktreeState::Ready,
            ports: Some(PortBlock { base: 20000, count: 10 }),
            created_ms: 2,
            removed_ms: None,
            last_used_ms: None,
            status: None,
            locked: false,
            pr: None,
            title: Some("Fix the login bug".into()),
            integrated: None,
        };
        s.save_worktree(wt.clone());
        s.save_trust(TrustRow {
            project: "p1".into(),
            scope: "scripts".into(),
            fingerprint: "abc".into(),
            approved_ms: 3,
        });
        let reg = s.load_registry().await.unwrap();
        assert_eq!(reg.projects, vec![p]);
        assert_eq!(reg.workspaces, vec![ws]);
        assert_eq!(reg.worktrees, vec![wt.clone()]);
        assert_eq!(reg.trust.len(), 1);
        // A removed worktree frees its name for a new one.
        wt.state = WorktreeState::Removed;
        s.save_worktree(wt.clone());
        s.save_worktree(WorktreeInfo { id: "t2".into(), state: WorktreeState::Ready, ..wt });
        let reg = s.load_registry().await.unwrap();
        assert_eq!(reg.worktrees.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["t2"]);
        // The pin and both worktree rows (history included).
        assert_eq!(s.rename_profile("work".into(), "acme".into()).await.unwrap(), 3);
        let reg = s.load_registry().await.unwrap();
        assert_eq!(reg.projects[0].pin.as_deref(), Some("acme"));
        s.delete_project("p1".into());
        let reg = s.load_registry().await.unwrap();
        assert!(
            reg.projects.is_empty() && reg.worktrees.is_empty() && reg.trust.is_empty(),
            "cascades"
        );
        assert_eq!(reg.workspaces[0].projects, Vec::<String>::new());
    }

    #[tokio::test]
    async fn sessions_kv_and_orphans() {
        let s = Store::open_in_memory().unwrap();
        s.save_session(record("a", "run1"));
        s.save_session(record("b", "run2"));
        s.event("a".into(), "process", serde_json::json!({"state": "running"}));
        assert_eq!(s.close_orphans("run2".into()).await.unwrap(), 1);
        let h = s.history(10).await.unwrap();
        let a = h.iter().find(|r| r.id == "a").unwrap();
        assert!(matches!(a.state, ProcessState::Failed { .. }) && a.ended_ms.is_some());
        assert_eq!(h.iter().find(|r| r.id == "b").unwrap().state, ProcessState::Running);

        assert_eq!(s.kv_get("view:main".into()).await.unwrap(), None);
        s.kv_set("view:main".into(), serde_json::json!({"panes": 2})).await.unwrap();
        assert_eq!(
            s.kv_get("view:main".into()).await.unwrap(),
            Some(serde_json::json!({"panes": 2}))
        );
        s.forget_session("a".into());
        s.flush().await.unwrap();
        assert_eq!(s.history(10).await.unwrap().len(), 1);
        s.prune().await.unwrap();
    }

    #[test]
    fn reopening_a_file_keeps_data() {
        let dir = std::env::temp_dir().join(format!("ptyro-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let s = Store::open(&path).unwrap();
            s.kv_set("k".into(), serde_json::json!(1)).await.unwrap();
        });
        rt.block_on(async {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.kv_get("k".into()).await.unwrap(), Some(serde_json::json!(1)));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_backup_is_a_whole_copy_that_opens_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ptyro.db");
        let store = Store::open(&db).unwrap();
        store.kv_set("view:a".into(), serde_json::json!({ "n": 1 })).await.unwrap();
        let copy = dir.path().join("copy.db");
        store.backup_to(copy.clone()).await.unwrap();
        // Written after the copy: not in it.
        store.kv_set("view:b".into(), serde_json::json!({ "n": 2 })).await.unwrap();
        let again = Store::open(&copy).unwrap();
        assert_eq!(
            again.kv_get("view:a".into()).await.unwrap(),
            Some(serde_json::json!({ "n": 1 }))
        );
        assert_eq!(again.kv_get("view:b".into()).await.unwrap(), None);
        // It won't write over a file that is there.
        assert!(store.backup_to(copy).await.is_err());
    }
}
