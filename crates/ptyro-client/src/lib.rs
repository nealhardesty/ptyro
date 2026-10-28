//! Async client for the daemon socket (SPEC §6), shared by the TUI, the CLI and tests.
//!
//! [`Client::connect`] performs the handshake and, when asked, starts the daemon first
//! (SPEC §4.1 auto-spawn). Responses are matched to requests by id; everything the daemon
//! pushes (frames, events, goodbye) arrives on [`Client::pushes`].

pub mod remote;

use ptyro_core::sync::MutexExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use ptyro_core::paths::Paths;
use ptyro_core::{ClientId, Error, ErrorCode};
use ptyro_protocol::MAX_FRAME;
use ptyro_protocol::{
    ClientInfo, ClientKind, ClientMsg, Encoding, Outcome, OuterTerm, PROTO, Reply, Request,
    ServerMsg, decode, encode, framing,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::Framed;

const SPAWN_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("daemon unavailable: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Rejected(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ClientError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Unavailable(_) | Self::Io(_) => ErrorCode::DaemonUnavailable,
            Self::Rejected(_) | Self::Protocol(_) => ErrorCode::ProtocolMismatch,
        }
    }
}

impl From<ClientError> for Error {
    fn from(e: ClientError) -> Self {
        Error::new(e.code(), e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub paths: Paths,
    pub kind: ClientKind,
    /// Start the daemon when none is listening. Default on unless `PTYRO_NO_AUTOSPAWN=1`.
    pub autospawn: bool,
    /// The binary to start as the daemon; default `$PTYRO_BIN`, else the current executable.
    pub daemon_exe: Option<PathBuf>,
    pub outer: Option<OuterTerm>,
    pub encoding: Encoding,
    /// Ask for zstd-compressed frames (M8.7): worth it over slow links (ssh, M9), not locally.
    /// Default from `PTYRO_COMPRESS=1`; always on with `remote`.
    pub compress: bool,
    /// Reach a daemon on another machine over ssh (M9.1): `[ssh://][user@]host[:port]`.
    /// Default from `PTYRO_REMOTE`.
    pub remote: Option<String>,
    /// A paired device's token (web clients; tests).
    pub token: Option<String>,
    /// Reach the daemon through this command's stdin and stdout instead (MM1.B: `ptyro
    /// machine-bridge …` to another machine's `ptyro serve`). `via` names it in the Hello.
    pub command: Option<(Vec<String>, String)>,
}

impl ConnectOptions {
    pub fn new(paths: Paths, kind: ClientKind) -> Self {
        Self {
            paths,
            kind,
            autospawn: std::env::var("PTYRO_NO_AUTOSPAWN").map_or(true, |v| v != "1"),
            daemon_exe: None,
            outer: None,
            encoding: Encoding::MsgPack,
            compress: std::env::var("PTYRO_COMPRESS").is_ok_and(|v| v == "1"),
            remote: std::env::var("PTYRO_REMOTE").ok().filter(|r| !r.is_empty()),
            token: None,
            command: None,
        }
    }
}

/// Requests awaiting their response; `None` once the connection is gone, so a request made after
/// that fails at once instead of waiting forever.
type Pending = Arc<Mutex<Option<HashMap<u32, oneshot::Sender<Outcome>>>>>;

pub struct Client {
    pub id: ClientId,
    pub daemon_version: String,
    /// What the daemon's machine calls itself (`Welcome.machine`).
    pub machine: Option<String>,
    tx: mpsc::UnboundedSender<ClientMsg>,
    pending: Pending,
    next_id: AtomicU32,
    pushes: Option<mpsc::UnboundedReceiver<ServerMsg>>,
    /// The reader holds the socket's read half; it must stop for the connection to close.
    reader: tokio::task::JoinHandle<()>,
    /// The ssh carrying a remote connection; killed with the client.
    _ssh: Option<tokio::process::Child>,
}

impl Drop for Client {
    fn drop(&mut self) {
        // The writer flushes queued messages and ends when `tx` drops; the reader would otherwise
        // wait forever on a socket the daemon still considers open.
        self.reader.abort();
    }
}

/// A spawned transport's stdout and stdin as one stream (`remote::spawn` and `remote::ssh` pipe
/// both).
fn pipes(
    child: &mut tokio::process::Child,
) -> Result<tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>, ClientError> {
    match (child.stdout.take(), child.stdin.take()) {
        (Some(out), Some(inp)) => Ok(tokio::io::join(out, inp)),
        _ => Err(ClientError::Unavailable("the transport's pipes weren't set up".into())),
    }
}

impl Client {
    pub async fn connect(opts: &ConnectOptions) -> Result<Self, ClientError> {
        if let Some((argv, via)) = &opts.command {
            let mut child = remote::spawn(argv)?;
            let stream = pipes(&mut child)?;
            let stderr = child.stderr.take().map(remote::collect);
            return match Self::handshake(stream, opts, Some(via.clone())).await {
                Ok(mut c) => {
                    c._ssh = Some(child);
                    Ok(c)
                }
                Err(e) => Err(remote::explain_command(e, via, &mut child, stderr).await),
            };
        }
        if let Some(target) = &opts.remote {
            let mut ssh = remote::ssh(target)?;
            let stream = pipes(&mut ssh)?;
            let stderr = ssh.stderr.take().map(remote::collect);
            let via = format!("ssh://{}", remote::host_of(target));
            return match Self::handshake(stream, opts, Some(via)).await {
                Ok(mut c) => {
                    c._ssh = Some(ssh);
                    Ok(c)
                }
                Err(e) => Err(remote::explain(e, target, &mut ssh, stderr).await),
            };
        }
        Self::handshake(connect_raw(opts).await?, opts, None).await
    }

    /// Connect locally on behalf of a client that reached us another way (`ptyro serve`): `via`
    /// goes into the Hello for the daemon's audit trail.
    pub async fn connect_via(opts: &ConnectOptions, via: &str) -> Result<Self, ClientError> {
        Self::handshake(connect_raw(opts).await?, opts, Some(via.to_owned())).await
    }

    async fn handshake<S>(
        stream: S,
        opts: &ConnectOptions,
        via: Option<String>,
    ) -> Result<Self, ClientError>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let mut framed = Framed::new(stream, framing());
        let hello = ClientMsg::Hello {
            proto: PROTO,
            client: ClientInfo {
                kind: opts.kind,
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
                outer: opts.outer.clone(),
                token: opts.token.clone(),
                via: via.clone(),
            },
            features: if opts.compress || via.as_deref().is_some_and(|v| v.starts_with("ssh://")) {
                vec![ptyro_protocol::FEATURE_ZSTD.into()]
            } else {
                vec![]
            },
        };
        let bytes =
            encode(&hello, opts.encoding).map_err(|e| ClientError::Protocol(e.to_string()))?;
        framed.send(bytes).await?;
        let reply = tokio::time::timeout(Duration::from_secs(5), framed.next())
            .await
            .map_err(|_| ClientError::Unavailable("daemon did not answer the handshake".into()))?
            .ok_or_else(|| ClientError::Unavailable("daemon closed the connection".into()))??;
        let (msg, _) =
            decode::<ServerMsg>(&reply).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (id, daemon_version, machine) = match msg {
            ServerMsg::Welcome { client_id, daemon_version, machine, .. } => {
                (client_id, daemon_version, machine)
            }
            ServerMsg::Reject { reason, daemon_version } => {
                return Err(ClientError::Rejected(format!(
                    "daemon {daemon_version} rejected this client: {reason}. Restart it with `ptyro daemon stop` (sessions will end)."
                )));
            }
            other => return Err(ClientError::Protocol(format!("expected welcome, got {other:?}"))),
        };

        let (mut sink, mut stream) = framed.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
        let enc = opts.encoding;
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let writer_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                // A message that can't be sent whole (too big for a frame, or unencodable) fails
                // its own request; the connection and everything else on it carry on.
                let sent = match encode(&msg, enc) {
                    Ok(bytes) if bytes.len() > MAX_FRAME => Err(format!(
                        "the request is too large to send ({} bytes; the limit is {MAX_FRAME})",
                        bytes.len()
                    )),
                    Ok(bytes) => {
                        if sink.send(bytes).await.is_err() {
                            break;
                        }
                        Ok(())
                    }
                    Err(e) => Err(format!("the request can't be encoded: {e}")),
                };
                if let (Err(why), ClientMsg::Request { id, .. }) = (sent, &msg) {
                    let tx = writer_pending.locked().as_mut().and_then(|p| p.remove(id));
                    if let Some(tx) = tx {
                        let _ = tx.send(Outcome::Err { error: Error::invalid(why) });
                    }
                }
            }
        });
        let (push_tx, push_rx) = mpsc::unbounded_channel();
        let reader_pending = Arc::clone(&pending);
        let reader = tokio::spawn(async move {
            while let Some(Ok(bytes)) = stream.next().await {
                let msg = match decode::<ServerMsg>(&bytes) {
                    Ok((msg, _)) => msg,
                    Err(e) => {
                        // A response we can't read still answers its request: fail it rather
                        // than leave the caller waiting forever.
                        if let Some(id) = response_id(&bytes) {
                            let tx = reader_pending.locked().as_mut().and_then(|p| p.remove(&id));
                            if let Some(tx) = tx {
                                let error = Error::new(
                                    ErrorCode::ProtocolMismatch,
                                    format!("unreadable response from the daemon: {e}"),
                                );
                                let _ = tx.send(Outcome::Err { error });
                            }
                        }
                        continue;
                    }
                };
                match msg {
                    ServerMsg::Response { id, outcome } => {
                        let tx = reader_pending.locked().as_mut().and_then(|p| p.remove(&id));
                        if let Some(tx) = tx {
                            let _ = tx.send(outcome);
                        }
                    }
                    other => {
                        let _ = push_tx.send(other);
                    }
                }
            }
            // Connection gone: fail every outstanding request, and every later one.
            *reader_pending.locked() = None;
        });
        Ok(Self {
            id,
            daemon_version,
            machine,
            tx,
            pending,
            next_id: AtomicU32::new(1),
            pushes: Some(push_rx),
            reader,
            _ssh: None,
        })
    }

    /// Send a request and wait for its response.
    pub async fn request(&self, req: Request) -> Result<Reply, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let lost = || Error::new(ErrorCode::DaemonUnavailable, "connection to the daemon was lost");
        match self.pending.locked().as_mut() {
            Some(p) => p.insert(id, tx),
            None => return Err(lost()),
        };
        // A caller that gives up (a timeout drops this future) must not leave its entry behind.
        struct Forget<'a>(&'a Pending, u32);
        impl Drop for Forget<'_> {
            fn drop(&mut self) {
                if let Some(p) = self.0.locked().as_mut() {
                    p.remove(&self.1);
                }
            }
        }
        let _forget = Forget(&self.pending, id);
        self.send(ClientMsg::Request { id, req })?;
        let outcome = rx.await.map_err(|_| lost())?;
        outcome.into()
    }

    /// Send a message that has no response (input, acks, subscriptions).
    pub fn send(&self, msg: ClientMsg) -> Result<(), Error> {
        self.tx.send(msg).map_err(|_| {
            Error::new(ErrorCode::DaemonUnavailable, "connection to the daemon was lost")
        })
    }

    /// Frames, events and goodbyes. Can be taken once.
    pub fn pushes(&mut self) -> mpsc::UnboundedReceiver<ServerMsg> {
        self.pushes.take().expect("pushes already taken")
    }
}

/// The daemon's socket, starting the daemon first when allowed and none is listening. `ptyro
/// proxy` bridges this to ssh.
pub async fn connect_raw(opts: &ConnectOptions) -> Result<UnixStream, ClientError> {
    let stream = match UnixStream::connect(&opts.paths.socket).await {
        Ok(s) => Ok(s),
        Err(e) if opts.autospawn && is_absent(&e) => spawn_daemon(opts).await,
        Err(e) if is_absent(&e) => {
            Err(ClientError::Unavailable(format!("no daemon at {}", opts.paths.socket.display())))
        }
        Err(e) => Err(e.into()),
    }?;
    ours(&stream, opts)?;
    Ok(stream)
}

/// Whether the process on the other end of the socket is ours: tokens, typed input and prompts go
/// down it, so a socket another user made in a shared directory must not be talked to.
fn ours(stream: &UnixStream, opts: &ConnectOptions) -> Result<(), ClientError> {
    match stream.peer_cred() {
        Ok(c) if c.uid() == ptyro_core::paths::current_uid() => Ok(()),
        Ok(c) => Err(ClientError::Unavailable(format!(
            "{} is another user's (uid {}), not yours: refusing to talk to it. Remove it or set PTYRO_HOME / PTYRO_SOCKET to a private place",
            opts.paths.socket.display(),
            c.uid()
        ))),
        // A platform that can't say: nothing to check against.
        Err(_) => Ok(()),
    }
}

/// The id of a `response` frame that doesn't decode as a whole (e.g. a newer daemon's reply).
fn response_id(bytes: &[u8]) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        t: String,
        id: Option<u32>,
    }
    let (env, _) = decode::<Envelope>(bytes).ok()?;
    (env.t == "response").then_some(env.id).flatten()
}

fn is_absent(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
}

/// Start `ptyro daemon start --spawned` and wait until it accepts connections. The daemon
/// detaches itself (`setsid`) before starting its runtime; we never fork a tokio process.
async fn spawn_daemon(opts: &ConnectOptions) -> Result<UnixStream, ClientError> {
    let exe = match &opts.daemon_exe {
        Some(p) => p.clone(),
        None => match std::env::var_os("PTYRO_BIN") {
            Some(p) => PathBuf::from(p),
            None => std::env::current_exe()?,
        },
    };
    let mut child = std::process::Command::new(&exe)
        .args(["daemon", "start", "--spawned"])
        // The daemon belongs to no session, even when an agent's CLI call started it: what it
        // runs must not be taken for that agent (who's calling, SPEC §17).
        .env_remove("PTYRO_SESSION_ID")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ClientError::Unavailable(format!("cannot start {}: {e}", exe.display())))?;
    let start = Instant::now();
    let mut exited: Option<(Instant, std::process::ExitStatus)> = None;
    loop {
        match UnixStream::connect(&opts.paths.socket).await {
            Ok(s) => {
                // The daemon is ours to reap when it ends; nobody else will.
                if exited.is_none() {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                return Ok(s);
            }
            Err(e) if !is_absent(&e) => return Err(e.into()),
            Err(_) => {}
        }
        if exited.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            // It may have lost the race with another client's daemon, which has the lock but
            // hasn't bound the socket yet: keep waiting for that socket, a little while.
            exited = Some((Instant::now(), status));
        }
        if let Some((at, status)) = exited
            && at.elapsed() > Duration::from_secs(3)
        {
            return Err(ClientError::Unavailable(format!(
                "daemon exited during startup ({status}); see logs in {}",
                opts.paths.log_dir().display()
            )));
        }
        if start.elapsed() > SPAWN_WAIT {
            return Err(ClientError::Unavailable(format!(
                "daemon did not start within {}s; see logs in {}",
                SPAWN_WAIT.as_secs(),
                opts.paths.log_dir().display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_protocol::MAX_FRAME;

    fn opts(dir: &std::path::Path) -> ConnectOptions {
        let home = dir.to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        ConnectOptions::new(paths, ClientKind::Test)
    }

    /// A daemon that only says welcome and answers every request `Done`.
    fn fake_daemon(b: tokio::io::DuplexStream) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut f = Framed::new(b, framing());
            let _hello = f.next().await;
            let welcome = ServerMsg::Welcome {
                proto: PROTO,
                daemon_version: "test".into(),
                client_id: ClientId(1),
                features: vec![],
                started_ms: 0,
                scope: None,
                machine: None,
            };
            f.send(encode(&welcome, Encoding::MsgPack).unwrap()).await.unwrap();
            while let Some(Ok(bytes)) = f.next().await {
                if let Ok((ClientMsg::Request { id, .. }, _)) = decode::<ClientMsg>(&bytes) {
                    let done =
                        ServerMsg::Response { id, outcome: Outcome::Ok { reply: Reply::Done } };
                    f.send(encode(&done, Encoding::MsgPack).unwrap()).await.unwrap();
                }
            }
        })
    }

    #[tokio::test]
    async fn a_request_too_big_for_a_frame_fails_alone_and_the_connection_lives() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = tokio::io::duplex(1 << 16);
        let _server = fake_daemon(b);
        let c = Client::handshake(a, &opts(d.path()), None).await.unwrap();
        let big = Request::Rename { addr: "x".repeat(MAX_FRAME + 1), name: "n".into() };
        let e = tokio::time::timeout(Duration::from_secs(10), c.request(big))
            .await
            .expect("answered, not hung")
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid);
        assert!(e.message.contains("too large"), "{}", e.message);
        // The next request on the same connection works, and nothing is left pending.
        let r = c.request(Request::Rename { addr: "a".into(), name: "b".into() }).await.unwrap();
        assert!(matches!(r, Reply::Done));
        assert!(c.pending.locked().as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_caller_that_gives_up_leaves_no_pending_entry() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = tokio::io::duplex(1 << 16);
        // A daemon that never answers requests.
        let _server = tokio::spawn(async move {
            let mut f = Framed::new(b, framing());
            let _ = f.next().await;
            let welcome = ServerMsg::Welcome {
                proto: PROTO,
                daemon_version: "t".into(),
                client_id: ClientId(1),
                features: vec![],
                started_ms: 0,
                scope: None,
                machine: None,
            };
            f.send(encode(&welcome, Encoding::MsgPack).unwrap()).await.unwrap();
            while f.next().await.is_some() {}
        });
        let c = Client::handshake(a, &opts(d.path()), None).await.unwrap();
        let r = tokio::time::timeout(Duration::from_millis(100), c.request(Request::List)).await;
        assert!(r.is_err(), "timed out");
        assert!(c.pending.locked().as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_socket_of_ours_is_talked_to_and_losing_the_start_race_keeps_waiting() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let mut o = opts(d.path());
        std::fs::create_dir_all(o.paths.socket.parent().unwrap()).unwrap();
        // A "daemon" that exits at once, as one that finds another holding the lock does; the
        // other's socket appears a moment later.
        let exe = d.path().join("quick-exit");
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        o.autospawn = true;
        o.daemon_exe = Some(exe);
        let sock = o.paths.socket.clone();
        let listener = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let l = tokio::net::UnixListener::bind(&sock).unwrap();
            let _ = l.accept().await;
        });
        let s = connect_raw(&o).await.expect("waited for the socket the winner bound");
        // It is ours (the same user made it), so it passed the owner check.
        assert_eq!(s.peer_cred().unwrap().uid(), ptyro_core::paths::current_uid());
        drop(s);
        listener.abort();
    }

    #[test]
    fn unreadable_responses_still_name_their_request() {
        let bad =
            br#"{"t":"response","id":7,"outcome":{"t":"ok","reply":{"t":"from_the_future"}}}"#;
        assert!(decode::<ServerMsg>(bad).is_err());
        assert_eq!(response_id(bad), Some(7));
        let packed = encode(
            &ServerMsg::Response { id: 9, outcome: Outcome::Err { error: Error::not_found("x") } },
            Encoding::MsgPack,
        )
        .unwrap();
        assert_eq!(response_id(&packed), Some(9));
        assert_eq!(response_id(br#"{"t":"screen","id":1}"#), None);
    }
}
