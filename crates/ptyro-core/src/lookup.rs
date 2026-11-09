//! One way to find what a user names (SPEC §2.3), for every kind of thing: sessions, worktrees,
//! projects, workspaces, profiles, chats, runs, change sets, devices, archives and machines.
//!
//! The order: an exact name, then the qualified form where the kind has one (`project/name`),
//! then a full id, then a handle (the end of an id, at least [`MIN_SUFFIX`] characters, with
//! or without its `x:` prefix). Names and ids are lowercase, so what the user types is
//! lowercased before it is compared. Several matches are an `Ambiguous` error that lists them as
//! the user would address them; none is a `NotFound` error that names the kind.

use crate::{Error, ErrorCode};

/// The fewest characters of an id that address it.
pub const MIN_SUFFIX: usize = crate::ids::MIN_HANDLE;

/// How many candidates an error lists before it says how many more there are.
const LISTED: usize = 5;

/// What the user typed, ready to compare with names and ids: trimmed and lowercase.
pub fn key(addr: &str) -> String {
    addr.trim().to_lowercase()
}

/// Whether two names are the same name, whatever their case (device names, legacy names).
pub fn same(a: &str, b: &str) -> bool {
    key(a) == key(b)
}

/// Whether `key` (see [`key`]) addresses `id`: all of it, or its last [`MIN_SUFFIX`] or more
/// characters.
pub fn id_matches(id: &str, key: &str) -> bool {
    !key.is_empty() && (id == key || (key.len() >= MIN_SUFFIX && id.ends_with(key)))
}

/// The one thing in `found`, or `None` when there is nothing. Several are [`ambiguous`], each
/// listed by `label`.
pub fn one<T>(
    kind: &str,
    addr: &str,
    mut found: Vec<T>,
    label: impl Fn(&T) -> String,
) -> Result<Option<T>, Error> {
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(ambiguous(kind, addr, &found.iter().map(label).collect::<Vec<_>>())),
    }
}

/// Several things match `addr`: `3 runs match "ab12": say which, r-ab12cd, r-ab12ef or r-ab12gh`.
pub fn ambiguous(kind: &str, addr: &str, which: &[String]) -> Error {
    Error::new(
        ErrorCode::Ambiguous,
        format!("{} {kind}s match {:?}: say which, {}", which.len(), addr.trim(), list(which)),
    )
}

/// Several things share the name: `2 sessions are named "build": say which, app/build or
/// lib/build`.
pub fn ambiguous_name(kind: &str, name: &str, which: &[String]) -> Error {
    Error::new(
        ErrorCode::Ambiguous,
        format!("{} {kind}s are named {:?}: say which, {}", which.len(), name.trim(), list(which)),
    )
}

/// Nothing matches `addr`. When it is too short to be a handle, the message says so.
pub fn not_found(kind: &str, addr: &str) -> Error {
    let addr = addr.trim();
    let handle = addr.split_once(':').map_or(addr, |(_, h)| h);
    if !handle.is_empty()
        && handle.len() < MIN_SUFFIX
        && handle.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Error::new(
            ErrorCode::NotFound,
            format!(
                "no {kind} matches {addr:?} (the end of an id needs at least {MIN_SUFFIX} characters)"
            ),
        );
    }
    Error::missing(kind, addr)
}

/// Nothing is named `addr`, among a few that are: `no device matches "phone"; the devices are
/// laptop and tablet`.
pub fn not_found_among(kind: &str, addr: &str, known: &[String]) -> Error {
    let base = Error::missing(kind, addr.trim());
    if known.is_empty() {
        return Error::new(ErrorCode::NotFound, format!("{}; there are no {kind}s", base.message));
    }
    Error::new(ErrorCode::NotFound, format!("{}; the {kind}s are {}", base.message, and(known)))
}

/// `a, b or c`; past [`LISTED`]: `a, b, c, d, e and 3 more`.
fn list(which: &[String]) -> String {
    join(which, "or")
}

fn and(which: &[String]) -> String {
    join(which, "and")
}

fn join(which: &[String], word: &str) -> String {
    match which {
        [] => String::new(),
        [one] => one.clone(),
        _ if which.len() > LISTED => {
            format!("{} and {} more", which[..LISTED].join(", "), which.len() - LISTED)
        }
        [first @ .., last] => format!("{} {word} {last}", first.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn an_id_needs_four_characters_or_all_of_it() {
        let id = "01jb7f3kq9";
        assert!(id_matches(id, "01jb7f3kq9"));
        assert!(id_matches(id, "3kq9"));
        assert!(id_matches(id, "f3kq9"));
        assert!(!id_matches(id, "kq9"), "three characters are too few");
        assert!(!id_matches(id, ""));
        assert!(!id_matches(id, "7f3k"), "a suffix, never a substring");
        // A short id is matched whole.
        assert!(id_matches("ab", "ab"));
    }

    #[test]
    fn input_is_lowercased_and_trimmed() {
        assert_eq!(key("  Build "), "build");
        assert!(same("Neal's Phone", "neal's phone"));
        assert!(id_matches("01jb7f3kq9", &key("3KQ9")));
    }

    #[test]
    fn one_is_none_some_or_ambiguous() {
        assert_eq!(one("run", "x", Vec::<u8>::new(), |n| n.to_string()).unwrap(), None);
        assert_eq!(one("run", "x", vec![7u8], |n| n.to_string()).unwrap(), Some(7));
        let e = one("run", "ab12", vec![1u8, 2], |n| format!("r-{n}")).unwrap_err();
        assert_eq!(e.code, ErrorCode::Ambiguous);
        assert_eq!(e.message, r#"2 runs match "ab12": say which, r-1 or r-2"#);
    }

    #[test]
    fn ambiguity_lists_five_then_counts() {
        let e = ambiguous_name("session", "build", &s(&["app/build", "lib/build"]));
        assert_eq!(e.message, r#"2 sessions are named "build": say which, app/build or lib/build"#);
        let many = s(&["a", "b", "c", "d", "e", "f", "g"]);
        let e = ambiguous("chat", "fix", &many);
        assert_eq!(e.code, ErrorCode::Ambiguous);
        assert!(e.message.ends_with("say which, a, b, c, d, e and 2 more"), "{}", e.message);
    }

    #[test]
    fn not_found_names_the_kind_and_why_a_short_one_fails() {
        let e = not_found("run", "zzzz9");
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(e.message, r#"no run matches "zzzz9""#);
        let e = not_found("session", "ab");
        assert!(e.message.contains("at least 4 characters"), "{}", e.message);
        let e = not_found("session", "s:ab");
        assert!(e.message.contains("at least 4 characters"), "{}", e.message);
        assert!(!not_found("session", "@last").message.contains("characters"));
        let e = not_found_among("device", "phone", &s(&["laptop", "tablet"]));
        assert_eq!(e.message, r#"no device matches "phone"; the devices are laptop and tablet"#);
        let e = not_found_among("device", "phone", &[]);
        assert!(e.message.ends_with("there are no devices"), "{}", e.message);
    }
}
