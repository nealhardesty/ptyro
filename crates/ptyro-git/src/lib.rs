//! Worktree operations via the git CLI (SPEC §10): registration facts, `worktree add/lock/remove`,
//! `worktree list --porcelain`, repository status, and the removal safety checks of §10.8.
//!
//! Everything shells out to `git` (blocking): it is what users have, it honours their config and
//! hooks, and every porcelain used here is stable. Callers in async code use `spawn_blocking`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ptyro_core::model::RepoStatus;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git is not installed or not on PATH")]
    Missing,
    #[error("git {args}: {message}")]
    Failed { args: String, message: String },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub mod orch;
pub mod review;

pub type Result<T, E = GitError> = std::result::Result<T, E>;

/// A git runner: inherits the environment, or runs with exactly `env` (a profile's, so fetches
/// use that profile's SSH key, SPEC §10.10).
#[derive(Debug, Clone, Default)]
pub struct Git {
    env: Option<Vec<(OsString, OsString)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Without `refs/heads/`.
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: bool,
}

/// Where a new worktree's checkout comes from (SPEC §10.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A new branch from `base`.
    NewBranch { branch: String, base: String },
    /// An existing local branch (or `origin/<branch>`, tracked).
    Existing { branch: String },
    /// GitHub `pull/<n>/head`, fetched into `branch`.
    PullRequest { number: u32, branch: String, remote: String },
}

/// What would be lost by removing a worktree (SPEC §10.8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Safety {
    /// `git status` lines of changed tracked files.
    pub uncommitted: Vec<String>,
    /// Untracked files that aren't ignored.
    pub untracked: Vec<String>,
    /// Commits on this branch that are on no remote and not in the base.
    pub unpushed: Vec<String>,
    /// Stash entries made on this branch.
    pub stashes: Vec<String>,
}

impl Safety {
    pub fn is_clean(&self) -> bool {
        self.uncommitted.is_empty()
            && self.untracked.is_empty()
            && self.unpushed.is_empty()
            && self.stashes.is_empty()
    }

    /// A precise, human report: what's at risk, with the first few items of each kind.
    pub fn report(&self) -> String {
        let mut out = Vec::new();
        let mut section = |n: usize, what: &str, items: &[String]| {
            if n == 0 {
                return;
            }
            let mut s = format!("{n} {what}");
            for i in items.iter().take(5) {
                s.push_str(&format!("\n    {i}"));
            }
            if items.len() > 5 {
                s.push_str(&format!("\n    … and {} more", items.len() - 5));
            }
            out.push(s);
        };
        section(self.uncommitted.len(), "uncommitted change(s)", &self.uncommitted);
        section(self.untracked.len(), "untracked file(s)", &self.untracked);
        section(self.unpushed.len(), "commit(s) not pushed or merged", &self.unpushed);
        section(self.stashes.len(), "stash entr(y/ies)", &self.stashes);
        out.join("\n  ")
    }
}

/// What happens to a worktree's branch on removal (SPEC §10.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchFate {
    Delete(String),
    Keep(String),
}

fn lines(s: &str) -> impl Iterator<Item = &str> {
    s.lines().map(str::trim_end).filter(|l| !l.is_empty())
}

impl Git {
    pub fn new() -> Self {
        Self { env: None }
    }

    pub fn with_env(env: Vec<(OsString, OsString)>) -> Self {
        Self { env: Some(env) }
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir).args(args);
        if let Some(env) = &self.env {
            c.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        // Never block on a credential or host-key prompt: there is no terminal here.
        c.env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null());
        // Background status refreshes must never take `index.lock`: the user's (or an agent's)
        // own `git add`/`commit` running at the same moment would fail.
        c.env("GIT_OPTIONAL_LOCKS", "0");
        c
    }

    fn output(&self, dir: &Path, args: &[&str]) -> Result<Output> {
        self.command(dir, args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound { GitError::Missing } else { e.into() }
        })
    }

    /// Run git in `dir`; stdout on success, a `Failed` with stderr otherwise.
    pub fn run(&self, dir: &Path, args: &[&str]) -> Result<String> {
        let out = self.output(dir, args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let message = err
                .lines()
                .map(|l| l.trim_start_matches("fatal: ").trim_start_matches("error: "))
                .filter(|l| !l.is_empty() && !l.starts_with("hint:"))
                .collect::<Vec<_>>()
                .join("; ");
            Err(GitError::Failed { args: args.join(" "), message })
        }
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> bool {
        self.output(dir, args).is_ok_and(|o| o.status.success())
    }

    pub fn version(&self) -> Result<String> {
        Ok(self.run(Path::new("."), &["--version"])?.trim().to_owned())
    }

    /// The checkout containing `path`.
    pub fn toplevel(&self, path: &Path) -> Result<PathBuf> {
        let out = self.run(path, &["rev-parse", "--show-toplevel"]).map_err(|_| {
            GitError::Invalid(format!("{} is not inside a git repository", path.display()))
        })?;
        Ok(PathBuf::from(out.trim()))
    }

    /// The repository's main checkout, also when `path` is inside a linked worktree.
    pub fn main_root(&self, path: &Path) -> Result<PathBuf> {
        let top = self.toplevel(path)?;
        let common =
            self.run(&top, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let common = PathBuf::from(common.trim());
        if self.run(&top, &["rev-parse", "--is-bare-repository"])?.trim() == "true" {
            return Err(GitError::Invalid("bare repositories are not supported".into()));
        }
        match common.file_name() {
            Some(n) if n == ".git" => Ok(common.parent().map_or(top, Path::to_path_buf)),
            _ => Ok(top),
        }
    }

    /// The per-worktree admin directory (`.git` or `.git/worktrees/<name>`).
    pub fn git_dir(&self, path: &Path) -> Result<PathBuf> {
        let out = self.run(path, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
        Ok(PathBuf::from(out.trim()))
    }

    pub fn common_dir(&self, path: &Path) -> Result<PathBuf> {
        let out = self.run(path, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(out.trim()))
    }

    pub fn remotes(&self, root: &Path) -> Vec<Remote> {
        let Ok(out) = self.run(root, &["remote", "-v"]) else { return Vec::new() };
        let mut v: Vec<Remote> = Vec::new();
        for l in lines(&out) {
            let mut parts = l.split_whitespace();
            if let (Some(name), Some(url)) = (parts.next(), parts.next())
                && !v.iter().any(|r| r.name == name)
            {
                v.push(Remote { name: name.into(), url: url.into() });
            }
        }
        v
    }

    /// `origin`, else the first remote.
    pub fn main_remote(&self, root: &Path) -> Option<Remote> {
        let remotes = self.remotes(root);
        remotes.iter().find(|r| r.name == "origin").cloned().or_else(|| remotes.into_iter().next())
    }

    /// `origin/HEAD`'s branch, else `main` / `master` if they exist, else the current branch.
    pub fn default_branch(&self, root: &Path) -> Option<String> {
        if let Some(remote) = self.main_remote(root)
            && let Ok(out) = self.run(
                root,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    &format!("refs/remotes/{}/HEAD", remote.name),
                ],
            )
        {
            let full = out.trim();
            return Some(full.split_once('/').map_or(full, |(_, b)| b).to_owned());
        }
        for b in ["main", "master"] {
            if self.branch_exists(root, b) {
                return Some(b.into());
            }
        }
        self.current_branch(root)
    }

    pub fn current_branch(&self, path: &Path) -> Option<String> {
        let out = self.run(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
        Some(out.trim().to_owned()).filter(|b| !b.is_empty())
    }

    pub fn branch_exists(&self, root: &Path, branch: &str) -> bool {
        self.ok(root, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
    }

    pub fn ref_exists(&self, root: &Path, r: &str) -> bool {
        self.ok(
            root,
            &["rev-parse", "--verify", "--quiet", "--end-of-options", &format!("{r}^{{commit}}")],
        )
    }

    pub fn rev(&self, path: &Path, r: &str) -> Option<String> {
        let out = self
            .run(
                path,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("{r}^{{commit}}"),
                ],
            )
            .ok()?;
        Some(out.trim().to_owned())
    }

    /// The base a new worktree starts from: `remote/default` when that exists, else `default`.
    pub fn default_base(&self, root: &Path) -> Option<String> {
        let branch = self.default_branch(root)?;
        if let Some(r) = self.main_remote(root) {
            let remote_ref = format!("{}/{branch}", r.name);
            if self.ref_exists(root, &remote_ref) {
                return Some(remote_ref);
            }
        }
        Some(branch)
    }

    /// Validate a branch name the way git does.
    pub fn check_branch_name(&self, name: &str) -> Result<()> {
        if self.ok(Path::new("."), &["check-ref-format", "--branch", name])
            && !name.starts_with('-')
        {
            Ok(())
        } else {
            Err(GitError::Invalid(format!("{name:?} is not a valid branch name")))
        }
    }

    /// Fetch `what` (a branch or refspec) from `remote`. Failures are reported, not fatal, by
    /// callers: offline work must keep working.
    pub fn fetch(&self, root: &Path, remote: &str, what: &str) -> Result<()> {
        self.run(root, &["fetch", "--quiet", remote, what]).map(drop)
    }

    /// `git worktree add` (SPEC §10.3 step 2).
    pub fn worktree_add(&self, root: &Path, path: &Path, source: &Source) -> Result<()> {
        let p = path.to_string_lossy();
        match source {
            Source::NewBranch { branch, base } => {
                self.check_branch_name(branch)?;
                if self.branch_exists(root, branch) {
                    return Err(GitError::Invalid(format!(
                        "branch {branch} already exists (use it with --branch {branch} --existing)"
                    )));
                }
                // --no-track: the new branch must not pretend `origin/main` is its upstream.
                self.run(root, &["worktree", "add", "--no-track", "-b", branch, &p, base])?;
            }
            Source::Existing { branch } => {
                if self.branch_exists(root, branch) {
                    self.run(root, &["worktree", "add", &p, branch])?;
                } else if let Some(r) = self.main_remote(root)
                    && self.ref_exists(root, &format!("{}/{branch}", r.name))
                {
                    let upstream = format!("{}/{branch}", r.name);
                    self.run(root, &["worktree", "add", "--track", "-b", branch, &p, &upstream])?;
                } else {
                    return Err(GitError::Invalid(format!(
                        "no branch {branch} here or on the remote"
                    )));
                }
            }
            Source::PullRequest { number, branch, remote } => {
                self.check_branch_name(branch)?;
                self.fetch(root, remote, &format!("pull/{number}/head:{branch}"))?;
                self.run(root, &["worktree", "add", &p, branch])?;
            }
        }
        Ok(())
    }

    pub fn worktree_list(&self, root: &Path) -> Result<Vec<WorktreeEntry>> {
        let out = self.run(root, &["worktree", "list", "--porcelain"])?;
        Ok(parse_worktree_list(&out))
    }

    pub fn lock(&self, root: &Path, path: &Path, reason: &str) -> Result<()> {
        let p = path.to_string_lossy();
        match self.run(root, &["worktree", "lock", "--reason", reason, &p]) {
            Err(GitError::Failed { message, .. }) if message.contains("already locked") => Ok(()),
            other => other.map(drop),
        }
    }

    pub fn unlock(&self, root: &Path, path: &Path) -> Result<()> {
        let p = path.to_string_lossy();
        match self.run(root, &["worktree", "unlock", &p]) {
            Err(GitError::Failed { message, .. }) if message.contains("not locked") => Ok(()),
            other => other.map(drop),
        }
    }

    /// `git worktree remove` (after the safety checks; `force` drops local changes).
    pub fn worktree_remove(&self, root: &Path, path: &Path, force: bool) -> Result<()> {
        let p = path.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            // Twice: also removes a locked worktree.
            args.extend(["--force", "--force"]);
        }
        args.push(&p);
        self.run(root, &args).map(drop)?;
        let _ = self.run(root, &["worktree", "prune"]);
        Ok(())
    }

    /// Branch, upstream, counts and diffstat (SPEC §10.9). `base` is the project base.
    pub fn status(&self, path: &Path, base: Option<&str>) -> Result<RepoStatus> {
        let out =
            self.run(path, &["status", "--porcelain=v2", "--branch", "--untracked-files=normal"])?;
        let mut s = RepoStatus { at_ms: ptyro_core::now_ms(), ..RepoStatus::default() };
        for l in lines(&out) {
            if let Some(h) = l.strip_prefix("# ") {
                if let Some(b) = h.strip_prefix("branch.head ") {
                    s.branch = (b != "(detached)").then(|| b.to_owned());
                } else if let Some(u) = h.strip_prefix("branch.upstream ") {
                    s.upstream = Some(u.to_owned());
                } else if let Some(ab) = h.strip_prefix("branch.ab ") {
                    let mut it = ab.split_whitespace();
                    s.ahead =
                        it.next().and_then(|a| a.trim_start_matches('+').parse().ok()).unwrap_or(0);
                    s.behind =
                        it.next().and_then(|b| b.trim_start_matches('-').parse().ok()).unwrap_or(0);
                } else if let Some(oid) = h.strip_prefix("branch.oid ") {
                    s.head = (oid != "(initial)").then(|| oid.chars().take(9).collect());
                }
                continue;
            }
            match l.as_bytes().first() {
                Some(b'1' | b'2') => {
                    let xy = l.split_whitespace().nth(1).unwrap_or("..");
                    let (x, y) = (xy.as_bytes()[0], xy.as_bytes().get(1).copied().unwrap_or(b'.'));
                    if x != b'.' {
                        s.staged += 1;
                    }
                    if y != b'.' {
                        s.modified += 1;
                    }
                }
                Some(b'u') => s.conflicts += 1,
                Some(b'?') => s.untracked += 1,
                _ => {}
            }
        }
        if s.head.is_some() {
            s.subject =
                self.run(path, &["log", "-1", "--format=%s"]).ok().map(|o| o.trim().to_owned());
        }
        let merge_base = base
            .filter(|b| self.ref_exists(path, b))
            .and_then(|b| self.run(path, &["merge-base", "HEAD", b]).ok())
            .map(|m| m.trim().to_owned());
        if let (Some(b), Some(_)) = (base, &merge_base)
            && let Ok(out) =
                self.run(path, &["rev-list", "--left-right", "--count", &format!("HEAD...{b}")])
        {
            let mut it = out.split_whitespace();
            s.base_ahead = it.next().and_then(|a| a.parse().ok()).unwrap_or(0);
            s.base_behind = it.next().and_then(|a| a.parse().ok()).unwrap_or(0);
        }
        let against = merge_base.as_deref().unwrap_or("HEAD");
        if s.head.is_some()
            && let Ok(out) = self.run(path, &["diff", "--numstat", against])
        {
            for l in lines(&out) {
                let mut it = l.split('\t');
                s.insertions += it.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
                s.deletions += it.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
            }
        }
        Ok(s)
    }

    /// Everything removal would lose (SPEC §10.8 step 2).
    pub fn safety(&self, path: &Path, branch: Option<&str>, base: Option<&str>) -> Result<Safety> {
        let mut s = Safety::default();
        let out = self.run(path, &["status", "--porcelain=v1", "--untracked-files=all"])?;
        for l in lines(&out) {
            if let Some(f) = l.strip_prefix("?? ") {
                s.untracked.push(f.to_owned());
            } else {
                s.uncommitted.push(l.to_owned());
            }
        }
        // Commits only this worktree has: not on any remote and not in the base.
        let mut args = vec!["log", "--format=%h %s", "HEAD", "--not", "--remotes"];
        if let Some(b) = base.filter(|b| self.ref_exists(path, b)) {
            args.push(b);
        }
        if self.ref_exists(path, "HEAD") {
            let out = self.run(path, &args)?;
            s.unpushed = lines(&out).map(str::to_owned).collect();
        }
        if let Some(branch) = branch {
            let out = self.run(path, &["stash", "list", "--format=%gd %gs"]).unwrap_or_default();
            let on = [format!(" On {branch}: "), format!(" WIP on {branch}: ")];
            s.stashes = lines(&out)
                .filter(|l| on.iter().any(|o| l.contains(o.as_str())))
                .map(str::to_owned)
                .collect();
        }
        Ok(s)
    }

    /// Delete the branch only if nothing is lost: fully merged into `base`, or pushed and
    /// identical to its upstream (SPEC §10.8 step 5).
    pub fn branch_fate(&self, root: &Path, branch: &str, base: Option<&str>) -> BranchFate {
        let Some(tip) = self.rev(root, &format!("refs/heads/{branch}")) else {
            return BranchFate::Keep(format!("branch {branch} not found"));
        };
        if let Some(b) = base.filter(|b| self.ref_exists(root, b))
            && self.ok(root, &["merge-base", "--is-ancestor", &tip, b])
        {
            return BranchFate::Delete(format!("merged into {b}"));
        }
        if let Ok(up) =
            self.run(root, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")])
            && let Some(up_tip) = self.rev(root, up.trim())
        {
            if up_tip == tip {
                return BranchFate::Delete(format!("identical to {}", up.trim()));
            }
            let n = self
                .run(root, &["rev-list", "--count", &format!("{}..{tip}", up.trim())])
                .map(|o| o.trim().to_owned())
                .unwrap_or_default();
            return BranchFate::Keep(format!("{n} commit(s) not pushed to {}", up.trim()));
        }
        let mut args = vec!["rev-list", "--count", tip.as_str(), "--not", "--remotes"];
        if let Some(b) = base.filter(|b| self.ref_exists(root, b)) {
            args.push(b);
        }
        let n = self.run(root, &args).map(|o| o.trim().to_owned()).unwrap_or_default();
        BranchFate::Keep(format!("{n} unpushed commit(s), not merged"))
    }

    pub fn delete_branch(&self, root: &Path, branch: &str) -> Result<()> {
        self.run(root, &["branch", "-D", branch]).map(drop)
    }

    /// Whether `branch` has an upstream or exists on any remote (then renaming is refused).
    pub fn is_published(&self, root: &Path, branch: &str) -> bool {
        if self.ok(root, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")]) {
            return true;
        }
        self.remotes(root).iter().any(|r| self.ref_exists(root, &format!("{}/{branch}", r.name)))
    }

    pub fn rename_branch(&self, root: &Path, old: &str, new: &str) -> Result<()> {
        self.check_branch_name(new)?;
        self.run(root, &["branch", "-m", old, new]).map(drop)
    }

    /// Subjects of the commits on HEAD that `base` lacks, oldest first.
    pub fn subjects_since(&self, path: &Path, base: &str) -> Vec<String> {
        self.run(
            path,
            &[
                "log",
                "--reverse",
                "--format=%s",
                "--end-of-options",
                &format!("{base}..HEAD"),
                "--",
            ],
        )
        .map(|o| lines(&o).map(str::to_owned).collect())
        .unwrap_or_default()
    }

    /// Untracked files under `root` matching gitignore-style `patterns` (SPEC §10.4). With
    /// `ignored_only`, only those git also ignores (`.worktreeinclude` semantics). git does the
    /// matching itself, so the semantics are exactly gitignore's.
    pub fn untracked_matching(
        &self,
        root: &Path,
        patterns: &[String],
        ignored_only: bool,
    ) -> Result<Vec<PathBuf>> {
        if patterns.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<String> =
            vec!["ls-files".into(), "-z".into(), "--others".into(), "--ignored".into()];
        for p in patterns {
            args.push(format!("--exclude={p}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(root, &refs)?;
        let mut found: Vec<PathBuf> =
            out.split('\0').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        if ignored_only && !found.is_empty() {
            let mut c = self.command(root, &["check-ignore", "-z", "--stdin"]);
            c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
            let mut child = c.spawn()?;
            // Feed stdin from another thread: git answers as it reads, and with many paths a
            // full stdout pipe would block it while we still block writing.
            let mut stdin = child.stdin.take().expect("piped");
            let input: Vec<u8> = found
                .iter()
                .flat_map(|f| f.as_os_str().as_encoded_bytes().iter().copied().chain([0]))
                .collect();
            let writer = std::thread::spawn(move || {
                use std::io::Write;
                stdin.write_all(&input)
            });
            let out = child.wait_with_output()?;
            writer.join().map_err(|_| std::io::Error::other("check-ignore writer panicked"))??;
            let ignored: std::collections::HashSet<PathBuf> = String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            found.retain(|f| ignored.contains(f));
        }
        found.sort();
        Ok(found)
    }

    /// Add `line` to `.git/info/exclude` once (never to `.gitignore`).
    pub fn exclude(&self, root: &Path, line: &str) -> Result<()> {
        let info = self.common_dir(root)?.join("info");
        std::fs::create_dir_all(&info)?;
        let path = info.join("exclude");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l.trim() == line) {
            return Ok(());
        }
        let mut new = text;
        if !new.is_empty() && !new.ends_with('\n') {
            new.push('\n');
        }
        new.push_str(line);
        new.push('\n');
        std::fs::write(path, new)?;
        Ok(())
    }
}

pub fn parse_worktree_list(out: &str) -> Vec<WorktreeEntry> {
    let mut v = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;
    for l in out.lines() {
        if l.is_empty() {
            v.extend(cur.take());
            continue;
        }
        let (k, val) = l.split_once(' ').unwrap_or((l, ""));
        if k == "worktree" {
            v.extend(cur.take());
            cur = Some(WorktreeEntry { path: PathBuf::from(val), ..WorktreeEntry::default() });
            continue;
        }
        let Some(e) = cur.as_mut() else { continue };
        match k {
            "HEAD" => e.head = Some(val.to_owned()),
            "branch" => e.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_owned()),
            "bare" => e.bare = true,
            "detached" => e.detached = true,
            "locked" => e.locked = Some(val.to_owned()),
            "prunable" => e.prunable = true,
            _ => {}
        }
    }
    v.extend(cur);
    v
}

/// `git@github.com:acme/repo.git`, `https://github.com/acme/repo` → `github.com/acme/repo`, so
/// match rules written either way match either spelling (SPEC §10.10).
pub fn normalize_remote(url: &str) -> String {
    let mut u = url.trim();
    let mut is_url = false;
    for scheme in ["ssh://", "https://", "http://", "git://", "git+ssh://"] {
        if let Some(rest) = u.strip_prefix(scheme) {
            u = rest;
            is_url = true;
        }
    }
    let u = u.split_once('@').filter(|(user, _)| !user.contains('/')).map_or(u, |(_, h)| h);
    let u = match u.split_once(':') {
        // `host:path` (scp style: no port, `1password/cli` is a path) or `scheme://host:port/path`.
        Some((host, rest)) if !host.contains('/') => {
            let rest =
                if is_url { rest.trim_start_matches(|c: char| c.is_ascii_digit()) } else { rest };
            let rest = rest.trim_start_matches('/');
            format!("{host}/{rest}")
        }
        _ => u.to_owned(),
    };
    u.trim_end_matches('/').trim_end_matches(".git").to_ascii_lowercase()
}

/// Copy a tree with copy-on-write where the file system supports it (APFS clonefile, btrfs/xfs
/// reflink). With `full_fallback`, falls back to an ordinary copy; otherwise reports failure so
/// the caller can symlink instead.
pub fn clone_tree(src: &Path, dst: &Path, full_fallback: bool) -> std::io::Result<bool> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let try_cmd = |args: &[&str]| -> bool {
        Command::new("cp")
            .args(args)
            .arg(src)
            .arg(dst)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let cow = if cfg!(target_os = "macos") {
        try_cmd(&["-c", "-R", "-p"])
    } else {
        try_cmd(&["-a", "--reflink=always"])
    };
    if cow {
        return Ok(true);
    }
    let _ = std::fs::remove_dir_all(dst);
    if full_fallback && try_cmd(&["-R", "-p"]) {
        return Ok(false);
    }
    Err(std::io::Error::other(format!("cannot copy {}", src.display())))
}

#[cfg(test)]
mod tests;
