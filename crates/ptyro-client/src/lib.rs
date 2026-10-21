//! Async client for the daemon socket (SPEC §6), shared by the TUI, the CLI and tests.
//!
//! [`Client::connect`] performs the handshake and, when asked, starts the daemon first
//! (SPEC §4.1 auto-spawn). Responses are matched to requests by id; everything the daemon
//! pushes (frames, events, goodbye) arrives on [`Client::pushes`].

pub mod remote;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use ptyro_core::paths::Paths;
use ptyro_core::{ClientId, Error, ErrorCode};
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
        }
    }
}

/// Requests awaiting their response; `None` once the connection is gone, so a request made after
/// that fails at once instead of waiting forever.
type Pending = Arc<Mutex<Option<HashMap<u32, oneshot::Sender<Outcome>>>>>;

pub struct Client {
    pub id: ClientId,
    pub daemon_version: String,
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

impl Client {
    pub async fn connect(opts: &ConnectOptions) -> Result<Self, ClientError> {
        if let Some(target) = &opts.remote {
            let mut ssh = remote::ssh(target)?;
            let stream = tokio::io::join(
                ssh.stdout.take().expect("piped stdout"),
                ssh.stdin.take().expect("piped stdin"),
            );
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
        let (id, daemon_version) = match msg {
            ServerMsg::Welcome { client_id, daemon_version, .. } => (client_id, daemon_version),
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
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let Ok(bytes) = encode(&msg, enc) else { continue };
                if sink.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
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
                            let tx = reader_pending
                                .lock()
                                .expect("pending")
                                .as_mut()
                                .and_then(|p| p.remove(&id));
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
                        let tx = reader_pending
                            .lock()
                            .expect("pending")
                            .as_mut()
                            .and_then(|p| p.remove(&id));
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
            *reader_pending.lock().expect("pending") = None;
        });
        Ok(Self {
            id,
            daemon_version,
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
        match self.pending.lock().expect("pending").as_mut() {
            Some(p) => p.insert(id, tx),
            None => return Err(lost()),
        };
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
    match UnixStream::connect(&opts.paths.socket).await {
        Ok(s) => Ok(s),
        Err(e) if opts.autospawn && is_absent(&e) => spawn_daemon(opts).await,
        Err(e) if is_absent(&e) => {
            Err(ClientError::Unavailable(format!("no daemon at {}", opts.paths.socket.display())))
        }
        Err(e) => Err(e.into()),
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| ClientError::Unavailable(format!("cannot start {}: {e}", exe.display())))?;
    let start = Instant::now();
    loop {
        match UnixStream::connect(&opts.paths.socket).await {
            Ok(s) => return Ok(s),
            Err(e) if !is_absent(&e) => return Err(e.into()),
            Err(_) => {}
        }
        if let Ok(Some(status)) = child.try_wait() {
            // Lost a race with another client's daemon? Then the socket is about to appear.
            if let Ok(s) = UnixStream::connect(&opts.paths.socket).await {
                return Ok(s);
            }
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
