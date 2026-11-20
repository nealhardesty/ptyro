//! The one way in (SPEC §25.5). Everything that reaches `kb.db` is a [`Plan`], and the only
//! way to make one is [`plan`], which needs proof that [`gate`] let the source through. So no
//! new source can skip the gate, redaction or the stamp:
//!
//! 1. [`gate`]: is the knowledge base on, and this source, for this agent and project? Is the
//!    session unrecorded or private, the URI forgotten, the file on the never list or too big?
//!    It is asked *before* the source is read.
//! 2. The caller reads the source, from its cursor or whole, into a [`Source`].
//! 3. [`plan`] redacts (§25.7) before anything is cut, hashed or kept; cuts by the body's kind
//!    ([`super::chunk`]); stamps the result with what the *daemon* knows ([`Stamp`]: the
//!    writer and the profile are never a caller's claim); and hashes it.
//! 4. `KbDb::write` stores the plan in one transaction.
//!
//! Nothing here reads a file or a clock, so the same source always gives the same plan.

use std::path::Path;

use regex::Regex;
use sha2::{Digest, Sha256};

use super::chunk::{self, Part};
use super::redact::{MAX_HITS, Redactor};
use super::{ItemKind, Writer};

/// The largest file read (`kb.max_file`'s default, §25.13).
pub const MAX_FILE: u64 = 1 << 20;

/// Why a source was not ingested. `Display` is one line for `ptyro kb status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// `[kb] enabled = false`.
    Off,
    /// `[kb] sessions = false`: agent sessions are not kept.
    SessionsOff,
    /// `[kb] shell = "off"`: nothing of a shell is kept.
    ShellsOff,
    /// `[agents.<name>] kb = false`.
    AgentOff,
    /// `[knowledge] enabled = false` in the project's `.ptyro.toml`.
    ProjectOff,
    /// A session whose screen is not recorded (a login, §7.6).
    Unrecorded,
    /// A private session (§25.7a).
    Private,
    /// The URI was forgotten: its tombstone keeps it out (§25.12).
    Forgotten,
    /// The path is on the never list (§25.7).
    Never,
    TooBig {
        bytes: u64,
        max: u64,
    },
    /// More than [`MAX_HITS`] redactions: probably a keystore or a fixture of credentials.
    Secrets {
        hits: usize,
    },
}

impl std::fmt::Display for Skip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Skip::Off => f.write_str("the knowledge base is off"),
            Skip::SessionsOff => f.write_str("agent sessions are not kept (`kb.sessions` is off)"),
            Skip::ShellsOff => f.write_str("shell commands are not kept (`kb.shell` is off)"),
            Skip::AgentOff => f.write_str("its agent is kept out of the knowledge base"),
            Skip::ProjectOff => f.write_str("its project is kept out of the knowledge base"),
            Skip::Unrecorded => f.write_str("the session is not recorded"),
            Skip::Private => f.write_str("the session is private"),
            Skip::Forgotten => f.write_str("it was forgotten"),
            Skip::Never => f.write_str("its path is one that is never read (keys and credentials)"),
            Skip::TooBig { bytes, max } => {
                write!(f, "it is too big ({bytes} bytes; at most {max} are read)")
            }
            Skip::Secrets { hits } => {
                write!(
                    f,
                    "it holds {hits} things that look like credentials (more than {MAX_HITS}): none of it is kept"
                )
            }
        }
    }
}

/// Paths that are never read, whatever includes them (§25.7): the built-in list, and
/// `kb.exclude`'s globs added to it.
///
/// A pattern with no `/` is tried on every part of a path, so it names a file (`*.pem`) or a
/// folder with everything in it (`.ssh`). One with a `/` is tried on the path's end
/// (`config/secrets/*.yml`) and on each folder above it. `*` is anything but a `/`, `**`
/// anything, `?` one character. Case doesn't matter.
#[derive(Debug, Clone)]
pub struct Never {
    part: Option<Regex>,
    path: Option<Regex>,
}

/// §25.7's list.
pub const NEVER: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.kdbx",
    "id_rsa*",
    "id_ed25519*",
    ".npmrc",
    ".netrc",
    ".pgpass",
    "credentials*",
    "secrets.*",
    ".ssh",
    ".aws",
    ".gnupg",
    ".git",
];

impl Default for Never {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl Never {
    /// The built-in list and `more` (`kb.exclude`).
    pub fn new(more: &[String]) -> Self {
        Self::build(NEVER.iter().copied().chain(more.iter().map(String::as_str)))
    }

    /// These globs alone, without the built-in list: a root's `include` and `exclude`
    /// ([`Never::is_never`] then says whether a path matches one). None: nothing matches.
    pub fn only(globs: &[String]) -> Self {
        Self::build(globs.iter().map(String::as_str))
    }

    fn build<'a>(all: impl Iterator<Item = &'a str>) -> Self {
        let all = all.map(str::trim).filter(|p| !p.is_empty());
        let (mut parts, mut paths) = (Vec::new(), Vec::new());
        for p in all {
            let p = p.replace('\\', "/");
            let p = p.trim_matches('/');
            if p.contains('/') { &mut paths } else { &mut parts }.push(glob(p));
        }
        let build = |v: Vec<String>, wrap: fn(&str) -> String| {
            (!v.is_empty()).then(|| Regex::new(&wrap(&v.join("|"))).ok()).flatten()
        };
        Self {
            part: build(parts, |alt| format!("(?i)^(?:{alt})$")),
            path: build(paths, |alt| format!("(?i)(?:^|/)(?:{alt})(?:/|$)")),
        }
    }

    /// Whether `path` (absolute, or inside a root) is never to be read.
    pub fn is_never(&self, path: &Path) -> bool {
        let path = path.to_string_lossy().replace('\\', "/");
        self.part.as_ref().is_some_and(|re| path.split('/').any(|part| re.is_match(part)))
            || self.path.as_ref().is_some_and(|re| re.is_match(&path))
    }
}

/// A glob as a regex (unanchored): everything is itself but `*`, `**` and `?`.
fn glob(p: &str) -> String {
    let mut out = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                out.push_str(".*");
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            c => out.push_str(&regex::escape(c.encode_utf8(&mut [0; 4]))),
        }
    }
    out
}

/// What the gate asks, all of it in one place. The daemon fills it from what *it* knows.
#[derive(Debug, Clone)]
pub struct Gate<'a> {
    /// `kb.enabled`.
    pub enabled: bool,
    /// The source is an agent's session and `kb.sessions` is off.
    pub sessions_off: bool,
    /// The source is a shell's command and `kb.shell` is `off`.
    pub shells_off: bool,
    /// The source's agent has `kb = false`.
    pub agent_off: bool,
    /// The source's project has `[kb] enabled = false`.
    pub project_off: bool,
    /// The session it comes from is not recorded (§7.6).
    pub unrecorded: bool,
    /// The session it comes from is private (§25.7a).
    pub private: bool,
    /// The URI has a tombstone (`KbDb::tombstoned`).
    pub forgotten: bool,
    /// The file it is read from, when it is one.
    pub path: Option<&'a Path>,
    pub never: &'a Never,
    /// The file's size, when it is a file with a limit (a transcript has none).
    pub bytes: Option<u64>,
    /// `kb.max_file`.
    pub max_bytes: u64,
}

impl<'a> Gate<'a> {
    /// A gate that lets everything through but the never list: set what applies.
    pub fn open(never: &'a Never) -> Self {
        Self {
            enabled: true,
            sessions_off: false,
            shells_off: false,
            agent_off: false,
            project_off: false,
            unrecorded: false,
            private: false,
            forgotten: false,
            path: None,
            never,
            bytes: None,
            max_bytes: MAX_FILE,
        }
    }
}

/// Proof that [`gate`] let a source through: [`plan`] takes nothing else.
#[derive(Debug)]
pub struct Passed(());

/// The gate (§25.5 step 1): asked before a source is read.
pub fn gate(g: &Gate<'_>) -> Result<Passed, Skip> {
    if !g.enabled {
        return Err(Skip::Off);
    }
    if g.sessions_off {
        return Err(Skip::SessionsOff);
    }
    if g.shells_off {
        return Err(Skip::ShellsOff);
    }
    if g.agent_off {
        return Err(Skip::AgentOff);
    }
    if g.project_off {
        return Err(Skip::ProjectOff);
    }
    if g.unrecorded {
        return Err(Skip::Unrecorded);
    }
    if g.private {
        return Err(Skip::Private);
    }
    if g.forgotten {
        return Err(Skip::Forgotten);
    }
    if g.path.is_some_and(|p| g.never.is_never(p)) {
        return Err(Skip::Never);
    }
    match g.bytes {
        Some(bytes) if bytes > g.max_bytes => Err(Skip::TooBig { bytes, max: g.max_bytes }),
        _ => Ok(Passed(())),
    }
}

/// What a source holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// A Markdown document: cut at its headings ([`chunk::markdown`]). `name` starts every
    /// heading path (the file's name, a note's title): redacted as the text is.
    Markdown { name: String, text: String, at_ms: u64 },
    /// A document with no headings ([`chunk::plain`]).
    Plain { name: String, text: String, at_ms: u64 },
    /// Pieces the source already has: a transcript's turns, a file's comments, a commit's
    /// message and paths, a shell's commands. Each is cut further only if it is too long
    /// ([`chunk::message`]).
    Parts(Vec<Part>),
    /// Several of those, one after another: a file's path, then its text or its comments.
    Seq(Vec<Body>),
}

/// One thing read, as its reader hands it over: what it *is*, never who says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub kind: ItemKind,
    /// The item's identity (§25.3).
    pub uri: String,
    pub title: Option<String>,
    /// Its path inside its root.
    pub rel: Option<String>,
    pub body: Body,
    /// `false`: the body is all the item holds, and what the item had that the body lacks
    /// goes (a file read again). `true`: the body is what was read since the cursor (a
    /// transcript): its parts follow the item's, and a part whose anchor the item already has
    /// stands in for that one, so reading a piece twice doubles nothing.
    pub append: bool,
    /// A session's touched files, a commit's changed files.
    pub files: Vec<String>,
    /// Where reading stopped, kept with the item for the next read.
    pub cursor: Option<serde_json::Value>,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
}

impl Source {
    pub fn new(kind: ItemKind, uri: impl Into<String>, body: Body) -> Self {
        Self {
            kind,
            uri: uri.into(),
            title: None,
            rel: None,
            body,
            append: false,
            files: Vec::new(),
            cursor: None,
            started_ms: None,
            ended_ms: None,
        }
    }
}

/// Where a source came from and whose it is (§25.5 step 5), as the daemon knows it: from the
/// hook's session, the tool caller's session, a client's device, a root's row. Never from the
/// source's own text or a caller's argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub writer: Writer,
    /// `None`: every profile may see it.
    pub profile: Option<String>,
    pub root: Option<String>,
    pub project: Option<String>,
    pub workspace: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// ptyro's session id.
    pub session: Option<String>,
}

impl Stamp {
    pub fn new(writer: Writer, profile: Option<String>) -> Self {
        Self {
            writer,
            profile,
            root: None,
            project: None,
            workspace: None,
            worktree: None,
            branch: None,
            agent: None,
            model: None,
            session: None,
        }
    }
}

/// What `KbDb::write` stores: an item and its chunks, redacted, cut and stamped. Only [`plan`]
/// makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plan {
    pub kind: ItemKind,
    pub uri: String,
    pub title: Option<String>,
    pub rel: Option<String>,
    pub stamp: Stamp,
    pub files: Vec<String>,
    pub cursor: Option<serde_json::Value>,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
    /// [`Source::append`].
    pub append: bool,
    pub chunks: Vec<Part>,
    /// SHA-256 of what would be stored (the title, the path, the files and every chunk), for a
    /// source that is all of its item: the same hash again means nothing is written. `None`
    /// for an appended piece, which is told apart by its anchors.
    pub hash: Option<String>,
    /// How many secrets were replaced.
    pub hits: usize,
    /// Parts of an appended piece left out for holding more than [`MAX_HITS`] secrets each.
    pub dropped: usize,
}

/// §25.5 steps 3 to 5: redact, cut, stamp, hash.
///
/// The 25-hit rule (§25.7): a source that is all of its item (a file, a note, a commit) with
/// more than [`MAX_HITS`] redactions is refused whole ([`Skip::Secrets`]). In an appended piece
/// (a transcript's new turns) the rule is each part's: a turn that is a pasted keystore is
/// left out and counted in [`Plan::dropped`], and the conversation around it is kept.
pub fn plan(_: Passed, source: Source, stamp: Stamp, redactor: &Redactor) -> Result<Plan, Skip> {
    let Source { kind, uri, title, rel, body, append, files, cursor, started_ms, ended_ms } =
        source;
    let (mut hits, mut dropped) = (0, 0);
    let title = title.map(|t| {
        let r = redactor.redact(&t);
        hits += r.hits;
        r.text
    });
    let mut chunks = Vec::new();
    cut(body, append, redactor, &mut chunks, &mut hits, &mut dropped);
    if !append && hits > MAX_HITS {
        return Err(Skip::Secrets { hits });
    }
    let hash = (!append).then(|| hash(title.as_deref(), rel.as_deref(), &files, &chunks));
    Ok(Plan {
        kind,
        uri,
        title,
        rel,
        stamp,
        files,
        cursor,
        started_ms,
        ended_ms,
        append,
        chunks,
        hash,
        hits,
        dropped,
    })
}

/// A body redacted and cut into `out`.
fn cut(
    body: Body,
    append: bool,
    redactor: &Redactor,
    out: &mut Vec<Part>,
    hits: &mut usize,
    dropped: &mut usize,
) {
    match body {
        Body::Markdown { name, text, at_ms } => {
            let (name, r) = (redactor.redact(&name), redactor.redact(&text));
            *hits += name.hits + r.hits;
            out.extend(chunk::markdown(&name.text, &r.text, at_ms));
        }
        Body::Plain { name, text, at_ms } => {
            let (name, r) = (redactor.redact(&name), redactor.redact(&text));
            *hits += name.hits + r.hits;
            out.extend(chunk::plain(&name.text, &r.text, at_ms));
        }
        Body::Parts(parts) => {
            for mut part in parts {
                let text = redactor.redact(&part.text);
                let heading = part.heading.as_deref().map(|h| redactor.redact(h));
                let n = text.hits + heading.as_ref().map_or(0, |h| h.hits);
                if append && n > MAX_HITS {
                    *dropped += 1;
                    continue;
                }
                *hits += n;
                part.text = text.text;
                part.heading = heading.map(|h| h.text);
                out.extend(chunk::message(part));
            }
        }
        Body::Seq(bodies) => {
            for b in bodies {
                cut(b, append, redactor, out, hits, dropped);
            }
        }
    }
}

fn hash(title: Option<&str>, rel: Option<&str>, files: &[String], chunks: &[Part]) -> String {
    let mut h = Sha256::new();
    // Each field ends in a byte no text holds, so no two lists of fields hash alike.
    let mut field = |s: &str| {
        h.update(s.as_bytes());
        h.update([0xff]);
    };
    field(title.unwrap_or(""));
    field(rel.unwrap_or(""));
    for f in files {
        field(f);
    }
    field("");
    for c in chunks {
        field(c.kind.as_str());
        field(c.anchor.as_deref().unwrap_or(""));
        field(c.heading.as_deref().unwrap_or(""));
        field(&c.text);
        field(&c.meta.as_ref().map(|m| m.to_string()).unwrap_or_default());
    }
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::ChunkKind;

    fn red() -> Redactor {
        Redactor::new(vec![("hunter2-value".into(), "<secret:DB>".into())], &[]).0
    }

    fn pass() -> Passed {
        gate(&Gate::open(&Never::default())).unwrap()
    }

    fn stamp() -> Stamp {
        Stamp { agent: Some("claude".into()), ..Stamp::new(Writer::Agent, Some("work".into())) }
    }

    #[test]
    fn the_gate_closes_for_each_reason_in_order() {
        let never = Never::default();
        let open = Gate::open(&never);
        assert!(gate(&open).is_ok());
        let all = Gate {
            enabled: false,
            sessions_off: true,
            shells_off: true,
            agent_off: true,
            project_off: true,
            unrecorded: true,
            private: true,
            forgotten: true,
            path: Some(Path::new("/repo/.env")),
            bytes: Some(MAX_FILE + 1),
            ..open.clone()
        };
        let mut g = all;
        let mut seen = Vec::new();
        loop {
            match gate(&g) {
                Ok(_) => break,
                Err(why) => {
                    assert!(!why.to_string().is_empty());
                    match why {
                        Skip::Off => g.enabled = true,
                        Skip::SessionsOff => g.sessions_off = false,
                        Skip::ShellsOff => g.shells_off = false,
                        Skip::AgentOff => g.agent_off = false,
                        Skip::ProjectOff => g.project_off = false,
                        Skip::Unrecorded => g.unrecorded = false,
                        Skip::Private => g.private = false,
                        Skip::Forgotten => g.forgotten = false,
                        Skip::Never => g.path = Some(Path::new("/repo/README.md")),
                        Skip::TooBig { .. } => g.bytes = Some(MAX_FILE),
                        Skip::Secrets { .. } => unreachable!(),
                    }
                    seen.push(why);
                }
            }
        }
        assert_eq!(
            seen,
            [
                Skip::Off,
                Skip::SessionsOff,
                Skip::ShellsOff,
                Skip::AgentOff,
                Skip::ProjectOff,
                Skip::Unrecorded,
                Skip::Private,
                Skip::Forgotten,
                Skip::Never,
                Skip::TooBig { bytes: MAX_FILE + 1, max: MAX_FILE },
            ]
        );
    }

    /// §25.7's never list, and `kb.exclude` adding to it.
    #[test]
    fn the_never_list() {
        let never = Never::default();
        for p in [
            "/repo/.env",
            "/repo/.env.local",
            "/repo/app/.ENV.production",
            "/repo/certs/server.pem",
            "/repo/server.key",
            "/repo/a.p12",
            "/repo/a.pfx",
            "/home/me/passwords.kdbx",
            "/home/me/.ssh/config",
            "/home/me/.ssh/id_ed25519.pub",
            "/backup/id_rsa",
            "/backup/id_rsa.old",
            "/repo/.npmrc",
            "/home/me/.netrc",
            "/home/me/.pgpass",
            "/home/me/.aws/config",
            "/repo/credentials.json",
            "/repo/config/secrets.yml",
            "/home/me/.gnupg/pubring.kbx",
            "/repo/.git/config",
            "C:\\Users\\me\\.ssh\\known_hosts",
            ".env",
        ] {
            assert!(never.is_never(Path::new(p)), "{p}");
        }
        for p in [
            "/repo/README.md",
            "/repo/src/env.rs",
            "/repo/docs/environment.md",
            "/repo/src/keyboard.rs",
            "/repo/src/key.rs",
            "/repo/.github/workflows/ci.yml",
            "/repo/.gitignore",
            "/repo/src/secrets_manager.rs",
            "/repo/my_credentials_test.rs",
            "/repo/pemmican.txt",
        ] {
            assert!(!never.is_never(Path::new(p)), "{p}");
        }
        let more = Never::new(&[
            "*.sqlite".into(),
            "private".into(),
            "fixtures/keys/**".into(),
            "notes/*.draft.md".into(),
            " ".into(),
        ]);
        for p in [
            "/repo/data.SQLITE",
            "/repo/private/plan.md",
            "/repo/tests/fixtures/keys/a/b.txt",
            "/repo/notes/x.draft.md",
            "/repo/.env",
        ] {
            assert!(more.is_never(Path::new(p)), "{p}");
        }
        for p in ["/repo/privately.md", "/repo/fixtures/other/a.txt", "/repo/notes/sub/x.draft.md"]
        {
            assert!(!more.is_never(Path::new(p)), "{p}");
        }
        let closed = Gate { path: Some(Path::new("/repo/.env")), ..Gate::open(&never) };
        assert_eq!(gate(&closed).unwrap_err(), Skip::Never);
    }

    #[test]
    fn a_document_is_redacted_before_it_is_cut_and_stamped_by_the_daemon() {
        let text = "# Deploy\n\nThe db password is hunter2-value.\n\n## Keys\n\nexport API_TOKEN=abc123def456\n";
        let mut src = Source::new(
            ItemKind::File,
            "file:///repo/docs/deploy.md",
            Body::Markdown { name: "deploy.md".into(), text: text.into(), at_ms: 7 },
        );
        src.title = Some("Deploy with hunter2-value".into());
        src.rel = Some("docs/deploy.md".into());
        let p = plan(pass(), src.clone(), stamp(), &red()).unwrap();
        assert_eq!(p.title.as_deref(), Some("Deploy with <secret:DB>"));
        assert_eq!(p.hits, 3);
        assert_eq!(p.chunks.len(), 2);
        assert_eq!(p.chunks[0].text, "The db password is <secret:DB>.");
        assert_eq!(p.chunks[0].heading.as_deref(), Some("deploy.md › Deploy"));
        assert_eq!(p.chunks[1].text, "export API_TOKEN=<redacted>");
        assert_eq!(p.chunks[1].anchor.as_deref(), Some("keys"));
        assert_eq!((p.stamp.writer, p.stamp.profile.as_deref()), (Writer::Agent, Some("work")));
        assert!(!p.append && p.dropped == 0);
        // The same source, the same plan; other text, another hash.
        let again = plan(pass(), src.clone(), stamp(), &red()).unwrap();
        assert_eq!(again, p);
        assert_eq!(p.hash.as_deref().map(str::len), Some(64));
        src.body =
            Body::Markdown { name: "deploy.md".into(), text: format!("{text}more"), at_ms: 7 };
        assert_ne!(plan(pass(), src, stamp(), &red()).unwrap().hash, p.hash);
    }

    /// A body's name starts every heading (a note's is its first line): a secret in it is
    /// stored in no heading.
    #[test]
    fn a_name_is_redacted_in_every_heading() {
        for body in [
            Body::Markdown {
                name: "the key is hunter2-value".into(),
                text: "Text.\n\n## Why\n\nMore.\n".into(),
                at_ms: 1,
            },
            Body::Plain { name: "password: s3cretvalue9".into(), text: "Text.".into(), at_ms: 1 },
        ] {
            let src = Source::new(ItemKind::Note, "note://_all/_global/x", body);
            let p = plan(pass(), src, stamp(), &red()).unwrap();
            assert_eq!(p.hits, 1);
            assert!(!p.chunks.is_empty());
            for c in &p.chunks {
                let h = c.heading.as_deref().unwrap_or_default();
                assert!(!h.contains("hunter2") && !h.contains("s3cret"), "{h}");
                assert!(h.contains("<secret:DB>") || h.contains("<redacted>"), "{h}");
            }
        }
    }

    /// §25.7: a file with more than 25 hits is skipped whole; 25 are redacted and kept.
    #[test]
    fn the_25_hit_rule() {
        let line =
            |n: usize| (0..n).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect::<String>();
        let file = |n: usize| {
            Source::new(
                ItemKind::File,
                "file:///repo/fixtures.txt",
                Body::Plain { name: "fixtures.txt".into(), text: line(n), at_ms: 0 },
            )
        };
        let kept = plan(pass(), file(MAX_HITS), stamp(), &red()).unwrap();
        assert_eq!(kept.hits, MAX_HITS);
        assert!(kept.chunks.iter().all(|c| !c.text.contains("def456")));
        assert_eq!(
            plan(pass(), file(MAX_HITS + 1), stamp(), &red()).unwrap_err(),
            Skip::Secrets { hits: MAX_HITS + 1 }
        );
        // A file given as parts (its comments) counts them together.
        let parts = (0..MAX_HITS + 1)
            .map(|i| Part::new(ChunkKind::Comment, format!("password: s3cret{i}"), 0))
            .collect();
        let src = Source::new(ItemKind::File, "file:///repo/a.rs", Body::Parts(parts));
        assert!(matches!(plan(pass(), src, stamp(), &red()), Err(Skip::Secrets { .. })));
        // A conversation: the turn that is a keystore is left out, the rest is kept.
        let turns = vec![
            Part::new(ChunkKind::User, "here is my env file", 1).anchor("u1"),
            Part::new(ChunkKind::User, line(MAX_HITS + 5), 2).anchor("u2"),
            Part::new(
                ChunkKind::Assistant,
                "Don't paste that. Rotate GITHUB_TOKEN=abc123def456 now.",
                3,
            )
            .anchor("a1"),
        ];
        let mut src = Source::new(ItemKind::Session, "session://claude/c1", Body::Parts(turns));
        src.append = true;
        let p = plan(pass(), src, stamp(), &red()).unwrap();
        assert_eq!((p.chunks.len(), p.dropped, p.hits), (2, 1, 1));
        assert_eq!(p.chunks[1].text, "Don't paste that. Rotate GITHUB_TOKEN=<redacted> now.");
        assert_eq!(p.hash, None);
    }

    #[test]
    fn long_turns_are_cut_and_empty_ones_dropped() {
        let turns = vec![
            Part::new(ChunkKind::User, "  ", 1).anchor("u0"),
            Part::new(ChunkKind::Assistant, "A sentence of an answer. ".repeat(300), 2)
                .anchor("a1"),
            Part::new(ChunkKind::Tool, "Edit src/main.rs", 3).anchor("a1"),
        ];
        let mut src = Source::new(ItemKind::Session, "session://claude/c2", Body::Parts(turns));
        src.append = true;
        let p = plan(pass(), src, stamp(), &red()).unwrap();
        assert!(p.chunks.len() > 5);
        assert!(p.chunks.iter().all(|c| c.anchor.as_deref() == Some("a1")));
        assert_eq!(p.chunks.last().map(|c| c.kind), Some(ChunkKind::Tool));
    }
}
