//! The TUI's state and behaviour (SPEC §11, M2): sessions, panes, focus and modes, and what every
//! key, click, frame and event does to them. Drawing lives in `ui.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ptyro_client::{Client, ConnectOptions};
use ptyro_core::cache::ScreenCache;
use ptyro_core::config::Config;
use ptyro_core::input::{InputEvent, KeyCode, KeyEvent, Mods};
use ptyro_core::model::{
    MAIN_WORKSPACE, ProfileInfo, ProjectInfo, RemoveOpts, ScriptsInfo, WorkspaceInfo, WorktreeInfo,
    WorktreeSpec, WorktreeState,
};
use ptyro_core::paths::Paths;
use ptyro_core::screen::ScrollbackDoc;
use ptyro_core::session::{ProcessState, SessionInfo, SessionKind, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind, InboxEntry, Usage};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::copy::{CopyMode, Outcome as CopyOutcome, Pos, SelMode};
use crate::keymap::{self, Command, Keymap, Table};
use crate::layout::{Dir, Layout, PaneId, Toward};
use crate::palette::{Item, Palette, Picked, Target};
use crate::theme::{Depth, Theme};
use crate::tree::{self, Row};
use crate::{AttachEnd, Options};

/// Saved per view (SPEC §11, M2.10): the layout and what each pane shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedView {
    pub v: u32,
    pub layout: Layout,
    pub panes: HashMap<PaneId, Option<String>>,
    pub focus: PaneId,
    pub zoom: bool,
    pub sidebar: bool,
    pub sidebar_width: u16,
}

/// Where the TUI remembers which workspace it showed last.
pub const LAST_WORKSPACE: &str = "_tui";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Pane,
    Sidebar,
}

pub enum Overlay {
    Help {
        filter: String,
        scroll: usize,
    },
    /// The inbox (SPEC §9.5): `tab` filters by workspace (`None` = all).
    Inbox {
        selected: usize,
        tab: Option<String>,
    },
    /// A session's last screen and what it wants, with an inline reply (SPEC §11.3 `p`).
    Peek {
        session: SessionId,
        screen: String,
        reply: String,
    },
    /// The tour (`ptyro tour`, palette `help.tour`).
    Tour {
        page: usize,
    },
    /// Worktrees ready for review, best first (M6.9); ⏎ opens the inspector.
    ReviewQueue {
        items: Vec<ptyro_core::model::ReviewItem>,
        selected: usize,
    },
    /// `status --explain` for a session.
    Explain {
        session: SessionId,
        lines: Vec<ptyro_protocol::ExplainLine>,
        scroll: usize,
    },
    Picker {
        palette: Palette,
        purpose: Purpose,
    },
    Prompt {
        title: String,
        text: String,
        kind: PromptKind,
    },
    Confirm {
        text: String,
        action: Confirmed,
    },
    /// Lifecycle scripts shown in full before trusting them (SPEC §10.5).
    Trust {
        scripts: ScriptsInfo,
        then: AfterTrust,
        scroll: usize,
    },
}

/// Where a new session or worktree goes, gathered by the wizard (SPEC §11.5, M3.13).
#[derive(Debug, Clone, Default)]
pub struct Wizard {
    pub project: Option<String>,
    pub worktree: Option<String>,
    pub split: Option<Dir>,
    /// The agent picked in step one (`None`: a shell; `Some("")`: ask for a command).
    pub agent: Option<String>,
    /// For `N`: the new worktree's name.
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Purpose {
    Palette,
    /// Show the pick in the focused pane.
    Show,
    /// Show the pick in a new split.
    Split(Dir),
    /// Wizard step one: what to run.
    Agent(Wizard),
    /// Wizard step two: where.
    Target(Wizard),
    Workspace,
    Editor(String),
    Script(String),
    AddProject,
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    /// A command line; empty runs your shell.
    Command(Wizard),
    Rename(SessionId),
    /// One prompt for these sessions (`b`).
    Broadcast(Vec<SessionId>),
    /// A line comment from the inspector (M6.3).
    ReviewComment {
        worktree: String,
        path: String,
        line: u32,
        code: String,
    },
    /// A commit message from the inspector (M6.4).
    CommitMessage {
        worktree: String,
        paths: Vec<String>,
    },
    WorktreeName(Wizard),
    NewWorkspace,
    /// Typed in the runs view (M7).
    Orch(crate::runs::OrchInput),
    /// Typed in the change-set view (M11).
    Sets(crate::sets::SetsInput),
}

#[derive(Debug, Clone)]
pub enum Confirmed {
    Stop(SessionId),
    /// Resend with the guard / pin override acknowledged.
    Spawn(Box<SpawnSpec>, Option<Dir>),
    Worktree(Box<WorktreeSpec>, Option<Dir>),
    RemoveWorktree {
        id: String,
        force: bool,
    },
    /// "Pinned to X — keep it?" after adding a project; `n` unpins.
    KeepPin(String),
    /// Integrate this worktree into the main branch (M6.7).
    Integrate(String),
    /// Send this request, then show this (runs view: cancel, pick, discard).
    Send(Box<Request>, String),
}

#[derive(Debug, Clone)]
pub enum AfterTrust {
    /// Create this worktree (asked before creating it, so its agent starts after setup).
    Create(Box<WorktreeSpec>, Option<Dir>),
    /// Run setup in the new worktree now.
    Setup(String),
    /// Run this script in this worktree.
    Run(String, Option<String>),
    Nothing,
}

/// What a background request was for, so its reply can be applied.
#[derive(Debug, Clone)]
pub enum Pending {
    Spawned {
        split: Option<Dir>,
        spec: Box<SpawnSpec>,
    },
    WorktreeCreated {
        split: Option<Dir>,
        spec: Box<WorktreeSpec>,
    },
    /// Copy mode for this pane, while it still shows this session.
    Scrollback {
        pane: PaneId,
        session: SessionId,
        then: CopyStart,
    },
    Done(String),
    List,
    Registry,
    Saved,
    /// Wizard title: the profile a spawn would get and why.
    Resolved,
    /// Show the trust dialog for these scripts, then do `then`.
    TrustDialog(AfterTrust),
    Trusted(AfterTrust),
    Removed(String),
    ProjectAdded,
    Discovered,
    Scripts(String),
    /// Check the project's scripts are trusted before creating a worktree.
    PreCreate(Box<WorktreeSpec>, Option<Dir>),
    Peek(SessionId),
    Explain(SessionId),
    /// Replies for an inspector that has moved on (another worktree, diff base or load) are
    /// dropped.
    InspectDiff {
        worktree: String,
        against: ptyro_core::model::DiffAgainst,
    },
    InspectComments(String),
    InspectCommits(String),
    /// Events of the named session, for this worktree's load number `load`.
    InspectEvents {
        worktree: String,
        load: u64,
        name: String,
    },
    InspectSent,
    ReviewQueue,
    /// `RunList` for the runs view (M7).
    Runs,
    /// Older history for copy mode, from the recording (M8.2).
    DeepScrollback(PaneId, SessionId),
    /// The compare doc of this run.
    Compare(String),
    /// A change set's ledger, for the set view (M11).
    CsLog(String),
}

#[derive(Debug, Clone, Copy)]
pub enum CopyStart {
    Keys {
        page_up: bool,
    },
    /// Mouse wheel up by this many lines.
    Wheel(usize),
    /// Mouse drag selecting from this screen position.
    Drag {
        x: u16,
        y: u16,
    },
}

pub enum AppMsg {
    Reply(Pending, Result<Reply, Error>),
    /// A workspace's saved view, after switching to it.
    View(String, Result<Reply, Error>),
}

/// One pane's live state.
#[derive(Default)]
pub struct PaneState {
    pub session: Option<SessionId>,
    pub copy: Option<CopyMode>,
    /// Waiting for a scrollback snapshot.
    pub loading: bool,
    pub last: Option<SessionId>,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

enum Drag {
    Divider(crate::layout::Divider),
    Select { pane: PaneId, started: bool },
}

pub struct App {
    pub opts: Options,
    pub connect: ConnectOptions,
    pub paths: Paths,
    pub client: Option<Arc<Client>>,
    pub pushes: Option<mpsc::UnboundedReceiver<ServerMsg>>,
    pub tx: mpsc::UnboundedSender<AppMsg>,
    pub rx: mpsc::UnboundedReceiver<AppMsg>,
    pub config: Config,
    pub keymap: Keymap,
    pub theme: Theme,
    pub theme_name: String,
    pub depth: Depth,
    pub kitty: bool,
    pub sessions: Vec<SessionInfo>,
    pub projects: Vec<ProjectInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub profiles: Vec<ProfileInfo>,
    /// The workspace on screen (SPEC §2.1).
    pub workspace: String,
    /// Collapsed project and worktree ids in the sidebar.
    pub collapsed: HashSet<String>,
    /// Sessions marked with Space in the sidebar, for `b` (broadcast).
    pub marked: Vec<SessionId>,
    /// The inspector (`D`), drawn over the panes (M6).
    pub inspector: Option<crate::inspector::Inspector>,
    /// Runs (`O`), drawn over the panes (M7).
    pub orch: Option<crate::runs::Orch>,
    /// The change-set view (`S`, M11.11).
    pub sets_view: Option<crate::sets::Sets>,
    /// Change sets in the sidebar.
    pub set_nodes: Vec<crate::sets::SetNode>,
    /// Close the inspector when the session it asked for appears (`E`: commit in the editor).
    pub close_inspector_after: bool,
    /// Show the agent that starts in this worktree as soon as it does (after setup).
    pub follow: Option<(String, PaneId)>,
    /// The wizard's profile line ("work — pinned by project").
    pub resolved: Option<String>,
    /// The inbox (newest first), kept current by `InboxAdded` (SPEC §9.5).
    pub inbox: Vec<InboxEntry>,
    /// Usage per (profile, agent) (M4.17).
    pub usage: HashMap<(String, String), Usage>,
    /// Runs (M7), newest first, kept current by `RunUpdated`.
    pub runs: Vec<ptyro_run::wire::RunInfo>,
    /// Whether our outer terminal window has focus (a watched session raises no attention).
    pub terminal_focused: bool,
    pub caches: HashMap<SessionId, ScreenCache>,
    /// Structured sessions' transcripts (M10), and those shown as a terminal instead.
    pub chats: HashMap<SessionId, crate::chat::ChatView>,
    pub chat_terminal: HashSet<SessionId>,
    subscribed: HashSet<SessionId>,
    sizes: HashMap<SessionId, TermSize>,
    pub layout: Layout,
    pub panes: HashMap<PaneId, PaneState>,
    pub focused: PaneId,
    pub zoom: bool,
    pub focus: Focus,
    pub sidebar: bool,
    pub sidebar_width: u16,
    /// The selected sidebar row, and its index when last set (the fallback once it's gone).
    sidebar_row: Option<Row>,
    sidebar_sel: usize,
    pub prefix: bool,
    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,
    pub buffer: Option<String>,
    pub controller: HashMap<SessionId, bool>,
    pub area: Rect,
    pub dirty: bool,
    pub quit: Option<AttachEnd>,
    pub lost_since: Option<Instant>,
    view_dirty_since: Option<Instant>,
    last_list: Instant,
    drag: Option<Drag>,
    reported_focus: Option<(SessionId, bool)>,
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

impl App {
    pub fn new(
        opts: Options,
        connect: ConnectOptions,
        paths: Paths,
        config: Config,
        kitty: bool,
        area: Rect,
    ) -> Self {
        let (keymap, _) = Keymap::new(&config);
        let theme_name = config.ui.theme.clone();
        let theme = Theme::resolve(&config, &theme_name, no_color());
        let (tx, rx) = mpsc::unbounded_channel();
        let mut panes = HashMap::new();
        panes.insert(1, PaneState::default());
        let workspace = opts.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into());
        let follow = opts.follow_worktree.clone().map(|w| (w, 1));
        let opts_tour = opts.tour;
        Self {
            projects: Vec::new(),
            worktrees: Vec::new(),
            workspaces: Vec::new(),
            profiles: Vec::new(),
            workspace,
            collapsed: HashSet::new(),
            marked: Vec::new(),
            inspector: None,
            orch: None,
            sets_view: None,
            set_nodes: Vec::new(),
            close_inspector_after: false,
            follow,
            resolved: None,
            inbox: Vec::new(),
            usage: HashMap::new(),
            runs: Vec::new(),
            terminal_focused: true,
            sidebar: config.ui.sidebar && !opts.raw,
            sidebar_width: config.ui.sidebar_width,
            opts,
            connect,
            paths,
            client: None,
            pushes: None,
            tx,
            rx,
            keymap,
            theme,
            theme_name,
            depth: Depth::detect(|k| std::env::var(k).ok()),
            kitty,
            config,
            sessions: Vec::new(),
            caches: HashMap::new(),
            chats: HashMap::new(),
            chat_terminal: HashSet::new(),
            subscribed: HashSet::new(),
            sizes: HashMap::new(),
            layout: Layout::default(),
            panes,
            focused: 1,
            zoom: false,
            focus: Focus::Pane,
            sidebar_row: None,
            sidebar_sel: 0,
            prefix: false,
            overlay: opts_tour.then_some(Overlay::Tour { page: 0 }),
            toast: None,
            buffer: None,
            controller: HashMap::new(),
            area,
            dirty: true,
            quit: None,
            lost_since: None,
            view_dirty_since: None,
            last_list: Instant::now(),
            drag: None,
            reported_focus: None,
        }
    }

    // ── connection ──────────────────────────────────────────────────────────

    pub async fn connect(&mut self) -> Result<(), String> {
        let mut client = Client::connect(&self.connect).await.map_err(|e| e.to_string())?;
        self.pushes = Some(client.pushes());
        let client = Arc::new(client);
        client.send(ClientMsg::WatchEvents).map_err(|e| e.message)?;
        let sessions = match client.request(Request::List).await.map_err(|e| e.message)? {
            Reply::Sessions { sessions } => sessions,
            _ => Vec::new(),
        };
        self.sessions = sessions;
        self.load_registry(&client).await;
        self.client = Some(Arc::clone(&client));
        self.lost_since = None;
        self.subscribed.clear();
        self.sizes.clear();
        self.caches.clear();
        // A new daemon (or connection) knows nothing of our focus or control.
        self.reported_focus = None;
        self.controller.clear();
        Ok(())
    }

    /// Workspaces, projects, worktrees and profiles (events keep them current).
    async fn load_registry(&mut self, client: &Client) {
        if let Ok(Reply::Workspaces { workspaces }) = client.request(Request::WorkspaceList).await {
            self.workspaces = workspaces;
        }
        if let Ok(Reply::Projects { projects }) = client.request(Request::ProjectList).await {
            self.projects = projects;
        }
        if let Ok(Reply::Worktrees { worktrees }) =
            client.request(Request::WorktreeList { project: None }).await
        {
            self.worktrees = worktrees;
        }
        if let Ok(Reply::Profiles { profiles }) = client.request(Request::ProfileList).await {
            self.profiles = profiles;
        }
        if let Ok(Reply::Inbox { entries }) = client
            .request(Request::Inbox { workspace: None, unread_only: false, limit: Some(500) })
            .await
        {
            self.inbox = entries;
        }
        if let Ok(Reply::Usage { usage }) = client.request(Request::Usage).await {
            self.usage =
                usage.into_iter().map(|u| ((u.profile.clone(), u.agent.clone()), u)).collect();
        }
        if let Ok(Reply::Runs { runs }) =
            client.request(Request::RunList { active: false, limit: Some(100) }).await
        {
            self.runs = runs;
            self.refresh_set_nodes();
        }
        // The morning digest (M7.12): what runs did while nobody watched.
        let unread = |k: ptyro_core::status::AttentionKind| {
            self.inbox.iter().filter(|e| e.kind == k && e.read_ms.is_none()).count()
        };
        let (ended, gates) = (unread(ptyro_core::status::AttentionKind::Run), self.open_gates());
        if ended + gates > 0 {
            self.toast(format!(
                "while you were away: {ended} run update(s), {gates} gate(s) waiting — {} O",
                ptyro_core::config::key_label(&self.keymap.prefix)
            ));
        }
    }

    /// Restore the saved view (unless raw or view-only), then make sure `opts.focus` is shown.
    pub async fn restore_view(&mut self) {
        if self.opts.workspace.is_none()
            && !self.opts.raw
            && let Some(client) = &self.client
            && let Ok(Reply::View { view: Some(v) }) =
                client.request(Request::LoadView { name: LAST_WORKSPACE.into() }).await
            && let Some(ws) = v["workspace"].as_str()
            && self.workspaces.iter().any(|w| w.name == ws)
        {
            self.workspace = ws.to_owned();
        }
        if let Some(id) = self.opts.focus
            && let Some(ws) = self.session(id).map(|s| self.home_of(s))
        {
            self.workspace = ws;
        }
        self.load_view().await;
    }

    /// The workspace a session shows up in: its project's (if in this list), else its home.
    pub fn home_of(&self, s: &SessionInfo) -> String {
        if let Some(p) = &s.project
            && let Some(w) =
                self.workspaces.iter().find(|w| w.name == self.workspace && w.projects.contains(p))
        {
            return w.name.clone();
        }
        if let Some(p) = &s.project
            && let Some(w) = self.workspaces.iter().find(|w| w.projects.contains(p))
        {
            return w.name.clone();
        }
        s.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into())
    }

    /// Load the current workspace's saved view.
    async fn load_view(&mut self) {
        self.layout = Layout::default();
        self.panes = HashMap::from([(1, PaneState::default())]);
        self.focused = 1;
        self.zoom = false;
        if !self.opts.raw
            && let Some(client) = &self.client
            && let Ok(Reply::View { view: Some(v) }) =
                client.request(Request::LoadView { name: self.workspace.clone() }).await
            && let Ok(saved) = serde_json::from_value::<SavedView>(v)
        {
            let ordered_all = self.ordered();
            self.layout = saved.layout.sanitize();
            self.panes = self
                .layout
                .panes()
                .into_iter()
                .map(|p| {
                    let session = saved
                        .panes
                        .get(&p)
                        .cloned()
                        .flatten()
                        .and_then(|s| s.parse::<SessionId>().ok())
                        .filter(|id| ordered_all.contains(id));
                    (p, PaneState { session, ..PaneState::default() })
                })
                .collect();
            self.focused =
                if self.layout.contains(saved.focus) { saved.focus } else { self.layout.first() };
            self.zoom = saved.zoom;
            self.sidebar = saved.sidebar;
            self.sidebar_width = saved.sidebar_width.clamp(12, 80);
        }
        let ordered = self.ordered();
        let want = self.opts.focus.take().or_else(|| {
            let shown = self.panes.get(&self.focused).and_then(|p| p.session);
            shown.or_else(|| {
                ordered
                    .iter()
                    .rev()
                    .copied()
                    .find(|id| self.session(*id).is_some_and(|s| s.state.is_live()))
            })
        });
        if let Some(id) = want {
            if let Some((pane, _)) = self.panes.iter().find(|(_, p)| p.session == Some(id)) {
                self.focused = *pane;
            } else if let Some(p) = self.panes.get_mut(&self.focused) {
                p.session = Some(id);
            }
        }
        // Fill empty panes with sessions not shown yet.
        for pane in self.layout.panes() {
            if self.panes.get(&pane).is_some_and(|p| p.session.is_none()) {
                let shown: Vec<SessionId> = self.panes.values().filter_map(|p| p.session).collect();
                if let Some(id) = ordered.iter().find(|id| !shown.contains(id)) {
                    self.panes.entry(pane).or_default().session = Some(*id);
                }
            }
        }
        self.sync();
    }

    fn client(&self) -> Option<&Arc<Client>> {
        self.client.as_ref()
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(c) = self.client() {
            let _ = c.send(msg);
        }
    }

    pub(crate) fn request(&self, pending: Pending, req: Request) {
        let Some(client) = self.client().cloned() else { return };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = client.request(req).await;
            let _ = tx.send(AppMsg::Reply(pending, res));
        });
    }

    pub fn lost(&mut self, reason: &str) {
        if self.client.is_some() {
            self.toast_err(format!("lost the daemon ({reason}); reconnecting…"));
        }
        self.client = None;
        self.pushes = None;
        self.lost_since.get_or_insert_with(Instant::now);
        self.dirty = true;
    }

    // ── geometry, subscriptions, sizes ─────────────────────────────────────

    pub fn main_area(&self) -> Rect {
        let mut a = self.area;
        if !self.opts.raw && self.config.ui.status_bar && a.height > 2 {
            a.height -= 1;
        }
        if self.sidebar && !self.zoom && a.width > self.sidebar_width + 20 {
            a.x += self.sidebar_width + 1;
            a.width -= self.sidebar_width + 1;
        }
        a
    }

    pub fn sidebar_area(&self) -> Option<Rect> {
        let main = self.main_area();
        (main.x > self.area.x).then_some(Rect {
            x: self.area.x,
            y: self.area.y,
            width: self.sidebar_width,
            height: main.height,
        })
    }

    /// Visible panes with their outer rects (header included).
    pub fn pane_rects(&self) -> Vec<(PaneId, Rect)> {
        let main = self.main_area();
        if self.zoom {
            return vec![(self.focused, main)];
        }
        self.layout.rects(main)
    }

    /// The part of a pane rect the session draws in (below the header).
    pub fn inner(&self, r: Rect) -> Rect {
        if self.opts.raw || r.height < 3 {
            r
        } else {
            Rect { y: r.y + 1, height: r.height - 1, ..r }
        }
    }

    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn focused_session(&self) -> Option<SessionId> {
        self.panes.get(&self.focused).and_then(|p| p.session)
    }

    /// Subscribe to what's visible, unsubscribe the rest, size every visible session to its pane,
    /// and tell the daemon what's focused.
    pub fn sync(&mut self) {
        let rects = self.pane_rects();
        let mut visible: HashMap<SessionId, TermSize> = HashMap::new();
        for (pane, rect) in &rects {
            let Some(id) = self.panes.get(pane).and_then(|p| p.session) else { continue };
            let inner = self.inner(*rect);
            let size = TermSize::new(inner.width.max(2), inner.height.max(1));
            // The focused pane wins when one session is shown twice.
            if *pane == self.focused || !visible.contains_key(&id) {
                visible.insert(id, size);
            }
        }
        let control = !self.opts.view_only;
        for id in self.subscribed.clone() {
            if !visible.contains_key(&id) {
                self.send(ClientMsg::Unsubscribe { session: id });
                self.subscribed.remove(&id);
                self.caches.remove(&id);
                self.sizes.remove(&id);
            }
        }
        for (id, size) in visible {
            let live = self.session(id).is_some_and(|s| s.state.is_live());
            if self.subscribed.insert(id) {
                let info_size = self.session(id).map_or(size, |s| s.size);
                self.caches.insert(id, ScreenCache::new(info_size));
                self.send(ClientMsg::Subscribe {
                    session: id,
                    size: Some(size),
                    control: control && live,
                });
                self.sizes.insert(id, size);
            } else if self.sizes.get(&id) != Some(&size) {
                self.sizes.insert(id, size);
                if control && live {
                    self.send(ClientMsg::Resize { session: id, size });
                }
            }
        }
        for (pane, rect) in rects {
            let inner = self.inner(rect);
            if let Some(c) = self.panes.get_mut(&pane).and_then(|p| p.copy.as_mut()) {
                c.set_height(inner.height as usize);
            }
        }
        let focused = self.focused_session().map(|id| (id, self.terminal_focused));
        if focused != self.reported_focus && !self.opts.view_only {
            if let Some((id, tf)) = focused {
                self.send(ClientMsg::Focus { session: id, terminal_focused: tf });
            }
            self.reported_focus = focused;
        }
        self.dirty = true;
    }

    fn view_changed(&mut self) {
        if !self.opts.raw && !self.opts.view_only {
            self.view_dirty_since = Some(Instant::now());
        }
        self.sync();
    }

    pub fn save_view(&mut self) {
        self.view_dirty_since = None;
        if self.opts.raw || self.opts.view_only {
            return;
        }
        let saved = SavedView {
            v: 1,
            layout: self.layout.clone(),
            panes: self.panes.iter().map(|(k, p)| (*k, p.session.map(|s| s.full()))).collect(),
            focus: self.focused,
            zoom: self.zoom,
            sidebar: self.sidebar,
            sidebar_width: self.sidebar_width,
        };
        if let Ok(view) = serde_json::to_value(saved) {
            self.request(Pending::Saved, Request::SaveView { name: self.workspace.clone(), view });
            let last = serde_json::json!({ "workspace": self.workspace });
            self.request(
                Pending::Saved,
                Request::SaveView { name: LAST_WORKSPACE.into(), view: last },
            );
        }
    }

    /// Periodic work: toast expiry, debounced view saves, the sidebar's foreground names.
    pub fn tick(&mut self) {
        if self.toast.as_ref().is_some_and(|t| Instant::now() > t.until) {
            self.toast = None;
            self.dirty = true;
        }
        if self.view_dirty_since.is_some_and(|t| t.elapsed() > Duration::from_millis(500)) {
            self.save_view();
        }
        if self.last_list.elapsed() > Duration::from_secs(2) && self.client.is_some() {
            self.last_list = Instant::now();
            self.request(Pending::List, Request::List);
        }
        if self.config.ui.clock {
            self.dirty = true;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: false,
            until: Instant::now() + Duration::from_secs(3),
        });
        self.dirty = true;
    }

    pub fn toast_err(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: true,
            until: Instant::now() + Duration::from_secs(6),
        });
        self.dirty = true;
    }

    // ── server messages ─────────────────────────────────────────────────────

    pub fn on_server(&mut self, msg: ServerMsg) {
        self.dirty = true;
        match msg {
            ServerMsg::Screen { frame } => {
                if let Some(cache) = self.caches.get_mut(&frame.session) {
                    cache.apply(frame.keyframe, frame.seq, &frame.delta);
                }
                self.send(ClientMsg::Ack { session: frame.session, seq: frame.seq });
            }
            ServerMsg::Event { event } => self.on_event(event),
            ServerMsg::Chat { session, rev, reset, items, draft } => {
                self.chats.entry(session).or_default().apply(rev, reset, items, draft);
            }
            ServerMsg::Goodbye { reason } => self.lost(&reason),
            _ => {}
        }
    }

    fn upsert(&mut self, info: SessionInfo) {
        match self.sessions.iter_mut().find(|s| s.id == info.id) {
            Some(s) => *s = info,
            None => self.sessions.push(info),
        }
    }

    fn on_event(&mut self, event: DomainEvent) {
        match event {
            // We fell behind: what we know may be stale.
            DomainEvent::EventsLost { .. } => self.request(Pending::List, Request::List),
            DomainEvent::SessionCreated { info } => {
                // The agent a new worktree was waiting for (after its setup script).
                let follow = self.follow.as_ref().is_some_and(|(w, _)| {
                    info.worktree.as_deref() == Some(w.as_str()) && info.kind == SessionKind::Agent
                });
                let id = info.id;
                self.upsert(info);
                if follow && let Some((_, pane)) = self.follow.take() {
                    if self.layout.contains(pane) {
                        self.focused = pane;
                    }
                    self.show(id, None);
                }
            }
            DomainEvent::SessionUpdated { info } => self.upsert(info),
            DomainEvent::SessionExited { info } => {
                if self.opts.raw && self.focused_session() == Some(info.id) {
                    self.quit = Some(AttachEnd::Exited(info.state.clone()));
                }
                self.upsert(info);
            }
            DomainEvent::SessionRestarted { info } => {
                let id = info.id;
                self.upsert(info);
                // Fresh process, fresh screen: resubscribe.
                if self.subscribed.remove(&id) {
                    self.send(ClientMsg::Unsubscribe { session: id });
                    self.sizes.remove(&id);
                }
                self.sync();
            }
            DomainEvent::SessionRemoved { id } => {
                self.sessions.retain(|s| s.id != id);
                for p in self.panes.values_mut() {
                    if p.session == Some(id) {
                        p.session = None;
                        p.copy = None;
                    }
                    if p.last == Some(id) {
                        p.last = None;
                    }
                }
                if self.opts.raw {
                    self.quit = Some(AttachEnd::Gone);
                }
                self.sync();
            }
            DomainEvent::ControllerChanged { session, controller } => {
                let me = self.client().map(|c| c.id);
                self.controller.insert(session, controller.is_none_or(|c| Some(c) == me));
            }
            DomainEvent::Bell { session } => {
                if self.subscribed.contains(&session) {
                    crate::bell();
                }
            }
            // A program's copy goes to your terminal's clipboard, not ptyro's paste buffer (that
            // holds only what you yanked, so a session can't plant text for another).
            DomainEvent::Clipboard { session, text } => {
                if self.subscribed.contains(&session) && self.config.clipboard.allow_write {
                    crate::osc52(&text);
                }
            }
            DomainEvent::Notification { .. } => {}
            DomainEvent::InboxAdded { entry } => {
                // An open inbox keeps the same entry selected (it's above it, in its tab).
                let shown = |tab: Option<&str>| {
                    tab.is_none_or(|t| entry.workspace.as_deref().unwrap_or(MAIN_WORKSPACE) == t)
                };
                if let Some(Overlay::Inbox { selected, tab }) = &self.overlay
                    && shown(tab.as_deref())
                    && !self.inbox_tab(tab.as_deref()).is_empty()
                {
                    let (selected, tab) = (*selected + 1, tab.clone());
                    self.overlay = Some(Overlay::Inbox { selected, tab });
                }
                self.inbox.insert(0, entry);
                self.inbox.truncate(500);
            }
            DomainEvent::Notify { session, title, body, bell, .. } => {
                // Our outer terminal shows it (SPEC §9.5); not for what we're looking at.
                if !(self.terminal_focused && self.focused_session() == Some(session)) {
                    crate::notify_outer(&title, &body);
                    if bell {
                        crate::bell();
                    }
                }
            }
            DomainEvent::UsageUpdated { usage } => {
                self.usage.insert((usage.profile.clone(), usage.agent.clone()), usage);
            }
            DomainEvent::RunUpdated { run } => self.run_updated(run),
            DomainEvent::ProjectUpdated { info } => {
                upsert_by(&mut self.projects, info, |p| p.id.clone())
            }
            DomainEvent::ProjectRemoved { id } => self.projects.retain(|p| p.id != id),
            DomainEvent::WorkspaceUpdated { info } => {
                upsert_by(&mut self.workspaces, info, |w| w.id.clone())
            }
            DomainEvent::WorkspaceRemoved { id } => {
                let name = self.workspaces.iter().find(|w| w.id == id).map(|w| w.name.clone());
                self.workspaces.retain(|w| w.id != id);
                if name.as_deref() == Some(self.workspace.as_str()) {
                    self.switch_workspace(MAIN_WORKSPACE.into());
                }
            }
            DomainEvent::WorktreeUpdated { info } => {
                upsert_by(&mut self.worktrees, info, |w| w.id.clone())
            }
            DomainEvent::WorktreeRemoved { id } => self.worktrees.retain(|w| w.id != id),
            DomainEvent::WorktreeProgress { worktree, step } => {
                self.toast(format!("{worktree}: {step}"))
            }
            DomainEvent::ConfigReloaded { error } => match error {
                Some(e) => self.toast_err(format!("config not applied: {e}")),
                None => {
                    self.reload_config();
                    self.request(Pending::Registry, Request::ProfileList);
                }
            },
        }
    }

    fn reload_config(&mut self) {
        let path = Config::path(&self.paths);
        match Config::load(&path) {
            Ok(config) => {
                let (keymap, errors) = Keymap::new(&config);
                self.keymap = keymap;
                self.theme_name = config.ui.theme.clone();
                self.theme = Theme::resolve(&config, &self.theme_name, no_color());
                self.config = config;
                match errors.first() {
                    Some(e) => self.toast_err(format!("config: {e}")),
                    None => self.toast("config reloaded"),
                }
                self.sync();
            }
            Err(e) => self.toast_err(e.to_string()),
        }
    }

    pub fn on_reply(&mut self, msg: AppMsg) {
        self.dirty = true;
        let (pending, res) = match msg {
            AppMsg::Reply(p, r) => (p, r),
            AppMsg::View(name, r) => {
                // Switched on again since: that view is stale.
                if name != self.workspace {
                    return;
                }
                let view = match r {
                    Ok(Reply::View { view }) => view,
                    _ => None,
                };
                self.apply_view(view);
                return;
            }
        };
        match (pending, res) {
            (Pending::List, Ok(Reply::Sessions { sessions })) => self.sessions = sessions,
            (Pending::Registry, Ok(Reply::Profiles { profiles })) => self.profiles = profiles,
            (Pending::Spawned { split, .. }, Ok(Reply::Session { info })) => {
                if std::mem::take(&mut self.close_inspector_after) {
                    self.inspector = None;
                }
                let id = info.id;
                self.upsert(info);
                self.show(id, split);
            }
            (Pending::Spawned { split, spec }, Err(e))
                if e.code == ErrorCode::NeedsConfirmation =>
            {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("{} [y/N]", e.message),
                    action: Confirmed::Spawn(spec, split),
                });
            }
            (Pending::WorktreeCreated { split, spec }, Err(e))
                if e.code == ErrorCode::NeedsConfirmation =>
            {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("{} [y/N]", e.message),
                    action: Confirmed::Worktree(spec, split),
                });
            }
            (
                Pending::WorktreeCreated { split, .. },
                Ok(Reply::WorktreeCreated {
                    info,
                    setup,
                    agent,
                    agent_pending,
                    untrusted,
                    notes,
                    ..
                }),
            ) => {
                let pane_shown = agent.as_ref().or(setup.as_ref()).map(|s| s.id);
                upsert_by(&mut self.worktrees, info.clone(), |w| w.id.clone());
                for s in [setup, agent].into_iter().flatten() {
                    self.upsert(s);
                }
                if let Some(id) = pane_shown {
                    self.show(id, split);
                }
                if agent_pending {
                    self.follow = Some((info.id.clone(), self.focused));
                }
                let profile =
                    notes.iter().find(|n| n.starts_with("profile ")).cloned().unwrap_or_default();
                self.toast(format!(
                    "{} on {} · {profile}",
                    info.qualified(),
                    info.branch.as_deref().unwrap_or("-")
                ));
                if let Some(scripts) = untrusted {
                    self.overlay = Some(Overlay::Trust {
                        scripts,
                        then: AfterTrust::Setup(info.id),
                        scroll: 0,
                    });
                }
            }
            (Pending::Resolved, Ok(Reply::Resolved { profile, reason, question })) => {
                let label = self.profile_label(&profile);
                let ask = if question.is_some() { " · will ask first" } else { "" };
                self.resolved = Some(format!("{label} — {}{ask}", reason.label()));
                if let Some(Overlay::Picker {
                    palette,
                    purpose: Purpose::Agent(w) | Purpose::Target(w),
                }) = &mut self.overlay
                {
                    palette.title =
                        wizard_title(w, &self.projects, &self.worktrees, self.resolved.as_deref());
                }
            }
            (Pending::TrustDialog(then), Ok(Reply::Scripts { scripts })) => {
                if scripts.text.is_empty() {
                    self.toast("this project has no scripts (.ptyro.toml [scripts])");
                } else if scripts.trusted {
                    self.after_trust(then);
                } else {
                    self.overlay = Some(Overlay::Trust { scripts, then, scroll: 0 });
                }
            }
            (Pending::Trusted(then), Ok(_)) => self.after_trust(then),
            (Pending::Done(_), Ok(Reply::Notified { results })) => self.toast(results.join(" · ")),
            (Pending::Peek(id), Ok(Reply::Screen { text })) => {
                if let Some(Overlay::Peek { session, screen, .. }) = &mut self.overlay
                    && *session == id
                {
                    *screen = text;
                }
            }
            (Pending::InspectDiff { worktree, against }, Ok(Reply::Diff { doc })) => {
                if let Some(i) = &mut self.inspector
                    && i.worktree == worktree
                    && i.against == against
                {
                    i.file = i.file.min(doc.files.len().saturating_sub(1));
                    i.diff = Some(doc);
                    i.error = None;
                }
            }
            (Pending::InspectDiff { worktree, .. } | Pending::InspectCommits(worktree), Err(e)) => {
                if let Some(i) = &mut self.inspector
                    && i.worktree == worktree
                {
                    i.error = Some(e.message);
                }
            }
            (Pending::InspectComments(worktree), Ok(Reply::Comments { comments })) => {
                if let Some(i) = &mut self.inspector
                    && i.worktree == worktree
                {
                    i.comments = comments;
                }
            }
            (Pending::InspectCommits(worktree), Ok(Reply::Commits { commits })) => {
                if let Some(i) = &mut self.inspector
                    && i.worktree == worktree
                {
                    i.commits = Some(commits);
                    i.error = None;
                }
            }
            (Pending::InspectEvents { worktree, load, name }, Ok(Reply::Events { events })) => {
                if let Some(i) = &mut self.inspector
                    && i.worktree == worktree
                    && i.load == load
                {
                    i.events.extend(events.into_iter().map(|e| (name.clone(), e)));
                }
            }
            (Pending::InspectSent, Ok(Reply::Session { info })) => {
                if let Some(i) = &mut self.inspector {
                    i.comments.clear();
                }
                self.toast(format!("comments sent to {} (typed once its turn is over)", info.name));
            }
            (Pending::CsLog(run), Ok(Reply::Cs { value })) => {
                if let Ok(entries) = serde_json::from_value(value) {
                    self.got_ledger(&run, entries);
                }
            }
            (Pending::Done(msg), Ok(Reply::Cs { .. })) => self.toast(msg),
            (Pending::Runs, Ok(Reply::Runs { runs })) => {
                self.keeping_run_selection(|app| app.runs = runs);
                self.refresh_set_nodes();
                if let Some(o) = &mut self.orch {
                    o.loading = false;
                }
            }
            (Pending::Compare(run), Ok(Reply::Compare { doc })) => self.got_compare(&run, doc),
            (Pending::ReviewQueue, Ok(Reply::ReviewQueue { items })) => {
                if items.is_empty() {
                    self.toast("nothing waits for review");
                } else {
                    self.overlay = Some(Overlay::ReviewQueue { items, selected: 0 });
                }
            }
            (Pending::Done(_), Ok(Reply::Committed { sha })) => {
                self.toast(format!("committed {}", &sha[..10.min(sha.len())]));
                self.inspector_load();
            }
            (Pending::Done(_), Ok(Reply::Notes { notes })) => {
                self.toast(notes.join(" · "));
                self.inspector_load();
            }
            (Pending::Done(_), Ok(Reply::Pr { info: Some(pr) })) => {
                self.toast(format!("PR #{}: {}", pr.number, pr.url));
            }
            (Pending::Explain(id), Ok(Reply::Explain { lines })) => {
                // Newest last: start at the bottom.
                let scroll = lines.len().saturating_sub(self.overlay_body_h(2));
                self.overlay = Some(Overlay::Explain { session: id, lines, scroll });
            }
            (Pending::PreCreate(spec, split), Ok(Reply::Scripts { scripts })) => {
                if scripts.names.iter().any(|n| n == "setup") && !scripts.trusted {
                    self.overlay = Some(Overlay::Trust {
                        scripts,
                        then: AfterTrust::Create(spec, split),
                        scroll: 0,
                    });
                } else {
                    self.create_worktree(*spec, split);
                }
            }
            (Pending::PreCreate(spec, split), Err(_)) => self.create_worktree(*spec, split),
            (Pending::Removed(_), Ok(Reply::Removed { notes })) => self.toast(notes.join(" · ")),
            (Pending::Removed(id), Err(e)) if e.code == ErrorCode::DirtyWorktree => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("{}\n\nDiscard that work and remove it anyway? [y/N]", e.message),
                    action: Confirmed::RemoveWorktree { id, force: true },
                });
            }
            (Pending::ProjectAdded, Ok(Reply::Project { info, notes })) => {
                upsert_by(&mut self.projects, info.clone(), |p| p.id.clone());
                let pinned = notes.iter().any(|n| n.starts_with("pinned to profile"));
                self.toast(format!("added {} {}", info.slug, notes.join(" · ")));
                if pinned && let Some(pin) = &info.pin {
                    self.overlay = Some(Overlay::Confirm {
                        text: format!(
                            "{} matches profile {pin}'s rules and is now pinned to it. Keep the pin? [Y/n]",
                            info.slug
                        ),
                        action: Confirmed::KeepPin(info.id.clone()),
                    });
                }
            }
            (Pending::Discovered, Ok(Reply::Paths { paths })) => self.open_add_project(paths),
            (Pending::Scripts(wt), Ok(Reply::Scripts { scripts })) => {
                self.open_script_picker(wt, scripts)
            }
            (Pending::Spawned { .. } | Pending::Done(_), Err(e))
                if e.code == ErrorCode::UntrustedProject =>
            {
                self.toast_err(format!(
                    "{} ({} : → project.trust)",
                    e.message,
                    ptyro_core::config::key_label(&self.keymap.prefix)
                ));
            }
            (Pending::Scrollback { pane, session, then }, Ok(Reply::Scrollback { doc })) => {
                if self.panes.get(&pane).is_some_and(|p| p.session == Some(session)) {
                    self.start_copy(pane, &doc, then)
                } else if let Some(p) = self.panes.get_mut(&pane) {
                    // The pane shows another session now.
                    p.loading = false;
                }
            }
            (Pending::DeepScrollback(pane, session), Ok(Reply::Scrollback { doc })) => {
                if let Some(c) = self
                    .panes
                    .get_mut(&pane)
                    .filter(|p| p.session == Some(session))
                    .and_then(|p| p.copy.as_mut())
                {
                    c.extend(&doc);
                    self.dirty = true;
                }
            }
            (Pending::Scrollback { pane, .. }, Err(e)) => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.loading = false;
                }
                self.toast_err(e.message);
            }
            (Pending::Done(msg), Ok(_)) => {
                if !msg.is_empty() {
                    self.toast(msg);
                }
            }
            (Pending::Saved, Ok(_)) => {}
            (_, Err(e)) => self.toast_err(e.message),
            _ => {}
        }
    }

    // ── showing sessions ────────────────────────────────────────────────────

    /// Show `id` in the focused pane, or in a new split.
    pub fn show(&mut self, id: SessionId, split: Option<Dir>) {
        let pane = match split {
            Some(dir) => match self.layout.split(self.focused, dir) {
                Some(p) => {
                    self.panes.insert(p, PaneState::default());
                    self.zoom = false;
                    p
                }
                None => {
                    self.toast_err(format!("at most {} panes", crate::layout::MAX_PANES));
                    self.focused
                }
            },
            None => self.focused,
        };
        let p = self.panes.entry(pane).or_default();
        if p.session != Some(id) {
            p.last = p.session;
            p.session = Some(id);
            p.copy = None;
        }
        self.focused = pane;
        self.focus = Focus::Pane;
        // A session from another workspace brings its workspace along.
        if let Some(s) = self.session(id).cloned()
            && !self.ordered().contains(&id)
        {
            let ws = self.home_of(&s);
            if ws != self.workspace {
                self.workspace = ws;
            }
        }
        let rows = self.rows();
        if let Some(i) = rows.iter().position(|r| *r == Row::Session(id)) {
            self.set_sel(&rows, i);
        }
        self.view_changed();
    }

    fn cycle(&mut self, step: isize) {
        let ordered = self.ordered();
        if ordered.is_empty() {
            return;
        }
        let n = ordered.len() as isize;
        let cur = self.focused_session().and_then(|id| ordered.iter().position(|s| *s == id));
        let next = cur.map_or(0, |c| ((c as isize + step).rem_euclid(n)) as usize);
        self.show(ordered[next], None);
    }

    /// Move the sidebar selection, skipping group labels.
    fn nav_move(&mut self, to: usize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let to = to.min(rows.len() - 1);
        let forward = to >= self.sel_index(&rows);
        let mut i = to;
        while !rows[i].selectable() {
            i = if forward { i + 1 } else { i.wrapping_sub(1) };
            if i >= rows.len() {
                return;
            }
        }
        self.set_sel(&rows, i);
    }

    /// The selected row's index in `rows`: where it is now, else the nearest selectable row to
    /// where it was.
    pub fn sel_index(&self, rows: &[Row]) -> usize {
        anchor_index(rows, self.sidebar_row.as_ref(), self.sidebar_sel)
    }

    pub(crate) fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        rows.get(self.sel_index(&rows)).cloned()
    }

    fn set_sel(&mut self, rows: &[Row], i: usize) {
        self.sidebar_sel = i;
        self.sidebar_row = rows.get(i).cloned();
    }

    /// Rows scrolled off the top of the sidebar (in `area`) to keep row `sel` visible; drawing
    /// and clicking share it.
    pub fn sidebar_skip(&self, area: Rect, sel: usize) -> usize {
        // Less the title and the hint line.
        let visible = area.height.saturating_sub(2) as usize;
        if self.focus == Focus::Sidebar && sel >= visible { sel + 1 - visible } else { 0 }
    }

    // ── terminal input ──────────────────────────────────────────────────────

    pub fn on_key(&mut self, ck: &crossterm::event::KeyEvent) {
        self.dirty = true;
        let Some(key) = crate::keys::key(ck) else { return };
        if key.kind == ptyro_core::input::KeyKind::Release {
            // Releases only matter to the program (kitty REPORT_EVENT_TYPES).
            if self.overlay.is_none()
                && !self.prefix
                && self.focus == Focus::Pane
                && !self.in_copy()
            {
                self.forward(InputEvent::Key { key });
            }
            return;
        }
        if self.overlay.is_some() {
            // The prefix always works: it closes whatever is open and starts a command.
            if self.keymap.is_prefix(key) && !matches!(self.overlay, Some(Overlay::Prompt { .. })) {
                self.overlay = None;
                self.prefix = true;
                return;
            }
            self.overlay_key(key);
            return;
        }
        if self.orch.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.prefix = true;
            } else {
                self.orch_key(key);
            }
            return;
        }
        if self.sets_view.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.prefix = true;
            } else {
                self.sets_key(key);
            }
            return;
        }
        if self.inspector.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.prefix = true;
            } else {
                self.inspector_key(key);
            }
            return;
        }
        if self.prefix {
            self.prefix = false;
            if self.keymap.is_prefix(key) {
                // Prefix twice: the program gets one.
                self.forward(InputEvent::Key { key });
                return;
            }
            match self.keymap.lookup(Table::Prefix, key).cloned() {
                Some(cmd) => self.run(&cmd),
                None if key.code == KeyCode::Esc => {}
                None => self.toast(format!(
                    "{} {}: not bound ({0} ? lists keys)",
                    ptyro_core::config::key_label(&self.keymap.prefix),
                    ptyro_core::config::key_label(&keymap::normalize(key))
                )),
            }
            return;
        }
        if self.keymap.is_prefix(key) {
            self.prefix = true;
            return;
        }
        if let Some(cmd) = self.keymap.lookup(Table::Global, key).cloned() {
            self.run(&cmd);
            return;
        }
        if self.in_copy() {
            self.copy_key(key);
            return;
        }
        if self.focus == Focus::Sidebar {
            if let Some(cmd) = self.keymap.lookup(Table::Nav, key).cloned() {
                self.run(&cmd);
            }
            return;
        }
        // An exited pane offers Enter to restart; a dormant one, to resume.
        if let Some(id) = self.focused_session()
            && let Some(state) =
                self.session(id).map(|s| s.state.clone()).filter(|st| !st.is_live())
        {
            if key.code == KeyCode::Enter {
                let req = if state == ProcessState::Dormant {
                    Request::Resume { addr: id.full() }
                } else {
                    Request::Restart { addr: id.full() }
                };
                self.request(Pending::Done(String::new()), req);
            }
            return;
        }
        // A chat view scrolls itself; every other key goes to the session's line editor.
        if let Some(id) = self.focused_session().filter(|id| self.chat_shown(*id))
            && let Some(view) = self.chats.get_mut(&id)
        {
            match key.code {
                KeyCode::PageUp => {
                    view.scroll = (view.scroll + 10).min(view.max_scroll.get());
                    self.dirty = true;
                    return;
                }
                KeyCode::PageDown => {
                    view.scroll = view.scroll.saturating_sub(10);
                    self.dirty = true;
                    return;
                }
                _ => view.scroll = 0,
            }
        }
        self.forward(InputEvent::Key { key });
    }

    pub fn on_paste(&mut self, text: String) {
        if let Some(Overlay::Prompt { text: t, .. }) = &mut self.overlay {
            t.push_str(&text.replace(['\n', '\r'], " "));
            self.dirty = true;
            return;
        }
        self.forward(InputEvent::Paste { text });
    }

    pub fn on_focus(&mut self, gained: bool) {
        self.forward(InputEvent::Focus { gained });
        self.terminal_focused = gained;
        self.sync();
    }

    /// The pane shows this structured session's chat view (not its terminal view).
    pub fn chat_shown(&self, id: SessionId) -> bool {
        self.session(id).is_some_and(|s| s.structured.is_some())
            && !self.chat_terminal.contains(&id)
    }

    fn in_copy(&self) -> bool {
        self.panes.get(&self.focused).is_some_and(|p| p.copy.is_some())
    }

    /// Send input to the focused session (never in view-only mode).
    fn forward(&self, event: InputEvent) {
        if self.opts.view_only || self.focus != Focus::Pane {
            return;
        }
        if let Some(id) = self.focused_session()
            && self.session(id).is_some_and(|s| s.state.is_live())
        {
            self.send(ClientMsg::Input { session: id, event });
        }
    }

    pub fn on_resize(&mut self, w: u16, h: u16) {
        self.area = Rect::new(0, 0, w, h);
        self.sync();
    }

    // ── actions ─────────────────────────────────────────────────────────────

    pub fn run(&mut self, cmd: &Command) {
        self.dirty = true;
        let focused = self.focused_session();
        match cmd.action {
            "help" => self.overlay = Some(Overlay::Help { filter: String::new(), scroll: 0 }),
            "palette" => self.open_palette(),
            "app.detach" => self.quit = Some(AttachEnd::Detached),
            "session.new" => self.open_wizard(None),
            "attention.next" => self.next_attention(),
            "inbox" => {
                self.overlay =
                    Some(Overlay::Inbox { selected: 0, tab: Some(self.workspace.clone()) })
            }
            "peek" => {
                let target = if self.focus == Focus::Sidebar {
                    match self.selected_row() {
                        Some(Row::Session(id)) => Some(id),
                        _ => None,
                    }
                } else {
                    self.attention_order(false).first().copied().or(focused)
                };
                match target {
                    Some(id) => self.open_peek(id),
                    None => self.toast("nothing to peek at"),
                }
            }
            "session.resume" | "session.hibernate" | "session.explain" | "session.mute" => {
                let target = if self.focus == Focus::Sidebar {
                    match self.selected_row() {
                        Some(Row::Session(id)) => Some(id),
                        _ => None,
                    }
                } else {
                    focused
                };
                let Some(id) = target else {
                    self.toast("no session here");
                    return;
                };
                let addr = id.full();
                match cmd.action {
                    "session.resume" => {
                        self.request(Pending::Done("resumed".into()), Request::Resume { addr })
                    }
                    "session.hibernate" => self
                        .request(Pending::Done("hibernated".into()), Request::Hibernate { addr }),
                    "session.mute" => self.request(
                        Pending::Done("notifications muted for 1h".into()),
                        Request::Mute { target: addr, minutes: Some(60), unmute: false },
                    ),
                    _ => self.request(Pending::Explain(id), Request::StatusExplain { addr }),
                }
            }
            "notify.test" => self.request(Pending::Done(String::new()), Request::NotifyTest),
            "session.command" => {
                let (project, worktree) = self.context();
                self.prompt_command(Wizard { project, worktree, ..Wizard::default() });
            }
            "worktree.new" => {
                let (project, _) = self.context();
                match project {
                    Some(project) => {
                        let w = Wizard { project: Some(project), ..Wizard::default() };
                        self.resolve_for(&w);
                        self.overlay = Some(Overlay::Prompt {
                            title: format!(
                                "{} — name (empty: a random one)",
                                wizard_title(
                                    &Wizard { name: Some(String::new()), ..w.clone() },
                                    &self.projects,
                                    &self.worktrees,
                                    None
                                )
                            ),
                            text: String::new(),
                            kind: PromptKind::WorktreeName(w),
                        });
                    }
                    None => self.toast(format!(
                        "no project here yet: {} : project.add",
                        ptyro_core::config::key_label(&self.keymap.prefix)
                    )),
                }
            }
            "workspace.switch" => self.open_workspaces(),
            "worktree.open" => match self.context().1 {
                Some(wt) => self.open_editors(wt),
                None => self.toast("not in a worktree (sessions started in a project are)"),
            },
            "script.run" => match self.context().1 {
                Some(wt) => self.run_script(wt, None),
                None => self.toast("not in a worktree"),
            },
            "script.pick" => match self.context().1 {
                Some(wt) => {
                    let addr =
                        self.worktree(&wt).map_or_else(|| wt.clone(), WorktreeInfo::qualified);
                    let project = self.worktree(&wt).map(|w| w.project.clone()).unwrap_or_default();
                    self.request(
                        Pending::Scripts(wt),
                        Request::ProjectScripts { addr: project, worktree: Some(addr) },
                    );
                }
                None => self.toast("not in a worktree"),
            },
            "project.add" => {
                self.request(Pending::Discovered, Request::Discover { refresh: false })
            }
            "project.trust" => match self.context().0 {
                Some(p) => self.request(
                    Pending::TrustDialog(AfterTrust::Nothing),
                    Request::ProjectScripts { addr: p, worktree: None },
                ),
                None => self.toast("no project here"),
            },
            "worktree.remove" => match self.context().1.and_then(|w| self.worktree(&w).cloned()) {
                Some(w) if w.is_root => self.toast("that's the main checkout; it can't be removed"),
                Some(w) => self.confirm_remove(&w),
                None => self.toast("not in a worktree"),
            },
            "session.next" => self.cycle(1),
            "session.prev" => self.cycle(-1),
            "session.focus" => {
                let n = cmd.number().unwrap_or(1);
                match self.numbered().get(n.saturating_sub(1)) {
                    Some(id) => self.show(*id, None),
                    None => self.toast(format!("no session {n}")),
                }
            }
            "session.find" => self.open_finder("Show session", Purpose::Show, false),
            "session.last" => {
                match self
                    .panes
                    .get(&self.focused)
                    .and_then(|p| p.last)
                    .filter(|id| self.session(*id).is_some())
                {
                    Some(id) => self.show(id, None),
                    None => self.toast("no previous session in this pane"),
                }
            }
            "session.stop"
            | "session.kill"
            | "session.restart"
            | "session.rename"
            | "session.take_control" => {
                let target = if self.focus == Focus::Sidebar {
                    match self.selected_row() {
                        Some(Row::Session(id)) => Some(id),
                        Some(Row::Worktree(w)) if cmd.action == "session.stop" => {
                            if let Some(w) = self.worktree(&w).cloned().filter(|w| !w.is_root) {
                                self.confirm_remove(&w);
                            }
                            return;
                        }
                        _ => None,
                    }
                } else {
                    focused
                };
                let Some(id) = target else {
                    self.toast("no session here");
                    return;
                };
                self.session_action(cmd.action, id);
            }
            "focus.toggle" => {
                self.focus = match self.focus {
                    Focus::Pane if self.sidebar_area().is_some() => {
                        let rows = self.rows();
                        if let Some(i) =
                            focused.and_then(|id| rows.iter().position(|r| *r == Row::Session(id)))
                        {
                            self.set_sel(&rows, i);
                        }
                        Focus::Sidebar
                    }
                    Focus::Pane => {
                        self.toast(format!(
                            "the sidebar is hidden ({} \\ shows it)",
                            ptyro_core::config::key_label(&self.keymap.prefix)
                        ));
                        Focus::Pane
                    }
                    Focus::Sidebar => Focus::Pane,
                };
            }
            "sidebar.toggle" => {
                self.sidebar = !self.sidebar;
                if !self.sidebar {
                    self.focus = Focus::Pane;
                }
                self.view_changed();
            }
            "pane.split_right" => self.open_finder(
                "Split right: show which session?",
                Purpose::Split(Dir::Right),
                true,
            ),
            "pane.split_down" => {
                self.open_finder("Split down: show which session?", Purpose::Split(Dir::Down), true)
            }
            "pane.close" => {
                if self.layout.close(self.focused) {
                    self.panes.remove(&self.focused);
                    self.focused = self.layout.first();
                    self.zoom = false;
                    self.view_changed();
                } else {
                    self.toast("this is the only pane");
                }
            }
            "pane.zoom" => {
                if self.layout.panes().len() > 1 || self.zoom {
                    self.zoom = !self.zoom;
                    self.view_changed();
                } else {
                    self.toast("nothing to zoom: only one pane");
                }
            }
            "pane.next" | "pane.prev" => {
                let panes = self.layout.panes();
                if let Some(i) = panes.iter().position(|p| *p == self.focused) {
                    let n = panes.len();
                    let j = if cmd.action == "pane.next" { (i + 1) % n } else { (i + n - 1) % n };
                    self.focus_pane(panes[j]);
                }
            }
            "pane.left" | "pane.right" | "pane.up" | "pane.down" => {
                let toward = match cmd.action {
                    "pane.left" => Toward::Left,
                    "pane.right" => Toward::Right,
                    "pane.up" => Toward::Up,
                    _ => Toward::Down,
                };
                if let Some(p) = self.layout.neighbor(self.main_area(), self.focused, toward) {
                    self.focus_pane(p);
                }
            }
            "pane.wider" | "pane.narrower" | "pane.taller" | "pane.shorter" => {
                let (dir, delta) = match cmd.action {
                    "pane.wider" => (Dir::Right, 4),
                    "pane.narrower" => (Dir::Right, -4),
                    "pane.taller" => (Dir::Down, 2),
                    _ => (Dir::Down, -2),
                };
                if self.layout.resize(self.main_area(), self.focused, dir, delta) {
                    self.view_changed();
                } else {
                    self.toast("no split in that direction");
                }
            }
            "copy.enter" => self.enter_copy(CopyStart::Keys { page_up: false }),
            "copy.page_up" => self.enter_copy(CopyStart::Keys { page_up: true }),
            "paste.buffer" => match self.buffer.clone() {
                Some(text) => self.forward(InputEvent::Paste { text }),
                None => self.toast(format!(
                    "nothing yanked yet (copy mode: {} [ … y)",
                    ptyro_core::config::key_label(&self.keymap.prefix)
                )),
            },
            "view.chat" => {
                match self
                    .focused_session()
                    .filter(|id| self.session(*id).is_some_and(|s| s.structured.is_some()))
                {
                    Some(id) => {
                        if !self.chat_terminal.remove(&id) {
                            self.chat_terminal.insert(id);
                        }
                    }
                    None => self.toast(
                        "only structured sessions have a chat view (`ptyro new --structured`)",
                    ),
                }
            }
            "view.redraw" => {
                for id in self.subscribed.drain().collect::<Vec<_>>() {
                    self.send(ClientMsg::Unsubscribe { session: id });
                    self.sizes.remove(&id);
                }
                self.sync();
            }
            "theme.next" => {
                let names = crate::theme::names(&self.config);
                let i = names
                    .iter()
                    .position(|n| *n == self.theme_name)
                    .map_or(0, |i| (i + 1) % names.len());
                self.set_theme(names[i].clone());
            }
            "config.edit" => {
                let argv = vec![
                    std::env::current_exe()
                        .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned()),
                    "config".into(),
                    "edit".into(),
                ];
                self.spawn(argv, Some(Dir::Down), None);
            }
            "nav.down" | "nav.up" | "nav.top" | "nav.bottom" => {
                let rows = self.rows();
                let (n, cur) = (rows.len(), self.sel_index(&rows));
                if n > 0 {
                    let to = match cmd.action {
                        "nav.down" => (cur + 1).min(n - 1),
                        "nav.up" => cur.saturating_sub(1),
                        "nav.top" => 0,
                        _ => n - 1,
                    };
                    self.nav_move(to);
                }
            }
            "nav.select" => match self.selected_row() {
                Some(Row::Session(id)) => {
                    match self.marked.iter().position(|m| *m == id) {
                        Some(i) => {
                            self.marked.remove(i);
                        }
                        None => self.marked.push(id),
                    }
                    if !self.marked.is_empty() {
                        let key = self.keymap.key_for("broadcast").unwrap_or_else(|| "b".into());
                        self.toast(format!(
                            "{} marked: b (or {key}) broadcasts to them",
                            self.marked.len()
                        ));
                    }
                }
                _ => self.run(&Command { action: "nav.open", arg: None }),
            },
            "broadcast" => {
                self.marked
                    .retain(|id| self.sessions.iter().any(|s| s.id == *id && s.state.is_live()));
                if self.marked.is_empty() {
                    self.toast("mark sessions with Space in the sidebar first (Tab focuses it)");
                    return;
                }
                let names: Vec<String> = self
                    .marked
                    .iter()
                    .filter_map(|id| self.session(*id).map(|s| s.name.clone()))
                    .collect();
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Broadcast to {}", names.join(", ")),
                    text: String::new(),
                    kind: PromptKind::Broadcast(self.marked.clone()),
                });
            }
            "help.tour" => self.overlay = Some(Overlay::Tour { page: 0 }),
            "inspector" => {
                if self.inspector.take().is_none() {
                    self.open_inspector();
                }
            }
            "review.queue" => self.request(Pending::ReviewQueue, Request::ReviewQueue),
            "runs" => {
                if self.orch.take().is_none() {
                    self.open_runs();
                }
            }
            "sets" => {
                if self.sets_view.take().is_none() {
                    let run = match self.selected_row() {
                        Some(Row::Set(r)) if self.focus == Focus::Sidebar => Some(r),
                        _ => None,
                    };
                    self.open_sets(run);
                }
            }
            "nav.open" | "nav.collapse" | "nav.expand" => match self.selected_row() {
                Some(Row::Session(id)) if cmd.action == "nav.open" => self.show(id, None),
                Some(Row::Set(run)) if cmd.action == "nav.open" => self.open_sets(Some(run)),
                Some(Row::Project(id) | Row::Worktree(id) | Row::Set(id)) => {
                    let collapse = match cmd.action {
                        "nav.collapse" => true,
                        "nav.expand" => false,
                        _ => !self.collapsed.contains(&id),
                    };
                    if collapse {
                        self.collapsed.insert(id);
                    } else {
                        self.collapsed.remove(&id);
                    }
                }
                _ => {}
            },
            other => self.toast(format!("{other}: not available yet")),
        }
    }

    fn focus_pane(&mut self, p: PaneId) {
        self.focused = p;
        self.focus = Focus::Pane;
        self.view_changed();
    }

    pub fn set_theme(&mut self, name: String) {
        self.theme = Theme::resolve(&self.config, &name, no_color());
        self.toast(format!("theme {name} (set ui.theme in config to keep it)"));
        self.theme_name = name;
    }

    fn session_action(&mut self, action: &str, id: SessionId) {
        let Some(info) = self.session(id).cloned() else { return };
        let addr = id.full();
        match action {
            "session.stop" if !info.state.is_live() => {
                self.request(
                    Pending::Done(format!("removed {}", info.name)),
                    Request::Remove { addr },
                );
            }
            "session.stop" if self.config.ui.confirm_kill => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("Stop {} ({})? [y/N]", info.name, info.argv.join(" ")),
                    action: Confirmed::Stop(id),
                });
            }
            "session.stop" => self.request(
                Pending::Done(format!("stopped {}", info.name)),
                Request::Kill { addr, signal: None },
            ),
            "session.kill" => {
                self.request(
                    Pending::Done(format!("killed {}", info.name)),
                    Request::Kill { addr, signal: Some(9) },
                );
            }
            "session.restart" if info.state.is_live() => {
                self.toast(format!("{} is still running", info.name))
            }
            "session.restart" => self.request(
                Pending::Done(format!("restarted {}", info.name)),
                Request::Restart { addr },
            ),
            "session.rename" => {
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Rename {}", info.name),
                    text: info.name.clone(),
                    kind: PromptKind::Rename(id),
                });
            }
            "session.take_control" => {
                if let Some(size) = self.sizes.get(&id).copied() {
                    self.send(ClientMsg::Resize { session: id, size });
                    // Input makes us the controller even when another client resized last.
                    self.send(ClientMsg::Input {
                        session: id,
                        event: InputEvent::Raw { bytes: Vec::new() },
                    });
                }
            }
            _ => {}
        }
    }

    fn client_env() -> BTreeMap<String, String> {
        ptyro_pty::CLIENT_ENV
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
            .collect()
    }

    /// "Here": the focused session's directory, else the TUI's own.
    fn here(&self) -> std::path::PathBuf {
        self.focused_session()
            .and_then(|id| self.session(id))
            .and_then(|s| s.live_cwd.clone().or_else(|| Some(s.cwd.clone())))
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| "/".into())
    }

    /// A spawn in the focused session's directory (or the current one).
    fn spawn(&mut self, argv: Vec<String>, split: Option<Dir>, name: Option<String>) {
        let spec = SpawnSpec { argv, cwd: self.here(), name, ..SpawnSpec::default() };
        self.spawn_spec(spec, split);
    }

    /// Send a spawn with this client's environment, size and workspace filled in.
    fn spawn_spec(&mut self, mut spec: SpawnSpec, split: Option<Dir>) {
        spec.env = Self::client_env();
        spec.size = spec
            .size
            .or_else(|| self.focused_session().and_then(|id| self.sizes.get(&id).copied()));
        spec.workspace = spec.workspace.or_else(|| Some(self.workspace.clone()));
        if spec.cwd.as_os_str().is_empty() {
            spec.cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        }
        let req = Request::Spawn { spec: spec.clone() };
        self.request(Pending::Spawned { split, spec: Box::new(spec) }, req);
    }

    fn create_worktree(&mut self, spec: WorktreeSpec, split: Option<Dir>) {
        let mut spec = spec;
        spec.env = Self::client_env();
        spec.size = spec
            .size
            .or_else(|| self.focused_session().and_then(|id| self.sizes.get(&id).copied()));
        spec.workspace = spec.workspace.or_else(|| Some(self.workspace.clone()));
        let req = Request::WorktreeNew { spec: spec.clone() };
        self.request(Pending::WorktreeCreated { split, spec: Box::new(spec) }, req);
    }

    // ── the new-session wizard (SPEC §11.5, M3.13) ─────────────────────────

    /// Ask the daemon which profile a spawn there would get (shown in the wizard title).
    fn resolve_for(&mut self, w: &Wizard) {
        self.resolved = None;
        let target = w.worktree.clone().or_else(|| {
            w.project
                .as_ref()
                .and_then(|p| self.worktrees.iter().find(|x| x.project == *p && x.is_root))
                .map(|x| x.id.clone())
        });
        let spec = SpawnSpec {
            agent: w.agent.clone().filter(|a| !a.is_empty()),
            cwd: std::env::current_dir().unwrap_or_else(|_| "/".into()),
            worktree: target,
            workspace: Some(self.workspace.clone()),
            ..SpawnSpec::default()
        };
        self.request(Pending::Resolved, Request::ProfileResolve { spec });
    }

    /// Step one: what to run. The default agent is first, so `n ⏎ ⏎` is the fast path.
    pub fn open_wizard(&mut self, split: Option<Dir>) {
        let (project, worktree) = self.context();
        let w = Wizard { project, worktree, split, ..Wizard::default() };
        self.resolve_for(&w);
        let default = ptyro_agents::default_agent(&self.config);
        let mut agents = ptyro_agents::names(&self.config);
        agents.sort_by_key(|a| *a != default);
        let mut items: Vec<Item> = agents
            .iter()
            .map(|a| Item {
                label: a.clone(),
                detail: if *a == default { "agent (default)".into() } else { "agent".into() },
                key: None,
                target: Target::Agent(a.clone()),
            })
            .collect();
        items.push(Item {
            label: "shell".into(),
            detail: "your $SHELL".into(),
            key: None,
            target: Target::Agent("@shell".into()),
        });
        items.push(Item {
            label: "command…".into(),
            detail: "type a command line".into(),
            key: None,
            target: Target::Agent("@command".into()),
        });
        // Running as another profile is explicit (and may ask first, SPEC §10.10).
        for p in self.profiles.iter().filter(|p| p.defined) {
            items.push(Item {
                label: format!("{default}@{}", p.name),
                detail: format!("{default} as profile {}", p.label),
                key: None,
                target: Target::Agent(format!("{default}@{}", p.name)),
            });
        }
        let title = wizard_title(&w, &self.projects, &self.worktrees, None);
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Agent(w),
        });
    }

    /// Step two: where. A new worktree of the current project first.
    fn open_targets(&mut self, w: Wizard) {
        let mut items = Vec::new();
        if let Some(p) = w.project.as_ref().and_then(|p| self.project(p)) {
            items.push(Item {
                label: format!("+ new worktree in {}", p.slug),
                detail: "an isolated checkout on a new branch, random name".into(),
                key: None,
                target: Target::NewWorktree(p.id.clone()),
            });
        }
        let mut wts: Vec<&WorktreeInfo> = self
            .worktrees
            .iter()
            .filter(|x| {
                x.state == WorktreeState::Ready && self.tree().project_ids().contains(&x.project)
            })
            .collect();
        // The context worktree first, then its project's, then the rest.
        wts.sort_by_key(|x| {
            (Some(&x.id) != w.worktree.as_ref(), Some(&x.project) != w.project.as_ref(), !x.is_root)
        });
        for x in wts {
            let branch = x.branch.clone().unwrap_or_default();
            items.push(Item {
                label: x.qualified(),
                detail: branch,
                key: None,
                target: Target::Worktree(x.id.clone()),
            });
        }
        let here = self.here().display().to_string();
        items.push(Item { label: "here".into(), detail: here, key: None, target: Target::Here });
        let title = wizard_title(&w, &self.projects, &self.worktrees, self.resolved.as_deref());
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Target(w),
        });
    }

    fn prompt_command(&mut self, w: Wizard) {
        let place = w
            .worktree
            .as_ref()
            .and_then(|x| self.worktree(x))
            .map(|x| format!(" in {}", x.qualified()))
            .unwrap_or_default();
        self.overlay = Some(Overlay::Prompt {
            title: format!("New session{place} — command (empty: your shell)"),
            text: String::new(),
            kind: PromptKind::Command(w),
        });
    }

    /// Wizard finished: start it.
    fn wizard_done(&mut self, w: Wizard, target: Target) {
        let agent = w.agent.clone();
        match target {
            Target::NewWorktree(project) => {
                let spec = WorktreeSpec {
                    project: project.clone(),
                    name: w.name.clone().filter(|n| !n.is_empty()),
                    setup: true,
                    agent: agent.filter(|a| !a.starts_with('@')),
                    ..WorktreeSpec::default()
                };
                let req = Request::ProjectScripts { addr: project, worktree: None };
                self.request(Pending::PreCreate(Box::new(spec), w.split), req);
            }
            Target::Worktree(id) => self.start_in(w, Some(id)),
            Target::Here => self.start_in(w, None),
            _ => {}
        }
    }

    fn start_in(&mut self, w: Wizard, worktree: Option<String>) {
        match w.agent.as_deref() {
            Some("@command") => self.prompt_command(Wizard { worktree, ..w }),
            Some("@shell") | None => {
                let cwd = if worktree.is_some() { Default::default() } else { self.here() };
                self.spawn_spec(SpawnSpec { cwd, worktree, ..SpawnSpec::default() }, w.split);
            }
            Some(a) => {
                let cwd = if worktree.is_some() { Default::default() } else { self.here() };
                self.spawn_spec(
                    SpawnSpec { agent: Some(a.to_owned()), cwd, worktree, ..SpawnSpec::default() },
                    w.split,
                );
            }
        }
    }

    // ── workspaces, editors, scripts, projects ─────────────────────────────

    fn open_workspaces(&mut self) {
        let mut items: Vec<Item> = self
            .workspaces
            .iter()
            .map(|w| {
                let n = self.sessions.iter().filter(|s| self.home_of(s) == w.name).count();
                let current = if w.name == self.workspace { " · current" } else { "" };
                Item {
                    label: w.name.clone(),
                    detail: format!(
                        "[{}] {} project(s), {n} session(s){current}",
                        self.profile_label(&w.profile),
                        w.projects.len()
                    ),
                    key: None,
                    target: Target::Workspace(w.name.clone()),
                }
            })
            .collect();
        items.push(Item {
            label: "+ new workspace".into(),
            detail: String::new(),
            key: None,
            target: Target::NewWorkspace,
        });
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Workspaces", items),
            purpose: Purpose::Workspace,
        });
    }

    fn open_editors(&mut self, wt: String) {
        let default = self.config.default_editor();
        let mut names = self.config.editor_names();
        names.sort_by_key(|n| *n != default);
        let items = names
            .into_iter()
            .filter_map(|n| {
                let def = self.config.editor(&n)?;
                let how =
                    if def.inside { "opens here, as a session" } else { "opens its own window" };
                Some(Item {
                    label: n.clone(),
                    detail: format!("{} — {how}", def.command.join(" ")),
                    key: None,
                    target: Target::Editor(n),
                })
            })
            .collect();
        let title = format!(
            "Open {} in",
            self.worktree(&wt).map_or_else(String::new, WorktreeInfo::qualified)
        );
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Editor(wt),
        });
    }

    fn open_editor(&mut self, wt: &str, name: &str) {
        let (Some(def), Some(w)) = (self.config.editor(name), self.worktree(wt).cloned()) else {
            return;
        };
        let path = w.path.to_string_lossy().into_owned();
        let mut argv: Vec<String> =
            def.command.iter().map(|a| a.replace("{path}", &path)).collect();
        if !def.command.iter().any(|a| a.contains("{path}")) {
            argv.push(path);
        }
        if def.inside {
            self.spawn_spec(
                SpawnSpec {
                    argv,
                    worktree: Some(w.id.clone()),
                    confirmed: true,
                    ..SpawnSpec::default()
                },
                None,
            );
            return;
        }
        let Some((prog, args)) = argv.split_first() else { return };
        match std::process::Command::new(prog)
            .args(args)
            .current_dir(&w.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => self.toast(format!("opened {} in {name}", w.qualified())),
            Err(e) => self.toast_err(format!("{prog}: {e}")),
        }
    }

    fn run_script(&mut self, wt: String, script: Option<String>) {
        let size = self.focused_session().and_then(|id| self.sizes.get(&id).copied());
        let addr = wt.clone();
        let req = Request::WorktreeRun { addr, script: script.clone(), size };
        let Some(client) = self.client().cloned() else { return };
        let tx = self.tx.clone();
        let project = self.worktree(&wt).map(|w| w.project.clone()).unwrap_or_default();
        tokio::spawn(async move {
            let res = client.request(req).await;
            let msg = match res {
                Err(e) if e.code == ErrorCode::UntrustedProject => AppMsg::Reply(
                    Pending::TrustDialog(AfterTrust::Run(wt.clone(), script)),
                    client
                        .request(Request::ProjectScripts { addr: project, worktree: Some(wt) })
                        .await,
                ),
                other => {
                    AppMsg::Reply(Pending::Spawned { split: None, spec: Box::default() }, other)
                }
            };
            let _ = tx.send(msg);
        });
    }

    fn open_script_picker(&mut self, wt: String, scripts: ScriptsInfo) {
        if scripts.names.is_empty() {
            self.toast("no scripts in .ptyro.toml");
            return;
        }
        let items = scripts
            .names
            .iter()
            .map(|n| Item {
                label: n.trim_start_matches("run:").to_owned(),
                detail: if n.starts_with("run:") {
                    "run script".into()
                } else {
                    "lifecycle script".into()
                },
                key: None,
                target: Target::Script(n.clone()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Run script", items),
            purpose: Purpose::Script(wt),
        });
    }

    fn after_trust(&mut self, then: AfterTrust) {
        match then {
            AfterTrust::Create(spec, split) => self.create_worktree(*spec, split),
            AfterTrust::Setup(wt) => self.run_script(wt, Some("setup".into())),
            AfterTrust::Run(wt, script) => self.run_script(wt, script),
            AfterTrust::Nothing => self.toast("scripts trusted"),
        }
    }

    fn open_add_project(&mut self, paths: Vec<std::path::PathBuf>) {
        let mut items = Vec::new();
        if let Ok(here) = std::env::current_dir() {
            items.push(Item {
                label: "here".into(),
                detail: here.display().to_string(),
                key: None,
                target: Target::Path(here),
            });
        }
        for p in paths.into_iter().filter(|p| !self.projects.iter().any(|x| x.root == *p)) {
            let label = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            items.push(Item {
                label,
                detail: p.display().to_string(),
                key: None,
                target: Target::Path(p),
            });
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(format!("Add a project to {}", self.workspace), items),
            purpose: Purpose::AddProject,
        });
    }

    fn confirm_remove(&mut self, w: &WorktreeInfo) {
        let live = self
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(w.id.as_str()) && s.state.is_live())
            .count();
        let note = if live > 0 {
            format!(" Its {live} running session(s) will be stopped.")
        } else {
            String::new()
        };
        self.overlay = Some(Overlay::Confirm {
            text: format!(
                "Remove worktree {} ({})?{note} [y/N]",
                w.qualified(),
                w.branch.as_deref().unwrap_or("-")
            ),
            action: Confirmed::RemoveWorktree { id: w.id.clone(), force: false },
        });
    }

    fn session_items(&self, include_new: bool) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        if include_new {
            items.push(Item {
                label: "+ new shell".into(),
                detail: "start your shell in a new session".into(),
                key: None,
                target: Target::NewShell,
            });
        }
        let numbered = self.numbered();
        let mut order: Vec<&SessionInfo> =
            self.ordered().iter().filter_map(|id| self.session(*id)).collect();
        // Sessions of other workspaces stay findable, after this one's.
        let rest: Vec<&SessionInfo> =
            self.sessions.iter().filter(|s| !order.iter().any(|o| o.id == s.id)).collect();
        order.extend(rest);
        for s in order {
            let i = numbered.iter().position(|id| *id == s.id).unwrap_or(usize::MAX);
            let what = s.foreground.clone().unwrap_or_else(|| s.argv.join(" "));
            let status =
                s.program_status.as_ref().map(|p| format!(" · {}", p.state)).unwrap_or_default();
            items.push(Item {
                label: s.name.clone(),
                detail: format!("{what} · {}{status}", s.state.label()),
                key: (i < 9).then(|| (i + 1).to_string()),
                target: Target::Session(s.id),
            });
        }
        items
    }

    fn open_finder(&mut self, title: &str, purpose: Purpose, include_new: bool) {
        let items = self.session_items(include_new);
        self.overlay = Some(Overlay::Picker { palette: Palette::new(title, items), purpose });
    }

    fn open_palette(&mut self) {
        let mut items: Vec<Item> = keymap::ACTIONS
            .iter()
            .filter(|a| a.arg == keymap::Arg::None && !a.id.starts_with("nav."))
            .map(|a| Item {
                label: a.id.into(),
                detail: a.desc.into(),
                key: self.keymap.key_for(a.id),
                target: Target::Action(Command { action: a.id, arg: None }),
            })
            .collect();
        for s in self.session_items(false) {
            items.push(Item { label: format!("go {}", s.label), ..s });
        }
        for w in &self.workspaces {
            if w.name != self.workspace {
                items.push(Item {
                    label: format!("workspace {}", w.name),
                    detail: format!("switch to it [{}]", self.profile_label(&w.profile)),
                    key: None,
                    target: Target::Workspace(w.name.clone()),
                });
            }
        }
        for t in crate::theme::names(&self.config) {
            items.push(Item {
                label: format!("theme {t}"),
                detail: if t == self.theme_name {
                    "current theme".into()
                } else {
                    "switch theme".into()
                },
                key: None,
                target: Target::Theme(t),
            });
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Command palette", items),
            purpose: Purpose::Palette,
        });
    }

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else { return };
        match overlay {
            Overlay::Inbox { selected, tab } => self.inbox_key(key, selected, tab),
            Overlay::Peek { session, screen, reply } => self.peek_key(key, session, screen, reply),
            Overlay::Tour { page } => {
                let last = crate::ui::tour_pages(self).len() - 1;
                let page = match key.code {
                    KeyCode::Esc | KeyCode::Char { c: 'q' } => return,
                    KeyCode::Left | KeyCode::Backspace | KeyCode::Char { c: 'h' } => {
                        page.saturating_sub(1)
                    }
                    _ if page == last => return,
                    _ => page + 1,
                };
                self.overlay = Some(Overlay::Tour { page });
            }
            Overlay::ReviewQueue { items, mut selected } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char { c: 'q' } => return,
                    KeyCode::Down | KeyCode::Char { c: 'j' } => {
                        selected = (selected + 1).min(items.len().saturating_sub(1))
                    }
                    KeyCode::Up | KeyCode::Char { c: 'k' } => selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        if let Some(it) = items.get(selected) {
                            self.inspect(it.worktree.id.clone());
                        }
                        return;
                    }
                    KeyCode::Char { c: 'd' } => {
                        if let Some(it) = items.get(selected) {
                            self.request(
                                Pending::Done(format!("{} reviewed", it.worktree.qualified())),
                                Request::ReviewDone { addr: it.worktree.id.clone() },
                            );
                        }
                        return;
                    }
                    _ => {}
                }
                self.overlay = Some(Overlay::ReviewQueue { items, selected });
            }
            Overlay::Explain { session, lines, mut scroll } => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char { c: 'q' } => return,
                    KeyCode::Down | KeyCode::Char { c: 'j' } => scroll += 1,
                    KeyCode::Up | KeyCode::Char { c: 'k' } => scroll = scroll.saturating_sub(1),
                    _ => {}
                }
                let scroll = scroll.min(lines.len().saturating_sub(self.overlay_body_h(2)));
                self.overlay = Some(Overlay::Explain { session, lines, scroll });
            }
            Overlay::Help { mut filter, mut scroll } => {
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Char { c: 'q' } if filter.is_empty() => return,
                    KeyCode::Char { c: 'u' } if key.mods.has(Mods::CTRL) => filter.clear(),
                    KeyCode::Down => scroll += 1,
                    KeyCode::Up => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll += 10,
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::Backspace => {
                        filter.pop();
                        scroll = 0;
                    }
                    KeyCode::Char { c } if !key.mods.has(Mods::CTRL) => {
                        filter.push(c);
                        scroll = 0;
                    }
                    _ => {}
                }
                let max = crate::ui::help_lines(self, &filter)
                    .len()
                    .saturating_sub(self.overlay_body_h(2));
                self.overlay = Some(Overlay::Help { filter, scroll: scroll.min(max) });
            }
            Overlay::Picker { mut palette, purpose } => match palette.key(key) {
                Picked::Stay => self.overlay = Some(Overlay::Picker { palette, purpose }),
                Picked::Close => {}
                Picked::Pick(target) => self.picked(target, purpose),
            },
            Overlay::Prompt { title, mut text, kind } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.prompt_done(text, kind),
                KeyCode::Backspace => {
                    text.pop();
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                KeyCode::Char { c: 'u' } if key.mods.has(Mods::CTRL) => {
                    self.overlay = Some(Overlay::Prompt { title, text: String::new(), kind });
                }
                KeyCode::Char { c } if !key.mods.has(Mods::CTRL) => {
                    text.push(c);
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                _ => self.overlay = Some(Overlay::Prompt { title, text, kind }),
            },
            Overlay::Confirm { action, text } => match (key.code, &action) {
                // "Keep the pin? [Y/n]": Enter keeps it.
                (KeyCode::Enter, Confirmed::KeepPin(_)) => {}
                (KeyCode::Char { c: 'n' | 'N' }, Confirmed::KeepPin(id)) => {
                    let id = id.clone();
                    self.request(
                        Pending::Done("unpinned".into()),
                        Request::ProjectPin { addr: id, profile: None },
                    );
                }
                (KeyCode::Char { c: 'y' | 'Y' }, _) => self.confirmed(action),
                (KeyCode::Char { c: 'n' | 'N' } | KeyCode::Esc | KeyCode::Enter, _) => {}
                _ => self.overlay = Some(Overlay::Confirm { action, text }),
            },
            Overlay::Trust { scripts, then, mut scroll } => match key.code {
                KeyCode::Char { c: 'y' | 'Y' } => {
                    let req = Request::ProjectTrust {
                        addr: scripts.project.clone(),
                        fingerprint: scripts.fingerprint.clone(),
                    };
                    self.request(Pending::Trusted(then), req);
                }
                KeyCode::Char { c: 'n' | 'N' } | KeyCode::Esc | KeyCode::Char { c: 'q' } => {
                    if let AfterTrust::Create(mut spec, split) = then {
                        // Still make the worktree, just without running its scripts.
                        spec.setup = false;
                        self.create_worktree(*spec, split);
                    }
                    self.toast(format!(
                        "not trusted; `ptyro project trust {}` later",
                        scripts.project
                    ));
                }
                KeyCode::Down | KeyCode::Char { c: 'j' } => {
                    // Three header lines above the scripts.
                    let max =
                        (scripts.text.lines().count() + 3).saturating_sub(self.overlay_body_h(4));
                    scroll = (scroll + 1).min(max);
                    self.overlay = Some(Overlay::Trust { scripts, then, scroll });
                }
                KeyCode::Up | KeyCode::Char { c: 'k' } => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Some(Overlay::Trust { scripts, then, scroll });
                }
                _ => self.overlay = Some(Overlay::Trust { scripts, then, scroll }),
            },
        }
    }

    /// Text rows inside a full-height overlay box (help, explain, trust), less `chrome` rows.
    fn overlay_body_h(&self, chrome: u16) -> usize {
        let r = crate::ui::boxed(self.area, 0, self.area.height.saturating_sub(4));
        r.height.saturating_sub(chrome) as usize
    }

    fn confirmed(&mut self, action: Confirmed) {
        match action {
            Confirmed::Send(req, toast) => self.request(Pending::Done(toast), *req),
            Confirmed::Integrate(wt) => self.request(
                Pending::Done(String::new()),
                Request::WorktreeIntegrate {
                    addr: wt,
                    into: None,
                    cherry_pick: false,
                    push: false,
                },
            ),
            Confirmed::Stop(id) => {
                let name = self.session(id).map(|s| s.name.clone()).unwrap_or_default();
                self.request(
                    Pending::Done(format!("stopped {name}")),
                    Request::Kill { addr: id.full(), signal: None },
                );
            }
            Confirmed::Spawn(mut spec, split) => {
                spec.override_pin = true;
                spec.confirmed = true;
                self.spawn_spec(*spec, split);
            }
            Confirmed::Worktree(mut spec, split) => {
                spec.override_pin = true;
                spec.confirmed = true;
                self.create_worktree(*spec, split);
            }
            Confirmed::RemoveWorktree { id, force } => {
                let opts = RemoveOpts { force, stop_sessions: true, keep_branch: false };
                self.request(
                    Pending::Removed(id.clone()),
                    Request::WorktreeRemove { addr: id, opts },
                );
            }
            Confirmed::KeepPin(_) => {}
        }
    }

    fn picked(&mut self, target: Target, purpose: Purpose) {
        let split = match purpose {
            Purpose::Split(d) => Some(d),
            _ => None,
        };
        match (target, purpose) {
            (Target::Agent(a), Purpose::Agent(mut w)) => {
                w.agent = (a != "@shell").then_some(a);
                if w.name.is_some() {
                    // `N`: the target is the new worktree.
                    let project = w.project.clone().unwrap_or_default();
                    self.wizard_done(w, Target::NewWorktree(project));
                } else {
                    self.resolve_for(&w);
                    self.open_targets(w);
                }
            }
            (
                t @ (Target::NewWorktree(_) | Target::Worktree(_) | Target::Here),
                Purpose::Target(w),
            ) => self.wizard_done(w, t),
            (Target::Workspace(name), _) => self.switch_workspace(name),
            (Target::NewWorkspace, _) => {
                self.overlay = Some(Overlay::Prompt {
                    title: "New workspace — name".into(),
                    text: String::new(),
                    kind: PromptKind::NewWorkspace,
                });
            }
            (Target::Editor(name), Purpose::Editor(wt)) => self.open_editor(&wt, &name),
            (Target::Script(name), Purpose::Script(wt)) => self.run_script(wt, Some(name)),
            (Target::Path(path), _) => {
                let workspace = Some(self.workspace.clone());
                self.request(Pending::ProjectAdded, Request::ProjectAdd { path, workspace });
            }
            (Target::Action(cmd), _) => self.run(&cmd),
            (Target::Session(id), _) => self.show(id, split),
            (Target::Theme(name), _) => self.set_theme(name),
            (Target::NewShell, _) => self.spawn(Vec::new(), split, None),
            _ => {}
        }
    }

    fn prompt_done(&mut self, text: String, kind: PromptKind) {
        match kind {
            PromptKind::Command(w) => {
                let argv: Vec<String> = if text.trim().is_empty() {
                    Vec::new()
                } else {
                    // Run through the shell so pipes, quotes and $VARS work as typed.
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                    vec![shell, "-c".into(), text.trim().to_owned()]
                };
                if w.worktree.is_some() {
                    self.spawn_spec(
                        SpawnSpec { argv, worktree: w.worktree, ..SpawnSpec::default() },
                        w.split,
                    );
                } else {
                    self.spawn(argv, w.split, None);
                }
            }
            PromptKind::WorktreeName(mut w) => {
                let name = text.trim().to_owned();
                if !name.is_empty()
                    && let Err(e) = ptyro_core::names::validate(&name)
                {
                    self.toast_err(e);
                    return;
                }
                w.name = Some(name);
                // Then: which agent (shell for none).
                let mut items: Vec<Item> = ptyro_agents::names(&self.config)
                    .into_iter()
                    .map(|a| Item {
                        label: a.clone(),
                        detail: "agent".into(),
                        key: None,
                        target: Target::Agent(a),
                    })
                    .collect();
                let default = ptyro_agents::default_agent(&self.config);
                items.sort_by_key(|i| i.label != default);
                items.push(Item {
                    label: "no agent".into(),
                    detail: "just the worktree (and its setup)".into(),
                    key: None,
                    target: Target::Agent("@shell".into()),
                });
                let title =
                    wizard_title(&w, &self.projects, &self.worktrees, self.resolved.as_deref());
                self.overlay = Some(Overlay::Picker {
                    palette: Palette::new(title, items),
                    purpose: Purpose::Agent(w),
                });
            }
            PromptKind::ReviewComment { worktree, path, line, code } => {
                if !text.trim().is_empty() {
                    let req = Request::ReviewAdd {
                        addr: worktree.clone(),
                        path,
                        line,
                        code: (!code.trim().is_empty()).then_some(code),
                        text,
                    };
                    self.request(Pending::InspectComments(worktree), req);
                }
            }
            PromptKind::CommitMessage { worktree, paths } => {
                if !text.trim().is_empty() {
                    let all = paths.is_empty();
                    self.request(
                        Pending::Done(String::new()),
                        Request::WorktreeCommit { addr: worktree, message: text, paths, all },
                    );
                    if let Some(i) = &mut self.inspector {
                        i.marked.clear();
                    }
                }
            }
            PromptKind::Orch(input) => self.orch_input(input, text),
            PromptKind::Sets(input) => self.sets_input(input, text),
            PromptKind::NewWorkspace => {
                let name = text.trim().to_owned();
                if !name.is_empty() {
                    let req =
                        Request::WorkspaceNew { name: name.clone(), profile: None, notify: None };
                    self.request(Pending::Done(format!("workspace {name} created")), req);
                    self.switch_workspace(name);
                }
            }
            PromptKind::Broadcast(ids) => {
                if text.trim().is_empty() {
                    return;
                }
                for id in &ids {
                    let req = Request::SendText {
                        addr: id.full(),
                        text: text.clone(),
                        enter: true,
                        // Each gets it once it's ready (never typed into a dialog).
                        wait_ready_ms: Some(120_000),
                    };
                    self.request(Pending::Done(String::new()), req);
                }
                self.toast(format!("sent to {} session(s), each once it's ready", ids.len()));
                self.marked.clear();
            }
            PromptKind::Rename(id) => {
                let name = text.trim().to_owned();
                if !name.is_empty() {
                    self.request(
                        Pending::Done(format!("renamed to {name}")),
                        Request::Rename { addr: id.full(), name },
                    );
                }
            }
        }
    }

    // ── copy mode ───────────────────────────────────────────────────────────

    fn enter_copy(&mut self, then: CopyStart) {
        let pane = self.focused;
        let Some(id) = self.panes.get(&pane).and_then(|p| p.session) else { return };
        if let Some(p) = self.panes.get_mut(&pane) {
            if p.loading {
                return;
            }
            p.loading = true;
        }
        self.request(
            Pending::Scrollback { pane, session: id, then },
            Request::ReadScrollback { addr: id.full(), lines: None },
        );
    }

    fn start_copy(&mut self, pane: PaneId, doc: &ScrollbackDoc, then: CopyStart) {
        let height = self
            .pane_rects()
            .iter()
            .find(|(p, _)| *p == pane)
            .map_or(doc.size.rows as usize, |(_, r)| self.inner(*r).height as usize);
        let Some(p) = self.panes.get_mut(&pane) else { return };
        p.loading = false;
        let mut c = CopyMode::new(doc, height);
        match then {
            CopyStart::Keys { page_up: true } => c.scroll(-(height as isize)),
            CopyStart::Keys { page_up: false } => {}
            CopyStart::Wheel(n) => c.scroll(-(n as isize)),
            CopyStart::Drag { x, y } => {
                // The same base the rest of the drag uses (`top` follows the live screen's pan).
                let anchor = Pos { row: c.top + y as usize, col: x as usize };
                c.select_from(anchor, SelMode::Char);
                c.move_cursor(anchor);
            }
        }
        p.copy = Some(c);
        self.dirty = true;
    }

    fn copy_key(&mut self, key: KeyEvent) {
        let Some(c) = self.panes.get_mut(&self.focused).and_then(|p| p.copy.as_mut()) else {
            return;
        };
        match c.key(key) {
            CopyOutcome::Stay => {}
            CopyOutcome::MoreHistory => {
                let pane = self.focused;
                if let Some(id) = self.panes.get(&pane).and_then(|p| p.session) {
                    self.request(
                        Pending::DeepScrollback(pane, id),
                        Request::ReadScrollback { addr: id.full(), lines: Some(1_000_000) },
                    );
                }
            }
            CopyOutcome::Exit => self.exit_copy(),
            CopyOutcome::Yank(text) => {
                self.yank(text);
                self.exit_copy();
            }
        }
    }

    fn exit_copy(&mut self) {
        if let Some(p) = self.panes.get_mut(&self.focused) {
            p.copy = None;
        }
        self.dirty = true;
    }

    fn yank(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        crate::osc52(&text);
        let n = text.chars().count();
        self.buffer = Some(text);
        self.toast(format!(
            "copied {n} characters ({} ] pastes)",
            ptyro_core::config::key_label(&self.keymap.prefix)
        ));
    }

    // ── mouse ───────────────────────────────────────────────────────────────

    fn pane_at(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.pane_rects().into_iter().find(|(_, r)| r.contains((x, y).into()))
    }

    pub fn on_mouse(&mut self, m: &crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind as K;
        self.dirty = true;
        let (x, y) = (m.column, m.row);
        if self.overlay.is_some() {
            if matches!(m.kind, K::Down(_)) {
                self.overlay = None;
            }
            return;
        }
        // Dragging a divider or a selection continues wherever the mouse goes.
        match (&self.drag, m.kind) {
            (Some(Drag::Divider(d)), K::Drag(_)) => {
                let d = d.clone();
                self.layout.drag(&d, x, y);
                self.view_changed();
                return;
            }
            (Some(Drag::Divider(_)), K::Up(_)) => {
                self.drag = None;
                return;
            }
            (Some(Drag::Select { pane, started }), K::Drag(_)) => {
                let (pane, started) = (*pane, *started);
                self.drag_select(pane, x, y, started);
                return;
            }
            (Some(Drag::Select { pane, .. }), K::Up(_)) => {
                let pane = *pane;
                self.drag = None;
                let text = self
                    .panes
                    .get(&pane)
                    .and_then(|p| p.copy.as_ref())
                    .filter(|c| c.selection.is_some())
                    .map(CopyMode::selected_text);
                if let Some(text) = text {
                    self.focused = pane;
                    self.yank(text);
                    self.exit_copy();
                }
                return;
            }
            _ => {}
        }
        if let Some(side) = self.sidebar_area()
            && side.contains((x, y).into())
        {
            match m.kind {
                K::Down(_) => {
                    let rows = self.rows();
                    let skip = self.sidebar_skip(side, self.sel_index(&rows));
                    // The title line has no row.
                    let row = y.checked_sub(side.y + 1).map(|r| r as usize + skip);
                    let row = row.unwrap_or(usize::MAX);
                    match rows.get(row).cloned() {
                        Some(Row::Session(id)) => {
                            self.set_sel(&rows, row);
                            self.show(id, None);
                        }
                        Some(Row::Set(run)) => {
                            self.set_sel(&rows, row);
                            self.open_sets(Some(run));
                        }
                        Some(Row::Project(id) | Row::Worktree(id)) => {
                            self.set_sel(&rows, row);
                            if !self.collapsed.remove(&id) {
                                self.collapsed.insert(id);
                            }
                        }
                        _ => {}
                    }
                }
                K::ScrollDown => {
                    let rows = self.rows();
                    let cur = self.sel_index(&rows);
                    self.nav_move((cur + 1).min(rows.len().saturating_sub(1)));
                }
                K::ScrollUp => {
                    let cur = self.sel_index(&self.rows());
                    self.nav_move(cur.saturating_sub(1));
                }
                _ => {}
            }
            return;
        }
        if matches!(m.kind, K::Down(crossterm::event::MouseButton::Left)) && !self.zoom {
            let hit = self
                .layout
                .dividers(self.main_area())
                .into_iter()
                .find(|d| d.rect.contains((x, y).into()));
            if let Some(d) = hit {
                self.drag = Some(Drag::Divider(d));
                return;
            }
        }
        let Some((pane, rect)) = self.pane_at(x, y) else { return };
        let inner = self.inner(rect);
        if matches!(m.kind, K::Down(_)) && pane != self.focused {
            self.focus_pane(pane);
        }
        self.focus = Focus::Pane;
        if !inner.contains((x, y).into()) {
            return; // the header
        }
        let (cx, cy) = (x - inner.x, y - inner.y);
        let copying = self.panes.get(&pane).is_some_and(|p| p.copy.is_some());
        let wants_mouse = self
            .panes
            .get(&pane)
            .and_then(|p| p.session)
            .and_then(|id| self.caches.get(&id))
            .is_some_and(|c| c.modes.mouse);
        if wants_mouse && !copying {
            let mut mouse = crate::keys::mouse(m);
            mouse.x = cx;
            mouse.y = cy;
            self.forward(InputEvent::Mouse { mouse });
            return;
        }
        match m.kind {
            K::ScrollUp => {
                if let Some(c) = self.panes.get_mut(&pane).and_then(|p| p.copy.as_mut()) {
                    c.scroll(-3);
                } else {
                    self.focused = pane;
                    self.enter_copy(CopyStart::Wheel(3));
                }
            }
            K::ScrollDown => {
                if let Some(c) = self.panes.get_mut(&pane).and_then(|p| p.copy.as_mut()) {
                    c.scroll(3);
                    // Scrolled back to the live screen without a selection: leave copy mode.
                    if c.at_bottom() && c.selection.is_none() {
                        self.focused = pane;
                        self.exit_copy();
                    }
                }
            }
            K::Down(crossterm::event::MouseButton::Left) => {
                if let Some(c) = self.panes.get_mut(&pane).and_then(|p| p.copy.as_mut()) {
                    let p = Pos { row: c.top + cy as usize, col: cx as usize };
                    c.selection = None;
                    c.move_cursor(p);
                }
                self.drag = Some(Drag::Select { pane, started: false });
            }
            _ => {}
        }
    }

    fn drag_select(&mut self, pane: PaneId, x: u16, y: u16, started: bool) {
        let Some((_, rect)) = self.pane_rects().into_iter().find(|(p, _)| *p == pane) else {
            return;
        };
        let inner = self.inner(rect);
        let cx = x.clamp(inner.x, inner.right().saturating_sub(1)) - inner.x;
        let cy = y.clamp(inner.y, inner.bottom().saturating_sub(1)) - inner.y;
        let has_copy = self.panes.get(&pane).is_some_and(|p| p.copy.is_some());
        if !has_copy {
            if !started {
                self.focused = pane;
                self.enter_copy(CopyStart::Drag { x: cx, y: cy });
                self.drag = Some(Drag::Select { pane, started: true });
            }
            return;
        }
        if let Some(c) = self.panes.get_mut(&pane).and_then(|p| p.copy.as_mut()) {
            // Dragging past the top or bottom edge scrolls.
            if y < inner.y {
                c.scroll(-1);
            } else if y >= inner.bottom() {
                c.scroll(1);
            }
            if c.selection.is_none() {
                let anchor = c.cursor;
                c.select_from(anchor, SelMode::Char);
            }
            let p = Pos { row: c.top + cy as usize, col: cx as usize };
            c.move_cursor(p);
        }
        self.drag = Some(Drag::Select { pane, started: true });
    }

    // ── workspaces, projects, worktrees ─────────────────────────────────────

    pub fn tree(&self) -> tree::Inputs<'_> {
        tree::Inputs {
            workspace: &self.workspace,
            workspaces: &self.workspaces,
            projects: &self.projects,
            worktrees: &self.worktrees,
            sessions: &self.sessions,
            collapsed: &self.collapsed,
            sets: &self.set_nodes,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.tree().rows()
    }

    /// The workspace's sessions in tree order (collapsed ones included).
    pub fn ordered(&self) -> Vec<SessionId> {
        self.tree().sessions()
    }

    /// Visible session rows, in order: what `1`–`9` pick.
    pub fn numbered(&self) -> Vec<SessionId> {
        Self::numbered_from(&self.rows())
    }

    pub fn numbered_from(rows: &[Row]) -> Vec<SessionId> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Session(id) => Some(*id),
                _ => None,
            })
            .collect()
    }

    pub fn worktree(&self, id: &str) -> Option<&WorktreeInfo> {
        self.worktrees.iter().find(|w| w.id == id)
    }

    pub fn project(&self, id: &str) -> Option<&ProjectInfo> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn profile(&self, name: &str) -> Option<&ProfileInfo> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn profile_label(&self, name: &str) -> String {
        self.profile(name).map_or_else(|| name.to_owned(), |p| p.label.clone())
    }

    pub fn workspace_profile(&self) -> String {
        self.workspaces
            .iter()
            .find(|w| w.name == self.workspace)
            .map_or_else(|| ptyro_core::model::DEFAULT_PROFILE.into(), |w| w.profile.clone())
    }

    /// The worktree (and project) a new session would go in: the selected sidebar row's, else
    /// the focused session's, else the workspace's first project's main checkout.
    pub fn context(&self) -> (Option<String>, Option<String>) {
        if self.focus == Focus::Sidebar
            && let Some(r) = self.selected_row()
        {
            match &r {
                Row::Project(p) => {
                    let root = self
                        .worktrees
                        .iter()
                        .find(|w| w.project == *p && w.is_root)
                        .map(|w| w.id.clone());
                    return (Some(p.clone()), root);
                }
                Row::Worktree(w) => {
                    return (self.worktree(w).map(|x| x.project.clone()), Some(w.clone()));
                }
                Row::Session(id) => {
                    if let Some(s) = self.session(*id) {
                        return (s.project.clone(), s.worktree.clone());
                    }
                }
                Row::Group(_) | Row::Set(_) => return (None, None),
            }
        }
        if let Some(s) = self.focused_session().and_then(|id| self.session(id))
            && s.project.is_some()
        {
            return (s.project.clone(), s.worktree.clone());
        }
        let first = self.tree().project_ids().into_iter().next();
        let root = first
            .as_ref()
            .and_then(|p| self.worktrees.iter().find(|w| w.project == *p && w.is_root))
            .map(|w| w.id.clone());
        (first, root)
    }

    pub fn switch_workspace(&mut self, name: String) {
        if name == self.workspace {
            return;
        }
        self.save_view();
        self.workspace = name;
        self.sidebar_row = None;
        self.sidebar_sel = 0;
        let Some(client) = self.client.clone() else { return };
        let name = self.workspace.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = client.request(Request::LoadView { name: name.clone() }).await;
            let _ = tx.send(AppMsg::View(name, res));
        });
        self.toast(format!("workspace {}", self.workspace));
    }

    /// Apply a loaded workspace view (or a fresh layout).
    pub fn apply_view(&mut self, view: Option<serde_json::Value>) {
        self.layout = Layout::default();
        self.panes = HashMap::from([(1, PaneState::default())]);
        self.focused = 1;
        self.zoom = false;
        let ordered = self.ordered();
        if let Some(saved) = view.and_then(|v| serde_json::from_value::<SavedView>(v).ok()) {
            self.layout = saved.layout.sanitize();
            self.panes = self
                .layout
                .panes()
                .into_iter()
                .map(|p| {
                    let session = saved
                        .panes
                        .get(&p)
                        .cloned()
                        .flatten()
                        .and_then(|s| s.parse::<SessionId>().ok())
                        .filter(|id| ordered.contains(id));
                    (p, PaneState { session, ..PaneState::default() })
                })
                .collect();
            self.focused =
                if self.layout.contains(saved.focus) { saved.focus } else { self.layout.first() };
            self.zoom = saved.zoom;
            self.sidebar = saved.sidebar;
            self.sidebar_width = saved.sidebar_width.clamp(12, 80);
        }
        for pane in self.layout.panes() {
            if self.panes.get(&pane).is_some_and(|p| p.session.is_none()) {
                let shown: Vec<SessionId> = self.panes.values().filter_map(|p| p.session).collect();
                if let Some(id) = ordered.iter().find(|id| !shown.contains(id)) {
                    self.panes.entry(pane).or_default().session = Some(*id);
                }
            }
        }
        self.sync();
    }

    // ── attention, peek, inbox (SPEC §9.4–§9.6, M4.5) ───────────────────────

    /// Sessions wanting attention, most urgent first (blocked > errored > done > idle >
    /// exited, oldest first); only this workspace's unless `everywhere`.
    pub fn attention_order(&self, everywhere: bool) -> Vec<SessionId> {
        let here = self.ordered();
        let mut v: Vec<(&SessionInfo, AttentionKind, u64)> = self
            .sessions
            .iter()
            .filter(|s| everywhere || here.contains(&s.id))
            .filter_map(|s| s.attention.as_ref().map(|a| (s, a.kind, a.since_ms)))
            .collect();
        v.sort_by_key(|(_, k, t)| (*k, *t));
        v.into_iter().map(|(s, _, _)| s.id).collect()
    }

    /// `a`: the next session that wants you (`attention.scope`).
    fn next_attention(&mut self) {
        use ptyro_core::config::AttentionScope;
        let scope = self.config.attention.scope;
        let mut target = self.attention_order(scope == AttentionScope::Global).first().copied();
        if target.is_none() && scope == AttentionScope::CurrentFirst {
            target = self.attention_order(true).first().copied();
        }
        let Some(id) = target else {
            self.toast("nothing needs you");
            return;
        };
        if let Some(s) = self.session(id).cloned() {
            let ws = self.home_of(&s);
            if ws != self.workspace {
                self.toast(format!("→ {ws}"));
            }
        }
        self.show(id, None);
    }

    /// Attention counts of other workspaces, for status-bar badges (`me ⚠1`).
    pub fn other_workspace_attention(&self) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for w in &self.workspaces {
            if w.name == self.workspace {
                continue;
            }
            let n = self
                .sessions
                .iter()
                .filter(|s| s.attention.is_some() && self.home_of(s) == w.name)
                .count();
            if n > 0 {
                out.push((w.name.clone(), n));
            }
        }
        out
    }

    pub fn unread(&self) -> usize {
        self.inbox.iter().filter(|e| e.read_ms.is_none()).count()
    }

    fn open_peek(&mut self, id: SessionId) {
        self.overlay =
            Some(Overlay::Peek { session: id, screen: String::new(), reply: String::new() });
        self.request(
            Pending::Peek(id),
            Request::ReadScreen { addr: id.full(), format: ptyro_protocol::ScreenFormat::Plain },
        );
    }

    fn peek_key(&mut self, key: KeyEvent, session: SessionId, screen: String, mut reply: String) {
        let blocked = self
            .session(session)
            .and_then(|s| s.status.as_ref())
            .is_some_and(|r| r.status == AgentStatus::Blocked);
        let ctrl = key.mods.has(Mods::CTRL);
        match key.code {
            KeyCode::Esc => return,
            // With nothing typed, y/a/n answer a permission prompt.
            KeyCode::Char { c: c @ ('y' | 'a' | 'n') } if reply.is_empty() && blocked && !ctrl => {
                let decision = match c {
                    'y' => "allow",
                    'a' => "always",
                    _ => "deny",
                };
                self.request(
                    Pending::Done(format!("answered {decision}")),
                    Request::Answer {
                        addr: session.full(),
                        decision: decision.into(),
                        expect: None,
                    },
                );
                return;
            }
            KeyCode::Enter if reply.is_empty() => {
                self.show(session, None);
                return;
            }
            KeyCode::Enter => {
                self.request(
                    Pending::Done("sent".into()),
                    Request::SendText {
                        addr: session.full(),
                        text: reply,
                        enter: true,
                        wait_ready_ms: None,
                    },
                );
                self.request(
                    Pending::Done(String::new()),
                    Request::ClearAttention { addr: session.full() },
                );
                return;
            }
            KeyCode::Backspace => {
                reply.pop();
            }
            KeyCode::Char { c: 'u' } if ctrl => reply.clear(),
            KeyCode::Char { c } if !ctrl => reply.push(c),
            _ => {}
        }
        self.overlay = Some(Overlay::Peek { session, screen, reply });
    }

    /// Inbox entries for a tab (`None` = all).
    pub fn inbox_tab(&self, tab: Option<&str>) -> Vec<&InboxEntry> {
        self.inbox
            .iter()
            .filter(|e| {
                tab.is_none_or(|t| {
                    e.workspace.as_deref().unwrap_or(ptyro_core::model::MAIN_WORKSPACE) == t
                })
            })
            .collect()
    }

    fn inbox_key(&mut self, key: KeyEvent, mut selected: usize, mut tab: Option<String>) {
        let n = self.inbox_tab(tab.as_deref()).len();
        let entry = self.inbox_tab(tab.as_deref()).get(selected).map(|e| (e.id, e.session.clone()));
        // Gate and run entries open the run (M7.5): "gate g-7f3a · run r-1b2c3d".
        let run_of = self.inbox_tab(tab.as_deref()).get(selected).and_then(|e| {
            matches!(
                e.kind,
                ptyro_core::status::AttentionKind::Gate | ptyro_core::status::AttentionKind::Run
            )
            .then(|| {
                e.title
                    .split_whitespace()
                    .chain(e.body.split_whitespace())
                    .find(|w| w.starts_with("r-"))
                    .map(str::to_owned)
            })
            .flatten()
        });
        match key.code {
            KeyCode::Esc | KeyCode::Char { c: 'q' } => return,
            KeyCode::Down | KeyCode::Char { c: 'j' } => {
                selected = (selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char { c: 'k' } => selected = selected.saturating_sub(1),
            // Cycle tabs: this workspace, every other, All.
            KeyCode::Tab => {
                let mut tabs: Vec<Option<String>> = std::iter::once(Some(self.workspace.clone()))
                    .chain(
                        self.workspaces
                            .iter()
                            .filter(|w| w.name != self.workspace)
                            .map(|w| Some(w.name.clone())),
                    )
                    .collect();
                tabs.push(None);
                let i = tabs.iter().position(|t| *t == tab).unwrap_or(0);
                tab = tabs[(i + 1) % tabs.len()].clone();
                selected = 0;
            }
            KeyCode::Enter => {
                if let Some((id, sess)) = entry {
                    self.request(
                        Pending::Done(String::new()),
                        Request::InboxMark { ids: vec![id], all: false, session: None, read: true },
                    );
                    if let Some(run) = run_of {
                        self.open_run(run);
                        return;
                    }
                    if let Ok(sid) = sess.parse::<SessionId>()
                        && self.session(sid).is_some()
                    {
                        self.show(sid, None);
                        return;
                    }
                    self.toast("that session is gone");
                }
            }
            KeyCode::Char { c: 'p' } => {
                if let Some(sid) = entry
                    .and_then(|(_, s)| s.parse::<SessionId>().ok())
                    .filter(|id| self.session(*id).is_some())
                {
                    self.open_peek(sid);
                    return;
                }
            }
            KeyCode::Char { c: 'r' } => {
                if let Some((id, _)) = entry {
                    self.request(
                        Pending::Done(String::new()),
                        Request::InboxMark { ids: vec![id], all: false, session: None, read: true },
                    );
                    if let Some(e) = self.inbox.iter_mut().find(|e| e.id == id) {
                        e.read_ms = Some(ptyro_core::now_ms());
                    }
                }
            }
            KeyCode::Char { c: 'R' } => {
                self.request(
                    Pending::Done("all read".into()),
                    Request::InboxMark { ids: Vec::new(), all: true, session: None, read: true },
                );
                for e in &mut self.inbox {
                    e.read_ms.get_or_insert(ptyro_core::now_ms());
                }
            }
            _ => {}
        }
        self.overlay = Some(Overlay::Inbox { selected, tab });
    }

    /// The focused session's profile usage, for the status bar.
    pub fn usage_for(&self, profile: &str) -> Vec<&Usage> {
        let mut v: Vec<&Usage> = self.usage.values().filter(|u| u.profile == profile).collect();
        v.sort_by(|a, b| a.agent.cmp(&b.agent));
        v
    }

    /// Status counts for the bar: (live, working, blocked).
    pub fn counts(&self) -> (usize, usize, usize) {
        let live = self.sessions.iter().filter(|s| s.state.is_live()).count();
        let status = |want: AgentStatus| {
            self.sessions
                .iter()
                .filter(|s| {
                    s.state.is_live() && s.status.as_ref().is_some_and(|r| r.status == want)
                })
                .count()
        };
        (live, status(AgentStatus::Working), status(AgentStatus::Blocked))
    }

    pub fn is_controller(&self, id: SessionId) -> bool {
        self.controller.get(&id).copied().unwrap_or(true)
    }

    pub fn exit_state(&self, id: SessionId) -> Option<ProcessState> {
        self.session(id).filter(|s| !s.state.is_live()).map(|s| s.state.clone())
    }
}

/// Where `anchor` is in `rows`; once it's gone, the selectable row nearest `fallback` (its
/// index when it was selected).
fn anchor_index(rows: &[Row], anchor: Option<&Row>, fallback: usize) -> usize {
    if let Some(i) = anchor.and_then(|a| rows.iter().position(|r| r == a)) {
        return i;
    }
    let at = fallback.min(rows.len().saturating_sub(1));
    (at..rows.len()).chain((0..at).rev()).find(|i| rows[*i].selectable()).unwrap_or(at)
}

fn upsert_by<T>(v: &mut Vec<T>, item: T, key: impl Fn(&T) -> String) {
    let k = key(&item);
    match v.iter_mut().find(|x| key(x) == k) {
        Some(x) => *x = item,
        None => v.push(item),
    }
}

/// The wizard's title: where the session goes and under which profile.
pub fn wizard_title(
    w: &Wizard,
    projects: &[ProjectInfo],
    worktrees: &[WorktreeInfo],
    resolved: Option<&str>,
) -> String {
    let place = match (&w.worktree, &w.project) {
        (Some(id), _) => worktrees.iter().find(|x| x.id == *id).map(WorktreeInfo::qualified),
        (None, Some(p)) => {
            projects.iter().find(|x| x.id == *p).map(|p| format!("{} (new worktree)", p.slug))
        }
        _ => None,
    };
    let what = if w.name.is_some() { "New worktree" } else { "New session" };
    let mut t = match place {
        Some(p) => format!("{what} in {p}"),
        None => what.to_owned(),
    };
    if let Some(r) = resolved {
        t.push_str(&format!(" · profile {r}"));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selection_stays_on_its_row() {
        let a = SessionId::new();
        let b = SessionId::new();
        let rows = vec![Row::Group("other sessions".into()), Row::Session(a), Row::Session(b)];
        let sel = Row::Session(b);
        assert_eq!(anchor_index(&rows, Some(&sel), 2), 2);
        // A session appears above it: the selection moves with it, not onto the newcomer.
        let c = SessionId::new();
        let more = vec![rows[0].clone(), Row::Session(c), Row::Session(a), Row::Session(b)];
        assert_eq!(anchor_index(&more, Some(&sel), 2), 3);
        // It's gone: the nearest selectable row where it was, skipping labels.
        let gone = vec![Row::Group("x".into()), Row::Session(a)];
        assert_eq!(anchor_index(&gone, Some(&sel), 2), 1);
        assert_eq!(anchor_index(&gone, None, 0), 1);
        assert_eq!(anchor_index(&[], Some(&sel), 3), 0);
    }
}
