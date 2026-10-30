//! Chats in the TUI (SPEC §22.12, ROADMAP CHAT1.7): the CHATS section under each workspace in
//! the sidebar, the chat list (`g`), and the chat view (`m` for a new one), which takes the place
//! of the panes beside the sidebar. The view is the structured sessions' chat view (`chat.rs`)
//! with a header and a composer of its own: ⏎ sends, `a`/`A`/`d` answer a permission card,
//! `Ctrl-C` stops an answer, Esc goes back.

use ptyro_core::chat::{ChatBody, ChatItem};
use ptyro_core::chats::{ChatHit, ChatInfo, ChatSiblings};
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
    /// Only chats imported from the agents' own histories (`i`; `chat ls --imported`).
    pub imported: bool,
    /// Messages that match the query (SPEC §22.10), from every machine, best first.
    pub hits: Vec<ChatHit>,
    /// The query those hits answer (a reply to an older one is dropped).
    pub hits_for: String,
    /// Asked once whether to switch meaning search on (`by:meaning` while it is off).
    pub meaning_asked: bool,
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
    /// A new chat's folders (`/folder DIR` before the first message).
    pub folders: Vec<String>,
    /// Where Esc goes back to.
    pub from_list: Option<ChatList>,
    /// A message is on its way (a new chat being made, or one sent): another waits for it.
    pub sending: bool,
    /// A new chat that keeps nothing (`C-i` flips it before the first message).
    pub incognito: bool,
    /// Where the shown branch has others.
    pub siblings: Vec<ChatSiblings>,
    /// Find in the chat: the matching messages, and which is shown.
    pub found: Vec<String>,
    pub found_at: usize,
    /// Show this message once the branch arrives (a search hit, a citation).
    pub focus: Option<String>,
    /// A new chat's first message goes to these agents, each answer a branch (`/compare A B`).
    pub compare: Vec<String>,
}

/// Why an imported chat isn't retried or edited (the daemon refuses it too).
const IMPORTED_AS_WRITTEN: &str = "an imported chat stays as its agent wrote it: send it a message (it continues as a fork), then retry or edit there";

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

    /// Whether a chat's preset reads its folders, where this TUI can tell: `None` for another
    /// machine's chat, or a preset this config doesn't define (that machine's config says).
    pub(crate) fn chat_reads(&self, c: &ChatInfo) -> Option<bool> {
        if c.machine.is_some() {
            return None;
        }
        self.config.chat.preset(&c.tools).map(|t| t.read)
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
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel
            && let Some(a) = &mut l.archived
        {
            a.retain(|c| c.id != id);
        }
        if let Some(ChatPanel::View(v)) = &self.chat_panel
            && v.id.as_deref() == Some(id)
        {
            self.close_chat();
            self.toast("that chat was deleted");
        }
        self.dirty = true;
    }

    /// The open chat's machine came back (follow it again) or went away (say so).
    pub(crate) fn chat_machine_news(&mut self, m: &ptyro_core::model::MachineInfo) {
        use ptyro_core::model::MachineState;
        let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return };
        let Some(id) = v.id.clone() else { return };
        let mine =
            self.chat_list.iter().any(|c| c.id == id && c.machine.as_deref() == Some(&m.name));
        if !mine {
            return;
        }
        match m.state {
            MachineState::Online => {
                self.send(ClientMsg::ChatUnsubscribe { chat: id.clone() });
                self.send(ClientMsg::ChatSubscribe { chat: id });
            }
            MachineState::Offline => {
                let text = format!("{} went offline; this chat carries on when it is back", m.name);
                let item = ChatItem::new(
                    format!("offline-{}", ptyro_core::now_ms()),
                    ChatBody::Notice { level: ptyro_core::chat::NoticeLevel::Warn, text },
                );
                let d = v.draft.clone();
                v.view.apply(v.view.rev, false, vec![item], d);
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// A followed chat's items (with a reset: where its branch has others).
    pub(crate) fn chat_live(
        &mut self,
        chat: String,
        rev: u64,
        reset: bool,
        items: Vec<ChatItem>,
        siblings: Vec<ChatSiblings>,
    ) {
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.as_deref() == Some(chat.as_str())
        {
            let draft = v.draft.clone();
            v.view.apply(rev, reset, items, draft);
            if reset {
                v.view.set_marks(siblings.iter().map(|s| {
                    (s.node.clone(), (s.at, s.of, s.label().unwrap_or_default().to_owned()))
                }));
                v.siblings = siblings;
                // Another branch: find-in-chat's matches are this one's.
                if let Some(q) = v.view.highlight.clone() {
                    v.found = v.view.find(&q);
                    v.found_at = 0;
                }
            }
            if let Some(n) = v.focus.clone()
                && v.view.items.iter().any(|i| i.id == n)
            {
                v.view.focus_on(&n);
                v.focus = None;
            }
            self.dirty = true;
        }
    }

    /// Open a chat at a message (a search hit, a citation): its branch is shown first.
    pub(crate) fn open_chat_at(&mut self, chat: &str, node: Option<&str>) {
        if let Some(n) = node {
            self.request(
                Pending::Done(String::new()),
                Request::ChatBranch { chat: chat.to_owned(), to: n.to_owned() },
            );
        }
        self.open_chat(chat);
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
            v.focus = node.map(str::to_owned);
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
            folders: Vec::new(),
            from_list: None,
            sending: false,
            incognito: self.config.chat.incognito,
            siblings: Vec::new(),
            compare: Vec::new(),
            found: Vec::new(),
            found_at: 0,
            focus: None,
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
                // The chat already shown (a hit or citation in it): followed again, not left.
                let same =
                    matches!(&other, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id));
                self.chat_panel = other;
                if same {
                    self.send(ClientMsg::ChatUnsubscribe { chat: id.to_owned() });
                } else {
                    self.leave_chat();
                }
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
            folders: Vec::new(),
            from_list,
            sending: false,
            incognito: false,
            siblings: Vec::new(),
            compare: Vec::new(),
            found: Vec::new(),
            found_at: 0,
            focus: None,
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
            imported: false,
            hits: Vec::new(),
            hits_for: String::new(),
            meaning_asked: false,
        }));
        self.dirty = true;
    }

    /// Stop following the chat shown, if one is (a side chat not kept goes with it).
    fn leave_chat(&mut self) {
        if let Some(ChatPanel::View(v)) = &self.chat_panel
            && let Some(id) = v.id.clone()
        {
            self.left_chat(&id);
        }
    }

    /// A chat's view was left: stop following it; a side chat goes, unless kept (SPEC §22.7).
    fn left_chat(&mut self, id: &str) {
        self.send(ClientMsg::ChatUnsubscribe { chat: id.to_owned() });
        if self.chat(id).is_some_and(|c| c.side_of.is_some()) {
            self.request(
                Pending::Done("side chat closed".into()),
                Request::ChatDelete { chat: id.to_owned() },
            );
        }
    }

    pub(crate) fn close_chat(&mut self) {
        self.leave_chat();
        self.chat_panel = None;
        self.dirty = true;
    }

    /// The chat being acted on: the one shown, else the selected sidebar row's, else the list's.
    pub(crate) fn chat_target(&self) -> Option<String> {
        // The sidebar's selected chat row, when the sidebar has the keys (or a row menu ran).
        if self.focus == Focus::Sidebar
            && let Some(Row::Chat(id)) = self.selected_row()
        {
            return Some(id);
        }
        match &self.chat_panel {
            Some(ChatPanel::View(v)) => return v.id.clone(),
            Some(ChatPanel::List(l)) => {
                let rows = self.list_rows(l);
                // Past the chats come the message hits: theirs is the hit's chat.
                return match rows.get(l.sel) {
                    Some(c) => Some(c.id.clone()),
                    None => match l.hits.get(l.sel - rows.len()) {
                        Some(h) => Some(h.chat.id.clone()),
                        None => rows.last().map(|c| c.id.clone()),
                    },
                };
            }
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
            "chat.incognito" => {
                self.new_chat();
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.incognito = true;
                }
                return;
            }
            "chat.search" => {
                // Every chat, every workspace, every machine: the list, typing its search.
                self.open_chat_list(None);
                if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
                    l.typing = true;
                }
                return;
            }
            "chat.find" => {
                self.overlay = Some(Overlay::Prompt {
                    title: "Ask your chats (an agent searches them and cites what it finds)".into(),
                    text: String::new(),
                    kind: PromptKind::ChatFind,
                });
                return;
            }
            "chat.list" => {
                if matches!(self.chat_panel, Some(ChatPanel::List(_))) {
                    return self.close_chat();
                }
                return self.open_chat_list(Some(self.chat_workspace()));
            }
            "chat.import" => {
                // It copies coding history into chat.db: asked first (SPEC §22.10), and narrowed
                // there as `chat import`'s flags do.
                self.overlay = Some(Overlay::Prompt {
                    title: "Import your agents' own histories as read-only, searchable chats (their text is copied into chat.db)? ⏎ every agent's and profile's; or narrow it: codex --since 30d --profile work --dry-run (counts only). Esc: no".into(),
                    text: String::new(),
                    kind: PromptKind::ChatImport,
                });
                return;
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
            keep: None,
            folders: None,
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
            "chat.folder" => {
                let now = info.map(|c| c.folders.join(", ")).unwrap_or_default();
                self.overlay = Some(Overlay::Prompt {
                    title: format!(
                        "A folder this chat may read (none: no folders){}",
                        if now.is_empty() { String::new() } else { format!("; now {now}") }
                    ),
                    text: String::new(),
                    kind: PromptKind::ChatFolder(id),
                });
            }
            "chat.task" => self.chat_task(id, TaskArgs::default()),
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
            "chat.retry" | "chat.edit" if info.as_ref().is_some_and(|c| c.source.is_some()) => {
                self.toast(IMPORTED_AS_WRITTEN)
            }
            "chat.retry" => self.request(
                Pending::Done("asked again: the old answer stays as a branch".into()),
                Request::ChatRetry { chat: id, node: None },
            ),
            "chat.edit" => self.pick_message(Purpose::ChatEditPick, "Edit which message?"),
            "chat.fork" => self.pick_message(Purpose::ChatForkAt, "Fork from where?"),
            "chat.side" => self.request(
                Pending::ChatOpen,
                Request::ChatFork { chat: id, node: None, agent: None, side: true },
            ),
            "chat.keep" => {
                if info.as_ref().is_some_and(|c| c.side_of.is_some()) {
                    // Kept at once here, so leaving before the daemon's word doesn't drop it.
                    if let Some(c) = self.chat_list.iter_mut().find(|c| c.id == id) {
                        c.side_of = None;
                    }
                    self.request(
                        Pending::Done("kept: it is a chat of its own now".into()),
                        Request::ChatUpdate {
                            chat: id,
                            title: None,
                            pinned: None,
                            archived: None,
                            agent: None,
                            preset: None,
                            keep: Some(true),
                            folders: None,
                        },
                    );
                } else {
                    self.toast("not a side chat: nothing to keep");
                }
            }
            "chat.branch" => self.pick_branch(),
            "chat.compare" => {
                if info.as_ref().is_some_and(|c| c.source.is_some()) {
                    return self.toast(
                        "an imported chat stays as its agent wrote it: send it a message (it continues as a fork), then compare there",
                    );
                }
                self.overlay = Some(Overlay::Prompt {
                    title: "Compare: which agents answer the last message again? (space-separated; empty: every installed one)".into(),
                    text: String::new(),
                    kind: PromptKind::ChatCompare(id),
                });
            }
            "chat.pick" => {
                self.request(Pending::ChatPicked, Request::ChatPick { chat: id, node: None })
            }
            "chat.find_in" => {
                self.overlay = Some(Overlay::Prompt {
                    title: "Find in this chat".into(),
                    text: String::new(),
                    kind: PromptKind::ChatFindIn,
                });
            }
            "chat.cite" => self.pick_citation(),
            _ => {}
        }
    }

    /// The person's messages of the shown branch, newest first, to pick one (`purpose`).
    /// (The view's own chat: its messages are the ones offered.)
    fn pick_message(&mut self, purpose: fn(String) -> Purpose, title: &str) {
        let Some(ChatPanel::View(v)) = &self.chat_panel else {
            return self.toast("open the chat first (⏎ on it)");
        };
        let Some(chat) = v.id.clone() else { return self.toast("no message yet") };
        let mut items: Vec<Item> = Vec::new();
        if matches!(purpose(String::new()), Purpose::ChatForkAt(_)) {
            items.push(Item {
                label: "from the end".into(),
                detail: "the whole conversation".into(),
                key: None,
                target: Target::Chat(String::new()),
            });
        }
        for it in v.view.items.iter().rev() {
            if let ChatBody::User { text } = &it.body {
                items.push(Item {
                    label: ptyro_core::chats::first_line_title(text),
                    detail: crate::ui::ago(it.at_ms),
                    key: None,
                    target: Target::Chat(it.id.clone()),
                });
            }
        }
        if items.is_empty() {
            return self.toast("no message yet");
        }
        self.overlay =
            Some(Overlay::Picker { palette: Palette::new(title, items), purpose: purpose(chat) });
    }

    /// The branch points of the shown chat, each choice of each, to show another.
    fn pick_branch(&mut self) {
        let Some(ChatPanel::View(v)) = &self.chat_panel else {
            return self.toast("open the chat first (⏎ on it)");
        };
        let Some(chat) = v.id.clone() else { return self.toast("no message yet") };
        let mut items = Vec::new();
        for s in &v.siblings {
            let named = s.label().is_some();
            for (i, id) in s.ids.iter().enumerate() {
                let here = i + 1 == s.at as usize;
                let who = match s.agents.get(i) {
                    Some(a) if named && !a.is_empty() => format!(" ({a})"),
                    _ => String::new(),
                };
                items.push(Item {
                    label: format!("{}/{}{who} at message {}", i + 1, s.of, short(&s.node)),
                    detail: if here { "shown".into() } else { "show this branch".into() },
                    key: None,
                    target: Target::Chat(id.clone()),
                });
            }
        }
        if items.is_empty() {
            return self.toast("one branch only: retry an answer (/retry) or edit a message (/edit) to make another");
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Which branch?", items),
            purpose: Purpose::ChatBranchPick(chat),
        });
    }

    /// Citations (`c:7f3k/01jb…`) in the shown chat's answers, to open one.
    fn pick_citation(&mut self) {
        let Some(ChatPanel::View(v)) = &self.chat_panel else { return };
        let mut seen = Vec::new();
        for it in &v.view.items {
            if let ChatBody::Assistant { text, .. } = &it.body {
                for c in citations(text) {
                    if !seen.contains(&c) {
                        seen.push(c);
                    }
                }
            }
        }
        if seen.is_empty() {
            return self.toast("no citations (c:7f3k/…) in this chat");
        }
        let items = seen
            .into_iter()
            .map(|c| Item {
                label: c.clone(),
                detail: "open it there".into(),
                key: None,
                target: Target::Chat(c),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Open a citation", items),
            purpose: Purpose::ChatCite,
        });
    }

    /// Find `text` in the chat shown: highlight it and show the first message with it.
    pub(crate) fn find_in_chat(&mut self, text: &str) {
        let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return };
        v.found = v.view.find(text);
        v.found_at = 0;
        v.view.highlight = Some(text.to_owned()).filter(|t| !t.is_empty());
        v.view.apply(v.view.rev, false, Vec::new(), v.draft.clone());
        let msg = match v.found.first().cloned() {
            Some(n) => {
                v.view.focus_on(&n);
                format!("1/{} · C-f next · Esc clears", v.found.len())
            }
            None => format!("{text:?}: not in this chat (C-q C-g searches every chat)"),
        };
        self.toast(msg);
    }

    /// What a picker of ours picked.
    pub(crate) fn chat_pick(&mut self, purpose: Purpose, value: String) {
        match purpose {
            Purpose::ChatForkAt(chat) => self.request(
                Pending::ChatOpen,
                Request::ChatFork {
                    chat,
                    node: Some(value).filter(|v| !v.is_empty()),
                    agent: None,
                    side: false,
                },
            ),
            Purpose::ChatEditPick(chat) => {
                let text = match &self.chat_panel {
                    Some(ChatPanel::View(v)) => v.view.items.iter().find_map(|i| match &i.body {
                        ChatBody::User { text } if i.id == value => Some(text.clone()),
                        _ => None,
                    }),
                    _ => None,
                };
                self.overlay = Some(Overlay::Prompt {
                    title: "Say it differently (the old message and its answer stay as a branch)"
                        .into(),
                    text: text.unwrap_or_default(),
                    kind: PromptKind::ChatEdit(chat, value),
                });
            }
            Purpose::ChatBranchPick(chat) => {
                self.request(Pending::Done(String::new()), Request::ChatBranch { chat, to: value })
            }
            Purpose::ChatCite => {
                // `c:7f3k/01jb…`, maybe with a machine: the chat, then the node.
                let (chat, node) = value.rsplit_once('/').unwrap_or((value.as_str(), ""));
                let node = Some(node.to_owned()).filter(|n| !n.is_empty());
                // The view follows a chat by its id: an address is looked up first.
                match self.chat_list.iter().find(|c| c.addr() == chat || c.id == chat) {
                    Some(c) => {
                        let id = c.id.clone();
                        self.open_chat_at(&id, node.as_deref());
                    }
                    None => self.request(
                        Pending::ChatOpenAt(node),
                        Request::ChatRead { chat: chat.to_owned(), all: false },
                    ),
                }
            }
            _ => {}
        }
    }

    /// A slash command typed into the composer (SPEC §22.12); `false`: not one.
    fn chat_slash(&mut self, chat: Option<String>, text: &str) -> bool {
        let Some(cmd) = text.trim().strip_prefix('/') else { return false };
        let (name, arg) = cmd.split_once(' ').map_or((cmd, ""), |(a, b)| (a, b.trim()));
        let need = |me: &mut Self| {
            me.toast("send a first message, then /commands work on the chat");
        };
        match (name, chat) {
            ("help" | "?", _) => self.toast(
                "/retry /edit [text] /compare [AGENTS…] [question] /pick /fork /side [question] /keep /agent NAME /tools NAME /folder DIR|none /task [PROJECT] [--agent A] [--name N] /title TEXT /search WORDS /find QUESTION /export /incognito /branch /cite",
            ),
            ("incognito", _) => self.chat_action("chat.incognito"),
            ("search", _) => {
                self.chat_action("chat.search");
                if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
                    l.query = arg.to_owned();
                }
                self.search_chats();
            }
            ("find", _) if !arg.is_empty() => {
                self.request(
                    Pending::ChatOpen,
                    Request::ChatFind { question: arg.to_owned(), agent: None, workspace: None },
                );
            }
            ("find", _) => self.chat_action("chat.find"),
            ("tools", None) if arg.is_empty() => self.pick_chat_preset(None),
            ("tools", None) => self.chat_picked(arg.to_owned(), false, None),
            ("folder" | "folders", c) if !arg.is_empty() => self.chat_folder(c, arg),
            ("folder" | "folders", None) => {
                self.toast("/folder DIR: a folder the new chat may read (/folder none: none)")
            }
            ("folder" | "folders", Some(_)) => self.chat_action("chat.folder"),
            ("compare", c) => self.chat_compare_slash(c, arg),
            (_, None) => need(self),
            ("pick", Some(_)) => self.chat_action("chat.pick"),
            ("task", Some(c)) => match TaskArgs::parse(arg) {
                Ok(t) => self.chat_task(c, t),
                Err(e) => self.toast_err(e),
            },
            ("retry" | "edit", Some(c)) if self.chat(&c).is_some_and(|i| i.source.is_some()) => {
                self.toast(IMPORTED_AS_WRITTEN)
            }
            ("retry", Some(_)) => self.chat_action("chat.retry"),
            ("edit", Some(c)) if !arg.is_empty() => {
                let last = match &self.chat_panel {
                    Some(ChatPanel::View(v)) => v.view.items.iter().rev().find_map(|i| {
                        matches!(i.body, ChatBody::User { .. }).then(|| i.id.clone())
                    }),
                    _ => None,
                };
                match last {
                    Some(n) => self.request(
                        Pending::Done(String::new()),
                        Request::ChatEdit { chat: c, node: n, text: arg.to_owned() },
                    ),
                    None => self.toast("no message to edit"),
                }
            }
            ("edit", Some(_)) => self.chat_action("chat.edit"),
            ("fork", Some(_)) => self.chat_action("chat.fork"),
            ("side", Some(c)) if !arg.is_empty() => self.request(
                Pending::ChatSideAsk(arg.to_owned()),
                Request::ChatFork { chat: c, node: None, agent: None, side: true },
            ),
            ("side", Some(_)) => self.chat_action("chat.side"),
            ("keep", Some(_)) => self.chat_action("chat.keep"),
            ("branch", Some(_)) => self.chat_action("chat.branch"),
            ("cite", Some(_)) => self.chat_action("chat.cite"),
            ("export", Some(_)) => self.chat_action("chat.export"),
            ("agent", Some(_)) if arg.is_empty() => self.chat_action("chat.agent"),
            ("agent", Some(c)) => self.chat_picked(arg.to_owned(), true, Some(c)),
            ("tools", Some(_)) if arg.is_empty() => self.chat_action("chat.preset"),
            ("tools", Some(c)) => self.chat_picked(arg.to_owned(), false, Some(c)),
            ("title", Some(_)) if arg.is_empty() => self.chat_action("chat.retitle"),
            ("title", Some(c)) => self.request(
                Pending::Done("retitled".into()),
                Request::ChatUpdate {
                    chat: c,
                    title: Some(arg.to_owned()),
                    pinned: None,
                    archived: None,
                    agent: None,
                    preset: None,
                    keep: None,
                    folders: None,
                },
            ),
            (other, _) => self.toast(format!("/{other}: not a command (/help lists them)")),
        }
        true
    }

    /// Ask the daemon for the list's query's message hits (from 3 letters on, every machine).
    /// The agents ptyro knows (for `/compare`'s words): the built-in ones and yours.
    fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> =
            ["claude", "codex", "gemini", "opencode"].map(String::from).into();
        let mine: Vec<String> =
            self.config.agents.keys().filter(|k| !names.contains(k)).cloned().collect();
        names.extend(mine);
        names
    }

    /// `/compare [AGENTS…] [question]`: the words that name agents (`codex`, `codex@work`), then
    /// the question. In a new chat its first message goes to them; in a chat the question (or,
    /// with none, the last message) does.
    fn chat_compare_slash(&mut self, chat: Option<String>, arg: &str) {
        let names = self.agent_names();
        let mut agents: Vec<String> = Vec::new();
        let mut words = arg.split_whitespace().peekable();
        while let Some(named) = words.peek().and_then(|w| agent_words(&names, w)) {
            agents.extend(named);
            words.next();
        }
        let text = words.collect::<Vec<&str>>().join(" ");
        let Some(c) = chat else {
            let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return };
            if agents.len() < 2 {
                return self.toast(
                    "/compare AGENT AGENT…: the first message goes to each (e.g. /compare claude codex)",
                );
            }
            v.compare = agents.clone();
            if !text.is_empty() {
                v.draft = text;
                let d = v.draft.clone();
                v.view.apply(v.view.rev, false, Vec::new(), d);
            }
            return self.toast(format!(
                "the first message goes to {} (each answer a branch); ⏎ sends it",
                agents.join(", ")
            ));
        };
        self.compare_request(c, agents, Some(text).filter(|t| !t.is_empty()));
    }

    /// `chat.import`'s answer: what to import (empty: everything), then the import.
    pub(crate) fn chat_import(&mut self, text: &str) {
        match import_request(text) {
            Ok(req) => {
                let dry = matches!(req, Request::ChatImport { dry_run: true, .. });
                self.toast(if dry {
                    "counting what your agents' histories would bring…"
                } else {
                    "importing your agents' histories…"
                });
                self.request(Pending::ChatImport, req);
            }
            Err(e) => self.toast_err(e),
        }
    }

    /// `chat.compare`'s answer: the agents typed (empty: every installed one).
    pub(crate) fn chat_compare(&mut self, chat: String, text: &str) {
        let agents = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
            .collect();
        self.compare_request(chat, agents, None);
    }

    fn compare_request(&mut self, chat: String, agents: Vec<String>, text: Option<String>) {
        let who =
            if agents.is_empty() { "every installed agent".to_owned() } else { agents.join(", ") };
        let done = Pending::Done(format!(
            "asking {who}: each answer is a branch (Alt-←/→ flips, /pick keeps one)"
        ));
        let req = Request::ChatCompare { chat, agents, text, confirmed: false };
        self.request(Pending::Confirm(Box::new(req.clone()), Box::new(done)), req);
    }

    /// The reply to a message sent to `sent_to`: when it names another chat, the message went
    /// to a fork (an imported chat carries on as one). The view goes there if it still shows
    /// the chat the message went to.
    pub(crate) fn chat_forked(&mut self, sent_to: &str, info: ChatInfo) {
        if info.id == sent_to {
            return;
        }
        let shown = matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(sent_to));
        let id = info.id.clone();
        let label = info.label().to_owned();
        self.chat_updated(info);
        if shown {
            self.open_chat(&id);
        }
        self.toast(format!("continued as a fork ({label}): the imported chat stays as it was"));
    }

    pub(crate) fn search_chats(&mut self) {
        let Some(ChatPanel::List(l)) = &mut self.chat_panel else { return };
        let q = l.query.trim().to_owned();
        if q.chars().count() < 3 {
            l.hits.clear();
            l.hits_for.clear();
            return;
        }
        let req = Request::ChatSearch {
            query: q.clone(),
            chat: None,
            workspace: l.workspace.clone(),
            agent: None,
            pinned: false,
            archived: l.archived.is_some(),
            limit: Some(30),
            machines: true,
        };
        self.request(Pending::ChatHits(q), req);
    }

    /// `by:meaning` with meaning search off: offer, once, to switch it on.
    pub(crate) fn offer_meaning(&mut self, why: String) {
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel
            && !l.meaning_asked
        {
            l.meaning_asked = true;
            self.overlay = Some(Overlay::Confirm {
                text: format!("{why}. Switch it on? [y/N]"),
                action: Confirmed::MeaningOn,
            });
            self.dirty = true;
        }
    }

    /// Switch meaning search on in the config file, as the settings panel would; the daemon
    /// sees the change and downloads its model.
    pub(crate) fn meaning_on(&mut self) {
        let layer =
            ptyro_core::settings::Layer::User(ptyro_core::config::Config::path(&self.paths));
        let op = ptyro_core::settings::Op::Set { key: "chat.meaning".into(), value: "true".into() };
        let lk = ptyro_core::settings::Lookups::default();
        match ptyro_core::settings::apply(&layer, &op, &lk, &|_| Ok(())) {
            Ok(_) => {
                self.toast(
                    "meaning search is on: its model is downloading (ptyro doctor shows how far)",
                );
                self.reload_config_with(false);
            }
            Err(e) => self.toast_err(format!("chat.meaning: {e}")),
        }
    }

    pub(crate) fn got_hits(&mut self, query: String, hits: Vec<ChatHit>) {
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel
            && l.query.trim() == query
        {
            l.hits = hits;
            l.hits_for = query;
            self.dirty = true;
        }
    }

    fn archived_shown(&self) -> bool {
        matches!(&self.chat_panel, Some(ChatPanel::List(l)) if l.archived.is_some())
    }

    /// `/folder DIR`: one more folder the chat (or the new chat being written) may read;
    /// `none` takes them all away.
    pub(crate) fn chat_folder(&mut self, chat: Option<String>, arg: &str) {
        let clear = matches!(arg, "none" | "-");
        let dir = if arg.starts_with('/') || arg.starts_with('~') || clear {
            arg.to_owned()
        } else {
            let p = std::env::current_dir().map(|d| d.join(arg)).unwrap_or_else(|_| arg.into());
            p.canonicalize().unwrap_or(p).to_string_lossy().into_owned()
        };
        let Some(chat) = chat else {
            if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                if clear {
                    v.folders.clear();
                } else if !v.folders.contains(&dir) {
                    v.folders.push(dir.clone());
                }
            }
            let msg = if clear { "no folders".to_owned() } else { format!("it may read {dir}") };
            self.toast(msg);
            self.dirty = true;
            return;
        };
        let mut folders = if clear {
            Vec::new()
        } else {
            self.chat(&chat).map(|c| c.folders.clone()).unwrap_or_default()
        };
        if !clear && !folders.contains(&dir) {
            folders.push(dir.clone());
        }
        let reads = self.chat(&chat).and_then(|c| self.chat_reads(c));
        let msg = match (clear, reads) {
            (true, _) => "no folders: the next message starts it without any".to_owned(),
            (false, Some(false)) => {
                format!("{dir} attached; this preset reads no files: /tools read lets it")
            }
            (false, _) => format!("it may read {dir} from the next message on"),
        };
        self.request(
            Pending::Done(msg),
            Request::ChatUpdate {
                chat,
                title: None,
                pinned: None,
                archived: None,
                agent: None,
                preset: None,
                keep: None,
                folders: Some(folders),
            },
        );
    }

    /// `chat.task` (SPEC §22.7): the chat to a new worktree of a project; asks which project
    /// when its workspace has several (or none and there are several).
    pub(crate) fn chat_task(&mut self, chat: String, task: TaskArgs) {
        let TaskArgs { project, agent, name } = task;
        let project = project.or_else(|| {
            let ws = self.chat(&chat).map(|c| c.workspace.clone()).unwrap_or_default();
            let ids: Vec<String> = self
                .workspaces
                .iter()
                .find(|w| w.name == ws)
                .map(|w| w.projects.clone())
                .unwrap_or_default();
            let here: Vec<&ptyro_core::model::ProjectInfo> =
                self.projects.iter().filter(|p| p.machine.is_none()).collect();
            let mine: Vec<&ptyro_core::model::ProjectInfo> =
                here.iter().copied().filter(|p| ids.contains(&p.id)).collect();
            match (mine.as_slice(), here.as_slice()) {
                ([p], _) | ([], [p]) => Some(p.slug.clone()),
                _ => None,
            }
        });
        if let Some(p) = project {
            let done = Pending::Done("made a task: the chat says where it went".into());
            let req = Request::ChatTask { chat, project: Some(p), agent, name, confirmed: false };
            return self.request(Pending::Confirm(Box::new(req.clone()), Box::new(done)), req);
        }
        let items: Vec<Item> = self
            .projects
            .iter()
            .filter(|p| p.machine.is_none())
            .map(|p| Item {
                label: p.slug.clone(),
                detail: p.root.display().to_string(),
                key: None,
                target: Target::Agent(p.slug.clone()),
            })
            .collect();
        if items.is_empty() {
            return self.toast("a task needs a project: add one first (project.add)");
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Which project should it work in?", items),
            purpose: Purpose::ChatTaskProject(chat, TaskArgs { project: None, agent, name }),
        });
    }

    /// Pick an agent: for the chat `id` (switching it), or for the new chat being written.
    pub(crate) fn pick_chat_agent(&mut self, id: Option<String>) {
        let mut names: Vec<String> =
            ["claude", "codex", "gemini", "opencode"].map(String::from).into();
        let mine: Vec<String> =
            self.config.agents.keys().filter(|k| !names.contains(k)).cloned().collect();
        names.extend(mine);
        // Each with where its program is (an npx cache, nvm…), or how to install it.
        let path = std::env::var("PATH").unwrap_or_default();
        let items = names
            .into_iter()
            .map(|n| {
                let prog = ptyro_agents::command(&self.config, &n)
                    .and_then(|a| a.into_iter().next())
                    .unwrap_or_else(|| n.clone());
                let detail = match ptyro_agents::locate::find(&prog, &path) {
                    Some(f) => f.describe(),
                    None => ptyro_agents::locate::install_hint(&prog)
                        .map_or_else(|| "not found".into(), |h| format!("not installed: {h}")),
                };
                Item { detail, label: n.clone(), key: None, target: Target::Agent(n) }
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
                Item { label: p.clone(), detail: t.describe(), key: None, target: Target::Agent(p) }
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
                        keep: None,
                        folders: None,
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

    /// A message went (or didn't) to `chat` (`None`: a new chat's first message): a refused one
    /// is put back to send again, when that chat is still the one shown. A new chat's first
    /// message refused (or not agreed to) made no chat: it no longer shows as sent.
    pub(crate) fn chat_sent(&mut self, chat: Option<&str>, text: String, err: Option<String>) {
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.as_deref() == chat
        {
            v.sending = false;
            if err.is_some() && v.draft.is_empty() {
                v.draft = text;
            }
            let fresh = err.is_some() && chat.is_none();
            let d = v.draft.clone();
            v.view.apply(v.view.rev, fresh, Vec::new(), d);
        }
        if let Some(e) = err {
            self.toast(e);
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
            v.sending = false;
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
        let alt = key.mods.has(Mods::ALT);
        v.view.settle_focus();
        // Alt-←/→: the newest branch point's other answers (or messages), in turn.
        if alt
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
            && let (Some(id), Some(s)) = (v.id.clone(), v.siblings.last().cloned())
        {
            let at = s.at as usize - 1;
            let to = if key.code == KeyCode::Left {
                (at + s.ids.len() - 1) % s.ids.len()
            } else {
                (at + 1) % s.ids.len()
            };
            self.request(
                Pending::Done(String::new()),
                Request::ChatBranch { chat: id, to: s.ids[to].clone() },
            );
            self.chat_panel = Some(ChatPanel::View(Box::new(v)));
            return;
        }
        // C-f: find in the chat; again: the next match.
        if ctrl && key.code == (KeyCode::Char { c: 'f' }) {
            if v.found.is_empty() {
                self.chat_panel = Some(ChatPanel::View(Box::new(v)));
                return self.chat_action("chat.find_in");
            }
            v.found_at = (v.found_at + 1) % v.found.len();
            let n = v.found[v.found_at].clone();
            v.view.focus_on(&n);
            self.toast(format!("{}/{}", v.found_at + 1, v.found.len()));
            self.chat_panel = Some(ChatPanel::View(Box::new(v)));
            return;
        }
        // C-i (or Tab: what most terminals send for it): a new chat that keeps nothing (before
        // its first message).
        if (key.code == KeyCode::Tab || ctrl && key.code == (KeyCode::Char { c: 'i' }))
            && v.id.is_none()
        {
            v.incognito = !v.incognito;
            self.toast(if v.incognito {
                "incognito: nothing of this chat is kept, and it ends with its agent"
            } else {
                "kept: this chat is saved and searchable"
            });
            self.chat_panel = Some(ChatPanel::View(Box::new(v)));
            return;
        }
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
                } else if v.view.highlight.is_some() {
                    v.view.highlight = None;
                    v.found.clear();
                } else {
                    if let Some(id) = &v.id {
                        self.left_chat(id);
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
            KeyCode::Enter if v.sending => self.toast("one moment: the last message is on its way"),
            KeyCode::Enter if v.draft.trim_start().starts_with('/') => {
                let text = std::mem::take(&mut v.draft);
                let chat = v.id.clone();
                v.view.apply(v.view.rev, false, Vec::new(), String::new());
                self.chat_panel = Some(ChatPanel::View(Box::new(v)));
                self.chat_slash(chat, &text);
                return;
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut v.draft);
                if !text.trim().is_empty() {
                    v.sending = true;
                    match v.id.clone() {
                        Some(id) => self.request(
                            Pending::ChatSent { chat: id.clone(), text: text.clone() },
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
                            let req = Request::ChatNew {
                                workspace: Some(v.workspace.clone()),
                                agent: v.agent.clone(),
                                preset: v.preset.clone(),
                                title: None,
                                prompt: Some(text.clone()),
                                incognito: v.incognito,
                                folders: v.folders.clone(),
                                compare: v.compare.clone(),
                                confirmed: false,
                            };
                            let made = Box::new(Pending::ChatMade(text));
                            self.request(Pending::Confirm(Box::new(req.clone()), made), req);
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
        let (q, agent) = title_query(&l.query);
        source
            .iter()
            // Side chats sit under their chat, incognito ones are never "all chats" (§22.12).
            .filter(|c| c.side_of.is_none() && !c.incognito)
            .filter(|c| l.workspace.as_ref().is_none_or(|w| c.workspace == *w))
            .filter(|c| !l.imported || c.source.is_some())
            .filter(|c| agent.as_ref().is_none_or(|a| c.agent.to_lowercase().contains(a)))
            .filter(|c| {
                q.is_empty()
                    || c.label().to_lowercase().contains(&q)
                    || c.agent.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    fn chat_list_key(&mut self, mut l: ChatList, key: KeyEvent) {
        // Chats whose title matches, then the messages that do (from every machine).
        let chats = self.list_rows(&l).len();
        let n = chats + l.hits.len();
        // Rows go (deleted, archived, filtered): the selection stays on one.
        l.sel = l.sel.min(n.saturating_sub(1));
        let ctrl = key.mods.has(Mods::CTRL);
        if l.typing {
            let mut changed = false;
            match key.code {
                KeyCode::Esc => {
                    l.typing = false;
                    l.query.clear();
                    changed = true;
                }
                KeyCode::Enter | KeyCode::Down => l.typing = false,
                KeyCode::Backspace => {
                    l.query.pop();
                    changed = true;
                }
                KeyCode::Char { c } if !ctrl => {
                    l.query.push(c);
                    l.sel = 0;
                    changed = true;
                }
                _ => {}
            }
            self.chat_panel = Some(ChatPanel::List(l));
            if changed {
                self.search_chats();
            }
            return;
        }
        // ⏎ on a message hit: that chat, at that message (its branch shown).
        if key.code == KeyCode::Enter
            && l.sel >= chats
            && let Some(h) = l.hits.get(l.sel - chats).cloned()
        {
            self.chat_panel = Some(ChatPanel::List(l));
            return self.open_chat_at(&h.chat.id, h.node.as_deref());
        }
        // Tab and `A` change what is searched: the hits are asked again.
        let rescope = matches!(key.code, KeyCode::Tab | KeyCode::Char { c: 'A' });
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
            KeyCode::Char { c: 'I' } => Some("chat.import"),
            KeyCode::Char { c: 'i' } => {
                l.imported = !l.imported;
                l.sel = 0;
                None
            }
            KeyCode::Char { c: '.' } => {
                self.chat_panel = Some(ChatPanel::List(l));
                return self.chat_menu();
            }
            _ => None,
        };
        self.chat_panel = Some(ChatPanel::List(l));
        if rescope {
            self.search_chats();
        }
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
    "chat.folder",
    "chat.task",
    "chat.compare",
    "chat.pick",
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
        _ if c.incognito => ("◌", t.dim),
        _ if c.pinned => ("★", t.accent),
        _ if c.source.is_some() => ("⇣", t.dim),
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
            // Another machine's while it is down: listed as it was, greyed (SPEC §22.11).
            let offline = app
                .machine_of_name(&c.workspace)
                .is_some_and(|m| m.state != ptyro_core::model::MachineState::Online);
            let mut name = fg(app, if offline { t.exited } else { t.text });
            if selected_chat == Some(id.as_str()) {
                name = name.add_modifier(Modifier::BOLD).fg(app.depth.fit(t.accent));
            }
            let indent = if c.side_of.is_some() { "     ↳" } else { "   " };
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(format!("{g} "), fg(app, gc)),
                Span::styled(c.label().to_owned(), name),
                Span::styled(format!(" {}", c.agent), fg(app, t.dim)),
            ];
            if c.attention.is_some() {
                spans.push(Span::styled(" ⚠", fg(app, t.blocked)));
            } else if c.shell {
                // A shell preset: marked as a `dangerous` session is.
                spans.push(Span::styled(" ⚠", fg(app, t.error)));
            }
            if c.unread > 0 {
                spans.push(Span::styled(" ●", fg(app, t.search).add_modifier(Modifier::BOLD)));
            }
            if offline {
                spans.push(Span::styled(" offline", fg(app, t.dim)));
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
            // As the daemon chooses: given folders and no preset, a chat reads.
            v.preset.clone().unwrap_or_else(|| {
                let default = app.config.chat.preset.clone();
                let reads = app.config.chat.preset(&default).is_some_and(|t| t.read);
                if v.folders.is_empty() || reads { default } else { "read".into() }
            }),
        ),
    };
    head.push(Span::styled(format!("· {agent} · tools: {preset}"), fg(app, t.dim)));
    let tools = app.config.chat.preset(&preset);
    let (best_effort, shell, folders, reads) = match info {
        Some(c) => (c.best_effort, c.shell, c.folders.len(), app.chat_reads(c)),
        None => {
            (false, tools.as_ref().is_some_and(|t| t.shell), v.folders.len(), tools.map(|t| t.read))
        }
    };
    if best_effort {
        head.push(Span::styled(" · best effort", fg(app, t.blocked)));
    }
    if shell {
        head.push(Span::styled(" · ⚠ shell", fg(app, t.error)));
    }
    if folders > 0 {
        let s = if folders == 1 { "" } else { "s" };
        // Said only where it is known (a preset of this machine's config).
        let note = if reads == Some(false) { " (not read: /tools read)" } else { "" };
        head.push(Span::styled(format!(" · {folders} folder{s}{note}"), fg(app, t.dim)));
    }
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
    let side = info.and_then(|c| c.side_of.as_deref()).and_then(|p| app.chat(p));
    if v.incognito || info.is_some_and(|c| c.incognito) {
        head.push(Span::styled(" ◌ incognito", fg(app, t.accent)));
    }
    if let Some(p) = side {
        head.push(Span::styled(
            format!(" ↳ side chat of {} (/keep keeps it)", p.label()),
            fg(app, t.accent),
        ));
    }
    if let Some(s) = v.siblings.last() {
        let who = s.label().map(|a| format!(" {a}")).unwrap_or_default();
        head.push(Span::styled(format!(" ⎇ {}/{}{who}", s.at, s.of), fg(app, t.dim)));
    }
    if let Some(from) = info.and_then(ChatInfo::imported_from) {
        head.push(Span::styled(format!(" ⇣ {from} (read-only)"), fg(app, t.accent)));
    }
    if let Some(cmp) = info.and_then(|c| c.comparing.as_ref()) {
        head.push(Span::styled(format!(" · {}", cmp.line()), fg(app, t.working)));
    }
    if v.id.is_none() && !v.compare.is_empty() {
        head.push(Span::styled(format!(" · asks {}", v.compare.join(", ")), fg(app, t.accent)));
    }
    let hint = if v.id.is_none() {
        " C-o agent · Tab incognito · Esc back"
    } else {
        " /help · C-f find · Esc back"
    };
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
    let sel = l.sel.min(rows.len().saturating_sub(1));
    let mut scope = match (&l.workspace, l.archived.is_some()) {
        (_, true) => "archived chats".to_owned(),
        (Some(w), _) => format!("chats in {}", crate::ui::workspace_label(w)),
        (None, _) => "every chat".to_owned(),
    };
    if l.imported {
        scope.push_str(", imported only (i: all)");
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {scope} "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!("({})", rows.len()), fg(app, t.dim)),
    ])];
    let search = if l.typing || !l.query.is_empty() {
        format!(" / {}{}", l.query, if l.typing { "▏" } else { "" })
    } else {
        " / search · ⏎ open · m new · * pin · x archive · X delete · , title · A archived · i imported · I import · Tab all · Esc"
            .to_owned()
    };
    lines.push(Line::styled(search, fg(app, t.dim)));
    if rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if !l.query.is_empty() {
                "  nothing matches"
            } else if l.imported {
                "  no imported chats: I imports your agents' own histories"
            } else {
                "  no chats here yet: m starts one"
            },
            fg(app, t.dim),
        ));
    }
    let now = ptyro_core::now_ms();
    let mut last_group = "";
    let room = area.height.saturating_sub(3) as usize;
    let skip = sel.saturating_sub(room.saturating_sub(4));
    for (i, c) in rows.iter().enumerate().skip(skip) {
        let g = if c.pinned { "Pinned" } else { group(c.last_ms, now) };
        if g != last_group {
            lines.push(Line::styled(format!(" {g}"), fg(app, t.dim).add_modifier(Modifier::BOLD)));
            last_group = g;
        }
        let (glyph, gc) = glyph(app, c);
        let mut name = fg(app, t.text);
        if i == sel {
            name = name.add_modifier(Modifier::REVERSED);
        }
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{glyph} "), fg(app, gc)),
            Span::styled(format!(" {} ", c.label()), name),
            Span::styled(format!(" {} · {}", c.agent, crate::ui::ago(c.last_ms)), fg(app, t.dim)),
        ];
        if l.workspace.is_none() {
            let ws = crate::ui::workspace_label(&c.workspace);
            spans.push(Span::styled(format!(" · {ws}"), fg(app, t.dim)));
        }
        if c.unread > 0 {
            spans.push(Span::styled(" ●", fg(app, t.search)));
        }
        lines.push(Line::from(spans));
    }
    // The messages that match (SPEC §22.10), each with its snippet; ⏎ opens it there.
    if !l.hits.is_empty() {
        let n = rows.len();
        lines.push(Line::styled(
            format!(" Messages ({})", l.hits.len()),
            fg(app, t.dim).add_modifier(Modifier::BOLD),
        ));
        for (i, h) in l.hits.iter().enumerate() {
            let mut spans = vec![Span::raw("  ")];
            let name = format!(" {} ", h.chat.label());
            let style = if n + i == sel {
                fg(app, t.text).add_modifier(Modifier::REVERSED)
            } else {
                fg(app, t.text)
            };
            spans.push(Span::styled(name, style));
            if let Some(m) = &h.chat.machine {
                spans.push(Span::styled(format!("{m} "), fg(app, t.accent)));
            }
            // ≈: found by meaning (SPEC §22.10), none of the words is in it.
            if h.how == "meaning" {
                spans.push(Span::styled("≈ ", fg(app, t.search)));
            }
            spans.extend(snippet_spans(app, &h.snippet));
            lines.push(Line::from(spans));
        }
    } else if l.query.chars().count() >= 3 && l.hits_for == l.query.trim() {
        lines.push(Line::styled(" no message matches", fg(app, t.dim)));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// A snippet's text, its matches bold (the marks are control characters, never shown).
fn snippet_spans(app: &App, snippet: &str) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mut out = Vec::new();
    let mut on = false;
    for part in
        snippet.replace('\n', " ").split([ptyro_core::chats::MARK_ON, ptyro_core::chats::MARK_OFF])
    {
        if !part.is_empty() {
            let st =
                if on { fg(app, t.search).add_modifier(Modifier::BOLD) } else { fg(app, t.dim) };
            out.push(Span::styled(part.to_owned(), st));
        }
        on = !on;
    }
    out
}

/// `c:7f3k/01jb…` (or `laptop2:c:7f3k/01jb…`) citations in a text.
fn citations(text: &str) -> Vec<String> {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | '`' | '*')
    })
    .map(|w| w.trim_end_matches(['.', ':']))
    .filter(|w| {
        let body = w.rsplit_once("c:").map(|(_, b)| b).unwrap_or("");
        w.contains("c:")
            && body.split_once('/').is_some_and(|(h, n)| {
                h.len() >= 4 && n.len() >= 6 && n.chars().all(|c| c.is_ascii_alphanumeric())
            })
    })
    .map(str::to_owned)
    .collect()
}

/// `/task [PROJECT] [--agent A] [--name N]`, as `chat task`'s flags (`agent:A`, `name:N` too).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskArgs {
    pub project: Option<String>,
    /// `agent` or `agent@profile` (default: the chat's agent).
    pub agent: Option<String>,
    /// The worktree's name (default: from the chat's title).
    pub name: Option<String>,
}

impl TaskArgs {
    fn parse(text: &str) -> Result<Self, String> {
        let mut t = Self::default();
        let mut words = text.split_whitespace();
        while let Some(w) = words.next() {
            let flag = w.starts_with("--");
            let (name, inline) = match w.trim_start_matches('-').split_once(['=', ':']) {
                Some((n, v)) if flag || matches!(n, "agent" | "name") => (n, Some(v.to_owned())),
                _ => (w.trim_start_matches('-'), None),
            };
            let value = |words: &mut std::str::SplitWhitespace<'_>| {
                inline
                    .clone()
                    .or_else(|| words.next().map(str::to_owned))
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("--{name} needs a value"))
            };
            match name {
                "agent" if flag || inline.is_some() => t.agent = Some(value(&mut words)?),
                "name" if flag || inline.is_some() => t.name = Some(value(&mut words)?),
                "project" | "p" if flag => t.project = Some(value(&mut words)?),
                _ if flag => {
                    return Err(format!("{w}: /task [PROJECT] [--agent A] [--name N]"));
                }
                _ if t.project.is_none() => t.project = Some(w.to_owned()),
                _ => return Err(format!("{w}: /task [PROJECT] [--agent A] [--name N]")),
            }
        }
        Ok(t)
    }
}

/// `chat.import`'s line as `chat import`'s flags: `[claude|codex|gemini|all] [--profile P]
/// [--since 30d] [--dry-run]` (`profile:P`, `since:30d` and `dry-run` too). Empty: everything.
fn import_request(text: &str) -> Result<Request, String> {
    let (mut agent, mut profile, mut since, mut dry_run) = (None, None, None, false);
    let mut words = text.split_whitespace();
    while let Some(w) = words.next() {
        let (name, inline) = match w.split_once(['=', ':']) {
            Some((n, v)) => (n.trim_start_matches('-'), Some(v.to_owned())),
            None => (w.trim_start_matches('-'), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| words.next().map(str::to_owned))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} needs a value: --{name} …"))
        };
        match name {
            "claude" | "codex" | "gemini" | "all" if inline.is_none() => {
                agent = Some(name.to_owned())
            }
            "agent" => agent = Some(value()?),
            "profile" => profile = Some(value()?),
            "since" => since = Some(value()?),
            "dry-run" | "dry" => dry_run = true,
            _ => {
                return Err(format!(
                    "{w}: import takes claude, codex, gemini or all, --profile P, --since 30d, --dry-run"
                ));
            }
        }
    }
    Ok(Request::ChatImport { agent, profile, since, dry_run })
}

/// The list's search box as a title filter: the search's operators (`by:meaning`, `since:7d`,
/// `workspace:w`, `-word`) are the message search's, not title text; `agent:x` narrows by agent.
/// Lowercased: (the title text, the agent asked for).
fn title_query(q: &str) -> (String, Option<String>) {
    let mut agent = None;
    let mut words = Vec::new();
    for w in q.split_whitespace() {
        match w.split_once(':') {
            Some(("agent", a)) => agent = Some(a.to_lowercase()).filter(|a| !a.is_empty()),
            Some(("by" | "since" | "workspace" | "ws", _)) => {}
            _ if w.starts_with('-') && w.len() > 1 => {}
            _ => words.push(w.trim_matches('"').trim_end_matches('*').to_lowercase()),
        }
    }
    (words.join(" "), agent)
}

/// The agents a word of `/compare` names (`codex`, `codex@work`, `claude,codex@work,`), or
/// `None` when any part of it isn't one: then it is the question's.
fn agent_words(names: &[String], word: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = word.split(',').filter(|a| !a.is_empty()).collect();
    let known = |a: &&str| names.iter().any(|n| n == a.split('@').next().unwrap_or(a));
    (!parts.is_empty() && parts.iter().all(known))
        .then(|| parts.into_iter().map(str::to_owned).collect())
}

/// A node id's end, as people see it.
fn short(id: &str) -> String {
    id[id.len().saturating_sub(6)..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_are_found() {
        let t =
            "As we said (c:7f3k/01jbabcdef) and laptop2:c:a1b2/zz99887766. Not c:x/y or c:7f3k.";
        assert_eq!(citations(t), ["c:7f3k/01jbabcdef", "laptop2:c:a1b2/zz99887766"]);
    }

    #[test]
    fn a_task_takes_the_clis_flags() {
        let t = |p: Option<&str>, a: Option<&str>, n: Option<&str>| TaskArgs {
            project: p.map(Into::into),
            agent: a.map(Into::into),
            name: n.map(Into::into),
        };
        assert_eq!(TaskArgs::parse(""), Ok(t(None, None, None)));
        assert_eq!(TaskArgs::parse("web"), Ok(t(Some("web"), None, None)));
        assert_eq!(
            TaskArgs::parse("web --agent codex@work --name fix-retry"),
            Ok(t(Some("web"), Some("codex@work"), Some("fix-retry")))
        );
        assert_eq!(TaskArgs::parse("--agent=codex name:x"), Ok(t(None, Some("codex"), Some("x"))));
        assert!(TaskArgs::parse("web api").is_err());
        assert!(TaskArgs::parse("--agent").is_err());
        assert!(TaskArgs::parse("--yes").is_err());
    }

    #[test]
    fn an_import_is_narrowed_as_the_cli_does() {
        let r = |t: &str| import_request(t).map_err(|e| e.to_string());
        assert_eq!(
            r(""),
            Ok(Request::ChatImport { agent: None, profile: None, since: None, dry_run: false })
        );
        let want = Request::ChatImport {
            agent: Some("codex".into()),
            profile: Some("work".into()),
            since: Some("30d".into()),
            dry_run: true,
        };
        assert_eq!(r("codex --since 30d --profile work --dry-run"), Ok(want.clone()));
        assert_eq!(r("codex since:30d profile:work dry-run"), Ok(want.clone()));
        assert_eq!(r("--agent=codex --since=30d --profile=work --dry-run"), Ok(want));
        assert!(r("cursor").is_err());
        assert!(r("--since").is_err());
    }

    #[test]
    fn the_title_filter_leaves_the_search_operators_out() {
        assert_eq!(
            title_query("by:meaning Programming language"),
            ("programming language".into(), None)
        );
        assert_eq!(title_query("redis since:7d -budget"), ("redis".into(), None));
        assert_eq!(title_query("agent:Codex retry*"), ("retry".into(), Some("codex".into())));
        assert_eq!(title_query("by:words"), (String::new(), None));
        assert_eq!(title_query("\"a phrase\""), ("a phrase".into(), None));
    }

    #[test]
    fn compare_words_name_agents_with_commas_too() {
        let names: Vec<String> = ["claude", "codex", "gemini"].map(String::from).into();
        let w = |x: &str| agent_words(&names, x);
        assert_eq!(w("claude,codex"), Some(vec!["claude".into(), "codex".into()]));
        assert_eq!(w("codex@work,claude,"), Some(vec!["codex@work".into(), "claude".into()]));
        assert_eq!(w("gemini"), Some(vec!["gemini".into()]));
        // A word that isn't (wholly) agents is the question's.
        assert_eq!(w("claude,why"), None);
        assert_eq!(w("why"), None);
        assert_eq!(w(","), None);
    }

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
