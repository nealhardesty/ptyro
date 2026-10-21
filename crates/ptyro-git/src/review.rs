//! What review and shipping need from git (M6): diffs against the base, the upstream or a
//! snapshot, commits since the base, committing, pushing, and the integrate primitives (rebase,
//! fast-forward, cherry-pick). Everything shells out to git, like the rest of this crate.

use std::path::Path;

use ptyro_core::model::{CommitInfo, DiffDoc, DiffFile, DiffLine, Hunk, LineKind};

use crate::{Git, GitError, Result};

/// Diffs stop growing here (lines); the rest is reported as truncated.
pub const MAX_LINES: usize = 20_000;
/// Untracked files bigger than this are listed without their content.
const MAX_UNTRACKED: u64 = 256 * 1024;

impl Git {
    pub fn merge_base(&self, path: &Path, a: &str, b: &str) -> Option<String> {
        self.run(path, &["merge-base", "--end-of-options", a, b])
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn upstream(&self, path: &Path) -> Option<String> {
        self.run(path, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"])
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// A commit of the working tree as it is right now, without touching the index, the stash
    /// list or any file (`git stash create`); `HEAD` when nothing is uncommitted.
    pub fn snapshot(&self, path: &Path) -> Option<String> {
        match self.run(path, &["stash", "create"]) {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            _ => self.rev(path, "HEAD"),
        }
    }

    /// The working tree (and, with `untracked`, new files git doesn't track yet) against `from`.
    pub fn diff(&self, path: &Path, from: &str, untracked: bool) -> Result<DiffDoc> {
        let out = self.run(
            path,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "-M",
                "--unified=3",
                "--end-of-options",
                from,
                "--",
            ],
        )?;
        let (mut files, mut truncated) = parse_diff(&out);
        if untracked {
            let list = self.run(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
            let mut lines: usize =
                files.iter().map(|f| f.hunks.iter().map(|h| h.lines.len()).sum::<usize>()).sum();
            for rel in list.split('\0').filter(|s| !s.is_empty()) {
                let f = untracked_file(&path.join(rel), rel, &mut lines);
                truncated |= f.hunks.is_empty() && !f.binary && f.added > 0;
                files.push(f);
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(DiffDoc { files, truncated, from: Some(from.to_owned()), ..DiffDoc::default() })
    }

    /// Commits in `from..HEAD`, newest first, with their line counts.
    pub fn log(&self, path: &Path, from: &str) -> Result<Vec<CommitInfo>> {
        let range = format!("{from}..HEAD");
        let out = self.run(
            path,
            &[
                "log",
                "--no-color",
                "--numstat",
                "--format=%x1e%H%x1f%s%x1f%an%x1f%ct",
                "--end-of-options",
                &range,
                "--",
            ],
        )?;
        Ok(parse_log(&out))
    }

    /// Stage everything (`all`) or `paths`, then commit with `message`. Returns the new commit.
    pub fn commit(
        &self,
        path: &Path,
        message: &str,
        paths: &[String],
        all: bool,
    ) -> Result<String> {
        if all {
            self.run(path, &["add", "--all"])?;
        } else if !paths.is_empty() {
            let mut args = vec!["add", "--"];
            args.extend(paths.iter().map(String::as_str));
            self.run(path, &args)?;
        }
        if self.ok(path, &["diff", "--cached", "--quiet"]) {
            return Err(GitError::Invalid("nothing to commit (stage files, or use --all)".into()));
        }
        self.run(path, &["commit", "--quiet", "-m", message])?;
        self.rev(path, "HEAD").ok_or_else(|| GitError::Invalid("no HEAD after committing".into()))
    }

    /// Push `branch` to `remote`, setting the upstream.
    pub fn push(&self, path: &Path, remote: &str, branch: &str) -> Result<()> {
        self.run(path, &["push", "--quiet", "--set-upstream", remote, branch]).map(drop)
    }

    pub fn is_ancestor(&self, path: &Path, a: &str, b: &str) -> bool {
        self.ok(path, &["merge-base", "--is-ancestor", "--end-of-options", a, b])
    }

    /// The rebase, cherry-pick, revert or merge already paused in the checkout at `path`.
    fn in_progress(&self, path: &Path) -> Result<Option<&'static str>> {
        let dir = self.git_dir(path)?;
        Ok([
            ("rebase-merge", "a rebase"),
            ("rebase-apply", "a rebase"),
            ("CHERRY_PICK_HEAD", "a cherry-pick"),
            ("REVERT_HEAD", "a revert"),
            ("MERGE_HEAD", "a merge"),
        ]
        .into_iter()
        .find(|(f, _)| dir.join(f).exists())
        .map(|(_, what)| what))
    }

    /// Refuse to start when something is already paused there: it's the user's, and our
    /// abort-on-failure must never undo it.
    fn refuse_if_in_progress(&self, path: &Path) -> Result<()> {
        match self.in_progress(path)? {
            Some(what) => Err(GitError::Invalid(format!(
                "{what} is already in progress in {}; finish or abort it first",
                path.display()
            ))),
            None => Ok(()),
        }
    }

    /// Rebase the checkout at `path` onto `onto`; aborts and reports on a conflict.
    pub fn rebase(&self, path: &Path, onto: &str) -> Result<()> {
        self.refuse_if_in_progress(path)?;
        match self.run(path, &["rebase", "--quiet", "--end-of-options", onto]) {
            Ok(_) => Ok(()),
            Err(e) => {
                if self.in_progress(path)?.is_some() {
                    let _ = self.run(path, &["rebase", "--abort"]);
                }
                Err(GitError::Invalid(format!(
                    "rebasing onto {onto} conflicts, so nothing changed ({e})"
                )))
            }
        }
    }

    /// Fast-forward the branch checked out at `path` to `to`.
    pub fn merge_ff(&self, path: &Path, to: &str) -> Result<()> {
        self.run(path, &["merge", "--ff-only", "--quiet", "--end-of-options", to]).map(drop)
    }

    /// Move a branch that isn't checked out anywhere to `to` (only ever forward).
    pub fn move_branch(&self, root: &Path, branch: &str, to: &str) -> Result<()> {
        if !self.is_ancestor(root, branch, to) {
            return Err(GitError::Invalid(format!(
                "{branch} isn't behind {to}: not a fast-forward"
            )));
        }
        self.run(root, &["branch", "--force", branch, to]).map(drop)
    }

    /// Cherry-pick `range` onto the branch checked out at `path`; aborts on a conflict.
    pub fn cherry_pick(&self, path: &Path, range: &str) -> Result<()> {
        self.refuse_if_in_progress(path)?;
        match self.run(path, &["cherry-pick", "--end-of-options", range]) {
            Ok(_) => Ok(()),
            Err(e) => {
                if self.in_progress(path)?.is_some() {
                    let _ = self.run(path, &["cherry-pick", "--abort"]);
                }
                Err(GitError::Invalid(format!(
                    "cherry-picking {range} conflicts, so nothing changed ({e})"
                )))
            }
        }
    }

    /// Whether the checkout has uncommitted or untracked changes.
    pub fn dirty(&self, path: &Path) -> bool {
        self.run(path, &["status", "--porcelain=v1", "--untracked-files=normal"])
            .is_ok_and(|s| !s.trim().is_empty())
    }

    /// The worktree (path) that has `branch` checked out, if any.
    pub fn checked_out_at(&self, root: &Path, branch: &str) -> Option<std::path::PathBuf> {
        self.worktree_list(root)
            .ok()?
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(branch))
            .map(|w| w.path)
    }
}

fn untracked_file(abs: &Path, rel: &str, lines: &mut usize) -> DiffFile {
    let mut f = DiffFile {
        path: rel.to_owned(),
        old_path: None,
        status: "untracked".into(),
        added: 0,
        removed: 0,
        binary: false,
        hunks: Vec::new(),
    };
    // Never follow a link: its content could be anything on the machine (~/.ssh/id_ed25519).
    // Like git, show what it points to instead.
    let Ok(meta) = std::fs::symlink_metadata(abs) else { return f };
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(abs).map(|t| t.to_string_lossy().into_owned());
        f.added = 1;
        *lines += 1;
        f.hunks.push(Hunk {
            header: "@@ -0,0 +1 @@".into(),
            old_start: 0,
            new_start: 1,
            lines: vec![DiffLine {
                kind: LineKind::Added,
                old: None,
                new: Some(1),
                text: target.unwrap_or_default(),
            }],
        });
        return f;
    }
    if !meta.is_file() {
        return f;
    }
    let bytes = if meta.len() <= MAX_UNTRACKED {
        std::fs::read(abs).unwrap_or_default()
    } else {
        Vec::new()
    };
    if bytes.contains(&0) {
        f.binary = true;
        return f;
    }
    let text = String::from_utf8_lossy(&bytes);
    let body: Vec<&str> = text.lines().collect();
    f.added = if meta.len() > MAX_UNTRACKED { 1 } else { body.len() as u32 };
    if meta.len() > MAX_UNTRACKED || *lines + body.len() > MAX_LINES {
        return f;
    }
    *lines += body.len();
    f.hunks.push(Hunk {
        header: format!("@@ -0,0 +1,{} @@", body.len()),
        old_start: 0,
        new_start: 1,
        lines: body
            .iter()
            .enumerate()
            .map(|(i, l)| DiffLine {
                kind: LineKind::Added,
                old: None,
                new: Some(i as u32 + 1),
                text: (*l).to_owned(),
            })
            .collect(),
    });
    f
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

pub fn parse_log(out: &str) -> Vec<CommitInfo> {
    let mut v = Vec::new();
    for rec in out.split('\u{1e}').filter(|r| !r.trim().is_empty()) {
        let mut lines = rec.lines();
        let head = lines.next().unwrap_or("");
        let f: Vec<&str> = head.split('\u{1f}').collect();
        if f.len() < 4 {
            continue;
        }
        let (mut added, mut removed, mut files) = (0, 0, 0);
        for l in lines.filter(|l| !l.trim().is_empty()) {
            let mut p = l.split('\t');
            added += p.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
            removed += p.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
            files += 1;
        }
        v.push(CommitInfo {
            sha: f[0].to_owned(),
            subject: f[1].to_owned(),
            author: f[2].to_owned(),
            at_ms: f[3].trim().parse::<u64>().unwrap_or(0) * 1000,
            added,
            removed,
            files,
        });
    }
    v
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

    #[cfg(unix)]
    #[test]
    fn untracked_symlinks_show_their_target_not_its_content() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("secret"), "PRIVATE KEY\n").unwrap();
        std::os::unix::fs::symlink(d.path().join("secret"), d.path().join("link")).unwrap();
        let mut n = 0;
        let f = untracked_file(&d.path().join("link"), "link", &mut n);
        let text: Vec<&str> = f.hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, vec![d.path().join("secret").to_string_lossy()]);
        assert_eq!((f.added, n), (1, 1));
    }

    #[test]
    fn refuses_to_rebase_over_an_operation_in_progress() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        let g = Git::new();
        let id = ["-c", "user.name=T", "-c", "user.email=t@x"];
        let git = |a: &[&str]| g.run(p, &[&id[..], a].concat()).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1\n").unwrap();
        git(&["add", "a"]);
        git(&["commit", "-q", "-m", "1"]);
        git(&["checkout", "-q", "-b", "x"]);
        std::fs::write(p.join("a"), "x\n").unwrap();
        git(&["commit", "-qam", "x"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("a"), "main\n").unwrap();
        git(&["commit", "-qam", "main"]);
        // The user's own cherry-pick stops on a conflict.
        assert!(g.run(p, &[&id[..], &["cherry-pick", "x"]].concat()).is_err());
        let gd = g.git_dir(p).unwrap();
        assert!(gd.join("CHERRY_PICK_HEAD").exists());
        let e = g.rebase(p, "x").unwrap_err().to_string();
        assert!(e.contains("already in progress"), "{e}");
        assert!(g.cherry_pick(p, "x").is_err());
        assert!(gd.join("CHERRY_PICK_HEAD").exists(), "the user's cherry-pick survives");
    }

    #[test]
    fn parses_logs() {
        let out = "\u{1e}abc\u{1f}Fix it\u{1f}Me\u{1f}1700000000\n\n3\t1\tsrc/a.rs\n-\t-\tx.bin\n\u{1e}def\u{1f}Init\u{1f}Me\u{1f}1690000000\n";
        let c = parse_log(out);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].subject.as_str(), c[0].added, c[0].removed, c[0].files),
            ("Fix it", 3, 1, 2)
        );
        assert_eq!(c[1].at_ms, 1_690_000_000_000);
    }
}
