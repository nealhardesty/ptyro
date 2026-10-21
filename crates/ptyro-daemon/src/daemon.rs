//! Daemon-wide state: the session registry, address resolution and request handling.
//!
//! Session mutations go through one lock, which serializes them the way SPEC §4.2's supervisor
//! would. Workspaces, projects and worktrees live in [`Registry`] (another lock), mirrored to the
//! store; their operations are in `registry.rs`, `worktrees.rs` and `launch.rs`.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ptyro_core::config::Config;
use ptyro_core::ids::short_handle;
use ptyro_core::model::MAIN_WORKSPACE;
use ptyro_core::paths::Paths;
use ptyro_core::session::{HistoryEntry, SessionInfo, SessionKind, SpawnSpec};
use ptyro_core::{ClientId, Error, ErrorCode, SessionId, TermSize, names, now_ms};
use ptyro_protocol::{DaemonStatus, DomainEvent, PROTO, Reply, Request};
use ptyro_store::{Registry, Store};
use tokio::sync::{broadcast, mpsc, watch};

use crate::launch::Launch;
use crate::session::{Place, Session};

pub struct Daemon {
    pub paths: Paths,
    pub started_ms: u64,
    /// Identifies this daemon instance in the store (sessions from earlier runs are history).
    pub run_id: String,
    sessions: RwLock<Vec<Arc<Session>>>,
    pub events: broadcast::Sender<DomainEvent>,
    next_client: AtomicU64,
    pub clients: AtomicU32,
    /// Set once shutdown starts; connections send `Goodbye` and close.
    pub shutdown: watch::Sender<bool>,
    /// The environment the daemon started with: the base for every child (SPEC §5.1).
    pub(crate) base_env: Vec<(OsString, OsString)>,
    pub store: Option<Store>,
    pub config: RwLock<Config>,
    /// The session most recently focused in any TUI (`@focused`).
    focused: Mutex<Option<SessionId>>,
    /// Views saved while the store is unavailable.
    views: Mutex<BTreeMap<String, serde_json::Value>>,
    /// Workspaces, projects, worktrees and trust (SPEC §14.2), mirrored to the store.
    pub registry: Mutex<Registry>,
    /// Resolved secrets kept for `secret_ttl` (memory only, SPEC §10.10).
    pub(crate) secrets: Mutex<HashMap<String, (String, std::time::Instant)>>,
    /// Worktree ids whose repository status should be recomputed.
    pub(crate) status_tx: mpsc::UnboundedSender<String>,
    pub(crate) status_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    /// File watchers per worktree id.
    pub(crate) watchers: Mutex<HashMap<String, crate::background::Watcher>>,
    /// Every session's signals go through here, to one processor (`agents.rs`).
    pub(crate) feed_tx: mpsc::UnboundedSender<crate::session::Feed>,
    feed_rx: Mutex<Option<mpsc::UnboundedReceiver<crate::session::Feed>>>,
    /// What each client shows in its focused pane, and whether its terminal has focus.
    pub(crate) viewing: Mutex<HashMap<ClientId, (SessionId, bool)>>,
    /// Muted notification targets → until (ms; `None` = until unmuted).
    pub(crate) mutes: Mutex<crate::notifier::Mutes>,
    pub(crate) last_notified: Mutex<HashMap<SessionId, u64>>,
    /// The inbox when the store is unavailable.
    pub(crate) inbox_mem: Mutex<Vec<ptyro_core::status::InboxEntry>>,
    /// Approve/deny link nonces already used → their expiry.
    pub(crate) used_links: Mutex<HashMap<String, u64>>,
    /// Usage per (profile, agent).
    pub(crate) usage: Mutex<HashMap<(String, String), ptyro_core::status::Usage>>,
    pub(crate) codex_usage: Mutex<std::collections::HashSet<String>>,
    /// OpenCode servers: session → (port, password).
    pub(crate) sidecars: Mutex<HashMap<SessionId, (u16, String)>>,
    /// Sessions whose sidecar event stream is connected right now.
    pub(crate) sidecar_live: Mutex<std::collections::HashSet<SessionId>>,
    pub(crate) oc_pending: Mutex<crate::sidecar::Pending>,
    /// Problems worth showing in `ptyro doctor` (deduplicated).
    pub(crate) warnings: Mutex<Vec<String>>,
    /// The PR-event loop's memory per worktree (M6.10).
    pub(crate) pr_watches: Mutex<crate::github::Watches>,
    /// Line comments per worktree, waiting to be sent (M6.3).
    pub(crate) review_comments: Mutex<HashMap<String, Vec<ptyro_core::model::ReviewComment>>>,
    pub(crate) reviewed: Mutex<crate::review::Reviewed>,
    /// Pair mode per project (M6.7).
    pub(crate) pairs: Mutex<crate::review::Pairs>,
    /// Agent spawns held by `review.max_unreviewed` (M6.9).
    pub(crate) spawn_queue: Mutex<std::collections::VecDeque<ptyro_core::session::SpawnSpec>>,
    /// Runs: recipes, fan-outs, leads (M7.1).
    pub(crate) runs: Mutex<crate::runs::Runs>,
    /// The open part of the message ledger (M7.7).
    pub(crate) mail: Mutex<Vec<ptyro_store::MessageRow>>,
    /// File leases (M7.13).
    pub(crate) leases: Mutex<Vec<ptyro_store::LeaseRow>>,
    /// Collisions already reported (pair + files).
    pub(crate) radar_seen: Mutex<std::collections::HashSet<String>>,
    /// One mail delivery pass at a time (two would type the same message twice).
    pub(crate) mail_pass: tokio::sync::Mutex<()>,
    // ── live upgrade (M8.1) ──
    pub(crate) upgrading: std::sync::atomic::AtomicBool,
    /// Tells every PTY reader to park.
    pub(crate) io_pause: watch::Sender<bool>,
    pub(crate) io: crate::session::IoControl,
    /// Some(reason): connections say goodbye and close (an upgrade).
    pub(crate) goodbye: watch::Sender<Option<String>>,
    pub generation: AtomicU32,
    pub(crate) listener_fd: std::sync::atomic::AtomicI32,
    pub(crate) foreground: std::sync::atomic::AtomicBool,
    pub(crate) reaper: Arc<crate::handoff::Reaper>,
    /// This image came from a handoff (agents keep working; no "the daemon restarted" nudges).
    pub handed_off: std::sync::atomic::AtomicBool,
    pub rolled_back: std::sync::atomic::AtomicBool,
    pub upgraded_ms: AtomicU64,
    /// The watchdog's lag tick (M8.5).
    pub(crate) lag: Arc<crate::watchdog::Lag>,
    // ── remote (M9) ──
    pub(crate) devices: Mutex<Vec<crate::devices::DeviceRecord>>,
    /// Bumped on revocation: device connections re-check that they're still paired.
    pub(crate) devices_epoch: watch::Sender<u64>,
    pub(crate) vapid: Mutex<Option<Arc<crate::push::Vapid>>>,
    /// Claude permission prompts held for a remote answer (M9.5).
    pub(crate) held: Mutex<HashMap<SessionId, crate::approvals::Held>>,
    pub(crate) serve_child: crate::serve_child::ServeChild,
    // ── change sets (M11) ──
    /// Per set name: its actor's memory and the push or land in progress.
    pub(crate) cs: Mutex<HashMap<String, crate::changesets::CsLive>>,
    /// Launches in flight: sessions relaunching, names being started (`launch.rs` `Claim`).
    pub(crate) launching:
        Mutex<(std::collections::HashSet<SessionId>, std::collections::HashSet<String>)>,
    /// Snapshot writes (`save_kv`): the latest generation per key, and one writer at a time.
    pub(crate) kv_gens: Arc<Mutex<HashMap<String, u64>>>,
    pub(crate) kv_writer: Arc<tokio::sync::Mutex<()>>,
    pub(crate) project_adding: tokio::sync::Mutex<()>,
    /// Lead spawns, one at a time (their limits are check-then-act).
    pub(crate) lead_spawning: tokio::sync::Mutex<()>,
}

impl Daemon {
    pub fn new(
        paths: Paths,
        store: Option<Store>,
        config: Config,
        registry: Registry,
    ) -> Arc<Self> {
        Self::with_identity(paths, store, config, registry, None)
    }

    /// `identity`: `(run_id, started_ms)` carried over by a live upgrade.
    pub fn with_identity(
        paths: Paths,
        store: Option<Store>,
        config: Config,
        registry: Registry,
        identity: Option<(String, u64)>,
    ) -> Arc<Self> {
        let (status_tx, status_rx) = mpsc::unbounded_channel();
        let (feed_tx, feed_rx) = mpsc::unbounded_channel();
        let (io_pause, pause_rx) = watch::channel(false);
        let (run_id, started_ms) = identity.unwrap_or_else(|| (SessionId::new().full(), now_ms()));
        Arc::new(Self {
            paths,
            started_ms,
            run_id,
            sessions: RwLock::new(Vec::new()),
            events: broadcast::channel(1024).0,
            next_client: AtomicU64::new(1),
            clients: AtomicU32::new(0),
            shutdown: watch::Sender::new(false),
            // The handoff's own variables are for this process, never its children.
            base_env: std::env::vars_os()
                .filter(|(k, _)| !k.to_string_lossy().starts_with("PTYRO_HANDOFF_"))
                .collect(),
            store,
            config: RwLock::new(config),
            focused: Mutex::new(None),
            views: Mutex::new(BTreeMap::new()),
            registry: Mutex::new(registry),
            secrets: Mutex::new(HashMap::new()),
            status_tx,
            status_rx: Mutex::new(Some(status_rx)),
            watchers: Mutex::new(HashMap::new()),
            feed_tx,
            feed_rx: Mutex::new(Some(feed_rx)),
            viewing: Mutex::new(HashMap::new()),
            mutes: Mutex::new(HashMap::new()),
            last_notified: Mutex::new(HashMap::new()),
            inbox_mem: Mutex::new(Vec::new()),
            used_links: Mutex::new(HashMap::new()),
            usage: Mutex::new(HashMap::new()),
            codex_usage: Mutex::new(std::collections::HashSet::new()),
            sidecars: Mutex::new(HashMap::new()),
            sidecar_live: Mutex::new(std::collections::HashSet::new()),
            oc_pending: Mutex::new(HashMap::new()),
            warnings: Mutex::new(Vec::new()),
            pr_watches: Mutex::new(HashMap::new()),
            review_comments: Mutex::new(HashMap::new()),
            reviewed: Mutex::new(HashMap::new()),
            pairs: Mutex::new(HashMap::new()),
            spawn_queue: Mutex::new(std::collections::VecDeque::new()),
            runs: Mutex::new(HashMap::new()),
            mail: Mutex::new(Vec::new()),
            leases: Mutex::new(Vec::new()),
            radar_seen: Mutex::new(std::collections::HashSet::new()),
            mail_pass: tokio::sync::Mutex::new(()),
            upgrading: std::sync::atomic::AtomicBool::new(false),
            io_pause,
            io: crate::session::IoControl {
                pause: pause_rx,
                readers: Arc::new(AtomicU64::new(0)),
                parked: Arc::new(AtomicU64::new(0)),
            },
            goodbye: watch::Sender::new(None),
            generation: AtomicU32::new(0),
            listener_fd: std::sync::atomic::AtomicI32::new(-1),
            foreground: std::sync::atomic::AtomicBool::new(false),
            reaper: Arc::new(crate::handoff::Reaper::default()),
            handed_off: std::sync::atomic::AtomicBool::new(false),
            rolled_back: std::sync::atomic::AtomicBool::new(false),
            upgraded_ms: AtomicU64::new(0),
            lag: Arc::new(crate::watchdog::Lag::default()),
            devices: Mutex::new(Vec::new()),
            devices_epoch: watch::Sender::new(0),
            vapid: Mutex::new(None),
            held: Mutex::new(HashMap::new()),
            serve_child: crate::serve_child::ServeChild::default(),
            cs: Mutex::new(HashMap::new()),
            launching: Mutex::new(Default::default()),
            kv_gens: Arc::new(Mutex::new(HashMap::new())),
            kv_writer: Arc::new(tokio::sync::Mutex::new(())),
            project_adding: tokio::sync::Mutex::new(()),
            lead_spawning: tokio::sync::Mutex::new(()),
        })
    }

    pub(crate) fn hub(&self) -> crate::session::Hub {
        crate::session::Hub {
            events: self.events.clone(),
            store: self.store.clone(),
            run_id: self.run_id.clone(),
            feed: self.feed_tx.clone(),
            io: self.io.clone(),
        }
    }

    /// Where a session records, if it does (`[recorder]`, an agent's `record = false`).
    pub(crate) fn record_spec(&self, id: SessionId, agent: Option<&str>) -> Option<(PathBuf, u64)> {
        let config = self.config();
        if !config.recorder.enabled {
            return None;
        }
        if agent.is_some_and(|a| ptyro_agents::def(&config, a).record == Some(false)) {
            return None;
        }
        let max = config.recorder.max_bytes().unwrap_or(16 << 20);
        Some((self.paths.data.join("sessions").join(id.full()), max))
    }

    /// Delete recordings of sessions nobody remembers any more (the store forgets ended
    /// sessions after 30 days).
    pub async fn prune_recordings(&self) {
        let Some(store) = &self.store else { return };
        let Ok(known) = store.session_ids().await else { return };
        let live: std::collections::HashSet<String> =
            self.sessions().iter().map(|s| s.id.full()).collect();
        let dir = self.paths.data.join("sessions");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.parse::<SessionId>().is_ok() && !known.contains(&name) && !live.contains(&name)
            {
                let _ = std::fs::remove_dir_all(e.path());
                tracing::debug!(session = %name, "recording pruned");
            }
        }
    }

    pub(crate) fn focused_id(&self) -> Option<SessionId> {
        *self.focused.lock().expect("focused")
    }

    /// Bring the previous image's sessions back (live upgrade, M8.1).
    pub fn adopt_sessions(self: &Arc<Self>, image: &mut crate::handoff::Image) {
        self.reaper.start();
        let imgs = std::mem::take(&mut image.sessions);
        let emus: Vec<std::io::Result<ptyro_term::AlacrittyEmulator>> = std::thread::scope(|sc| {
            let hs: Vec<_> =
                imgs.iter().map(|i| sc.spawn(move || Session::decode_emulator(i))).collect();
            hs.into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(std::io::Error::other("decode panicked"))))
                .collect()
        });
        for (img, emu) in imgs.into_iter().zip(emus) {
            let id = img.id.clone();
            let record =
                id.parse().ok().and_then(|i| self.record_spec(i, img.meta.place.agent.as_deref()));
            let adopted =
                emu.and_then(|emu| Session::adopt(img, emu, self.hub(), &self.reaper, record));
            match adopted {
                Ok(s) => self.push_session(s),
                Err(e) => tracing::error!(session = %id, "not adopted: {e}"),
            }
        }
        if let Some(f) = image.focused.as_deref().and_then(|f| f.parse().ok()) {
            self.set_focused(f);
        }
        for (s, port, password) in std::mem::take(&mut image.sidecars) {
            if let Some(sess) = s.parse().ok().and_then(|i| self.session(i)).filter(|x| x.is_live())
            {
                self.start_sidecar(&sess, ptyro_agents::Sidecar::OpenCode { port, password });
            }
        }
        for s in self.sessions() {
            if s.is_live() {
                s.nudge_redraw();
            }
        }
    }

    /// Start the signal processor and the agent ticks. Call once, inside the runtime.
    pub fn start_agents(self: &Arc<Self>) {
        if let Some(rx) = self.feed_rx.lock().expect("feed").take() {
            self.start_feed(rx);
        }
        self.start_agent_ticks();
        self.start_callback();
    }

    /// Bring back agent sessions an earlier daemon left resumable (SPEC §7.5).
    pub async fn restore_dormant(self: &Arc<Self>) {
        let Some(store) = self.store.clone() else { return };
        if let Ok(Some(v)) = store.kv_get("mutes".into()).await
            && let Ok(m) = serde_json::from_value(v)
        {
            *self.mutes.lock().expect("mutes") = m;
        }
        let mode = self.config().daemon.restore_on_start;
        if mode == ptyro_core::config::RestoreOnStart::None {
            return;
        }
        let records = match store.dormant().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("store: {e}");
                return;
            }
        };
        let scrollback = self.config().daemon.scrollback_lines;
        let mut restored = Vec::new();
        for r in records {
            let Ok(id) = r.id.parse::<SessionId>() else { continue };
            if self.session(id).is_some() {
                continue;
            }
            let adapter = r.agent.as_deref().map(|a| ptyro_agents::adapter_name(&self.config(), a));
            let spec = crate::session::DormantSpec {
                name: if self.name_taken(&r.name) {
                    self.free_name(&r.name)
                } else {
                    r.name.clone()
                },
                kind: r.kind,
                argv: r.argv.clone(),
                cwd: r.cwd.clone().into(),
                place: Place {
                    profile: r.profile.clone(),
                    worktree: r.worktree.clone(),
                    project: r.project.clone(),
                    workspace: r.workspace.clone(),
                    agent: r.agent.clone(),
                    script: r.script.clone(),
                },
                adapter,
                conversation: r.conversation.clone(),
                ended_ms: r.ended_ms,
                structured: r
                    .launch
                    .as_ref()
                    .and_then(|l| l["structured"].as_str())
                    .map(str::to_owned),
            };
            let s = crate::session::Session::dormant(
                id,
                spec,
                TermSize::DEFAULT,
                scrollback,
                self.hub(),
            );
            s.save(None);
            let dir = self.paths.data.join("sessions").join(id.full());
            if dir.is_dir() {
                s.load_recording(dir);
            }
            self.push_session(Arc::clone(&s));
            restored.push(s);
        }
        if !restored.is_empty() {
            tracing::info!("{} dormant agent session(s) restored", restored.len());
        }
        if mode == ptyro_core::config::RestoreOnStart::Resume {
            // Staggered, three at a time (SPEC §7.5).
            let me = Arc::clone(self);
            tokio::spawn(async move {
                for chunk in restored.chunks(3) {
                    let ids: Vec<String> = chunk.iter().map(|s| s.id.full()).collect();
                    let tasks = ids.iter().map(|id| me.resume(id));
                    for r in futures::future::join_all(tasks).await {
                        if let Err(e) = r {
                            tracing::warn!("resume: {}", e.message);
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
        }
    }

    pub fn next_client_id(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    pub fn config(&self) -> Config {
        self.config.read().expect("config").clone()
    }

    pub fn sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.read().expect("sessions").clone()
    }

    pub fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.sessions().into_iter().find(|s| s.id == id)
    }

    pub fn info(&self, s: &Session) -> SessionInfo {
        let ids: Vec<SessionId> = self.sessions().iter().map(|s| s.id).collect();
        let mut info = s.info(short_handle(&s.id, &ids));
        info.held = self.is_held(s.id);
        info
    }

    pub fn set_focused(&self, id: SessionId) {
        *self.focused.lock().expect("focused") = Some(id);
    }

    pub fn frame_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            self.config.read().expect("config").daemon.frame_interval_ms,
        )
    }

    /// One session (SPEC §2.3): `@last`, `@focused`, a full id, `s:<handle>`, an exact name,
    /// `project/name`, or a handle (id suffix). Exact names win over handles. Selectors that can
    /// match several sessions are an error here unless they match exactly one; use
    /// [`resolve_many`].
    pub fn resolve(&self, addr: &str) -> Result<Arc<Session>, Error> {
        let sessions = self.sessions();
        let addr = addr.trim();
        match addr {
            "@last" => return sessions.last().cloned().ok_or_else(|| Error::not_found(addr)),
            "@focused" => {
                let id = *self.focused.lock().expect("focused");
                return id.and_then(|id| self.session(id)).ok_or_else(|| Error::not_found(addr));
            }
            _ => {}
        }
        if addr.starts_with('@') || addr.contains(['*', '?', ',']) {
            return match self.resolve_many(addr)?.as_slice() {
                [one] => Ok(Arc::clone(one)),
                [] => Err(Error::not_found(addr)),
                many => Err(Error::new(
                    ErrorCode::Ambiguous,
                    format!("{addr:?} matches {} sessions", many.len()),
                )),
            };
        }
        if let Some(s) = sessions.iter().find(|s| s.meta.lock().expect("meta").name == addr) {
            return Ok(Arc::clone(s));
        }
        if let Some((project, name)) = addr.split_once('/') {
            let project = self.find_project(project)?;
            return sessions
                .iter()
                .find(|s| {
                    let m = s.meta.lock().expect("meta");
                    m.name == name && m.place.project.as_deref() == Some(project.id.as_str())
                })
                .cloned()
                .ok_or_else(|| Error::not_found(addr));
        }
        let handle = addr.strip_prefix("s:").unwrap_or(addr).to_ascii_lowercase();
        if handle.is_empty() {
            return Err(Error::invalid("empty session address"));
        }
        let matches: Vec<_> = sessions.iter().filter(|s| s.id.full().ends_with(&handle)).collect();
        match matches.as_slice() {
            [one] => Ok(Arc::clone(one)),
            [] => Err(Error::not_found(addr)),
            many => Err(Error::new(
                ErrorCode::Ambiguous,
                format!("{addr:?} matches {} sessions; use a longer handle", many.len()),
            )),
        }
    }

    /// Selectors (SPEC §2.3), joined with `,`: anything [`resolve`] takes, plus `@all`, `@live`
    /// (`@running`), `@exited`, `@working`/`@blocked`/`@idle`/`@done`/`@error` (program status), and
    /// name globs with `*` and `?`. Order follows the session list; duplicates are dropped.
    pub fn resolve_many(&self, selector: &str) -> Result<Vec<Arc<Session>>, Error> {
        let sessions = self.sessions();
        let mut picked: Vec<SessionId> = Vec::new();
        for part in selector.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let status_is = |s: &Session, want: &str| {
                s.meta
                    .lock()
                    .expect("meta")
                    .program_status
                    .as_ref()
                    .is_some_and(|p| p.state == want)
            };
            let found: Vec<SessionId> = match part {
                "@all" => sessions.iter().map(|s| s.id).collect(),
                "@live" | "@running" => {
                    sessions.iter().filter(|s| s.is_live()).map(|s| s.id).collect()
                }
                "@exited" => sessions.iter().filter(|s| !s.is_live()).map(|s| s.id).collect(),
                "@last" | "@focused" => vec![self.resolve(part)?.id],
                p if p.starts_with('@') => {
                    let want = &p[1..];
                    if !["working", "blocked", "idle", "done", "error"].contains(&want) {
                        return Err(Error::invalid(format!(
                            "unknown selector {p:?}; try @all, @live, @exited, @focused, @last, @working, @blocked, @idle, @done"
                        )));
                    }
                    sessions.iter().filter(|s| status_is(s, want)).map(|s| s.id).collect()
                }
                p if p.contains(['*', '?']) => sessions
                    .iter()
                    .filter(|s| {
                        let name = s.meta.lock().expect("meta").name.clone();
                        glob(p, &name)
                    })
                    .map(|s| s.id)
                    .collect(),
                p => vec![self.resolve(p)?.id],
            };
            for id in found {
                if !picked.contains(&id) {
                    picked.push(id);
                }
            }
        }
        Ok(sessions.into_iter().filter(|s| picked.contains(&s.id)).collect())
    }

    pub(crate) fn name_taken(&self, name: &str) -> bool {
        self.launching.lock().expect("launching").1.contains(name)
            || self.sessions().iter().any(|s| s.meta.lock().expect("meta").name == name)
    }

    /// `name`, or `name-2`, `name-3`… whichever is free.
    pub(crate) fn free_name(&self, name: &str) -> String {
        if !self.name_taken(name) {
            return name.to_owned();
        }
        (2..).map(|k| format!("{name}-{k}")).find(|n| !self.name_taken(n)).expect("infinite")
    }

    /// Sessions running (or exited) in a worktree.
    pub fn sessions_in(&self, worktree: &str) -> Vec<Arc<Session>> {
        self.sessions()
            .into_iter()
            .filter(|s| s.meta.lock().expect("meta").place.worktree.as_deref() == Some(worktree))
            .collect()
    }

    pub(crate) fn push_session(&self, s: Arc<Session>) {
        self.sessions.write().expect("sessions").push(s);
    }

    pub(crate) fn forget_session(&self, id: SessionId) {
        self.sessions.write().expect("sessions").retain(|o| o.id != id);
        self.last_notified.lock().expect("last_notified").remove(&id);
        let _ = self.events.send(DomainEvent::SessionRemoved { id });
    }

    /// Start a session (SPEC §7.2): an agent, a command or a shell, in a worktree or any
    /// directory, under the profile SPEC §10.10 picks.
    pub async fn spawn(self: &Arc<Self>, spec: SpawnSpec) -> Result<Arc<Session>, Error> {
        let config = self.config();
        let (verify, lead) = (spec.verify.clone(), spec.lead.clone());
        if lead.is_some() && spec.agent.is_none() {
            return Err(Error::invalid("--lead needs an agent (--agent claude)"));
        }
        let policy: Option<ptyro_run::Policy> = lead
            .map(|p| serde_json::from_value(p).map_err(|e| Error::invalid(format!("policy: {e}"))))
            .transpose()?;
        let (agent, agent_profile) = match spec.agent.as_deref() {
            Some(a) => {
                let (a, p) = ptyro_agents::split_agent(a);
                (Some(a.to_owned()), p.map(str::to_owned))
            }
            None => (None, None),
        };
        if let (Some(a), Some(b)) = (&spec.profile, &agent_profile)
            && a != b
        {
            return Err(Error::invalid(format!("--profile {a} contradicts the agent's @{b}")));
        }
        let explicit = spec.profile.clone().or(agent_profile);
        let target = self.target(spec.worktree.as_deref(), &spec.cwd)?;
        let cwd = target.as_ref().map_or_else(|| spec.cwd.clone(), |(w, _)| w.path.clone());
        if !cwd.is_dir() {
            return Err(Error::invalid(format!("not a directory: {}", cwd.display())));
        }
        let workspace =
            self.home_workspace(spec.workspace.as_deref(), target.as_ref().map(|(_, p)| p))?;
        let res = self.resolve_profile(
            explicit.as_deref(),
            target.as_ref(),
            &workspace,
            spec.override_pin,
            spec.confirmed,
        )?;
        if let Some(q) = res.question {
            return Err(Error::new(ErrorCode::NeedsConfirmation, q));
        }
        if let Some(w) = &res.warning {
            tracing::warn!("{w}");
        }
        let (argv, kind) = match (&agent, spec.argv.is_empty()) {
            (Some(a), _) => {
                if ptyro_agents::command(&config, a).is_none() {
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        format!(
                            "no agent named {a:?} (known: {})",
                            ptyro_agents::names(&config).join(", ")
                        ),
                    ));
                }
                // Extra arguments; the adapter builds the rest at launch.
                (spec.argv.clone(), SessionKind::Agent)
            }
            (None, true) => (vec![user_shell(), "-l".into()], SessionKind::Shell),
            (None, false) => (spec.argv.clone(), SessionKind::Command),
        };
        let id = SessionId::new();
        let name = match spec.name {
            Some(n) => {
                names::validate(&n).map_err(Error::invalid)?;
                if self.name_taken(&n) {
                    return Err(Error::conflict(format!("a session named {n:?} exists")));
                }
                n
            }
            // Agents are named after themselves (`claude`, `claude-2`): under a worktree in the
            // sidebar that reads better than a random name.
            None => match &agent {
                Some(a) if names::validate(a).is_ok() => self.free_name(a),
                _ => names::friendly(id.seed(), |n| self.name_taken(n)),
            },
        };
        let place = Place {
            profile: res.profile,
            worktree: target.as_ref().map(|(w, _)| w.id.clone()),
            project: target.as_ref().map(|(_, p)| p.id.clone()),
            workspace: Some(workspace),
            agent: agent.clone(),
            script: None,
        };
        // `@review` names a prompt snippet from the project's `[prompts]` (SPEC §13.3).
        let prompt = match spec.prompt.as_deref().and_then(|p| p.strip_prefix('@')) {
            Some(snippet) => {
                let cfg = target
                    .as_ref()
                    .and_then(|(w, p)| {
                        ptyro_core::project::ProjectConfig::load(&p.root, &w.path).ok()
                    })
                    .unwrap_or_default();
                Some(cfg.prompts.get(snippet).cloned().ok_or_else(|| {
                    Error::missing(
                        "prompt snippet",
                        format!("@{snippet} (in .ptyro.toml [prompts])"),
                    )
                })?)
            }
            None => spec.prompt.clone(),
        };
        if spec.structured && agent.is_none() {
            return Err(Error::invalid(
                "--structured drives an agent over its protocol: give it --agent",
            ));
        }
        let start = agent.map(|name| crate::launch::AgentStart {
            name,
            resume: None,
            prompt,
            command: None,
            mcp: spec.lead.is_some() || spec.mcp,
            structured: spec.structured,
        });
        let s = self
            .launch(Launch {
                id,
                argv,
                kind,
                name,
                cwd,
                size: spec.size.unwrap_or(TermSize::DEFAULT),
                client_env: spec.env,
                place,
                replace: false,
                agent: start,
            })
            .await?;
        match verify {
            Some(check) => self.set_verify(&s, Some(check), None)?,
            None => self.auto_verify(&s),
        }
        if let Some(p) = policy {
            self.lead_start(&s, Some(p));
        }
        Ok(s)
    }

    /// Run an exited session's command again in place: same id, name, cwd, size and profile, new
    /// process and a fresh screen (SPEC §7.3). Bound to its profile like resume (SPEC §10.10).
    pub async fn restart(self: &Arc<Self>, addr: &str) -> Result<Arc<Session>, Error> {
        let old = self.resolve(addr)?;
        if old.is_live() {
            return Err(Error::conflict("session is still running; kill it first"));
        }
        let meta = old.meta.lock().expect("meta").clone();
        if !ptyro_agents::profile::exists(&self.config(), &meta.place.profile) {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "profile {:?} no longer exists, and this session's account lives in it; restore the profile or start a new session",
                    meta.place.profile
                ),
            ));
        }
        // An agent restarts fresh through its adapter (resume continues the conversation).
        let agent = (meta.kind == SessionKind::Agent).then(|| crate::launch::AgentStart {
            name: meta.place.agent.clone().unwrap_or_else(|| "generic".into()),
            resume: None,
            prompt: None,
            command: None,
            mcp: false,
            structured: meta.structured.is_some(),
        });
        let new = self
            .launch(Launch {
                id: old.id,
                argv: if agent.is_some() { Vec::new() } else { meta.argv },
                kind: meta.kind,
                name: meta.name,
                cwd: meta.cwd,
                size: old.size(),
                client_env: meta.client_env,
                place: meta.place,
                replace: true,
                agent,
            })
            .await?;
        if let Some(store) = &self.store {
            store.event(old.id.full(), "restart", serde_json::json!({}));
        }
        tracing::info!(session = %old.id, "restarted");
        let _ = self.events.send(DomainEvent::SessionRestarted { info: self.info(&new) });
        Ok(new)
    }

    /// Persist a snapshot under `key` in the background. Only the newest snapshot of a key is
    /// written: an older one that lost a race to the store is dropped, never written last.
    pub(crate) fn save_kv(&self, key: &str, value: serde_json::Value) {
        let Some(store) = self.store.clone() else { return };
        let generation = {
            let mut g = self.kv_gens.lock().expect("kv_gens");
            let e = g.entry(key.to_owned()).or_default();
            *e += 1;
            *e
        };
        let (gens, writer, key) =
            (Arc::clone(&self.kv_gens), Arc::clone(&self.kv_writer), key.to_owned());
        tokio::spawn(async move {
            let _one = writer.lock().await;
            if gens.lock().expect("kv_gens").get(&key) != Some(&generation) {
                return;
            }
            if let Err(e) = store.kv_set(key.clone(), value).await {
                tracing::warn!("saving {key}: {e}");
            }
        });
    }

    /// Put a relaunched session in its slot; `false` when the slot is gone (it was removed).
    pub(crate) fn replace_session(&self, new: &Arc<Session>) -> bool {
        let mut sessions = self.sessions.write().expect("sessions");
        match sessions.iter_mut().find(|s| s.id == new.id) {
            Some(slot) => {
                *slot = Arc::clone(new);
                true
            }
            None => false,
        }
    }

    pub fn status(&self) -> DaemonStatus {
        let sessions = self.sessions();
        DaemonStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            proto: PROTO,
            pid: std::process::id(),
            started_ms: self.started_ms,
            sessions: sessions.len() as u32,
            live_sessions: sessions.iter().filter(|s| s.is_live()).count() as u32,
            clients: self.clients.load(Ordering::Relaxed),
            socket: self.paths.socket.clone(),
            log_dir: self.paths.log_dir(),
            generation: self.generation.load(Ordering::Relaxed),
            upgraded_ms: Some(self.upgraded_ms.load(Ordering::Relaxed)).filter(|t| *t > 0),
            rolled_back: self.rolled_back.load(Ordering::Relaxed),
            metrics: Some(self.metrics()),
        }
    }

    pub async fn handle(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        self.handle_as(req, crate::mail::Caller::default()).await
    }

    /// A request from a client: who sent it decides what an agent may do (M7.6).
    pub async fn handle_as(
        self: &Arc<Self>,
        req: Request,
        caller: crate::mail::Caller,
    ) -> Result<Reply, Error> {
        self.guard_caller(&req, caller)?;
        self.guard_device(&req, caller)?;
        match req {
            req @ (Request::DevicePair { .. }
            | Request::DeviceList
            | Request::DeviceRevoke { .. }
            | Request::PushKey
            | Request::PushSubscribe { .. }
            | Request::PushUnsubscribe { .. }
            | Request::PushTest) => self.handle_devices(req, caller).await,
            req @ (Request::RunList { .. }
            | Request::RunGet { .. }
            | Request::RunEvents { .. }
            | Request::RunCancel { .. }
            | Request::RunRetry { .. }
            | Request::RunFork { .. }
            | Request::RecipeList { .. }
            | Request::RecipeShow { .. }
            | Request::RecipeRun { .. }
            | Request::Fanout { .. }
            | Request::GateList { .. }
            | Request::GateResolve { .. }) => self.handle_runs(req).await,
            req @ (Request::Compare { .. }
            | Request::Pick { .. }
            | Request::Discard { .. }
            | Request::Iterate { .. }) => self.handle_compare(req).await,
            req @ (Request::MsgPost { .. }
            | Request::MsgCheck { .. }
            | Request::MsgAck { .. }
            | Request::MsgList { .. }
            | Request::LeaseClaim { .. }
            | Request::LeaseRelease { .. }
            | Request::LeaseList
            | Request::Radar { .. }
            | Request::LeadStart { .. }
            | Request::Lead { .. }
            | Request::SetVerify { .. }) => self.handle_mail(req, caller).await,
            Request::Cs { op } => self.handle_cs(op, caller).await,
            req @ (Request::CheckpointList { .. }
            | Request::CheckpointDiff { .. }
            | Request::CheckpointRestore { .. }) => self.checkpoint_request(req).await,
            Request::Spawn { spec } => {
                // `review.max_unreviewed`: a new agent waits while too much waits for review.
                if spec.agent.is_some()
                    && !spec.unqueued
                    && let Some(unreviewed) = self.review_gate()
                {
                    let position = self.queue_spawn(spec);
                    return Ok(Reply::Queued { position, unreviewed });
                }
                let s = self.spawn(spec).await?;
                Ok(Reply::Session { info: self.info(&s) })
            }
            req @ (Request::WorktreeDiff { .. }
            | Request::WorktreeLog { .. }
            | Request::WorktreeCommit { .. }
            | Request::WorktreeIntegrate { .. }
            | Request::WorktreePair { .. }
            | Request::PrGet { .. }
            | Request::PrCreate { .. }
            | Request::PrFix { .. }
            | Request::PrAutomerge { .. }
            | Request::PrWatch { .. }
            | Request::PrReply { .. }
            | Request::ReviewAdd { .. }
            | Request::ReviewList { .. }
            | Request::ReviewRemove { .. }
            | Request::ReviewSend { .. }
            | Request::ReviewQueue
            | Request::ReviewDone { .. }) => self.handle_review(req).await,
            Request::List => {
                let sessions = self.sessions().iter().map(|s| self.info(s)).collect();
                Ok(Reply::Sessions { sessions })
            }
            Request::Get { addr } => {
                let s = self.resolve(&addr)?;
                Ok(Reply::Session { info: self.info(&s) })
            }
            Request::Resolve { addr } => {
                let sessions = self.resolve_many(&addr)?.iter().map(|s| self.info(s)).collect();
                Ok(Reply::Sessions { sessions })
            }
            Request::Kill { addr, signal } => {
                let s = self.resolve(&addr)?;
                match signal {
                    None => self.stop_session(&s).await,
                    Some(_) => s.kill(signal).await.map_err(|e| Error::invalid(e.to_string()))?,
                }
                Ok(Reply::Session { info: self.info(&s) })
            }
            Request::Remove { addr } => {
                let s = self.resolve(&addr)?;
                if self.launching.lock().expect("launching").0.contains(&s.id) {
                    return Err(Error::conflict("the session is restarting; remove it after"));
                }
                if s.is_live() {
                    return Err(Error::conflict("session is still running; kill it first"));
                }
                // A removed dormant session must not come back at the next start.
                {
                    let mut m = s.meta.lock().expect("meta");
                    if m.state == ptyro_core::session::ProcessState::Dormant {
                        m.state =
                            ptyro_core::session::ProcessState::Exited { code: None, signal: None };
                    }
                }
                s.save(None);
                self.forget_session(s.id);
                Ok(Reply::Done)
            }
            Request::Restart { addr } => {
                let s = self.restart(&addr).await?;
                Ok(Reply::Session { info: self.info(&s) })
            }
            Request::Rename { addr, name } => {
                let s = self.resolve(&addr)?;
                names::validate(&name).map_err(Error::invalid)?;
                if self.name_taken(&name) {
                    return Err(Error::conflict(format!("a session named {name:?} exists")));
                }
                s.rename(name);
                let info = self.info(&s);
                let _ = self.events.send(DomainEvent::SessionUpdated { info: info.clone() });
                Ok(Reply::Session { info })
            }
            Request::SendBytes { addr, bytes } => {
                let s = self.resolve(&addr)?;
                if !s.is_live() {
                    return Err(Error::conflict("session has exited"));
                }
                if bytes.len() > 1 << 20 {
                    return Err(Error::invalid("at most 1 MiB at a time"));
                }
                s.write_raw(&bytes);
                Ok(Reply::Done)
            }
            Request::SendText { addr, text, enter, wait_ready_ms } => {
                let s = self.resolve(&addr)?;
                if !s.is_live() {
                    return Err(Error::conflict("session has exited"));
                }
                match wait_ready_ms {
                    Some(t) => self.send_when_ready(&s, &text, enter, t).await?,
                    None => {
                        s.send_text(&text, enter).await;
                        if enter {
                            s.meta.lock().expect("meta").last_prompt_ms = Some(now_ms());
                        }
                    }
                }
                Ok(Reply::Done)
            }
            Request::ReadScreen { addr, format } => {
                Ok(Reply::Screen { text: self.resolve(&addr)?.read_screen(format) })
            }
            Request::ReadScrollback { addr, lines } => {
                let s = self.resolve(&addr)?;
                // An explicit count may reach past memory into the recording (M8.2).
                let doc = match lines {
                    Some(n) => tokio::task::spawn_blocking(move || s.scrollback_deep(n))
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?,
                    None => s.scrollback(u32::MAX),
                };
                Ok(Reply::Scrollback { doc })
            }
            Request::SaveView { name, view } => {
                match &self.store {
                    Some(store) => store
                        .kv_set(format!("view:{name}"), view)
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?,
                    None => {
                        self.views.lock().expect("views").insert(name, view);
                    }
                }
                Ok(Reply::Done)
            }
            Request::LoadView { name } => {
                let view = match &self.store {
                    Some(store) => store
                        .kv_get(format!("view:{name}"))
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?,
                    None => self.views.lock().expect("views").get(&name).cloned(),
                };
                Ok(Reply::View { view })
            }
            Request::History { limit } => {
                let Some(store) = &self.store else {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        "the store is unavailable (see daemon logs)",
                    ));
                };
                let records = store
                    .history(limit.unwrap_or(100) as usize)
                    .await
                    .map_err(|e| Error::internal(e.to_string()))?;
                let current: Vec<String> = self.sessions().iter().map(|s| s.id.full()).collect();
                let entries = records
                    .into_iter()
                    .map(|r| HistoryEntry {
                        current: current.contains(&r.id),
                        id: r.id,
                        name: r.name,
                        kind: r.kind,
                        argv: r.argv,
                        cwd: PathBuf::from(r.cwd),
                        state: r.state,
                        created_ms: r.created_ms,
                        ended_ms: r.ended_ms,
                    })
                    .collect();
                Ok(Reply::History { entries })
            }
            Request::Status => Ok(Reply::Status { status: self.status() }),
            Request::Upgrade { binary } => self.upgrade(binary).await,
            Request::Events { session, limit } => {
                let Some(store) = &self.store else {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        "the store is unavailable (see daemon logs)",
                    ));
                };
                let id = match session {
                    Some(a) => Some(self.resolve(&a)?.id.full()),
                    None => None,
                };
                let events = store
                    .events(id, limit.unwrap_or(100) as usize)
                    .await
                    .map_err(|e| Error::internal(e.to_string()))?
                    .into_iter()
                    .map(|(session, at_ms, kind, detail)| ptyro_protocol::EventRecord {
                        session,
                        at_ms,
                        kind,
                        detail,
                    })
                    .collect();
                Ok(Reply::Events { events })
            }
            Request::Doctor => Ok(Reply::Doctor { report: self.doctor() }),
            Request::SelfTest { adapter } => {
                Ok(Reply::SelfTest { result: self.self_test(&adapter).await? })
            }
            Request::ReloadConfig => match self.reload_config() {
                None => Ok(Reply::Done),
                Some(e) => Err(Error::invalid(e)),
            },
            Request::Shutdown { force } => {
                let live = self.sessions().iter().filter(|s| s.is_live()).count();
                if live > 0 && !force {
                    return Err(Error::conflict(format!(
                        "{live} session(s) still running; use --force to stop them"
                    )));
                }
                let _ = self.shutdown.send(true);
                Ok(Reply::Done)
            }
            Request::Hook { session, adapter, event, payload, env } => {
                self.hook(&session, &adapter, &event, payload, env).await
            }
            Request::StatusLine { session, payload } => self.statusline(&session, &payload),
            Request::Report { addr, state, kind, msg, progress } => {
                self.report(&addr, &state, kind, msg, progress).await
            }
            Request::StatusExplain { addr } => {
                let s = self.resolve(&addr)?;
                Ok(Reply::Explain { lines: self.explain(&s) })
            }
            Request::Resume { addr } => self.resume(&addr).await,
            Request::Convert { addr, structured, force } => {
                self.convert(&addr, structured, force).await
            }
            Request::ChatGet { addr } => {
                let s = self.resolve(&addr)?;
                let st = s.structured.as_ref().ok_or_else(|| {
                    Error::invalid("not a structured session (`ptyro new --structured`)")
                })?;
                Ok(Reply::Transcript { items: st.chat.lock().expect("chat").items.clone() })
            }
            Request::Hibernate { addr } => self.hibernate(&addr).await,
            Request::ClearAttention { addr } => {
                let s = self.resolve(&addr)?;
                self.clear_attention(&s);
                Ok(Reply::Done)
            }
            Request::Answer { addr, decision, expect } => {
                let d = ptyro_agents::Decision::parse(&decision)
                    .ok_or_else(|| Error::invalid("decision must be allow, always or deny"))?;
                self.answer(&addr, d, expect).await
            }
            Request::Inbox { workspace, unread_only, limit } => {
                let limit = limit.unwrap_or(100) as usize;
                let entries = match &self.store {
                    Some(st) => st
                        .inbox(workspace, unread_only, limit)
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?,
                    None => {
                        let mem = self.inbox_mem.lock().expect("inbox");
                        mem.iter()
                            .rev()
                            .filter(|e| {
                                workspace.as_ref().is_none_or(|w| e.workspace.as_ref() == Some(w))
                            })
                            .filter(|e| !unread_only || e.read_ms.is_none())
                            .take(limit)
                            .cloned()
                            .collect()
                    }
                };
                Ok(Reply::Inbox { entries })
            }
            Request::InboxMark { ids, all, session, read } => {
                let session = session.map(|a| self.resolve(&a).map(|s| s.id.full())).transpose()?;
                match &self.store {
                    Some(st) => {
                        st.inbox_mark(ids, session, all, read)
                            .await
                            .map_err(|e| Error::internal(e.to_string()))?;
                    }
                    None => {
                        let at = read.then(now_ms);
                        for e in self.inbox_mem.lock().expect("inbox").iter_mut() {
                            if ids.contains(&e.id)
                                || (all && session.as_ref().is_none_or(|s| *s == e.session))
                            {
                                e.read_ms = at;
                            }
                        }
                    }
                }
                Ok(Reply::Done)
            }
            Request::Mute { target, minutes, unmute } => self.mute(&target, minutes, unmute).await,
            Request::Usage => Ok(self.usage_list()),
            Request::NotifyTest => self.notify_test().await,
            other => self.handle_registry(other).await,
        }
    }

    /// Re-read the config file; on error keep the old one. Tells every client either way.
    pub fn reload_config(&self) -> Option<String> {
        let path = Config::path(&self.paths);
        let error = match Config::load(&path) {
            Ok(c) => {
                *self.config.write().expect("config") = c;
                self.secrets.lock().expect("secrets").clear();
                tracing::info!("config reloaded");
                None
            }
            Err(e) => {
                tracing::warn!("config not reloaded: {e}");
                Some(e.to_string())
            }
        };
        let _ = self.events.send(DomainEvent::ConfigReloaded { error: error.clone() });
        error
    }

    /// Stop every live session gracefully, in parallel. Agents that can resume hibernate, so
    /// the next daemon brings them back dormant (SPEC §4.4).
    pub async fn stop_all(self: &Arc<Self>) {
        let live: Vec<_> = self.sessions().into_iter().filter(|s| s.is_live()).collect();
        for s in &live {
            let resumable = s.kind() == SessionKind::Agent
                && s.meta.lock().expect("meta").conversation.is_some()
                && s.adapter().is_some_and(|a| ptyro_agents::adapter(&a).caps().resume);
            if resumable {
                s.hibernating.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        futures::future::join_all(live.iter().map(|s| self.stop_session(s))).await;
    }

    /// The workspace a new session belongs to: the one asked for, else the first workspace
    /// listing its project, else `main`.
    pub(crate) fn home_workspace(
        &self,
        asked: Option<&str>,
        project: Option<&ptyro_core::model::ProjectInfo>,
    ) -> Result<String, Error> {
        let reg = self.registry.lock().expect("registry");
        if let Some(name) = asked {
            return reg
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .map(|w| w.name.clone())
                .ok_or_else(|| Error::missing("workspace", name));
        }
        if let Some(p) = project
            && let Some(w) = reg.workspaces.iter().find(|w| w.projects.contains(&p.id))
        {
            return Ok(w.name.clone());
        }
        Ok(MAIN_WORKSPACE.into())
    }
}

pub(crate) fn user_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

/// `*` matches any run of characters, `?` exactly one. Linear in practice (it backtracks only
/// to the last `*`), so a hostile pattern can't spin.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += 1;
            }
            Some(c) if *c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn globs() {
        assert!(glob("build-*", "build-web"));
        assert!(glob("*rook", "brisk-rook"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "abbc"));
        assert!(glob("*", ""));
        assert!(!glob("x*", "yx"));
        assert!(glob("a*b*c", "aXbYc") && !glob("a*b*c", "aXbY"));
        // The pathological pattern finishes at once.
        let name = "a".repeat(64);
        assert!(!glob("*a*a*a*a*a*a*a*a*a*a*a*a*b", &name));
    }
}
