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

/// The port a URL names, if it names one (`https://[::1]:8443/x` is 8443; `https://box` none).
pub fn url_port(url: &str) -> Option<u16> {
    let (_, rest) = url.split_once("://")?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit_once('@').map_or(auth, |x| x.1);
    let port =
        if auth.starts_with('[') { auth.split_once("]:")?.1 } else { auth.rsplit_once(':')?.1 };
    port.parse().ok()
}

/// `url` with `port` written out when it names none: `https://box.ts.net` becomes
/// `https://box.ts.net:443`, `http://[::1]` becomes `http://[::1]:7091`.
pub fn with_port(url: &str, port: u16) -> String {
    if url_port(url).is_some() {
        return url.to_owned();
    }
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    format!("{scheme}://{}:{port}{}", &rest[..end], &rest[end..])
}

/// `url` with its port replaced by (or given) `port`.
pub fn set_port(url: &str, port: u16) -> String {
    let Some(old) = url_port(url) else { return with_port(url, port) };
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = rest[..end].strip_suffix(&format!(":{old}")).unwrap_or(&rest[..end]);
    format!("{scheme}://{auth}:{port}{}", &rest[end..])
}

fn default_port(url: &str) -> u16 {
    if url.starts_with("https://") { 443 } else { 80 }
}

/// A listen address phones can open: a wildcard (`0.0.0.0`, `::`) is every interface, not a
/// place, so it becomes this machine's LAN address (else loopback). The port stays.
pub fn reachable(listen: &str) -> String {
    let Ok(addr) = listen.parse::<std::net::SocketAddr>() else { return listen.to_owned() };
    if !addr.ip().is_unspecified() {
        return addr.to_string();
    }
    let ip = crate::tls::local_addresses()
        .into_iter()
        .find(|ip| !ip.is_loopback() && ip.is_ipv4() == addr.is_ipv4())
        .or_else(|| crate::tls::local_addresses().into_iter().find(|ip| !ip.is_loopback()))
        .unwrap_or_else(|| std::net::Ipv4Addr::LOCALHOST.into());
    std::net::SocketAddr::new(ip, addr.port()).to_string()
}

/// Where the web app is reached from a phone: `--url`, `[remote] public_url`, Tailscale HTTPS
/// in front of the listen port, or the listen address itself.
pub fn base_url(config: &Config, explicit: Option<String>, https: bool) -> String {
    resolve_base(config, explicit, https, tailscale_url(&config.remote.listen)).0
}

/// The link base and what to tell the user about it. The rule (SPEC §16): the port is always
/// written out. An address ptyro builds (listen address, Tailscale's name on 443) carries its
/// port. An address the user gave without a port is a reverse proxy or Tailscale on the scheme's
/// default port when ptyro sits behind one (`trusted_proxy`, or the Tailscale name), so that
/// port is written out; otherwise ptyro itself serves it, on its listen port, which is added
/// (a bare host would reach nothing). A given port that isn't the one ptyro listens on, with
/// no proxy in front, is kept and warned about.
pub fn resolve_base(
    config: &Config,
    explicit: Option<String>,
    https: bool,
    tailscale: Option<String>,
) -> (String, Vec<String>) {
    let scheme = if https { "https" } else { "http" };
    let listen = reachable(&config.remote.listen);
    let listen_port = listen.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
    let given = explicit.or_else(|| config.remote.public_url.clone());
    let mut notes = Vec::new();
    let url = match (given, tailscale) {
        (Some(u), _) => {
            let u = u.trim_end_matches('/').to_owned();
            if u.split_once("://").is_none() {
                notes.push(format!(
                    "{u} has no http:// or https:// in front; use e.g. --url {scheme}://{u}"
                ));
                u
            } else if url_port(&u).is_some() {
                if let (Some(p), Some(l)) = (url_port(&u), listen_port)
                    && p != l
                    && !config.remote.trusted_proxy
                {
                    notes.push(format!(
                        "{u} names port {p}, but ptyro listens on {l}: phones reach nothing \
                         unless a proxy forwards {p} to it. Use --url {}, or set `[remote] \
                         trusted_proxy = true` if a proxy does",
                        set_port(&u, l)
                    ));
                }
                u
            } else if config.remote.trusted_proxy {
                with_port(&u, default_port(&u))
            } else if let Some(l) = listen_port {
                if l != default_port(&u) {
                    notes.push(format!(
                        "added :{l} to {u}, the port ptyro listens on. If a reverse proxy serves \
                         it on :{} instead, set `[remote] trusted_proxy = true`",
                        default_port(&u)
                    ));
                }
                with_port(&u, l)
            } else {
                with_port(&u, default_port(&u))
            }
        }
        (None, Some(ts)) => with_port(ts.trim_end_matches('/'), 443),
        (None, None) => format!("{scheme}://{listen}"),
    };
    (url, notes)
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
        match c.request(Request::DevicePair { name, scope }).await? {
            Reply::Pairing { device, code, expires_ms, replaces } => {
                (device, code, expires_ms, replaces)
            }
            r => return Err(unexpected(r)),
        };
    let mut config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
    // The running `ptyro serve`'s own address (it knows its flags); else what config implies.
    let served = served(&ctx.paths);
    let https = !(config.remote.insecure || config.remote.trusted_proxy);
    // `ptyro serve --listen` can differ from the config: what it listens on is what counts.
    if let Some(l) = served.as_ref().and_then(|s| s.listen.clone()) {
        config.remote.listen = l;
    }
    let (base, notes) = match (&url, &served) {
        (None, Some(s)) if config.remote.public_url.is_none() => {
            (with_port(&s.url, default_port(&s.url)), Vec::new())
        }
        _ => resolve_base(&config, url, https, tailscale_url(&config.remote.listen)),
    };
    let ca = served.as_ref().and_then(|s| s.ca_fingerprint.clone());
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

    fn cfg(listen: &str, public: Option<&str>, proxy: bool) -> Config {
        let mut c = Config::default();
        c.remote.listen = listen.into();
        c.remote.public_url = public.map(str::to_owned);
        c.remote.trusted_proxy = proxy;
        c
    }

    fn base(c: &Config, url: Option<&str>, ts: Option<&str>) -> (String, Vec<String>) {
        resolve_base(c, url.map(str::to_owned), true, ts.map(str::to_owned))
    }

    #[test]
    fn ports_are_found_and_written_out() {
        assert_eq!(url_port("https://[::1]:8443/x"), Some(8443));
        assert_eq!(url_port("https://[::1]/x"), None);
        assert_eq!(url_port("http://box"), None);
        assert_eq!(url_port("http://u:p@box:81"), Some(81));
        assert_eq!(with_port("http://[::1]", 7091), "http://[::1]:7091");
        assert_eq!(with_port("https://box.ts.net/a?b#c", 443), "https://box.ts.net:443/a?b#c");
        assert_eq!(with_port("http://10.0.0.5:80", 7091), "http://10.0.0.5:80");
        assert_eq!(set_port("https://box:8443/x", 7091), "https://box:7091/x");
        assert_eq!(set_port("https://[::1]:8443", 7091), "https://[::1]:7091");
    }

    #[test]
    fn built_links_carry_the_port_for_v4_v6_and_hostnames() {
        assert_eq!(
            base(&cfg("127.0.0.1:7091", None, false), None, None).0,
            "https://127.0.0.1:7091"
        );
        assert_eq!(base(&cfg("[::1]:7091", None, false), None, None).0, "https://[::1]:7091");
        assert_eq!(
            base(&cfg("192.168.1.9:8443", None, false), None, None).0,
            "https://192.168.1.9:8443"
        );
        assert_eq!(
            base(&cfg("localhost:7091", None, false), None, None).0,
            "https://localhost:7091"
        );
    }

    #[test]
    fn a_wildcard_listen_address_is_never_the_link() {
        for l in ["0.0.0.0:7091", "[::]:7091"] {
            let (u, _) = base(&cfg(l, None, false), None, None);
            assert!(u.ends_with(":7091"), "{u}");
            assert!(!u.contains("0.0.0.0") && !u.contains("[::]"), "{u}");
        }
    }

    #[test]
    fn tailscale_names_get_their_port() {
        let c = cfg("127.0.0.1:7091", None, false);
        assert_eq!(
            base(&c, None, Some("https://box.tail1.ts.net")).0,
            "https://box.tail1.ts.net:443"
        );
    }

    #[test]
    fn a_given_url_without_a_port_gets_the_one_that_serves_it() {
        // ptyro serves it itself: its own port.
        let c = cfg("192.168.1.9:7091", None, false);
        let (u, notes) = base(&c, Some("https://phone-host/"), None);
        assert_eq!(u, "https://phone-host:7091");
        assert!(notes.iter().any(|n| n.contains("trusted_proxy")), "{notes:?}");
        // Behind a proxy on the scheme's port: that port, written out.
        let c = cfg("127.0.0.1:7091", None, true);
        assert_eq!(
            base(&c, Some("https://ptyro.example.com"), None).0,
            "https://ptyro.example.com:443"
        );
        assert_eq!(
            resolve_base(&c, Some("http://ptyro.example.com".into()), false, None).0,
            "http://ptyro.example.com:80"
        );
        // `[remote] public_url` counts as given.
        let c = cfg("127.0.0.1:7091", Some("https://p.example.com"), true);
        assert_eq!(base(&c, None, None).0, "https://p.example.com:443");
    }

    #[test]
    fn a_given_port_is_kept_and_a_mismatch_is_warned_about_with_the_fix() {
        let c = cfg("127.0.0.1:7091", None, false);
        let (u, notes) = base(&c, Some("https://box:9000"), None);
        assert_eq!(u, "https://box:9000");
        assert!(
            notes.iter().any(|n| n.contains("9000") && n.contains("--url https://box:7091")),
            "{notes:?}"
        );
        // The right port: nothing to say. Behind a proxy: nothing to say either.
        assert!(base(&c, Some("https://box:7091"), None).1.is_empty());
        assert!(
            base(&cfg("127.0.0.1:7091", None, true), Some("https://box:9000"), None).1.is_empty()
        );
        // [::1] with a port is kept as is.
        assert_eq!(base(&c, Some("http://[::1]:7091"), None).0, "http://[::1]:7091");
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
