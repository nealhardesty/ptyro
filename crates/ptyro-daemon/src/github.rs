//! Pull requests through the GitHub CLI (SPEC §10.9, M6.5, M6.10). `gh` runs in the worktree,
//! under its profile (so `GH_CONFIG_DIR`/`GH_TOKEN` pick the right account), with a timeout;
//! without `gh` every PR feature says so and the rest of ptyro doesn't notice.
//!
//! The poller asks about each worktree whose branch is pushed at most once per `github.poll`.
//! For PRs ptyro opened (or `ptyro pr watch`), the PR-event loop sends *new* CI failures (with a
//! log tail) and *new* review comments to the worktree's agent, only once its turn is over,
//! tagged with where they came from. Questions and other comments that aren't clearly a request
//! raise attention instead of being typed into the agent.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{CheckRun, DEFAULT_PROFILE, PrInfo, WorktreeInfo, WorktreeState};
use ptyro_core::session::SessionKind;
use ptyro_core::status::{AgentStatus, AttentionKind};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::Reply;
use serde_json::Value;

use crate::daemon::Daemon;
use crate::session::Session;
use crate::worktrees::blocking;

/// What the PR-event loop remembers per worktree.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Watch {
    /// Feed events to the agent (PRs ptyro opened, or `pr watch`).
    pub on: bool,
    /// Review and PR comment ids already seen (baseline taken on the first poll).
    pub seen: HashSet<u64>,
    /// Checks that were failing at the last poll.
    pub failing: HashSet<String>,
    pub baselined: bool,
    /// Messages waiting for the agent's turn to end.
    pub pending: Vec<String>,
    /// Failing or waiting since (for `escalate_after`).
    pub trouble_since: Option<u64>,
    pub escalated: bool,
    #[serde(skip)]
    pub next_poll_ms: u64,
}

pub type Watches = HashMap<String, Watch>;

fn gh_err(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::Unsupported, msg)
}

/// `https://github.com/o/r/pull/123`, `#123`, `123` → 123.
pub fn number_of(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches('/');
    let tail = s.rsplit(['/', '#']).next()?;
    tail.parse().ok()
}

/// Comments that ask rather than tell go to the human (R-GIT-01: "ask when ambiguous").
pub fn ambiguous(body: &str) -> bool {
    let b = body.trim();
    if b.is_empty() {
        return true;
    }
    let questions = b.matches('?').count();
    let asks_back = ["why ", "what do you think", "should we", "do we need", "wdyt", "thoughts"]
        .iter()
        .any(|w| b.to_lowercase().contains(w));
    (questions > 0 && b.lines().count() <= 3 && !b.contains("```")) || asks_back
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// How long one `gh` call may take: a stalled network or credential helper must not hold the PR
/// poller (and every request that asks `gh`) forever.
const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// `Command::output`, killed after `limit`: `TimedOut` when it was. Both pipes are read on
/// threads, so a chatty command can't fill one and wait.
fn output_bounded(
    mut cmd: std::process::Command,
    limit: Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    let drain = |mut pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe.as_mut() {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"));
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

impl Daemon {
    /// The profile a worktree's tools run as.
    pub(crate) fn wt_profile(&self, wt: &WorktreeInfo) -> String {
        wt.profile
            .clone()
            .or_else(|| self.project_by_id(&wt.project).and_then(|p| p.pin))
            .unwrap_or_else(|| DEFAULT_PROFILE.into())
    }

    /// The environment git and gh get for a worktree: its profile's accounts and identity.
    pub(crate) async fn tool_env(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
    ) -> Result<Vec<(OsString, OsString)>, Error> {
        let plan = self.profile_plan(&self.wt_profile(wt)).await?;
        Ok(plan.apply(self.base_env.iter().cloned()))
    }

    /// Run `gh` in the worktree. `Ok(stdout)` also when gh exits non-zero but printed JSON
    /// (`gh pr checks` exits 8 while checks are pending).
    pub(crate) async fn gh(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        args: Vec<String>,
    ) -> Result<String, Error> {
        let env = self.tool_env(wt).await?;
        let dir = wt.path.clone();
        let out = blocking(move || {
            let mut c = std::process::Command::new("gh");
            c.args(&args)
                .current_dir(&dir)
                .env_clear()
                .envs(env)
                .env("GH_PROMPT_DISABLED", "1")
                .env("NO_COLOR", "1")
                .stdin(std::process::Stdio::null());
            output_bounded(c, GH_TIMEOUT)
        })
        .await?;
        let out = match out {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(gh_err("the GitHub CLI (`gh`) isn't installed: PR features need it"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return Err(Error::new(
                    ErrorCode::Timeout,
                    format!(
                        "gh didn't answer within {}s (offline, or waiting on a login?)",
                        GH_TIMEOUT.as_secs()
                    ),
                ));
            }
            Err(e) => return Err(Error::internal(format!("gh: {e}"))),
        };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() || stdout.trim_start().starts_with(['[', '{']) {
            Ok(stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            Err(Error::new(
                ErrorCode::Conflict,
                format!("gh: {}", if err.is_empty() { stdout.trim().to_owned() } else { err }),
            ))
        }
    }

    pub(crate) async fn gh_json(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        args: &[&str],
    ) -> Result<Value, Error> {
        let out = self.gh(wt, args.iter().map(|a| (*a).to_owned()).collect()).await?;
        serde_json::from_str(&out)
            .map_err(|e| Error::internal(format!("gh printed something unexpected: {e}")))
    }

    /// Ask gh about the worktree's PR (by its branch). `None`: no PR (yet).
    pub(crate) async fn pr_lookup(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
    ) -> Result<Option<PrInfo>, Error> {
        let Some(branch) = wt.branch.clone() else { return Ok(None) };
        let fields = "number,url,title,state,isDraft,reviewDecision,autoMergeRequest";
        let v = match self.gh_json(wt, &["pr", "view", &branch, "--json", fields]).await {
            Ok(v) => v,
            Err(e)
                if e.code == ErrorCode::Conflict
                    && e.message.to_lowercase().contains("no pull requests found") =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let number = v["number"].as_u64().unwrap_or(0) as u32;
        let checks = match self
            .gh_json(
                wt,
                &["pr", "checks", &number.to_string(), "--json", "name,bucket,link,workflow"],
            )
            .await
        {
            Ok(Value::Array(a)) => a
                .iter()
                .map(|c| CheckRun {
                    name: str_of(c, "name"),
                    bucket: str_of(c, "bucket"),
                    link: c["link"].as_str().map(str::to_owned),
                    workflow: c["workflow"].as_str().filter(|w| !w.is_empty()).map(str::to_owned),
                })
                .collect(),
            _ => Vec::new(),
        };
        let ours = self.pr_watches.lock().expect("watches").get(&wt.id).is_some_and(|w| w.on);
        Ok(Some(PrInfo {
            number,
            url: str_of(&v, "url"),
            title: str_of(&v, "title"),
            state: str_of(&v, "state"),
            draft: v["isDraft"].as_bool().unwrap_or(false),
            review: v["reviewDecision"].as_str().filter(|r| !r.is_empty()).map(str::to_owned),
            checks,
            automerge: !v["autoMergeRequest"].is_null(),
            ours,
            at_ms: now_ms(),
        }))
    }

    /// Refresh one worktree's PR and tell the clients.
    pub(crate) async fn refresh_pr(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
    ) -> Result<Option<PrInfo>, Error> {
        let pr = self.pr_lookup(wt).await?;
        let p = pr.clone();
        // A worktree made for a PR reads as that PR (UX1.24), unless it has a title: stored.
        let titled = wt.title.is_none() && p.as_ref().is_some_and(|pr| !pr.title.is_empty());
        let title = p.as_ref().map(|pr| format!("#{} {}", pr.number, pr.title));
        let updated = self.update_worktree(&wt.id, move |w| {
            if titled {
                w.title = title;
            }
            w.pr = p;
        });
        // `[worktrees] archive_on_merge` (UX1.12): when a PR seen open becomes merged. Not on a
        // first sighting, so a worktree restored after its PR merged isn't archived again.
        let was_open = wt.pr.as_ref().is_some_and(|p| p.state != "MERGED");
        if was_open && let Some(w) = &updated {
            self.archive_if_merged(w);
        }
        // The scratchpad item this worktree was started from is done (NOTE1.3).
        if let Some(pr) = &updated.as_ref().and_then(|w| w.pr.clone())
            && pr.state == "MERGED"
            && wt.pr.as_ref().is_none_or(|p| p.state != "MERGED")
        {
            self.pad_finished(&wt.qualified(), Some(u64::from(pr.number)));
        }
        if titled && let Some(w) = updated {
            self.put_worktree(w);
        }
        Ok(pr)
    }

    /// `wt new --issue N|URL` (M6.6): name the worktree and branch after the issue and, for an
    /// agent with no prompt of its own, seed one from the issue.
    pub(crate) async fn from_issue(
        self: &Arc<Self>,
        p: &ptyro_core::model::ProjectInfo,
        issue: &str,
        spec: &mut ptyro_core::model::WorktreeSpec,
    ) -> Result<String, Error> {
        let n = number_of(issue)
            .ok_or_else(|| Error::invalid(format!("{issue:?} isn't an issue number or URL")))?;
        let root = self
            .root_worktree(&p.id)
            .ok_or_else(|| Error::missing("worktree", format!("{}/@root", p.slug)))?;
        let v = self
            .gh_json(&root, &["issue", "view", &n.to_string(), "--json", "number,title,body,url"])
            .await?;
        let title = str_of(&v, "title");
        let slug: String = ptyro_core::model::slugify(&title)
            .chars()
            .take(32)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned();
        if spec.name.is_none() {
            spec.name = Some(format!("issue-{n}"));
        }
        if spec.title.is_none() && !title.is_empty() {
            spec.title = Some(format!("#{n} {title}"));
        }
        if spec.branch.is_none() {
            spec.branch = Some(if slug.is_empty() {
                format!("issue-{n}")
            } else {
                format!("issue-{n}-{slug}")
            });
        }
        if spec.prompt.is_none() {
            spec.prompt = Some(format!(
                "Work on GitHub issue #{n}: {title}\n{}\n\n{}\n\nWhen it's done, commit with a message that says `Fixes #{n}`.",
                str_of(&v, "url"),
                str_of(&v, "body").trim()
            ));
        }
        Ok(format!("from issue #{n}: {title}"))
    }

    /// Push the branch and open a PR, then watch it (M6.5).
    pub(crate) async fn pr_create(
        self: &Arc<Self>,
        addr: &str,
        draft: bool,
        title: Option<String>,
        body: Option<String>,
        base: Option<String>,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid("open PRs from a worktree, not the main checkout"));
        }
        let branch =
            wt.branch.clone().ok_or_else(|| Error::invalid("the worktree has no branch"))?;
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let base_branch = base
            .or_else(|| {
                wt.base.clone().map(|b| b.rsplit_once('/').map_or(b.clone(), |(_, x)| x.to_owned()))
            })
            .or_else(|| p.default_branch.clone())
            .unwrap_or_else(|| "main".into());
        let env = self.tool_env(&wt).await?;
        let (path, b) = (wt.path.clone(), branch.clone());
        let base_ref = wt.base.clone().unwrap_or_else(|| format!("origin/{base_branch}"));
        let commits = blocking(move || -> Result<Vec<ptyro_core::model::CommitInfo>, Error> {
            let git = ptyro_git::Git::with_env(env);
            let remote = git.main_remote(&path).map(|r| r.name).unwrap_or_else(|| "origin".into());
            git.push(&path, &remote, &b).map_err(crate::registry::git_err)?;
            let from = git.merge_base(&path, &base_ref, "HEAD").unwrap_or(base_ref);
            git.log(&path, &from).map_err(crate::registry::git_err)
        })
        .await??;
        if commits.is_empty() {
            return Err(Error::invalid(format!(
                "{branch} has no commits beyond {base_branch}: commit first"
            )));
        }
        let title = title.unwrap_or_else(|| {
            if commits.len() == 1 { commits[0].subject.clone() } else { wt.name.replace('-', " ") }
        });
        let session = self
            .sessions_in(&wt.id)
            .into_iter()
            .find(|s| s.kind() == SessionKind::Agent)
            .map(|s| s.id.full());
        let mut text = body.unwrap_or_else(|| {
            let mut b = String::from("## Changes\n\n");
            for c in commits.iter().rev() {
                b.push_str(&format!("- {}\n", c.subject));
            }
            b
        });
        // R-GIT-03: a marker that finds the session again from the PR.
        text.push_str(&format!(
            "\n\n<sub>Opened with ptyro from worktree `{}`.</sub>\n<!-- ptyro:worktree={}{} -->\n",
            wt.qualified(),
            wt.id,
            session.map(|s| format!(" session={s}")).unwrap_or_default()
        ));
        let mut args: Vec<String> = [
            "pr",
            "create",
            "--head",
            &branch,
            "--base",
            &base_branch,
            "--title",
            &title,
            "--body",
            &text,
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        if draft {
            args.push("--draft".into());
        }
        let url = self.gh(&wt, args).await?;
        {
            let mut w = self.pr_watches.lock().expect("watches");
            let e = w.entry(wt.id.clone()).or_default();
            e.on = self.config().github.watch;
            e.next_poll_ms = 0;
        }
        self.save_watches();
        let info = self.refresh_pr(&wt).await.ok().flatten();
        let info = info.or_else(|| {
            Some(PrInfo {
                number: crate::github::number_of(url.trim()).unwrap_or(0),
                url: url.trim().to_owned(),
                title,
                state: "OPEN".into(),
                draft,
                review: None,
                checks: Vec::new(),
                automerge: false,
                ours: true,
                at_ms: now_ms(),
            })
        });
        Ok(Reply::Pr { info })
    }

    /// The run id in a check's link (`…/actions/runs/<id>/job/<job>`).
    fn run_id(link: &str) -> Option<String> {
        let rest = link.split("/actions/runs/").nth(1)?;
        Some(rest.split('/').next()?.to_owned()).filter(|r| r.chars().all(|c| c.is_ascii_digit()))
    }

    /// The failing checks with a log tail each, as one message for the agent.
    async fn failure_report(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        pr: &PrInfo,
        only: Option<&HashSet<String>>,
    ) -> String {
        let lines = self.config().github.log_lines;
        let mut msg = format!("[via ptyro: CI on PR #{} is failing]\n", pr.number);
        let mut runs_done = HashSet::new();
        for c in pr.checks.iter().filter(|c| c.bucket == "fail" || c.bucket == "cancel") {
            if only.is_some_and(|o| !o.contains(&c.name)) {
                continue;
            }
            msg.push_str(&format!(
                "\n✗ {}{}\n",
                c.name,
                c.link.as_deref().map(|l| format!(" ({l})")).unwrap_or_default()
            ));
            if let Some(run) = c.link.as_deref().and_then(Self::run_id)
                && runs_done.insert(run.clone())
                && let Ok(log) =
                    self.gh(wt, vec!["run".into(), "view".into(), run, "--log-failed".into()]).await
            {
                let all: Vec<&str> = log.lines().collect();
                msg.push_str("```\n");
                for l in &all[all.len().saturating_sub(lines)..] {
                    msg.push_str(l);
                    msg.push('\n');
                }
                msg.push_str("```\n");
            }
        }
        msg.push_str("\nFind the cause, fix it, and commit and push the fix.");
        msg
    }

    /// `ptyro pr fix`: the failing checks' logs go to the agent now (once its turn is over).
    pub(crate) async fn pr_fix(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let pr = self
            .refresh_pr(&wt)
            .await?
            .ok_or_else(|| Error::missing("pull request", wt.qualified()))?;
        let (_, failed, pending) = pr.tally();
        if failed == 0 {
            return Err(Error::conflict(if pending > 0 {
                format!("PR #{}: no failing checks yet ({pending} still running)", pr.number)
            } else {
                format!("PR #{}: no failing checks", pr.number)
            }));
        }
        let msg = self.failure_report(&wt, &pr, None).await;
        let s = self
            .owner(&wt)
            .ok_or_else(|| Error::missing("agent session", format!("in {}", wt.qualified())))?;
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        self.deliver_when_idle(&s, msg);
        Ok(Reply::Notes {
            notes: vec![format!(
                "sent {failed} failing check(s) to {name} (typed once its turn is over)"
            )],
        })
    }

    pub(crate) async fn pr_automerge(
        self: &Arc<Self>,
        addr: &str,
        method: Option<String>,
        off: bool,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let pr = self
            .refresh_pr(&wt)
            .await?
            .ok_or_else(|| Error::missing("pull request", wt.qualified()))?;
        let n = pr.number.to_string();
        let args: Vec<String> = if off {
            vec!["pr".into(), "merge".into(), n, "--disable-auto".into()]
        } else {
            let m = method.unwrap_or_else(|| self.config().github.merge_method);
            if !["squash", "merge", "rebase"].contains(&m.as_str()) {
                return Err(Error::invalid(format!("merge method {m:?}: squash, merge or rebase")));
            }
            vec!["pr".into(), "merge".into(), n, "--auto".into(), format!("--{m}")]
        };
        self.gh(&wt, args).await?;
        Ok(Reply::Pr { info: self.refresh_pr(&wt).await.ok().flatten() })
    }

    pub(crate) async fn pr_reply(
        self: &Arc<Self>,
        addr: &str,
        comment: Option<u64>,
        text: &str,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let pr = self
            .refresh_pr(&wt)
            .await?
            .ok_or_else(|| Error::missing("pull request", wt.qualified()))?;
        let body = format!("{}\n\n<sub>— via ptyro</sub>", text.trim_end());
        let args: Vec<String> = match comment {
            Some(id) => vec![
                "api".into(),
                "--method".into(),
                "POST".into(),
                format!("repos/{{owner}}/{{repo}}/pulls/{}/comments/{id}/replies", pr.number),
                "-f".into(),
                format!("body={body}"),
            ],
            None => {
                vec!["pr".into(), "comment".into(), pr.number.to_string(), "--body".into(), body]
            }
        };
        self.gh(&wt, args).await?;
        Ok(Reply::Done)
    }

    pub(crate) fn pr_watch(&self, addr: &str, off: bool) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        {
            let mut w = self.pr_watches.lock().expect("watches");
            let e = w.entry(wt.id.clone()).or_default();
            e.on = !off;
            e.next_poll_ms = 0;
            if off {
                e.pending.clear();
            }
        }
        self.save_watches();
        self.update_worktree(&wt.id, |w| {
            if let Some(p) = &mut w.pr {
                p.ours = !off;
            }
        });
        Ok(Reply::Done)
    }

    /// The agent session that owns a worktree: the newest live agent in it.
    pub(crate) fn owner(&self, wt: &WorktreeInfo) -> Option<Arc<Session>> {
        self.sessions_in(&wt.id)
            .into_iter()
            .filter(|s| s.kind() == SessionKind::Agent && s.is_live())
            .max_by_key(|s| s.id.created_ms())
    }

    /// Type `text` once the agent's turn is over (never mid-turn, never into a dialog).
    pub(crate) fn deliver_when_idle(self: &Arc<Self>, s: &Arc<Session>, text: String) {
        let (me, s) = (Arc::clone(self), Arc::clone(s));
        tokio::spawn(async move {
            me.deliver_idle(&s, &text).await;
        });
    }

    /// [`Daemon::deliver_when_idle`], awaited: `false` if the session ended first.
    pub(crate) async fn deliver_idle(self: &Arc<Self>, s: &Arc<Session>, text: &str) -> bool {
        loop {
            if !s.is_live() {
                return false;
            }
            let working = s.status() == Some(AgentStatus::Working);
            if !working && self.ready_for_input(s) {
                self.type_prompt(s, text, true).await;
                s.record_event("delivered", serde_json::json!({ "chars": text.chars().count() }));
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub(crate) fn save_watches(&self) {
        let v =
            serde_json::to_value(&*self.pr_watches.lock().expect("watches")).unwrap_or_default();
        self.save_kv("pr_watches", v);
    }

    pub(crate) async fn load_watches(&self) {
        if let Some(store) = &self.store
            && let Ok(Some(v)) = store.kv_get("pr_watches".into()).await
            && let Ok(w) = serde_json::from_value::<Watches>(v)
        {
            *self.pr_watches.lock().expect("watches") = w;
        }
    }

    /// Every 10 s: poll the worktrees that are due (pushed branches, at most once per
    /// `github.poll`), then run the PR-event loop on the watched ones.
    pub fn start_pr_poller(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            me.load_watches().await;
            // No gh, no polling (checked once a minute, so installing it later just works).
            let mut gh_checked_ms = 0u64;
            let mut have_gh = false;
            loop {
                // Tests set PTYRO_TEST_FAST_POLL to watch PRs without waiting a minute.
                let fast = std::env::var_os("PTYRO_TEST_FAST_POLL").is_some();
                tokio::time::sleep(Duration::from_millis(if fast { 300 } else { 10_000 })).await;
                let now = now_ms();
                if now.saturating_sub(gh_checked_ms) > 60_000 || gh_checked_ms == 0 {
                    gh_checked_ms = now;
                    have_gh = which_gh(&me.base_env);
                }
                if !have_gh {
                    continue;
                }
                me.poll_due().await;
            }
        });
    }

    async fn poll_due(self: &Arc<Self>) {
        let poll = ptyro_core::config::parse_duration(&self.config().github.poll)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(60_000)
            .max(30_000);
        // Never more than twice a minute against GitHub; tests shorten it.
        let poll = if std::env::var_os("PTYRO_TEST_FAST_POLL").is_some() { 500 } else { poll };
        let now = now_ms();
        let due: Vec<WorktreeInfo> = {
            let reg = self.registry.lock().expect("registry");
            let w = self.pr_watches.lock().expect("watches");
            reg.worktrees
                .iter()
                .filter(|wt| !wt.is_root && wt.state == WorktreeState::Ready && wt.branch.is_some())
                .filter(|wt| {
                    wt.pr.is_some()
                        || w.contains_key(&wt.id)
                        || wt.status.as_ref().is_some_and(|s| s.upstream.is_some())
                })
                .filter(|wt| w.get(&wt.id).is_none_or(|x| x.next_poll_ms <= now))
                .cloned()
                .collect()
        };
        for wt in due {
            self.pr_watches
                .lock()
                .expect("watches")
                .entry(wt.id.clone())
                .or_default()
                .next_poll_ms = now + poll;
            match self.refresh_pr(&wt).await {
                Ok(Some(pr)) => self.pr_events(&wt, &pr).await,
                Ok(None) => {}
                Err(e) => tracing::debug!(worktree = %wt.qualified(), "gh: {}", e.message),
            }
        }
        self.flush_pending();
    }

    /// The PR-event loop for one poll (M6.10).
    async fn pr_events(self: &Arc<Self>, wt: &WorktreeInfo, pr: &PrInfo) {
        let (on, baselined, before) = {
            let w = self.pr_watches.lock().expect("watches");
            let e = w.get(&wt.id).cloned().unwrap_or_default();
            (e.on, e.baselined, e.failing)
        };
        let failing: HashSet<String> = pr
            .checks
            .iter()
            .filter(|c| c.bucket == "fail" || c.bucket == "cancel")
            .map(|c| c.name.clone())
            .collect();
        let comments = self.pr_comments(wt, pr.number).await;
        let mut messages = Vec::new();
        let mut asks = Vec::new();
        {
            let mut w = self.pr_watches.lock().expect("watches");
            let e = w.entry(wt.id.clone()).or_default();
            if !baselined {
                // The first look is the baseline: only what happens from now on is news.
                e.seen.extend(comments.iter().map(|c| c.0));
                e.baselined = true;
            } else if on && pr.state == "OPEN" {
                for (id, line_comment, who, path, body, trusted) in &comments {
                    if !e.seen.insert(*id) {
                        continue;
                    }
                    let place = path.as_deref().map(|p| format!(" on {p}")).unwrap_or_default();
                    // Anyone can comment on a public PR: only the repository's own people
                    // instruct the agent; the rest comes to you.
                    if !trusted {
                        asks.push(format!(
                            "PR #{}: {who} (not a collaborator) commented{place}: {}",
                            pr.number,
                            body.lines().next().unwrap_or("")
                        ));
                    } else if !line_comment && ambiguous(body) {
                        asks.push(format!(
                            "PR #{}: {who} asks{place}: {}",
                            pr.number,
                            body.lines().next().unwrap_or("")
                        ));
                    } else {
                        messages.push(format!(
                            "[via ptyro: review comment from {who} on PR #{}{place} (comment {id})]\n{}\n\nAddress it, then commit and push.",
                            pr.number,
                            body.trim()
                        ));
                    }
                }
            }
            e.failing = failing.clone();
            let trouble = !failing.is_empty() || !e.pending.is_empty();
            e.trouble_since = if trouble { e.trouble_since.or(Some(now_ms())) } else { None };
            if !trouble {
                e.escalated = false;
            }
        }
        let new_failures: HashSet<String> = failing.difference(&before).cloned().collect();
        if on && baselined && pr.state == "OPEN" && !new_failures.is_empty() {
            messages.insert(0, self.failure_report(wt, pr, Some(&new_failures)).await);
        }
        if !messages.is_empty() {
            self.pr_watches
                .lock()
                .expect("watches")
                .entry(wt.id.clone())
                .or_default()
                .pending
                .extend(messages);
        }
        for a in asks {
            if let Some(s) = self.owner(wt).or_else(|| self.sessions_in(&wt.id).into_iter().next())
            {
                self.raise(&s, AttentionKind::Notification, a).await;
            }
        }
        self.escalate(wt, pr).await;
        self.save_watches();
    }

    /// Review comments (line comments) and PR conversation comments: (id, is a line comment,
    /// author, path, body without control characters, from the repository's owner, a member or
    /// a collaborator). Bots' comments are left out.
    async fn pr_comments(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        n: u32,
    ) -> Vec<(u64, bool, String, Option<String>, String, bool)> {
        let trusted = |a: Option<&str>| matches!(a, Some("OWNER" | "MEMBER" | "COLLABORATOR"));
        let clean = |s: String| -> String {
            s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
        };
        let mut out = Vec::new();
        // Newest first: a busy PR's new comments are always on the first page.
        if let Ok(Value::Array(a)) = self
            .gh_json(
                wt,
                &[
                    "api",
                    &format!(
                        "repos/{{owner}}/{{repo}}/pulls/{n}/comments?per_page=100&sort=created&direction=desc"
                    ),
                ],
            )
            .await
        {
            for c in a {
                let who = c["user"]["login"].as_str().unwrap_or("someone").to_owned();
                if who.ends_with("[bot]")
                    || c["in_reply_to_id"].is_u64()
                        && c["body"].as_str().is_some_and(|b| b.contains("via ptyro"))
                {
                    continue;
                }
                let line = c["line"].as_u64().or_else(|| c["original_line"].as_u64());
                let path = c["path"].as_str().map(|p| match line {
                    Some(l) => format!("{p}:{l}"),
                    None => p.to_owned(),
                });
                let ok = trusted(c["author_association"].as_str());
                out.push((c["id"].as_u64().unwrap_or(0), true, who, path, clean(str_of(&c, "body")), ok));
            }
        }
        if let Ok(v) = self.gh_json(wt, &["pr", "view", &n.to_string(), "--json", "comments"]).await
        {
            for c in v["comments"].as_array().into_iter().flatten() {
                let who = c["author"]["login"].as_str().unwrap_or("someone").to_owned();
                let body = str_of(c, "body");
                if who.ends_with("[bot]") || body.contains("via ptyro") {
                    continue;
                }
                // Conversation comments have node ids: hash them into the same id space.
                let id = c["id"].as_str().map(fnv).or_else(|| c["id"].as_u64()).unwrap_or(0);
                let ok = trusted(c["authorAssociation"].as_str());
                out.push((id, false, who, None, clean(body), ok));
            }
        }
        out
    }

    /// Deliver what's pending to each watched worktree's agent once its turn is over.
    fn flush_pending(self: &Arc<Self>) {
        let ids: Vec<String> = self
            .pr_watches
            .lock()
            .expect("watches")
            .iter()
            .filter(|(_, w)| !w.pending.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            let Some(wt) = self.worktree_by_id(&id) else { continue };
            let Some(s) = self.owner(&wt) else { continue };
            if s.status() == Some(AgentStatus::Working) || !self.ready_for_input(&s) {
                continue;
            }
            let msgs = std::mem::take(
                &mut self.pr_watches.lock().expect("watches").entry(id).or_default().pending,
            );
            self.deliver_when_idle(&s, msgs.join("\n\n---\n\n"));
        }
        self.save_watches();
    }

    async fn escalate(self: &Arc<Self>, wt: &WorktreeInfo, pr: &PrInfo) {
        let after = ptyro_core::config::parse_duration(&self.config().github.escalate_after)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(7_200_000);
        if after == 0 {
            return;
        }
        let due = {
            let mut w = self.pr_watches.lock().expect("watches");
            let e = w.entry(wt.id.clone()).or_default();
            let due = e.on
                && !e.escalated
                && e.trouble_since.is_some_and(|t| now_ms().saturating_sub(t) >= after);
            if due {
                e.escalated = true;
            }
            due
        };
        if due
            && let Some(s) = self.owner(wt).or_else(|| self.sessions_in(&wt.id).into_iter().next())
        {
            let (_, failed, _) = pr.tally();
            let what = if failed > 0 {
                format!("{failed} check(s) still failing")
            } else {
                "review comments still waiting".into()
            };
            self.raise(&s, AttentionKind::Errored, format!("PR #{} needs you: {what}", pr.number))
                .await;
        }
    }
}

fn which_gh(env: &[(OsString, OsString)]) -> bool {
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join("gh").is_file())
}

/// FNV-1a: a stable id for GitHub node ids.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_and_ambiguity() {
        assert_eq!(super::number_of("https://github.com/o/r/pull/123"), Some(123));
        assert_eq!(super::number_of("#7"), Some(7));
        assert_eq!(super::number_of("https://github.com/o/r/issues/45/"), Some(45));
        assert!(super::ambiguous("Why not use a HashMap here?"));
        assert!(super::ambiguous(""));
        assert!(!super::ambiguous("Please rename `x` to `count`."));
        assert!(!super::ambiguous("This leaks the file handle; close it in the error path."));
    }
}
