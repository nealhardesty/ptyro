//! Chats (SPEC §22): plain conversations with an agent, kept in `chat.db`.
//!
//! A chat is ptyro's own record (a tree of nodes). Its *engine* is a hidden structured session
//! (`Place::chat`) running in the chat's scratch directory under its workspace's profile; it
//! serves the record and can be replaced. Sending a message:
//! 1. writes the user's node (its parent is the shown branch's tip, the chat's `leaf`);
//! 2. starts an engine when none runs: the chat's current conv continued natively when the agent
//!    can (`--resume`, `thread/resume`, `session/load`), else a new conv whose first prompt
//!    carries the branch as a seed block (§22.6);
//! 3. follows the engine's transcript: each item becomes a node (a streaming answer is written
//!    every 2 s and when it completes), and every follower of the chat sees it.
//!
//! An idle engine hibernates after `[chat] hibernate_after`; the next message resumes it.

use ptyro_core::sync::MutexExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, NoticeLevel, Transcript};
use ptyro_core::chats::{self, ChatHit, ChatInfo, ChatNode, ChatSiblings};
use ptyro_core::config::{ChatTools, parse_duration};
use ptyro_core::model::MAIN_WORKSPACE;
use ptyro_core::session::SessionKind;
use ptyro_core::status::{AgentStatus, AttentionKind};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize, now_ms};
use ptyro_protocol::{DomainEvent, Reply, Request};
use ptyro_store::chat::{ChatDb, Conv, ListFilter, NewChat};
use tokio::sync::watch;

use crate::daemon::Daemon;
use crate::launch::{AgentStart, Launch};
use crate::session::{Place, Session};

mod compare;
pub(crate) mod import;

/// A fork's conv until its first answer has its own conversation (then it is `native_fork`, by
/// `update_conv`): Claude resumes the source's
/// conversation with `--fork-session`, leaving it as it was.
const FORK_PENDING: &str = "fork_pending";

/// A streaming answer is written at least this often (§22.4: a crash keeps what had arrived).
const FLUSH: Duration = Duration::from_secs(2);
/// The system prompt of a Claude chat: a general assistant, not a coding agent (CHAT0).
const SYSTEM_PROMPT: &str = "You are a helpful, knowledgeable assistant in a chat. Answer \
    clearly and directly, in Markdown where it helps. When a question needs current or \
    specific facts, search the web and say where the answer comes from.";
const TITLE_PROMPT: &str = "Write a title for this conversation: at most six words, plain \
    text, no quotes, no full stop. Reply with the title only.";

/// The chats part of the daemon: `chat.db` and the chats that are live (followed or running).
pub struct Chats {
    db: Result<ChatDb, String>,
    live: Mutex<HashMap<String, Arc<Live>>>,
    /// One message at a time per chat (a second waits for the first to be written).
    sending: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Incognito chats (SPEC §22.9): kept here only, never in `chat.db`; gone with their agent.
    incognito: Mutex<HashMap<String, ChatInfo>>,
    /// Meaning search (SPEC §22.10 tier 3).
    pub(crate) meaning: crate::meaning::Meaning,
    /// Compares under way, by chat (SPEC §22.7).
    comparing: Mutex<HashMap<String, Arc<compare::Compare>>>,
    /// A compare's helpers (in-memory chats nobody lists, finds or follows): helper → its chat.
    helpers: Mutex<HashMap<String, String>>,
    /// One import of the agents' histories at a time (SPEC §22.10), and the files it passed
    /// over (path → size and mtime): not read again until they change.
    importing: tokio::sync::Mutex<HashMap<String, (u64, u64)>>,
}

/// A chat someone follows or an engine serves.
pub(crate) struct Live {
    pub id: String,
    /// The shown branch, as the chat view's items (their ids are node ids).
    pub items: Mutex<Transcript>,
    /// Bumped on every change; followers wait on it.
    pub rev: watch::Sender<u64>,
    /// Bumped when the shown branch is reloaded: followers start over.
    pub epoch: watch::Sender<u64>,
    pub viewers: AtomicU32,
    /// The tip of the shown branch (the next node's parent).
    leaf: Mutex<Option<String>>,
    engine: Mutex<Option<Engine>>,
    /// A resumed engine that hasn't answered yet: what it says (a failed resume's error) is
    /// neither written nor alerted; the message is seeded to a new one instead.
    quiet: std::sync::atomic::AtomicBool,
    /// Where the shown branch has others, sent with every reset (SPEC §22.7).
    pub siblings: Mutex<Vec<ChatSiblings>>,
    /// The last node its engine was given (for a chat with no conv row: an incognito one).
    given: Mutex<Option<String>>,
}

#[derive(Clone)]
struct Engine {
    session: Arc<Session>,
}

impl Live {
    fn bump(&self) {
        let rev = self.items.locked().rev;
        self.rev.send_modify(|r| *r = rev.max(*r + 1));
    }

    fn engine(&self) -> Option<Engine> {
        self.engine.locked().clone().filter(|e| e.session.is_live())
    }
}

impl Chats {
    pub fn open(path: &std::path::Path) -> Self {
        let db = ChatDb::open(path).map_err(|e| {
            tracing::error!("chat.db: {e}");
            format!(
                "chats are unavailable: {} can't be opened ({e}). `ptyro doctor` says more",
                path.display()
            )
        });
        Self {
            db,
            live: Mutex::new(HashMap::new()),
            sending: Mutex::new(HashMap::new()),
            incognito: Mutex::new(HashMap::new()),
            meaning: crate::meaning::Meaning::default(),
            comparing: Mutex::new(HashMap::new()),
            helpers: Mutex::new(HashMap::new()),
            importing: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn db(&self) -> Result<&ChatDb, Error> {
        self.db.as_ref().map_err(|e| Error::new(ErrorCode::Internal, e.clone()))
    }

    fn live(&self, id: &str) -> Arc<Live> {
        Arc::clone(self.live.locked().entry(id.to_owned()).or_insert_with(|| {
            Arc::new(Live {
                id: id.to_owned(),
                items: Mutex::new(Transcript::default()),
                rev: watch::Sender::new(0),
                epoch: watch::Sender::new(0),
                viewers: AtomicU32::new(0),
                leaf: Mutex::new(None),
                engine: Mutex::new(None),
                quiet: std::sync::atomic::AtomicBool::new(false),
                siblings: Mutex::new(Vec::new()),
                given: Mutex::new(None),
            })
        }))
    }

    pub(crate) fn get_live(&self, id: &str) -> Option<Arc<Live>> {
        self.live.locked().get(id).cloned()
    }

    /// Forget a live chat nobody follows and no engine serves.
    /// Drop a live chat nobody follows. An incognito one's is its only record: it stays until
    /// the chat ends (`end_incognito`).
    fn settle(&self, id: &str) {
        if self.incognito.locked().contains_key(id) {
            return;
        }
        let mut live = self.live.locked();
        if let Some(l) = live.get(id)
            && l.viewers.load(Ordering::SeqCst) == 0
            && l.engine().is_none()
        {
            live.remove(id);
        }
    }

    fn lock_for(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.sending.locked().entry(id.to_owned()).or_default())
    }
}

/// The scratch directory of a chat's engine: empty, no project files, no hooks (§22.2).
fn scratch(daemon: &Daemon, chat: &str) -> PathBuf {
    daemon.paths.data.join("chat").join(chat)
}

impl Daemon {
    fn chat_db(&self) -> Result<ChatDb, Error> {
        if !self.config().chat.enabled {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "chats are off: set `[chat] enabled = true` (ptyro config set chat.enabled true)",
            ));
        }
        self.chats.db().cloned()
    }

    /// At the daemon's start: answers cut off by a stop are closed as interrupted (§22.4), and
    /// old tombstones and archives go once a day.
    /// At start, before connections are accepted (so nothing a new request starts is touched):
    /// stop the engines that crossed a live upgrade, close the answers they cut off, then purge
    /// old tombstones daily in the background.
    pub(crate) async fn start_chats(self: &Arc<Self>) {
        let Ok(db) = self.chats.db().cloned() else {
            if let Err(e) = self.chats.db() {
                self.warn(e.message);
            }
            return;
        };
        // Engines that crossed a live upgrade have no follower in this image (the next message
        // resumes the conversation). Which they are is taken now, and their cut-off answers are
        // closed now (one write), before connections are accepted; stopping them waits on each
        // process, so it happens behind, and an upgrade's clients are back at once.
        let crossed: Vec<Arc<Session>> =
            self.sessions().into_iter().filter(|s| s.meta.locked().place.chat.is_some()).collect();
        match db.close_interrupted().await {
            Ok(c) if !c.is_empty() => tracing::info!("{} chat answer(s) were cut off", c.len()),
            Ok(_) => {}
            Err(e) => tracing::warn!("chat.db: {e}"),
        }
        // Engines' session rows from before they were kept out of `ptyro.db`.
        if let Some(store) = &self.store {
            store.forget_sessions_in(self.paths.data.join("chat").to_string_lossy().into_owned());
        }
        // An incognito chat lived in the old image's memory alone, so it ends here: its folders
        // go with it (SPEC §22.9). So do the folders of chats that are gone (a compare's helpers
        // and incognito chats a crash cut off): which they are is taken now, before a new
        // incognito chat can make one.
        let engines: Vec<String> = std::fs::read_dir(self.paths.data.join("chat"))
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let me = Arc::clone(self);
        let old = db.clone();
        tokio::spawn(async move {
            let crossing: Vec<String> =
                crossed.iter().filter_map(|s| s.meta.locked().place.chat.clone()).collect();
            for id in engines.iter().filter(|id| !crossing.contains(id)) {
                if matches!(old.get(id.clone()).await, Ok(None)) {
                    me.remove_chat_dirs(id, None).await;
                }
            }
            for s in crossed {
                let place = s.meta.locked().place.clone();
                let Some(chat) = place.chat.clone() else { continue };
                s.hibernating.store(true, Ordering::Relaxed);
                me.stop_session(&s).await;
                me.forget_session(s.id);
                if matches!(old.get(chat.clone()).await, Ok(None)) {
                    let who = place.agent.as_deref().map(|a| (a, place.profile.as_str()));
                    me.remove_chat_dirs(&chat, who).await;
                }
            }
        });
        self.start_meaning(db.clone());
        self.start_chat_import();
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let days = me.config().chat.archive_purge_days;
                match db.compact(days).await {
                    Ok((n, purged)) => {
                        if n > 0 {
                            tracing::info!("chat.db: {n} deleted chat(s) compacted");
                        }
                        // Archived past `archive_purge_days`: their files go as a delete's do.
                        for id in purged {
                            me.remove_chat_dirs(&id, None).await;
                            let _ = me.events.send(DomainEvent::ChatRemoved { id, machine: None });
                        }
                    }
                    Err(e) => tracing::warn!("chat.db compaction: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
            }
        });
    }

    // ── the record: chat.db, or memory for an incognito chat ─────────────────────────────

    /// The incognito chats people see (a compare's helpers are not among them).
    fn visible_incognito(&self) -> Vec<ChatInfo> {
        let helpers = self.chats.helpers.locked();
        self.chats
            .incognito
            .locked()
            .values()
            .filter(|c| !helpers.contains_key(&c.id))
            .cloned()
            .collect()
    }

    fn incognito_of(&self, id: &str) -> Option<ChatInfo> {
        self.chats.incognito.locked().get(id).cloned()
    }

    /// A chat's row (an incognito one's from memory).
    async fn rec_get(&self, id: &str) -> Result<Option<ChatInfo>, Error> {
        if let Some(c) = self.incognito_of(id) {
            return Ok(Some(c));
        }
        self.chat_db()?.get(id.to_owned()).await.map_err(store_err)
    }

    /// Every chat id (for handles).
    async fn rec_ids(&self) -> Vec<String> {
        let mut ids = match self.chat_db() {
            Ok(db) => db.ids().await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        ids.extend(self.chats.incognito.locked().keys().cloned());
        ids
    }

    /// The branch to `leaf` (the shown one when `None`); an incognito chat's is what it showed.
    async fn rec_branch(&self, id: &str, leaf: Option<String>) -> Result<Vec<ChatNode>, Error> {
        if self.incognito_of(id).is_some() {
            // One branch, in order: up to `leaf`, when given.
            let mut nodes = self.chats.get_live(id).map(|l| live_nodes(&l)).unwrap_or_default();
            if let Some(i) = leaf.and_then(|l| nodes.iter().position(|n| n.id == l)) {
                nodes.truncate(i + 1);
            }
            return Ok(nodes);
        }
        self.chat_db()?.branch(id.to_owned(), leaf).await.map_err(store_err)
    }

    async fn rec_put_now(&self, id: &str, n: ChatNode) -> Result<(), Error> {
        if self.incognito_of(id).is_some() {
            return Ok(());
        }
        self.chat_db()?.put_node_now(id.to_owned(), n, None, false).await.map_err(store_err)
    }

    /// Load the shown branch (and where it has others) into the live chat; followers start over.
    async fn reload_branch(&self, live: &Arc<Live>) {
        let id = live.id.clone();
        if self.incognito_of(&id).is_some() {
            live.epoch.send_modify(|e| *e += 1);
            live.bump();
            return;
        }
        let Ok(db) = self.chat_db() else { return };
        let branch = db.branch(id.clone(), None).await.unwrap_or_default();
        let ids = branch.iter().map(|n| n.id.clone()).collect();
        *live.siblings.locked() = db.siblings(id, ids).await.unwrap_or_default();
        load_branch(live, &branch);
    }

    /// Where the shown branch has others, again (a new answer beside an old one).
    async fn refresh_siblings(&self, live: &Arc<Live>) {
        if self.incognito_of(&live.id).is_some() {
            return;
        }
        let Ok(db) = self.chat_db() else { return };
        let ids: Vec<String> = live.items.locked().items.iter().map(|i| i.id.clone()).collect();
        let sib = db.siblings(live.id.clone(), ids).await.unwrap_or_default();
        let changed = *live.siblings.locked() != sib;
        if changed {
            *live.siblings.locked() = sib;
            live.epoch.send_modify(|e| *e += 1);
            live.bump();
        }
    }

    /// A node of a chat by its id or the end of it (≥ 6 characters), or `7f3k/01jb…`.
    async fn find_node(&self, chat: &str, addr: &str) -> Result<ChatNode, Error> {
        let addr = addr.rsplit('/').next().unwrap_or(addr);
        if self.incognito_of(chat).is_some() {
            let nodes = self.rec_branch(chat, None).await?;
            return nodes
                .into_iter()
                .find(|n| n.id == addr || (addr.len() >= 6 && n.id.ends_with(addr)))
                .ok_or_else(|| Error::missing("message", addr));
        }
        let db = self.chat_db()?;
        let ids = db.find_node(chat.to_owned(), addr.to_owned()).await.map_err(store_err)?;
        match ids.as_slice() {
            [one] => db
                .node(chat.to_owned(), one.clone())
                .await
                .map_err(store_err)?
                .ok_or_else(|| Error::missing("message", addr)),
            [] => Err(Error::missing("message", addr)),
            _ => Err(Error::invalid(format!(
                "{addr:?} is the end of several messages' ids: give more of it"
            ))),
        }
    }

    /// Ends the chat when it is incognito: its record goes with its agent (SPEC §22.9).
    async fn end_incognito(self: &Arc<Self>, id: &str) {
        let gone = self.chats.incognito.locked().remove(id);
        if let Some(info) = gone {
            self.chats.live.locked().remove(id);
            self.remove_chat_dirs(id, Some((&info.agent, &info.profile))).await;
            // A compare's helper was never listed: nobody is told it went.
            if self.chats.helpers.locked().remove(id).is_none() {
                let _ =
                    self.events.send(DomainEvent::ChatRemoved { id: id.to_owned(), machine: None });
            }
        }
    }

    // ── addressing ────────────────────────────────────────────────────────────────────────

    /// A chat by address: a handle (`7f3k`, `c:7f3k`), a full id, `@last`, or a title
    /// substring that matches one chat.
    pub(crate) async fn find_chat(&self, addr: &str) -> Result<ChatInfo, Error> {
        let db = self.chat_db()?;
        let all = db
            .list(ListFilter { sides: true, ..ListFilter::default() })
            .await
            .map_err(store_err)?;
        let mut archived = db
            .list(ListFilter { archived: true, sides: true, ..ListFilter::default() })
            .await
            .map_err(store_err)?;
        let mut all = all;
        all.append(&mut archived);
        all.extend(self.visible_incognito());
        // `7f3k/01jb…` names a message of it: the chat is the part before the slash.
        let addr = addr.split('/').next().unwrap_or(addr);
        let a = addr.trim();
        let a = a.strip_prefix("c:").unwrap_or(a);
        if a.is_empty() {
            return Err(Error::invalid("which chat? (its handle, like 7f3k, or @last)"));
        }
        let found: Vec<&ChatInfo> = if a == "@last" {
            all.iter().max_by_key(|c| c.last_ms).into_iter().collect()
        } else {
            let lower = a.to_lowercase();
            let by_id: Vec<&ChatInfo> = all.iter().filter(|c| c.id.ends_with(&lower)).collect();
            if !by_id.is_empty() && lower.len() >= 4 {
                by_id
            } else {
                all.iter()
                    .filter(|c| {
                        c.title.as_deref().is_some_and(|t| t.to_lowercase().contains(&lower))
                    })
                    .collect()
            }
        };
        match found.as_slice() {
            [one] => Ok(self.chat_info((*one).clone(), &all)),
            [] => Err(Error::missing("chat", addr)),
            many => Err(Error::invalid(format!(
                "{addr:?} matches {} chats: {}",
                many.len(),
                many.iter()
                    .take(5)
                    .map(|c| format!(
                        "c:{} ({})",
                        chats::handle(&c.id, all.iter().map(|x| x.id.as_str())),
                        c.label()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// A row as clients see it: its handle, its workspace's name, its engine's status.
    fn chat_info(&self, mut c: ChatInfo, all: &[ChatInfo]) -> ChatInfo {
        c.handle = chats::handle(&c.id, all.iter().map(|x| x.id.as_str()));
        c.workspace = self.workspace_name(&c.workspace);
        let config = self.config();
        let preset = config.chat.preset(&c.tools).unwrap_or_default();
        c.shell = preset.shell;
        c.best_effort = best_effort(&ptyro_agents::adapter_name(&config, &c.agent), &preset);
        c.comparing = self.chats.comparing.locked().get(&c.id).map(|x| x.state.locked().clone());
        if let Some(e) = self.chats.get_live(&c.id).and_then(|l| l.engine()) {
            let info = self.info(&e.session);
            c.status = info.status.map(|s| s.status);
            c.attention = info.attention;
            c.engine = Some(info.id.full());
        }
        c
    }

    async fn chat_info_now(&self, id: &str) -> Result<ChatInfo, Error> {
        let c = self.rec_get(id).await?.ok_or_else(|| Error::missing("chat", id))?;
        let ids = self.rec_ids().await;
        let mut c = self.chat_info(c, &[]);
        c.handle = chats::handle(&c.id, ids.iter().map(String::as_str));
        Ok(c)
    }

    fn workspace_name(&self, id: &str) -> String {
        let reg = self.registry.locked();
        reg.workspaces
            .iter()
            .find(|w| w.id == id)
            .or_else(|| reg.workspaces.iter().find(|w| w.name == MAIN_WORKSPACE))
            .map_or_else(|| MAIN_WORKSPACE.to_owned(), |w| w.name.clone())
    }

    async fn chat_changed(&self, id: &str) {
        if self.chats.helpers.locked().contains_key(id) {
            return;
        }
        match self.chat_info_now(id).await {
            Ok(info) => {
                let _ = self.events.send(DomainEvent::ChatUpdated { info });
            }
            Err(e) => tracing::debug!("chat {id}: {}", e.message),
        }
    }

    // ── requests ──────────────────────────────────────────────────────────────────────────

    pub(crate) async fn handle_chat(
        self: &Arc<Self>,
        req: Request,
        caller: crate::mail::Caller,
    ) -> Result<Reply, Error> {
        // A chat's own engine asking (its history tools, SPEC §22.10): it may only read, and only
        // what `[chat] history_scope` lets it see. Which chat it is comes from the kernel (the
        // process's session), not from anything it says.
        let scope = match self.engine_chat_of(caller) {
            Some(chat) => {
                if !matches!(
                    req,
                    Request::ChatSearch { .. }
                        | Request::ChatRead { .. }
                        | Request::ChatList { .. }
                ) {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        "a chat's agent may search and read chats, not change them",
                    ));
                }
                Some(self.history_scope(&chat).await?)
            }
            None => None,
        };
        match req {
            Request::ChatSearch {
                query,
                chat,
                workspace,
                agent,
                pinned,
                archived,
                limit,
                machines,
            } => {
                let hits = self
                    .chat_search(
                        &query,
                        chat.clone(),
                        workspace.clone(),
                        agent.clone(),
                        pinned,
                        archived,
                        limit,
                        scope.as_ref(),
                    )
                    .await?;
                // Every machine that is up searches its own (SPEC §22.10), interleaved by each
                // one's rank, as bm25 scores don't compare across machines. A search narrowed to
                // a chat or a workspace here is about this machine only.
                let hits = if machines && scope.is_none() && chat.is_none() && workspace.is_none() {
                    self.search_machines(hits, &query, agent, pinned, archived, limit).await
                } else {
                    hits
                };
                Ok(Reply::ChatHits { hits })
            }
            Request::ChatFind { question, agent, workspace } => {
                let info = self.chat_find(question, agent, workspace).await?;
                Ok(Reply::Chat { info })
            }
            // The history tools' list: what the asking chat may see.
            Request::ChatList { pinned, archived, limit, .. } if scope.is_some() => {
                let scope = scope.unwrap_or_default();
                let db = self.chat_db()?;
                let ids = db.ids().await.map_err(store_err)?;
                let rows = db
                    .list(ListFilter {
                        archived,
                        pinned_only: pinned,
                        limit: limit.unwrap_or(0) as usize,
                        ..ListFilter::default()
                    })
                    .await
                    .map_err(store_err)?;
                let chats = rows
                    .into_iter()
                    .filter(|c| scope.sees(c) && scope.itself.as_deref() != Some(c.id.as_str()))
                    .map(|c| {
                        let mut c = self.chat_info(c, &[]);
                        c.handle = chats::handle(&c.id, ids.iter().map(String::as_str));
                        c
                    })
                    .collect();
                Ok(Reply::Chats { chats })
            }
            Request::ChatNew {
                workspace,
                agent,
                preset,
                title,
                prompt,
                incognito,
                folders,
                compare,
                confirmed,
            } => {
                let incognito = incognito || self.config().chat.incognito;
                let compare: Vec<String> = compare
                    .iter()
                    .flat_map(|a| a.split(','))
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect();
                if !compare.is_empty() {
                    // Asked of several agents: every name is known before the chat is made.
                    if incognito {
                        return Err(Error::invalid(
                            "an incognito chat keeps no branches, and each answer of a compare is one",
                        ));
                    }
                    let ws = workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.to_owned());
                    let ws = self.find_workspace(&ws)?.name;
                    let (a, p) = self.chat_agent(agent.as_deref(), &ws)?;
                    // Counted once each (`claude,claude` is one agent).
                    if self.compare_who((&a, &p), &ws, &compare, confirmed)?.len() < 2 {
                        return Err(Error::invalid(
                            "a compare needs two different agents or more (claude,codex)",
                        ));
                    }
                }
                let info =
                    self.chat_new(workspace, agent, preset, title, incognito, folders).await?;
                if !compare.is_empty()
                    && let Some(p) = prompt.clone().filter(|p| !p.trim().is_empty())
                {
                    if let Err(e) = self.chat_compare(&info, compare, Some(p), confirmed).await {
                        let live = self.chats.live(&info.id);
                        self.chat_notice(&live, &info.id, NoticeLevel::Error, &e.message).await;
                    }
                    return Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? });
                }
                // The chat is made whatever the agent does: an agent that won't start is said in
                // it (a notice), and the reply still names it, so a client shows it, retries in
                // it, or deletes it, rather than making another.
                if let Some(p) = prompt.filter(|p| !p.trim().is_empty())
                    && let Err(e) = self.chat_send(&info.id, p).await
                {
                    tracing::debug!(chat = %info.id, "first message: {}", e.message);
                }
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatList { workspace, all, pinned, archived, limit } => {
                let db = self.chat_db()?;
                let ws = match (&workspace, all) {
                    (_, true) => None,
                    (Some(w), false) => Some(self.find_workspace(w)?.id),
                    (None, false) => None,
                };
                let ids = db.ids().await.map_err(store_err)?;
                let rows = db
                    .list(ListFilter {
                        workspace: ws.clone(),
                        pinned_only: pinned,
                        archived,
                        limit: limit.unwrap_or(0) as usize,
                        sides: true,
                    })
                    .await
                    .map_err(store_err)?;
                let mut rows = rows;
                // Incognito chats are listed while they last (the sidebar shows them, `◌`).
                if !archived {
                    let mine: Vec<ChatInfo> = self
                        .visible_incognito()
                        .into_iter()
                        .filter(|c| ws.as_ref().is_none_or(|w| c.workspace == *w))
                        .filter(|c| !pinned || c.pinned)
                        .collect();
                    rows.extend(mine);
                }
                let ids: Vec<String> =
                    ids.into_iter().chain(self.chats.incognito.locked().keys().cloned()).collect();
                let chats = rows
                    .into_iter()
                    .map(|c| {
                        let mut c = self.chat_info(c, &[]);
                        c.handle = chats::handle(&c.id, ids.iter().map(String::as_str));
                        c
                    })
                    .collect();
                Ok(Reply::Chats { chats })
            }
            Request::ChatRead { chat, all } => {
                let info = self.find_scoped(&chat, scope.as_ref()).await?;
                let nodes = if all && !info.incognito {
                    self.chat_db()?.nodes(info.id.clone()).await.map_err(store_err)?
                } else {
                    self.rec_branch(&info.id, None).await?
                };
                let siblings = if info.incognito {
                    Vec::new()
                } else {
                    let shown = self.rec_branch(&info.id, None).await?;
                    let ids = shown.into_iter().map(|n| n.id).collect();
                    self.chat_db()?.siblings(info.id.clone(), ids).await.map_err(store_err)?
                };
                Ok(Reply::ChatTranscript { info, nodes, siblings })
            }
            Request::ChatRetry { chat, node } => {
                let info = self.find_chat(&chat).await?;
                if info.source.is_some() {
                    return Err(compare::imported_refusal(&info));
                }
                self.chat_retry(&info, node.as_deref()).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatEdit { chat, node, text } => {
                let info = self.find_chat(&chat).await?;
                if info.source.is_some() {
                    return Err(compare::imported_refusal(&info));
                }
                if text.trim().is_empty() {
                    return Err(Error::invalid("nothing to send"));
                }
                self.chat_edit(&info, &node, text).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatBranch { chat, to } => {
                let info = self.find_chat(&chat).await?;
                self.chat_branch(&info, &to).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatFork { chat, node, agent, side } => {
                let info = self.find_chat(&chat).await?;
                let made = self.chat_fork(&info, node.as_deref(), agent, side).await?;
                Ok(Reply::Chat { info: made })
            }
            Request::ChatSend { chat, text } => {
                let info = self.find_chat(&chat).await?;
                if text.trim().is_empty() {
                    return Err(Error::invalid("nothing to send"));
                }
                // An imported chat stays as its agent wrote it: it carries on as a fork, and the
                // reply names the fork (SPEC §22.10).
                let info = match &info.source {
                    Some(_) => self.continue_imported(&info).await?,
                    None => info,
                };
                self.chat_send(&info.id, text).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatAnswer { chat, decision } => {
                let info = self.find_chat(&chat).await?;
                let d = Decision::parse(&decision).ok_or_else(|| {
                    Error::invalid(format!("{decision:?}: allow, always or deny"))
                })?;
                let e = self.chats.get_live(&info.id).and_then(|l| l.engine());
                let st = e.as_ref().and_then(|e| e.session.structured.clone());
                if !st.is_some_and(|st| st.answer(d)) {
                    return Err(Error::conflict(format!("{} isn't asking anything", info.addr())));
                }
                Ok(Reply::Done)
            }
            Request::ChatStop { chat } => {
                let info = self.find_chat(&chat).await?;
                self.stop_compare(&info.id);
                if let Some(st) = self
                    .chats
                    .get_live(&info.id)
                    .and_then(|l| l.engine())
                    .and_then(|e| e.session.structured.clone())
                {
                    st.interrupt();
                }
                Ok(Reply::Done)
            }
            Request::ChatUpdate { chat, title, pinned, archived, agent, preset, keep, folders } => {
                let info = self.find_chat(&chat).await?;
                let folders = folders.map(|f| chat_folders(&f)).transpose()?;
                if info.incognito {
                    return self
                        .update_incognito(&info, title, pinned, archived, agent, preset, folders)
                        .await;
                }
                // An imported chat keeps the agent that wrote it (its title, pin and archive
                // are the person's to change).
                if info.source.is_some()
                    && (agent.is_some() || preset.is_some() || folders.is_some())
                {
                    return Err(compare::imported_refusal(&info));
                }
                let db = self.chat_db()?;
                if keep == Some(true) && info.side_of.is_some() {
                    db.keep_side(info.id.clone()).await.map_err(store_err)?;
                    // A kept side chat is a fork (§22.7); a title the user typed stays.
                    if let Some(rest) = info.title.as_deref().and_then(|t| t.strip_prefix("side: "))
                        && info.title_src != "user"
                    {
                        db.set_title(
                            info.id.clone(),
                            Some(format!("fork of {rest}")),
                            "auto".into(),
                        )
                        .await
                        .map_err(store_err)?;
                    }
                }
                if let Some(t) = title {
                    let t = t.trim().to_owned();
                    if t.is_empty() {
                        // An empty title asks for a new automatic one.
                        db.set_title(info.id.clone(), None, "auto".into())
                            .await
                            .map_err(store_err)?;
                        self.title_chat(&info.id, true);
                    } else {
                        db.set_title(
                            info.id.clone(),
                            Some(chats::first_line_title(&t)),
                            "user".into(),
                        )
                        .await
                        .map_err(store_err)?;
                    }
                }
                if let Some(p) = pinned {
                    db.set_pinned(info.id.clone(), p).await.map_err(store_err)?;
                }
                if let Some(a) = archived {
                    db.set_archived(info.id.clone(), a).await.map_err(store_err)?;
                }
                if let Some(a) = agent {
                    let ws =
                        self.find_workspace(&info.workspace).map(|w| w.name).unwrap_or_default();
                    let (agent, profile) = self.chat_agent(Some(&a), &ws)?;
                    db.set_agent(info.id.clone(), agent, profile).await.map_err(store_err)?;
                    // The next message goes to the new agent, seeded with the conversation.
                    self.stop_chat_engine(&info.id).await;
                }
                if let Some(p) = preset {
                    if self.config().chat.preset(&p).is_none() {
                        return Err(Error::invalid(format!(
                            "{p:?} isn't a preset ({})",
                            self.config().chat.presets().join(", ")
                        )));
                    }
                    db.set_tools(info.id.clone(), p).await.map_err(store_err)?;
                    self.stop_chat_engine(&info.id).await;
                }
                if let Some(f) = folders {
                    db.set_folders(info.id.clone(), f).await.map_err(store_err)?;
                    // The next message starts the agent again, with the folders it may read.
                    self.stop_chat_engine(&info.id).await;
                }
                self.chat_changed(&info.id).await;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatDelete { chat } => {
                let info = self.find_chat(&chat).await?;
                // A compare under way stops with it (its answers have nowhere to go).
                self.stop_compare(&info.id);
                self.stop_chat_engine(&info.id).await;
                if info.incognito {
                    self.end_incognito(&info.id).await;
                    return Ok(Reply::Done);
                }
                self.chat_db()?.delete(info.id.clone()).await.map_err(store_err)?;
                self.remove_chat_files(&info).await;
                self.chats.live.locked().remove(&info.id);
                let _ = self.events.send(DomainEvent::ChatRemoved { id: info.id, machine: None });
                Ok(Reply::Done)
            }
            Request::ChatTask { chat, project, agent, name, confirmed } => {
                self.chat_task(&chat, project, agent, name, confirmed).await
            }
            Request::ChatCompare { chat, agents, text, confirmed } => {
                let info = self.find_chat(&chat).await?;
                self.chat_compare(&info, agents, text, confirmed).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatPick { chat, node } => {
                let info = self.find_chat(&chat).await?;
                self.chat_pick(&info, node.as_deref()).await?;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatImport { agent, profile, since, dry_run } => {
                let report = self.chat_import(agent, profile, since, dry_run).await?;
                Ok(Reply::ChatImported { report })
            }
            Request::ChatPresets => {
                let config = self.config();
                let presets = config
                    .chat
                    .presets()
                    .into_iter()
                    .filter_map(|name| {
                        let t = config.chat.preset(&name)?;
                        Some(chats::ChatPreset {
                            builtin: ptyro_core::config::Chat::BUILTIN.contains(&name.as_str())
                                && !config.chat.tools.contains_key(&name),
                            about: t.describe(),
                            read: t.read,
                            shell: t.shell,
                            name,
                        })
                    })
                    .collect();
                Ok(Reply::ChatPresets { presets })
            }
            Request::ChatExport { chat, format } => {
                let info = self.find_chat(&chat).await?;
                let nodes = self.rec_branch(&info.id, None).await?;
                let text = match format.as_str() {
                    "json" => serde_json::to_string_pretty(
                        &serde_json::json!({ "chat": info, "nodes": nodes }),
                    )
                    .map_err(|e| Error::internal(e.to_string()))?,
                    "" | "md" | "markdown" => markdown(&info, &nodes),
                    f => return Err(Error::invalid(format!("{f:?}: md or json"))),
                };
                Ok(Reply::Text { text })
            }
            _ => Err(Error::internal("not a chat request")),
        }
    }

    /// The agent and profile for a chat: the one asked for (`agent@profile`), else `[chat]
    /// agent`, else the default agent, else the first installed one that has a structured mode.
    fn chat_agent(&self, asked: Option<&str>, workspace: &str) -> Result<(String, String), Error> {
        let config = self.config();
        let wanted = asked
            .map(str::to_owned)
            .or_else(|| Some(config.chat.agent.clone()).filter(|a| !a.is_empty()));
        let (agents, default) = self.agent_choices();
        let structured = |name: &str| {
            let def = ptyro_agents::def(&config, name);
            def.protocol.is_some()
                || ptyro_drive::protocol_for(&ptyro_agents::adapter_name(&config, name)).is_some()
        };
        let (name, profile) = match wanted {
            Some(w) => {
                let (a, p) = ptyro_agents::split_agent(&w);
                if ptyro_agents::command(&config, a).is_none() {
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        format!(
                            "no agent named {a:?} (known: {})",
                            ptyro_agents::names(&config).join(", ")
                        ),
                    ));
                }
                if !structured(a) {
                    return Err(Error::new(
                        ErrorCode::Unsupported,
                        format!(
                            "{a} can't chat: it has no structured mode (an ACP agent: `protocol = \"acp\"` in [agents.{a}])"
                        ),
                    ));
                }
                if let Some(c) = agents.iter().find(|c| c.name == a && !c.installed) {
                    let fix = ptyro_agents::locate::install_hint(&c.program).map_or_else(
                        || format!("install it, or set [agents.{a}] command to its full path"),
                        |h| format!("`{h}` installs it"),
                    );
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        format!(
                            "{a} isn't installed ({} is nowhere ptyro looks): {fix}",
                            c.program
                        ),
                    ));
                }
                (a.to_owned(), p.map(str::to_owned))
            }
            None => {
                let installed = |n: &str| agents.iter().any(|c| c.name == n && c.installed);
                let pick = std::iter::once(default.as_str())
                    .chain(["claude", "codex", "gemini", "opencode"])
                    .find(|n| installed(n) && structured(n))
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::NotFound,
                            "No agent found to chat with. Install one, then try again: Gemini CLI `npm i -g @google/gemini-cli`, Codex `npm i -g @openai/codex`, or Claude Code `curl -fsSL https://claude.ai/install.sh | bash` (`ptyro doctor` shows what it finds)",
                        )
                    })?;
                (pick.to_owned(), None)
            }
        };
        let res = self.resolve_profile(profile.as_deref(), None, workspace, false, true)?;
        Ok((name, res.profile))
    }

    async fn chat_new(
        self: &Arc<Self>,
        workspace: Option<String>,
        agent: Option<String>,
        preset: Option<String>,
        title: Option<String>,
        incognito: bool,
        folders: Vec<String>,
    ) -> Result<ChatInfo, Error> {
        let db = self.chat_db()?;
        let ws = match workspace.as_deref() {
            Some(w) => self.find_workspace(w)?,
            None => self.find_workspace(MAIN_WORKSPACE)?,
        };
        let (agent, profile) = self.chat_agent(agent.as_deref(), &ws.name)?;
        let config = self.config();
        let folders = chat_folders(&folders)?;
        // Folders and no preset asked for: a chat given folders reads them (§22.5).
        let preset = preset.unwrap_or_else(|| {
            let default = config.chat.preset.clone();
            let reads = config.chat.preset(&default).is_some_and(|t| t.read);
            if folders.is_empty() || reads { default } else { "read".into() }
        });
        if config.chat.preset(&preset).is_none() {
            return Err(Error::invalid(format!(
                "{preset:?} isn't a preset ({})",
                config.chat.presets().join(", ")
            )));
        }
        let id = chats::new_id();
        let title = title.map(|t| chats::first_line_title(&t)).filter(|t| !t.is_empty());
        if incognito {
            // Memory only: nothing of it reaches chat.db (SPEC §22.9).
            let now = now_ms();
            let row = ChatInfo {
                id: id.clone(),
                handle: String::new(),
                workspace: ws.id,
                machine: None,
                title_src: if title.is_some() { "user" } else { "auto" }.into(),
                title,
                agent,
                profile,
                tools: preset,
                pinned: false,
                archived: false,
                incognito: true,
                forked: false,
                side_of: None,
                leaf: None,
                nodes: 0,
                unread: 0,
                created_ms: now,
                last_ms: now,
                cost_micro_usd: 0,
                status: None,
                attention: None,
                engine: None,
                folders,
                best_effort: false,
                shell: false,
                comparing: None,
                source: None,
                source_dir: None,
            };
            self.chats.incognito.locked().insert(id.clone(), row);
            let info = self.chat_info_now(&id).await?;
            let _ = self.events.send(DomainEvent::ChatUpdated { info: info.clone() });
            return Ok(info);
        }
        db.create(NewChat {
            id: id.clone(),
            workspace: ws.id,
            title_src: if title.is_some() { "user" } else { "auto" }.into(),
            title,
            agent,
            profile,
            tools: preset,
            forked_from: None,
            side_of: None,
            folders,
        })
        .await
        .map_err(store_err)?;
        let info = self.chat_info_now(&id).await?;
        let _ = self.events.send(DomainEvent::ChatUpdated { info: info.clone() });
        Ok(info)
    }

    // ── sending, and the engine ───────────────────────────────────────────────────────────

    pub(crate) async fn chat_send(self: &Arc<Self>, id: &str, text: String) -> Result<(), Error> {
        let lock = self.chats.lock_for(id);
        let _one = lock.lock().await;
        self.chat_db()?;
        let info = self.rec_get(id).await?.ok_or_else(|| Error::missing("chat", id))?;
        let live = self.chats.live(id);
        self.busy_check(&live, &info)?;
        // The record first: the message is kept whatever the engine does.
        let branch = self.rec_branch(id, None).await?;
        let parent = if info.incognito {
            branch.last().map(|n| n.id.clone())
        } else {
            info.leaf.clone().or_else(|| branch.last().map(|n| n.id.clone()))
        };
        let user = ChatNode {
            id: chats::new_id(),
            parent: parent.clone(),
            at_ms: now_ms(),
            body: ChatBody::User { text: text.clone() },
        };
        self.rec_put_now(id, user.clone()).await?;
        if live.items.locked().items.is_empty() && !branch.is_empty() {
            self.reload_branch(&live).await;
        }
        live.items.locked().upsert(user.item());
        *live.leaf.locked() = Some(user.id.clone());
        live.bump();
        if let Some(e) = live.engine() {
            // Carrying on: whatever it hasn't seen (none, usually) is folded into the prompt.
            let conv = match info.incognito {
                true => None,
                false => self.chat_db()?.current_conv(id.to_owned()).await.map_err(store_err)?,
            };
            let seen =
                conv.as_ref().and_then(|c| c.node.clone()).or_else(|| live.given.locked().clone());
            let unseen = unseen_since(&branch, seen.as_deref());
            let prompt = with_seed(&unseen, &text, self.config().chat.seed_tokens);
            if let Some(st) = &e.session.structured {
                st.prompt(prompt);
            }
            self.chat_changed(id).await;
            return Ok(());
        }
        let started = self.start_engine(&live, &info, &branch, &user).await;
        if let Err(e) = &started {
            self.chat_notice(&live, id, NoticeLevel::Error, &e.message).await;
        }
        self.chat_changed(id).await;
        started
    }

    /// `find_chat`, as a chat's history tools see it: a chat outside `scope` (or the asking chat
    /// itself) is not found, and an ambiguous name names none of the chats it matched.
    async fn find_scoped(
        &self,
        addr: &str,
        scope: Option<&HistoryScope>,
    ) -> Result<ChatInfo, Error> {
        let Some(scope) = scope else { return self.find_chat(addr).await };
        let hidden = || Error::missing("chat", addr);
        let info = self.find_chat(addr).await.map_err(|_| hidden())?;
        let row = self.rec_get(&info.id).await?.ok_or_else(hidden)?;
        if !scope.sees(&row) || scope.itself.as_deref() == Some(row.id.as_str()) {
            return Err(hidden());
        }
        Ok(info)
    }

    /// The chat whose engine `caller` runs in, if it is one.
    pub(crate) fn engine_chat_of(&self, caller: crate::mail::Caller) -> Option<String> {
        if caller.is_link() {
            return None;
        }
        let s = self.session_of_pid(caller.pid?)?;
        s.meta.locked().place.chat.clone()
    }

    /// What a chat's history tools may see (`[chat] history_scope`): its profile's chats, its
    /// workspace's, or all; never an incognito one, nor itself.
    async fn history_scope(&self, chat: &str) -> Result<HistoryScope, Error> {
        let c = self.rec_get(chat).await?.ok_or_else(|| Error::missing("chat", chat))?;
        let scope = self.config().chat.history_scope;
        Ok(HistoryScope {
            profile: (scope == "profile").then(|| c.profile.clone()),
            workspace: (scope == "workspace").then(|| c.workspace.clone()),
            itself: Some(c.id),
        })
    }

    /// Search this machine's chats (SPEC §22.10).
    #[allow(clippy::too_many_arguments)]
    async fn chat_search(
        &self,
        query: &str,
        chat: Option<String>,
        workspace: Option<String>,
        agent: Option<String>,
        pinned: bool,
        archived: bool,
        limit: Option<u32>,
        scope: Option<&HistoryScope>,
    ) -> Result<Vec<ChatHit>, Error> {
        let db = self.chat_db()?;
        let q = chats::parse_query(query, now_ms()).map_err(Error::invalid)?;
        if q.fts.is_empty() && q.words.is_empty() {
            return Err(Error::invalid("say what to look for (words, \"a phrase\", prefix*)"));
        }
        // A one- or two-letter prefix matches half the index (CHAT0: 100–200 ms): wait for more.
        if q.fts
            .split_whitespace()
            .any(|t| t.ends_with('*') && t.trim_matches(['"', '*']).chars().count() < 3)
        {
            return Err(Error::invalid("a prefix needs at least 3 letters (`migr*`)"));
        }
        let chat = match chat {
            Some(c) => Some(self.find_scoped(&c, scope).await?.id),
            None => None,
        };
        let workspace = match workspace.or(q.workspace.clone()) {
            Some(w) => Some(self.find_workspace(&w)?.id),
            None => None,
        };
        let filter = ptyro_store::chat::SearchFilter {
            fts: q.fts.clone(),
            words: q.words.clone(),
            chat,
            workspace: scope.and_then(|s| s.workspace.clone()).or(workspace),
            agent: agent.or(q.agent.clone()),
            profile: scope.and_then(|s| s.profile.clone()),
            since_ms: q.since_ms,
            pinned,
            archived,
            limit: limit.unwrap_or(30) as usize,
        };
        let raw = self.search_hits(&db, filter, &q).await?;
        let ids = self.rec_ids().await;
        Ok(raw
            .into_iter()
            .filter(|(h, _)| scope.is_none_or(|s| s.itself.as_deref() != Some(h.chat.id.as_str())))
            .map(|(h, how)| {
                let mut c = self.chat_info(h.chat, &[]);
                c.handle = chats::handle(&c.id, ids.iter().map(String::as_str));
                ChatHit {
                    chat: c,
                    node: h.node,
                    kind: h.kind,
                    snippet: h.snippet,
                    at_ms: h.at_ms,
                    rank: (h.rank * 1000.0) as i64,
                    how: how.as_str().into(),
                }
            })
            .collect())
    }

    /// The same search on every linked machine that is up, interleaved with ours by rank.
    #[allow(clippy::too_many_arguments)]
    async fn search_machines(
        &self,
        ours: Vec<ChatHit>,
        query: &str,
        agent: Option<String>,
        pinned: bool,
        archived: bool,
        limit: Option<u32>,
    ) -> Vec<ChatHit> {
        let mut lists = vec![ours];
        for m in self.online_machines() {
            let req = Request::ChatSearch {
                query: query.to_owned(),
                chat: None,
                workspace: None,
                agent: agent.clone(),
                pinned,
                archived,
                limit,
                machines: false,
            };
            if let Ok(Reply::ChatHits { hits }) = self.forward(&m, req).await {
                let hidden = self.hidden_of(&m);
                lists.push(
                    hits.into_iter()
                        .filter(|h| {
                            !ptyro_core::model::split_machine(&h.chat.workspace)
                                .is_some_and(|(_, w)| hidden.iter().any(|x| x == w))
                        })
                        .collect(),
                );
            }
        }
        let mut out = interleave(lists);
        out.truncate(limit.unwrap_or(30) as usize);
        out
    }

    /// `chat find` (SPEC §22.10): an incognito chat whose agent has only the history tools,
    /// asked the question. Counted against `[chat] daily_cap` with the titles.
    async fn chat_find(
        self: &Arc<Self>,
        question: String,
        agent: Option<String>,
        workspace: Option<String>,
    ) -> Result<ChatInfo, Error> {
        if question.trim().is_empty() {
            return Err(Error::invalid("what should it find?"));
        }
        let info = self.chat_new(workspace, agent, None, None, true, Vec::new()).await?;
        let capped = self.count_cap(
            &format!("chat-cap:{}", info.profile),
            self.config().chat.daily_cap,
            "[chat] daily_cap",
        );
        if let Err(e) = capped {
            self.end_incognito(&info.id).await;
            return Err(e);
        }
        if let Some(c) = self.chats.incognito.locked().get_mut(&info.id) {
            c.tools = FIND_PRESET.into();
            c.title = Some(chats::first_line_title(&format!("find: {question}")));
            c.title_src = "user".into();
        }
        // Its agent didn't start: the reason is the answer, and nothing is left behind.
        if let Err(e) = self.chat_send(&info.id, question).await {
            self.stop_chat_engine(&info.id).await;
            self.end_incognito(&info.id).await;
            return Err(e);
        }
        self.chat_info_now(&info.id).await
    }

    /// One answer at a time: a message sent mid-answer would interleave with it (and be
    /// answered after it anyway). The client says so and keeps the text.
    fn busy_check(&self, live: &Live, info: &ChatInfo) -> Result<(), Error> {
        if self.chats.comparing.locked().contains_key(&info.id) {
            return Err(Error::new(
                ErrorCode::Busy,
                format!(
                    "c:{} is comparing answers: wait for them, or stop it (`ptyro chat stop`, Ctrl-C in the chat)",
                    chats::handle(&info.id, [info.id.as_str()])
                ),
            ));
        }
        let Some(e) = live.engine() else { return Ok(()) };
        match e.session.status() {
            Some(AgentStatus::Working | AgentStatus::Starting) => Err(Error::new(
                ErrorCode::Busy,
                format!(
                    "c:{} is still answering: wait for it, or stop it (`ptyro chat stop`, Ctrl-C in the chat)",
                    chats::handle(&info.id, [info.id.as_str()])
                ),
            )),
            Some(AgentStatus::Blocked) => Err(Error::new(
                ErrorCode::Busy,
                "it is waiting for your answer to its permission request first",
            )),
            _ => Ok(()),
        }
    }

    /// Ask again (SPEC §22.7): the message `node` answers (or is), answered afresh beside the old
    /// answer, which stays as a branch.
    async fn chat_retry(
        self: &Arc<Self>,
        info: &ChatInfo,
        node: Option<&str>,
    ) -> Result<(), Error> {
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, info)?;
        let branch = self.rec_branch(&info.id, None).await?;
        // The person's message it answers: `node` itself, or the last before it.
        let upto = match node {
            Some(n) => {
                let n = self.find_node(&info.id, n).await?;
                let full = self.rec_branch(&info.id, Some(n.id.clone())).await?;
                full.iter()
                    .rposition(|x| matches!(x.body, ChatBody::User { .. }))
                    .map(|i| full[i].clone())
            }
            None => branch.iter().rev().find(|x| matches!(x.body, ChatBody::User { .. })).cloned(),
        };
        let user = upto.ok_or_else(|| Error::invalid("nothing to retry: no message yet"))?;
        self.answer_afresh(&live, info, user).await
    }

    /// Say it differently (SPEC §22.7): `text` beside the message `node`, answered afresh.
    async fn chat_edit(
        self: &Arc<Self>,
        info: &ChatInfo,
        node: &str,
        text: String,
    ) -> Result<(), Error> {
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, info)?;
        let old = self.find_node(&info.id, node).await?;
        if !matches!(old.body, ChatBody::User { .. }) {
            return Err(Error::invalid("only a message of yours can be edited (retry an answer)"));
        }
        let new = ChatNode {
            id: chats::new_id(),
            parent: old.parent.clone(),
            at_ms: now_ms(),
            body: ChatBody::User { text },
        };
        self.rec_put_now(&info.id, new.clone()).await?;
        self.answer_afresh(&live, info, new).await
    }

    /// Show the branch ending at `user` (a person's message) and have it answered by a new
    /// engine, seeded with what comes before it: a retry, or an edit.
    async fn answer_afresh(
        self: &Arc<Self>,
        live: &Arc<Live>,
        info: &ChatInfo,
        user: ChatNode,
    ) -> Result<(), Error> {
        self.stop_chat_engine(&info.id).await;
        let before = match &user.parent {
            Some(p) => self.rec_branch(&info.id, Some(p.clone())).await?,
            None => Vec::new(),
        };
        if info.incognito {
            // Memory only: the old answer goes (an incognito chat keeps no branches).
            let mut items = before.clone();
            items.push(user.clone());
            replace_items(live, &items);
        } else {
            self.chat_db()?.set_leaf(info.id.clone(), user.id.clone()).await.map_err(store_err)?;
            self.reload_branch(live).await;
        }
        *live.leaf.locked() = Some(user.id.clone());
        let started = self.start_engine(live, info, &before, &user).await;
        if let Err(e) = &started {
            self.chat_notice(live, &info.id, NoticeLevel::Error, &e.message).await;
        }
        self.chat_changed(&info.id).await;
        started
    }

    /// Show another branch: the one through `to`, to its newest tip.
    async fn chat_branch(self: &Arc<Self>, info: &ChatInfo, to: &str) -> Result<(), Error> {
        if info.incognito {
            return Err(Error::invalid("an incognito chat keeps no branches"));
        }
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, info)?;
        self.show_branch(&live, info, to).await
    }

    /// `chat_branch` with the send lock held and the chat free.
    async fn show_branch(
        self: &Arc<Self>,
        live: &Arc<Live>,
        info: &ChatInfo,
        to: &str,
    ) -> Result<(), Error> {
        let node = self.find_node(&info.id, to).await?;
        let db = self.chat_db()?;
        let shown = db.branch(info.id.clone(), None).await.map_err(store_err)?;
        // Already on it (an ancestor of what is shown): nothing changes.
        if shown.iter().any(|n| n.id == node.id) {
            return Ok(());
        }
        let tip = db.tip(info.id.clone(), node.id).await.map_err(store_err)?;
        // Its engine knows the other branch: the next message seeds a new one.
        self.stop_chat_engine(&info.id).await;
        db.set_leaf(info.id.clone(), tip).await.map_err(store_err)?;
        self.reload_branch(live).await;
        self.chat_changed(&info.id).await;
        Ok(())
    }

    /// A new chat from this one's branch up to `node` (SPEC §22.7): its own nodes (a copy), its
    /// own engine. Claude continues natively from the end (`--fork-session`, CHAT0); every other
    /// fork is seeded. A side chat reads the chat and never writes back.
    async fn chat_fork(
        self: &Arc<Self>,
        info: &ChatInfo,
        node: Option<&str>,
        agent: Option<String>,
        side: bool,
    ) -> Result<ChatInfo, Error> {
        if info.incognito {
            return Err(Error::invalid(
                "an incognito chat keeps nothing to fork: `chat export` copies it out",
            ));
        }
        let db = self.chat_db()?;
        let at = match node {
            Some(n) => Some(self.find_node(&info.id, n).await?.id),
            None => None,
        };
        let path = db.branch(info.id.clone(), at.clone()).await.map_err(store_err)?;
        let ws = self.workspace_name(&info.workspace);
        let (agent, profile) = match agent {
            Some(a) => self.chat_agent(Some(&a), &ws)?,
            None => (info.agent.clone(), info.profile.clone()),
        };
        let id = chats::new_id();
        let title = format!("{} {}", if side { "side:" } else { "fork of" }, info.label());
        let src_ws = self.find_workspace(&ws).map(|w| w.id).unwrap_or_default();
        db.create(NewChat {
            id: id.clone(),
            workspace: src_ws,
            title: Some(chats::first_line_title(&title)),
            title_src: "auto".into(),
            agent: agent.clone(),
            profile: profile.clone(),
            tools: info.tools.clone(),
            forked_from: Some((
                info.id.clone(),
                path.last().map(|n| n.id.clone()).unwrap_or_default(),
            )),
            side_of: side.then(|| info.id.clone()),
            folders: info.folders.clone(),
        })
        .await
        .map_err(store_err)?;
        let map = db
            .copy_branch(id.clone(), path, self.config().chat.index_tools)
            .await
            .map_err(store_err)?;
        // Natively, where the agent can: the source's conversation ends where the fork starts,
        // and it is Claude's (it forks at its end only).
        let conv = db.current_conv(info.id.clone()).await.map_err(store_err)?;
        let adapter = ptyro_agents::adapter_name(&self.config(), &agent);
        if let Some(c) = conv
            && adapter == "claude"
            && c.agent == agent
            && c.profile == profile
            && c.conversation_id.is_some()
            && let Some((_, mine)) = map.last().filter(|(src, _)| c.node.as_deref() == Some(src))
        {
            db.add_conv(Conv {
                id: chats::new_id(),
                chat: id.clone(),
                agent: agent.clone(),
                profile: profile.clone(),
                conversation_id: c.conversation_id.clone(),
                node: Some(mine.clone()),
                how: FORK_PENDING.into(),
                ended_ms: None,
            })
            .await
            .map_err(store_err)?;
        }
        let made = self.chat_info_now(&id).await?;
        let _ = self.events.send(DomainEvent::ChatUpdated { info: made.clone() });
        Ok(made)
    }

    /// An incognito chat's changes, in memory (it can't be archived: it isn't kept).
    /// Make a chat a task (SPEC §22.7): an agent session in a new worktree of `project`, whose
    /// first prompt is the shown branch and its last answer; the chat says where it went.
    async fn chat_task(
        self: &Arc<Self>,
        chat: &str,
        project: Option<String>,
        agent: Option<String>,
        name: Option<String>,
        confirmed: bool,
    ) -> Result<Reply, Error> {
        let info = self.find_chat(chat).await?;
        // Incognito keeps nothing (SPEC §22.9), and a task would leave its words in a terminal
        // session, a worktree's name and the coding agent's own history.
        if info.incognito {
            return Err(Error::invalid(
                "an incognito chat keeps nothing, and a task would write it into a session and the agent's own history: `chat export` copies it out, or ask it again in a chat that isn't incognito",
            ));
        }
        if info.source.is_some() {
            return Err(compare::imported_refusal(&info));
        }
        // One thing at a time: not while it answers or compares (its last answer is the plan).
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, &info)?;
        let branch = self.rec_branch(&info.id, None).await?;
        let last = branch.iter().rev().find_map(|n| match &n.body {
            ChatBody::Assistant { text, .. } if !text.trim().is_empty() => Some(text.trim()),
            _ => None,
        });
        let Some(last) = last else {
            return Err(Error::invalid(format!(
                "{} has no answer to carry out yet: ask it something first",
                info.addr()
            )));
        };
        let p = match project {
            Some(p) => self.find_project(&p)?,
            None => self.task_project(&info)?,
        };
        let (agent, profile) = match agent {
            Some(a) => {
                let (a, p) = ptyro_agents::split_agent(&a);
                (a.to_owned(), p.map(str::to_owned))
            }
            // The chat's agent; the project's profile (its pin, its workspace), as any task.
            None => (info.agent.clone(), None),
        };
        let ws = self.workspace_name(&info.workspace);
        // The task's profile, as the worktree will resolve it: when it isn't the chat's, the
        // conversation would reach another account, so the person is asked first.
        if !confirmed {
            let root = self
                .root_worktree(&p.id)
                .ok_or_else(|| Error::missing("worktree", format!("{}/@root", p.slug)))?;
            let home = self.home_workspace(Some(&ws), Some(&p))?;
            let res = self.resolve_profile(
                profile.as_deref(),
                Some(&(root, p.clone())),
                &home,
                false,
                false,
            )?;
            if res.profile != info.profile {
                return Err(Error::new(
                    ErrorCode::NeedsConfirmation,
                    format!(
                        "{} is a chat of profile {}, and the task in {} would run under {} ({}): carry the conversation over to that account?",
                        info.addr(),
                        info.profile,
                        p.slug,
                        res.profile,
                        res.reason.label()
                    ),
                ));
            }
        }
        let seed = chats::seed_block(&branch, self.config().chat.seed_tokens as usize)
            .map(|(b, _)| b)
            .unwrap_or_default();
        let prompt = format!(
            "{seed}\nThe plan from that conversation (chat {}, its last answer):\n\n{last}\n\n\
             Carry this out in this repository.",
            info.addr()
        );
        let spec = ptyro_core::model::WorktreeSpec {
            project: p.id.clone(),
            name: name.filter(|n| !n.trim().is_empty()),
            title: Some(info.label().to_owned()),
            profile,
            confirmed,
            agent: Some(agent.clone()),
            prompt: Some(prompt),
            workspace: Some(ws),
            ..ptyro_core::model::WorktreeSpec::default()
        };
        let reply = self.create_worktree(spec).await?;
        if let Reply::WorktreeCreated { info: wt, agent: started, agent_pending, .. } = &reply {
            let what = match started {
                Some(s) => format!(
                    "made a task: {agent} works on it in {} (session {}; `ptyro attach {}`)",
                    wt.qualified(),
                    s.name,
                    s.name
                ),
                None if *agent_pending => format!(
                    "made a task: {agent} starts in {} once its setup has run",
                    wt.qualified()
                ),
                None => format!("made a task: the worktree {}", wt.qualified()),
            };
            if live.items.locked().items.is_empty() {
                self.reload_branch(&live).await;
            }
            self.chat_notice(&live, &info.id, NoticeLevel::Info, &what).await;
            self.chat_changed(&info.id).await;
        }
        Ok(reply)
    }

    /// The project a chat's task goes to when none is named: the only one of its workspace,
    /// else the only one there is; otherwise the error asks which.
    fn task_project(&self, info: &ChatInfo) -> Result<ptyro_core::model::ProjectInfo, Error> {
        let reg = self.registry.locked();
        let here: Vec<&ptyro_core::model::ProjectInfo> =
            reg.projects.iter().filter(|p| p.machine.is_none()).collect();
        let ws = reg.workspaces.iter().find(|w| w.id == info.workspace || w.name == info.workspace);
        let mine: Vec<&ptyro_core::model::ProjectInfo> = here
            .iter()
            .copied()
            .filter(|p| ws.is_some_and(|w| w.projects.contains(&p.id)))
            .collect();
        match (mine.as_slice(), here.as_slice()) {
            ([p], _) | ([], [p]) => Ok((*p).clone()),
            (_, []) => Err(Error::invalid(
                "a task needs a project, and there is none yet: add one (`ptyro project add PATH`)",
            )),
            (m, all) => {
                let names: Vec<&str> =
                    if m.is_empty() { all } else { m }.iter().map(|p| p.slug.as_str()).collect();
                Err(Error::invalid(format!(
                    "which project should it work in? --project P ({})",
                    names.join(", ")
                )))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_incognito(
        self: &Arc<Self>,
        info: &ChatInfo,
        title: Option<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
        agent: Option<String>,
        preset: Option<String>,
        folders: Option<Vec<String>>,
    ) -> Result<Reply, Error> {
        if archived == Some(true) {
            return Err(Error::invalid("an incognito chat isn't kept, so it can't be archived"));
        }
        let switched = agent.is_some() || preset.is_some() || folders.is_some();
        let resolved = match &agent {
            Some(a) => Some(self.chat_agent(Some(a), &self.workspace_name(&info.workspace))?),
            None => None,
        };
        if let Some(p) = &preset
            && self.config().chat.preset(p).is_none()
        {
            return Err(Error::invalid(format!("{p:?} isn't a preset")));
        }
        {
            let mut m = self.chats.incognito.locked();
            let Some(c) = m.get_mut(&info.id) else { return Err(Error::missing("chat", &info.id)) };
            if let Some(t) = title {
                c.title = Some(chats::first_line_title(&t)).filter(|t| !t.is_empty());
                c.title_src = "user".into();
            }
            if let Some(p) = pinned {
                c.pinned = p;
            }
            if let Some((a, prof)) = resolved {
                c.agent = a;
                c.profile = prof;
            }
            if let Some(p) = preset {
                c.tools = p;
            }
            if let Some(f) = folders {
                c.folders = f;
            }
        }
        if switched {
            // Its conversation lives only in that agent: the next message seeds a new one.
            if let Some(e) = self.chats.get_live(&info.id).and_then(|l| l.engine.locked().take()) {
                e.session.hibernating.store(true, Ordering::Relaxed);
                self.stop_session(&e.session).await;
                self.forget_session(e.session.id);
            }
        }
        self.chat_changed(&info.id).await;
        Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
    }

    /// A notice in the chat (never sent to an engine).
    async fn chat_notice(&self, live: &Arc<Live>, id: &str, level: NoticeLevel, text: &str) {
        let parent = live.leaf.locked().clone();
        let n = ChatNode {
            id: chats::new_id(),
            parent,
            at_ms: now_ms(),
            body: ChatBody::Notice { level, text: text.to_owned() },
        };
        let _ = self.rec_put_now(id, n.clone()).await;
        *live.leaf.locked() = Some(n.id.clone());
        live.items.locked().upsert(n.item());
        live.bump();
    }

    /// Start (or resume) the chat's engine and give it `text` (§22.6).
    async fn start_engine(
        self: &Arc<Self>,
        live: &Arc<Live>,
        info: &ChatInfo,
        branch: &[ChatNode],
        user: &ChatNode,
    ) -> Result<(), Error> {
        let ChatBody::User { text } = &user.body else {
            return Err(Error::internal("not a message"));
        };
        let db = self.chat_db()?;
        let config = self.config();
        let conv = match info.incognito {
            true => None,
            false => db.current_conv(info.id.clone()).await.map_err(store_err)?,
        };
        let fork_pending = conv.as_ref().is_some_and(|c| c.how == FORK_PENDING);
        // Native continue: same agent and profile, a conversation id, and its last node on
        // this branch (a fork's: the source's conversation, forked).
        let native = conv.as_ref().filter(|c| {
            c.agent == info.agent
                && c.profile == info.profile
                && c.conversation_id.is_some()
                && c.node.as_ref().is_some_and(|n| branch.iter().any(|b| &b.id == n))
        });
        let (conv_id, resume, seed_nodes, how) = match native {
            Some(c) => (
                c.id.clone(),
                c.conversation_id.clone(),
                unseen_since(branch, c.node.as_deref()),
                None,
            ),
            None => {
                if let Some(old) = &conv {
                    db.end_conv(old.id.clone());
                }
                let how = if branch.is_empty() { "new" } else { "seed" };
                (chats::new_id(), None, branch.to_vec(), Some(how))
            }
        };
        let preset = config.chat.preset(&info.tools).unwrap_or_default();
        let dir = scratch(self, &info.id);
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .map_err(|e| Error::internal(format!("{}: {e}", dir.display())))?;
        }
        let adapter = ptyro_agents::adapter_name(&config, &info.agent);
        let protocol = ptyro_agents::def(&config, &info.agent)
            .protocol
            .clone()
            .or_else(|| ptyro_drive::protocol_for(&adapter).map(str::to_owned))
            .unwrap_or_default();
        let plan = self.profile_plan(&info.profile).await?;
        let mcp = self.codex_mcp_servers(&adapter, &plan).await;
        let readable = codex_needs(&plan, &self.agent_program(&info.agent).unwrap_or_default());
        let best_effort = adapter == "codex" && mcp.is_none();
        let session_id = SessionId::new();
        // The chat history tools (SPEC §22.10), from `ptyro mcp --chat` in the engine itself;
        // the daemon scopes what they see by the engine they run in.
        let history = (config.chat.history_tool || info.tools == FIND_PRESET).then(|| {
            let ptyro = std::env::current_exe()
                .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned());
            serde_json::json!({
                "type": "stdio",
                "command": ptyro,
                "args": ["mcp", "--chat"],
                "env": {
                    "PTYRO_SESSION_ID": session_id.full(),
                    "PTYRO_CHAT_ID": info.id,
                    "PTYRO_SOCKET": self.paths.socket.to_string_lossy(),
                },
            })
        });
        // The MCP servers the preset keeps on, from the agent's own configuration.
        let (claude_mcp, missing) = match adapter.as_str() {
            "claude" => claude_mcp_defs(&plan, &preset.mcp),
            "codex" => {
                let known = mcp.clone().unwrap_or_default();
                let missing = preset.mcp.iter().filter(|m| !known.contains(m)).cloned().collect();
                (serde_json::Map::new(), missing)
            }
            _ => (serde_json::Map::new(), Vec::new()),
        };
        let extra = ChatLaunch {
            incognito: info.incognito,
            history,
            system: (info.tools == FIND_PRESET).then_some(FIND_PROMPT),
            folders: info.folders.clone(),
            adapter: adapter.clone(),
            claude_mcp,
        };
        let (args, params) =
            preset_launch(&protocol, &preset, &dir, mcp.unwrap_or_default(), &readable, &extra);
        let handle = chats::handle(&info.id, self.rec_ids().await.iter().map(String::as_str));
        let ws = self.workspace_name(&info.workspace);
        let session = self
            .launch(Launch {
                id: session_id,
                argv: args,
                kind: SessionKind::Agent,
                name: self.free_name(&format!("chat-{handle}")),
                cwd: dir,
                size: TermSize::new(100, 30),
                client_env: Default::default(),
                place: Place {
                    profile: info.profile.clone(),
                    workspace: Some(ws),
                    agent: Some(info.agent.clone()),
                    chat: Some(info.id.clone()),
                    ..Place::default()
                },
                replace: false,
                agent: Some(AgentStart {
                    name: info.agent.clone(),
                    resume: resume.clone(),
                    prompt: None,
                    command: None,
                    mcp: false,
                    structured: true,
                    params,
                    fork: fork_pending && resume.is_some(),
                    chat_tools: false,
                }),
            })
            .await?;
        if info.incognito && adapter != "claude" && adapter != "codex" {
            // Gemini's copy is in a folder named for the scratch folder: ptyro removes it
            // when the chat ends (`remove_chat_dirs`). Others' it can't find.
            let what = if adapter == "gemini" {
                format!(
                    "incognito: ptyro keeps nothing, and removes {}'s own copy when the chat ends",
                    info.agent
                )
            } else {
                format!("incognito: ptyro keeps nothing; {} may keep its own copy", info.agent)
            };
            self.chat_notice(live, &info.id, NoticeLevel::Warn, &what).await;
        }
        if !missing.is_empty() {
            let what = format!(
                "MCP {} not in {}'s own configuration, so left out (`{} mcp list` shows what it has)",
                missing.join(", "),
                info.agent,
                info.agent
            );
            self.chat_notice(live, &info.id, NoticeLevel::Warn, &what).await;
        }
        if let (Some(how), false) = (how, info.incognito) {
            db.add_conv(Conv {
                id: conv_id.clone(),
                chat: info.id.clone(),
                agent: info.agent.clone(),
                profile: info.profile.clone(),
                conversation_id: self.info(&session).conversation,
                node: None,
                how: how.into(),
                ended_ms: None,
            })
            .await
            .map_err(store_err)?;
        }
        *live.engine.locked() = Some(Engine { session: Arc::clone(&session) });
        live.quiet.store(resume.is_some(), Ordering::SeqCst);
        if best_effort {
            let what = "tools: best effort (codex mcp list didn't answer, so your MCP servers may be on in this chat)";
            self.chat_notice(live, &info.id, NoticeLevel::Warn, what).await;
        }
        let seed = chats::seed_block(&seed_nodes, config.chat.seed_tokens as usize);
        if let (Some((_, n)), None) = (&seed, &resume) {
            let what =
                format!("continued with {} from the saved conversation ({n} messages)", info.agent);
            // On the branch, between the message and its answer; never sent to an engine.
            self.chat_notice(live, &info.id, NoticeLevel::Info, &what).await;
        }
        let prompt = match &seed {
            Some((block, _)) => format!("{block}{text}"),
            None => text.clone(),
        };
        if let Some(st) = &session.structured {
            st.prompt(prompt);
        }
        *live.given.locked() = Some(user.id.clone());
        // (A native fork that can't resume the source's conversation is seeded too.)
        let resumed = resume.is_some();
        self.follow_engine(Arc::clone(live), session, conv_id, resumed, user.id.clone());
        Ok(())
    }

    /// The user's MCP servers, to switch each off in a Codex chat (CHAT0: its table can't be
    /// emptied, only each entry switched off).
    /// `None` when they couldn't be listed (the chat then says its tools are best effort).
    /// Listed under the chat's profile, so its `CODEX_HOME`'s servers.
    async fn codex_mcp_servers(
        &self,
        adapter: &str,
        plan: &ptyro_agents::profile::Plan,
    ) -> Option<Vec<String>> {
        if adapter != "codex" {
            return Some(Vec::new());
        }
        let argv = self.agent_argv("codex")?;
        let env = plan.apply(self.base_env.iter().cloned());
        tokio::task::spawn_blocking(move || {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..]).args(["mcp", "list", "--json"]).env_clear().envs(env);
            c.stderr(std::process::Stdio::null());
            let Ok((true, out)) = crate::notifier::run_bounded(c, None, Duration::from_secs(5))
            else {
                return None;
            };
            let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
            // Every name, whatever its case: they are only keys of the thread's config.
            Some(
                v.as_array()?
                    .iter()
                    .filter_map(|s| s["name"].as_str().map(str::to_owned))
                    .filter(|n| !n.is_empty())
                    .collect(),
            )
        })
        .await
        .ok()
        .flatten()
    }

    /// Follow an engine: its items become nodes, until it ends or hibernates.
    fn follow_engine(
        self: &Arc<Self>,
        live: Arc<Live>,
        session: Arc<Session>,
        conv: String,
        resumed: bool,
        pending: String,
    ) {
        // (`pending`: the message's node, sent again seeded when a resume fails.)
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let Some(st) = session.structured.clone() else { return };
            let Ok(db) = me.chat_db() else { return };
            let chat = live.id.clone();
            // An incognito chat's nodes are only ever in memory (SPEC §22.9).
            let keep = me.incognito_of(&chat).is_none();
            let mut rx = st.chat_rev.subscribe();
            let mut seen = 0u64;
            // Engine item id → (node id, its parent, last written, the body written).
            let mut nodes: HashMap<String, (String, Option<String>, Instant, Option<ChatBody>)> =
                HashMap::new();
            let mut replied = false;
            let mut failed_resume = false;
            // Stopped on purpose (hibernated), and whether it said why it ended.
            let mut stopped = false;
            let mut said_error = false;
            let mut status = (None, None);
            let idle_for = |me: &Daemon| {
                parse_duration(&me.config().chat.hibernate_after)
                    .unwrap_or(Duration::from_secs(120))
            };
            let index_tools = me.config().chat.index_tools;
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            let mut idle_since = Instant::now();
            loop {
                let changed = tokio::select! {
                    r = rx.changed() => r.is_ok(),
                    _ = tick.tick() => true,
                };
                let items = session.chat_since(seen).map(|(rev, items, _)| (rev, items));
                if let Some((rev, items)) = items {
                    seen = rev;
                    for it in items {
                        if matches!(it.body, ChatBody::User { .. }) {
                            continue; // the chat wrote the person's message itself
                        }
                        // A resumed engine that hasn't answered: its error and turn end are a
                        // failed resume (the conversation is gone), retried below with a seed,
                        // not written into the chat.
                        if resumed && !replied {
                            match &it.body {
                                ChatBody::TurnEnd { .. } => {
                                    failed_resume = true;
                                    continue;
                                }
                                ChatBody::Notice { .. } => continue,
                                _ => {
                                    replied = true;
                                    live.quiet.store(false, Ordering::SeqCst);
                                }
                            }
                        } else if !matches!(
                            it.body,
                            ChatBody::Notice { .. } | ChatBody::TurnEnd { .. }
                        ) {
                            replied = true;
                        }
                        if matches!(it.body, ChatBody::Notice { level: NoticeLevel::Error, .. }) {
                            said_error = true;
                        }
                        let fresh = !nodes.contains_key(&it.id);
                        let entry = nodes.entry(it.id.clone()).or_insert_with(|| {
                            let parent = live.leaf.locked().clone();
                            let id = chats::new_id();
                            *live.leaf.locked() = Some(id.clone());
                            (id, parent, Instant::now() - FLUSH, None)
                        });
                        // An answer's first node may sit beside an earlier one (a retry, an
                        // edit): say where the branch now has others.
                        let first_of_turn = fresh
                            && entry.1.as_ref().is_some_and(|p| {
                                live.items.locked().get(p).is_some_and(|i| {
                                    matches!(
                                        i.body,
                                        ChatBody::User { .. } | ChatBody::Notice { .. }
                                    )
                                })
                            });
                        let node = ChatNode {
                            id: entry.0.clone(),
                            parent: entry.1.clone(),
                            at_ms: it.at_ms,
                            body: it.body.clone(),
                        };
                        live.items.locked().upsert(node.item());
                        let due = !chats::streaming(&it.body) || entry.2.elapsed() >= FLUSH;
                        if due && entry.3.as_ref() != Some(&it.body) {
                            if keep {
                                db.put_node(chat.clone(), node, Some(conv.clone()), index_tools);
                            }
                            entry.2 = Instant::now();
                            entry.3 = Some(it.body.clone());
                        }
                        if first_of_turn && keep {
                            me.refresh_siblings(&live).await;
                        }
                        if let ChatBody::TurnEnd { .. } = &it.body {
                            let leaf = live.leaf.locked().clone();
                            if keep {
                                db.update_conv(
                                    conv.clone(),
                                    me.info(&session).conversation,
                                    leaf.clone(),
                                );
                            }
                            *live.given.locked() = leaf;
                            me.chat_turn_ended(&live).await;
                        }
                    }
                    live.bump();
                }
                let info = me.info(&session);
                let now = (
                    info.status.as_ref().map(|s| s.status),
                    info.attention.as_ref().map(|a| a.since_ms),
                );
                if now != status {
                    status = now;
                    me.chat_changed(&chat).await;
                }
                let working = matches!(
                    status.0,
                    Some(AgentStatus::Working | AgentStatus::Starting | AgentStatus::Blocked)
                );
                if working || !changed {
                    idle_since = Instant::now();
                }
                if !session.is_live() {
                    break;
                }
                if failed_resume {
                    session.hibernating.store(true, Ordering::Relaxed);
                    me.stop_session(&session).await;
                    break;
                }
                if !working && idle_since.elapsed() >= idle_for(&me) && !st.has_pending() {
                    // Not while a message is being sent: it would go to a dying engine.
                    let lock = me.chats.lock_for(&chat);
                    let Ok(_one) = lock.try_lock() else { continue };
                    if live.engine().is_some_and(|e| e.session.id == session.id) {
                        tracing::debug!(chat = %chat, "chat engine hibernates");
                        stopped = true;
                        session.hibernating.store(true, Ordering::Relaxed);
                        me.stop_session(&session).await;
                        *live.engine.locked() = None;
                    }
                    break;
                }
            }
            // Whatever was still streaming is written as it stands.
            for (_, (id, parent, _, written)) in nodes {
                let body = live.items.locked().get(&id).map(|i| i.body.clone());
                if let Some(mut b) = body
                    && written.as_ref() != Some(&b)
                {
                    if let ChatBody::Assistant { streaming, .. } = &mut b {
                        *streaming = false;
                    }
                    if keep {
                        db.put_node(
                            chat.clone(),
                            ChatNode { id, parent, at_ms: now_ms(), body: b },
                            Some(conv.clone()),
                            index_tools,
                        );
                    }
                }
            }
            session.save(None);
            me.forget_session(session.id);
            // It ended before it answered, and said nothing: say so, so nobody waits for good.
            let deliberate = stopped || session.hibernating.load(Ordering::Relaxed);
            if !replied && !resumed && !deliberate && !said_error {
                let agent = session.adapter().unwrap_or_else(|| "the agent".into());
                let text = format!(
                    "{agent} stopped before it answered (is it installed and logged in? `ptyro doctor` checks)"
                );
                me.chat_notice(&live, &chat, NoticeLevel::Error, &text).await;
            }
            let was_ours = {
                let mut e = live.engine.locked();
                if e.as_ref().is_some_and(|e| e.session.id == session.id) {
                    *e = None;
                    true
                } else {
                    false
                }
            } || failed_resume;
            live.quiet.store(false, Ordering::SeqCst);
            // A resume the agent couldn't do (its conversation is gone): start over with the
            // record as the seed.
            if was_ours && resumed && !replied {
                tracing::info!(chat = %chat, "the agent couldn't resume its conversation; seeding");
                db.end_conv(conv.clone());
                // One sender at a time: a message sent meanwhile mustn't start a second engine.
                let lock = me.chats.lock_for(&chat);
                let _one = lock.lock().await;
                if live.engine().is_none()
                    && let Ok(Some(info)) = db.get(chat.clone()).await
                {
                    let mut branch = db.branch(chat.clone(), None).await.unwrap_or_default();
                    // The person's message is the prompt itself, not part of the seed.
                    if let Some(i) = branch.iter().position(|n| n.id == pending) {
                        let user = branch[i].clone();
                        branch.truncate(i);
                        if let Err(e) = me.start_engine(&live, &info, &branch, &user).await {
                            me.chat_notice(&live, &chat, NoticeLevel::Error, &e.message).await;
                        }
                    }
                }
            }
            // An incognito chat ends with its engine: when that ends of itself or goes idle. One
            // taken from this follower (a retry, an edit, another agent) is followed by the next
            // message's; the send lock waits out a retry that is starting it.
            if !keep && (was_ours || stopped) {
                let lock = me.chats.lock_for(&chat);
                let _one = lock.lock().await;
                if live.engine().is_none() {
                    me.end_incognito(&chat).await;
                    return;
                }
            }
            me.chat_changed(&chat).await;
            me.chats.settle(&chat);
        });
    }

    /// An answer is complete: unread when nobody is looking, a title for a new chat.
    async fn chat_turn_ended(self: &Arc<Self>, live: &Arc<Live>) {
        let Ok(db) = self.chat_db() else { return };
        let id = live.id.clone();
        // Incognito: no unread, no titler (nothing of it leaves); the first line is its title.
        if self.incognito_of(&id).is_some() {
            let first = live.items.locked().items.iter().find_map(|i| match &i.body {
                ChatBody::User { text } => Some(chats::first_line_title(text)),
                _ => None,
            });
            if let Some(c) = self.chats.incognito.locked().get_mut(&id)
                && c.title.is_none()
            {
                c.title = first;
            }
            self.chat_changed(&id).await;
            return;
        }
        if live.viewers.load(Ordering::SeqCst) == 0
            && let Ok(Some(c)) = db.get(id.clone()).await
        {
            let _ = db.set_unread(id.clone(), c.unread + 1).await;
            if self.config().chat.notify == "all" {
                let text = format!("answered: {}", c.label());
                if let Some(e) = live.engine() {
                    self.raise(&e.session, AttentionKind::Done, text).await;
                }
            }
        }
        self.title_chat(&id, false);
        self.chat_changed(&id).await;
    }

    /// Stop a chat's engine (deleted, agent switched); its conversation stays resumable.
    async fn stop_chat_engine(self: &Arc<Self>, id: &str) {
        let e = self.chats.get_live(id).and_then(|l| l.engine.locked().take());
        if let Some(e) = e {
            // Stopped on purpose: no "exited" alert.
            e.session.hibernating.store(true, Ordering::Relaxed);
            self.stop_session(&e.session).await;
            e.session.save(None);
            self.forget_session(e.session.id);
        }
    }

    /// A deleted chat's scratch directory, and the project folder it made in Claude's own
    /// history (so `claude --resume` doesn't list dead chats; CHAT0), or Gemini's.
    async fn remove_chat_files(self: &Arc<Self>, info: &ChatInfo) {
        self.remove_chat_dirs(&info.id, Some((&info.agent, &info.profile))).await;
    }

    /// The same by id (`who`: its agent and profile, when known; else only the scratch
    /// directory and the default profile's Claude and Gemini folders).
    async fn remove_chat_dirs(self: &Arc<Self>, id: &str, who: Option<(&str, &str)>) {
        let dir = scratch(self, id);
        let config = self.config();
        let (agent, profile) = who.unwrap_or(("claude", ptyro_core::model::DEFAULT_PROFILE));
        let adapter = ptyro_agents::adapter_name(&config, agent);
        // Gemini keeps a session under `<home>/.gemini/{tmp,history}/<slug>/`, each folder
        // marked with the directory it is for (`.project_root`, read from its source, 0.66.0).
        let gemini_home = match adapter.as_str() {
            "gemini" => self.profile_plan(profile).await.ok().and_then(|plan| {
                plan.set.iter().find(|(k, _)| k == "GEMINI_CLI_HOME").map(|(_, h)| PathBuf::from(h))
            }),
            _ => None,
        };
        if adapter == "gemini" || who.is_none() {
            let home = gemini_home
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .map(|h| h.join(".gemini"));
            if let Some(home) = home {
                remove_gemini_copies(&home, &dir);
            }
        }
        if adapter == "claude"
            && let Ok(plan) = self.profile_plan(profile).await
        {
            let home = ptyro_agents::home_var("claude")
                .and_then(|v| plan.set.iter().find(|(k, _)| k == v).map(|(_, p)| PathBuf::from(p)))
                .or_else(|| ptyro_agents::default_dir("claude"));
            if let Some(home) = home {
                let slug: String = dir
                    .to_string_lossy()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let p = home.join("projects").join(slug);
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(p);
                }
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── following ─────────────────────────────────────────────────────────────────────────

    /// A client follows a chat: the branch now, then each change. Being shown clears unread.
    pub(crate) async fn chat_follow(self: &Arc<Self>, addr: &str) -> Result<Arc<Live>, Error> {
        let info = self.find_chat(addr).await?;
        let db = self.chat_db()?;
        // Not while a message is starting its engine: loading the branch then would replace
        // what that engine has already said.
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        if live.engine().is_none() && !info.incognito {
            db.branch(info.id.clone(), None).await.map_err(store_err)?;
            self.reload_branch(&live).await;
        } else if live.siblings.locked().is_empty() && !info.incognito {
            self.refresh_siblings(&live).await;
        }
        if info.unread > 0 && !info.incognito {
            let _ = db.set_unread(info.id.clone(), 0).await;
            self.chat_changed(&info.id).await;
        }
        // Last, with no await after it: the caller's guard takes it back however it ends.
        live.viewers.fetch_add(1, Ordering::SeqCst);
        Ok(live)
    }

    pub(crate) fn chat_unfollow(&self, live: &Arc<Live>) {
        live.viewers.fetch_sub(1, Ordering::SeqCst);
        self.chats.settle(&live.id);
    }

    /// Whether a chat engine's attention of `kind` reaches you (`[chat] notify`, §22.11).
    pub(crate) fn chat_wants(&self, s: &Session, kind: AttentionKind) -> bool {
        let Some(chat) = s.meta.locked().place.chat.clone() else { return true };
        // A compare's helper: its chat tells how it went (SPEC §22.7).
        if self.chats.helpers.locked().contains_key(&chat) {
            return false;
        }
        let live = self.chats.get_live(&chat);
        // A resume being retried: its error isn't news.
        if live.as_ref().is_some_and(|l| l.quiet.load(Ordering::SeqCst)) {
            return false;
        }
        let shown = live.is_some_and(|l| l.viewers.load(Ordering::SeqCst) > 0);
        let urgent = matches!(kind, AttentionKind::Blocked | AttentionKind::Errored);
        match self.config().chat.notify.as_str() {
            "off" => false,
            // The chat's own "answered" (Done), never the generic idle or exited ones.
            "all" => urgent || (kind == AttentionKind::Done && !shown),
            _ => urgent,
        }
    }

    // ── titles (§22.8) ────────────────────────────────────────────────────────────────────

    /// Give a chat a title once its first answer is in: the titler's, else the first line.
    fn title_chat(self: &Arc<Self>, id: &str, again: bool) {
        let me = Arc::clone(self);
        let id = id.to_owned();
        tokio::spawn(async move {
            let Ok(db) = me.chat_db() else { return };
            let Ok(Some(c)) = db.get(id.clone()).await else { return };
            if c.title.is_some() && !again {
                return;
            }
            let branch = db.branch(id.clone(), None).await.unwrap_or_default();
            let first_user = branch.iter().find_map(|n| match &n.body {
                ChatBody::User { text } => Some(text.clone()),
                _ => None,
            });
            let Some(first_user) = first_user else { return };
            let first_answer = branch.iter().find_map(|n| match &n.body {
                ChatBody::Assistant { text, streaming: false } => Some(text.clone()),
                _ => None,
            });
            let config = me.config();
            let mut title = None;
            if config.chat.title == "auto"
                && let Some(answer) = &first_answer
            {
                match me.make_title(&c.profile, &first_user, answer).await {
                    Ok(t) => title = Some(t),
                    Err(e) => tracing::debug!(chat = %id, "title: {}", e.message),
                }
            }
            let title = title.unwrap_or_else(|| chats::first_line_title(&first_user));
            if !title.is_empty()
                && db.set_title(id.clone(), Some(title), "auto".into()).await.unwrap_or(false)
            {
                me.chat_changed(&id).await;
            }
        });
    }

    async fn make_title(
        self: &Arc<Self>,
        profile: &str,
        user: &str,
        answer: &str,
    ) -> Result<String, Error> {
        let config = self.config();
        let argv = if config.chat.title_command.is_empty() {
            config.summaries.command.clone()
        } else {
            config.chat.title_command.clone()
        };
        if argv.is_empty() {
            return Err(Error::invalid("no title command"));
        }
        // Titles and `chat find` count together, per profile per day (SPEC §22.13).
        self.count_cap(&format!("chat-cap:{profile}"), config.chat.daily_cap, "[chat] daily_cap")?;
        let plan = self.profile_plan(profile).await?;
        let secrets = crate::summary::secrets(&plan);
        let prompt = crate::summary::redact(
            &format!(
                "{TITLE_PROMPT}\n\n[user] {}\n[assistant] {}",
                chats::clip(user, 2000),
                chats::clip(answer, 2000)
            ),
            &secrets,
        );
        let env = plan.apply(self.base_env.iter().cloned());
        let dir = self.paths.data.join("summaries");
        let out = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..])
                .current_dir(&dir)
                .env_clear()
                .envs(env)
                .stderr(std::process::Stdio::null());
            crate::notifier::run_bounded(c, Some(prompt.into_bytes()), Duration::from_secs(60))
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::internal(e.to_string()))?;
        // Claude's JSON `result`, else the plain line it printed (a lone quoted line parses as
        // a JSON string, which has no `result`).
        let raw = String::from_utf8_lossy(&out.1).into_owned();
        let text = crate::summary::parse(&raw)
            .or(Some(raw))
            .map(|t| chats::clean_title(&t))
            .filter(|t| out.0 && !t.is_empty());
        text.ok_or_else(|| Error::internal("the titler gave no title"))
    }

    /// Counts one against today's cap under `key` (UTC days, in memory).
    fn count_cap(&self, key: &str, cap: u32, what: &str) -> Result<(), Error> {
        let key = format!("{key}:{}", now_ms() / 86_400_000);
        let mut views = self.views.locked();
        let n = views.get(&key).and_then(serde_json::Value::as_u64).unwrap_or(0);
        if n >= u64::from(cap) {
            return Err(Error::conflict(format!("today's {cap} are used ({what})")));
        }
        views.insert(key, (n + 1).into());
        Ok(())
    }

    /// A workspace is going: its chats move to `main` (§22.2).
    pub(crate) async fn chats_follow_workspace(&self, removed: &str) {
        let Ok(db) = self.chats.db() else { return };
        let Ok(main) = self.find_workspace(MAIN_WORKSPACE) else { return };
        if let Ok(n) = db.move_workspace(removed.to_owned(), main.id).await
            && n > 0
        {
            tracing::info!("{n} chat(s) moved to {MAIN_WORKSPACE}");
        }
    }

    /// `ptyro doctor`'s line about chats.
    pub(crate) async fn chat_doctor(&self) -> Option<String> {
        if !self.config().chat.enabled {
            return Some("chats: off ([chat] enabled = false)".into());
        }
        let db = match self.chats.db() {
            Ok(db) => db,
            Err(e) => return Some(e.message),
        };
        match db.doctor().await {
            Ok(d) if d.integrity == "ok" && d.fts5 => Some(format!(
                "chats: {} chat(s), {} message(s), {} in {}",
                d.chats,
                d.nodes,
                human_bytes(d.bytes),
                db.path.display()
            )),
            Ok(d) if !d.fts5 => {
                Some("chats: this SQLite has no FTS5, so chat search won't work".into())
            }
            Ok(d) => Some(format!(
                "chats: {} failed its integrity check: {}",
                db.path.display(),
                d.integrity
            )),
            Err(e) => Some(format!("chats: {e}")),
        }
    }
}

fn human_bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.0} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

fn store_err(e: ptyro_store::StoreError) -> Error {
    Error::new(ErrorCode::Internal, format!("chat.db: {e}"))
}

/// What a live chat shows, as nodes (each the next's parent): an incognito chat's record.
fn live_nodes(live: &Live) -> Vec<ChatNode> {
    let mut parent: Option<String> = None;
    live.items
        .locked()
        .items
        .iter()
        .map(|i| {
            let n = ChatNode {
                id: i.id.clone(),
                parent: parent.clone(),
                at_ms: i.at_ms,
                body: i.body.clone(),
            };
            parent = Some(i.id.clone());
            n
        })
        .collect()
}

/// Show exactly `nodes` (an incognito chat's retry or edit drops what came after).
fn replace_items(live: &Live, nodes: &[ChatNode]) {
    load_branch(live, nodes);
}

fn load_branch(live: &Live, branch: &[ChatNode]) {
    let mut t = live.items.locked();
    *t = Transcript::default();
    for n in branch {
        t.upsert(n.item());
    }
    drop(t);
    *live.leaf.locked() = branch.last().map(|n| n.id.clone());
    live.epoch.send_modify(|e| *e += 1);
    live.bump();
}

/// The nodes of `branch` after `seen` (all of them when `seen` is not on it).
fn unseen_since(branch: &[ChatNode], seen: Option<&str>) -> Vec<ChatNode> {
    match seen.and_then(|s| branch.iter().position(|n| n.id == s)) {
        Some(i) => branch[i + 1..].to_vec(),
        None if seen.is_none() => branch.to_vec(),
        None => branch.to_vec(),
    }
}

fn with_seed(unseen: &[ChatNode], text: &str, tokens: u32) -> String {
    match chats::seed_block(unseen, tokens as usize) {
        Some((block, _)) => format!("{block}{text}"),
        None => text.to_owned(),
    }
}

/// Gemini's own folders for the chat directory `dir`: the ones under `<gemini dir>/tmp` and
/// `history` whose `.project_root` names it (its session files are in them).
fn remove_gemini_copies(gemini: &std::path::Path, dir: &std::path::Path) {
    let want = [Some(dir.to_path_buf()), dir.canonicalize().ok()];
    for base in ["tmp", "history"] {
        let Ok(rd) = std::fs::read_dir(gemini.join(base)) else { continue };
        for e in rd.flatten() {
            let owner = std::fs::read_to_string(e.path().join(".project_root")).unwrap_or_default();
            let owner = PathBuf::from(owner.trim());
            if !owner.as_os_str().is_empty() && want.iter().flatten().any(|w| *w == owner) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// The MCP servers `names`, as Claude's own config defines them (`<config dir>/.claude.json`,
/// else `~/.claude.json`: `mcpServers`), and the names it doesn't have.
fn claude_mcp_defs(
    plan: &ptyro_agents::profile::Plan,
    names: &[String],
) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
    if names.is_empty() {
        return (serde_json::Map::new(), Vec::new());
    }
    let file = plan
        .set
        .iter()
        .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
        .map(|(_, d)| PathBuf::from(d).join(".claude.json"))
        .unwrap_or_else(|| ptyro_core::config::expand_home("~/.claude.json"));
    let all: serde_json::Value = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut found = serde_json::Map::new();
    let mut missing = Vec::new();
    for n in names {
        match all["mcpServers"].get(n) {
            Some(def) => {
                found.insert(n.clone(), def.clone());
            }
            None => missing.push(n.clone()),
        }
    }
    (found, missing)
}

/// The folders a folder-limited Codex chat must still read to start (§20.8): the profile's
/// `CODEX_HOME` and the folder of the `codex` it runs (`program`: found, as `agent_program`;
/// with the `node` that runs it when it is a script).
fn codex_needs(plan: &ptyro_agents::profile::Plan, program: &[String]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = plan.set.iter().find(|(k, _)| k == "CODEX_HOME") {
        v.push(PathBuf::from(&h.1));
    }
    for prog in program.iter().filter(|p| p.contains('/')) {
        let f = PathBuf::from(prog);
        // Both where it is linked from and where it really is (nvm, bun, Homebrew).
        for p in [Some(f.clone()), f.canonicalize().ok()].into_iter().flatten() {
            if let Some(d) = p.parent() {
                v.push(d.to_owned());
            }
        }
    }
    v
}

/// A preset as launch arguments and driver fields, per protocol (§22.5, CHAT0 §20.8).
/// What a chat's history tools may see.
#[derive(Debug, Clone, Default)]
pub(crate) struct HistoryScope {
    profile: Option<String>,
    /// A workspace id.
    workspace: Option<String>,
    /// The asking chat: never its own hit.
    itself: Option<String>,
}

impl HistoryScope {
    /// A raw row (its workspace an id).
    fn sees(&self, c: &ChatInfo) -> bool {
        !c.incognito
            && self.profile.as_ref().is_none_or(|p| *p == c.profile)
            && self.workspace.as_ref().is_none_or(|w| *w == c.workspace)
    }
}

/// Several machines' hits, each list best first, taken one from each in turn (bm25 isn't
/// comparable across machines), each round newest first.
fn interleave(lists: Vec<Vec<ChatHit>>) -> Vec<ChatHit> {
    let mut iters: Vec<std::vec::IntoIter<ChatHit>> =
        lists.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    loop {
        let mut round: Vec<ChatHit> = iters.iter_mut().filter_map(Iterator::next).collect();
        if round.is_empty() {
            return out;
        }
        round.sort_by_key(|h| std::cmp::Reverse(h.at_ms));
        out.extend(round);
    }
}

/// What a chat's launch adds to its preset.
#[derive(Debug, Default)]
struct ChatLaunch {
    /// Nothing kept by the agent either, where it can say so (SPEC §22.9).
    incognito: bool,
    /// `ptyro mcp --chat`, as an MCP server entry: the chat history tools (SPEC §22.10).
    history: Option<serde_json::Value>,
    /// Instead of the chat system prompt (`chat find`'s).
    system: Option<&'static str>,
    /// The chat's own folders: read with the preset's `dirs` when the preset reads (CHAT4).
    folders: Vec<String>,
    /// The agent's adapter (`gemini`: the policy flags; another ACP agent gets none).
    adapter: String,
    /// The preset's MCP servers as Claude's own config defines them, by name.
    claude_mcp: serde_json::Map<String, serde_json::Value>,
}

/// The chat history tools, by their names in Claude.
const HISTORY_TOOLS: [&str; 3] =
    ["mcp__ptyro__chat_search", "mcp__ptyro__chat_read", "mcp__ptyro__chat_list"];

/// `chat find`'s engine: no tools but the history ones, and this prompt.
const FIND_PRESET: &str = "find";
const FIND_PROMPT: &str = "You answer questions about the user's past chats, and nothing else. \
    Use chat_search (try several phrasings and words), then chat_read around the best hits, \
    before you answer. Answer briefly, and cite every fact with the message it comes from, \
    written exactly as c:HANDLE/NODE (the chat's handle and the node id the tools give). If the \
    chats don't say, say that.";

/// Whether an agent can't enforce `p` by itself, or only partly (SPEC §22.5's table): ptyro's
/// answer to each permission request stands behind it, and the header says `best effort`.
fn best_effort(adapter: &str, p: &ChatTools) -> bool {
    match adapter {
        "claude" => false,
        // Files outside $HOME stay readable to a folder-limited Codex (CHAT0).
        "codex" => p.read,
        // Gemini's policy and OpenCode's ruleset are read from their source, not yet run (a
        // turn needs a login); any other agent is told nothing at all.
        _ => true,
    }
}

/// The folders a chat was given, as full paths that exist on this machine.
fn chat_folders(folders: &[String]) -> Result<Vec<String>, Error> {
    let mut out: Vec<String> = Vec::new();
    for f in folders.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        let p = ptyro_core::config::expand_home(f);
        if !p.is_absolute() {
            return Err(Error::invalid(format!(
                "{f:?}: give the folder's full path (~/notes works)"
            )));
        }
        let real = p
            .canonicalize()
            .ok()
            .filter(|r| r.is_dir())
            .ok_or_else(|| Error::invalid(format!("{f}: no such folder on this machine")))?;
        let s = real.to_string_lossy().into_owned();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

/// An MCP server entry (`{type, command, args, env: {…}}`) in ACP's shape: `{name, command,
/// args, env: [{name, value}]}` (`session/new`'s `mcpServers`).
fn acp_server(name: &str, server: &serde_json::Value) -> serde_json::Value {
    let env: Vec<serde_json::Value> = server["env"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| serde_json::json!({ "name": k, "value": v.as_str().unwrap_or_default() }))
        .collect();
    serde_json::json!({
        "name": name,
        "command": server["command"],
        "args": server["args"].as_array().cloned().unwrap_or_default(),
        "env": env,
    })
}

/// Write `text` to `file`, readable by its owner alone (made so before anything is in it).
fn write_private(file: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)?;
    // A file from before keeps its old mode: made private either way.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(text.as_bytes())
}

fn preset_launch(
    protocol: &str,
    p: &ChatTools,
    dir: &std::path::Path,
    codex_mcp: Vec<String>,
    codex_needs: &[PathBuf],
    extra: &ChatLaunch,
) -> (Vec<String>, Option<serde_json::Value>) {
    use serde_json::json;
    let mut dirs: Vec<String> = p
        .dirs
        .iter()
        .map(|d| ptyro_core::config::expand_home(d).to_string_lossy().into_owned())
        .collect();
    for f in &extra.folders {
        if !dirs.contains(f) {
            dirs.push(f.clone());
        }
    }
    match protocol {
        "stream-json" => {
            let mut tools: Vec<String> = Vec::new();
            let mut allowed: Vec<String> = Vec::new();
            if p.web {
                tools.extend(["WebSearch".into(), "WebFetch".into()]);
                allowed.extend(["WebSearch".into(), "WebFetch".into()]);
            }
            if p.read {
                // No allow for the read tools: a bare allow reads anywhere (CHAT0).
                tools.extend(["Read".into(), "Glob".into(), "Grep".into()]);
            }
            if p.shell {
                tools.push("Bash".into());
                allowed.push("Bash".into());
            }
            for a in &p.allow {
                tools.push(a.clone());
                allowed.push(a.clone());
            }
            if extra.history.is_some() {
                allowed.extend(HISTORY_TOOLS.map(str::to_owned));
            }
            // Every tool of a server the preset names (`mcp__<server>`).
            allowed.extend(extra.claude_mcp.keys().map(|n| format!("mcp__{n}")));
            let mut v = vec!["--tools".to_owned(), tools.join(",")];
            if !allowed.is_empty() {
                v.extend(["--allowedTools".into(), allowed.join(",")]);
            }
            if p.read {
                for d in &dirs {
                    v.extend(["--add-dir".into(), d.clone()]);
                }
            }
            v.push("--strict-mcp-config".into());
            let mut servers = extra.claude_mcp.clone();
            if let Some(h) = &extra.history {
                servers.insert("ptyro".into(), h.clone());
            }
            if servers.is_empty() {
                v.push("--safe-mode".into());
            } else {
                // `--safe-mode` drops even an explicit MCP server (verified, Claude 2.1.314):
                // with MCP servers, the same customizations are kept out by naming no setting
                // sources and no slash commands (CLAUDE.md, hooks, skills, plugins). The servers
                // go in a file only its owner reads: a server's definition can carry its
                // tokens, and a command line is anyone's to see (`ps`, a session's argv).
                let file = dir.join("mcp.json");
                let _ = write_private(&file, &json!({ "mcpServers": servers }).to_string());
                v.extend([
                    "--setting-sources".into(),
                    String::new(),
                    "--disable-slash-commands".into(),
                    "--mcp-config".into(),
                    file.to_string_lossy().into_owned(),
                ]);
            }
            if extra.incognito {
                v.push("--no-session-persistence".into());
            }
            v.extend(["--system-prompt".into(), extra.system.unwrap_or(SYSTEM_PROMPT).into()]);
            (v, None)
        }
        "app-server" => {
            // Every server of the user's is switched off by name, but the ones the preset keeps.
            let mcp: serde_json::Map<String, serde_json::Value> = codex_mcp
                .into_iter()
                .filter(|n| !p.mcp.contains(n))
                .map(|n| (n, json!({ "enabled": false })))
                .collect();
            let mut config = json!({
                "web_search": if p.web { "live" } else { "disabled" },
                "features": { "shell_tool": p.read || p.shell, "unified_exec": p.shell, "hooks": false },
                "mcp_servers": mcp,
            });
            if let Some(server) = &extra.history {
                config["mcp_servers"]["ptyro"] = server.clone();
                if let Some(o) = config["mcp_servers"]["ptyro"].as_object_mut() {
                    o.remove("type");
                }
            }
            let mut params =
                json!({ "developerInstructions": extra.system.unwrap_or(SYSTEM_PROMPT) });
            if extra.incognito {
                params["ephemeral"] = json!(true);
            }
            // A `read` chat always gets the folder-limited profile, folders or none: the
            // read-only sandbox alone still reads every file (CHAT0).
            if p.read {
                // A folder-limited read: a profile that denies $HOME and re-allows the folders and
                // what Codex needs to start; no sandbox parameter (it would win over it).
                let mut fs = serde_json::Map::new();
                if let Some(home) = std::env::var_os("HOME") {
                    let home = PathBuf::from(home);
                    fs.insert(home.to_string_lossy().into_owned(), json!("deny"));
                    for d in [home.join(".codex"), home.join(".local/bin")] {
                        fs.insert(d.to_string_lossy().into_owned(), json!("read"));
                    }
                }
                // What Codex needs to start: its profile's `CODEX_HOME`, its binary's folder.
                for d in codex_needs {
                    fs.insert(d.to_string_lossy().into_owned(), json!("read"));
                }
                fs.insert(dir.to_string_lossy().into_owned(), json!("write"));
                for d in &dirs {
                    fs.insert(d.clone(), json!("read"));
                }
                config["default_permissions"] = json!("ptyro-chat");
                config["permissions"] =
                    json!({ "ptyro-chat": { "extends": ":read-only", "filesystem": fs } });
                params["sandbox"] = serde_json::Value::Null;
            } else if p.shell {
                // Commands run in Codex's sandbox: they write only the chat's scratch folder.
                params["sandbox"] = json!("workspace-write");
            } else {
                params["sandbox"] = json!("read-only");
            }
            params["config"] = config;
            (Vec::new(), Some(params))
        }
        "acp" => {
            // MCP servers come in `session/new` (and `session/load`), as ACP says.
            let servers: Vec<serde_json::Value> =
                extra.history.iter().map(|h| acp_server("ptyro", h)).collect();
            let params = (!servers.is_empty()).then(|| json!({ "mcpServers": servers }));
            if extra.adapter != "gemini" {
                return (Vec::new(), params);
            }
            // Gemini's policy engine (read from its source, 0.66.0): deny everything, then allow
            // what the preset names. Loaded as an admin policy, the tier above the user's own
            // rules and "always allow" answers, so nothing of theirs widens a chat; and as the
            // user policy too, which replaces theirs, for when a system policy makes Gemini
            // ignore `--admin-policy`.
            let mut rules =
                String::from("[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 900\n");
            let mut allow: Vec<String> = Vec::new();
            if p.web {
                allow.extend(["google_web_search".into(), "web_fetch".into()]);
            }
            if p.read {
                // Its file tools read only its workspace: the scratch folder and the
                // `--include-directories` below.
                allow.extend(
                    ["read_file", "read_many_files", "glob", "grep_search", "list_directory"]
                        .map(str::to_owned),
                );
            }
            if p.shell {
                allow.push("run_shell_command".into());
            }
            allow.extend(p.allow.iter().cloned());
            // MCP tools are `mcp_<server>_<tool>`.
            let mut names: Vec<String> = Vec::new();
            if extra.history.is_some() {
                names.push("ptyro".into());
            }
            names.extend(p.mcp.iter().cloned());
            allow.extend(names.iter().map(|n| format!("mcp_{n}_*")));
            for a in allow {
                rules.push_str(&format!(
                    "\n[[rule]]\ntoolName = {}\ndecision = \"allow\"\npriority = 950\n",
                    serde_json::Value::String(a)
                ));
            }
            let file = dir.join("policy.toml");
            let _ = std::fs::write(&file, rules);
            let file = file.to_string_lossy().into_owned();
            let mut v = vec![
                "--admin-policy".into(),
                file.clone(),
                "--policy".into(),
                file,
                // No extension of the user's, and only the MCP servers named here (an empty
                // name is Gemini's "none").
                "--extensions".into(),
                "none".into(),
                "--allowed-mcp-server-names".into(),
                names.join(","),
            ];
            if p.read {
                for d in &dirs {
                    v.extend(["--include-directories".into(), d.clone()]);
                }
            }
            (v, params)
        }
        "opencode" => {
            let mut rules = vec![json!({ "permission": "*", "pattern": "*", "action": "deny" })];
            if p.web {
                for t in ["websearch", "webfetch"] {
                    rules.push(json!({ "permission": t, "pattern": "*", "action": "allow" }));
                }
            }
            if p.read {
                for d in &dirs {
                    for t in ["read", "glob", "grep"] {
                        rules.push(json!({ "permission": t, "pattern": format!("{d}/**"), "action": "allow" }));
                    }
                }
            }
            if p.shell {
                rules.push(json!({ "permission": "bash", "pattern": "*", "action": "allow" }));
            }
            // OpenCode names a server's tools `<server>_<tool>` (its docs; not run).
            for m in &p.mcp {
                rules.push(
                    json!({ "permission": format!("{m}_*"), "pattern": "*", "action": "allow" }),
                );
            }
            for a in &p.allow {
                rules.push(json!({ "permission": a, "pattern": "*", "action": "allow" }));
            }
            (Vec::new(), Some(json!({ "permission": rules })))
        }
        _ => (Vec::new(), None),
    }
}

/// The shown branch as Markdown (`ptyro chat export`).
pub(crate) fn markdown(info: &ChatInfo, nodes: &[ChatNode]) -> String {
    let mut out = format!("# {}\n\n", info.label());
    out.push_str(&format!("*{}@{} · {}*\n\n", info.agent, info.profile, info.addr()));
    for n in nodes {
        match &n.body {
            ChatBody::User { text } => out.push_str(&format!("**You:** {}\n\n", text.trim())),
            ChatBody::Assistant { text, .. } => out.push_str(&format!("{}\n\n", text.trim())),
            ChatBody::Tool { name, title, .. } => out.push_str(&format!("> *{name}* {title}\n\n")),
            ChatBody::Notice { text, .. } => out.push_str(&format!("> {text}\n\n")),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, body: ChatBody) -> ChatNode {
        ChatNode { id: id.into(), parent: None, at_ms: 0, body }
    }

    #[test]
    fn presets_per_agent() {
        let dir = std::path::Path::new("/d/chat/c1");
        let chat = ChatTools { web: true, ..ChatTools::default() };
        let (a, p) = preset_launch("stream-json", &chat, dir, vec![], &[], &ChatLaunch::default());
        assert_eq!(
            &a[..4],
            ["--tools", "WebSearch,WebFetch", "--allowedTools", "WebSearch,WebFetch"]
        );
        assert!(
            a.contains(&"--safe-mode".into())
                && a.contains(&"--strict-mcp-config".into())
                && p.is_none()
        );
        let none = ChatTools::default();
        let (a, _) = preset_launch("stream-json", &none, dir, vec![], &[], &ChatLaunch::default());
        assert_eq!(&a[..2], ["--tools", ""]);
        assert!(!a.contains(&"--allowedTools".into()));
        let read = ChatTools {
            web: true,
            read: true,
            dirs: vec!["/notes".into()],
            ..ChatTools::default()
        };
        let (a, _) = preset_launch("stream-json", &read, dir, vec![], &[], &ChatLaunch::default());
        assert!(a.windows(2).any(|w| w == ["--add-dir", "/notes"]));
        assert_eq!(a[3], "WebSearch,WebFetch", "the read tools are not allowed outright");
        let (_, p) = preset_launch(
            "app-server",
            &chat,
            dir,
            vec!["GitHub".into()],
            &[],
            &ChatLaunch::default(),
        );
        let p = p.unwrap();
        assert_eq!(p["sandbox"], "read-only");
        assert_eq!(p["config"]["web_search"], "live");
        assert_eq!(p["config"]["features"]["shell_tool"], false);
        assert_eq!(p["config"]["mcp_servers"]["GitHub"]["enabled"], false);
        let (_, p) = preset_launch(
            "app-server",
            &read,
            dir,
            vec![],
            &["/opt/codex/bin".into()],
            &ChatLaunch::default(),
        );
        let p = p.unwrap();
        assert!(
            p["sandbox"].is_null()
                && p["config"]["permissions"]["ptyro-chat"]["filesystem"]["/notes"] == "read"
        );
        assert_eq!(
            p["config"]["permissions"]["ptyro-chat"]["filesystem"]["/opt/codex/bin"],
            "read"
        );
        // `read` with no folders: still the limited profile, never the read-only sandbox (which
        // reads every file).
        let bare = ChatTools { web: true, read: true, ..ChatTools::default() };
        let (_, p) = preset_launch("app-server", &bare, dir, vec![], &[], &ChatLaunch::default());
        assert!(p.unwrap()["sandbox"].is_null());
        let (_, p) = preset_launch("opencode", &chat, dir, vec![], &[], &ChatLaunch::default());
        let rules = p.unwrap()["permission"].as_array().unwrap().clone();
        assert_eq!(rules[0]["action"], "deny");
        assert!(rules.iter().any(|r| r["permission"] == "websearch" && r["action"] == "allow"));
    }

    #[test]
    fn shell_mcp_and_folders_per_agent() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        let shell = ChatTools {
            web: true,
            read: true,
            shell: true,
            mcp: vec!["github".into()],
            ..ChatTools::default()
        };
        let mut gh = serde_json::Map::new();
        gh.insert("github".into(), serde_json::json!({ "type": "stdio", "command": "gh-mcp" }));
        let extra = ChatLaunch {
            folders: vec!["/notes".into()],
            claude_mcp: gh,
            adapter: "claude".into(),
            ..ChatLaunch::default()
        };
        // Claude: Bash among the tools and allowed; the named server in --mcp-config (so no
        // --safe-mode, which would drop it); the chat's folder added.
        let (a, _) = preset_launch("stream-json", &shell, dir, vec![], &[], &extra);
        assert_eq!(a[1], "WebSearch,WebFetch,Read,Glob,Grep,Bash");
        assert_eq!(a[3], "WebSearch,WebFetch,Bash,mcp__github");
        assert!(a.windows(2).any(|w| w == ["--add-dir", "/notes"]));
        assert!(!a.contains(&"--safe-mode".into()));
        // Its definition (a server's tokens, maybe) in a file only its owner reads, never on
        // the command line.
        let cfg = &a[a.iter().position(|x| x == "--mcp-config").unwrap() + 1];
        assert_eq!(std::path::Path::new(cfg), dir.join("mcp.json"));
        assert!(std::fs::read_to_string(cfg).unwrap().contains("gh-mcp"));
        assert!(!a.iter().any(|x| x.contains("gh-mcp")));
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cfg).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Codex: the shell on, the named server kept, the others off.
        let (_, p) = preset_launch(
            "app-server",
            &shell,
            dir,
            vec!["github".into(), "jira".into()],
            &[],
            &extra,
        );
        let p = p.unwrap();
        assert_eq!(p["config"]["features"]["unified_exec"], true);
        assert!(p["config"]["mcp_servers"].get("github").is_none());
        assert_eq!(p["config"]["mcp_servers"]["jira"]["enabled"], false);
        assert_eq!(p["config"]["permissions"]["ptyro-chat"]["filesystem"]["/notes"], "read");
        let only_shell = ChatTools { shell: true, ..ChatTools::default() };
        let (_, p) = preset_launch("app-server", &only_shell, dir, vec![], &[], &extra);
        assert_eq!(p.unwrap()["sandbox"], "workspace-write");
        // Gemini: an admin policy (and the user one), no extensions, only the named servers,
        // the folders as its workspace, the history server in `session/new`.
        let history = serde_json::json!({ "type": "stdio", "command": "/bin/ptyro", "args": ["mcp", "--chat"], "env": { "PTYRO_CHAT_ID": "c1" } });
        let gem = ChatLaunch {
            folders: vec!["/notes".into()],
            adapter: "gemini".into(),
            history: Some(history),
            ..ChatLaunch::default()
        };
        let (a, p) = preset_launch("acp", &shell, dir, vec![], &[], &gem);
        let policy = dir.join("policy.toml").to_string_lossy().into_owned();
        assert!(a.windows(2).any(|w| w == ["--admin-policy".to_owned(), policy.clone()]));
        assert!(a.windows(2).any(|w| w == ["--policy".to_owned(), policy.clone()]));
        assert!(a.windows(2).any(|w| w == ["--extensions", "none"]));
        assert!(a.windows(2).any(|w| w == ["--allowed-mcp-server-names", "ptyro,github"]));
        assert!(a.windows(2).any(|w| w == ["--include-directories", "/notes"]));
        let rules = std::fs::read_to_string(&policy).unwrap();
        for tool in
            ["run_shell_command", "read_file", "google_web_search", "mcp_ptyro_*", "mcp_github_*"]
        {
            assert!(rules.contains(&format!("toolName = \"{tool}\"")), "{tool}: {rules}");
        }
        let p = p.unwrap();
        assert_eq!(p["mcpServers"][0]["name"], "ptyro");
        assert_eq!(
            p["mcpServers"][0]["env"][0],
            serde_json::json!({ "name": "PTYRO_CHAT_ID", "value": "c1" })
        );
        // The default chat on Gemini: web only, and not one MCP server (Gemini's "none").
        let chat = ChatTools { web: true, ..ChatTools::default() };
        let plain = ChatLaunch { adapter: "gemini".into(), ..ChatLaunch::default() };
        let (a, p) = preset_launch("acp", &chat, dir, vec![], &[], &plain);
        assert!(a.windows(2).any(|w| w == ["--allowed-mcp-server-names", ""]) && p.is_none());
        assert!(!a.contains(&"--include-directories".into()));
        let rules = std::fs::read_to_string(&policy).unwrap();
        assert!(!rules.contains("read_file") && rules.contains("google_web_search"));
        // Another ACP agent: told nothing it wouldn't understand.
        let other = ChatLaunch { adapter: "generic".into(), ..ChatLaunch::default() };
        assert!(preset_launch("acp", &shell, dir, vec![], &[], &other).0.is_empty());
        // OpenCode: bash allowed, the named server's tools.
        let (_, p) = preset_launch("opencode", &shell, dir, vec![], &[], &extra);
        let rules = p.unwrap()["permission"].as_array().unwrap().clone();
        assert!(rules.iter().any(|r| r["permission"] == "bash" && r["action"] == "allow"));
        assert!(rules.iter().any(|r| r["permission"] == "github_*"));
        assert!(rules.iter().any(|r| r["pattern"] == "/notes/**"));
        assert!(best_effort("gemini", &chat) && !best_effort("claude", &shell));
        assert!(best_effort("codex", &shell) && !best_effort("codex", &chat));
    }

    #[test]
    fn what_an_engine_hasnt_seen() {
        let b = vec![
            node("1", ChatBody::User { text: "a".into() }),
            node("2", ChatBody::Assistant { text: "b".into(), streaming: false }),
            node("3", ChatBody::User { text: "c".into() }),
        ];
        assert_eq!(unseen_since(&b, Some("2")).len(), 1);
        assert_eq!(unseen_since(&b, Some("3")).len(), 0);
        assert_eq!(unseen_since(&b, None).len(), 3);
        assert_eq!(with_seed(&[], "hi", 100), "hi");
        assert!(with_seed(&b[2..], "hi", 100).starts_with(chats::SEED_HEAD));
        let info = ChatInfo {
            id: "01x".into(),
            handle: "7f3k".into(),
            workspace: "main".into(),
            machine: None,
            title: Some("Lifetimes".into()),
            title_src: "auto".into(),
            agent: "claude".into(),
            profile: "default".into(),
            tools: "chat".into(),
            pinned: false,
            archived: false,
            incognito: false,
            forked: false,
            side_of: None,
            leaf: None,
            nodes: 3,
            unread: 0,
            created_ms: 0,
            last_ms: 0,
            cost_micro_usd: 0,
            status: None,
            attention: None,
            engine: None,
            folders: Vec::new(),
            best_effort: false,
            shell: false,
            comparing: None,
            source: None,
            source_dir: None,
        };
        let md = markdown(&info, &b);
        assert!(
            md.starts_with("# Lifetimes") && md.contains("**You:** a") && md.contains("c:7f3k")
        );
    }
}
