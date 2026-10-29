//! `ptyro machine …` (MM1.B, SPEC §16.4b): mirror other machines' workspaces here.
//!
//! - `machine add ssh://laptop2`: over ssh, as `--remote` (keys or an agent; nothing to pair).
//! - `machine pair` on laptop2 prints a code; `machine add CODE` here claims it, once, from its
//!   `ptyro serve` (its certificate authority pinned by the fingerprint the code carries) and
//!   keeps the token it is given.
//! - `machine-bridge` (hidden) is what the daemon runs to reach a machine over its `ptyro serve`:
//!   protocol frames on stdin and stdout, one WebSocket message each, the HTTPS checked against
//!   the pinned certificate authority.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use ptyro_core::Error;
use ptyro_core::model::{MachineInfo, MachineState, MachineTransport};
use ptyro_protocol::{Reply, Request, Scope, framing};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::{FramedRead, FramedWrite};

use crate::cli::{Ctx, no_daemon, unexpected};

/// A machine pairing code: `https://laptop2.tail.ts.net:7091/#machine=ptp_…&ca=AB:CD…&name=laptop2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub base: String,
    pub code: String,
    pub ca: Option<String>,
    pub name: Option<String>,
}

impl Code {
    pub fn parse(s: &str) -> Option<Self> {
        let (base, frag) = s.trim().split_once("#machine=")?;
        let base = base.trim_end_matches('/').to_owned();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return None;
        }
        let mut parts = frag.split('&');
        let code = parts.next()?.to_owned();
        let (mut ca, mut name) = (None, None);
        for p in parts {
            match p.split_once('=') {
                Some(("ca", v)) if !v.is_empty() => ca = Some(v.to_owned()),
                Some(("name", v)) if !v.is_empty() => name = Some(v.to_owned()),
                _ => {}
            }
        }
        (code.starts_with("ptp_")).then_some(Self { base, code, ca, name })
    }

    pub fn text(&self) -> String {
        let mut s = format!("{}/#machine={}", self.base, self.code);
        if let Some(ca) = &self.ca {
            s.push_str(&format!("&ca={ca}"));
        }
        if let Some(n) = &self.name {
            s.push_str(&format!("&name={n}"));
        }
        s
    }
}

/// `ssh://me@laptop2`, `me@laptop2`, `laptop2` → an ssh transport.
fn ssh_target(t: &str) -> MachineTransport {
    let t = t.trim();
    MachineTransport::Ssh { target: t.strip_prefix("ssh://").unwrap_or(t).to_owned() }
}

/// What a machine's state means, in a line.
pub fn state_line(m: &MachineInfo) -> String {
    let mut s = format!("{} ({}): {}", m.name, m.transport.label(), m.state.label());
    if let Some(v) = &m.version {
        s.push_str(&format!(", ptyro {v}"));
    }
    if m.state == MachineState::Online || !m.workspaces.is_empty() {
        let shown = m.workspaces.iter().filter(|w| !m.hidden.contains(w)).count();
        s.push_str(&format!(", {shown} workspace(s)"));
        if !m.hidden.is_empty() {
            s.push_str(&format!(" ({} hidden)", m.hidden.len()));
        }
    }
    s
}

pub async fn add(
    ctx: &Ctx,
    target: String,
    name: Option<String>,
    force: bool,
    json: bool,
) -> Result<i32, Error> {
    let (transport, token, name) = match Code::parse(&target) {
        Some(code) => {
            let token = claim(&code).await?;
            let fingerprint = code.ca.clone();
            (
                MachineTransport::Token { url: code.base.clone(), fingerprint },
                Some(token),
                name.or(code.name.clone()),
            )
        }
        None if target.contains("#machine=") => {
            return Err(Error::invalid(
                "that looks like a machine code but isn't whole: copy all of what `ptyro machine pair` printed",
            ));
        }
        None => (ssh_target(&target), None, name),
    };
    let c = ctx.connect(true).await?;
    let info = match c.request(Request::MachineAdd { name, transport, token, force }).await? {
        Reply::Machine { info } => info,
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
        return Ok(0);
    }
    println!("added {}", state_line(&info));
    if info.state == MachineState::Mismatch {
        println!("It is added, but its workspaces won't show until the versions match.");
    }
    if let Some(e) = &info.error {
        eprintln!("{e}");
    }
    if let Some(f) = &info.fix {
        eprintln!("-> {f}");
    }
    println!(
        "Its workspaces show beside yours as {}:<workspace>; things there are addressed {}:<name>.",
        info.name, info.name
    );
    Ok(0)
}

pub async fn ls(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let machines = match c.request(Request::MachineList).await? {
        Reply::Machines { machines } => machines,
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&machines).unwrap_or_default());
        return Ok(0);
    }
    if machines.is_empty() {
        eprintln!(
            "no other machines: `ptyro machine add ssh://HOST`, or `ptyro machine pair` on the other one and `ptyro machine add CODE` here"
        );
    }
    for m in &machines {
        println!("{}", state_line(m));
        if m.state != MachineState::Online {
            if let Some(e) = &m.error {
                println!("    {e}");
            }
            if let Some(f) = &m.fix {
                println!("    -> {f}");
            }
        }
        for w in &m.workspaces {
            let hidden = if m.hidden.contains(w) { "  (hidden)" } else { "" };
            println!("    {}:{w}{hidden}", m.name);
        }
    }
    Ok(0)
}

pub async fn simple(ctx: &Ctx, req: Request, said: String) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match c.request(req).await? {
        Reply::Machine { info } => println!("{}", state_line(&info)),
        Reply::Done => println!("{said}"),
        r => return Err(unexpected(r)),
    }
    Ok(0)
}

/// `machine pair`: a code another machine claims (`machine add CODE`) to mirror this one.
pub async fn pair(ctx: &Ctx, url: Option<String>, json: bool) -> Result<i32, Error> {
    let (base, notes, ca, serving) = crate::remote::served_base(ctx, url.clone());
    if !serving && url.is_none() {
        return Err(Error::invalid(
            "this machine's web app server isn't running, and a code is claimed from it: turn it on (`ptyro serve`, or `[remote] serve = true`), or skip pairing and add this machine over ssh on the other one (`ptyro machine add ssh://THIS-HOST`)",
        ));
    }
    let c = ctx.connect(true).await.map_err(no_daemon)?;
    let me = match c.request(Request::Status).await {
        Ok(Reply::Status { status }) => status.machine,
        _ => None,
    };
    let (code, expires_ms) = match c
        .request(Request::DevicePair { name: "machine".into(), scope: Scope::Full, machine: true })
        .await?
    {
        Reply::Pairing { code, expires_ms, .. } => (code, expires_ms),
        r => return Err(unexpected(r)),
    };
    let code = Code {
        base: base.trim_end_matches('/').to_owned(),
        code,
        ca,
        name: me.as_deref().and_then(ptyro_core::names::as_name),
    };
    if json {
        println!(
            "{}",
            serde_json::json!({ "code": code.text(), "expires_ms": expires_ms, "notes": notes })
        );
        return Ok(0);
    }
    for n in &notes {
        eprintln!("note: {n}");
    }
    println!("On the other machine, run (within 10 minutes, once):\n");
    println!("    ptyro machine add '{}'\n", code.text());
    println!(
        "It mirrors this machine's workspaces over this web app's server, with full access: revoke it here with `ptyro devices revoke` (it's listed as \"machine\")."
    );
    Ok(0)
}

// ── TLS with the paired certificate authority ───────────────────────────────

/// Accepts a server whose chain carries the certificate authority with this fingerprint, and
/// whose certificate that authority signed for the name connected to.
#[derive(Debug)]
struct Pinned {
    fingerprint: Option<String>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

/// A machine on a tailnet has a `*.ts.net` name, and Tailscale can give it a certificate every
/// browser trusts (`[remote] tailscale_cert`): that, checked against the public roots, is as
/// good as the pinned authority for such a name.
fn is_tailnet_name(host: &str) -> bool {
    host.trim_end_matches('.').to_ascii_lowercase().ends_with(".ts.net")
}

fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_ascii_uppercase()
    };
    norm(a) == norm(b)
}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let pinned = self.fingerprint.as_deref().and_then(|f| {
            intermediates.iter().find(|c| same_fingerprint(&crate::tls::fingerprint(c), f))
        });
        let Some(ca) = pinned else {
            if matches!(server_name, rustls::pki_types::ServerName::DnsName(n) if is_tailnet_name(n.as_ref()))
            {
                let roots =
                    rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
                return rustls::client::WebPkiServerVerifier::builder_with_provider(
                    Arc::new(roots),
                    Arc::clone(&self.provider),
                )
                .build()
                .map_err(|e| rustls::Error::General(e.to_string()))?
                .verify_server_cert(
                    end_entity,
                    intermediates,
                    server_name,
                    ocsp,
                    now,
                );
            }
            return Err(rustls::Error::General(
                "its certificate isn't the one paired (another server, or its certificates were remade: pair again)".into(),
            ));
        };
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::clone(&self.provider),
        )
        .build()
        .map_err(|e| rustls::Error::General(e.to_string()))?;
        verifier.verify_server_cert(end_entity, &[], server_name, ocsp, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// `https://host:port/…` → (https, host, port).
fn parse_base(url: &str) -> Result<(bool, String, u16), Error> {
    let (tls, rest) =
        if let Some(r) = url.strip_prefix("https://").or_else(|| url.strip_prefix("wss://")) {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://").or_else(|| url.strip_prefix("ws://")) {
            (false, r)
        } else {
            return Err(Error::invalid(format!("{url}: expected https://HOST[:PORT]")));
        };
    let authority = rest.split(['/', '#', '?']).next().unwrap_or(rest);
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (h, after) = inner.split_once(']').unwrap_or((inner, ""));
        (h.to_owned(), after.strip_prefix(':').filter(|p| digits(p)).and_then(|p| p.parse().ok()))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) if digits(p) => (h.to_owned(), p.parse().ok()),
            _ => (authority.to_owned(), None),
        }
    };
    let port: u16 = port.unwrap_or(if tls { 443 } else { 80 });
    if host.is_empty() {
        return Err(Error::invalid(format!("{url}: no host")));
    }
    Ok((tls, host, port))
}

/// A connection to `url`'s server: TLS checked against the pinned authority, or plain HTTP (only
/// sensible over Tailscale, which encrypts).
async fn open(url: &str, fingerprint: Option<&str>) -> Result<(Box<dyn Io>, String, bool), Error> {
    let (tls, host, port) = parse_base(url)?;
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await.map_err(|e| {
        Error::new(ptyro_core::ErrorCode::DaemonUnavailable, format!("{host}:{port}: {e}"))
    })?;
    let authority = format!("{host}:{port}");
    if !tls {
        return Ok((Box::new(tcp), authority, false));
    }
    if fingerprint.is_none() && !is_tailnet_name(&host) {
        return Err(Error::invalid(format!(
            "{url} is HTTPS with a certificate this machine can't check (the code carried none): pair again with `ptyro machine pair` there while `ptyro serve` uses its own HTTPS, or add it over ssh"
        )));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::internal(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint: fingerprint.map(str::to_owned),
            provider,
        }))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| Error::invalid(format!("{host}: {e}")))?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| Error::new(ptyro_core::ErrorCode::DaemonUnavailable, format!("{url}: {e}")))?;
    Ok((Box::new(stream), authority, true))
}

/// Trade a machine code for its token, at the machine that printed it (`POST /api/pair`).
pub async fn claim(code: &Code) -> Result<String, Error> {
    let (mut io, authority, _) = open(&code.base, code.ca.as_deref()).await?;
    let body = serde_json::json!({ "code": code.code }).to_string();
    let req = format!(
        "POST /api/pair HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    io.write_all(req.as_bytes()).await.map_err(|e| Error::internal(e.to_string()))?;
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        io.take(1 << 20).read_to_end(&mut buf),
    )
    .await;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status != 200 {
        let why = body.trim();
        return Err(Error::new(
            ptyro_core::ErrorCode::PermissionDenied,
            format!(
                "{} refused the code ({status}{}): codes work once, for 10 minutes; make a fresh one with `ptyro machine pair` there",
                code.base,
                if why.is_empty() { String::new() } else { format!(": {why}") }
            ),
        ));
    }
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| Error::internal(format!("unexpected answer from {}: {e}", code.base)))?;
    v["token"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::internal(format!("{} answered without a token", code.base)))
}

/// `machine-bridge`: stdin/stdout ⇄ the machine's `ptyro serve` WebSocket. What it says on stderr
/// is the daemon's reason when the link fails, so the last line says it plainly.
pub async fn bridge(url: String, fingerprint: Option<String>) -> Result<i32, Error> {
    let (io, authority, tls) = match open(&url, fingerprint.as_deref()).await {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{}", e.message);
            return Ok(2);
        }
    };
    let ws_url = format!("{}://{authority}/ws", if tls { "wss" } else { "ws" });
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(ptyro_protocol::MAX_FRAME))
        .max_frame_size(Some(ptyro_protocol::MAX_FRAME));
    let ws = match tokio_tungstenite::client_async_with_config(ws_url.as_str(), io, Some(cfg)).await
    {
        Ok((ws, _)) => ws,
        Err(e) => {
            eprintln!("{url}: no WebSocket ({e}): is it ptyro serve?");
            return Ok(2);
        }
    };
    use tokio_tungstenite::tungstenite::Message;
    let (mut wtx, mut wrx) = ws.split();
    let mut input = FramedRead::new(tokio::io::stdin(), framing());
    let mut output = FramedWrite::new(tokio::io::stdout(), framing());
    loop {
        tokio::select! {
            f = input.next() => match f {
                Some(Ok(bytes)) => {
                    if wtx.send(Message::binary(bytes.freeze())).await.is_err() { break; }
                }
                _ => break,
            },
            m = wrx.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    if output.send(b).await.is_err() { break; }
                }
                Some(Ok(Message::Text(t))) => {
                    if output.send(bytes::Bytes::copy_from_slice(t.as_bytes())).await.is_err() { break; }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    eprintln!("{url}: {e}");
                    break;
                }
                None => break,
            },
        }
    }
    let _ = wtx.send(Message::Close(None)).await;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tailnet_names_may_skip_the_pin() {
        assert!(is_tailnet_name("box.tail1234.ts.net"));
        assert!(is_tailnet_name("BOX.tail1234.ts.net."));
        for no in ["example.com", "ts.net.evil.com", "192.168.1.5", "notts.net"] {
            assert!(!is_tailnet_name(no), "{no}");
        }
    }

    #[tokio::test]
    async fn a_code_without_a_pin_is_refused_for_any_name_but_a_tailnets() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://127.0.0.1:{}", l.local_addr().unwrap().port());
        let Err(e) = open(&url, None).await else { panic!("no pin, no tailnet name") };
        assert!(e.message.contains("can't check"), "{}", e.message);
    }

    #[test]
    fn codes_read_back_what_they_say() {
        let c = Code {
            base: "https://laptop2.tail.ts.net:7091".into(),
            code: "ptp_abc".into(),
            ca: Some("AB:CD".into()),
            name: Some("laptop2".into()),
        };
        assert_eq!(Code::parse(&c.text()), Some(c.clone()));
        assert_eq!(Code::parse(&format!("  {}  ", c.text())), Some(c));
        assert!(Code::parse("ssh://laptop2").is_none());
        assert!(Code::parse("https://x/#machine=nope").is_none());
    }

    #[test]
    fn bases_name_their_host_and_port() {
        assert_eq!(
            parse_base("https://laptop2.tail.ts.net:7091").unwrap(),
            (true, "laptop2.tail.ts.net".into(), 7091)
        );
        assert_eq!(
            parse_base("http://100.64.0.2:7091/").unwrap(),
            (false, "100.64.0.2".into(), 7091)
        );
        assert_eq!(parse_base("https://box").unwrap(), (true, "box".into(), 443));
        assert_eq!(parse_base("https://[fd7a::1]:7091").unwrap(), (true, "fd7a::1".into(), 7091));
        assert!(parse_base("ftp://x").is_err());
        assert!(same_fingerprint("ab:cd:ef", "ABCDEF"));
        assert_eq!(
            ssh_target("ssh://me@laptop2"),
            MachineTransport::Ssh { target: "me@laptop2".into() }
        );
    }
}
