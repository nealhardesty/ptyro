//! Git for orchestration (M7): checkpoints that never touch the user's index or create visible
//! commits, refs under `refs/ptyro/`, restore, conflict prediction with `merge-tree`, throwaway
//! worktrees for evidence, and patch application for pick-by-hunk.

use std::path::{Path, PathBuf};

use crate::{Git, GitError, Result};

/// The identity checkpoint commits carry (they are ptyro's, not the user's).
const IDENT: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "ptyro"),
    ("GIT_AUTHOR_EMAIL", "ptyro@localhost"),
    ("GIT_COMMITTER_NAME", "ptyro"),
    ("GIT_COMMITTER_EMAIL", "ptyro@localhost"),
];

/// Where [`Git::restore`] keeps the HEAD it moved away from (a per-worktree ref).
pub const BEFORE_RESTORE: &str = "refs/worktree/ptyro/before-restore";

/// A checkpoint: a commit of HEAD plus everything uncommitted (untracked files included,
/// ignored ones not), whose parent is HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub commit: String,
    pub tree: String,
    pub head: Option<String>,
}

impl Git {
    fn run_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String> {
        let mut c = self.command(dir, args);
        c.envs(env.iter().copied());
        let out = c.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { GitError::Missing } else { e.into() }
        })?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(GitError::Failed {
                args: args.join(" "),
                message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    /// Commit the working tree as it is, through a private index: the user's index, stash
    /// list, branches and files are untouched.
    pub fn checkpoint(&self, path: &Path, message: &str) -> Result<Snapshot> {
        let git_dir = self.git_dir(path)?;
        // One private index per call: checkpoints of one repo run concurrently (turn ends, run
        // refs, the radar), and a shared index file would make them clobber each other.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let index = git_dir.join(format!("ptyro-ckpt-index-{}-{n}", std::process::id()));
        let idx = index.to_string_lossy().into_owned();
        let env = [("GIT_INDEX_FILE", idx.as_str())];
        let head = self.rev(path, "HEAD");
        let result = (|| {
            // Start from a copy of the real index: its stat data lets `add -A` skip unchanged
            // files instead of rehashing the whole tree. `add -A` then makes it the work tree.
            let real = git_dir.join("index");
            if !(real.is_file() && std::fs::copy(&real, &index).is_ok()) && head.is_some() {
                self.run_with(path, &["read-tree", "HEAD"], &env)?;
            }
            self.run_with(path, &["add", "-A", "--", "."], &env)?;
            let tree = self.run_with(path, &["write-tree"], &env)?.trim().to_owned();
            let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
            if let Some(h) = &head {
                args.extend(["-p", h.as_str()]);
            }
            let mut e: Vec<(&str, &str)> = env.to_vec();
            e.extend(IDENT);
            let commit = self.run_with(path, &args, &e)?.trim().to_owned();
            Ok(Snapshot { commit, tree, head: head.clone() })
        })();
        let _ = std::fs::remove_file(&index);
        result
    }

    pub fn update_ref(&self, path: &Path, name: &str, sha: &str) -> Result<()> {
        self.run(path, &["update-ref", name, sha]).map(|_| ())
    }

    pub fn delete_ref(&self, path: &Path, name: &str) -> Result<()> {
        self.run(path, &["update-ref", "-d", name]).map(|_| ())
    }

    /// `(refname, commit, subject+body)` under `prefix`, in ref order.
    pub fn refs(&self, path: &Path, prefix: &str) -> Result<Vec<(String, String, String)>> {
        let out = self.run(
            path,
            &["for-each-ref", "--format=%(refname)%00%(objectname)%00%(contents)%00%00", prefix],
        )?;
        Ok(out
            .split("\0\0")
            .filter_map(|rec| {
                let rec = rec.trim_start_matches('\n');
                let mut p = rec.splitn(3, '\0');
                let (r, o, c) = (p.next()?, p.next()?, p.next().unwrap_or(""));
                (!r.is_empty()).then(|| (r.to_owned(), o.to_owned(), c.to_owned()))
            })
            .collect())
    }

    pub fn tree_of(&self, path: &Path, rev: &str) -> Option<String> {
        self.run(
            path,
            &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{rev}^{{tree}}")],
        )
        .ok()
        .map(|s| s.trim().to_owned())
    }

    /// Put the worktree back to `ckpt`: HEAD to the checkpoint's parent, files to its tree
    /// (files it doesn't have are deleted), the index to HEAD (changes stay uncommitted).
    /// Untracked files that the checkpoint doesn't have are removed; ignored files are left
    /// alone. When HEAD moves, the old HEAD is kept in [`BEFORE_RESTORE`] (per worktree), so
    /// commits made after the checkpoint stay reachable.
    pub fn restore(&self, path: &Path, ckpt: &str) -> Result<()> {
        let ckpt = self
            .rev(path, ckpt)
            .ok_or_else(|| GitError::Invalid(format!("no checkpoint {ckpt}")))?;
        let parent = self.rev(path, &format!("{ckpt}^"));
        let old = self.rev(path, "HEAD");
        if let Some(o) = old.as_deref().filter(|o| parent.as_deref() != Some(*o)) {
            self.update_ref(path, BEFORE_RESTORE, o)?;
        }
        match &parent {
            Some(p) => self.run(path, &["reset", "-q", "--hard", p])?,
            None => String::new(),
        };
        self.run(path, &["clean", "-q", "-fd"])?;
        // `--no-overlay`: files the checkpoint lacks go too (deleted stays deleted).
        self.run(path, &["checkout", "-q", "--no-overlay", &ckpt, "--", "."])?;
        if parent.is_some() {
            self.run(path, &["reset", "-q"])?;
        }
        Ok(())
    }

    /// Files `a` and `b` would conflict on if merged (`git merge-tree --write-tree`, git ≥ 2.38).
    pub fn conflicts(&self, path: &Path, a: &str, b: &str) -> Result<Vec<String>> {
        let out = self
            .output(path, &["merge-tree", "--write-tree", "--name-only", "--no-messages", a, b])?;
        match out.status.code() {
            Some(0) => Ok(Vec::new()),
            Some(1) => Ok(String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()),
            _ => Err(GitError::Failed {
                args: "merge-tree --write-tree".into(),
                message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            }),
        }
    }

    /// Paths that differ between two commits (or a commit and the work tree, `b = None`).
    pub fn changed(&self, path: &Path, a: &str, b: Option<&str>) -> Result<Vec<String>> {
        let mut args = vec!["diff", "--name-only", "--no-renames", "--end-of-options", a];
        args.extend(b);
        args.push("--");
        Ok(self.run(path, &args)?.lines().map(str::to_owned).filter(|l| !l.is_empty()).collect())
    }

    /// `git diff a b` (patch or `--stat`), limited to `paths` when given.
    pub fn diff_between(
        &self,
        path: &Path,
        a: &str,
        b: &str,
        stat: bool,
        paths: &[String],
    ) -> Result<String> {
        let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
        // `--binary` makes a patch even with `--stat`: only for patches (they may be applied).
        args.push(if stat { "--stat" } else { "--binary" });
        args.extend(["--end-of-options", a, b, "--"]);
        args.extend(paths.iter().map(String::as_str));
        self.run(path, &args)
    }

    /// Apply a patch to the work tree (`--3way` falls back to a merge when context moved).
    pub fn apply(&self, path: &Path, patch: &str) -> Result<()> {
        use std::io::Write;
        let mut c = self.command(path, &["apply", "--3way", "--whitespace=nowarn", "-"]);
        c.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = c.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(patch.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(GitError::Failed {
                args: "apply".into(),
                message: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    /// A detached throwaway worktree at `rev` (evidence runs); remove it with
    /// [`drop_scratch`](Self::drop_scratch).
    pub fn scratch(&self, root: &Path, dir: &Path, rev: &str) -> Result<PathBuf> {
        if dir.exists() {
            let _ = self.drop_scratch(root, dir);
        }
        self.run(root, &["worktree", "add", "-q", "--detach", &dir.to_string_lossy(), rev])?;
        Ok(dir.to_path_buf())
    }

    pub fn drop_scratch(&self, root: &Path, dir: &Path) -> Result<()> {
        let r = self.run(root, &["worktree", "remove", "--force", &dir.to_string_lossy()]);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
            let _ = self.run(root, &["worktree", "prune"]);
        }
        r.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "user.email", "t@x"]);
        std::fs::write(p.join("a.txt"), "1\n2\n3\n").unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-q", "-m", "init"]);
        d
    }

    #[test]
    fn checkpoints_capture_everything_and_restore_it() {
        let d = repo();
        let p = d.path();
        let g = Git::new();
        std::fs::write(p.join("a.txt"), "1\n2\nchanged\n").unwrap();
        std::fs::write(p.join("new.txt"), "untracked\n").unwrap();
        git(p, &["add", "a.txt"]); // something staged, to prove the index survives
        let staged_before = g.run(p, &["diff", "--cached", "--name-only"]).unwrap();
        let s = g.checkpoint(p, "ptyro checkpoint\n\nturn: 1").unwrap();
        assert_eq!(g.run(p, &["diff", "--cached", "--name-only"]).unwrap(), staged_before);
        assert!(g.run(p, &["stash", "list"]).unwrap().is_empty());
        g.update_ref(p, "refs/ptyro/ckpt/s1/1", &s.commit).unwrap();
        let refs = g.refs(p, "refs/ptyro/ckpt/s1/").unwrap();
        assert_eq!(refs.len(), 1);
        assert!(refs[0].2.contains("turn: 1"));
        // Wreck the tree, then restore.
        std::fs::write(p.join("a.txt"), "gone\n").unwrap();
        std::fs::remove_file(p.join("new.txt")).unwrap();
        std::fs::write(p.join("junk.txt"), "x").unwrap();
        g.restore(p, &s.commit).unwrap();
        assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "1\n2\nchanged\n");
        assert_eq!(std::fs::read_to_string(p.join("new.txt")).unwrap(), "untracked\n");
        assert!(!p.join("junk.txt").exists());
        assert_eq!(g.rev(p, "HEAD"), s.head, "HEAD is where it was");
        assert_eq!(g.tree_of(p, &s.commit), Some(s.tree));
    }

    #[test]
    fn restore_keeps_deletions_and_saves_the_moved_head() {
        let d = repo();
        let p = d.path();
        let g = Git::new();
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        git(p, &["add", "b.txt"]);
        git(p, &["commit", "-q", "-m", "b"]);
        let base = g.rev(p, "HEAD").unwrap();
        std::fs::remove_file(p.join("a.txt")).unwrap();
        let s = g.checkpoint(p, "ckpt").unwrap();
        // Later: a.txt comes back, and a commit lands after the checkpoint.
        git(p, &["checkout", "-q", "--", "a.txt"]);
        std::fs::write(p.join("c.txt"), "c\n").unwrap();
        git(p, &["add", "c.txt"]);
        git(p, &["commit", "-q", "-m", "after"]);
        let after = g.rev(p, "HEAD").unwrap();
        g.restore(p, &s.commit).unwrap();
        assert!(!p.join("a.txt").exists(), "deleted in the checkpoint stays deleted");
        assert!(!p.join("c.txt").exists());
        assert_eq!(g.rev(p, "HEAD").as_deref(), Some(base.as_str()));
        assert_eq!(g.rev(p, BEFORE_RESTORE), Some(after));
        // The work tree is exactly the checkpoint's tree, and the index is HEAD's.
        assert_eq!(g.checkpoint(p, "again").unwrap().tree, s.tree);
        assert!(g.run(p, &["diff", "--cached", "--name-only"]).unwrap().is_empty());
    }

    #[test]
    fn checkpoint_starts_from_the_real_index_but_captures_the_work_tree() {
        let d = repo();
        let p = d.path();
        let g = Git::new();
        std::fs::write(p.join("a.txt"), "staged\n").unwrap();
        git(p, &["add", "a.txt"]);
        std::fs::write(p.join("a.txt"), "work tree\n").unwrap();
        let staged = g.run(p, &["diff", "--cached", "--name-status"]).unwrap();
        let s = g.checkpoint(p, "ckpt").unwrap();
        let blob = g.run(p, &["show", &format!("{}:a.txt", s.commit)]).unwrap();
        assert_eq!(blob, "work tree\n");
        assert_eq!(g.run(p, &["diff", "--cached", "--name-status"]).unwrap(), staged);
    }

    #[test]
    fn merge_tree_predicts_conflicts() {
        let d = repo();
        let p = d.path();
        let g = Git::new();
        git(p, &["checkout", "-q", "-b", "x"]);
        std::fs::write(p.join("a.txt"), "1\nX\n3\n").unwrap();
        git(p, &["commit", "-qam", "x"]);
        git(p, &["checkout", "-q", "-b", "y", "main"]);
        std::fs::write(p.join("a.txt"), "1\nY\n3\n").unwrap();
        git(p, &["commit", "-qam", "y"]);
        match g.conflicts(p, "x", "y") {
            Ok(c) => assert_eq!(c, vec!["a.txt"]),
            Err(e) => eprintln!("merge-tree unavailable (old git?): {e}"),
        }
        assert_eq!(g.changed(p, "main", Some("x")).unwrap(), vec!["a.txt"]);
        let patch = g.diff_between(p, "main", "x", false, &[]).unwrap();
        git(p, &["checkout", "-q", "main"]);
        g.apply(p, &patch).unwrap();
        assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "1\nX\n3\n");
        let scratch = p.join("../scratch-wt");
        let w = g.scratch(p, &scratch, "y").unwrap();
        assert_eq!(std::fs::read_to_string(w.join("a.txt")).unwrap(), "1\nY\n3\n");
        g.drop_scratch(p, &w).unwrap();
        assert!(!scratch.exists());
    }
}
