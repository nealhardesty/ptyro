//! Worktrees kept from clean-up (SPEC §10.8): "Keep it" on a stale worktree, in `ptyro gc`, the
//! TUI's clean-up list (`C-k`) and the web's Clean up. `gc` stops offering a kept worktree until
//! something changes: it counts as unused again only `stale_days` after it was kept, and as merged
//! again only once its branch moves on. A directory that is gone is always reported. Kept in `kv`
//! across restarts; a removed worktree's mark goes with it.

use std::collections::HashMap;

use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, now_ms};
use ptyro_git::Git;
use serde::{Deserialize, Serialize};

use crate::daemon::Daemon;

/// Worktree id → when it was kept.
const KEY: &str = "gc_kept";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Kept {
    pub at_ms: u64,
    /// Its branch's tip then: merged stays quiet while the branch is still there.
    #[serde(default)]
    pub tip: Option<String>,
}

impl Daemon {
    pub(crate) async fn load_kept(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get(KEY.into()).await
            && let Ok(m) = serde_json::from_value::<HashMap<String, Kept>>(v)
        {
            self.kept.locked().extend(m);
        }
    }

    pub(crate) fn kept_of(&self, worktree: &str) -> Option<Kept> {
        self.kept.locked().get(worktree).cloned()
    }

    /// `GcKeep`: keep `addr` from clean-up, with what it is now.
    pub(crate) async fn keep_worktree(&self, addr: &str) -> Result<String, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid("the main checkout is never cleaned up"));
        }
        let tip = match (&wt.branch, self.project_by_id(&wt.project)) {
            (Some(b), Some(p)) => {
                let (root, b) = (p.root.clone(), format!("refs/heads/{b}"));
                tokio::task::spawn_blocking(move || Git::new().rev(&root, &b)).await.ok().flatten()
            }
            _ => None,
        };
        let days = self.config().worktrees.stale_days;
        self.set_kept(&wt.id, Some(Kept { at_ms: now_ms(), tip })).await;
        Ok(format!(
            "kept {}: clean-up leaves it be until it is unused for another {days} days, or its branch moves on",
            wt.qualified()
        ))
    }

    /// A removed worktree's mark goes with it.
    pub(crate) async fn forget_kept(&self, worktree: &str) {
        self.set_kept(worktree, None).await;
    }

    async fn set_kept(&self, worktree: &str, kept: Option<Kept>) {
        let snapshot = {
            let mut all = self.kept.locked();
            match kept {
                Some(k) => {
                    all.insert(worktree.to_owned(), k);
                }
                None if all.remove(worktree).is_none() => return,
                None => {}
            }
            serde_json::to_value(&*all).unwrap_or_default()
        };
        if let Some(st) = &self.store {
            let _ = st.kv_set(KEY.into(), snapshot).await;
        }
    }
}
