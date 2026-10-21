//! Credentials (M9): bytes from the OS's random source, failing rather than guessing, and the
//! URL-safe base64 that tokens, Web Push keys and JWTs use.

use std::io::Read;

/// `n` bytes from the operating system's CSPRNG. There is no fallback: a credential must never
/// come from anything weaker.
pub fn random_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

/// A new secret token: `prefix` + 32 random bytes in base64url (256 bits).
pub fn token(prefix: &str) -> std::io::Result<String> {
    Ok(format!("{prefix}{}", b64url(&random_bytes(32)?)))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding (RFC 4648 §5).
pub fn b64url(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            s.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    s
}

/// Decode base64url, padded or not; also accepts the standard alphabet (`+/`), which browsers
/// hand out in some push subscriptions.
pub fn unb64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Equality that takes the same time wherever the inputs differ (token checks).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_rfc4648_vectors() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64url(raw.as_bytes()), enc);
            assert_eq!(unb64url(enc).unwrap(), raw.as_bytes());
        }
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(unb64url("+/8=").unwrap(), [0xfb, 0xff]);
        assert!(unb64url("a b").is_none());
    }

    #[test]
    fn tokens_are_long_and_distinct() {
        let (a, b) = (token("ptd_").unwrap(), token("ptd_").unwrap());
        assert_ne!(a, b);
        assert_eq!(unb64url(&a[4..]).unwrap().len(), 32);
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"ab", b"abc"));
    }
}
