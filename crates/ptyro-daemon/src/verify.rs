//! Verified-done (M7.9) and checkpoints (M7.11).
//!
//! **Verified-done.** A session with a check counts a turn as done only once the check passes.
//! Claude is bounced from its own Stop hook (`{"decision":"block","reason":…}`): the turn never
//! ends, so nothing downstream sees a false "done". Other agents get the failures as a follow-up
//! prompt once they're idle. Either way the evidence (command, exit code, output tail, tree) is
//! kept, and `done(claimed)` (bounces ran out) stays distinct from `done(verified)`.
//!
//! **Checkpoints.** Every agent turn that changed the worktree leaves
//! `refs/ptyro/ckpt/<session>/<n>`: HEAD plus everything uncommitted, written through a private
//! index (no visible commit, no stash, the user's index untouched). `ptyro checkpoint restore`
//! puts any of them back, checkpointing the present first.

use ptyro_core::sync::MutexExt;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ptyro_core::model::{DiffAgainst, DiffDoc, WorktreeInfo};
use ptyro_core::session::{Evidence, SessionKind, Verify, VerifyState};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{Checkpoint, DomainEvent, Reply};
use serde_json::json;

use crate::daemon::Daemon;
use crate::session::Session;
use crate::worktrees::blocking;

const CHECK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Checkpoints kept per session (older ones are deleted).
const KEEP_CHECKPOINTS: u32 = 200;
/// What the bounce prompt starts with (a prompt that doesn't is the user's: it resets bounces).
pub const BOUNCE_MARK: &str = "[via ptyro: verify]";

/// A check ready to run: its line as written (for the evidence) and the argv that runs it.
pub(crate) struct Resolved {
    pub line: String,
    pub argv: Vec<String>,
}

impl Daemon {
    /// `scripts.run.<name>`, `run:<name>` or a bare run-script name → its command (trusted
    /// scripts only); anything else is a shell command as written. Either way, in the shell the
    /// project's scripts run in (`[scripts] shell`, WIN6).
    pub(crate) fn resolve_command(&self, wt: &WorktreeInfo, cmd: &str) -> Result<Resolved, Error> {
        let cmd = cmd.trim();
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let cfg =
            ptyro_core::project::ProjectConfig::load(&p.root, &wt.path).map_err(Error::invalid)?;
        let name =
            cmd.strip_prefix("scripts.run.").or_else(|| cmd.strip_prefix("run:")).or_else(|| {
                (!cmd.contains(char::is_whitespace) && cfg.scripts.run.contains_key(cmd))
                    .then_some(cmd)
            });
        let argv = |role: &str, line: &str| {
            cfg.script_argv(role, line)
                .map(|argv| Resolved { line: line.to_owned(), argv })
                .map_err(|m| Error::new(ErrorCode::NotFound, format!("{}: {}", p.slug, m.message)))
        };
        let Some(name) = name else { return argv("check", cmd) };
        let (_, script) = cfg.scripts.run_script(Some(name)).ok_or_else(|| {
            Error::new(
                ErrorCode::NotFound,
                format!("no run script {name}: add [scripts.run.{name}] to .ptyro.toml"),
            )
        })?;
        if !self.is_trusted(&p.id, &cfg.trust_fingerprint()) {
            return Err(Error::new(
                ErrorCode::UntrustedProject,
                format!(
                    "{}'s scripts are not trusted; review them with `ptyro project trust {}`",
                    p.slug, p.slug
                ),
            ));
        }
        argv(&format!("run:{name}"), &script.command)
    }

    /// Run a check in a worktree and keep what it said (`log`), with the worktree's variables.
    pub(crate) async fn run_check(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        cmd: &str,
        log: &Path,
        extra: Option<BTreeMap<String, String>>,
    ) -> Result<Evidence, Error> {
        let command = self.resolve_command(wt, cmd)?;
        self.run_in(&wt.path, &command, log, self.worktree_env(wt), extra).await
    }

    /// A resolved check in `dir`: combined output to `log`, exit code, tail, tree, timing.
    pub(crate) async fn run_in(
        self: &Arc<Self>,
        dir: &Path,
        check: &Resolved,
        log: &Path,
        env: Vec<(String, String)>,
        extra: Option<BTreeMap<String, String>>,
    ) -> Result<Evidence, Error> {
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let started = Instant::now();
        let command = check.line.as_str();
        // stdout and stderr both go to the log (`run_to_log`), interleaved as written.
        let mut c = ptyro_pty::command(&check.argv);
        c.current_dir(dir)
            .env_clear()
            .envs(self.base_env().iter().map(|(k, v)| (k, v)))
            .envs(env)
            .envs(extra.unwrap_or_default())
            .env("PTYRO_CHECK", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        ptyro_pty::own_group(&mut c);
        // Its output goes to the log file as it is written, never into the daemon's memory:
        // a check that prints without end is stopped at a size, not at the daemon's death.
        let ran = run_to_log(c, log, CHECK_TIMEOUT, CHECK_LOG_CAP)
            .await
            .map_err(|e| Error::internal(format!("cannot run `{command}`: {e}")))?;
        if let Some(why) = &ran.stopped {
            return Ok(Evidence {
                command: command.to_owned(),
                exit_code: if why.starts_with("timed out") { 124 } else { 125 },
                tail: format!("{why}\n{}", tail(&ran.text, 20)),
                tree: None,
                at_ms: now_ms(),
                duration_ms: started.elapsed().as_millis() as u64,
            });
        }
        let text = ran.text;
        let path = dir.to_path_buf();
        let tree = blocking(move || {
            ptyro_git::Git::new().checkpoint(&path, "ptyro check").ok().map(|s| s.tree)
        })
        .await
        .ok()
        .flatten();
        Ok(Evidence {
            command: command.to_owned(),
            exit_code: ran.code.unwrap_or(-1),
            tail: tail(&text, 40),
            tree,
            at_ms: now_ms(),
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    // ── verified-done ──────────────────────────────────────────────────────

    /// Turn verified-done on (`Some(check)`) or off for a session.
    pub(crate) fn set_verify(
        &self,
        s: &Arc<Session>,
        check: Option<String>,
        max_bounces: Option<u32>,
    ) -> Result<(), Error> {
        let v = match check {
            None => None,
            Some(c) => {
                let wt =
                    s.meta.locked().place.worktree.clone().and_then(|w| self.worktree_by_id(&w));
                let wt = wt
                    .ok_or_else(|| Error::invalid("verified-done needs a session in a worktree"))?;
                // Resolve now: an untrusted or missing script is an error at set time.
                self.resolve_command(&wt, &c)?;
                Some(Verify {
                    check: c,
                    max_bounces: max_bounces.unwrap_or(3),
                    ..Verify::default()
                })
            }
        };
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).verify = v;
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        Ok(())
    }

    /// A new step or prompt: the bounce count starts again.
    pub(crate) fn reset_verify(&self, s: &Arc<Session>) {
        if let Some(v) =
            &mut s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).verify
        {
            v.bounces = 0;
            v.state = VerifyState::Pending;
        }
    }

    /// Project `[verify] auto = true`: every agent in its worktrees is verified.
    pub(crate) fn auto_verify(&self, s: &Arc<Session>) {
        if s.kind() != SessionKind::Agent
            || s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).verify.is_some()
        {
            return;
        }
        let Some(wt) = s.meta.locked().place.worktree.clone().and_then(|w| self.worktree_by_id(&w))
        else {
            return;
        };
        let Some(p) = self.project_by_id(&wt.project) else { return };
        let Ok(cfg) = ptyro_core::project::ProjectConfig::load(&p.root, &wt.path) else { return };
        if let (true, Some(check)) = (cfg.verify.auto, cfg.verify.check.clone())
            && let Err(e) =
                self.set_verify(s, Some(format!("scripts.run.{check}")), cfg.verify.max_bounces)
        {
            tracing::warn!("[verify] auto: {}", e.message);
        }
    }

    /// Run the session's check for the turn that just ended. `Some(reason)`: bounce it.
    async fn verify_turn(self: &Arc<Self>, s: &Arc<Session>) -> Option<String> {
        let (v, wt) = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let wt = m.place.worktree.clone();
            let v = m.verify.as_mut()?;
            v.state = VerifyState::Checking;
            (v.clone(), wt)
        };
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        let wt = wt.and_then(|w| self.worktree_by_id(&w))?;
        let log = self.paths.data.join("sessions").join(s.id.full()).join("verify.log");
        let ev = match self.run_check(&wt, &v.check, &log, None).await {
            Ok(e) => e,
            Err(e) => Evidence {
                command: v.check.clone(),
                exit_code: -1,
                tail: e.message,
                at_ms: now_ms(),
                ..Evidence::default()
            },
        };
        let passed = ev.exit_code == 0;
        let bounce = !passed && v.bounces < v.max_bounces;
        s.record_event("verify", json!({ "check": ev.command, "exit_code": ev.exit_code, "tree": ev.tree, "bounce": bounce }));
        let reason = bounce.then(|| {
            format!(
                "{BOUNCE_MARK} Not done yet: `{}` exits {} (attempt {} of {}). Its output ends with:\n{}\nFix the cause and finish again; don't weaken the check.",
                ev.command, ev.exit_code, v.bounces + 1, v.max_bounces, tail(&ev.tail, 25)
            )
        });
        if let Some(v) =
            &mut s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).verify
        {
            v.state = if passed {
                VerifyState::Passed
            } else if bounce {
                VerifyState::Bounced
            } else {
                VerifyState::Failed
            };
            if bounce {
                v.bounces += 1;
            }
            v.last = Some(ev);
        }
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        reason
    }

    /// Claude's Stop hook: check before the turn is allowed to end. `Some(stdout)` blocks it.
    pub(crate) async fn verify_stop_hook(self: &Arc<Self>, s: &Arc<Session>) -> Option<String> {
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).verify.as_ref()?;
        let reason = self.verify_turn(s).await?;
        Some(json!({ "decision": "block", "reason": reason }).to_string())
    }

    /// Agents without a blocking Stop hook: check after the turn, bounce with a follow-up.
    pub(crate) fn verify_after_turn(self: &Arc<Self>, s: &Arc<Session>) {
        let fresh = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(v) = &m.verify else { return };
            // The Stop hook already decided this turn.
            v.last.as_ref().is_some_and(|e| e.at_ms + 2000 >= now_ms())
                && matches!(v.state, VerifyState::Passed | VerifyState::Failed)
        };
        if fresh {
            return;
        }
        let (me, s) = (Arc::clone(self), Arc::clone(s));
        tokio::spawn(async move {
            if let Some(reason) = me.verify_turn(&s).await {
                me.deliver_when_idle(&s, reason);
            }
        });
    }

    /// A prompt arrived (UserPromptSubmit): one that isn't a bounce is new work. Not while a
    /// bounce is outstanding: hooks arrive on their own connections, so a busy machine can deliver
    /// the turn's own prompt after that turn was already checked and bounced.
    pub(crate) fn verify_on_prompt(&self, s: &Arc<Session>, text: &str) {
        let bounced =
            s.meta.locked().verify.as_ref().is_some_and(|v| v.state == VerifyState::Bounced);
        if !text.contains(BOUNCE_MARK) && !bounced {
            self.reset_verify(s);
        }
    }

    // ── checkpoints ────────────────────────────────────────────────────────

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
                    let (files, truncated) = ptyro_git::review::parse_diff(&text);
                    Ok(DiffDoc { against: DiffAgainst::Head, from: Some(base), files, truncated })
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

/// The last `lines` lines of `text`, at most 4000 characters.
pub(crate) fn tail(text: &str, lines: usize) -> String {
    let v: Vec<&str> = text.lines().collect();
    let start = v.len().saturating_sub(lines);
    v[start..].join("\n").chars().rev().take(4000).collect::<String>().chars().rev().collect()
}

/// The most a check may write before it is stopped, and the most of it kept in memory (the tail).
pub(crate) const CHECK_LOG_CAP: u64 = 64 << 20;
const CHECK_TAIL_BYTES: u64 = 256 << 10;

/// How a command run by [`run_to_log`] ended.
pub(crate) struct Ran {
    pub code: Option<i32>,
    /// The end of its output (the whole of it is in the log file).
    pub text: String,
    /// Why it was stopped: it ran out of time, or wrote too much.
    pub stopped: Option<String>,
}

/// Run `c` with stdout and stderr going straight to `log`, in its own process group, killed
/// after `limit` or when the log passes `cap` bytes.
pub(crate) async fn run_to_log(
    mut c: tokio::process::Command,
    log: &Path,
    limit: Duration,
    cap: u64,
) -> std::io::Result<Ran> {
    use std::io::{Read, Seek, SeekFrom};
    let file = ptyro_core::os::private(
        std::fs::OpenOptions::new().write(true).create(true).truncate(true),
    )
    .open(log)?;
    c.stdout(std::process::Stdio::from(file.try_clone()?))
        .stderr(std::process::Stdio::from(file))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    ptyro_pty::own_group(&mut c);
    let mut child = c.spawn()?;
    // The whole group goes when this returns early or is dropped: `cargo test` under the shell
    // goes too, not just the shell.
    let _group = GroupGuard(child.id());
    let deadline = Instant::now() + limit;
    let (mut status, mut stopped) = (None, None);
    loop {
        tokio::select! {
            r = child.wait() => {
                status = Some(r?);
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(500)) => {
                if std::fs::metadata(log).map_or(0, |m| m.len()) > cap {
                    stopped = Some(format!("wrote more than {} KiB of output; stopped", cap >> 10));
                } else if Instant::now() >= deadline {
                    stopped = Some(format!("timed out after {} minutes", limit.as_secs() / 60));
                }
                if stopped.is_some() {
                    let _ = child.start_kill();
                    let _ = ptyro_pty::signal_group(child.id().unwrap_or(0), ptyro_pty::Signal::KILL);
                    let _ = child.wait().await;
                    break;
                }
            }
        }
    }
    let mut text = Vec::new();
    if let Ok(mut f) = std::fs::File::open(log) {
        let len = f.metadata().map_or(0, |m| m.len());
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(CHECK_TAIL_BYTES)));
        let _ = f.read_to_end(&mut text);
    }
    Ok(Ran {
        code: status.and_then(|s| s.code()),
        text: String::from_utf8_lossy(&text).into_owned(),
        stopped,
    })
}

/// Kills a process group when dropped (a check that timed out, or was given up on).
/// Harmless once the group is gone.
pub(crate) struct GroupGuard(pub Option<u32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = ptyro_pty::signal_group(pid, ptyro_pty::Signal::KILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)] // runs sh
    #[tokio::test]
    async fn a_check_that_prints_without_end_is_stopped_and_only_its_tail_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("check.log");
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg("while :; do echo 0123456789abcdef0123456789abcdef; done");
        let ran = run_to_log(c, &log, Duration::from_secs(30), 256 << 10).await.unwrap();
        assert!(ran.stopped.as_deref().is_some_and(|w| w.contains("output")), "{:?}", ran.stopped);
        assert!(ran.code != Some(0) && ran.text.len() as u64 <= CHECK_TAIL_BYTES);
        // A normal check: its exit code, its output, nothing stopped.
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg("echo out; echo err >&2; exit 3");
        let ran = run_to_log(c, &log, Duration::from_secs(30), 1 << 20).await.unwrap();
        assert_eq!((ran.code, ran.stopped.is_none()), (Some(3), true));
        assert!(ran.text.contains("out") && ran.text.contains("err"), "{}", ran.text);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&log).unwrap().permissions(),
        );
        assert_eq!(mode & 0o077, 0, "a check's log is private");
        // And one that outlives its time is killed with its group.
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg("sleep 60");
        let ran = run_to_log(c, &log, Duration::from_millis(600), 1 << 20).await.unwrap();
        assert!(
            ran.stopped.as_deref().is_some_and(|w| w.starts_with("timed out")),
            "{:?}",
            ran.stopped
        );
    }
}
