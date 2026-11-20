//! What is made of a file under a root, and of a commit (SPEC §25.6.3): text in, a
//! [`Source`] out. Nothing here reads a disk or a repository.
//!
//! A file gives up to four things, each a tier with its own switch:
//!
//! - **its path**, always: one `path` piece, the path inside the root and then its name's
//!   parts, so `RateLimiter.ts` is found by `rate limiter`;
//! - **a document's text** (Markdown by its headings, other documents in pieces);
//! - **a source file's comments** ([`super::comments`]), each with the line it stands on;
//! - **its code**, when that is switched on: windows of [`WINDOW`] lines, searched by words.

use std::collections::HashSet;

use super::chunk::Part;
use super::comments;
use super::ingest::{Body, Source};
use super::{ChunkKind, ItemKind};

/// How many lines a `code` piece holds.
pub const WINDOW: usize = 60;
/// How much of a file is looked at to tell that it is not text.
pub const SNIFF: usize = 8 << 10;
/// The most files a commit's `path` piece names.
pub const COMMIT_FILES: usize = 50;

/// Which of a file's tiers are read (its path always is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiers {
    pub comments: bool,
    pub code: bool,
}

/// Not text: a NUL in its first [`SNIFF`] bytes.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(SNIFF)].contains(&0)
}

/// What kind of document a file is, by its name: `Some(true)` Markdown, `Some(false)` another
/// document (cut in pieces, no headings), `None` not a document.
pub fn document(name: &str) -> Option<bool> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => match ext.to_ascii_lowercase().as_str() {
            "md" | "mdx" | "markdown" => Some(true),
            "txt" | "rst" | "adoc" | "org" => Some(false),
            _ => None,
        },
        _ => ["README", "CHANGELOG", "NOTES", "TODO"]
            .iter()
            .any(|n| base.eq_ignore_ascii_case(n))
            .then_some(false),
    }
}

/// The words in a file's name, when they are not the name itself: `RateLimiter.ts` gives
/// `rate limiter`, `kb_spike.rs` gives `kb spike`, `README.md` gives nothing.
pub fn name_parts(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => base,
    };
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut prev: Option<char> = None;
    let chars: Vec<char> = stem.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            words.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
            prev = None;
            continue;
        }
        // A hump: `rateLimiter`, and the last capital of a run (`HTTPServer` is `http server`).
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let hump = prev.is_some_and(|p| {
            c.is_uppercase() && (p.is_lowercase() || p.is_numeric() || next_lower)
        });
        if hump && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        word.extend(c.to_lowercase());
        prev = Some(c);
    }
    words.extend((!word.is_empty()).then_some(word));
    (words.len() > 1).then(|| words.join(" "))
}

/// A file under a root as a source. `rel` is its path inside the root (with `/`), `text` what
/// it holds, `at_ms` when it was last written. `repeated` is asked of every comment kept:
/// `true` drops it (a block that many files of a root repeat is stored once).
pub fn source(
    uri: impl Into<String>,
    rel: &str,
    text: &str,
    at_ms: u64,
    tiers: Tiers,
    mut repeated: impl FnMut(&str) -> bool,
) -> Source {
    let path = match name_parts(rel) {
        Some(parts) => format!("{rel}\n{parts}"),
        None => rel.to_owned(),
    };
    let mut bodies = vec![Body::Parts(vec![Part::new(ChunkKind::Path, path, at_ms)])];
    match document(rel) {
        Some(true) => {
            bodies.push(Body::Markdown { name: rel.to_owned(), text: text.to_owned(), at_ms })
        }
        Some(false) => {
            bodies.push(Body::Plain { name: rel.to_owned(), text: text.to_owned(), at_ms })
        }
        None if comments::marks(rel, text).is_some() => {
            let mut parts = Vec::new();
            if tiers.comments {
                for b in comments::extract(rel, text) {
                    if repeated(&b.text) {
                        continue;
                    }
                    let mut p =
                        Part::new(ChunkKind::Comment, b.text.as_str(), at_ms).anchor(b.anchor());
                    if !b.code.is_empty() {
                        p = p.heading(b.code);
                    }
                    parts.push(p);
                }
            }
            if tiers.code {
                let lines: Vec<&str> = text.lines().collect();
                for (i, window) in lines.chunks(WINDOW).enumerate() {
                    let from = i * WINDOW + 1;
                    parts.push(
                        Part::new(ChunkKind::Code, window.join("\n"), at_ms)
                            .anchor(format!("L{from}-L{}", from + window.len() - 1)),
                    );
                }
            }
            bodies.push(Body::Parts(parts));
        }
        None => {}
    }
    let mut s = Source::new(ItemKind::File, uri, Body::Seq(bodies));
    s.title = Some(rel.to_owned());
    s.rel = Some(rel.to_owned());
    s.started_ms = Some(at_ms);
    s.ended_ms = Some(at_ms);
    s
}

/// A commit as a source: its message (subject and body), and the files it changed (at most
/// [`COMMIT_FILES`]), each file once.
pub fn commit(
    uri: impl Into<String>,
    subject: &str,
    body: &str,
    files: &[String],
    at_ms: u64,
) -> Source {
    let mut seen = HashSet::new();
    let files: Vec<String> = files
        .iter()
        .filter(|f| !f.trim().is_empty() && seen.insert(f.as_str()))
        .take(COMMIT_FILES)
        .cloned()
        .collect();
    let message = if body.trim().is_empty() {
        subject.trim().to_owned()
    } else {
        format!("{}\n\n{}", subject.trim(), body.trim())
    };
    let mut parts = vec![Part::new(ChunkKind::Message, message, at_ms)];
    if !files.is_empty() {
        parts.push(Part::new(ChunkKind::Path, files.join("\n"), at_ms));
    }
    let mut s = Source::new(ItemKind::Commit, uri, Body::Parts(parts));
    s.title = Some(subject.trim().to_owned()).filter(|t| !t.is_empty());
    s.files = files;
    s.started_ms = Some(at_ms);
    s.ended_ms = Some(at_ms);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kb::Writer;
    use crate::kb::ingest::{Gate, Never, Stamp, gate, plan};
    use crate::kb::redact::Redactor;

    fn planned(s: Source) -> Vec<Part> {
        let never = Never::default();
        let passed = gate(&Gate::open(&never)).unwrap();
        plan(passed, s, Stamp::new(Writer::File, None), &Redactor::new(Vec::new(), &[]).0)
            .unwrap()
            .chunks
    }

    fn kinds(parts: &[Part]) -> Vec<&'static str> {
        parts.iter().map(|p| p.kind.as_str()).collect()
    }

    #[test]
    fn a_names_parts() {
        for (name, parts) in [
            ("src/RateLimiter.ts", Some("rate limiter")),
            ("kb_spike.rs", Some("kb spike")),
            ("HTTPServer.java", Some("http server")),
            ("web/rate-limiter.test.mjs", Some("rate limiter test")),
            ("parseV2Header.go", Some("parse v2 header")),
            ("README.md", None),
            ("Makefile", None),
            (".gitignore", None),
            ("main.rs", None),
            ("ÉcoleNormale.txt", Some("école normale")),
        ] {
            assert_eq!(name_parts(name).as_deref(), parts, "{name}");
        }
    }

    #[test]
    fn which_files_are_documents() {
        for md in ["README.md", "docs/a.MDX", "x.markdown"] {
            assert_eq!(document(md), Some(true), "{md}");
        }
        for doc in ["notes.txt", "a.rst", "b.adoc", "c.org", "README", "docs/CHANGELOG", "todo"] {
            assert_eq!(document(doc), Some(false), "{doc}");
        }
        for not in ["main.rs", "LICENSE", ".md", "readme.rs", "Makefile"] {
            assert_eq!(document(not), None, "{not}");
        }
        assert!(is_binary(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(!is_binary("plain text, é".as_bytes()));
        let mut late = vec![b'a'; SNIFF];
        late.push(0);
        assert!(!is_binary(&late), "only the first 8 KiB are looked at");
    }

    #[test]
    fn a_document_is_its_path_and_its_sections() {
        let text = "# Deploy\n\nPush the tag.\n\n## Rollback\n\nRevert and push again.\n";
        let p = planned(source(
            "file:///r/docs/DeployGuide.md",
            "docs/DeployGuide.md",
            text,
            5,
            Tiers { comments: true, code: true },
            |_| false,
        ));
        assert_eq!(kinds(&p), ["path", "text", "text"]);
        assert_eq!(p[0].text, "docs/DeployGuide.md\ndeploy guide");
        assert_eq!(p[1].heading.as_deref(), Some("docs/DeployGuide.md › Deploy"));
        assert_eq!(p[2].anchor.as_deref(), Some("rollback"));
        assert_eq!(p[2].text, "Revert and push again.");
        let p = planned(source(
            "file:///r/NOTES",
            "NOTES",
            "remember the milk",
            5,
            Tiers { comments: false, code: false },
            |_| false,
        ));
        assert_eq!(kinds(&p), ["path", "text"]);
        assert_eq!(p[1].heading.as_deref(), Some("NOTES"));
    }

    const RUST: &str = "// Copyright 2026 Somebody. All rights reserved.\n\nuse std::fmt;\n\n/// Waits between two tries, longer each time,\n/// so that a server that is down is not hammered.\nfn backoff(n: u32) -> u64 {\n    // x\n    1 << n\n}\n";

    #[test]
    fn a_source_file_by_its_tiers() {
        let off = Tiers { comments: false, code: false };
        let p = planned(source("file:///r/src/retry.rs", "src/retry.rs", RUST, 5, off, |_| false));
        assert_eq!(kinds(&p), ["path"]);
        assert_eq!(p[0].text, "src/retry.rs");

        let comments = Tiers { comments: true, code: false };
        let p =
            planned(source("file:///r/src/retry.rs", "src/retry.rs", RUST, 5, comments, |_| false));
        // The licence and the two-letter comment are dropped by the rules.
        assert_eq!(kinds(&p), ["path", "comment"]);
        assert!(p[1].text.starts_with("Waits between two tries"));
        assert_eq!(p[1].anchor.as_deref(), Some("L5-L6"));
        assert_eq!(p[1].heading.as_deref(), Some("fn backoff(n: u32) -> u64 {"));

        let all = Tiers { comments: true, code: true };
        let p = planned(source("file:///r/src/retry.rs", "src/retry.rs", RUST, 5, all, |_| false));
        assert_eq!(kinds(&p), ["path", "comment", "code"]);
        assert_eq!(p[2].anchor.as_deref(), Some("L1-L10"));
        assert!(p[2].text.contains("1 << n"));

        // A block that another file of the root holds already is not stored again.
        let p =
            planned(source("file:///r/src/retry.rs", "src/retry.rs", RUST, 5, comments, |_| true));
        assert_eq!(kinds(&p), ["path"]);

        // A long file's code is in windows of 60 lines.
        let long: String = (1..=130).map(|i| format!("let a{i} = {i};\n")).collect();
        let p = planned(source("file:///r/a.rs", "a.rs", &long, 5, all, |_| false));
        let anchors: Vec<_> = p.iter().filter_map(|c| c.anchor.as_deref()).collect();
        assert_eq!(anchors, ["L1-L60", "L61-L120", "L121-L130"]);

        // What the table doesn't know is its path alone, whatever is switched on.
        let p =
            planned(source("file:///r/data.json", "data.json", "{\"a\": 1}", 5, all, |_| false));
        assert_eq!(kinds(&p), ["path"]);
    }

    #[test]
    fn comments_of_real_languages() {
        let one = |rel: &str, text: &str| -> Vec<(String, Option<String>)> {
            planned(source(
                "file:///x",
                rel,
                text,
                0,
                Tiers { comments: true, code: false },
                |_| false,
            ))
            .into_iter()
            .filter(|p| p.kind == ChunkKind::Comment)
            .map(|p| (p.text, p.heading))
            .collect()
        };
        let py = "import os\n\ndef retry(n):\n    \"\"\"Try again after a pause that doubles each time.\"\"\"\n    return n\n\n# Used by the nightly job only.\nLIMIT = 3\n";
        assert_eq!(
            one("jobs/retry.py", py),
            [
                (
                    "Try again after a pause that doubles each time.".to_owned(),
                    Some("def retry(n):".to_owned())
                ),
                ("Used by the nightly job only.".to_owned(), Some("LIMIT = 3".to_owned())),
            ]
        );
        let ts = "/**\n * Limits how many requests one client may make.\n */\nexport class RateLimiter {}\nconst url = \"http://example.com\"; // not read\n";
        assert_eq!(
            one("web/RateLimiter.ts", ts),
            [(
                "Limits how many requests one client may make.".to_owned(),
                Some("export class RateLimiter {}".to_owned())
            )]
        );
        let go = "//go:build linux\n\npackage main\n\n// Serve answers until the listener closes.\nfunc Serve() {}\n";
        assert_eq!(
            one("cmd/serve.go", go),
            [(
                "Serve answers until the listener closes.".to_owned(),
                Some("func Serve() {}".to_owned())
            )]
        );
        let sh = "#!/bin/sh\n# Builds the release archive for every target.\nmake dist\n";
        assert_eq!(
            one("scripts/release", sh),
            [(
                "Builds the release archive for every target.".to_owned(),
                Some("make dist".to_owned())
            )]
        );
        let sql = "-- Sessions that ended without an answer, newest first.\nSELECT 1;\n";
        assert_eq!(one("q.sql", sql).len(), 1);
    }

    #[test]
    fn a_commit_is_its_message_and_its_files() {
        let files: Vec<String> =
            (0..60).map(|i| format!("src/f{i}.rs")).chain(["src/f0.rs".to_owned()]).collect();
        let s = commit(
            "commit://r1/abc",
            " Retry with backoff ",
            "Because the server\nwas hammered.\n",
            &files,
            9,
        );
        assert_eq!(s.title.as_deref(), Some("Retry with backoff"));
        assert_eq!(s.files.len(), COMMIT_FILES);
        assert_eq!((s.started_ms, s.ended_ms, s.kind), (Some(9), Some(9), ItemKind::Commit));
        let p = planned(s);
        assert_eq!(kinds(&p), ["message", "path"]);
        assert_eq!(p[0].text, "Retry with backoff\n\nBecause the server\nwas hammered.");
        assert!(p[1].text.starts_with("src/f0.rs\nsrc/f1.rs"));
        let p = planned(commit("commit://r1/def", "Fix a typo", "", &[], 9));
        assert_eq!(kinds(&p), ["message"]);
        assert_eq!(p[0].text, "Fix a typo");
    }
}
