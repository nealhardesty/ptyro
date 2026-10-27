//! Quick-new (UX3.2): what each workspace last started, so `SpawnSpec.again` can start another
//! of it in one key (`C-q C-n`, `ptyro new --again`, the web's "+ again"). Remembered for
//! sessions people start (an agent, a shell, a command line), not for a client's tools, the
//! quick terminal or the scratchpad's editor, and kept in `kv` across restarts.

use std::collections::HashMap;
use std::sync::Arc;

use ptyro_core::Error;
use ptyro_core::session::{LastLaunch, SpawnSpec};
use ptyro_core::sync::MutexExt;

use crate::daemon::Daemon;

const KEY: &str = "last_launch";

impl Daemon {
    pub(crate) async fn load_last_launch(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get(KEY.into()).await
            && let Ok(m) = serde_json::from_value::<HashMap<String, LastLaunch>>(v)
        {
            self.last_launch.locked().extend(m);
        }
    }

    pub(crate) fn last_launch(&self, workspace: &str) -> Option<LastLaunch> {
        self.last_launch.locked().get(workspace).cloned()
    }

    /// Remember what `spec` started in `workspace`, unless it's a tool, a quick terminal or the
    /// scratchpad's editor.
    pub(crate) async fn remember_launch(&self, workspace: &str, spec: &SpawnSpec) {
        if spec.tool || spec.quick || spec.pad {
            return;
        }
        let launch = LastLaunch {
            agent: spec.agent.clone(),
            profile: spec.profile.clone(),
            argv: spec.argv.clone(),
        };
        let snapshot = {
            let mut all = self.last_launch.locked();
            if all.get(workspace) == Some(&launch) {
                return;
            }
            all.insert(workspace.to_owned(), launch);
            serde_json::to_value(&*all).unwrap_or_default()
        };
        if let Some(st) = &self.store {
            let _ = st.kv_set(KEY.into(), snapshot).await;
        }
    }

    /// `again`: fill in the agent (or shell, or command) its workspace last started, else the
    /// default agent. What `spec` names itself wins.
    pub(crate) async fn resolve_again(
        self: &Arc<Self>,
        mut spec: SpawnSpec,
    ) -> Result<SpawnSpec, Error> {
        spec.again = false;
        if spec.agent.is_some() || !spec.argv.is_empty() {
            return Ok(spec);
        }
        if spec.worktree.is_none() {
            self.auto_register(&spec.cwd, spec.workspace.as_deref()).await;
        }
        let target = self.target(spec.worktree.as_deref(), &spec.cwd)?;
        let workspace =
            self.home_workspace(spec.workspace.as_deref(), target.as_ref().map(|(_, p)| p))?;
        let last = self.last_launch(&workspace).unwrap_or_else(|| LastLaunch {
            agent: Some(ptyro_agents::default_agent(&self.config())),
            ..LastLaunch::default()
        });
        spec.agent = last.agent;
        spec.argv = last.argv;
        spec.profile = spec.profile.or(last.profile);
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
