//! Chats in the TUI (SPEC §22.12, ROADMAP CHAT1.7): the CHATS section under each workspace in
//! the sidebar, the chat list (`g`), and the chat view (`m` for a new one), which takes the place
//! of the panes beside the sidebar. The view is the structured sessions' chat view (`chat.rs`)
//! with a header and a composer of its own: ⏎ sends, Esc goes back; its other keys (and the
//! list's) are the key map's `chat` and `chatlist` tables.

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

use crate::app::{App, Confirmed, Focus, Offer, Overlay, Pending, PromptKind, Purpose, Then};
use crate::keymap::{Command, Table};
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
    /// The workspace it was opened for: where `Tab` comes back to.
    pub home: Option<String>,
    pub sel: usize,
    pub query: String,
    /// Typing into the search box.
    pub typing: bool,
    /// Archived chats instead (fetched when asked for).
    pub archived: Option<Vec<ChatInfo>>,
    /// Only chats imported from the agents' own histories (`i`; `chat ls --imported`).
    pub imported: bool,
    /// Only the chats in this state (`s` steps through them).
    pub state: StateFilter,
    /// Messages that match the query (SPEC §22.10), from every machine, best first.
    pub hits: Vec<ChatHit>,
    /// The query those hits answer (a reply to an older one is dropped).
    pub hits_for: String,
    /// Asked once whether to switch meaning search on (`by:meaning` while it is off).
    pub meaning_asked: bool,
    /// The selected chat's last messages: (its id, their lines); asked for as the selection moves.
    pub preview: Option<(String, Vec<String>)>,
}

/// The chat list's state filter (SPEC §22.12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StateFilter {
    #[default]
    All,
    NeedsYou,
    Working,
    Unread,
    Pinned,
}

impl StateFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::NeedsYou,
            Self::NeedsYou => Self::Working,
            Self::Working => Self::Unread,
            Self::Unread => Self::Pinned,
            Self::Pinned => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "",
            Self::NeedsYou => "needing you",
            Self::Working => "working",
            Self::Unread => "unread",
            Self::Pinned => "pinned",
        }
    }

    fn admits(self, c: &ChatInfo) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => c.attention.is_some() || c.status == Some(AgentStatus::Blocked),
            Self::Working => {
                matches!(c.status, Some(AgentStatus::Working | AgentStatus::Starting))
            }
            Self::Unread => c.unread > 0,
            Self::Pinned => c.pinned,
        }
    }
}

pub struct ChatViewState {
    /// `None` until a new chat is made by its first message.
    pub id: Option<String>,
    /// A new chat's own number: the reply that makes it finds it by this, wherever it went.
    pub token: u64,
    pub workspace: String,
    /// The chat as last heard (kept when it leaves the list: archived, or opened from there).
    pub info: Option<ChatInfo>,
    pub view: crate::chat::ChatView,
    pub draft: String,
    /// The composer's cursor, in characters (past the end: at the end).
    pub at: usize,
    /// A new chat's agent and preset, picked with `C-o` (or `/agent`, `/tools`).
    pub agent: Option<String>,
    pub preset: Option<String>,
    /// A new chat's folders (`/folder DIR` before the first message).
    pub folders: Vec<String>,
    /// A new chat's title (`/title TEXT` before the first message).
    pub title: Option<String>,
    /// Where Esc goes back to: the list it was opened from, or the chat a side chat is of.
    pub back: Option<Box<ChatPanel>>,
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
    /// Its transcript has arrived (pickers of its messages wait for it).
    pub loaded: bool,
    /// An action that waits for the transcript (a fork asked for from the chat's row).
    pub then: Option<&'static str>,
    /// Its machine's going offline is said once, until it is back.
    pub offline_said: bool,
    /// `C-o` in a new chat: the agent, then its tools, then a folder.
    pub choosing: bool,
}

impl ChatViewState {
    /// An empty view of `id` (`None`: a new chat), its draft as it was left.
    fn blank(id: Option<String>, workspace: String, draft: String) -> Self {
        let mut v = Self {
            id,
            token: NEXT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            workspace,
            info: None,
            view: crate::chat::ChatView::default(),
            draft: String::new(),
            at: 0,
            agent: None,
            preset: None,
            folders: Vec::new(),
            title: None,
            back: None,
            sending: false,
            incognito: false,
            siblings: Vec::new(),
            found: Vec::new(),
            found_at: 0,
            focus: None,
            compare: Vec::new(),
            loaded: false,
            then: None,
            offline_said: false,
            choosing: false,
        };
        v.set_draft(draft);
        v
    }

    /// Edit the draft at its cursor, which the view then draws there.
    fn edit<R>(&mut self, f: impl FnOnce(&mut crate::settings::LineEdit) -> R) -> R {
        let mut e =
            crate::settings::LineEdit { text: std::mem::take(&mut self.draft), at: self.at };
        e.at = e.at.min(e.text.chars().count());
        let r = f(&mut e);
        (self.draft, self.at) = (e.text, e.at);
        self.view.set_draft(self.draft.clone(), Some(self.at));
        r
    }

    /// The draft replaced as a whole: the cursor goes to its end.
    fn set_draft(&mut self, text: String) {
        self.draft = text;
        self.at = usize::MAX;
        self.view.set_draft(self.draft.clone(), None);
    }
}

/// New chats' tokens (see [`ChatViewState::token`]).
static NEXT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

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

impl ChatList {
    fn new(workspace: Option<String>) -> Self {
        Self {
            home: workspace.clone(),
            workspace,
            sel: 0,
            query: String::new(),
            typing: false,
            archived: None,
            imported: false,
            state: StateFilter::All,
            hits: Vec::new(),
            hits_for: String::new(),
            meaning_asked: false,
            preview: None,
        }
    }
}

/// A chat held by a panel (the one shown, the one Esc goes back to, the list's archived ones).
fn held<'a>(p: &'a ChatPanel, id: &str) -> Option<&'a ChatInfo> {
    match p {
        ChatPanel::View(v) if v.id.as_deref() == Some(id) && v.info.is_some() => v.info.as_ref(),
        ChatPanel::View(v) => v.back.as_deref().and_then(|b| held(b, id)),
        ChatPanel::List(l) => l.archived.iter().flatten().find(|c| c.id == id),
    }
}

/// What a panel holds of `info`'s chat, brought up to date.
fn refresh_held(p: &mut ChatPanel, info: &ChatInfo) {
    match p {
        ChatPanel::View(v) => {
            if v.id.as_deref() == Some(info.id.as_str()) {
                v.info = Some(info.clone());
            }
            if let Some(b) = &mut v.back {
                refresh_held(b, info);
            }
        }
        ChatPanel::List(l) => {
            if let Some(a) = &mut l.archived {
                if info.archived {
                    if let Some(c) = a.iter_mut().find(|c| c.id == info.id) {
                        *c = info.clone();
                    }
                } else {
                    a.retain(|c| c.id != info.id);
                }
            }
        }
    }
}

/// A message that didn't go, in front of what was typed since.
fn put_back(failed: String, now: &str) -> String {
    if now.trim().is_empty() { failed } else { format!("{failed}\n{now}") }
}

/// Where `chat.export` writes: Downloads where there is one, else the home folder.
fn export_dir() -> std::path::PathBuf {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(h) if h.join("Downloads").is_dir() => h.join("Downloads"),
        Some(h) => h,
        None => std::env::temp_dir(),
    }
}

fn working(c: &ChatInfo) -> bool {
    matches!(c.status, Some(AgentStatus::Starting | AgentStatus::Working | AgentStatus::Blocked))
}

/// The composer's slash commands (`/help` lists them; picking one starts it in the composer).
const SLASH: &[(&str, &str)] = &[
    ("/retry", "ask again: the old answer stays as a branch"),
    ("/edit", "say your last message differently (/edit TEXT)"),
    ("/compare", "several agents answer (/compare AGENTS… [question])"),
    ("/pick", "keep the answer shown: its agent takes the next message"),
    ("/fork", "a new chat from here or from any message"),
    ("/side", "ask about this chat without changing it (/side QUESTION)"),
    ("/keep", "keep this side chat as a chat of its own"),
    ("/agent", "switch agent (/agent NAME); a new chat: its agent"),
    ("/tools", "what it may use (/tools NAME)"),
    ("/folder", "a folder it may read (/folder DIR, /folder none)"),
    ("/task", "an agent carries it out in a new worktree ([PROJECT] [--agent A] [--name N])"),
    ("/title", "its title (/title TEXT)"),
    ("/search", "search every chat's messages (/search WORDS)"),
    ("/find", "ask your chats a question (/find QUESTION)"),
    ("/export", "write it out as Markdown"),
    ("/incognito", "a new chat that keeps nothing"),
    ("/branch", "show another branch"),
    ("/cite", "open a citation of its answers"),
];

impl App {
    pub(crate) fn chats_on(&self) -> bool {
        self.config.chat.enabled
    }

    /// Chats are off: offered to be switched on, as the settings panel would.
    fn chats_off_toast(&mut self) {
        self.toast_offer(
            "chats are off",
            false,
            Offer {
                label: "switch them on".into(),
                then: Then::Set {
                    key: "chat.enabled".into(),
                    value: "true".into(),
                    done: "chats are on".into(),
                },
            },
        );
    }

    /// No chat to act on: offer the list of them.
    fn no_chat_toast(&mut self) {
        self.toast_offer(
            "no chat selected",
            false,
            Offer {
                label: "list the chats".into(),
                then: Then::Run(Command { action: "chat.list", arg: None }),
            },
        );
    }

    /// Whether a chat's preset reads its folders, where this TUI can tell: `None` for another
    /// machine's chat, or a preset this config doesn't define (that machine's config says).
    pub(crate) fn chat_reads(&self, c: &ChatInfo) -> Option<bool> {
        if c.machine.is_some() {
            return None;
        }
        self.config.chat.preset(&c.tools).map(|t| t.read)
    }

    /// A chat by id: listed, or held by the chat panel (archived, or opened from the archive).
    pub(crate) fn chat(&self, id: &str) -> Option<&ChatInfo> {
        self.chat_list.iter().find(|c| c.id == id).or_else(|| {
            [&self.chat_panel, &self.chat_aside].into_iter().flatten().find_map(|p| held(p, id))
        })
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
        // The chat shown (or put aside, or gone back to) keeps it, archived or not.
        for p in [&mut self.chat_panel, &mut self.chat_aside].into_iter().flatten() {
            refresh_held(p, &info);
        }
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

    /// A chat left the lists: deleted, or (`machine`) no longer shown from that machine.
    pub(crate) fn chat_removed(&mut self, id: &str, machine: Option<&str>) {
        let ws = self.chat(id).map(|c| c.workspace.clone());
        self.chat_list.retain(|c| c.id != id);
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel
            && let Some(a) = &mut l.archived
        {
            a.retain(|c| c.id != id);
        }
        self.chat_drafts.remove(id);
        if matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id))
            && let Some(ChatPanel::View(v)) = self.chat_panel.take()
        {
            self.send(ClientMsg::ChatUnsubscribe { chat: id.to_owned() });
            match v.back {
                Some(b) => self.chat_back(*b),
                None => self.chat_panel = None,
            }
            let hidden = machine.zip(ws.as_deref()).is_some_and(|(m, ws)| {
                let name = crate::tree::machine_and_name(ws).1;
                self.machines.iter().any(|x| x.name == m && x.hidden.iter().any(|h| h == name))
            });
            self.toast(match machine {
                None => "that chat was deleted".to_owned(),
                Some(m) if hidden => format!("that chat was hidden with its workspace on {m}"),
                Some(m) => format!(
                    "that chat isn't shown here any more: its workspace on {m} was hidden, or it was archived or deleted there"
                ),
            });
        }
        if matches!(&self.chat_aside, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id)) {
            self.chat_aside = None;
        }
        self.dirty = true;
    }

    /// The open chat's machine came back (follow it again) or went away (say so, once).
    /// `self.machines` still has what was known before.
    pub(crate) fn chat_machine_news(&mut self, m: &ptyro_core::model::MachineInfo) {
        use ptyro_core::model::MachineState;
        let before = self.machines.iter().find(|x| x.name == m.name).map(|x| x.state);
        let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return };
        let Some(id) = v.id.clone() else { return };
        let on = |c: &ChatInfo| c.id == id && c.machine.as_deref() == Some(m.name.as_str());
        if !(v.info.as_ref().is_some_and(on) || self.chat_list.iter().any(on)) {
            return;
        }
        match m.state {
            MachineState::Online => {
                v.offline_said = false;
                // Back from being away (not one more word that it is up): followed afresh.
                if before.is_some_and(|b| b != MachineState::Online) {
                    self.send(ClientMsg::ChatUnsubscribe { chat: id.clone() });
                    self.send(ClientMsg::ChatSubscribe { chat: id });
                }
            }
            MachineState::Offline if !v.offline_said => {
                v.offline_said = true;
                let text = format!("{} went offline; this chat carries on when it is back", m.name);
                let item = ChatItem::new(
                    format!("offline-{}-{}", ptyro_core::now_ms(), v.view.items.len()),
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
        let mut then = None;
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.as_deref() == Some(chat.as_str())
        {
            let first = !v.loaded;
            let draft = v.draft.clone();
            v.view.apply(rev, reset, items, draft);
            if reset {
                v.loaded = true;
                v.view.set_marks(siblings.iter().map(|s| {
                    (s.node.clone(), (s.at, s.of, s.label().unwrap_or_default().to_owned()))
                }));
                v.siblings = siblings;
                // Another branch: find-in-chat's matches are this one's.
                if let Some(q) = v.view.highlight.clone() {
                    v.found = v.view.find(&q);
                    v.found_at = 0;
                    // Asked before the transcript was here: its first match is shown now.
                    if first
                        && v.focus.is_none()
                        && let Some(n) = v.found.first()
                    {
                        v.view.focus_on(n);
                    }
                }
                then = v.then.take();
            }
            if let Some(n) = v.focus.clone()
                && v.view.items.iter().any(|i| i.id == n)
            {
                v.view.focus_on(&n);
                v.focus = None;
            }
            self.dirty = true;
        }
        if let Some(a) = then {
            self.chat_action(a);
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

    /// The composer's hints, with the keys `[keys]` gives the chat now.
    fn dress(&self, v: &mut ChatViewState) {
        let k = |a: &str| self.keymap.key_in(Table::Chat, a);
        let card: Vec<(String, &str)> =
            [("chat.allow", "allow"), ("chat.always", "always"), ("chat.deny", "deny")]
                .into_iter()
                .filter_map(|(a, w)| k(a).map(|k| (k, w)))
                .collect();
        let letters: Vec<&str> = card.iter().map(|(k, _)| k.as_str()).collect();
        v.view.hints = Some(crate::chat::Hints {
            card: card.iter().map(|(k, w)| format!("{k} {w}")).collect::<Vec<_>>().join(" · "),
            card_behind: format!("(Esc clears the draft, then {})", letters.join("/")),
            empty: match k("chat.stop") {
                Some(s) => format!("type a message · ⏎ sends · {s} stops an answer"),
                None => "type a message · ⏎ sends".into(),
            },
            branches: [k("chat.branch_prev"), k("chat.branch_next")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
        });
    }

    /// The keys changed: the open chats' hints say the new ones.
    pub(crate) fn redress_chats(&mut self) {
        for slot in [0, 1] {
            let p = if slot == 0 { self.chat_panel.take() } else { self.chat_aside.take() };
            let p = p.map(|p| match p {
                ChatPanel::View(mut v) => {
                    self.dress(&mut v);
                    v.view.apply(v.view.rev, false, Vec::new(), v.draft.clone());
                    ChatPanel::View(v)
                }
                l => l,
            });
            if slot == 0 {
                self.chat_panel = p;
            } else {
                self.chat_aside = p;
            }
        }
    }

    /// `chat.new`: an empty chat view in the workspace; its first message makes the chat.
    pub(crate) fn new_chat(&mut self) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        let workspace = self.chat_workspace();
        self.chat_takes_keys();
        self.dirty = true;
        // A new chat already shown for that workspace stays as it is (its agent, its draft).
        if matches!(&self.chat_panel, Some(ChatPanel::View(v))
            if v.id.is_none() && !v.sending && v.view.items.is_empty() && v.workspace == workspace)
        {
            return;
        }
        let back = match self.chat_panel.take() {
            Some(ChatPanel::List(l)) => Some(Box::new(ChatPanel::List(l))),
            other => {
                self.let_go(other);
                None
            }
        };
        let draft = self.chat_drafts.remove("").unwrap_or_default();
        let mut v = ChatViewState::blank(None, workspace, draft);
        v.incognito = self.config.chat.incognito;
        v.back = back;
        self.dress(&mut v);
        self.chat_panel = Some(ChatPanel::View(Box::new(v)));
    }

    /// Open a chat in the view, following it; its draft is as it was left.
    pub(crate) fn open_chat(&mut self, id: &str) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        let info = self.chat(id).cloned();
        let side_of = info.as_ref().and_then(|c| c.side_of.clone());
        let mut same = None;
        let back = match self.chat_panel.take() {
            Some(ChatPanel::List(l)) => Some(Box::new(ChatPanel::List(l))),
            // The chat already shown (a hit or citation in it, its menu's open): followed
            // again, as it is.
            Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id) => {
                same = Some(v);
                None
            }
            // A side chat opened from its chat: Esc goes back there.
            Some(ChatPanel::View(v)) if side_of.is_some() && v.id == side_of => {
                if let Some(p) = &v.id {
                    self.send(ClientMsg::ChatUnsubscribe { chat: p.clone() });
                }
                Some(Box::new(ChatPanel::View(v)))
            }
            other => {
                self.let_go(other);
                None
            }
        };
        let v = match same {
            Some(mut v) => {
                self.send(ClientMsg::ChatUnsubscribe { chat: id.to_owned() });
                v.focus = None;
                v
            }
            None => {
                let draft = self.chat_drafts.remove(id).unwrap_or_default();
                let workspace =
                    info.as_ref().map_or_else(|| self.workspace.clone(), |c| c.workspace.clone());
                let mut v = ChatViewState::blank(Some(id.to_owned()), workspace, draft);
                v.info = info;
                v.back = back;
                self.dress(&mut v);
                Box::new(v)
            }
        };
        self.send(ClientMsg::ChatSubscribe { chat: id.to_owned() });
        self.chat_takes_keys();
        self.chat_panel = Some(ChatPanel::View(v));
        self.dirty = true;
    }

    /// `chat.list`: the workspace's chats (Tab: every workspace's).
    pub(crate) fn open_chat_list(&mut self, workspace: Option<String>) {
        if !self.chats_on() {
            return self.chats_off_toast();
        }
        self.leave_chat();
        self.chat_takes_keys();
        self.chat_panel = Some(ChatPanel::List(ChatList::new(workspace)));
        self.preview_selected();
        self.dirty = true;
    }

    /// A chat (or the list) just opened has the keyboard, wherever the keys were: typing goes
    /// into it, not to the sidebar it was opened from (as a session shown from there does).
    fn chat_takes_keys(&mut self) {
        self.focus = Focus::Pane;
        self.sidebar_zoom = false;
    }

    /// Stop following the chat shown, if one is (a side chat not kept goes with it); its draft
    /// waits for it to open again.
    fn leave_chat(&mut self) {
        let p = self.chat_panel.take();
        self.let_go(p);
    }

    /// A panel no longer shown: each chat in it (and in what Esc would go back to) left.
    fn let_go(&mut self, p: Option<ChatPanel>) {
        if let Some(ChatPanel::View(v)) = p {
            let v = *v;
            self.stash(&v);
            if let Some(id) = &v.id {
                self.left_chat(id);
            }
            self.let_go(v.back.map(|b| *b));
        }
    }

    /// Keep a view's draft for when its chat opens again (`""`: a new chat's).
    fn stash(&mut self, v: &ChatViewState) {
        let key = v.id.clone().unwrap_or_default();
        if v.draft.trim().is_empty() {
            self.chat_drafts.remove(&key);
        } else {
            self.chat_drafts.insert(key, v.draft.clone());
        }
    }

    /// Leave the chat shown for where it was opened from: the list, or the chat a side chat
    /// is of; else nothing.
    fn leave_view(&mut self, v: ChatViewState) {
        self.stash(&v);
        if let Some(id) = &v.id {
            self.left_chat(id);
        }
        let parent = v.info.as_ref().and_then(|c| c.side_of.clone());
        match (v.back, parent) {
            (Some(b), _) => self.chat_back(*b),
            (None, Some(p)) => self.open_chat(&p),
            (None, None) => self.chat_panel = None,
        }
        self.dirty = true;
    }

    /// `p` shown again; a chat in it followed afresh.
    fn chat_back(&mut self, p: ChatPanel) {
        if let ChatPanel::View(v) = &p
            && let Some(id) = &v.id
        {
            self.send(ClientMsg::ChatUnsubscribe { chat: id.clone() });
            self.send(ClientMsg::ChatSubscribe { chat: id.clone() });
        }
        self.chat_panel = Some(p);
        self.preview_selected();
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

    /// Put the chat (or the list) shown aside, as it is (its draft too), for `session.last`. It
    /// stays followed, so the daemon doesn't take a side chat put aside for one nobody has.
    pub(crate) fn set_chat_aside(&mut self) {
        self.drop_chat_aside();
        self.chat_aside = self.chat_panel.take();
        self.dirty = true;
    }

    /// No way back to the chat put aside: it is left (a side chat goes, unless kept).
    pub(crate) fn drop_chat_aside(&mut self) {
        let p = self.chat_aside.take();
        self.let_go(p);
    }

    /// The chat put aside, shown again as it was; false when there is none. Followed afresh:
    /// what it said meanwhile went by unseen.
    pub(crate) fn bring_chat_back(&mut self) -> bool {
        let Some(p) = self.chat_aside.take() else { return false };
        self.chat_takes_keys();
        self.chat_back(p);
        self.dirty = true;
        true
    }

    pub(crate) fn close_chat(&mut self) {
        self.leave_chat();
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
            Some(ChatPanel::List(l)) => return self.list_target(l),
            None => {}
        }
        None
    }

    /// The list's selected chat: a chat row's, or a message hit's.
    fn list_target(&self, l: &ChatList) -> Option<String> {
        let rows = self.list_rows(l);
        match rows.get(l.sel) {
            Some(c) => Some(c.id.clone()),
            None => match l.hits.get(l.sel - rows.len()) {
                Some(h) => Some(h.chat.id.clone()),
                None => rows.last().map(|c| c.id.clone()),
            },
        }
    }

    /// What the chat panel shows, to tell later whether it still does (a slow reply).
    pub(crate) fn chat_shown_key(&self) -> String {
        match &self.chat_panel {
            Some(ChatPanel::View(v)) => match &v.id {
                Some(id) => format!("c:{id}"),
                None => format!("n:{}", v.token),
            },
            Some(ChatPanel::List(_)) => "list".into(),
            None => String::new(),
        }
    }

    /// A chat made or found for the person (a fork, a side chat, an answer to `/find`): shown
    /// at `node` if what asked for it (`origin`) still is, else offered, so it never takes the
    /// place of what they went on to; true when shown.
    pub(crate) fn chat_ready(&mut self, info: ChatInfo, origin: &str, node: Option<&str>) -> bool {
        let id = info.id.clone();
        let label = info.label().to_owned();
        self.chat_updated(info);
        if self.chat_shown_key() == origin {
            self.open_chat_at(&id, node);
            return true;
        }
        self.toast_offer(
            format!("{label} is ready"),
            false,
            Offer {
                label: "open it".into(),
                then: Then::Run(Command { action: "chat.open", arg: Some(id) }),
            },
        );
        false
    }

    // ── actions ──────────────────────────────────────────────────────────────────────────

    /// A `chat.*` action from a key, the palette or a row menu.
    pub(crate) fn chat_action(&mut self, action: &str) {
        match action {
            "chat.new" => return self.new_chat(),
            "chat.incognito" => {
                // A new chat before its first message: flipped; else a new one that keeps nothing.
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                    && v.id.is_none()
                {
                    v.incognito = !v.incognito;
                    let on = v.incognito;
                    return self.toast(if on {
                        "incognito: nothing of this chat is kept, and it ends with its agent"
                    } else {
                        "kept: this chat is saved and searchable"
                    });
                }
                self.new_chat();
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                    && v.id.is_none()
                {
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
            "chat.list_search" | "chat.list_all" | "chat.list_archived" | "chat.list_imported"
            | "chat.list_state" => return self.chat_list_action(action),
            "chat.allow" | "chat.always" | "chat.deny" | "chat.branch_prev"
            | "chat.branch_next" | "chat.page_up" | "chat.page_down" => {
                return self.chat_view_action(action);
            }
            _ => {}
        }
        let Some(id) = self.chat_target() else { return self.no_chat_target(action) };
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
            confirmed: false,
        };
        // A picker of the chat's messages (or branches) needs them: the chat opens first.
        let wants_transcript =
            matches!(action, "chat.edit" | "chat.fork" | "chat.branch" | "chat.cite");
        let loaded = matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id.as_str()) && v.loaded);
        let imported = info.as_ref().is_some_and(|c| c.source.is_some());
        if wants_transcript && !loaded && !(imported && action == "chat.edit") {
            self.open_chat(&id);
            if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                v.then = crate::keymap::spec(action).map(|s| s.id);
            }
            return;
        }
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
                // Which way is the chat's own: an archived one comes back.
                let archived = info.as_ref().map_or_else(|| self.archived_shown(), |c| c.archived);
                let msg = if archived { "back from the archive" } else { "archived" };
                self.request(Pending::Done(msg.into()), update(None, Some(!archived)));
                if let Some(ChatPanel::List(l)) = &mut self.chat_panel
                    && let Some(a) = &mut l.archived
                {
                    a.retain(|c| c.id != id);
                }
                // Only that chat's view goes, back to where it was opened from.
                if matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id.as_str()))
                    && let Some(ChatPanel::View(v)) = self.chat_panel.take()
                {
                    self.leave_view(*v);
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
            "chat.folder" => self.ask_chat_folder(Some(id)),
            "chat.task" => self.chat_task(id, TaskArgs::default()),
            "chat.export" => {
                let name = format!(
                    "ptyro-chat-{}.md",
                    info.as_ref().map_or(id.as_str(), |c| c.handle.as_str())
                );
                self.request(
                    Pending::ChatExport(export_dir().join(name)),
                    Request::ChatExport { chat: id, format: "md".into() },
                );
            }
            "chat.stop" => {
                // Nothing to stop: the key clears the draft instead.
                let idle = !info.as_ref().is_some_and(working);
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                    && v.id.as_deref() == Some(id.as_str())
                    && idle
                    && !v.draft.is_empty()
                {
                    return v.set_draft(String::new());
                }
                self.request(Pending::Done("stopped".into()), Request::ChatStop { chat: id })
            }
            "chat.retry" | "chat.edit" if imported => self.toast(IMPORTED_AS_WRITTEN),
            "chat.retry" => self.request(
                Pending::Done("asked again: the old answer stays as a branch".into()),
                Request::ChatRetry { chat: id, node: None },
            ),
            "chat.edit" => self.pick_message(Purpose::ChatEditPick, "Edit which message?"),
            "chat.fork" => self.pick_message(Purpose::ChatForkAt, "Fork from where?"),
            "chat.side" => self.request(
                Pending::ChatOpen(self.chat_shown_key()),
                Request::ChatFork {
                    chat: id,
                    node: None,
                    agent: None,
                    side: true,
                    confirmed: false,
                },
            ),
            "chat.keep" => {
                if info.as_ref().is_some_and(|c| c.side_of.is_some()) {
                    // Kept at once here, so leaving before the daemon's word doesn't drop it.
                    if let Some(c) = self.chat_list.iter_mut().find(|c| c.id == id) {
                        c.side_of = None;
                    }
                    if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                        && let Some(c) = &mut v.info
                        && c.id == id
                    {
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
                            confirmed: false,
                        },
                    );
                } else {
                    self.toast("not a side chat: nothing to keep");
                }
            }
            "chat.branch" => self.pick_branch(),
            "chat.compare" => {
                if imported {
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
                // Again, with matches shown: the next one.
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                    && v.id.as_deref() == Some(id.as_str())
                    && !v.found.is_empty()
                {
                    v.found_at = (v.found_at + 1) % v.found.len();
                    let n = v.found[v.found_at].clone();
                    v.view.focus_on(&n);
                    let msg = format!("{}/{}", v.found_at + 1, v.found.len());
                    return self.toast(msg);
                }
                if !matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id.as_str()))
                {
                    self.open_chat(&id);
                }
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Find in {label}"),
                    text: String::new(),
                    kind: PromptKind::ChatFindIn(id),
                });
            }
            "chat.cite" => self.pick_citation(),
            _ => {}
        }
    }

    /// An action with no chat to act on: a new chat's own choices, or the list to pick one.
    fn no_chat_target(&mut self, action: &str) {
        let new = matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.is_none());
        match action {
            "chat.agent" if new => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.choosing = true;
                }
                self.pick_chat_agent(None)
            }
            "chat.preset" if new => self.pick_chat_preset(None),
            "chat.folder" if new => self.ask_chat_folder(None),
            "chat.stop" if new => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.set_draft(String::new());
                }
            }
            _ if new => self.toast("send a first message: then this works on the chat"),
            _ => self.no_chat_toast(),
        }
    }

    /// The chat view's own actions: a permission card answered, branches flipped, scrolling.
    fn chat_view_action(&mut self, action: &str) {
        let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return self.no_chat_toast() };
        match action {
            "chat.page_up" | "chat.page_down" => {
                let page = v.view.body_rows.get().saturating_sub(2).max(1) as isize;
                v.view.scroll_by(if action == "chat.page_up" { page } else { -page });
            }
            "chat.allow" | "chat.always" | "chat.deny" => {
                let Some(id) = v.id.clone().filter(|_| v.view.pending_permission()) else {
                    return self.toast("nothing to answer: no permission card is waiting");
                };
                let d = action.trim_start_matches("chat.").to_owned();
                self.request(
                    Pending::Done(String::new()),
                    Request::ChatAnswer { chat: id, decision: d },
                );
            }
            _ => {
                // The newest branch point's other answers (or messages), in turn.
                let (Some(id), Some(s)) = (v.id.clone(), v.siblings.last().cloned()) else {
                    return self.toast("one branch only: retry an answer (/retry) or edit a message (/edit) to make another");
                };
                let at = s.at as usize - 1;
                let n = s.ids.len();
                let to = if action == "chat.branch_prev" { (at + n - 1) % n } else { (at + 1) % n };
                self.request(
                    Pending::Done(String::new()),
                    Request::ChatBranch { chat: id, to: s.ids[to].clone() },
                );
            }
        }
        self.dirty = true;
    }

    /// The chat list's own actions (the list opens first when it isn't shown).
    fn chat_list_action(&mut self, action: &str) {
        if !matches!(self.chat_panel, Some(ChatPanel::List(_))) {
            self.open_chat_list(Some(self.chat_workspace()));
        }
        let here = self.workspace.clone();
        let Some(ChatPanel::List(l)) = &mut self.chat_panel else { return };
        let (mut rescope, mut fetch, mut say) = (false, false, None);
        match action {
            "chat.list_search" => l.typing = true,
            "chat.list_all" => {
                l.workspace = match l.workspace {
                    Some(_) => None,
                    None => Some(l.home.clone().unwrap_or(here)),
                };
                rescope = true;
            }
            "chat.list_archived" => {
                if l.archived.take().is_none() {
                    l.archived = Some(Vec::new());
                    fetch = true;
                }
                rescope = true;
            }
            "chat.list_imported" => l.imported = !l.imported,
            _ => {
                l.state = l.state.next();
                say = Some(match l.state {
                    StateFilter::All => "every chat".to_owned(),
                    s => format!("only the chats {}", s.label()),
                });
            }
        }
        l.sel = 0;
        if fetch {
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
        if rescope {
            self.search_chats();
        }
        if let Some(s) = say {
            self.toast(s);
        }
        self.preview_selected();
        self.dirty = true;
    }

    /// The person's messages of the shown branch, newest first, to pick one (`purpose`).
    /// (The view's own chat, which the caller opened: its messages are the ones offered.)
    fn pick_message(&mut self, purpose: fn(String) -> Purpose, title: &str) {
        let Some(ChatPanel::View(v)) = &self.chat_panel else { return };
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
        let Some(ChatPanel::View(v)) = &self.chat_panel else { return };
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

    /// Find `text` in `chat` (opened when it isn't shown): highlight it and show the first
    /// message with it (once the transcript is here).
    pub(crate) fn find_in_chat(&mut self, chat: &str, text: &str) {
        if !matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.as_deref() == Some(chat)) {
            self.open_chat(chat);
        }
        let next = self.keymap.key_in(Table::Chat, "chat.find_in");
        let Some(ChatPanel::View(v)) = &mut self.chat_panel else { return };
        v.found = v.view.find(text);
        v.found_at = 0;
        v.view.highlight = Some(text.to_owned()).filter(|t| !t.is_empty());
        if text.is_empty() {
            return;
        }
        match v.found.first().cloned() {
            Some(n) => {
                v.view.focus_on(&n);
                let next = next.map(|k| format!(" · {k} next")).unwrap_or_default();
                let msg = format!("1/{}{next} · Esc clears", v.found.len());
                self.toast(msg);
            }
            None if !v.loaded => self.toast(format!("finding {text:?} as the chat arrives")),
            None => self.toast_offer(
                format!("{text:?}: not in this chat"),
                false,
                Offer {
                    label: "search every chat".into(),
                    then: Then::Run(Command { action: "chat.search", arg: Some(text.to_owned()) }),
                },
            ),
        }
    }

    /// What a picker of ours picked.
    pub(crate) fn chat_pick(&mut self, purpose: Purpose, value: String) {
        match purpose {
            Purpose::ChatForkAt(chat) => self.request(
                Pending::ChatOpen(self.chat_shown_key()),
                Request::ChatFork {
                    chat,
                    node: Some(value).filter(|v| !v.is_empty()),
                    agent: None,
                    side: false,
                    confirmed: false,
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
                        Pending::ChatOpenAt(node, self.chat_shown_key()),
                        Request::ChatRead {
                            chat: chat.to_owned(),
                            all: false,
                            from: None,
                            limit: None,
                        },
                    ),
                }
            }
            Purpose::ChatSlash => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.set_draft(format!("{value} "));
                }
            }
            _ => {}
        }
    }

    /// `/help`: the commands, to pick one into the composer.
    fn slash_help(&mut self) {
        let items = SLASH
            .iter()
            .map(|(c, d)| Item {
                label: (*c).into(),
                detail: (*d).into(),
                key: None,
                target: Target::Chat((*c).into()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(
                "Chat commands (⏎ puts one in the composer; // sends a /)",
                items,
            ),
            purpose: Purpose::ChatSlash,
        });
    }

    /// A slash command typed into the composer (SPEC §22.12); `false`: not one.
    fn chat_slash(&mut self, chat: Option<String>, text: &str) -> bool {
        let Some(cmd) = text.trim().strip_prefix('/') else { return false };
        let (name, arg) = cmd.split_once(' ').map_or((cmd, ""), |(a, b)| (a, b.trim()));
        let need = |me: &mut Self| {
            me.toast("send a first message, then /commands work on the chat");
        };
        match (name, chat) {
            ("help" | "?", _) => self.slash_help(),
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
                    Pending::ChatOpen(self.chat_shown_key()),
                    Request::ChatFind { question: arg.to_owned(), agent: None, workspace: None },
                );
            }
            ("find", _) => self.chat_action("chat.find"),
            ("tools", None) if arg.is_empty() => self.pick_chat_preset(None),
            ("tools", None) => self.chat_picked(arg.to_owned(), false, None),
            ("folder" | "folders", c) if !arg.is_empty() => self.chat_folder(c, arg),
            ("folder" | "folders", None) => self.ask_chat_folder(None),
            ("folder" | "folders", Some(_)) => self.chat_action("chat.folder"),
            ("compare", c) => self.chat_compare_slash(c, arg),
            // A new chat's agent and title go with its first message.
            ("agent", None) if arg.is_empty() => self.pick_chat_agent(None),
            ("agent", None) => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.agent = Some(arg.to_owned());
                }
                self.toast(format!("{arg} answers the first message"));
            }
            ("title", None) if arg.is_empty() => {
                self.toast("/title TEXT: the new chat's title (else one is made from it)")
            }
            ("title", None) => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.title = Some(arg.to_owned());
                }
                self.toast(format!("titled {arg:?} once its first message goes"));
            }
            // A command of a chat's own, before there is one; anything else falls to the end.
            (
                "pick" | "task" | "retry" | "edit" | "fork" | "side" | "keep" | "branch" | "cite"
                | "export",
                None,
            ) => need(self),
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
                Pending::ChatSideAsk(arg.to_owned(), self.chat_shown_key()),
                Request::ChatFork {
                    chat: c,
                    node: None,
                    agent: None,
                    side: true,
                    confirmed: false,
                },
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
                    confirmed: false,
                },
            ),
            // Not lost: back in the composer, to fix or to send as a message.
            (other, _) => {
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    v.set_draft(text.to_owned());
                }
                self.toast(format!(
                    "/{other}: not a command (/help lists them; start with // to send it as a message)"
                ));
            }
        }
        true
    }

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
                v.set_draft(text);
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
        let k = |a: &str| self.keymap.key_in(Table::Chat, a);
        let flips = match (k("chat.branch_prev"), k("chat.branch_next")) {
            (Some(p), Some(n)) => format!("{p}/{n} flips"),
            _ => "/branch shows another".into(),
        };
        let done = Pending::Done(format!(
            "asking {who}: each answer is a branch ({flips}, /pick keeps one)"
        ));
        let req = Request::ChatCompare { chat, agents, text, confirmed: false };
        self.request(Pending::Confirm(Box::new(req.clone()), Box::new(done)), req);
    }

    /// The reply to a message sent to `sent_to`: when it names another chat, the message went
    /// to a fork (an imported chat carries on as one). The view goes there if it still shows
    /// the chat the message went to, with what was typed meanwhile.
    pub(crate) fn chat_forked(&mut self, sent_to: &str, info: ChatInfo) {
        if info.id == sent_to {
            return;
        }
        let id = info.id.clone();
        let label = info.label().to_owned();
        self.chat_updated(info);
        if let Some(ChatPanel::View(v)) = &mut self.chat_panel
            && v.id.as_deref() == Some(sent_to)
        {
            let draft = std::mem::take(&mut v.draft);
            self.open_chat(&id);
            if let Some(ChatPanel::View(v)) = &mut self.chat_panel
                && v.draft.is_empty()
            {
                v.set_draft(draft);
            }
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

    /// The list's selected chat's last messages, asked for when the selection moved to it.
    pub(crate) fn preview_selected(&mut self) {
        let Some(ChatPanel::List(l)) = &self.chat_panel else { return };
        let Some(id) = self.list_target(l) else { return };
        if l.preview.as_ref().is_some_and(|(p, _)| *p == id) {
            return;
        }
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
            l.preview = Some((id.clone(), Vec::new()));
        }
        let req = Request::ChatRead { chat: id.clone(), all: false, from: None, limit: None };
        self.request(Pending::ChatPreview(id), req);
    }

    /// A chat's transcript for the list's preview: its last question and answer.
    pub(crate) fn got_preview(&mut self, id: &str, nodes: Vec<ptyro_core::chats::ChatNode>) {
        let Some(ChatPanel::List(l)) = &mut self.chat_panel else { return };
        let Some((p, lines)) = &mut l.preview else { return };
        if p != id {
            return;
        }
        let mut out = Vec::new();
        for n in nodes.iter().rev() {
            let line = match &n.body {
                ChatBody::User { text } => {
                    format!("› {}", ptyro_core::chats::first_line_title(text))
                }
                ChatBody::Assistant { text, .. } => {
                    text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().to_owned()
                }
                _ => continue,
            };
            out.insert(0, line);
            if out.len() == 2 {
                break;
            }
        }
        *lines = out;
        self.dirty = true;
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
        self.set_config(
            "chat.meaning",
            "true",
            "meaning search is on: its model is downloading (ptyro doctor shows how far)",
        );
    }

    /// Set `key` in the config file, as the settings panel would, and read the file again.
    pub(crate) fn set_config(&mut self, key: &str, value: &str, done: &str) {
        let layer =
            ptyro_core::settings::Layer::User(ptyro_core::config::Config::path(&self.paths));
        let op = ptyro_core::settings::Op::Set { key: key.into(), value: value.into() };
        let lk = ptyro_core::settings::Lookups::default();
        match ptyro_core::settings::apply(&layer, &op, &lk, &|_| Ok(())) {
            Ok(_) => {
                self.toast(done);
                self.reload_config_with(false);
            }
            Err(e) => self.toast_err(format!("{key}: {e}")),
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

    /// Ask for a folder the chat (`None`: the new chat being written) may read.
    fn ask_chat_folder(&mut self, id: Option<String>) {
        let now = match &id {
            Some(c) => self.chat(c).map(|c| c.folders.join(", ")).unwrap_or_default(),
            None => match &self.chat_panel {
                Some(ChatPanel::View(v)) => v.folders.join(", "),
                _ => String::new(),
            },
        };
        let what = if id.is_some() { "this chat" } else { "the new chat (empty: none for now)" };
        self.overlay = Some(Overlay::Prompt {
            title: format!(
                "A folder {what} may read (none: no folders){}",
                if now.is_empty() { String::new() } else { format!("; now {now}") }
            ),
            text: String::new(),
            kind: PromptKind::ChatFolder(id),
        });
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
                confirmed: false,
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
            return self.toast_offer(
                "a task needs a project",
                false,
                Offer {
                    label: "add one".into(),
                    then: Then::Run(Command { action: "project.add", arg: None }),
                },
            );
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Which project should it work in?", items),
            purpose: Purpose::ChatTaskProject(chat, TaskArgs { project: None, agent, name }),
        });
    }

    /// Pick an agent: for the chat `id` (switching it), or for the new chat being written.
    pub(crate) fn pick_chat_agent(&mut self, id: Option<String>) {
        let names = self.agent_names();
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
                // Another profile's agent asks first (the conversation goes to that account).
                let req = Request::ChatUpdate {
                    chat,
                    title: None,
                    pinned: None,
                    archived: None,
                    agent: a,
                    preset: p,
                    keep: None,
                    folders: None,
                    confirmed: false,
                };
                let then = Box::new(Pending::Done("the next message carries the chat over".into()));
                self.request(Pending::Confirm(Box::new(req.clone()), then), req);
            }
            None => {
                let mut folder = false;
                if let Some(ChatPanel::View(v)) = &mut self.chat_panel {
                    if agent {
                        v.agent = Some(value);
                    } else {
                        v.preset = Some(value);
                        // `C-o`'s last step: a folder it may read.
                        folder = std::mem::take(&mut v.choosing);
                    }
                }
                // After the agent, what it may use.
                if agent {
                    self.pick_chat_preset(None);
                } else if folder {
                    self.ask_chat_folder(None);
                }
            }
        }
        self.dirty = true;
    }

    /// The view (shown or put aside) that `f` picks out.
    fn held_view_mut(&mut self, f: impl Fn(&ChatViewState) -> bool) -> Option<&mut ChatViewState> {
        [&mut self.chat_panel, &mut self.chat_aside].into_iter().flatten().find_map(|p| match p {
            ChatPanel::View(v) if f(v) => Some(&mut **v),
            _ => None,
        })
    }

    /// A message went (or didn't) to `chat`: a refused one goes back in front of its draft,
    /// shown or kept for when the chat opens again.
    pub(crate) fn chat_sent(&mut self, chat: &str, text: String, err: Option<String>) {
        let refused = err.is_some();
        match self.held_view_mut(|v| v.id.as_deref() == Some(chat)) {
            Some(v) => {
                v.sending = false;
                if refused {
                    let d = put_back(text, &v.draft);
                    v.set_draft(d);
                }
            }
            None if refused => {
                let d = put_back(text, self.chat_drafts.get(chat).map_or("", |d| d.as_str()));
                self.chat_drafts.insert(chat.to_owned(), d);
            }
            None => {}
        }
        if let Some(e) = err {
            self.toast(e);
        }
        self.dirty = true;
    }

    /// A new chat's first message refused (or not agreed to) made no chat: it no longer shows
    /// as sent, and is back in that composer (or the next new chat's).
    pub(crate) fn chat_not_made(&mut self, token: u64, text: String, err: String) {
        match self.held_view_mut(|v| v.id.is_none() && v.token == token) {
            Some(v) => {
                v.sending = false;
                let d = put_back(text, &v.draft);
                v.view.apply(v.view.rev, true, Vec::new(), String::new());
                v.set_draft(d);
            }
            None => {
                let d = put_back(text, self.chat_drafts.get("").map_or("", |d| d.as_str()));
                self.chat_drafts.insert(String::new(), d);
            }
        }
        self.toast(err);
        self.dirty = true;
    }

    /// The new chat `token` was made by its first message: that view follows it (whatever is
    /// shown now); gone, the chat is offered.
    pub(crate) fn chat_made(&mut self, info: ChatInfo, token: u64) {
        let id = info.id.clone();
        let label = info.label().to_owned();
        self.chat_updated(info.clone());
        match self.held_view_mut(|v| v.id.is_none() && v.token == token) {
            Some(v) => {
                v.sending = false;
                v.id = Some(id.clone());
                v.info = Some(info);
                self.send(ClientMsg::ChatSubscribe { chat: id });
            }
            None => self.toast_offer(
                format!("{label} started"),
                false,
                Offer {
                    label: "open it".into(),
                    then: Then::Run(Command { action: "chat.open", arg: Some(id) }),
                },
            ),
        }
        self.dirty = true;
    }

    /// The person agreed to make the new chat `token`: its message is on its way again.
    pub(crate) fn chat_sending(&mut self, token: u64) {
        if let Some(v) = self.held_view_mut(|v| v.id.is_none() && v.token == token) {
            v.sending = true;
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
                v.edit(|e| e.insert(text));
            }
            Some(ChatPanel::List(l)) => {
                l.query.push_str(&text.replace('\n', " "));
                l.typing = true;
                l.sel = 0;
                self.search_chats();
            }
            None => {}
        }
        self.dirty = true;
    }

    /// Whether a chat-table key's action applies now; when it doesn't, the key is typed.
    fn chat_key_applies(v: &ChatViewState, action: &str) -> bool {
        match action {
            "chat.allow" | "chat.always" | "chat.deny" => {
                v.id.is_some() && v.view.pending_permission() && v.draft.is_empty()
            }
            "chat.incognito" => v.id.is_none(),
            "chat.branch_prev" | "chat.branch_next" => v.id.is_some() && !v.siblings.is_empty(),
            _ => true,
        }
    }

    fn chat_view_key(&mut self, mut v: ChatViewState, key: KeyEvent) {
        v.view.settle_focus();
        // The chat's own keys (`[keys]` `chat:`), where they apply.
        if let Some(cmd) = self.keymap.lookup(Table::Chat, key).cloned()
            && Self::chat_key_applies(&v, cmd.action)
        {
            self.chat_panel = Some(ChatPanel::View(Box::new(v)));
            return self.run(&cmd);
        }
        match key.code {
            KeyCode::Esc => {
                if !v.draft.is_empty() {
                    v.set_draft(String::new());
                } else if v.view.highlight.is_some() {
                    v.view.highlight = None;
                    v.found.clear();
                } else {
                    return self.leave_view(v);
                }
            }
            _ if crate::app::is_newline_key(&key) => v.edit(|e| e.insert("\n")),
            KeyCode::Enter if v.sending => self.toast("one moment: the last message is on its way"),
            KeyCode::Enter
                if v.draft.trim_start().starts_with('/')
                    && ptyro_core::chats::slash_message(&v.draft).is_none() =>
            {
                let text = std::mem::take(&mut v.draft);
                v.set_draft(String::new());
                let chat = v.id.clone();
                self.chat_panel = Some(ChatPanel::View(Box::new(v)));
                self.chat_slash(chat, &text);
                return;
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut v.draft);
                v.set_draft(String::new());
                let text = ptyro_core::chats::slash_message(&text).unwrap_or(text);
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
                                title: v.title.clone(),
                                prompt: Some(text.clone()),
                                incognito: Some(v.incognito),
                                folders: v.folders.clone(),
                                compare: v.compare.clone(),
                                confirmed: false,
                            };
                            let made = Box::new(Pending::ChatMade { text, token: v.token });
                            self.request(Pending::Confirm(Box::new(req.clone()), made), req);
                        }
                    }
                }
            }
            // Between a draft's lines; a draft of one line has none to go to.
            KeyCode::Up | KeyCode::Down => {
                let up = key.code == KeyCode::Up;
                v.edit(|e| e.vertical(up));
            }
            // The rest edit the draft at its cursor (←/→, C-a/C-e, C-u/C-k, C-w, Delete…).
            _ => {
                if v.edit(|e| e.key(key)) {
                    v.view.scroll = 0;
                }
            }
        }
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
            .filter(|c| l.state.admits(c))
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
                // ⏎ ends the typing and opens the selected row (below).
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
            if key.code != KeyCode::Enter {
                self.chat_panel = Some(ChatPanel::List(l));
                if changed {
                    self.search_chats();
                }
                return self.preview_selected();
            }
        }
        // ⏎ on a message hit: that chat, at that message (its branch shown).
        if key.code == KeyCode::Enter
            && l.sel >= chats
            && let Some(h) = l.hits.get(l.sel - chats).cloned()
        {
            self.chat_panel = Some(ChatPanel::List(l));
            return self.open_chat_at(&h.chat.id, h.node.as_deref());
        }
        // The list's own keys (`[keys]` `chatlist:`).
        if let Some(cmd) = self.keymap.lookup(Table::ChatList, key).cloned() {
            self.chat_panel = Some(ChatPanel::List(l));
            return self.run(&cmd);
        }
        let open = match key.code {
            KeyCode::Esc | KeyCode::Char { c: 'q' } => {
                self.chat_panel = None;
                return;
            }
            KeyCode::Char { c: 'j' } | KeyCode::Down => {
                l.sel = (l.sel + 1).min(n.saturating_sub(1));
                false
            }
            KeyCode::Char { c: 'k' } | KeyCode::Up => {
                l.sel = l.sel.saturating_sub(1);
                false
            }
            KeyCode::Enter => true,
            _ => false,
        };
        self.chat_panel = Some(ChatPanel::List(l));
        if open {
            return self.chat_action("chat.open");
        }
        self.preview_selected();
    }

    /// The list's row `i` (chats, then message hits) clicked: opened.
    pub(crate) fn list_clicked(&mut self, i: usize) {
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
            l.sel = i;
            l.typing = false;
        }
        self.chat_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
    }

    pub(crate) fn got_archived(&mut self, chats: Vec<ChatInfo>) {
        if let Some(ChatPanel::List(l)) = &mut self.chat_panel {
            l.archived = Some(chats);
        }
        self.preview_selected();
        self.dirty = true;
    }

    /// What a chat's menu offers: [`CHAT_ACTIONS`], less what doesn't apply to it now.
    pub(crate) fn chat_actions_for(&self, id: &str) -> Vec<&'static str> {
        let c = self.chat(id);
        let imported = c.is_some_and(|c| c.source.is_some());
        let side = c.is_some_and(|c| c.side_of.is_some());
        let busy = c.is_some_and(working);
        let shown = match &self.chat_panel {
            Some(ChatPanel::View(v)) if v.id.as_deref() == Some(id) => Some(v),
            _ => None,
        };
        CHAT_ACTIONS
            .iter()
            .copied()
            .filter(|a| match *a {
                "chat.open" => shown.is_none(),
                "chat.retry" | "chat.edit" | "chat.compare" => !imported,
                "chat.keep" => side,
                "chat.stop" => busy,
                "chat.branch" => shown.is_none_or(|v| !v.siblings.is_empty()),
                _ => true,
            })
            .collect()
    }

    /// The chat's actions, as a menu (a new chat's own choices, before its first message).
    pub(crate) fn chat_menu(&mut self) {
        let (title, actions) = match self.chat_target() {
            Some(id) => (
                self.chat(&id).map_or_else(|| "Chat".into(), |c| format!("Chat {}", c.label())),
                self.chat_actions_for(&id),
            ),
            None if matches!(&self.chat_panel, Some(ChatPanel::View(v)) if v.id.is_none()) => {
                ("New chat".to_owned(), NEW_CHAT_ACTIONS.to_vec())
            }
            None => return self.no_chat_toast(),
        };
        let items = actions
            .iter()
            .filter_map(|a| crate::keymap::spec(a))
            .map(|a| Item {
                label: a.id.into(),
                detail: a.desc.into(),
                key: self.keymap.key_in(Table::Chat, a.id).or_else(|| self.keymap.key_for(a.id)),
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
            Row::ChatAll(ws) => self.open_chat_list(Some(ws.clone())),
            _ => return false,
        }
        true
    }
}

/// What a chat row's menu offers (and the palette, with a chat in view); see
/// [`App::chat_actions_for`] for what is left out where it doesn't apply.
pub const CHAT_ACTIONS: &[&str] = &[
    "chat.open",
    "chat.retitle",
    "chat.pin",
    "chat.stop",
    "chat.retry",
    "chat.edit",
    "chat.branch",
    "chat.fork",
    "chat.side",
    "chat.keep",
    "chat.find_in",
    "chat.cite",
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

/// A new chat's menu, before its first message.
const NEW_CHAT_ACTIONS: &[&str] = &["chat.agent", "chat.preset", "chat.folder", "chat.incognito"];

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

/// The column of the CHATS heading's `＋` (` ▾ CHATS  ＋`): a click from there starts a chat.
pub(crate) const HEADING_PLUS: u16 = 8;

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
                Span::styled("  ＋", fg(app, t.accent)),
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
            let down = app
                .machine_of_name(&c.workspace)
                .filter(|m| m.state != ptyro_core::model::MachineState::Online);
            let mut name = fg(app, if down.is_some() { t.exited } else { t.text });
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
            spans.extend(marks(app, c));
            if let Some(m) = down {
                spans.push(Span::styled(offline_since(m), fg(app, t.dim)));
            }
            Line::from(spans)
        }
        _ => return None,
    })
}

/// A row's marks after its agent: `⎇` a fork, `⚠` it needs you (or has a shell), `●` unread.
fn marks(app: &App, c: &ChatInfo) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mut spans = Vec::new();
    if c.forked {
        spans.push(Span::styled(" ⎇", fg(app, t.dim)));
    }
    if c.attention.is_some() {
        spans.push(Span::styled(" ⚠", fg(app, t.blocked)));
    } else if c.shell {
        // A shell preset: marked as a `dangerous` session is.
        spans.push(Span::styled(" ⚠", fg(app, t.error)));
    }
    if c.unread > 0 {
        spans.push(Span::styled(" ●", fg(app, t.search).add_modifier(Modifier::BOLD)));
    }
    spans
}

/// ` offline since 14:02` (or ` offline` when it never said since when).
fn offline_since(m: &ptyro_core::model::MachineInfo) -> String {
    match m.since_ms.map(crate::ui::clock).filter(|c| !c.is_empty()) {
        Some(at) => format!(" offline since {at}"),
        None => " offline".into(),
    }
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

/// `C-o agent`: an action's key in the chat table, with what it does.
fn chat_hint(app: &App, action: &str, what: &str) -> Option<String> {
    app.keymap.key_in(Table::Chat, action).map(|k| format!("{k} {what}"))
}

fn draw_view(app: &App, f: &mut Frame<'_>, area: Rect, v: &ChatViewState) {
    let t = &app.theme;
    let info = v.id.as_deref().and_then(|id| app.chat(id)).or(v.info.as_ref());
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
        None => (
            false,
            tools.as_ref().is_some_and(|t| t.shell),
            v.folders.len(),
            tools.as_ref().map(|t| t.read),
        ),
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
        if c.archived {
            head.push(Span::styled(" · archived", fg(app, t.dim)));
        }
    }
    let side = info.and_then(|c| c.side_of.as_deref()).and_then(|p| app.chat(p));
    let incognito = v.incognito || info.is_some_and(|c| c.incognito);
    if incognito {
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
    let hints: Vec<String> = if v.id.is_none() {
        [chat_hint(app, "chat.agent", "agent"), chat_hint(app, "chat.incognito", "incognito")]
            .into_iter()
            .flatten()
            .collect()
    } else {
        ["/help".to_owned()].into_iter().chain(chat_hint(app, "chat.find_in", "find")).collect()
    };
    head.push(Span::styled(format!(" {} · Esc back", hints.join(" · ")), fg(app, t.dim)));
    f.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..area });
    let body = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
    // The cursor only where the keys go: the chat has them, nothing is drawn over it.
    let focused = app.overlay.is_none()
        && !app.prefix
        && app.focus == Focus::Pane
        && app.orch.is_none()
        && app.sets_view.is_none()
        && app.inspector.is_none()
        && app.pad_popup.is_none()
        && !app.sidebar_zoom;
    if v.id.is_none() && v.view.items.is_empty() && v.draft.is_empty() {
        // What this chat will be: its tools as its preset says, and whether it is kept.
        let mut can = Vec::new();
        if let Some(t) = &tools {
            if t.web {
                can.push("search the web".to_owned());
            }
            if t.read && (folders > 0 || !t.dirs.is_empty()) {
                can.push("read its folders".to_owned());
            }
            if t.shell {
                can.push("run commands".to_owned());
            }
        }
        let can = if can.is_empty() {
            "it uses no tools".to_owned()
        } else {
            format!("it can {}", can.join(" and "))
        };
        let kept = if incognito {
            "Incognito: nothing of it is kept."
        } else {
            "It is kept, and searchable."
        };
        let choose = chat_hint(app, "chat.agent", "picks the agent, its tools and a folder")
            .map(|h| format!(" {h}."))
            .unwrap_or_default();
        let lines = vec![
            Line::styled("Ask anything.", fg(app, t.text).add_modifier(Modifier::BOLD)),
            Line::styled(format!("A chat with {agent}: {can}."), fg(app, t.dim)),
            Line::styled(kept, fg(app, t.dim)),
            Line::styled(format!("Type, then ⏎.{choose}"), fg(app, t.dim)),
        ];
        let r = Rect { y: body.y + body.height / 3, height: 4, ..body };
        f.render_widget(Paragraph::new(lines).centered(), r);
    }
    let cursor = crate::chat::draw(&v.view, f.buffer_mut(), body, t, app.depth, focused);
    if let Some(c) = cursor {
        f.set_cursor_position(c);
    }
}

/// The chat list as lines, each with the row it shows (chats, then message hits, counted on),
/// and the selected row's line.
fn list_lines(app: &App, l: &ChatList) -> (Vec<(Line<'static>, Option<usize>)>, usize) {
    let t = &app.theme;
    let rows = app.list_rows(l);
    let sel = l.sel.min((rows.len() + l.hits.len()).saturating_sub(1));
    let mut out: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut sel_line = 0;
    if rows.is_empty() {
        out.push((Line::raw(""), None));
        let none = if !l.query.is_empty() || l.state != StateFilter::All {
            "  nothing matches".to_owned()
        } else if l.imported {
            let k = app.keymap.key_in(Table::ChatList, "chat.import").unwrap_or_default();
            format!("  no imported chats: {k} imports your agents' own histories")
        } else {
            let k = app.keymap.key_in(Table::ChatList, "chat.new").unwrap_or_default();
            format!("  no chats here yet: {k} starts one")
        };
        out.push((Line::styled(none, fg(app, t.dim)), None));
    }
    let now = ptyro_core::now_ms();
    let mut last_group = "";
    for (i, c) in rows.iter().enumerate() {
        let g = if c.pinned { "Pinned" } else { group(c.last_ms, now) };
        if g != last_group {
            out.push((
                Line::styled(format!(" {g}"), fg(app, t.dim).add_modifier(Modifier::BOLD)),
                None,
            ));
            last_group = g;
        }
        let (glyph, gc) = glyph(app, c);
        let mut name = fg(app, t.text);
        if i == sel {
            name = name.add_modifier(Modifier::REVERSED);
            sel_line = out.len();
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
        spans.extend(marks(app, c));
        if let Some(m) = app
            .machine_of_name(&c.workspace)
            .filter(|m| m.state != ptyro_core::model::MachineState::Online)
        {
            spans.push(Span::styled(offline_since(m), fg(app, t.dim)));
        }
        out.push((Line::from(spans), Some(i)));
        // The selected chat's last question and answer, under it.
        if i == sel
            && let Some((p, lines)) = &l.preview
            && *p == c.id
        {
            for line in lines {
                out.push((Line::styled(format!("       {line}"), fg(app, t.dim)), Some(i)));
            }
        }
    }
    // The messages that match (SPEC §22.10), each with its snippet; ⏎ opens it there.
    if !l.hits.is_empty() {
        let n = rows.len();
        out.push((
            Line::styled(
                format!(" Messages ({})", l.hits.len()),
                fg(app, t.dim).add_modifier(Modifier::BOLD),
            ),
            None,
        ));
        for (i, h) in l.hits.iter().enumerate() {
            let mut spans = vec![Span::raw("  ")];
            let name = format!(" {} ", h.chat.label());
            let style = if n + i == sel {
                sel_line = out.len();
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
            out.push((Line::from(spans), Some(n + i)));
        }
    } else if l.query.chars().count() >= 3 && l.hits_for == l.query.trim() {
        out.push((Line::styled(" no message matches", fg(app, t.dim)), None));
    }
    (out, sel_line)
}

/// The first of the list's lines shown in `room` rows: the selected one stays in view.
fn list_skip(sel_line: usize, room: usize) -> usize {
    sel_line.saturating_sub(room.saturating_sub(4))
}

/// The list's two header lines.
const LIST_HEAD: u16 = 2;

/// The list row (chats, then message hits) drawn at screen row `y`, for a click.
pub(crate) fn list_row_at(app: &App, area: Rect, y: u16) -> Option<usize> {
    let Some(ChatPanel::List(l)) = &app.chat_panel else { return None };
    let (lines, sel_line) = list_lines(app, l);
    let room = area.height.saturating_sub(LIST_HEAD + 1) as usize;
    let at = y.checked_sub(area.y + LIST_HEAD)? as usize;
    lines.get(list_skip(sel_line, room) + at).and_then(|(_, r)| *r)
}

fn draw_list(app: &App, f: &mut Frame<'_>, area: Rect, l: &ChatList) {
    let t = &app.theme;
    let rows = app.list_rows(l);
    let mut scope = match (&l.workspace, l.archived.is_some()) {
        (_, true) => "archived chats".to_owned(),
        (Some(w), _) => format!("chats in {}", crate::ui::workspace_label(w)),
        (None, _) => "every chat".to_owned(),
    };
    if l.imported {
        scope.push_str(", imported only");
    }
    if l.state != StateFilter::All {
        scope.push_str(&format!(", {} only", l.state.label()));
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {scope} "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!("({})", rows.len()), fg(app, t.dim)),
    ])];
    let search = if l.typing || !l.query.is_empty() {
        format!(" / {}{}", l.query, if l.typing { "▏" } else { "" })
    } else {
        // The list's keys, as `[keys]` has them now.
        let k =
            |a: &str, w: &str| app.keymap.key_in(Table::ChatList, a).map(|k| format!("{k} {w}"));
        let keys: Vec<String> = [
            k("chat.list_search", "search"),
            Some("⏎ open".to_owned()),
            k("chat.new", "new"),
            k("chat.pin", "pin"),
            k("chat.archive", if l.archived.is_some() { "unarchive" } else { "archive" }),
            k("chat.delete", "delete"),
            k("chat.retitle", "title"),
            k("chat.fork", "fork"),
            k("chat.list_state", "state"),
            k("chat.list_archived", "archived"),
            k("chat.list_imported", "imported"),
            k("chat.import", "import"),
            k("chat.list_all", if l.workspace.is_some() { "all" } else { "here" }),
            Some("Esc".to_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!(" {}", keys.join(" · "))
    };
    lines.push(Line::styled(search, fg(app, t.dim)));
    let (body, sel_line) = list_lines(app, l);
    let room = area.height.saturating_sub(LIST_HEAD + 1) as usize;
    lines.extend(body.into_iter().skip(list_skip(sel_line, room)).map(|(l, _)| l));
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
