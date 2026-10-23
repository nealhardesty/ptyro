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
    async fn archives(&self) -> Vec<ArchiveEntry> {
        let v = match &self.store {
            Some(store) => store.kv_get(ARCHIVES.into()).await.ok().flatten(),
            None => self.views.lock().expect("views").get(ARCHIVES).cloned(),
        };
        v.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
    }

    async fn save_archives(&self, list: &[ArchiveEntry]) {
        let v = serde_json::to_value(list).unwrap_or_default();
        match &self.store {
            Some(store) => {
                let _ = store.kv_set(ARCHIVES.into(), v).await;
            }
            None => {
                self.views.lock().expect("views").insert(ARCHIVES.into(), v);
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
            .map(|s| s.meta.lock().expect("meta").name.clone())
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
        self.progress(&wt, "archiving: a snapshot of its files");
        let (path, root, name, label) =
            (wt.path.clone(), p.root.clone(), wt.name.clone(), wt.qualified());
        let snapshot = blocking(move || -> Result<String, Error> {
            let git = Git::new();
            let snap =
                git.checkpoint(&path, &format!("ptyro archive of {label}")).map_err(git_err)?;
            // `refs/ptyro/archive/<name>`, or `-2`, `-3` when an earlier one has that name.
            let base = format!("refs/ptyro/archive/{name}");
            let refname = std::iter::once(base.clone())
                .chain((2..).map(|k| format!("{base}-{k}")))
                .find(|r| !git.ref_exists(&root, r))
                .expect("infinite");
            git.update_ref(&root, &refname, &snap.commit).map_err(git_err)?;
            Ok(refname)
        })
        .await??;
        // The snapshot holds everything now: removing can't lose work.
        let opts = RemoveOpts { force: true, stop_sessions, keep_branch: true };
        let removed = self.remove_worktree(&wt.id, opts).await?;
        let mut list = self.archives().await;
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
        self.save_archives(&list).await;
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
        let mut list = self.archives().await;
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
        blocking(move || Git::new().restore(&path, &snapshot)).await?.map_err(git_err)?;
        let _ = self.refresh_status(&info.id).await;
        list.remove(i);
        self.save_archives(&list).await;
        Ok(Reply::Worktree { info: self.find_worktree(&info.id)? })
    }

    pub(crate) async fn list_archives(&self, project: Option<&str>) -> Result<Reply, Error> {
        let entries = self
            .archives()
            .await
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
                let turns = a.meta.lock().expect("meta").turns;
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
                    let done = a.meta.lock().expect("meta").turns > turns
                        && a.status() != Some(AgentStatus::Working);
                    // Never started on it in a minute: it isn't going to; commit here instead.
                    let ignored = !worked && start.elapsed() > Duration::from_secs(60);
                    if done || ignored || !a.is_live() {
                        break;
                    }
                }
                if !self.dirty(&wt).await? {
                    notes.push(format!("{} committed its work", a.meta.lock().expect("meta").name));
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

    /// `[worktrees] archive_on_merge`: its PR merged and nothing runs in it, so archive it. The
    /// caller decides that it just merged (it saw the PR open before).
    pub(crate) fn archive_if_merged(self: &Arc<Self>, wt: &WorktreeInfo) {
        let merged = wt.pr.as_ref().is_some_and(|p| p.state == "MERGED");
        if !merged || wt.is_root || !self.config().worktrees.archive_on_merge {
            return;
        }
        if self.sessions_in(&wt.id).iter().any(|s| s.busy()) {
            return;
        }
        let (me, id, label) = (Arc::clone(self), wt.id.clone(), wt.qualified());
        tokio::spawn(async move {
            match me.archive_worktree(&id, false).await {
                Ok(_) => tracing::info!("archived {label}: its PR merged"),
                Err(e) => tracing::warn!("archive on merge, {label}: {}", e.message),
            }
        });
    }
}
