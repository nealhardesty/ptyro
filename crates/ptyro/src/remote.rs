//! Remote access from the command line (SPEC §16, M9): `ptyro proxy` (what `--remote` runs over
//! ssh), `ptyro pair` (a QR code that opens the web app signed in) and `ptyro devices`.

use ptyro_core::Error;
use ptyro_core::config::Config;
use ptyro_protocol::{Reply, Request, Scope};

use crate::DevicesCmd;
use crate::cli::{Ctx, no_daemon, unexpected};

/// Bridge stdin/stdout to this machine's daemon socket, starting the daemon if needed. The
/// bytes are the protocol itself; nothing is interpreted here.
pub async fn proxy(ctx: &Ctx) -> Result<i32, Error> {
    let opts =
        ptyro_client::ConnectOptions::new(ctx.paths.clone(), ptyro_protocol::ClientKind::Proxy);
    let mut sock = ptyro_client::connect_raw(&opts).await.map_err(Error::from)?;
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    // Either side closing ends the bridge (ssh closing stdin, the daemon going away).
    let _ = tokio::io::copy_bidirectional(&mut stdio, &mut sock).await;
    Ok(0)
}

/// Where the web app is reached from a phone: `--url`, `[remote] public_url`, Tailscale HTTPS
/// in front of the listen port, or the listen address itself.
pub fn base_url(config: &Config, explicit: Option<String>, https: bool) -> String {
    let scheme = if https { "https" } else { "http" };
    explicit
        .or_else(|| config.remote.public_url.clone())
        .or_else(|| tailscale_url(&config.remote.listen))
        .unwrap_or_else(|| format!("{scheme}://{}", config.remote.listen))
        .trim_end_matches('/')
        .to_owned()
}

/// What a running `ptyro serve` serves (`state/serve.json`), so `ptyro pair` points phones at
/// it: its URL and, over HTTPS, its CA's fingerprint.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Served {
    pub pid: u32,
    pub url: String,
    pub ca_fingerprint: Option<String>,
    /// Where it listens, to tell it from a process that only has its pid now.
    #[serde(default)]
    pub listen: Option<String>,
}

pub fn record_serve(
    paths: &ptyro_core::paths::Paths,
    url: &str,
    ca: Option<&str>,
    listen: std::net::SocketAddr,
) {
    let s = Served {
        pid: std::process::id(),
        url: url.into(),
        ca_fingerprint: ca.map(str::to_owned),
        listen: Some(listen.to_string()),
    };
    let _ = std::fs::create_dir_all(&paths.state);
    let _ = std::fs::write(
        paths.state.join("serve.json"),
        serde_json::to_string(&s).unwrap_or_default(),
    );
}

/// The running `ptyro serve`'s record, if it's still running.
pub fn served(paths: &ptyro_core::paths::Paths) -> Option<Served> {
    let s: Served =
        serde_json::from_str(&std::fs::read_to_string(paths.state.join("serve.json")).ok()?)
            .ok()?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(s.pid).ok()?)?;
    if rustix::process::test_kill_process(pid).is_err() {
        return None;
    }
    // A pid can be reused after a crash: what listens where it said it would settles it.
    if let Some(addr) = s.listen.as_deref().and_then(|l| l.parse::<std::net::SocketAddr>().ok()) {
        let addr = if addr.ip().is_unspecified() {
            std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), addr.port())
        } else {
            addr
        };
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).ok()?;
    }
    Some(s)
}

/// Whether `tailscale serve status --json` has a handler proxying to `127.0.0.1:<port>` (or
/// `localhost`): the port must be exactly it, not one it starts with.
pub fn proxies_to(status: &serde_json::Value, port: &str) -> bool {
    proxy_targets(status).iter().any(|p| {
        let rest = p.split_once("://").map_or(p.as_str(), |x| x.1);
        let authority = rest.split('/').next().unwrap_or("");
        authority == format!("127.0.0.1:{port}") || authority == format!("localhost:{port}")
    })
}

fn proxy_targets(status: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(web) = status["Web"].as_object() {
        for site in web.values() {
            for h in site["Handlers"].as_object().into_iter().flat_map(|m| m.values()) {
                if let Some(p) = h["Proxy"].as_str() {
                    out.push(p.to_owned());
                }
            }
        }
    }
    out
}

/// What `tailscale serve` already has on :443 when it is not a proxy to `port` (so replacing it
/// would break it), as a short description.
pub fn serves_443_elsewhere(status: &serde_json::Value, port: u16) -> Option<String> {
    let web = status["Web"].as_object()?;
    let on_443: Vec<&serde_json::Value> =
        web.iter().filter(|(k, _)| k.ends_with(":443")).map(|(_, v)| v).collect();
    if on_443.is_empty() {
        return None;
    }
    let mut theirs = Vec::new();
    for site in on_443 {
        for h in site["Handlers"].as_object().into_iter().flat_map(|m| m.values()) {
            let what = h["Proxy"]
                .as_str()
                .or_else(|| h["Path"].as_str())
                .or_else(|| h["Text"].as_str().map(|_| "some text"))
                .unwrap_or("something");
            theirs.push(what.to_owned());
        }
    }
    let ours = proxies_to(status, &port.to_string());
    (!ours && !theirs.is_empty()).then(|| theirs.join(", "))
}

/// `https://<machine>.<tailnet>.ts.net` when `tailscale serve` proxies to our port.
pub fn tailscale_url(listen: &str) -> Option<String> {
    let port = listen.rsplit(':').next()?;
    let serve = std::process::Command::new("tailscale")
        .args(["serve", "status", "--json"])
        .output()
        .ok()?;
    let status: serde_json::Value = serde_json::from_slice(&serve.stdout).ok()?;
    if !proxies_to(&status, port) {
        return None;
    }
    let status =
        std::process::Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&status.stdout).ok()?;
    let name = v["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    (!name.is_empty()).then(|| format!("https://{name}"))
}

/// A QR code as text, two rows per line (fits in a terminal).
pub fn qr(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

pub async fn pair(
    ctx: &Ctx,
    name: String,
    scope: &str,
    url: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    let scope: Scope = scope.parse().map_err(Error::invalid)?;
    let c = ctx.connect(true).await.map_err(no_daemon)?;
    let (device, code, expires_ms) = match c.request(Request::DevicePair { name, scope }).await? {
        Reply::Pairing { device, code, expires_ms } => (device, code, expires_ms),
        r => return Err(unexpected(r)),
    };
    let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
    // The running `ptyro serve`'s own address (it knows its flags); else what config implies.
    let served = served(&ctx.paths);
    let https = !(config.remote.insecure || config.remote.trusted_proxy);
    let base = match (&url, &served) {
        (None, Some(s)) if config.remote.public_url.is_none() => s.url.clone(),
        _ => base_url(&config, url, https),
    };
    let ca = served.as_ref().and_then(|s| s.ca_fingerprint.clone());
    // What we print stays in scrollback and recordings, so it is a code the web app trades
    // (once, within minutes) for the token, never the token. It travels in the fragment:
    // browsers never send that to a server or put it in logs.
    let link = format!("{base}/#pair={code}");
    if json {
        println!(
            "{}",
            serde_json::json!({ "device": device, "url": link, "expires_ms": expires_ms, "ca": ca.as_ref().map(|f| serde_json::json!({ "url": format!("{base}/ca.crt"), "sha256": f })) })
        );
        return Ok(0);
    }
    if let Some(q) = qr(&link) {
        println!("{q}");
    }
    let minutes = expires_ms.saturating_sub(ptyro_core::now_ms()).div_ceil(60_000);
    println!("Scan with the phone's camera, or open:\n  {link}\n");
    println!(
        "The link works once, for {minutes} minutes, and needs `ptyro serve` running. Waiting \
         for {} to open it (Ctrl-C stops waiting; the link keeps working)…",
        device.name
    );
    println!(
        "This device may {}.\n",
        match scope {
            Scope::Read =>
                "watch: sessions, screens and the inbox. `--scope approve` also answers prompts",
            Scope::Approve =>
                "watch and answer prompts and gates. `--scope control` also types into sessions, \
                and starts configured agents; `--scope full` is the whole web app: tasks and \
                worktrees, review and ship, the config, devices",
            Scope::Control =>
                "watch, answer, type and start configured agents. `--scope full` is the whole \
                web app: worktrees, review and ship, the config, devices",
            Scope::Full => "do everything the web app does, and anything you can on this machine",
        }
    );
    if std::env::var_os("PTYRO_SESSION_ID").is_some() {
        eprintln!(
            "note: this pane is recorded and readable by paired devices and agents; until the \
             link is used or expires, so is the link. A plain terminal keeps it to you."
        );
    }
    if let Some(f) = &ca {
        println!(
            "First time on this phone? Open {base}/ca.crt and trust it (iPhone: Settings → \
             General → VPN & Device Management, then About → Certificate Trust Settings; Android: \
             Settings → Security → Install a certificate). Its SHA-256 is {f}. docs/remote.md has \
             the steps.\n"
        );
    } else if !base.starts_with("https://")
        && !base.contains("127.0.0.1")
        && !base.contains("localhost")
    {
        eprintln!(
            "note: {base} isn't HTTPS, so phones won't install the app or show notifications, \
             and the pairing code in the link crosses the network in clear. Drop `insecure` \
             (ptyro speaks HTTPS by default), or put Tailscale HTTPS in front with \
             `ptyro serve --tailscale` (docs/remote.md)."
        );
    }
    // Who claimed it tells the user it was their phone (a code read off a recording would
    // show up here too, and `ptyro devices revoke` undoes it).
    while ptyro_core::now_ms() < expires_ms {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let Reply::Devices { devices } = c.request(Request::DeviceList).await? else { continue };
        match devices.iter().find(|d| d.id == device.id) {
            Some(d) if d.pending => {}
            Some(d) => {
                println!(
                    "Paired {} ({}, scope {}) from {}. It stays paired until `ptyro devices revoke {}`.",
                    d.name,
                    d.id,
                    d.scope.label(),
                    d.last_via.as_deref().unwrap_or("?"),
                    d.id
                );
                return Ok(0);
            }
            None => break,
        }
    }
    eprintln!("The link expired unused (or was revoked); run `ptyro pair` again.");
    Ok(1)
}

pub async fn devices(ctx: &Ctx, cmd: Option<DevicesCmd>) -> Result<i32, Error> {
    let c = ctx.connect(true).await.map_err(no_daemon)?;
    match cmd.unwrap_or(DevicesCmd::Ls { json: false }) {
        DevicesCmd::Ls { json } => {
            let devices = match c.request(Request::DeviceList).await? {
                Reply::Devices { devices } => devices,
                r => return Err(unexpected(r)),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&devices).unwrap_or_default());
                return Ok(0);
            }
            if devices.is_empty() {
                println!("No paired devices. `ptyro pair` pairs a phone.");
                return Ok(0);
            }
            println!(
                "{:<13} {:<16} {:<8} {:<12} {:<5} VIA",
                "ID", "NAME", "SCOPE", "LAST SEEN", "PUSH"
            );
            for d in devices {
                let seen = if d.pending {
                    "unclaimed".into()
                } else {
                    d.last_seen_ms.map_or_else(|| "never".into(), crate::cli::age)
                };
                println!(
                    "{:<13} {:<16} {:<8} {:<12} {:<5} {}",
                    d.id,
                    d.name,
                    d.scope.label(),
                    seen,
                    if d.push > 0 { "on" } else { "-" },
                    d.last_via.unwrap_or_default()
                );
            }
            Ok(0)
        }
        DevicesCmd::Revoke { id } => {
            c.request(Request::DeviceRevoke { id: id.clone() }).await?;
            eprintln!("revoked {id}: its token no longer works and its connections closed");
            Ok(0)
        }
        DevicesCmd::PushTest => {
            if let Reply::Notes { notes } = c.request(Request::PushTest).await? {
                for n in notes {
                    println!("{n}");
                }
            }
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(proxy: &str) -> serde_json::Value {
        serde_json::json!({
            "TCP": { "443": { "HTTPS": true } },
            "Web": { "box.tail1234.ts.net:443": { "Handlers": { "/": { "Proxy": proxy } } } }
        })
    }

    #[test]
    fn the_port_must_be_the_port_and_not_the_start_of_one() {
        let s = status("http://127.0.0.1:7091");
        assert!(proxies_to(&s, "7091"));
        assert!(!proxies_to(&s, "709"), "a prefix of the port isn't the port");
        assert!(!proxies_to(&s, "70910"));
        assert!(proxies_to(&status("http://localhost:7091"), "7091"));
        assert!(proxies_to(&status("127.0.0.1:7091/x"), "7091"));
        assert!(!proxies_to(&status("http://example.com:7091"), "7091"));
        assert!(!proxies_to(&serde_json::json!({}), "7091"));
    }

    #[test]
    fn something_else_on_443_is_not_ours_to_replace() {
        // Ours already, or nothing there: fine to (re)point it.
        assert_eq!(serves_443_elsewhere(&status("http://127.0.0.1:7091"), 7091), None);
        assert_eq!(serves_443_elsewhere(&serde_json::json!({}), 7091), None);
        // Another service: say what.
        assert_eq!(
            serves_443_elsewhere(&status("http://127.0.0.1:3000"), 7091).as_deref(),
            Some("http://127.0.0.1:3000")
        );
    }
}
