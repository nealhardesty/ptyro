//! Remote access from the command line (SPEC §16, M9): `ptyro proxy` (what `--remote` runs over
//! ssh), `ptyro pair` (a QR code that opens the web app signed in) and `ptyro devices`.

use ptyro_core::config::{Access, Config};
use ptyro_core::{Error, ErrorCode};
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
    let out = ptyro_core::os::tailscale().args(["status", "--json"]).output().ok()?;
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
    /// Whose certificate it shows: `own` (`tls_cert`), `tailscale` (Tailscale's for
    /// `tailscale_name`, ptyro's CA for the rest), `ptyro`, or empty (plain HTTP).
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub tailscale_name: Option<String>,
    /// Why Tailscale's certificate isn't in use, with the fix.
    #[serde(default)]
    pub cert_note: Option<String>,
}

pub fn record_serve(
    paths: &ptyro_core::paths::Paths,
    url: &str,
    ca: Option<&str>,
    listen: std::net::SocketAddr,
    cert: &str,
    tailscale_name: Option<&str>,
) {
    let s = Served {
        pid: std::process::id(),
        url: url.into(),
        ca_fingerprint: ca.map(str::to_owned),
        listen: Some(listen.to_string()),
        cert: cert.into(),
        tailscale_name: tailscale_name.map(str::to_owned),
        cert_note: None,
    };
    let _ = std::fs::create_dir_all(&paths.state);
    let _ = std::fs::write(
        paths.state.join("serve.json"),
        serde_json::to_string(&s).unwrap_or_default(),
    );
}

/// Say in the record which certificate is in use now, and why Tailscale's isn't if it should be.
pub fn record_cert(
    paths: &ptyro_core::paths::Paths,
    cert: &str,
    tailscale_name: Option<&str>,
    note: Option<&str>,
) {
    let path = paths.state.join("serve.json");
    let Some(mut s) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Served>(&t).ok())
        .filter(|s| s.pid == std::process::id())
    else {
        return;
    };
    s.cert = cert.into();
    s.tailscale_name = tailscale_name.map(str::to_owned);
    s.cert_note = note.map(str::to_owned);
    let _ = std::fs::write(&path, serde_json::to_string(&s).unwrap_or_default());
}

/// Whether `base` is an address the running server answers with Tailscale's certificate, which
/// phones already trust.
pub fn public_cert_for(served: Option<&Served>, base: &str) -> bool {
    let host = base.split_once("://").map_or(base, |x| x.1);
    let host = host.split(['/', ':']).next().unwrap_or("").to_ascii_lowercase();
    served.is_some_and(|s| {
        s.cert == "tailscale" && s.tailscale_name.as_deref().is_some_and(|n| n == host)
    })
}

/// Why a pairing link opens on a certificate warning: Tailscale's certificate, which every
/// device trusts, should be in use and isn't, so the device gets ptyro's own, which it trusts
/// only once its CA is installed. Browsers won't install the app over a certificate they
/// don't trust. `None` when Tailscale's is in use or wasn't meant to be.
pub fn untrusted_link(served: Option<&Served>, public: bool) -> Option<String> {
    let s = served.filter(|s| !public && s.cert != "own")?;
    let note = s.cert_note.as_deref()?;
    // The fix, without the raw `tailscale cert` output it ends with.
    let fix = note.split_once(" (").map_or(note, |x| x.0);
    let fix = fix.lines().next().unwrap_or(fix);
    let mut c = fix.chars();
    let fix = c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default();
    Some(format!(
        "this link opens on a certificate warning, and the browser won't install the app, until \
         the device trusts ptyro's certificate: Tailscale's, which every device trusts already, \
         isn't in use. {fix}; the web app takes it up within 10 minutes (`ptyro web off` and `on` \
         at once), then pair again"
    ))
}

/// The running `ptyro serve`'s record, if it's still running.
pub fn served(paths: &ptyro_core::paths::Paths) -> Option<Served> {
    let s: Served =
        serde_json::from_str(&std::fs::read_to_string(paths.state.join("serve.json")).ok()?)
            .ok()?;
    if !ptyro_pty::alive(s.pid) {
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

/// PowerShell, to tell whether Windows' user store trusts the CA whose SHA-256 is `fingerprint`
/// (`AB:CD:…`): prints `True` or `False`.
fn trust_check_script(fingerprint: &str) -> String {
    let hex: String = fingerprint.chars().filter(char::is_ascii_hexdigit).collect();
    format!(
        "$h = '{hex}'; [bool](Get-ChildItem Cert:\\CurrentUser\\Root | Where-Object {{ \
         ([BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($_.RawData)) \
         -replace '-', '') -eq $h }})"
    )
}

/// Windows' PowerShell, from inside WSL (`w`).
fn powershell(w: &ptyro_core::wsl::Wsl) -> std::process::Command {
    let mut c = std::process::Command::new(w.windows_exe("powershell.exe"));
    c.current_dir(std::path::Path::new(&w.mount_root).join("c"));
    c
}

/// Whether Windows trusts ptyro's CA for this user, so its browsers open the web app without a
/// warning: asked from inside WSL (`w`). `None`: PowerShell didn't say.
fn windows_trusts(w: &ptyro_core::wsl::Wsl, fingerprint: &str) -> Option<bool> {
    let out = powershell(w)
        .args(["-NoProfile", "-NonInteractive", "-Command", &trust_check_script(fingerprint)])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "True" => Some(true),
        "False" => Some(false),
        _ => None,
    }
}

/// Put ptyro's CA in Windows' trusted roots for this user (no administrator needed). Windows
/// shows its own "install this certificate?" dialog, so nothing is trusted without a click there.
fn trust_on_windows(w: &ptyro_core::wsl::Wsl, ca_pem: &str) -> Result<(), Error> {
    let file = w
        .windows_home()
        .ok_or_else(|| Error::internal("no Windows home (interop?)"))?
        .join("AppData/Local/Temp/ptyro-ca.crt");
    std::fs::write(&file, ca_pem)
        .map_err(|e| Error::internal(format!("{}: {e}", file.display())))?;
    let win = w.windows_path(&file).unwrap_or_default();
    let script = format!(
        "Import-Certificate -FilePath '{}' -CertStoreLocation Cert:\\CurrentUser\\Root | Out-Null",
        win.replace('\'', "''")
    );
    let status = powershell(w)
        .args(["-NoProfile", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_file(&file);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(Error::invalid(
            "Windows didn't add it (the dialog was declined?): the browser warns first, and \
             continuing past the warning works too",
        )),
        Err(e) => Err(Error::internal(format!("powershell.exe: {e}"))),
    }
}

/// The NSS certificate databases this user's Linux browsers read: Chrome's and Chromium's
/// shared one (`~/.pki/nssdb`, snap Chromium's own) and each Firefox profile's (also snap's and
/// Flatpak's). Only ones that exist: a browser that never ran has nothing to trust in.
fn browser_stores(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = [".pki/nssdb", "snap/chromium/current/.pki/nssdb"]
        .iter()
        .map(|d| home.join(d))
        .filter(|d| d.join("cert9.db").is_file())
        .collect();
    for root in [
        ".mozilla/firefox",
        "snap/firefox/common/.mozilla/firefox",
        ".var/app/org.mozilla.firefox/.mozilla/firefox",
    ] {
        let Ok(dir) = std::fs::read_dir(home.join(root)) else { continue };
        let mut profiles: Vec<_> =
            dir.flatten().map(|e| e.path()).filter(|p| p.join("cert9.db").is_file()).collect();
        profiles.sort();
        out.extend(profiles);
    }
    out
}

/// The name ptyro's CA goes by in an NSS database: one per CA, so a new CA is a new entry.
fn nss_nickname(fingerprint: &str) -> String {
    let hex: String = fingerprint.chars().filter(char::is_ascii_hexdigit).take(16).collect();
    format!("ptyro local CA {hex}")
}

/// Whether `db` holds ptyro's CA. `None`: `certutil` isn't installed.
fn nss_trusts(db: &std::path::Path, nickname: &str) -> Option<bool> {
    let out = std::process::Command::new("certutil")
        .args(["-L", "-d"])
        .arg(format!("sql:{}", db.display()))
        .args(["-n", nickname])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(out.status.success())
}

/// Offer to trust ptyro's CA in the browsers' stores (`dbs`) that don't yet, so `pair --open`
/// opens without a warning. Asks first (not with `--json`, which can't). What to tell the user
/// when the browser will still warn.
fn trust_on_linux(
    dbs: &[std::path::PathBuf],
    fingerprint: &str,
    ca_pem: &std::path::Path,
    json: bool,
) -> Option<String> {
    if dbs.is_empty() {
        return Some(
            "the browser may warn about ptyro's certificate first: there is no Chrome, Chromium \
             or Firefox store here to trust it in; continue past the warning"
                .into(),
        );
    }
    let nick = nss_nickname(fingerprint);
    let mut missing = Vec::new();
    for db in dbs {
        match nss_trusts(db, &nick) {
            Some(true) => {}
            Some(false) => missing.push(db),
            None => {
                return Some(
                    "the browser warns about ptyro's certificate first: install certutil \
                     (libnss3-tools on Debian and Ubuntu, nss-tools on Fedora, nss on Arch) and \
                     run `ptyro pair --open` again to trust it, or continue past the warning"
                        .into(),
                );
            }
        }
    }
    if missing.is_empty() {
        return None;
    }
    let later = "the browser warns about ptyro's certificate first (this computer's browsers \
                 don't trust it yet): run `ptyro pair --open` in a terminal to trust it, or \
                 continue past the warning";
    if json
        || !crate::cli::confirm(
            "This computer's browsers (Chrome, Chromium, Firefox) don't trust ptyro's certificate \
             yet, so the page would open on a warning. Trust it, for your user only?",
        )
    {
        return Some(later.into());
    }
    let mut failed = Vec::new();
    for db in missing {
        let ok = std::process::Command::new("certutil")
            .args(["-A", "-d"])
            .arg(format!("sql:{}", db.display()))
            .args(["-t", "C,,", "-n", &nick, "-i"])
            .arg(ca_pem)
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            failed.push(db.display().to_string());
        }
    }
    if failed.is_empty() {
        eprintln!("This computer's browsers trust ptyro's certificate now.");
        None
    } else {
        Some(format!(
            "certutil couldn't add ptyro's certificate to {}: that browser warns first; \
             continue past the warning",
            failed.join(", ")
        ))
    }
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
    open: bool,
) -> Result<i32, Error> {
    // A paired device opens the web app, so pairing turns it on (and keeps it on) when it is
    // off, as `ptyro web on` does: who can reach it is picked the same way (Tailscale when it
    // runs here). A code nothing could use is never handed out.
    if !served_base(ctx, url.clone()).3 {
        let (web, notes) = crate::webapp::ask(ctx, Some(true), None).await.map_err(no_daemon)?;
        if !web.up {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!(
                    "the web app, which a paired device opens, didn't start: {}",
                    crate::webapp::next(&web)
                ),
            ));
        }
        eprintln!(
            "Turned the web app on for the device to open (it stays on; `ptyro web off` turns it off)."
        );
        for n in notes {
            eprintln!("  {n}");
        }
    }
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
    let (base, mut notes, ca, _) = served_base(ctx, url);
    // Tailscale's certificate on this address: phones trust it already, no CA to hand out.
    let running = served(&ctx.paths);
    let public = public_cert_for(running.as_ref(), &base);
    let ca = ca.filter(|_| !public);
    if let Some(w) = untrusted_link(running.as_ref(), public) {
        notes.push(w);
    }
    // What we print stays in scrollback and recordings, so it is a code the web app trades
    // (once, within minutes) for the token, never the token. It travels in the fragment:
    // browsers never send that to a server or put it in logs.
    let link = format!("{base}/#pair={code}");
    // `--open` pairs the browser right here: the "only on this computer" note doesn't apply.
    let mut notes = if open { Vec::new() } else { notes };
    // From WSL, Windows' browsers check certificates against Windows' own store: without
    // ptyro's CA there, the page opens on a warning. Offer to put it there (Windows asks too).
    let wsl = ptyro_core::wsl::detect().filter(|w| w.interop);
    if open
        && let Some(f) = &ca
        && let Some(wsl) = wsl
        && windows_trusts(wsl, f) == Some(false)
    {
        let pem = std::fs::read_to_string(crate::tls::dir(&ctx.paths.data).join("ca.pem"));
        let asked = !json
            && crate::cli::confirm(
                "Windows doesn't trust ptyro's certificate yet, so the browser would show a \
                 warning first. Trust it for your Windows user? (Windows asks you to confirm)",
            );
        match pem {
            Ok(pem) if asked => match trust_on_windows(wsl, &pem) {
                Ok(()) => eprintln!("Windows trusts ptyro's certificate now."),
                Err(e) => eprintln!("note: {e}"),
            },
            _ => notes.push(
                "the browser warns about ptyro's certificate first (Windows doesn't trust it yet): \
                 run `ptyro pair --open` in a terminal to trust it, or continue past the warning"
                    .into(),
            ),
        }
    }
    // On Linux itself, Chrome, Chromium and Firefox check certificates against their own NSS
    // databases, not the system's: the same offer, for those (this user only, no sudo).
    if open
        && cfg!(target_os = "linux")
        && wsl.is_none()
        && let Some(f) = &ca
    {
        let ca_pem = crate::tls::dir(&ctx.paths.data).join("ca.pem");
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        if let Some(n) = trust_on_linux(&browser_stores(&home), f, &ca_pem, json) {
            notes.push(n);
        }
    }
    let opened = open && ptyro_core::open::open(&ptyro_core::open::Opened::Url(&link));
    if json {
        println!(
            "{}",
            serde_json::json!({ "device": device, "url": link, "expires_ms": expires_ms, "ca": ca.as_ref().map(|f| serde_json::json!({ "url": format!("{base}/ca.crt"), "sha256": f })), "machine": machine, "warning": warning, "replaces": replaces, "notes": notes, "opened": opened })
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
    let minutes = expires_ms.saturating_sub(ptyro_core::now_ms()).div_ceil(60_000);
    if opened {
        println!(
            "Opened in {} browser:\n  {link}\n",
            if ptyro_core::wsl::detect().is_some() { "Windows'" } else { "this computer's" }
        );
    } else {
        if open {
            eprintln!("note: nothing here opens a browser; open the link below by hand");
        }
        if let Some(q) = qr(&link) {
            println!("{q}");
        }
        println!("Scan with the phone's camera, or open:\n  {link}\n");
    }
    println!(
        "The link works once, for {minutes} minutes, and needs the web app on (`ptyro web on`). Waiting \
         for {} to open it (Ctrl-C stops waiting; the link keeps working)…",
        device.name
    );
    println!(
        "This device may {}.\n",
        match scope {
            _ if warning.is_some() =>
                "do everything the web app does, and anything you can on this machine (see the warning above)",
            Scope::Read => "watch: sessions and screens. `--scope approve` also answers prompts",
            Scope::Approve =>
                "watch and answer prompts. `--scope control` also types into sessions, \
                and starts configured agents; `--scope full` is the whole web app: tasks and \
                worktrees, the config, devices",
            Scope::Control =>
                "watch, answer, type and start configured agents. `--scope full` is the whole \
                web app: worktrees, the config, devices",
            Scope::Full => "do everything the web app does, and anything you can on this machine",
        }
    );
    if std::env::var_os("PTYRO_SESSION_ID").is_some() {
        eprintln!(
            "note: this pane is recorded and readable by paired devices and agents; until the \
             link is used or expires, so is the link. A plain terminal keeps it to you."
        );
    }
    if public {
        println!("This address uses Tailscale's certificate: nothing to install on the phone.\n");
    } else if let Some(f) = ca.as_ref().filter(|_| !opened) {
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
                    d.last_seen_ms.map_or_else(|| "never".into(), crate::cli::ago)
                };
                println!(
                    "{:<13} {:<16} {:<8} {:<12} {:<5} {}{}",
                    d.id,
                    d.name,
                    d.scope.label(),
                    seen,
                    if d.push > 0 { "on" } else { "-" },
                    d.last_via.unwrap_or_default(),
                    // Another machine's link (`machine pair`), not a person's device.
                    if d.machine { "  [machine link]" } else { "" }
                );
            }
            Ok(0)
        }
        DevicesCmd::Revoke { id, yes } => {
            // Found first, then asked about by its name: a typo asks nothing.
            let devices = match c.request(Request::DeviceList).await? {
                Reply::Devices { devices } => devices,
                r => return Err(unexpected(r)),
            };
            let d = ptyro_core::lookup::pick("device", &id, &devices, |d| &d.name, |d| &d.id)?;
            let (id, name) = (d.id.clone(), d.name.clone());
            crate::cli::ask_first(
                &format!("Revoke device {name} ({id})? {}", ptyro_core::removal::DEVICE),
                yes,
            )?;
            c.request(Request::DeviceRevoke { id }).await?;
            eprintln!("revoked {name}: its token no longer works and its connections closed");
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

    #[test]
    fn windows_trust_is_checked_by_the_sha256_ptyro_prints() {
        let s = trust_check_script("71:50:16:D0:2A");
        assert!(s.starts_with("$h = '715016D02A';"), "{s}");
        assert!(s.contains(r"Cert:\CurrentUser\Root") && s.contains("SHA256"), "{s}");
    }

    fn served_with(cert: &str, note: Option<&str>) -> Served {
        Served {
            pid: 1,
            url: "https://box.tail1234.ts.net:7091".into(),
            ca_fingerprint: Some("AB:CD".into()),
            listen: None,
            cert: cert.into(),
            tailscale_name: None,
            cert_note: note.map(str::to_owned),
        }
    }

    #[test]
    fn a_link_without_tailscales_certificate_says_why_and_the_fix() {
        let note = "Tailscale won't let this user ask for a certificate: run `sudo tailscale set \
                    --operator=$USER` once (Access denied: cert access denied\n\nUse 'sudo …'.)";
        let w = untrusted_link(Some(&served_with("ptyro", Some(note))), false).unwrap();
        assert!(w.contains("won't install the app"), "{w}");
        assert!(w.contains("run `sudo tailscale set --operator=$USER` once;"), "{w}");
        assert!(!w.contains("Access denied") && !w.contains('\n'), "{w}");
        let w = untrusted_link(Some(&served_with("ptyro", Some("your tailnet has no …"))), false);
        assert!(w.unwrap().contains("Your tailnet has no …;"));
        // Tailscale's in use, not meant to be, or the owner's own certificate: nothing to say.
        assert!(untrusted_link(Some(&served_with("tailscale", Some(note))), true).is_none());
        assert!(untrusted_link(Some(&served_with("ptyro", None)), false).is_none());
        assert!(untrusted_link(Some(&served_with("own", Some(note))), false).is_none());
        assert!(untrusted_link(None, false).is_none());
    }

    #[test]
    fn linux_browser_stores_are_the_ones_that_exist() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert!(browser_stores(h).is_empty());
        for d in [".pki/nssdb", ".mozilla/firefox/b.default", ".mozilla/firefox/a.other"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
            std::fs::write(h.join(d).join("cert9.db"), "").unwrap();
        }
        std::fs::create_dir_all(h.join(".mozilla/firefox/Crash Reports")).unwrap();
        let got = browser_stores(h);
        assert_eq!(
            got,
            [".pki/nssdb", ".mozilla/firefox/a.other", ".mozilla/firefox/b.default"]
                .map(|d| h.join(d))
        );
        // No store: the note says so instead of asking.
        let n = trust_on_linux(&[], "AB", std::path::Path::new("ca.pem"), true).unwrap();
        assert!(n.contains("no Chrome, Chromium or Firefox store"), "{n}");
        assert_eq!(
            nss_nickname("2C:8F:44:57:92:6E:A4:B7:10:BB"),
            "ptyro local CA 2C8F4457926EA4B7"
        );
    }

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
        // The tests speak plain HTTP, on this computer.
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
