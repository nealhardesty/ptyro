//! Reading a diff (a checkpoint's, SPEC §15.3) into files, hunks and lines, and the small
//! questions pair mode and `gc` ask: a snapshot of the working tree, whether it is dirty, whether
//! one commit is an ancestor of another. Everything shells out to git, like the rest of this
//! crate.

use std::path::Path;

use ptyro_core::model::{DiffFile, DiffLine, Hunk, LineKind};

use crate::Git;

/// Diffs stop growing here (lines); the rest is reported as truncated.
pub const MAX_LINES: usize = 20_000;

impl Git {
    /// A commit of the working tree as it is right now, without touching the index, the stash
    /// list or any file (`git stash create`); `HEAD` when nothing is uncommitted.
    pub fn snapshot(&self, path: &Path) -> Option<String> {
        match self.run(path, &["stash", "create"]) {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => self.rev(path, "HEAD"),
        }
    }

    pub fn is_ancestor(&self, path: &Path, a: &str, b: &str) -> bool {
        self.ok(path, &["merge-base", "--is-ancestor", "--end-of-options", a, b])
    }

    /// Whether the checkout has uncommitted or untracked changes.
    pub fn dirty(&self, path: &Path) -> bool {
        self.run(path, &["status", "--porcelain=v1", "--untracked-files=normal"])
            .is_ok_and(|s| !s.trim().is_empty())
    }
}

/// `-12,4` / `+3` → start.
fn range_start(s: &str) -> u32 {
    s[1..].split(',').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// A path as git prints it: `"..."` with C escapes (`\t`, `\"`, `\\`, `\303\251` octal bytes)
/// when `core.quotePath` quoted it, else as is.
fn unquote(p: &str) -> String {
    let p = p.trim();
    let Some(q) = p.strip_prefix('"').and_then(|q| q.strip_suffix('"')) else {
        return p.to_owned();
    };
    let mut out = Vec::with_capacity(q.len());
    let mut b = q.bytes().peekable();
    while let Some(c) = b.next() {
        if c != b'\\' {
            out.push(c);
            continue;
        }
        match b.next() {
            Some(d @ b'0'..=b'7') => {
                let mut n = u32::from(d - b'0');
                for _ in 0..2 {
                    match b.peek() {
                        Some(&o @ b'0'..=b'7') => {
                            n = n * 8 + u32::from(o - b'0');
                            b.next();
                        }
                        _ => break,
                    }
                }
                out.push(n as u8);
            }
            Some(b'a') => out.push(7),
            Some(b'b') => out.push(8),
            Some(b'f') => out.push(12),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'v') => out.push(11),
            Some(o) => out.push(o),
            None => out.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `a/x` or `"a/x"` → `x` (unquoting first: the prefix sits inside the quotes).
fn strip_side(p: &str, side: &str) -> String {
    let p = unquote(p);
    p.strip_prefix(side).map(str::to_owned).unwrap_or(p)
}

/// Parse `git diff` output (unified, `-M`). Returns the files and whether it was cut short.
pub fn parse_diff(out: &str) -> (Vec<DiffFile>, bool) {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/x b/x` or `"a/x" "b/x"`; the exact paths come from ---/+++ or rename lines.
            let path = if rest.ends_with('"') {
                rest.rsplit_once(" \"b/").map_or(rest.to_owned(), |(_, b)| format!("\"b/{b}"))
            } else {
                rest.rsplit_once(" b/").map_or(rest.to_owned(), |(_, b)| format!("b/{b}"))
            };
            files.push(DiffFile {
                path: strip_side(&path, "b/"),
                old_path: None,
                status: "modified".into(),
                added: 0,
                removed: 0,
                binary: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if f.hunks.is_empty() {
            if line.starts_with("new file mode") {
                f.status = "added".into();
                continue;
            }
            if line.starts_with("deleted file mode") {
                f.status = "deleted".into();
                continue;
            }
            if let Some(p) = line.strip_prefix("rename from ") {
                f.old_path = Some(unquote(p));
                f.status = "renamed".into();
                continue;
            }
            if let Some(p) = line.strip_prefix("rename to ") {
                f.path = unquote(p);
                continue;
            }
            if line.starts_with("Binary files ") {
                f.binary = true;
                continue;
            }
            if let Some(p) = line.strip_prefix("+++ ") {
                if p != "/dev/null" {
                    f.path = strip_side(p, "b/");
                }
                continue;
            }
            if line.starts_with("--- ")
                || line.starts_with("index ")
                || line.starts_with("similarity ")
                || line.starts_with("old mode")
                || line.starts_with("new mode")
            {
                continue;
            }
        }
        if let Some(h) = line.strip_prefix("@@ ") {
            let mut parts = h.splitn(3, ' ');
            let (o, n) = (parts.next().unwrap_or("-0"), parts.next().unwrap_or("+0"));
            old_no = range_start(o);
            new_no = range_start(n);
            f.hunks.push(Hunk {
                header: line.to_owned(),
                old_start: old_no,
                new_start: new_no,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(h) = f.hunks.last_mut() else { continue };
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            Some('\\') => continue, // "\ No newline at end of file"
            None => (LineKind::Context, ""),
            _ => continue,
        };
        let (old, new) = match kind {
            LineKind::Added => {
                f.added += 1;
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                f.removed += 1;
                old_no += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
        };
        total += 1;
        if total > MAX_LINES {
            truncated = true;
            continue;
        }
        h.lines.push(DiffLine { kind, old, new, text: text.to_owned() });
    }
    (files, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unified_diffs() {
        let out = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,4 @@ fn main()\n line1\n-old\n+new\n+added\n line3\n\\ No newline at end of file\ndiff --git a/x.bin b/x.bin\nBinary files a/x.bin and b/x.bin differ\ndiff --git a/old.txt b/new.txt\nsimilarity index 90%\nrename from old.txt\nrename to new.txt\n@@ -5 +5 @@\n-a\n+b\ndiff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let (f, t) = parse_diff(out);
        assert!(!t);
        assert_eq!(f.len(), 4);
        assert_eq!((f[0].path.as_str(), f[0].added, f[0].removed), ("src/a.rs", 2, 1));
        let l = &f[0].hunks[0].lines;
        assert_eq!((l[1].kind, l[1].old, l[1].new), (LineKind::Removed, Some(2), None));
        assert_eq!((l[2].kind, l[2].new), (LineKind::Added, Some(2)));
        assert_eq!((l[4].old, l[4].new), (Some(3), Some(4)));
        assert!(f[1].binary);
        assert_eq!(
            (f[2].status.as_str(), f[2].old_path.as_deref(), f[2].path.as_str()),
            ("renamed", Some("old.txt"), "new.txt")
        );
        assert_eq!(f[2].hunks[0].lines[1].new, Some(5));
        assert_eq!((f[3].status.as_str(), f[3].path.as_str()), ("deleted", "gone"));
    }

    #[test]
    fn parses_quoted_paths() {
        let out = "diff --git \"a/caf\\303\\251 x.txt\" \"b/caf\\303\\251 x.txt\"\nnew file mode 100644\n--- /dev/null\n+++ \"b/caf\\303\\251 x.txt\"\n@@ -0,0 +1 @@\n+hi\ndiff --git \"a/tab\\there\" \"b/q\\\"uote\"\nsimilarity index 100%\nrename from \"tab\\there\"\nrename to \"q\\\"uote\"\n";
        let (f, _) = parse_diff(out);
        assert_eq!(f[0].path, "café x.txt");
        assert_eq!(f[1].path, "q\"uote");
        assert_eq!(f[1].old_path.as_deref(), Some("tab\there"));
        assert_eq!(unquote("\"a\\\\b\\nc\""), "a\\b\nc");
        assert_eq!(unquote("plain b"), "plain b");
    }
}
