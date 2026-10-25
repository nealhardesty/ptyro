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

/// The certificates `serve` uses: its rustls config, the CA to hand out, and when the server
/// certificate ends.
pub struct Tls {
    pub config: Arc<rustls::ServerConfig>,
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
pub fn load_or_make(dir: &Path, names: &[String], now: OffsetDateTime) -> Result<Tls, Error> {
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
            let cert = p.signed_by(&key, &ca, &ca_key).map_err(tls_err)?;
            let (c, k) = (cert.pem(), key.serialize_pem());
            std::fs::write(dir.join("server.pem"), &c).map_err(|e| io("server.pem", e))?;
            write_private(&dir.join("server-key.pem"), &k)?;
            let m = Meta { names: all, server_not_after: not_after, ..meta };
            let json = serde_json::to_string_pretty(&m).unwrap_or_default();
            std::fs::write(dir.join("tls.json"), json).map_err(|e| io("tls.json", e))?;
            (c, k, m)
        }
    };
    let config = server_config(&server_pem, &key_pem, Some(&ca_der))?;
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
pub fn from_files(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, Error> {
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
    };
    server_config(&read(cert)?, &read(key)?, None)
}

fn server_config(
    cert_pem: &str,
    key_pem: &str,
    ca_der: Option<&[u8]>,
) -> Result<Arc<rustls::ServerConfig>, Error> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let bad = |e: String| Error::internal(format!("the server certificate: {e}"));
    let mut chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .map_err(|e| bad(e.to_string()))?;
    if chain.is_empty() {
        return Err(bad("no certificate in it".into()));
    }
    if let Some(ca) = ca_der {
        chain.push(CertificateDer::from(ca.to_vec()));
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| bad(e.to_string()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| bad(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| bad(e.to_string()))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn pem_der(pem: &str) -> Result<Vec<u8>, Error> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    CertificateDer::from_pem_slice(pem.as_bytes())
        .map(|c| c.to_vec())
        .map_err(|e| Error::internal(format!("ca.pem: {e}")))
}

fn fingerprint(der: &[u8]) -> String {
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

/// Whether a peer may connect under `--local`: this machine, a private network (RFC 1918,
/// IPv6 unique local), link-local, or a tailnet (100.64.0.0/10).
///
/// 100.64.0.0/10 is the carrier-grade NAT range, which Tailscale uses for tailnets; on a mobile
/// carrier or a provider that NATs it is shared with other customers, not private to you. Under
/// `--local` on such a network, the tailnet's own ACLs are not what keeps strangers out: the
/// device token is (and the lockout).
pub fn local_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return local_peer(IpAddr::V4(v4));
            }
            let s = v.segments()[0];
            v.is_loopback() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
        }
    }
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
        let first = load_or_make(d.path(), &names, now()).unwrap();
        for k in ["ca-key.pem", "server-key.pem"] {
            let mode = std::fs::metadata(d.path().join(k)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{k}");
        }
        assert_eq!(expires(d.path()), Some(now() + Duration::days(SERVER_DAYS)));
        let server = std::fs::read_to_string(d.path().join("server.pem")).unwrap();
        // Again: nothing new.
        let again = load_or_make(d.path(), &names, now() + Duration::days(10)).unwrap();
        assert_eq!(again.ca_fingerprint, first.ca_fingerprint);
        assert_eq!(std::fs::read_to_string(d.path().join("server.pem")).unwrap(), server);
        // A new address: a new server certificate, the same CA (phones keep trusting it).
        let more = [names.clone(), vec!["192.168.1.20".into()]].concat();
        let moved = load_or_make(d.path(), &more, now() + Duration::days(11)).unwrap();
        assert_eq!(moved.ca_fingerprint, first.ca_fingerprint);
        assert!(moved.names.contains(&"192.168.1.20".to_owned()));
        assert_ne!(std::fs::read_to_string(d.path().join("server.pem")).unwrap(), server);
        // Ending soon (it was issued on day 11, so 25 days left): renewed, still the same CA.
        let late = now() + Duration::days(11 + SERVER_DAYS - RENEW_DAYS + 5);
        let renewed = load_or_make(d.path(), &more, late).unwrap();
        assert_eq!(renewed.ca_fingerprint, first.ca_fingerprint);
        assert_eq!(expires(d.path()), Some(late + Duration::days(SERVER_DAYS)));
        // The CA gone: a new one.
        std::fs::remove_file(d.path().join("ca-key.pem")).unwrap();
        let other = load_or_make(d.path(), &more, late).unwrap();
        assert_ne!(other.ca_fingerprint, first.ca_fingerprint);
    }

    #[test]
    fn local_peers_are_this_machine_private_networks_and_tailnets() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.20",
            "169.254.1.1",
            "100.101.1.2",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(local_peer(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.128.0.1", "2001:4860::8888", "::ffff:1.1.1.1"] {
            assert!(!local_peer(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn the_certificate_names_this_machine() {
        let n = names_for(&["workbox.tail1234.ts.net".into(), "0.0.0.0".into()]);
        assert!(n.contains(&"localhost".to_owned()) && n.contains(&"::1".to_owned()));
        assert!(n.contains(&"workbox.tail1234.ts.net".to_owned()));
        assert!(!n.contains(&"0.0.0.0".to_owned()));
    }
}
