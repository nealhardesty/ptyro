//! Ask several agents one question (CHAT5, SPEC §22.7, RESEARCH R-CHAT-10): every agent
//! answers the same message, and each answer is a branch under it (CHAT2's tree), labelled with
//! the agent that wrote it, so the navigator reads `‹ 2/3 codex ›`.
//!
//! The chat's own agent answers in its own conversation (it carries on natively, as any
//! message). Every other agent answers in a **helper**: an in-memory chat (as an incognito one is)
//! that nobody lists, finds or follows, seeded with the branch above the message. When it has
//! answered, its answer is copied into the chat under the message, attributed to its agent by a
//! conv row (ended: it is who wrote it, not a conversation to continue), and the helper goes with
//! its scratch folder and the agent's own copy. Picking an answer (`chat pick`) shows its branch
//! and switches the chat to its agent, whose next message is seeded with that branch.
//!
//! A helper never shows a permission card: what its preset doesn't allow is denied (as ptyro's
//! answer stands behind every preset), and the progress says so. The chat's own agent asks as it
//! always does. One answer at a time holds for the whole compare, and `ChatStop` stops them all.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, NoticeLevel};
use ptyro_core::chats::{self, ChatCompare, ChatInfo, ChatNode, CompareAgent};
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_store::chat::Conv;

use super::{Live, load_branch, store_err};
use crate::daemon::Daemon;

/// How long a helper may take to answer: one that never ends would keep the chat busy.
const HELPER_TIMEOUT: Duration = Duration::from_secs(600);

/// The live item that shows a compare's progress while it runs (not a node: it is replaced by a
/// notice with the outcome when every agent is in).
fn progress_id(chat: &str) -> String {
    format!("compare-{chat}")
}

/// A compare under way.
pub(crate) struct Compare {
    pub(super) state: Mutex<ChatCompare>,
    /// Its helpers' chat ids (the chat's own agent has none).
    helpers: Mutex<Vec<String>>,
    stopped: AtomicBool,
    /// The chat's own agent's label, when it answers too (its answer is the one shown).
    own: Option<String>,
}

impl Compare {
    fn set(&self, agent: &str, state: &str, note: &str) {
        let mut s = self.state.locked();
        if let Some(a) = s.agents.iter_mut().find(|a| a.agent == agent) {
            a.state = state.to_owned();
            a.note = note.to_owned();
        }
    }
}

/// How one agent's answer ended.
enum Outcome {
    Done,
    Stopped,
    Failed(String),
    /// Not done within `HELPER_TIMEOUT`: stopped, and what it said so far left out.
    TimedOut,
}

impl Daemon {
    /// Every installed agent that can chat, the chat's own first.
    fn compare_defaults(&self, info: &ChatInfo) -> Vec<String> {
        let config = self.config();
        let (agents, _) = self.agent_choices();
        let structured = |name: &str| {
            ptyro_agents::def(&config, name).protocol.is_some()
                || ptyro_drive::protocol_for(&ptyro_agents::adapter_name(&config, name)).is_some()
        };
        let mut out = vec![info.agent.clone()];
        for c in agents.iter().filter(|c| c.installed && structured(&c.name)) {
            if !out.contains(&c.name) {
                out.push(c.name.clone());
            }
        }
        out
    }

    /// Who answers a compare in a chat of `own` (its agent and profile), in workspace `ws`:
    /// each name resolved as a chat's agent is, a bare name under the chat's own profile, each
    /// agent and profile once (SPEC §22.7). A name under another profile is asked about first
    /// (`confirmed`): the branch would go to that profile's account, and that agent's history
    /// tools search that profile's chats.
    pub(super) fn compare_who(
        &self,
        own: (&str, &str),
        ws: &str,
        asked: &[String],
        confirmed: bool,
    ) -> Result<Vec<(String, String)>, Error> {
        let mut who: Vec<(String, String)> = Vec::new();
        for a in asked.iter().flat_map(|a| a.split(',')).map(str::trim).filter(|a| !a.is_empty()) {
            let name = match ptyro_agents::split_agent(a) {
                (n, None) => format!("{n}@{}", own.1),
                _ => a.to_owned(),
            };
            let (agent, profile) = self.chat_agent(Some(&name), ws)?;
            if !who.iter().any(|w| *w == (agent.clone(), profile.clone())) {
                who.push((agent, profile));
            }
        }
        let others: Vec<String> =
            who.iter().filter(|(_, p)| p != own.1).map(|(a, p)| chats::agent_label(a, p)).collect();
        if !others.is_empty() && !confirmed {
            return Err(Error::new(
                ErrorCode::NeedsConfirmation,
                format!(
                    "{} {} under another profile than this chat's ({}): the conversation would go to that account, and its history tools search that profile's chats. Ask anyway?",
                    others.join(", "),
                    if others.len() == 1 { "runs" } else { "run" },
                    own.1
                ),
            ));
        }
        Ok(who)
    }

    /// Ask `agents` the message `text` (a new one), or the last one again (SPEC §22.7).
    pub(super) async fn chat_compare(
        self: &Arc<Self>,
        info: &ChatInfo,
        agents: Vec<String>,
        text: Option<String>,
        confirmed: bool,
    ) -> Result<(), Error> {
        if info.incognito {
            return Err(Error::invalid(
                "an incognito chat keeps no branches, and each answer of a compare is one",
            ));
        }
        if info.source.is_some() {
            return Err(imported_refusal(info));
        }
        let text = text.filter(|t| !t.trim().is_empty());
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, info)?;
        let db = self.chat_db()?;
        let ws = self.workspace_name(&info.workspace);
        // Who: as asked, else every installed one; a bare name under the chat's profile.
        let asked = if agents.is_empty() { self.compare_defaults(info) } else { agents };
        let mut who = self.compare_who((&info.agent, &info.profile), &ws, &asked, confirmed)?;
        let branch = self.rec_branch(&info.id, None).await?;
        if live.items.locked().items.is_empty() && !branch.is_empty() {
            self.reload_branch(&live).await;
        }
        // The message: a new one under the shown branch, or the last of the person's again.
        let (user, before, fresh) = match &text {
            Some(t) => {
                let parent = info.leaf.clone().or_else(|| branch.last().map(|n| n.id.clone()));
                let user = ChatNode {
                    id: chats::new_id(),
                    parent,
                    at_ms: now_ms(),
                    body: ChatBody::User { text: t.clone() },
                };
                (user, branch.clone(), true)
            }
            None => {
                let i = branch
                    .iter()
                    .rposition(|n| matches!(n.body, ChatBody::User { .. }))
                    .ok_or_else(|| Error::invalid("nothing to compare: ask something first"))?;
                (branch[i].clone(), branch[..i].to_vec(), false)
            }
        };
        if !fresh {
            // Asked again: an agent that already answered it keeps its answer.
            let answered: Vec<String> = db
                .child_agents(info.id.clone(), user.id.clone())
                .await
                .map_err(store_err)?
                .into_iter()
                .map(|(_, a)| a)
                .collect();
            let all = who.len();
            who.retain(|(a, p)| !answered.contains(&chats::agent_label(a, p)));
            if who.is_empty() {
                return Err(Error::invalid(format!(
                    "{} answered it already: name others (--agents), or /retry asks one again",
                    if all == 1 { "that agent" } else { "every one of them" }
                )));
            }
        } else if who.len() < 2 {
            return Err(Error::invalid(
                "a compare needs two agents or more: name them (--agents claude,codex), or install another",
            ));
        }
        let own = who.iter().position(|(a, p)| *a == info.agent && *p == info.profile);
        let cmp = Arc::new(Compare {
            state: Mutex::new(ChatCompare {
                node: user.id.clone(),
                agents: who
                    .iter()
                    .map(|(a, p)| CompareAgent {
                        agent: chats::agent_label(a, p),
                        state: "asking".into(),
                        note: String::new(),
                    })
                    .collect(),
            }),
            helpers: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            own: own.map(|i| chats::agent_label(&who[i].0, &who[i].1)),
        });
        if fresh {
            self.rec_put_now(&info.id, user.clone()).await?;
            live.items.locked().upsert(user.item());
            *live.leaf.locked() = Some(user.id.clone());
        } else if own.is_some() {
            // The chat's own agent answers it again: its branch is shown, as a retry's is.
            self.stop_chat_engine(&info.id).await;
            db.set_leaf(info.id.clone(), user.id.clone()).await.map_err(store_err)?;
            self.reload_branch(&live).await;
            *live.leaf.locked() = Some(user.id.clone());
        } else {
            self.stop_chat_engine(&info.id).await;
        }
        self.chats.comparing.locked().insert(info.id.clone(), Arc::clone(&cmp));
        show_progress(&live, &cmp);
        // The chat's own agent, in its own conversation (a new message continues it).
        if let Some(i) = own {
            let label = chats::agent_label(&who[i].0, &who[i].1);
            let started = match (fresh, live.engine()) {
                (true, Some(e)) => {
                    let conv = db.current_conv(info.id.clone()).await.map_err(store_err)?;
                    let seen = conv.and_then(|c| c.node).or_else(|| live.given.locked().clone());
                    let unseen = super::unseen_since(&branch, seen.as_deref());
                    let t = text.clone().unwrap_or_default();
                    if let Some(st) = &e.session.structured {
                        st.prompt(super::with_seed(&unseen, &t, self.config().chat.seed_tokens));
                    }
                    Ok(())
                }
                _ => self.start_engine(&live, info, &before, &user).await,
            };
            match started {
                Ok(()) => self.watch_own(&live, &cmp, label, user.id.clone()),
                Err(e) => cmp.set(&label, "failed", &chats::clip(&e.message, 80)),
            }
        }
        for (i, (agent, profile)) in who.iter().enumerate() {
            if Some(i) == own {
                continue;
            }
            self.compare_helper(
                info.clone(),
                &cmp,
                agent.clone(),
                profile.clone(),
                before.clone(),
                user.clone(),
            );
        }
        // (Not holding the send lock: when every agent is already in, this finishes it.)
        drop(_one);
        self.compare_done(&info.id, &cmp).await;
        Ok(())
    }

    /// The chat's own agent's answer: done at its turn's end, failed if it ends without one.
    fn watch_own(
        self: &Arc<Self>,
        live: &Arc<Live>,
        cmp: &Arc<Compare>,
        label: String,
        user: String,
    ) {
        let me = Arc::clone(self);
        let (live, cmp) = (Arc::clone(live), Arc::clone(cmp));
        tokio::spawn(async move {
            let mut rx = live.rev.subscribe();
            let mut gone_since: Option<Instant> = None;
            loop {
                let outcome = {
                    let t = live.items.locked();
                    let after = t.items.iter().skip_while(|i| i.id != user).skip(1);
                    let mut end = None;
                    let mut error = None;
                    for it in after {
                        match &it.body {
                            ChatBody::TurnEnd { stop, .. } => {
                                end = Some(if stop == "cancelled" || stop == "interrupted" {
                                    Outcome::Stopped
                                } else {
                                    Outcome::Done
                                });
                            }
                            ChatBody::Notice { level: NoticeLevel::Error, text } => {
                                error = Some(chats::clip(text, 80));
                            }
                            _ => {}
                        }
                    }
                    end.or_else(|| {
                        // A resume being retried leaves it without an engine for a moment.
                        if live.engine().is_some() {
                            gone_since = None;
                            return None;
                        }
                        let since = *gone_since.get_or_insert_with(Instant::now);
                        (since.elapsed() > Duration::from_secs(3)).then(|| {
                            Outcome::Failed(
                                error.unwrap_or_else(|| "stopped before it answered".into()),
                            )
                        })
                    })
                };
                if let Some(o) = outcome {
                    match o {
                        Outcome::Done => cmp.set(&label, "done", ""),
                        Outcome::Stopped => cmp.set(&label, "stopped", ""),
                        Outcome::Failed(why) => cmp.set(&label, "failed", &why),
                        Outcome::TimedOut => cmp.set(&label, "failed", "timed out"),
                    }
                    me.compare_done(&live.id, &cmp).await;
                    return;
                }
                let _ = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
            }
        });
    }

    /// One other agent's answer, in a helper of its own; copied in when it is complete.
    fn compare_helper(
        self: &Arc<Self>,
        info: ChatInfo,
        cmp: &Arc<Compare>,
        agent: String,
        profile: String,
        before: Vec<ChatNode>,
        user: ChatNode,
    ) {
        let me = Arc::clone(self);
        let cmp = Arc::clone(cmp);
        tokio::spawn(async move {
            let label = chats::agent_label(&agent, &profile);
            let helper = chats::new_id();
            let row = ChatInfo {
                id: helper.clone(),
                agent: agent.clone(),
                profile: profile.clone(),
                incognito: true,
                side_of: Some(info.id.clone()),
                title: None,
                leaf: None,
                pinned: false,
                comparing: None,
                source: None,
                ..info.clone()
            };
            me.chats.helpers.locked().insert(helper.clone(), info.id.clone());
            me.chats.incognito.locked().insert(helper.clone(), row.clone());
            cmp.helpers.locked().push(helper.clone());
            let live = me.chats.live(&helper);
            let mut shown = before.clone();
            shown.push(user.clone());
            load_branch(&live, &shown);
            let outcome = match me.start_engine(&live, &row, &before, &user).await {
                Ok(()) => me.helper_outcome(&live, &cmp, &user.id).await,
                Err(e) => Outcome::Failed(chats::clip(&e.message, 80)),
            };
            let mut answer = helper_answer(&live, &user.id);
            // Stopped before it said anything: a tool call alone is no answer to keep.
            if matches!(outcome, Outcome::Stopped)
                && !answer.iter().any(|n| matches!(n.body, ChatBody::Assistant { .. }))
                || matches!(outcome, Outcome::TimedOut)
            {
                answer.clear();
            }
            // Deleted meanwhile: nothing is copied into it.
            let exists = match me.chat_db() {
                Ok(db) => matches!(db.get(info.id.clone()).await, Ok(Some(_))),
                Err(_) => false,
            };
            if !exists {
                answer.clear();
            }
            let (state, note) = match outcome {
                Outcome::TimedOut => ("failed", "timed out".to_owned()),
                _ if answer.is_empty() && cmp.stopped.load(Ordering::SeqCst) => {
                    ("stopped", String::new())
                }
                Outcome::Failed(why) if answer.is_empty() => ("failed", why),
                _ if answer.is_empty() => ("failed", "answered nothing".to_owned()),
                Outcome::Stopped => ("stopped", String::new()),
                _ => ("done", String::new()),
            };
            let denied = answer
                .iter()
                .filter(|n| {
                    matches!(&n.body, ChatBody::Permission { decided: Some(d), .. } if d == "deny")
                })
                .count();
            let note = match (state, denied) {
                ("done", 1) => "a tool it asked for was denied".to_owned(),
                ("done", n) if n > 1 => format!("{n} tools it asked for were denied"),
                _ => note,
            };
            if !answer.is_empty()
                && let Ok(db) = me.chat_db()
            {
                // Each node the next's parent, the first under the message.
                let mut parent = user.id.clone();
                let nodes: Vec<ChatNode> = answer
                    .into_iter()
                    .map(|n| {
                        let id = chats::new_id();
                        let node = ChatNode { id: id.clone(), parent: Some(parent.clone()), ..n };
                        parent = id;
                        node
                    })
                    .collect();
                let conv = Conv {
                    id: chats::new_id(),
                    chat: info.id.clone(),
                    agent: agent.clone(),
                    profile: profile.clone(),
                    conversation_id: None,
                    node: None,
                    how: "compare".into(),
                    ended_ms: None,
                };
                let index_tools = me.config().chat.index_tools;
                if let Err(e) = db.put_branch(info.id.clone(), nodes, conv, index_tools).await {
                    tracing::warn!(chat = %info.id, "compare: {e}");
                }
                if let Some(main) = me.chats.get_live(&info.id) {
                    me.refresh_siblings(&main).await;
                }
            }
            // The helper goes, with its scratch folder and the agent's own copy, before its
            // agent counts as in (so a compare that is over has left nothing behind).
            let engine = live.engine.locked().take();
            if let Some(e) = engine {
                e.session.hibernating.store(true, Ordering::Relaxed);
                me.stop_session(&e.session).await;
                me.forget_session(e.session.id);
            }
            me.end_incognito(&helper).await;
            me.chats.helpers.locked().remove(&helper);
            cmp.set(&label, state, &note);
            me.compare_done(&info.id, &cmp).await;
        });
    }

    /// Wait for a helper's answer: its turn's end, or its engine's. A permission request is
    /// denied (a helper has no card to show).
    async fn helper_outcome(&self, live: &Arc<Live>, cmp: &Arc<Compare>, user: &str) -> Outcome {
        let mut rx = live.rev.subscribe();
        let mut asked_stop: Option<Instant> = None;
        let started = Instant::now();
        loop {
            let (end, error, asks) = {
                let t = live.items.locked();
                let mut end = None;
                let mut error = None;
                let mut asks = false;
                for it in t.items.iter().skip_while(|i| i.id != user).skip(1) {
                    match &it.body {
                        ChatBody::TurnEnd { stop, .. } => end = Some(stop.clone()),
                        ChatBody::Notice { level: NoticeLevel::Error, text } => {
                            error = Some(chats::clip(text, 80));
                        }
                        ChatBody::Permission { decided: None, .. } => asks = true,
                        _ => {}
                    }
                }
                (end, error, asks)
            };
            if let Some(stop) = end {
                return if stop == "cancelled" || stop == "interrupted" {
                    Outcome::Stopped
                } else {
                    Outcome::Done
                };
            }
            let engine = live.engine();
            let Some(e) = engine else {
                return Outcome::Failed(
                    error.unwrap_or_else(|| "stopped before it answered".into()),
                );
            };
            if let Some(st) = &e.session.structured {
                if asks && st.has_pending() {
                    st.answer(Decision::Deny);
                }
                if started.elapsed() > HELPER_TIMEOUT {
                    st.interrupt();
                    return Outcome::TimedOut;
                }
                if cmp.stopped.load(Ordering::SeqCst) {
                    let since = *asked_stop.get_or_insert_with(|| {
                        st.interrupt();
                        Instant::now()
                    });
                    // An agent that doesn't stop when asked is stopped.
                    if since.elapsed() > Duration::from_secs(10) {
                        return Outcome::Stopped;
                    }
                }
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
        }
    }

    /// When every agent is in: the progress goes, a notice with the outcome stays on the shown
    /// branch (the first answer's, when the chat's own agent wasn't asked), and the chat is free.
    async fn compare_done(self: &Arc<Self>, chat: &str, cmp: &Arc<Compare>) {
        let live = self.chats.get_live(chat);
        let state = cmp.state.locked().clone();
        if state.agents.iter().any(|a| a.state == "asking") {
            if let Some(l) = &live {
                show_progress(l, cmp);
            }
            self.chat_changed(chat).await;
            return;
        }
        // Once: the last one in finishes it.
        let mine = {
            let mut m = self.chats.comparing.locked();
            match m.get(chat) {
                Some(c) if Arc::ptr_eq(c, cmp) => m.remove(chat).is_some(),
                _ => false,
            }
        };
        if !mine {
            return;
        }
        // Deleted meanwhile: nothing to tell it, and no live state to make again.
        let exists = match self.chat_db() {
            Ok(db) => matches!(db.get(chat.to_owned()).await, Ok(Some(_))),
            Err(_) => false,
        };
        if !exists {
            return;
        }
        let lock = self.chats.lock_for(chat);
        let _one = lock.lock().await;
        let live = self.chats.live(chat);
        {
            let id = progress_id(chat);
            live.items.locked().items.retain(|i| i.id != id);
        }
        // No answer of the chat's own agent: the first other answer in is shown.
        let done = |label: &str| state.agents.iter().any(|a| a.agent == label && a.state == "done");
        if !cmp.own.as_deref().is_some_and(done)
            && let Ok(db) = self.chat_db()
            && let Ok(kids) = db.child_agents(chat.to_owned(), state.node.clone()).await
            && let Some((first, _)) = kids.iter().find(|(_, a)| done(a))
            && let Ok(tip) = db.tip(chat.to_owned(), first.clone()).await
        {
            let _ = db.set_leaf(chat.to_owned(), tip).await;
        }
        self.reload_branch(&live).await;
        let ok = state.agents.iter().filter(|a| a.state == "done").count();
        let hint = if ok > 1 {
            " — ‹ › shows each answer; /pick keeps one (its agent takes the next message)"
        } else {
            ""
        };
        let level = if ok == 0 { NoticeLevel::Warn } else { NoticeLevel::Info };
        self.chat_notice(&live, chat, level, &format!("{}{hint}", state.line())).await;
        self.chat_changed(chat).await;
        self.chats.settle(chat);
    }

    /// `ChatStop` stops a compare's helpers too (the chat's own agent is stopped as ever).
    pub(super) fn stop_compare(&self, chat: &str) {
        let Some(cmp) = self.chats.comparing.locked().get(chat).cloned() else { return };
        cmp.stopped.store(true, Ordering::SeqCst);
        for h in cmp.helpers.locked().iter() {
            if let Some(st) = self
                .chats
                .get_live(h)
                .and_then(|l| l.engine())
                .and_then(|e| e.session.structured.clone())
            {
                st.interrupt();
            }
        }
    }

    /// Keep an answer (SPEC §22.7): show its branch, and switch the chat to the agent that
    /// wrote it, so the next message continues there (seeded with that branch).
    pub(super) async fn chat_pick(
        self: &Arc<Self>,
        info: &ChatInfo,
        node: Option<&str>,
    ) -> Result<(), Error> {
        if info.incognito {
            return Err(Error::invalid("an incognito chat keeps no branches"));
        }
        if info.source.is_some() {
            return Err(imported_refusal(info));
        }
        // Not mid-answer or mid-compare: the notice would split an answer being written.
        let lock = self.chats.lock_for(&info.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&info.id);
        self.busy_check(&live, info)?;
        let db = self.chat_db()?;
        let tip = match node {
            Some(n) => {
                let n = self.find_node(&info.id, n).await?;
                self.show_branch(&live, info, &n.id).await?;
                db.get(info.id.clone())
                    .await
                    .map_err(store_err)?
                    .and_then(|c| c.leaf)
                    .unwrap_or(n.id)
            }
            None => info.leaf.clone().ok_or_else(|| Error::invalid("nothing to keep yet"))?,
        };
        let Some((agent, profile)) =
            db.branch_agent(info.id.clone(), tip).await.map_err(store_err)?
        else {
            return Err(Error::new(
                ErrorCode::NotFound,
                "no agent wrote that branch (it is yours, or imported): nothing to switch to",
            ));
        };
        let text = if agent == info.agent && profile == info.profile {
            format!("kept this answer: {} carries on from it", chats::agent_label(&agent, &profile))
        } else {
            db.set_agent(info.id.clone(), agent.clone(), profile.clone())
                .await
                .map_err(store_err)?;
            // Its conversation knows another branch: the next message seeds the new agent.
            self.stop_chat_engine(&info.id).await;
            format!(
                "kept {}'s answer: the next message goes to {}",
                chats::agent_label(&agent, &profile),
                agent
            )
        };
        if live.items.locked().items.is_empty() {
            self.reload_branch(&live).await;
        }
        self.chat_notice(&live, &info.id, NoticeLevel::Info, &text).await;
        self.chat_changed(&info.id).await;
        Ok(())
    }
}

/// What a helper answered, as nodes (their parents are set when they are copied): its words,
/// tool calls, cards and turn end, without the notices ptyro itself put there.
fn helper_answer(live: &Live, user: &str) -> Vec<ChatNode> {
    let t = live.items.locked();
    let answer: Vec<ChatNode> = t
        .items
        .iter()
        .skip_while(|i| i.id != user)
        .skip(1)
        .filter(|i| match &i.body {
            ChatBody::Notice { level, .. } => *level == NoticeLevel::Error,
            ChatBody::Assistant { text, .. } => !text.trim().is_empty(),
            _ => true,
        })
        .map(|i| {
            let mut body = i.body.clone();
            if let ChatBody::Assistant { streaming, .. } = &mut body {
                *streaming = false;
            }
            ChatNode { id: i.id.clone(), parent: None, at_ms: i.at_ms, body }
        })
        .collect();
    // An answer has words or a tool call; a turn end alone (or an error) is not one.
    let said =
        answer.iter().any(|n| matches!(n.body, ChatBody::Assistant { .. } | ChatBody::Tool { .. }));
    if said { answer } else { Vec::new() }
}

/// The progress line, as an item of the live chat (replaced in place as agents finish).
fn show_progress(live: &Live, cmp: &Compare) {
    let line = cmp.state.locked().line();
    let id = progress_id(&live.id);
    let item = ptyro_core::chat::ChatItem::new(
        id,
        ChatBody::Notice { level: NoticeLevel::Info, text: line },
    );
    live.items.locked().upsert(item);
    live.bump();
}

/// Why an imported chat isn't changed in place (it is the agent's record).
pub(super) fn imported_refusal(info: &ChatInfo) -> Error {
    let agent = info.source.as_deref().and_then(|s| s.split(':').next()).unwrap_or("its agent");
    Error::invalid(format!(
        "an imported chat stays as {agent} wrote it: send it a message to continue it as a fork"
    ))
}
