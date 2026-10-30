//! The quick terminal (UX1.25, UX2.3, UX3.4, SPEC §11.2h): a worktree's one shell, in a drawer (a
//! pane split off below one of its project's panes) or a full-sized panel of its own:
//! `shell.drawer` (`C-t`) and `shell.pane` (`T`); `shell.here` (⏎ on its line) as
//! `[ui] shell_style` says. `!` is not it: that starts another shell (`shell.new`).
//! A key shows it and focuses it, focuses it when it's shown, and hides it when it's focused. Hiding closes only the pane: the
//! daemon keeps the shell, so it comes back as it was.

use ptyro_core::SessionId;
use ptyro_core::config::{ShellStyle, key_label};
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

/// Where `tuck_away` compares: the workspace and the session in each pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stance {
    workspace: String,
    shown: Vec<(PaneId, Option<SessionId>)>,
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

    /// The key to show for hiding the quick terminal's drawer: `C-q C-t` while it's bound to it
    /// (`!` works too, but Ctrl-T is the one to learn), else whatever is.
    pub fn quick_key(&self) -> Option<String> {
        let ctrl_t = KeyEvent::new(KeyCode::Char { c: 't' }, Mods(Mods::CTRL));
        match self.keymap.lookup(Table::Prefix, ctrl_t) {
            Some(c) if c.action == "shell.drawer" || c.action == "shell.here" => {
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

    /// `shell.here` with the configured `[ui] shell_style`.
    pub fn toggle_quick(
        &mut self,
        want: Option<SessionId>,
        worktree: Option<String>,
        cwd: Option<std::path::PathBuf>,
    ) {
        self.toggle_quick_as(self.config.ui.shell_style, want, worktree, cwd);
    }

    /// The quick terminal of a worktree: hide it when it's the focused drawer, switch back
    /// when it's the focused panel; focus it when it's shown elsewhere; else show it `style`'s
    /// way (UX2.3). It is `want`, else the one of `worktree` or `cwd`, else of where you are
    /// (the selected row's worktree, the focused session's, else its directory). One is
    /// started when there's none.
    pub fn toggle_quick_as(
        &mut self,
        style: ShellStyle,
        want: Option<SessionId>,
        worktree: Option<String>,
        cwd: Option<std::path::PathBuf>,
    ) {
        let shown = self.drawer_session();
        if self.in_drawer() && style != ShellStyle::Pane && (want.is_none() || want == shown) {
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
            Some(id) => self.show_quick_existing(style, id),
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
                self.request(Pending::Quick(style), req);
            }
        }
    }

    /// The pane (not the drawer) showing the quick terminal `id` as a panel of its own.
    pub(crate) fn quick_panel(&self, id: SessionId) -> Option<PaneId> {
        let drawer = self.drawer.map(|d| d.pane);
        self.layout
            .panes()
            .into_iter()
            .filter(|p| Some(*p) != drawer)
            .find(|p| self.panes.get(p).is_some_and(|s| s.session == Some(id)))
    }

    fn show_quick_existing(&mut self, style: ShellStyle, id: SessionId) {
        if let Some(pane) = self.quick_panel(id) {
            if self.focus == Focus::Pane && self.focused == pane {
                // Pressed again in its own panel: back to what it replaced (UX3.1: the kept
                // layout, else the pane's previous session).
                if !self.go_back() {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.session = None;
                    }
                    self.view_changed();
                }
            } else {
                self.focused = pane;
                self.focus = Focus::Pane;
                self.sidebar_zoom = false;
                self.view_changed();
            }
        } else if let Some(d) = self.drawer.filter(|_| self.drawer_session() == Some(id)) {
            if style == ShellStyle::Pane {
                self.hide_drawer();
                self.open_panel(id);
            } else {
                self.focused = d.pane;
                self.focus = Focus::Pane;
                self.sidebar_zoom = false;
                self.view_changed();
            }
        } else {
            self.show_quick(style, id);
        }
    }

    /// Show the quick terminal `id`, which is not on screen: in a drawer under a pane of its
    /// own project when one is open, else as a panel of its own (UX2.3). `ShellStyle::Pane`
    /// is always the panel; `Drawer` says why when it falls back to it.
    pub fn show_quick(&mut self, style: ShellStyle, id: SessionId) {
        let host = self.session(id).cloned().and_then(|me| self.quick_host(&me));
        match (style, host) {
            (ShellStyle::Pane, _) | (_, None) => {
                if style == ShellStyle::Drawer {
                    self.toast(
                        "nothing of this project is open, so its shell has a panel of its own",
                    );
                }
                self.open_panel(id);
            }
            (_, Some(host)) => {
                self.focused = host;
                self.focus = Focus::Pane;
                self.open_drawer(id);
            }
        }
    }

    /// A pane (the focused one first, never the drawer) showing a session of the same project
    /// as the quick terminal `me`, which a drawer can hang from.
    fn quick_host(&self, me: &ptyro_core::session::SessionInfo) -> Option<PaneId> {
        let drawer = self.drawer.map(|d| d.pane);
        let mut panes = self.layout.panes();
        panes.retain(|p| Some(*p) != drawer);
        if let Some(i) = panes.iter().position(|p| *p == self.focused) {
            panes.swap(0, i);
        }
        panes.into_iter().find(|p| {
            self.panes
                .get(p)
                .and_then(|s| s.session)
                .filter(|s| *s != me.id)
                .and_then(|s| self.session(s))
                .is_some_and(|s| match (&s.project, &me.project) {
                    (Some(a), Some(b)) => a == b,
                    (None, None) => s.cwd == me.cwd,
                    _ => false,
                })
        })
    }

    /// Show `id` as a panel of its own: fill the focused pane (an empty one, or the one a
    /// drawer hangs from instead of the drawer), as `session.new` does. Pressing again goes back.
    fn open_panel(&mut self, id: SessionId) {
        if let Some(d) = self.drawer.filter(|d| d.pane == self.focused)
            && self.layout.contains(d.host)
        {
            self.focused = d.host;
        }
        self.show(id, None);
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
            self.toast_err("no room for the drawer: close a pane first");
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
            .is_some_and(|s| (s.quick || s.pad) && s.state.is_live());
        if self.drawer.is_some() && !live {
            self.hide_drawer();
        }
    }

    /// What is on screen, for `tuck_away`: the workspace, and what every pane shows.
    pub(crate) fn quick_stance(&self) -> Stance {
        let mut shown: Vec<_> = self.panes.iter().map(|(id, p)| (*id, p.session)).collect();
        shown.sort_by_key(|(id, _)| *id);
        Stance { workspace: self.workspace.clone(), shown }
    }

    /// The drawer and the scratchpad's popup are for quick actions: when another session or
    /// workspace shows, they go away (the shells and the editor keep running). Moving focus
    /// between panes that were already showing doesn't: the top agent and the drawer are used
    /// together. Focus in the sidebar doesn't count either: that is how you pick the next thing.
    pub(crate) fn tuck_away(&mut self, before: Stance) {
        let after = self.quick_stance();
        let drawer = self.drawer.map(|d| d.pane);
        let shows_new = after.workspace != before.workspace
            || after.shown.iter().any(|(id, s)| {
                Some(*id) != drawer && s.is_some() && !before.shown.contains(&(*id, *s))
            });
        if shows_new && self.drawer.is_some() {
            self.hide_drawer();
        }
        if shows_new && self.pad_popup.is_some() {
            self.pad_popup = None;
            self.sync();
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
