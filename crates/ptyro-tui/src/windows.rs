//! SHELLS windows in the TUI (SPEC §23.17): a number in SHELLS holds up to four panes, and
//! showing any of them shows the whole window as the daemon keeps it (tmux's window). `v`/`s` on
//! a shell of SHELLS splits its window with a copy of it (`ShellSplit`); resizing, moving focus
//! and zooming the window's screen are sent back (`ShellLayout`), and another client's change
//! is applied here. Break a pane out to a number of its own, or join a shell into a window.

use std::collections::HashMap;

use ptyro_core::SessionId;
use ptyro_core::session::SessionInfo;
use ptyro_core::window::{PaneTree, SplitDir, WindowLayout};
use ptyro_protocol::Request;

use crate::app::{App, Overlay, Pending, PromptKind};
use crate::layout::{Dir, Layout, Node};
use crate::screens::Screen;

/// A window: its workspace and number.
pub(crate) type WindowKey = (String, u32);

fn to_node(t: &PaneTree) -> Node {
    match t {
        PaneTree::Pane { pane } => Node::Pane { id: *pane },
        PaneTree::Split { dir, share, a, b } => Node::Split {
            dir: match dir {
                SplitDir::Right => Dir::Right,
                SplitDir::Down => Dir::Down,
            },
            ratio: f32::from(*share) / 1000.0,
            a: Box::new(to_node(a)),
            b: Box::new(to_node(b)),
        },
    }
}

/// The screen's split tree as a window's, its panes renamed by `pane_of` (`None`: a pane that
/// isn't one of the window's).
fn to_tree(n: &Node, pane_of: &impl Fn(u32) -> Option<u32>) -> Option<PaneTree> {
    Some(match n {
        Node::Pane { id } => PaneTree::Pane { pane: pane_of(*id)? },
        Node::Split { dir, ratio, a, b } => PaneTree::Split {
            dir: match dir {
                Dir::Right => SplitDir::Right,
                Dir::Down => SplitDir::Down,
            },
            share: (ratio * 1000.0).round().clamp(0.0, 1000.0) as u16,
            a: Box::new(to_tree(a, pane_of)?),
            b: Box::new(to_tree(b, pane_of)?),
        },
    })
}

impl App {
    /// A loose session's window.
    pub(crate) fn window_key(&self, s: &SessionInfo) -> Option<WindowKey> {
        s.loose.then_some(())?;
        Some((self.tree().home(s), s.slot?))
    }

    /// A window's panes, by pane number.
    pub(crate) fn window_panes(&self, key: &WindowKey) -> Vec<&SessionInfo> {
        let mut v: Vec<&SessionInfo> =
            self.sessions.iter().filter(|s| self.window_key(s).as_ref() == Some(key)).collect();
        v.sort_by_key(|s| s.pane);
        v
    }

    /// The pane `Alt-N` and a click on a window's row show: the one that had focus there, else
    /// its first.
    pub(crate) fn window_focus(&self, key: &WindowKey) -> Option<SessionId> {
        let panes = self.window_panes(key);
        let focus = panes.first().and_then(|s| s.layout.as_ref()).map(|l| l.focus);
        panes.iter().find(|s| s.pane == focus && focus.is_some()).or(panes.first()).map(|s| s.id)
    }

    /// The screen that shows `id`'s window as its layout says, `id` focused; `None` for a
    /// session that isn't a pane of a window of several.
    pub(crate) fn window_screen(&self, id: SessionId) -> Option<Screen> {
        let s = self.session(id)?;
        let layout = s.layout.as_ref()?;
        let key = self.window_key(s)?;
        let by_pane: HashMap<u32, SessionId> =
            self.window_panes(&key).iter().filter_map(|o| Some((o.pane?, o.id))).collect();
        let panes: HashMap<u32, Option<SessionId>> =
            layout.panes().into_iter().map(|p| (p, by_pane.get(&p).copied())).collect();
        if panes.values().any(Option::is_none) {
            return None;
        }
        Some(Screen {
            layout: Layout::from_root(to_node(&layout.root)),
            panes,
            focus: s.pane.unwrap_or(layout.focus),
            zoom: layout.zoom,
            drawer: None,
        })
    }

    /// The window the screen shows, and its arrangement as the screen has it: every pane but
    /// the drawer shows a pane of the same window. `None` for any other screen.
    fn window_on_screen(&self) -> Option<(WindowKey, Option<WindowLayout>, usize)> {
        let drawer = self.drawer.map(|d| d.pane);
        let shown: Vec<(u32, SessionId)> = self
            .layout
            .panes()
            .into_iter()
            .filter(|p| Some(*p) != drawer)
            .map(|p| Some((p, self.panes.get(&p)?.session?)))
            .collect::<Option<_>>()?;
        let key = self.window_key(self.session(self.focused_session()?)?)?;
        let mut pane_of = HashMap::new();
        for (p, id) in &shown {
            let s = self.session(*id)?;
            if self.window_key(s).as_ref() != Some(&key) {
                return None;
            }
            pane_of.insert(*p, s.pane?);
        }
        // The drawer isn't the window's: arrange what's above it.
        let mut layout = self.layout.clone();
        if let Some(d) = drawer {
            layout.close(d);
        }
        let derived = to_tree(&layout.root, &|p| pane_of.get(&p).copied()).map(|root| {
            let focus = pane_of.get(&self.focused).copied().unwrap_or_default();
            WindowLayout { root, focus, zoom: self.zoom }
        });
        Some((key, derived, shown.len()))
    }

    /// Keep the screen and the daemon's window layout the same (from `tick`): a change made here
    /// (a resize, focus, zoom) is sent; one made elsewhere (another client, a split, a pane that
    /// went) is shown.
    pub(crate) fn sync_window(&mut self) {
        let Some((key, derived, shown)) = self.window_on_screen() else { return };
        let panes = self.window_panes(&key);
        let theirs = panes.first().and_then(|s| s.layout.clone());
        let count = panes.len();
        let addr = panes.first().map(|s| s.id.full());
        let seen = self.window_seen.as_ref().filter(|(k, _)| *k == key).map(|(_, l)| l.clone());
        if seen.is_none() {
            self.window_sent.clear();
        }
        // An echo of something sent from here (a drag sends several before the first comes
        // back) is old news, not another client's change.
        let echo = theirs.as_ref().is_some_and(|t| self.window_sent.contains(t));
        if seen.as_ref() != Some(&theirs) && !echo {
            // News from the daemon (or the first look): the screen follows.
            self.window_seen = Some((key, theirs.clone()));
            if theirs.is_some() && derived != theirs {
                self.reshow_window();
            }
            return;
        }
        // What differs on screen from the daemon's was done here: send it, once.
        if let (Some(mine), Some(addr)) = (derived.clone(), addr)
            && theirs.as_ref() != Some(&mine)
            && self.window_sent.last() != Some(&mine)
            && shown == count
            && count > 1
        {
            self.window_seen = Some((key, Some(mine.clone())));
            if self.window_sent.len() >= 16 {
                self.window_sent.remove(0);
            }
            self.window_sent.push(mine.clone());
            self.request(Pending::Done(String::new()), Request::ShellLayout { addr, layout: mine });
        } else if echo && theirs == derived {
            // Caught up: the daemon has what the screen shows.
            self.window_seen = Some((key, theirs));
            self.window_sent.clear();
        }
    }

    /// Show the focused session's window again as the daemon has it, its focus included: two
    /// clients on one window agree on the focused pane instead of each sending its own.
    fn reshow_window(&mut self) {
        let Some(id) = self.focused_session() else { return };
        let Some(mut screen) = self.window_screen(id) else { return };
        let focus = self.session(id).and_then(|s| s.layout.as_ref()).map(|l| l.focus);
        if let Some(f) = focus.filter(|f| screen.layout.contains(*f)) {
            screen.focus = f;
        }
        self.apply_window(screen);
        self.view_changed();
    }

    /// `v`/`s` on a shell of SHELLS: a copy of it as a new pane of its window.
    pub(crate) fn window_split(&mut self, id: SessionId, dir: Dir) {
        let dir = match dir {
            Dir::Right => SplitDir::Right,
            Dir::Down => SplitDir::Down,
        };
        let size = self.sizes.get(&id).copied();
        let req = Request::ShellSplit { addr: id.full(), dir, env: Self::client_env(), size };
        self.request(Pending::Spawned { split: None, spec: Box::default(), back: false }, req);
    }

    /// `shells.break`: the pane gets a number of its own (tmux's `break-pane`).
    pub(crate) fn window_break(&mut self, s: &SessionInfo) {
        let alone = self.window_key(s).is_none_or(|k| self.window_panes(&k).len() < 2);
        if alone {
            return self.toast(format!(
                "shell {} has one pane: it has a number of its own already",
                s.slot.unwrap_or_default()
            ));
        }
        self.request(
            Pending::Done(format!("{} has a number of its own now", s.name)),
            Request::ShellJoin { addr: s.id.full(), into: None, dir: SplitDir::Right },
        );
    }

    /// `shells.join`: ask which window to join (tmux's `join-pane`).
    pub(crate) fn window_join_ask(&mut self, s: &SessionInfo) {
        self.overlay = Some(Overlay::Prompt {
            title: format!(
                "Join shell {} into which shell? (a number; it goes below that window's focused pane)",
                s.slot.unwrap_or_default()
            ),
            text: String::new(),
            kind: PromptKind::ShellJoin(s.id),
        });
    }

    /// What was typed for `shells.join`.
    pub(crate) fn window_join_typed(&mut self, id: SessionId, text: &str) {
        match text.trim().parse::<u32>() {
            Ok(n) if n > 0 => self.request(
                Pending::Done(format!("joined shell {n}")),
                Request::ShellJoin { addr: id.full(), into: Some(n), dir: SplitDir::Down },
            ),
            _ => self.toast_err("a shell's number, from 1 up"),
        }
    }
}
