//! Quick new (UX3.2, UX3.7): the agent each worktree last started, so `SpawnSpec.again` can start
//! another of it in one key (`C-q C-n`, `ptyro new --again`, the web's "+ claude"). It is always
//! an agent (`!` is the key for a shell): the worktree's last one (with its profile and
//! arguments), else its project's `default_agent`, else `[agents] default` (claude). Remembered
//! for agents people start, never a client's tool, the quick terminal or the scratchpad's editor,
//! and kept in `kv` across restarts.

use std::collections::HashMap;
use std::sync::Arc;

use ptyro_core::Error;
use ptyro_core::model::{ProjectInfo, WorktreeInfo};
use ptyro_core::session::{LastLaunch, SpawnSpec};
use ptyro_core::sync::MutexExt;

use crate::daemon::Daemon;

/// Worktree id → the agent it last started. (`last_launch`, before UX3.7, was per workspace and
/// held shells too; it is no longer read.)
const KEY: &str = "last_agent";

impl Daemon {
    pub(crate) async fn load_last_launch(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get(KEY.into()).await
            && let Ok(m) = serde_json::from_value::<HashMap<String, LastLaunch>>(v)
        {
            self.last_launch.locked().extend(m);
        }
    }

    /// What quick new starts in `target`: its worktree's last agent, else its project's default
    /// agent, else the default agent.
    pub(crate) fn next_launch(&self, target: Option<&(WorktreeInfo, ProjectInfo)>) -> LastLaunch {
        if let Some(l) = target.and_then(|(w, _)| self.last_launch.locked().get(&w.id).cloned()) {
            return l;
        }
        let project = target.and_then(|(w, p)| {
            ptyro_core::project::ProjectConfig::load(&p.root, &w.path).ok()?.project.default_agent
        });
        let agent = project
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| ptyro_agents::default_agent(&self.config()));
        LastLaunch { agent: Some(agent), ..LastLaunch::default() }
    }

    /// `LastLaunch`: what quick new would start in `worktree` (an address), or anywhere outside
    /// one.
    pub(crate) fn launch_for(&self, worktree: Option<&str>) -> Result<LastLaunch, Error> {
        let target = match worktree {
            Some(w) => self.target(Some(w), std::path::Path::new(""))?,
            None => None,
        };
        Ok(self.next_launch(target.as_ref()))
    }

    /// Remember the agent `spec` started in the worktree `worktree` (an id). Shells, command
    /// lines, tools, quick terminals and the scratchpad's editor don't count.
    pub(crate) async fn remember_launch(&self, worktree: Option<&str>, spec: &SpawnSpec) {
        let (Some(worktree), Some(agent)) = (worktree, &spec.agent) else { return };
        if spec.tool || spec.quick || spec.pad {
            return;
        }
        let launch = LastLaunch {
            agent: Some(agent.clone()),
            profile: spec.profile.clone(),
            argv: spec.argv.clone(),
        };
        let snapshot = {
            let mut all = self.last_launch.locked();
            if all.get(worktree) == Some(&launch) {
                return;
            }
            all.insert(worktree.to_owned(), launch);
            serde_json::to_value(&*all).unwrap_or_default()
        };
        if let Some(st) = &self.store {
            let _ = st.kv_set(KEY.into(), snapshot).await;
        }
    }

    /// A removed worktree's memory goes with it.
    pub(crate) async fn forget_launch(&self, worktree: &str) {
        let snapshot = {
            let mut all = self.last_launch.locked();
            if all.remove(worktree).is_none() {
                return;
            }
            serde_json::to_value(&*all).unwrap_or_default()
        };
        if let Some(st) = &self.store {
            let _ = st.kv_set(KEY.into(), snapshot).await;
        }
    }

    /// `again`: fill in the agent (with its profile and arguments) quick new starts where `spec`
    /// runs. What `spec` names itself wins.
    pub(crate) async fn resolve_again(
        self: &Arc<Self>,
        mut spec: SpawnSpec,
    ) -> Result<SpawnSpec, Error> {
        spec.again = false;
        if spec.agent.is_some() || !spec.argv.is_empty() || spec.command.is_some() {
            return Ok(spec);
        }
        if spec.worktree.is_none() {
            self.auto_register(&spec.cwd, spec.workspace.as_deref()).await;
        }
        let target = self.target(spec.worktree.as_deref(), &spec.cwd)?;
        let next = self.next_launch(target.as_ref());
        spec.agent = next.agent;
        spec.argv = next.argv;
        spec.profile = spec.profile.or(next.profile);
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_say_what_starts() {
        let l = |agent: Option<&str>, profile: Option<&str>, argv: &[&str]| LastLaunch {
            agent: agent.map(str::to_owned),
            profile: profile.map(str::to_owned),
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        };
        assert_eq!(l(Some("claude"), None, &[]).label(), "claude");
        assert_eq!(l(Some("claude"), Some("work"), &[]).label(), "claude@work");
        assert_eq!(l(Some("claude@work"), Some("work"), &[]).label(), "claude@work");
        assert_eq!(l(None, None, &[]).label(), "shell");
        assert_eq!(l(None, None, &["/usr/bin/htop", "-d"]).label(), "htop");
    }
}
