//! Full-sized panels and the kept layout (UX3.1, SPEC §11.2i). Only a split command (`v`, `s`)
//! puts something in a split, and what it puts there is a clone of the session it was split from.
//! Anything else that opens (a new session, the quick terminal's panel, a tool's output, a
//! session picked in the sidebar) opens full sized. A split layout it replaces is kept whole:
//! `` ` `` brings it back, and so does opening one of its sessions.

use std::collections::HashMap;

use ptyro_core::SessionId;
use ptyro_core::session::{SessionKind, SpawnSpec};
use ptyro_protocol::Request;
use serde::{Deserialize, Serialize};

use crate::app::{App, Focus, PaneState, Pending};
use crate::layout::{Dir, Layout, MAX_PANES, PaneId};
use crate::quick::Drawer;
use crate::tree::Row;

/// A whole screen of panes: what `` ` `` swaps with the one showing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub layout: Layout,
    pub panes: HashMap<PaneId, Option<SessionId>>,
    pub focus: PaneId,
    #[serde(default)]
    pub zoom: bool,
    #[serde(default)]
    pub drawer: Option<Drawer>,
}

impl Screen {
    pub fn shows(&self, id: SessionId) -> Option<PaneId> {
        let drawer = self.drawer.map(|d| d.pane);
        self.panes
            .iter()
            .filter(|(p, _)| Some(**p) != drawer)
            .find(|(_, s)| **s == Some(id))
            .map(|(p, _)| *p)
            .or_else(|| self.panes.iter().find(|(_, s)| **s == Some(id)).map(|(p, _)| *p))
    }

    /// Drop the panes showing `id` (their siblings take the space); `false` when nothing is left
    /// worth going back to.
    pub fn forget(&mut self, id: SessionId) -> bool {
        let gone: Vec<PaneId> =
            self.panes.iter().filter(|(_, s)| **s == Some(id)).map(|(p, _)| *p).collect();
        for p in gone {
            // The drawer goes with its host, and is gone itself when it showed `id`.
            if let Some(d) = self.drawer.filter(|d| d.pane == p || d.host == p) {
                self.drawer = None;
                self.layout.close(d.pane);
                self.panes.remove(&d.pane);
                if d.pane == p {
                    continue;
                }
            }
            if self.layout.close(p) {
                self.panes.remove(&p);
            } else {
                self.panes.insert(p, None);
            }
        }
        if !self.layout.contains(self.focus) {
            self.focus = self.layout.first();
        }
        self.panes.values().any(Option::is_some)
    }
}

impl App {
    /// The panes on screen besides the drawer: more than one is a split.
    pub fn is_split(&self) -> bool {
        let drawer = self.drawer.map(|d| d.pane);
        self.layout.panes().into_iter().filter(|p| Some(*p) != drawer).count() > 1
    }

    pub(crate) fn snapshot(&self) -> Screen {
        Screen {
            layout: self.layout.clone(),
            panes: self.panes.iter().map(|(k, p)| (*k, p.session)).collect(),
            focus: self.focused,
            zoom: self.zoom,
            drawer: self.drawer,
        }
    }

    fn apply_screen(&mut self, s: Screen) {
        self.layout = s.layout;
        self.panes = self
            .layout
            .panes()
            .into_iter()
            .map(|p| {
                let session = s.panes.get(&p).copied().flatten();
                (p, PaneState { session, ..PaneState::default() })
            })
            .collect();
        self.focused = if self.layout.contains(s.focus) { s.focus } else { self.layout.first() };
        self.zoom = s.zoom;
        self.drawer = s.drawer;
        self.check_drawer();
    }

    /// Put `id` on screen without a split (UX3.1), and return its pane: where it already shows,
    /// else back in the kept layout that shows it, else in the one pane there is, else full
    /// sized in place of the split, which is kept.
    pub(crate) fn place(&mut self, id: SessionId) -> PaneId {
        if let Some(p) = self.snapshot().shows(id) {
            return p;
        }
        if let Some(p) = self.kept.as_ref().and_then(|k| k.shows(id)) {
            self.swap_screens();
            return p;
        }
        if self.is_split() {
            self.kept = Some(Box::new(self.snapshot()));
            self.apply_screen(Screen {
                layout: Layout::default(),
                panes: HashMap::from([(1, None)]),
                focus: 1,
                zoom: false,
                drawer: None,
            });
            return 1;
        }
        // One pane (and maybe its drawer): the session takes the pane, never the drawer.
        match self.drawer {
            Some(d) if d.pane == self.focused && self.layout.contains(d.host) => d.host,
            _ => self.focused,
        }
    }

    /// Show the kept layout, keeping the screen it replaces in its place.
    pub(crate) fn swap_screens(&mut self) {
        let Some(kept) = self.kept.take() else { return };
        let now = self.snapshot();
        self.apply_screen(*kept);
        self.kept = now.panes.values().any(Option::is_some).then(|| Box::new(now));
    }

    /// `` ` ``: the kept layout, else the session this pane showed before. False when there's
    /// neither.
    pub(crate) fn go_back(&mut self) -> bool {
        if self.kept.is_some() {
            self.swap_screens();
            self.focus = Focus::Pane;
            if let Some(id) = self.focused_session() {
                self.reveal(id);
            }
            self.view_changed();
            return true;
        }
        match self
            .panes
            .get(&self.focused)
            .and_then(|p| p.last)
            .filter(|id| self.session(*id).is_some())
        {
            Some(id) => {
                self.show(id, None);
                true
            }
            None => false,
        }
    }

    /// A session left: the kept layout drops its panes, and goes when nothing is left in it.
    pub(crate) fn forget_kept(&mut self, id: SessionId) {
        if let Some(k) = self.kept.as_mut()
            && !k.forget(id)
        {
            self.kept = None;
        }
    }

    /// A saved view's kept layout, without the sessions that are gone.
    pub(crate) fn restore_kept(&mut self, kept: Option<Screen>, known: &[SessionId]) {
        self.kept = kept.and_then(|mut k| {
            k.layout = k.layout.sanitize();
            let gone: Vec<SessionId> =
                k.panes.values().flatten().filter(|id| !known.contains(id)).copied().collect();
            let mut left = k.panes.values().any(Option::is_some);
            for id in gone {
                left = k.forget(id);
            }
            left.then(|| Box::new(k))
        });
    }

    /// A restored view's empty panes: a split's close (only a split command fills a split); the
    /// one pane shows a session that isn't on screen.
    pub(crate) fn settle_empty_panes(&mut self, ordered: &[SessionId]) {
        let drawer = self.drawer.map(|d| d.pane);
        for pane in self.layout.panes() {
            if Some(pane) == drawer || self.panes.get(&pane).is_none_or(|p| p.session.is_some()) {
                continue;
            }
            if self.is_split() {
                self.close_pane(pane);
                continue;
            }
            let shown: Vec<SessionId> = self.panes.values().filter_map(|p| p.session).collect();
            if let Some(id) = ordered.iter().find(|id| !shown.contains(id)) {
                self.panes.entry(pane).or_default().session = Some(*id);
            }
        }
    }

    /// Make `id`'s row visible and select it (UX3.3): its project and worktree unfold. A row
    /// the tree can't place yet (its project's news still on the way) is placed when it can.
    pub(crate) fn reveal(&mut self, id: SessionId) {
        let Some(s) = self.session(id) else { return };
        for node in [s.project.clone(), s.worktree.clone()].into_iter().flatten() {
            self.collapsed.remove(&node);
        }
        let rows = self.rows();
        match rows.iter().position(|r| r.session() == Some(id)) {
            Some(i) => {
                self.set_sel(&rows, i);
                self.revealing = None;
            }
            None => self.revealing = Some((id, std::time::Instant::now())),
        }
    }

    /// A worktree's row, unfolded and selected (a new worktree with nothing in it yet).
    pub(crate) fn reveal_worktree(&mut self, id: &str, project: &str) {
        self.collapsed.remove(project);
        self.collapsed.remove(id);
        let rows = self.rows();
        if let Some(i) = rows.iter().position(|r| *r == Row::Worktree(id.to_owned())) {
            self.set_sel(&rows, i);
        }
    }

    /// Try a pending reveal again (news arrived), for a few seconds.
    pub(crate) fn retry_reveal(&mut self) {
        if let Some((id, since)) = self.revealing {
            if since.elapsed() > std::time::Duration::from_secs(5) {
                self.revealing = None;
            } else {
                self.reveal(id);
            }
        }
    }

    /// A tool (an editor, a `ptyro …` command) full sized in the focused session's directory,
    /// not remembered for quick-new; `back`: its screen goes back to what it replaced when it
    /// exits.
    pub(crate) fn spawn_tool(&mut self, argv: Vec<String>, name: Option<String>, back: bool) {
        let spec = SpawnSpec { argv, cwd: self.here(), name, tool: true, ..SpawnSpec::default() };
        self.spawn_spec_as(spec, None, back);
    }

    /// `v` / `s` (UX3.1): split the focused pane and start a clone of what it shows there (the
    /// selected session in the sidebar): the same agent and profile in the same worktree, a
    /// fresh conversation; a shell (or anything else) gets a shell in its directory.
    pub(crate) fn split_clone(&mut self, dir: Dir) {
        if self.drawer.is_some_and(|d| d.pane == self.focused) {
            self.toast_err("the drawer can't be split: focus another pane first");
            return;
        }
        if self.layout.panes().len() - usize::from(self.drawer.is_some()) >= MAX_PANES {
            self.toast_err(format!(
                "at most {MAX_PANES} panes: close one to open another (it is in the sidebar)"
            ));
            return;
        }
        let source = match (self.focus, self.selected_row()) {
            (Focus::Sidebar, Some(Row::Session(id) | Row::Quick(id))) => Some(id),
            _ => self.focused_session(),
        };
        let spec = match source.and_then(|id| self.session(id)) {
            Some(s) if s.kind == SessionKind::Agent && s.agent.is_some() => SpawnSpec {
                agent: s.agent.clone(),
                profile: Some(s.profile.clone()),
                worktree: s.worktree.clone(),
                cwd: s.cwd.clone(),
                // It runs where its source runs, under the same profile: agreed to already.
                override_pin: true,
                confirmed: true,
                ..SpawnSpec::default()
            },
            Some(s) => SpawnSpec {
                cwd: s.live_cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(|| s.cwd.clone()),
                ..SpawnSpec::default()
            },
            None => SpawnSpec { cwd: self.here(), ..SpawnSpec::default() },
        };
        self.spawn_spec(spec, Some(dir));
    }

    /// `shell.new` (`!`, UX3.4): another shell, of its own, in the context's worktree (else
    /// where you are), full sized. The worktree's quick terminal is `C-t` / `T`.
    pub(crate) fn new_shell(&mut self) {
        let (_, worktree) = self.context();
        let cwd = if worktree.is_some() { Default::default() } else { self.here() };
        self.spawn_spec(SpawnSpec { worktree, cwd, ..SpawnSpec::default() }, None);
    }

    // ── quick-new (UX3.2) ───────────────────────────────────────────────────

    /// Ask what quick-new would start in this workspace (for its label).
    pub(crate) fn refresh_again(&mut self) {
        let ws = self.workspace.clone();
        self.request(Pending::Again(ws.clone()), Request::LastLaunch { workspace: ws });
    }

    /// What quick-new starts, for people: `claude`, `shell`.
    pub fn again_label(&self) -> String {
        self.again.as_ref().map_or_else(
            || ptyro_agents::default_agent(&self.config),
            ptyro_core::session::LastLaunch::label,
        )
    }

    /// `session.again` (`C-q C-n`): another of what this workspace last started, in the
    /// context's worktree (the selected row's, the focused session's), no questions asked. It
    /// opens full sized, like any new session.
    pub(crate) fn quick_new(&mut self) {
        let (_, worktree) = self.context();
        let cwd = if worktree.is_some() { Default::default() } else { self.here() };
        self.spawn_spec(SpawnSpec { again: true, worktree, cwd, ..SpawnSpec::default() }, None);
    }
}
