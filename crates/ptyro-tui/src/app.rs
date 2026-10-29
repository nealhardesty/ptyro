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
use ptyro_protocol::{ClientMsg, DomainEvent, PadDoc, Reply, Request, ServerMsg};
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::copy::{CopyMode, Outcome as CopyOutcome, Pos, SelMode};
use crate::keymap::{self, Command, Keymap, Table};
use crate::layout::{Dir, Layout, PaneId, Toward};
use crate::palette::{Item, Palette, Picked, Target};
use crate::quick::Drawer;
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
    /// The quick terminal's drawer (UX1.25), and its share of its host's height.
    #[serde(default)]
    pub drawer: Option<Drawer>,
    #[serde(default = "drawer_share")]
    pub drawer_share: f32,
    /// The split layout a full-sized panel replaced (UX3.1).
    #[serde(default)]
    pub kept: Option<crate::screens::Screen>,
}

fn drawer_share() -> f32 {
    crate::quick::DRAWER_SHARE
}

/// Where the TUI remembers which workspace it showed last.
pub const LAST_WORKSPACE: &str = "_tui";

/// The one layout for every workspace (MM1.B: they are all on screen at once). Before, each
/// workspace had its own, under its name: the last one's is taken over once.
pub const ALL_VIEW: &str = "_all";

/// The widest the sidebar gets (the settings schema's range ends here too).
const SIDEBAR_MAX: u16 = 120;

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
    /// `n` (UX1.9): "what should it do?", one box. ⏎ starts the default agent on it in a new
    /// worktree named after it; Tab edits the name; `C-o` opens the full wizard.
    Task {
        text: String,
        /// The name typed over the suggestion; `None` follows the prompt.
        name: Option<String>,
        /// Typing goes to the name, not the prompt.
        naming: bool,
        wizard: Wizard,
    },
    /// A project's first worktree (UX1.11): what ptyro found it needs; y writes it locally.
    Suggest {
        suggestions: ptyro_core::model::SetupSuggestions,
        spec: Box<WorktreeSpec>,
        split: Option<Dir>,
    },
    /// Lifecycle scripts shown in full before trusting them (SPEC §10.5).
    Trust {
        scripts: ScriptsInfo,
        then: AfterTrust,
        scroll: usize,
    },
    /// Every configuration option, edited in place (CFG1.1).
    Settings(Box<crate::settings::Panel>),
    /// About (UX2.1): `ptyro about --json` (`None` while it runs), the picked row of
    /// `about::picks`, and how far it is scrolled.
    About {
        doc: Option<serde_json::Value>,
        sel: usize,
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
    /// The first prompt, from the task box (UX1.9); also the title and, slugged, the name.
    pub prompt: Option<String>,
    /// A name to offer, dimmed, that ⏎ takes when nothing was typed (UX1.24).
    pub suggested: Option<String>,
    /// Where the work comes from (UX1.10): a PR, an issue, an existing branch, or the main
    /// checkout's uncommitted changes.
    pub pr: Option<u32>,
    pub issue: Option<String>,
    pub branch: Option<String>,
    pub take: bool,
    /// Started from the scratchpad item on this line (NOTE1.3).
    pub pad_item: Option<u32>,
}

/// A PR or an issue named in text (UX1.10): a GitHub URL, or `pr 12` / `issue 12`.
pub fn work_source(text: &str) -> Option<(Option<u32>, Option<String>)> {
    let t = text.trim();
    let number_after = |key: &str| {
        t.split(key).nth(1).and_then(|r| {
            r.trim_start_matches('#')
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u32>()
                .ok()
        })
    };
    if t.contains("github.com/") && t.contains("/pull/") {
        return number_after("/pull/").map(|n| (Some(n), None));
    }
    if t.contains("github.com/") && t.contains("/issues/") {
        return number_after("/issues/")
            .map(|_| (None, Some(t.split_whitespace().next().unwrap_or(t).to_owned())));
    }
    let lower = t.to_lowercase();
    let rest = |p: &str| {
        lower.strip_prefix(p).and_then(|r| r.trim().trim_start_matches('#').parse::<u32>().ok())
    };
    if let Some(n) = rest("pr ").or_else(|| rest("pr#")) {
        return Some((Some(n), None));
    }
    if let Some(n) = rest("issue ").or_else(|| rest("issue#")) {
        return Some((None, Some(n.to_string())));
    }
    None
}

#[derive(Debug, Clone)]
pub enum Purpose {
    Palette,
    /// Show the pick in the focused pane.
    Show,
    /// Where a new workspace goes: this machine or another (MM1.B).
    NewWorkspaceOn,
    /// A machine's hidden workspace to show again (`machine:workspace`).
    MachineUnhide,
    /// A machine to act on (MM1.B): retry, hide its workspace, remove it.
    MachinePick(&'static str),
    /// Wizard step one: what to run.
    Agent(Wizard),
    /// Wizard step two: where.
    Target(Wizard),
    Workspace,
    Editor(String),
    Script(String),
    /// Add a project, then carry on (UX1.6).
    AddProject(Option<AfterAdd>),
    /// A row's actions (UX1.15): run with that row selected in the sidebar.
    RowMenu(Row),
    /// Home (UX1.19): what to do next.
    Home,
    /// What the doctor found, to fix (UX1.20).
    Health,
    /// `ptyro <these args> <the pick>`, in a split (UX1.20, UX1.21).
    Cli(Vec<String>),
    /// A word of a `ptyro …` command being put together (UX1.21).
    CliPick(Box<crate::cli_actions::CliJob>),
    /// The profile to pin this project to.
    Pin(String),
    /// This workspace's profile, or how it notifies.
    WorkspaceProfile,
    WorkspaceNotify,
    /// How to finish this worktree (UX1.12): a PR, or integrate.
    Finish(String),
    /// An archived worktree to bring back.
    Restore,
    /// A stale worktree to clean up (archive).
    Gc,
    /// The existing branch a new worktree checks out (UX1.10).
    Branch(Wizard),
    /// The scratchpad's open items (NOTE1.3).
    PadItems,
    /// The scratchpad's saved copies.
    PadHistory,
}

/// What asked for a project when there was none (UX1.6): done once the project is added.
#[derive(Debug, Clone)]
pub enum AfterAdd {
    /// Re-run this action, in the new project's main checkout.
    Run(&'static str),
    /// The wizard's second step, in the new project.
    Targets(Wizard),
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
    /// A line for the scratchpad, about this session (NOTE1.2).
    PadAdd(Option<SessionId>),
    /// A name for a new workspace; ⏎ on nothing takes the suggestion. On another machine
    /// (MM1.B) when the second is its name.
    NewWorkspace(Option<String>, Option<String>),
    /// A session's title (UX1.24): free text; empty clears it.
    Retitle(SessionId),
    /// A worktree's title (by id).
    RetitleWorktree(String),
    /// A PR or issue to start a worktree from (UX1.10).
    WorkSource(Wizard),
    /// A word for a `ptyro …` command being put together (UX1.21); `true`: `k=v` variables.
    CliAsk(Box<crate::cli_actions::CliJob>, bool),
    /// A branch's new name, for this worktree (by id).
    RenameBranch(String),
    /// A PR to go to (UX1.21).
    OpenPr,
    /// Another machine to mirror (MM1.B): `ssh://host`, or the code `machine pair` printed.
    MachineAdd,
    /// A workspace's new name.
    RenameWorkspace(String),
    /// Typed in the runs view (M7).
    Orch(crate::runs::OrchInput),
    /// Typed in the change-set view (M11).
    Sets(crate::sets::SetsInput),
}

/// A key that starts a new line in a text box instead of sending it (UX1.26): Shift+Enter
/// (with the kitty keyboard protocol), Alt+Enter, or Ctrl+J (every terminal).
pub(crate) fn is_newline_key(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key.mods.has(Mods::SHIFT | Mods::ALT | Mods::META),
        KeyCode::Char { c: 'j' } => key.mods.has(Mods::CTRL),
        _ => false,
    }
}

impl PromptKind {
    /// Free text that may run over several lines (UX1.26): a prompt for agents, a comment, a
    /// commit message. Names, titles and command lines stay on one line.
    pub fn multiline(&self) -> bool {
        matches!(self, Self::Broadcast(_) | Self::ReviewComment { .. } | Self::CommitMessage { .. })
    }

    /// The text is a name (`[a-z0-9._-]`): bad characters are marked and Tab tidies it.
    pub fn is_name(&self) -> bool {
        matches!(
            self,
            Self::Rename(_)
                | Self::WorktreeName(_)
                | Self::NewWorkspace(..)
                | Self::RenameWorkspace(_)
        )
    }

    /// `text` as this prompt keeps it: a name or branch swaps what it can't hold for `_`.
    pub fn fit(&self, text: &str) -> String {
        match self {
            _ if self.is_name() => ptyro_core::names::typed(text),
            Self::RenameBranch(_) => ptyro_core::names::branch_typed(text),
            _ => text.to_owned(),
        }
    }

    /// The name ⏎ takes when nothing was typed (shown dimmed in the field, UX1.24).
    pub fn suggestion(&self) -> Option<&str> {
        match self {
            Self::WorktreeName(w) => w.suggested.as_deref(),
            Self::NewWorkspace(s, _) => s.as_deref(),
            _ => None,
        }
    }
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
    /// Archive this worktree, stopping what runs in it (UX1.12).
    Archive(String),
    /// Add a line to a file: a health fix (UX1.20).
    Append(std::path::PathBuf, String),
    /// Send this request, then show this (runs view: cancel, pick, discard).
    Send(Box<Request>, String),
    /// Quit ptyro: stop every session and the daemon (`daemon stop --force`).
    Quit,
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
        /// A tool whose screen goes back to what it replaced when it exits (UX3.1).
        back: bool,
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
    /// A workspace was renamed: the TUI follows it once the daemon has agreed.
    WorkspaceRenamed(String),
    /// A workspace was made: switch to it once the daemon has agreed.
    WorkspaceMade(String),
    /// A quick terminal to show in the drawer (UX1.25).
    Quick(ptyro_core::config::ShellStyle),
    /// The scratchpad's editor to show (NOTE1.1).
    Pad,
    /// A line was added to the scratchpad.
    PadAdded,
    /// The scratchpad's text, for its open items; `refresh`: only to update a list already open.
    PadItems {
        refresh: bool,
    },
    PadHistory,
    List,
    Registry,
    Saved,
    /// What quick-new would start in this workspace (UX3.2), for its label.
    Again(String),
    /// Wizard title: the profile a spawn would get and why.
    Resolved,
    /// Show the trust dialog for these scripts, then do `then`.
    TrustDialog(AfterTrust),
    Trusted(AfterTrust),
    Removed(String),
    ProjectAdded(Option<AfterAdd>),
    Discovered(Option<AfterAdd>),
    Scripts(String),
    /// Check the project's scripts are trusted before creating a worktree.
    PreCreate(Box<WorktreeSpec>, Option<Dir>),
    Peek(SessionId),
    Explain(SessionId),
    /// `session.summarize` (UX1.3): toast the summary.
    Summarized,
    /// `app.quit`: the daemon is stopping.
    Quit,
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
    /// The context worktree's scripts, for an open palette (UX1.18).
    PaletteScripts(String),
    /// Archived worktrees, for the restore picker (UX1.12).
    Archives,
    /// Stale worktrees, for the clean-up picker.
    Gc,
    /// The project's branches, for the wizard (UX1.10).
    Branches(Wizard),
    /// Setup suggestions before a project's first worktree (UX1.11).
    Suggest(Box<WorktreeSpec>, Option<Dir>),
    /// They were written: check the scripts (trust) again, then create.
    Suggested(Box<WorktreeSpec>, Option<Dir>),
    /// The repositories for the first run, and for home (UX1.19).
    FirstRunRepos,
    Home,
    /// `project.suggest`: say what was found, offering to use it.
    SuggestShow(String),
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
    /// What `ptyro doctor` found (UX1.20).
    Health(Vec<crate::health::Check>),
    /// What `ptyro about --json` said (UX2.1); `None` if it could not run.
    About(Option<serde_json::Value>),
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
    /// What `toast.act` (`C-q u`) does about it: undo it, or fix it (UX1.5, UX1.17).
    pub offer: Option<Offer>,
}

/// A message's one follow-up, run by `toast.act` while the message shows.
#[derive(Debug, Clone)]
pub struct Offer {
    /// What it does, for the message: "undo", "new one".
    pub label: String,
    pub then: Then,
}

#[derive(Debug, Clone)]
pub enum Then {
    /// Run a TUI action.
    Run(Command),
    /// Send this request; toast the text once it's done.
    Send(Box<Request>, String),
    /// Open this file in `$EDITOR`, in a split.
    Edit(std::path::PathBuf),
}

/// An offer to run a TUI action.
fn run_offer(label: &str, action: &'static str) -> Offer {
    Offer { label: label.into(), then: Then::Run(Command { action, arg: None }) }
}

/// The CLI commands error messages suggest, and the TUI action that does each (UX1.17): a
/// message that names one offers that action instead of leaving you to type it. Longest first.
const SUGGESTED: &[(&str, &str, &str)] = &[
    ("`ptyro project trust", "project.trust", "review and trust them"),
    ("`ptyro project add", "project.add", "add a project"),
    ("`ptyro status --explain", "session.explain", "see why"),
    ("`ptyro new --structured", "session.new", "start one"),
    ("`ptyro new --agent", "session.new", "start one"),
    ("`ptyro new", "session.new", "start one"),
    ("`ptyro wt diff", "inspector", "inspect it"),
    ("`ptyro pr fix", "inspector", "inspect its checks"),
    ("`ptyro pr watch", "inspector", "inspect its checks"),
    ("`ptyro events", "inspector", "see its events"),
    ("`ptyro run retry", "runs", "see the run"),
    ("`ptyro fanout", "runs", "see runs"),
    ("`ptyro config edit", "config.edit", "edit the config"),
];

/// What a message suggests doing, as a TUI action (see [`SUGGESTED`]).
pub fn offer_for(message: &str) -> Option<Offer> {
    SUGGESTED
        .iter()
        .find(|(cli, _, _)| message.contains(cli))
        .map(|(_, action, label)| run_offer(label, action))
}

enum Drag {
    Divider(crate::layout::Divider),
    /// The sidebar's right edge (UX1.7).
    Sidebar,
    Select {
        pane: PaneId,
        started: bool,
    },
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
    /// Other machines whose workspaces this daemon mirrors (MM1.B), and how their links stand.
    pub machines: Vec<ptyro_core::model::MachineInfo>,
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
    /// The short name of the machine the daemon runs on (a remote daemon's, not ours).
    pub machine: Option<String>,
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
    /// The sidebar fills the screen with every row open: everything at once (UX1.2).
    pub sidebar_zoom: bool,
    /// The quick terminal's drawer, when open, and the share of its host it takes (UX1.25).
    pub drawer: Option<Drawer>,
    pub drawer_share: f32,
    /// The split layout a full-sized panel replaced, kept whole for `` ` `` (UX3.1).
    pub kept: Option<Box<crate::screens::Screen>>,
    /// A session whose row is to be selected once the tree can place it (UX3.3).
    pub(crate) revealing: Option<(SessionId, Instant)>,
    /// Tools (an editor) whose screen goes back to what it replaced when they exit (UX3.1).
    pub(crate) tool_back: HashSet<SessionId>,
    /// What quick-new (`session.again`) starts in this workspace; `None`: the default agent.
    pub again: Option<ptyro_core::session::LastLaunch>,
    /// The scratchpad's open items, kept current by `ScratchChanged` (NOTE1.2).
    pub pad_open: u32,
    /// The scratchpad's editor, when it is shown as a popup (NOTE1.1).
    pub pad_popup: Option<SessionId>,
    /// The pad as the items list last read it, to start an item from.
    pub(crate) pad_doc: Option<PadDoc>,
    /// The items list shows ticked ones too.
    pub(crate) pad_show_done: bool,
    /// The selected sidebar row, and its index when last set (the fallback once it's gone).
    sidebar_row: Option<Row>,
    sidebar_sel: usize,
    pub prefix: bool,
    /// When the prefix was pressed (the which-key menu follows, UX1.14).
    pub prefix_at: Option<Instant>,
    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,
    pub buffer: Option<String>,
    pub controller: HashMap<SessionId, bool>,
    pub area: Rect,
    pub dirty: bool,
    pub quit: Option<AttachEnd>,
    /// The tour's coach (UX1.22), when this TUI runs `ptyro tour`.
    pub tour: Option<crate::tour::Tour>,
    /// `app.quit` was confirmed: the daemon going away ends the TUI instead of reconnecting
    /// (which would start a new one).
    quitting: bool,
    pub lost_since: Option<Instant>,
    view_dirty_since: Option<Instant>,
    last_list: Instant,
    /// The sessions the last `List` reply named (to tell "gone" from "newer than the reply").
    last_listed: HashSet<SessionId>,
    drag: Option<Drag>,
    reported_focus: Option<(SessionId, bool)>,
    /// Sessions this TUI stopped: their panes move on when they exit (`[ui] on_stop`, UX1.23).
    stopping: HashSet<SessionId>,
    /// A project just added, and what to do in it once its main checkout is known (UX1.6).
    after_add: Option<(String, AfterAdd)>,
    /// The project `context()` answers while an `after_add` runs.
    context_hint: Option<String>,
    /// The first-run screen (UX1.19), drawn over everything.
    pub first_run: Option<crate::onboard::FirstRun>,
    /// What the doctor found, for the badge (UX1.20).
    pub health: crate::health::Health,
    /// The last `ptyro …` command run in a split (`run_cli`).
    pub last_cli: Option<Vec<String>>,
    /// Addresses drawn on screen this frame, for the terminal to make links (OSC 8, UX2.1).
    pub links: std::cell::RefCell<Vec<crate::about::Link>>,
    pub started: Instant,
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
            machines: Vec::new(),
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
            machine: None,
            runs: Vec::new(),
            terminal_focused: true,
            sidebar: config.ui.sidebar && !opts.raw,
            sidebar_width: config.ui.sidebar_width.clamp(12, SIDEBAR_MAX),
            sidebar_zoom: false,
            drawer: None,
            drawer_share: crate::quick::DRAWER_SHARE,
            kept: None,
            revealing: None,
            tool_back: HashSet::new(),
            again: None,
            pad_open: 0,
            pad_popup: None,
            pad_doc: None,
            pad_show_done: false,
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
            prefix_at: None,
            overlay: None,
            tour: opts_tour.then(crate::tour::Tour::default),
            toast: None,
            buffer: None,
            controller: HashMap::new(),
            area,
            dirty: true,
            quit: None,
            quitting: false,
            lost_since: None,
            view_dirty_since: None,
            last_list: Instant::now(),
            last_listed: HashSet::new(),
            drag: None,
            reported_focus: None,
            stopping: HashSet::new(),
            after_add: None,
            context_hint: None,
            first_run: None,
            health: crate::health::Health::default(),
            last_cli: None,
            links: std::cell::RefCell::new(Vec::new()),
            started: Instant::now(),
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
        self.last_listed = sessions.iter().map(|s| s.id).collect();
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
        if let Ok(Reply::Machines { machines }) = client.request(Request::MachineList).await {
            self.machines = machines;
        }
        if let Ok(Reply::Inbox { entries }) = client
            .request(Request::Inbox { workspace: None, unread_only: false, limit: Some(500) })
            .await
        {
            self.inbox = entries;
        }
        if let Ok(Reply::ScratchItems { items, .. }) =
            client.request(Request::ScratchTodo { all: false }).await
        {
            self.pad_open = u32::try_from(items.len()).unwrap_or(u32::MAX);
        }
        if let Ok(Reply::Status { status }) = client.request(Request::Status).await {
            self.machine = status.machine;
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
            self.toast_offer(
                format!("while you were away: {ended} run update(s), {gates} gate(s) waiting"),
                false,
                run_offer("see them", "runs"),
            );
        }
    }

    /// Restore the saved view (unless raw or view-only), then make sure `opts.focus` is shown.
    pub async fn restore_view(&mut self) {
        // A layout changed while the daemon was away is newer than the one it kept: send it
        // first, and stay on the workspace it belongs to, instead of loading the old one over it.
        let unsaved = self.view_dirty_since.is_some() && !self.opts.raw && !self.opts.view_only;
        if unsaved
            && let Some(client) = &self.client
            && let Some(reqs) = self.view_requests()
        {
            for r in reqs {
                let _ = client.request(r).await;
            }
            self.view_dirty_since = None;
        }
        if self.opts.workspace.is_none()
            && !unsaved
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
        self.refresh_again();
    }

    /// The workspace a session shows up under in the sidebar (MM1.B: the first that has its
    /// project, else its home, else its machine's `main`).
    pub fn home_of(&self, s: &SessionInfo) -> String {
        self.tree().home(s)
    }

    /// Load the current workspace's saved view.
    async fn load_view(&mut self) {
        // The scratchpad's popup is a session of the daemon we were connected to: gone with it
        // (it would swallow every key and click, showing nothing).
        if let Some(id) = self.pad_popup.filter(|id| self.session(*id).is_none()) {
            self.pad_gone(id);
        }
        self.layout = Layout::default();
        self.panes = HashMap::from([(1, PaneState::default())]);
        self.focused = 1;
        self.zoom = false;
        self.drawer = None;
        self.kept = None;
        let saved = match &self.client {
            Some(client) if !self.opts.raw => {
                let load = |name: String| async move {
                    match client.request(Request::LoadView { name }).await {
                        Ok(Reply::View { view: Some(v) }) => {
                            serde_json::from_value::<SavedView>(v).ok()
                        }
                        _ => None,
                    }
                };
                match load(ALL_VIEW.into()).await {
                    Some(v) => Some(v),
                    // From before one layout for all: the last workspace's, once.
                    None => load(self.workspace.clone()).await,
                }
            }
            _ => None,
        };
        if let Some(saved) = saved {
            let mut ordered_all = self.ordered();
            ordered_all.extend(self.sessions.iter().filter(|s| s.quick || s.pad).map(|s| s.id));
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
            self.sidebar_width = saved.sidebar_width.clamp(12, SIDEBAR_MAX);
            self.restore_drawer(saved.drawer, saved.drawer_share);
            self.restore_kept(saved.kept, &ordered_all);
        }
        let ordered = self.ordered();
        // `ptyro shell` asks for a quick terminal: it opens in the drawer (UX1.25).
        let focus = self.opts.focus.take();
        let quick = focus.filter(|id| self.session(*id).is_some_and(|s| s.quick));
        let want = focus.filter(|_| quick.is_none()).or_else(|| {
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
        self.settle_empty_panes(&ordered);
        self.sync();
        if let Some(id) = quick {
            self.toggle_quick(Some(id), None, None);
        }
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
        let Some(client) = self.client().cloned() else {
            // Not connected (the daemon is restarting): say so for what the person asked for,
            // instead of silently doing nothing. Background refreshes just try again.
            if !matches!(
                pending,
                Pending::Saved
                    | Pending::List
                    | Pending::Registry
                    | Pending::Resolved
                    | Pending::PadItems { .. }
            ) {
                let why = "not connected: waiting for the daemon to come back";
                let _ = self.tx.send(AppMsg::Reply(
                    pending,
                    Err(ptyro_core::Error::new(ErrorCode::DaemonUnavailable, why)),
                ));
            }
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = client.request(req).await;
            let _ = tx.send(AppMsg::Reply(pending, res));
        });
    }

    pub fn lost(&mut self, reason: &str) {
        if self.quitting {
            self.client = None;
            self.pushes = None;
            self.quit = Some(AttachEnd::Stopped);
            return;
        }
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
        if self.sidebar && !self.zoom && a.width > self.sidebar_width.saturating_add(20) {
            a.x += self.sidebar_width + 1;
            a.width -= self.sidebar_width + 1;
        }
        a
    }

    pub fn sidebar_area(&self) -> Option<Rect> {
        let main = self.main_area();
        if self.sidebar_zoom {
            // Over the panes, which keep their size: nothing is resized for a look.
            return Some(Rect {
                width: self.area.width.saturating_sub(1),
                height: main.height,
                ..self.area
            });
        }
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
            // A zoomed pane keeps its drawer, and the drawer zooms with its host (UX1.25).
            if let Some(d) = self.drawer
                && (self.focused == d.host || self.focused == d.pane)
            {
                let (top, _, bottom) = self.layout.stacked(main, d.pane);
                return vec![(d.host, top), (d.pane, bottom)];
            }
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
        // Keys never go to a sidebar nobody can see (zoomed pane, narrow terminal).
        if self.focus == Focus::Sidebar && self.sidebar_area().is_none() {
            self.focus = Focus::Pane;
        }
        self.follow_focused_workspace();
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
        if let Some(id) = self.pad_popup {
            let r = self.inner_popup();
            visible.insert(id, TermSize::new(r.width.max(2), r.height.max(1)));
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

    /// Remove `pane` from the layout; focus (and zoom) leave it if it had them. False for the last pane.
    pub(crate) fn close_pane(&mut self, pane: PaneId) -> bool {
        // The drawer goes with its host; closing the drawer is hiding it (UX1.25).
        match self.drawer {
            Some(d) if d.pane == pane => {
                self.hide_drawer();
                return true;
            }
            Some(d) if d.host == pane => self.hide_drawer(),
            _ => {}
        }
        if !self.layout.close(pane) {
            return false;
        }
        self.panes.remove(&pane);
        if self.focused == pane {
            self.focused = self.layout.first();
            self.zoom = false;
        }
        true
    }

    /// Panes showing `gone` show its neighbour instead (UX1.23). `removed`: it's leaving the
    /// list, so a pane with no neighbour to show closes (a split) or shows the empty state
    /// (the last pane); a stopped one stays put when there's nothing else to show.
    fn move_on(&mut self, gone: SessionId, order: &[SessionId], removed: bool) {
        let Some(info) = self.session(gone).cloned() else { return };
        let mut panes: Vec<PaneId> =
            self.panes.iter().filter(|(_, p)| p.session == Some(gone)).map(|(k, _)| *k).collect();
        // The focused pane picks first: it gets the nearest neighbour.
        panes.sort_by_key(|p| (*p != self.focused, *p));
        for pane in panes {
            // Only a split command fills a split (UX3.1): a split's pane closes instead, unless
            // it was stopped and stays to show its last screen.
            if self.is_split() && self.drawer.is_none_or(|d| d.pane != pane) {
                if removed {
                    self.close_pane(pane);
                }
                continue;
            }
            let shown: Vec<SessionId> = self
                .panes
                .iter()
                .filter(|(k, _)| **k != pane)
                .filter_map(|(_, p)| p.session)
                .collect();
            match neighbour(order, &info, &self.sessions, &shown) {
                Some(next) => {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.session = Some(next);
                        p.copy = None;
                        p.loading = false;
                        // `` ` `` brings a stopped session back; a removed one is gone.
                        p.last = (!removed).then_some(gone);
                    }
                    if pane == self.focused {
                        let rows = self.rows();
                        if let Some(i) = rows.iter().position(|r| *r == Row::Session(next)) {
                            self.set_sel(&rows, i);
                        }
                    }
                }
                None if removed => {
                    if !self.close_pane(pane)
                        && let Some(p) = self.panes.get_mut(&pane)
                    {
                        p.session = None;
                        p.copy = None;
                    }
                }
                None => {}
            }
        }
        self.view_changed();
    }

    pub(crate) fn view_changed(&mut self) {
        if !self.opts.raw && !self.opts.view_only {
            self.view_dirty_since = Some(Instant::now());
        }
        self.sync();
    }

    /// The requests that save this workspace's view and remember it as the last one.
    fn view_requests(&self) -> Option<[Request; 2]> {
        let saved = SavedView {
            v: 1,
            layout: self.layout.clone(),
            panes: self.panes.iter().map(|(k, p)| (*k, p.session.map(|s| s.full()))).collect(),
            focus: self.focused,
            zoom: self.zoom,
            sidebar: self.sidebar,
            sidebar_width: self.sidebar_width,
            drawer: self.drawer,
            drawer_share: self.drawer_share,
            kept: self.kept.as_deref().cloned(),
        };
        let view = serde_json::to_value(saved).ok()?;
        let last = serde_json::json!({ "workspace": self.workspace });
        Some([
            Request::SaveView { name: ALL_VIEW.into(), view },
            Request::SaveView { name: LAST_WORKSPACE.into(), view: last },
        ])
    }

    pub fn save_view(&mut self) {
        if self.opts.raw || self.opts.view_only {
            self.view_dirty_since = None;
            return;
        }
        // With no daemon to take it the change stays unsaved (and is saved on reconnect):
        // clearing the flag here would drop a layout changed during an outage.
        if self.client.is_none() {
            return;
        }
        self.view_dirty_since = None;
        if let Some(reqs) = self.view_requests() {
            for r in reqs {
                self.request(Pending::Saved, r);
            }
        }
    }

    /// Periodic work: toast expiry, debounced view saves, the sidebar's foreground names.
    pub fn tick(&mut self) {
        if let Some(mut tour) = self.tour.take() {
            if tour.advance(self) {
                self.dirty = true;
            }
            self.tour = Some(tour);
        }
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
        self.health_tick(self.started);
    }

    fn start_prefix(&mut self) {
        self.prefix = true;
        self.prefix_at = Some(Instant::now());
    }

    /// When the which-key menu is due, while the prefix waits for its key (UX1.14).
    pub fn which_key_due(&self) -> Option<Instant> {
        let delay = self.config.ui.which_key_delay;
        (self.prefix && delay > 0)
            .then(|| self.prefix_at.map(|t| t + Duration::from_millis(delay)))
            .flatten()
    }

    /// The which-key menu shows: the prefix waited long enough for its key.
    pub fn which_key_shown(&self) -> bool {
        self.which_key_due().is_some_and(|t| Instant::now() >= t)
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: false,
            until: Instant::now() + Duration::from_secs(3),
            offer: None,
        });
        self.dirty = true;
    }

    pub fn toast_err(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: true,
            until: Instant::now() + Duration::from_secs(6),
            offer: None,
        });
        self.dirty = true;
    }

    /// A message with a follow-up that `toast.act` runs (`C-q u`): it stays up longer, and says
    /// which key does what.
    pub fn toast_offer(&mut self, text: impl Into<String>, error: bool, offer: Offer) {
        let key = self.keymap.key_for("toast.act").unwrap_or_else(|| "toast.act".into());
        self.toast = Some(Toast {
            text: format!("{} · {key} {}", text.into(), offer.label),
            error,
            until: Instant::now() + Duration::from_secs(10),
            offer: Some(offer),
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
            ServerMsg::Event { event } => {
                self.on_event(event);
                self.retry_reveal();
            }
            ServerMsg::Chat { session, rev, reset, items, draft } => {
                self.chats.entry(session).or_default().apply(rev, reset, items, draft);
            }
            ServerMsg::Goodbye { reason } => self.lost(&reason),
            _ => {}
        }
    }

    /// A `List` reply (every 2 s, and after events were lost): it is the truth about what
    /// exists, so a session it no longer names (whose removal event never came) is gone from
    /// panes and every per-session map. One made by an event after the reply was asked for
    /// isn't in it yet and stays.
    fn got_list(&mut self, listed: Vec<SessionInfo>) {
        let named: HashSet<SessionId> = listed.iter().map(|s| s.id).collect();
        let gone: Vec<SessionId> = self
            .sessions
            .iter()
            .map(|s| s.id)
            .filter(|id| self.last_listed.contains(id) && !named.contains(id))
            .collect();
        if !gone.is_empty() && !self.opts.raw {
            let order = self.ordered();
            for id in &gone {
                if self.is_pad(*id) {
                    self.pad_gone(*id);
                } else if self.drawer_session() == Some(*id) {
                    self.hide_drawer();
                }
                self.forget_kept(*id);
                self.move_on(*id, &order, true);
            }
        }
        let newer: Vec<SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| !self.last_listed.contains(&s.id) && !named.contains(&s.id))
            .cloned()
            .collect();
        self.sessions = listed;
        self.sessions.extend(newer);
        for id in gone {
            self.forget_session(id);
        }
        self.last_listed = named;
    }

    /// Drop what is kept per session once it is gone (these maps would only grow).
    fn forget_session(&mut self, id: SessionId) {
        self.stopping.remove(&id);
        self.marked.retain(|m| *m != id);
        self.chats.remove(&id);
        self.chat_terminal.remove(&id);
        self.controller.remove(&id);
        self.caches.remove(&id);
        self.sizes.remove(&id);
        self.subscribed.remove(&id);
        for p in self.panes.values_mut() {
            if p.last == Some(id) {
                p.last = None;
            }
        }
    }

    pub(crate) fn upsert(&mut self, info: SessionInfo) {
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
                let id = info.id;
                self.upsert(info);
                // An editor that's done gives its screen back to what it replaced (UX3.1).
                if self.tool_back.remove(&id) && self.focused_session() == Some(id) {
                    self.go_back();
                }
                // You stopped it here: its panes move on, and it stays in the sidebar.
                if self.stopping.remove(&id)
                    && !self.opts.raw
                    && self.config.ui.on_stop == ptyro_core::config::OnStop::Next
                {
                    let order = self.ordered();
                    self.move_on(id, &order, false);
                }
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
                self.stopping.remove(&id);
                if self.opts.raw {
                    self.sessions.retain(|s| s.id != id);
                    if self.focused_session() == Some(id) {
                        self.quit = Some(AttachEnd::Gone);
                    }
                    self.sync();
                    return;
                }
                // `exit` in the quick terminal: its drawer closes (UX1.25); so does the scratchpad's
                // editor's, in whatever showed it.
                if self.is_pad(id) {
                    self.pad_gone(id);
                } else if self.drawer_session() == Some(id) {
                    self.hide_drawer();
                }
                // The order as it was, so "the next one" means the one after it in the sidebar.
                let order = self.ordered();
                self.forget_kept(id);
                self.tool_back.remove(&id);
                self.move_on(id, &order, true);
                self.sessions.retain(|s| s.id != id);
                self.forget_session(id);
                self.view_changed();
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
            DomainEvent::SessionResumed { session } => {
                if self.subscribed.contains(&session) {
                    let name = self.session(session).map_or("the session", |s| s.name.as_str());
                    self.toast(format!(
                        "resumed {name}: Ctrl-Z suspended it, and there's no shell here to fg it"
                    ));
                }
            }
            DomainEvent::Notification { .. } => {}
            DomainEvent::ScratchChanged { open, .. } => self.pad_changed(open),
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
            DomainEvent::ProjectAutoAdded { info, workspace, notes } => {
                upsert_by(&mut self.projects, info.clone(), |p| p.id.clone());
                let undo = Request::ProjectRemove { addr: info.id.clone(), release: true };
                let done = format!("forgot {}; it won't be registered on its own again", info.slug);
                self.toast_offer(
                    format!("registered {} in {workspace}", info.slug),
                    false,
                    Offer { label: "undo".into(), then: Then::Send(Box::new(undo), done) },
                );
                // A pin its rules suggest still asks, as adding it by hand does (SPEC §10.10).
                if self.overlay.is_none()
                    && notes.iter().any(|n| n.starts_with("pinned to profile"))
                    && let Some(pin) = &info.pin
                {
                    self.overlay = Some(Overlay::Confirm {
                        text: format!(
                            "{} matches profile {pin}'s rules and is now pinned to it. Keep the pin? [Y/n]",
                            info.slug
                        ),
                        action: Confirmed::KeepPin(info.id.clone()),
                    });
                }
            }
            DomainEvent::WorkspaceUpdated { info } => {
                upsert_by(&mut self.workspaces, info, |w| w.id.clone())
            }
            DomainEvent::MachineUpdated { info } => {
                self.machine_news(info);
            }
            DomainEvent::MachineRemoved { name } => self.machines.retain(|m| m.name != name),
            DomainEvent::WorkspaceRemoved { id } => {
                let name = self.workspaces.iter().find(|w| w.id == id).map(|w| w.name.clone());
                self.workspaces.retain(|w| w.id != id);
                if name.as_deref() == Some(self.workspace.as_str()) {
                    self.switch_workspace(MAIN_WORKSPACE.into());
                }
            }
            DomainEvent::WorktreeUpdated { info } => {
                upsert_by(&mut self.worktrees, info, |w| w.id.clone());
                self.try_after_add();
            }
            DomainEvent::WorktreeRemoved { id } => self.worktrees.retain(|w| w.id != id),
            DomainEvent::WorktreeProgress { worktree, step } => {
                self.toast(format!("{worktree}: {step}"))
            }
            DomainEvent::ConfigReloaded { error } => match error {
                Some(e) => self.toast_err(format!("config not applied: {e}")),
                None => {
                    // The panel made this change (or is showing the file): it says its own.
                    let in_panel = matches!(self.overlay, Some(Overlay::Settings(_)));
                    self.reload_config_with(!in_panel);
                    if let Some(Overlay::Settings(p)) = &mut self.overlay {
                        p.refresh();
                    }
                    self.request(Pending::Registry, Request::ProfileList);
                }
            },
        }
    }

    /// Read the config again; `say` toasts that it was (the settings panel makes the change
    /// itself and says its own).
    fn reload_config_with(&mut self, say: bool) {
        let path = Config::path(&self.paths);
        match Config::load(&path) {
            Ok(config) => {
                let (keymap, errors) = Keymap::new(&config);
                self.keymap = keymap;
                self.theme_name = config.ui.theme.clone();
                self.theme = Theme::resolve(&config, &self.theme_name, no_color());
                let old = std::mem::replace(&mut self.config, config);
                // What a change to the file does at once, beyond what is read each time it is
                // used: the sidebar's visibility and width follow the file when the file moved.
                // (The mouse is the event loop's, which toggles terminal capture to match.)
                if old.ui.sidebar != self.config.ui.sidebar && !self.opts.raw {
                    self.sidebar = self.config.ui.sidebar;
                }
                if old.ui.sidebar_width != self.config.ui.sidebar_width {
                    self.set_sidebar_width(self.config.ui.sidebar_width);
                }
                match errors.first() {
                    Some(e) => self.toast_err(format!("config: {e}")),
                    None if say => self.toast("config reloaded"),
                    None => {}
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
            AppMsg::Health(checks) => {
                self.got_health(checks);
                return;
            }
            AppMsg::About(doc) => {
                if let Some(Overlay::About { doc: slot, .. }) = &mut self.overlay {
                    *slot = Some(doc.unwrap_or_else(|| serde_json::json!({ "failed": true })));
                }
                return;
            }
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
                self.refresh_again();
                return;
            }
        };
        // A reply that opens a dialog must not replace the box someone is typing in: it is
        // held aside while the reply is handled, then put back (and the reply said so).
        // (A peek's own screen arrives into the box it fills in: that one is not held.)
        let held = match &self.overlay {
            Some(Overlay::Prompt { .. } | Overlay::Task { .. } | Overlay::Peek { .. })
                if !matches!(pending, Pending::Peek(_)) =>
            {
                self.overlay.take()
            }
            _ => None,
        };
        self.handle_reply(pending, res);
        if let Some(h) = held {
            if self.overlay.is_some() {
                self.toast("a reply arrived while you were typing: ask again when you're done");
            }
            self.overlay = Some(h);
        }
    }

    fn handle_reply(&mut self, pending: Pending, res: Result<Reply, ptyro_core::Error>) {
        match (pending, res) {
            (
                p @ (Pending::Pad
                | Pending::PadAdded
                | Pending::PadItems { .. }
                | Pending::PadHistory),
                r,
            ) => self.pad_reply(p, r),
            (Pending::List, Ok(Reply::Sessions { sessions })) => self.got_list(sessions),
            (Pending::Registry, Ok(Reply::Profiles { profiles })) => self.profiles = profiles,
            (Pending::Spawned { split, back, .. }, Ok(Reply::Session { info })) => {
                if std::mem::take(&mut self.close_inspector_after) {
                    self.inspector = None;
                }
                let id = info.id;
                if back {
                    self.tool_back.insert(id);
                }
                self.refresh_again();
                self.upsert(info);
                self.show(id, split);
            }
            (Pending::Quick(style), Ok(Reply::Session { info })) => {
                let id = info.id;
                self.upsert(info);
                self.show_quick(style, id);
            }
            (Pending::Spawned { split, spec, .. }, Err(e))
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
                self.refresh_again();
                upsert_by(&mut self.worktrees, info.clone(), |w| w.id.clone());
                for s in [setup, agent].into_iter().flatten() {
                    self.upsert(s);
                }
                match pane_shown {
                    Some(id) => self.show(id, split),
                    None => self.reveal_worktree(&info.id, &info.project),
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
                    let file = self.project(&scripts.project).map(|p| p.root.join(".ptyro.toml"));
                    match file {
                        Some(f) => self.toast_offer(
                            "this project has no scripts yet",
                            false,
                            Offer { label: "add some in .ptyro.toml".into(), then: Then::Edit(f) },
                        ),
                        None => self.toast("this project has no scripts yet"),
                    }
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
            (Pending::Quit, Ok(_)) => self.quit = Some(AttachEnd::Stopped),
            (Pending::Quit, Err(e)) => {
                self.quitting = false;
                self.toast_err(format!("ptyro didn't stop: {}", e.message));
            }
            (Pending::Summarized, Ok(Reply::Session { info })) => {
                let text = info.summary.map(|s| s.text).unwrap_or_default();
                let text = format!("≈ {}: {text}", info.name);
                if self.config.summaries.enabled {
                    self.toast(text);
                } else {
                    let every = "after every turn: [summaries] enabled = true";
                    self.toast_offer(text, false, run_offer(every, "config.edit"));
                }
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
                } else if !scripts.names.iter().any(|n| n == "setup") && spec.setup {
                    // No setup yet: on a project's first worktree, what ptyro found (UX1.11).
                    let addr = spec.project.clone();
                    self.request(
                        Pending::Suggest(spec, split),
                        Request::ProjectSuggest { addr, apply: None },
                    );
                } else {
                    self.create_worktree(*spec, split);
                }
            }
            (Pending::PreCreate(spec, split), Err(_)) => self.create_worktree(*spec, split),
            (Pending::Suggest(spec, split), Ok(Reply::Suggestions { suggestions })) => {
                if suggestions.offered || suggestions.is_empty() {
                    self.create_worktree(*spec, split);
                } else {
                    self.overlay = Some(Overlay::Suggest { suggestions, spec, split });
                }
            }
            (Pending::Suggest(spec, split), Err(_)) => self.create_worktree(*spec, split),
            (Pending::SuggestShow(project), Ok(Reply::Suggestions { suggestions: s })) => {
                if s.is_empty() {
                    self.toast(format!(
                        "{}: nothing to suggest (its config has what it needs)",
                        s.project
                    ));
                } else {
                    let mut found = Vec::new();
                    if let Some(cmd) = &s.setup {
                        found.push(format!("setup: {cmd}"));
                    }
                    if !s.copy.is_empty() {
                        found.push(format!("copy {}", s.copy.join(", ")));
                    }
                    self.toast_offer(
                        format!("{}: {}", s.project, found.join(" · ")),
                        false,
                        Offer {
                            label: "use them (.ptyro.local.toml)".into(),
                            then: Then::Send(
                                Box::new(Request::ProjectSuggest {
                                    addr: project,
                                    apply: Some(true),
                                }),
                                "written to .ptyro.local.toml".into(),
                            ),
                        },
                    );
                }
            }
            (Pending::Suggested(spec, split), Ok(_)) => {
                self.toast("written to .ptyro.local.toml (yours, not committed)");
                let addr = spec.project.clone();
                self.request(
                    Pending::PreCreate(spec, split),
                    Request::ProjectScripts { addr, worktree: None },
                );
            }
            (Pending::Branches(w), Ok(Reply::Branches { names })) => {
                if names.is_empty() {
                    self.toast("no branch to check out: every one has a worktree");
                } else {
                    let items = names
                        .into_iter()
                        .map(|b| Item {
                            label: b.clone(),
                            detail: "check it out in a new worktree".into(),
                            key: None,
                            target: Target::Path(b.into()),
                        })
                        .collect();
                    self.overlay = Some(Overlay::Picker {
                        palette: Palette::new("On which branch?", items),
                        purpose: Purpose::Branch(w),
                    });
                }
            }
            (Pending::Removed(_), Ok(Reply::Removed { notes })) => self.toast(notes.join(" · ")),
            (Pending::Removed(id), Err(e)) if e.code == ErrorCode::DirtyWorktree => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "{}\n\na: archive it instead (its work is kept; restore brings it back)\ny: discard that work and remove it · n: neither",
                        e.message
                    ),
                    action: Confirmed::RemoveWorktree { id, force: true },
                });
            }
            (Pending::Gc, Ok(Reply::Gc { entries })) => {
                if entries.is_empty() {
                    self.toast("nothing stale: every worktree is in use");
                } else {
                    let items = entries
                        .iter()
                        .map(|e| Item {
                            label: e.worktree.qualified(),
                            detail: e.reasons.join(" · "),
                            key: None,
                            target: Target::Worktree(e.worktree.id.clone()),
                        })
                        .collect();
                    self.overlay = Some(Overlay::Picker {
                        palette: Palette::new(
                            "Stale worktrees: ⏎ archives one (its work is kept)",
                            items,
                        ),
                        purpose: Purpose::Gc,
                    });
                }
            }
            (Pending::Archives, Ok(Reply::Archives { entries })) => {
                if entries.is_empty() {
                    self.toast("nothing archived yet");
                } else {
                    let items = entries
                        .iter()
                        .map(|e| Item {
                            label: format!("{}/{}", e.project_slug, e.name),
                            detail: format!(
                                "{} · archived {} ago{}",
                                e.branch,
                                crate::ui::ago(e.archived_ms),
                                e.title.as_deref().map(|t| format!(" · {t}")).unwrap_or_default()
                            ),
                            key: None,
                            target: Target::Path(std::path::PathBuf::from(format!(
                                "{}/{}",
                                e.project_slug, e.name
                            ))),
                        })
                        .collect();
                    self.overlay = Some(Overlay::Picker {
                        palette: Palette::new("Restore an archived worktree", items),
                        purpose: Purpose::Restore,
                    });
                }
            }
            (Pending::ProjectAdded(then), Ok(Reply::Project { info, notes })) => {
                upsert_by(&mut self.projects, info.clone(), |p| p.id.clone());
                if let Some(then) = then {
                    self.after_add = Some((info.id.clone(), then));
                    self.try_after_add();
                }
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
            (Pending::FirstRunRepos, Ok(Reply::Paths { paths })) => self.first_run_repos(paths),
            (Pending::Home, Ok(Reply::Paths { paths })) => self.show_home(paths),
            (Pending::Home, Err(_)) => self.show_home(Vec::new()),
            (Pending::Discovered(then), Ok(Reply::Paths { paths })) => {
                self.open_add_project(paths, then)
            }
            (Pending::PaletteScripts(wt), Ok(Reply::Scripts { scripts })) => {
                let place = self.worktree(&wt).map(WorktreeInfo::qualified).unwrap_or_default();
                if let Some(Overlay::Picker { palette, purpose: Purpose::Palette }) =
                    &mut self.overlay
                {
                    palette.extend(scripts.names.iter().map(|n| Item {
                        label: format!("script {}", n.trim_start_matches("run:")),
                        detail: format!("run it in {place}"),
                        key: None,
                        target: Target::RunScript(wt.clone(), n.clone()),
                    }));
                }
            }
            (Pending::Scripts(wt), Ok(Reply::Scripts { scripts })) => {
                self.open_script_picker(wt, scripts)
            }
            (Pending::Spawned { .. } | Pending::Done(_), Err(e))
                if e.code == ErrorCode::UntrustedProject =>
            {
                self.toast_offer(
                    e.message,
                    true,
                    run_offer("review and trust them", "project.trust"),
                );
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
            (Pending::WorkspaceRenamed(name), Ok(_)) => {
                self.workspace = name.clone();
                self.toast(format!("workspace {name}"));
            }
            (Pending::WorkspaceMade(name), Ok(reply)) => {
                // Known now, not when its event arrives: the sidebar shows it at once (UX3.3).
                if let Reply::Workspace { info } = reply {
                    upsert_by(&mut self.workspaces, info, |w| w.id.clone());
                }
                self.toast(format!("workspace {name} created"));
                self.switch_workspace(name);
            }
            (Pending::Done(msg), Ok(_)) => {
                if !msg.is_empty() {
                    self.toast(msg);
                }
            }
            (Pending::Saved, Ok(_)) => {}
            (Pending::Again(ws), Ok(Reply::LastLaunch { launch })) => {
                if ws == self.workspace {
                    self.again = launch;
                }
            }
            (_, Err(e)) => match offer_for(&e.message) {
                Some(offer) => self.toast_offer(e.message, true, offer),
                None => self.toast_err(e.message),
            },
            _ => {}
        }
    }

    // ── showing sessions ────────────────────────────────────────────────────

    /// Show `id`: in a new split when one is asked for, else without a split (UX3.1, `place`):
    /// where it shows already, in the kept layout that has it, or full sized.
    pub fn show(&mut self, id: SessionId, split: Option<Dir>) {
        let pane = match split {
            // The drawer hangs from its host: splitting it would nest a pane inside it.
            Some(_) if self.drawer.is_some_and(|d| d.pane == self.focused) => {
                self.toast_err("the drawer can't be split: focus another pane first");
                return;
            }
            Some(dir) => match self.layout.split(self.focused, dir) {
                Some(p) => {
                    self.panes.insert(p, PaneState::default());
                    self.zoom = false;
                    p
                }
                // Not shown in the pane that has a session already: that one stays as it is.
                None => {
                    self.toast_err(format!(
                        "at most {} panes: close one to open another (it is in the sidebar)",
                        crate::layout::MAX_PANES
                    ));
                    return;
                }
            },
            None => self.place(id),
        };
        let p = self.panes.entry(pane).or_default();
        if p.session != Some(id) {
            p.last = p.session;
            p.session = Some(id);
            p.copy = None;
        }
        self.focused = pane;
        self.focus = Focus::Pane;
        self.sidebar_zoom = false;
        // A session from another workspace brings its workspace along.
        if let Some(s) = self.session(id).cloned()
            && !self.ordered().contains(&id)
        {
            let ws = self.home_of(&s);
            if ws != self.workspace {
                self.workspace = ws;
            }
        }
        self.reveal(id);
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

    pub(crate) fn set_sel(&mut self, rows: &[Row], i: usize) {
        self.sidebar_sel = i;
        self.sidebar_row = rows.get(i).cloned();
        if self.focus == Focus::Sidebar
            && let Some(w) = tree::Inputs::workspace_of(rows, i)
        {
            self.enter_workspace(w);
        }
    }

    /// The workspace you're in (MM1.B: all are on screen at once) is the selected row's in the
    /// sidebar, else the focused session's: what `n`, quick new, the status bar and new
    /// workspace-wide things go by.
    pub(crate) fn enter_workspace(&mut self, w: String) {
        if w != self.workspace {
            self.workspace = w;
            self.view_dirty_since.get_or_insert_with(Instant::now);
            self.refresh_again();
        }
    }

    /// After the focus moved to a pane: its session's workspace.
    fn follow_focused_workspace(&mut self) {
        if self.focus == Focus::Pane
            && let Some(s) = self.focused_session().and_then(|id| self.session(id))
        {
            let w = self.tree().home(s);
            self.enter_workspace(w);
        }
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
            if self.overlay.is_none() && !self.prefix {
                self.forward_popup(InputEvent::Key { key });
            }
            return;
        }
        if self.first_run.is_some() && !self.prefix && !self.keymap.is_prefix(key) {
            self.first_run_key(key);
            return;
        }
        if self.overlay.is_some() {
            // The prefix always works: it closes whatever is open and starts a command.
            if self.keymap.is_prefix(key) && !matches!(self.overlay, Some(Overlay::Prompt { .. })) {
                self.overlay = None;
                self.start_prefix();
                return;
            }
            // A prefix still waiting when the overlay came up (a dialog an event opened) must
            // not turn the overlay's closing key into a command afterwards.
            self.prefix = false;
            self.overlay_key(key);
            return;
        }
        // The scratchpad's popup has the keyboard; the prefix still reaches ptyro (NOTE1.1).
        if self.pad_popup.is_some() && !self.prefix && !self.keymap.is_prefix(key) {
            self.forward_popup(InputEvent::Key { key });
            return;
        }
        if self.orch.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.start_prefix();
            } else {
                self.orch_key(key);
            }
            return;
        }
        if self.sets_view.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.start_prefix();
            } else {
                self.sets_key(key);
            }
            return;
        }
        if self.inspector.is_some() && !self.prefix {
            if self.keymap.is_prefix(key) {
                self.start_prefix();
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
                None => self.toast_offer(
                    format!(
                        "{} {}: not bound",
                        ptyro_core::config::key_label(&self.keymap.prefix),
                        ptyro_core::config::key_label(&keymap::normalize(key))
                    ),
                    false,
                    run_offer("list the keys", "help"),
                ),
            }
            return;
        }
        if self.keymap.is_prefix(key) {
            self.start_prefix();
            return;
        }
        if let Some(cmd) = self.keymap.lookup(Table::Global, key).cloned() {
            self.run(&cmd);
            return;
        }
        // (With the sidebar focused, the sidebar's keys: copy mode waits in its pane.)
        if self.focus == Focus::Pane && self.in_copy() {
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

    /// A full-screen view (inspector, runs, change sets, first run) is drawn over the panes:
    /// the mouse and pastes are not for what is hidden under it.
    fn full_view(&self) -> bool {
        self.inspector.is_some()
            || self.orch.is_some()
            || self.sets_view.is_some()
            || self.first_run.is_some()
    }

    pub fn on_paste(&mut self, text: String) {
        // Into the box that's open, new lines kept where it takes several (UX1.26); never on
        // to the session behind it.
        let lines = text.replace("\r\n", "\n").replace('\r', "\n");
        let flat = || lines.replace('\n', " ");
        let (full, copying) = (self.full_view(), self.in_copy());
        match &mut self.overlay {
            Some(Overlay::Prompt { text: t, kind, .. }) => {
                t.push_str(&if kind.multiline() { lines.clone() } else { flat() });
                *t = kind.fit(t);
            }
            Some(Overlay::Task { text: t, name, naming, .. }) => {
                if *naming {
                    let n = name.get_or_insert_with(String::new);
                    n.push_str(&flat());
                    *n = ptyro_core::names::typed(n);
                } else {
                    t.push_str(&lines);
                }
            }
            Some(Overlay::Peek { reply, .. }) => reply.push_str(&lines),
            Some(Overlay::Settings(p)) => p.paste(&text),
            Some(Overlay::Picker { palette, .. }) => palette.paste(&text),
            Some(Overlay::Help { filter, scroll }) => {
                filter.push_str(&flat());
                *scroll = 0;
            }
            Some(_) => {}
            None if full => {}
            None if self.pad_popup.is_some() => {
                self.forward_popup(InputEvent::Paste { text });
                return;
            }
            None if copying => {}
            None => {
                self.forward(InputEvent::Paste { text });
                return;
            }
        }
        self.dirty = true;
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

    /// Send input to the scratchpad's popup editor.
    fn forward_popup(&self, event: InputEvent) {
        if let Some(id) = self.pad_popup
            && !self.opts.view_only
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
        let before = self.quick_stance();
        self.run_command(cmd);
        self.tuck_away(before);
    }

    fn run_command(&mut self, cmd: &Command) {
        self.dirty = true;
        let focused = self.focused_session();
        match cmd.action {
            "help" => self.overlay = Some(Overlay::Help { filter: String::new(), scroll: 0 }),
            "palette" => self.open_palette(),
            "app.detach" => self.quit = Some(AttachEnd::Detached),
            "app.quit" => self.confirm_quit(),
            "session.new" => self.open_task(None),
            "session.again" => self.quick_new(),
            "attention.next" => self.next_attention(),
            "inbox" => {
                self.overlay =
                    Some(Overlay::Inbox { selected: 0, tab: Some(self.workspace.clone()) })
            }
            "peek" => {
                let target = if self.focus == Focus::Sidebar {
                    self.selected_row().and_then(|r| r.session())
                } else {
                    self.attention_order(false).first().copied().or(focused)
                };
                match target {
                    Some(id) => self.open_peek(id),
                    None => self.toast("nothing to peek at"),
                }
            }
            "session.resume" | "session.hibernate" | "session.explain" | "session.mute"
            | "session.adopt" | "session.summarize" => {
                let target = if self.focus == Focus::Sidebar {
                    self.selected_row().and_then(|r| r.session())
                } else {
                    focused
                };
                let Some(id) = target else {
                    self.toast_offer(
                        "no session here",
                        false,
                        run_offer("start one", "session.new"),
                    );
                    return;
                };
                let addr = id.full();
                match cmd.action {
                    "session.resume" => {
                        self.request(Pending::Done("resumed".into()), Request::Resume { addr })
                    }
                    "session.hibernate" => self
                        .request(Pending::Done("hibernated".into()), Request::Hibernate { addr }),
                    "session.adopt" => {
                        self.toast("adopting: Claude exits here and comes back managed…");
                        self.request(
                            Pending::Spawned { split: None, spec: Box::default(), back: false },
                            Request::Adopt { addr },
                        )
                    }
                    "session.summarize" => {
                        self.toast("summarizing (a small model, on its profile's account)…");
                        self.request(Pending::Summarized, Request::Summarize { addr })
                    }
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
                        let suggested = Some(self.free_worktree_name(&project, None));
                        let w = Wizard { project: Some(project), suggested, ..Wizard::default() };
                        self.resolve_for(&w);
                        self.overlay = Some(Overlay::Prompt {
                            title: format!(
                                "{} — name",
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
                    None => self.pick_project(Some(AfterAdd::Run("worktree.new"))),
                }
            }
            "workspace.switch" => self.open_workspaces(),
            "worktree.open" => match self.context().1 {
                Some(wt) => self.open_editors(wt),
                None => self.pick_project(Some(AfterAdd::Run("worktree.open"))),
            },
            "script.run" => match self.context().1 {
                Some(wt) => self.run_script(wt, None),
                None => self.pick_project(Some(AfterAdd::Run("script.run"))),
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
                None => self.pick_project(Some(AfterAdd::Run("script.pick"))),
            },
            "project.add" => self.pick_project(None),
            "project.suggest" => match self.context().0 {
                Some(p) => self.request(
                    Pending::SuggestShow(p.clone()),
                    Request::ProjectSuggest { addr: p, apply: None },
                ),
                None => self.pick_project(Some(AfterAdd::Run("project.suggest"))),
            },
            "project.trust" => match self.context().0 {
                Some(p) => self.request(
                    Pending::TrustDialog(AfterTrust::Nothing),
                    Request::ProjectScripts { addr: p, worktree: None },
                ),
                None => self.pick_project(Some(AfterAdd::Run("project.trust"))),
            },
            "worktree.finish" | "worktree.archive" => {
                match self.context().1.and_then(|w| self.worktree(&w).cloned()) {
                    Some(w) if w.is_root => {
                        self.toast("that's the main checkout: finish a worktree of it")
                    }
                    // Shipped: finishing it is cleaning up (the hint says so, UX1.12).
                    Some(w) if cmd.action == "worktree.archive" || w.shipped() => {
                        self.confirm_archive(&w)
                    }
                    Some(w) => {
                        let items = vec![
                            Item {
                                label: "a pull request".into(),
                                detail: "commit what's left, push, open a PR".into(),
                                key: Some("p".into()),
                                target: Target::Action(Command { action: "pr", arg: None }),
                            },
                            Item {
                                label: "integrate".into(),
                                detail: "commit what's left, rebase onto the main branch and fast-forward it".into(),
                                key: Some("i".into()),
                                target: Target::Action(Command { action: "integrate", arg: None }),
                            },
                        ];
                        self.overlay = Some(Overlay::Picker {
                            palette: Palette::new(format!("Finish {}", w.qualified()), items)
                                .with_hotkeys(),
                            purpose: Purpose::Finish(w.id.clone()),
                        });
                    }
                    None => self.pick_project(Some(AfterAdd::Run("worktree.finish"))),
                }
            }
            "worktree.restore" => {
                self.request(Pending::Archives, Request::WorktreeArchives { project: None })
            }
            "worktree.gc" => self.request(Pending::Gc, Request::Gc),
            "shell.new" => self.new_shell(),
            a if a.starts_with("machine.") => self.machine_action(a),
            "shell.here" | "shell.drawer" | "shell.pane" => {
                // A quick terminal row is that one; anything else is where you are.
                let want = match (self.focus, self.selected_row()) {
                    (Focus::Sidebar, Some(Row::Quick(id))) => Some(id),
                    _ => None,
                };
                // It belongs to a worktree (UX3.4): with none here, pick a project, then go on.
                if want.is_none() && self.context().1.is_none() {
                    self.pick_project(Some(AfterAdd::Run(cmd.action)));
                    return;
                }
                if cmd.action == "shell.here" {
                    self.toggle_quick(want, None, None);
                    return;
                }
                let style = if cmd.action == "shell.pane" {
                    ptyro_core::config::ShellStyle::Pane
                } else {
                    ptyro_core::config::ShellStyle::Drawer
                };
                self.toggle_quick_as(style, want, None, None);
            }
            "scratchpad.toggle" => self.toggle_pad(),
            "scratchpad.add" => self.open_pad_add(String::new(), None),
            "scratchpad.note" => self.note_this(),
            "scratchpad.items" => self.open_pad_items(),
            "scratchpad.history" => self.open_pad_history(),
            "worktree.retitle" => match self.context().1.and_then(|w| self.worktree(&w).cloned()) {
                Some(w) => {
                    self.overlay = Some(Overlay::Prompt {
                        title: format!(
                            "Title of {} (what it's for; empty clears it)",
                            w.qualified()
                        ),
                        text: w.title.clone().unwrap_or_default(),
                        kind: PromptKind::RetitleWorktree(w.id.clone()),
                    })
                }
                None => self.pick_project(Some(AfterAdd::Run("worktree.retitle"))),
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
                if !self.go_back() {
                    self.toast("no previous session in this pane");
                }
            }
            "session.stop"
            | "session.kill"
            | "session.restart"
            | "session.rename"
            | "session.retitle"
            | "session.take_control" => {
                let target = if self.focus == Focus::Sidebar {
                    match self.selected_row() {
                        Some(Row::Session(id) | Row::Quick(id)) => Some(id),
                        Some(Row::Worktree(w)) if cmd.action == "session.stop" => {
                            if let Some(w) = self.worktree(&w).cloned().filter(|w| !w.is_root) {
                                // Shipped (merged, or integrated into main): clean up by archiving,
                                // which never refuses (UX1.12).
                                if w.shipped() {
                                    self.confirm_archive(&w);
                                } else {
                                    self.confirm_remove(&w);
                                }
                            }
                            return;
                        }
                        _ => None,
                    }
                } else {
                    focused
                };
                let Some(id) = target else {
                    self.toast_offer(
                        "no session here",
                        false,
                        run_offer("start one", "session.new"),
                    );
                    return;
                };
                self.session_action(cmd.action, id);
            }
            "focus.toggle" if self.sidebar_zoom => {
                self.sidebar_zoom = false;
                self.focus = Focus::Pane;
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
                        if !self.sidebar {
                            self.toast_offer(
                                "the sidebar is hidden",
                                false,
                                run_offer("show it", "sidebar.toggle"),
                            );
                        } else if self.zoom {
                            self.toast("the sidebar is hidden while a pane is zoomed");
                        } else {
                            self.toast("the terminal is too narrow for the sidebar");
                        }
                        Focus::Pane
                    }
                    Focus::Sidebar => Focus::Pane,
                };
            }
            "row.menu" => self.open_row_menu(),
            "home" => self.open_home(),
            "app.about" => self.open_about(),
            "health" => self.open_health(),
            "daemon.logs" => self.run_cli(&["daemon".into(), "logs".into(), "-f".into()]),
            "daemon.status" => self.run_cli(&["daemon".into(), "status".into()]),
            "daemon.restart" => self.run_cli(&["daemon".into(), "restart".into()]),
            "daemon.upgrade" => self.run_cli(&["daemon".into(), "upgrade".into()]),
            "daemon.service" => self.run_cli(&["daemon".into(), "install-service".into()]),
            "daemon.unservice" => self.run_cli(&["daemon".into(), "uninstall-service".into()]),
            "doctor" => self.run_cli(&["doctor".into()]),
            "integrations" => self.run_cli(&["integrations".into(), "ls".into()]),
            "integrations.install" | "integrations.uninstall" => {
                let verb = cmd.action.trim_start_matches("integrations.");
                self.pick_cli(
                    &format!("{verb} which agent's integration?"),
                    &["integrations", verb],
                    &[("gemini", "a linked extension"), ("antigravity", "its hooks")],
                );
            }
            "first.run" => self.open_first_run(),
            "toast.act" => match self.toast.take().and_then(|t| t.offer) {
                Some(Offer { then: Then::Run(cmd), .. }) => self.run(&cmd),
                Some(Offer { then: Then::Send(req, done), .. }) => {
                    self.request(Pending::Done(done), *req)
                }
                Some(Offer { then: Then::Edit(path), .. }) => self.edit_file(path, None),
                None => self.toast("nothing to undo or fix right now"),
            },
            "sidebar.wider" => self.set_sidebar_width(self.sidebar_width + 1),
            "sidebar.narrower" => self.set_sidebar_width(self.sidebar_width.saturating_sub(1)),
            "sidebar.zoom" => {
                self.sidebar_zoom = !self.sidebar_zoom;
                if self.sidebar_zoom {
                    let rows = self.rows();
                    if let Some(i) = self
                        .focused_session()
                        .and_then(|id| rows.iter().position(|r| *r == Row::Session(id)))
                    {
                        self.set_sel(&rows, i);
                    }
                    self.focus = Focus::Sidebar;
                } else {
                    self.focus = Focus::Pane;
                }
            }
            "sidebar.toggle" => {
                self.sidebar = !self.sidebar;
                if !self.sidebar {
                    self.focus = Focus::Pane;
                }
                self.view_changed();
            }
            "pane.split_right" => self.split_clone(Dir::Right),
            "pane.split_down" => self.split_clone(Dir::Down),
            "pane.close" => {
                if self.close_pane(self.focused) {
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
                None => self.toast_offer(
                    "nothing yanked yet",
                    false,
                    Offer {
                        label: "copy something".into(),
                        then: Then::Run(Command { action: "copy.enter", arg: None }),
                    },
                ),
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
                    None => match self.focused_session().filter(|id| {
                        self.session(*id).is_some_and(|s| s.kind == SessionKind::Agent)
                    }) {
                        // An agent in its own UI: offer to switch it, same conversation (M10.6).
                        Some(id) => self.toast_offer(
                            "only structured sessions have a chat view",
                            false,
                            Offer {
                                label: "switch this one".into(),
                                then: Then::Send(
                                    Box::new(Request::Convert {
                                        addr: id.full(),
                                        structured: true,
                                        force: false,
                                    }),
                                    "switching to structured mode (same conversation)…".into(),
                                ),
                            },
                        ),
                        None => self.toast("only an agent's structured session has a chat view"),
                    },
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
            "config.panel" => self.open_settings(cmd.arg.as_deref()),
            "config.edit" => {
                let argv = vec![
                    std::env::current_exe()
                        .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned()),
                    "config".into(),
                    "edit".into(),
                ];
                self.spawn_tool(argv, None, true);
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
                    self.toast_offer(
                        "mark sessions with Space in the sidebar first",
                        false,
                        run_offer("go to the sidebar", "focus.toggle"),
                    );
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
            // The tour runs in a ptyro of its own: this TUI steps aside for it, then comes back.
            "help.tour" => match self.tour {
                Some(_) => {
                    self.tour = Some(crate::tour::Tour::default());
                    self.toast("the tour starts over");
                }
                None => self.quit = Some(AttachEnd::Tour),
            },
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
                Some(Row::Session(id)) if cmd.action == "nav.open" => {
                    self.sidebar_zoom = false;
                    self.show(id, None)
                }
                Some(Row::Quick(id)) if cmd.action == "nav.open" => {
                    self.sidebar_zoom = false;
                    self.toggle_quick(Some(id), None, None);
                }
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
            other if crate::cli_actions::template(other).is_some() => self.start_cli(other),
            other => self.native_extra(other),
        }
    }

    /// The sidebar's width, kept within 12–80 and leaving the panes at least 21 columns, saved
    /// with the workspace's view (UX1.7, UX1.8).
    fn set_sidebar_width(&mut self, width: u16) {
        let max = SIDEBAR_MAX.min(self.area.width.saturating_sub(22)).max(12);
        let w = width.clamp(12, max);
        if w != self.sidebar_width {
            self.sidebar_width = w;
            self.view_changed();
        }
    }

    fn focus_pane(&mut self, p: PaneId) {
        self.focused = p;
        self.focus = Focus::Pane;
        self.view_changed();
    }

    pub fn set_theme(&mut self, name: String) {
        self.theme = Theme::resolve(&self.config, &name, no_color());
        self.toast_offer(
            format!("theme {name} (set ui.theme to keep it)"),
            false,
            run_offer("keep it in Settings", "config.panel"),
        );
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
            "session.stop" => {
                self.stopping.insert(id);
                self.request(
                    Pending::Done(format!("stopped {}", info.name)),
                    Request::Kill { addr, signal: None },
                );
            }
            "session.kill" => {
                self.stopping.insert(id);
                self.request(
                    Pending::Done(format!("killed {}", info.name)),
                    Request::Kill { addr, signal: Some(9) },
                );
            }
            "session.restart" if info.state.is_live() => self.toast_offer(
                format!("{} is still running", info.name),
                false,
                run_offer("stop it", "session.stop"),
            ),
            "session.restart" => self.request(
                Pending::Done(format!("restarted {}", info.name)),
                Request::Restart { addr },
            ),
            "session.rename" => {
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Rename {} (its address; C-t: its title)", info.name),
                    text: info.name.clone(),
                    kind: PromptKind::Rename(id),
                });
            }
            "session.retitle" => self.open_retitle(id),
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

    /// The title prompt (UX1.24), filled with the current title.
    fn open_retitle(&mut self, id: SessionId) {
        let Some(info) = self.session(id) else { return };
        let current = info.title.clone().filter(|t| info.window_title.as_ref() != Some(t));
        self.overlay = Some(Overlay::Prompt {
            title: format!("Title of {} (what people read; empty clears it)", info.name),
            text: current.unwrap_or_default(),
            kind: PromptKind::Retitle(id),
        });
    }

    pub(crate) fn client_env() -> BTreeMap<String, String> {
        ptyro_pty::CLIENT_ENV
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
            .collect()
    }

    /// "Here": the focused session's directory, else the TUI's own.
    pub(crate) fn here(&self) -> std::path::PathBuf {
        self.focused_session()
            .and_then(|id| self.session(id))
            .and_then(|s| s.live_cwd.clone().or_else(|| Some(s.cwd.clone())))
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| "/".into())
    }

    /// A spawn in the focused session's directory (or the current one).
    pub(crate) fn spawn(&mut self, argv: Vec<String>, split: Option<Dir>, name: Option<String>) {
        let spec = SpawnSpec { argv, cwd: self.here(), name, ..SpawnSpec::default() };
        self.spawn_spec(spec, split);
    }

    /// Send a spawn with this client's environment, size and workspace filled in.
    pub(crate) fn spawn_spec(&mut self, spec: SpawnSpec, split: Option<Dir>) {
        self.spawn_spec_as(spec, split, false);
    }

    /// `spawn_spec`; `back`: a tool whose screen goes back to what it replaced when it exits.
    pub(crate) fn spawn_spec_as(&mut self, mut spec: SpawnSpec, split: Option<Dir>, back: bool) {
        spec.env = Self::client_env();
        spec.size = spec
            .size
            .or_else(|| self.focused_session().and_then(|id| self.sizes.get(&id).copied()));
        spec.workspace = self.workspace_for(spec.worktree.as_deref(), None, spec.workspace);
        if spec.cwd.as_os_str().is_empty() {
            spec.cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        }
        let req = Request::Spawn { spec: spec.clone() };
        self.request(Pending::Spawned { split, spec: Box::new(spec), back }, req);
    }

    /// The workspace a spawn or a new worktree is made in. A worktree (or project) shown in a
    /// workspace goes there, whichever machine that is: the selected row may be in another
    /// workspace, and a request naming a workspace on one machine and a worktree of another
    /// can't be served by either. Else `given`, else the current workspace.
    fn workspace_for(
        &self,
        worktree: Option<&str>,
        project: Option<&str>,
        given: Option<String>,
    ) -> Option<String> {
        let project = project.map(str::to_owned).or_else(|| {
            let id = worktree?;
            self.worktrees.iter().find(|w| w.id == id).map(|w| w.project.clone())
        });
        let owner = project.and_then(|p| {
            let tree = self.tree();
            let order = tree.order();
            // The current workspace first, when it lists the project too.
            order
                .iter()
                .find(|w| w.name == self.workspace && w.projects.contains(&p))
                .or_else(|| order.iter().find(|w| w.projects.contains(&p)))
                .map(|w| w.name.clone())
        });
        owner.or(given).or_else(|| Some(self.workspace.clone()))
    }

    fn create_worktree(&mut self, spec: WorktreeSpec, split: Option<Dir>) {
        let mut spec = spec;
        spec.env = Self::client_env();
        spec.size = spec
            .size
            .or_else(|| self.focused_session().and_then(|id| self.sizes.get(&id).copied()));
        spec.workspace = self.workspace_for(None, Some(&spec.project), spec.workspace);
        let req = Request::WorktreeNew { spec: spec.clone() };
        self.request(Pending::WorktreeCreated { split, spec: Box::new(spec) }, req);
    }

    // ── the new-session wizard (SPEC §11.5, M3.13) ─────────────────────────

    /// Ask the daemon which profile a spawn there would get (shown in the wizard title).
    pub(crate) fn resolve_for(&mut self, w: &Wizard) {
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
            workspace: self.workspace_for(target.as_deref(), None, None),
            worktree: target,
            ..SpawnSpec::default()
        };
        self.request(Pending::Resolved, Request::ProfileResolve { spec });
    }

    /// A workspace name to offer: a defined profile's without a workspace yet (`work`), else
    /// `workspace-2`, `-3`… (UX1.24).
    fn free_workspace_name(&self) -> String {
        let taken = |n: &str| self.workspaces.iter().any(|w| w.name == n);
        self.profiles
            .iter()
            .filter(|p| p.defined && p.name != ptyro_core::model::DEFAULT_PROFILE)
            .map(|p| p.name.clone())
            .find(|n| !taken(n))
            .unwrap_or_else(|| {
                (2..).map(|k| format!("workspace-{k}")).find(|n| !taken(n)).expect("infinite")
            })
    }

    /// A worktree name not taken in `project`: `want` (a slug of a prompt) or a friendly pair,
    /// with `-2`, `-3` when taken (UX1.24).
    pub fn free_worktree_name(&self, project: &str, want: Option<&str>) -> String {
        let taken = |n: &str| self.worktrees.iter().any(|w| w.project == project && w.name == n);
        match want {
            Some(n) if !taken(n) => n.to_owned(),
            Some(n) => (2..).map(|k| format!("{n}-{k}")).find(|c| !taken(c)).expect("infinite"),
            None => ptyro_core::names::friendly(SessionId::new().seed(), taken),
        }
    }

    /// `n` (UX1.9): "what should it do?" in the context's project (or here).
    pub fn open_task(&mut self, split: Option<Dir>) {
        let (project, worktree) = self.context();
        let wizard = Wizard { project, worktree, split, ..Wizard::default() };
        self.resolve_for(&wizard);
        self.overlay =
            Some(Overlay::Task { text: String::new(), name: None, naming: false, wizard });
    }

    /// What the task box will make of what's typed: the name (an edited one, else a slug of the
    /// prompt, else a random one when the daemon picks), the agent, and where.
    pub fn task_plan(
        &self,
        text: &str,
        name: Option<&str>,
        w: &Wizard,
    ) -> (Option<String>, String, String) {
        let agent = ptyro_agents::default_agent(&self.config);
        // A PR or issue pasted in: a worktree on it (UX1.10).
        if let Some((pr, issue)) = work_source(text) {
            let slug = name.map(str::to_owned).or_else(|| pr.map(|n| format!("pr-{n}")));
            let place = match (pr, &issue) {
                (Some(n), _) => format!("PR #{n}'s branch, in a new worktree"),
                (None, Some(i)) => {
                    format!("a new worktree for issue {}", i.rsplit('/').next().unwrap_or(i))
                }
                _ => String::new(),
            };
            return (slug, agent, place);
        }
        let slug = name.map(str::to_owned).or_else(|| ptyro_core::names::from_text(text)).map(
            |n| match &w.project {
                Some(p) if name.is_none() => self.free_worktree_name(p, Some(&n)),
                _ => n,
            },
        );
        let place = match w.project.as_ref().and_then(|p| self.project(p)) {
            Some(p) => format!("new worktree in {}", p.slug),
            None => format!("here, {}", self.here().display()),
        };
        (slug, agent, place)
    }

    /// A key for `action` where the focus is: the sidebar's own (no prefix), else after the
    /// prefix.
    fn hint_key(&self, action: &str) -> Option<String> {
        if self.focus == Focus::Sidebar
            && let Some(k) = self.keymap.key_in(Table::Nav, action)
        {
            return Some(k);
        }
        self.keymap.key_for(action)
    }

    /// The status bar's hint for what's selected (UX1.16): what you'd most likely do next with
    /// it, and the key. `None` when there's nothing in particular.
    pub fn context_hint(&self) -> Option<String> {
        let row = match self.focus {
            Focus::Sidebar => self.selected_row()?,
            Focus::Pane if self.focused_session().is_some_and(|s| self.is_pad(s)) => return None,
            Focus::Pane if self.in_drawer() => Row::Quick(self.focused_session()?),
            Focus::Pane => Row::Session(self.focused_session()?),
        };
        let with = |action: &str, what: &str| self.hint_key(action).map(|k| format!("{k} {what}"));
        let worktree_hint = |w: &WorktreeInfo| {
            if w.shipped() {
                let what = if w.pr.as_ref().is_some_and(|p| p.state == "MERGED") {
                    "clean up (merged)"
                } else {
                    "clean up (in main)"
                };
                return with("worktree.finish", what).or_else(|| with("worktree.remove", what));
            }
            let changed = w.status.as_ref().is_some_and(|s| s.insertions + s.deletions > 0);
            changed.then(|| with("inspector", "review its changes")).flatten()
        };
        match row {
            Row::Session(id) => {
                let s = self.session(id)?;
                if s.state == ProcessState::Dormant {
                    return Some("⏎ resumes it".into());
                }
                if !s.state.is_live() {
                    let restart = if self.focus == Focus::Sidebar { "R" } else { "⏎" };
                    return Some(format!(
                        "{restart} restart · {}",
                        with("session.stop", "remove").unwrap_or_default()
                    ));
                }
                if s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked) {
                    return with("peek", "answer it");
                }
                if s.inner_agent.as_deref() == Some("claude") {
                    return with("session.adopt", "adopt it (hooks, resume)");
                }
                if let Some(w) = s.worktree.as_deref().and_then(|w| self.worktree(w))
                    && s.status.as_ref().is_none_or(|r| r.status != AgentStatus::Working)
                    && let Some(h) = worktree_hint(w)
                {
                    return Some(h);
                }
                if self.attention_order(false).iter().any(|a| *a != id) {
                    return with("attention.next", "who needs you");
                }
                None
            }
            Row::Worktree(w) => self
                .worktree(&w)
                .and_then(worktree_hint)
                .or_else(|| with("session.new", "start a task here")),
            Row::Project(_) => with("worktree.new", "new worktree"),
            // A quick terminal (UX1.25): the key that shows or hides it.
            Row::Quick(id) => {
                let s = self.session(id)?;
                if s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked) {
                    return with("peek", "answer it");
                }
                if self.focus == Focus::Pane {
                    return self.quick_key().map(|k| format!("{k} hide it"));
                }
                let open = self.drawer_session() == Some(id) || self.quick_panel(id).is_some();
                Some(format!("⏎ {} it", if open { "hide" } else { "show" }))
            }
            // A workspace on another machine that isn't reachable says why, and what to do.
            Row::Workspace(w) => match self.machine_of_name(&w) {
                Some(m) if m.state != ptyro_core::model::MachineState::Online => Some(
                    m.fix.clone().unwrap_or_else(|| format!("{} is {}", m.name, m.state.label())),
                ),
                _ => with("session.new", "start a task here"),
            },
            Row::Set(_) | Row::Group(_) => None,
        }
    }

    /// The UX1.21 actions done here rather than through the CLI.
    fn native_extra(&mut self, action: &str) {
        let (project, wt) = self.context();
        let worktree = wt.as_deref().and_then(|w| self.worktree(w)).cloned();
        let need_wt = |me: &mut Self| {
            me.toast_offer("pick a worktree first", false, run_offer("make one", "worktree.new"))
        };
        match action {
            "session.convert" => {
                match self.focused_session().and_then(|id| self.session(id)).cloned() {
                    Some(s) if s.kind == SessionKind::Agent => {
                        let structured = s.structured.is_none();
                        self.request(
                            Pending::Spawned { split: None, spec: Box::default(), back: false },
                            Request::Convert { addr: s.id.full(), structured, force: false },
                        );
                        self.toast(if structured {
                            "to structured mode, same conversation…"
                        } else {
                            "to its own UI, same conversation…"
                        });
                    }
                    _ => self.toast("only an agent session converts"),
                }
            }
            "project.remove" | "project.untrust" | "project.pin" | "project.unpin"
            | "project.leave" => {
                let Some(p) = project.and_then(|p| self.project(&p).cloned()) else {
                    self.pick_project(None);
                    return;
                };
                match action {
                    "project.remove" => {
                        self.overlay = Some(Overlay::Confirm {
                            text: format!(
                                "Forget project {}? Its worktrees stay on disk, and it won't register on its own again. [y/N]",
                                p.slug
                            ),
                            action: Confirmed::Send(
                                Box::new(Request::ProjectRemove { addr: p.id, release: false }),
                                "forgotten".into(),
                            ),
                        });
                    }
                    "project.untrust" => self.request(
                        Pending::Done("scripts no longer trusted".into()),
                        Request::ProjectUntrust { addr: p.id },
                    ),
                    "project.unpin" => self.request(
                        Pending::Done("unpinned".into()),
                        Request::ProjectPin { addr: p.id, profile: None },
                    ),
                    "project.leave" => self.request(
                        Pending::Done(format!("{} left {}", p.slug, self.workspace)),
                        Request::WorkspaceMember {
                            name: self.workspace.clone(),
                            project: p.id,
                            remove: true,
                        },
                    ),
                    _ => {
                        let items = self
                            .profiles
                            .iter()
                            .map(|x| Item {
                                label: x.name.clone(),
                                detail: x.label.clone(),
                                key: None,
                                target: Target::Path(x.name.clone().into()),
                            })
                            .collect();
                        self.overlay = Some(Overlay::Picker {
                            palette: Palette::new(
                                format!("Pin {} to which profile?", p.slug),
                                items,
                            ),
                            purpose: Purpose::Pin(p.id),
                        });
                    }
                }
            }
            "worktree.lock" | "worktree.unlock" => match worktree {
                Some(w) => self.request(
                    Pending::Done(
                        if action == "worktree.lock" { "locked" } else { "unlocked" }.into(),
                    ),
                    Request::WorktreeLock { addr: w.id, lock: action == "worktree.lock" },
                ),
                None => need_wt(self),
            },
            "worktree.pair" | "worktree.unpair" => match worktree {
                Some(w) => self.request(
                    Pending::Done(String::new()),
                    Request::WorktreePair {
                        addr: w.id,
                        off: action == "worktree.unpair",
                        refresh: false,
                    },
                ),
                None => need_wt(self),
            },
            "worktree.rename_branch" => match worktree {
                Some(w) => {
                    self.overlay = Some(Overlay::Prompt {
                        title: format!(
                            "Rename {}'s branch (empty: from its first commit)",
                            w.qualified()
                        ),
                        text: w.branch.clone().unwrap_or_default(),
                        kind: PromptKind::RenameBranch(w.id),
                    });
                }
                None => need_wt(self),
            },
            "workspace.rename" => {
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Rename workspace {}", self.workspace),
                    text: self.workspace.clone(),
                    kind: PromptKind::RenameWorkspace(self.workspace.clone()),
                });
            }
            "workspace.remove" => {
                let name = self.workspace.clone();
                self.overlay = Some(Overlay::Confirm {
                    text: format!("Remove workspace {name}? Its projects stay registered. [y/N]"),
                    action: Confirmed::Send(
                        Box::new(Request::WorkspaceRemove { name }),
                        "workspace removed".into(),
                    ),
                });
            }
            "workspace.profile" => {
                let items = self
                    .profiles
                    .iter()
                    .map(|x| Item {
                        label: x.name.clone(),
                        detail: x.label.clone(),
                        key: None,
                        target: Target::Path(x.name.clone().into()),
                    })
                    .collect();
                self.overlay = Some(Overlay::Picker {
                    palette: Palette::new(
                        format!("Workspace {} runs as which profile?", self.workspace),
                        items,
                    ),
                    purpose: Purpose::WorkspaceProfile,
                });
            }
            "workspace.notify" => {
                let items = [
                    ("all", "notify for everything"),
                    ("badge", "only a badge in the status bar"),
                    ("mute", "nothing"),
                ]
                .iter()
                .map(|(n, d)| Item {
                    label: (*n).into(),
                    detail: (*d).into(),
                    key: None,
                    target: Target::Path((*n).into()),
                })
                .collect();
                self.overlay = Some(Overlay::Picker {
                    palette: Palette::new(
                        format!("How does {} notify you?", self.workspace),
                        items,
                    ),
                    purpose: Purpose::WorkspaceNotify,
                });
            }
            "workspace.mute" | "project.mute" => {
                let target = if action == "workspace.mute" {
                    Some(format!("workspace:{}", self.workspace))
                } else {
                    project.and_then(|p| self.project(&p).map(|p| format!("project:{}", p.slug)))
                };
                match target {
                    Some(target) => self.request(
                        Pending::Done("muted for 1h (the inbox still gets them)".into()),
                        Request::Mute { target, minutes: Some(60), unmute: false },
                    ),
                    None => self.pick_project(None),
                }
            }
            "pr.open" => {
                self.overlay = Some(Overlay::Prompt {
                    title: "Which PR? (its number, `pr 12`, or URL)".into(),
                    text: String::new(),
                    kind: PromptKind::OpenPr,
                });
            }
            other => self.toast(format!("{other}: not available yet")),
        }
    }

    /// `pr.open`: the worktree that has this PR (its session), else a new one on it.
    fn open_pr(&mut self, text: &str) {
        let t = text.trim();
        let number = t.parse::<u32>().ok().or_else(|| work_source(t).and_then(|(pr, _)| pr));
        let Some(n) = number else {
            self.toast_err("not a PR: its number, `pr 12`, or its URL");
            return;
        };
        if let Some(w) =
            self.worktrees.iter().find(|w| w.pr.as_ref().is_some_and(|p| p.number == n)).cloned()
        {
            match self
                .sessions
                .iter()
                .find(|s| s.worktree.as_deref() == Some(w.id.as_str()))
                .map(|s| s.id)
            {
                Some(id) => self.show(id, None),
                None => self.picked(Target::Worktree(w.id), Purpose::Palette),
            }
            return;
        }
        let Some(p) = self.context().0 else {
            self.pick_project(None);
            return;
        };
        let agent = ptyro_agents::default_agent(&self.config);
        let w = Wizard {
            project: Some(p.clone()),
            agent: Some(agent),
            pr: Some(n),
            name: Some(format!("pr-{n}")),
            ..Wizard::default()
        };
        self.wizard_done(w, Target::NewWorktree(p));
    }

    /// Pick one of `choices`, then `ptyro <args> <choice>` runs in a split.
    pub(crate) fn pick_cli(&mut self, title: &str, args: &[&str], choices: &[(&str, &str)]) {
        let items = choices
            .iter()
            .map(|(c, d)| Item {
                label: (*c).to_owned(),
                detail: (*d).to_owned(),
                key: None,
                target: Target::Path((*c).into()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::Cli(args.iter().map(|a| (*a).to_owned()).collect()),
        });
    }

    /// The actions that apply to a row (UX1.15), for its menu: a session's, a worktree's, a
    /// project's. Only what makes sense now (a stopped session restarts, a live one stops).
    pub fn row_actions(&self, row: &Row) -> Vec<&'static str> {
        match row {
            Row::Session(id) => {
                let Some(s) = self.session(*id) else { return Vec::new() };
                let mut v = vec!["peek"];
                if s.state == ProcessState::Dormant {
                    v.push("session.resume");
                } else if !s.state.is_live() {
                    v.push("session.restart");
                }
                if s.inner_agent.as_deref() == Some("claude") {
                    v.push("session.adopt");
                }
                v.extend(["session.retitle", "session.rename", "session.explain"]);
                v.push("scratchpad.note");
                if s.state.is_live() {
                    v.push("session.summarize");
                }
                if s.kind == SessionKind::Agent && s.state.is_live() {
                    v.extend(["session.hibernate", "session.mute"]);
                    if s.structured.is_some() {
                        v.push("view.chat");
                    }
                }
                if s.worktree.is_some() {
                    v.extend(["inspector", "worktree.open", "script.run", "script.pick"]);
                }
                v.push("pane.split_right");
                if s.state.is_live() {
                    v.extend(["session.stop", "session.kill"]);
                } else {
                    v.push("session.stop");
                }
                v
            }
            Row::Worktree(w) => {
                let root = self.worktree(w).is_some_and(|w| w.is_root);
                let mut v = vec![
                    "session.new",
                    "session.again",
                    "shell.new",
                    "shell.drawer",
                    "shell.pane",
                    "inspector",
                    "worktree.finish",
                ];
                v.extend(["worktree.open", "script.run", "script.pick", "worktree.retitle"]);
                if !root {
                    v.extend(["worktree.archive", "worktree.remove"]);
                }
                v
            }
            Row::Project(_) => vec![
                "session.new",
                "session.again",
                "worktree.new",
                "shell.new",
                "shell.drawer",
                "shell.pane",
                "project.trust",
                "script.pick",
                "worktree.open",
            ],
            // A quick terminal (UX1.25): show or hide it, and what a shell offers.
            Row::Quick(id) => {
                let Some(s) = self.session(*id) else { return Vec::new() };
                let mut v = vec!["shell.drawer", "shell.pane", "shell.new"];
                if s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked) {
                    v.push("peek");
                }
                if s.inner_agent.as_deref() == Some("claude") {
                    v.push("session.adopt");
                }
                v.extend(["session.rename", "session.explain", "session.stop"]);
                v
            }
            Row::Set(_) => vec!["sets"],
            // A workspace (MM1.B): start something in it, set it up; another machine's adds what
            // its link can do.
            Row::Workspace(w) => {
                let mut v = vec!["session.new", "session.again", "worktree.new", "project.add"];
                if let Some(m) = self.machine_of_name(w) {
                    if m.state != ptyro_core::model::MachineState::Online {
                        v.insert(0, "machine.retry");
                    }
                    v.extend(["machine.hide", "machine.remove"]);
                } else {
                    v.extend(["workspace.rename", "workspace.profile", "workspace.notify"]);
                }
                v.extend(["machine.add", "machine.list"]);
                v
            }
            Row::Group(_) => Vec::new(),
        }
    }

    /// `.` / right-click (UX1.15): the row's actions, with their keys. In the pane, the focused
    /// session's.
    fn open_row_menu(&mut self) {
        let row = match (self.focus, self.selected_row(), self.focused_session()) {
            (Focus::Sidebar, Some(r), _) => r,
            (_, _, Some(id)) if self.in_drawer() => Row::Quick(id),
            (_, _, Some(id)) => Row::Session(id),
            _ => {
                self.toast_offer(
                    "nothing here yet",
                    false,
                    run_offer("start a task", "session.new"),
                );
                return;
            }
        };
        let title = match &row {
            Row::Session(id) => self.session(*id).map(|s| format!("Session {}", s.name)),
            Row::Quick(_) => Some("Quick terminal".into()),
            Row::Worktree(w) => self.worktree(w).map(|w| format!("Worktree {}", w.qualified())),
            Row::Project(p) => self.project(p).map(|p| format!("Project {}", p.slug)),
            Row::Set(_) => Some("Change set".into()),
            Row::Workspace(w) => Some(format!("Workspace {}", crate::ui::workspace_label(w))),
            Row::Group(_) => None,
        };
        let items: Vec<Item> = self
            .row_actions(&row)
            .into_iter()
            .filter_map(|a| keymap::spec(a))
            .map(|a| Item {
                label: a.id.into(),
                detail: a.desc.into(),
                key: self.keymap.key_for(a.id),
                target: Target::Action(Command { action: a.id, arg: None }),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title.unwrap_or_else(|| "Actions".into()), items),
            purpose: Purpose::RowMenu(row),
        });
    }

    /// The task box's ⏎: the default agent on the prompt, in a new worktree named after it (or
    /// here, with no project). Nothing typed: today's fast path, a random name.
    pub(crate) fn start_task(&mut self, text: String, name: Option<String>, w: Wizard) {
        let text = text.trim().to_owned();
        let (slug, agent, _) = self.task_plan(&text, name.as_deref(), &w);
        if let (Some((pr, issue)), Some(p)) = (work_source(&text), w.project.clone()) {
            // The issue's text becomes the prompt (the daemon's); a PR's agent starts on its branch.
            let w = Wizard { agent: Some(agent), name: slug, pr, issue, ..w };
            self.wizard_done(w, Target::NewWorktree(p));
            return;
        }
        let prompt = (!text.is_empty()).then_some(text);
        let w = Wizard { agent: Some(agent), name: slug, prompt, ..w };
        match w.project.clone() {
            Some(p) => self.wizard_done(w, Target::NewWorktree(p)),
            None => self.start_in(w, None),
        }
    }

    fn task_key(
        &mut self,
        key: KeyEvent,
        mut text: String,
        mut name: Option<String>,
        mut naming: bool,
        w: Wizard,
    ) {
        let ctrl = key.mods.has(Mods::CTRL);
        match key.code {
            KeyCode::Esc => return,
            // A new line in what it should do; a name stays one word.
            _ if is_newline_key(&key) => {
                if !naming {
                    text.push('\n');
                }
            }
            KeyCode::Enter => {
                if let Some(n) = name.as_deref().filter(|n| !n.is_empty())
                    && let Some(e) = ptyro_core::names::problem(n)
                {
                    self.toast_err(format!("name: {e}"));
                } else {
                    self.start_task(text, name.filter(|n| !n.is_empty()), w);
                    return;
                }
            }
            // The full wizard: pick the agent and where, the prompt carried along.
            KeyCode::Char { c: 'o' } if ctrl => {
                let prompt = (!text.trim().is_empty()).then(|| text.trim().to_owned());
                self.open_wizard_with(Wizard { prompt, name: None, ..w });
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                naming = !naming;
                if naming && name.is_none() {
                    name = self.task_plan(&text, None, &w).0;
                }
            }
            KeyCode::Char { c: 'u' } if ctrl => {
                if naming {
                    name = Some(String::new());
                } else {
                    text.clear();
                }
            }
            KeyCode::Backspace => {
                if naming {
                    name.get_or_insert_with(String::new).pop();
                } else {
                    text.pop();
                }
            }
            KeyCode::Char { c } if !ctrl => {
                if naming {
                    let n = name.get_or_insert_with(String::new);
                    n.push(c);
                    *n = ptyro_core::names::typed(n);
                } else {
                    text.push(c);
                }
            }
            _ => {}
        }
        self.overlay = Some(Overlay::Task { text, name, naming, wizard: w });
    }

    /// The full wizard, step one: what to run (from the task box's `C-o`). The default agent
    /// is first.
    fn open_wizard_with(&mut self, w: Wizard) {
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

    /// Step two: where. A new worktree of the current project first; with no project, after
    /// "here", adding one (then a new worktree in it).
    fn open_targets(&mut self, w: Wizard) {
        let mut items = Vec::new();
        let project = w.project.as_ref().and_then(|p| self.project(p)).cloned();
        if let Some(p) = &project {
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
        // Where else the work can come from (UX1.10).
        if let Some(p) = &project {
            items.push(Item {
                label: "+ from a PR or an issue…".into(),
                detail: "paste its URL, or its number".into(),
                key: None,
                target: Target::Action(Command { action: "wizard.from", arg: None }),
            });
            items.push(Item {
                label: "+ on an existing branch…".into(),
                detail: "a branch without a worktree, local or the remote's".into(),
                key: None,
                target: Target::Action(Command { action: "wizard.branch", arg: None }),
            });
            let dirty = self
                .worktrees
                .iter()
                .find(|x| x.project == p.id && x.is_root)
                .and_then(|x| x.status.as_ref())
                .is_some_and(|s| s.dirty() > 0);
            if dirty {
                items.push(Item {
                    label: "+ with my uncommitted changes".into(),
                    detail: format!("a new worktree takes them from {}'s main checkout", p.slug),
                    key: None,
                    target: Target::Action(Command { action: "wizard.take", arg: None }),
                });
            }
        }
        let here = self.here().display().to_string();
        items.push(Item { label: "here".into(), detail: here, key: None, target: Target::Here });
        if project.is_none() {
            items.push(Item {
                label: "+ add a project…".into(),
                detail: "pick a repository, then start in a new worktree of it".into(),
                key: None,
                target: Target::AddProject,
            });
        }
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
        match target {
            Target::NewWorktree(project) => {
                let spec = worktree_spec(&w, &project);
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
                    SpawnSpec {
                        agent: Some(a.to_owned()),
                        cwd,
                        worktree,
                        prompt: w.prompt.clone(),
                        title: w.prompt.clone(),
                        name: w.name.clone().filter(|n| !n.is_empty()),
                        ..SpawnSpec::default()
                    },
                    w.split,
                );
            }
        }
    }

    // ── workspaces, editors, scripts, projects ─────────────────────────────

    fn open_workspaces(&mut self) {
        // In the sidebar's order: this machine's, then each other machine's (MM1.B).
        let order: Vec<WorkspaceInfo> = self.tree().order().into_iter().cloned().collect();
        let mut items: Vec<Item> = order
            .iter()
            .map(|w| {
                let n = self.sessions.iter().filter(|s| self.home_of(s) == w.name).count();
                let current = if w.name == self.workspace { " · current" } else { "" };
                let state = self
                    .machine_of_name(&w.name)
                    .filter(|m| m.state != ptyro_core::model::MachineState::Online)
                    .map(|m| format!(" · {}", m.state.label()))
                    .unwrap_or_default();
                Item {
                    label: crate::ui::workspace_label(&w.name),
                    detail: format!(
                        "[{}] {} project(s), {n} session(s){current}{state}",
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

    /// `+ new workspace` with other machines (MM1.B): this machine, or one of them.
    fn open_new_workspace_where(&mut self) {
        let here = self.machine.clone().unwrap_or_else(|| "this machine".into());
        let mut items = vec![Item {
            label: format!("{here} (this machine)"),
            detail: String::new(),
            key: None,
            target: Target::Machine(String::new()),
        }];
        for m in &self.machines {
            let online = m.state == ptyro_core::model::MachineState::Online;
            items.push(Item {
                label: m.name.clone(),
                detail: if online {
                    String::new()
                } else {
                    format!("{}: can't make one there now", m.state.label())
                },
                key: None,
                target: Target::Machine(m.name.clone()),
            });
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("New workspace — on which machine?", items),
            purpose: Purpose::NewWorkspaceOn,
        });
    }

    fn prompt_new_workspace(&mut self, machine: Option<String>) {
        let on = machine.as_ref().map(|m| format!(" on {m}")).unwrap_or_default();
        let suggested = machine.is_none().then(|| self.free_workspace_name());
        self.overlay = Some(Overlay::Prompt {
            title: format!("New workspace{on} — name"),
            text: String::new(),
            kind: PromptKind::NewWorkspace(suggested, machine),
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
                other => AppMsg::Reply(
                    Pending::Spawned { split: None, spec: Box::default(), back: false },
                    other,
                ),
            };
            let _ = tx.send(msg);
        });
    }

    fn open_script_picker(&mut self, wt: String, scripts: ScriptsInfo) {
        if scripts.names.is_empty() {
            let file = self
                .worktree(&wt)
                .and_then(|w| self.project(&w.project))
                .map(|p| p.root.join(".ptyro.toml"));
            match file {
                Some(f) => self.toast_offer(
                    "no scripts yet",
                    false,
                    Offer { label: "add some in .ptyro.toml".into(), then: Then::Edit(f) },
                ),
                None => self.toast("no scripts yet"),
            }
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

    /// The discovery picker; once a project is picked and added, `then` (UX1.6).
    fn pick_project(&mut self, then: Option<AfterAdd>) {
        self.request(Pending::Discovered(then), Request::Discover { refresh: false })
    }

    /// Once the project `after_add` waits for has its main checkout here, do what asked for it.
    fn try_after_add(&mut self) {
        let Some((project, _)) = &self.after_add else { return };
        if !self.worktrees.iter().any(|w| w.project == *project && w.is_root) {
            return;
        }
        let Some((project, then)) = self.after_add.take() else { return };
        self.context_hint = Some(project.clone());
        match then {
            AfterAdd::Run(action) => self.run(&Command { action, arg: None }),
            AfterAdd::Targets(w) => {
                let root = self.root_of(&project);
                let w = Wizard { project: Some(project), worktree: root, ..w };
                self.resolve_for(&w);
                self.open_targets(w);
            }
        }
        self.context_hint = None;
    }

    pub(crate) fn root_of(&self, project: &str) -> Option<String> {
        self.worktrees.iter().find(|w| w.project == project && w.is_root).map(|w| w.id.clone())
    }

    fn open_add_project(&mut self, paths: Vec<std::path::PathBuf>, then: Option<AfterAdd>) {
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
            palette: Palette::new(
                match &then {
                    Some(_) => {
                        format!("No project here yet: pick one to add to {}", self.workspace)
                    }
                    None => format!("Add a project to {}", self.workspace),
                },
                items,
            ),
            purpose: Purpose::AddProject(then),
        });
    }

    /// "Quit ptyro?", saying what stops: agents that can resume hibernate, the rest end.
    fn confirm_quit(&mut self) {
        let live: Vec<&SessionInfo> =
            self.sessions.iter().filter(|s| s.state.is_live() && !s.pad).collect();
        let (sleep, end): (Vec<&SessionInfo>, Vec<&SessionInfo>) =
            live.iter().partition(|s| s.kind == SessionKind::Agent && s.conversation.is_some());
        let names = |v: &[&SessionInfo]| {
            let mut n: Vec<&str> = v.iter().take(4).map(|s| s.name.as_str()).collect();
            if v.len() > 4 {
                n.push("…");
            }
            n.join(", ")
        };
        let mut text = String::from("Quit ptyro: stop the daemon and every session?");
        if !sleep.is_empty() {
            text.push_str(&format!(
                "\n{} agent(s) hibernate and resume next time: {}",
                sleep.len(),
                names(&sleep)
            ));
        }
        if !end.is_empty() {
            text.push_str(&format!("\n{} session(s) end for good: {}", end.len(), names(&end)));
        }
        let detach = self.keymap.key_for("app.detach").unwrap_or_else(|| "detach".into());
        text.push_str(&format!("\n[y/N] · {detach} detaches instead, keeping everything running"));
        self.overlay = Some(Overlay::Confirm { action: Confirmed::Quit, text });
    }

    fn confirm_archive(&mut self, w: &WorktreeInfo) {
        let live = self
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(w.id.as_str()) && s.state.is_live())
            .count();
        let note =
            if live > 0 { format!(" Its {live} running session(s) stop.") } else { String::new() };
        self.overlay = Some(Overlay::Confirm {
            text: format!(
                "Archive {} ({})? Its branch and a snapshot of its files are kept; restore brings it back.{note} [Y/n]",
                w.qualified(),
                w.branch.as_deref().unwrap_or("-")
            ),
            action: Confirmed::Archive(w.id.clone()),
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
        let rest: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| !s.pad && !order.iter().any(|o| o.id == s.id))
            .collect();
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
        // Worktrees and projects (UX1.18): go to them.
        let here = self.tree().project_ids();
        for w in self.worktrees.iter().filter(|w| here.contains(&w.project)) {
            items.push(Item {
                label: format!("worktree {}", w.qualified()),
                detail: [w.branch.clone(), w.title.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · "),
                key: None,
                target: Target::Worktree(w.id.clone()),
            });
        }
        for p in self.projects.iter().filter(|p| here.contains(&p.id)) {
            items.push(Item {
                label: format!("project {}", p.slug),
                detail: p.root.display().to_string(),
                key: None,
                target: Target::Project(p.id.clone()),
            });
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
        // Every setting, to find it by what it does (CFG1.1).
        for s in ptyro_core::settings::settings(ptyro_core::settings::Scope::User)
            .filter(|s| !s.key.contains('*'))
        {
            items.push(Item {
                label: format!("setting {}", s.key),
                detail: s.help.into(),
                key: None,
                target: Target::Setting(s.key.into()),
            });
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Command palette", items),
            purpose: Purpose::Palette,
        });
        // The context worktree's scripts join as soon as the daemon says what they are.
        if let (Some(project), Some(wt)) = self.context() {
            let addr = self.worktree(&wt).map_or_else(|| wt.clone(), WorktreeInfo::qualified);
            self.request(
                Pending::PaletteScripts(wt),
                Request::ProjectScripts { addr: project, worktree: Some(addr) },
            );
        }
    }

    pub(crate) fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else { return };
        match overlay {
            Overlay::Inbox { selected, tab } => self.inbox_key(key, selected, tab),
            Overlay::Peek { session, screen, reply } => self.peek_key(key, session, screen, reply),
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
            Overlay::About { doc, sel, scroll } => self.about_key(key, doc, sel, scroll),
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
                    KeyCode::Char { c: 'c' } if key.mods.has(Mods::CTRL) => return,
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
            Overlay::Settings(mut p) => {
                let acts = p.key(key);
                let closing = acts.contains(&crate::settings::Action::Close);
                if !closing {
                    self.overlay = Some(Overlay::Settings(p));
                }
                self.settings_actions(acts);
            }
            Overlay::Task { text, name, naming, wizard } => {
                self.task_key(key, text, name, naming, wizard)
            }
            Overlay::Suggest { suggestions, spec, split } => match key.code {
                KeyCode::Char { c: 'y' | 'Y' } | KeyCode::Enter => {
                    // Written (and trusted when they're its only scripts); then the usual check.
                    let addr = spec.project.clone();
                    self.request(
                        Pending::Suggested(spec, split),
                        Request::ProjectSuggest { addr, apply: Some(true) },
                    );
                }
                // Esc backs out of the whole thing; `n` is the answer "no, and don't ask again".
                KeyCode::Esc => self.toast("not created"),
                KeyCode::Char { c: 'n' | 'N' } => {
                    self.request(
                        Pending::Done(String::new()),
                        Request::ProjectSuggest { addr: spec.project.clone(), apply: Some(false) },
                    );
                    self.create_worktree(*spec, split);
                }
                _ => self.overlay = Some(Overlay::Suggest { suggestions, spec, split }),
            },
            Overlay::Picker { mut palette, purpose } => {
                // The scratchpad's list has control keys beside searching (NOTE1.3).
                if matches!(purpose, Purpose::PadItems) {
                    match self.pad_items_ctrl(&palette, key) {
                        Some(true) => {
                            self.overlay = Some(Overlay::Picker { palette, purpose });
                            return;
                        }
                        Some(false) => return,
                        None => {}
                    }
                }
                match palette.key(key) {
                    Picked::Stay => self.overlay = Some(Overlay::Picker { palette, purpose }),
                    Picked::Close => {}
                    Picked::Pick(target) => self.picked(target, purpose),
                }
            }
            Overlay::Prompt { title, mut text, kind } => match key.code {
                KeyCode::Esc => {}
                _ if kind.multiline() && is_newline_key(&key) => {
                    text.push('\n');
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                // The problem is already on screen; keep what was typed so it can be fixed.
                KeyCode::Enter if self.prompt_check(&kind, &text).is_err() => {
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                KeyCode::Enter => self.prompt_done(text, kind),
                KeyCode::Tab if kind.is_name() => {
                    let text = ptyro_core::names::tidy(&text).unwrap_or(text);
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                KeyCode::Char { c: 'u' } if key.mods.has(Mods::CTRL) => {
                    self.overlay = Some(Overlay::Prompt { title, text: String::new(), kind });
                }
                // Renaming: C-t edits the title instead (UX1.24).
                KeyCode::Char { c: 't' } if key.mods.has(Mods::CTRL) => match kind {
                    PromptKind::Rename(id) => self.open_retitle(id),
                    kind => self.overlay = Some(Overlay::Prompt { title, text, kind }),
                },
                KeyCode::Char { c } if !key.mods.has(Mods::CTRL) => {
                    text.push(c);
                    let text = kind.fit(&text);
                    self.overlay = Some(Overlay::Prompt { title, text, kind });
                }
                _ => self.overlay = Some(Overlay::Prompt { title, text, kind }),
            },
            Overlay::Confirm { action, text } => match (key.code, &action) {
                // "Keep the pin? [Y/n]": Enter keeps it.
                (KeyCode::Enter, Confirmed::KeepPin(_)) => {}
                // Archiving loses nothing: Enter agrees.
                (KeyCode::Enter, Confirmed::Archive(_)) => self.confirmed(action),
                // A removal that would lose work: `a` archives it instead (UX1.12).
                (KeyCode::Char { c: 'a' | 'A' }, Confirmed::RemoveWorktree { id, force: true }) => {
                    let id = id.clone();
                    self.confirmed(Confirmed::Archive(id));
                }
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
                // Esc and q back out; n goes on without running the scripts.
                KeyCode::Esc | KeyCode::Char { c: 'q' } => self.toast("cancelled"),
                KeyCode::Char { c: 'n' | 'N' } => {
                    if let AfterTrust::Create(mut spec, split) = then {
                        // Still make the worktree, just without running its scripts.
                        spec.setup = false;
                        self.create_worktree(*spec, split);
                    }
                    self.toast_offer(
                        format!("{}'s scripts not trusted", scripts.project),
                        false,
                        run_offer("trust them after all", "project.trust"),
                    );
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
    /// The settings panel (CFG1.1), on `key` when given (`ui.theme`).
    pub fn open_settings(&mut self, key: Option<&str>) {
        let (project, _) = self.context();
        let proj = project
            .and_then(|id| self.projects.iter().find(|p| p.id == id || p.slug == id))
            .map(|p| crate::settings::Proj { name: p.slug.clone(), root: p.root.clone() });
        let mut panel = crate::settings::Panel::new(Config::path(&self.paths), proj);
        if let Some(k) = key {
            panel.jump(k);
        }
        self.overlay = Some(Overlay::Settings(Box::new(panel)));
    }

    /// What the settings panel asked for.
    fn settings_actions(&mut self, acts: Vec<crate::settings::Action>) {
        use crate::settings::Action as A;
        for a in acts {
            match a {
                A::Close => {
                    // (A theme being tried goes back to the one in use.)
                    self.theme = Theme::resolve(&self.config, &self.theme_name.clone(), no_color());
                    self.overlay = None;
                }
                A::Reload => self.reload_config_with(false),
                A::Toast(s) => self.toast(s),
                A::ToastErr(s) => self.toast_err(s),
                A::Theme(Some(n)) => self.theme = Theme::resolve(&self.config, &n, no_color()),
                A::Theme(None) => {
                    self.theme = Theme::resolve(&self.config, &self.theme_name.clone(), no_color());
                }
                A::Run(id) => {
                    self.overlay = None;
                    self.run(&Command { action: id, arg: None });
                }
                A::EditFile(path, line) => {
                    self.overlay = None;
                    self.edit_file(path, line);
                }
            }
        }
    }

    /// Open a file in `$VISUAL` / `$EDITOR`, full sized, at a line when the editor knows how.
    fn edit_file(&mut self, path: std::path::PathBuf, line: Option<usize>) {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let mut argv: Vec<String> = editor.split_whitespace().map(str::to_owned).collect();
        let prog =
            argv.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()).unwrap_or_default();
        if let Some(l) = line
            && ["vi", "vim", "nvim", "nano", "emacs", "view", "less"].contains(&prog.as_str())
        {
            argv.push(format!("+{l}"));
        }
        argv.push(path.to_string_lossy().into_owned());
        self.spawn_tool(argv, None, true);
    }

    fn overlay_body_h(&self, chrome: u16) -> usize {
        let r = crate::ui::boxed(self.area, 0, self.area.height.saturating_sub(4));
        r.height.saturating_sub(chrome) as usize
    }

    fn confirmed(&mut self, action: Confirmed) {
        match action {
            Confirmed::Send(req, toast) => self.request(Pending::Done(toast), *req),
            Confirmed::Quit => {
                self.quitting = true;
                self.toast("stopping ptyro…");
                self.request(Pending::Quit, Request::Shutdown { force: true });
            }
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
                self.stopping.insert(id);
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
            Confirmed::Append(file, line) => self.append_line(file, line),
            Confirmed::Archive(id) => {
                self.request(
                    Pending::Done(String::new()),
                    Request::WorktreeArchive { addr: id, stop_sessions: true },
                );
            }
        }
    }

    pub(crate) fn picked(&mut self, target: Target, purpose: Purpose) {
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
            // The wizard's other sources (UX1.10).
            (Target::Action(Command { action: "wizard.from", .. }), Purpose::Target(w)) => {
                self.overlay = Some(Overlay::Prompt {
                    title: "From which PR or issue? (its URL, `pr 12` or `issue 34`)".into(),
                    text: String::new(),
                    kind: PromptKind::WorkSource(w),
                });
            }
            (Target::Action(Command { action: "wizard.branch", .. }), Purpose::Target(w)) => {
                let project = w.project.clone().unwrap_or_default();
                self.request(Pending::Branches(w), Request::Branches { project });
            }
            (Target::Action(Command { action: "wizard.take", .. }), Purpose::Target(w)) => {
                let project = w.project.clone().unwrap_or_default();
                self.wizard_done(Wizard { take: true, ..w }, Target::NewWorktree(project));
            }
            (Target::Path(b), Purpose::Branch(w)) => {
                let project = w.project.clone().unwrap_or_default();
                let branch = b.to_string_lossy().into_owned();
                let w = Wizard {
                    name: w.name.clone().or_else(|| ptyro_core::names::from_text(&branch)),
                    branch: Some(branch),
                    ..w
                };
                self.wizard_done(w, Target::NewWorktree(project));
            }
            (Target::Workspace(name), _) => self.switch_workspace(name),
            // With other machines (MM1.B): which one, first.
            (Target::NewWorkspace, _) if !self.machines.is_empty() => {
                self.open_new_workspace_where()
            }
            (Target::NewWorkspace, _) => self.prompt_new_workspace(None),
            (Target::Machine(m), Purpose::MachineUnhide) => {
                if let Some((name, ws)) = m.split_once(':') {
                    let req = Request::MachineHide {
                        name: name.into(),
                        workspace: ws.into(),
                        hidden: false,
                    };
                    self.request(Pending::Done(format!("{m} is shown again")), req);
                }
            }
            (Target::Machine(m), Purpose::MachinePick(action)) => self.machine_on(action, m),
            (Target::Machine(m), Purpose::NewWorkspaceOn) => {
                self.prompt_new_workspace((!m.is_empty()).then_some(m))
            }
            (Target::Editor(name), Purpose::Editor(wt)) => self.open_editor(&wt, &name),
            (Target::Script(name), Purpose::Script(wt)) => self.run_script(wt, Some(name)),
            (Target::Worktree(id), Purpose::Gc) => {
                if let Some(w) = self.worktree(&id).cloned() {
                    self.confirm_archive(&w);
                }
            }
            (Target::Path(word), Purpose::CliPick(mut job)) => {
                job.done.push(word.to_string_lossy().into_owned());
                self.cli_step(*job);
            }
            (Target::Path(p), Purpose::Pin(project)) => self.request(
                Pending::Done("pinned".into()),
                Request::ProjectPin {
                    addr: project,
                    profile: Some(p.to_string_lossy().into_owned()),
                },
            ),
            (Target::Path(p), Purpose::WorkspaceProfile) => self.request(
                Pending::Done("workspace profile set".into()),
                Request::WorkspaceSet {
                    name: self.workspace.clone(),
                    profile: Some(p.to_string_lossy().into_owned()),
                    notify: None,
                },
            ),
            (Target::Path(n), Purpose::WorkspaceNotify) => self.request(
                Pending::Done("workspace notifications set".into()),
                Request::WorkspaceSet {
                    name: self.workspace.clone(),
                    profile: None,
                    notify: Some(n.to_string_lossy().into_owned()),
                },
            ),
            (Target::Path(arg), Purpose::Cli(mut args)) => {
                args.push(arg.to_string_lossy().into_owned());
                self.run_cli(&args);
            }
            (Target::Path(addr), Purpose::Restore) => {
                self.request(
                    Pending::Done("restored".into()),
                    Request::WorktreeRestore { addr: addr.to_string_lossy().into_owned() },
                );
            }
            (Target::Path(path), purpose) => {
                let then = match purpose {
                    Purpose::AddProject(then) => then,
                    _ => None,
                };
                let workspace = Some(self.workspace.clone());
                self.request(Pending::ProjectAdded(then), Request::ProjectAdd { path, workspace });
            }
            (Target::AddProject, Purpose::Target(w)) => {
                self.pick_project(Some(AfterAdd::Targets(w)))
            }
            (Target::Action(Command { action, .. }), Purpose::Finish(id)) => {
                let integrate = action == "integrate";
                self.toast(if integrate {
                    "finishing: committing what's left, then integrating…"
                } else {
                    "finishing: committing what's left, then a PR…"
                });
                self.request(
                    Pending::Done(String::new()),
                    Request::WorktreeFinish { addr: id, integrate },
                );
            }
            (Target::PadItem(line), Purpose::PadItems) => self.start_pad_item(line),
            (Target::PadRev(rev), Purpose::PadHistory) => self.restore_pad(rev),
            (target, Purpose::Home) => self.home_picked(target),
            (Target::Fix(i), Purpose::Health) => self.fix(i),
            // A row's action: with that row selected, so it's what the action works on.
            (Target::Action(cmd), Purpose::RowMenu(row)) => {
                let rows = self.rows();
                // Its row went while the menu was open: running the action would hit whichever
                // row is nearest now (a kill meant for A landing on B).
                let Some(i) = rows.iter().position(|r| *r == row) else {
                    self.toast("that row is gone");
                    return;
                };
                self.set_sel(&rows, i);
                self.focus = Focus::Sidebar;
                self.run(&cmd);
            }
            (Target::Action(cmd), _) => self.run(&cmd),
            // From the palette: go to a worktree (its first session) or a project.
            (Target::Worktree(id), _) => {
                let rows = self.rows();
                if let Some(i) = rows.iter().position(|r| *r == Row::Worktree(id.clone())) {
                    self.set_sel(&rows, i);
                }
                match self.sessions.iter().find(|s| s.worktree.as_deref() == Some(id.as_str())) {
                    Some(s) => {
                        let sid = s.id;
                        self.show(sid, None);
                    }
                    None => self.focus = Focus::Sidebar,
                }
            }
            (Target::Project(id), _) => {
                self.collapsed.remove(&id);
                let rows = self.rows();
                if let Some(i) = rows.iter().position(|r| *r == Row::Project(id.clone())) {
                    self.set_sel(&rows, i);
                }
                self.focus = Focus::Sidebar;
            }
            (Target::RunScript(wt, name), _) => self.run_script(wt, Some(name)),
            (Target::Session(id), _) => self.show(id, None),
            (Target::Theme(name), _) => self.set_theme(name),
            (Target::Setting(key), _) => self.open_settings(Some(&key)),
            (Target::NewShell, _) => self.spawn(Vec::new(), None, None),
            _ => {}
        }
    }

    /// What the prompt's text would do, checked on every keystroke and shown under it: `Err`
    /// is a problem (⏎ is refused until it's fixed), `Ok(Some)` a note. The daemon checks
    /// again; this only says it sooner, mirroring its rules from what the TUI already knows.
    pub(crate) fn prompt_check(
        &self,
        kind: &PromptKind,
        text: &str,
    ) -> Result<Option<String>, String> {
        let t = text.trim();
        let name = |t: &str| ptyro_core::names::problem(t).map_or(Ok(()), Err);
        match kind {
            PromptKind::Rename(id) => {
                let current = self.session(*id).map(|s| s.name.as_str());
                if t.is_empty() || current == Some(t) {
                    return Ok(Some("unchanged".into()));
                }
                name(t)?;
                if self.sessions.iter().any(|s| s.id != *id && s.name == t) {
                    return Err(format!("a session named {t} exists"));
                }
            }
            PromptKind::WorktreeName(w) if !t.is_empty() => {
                name(t)?;
                if let Some(x) = self
                    .worktrees
                    .iter()
                    .find(|x| Some(&x.project) == w.project.as_ref() && x.name == t)
                {
                    return Err(format!("{}/{t} exists", x.project_slug));
                }
            }
            PromptKind::NewWorkspace(_, machine) if !t.is_empty() => {
                name(t)?;
                let full = machine.as_ref().map_or_else(|| t.to_owned(), |m| format!("{m}:{t}"));
                if self.workspaces.iter().any(|w| w.name == full) {
                    return Err(format!("workspace {} exists", crate::ui::workspace_label(&full)));
                }
            }
            PromptKind::RenameWorkspace(old) => {
                if old == "main" {
                    return Err("the main workspace can't be renamed".into());
                }
                if t.is_empty() || t == old {
                    return Ok(Some("unchanged".into()));
                }
                name(t)?;
                if self.workspaces.iter().any(|w| w.name == t) {
                    return Err(format!("workspace {t} exists"));
                }
            }
            PromptKind::Orch(crate::runs::OrchInput::FanoutAgents { .. }) => {
                for a in t.split([',', ' ']).map(str::trim).filter(|a| !a.is_empty()) {
                    let (agent, _) = ptyro_agents::split_agent(a);
                    if ptyro_agents::command(&self.config, agent).is_none() {
                        return Err(format!(
                            "unknown agent {agent} (known: {})",
                            ptyro_agents::names(&self.config).join(", ")
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(None)
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
                // Nothing typed: the name it offered (UX1.24).
                let typed = text.trim().to_owned();
                w.name = Some(if typed.is_empty() {
                    w.suggested.clone().unwrap_or_default()
                } else {
                    typed
                });
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
            PromptKind::Retitle(id) => {
                let title = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
                let done = if title.is_some() { "retitled" } else { "title cleared" };
                self.request(
                    Pending::Done(done.into()),
                    Request::Retitle { addr: id.full(), title, worktree: false },
                );
            }
            PromptKind::PadAdd(about) => {
                if !text.trim().is_empty() {
                    self.pad_add(text, false, about);
                }
            }
            PromptKind::CliAsk(job, vars) => self.cli_answer(*job, &text, vars),
            PromptKind::RenameBranch(id) => {
                let branch = Some(text.trim().to_owned()).filter(|b| !b.is_empty());
                self.request(
                    Pending::Done("branch renamed".into()),
                    Request::WorktreeRenameBranch { addr: id, branch },
                );
            }
            PromptKind::RenameWorkspace(old) => {
                let new_name = text.trim().to_owned();
                if !new_name.is_empty() && new_name != old {
                    // The name changes here once the daemon has accepted it: it refuses `main`
                    // and bad names, and a rename that failed must not leave this TUI on a
                    // workspace that doesn't exist.
                    self.request(
                        Pending::WorkspaceRenamed(new_name.clone()),
                        Request::WorkspaceRename { name: old, new_name },
                    );
                }
            }
            PromptKind::OpenPr => self.open_pr(&text),
            PromptKind::MachineAdd => {
                let t = text.trim();
                if !t.is_empty() {
                    self.run_cli(&["machine".into(), "add".into(), t.to_owned()]);
                }
            }
            PromptKind::WorkSource(w) => match work_source(&text) {
                Some((pr, issue)) => {
                    let project = w.project.clone().unwrap_or_default();
                    self.wizard_done(Wizard { pr, issue, ..w }, Target::NewWorktree(project));
                }
                None => self.toast_err("not a PR or issue: paste its GitHub URL, or `pr 12`"),
            },
            PromptKind::RetitleWorktree(id) => {
                let title = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
                let done = if title.is_some() { "retitled" } else { "title cleared" };
                self.request(
                    Pending::Done(done.into()),
                    Request::Retitle { addr: id, title, worktree: true },
                );
            }
            PromptKind::NewWorkspace(suggested, machine) => {
                let typed = text.trim().to_owned();
                let name = if typed.is_empty() { suggested.unwrap_or_default() } else { typed };
                // On another machine: made there (its name carries the machine, MM1.B).
                let name = match machine {
                    Some(m) if !name.is_empty() => format!("{m}:{name}"),
                    _ => name,
                };
                if !name.is_empty() {
                    let req =
                        Request::WorkspaceNew { name: name.clone(), profile: None, notify: None };
                    self.request(Pending::WorkspaceMade(name), req);
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
                let same = self.session(id).is_some_and(|s| s.name == name);
                if !name.is_empty() && !same {
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
        // A drag let go of before the scrollback arrived selects nothing: entering copy mode
        // now would leave a selection nobody is dragging.
        if matches!(then, CopyStart::Drag { .. })
            && !matches!(self.drag, Some(Drag::Select { pane: p, .. }) if p == pane)
        {
            if let Some(p) = self.panes.get_mut(&pane) {
                p.loading = false;
            }
            return;
        }
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
            CopyOutcome::Note(text) => {
                self.pad_quote(&text);
                self.exit_copy();
            }
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
        let before = self.quick_stance();
        self.mouse_event(m);
        self.tuck_away(before);
    }

    fn mouse_event(&mut self, m: &crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind as K;
        self.dirty = true;
        let (x, y) = (m.column, m.row);
        // The popup is modal: a click doesn't reach the panes behind it.
        if self.pad_popup.is_some() && self.overlay.is_none() {
            return;
        }
        if let Some(Overlay::Settings(p)) = &mut self.overlay {
            let acts = p.mouse(m.kind, x, y);
            if acts.contains(&crate::settings::Action::Close) {
                self.overlay = None;
            }
            self.settings_actions(acts);
            return;
        }
        if self.overlay.is_some() {
            if matches!(m.kind, K::Down(_)) {
                self.overlay = None;
            }
            return;
        }
        if self.full_view() {
            return;
        }
        // Dragging a divider or a selection continues wherever the mouse goes.
        match (&self.drag, m.kind) {
            (Some(Drag::Sidebar), K::Drag(_)) => {
                self.set_sidebar_width(x);
                return;
            }
            (Some(Drag::Sidebar), K::Up(_)) => {
                self.drag = None;
                return;
            }
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
        // The sidebar's edge: drag it to resize the sidebar (UX1.7).
        if let Some(side) = self.sidebar_area().filter(|_| !self.sidebar_zoom)
            && matches!(m.kind, K::Down(crossterm::event::MouseButton::Left))
            && x == side.right()
            && y < side.bottom()
        {
            self.drag = Some(Drag::Sidebar);
            return;
        }
        if let Some(side) = self.sidebar_area()
            && side.contains((x, y).into())
        {
            match m.kind {
                // Right-click: that row's actions (UX1.15).
                K::Down(crossterm::event::MouseButton::Right) => {
                    let rows = self.rows();
                    let body = crate::ui::sidebar_body(self, side);
                    // (Not the hint line along the bottom: it has no row.)
                    let row = y
                        .checked_sub(side.y + 1)
                        .filter(|l| *l < side.height.saturating_sub(2))
                        .and_then(|l| body.lines.get(l as usize + body.skip))
                        .map(|(i, _)| *i);
                    if let Some(i) = row.filter(|i| rows.get(*i).is_some_and(Row::selectable)) {
                        self.set_sel(&rows, i);
                        self.focus = Focus::Sidebar;
                        self.open_row_menu();
                    }
                }
                K::Down(_) => {
                    let rows = self.rows();
                    let body = crate::ui::sidebar_body(self, side);
                    // The title line has no row; a row may span several lines (UX1.2).
                    let row = y
                        .checked_sub(side.y + 1)
                        .filter(|l| *l < side.height.saturating_sub(2))
                        .and_then(|l| body.lines.get(l as usize + body.skip))
                        .map_or(usize::MAX, |(i, _)| *i);
                    match rows.get(row).cloned() {
                        Some(Row::Session(id)) => {
                            self.set_sel(&rows, row);
                            self.show(id, None);
                        }
                        Some(Row::Quick(id)) => {
                            self.set_sel(&rows, row);
                            self.toggle_quick(Some(id), None, None);
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
            // To the session of the pane under the pointer, not the focused one: the
            // coordinates are this pane's.
            if let Some(id) = self.panes.get(&pane).and_then(|p| p.session)
                && !self.opts.view_only
                && self.session(id).is_some_and(|s| s.state.is_live())
            {
                self.send(ClientMsg::Input { session: id, event: InputEvent::Mouse { mouse } });
            }
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
        if let Some(p) = &self.context_hint {
            return (Some(p.clone()), self.root_of(p));
        }
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
                Row::Session(id) | Row::Quick(id) => {
                    if let Some(s) = self.session(*id) {
                        return (s.project.clone(), s.worktree.clone());
                    }
                }
                // A workspace's header: its first project's main checkout.
                Row::Workspace(w) => {
                    let first = self.tree().project_ids_of(w).into_iter().next();
                    let root = first.as_deref().and_then(|p| self.root_of(p));
                    return (first, root);
                }
                Row::Group(_) | Row::Set(_) => return (None, None),
            }
        }
        if let Some(s) = self.focused_session().and_then(|id| self.session(id))
            && s.project.is_some()
        {
            return (s.project.clone(), s.worktree.clone());
        }
        let first = self
            .tree()
            .project_ids_of(&self.workspace)
            .into_iter()
            .next()
            .or_else(|| self.tree().project_ids().into_iter().next());
        let root = first
            .as_ref()
            .and_then(|p| self.worktrees.iter().find(|w| w.project == *p && w.is_root))
            .map(|w| w.id.clone());
        (first, root)
    }

    /// `w` (MM1.B: every workspace is on screen): go to a workspace's header, unfolded, in the
    /// sidebar. The panes stay as they are.
    pub fn switch_workspace(&mut self, name: String) {
        self.collapsed.remove(&tree::fold_key(&name));
        self.focus = Focus::Sidebar;
        self.sidebar_zoom = false;
        if !self.sidebar {
            self.sidebar = true;
            self.view_changed();
        }
        let rows = self.rows();
        match rows.iter().position(|r| *r == Row::Workspace(name.clone())) {
            Some(i) => self.set_sel(&rows, i),
            None => self.enter_workspace(name.clone()),
        }
        self.toast(format!("workspace {}", crate::ui::workspace_label(&name)));
    }

    /// Apply a loaded workspace view (or a fresh layout).
    pub fn apply_view(&mut self, view: Option<serde_json::Value>) {
        self.layout = Layout::default();
        self.panes = HashMap::from([(1, PaneState::default())]);
        self.focused = 1;
        self.zoom = false;
        self.drawer = None;
        self.kept = None;
        let ordered = self.ordered();
        let quick: Vec<SessionId> =
            self.sessions.iter().filter(|s| s.quick || s.pad).map(|s| s.id).collect();
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
                        .filter(|id| ordered.contains(id) || quick.contains(id));
                    (p, PaneState { session, ..PaneState::default() })
                })
                .collect();
            self.focused =
                if self.layout.contains(saved.focus) { saved.focus } else { self.layout.first() };
            self.zoom = saved.zoom;
            self.sidebar = saved.sidebar;
            self.sidebar_width = saved.sidebar_width.clamp(12, SIDEBAR_MAX);
            self.restore_drawer(saved.drawer, saved.drawer_share);
            let known: Vec<SessionId> = ordered.iter().chain(&quick).copied().collect();
            self.restore_kept(saved.kept, &known);
        }
        self.settle_empty_panes(&ordered);
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
            .filter(|s| !s.pad && (everywhere || here.contains(&s.id)))
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
        // y/a/n answer a permission prompt only: a question wants a typed reply, and "no, use
        // sqlite" must not start by answering "deny".
        let blocked = self.session(session).and_then(|s| s.status.as_ref()).is_some_and(|r| {
            r.status == AgentStatus::Blocked && r.kind.as_deref() == Some("permission")
        });
        let ctrl = key.mods.has(Mods::CTRL);
        match key.code {
            KeyCode::Esc => return,
            _ if is_newline_key(&key) => reply.push('\n'),
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
            // Note it for later (NOTE1.2): the entry's words and a link back to its session.
            KeyCode::Char { c: 'n' } => {
                if let Some(e) = self.inbox_tab(tab.as_deref()).get(selected) {
                    let about = e.session.parse::<SessionId>().ok();
                    let words = if e.body.trim().is_empty() { &e.title } else { &e.body };
                    let text = format!(
                        "{} (ptyro://session/{})",
                        words.lines().next().unwrap_or("").trim(),
                        e.session_name
                    );
                    self.open_pad_add(text, about);
                }
                return;
            }
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
        let live = self.sessions.iter().filter(|s| s.state.is_live() && !s.pad).count();
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

/// What a pane shows once `gone` leaves it (UX1.23): live sessions first (a dormant one never:
/// showing it would wake it), then the nearest place (same worktree, then project, then
/// anywhere), then the nearest in the sidebar's `order`, the one after before the one before.
/// `shown` (in other panes) are skipped.
fn neighbour(
    order: &[SessionId],
    gone: &SessionInfo,
    sessions: &[SessionInfo],
    shown: &[SessionId],
) -> Option<SessionId> {
    let at = order.iter().position(|id| *id == gone.id).unwrap_or(0);
    let same = |a: &Option<String>, b: &Option<String>| a.is_some() && a == b;
    order
        .iter()
        .enumerate()
        .filter(|(_, id)| **id != gone.id && !shown.contains(id))
        .filter_map(|(i, id)| sessions.iter().find(|s| s.id == *id).map(|s| (i, s)))
        .filter(|(_, s)| s.state != ProcessState::Dormant)
        .min_by_key(|(i, s)| {
            let place = if same(&s.worktree, &gone.worktree) {
                0
            } else if same(&s.project, &gone.project) {
                1
            } else {
                2
            };
            (!s.state.is_live(), place, i.abs_diff(at), *i < at)
        })
        .map(|(_, s)| s.id)
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

/// The new worktree the wizard asks for in `project`.
pub(crate) fn worktree_spec(w: &Wizard, project: &str) -> WorktreeSpec {
    let agent = w.agent.clone().filter(|a| !a.starts_with('@'));
    WorktreeSpec {
        project: project.to_owned(),
        name: w.name.clone().filter(|n| !n.is_empty()),
        setup: true,
        // The prompt goes to the agent; with no agent it's still the title.
        prompt: w.prompt.clone().filter(|_| agent.is_some()),
        title: w.prompt.clone(),
        agent,
        // Where the work comes from (UX1.10).
        pr: w.pr,
        issue: w.issue.clone(),
        existing: w.branch.is_some(),
        branch: w.branch.clone(),
        take_changes: w.take,
        // Started from a scratchpad item (NOTE1.3).
        pad_item: w.pad_item,
        ..WorktreeSpec::default()
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

    fn app() -> App {
        let home = std::env::temp_dir().join("ptyro-tui-prompt-test");
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let area = Rect::new(0, 0, 100, 30);
        App::new(Options::default(), connect, paths, Config::default(), false, area)
    }

    fn session(name: &str) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": SessionId::new(), "handle": name, "name": name, "kind": SessionKind::Shell,
            "argv": [], "cwd": "/", "state": ProcessState::Running,
            "size": {"cols": 80, "rows": 24}, "created_ms": 0,
        }))
        .unwrap()
    }

    fn rename(app: &mut App, s: &SessionInfo) {
        app.overlay = Some(Overlay::Prompt {
            title: "Rename".into(),
            text: s.name.clone(),
            kind: PromptKind::Rename(s.id),
        });
    }

    fn typed(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                '\x08' => KeyCode::Backspace,
                c => KeyCode::Char { c },
            };
            app.overlay_key(KeyEvent::new(code, Mods::NONE));
        }
    }

    fn prompt_text(app: &App) -> Option<&str> {
        match &app.overlay {
            Some(Overlay::Prompt { text, .. }) => Some(text),
            _ => None,
        }
    }

    fn screen(app: &App) -> String {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let buf = t.backend().buffer();
        buf.content.chunks(buf.area.width as usize).fold(String::new(), |mut s, row| {
            s.extend(row.iter().map(|c| c.symbol()));
            s.push('\n');
            s
        })
    }

    fn screen_at(app: &App, w: u16) -> String {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 30)).unwrap();
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let buf = t.backend().buffer();
        buf.content
            .chunks(buf.area.width as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_machine_comes_first_in_the_sidebar_title() {
        let mut app = app();
        app.machine = Some("studio".into());
        let wide = screen_at(&app, 100);
        let first = wide.lines().next().unwrap();
        // Which machine this is, always (MM1.B: others' workspaces are listed too), then where
        // you are.
        assert!(first.contains("● studio · main"), "{first}");
        // Narrow: the machine stays, nothing wraps or breaks.
        let narrow = screen_at(&app, 22);
        assert!(narrow.lines().next().unwrap().contains("studio"), "{narrow}");
        assert_eq!(narrow.lines().count(), 30);
        // Unknown (an older daemon): nothing extra.
        app.machine = None;
        assert!(!screen_at(&app, 100).contains("studio"));
        assert_eq!(crate::window_title("studio"), "ptyro \u{b7} studio");
        assert_eq!(crate::window_title("a\u{1b}b"), "ptyro \u{b7} ab");
    }

    #[test]
    fn the_settings_panel_opens_from_its_key_and_draws() {
        let mut app = app();
        // Its key after the prefix, and the palette's action, open the same panel.
        assert_eq!(app.keymap.key_for("config.panel").as_deref(), Some("C-q C"));
        app.run(&Command { action: "config.panel", arg: None });
        assert!(matches!(app.overlay, Some(Overlay::Settings(_))));
        let shown = screen(&app);
        assert!(
            shown.contains("Settings")
                && shown.contains("Interface")
                && shown.contains("Key bindings"),
            "{shown}"
        );
        // Down to the settings, and a key on one of them.
        for k in [KeyCode::Enter, KeyCode::Down] {
            app.overlay_key(KeyEvent::new(k, Mods::NONE));
        }
        let shown = screen(&app);
        assert!(
            shown.contains("prefix") && shown.contains("sidebar_width")
                || shown.contains("sidebar width")
                || shown.contains("sidebar"),
            "{shown}"
        );
        // Esc twice leaves it.
        for _ in 0..2 {
            app.overlay_key(KeyEvent::new(KeyCode::Esc, Mods::NONE));
        }
        assert!(app.overlay.is_none());
    }

    #[test]
    fn every_setting_is_in_the_palette_and_opens_the_panel_on_it() {
        let mut app = app();
        app.run(&Command { action: "palette", arg: None });
        for c in "setting sidebar_width".chars() {
            app.overlay_key(KeyEvent::char(c));
        }
        app.overlay_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        let Some(Overlay::Settings(p)) = &app.overlay else { panic!("the panel") };
        let _ = p;
        let shown = screen(&app);
        assert!(
            shown.contains("sidebar_width") && shown.contains("How wide the sidebar is"),
            "{shown}"
        );
    }

    fn agent(name: &str, kind: &str) -> SessionInfo {
        let mut s = session(name);
        s.kind = SessionKind::Agent;
        status(&mut s, AgentStatus::Blocked, Some(kind), Some("it wants you"));
        s
    }

    #[test]
    fn a_question_is_typed_into_and_only_a_permission_prompt_answers_on_a_key() {
        let mut app = app();
        let q = agent("asker", "question");
        let id = q.id;
        app.sessions = vec![q];
        app.overlay =
            Some(Overlay::Peek { session: id, screen: String::new(), reply: String::new() });
        typed(&mut app, "no, use sqlite");
        let Some(Overlay::Peek { reply, .. }) = &app.overlay else { panic!("still open") };
        assert_eq!(reply, "no, use sqlite", "the first n is part of the reply");
        assert!(app.rx.try_recv().is_err(), "nothing was answered");
        // A permission prompt still answers on a bare key.
        app.sessions = vec![agent("asker", "permission")];
        let id = app.sessions[0].id;
        app.overlay =
            Some(Overlay::Peek { session: id, screen: String::new(), reply: String::new() });
        app.overlay_key(KeyEvent::char('n'));
        assert!(app.overlay.is_none());
        let Ok(AppMsg::Reply(Pending::Done(what), _)) = app.rx.try_recv() else {
            panic!("an answer was asked for")
        };
        assert_eq!(what, "answered deny");
    }

    #[test]
    fn a_workspace_rename_the_daemon_refuses_does_not_move_the_tui() {
        let mut app = app();
        app.workspace = "main".into();
        // `main` can't be renamed: the prompt itself says so.
        let ws = PromptKind::RenameWorkspace("main".into());
        assert!(app.prompt_check(&ws, "work").unwrap_err().contains("can't be renamed"));
        // A refusal that comes back leaves the name alone; an acceptance moves it.
        app.workspace = "side".into();
        app.on_reply(AppMsg::Reply(
            Pending::WorkspaceRenamed("work".into()),
            Err(ptyro_core::Error::new(ErrorCode::Invalid, "no")),
        ));
        assert_eq!(app.workspace, "side");
        app.on_reply(AppMsg::Reply(Pending::WorkspaceRenamed("work".into()), Ok(Reply::Done)));
        assert_eq!(app.workspace, "work");
        // A new workspace is switched to only once it exists.
        app.on_reply(AppMsg::Reply(
            Pending::WorkspaceMade("fresh".into()),
            Err(ptyro_core::Error::new(ErrorCode::Invalid, "no")),
        ));
        assert_eq!(app.workspace, "work");
        app.on_reply(AppMsg::Reply(Pending::WorkspaceMade("fresh".into()), Ok(Reply::Done)));
        assert_eq!(app.workspace, "fresh");
    }

    #[test]
    fn a_late_reply_does_not_replace_the_box_being_typed_in() {
        let mut app = app();
        let s = session("a");
        let id = s.id;
        app.sessions = vec![s];
        app.overlay = Some(Overlay::Prompt {
            title: "Rename".into(),
            text: "half-typed".into(),
            kind: PromptKind::Rename(id),
        });
        app.on_reply(AppMsg::Reply(Pending::Explain(id), Ok(Reply::Explain { lines: Vec::new() })));
        let Some(Overlay::Prompt { text, .. }) = &app.overlay else {
            panic!("the box is still there")
        };
        assert_eq!(text, "half-typed");
        assert!(app.toast.as_ref().is_some_and(|t| t.text.contains("while you were typing")));
        // With nothing being typed, the reply opens its dialog as before.
        app.overlay = None;
        app.on_reply(AppMsg::Reply(Pending::Explain(id), Ok(Reply::Explain { lines: Vec::new() })));
        assert!(matches!(app.overlay, Some(Overlay::Explain { .. })));
    }

    #[test]
    fn a_row_menu_for_a_row_that_went_does_nothing() {
        let mut app = app();
        let s = session("gone");
        let id = s.id;
        // (The row isn't in the tree: it was removed while the menu was open.)
        app.picked(
            Target::Action(Command { action: "session.kill", arg: None }),
            Purpose::RowMenu(Row::Session(id)),
        );
        assert!(app.overlay.is_none(), "no kill dialog for whatever row is nearest");
        assert!(app.toast.as_ref().is_some_and(|t| t.text.contains("gone")));
    }

    #[test]
    fn the_wheel_does_not_reach_panes_hidden_under_a_full_screen_view() {
        let mut app = app();
        let s = session("shell");
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.sessions = vec![s];
        app.inspector = Some(crate::inspector::Inspector::new("w".into()));
        app.on_mouse(&mouse(crossterm::event::MouseEventKind::ScrollUp, 60, 10));
        let p = &app.panes[&1];
        assert!(p.copy.is_none() && !p.loading, "no copy mode started behind the inspector");
    }

    #[test]
    fn a_list_reply_drops_the_sessions_it_no_longer_names_and_keeps_newer_ones() {
        let mut app = app();
        let (a, b) = (session("a"), session("b"));
        let (aid, bid) = (a.id, b.id);
        app.sessions = vec![a.clone(), b];
        app.last_listed = [aid, bid].into_iter().collect();
        app.panes.get_mut(&1).unwrap().session = Some(bid);
        app.marked = vec![bid];
        // A session an event made after the reply was asked for isn't in it yet.
        let c = session("c");
        let cid = c.id;
        app.sessions.push(c);
        app.got_list(vec![a]);
        let names: Vec<&str> = app.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "c"], "b's removal event was lost; c is newer than the reply");
        let shown = app.panes[&1].session;
        assert!(shown == Some(aid) || shown == Some(cid), "the pane moved on from b: {shown:?}");
        assert!(app.marked.is_empty());
        assert!(!app.last_listed.contains(&cid), "c shows in the next reply");
    }

    #[test]
    fn a_layout_changed_while_disconnected_stays_unsaved() {
        let mut app = app();
        app.view_dirty_since = Some(Instant::now());
        app.client = None;
        app.save_view();
        assert!(app.view_dirty_since.is_some(), "kept to be saved when the daemon is back");
    }

    #[test]
    fn help_can_be_searched_for_words_starting_with_q_and_shows_custom_focus_keys() {
        let mut app = app();
        app.overlay = Some(Overlay::Help { filter: String::new(), scroll: 0 });
        typed(&mut app, "quit");
        let Some(Overlay::Help { filter, .. }) = &app.overlay else { panic!("still open") };
        assert_eq!(filter, "quit");
        // A key of your own bound to `session.focus 2` is its own line, not folded into 1…9.
        let mut config = Config::default();
        config.keys.insert("y".into(), "session.focus 2".into());
        let (map, errors) = Keymap::new(&config);
        assert!(errors.is_empty(), "{errors:?}");
        let b = map.bindings();
        assert!(b.iter().any(|(_, k, c)| k == "y" && c.arg.as_deref() == Some("2")), "{b:?}");
        assert!(b.iter().any(|(_, k, _)| k == "1…9"), "the default digits still fold");
    }

    #[test]
    fn the_key_shown_for_an_action_is_the_plain_one() {
        let (map, _) = Keymap::new(&Config::default());
        assert_eq!(map.key_for("session.next").as_deref(), Some("C-q j"));
        assert_eq!(map.key_for("palette").as_deref(), Some("C-q :"));
        assert_eq!(map.key_in(Table::Nav, "nav.down").as_deref(), Some("j"));
    }

    #[test]
    fn a_new_pane_at_the_limit_leaves_the_one_that_is_there_alone() {
        let mut app = app();
        let (a, b) = (session("a"), session("b"));
        app.sessions = vec![a.clone(), b.clone()];
        app.panes.get_mut(&1).unwrap().session = Some(a.id);
        for _ in 0..crate::layout::MAX_PANES {
            app.show(b.id, Some(Dir::Right));
        }
        let shown_a = app.panes.values().filter(|p| p.session == Some(a.id)).count();
        assert_eq!(shown_a, 1, "a is still shown after the limit was hit");
        assert!(app.layout.panes().len() <= crate::layout::MAX_PANES);
    }

    fn placed(name: &str, project: Option<&str>, worktree: Option<&str>) -> SessionInfo {
        let mut s = session(name);
        s.project = project.map(Into::into);
        s.worktree = worktree.map(Into::into);
        s
    }

    fn shows(app: &App, pane: PaneId) -> Option<String> {
        let id = app.panes.get(&pane)?.session?;
        app.session(id).map(|s| s.name.clone())
    }

    #[test]
    fn the_neighbour_is_live_nearby_and_after_first() {
        let (a, b, c) = (session("a"), session("b"), session("c"));
        let all = vec![a.clone(), b.clone(), c.clone()];
        let order: Vec<SessionId> = all.iter().map(|s| s.id).collect();
        // The one after, else the one before.
        assert_eq!(neighbour(&order, &b, &all, &[]), Some(c.id));
        assert_eq!(neighbour(&order, &c, &all, &[]), Some(b.id));
        // Shown in another pane: skipped.
        assert_eq!(neighbour(&order, &b, &all, &[c.id]), Some(a.id));
        assert_eq!(neighbour(&order, &b, &all, &[a.id, c.id]), None);

        // Live beats exited, and a dormant agent is never picked (showing it would wake it).
        let mut exited = session("exited");
        exited.state = ProcessState::Exited { code: Some(0), signal: None };
        let mut dormant = session("dormant");
        dormant.state = ProcessState::Dormant;
        let gone = session("gone");
        let all = vec![gone.clone(), dormant.clone(), exited.clone(), a.clone()];
        let order: Vec<SessionId> = all.iter().map(|s| s.id).collect();
        assert_eq!(neighbour(&order, &gone, &all, &[]), Some(a.id));
        assert_eq!(neighbour(&order, &gone, &all, &[a.id]), Some(exited.id));
        assert_eq!(neighbour(&order, &gone, &all, &[a.id, exited.id]), None);

        // Same worktree beats same project beats elsewhere, however far down.
        let gone = placed("gone", Some("p"), Some("w1"));
        let near = placed("near", None, None);
        let project = placed("project", Some("p"), Some("w2"));
        let worktree = placed("worktree", Some("p"), Some("w1"));
        let all = vec![gone.clone(), near.clone(), project.clone(), worktree.clone()];
        let order: Vec<SessionId> = all.iter().map(|s| s.id).collect();
        assert_eq!(neighbour(&order, &gone, &all, &[]), Some(worktree.id));
        assert_eq!(neighbour(&order, &gone, &all, &[worktree.id]), Some(project.id));
    }

    #[test]
    fn a_removed_session_s_pane_shows_the_next_one() {
        let mut app = app();
        let (a, b, c) = (session("a"), session("b"), session("c"));
        app.sessions = vec![a.clone(), b.clone(), c.clone()];
        app.panes.get_mut(&1).unwrap().session = Some(b.id);
        app.on_event(DomainEvent::SessionRemoved { id: b.id });
        assert_eq!(shows(&app, 1).as_deref(), Some("c"), "the one after it");
        app.on_event(DomainEvent::SessionRemoved { id: c.id });
        assert_eq!(shows(&app, 1).as_deref(), Some("a"), "the last one: the one before");
        app.on_event(DomainEvent::SessionRemoved { id: a.id });
        assert_eq!(shows(&app, 1), None);
        let shown = screen(&app);
        assert!(shown.contains("Nothing running in main yet"), "{shown}");
        assert!(shown.contains("start a task: say what to do"), "{shown}");
    }

    #[test]
    fn in_splits_a_removed_session_s_pane_closes_never_taking_another() {
        let mut app = app();
        let (a, b, c) = (session("a"), session("b"), session("c"));
        app.sessions = vec![a.clone(), b.clone(), c.clone()];
        let right = app.layout.split(1, Dir::Right).unwrap();
        app.panes.insert(right, PaneState::default());
        app.panes.get_mut(&1).unwrap().session = Some(a.id);
        app.panes.get_mut(&right).unwrap().session = Some(b.id);
        app.focused = right;
        // `c` isn't shown, but only a split command fills a split (UX3.1): the split closes.
        app.on_event(DomainEvent::SessionRemoved { id: b.id });
        assert!(!app.layout.contains(right));
        assert_eq!(app.focused, 1);
        assert_eq!(shows(&app, 1).as_deref(), Some("a"));
        // The one pane left moves on as before.
        app.on_event(DomainEvent::SessionRemoved { id: a.id });
        assert_eq!(shows(&app, 1).as_deref(), Some("c"));
    }

    #[test]
    fn the_tour_moves_on_as_each_step_is_done_and_says_the_keys() {
        use crate::tour::Step;
        let mut app = app();
        with_project(&mut app);
        let w = feature(&mut app, false);
        app.tour = Some(crate::tour::Tour::default());
        let step = |app: &mut App| {
            app.tick();
            app.tour.as_ref().unwrap().step
        };
        let coach = |app: &App| app.tour.as_ref().unwrap().coach(app).1.join(" ");
        assert_eq!(step(&mut app), Step::Task);
        assert!(coach(&app).contains("Press C-q n"), "{}", coach(&app));
        // A shell is no task; the agent the task box started, in its own worktree, is.
        let mut agent = session("fix-typo");
        agent.kind = SessionKind::Agent;
        agent.task = Some("fix the typo".into());
        agent.worktree = Some(w.id.clone());
        status(&mut agent, AgentStatus::Working, None, None);
        app.sessions = vec![session("zsh"), agent];
        assert_eq!(step(&mut app), Step::Answer);
        status(&mut app.sessions[1], AgentStatus::Blocked, Some("permission"), Some("Bash: sed"));
        assert_eq!(step(&mut app), Step::Answer);
        assert!(coach(&app).contains("Press C-q p to peek, then y"), "{}", coach(&app));
        // Answered: it works again.
        status(&mut app.sessions[1], AgentStatus::Working, None, None);
        assert_eq!(step(&mut app), Step::Review);
        app.inspector = Some(crate::inspector::Inspector::new(w.id.clone()));
        assert_eq!(step(&mut app), Step::Finish);
        assert!(coach(&app).contains("press F on it; pick integrate"), "{}", coach(&app));
        // Integrated, clean, at that commit: shipped.
        for x in app.worktrees.iter_mut().filter(|x| x.id == w.id) {
            x.integrated = Some("abc1234ffff".into());
            x.status = Some(
                serde_json::from_value(serde_json::json!({
                    "head": "abc1234", "ahead": 0, "behind": 0, "base_ahead": 0, "base_behind": 0,
                    "staged": 0, "modified": 0, "untracked": 0, "conflicts": 0, "insertions": 0,
                    "deletions": 0, "at_ms": 0
                }))
                .unwrap(),
            );
        }
        assert_eq!(step(&mut app), Step::CleanUp);
        app.worktrees.retain(|x| x.id != w.id);
        assert_eq!(step(&mut app), Step::Done);
        assert!(coach(&app).contains("C-q d leaves the tour"), "{}", coach(&app));
        // `help.tour` inside the tour starts it over (outside, the TUI steps aside for it).
        app.run(&Command { action: "help.tour", arg: None });
        assert_eq!(app.tour.as_ref().unwrap().step, Step::Task);
        let mut plain = self::app();
        plain.run(&Command { action: "help.tour", arg: None });
        assert!(matches!(plain.quit, Some(AttachEnd::Tour)));
    }

    #[test]
    fn text_boxes_take_several_lines_and_pastes_stay_in_them() {
        let mut app = app();
        let task = |app: &App| match &app.overlay {
            Some(Overlay::Task { text, name, .. }) => (text.clone(), name.clone()),
            _ => panic!("the task box closed"),
        };
        app.overlay = Some(Overlay::Task {
            text: String::new(),
            name: None,
            naming: false,
            wizard: Wizard::default(),
        });
        // Shift+Enter, Alt+Enter and Ctrl+J each start a new line; Enter alone would send.
        for (code, mods) in [
            (KeyCode::Enter, Mods(Mods::SHIFT)),
            (KeyCode::Enter, Mods(Mods::ALT)),
            (KeyCode::Char { c: 'j' }, Mods(Mods::CTRL)),
        ] {
            key(&mut app, KeyCode::Char { c: 'a' }, Mods::NONE);
            key(&mut app, code, mods);
        }
        assert_eq!(task(&app).0, "a\na\na\n");
        // A paste keeps its new lines (and doesn't reach the session behind the box).
        app.on_paste("one\r\ntwo".into());
        assert_eq!(task(&app).0, "a\na\na\none\ntwo");
        // The name stays one word.
        key(&mut app, KeyCode::Tab, Mods::NONE);
        key(&mut app, KeyCode::Char { c: 'j' }, Mods(Mods::CTRL));
        app.on_paste("x\ny".into());
        assert_eq!(task(&app).1.as_deref().map(|n| n.ends_with("x_y")), Some(true));

        // Peek's reply, and a broadcast prompt: several lines. A rename: one.
        let id = SessionId::new();
        app.overlay =
            Some(Overlay::Peek { session: id, screen: String::new(), reply: "hi".into() });
        key(&mut app, KeyCode::Enter, Mods(Mods::SHIFT));
        app.on_paste("there".into());
        assert!(matches!(&app.overlay, Some(Overlay::Peek { reply, .. }) if reply == "hi\nthere"));
        for (kind, want) in
            [(PromptKind::Broadcast(vec![id]), "a\n"), (PromptKind::Rename(id), "a")]
        {
            app.overlay = Some(Overlay::Prompt { title: String::new(), text: "a".into(), kind });
            key(&mut app, KeyCode::Char { c: 'j' }, Mods(Mods::CTRL));
            assert!(
                matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == want),
                "{want:?}"
            );
        }
        app.on_paste("b\nc".into());
        // A name can't hold a space: it becomes _ (the rename prompt is a name).
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "ab_c"));
    }

    #[test]
    fn a_name_prompt_swaps_what_it_cant_hold_for_an_underscore() {
        let mut app = app();
        let id = SessionId::new();
        app.overlay = Some(Overlay::Prompt {
            title: String::new(),
            text: String::new(),
            kind: PromptKind::Rename(id),
        });
        for c in "My Feat/2".chars() {
            key(&mut app, KeyCode::Char { c }, Mods::NONE);
        }
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "my_feat_2"));
        app.on_paste(" a b".into());
        assert!(
            matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "my_feat_2_a_b")
        );
        // Free text isn't touched.
        app.overlay = Some(Overlay::Prompt {
            title: String::new(),
            text: String::new(),
            kind: PromptKind::Retitle(id),
        });
        key(&mut app, KeyCode::Char { c: ' ' }, Mods::NONE);
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == " "));
    }

    #[test]
    fn quitting_asks_says_what_stops_and_ends_without_reconnecting() {
        let mut app = app();
        let mut agent = session("claude-rook");
        agent.kind = SessionKind::Agent;
        agent.conversation = Some("c1".into());
        app.sessions = vec![agent, session("zsh")];
        app.run(&Command { action: "app.quit", arg: None });
        let Some(Overlay::Confirm { action: Confirmed::Quit, text }) = &app.overlay else {
            panic!("{:?}", app.overlay.is_some())
        };
        assert!(text.contains("1 agent(s) hibernate and resume next time: claude-rook"), "{text}");
        assert!(text.contains("1 session(s) end for good: zsh"), "{text}");
        assert!(text.contains("C-q d detaches instead"), "{text}");
        // Enter is no.
        key(&mut app, KeyCode::Enter, Mods::NONE);
        assert!(app.overlay.is_none() && !app.quitting);

        app.run(&Command { action: "app.quit", arg: None });
        key(&mut app, KeyCode::Char { c: 'y' }, Mods::NONE);
        assert!(app.quitting);
        // The daemon closing the connection ends it: no reconnecting (that would start one).
        app.lost("connection closed");
        assert!(matches!(app.quit, Some(AttachEnd::Stopped)));
        assert!(app.lost_since.is_none(), "not waiting to reconnect");
    }

    #[test]
    fn a_session_stopped_here_moves_its_pane_on_unless_on_stop_is_stay() {
        let mut app = app();
        let (a, b) = (session("a"), session("b"));
        app.sessions = vec![a.clone(), b.clone()];
        app.panes.get_mut(&1).unwrap().session = Some(a.id);
        let exited = |s: &SessionInfo| {
            let mut s = s.clone();
            s.state = ProcessState::Exited { code: Some(0), signal: None };
            DomainEvent::SessionExited { info: s }
        };
        // Exiting on its own (not stopped here): the pane keeps showing it, with ⏎ restart.
        app.on_event(exited(&b));
        assert_eq!(shows(&app, 1).as_deref(), Some("a"));
        app.confirmed(Confirmed::Stop(a.id));
        app.on_event(exited(&a));
        assert_eq!(shows(&app, 1).as_deref(), Some("b"), "an exited one beats nothing");
        assert_eq!(app.panes[&1].last, Some(a.id), "` brings the stopped one back");
        assert!(app.session(a.id).is_some(), "it stays in the sidebar");

        let mut app = self::app();
        app.config.ui.on_stop = ptyro_core::config::OnStop::Stay;
        app.sessions = vec![a.clone(), b.clone()];
        app.panes.get_mut(&1).unwrap().session = Some(a.id);
        app.confirmed(Confirmed::Stop(a.id));
        app.on_event(exited(&a));
        assert_eq!(shows(&app, 1).as_deref(), Some("a"));
    }

    fn status(s: &mut SessionInfo, st: AgentStatus, kind: Option<&str>, detail: Option<&str>) {
        use ptyro_core::status::{Confidence, SignalSource, StatusRecord};
        let mut r = StatusRecord::new(st, Confidence::High, SignalSource::Hook);
        r.kind = kind.map(Into::into);
        r.detail = detail.map(Into::into);
        r.since_ms = ptyro_core::now_ms() - 125_000;
        s.status = Some(r);
    }

    /// The sidebar's columns of the screen, one string per line.
    fn sidebar(app: &App) -> Vec<String> {
        screen(app).lines().map(|l| l.chars().take(30).collect::<String>()).collect()
    }

    fn rows_say(app: &App, want: &str) -> bool {
        sidebar(app).iter().any(|l| l.contains(want))
    }

    #[test]
    fn session_rows_say_what_each_one_is_doing() {
        let mut app = app();
        let mut working = session("fixer");
        working.kind = SessionKind::Agent;
        working.agent = Some("claude".into());
        working.foreground = Some("claude".into());
        working.task = Some("fix the flaky test in tests/api.rs".into());
        working.activity = Some("Bash: cargo test".into());
        status(&mut working, AgentStatus::Working, None, None);
        let mut blocked = session("asker");
        blocked.kind = SessionKind::Agent;
        status(&mut blocked, AgentStatus::Blocked, Some("permission"), Some("Bash: npm i"));
        let mut idle = session("done-one");
        idle.kind = SessionKind::Agent;
        idle.task = Some("add --json".into());
        idle.last_reply = Some("All tests pass.".into());
        status(&mut idle, AgentStatus::Idle, None, None);
        let mut shell = session("sh");
        shell.command = Some(ptyro_core::session::ShellCommand {
            text: Some("cargo test".into()),
            started_ms: ptyro_core::now_ms() - 50_000,
            ended_ms: Some(ptyro_core::now_ms() - 9_000),
            exit: Some(101),
        });
        app.sessions = vec![working, blocked, idle, shell];
        let lines = sidebar(&app);
        let has = |w: &str| lines.iter().any(|l| l.contains(w));
        assert!(has("fixer claude 2m"), "line 1: the program, and how long: {lines:#?}");
        assert!(has("now Bash: cargo test"), "{lines:#?}");
        assert!(has("task fix the flaky"), "{lines:#?}");
        assert!(has("wants Bash: npm i"), "{lines:#?}");
        assert!(has("y/a/n answers"), "{lines:#?}");
        assert!(has("last All tests pass."), "{lines:#?}");
        assert!(has("last cargo test ✗101"), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("2.1.")), "never a version");

        // Compact: one line each.
        app.config.ui.sidebar_detail = ptyro_core::config::SidebarDetail::Compact;
        assert!(!rows_say(&app, "task fix") && !rows_say(&app, "wants"));
        assert!(rows_say(&app, "fixer"));
    }

    #[test]
    fn a_summary_stands_in_for_its_turns_last_reply() {
        let mut app = app();
        let mut idle = session("done-one");
        idle.kind = SessionKind::Agent;
        idle.turns = 2;
        idle.last_reply = Some("I changed 14 files and ran the suite twice.".into());
        idle.summary = Some(ptyro_core::session::Summary {
            text: "Fixed the login test; waiting for review".into(),
            turn: 2,
            at_ms: ptyro_core::now_ms(),
        });
        status(&mut idle, AgentStatus::Idle, None, None);
        app.sessions = vec![idle];
        assert!(rows_say(&app, "≈ Fixed the login test"), "{:#?}", sidebar(&app));
        assert!(!rows_say(&app, "last I changed"), "the summary says it: {:#?}", sidebar(&app));
        // From an earlier turn: both.
        app.sessions[0].turns = 3;
        assert!(rows_say(&app, "≈ Fixed") && rows_say(&app, "last I changed"));
        // Working: what it does now instead.
        status(&mut app.sessions[0], AgentStatus::Working, None, None);
        assert!(!rows_say(&app, "≈ Fixed"));
        assert!(app.row_actions(&Row::Session(app.sessions[0].id)).contains(&"session.summarize"));
    }

    #[test]
    fn an_agent_started_in_a_shell_reads_shell_then_agent_and_offers_adopting() {
        let mut app = app();
        let mut sh = session("sh");
        sh.argv = vec!["/bin/zsh".into(), "-l".into()];
        sh.foreground = Some("claude".into());
        sh.inner_agent = Some("claude".into());
        status(&mut sh, AgentStatus::Idle, None, None);
        sh.status.as_mut().unwrap().confidence = ptyro_core::status::Confidence::Low;
        app.sessions = vec![sh];
        let lines = sidebar(&app);
        assert!(lines.iter().any(|l| l.contains("◌ sh zsh › claude")), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("A adopts it")), "{lines:#?}");
    }

    #[test]
    fn when_rows_dont_fit_only_the_ones_in_focus_or_wanting_you_open_up() {
        let mut app = app();
        let mut many: Vec<SessionInfo> = (0..14)
            .map(|n| {
                let mut s = session(&format!("s{n:02}"));
                s.task = Some(format!("task number {n}"));
                s
            })
            .collect();
        status(&mut many[7], AgentStatus::Blocked, Some("question"), Some("which db?"));
        app.panes.get_mut(&1).unwrap().session = Some(many[3].id);
        app.sessions = many;
        // 14 rows and 16 detail lines don't fit in 27: the focused and blocked rows open.
        assert!(rows_say(&app, "task number 3"), "the focused one");
        assert!(rows_say(&app, "wants which db?"), "the blocked one");
        assert!(!rows_say(&app, "task number 5"), "the rest stay one line");
        app.config.ui.sidebar_detail = ptyro_core::config::SidebarDetail::Full;
        assert!(rows_say(&app, "task number 0"));
    }

    #[test]
    fn a_click_on_a_detail_line_picks_its_session() {
        let mut app = app();
        let mut a = session("alpha");
        a.task = Some("first task".into());
        let b = session("beta");
        app.sessions = vec![a.clone(), b.clone()];
        let lines = sidebar(&app);
        let y = lines.iter().position(|l| l.contains("first task")).unwrap() as u16;
        let beta = lines.iter().position(|l| l.contains("beta")).unwrap() as u16;
        let click = |row: u16| crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.on_mouse(&click(beta));
        assert_eq!(app.focused_session(), Some(b.id));
        app.on_mouse(&click(y));
        assert_eq!(app.focused_session(), Some(a.id), "its detail line is part of its row");
    }

    #[test]
    fn everything_at_once_opens_every_row_without_resizing_a_session() {
        let mut app = app();
        let sessions: Vec<SessionInfo> = (0..14)
            .map(|n| {
                let mut s = session(&format!("s{n:02}"));
                s.task = Some(format!("the task of session number {n}, which is long"));
                s
            })
            .collect();
        app.panes.get_mut(&1).unwrap().session = Some(sessions[0].id);
        app.sessions = sessions.clone();
        let main = app.main_area();
        app.run(&Command { action: "sidebar.zoom", arg: None });
        assert_eq!(app.main_area(), main, "the panes keep their size");
        let shown = screen(&app);
        assert!(shown.contains("the task of session number 9, which is long"), "{shown}");
        assert!(shown.contains("everything at once"), "{shown}");
        // ⏎ on a row opens it and goes back.
        app.nav_move(4);
        app.run(&Command { action: "nav.open", arg: None });
        assert!(!app.sidebar_zoom);
        assert_eq!(app.focus, Focus::Pane);
        assert_ne!(app.focused_session(), Some(sessions[0].id));
    }

    fn mouse(
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
    ) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_sidebar_resizes_by_its_edge_and_by_keys_within_bounds() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let mut app = app();
        app.sessions = vec![session("a")];
        assert_eq!(app.sidebar_width, 30);
        // Grab the edge (the divider column), drag it right, let go.
        app.on_mouse(&mouse(K::Down(MouseButton::Left), 30, 5));
        app.on_mouse(&mouse(K::Drag(MouseButton::Left), 42, 5));
        app.on_mouse(&mouse(K::Up(MouseButton::Left), 42, 5));
        assert_eq!(app.sidebar_width, 42);
        assert_eq!(app.main_area().x, 43, "the panes start after it");
        // Too far either way: 12 at least, and the panes keep 21 columns (100 wide: 78).
        app.on_mouse(&mouse(K::Down(MouseButton::Left), 42, 5));
        app.on_mouse(&mouse(K::Drag(MouseButton::Left), 2, 5));
        assert_eq!(app.sidebar_width, 12);
        app.on_mouse(&mouse(K::Drag(MouseButton::Left), 99, 5));
        assert_eq!(app.sidebar_width, 78);
        app.on_mouse(&mouse(K::Up(MouseButton::Left), 99, 5));
        // A click elsewhere doesn't grab it.
        app.on_mouse(&mouse(K::Down(MouseButton::Left), 50, 5));
        app.on_mouse(&mouse(K::Drag(MouseButton::Left), 20, 5));
        assert_eq!(app.sidebar_width, 78);
        app.on_mouse(&mouse(K::Up(MouseButton::Left), 20, 5));
        // In the sidebar, < and > move it a column at a time, within the same bounds.
        app.focus = Focus::Sidebar;
        let key = |c| KeyEvent::new(KeyCode::Char { c }, Mods::NONE);
        let narrower = app.keymap.lookup(Table::Nav, key('<')).cloned().unwrap();
        let wider = app.keymap.lookup(Table::Nav, key('>')).cloned().unwrap();
        app.run(&narrower);
        assert_eq!(app.sidebar_width, 77);
        app.run(&wider);
        app.run(&wider);
        assert_eq!(app.sidebar_width, 78);
        assert!(app.view_dirty_since.is_some(), "saved with the workspace's view");
    }

    fn project(id: &str, slug: &str) -> ProjectInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "slug": slug, "root": format!("/r/{slug}"), "created_ms": 0,
        }))
        .unwrap()
    }

    fn root(project: &str) -> WorktreeInfo {
        serde_json::from_value(serde_json::json!({
            "id": format!("{project}-root"), "project": project, "project_slug": project,
            "name": "@root", "path": "/r", "is_root": true, "state": "ready", "created_ms": 0,
        }))
        .unwrap()
    }

    #[test]
    fn a_project_registered_on_its_own_says_so_and_offers_an_undo() {
        let mut app = app();
        app.on_event(DomainEvent::ProjectAutoAdded {
            info: project("p1", "myrepo"),
            workspace: "main".into(),
            notes: Vec::new(),
        });
        assert!(app.project("p1").is_some());
        let t = app.toast.as_ref().unwrap();
        assert_eq!(t.text, "registered myrepo in main · C-q u undo");
        assert!(matches!(
            &t.offer,
            Some(Offer { then: Then::Send(req, _), .. })
                if matches!(**req, Request::ProjectRemove { release: true, .. })
        ));
        // `C-q u` takes it (the request goes to the daemon) and clears the message.
        let act = app.keymap.lookup(Table::Prefix, KeyEvent::char('u')).cloned().unwrap();
        app.run(&act);
        assert!(app.toast.is_none());
        app.run(&act);
        assert_eq!(app.toast.as_ref().unwrap().text, "nothing to undo or fix right now");
    }

    #[test]
    fn with_no_project_an_action_carries_on_once_one_is_added() {
        let mut app = app();
        // `N` with no project asks for one; once it's added and its checkout is known, `N`
        // goes on as if it had been there all along.
        app.run(&Command { action: "worktree.new", arg: None });
        assert!(app.overlay.is_none(), "the picker opens once discovery answers");
        app.projects.push(project("p1", "myrepo"));
        app.after_add = Some(("p1".into(), AfterAdd::Run("worktree.new")));
        app.try_after_add();
        assert!(app.after_add.is_some(), "waits for the project's main checkout");
        app.on_event(DomainEvent::WorktreeUpdated { info: root("p1") });
        assert!(app.after_add.is_none());
        match &app.overlay {
            Some(Overlay::Prompt { kind: PromptKind::WorktreeName(w), title, .. }) => {
                assert_eq!(w.project.as_deref(), Some("p1"));
                assert!(title.contains("myrepo"), "{title}");
            }
            _ => panic!("the new-worktree prompt, in the new project"),
        }
        // The wizard's "where" offers adding one when there's none (after "here").
        app.overlay = None;
        app.projects.clear();
        app.worktrees.clear();
        app.open_targets(Wizard::default());
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!() };
        let labels: Vec<&str> =
            (0..palette.filtered.len()).map(|i| palette.item(i).label.as_str()).collect();
        assert_eq!(labels, ["here", "+ add a project…"]);
    }

    fn key(app: &mut App, code: KeyCode, mods: Mods) {
        app.overlay_key(KeyEvent::new(code, mods));
    }

    /// A project, its main checkout, and the workspace `main` holding it.
    fn with_project(app: &mut App) {
        app.projects.push(project("p1", "myrepo"));
        app.worktrees.push(root("p1"));
        app.workspaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "w", "name": "main", "profile": "default", "projects": ["p1"],
                "notify": "all", "created_ms": 0,
            }))
            .unwrap(),
        );
    }

    #[test]
    fn the_task_box_names_the_work_after_what_you_type() {
        let mut app = app();
        with_project(&mut app);
        app.run(&Command { action: "session.new", arg: None });
        let shown = screen(&app);
        assert!(shown.contains("What should it do?"), "{shown}");
        assert!(
            shown.contains("→ claude · new worktree in myrepo · a random name"),
            "an empty ⏎ is the old n ⏎ ⏎: {shown}"
        );
        typed(&mut app, "Fix the flaky test in tests/api.rs");
        let shown = screen(&app);
        assert!(shown.contains("→ fix-flaky-test · claude · new worktree in myrepo"), "{shown}");
        // A worktree by that name exists: the next free one.
        let mut taken = root("p1");
        taken.id = "w2".into();
        taken.name = "fix-flaky-test".into();
        taken.is_root = false;
        app.worktrees.push(taken);
        assert!(screen(&app).contains("→ fix-flaky-test-2 ·"));
        // Tab edits the name: it starts from the suggestion and stops following the prompt.
        key(&mut app, KeyCode::Tab, Mods::NONE);
        typed(&mut app, "\x08\x08");
        typed(&mut app, "-x");
        assert!(screen(&app).contains("→ fix-flaky-test-x ·"), "{}", screen(&app));
        key(&mut app, KeyCode::Tab, Mods::NONE);
        typed(&mut app, " now");
        assert!(screen(&app).contains("→ fix-flaky-test-x ·"), "the edited name stays");
        // A space in the name becomes _; what no character fixes says why, and ⏎ keeps the box.
        key(&mut app, KeyCode::Tab, Mods::NONE);
        typed(&mut app, " y");
        assert!(screen(&app).contains("→ fix-flaky-test-x_y ·"), "{}", screen(&app));
        typed(&mut app, ".");
        assert!(screen(&app).contains("✗ name:"), "{}", screen(&app));
        key(&mut app, KeyCode::Enter, Mods::NONE);
        assert!(matches!(app.overlay, Some(Overlay::Task { .. })));
        // C-o: the full wizard, the prompt carried along.
        key(&mut app, KeyCode::Char { c: 'o' }, Mods(Mods::CTRL));
        match &app.overlay {
            Some(Overlay::Picker { purpose: Purpose::Agent(w), .. }) => {
                assert_eq!(w.prompt.as_deref(), Some("Fix the flaky test in tests/api.rs now"));
            }
            _ => panic!("the agent picker"),
        }
    }

    #[test]
    fn create_dialogs_offer_a_name_that_enter_takes() {
        let mut app = app();
        with_project(&mut app);
        app.run(&Command { action: "worktree.new", arg: None });
        let Some(Overlay::Prompt { kind: PromptKind::WorktreeName(w), .. }) = &app.overlay else {
            panic!("the name prompt")
        };
        let offered = w.suggested.clone().unwrap();
        let shown = screen(&app);
        assert!(
            shown.contains(&offered) && shown.contains(&format!("⏎ takes {offered}")),
            "{shown}"
        );
        typed(&mut app, "\n");
        match &app.overlay {
            Some(Overlay::Picker { purpose: Purpose::Agent(w), .. }) => {
                assert_eq!(w.name.as_deref(), Some(offered.as_str()))
            }
            _ => panic!("then the agent"),
        }
        // A workspace: a profile's name, else workspace-2.
        app.overlay = None;
        app.picked(Target::NewWorkspace, Purpose::Workspace);
        assert!(screen(&app).contains("workspace-2"));
    }

    #[test]
    fn a_title_shows_where_people_read_and_rename_can_switch_to_it() {
        let mut app = app();
        let mut s = session("fix-flaky-test");
        s.title = Some("Fix the flaky test in tests/api.rs".into());
        s.window_title = Some("fix-flaky-test".into());
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.sessions = vec![s.clone()];
        let shown = screen(&app);
        assert!(shown.contains("task Fix the flaky"), "the row: {shown}");
        assert!(shown.contains("· Fix the flaky test in tests/api.rs"), "the header: {shown}");
        // A window title alone isn't a title.
        let mut w = session("plain");
        w.title = Some("vim".into());
        w.window_title = Some("vim".into());
        assert_eq!(crate::ui::session_title(&w), None);
        // `,` renames; C-t there edits the title instead, filled with it.
        app.run(&Command { action: "session.rename", arg: None });
        key(&mut app, KeyCode::Char { c: 't' }, Mods(Mods::CTRL));
        assert_eq!(prompt_text(&app), Some("Fix the flaky test in tests/api.rs"));
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { kind: PromptKind::Retitle(_), .. })));
    }

    #[test]
    fn messages_offer_the_fix_instead_of_a_command_to_type() {
        let mut app = app();
        // An error naming a CLI command offers its TUI action.
        let o = offer_for("not trusted: review with `ptyro project trust myrepo`").unwrap();
        assert!(matches!(o.then, Then::Run(Command { action: "project.trust", .. })), "{o:?}");
        assert!(offer_for("something broke").is_none());
        let act = |app: &mut App| {
            let c = app.keymap.lookup(Table::Prefix, KeyEvent::char('u')).cloned().unwrap();
            app.run(&c);
        };
        // An unbound key: `C-q u` lists the keys.
        app.prefix = true;
        app.on_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('y'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.toast.as_ref().unwrap().text, "C-q y: not bound · C-q u list the keys");
        act(&mut app);
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })));
        // Broadcast with nothing marked: `C-q u` goes to the sidebar to mark some.
        app.overlay = None;
        app.sessions = vec![session("a")];
        app.run(&Command { action: "broadcast", arg: None });
        act(&mut app);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    /// No message the TUI writes tells you to type a command (UX1.17): it offers the fix.
    #[test]
    fn no_toast_names_a_command_to_type() {
        for (file, src) in [
            ("app.rs", include_str!("app.rs")),
            ("inspector.rs", include_str!("inspector.rs")),
            ("runs.rs", include_str!("runs.rs")),
            ("sets.rs", include_str!("sets.rs")),
        ] {
            let lines: Vec<&str> = src.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if !(l.contains("toast(") || l.contains("toast_err(") || l.contains("toast_offer("))
                {
                    continue;
                }
                // The call and the lines of its arguments.
                let call = lines[i..(i + 6).min(lines.len())].join(" ");
                let call = call.split(';').next().unwrap_or("");
                assert!(
                    !call.contains("`ptyro ") && !call.contains(": project."),
                    "{file}:{}: a message names a command to type: {call}",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn a_pause_after_the_prefix_shows_what_each_key_does() {
        let mut app = app();
        app.prefix = true;
        app.prefix_at = Some(Instant::now());
        assert!(!screen(&app).contains("then…"), "not before the delay");
        app.prefix_at = Some(Instant::now() - Duration::from_secs(1));
        let shown = screen(&app);
        assert!(shown.contains("C-q then…"), "{shown}");
        for want in ["n a task", "a who needs you", "D diff & ship", ". its actions", "? all keys"]
        {
            assert!(shown.contains(want), "{want}: {shown}");
        }
        // Rebinding shows: `y` now zooms.
        let mut config = Config::default();
        config.keys.insert("y".into(), "pane.zoom".into());
        config.keys.insert("z".into(), "none".into());
        app.keymap = Keymap::new(&config).0;
        assert!(screen(&app).contains("y zoom"));
        app.config.ui.which_key_delay = 0;
        assert!(!screen(&app).contains("then…"), "0 turns it off");
    }

    #[test]
    fn a_row_s_menu_lists_what_applies_and_runs_it_there() {
        let mut app = app();
        with_project(&mut app);
        let mut live = session("live");
        live.worktree = Some("p1-root".into());
        live.project = Some("p1".into());
        let mut gone = session("gone");
        gone.state = ProcessState::Exited { code: Some(1), signal: None };
        app.sessions = vec![live.clone(), gone.clone()];
        let live_actions = app.row_actions(&Row::Session(live.id));
        assert!(live_actions.contains(&"session.stop") && live_actions.contains(&"inspector"));
        assert!(!live_actions.contains(&"session.restart"));
        let gone_actions = app.row_actions(&Row::Session(gone.id));
        assert!(
            gone_actions.contains(&"session.restart") && !gone_actions.contains(&"session.kill")
        );
        assert!(!app.row_actions(&Row::Worktree("p1-root".into())).contains(&"worktree.remove"));
        // Right-click a row: its menu, keys shown; pick one and it runs on that row.
        let lines = sidebar(&app);
        let y = lines.iter().position(|l| l.contains("gone")).unwrap() as u16;
        use crossterm::event::{MouseButton, MouseEventKind as K};
        app.on_mouse(&mouse(K::Down(MouseButton::Right), 3, y));
        let shown = screen(&app);
        assert!(shown.contains("Session gone") && shown.contains("session.restart"), "{shown}");
        assert!(shown.contains("C-q C-r"), "with its key: {shown}");
        app.overlay = None;
        app.focus = Focus::Pane;
        app.panes.get_mut(&1).unwrap().session = Some(live.id);
        app.run(&Command { action: "row.menu", arg: None });
        assert!(screen(&app).contains("Session live"), "in the pane: the focused session's");
    }

    #[test]
    fn the_status_bar_hints_what_to_do_with_what_s_selected() {
        let mut app = app();
        let mut asker = session("asker");
        status(&mut asker, AgentStatus::Blocked, Some("permission"), Some("Bash: rm"));
        let mut gone = session("gone");
        gone.state = ProcessState::Exited { code: Some(0), signal: None };
        app.sessions = vec![asker.clone(), gone.clone()];
        app.panes.get_mut(&1).unwrap().session = Some(asker.id);
        assert_eq!(app.context_hint().as_deref(), Some("C-q p answer it"));
        assert!(screen(&app).contains("C-q p answer it"));
        // In the sidebar, its own keys (no prefix).
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Session(gone.id)).unwrap();
        app.set_sel(&rows, i);
        assert_eq!(app.context_hint().as_deref(), Some("R restart · x remove"));
    }

    #[test]
    fn the_palette_finds_worktrees_projects_and_scripts_too() {
        let mut app = app();
        with_project(&mut app);
        app.run(&Command { action: "palette", arg: None });
        typed(&mut app, "worktree p1");
        assert!(screen(&app).contains("worktree p1/@root"), "{}", screen(&app));
        key(&mut app, KeyCode::Char { c: 'u' }, Mods(Mods::CTRL));
        typed(&mut app, "project my");
        assert!(screen(&app).contains("project myrepo"), "{}", screen(&app));
        key(&mut app, KeyCode::Char { c: 'u' }, Mods(Mods::CTRL));
        // Scripts arrive after it opened, and the query applies to them.
        typed(&mut app, "dev");
        let scripts: ScriptsInfo = serde_json::from_value(serde_json::json!({
            "project": "myrepo", "text": "", "trusted": true, "fingerprint": "f",
            "names": ["setup", "run:dev"],
        }))
        .unwrap();
        app.on_reply(AppMsg::Reply(
            Pending::PaletteScripts("p1-root".into()),
            Ok(Reply::Scripts { scripts }),
        ));
        let shown = screen(&app);
        assert!(shown.contains("script dev") && shown.contains("run it in p1/@root"), "{shown}");
        // A project: selected in the sidebar.
        app.overlay = None;
        app.picked(Target::Project("p1".into()), Purpose::Palette);
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(app.selected_row(), Some(Row::Project("p1".into())));
    }

    fn feature(app: &mut App, merged: bool) -> WorktreeInfo {
        let mut w = root("p1");
        w.id = "p1-feat".into();
        w.name = "feat".into();
        w.is_root = false;
        w.branch = Some("ptyro/feat".into());
        if merged {
            w.pr = Some(
                serde_json::from_value(serde_json::json!({
                    "number": 7, "url": "u", "title": "Add five", "state": "MERGED", "draft": false,
                    "checks": [], "automerge": false, "ours": true, "at_ms": 0,
                }))
                .unwrap(),
            );
        }
        app.worktrees.push(w.clone());
        w
    }

    #[test]
    fn finishing_and_cleaning_up_a_worktree() {
        let mut app = app();
        with_project(&mut app);
        let w = feature(&mut app, true);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Worktree(w.id.clone())).unwrap();
        app.set_sel(&rows, i);
        // Merged: the hint says clean up, and `x` archives (it never refuses) instead of removing.
        assert!(app.context_hint().unwrap().contains("clean up (merged)"));
        let x = app.keymap.lookup(Table::Nav, KeyEvent::char('x')).cloned().unwrap();
        app.run(&x);
        match &app.overlay {
            Some(Overlay::Confirm { action: Confirmed::Archive(id), text }) => {
                assert_eq!(id, &w.id);
                assert!(text.contains("snapshot of its files are kept"), "{text}");
            }
            _ => panic!("archive, not remove"),
        }
        // `F`, as the hint says, cleans it up too.
        app.overlay = None;
        let f = app.keymap.lookup(Table::Nav, KeyEvent::char('F')).cloned().unwrap();
        app.run(&f);
        assert!(matches!(
            &app.overlay,
            Some(Overlay::Confirm { action: Confirmed::Archive(_), .. })
        ));
        // Not shipped yet: finish is a PR or integrating.
        app.overlay = None;
        app.worktrees.iter_mut().for_each(|x| x.pr = None);
        app.run(&f);
        let shown = screen(&app);
        assert!(shown.contains("Finish p1/feat") && shown.contains("a pull request"), "{shown}");
        assert!(shown.contains("integrate"), "{shown}");
        // Integrated into main, clean, at that commit: shipped too ("in main"); a new commit
        // there makes it unshipped again.
        app.overlay = None;
        let status = |head: &str| ptyro_core::model::RepoStatus {
            head: Some(head.into()),
            ..serde_json::from_value(serde_json::json!({
                "ahead": 0, "behind": 0, "base_ahead": 0, "base_behind": 0, "staged": 0,
                "modified": 0, "untracked": 0, "conflicts": 0, "insertions": 0, "deletions": 0,
                "at_ms": 0
            }))
            .unwrap()
        };
        for x in app.worktrees.iter_mut().filter(|x| x.id == w.id) {
            x.integrated = Some("0e2da8484aaaa".into());
            x.status = Some(status("0e2da8484"));
        }
        assert!(app.context_hint().unwrap().contains("clean up (in main)"));
        assert!(screen(&app).contains("in main"), "{}", screen(&app));
        for x in app.worktrees.iter_mut().filter(|x| x.id == w.id) {
            x.status = Some(status("1f00ba7"));
        }
        assert!(!app.context_hint().unwrap_or_default().contains("clean up"));
        // A removal that would lose work offers archiving instead.
        app.overlay = None;
        app.on_reply(AppMsg::Reply(
            Pending::Removed(w.id.clone()),
            Err(Error::new(ErrorCode::DirtyWorktree, "refusing to remove p1/feat")),
        ));
        assert!(screen(&app).contains("a: archive it instead"));
        key(&mut app, KeyCode::Char { c: 'a' }, Mods::NONE);
        assert!(app.overlay.is_none(), "archived (the request went out)");
        // The row menu has them all, the companion shell too.
        let actions = app.row_actions(&Row::Worktree(w.id.clone()));
        for a in ["worktree.finish", "worktree.archive", "shell.drawer", "shell.new"] {
            assert!(actions.contains(&a) && keymap::spec(a).is_some(), "{a}");
        }
    }

    #[test]
    fn prs_and_issues_are_recognised_in_text() {
        assert_eq!(work_source("https://github.com/acme/app/pull/123"), Some((Some(123), None)));
        assert_eq!(
            work_source("https://github.com/acme/app/pull/123/files"),
            Some((Some(123), None))
        );
        assert_eq!(
            work_source("https://github.com/acme/app/issues/45"),
            Some((None, Some("https://github.com/acme/app/issues/45".into())))
        );
        assert_eq!(work_source("pr 7"), Some((Some(7), None)));
        assert_eq!(work_source("PR #7"), Some((Some(7), None)));
        assert_eq!(work_source("issue 9"), Some((None, Some("9".into()))));
        assert_eq!(work_source("fix the pr template"), None);
        assert_eq!(work_source("pr seven"), None);
    }

    #[test]
    fn work_can_come_from_a_pr_a_branch_or_your_changes() {
        let mut app = app();
        with_project(&mut app);
        // The task box takes a pasted PR.
        app.run(&Command { action: "session.new", arg: None });
        typed(&mut app, "https://github.com/acme/app/pull/12");
        assert!(screen(&app).contains("→ pr-12 · claude · PR #12's branch, in a new worktree"));
        // The wizard's "where": a PR or issue, a branch, and (the main checkout is dirty) your
        // changes.
        app.overlay = None;
        let st = ptyro_core::model::RepoStatus { modified: 2, ..Default::default() };
        app.worktrees[0].status = Some(st);
        app.open_targets(Wizard { project: Some("p1".into()), ..Wizard::default() });
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!() };
        let labels: Vec<&str> =
            (0..palette.filtered.len()).map(|i| palette.item(i).label.as_str()).collect();
        for want in [
            "+ from a PR or an issue…",
            "+ on an existing branch…",
            "+ with my uncommitted changes",
        ] {
            assert!(labels.contains(&want), "{want}: {labels:?}");
        }
    }

    #[test]
    fn a_first_worktree_offers_what_the_project_needs() {
        let mut app = app();
        with_project(&mut app);
        let spec = WorktreeSpec { project: "p1".into(), setup: true, ..WorktreeSpec::default() };
        let suggestions = ptyro_core::model::SetupSuggestions {
            project: "myrepo".into(),
            setup: Some("pnpm install --frozen-lockfile".into()),
            from: Some("pnpm-lock.yaml".into()),
            copy: vec![".env".into()],
            offered: false,
        };
        app.on_reply(AppMsg::Reply(
            Pending::Suggest(Box::new(spec.clone()), None),
            Ok(Reply::Suggestions { suggestions: suggestions.clone() }),
        ));
        let shown = screen(&app);
        assert!(
            shown.contains("pnpm-lock.yaml → setup: pnpm install --frozen-lockfile"),
            "{shown}"
        );
        assert!(shown.contains(".env → copied into each worktree"), "{shown}");
        key(&mut app, KeyCode::Char { c: 'n' }, Mods::NONE);
        assert!(app.overlay.is_none(), "declined: the worktree is made without them");
        // Offered once: not again.
        app.on_reply(AppMsg::Reply(
            Pending::Suggest(Box::new(spec), None),
            Ok(Reply::Suggestions {
                suggestions: ptyro_core::model::SetupSuggestions { offered: true, ..suggestions },
            }),
        ));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn the_first_run_ticks_what_to_use_and_writes_the_config() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| h.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let mut app = App::new(
            Options::default(),
            connect,
            paths,
            Config::default(),
            false,
            Rect::new(0, 0, 100, 30),
        );
        use crate::onboard::{Choice, ChoiceKind, FirstRun};
        app.first_run = Some(FirstRun {
            agents: vec![
                ("claude".into(), "claude".into(), true),
                ("codex".into(), "codex".into(), false),
            ],
            choices: vec![
                Choice {
                    section: "accounts",
                    label: "profile work".into(),
                    detail: "claude ~/.claude-work".into(),
                    on: true,
                    kind: ChoiceKind::Profile(
                        "[profiles.work]\nclaude = \"~/.claude-work\"\n".into(),
                    ),
                },
                Choice {
                    section: "notifications",
                    label: "desktop notifications".into(),
                    detail: String::new(),
                    on: false,
                    kind: ChoiceKind::Desktop,
                },
            ],
            sel: 0,
        });
        app.first_run_repos(vec!["/src/alpha".into()]);
        let shown = screen(&app);
        assert!(shown.contains("Welcome to ptyro"), "{shown}");
        assert!(shown.contains("✓ claude (default)") && !shown.contains("✓ codex"), "{shown}");
        assert!(shown.contains("[x] profile work") && shown.contains("[ ] alpha"), "{shown}");
        // j j: past the repository to notifications; Space ticks it; ⏎ writes the config.
        let k = |app: &mut App, code| app.first_run_key(KeyEvent::new(code, Mods::NONE));
        k(&mut app, KeyCode::Char { c: 'j' });
        k(&mut app, KeyCode::Char { c: 'j' });
        k(&mut app, KeyCode::Char { c: ' ' });
        k(&mut app, KeyCode::Enter);
        assert!(app.first_run.is_none());
        let text = std::fs::read_to_string(Config::path(&app.paths)).unwrap();
        assert!(text.contains("[profiles.work]") && text.contains("\"desktop\""), "{text}");
        assert!(app.toast.as_ref().unwrap().text.contains("1 profile(s), 0 project(s)"));
        assert!(app.toast.as_ref().unwrap().text.contains("take the tour"));
    }

    #[test]
    fn home_lists_what_to_do_next() {
        let mut app = app();
        with_project(&mut app);
        let mut asker = session("asker");
        asker.attention = Some(
            serde_json::from_value(serde_json::json!({
                "kind": "blocked", "since_ms": 1, "text": "wants permission: Bash: rm",
            }))
            .unwrap(),
        );
        app.sessions = vec![asker];
        app.show_home(vec!["/r/myrepo".into(), "/src/other".into()]);
        let Some(Overlay::Picker { palette, purpose: Purpose::Home }) = &app.overlay else {
            panic!("home")
        };
        let labels: Vec<String> =
            (0..palette.filtered.len()).map(|i| palette.item(i).label.clone()).collect();
        assert_eq!(labels, ["⚑ asker", "▸ myrepo", "+ other", "a shell, here"], "known repos once");
    }

    #[test]
    fn the_health_badge_lists_what_the_doctor_found_with_fixes() {
        let mut app = app();
        let check = |section: &str, name: &str, level: &str, text: &str| crate::health::Check {
            section: section.into(),
            name: name.into(),
            level: level.into(),
            text: text.into(),
        };
        app.got_health(vec![
            check("terminal", "TERM", "ok", "xterm-256color"),
            check(
                "daemon",
                "running",
                "warn",
                "not this ptyro: `ptyro daemon upgrade` (sessions keep running)",
            ),
            check(
                "profile work",
                "claude",
                "warn",
                "not logged in (`ptyro profile login <profile> claude`)",
            ),
        ]);
        let shell = usize::from(crate::health::shell_init_missing().is_some());
        let n = 2 + shell;
        assert_eq!(app.health.findings.len(), n);
        assert!(screen(&app).contains(&format!("⚕ {n}")), "the badge");
        assert!(app.toast.as_ref().unwrap().text.contains("C-q M shows them"));
        let m = app.keymap.lookup(Table::Prefix, KeyEvent::char('M')).cloned().unwrap();
        app.run(&m);
        let shown = screen(&app);
        assert!(shown.contains("Health: ⏎ fixes one"), "{shown}");
        assert!(shown.contains("! daemon running"), "{shown}");
        // The same findings again: not announced twice.
        app.overlay = None;
        app.toast = None;
        app.got_health(vec![
            check("daemon", "running", "warn", "not this ptyro: `ptyro daemon upgrade`"),
            check(
                "profile work",
                "claude",
                "warn",
                "not logged in (`ptyro profile login <profile> claude`)",
            ),
        ]);
        assert!(app.toast.is_none());
        // A file fix asks first, then appends.
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        app.health.findings.push(crate::health::Finding {
            problem: false,
            what: "shell integration".into(),
            text: "missing".into(),
            fix: Some(crate::health::Fix::Append { file: rc.clone(), line: "eval x".into() }),
        });
        let i = app.health.findings.len() - 1;
        app.fix(i);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Confirm { action: Confirmed::Append(..), .. })
        ));
        key(&mut app, KeyCode::Char { c: 'y' }, Mods::NONE);
        assert!(std::fs::read_to_string(&rc).unwrap().contains("eval x"));
        assert_eq!(app.health.findings.len(), i, "fixed: off the list");
    }

    #[test]
    fn the_cli_s_long_tail_runs_with_what_is_selected_picked_or_typed() {
        let mut app = app();
        app.profiles = vec![
            serde_json::from_value(serde_json::json!({
                "name": "work", "label": "wrk", "defined": true, "isolate": false,
                "dirs": {}, "vars": [],
            }))
            .unwrap(),
        ];
        // Two picks: the profile, then the agent.
        app.run(&Command { action: "profile.login", arg: None });
        typed(&mut app, "work\n");
        typed(&mut app, "claude\n");
        assert_eq!(
            app.last_cli.as_deref(),
            Some(&["profile", "login", "work", "claude"].map(String::from)[..])
        );
        // A question and variables.
        app.run(&Command { action: "recipe.run", arg: None });
        typed(&mut app, "implement-and-review\n");
        typed(&mut app, "task=add json, n=2\n");
        assert_eq!(
            app.last_cli.clone().unwrap(),
            ["recipe", "run", "implement-and-review", "--var", "task=add json", "--var", "n=2"]
        );
        // The selected session, a choice, and a file.
        let s = session("worker");
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.sessions = vec![s];
        app.run(&Command { action: "session.export", arg: None });
        typed(&mut app, "text\n");
        typed(&mut app, "out.txt\n");
        assert_eq!(
            app.last_cli.clone().unwrap(),
            ["export", "worker", "--format", "text", "-o", "out.txt"]
        );
        // An optional word left empty: left out.
        app.run(&Command { action: "session.verify", arg: None });
        typed(&mut app, "\n");
        assert_eq!(app.last_cli.clone().unwrap(), ["verify", "worker"]);
        // Nothing selected that it needs: it says what to pick.
        app.last_cli = None;
        app.run(&Command { action: "set.push", arg: None });
        assert!(app.last_cli.is_none());
        assert!(app.toast.as_ref().unwrap().text.contains("pick a change set first"));
    }

    #[test]
    fn opening_a_pr_goes_to_its_worktree_s_session() {
        let mut app = app();
        with_project(&mut app);
        let w = feature(&mut app, true);
        let mut s = session("feat-agent");
        s.worktree = Some(w.id.clone());
        s.project = Some("p1".into());
        app.sessions = vec![s.clone()];
        app.run(&Command { action: "pr.open", arg: None });
        typed(&mut app, "7\n");
        assert_eq!(app.focused_session(), Some(s.id));
    }

    #[test]
    fn a_bad_name_is_flagged_as_it_is_typed_and_enter_keeps_it() {
        let mut app = app();
        let s = session("brisk-rook");
        app.sessions = vec![s.clone(), session("quiet-jay")];
        rename(&mut app, &s);
        assert!(screen(&app).contains("⏎ ok (unchanged)"));

        // What it can't hold becomes _, and the dialog says so.
        typed(&mut app, "/X");
        assert_eq!(prompt_text(&app), Some("brisk-rook_x"));
        assert!(screen(&app).contains("becomes _ (a-z 0-9 . _ -)"), "{}", screen(&app));
        // What no one character fixes is flagged, and ⏎ doesn't throw the typing away.
        typed(&mut app, ".");
        let shown = screen(&app);
        assert!(shown.contains("✗ must not end with . or .lock"), "{shown}");
        typed(&mut app, "\n");
        assert_eq!(prompt_text(&app), Some("brisk-rook_x."));
        typed(&mut app, "\x08");
        assert_eq!(prompt_text(&app), Some("brisk-rook_x"));
        assert!(app.prompt_check(&PromptKind::Rename(s.id), "brisk-rook_x").is_ok());
        assert!(
            app.prompt_check(&PromptKind::Rename(s.id), "brisk/rook").is_err(),
            "the daemon's rule"
        );

        // Another session's name is taken; its own isn't.
        assert_eq!(
            app.prompt_check(&PromptKind::Rename(s.id), "quiet-jay"),
            Err("a session named quiet-jay exists".into())
        );
        assert!(app.prompt_check(&PromptKind::Rename(s.id), "brisk-rook").is_ok());
    }

    #[test]
    fn workspace_and_fanout_prompts_are_checked_too() {
        let app = app();
        let ws = PromptKind::NewWorkspace(None, None);
        assert!(app.prompt_check(&ws, "").is_ok(), "empty cancels");
        assert!(app.prompt_check(&ws, "Side Job").unwrap_err().contains("'S' (uppercase)"));
        assert!(app.prompt_check(&ws, "side-job").is_ok());

        let fan = PromptKind::Orch(crate::runs::OrchInput::FanoutAgents { project: "p".into() });
        assert!(app.prompt_check(&fan, "claude, codex@work").is_ok());
        assert!(
            app.prompt_check(&fan, "claude,cladue").unwrap_err().contains("unknown agent cladue")
        );
        // Free text isn't a name: anything goes.
        assert!(app.prompt_check(&PromptKind::Broadcast(Vec::new()), "Fix /it/ now!").is_ok());
    }

    // ── the quick terminal (UX1.25) ──────────────────────────────────────

    /// A project with two agents in its main checkout, the first shown.
    fn with_agents(app: &mut App) -> (SessionId, SessionId) {
        with_project(app);
        let (a, b) = (
            placed("claude", Some("p1"), Some("p1-root")),
            placed("codex", Some("p1"), Some("p1-root")),
        );
        app.sessions.extend([a.clone(), b.clone()]);
        app.show(a.id, None);
        (a.id, b.id)
    }

    fn quick(name: &str, worktree: Option<&str>) -> SessionInfo {
        let mut s = placed(name, worktree.map(|_| "p1"), worktree);
        s.quick = true;
        s.argv = vec!["/bin/zsh".into(), "-l".into()];
        s
    }

    /// `[ui] shell_style = "auto"`: `!` is a drawer while its project is open (the default is
    /// a panel since UX3.1).
    fn auto_shell(app: &mut App) {
        app.config.ui.shell_style = ptyro_core::config::ShellStyle::Auto;
    }

    /// The first `shell.here` asks the daemon; here it answers with `shell`.
    fn answered(app: &mut App, shell: &SessionInfo) {
        app.run(&Command { action: "shell.here", arg: None });
        assert!(app.drawer.is_none(), "nothing to show until the daemon answers");
        app.on_reply(AppMsg::Reply(
            Pending::Quick(app.config.ui.shell_style),
            Ok(Reply::Session { info: shell.clone() }),
        ));
    }

    fn here(app: &mut App) {
        app.run(&Command { action: "shell.here", arg: None });
    }

    #[test]
    fn one_key_shows_focuses_and_hides_one_quick_terminal() {
        let mut app = app();
        auto_shell(&mut app);
        let (agent, _) = with_agents(&mut app);
        let host = app.focused;
        let shell = quick("myrepo.shell", Some("p1-root"));
        answered(&mut app, &shell);
        // Shown below the agent, and focused.
        let d = app.drawer.expect("a drawer");
        assert_eq!((d.host, app.focused), (host, d.pane));
        assert_eq!(shows(&app, d.pane).as_deref(), Some("myrepo.shell"));
        let rects: HashMap<PaneId, Rect> = app.pane_rects().into_iter().collect();
        assert!(rects[&d.pane].y > rects[&host].y && rects[&d.pane].height < rects[&host].height);
        assert!(screen(&app).contains("hide"), "the drawer says how to hide it");
        // Focused: hidden, and focus is back on the agent. The shell itself stays.
        here(&mut app);
        assert!(app.drawer.is_none());
        assert_eq!((app.focused, app.focused_session()), (host, Some(agent)));
        assert_eq!(app.layout.panes().len(), 1);
        // Again: the same shell, no new one asked for (it's known and live).
        here(&mut app);
        let d = app.drawer.expect("shown again");
        assert_eq!(app.panes[&d.pane].session, Some(shell.id));
        // Shown but not focused: focused, not hidden.
        app.focused = host;
        here(&mut app);
        assert_eq!(app.focused, d.pane);
        assert!(app.drawer.is_some());
    }

    #[test]
    fn sessions_of_a_worktree_share_its_quick_terminal_and_c_q_c_t_toggles_it() {
        let mut app = app();
        let (_, codex) = with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        // From the other agent of the same worktree, by the keys: the same shell.
        app.show(codex, None);
        let press = |app: &mut App, c: char| {
            app.on_key(&crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::CONTROL,
            ))
        };
        press(&mut app, 'q');
        press(&mut app, 't');
        let d = app.drawer.expect("C-q C-t opens it");
        assert_eq!(app.panes[&d.pane].session, Some(shell.id));
        press(&mut app, 'q');
        press(&mut app, 't');
        assert!(app.drawer.is_none(), "and closes it");
        // Not one of the sessions j/k and numbers go through, and never filled into a pane.
        assert!(!app.ordered().contains(&shell.id));
        assert!(!app.numbered().contains(&shell.id));
    }

    #[test]
    fn with_no_worktree_the_quick_terminal_asks_for_a_project_first() {
        let mut app = app();
        // An empty pane: no session, no project, so no worktree to belong to (UX3.4).
        while app.rx.try_recv().is_ok() {}
        act(&mut app, "shell.drawer");
        assert!(app.drawer.is_none());
        let mut asked = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::Discovered(then), _) = m {
                asked = Some(then);
            }
        }
        assert!(
            matches!(asked, Some(Some(AfterAdd::Run("shell.drawer")))),
            "the project picker, then the drawer: {asked:?}"
        );
    }

    /// Project p1 with one agent shown, and project p2's quick terminal (not yet on screen).
    fn two_projects(app: &mut App) -> (SessionId, SessionInfo) {
        let (agent, _) = with_agents(app);
        app.projects.push(project("p2", "other"));
        app.worktrees.push(root("p2"));
        let shell = quick("other.shell", Some("p2-root"));
        let mut shell = shell;
        shell.project = Some("p2".into());
        app.sessions.push(shell.clone());
        (agent, shell)
    }

    fn act(app: &mut App, action: &'static str) {
        app.run(&Command { action, arg: None });
    }

    #[test]
    fn a_shell_of_a_project_nobody_has_open_is_a_panel_of_its_own() {
        let mut app = app();
        let (agent, shell) = two_projects(&mut app);
        let pane = app.focused;
        // Project p2 has no pane: `!` replaces the focused pane, no split, no drawer.
        app.context_hint = Some("p2".into());
        act(&mut app, "shell.here");
        assert!(app.drawer.is_none());
        assert_eq!(app.layout.panes().len(), 1);
        assert_eq!((app.focused, app.focused_session()), (pane, Some(shell.id)));
        assert!(screen(&app).contains("back"), "its header says how to go back: {}", screen(&app));
        // Again: back to what it replaced.
        act(&mut app, "shell.here");
        assert_eq!(app.focused_session(), Some(agent));
        // And once more it's a panel again, not a split.
        act(&mut app, "shell.here");
        assert_eq!(app.focused_session(), Some(shell.id));
        assert_eq!(app.layout.panes().len(), 1);
    }

    #[test]
    fn a_shell_of_an_open_project_is_a_drawer_and_an_empty_pane_is_filled() {
        let mut app = app();
        auto_shell(&mut app);
        let (agent, _) = with_agents(&mut app);
        let host = app.focused;
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        act(&mut app, "shell.here");
        assert_eq!(app.drawer.map(|d| d.host), Some(host), "p1 is open: a drawer");
        assert_eq!(app.panes[&host].session, Some(agent));
        // Hide it, then empty the pane: nothing of p1 is open, so the shell fills the pane.
        act(&mut app, "shell.here");
        assert!(app.drawer.is_none());
        app.panes.get_mut(&host).unwrap().session = None;
        act(&mut app, "shell.here");
        assert!(app.drawer.is_none());
        assert_eq!(app.focused_session(), Some(shell.id));
        act(&mut app, "shell.here");
        assert_eq!(app.focused_session(), None, "back to the empty pane it filled");
    }

    #[test]
    fn a_drawer_goes_under_a_pane_of_the_project_even_when_another_is_focused() {
        let mut app = app();
        let (_, shell) = two_projects(&mut app);
        let (left, other_shell_host) =
            (app.focused, app.layout.split(app.focused, Dir::Right).unwrap());
        app.panes.insert(other_shell_host, PaneState::default());
        let b = placed("gemini", Some("p2"), Some("p2-root"));
        app.sessions.push(b.clone());
        app.panes.get_mut(&other_shell_host).unwrap().session = Some(b.id);
        app.focused = left; // an agent of p1 is focused; p2's is beside it
        app.context_hint = Some("p2".into()); // and p2 is the one asked about
        act(&mut app, "shell.drawer");
        let d = app.drawer.expect("a drawer");
        assert_eq!(d.host, other_shell_host, "under p2's pane, not the focused p1 one");
        assert_eq!(app.panes[&d.pane].session, Some(shell.id));
        // Again from inside: hides.
        act(&mut app, "shell.drawer");
        assert!(app.drawer.is_none());
    }

    #[test]
    fn the_drawer_key_says_why_when_no_pane_of_the_project_is_open() {
        let mut app = app();
        let (_, shell) = two_projects(&mut app);
        app.context_hint = Some("p2".into());
        act(&mut app, "shell.drawer");
        assert!(app.drawer.is_none());
        assert_eq!(app.focused_session(), Some(shell.id));
        assert!(app.toast.as_ref().is_some_and(|t| t.text.contains("panel of its own")));
    }

    #[test]
    fn the_pane_key_is_a_panel_even_where_a_drawer_would_do_and_moves_a_drawer() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        act(&mut app, "shell.pane");
        assert!(app.drawer.is_none());
        assert_eq!(app.focused_session(), Some(shell.id));
        act(&mut app, "shell.pane");
        assert_eq!(app.focused_session(), Some(agent));
        // A shown drawer becomes a panel.
        act(&mut app, "shell.drawer");
        assert!(app.drawer.is_some());
        act(&mut app, "shell.pane");
        assert!(app.drawer.is_none());
        assert_eq!(app.focused_session(), Some(shell.id));
    }

    #[test]
    fn shell_style_sets_what_bang_does_and_the_keys_are_bound() {
        let mut app = app();
        with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        app.config = Config::parse("[ui]\nshell_style = \"pane\"\n").unwrap();
        act(&mut app, "shell.here");
        assert!(app.drawer.is_none(), "pane: always a panel, though p1 is open");
        assert_eq!(app.focused_session(), Some(shell.id));
        act(&mut app, "shell.here");
        app.config = Config::parse("[ui]\nshell_style = \"drawer\"\n").unwrap();
        act(&mut app, "shell.here");
        assert!(app.drawer.is_some());
        for (key, action) in [("C-t", "shell.drawer"), ("T", "shell.pane"), ("!", "shell.new")] {
            assert!(
                app.keymap.key_for(action).is_some_and(|k| k.ends_with(key)),
                "{action} is on {key}"
            );
        }
        for a in ["shell.new", "shell.here", "shell.drawer", "shell.pane"] {
            assert!(crate::keymap::ACTIONS.iter().any(|x| x.id == a), "{a} is registered");
        }
        assert!(app.row_actions(&Row::Quick(shell.id)).contains(&"shell.pane"));
    }

    #[test]
    fn the_quick_terminal_s_line_toggles_it_and_says_what_runs() {
        let mut app = app();
        auto_shell(&mut app);
        with_agents(&mut app);
        let mut shell = quick("myrepo.shell", Some("p1-root"));
        shell.command = Some(ptyro_core::session::ShellCommand {
            text: Some("cargo test".into()),
            started_ms: 0,
            ended_ms: None,
            exit: None,
        });
        app.sessions.push(shell.clone());
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Quick(shell.id)).expect("its line");
        assert!(matches!(rows[i - 1], Row::Worktree(_)), "first under its worktree");
        assert!(screen(&app).contains("›_ quick shell cargo te"), "{}", screen(&app));
        app.focus = Focus::Sidebar;
        app.set_sel(&rows, i);
        assert_eq!(app.context_hint().as_deref(), Some("⏎ show it"));
        app.run(&Command { action: "nav.open", arg: None });
        assert_eq!(app.drawer_session(), Some(shell.id));
        app.focus = Focus::Sidebar;
        app.set_sel(&app.rows(), i);
        assert_eq!(app.context_hint().as_deref(), Some("⏎ hide it"));
        assert!(app.row_actions(&Row::Quick(shell.id)).contains(&"shell.drawer"));
        // A finished command: how it went.
        let s = app.sessions.iter_mut().find(|s| s.id == shell.id).unwrap();
        s.command.as_mut().unwrap().ended_ms = Some(1);
        s.command.as_mut().unwrap().exit = Some(1);
        assert!(screen(&app).contains("›_ quick shell ✗ cargo"));
    }

    #[test]
    fn showing_another_session_tucks_the_drawer_and_the_popup_away_and_the_open_line_is_tinted() {
        let mut app = app();
        auto_shell(&mut app);
        let (agent, other) = with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        app.run(&Command { action: "shell.here", arg: None });
        app.on_reply(AppMsg::Reply(
            Pending::Quick(app.config.ui.shell_style),
            Ok(Reply::Session { info: shell.clone() }),
        ));
        assert_eq!(app.drawer_session(), Some(shell.id));
        // Open: its sidebar line is tinted; closed, it isn't.
        let tint = |app: &App| {
            let mut t =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
            t.draw(|f| crate::ui::draw(app, f)).unwrap();
            let buf = t.backend().buffer().clone();
            let row = (0..buf.area.height)
                .find(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, *y)].symbol())
                        .collect::<String>()
                        .contains("quick shell")
                })
                .expect("its line");
            buf[(2, row)].bg
        };
        assert_eq!(tint(&app), app.depth.fit(app.theme.bar_bg));
        // Another session picked in the sidebar: the drawer goes, the shell keeps running.
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Session(other)).expect("the other agent");
        app.set_sel(&rows, i);
        app.run(&Command { action: "nav.open", arg: None });
        assert!(app.drawer.is_none(), "the drawer tucked away");
        assert_eq!(app.focused_session(), Some(other));
        assert_ne!(tint(&app), app.depth.fit(app.theme.bar_bg));
        assert!(app.sessions.iter().any(|s| s.id == shell.id));
        // The scratchpad's popup too.
        pad_shown(&mut app, "popup");
        assert!(app.pad_popup.is_some());
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Session(agent)).unwrap();
        app.set_sel(&rows, i);
        app.run(&Command { action: "nav.open", arg: None });
        assert!(app.pad_popup.is_none(), "the popup tucked away");
    }

    #[test]
    fn exit_closes_the_drawer_and_the_next_press_asks_for_a_fresh_one() {
        let mut app = app();
        auto_shell(&mut app);
        let (agent, _) = with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        answered(&mut app, &shell);
        app.on_server(ServerMsg::Event { event: DomainEvent::SessionRemoved { id: shell.id } });
        assert!(app.drawer.is_none());
        assert_eq!(app.layout.panes().len(), 1);
        assert_eq!(app.focused_session(), Some(agent), "the agent's pane didn't move on");
        assert_eq!(app.quick_of(Some("p1-root"), std::path::Path::new("")), None);
    }

    #[test]
    fn a_zoomed_pane_keeps_its_drawer_and_closing_the_host_takes_it_along() {
        let mut app = app();
        auto_shell(&mut app);
        with_agents(&mut app);
        let host = app.focused;
        let right = app.layout.split(host, Dir::Right).unwrap();
        app.panes.insert(right, PaneState::default());
        let shell = quick("myrepo.shell", Some("p1-root"));
        answered(&mut app, &shell);
        let d = app.drawer.unwrap();
        app.zoom = true;
        let shown: Vec<PaneId> = app.pane_rects().into_iter().map(|(p, _)| p).collect();
        assert_eq!(shown, vec![host, d.pane], "zoomed with its drawer");
        app.zoom = false;
        // Five panes are fine with a drawer: four of your own plus it.
        assert!(app.layout.panes().len() <= crate::layout::MAX_WITH_DRAWER);
        app.focused = host;
        app.run(&Command { action: "pane.close", arg: None });
        assert!(app.drawer.is_none());
        assert_eq!(app.layout.panes(), vec![right], "the drawer went with its host");
    }

    #[test]
    fn the_drawer_and_its_height_come_back_with_the_view() {
        let mut app = app();
        auto_shell(&mut app);
        with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        answered(&mut app, &shell);
        let d = app.drawer.unwrap();
        app.run(&Command { action: "pane.taller", arg: None });
        let share = app.layout.below_share(d.pane).unwrap();
        assert!(share > crate::quick::DRAWER_SHARE);
        let saved = serde_json::json!({
            "v": 1, "layout": app.layout, "focus": d.pane, "zoom": false, "sidebar": true,
            "sidebar_width": 30, "drawer": app.drawer, "drawer_share": share,
            "panes": app.panes.iter().map(|(k, p)| (k.to_string(), p.session.map(|s| s.full()))).collect::<HashMap<_, _>>(),
        });
        app.apply_view(Some(saved.clone()));
        assert_eq!(app.drawer, Some(d));
        assert_eq!(app.drawer_session(), Some(shell.id));
        // Hidden, it remembers how tall it was.
        here(&mut app);
        assert!((app.drawer_share - share).abs() < 1e-6);
        // After a daemon restart the shell is gone: the drawer closes instead of showing another.
        app.sessions.retain(|s| s.id != shell.id);
        app.apply_view(Some(saved));
        assert!(app.drawer.is_none());
        assert_eq!(app.layout.panes().len(), 1);
    }

    // ── the scratchpad (NOTE1) ───────────────────────────────────────────

    fn pad_session(name: &str) -> SessionInfo {
        let mut s = session(name);
        s.pad = true;
        s.argv = vec!["nvim".into(), "/data/scratchpad.md".into()];
        s
    }

    fn pad_doc(text: &str) -> PadDoc {
        PadDoc::of("/data/scratchpad.md".into(), text.into())
    }

    fn toggle_pad(app: &mut App) {
        app.run(&Command { action: "scratchpad.toggle", arg: None });
    }

    fn pad_shown(app: &mut App, mode: &str) -> SessionInfo {
        app.config = Config::parse(&format!("[scratchpad]\nmode = \"{mode}\"\n")).unwrap();
        let pad = pad_session("scratchpad");
        toggle_pad(app);
        app.on_reply(AppMsg::Reply(Pending::Pad, Ok(Reply::Session { info: pad.clone() })));
        pad
    }

    #[test]
    fn the_scratchpad_key_shows_focuses_and_hides_one_editor_in_a_drawer() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        let host = app.focused;
        assert_eq!(app.keymap.key_for("scratchpad.toggle").as_deref(), Some("C-q ;"));
        let pad = pad_shown(&mut app, "drawer");
        let d = app.drawer.expect("a drawer");
        assert_eq!((d.host, app.focused), (host, d.pane));
        assert_eq!(shows(&app, d.pane), Some(pad.name.clone()));
        assert!(screen(&app).contains("✎"), "its header says what it is");
        toggle_pad(&mut app);
        assert!(app.drawer.is_none(), "focused: hides it");
        assert_eq!(app.focused, host);
        toggle_pad(&mut app);
        assert_eq!(app.drawer_session(), Some(pad.id), "the editor was kept, so no new request");
        // Unfocused, the key focuses it.
        app.focused = host;
        toggle_pad(&mut app);
        assert_eq!(app.focused, app.drawer.unwrap().pane);
        // Shown in the drawer, it is in no list of sessions.
        assert!(!app.ordered().contains(&pad.id) && app.ordered().contains(&agent));
        assert!(!app.rows().iter().any(|r| r.session() == Some(pad.id)));
        assert_eq!(app.counts().0, 2, "the editor isn't a live session of yours");
        assert!(app.attention_order(true).is_empty());
        // The editor exits: the drawer closes.
        app.on_event(DomainEvent::SessionRemoved { id: pad.id });
        assert!(app.drawer.is_none());
    }

    #[test]
    fn the_quick_terminal_and_the_scratchpad_share_the_one_drawer() {
        let mut app = app();
        auto_shell(&mut app);
        with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        app.run(&Command { action: "shell.here", arg: None });
        app.on_reply(AppMsg::Reply(
            Pending::Quick(app.config.ui.shell_style),
            Ok(Reply::Session { info: shell.clone() }),
        ));
        assert_eq!(app.drawer_session(), Some(shell.id));
        let pad = pad_shown(&mut app, "drawer");
        assert_eq!(app.drawer_session(), Some(pad.id), "the pad replaced the shell");
        assert_eq!(app.layout.panes().len(), 2, "the agent's pane and one drawer, not two");
    }

    #[test]
    fn the_popup_takes_the_keyboard_but_not_the_prefix_and_draws_over_everything() {
        let mut app = app();
        with_agents(&mut app);
        let pad = pad_shown(&mut app, "popup");
        assert_eq!(app.pad_popup, Some(pad.id));
        let shown = screen(&app);
        assert!(shown.contains("✎ Scratchpad · "), "{shown}");
        assert!(
            shown.contains("scratchpad.md") || shown.contains("…"),
            "the file is named: {shown}"
        );
        // Keys go to the editor: a letter isn't a sidebar key, and nothing opens.
        let before = app.focused;
        app.on_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.pad_popup.is_some() && app.overlay.is_none() && app.focused == before);
        // Prefix, then the key: closes it.
        let ctrl_q = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        app.on_key(&ctrl_q);
        assert!(app.prefix);
        app.on_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(';'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(app.pad_popup.is_none());
        assert!(!screen(&app).contains("✎ Scratchpad · "));
        // It sizes the editor to the window, not to a pane.
        toggle_pad(&mut app);
        let r = app.inner_popup();
        assert!(r.width < app.area.width && r.height < app.area.height && r.width > 40);
    }

    #[test]
    fn the_pane_mode_splits_beside_and_the_key_closes_that_pane() {
        let mut app = app();
        with_agents(&mut app);
        let panes = app.layout.panes().len();
        let pad = pad_shown(&mut app, "pane");
        assert_eq!(app.layout.panes().len(), panes + 1);
        assert_eq!(app.focused_session(), Some(pad.id));
        assert!(app.drawer.is_none() && app.pad_popup.is_none());
        let p = app.focused;
        let other = app.layout.panes().into_iter().find(|x| *x != p).unwrap();
        app.focused = other;
        toggle_pad(&mut app);
        assert_eq!(app.focused, p, "shown elsewhere: focus it");
        toggle_pad(&mut app);
        assert_eq!(app.layout.panes().len(), panes, "focused: closes its pane");
        assert_eq!(app.pad_session(), Some(pad.id), "closing the pane doesn't stop the editor");
    }

    #[test]
    fn the_count_is_kept_from_events_and_shown_on_the_status_bar() {
        let mut app = app();
        app.config.ui.status_bar = true;
        assert!(!screen(&app).contains("✎"));
        app.on_event(DomainEvent::ScratchChanged { rev: "abc".into(), open: 7, by: None });
        assert_eq!(app.pad_open, 7);
        assert!(screen(&app).contains("✎ 7"), "{}", screen(&app));
        app.on_event(DomainEvent::ScratchChanged {
            rev: "abd".into(),
            open: 0,
            by: Some("file".into()),
        });
        assert!(!screen(&app).contains("✎"));
    }

    #[test]
    fn add_opens_a_one_line_box_and_note_starts_from_what_the_session_is_for() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        app.sessions.iter_mut().find(|s| s.id == agent).unwrap().task =
            Some("fix the flaky test\nin tests/api.rs".into());
        app.run(&Command { action: "scratchpad.add", arg: None });
        assert_eq!(prompt_text(&app), Some(""));
        assert!(screen(&app).contains("Add to the scratchpad"));
        typed(&mut app, "login times out");
        assert_eq!(prompt_text(&app), Some("login times out"));
        typed(&mut app, "\n");
        assert!(app.overlay.is_none(), "⏎ sends it");
        // The reply toasts the count.
        app.on_reply(AppMsg::Reply(
            Pending::PadAdded,
            Ok(Reply::Scratch { doc: pad_doc("- [ ] a\n- [ ] b\n") }),
        ));
        assert_eq!(app.pad_open, 2);
        assert_eq!(
            app.toast.as_ref().map(|t| t.text.as_str()),
            Some("added to the scratchpad (2 open)")
        );
        // Note this, in the pane: the focused session's task, first line.
        app.run(&Command { action: "scratchpad.note", arg: None });
        assert_eq!(prompt_text(&app), Some("fix the flaky test"));
        // It is a row-menu action for a session.
        assert!(app.row_actions(&Row::Session(agent)).contains(&"scratchpad.note"));
        // Esc cancels.
        key(&mut app, KeyCode::Esc, Mods::NONE);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn the_inbox_notes_an_entry_with_a_link_back() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        let name = app.session(agent).unwrap().name.clone();
        app.inbox.push(
            serde_json::from_value(serde_json::json!({
                "id": 1, "session": agent.full(), "session_name": name, "at_ms": 1,
                "kind": "blocked", "title": "[work] claude", "body": "wants to run rm -rf build",
                "workspace": "main",
            }))
            .unwrap(),
        );
        app.run(&Command { action: "inbox", arg: None });
        key(&mut app, KeyCode::Char { c: 'n' }, Mods::NONE);
        assert_eq!(
            prompt_text(&app),
            Some(format!("wants to run rm -rf build (ptyro://session/{name})").as_str())
        );
    }

    #[test]
    fn the_items_list_searches_ticks_and_starts_an_item_as_a_task() {
        let mut app = app();
        with_project(&mut app);
        let text = "# Pad\n\n## Bugs\n- [ ] 2026-10-24 14:02 · by `claude-rook` · login times out\n- [x] fixed one (#4)\n- [ ] flaky test in api\n  see tests/api.rs\n";
        app.run(&Command { action: "scratchpad.items", arg: None });
        app.on_reply(AppMsg::Reply(
            Pending::PadItems { refresh: false },
            Ok(Reply::Scratch { doc: pad_doc(text) }),
        ));
        assert_eq!(app.pad_open, 2);
        let shown = screen(&app);
        assert!(shown.contains("Scratchpad · 2 open · /data/scratchpad.md"), "{shown}");
        assert!(shown.contains("login times out") && shown.contains("by claude-rook"), "{shown}");
        assert!(!shown.contains("fixed one"), "ticked ones are hidden");
        // ^a shows them too; typing searches.
        key(&mut app, KeyCode::Char { c: 'a' }, Mods(Mods::CTRL));
        assert!(app.pad_show_done);
        app.on_reply(AppMsg::Reply(
            Pending::PadItems { refresh: true },
            Ok(Reply::Scratch { doc: pad_doc(text) }),
        ));
        assert!(screen(&app).contains("✓ fixed one"));
        typed(&mut app, "flaky");
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else {
            panic!("the list is open")
        };
        assert_eq!(palette.filtered.len(), 1);
        // A refresh keeps what was typed.
        app.on_event(DomainEvent::ScratchChanged { rev: "x".into(), open: 2, by: None });
        app.on_reply(AppMsg::Reply(
            Pending::PadItems { refresh: true },
            Ok(Reply::Scratch { doc: pad_doc(text) }),
        ));
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!() };
        assert_eq!((palette.query.as_str(), palette.filtered.len()), ("flaky", 1));
        // ⏎ closes the list (the task box would have opened a worktree request).
        key(&mut app, KeyCode::Enter, Mods::NONE);
        assert!(app.overlay.is_none());
        // What it starts: the item's words and following lines, tied to its line.
        let w = Wizard { project: Some("p1".into()), pad_item: Some(6), ..Wizard::default() };
        let spec = worktree_spec(
            &Wizard { prompt: Some("x".into()), agent: Some("claude".into()), ..w },
            "p1",
        );
        assert_eq!(spec.pad_item, Some(6));
        let doc = pad_doc(text);
        let item =
            ptyro_core::scratchpad::items(&doc.text).into_iter().find(|i| i.line == 6).unwrap();
        assert_eq!(
            ptyro_core::scratchpad::body(&doc.text, &item),
            "flaky test in api\nsee tests/api.rs"
        );
    }

    #[test]
    fn ctrl_e_in_the_list_opens_the_editor_and_history_asks_before_restoring() {
        let mut app = app();
        with_agents(&mut app);
        app.config = Config::parse("[scratchpad]\nmode = \"popup\"\n").unwrap();
        let pad = pad_session("scratchpad");
        app.sessions.push(pad.clone());
        app.show_pad_items(pad_doc("- [ ] one\n"), None);
        key(&mut app, KeyCode::Char { c: 'e' }, Mods(Mods::CTRL));
        assert!(app.overlay.is_none());
        assert_eq!(app.pad_popup, Some(pad.id), "the editor, which was already running");
        app.show_pad_history(vec![ptyro_core::scratchpad::Snapshot {
            rev: "aabbccddeeff".into(),
            at_ms: ptyro_core::now_ms(),
            bytes: 10,
            lines: 3,
        }]);
        assert!(screen(&app).contains("aabbccddeeff"));
        key(&mut app, KeyCode::Enter, Mods::NONE);
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("asks first") };
        assert!(text.contains("aabbccddeeff"), "{text}");
    }

    // ── full-sized panels, splits on request, quick-new, reveal (UX3) ────────

    /// Two agents of p1 side by side, the right one focused.
    fn split_two(app: &mut App) -> (SessionId, SessionId, PaneId) {
        let (a, b) = with_agents(app);
        app.on_reply(AppMsg::Reply(
            Pending::Spawned { split: Some(Dir::Right), spec: Box::default(), back: false },
            Ok(Reply::Session { info: app.session(b).unwrap().clone() }),
        ));
        let right = app.focused;
        assert_eq!(app.layout.panes().len(), 2);
        (a, b, right)
    }

    fn spawned(app: &mut App, info: &SessionInfo, back: bool) {
        app.sessions.push(info.clone());
        app.on_reply(AppMsg::Reply(
            Pending::Spawned { split: None, spec: Box::default(), back },
            Ok(Reply::Session { info: info.clone() }),
        ));
    }

    #[test]
    fn a_new_session_opens_full_sized_and_back_brings_the_split_whole() {
        let mut app = app();
        let (a, b, _) = split_two(&mut app);
        let fresh = placed("gemini", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        assert_eq!(app.layout.panes().len(), 1, "full sized, not in a split");
        assert_eq!(app.focused_session(), Some(fresh.id));
        assert!(app.kept.is_some(), "the split is kept");
        // ` swaps: the split is back as it was, and ` again shows the panel.
        act(&mut app, "session.last");
        assert_eq!(app.layout.panes().len(), 2);
        let shown: HashSet<SessionId> = app.panes.values().filter_map(|p| p.session).collect();
        assert_eq!(shown, HashSet::from([a, b]));
        act(&mut app, "session.last");
        assert_eq!(app.focused_session(), Some(fresh.id));
        // Picking a session of the kept split brings the split back, focused on it.
        app.show(a, None);
        assert_eq!(app.layout.panes().len(), 2);
        assert_eq!(app.focused_session(), Some(a));
        // Picking one already on screen focuses it where it is: nothing moves.
        app.show(b, None);
        assert_eq!(app.layout.panes().len(), 2);
        assert_eq!(app.focused_session(), Some(b));
    }

    #[test]
    fn in_one_pane_a_new_session_takes_it_and_back_is_the_previous() {
        let mut app = app();
        let (a, _) = with_agents(&mut app);
        let fresh = placed("gemini", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        assert_eq!(app.layout.panes().len(), 1);
        assert!(app.kept.is_none());
        act(&mut app, "session.last");
        assert_eq!(app.focused_session(), Some(a));
    }

    #[test]
    fn the_kept_split_comes_back_with_the_view_and_forgets_gone_sessions() {
        let mut app = app();
        let (a, b, _) = split_two(&mut app);
        let fresh = placed("gemini", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        let saved = serde_json::to_value(SavedView {
            v: 1,
            layout: app.layout.clone(),
            panes: app.panes.iter().map(|(k, p)| (*k, p.session.map(|s| s.full()))).collect(),
            focus: app.focused,
            zoom: false,
            sidebar: true,
            sidebar_width: 30,
            drawer: None,
            drawer_share: crate::quick::DRAWER_SHARE,
            kept: app.kept.as_deref().cloned(),
        })
        .unwrap();
        app.apply_view(Some(saved));
        assert!(app.kept.as_ref().is_some_and(|k| k.shows(a).is_some() && k.shows(b).is_some()));
        // `b` goes: the kept split loses its pane; back shows `a` alone.
        app.on_event(DomainEvent::SessionRemoved { id: b });
        act(&mut app, "session.last");
        assert_eq!(app.layout.panes().len(), 1);
        assert_eq!(app.focused_session(), Some(a));
    }

    #[test]
    fn a_split_starts_a_clone_of_what_it_splits() {
        let mut app = app();
        let (a, _) = with_agents(&mut app);
        let s = app.sessions.iter_mut().find(|s| s.id == a).unwrap();
        (s.kind, s.agent, s.profile) = (SessionKind::Agent, Some("claude".into()), "work".into());
        let s = s.clone();
        let mut sent = None;
        while app.rx.try_recv().is_ok() {}
        act(&mut app, "pane.split_right");
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::Spawned { split, spec, .. }, _) = m {
                sent = Some((split, spec));
            }
        }
        let (split, spec) = sent.expect("a spawn went out");
        assert_eq!(split, Some(Dir::Right));
        assert_eq!(spec.agent, s.agent, "the same agent");
        assert_eq!(spec.profile.as_deref(), Some(s.profile.as_str()));
        assert_eq!(spec.worktree, s.worktree, "in the same worktree");
        assert!(spec.resume.is_none() && spec.prompt.is_none(), "a fresh conversation");
        assert!(app.overlay.is_none(), "no picker");
    }

    #[test]
    fn an_editor_opens_full_sized_and_gives_the_screen_back_when_it_exits() {
        let mut app = app();
        let (a, b, _) = split_two(&mut app);
        let mut editor = placed("vim", Some("p1"), Some("p1-root"));
        editor.kind = SessionKind::Command;
        spawned(&mut app, &editor, true);
        assert_eq!(app.layout.panes().len(), 1);
        editor.state = ProcessState::Exited { code: Some(0), signal: None };
        app.on_event(DomainEvent::SessionExited { info: editor });
        assert_eq!(app.layout.panes().len(), 2, "the split is back");
        let shown: HashSet<SessionId> = app.panes.values().filter_map(|p| p.session).collect();
        assert_eq!(shown, HashSet::from([a, b]));
    }

    #[test]
    fn a_new_session_in_a_folded_worktree_unfolds_it_and_is_selected() {
        let mut app = app();
        with_project(&mut app);
        app.collapsed.insert("p1".into());
        app.collapsed.insert("p1-root".into());
        let fresh = placed("claude", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        assert!(app.rows().contains(&Row::Session(fresh.id)), "its row shows");
        assert_eq!(app.selected_row(), Some(Row::Session(fresh.id)));
    }

    #[test]
    fn a_row_the_tree_cannot_place_yet_is_selected_once_its_project_arrives() {
        let mut app = app();
        with_project(&mut app);
        let fresh = placed("claude", Some("p2"), Some("p2-root"));
        spawned(&mut app, &fresh, false);
        // p2 isn't known yet: it waits under "other sessions", selected.
        assert_eq!(app.selected_row(), Some(Row::Session(fresh.id)));
        app.on_server(ServerMsg::Event {
            event: DomainEvent::ProjectUpdated { info: project("p2", "two") },
        });
        app.on_server(ServerMsg::Event {
            event: DomainEvent::WorktreeUpdated { info: root("p2") },
        });
        let mut ws = app.workspaces[0].clone();
        ws.projects.push("p2".into());
        app.on_server(ServerMsg::Event { event: DomainEvent::WorkspaceUpdated { info: ws } });
        // Under its project now, and still the selected row.
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Session(fresh.id)).unwrap();
        assert!(rows[..at].contains(&Row::Worktree("p2-root".into())));
        assert_eq!(app.selected_row(), Some(Row::Session(fresh.id)));
    }

    #[test]
    fn a_new_workspace_is_known_as_soon_as_it_is_made() {
        let mut app = app();
        with_project(&mut app);
        let info: WorkspaceInfo = serde_json::from_value(serde_json::json!({
            "id": "w2", "name": "fresh", "profile": "default", "notify": "all",
            "projects": [], "created_ms": 0,
        }))
        .unwrap();
        app.on_reply(AppMsg::Reply(
            Pending::WorkspaceMade("fresh".into()),
            Ok(Reply::Workspace { info }),
        ));
        assert!(app.workspaces.iter().any(|w| w.name == "fresh"));
        assert_eq!(app.workspace, "fresh");
    }

    #[test]
    fn bang_starts_another_shell_in_this_worktree_and_c_n_starts_again() {
        let mut app = app();
        with_agents(&mut app);
        let spawns = |app: &mut App| {
            let mut v = Vec::new();
            while let Ok(m) = app.rx.try_recv() {
                if let AppMsg::Reply(Pending::Spawned { split, spec, .. }, _) = m {
                    v.push((split, spec));
                }
            }
            v
        };
        spawns(&mut app);
        act(&mut app, "shell.new");
        act(&mut app, "shell.new");
        let v = spawns(&mut app);
        assert_eq!(v.len(), 2, "each press starts one");
        for (split, spec) in v {
            assert_eq!(split, None, "full sized");
            assert_eq!(spec.worktree.as_deref(), Some("p1-root"));
            assert!(spec.agent.is_none() && spec.argv.is_empty() && !spec.quick, "a plain shell");
        }
        act(&mut app, "session.again");
        let v = spawns(&mut app);
        assert!(v[0].1.again && v[0].1.worktree.as_deref() == Some("p1-root"));
        // Its label says what it starts.
        assert_eq!(app.again_label(), "claude");
        app.again = Some(ptyro_core::session::LastLaunch::default());
        assert_eq!(app.again_label(), "shell");
        for a in ["shell.new", "session.again"] {
            assert!(app.keymap.key_for(a).is_some(), "{a} has a key");
        }
    }

    // ── many machines (MM1.B) ─────────────────────────────────────────────

    fn machine(
        name: &str,
        state: ptyro_core::model::MachineState,
    ) -> ptyro_core::model::MachineInfo {
        ptyro_core::model::MachineInfo {
            name: name.into(),
            transport: ptyro_core::model::MachineTransport::Ssh { target: name.into() },
            state,
            since_ms: Some(0),
            version: Some("0.0.13".into()),
            calls_itself: Some(name.into()),
            workspaces: vec!["work".into()],
            hidden: vec![],
            error: Some(format!("{name} went away")),
            fix: Some("it reconnects by itself when it is back".into()),
            added_ms: 0,
        }
    }

    /// Our project p1 in `main`, and laptop2's workspace `work` with one session.
    fn with_laptop2(app: &mut App, state: ptyro_core::model::MachineState) -> SessionInfo {
        with_project(app);
        let mut w: WorkspaceInfo = serde_json::from_value(serde_json::json!({
            "id": "rw", "name": "laptop2:work", "profile": "laptop2:default", "notify": "all",
            "projects": [], "created_ms": 0,
        }))
        .unwrap();
        w.machine = Some("laptop2".into());
        app.workspaces.push(w);
        app.machines.push(machine("laptop2", state));
        let mut s = session("laptop2:fix-flaky-test");
        s.machine = Some("laptop2".into());
        s.workspace = Some("laptop2:work".into());
        app.sessions.push(s.clone());
        s
    }

    #[test]
    fn every_workspace_is_in_the_sidebar_and_the_selected_one_is_where_you_are() {
        let mut app = app();
        let remote = with_laptop2(&mut app, ptyro_core::model::MachineState::Online);
        let rows = app.rows();
        assert!(rows.contains(&Row::Workspace("main".into())));
        let at = rows.iter().position(|r| *r == Row::Workspace("laptop2:work".into())).unwrap();
        assert_eq!(rows[at + 1], Row::Session(remote.id), "its session under its header");
        let shown = screen(&app);
        assert!(shown.contains("laptop2 / work"), "{shown}");
        assert!(!shown.contains("laptop2:fix-flaky-test"), "no machine twice in its rows: {shown}");
        // Selecting there makes it the workspace you're in: quick new and `n` go there.
        app.focus = Focus::Sidebar;
        app.set_sel(&rows, at + 1);
        assert_eq!(app.workspace, "laptop2:work");
        assert!(screen(&app).lines().next().unwrap().contains("laptop2 / work"));
        // Showing a session of ours brings you back.
        let ours = placed("mine", Some("p1"), Some("p1-root"));
        app.sessions.push(ours.clone());
        app.focus = Focus::Pane;
        app.show(ours.id, None);
        assert_eq!(app.workspace, "main");
        // `w`, then a workspace: its header, unfolded and selected; the panes stay.
        app.collapsed.insert(crate::tree::fold_key("laptop2:work"));
        app.picked(Target::Workspace("laptop2:work".into()), Purpose::Workspace);
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(app.selected_row(), Some(Row::Workspace("laptop2:work".into())));
        assert!(app.rows().contains(&Row::Session(remote.id)));
        assert_eq!(app.focused_session(), Some(ours.id));
    }

    #[test]
    fn an_offline_machine_says_so_where_its_things_are_and_offers_the_fix() {
        let mut app = app();
        let remote = with_laptop2(&mut app, ptyro_core::model::MachineState::Offline);
        let shown = screen(&app);
        assert!(shown.contains("laptop2 / work offline"), "{shown}");
        assert!(
            !shown.contains("[laptop2:"),
            "a machine's own default profile has no badge: {shown}"
        );
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Workspace("laptop2:work".into())).unwrap();
        app.focus = Focus::Sidebar;
        app.set_sel(&rows, at);
        assert_eq!(app.context_hint().as_deref(), Some("it reconnects by itself when it is back"));
        let menu = app.row_actions(&Row::Workspace("laptop2:work".into()));
        for a in ["machine.retry", "machine.hide", "machine.remove", "session.new"] {
            assert!(menu.contains(&a), "{a}: {menu:?}");
            assert!(crate::keymap::spec(a).is_some(), "{a} is an action");
        }
        // Its session's pane says it too.
        app.focus = Focus::Pane;
        app.show(remote.id, None);
        let shown = screen(&app);
        assert!(shown.contains("laptop2 offline since"), "{shown}");
        // News of it coming back is said once.
        app.machine_news(machine("laptop2", ptyro_core::model::MachineState::Online));
        assert!(app.toast.as_ref().is_some_and(|t| t.text.contains("laptop2 is back")));
    }

    #[test]
    fn a_new_workspace_asks_which_machine_and_is_made_there() {
        let mut app = app();
        with_laptop2(&mut app, ptyro_core::model::MachineState::Online);
        app.picked(Target::NewWorkspace, Purpose::Workspace);
        let Some(Overlay::Picker { palette, purpose: Purpose::NewWorkspaceOn }) = &app.overlay
        else {
            panic!("asks where")
        };
        assert_eq!(palette.item(1).label, "laptop2");
        app.picked(Target::Machine("laptop2".into()), Purpose::NewWorkspaceOn);
        let Some(Overlay::Prompt { title, kind, .. }) = &app.overlay else { panic!() };
        assert!(title.contains("on laptop2"), "{title}");
        assert!(matches!(kind, PromptKind::NewWorkspace(None, Some(m)) if m == "laptop2"));
        // Taken there already: said as it is typed.
        let kind = PromptKind::NewWorkspace(None, Some("laptop2".into()));
        assert!(app.prompt_check(&kind, "work").is_err());
        assert!(app.prompt_check(&kind, "spikes").is_ok());
        while app.rx.try_recv().is_ok() {}
        app.overlay = None;
        app.prompt_done("spikes".into(), kind);
        let mut asked = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::WorkspaceMade(n), _) = m {
                asked = Some(n);
            }
        }
        assert_eq!(
            asked.as_deref(),
            Some("laptop2:spikes"),
            "the name carries the machine: made there"
        );
    }
}
