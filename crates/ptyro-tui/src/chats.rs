//! Chats in the TUI (SPEC §22.12, ROADMAP CHAT1.7): the CHATS section under each workspace in
//! the sidebar, the chat list (`g`), and the chat view (`m` for a new one), which takes the place
//! of the panes beside the sidebar. The view is the structured sessions' chat view (`chat.rs`)
//! with a header and a composer of its own: ⏎ sends, `a`/`A`/`d` answer a permission card,
//! `Ctrl-C` stops an answer, Esc goes back.

use ptyro_core::chat::{ChatBody, ChatItem};
use ptyro_core::chats::ChatInfo;
use ptyro_core::input::{KeyCode, KeyEvent, KeyKind, Mods};
use ptyro_core::status::AgentStatus;
use ptyro_protocol::{ClientMsg, Request};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Confirmed, Focus, Overlay, Pending, PromptKind, Purpose};
use crate::palette::{Item, Palette, Target};
use crate::tree::Row;

/// The chat panel: the list, or one chat.
pub enum ChatPanel {
    List(ChatList),
    View(Box<ChatViewState>),
}

pub struct ChatList {
    /// One workspace's chats; `None`: every workspace's.
    pub workspace: Option<String>,
    pub sel: usize,
    pub query: String,
    /// Typing into the search box.
    pub typing: bool,
    /// Archived chats instead (fetched when asked for).
    pub archived: Option<Vec<ChatInfo>>,
}

pub struct ChatViewState {
    /// `None` until a new chat is made by its first message.
    pub id: Option<String>,
    pub workspace: String,
    pub view: crate::chat::ChatView,
    pub draft: String,
    /// A new chat's agent and preset, picked with `C-o`.
    pub agent: Option<String>,
    pub preset: Option<String>,
    /// Where Esc goes back to.
    pub from_list: Option<ChatList>,
}

/// The day group of a chat in the list.
fn group(last_ms: u64, now: u64) -> &'static str {
    let day = 86_400_000;
    let today = now - now % day;
    match last_ms {
        t if t >= today => "Today",
        t if t >= today.saturating_sub(day) => "Yesterday",
        t if t >= today.saturating_sub(6 * day) => "This week",
        _ => "Earlier",
    }
}

impl App {
    pub(crate) fn chats_on(&self) -> bool {
        self.config.chat.enabled
    }

    fn chats_off_toast(&mut self) {
        self.toast("chats are off: `[chat] enabled` in the settings (C-q C)");
    }

    pub(crate) fn chat(&self, id: &str) -> Option<&ChatInfo> {
        self.chat_list.iter().find(|c| c.id == id)
    }

    /// The workspace a new chat goes to: the selected row's, else the one shown.
    fn chat_workspace(&self) -> String {
        if self.focus == Focus::Sidebar {
            let rows = self.rows();
            if let Some(w) = crate::tree::Inputs::workspace_of(&rows, self.sel_index(&rows)) {
                return w;
            }
        }
        self.workspace.clone()
    }

    pub(crate) fn refresh_chats(&self) {
        if self.chats_on() {
            let req = Request::ChatList {
                workspace: None,
                all: true,
                pinned: false,
                archived: false,
                limit: None,
            };
            self.request(Pending::Chats, req);
        }
    }

    pub(crate) fn got_chats(&mut self, chats: Vec<ChatInfo>) {
        self.chat_list = chats;
        self.dirty = true;
    }

    pub(crate) fn chat_updated(&mut self, info: ChatInfo) {
        let shown = !info.archived;
        match self.chat_list.iter().position(|c| c.id == info.id) {
            Some(i) if shown => self.chat_list[i] = info,
            Some(i) => {
                self.chat_list.remove(i);
            }
            None if shown => self.chat_list.push(info),
            None => {}
        }
        self.chat_list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_ms.cmp(&a.last_ms)));
        self.dirty = true;
    }

    pub(crate) fn chat_removed(&mut self, id: &str) {
        self.chat_list.retain(|c| c.id != id);
        if let Some(ChatPanel::View(v)) = &self.chat_panel
            && v.id.as_deref() == Some(id)
        {
            self.close_chat();
            self.toast("that chat was deleted");
        }
        self.dirty = true;
    }

    /// A followed chat's items.
    pub(crate) fn chat_live(&mut self, chat: String, rev: u64, reset: bool, items: Vec<ChatItem>) {
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.as_deref() == Some(chat.as_str())
        {
            let draft = v.draft.clone();
            v.view.apply(rev, reset, items, draft);
            self.dirty = true;
        }
    }

    // ── opening and closing ──────────────────────────────────────────────────────────────

    /// `chat.new`: an empty chat view in the workspace; its first message makes the chat.
    pub(crate) fn new_chat(&mut self) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        let workspace = self.chat_workspace();
        self.leave_chat();
        self.chat_panel = Some(ChatPanel::View(Box::new(ChatViewState {
            id: None,
            workspace,
            view: crate::chat::ChatView::default(),
            draft: String::new(),
            agent: None,
            preset: None,
            from_list: None,
        })));
        self.dirty = true;
    }

    /// Open a chat in the view, following it.
    pub(crate) fn open_chat(&mut self, id: &str) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        let from_list = match self.chat_panel.take() {
            Some(ChatPanel::List(l)) => Some(l),
            other => {
                self.chat_panel = other;
                self.leave_chat();
                None
            }
        };
        let workspace =
            self.chat(id).map_or_else(|| self.workspace.clone(), |c| c.workspace.clone());
        self.send(ClientMsg::ChatSubscribe { chat: id.to_owned() });
        self.chat_panel = Some(ChatPanel::View(Box::new(ChatViewState {
            id: Some(id.to_owned()),
            workspace,
            view: crate::chat::ChatView::default(),
            draft: String::new(),
            agent: None,
            preset: None,
            from_list,
        })));
        self.dirty = true;
    }

    /// `chat.list`: the workspace's chats (Tab: every workspace's).
    pub(crate) fn open_chat_list(&mut self, workspace: Option<String>) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        self.leave_chat();
        self.chat_panel = Some(ChatPanel::List(ChatList {
            workspace,
            sel: 0,
            query: String::new(),
            typing: false,
            archived: None,
        }));
        self.dirty = true;
    }

    /// Stop following the chat shown, if one is.
    fn leave_chat(&mut self) {
        if let Some(ChatPanel::View(v)) = &self.chat_panel
            && let Some(id) = &v.id
        {
            self.send(ClientMsg::ChatUnsubscribe { chat: id.clone() });
        }
    }

    pub(crate) fn close_chat(&mut self) {
        self.leave_chat();
        self.chat_panel = None;
        self.dirty = true;
    }

    /// The chat being acted on: the one shown, else the selected sidebar row's, else the list's.
    pub(crate) fn chat_target(&self) -> Option<String> {
        match &self.chat_panel {
            Some(ChatPanel::View(v)) => return v.id.clone(),
            Some(ChatPanel::List(l)) => return self.list_rows(l).get(l.sel).map(|c| c.id.clone()),
            None => {}
        }
        match self.selected_row() {
            Some(Row::Chat(id)) if self.focus == Focus::Sidebar => Some(id),
            _ => None,
        }
    }

    // ── actions ──────────────────────────────────────────────────────────────────────────

    /// A `chat.*` action from a key, the palette or a row menu.
    pub(crate) fn chat_action(&mut self, action: &str) {
        match action {
            "chat.new" => return self.new_chat(),
            "chat.list" => {
                if matches!(self.chat_panel, Some(ChatPanel::List(_))) {
                    return self.close_chat();
                }
                return self.open_chat_list(Some(self.chat_workspace()));
            }
            _ => {}
        }
        let Some(id) = self.chat_target() else {
            self.toast("no chat selected: open one (C-q g) or select its row");
            return;
        };
        let info = self.chat(&id).cloned();
        let label = info.as_ref().map_or_else(|| "this chat".to_owned(), |c| c.label().to_owned());
        let update = |pinned: Option<bool>, archived: Option<bool>| Request::ChatUpdate {
            chat: id.clone(),
            title: None,
            pinned,
            archived,
            agent: None,
            preset: None,
        };
        match action {
            "chat.open" => self.open_chat(&id),
            "chat.retitle" => {
                self.overlay = Some(Overlay::Prompt {
                    title: "Chat title (empty: a new automatic one)".into(),
                    text: info.and_then(|c| c.title).unwrap_or_default(),
                    kind: PromptKind::ChatRetitle(id),
                });
            }
            "chat.pin" => {
                let on = !info.as_ref().is_some_and(|c| c.pinned);
                let msg = if on { "pinned" } else { "unpinned" };
                self.request(Pending::Done(msg.into()), update(Some(on), None));
            }
            "chat.archive" => {
                let archived = self.archived_shown();
                let msg = if archived { "back from the archive" } else { "archived" };
                self.request(Pending::Done(msg.into()), update(None, Some(!archived)));
                if let Some(ChatPanel::List(l)) = &mut self.chat_panel
                    && let Some(a) = &mut l.archived
                {
                    a.retain(|c| c.id != id);
                }
                if matches!(self.chat_panel, Some(ChatPanel::View(_))) {
                    self.close_chat();
                }
            }
            "chat.delete" => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("Delete the chat {label:?}? (y/n)"),
                    action: Confirmed::Send(
                        Box::new(Request::ChatDelete { chat: id }),
                        "chat deleted".into(),
                    ),
                });
            }
            "chat.agent" => self.pick_chat_agent(Some(id)),
            "chat.preset" => self.pick_chat_preset(Some(id)),
            "chat.export" => {
                let path = std::env::temp_dir().join(format!(
                    "ptyro-chat-{}.md",
                    info.as_ref().map_or(id.as_str(), |c| c.handle.as_str())
                ));
                self.request(
                    Pending::ChatExport(path),
                    Request::ChatExport { chat: id, format: "md".into() },
                );
            }
            "chat.stop" => {
                self.request(Pending::Done("stopped".into()), Request::ChatStop { chat: id })
            }
            _ => {}
        }
    }

    fn archived_shown(&self) -> bool {
        matches!(&self.chat_panel, Some(ChatPanel::List(l)) if l.archived.is_some())
    }

    /// Pick an agent: for the chat `id` (switching it), or for the new chat being written.
    pub(crate) fn pick_chat_agent(&mut self, id: Option<String>) {
        let mut names: Vec<String> =
            ["claude", "codex", "gemini", "opencode"].map(String::from).into();
        let mine: Vec<String> =
            self.config.agents.keys().filter(|k| !names.contains(k)).cloned().collect();
        names.extend(mine);
        let items = names
            .into_iter()
            .map(|n| Item {
                detail: "agent".into(),
                label: n.clone(),
                key: None,
                target: Target::Agent(n),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Which agent? (the conversation carries over)", items),
            purpose: Purpose::ChatAgent(id),
        });
    }

    pub(crate) fn pick_chat_preset(&mut self, id: Option<String>) {
        let items = self
            .config
            .chat
            .presets()
            .into_iter()
            .map(|p| {
                let t = self.config.chat.preset(&p).unwrap_or_default();
                let mut what = Vec::new();
                if t.web {
                    what.push("web search");
                }
                if t.read {
                    what.push("reads its folders");
                }
                if what.is_empty() {
                    what.push("no tools");
                }
                Item {
                    label: p.clone(),
                    detail: what.join(", "),
                    key: None,
                    target: Target::Agent(p),
                }
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("What may the chat use?", items),
            purpose: Purpose::ChatPreset(id),
        });
    }

    /// An agent or preset was picked.
    pub(crate) fn chat_picked(&mut self, value: String, agent: bool, id: Option<String>) {
        match id {
            Some(chat) => {
                let (a, p) = if agent { (Some(value), None) } else { (None, Some(value)) };
                self.request(
                    Pending::Done("the next message carries the chat over".into()),
                    Request::ChatUpdate {
                        chat,
                        title: None,
                        pinned: None,
                        archived: None,
                        agent: a,
                        preset: p,
                    },
                );
            }
            None => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    if agent {
                        v.agent = Some(value);
                    } else {
                        v.preset = Some(value);
                    }
                }
                // After the agent, what it may use.
                if agent {
                    self.pick_chat_preset(None);
                }
            }
        }
        self.dirty = true;
    }

    /// A new chat was made by its first message: follow it.
    pub(crate) fn chat_made(&mut self, info: ChatInfo) {
        let id = info.id.clone();
        self.chat_updated(info);
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.is_none()
        {
            v.id = Some(id.clone());
            v.view = crate::chat::ChatView::default();
            self.send(ClientMsg::ChatSubscribe { chat: id });
        }
    }

    // ── keys ─────────────────────────────────────────────────────────────────────────────

    pub(crate) fn chat_key(&mut self, key: KeyEvent) {
        if key.kind == KeyKind::Release {
            return;
        }
        self.dirty = true;
        match self.chat_panel.take() {
            Some(ChatPanel::List(l)) => self.chat_list_key(l, key),
            Some(ChatPanel::View(v)) => self.chat_view_key(*v, key),
            None => {}
        }
    }

    pub(crate) fn chat_paste(&mut self, text: &str) {
        match &mut self.chat_panel {
            Some(ChatPanel::View(v)) => {
                v.draft.push_str(text);
                v.view.draft = v.draft.clone();
                v.view.apply(v.view.rev, false, Vec::new(), v.draft.clone());
            }
            Some(ChatPanel::List(l)) => {
                l.query.push_str(&text.replace('\n', " "));
                l.typing = true;
                l.sel = 0;
            }
            None => {}
        }
        self.dirty = true;
    }

    fn chat_view_key(&mut self, mut v: ChatViewState, key: KeyEvent) {
        let ctrl = key.mods.has(Mods::CTRL);
        let pending = v.view.pending_permission();
        if pending && v.draft.is_empty() && !ctrl {
            let d = match key.code {
                KeyCode::Char { c: 'a' | 'y' | '1' } => Some("allow"),
                KeyCode::Char { c: 'A' | '2' } => Some("always"),
                KeyCode::Char { c: 'd' | 'n' | '3' } => Some("deny"),
                _ => None,
            };
            if let (Some(d), Some(id)) = (d, v.id.clone()) {
                self.request(
                    Pending::Done(String::new()),
                    Request::ChatAnswer { chat: id, decision: d.into() },
                );
                self.chat_panel = Some(ChatPanel::View(Box::new(v)));
                return;
            }
        }
        match key.code {
            KeyCode::Esc => {
                if !v.draft.is_empty() {
                    v.draft.clear();
                } else {
                    if let Some(id) = &v.id {
                        self.send(ClientMsg::ChatUnsubscribe { chat: id.clone() });
                    }
                    self.chat_panel = v.from_list.map(ChatPanel::List);
                    return;
                }
            }
            KeyCode::Char { c: 'c' } if ctrl => {
                if v.draft.is_empty() {
                    if let Some(id) = v.id.clone() {
                        self.request(Pending::Done(String::new()), Request::ChatStop { chat: id });
                    }
                } else {
                    v.draft.clear();
                }
            }
            KeyCode::Char { c: 'o' } if ctrl => {
                let id = v.id.clone();
                self.chat_panel = Some(ChatPanel::View(Box::new(v)));
                return self.pick_chat_agent(id);
            }
            _ if crate::app::is_newline_key(&key) => v.draft.push('\n'),
            KeyCode::Enter => {
                let text = std::mem::take(&mut v.draft);
                if !text.trim().is_empty() {
                    match v.id.clone() {
                        Some(id) => self.request(
                            Pending::Done(String::new()),
                            Request::ChatSend { chat: id, text },
                        ),
                        None => {
                            // The message shows at once; the chat is made by it.
                            v.view.apply(
                                0,
                                true,
                                vec![ChatItem::new("first", ChatBody::User { text: text.clone() })],
                                String::new(),
                            );
                            self.request(
                                Pending::ChatMade,
                                Request::ChatNew {
                                    workspace: Some(v.workspace.clone()),
                                    agent: v.agent.clone(),
                                    preset: v.preset.clone(),
                                    title: None,
                                    prompt: Some(text),
                                },
                            );
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                v.draft.pop();
            }
            KeyCode::PageUp => v.view.scroll = (v.view.scroll + 10).min(v.view.max_scroll.get()),
            KeyCode::PageDown => v.view.scroll = v.view.scroll.saturating_sub(10),
            KeyCode::Char { c } if !ctrl && !key.mods.has(Mods::ALT) => {
                v.draft.push(c);
                v.view.scroll = 0;
            }
            _ => {}
        }
        let draft = v.draft.clone();
        v.view.apply(v.view.rev, false, Vec::new(), draft);
        self.chat_panel = Some(ChatPanel::View(Box::new(v)));
    }

    /// The list's rows: its workspace's (or every) chat, or the archived ones, filtered.
    pub(crate) fn list_rows(&self, l: &ChatList) -> Vec<ChatInfo> {
        let source = l.archived.as_deref().unwrap_or(&self.chat_list);
        let q = l.query.to_lowercase();
        source
            .iter()
            .filter(|c| l.workspace.as_ref().is_none_or(|w| c.workspace == *w))
            .filter(|c| {
                q.is_empty()
                    || c.label().to_lowercase().contains(&q)
                    || c.agent.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    fn chat_list_key(&mut self, mut l: ChatList, key: KeyEvent) {
        let n = self.list_rows(&l).len();
        let ctrl = key.mods.has(Mods::CTRL);
        if l.typing {
            match key.code {
                KeyCode::Esc => {
                    l.typing = false;
                    l.query.clear();
                }
                KeyCode::Enter | KeyCode::Down => l.typing = false,
                KeyCode::Backspace => {
                    l.query.pop();
                }
                KeyCode::Char { c } if !ctrl => {
                    l.query.push(c);
                    l.sel = 0;
                }
                _ => {}
            }
            self.chat_panel = Some(ChatPanel::List(l));
            return;
        }
        let action = match key.code {
            KeyCode::Esc | KeyCode::Char { c: 'q' } => {
                self.chat_panel = None;
                return;
            }
            KeyCode::Char { c: 'j' } | KeyCode::Down => {
                l.sel = (l.sel + 1).min(n.saturating_sub(1));
                None
            }
            KeyCode::Char { c: 'k' } | KeyCode::Up => {
                l.sel = l.sel.saturating_sub(1);
                None
            }
            KeyCode::Char { c: '/' } => {
                l.typing = true;
                None
            }
            KeyCode::Tab => {
                l.workspace = match l.workspace {
                    Some(_) => None,
                    None => Some(self.workspace.clone()),
                };
                l.sel = 0;
                None
            }
            KeyCode::Char { c: 'A' } => {
                if l.archived.take().is_none() {
                    l.archived = Some(Vec::new());
                    self.request(
                        Pending::ArchivedChats,
                        Request::ChatList {
                            workspace: None,
                            all: true,
                            pinned: false,
                            archived: true,
                            limit: None,
                        },
                    );
                }
                l.sel = 0;
                None
            }
            KeyCode::Enter => Some("chat.open"),
            KeyCode::Char { c: 'm' | 'n' } => Some("chat.new"),
            KeyCode::Char { c: '*' } => Some("chat.pin"),
            KeyCode::Char { c: 'x' } => Some("chat.archive"),
            KeyCode::Char { c: 'X' } => Some("chat.delete"),
            KeyCode::Char { c: ',' } => Some("chat.retitle"),
            KeyCode::Char { c: 'e' } => Some("chat.export"),
            KeyCode::Char { c: '.' } => {
                self.chat_panel = Some(ChatPanel::List(l));
                return self.chat_menu();
            }
            _ => None,
        };
        self.chat_panel = Some(ChatPanel::List(l));
        if let Some(a) = action {
            self.chat_action(a);
        }
    }

    pub(crate) fn got_archived(&mut self, chats: Vec<ChatInfo>) {
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
            l.archived = Some(chats);
        }
        self.dirty = true;
    }

    /// The chat's actions, as a menu.
    pub(crate) fn chat_menu(&mut self) {
        let Some(id) = self.chat_target() else { return };
        let title = self.chat(&id).map_or_else(|| "Chat".into(), |c| format!("Chat {}", c.label()));
        let items = CHAT_ACTIONS
            .iter()
            .filter_map(|a| crate::keymap::spec(a))
            .map(|a| Item {
                label: a.id.into(),
                detail: a.desc.into(),
                key: self.keymap.key_for(a.id),
                target: Target::Action(crate::keymap::Command { action: a.id, arg: None }),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Palette,
        });
    }

    /// `nav.open` on a chat row of the sidebar.
    pub(crate) fn chat_row_open(&mut self, row: &Row) -> bool {
        match row {
            Row::Chat(id) => self.open_chat(id),
            Row::ChatNew(_) => self.new_chat(),
            Row::Chats(ws) | Row::ChatAll(ws) => self.open_chat_list(Some(ws.clone())),
            _ => return false,
        }
        true
    }
}

/// What a chat row's menu offers (and the palette, with a chat in view).
pub const CHAT_ACTIONS: &[&str] = &[
    "chat.open",
    "chat.retitle",
    "chat.pin",
    "chat.archive",
    "chat.agent",
    "chat.preset",
    "chat.export",
    "chat.delete",
];

// ── drawing ─────────────────────────────────────────────────────────────────────────────────

fn fg(app: &App, c: ratatui::style::Color) -> Style {
    Style::new().fg(app.depth.fit(c))
}

/// A chat's status glyph and its colour.
fn glyph(app: &App, c: &ChatInfo) -> (&'static str, ratatui::style::Color) {
    let t = &app.theme;
    if c.attention.is_some() || c.status == Some(AgentStatus::Blocked) {
        return ("◉", t.blocked);
    }
    match c.status {
        Some(AgentStatus::Working | AgentStatus::Starting) => ("●", t.working),
        Some(AgentStatus::Errored) => ("✗", t.error),
        _ if c.pinned => ("★", t.accent),
        _ => (" ", t.dim),
    }
}

/// A sidebar row of the CHATS section, or `None` when `row` isn't one.
pub(crate) fn sidebar_line(
    app: &App,
    row: &Row,
    selected_chat: Option<&str>,
) -> Option<Line<'static>> {
    let t = &app.theme;
    Some(match row {
        Row::Chats(ws) => {
            let arrow =
                if app.collapsed.contains(&crate::tree::chats_key(ws)) { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(format!(" {arrow} "), fg(app, t.dim)),
                Span::styled("CHATS", fg(app, t.dim).add_modifier(Modifier::BOLD)),
                Span::styled("  ＋", fg(app, t.dim)),
            ])
        }
        Row::ChatNew(_) => Line::from(vec![
            Span::raw("     "),
            Span::styled(
                format!("＋ new chat {}", app.keymap.key_for("chat.new").unwrap_or_default()),
                fg(app, t.dim),
            ),
        ]),
        Row::ChatAll(ws) => Line::from(vec![
            Span::raw("     "),
            Span::styled(format!("all chats… ({})", app.tree().chat_count(ws)), fg(app, t.dim)),
        ]),
        Row::Chat(id) => {
            let c = app.chat(id)?;
            let (g, gc) = glyph(app, c);
            let mut name = fg(app, t.text);
            if selected_chat == Some(id.as_str()) {
                name = name.add_modifier(Modifier::BOLD).fg(app.depth.fit(t.accent));
            }
            let mut spans = vec![
                Span::raw("   "),
                Span::styled(format!("{g} "), fg(app, gc)),
                Span::styled(c.label().to_owned(), name),
                Span::styled(format!(" {}", c.agent), fg(app, t.dim)),
            ];
            if c.attention.is_some() {
                spans.push(Span::styled(" ⚠", fg(app, t.blocked)));
            }
            if c.unread > 0 {
                spans.push(Span::styled(" ●", fg(app, t.search).add_modifier(Modifier::BOLD)));
            }
            Line::from(spans)
        }
        _ => return None,
    })
}

/// The chat panel, over the panes (the sidebar stays).
pub(crate) fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(panel) = &app.chat_panel else { return };
    f.render_widget(Clear, area);
    match panel {
        ChatPanel::List(l) => draw_list(app, f, area, l),
        ChatPanel::View(v) => draw_view(app, f, area, v),
    }
}

fn draw_view(app: &App, f: &mut Frame<'_>, area: Rect, v: &ChatViewState) {
    let t = &app.theme;
    let info = v.id.as_deref().and_then(|id| app.chat(id));
    let mut head = vec![Span::styled(
        format!(" {} ", info.map_or("new chat", ChatInfo::label)),
        fg(app, t.accent).add_modifier(Modifier::BOLD),
    )];
    let (agent, preset) = match info {
        Some(c) => (format!("{}@{}", c.agent, c.profile), c.tools.clone()),
        None => (
            v.agent.clone().unwrap_or_else(|| {
                Some(app.config.chat.agent.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| "default agent".into())
            }),
            v.preset.clone().unwrap_or_else(|| app.config.chat.preset.clone()),
        ),
    };
    head.push(Span::styled(format!("· {agent} · tools: {preset}"), fg(app, t.dim)));
    if let Some(c) = info {
        head.push(Span::styled(format!(" · {}", c.addr()), fg(app, t.dim)));
        if c.cost_micro_usd > 0 {
            head.push(Span::styled(
                format!(" · ${:.3}", c.cost_micro_usd as f64 / 1e6),
                fg(app, t.dim),
            ));
        }
        let (g, gc) = glyph(app, c);
        if g != " " && g != "★" {
            head.push(Span::styled(format!(" {g}"), fg(app, gc)));
        }
    }
    let hint = if v.id.is_none() { " C-o agent · Esc back" } else { " Esc back · C-q . menu" };
    head.push(Span::styled(hint, fg(app, t.dim)));
    f.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..area });
    let body = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
    let focused = app.overlay.is_none() && !app.prefix;
    if v.id.is_none() && v.view.items.is_empty() && v.draft.is_empty() {
        let lines = vec![
            Line::styled("Ask anything.", fg(app, t.text).add_modifier(Modifier::BOLD)),
            Line::styled(
                format!("A chat with {agent}: it can search the web, and it is kept."),
                fg(app, t.dim),
            ),
            Line::styled("Type, then ⏎. C-o picks the agent and its tools.", fg(app, t.dim)),
        ];
        let r = Rect { y: body.y + body.height / 3, height: 3, ..body };
        f.render_widget(Paragraph::new(lines).centered(), r);
    }
    let cursor = crate::chat::draw(&v.view, f.buffer_mut(), body, t, app.depth, focused);
    if let Some(c) = cursor {
        f.set_cursor_position(c);
    }
}

fn draw_list(app: &App, f: &mut Frame<'_>, area: Rect, l: &ChatList) {
    let t = &app.theme;
    let rows = app.list_rows(l);
    let scope = match (&l.workspace, l.archived.is_some()) {
        (_, true) => "archived chats".to_owned(),
        (Some(w), _) => format!("chats in {}", crate::ui::workspace_label(w)),
        (None, _) => "every chat".to_owned(),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {scope} "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!("({})", rows.len()), fg(app, t.dim)),
    ])];
    let search = if l.typing || !l.query.is_empty() {
        format!(" / {}{}", l.query, if l.typing { "▏" } else { "" })
    } else {
        " / search · ⏎ open · m new · * pin · x archive · X delete · , title · A archived · Tab all · Esc"
            .to_owned()
    };
    lines.push(Line::styled(search, fg(app, t.dim)));
    if rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if l.query.is_empty() {
                "  no chats here yet: m starts one"
            } else {
                "  nothing matches"
            },
            fg(app, t.dim),
        ));
    }
    let now = ptyro_core::now_ms();
    let mut last_group = "";
    let room = area.height.saturating_sub(3) as usize;
    let skip = l.sel.saturating_sub(room.saturating_sub(4));
    for (i, c) in rows.iter().enumerate().skip(skip) {
        let g = if c.pinned { "Pinned" } else { group(c.last_ms, now) };
        if g != last_group {
            lines.push(Line::styled(format!(" {g}"), fg(app, t.dim).add_modifier(Modifier::BOLD)));
            last_group = g;
        }
        let (glyph, gc) = glyph(app, c);
        let sel = i == l.sel;
        let mut name = fg(app, t.text);
        if sel {
            name = name.add_modifier(Modifier::REVERSED);
        }
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{glyph} "), fg(app, gc)),
            Span::styled(format!(" {} ", c.label()), name),
            Span::styled(format!(" {} · {}", c.agent, crate::ui::ago(c.last_ms)), fg(app, t.dim)),
        ];
        if l.workspace.is_none() {
            spans.push(Span::styled(format!(" · {}", c.workspace), fg(app, t.dim)));
        }
        if c.unread > 0 {
            spans.push(Span::styled(" ●", fg(app, t.search)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_groups() {
        let day = 86_400_000;
        let now = 100 * day + 5000;
        assert_eq!(group(now - 1000, now), "Today");
        assert_eq!(group(now - day, now), "Yesterday");
        assert_eq!(group(now - 3 * day, now), "This week");
        assert_eq!(group(now - 30 * day, now), "Earlier");
    }
}
