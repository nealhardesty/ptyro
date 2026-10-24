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
//! Listens on loopback or Tailscale addresses unless `--insecure`: elsewhere plain HTTP would
//! carry tokens in clear. For HTTPS (needed by phones for notifications and installing the app),
//! `--tailscale` puts `tailscale serve` in front.
//!
//! Every request must name a host it expects (an IP address, localhost, `[remote] public_url`'s
//! host, this machine's tailnet name), so a DNS-rebinding page can't reach it under a name of its
//! own. `X-Forwarded-For` counts only behind a proxy we trust (`[remote] trusted_proxy`, or
//! `tailscale serve` fronting our port).

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
use ptyro_core::config::{Config, safe_address};
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
    js!("kit"),
    js!("projects"),
    js!("qr"),
    js!("review"),
    js!("runs"),
    js!("screen"),
    js!("sessions"),
    js!("settings"),
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
/// Open connections in all.
const MAX_CONNECTIONS: usize = 256;

struct State {
    ctx_paths: ptyro_core::paths::Paths,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    pending: Mutex<HashMap<IpAddr, u32>>,
    /// A local proxy fronts us, so its `X-Forwarded-For` names the real client.
    trust_proxy: bool,
    /// Host names (lowercase, no port) requests may carry besides IP addresses and localhost.
    hosts: Vec<String>,
    /// The CA phones trust once (`/ca.crt`), when serving HTTPS.
    ca_pem: Option<String>,
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
        let mut p = self.st.pending.lock().expect("pending");
        if let Some(n) = p.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                p.remove(&self.ip);
            }
        }
    }
}

impl State {
    /// Start a login from `ip`, unless it has [`MAX_PENDING`] going already.
    fn begin(self: &Arc<Self>, ip: IpAddr) -> Option<Pending> {
        let mut p = self.pending.lock().expect("pending");
        let n = p.entry(ip).or_insert(0);
        if *n >= MAX_PENDING {
            return None;
        }
        *n += 1;
        Some(Pending { st: Arc::clone(self), ip })
    }

    fn locked_out(&self, ip: IpAddr) -> bool {
        let f = self.failures.lock().expect("failures");
        f.get(&ip).is_some_and(|(n, t)| *n >= MAX_FAILURES && t.elapsed() < FAILURE_WINDOW)
    }

    fn failed(&self, ip: IpAddr) {
        let mut f = self.failures.lock().expect("failures");
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
        if e.0 == MAX_FAILURES {
            tracing::warn!(%ip, "locked out for {}m after {MAX_FAILURES} failed device logins", FAILURE_WINDOW.as_secs() / 60);
        }
    }
}

pub async fn run(
    ctx: &Ctx,
    listen: Option<String>,
    insecure: bool,
    local: bool,
    own: Option<(std::path::PathBuf, std::path::PathBuf)>,
    tailscale: bool,
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
    let tailscale = tailscale || config.remote.tailscale;
    let insecure = insecure || config.remote.insecure;
    let local = local || config.remote.local;
    let listen = listen.unwrap_or_else(|| match local {
        // Every interface, on the configured port.
        true => format!("0.0.0.0:{}", config.remote.listen.rsplit(':').next().unwrap_or("7091")),
        false => config.remote.listen.clone(),
    });
    let addr: SocketAddr = listen
        .parse()
        .map_err(|e| Error::invalid(format!("--listen {listen}: {e} (like 127.0.0.1:7091)")))?;
    // TLS ends before us (`tailscale serve`, a reverse proxy on this machine): plain HTTP, on
    // loopback, where only that proxy reaches us. Else HTTPS, unless asked for plain HTTP.
    let fronted = tailscale
        || config.remote.trusted_proxy
        || crate::remote::tailscale_url(&addr.to_string()).is_some();
    let plain = insecure || fronted;
    if plain && !insecure && !addr.ip().is_loopback() {
        return Err(Error::invalid(format!(
            "{addr}: behind Tailscale HTTPS or a reverse proxy, listen on 127.0.0.1 (the proxy \
             reaches us there), or drop `trusted_proxy` to speak HTTPS on {addr} yourself"
        )));
    }
    if !safe_address(addr.ip()) && !local && !insecure {
        return Err(Error::invalid(format!(
            "{addr} isn't loopback or a Tailscale address: `--local` lets in this machine, private \
             networks and tailnets only (over HTTPS), and `--insecure` everyone, in clear"
        )));
    }
    let tailnet = tailnet_names();
    let mut cert_names: Vec<String> = tailnet.clone();
    cert_names.extend(config.remote.public_url.iter().filter_map(|u| url_host(u)));
    if !addr.ip().is_unspecified() {
        cert_names.push(addr.ip().to_string());
    }
    let tls_dir = crate::tls::dir(&ctx.paths.data);
    // Your own certificate (flags, else config), or the one ptyro makes.
    let own = own.or_else(|| {
        let (c, k) = (config.remote.tls_cert.as_ref()?, config.remote.tls_key.as_ref()?);
        let path = ptyro_core::config::expand_home;
        Some((path(c), path(k)))
    });
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
            Some(crate::tls::load_or_make(&tls_dir, &names, time::OffsetDateTime::now_utc())?)
        }
    };
    let own_cert = own.is_some() && !plain;
    // Listening means ready: the certificate exists before anyone can connect.
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| Error::internal(format!("listen on {addr}: {e}")))?;
    if tailscale {
        tailscale_front(addr)?;
    }
    // What phones open: an address they can reach (this machine's LAN address under --local).
    let shown_addr = if addr.ip().is_unspecified() {
        let lan = crate::tls::local_addresses().into_iter().find(|ip| !ip.is_loopback());
        SocketAddr::new(lan.unwrap_or(IpAddr::from([127, 0, 0, 1])), addr.port())
    } else {
        addr
    };
    let mut shown = config.clone();
    shown.remote.listen = shown_addr.to_string();
    let base = crate::remote::base_url(&shown, None, tls.is_some());
    let mut hosts: Vec<String> =
        config.remote.public_url.iter().chain([&base]).filter_map(|u| url_host(u)).collect();
    hosts.extend(tailnet);
    if let Some(t) = &tls {
        hosts.extend(t.names.iter().cloned());
    }
    let ca = tls.as_ref().filter(|_| !own_cert).map(|t| t.ca_fingerprint.as_str());
    crate::remote::record_serve(&ctx.paths, &base, ca);
    match &tls {
        Some(_) if own_cert => eprintln!(
            "ptyro serve: listening on https://{addr}, with your certificate; phones open {base} \
             (pair one with `ptyro pair`)"
        ),
        Some(t) => eprintln!(
            "ptyro serve: listening on https://{addr}{}; phones open {base} (pair one with \
             `ptyro pair`; the first time, trust ptyro's certificate from {base}/ca.crt, \
             SHA-256 {})",
            if local { ", for this machine, private networks and tailnets" } else { "" },
            t.ca_fingerprint
        ),
        None => eprintln!(
            "ptyro serve: listening on http://{addr}; phones open {base} (pair one with `ptyro pair`)"
        ),
    }
    let acceptor = tls.as_ref().map(|t| Arc::new(Mutex::new(Arc::clone(&t.config))));
    if let Some(current) = &acceptor {
        // Renewed while it runs: a fresh certificate before this one ends.
        let (current, dir, names) = (Arc::clone(current), tls_dir.clone(), cert_names.clone());
        let own = own.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
                let fresh = match &own {
                    Some((cert, key)) => crate::tls::from_files(cert, key),
                    None => {
                        let names = crate::tls::names_for(&names);
                        crate::tls::load_or_make(&dir, &names, time::OffsetDateTime::now_utc())
                            .map(|t| t.config)
                    }
                };
                match fresh {
                    Ok(c) => *current.lock().expect("tls") = c,
                    Err(e) => tracing::warn!("renewing the certificate: {}", e.message),
                }
            }
        });
    }
    let state = Arc::new(State {
        ctx_paths: ctx.paths.clone(),
        failures: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        trust_proxy: fronted,
        hosts,
        ca_pem: tls.as_ref().filter(|_| !own_cert).map(|t| t.ca_pem.clone()),
        remote: ctx.remote.is_some(),
    });
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    if managed {
        tokio::spawn(exit_with_daemon(ctx.paths.socket.clone()));
    }
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // `--local`: only this machine, private networks and tailnets get in.
        if local && !crate::tls::local_peer(peer.ip()) {
            tracing::debug!(%peer, "not a local address");
            continue;
        }
        // Over the limit: drop it (a WebSocket keeps its slot until it closes).
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            tracing::debug!(%peer, "too many connections");
            continue;
        };
        let slot = Arc::new(slot);
        let st = Arc::clone(&state);
        let tls = acceptor
            .as_ref()
            .map(|c| tokio_rustls::TlsAcceptor::from(Arc::clone(&c.lock().expect("tls"))));
        tokio::spawn(async move {
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

/// Started by the daemon: leave when it's gone for good (its socket missing for 30 s).
async fn exit_with_daemon(socket: std::path::PathBuf) {
    let mut missing = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        missing = if socket.exists() { 0 } else { missing + 1 };
        if missing >= 6 {
            std::process::exit(0);
        }
    }
}

/// `tailscale serve --bg` HTTPS → our port. It changes Tailscale's config, which `--tailscale`
/// asks for; `tailscale serve reset` undoes it.
fn tailscale_front(addr: SocketAddr) -> Result<(), Error> {
    let target = format!("http://127.0.0.1:{}", addr.port());
    let out = std::process::Command::new("tailscale")
        .args(["serve", "--bg", "--https=443", &target])
        .output()
        .map_err(|e| Error::invalid(format!("tailscale: {e} (is Tailscale installed?)")))?;
    if !out.status.success() {
        return Err(Error::invalid(format!(
            "tailscale serve: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    if !addr.ip().is_loopback() {
        eprintln!("note: tailscale serve proxies to 127.0.0.1:{}; listen there too", addr.port());
    }
    Ok(())
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
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
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

/// Who connected, for the audit trail: the peer, or what a trusted local proxy in front
/// (Tailscale serve, a TLS reverse proxy) says about the real client. That's the last
/// X-Forwarded-For entry, the one the proxy added: earlier ones come from the client and prove
/// nothing. Without a trusted proxy the header is ignored: any local process could send it.
fn via(req: &Request<Incoming>, peer: SocketAddr, trust_proxy: bool) -> (String, IpAddr) {
    let hdr = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    if trust_proxy
        && peer.ip().is_loopback()
        && let Some(xff) = hdr("x-forwarded-for")
    {
        let real = last_hop(&xff).to_owned();
        let ip = real.parse().unwrap_or(peer.ip());
        let who = hdr("tailscale-user-login").map(|u| format!(" {u}")).unwrap_or_default();
        return (format!("ws:{real}{who}"), ip);
    }
    (format!("ws:{peer}"), peer.ip())
}

fn last_hop(xff: &str) -> &str {
    xff.rsplit(',').next().unwrap_or("").trim()
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
    let (via, ip) = via(&req, peer, st.trust_proxy);
    let path = req.uri().path().to_owned();
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
                let mut r = respond(StatusCode::OK, ctype, Bytes::from_static(data));
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
                let cfg = WebSocketConfig::default()
                    .max_message_size(Some(ptyro_protocol::MAX_FRAME))
                    .max_frame_size(Some(ptyro_protocol::MAX_FRAME));
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
    let opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
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
    let mut welcomed = false;
    // Counts against MAX_PENDING until the daemon answers the Hello.
    let mut login = Some(login);
    loop {
        tokio::select! {
            m = wrx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    if dtx.send(Bytes::copy_from_slice(t.as_bytes())).await.is_err() { break; }
                }
                Some(Ok(Message::Binary(b))) => {
                    if dtx.send(b).await.is_err() { break; }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
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
                if wtx.send(Message::Ping(Bytes::new())).await.is_err() { break; }
            }
        }
    }
    let _ = wtx.send(Message::Close(None)).await;
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
    let body = match Limited::new(req.into_body(), 1 << 20).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return respond(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "too large"),
    };
    let Ok(request) = serde_json::from_slice::<ptyro_protocol::Request>(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a JSON request");
    };
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
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
            return respond(StatusCode::BAD_GATEWAY, "text/plain", e.to_string());
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
    let body = match Limited::new(req.into_body(), 1 << 20).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return respond(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "too large"),
    };
    let Ok(args) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected a JSON request");
    };
    let mut opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Web);
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
            return respond(StatusCode::BAD_GATEWAY, "text/plain", e.to_string());
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
    let body = match Limited::new(req.into_body(), 4096).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return respond(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "too large"),
    };
    let Ok(Claim { code }) = serde_json::from_slice(&body) else {
        return respond(StatusCode::BAD_REQUEST, "text/plain", "expected {\"code\": …}");
    };
    // A local client of the daemon: the claim is the one thing it asks for here.
    let opts = ptyro_client::ConnectOptions::new(st.ctx_paths.clone(), ClientKind::Proxy);
    let client = match ptyro_client::Client::connect(&opts).await {
        Ok(c) => c,
        Err(e) => return respond(StatusCode::BAD_GATEWAY, "text/plain", e.to_string()),
    };
    let claim = ptyro_protocol::Request::DeviceClaim { code, via: Some(via) };
    let out = tokio::time::timeout(Duration::from_secs(10), client.request(claim)).await;
    drop(login);
    match out {
        Ok(Ok(ptyro_protocol::Reply::Paired { device, token })) => respond(
            StatusCode::OK,
            "application/json",
            serde_json::json!({ "token": token, "device": device }).to_string(),
        ),
        Ok(Err(e)) if e.code == ptyro_core::ErrorCode::PermissionDenied => {
            st.failed(ip);
            respond(StatusCode::UNAUTHORIZED, "text/plain", e.message)
        }
        Ok(Err(e)) => respond(StatusCode::BAD_GATEWAY, "text/plain", e.message),
        Ok(Ok(_)) | Err(_) => respond(StatusCode::BAD_GATEWAY, "text/plain", "no answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_added_address_counts() {
        assert_eq!(last_hop("6.6.6.6, 1.2.3.4"), "1.2.3.4");
        assert_eq!(last_hop("1.2.3.4"), "1.2.3.4");
    }

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
            trust_proxy: false,
            hosts: Vec::new(),
            ca_pem: None,
            remote: false,
        });
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let held: Vec<Pending> = (0..MAX_PENDING).map(|_| st.begin(ip).unwrap()).collect();
        assert!(st.begin(ip).is_none());
        assert!(st.begin("5.6.7.8".parse().unwrap()).is_some());
        drop(held);
        assert!(st.pending.lock().unwrap().is_empty());
        let old: IpAddr = "9.9.9.9".parse().unwrap();
        // (Instant can't go back further than boot.)
        if let Some(then) = Instant::now().checked_sub(FAILURE_WINDOW) {
            st.failures.lock().unwrap().insert(old, (3, then));
            st.failed(ip);
            assert!(!st.failures.lock().unwrap().contains_key(&old));
        }
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
