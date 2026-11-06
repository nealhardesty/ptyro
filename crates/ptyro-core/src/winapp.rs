//! What makes ptyro an app Windows knows (WIN10): its AppUserModelID, which toasts show as
//! "ptyro" with its icon, and its `ptyro:` links, which a toast's click follows to its session.
//!
//! A link is `ptyro://attach/<session>`, with `?remote=<remote>` when the session is another
//! daemon's (`wsl://Ubuntu`, `local`). Windows hands it to `ptyrow.exe open-url`, and so does a
//! browser when a page links to one (it asks first), so [`parse_link`] lets through only a
//! session address and a remote of the forms ptyro itself writes: never anything a command line
//! could take for something else. All it can do is show a session.

/// ptyro's AppUserModelID.
pub const AUMID: &str = "dev.ptyro.app";

/// The URL scheme of ptyro's links.
pub const SCHEME: &str = "ptyro";

/// Where Windows keeps an unpackaged app's AppUserModelID (`HKCU\…`): its name and icon.
pub fn aumid_key() -> String {
    format!("Software\\Classes\\AppUserModelId\\{AUMID}")
}

/// Where Windows looks up what opens `ptyro:` links (`HKCU\…`).
pub fn scheme_key() -> String {
    format!("Software\\Classes\\{SCHEME}")
}

/// What a toast's click opens: `session` (an id or a name), on `remote`'s daemon when given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub session: String,
    pub remote: Option<String>,
}

/// A session address: an id, a handle or a name (letters, digits, `.`, `_`, `-`).
fn is_address(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A remote ptyro writes into a link: `local`, or `wsl://` with a distro name (WSL's own rule:
/// letters, digits, `.`, `_`, `-`).
fn is_remote(s: &str) -> bool {
    s == "local"
        || s.strip_prefix("wsl://").is_some_and(|d| {
            d.len() <= 64
                && !d.starts_with('-')
                && d.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
}

/// The link to `session` (on `remote`'s daemon). `None` when either isn't of the forms
/// [`parse_link`] takes, so ptyro never writes a link it would refuse.
pub fn attach_link(session: &str, remote: Option<&str>) -> Option<String> {
    if !is_address(session) || remote.is_some_and(|r| !is_remote(r)) {
        return None;
    }
    Some(match remote {
        // `:` and `/` are fine in a query; nothing else in a remote needs escaping.
        Some(r) => format!("{SCHEME}://attach/{session}?remote={}", r.replace('/', "%2F")),
        None => format!("{SCHEME}://attach/{session}"),
    })
}

/// A `ptyro:` link, checked: anything but `ptyro://attach/<address>[?remote=<remote>]` is refused.
pub fn parse_link(uri: &str) -> Result<Link, String> {
    let bad = || format!("not a link ptyro opens: {uri:?}");
    let rest = uri
        .strip_prefix(&format!("{SCHEME}://attach/"))
        .or_else(|| uri.strip_prefix(&format!("{SCHEME}:attach/")))
        .ok_or_else(bad)?;
    let (session, query) = match rest.split_once('?') {
        Some((s, q)) => (s.trim_end_matches('/'), Some(q)),
        None => (rest.trim_end_matches('/'), None),
    };
    if !is_address(session) {
        return Err(bad());
    }
    let mut remote = None;
    for pair in query.into_iter().flat_map(|q| q.split('&')).filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("remote", v)) => {
                let v = v
                    .replace("%2F", "/")
                    .replace("%2f", "/")
                    .replace("%3A", ":")
                    .replace("%3a", ":");
                if !is_remote(&v) {
                    return Err(bad());
                }
                remote = Some(v);
            }
            _ => return Err(bad()),
        }
    }
    Ok(Link { session: session.to_owned(), remote })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip_and_anything_else_is_refused() {
        let l = attach_link("01m6mxc13w7wz153srvrxtcsg5", Some("wsl://Ubuntu-24.04")).unwrap();
        assert_eq!(l, "ptyro://attach/01m6mxc13w7wz153srvrxtcsg5?remote=wsl:%2F%2FUbuntu-24.04");
        assert_eq!(
            parse_link(&l).unwrap(),
            Link {
                session: "01m6mxc13w7wz153srvrxtcsg5".into(),
                remote: Some("wsl://Ubuntu-24.04".into())
            }
        );
        assert_eq!(
            parse_link("ptyro://attach/c1/").unwrap(),
            Link { session: "c1".into(), remote: None }
        );
        assert_eq!(
            parse_link("ptyro://attach/c1?remote=local").unwrap().remote.as_deref(),
            Some("local")
        );
        // Only what ptyro writes: never an option, a path, a command or another remote.
        for bad in [
            "ptyro://attach/--help",
            "ptyro://attach/a b",
            "ptyro://attach/a&calc",
            "ptyro://attach/..%2F..",
            "ptyro://attach/",
            "ptyro://run/calc",
            "ptyro://attach/c1?remote=ssh://evil",
            "ptyro://attach/c1?remote=wsl://-d",
            "ptyro://attach/c1?remote=wsl://a;calc",
            "ptyro://attach/c1?cmd=calc",
            "https://attach/c1",
        ] {
            assert!(parse_link(bad).is_err(), "{bad}");
        }
        assert_eq!(attach_link("--help", None), None, "never written either");
        assert_eq!(attach_link("c1", Some("ssh://host")), None);
    }
}
