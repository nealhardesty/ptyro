//! The Library (SPEC §25.10, ROADMAP KB1.8): what the knowledge base kept, searched and read in
//! one panel over the panes (`kb.open`, `C-q L`). A search box, a row of filters (kind,
//! project, since), the hits on the left and the selected one with its neighbours on the right;
//! with nothing typed it is the history, by day. `⏎` opens the source: a chat at its message,
//! anything else in the reader (its stored pieces as a transcript, drawn by the chat view's
//! renderer). `r` goes to the session it happened in, `d` forgets (the daemon asks first, in
//! its words), `s` shows how the knowledge base stands.
//!
//! It is one of the chat panel's shapes (`ChatPanel::Library`), so the keys, the mouse, `` ` ``
//! back and forth and Esc from a chat opened here work as they do for the chat list. What it
//! shows is asked of the daemon (`KbSearch`, `KbRecent`, `KbRead`, `KbStatus`) and asked again
//! when the daemon says the knowledge base changed (`KbUpdated`).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use ptyro_core::chat::{ChatBody, ChatItem, NoticeLevel, ToolStatus};
use ptyro_core::chats::{MARK_OFF, MARK_ON};
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::kb::card::minute;
use ptyro_core::kb::search::{HitKind, KbHit, KbListed, KbStatusInfo, pieces};
use ptyro_core::kb::{ChunkKind, ItemKind, KbChunk};
use ptyro_core::session::ProcessState;
use ptyro_core::{ErrorCode, now_ms};
use ptyro_protocol::{Reply, Request};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{App, Confirmed, Focus, Overlay, Pending, PromptKind, Purpose};
use crate::chat::ChatView;
use crate::chats::ChatPanel;
use crate::palette::{Item, Palette, Target};
use crate::tree::Row;
use crate::ui::fg;

/// A search starts at this many characters, as the chat list's and the web's do; fewer is the
/// history.
const MIN_QUERY: usize = 3;
/// The narrowest panel that shows the selected row's text beside the list.
const TWO_COLUMNS: u16 = 80;
/// How many rows a page key moves.
const PAGE: usize = 10;

/// What the Library's menu (`row.menu`) offers.
pub(crate) const ACTIONS: &[&str] =
    &["kb.search", "kb.recent", "kb.show", "kb.forget", "kb.status", "kb.open"];

/// Asks are numbered, so an answer to an older one (the query has moved on) is dropped.
static NEXT_ASK: AtomicU64 = AtomicU64::new(1);

/// The Library's state.
pub struct Library {
    pub query: String,
    /// Typing into the search box.
    pub typing: bool,
    pub kind: Option<HitKind>,
    /// (id, what it is called).
    pub project: Option<(String, String)>,
    pub since: Option<&'static str>,
    /// A row's history: only what was kept of this session or worktree.
    pub of: Option<Of>,
    /// The items that history holds (their ids): what a search within it keeps.
    of_items: Vec<String>,
    /// The filter `Tab` is on (`None`: the list has the keys).
    pub chip: Option<Filter>,
    pub rows: Rows,
    /// The last ask's number: the answer that counts.
    asked: u64,
    /// Nothing has answered yet.
    pub loading: bool,
    /// Why the search was by words alone (`KbHits.words_only`).
    pub words_only: Option<String>,
    /// What the daemon said of the query instead of answering it.
    pub note: Option<String>,
    pub sel: usize,
    pub status: Option<KbStatusInfo>,
    /// The status is shown in place of the selected row's text.
    pub show_status: bool,
    pub preview: Option<Preview>,
    pub reader: Option<Reader>,
    /// Asked once whether to switch meaning search on (`by:meaning` while it is off).
    meaning_asked: bool,
    /// Where each row was drawn (screen row, row), for a click.
    drawn: RefCell<Vec<(u16, usize)>>,
}

/// What the list holds.
pub enum Rows {
    /// Nothing typed: the history, newest first.
    History(Vec<KbListed>),
    Hits(Vec<KbHit>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Self::History(v) => v.len(),
            Self::Hits(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a row's history is of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Of {
    /// A ptyro session (its id, its name).
    Session(String, String),
    /// A worktree (its id, its name).
    Worktree(String, String),
}

impl Of {
    fn label(&self) -> String {
        match self {
            Self::Session(_, name) => format!("session {name}"),
            Self::Worktree(_, name) => format!("worktree {name}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Kind,
    Project,
    Since,
}

impl Filter {
    const ALL: [Self; 3] = [Self::Kind, Self::Project, Self::Since];
}

/// How far back `since` may look: what the pick offers.
const SINCE: &[(&str, &str)] = &[
    ("1d", "the last day"),
    ("7d", "the last week"),
    ("30d", "the last 30 days"),
    ("90d", "the last 90 days"),
    ("365d", "the last year"),
];

/// The selected row's text: the piece that matched with its neighbours, or an item's start.
pub struct Preview {
    /// The address it was asked for (the selected row's).
    pub addr: String,
    pub item: Option<KbListed>,
    pub chunks: Vec<KbChunk>,
    pub at: Option<u32>,
}

/// An item read whole (`⏎`): its pieces as a transcript, read only.
pub struct Reader {
    pub item: KbListed,
    pub view: ChatView,
    /// The pieces that hold what `/` looked for, and the one shown.
    found: Vec<String>,
    found_at: usize,
}

impl Library {
    fn new() -> Self {
        Self {
            query: String::new(),
            typing: false,
            kind: None,
            project: None,
            since: None,
            of: None,
            of_items: Vec::new(),
            chip: None,
            rows: Rows::History(Vec::new()),
            asked: 0,
            loading: true,
            words_only: None,
            note: None,
            sel: 0,
            status: None,
            show_status: false,
            preview: None,
            reader: None,
            meaning_asked: false,
            drawn: RefCell::new(Vec::new()),
        }
    }

    /// The query is one to search for (else the list is the history).
    fn searching(&self) -> bool {
        self.query.trim().chars().count() >= MIN_QUERY
    }

    /// The knowledge base is switched off (as far as the daemon has said).
    fn off(&self) -> bool {
        self.status.as_ref().is_some_and(|s| !s.enabled)
    }

    /// The selected row's address (a piece's, an item's, a chat's message), and the item's.
    fn selected(&self) -> Option<(String, String)> {
        match &self.rows {
            Rows::Hits(h) => h.get(self.sel).map(|h| {
                let item = h.addr.split_once('/').map_or(h.addr.as_str(), |(i, _)| i);
                (h.addr.clone(), item.to_owned())
            }),
            Rows::History(i) => i.get(self.sel).map(|i| (i.addr.clone(), i.addr.clone())),
        }
    }

    /// The selected row is a chat's (it lives in `chat.db`: opened and deleted as a chat).
    fn selected_chat(&self) -> Option<&KbHit> {
        match &self.rows {
            Rows::Hits(h) => h.get(self.sel).filter(|h| h.kind == HitKind::Chat),
            Rows::History(_) => None,
        }
    }

    /// What the list asks the daemon for: the search, or (nothing typed) the history, under
    /// the filters. `here`: the project you are in, whose hits rank higher. `None`: the
    /// history of chats, which the knowledge base has none of.
    fn request(&self, here: Option<String>) -> Option<Request> {
        let project = self.project.as_ref().map(|(id, _)| id.clone());
        if self.searching() {
            let mut query = self.query.trim().to_owned();
            if let Some(s) = self.since {
                query.push_str(&format!(" since:{s}"));
            }
            // A row's history keeps only its own of what is found: ask for more.
            let limit = if self.of.is_some() { ptyro_core::kb::search::MAX_LIMIT } else { 30 };
            return Some(Request::KbSearch {
                query,
                project,
                kinds: self.kind.into_iter().collect(),
                since_ms: None,
                before_ms: None,
                limit: Some(limit),
                here,
            });
        }
        let kinds = match self.kind.map(HitKind::stored) {
            Some(None) => return None,
            Some(Some((kind, _))) => vec![kind],
            None => Vec::new(),
        };
        let since_ms = self
            .since
            .and_then(|s| ptyro_core::config::parse_duration(s).ok())
            .map(|d| now_ms().saturating_sub(d.as_millis() as u64));
        let (session, worktree) = match &self.of {
            Some(Of::Session(id, _)) => (Some(id.clone()), None),
            Some(Of::Worktree(id, _)) => (None, Some(id.clone())),
            None => (None, None),
        };
        Some(Request::KbRecent {
            kinds,
            project,
            session,
            worktree,
            since_ms,
            before_ms: None,
            limit: Some(ptyro_core::kb::search::MAX_LIMIT),
        })
    }

    /// The kinds the kind filter offers: sessions and chats, and whatever else is kept.
    fn kinds(&self) -> Vec<HitKind> {
        let held: Vec<ItemKind> = self
            .status
            .iter()
            .flat_map(|s| s.kinds.iter().filter(|k| k.items > 0).map(|k| k.kind))
            .collect();
        HitKind::ALL
            .iter()
            .copied()
            .filter(|k| match k.stored() {
                None => true,
                Some((ItemKind::Session, _)) => true,
                Some((item, _)) => held.contains(&item),
            })
            .collect()
    }
}

/// When an item happened: its end, else its start, else when it was kept.
fn at(i: &KbListed) -> u64 {
    i.item.ended_ms.or(i.item.started_ms).or(i.item.ingested_ms).unwrap_or(0)
}

/// `2026-11-19 22:38`, local time.
fn when(ms: u64) -> String {
    minute(crate::clock::local_ms(ms))
}

/// What an item is called in a list: its title, its path, else its URI.
fn title(i: &KbListed) -> String {
    let t = i.item.title.as_deref().or(i.item.rel.as_deref()).unwrap_or(i.item.uri.as_str());
    t.replace('\n', " ")
}

/// The day an item is listed under: `Today`, `Yesterday`, else `Tue 2026-11-17`. Both
/// moments are local ones ([`crate::clock::local_ms`]).
fn day(ms: u64, now: u64) -> String {
    const DAY: u64 = 86_400_000;
    match (now / DAY).saturating_sub(ms / DAY) {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            let when = minute(ms);
            // (1970-01-01 was a Thursday.)
            format!("{} {}", names[((ms / DAY + 4) % 7) as usize], &when[..10.min(when.len())])
        }
    }
}

/// The words a snippet marks as matched, lowercased, each once.
fn matched(snippet: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, part) in snippet.split([MARK_ON, MARK_OFF]).enumerate() {
        let word = part.trim().to_lowercase();
        if i % 2 == 1 && !word.is_empty() && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// A snippet's text, its matches bold (the marks are control characters, never shown).
fn snippet_spans(app: &App, snippet: &str) -> Vec<Span<'static>> {
    let t = &app.theme;
    let flat = snippet.replace('\n', " ");
    flat.split([MARK_ON, MARK_OFF])
        .enumerate()
        .filter(|(_, part)| !part.is_empty())
        .map(|(i, part)| {
            let st = if i % 2 == 1 {
                fg(app, t.search).add_modifier(Modifier::BOLD)
            } else {
                fg(app, t.dim)
            };
            Span::styled(part.to_owned(), st)
        })
        .collect()
}

/// A line of text with every one of `words` in it highlighted (case-insensitively).
fn lit_line(app: &App, line: &str, words: &[String], base: Style) -> Line<'static> {
    let hl = fg(app, app.theme.search).add_modifier(Modifier::BOLD);
    let low = line.to_lowercase();
    // (Lowercasing may change a character's length: then the line is shown plain.)
    if words.is_empty() || low.len() != line.len() {
        return Line::styled(line.to_owned(), base);
    }
    let mut spans = Vec::new();
    let mut from = 0;
    while from < line.len() {
        let next = words
            .iter()
            .filter_map(|w| low[from..].find(w.as_str()).map(|i| (from + i, w.len())))
            .min();
        match next {
            Some((i, n)) if line.is_char_boundary(i) && line.is_char_boundary(i + n) => {
                if i > from {
                    spans.push(Span::styled(line[from..i].to_owned(), base));
                }
                spans.push(Span::styled(line[i..i + n].to_owned(), hl));
                from = i + n;
            }
            _ => break,
        }
    }
    if from < line.len() {
        spans.push(Span::styled(line[from..].to_owned(), base));
    }
    Line::from(spans)
}

/// A stored piece as the chat view draws it.
fn chat_item(c: &KbChunk) -> ChatItem {
    let text = c.text.clone();
    let body = match c.kind {
        ChunkKind::User => ChatBody::User { text },
        ChunkKind::Assistant => ChatBody::Assistant { text, streaming: false },
        ChunkKind::Tool => {
            let (name, what) = text.split_once(' ').unwrap_or((text.as_str(), ""));
            ChatBody::Tool {
                name: name.to_owned(),
                title: what.to_owned(),
                status: ToolStatus::Done,
                output: None,
                diff: None,
                files: Vec::new(),
            }
        }
        ChunkKind::Card | ChunkKind::Summary => ChatBody::Notice { level: NoticeLevel::Info, text },
        // A document's section, a comment, a command: its text under where it stands.
        _ => ChatBody::Assistant {
            text: match &c.heading {
                Some(h) => format!("{h}\n{text}"),
                None => text,
            },
            streaming: false,
        },
    };
    ChatItem { id: c.idx.to_string(), at_ms: c.at_ms, rev: 0, body }
}

// ── opening, asking, answers ────────────────────────────────────────────────────────────────

impl App {
    fn library(&self) -> Option<&Library> {
        match &self.chat_panel {
            Some(ChatPanel::Library(l)) => Some(l),
            _ => None,
        }
    }

    fn library_mut(&mut self) -> Option<&mut Library> {
        match &mut self.chat_panel {
            Some(ChatPanel::Library(l)) => Some(l),
            _ => None,
        }
    }

    /// Show the Library (a fresh one unless it is shown already), give it the keys, and set
    /// it up with `f`.
    fn show_library(&mut self, f: impl FnOnce(&mut Library)) {
        let fresh = self.library().is_none();
        if fresh {
            self.leave_chat();
            self.chat_panel = Some(ChatPanel::Library(Box::new(Library::new())));
            self.request(Pending::KbStatus, Request::KbStatus);
        }
        self.chat_takes_keys();
        self.dirty = true;
        if let Some(l) = self.library_mut() {
            f(l);
        }
        if fresh {
            self.library_ask();
        }
    }

    /// A `kb.*` action, from a key, the palette or a row's menu.
    pub(crate) fn library_action(&mut self, action: &str) {
        match action {
            // `C-q L` again closes it, as the chat list's key does.
            "kb.open" if self.library().is_some() && self.focus == Focus::Pane => self.close_chat(),
            "kb.open" => self.show_library(|_| {}),
            "kb.search" => self.show_library(|l| {
                (l.typing, l.chip, l.reader, l.show_status) = (true, None, None, false);
            }),
            "kb.recent" => {
                self.show_library(|l| {
                    (l.typing, l.reader, l.show_status) = (false, None, false);
                    l.query.clear();
                });
                self.library_ask();
            }
            "kb.status" => {
                self.show_library(|l| (l.show_status, l.reader, l.typing) = (true, None, false));
                self.request(Pending::KbStatus, Request::KbStatus);
            }
            "kb.enable" => {
                if self.config.kb.enabled {
                    return self.toast("the knowledge base is on already");
                }
                self.set_config(
                    "kb.enabled",
                    "true",
                    "the knowledge base is on: agent sessions are kept from here on",
                );
                self.library_news();
            }
            "kb.history" => self.library_history(),
            // The selected row's, with the Library shown; else asked for by its address.
            "kb.show" if self.library().is_some_and(|l| l.selected().is_some()) => {
                self.library_open()
            }
            "kb.forget" if self.library().is_some_and(|l| l.selected().is_some()) => {
                self.library_forget()
            }
            "kb.show" | "kb.forget" => {
                let (title, kind) = if action == "kb.show" {
                    ("Open which (an address from a search, like k:7f3k/12)", PromptKind::KbShow)
                } else {
                    ("Forget which (an address from a search, like k:7f3k)", PromptKind::KbForget)
                };
                self.overlay =
                    Some(Overlay::Prompt { title: title.into(), text: String::new(), kind });
            }
            _ => {}
        }
    }

    /// An address typed for `kb.show` or `kb.forget`.
    pub(crate) fn library_typed(&mut self, kind: &PromptKind, text: &str) {
        let addr = text.trim().to_owned();
        if addr.is_empty() {
            return;
        }
        match kind {
            PromptKind::KbForget => self.forget_asked(addr),
            // A chat's address is the chats' to open (a citation's way).
            _ if addr.starts_with("c:") => self.chat_pick(Purpose::ChatCite, addr),
            _ => self.read_item(&addr),
        }
    }

    /// `kb.history`: the Library with only what was kept of the selected session, worktree or
    /// project (the pane's session, with the panes focused).
    fn library_history(&mut self) {
        let row = match (self.focus, self.selected_row(), self.focused_session()) {
            (Focus::Sidebar, Some(r), _) => Some(r),
            (_, _, Some(id)) => Some(Row::Session(id)),
            _ => None,
        };
        self.library_of(row);
    }

    /// The Library with only what was kept of `row`: a session, a worktree or a project.
    pub(crate) fn library_of(&mut self, row: Option<Row>) {
        let (mut of, mut project) = (None, None);
        match &row {
            Some(Row::Session(id) | Row::Quick(id)) => {
                of = self.session(*id).map(|s| Of::Session(id.full(), s.name.clone()));
            }
            Some(Row::Worktree(w)) => {
                of = self.worktree(w).map(|w| Of::Worktree(w.id.clone(), w.qualified()));
            }
            Some(Row::Project(p)) => {
                project = self.project(p).map(|p| (p.id.clone(), p.slug.clone()))
            }
            _ => {}
        }
        if of.is_none() && project.is_none() {
            // Anywhere else: the project you are in, when there is one.
            project = self
                .context()
                .0
                .and_then(|p| self.project(&p).map(|p| (p.id.clone(), p.slug.clone())));
        }
        if of.is_none() && project.is_none() {
            return self.toast("pick a session, a worktree or a project first (in the sidebar)");
        }
        self.show_library(|l| {
            (l.of, l.project, l.kind, l.since) = (of, project, None, None);
            (l.typing, l.reader, l.show_status, l.chip) = (false, None, false, None);
            l.of_items.clear();
            l.query.clear();
        });
        self.library_ask();
    }

    /// Ask the daemon for what the list shows now: the search, or the history.
    pub(crate) fn library_ask(&mut self) {
        let here = self.context().0;
        let Some(l) = self.library_mut() else { return };
        l.asked = NEXT_ASK.fetch_add(1, Ordering::Relaxed);
        let Some(req) = l.request(here) else {
            // A chat is no item: the history of chats is the chat list's.
            (l.rows, l.loading, l.preview) = (Rows::History(Vec::new()), false, None);
            l.note = Some("chats are in the chat list: type to search them here".into());
            return;
        };
        let asked = l.asked;
        self.request(Pending::KbRows(asked), req);
    }

    /// The knowledge base changed (`KbUpdated`), or its switch did: what is shown is asked for
    /// again. The selection stays on its row where that row still is.
    pub(crate) fn library_news(&mut self) {
        if self.library().is_some() {
            self.request(Pending::KbStatus, Request::KbStatus);
            self.library_ask();
        }
    }

    /// An answer to `library_ask`.
    pub(crate) fn library_rows(&mut self, asked: u64, res: Result<Reply, ptyro_core::Error>) {
        let Some(l) = self.library_mut() else { return };
        if l.asked != asked {
            return;
        }
        let before = l.selected().map(|(addr, _)| addr);
        (l.loading, l.note, l.words_only) = (false, None, None);
        match res {
            Ok(Reply::KbItems { items }) => {
                if l.of.is_some() {
                    l.of_items = items.iter().map(|i| i.item.id.clone()).collect();
                }
                l.rows = Rows::History(items);
            }
            Ok(Reply::KbHits { mut hits, words_only }) => {
                // A row's history: of what was found, only that session's or worktree's (the
                // history asked for before the typing began says which items those are).
                if l.of.is_some() {
                    hits.retain(|h| l.of_items.contains(&h.item));
                    hits.truncate(30);
                }
                l.words_only = words_only;
                l.rows = Rows::Hits(hits);
            }
            Ok(_) => {}
            // `by:meaning` while it is off: offer, once, to switch it on.
            Err(e) if e.code == ErrorCode::NeedsConfirmation => {
                l.rows = Rows::Hits(Vec::new());
                if !l.meaning_asked {
                    l.meaning_asked = true;
                    self.overlay = Some(Overlay::Confirm {
                        text: format!("{}. Switch it on? [y/N]", e.message),
                        action: Confirmed::MeaningOn,
                    });
                }
            }
            // Switched off: the status says so, and the panel offers to switch it on.
            Err(e) if e.code == ErrorCode::Unsupported => {
                l.rows = Rows::History(Vec::new());
                l.status.get_or_insert_with(KbStatusInfo::default).enabled = false;
            }
            // A query still being typed, or the daemon's trouble: said under the box.
            Err(e) => {
                l.rows =
                    if l.searching() { Rows::Hits(Vec::new()) } else { Rows::History(Vec::new()) };
                l.note = Some(e.message);
            }
        }
        let Some(l) = self.library_mut() else { return };
        // The row that was selected, where it is now; else the same place in the list.
        let found = before.and_then(|b| match &l.rows {
            Rows::Hits(h) => h.iter().position(|h| h.addr == b),
            Rows::History(i) => i.iter().position(|i| i.addr == b),
        });
        l.sel = found.unwrap_or(l.sel).min(l.rows.len().saturating_sub(1));
        self.dirty = true;
        self.library_preview();
    }

    pub(crate) fn library_status(&mut self, status: KbStatusInfo) {
        if let Some(l) = self.library_mut() {
            l.status = Some(status);
            self.dirty = true;
        }
    }

    /// The selected row's text, asked for when the selection moved to it.
    fn library_preview(&mut self) {
        let Some(l) = self.library_mut() else { return };
        let Some((addr, _)) = l.selected() else {
            l.preview = None;
            return;
        };
        if l.preview.as_ref().is_some_and(|p| p.addr == addr) {
            return;
        }
        l.preview = Some(Preview { addr: addr.clone(), item: None, chunks: Vec::new(), at: None });
        // A chat's message is shown by its snippet: ⏎ opens the chat there.
        if l.selected_chat().is_some() {
            return;
        }
        // A piece with its neighbours; an item from its start.
        let piece = addr.contains('/');
        let req =
            Request::KbRead { addr: addr.clone(), before: None, after: (!piece).then_some(5) };
        self.request(Pending::KbPreview(addr), req);
    }

    pub(crate) fn library_previewed(
        &mut self,
        addr: &str,
        item: KbListed,
        chunks: Vec<KbChunk>,
        at: Option<u32>,
    ) {
        if let Some(p) = self.library_mut().and_then(|l| l.preview.as_mut())
            && p.addr == addr
        {
            (p.item, p.chunks, p.at) = (Some(item), chunks, at);
            self.dirty = true;
        }
    }

    /// `⏎`: the selected row's source. A chat at its message; anything else in the reader.
    fn library_open(&mut self) {
        let Some(l) = self.library() else { return };
        if let Some(h) = l.selected_chat().cloned() {
            return self.open_chat_at(&h.item, h.node.as_deref());
        }
        if let Some((addr, _)) = l.selected() {
            self.read_item(&addr);
        }
    }

    /// Read an item whole, at the piece its address names.
    fn read_item(&mut self, addr: &str) {
        let (item, piece) = match addr.split_once('/') {
            Some((item, n)) => (item, n.parse::<u32>().ok()),
            None => (addr, None),
        };
        let req = Request::KbRead { addr: item.to_owned(), before: None, after: None };
        self.request(Pending::KbReader(piece), req);
    }

    /// An item arrived for the reader.
    pub(crate) fn library_read(
        &mut self,
        piece: Option<u32>,
        item: KbListed,
        chunks: Vec<KbChunk>,
    ) {
        // The words that found it, highlighted in it.
        let lit = self.library().and_then(|l| match &l.rows {
            Rows::Hits(h) => h.get(l.sel).and_then(|h| matched(&h.snippet).into_iter().next()),
            Rows::History(_) => None,
        });
        let mut view = ChatView::default();
        (view.read_only, view.highlight) = (true, lit);
        view.apply(0, true, chunks.iter().map(chat_item).collect(), String::new());
        // The piece asked for at the top; an item from its start.
        let first = chunks.first().map(|c| c.idx);
        if let Some(n) = piece.or(first) {
            view.focus_on(&n.to_string());
        }
        self.show_library(|l| {
            (l.typing, l.show_status) = (false, false);
            l.reader = Some(Reader { item, view, found: Vec::new(), found_at: 0 });
        });
    }

    /// `/` in the reader: highlight `text` and show the first piece that has it.
    pub(crate) fn library_find(&mut self, text: &str) {
        let Some(r) = self.library_mut().and_then(|l| l.reader.as_mut()) else { return };
        let text = text.trim();
        r.found = r.view.find(text);
        r.found_at = 0;
        r.view.highlight = Some(text.to_owned()).filter(|t| !t.is_empty());
        if text.is_empty() {
            return;
        }
        let said = match r.found.first() {
            Some(first) => {
                r.view.focus_on(first);
                format!("1/{} · n next · N the one before", r.found.len())
            }
            None => format!("{text:?}: not in it"),
        };
        self.toast(said);
    }

    /// `r`: the session it happened in. The item says which; a chat is opened as `⏎` does.
    fn library_resume(&mut self) {
        let Some(l) = self.library() else { return };
        if l.selected_chat().is_some() {
            return self.library_open();
        }
        let item = match (&l.reader, l.selected()) {
            (Some(r), _) => r.item.addr.clone(),
            (None, Some((_, item))) => item,
            (None, None) => return,
        };
        let req = Request::KbRead { addr: item, before: Some(0), after: Some(0) };
        self.request(Pending::KbResume, req);
    }

    /// The item whose session `r` asked for.
    pub(crate) fn library_resumed(&mut self, item: &KbListed) {
        if item.item.kind != ItemKind::Session {
            return self.toast(format!(
                "a {} has no session to go to: ⏎ reads what is kept of it",
                item.item.kind
            ));
        }
        let found = item.item.session.as_deref().and_then(|id| {
            self.sessions.iter().find(|s| s.id.full().eq_ignore_ascii_case(id)).cloned()
        });
        let Some(s) = found else {
            let conversation = item.item.uri.rsplit('/').next().unwrap_or_default();
            return self.toast_err(format!(
                "the session it happened in is no longer in ptyro: ⏎ reads what was kept of it (its agent may still carry it on by its own id, {conversation})"
            ));
        };
        // Dormant: the agent's own resume; exited: shown, where ⏎ restarts it.
        if s.state == ProcessState::Dormant {
            self.request(Pending::Done("resumed".into()), Request::Resume { addr: s.id.full() });
        }
        self.pick(s.id);
    }

    /// `d`: forget the selected row's item. The daemon says what would go and asks.
    fn library_forget(&mut self) {
        let Some(l) = self.library() else { return };
        if l.selected_chat().is_some() {
            let key = self.keymap.key_for("chat.list").unwrap_or_else(|| "chat.list".into());
            return self
                .toast(format!("that is a chat: the chat list ({key}) archives or deletes one"));
        }
        let item = match (&l.reader, l.selected()) {
            (Some(r), _) => r.item.item.id.clone(),
            (None, Some((_, item))) => item,
            (None, None) => return,
        };
        self.forget_asked(item);
    }

    fn forget_asked(&mut self, addr: String) {
        let req =
            Request::KbForget { addrs: vec![addr], query: None, confirmed: false, session: None };
        let then = Pending::Confirm(Box::new(req.clone()), Box::new(Pending::KbForgotten));
        self.request(then, req);
    }

    /// Forgotten: said as the command says it; the list no longer has it.
    pub(crate) fn library_forgot(&mut self, items: u32, chunks: u32) {
        self.toast(format!(
            "forgot {items} item{} ({}): deleted for good, and never kept again",
            if items == 1 { "" } else { "s" },
            pieces(u64::from(chunks))
        ));
        if let Some(l) = self.library_mut() {
            l.reader = None;
        }
        self.library_news();
    }

    // ── filters ─────────────────────────────────────────────────────────────────────────────

    /// A filter's pick: what it may be, with `any` first.
    fn library_pick(&mut self, f: Filter) {
        let Some(l) = self.library() else { return };
        let row = |value: &str, detail: &str| Item {
            label: value.to_owned(),
            detail: detail.to_owned(),
            key: None,
            target: Target::Path(value.into()),
        };
        let (title, mut items) = match f {
            Filter::Kind => (
                "Only this kind",
                l.kinds().iter().map(|k| row(k.as_str(), "")).collect::<Vec<_>>(),
            ),
            Filter::Project => (
                "Only this project's",
                self.projects
                    .iter()
                    .filter(|p| p.machine.is_none())
                    .map(|p| row(&p.slug, &p.root.display().to_string()))
                    .collect(),
            ),
            Filter::Since => {
                ("No older than", SINCE.iter().map(|(v, what)| row(v, what)).collect())
            }
        };
        items.insert(0, row("any", "no filter"));
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Library(f),
        });
    }

    /// A filter was picked (`any`: none).
    pub(crate) fn library_picked(&mut self, f: Filter, value: &str) {
        let project = self
            .projects
            .iter()
            .find(|p| p.machine.is_none() && p.slug == value)
            .map(|p| (p.id.clone(), p.slug.clone()));
        let Some(l) = self.library_mut() else { return };
        match f {
            Filter::Kind => l.kind = HitKind::parse(value),
            Filter::Project => l.project = project,
            Filter::Since => l.since = SINCE.iter().map(|(v, _)| *v).find(|v| *v == value),
        }
        l.sel = 0;
        self.library_ask();
    }

    // ── keys ────────────────────────────────────────────────────────────────────────────────

    pub(crate) fn library_key(&mut self, mut l: Box<Library>, key: KeyEvent) {
        let ctrl = key.mods.has(Mods::CTRL);
        let ch = match key.code {
            KeyCode::Char { c } if !ctrl => Some(c),
            _ => None,
        };
        // Switched off: ⏎ switches it on; nothing else to do here but leave.
        if l.off() {
            let on = key.code == KeyCode::Enter;
            let close = matches!(key.code, KeyCode::Esc) || ch == Some('q');
            if !close {
                self.chat_panel = Some(ChatPanel::Library(l));
            }
            if on {
                self.library_action("kb.enable");
            }
            return;
        }
        if let Some(r) = &mut l.reader {
            r.view.settle_focus();
            let page = r.view.body_rows.get().saturating_sub(2).max(1) as isize;
            let (mut act, mut close) = (None, false);
            match (key.code, ch) {
                (KeyCode::Esc, _) | (_, Some('q')) => close = true,
                (KeyCode::Up, _) | (_, Some('k')) => r.view.scroll_by(1),
                (KeyCode::Down, _) | (_, Some('j')) => r.view.scroll_by(-1),
                (KeyCode::PageUp, _) => r.view.scroll_by(page),
                (KeyCode::PageDown, _) | (_, Some(' ')) => r.view.scroll_by(-page),
                (_, Some('g')) => r.view.scroll = r.view.max_scroll.get(),
                (_, Some('G')) => r.view.scroll = 0,
                (_, Some('n' | 'N')) if !r.found.is_empty() => {
                    let n = r.found.len();
                    r.found_at =
                        if ch == Some('n') { r.found_at + 1 } else { r.found_at + n - 1 } % n;
                    r.view.focus_on(&r.found[r.found_at]);
                }
                (_, Some(c @ ('/' | 'r' | 'd' | 'y' | 'Y'))) => act = Some(c),
                _ => {}
            }
            if close {
                l.reader = None;
            }
            let copy = match (act, &l.reader) {
                (Some('y'), Some(r)) => Some((r.item.addr.clone(), "its address")),
                (Some('Y'), Some(r)) => Some((
                    r.view
                        .items
                        .iter()
                        .filter_map(|i| match &i.body {
                            ChatBody::User { text } => Some(format!("› {text}")),
                            ChatBody::Assistant { text, .. } | ChatBody::Notice { text, .. } => {
                                Some(text.clone())
                            }
                            ChatBody::Tool { name, title, .. } => Some(format!("{name} {title}")),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                    "its text",
                )),
                _ => None,
            };
            self.chat_panel = Some(ChatPanel::Library(l));
            match act {
                Some('/') => {
                    self.overlay = Some(Overlay::Prompt {
                        title: "Find in it".into(),
                        text: String::new(),
                        kind: PromptKind::KbFind,
                    })
                }
                Some('r') => self.library_resume(),
                Some('d') => self.library_forget(),
                _ => {}
            }
            if let Some((text, what)) = copy {
                crate::osc52(&text);
                self.toast(format!("copied {what}"));
            }
            return;
        }
        if l.typing {
            let mut changed = false;
            match key.code {
                KeyCode::Esc => {
                    l.typing = false;
                    changed = !l.query.is_empty();
                    l.query.clear();
                }
                // ⏎ and ↓ go to what was found.
                KeyCode::Enter | KeyCode::Down | KeyCode::Tab => l.typing = false,
                KeyCode::Backspace => changed = l.query.pop().is_some(),
                KeyCode::Char { c: 'u' } if ctrl => {
                    changed = !l.query.is_empty();
                    l.query.clear();
                }
                KeyCode::Char { c } if !ctrl => {
                    l.query.push(c);
                    changed = true;
                }
                _ => {}
            }
            if changed {
                l.sel = 0;
            }
            self.chat_panel = Some(ChatPanel::Library(l));
            if changed {
                self.library_ask();
            }
            return;
        }
        // On the filters: ←/→ and Tab move, ⏎ picks, Backspace clears, Esc goes back.
        if let Some(f) = l.chip {
            let at = Filter::ALL.iter().position(|x| *x == f).unwrap_or(0);
            let (mut pick, mut cleared) = (false, false);
            match (key.code, ch) {
                (KeyCode::Tab | KeyCode::Right, _) | (_, Some('l')) => {
                    l.chip = Filter::ALL.get(at + 1).copied();
                }
                (KeyCode::Left, _) | (_, Some('h')) => {
                    l.chip = Some(Filter::ALL[at.saturating_sub(1)]);
                }
                (KeyCode::Enter, _) | (_, Some(' ')) => pick = true,
                (KeyCode::Backspace, _) | (_, Some('x')) => {
                    match f {
                        Filter::Kind => l.kind = None,
                        Filter::Project => l.project = None,
                        Filter::Since => l.since = None,
                    }
                    cleared = true;
                }
                (KeyCode::Esc | KeyCode::Down, _) | (_, Some('j' | 'q')) => l.chip = None,
                _ => {}
            }
            self.chat_panel = Some(ChatPanel::Library(l));
            if pick {
                self.library_pick(f);
            } else if cleared {
                self.library_ask();
            }
            return;
        }
        let n = l.rows.len();
        let last = n.saturating_sub(1);
        let mut act = None;
        match (key.code, ch) {
            (KeyCode::Esc, _) | (_, Some('q')) => return,
            (_, Some('/' | 'i')) => l.typing = true,
            (KeyCode::Tab, _) => l.chip = Some(Filter::Kind),
            (KeyCode::Down, _) | (_, Some('j')) => l.sel = (l.sel + 1).min(last),
            (KeyCode::Up, _) | (_, Some('k')) => l.sel = l.sel.saturating_sub(1),
            (KeyCode::PageDown, _) => l.sel = (l.sel + PAGE).min(last),
            (KeyCode::PageUp, _) => l.sel = l.sel.saturating_sub(PAGE),
            (_, Some('g')) => l.sel = 0,
            (_, Some('G')) => l.sel = last,
            (_, Some('s')) => l.show_status = !l.show_status,
            // A row's history ends: everything again.
            (KeyCode::Backspace, _) if l.of.is_some() => {
                l.of = None;
                l.of_items.clear();
                act = Some('a');
            }
            (KeyCode::Enter, _) => act = Some('o'),
            (_, Some(c @ ('r' | 'd' | 'y' | 'Y'))) => act = Some(c),
            _ => {}
        }
        // What `y` and `Y` copy: the address; the piece's text (the snippet until it is here).
        let copy = match act {
            Some('y') => l.selected().map(|(addr, _)| (addr, "its address")),
            Some('Y') => l.selected().map(|(addr, _)| {
                let text = l
                    .preview
                    .as_ref()
                    .filter(|p| p.addr == addr && !p.chunks.is_empty())
                    .map(|p| {
                        let one = p.at.and_then(|at| p.chunks.iter().find(|c| c.idx == at));
                        match one {
                            Some(c) => c.text.clone(),
                            None => p
                                .chunks
                                .iter()
                                .map(|c| c.text.as_str())
                                .collect::<Vec<_>>()
                                .join("\n\n"),
                        }
                    })
                    .or_else(|| match &l.rows {
                        Rows::Hits(h) => {
                            h.get(l.sel).map(|h| h.snippet.replace([MARK_ON, MARK_OFF], ""))
                        }
                        Rows::History(i) => i.get(l.sel).map(title),
                    })
                    .unwrap_or_default();
                (text, "its text")
            }),
            _ => None,
        };
        let status = l.show_status && l.status.is_none();
        self.chat_panel = Some(ChatPanel::Library(l));
        match act {
            Some('o') => self.library_open(),
            Some('r') => self.library_resume(),
            Some('d') => self.library_forget(),
            Some('a') => self.library_ask(),
            _ => {}
        }
        if let Some((text, what)) = copy {
            crate::osc52(&text);
            self.toast(format!("copied {what}"));
        }
        if status {
            self.request(Pending::KbStatus, Request::KbStatus);
        }
        self.library_preview();
    }

    /// Pasted text goes into the search box.
    pub(crate) fn library_paste(&mut self, text: &str) {
        let Some(l) = self.library_mut() else { return };
        if l.reader.is_some() || l.off() {
            return;
        }
        l.query.push_str(&text.replace('\n', " "));
        (l.typing, l.chip, l.sel) = (true, None, 0);
        self.library_ask();
    }

    /// The mouse over the Library: the wheel moves (or scrolls the reader), a click selects.
    pub(crate) fn library_mouse(&mut self, y: u16, up: Option<bool>) {
        self.dirty = true;
        let Some(l) = self.library_mut() else { return };
        match (up, &mut l.reader) {
            (Some(up), Some(r)) => {
                r.view.settle_focus();
                r.view.scroll_by(if up { 3 } else { -3 });
            }
            (Some(up), None) => {
                let last = l.rows.len().saturating_sub(1);
                l.sel = if up { l.sel.saturating_sub(1) } else { (l.sel + 1).min(last) };
                self.library_preview();
            }
            (None, None) => {
                let row = l.drawn.borrow().iter().find(|(at, _)| *at == y).map(|(_, r)| *r);
                if let Some(row) = row {
                    (l.sel, l.typing, l.chip) = (row, false, None);
                    self.library_preview();
                }
            }
            (None, Some(_)) => {}
        }
    }
}

// ── drawing ─────────────────────────────────────────────────────────────────────────────────

pub(crate) fn draw(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    l.drawn.borrow_mut().clear();
    if area.height < 4 || area.width < 20 {
        return;
    }
    let t = &app.theme;
    let dim = fg(app, t.dim);
    if let Some(r) = &l.reader {
        return draw_reader(app, f, area, r);
    }
    // The title, with how much is kept.
    let mut head = vec![Span::styled(" Library ", fg(app, t.accent).add_modifier(Modifier::BOLD))];
    if let Some(s) = l.status.as_ref().filter(|s| s.enabled && s.error.is_none()) {
        let mut held: Vec<String> = s
            .kinds
            .iter()
            .map(|k| format!("{} {}{}", k.items, k.kind, if k.items == 1 { "" } else { "s" }))
            .collect();
        held.push(ptyro_core::model::human_bytes(s.bytes));
        if s.queued > 0 {
            held.push(format!("{} waiting to be read", s.queued));
        }
        head.push(Span::styled(held.join(" · "), dim));
    }
    f.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..area });
    if l.off() || l.status.as_ref().is_some_and(|s| s.error.is_some()) {
        return draw_off(app, f, area, l);
    }
    // The search box.
    let box_line = if l.typing || !l.query.is_empty() {
        Line::from(vec![
            Span::styled(" / ", fg(app, t.accent)),
            Span::styled(
                format!("{}{}", l.query, if l.typing { "▏" } else { "" }),
                fg(app, t.text),
            ),
        ])
    } else {
        Line::styled(
            " / search what was kept, and your chats: words, \"a phrase\", prefix*, agent:codex, by:meaning",
            dim,
        )
    };
    f.render_widget(Paragraph::new(box_line), Rect { y: area.y + 1, height: 1, ..area });
    // The filters.
    let chip = |f: Filter, label: &str, value: Option<String>| {
        let set = value.is_some();
        let text = format!(" {label}: {} ", value.unwrap_or_else(|| "any".into()));
        let mut st = if set { fg(app, t.accent) } else { dim };
        if l.chip == Some(f) {
            st = st.add_modifier(Modifier::REVERSED);
        }
        Span::styled(text, st)
    };
    let mut chips = vec![
        chip(Filter::Kind, "kind", l.kind.map(|k| k.as_str().to_owned())),
        chip(Filter::Project, "project", l.project.as_ref().map(|(_, name)| name.clone())),
        chip(Filter::Since, "since", l.since.map(str::to_owned)),
    ];
    if let Some(of) = &l.of {
        chips.push(Span::styled(format!(" only {} ", of.label()), fg(app, t.accent)));
    }
    f.render_widget(Paragraph::new(Line::from(chips)), Rect { y: area.y + 2, height: 1, ..area });

    let hint = if l.typing {
        "⏎ or ↓ the list · Esc clears · C-u empties the box".to_owned()
    } else if l.chip.is_some() {
        "←/→ or Tab the filters · ⏎ pick · Backspace clear · Esc the list".to_owned()
    } else {
        let mut keys = vec!["/ search", "Tab filters", "j/k move", "⏎ open", "r its session"];
        keys.extend(["y address", "Y text", "d forget"]);
        keys.push(if l.show_status { "s the list" } else { "s status" });
        if l.of.is_some() {
            keys.push("Backspace everything");
        }
        keys.push("Esc");
        keys.join(" · ")
    };
    let hint_h = draw_hint(app, f, area, &hint);
    let body = Rect { y: area.y + 3, height: area.height.saturating_sub(3 + hint_h), ..area };
    let two = area.width >= TWO_COLUMNS;
    let left = if two { Rect { width: (body.width * 45 / 100).max(36), ..body } } else { body };
    if l.show_status && !two {
        draw_status(app, f, body, l);
    } else {
        draw_rows(app, f, left, l, !two);
    }
    if two {
        let right =
            Rect { x: left.right() + 2, width: body.width.saturating_sub(left.width + 2), ..body };
        for y in body.y..body.bottom() {
            f.render_widget(
                Paragraph::new(Span::styled("│", fg(app, t.border))),
                Rect { x: left.right(), y, width: 1, height: 1 },
            );
        }
        if l.show_status {
            draw_status(app, f, right, l);
        } else {
            draw_preview(app, f, right, l);
        }
    }
}

/// The keys, at the foot of the panel: on as many lines as they need where there is room
/// (a narrow terminal), else on one. Returns how many lines they took.
fn draw_hint(app: &App, f: &mut Frame<'_>, area: Rect, hint: &str) -> u16 {
    let dim = fg(app, app.theme.dim);
    let most = if area.height >= 12 { 3 } else { 1 };
    let lines: Vec<Line<'static>> = wrapped(hint, area.width.saturating_sub(2) as usize)
        .into_iter()
        .take(most)
        .map(|l| Line::styled(format!(" {l}"), dim))
        .collect();
    let h = lines.len() as u16;
    f.render_widget(Paragraph::new(lines), Rect { y: area.bottom() - h, height: h, ..area });
    h
}

/// Switched off, or `kb.db` can't be opened: what that means and the way out.
fn draw_off(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let Some(s) = &l.status else { return };
    let mut lines = vec![Line::raw("")];
    if let Some(e) = s.error.as_ref().filter(|_| s.enabled) {
        lines.push(Line::styled(" The knowledge base can't be read.", fg(app, t.error)));
        lines.push(Line::styled(format!(" {e}"), fg(app, t.text)));
        lines.push(Line::raw(""));
        lines.push(Line::styled(" Esc closes", dim));
    } else {
        lines.push(Line::styled(
            " The knowledge base is off.",
            fg(app, t.text).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::styled(" Nothing is kept and nothing is searched.", fg(app, t.text)));
        if s.bytes > 0 {
            lines.push(Line::styled(
                format!(
                    " What was kept before stays in {} ({}).",
                    s.path,
                    ptyro_core::model::human_bytes(s.bytes)
                ),
                dim,
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" ⏎ switch it on", fg(app, t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(
                ": agent sessions are kept from then on, searchable here and by your agents",
                fg(app, t.text),
            ),
        ]));
        lines.push(Line::styled(
            " (Knowledge → enabled in the settings; a private session is never kept) · Esc closes",
            dim,
        ));
    }
    let body = Rect { y: area.y + 1, height: area.height - 1, ..area };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

/// `text` in lines of at most `width` columns, broken at spaces where it can be.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && line.width() + 1 + word.width() > width {
            out.push(std::mem::take(&mut line));
        } else if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out
}

/// Styled text in at most `most` lines of `width` columns: cut where a line is full, the rest
/// dropped.
fn span_lines(spans: Vec<Span<'static>>, width: usize, most: usize) -> Vec<Vec<Span<'static>>> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = Vec::new();
    let mut col = 0;
    'spans: for span in spans {
        let mut part = String::new();
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if col + w > width {
                line.push(Span::styled(std::mem::take(&mut part), span.style));
                if out.len() + 1 == most {
                    break 'spans;
                }
                out.push(std::mem::take(&mut line));
                col = 0;
            }
            part.push(c);
            col += w;
        }
        line.push(Span::styled(part, span.style));
    }
    out.push(line);
    out
}

/// The list: the history by day, or the hits. A row is two lines: what it is and when, then
/// where it is from and (a hit) the text around the match. `alone` (no room for the selected
/// row's text beside the list): the text around the match gets lines of its own.
fn draw_rows(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library, alone: bool) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let width = area.width as usize;
    let mut out: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut sel_line = 0;
    // Something said instead of rows, or under them: wrapped, never cut.
    let say = |out: &mut Vec<(Line<'static>, Option<usize>)>, text: &str| {
        for line in wrapped(text, width.saturating_sub(2)) {
            out.push((Line::styled(format!(" {line}"), dim), None));
        }
    };
    // A row's first line: its kind, its title (cut to leave room), and when.
    let first = |kind: &str, what: String, when: String, on: bool| {
        let mut what = what;
        crate::ui::truncate_width(&mut what, width.saturating_sub(13 + when.chars().count()));
        let mut name = fg(app, t.text);
        if on {
            name = name.add_modifier(Modifier::REVERSED);
        }
        Line::from(vec![
            Span::styled(format!(" {kind:<8}"), fg(app, t.accent)),
            Span::styled(format!(" {what} "), name),
            Span::styled(format!(" {when}"), dim),
        ])
    };
    const INDENT: &str = "          ";
    if let Some(note) = &l.note {
        say(&mut out, note);
    }
    match &l.rows {
        _ if l.loading => say(&mut out, "Looking…"),
        Rows::History(items) => {
            if items.is_empty() && l.note.is_none() {
                let filtered = l.kind.is_some() || l.project.is_some() || l.since.is_some();
                let none = match (&l.of, filtered) {
                    (Some(of), _) => format!("nothing is kept of {}", of.label()),
                    (None, true) => "nothing kept matches these filters".to_owned(),
                    (None, false) => {
                        "nothing is kept yet: agent sessions are, as they happen".to_owned()
                    }
                };
                say(&mut out, &none);
                if l.status.as_ref().is_some_and(|s| !s.sessions) {
                    say(&mut out, "agent sessions are not kept (`kb.sessions` is off)");
                }
            }
            let now = crate::clock::local_ms(now_ms());
            let mut last = String::new();
            for (i, it) in items.iter().enumerate() {
                let d = day(crate::clock::local_ms(at(it)), now);
                if d != last {
                    out.push((
                        Line::styled(format!(" {d}"), dim.add_modifier(Modifier::BOLD)),
                        None,
                    ));
                    last = d;
                }
                if i == l.sel {
                    sel_line = out.len();
                }
                let time = when(at(it)).get(11..).unwrap_or_default().to_owned();
                out.push((first(it.item.kind.as_str(), title(it), time, i == l.sel), Some(i)));
                let mut about: Vec<String> = [it.project_name.clone(), it.item.agent.clone()]
                    .into_iter()
                    .flatten()
                    .collect();
                about.push(pieces(u64::from(it.item.chunks)));
                out.push((Line::styled(format!("{INDENT}{}", about.join(" · ")), dim), Some(i)));
            }
        }
        Rows::Hits(hits) => {
            if hits.is_empty() && l.note.is_none() {
                say(&mut out, "nothing matches");
            }
            for (i, h) in hits.iter().enumerate() {
                if i == l.sel {
                    sel_line = out.len();
                }
                let what = h.title.clone().unwrap_or_else(|| h.addr.clone()).replace('\n', " ");
                let when = crate::ui::ago(h.at_ms);
                out.push((first(h.kind.as_str(), what, when, i == l.sel), Some(i)));
                let about: Vec<String> =
                    [h.machine.clone(), h.project_name.clone(), h.agent.clone()]
                        .into_iter()
                        .flatten()
                        .collect();
                let mut second = Vec::new();
                if !about.is_empty() {
                    second.push(Span::styled(about.join(" · "), dim));
                }
                if alone {
                    second.insert(0, Span::raw(INDENT));
                    out.push((Line::from(std::mem::take(&mut second)), Some(i)));
                } else if !about.is_empty() {
                    second.push(Span::styled(" · ", dim));
                }
                // ≈: found by meaning, none of the words is in it.
                if h.how == "meaning" {
                    second.push(Span::styled("≈ ", fg(app, t.search)));
                }
                second.extend(snippet_spans(app, &h.snippet));
                let room = width.saturating_sub(INDENT.len() + 1);
                for mut line in span_lines(second, room, if alone { 3 } else { 1 }) {
                    line.insert(0, Span::raw(INDENT));
                    out.push((Line::from(line), Some(i)));
                }
            }
            // Why it was by words alone.
            if let Some(why) = &l.words_only {
                out.push((Line::raw(""), None));
                say(&mut out, &format!("({why})"));
            }
        }
    }
    let room = area.height as usize;
    let skip = sel_line.saturating_sub(room.saturating_sub(4));
    let mut drawn = l.drawn.borrow_mut();
    let mut lines = Vec::new();
    for (n, (line, row)) in out.into_iter().skip(skip).take(room).enumerate() {
        if let Some(r) = row {
            drawn.push((area.y + n as u16, r));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The selected row's text: the piece that matched among its neighbours, the match
/// highlighted; for a row of the history, how the item starts.
fn draw_preview(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let hit = match &l.rows {
        Rows::Hits(h) => h.get(l.sel),
        Rows::History(_) => None,
    };
    let words = hit.map(|h| matched(&h.snippet)).unwrap_or_default();
    if let Some(h) = hit.filter(|h| h.kind == HitKind::Chat) {
        let what = h.title.clone().unwrap_or_else(|| h.addr.clone());
        lines.push(Line::styled(what, fg(app, t.text).add_modifier(Modifier::BOLD)));
        lines.push(Line::styled(format!("a chat's {} · {}", h.part, crate::ui::ago(h.at_ms)), dim));
        lines.push(Line::raw(""));
        lines.push(Line::from(snippet_spans(app, &h.snippet)));
        lines.push(Line::raw(""));
        lines.push(Line::styled("⏎ opens the chat at this message", dim));
    } else if let Some((p, item)) =
        l.preview.as_ref().and_then(|p| p.item.as_ref().map(|item| (p, item)))
    {
        lines.push(Line::styled(title(item), fg(app, t.text).add_modifier(Modifier::BOLD)));
        let mut about: Vec<String> = vec![item.item.kind.as_str().to_owned()];
        about.extend([item.project_name.clone(), item.item.agent.clone()].into_iter().flatten());
        about.push(when(at(item)));
        about.push(pieces(u64::from(item.item.chunks)));
        lines.push(Line::styled(about.join(" · "), dim));
        lines.push(Line::styled(format!("{} · {}", item.addr, item.item.uri), dim));
        for c in &p.chunks {
            let here = p.at == Some(c.idx);
            lines.push(Line::raw(""));
            let place = c.heading.as_deref().map(|h| format!(" · {h}")).unwrap_or_default();
            lines.push(Line::styled(
                format!("{} {}{place}", if here { "▶" } else { " " }, c.kind),
                if here { fg(app, t.accent).add_modifier(Modifier::BOLD) } else { dim },
            ));
            // The piece that matched in full; a neighbour by how it starts.
            let most = if here || p.at.is_none() { area.height as usize } else { 4 };
            let base = if here || p.at.is_none() { fg(app, t.text) } else { dim };
            let all: Vec<&str> = c.text.lines().collect();
            for line in all.iter().take(most) {
                lines.push(lit_line(app, &format!("  {line}"), &words, base));
            }
            if all.len() > most {
                lines.push(Line::styled(format!("  … {} lines", all.len()), dim));
            }
        }
    } else if l.rows.is_empty() || l.loading {
        // Nothing selected: nothing to show beside the list.
    } else {
        lines.push(Line::styled("Looking…", dim));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// How the knowledge base stands, as `ptyro kb` says it.
fn draw_status(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    let t = &app.theme;
    let lines: Vec<Line<'static>> = match &l.status {
        Some(s) => s
            .lines()
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let st = if i == 0 {
                    fg(app, t.text).add_modifier(Modifier::BOLD)
                } else {
                    fg(app, t.text)
                };
                Line::styled(line, st)
            })
            .collect(),
        None => vec![Line::styled("Looking…", fg(app, t.dim))],
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// The reader: an item's pieces as a transcript, drawn by the chat view's renderer.
fn draw_reader(app: &App, f: &mut Frame<'_>, area: Rect, r: &Reader) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let mut about: Vec<String> = vec![r.item.item.kind.as_str().to_owned()];
    about.extend([r.item.project_name.clone(), r.item.item.agent.clone()].into_iter().flatten());
    about.push(when(at(&r.item)));
    about.push(r.item.addr.clone());
    let head = Line::from(vec![
        Span::styled(
            format!(" {} ", title(&r.item)),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(about.join(" · "), dim),
    ]);
    f.render_widget(Paragraph::new(head), Rect { height: 1, ..area });
    let mut keys = vec!["read only", "j/k scroll", "/ find"];
    if !r.found.is_empty() {
        keys.push("n/N next");
    }
    if r.item.item.kind == ItemKind::Session {
        keys.push("r its session");
    }
    keys.extend(["y address", "Y text", "d forget", "Esc the list"]);
    let hint_h = draw_hint(app, f, area, &keys.join(" · "));
    let body = Rect { y: area.y + 1, height: area.height.saturating_sub(1 + hint_h), ..area };
    crate::chat::draw(&r.view, f.buffer_mut(), body, t, app.depth, false);
}

#[cfg(test)]
mod tests;
