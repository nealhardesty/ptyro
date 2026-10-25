//! Finishing and cleaning up a worktree (ROADMAP UX1.12, SPEC §10.8):
//! - **finish**: commit what's left (its agent asked to, else with its title as the message),
//!   then open a PR or integrate into the main branch;
//! - **archive**: a snapshot of its files (committed or not, untracked included) under
//!   `refs/ptyro/archive/<name>`, its branch kept, the worktree removed. It loses nothing, so it
//!   never refuses the way `wt rm` must;
//! - **restore**: the branch checked out again and the snapshot put back as uncommitted work;
//! - **archive on merge** (`[worktrees] archive_on_merge`): when its PR merges and nothing runs
//!   in it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ptyro_core::model::{ArchiveEntry, RemoveOpts, WorktreeInfo, WorktreeSpec};
use ptyro_core::session::SessionKind;
use ptyro_core::status::AgentStatus;
use ptyro_core::{Error, now_ms};
use ptyro_git::Git;
use ptyro_protocol::{DomainEvent, Reply, Request};

use crate::daemon::Daemon;
use crate::registry::git_err;
use crate::worktrees::blocking;

/// The kv key of the archived worktrees.
const ARCHIVES: &str = "archive:list";
/// How long `finish` waits for an agent to commit before committing what's left itself.
const COMMIT_WAIT: Duration = Duration::from_secs(600);

impl Daemon {
    /// The archived worktrees. A list that can't be read is an error, not an empty list: the
    /// next write would replace every earlier entry with the new one.
    async fn archives(&self) -> Result<Vec<ArchiveEntry>, Error> {
        let v = match &self.store {
            Some(store) => store
                .kv_get(ARCHIVES.into())
                .await
                .map_err(|e| Error::internal(format!("reading the archive list: {e}")))?,
            None => self.views.lock().expect("views").get(ARCHIVES).cloned(),
        };
        match v {
            None => Ok(Vec::new()),
            Some(v) => serde_json::from_value(v)
                .map_err(|e| Error::internal(format!("the archive list isn't readable: {e}"))),
        }
    }

    async fn save_archives(&self, list: &[ArchiveEntry]) -> Result<(), Error> {
        let v = serde_json::to_value(list).unwrap_or_default();
        match &self.store {
            Some(store) => store
                .kv_set(ARCHIVES.into(), v)
                .await
                .map_err(|e| Error::internal(format!("saving the archive list: {e}"))),
            None => {
                self.views.lock().expect("views").insert(ARCHIVES.into(), v);
                Ok(())
            }
        }
    }

    fn progress(&self, wt: &WorktreeInfo, step: &str) {
        let _ = self
            .events
            .send(DomainEvent::WorktreeProgress { worktree: wt.qualified(), step: step.into() });
    }

    /// Archive a worktree: snapshot, keep the branch, remove it (see the module docs).
    pub(crate) async fn archive_worktree(
        self: &Arc<Self>,
        addr: &str,
        stop_sessions: bool,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid(format!(
                "{} is the project's main checkout; only worktrees are archived",
                wt.qualified()
            )));
        }
        // One at a time per worktree: two at once would both snapshot it and both list it.
        if !self.archiving.lock().expect("archiving").insert(wt.id.clone()) {
            return Err(Error::conflict(format!("{} is being archived already", wt.qualified())));
        }
        struct Done<'a>(&'a Daemon, String);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.archiving.lock().expect("archiving").remove(&self.1);
            }
        }
        let _done = Done(self, wt.id.clone());
        let branch = wt.branch.clone().ok_or_else(|| {
            Error::invalid(format!(
                "{} has no branch (a detached checkout): remove it",
                wt.qualified()
            ))
        })?;
        let live: Vec<String> = self
            .sessions_in(&wt.id)
            .iter()
            .filter(|s| s.busy())
            .map(|s| s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone())
            .collect();
        if !live.is_empty() && !stop_sessions {
            return Err(Error::conflict(format!(
                "{} has running sessions ({}); stop them first, or archive with --stop-sessions",
                wt.qualified(),
                live.join(", ")
            )));
        }
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        // Its agents stop first: whatever they write in the next seconds belongs in the
        // snapshot, not in a directory that is about to be deleted.
        if stop_sessions {
            let running: Vec<_> =
                self.sessions_in(&wt.id).into_iter().filter(|s| s.is_live()).collect();
            futures::future::join_all(running.iter().map(|s| s.kill(None))).await;
        }
        self.progress(&wt, "archiving: a snapshot of its files");
        let (path, root, name, label) =
            (wt.path.clone(), p.root.clone(), wt.name.clone(), wt.qualified());
        let extras_root = self.paths.data.join("archive-extras");
        let snapshot = blocking(move || -> Result<String, Error> {
            let git = Git::new();
            // The snapshot is what git would commit: files it ignores (an `.env`, a local
            // database) aren't in it, and removing the worktree deletes them. The small ones
            // are kept beside it; a big one stops the archive rather than being lost.
            let ignored = ignored_files(&git, &path)?;
            let snap =
                git.checkpoint(&path, &format!("ptyro archive of {label}")).map_err(git_err)?;
            // `refs/ptyro/archive/<name>`, or `-2`, `-3` when an earlier one has that name.
            let base = format!("refs/ptyro/archive/{name}");
            let refname = std::iter::once(base.clone())
                .chain((2..).map(|k| format!("{base}-{k}")))
                .find(|r| !git.ref_exists(&root, r))
                .expect("infinite");
            if !ignored.is_empty() {
                keep_ignored(&path, &ignored, &extras_dir(&extras_root, &refname))?;
            }
            git.update_ref(&root, &refname, &snap.commit).map_err(git_err)?;
            Ok(refname)
        })
        .await??;
        // The list is read before anything is deleted (a list that can't be read stops the
        // archive, rather than being replaced by one entry later) and held while the worktree
        // goes, so two archives at once can't each write back a list without the other's entry.
        let _one = self.archive_list.lock().await;
        let mut list = self.archives().await?;
        let opts = RemoveOpts { force: true, stop_sessions, keep_branch: true };
        let removed = self.remove_worktree(&wt.id, opts).await?;
        list.insert(
            0,
            ArchiveEntry {
                project: p.id.clone(),
                project_slug: p.slug.clone(),
                name: wt.name.clone(),
                branch: branch.clone(),
                snapshot: snapshot.clone(),
                path: wt.path.clone(),
                title: wt.title.clone(),
                archived_ms: now_ms(),
            },
        );
        self.save_archives(&list).await.map_err(|e| {
            Error::internal(format!(
                "{} was removed, but ptyro couldn't write it into the archive list ({}): its snapshot is kept as {snapshot}, and the branch {branch} is too",
                wt.qualified(),
                e.message
            ))
        })?;
        let mut notes = vec![format!(
            "archived {}: branch {branch} and a snapshot of its files ({snapshot}) are kept; `ptyro wt restore {}` brings it back",
            wt.qualified(),
            wt.qualified()
        )];
        if let Reply::Removed { notes: n } = removed {
            notes.extend(n.into_iter().filter(|n| !n.starts_with("discarded")));
        }
        Ok(Reply::Notes { notes })
    }

    /// Bring an archived worktree back: its branch checked out where it was, and its files as
    /// they were (uncommitted work uncommitted again).
    pub(crate) async fn restore_worktree(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        let _one = self.archive_list.lock().await;
        let mut list = self.archives().await?;
        let (slug, name) = match addr.split_once('/') {
            Some((p, n)) => (Some(p), n),
            None => (None, addr),
        };
        let matches: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == name && slug.is_none_or(|s| s == e.project_slug))
            .map(|(i, _)| i)
            .collect();
        let i = match matches.as_slice() {
            [] => return Err(Error::missing("archived worktree", addr)),
            [i] => *i,
            // Newest first: the latest of several with that name.
            [i, ..] if slug.is_some() => *i,
            _ => {
                return Err(Error::new(
                    ptyro_core::ErrorCode::Ambiguous,
                    format!(
                        "several archived worktrees are named {name}: say which project (`myrepo/{name}`)"
                    ),
                ));
            }
        };
        let e = list[i].clone();
        // The branch must still be where the snapshot was taken: restoring puts the files back
        // over its tip, and a branch that has moved on (an integrate, a commit from another
        // checkout) would otherwise be reset backwards.
        if let Some(p) = self.project_by_id(&e.project) {
            let (root, branch, snap) = (p.root.clone(), e.branch.clone(), e.snapshot.clone());
            let moved = blocking(move || {
                let g = Git::new();
                match (g.rev(&root, &branch), g.rev(&root, &format!("{snap}^"))) {
                    (Some(now), Some(then)) if now != then => Some((now, then)),
                    _ => None,
                }
            })
            .await?;
            if let Some((now, then)) = moved {
                return Err(Error::conflict(format!(
                    "{} has moved since it was archived (now {}, archived at {}), and restoring would reset it backwards. The snapshot is kept as {}: check it out with `git checkout {} -- .` in a worktree of the branch.",
                    e.branch,
                    &now[..now.len().min(8)],
                    &then[..then.len().min(8)],
                    e.snapshot,
                    e.snapshot,
                )));
            }
        }
        let spec = WorktreeSpec {
            project: e.project.clone(),
            name: Some(e.name.clone()),
            branch: Some(e.branch.clone()),
            existing: true,
            setup: false,
            fetch: Some(false),
            title: e.title.clone(),
            path: Some(e.path.clone()).filter(|p| !p.exists()),
            ..WorktreeSpec::default()
        };
        let info = match self.create_worktree(spec).await? {
            Reply::WorktreeCreated { info, .. } => info,
            other => return Ok(other),
        };
        let (path, snapshot) = (info.path.clone(), e.snapshot.clone());
        let extras = extras_dir(&self.paths.data.join("archive-extras"), &snapshot);
        blocking(move || -> Result<(), Error> {
            Git::new().restore_onto_tip(&path, &snapshot).map_err(git_err)?;
            // The ignored files kept with it, where they were (never over a file that is there).
            restore_ignored(&extras, &path);
            Ok(())
        })
        .await??;
        let _ = self.refresh_status(&info.id).await;
        list.remove(i);
        self.save_archives(&list).await?;
        Ok(Reply::Worktree { info: self.find_worktree(&info.id)? })
    }

    pub(crate) async fn list_archives(&self, project: Option<&str>) -> Result<Reply, Error> {
        let entries = self
            .archives()
            .await?
            .into_iter()
            .filter(|e| project.is_none_or(|p| p == e.project || p == e.project_slug))
            .collect();
        Ok(Reply::Archives { entries })
    }

    /// Finish: commit what's left, then a PR or integrating (see the module docs).
    pub(crate) async fn finish_worktree(
        self: &Arc<Self>,
        addr: &str,
        integrate: bool,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid("finish a worktree, not the main checkout"));
        }
        let mut notes = Vec::new();
        if self.dirty(&wt).await? {
            // Its agent knows what it did: ask it to commit, and wait for that turn.
            let agent = self
                .sessions_in(&wt.id)
                .into_iter()
                .filter(|s| s.kind() == SessionKind::Agent && s.is_live())
                .max_by_key(|s| s.id.created_ms());
            if let Some(a) = agent.filter(|a| a.status() != Some(AgentStatus::Working)) {
                self.progress(&wt, "asking its agent to commit");
                let turns = a.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turns;
                self.send_when_ready(
                    &a,
                    "[via ptyro] Please commit all your work now: stage what belongs together, and write a clear commit message (subject line under 72 characters, then why). Don't push.",
                    true,
                    120_000,
                )
                .await?;
                let (start, deadline) = (Instant::now(), Instant::now() + COMMIT_WAIT);
                let mut worked = false;
                while Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    worked |= a.status() == Some(AgentStatus::Working);
                    let done =
                        a.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turns
                            > turns
                            && a.status() != Some(AgentStatus::Working);
                    // Never started on it in a minute: it isn't going to; commit here instead.
                    let ignored = !worked && start.elapsed() > Duration::from_secs(60);
                    if done || ignored || !a.is_live() {
                        break;
                    }
                }
                if !self.dirty(&wt).await? {
                    notes.push(format!(
                        "{} committed its work",
                        a.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name
                    ));
                }
            }
            if self.dirty(&wt).await? {
                // Whatever is left: committed as the worktree's profile, its title the message.
                let message = wt.title.clone().unwrap_or_else(|| format!("Work on {}", wt.name));
                let reply = self
                    .handle_review(Request::WorktreeCommit {
                        addr: wt.id.clone(),
                        message: message.clone(),
                        paths: Vec::new(),
                        all: true,
                    })
                    .await?;
                if let Reply::Committed { sha } = reply {
                    notes.push(format!("committed {} \"{message}\"", &sha[..10.min(sha.len())]));
                }
            }
        }
        let reply = if integrate {
            self.progress(&wt, "integrating into the main branch");
            self.handle_review(Request::WorktreeIntegrate {
                addr: wt.id.clone(),
                into: None,
                cherry_pick: false,
                push: false,
            })
            .await?
        } else {
            self.progress(&wt, "pushing and opening a PR");
            let draft = self.config().github.draft;
            self.pr_create(&wt.id, draft, wt.title.clone(), None, None).await?
        };
        match reply {
            Reply::Notes { notes: n } => notes.extend(n),
            Reply::Pr { info: Some(pr) } => notes.push(format!("PR #{}: {}", pr.number, pr.url)),
            _ => {}
        }
        Ok(Reply::Notes { notes })
    }

    /// Uncommitted or untracked changes in `wt`.
    async fn dirty(&self, wt: &WorktreeInfo) -> Result<bool, Error> {
        let (path, base) = (wt.path.clone(), wt.base.clone());
        let s =
            blocking(move || Git::new().status(&path, base.as_deref())).await?.map_err(git_err)?;
        Ok(s.dirty() > 0)
    }

    /// `[worktrees] archive_on_merge`: its work shipped (its PR merged, or it was integrated
    /// into the main branch) and nothing runs in it, so archive it. The caller decides that it
    /// just shipped.
    pub(crate) fn archive_if_merged(self: &Arc<Self>, wt: &WorktreeInfo) {
        if !wt.shipped() || !self.config().worktrees.archive_on_merge {
            return;
        }
        if self.sessions_in(&wt.id).iter().any(|s| s.busy()) {
            return;
        }
        let (me, id, label) = (Arc::clone(self), wt.id.clone(), wt.qualified());
        tokio::spawn(async move {
            match me.archive_worktree(&id, false).await {
                Ok(_) => tracing::info!("archived {label}: its work shipped"),
                Err(e) => tracing::warn!("archive on merge, {label}: {}", e.message),
            }
        });
    }
}

/// Directories whose ignored contents are build output, caches and dependencies: regenerated,
/// and too big to keep.
const REGENERABLE: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".cache",
    ".next",
    ".nuxt",
    ".gradle",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".parcel-cache",
    ".turbo",
    "coverage",
    ".idea",
    ".vscode",
    ".DS_Store",
];
/// The most one ignored file may weigh to be kept with an archive, and all of them together.
const KEEP_FILE: u64 = 1 << 20;
const KEEP_TOTAL: u64 = 16 << 20;

/// Where the ignored files of the archive `snapshot` (`refs/ptyro/archive/x`) are kept.
fn extras_dir(root: &std::path::Path, snapshot: &str) -> std::path::PathBuf {
    root.join(snapshot.trim_start_matches("refs/").replace('/', "_"))
}

/// The files git ignores in `path` that are worth keeping: not build output or caches, and
/// not links. Too big to keep stops the archive, naming them.
fn ignored_files(git: &Git, path: &std::path::Path) -> Result<Vec<std::path::PathBuf>, Error> {
    let out = git
        .run(path, &["ls-files", "--others", "--ignored", "--exclude-standard", "-z"])
        .map_err(git_err)?;
    let (mut keep, mut total, mut big) = (Vec::new(), 0u64, Vec::new());
    for rel in out.split('\0').filter(|r| !r.is_empty()) {
        let rel = std::path::PathBuf::from(rel);
        if rel.components().any(|c| REGENERABLE.iter().any(|n| c.as_os_str() == *n)) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(path.join(&rel)) else { continue };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > KEEP_FILE || total + meta.len() > KEEP_TOTAL {
            big.push(format!("{} ({} KiB)", rel.display(), meta.len() / 1024));
        } else {
            total += meta.len();
            keep.push(rel);
        }
    }
    if big.is_empty() {
        return Ok(keep);
    }
    big.truncate(6);
    Err(Error::conflict(format!(
        "it has ignored files git doesn't keep that are too big to keep with an archive, and removing the worktree would delete them: {}. Move them out or delete them, then archive again.",
        big.join(", ")
    )))
}

/// Copy the ignored files `rel` of `path` into `to`, keeping their paths.
fn keep_ignored(
    path: &std::path::Path,
    rel: &[std::path::PathBuf],
    to: &std::path::Path,
) -> Result<(), Error> {
    for r in rel {
        let dest = to.join(r);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::internal(format!("{}: {e}", parent.display())))?;
        }
        std::fs::copy(path.join(r), &dest)
            .map_err(|e| Error::internal(format!("keeping {}: {e}", r.display())))?;
    }
    Ok(())
}

/// Put kept ignored files back under `path`, skipping any that exist, then forget the copies.
fn restore_ignored(from: &std::path::Path, path: &std::path::Path) {
    fn walk(dir: &std::path::Path, base: &std::path::Path, path: &std::path::Path) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, path);
            } else if let Ok(rel) = p.strip_prefix(base) {
                let dest = path.join(rel);
                if std::fs::symlink_metadata(&dest).is_err() {
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::copy(&p, &dest);
                }
            }
        }
    }
    walk(from, from, path);
    let _ = std::fs::remove_dir_all(from);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dir: &std::path::Path) -> Git {
        let g = Git::new();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@e"],
            vec!["config", "user.name", "t"],
        ] {
            g.run(dir, &args).unwrap();
        }
        std::fs::write(dir.join(".gitignore"), ".env*\nnode_modules/\n*.bin\n").unwrap();
        g.run(dir, &["add", ".gitignore"]).unwrap();
        g.run(dir, &["commit", "-q", "-m", "init"]).unwrap();
        g
    }

    /// Archiving deletes the checkout, and the snapshot has only what git would commit: the
    /// small ignored files (an `.env`) are kept beside it and come back; build output isn't
    /// worth keeping; one too big to keep stops the archive instead of being deleted.
    #[test]
    fn archiving_keeps_small_ignored_files_and_refuses_to_lose_big_ones() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir_all(wt.join("node_modules/x")).unwrap();
        let g = repo(&wt);
        std::fs::write(wt.join(".env.local"), "TOKEN=1").unwrap();
        std::fs::write(wt.join("node_modules/x/index.js"), "big tree").unwrap();
        let mut ignored = ignored_files(&g, &wt).unwrap();
        ignored.sort();
        assert_eq!(ignored, [std::path::PathBuf::from(".env.local")], "build output isn't kept");
        let extras = d.path().join("extras").join("a_b");
        keep_ignored(&wt, &ignored, &extras).unwrap();
        // The worktree goes; a new checkout gets the file back, but never over one that is there.
        let back = d.path().join("back");
        std::fs::create_dir_all(&back).unwrap();
        std::fs::write(back.join(".env.local"), "NEWER").unwrap();
        restore_ignored(&extras, &back);
        assert_eq!(std::fs::read_to_string(back.join(".env.local")).unwrap(), "NEWER");
        assert!(!extras.exists(), "the copies are forgotten once restored");
        let fresh = d.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        keep_ignored(&wt, &ignored, &extras).unwrap();
        restore_ignored(&extras, &fresh);
        assert_eq!(std::fs::read_to_string(fresh.join(".env.local")).unwrap(), "TOKEN=1");
        // A big ignored file (a local database) stops the archive, naming it.
        std::fs::write(wt.join("local.bin"), vec![0u8; 2 << 20]).unwrap();
        let err = ignored_files(&g, &wt).unwrap_err();
        assert!(
            err.message.contains("local.bin") && err.message.contains("too big"),
            "{}",
            err.message
        );
        assert_eq!(
            extras_dir(std::path::Path::new("/x"), "refs/ptyro/archive/feat-2"),
            std::path::PathBuf::from("/x/ptyro_archive_feat-2")
        );
    }
}
