//! The quick terminal (UX1.25, SPEC §11.2h): one shell per worktree, or per directory outside
//! one, that `QuickShell` starts on first use and hands back after. It runs under the
//! worktree's profile like any session there, but it's quiet (`raise` lets only a blocked agent
//! started in it through), it never holds up removing its worktree while at its prompt
//! (`Session::busy`), and `exit` in it forgets it, so the next one starts fresh.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ptyro_core::session::SpawnSpec;
use ptyro_core::{Error, ErrorCode, TermSize};
use ptyro_protocol::Reply;

use crate::daemon::Daemon;
use crate::session::Session;

/// One start at a time, so two clients asking at once share one shell.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Daemon {
    /// `QuickShell`: the quick terminal of `worktree` (an address), else of the worktree `cwd`
    /// is in, else of `cwd` itself. Started when there's none and `create` is set.
    pub(crate) async fn quick_shell(
        self: &Arc<Self>,
        worktree: Option<String>,
        cwd: PathBuf,
        create: bool,
        env: BTreeMap<String, String>,
        size: Option<TermSize>,
        workspace: Option<String>,
    ) -> Result<Reply, Error> {
        if create && worktree.is_none() {
            // As `new` does: the repository you're in shows up in the sidebar (UX1.5).
            self.auto_register(&cwd, workspace.as_deref()).await;
        }
        let place = self.target(worktree.as_deref(), &cwd)?;
        let wt = place.as_ref().map(|(w, _)| w.id.clone());
        let _one = if create { Some(STARTING.lock().await) } else { None };
        if let Some(s) = self.quick_of(wt.as_deref(), &cwd) {
            return Ok(Reply::Session { info: self.info(&s) });
        }
        if !create {
            let here = match &place {
                Some((w, _)) => w.qualified(),
                None => cwd.display().to_string(),
            };
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("no quick terminal in {here} yet; `ptyro shell` starts one"),
            ));
        }
        let base = match &place {
            Some((w, p)) if w.is_root => p.slug.clone(),
            Some((w, _)) => w.name.clone(),
            None => dir_name(&cwd),
        };
        let spec = SpawnSpec {
            worktree: wt,
            cwd,
            name: Some(self.free_name(&format!("{base}.shell"))),
            env,
            size,
            workspace,
            quick: true,
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// The live quick terminal of a worktree (by id), or of a directory outside any.
    fn quick_of(&self, worktree: Option<&str>, cwd: &Path) -> Option<Arc<Session>> {
        self.sessions().into_iter().find(|s| {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.place.quick
                && m.state.is_live()
                && match worktree {
                    Some(w) => m.place.worktree.as_deref() == Some(w),
                    None => m.place.worktree.is_none() && m.cwd == cwd,
                }
        })
    }
}

/// A name from a directory's last component (`~/notes` → `notes`), `home` when there's none.
fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|n| ptyro_core::names::from_text(&n.to_string_lossy()))
        .unwrap_or_else(|| "home".into())
}
