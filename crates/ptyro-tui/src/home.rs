//! Home (SPEC §26.9): one look at everything, in a panel over the panes (`home`, `H`; and what
//! a bare `ptyro` opens on, unless `[ui] home_at_start = false`). A row a project, ranked by
//! the daemon (`Request::Home`, `ptyro_core::home`): what needs you, what was left hanging,
//! what is being worked on, what is quiet. `→` unfolds a row into its trail: where it was left
//! off, what happened there, the files that work touched most. Every profile's usage is under
//! it, all of it.
//!
//! It is one of the chat panel's shapes (`ChatPanel::Home`), so `` ` `` back and forth, Esc and
//! the sidebar work as they do for the Library. What it shows is the daemon's view: asked
//! again a moment after anything changes, and every half minute for the ages. The selection
//! goes by what a line is, not where: a row that moves takes the cursor with it.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use ptyro_core::home::{self, HomeView, Level, Memory, Next, Section, Thing};
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::kb::card::minute;
use ptyro_core::session::ProcessState;
use ptyro_core::{dur, now_ms};
use ptyro_protocol::{HomeDismiss, Reply, Request};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, Pending, PromptKind};
use crate::chats::ChatPanel;
use crate::keymap::Command;
use crate::tree::Row;
use crate::ui::fg;

/// How many rows of a section show before `… N more`.
const CAP_HANGING: usize = 8;
const CAP_QUIET: usize = 6;
/// Asked again this long after something changed, and this often for the ages.
const SETTLE: Duration = Duration::from_millis(700);
const AGES: Duration = Duration::from_secs(30);
/// What a line is: the selection is one of these, so it follows its line when rows move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    /// A project's row (its key).
    Row(String),
    /// Another thing of the unfolded row (the row's key, the thing's).
    Thing(String, String),
    /// Where the unfolded row was left off.
    Left(String),
    /// A line of its trail (the item's address).
    Entry(String),
    /// `… N more` of a section.
    More(Section),
}

/// One drawn line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum L {
    Head(Section, usize),
    Row(usize),
    Thing(usize, usize),
    /// Where it was left off: you (false) or the reply (true).
    Left(usize, bool),
    Day(String),
    Entry(usize, usize),
    Hot(usize),
    Note(String),
    More(Section, usize),
    Blank,
}

pub struct HomeScreen {
    pub view: Option<HomeView>,
    pub error: Option<String>,
    pub cur: Option<Sel>,
    /// The unfolded row's key.
    pub open: Option<String>,
    /// Sections shown whole.
    pub full: Vec<Section>,
    asked: Instant,
    /// Something changed since the view was asked for.
    stale: bool,
    scroll: Cell<usize>,
    /// Where each selectable line was drawn: (screen row, what it is).
    drawn: RefCell<Vec<(u16, Sel)>>,
}

impl HomeScreen {
    pub fn new() -> Self {
        Self {
            view: None,
            error: None,
            cur: None,
            open: None,
            full: Vec::new(),
            asked: Instant::now(),
            stale: false,
            scroll: Cell::new(0),
            drawn: RefCell::new(Vec::new()),
        }
    }

    fn lines(&self) -> Vec<L> {
        let Some(v) = &self.view else { return Vec::new() };
        let mut out = Vec::new();
        for section in Section::ALL {
            let rows: Vec<usize> =
                (0..v.rows.len()).filter(|i| v.rows[*i].section == section).collect();
            if rows.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(L::Blank);
            }
            out.push(L::Head(section, rows.len()));
            let cap = match section {
                _ if self.full.contains(&section) => usize::MAX,
                Section::Hanging => CAP_HANGING,
                Section::Quiet => CAP_QUIET,
                _ => usize::MAX,
            };
            let mut hidden = 0;
            for (n, i) in rows.into_iter().enumerate() {
                let row = &v.rows[i];
                let open = self.open.as_deref() == Some(row.key.as_str());
                // (The unfolded row is shown wherever the cap falls.)
                if n >= cap && !open {
                    hidden += 1;
                    continue;
                }
                out.push(L::Row(i));
                if !open {
                    continue;
                }
                for t in 1..row.things.len() {
                    out.push(L::Thing(i, t));
                }
                let Some(trail) = v.trail.as_ref().filter(|t| t.key == row.key) else {
                    out.push(L::Note("…".into()));
                    continue;
                };
                if let Some(l) = &trail.left_off {
                    if l.you.is_some() {
                        out.push(L::Left(i, false));
                    }
                    if l.reply.is_some() {
                        out.push(L::Left(i, true));
                    }
                }
                let mut day = String::new();
                for (e, entry) in trail.entries.iter().enumerate() {
                    let d = day_of(entry.at_ms);
                    if d != day {
                        out.push(L::Day(d.clone()));
                        day = d;
                    }
                    out.push(L::Entry(i, e));
                }
                if !trail.hot.is_empty() {
                    out.push(L::Hot(i));
                }
                if trail.left_off.is_none() && trail.entries.is_empty() {
                    out.push(L::Note(match (v.memory, row.project.is_some(), &row.machine) {
                        (Memory::Off, ..) => {
                            "the knowledge base is off: a trail needs it (Settings → Knowledge → enabled)".into()
                        }
                        (Memory::Withheld, ..) => "nothing more to show here".into(),
                        (_, false, _) => "sessions of no project have no trail".into(),
                        (_, _, Some(m)) => format!("its trail is on {m}: open ptyro there to see it"),
                        _ => "nothing kept of it yet: its trail fills in as agents work here".into(),
                    }));
                }
            }
            if hidden > 0 {
                out.push(L::More(section, hidden));
            }
        }
        out
    }

    fn sel_of(&self, l: &L) -> Option<Sel> {
        let v = self.view.as_ref()?;
        Some(match l {
            L::Row(i) => Sel::Row(v.rows[*i].key.clone()),
            L::Thing(i, t) => Sel::Thing(v.rows[*i].key.clone(), v.rows[*i].things[*t].key.clone()),
            L::Left(i, _) => Sel::Left(v.rows[*i].key.clone()),
            L::Entry(_, e) => Sel::Entry(v.trail.as_ref()?.entries.get(*e)?.addr.clone()),
            L::More(s, _) => Sel::More(*s),
            _ => return None,
        })
    }

    /// Every line that can be selected, in order, each once.
    fn stops(&self) -> Vec<Sel> {
        let mut out: Vec<Sel> = Vec::new();
        for l in self.lines() {
            if let Some(s) = self.sel_of(&l)
                && out.last() != Some(&s)
            {
                out.push(s);
            }
        }
        out
    }

    /// The selection, or the first line when it is gone.
    fn current(&self) -> Option<Sel> {
        let stops = self.stops();
        match &self.cur {
            Some(c) if stops.contains(c) => Some(c.clone()),
            // A thing that went (answered, put away): its row is still the place.
            Some(Sel::Thing(k, _) | Sel::Left(k)) if stops.contains(&Sel::Row(k.clone())) => {
                Some(Sel::Row(k.clone()))
            }
            _ => stops.into_iter().next(),
        }
    }

    fn step(&mut self, by: isize) {
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let at = self.current().and_then(|c| stops.iter().position(|s| *s == c)).unwrap_or(0);
        let to = (at as isize + by).clamp(0, stops.len() as isize - 1) as usize;
        self.cur = Some(stops[to].clone());
    }

    /// The row the selection is in, and the thing it is on.
    fn at(&self) -> Option<(&home::HomeRow, Option<&Thing>)> {
        let v = self.view.as_ref()?;
        match self.current()? {
            Sel::Row(k) => v.row(&k).map(|r| (r, Some(r.next()))),
            Sel::Thing(k, t) => v.row(&k).map(|r| (r, r.things.iter().find(|x| x.key == t))),
            Sel::Left(k) => v.row(&k).map(|r| (r, None)),
            Sel::Entry(_) => self.open.as_deref().and_then(|k| v.row(k)).map(|r| (r, None)),
            Sel::More(_) => None,
        }
    }
}

/// A moment's local date: `today`, `yesterday`, else `2026-11-11`.
fn day_of(ms: u64) -> String {
    let date = |ms: u64| minute(crate::clock::local_ms(ms))[..10].to_owned();
    let (d, now) = (date(ms), now_ms());
    if d == date(now) {
        "today".into()
    } else if d == date(now.saturating_sub(86_400_000)) {
        "yesterday".into()
    } else {
        d
    }
}

impl App {
    fn home_screen(&self) -> Option<&HomeScreen> {
        match &self.chat_panel {
            Some(ChatPanel::Home(h)) => Some(h),
            _ => None,
        }
    }

    fn home_mut(&mut self) -> Option<&mut HomeScreen> {
        match &mut self.chat_panel {
            Some(ChatPanel::Home(h)) => Some(h),
            _ => None,
        }
    }

    /// `home` (`H`): show it; again, with it shown and focused, closes it.
    pub(crate) fn toggle_home(&mut self) {
        if self.home_screen().is_some() && self.focus == Focus::Pane {
            return self.close_chat();
        }
        self.show_home_screen();
    }

    pub(crate) fn show_home_screen(&mut self) {
        if self.home_screen().is_none() {
            self.leave_chat();
            self.chat_panel = Some(ChatPanel::Home(Box::new(HomeScreen::new())));
        }
        self.chat_takes_keys();
        self.dirty = true;
        self.home_ask(true, None);
    }

    /// The view a bare `ptyro` asked for as it started: home opens on it, unless there is
    /// nothing to show (no project, nothing that wants you) or something else took the screen
    /// meanwhile. An older daemon, or none: the panes, as before.
    pub(crate) fn home_started(&mut self, res: Result<Reply, ptyro_core::Error>) {
        let Ok(Reply::Home { home }) = res else { return };
        if home.rows.is_empty() || self.overlay.is_some() || self.chat_panel.is_some() {
            return;
        }
        self.chat_panel = Some(ChatPanel::Home(Box::new(HomeScreen::new())));
        self.chat_takes_keys();
        self.home_got(Ok(Reply::Home { home }));
    }

    /// Ask for the view (with the unfolded row's trail). `here`: because of something you did.
    fn home_ask(&mut self, here: bool, dismiss: Option<HomeDismiss>) {
        let Some(h) = self.home_mut() else { return };
        (h.asked, h.stale) = (Instant::now(), false);
        let trail = h.open.clone();
        self.request(Pending::HomeView, Request::Home { trail, dismiss, restore: false, here });
    }

    /// Something changed (any event of the daemon's): the view is asked for again shortly.
    pub(crate) fn home_news(&mut self) {
        if let Some(h) = self.home_mut() {
            h.stale = true;
        }
    }

    pub(crate) fn home_tick(&mut self) {
        let Some(h) = self.home_screen() else { return };
        let due = if h.stale { SETTLE } else { AGES };
        if h.asked.elapsed() >= due && self.client().is_some() {
            self.home_ask(false, None);
        }
    }

    pub(crate) fn home_got(&mut self, res: Result<Reply, ptyro_core::Error>) {
        let Some(h) = self.home_mut() else { return };
        match res {
            Ok(Reply::Home { home }) => {
                // A row that is gone can't stay unfolded.
                if h.open.as_ref().is_some_and(|k| home.row(k).is_none()) {
                    h.open = None;
                }
                (h.view, h.error) = (Some(*home), None);
            }
            Ok(_) => {}
            Err(e) if e.code == ptyro_core::ErrorCode::Unsupported => {
                h.error = Some(
                    "this daemon is older than home: `ptyro daemon upgrade` brings it up to date"
                        .into(),
                )
            }
            Err(e) => {
                if h.view.is_none() {
                    h.error = Some(e.message.clone());
                }
                self.toast_err(e.message);
            }
        }
        self.dirty = true;
    }

    pub(crate) fn home_key(&mut self, mut h: Box<HomeScreen>, key: KeyEvent) {
        let ctrl = key.mods.has(Mods::CTRL);
        let ch = match key.code {
            KeyCode::Char { c } if !ctrl => Some(c),
            _ => None,
        };
        let cur = h.current();
        let row_key = h.at().map(|(r, _)| r.key.clone());
        let mut act: Option<char> = None;
        match (key.code, ch) {
            (KeyCode::Esc, _) | (_, Some('q')) => return self.dirty = true,
            (KeyCode::Up, _) | (_, Some('k')) => h.step(-1),
            (KeyCode::Down, _) | (_, Some('j')) => h.step(1),
            (KeyCode::PageUp, _) => h.step(-10),
            (KeyCode::PageDown, _) => h.step(10),
            (KeyCode::Home, _) | (_, Some('g')) => h.step(isize::MIN / 2),
            (KeyCode::End, _) | (_, Some('G')) => h.step(isize::MAX / 2),
            // Unfold the row the cursor is in; again, or ←, folds it.
            (KeyCode::Right | KeyCode::Tab, _) | (_, Some('l' | ' ')) => match (&cur, row_key) {
                (Some(Sel::More(s)), _) => h.full.push(*s),
                (_, Some(k)) if h.open.as_deref() == Some(k.as_str()) => {
                    (h.open, h.cur) = (None, Some(Sel::Row(k)));
                }
                (_, Some(k)) => {
                    (h.open, h.cur) = (Some(k.clone()), Some(Sel::Row(k)));
                    act = Some('t');
                }
                _ => {}
            },
            (KeyCode::Left, _) | (_, Some('h')) => {
                if let Some(k) = h.open.take() {
                    h.cur = Some(Sel::Row(k));
                }
            }
            (KeyCode::Enter, _) => act = Some('\n'),
            (_, Some(c @ ('n' | 'c' | 'p' | 'x' | 'u' | 'a' | 'r' | 'm' | '/'))) => act = Some(c),
            _ => {}
        }
        // What the selection is, before the screen goes back into the app.
        let at = h.at().map(|(r, t)| (r.clone(), t.cloned()));
        let cur = h.current();
        let entry = match &cur {
            Some(Sel::Entry(addr)) => h
                .view
                .as_ref()
                .and_then(|v| v.trail.as_ref())
                .and_then(|t| t.entries.iter().find(|e| e.addr == *addr))
                .cloned(),
            _ => None,
        };
        let left = match &cur {
            Some(Sel::Left(_)) => {
                h.view.as_ref().and_then(|v| v.trail.as_ref()).and_then(|t| t.left_off.clone())
            }
            _ => None,
        };
        if act == Some('m') {
            h.full = Section::ALL.to_vec();
        }
        self.chat_panel = Some(ChatPanel::Home(h));
        self.dirty = true;
        let Some(act) = act else { return };
        match (act, cur) {
            ('t', _) => self.home_ask(true, None),
            ('r', _) => self.home_ask(true, None),
            ('a', _) => self.open_home(),
            ('u', _) => {
                self.request(
                    Pending::HomeView,
                    Request::Home {
                        trail: self.home_screen().and_then(|h| h.open.clone()),
                        dismiss: None,
                        restore: true,
                        here: true,
                    },
                );
                self.toast("everything put away is back");
            }
            ('\n', Some(Sel::More(s))) => {
                if let Some(h) = self.home_mut() {
                    h.full.push(s);
                }
            }
            ('\n', Some(Sel::Entry(_))) => {
                if let Some(e) = entry {
                    self.home_open_item(e.session.as_deref(), &e.addr);
                }
            }
            ('\n', Some(Sel::Left(_))) => {
                if let Some(l) = left
                    && let Some(addr) = &l.addr
                {
                    self.home_open_item(l.session.as_deref(), addr);
                }
            }
            (_, _) => {
                let Some((row, thing)) = at else { return };
                match act {
                    '\n' => {
                        if let Some(t) = thing {
                            self.home_go(&row, &t)
                        }
                    }
                    'n' => self.home_task(&row, String::new()),
                    'c' => {
                        let addr = thing.as_ref().and_then(|t| t.item.clone()).or_else(|| {
                            let h = self.home_screen()?;
                            let t = h.view.as_ref()?.trail.as_ref().filter(|t| t.key == row.key)?;
                            t.left_off.as_ref()?.addr.clone()
                        });
                        match addr {
                            Some(a) => self.home_task(
                                &row,
                                format!("Pick up where this was left off (read it first: `ptyro kb show {a}`). "),
                            ),
                            None => self.toast(
                                "nothing kept to continue from here: `n` starts a new task in it",
                            ),
                        }
                    }
                    'p' => match thing.as_ref().and_then(|t| self.home_session(t)) {
                        Some(id) => self.open_peek(id),
                        None => self.toast("no session here to peek at"),
                    },
                    'x' => match thing {
                        Some(t) if t.next.dismissable() => {
                            self.home_ask(true, Some(HomeDismiss { key: t.key, stamp: t.stamp }));
                            self.toast("put away until it changes · u brings everything back");
                        }
                        Some(t) => self.toast(format!(
                            "\"{}\" can't be put away: only what was left hanging can",
                            t.next.verb()
                        )),
                        None => {}
                    },
                    '/' => match &row.project {
                        Some(_) => self.home_library(&row),
                        None => self.run(&Command { action: "kb.open", arg: None }),
                    },
                    _ => {}
                }
            }
        }
    }

    /// The session a thing is about, if ptyro still lists it.
    fn home_session(&self, t: &Thing) -> Option<ptyro_core::SessionId> {
        let id = t.session.as_deref()?;
        self.sessions.iter().find(|s| s.id.full().eq_ignore_ascii_case(id)).map(|s| s.id)
    }

    /// ⏎ on a thing: go where it is. A session is shown (a sleeping one woken); work with
    /// nothing running opens its worktree's quick terminal; what only the knowledge base
    /// holds is read; a project with nothing open gets the task box.
    fn home_go(&mut self, row: &home::HomeRow, t: &Thing) {
        if let Some(id) = self.home_session(t) {
            if self.session(id).is_some_and(|s| s.state == ProcessState::Dormant) {
                self.request(Pending::Done("resumed".into()), Request::Resume { addr: id.full() });
            }
            return self.pick(id);
        }
        match t.next {
            Next::Commit | Next::Push => {
                let rows = self.rows();
                let Some(i) = t
                    .worktree
                    .as_ref()
                    .and_then(|w| rows.iter().position(|r| *r == Row::Worktree(w.clone())))
                else {
                    return self.toast_err(format!(
                        "{}'s worktree isn't in the sidebar (its project is folded, or on another machine)",
                        row.name
                    ));
                };
                self.close_chat();
                self.focus = Focus::Sidebar;
                self.set_sel(&rows, i);
                self.run(&Command { action: "shell.pane", arg: None });
            }
            Next::Nothing => self.home_task(row, String::new()),
            _ => match &t.item {
                Some(addr) => self.library_typed(&PromptKind::KbShow, addr),
                None => self.toast("nothing to go to here"),
            },
        }
    }

    /// A trail's line: its session when ptyro still lists it, else what was kept of it.
    fn home_open_item(&mut self, session: Option<&str>, addr: &str) {
        let found = session
            .and_then(|id| self.sessions.iter().find(|s| s.id.full().eq_ignore_ascii_case(id)))
            .map(|s| (s.id, s.state == ProcessState::Dormant));
        match found {
            Some((id, dormant)) => {
                if dormant {
                    self.request(
                        Pending::Done("resumed".into()),
                        Request::Resume { addr: id.full() },
                    );
                }
                self.pick(id);
            }
            None => self.library_typed(&PromptKind::KbShow, addr),
        }
    }

    /// The task box, in a row's project, with `text` already typed.
    fn home_task(&mut self, row: &home::HomeRow, text: String) {
        let Some(project) = &row.project else {
            return self.toast(
                "these sessions are in no project: pick a project's row, or `a` to add one",
            );
        };
        let rows = self.rows();
        let Some(i) = rows.iter().position(|r| *r == Row::Project(project.clone())) else {
            return self.toast_err(format!(
                "{} isn't in the sidebar (its workspace is folded): unfold it, then `n`",
                row.name
            ));
        };
        self.close_chat();
        self.focus = Focus::Sidebar;
        self.set_sel(&rows, i);
        self.run(&Command { action: "session.new", arg: None });
        if let Some(crate::app::Overlay::Task { text: typed, .. }) = &mut self.overlay {
            *typed = text;
        }
    }

    /// `/`: the Library, with only what was kept of this row's project.
    fn home_library(&mut self, row: &home::HomeRow) {
        let rows = self.rows();
        match row
            .project
            .as_ref()
            .and_then(|p| rows.iter().position(|r| *r == Row::Project(p.clone())))
        {
            Some(i) => {
                self.close_chat();
                self.focus = Focus::Sidebar;
                self.set_sel(&rows, i);
                self.run(&Command { action: "kb.history", arg: None });
            }
            None => self.run(&Command { action: "kb.open", arg: None }),
        }
    }

    pub(crate) fn home_mouse(&mut self, y: u16, up: Option<bool>) {
        self.dirty = true;
        let Some(h) = self.home_mut() else { return };
        match up {
            Some(up) => h.step(if up { -1 } else { 1 }),
            None => {
                let hit = h.drawn.borrow().iter().find(|(at, _)| *at == y).map(|(_, s)| s.clone());
                let Some(hit) = hit else { return };
                // A click selects; a click on the selection goes there.
                if h.current().as_ref() == Some(&hit) {
                    self.chat_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
                } else {
                    h.cur = Some(hit);
                }
            }
        }
    }
}

fn pad(s: &str, w: usize) -> String {
    let have = s.width();
    if have >= w { s.to_owned() } else { format!("{s}{}", " ".repeat(w - have)) }
}

/// `s` cut to `w` columns, `…` when cut.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) + 1 > w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

pub(crate) fn draw(app: &App, f: &mut Frame<'_>, area: Rect, h: &HomeScreen) {
    h.drawn.borrow_mut().clear();
    if area.height < 5 || area.width < 30 {
        return;
    }
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let w = area.width as usize;
    let now = h.view.as_ref().map_or_else(now_ms, |v| v.at_ms.max(now_ms()));

    // The head: what stands where, and what happened while you were away.
    let mut head = vec![Span::styled(" Home ", fg(app, t.accent).add_modifier(Modifier::BOLD))];
    if let Some(v) = &h.view {
        let c = v.counts();
        let mut part = |text: String, style: Style| {
            head.push(Span::styled(if head.len() > 1 { " · " } else { " " }, dim));
            head.push(Span::styled(text, style));
        };
        if c.needs > 0 {
            part(
                format!("{} need{} you", c.needs, if c.needs == 1 { "s" } else { "" }),
                fg(app, t.blocked).add_modifier(Modifier::BOLD),
            );
        }
        if c.hanging > 0 {
            part(format!("{} left hanging", c.hanging), fg(app, t.idle));
        }
        if c.working > 0 {
            part(format!("{} working", c.working), fg(app, t.working));
        }
        if c.needs + c.hanging + c.working == 0 {
            part("all quiet".into(), dim);
        }
        if let Some(a) = &v.away {
            part(a.line(), fg(app, t.accent));
        }
    }
    f.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..area });

    // The foot: every meter, wrapped, then the keys.
    let mut foot: Vec<Line<'static>> = Vec::new();
    if let Some(v) = &h.view {
        let mut line: Vec<Span<'static>> = vec![Span::raw(" ")];
        let mut used = 1;
        for m in &v.meters {
            let text = m.text();
            if used > 1 && used + text.width() + 3 > w {
                foot.push(Line::from(std::mem::replace(&mut line, vec![Span::raw(" ")])));
                used = 1;
            }
            if used > 1 {
                line.push(Span::raw("   "));
                used += 3;
            }
            used += text.width();
            line.push(Span::styled(format!("{} ", m.label), dim));
            for (i, p) in m.parts.iter().enumerate() {
                if i > 0 {
                    line.push(Span::styled(" · ", dim));
                }
                let style = match p.level {
                    Level::Limited => fg(app, t.error).add_modifier(Modifier::BOLD),
                    Level::Warn => fg(app, t.working),
                    Level::Old | Level::Unknown | Level::Quiet => dim,
                };
                line.push(Span::styled(p.text.clone(), style));
            }
        }
        if used > 1 {
            foot.push(Line::from(line));
        }
    }
    let on_thing = h.at().and_then(|(_, t)| t.cloned());
    let mut keys = vec!["⏎ go", "→ unfold", "n new task", "c continue"];
    if on_thing.as_ref().is_some_and(|t| t.session.is_some()) {
        keys.push("p peek");
    }
    if on_thing.as_ref().is_some_and(|t| t.next.dismissable()) {
        keys.push("x put away");
    }
    keys.extend(["/ its history", "a add a repository", "r refresh", "Esc close"]);
    foot.push(Line::styled(
        format!(" {}", fit(&keys.join(" · "), w.saturating_sub(2))),
        fg(app, t.accent),
    ));
    let foot_h = (foot.len() as u16).min(area.height.saturating_sub(3));
    let foot_at = area.y + area.height - foot_h;
    f.render_widget(
        Paragraph::new(foot.into_iter().take(foot_h as usize).collect::<Vec<_>>()),
        Rect { y: foot_at, height: foot_h, ..area },
    );

    let body = Rect { y: area.y + 2, height: foot_at.saturating_sub(area.y + 3), ..area };
    if body.height == 0 {
        return;
    }
    let Some(v) = &h.view else {
        let text = h.error.clone().unwrap_or_else(|| "looking…".into());
        f.render_widget(Paragraph::new(Line::styled(format!("  {text}"), dim)), body);
        return;
    };
    if v.rows.is_empty() {
        let lines = vec![
            Line::styled("  No projects yet.", bold),
            Line::styled(
                "  a  add a repository (or run `ptyro` inside one: it is added by itself)",
                dim,
            ),
        ];
        f.render_widget(Paragraph::new(lines), body);
        return;
    }
    let lines = h.lines();
    let cur = h.current();
    let name_w = v.rows.iter().map(|r| r.name.width()).max().unwrap_or(0).min(w / 4).max(4);
    let weeks = v.rows.iter().any(|r| !r.week.is_empty());
    let sel_style = |on: bool| {
        if on { Style::new().bg(app.depth.fit(t.selection_bg)) } else { Style::new() }
    };
    let verb_style = |n: Next| match n {
        Next::Answer | Next::Look => fg(app, t.blocked).add_modifier(Modifier::BOLD),
        Next::Failed => fg(app, t.error).add_modifier(Modifier::BOLD),
        Next::Review => fg(app, t.done).add_modifier(Modifier::BOLD),
        Next::Working => fg(app, t.working),
        Next::Limit => fg(app, t.exited),
        n if n.section() == Section::Hanging => fg(app, t.idle),
        _ => dim,
    };
    // A thing's line: the verb, what was said, and at the right where and since when.
    let thing_spans = |lead: Vec<Span<'static>>, th: &Thing, more: usize, on: bool| {
        let st = sel_style(on);
        let mut tail: Vec<String> = Vec::new();
        tail.extend(th.place().map(str::to_owned));
        tail.extend(th.left.clone());
        if more > 0 {
            tail.push(format!("+{more}"));
        }
        if th.at_ms > 0 {
            tail.push(dur(now.saturating_sub(th.at_ms)));
        }
        let tail = tail.join(" · ");
        let lead_w: usize = lead.iter().map(|s| s.content.width()).sum();
        let verb = pad(th.next.verb(), 9);
        let room = w.saturating_sub(lead_w + verb.width() + 2 + tail.width() + 3);
        let text = match (th.next, th.text.is_empty()) {
            (Next::Nothing, _) => "nothing open".to_owned(),
            (_, true) => String::new(),
            _ => th.text.clone(),
        };
        let text = fit(&text, room);
        let gap = w.saturating_sub(lead_w + verb.width() + 2 + text.width() + tail.width() + 1);
        let quiet = th.next.section() == Section::Quiet;
        let mut spans = lead;
        spans.push(Span::styled(verb, verb_style(th.next).patch(st)));
        spans.push(Span::styled("  ", st));
        spans.push(Span::styled(text, if quiet { dim.patch(st) } else { st }));
        spans.push(Span::styled(" ".repeat(gap), st));
        spans.push(Span::styled(tail, dim.patch(st)));
        spans.push(Span::styled(" ", st));
        Line::from(spans)
    };
    let mut drawn: Vec<(Line<'static>, Option<Sel>)> = Vec::new();
    for l in &lines {
        let sel = h.sel_of(l);
        let on = sel.is_some() && sel == cur;
        let st = sel_style(on);
        let line = match l {
            L::Blank => Line::raw(""),
            L::Head(s, n) => {
                let style = match s {
                    Section::Needs => fg(app, t.blocked).add_modifier(Modifier::BOLD),
                    Section::Hanging => fg(app, t.idle).add_modifier(Modifier::BOLD),
                    Section::Working => fg(app, t.working).add_modifier(Modifier::BOLD),
                    Section::Quiet => dim.add_modifier(Modifier::BOLD),
                };
                Line::from(vec![
                    Span::styled(format!(" {}", s.title()), style),
                    Span::styled(format!("  {n}"), dim),
                ])
            }
            L::Row(i) => {
                let r = &v.rows[*i];
                let open = h.open.as_deref() == Some(r.key.as_str());
                let mark = match (on, open) {
                    (_, true) => " ▾ ",
                    (true, false) => " ▸ ",
                    _ => "   ",
                };
                let name = match &r.machine {
                    Some(m) => format!("{m}:{}", r.name),
                    None => r.name.clone(),
                };
                let mut lead = vec![
                    Span::styled(mark, fg(app, t.accent).patch(st)),
                    Span::styled(
                        pad(&fit(&name, name_w), name_w),
                        if r.section == Section::Quiet { dim.patch(st) } else { bold.patch(st) },
                    ),
                    Span::styled("  ", st),
                ];
                if weeks {
                    lead.push(Span::styled(pad(&home::spark(&r.week), 7), dim.patch(st)));
                    lead.push(Span::styled("  ", st));
                }
                thing_spans(lead, r.next(), r.things.len() - 1, on)
            }
            L::Thing(i, n) => {
                let lead =
                    vec![Span::styled(" ".repeat(name_w + 5 + if weeks { 9 } else { 0 }), st)];
                thing_spans(lead, &v.rows[*i].things[*n], 0, on)
            }
            L::Left(_, reply) => {
                let l = v.trail.as_ref().and_then(|t| t.left_off.as_ref());
                let (who, said) = match (reply, l) {
                    (false, Some(l)) => ("you".to_owned(), l.you.clone().unwrap_or_default()),
                    (true, Some(l)) => (
                        l.agent.clone().unwrap_or_else(|| "agent".into()),
                        l.reply.clone().unwrap_or_default(),
                    ),
                    _ => (String::new(), String::new()),
                };
                let label =
                    if *reply && l.is_some_and(|l| l.you.is_some()) { "" } else { "left off" };
                let lead = format!("      {}  {who}: ", pad(label, 9));
                let room = w.saturating_sub(lead.width() + 1);
                Line::from(vec![
                    Span::styled(lead, dim.patch(st)),
                    Span::styled(pad(&fit(&said, room), room + 1), st),
                ])
            }
            L::Day(d) => Line::styled(format!("      {d}"), dim.add_modifier(Modifier::ITALIC)),
            L::Entry(_, e) => {
                let Some(e) = v.trail.as_ref().and_then(|t| t.entries.get(*e)) else { continue };
                let lead = format!("        {}  ", pad(&e.kind, 7));
                let detail =
                    if e.detail.is_empty() { String::new() } else { format!("  {}", e.detail) };
                let room = w.saturating_sub(lead.width() + detail.width() + 1);
                let title = fit(&e.title, room);
                let gap = w.saturating_sub(lead.width() + title.width() + detail.width());
                Line::from(vec![
                    Span::styled(lead, dim.patch(st)),
                    Span::styled(title, st),
                    Span::styled(format!("{detail}{}", " ".repeat(gap)), dim.patch(st)),
                ])
            }
            L::Hot(_) => {
                let hot: Vec<String> = v
                    .trail
                    .iter()
                    .flat_map(|t| &t.hot)
                    .map(|x| format!("{} ×{}", x.path, x.count))
                    .collect();
                Line::styled(
                    fit(&format!("      hot files  {}", hot.join(" · ")), w.saturating_sub(1)),
                    dim,
                )
            }
            L::Note(n) => Line::styled(fit(&format!("      {n}"), w.saturating_sub(1)), dim),
            L::More(_, n) => {
                Line::styled(pad(&format!("   … {n} more (⏎ shows them)"), w), dim.patch(st))
            }
        };
        drawn.push((line, sel));
    }
    // Keep the selection in view, with a line of what is around it.
    let rows = body.height as usize;
    let at = drawn.iter().position(|(_, s)| s.is_some() && *s == cur).unwrap_or(0);
    let mut skip = h.scroll.get().min(drawn.len().saturating_sub(rows));
    if at < skip + 1 {
        skip = at.saturating_sub(1);
    } else if at + 2 > skip + rows {
        skip = (at + 2).saturating_sub(rows);
    }
    h.scroll.set(skip);
    let mut hits = h.drawn.borrow_mut();
    let shown: Vec<Line<'static>> = drawn
        .into_iter()
        .enumerate()
        .skip(skip)
        .take(rows)
        .map(|(i, (line, sel))| {
            if let Some(s) = sel {
                hits.push((body.y + (i - skip) as u16, s));
            }
            line
        })
        .collect();
    f.render_widget(Paragraph::new(shown), body);
}

#[cfg(test)]
mod tests;
