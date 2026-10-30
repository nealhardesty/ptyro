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
use ptyro_core::chats::{self, ChatInfo, ChatNode};
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
        Self { db, live: Mutex::new(HashMap::new()), sending: Mutex::new(HashMap::new()) }
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
            })
        }))
    }

    pub(crate) fn get_live(&self, id: &str) -> Option<Arc<Live>> {
        self.live.locked().get(id).cloned()
    }

    /// Forget a live chat nobody follows and no engine serves.
    fn settle(&self, id: &str) {
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
    pub(crate) fn start_chats(self: &Arc<Self>) {
        let Ok(db) = self.chats.db().cloned() else {
            if let Err(e) = self.chats.db() {
                self.warn(e.message);
            }
            return;
        };
        let me = Arc::clone(self);
        tokio::spawn(async move {
            // Engines that crossed a live upgrade have no follower in this image: stop them (a
            // cut-off answer is closed below; the next message resumes the conversation).
            for s in me.sessions() {
                let chat = s.meta.locked().place.chat.is_some();
                if chat {
                    s.hibernating.store(true, Ordering::Relaxed);
                    me.stop_session(&s).await;
                    s.save(None);
                    me.forget_session(s.id);
                }
            }
            match db.close_interrupted().await {
                Ok(c) if !c.is_empty() => tracing::info!("{} chat answer(s) were cut off", c.len()),
                Ok(_) => {}
                Err(e) => tracing::warn!("chat.db: {e}"),
            }
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

    // ── addressing ────────────────────────────────────────────────────────────────────────

    /// A chat by address: a handle (`7f3k`, `c:7f3k`), a full id, `@last`, or a title
    /// substring that matches one chat.
    pub(crate) async fn find_chat(&self, addr: &str) -> Result<ChatInfo, Error> {
        let db = self.chat_db()?;
        let all = db.list(ListFilter::default()).await.map_err(store_err)?;
        let mut archived = db
            .list(ListFilter { archived: true, ..ListFilter::default() })
            .await
            .map_err(store_err)?;
        let mut all = all;
        all.append(&mut archived);
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
        if let Some(e) = self.chats.get_live(&c.id).and_then(|l| l.engine()) {
            let info = self.info(&e.session);
            c.status = info.status.map(|s| s.status);
            c.attention = info.attention;
            c.engine = Some(info.id.full());
        }
        c
    }

    async fn chat_info_now(&self, id: &str) -> Result<ChatInfo, Error> {
        let db = self.chat_db()?;
        let c = db
            .get(id.to_owned())
            .await
            .map_err(store_err)?
            .ok_or_else(|| Error::missing("chat", id))?;
        let ids = db.ids().await.map_err(store_err)?;
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
        match self.chat_info_now(id).await {
            Ok(info) => {
                let _ = self.events.send(DomainEvent::ChatUpdated { info });
            }
            Err(e) => tracing::debug!("chat {id}: {}", e.message),
        }
    }

    // ── requests ──────────────────────────────────────────────────────────────────────────

    pub(crate) async fn handle_chat(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::ChatNew { workspace, agent, preset, title, prompt } => {
                let info = self.chat_new(workspace, agent, preset, title).await?;
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
                        workspace: ws,
                        pinned_only: pinned,
                        archived,
                        limit: limit.unwrap_or(0) as usize,
                    })
                    .await
                    .map_err(store_err)?;
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
                let info = self.find_chat(&chat).await?;
                let db = self.chat_db()?;
                let nodes = if all {
                    db.nodes(info.id.clone()).await
                } else {
                    db.branch(info.id.clone(), None).await
                }
                .map_err(store_err)?;
                Ok(Reply::ChatTranscript { info, nodes })
            }
            Request::ChatSend { chat, text } => {
                let info = self.find_chat(&chat).await?;
                if text.trim().is_empty() {
                    return Err(Error::invalid("nothing to send"));
                }
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
            Request::ChatUpdate { chat, title, pinned, archived, agent, preset } => {
                let info = self.find_chat(&chat).await?;
                let db = self.chat_db()?;
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
                self.chat_changed(&info.id).await;
                Ok(Reply::Chat { info: self.chat_info_now(&info.id).await? })
            }
            Request::ChatDelete { chat } => {
                let info = self.find_chat(&chat).await?;
                self.stop_chat_engine(&info.id).await;
                self.chat_db()?.delete(info.id.clone()).await.map_err(store_err)?;
                self.remove_chat_files(&info).await;
                self.chats.live.locked().remove(&info.id);
                let _ = self.events.send(DomainEvent::ChatRemoved { id: info.id, machine: None });
                Ok(Reply::Done)
            }
            Request::ChatExport { chat, format } => {
                let info = self.find_chat(&chat).await?;
                let nodes =
                    self.chat_db()?.branch(info.id.clone(), None).await.map_err(store_err)?;
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
                            "No agent found to chat with: install Claude Code, Codex, Gemini CLI or OpenCode (`ptyro doctor` checks them)",
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
    ) -> Result<ChatInfo, Error> {
        let db = self.chat_db()?;
        let ws = match workspace.as_deref() {
            Some(w) => self.find_workspace(w)?,
            None => self.find_workspace(MAIN_WORKSPACE)?,
        };
        let (agent, profile) = self.chat_agent(agent.as_deref(), &ws.name)?;
        let config = self.config();
        let preset = preset.unwrap_or_else(|| config.chat.preset.clone());
        if config.chat.preset(&preset).is_none() {
            return Err(Error::invalid(format!(
                "{preset:?} isn't a preset ({})",
                config.chat.presets().join(", ")
            )));
        }
        let id = chats::new_id();
        let title = title.map(|t| chats::first_line_title(&t)).filter(|t| !t.is_empty());
        db.create(NewChat {
            id: id.clone(),
            workspace: ws.id,
            title_src: if title.is_some() { "user" } else { "auto" }.into(),
            title,
            agent,
            profile,
            tools: preset,
            forked_from: None,
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
        let db = self.chat_db()?;
        let info = db
            .get(id.to_owned())
            .await
            .map_err(store_err)?
            .ok_or_else(|| Error::missing("chat", id))?;
        let live = self.chats.live(id);
        // One answer at a time: a message sent mid-answer would interleave with it (and be
        // answered after it anyway). The client says so and keeps the text.
        if let Some(e) = live.engine() {
            match e.session.status() {
                Some(AgentStatus::Working | AgentStatus::Starting) => {
                    return Err(Error::new(
                        ErrorCode::Busy,
                        format!(
                            "c:{} is still answering: wait for it, or stop it (`ptyro chat stop`, Ctrl-C in the chat)",
                            chats::handle(&info.id, [info.id.as_str()])
                        ),
                    ));
                }
                Some(AgentStatus::Blocked) => {
                    return Err(Error::new(
                        ErrorCode::Busy,
                        "it is waiting for your answer to its permission request first",
                    ));
                }
                _ => {}
            }
        }
        // The record first: the message is kept whatever the engine does.
        let branch = db.branch(id.to_owned(), None).await.map_err(store_err)?;
        let parent = info.leaf.clone().or_else(|| branch.last().map(|n| n.id.clone()));
        let user = ChatNode {
            id: chats::new_id(),
            parent: parent.clone(),
            at_ms: now_ms(),
            body: ChatBody::User { text: text.clone() },
        };
        db.put_node_now(id.to_owned(), user.clone(), None, false).await.map_err(store_err)?;
        if live.items.locked().items.is_empty() && !branch.is_empty() {
            load_branch(&live, &branch);
        }
        live.items.locked().upsert(user.item());
        *live.leaf.locked() = Some(user.id.clone());
        live.bump();
        if let Some(e) = live.engine() {
            // Carrying on: whatever it hasn't seen (none, usually) is folded into the prompt.
            let conv = db.current_conv(id.to_owned()).await.map_err(store_err)?;
            let seen = conv.as_ref().and_then(|c| c.node.clone());
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

    /// A notice in the chat (never sent to an engine).
    async fn chat_notice(&self, live: &Arc<Live>, id: &str, level: NoticeLevel, text: &str) {
        let parent = live.leaf.locked().clone();
        let n = ChatNode {
            id: chats::new_id(),
            parent,
            at_ms: now_ms(),
            body: ChatBody::Notice { level, text: text.to_owned() },
        };
        if let Ok(db) = self.chat_db() {
            let _ = db.put_node_now(id.to_owned(), n.clone(), None, false).await;
        }
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
        let conv = db.current_conv(info.id.clone()).await.map_err(store_err)?;
        // Native continue: same agent and profile, a conversation id, and its last node on
        // this branch.
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
        let readable = codex_needs(&config, &info.agent, &plan, &self.base_env);
        let best_effort = adapter == "codex" && mcp.is_none();
        let (args, params) =
            preset_launch(&protocol, &preset, &dir, mcp.unwrap_or_default(), &readable);
        let handle =
            chats::handle(&info.id, db.ids().await.unwrap_or_default().iter().map(String::as_str));
        let ws = self.workspace_name(&info.workspace);
        let session = self
            .launch(Launch {
                id: SessionId::new(),
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
                }),
            })
            .await?;
        if let Some(how) = how {
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
        self.follow_engine(Arc::clone(live), session, conv_id, resume.is_some(), user.id.clone());
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
        let config = self.config();
        let argv = ptyro_agents::command(&config, "codex")?;
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
                        let entry = nodes.entry(it.id.clone()).or_insert_with(|| {
                            let parent = live.leaf.locked().clone();
                            let id = chats::new_id();
                            *live.leaf.locked() = Some(id.clone());
                            (id, parent, Instant::now() - FLUSH, None)
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
                            db.put_node(chat.clone(), node, Some(conv.clone()), index_tools);
                            entry.2 = Instant::now();
                            entry.3 = Some(it.body.clone());
                        }
                        if let ChatBody::TurnEnd { .. } = &it.body {
                            let leaf = live.leaf.locked().clone();
                            db.update_conv(conv.clone(), me.info(&session).conversation, leaf);
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
                    db.put_node(
                        chat.clone(),
                        ChatNode { id, parent, at_ms: now_ms(), body: b },
                        Some(conv.clone()),
                        index_tools,
                    );
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
            me.chat_changed(&chat).await;
            me.chats.settle(&chat);
        });
    }

    /// An answer is complete: unread when nobody is looking, a title for a new chat.
    async fn chat_turn_ended(self: &Arc<Self>, live: &Arc<Live>) {
        let Ok(db) = self.chat_db() else { return };
        let id = live.id.clone();
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
    /// history (so `claude --resume` doesn't list dead chats; CHAT0).
    async fn remove_chat_files(self: &Arc<Self>, info: &ChatInfo) {
        self.remove_chat_dirs(&info.id, Some(info)).await;
    }

    /// The same by id (`info`: its agent and profile, when known; else only the scratch
    /// directory and the default profile's Claude folder).
    async fn remove_chat_dirs(self: &Arc<Self>, id: &str, info: Option<&ChatInfo>) {
        let dir = scratch(self, id);
        let config = self.config();
        let (agent, profile) = info.map_or_else(
            || ("claude".to_owned(), ptyro_core::model::DEFAULT_PROFILE.to_owned()),
            |i| (i.agent.clone(), i.profile.clone()),
        );
        if ptyro_agents::adapter_name(&config, &agent) == "claude"
            && let Ok(plan) = self.profile_plan(&profile).await
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
        let live = self.chats.live(&info.id);
        if live.engine().is_none() {
            let branch = db.branch(info.id.clone(), None).await.map_err(store_err)?;
            load_branch(&live, &branch);
        }
        if info.unread > 0 {
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
        self.count_cap(
            &format!("chat-titles:{profile}"),
            config.chat.daily_cap,
            "[chat] daily_cap",
        )?;
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

/// The folders a folder-limited Codex chat must still read to start (§20.8): the profile's
/// `CODEX_HOME` and the folder of the `codex` it runs.
fn codex_needs(
    config: &ptyro_core::config::Config,
    agent: &str,
    plan: &ptyro_agents::profile::Plan,
    base_env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = plan.set.iter().find(|(k, _)| k == "CODEX_HOME") {
        v.push(PathBuf::from(&h.1));
    }
    if let Some(prog) = ptyro_agents::command(config, agent).and_then(|a| a.into_iter().next()) {
        let path =
            base_env.iter().find(|(k, _)| k == "PATH").map(|(_, p)| p.clone()).unwrap_or_default();
        let found = if prog.contains('/') {
            Some(PathBuf::from(&prog))
        } else {
            std::env::split_paths(&path).map(|d| d.join(&prog)).find(|p| p.is_file())
        };
        if let Some(f) = found {
            // Both where it is linked from and where it really is (nvm, bun, Homebrew).
            for p in [Some(f.clone()), f.canonicalize().ok()].into_iter().flatten() {
                if let Some(d) = p.parent() {
                    v.push(d.to_owned());
                }
            }
        }
    }
    v
}

/// A preset as launch arguments and driver fields, per protocol (§22.5, CHAT0 §20.8).
fn preset_launch(
    protocol: &str,
    p: &ChatTools,
    dir: &std::path::Path,
    codex_mcp: Vec<String>,
    codex_needs: &[PathBuf],
) -> (Vec<String>, Option<serde_json::Value>) {
    use serde_json::json;
    let dirs: Vec<String> = p
        .dirs
        .iter()
        .map(|d| ptyro_core::config::expand_home(d).to_string_lossy().into_owned())
        .collect();
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
            for a in &p.allow {
                tools.push(a.clone());
                allowed.push(a.clone());
            }
            let mut v = vec!["--tools".to_owned(), tools.join(",")];
            if !allowed.is_empty() {
                v.extend(["--allowedTools".into(), allowed.join(",")]);
            }
            if p.read {
                for d in &dirs {
                    v.extend(["--add-dir".into(), d.clone()]);
                }
            }
            v.extend([
                "--strict-mcp-config".into(),
                "--safe-mode".into(),
                "--system-prompt".into(),
                SYSTEM_PROMPT.into(),
            ]);
            (v, None)
        }
        "app-server" => {
            let mcp: serde_json::Map<String, serde_json::Value> =
                codex_mcp.into_iter().map(|n| (n, json!({ "enabled": false }))).collect();
            let mut config = json!({
                "web_search": if p.web { "live" } else { "disabled" },
                "features": { "shell_tool": p.read, "unified_exec": false, "hooks": false },
                "mcp_servers": mcp,
            });
            let mut params = json!({ "developerInstructions": SYSTEM_PROMPT });
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
            } else {
                params["sandbox"] = json!("read-only");
            }
            params["config"] = config;
            (Vec::new(), Some(params))
        }
        "acp" => {
            // Gemini's policy engine: deny everything, then allow what the preset names (§22.5,
            // read from its files; best effort until a login lets it be run).
            let mut rules =
                String::from("[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 900\n");
            let mut allow: Vec<String> = Vec::new();
            if p.web {
                allow.extend(["google_web_search".into(), "web_fetch".into()]);
            }
            if p.read {
                allow.extend([
                    "read_file".into(),
                    "glob".into(),
                    "grep_search".into(),
                    "list_directory".into(),
                ]);
            }
            allow.extend(p.allow.iter().cloned());
            for a in allow {
                rules.push_str(&format!(
                    "\n[[rule]]\ntoolName = {}\ndecision = \"allow\"\npriority = 950\n",
                    serde_json::Value::String(a)
                ));
            }
            let file = dir.join("policy.toml");
            let _ = std::fs::write(&file, rules);
            (vec!["--policy".into(), file.to_string_lossy().into_owned()], None)
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
        let (a, p) = preset_launch("stream-json", &chat, dir, vec![], &[]);
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
        let (a, _) = preset_launch("stream-json", &none, dir, vec![], &[]);
        assert_eq!(&a[..2], ["--tools", ""]);
        assert!(!a.contains(&"--allowedTools".into()));
        let read = ChatTools {
            web: true,
            read: true,
            dirs: vec!["/notes".into()],
            ..ChatTools::default()
        };
        let (a, _) = preset_launch("stream-json", &read, dir, vec![], &[]);
        assert!(a.windows(2).any(|w| w == ["--add-dir", "/notes"]));
        assert_eq!(a[3], "WebSearch,WebFetch", "the read tools are not allowed outright");
        let (_, p) = preset_launch("app-server", &chat, dir, vec!["GitHub".into()], &[]);
        let p = p.unwrap();
        assert_eq!(p["sandbox"], "read-only");
        assert_eq!(p["config"]["web_search"], "live");
        assert_eq!(p["config"]["features"]["shell_tool"], false);
        assert_eq!(p["config"]["mcp_servers"]["GitHub"]["enabled"], false);
        let (_, p) = preset_launch("app-server", &read, dir, vec![], &["/opt/codex/bin".into()]);
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
        let (_, p) = preset_launch("app-server", &bare, dir, vec![], &[]);
        assert!(p.unwrap()["sandbox"].is_null());
        let (_, p) = preset_launch("opencode", &chat, dir, vec![], &[]);
        let rules = p.unwrap()["permission"].as_array().unwrap().clone();
        assert_eq!(rules[0]["action"], "deny");
        assert!(rules.iter().any(|r| r["permission"] == "websearch" && r["action"] == "allow"));
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
            leaf: None,
            nodes: 3,
            unread: 0,
            created_ms: 0,
            last_ms: 0,
            cost_micro_usd: 0,
            status: None,
            attention: None,
            engine: None,
        };
        let md = markdown(&info, &b);
        assert!(
            md.starts_with("# Lifetimes") && md.contains("**You:** a") && md.contains("c:7f3k")
        );
    }
}
