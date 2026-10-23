//! The quick terminal's drawer (UX1.25, SPEC §11.2h): the worktree's one shell, in a pane split
//! off below the focused one. One key (`shell.here`: `C-q C-t`, `!`) shows it and focuses it,
//! focuses it when it's shown, and hides it when it's focused. Hiding closes only the pane: the
//! daemon keeps the shell, so it comes back as it was.

use ptyro_core::SessionId;
use ptyro_core::config::key_label;
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_protocol::Request;
use serde::{Deserialize, Serialize};

use crate::app::{App, Focus, PaneState, Pending};
use crate::keymap::Table;
use crate::layout::{Dir, MAX_WITH_DRAWER, PaneId};

/// The drawer's share of its host's height when first opened.
pub const DRAWER_SHARE: f32 = 0.3;

/// The open drawer: `pane`, split off below `host`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drawer {
    pub host: PaneId,
    pub pane: PaneId,
}

impl App {
    /// Whether the focused pane is the drawer.
    pub fn in_drawer(&self) -> bool {
        self.focus == Focus::Pane && self.drawer.is_some_and(|d| d.pane == self.focused)
    }

    /// The session the drawer shows.
    pub fn drawer_session(&self) -> Option<SessionId> {
        self.drawer.and_then(|d| self.panes.get(&d.pane)).and_then(|p| p.session)
    }

    /// The key to show for the quick terminal: `C-q C-t` while it's bound to it (`!` works too,
    /// but Ctrl-T is the one to learn), else whatever is.
    pub fn quick_key(&self) -> Option<String> {
        let ctrl_t = KeyEvent::new(KeyCode::Char { c: 't' }, Mods(Mods::CTRL));
        match self.keymap.lookup(Table::Prefix, ctrl_t) {
            Some(c) if c.action == "shell.here" => {
                Some(format!("{} {}", key_label(&self.keymap.prefix), key_label(&ctrl_t)))
            }
            _ => self.keymap.key_for("shell.here"),
        }
    }

    /// The live quick terminal of a worktree (by id), or of a directory outside any.
    pub fn quick_of(&self, worktree: Option<&str>, cwd: &std::path::Path) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|s| {
                s.quick
                    && s.state.is_live()
                    && match worktree {
                        Some(w) => s.worktree.as_deref() == Some(w),
                        None => s.worktree.is_none() && s.cwd == cwd,
                    }
            })
            .map(|s| s.id)
    }

    /// `shell.here`: hide the drawer when it's focused; else show (or focus) the quick terminal
    /// `want`, else the one of `worktree` or `cwd`, else of where you are (the selected row's
    /// worktree, the focused session's, else its directory). One is started when there's none.
    pub fn toggle_quick(
        &mut self,
        want: Option<SessionId>,
        worktree: Option<String>,
        cwd: Option<std::path::PathBuf>,
    ) {
        let shown = self.drawer_session();
        if self.in_drawer() && (want.is_none() || want == shown) {
            self.hide_drawer();
            return;
        }
        let (worktree, cwd) = match (worktree, cwd) {
            (None, None) => match self.context().1 {
                Some(w) => (Some(w), Default::default()),
                None => (None, self.here()),
            },
            (w, c) => (w, c.unwrap_or_default()),
        };
        let want = want.or_else(|| self.quick_of(worktree.as_deref(), &cwd));
        match want {
            Some(id) if shown == Some(id) => {
                if let Some(d) = self.drawer {
                    self.focused = d.pane;
                    self.focus = Focus::Pane;
                    self.sidebar_zoom = false;
                    self.view_changed();
                }
            }
            Some(id) => self.open_drawer(id),
            None => {
                let cwd = if cwd.as_os_str().is_empty() && worktree.is_none() {
                    std::env::current_dir().unwrap_or_else(|_| "/".into())
                } else {
                    cwd
                };
                let req = Request::QuickShell {
                    worktree,
                    cwd,
                    create: true,
                    env: Self::client_env(),
                    size: None,
                    workspace: Some(self.workspace.clone()),
                };
                self.request(Pending::Quick, req);
            }
        }
    }

    /// Show `id` in the drawer under the focused pane (the one the drawer hangs from, when the
    /// drawer is focused), replacing a drawer that shows something else, and focus it.
    pub fn open_drawer(&mut self, id: SessionId) {
        let host = match self.drawer {
            Some(d) if d.pane == self.focused => d.host,
            _ => self.focused,
        };
        self.hide_drawer();
        let host = if self.layout.contains(host) { host } else { self.layout.first() };
        let Some(pane) =
            self.layout.split_at(host, Dir::Down, 1.0 - self.drawer_share, MAX_WITH_DRAWER)
        else {
            return;
        };
        self.panes.insert(pane, PaneState { session: Some(id), ..PaneState::default() });
        self.drawer = Some(Drawer { host, pane });
        self.focused = pane;
        self.focus = Focus::Pane;
        self.sidebar_zoom = false;
        self.view_changed();
    }

    /// Close the drawer's pane, keeping its height for next time. Focus goes back to its host.
    pub fn hide_drawer(&mut self) {
        let Some(d) = self.drawer.take() else { return };
        if let Some(share) = self.layout.below_share(d.pane) {
            self.drawer_share = share.clamp(0.1, 0.9);
        }
        if self.layout.close(d.pane) {
            self.panes.remove(&d.pane);
        }
        if self.focused == d.pane || !self.layout.contains(self.focused) {
            self.focused = if self.layout.contains(d.host) { d.host } else { self.layout.first() };
        }
        self.view_changed();
    }

    /// A saved view's drawer, once its panes are back: kept when it still shows its quick
    /// terminal, else closed (the shell is gone: a daemon restart), never filled with another.
    pub(crate) fn restore_drawer(&mut self, drawer: Option<Drawer>, share: f32) {
        self.drawer_share = share.clamp(0.1, 0.9);
        self.drawer = drawer;
        self.check_drawer();
        let live = self
            .drawer_session()
            .and_then(|id| self.session(id))
            .is_some_and(|s| s.quick && s.state.is_live());
        if self.drawer.is_some() && !live {
            self.hide_drawer();
        }
    }

    /// Forget a drawer whose panes are gone or that no longer hangs below its host (a view
    /// restored from elsewhere, a pane closed under it).
    pub(crate) fn check_drawer(&mut self) {
        if let Some(d) = self.drawer
            && !(self.layout.contains(d.host) && self.layout.below_share(d.pane).is_some())
        {
            self.drawer = None;
        }
    }
}
