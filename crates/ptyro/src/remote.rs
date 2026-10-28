//! Remote access from the command line (SPEC §16, M9): `ptyro proxy` (what `--remote` runs over
//! ssh), `ptyro pair` (a QR code that opens the web app signed in) and `ptyro devices`.

use ptyro_core::Error;
use ptyro_core::config::{Access, Config};
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

/// The scope `ptyro pair` gives when none is named: the whole web app needs `full`. Every place
/// that pairs says so in plain words (`full_warning`) and offers the lower scopes.
pub const DEFAULT_SCOPE: Scope = Scope::Full;

/// What a device with `full` scope can do, said so the owner can't miss it. Shown wherever the
/// default scope was used (not when a scope was asked for).
pub fn full_warning() -> &'static str {
    "This device gets FULL control: it can run commands as you, change settings, and pair or \
     revoke other devices. Pair only your own phone or browser. Use --scope approve (answer \
     prompts only) or --scope control (also type into sessions) for less."
}

/// This machine's Tailscale addresses (`tailscale status --json`'s `Self.TailscaleIPs`), empty
/// when Tailscale isn't installed or is down.
pub fn tailscale_ips() -> Vec<std::net::IpAddr> {
    let Some(v) = tailscale_status() else { return Vec::new() };
    v["Self"]["TailscaleIPs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|ip| ip.as_str()?.parse::<std::net::IpAddr>().ok())
        .filter(|ip| ptyro_core::config::safe_address(*ip) && !ip.is_loopback())
        .collect()
}

/// This machine's MagicDNS name (`box.tailnet.ts.net`), if Tailscale is up.
pub fn tailscale_name() -> Option<String> {
    let v = tailscale_status()?;
    let name = v["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    (!name.is_empty()).then(|| name.to_lowercase())
}

fn tailscale_status() -> Option<serde_json::Value> {
    let out = std::process::Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// `host` as the authority of a URL: an IPv6 address gets its brackets.
fn authority(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Where the web app is reached from a phone: `[remote] public_url` as written, else the address
/// `[remote] access` makes reachable (this computer's loopback; its Tailscale name; its LAN
/// address) with `[remote] port`.
pub fn base_url(config: &Config, https: bool) -> String {
    if let Some(u) = &config.remote.public_url {
        return u.trim_end_matches('/').to_owned();
    }
    let r = &config.remote;
    let lan = || {
        crate::tls::local_addresses()
            .into_iter()
            .find(|ip| !ip.is_loopback())
            .map(|ip| ip.to_string())
    };
    let host = match r.access {
        Access::Localhost => None,
        Access::Tailscale => {
            tailscale_name().or_else(|| tailscale_ips().first().map(ToString::to_string))
        }
        Access::Open => lan().or_else(tailscale_name),
    }
    .unwrap_or_else(|| "127.0.0.1".into());
    format!("{}://{}", if https { "https" } else { "http" }, authority(&host, r.port))
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

/// Where this machine's web app is reached from elsewhere (its base URL), notes about it, its
/// certificate authority's fingerprint, and whether `ptyro serve` runs at all.
pub fn served_base(ctx: &Ctx, url: Option<String>) -> (String, Vec<String>, Option<String>, bool) {
    let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
    // The running `ptyro serve`'s own address (it knows its flags); else what config implies.
    let served = served(&ctx.paths);
    let mut notes = Vec::new();
    let base = match (&url, &served) {
        (Some(u), _) => u.trim_end_matches('/').to_owned(),
        (None, Some(s)) if config.remote.public_url.is_none() => s.url.clone(),
        _ => base_url(&config, true),
    };
    if url.is_none()
        && config.remote.public_url.is_none()
        && config.remote.access == Access::Localhost
    {
        notes.push(
            "that address only works on this computer. For a phone: `ptyro config set remote.access tailscale` (your Tailscale network) or `open` (the Wi-Fi too)"
                .into(),
        );
    }
    let ca = served.as_ref().and_then(|s| s.ca_fingerprint.clone());
    (base, notes, ca, served.is_some())
}

pub async fn pair(
    ctx: &Ctx,
    name: String,
    scope: Option<&str>,
    url: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    // No scope named: the default, with its warning. A named one is the owner's choice.
    let defaulted = scope.is_none();
    let scope: Scope = scope.map_or(Ok(DEFAULT_SCOPE), str::parse).map_err(Error::invalid)?;
    let warning = (defaulted && scope == Scope::Full).then(full_warning);
    let c = ctx.connect(true).await.map_err(no_daemon)?;
    // Which machine the link opens on, for people who pair several.
    let machine = match c.request(Request::Status).await {
        Ok(Reply::Status { status }) => status.machine,
        _ => None,
    };
    let (device, code, expires_ms, replaces) =
        match c.request(Request::DevicePair { name, scope, machine: false }).await? {
            Reply::Pairing { device, code, expires_ms, replaces } => {
                (device, code, expires_ms, replaces)
            }
            r => return Err(unexpected(r)),
        };
    let (base, notes, ca, _) = served_base(ctx, url);
    // What we print stays in scrollback and recordings, so it is a code the web app trades
    // (once, within minutes) for the token, never the token. It travels in the fragment:
    // browsers never send that to a server or put it in logs.
    let link = format!("{base}/#pair={code}");
    if json {
        println!(
            "{}",
            serde_json::json!({ "device": device, "url": link, "expires_ms": expires_ms, "ca": ca.as_ref().map(|f| serde_json::json!({ "url": format!("{base}/ca.crt"), "sha256": f })), "machine": machine, "warning": warning, "replaces": replaces, "notes": notes })
        );
        return Ok(0);
    }
    if let Some(m) = &machine {
        println!("Pairing {:?} with {m}.\n", device.name);
    }
    for n in &notes {
        eprintln!("note: {n}");
    }
    if let Some(w) = warning {
        eprintln!("WARNING: {w}\n");
    }
    if let Some(old) = &replaces {
        println!(
            "Replacing the existing device {:?} ({}, was {}, now {}): its old link and token stop \
             working when this one is opened.\n",
            old.name,
            old.id,
            old.scope.label(),
            scope.label()
        );
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
            _ if warning.is_some() =>
                "do everything the web app does, and anything you can on this machine (see the warning above)",
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
             and the pairing code in the link crosses the network in clear. Open the https:// \
             address `ptyro web status` shows, or leave `--url` out (docs/remote.md)."
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
                if let Some(old) = &replaces {
                    println!("{} was {}, now {}.", old.name, old.scope.label(), d.scope.label());
                }
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

    fn cfg(access: Access, port: u16, public: Option<&str>) -> Config {
        let mut c = Config::default();
        c.remote.access = access;
        c.remote.port = port;
        c.remote.public_url = public.map(str::to_owned);
        c
    }

    #[test]
    fn the_address_phones_open_follows_access_and_port() {
        // This computer only: loopback, on the port.
        let c = cfg(Access::Localhost, 7091, None);
        assert_eq!(base_url(&c, true), "https://127.0.0.1:7091");
        assert_eq!(base_url(&cfg(Access::Localhost, 8443, None), true), "https://127.0.0.1:8443");
        // The tour and the tests speak plain HTTP, on this computer.
        assert_eq!(base_url(&c, false), "http://127.0.0.1:7091");
        // `public_url` is what the owner wrote, minus a trailing slash.
        let c = cfg(Access::Open, 7091, Some("https://ptyro.example.com:8443/"));
        assert_eq!(base_url(&c, true), "https://ptyro.example.com:8443");
    }

    #[test]
    fn an_ipv6_host_gets_its_brackets() {
        assert_eq!(authority("fd7a:115c::1", 7091), "[fd7a:115c::1]:7091");
        assert_eq!(authority("100.64.0.9", 7091), "100.64.0.9:7091");
        assert_eq!(authority("box.tail1234.ts.net", 7091), "box.tail1234.ts.net:7091");
    }

    #[test]
    fn the_default_scope_is_full_and_says_what_that_means() {
        assert_eq!(DEFAULT_SCOPE, Scope::Full);
        assert_eq!(Scope::default(), Scope::Full);
        let w = full_warning();
        assert!(
            w.contains("FULL")
                && w.contains("run commands as you")
                && w.contains("--scope approve")
        );
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
}
