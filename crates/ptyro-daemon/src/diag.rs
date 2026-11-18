//! What the daemon tells `ptyro doctor` (M5.6): its environment, the problems it noticed, and a
//! hook self-test that runs the fake agent through an adapter's real integration.

use ptyro_core::sync::MutexExt;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::session::SessionKind;
use ptyro_core::status::{AgentStatus, SignalSource};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize, now_ms};
use ptyro_protocol::{DaemonDoctor, SelfTestResult};

use crate::daemon::Daemon;
use crate::launch::{AgentStart, Launch};
use crate::session::Place;

/// Adapters whose integration ptyro wires in per launch, so a self-test needs nothing installed.
const SELF_TESTABLE: &[&str] = &["claude", "codex", "opencode"];

impl Daemon {
    /// Remember a problem for `ptyro doctor` (once).
    pub(crate) fn warn(&self, text: String) {
        let mut w = self.warnings.locked();
        if !w.contains(&text) {
            tracing::warn!("{text}");
            w.push(text);
        }
    }

    pub(crate) fn doctor(&self) -> DaemonDoctor {
        let var = |k: &str| {
            self.base_env()
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let leaked_vars = ptyro_agents::profile::MANAGED_VARS
            .iter()
            .filter(|v| **v != "PTYRO_PROFILE")
            .filter(|v| var(v).is_some())
            .map(|v| (*v).to_owned())
            .collect();
        let warnings = self.warnings.locked().clone();
        let config_error =
            warnings.iter().find(|w| w.starts_with("the config didn't load")).cloned();
        DaemonDoctor {
            leaked_vars,
            path: var("PATH").unwrap_or_default(),
            warnings,
            config_error,
            chats: None,
            meaning: None,
            left_behind: Vec::new(),
        }
    }

    /// What archived worktrees and pair mode (both removed 2026-11-17) left in ptyro's records,
    /// read as plain JSON: notes for each archive (its branch and snapshot stay in the
    /// repository) and a warning for each main checkout still paired, each with how to get it
    /// back with git. Nothing here changes a repository.
    pub(crate) async fn left_behind(&self) -> (Vec<String>, Vec<String>) {
        let Some(store) = &self.store else { return (Vec::new(), Vec::new()) };
        let get = |k: &'static str| async move { store.kv_get(k.into()).await.ok().flatten() };
        let root = |project: &str| {
            self.project_by_id(project)
                .map_or_else(|| "the repository".to_owned(), |p| p.root.display().to_string())
        };
        let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_owned();
        let mut notes = Vec::new();
        let archives = get("archive:list").await;
        let archives = archives.as_ref().and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if !archives.is_empty() {
            let each: Vec<String> = archives
                .iter()
                .take(5)
                .map(|e| {
                    format!(
                        "{}/{} (branch {}, snapshot {}, in {})",
                        text(e, "project_slug"),
                        text(e, "name"),
                        text(e, "branch"),
                        text(e, "snapshot"),
                        root(&text(e, "project"))
                    )
                })
                .collect();
            let more = archives.len().saturating_sub(each.len());
            notes.push(format!(
                "{} worktree(s) archived before archives were removed (2026-11-17) are still in \
                 their repositories, each a branch and a snapshot of its files: {}{}. To bring one \
                 back: `git -C REPO worktree add DIR BRANCH`, then `git -C DIR checkout SNAPSHOT \
                 -- .`; files git ignores that were kept with them are in {}",
                archives.len(),
                each.join("; "),
                if more > 0 { format!(" and {more} more") } else { String::new() },
                self.paths.data.join("archive-extras").display()
            ));
        }
        let mut warnings = Vec::new();
        let pairs = get("pairs").await;
        for (project, c) in pairs.as_ref().and_then(|v| v.as_object()).into_iter().flatten() {
            let orig = text(c, "orig");
            let back = if c["orig_is_branch"].as_bool() == Some(true) {
                format!("its branch {orig}")
            } else {
                format!("commit {}", &orig[..12.min(orig.len())])
            };
            let stash = c["stash"].as_str().map_or_else(String::new, |sha| {
                format!(
                    ", then `git -C {} stash apply {sha}` brings back the work it had (it is in \
                     `git stash list`)",
                    root(project)
                )
            });
            let wt = text(c, "worktree");
            let wt = self.worktree_by_id(&wt).map_or(wt, |w| w.qualified());
            warnings.push(format!(
                "the main checkout {} still shows {wt}: pair mode was removed (2026-11-17). Copy \
                 any edits you made there to {wt} first; `git -C {} checkout --force {orig}` puts \
                 it back on {back}{stash}",
                root(project),
                root(project)
            ));
        }
        (notes, warnings)
    }

    /// Start `ptyro fake-agent --as <adapter>` through the adapter, give it one short turn, and
    /// report the statuses that arrived and where from. It passes when the turn's working and
    /// idle both came from the integration (hooks or the sidecar), not from guessing.
    pub(crate) async fn self_test(
        self: &Arc<Self>,
        adapter: &str,
    ) -> Result<SelfTestResult, Error> {
        if !SELF_TESTABLE.contains(&adapter) {
            return Err(Error::invalid(format!(
                "the self-test covers {} (the others need an integration installed first)",
                SELF_TESTABLE.join(", ")
            )));
        }
        let exe = std::env::current_exe().map_err(|e| Error::internal(e.to_string()))?;
        let dir = self.paths.data.join("selftest");
        std::fs::create_dir_all(&dir).map_err(|e| Error::internal(e.to_string()))?;
        let id = SessionId::new();
        let name = self.free_name(None, &format!("selftest-{adapter}"));
        let s = self
            .launch(Launch {
                id,
                argv: Vec::new(),
                kind: SessionKind::Agent,
                name,
                cwd: dir,
                size: TermSize::new(100, 30),
                client_env: Default::default(),
                place: Place {
                    profile: ptyro_core::model::DEFAULT_PROFILE.into(),
                    agent: Some(adapter.to_owned()),
                    ..Place::default()
                },
                replace: false,
                agent: Some(AgentStart {
                    name: adapter.to_owned(),
                    resume: None,
                    prompt: None,
                    command: Some(vec![
                        exe.to_string_lossy().into_owned(),
                        "fake-agent".into(),
                        "--as".into(),
                        adapter.into(),
                    ]),
                    structured: false,
                    params: None,
                    fork: false,
                }),
            })
            .await?;
        let mut seen: Vec<(AgentStatus, SignalSource)> = Vec::new();
        let mut sent = false;
        let start = now_ms();
        let integrated =
            |src: SignalSource| matches!(src, SignalSource::Hook | SignalSource::Sidecar);
        // Passes when the turn's start came from the integration (hooks or the sidecar).
        let ok = loop {
            if let Some(r) = s.engine.locked().record.clone()
                && seen.last() != Some(&(r.status, r.source))
            {
                seen.push((r.status, r.source));
            }
            // Whether or not it won the race against OSC 7501, the integration must have spoken.
            let worked = s.engine.locked().explain.iter().any(|l| {
                l.signal.starts_with("working")
                    && (l.source.starts_with("hook") || l.source.starts_with("sidecar"))
            });
            // The end of the turn may come from OSC 7501 first (the agent's own report).
            let settled = seen.last().is_some_and(|(st, src)| {
                *st == AgentStatus::Idle && (integrated(*src) || *src == SignalSource::Osc)
            });
            if worked && settled {
                break true;
            }
            // A sidecar must be listening before the turn, or its events are lost.
            let listening = adapter != "opencode" || self.sidecar_live.locked().contains(&s.id);
            if !sent && listening && now_ms().saturating_sub(start) > 300 {
                s.send_text("work 0.2 ptyro self-test", true).await;
                sent = true;
            }
            if !s.is_live() || now_ms().saturating_sub(start) > 15_000 {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if s.is_live() {
            s.send_text("exit", true).await;
            let _ = tokio::time::timeout(Duration::from_secs(3), s.wait_exit()).await;
            let _ = s.kill(None).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), s.wait_exit()).await;
        self.forget_session(s.id);
        let detail = (!ok).then(|| {
            if seen.is_empty() {
                "no status arrived at all: the integration didn't reach the daemon".to_owned()
            } else {
                "the turn's status didn't come from the integration (hooks); ptyro would be guessing"
                    .to_owned()
            }
        });
        if ok {
            Ok(SelfTestResult {
                adapter: adapter.into(),
                ok,
                seen: seen
                    .iter()
                    .map(|(st, src)| format!("{} via {}", st.label(), src.label()))
                    .collect(),
                detail,
            })
        } else {
            Err(Error::new(
                ErrorCode::Internal,
                format!(
                    "{adapter} self-test failed: {} (seen: {})",
                    detail.unwrap_or_default(),
                    seen.iter()
                        .map(|(st, src)| format!("{} via {}", st.label(), src.label()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use ptyro_core::config::Config;
    use ptyro_core::paths::Paths;
    use ptyro_store::{Registry, Store};

    fn daemon(dir: &std::path::Path) -> (std::sync::Arc<super::Daemon>, Store) {
        let home = dir.to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        let store = Store::open(&dir.join("ptyro.db")).unwrap();
        let d =
            super::Daemon::new(paths, Some(store.clone()), Config::default(), Registry::default());
        (d, store)
    }

    /// An archive and a paired main checkout from before both were removed (2026-11-17): the
    /// doctor is told where each is and how to get it back with git, and nothing else changes.
    #[tokio::test]
    async fn archives_and_a_pair_left_behind_are_told_with_their_way_back() {
        let dir = tempfile::tempdir().unwrap();
        let (d, store) = daemon(dir.path());
        assert_eq!(d.left_behind().await, (Vec::new(), Vec::new()), "nothing left: nothing said");
        let archive = serde_json::json!([{
            "id": "a1", "project": "p1", "project_slug": "web", "name": "old",
            "branch": "old", "snapshot": "refs/ptyro/archive/old", "path": "/w/old",
            "archived_ms": 1,
        }]);
        store.kv_set("archive:list".into(), archive).await.unwrap();
        let pair = serde_json::json!({ "p1": {
            "worktree": "t1", "orig": "main", "orig_is_branch": true, "stash": "abc123",
            "synced": "def", "copied": [], "preexisting": [],
        }});
        store.kv_set("pairs".into(), pair).await.unwrap();
        let (notes, warnings) = d.left_behind().await;
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("web/old (branch old, snapshot refs/ptyro/archive/old")
                && notes[0].contains("git -C DIR checkout SNAPSHOT -- ."),
            "{}",
            notes[0]
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let w = &warnings[0];
        assert!(w.contains("pair mode was removed") && w.contains("checkout --force main"), "{w}");
        assert!(w.contains("stash apply abc123") && w.contains("its branch main"), "{w}");
    }
}
