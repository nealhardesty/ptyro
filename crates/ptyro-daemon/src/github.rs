//! Starting work from GitHub through its CLI (SPEC §10.9): `wt new --issue N|URL` names the
//! worktree after the issue and seeds the agent's first prompt from it, and a worktree made for
//! a pull request (`--pr`) takes the PR's title. `gh` runs under the worktree's profile (so
//! `GH_CONFIG_DIR`/`GH_TOKEN` pick the right account), with a timeout. It is optional: without
//! it `--issue` says so, `--pr` still works (git fetches the PR), and the rest of ptyro doesn't
//! notice.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{DEFAULT_PROFILE, WorktreeInfo};
use ptyro_core::{Error, ErrorCode};
use serde_json::Value;

use crate::daemon::Daemon;
use crate::worktrees::blocking;

fn gh_err(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::Unsupported, msg)
}

/// `https://github.com/o/r/pull/123`, `#123`, `123` → 123.
pub fn number_of(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches('/');
    let tail = s.rsplit(['/', '#']).next()?;
    tail.parse().ok()
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
}

/// How long one `gh` call may take: a stalled network or credential helper must not hold a
/// worktree's creation forever.
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
        Ok(plan.apply(self.base_env().iter().cloned()))
    }

    /// Run `gh` in the worktree. `Ok(stdout)` also when gh exits non-zero but printed JSON.
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
                let fix = ptyro_agents::locate::install_fix("gh")
                    .map(|f| format!("; {f}"))
                    .unwrap_or_default();
                return Err(gh_err(format!(
                    "the GitHub CLI (`gh`) isn't installed: starting from an issue needs it{fix}"
                )));
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

    /// A worktree made for pull request `n` (`--pr`) reads as that PR (UX1.24): its title, asked
    /// of `gh` in the background, best effort. Without `gh` (or offline) it keeps its name.
    pub(crate) fn title_from_pr(self: &Arc<Self>, wt: &WorktreeInfo, n: u32) {
        let (me, wt) = (Arc::clone(self), wt.clone());
        tokio::spawn(async move {
            let Ok(v) = me.gh_json(&wt, &["pr", "view", &n.to_string(), "--json", "title"]).await
            else {
                return;
            };
            let title = str_of(&v, "title");
            if title.is_empty() {
                return;
            }
            let title = format!("#{n} {title}");
            if let Some(w) = me.update_worktree(&wt.id, move |w| {
                if w.title.is_none() {
                    w.title = Some(title);
                }
            }) {
                me.put_worktree(w);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers() {
        assert_eq!(super::number_of("https://github.com/o/r/pull/123"), Some(123));
        assert_eq!(super::number_of("#7"), Some(7));
        assert_eq!(super::number_of("https://github.com/o/r/issues/45/"), Some(45));
    }
}
