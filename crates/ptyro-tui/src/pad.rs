//! The scratchpad in the TUI (NOTE1.1–NOTE1.3, NOTE1.7, SPEC §13.6): the notes files, in your
//! editor in a drawer, a popup or a pane; a line added without opening it; the open items, to
//! search, tick and start as tasks; and the saved copies. There are two pads, the one for
//! everywhere and the project's; `Ctrl-Q g` switches, and which one you get follows where you are.
//! The files and their logic are the daemon's (`ptyro_core::scratchpad`); this is only how it
//! looks and which keys reach it.

use ptyro_core::SessionId;
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::scratchpad::{self as pad, PadSel};
use ptyro_protocol::{PadDoc, PadMark, Reply, Request};
use ratatui::layout::Rect;

use crate::app::{App, Confirmed, Focus, Overlay, PaneState, Pending, PromptKind, Purpose, Wizard};
use crate::layout::{Dir, PaneId};
use crate::palette::{Item, Palette, Target};

/// The popup's share of the screen.
const POPUP_W: u16 = 80;
const POPUP_H: u16 = 70;

/// Where the popup is drawn, and so the size its editor gets.
pub fn popup_rect(area: Rect) -> Rect {
    let w = (area.width * POPUP_W / 100).max(20.min(area.width)).min(area.width);
    let h = (area.height * POPUP_H / 100).max(6.min(area.height)).min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

impl App {
    pub fn is_pad(&self, id: SessionId) -> bool {
        self.session(id).is_some_and(|s| s.pad)
    }

    /// A scratchpad's live editor session (any of them).
    pub fn pad_session(&self) -> Option<SessionId> {
        self.sessions.iter().find(|s| s.pad && s.state.is_live()).map(|s| s.id)
    }

    /// The live editor session of the pad `sel` (`Global` or `Project(id)`).
    fn pad_session_of(&self, sel: &PadSel) -> Option<SessionId> {
        let project = match sel {
            PadSel::Project(id) => Some(id),
            _ => None,
        };
        self.sessions
            .iter()
            .find(|s| s.pad && s.state.is_live() && s.pad_project.as_ref() == project)
            .map(|s| s.id)
    }

    /// What a pad is called: `everywhere`, or `ptyro (this project)`.
    pub fn pad_label(&self, sel: &PadSel) -> String {
        match sel {
            PadSel::Project(id) => {
                let name = self
                    .projects
                    .iter()
                    .find(|p| p.id == *id)
                    .map_or(id.as_str(), |p| p.slug.as_str());
                format!("{name} (this project)")
            }
            _ => "everywhere".into(),
        }
    }

    /// The pad a key acts on: the project you are in, unless `Ctrl-Q g` chose the one for
    /// everywhere, else the one for everywhere. The project is remembered while the pad has the
    /// focus, since the pad itself says nothing about where you were.
    pub fn pad_want(&mut self) -> PadSel {
        let here = self.where_am_i().0;
        let on_pad = self.focused_session().is_some_and(|s| self.is_pad(s));
        if here.is_some() || !on_pad {
            self.pad_ctx = here;
        }
        match (&self.pad_ctx, self.pad_global) {
            (Some(p), false) => PadSel::Project(p.clone()),
            _ => PadSel::Global,
        }
    }

    /// The pane an editor has in `pane` mode (not the drawer's).
    fn pad_pane(&self, id: SessionId) -> Option<PaneId> {
        self.layout.panes().into_iter().find(|p| {
            self.drawer.is_none_or(|d| d.pane != *p)
                && self.panes.get(p).and_then(|s| s.session) == Some(id)
        })
    }

    /// The part of the popup the editor draws in (inside its border).
    pub(crate) fn inner_popup(&self) -> Rect {
        let r = popup_rect(self.area);
        Rect {
            x: r.x + 1,
            y: r.y + 1,
            width: r.width.saturating_sub(2),
            height: r.height.saturating_sub(2),
        }
    }

    /// The key that shows or hides it, for hints.
    pub fn pad_key(&self) -> Option<String> {
        self.keymap.key_for("scratchpad.toggle")
    }

    /// `scratchpad.toggle`: hidden → show it and focus it; shown but elsewhere → focus it; focused
    /// → hide it. The editor keeps running. Where it shows is `[scratchpad] mode`; which pad is
    /// the project's you are in, else the one for everywhere (`scratchpad.cycle` switches).
    pub fn toggle_pad(&mut self) {
        if self.pad_popup.take().is_some() {
            self.sync();
            return;
        }
        let want = self.pad_want();
        let Some(id) = self.pad_session_of(&want) else {
            let req = Request::ScratchPane {
                pad: want,
                create: true,
                env: Self::client_env(),
                size: None,
                workspace: Some(self.workspace.clone()),
            };
            self.request(Pending::Pad, req);
            return;
        };
        if self.drawer_session() == Some(id) {
            if self.in_drawer() {
                self.hide_drawer();
            } else if let Some(d) = self.drawer {
                self.focused = d.pane;
                self.focus = Focus::Pane;
                self.sidebar_zoom = false;
                self.view_changed();
            }
        } else if let Some(p) = self.pad_pane(id) {
            if self.focused == p && self.focus == Focus::Pane {
                self.close_pane(p);
            } else {
                self.focused = p;
                self.focus = Focus::Pane;
                self.sidebar_zoom = false;
                self.view_changed();
            }
        } else {
            self.show_pad(id);
        }
    }

    /// `scratchpad.cycle`: the other pad, this project's ⇄ the one for everywhere. What was
    /// showing is swapped for it; with nothing showing, the other pad opens.
    pub fn cycle_pad(&mut self) {
        if matches!(self.pad_want(), PadSel::Global) && self.pad_ctx.is_none() {
            self.toast("not in a project: the scratchpad is the one for everywhere");
            return;
        }
        self.pad_global = !self.pad_global;
        let ids: Vec<SessionId> = self.sessions.iter().filter(|s| s.pad).map(|s| s.id).collect();
        for id in ids {
            self.pad_gone(id);
        }
        self.toggle_pad();
    }

    /// Show the editor the way `[scratchpad] mode` says.
    pub(crate) fn show_pad(&mut self, id: SessionId) {
        match self.config.scratchpad.mode.as_str() {
            "popup" => {
                self.pad_popup = Some(id);
                self.sync();
            }
            "pane" => {
                let Some(p) = self.layout.split(self.focused, Dir::Right) else {
                    self.toast_err(format!("at most {} panes", crate::layout::MAX_PANES));
                    return;
                };
                self.panes.insert(p, PaneState { session: Some(id), ..PaneState::default() });
                self.zoom = false;
                self.focused = p;
                self.focus = Focus::Pane;
                self.sidebar_zoom = false;
                self.view_changed();
            }
            _ => self.open_drawer(id),
        }
    }

    /// The editor's session is gone (it exited): whatever showed it closes.
    pub(crate) fn pad_gone(&mut self, id: SessionId) {
        if self.pad_popup == Some(id) {
            self.pad_popup = None;
        }
        if self.drawer_session() == Some(id) {
            self.hide_drawer();
        }
        if let Some(p) = self
            .layout
            .panes()
            .into_iter()
            .find(|p| self.panes.get(p).and_then(|s| s.session) == Some(id))
        {
            self.close_pane(p);
        }
    }

    // ── adding ──────────────────────────────────────────────────────────

    /// `scratchpad.add`: a one-line box, stamped with where you are, without opening the pad.
    pub fn open_pad_add(&mut self, text: String, about: Option<SessionId>) {
        self.overlay = Some(Overlay::Prompt {
            title: self.pad_add_title(),
            text,
            kind: PromptKind::PadAdd(about.or_else(|| self.focused_session())),
        });
    }

    /// The box's title says which pad it adds to, and how to switch.
    pub(crate) fn pad_add_title(&mut self) -> String {
        let want = self.pad_want();
        let label = self.pad_label(&want);
        let switch = if self.pad_ctx.is_some() { ", ^g switches" } else { "" };
        format!("Add to the scratchpad — {label} (⏎ adds{switch})")
    }

    /// `scratchpad.note`: the same box, started from the selected session's task or last words.
    pub fn note_this(&mut self) {
        let id = match (self.focus, self.selected_row()) {
            (Focus::Sidebar, Some(r)) => r.session(),
            _ => self.focused_session(),
        };
        let words = id
            .and_then(|id| self.session(id))
            .filter(|s| !s.pad)
            .and_then(|s| {
                s.title.clone().or_else(|| s.task.clone()).or_else(|| s.last_reply.clone())
            })
            .map(|t| t.lines().next().unwrap_or("").trim().to_owned())
            .unwrap_or_default();
        self.open_pad_add(words, id);
    }

    pub(crate) fn pad_add(&mut self, text: String, note: bool, about: Option<SessionId>) {
        let req = Request::ScratchAdd {
            pad: self.pad_want(),
            text,
            section: None,
            note,
            stamp: true,
            session: about.map(|s| s.full()),
            cwd: std::env::current_dir().ok(),
        };
        self.request(Pending::PadAdded, req);
    }

    /// Copy mode's `a`: what's selected, quoted, as a note.
    pub(crate) fn pad_quote(&mut self, text: &str) {
        let quoted: Vec<String> = text.lines().map(|l| format!("> {l}")).collect();
        self.pad_add(quoted.join("\n"), true, self.focused_session());
    }

    // ── the open items ──────────────────────────────────────────────────

    /// `scratchpad.items`: read the pad, then list its open items.
    pub fn open_pad_items(&mut self) {
        let pad = self.pad_want();
        self.request(Pending::PadItems { refresh: false }, Request::ScratchGet { pad });
    }

    /// The items list from a fresh read of the pad; `keep` is what was typed and where.
    pub(crate) fn show_pad_items(&mut self, doc: PadDoc, keep: Option<(String, usize)>) {
        self.set_pad_count(&doc.pad, doc.open);
        let items: Vec<Item> = pad::items(&doc.text)
            .into_iter()
            .filter(|i| self.pad_show_done || !i.done)
            .map(|i| {
                let mut detail: Vec<String> = Vec::new();
                detail.extend(i.section.clone());
                if let Some(w) = &i.worktree {
                    detail.push(format!("→ {w}"));
                }
                if let Some(n) = i.pr {
                    detail.push(format!("#{n}"));
                }
                if let Some(b) = &i.by {
                    detail.push(format!("by {b}"));
                }
                detail.extend(i.at.clone());
                Item {
                    label: format!("{}{}", if i.done { "✓ " } else { "" }, i.text),
                    detail: detail.join(" · "),
                    key: Some(format!("L{}", i.line)),
                    target: Target::PadItem(i.line),
                }
            })
            .collect();
        let path = doc.path.display().to_string();
        let which = self.pad_label(&doc.pad);
        let switch = if self.pad_ctx.is_some() { " · ^g other pad" } else { "" };
        let title = format!(
            "Scratchpad — {which} · {} open · {path}   ⏎ start as a task · ^x tick · ^a ticked · ^e editor{switch}",
            doc.open
        );
        let mut palette = Palette::new(title, items);
        if let Some((q, sel)) = keep {
            palette = palette.with_query(q, sel);
        }
        self.pad_doc = Some(doc);
        self.overlay = Some(Overlay::Picker { palette, purpose: Purpose::PadItems });
    }

    /// The items list's control keys, which can't be letters because letters search. `Some(true)`:
    /// handled, the list stays; `Some(false)`: handled, it closes.
    pub(crate) fn pad_items_ctrl(&mut self, palette: &Palette, key: KeyEvent) -> Option<bool> {
        if !key.mods.has(Mods::CTRL) {
            return None;
        }
        match key.code {
            KeyCode::Char { c: 'x' } => {
                if let Some(Target::PadItem(line)) = palette
                    .filtered
                    .get(palette.selected)
                    .map(|_| palette.item(palette.selected).target.clone())
                {
                    let done = self
                        .pad_doc
                        .as_ref()
                        .and_then(|d| pad::items(&d.text).into_iter().find(|i| i.line == line))
                        .is_some_and(|i| i.done);
                    let mark = if done { PadMark::Open } else { PadMark::Done { pr: None } };
                    let pad = self.pad_doc.as_ref().map(|d| d.pad.clone()).unwrap_or_default();
                    self.request(
                        Pending::Done(String::new()),
                        Request::ScratchMark { pad, sel: line.to_string(), mark },
                    );
                }
                Some(true)
            }
            KeyCode::Char { c: 'a' } => {
                self.pad_show_done = !self.pad_show_done;
                let pad = self.pad_doc.as_ref().map(|d| d.pad.clone()).unwrap_or_default();
                self.request(Pending::PadItems { refresh: true }, Request::ScratchGet { pad });
                Some(true)
            }
            // The other pad's list, in place (the list is rebuilt when the reply comes).
            KeyCode::Char { c: 'g' } if self.pad_ctx.is_some() => {
                self.pad_global = !self.pad_global;
                let pad = self.pad_want();
                self.request(Pending::PadItems { refresh: true }, Request::ScratchGet { pad });
                Some(true)
            }
            KeyCode::Char { c: 'e' } => {
                self.toggle_pad();
                Some(false)
            }
            _ => None,
        }
    }

    /// ⏎ on an item: a task for your default agent, as `n` starts one, told what the item says,
    /// in a new worktree that the item is then tagged with.
    pub(crate) fn start_pad_item(&mut self, line: u32) {
        let Some(doc) = self.pad_doc.clone() else { return };
        let Some(item) = pad::items(&doc.text).into_iter().find(|i| i.line == line) else {
            self.toast_err("that line has changed; open the list again");
            return;
        };
        if item.done {
            self.toast_err("already ticked (^x unticks it)");
            return;
        }
        // The task goes in the project whose pad the item is in.
        let (mut project, mut worktree) = self.context();
        let of_project = matches!(doc.pad, PadSel::Project(_));
        if let PadSel::Project(id) = &doc.pad
            && project.as_ref() != Some(id)
        {
            worktree = self.root_of(id);
            project = Some(id.clone());
        }
        let wizard =
            Wizard { project, worktree, pad_item: Some((line, of_project)), ..Wizard::default() };
        self.resolve_for(&wizard);
        self.start_task(pad::body(&doc.text, &item), None, wizard);
    }

    // ── saved copies ────────────────────────────────────────────────────

    pub fn open_pad_history(&mut self) {
        self.pad_for = self.pad_want();
        self.request(Pending::PadHistory, Request::ScratchHistory { pad: self.pad_for.clone() });
    }

    pub(crate) fn show_pad_history(&mut self, snapshots: Vec<pad::Snapshot>) {
        let now = ptyro_core::now_ms();
        let items: Vec<Item> = snapshots
            .into_iter()
            .map(|s| Item {
                label: s.rev.clone(),
                detail: format!(
                    "{} ago · {} lines · {} bytes",
                    crate::ui::ago(now.saturating_sub(s.at_ms)),
                    s.lines,
                    s.bytes
                ),
                key: None,
                target: Target::PadRev(s.rev),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(
                format!(
                    "Scratchpad — {}: put a saved copy back (what's there is saved first)",
                    self.pad_label(&self.pad_for)
                ),
                items,
            ),
            purpose: Purpose::PadHistory,
        });
    }

    pub(crate) fn restore_pad(&mut self, rev: String) {
        self.overlay = Some(Overlay::Confirm {
            text: format!("Put saved copy {rev} back as the scratchpad? [y/N]"),
            action: Confirmed::Send(
                Box::new(Request::ScratchRestore { pad: self.pad_for.clone(), rev }),
                "restored".into(),
            ),
        });
    }

    // ── what the daemon says ────────────────────────────────────────────

    /// Remember a pad's open count (for the status bar).
    pub(crate) fn set_pad_count(&mut self, pad: &PadSel, open: u32) {
        match pad {
            PadSel::Project(id) => {
                self.pad_counts.insert(id.clone(), open);
            }
            _ => self.pad_open = open,
        }
    }

    /// The pad changed somewhere: the count, and an open list of it.
    pub(crate) fn pad_changed(&mut self, pad: &PadSel, open: u32) {
        self.set_pad_count(pad, open);
        if let Some(Overlay::Picker { purpose: Purpose::PadItems, .. }) = &self.overlay
            && self.pad_doc.as_ref().is_none_or(|d| d.pad == *pad)
        {
            self.request(
                Pending::PadItems { refresh: true },
                Request::ScratchGet { pad: pad.clone() },
            );
        }
    }

    pub(crate) fn pad_reply(&mut self, pending: Pending, reply: Result<Reply, ptyro_core::Error>) {
        match (pending, reply) {
            (Pending::Pad, Ok(Reply::Session { info })) => {
                let id = info.id;
                self.upsert(info);
                self.show_pad(id);
            }
            (Pending::PadAdded, Ok(Reply::Scratch { doc })) => {
                self.set_pad_count(&doc.pad, doc.open);
                let which = self.pad_label(&doc.pad);
                self.toast(format!("added to the scratchpad — {which} ({} open)", doc.open));
            }
            (Pending::PadItems { refresh }, Ok(Reply::Scratch { doc })) => {
                if refresh {
                    // Only an open list is rebuilt, keeping what was typed and the place in it.
                    if let Some(Overlay::Picker { palette, purpose: Purpose::PadItems }) =
                        &self.overlay
                    {
                        let keep = (palette.query.clone(), palette.selected);
                        self.show_pad_items(doc, Some(keep));
                    } else {
                        self.set_pad_count(&doc.pad, doc.open);
                    }
                } else {
                    self.show_pad_items(doc, None);
                }
            }
            (Pending::PadHistory, Ok(Reply::ScratchLog { snapshots })) => {
                if snapshots.is_empty() {
                    self.toast("no saved copies yet");
                } else {
                    self.show_pad_history(snapshots);
                }
            }
            (
                Pending::Pad | Pending::PadAdded | Pending::PadItems { .. } | Pending::PadHistory,
                Err(e),
            ) => {
                self.toast_err(e.message);
            }
            _ => {}
        }
    }
}
