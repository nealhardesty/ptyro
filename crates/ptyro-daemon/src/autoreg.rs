//! The repository you're in is a project (ROADMAP UX1.5, SPEC §10.1): a session whose directory
//! is in a git repository that isn't registered registers it, in the session's workspace. At
//! spawn, when a shell `cd`s into one, and once for the sessions already running when the daemon
//! starts. A repository you forgot (`project rm`, or undoing this) is remembered and never
//! registered on its own again; adding it by hand lifts that.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::SessionId;
use ptyro_core::model::{MAIN_WORKSPACE, ProjectInfo, WorktreeState};
use ptyro_git::Git;
use ptyro_protocol::{DomainEvent, Reply};

use crate::daemon::Daemon;
use crate::session::Session;

/// The kv key of the repositories never to register on their own (their roots).
const DECLINED: &str = "autoreg:declined";
/// How often running sessions' directories are looked at for a new repository.
const SWEEP: Duration = Duration::from_secs(2);

impl Daemon {
    pub(crate) async fn load_declined(&self) {
        if let Some(store) = &self.store
            && let Ok(Some(v)) = store.kv_get(DECLINED.into()).await
            && let Ok(list) = serde_json::from_value::<Vec<PathBuf>>(v)
        {
            *self.declined.lock().expect("declined") = list.into_iter().collect();
        }
    }

    fn save_declined(&self) {
        let list: Vec<PathBuf> = self.declined.lock().expect("declined").iter().cloned().collect();
        self.save_kv(DECLINED, serde_json::to_value(list).unwrap_or_default());
    }

    /// Never register `root` on its own again.
    pub(crate) fn decline(&self, root: &Path) {
        if self.declined.lock().expect("declined").insert(root.to_path_buf()) {
            self.save_declined();
        }
    }

    pub(crate) fn undecline(&self, root: &Path) {
        if self.declined.lock().expect("declined").remove(root) {
            self.save_declined();
        }
    }

    /// The workspace the TUI showed last (its `_tui` view), else `main`: where a project added
    /// without `--workspace` goes.
    pub(crate) async fn last_workspace(&self) -> String {
        let shown = match &self.store {
            Some(store) => store.kv_get("view:_tui".into()).await.ok().flatten(),
            None => self.views.lock().expect("views").get("_tui").cloned(),
        };
        let name = shown.and_then(|v| v["workspace"].as_str().map(str::to_owned));
        let reg = self.registry.lock().expect("registry");
        name.filter(|n| reg.workspaces.iter().any(|w| w.name == *n))
            .unwrap_or_else(|| MAIN_WORKSPACE.into())
    }

    /// Whether a registered worktree (any state but removed) holds `dir`.
    fn in_a_project(&self, dir: &Path) -> bool {
        let reg = self.registry.lock().expect("registry");
        let known = reg.worktrees.iter().filter(|w| w.state != WorktreeState::Removed);
        crate::registry::innermost(known, dir).is_some()
    }

    /// Register the repository `dir` is in, in `workspace` (else the last one shown), when
    /// that's wanted: `[discover] auto_register`, a repository that isn't a project, isn't your
    /// home directory (a dotfiles repository would claim everything) and wasn't declined.
    /// Tells clients (`ProjectAutoAdded`) and returns the project.
    pub(crate) async fn auto_register(
        self: &Arc<Self>,
        dir: &Path,
        workspace: Option<&str>,
    ) -> Option<ProjectInfo> {
        if !self.config().discover.auto_register {
            return None;
        }
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if self.in_a_project(&dir) {
            return None;
        }
        let probe = dir.clone();
        let root = tokio::task::spawn_blocking(move || Git::new().main_root(&probe).ok())
            .await
            .ok()
            .flatten()?;
        let root = root.canonicalize().unwrap_or(root);
        let home = std::env::var_os("HOME").map(PathBuf::from).and_then(|h| h.canonicalize().ok());
        if home.as_deref() == Some(root.as_path())
            || self.declined.lock().expect("declined").contains(&root)
            || root.starts_with(&self.paths.data)
        {
            return None;
        }
        let workspace = match workspace {
            Some(w) if self.find_workspace(w).is_ok() => w.to_owned(),
            _ => self.last_workspace().await,
        };
        match self.project_add(root, Some(workspace.clone())).await {
            Ok(Reply::Project { info, notes }) => {
                tracing::info!("registered {} ({}) on its own", info.slug, info.root.display());
                let _ = self.events.send(DomainEvent::ProjectAutoAdded {
                    info: info.clone(),
                    workspace,
                    notes,
                });
                Some(info)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("auto-register {}: {}", dir.display(), e.message);
                None
            }
        }
    }

    /// Put a session under the worktree holding `dir` (`None`: outside every project), and tell
    /// clients. Its profile stays: it's fixed for the session's lifetime (SPEC §10.10).
    pub(crate) fn place_session(&self, s: &Arc<Session>, dir: Option<&Path>) {
        let target = dir.and_then(|d| self.target(None, d).ok().flatten());
        {
            let mut m = s.meta.lock().expect("meta");
            let (worktree, project) = match &target {
                Some((w, p)) => (Some(w.id.clone()), Some(p.id.clone())),
                None => (None, None),
            };
            if m.place.worktree == worktree && m.place.project == project {
                return;
            }
            m.place.worktree = worktree;
            m.place.project = project;
        }
        s.save(None);
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
    }

    /// A session outside every project, in a repository: register it (unless it is a project
    /// already) and move the session in.
    async fn adopt_dir(self: &Arc<Self>, s: &Arc<Session>, dir: &Path) {
        let ws = s.meta.lock().expect("meta").place.workspace.clone();
        if self.in_a_project(dir) || self.auto_register(dir, ws.as_deref()).await.is_some() {
            self.place_session(s, Some(dir));
        }
    }

    /// Where a session is now: the shell's report (OSC 7), else its foreground process's
    /// directory, else where it started.
    fn current_dir(&self, s: &Arc<Session>) -> PathBuf {
        let info = self.info(s);
        info.live_cwd.unwrap_or(info.cwd)
    }

    /// Once at start (the sessions an earlier daemon, or a live upgrade, left running), then
    /// every few seconds for the directories sessions moved to.
    pub(crate) fn start_auto_register(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut seen: HashSet<(SessionId, PathBuf)> = HashSet::new();
            loop {
                if *me.shutdown.borrow() {
                    return;
                }
                if me.config().discover.auto_register {
                    for s in me.sessions().into_iter().filter(|s| s.is_live()) {
                        let dir = me.current_dir(&s);
                        if !seen.insert((s.id, dir.clone())) {
                            continue;
                        }
                        let placed = s.meta.lock().expect("meta").place.project.is_some();
                        if placed {
                            // Elsewhere now: its new repository joins the sidebar too.
                            if !me.in_a_project(&dir) {
                                let ws = s.meta.lock().expect("meta").place.workspace.clone();
                                me.auto_register(&dir, ws.as_deref()).await;
                            }
                        } else {
                            me.adopt_dir(&s, &dir).await;
                        }
                    }
                }
                tokio::time::sleep(SWEEP).await;
            }
        });
    }
}
