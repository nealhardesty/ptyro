//! One client connection (SPEC §6): handshake, requests, subscriptions with frame pacing and
//! acks, input routing, controller tracking (SPEC §5.8) and event delivery.

use ptyro_core::sync::MutexExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use ptyro_core::now_ms;
use ptyro_core::{ClientId, SessionId, TermSize};
use ptyro_protocol::{
    ClientKind, ClientMsg, DomainEvent, OuterTerm, PROTO, Refused, RefusedWhy, ScreenFrame, SeqKey,
    ServerMsg, decode, encode, framing,
};

use crate::typed::Plan;
use ptyro_term::StyleTable;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

use crate::daemon::Daemon;
use crate::session::Session;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

struct Sub {
    task: JoinHandle<()>,
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
    caller: crate::guard::Caller,
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

pub async fn serve(daemon: Arc<Daemon>, stream: crate::listen::Stream, pid: Option<u32>) {
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
    // Numbered input (1.52, WEB2.3), said back to a client that asks.
    let seq = features.iter().any(|f| f == ptyro_protocol::FEATURE_INPUT_SEQ);
    let mut agreed = Vec::new();
    if compress {
        agreed.push(ptyro_protocol::FEATURE_ZSTD.to_owned());
    }
    if seq {
        agreed.push(ptyro_protocol::FEATURE_INPUT_SEQ.to_owned());
    }
    let welcome = ServerMsg::Welcome {
        proto: PROTO,
        daemon_version: version,
        client_id: id,
        features: agreed,
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
        caller: crate::guard::Caller { pid, kind: Some(client.kind), grant },
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
                        struct Tag {
                            t: String,
                        }
                        #[derive(serde::Deserialize)]
                        struct Envelope {
                            t: String,
                            id: Option<u32>,
                            req: Option<Tag>,
                        }
                        if let Ok((Envelope { t, id: Some(rid), req }, _)) = decode::<Envelope>(&bytes)
                            && t == "request"
                        {
                            // One an older ptyro had says what went, not that it is unreadable.
                            let error = req
                                .and_then(|r| ptyro_protocol::retired_request(&r.t))
                                .unwrap_or_else(|| ptyro_core::Error::invalid(format!("the daemon couldn't read this request: {e}")));
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
    // The writer ends once every sender is gone; a long request (a wait) still holds
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
            ClientMsg::Keys { session, stream, keys, replay } => {
                self.keys(session, stream, keys, replay).await
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

    /// Numbered input (1.52, WEB2.3; `typed.rs`): the same rules as `Input` (scope, scripts,
    /// an agent's guard, control), then each number typed at most once, a replay only while the
    /// session waits for what it did; always answered with `Typed`, except while the daemon
    /// hands off to its next image (the client sends them again there).
    async fn keys(&mut self, id: SessionId, stream: String, keys: Vec<SeqKey>, replay: bool) {
        let (first, last) = (keys.first().map_or(0, |k| k.n), keys.last().map_or(0, |k| k.n));
        let refuse = |why: RefusedWhy, reason: &str| ServerMsg::Typed {
            session: id,
            stream: stream.clone(),
            upto: last,
            refused: vec![Refused { from: first, to: last, why, reason: reason.to_owned() }],
        };
        let script =
            |s: &Arc<Session>| s.meta.locked().kind == ptyro_core::session::SessionKind::Script;
        let Some(s) = self.daemon.session(id).filter(|s| s.is_live() && !script(s)) else {
            let why = "the session isn't running";
            let _ = self.out.send(refuse(RefusedWhy::Ended, why)).await;
            return;
        };
        if let Some(g) = self.caller.grant {
            if g.scope < ptyro_protocol::Scope::Control {
                tracing::debug!(client = %self.id, "keys refused: scope {}", g.scope.label());
                let why = "this device may only watch: typing needs `control` scope";
                let _ = self.out.send(refuse(RefusedWhy::Scope, why)).await;
                return;
            }
        } else if let Some(me) = self.placed_session() {
            let req = ptyro_protocol::Request::SendBytes { addr: s.id.full(), bytes: Vec::new() };
            if let Err(e) = self.daemon.guard_agent(&req, &me) {
                tracing::debug!(client = %self.id, "keys refused: {}", e.message);
                let _ = self.out.send(refuse(RefusedWhy::Scope, &e.message)).await;
                return;
            }
        }
        let token = s.state_token();
        let decided = {
            let mut table = s.typed.locked();
            // Handing off: nothing more is typed or settled here, so the image holds exactly
            // what was (it is written after this, under the same lock).
            if self.daemon.goodbye.borrow().is_some() {
                None
            } else {
                let who = self.caller.grant.map(|g| g.device);
                let mut d = table.decide(who, &stream, &keys, replay, &token, now_ms());
                let mut typed = 0;
                if let Plan::Type { from } = &d.plan {
                    // A key the input queue drops (the session isn't reading) is not typed, and
                    // neither is anything after it: refused, not said typed.
                    let rest = &keys[*from..];
                    typed = rest.iter().take_while(|k| s.input_now(&k.event)).count();
                    if let Some(k) = rest.get(typed) {
                        d.refused.push(table.overflowed(who, &stream, k.n, d.upto));
                    }
                }
                Some((d, typed))
            }
        };
        let Some((d, typed)) = decided else { return };
        if typed > 0 {
            // Typing makes this client the controller, as `Input` does; a copy or a refusal
            // doesn't (it typed nothing).
            self.take_control(&s).await;
            if let Some(g) = self.caller.grant {
                self.daemon.audit(&s, g, if replay { "typed (sent again)" } else { "typed" });
            }
        }
        if let Some(r) = d.refused.last() {
            tracing::info!(client = %self.id, session = %s.id, why = ?r.why, "keys not typed: {}", r.reason);
        }
        let msg = ServerMsg::Typed { session: id, stream, upto: d.upto, refused: d.refused };
        let _ = self.out.send(msg).await;
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
        let task =
            tokio::spawn(stream_frames(Arc::clone(&session), self.out.clone(), acks_rx, interval));
        self.subs.insert(id, Sub { task, acks });
    }

    fn unsubscribe(&mut self, id: SessionId) {
        if let Some(sub) = self.subs.remove(&id) {
            sub.task.abort();
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
            | ClientMsg::Keys { session, .. }
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
        if let ClientMsg::Keys { stream, keys, replay, .. } = msg {
            self.keys_upstream(session, stream, keys, replay, &machine, &transport, online, scope)
                .await;
            return None;
        }
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

    /// Numbered keys for another machine's session (1.52): its daemon settles them and its
    /// `Typed` comes back through `relay_upstream`. Refused here when this device may only watch
    /// or the machine is offline. A machine whose ptyro can't number input gets them as plain
    /// `Input`, acknowledged once handed to its connection (what it could promise before); a
    /// replay can't be checked there, so it is refused.
    #[allow(clippy::too_many_arguments)]
    async fn keys_upstream(
        &mut self,
        session: SessionId,
        stream: String,
        keys: Vec<SeqKey>,
        replay: bool,
        machine: &str,
        transport: &ptyro_core::model::MachineTransport,
        online: bool,
        scope: Option<ptyro_protocol::Scope>,
    ) {
        let (first, last) = (keys.first().map_or(0, |k| k.n), keys.last().map_or(0, |k| k.n));
        let answer = |refused: Option<(RefusedWhy, String)>| ServerMsg::Typed {
            session,
            stream: stream.clone(),
            upto: last,
            refused: refused
                .map(|(why, reason)| Refused { from: first, to: last, why, reason })
                .into_iter()
                .collect(),
        };
        let why = if scope.is_some_and(|s| s < ptyro_protocol::Scope::Control) {
            Some((
                RefusedWhy::Scope,
                "this device may only watch: typing needs `control` scope".into(),
            ))
        } else if !online {
            Some((RefusedWhy::Offline, format!("{machine} is offline")))
        } else {
            None
        };
        if let Some(why) = why {
            let _ = self.out.send(answer(Some(why))).await;
            return;
        }
        if !self.ensure_upstream(machine, transport).await {
            return; // Unanswered: the client sends them again, or gives up and says so.
        }
        let numbered = self.upstreams.get(machine).is_some_and(|u| {
            u.client.features.iter().any(|f| f == ptyro_protocol::FEATURE_INPUT_SEQ)
        });
        if numbered {
            let msg = ClientMsg::Keys { session, stream: stream.clone(), keys, replay };
            self.send_upstream(machine, transport, msg).await;
            return;
        }
        if replay {
            let why =
                format!("{machine} runs an older ptyro, which can't check they are still wanted");
            let _ = self.out.send(answer(Some((RefusedWhy::Unknown, why)))).await;
            return;
        }
        let sent = self.upstreams.get(machine).is_some_and(|u| {
            keys.into_iter()
                .all(|k| u.client.send(ClientMsg::Input { session, event: k.event }).is_ok())
        });
        if sent {
            let _ = self.out.send(answer(None)).await;
        } else {
            self.drop_upstream(machine);
        }
    }

    /// This client's connection to `machine`, made if there is none. False: it can't be reached.
    async fn ensure_upstream(
        &mut self,
        machine: &str,
        transport: &ptyro_core::model::MachineTransport,
    ) -> bool {
        if self.upstreams.contains_key(machine) {
            return true;
        }
        match self.daemon.dial(machine, transport).await {
            Ok(mut c) => {
                let pushes = c.pushes();
                let theirs = c.id.0;
                self.daemon.upstream_ids.locked().insert((machine.to_owned(), theirs), self.id);
                let task = tokio::spawn(relay_upstream(pushes, self.out.clone()));
                self.upstreams
                    .insert(machine.to_owned(), Upstream { client: Arc::new(c), task, theirs });
                true
            }
            Err(e) => {
                tracing::debug!(client = %self.id, machine, "upstream: {}", e.message);
                false
            }
        }
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
                let msg = ServerMsg::ChatLive { chat, rev: 0, reset: true, items: vec![item] };
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
            if !self.ensure_upstream(&machine, transport).await {
                return;
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

/// Frames sent to a client and not acked yet, and how long its acks take (WEB2.4). A client
/// whose acks come back quickly (a terminal on this computer, a LAN) has one frame in flight, as
/// always; one far away (a phone on cellular, 100–300 ms) gets two or three, so a change made
/// while a frame is on its way isn't held back a whole round trip before it is even sent. Still
/// bounded: a client that stops acking gets at most `MAX` frames, then nothing until it acks.
#[derive(Debug, Default)]
struct Inflight {
    sent: VecDeque<(u64, Instant)>,
    /// The smoothed time from a frame to its ack.
    srtt: Option<Duration>,
}

impl Inflight {
    const MAX: usize = 3;
    /// Acks this slow get a second frame in flight…
    const TWO: Duration = Duration::from_millis(40);
    /// …and this slow, a third.
    const THREE: Duration = Duration::from_millis(120);

    fn window(&self) -> usize {
        match self.srtt {
            Some(r) if r >= Self::THREE => Self::MAX,
            Some(r) if r >= Self::TWO => 2,
            _ => 1,
        }
    }

    fn full(&self) -> bool {
        self.sent.len() >= self.window()
    }

    fn sent(&mut self, seq: u64, at: Instant) {
        self.sent.push_back((seq, at));
    }

    /// The client has every frame up to `upto`: the newest of them says how long acks take.
    fn acked(&mut self, upto: u64, now: Instant) {
        let mut took = None;
        while let Some(&(seq, at)) = self.sent.front() {
            if seq > upto {
                break;
            }
            took = Some(now.saturating_duration_since(at));
            self.sent.pop_front();
        }
        if let Some(t) = took {
            self.srtt = Some(self.srtt.map_or(t, |s| (s * 3 + t) / 4));
        }
    }
}

/// Send frames for one session to one client: a keyframe first, then deltas, at most one per
/// `daemon.frame_interval_ms`, with one frame in flight until the client acks it (two or three
/// for a client far away: `Inflight`). A slow client simply gets fewer, bigger deltas; the
/// session never waits for it.
async fn stream_frames(
    session: Arc<Session>,
    out: mpsc::Sender<ServerMsg>,
    mut acks: watch::Receiver<u64>,
    interval: Duration,
) {
    let mut changed = session.changed.subscribe();
    let mut styles = StyleTable::default();
    let mut sent: Option<u64> = None;
    let mut sent_token: Option<String> = None;
    let mut flight = Inflight::default();
    loop {
        flight.acked(*acks.borrow_and_update(), Instant::now());
        if flight.full() {
            if acks.changed().await.is_err() {
                return;
            }
            continue;
        }
        changed.borrow_and_update();
        // What the session waits for as this is drawn (WEB2.3), taken before the screen (the
        // token takes the screen's lock, and `meta`'s, on its own): never newer than what the
        // frame shows, so a key is never vouched for by a prompt its page hadn't drawn yet.
        let token = session.state_token();
        let at_ms = now_ms();
        let frame = {
            let mut term = session.term.locked();
            let term = &mut *term;
            let seq = term.tracker.seq();
            // What the session waits for changed with nothing new on its screen (a hook set its
            // status): a frame all the same, its token alone, so its page's keys say what it
            // waits for as the page shows it.
            let moved = sent_token.as_deref() != Some(token.as_str());
            let delta = if sent == Some(seq) {
                ptyro_core::screen::ScreenDelta::default()
            } else {
                // Each delta is from the frame sent before it: the client applies them in order.
                term.tracker.build(&term.emu, sent, &mut styles)
            };
            // Damage that changed nothing (and no new token): no frame, nothing to ack.
            (sent.is_none() || !delta.is_empty() || moved).then(|| ScreenFrame {
                session: session.id,
                seq,
                keyframe: sent.is_none(),
                delta,
                state_token: Some(token.clone()),
                at_ms: Some(at_ms),
            })
        };
        if let Some(frame) = frame {
            let seq = frame.seq;
            crate::watchdog::FRAMES_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if out.send(ServerMsg::Screen { frame }).await.is_err() {
                return;
            }
            sent = Some(seq);
            sent_token = Some(token);
            flight.sent(seq, Instant::now());
            tokio::time::sleep(interval).await;
            continue;
        }
        let waiting = !flight.sent.is_empty();
        tokio::select! {
            r = changed.changed() => if r.is_err() { return; },
            // (An ack is a sample of how far away the client is, taken as it comes.)
            r = acks.changed(), if waiting => if r.is_err() { return; },
        }
    }
}

/// A followed chat (SPEC §22.14): its conversation, then what changed; the whole of it again
/// when it is reloaded (a retry, an edit). A chat that can't be followed says why, as a notice.
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
                .send(ServerMsg::ChatLive { chat, rev: 0, reset: true, items: vec![item] })
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
            for msg in live_frames(&live.id, rev, reset, items) {
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

/// A chat's items as `ChatLive` frames that each fit in a frame (a long chat is more than one
/// holds, and a frame too big is dropped): the first carries `reset`, the rest carry on from
/// it. A tool's output too big for a frame by itself is cut.
fn live_frames(
    chat: &str,
    rev: u64,
    reset: bool,
    items: Vec<ptyro_core::chat::ChatItem>,
) -> Vec<ServerMsg> {
    const BUDGET: usize = ptyro_protocol::MAX_FRAME / 2;
    let size = |i: &ptyro_core::chat::ChatItem| serde_json::to_vec(i).map_or(0, |v| v.len());
    let mut frames: Vec<Vec<ptyro_core::chat::ChatItem>> = Vec::new();
    let mut cur = Vec::new();
    let mut used = 0;
    for mut it in items {
        let mut n = size(&it);
        if n > BUDGET
            && let ptyro_core::chat::ChatBody::Tool { output: Some(o), .. } = &mut it.body
        {
            *o = ptyro_core::chats::clip(o, BUDGET / 2);
            n = size(&it);
        }
        if used + n > BUDGET && !cur.is_empty() {
            frames.push(std::mem::take(&mut cur));
            used = 0;
        }
        used += n;
        cur.push(it);
    }
    // The last (or only, maybe empty: a reset to nothing) frame.
    frames.push(cur);
    frames
        .into_iter()
        .enumerate()
        .map(|(i, items)| ServerMsg::ChatLive {
            chat: chat.to_owned(),
            rev,
            reset: reset && i == 0,
            items,
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
            ServerMsg::Screen { .. } | ServerMsg::ChatLive { .. } | ServerMsg::Typed { .. } => true,
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
            | DomainEvent::Alert { .. }
            | DomainEvent::UsageUpdated { .. }
            | DomainEvent::HostLoad { .. }
            | DomainEvent::MachineUpdated { .. }
            | DomainEvent::MachineRemoved { .. }
            | DomainEvent::ChatUpdated { .. }
            | DomainEvent::ChatRemoved { .. }
            | DomainEvent::KbUpdated { .. }
            | DomainEvent::KbProgress { .. }
            | DomainEvent::SetupProgress { .. } => {
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
    fn frames_in_flight_follow_how_far_away_the_client_is() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut f = Inflight::default();
        assert_eq!(f.window(), 1, "one in flight until acks are measured");
        f.sent(1, ms(0));
        assert!(f.full());
        // A terminal nearby: acks in a few ms, one frame in flight, as before.
        f.acked(1, ms(5));
        assert_eq!((f.window(), f.full()), (1, false));
        for i in 2..20 {
            f.sent(i, ms(i * 10));
            f.acked(i, ms(i * 10 + 15));
        }
        assert_eq!(f.window(), 1, "{:?}", f.srtt);
        // A phone on cellular: two, then three.
        for i in 20..40 {
            f.sent(i, ms(i * 1000));
            f.acked(i, ms(i * 1000 + 80));
        }
        assert_eq!(f.window(), 2, "{:?}", f.srtt);
        for i in 40..60 {
            f.sent(i, ms(i * 1000));
            f.acked(i, ms(i * 1000 + 250));
        }
        assert_eq!(f.window(), Inflight::MAX, "{:?}", f.srtt);
        // Never more than three, whatever the client does: one that stops acking gets no more.
        for i in 60..63 {
            assert!(!f.full());
            f.sent(i, ms(70_000));
        }
        assert!(f.full());
        // One ack covers every frame up to it.
        f.acked(61, ms(70_300));
        assert_eq!(f.sent.len(), 1);
        f.acked(1000, ms(70_400));
        assert!(f.sent.is_empty());
        // An old ack (a reconnect's) changes nothing.
        f.acked(3, ms(80_000));
        assert_eq!(f.window(), Inflight::MAX);
    }

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
        let frames = live_frames("c1", 7, true, items);
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
        );
        assert_eq!(one.len(), 1);
        assert_eq!(live_frames("c1", 1, true, Vec::new()).len(), 1, "an empty reset still resets");
    }
}
