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

use crate::app::{App, Focus, Overlay, PaneState, Pending, Purpose};
use crate::layout::{Dir, Layout, MAX_PANES, PaneId};
use crate::palette::{Item, Palette, Target};
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

    /// The session a way back (`` ` ``) returns to: the focused one, or, when the quick
    /// terminal's drawer has focus, the session of the pane it hangs under (the drawer is that
    /// pane's, not a place of its own).
    fn way_back(&self) -> Option<SessionId> {
        match self.drawer {
            Some(d) if d.pane == self.focused => self.panes.get(&d.host).and_then(|p| p.session),
            _ => self.focused_session(),
        }
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
        // A pane of a SHELLS window: the whole window, as the daemon keeps it (SPEC §23.17).
        if let Some(screen) = self.window_screen(id) {
            let p = screen.focus;
            self.apply_window(screen);
            return p;
        }
        if self.is_split() {
            let back = self.way_back();
            self.kept = Some(Box::new(self.snapshot()));
            self.apply_screen(Screen {
                layout: Layout::default(),
                panes: HashMap::from([(1, None)]),
                focus: 1,
                zoom: false,
                drawer: None,
            });
            self.panes.entry(1).or_default().last = back;
            return 1;
        }
        // One pane (and maybe its drawer): the session takes the pane, never the drawer.
        match self.drawer {
            Some(d) if d.pane == self.focused && self.layout.contains(d.host) => d.host,
            _ => self.focused,
        }
    }

    /// Where a session you picked goes (the sidebar, `/`, `1`–`9`, `j`/`k`, `` ` ``): as
    /// [`Self::place`], except that in a split of your own it takes the focused pane (the one a
    /// drawer hangs from, when the drawer has focus), so the split stays a split. A SHELLS
    /// window's screen is the daemon's, so a session from elsewhere still opens full sized there.
    pub(crate) fn place_picked(&mut self, id: SessionId) -> PaneId {
        let window_here =
            self.panes.values().filter_map(|p| p.session).any(|s| self.window_screen(s).is_some());
        let elsewhere = self.snapshot().shows(id).is_some()
            || self.kept.as_ref().is_some_and(|k| k.shows(id).is_some())
            || self.window_screen(id).is_some();
        if !self.is_split() || window_here || elsewhere {
            return self.place(id);
        }
        match self.drawer {
            Some(d) if d.pane == self.focused && self.layout.contains(d.host) => d.host,
            _ => self.focused,
        }
    }

    /// Show a SHELLS window's screen in place of this one, which is kept when it shows anything
    /// (`` ` `` brings it back) and isn't the same window.
    pub(crate) fn apply_window(&mut self, screen: Screen) {
        let back = self.way_back().filter(|id| screen.shows(*id).is_none());
        let now = self.snapshot();
        // The quick terminal's drawer isn't the window's: it stays open, under the same pane.
        let drawer = self.drawer.and_then(|d| {
            let quick = self.panes.get(&d.pane)?.session?;
            let host = self.panes.get(&d.host)?.session?;
            Some((quick, host))
        });
        let same = now
            .panes
            .iter()
            .filter(|(p, _)| self.drawer.is_none_or(|d| d.pane != **p))
            .filter_map(|(_, s)| *s)
            .all(|id| screen.shows(id).is_some());
        if !same && now.panes.values().any(Option::is_some) {
            self.kept = Some(Box::new(now));
        }
        self.apply_screen(screen);
        if back.is_some()
            && let Some(p) = self.panes.get_mut(&self.focused)
        {
            p.last = back;
        }
        if let Some((quick, host)) = drawer
            && let Some(h) = self.snapshot().shows(host)
            && let Some(p) = self.layout.split_at(
                h,
                Dir::Down,
                1.0 - self.drawer_share,
                crate::layout::MAX_WITH_DRAWER,
            )
        {
            self.panes.insert(p, PaneState { session: Some(quick), ..PaneState::default() });
            self.drawer = Some(Drawer { host: h, pane: p });
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
                self.pick(id);
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

    /// Make `id`'s row visible and select it (UX3.3): its workspace, project and worktree unfold.
    /// A row the tree can't place yet (its project's news still on the way) is placed when it can.
    /// One the tab over the sidebar doesn't list switches to the tab that has it (SPEC §11.2k):
    /// every way to a session (a pick, a jump, a notification, a new one) comes through here.
    pub(crate) fn reveal(&mut self, id: SessionId) {
        self.follow_tab_to_session(id);
        let Some(s) = self.session(id) else { return };
        let ws = crate::tree::fold_key(&self.tree().home(s));
        for node in [s.project.clone(), s.worktree.clone(), Some(ws)].into_iter().flatten() {
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

    /// A worktree's row, unfolded and selected (a new worktree with nothing in it yet), its
    /// workspace too.
    pub(crate) fn reveal_worktree(&mut self, id: &str, project: &str) {
        let tree = self.tree();
        let ws = tree.order().into_iter().find(|w| w.projects.iter().any(|p| p == project));
        if let Some(ws) = ws.map(|w| crate::tree::fold_key(&w.name)) {
            self.collapsed.remove(&ws);
        }
        self.collapsed.remove(project);
        self.collapsed.remove(id);
        let rows = self.rows_with(&Row::Worktree(id.to_owned()));
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
    /// Whether the focused pane can be split now; says why not when it can't.
    fn can_split(&mut self) -> bool {
        if self.drawer.is_some_and(|d| d.pane == self.focused) {
            self.toast_err("the drawer can't be split: focus another pane first");
            return false;
        }
        if self.layout.panes().len() - usize::from(self.drawer.is_some()) >= MAX_PANES {
            self.toast_err(format!(
                "at most {MAX_PANES} panes: close one to open another (it is in the sidebar)"
            ));
            return false;
        }
        true
    }

    /// The session a split is made from: the selected row's in the sidebar, else the focused
    /// pane's.
    fn split_source(&self) -> Option<SessionId> {
        match (self.focus, self.selected_row()) {
            (Focus::Sidebar, Some(Row::Session(id) | Row::Quick(id))) => Some(id),
            _ => self.focused_session(),
        }
    }

    /// `v` / `s`: what goes in the new pane. ⏎ takes the first, a clone of the session split
    /// (as `v` / `s` always did); then a new shell there, and every session not on screen, so
    /// an agent and a shell (or any two sessions) sit side by side.
    pub(crate) fn open_split_picker(&mut self, dir: Dir) {
        if !self.can_split() {
            return;
        }
        let source = self.split_source().and_then(|id| self.session(id)).cloned();
        let side = if dir == Dir::Right { "beside" } else { "below" };
        let mut items = vec![Item {
            label: match &source {
                Some(s) => format!("clone {}", s.name),
                None => "a new shell".into(),
            },
            detail: match &source {
                Some(s) if s.kind == SessionKind::Agent && s.agent.is_some() => {
                    format!("{side}: the same agent in the same place, a fresh conversation")
                }
                Some(_) => format!("{side}: a new shell in its directory"),
                None => format!("{side}: your shell, here"),
            },
            key: None,
            target: if source.is_some() { Target::Clone } else { Target::NewShell },
        }];
        if source.as_ref().is_some_and(|s| s.kind == SessionKind::Agent) {
            items.push(Item {
                label: "+ new shell".into(),
                detail: format!("{side}: a new shell in its worktree"),
                key: None,
                target: Target::NewShell,
            });
        }
        let shown = self.snapshot();
        items.extend(
            self.session_items(false)
                .into_iter()
                .filter(|i| matches!(i.target, Target::Session(id) if shown.shows(id).is_none())),
        );
        let title = if dir == Dir::Right { "Split: beside it" } else { "Split: below it" };
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Split(dir),
        });
    }

    /// The split picker's `+ new shell`: a shell in the source session's worktree (its live
    /// directory), in the new pane.
    pub(crate) fn split_shell(&mut self, dir: Dir) {
        if !self.can_split() {
            return;
        }
        let spec = match self.split_source().and_then(|id| self.session(id)) {
            Some(s) => SpawnSpec {
                worktree: s.worktree.clone(),
                cwd: s.live_cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(|| s.cwd.clone()),
                ..SpawnSpec::default()
            },
            None => SpawnSpec { cwd: self.here(), ..SpawnSpec::default() },
        };
        self.spawn_spec(spec, Some(dir));
    }

    pub(crate) fn split_clone(&mut self, dir: Dir) {
        if !self.can_split() {
            return;
        }
        let source = self.split_source();
        let spec = match source.and_then(|id| self.session(id)) {
            // A shell of SHELLS: a copy of it as a pane of its window (SPEC §23.17).
            Some(s) if s.loose && s.slot.is_some() => {
                let id = s.id;
                self.focus = Focus::Pane;
                return self.window_split(id, dir);
            }
            Some(s) if s.kind == SessionKind::Agent && s.agent.is_some() => SpawnSpec {
                agent: s.agent.clone(),
                profile: Some(s.profile.clone()),
                worktree: s.worktree.clone(),
                cwd: s.cwd.clone(),
                // It runs where its source runs, under the same profile: agreed to already.
                override_pin: true,
                confirmed: true,
                // A clone of a private session is private (SPEC §25.7a).
                private: s.private.then_some(true),
                ..SpawnSpec::default()
            },
            Some(s) => SpawnSpec {
                cwd: s.live_cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(|| s.cwd.clone()),
                private: s.private.then_some(true),
                ..SpawnSpec::default()
            },
            None => SpawnSpec { cwd: self.here(), ..SpawnSpec::default() },
        };
        self.spawn_spec(spec, Some(dir));
    }

    /// `shell.new` (`!`, UX3.4): another shell, of its own, in the context's worktree (else
    /// where you are), full sized. The worktree's quick terminal is `C-t` / `T`.
    pub(crate) fn new_shell(&mut self) {
        // A worktree around: the selected row's (in the sidebar), else the focused session's.
        // None (an empty pane, home, a chat, a shell of SHELLS, the SHELLS heading): a shell in
        // the workspace's SHELLS (SPEC §23.6, amends UX3.4), not the first project's checkout.
        let worktree = if self.focus == crate::app::Focus::Sidebar || self.context_hint().is_some()
        {
            self.context().1
        } else {
            self.focused_session().and_then(|id| self.session(id)).and_then(|s| s.worktree.clone())
        };
        if worktree.is_none() {
            return self.shells_new(None, false);
        }
        self.spawn_spec(SpawnSpec { worktree, ..SpawnSpec::default() }, None);
    }

    // ── quick new (UX3.2, UX3.7) ────────────────────────────────────────────

    /// Ask what quick new would start in the context's worktree (for its label). `tick` asks
    /// again when the context moves to another worktree.
    pub(crate) fn refresh_again(&mut self) {
        let (_, worktree) = self.context();
        self.again_for = Some(worktree.clone());
        let workspace = self.workspace.clone();
        self.request(Pending::Again(worktree.clone()), Request::LastLaunch { workspace, worktree });
    }

    /// What quick new starts here, for people: `claude`, `codex@work`.
    pub fn again_label(&self) -> String {
        match &self.again {
            Some((w, l)) if *w == self.context().1 => l.label(),
            _ => ptyro_agents::default_agent(&self.config),
        }
    }

    /// `session.again` (`C-q C-n`, `C-n` in the sidebar): another of the agent the context's
    /// worktree (the selected row's, the focused session's) last started, else its project's
    /// default agent, else the default agent; no questions asked. It opens full sized, like any
    /// new session. A shell is `!`.
    pub(crate) fn quick_new(&mut self) {
        let (_, worktree) = self.context();
        let cwd = if worktree.is_some() { Default::default() } else { self.here() };
        self.spawn_spec(SpawnSpec { again: true, worktree, cwd, ..SpawnSpec::default() }, None);
    }
}

// ── other machines (MM1.B) ──────────────────────────────────────────────────

impl App {
    /// A machine's link changed: say so when it went down, came back, or runs another version
    /// (with what to do), and keep the list current.
    pub(crate) fn machine_news(&mut self, info: ptyro_core::model::MachineInfo) {
        use ptyro_core::model::MachineState as S;
        let before = self.machines.iter().find(|m| m.name == info.name).map(|m| m.state);
        if before != Some(info.state) {
            match info.state {
                S::Online if before.is_some_and(|b| b != S::Connecting) || before.is_none() => {
                    self.toast(format!("{} is back", info.name));
                }
                S::Offline if before == Some(S::Online) => {
                    self.toast_err(format!(
                        "{} went offline: its workspaces show as they were",
                        info.name
                    ));
                }
                S::Mismatch => {
                    let fix = info.fix.clone().unwrap_or_default();
                    self.toast_err(format!("{}: {}", info.error.clone().unwrap_or_default(), fix));
                }
                _ => {}
            }
        }
        match self.machines.iter_mut().find(|m| m.name == info.name) {
            Some(m) => *m = info,
            None => self.machines.push(info),
        }
    }

    /// The machine a workspace (or anything named after one) is on: `laptop2` for
    /// `laptop2:work`; `None` here.
    pub fn machine_of_name<'a>(
        &'a self,
        name: &'a str,
    ) -> Option<&'a ptyro_core::model::MachineInfo> {
        let (m, _) = ptyro_core::model::split_machine(name)?;
        self.machines.iter().find(|x| x.name == m)
    }
}

// ── machine actions (MM1.B): the TUI's side of `ptyro machine …` ────────────

impl App {
    /// The machine of the workspace you're in, if it is another one.
    pub(crate) fn machine_here(&self) -> Option<String> {
        ptyro_core::model::split_machine(&self.workspace).map(|(m, _)| m.to_owned())
    }

    pub(crate) fn machine_action(&mut self, action: &'static str) {
        use crate::app::{Overlay, Purpose};
        use crate::palette::{Item, Palette, Target};
        match action {
            "machine.list" => self.run_cli(&["machine".into(), "ls".into()]),
            "machine.pair" => self.open_machine_pair_form(),
            "machine.add" => self.open_machine_form(),
            "machine.unhide" => {
                let items: Vec<Item> = self
                    .machines
                    .iter()
                    .flat_map(|m| m.hidden.iter().map(move |w| (m.name.clone(), w.clone())))
                    .map(|(m, w)| Item {
                        label: format!("{m} / {w}"),
                        detail: "hidden here".into(),
                        key: None,
                        target: Target::Machine(format!("{m}:{w}")),
                    })
                    .collect();
                if items.is_empty() {
                    self.toast("no hidden workspaces");
                    return;
                }
                self.overlay = Some(Overlay::Picker {
                    palette: Palette::new("Show again", items),
                    purpose: Purpose::MachineUnhide,
                });
            }
            "machine.retry" | "machine.hide" | "machine.remove" | "machine.update" => {
                match self.machine_here() {
                    Some(m) => self.machine_on(action, m),
                    None if self.machines.is_empty() => self.toast_offer(
                        "no other machines yet",
                        false,
                        crate::app::Offer {
                            label: "add one".into(),
                            then: crate::app::Then::Run(crate::keymap::Command {
                                action: "machine.add",
                                arg: None,
                            }),
                        },
                    ),
                    None => {
                        let items = self
                            .machines
                            .iter()
                            .map(|m| Item {
                                label: m.name.clone(),
                                detail: m.state.label().into(),
                                key: None,
                                target: Target::Machine(m.name.clone()),
                            })
                            .collect();
                        self.overlay = Some(Overlay::Picker {
                            palette: Palette::new("Which machine?", items),
                            purpose: Purpose::MachinePick(action),
                        });
                    }
                }
            }
            other => self.toast(format!("{other}: not available")),
        }
    }

    /// Act on one machine: try its link now, hide the workspace you're in (when it's that
    /// machine's), or stop mirroring it (asked first).
    pub(crate) fn machine_on(&mut self, action: &str, m: String) {
        use crate::app::{Confirmed, Overlay};
        match action {
            "machine.retry" => self.request(
                Pending::Done(format!("trying {m} again")),
                Request::MachineRetry { name: m },
            ),
            // The CLI's own, full sized: it says what happens there and asks first.
            "machine.update" => self.run_cli(&["machine".into(), "update".into(), m]),
            "machine.hide" => {
                let Some((_, ws)) = ptyro_core::model::split_machine(&self.workspace)
                    .filter(|(x, _)| *x == m)
                    .map(|(x, w)| (x.to_owned(), w.to_owned()))
                else {
                    self.toast(format!("select one of {m}'s workspaces to hide it"));
                    return;
                };
                let said = format!("{m} / {ws} is hidden here (machine.unhide shows it)");
                let req = Request::MachineHide { name: m, workspace: ws, hidden: true };
                self.request(Pending::Done(said), req);
            }
            "machine.remove" => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("Stop mirroring {m}? {} [y/N]", ptyro_core::removal::MACHINE),
                    action: Confirmed::Send(
                        Box::new(Request::MachineRemove { name: m.clone() }),
                        format!("{m} is no longer mirrored here"),
                    ),
                });
            }
            _ => {}
        }
    }
}
