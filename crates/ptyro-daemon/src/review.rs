//! Review and ship (M6): a worktree's diff and commits, committing as its profile, line
//! comments batched into a prompt for its agent, integrating into the target branch, pair mode
//! in the main checkout, and the review queue with its `max_unreviewed` gate and digest.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{
    DiffAgainst, DiffDoc, ReviewComment, ReviewItem, WorktreeInfo, WorktreeState,
};
use ptyro_core::session::{SessionKind, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_git::Git;
use ptyro_protocol::{Reply, Request};

use crate::daemon::Daemon;
use crate::registry::git_err;
use crate::worktrees::blocking;

/// Pair mode in a project's main checkout (M6.7).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Pair {
    pub worktree: String,
    /// What the main checkout had: a branch, else a commit.
    pub orig: String,
    pub orig_is_branch: bool,
    /// The user's own uncommitted work in the main checkout, stashed (commit id).
    pub stash: Option<String>,
    /// The main checkout as ptyro left it after the last sync.
    pub synced: String,
    /// Untracked files ptyro copied in (removed again on unpair).
    pub copied: Vec<String>,
    /// Untracked files that were already there.
    pub preexisting: Vec<String>,
}

pub type Pairs = HashMap<String, Pair>;

/// Per worktree: when it was last reviewed (its agent's turn at the time).
pub type Reviewed = HashMap<String, u64>;

fn untracked(git: &Git, dir: &Path) -> Vec<String> {
    git.run(dir, &["ls-files", "--others", "--exclude-standard", "-z"])
        .map(|s| s.split('\0').filter(|x| !x.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The tracked changes in `dir` against `from`, as a binary patch.
fn patch(git: &Git, dir: &Path, from: &str) -> Result<String, Error> {
    git.run(dir, &["diff", "--binary", "--no-color", "--no-ext-diff", from, "--"]).map_err(git_err)
}

fn apply(git: &Git, dir: &Path, patch_text: &str) -> Result<(), Error> {
    if patch_text.trim().is_empty() {
        return Ok(());
    }
    // A worktree's changes can hold secrets: a 0600 file with a random name, removed on drop.
    let write = || -> std::io::Result<tempfile::NamedTempFile> {
        let mut f = tempfile::Builder::new().prefix("ptyro-pair-").suffix(".patch").tempfile()?;
        std::io::Write::write_all(&mut f, patch_text.as_bytes())?;
        Ok(f)
    };
    let tmp = write().map_err(|e| Error::internal(e.to_string()))?;
    let r =
        git.run(dir, &["apply", "--binary", "--whitespace=nowarn", &tmp.path().to_string_lossy()]);
    r.map(drop).map_err(|e| Error::conflict(format!("the changes don't apply cleanly ({e})")))
}

fn copy_files(from: &Path, to: &Path, files: &[String]) -> Result<(), Error> {
    for f in files {
        let (src, dst) = (from.join(f), to.join(f));
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::internal(e.to_string()))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| Error::internal(format!("{f}: {e}")))?;
    }
    Ok(())
}

impl Daemon {
    pub(crate) async fn handle_review(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::WorktreeDiff { addr, against } => {
                Ok(Reply::Diff { doc: self.worktree_diff(&addr, against).await? })
            }
            Request::WorktreeLog { addr } => {
                let wt = self.find_worktree(&addr)?;
                let from = self.diff_base(&wt).await?;
                let path = wt.path.clone();
                let commits =
                    blocking(move || Git::new().log(&path, &from)).await?.map_err(git_err)?;
                Ok(Reply::Commits { commits })
            }
            Request::WorktreeCommit { addr, message, paths, all } => {
                let wt = self.find_worktree(&addr)?;
                if message.trim().is_empty() {
                    return Err(Error::invalid("a commit needs a message"));
                }
                let env = self.tool_env(&wt).await?;
                let path = wt.path.clone();
                let sha = blocking(move || Git::with_env(env).commit(&path, &message, &paths, all))
                    .await?
                    .map_err(git_err)?;
                let _ = self.refresh_status(&wt.id).await;
                Ok(Reply::Committed { sha })
            }
            Request::WorktreeIntegrate { addr, into, cherry_pick, push } => {
                self.integrate(&addr, into, cherry_pick, push).await
            }
            Request::WorktreePair { addr, off, refresh } => self.pair(&addr, off, refresh).await,
            Request::PrGet { addr, refresh } => {
                let wt = self.find_worktree(&addr)?;
                let info = if refresh || wt.pr.is_none() {
                    self.refresh_pr(&wt).await?
                } else {
                    wt.pr.clone()
                };
                Ok(Reply::Pr { info })
            }
            Request::PrCreate { addr, draft, title, body, base } => {
                self.pr_create(&addr, draft, title, body, base).await
            }
            Request::PrFix { addr } => self.pr_fix(&addr).await,
            Request::PrAutomerge { addr, method, off } => {
                self.pr_automerge(&addr, method, off).await
            }
            Request::PrWatch { addr, off } => self.pr_watch(&addr, off),
            Request::PrReply { addr, comment, text } => self.pr_reply(&addr, comment, &text).await,
            Request::ReviewAdd { addr, path, line, code, text } => {
                let wt = self.find_worktree(&addr)?;
                if text.trim().is_empty() {
                    return Err(Error::invalid("an empty comment"));
                }
                let comments = {
                    let mut all = self.review_comments.lock().expect("review");
                    let list = all.entry(wt.id.clone()).or_default();
                    let id = list.iter().map(|c| c.id).max().unwrap_or(0) + 1;
                    list.push(ReviewComment { id, path, line, code, text, at_ms: now_ms() });
                    list.clone()
                };
                self.save_review_state();
                Ok(Reply::Comments { comments })
            }
            Request::ReviewList { addr } => {
                let wt = self.find_worktree(&addr)?;
                let comments = self
                    .review_comments
                    .lock()
                    .expect("review")
                    .get(&wt.id)
                    .cloned()
                    .unwrap_or_default();
                Ok(Reply::Comments { comments })
            }
            Request::ReviewRemove { addr, id } => {
                let wt = self.find_worktree(&addr)?;
                let comments = {
                    let mut all = self.review_comments.lock().expect("review");
                    let list = all.entry(wt.id.clone()).or_default();
                    match id {
                        Some(i) => list.retain(|c| c.id != i),
                        None => list.clear(),
                    }
                    list.clone()
                };
                self.save_review_state();
                Ok(Reply::Comments { comments })
            }
            Request::ReviewSend { addr, session, note } => {
                self.review_send(&addr, session, note).await
            }
            Request::ReviewQueue => Ok(Reply::ReviewQueue { items: self.review_queue() }),
            Request::ReviewDone { addr } => {
                let wt = self.find_worktree(&addr)?;
                self.mark_reviewed(&wt.id);
                Ok(Reply::Done)
            }
            other => Err(Error::invalid(format!("unsupported request {other:?}"))),
        }
    }

    /// The merge base with the worktree's base (`origin/main`), else the base itself.
    pub(crate) async fn diff_base(&self, wt: &WorktreeInfo) -> Result<String, Error> {
        let base = wt
            .base
            .clone()
            .or_else(|| self.project_by_id(&wt.project).and_then(|p| p.default_branch))
            .unwrap_or_else(|| "HEAD".into());
        let path = wt.path.clone();
        blocking(move || Git::new().merge_base(&path, &base, "HEAD").unwrap_or(base)).await
    }

    pub(crate) async fn worktree_diff(
        &self,
        addr: &str,
        against: DiffAgainst,
    ) -> Result<DiffDoc, Error> {
        let wt = self.find_worktree(addr)?;
        let path = wt.path.clone();
        let mut since_ms = None;
        let from = match against {
            DiffAgainst::Base => self.diff_base(&wt).await?,
            DiffAgainst::Head => "HEAD".into(),
            DiffAgainst::Upstream => {
                let p = path.clone();
                blocking(move || Git::new().upstream(&p)).await?.ok_or_else(|| {
                    Error::missing("upstream", format!("{} (push it first)", wt.qualified()))
                })?
            }
            DiffAgainst::LastTurn => {
                let (base, at) = self
                    .sessions_in(&wt.id)
                    .into_iter()
                    .filter(|s| s.kind() == SessionKind::Agent)
                    .filter_map(|s| {
                        let m = s.meta.lock().expect("meta");
                        m.turn_base.clone().map(|b| (b, m.turn_started_ms))
                    })
                    .max_by_key(|(_, at)| *at)
                    .ok_or_else(|| {
                        Error::missing(
                            "agent turn",
                            format!("in {} (no agent has worked there yet)", wt.qualified()),
                        )
                    })?;
                since_ms = Some(at);
                base
            }
        };
        let f = from.clone();
        let mut doc = blocking(move || Git::new().diff(&path, &f, true)).await?.map_err(git_err)?;
        doc.against = against;
        // A snapshot can't hold untracked files: keep the ones written during the turn.
        if let Some(since) = since_ms {
            doc.files.retain(|f| {
                f.status != "untracked"
                    || std::fs::metadata(wt.path.join(&f.path))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .is_some_and(|t| t.as_millis() as u64 + 1000 >= since)
            });
        }
        Ok(doc)
    }

    /// At the start of each agent turn: remember the worktree's state, for "diff against the
    /// last turn" (never touches the index, the stash list or any file).
    pub(crate) fn snapshot_turn(self: &Arc<Self>, s: &Arc<crate::session::Session>) {
        let Some(wt) = s
            .meta
            .lock()
            .expect("meta")
            .place
            .worktree
            .clone()
            .and_then(|w| self.worktree_by_id(&w))
        else {
            return;
        };
        let s = Arc::clone(s);
        tokio::spawn(async move {
            let at = now_ms();
            let path = wt.path.clone();
            if let Ok(Some(sha)) = blocking(move || Git::new().snapshot(&path)).await {
                let mut m = s.meta.lock().expect("meta");
                m.turn_base = Some(sha);
                m.turn_started_ms = at;
            }
        });
    }

    /// Batch the line comments into one prompt for the agent (M6.3).
    async fn review_send(
        self: &Arc<Self>,
        addr: &str,
        session: Option<String>,
        note: Option<String>,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let comments =
            self.review_comments.lock().expect("review").get(&wt.id).cloned().unwrap_or_default();
        if comments.is_empty() {
            return Err(Error::conflict(format!(
                "no review comments on {} to send",
                wt.qualified()
            )));
        }
        let s = match session {
            Some(a) => self.resolve(&a)?,
            None => self
                .owner(&wt)
                .ok_or_else(|| Error::missing("agent session", format!("in {}", wt.qualified())))?,
        };
        let mut text = format!("[via ptyro: review of your changes in {}]\n", wt.qualified());
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            text.push_str(&format!("{}\n", n.trim()));
        }
        text.push_str("Please address these comments:\n");
        for (i, c) in comments.iter().enumerate() {
            text.push_str(&format!("\n{}. {}:{}", i + 1, c.path, c.line));
            if let Some(code) = c.code.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                text.push_str(&format!("  `{code}`"));
            }
            text.push_str(&format!("\n   {}\n", c.text.trim().replace('\n', "\n   ")));
        }
        text.push_str("\nWhen you're done, say which comments you addressed and how.");
        // The comments stay until they've reached the agent (it might exit first).
        let (me, s2, wid, sent) = (Arc::clone(self), Arc::clone(&s), wt.id.clone(), comments.len());
        tokio::spawn(async move {
            if me.deliver_idle(&s2, &text).await {
                {
                    let mut all = me.review_comments.lock().expect("review");
                    if let Some(v) = all.get_mut(&wid) {
                        v.drain(..sent.min(v.len()));
                        if v.is_empty() {
                            all.remove(&wid);
                        }
                    }
                }
                me.mark_reviewed(&wid);
                me.save_review_state();
            }
        });
        Ok(Reply::Session { info: self.info(&s) })
    }

    pub(crate) fn mark_reviewed(&self, wt: &str) {
        self.reviewed.lock().expect("reviewed").insert(wt.to_owned(), now_ms());
        self.save_review_state();
    }

    pub(crate) fn save_review_state(&self) {
        let comments = serde_json::to_value(&*self.review_comments.lock().expect("review"))
            .unwrap_or_default();
        let reviewed =
            serde_json::to_value(&*self.reviewed.lock().expect("reviewed")).unwrap_or_default();
        let pairs = serde_json::to_value(&*self.pairs.lock().expect("pairs")).unwrap_or_default();
        self.save_kv("review_comments", comments);
        self.save_kv("reviewed", reviewed);
        self.save_kv("pairs", pairs);
    }

    pub(crate) async fn load_review_state(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get("review_comments".into()).await
            && let Ok(c) = serde_json::from_value(v)
        {
            *self.review_comments.lock().expect("review") = c;
        }
        if let Ok(Some(v)) = store.kv_get("reviewed".into()).await
            && let Ok(r) = serde_json::from_value(v)
        {
            *self.reviewed.lock().expect("reviewed") = r;
        }
        if let Ok(Some(v)) = store.kv_get("pairs".into()).await
            && let Ok(p) = serde_json::from_value(v)
        {
            *self.pairs.lock().expect("pairs") = p;
        }
    }

    /// Worktrees whose agent finished a turn you haven't reviewed, best first (M6.9).
    pub(crate) fn review_queue(&self) -> Vec<ReviewItem> {
        let now = now_ms();
        let reviewed = self.reviewed.lock().expect("reviewed").clone();
        let worktrees: Vec<WorktreeInfo> = self
            .registry
            .lock()
            .expect("registry")
            .worktrees
            .iter()
            .filter(|w| !w.is_root && w.state == WorktreeState::Ready)
            .cloned()
            .collect();
        let mut items = Vec::new();
        for wt in worktrees {
            let Some(st) = &wt.status else { continue };
            let lines = st.insertions + st.deletions;
            if lines == 0 && st.base_ahead == 0 && st.untracked == 0 {
                continue;
            }
            let agents: Vec<_> = self
                .sessions_in(&wt.id)
                .into_iter()
                .filter(|s| s.kind() == SessionKind::Agent)
                .collect();
            // The newest agent decides: done with its turn (idle, done, error, exited).
            let Some(s) = agents.iter().max_by_key(|s| s.id.created_ms()) else { continue };
            let (status, since) = {
                let e = s.engine.lock().expect("engine");
                (e.record.as_ref().map(|r| r.status), e.record.as_ref().map_or(0, |r| r.since_ms))
            };
            let finished_at = match (s.is_live(), status) {
                (true, Some(AgentStatus::Idle | AgentStatus::Done | AgentStatus::Errored)) => since,
                (false, _) => s.meta.lock().expect("meta").ended_ms.unwrap_or(since),
                _ => continue,
            };
            if s.meta.lock().expect("meta").turns == 0 && s.is_live() {
                continue;
            }
            if reviewed.get(&wt.id).is_some_and(|r| *r >= finished_at) {
                continue;
            }
            let mut score = 50;
            let mut reasons = Vec::new();
            match &wt.pr {
                Some(pr) => {
                    let (pass, fail, pending) = pr.tally();
                    if fail > 0 {
                        score -= 30;
                        reasons.push(format!("{fail} check(s) failing"));
                    } else if pending > 0 {
                        score -= 5;
                        reasons.push(format!("{pending} check(s) running"));
                    } else if pass > 0 {
                        score += 30;
                        reasons.push("checks pass".into());
                    }
                    reasons.push(format!("PR #{}", pr.number));
                }
                None => reasons.push("no PR yet".into()),
            }
            let files = st.modified + st.untracked + st.staged;
            reasons.push(format!(
                "+{} −{}{}",
                st.insertions,
                st.deletions,
                if files > 0 { format!(", {files} uncommitted file(s)") } else { String::new() }
            ));
            score += match lines {
                0..=50 => 20,
                51..=200 => 10,
                201..=1000 => 0,
                _ => -15,
            };
            if lines <= 50 {
                reasons.push("small diff".into());
            }
            if status == Some(AgentStatus::Errored) {
                score -= 20;
                reasons.push("the agent stopped with an error".into());
            }
            // Verified-done (M7.9): proven beats claimed.
            match s.meta.lock().expect("meta").verify.as_ref().map(|v| v.state) {
                Some(ptyro_core::session::VerifyState::Passed) => {
                    score += 25;
                    reasons.push("verified".into());
                }
                Some(ptyro_core::session::VerifyState::Failed) => {
                    score -= 25;
                    reasons.push("claims done; its check fails".into());
                }
                _ => {}
            }
            let waiting_ms = now.saturating_sub(finished_at);
            score += (waiting_ms / 600_000).min(20) as i32;
            let last_message = last_message(s);
            items.push(ReviewItem {
                worktree: wt.clone(),
                session: Some(s.id.full()),
                score,
                reasons,
                last_message,
                waiting_ms,
            });
        }
        items.sort_by(|a, b| b.score.cmp(&a.score).then(b.waiting_ms.cmp(&a.waiting_ms)));
        items
    }

    /// `review.max_unreviewed`: `Some(waiting)` when a new agent must wait.
    pub(crate) fn review_gate(&self) -> Option<u32> {
        let max = self.config().review.max_unreviewed;
        if max == 0 {
            return None;
        }
        let n = self.review_queue().len() as u32;
        (n >= max).then_some(n)
    }

    /// Hold an agent spawn until the queue has room (M6.9). Returns its position.
    pub(crate) fn queue_spawn(&self, spec: SpawnSpec) -> u32 {
        let mut q = self.spawn_queue.lock().expect("spawn_queue");
        q.push_back(spec);
        q.len() as u32
    }

    /// Every few seconds: start held spawns while there's room, and send the digest.
    pub fn start_review_ticks(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            me.load_review_state().await;
            let mut last_digest = 0u64;
            let mut digested: HashSet<String> = HashSet::new();
            loop {
                tokio::time::sleep(Duration::from_secs(3)).await;
                while me.review_gate().is_none() {
                    let Some(spec) = me.spawn_queue.lock().expect("spawn_queue").pop_front() else {
                        break;
                    };
                    if let Err(e) = me.spawn(spec).await {
                        tracing::warn!("held spawn: {}", e.message);
                    }
                }
                let every = ptyro_core::config::parse_duration(&me.config().review.digest)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                if every == 0 {
                    continue;
                }
                let items = me.review_queue();
                let fresh: Vec<&ReviewItem> =
                    items.iter().filter(|i| !digested.contains(&i.worktree.id)).collect();
                if !fresh.is_empty() && now_ms().saturating_sub(last_digest) >= every {
                    last_digest = now_ms();
                    me.digest(&items).await;
                    digested = items.iter().map(|i| i.worktree.id.clone()).collect();
                }
            }
        });
    }

    async fn digest(self: &Arc<Self>, items: &[ReviewItem]) {
        let Some(first) = items.first() else { return };
        let body = items
            .iter()
            .take(5)
            .map(|i| format!("{} ({})", i.worktree.qualified(), i.reasons.join(", ")))
            .collect::<Vec<_>>()
            .join("\n");
        let note = crate::notifier::Note {
            session: first.session.clone().unwrap_or_default(),
            session_name: "ptyro".into(),
            kind: AttentionKind::Done,
            title: format!("{} ready for review", items.len()),
            body,
            profile: ptyro_core::model::DEFAULT_PROFILE.into(),
            workspace: None,
            project: None,
            approve_url: None,
            deny_url: None,
        };
        let config = self.config();
        let quiet = config
            .notifications
            .quiet_hours
            .as_deref()
            .and_then(|q| ptyro_core::config::parse_quiet_hours(q).ok())
            .zip(ptyro_pty::local_time_hm())
            .is_some_and(|(q, (h, m))| {
                ptyro_core::config::in_quiet_hours(q, u16::from(h) * 60 + u16::from(m))
            });
        if !quiet {
            self.deliver(&note, false).await;
        }
    }

    // ── integrate (M6.7) ───────────────────────────────────────────────────

    async fn integrate(
        self: &Arc<Self>,
        addr: &str,
        into: Option<String>,
        cherry_pick: bool,
        push: bool,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid(
                "integrate a worktree into the main line, not the main checkout",
            ));
        }
        if self.sessions_in(&wt.id).iter().any(|s| s.status() == Some(AgentStatus::Working)) {
            return Err(Error::conflict(format!(
                "an agent is working in {}: wait for its turn to end",
                wt.qualified()
            )));
        }
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let target = into.or_else(|| p.default_branch.clone()).unwrap_or_else(|| "main".into());
        let branch =
            wt.branch.clone().ok_or_else(|| Error::invalid("the worktree has no branch"))?;
        if branch == target {
            return Err(Error::invalid(format!("{} is on {target} already", wt.qualified())));
        }
        let env = self.tool_env(&wt).await?;
        let (root, path) = (p.root.clone(), wt.path.clone());
        let notes = blocking(move || -> Result<Vec<String>, Error> {
            let git = Git::with_env(env);
            if git.dirty(&path) {
                return Err(Error::new(
                    ErrorCode::DirtyWorktree,
                    "commit (or stash) the worktree's changes first",
                ));
            }
            if !git.branch_exists(&root, &target) {
                return Err(Error::missing("branch", &target));
            }
            let mut notes = Vec::new();
            let at = git.checked_out_at(&root, &target);
            if cherry_pick {
                let dest = at.ok_or_else(|| {
                    Error::conflict(format!(
                        "cherry-pick needs {target} checked out somewhere (the main checkout, say)"
                    ))
                })?;
                if git.dirty(&dest) {
                    return Err(Error::new(
                        ErrorCode::DirtyWorktree,
                        format!("{} has uncommitted changes", dest.display()),
                    ));
                }
                let from = git
                    .merge_base(&path, &target, "HEAD")
                    .ok_or_else(|| Error::invalid("no common history"))?;
                let n = git.log(&path, &from).map_err(git_err)?.len();
                if n == 0 {
                    return Err(Error::conflict(format!("{branch} has nothing beyond {target}")));
                }
                git.cherry_pick(&dest, &format!("{from}..{branch}")).map_err(git_err)?;
                notes.push(format!("cherry-picked {n} commit(s) from {branch} onto {target}"));
            } else {
                git.rebase(&path, &target).map_err(git_err)?;
                notes.push(format!("rebased {branch} onto {target}"));
                let head = git.rev(&path, "HEAD").ok_or_else(|| Error::internal("no HEAD"))?;
                match at {
                    Some(dest) => {
                        git.merge_ff(&dest, &head).map_err(git_err)?;
                        notes.push(format!("fast-forwarded {target} in {}", dest.display()));
                    }
                    None => {
                        git.move_branch(&root, &target, &head).map_err(git_err)?;
                        notes.push(format!("fast-forwarded {target}"));
                    }
                }
            }
            if push {
                let remote =
                    git.main_remote(&root).map(|r| r.name).unwrap_or_else(|| "origin".into());
                git.run(&root, &["push", "--quiet", &remote, &target]).map_err(git_err)?;
                notes.push(format!("pushed {target} to {remote}"));
            }
            Ok(notes)
        })
        .await??;
        let _ = self.refresh_status(&wt.id).await;
        if let Some(r) = self.root_worktree(&p.id) {
            let _ = self.refresh_status(&r.id).await;
        }
        Ok(Reply::Notes { notes })
    }

    // ── pair mode (M6.7) ───────────────────────────────────────────────────

    async fn pair(self: &Arc<Self>, addr: &str, off: bool, refresh: bool) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let current = self.pairs.lock().expect("pairs").get(&p.id).cloned();
        if let Some(c) = &current
            && c.worktree != wt.id
        {
            let other = self
                .worktree_by_id(&c.worktree)
                .map_or_else(|| c.worktree.clone(), |w| w.qualified());
            return Err(Error::conflict(format!(
                "the main checkout is paired with {other}: unpair it first"
            )));
        }
        if wt.is_root {
            return Err(Error::invalid(
                "pair a worktree with the main checkout, not the main checkout itself",
            ));
        }
        let env = self.tool_env(&wt).await?;
        let (root, path, id) = (p.root.clone(), wt.path.clone(), wt.id.clone());
        let label = wt.qualified();
        let result = blocking(move || -> Result<(Option<Pair>, Vec<String>), Error> {
            let git = Git::with_env(env);
            match (current, off) {
                (None, true) => Err(Error::conflict("the main checkout isn't paired")),
                (None, false) => {
                    pair_on(&git, &root, &path, &id, &label).map(|(p, n)| (Some(p), n))
                }
                (Some(mut c), false) => {
                    let notes = carry_back(&git, &root, &path, &c)?;
                    let (synced, copied) = sync_into(&git, &root, &path, &c.preexisting)?;
                    c.synced = synced;
                    c.copied = copied;
                    let mut n = notes;
                    n.push(if refresh {
                        format!("main checkout re-synced with {label}")
                    } else {
                        format!("already paired with {label}; re-synced")
                    });
                    Ok((Some(c), n))
                }
                (Some(c), true) => pair_off(&git, &root, &path, &c).map(|n| (None, n)),
            }
        })
        .await??;
        {
            let mut pairs = self.pairs.lock().expect("pairs");
            match result.0 {
                Some(pair) => {
                    pairs.insert(p.id.clone(), pair);
                }
                None => {
                    pairs.remove(&p.id);
                }
            }
        }
        self.save_review_state();
        for w in [Some(wt.clone()), self.root_worktree(&p.id)].into_iter().flatten() {
            let _ = self.refresh_status(&w.id).await;
        }
        Ok(Reply::Notes { notes: result.1 })
    }
}

/// What the agent last said: its turn's reply when the adapter reports one, else the status
/// detail, else the screen's last meaningful line.
pub(crate) fn last_message(s: &crate::session::Session) -> Option<String> {
    let reply = s.meta.lock().expect("meta").last_reply.clone();
    reply
        .as_deref()
        .and_then(|r| r.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|l| l.chars().take(140).collect())
        .or_else(|| s.engine.lock().expect("engine").record.as_ref().and_then(|r| r.detail.clone()))
        .or_else(|| last_line(&s.read_screen(ptyro_protocol::ScreenFormat::Plain)))
}

pub(crate) fn last_line(screen: &str) -> Option<String> {
    screen
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !matches!(*l, ">" | "❯" | "›")
                && !l.starts_with("> ")
                && !l.chars().all(|c| "─━-═ ".contains(c))
        })
        .map(|l| l.chars().take(140).collect())
}

/// Put the worktree's state into the main checkout (detached), stashing what was there.
fn pair_on(
    git: &Git,
    root: &Path,
    wt: &Path,
    id: &str,
    label: &str,
) -> Result<(Pair, Vec<String>), Error> {
    let mut notes = Vec::new();
    let orig_branch = git.current_branch(root);
    let orig = orig_branch
        .clone()
        .or_else(|| git.rev(root, "HEAD"))
        .ok_or_else(|| Error::internal("the main checkout has no HEAD"))?;
    let stash = if git.dirty(root) {
        git.run(
            root,
            &[
                "stash",
                "push",
                "--include-untracked",
                "--quiet",
                "-m",
                &format!("ptyro pair: {label}"),
            ],
        )
        .map_err(git_err)?;
        let sha = git.rev(root, "stash@{0}").ok_or_else(|| Error::internal("stash vanished"))?;
        notes.push(format!(
            "stashed the main checkout's uncommitted work ({})",
            &sha[..12.min(sha.len())]
        ));
        Some(sha)
    } else {
        None
    };
    let preexisting = untracked(git, root);
    let synced = sync_into(git, root, wt, &preexisting);
    let (synced, copied) = match synced {
        Ok(x) => x,
        Err(e) => {
            // Put everything back as it was.
            let _ = git.run(root, &["checkout", "--force", "--quiet", &orig]);
            if stash.is_some() {
                let _ = git.run(root, &["stash", "pop", "--quiet"]);
            }
            return Err(e);
        }
    };
    notes.push(format!(
        "the main checkout ({}) now shows {label}; edits you make there go back to it on `ptyro wt unpair`",
        root.display()
    ));
    Ok((
        Pair {
            worktree: id.to_owned(),
            orig,
            orig_is_branch: orig_branch.is_some(),
            stash,
            synced,
            copied,
            preexisting,
        },
        notes,
    ))
}

/// Make the main checkout show the worktree: its commit (detached), its uncommitted changes and
/// its new files. Returns the synced snapshot and the files copied in.
fn sync_into(
    git: &Git,
    root: &Path,
    wt: &Path,
    preexisting: &[String],
) -> Result<(String, Vec<String>), Error> {
    let head = git.rev(wt, "HEAD").ok_or_else(|| Error::internal("the worktree has no HEAD"))?;
    // Back to a clean state first: drop what an earlier sync applied (never the user's own
    // files; carry_back moved those to the worktree already).
    git.run(root, &["checkout", "--force", "--quiet", "--detach", &head]).map_err(git_err)?;
    git.run(root, &["reset", "--hard", "--quiet", &head]).map_err(git_err)?;
    for f in untracked(git, root) {
        if !preexisting.contains(&f) {
            let _ = std::fs::remove_file(root.join(&f));
        }
    }
    apply(git, root, &patch(git, wt, "HEAD")?)?;
    let new: Vec<String> =
        untracked(git, wt).into_iter().filter(|f| !preexisting.contains(f)).collect();
    copy_files(wt, root, &new)?;
    let synced = git.snapshot(root).unwrap_or(head);
    Ok((synced, new))
}

/// Edits made in the main checkout since the last sync go to the worktree.
fn carry_back(git: &Git, root: &Path, wt: &Path, c: &Pair) -> Result<Vec<String>, Error> {
    let mut notes = Vec::new();
    let tracked = patch(git, root, &c.synced)?;
    // Files the user created in the main checkout during the pair.
    let created: Vec<String> = untracked(git, root)
        .into_iter()
        .filter(|f| !c.preexisting.contains(f) && !c.copied.contains(f))
        .collect();
    // Files ptyro copied in that the user changed since.
    let changed_copies: Vec<String> = c
        .copied
        .iter()
        .filter(|f| {
            let (a, b) = (std::fs::read(root.join(f)).ok(), std::fs::read(wt.join(f)).ok());
            a.is_some() && a != b
        })
        .cloned()
        .collect();
    if !tracked.trim().is_empty() {
        apply(git, wt, &tracked).map_err(|e| Error::conflict(format!("your edits in the main checkout don't apply to the worktree, so nothing changed: {}", e.message)))?;
        notes.push("carried your edits in the main checkout over to the worktree".into());
    }
    copy_files(root, wt, &created)?;
    copy_files(root, wt, &changed_copies)?;
    if !created.is_empty() {
        notes.push(format!("carried {} new file(s) over to the worktree", created.len()));
    }
    Ok(notes)
}

fn pair_off(git: &Git, root: &Path, wt: &Path, c: &Pair) -> Result<Vec<String>, Error> {
    let mut notes = carry_back(git, root, wt, c)?;
    let created: Vec<String> =
        untracked(git, root).into_iter().filter(|f| !c.preexisting.contains(f)).collect();
    git.run(root, &["reset", "--hard", "--quiet"]).map_err(git_err)?;
    for f in created {
        let _ = std::fs::remove_file(root.join(&f));
    }
    git.run(root, &["checkout", "--quiet", &c.orig]).map_err(git_err)?;
    notes.push(format!(
        "the main checkout is back on {}",
        if c.orig_is_branch { c.orig.clone() } else { c.orig[..12.min(c.orig.len())].to_owned() }
    ));
    if let Some(sha) = &c.stash {
        match git.run(root, &["stash", "apply", "--quiet", sha]) {
            Ok(_) => {
                let list = git.run(root, &["stash", "list", "--format=%H"]).unwrap_or_default();
                if let Some(i) = list.lines().position(|l| l.trim() == sha) {
                    let _ = git.run(root, &["stash", "drop", "--quiet", &format!("stash@{{{i}}}")]);
                }
                notes.push("restored the main checkout's uncommitted work".into());
            }
            Err(e) => notes.push(format!(
                "your stashed work didn't apply cleanly; it's still in `git stash list` ({e})"
            )),
        }
    }
    Ok(notes)
}
