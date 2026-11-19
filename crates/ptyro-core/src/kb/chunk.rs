//! The cutters (SPEC §25.5 step 4): text into pieces a search can return. A piece aims at
//! [`AIM`] characters and never passes [`LIMIT`]; it is cut at a paragraph, then a sentence,
//! then a word. Nothing overlaps: a document's piece carries its heading path instead, and a
//! message cut in parts keeps its anchor.
//!
//! - [`split`]: any text.
//! - [`markdown`]: a document by its headings; [`plain`]: one with none.
//! - [`message`]: one turn of a conversation (or a comment, a command: anything that is
//!   already one piece unless it is too long).
//!
//! All of them are linear in the text and take any text at all.

use std::collections::HashMap;

use super::ChunkKind;

/// What a piece aims at, in characters.
pub const AIM: usize = 1200;
/// What no piece passes, in characters.
pub const LIMIT: usize = 2000;
/// Text up to this long is one piece: cutting 1,300 characters in two helps nobody.
const WHOLE: usize = AIM + AIM / 4;
/// A cut is not made before this, unless there is no other.
const MIN: usize = AIM / 2;
/// The most a heading path holds.
const HEADING: usize = 300;

/// One piece of an item, before it is stored: a chunk without its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub kind: ChunkKind,
    /// A message's uuid, a comment's lines (`L120-L161`), a heading's slug: what finds the
    /// piece again in its source. Parts cut from one thing share it.
    pub anchor: Option<String>,
    /// A document's heading path, the line a comment stands on.
    pub heading: Option<String>,
    pub text: String,
    /// A command's folder, exit and duration; a repeat count.
    pub meta: Option<serde_json::Value>,
    pub at_ms: u64,
}

impl Part {
    pub fn new(kind: ChunkKind, text: impl Into<String>, at_ms: u64) -> Self {
        Self { kind, anchor: None, heading: None, text: text.into(), meta: None, at_ms }
    }

    pub fn anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    pub fn heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = Some(heading.into());
        self
    }
}

/// `text` in pieces, each trimmed, none empty, none over [`LIMIT`] characters.
pub fn split(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let (piece, tail) = rest.split_at(cut(rest));
        let piece = piece.trim_end();
        if !piece.is_empty() {
            out.push(piece);
        }
        rest = tail.trim_start();
    }
    out
}

/// Where the first piece of `text` ends: a byte offset (`text.len()` when it is one piece).
fn cut(text: &str) -> usize {
    // The best place of each rank (0 a paragraph, 1 a sentence or a line, 2 a word): the last
    // one at or before AIM, and the first one after it.
    let mut before = [None::<usize>; 3];
    let mut after = [None::<usize>; 3];
    // The last word boundary of all, for a text with no place past MIN.
    let mut early = None;
    let (mut newlines, mut space, mut last) = (0, false, ' ');
    let mut end = None;
    for (n, (i, c)) in text.char_indices().enumerate() {
        if n == LIMIT {
            end = Some(i);
            break;
        }
        if c.is_whitespace() {
            newlines += usize::from(c == '\n');
            space = true;
            continue;
        }
        if space && n > 0 {
            let rank = match newlines {
                2.. => 0,
                1 => 1,
                0 if matches!(last, '.' | '?' | '!' | '。' | '？' | '！') => 1,
                _ => 2,
            };
            // A better rank is also a place of every lesser one.
            for r in rank..3 {
                if n < MIN {
                    early = Some(i);
                } else if n <= AIM {
                    before[r] = Some(i);
                } else if after[r].is_none() {
                    after[r] = Some(i);
                }
            }
        }
        (newlines, space, last) = (0, false, c);
    }
    let Some(hard) = end else {
        // Under LIMIT. Short enough to stay whole?
        if text.chars().nth(WHOLE).is_none() {
            return text.len();
        }
        return (0..3).find_map(|r| before[r].or(after[r])).or(early).unwrap_or(text.len());
    };
    (0..3).find_map(|r| before[r].or(after[r])).or(early).unwrap_or(hard)
}

/// A turn, a comment, a command: itself when it is short, else its parts, each with the same
/// kind, anchor, heading and time. One with no text gives nothing.
pub fn message(part: Part) -> Vec<Part> {
    if part.text.len() <= WHOLE {
        // (Bytes: no more characters than that.)
        let text = part.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        if text.len() == part.text.len() {
            return vec![part];
        }
        return vec![Part { text: text.to_owned(), ..part }];
    }
    split(&part.text).into_iter().map(|t| Part { text: t.to_owned(), ..part.clone() }).collect()
}

/// A document with no headings (`.txt`, a `README` with no marks): its pieces, each under the
/// document's `name`.
pub fn plain(name: &str, text: &str, at_ms: u64) -> Vec<Part> {
    split(text).into_iter().map(|t| Part::new(ChunkKind::Text, t, at_ms).heading(name)).collect()
}

/// A Markdown document by its headings: every section's text in pieces, each under its heading
/// path (`SPEC.md › 22. Chats › 22.4 Storage`) and anchored at its heading's slug
/// (`224-storage`, as GitHub links it). A heading's own line is not text: its words are in the
/// path of everything under it. `#` lines inside a code fence are code.
pub fn markdown(name: &str, text: &str, at_ms: u64) -> Vec<Part> {
    let mut doc = Doc {
        name,
        text,
        at_ms,
        out: Vec::new(),
        path: Vec::new(),
        slugs: HashMap::new(),
        anchor: None,
        from: 0,
    };
    let mut fence: Option<(u8, usize)> = None;
    // A line that a `===` or `---` under it would make a heading: one line of text on its own.
    let mut alone: Option<(usize, usize)> = None;
    let mut blank = true;
    let mut at = 0;
    // A header between `---` lines at the top (a note's) is text, not a heading.
    if text.starts_with("---\n") || text.starts_with("---\r\n") {
        let mut pos = 0;
        for (n, line) in text.split_inclusive('\n').enumerate() {
            pos += line.len();
            if n > 0 && line.trim_end() == "---" {
                at = pos;
                break;
            }
        }
    }
    for line in text[at..].split_inclusive('\n') {
        let (start, end) = (at, at + line.len());
        at = end;
        let l = line.trim_end();
        let t = l.trim_start();
        let indented = l.len() - t.len() >= 4;
        if let Some((mark, n)) = fence {
            let run = t.bytes().take_while(|b| *b == mark).count();
            if !indented && run >= n && t[run..].trim().is_empty() {
                fence = None;
            }
            (alone, blank) = (None, false);
            continue;
        }
        if !indented && (t.starts_with("```") || t.starts_with("~~~")) {
            let mark = t.as_bytes()[0];
            fence = Some((mark, t.bytes().take_while(|b| *b == mark).count()));
            (alone, blank) = (None, false);
            continue;
        }
        if !indented && let Some((level, title)) = atx(t) {
            doc.heading(start, end, level, title);
            (alone, blank) = (None, true);
            continue;
        }
        if !indented
            && let Some((a, b)) = alone
            && let Some(level) = underline(t)
        {
            doc.heading(a, end, level, text[a..b].trim());
            (alone, blank) = (None, true);
            continue;
        }
        if t.is_empty() {
            (alone, blank) = (None, true);
        } else {
            let listed = t.starts_with(['|', '>'])
                || t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("+ ");
            alone = (blank && !indented && !listed).then_some((start, end));
            blank = false;
        }
    }
    doc.flush(text.len());
    doc.out
}

struct Doc<'a> {
    name: &'a str,
    text: &'a str,
    at_ms: u64,
    out: Vec<Part>,
    /// The headings above here: (level, title).
    path: Vec<(u8, String)>,
    /// How often each slug was made, to number the repeats.
    slugs: HashMap<String, usize>,
    anchor: Option<String>,
    /// Where the current section's text starts.
    from: usize,
}

impl Doc<'_> {
    /// The section so far, up to `upto`, as parts.
    fn flush(&mut self, upto: usize) {
        let mut heading = self.name.to_owned();
        for (_, title) in self.path.iter().filter(|(_, t)| !t.is_empty()) {
            heading.push_str(" › ");
            heading.push_str(title);
        }
        if heading.len() > HEADING {
            let mut cut = HEADING;
            while !heading.is_char_boundary(cut) {
                cut -= 1;
            }
            heading.truncate(cut);
            heading.push('…');
        }
        for piece in split(&self.text[self.from.min(upto)..upto]) {
            self.out.push(Part {
                kind: ChunkKind::Text,
                anchor: self.anchor.clone(),
                heading: Some(heading.clone()),
                text: piece.to_owned(),
                meta: None,
                at_ms: self.at_ms,
            });
        }
    }

    /// A heading whose lines are `start..end`: what is above it is a section.
    fn heading(&mut self, start: usize, end: usize, level: u8, title: &str) {
        self.flush(start);
        self.from = end;
        while self.path.last().is_some_and(|(l, _)| *l >= level) {
            self.path.pop();
        }
        self.path.push((level, title.to_owned()));
        let slug = slug(title);
        let n = self.slugs.entry(slug.clone()).or_insert(0);
        self.anchor =
            Some(if *n == 0 { slug } else { format!("{slug}-{n}") }).filter(|s| !s.is_empty());
        *n += 1;
    }
}

/// `## Title ##` → (2, "Title").
fn atx(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    let rest = &line[level..];
    if !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let title = rest.trim();
    // Closing marks count only after a space (`# C#` is about C#).
    let open = title.trim_end_matches('#');
    let title =
        if open.is_empty() || open.ends_with([' ', '\t']) { open.trim_end() } else { title };
    Some((level as u8, title))
}

/// `===` under a line makes it a first-level heading, `---` a second-level one.
fn underline(line: &str) -> Option<u8> {
    match line.as_bytes().first() {
        Some(b'=') if line.len() >= 2 && line.bytes().all(|b| b == b'=') => Some(1),
        Some(b'-') if line.len() >= 3 && line.bytes().all(|b| b == b'-') => Some(2),
        _ => None,
    }
}

/// A heading's anchor as GitHub makes it: lower case, spaces to `-`, marks dropped.
pub fn slug(title: &str) -> String {
    title
        .chars()
        .filter_map(|c| match c {
            ' ' | '\t' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn short_text_is_one_piece_and_nothing_is_nothing() {
        assert_eq!(split("  one small thing.\n"), ["one small thing."]);
        assert!(split("").is_empty() && split(" \n\t ").is_empty());
        let just = "word ".repeat(WHOLE / 5);
        assert_eq!(split(&just).len(), 1);
    }

    #[test]
    fn a_cut_is_at_a_paragraph_then_a_sentence_then_a_word() {
        // Paragraphs of about 540 characters: two fit under the aim, three don't.
        let para = format!("{}.", "lorem ipsum ".repeat(45).trim_end());
        let text = [para.as_str(); 7].join("\n\n");
        let pieces = split(&text);
        assert_eq!(pieces.len(), 4, "{:?}", pieces.iter().map(|p| chars(p)).collect::<Vec<_>>());
        for p in &pieces[..3] {
            assert_eq!(*p, format!("{para}\n\n{para}"));
        }
        // One paragraph of sentences: cut after a full stop, at or under the aim.
        let sentence = "The quick brown fox jumps over the lazy dog again and again. ";
        let text = sentence.repeat(60);
        let pieces = split(&text);
        assert!(pieces.len() >= 3);
        for p in &pieces {
            assert!(p.ends_with('.') && chars(p) <= WHOLE, "{} {p:?}", chars(p));
            assert!(chars(p) > MIN || std::ptr::eq(*p, *pieces.last().unwrap()));
        }
        // No sentence ends: at a word.
        let text = "word ".repeat(1000);
        for p in split(&text) {
            assert!(p.ends_with("word") && p.starts_with("word") && chars(p) <= WHOLE);
        }
        // No space at all: a hard cut at the limit, between characters.
        let text = "é".repeat(LIMIT * 2 + 10);
        let pieces = split(&text);
        assert_eq!(pieces.iter().map(|p| chars(p)).collect::<Vec<_>>(), [LIMIT, LIMIT, 10]);
        // Nothing is lost or repeated but white space.
        let text = format!("{}\n\n{}", sentence.repeat(40), "tail ".repeat(700));
        let joined: String = split(&text).concat();
        let strip = |s: &str| s.split_whitespace().collect::<String>();
        assert_eq!(strip(&joined), strip(&text));
    }

    #[test]
    fn a_long_paragraph_is_preferred_whole_up_to_the_limit() {
        // 1,700 characters in one paragraph, then another: the cut is at the paragraph, past
        // the aim, not at a sentence before it.
        let long = format!("{}.", "alpha beta ".repeat(154).trim_end());
        let text = format!("{long}\n\nSecond paragraph here.");
        let pieces = split(&text);
        assert_eq!(pieces, [long.as_str(), "Second paragraph here."]);
    }

    #[test]
    fn a_message_keeps_its_anchor_through_its_parts() {
        let turn = Part::new(ChunkKind::Assistant, "Done. ".repeat(700), 5).anchor("uuid-1");
        let parts = message(turn);
        assert!(parts.len() >= 3);
        assert!(parts.iter().all(|p| p.anchor.as_deref() == Some("uuid-1")
            && p.kind == ChunkKind::Assistant
            && p.at_ms == 5
            && chars(&p.text) <= LIMIT));
        let short = Part::new(ChunkKind::User, " fix the test \n", 1);
        assert_eq!(message(short)[0].text, "fix the test");
        assert!(message(Part::new(ChunkKind::User, "  \n", 1)).is_empty());
    }

    #[test]
    fn markdown_is_cut_at_headings_with_their_path() {
        let text = "\
Intro before any heading.

# The Spec

Opening words.

## 22.4 Storage: `chat.db`

Where it lives.

```sh
# not a heading
ls
```

Still storage.

### Details ##

Deep.

## Other & more!

Second.

Setext Title
============

Under setext.

Sub one
-------

Under sub.

## Other & more!

Again.
";
        let parts = markdown("SPEC.md", text, 9);
        let got: Vec<(&str, Option<&str>, &str)> = parts
            .iter()
            .map(|p| (p.heading.as_deref().unwrap(), p.anchor.as_deref(), p.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("SPEC.md", None, "Intro before any heading."),
                ("SPEC.md › The Spec", Some("the-spec"), "Opening words."),
                (
                    "SPEC.md › The Spec › 22.4 Storage: `chat.db`",
                    Some("224-storage-chatdb"),
                    "Where it lives.\n\n```sh\n# not a heading\nls\n```\n\nStill storage."
                ),
                (
                    "SPEC.md › The Spec › 22.4 Storage: `chat.db` › Details",
                    Some("details"),
                    "Deep."
                ),
                ("SPEC.md › The Spec › Other & more!", Some("other--more"), "Second."),
                ("SPEC.md › Setext Title", Some("setext-title"), "Under setext."),
                ("SPEC.md › Setext Title › Sub one", Some("sub-one"), "Under sub."),
                ("SPEC.md › Setext Title › Other & more!", Some("other--more-1"), "Again."),
            ]
        );
        assert!(parts.iter().all(|p| p.kind == ChunkKind::Text && p.at_ms == 9));
    }

    #[test]
    fn markdown_traps() {
        // A note's header is text, not a setext heading; a rule and a list are not underlines'
        // text; `#hashtag` and `#!` are not headings; an indented `#` is code.
        let text = "---\nby: you\ntags: [a]\n---\n\nBody one.\n\n- item\n---\n\n#hashtag and\n    # code\n\ntwo lines\nof text\n---\nend";
        let parts = markdown("n.md", text, 0);
        assert_eq!(parts.len(), 1, "{parts:?}");
        assert!(parts[0].text.starts_with("---\nby: you") && parts[0].text.ends_with("end"));
        assert_eq!(parts[0].anchor, None);
        // Windows line ends, an empty heading, a heading at the very end, an unclosed fence.
        let parts = markdown("w.md", "# A\r\ntext\r\n#\r\nmore\r\n## End", 0);
        assert_eq!(parts.len(), 2);
        assert_eq!(
            (parts[0].heading.as_deref(), parts[0].text.as_str()),
            (Some("w.md › A"), "text")
        );
        assert_eq!((parts[1].heading.as_deref(), parts[1].anchor.as_deref()), (Some("w.md"), None));
        let parts = markdown("f.md", "# A\n```\n# B\nnever closed", 0);
        assert_eq!(parts.len(), 1);
        assert_eq!(atx("# C#"), Some((1, "C#")));
        assert_eq!(atx("####### seven"), None);
        // A long section is cut, each piece under the same heading and anchor.
        let long = format!("# Big\n\n{}", "A sentence of the section. ".repeat(200));
        let parts = markdown("b.md", &long, 0);
        assert!(parts.len() > 3);
        assert!(
            parts.iter().all(|p| p.anchor.as_deref() == Some("big") && chars(&p.text) <= LIMIT)
        );
        assert_eq!(plain("notes.txt", "# not a heading here\n\ntext", 0).len(), 1);
    }

    /// Linear, whatever the text: 4 MB of each shape that could go wrong.
    #[test]
    fn large_and_odd_text_is_cut_quickly() {
        let t = std::time::Instant::now();
        for text in [
            "x".repeat(4 << 20),
            "\n".repeat(4 << 20),
            "# h\n".repeat(1 << 20),
            "a. ".repeat(1 << 20),
            "```\n".repeat(1 << 20),
            "word\n---\n".repeat(400_000),
        ] {
            for p in markdown("big.md", &text, 0) {
                assert!(chars(&p.text) <= LIMIT);
            }
            for p in split(&text) {
                assert!(chars(p) <= LIMIT && !p.is_empty());
            }
        }
        assert!(t.elapsed().as_secs() < 60, "{:?}", t.elapsed());
    }
}
