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
pub fn base_url(config: &Config, explicit: Option<String>) -> String {
    explicit
        .or_else(|| config.remote.public_url.clone())
        .or_else(|| tailscale_url(&config.remote.listen))
        .unwrap_or_else(|| format!("http://{}", config.remote.listen))
        .trim_end_matches('/')
        .to_owned()
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
    let (device, token) = match c.request(Request::DevicePair { name, scope }).await? {
        Reply::Paired { device, token } => (device, token),
        r => return Err(unexpected(r)),
    };
    let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
    let base = base_url(&config, url);
    // The token travels in the fragment: browsers never send it to a server or put it in logs.
    let link = format!("{base}/#pair={token}");
    if json {
        println!("{}", serde_json::json!({ "device": device, "url": link }));
        return Ok(0);
    }
    if let Some(q) = qr(&link) {
        println!("{q}");
    }
    println!("Scan with the phone's camera, or open:\n  {link}\n");
    println!(
        "Paired {} ({}, scope {}). It stays paired until `ptyro devices revoke {}`.",
        device.name,
        device.id,
        device.scope.label(),
        device.id
    );
    if !base.starts_with("https://") && !base.contains("127.0.0.1") && !base.contains("localhost") {
        eprintln!(
            "note: {base} isn't HTTPS, so phones won't install the app or show notifications. \
             `ptyro serve --tailscale` puts Tailscale HTTPS in front (docs/remote.md)."
        );
    }
    Ok(0)
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
                let seen = d.last_seen_ms.map_or_else(|| "never".into(), crate::cli::age);
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
