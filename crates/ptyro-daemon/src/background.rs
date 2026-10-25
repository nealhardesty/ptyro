//! The daemon's background work: repository status on file changes (debounced, SPEC §10.9),
//! listening ports per session (SPEC §10.7), and worktree locks following session lifetimes.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{WorktreeInfo, WorktreeState};
use ptyro_protocol::DomainEvent;

use crate::daemon::Daemon;

const STATUS_DEBOUNCE: Duration = Duration::from_millis(500);
const PORT_SCAN: Duration = Duration::from_secs(3);
/// Worktrees with live sessions are re-checked this often even without file events.
const ACTIVE_REFRESH: Duration = Duration::from_secs(10);
const IDLE_REFRESH: Duration = Duration::from_secs(60);

pub type Watcher = notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>;

impl Daemon {
    /// [`watch_worktree`](Self::watch_worktree) on a blocking thread: registering a recursive
    /// watch on a big tree takes a while and must not stall the runtime.
    pub fn watch_worktree_bg(self: &Arc<Self>, wt: &WorktreeInfo) {
        let (me, wt) = (Arc::clone(self), wt.clone());
        tokio::task::spawn_blocking(move || me.watch_worktree(&wt));
    }

    /// Refresh status on file changes in the worktree (recursive; `.git/objects` churn ignored).
    pub fn watch_worktree(&self, wt: &WorktreeInfo) {
        if !self.config().worktrees.watch || wt.state != WorktreeState::Ready {
            return;
        }
        if self.watchers.lock().expect("watchers").contains_key(&wt.id) {
            return;
        }
        // Registering a recursive watch can take seconds on a big tree: not under the lock.
        let (tx, id) = (self.status_tx.clone(), wt.id.clone());
        let debouncer = notify_debouncer_mini::new_debouncer(
            STATUS_DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res
                    && events.iter().any(|e| {
                        let p = e.path.to_string_lossy();
                        !p.contains("/.git/objects/")
                            && !p.contains("/.git/logs/")
                            && !p.ends_with(".lock")
                    })
                {
                    let _ = tx.send(id.clone());
                }
            },
        );
        let mut debouncer = match debouncer {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!("watch unavailable: {e}");
                return;
            }
        };
        if let Err(e) = debouncer.watcher().watch(&wt.path, notify::RecursiveMode::Recursive) {
            // Too many directories for inotify, usually: periodic refresh still covers it.
            tracing::warn!(worktree = %wt.qualified(), "cannot watch ({e}); refreshing periodically instead");
            return;
        }
        self.watchers.lock().expect("watchers").entry(wt.id.clone()).or_insert(debouncer);
    }

    pub fn unwatch(&self, id: &str) {
        self.watchers.lock().expect("watchers").remove(id);
    }

    pub fn unwatch_project(&self, project: &str) {
        let ids: Vec<String> = {
            let reg = self.registry.lock().expect("registry");
            reg.worktrees.iter().filter(|w| w.project == project).map(|w| w.id.clone()).collect()
        };
        for id in ids {
            self.unwatch(&id);
        }
    }

    /// Start the background tasks. Call once, inside the runtime.
    pub fn start_background(self: &Arc<Self>) {
        let ready: Vec<WorktreeInfo> = self
            .registry
            .lock()
            .expect("registry")
            .worktrees
            .iter()
            .filter(|w| w.state == WorktreeState::Ready)
            .cloned()
            .collect();
        for w in &ready {
            self.watch_worktree_bg(w);
        }
        if let Some(mut rx) = self.status_rx.lock().expect("status_rx").take() {
            let me = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(first) = rx.recv().await {
                    // Coalesce a burst (a build touching many files) into one refresh per worktree.
                    let mut ids: HashSet<String> = HashSet::from([first]);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    while let Ok(id) = rx.try_recv() {
                        ids.insert(id);
                    }
                    for id in ids {
                        me.refresh_status(&id).await;
                    }
                }
            });
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut events = me.events.subscribe();
            loop {
                match events.recv().await {
                    Ok(
                        DomainEvent::SessionExited { info }
                        | DomainEvent::SessionCreated { info }
                        | DomainEvent::SessionRestarted { info },
                    ) => {
                        if let Some(w) = info.worktree {
                            me.sync_locks().await;
                            let _ = me.status_tx.send(w);
                        }
                    }
                    Ok(DomainEvent::SessionRemoved { .. }) => me.sync_locks().await,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        me.sync_locks().await
                    }
                    Err(_) => return,
                }
            }
        });
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(PORT_SCAN);
            let mut since_active = Duration::ZERO;
            let mut since_idle = Duration::ZERO;
            loop {
                tick.tick().await;
                me.scan_ports().await;
                since_active += PORT_SCAN;
                since_idle += PORT_SCAN;
                let (active, idle) = (since_active >= ACTIVE_REFRESH, since_idle >= IDLE_REFRESH);
                if active || idle {
                    let ids: Vec<String> = {
                        let reg = me.registry.lock().expect("registry");
                        reg.worktrees.iter().map(|w| w.id.clone()).collect()
                    };
                    for id in ids {
                        let busy = me.sessions_in(&id).iter().any(|s| s.is_live());
                        if idle || busy {
                            let _ = me.status_tx.send(id);
                        }
                    }
                    if active {
                        since_active = Duration::ZERO;
                    }
                    if idle {
                        since_idle = Duration::ZERO;
                    }
                }
            }
        });
    }

    /// Update every live session's listening ports; announce changes.
    async fn scan_ports(self: &Arc<Self>) {
        let live: Vec<(Arc<crate::session::Session>, u32)> = self
            .sessions()
            .into_iter()
            .filter(|s| s.is_live())
            .filter_map(|s| s.pid().map(|p| (s, p)))
            .collect();
        if live.is_empty() {
            return;
        }
        let pids: Vec<u32> = live.iter().map(|(_, p)| *p).collect();
        let Ok(found) =
            tokio::task::spawn_blocking(move || ptyro_pty::listening_ports(&pids)).await
        else {
            return;
        };
        for (s, pid) in live {
            let ports = found.get(&pid).cloned().unwrap_or_default();
            let changed = {
                let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let changed = m.ports != ports;
                m.ports = ports;
                changed
            };
            if changed {
                let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
            }
        }
    }
}
