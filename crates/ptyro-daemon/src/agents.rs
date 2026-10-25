//! Agent intelligence in the daemon (SPEC §8, §9): the one place signals are reduced into status,
//! attention raised and cleared, and agents launched, resumed, hibernated, stopped, prompted and
//! answered.
//!
//! Every source (OSC 7501, titles, OSC 133, hooks, sidecars, self reports, heuristics) arrives as
//! a [`Feed`] on one channel and is processed in order by [`Daemon::run_feed`] (hooks and reports
//! wait for theirs, [`Daemon::apply_queued`]), so status, the inbox and notifications can never
//! disagree: a Blocked transition and a Working one never interleave.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ptyro_agents::{Decision, StopStep};
use ptyro_core::config::parse_duration;
use ptyro_core::session::{ProcessState, SessionKind};
use ptyro_core::status::{
    AgentStatus, Attention, AttentionKind, Child, Confidence, Signal, SignalSource, Sourced,
};
use ptyro_core::{Error, ErrorCode, SessionId, now_ms};
use ptyro_protocol::{DomainEvent, Reply};
use tokio::sync::mpsc;

use crate::daemon::Daemon;
use crate::engine::{Input, Transition};
use crate::session::{Feed, Session};

const HEURISTIC_ACTIVE_MS: u64 = 2_000;
const HEURISTIC_QUIET_MS: u64 = 30_000;
const ROSTER_AFTER_MS: u64 = 60_000;

fn ms(s: &str) -> u64 {
    parse_duration(s).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Why a transition wants the user (SPEC §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readiness {
    Ready,
    Blocked,
    NotYet,
}

pub fn attention_for(
    t: &Transition,
    kind: SessionKind,
    min_work_ms: u64,
) -> Option<(AttentionKind, String)> {
    let detail = t.to.detail.clone();
    match t.to.status {
        AgentStatus::Blocked => {
            let what = match t.to.kind.as_deref() {
                Some("permission") => "wants permission",
                Some("auth") => "needs you to log in",
                _ => "has a question",
            };
            Some((
                AttentionKind::Blocked,
                match detail {
                    Some(d) => format!("{what}: {d}"),
                    None => what.to_owned(),
                },
            ))
        }
        AgentStatus::Errored => {
            let k = t.to.kind.clone().unwrap_or_else(|| "error".into());
            Some((
                AttentionKind::Errored,
                match detail {
                    Some(d) => format!("stopped ({k}): {d}"),
                    None => format!("stopped ({k})"),
                },
            ))
        }
        AgentStatus::Done => Some((
            AttentionKind::Done,
            detail.map_or_else(|| "is done".into(), |d| format!("is done: {d}")),
        )),
        AgentStatus::Idle
            if t.from == Some(AgentStatus::Working)
                && t.worked_ms >= min_work_ms
                && t.from_confidence.is_some_and(|c| c > Confidence::Low) =>
        {
            let took = human(t.worked_ms);
            Some((
                AttentionKind::Idle,
                match kind {
                    SessionKind::Shell => match detail {
                        Some(d) => format!("finished a command after {took} ({d})"),
                        None => format!("finished a command after {took}"),
                    },
                    _ => format!("finished its turn after {took}"),
                },
            ))
        }
        _ => None,
    }
}

pub fn human(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

impl Daemon {
    /// The signal processor: one task, in order.
    pub fn start_feed(self: &Arc<Self>, mut rx: mpsc::UnboundedReceiver<Feed>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(f) = rx.recv().await {
                me.run_feed(f).await;
            }
        });
    }

    pub(crate) async fn run_feed(self: &Arc<Self>, f: Feed) {
        match f {
            Feed::Signals(id, list) => {
                if let Some(s) = self.session(id) {
                    self.apply_signals(&s, list).await;
                }
            }
            Feed::Apply(id, list, done) => {
                if let Some(s) = self.session(id) {
                    self.apply_signals(&s, list).await;
                }
                let _ = done.send(());
            }
            Feed::Flush(id) => {
                let Some(s) = self.session(id) else { return };
                let t = s.engine.lock().expect("engine").flush(now_ms());
                if let Some(t) = t {
                    self.on_transition(&s, &t).await;
                    let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
                }
            }
            Feed::Exited(id) => {
                let Some(s) = self.session(id) else { return };
                s.engine.lock().expect("engine").clear();
                let (state, kind, started) = {
                    let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    (m.state.clone(), m.kind, s.id.created_ms())
                };
                let ran = now_ms().saturating_sub(started);
                let failed = matches!(state, ProcessState::Exited { code: Some(c), .. } if c != 0)
                    || matches!(
                        state,
                        ProcessState::Exited { signal: Some(_), .. } | ProcessState::Failed { .. }
                    );
                // Agents always; commands and scripts when they failed or ran a while; shells never
                // (you typed `exit`).
                let wanted = state != ProcessState::Dormant
                    && !s.hibernating.load(Ordering::Relaxed)
                    && match kind {
                        SessionKind::Agent => true,
                        SessionKind::Command | SessionKind::Script => {
                            failed || ran >= self.min_work_ms()
                        }
                        SessionKind::Shell => false,
                    };
                if wanted {
                    self.raise(&s, AttentionKind::Exited, state.label()).await;
                }
                if state != ProcessState::Dormant && !s.hibernating.load(Ordering::Relaxed) {
                    self.drop_leases(&s);
                    if kind == SessionKind::Agent {
                        self.notice_lead(&s, "exited");
                        self.lead_exited(&s).await;
                    }
                }
                self.run_member_changed(&s).await;
                let _ = self.events.send(DomainEvent::SessionExited { info: self.info(&s) });
                // `exit` in a quick terminal closes it; the next one starts fresh (UX1.25).
                if s.is_quick() {
                    s.save(None);
                    self.forget_session(s.id);
                }
            }
        }
    }

    pub(crate) fn min_work_ms(&self) -> u64 {
        ms(&self.config.read().expect("config").attention.min_work)
    }

    /// Apply signals in the feed's order, and wait until they are (hooks, reports and ticks):
    /// one batch at a time across every source, so a Blocked transition and a Working one never
    /// interleave.
    async fn apply_queued(self: &Arc<Self>, s: &Arc<Session>, list: Vec<Sourced>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.feed_tx.send(Feed::Apply(s.id, list, tx)).is_ok() {
            let _ = rx.await;
        }
    }

    async fn apply_signals(self: &Arc<Self>, s: &Arc<Session>, list: Vec<Sourced>) {
        let now = now_ms();
        let mut changed = false;
        let mut status_moved = false;
        let mut notes: Vec<String> = Vec::new();
        for sg in list {
            match sg.signal {
                Signal::Status { status, kind, detail, progress } => {
                    let (t, deferred) = {
                        let mut e = s.engine.lock().expect("engine");
                        let t = e.apply(
                            Input {
                                source: sg.source,
                                confidence: sg.confidence,
                                status,
                                kind,
                                detail,
                                progress,
                            },
                            now,
                        );
                        (t, e.has_deferred())
                    };
                    if deferred {
                        // Re-check once the tie window has passed.
                        let (tx, id) = (self.feed_tx.clone(), s.id);
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(crate::engine::TIE_MS + 20))
                                .await;
                            let _ = tx.send(Feed::Flush(id));
                        });
                    }
                    changed = true;
                    if let Some(t) = t {
                        status_moved = true;
                        self.on_transition(s, &t).await;
                    }
                }
                Signal::Activity { text } => {
                    s.engine.lock().expect("engine").activity = Some(text);
                    changed = true;
                }
                Signal::Conversation { id } if !conversation_id_ok(&id) => {
                    // It would be handed to the agent's CLI on resume: never an option.
                    tracing::warn!(session = %s.id, "ignoring a conversation id that isn't one");
                }
                Signal::Conversation { id } => {
                    let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if m.conversation.as_deref() != Some(id.as_str()) {
                        m.conversation = Some(id);
                        drop(m);
                        s.save(None);
                        changed = true;
                    }
                }
                Signal::Prompt { text } => {
                    self.verify_on_prompt(s, &text);
                    self.mail_seen(s, &text);
                    let first = {
                        let mut m =
                            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let first = m.first_prompt.is_none();
                        if first {
                            m.first_prompt = Some(text.chars().take(500).collect());
                            // Typed by hand, it's what the session is about (UX1.24).
                            if m.title.is_none() {
                                m.title = Some(ptyro_core::session::brief(&text, 120));
                            }
                        }
                        first
                    };
                    if first {
                        s.save(None);
                        changed = true;
                        self.after_first_prompt(s, &text).await;
                    }
                }
                // The agent's own turn ends count (once per turn: Stop, notify and OSC 7501 `done`
                // can all end the same one); its working and idle flickers don't.
                Signal::TurnEnd => {
                    if !self.counts_turn_ends(s) {
                        self.after_turn(s).await;
                    } else if matches!(
                        s.status(),
                        Some(AgentStatus::Working | AgentStatus::Blocked)
                    ) {
                        let mut m =
                            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        m.turn_end_seen = m.turn_open;
                    } else if self.close_turn(s) {
                        self.turn_ended(s).await;
                        self.after_turn(s).await;
                        changed = true;
                    }
                }
                Signal::Child { id, kind, running } => {
                    let warn = {
                        let mut m =
                            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        match m.children.iter_mut().find(|c| c.id == id) {
                            Some(c) => c.running = running,
                            None => m.children.push(Child { id, kind, running }),
                        }
                        m.children.retain(|c| c.running);
                        let teammates = m
                            .children
                            .iter()
                            .filter(|c| c.kind.as_deref() == Some("teammate"))
                            .count();
                        (teammates >= 2 && m.place.worktree.is_some()).then_some(teammates)
                    };
                    if let Some(n) = warn
                        && !s
                            .meta
                            .lock()
                            .expect("meta")
                            .attention
                            .as_ref()
                            .is_some_and(|a| a.text.contains("teammates"))
                    {
                        notes.push(format!("{n} teammates share this worktree: their edits can collide (give each its own worktree)"));
                    }
                    changed = true;
                }
                Signal::Notify { text } => notes.push(text),
                Signal::Reply { text } => {
                    s.record_event("reply", serde_json::json!({ "chars": text.chars().count() }));
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last_reply =
                        Some(text);
                }
                Signal::Messaging { socket } => {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).messaging =
                        Some(socket)
                }
            }
        }
        // A program notification that came with a status change only explains it.
        if !status_moved {
            for n in notes {
                self.raise(s, AttentionKind::Notification, n).await;
            }
        }
        if changed {
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        }
    }

    /// Whether its turns are the agent's own turn ends (`Signal::TurnEnd`: hooks, `notify`,
    /// OSC 7501 `done`, a structured driver) rather than every working → idle. Startup noise
    /// (output, an agent's own "working" while it loads) then never counts as a turn.
    pub(crate) fn counts_turn_ends(&self, s: &Session) -> bool {
        if s.structured.is_some() {
            return true;
        }
        if s.kind() != SessionKind::Agent {
            return false;
        }
        let (adapter, agent) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (m.adapter.clone(), m.place.agent.clone())
        };
        let Some(adapter) = adapter else { return false };
        ptyro_agents::adapter(&adapter).caps().turn_end_signal
            && agent.is_none_or(|a| ptyro_agents::def(&self.config(), &a).hooks())
    }

    /// Closes the open turn; whether one was open.
    fn close_turn(&self, s: &Session) -> bool {
        let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.turn_end_seen = false;
        std::mem::take(&mut m.turn_open)
    }

    /// A turn finished: count it, then checkpoint, verify, tell a lead, look for collisions
    /// (M7) and summarize (UX1.3).
    async fn turn_ended(self: &Arc<Self>, s: &Arc<Session>) {
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turns += 1;
        if s.kind() == SessionKind::Agent {
            self.checkpoint_turn(s);
            self.verify_after_turn(s);
            self.notice_lead(s, "done");
            self.radar_after_turn(s);
            self.run_member_changed(s).await;
        }
        if s.kind() == SessionKind::Agent
            || s.meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .inner_agent
                .is_some()
        {
            self.summarize_after(s);
        }
    }

    async fn on_transition(self: &Arc<Self>, s: &Arc<Session>, t: &Transition) {
        if t.to.status == AgentStatus::Working {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.last_reply = None;
            m.turn_open = true;
        }
        if t.from == Some(AgentStatus::Working)
            && matches!(t.to.status, AgentStatus::Idle | AgentStatus::Done)
        {
            if !self.counts_turn_ends(s) {
                self.close_turn(s);
                self.turn_ended(s).await;
            } else if s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turn_end_seen
                && self.close_turn(s)
            {
                self.turn_ended(s).await;
                self.after_turn(s).await;
            }
        }
        if t.to.status == AgentStatus::Blocked && s.kind() == SessionKind::Agent {
            self.notice_lead(s, "blocked");
        }
        if t.to.status == AgentStatus::Blocked && t.from != Some(AgentStatus::Blocked) {
            self.summarize_after(s);
        }
        // Answered in the agent's own dialog: a held hook has nothing left to wait for.
        if t.from == Some(AgentStatus::Blocked) && t.to.status != AgentStatus::Blocked {
            self.release_held(s.id);
        }
        s.record_event(
            "status",
            serde_json::json!({
                "from": t.from.map(|f| f.label()),
                "to": t.to.status.label(),
                "kind": t.to.kind,
                "detail": t.to.detail,
                "source": t.to.source.label(),
                "confidence": format!("{:?}", t.to.confidence).to_lowercase(),
            }),
        );
        if t.to.status == AgentStatus::Working {
            if s.kind() == SessionKind::Agent && t.from != Some(AgentStatus::Blocked) {
                self.snapshot_turn(s);
            }
            // Someone answered, or it got going again: what it wanted is no longer pending.
            let had = s
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .attention
                .take()
                .is_some();
            if had {
                let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
            }
            return;
        }
        if let Some(why) = attention_for(t, s.kind(), self.min_work_ms()) {
            self.raise(s, why.0, why.1).await;
        }
        // The rate limit is a property of the account: tell the usage meters.
        if t.to.status == AgentStatus::Errored && t.to.kind.as_deref() == Some("rate_limit") {
            self.rate_limited(s);
        }
    }

    /// Whether someone is looking at it right now (shown in a focused pane of a focused terminal).
    pub fn watched(&self, id: SessionId) -> bool {
        self.viewing.lock().expect("viewing").values().any(|(sid, focused)| *sid == id && *focused)
    }

    /// Set attention (unless watched) and send it to the inbox and notifier.
    pub(crate) async fn raise(
        self: &Arc<Self>,
        s: &Arc<Session>,
        kind: AttentionKind,
        text: String,
    ) {
        if self.watched(s.id) {
            return;
        }
        // A quick terminal is quiet (UX1.25), but an agent started in it that waits for you
        // still says so.
        if s.is_quick() && kind != AttentionKind::Blocked {
            return;
        }
        {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Keep the more urgent one if it's still pending.
            if m.attention.as_ref().is_some_and(|a| a.kind < kind) && kind != AttentionKind::Exited
            {
                return;
            }
            m.attention = Some(Attention { kind, since_ms: now_ms(), text: text.clone() });
        }
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        self.notify(s, kind, text).await;
    }

    pub fn clear_attention(self: &Arc<Self>, s: &Arc<Session>) {
        let had = s
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attention
            .take()
            .is_some();
        if had {
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        }
        if let Some(store) = self.store.clone() {
            let id = s.id.full();
            tokio::spawn(async move {
                let _ = store.inbox_mark(Vec::new(), Some(id), true, true).await;
            });
        }
    }

    /// A client reports what it's looking at (SPEC §9.4: seeing it clears its attention).
    pub fn viewing(
        self: &Arc<Self>,
        client: ptyro_core::ClientId,
        session: SessionId,
        terminal_focused: bool,
    ) {
        self.viewing.lock().expect("viewing").insert(client, (session, terminal_focused));
        if terminal_focused && let Some(s) = self.session(session) {
            self.clear_attention(&s);
            // Opening a dormant agent resumes it (SPEC §7.4).
            let resume = self.config.read().expect("config").attention.resume_on_focus;
            if resume
                && s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state
                    == ProcessState::Dormant
            {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    if let Err(e) = me.resume(&s.id.full()).await {
                        tracing::warn!("resume on focus: {}", e.message);
                    }
                });
            }
        }
    }

    pub fn forget_client(&self, client: ptyro_core::ClientId) {
        self.viewing.lock().expect("viewing").remove(&client);
    }

    // ── hooks and self reports ─────────────────────────────────────────────

    /// A session by id, waiting briefly for one being spawned: a fast child can run its first
    /// hook (SessionStart) before the daemon has registered it, and that signal must not be lost.
    pub(crate) async fn session_soon(&self, id: SessionId) -> Option<Arc<Session>> {
        for _ in 0..40 {
            if let Some(s) = self.session(id) {
                return Some(s);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    pub(crate) async fn hook(
        self: &Arc<Self>,
        session: &str,
        adapter: &str,
        event: &str,
        payload: serde_json::Value,
        env: BTreeMap<String, String>,
    ) -> Result<Reply, Error> {
        let id: SessionId = session.parse().map_err(|_| Error::invalid("bad session id"))?;
        let s = self.session_soon(id).await.ok_or_else(|| Error::not_found(session))?;
        let adapter = s.adapter().unwrap_or_else(|| adapter.to_owned());
        // Codex's `notify` also reports its side threads (the one that writes the conversation's
        // title, SPEC §20.2): only the session's own conversation counts.
        if adapter == "codex"
            && event == "notify"
            && let Some(thread) = payload.get("thread-id").and_then(|t| t.as_str())
            && let Some(own) = s
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .conversation
                .clone()
            && own != thread
        {
            s.record_event("hook", serde_json::json!({ "adapter": adapter, "event": event, "ignored": "another thread" }));
            return Ok(Reply::Done);
        }
        let mut signals: Vec<Sourced> = ptyro_agents::adapter(&adapter)
            .on_hook(event, &payload)
            .into_iter()
            .map(|signal| Sourced {
                signal,
                source: SignalSource::Hook,
                confidence: Confidence::High,
            })
            .collect();
        if let Some(sock) = env.get("CLAUDE_CODE_MESSAGING_SOCKET") {
            signals.push(Sourced {
                signal: Signal::Messaging { socket: sock.clone() },
                source: SignalSource::Hook,
                confidence: Confidence::High,
            });
        }
        s.record_event("hook", serde_json::json!({ "adapter": adapter, "event": event, "tool": payload.get("tool_name") }));
        // Verified-done: Claude's turn may not end until the check passes (M7.9).
        if adapter == "claude"
            && event == "Stop"
            && let Some(out) = self.verify_stop_hook(&s).await
        {
            s.record_event("bounced", serde_json::json!({}));
            let run = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).run.clone();
            if let Some((run, step)) = run {
                self.emit(
                    &run,
                    vec![ptyro_run::RunEvent::Bounced { step, detail: "done_when failed".into() }],
                );
            }
            // Not idle: it keeps working on the failures.
            let working: Vec<Sourced> = signals
                .into_iter()
                .filter(|x| !matches!(x.signal, Signal::Status { .. } | Signal::TurnEnd))
                .collect();
            self.apply_queued(&s, working).await;
            return Ok(Reply::HookOutput { stdout: out });
        }
        // Leases and guards (M7.13): deny the tool call, with a reason the agent reads.
        if matches!(event, "PreToolUse" | "BeforeTool" | "pre_tool_use")
            && let Some(reason) = self.pretool_denial(&s, &adapter, &payload)
        {
            s.record_event(
                "denied",
                serde_json::json!({ "tool": payload.get("tool_name"), "reason": reason }),
            );
            let (run, name) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (m.run.clone(), m.name.clone())
            };
            if let Some((run, _)) = run {
                self.emit(
                    &run,
                    vec![ptyro_run::RunEvent::Note {
                        text: format!("{name}: denied — {reason}")
                    }],
                );
            }
            self.apply_queued(&s, signals).await;
            return Ok(Reply::HookOutput { stdout: crate::verify::deny_output(&adapter, &reason) });
        }
        // Remote approvals (M9.5): held before the prompt is announced, so an answer that comes
        // at once finds it; then wait for it. Claude's dialog stays up meanwhile.
        let hold = (adapter == "claude" && event == "PermissionRequest")
            .then(|| self.begin_hold(&s))
            .flatten();
        self.apply_queued(&s, signals).await;
        if let Some(h) = hold
            && let Some(reply) = self.finish_hold(&s, h).await
        {
            return Ok(reply);
        }
        Ok(Reply::Done)
    }

    pub(crate) async fn report(
        self: &Arc<Self>,
        addr: &str,
        state: &str,
        kind: Option<String>,
        msg: Option<String>,
        progress: Option<u8>,
    ) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let status = AgentStatus::parse(state).ok_or_else(|| {
            Error::invalid(format!("unknown state {state:?} (working, blocked, idle, done, error)"))
        })?;
        let detail = msg.map(|m| m.chars().filter(|c| !c.is_control()).take(500).collect());
        let signal =
            Signal::Status { status, kind, detail, progress: progress.map(|p| p.min(100)) };
        self.apply_queued(
            &s,
            vec![Sourced { signal, source: SignalSource::Report, confidence: Confidence::High }],
        )
        .await;
        Ok(Reply::Done)
    }

    pub fn explain(&self, s: &Session) -> Vec<ptyro_protocol::ExplainLine> {
        s.engine.lock().expect("engine").explain.iter().cloned().collect()
    }

    // ── prompts ────────────────────────────────────────────────────────────

    /// Is the session ready for typed input (SPEC §8.2)? Its status says idle, or the adapter
    /// recognises the input box, or (for programs that never sent a confident status) the screen
    /// has been quiet for 1.5 s after output. Never while it's blocked on a dialog.
    fn readiness(&self, s: &Session) -> Readiness {
        // A structured session says exactly when it's ready (M10).
        if s.structured.is_some() {
            return match s.status() {
                Some(AgentStatus::Idle | AgentStatus::Done) => Readiness::Ready,
                Some(AgentStatus::Blocked) => Readiness::Blocked,
                _ => Readiness::NotYet,
            };
        }
        let adapter = ptyro_agents::adapter(&s.adapter().unwrap_or_else(|| "generic".into()));
        let (record, never_high) = {
            let e = s.engine.lock().expect("engine");
            (e.record.clone(), e.high_quiet_ms(now_ms()) == u64::MAX)
        };
        if record.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked) {
            return Readiness::Blocked;
        }
        let reliable_idle = record
            .as_ref()
            .is_some_and(|r| r.status == AgentStatus::Idle && r.confidence == Confidence::High);
        let screen = s.read_screen(ptyro_protocol::ScreenFormat::Plain);
        let quiet = !adapter.caps().hooks && never_high && {
            let last = s.last_output_ms.load(Ordering::Relaxed);
            last > 0 && now_ms().saturating_sub(last) >= 1_500
        };
        if reliable_idle || adapter.ready(&screen) || quiet {
            Readiness::Ready
        } else {
            Readiness::NotYet
        }
    }

    /// Ready for typed input right now (prompt delivery's test).
    pub(crate) fn ready_for_input(&self, s: &Session) -> bool {
        self.readiness(s) == Readiness::Ready
    }

    /// Type a prompt (text + Enter) and remember when, so `ptyro wait --for idle` waits for
    /// the turn it starts rather than the idle before it.
    pub(crate) async fn type_prompt(&self, s: &Session, text: &str, enter: bool) {
        // A moment for the input box to take focus.
        tokio::time::sleep(Duration::from_millis(200)).await;
        // A trailing newline (a prompt file's last line) would submit before our Enter does.
        let text = if enter { text.trim_end() } else { text };
        // From the moment typing starts: a fast agent can finish its turn before we're done.
        let started = now_ms();
        s.send_text(text, enter).await;
        {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.prompt_pending = false;
            if enter {
                m.last_prompt_ms = Some(started);
            }
        }
        s.record_event("prompt", serde_json::json!({ "chars": text.chars().count() }));
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
    }

    /// `send --wait-ready`: wait until the session is ready (up to `timeout_ms`), then type.
    pub(crate) async fn send_when_ready(
        self: &Arc<Self>,
        s: &Arc<Session>,
        text: &str,
        enter: bool,
        timeout_ms: u64,
    ) -> Result<(), Error> {
        let start = now_ms();
        loop {
            if !s.is_live() {
                return Err(Error::conflict("session has exited"));
            }
            if self.readiness(s) == Readiness::Ready {
                self.type_prompt(s, text, enter).await;
                return Ok(());
            }
            if now_ms().saturating_sub(start) >= timeout_ms {
                return Err(Error::new(
                    ErrorCode::Timeout,
                    format!(
                        "{} wasn't ready for input within {}",
                        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name,
                        human(timeout_ms)
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Type `prompt` into the agent once it's ready (SPEC §8.2).
    pub(crate) fn deliver_prompt(self: &Arc<Self>, s: Arc<Session>, prompt: String) {
        let me = Arc::clone(self);
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).prompt_pending = true;
        tokio::spawn(async move {
            let start = now_ms();
            // Typing into a dialog could pick its preselected answer ("No, exit"): wait for
            // the agent to say it's ready, and never type blind (SPEC §8.2, verified 2.1.304).
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                if !s.is_live() {
                    return;
                }
                let r = me.readiness(&s);
                if r == Readiness::Ready {
                    me.type_prompt(&s, &prompt, true).await;
                    return;
                }
                // Waiting on a dialog is fine (the user may answer it); give up only on silence.
                if r != Readiness::Blocked && now_ms().saturating_sub(start) > 120_000 {
                    s.meta
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .prompt_pending = false;
                    let me = Arc::clone(&me);
                    me.raise(
                        &s,
                        AttentionKind::Notification,
                        "never got ready for its first prompt; type it yourself".into(),
                    )
                    .await;
                    return;
                }
            }
        });
    }

    /// The first prompt names the branch when `worktrees.auto_rename_branch` is on (M3.16).
    async fn after_first_prompt(self: &Arc<Self>, s: &Arc<Session>, text: &str) {
        if !self.config().worktrees.auto_rename_branch {
            return;
        }
        let Some(wt) = s
            .meta
            .lock()
            .expect("meta")
            .place
            .worktree
            .clone()
            .and_then(|w| self.worktree_by_id(&w))
        else {
            return;
        };
        if wt.is_root || wt.branch.as_deref() != Some(&format!("ptyro/{}", wt.name)) {
            return;
        }
        let words: String =
            ptyro_core::model::slugify(text).split('-').take(6).collect::<Vec<_>>().join("-");
        if let Err(e) = self.rename_branch_to(&wt.id, &words).await {
            tracing::debug!("auto-rename skipped: {}", e.message);
        }
    }

    // ── after_turn (M4.14) ─────────────────────────────────────────────────

    async fn after_turn(self: &Arc<Self>, s: &Arc<Session>) {
        let (agent, wt) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (m.place.agent.clone(), m.place.worktree.clone())
        };
        let (Some(agent), Some(wt_id)) = (agent, wt) else { return };
        if ptyro_agents::def(&self.config(), &agent).after_turn == Some(false) {
            return;
        }
        let Some(wt) = self.worktree_by_id(&wt_id) else { return };
        let Some(p) = self.project_by_id(&wt.project) else { return };
        let cfg = ptyro_core::project::ProjectConfig::load(&p.root, &wt.path).unwrap_or_default();
        if cfg.scripts.after_turn.is_none() || !self.is_trusted(&p.id, &cfg.trust_fingerprint()) {
            return;
        }
        // One after_turn session per worktree, run again each turn.
        let existing = self.sessions_in(&wt.id).into_iter().find(|x| {
            x.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).place.script.as_deref()
                == Some("after_turn")
        });
        match existing {
            Some(x) if x.is_live() => {}
            Some(x) => {
                let _ = self.restart(&x.id.full()).await;
            }
            None => {
                if let Err(e) =
                    self.run_script(&wt, Some("after_turn"), None, BTreeMap::new()).await
                {
                    tracing::warn!("after_turn: {}", e.message);
                }
            }
        }
    }

    // ── stop, hibernate, resume, answer ────────────────────────────────────

    /// The adapter's graceful stop, then the usual signal sequence (SPEC §8.2).
    pub(crate) async fn stop_session(self: &Arc<Self>, s: &Arc<Session>) {
        if !s.is_live() {
            return;
        }
        if let Some(st) = &s.structured {
            // End a turn in progress, give it a moment, then stop the process.
            if s.status() == Some(AgentStatus::Working) {
                st.interrupt();
                let _ = tokio::time::timeout(Duration::from_secs(3), s.wait_exit()).await;
            }
            let _ = s.kill(None).await;
            return;
        }
        if s.kind() == SessionKind::Agent
            && let Some(a) = s.adapter()
        {
            for step in ptyro_agents::adapter(&a).stop() {
                match step {
                    StopStep::InterruptKeys(k) => {
                        if s.status() == Some(AgentStatus::Working) {
                            s.write_raw(&k);
                        }
                    }
                    StopStep::Keys(k) => s.write_raw(&k),
                    StopStep::Wait(d) => {
                        if tokio::time::timeout(d, s.wait_exit()).await.is_ok() {
                            return;
                        }
                    }
                }
            }
        }
        let _ = s.kill(None).await;
    }

    /// Stop an agent and keep it resumable.
    pub(crate) async fn hibernate(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        if s.kind() != SessionKind::Agent || !s.is_live() {
            return Err(Error::invalid("only running agent sessions hibernate"));
        }
        let resumable = s.adapter().is_some_and(|a| ptyro_agents::adapter(&a).caps().resume);
        if !resumable
            || s.meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .conversation
                .is_none()
        {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "ptyro doesn't know this agent's conversation id, so it couldn't resume it",
            ));
        }
        s.hibernating.store(true, Ordering::Relaxed);
        self.stop_session(&s).await;
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// Relaunch into its conversation, same id, name, place and profile (SPEC §7.4).
    pub async fn resume(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        self.resume_as(addr, None).await
    }

    /// Switch an agent between structured mode and its terminal UI, in the same conversation
    /// (M10.6): stop it as hibernation does, then resume it the other way.
    pub(crate) async fn convert(
        self: &Arc<Self>,
        addr: &str,
        structured: bool,
        force: bool,
    ) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        if s.kind() != SessionKind::Agent {
            return Err(Error::invalid("only agent sessions switch modes"));
        }
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        if s.structured.is_some() == structured {
            return Err(Error::conflict(format!(
                "{name} already runs {}",
                if structured { "in structured mode" } else { "in its terminal UI" }
            )));
        }
        let adapter = s.adapter().unwrap_or_else(|| "generic".into());
        let def = ptyro_agents::def(
            &self.config(),
            &s.meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .place
                .agent
                .clone()
                .unwrap_or_default(),
        );
        if structured && def.protocol.is_none() && ptyro_drive::protocol_for(&adapter).is_none() {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!("{adapter} has no structured mode"),
            ));
        }
        if s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).conversation.is_none() {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "ptyro doesn't know {name}'s conversation id yet, so it can't carry it over"
                ),
            ));
        }
        if s.is_live() {
            if s.status() == Some(AgentStatus::Working) && !force {
                return Err(Error::conflict(format!(
                    "{name} is in the middle of a turn: wait for it, or --force to interrupt"
                )));
            }
            s.hibernating.store(true, Ordering::Relaxed);
            self.stop_session(&s).await;
            let _ = tokio::time::timeout(Duration::from_secs(10), s.wait_exit()).await;
        }
        s.record_event("convert", serde_json::json!({ "structured": structured }));
        self.resume_as(addr, Some(structured)).await
    }

    /// Resume a dormant agent, in the mode it had (or `mode`).
    pub(crate) async fn resume_as(
        self: &Arc<Self>,
        addr: &str,
        mode: Option<bool>,
    ) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        if s.is_live() {
            return Ok(Reply::Session { info: self.info(&s) });
        }
        let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let conv = m.conversation.clone().ok_or_else(|| {
            Error::new(ErrorCode::Unsupported, "no conversation id known for this session")
        })?;
        let adapter = m.adapter.clone().unwrap_or_else(|| "generic".into());
        if !ptyro_agents::adapter(&adapter).caps().resume {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!("{adapter} sessions can't be resumed"),
            ));
        }
        if !ptyro_agents::profile::exists(&self.config(), &m.place.profile) {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "profile {:?} no longer exists; its conversation lives in that account",
                    m.place.profile
                ),
            ));
        }
        let new = self
            .launch(crate::launch::Launch {
                id: s.id,
                argv: Vec::new(),
                kind: SessionKind::Agent,
                name: m.name.clone(),
                cwd: m.cwd.clone(),
                size: s.size(),
                client_env: m.client_env.clone(),
                place: m.place.clone(),
                replace: true,
                agent: Some(crate::launch::AgentStart {
                    name: m.place.agent.clone().unwrap_or(adapter),
                    resume: Some(conv),
                    prompt: None,
                    command: None,
                    mcp: false,
                    structured: mode.unwrap_or(m.structured.is_some()),
                }),
            })
            .await?;
        s.record_event("resume", serde_json::json!({}));
        let _ = self.events.send(DomainEvent::SessionRestarted { info: self.info(&new) });
        Ok(Reply::Session { info: self.info(&new) })
    }

    /// Answer the permission prompt (keys for TUIs, the API for OpenCode).
    /// Answer a session's prompt. `expect` names the prompt the answer is for (the blocked
    /// status's `since_ms`): a different prompt pending now is refused, so a stale notification
    /// or link never approves a newer request.
    pub async fn answer(
        self: &Arc<Self>,
        addr: &str,
        decision: Decision,
        expect: Option<u64>,
    ) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let st = s.engine.lock().expect("engine").record.clone();
        if st.as_ref().map(|r| r.status) != Some(AgentStatus::Blocked) {
            return Err(Error::conflict(format!(
                "{} isn't waiting on a prompt",
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name
            )));
        }
        if let (Some(want), Some(r)) = (expect, &st)
            && r.since_ms != want
        {
            return Err(Error::conflict(format!(
                "that prompt changed since you saw it: {} now asks {}; look again before answering",
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name,
                r.detail.clone().unwrap_or_else(|| "something else".into())
            )));
        }
        let adapter = s.adapter().unwrap_or_else(|| "generic".into());
        if let Some(st) = &s.structured {
            // Through the protocol: an exact decision (M10).
            if !st.answer(decision) {
                return Err(Error::conflict("no permission request is open"));
            }
        } else if adapter == "opencode" {
            self.opencode_answer(&s, decision).await?;
        } else if adapter == "claude" && self.answer_held(s.id, decision) {
            // Exact, through the held hook (M9.5).
        } else {
            let screen = s.read_screen(ptyro_protocol::ScreenFormat::Plain);
            let keys = ptyro_agents::adapter(&adapter)
                .answer_screen(&screen, decision)
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::Unsupported,
                        format!("ptyro can't answer {adapter}'s prompts; open the session"),
                    )
                })?;
            s.write_raw(&keys);
        }
        s.record_event(
            "answer",
            serde_json::json!({ "decision": format!("{decision:?}").to_lowercase() }),
        );
        self.clear_attention(&s);
        Ok(Reply::Done)
    }

    // ── background: heuristic, hibernation, the Claude roster ──────────────

    /// Once a second: the activity heuristic (§9.3) and hibernation (§7.4).
    pub fn start_agent_ticks(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut n: u64 = 0;
            loop {
                tick.tick().await;
                n += 1;
                me.heuristic_tick().await;
                if n.is_multiple_of(2) {
                    me.hibernate_tick().await;
                }
                if n.is_multiple_of(15) {
                    me.roster_tick().await;
                }
            }
        });
    }

    async fn heuristic_tick(self: &Arc<Self>) {
        let now = now_ms();
        for s in self.sessions() {
            if !s.is_live() {
                continue;
            }
            if s.kind() == SessionKind::Shell {
                self.inner_agent_tick(&s).await;
                continue;
            }
            // Before the first hook: dialogs only the screen shows (trust, login, updates).
            if s.kind() == SessionKind::Agent
                && s.engine.lock().expect("engine").high_quiet_ms(now) == u64::MAX
                && let Some(a) = s.adapter()
                && let Some(signal) = ptyro_agents::adapter(&a)
                    .on_screen(&s.read_screen(ptyro_protocol::ScreenFormat::Plain))
            {
                if let Signal::Status { detail: Some(d), .. } = &signal
                    && d == ptyro_agents::codex::HOOKS_REVIEW
                {
                    self.warn(format!(
                        "Codex is holding ptyro's hooks for review in {}: this Codex version hashes hooks differently, so its status is a guess (report your `codex --version`)",
                        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name
                    ));
                }
                self.apply_queued(
                    &s,
                    vec![Sourced {
                        signal,
                        source: SignalSource::Screen,
                        confidence: Confidence::Medium,
                    }],
                )
                .await;
                continue;
            }
            let last = s.last_output_ms.load(Ordering::Relaxed);
            let (status, low, fresh) = {
                let e = s.engine.lock().expect("engine");
                (
                    e.record.as_ref().map(|r| r.status),
                    e.record.as_ref().is_none_or(|r| r.confidence == Confidence::Low),
                    e.high_quiet_ms(now) < e.stale_after_ms,
                )
            };
            // A reliable source spoke recently: the guess would be ignored anyway.
            if fresh {
                continue;
            }
            let signal = if last > 0
                && now.saturating_sub(last) < HEURISTIC_ACTIVE_MS
                && status != Some(AgentStatus::Working)
            {
                Some(Signal::Status {
                    status: AgentStatus::Working,
                    kind: None,
                    detail: Some("output".into()),
                    progress: None,
                })
            } else if last > 0
                && now.saturating_sub(last) >= HEURISTIC_QUIET_MS
                && status == Some(AgentStatus::Working)
                && low
            {
                Some(Signal::Status {
                    status: AgentStatus::Idle,
                    kind: None,
                    detail: Some("quiet".into()),
                    progress: None,
                })
            } else {
                None
            };
            if let Some(signal) = signal {
                self.apply_queued(
                    &s,
                    vec![Sourced {
                        signal,
                        source: SignalSource::Heuristic,
                        confidence: Confidence::Low,
                    }],
                )
                .await;
            }
        }
    }

    /// A shell session with an agent in its foreground, started by hand (UX1.4): note which
    /// one, drop what the shell said (one long "working" command), and read its status off its
    /// screen at low confidence until it exits. Then the shell speaks for itself again.
    async fn inner_agent_tick(self: &Arc<Self>, s: &Arc<Session>) {
        let config = self.config();
        let fg = s.foreground_pid().and_then(ptyro_pty::process_name);
        let now = fg.as_deref().and_then(|p| ptyro_agents::adapter_for_program(&config, p));
        let was =
            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).inner_agent.clone();
        if now != was {
            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).inner_agent =
                now.clone();
            s.engine.lock().expect("engine").forget_sources(true);
            s.record_event("inner_agent", serde_json::json!({ "agent": now }));
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        }
        let Some(adapter) = now else { return };
        let screen = s.read_screen(ptyro_protocol::ScreenFormat::Plain);
        if let Some(signal) = ptyro_agents::adapter(&adapter).screen_status(&screen) {
            let low = Sourced { signal, source: SignalSource::Screen, confidence: Confidence::Low };
            self.apply_queued(s, vec![low]).await;
        }
    }

    /// `Adopt` (UX1.4): the Claude started by hand in a shell session leaves (`/exit` at its
    /// prompt) and comes back as a managed agent session in the same place, under the shell's
    /// profile, resuming the conversation it was in (its newest transcript for that directory).
    pub(crate) async fn adopt(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let (inner, profile, workspace, name) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                m.inner_agent.clone(),
                m.place.profile.clone(),
                m.place.workspace.clone(),
                m.name.clone(),
            )
        };
        let Some(agent) = inner else {
            return Err(Error::invalid(format!(
                "{name} has no agent in its foreground to adopt (start one with `claude` there first)"
            )));
        };
        if agent != "claude" {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "adopting works for Claude so far; start {agent} with `ptyro new --agent {agent}`"
                ),
            ));
        }
        if s.status() == Some(AgentStatus::Working) {
            return Err(Error::conflict(format!(
                "{name}'s Claude is in the middle of a turn: adopt it once it's idle"
            )));
        }
        let info = self.info(&s);
        let cwd = info.live_cwd.clone().unwrap_or_else(|| info.cwd.clone());
        let config = self.config();
        let root = self.paths.data.join("profiles");
        let plan =
            ptyro_agents::profile::plan(&config, &profile, &root, &mut |_, _| Ok(String::new()))
                .map_err(Error::invalid)?;
        let dir = plan
            .shown
            .iter()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| std::path::PathBuf::from(v))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".claude"))
            })
            .ok_or_else(|| Error::internal("no HOME"))?;
        let conversation =
            ptyro_agents::claude::latest_conversation(&dir, &cwd).ok_or_else(|| {
                Error::missing("conversation", format!("Claude's transcript for {}", cwd.display()))
            })?;
        // Leave it: clear the input line, then /exit, and wait for the shell to come back.
        s.write_raw(b"\x15/exit\r");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let fg = s.foreground_pid().and_then(ptyro_pty::process_name);
            if fg.as_deref().and_then(|p| ptyro_agents::adapter_for_program(&config, p)).is_none() {
                break;
            }
            if std::time::Instant::now() > deadline {
                return Err(Error::new(
                    ErrorCode::Timeout,
                    format!("{name}'s Claude didn't exit; type /exit there, then adopt again"),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let spec = ptyro_core::session::SpawnSpec {
            agent: Some(agent),
            cwd,
            profile: Some(profile),
            workspace,
            size: Some(info.size),
            resume: Some(conversation),
            confirmed: true,
            ..Default::default()
        };
        let new = self.spawn(spec).await?;
        s.record_event("adopted", serde_json::json!({ "into": new.id.full() }));
        Ok(Reply::Session { info: self.info(&new) })
    }

    async fn hibernate_tick(self: &Arc<Self>) {
        let config = self.config();
        let now = now_ms();
        for s in self.sessions() {
            if s.kind() != SessionKind::Agent || !s.is_live() {
                continue;
            }
            let (agent, viewers, conv) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (m.place.agent.clone(), m.viewers, m.conversation.is_some())
            };
            let Some(after) = agent
                .and_then(|a| ptyro_agents::def(&config, &a).hibernate_after)
                .map(|h| ms(&h))
                .filter(|d| *d > 0)
            else {
                continue;
            };
            let idle_for = {
                let e = s.engine.lock().expect("engine");
                e.record
                    .as_ref()
                    .filter(|r| r.status == AgentStatus::Idle)
                    .map(|r| now.saturating_sub(r.since_ms))
            };
            // Idle long enough, nobody watching, nothing running in the foreground but the agent.
            let busy_child =
                s.pid().is_some_and(|pid| s.foreground_pid().is_some_and(|fg| fg != pid));
            if idle_for.is_some_and(|d| d >= after)
                && viewers == 0
                && conv
                && !busy_child
                && self.hibernate(&s.id.full()).await.is_ok()
            {
                tracing::info!(session = %s.id, "hibernated after {}", human(after));
            }
        }
    }

    /// `claude agents --json` as a medium-confidence cross-check when hooks have been quiet
    /// (M4.18): it knows every interactive session's state by `sessionId`.
    async fn roster_tick(self: &Arc<Self>) {
        let now = now_ms();
        let quiet: Vec<Arc<Session>> = self
            .sessions()
            .into_iter()
            .filter(|s| s.is_live() && s.adapter().as_deref() == Some("claude"))
            .filter(|s| s.engine.lock().expect("engine").high_quiet_ms(now) >= ROSTER_AFTER_MS)
            .collect();
        if quiet.is_empty() {
            return;
        }
        // One roster per Claude account.
        let mut by_profile: HashMap<String, Vec<Arc<Session>>> = HashMap::new();
        for s in quiet {
            let p = s
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .place
                .profile
                .clone();
            by_profile.entry(p).or_default().push(s);
        }
        for (profile, sessions) in by_profile {
            let Ok(plan) = self.profile_plan(&profile).await else { continue };
            let env = plan.apply(self.base_env.iter().cloned());
            let argv = ptyro_agents::command(&self.config(), "claude")
                .unwrap_or_else(|| vec!["claude".into()]);
            // Bounded: a hung `claude agents` must not stall every session's ticks.
            let out = tokio::task::spawn_blocking(move || {
                let mut c = std::process::Command::new(&argv[0]);
                c.args(["agents", "--json"])
                    .env_clear()
                    .envs(env)
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                crate::notifier::run_bounded(c, None, Duration::from_secs(5))
            })
            .await;
            let Ok(Ok((_, stdout))) = out else { continue };
            let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&stdout) else {
                continue;
            };
            for s in sessions {
                let conv = s
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .conversation
                    .clone();
                let Some(row) = list.iter().find(|r| r["sessionId"].as_str() == conv.as_deref())
                else {
                    continue;
                };
                if let Some(status) = row["status"].as_str().and_then(AgentStatus::parse) {
                    let signal =
                        Signal::Status { status, kind: None, detail: None, progress: None };
                    self.apply_queued(
                        &s,
                        vec![Sourced {
                            signal,
                            source: SignalSource::Sidecar,
                            confidence: Confidence::Medium,
                        }],
                    )
                    .await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::status::StatusRecord;

    fn t(
        from: Option<AgentStatus>,
        to: AgentStatus,
        kind: Option<&str>,
        detail: Option<&str>,
        worked: u64,
    ) -> Transition {
        let mut r = StatusRecord::new(to, Confidence::High, SignalSource::Hook);
        r.kind = kind.map(Into::into);
        r.detail = detail.map(Into::into);
        Transition { from, from_confidence: Some(Confidence::High), to: r, worked_ms: worked }
    }

    #[test]
    fn attention_rules() {
        let a = attention_for(
            &t(
                Some(AgentStatus::Working),
                AgentStatus::Blocked,
                Some("permission"),
                Some("Bash: npm install"),
                0,
            ),
            SessionKind::Agent,
            10_000,
        );
        assert_eq!(a, Some((AttentionKind::Blocked, "wants permission: Bash: npm install".into())));
        // A short turn doesn't nag; a long one does.
        assert!(
            attention_for(
                &t(Some(AgentStatus::Working), AgentStatus::Idle, None, None, 3_000),
                SessionKind::Agent,
                10_000
            )
            .is_none()
        );
        let a = attention_for(
            &t(Some(AgentStatus::Working), AgentStatus::Idle, None, None, 75_000),
            SessionKind::Agent,
            10_000,
        )
        .unwrap();
        assert_eq!(a, (AttentionKind::Idle, "finished its turn after 1m15s".into()));
        let a = attention_for(
            &t(Some(AgentStatus::Working), AgentStatus::Idle, None, Some("exit 1"), 20_000),
            SessionKind::Shell,
            10_000,
        )
        .unwrap();
        assert!(a.1.contains("finished a command") && a.1.contains("exit 1"));
        let a = attention_for(
            &t(None, AgentStatus::Errored, Some("rate_limit"), None, 0),
            SessionKind::Agent,
            0,
        )
        .unwrap();
        assert_eq!(a, (AttentionKind::Errored, "stopped (rate_limit)".into()));
        assert!(
            attention_for(
                &t(Some(AgentStatus::Idle), AgentStatus::Working, None, None, 0),
                SessionKind::Agent,
                0
            )
            .is_none()
        );
    }
}

/// What a conversation id may look like: letters, digits and `._:-`, starting with a letter or
/// digit, so it can never read as an option (`--dangerously-skip-permissions`) when it is given
/// to an agent's `--resume`.
pub(crate) fn conversation_id_ok(id: &str) -> bool {
    id.len() <= 200
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
}

#[cfg(test)]
mod id_tests {
    use super::conversation_id_ok;

    #[test]
    fn a_conversation_id_is_never_an_option() {
        for ok in ["0b0f2a8e-6a3f-4f9d-9a41-3d7f2c1d9e55", "ses_2f9aB", "latest", "thread:1.2"] {
            assert!(conversation_id_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "--dangerously-skip-permissions",
            "-y",
            "a b",
            "a;b",
            "x\ny",
            "$(id)",
            "..",
            ".hidden",
            &"a".repeat(201),
        ] {
            assert!(!conversation_id_ok(bad), "{bad:?}");
        }
    }
}
