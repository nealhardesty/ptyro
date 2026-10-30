//! `ptyro serve`'s HTTPS (ROADMAP WEB1.7, SPEC §16.3): a small local certificate authority and a
//! server certificate it signs, made on first use and kept under `<data>/serve/tls/` (keys 0600).
//!
//! - The CA (ECDSA P-256, ten years) is what a phone trusts once: `/ca.crt`, offered by
//!   `ptyro pair`. It is only ever re-made when its files are gone.
//! - The server certificate names `localhost`, the loopback addresses, this machine's name (and
//!   `<name>.local`), its LAN and Tailscale addresses, and the Tailscale DNS names. It lasts 397
//!   days (Apple's limit for certificates from a root a user installed is 825), and is re-issued
//!   when it has fewer than 30 days left or when a name or address it should hold is missing.
//!
//! - On a tailnet with HTTPS certificates on, `tailscale cert` gets a real (Let's Encrypt) one for
//!   this machine's `*.ts.net` name (ROADMAP TS1). The server answers that name with it and every
//!   other name or address with its own, so a phone on the tailnet name needs nothing installed.
//!
//! rcgen can't read a certificate back, so `tls.json` keeps what remaking the CA's certificate
//! needs (its name, dates and serial): with the same key, it is the same CA to whoever trusts it.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ptyro_core::Error;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SerialNumber,
};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

const CA_DAYS: i64 = 3650;
const SERVER_DAYS: i64 = 397;
/// Re-issued with fewer days than this left.
pub const RENEW_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Meta {
    ca_name: String,
    ca_not_before: i64,
    ca_not_after: i64,
    ca_serial: u64,
    /// What the server certificate holds, and when it ends (unix seconds).
    names: Vec<String>,
    server_not_after: i64,
}

/// A rustls config and a short hash of the certificates it serves. `serve` puts the hash in
/// `/sw.js`: Chrome shows a page its service worker answers with the certificate the worker was
/// installed over, so a new certificate needs a new worker (SPEC §20.9).
#[derive(Clone)]
pub struct Served {
    pub config: Arc<rustls::ServerConfig>,
    pub stamp: String,
}

/// The certificates `serve` uses: its rustls config, the CA to hand out, and when the server
/// certificate ends.
pub struct Tls {
    pub config: Served,
    pub ca_pem: String,
    /// SHA-256 of the CA certificate (`AB:CD:…`), for checking it on a phone.
    pub ca_fingerprint: String,
    /// What it holds (every name it was ever made for; what the server answers to now comes from
    /// its configuration, not from here).
    #[cfg_attr(not(test), allow(dead_code))]
    pub names: Vec<String>,
}

pub fn dir(data: &Path) -> PathBuf {
    data.join("serve").join("tls")
}

/// The CA and a server certificate for `names` (DNS names and IP addresses), made or renewed as
/// needed (see the module docs).
pub fn load_or_make(
    dir: &Path,
    names: &[String],
    now: OffsetDateTime,
    tailnet: Option<&TailnetCert>,
) -> Result<Tls, Error> {
    let io = |what: &str, e: std::io::Error| Error::internal(format!("{what}: {e}"));
    let tls_err = |e: rcgen::Error| Error::internal(format!("making a certificate: {e}"));
    std::fs::create_dir_all(dir).map_err(|e| io("the certificate directory", e))?;
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
    let meta: Option<Meta> = read("tls.json").and_then(|t| serde_json::from_str(&t).ok());
    // The CA: kept for as long as its key and description are there.
    let (ca_key, meta, new_ca) = match (read("ca-key.pem"), meta) {
        (Some(pem), Some(m)) if read("ca.pem").is_some() => {
            (KeyPair::from_pem(&pem).map_err(tls_err)?, m, false)
        }
        _ => {
            let key = KeyPair::generate().map_err(tls_err)?;
            let m = Meta {
                ca_name: format!("ptyro local CA ({})", host_name()),
                ca_not_before: (now - Duration::days(1)).unix_timestamp(),
                ca_not_after: (now + Duration::days(CA_DAYS)).unix_timestamp(),
                ca_serial: u64::from_le_bytes(
                    now.unix_timestamp_nanos().to_le_bytes()[..8].try_into().expect("8 bytes"),
                ),
                names: Vec::new(),
                server_not_after: 0,
            };
            write_private(&dir.join("ca-key.pem"), &key.serialize_pem())?;
            (key, m, true)
        }
    };
    // Remade to sign with (the same CA to whoever trusts it); `ca.pem` stays the one first
    // made, so what phones installed, `/ca.crt` and its fingerprint never change.
    let ca = ca_params(&meta)?.self_signed(&ca_key).map_err(tls_err)?;
    let ca_pem = match read("ca.pem") {
        Some(pem) if !new_ca => pem,
        _ => {
            std::fs::write(dir.join("ca.pem"), ca.pem()).map_err(|e| io("ca.pem", e))?;
            ca.pem()
        }
    };
    let ca_der = pem_der(&ca_pem)?;
    // The server certificate: re-issued when it ends soon or misses a name.
    let mut wanted: Vec<String> = names.to_vec();
    wanted.sort();
    wanted.dedup();
    let fresh = meta.server_not_after - now.unix_timestamp() > RENEW_DAYS * 86_400;
    let covers = wanted.iter().all(|n| meta.names.contains(n));
    let have = (read("server.pem"), read("server-key.pem"));
    let (server_pem, key_pem, meta) = match have {
        (Some(c), Some(k)) if fresh && covers => (c, k, meta),
        _ => {
            // Whatever it held before stays (an address seen once may come back).
            let mut all = meta.names.clone();
            all.extend(wanted);
            all.sort();
            all.dedup();
            let key = KeyPair::generate().map_err(tls_err)?;
            let mut p = CertificateParams::new(all.clone()).map_err(tls_err)?;
            p.distinguished_name = DistinguishedName::new();
            p.distinguished_name.push(DnType::CommonName, format!("ptyro serve ({})", host_name()));
            p.not_before = now - Duration::days(1);
            p.not_after = now + Duration::days(SERVER_DAYS);
            p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            p.use_authority_key_identifier_extension = true;
            let not_after = p.not_after.unix_timestamp();
            let issuer = rcgen::Issuer::new(ca_params(&meta)?, &ca_key);
            let cert = p.signed_by(&key, &issuer).map_err(tls_err)?;
            let (c, k) = (cert.pem(), key.serialize_pem());
            std::fs::write(dir.join("server.pem"), &c).map_err(|e| io("server.pem", e))?;
            write_private(&dir.join("server-key.pem"), &k)?;
            let m = Meta { names: all, server_not_after: not_after, ..meta };
            let json = serde_json::to_string_pretty(&m).unwrap_or_default();
            std::fs::write(dir.join("tls.json"), json).map_err(|e| io("tls.json", e))?;
            (c, k, m)
        }
    };
    let config = server_config(&server_pem, &key_pem, Some(&ca_der), tailnet)?;
    Ok(Tls { config, ca_fingerprint: fingerprint(&ca_der), ca_pem, names: meta.names })
}

/// When the server certificate in `dir` ends, if there is one (`ptyro doctor`).
pub fn expires(dir: &Path) -> Option<OffsetDateTime> {
    let m: Meta =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tls.json")).ok()?).ok()?;
    OffsetDateTime::from_unix_timestamp(m.server_not_after).ok()
}

fn ca_params(m: &Meta) -> Result<CertificateParams, Error> {
    let mut p = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| Error::internal(format!("the CA: {e}")))?;
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name.push(DnType::CommonName, m.ca_name.clone());
    p.distinguished_name.push(DnType::OrganizationName, "ptyro");
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let at = |s: i64| OffsetDateTime::from_unix_timestamp(s).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    p.not_before = at(m.ca_not_before);
    p.not_after = at(m.ca_not_after);
    p.serial_number = Some(SerialNumber::from_slice(&m.ca_serial.to_be_bytes()));
    Ok(p)
}

/// Your own certificate (`--cert`/`--key`, `[remote] tls_cert`/`tls_key`): a PEM chain, leaf
/// first, and its key. Read again every 12 h by `serve`, so a renewed one is picked up.
pub fn from_files(cert: &Path, key: &Path) -> Result<Served, Error> {
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
    };
    server_config(&read(cert)?, &read(key)?, None, None)
}

fn server_config(
    cert_pem: &str,
    key_pem: &str,
    ca_der: Option<&[u8]>,
    tailnet: Option<&TailnetCert>,
) -> Result<Served, Error> {
    let bad = |e: String| Error::internal(format!("the server certificate: {e}"));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut chain = parse_chain(cert_pem).map_err(bad)?;
    let mut leaves = chain[0].to_vec();
    if let Some(ca) = ca_der {
        chain.push(rustls::pki_types::CertificateDer::from(ca.to_vec()));
    }
    let builder = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| bad(e.to_string()))?
        .with_no_client_auth();
    let mut config = match tailnet {
        None => {
            let key = parse_key(key_pem).map_err(bad)?;
            builder.with_single_cert(chain, key).map_err(|e| bad(e.to_string()))?
        }
        Some(t) => {
            let tailnet_chain = parse_chain(&t.cert_pem).map_err(bad)?;
            leaves.extend_from_slice(&tailnet_chain[0]);
            let certified = |chain, key: &str| {
                let key = parse_key(key).map_err(bad)?;
                rustls::sign::CertifiedKey::from_der(chain, key, &provider)
                    .map(Arc::new)
                    .map_err(|e| bad(e.to_string()))
            };
            builder.with_cert_resolver(Arc::new(ByName {
                tailnet_name: t.name.clone(),
                tailnet: certified(tailnet_chain, &t.key_pem)?,
                own: certified(chain, key_pem)?,
            }))
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let stamp = fingerprint(&leaves).replace(':', "")[..16].to_ascii_lowercase();
    Ok(Served { config: Arc::new(config), stamp })
}

fn parse_chain(pem: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let chain: Vec<_> = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if chain.is_empty() { Err("no certificate in it".into()) } else { Ok(chain) }
}

fn parse_key(pem: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, String> {
    use rustls::pki_types::{PrivateKeyDer, pem::PemObject};
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).map_err(|e| e.to_string())
}

/// Which certificate answers: the tailnet name gets Tailscale's, anything else (an address,
/// `localhost`, `<name>.local`, no name at all) gets ptyro's own.
#[derive(Debug)]
struct ByName {
    tailnet_name: String,
    tailnet: Arc<rustls::sign::CertifiedKey>,
    own: Arc<rustls::sign::CertifiedKey>,
}

impl rustls::server::ResolvesServerCert for ByName {
    fn resolve(
        &self,
        hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let asked = hello.server_name().map(str::to_ascii_lowercase);
        Some(Arc::clone(if asked.as_deref() == Some(self.tailnet_name.as_str()) {
            &self.tailnet
        } else {
            &self.own
        }))
    }
}

/// Tailscale's certificate for this machine's `*.ts.net` name (a PEM chain, leaf first, and its
/// key), as `tailscale cert` wrote it.
#[derive(Debug, Clone)]
pub struct TailnetCert {
    pub name: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub not_after: OffsetDateTime,
}

/// `tailscale cert` renews one with fewer days than this left, and does nothing before: asking
/// every 12 hours is cheap. (Let's Encrypt certificates last 90 days.)
const TAILNET_RENEW_DAYS: i64 = 14;
/// `tailscale cert` waits on Let's Encrypt the first time.
const TAILNET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn tailnet_dir(dir: &Path) -> PathBuf {
    dir.join("tailscale")
}

/// The certificate `tailscale cert` wrote for `name` under `dir`, if it is there, names `name`
/// and has not ended (a day of slack for clocks).
pub fn load_tailnet(dir: &Path, name: &str, now: OffsetDateTime) -> Option<TailnetCert> {
    let d = tailnet_dir(dir);
    let cert_pem = std::fs::read_to_string(d.join(format!("{name}.crt"))).ok()?;
    let key_pem = std::fs::read_to_string(d.join(format!("{name}.key"))).ok()?;
    let leaf = parse_chain(&cert_pem).ok()?.into_iter().next()?;
    let (_, x509) = x509_parser::parse_x509_certificate(&leaf).ok()?;
    let not_after =
        OffsetDateTime::from_unix_timestamp(x509.validity().not_after.timestamp()).ok()?;
    let names_it = x509.subject_alternative_name().ok().flatten().is_some_and(|san| {
        san.value.general_names.iter().any(|g| {
            matches!(g, x509_parser::extensions::GeneralName::DNSName(n) if n.eq_ignore_ascii_case(name))
        })
    });
    (names_it && not_after > now + Duration::days(1)).then_some(TailnetCert {
        name: name.to_owned(),
        cert_pem,
        key_pem,
        not_after,
    })
}

/// Ask Tailscale for the certificate for `name` (it renews one with fewer than the renewal
/// window left, and does nothing otherwise), writing it under `dir`. Blocks: run it off the
/// async threads.
pub fn fetch_tailnet(dir: &Path, name: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let d = tailnet_dir(dir);
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    let mut child = std::process::Command::new("tailscale")
        .arg("cert")
        .arg("--cert-file")
        .arg(d.join(format!("{name}.crt")))
        .arg("--key-file")
        .arg(d.join(format!("{name}.key")))
        .arg("--min-validity")
        .arg(format!("{}h", TAILNET_RENEW_DAYS * 24 + 24))
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("tailscale cert: {e}"))?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < TAILNET_TIMEOUT => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "tailscale cert didn't finish in {}s",
                    TAILNET_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("tailscale cert: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut e, &mut err);
    }
    Err(tailnet_error(err.trim()))
}

/// What `tailscale cert` said, with the fix when we know it.
fn tailnet_error(said: &str) -> String {
    let lower = said.to_lowercase();
    if lower.contains("permission") || lower.contains("access denied") || lower.contains("operator")
    {
        format!(
            "Tailscale won't let this user ask for a certificate: run `sudo tailscale set --operator=$USER` once ({said})"
        )
    } else if lower.contains("https") && (lower.contains("not enabled") || lower.contains("enable"))
    {
        format!(
            "your tailnet has no HTTPS certificates yet: turn on \"HTTPS Certificates\" at https://login.tailscale.com/admin/dns ({said})"
        )
    } else {
        format!("tailscale cert: {said}")
    }
}

fn pem_der(pem: &str) -> Result<Vec<u8>, Error> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    CertificateDer::from_pem_slice(pem.as_bytes())
        .map(|c| c.to_vec())
        .map_err(|e| Error::internal(format!("ca.pem: {e}")))
}

pub(crate) fn fingerprint(der: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn write_private(path: &Path, text: &str) -> Result<(), Error> {
    // Keys: 0600 from the first byte, replaced whole, never left half-written.
    crate::config::write_atomic(path, text, 0o600)
}

/// This machine's name (`uname -n`), without a domain.
pub fn host_name() -> String {
    std::process::Command::new("uname")
        .arg("-n")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .and_then(|n| n.split('.').next().map(str::to_owned))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

/// The local addresses traffic to a LAN, to the internet and to a tailnet leaves from: a UDP
/// socket "connected" to an address picks one, and nothing is sent.
pub fn local_addresses() -> Vec<IpAddr> {
    let probes = ["192.0.2.1:9", "100.100.100.100:9", "[2001:db8::1]:9"];
    let mut out: Vec<IpAddr> = Vec::new();
    for p in probes {
        let bind = if p.starts_with('[') { "[::]:0" } else { "0.0.0.0:0" };
        if let Ok(s) = std::net::UdpSocket::bind(bind)
            && s.connect(p).is_ok()
            && let Ok(a) = s.local_addr()
            && !a.ip().is_unspecified()
            && !out.contains(&a.ip())
        {
            out.push(a.ip());
        }
    }
    out
}

/// Everything the server certificate should name: `localhost`, loopback, this machine, its
/// addresses, and `extra` (the listen address, Tailscale names).
pub fn names_for(extra: &[String]) -> Vec<String> {
    let host = host_name();
    let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned(), "::1".to_owned()];
    if host != "localhost" {
        names.push(host.to_lowercase());
        names.push(format!("{}.local", host.to_lowercase()));
    }
    names.extend(local_addresses().iter().map(IpAddr::to_string));
    names.extend(extra.iter().filter(|n| !n.is_empty() && *n != "0.0.0.0" && *n != "::").cloned());
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap()
    }

    #[test]
    fn certificates_are_made_once_kept_private_and_renewed_when_needed() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        let first = load_or_make(d.path(), &names, now(), None).unwrap();
        for k in ["ca-key.pem", "server-key.pem"] {
            let mode = std::fs::metadata(d.path().join(k)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{k}");
        }
        assert_eq!(expires(d.path()), Some(now() + Duration::days(SERVER_DAYS)));
        let server = std::fs::read_to_string(d.path().join("server.pem")).unwrap();
        // Again: nothing new.
        let again = load_or_make(d.path(), &names, now() + Duration::days(10), None).unwrap();
        assert_eq!(again.ca_fingerprint, first.ca_fingerprint);
        assert_eq!(std::fs::read_to_string(d.path().join("server.pem")).unwrap(), server);
        // A new address: a new server certificate, the same CA (phones keep trusting it).
        let more = [names.clone(), vec!["192.168.1.20".into()]].concat();
        let moved = load_or_make(d.path(), &more, now() + Duration::days(11), None).unwrap();
        assert_eq!(moved.ca_fingerprint, first.ca_fingerprint);
        assert!(moved.names.contains(&"192.168.1.20".to_owned()));
        assert_ne!(std::fs::read_to_string(d.path().join("server.pem")).unwrap(), server);
        // Ending soon (it was issued on day 11, so 25 days left): renewed, still the same CA.
        let late = now() + Duration::days(11 + SERVER_DAYS - RENEW_DAYS + 5);
        let renewed = load_or_make(d.path(), &more, late, None).unwrap();
        assert_eq!(renewed.ca_fingerprint, first.ca_fingerprint);
        assert_eq!(expires(d.path()), Some(late + Duration::days(SERVER_DAYS)));
        // The CA gone: a new one.
        std::fs::remove_file(d.path().join("ca-key.pem")).unwrap();
        let other = load_or_make(d.path(), &more, late, None).unwrap();
        assert_ne!(other.ca_fingerprint, first.ca_fingerprint);
    }

    #[test]
    fn the_certificate_names_this_machine() {
        let n = names_for(&["workbox.tail1234.ts.net".into(), "0.0.0.0".into()]);
        assert!(n.contains(&"localhost".to_owned()) && n.contains(&"::1".to_owned()));
        assert!(n.contains(&"workbox.tail1234.ts.net".to_owned()));
        assert!(!n.contains(&"0.0.0.0".to_owned()));
    }

    /// A certificate for `names` valid from `from` to `to`, as `tailscale cert` would write it.
    fn tailnet_files(dir: &Path, name: &str, names: &[&str], from: i64, to: i64) {
        let key = KeyPair::generate().unwrap();
        let mut p =
            CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
                .unwrap();
        p.not_before = OffsetDateTime::from_unix_timestamp(from).unwrap();
        p.not_after = OffsetDateTime::from_unix_timestamp(to).unwrap();
        let cert = p.self_signed(&key).unwrap();
        let d = tailnet_dir(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{name}.crt")), cert.pem()).unwrap();
        std::fs::write(d.join(format!("{name}.key")), key.serialize_pem()).unwrap();
    }

    #[test]
    fn tailscales_certificate_is_used_only_while_it_names_the_machine_and_has_not_ended() {
        let d = tempfile::tempdir().unwrap();
        let n = now().unix_timestamp();
        let day = 86_400;
        let name = "box.tail1.ts.net";
        assert!(load_tailnet(d.path(), name, now()).is_none(), "nothing there yet");
        tailnet_files(d.path(), name, &[name], n - day, n + 60 * day);
        let t = load_tailnet(d.path(), name, now()).expect("good for 60 more days");
        assert_eq!(t.name, name);
        assert_eq!(t.not_after.unix_timestamp(), n + 60 * day);
        // The same file for a name it doesn't hold, and when it has ended.
        assert!(load_tailnet(d.path(), "other.tail1.ts.net", now()).is_none());
        assert!(load_tailnet(d.path(), name, now() + Duration::days(60)).is_none());
        tailnet_files(d.path(), "wrong.tail1.ts.net", &["elsewhere.ts.net"], n - day, n + 60 * day);
        assert!(load_tailnet(d.path(), "wrong.tail1.ts.net", now()).is_none(), "SAN must match");
    }

    #[test]
    fn the_tailnet_name_gets_tailscales_certificate_and_nothing_else_does() {
        let d = tempfile::tempdir().unwrap();
        let n = now().unix_timestamp();
        let name = "box.tail1.ts.net";
        tailnet_files(d.path(), name, &[name], n - 86_400, n + 60 * 86_400);
        let t = load_tailnet(d.path(), name, now()).unwrap();
        let names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        let own = tempfile::tempdir().unwrap();
        // Builds with and without it; a mismatched key is refused, not served.
        assert!(load_or_make(own.path(), &names, now(), None).is_ok());
        assert!(load_or_make(own.path(), &names, now(), Some(&t)).is_ok());
        let bad = TailnetCert { key_pem: KeyPair::generate().unwrap().serialize_pem(), ..t };
        assert!(load_or_make(own.path(), &names, now(), Some(&bad)).is_err());
    }

    #[test]
    fn the_stamp_changes_with_the_certificates_served() {
        let d = tempfile::tempdir().unwrap();
        let n = now().unix_timestamp();
        let name = "box.tail1.ts.net";
        tailnet_files(d.path(), name, &[name], n - 86_400, n + 60 * 86_400);
        let t = load_tailnet(d.path(), name, now()).unwrap();
        let names = vec!["localhost".to_owned()];
        let own = tempfile::tempdir().unwrap();
        let stamp =
            |tailnet| load_or_make(own.path(), &names, now(), tailnet).unwrap().config.stamp;
        let first = stamp(None);
        assert_eq!(first.len(), 16);
        assert_eq!(stamp(None), first, "the same certificates: the same service worker");
        let with_tailnet = stamp(Some(&t));
        assert_ne!(with_tailnet, first, "Tailscale's certificate arriving");
        let more = [names.clone(), vec!["192.168.1.20".into()]].concat();
        let renewed = load_or_make(own.path(), &more, now(), Some(&t)).unwrap().config.stamp;
        assert_ne!(renewed, with_tailnet, "ptyro's own certificate re-issued");
    }

    #[test]
    fn tailscale_errors_come_with_the_fix() {
        let e = tailnet_error("Access denied: cert access denied");
        assert!(e.contains("set --operator"), "{e}");
        let e = tailnet_error("HTTPS is not enabled on your tailnet");
        assert!(e.contains("HTTPS Certificates") && e.contains("admin/dns"), "{e}");
        assert_eq!(tailnet_error("boom"), "tailscale cert: boom");
    }
}
