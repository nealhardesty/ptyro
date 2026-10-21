//! Identifiers and short handles (SPEC §2.3).
//!
//! Every session has a ULID, stored and sent in full. Humans see the shortest unambiguous
//! lowercase suffix of it (minimum 4 characters). The random part of a ULID is at the end, so a
//! suffix is unique far sooner than a prefix would be.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

pub const MIN_HANDLE: usize = 4;

/// Serialized as its 26-character lowercase string, readable in JSON and stable in msgpack.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(Ulid);

impl Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.full())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl SessionId {
    pub fn new() -> Self {
        Self(Ulid::generate())
    }

    /// Lowercase Crockford base32, 26 characters.
    pub fn full(&self) -> String {
        self.0.to_string().to_ascii_lowercase()
    }

    pub fn created_ms(&self) -> u64 {
        self.0.timestamp_ms()
    }

    /// 128 random-ish bits, used to seed friendly names without a RNG dependency.
    pub fn seed(&self) -> u128 {
        self.0.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full())
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s:{}", self.full())
    }
}

impl FromStr for SessionId {
    type Err = ulid::DecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ulid::from_string(&s.to_ascii_uppercase()).map(Self)
    }
}

/// Shortest suffix of `id` (≥ [`MIN_HANDLE`]) that no other id in `all` shares.
pub fn short_handle(id: &SessionId, all: &[SessionId]) -> String {
    let full = id.full();
    let others: Vec<String> = all.iter().filter(|o| *o != id).map(SessionId::full).collect();
    for len in MIN_HANDLE..full.len() {
        let suffix = &full[full.len() - len..];
        if !others.iter().any(|o| o.ends_with(suffix)) {
            return suffix.to_owned();
        }
    }
    full
}

/// A connection to the daemon, unique for the daemon's lifetime.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct ClientId(pub u64);

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_unique_suffixes() {
        let ids: Vec<SessionId> = (0..200).map(|_| SessionId::new()).collect();
        let handles: Vec<String> = ids.iter().map(|i| short_handle(i, &ids)).collect();
        for (i, h) in handles.iter().enumerate() {
            assert!(h.len() >= MIN_HANDLE);
            assert!(ids[i].full().ends_with(h.as_str()));
            let matching = ids.iter().filter(|o| o.full().ends_with(h.as_str())).count();
            assert_eq!(matching, 1, "{h} is ambiguous");
        }
    }

    #[test]
    fn round_trips_as_lowercase_string() {
        let id = SessionId::new();
        let s = id.to_string();
        assert_eq!(s, s.to_ascii_lowercase());
        assert_eq!(s.parse::<SessionId>().unwrap(), id);
    }
}
