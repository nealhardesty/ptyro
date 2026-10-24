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
}

pub fn record_serve(paths: &ptyro_core::paths::Paths, url: &str, ca: Option<&str>) {
    let s =
        Served { pid: std::process::id(), url: url.into(), ca_fingerprint: ca.map(str::to_owned) };
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
    rustix::process::test_kill_process(pid).is_ok().then_some(s)
}

/// `https://<machine>.<tailnet>.ts.net` when `tailscale serve` proxies to our port.
pub fn tailscale_url(listen: &str) -> Option<String> {
    let port = listen.rsplit(':').next()?;
    let serve = std::process::Command::new("tailscale")
        .args(["serve", "status", "--json"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&serve.stdout);
    if !(text.contains(&format!("127.0.0.1:{port}")) || text.contains(&format!("localhost:{port}")))
    {
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
            "note: {base} isn't HTTPS, so phones won't install the app or show notifications. \
             Drop `insecure` (ptyro speaks HTTPS by default), or put Tailscale HTTPS in front \
             with `ptyro serve --tailscale` (docs/remote.md)."
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
