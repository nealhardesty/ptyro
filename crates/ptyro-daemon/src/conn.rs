//! One client connection (SPEC §6): handshake, requests, subscriptions with frame pacing and
//! acks, input routing, controller tracking (SPEC §5.8) and event delivery.

use ptyro_core::sync::MutexExt;
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
    /// The session the caller runs in, worked out at its first keystroke (a process can't
    /// gain an ancestor later).
    placed: Option<Option<SessionId>>,
    /// Another machine's daemon mirroring this one (MM1.B): it gets this machine's own things.
    link: bool,
    /// Our own connection to each other machine whose sessions this client watches or types
    /// into (MM1.B): frames, acks, keys and sizes go through it as they are.
    upstreams: HashMap<String, Upstream>,
    /// Mirrored sessions this client is subscribed to, and their machine.
    remote: HashMap<SessionId, String>,
    /// Chats this client follows (SPEC §22.14): the task streaming each.
    chats: HashMap<String, Aborting>,
    /// Another machine's chats it follows, and their machine (CHAT1.9): over its upstream.
    remote_chats: HashMap<String, String>,
}

/// A task that ends when this is dropped (a client that goes away stops its streams).
struct Aborting(JoinHandle<()>);

impl Drop for Aborting {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Upstream {
    client: Arc<ptyro_client::Client>,
    task: JoinHandle<()>,
    /// Our client id there.
    theirs: u64,
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
        scope: grant.map(|g| g.scope),
        machine: Some(daemon.machine()),
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
    let mut writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let bytes = if compress {
                ptyro_protocol::encode_compressed(&msg, enc)
            } else {
                encode(&msg, enc)
            };
            // A message too big for a frame (a whole scrollback, a huge chat) is answered, if it
            // was a response, with the reason; either way the connection lives on. Only a real
            // write failure ends it.
            let bytes = match bytes {
                Ok(b) if b.len() <= ptyro_protocol::MAX_FRAME => b,
                other => {
                    let transcript = matches!(
                        &msg,
                        ServerMsg::Response {
                            outcome: ptyro_protocol::Outcome::Ok {
                                reply: ptyro_protocol::Reply::ChatTranscript { .. }
                            },
                            ..
                        }
                    );
                    let why = match &other {
                        // A chat read in parts (`from`, `limit`) always fits.
                        Ok(b) if transcript => format!(
                            "the chat is too large to read at once ({} bytes; the limit is {}): read it in parts (from, limit)",
                            b.len(),
                            ptyro_protocol::MAX_FRAME
                        ),
                        Ok(b) => format!(
                            "the reply is too large ({} bytes; the limit is {}): ask for less",
                            b.len(),
                            ptyro_protocol::MAX_FRAME
                        ),
                        Err(e) => format!("the reply can't be encoded: {e}"),
                    };
                    tracing::warn!("client {id}: {why}");
                    let ServerMsg::Response { id: rid, .. } = &msg else { continue };
                    let error = ptyro_core::Error::invalid(why);
                    let reply = ServerMsg::Response {
                        id: *rid,
                        outcome: ptyro_protocol::Outcome::Err { error },
                    };
                    match encode(&reply, enc) {
                        Ok(b) => b,
                        Err(_) => continue,
                    }
                }
            };
            if sink.send(bytes).await.is_err() {
                break;
            }
        }
    });
    let link = client.kind == ClientKind::Machine || grant.is_some_and(|g| g.machine);
    let interest = Arc::new(Mutex::new(Interest::default()));
    let events = tokio::spawn(forward_events(
        Arc::clone(&daemon),
        id,
        Arc::clone(&interest),
        out.clone(),
        link,
    ));
    let mut conn = Conn {
        id,
        daemon: Arc::clone(&daemon),
        out: out.clone(),
        subs: HashMap::new(),
        sizes: HashMap::new(),
        interest,
        outer: client.outer,
        caller: crate::mail::Caller { pid, kind: Some(client.kind), grant },
        placed: None,
        link,
        upstreams: HashMap::new(),
        remote: HashMap::new(),
        chats: HashMap::new(),
        remote_chats: HashMap::new(),
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
    // The writer ends once every sender is gone; a long request (a gate, a wait) still holds
    // one, so don't wait for it for ever: the client is gone.
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer).await.is_err() {
        writer.abort();
    }
    daemon.clients.fetch_sub(1, Ordering::Relaxed);
    tracing::debug!(client = %id, "disconnected");
}

impl Conn {
    async fn handle(&mut self, msg: ClientMsg) {
        // A chat's own agent, and whatever it runs, may search, read and list, by requests that
        // are scoped to its chat (SPEC §22.5): it never follows a chat, a screen or the events,
        // nor types into a session, here or on another machine.
        if !matches!(
            msg,
            ClientMsg::Hello { .. }
                | ClientMsg::Request { .. }
                | ClientMsg::Ping
                | ClientMsg::Ack { .. }
        ) && self.placed_session().is_some_and(|s| s.meta.locked().place.chat.is_some())
        {
            tracing::debug!(client = %self.id, "a chat engine's stream refused");
            return;
        }
        // Another machine's session (MM1.B): what is said about it goes to its machine.
        let msg = match self.route_to_machine(msg).await {
            Some(m) => m,
            None => return,
        };
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Request { id, req } => {
                let daemon = Arc::clone(&self.daemon);
                let out = self.out.clone();
                let caller = self.caller;
                tokio::spawn(async move {
                    // A request that panics still gets an answer: its caller isn't left waiting.
                    let work = tokio::spawn(async move { daemon.handle_as(req, caller).await });
                    let outcome: ptyro_protocol::Outcome = match work.await {
                        Ok(r) => r.into(),
                        Err(e) => {
                            tracing::error!("a request panicked: {e}");
                            ptyro_protocol::Outcome::Err {
                                error: ptyro_core::Error::internal(
                                    "the daemon hit an internal error handling this request (see its log)",
                                ),
                            }
                        }
                    };
                    let _ = out.send(ServerMsg::Response { id, outcome }).await;
                });
            }
            // A device watches a session at its own size and never resizes it under the person at
            // the terminal, unless it has `control` scope and asks: then it may take control (its
            // clipboard writes reach it, its size counts), like the TUI (WEB1.8, SPEC §5.8).
            ClientMsg::Subscribe { session, size, control } if self.caller.grant.is_some() => {
                let may =
                    self.caller.grant.is_some_and(|g| g.scope >= ptyro_protocol::Scope::Control);
                self.subscribe(session, size.filter(|_| may), control && may).await
            }
            ClientMsg::Subscribe { session, size, control } => {
                self.subscribe(session, size, control).await
            }
            ClientMsg::Unsubscribe { session } => self.unsubscribe(session),
            ClientMsg::ChatSubscribe { chat } => self.chat_subscribe(chat).await,
            ClientMsg::ChatUnsubscribe { chat } => self.chat_unsubscribe(chat).await,
            ClientMsg::Ack { session, seq } => {
                if let Some(sub) = self.subs.get(&session) {
                    sub.acks.send_modify(|a| *a = (*a).max(seq));
                }
            }
            ClientMsg::Input { session, event } => {
                // Lifecycle scripts are read-only (SPEC §7.1); stop them with `x`.
                let script = |s: &Arc<Session>| {
                    s.meta.locked().kind == ptyro_core::session::SessionKind::Script
                };
                if let Some(s) = self.daemon.session(session).filter(|s| s.is_live() && !script(s))
                {
                    if let Some(g) = self.caller.grant {
                        if g.scope < ptyro_protocol::Scope::Control {
                            tracing::debug!(client = %self.id, "input refused: scope {}", g.scope.label());
                            return;
                        }
                        self.daemon.audit(&s, g, "typed");
                        // Typing from a phone makes it the controller, as typing at the terminal does.
                        self.take_control(&s).await;
                    } else {
                        // An agent's keys pass the same guard as its `send` (M7.6): typing
                        // into a blocked session, or outside its run, is refused.
                        if let Some(me) = self.placed_session() {
                            let req = ptyro_protocol::Request::SendBytes {
                                addr: s.id.full(),
                                bytes: Vec::new(),
                            };
                            if let Err(e) = self.daemon.guard_agent(&req, &me) {
                                tracing::debug!(client = %self.id, "input refused: {}", e.message);
                                return;
                            }
                        }
                        self.take_control(&s).await;
                    }
                    s.input(&event).await;
                }
            }
            ClientMsg::Resize { .. }
                if self.caller.grant.is_some_and(|g| g.scope < ptyro_protocol::Scope::Control) => {}
            ClientMsg::Resize { session, size } => {
                self.sizes.insert(session, size);
                if let Some(s) = self.daemon.session(session) {
                    let controller = s.meta.locked().controller;
                    if controller.is_none_or(|c| c == self.id) {
                        self.take_control(&s).await;
                    }
                }
            }
            ClientMsg::WatchEvents => self.interest.locked().watch_all = true,
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
        session.meta.locked().viewers += 1;
        self.interest.locked().subscribed.insert(id);
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
                let mut meta = s.meta.locked();
                meta.viewers = meta.viewers.saturating_sub(1);
            }
            self.interest.locked().subscribed.remove(&id);
        }
    }

    /// A message about a mirrored session goes over this client's own connection to its machine
    /// (made on first use): subscriptions, acks, keys, sizes, focus. Anything else comes back to
    /// be handled here. Keys from a device need `control` here first, as for our own sessions.
    async fn route_to_machine(&mut self, msg: ClientMsg) -> Option<ClientMsg> {
        if self.link {
            return Some(msg);
        }
        let session = match &msg {
            ClientMsg::Subscribe { session, .. }
            | ClientMsg::Unsubscribe { session }
            | ClientMsg::Ack { session, .. }
            | ClientMsg::Input { session, .. }
            | ClientMsg::Resize { session, .. }
            | ClientMsg::Focus { session, .. } => *session,
            _ => return Some(msg),
        };
        if self.daemon.session(session).is_some() {
            return Some(msg);
        }
        let Some((machine, transport, online)) = self.daemon.machine_of_session(session) else {
            return Some(msg);
        };
        let scope = self.caller.grant.map(|g| g.scope);
        let msg = match msg {
            // A device below `control` watches at the session's own size and never types.
            ClientMsg::Input { .. } | ClientMsg::Resize { .. }
                if scope.is_some_and(|s| s < ptyro_protocol::Scope::Control) =>
            {
                return None;
            }
            ClientMsg::Subscribe { session, .. }
                if scope.is_some_and(|s| s < ptyro_protocol::Scope::Control) =>
            {
                ClientMsg::Subscribe { session, size: None, control: false }
            }
            // A paired device doesn't move focus (M9.2).
            ClientMsg::Focus { .. } if scope.is_some() => return None,
            m => m,
        };
        match &msg {
            ClientMsg::Subscribe { .. } => {
                self.remote.insert(session, machine.clone());
            }
            ClientMsg::Unsubscribe { .. } => {
                self.remote.remove(&session);
            }
            _ => {}
        }
        // Offline: nothing to say it to (its pane shows it as it was, with why).
        if !online {
            return None;
        }
        self.send_upstream(&machine, &transport, msg).await;
        None
    }

    /// Say `msg` to `machine` over this client's own connection to it (made the first time; one
    /// fresh one if it went away). Frames and transcripts come back through `relay_upstream`.
    /// Another machine's chat this client may follow (SPEC §22.11): named with its machine, by
    /// its id, or by a handle only that machine has. `None`: one of this machine's.
    async fn remote_chat(
        &self,
        chat: &str,
    ) -> Option<(String, String, ptyro_core::model::MachineTransport, bool)> {
        if self.link {
            return None;
        }
        if let Some(r) = self.daemon.machine_of_chat(chat, false) {
            return Some(r);
        }
        // A handle: ours first.
        match self.daemon.find_chat(chat).await {
            Err(e) if e.code == ptyro_core::ErrorCode::NotFound => {
                self.daemon.machine_of_chat(chat, true)
            }
            _ => None,
        }
    }

    /// Follow a chat. Kept by its full id (`laptop2:<id>` for another machine's), so any of its
    /// addresses stops it.
    async fn chat_subscribe(&mut self, chat: String) {
        // Another machine's chat (SPEC §22.11): followed over this client's own upstream to it,
        // as screens are; its engine runs there.
        if let Some((machine, there, transport, online)) = self.remote_chat(&chat).await {
            // Kept even while it is down: an unsubscribe goes to it, and the client follows
            // again when it is back (the machine's update says so).
            self.remote_chats.insert(format!("{machine}:{there}"), machine.clone());
            if !online {
                let since = self.daemon.offline_since(&machine);
                let text = format!(
                    "{machine} is offline{since}; this chat opens when it is back (it reconnects by itself)"
                );
                let item = ptyro_core::chat::ChatItem::new(
                    "offline",
                    ptyro_core::chat::ChatBody::Notice {
                        level: ptyro_core::chat::NoticeLevel::Warn,
                        text,
                    },
                );
                let msg = ServerMsg::ChatLive {
                    chat,
                    rev: 0,
                    reset: true,
                    items: vec![item],
                    siblings: Vec::new(),
                };
                let _ = self.out.send(msg).await;
                return;
            }
            self.send_upstream(&machine, &transport, ClientMsg::ChatSubscribe { chat: there })
                .await;
            return;
        }
        // (One that can't be found says why in the stream, kept by what was typed.)
        let key = self.daemon.find_chat(&chat).await.map_or_else(|_| chat.clone(), |c| c.id);
        let task =
            tokio::spawn(stream_chat_live(Arc::clone(&self.daemon), key.clone(), self.out.clone()));
        self.chats.insert(key, Aborting(task));
    }

    async fn chat_unsubscribe(&mut self, chat: String) {
        if self.chats.remove(&chat).is_some() || self.remote_chats.contains_key(&chat) {
            if let Some(machine) = self.remote_chats.remove(&chat) {
                self.unsubscribe_remote_chat(&machine, &chat);
            }
            return;
        }
        if let Some((machine, there, _, _)) = self.remote_chat(&chat).await {
            let key = format!("{machine}:{there}");
            if self.remote_chats.remove(&key).is_some() {
                self.unsubscribe_remote_chat(&machine, &key);
            }
            return;
        }
        if let Ok(c) = self.daemon.find_chat(&chat).await {
            self.chats.remove(&c.id);
        }
    }

    fn unsubscribe_remote_chat(&self, machine: &str, key: &str) {
        let there = key.strip_prefix(&format!("{machine}:")).unwrap_or(key).to_owned();
        if let Some(u) = self.upstreams.get(machine) {
            let _ = u.client.send(ClientMsg::ChatUnsubscribe { chat: there });
        }
    }

    async fn send_upstream(
        &mut self,
        machine: &str,
        transport: &ptyro_core::model::MachineTransport,
        msg: ClientMsg,
    ) {
        let machine = machine.to_owned();
        for _ in 0..2 {
            if !self.upstreams.contains_key(&machine) {
                match self.daemon.dial(&machine, transport).await {
                    Ok(mut c) => {
                        let pushes = c.pushes();
                        let theirs = c.id.0;
                        self.daemon
                            .upstream_ids
                            .locked()
                            .insert((machine.clone(), theirs), self.id);
                        let task = tokio::spawn(relay_upstream(pushes, self.out.clone()));
                        self.upstreams.insert(
                            machine.clone(),
                            Upstream { client: Arc::new(c), task, theirs },
                        );
                    }
                    Err(e) => {
                        tracing::debug!(client = %self.id, machine, "upstream: {}", e.message);
                        return;
                    }
                }
            }
            let sent = self.upstreams.get(&machine).map(|u| u.client.send(msg.clone()));
            match sent {
                Some(Ok(())) => return,
                // It went away: one fresh connection, then give up.
                _ => self.drop_upstream(&machine),
            }
        }
    }

    fn drop_upstream(&mut self, machine: &str) {
        if let Some(u) = self.upstreams.remove(machine) {
            u.task.abort();
            self.daemon.upstream_ids.locked().remove(&(machine.to_owned(), u.theirs));
        }
    }

    fn placed_session(&mut self) -> Option<Arc<Session>> {
        let daemon = &self.daemon;
        let pid = self.caller.pid;
        let id = *self
            .placed
            .get_or_insert_with(|| pid.and_then(|p| daemon.session_of_pid(p)).map(|s| s.id));
        id.and_then(|id| self.daemon.session(id))
    }

    /// The most recently active client controls the session and its size (SPEC §5.8).
    async fn take_control(&mut self, s: &Arc<Session>) {
        let changed = {
            let mut meta = s.meta.locked();
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
        for m in self.upstreams.keys().cloned().collect::<Vec<_>>() {
            self.drop_upstream(&m);
        }
        self.daemon.forget_client(self.id);
        for id in self.subs.keys().copied().collect::<Vec<_>>() {
            self.unsubscribe(id);
        }
        for s in self.daemon.sessions() {
            let mut meta = s.meta.locked();
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
            let mut term = session.term.locked();
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

/// A followed chat (SPEC §22.14): its shown branch, then what changed; the whole branch again
/// when it is reloaded. A chat that can't be followed says why, as a notice.
async fn stream_chat_live(daemon: Arc<Daemon>, chat: String, out: mpsc::Sender<ServerMsg>) {
    let live = match daemon.chat_follow(&chat).await {
        Ok(l) => l,
        Err(e) => {
            let item = ptyro_core::chat::ChatItem::new(
                "error",
                ptyro_core::chat::ChatBody::Notice {
                    level: ptyro_core::chat::NoticeLevel::Error,
                    text: e.message,
                },
            );
            let _ = out
                .send(ServerMsg::ChatLive {
                    chat,
                    rev: 0,
                    reset: true,
                    items: vec![item],
                    siblings: Vec::new(),
                })
                .await;
            return;
        }
    };
    // Stops following when this task ends, however it ends.
    struct Following(Arc<Daemon>, Arc<crate::chats::Live>);
    impl Drop for Following {
        fn drop(&mut self) {
            self.0.chat_unfollow(&self.1);
        }
    }
    let _f = Following(Arc::clone(&daemon), Arc::clone(&live));
    let (mut revs, mut epochs) = (live.rev.subscribe(), live.epoch.subscribe());
    let mut seen = 0u64;
    let mut epoch = u64::MAX;
    loop {
        let now = *epochs.borrow_and_update();
        revs.borrow_and_update();
        let reset = now != epoch;
        let (rev, items) = {
            let t = live.items.locked();
            (t.rev, if reset { t.items.clone() } else { t.since(seen) })
        };
        if reset || !items.is_empty() {
            // With a reset, where the branch has others (the client's ‹ 2/3 ›).
            let siblings = if reset { live.siblings.locked().clone() } else { Vec::new() };
            for msg in live_frames(&live.id, rev, reset, items, siblings) {
                if out.send(msg).await.is_err() {
                    return;
                }
            }
        }
        epoch = now;
        seen = rev;
        tokio::select! {
            r = revs.changed() => if r.is_err() { return },
            r = epochs.changed() => if r.is_err() { return },
        }
        // Streamed text arrives in many small pieces: send them a few at a time.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// A chat's items as `ChatLive` frames that each fit in a frame (a long chat's branch is more
/// than one holds, and a frame too big is dropped): the first carries `reset` and the siblings,
/// the rest carry on from it. A tool's output too big for a frame by itself is cut.
fn live_frames(
    chat: &str,
    rev: u64,
    reset: bool,
    items: Vec<ptyro_core::chat::ChatItem>,
    siblings: Vec<ptyro_core::chats::ChatSiblings>,
) -> Vec<ServerMsg> {
    const BUDGET: usize = ptyro_protocol::MAX_FRAME / 2;
    let size = |i: &ptyro_core::chat::ChatItem| serde_json::to_vec(i).map_or(0, |v| v.len());
    let mut frames: Vec<Vec<ptyro_core::chat::ChatItem>> = vec![Vec::new()];
    let mut used = 0;
    for mut it in items {
        let mut n = size(&it);
        if n > BUDGET
            && let ptyro_core::chat::ChatBody::Tool { output: Some(o), .. } = &mut it.body
        {
            *o = ptyro_core::chats::clip(o, BUDGET / 2);
            n = size(&it);
        }
        if used + n > BUDGET && frames.last().is_some_and(|f| !f.is_empty()) {
            frames.push(Vec::new());
            used = 0;
        }
        used += n;
        frames.last_mut().expect("one frame").push(it);
    }
    let mut siblings = Some(siblings);
    frames
        .into_iter()
        .enumerate()
        .map(|(i, items)| ServerMsg::ChatLive {
            chat: chat.to_owned(),
            rev,
            reset: reset && i == 0,
            items,
            siblings: if i == 0 { siblings.take().unwrap_or_default() } else { Vec::new() },
        })
        .collect()
}

/// What another machine sends this client over its upstream: the frames and transcripts it
/// subscribed to, and clipboard writes for the session it controls there. Everything else about
/// that machine comes through the daemon's link (`machines.rs`).
async fn relay_upstream(
    mut pushes: mpsc::UnboundedReceiver<ServerMsg>,
    out: mpsc::Sender<ServerMsg>,
) {
    while let Some(msg) = pushes.recv().await {
        let pass = match &msg {
            ServerMsg::Screen { .. } | ServerMsg::Chat { .. } | ServerMsg::ChatLive { .. } => true,
            ServerMsg::Event { event: DomainEvent::Clipboard { .. } } => true,
            ServerMsg::Goodbye { .. } => return,
            _ => false,
        };
        if pass && out.send(msg).await.is_err() {
            return;
        }
    }
}

async fn forward_events(
    daemon: Arc<Daemon>,
    me: ClientId,
    interest: Arc<Mutex<Interest>>,
    out: mpsc::Sender<ServerMsg>,
    link: bool,
) {
    let mut rx = daemon.events.subscribe();
    // Other machines' news, for every client but another machine's link (no echoes).
    let mut mirrored = (!link).then(|| daemon.mirrored.subscribe());
    loop {
        let got = tokio::select! {
            r = rx.recv() => r,
            r = async {
                match mirrored.as_mut() {
                    Some(m) => m.recv().await,
                    None => std::future::pending().await,
                }
            } => r,
        };
        let event = match got {
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
            | DomainEvent::ProjectAutoAdded { .. }
            | DomainEvent::WorkspaceUpdated { .. }
            | DomainEvent::WorkspaceRemoved { .. }
            | DomainEvent::WorktreeUpdated { .. }
            | DomainEvent::WorktreeRemoved { .. }
            | DomainEvent::WorktreeProgress { .. }
            | DomainEvent::InboxAdded { .. }
            | DomainEvent::UsageUpdated { .. }
            | DomainEvent::ScratchChanged { .. }
            | DomainEvent::MachineUpdated { .. }
            | DomainEvent::MachineRemoved { .. }
            | DomainEvent::ChatUpdated { .. }
            | DomainEvent::ChatRemoved { .. }
            | DomainEvent::RunUpdated { .. } => {
                let watching = interest.locked().watch_all;
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
            | DomainEvent::SessionResumed { session }
            | DomainEvent::Clipboard { session, .. }
            | DomainEvent::Notification { session, .. }
            | DomainEvent::Notify { session, .. } => *session,
        };
        let wanted = {
            let i = interest.locked();
            i.watch_all || i.subscribed.contains(&session)
        };
        // Clipboard writes go only to the controller (SPEC §17).
        let allowed = match &event {
            DomainEvent::Clipboard { .. } => {
                daemon.session(session).is_some_and(|s| s.meta.locked().controller == Some(me))
            }
            _ => true,
        };
        if wanted && allowed && out.send(ServerMsg::Event { event }).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_chat_is_sent_in_frames_that_fit() {
        use ptyro_core::chat::{ChatBody, ChatItem, ToolStatus};
        let big = "x".repeat(ptyro_protocol::MAX_FRAME / 5);
        let mut items: Vec<ChatItem> = (0..6)
            .map(|i| {
                ChatItem::new(
                    format!("a{i}"),
                    ChatBody::Assistant { text: big.clone(), streaming: false },
                )
            })
            .collect();
        let tool = ChatBody::Tool {
            name: "Bash".into(),
            title: "cat huge".into(),
            status: ToolStatus::Done,
            output: Some("y".repeat(ptyro_protocol::MAX_FRAME)),
            diff: None,
            files: Vec::new(),
        };
        items.push(ChatItem::new("t", tool));
        let frames = live_frames("c1", 7, true, items, Vec::new());
        assert!(frames.len() >= 4, "{}", frames.len());
        let mut ids = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let ServerMsg::ChatLive { reset, items, rev, .. } = f else { panic!() };
            assert_eq!((*reset, *rev), (i == 0, 7));
            let bytes = ptyro_protocol::encode(f, ptyro_protocol::Encoding::Json).unwrap();
            assert!(bytes.len() <= ptyro_protocol::MAX_FRAME, "frame {i}: {}", bytes.len());
            ids.extend(items.iter().map(|i| i.id.clone()));
        }
        assert_eq!(ids, ["a0", "a1", "a2", "a3", "a4", "a5", "t"], "every item, in order");
        // A small one is one frame, as before.
        let one = live_frames(
            "c1",
            1,
            true,
            vec![ChatItem::new("n", ChatBody::User { text: "hi".into() })],
            Vec::new(),
        );
        assert_eq!(one.len(), 1);
        assert_eq!(
            live_frames("c1", 1, true, Vec::new(), Vec::new()).len(),
            1,
            "an empty reset still resets"
        );
    }
}
