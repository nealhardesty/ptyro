//! Checkpoints (M7.11). Every agent turn that changed the worktree leaves
//! `refs/ptyro/ckpt/<session>/<n>`: HEAD plus everything uncommitted, written through a private
//! index (no visible commit, no stash, the user's index untouched). `ptyro checkpoint restore`
//! puts any of them back, checkpointing the present first.

use ptyro_core::sync::MutexExt;
use std::path::Path;
use std::sync::Arc;

use ptyro_core::model::{DiffDoc, WorktreeInfo};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{Checkpoint, Reply};

use crate::daemon::Daemon;
use crate::session::Session;
use crate::worktrees::blocking;

/// Checkpoints kept per session (older ones are deleted).
const KEEP_CHECKPOINTS: u32 = 200;

impl Daemon {
    /// After an agent turn: checkpoint the worktree if it changed since the last checkpoint.
    pub(crate) fn checkpoint_turn(self: &Arc<Self>, s: &Arc<Session>) {
        let (wt, name, turn) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (m.place.worktree.clone(), m.name.clone(), m.turns)
        };
        let Some(wt) = wt.and_then(|w| self.worktree_by_id(&w)) else { return };
        if !self.config().checkpoints() {
            return;
        }
        let s = Arc::clone(s);
        tokio::spawn(async move {
            let id = s.id.full();
            let path = wt.path.clone();
            let wid = wt.id.clone();
            let _ = blocking(move || -> Option<()> {
                let g = ptyro_git::Git::new();
                let prefix = format!("refs/ptyro/ckpt/{id}/");
                let existing = g.refs(&path, &prefix).ok()?;
                let numbered: Vec<(u32, String)> = existing.iter().filter_map(|(r, sha, _)| r.rsplit('/').next()?.parse().ok().map(|n| (n, sha.clone()))).collect();
                let last = numbered.iter().max_by_key(|(n, _)| *n).cloned();
                let n = last.as_ref().map_or(1, |(n, _)| n + 1);
                let msg = format!("ptyro checkpoint {n} of {name} (turn {turn})\n\nsession: {id}\nworktree: {wid}\nturn: {turn}\nat: {}\n", now_ms());
                let snap = g.checkpoint(&path, &msg).ok()?;
                if last.as_ref().and_then(|(_, sha)| g.tree_of(&path, sha)).as_deref() == Some(snap.tree.as_str()) {
                    return None; // nothing changed this turn
                }
                g.update_ref(&path, &format!("{prefix}{n}"), &snap.commit).ok()?;
                for (old, _) in numbered.iter().filter(|(k, _)| *k + KEEP_CHECKPOINTS <= n) {
                    let _ = g.delete_ref(&path, &format!("{prefix}{old}"));
                }
                Some(())
            })
            .await;
        });
    }

    fn checkpoints_of(
        &self,
        path: &Path,
        session: Option<&str>,
        worktree: Option<&str>,
    ) -> Vec<Checkpoint> {
        let g = ptyro_git::Git::new();
        let prefix = match session {
            Some(s) => format!("refs/ptyro/ckpt/{s}/"),
            None => "refs/ptyro/ckpt/".into(),
        };
        let mut out: Vec<Checkpoint> = g
            .refs(path, &prefix)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(r, sha, body)| {
                let field = |k: &str| {
                    body.lines().find_map(|l| l.strip_prefix(&format!("{k}: "))).map(str::to_owned)
                };
                if worktree.is_some_and(|w| field("worktree").as_deref() != Some(w)) {
                    return None;
                }
                let head = g.rev(path, &format!("{sha}^")).unwrap_or_default();
                let files = g
                    .changed(path, &format!("{sha}^"), Some(&sha))
                    .map(|v| v.len() as u32)
                    .unwrap_or(0);
                Some(Checkpoint {
                    n: r.rsplit('/').next()?.parse().ok()?,
                    session: field("session")?,
                    at_ms: field("at")?.parse().unwrap_or(0),
                    turn: field("turn").and_then(|t| t.parse().ok()).unwrap_or(0),
                    sha,
                    refname: r,
                    head,
                    files,
                })
            })
            .collect();
        out.sort_by_key(|c| (c.at_ms, c.n));
        out
    }

    /// The worktree and session (if one) an address names: a session, else a worktree.
    fn ckpt_target(&self, addr: &str) -> Result<(WorktreeInfo, Option<Arc<Session>>), Error> {
        if let Ok(s) = self.resolve(addr) {
            let wt = s.meta.locked().place.worktree.clone().and_then(|w| self.worktree_by_id(&w));
            return Ok((
                wt.ok_or_else(|| Error::invalid("that session isn't in a worktree"))?,
                Some(s),
            ));
        }
        Ok((self.find_worktree(addr)?, None))
    }

    pub(crate) async fn checkpoint_request(
        self: &Arc<Self>,
        req: ptyro_protocol::Request,
    ) -> Result<Reply, Error> {
        use ptyro_protocol::Request;
        match req {
            Request::CheckpointList { addr } => {
                let (wt, s) = self.ckpt_target(&addr)?;
                let (me, path, sid, wid) =
                    (Arc::clone(self), wt.path.clone(), s.map(|s| s.id.full()), wt.id.clone());
                let list = blocking(move || {
                    me.checkpoints_of(
                        &path,
                        sid.as_deref(),
                        if sid.is_none() { Some(&wid) } else { None },
                    )
                })
                .await?;
                Ok(Reply::Checkpoints { checkpoints: list })
            }
            Request::CheckpointDiff { addr, n, to } => {
                let (wt, s) = self.ckpt_target(&addr)?;
                let s = s.ok_or_else(|| {
                    Error::invalid("name the session whose checkpoints to compare")
                })?;
                let base = format!("refs/ptyro/ckpt/{}/{n}", s.id.full());
                let other = to.map(|t| format!("refs/ptyro/ckpt/{}/{t}", s.id.full()));
                let path = wt.path.clone();
                let doc = blocking(move || -> Result<DiffDoc, Error> {
                    let g = ptyro_git::Git::new();
                    let to = match other {
                        Some(o) => o,
                        None => {
                            g.checkpoint(&path, "ptyro diff")
                                .map_err(crate::registry::git_err)?
                                .commit
                        }
                    };
                    let text = g
                        .diff_between(&path, &base, &to, false, &[])
                        .map_err(crate::registry::git_err)?;
                    let (files, truncated) = ptyro_git::diff::parse_diff(&text);
                    Ok(DiffDoc { from: Some(base), files, truncated, against: () })
                })
                .await??;
                Ok(Reply::Diff { doc })
            }
            Request::CheckpointRestore { addr, n, turn } => {
                let (wt, s) = self.ckpt_target(&addr)?;
                let busy = self.sessions_in(&wt.id).into_iter().any(|x| {
                    x.is_live() && x.status() == Some(ptyro_core::status::AgentStatus::Working)
                });
                if busy {
                    return Err(Error::conflict(format!(
                        "an agent is working in {}; wait for it (or stop it) first",
                        wt.qualified()
                    )));
                }
                let (me, path) = (Arc::clone(self), wt.path.clone());
                let sid = s.as_ref().map(|s| s.id.full());
                let wid = wt.id.clone();
                let list =
                    blocking(move || me.checkpoints_of(&path, sid.as_deref(), Some(&wid))).await?;
                let pick = match (n, turn) {
                    (Some(n), _) => list.iter().rev().find(|c| c.n == n),
                    (None, Some(t)) => list.iter().rev().find(|c| c.turn == t),
                    (None, None) => list.last(),
                }
                .cloned()
                .ok_or_else(|| {
                    let which = match (n, turn) {
                        (Some(n), _) => format!("no checkpoint #{n}"),
                        (None, Some(t)) => format!("no checkpoint at turn {t}"),
                        (None, None) => "no checkpoint here yet".into(),
                    };
                    Error::new(ErrorCode::NotFound, which)
                })?;
                let (path, target) = (wt.path.clone(), pick.sha.clone());
                let session = pick.session.clone();
                let safety = blocking(move || -> Result<String, Error> {
                    let g = ptyro_git::Git::new();
                    // The present, first: a restore can always be undone.
                    let snap = g.checkpoint(&path, &format!("ptyro checkpoint before restore\n\nsession: {session}\nturn: 0\nat: {}\n", now_ms())).map_err(crate::registry::git_err)?;
                    let name = format!("refs/ptyro/ckpt/{session}/before-restore-{}", now_ms());
                    g.update_ref(&path, &name, &snap.commit).map_err(crate::registry::git_err)?;
                    g.restore(&path, &target).map_err(crate::registry::git_err)?;
                    Ok(name)
                })
                .await??;
                self.refresh_status(&wt.id).await;
                Ok(Reply::Notes {
                    notes: vec![
                        format!(
                            "restored {} to checkpoint {} (turn {})",
                            wt.qualified(),
                            pick.n,
                            pick.turn
                        ),
                        format!("the state before is kept as {safety}"),
                    ],
                })
            }
            other => Err(Error::invalid(format!("unsupported request {other:?}"))),
        }
    }
}
