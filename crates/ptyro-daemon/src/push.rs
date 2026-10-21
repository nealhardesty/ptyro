//! Web Push to paired devices (SPEC §16.4, M9.4): the daemon is its own application server.
//! VAPID (RFC 8292) identifies it to the browser's push service with an ES256-signed JWT; each
//! message is encrypted for the browser (RFC 8291, `aes128gcm` from RFC 8188), so the push
//! service only ever relays ciphertext. Delivery goes through curl, like the other sinks.

use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use ptyro_core::secret::{b64url, random_bytes, unb64url};
use ptyro_core::{Error, now_ms};
use ptyro_protocol::PushSub;
use sha2::Sha256;

use crate::daemon::Daemon;

/// The daemon's VAPID key pair, kept in `state/push-vapid.key` (0600).
pub struct Vapid {
    secret: SecretKey,
}

impl Vapid {
    fn load_or_create(path: &std::path::Path) -> std::io::Result<Self> {
        if path.exists() {
            // Never replace a key we can't read: every subscription is bound to it.
            let text = std::fs::read_to_string(path)?;
            let secret = unb64url(text.trim())
                .and_then(|raw| SecretKey::from_slice(&raw).ok())
                .ok_or_else(|| {
                    std::io::Error::other(format!(
                        "{} isn't a key ptyro can read; move it aside to make a new one (paired devices then turn notifications on again)",
                        path.display()
                    ))
                })?;
            return Ok(Self { secret });
        }
        let secret = new_secret()?;
        write_private(path, b64url(&secret.to_bytes()).as_bytes())?;
        Ok(Self { secret })
    }

    /// The uncompressed public point, base64url: `applicationServerKey` in the browser.
    pub fn public_b64(&self) -> String {
        b64url(self.secret.public_key().to_encoded_point(false).as_bytes())
    }

    /// `Authorization: vapid t=…, k=…` for one endpoint.
    pub fn authorization(&self, endpoint: &str, subject: &str) -> Result<String, String> {
        let aud = origin(endpoint).ok_or("the endpoint has no origin")?;
        let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims =
            serde_json::json!({ "aud": aud, "exp": now_ms() / 1000 + 12 * 3600, "sub": subject });
        let claims = b64url(claims.to_string().as_bytes());
        let input = format!("{header}.{claims}");
        let sig: Signature = SigningKey::from(&self.secret).sign(input.as_bytes());
        Ok(format!("vapid t={input}.{}, k={}", b64url(&sig.to_bytes()), self.public_b64()))
    }
}

fn new_secret() -> std::io::Result<SecretKey> {
    // A random 32-byte string is a valid scalar with overwhelming probability; retry otherwise.
    loop {
        if let Ok(k) = SecretKey::from_slice(&random_bytes(32)?) {
            return Ok(k);
        }
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

/// `https://fcm.googleapis.com/fcm/send/x` → `https://fcm.googleapis.com`.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

/// Encrypt `plaintext` for one subscription (RFC 8291 §3.4), with the given ephemeral key and
/// salt (random in use, fixed in the test vector). The result is the whole request body.
pub fn encrypt(
    plaintext: &[u8],
    ua_public: &[u8],
    auth_secret: &[u8],
    as_secret: &SecretKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, String> {
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| "bad p256dh key")?;
    let as_public = as_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua.as_affine());
    // IKM = HKDF(auth_secret, ecdh_secret, "WebPush: info" || 0 || ua_public || as_public)
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth_secret), shared.raw_secret_bytes())
        .expand(&info, &mut ikm)
        .map_err(|_| "hkdf")?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| "hkdf")?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| "hkdf")?;
    // One record: the payload, then the last-record delimiter (no padding).
    let mut record = plaintext.to_vec();
    record.push(2);
    let sealed = Aes128Gcm::new_from_slice(&cek)
        .map_err(|_| "aes key")?
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .map_err(|_| "aes-gcm")?;
    // Header: salt, record size, key id length, key id (our public key).
    let mut body = salt.to_vec();
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// What a notification says; the service worker shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Payload {
    pub title: String,
    pub body: String,
    /// The session to open on click.
    pub session: Option<String>,
    /// `blocked`, `errored`, `done`, `idle`, `gate`, `test`…
    pub kind: String,
    /// Notifications with the same tag replace each other.
    pub tag: String,
    /// The prompt a blocked notification is about (its status's `since_ms`): an answer from
    /// the notification names it, so a stale one never approves a newer prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
}

impl Payload {
    /// Keep the encrypted record under push services' ~4 KB limit.
    fn bounded(mut self) -> Self {
        let cut = |s: &mut String, n: usize| {
            if s.chars().count() > n {
                *s = s.chars().take(n).collect::<String>() + "…";
            }
        };
        cut(&mut self.title, 120);
        cut(&mut self.body, 1500);
        self
    }
}

impl Daemon {
    pub(crate) fn vapid(&self) -> Result<Arc<Vapid>, Error> {
        let mut v = self.vapid.lock().expect("vapid");
        if let Some(k) = v.as_ref() {
            return Ok(Arc::clone(k));
        }
        let k = Arc::new(
            Vapid::load_or_create(&self.paths.state.join("push-vapid.key"))
                .map_err(|e| Error::internal(format!("push key: {e}")))?,
        );
        *v = Some(Arc::clone(&k));
        Ok(k)
    }

    /// Push to every subscription (of one device, or all). Subscriptions the push service says
    /// are gone (404, 410) are dropped. Returns one line per attempt.
    pub(crate) async fn push_all(&self, device: Option<u64>, p: Payload) -> Vec<String> {
        let subs: Vec<(String, u64, PushSub)> = self
            .devices
            .lock()
            .expect("devices")
            .iter()
            .filter(|d| device.is_none_or(|id| u64::from_str_radix(&d.id, 16).ok() == Some(id)))
            .flat_map(|d| {
                let id = u64::from_str_radix(&d.id, 16).unwrap_or(0);
                d.push.iter().map(move |s| (d.name.clone(), id, s.clone()))
            })
            .collect();
        if subs.is_empty() {
            return Vec::new();
        }
        let Ok(vapid) = self.vapid() else { return vec!["push: no key".into()] };
        let body = serde_json::to_vec(&p.clone().bounded()).unwrap_or_default();
        let urgent = matches!(p.kind.as_str(), "blocked" | "errored" | "gate");
        let state = self.paths.state.clone();
        let mut out = Vec::new();
        let mut gone = Vec::new();
        for (name, _, sub) in subs {
            let (vapid, body, state) = (Arc::clone(&vapid), body.clone(), state.clone());
            let endpoint = sub.endpoint.clone();
            let r = tokio::task::spawn_blocking(move || send(&vapid, &sub, &body, urgent, &state))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            match r {
                Ok(m) => out.push(format!("push {name}: {m}")),
                Err(e) => {
                    if e.starts_with("HTTP 404") || e.starts_with("HTTP 410") {
                        gone.push(endpoint);
                    }
                    out.push(format!("push {name}: failed: {e}"));
                }
            }
        }
        if !gone.is_empty() {
            for d in self.devices.lock().expect("devices").iter_mut() {
                d.push.retain(|s| !gone.contains(&s.endpoint));
            }
            self.save_devices();
        }
        out
    }
}

fn send(
    vapid: &Vapid,
    sub: &PushSub,
    payload: &[u8],
    urgent: bool,
    state: &std::path::Path,
) -> Result<String, String> {
    let ua = unb64url(&sub.p256dh).ok_or("bad p256dh")?;
    let auth = unb64url(&sub.auth).ok_or("bad auth")?;
    let salt: [u8; 16] =
        random_bytes(16).map_err(|e| e.to_string())?.try_into().map_err(|_| "salt")?;
    let eph = new_secret().map_err(|e| e.to_string())?;
    let body = encrypt(payload, &ua, &auth, &eph, &salt)?;
    let auth_header = vapid.authorization(&sub.endpoint, "mailto:ptyro@localhost")?;
    let file =
        state.join(format!("push-{}.bin", b64url(&random_bytes(6).map_err(|e| e.to_string())?)));
    write_private(&file, &body).map_err(|e| e.to_string())?;
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let r = crate::notifier::curl(&[
        format!("url = {}", q(&sub.endpoint)),
        "request = \"POST\"".into(),
        format!("header = {}", q("TTL: 86400")),
        format!("header = {}", q(if urgent { "Urgency: high" } else { "Urgency: normal" })),
        format!("header = {}", q("Content-Encoding: aes128gcm")),
        format!("header = {}", q("Content-Type: application/octet-stream")),
        format!("header = {}", q(&format!("Authorization: {auth_header}"))),
        format!("data-binary = {}", q(&format!("@{}", file.display()))),
    ]);
    let _ = std::fs::remove_file(&file);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8291 §5 (and Appendix A): the published example, byte for byte.
    #[test]
    fn rfc8291_example() {
        let as_secret = SecretKey::from_slice(
            &unb64url("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw").unwrap(),
        )
        .unwrap();
        assert_eq!(
            b64url(as_secret.public_key().to_encoded_point(false).as_bytes()),
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        let ua_public = unb64url("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4").unwrap();
        let auth = unb64url("BTBZMqHH6r4Tts7J_aSIgg").unwrap();
        let salt: [u8; 16] = unb64url("DGv6ra1nlYgDCS1FRnbzlw").unwrap().try_into().unwrap();
        let body = encrypt(
            b"When I grow up, I want to be a watermelon",
            &ua_public,
            &auth,
            &as_secret,
            &salt,
        )
        .unwrap();
        assert_eq!(
            b64url(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn vapid_jwt_verifies() {
        use p256::ecdsa::VerifyingKey;
        use p256::ecdsa::signature::Verifier;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        let v = Vapid::load_or_create(&path).unwrap();
        assert_eq!(Vapid::load_or_create(&path).unwrap().public_b64(), v.public_b64(), "persisted");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let h = v.authorization("https://push.example.net/wpush/abc", "mailto:a@b").unwrap();
        let t = h.strip_prefix("vapid t=").unwrap().split(", k=").next().unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        let claims: serde_json::Value =
            serde_json::from_slice(&unb64url(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.net");
        let sig = Signature::from_slice(&unb64url(parts[2]).unwrap()).unwrap();
        let key = VerifyingKey::from_sec1_bytes(&unb64url(&v.public_b64()).unwrap()).unwrap();
        key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
    }
}
