//! One client connection (SPEC §6): handshake, requests, subscriptions with frame pacing and
//! acks, input routing, controller tracking (SPEC §5.8) and event delivery.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use ptyro_core::{ClientId, SessionId, TermSize};
use ptyro_protocol::{
    ClientKind, ClientMsg, DomainEvent, OuterTerm, PROTO, ScreenFrame, ServerMsg, decode, encode,
    framing,
};
use ptyro_term::StyleTable;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

use crate::daemon::Daemon;
use crate::session::Session;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

struct Sub {
    task: JoinHandle<()>,
    /// A structured session's transcript stream (M10).
    chat: Option<JoinHandle<()>>,
    acks: watch::Sender<u64>,
}

/// What the event-forwarding task needs to know about this connection.
#[derive(Default)]
struct Interest {
    subscribed: HashSet<SessionId>,
    watch_all: bool,
}

struct Conn {
    id: ClientId,
    daemon: Arc<Daemon>,
    out: mpsc::Sender<ServerMsg>,
    subs: HashMap<SessionId, Sub>,
    sizes: HashMap<SessionId, TermSize>,
    interest: Arc<Mutex<Interest>>,
    outer: Option<OuterTerm>,
    /// Who connected (the kernel says), for policy (M7.6).
    caller: crate::mail::Caller,
}

pub async fn serve(daemon: Arc<Daemon>, stream: UnixStream, pid: Option<u32>) {
    let mut framed = Framed::new(stream, framing());
    let Ok(Some(Ok(first))) = tokio::time::timeout(HELLO_TIMEOUT, framed.next()).await else {
        return;
    };
    let Ok((ClientMsg::Hello { proto, client, features }, enc)) = decode::<ClientMsg>(&first)
    else {
        tracing::debug!("first frame was not a Hello");
        return;
    };
    let version = env!("CARGO_PKG_VERSION").to_owned();
    if proto.major != PROTO.major {
        let reject = ServerMsg::Reject {
            reason: format!(
                "protocol {}.{} is not supported (daemon speaks {}.{})",
                proto.major, proto.minor, PROTO.major, PROTO.minor
            ),
            daemon_version: version,
        };
        if let Ok(b) = encode(&reject, enc) {
            let _ = framed.send(b).await;
        }
        return;
    }
    // Web and phone clients prove they're a paired device (M9.2); nothing else reaches the
    // daemon from them until they do.
    let remote =
        matches!(client.kind, ClientKind::Web | ClientKind::Mobile) || client.token.is_some();
    let grant = if remote {
        match daemon.authenticate(client.token.as_deref(), client.via.as_deref()) {
            Some(g) => Some(g),
            None => {
                tracing::warn!(via = ?client.via, "refused a client without a valid device token");
                let reject = ServerMsg::Reject {
                    reason: "unauthorized: this device isn't paired, or was revoked. Pair it with \
                             `ptyro pair` on the machine that runs ptyro"
                        .into(),
                    daemon_version: version,
                };
                if let Ok(b) = encode(&reject, enc) {
                    let _ = framed.send(b).await;
                }
                return;
            }
        }
    } else {
        None
    };
    let id = daemon.next_client_id();
    // Frame compression when the client asks for it (M8.7).
    let compress = features.iter().any(|f| f == ptyro_protocol::FEATURE_ZSTD);
    let welcome = ServerMsg::Welcome {
        proto: PROTO,
        daemon_version: version,
        client_id: id,
        features: if compress { vec![ptyro_protocol::FEATURE_ZSTD.into()] } else { vec![] },
        started_ms: daemon.started_ms,
    };
    let Ok(b) = encode(&welcome, enc) else { return };
    if framed.send(b).await.is_err() {
        return;
    }
    if let Some(g) = grant {
        tracing::info!(client = %id, device = %daemon.device_name(g.device), via = ?client.via, scope = g.scope.label(), "device connected");
    } else {
        tracing::debug!(client = %id, kind = ?client.kind, pid = client.pid, "connected");
    }
    daemon.clients.fetch_add(1, Ordering::Relaxed);

    let (mut sink, mut stream) = framed.split();
    let (out, mut out_rx) = mpsc::channel::<ServerMsg>(64);
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let bytes = if compress {
                ptyro_protocol::encode_compressed(&msg, enc)
            } else {
                encode(&msg, enc)
            };
            let Ok(bytes) = bytes else { continue };
            if sink.send(bytes).await.is_err() {
                break;
            }
        }
    });
    let interest = Arc::new(Mutex::new(Interest::default()));
    let events =
        tokio::spawn(forward_events(Arc::clone(&daemon), id, Arc::clone(&interest), out.clone()));
    let mut conn = Conn {
        id,
        daemon: Arc::clone(&daemon),
        out: out.clone(),
        subs: HashMap::new(),
        sizes: HashMap::new(),
        interest,
        outer: client.outer,
        caller: crate::mail::Caller { pid, kind: Some(client.kind), grant },
    };
    let mut shutdown = daemon.shutdown.subscribe();
    let mut goodbye = daemon.goodbye.subscribe();
    let mut revoked = daemon.devices_epoch.subscribe();
    loop {
        tokio::select! {
            // A revoked device is cut off at once.
            Ok(()) = revoked.changed(), if grant.is_some() => {
                if grant.is_some_and(|g| !daemon.device_valid(g.device)) {
                    let _ = out.send(ServerMsg::Goodbye { reason: "this device was revoked".into() }).await;
                    break;
                }
            }
            frame = stream.next() => match frame {
                Some(Ok(bytes)) => match decode::<ClientMsg>(&bytes) {
                    Ok((msg, _)) => conn.handle(msg).await,
                    Err(e) => {
                        tracing::debug!(client = %id, "bad frame: {e}");
                        // A request we can't read still gets an answer, so its caller doesn't
                        // wait forever (a newer client, a typo'd field).
                        #[derive(serde::Deserialize)]
                        struct Envelope {
                            t: String,
                            id: Option<u32>,
                        }
                        if let Ok((Envelope { t, id: Some(rid) }, _)) = decode::<Envelope>(&bytes)
                            && t == "request"
                        {
                            let error = ptyro_core::Error::invalid(format!("the daemon couldn't read this request: {e}"));
                            let _ = conn.out.send(ServerMsg::Response { id: rid, outcome: ptyro_protocol::Outcome::Err { error } }).await;
                        }
                    }
                },
                _ => break,
            },
            // The watch guard must not live across the `send` below (it isn't Send).
            () = async { let _ = shutdown.wait_for(|s| *s).await; } => {
                let _ = out.send(ServerMsg::Goodbye { reason: "daemon shutting down".into() }).await;
                break;
            }
            // A live upgrade (M8.1): clients reconnect to the next image.
            reason = async { goodbye.wait_for(Option::is_some).await.ok().and_then(|r| r.clone()) } => {
                let _ = out.send(ServerMsg::Goodbye { reason: reason.unwrap_or_default() }).await;
                break;
            }
        }
    }
    conn.close();
    events.abort();
    drop(conn);
    drop(out);
    let _ = writer.await;
    daemon.clients.fetch_sub(1, Ordering::Relaxed);
    tracing::debug!(client = %id, "disconnected");
}

impl Conn {
    async fn handle(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Request { id, req } => {
                let daemon = Arc::clone(&self.daemon);
                let out = self.out.clone();
                let caller = self.caller;
                tokio::spawn(async move {
                    let outcome = daemon.handle_as(req, caller).await.into();
                    let _ = out.send(ServerMsg::Response { id, outcome }).await;
                });
            }
            // A device watches a session at its own size; it never resizes it under the
            // person at the terminal.
            ClientMsg::Subscribe { session, size, control } if self.caller.grant.is_some() => {
                let _ = (size, control);
                self.subscribe(session, None, false).await
            }
            ClientMsg::Subscribe { session, size, control } => {
                self.subscribe(session, size, control).await
            }
            ClientMsg::Unsubscribe { session } => self.unsubscribe(session),
            ClientMsg::Ack { session, seq } => {
                if let Some(sub) = self.subs.get(&session) {
                    sub.acks.send_modify(|a| *a = (*a).max(seq));
                }
            }
            ClientMsg::Input { session, event } => {
                // Lifecycle scripts are read-only (SPEC §7.1); stop them with `x`.
                let script = |s: &Arc<Session>| {
                    s.meta.lock().expect("meta").kind == ptyro_core::session::SessionKind::Script
                };
                if let Some(s) = self.daemon.session(session).filter(|s| s.is_live() && !script(s))
                {
                    if let Some(g) = self.caller.grant {
                        if g.scope < ptyro_protocol::Scope::Control {
                            tracing::debug!(client = %self.id, "input refused: scope {}", g.scope.label());
                            return;
                        }
                        self.daemon.audit(&s, g, "typed");
                    } else {
                        self.take_control(&s).await;
                    }
                    s.input(&event).await;
                }
            }
            ClientMsg::Resize { .. } if self.caller.grant.is_some() => {}
            ClientMsg::Resize { session, size } => {
                self.sizes.insert(session, size);
                if let Some(s) = self.daemon.session(session) {
                    let controller = s.meta.lock().expect("meta").controller;
                    if controller.is_none_or(|c| c == self.id) {
                        self.take_control(&s).await;
                    }
                }
            }
            ClientMsg::WatchEvents => self.interest.lock().expect("interest").watch_all = true,
            // A paired device doesn't move `@focused` or quiet notifications (M9.2).
            ClientMsg::Focus { .. } if self.caller.grant.is_some() => {}
            ClientMsg::Focus { session, terminal_focused } => {
                self.daemon.set_focused(session);
                self.daemon.viewing(self.id, session, terminal_focused);
            }
            ClientMsg::Ping => {
                let _ = self.out.send(ServerMsg::Pong).await;
            }
        }
    }

    async fn subscribe(&mut self, id: SessionId, size: Option<TermSize>, control: bool) {
        let Some(session) = self.daemon.session(id) else {
            let _ =
                self.out.send(ServerMsg::Event { event: DomainEvent::SessionRemoved { id } }).await;
            return;
        };
        self.unsubscribe(id);
        if let Some(size) = size {
            self.sizes.insert(id, size);
        }
        if control {
            self.take_control(&session).await;
        }
        session.meta.lock().expect("meta").viewers += 1;
        self.interest.lock().expect("interest").subscribed.insert(id);
        let (acks, acks_rx) = watch::channel(0u64);
        let interval = self.daemon.frame_interval();
        let chat = session
            .structured
            .is_some()
            .then(|| tokio::spawn(stream_chat(Arc::clone(&session), self.out.clone())));
        let task =
            tokio::spawn(stream_frames(Arc::clone(&session), self.out.clone(), acks_rx, interval));
        self.subs.insert(id, Sub { task, chat, acks });
    }

    fn unsubscribe(&mut self, id: SessionId) {
        if let Some(sub) = self.subs.remove(&id) {
            sub.task.abort();
            if let Some(c) = sub.chat {
                c.abort();
            }
            if let Some(s) = self.daemon.session(id) {
                let mut meta = s.meta.lock().expect("meta");
                meta.viewers = meta.viewers.saturating_sub(1);
            }
            self.interest.lock().expect("interest").subscribed.remove(&id);
        }
    }

    /// The most recently active client controls the session and its size (SPEC §5.8).
    async fn take_control(&mut self, s: &Arc<Session>) {
        let changed = {
            let mut meta = s.meta.lock().expect("meta");
            let changed = meta.controller != Some(self.id);
            meta.controller = Some(self.id);
            changed
        };
        if changed {
            if let Some(o) = &self.outer {
                s.set_default_colors(o.fg, o.bg);
            }
            let _ = self
                .daemon
                .events
                .send(DomainEvent::ControllerChanged { session: s.id, controller: Some(self.id) });
        }
        if let Some(size) = self.sizes.get(&s.id).copied() {
            s.resize(size).await;
        }
    }

    fn close(&mut self) {
        self.daemon.forget_client(self.id);
        for id in self.subs.keys().copied().collect::<Vec<_>>() {
            self.unsubscribe(id);
        }
        for s in self.daemon.sessions() {
            let mut meta = s.meta.lock().expect("meta");
            if meta.controller == Some(self.id) {
                meta.controller = None;
                drop(meta);
                let _ = self
                    .daemon
                    .events
                    .send(DomainEvent::ControllerChanged { session: s.id, controller: None });
            }
        }
    }
}

/// Send frames for one session to one client: a keyframe first, then deltas, at most one frame
/// in flight (the client acks), at most one per `daemon.frame_interval_ms`. A slow client simply gets
/// fewer, bigger deltas; the session never waits for it.
async fn stream_frames(
    session: Arc<Session>,
    out: mpsc::Sender<ServerMsg>,
    mut acks: watch::Receiver<u64>,
    interval: Duration,
) {
    let mut changed = session.changed.subscribe();
    let mut styles = StyleTable::default();
    let mut sent: Option<u64> = None;
    loop {
        changed.borrow_and_update();
        let frame = {
            let mut term = session.term.lock().expect("term");
            let term = &mut *term;
            let seq = term.tracker.seq();
            if sent == Some(seq) {
                None
            } else {
                let delta = term.tracker.build(&term.emu, sent, &mut styles);
                // Damage that changed nothing: no frame, nothing to ack.
                (sent.is_none() || !delta.is_empty()).then(|| ScreenFrame {
                    session: session.id,
                    seq,
                    keyframe: sent.is_none(),
                    delta,
                })
            }
        };
        if let Some(frame) = frame {
            let seq = frame.seq;
            crate::watchdog::FRAMES_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if out.send(ServerMsg::Screen { frame }).await.is_err() {
                return;
            }
            sent = Some(seq);
            let paced = tokio::time::sleep(interval);
            if acks.wait_for(|a| *a >= seq).await.is_err() {
                return;
            }
            paced.await;
            continue;
        }
        if changed.changed().await.is_err() {
            return;
        }
    }
}

/// A structured session's transcript (M10): all of it first, then what changed (coalesced),
/// and the draft being typed into it.
async fn stream_chat(session: Arc<Session>, out: mpsc::Sender<ServerMsg>) {
    let Some(st) = session.structured.clone() else { return };
    let mut rx = st.chat_rev.subscribe();
    let (mut seen, mut first, mut draft_sent) = (0u64, true, String::new());
    loop {
        rx.borrow_and_update();
        if let Some((rev, items, draft)) = session.chat_since(if first { 0 } else { seen })
            && (first || !items.is_empty() || draft != draft_sent)
        {
            let msg = ServerMsg::Chat {
                session: session.id,
                rev,
                reset: first,
                items,
                draft: draft.clone(),
            };
            if out.send(msg).await.is_err() {
                return;
            }
            first = false;
            seen = rev;
            draft_sent = draft;
        }
        if rx.changed().await.is_err() {
            return;
        }
        // Streamed text arrives in many small pieces: send them a few at a time.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn forward_events(
    daemon: Arc<Daemon>,
    me: ClientId,
    interest: Arc<Mutex<Interest>>,
    out: mpsc::Sender<ServerMsg>,
) {
    let mut rx = daemon.events.subscribe();
    loop {
        let event = match rx.recv().await {
            Ok(e) => e,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(client = %me, "dropped {n} events");
                // Say so, so scripts (`ptyro events`) and TUIs know to re-read state.
                let event = DomainEvent::EventsLost { count: n };
                if out.send(ServerMsg::Event { event }).await.is_err() {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        let session = match &event {
            // Everyone hears about config changes.
            DomainEvent::ConfigReloaded { .. } | DomainEvent::EventsLost { .. } => {
                if out.send(ServerMsg::Event { event }).await.is_err() {
                    return;
                }
                continue;
            }
            // Workspace, project and worktree news goes to every watcher.
            DomainEvent::ProjectUpdated { .. }
            | DomainEvent::ProjectRemoved { .. }
            | DomainEvent::WorkspaceUpdated { .. }
            | DomainEvent::WorkspaceRemoved { .. }
            | DomainEvent::WorktreeUpdated { .. }
            | DomainEvent::WorktreeRemoved { .. }
            | DomainEvent::WorktreeProgress { .. }
            | DomainEvent::InboxAdded { .. }
            | DomainEvent::UsageUpdated { .. }
            | DomainEvent::RunUpdated { .. } => {
                let watching = interest.lock().expect("interest").watch_all;
                if watching && out.send(ServerMsg::Event { event }).await.is_err() {
                    return;
                }
                continue;
            }
            DomainEvent::SessionCreated { info }
            | DomainEvent::SessionUpdated { info }
            | DomainEvent::SessionRestarted { info }
            | DomainEvent::SessionExited { info } => info.id,
            DomainEvent::SessionRemoved { id } => *id,
            DomainEvent::ControllerChanged { session, .. }
            | DomainEvent::Bell { session }
            | DomainEvent::Clipboard { session, .. }
            | DomainEvent::Notification { session, .. }
            | DomainEvent::Notify { session, .. } => *session,
        };
        let wanted = {
            let i = interest.lock().expect("interest");
            i.watch_all || i.subscribed.contains(&session)
        };
        // Clipboard writes go only to the controller (SPEC §17).
        let allowed = match &event {
            DomainEvent::Clipboard { .. } => daemon
                .session(session)
                .is_some_and(|s| s.meta.lock().expect("meta").controller == Some(me)),
            _ => true,
        };
        if wanted && allowed && out.send(ServerMsg::Event { event }).await.is_err() {
            return;
        }
    }
}
