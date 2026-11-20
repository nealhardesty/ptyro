//! Notes (SPEC §25.6.4): small Markdown files about one thing, the only part of the knowledge
//! base that someone writes. The file is the truth; this module reads one and writes one, as
//! text. Where a note lives says whose it is and what it is about:
//! `<notes>/<profile or _all>/<project slug or _global>/<slug>.md`, which is also its URI,
//! `note://<profile or _all>/<project or _global>/<slug>`.
//!
//! A note may start with a header between `---` lines, which ptyro writes when it writes the
//! note (`by`, `session`, `tags`, `sources`, `created`, `updated`, `replaces`). A file with no
//! header is as good as any other: it is yours, written now.

use super::ItemKind;
use super::ingest::{Body, Source};

/// The folder of notes about no project.
pub const GLOBAL: &str = "_global";
/// The folder of notes that are no profile's: every profile's agents may see them.
pub const EVERYONE: &str = "_all";
/// Where a note that was replaced is kept, beside the others: on disk, out of search.
pub const OLD: &str = "_old";
/// `by` in the header of a note you wrote.
pub const YOU: &str = "you";
/// The most text an agent may give `remember` (§25.6.4).
pub const AGENT_MAX: usize = 8 * 1024;
/// The most notes a session's agent writes in a day.
pub const AGENT_A_DAY: u32 = 20;
/// The longest title made from a note's first line.
const TITLE: usize = 80;

/// What a note's header says. Every part is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    /// Who wrote it: `you`, or an agent as ptyro names it (`claude`, `claude@work`).
    pub by: Option<String>,
    /// The session or chat that wrote it, as an address; "a private session" for one.
    pub session: Option<String>,
    pub tags: Vec<String>,
    /// Addresses it rests on.
    pub sources: Vec<String>,
    pub created_ms: Option<u64>,
    pub updated_ms: Option<u64>,
    /// The URI of the note this one took the place of.
    pub replaces: Option<String>,
}

impl Header {
    /// Whether an agent wrote the note, and which.
    pub fn agent(&self) -> Option<&str> {
        self.by.as_deref().map(str::trim).filter(|b| !b.is_empty() && *b != YOU)
    }
}

/// A list as the header holds it: `[a, b]`, or `a, b`.
fn list(v: &str) -> Vec<String> {
    let v = v.trim();
    let v = v.strip_prefix('[').and_then(|v| v.strip_suffix(']')).unwrap_or(v);
    v.split(',')
        .map(|t| t.trim().trim_matches(['"', '\'']).trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// A note's text as its header and its body. A file that doesn't start with a header (a `---`
/// line, `key: value` lines, a `---` line) is all body.
pub fn parse(text: &str) -> (Header, &str) {
    let none = (Header::default(), text);
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return none;
    };
    let mut h = Header::default();
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        at += line.len();
        let l = line.trim();
        if l == "---" {
            return (h, &rest[at..]);
        }
        if l.is_empty() {
            continue;
        }
        let Some((key, value)) = l.split_once(':') else { return none };
        let value = value.trim();
        let one = || Some(value.trim_matches(['"', '\'']).to_owned()).filter(|v| !v.is_empty());
        match key.trim() {
            "by" => h.by = one(),
            "session" => h.session = one(),
            "tags" => h.tags = list(value),
            "sources" => h.sources = list(value),
            "created" => h.created_ms = time(value),
            "updated" => h.updated_ms = time(value),
            "replaces" => h.replaces = one(),
            // A key of your own: kept in the file, nothing to ptyro.
            k if !k.is_empty() && !k.contains(' ') => {}
            _ => return none,
        }
    }
    none
}

/// A note as ptyro writes it: the header (only what is known), then the body.
pub fn render(h: &Header, body: &str) -> String {
    let mut out = String::from("---\n");
    let mut line = |k: &str, v: &str| out.push_str(&format!("{k}: {}\n", v.replace('\n', " ")));
    if let Some(by) = &h.by {
        line("by", by);
    }
    if let Some(s) = &h.session {
        line("session", s);
    }
    if !h.tags.is_empty() {
        line("tags", &format!("[{}]", h.tags.join(", ")));
    }
    if !h.sources.is_empty() {
        line("sources", &format!("[{}]", h.sources.join(", ")));
    }
    if let Some(t) = h.created_ms {
        line("created", &stamp(t));
    }
    if let Some(t) = h.updated_ms {
        line("updated", &stamp(t));
    }
    if let Some(r) = &h.replaces {
        line("replaces", r);
    }
    out.push_str("---\n\n");
    out.push_str(body.trim());
    out.push('\n');
    out
}

/// A tag as it is kept: one word, no comma or bracket (those are the list's).
pub fn tag(raw: &str) -> Option<String> {
    let t: String = raw
        .trim()
        .trim_start_matches('#')
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| !matches!(c, ',' | '[' | ']' | '"' | '\''))
        .collect();
    (!t.is_empty()).then_some(t)
}

/// A body's title and the text under it: its first heading, when it starts with one (the text
/// is then what follows); else its first line, cut short (the text is all of it).
pub fn title(body: &str) -> (Option<String>, &str) {
    let trimmed = body.trim_start();
    let first = trimmed.lines().next().unwrap_or_default().trim();
    let hashes = first.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && first[hashes..].starts_with([' ', '\t']) {
        let t = first[hashes..].trim().trim_end_matches('#').trim();
        if !t.is_empty() {
            return (
                Some(t.to_owned()),
                trimmed[trimmed.find('\n').unwrap_or(trimmed.len())..].trim(),
            );
        }
    }
    let t: String = first.chars().take(TITLE).collect();
    ((!t.is_empty()).then_some(t), body.trim())
}

/// A new note's body from what was given: `text` under `# title`, unless it starts with a
/// heading of its own.
pub fn compose(text: &str, title: Option<&str>) -> String {
    let text = text.trim();
    match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if !text.starts_with('#') => format!("# {}\n\n{text}", t.replace('\n', " ")),
        _ => text.to_owned(),
    }
}

/// The file name a title gives (without `.md`): `[a-z0-9-]`, at most 40 characters; `note`
/// when nothing of it is left.
pub fn slug(title: &str) -> String {
    Some(crate::model::slugify(title)).filter(|s| !s.is_empty()).unwrap_or_else(|| "note".into())
}

/// `note://<profile or _all>/<project slug or _global>/<slug>`.
pub fn uri(profile: Option<&str>, project: Option<&str>, slug: &str) -> String {
    format!("note://{}/{}/{slug}", profile.unwrap_or(EVERYONE), project.unwrap_or(GLOBAL))
}

/// A note's place from its URI: its profile (`None`: every profile's), its project's slug
/// (`None`: about no project) and its slug.
pub fn place(uri: &str) -> Option<(Option<&str>, Option<&str>, &str)> {
    let mut parts = uri.strip_prefix("note://")?.split('/');
    let (profile, project, slug) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || profile.is_empty() || project.is_empty() || slug.is_empty() {
        return None;
    }
    Some((Some(profile).filter(|p| *p != EVERYONE), Some(project).filter(|p| *p != GLOBAL), slug))
}

/// A note's path inside the notes folder, from its URI.
pub fn rel(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("note://")?;
    place(uri).map(|_| format!("{rest}.md"))
}

/// A note read from its file, as a source for the one way in (§25.5): its title, its text cut
/// at its headings under that title, its tags as a last line so that they are found. `at_ms`:
/// when the file was last written. Also what its header says, for the stamp.
pub fn source(uri: &str, text: &str, at_ms: u64) -> (Source, Header) {
    let (header, body) = parse(text);
    let (title, under) = title(body);
    let name = title.clone().unwrap_or_else(|| uri.rsplit('/').next().unwrap_or(uri).to_owned());
    // A note that is one line is that line: a title alone would have no text to find.
    let mut text = if under.is_empty() { name.clone() } else { under.to_owned() };
    if !header.tags.is_empty() {
        text.push_str(&format!("\n\ntags: {}", header.tags.join(", ")));
    }
    let mut source = Source::new(ItemKind::Note, uri, Body::Markdown { name, text, at_ms });
    source.title = title;
    source.rel = rel(uri);
    source.started_ms = Some(header.created_ms.unwrap_or(at_ms));
    source.ended_ms = Some(at_ms);
    (source, header)
}

/// `2026-11-19T22:38:07Z`.
pub fn stamp(ms: u64) -> String {
    let m = super::card::minute(ms);
    format!("{}T{}:{:02}Z", &m[..10], &m[11..], ms / 1000 % 60)
}

/// A header's time: `2026-11-19`, `2026-11-19T22:38`, `2026-11-19 22:38:07Z` (UTC).
pub fn time(v: &str) -> Option<u64> {
    let v = v.trim().trim_matches(['"', '\'']).trim_end_matches('Z');
    let (date, clock) = match v.split_once(['T', ' ']) {
        Some((d, c)) => (d, c),
        None => (v, ""),
    };
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    if d.next().is_some()
        || !(1..=12).contains(&m)
        || !(1..=31).contains(&day)
        || !(1970..=9999).contains(&y)
    {
        return None;
    }
    let mut secs = 0;
    for (i, part) in clock.split(':').filter(|p| !p.is_empty()).enumerate() {
        let n = part.split('.').next()?.parse::<u64>().ok()?;
        match i {
            0 if n < 24 => secs += n * 3600,
            1 if n < 60 => secs += n * 60,
            2 if n < 61 => secs += n,
            _ => return None,
        }
    }
    // Days from the civil date (Howard Hinnant's algorithm, as `card::minute` undoes it).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some((days as u64 * 86_400 + secs) * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_is_read_and_a_file_without_one_is_all_body() {
        let text = "---\nby: claude@work\nsession: 01JABC\ntags: [db, decision]\nsources: k:7f3k/2, k:9abc\ncreated: 2026-11-19T22:38:07Z\nreplaces: note://work/web/old\nmood: fine\n---\n\n# Retry policy\n\nThree tries.\n";
        let (h, body) = parse(text);
        assert_eq!(h.by.as_deref(), Some("claude@work"));
        assert_eq!(h.agent(), Some("claude@work"));
        assert_eq!(h.session.as_deref(), Some("01JABC"));
        assert_eq!(h.tags, ["db", "decision"]);
        assert_eq!(h.sources, ["k:7f3k/2", "k:9abc"]);
        assert_eq!(h.created_ms.map(stamp).as_deref(), Some("2026-11-19T22:38:07Z"));
        assert_eq!(h.replaces.as_deref(), Some("note://work/web/old"));
        assert_eq!(body, "\n# Retry policy\n\nThree tries.\n");
        // What ptyro writes reads back the same.
        let again = render(&h, body);
        assert_eq!(parse(&again).0, h);
        assert!(again.ends_with("---\n\n# Retry policy\n\nThree tries.\n"), "{again}");

        // No header, a rule that isn't one, one that never closes: all of it is the note.
        for plain in ["# T\n\ntext", "---\n\nnot a header at all\n---\n", "---\nby: you\nno end"] {
            assert_eq!(parse(plain), (Header::default(), plain));
        }
        let (h, _) = parse("---\nby: you\n---\nmine");
        assert_eq!((h.by.as_deref(), h.agent()), (Some("you"), None));
    }

    #[test]
    fn the_title_is_the_first_heading_else_the_first_line() {
        assert_eq!(
            title("\n# Retry policy \n\nThree tries.\n"),
            (Some("Retry policy".into()), "Three tries.")
        );
        assert_eq!(title("## Why ##\n"), (Some("Why".into()), ""));
        assert_eq!(
            title("we chose X because Y\nmore"),
            (Some("we chose X because Y".into()), "we chose X because Y\nmore")
        );
        assert_eq!(title("#hashtag first"), (Some("#hashtag first".into()), "#hashtag first"));
        assert_eq!(title("  \n"), (None, ""));
        assert_eq!(title(&"x".repeat(200)).0.unwrap().len(), 80);
        assert_eq!(compose(" text ", Some("A title")), "# A title\n\ntext");
        assert_eq!(compose("# Mine\n\ntext", Some("A title")), "# Mine\n\ntext");
        assert_eq!(compose("text", None), "text");
        assert_eq!(slug("Retry policy: 3 tries!"), "retry-policy-3-tries");
        assert_eq!(slug("…"), "note");
        assert_eq!(tag(" #two words, "), Some("two-words".into()));
        assert_eq!(tag(" , "), None);
    }

    #[test]
    fn a_uri_says_whose_a_note_is_and_what_it_is_about() {
        let u = uri(Some("work"), Some("web"), "retry-policy");
        assert_eq!(u, "note://work/web/retry-policy");
        assert_eq!(place(&u), Some((Some("work"), Some("web"), "retry-policy")));
        assert_eq!(rel(&u).as_deref(), Some("work/web/retry-policy.md"));
        let u = uri(None, None, "x");
        assert_eq!(u, "note://_all/_global/x");
        assert_eq!(place(&u), Some((None, None, "x")));
        for bad in ["note://work/web", "note://work/web/a/b", "file:///x", "note://work//x"] {
            assert_eq!(place(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_note_is_a_source_under_its_title_with_its_tags_findable() {
        let text = "---\nby: codex\ntags: [db]\ncreated: 2026-11-01\n---\n# Storage\n\nWe chose SQLite.\n\n## Why\n\nNo server.\n";
        let (s, h) = source("note://work/web/storage", text, 5_000);
        assert_eq!(s.kind, ItemKind::Note);
        assert_eq!(s.title.as_deref(), Some("Storage"));
        assert_eq!(s.rel.as_deref(), Some("work/web/storage.md"));
        assert_eq!((s.started_ms, s.ended_ms), (time("2026-11-01"), Some(5_000)));
        assert_eq!(h.agent(), Some("codex"));
        let Body::Markdown { name, text, at_ms } = &s.body else { panic!() };
        assert_eq!((name.as_str(), *at_ms), ("Storage", 5_000));
        assert_eq!(text, "We chose SQLite.\n\n## Why\n\nNo server.\n\ntags: db");
        let parts = crate::kb::chunk::markdown(name, text, *at_ms);
        assert_eq!(parts[0].heading.as_deref(), Some("Storage"));
        assert_eq!(parts[1].heading.as_deref(), Some("Storage › Why"));
        // One line is its own title and its own text.
        let (s, _) = source("note://_all/_global/x", "# only a title\n", 1);
        let Body::Markdown { text, .. } = &s.body else { panic!() };
        assert_eq!(text, "only a title");
        let (s, _) = source("note://_all/_global/x", "we chose X because Y", 1);
        assert_eq!(s.title.as_deref(), Some("we chose X because Y"));
        assert_eq!(s.started_ms, Some(1));
    }

    #[test]
    fn times_are_read_in_the_shapes_people_write() {
        assert_eq!(time("1970-01-02"), Some(86_400_000));
        assert_eq!(time("2026-11-19 22:38"), time("2026-11-19T22:38:00Z"));
        assert_eq!(stamp(time("2024-02-29T23:59:59Z").unwrap()), "2024-02-29T23:59:59Z");
        assert!(time("9999-12-31T23:59:59Z").is_some());
        for bad in [
            "",
            "yesterday",
            "2026-13-01",
            "2026-11-19T25:00",
            "1969-01-01",
            "10000-01-01",
            "9223372036854775807-01-01",
        ] {
            assert_eq!(time(bad), None, "{bad}");
        }
    }
}
