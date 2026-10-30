//! `ptyro serve` (SPEC §16.3–16.4, M9.2–M9.3): the web app and the protocol over WebSocket, for
//! paired devices. It is its own process, so the daemon stays a local socket and a crash here
//! takes nothing else down.
//!
//! - `GET /`: the web app (embedded; no third-party scripts, a strict CSP).
//! - `GET /ws`: one WebSocket message per protocol frame. The first must be a `Hello` with a
//!   device token; this process rewrites its kind (`web`) and origin (`via`), so a browser can't
//!   pass for a local client, and the daemon checks the token before anything else reaches it.
//! - `POST /api/request`: one request with `Authorization: Bearer <token>` (notification
//!   actions, scripts).
//! - `POST /api/pair`: `{"code"}` from `ptyro pair`'s link, traded once for the device's token.
//!
//! Always HTTPS (phones need it for notifications and installing the app), with a certificate
//! ptyro makes or one of your own. `[remote] access` says on which interfaces it listens
//! (`localhost`, `tailscale`, `open`), `[remote] port` on which port.
//!
//! Every request must name a host it expects (an IP address, localhost, `[remote] public_url`'s
//! host, this machine's tailnet name), so a DNS-rebinding page can't reach it under a name of its
//! own.

use ptyro_core::sync::MutexExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use ptyro_core::Error;
use ptyro_core::config::{Access, Config};
use ptyro_protocol::{ClientKind, ClientMsg, Encoding, ServerMsg, decode, encode, framing};
use tokio::net::TcpListener;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_util::codec::Framed;

use crate::cli::Ctx;

type Body = Full<Bytes>;

/// The web app, built from `web/src` by `make web` (committed, so a plain `cargo build` has it).
macro_rules! js {
    ($name:literal) => {
        (
            concat!("/", $name, ".js"),
            "text/javascript; charset=utf-8",
            include_bytes!(concat!("../../../web/dist/", $name, ".js")),
        )
    };
}

const ASSETS: &[(&str, &str, &[u8])] = &[
    ("/", "text/html; charset=utf-8", include_bytes!("../../../web/dist/index.html")),
    ("/index.html", "text/html; charset=utf-8", include_bytes!("../../../web/dist/index.html")),
    js!("app"),
    js!("color"),
    js!("conn"),
    js!("data"),
    js!("flow"),
    js!("grid"),
    js!("keys"),
    js!("kit"),
    js!("machine"),
    js!("machines"),
    js!("chats"),
    js!("chatkit"),
    js!("projects"),
    js!("qr"),
    js!("review"),
    js!("runs"),
    js!("screen"),
    js!("sessions"),
    js!("settings"),
    js!("panel"),
    js!("cfgkit"),
    js!("term"),
    js!("pad"),
    js!("notes"),
    ("/sw.js", "text/javascript; charset=utf-8", include_bytes!("../../../web/dist/sw.js")),
    ("/style.css", "text/css; charset=utf-8", include_bytes!("../../../web/dist/style.css")),
    (
        "/manifest.webmanifest",
        "application/manifest+json",
        include_bytes!("../../../web/dist/manifest.webmanifest"),
    ),
    ("/icon.svg", "image/svg+xml", include_bytes!("../../../web/dist/icon.svg")),
    ("/icon-192.png", "image/png", include_bytes!("../../../web/dist/icon-192.png")),
    ("/icon-512.png", "image/png", include_bytes!("../../../web/dist/icon-512.png")),
    (
        "/icon-maskable-512.png",
        "image/png",
        include_bytes!("../../../web/dist/icon-maskable-512.png"),
    ),
    (
        "/apple-touch-icon.png",
        "image/png",
        include_bytes!("../../../web/dist/apple-touch-icon.png"),
    ),
];

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
    connect-src 'self'; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'none'; \
    frame-ancestors 'none'";

/// Failed device logins per address: this many in the window locks the address out for it.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(600);
/// Logins from one address still waiting for the daemon's answer: more are refused, so
/// parallel guesses can't outrun the lockout.
const MAX_PENDING: u32 = 4;
/// Logins from every address together, still waiting for the daemon's answer: each can hold a
/// large first message in memory before it proves who it is.
const MAX_PENDING_TOTAL: u32 = 16;
/// Open connections in all.
const MAX_CONNECTIONS: usize = 256;
/// Open connections from one address (one machine can't take every slot); behind a proxy on this
/// machine every client shares the loopback address, so that one gets more.
const MAX_PER_ADDR: u32 = 32;
const MAX_PER_PROXY: u32 = 192;
/// The whole body of a request, once its headers are in: a client that sends a length and then
/// stops can't hold a connection forever.
const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// [`BODY_TIMEOUT`], or `PTYRO_TEST_BODY_TIMEOUT_MS` (a test that waits for it).
fn body_timeout() -> Duration {
    ptyro_core::test_knob("PTYRO_TEST_BODY_TIMEOUT_MS")
        .and_then(|v| v.parse().ok())
        .map_or(BODY_TIMEOUT, Duration::from_millis)
}
/// A WebSocket that has said nothing, not even a pong to our pings, for this long is gone.
const WS_SILENCE: Duration = Duration::from_secs(70);

struct State {
    ctx_paths: ptyro_core::paths::Paths,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    pending: Mutex<HashMap<IpAddr, u32>>,
    /// Open connections per peer address.
    open: Mutex<HashMap<IpAddr, u32>>,
    /// Host names (lowercase, no port) requests may carry besides IP addresses and localhost.
    hosts: Vec<String>,
    /// The CA phones trust once (`/ca.crt`), when serving HTTPS.
    ca_pem: Option<String>,
    /// The certificates being served, when serving HTTPS: their stamp goes into `/sw.js`.
    served: Option<Arc<Mutex<crate::tls::Served>>>,
    /// The daemon is on another machine (`--remote`): host operations would act on this one.
    remote: bool,
}

/// One login in progress (see [`State::begin`]); dropping it ends it.
struct Pending {
    st: Arc<State>,
    ip: IpAddr,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut p = self.st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = p.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                p.remove(&self.ip);
            }
        }
    }
}

/// One open connection from an address (see [`State::open_from`]); dropping it frees the place.
struct Open {
    st: Arc<State>,
    ip: IpAddr,
}

impl Drop for Open {
    fn drop(&mut self) {
        let mut o = self.st.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(n) = o.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                o.remove(&self.ip);
            }
        }
    }
}

/// Who a failed login counts against. An IPv6 attacker has a whole /64 of addresses to rotate
/// through, so the network counts, not the address.
fn lockout_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) if v.to_ipv4_mapped().is_none() => {
            let mut o = v.octets();
            o[8..].fill(0);
            IpAddr::V6(o.into())
        }
        other => other,
    }
}

impl State {
    /// A place for a connection from `ip`, unless it has its share already.
    fn open_from(self: &Arc<Self>, ip: IpAddr) -> Option<Open> {
        let cap = if ip.is_loopback() { MAX_PER_PROXY } else { MAX_PER_ADDR };
        let mut o = self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let n = o.entry(ip).or_insert(0);
        if *n >= cap {
            return None;
        }
        *n += 1;
        Some(Open { st: Arc::clone(self), ip })
    }

    /// Start a login from `ip`, unless it has [`MAX_PENDING`] going already, or there are
    /// [`MAX_PENDING_TOTAL`] in all.
    fn begin(self: &Arc<Self>, ip: IpAddr) -> Option<Pending> {
        let ip = lockout_key(ip);
        let mut p = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if p.values().sum::<u32>() >= MAX_PENDING_TOTAL {
            return None;
        }
        let n = p.entry(ip).or_insert(0);
        if *n >= MAX_PENDING {
            return None;
        }
        *n += 1;
        Some(Pending { st: Arc::clone(self), ip })
    }

    fn locked_out(&self, ip: IpAddr) -> bool {
        let ip = lockout_key(ip);
        let f = self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.get(&ip).is_some_and(|(n, t)| *n >= MAX_FAILURES && t.elapsed() < FAILURE_WINDOW)
    }

    fn failed(&self, ip: IpAddr) {
        let ip = lockout_key(ip);
        let mut f = self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
        if e.0 == MAX_FAILURES {
            tracing::warn!(%ip, "locked out for {}m after {MAX_FAILURES} failed device logins", FAILURE_WINDOW.as_secs() / 60);
        }
    }
}

/// Where each `access` listens: the loopback address; plus this machine's Tailscale addresses
/// for `tailscale`; every IPv4 interface for `open`.
fn addresses(access: Access, port: u16) -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::new(
        if access == Access::Open {
            IpAddr::from([0, 0, 0, 0])
        } else {
            IpAddr::from([127, 0, 0, 1])
        },
        port,
    )];
    if access == Access::Tailscale {
        out.extend(crate::remote::tailscale_ips().into_iter().map(|ip| SocketAddr::new(ip, port)));
    }
    out
}

pub async fn run(
    ctx: &Ctx,
    access: Option<Access>,
    port: Option<u16>,
    own: Option<(std::path::PathBuf, std::path::PathBuf)>,
    plain: bool,
    managed: bool,
) -> Result<i32, Error> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PTYRO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
    let access = access.unwrap_or(config.remote.access);
    let port = port.unwrap_or(config.remote.port);
    // Plain HTTP exists for the tour and the tests, which open a page on this computer only
    // (a browser counts loopback as secure); it is not a setting, and nothing else gets it.
    if plain && access != Access::Localhost {
        return Err(Error::invalid(
            "plain HTTP is only for this computer (`--access localhost`); the web app is HTTPS otherwise",
        ));
    }
    let addrs = addresses(access, port);
    let tailnet = tailnet_names();
    let mut cert_names: Vec<String> = tailnet.clone();
    cert_names.extend(config.remote.public_url.iter().filter_map(|u| url_host(u)));
    cert_names
        .extend(addrs.iter().filter(|a| !a.ip().is_unspecified()).map(|a| a.ip().to_string()));
    cert_names.extend(crate::remote::tailscale_ips().iter().map(ToString::to_string));
    let tls_dir = crate::tls::dir(&ctx.paths.data);
    // Your own certificate (flags, else config), or the one ptyro makes. Half of a pair is a
    // mistake, said here rather than in the file's check: the panel sets them one at a time.
    if own.is_none() && config.remote.tls_cert.is_some() != config.remote.tls_key.is_some() {
        let (set, missing) = if config.remote.tls_cert.is_some() {
            ("tls_cert", "tls_key")
        } else {
            ("tls_key", "tls_cert")
        };
        return Err(Error::invalid(format!(
            "[remote] {set} is set but {missing} isn't: set {missing} too (`ptyro config set              remote.{missing} PATH`), or remove {set} to use the certificate ptyro makes"
        )));
    }
    let own = own.or_else(|| {
        let (c, k) = (config.remote.tls_cert.as_ref()?, config.remote.tls_key.as_ref()?);
        let path = ptyro_core::config::expand_home;
        Some((path(c), path(k)))
    });
    // Over Tailscale, with no certificate of your own, the tailnet name may get Tailscale's.
    let tailnet_name = (!plain && own.is_none() && access == Access::Tailscale)
        .then(|| ptyro_core::config::tailnet_cert_domain().ok())
        .flatten();
    // A Tailscale certificate from an earlier run answers at once; the task below renews it.
    let disk_tailnet = tailnet_name
        .as_deref()
        .and_then(|n| crate::tls::load_tailnet(&tls_dir, n, time::OffsetDateTime::now_utc()));
    let tls = match (&own, plain) {
        (_, true) => None,
        (Some((cert, key)), false) => Some(crate::tls::Tls {
            config: crate::tls::from_files(cert, key)?,
            ca_pem: String::new(),
            ca_fingerprint: String::new(),
            names: Vec::new(),
        }),
        (None, false) => {
            let names = crate::tls::names_for(&cert_names);
            Some(crate::tls::load_or_make(
                &tls_dir,
                &names,
                time::OffsetDateTime::now_utc(),
                disk_tailnet.as_ref(),
            )?)
        }
    };
    let own_cert = own.is_some() && !plain;
    // Listening means ready: the certificate exists before anyone can connect. The first address
    // is the one that must work; a Tailscale address that isn't up yet is picked up below.
    let mut listeners = Vec::new();
    for (i, addr) in addrs.iter().enumerate() {
        match TcpListener::bind(addr).await {
            Ok(l) => listeners.push(l),
            Err(e) if i == 0 => {
                return Err(Error::internal(format!(
                    "listen on {addr}: {e} (is another program using port {port}? `ptyro config set remote.port` picks another)"
                )));
            }
            Err(e) => tracing::warn!("listen on {addr}: {e}"),
        }
    }
    let mut shown = config.clone();
    shown.remote.access = access;
    shown.remote.port = port;
    let base = crate::remote::base_url(&shown, tls.is_some());
    // Names answered to now: a name the certificate held once (`tls.json` keeps them so it need
    // not be remade) doesn't stay allowed after it is no longer configured.
    let mut hosts: Vec<String> =
        config.remote.public_url.iter().chain([&base]).filter_map(|u| url_host(u)).collect();
    hosts.extend(tailnet);
    hosts.extend(crate::tls::names_for(&cert_names));
    let ca = tls.as_ref().filter(|_| !own_cert).map(|t| t.ca_fingerprint.as_str());
    let cert_kind = match (&tls, own_cert, &disk_tailnet) {
        (None, ..) => "",
        (_, true, _) => "own",
        (_, _, Some(_)) => "tailscale",
        _ => "ptyro",
    };
    crate::remote::record_serve(
        &ctx.paths,
        &base,
        ca,
        addrs[0],
        cert_kind,
        disk_tailnet.as_ref().map(|t| t.name.as_str()),
    );
    let scheme = if tls.is_some() { "https" } else { "http" };
    let on = listeners
        .iter()
        .filter_map(|l| l.local_addr().ok())
        .map(|a| format!("{scheme}://{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    match &tls {
        Some(_) if own_cert => eprintln!(
            "ptyro serve: listening on {on} ({access}: {}), with your certificate; phones open {base} \
             (pair one with `ptyro pair`)",
            access.blurb()
        ),
        Some(_) if cert_kind == "tailscale" => eprintln!(
            "ptyro serve: listening on {on} ({access}: {}), with Tailscale's certificate for its tailnet name; \
             phones open {base} (pair one with `ptyro pair`)",
            access.blurb()
        ),
        Some(t) => eprintln!(
            "ptyro serve: listening on {on} ({access}: {}); phones open {base} (pair one with \
             `ptyro pair`; the first time, trust ptyro's certificate from {base}/ca.crt, \
             SHA-256 {})",
            access.blurb(),
            t.ca_fingerprint
        ),
        None => eprintln!(
            "ptyro serve: listening on {on} (plain HTTP, this computer only); open {base}"
        ),
    }
    let acceptor = tls.as_ref().map(|t| Arc::new(Mutex::new(t.config.clone())));
    let live = Arc::new(Mutex::new(disk_tailnet));
    if let Some(current) = &acceptor {
        if own.is_none() && access == Access::Tailscale {
            tokio::spawn(tailnet_cert_task(
                ctx.paths.clone(),
                tls_dir.clone(),
                cert_names.clone(),
                Arc::clone(current),
                Arc::clone(&live),
            ));
        }
        // Renewed while it runs: a fresh certificate before this one ends.
        let (current, dir, names) = (Arc::clone(current), tls_dir.clone(), cert_names.clone());
        let own = own.clone();
        let live = Arc::clone(&live);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
                let fresh = match &own {
                    Some((cert, key)) => crate::tls::from_files(cert, key),
                    None => {
                        let names = crate::tls::names_for(&names);
                        let tailnet = live.locked().clone();
                        crate::tls::load_or_make(
                            &dir,
                            &names,
                            time::OffsetDateTime::now_utc(),
                            tailnet.as_ref(),
                        )
                        .map(|t| t.config)
                    }
                };
                match fresh {
                    Ok(c) => *current.locked() = c,
                    Err(e) => tracing::warn!("renewing the certificate: {}", e.message),
                }
            }
        });
    }
    let state = Arc::new(State {
        ctx_paths: ctx.paths.clone(),
        failures: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        open: Mutex::new(HashMap::new()),
        hosts,
        ca_pem: tls.as_ref().filter(|_| !own_cert).map(|t| t.ca_pem.clone()),
        served: acceptor.clone(),
        remote: ctx.remote.is_some(),
    });
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    if managed {
        tokio::spawn(exit_with_daemon(ctx.paths.socket.clone()));
    }
    let mut bound: Vec<SocketAddr> = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();
    for listener in listeners {
        tokio::spawn(accept_loop(
            listener,
            Arc::clone(&state),
            Arc::clone(&slots),
            acceptor.clone(),
        ));
    }
    // Tailscale may come up (or change address) after this starts: bind what appears, so the
    // phone doesn't need a restart of anything.
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if access != Access::Tailscale {
            continue;
        }
        for addr in addresses(access, port) {
            if bound.contains(&addr) {
                continue;
            }
            if let Ok(l) = TcpListener::bind(addr).await {
                eprintln!("ptyro serve: also listening on {scheme}://{addr} (Tailscale is up)");
                bound.push(addr);
                tokio::spawn(accept_loop(
                    l,
                    Arc::clone(&state),
                    Arc::clone(&slots),
                    acceptor.clone(),
                ));
            }
        }
    }
}

/// Accept connections on one listener, for ever.
async fn accept_loop(
    listener: TcpListener,
    state: Arc<State>,
    slots: Arc<tokio::sync::Semaphore>,
    acceptor: Option<Arc<Mutex<crate::tls::Served>>>,
) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // Over the limit: drop it (a WebSocket keeps its slot until it closes).
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            tracing::debug!(%peer, "too many connections");
            continue;
        };
        let slot = Arc::new(slot);
        let st = Arc::clone(&state);
        let Some(open) = st.open_from(peer.ip()) else {
            tracing::debug!(%peer, "too many connections from one address");
            continue;
        };
        let tls = acceptor
            .as_ref()
            .map(|c| tokio_rustls::TlsAcceptor::from(Arc::clone(&c.locked().config)));
        tokio::spawn(async move {
            let _open = open;
            let svc = service_fn(move |req| handle(req, Arc::clone(&st), peer, Arc::clone(&slot)));
            let http = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10))
                .clone();
            match tls {
                Some(tls) => {
                    let Ok(Ok(stream)) =
                        tokio::time::timeout(Duration::from_secs(10), tls.accept(stream)).await
                    else {
                        tracing::debug!(%peer, "TLS handshake failed");
                        return;
                    };
                    let _ = http.serve_connection(TokioIo::new(stream), svc).with_upgrades().await;
                }
                None => {
                    let _ = http.serve_connection(TokioIo::new(stream), svc).with_upgrades().await;
                }
            }
        });
    }
}

/// Started by the daemon: leave when it is gone (it is our parent), or its socket has been
/// missing for 30 s.
async fn exit_with_daemon(socket: std::path::PathBuf) {
    // The daemon that started us is our parent: when it is gone (killed, crashed, replaced) the
    // next one starts a server of its own, and ours would hold the port for good. A crash leaves
    // the socket file behind, so the file alone can't say.
    let parent = std::os::unix::process::parent_id();
    let mut missing = 0;
    for tick in 0u64.. {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if std::os::unix::process::parent_id() != parent {
            std::process::exit(0);
        }
        if tick % 5 == 4 {
            missing = if socket.exists() { 0 } else { missing + 1 };
            if missing >= 6 {
                std::process::exit(0);
            }
        }
    }
}

fn respond(status: StatusCode, ctype: &str, body: impl Into<Bytes>) -> Response<Body> {
    let mut r = Response::new(Full::new(body.into()));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(ctype).unwrap_or(HeaderValue::from_static("text/plain")),
    );
    secure(h);
    r
}

fn secure(h: &mut hyper::HeaderMap) {
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    // Answers carry tokens and screens: nothing keeps them (assets say otherwise for themselves).
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

/// Gets Tailscale's certificate for this machine's name, now and every 12 hours (`tailscale cert`
/// renews it when it is near its end), and starts answering the tailnet name with it as soon as
/// it is there. Anything that goes wrong leaves ptyro's own certificate answering and says why in
/// `ptyro web` and `ptyro doctor`.
async fn tailnet_cert_task(
    paths: ptyro_core::paths::Paths,
    dir: std::path::PathBuf,
    names: Vec<String>,
    current: Arc<Mutex<crate::tls::Served>>,
    live: Arc<Mutex<Option<crate::tls::TailnetCert>>>,
) {
    loop {
        let (d, p) = (dir.clone(), live.locked().clone());
        let outcome = tokio::task::spawn_blocking(move || {
            let name = ptyro_core::config::tailnet_cert_domain()?;
            crate::tls::fetch_tailnet(&d, &name)?;
            crate::tls::load_tailnet(&d, &name, time::OffsetDateTime::now_utc())
                .ok_or_else(|| format!("tailscale cert wrote no usable certificate for {name}"))
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        let wait = match outcome {
            Ok(cert) => {
                let names = crate::tls::names_for(&names);
                let now = time::OffsetDateTime::now_utc();
                match crate::tls::load_or_make(&dir, &names, now, Some(&cert)) {
                    Ok(t) => {
                        *current.locked() = t.config;
                        if p.is_none_or(|old| old.not_after != cert.not_after) {
                            eprintln!(
                                "ptyro serve: using Tailscale's certificate for {}; phones on that address need nothing installed",
                                cert.name
                            );
                        }
                        crate::remote::record_cert(&paths, "tailscale", Some(&cert.name), None);
                        *live.locked() = Some(cert);
                        Duration::from_secs(12 * 3600)
                    }
                    Err(e) => {
                        crate::remote::record_cert(&paths, "ptyro", None, Some(&e.message));
                        Duration::from_secs(600)
                    }
                }
            }
            Err(why) => {
                // Kept while it is good; a certificate that ended is not served.
                let usable = live
                    .locked()
                    .as_ref()
                    .is_some_and(|c| c.not_after > time::OffsetDateTime::now_utc());
                eprintln!("ptyro serve: no Tailscale certificate: {why}");
                let kind = if usable { "tailscale" } else { "ptyro" };
                let name = live.locked().as_ref().map(|c| c.name.clone());
                crate::remote::record_cert(&paths, kind, name.as_deref(), Some(&why));
                Duration::from_secs(300)
            }
        };
        tokio::time::sleep(wait).await;
    }
}

/// This machine's MagicDNS names (`box.tailnet.ts.net` and `box`), if Tailscale is up.
fn tailnet_names() -> Vec<String> {
    let Ok(out) = std::process::Command::new("tailscale").args(["status", "--json"]).output()
    else {
        return Vec::new();
    };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let name = v["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_lowercase();
    match name.split_once('.') {
        Some((short, _)) => vec![short.to_owned(), name],
        None => Vec::new(),
    }
}

/// The host of `scheme://host[:port]/…`, lowercase.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    Some(strip_port(authority).to_ascii_lowercase()).filter(|h| !h.is_empty())
}

/// `host:port` → `host`; `[v6]:port` → `v6`.
fn strip_port(h: &str) -> &str {
    if let Some(v6) = h.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("");
    }
    match h.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => h,
    }
}

/// The DNS-rebinding defence: a `Host` header must be an IP address, `localhost`, or a name
/// this server answers to. A rebinding page's requests carry the attacker's own name. (No
/// `Host` at all isn't a browser.)
fn host_ok(host: Option<&str>, names: &[String]) -> bool {
    let Some(host) = host else { return true };
    let h = strip_port(host).trim_end_matches('.').to_ascii_lowercase();
    h.parse::<IpAddr>().is_ok()
        || h == "localhost"
        || h.ends_with(".localhost")
        || names.contains(&h)
}

/// Who connected, for the audit trail: the peer (a proxy's `X-Forwarded-For` is never believed:
/// ptyro is always the one that speaks HTTPS to the client).
fn via(peer: SocketAddr) -> (String, IpAddr) {
    (format!("ws:{peer}"), peer.ip())
}

/// The CSRF defence for what changes state or opens a session: a request that says which page it
/// came from (`Origin`, always sent by a browser for a WebSocket and a POST) must come from this
/// server's own page, or a page the visitor happens to have open could use it. Requests with no
/// `Origin` are not browsers' (a script, `curl`, the notification actions).
fn origin_ok(origin: Option<&str>, host: Option<&str>, names: &[String]) -> bool {
    let Some(origin) = origin else { return true };
    let Some((_, authority)) = origin.split_once("://") else { return false };
    let authority = authority.trim_end_matches('/').to_ascii_lowercase();
    if host.is_some_and(|h| authority == h.trim_end_matches('.').to_ascii_lowercase()) {
        return true;
    }
    // Behind a proxy the Host may be rewritten: the names this server is configured for count.
    names.contains(&strip_port(&authority).to_owned())
}

async fn handle(
    req: Request<Incoming>,
    st: Arc<State>,
    peer: SocketAddr,
    slot: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Response<Body>, Infallible> {
    let host = req.headers().get(header::HOST).map(|v| v.to_str().unwrap_or("\0"));
    if !host_ok(host, &st.hosts) {
        tracing::debug!(%peer, ?host, "unexpected Host");
        return Ok(respond(StatusCode::MISDIRECTED_REQUEST, "text/plain", "unexpected Host"));
    }
    let path = req.uri().path().to_owned();
    // Cross-site: refused before it can count as anyone's failed login.
    if (path == "/ws" || req.method() == Method::POST)
        && !origin_ok(
            req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()),
            host,
            &st.hosts,
        )
    {
        tracing::debug!(%peer, "cross-site request refused");
        return Ok(respond(StatusCode::FORBIDDEN, "text/plain", "cross-site request refused"));
    }
    let (via, ip) = via(peer);
    let busy =
        || respond(StatusCode::TOO_MANY_REQUESTS, "text/plain", "too many logins; try later");
    Ok(match (req.method(), path.as_str()) {
        (&Method::GET, "/ws") => match st.begin(ip) {
            Some(login) if !st.locked_out(ip) => upgrade(req, st, via, ip, login, slot),
            _ => busy(),
        },
        (&Method::POST, "/api/request") => match st.begin(ip) {
            Some(login) if !st.locked_out(ip) => api_request(req, &st, via, ip, login).await,
            _ => busy(),
        },
        (&Method::POST, "/api/host") => match st.begin(ip) {
            Some(login) if !st.locked_out(ip) => api_host(req, &st, via, ip, login).await,
            _ => busy(),
        },
        (&Method::POST, "/api/pair") => match st.begin(ip) {
            Some(login) if !st.locked_out(ip) => api_pair(req, &st, via, ip, login).await,
            _ => busy(),
        },
        (&Method::GET, "/healthz") => respond(StatusCode::OK, "text/plain", "ok"),
        // The CA this machine's certificate comes from, for a phone to trust once.
        (&Method::GET, "/ca.crt") => match &st.ca_pem {
            Some(pem) => {
                let mut r = respond(StatusCode::OK, "application/x-x509-ca-cert", pem.clone());
                r.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("attachment; filename=\"ptyro-ca.crt\""),
                );
                r
            }
            None => {
                respond(StatusCode::NOT_FOUND, "text/plain", "this ptyro serve speaks plain HTTP")
            }
        },
        (&Method::GET | &Method::HEAD, p) => match ASSETS.iter().find(|(a, _, _)| *a == p) {
            Some((_, ctype, data)) => {
                let body = match &st.served {
                    // A new certificate makes a new service worker (see `tls::Served`).
                    Some(s) if p == "/sw.js" => {
                        let stamp = format!("\n// certificate {}\n", s.locked().stamp);
                        Bytes::from([data, stamp.as_bytes()].concat())
                    }
                    _ => Bytes::from_static(data),
                };
                let mut r = respond(StatusCode::OK, ctype, body);
                // The shell and the service worker must update; icons may be cached.
                let cache = if p.ends_with(".png") || p.ends_with(".svg") {
                    "public, max-age=86400"
                } else {
                    "no-cache"
                };
                r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
                r
            }
            None => respond(StatusCode::NOT_FOUND, "text/plain", "not found"),
        },
        _ => respond(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "method not allowed"),
    })
}

fn upgrade(
    req: Request<Incoming>,
    st: Arc<State>,
    via: String,
    ip: IpAddr,
    login: Pending,
    slot: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> Response<Body> {
    let headers = req.headers();
    let is_ws = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let Some(key) = headers.get(header::SEC_WEBSOCKET_KEY).map(|k| derive_accept_key(k.as_bytes()))
    else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a WebSocket upgrade");
    };
    if !is_ws {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a WebSocket upgrade");
    }
    tokio::spawn(async move {
        let _slot = slot;
        match hyper::upgrade::on(req).await {
            Ok(up) => {
                // What a browser sends is small; what it is sent (replies) isn't limited here.
                let cfg = WebSocketConfig::default()
                    .max_message_size(Some(ptyro_protocol::MAX_CLIENT_MESSAGE))
                    .max_frame_size(Some(ptyro_protocol::MAX_CLIENT_MESSAGE));
                let ws =
                    WebSocketStream::from_raw_socket(TokioIo::new(up), Role::Server, Some(cfg))
                        .await;
                bridge(ws, st, via, ip, login).await;
            }
            Err(e) => tracing::debug!("upgrade: {e}"),
        }
    });
    let mut r = respond(StatusCode::SWITCHING_PROTOCOLS, "text/plain", Bytes::new());
    let h = r.headers_mut();
    h.remove(header::CONTENT_TYPE);
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&key) {
        h.insert(header::SEC_WEBSOCKET_ACCEPT, v);
    }
    r
}

/// The browser's Hello, made safe: always a web client, with where it came from.
fn rewrite_hello(payload: &[u8], via: &str) -> Option<(Bytes, Encoding)> {
    let (msg, enc) = decode::<ClientMsg>(payload).ok()?;
    let ClientMsg::Hello { proto, mut client, features } = msg else { return None };
    client.kind = ClientKind::Web;
    client.via = Some(via.to_owned());
    client.pid = 0;
    client.outer = None;
    let features = features.into_iter().filter(|f| f != ptyro_protocol::FEATURE_ZSTD).collect();
    encode(&ClientMsg::Hello { proto, client, features }, enc).ok().map(|b| (b, enc))
}

async fn bridge<S>(ws: WebSocketStream<S>, st: Arc<State>, via: String, ip: IpAddr, login: Pending)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut wtx, mut wrx) = ws.split();
    // First message: the Hello (10 s).
    let first = match tokio::time::timeout(Duration::from_secs(10), wrx.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => Bytes::copy_from_slice(t.as_bytes()),
        Ok(Some(Ok(Message::Binary(b)))) => b,
        _ => return,
    };
    let Some((hello, enc)) = rewrite_hello(&first, &via) else {
        let _ = wtx.send(Message::Close(None)).await;
        return;
    };
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
    opts.autospawn = false;
    let sock = match ptyro_client::connect_raw(&opts).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("daemon: {e}");
            let _ = wtx.send(Message::Close(None)).await;
            return;
        }
    };
    let (mut dtx, mut drx) = Framed::new(sock, framing()).split();
    if dtx.send(hello).await.is_err() {
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    // Anything from the browser, a pong included, proves it is still there.
    let mut heard = Instant::now();
    let mut welcomed = false;
    // Counts against MAX_PENDING until the daemon answers the Hello.
    let mut login = Some(login);
    loop {
        tokio::select! {
            m = wrx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    heard = Instant::now();
                    if dtx.send(Bytes::copy_from_slice(t.as_bytes())).await.is_err() { break; }
                }
                Some(Ok(Message::Binary(b))) => {
                    heard = Instant::now();
                    if dtx.send(b).await.is_err() { break; }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => heard = Instant::now(),
                _ => break,
            },
            f = drx.next() => match f {
                Some(Ok(bytes)) => {
                    if !welcomed {
                        match decode::<ServerMsg>(&bytes) {
                            Ok((ServerMsg::Welcome { .. }, _)) => welcomed = true,
                            Ok((ServerMsg::Reject { .. }, _)) => st.failed(ip),
                            _ => {}
                        }
                        if welcomed {
                            drop(login.take());
                        }
                    }
                    let msg = if enc == Encoding::Json && bytes.first() == Some(&b'{') {
                        match String::from_utf8(bytes.to_vec()) {
                            Ok(s) => Message::text(s),
                            Err(_) => break,
                        }
                    } else {
                        Message::binary(bytes.freeze())
                    };
                    if wtx.send(msg).await.is_err() { break; }
                }
                _ => break,
            },
            _ = ping.tick() => {
                // A phone that left the Wi-Fi never closes: stop holding its place.
                if heard.elapsed() > WS_SILENCE { break; }
                if wtx.send(Message::Ping(Bytes::new())).await.is_err() { break; }
            }
        }
    }
    let _ = wtx.send(Message::Close(None)).await;
}

/// A request's body: at most `limit` bytes, and all of it within [`BODY_TIMEOUT`].
async fn read_body(body: Incoming, limit: usize) -> Result<Bytes, (StatusCode, &'static str)> {
    match tokio::time::timeout(body_timeout(), Limited::new(body, limit).collect()).await {
        Ok(Ok(b)) => Ok(b.to_bytes()),
        Ok(Err(_)) => Err((StatusCode::PAYLOAD_TOO_LARGE, "too large")),
        Err(_) => Err((StatusCode::REQUEST_TIMEOUT, "the body didn't arrive")),
    }
}

/// What a browser is told when the daemon can't be reached (the cause, with a path in it, goes to
/// the log).
fn daemon_unavailable(e: &dyn std::fmt::Display) -> Response<Body> {
    tracing::warn!("daemon: {e}");
    respond(StatusCode::BAD_GATEWAY, "text/plain", "the ptyro daemon isn't reachable")
}

/// One request for one token, answered as JSON: `{"t":"ok","reply":…}` or
/// `{"t":"err","error":{"code":…,"message":…}}` (the protocol's own outcome shape).
async fn api_request(
    req: Request<Incoming>,
    st: &State,
    via: String,
    ip: IpAddr,
    login: Pending,
) -> Response<Body> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let Some(token) = token else {
        return respond(StatusCode::UNAUTHORIZED, "text/plain", "a device token is required");
    };
    let body = match read_body(req.into_body(), 1 << 20).await {
        Ok(b) => b,
        Err((code, why)) => return respond(code, "text/plain", why),
    };
    let Ok(request) = serde_json::from_slice::<ptyro_protocol::Request>(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a JSON request");
    };
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
    // Anyone who can reach this port could otherwise start a daemon.
    opts.autospawn = false;
    opts.token = Some(token);
    opts.encoding = Encoding::Json;
    let client = ptyro_client::Client::connect_via(&opts, &via).await;
    drop(login);
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            if matches!(e, ptyro_client::ClientError::Rejected(_)) {
                st.failed(ip);
                return respond(StatusCode::UNAUTHORIZED, "text/plain", "unauthorized");
            }
            return daemon_unavailable(&e);
        }
    };
    let out = match tokio::time::timeout(Duration::from_secs(30), client.request(request)).await {
        Ok(Ok(reply)) => serde_json::json!({ "t": "ok", "reply": reply }),
        Ok(Err(e)) => {
            serde_json::json!({ "t": "err", "error": { "code": e.code, "message": e.message } })
        }
        Err(_) => {
            serde_json::json!({ "t": "err", "error": { "code": "timeout", "message": "timed out" } })
        }
    };
    respond(StatusCode::OK, "application/json", out.to_string())
}

/// `{"op": …}` with a device token of `full` scope: something the machine does for the web app
/// (the config file, the doctor, profiles, the daemon's log…), `crate::host`.
async fn api_host(
    req: Request<Incoming>,
    st: &State,
    via: String,
    ip: IpAddr,
    login: Pending,
) -> Response<Body> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let Some(token) = token else {
        return respond(StatusCode::UNAUTHORIZED, "text/plain", "a device token is required");
    };
    let body = match read_body(req.into_body(), 1 << 20).await {
        Ok(b) => b,
        Err((code, why)) => return respond(code, "text/plain", why),
    };
    let Ok(args) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a JSON request");
    };
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
    // Anyone who can reach this port could otherwise start a daemon.
    opts.autospawn = false;
    opts.token = Some(token);
    opts.encoding = Encoding::Json;
    let client = ptyro_client::Client::connect_via(&opts, &via).await;
    drop(login);
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            if matches!(e, ptyro_client::ClientError::Rejected(_)) {
                st.failed(ip);
                return respond(StatusCode::UNAUTHORIZED, "text/plain", "unauthorized");
            }
            return daemon_unavailable(&e);
        }
    };
    let err = |code: &str, message: &str| {
        respond(
            StatusCode::OK,
            "application/json",
            serde_json::json!({ "t": "err", "error": { "code": code, "message": message } })
                .to_string(),
        )
    };
    // Only a device that may do anything may change the machine: asking for the device list is
    // `full`'s alone, so it doubles as the check.
    if let Err(e) = client.request(ptyro_protocol::Request::DeviceList).await {
        return if e.code == ptyro_core::ErrorCode::PermissionDenied {
            err(
                "permission_denied",
                "this device can't change the machine: pair it with `ptyro pair --scope full`",
            )
        } else {
            err("internal", &e.message)
        };
    }
    if st.remote {
        return err(
            "unsupported",
            "this ptyro serve talks to a daemon on another machine; do that on the machine that runs it",
        );
    }
    let op = args["op"].as_str().unwrap_or("").to_owned();
    let ctx = crate::cli::Ctx::local(st.ctx_paths.clone());
    let out = match tokio::time::timeout(
        Duration::from_secs(90),
        crate::host::run(&ctx, &op, &args),
    )
    .await
    {
        Ok(Ok(v)) => serde_json::json!({ "t": "ok", "reply": v }),
        Ok(Err(e)) => {
            serde_json::json!({ "t": "err", "error": { "code": e.code, "message": e.message } })
        }
        Err(_) => {
            serde_json::json!({ "t": "err", "error": { "code": "timeout", "message": "timed out" } })
        }
    };
    respond(StatusCode::OK, "application/json", out.to_string())
}

/// `{"code": "ptp_…"}` → `{"token": "ptd_…", "device": DeviceInfo}`: the web app trades the
/// pairing code from `ptyro pair` for its token, once. A wrong code counts as a failed login.
async fn api_pair(
    req: Request<Incoming>,
    st: &State,
    via: String,
    ip: IpAddr,
    login: Pending,
) -> Response<Body> {
    #[derive(serde::Deserialize)]
    struct Claim {
        code: String,
    }
    let body = match read_body(req.into_body(), 4096).await {
        Ok(b) => b,
        Err((code, why)) => return respond(code, "text/plain", why),
    };
    let Ok(Claim { code }) = serde_json::from_slice(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected {\"code\": …}");
    };
    // A local client of the daemon: the claim is the one thing it asks for here.
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Proxy);
    opts.autospawn = false;
    let client = match ptyro_client::Client::connect(&opts).await {
        Ok(c) => c,
        Err(e) => return daemon_unavailable(&e),
    };
    let claim = ptyro_protocol::Request::DeviceClaim { code, via: Some(via) };
    let out = tokio::time::timeout(Duration::from_secs(10), client.request(claim)).await;
    drop(login);
    match out {
        Ok(Ok(ptyro_protocol::Reply::Paired { device, token, replaced })) => respond(
            StatusCode::OK,
            "application/json",
            serde_json::json!({ "token": token, "device": device, "replaced": replaced })
                .to_string(),
        ),
        Ok(Err(e)) if e.code == ptyro_core::ErrorCode::PermissionDenied => {
            st.failed(ip);
            respond(StatusCode::UNAUTHORIZED, "text/plain", e.message)
        }
        Ok(Err(e)) => {
            tracing::warn!("pairing: {}", e.message);
            respond(StatusCode::BAD_GATEWAY, "text/plain", "the code wasn't accepted")
        }
        Ok(Ok(_)) | Err(_) => respond(StatusCode::BAD_GATEWAY, "text/plain", "no answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_expected_hosts_are_served() {
        let names = vec![
            "box.tail1234.ts.net".to_owned(),
            "box".to_owned(),
            "ptyro.example.com".to_owned(),
        ];
        for ok in [
            None,
            Some("127.0.0.1:7091"),
            Some("[::1]:7091"),
            Some("100.101.102.103:7091"),
            Some("localhost:7091"),
            Some("LOCALHOST"),
            Some("box.tail1234.ts.net"),
            Some("box.tail1234.ts.net."),
            Some("box:7091"),
            Some("ptyro.example.com:443"),
        ] {
            assert!(host_ok(ok, &names), "{ok:?}");
        }
        for bad in [
            Some("evil.example"),
            Some("evil.example:7091"),
            Some("box.tail1234.ts.net.evil.example"),
            Some("127.0.0.1.nip.io"),
            Some("x"),
            Some(""),
            Some("\0"),
        ] {
            assert!(!host_ok(bad, &names), "{bad:?}");
        }
        assert_eq!(url_host("https://Box.ts.net/").as_deref(), Some("box.ts.net"));
        assert_eq!(url_host("http://127.0.0.1:7091").as_deref(), Some("127.0.0.1"));
        assert_eq!(
            url_host("https://u@ptyro.example.com:8443/x").as_deref(),
            Some("ptyro.example.com")
        );
        assert_eq!(url_host("nope"), None);
    }

    #[test]
    fn logins_in_flight_are_capped_and_failures_pruned() {
        let st = Arc::new(State {
            ctx_paths: ptyro_core::paths::Paths::resolve(|k| {
                (k == "PTYRO_HOME").then(|| "/nonexistent".into())
            })
            .unwrap(),
            failures: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            open: Mutex::new(HashMap::new()),
            hosts: Vec::new(),
            ca_pem: None,
            served: None,
            remote: false,
        });
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let held: Vec<Pending> = (0..MAX_PENDING).map(|_| st.begin(ip).unwrap()).collect();
        assert!(st.begin(ip).is_none());
        assert!(st.begin("5.6.7.8".parse().unwrap()).is_some());
        drop(held);
        assert!(st.pending.locked().is_empty());
        let old: IpAddr = "9.9.9.9".parse().unwrap();
        // (Instant can't go back further than boot.)
        if let Some(then) = Instant::now().checked_sub(FAILURE_WINDOW) {
            st.failures.locked().insert(old, (3, then));
            st.failed(ip);
            assert!(!st.failures.locked().contains_key(&old));
        }
    }

    #[test]
    fn a_page_from_elsewhere_cannot_use_the_server() {
        let names = vec!["box.tail1234.ts.net".to_owned()];
        // Not from a browser (no Origin), or from this server's own page.
        assert!(origin_ok(None, Some("127.0.0.1:7091"), &names));
        assert!(origin_ok(Some("http://127.0.0.1:7091"), Some("127.0.0.1:7091"), &names));
        assert!(origin_ok(
            Some("https://BOX.tail1234.ts.net"),
            Some("box.tail1234.ts.net"),
            &names
        ));
        // A proxy that rewrote Host: the configured name still counts.
        assert!(origin_ok(Some("https://box.tail1234.ts.net"), Some("127.0.0.1:7091"), &names));
        // Another site, another port of this machine, `null`, junk.
        for bad in [
            "https://evil.example",
            "http://127.0.0.1:9999",
            "http://localhost:7091",
            "null",
            "file://",
            "evil",
        ] {
            assert!(!origin_ok(Some(bad), Some("127.0.0.1:7091"), &names), "{bad}");
        }
    }

    #[test]
    fn an_ipv6_network_is_one_address_to_the_lockout() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(lockout_key(a), lockout_key(b));
        assert_ne!(lockout_key(a), lockout_key(c));
        let v4: IpAddr = "1.2.3.4".parse().unwrap();
        assert_eq!(lockout_key(v4), v4);
        assert_eq!(
            lockout_key("::ffff:1.2.3.4".parse().unwrap()),
            "::ffff:1.2.3.4".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn one_address_cannot_take_every_place() {
        let st = Arc::new(State {
            ctx_paths: ptyro_core::paths::Paths::resolve(|k| {
                (k == "PTYRO_HOME").then(|| "/nonexistent".into())
            })
            .unwrap(),
            failures: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            open: Mutex::new(HashMap::new()),
            hosts: Vec::new(),
            ca_pem: None,
            served: None,
            remote: false,
        });
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let held: Vec<Open> = (0..MAX_PER_ADDR).map(|_| st.open_from(ip).unwrap()).collect();
        assert!(st.open_from(ip).is_none());
        assert!(st.open_from("5.6.7.8".parse().unwrap()).is_some(), "others still get in");
        drop(held);
        assert!(st.open.locked().len() <= 1);
        assert!(st.open_from(ip).is_some(), "places come back");
        // Logins waiting on the daemon are capped in all, whoever they come from.
        let mut all = Vec::new();
        for n in 0..MAX_PENDING_TOTAL {
            let ip: IpAddr = format!("10.0.{}.{}", n / 4, n % 250 + 1).parse().unwrap();
            all.push(st.begin(ip).unwrap());
        }
        assert!(st.begin("9.9.9.9".parse().unwrap()).is_none());
    }

    #[test]
    fn a_browser_message_fits_a_whole_scratchpad_and_the_web_app_knows_the_limit() {
        use ptyro_protocol::MAX_CLIENT_MESSAGE;
        const {
            assert!(MAX_CLIENT_MESSAGE > ptyro_core::scratchpad::MAX_PAD_BYTES + 64 * 1024);
            assert!(
                MAX_CLIENT_MESSAGE < ptyro_protocol::MAX_FRAME / 2,
                "far below the frame limit"
            );
        }
        let conn = include_str!("../../../web/src/conn.ts");
        assert_eq!(MAX_CLIENT_MESSAGE, 4 * 1024 * 1024 + 256 * 1024);
        assert!(
            conn.contains("MAX_SEND = 4 * 1024 * 1024 + 256 * 1024;"),
            "web/src/conn.ts's MAX_SEND"
        );
    }

    #[test]
    fn hellos_are_rewritten() {
        let hello = r#"{"t":"hello","proto":{"major":1,"minor":8},"client":{"kind":"tui","version":"x","pid":5,"token":"ptd_a"},"features":["zstd"]}"#;
        let (b, enc) = rewrite_hello(hello.as_bytes(), "ws:1.2.3.4:5").unwrap();
        assert_eq!(enc, Encoding::Json);
        let (ClientMsg::Hello { client, features, .. }, _) = decode::<ClientMsg>(&b).unwrap()
        else {
            panic!()
        };
        assert_eq!(client.kind, ClientKind::Web);
        assert_eq!(client.via.as_deref(), Some("ws:1.2.3.4:5"));
        assert_eq!(client.token.as_deref(), Some("ptd_a"));
        assert!(features.is_empty());
        assert!(rewrite_hello(br#"{"t":"ping"}"#, "x").is_none());
    }
}
