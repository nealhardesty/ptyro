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
use ptyro_core::scope::{Scope, Tab};
use ptyro_core::screen::ScrollbackDoc;
use ptyro_core::session::{ProcessState, SessionInfo, SessionKind, Show, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind, Usage};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
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
    /// Which sessions the sidebar lists.
    #[serde(default)]
    pub show: Show,
    /// The tab over the sidebar (SPEC §11.2k); `None` is `all`. One that is gone by the time
    /// it is loaded is `all`.
    #[serde(default)]
    pub scope: Option<ptyro_core::scope::Scope>,
    /// The quick terminal's drawer (UX1.25), and its share of its host's height.
    #[serde(default)]
    pub drawer: Option<Drawer>,
    #[serde(default = "drawer_share")]
    pub drawer_share: f32,
    /// The split layout a full-sized panel replaced (UX3.1).
    #[serde(default)]
    pub kept: Option<crate::screens::Screen>,
}

/// What a tab is called on the strip: `all`, or its label (`laptop2`, `[work]`).
pub(crate) fn scope_label(tabs: &[Tab], scope: Option<&Scope>) -> String {
    match scope {
        None => "all".into(),
        Some(s) => tabs
            .iter()
            .find(|t| t.scope == *s)
            .map_or_else(|| s.profile_name().to_owned(), |t| t.label.clone()),
    }
}

fn drawer_share() -> f32 {
    crate::quick::DRAWER_SHARE
}

/// What the tab keys say while there is no strip (SPEC §11.2k).
const NO_TABS: &str = "no tabs: every session here is one machine's and one profile's";

/// How long after the wheel moved a tab its next turn waits.
const TAB_WHEEL_GAP: Duration = Duration::from_millis(150);

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
    /// A session's last screen and what it wants, with an inline reply (SPEC §11.3 `p`).
    Peek {
        session: SessionId,
        screen: String,
        reply: String,
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
    /// Lifecycle scripts shown in full before trusting them (SPEC §10.5).
    Trust {
        scripts: ScriptsInfo,
        then: AfterTrust,
        scroll: usize,
    },
    /// Every configuration option, edited in place (CFG1.1).
    Settings(Box<crate::settings::Panel>),
    /// Something being made, every option of it on one screen (SPEC §11.5a).
    Form(Box<crate::form::Form>),
    /// About (UX2.1): `ptyro about --json` (`None` while it runs), the picked row of
    /// `about::picks`, and how far it is scrolled.
    About {
        doc: Option<serde_json::Value>,
        sel: usize,
        scroll: usize,
    },
}

/// Where a new session or worktree goes, gathered by the wizard (SPEC §11.5, M3.13).
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Where the work comes from (UX1.10): a PR, an issue, or an existing branch.
    pub pr: Option<u32>,
    pub issue: Option<String>,
    pub branch: Option<String>,
    /// The rest of `ptyro new`'s options, from the form (SPEC §11.5a): the profile (`None`
    /// resolves), a new worktree's base, whether it skips setup or fetching, a command line for
    /// `@command`, and a title apart from the prompt.
    pub profile: Option<String>,
    pub base: Option<String>,
    pub no_setup: bool,
    pub no_fetch: bool,
    pub command: Option<String>,
    pub title: Option<String>,
    /// Private (SPEC §25.7a), as the form says it; `None` (quick new): as `kb.private` says.
    pub private: Option<bool>,
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
    /// What goes in a new split (`v`, `s`): a clone (⏎), a new shell, or a session.
    Split(Dir),
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
    /// What to watch a SHELLS shell for (SPEC §23.9).
    ShellWatch(SessionId),
    /// A chat's agent (`None`: the new chat being written's).
    ChatAgent(Option<String>),
    /// The project this chat's task works in (SPEC §22.7).
    ChatTaskProject(String, crate::chats::TaskArgs),
    /// A citation to open.
    ChatCite,
    /// A slash command (`/help`), into the composer.
    ChatSlash,
    /// Home (UX1.19): what to do next.
    Home,
    /// What the doctor found, to fix (UX1.20).
    Health,
    /// A word of a `ptyro …` command being put together (UX1.21).
    CliPick(Box<crate::cli_actions::CliJob>),
    /// One of the Library's filters (SPEC §25.10).
    Library(crate::library::Filter),
    /// The folder to read into the knowledge base (`kb.ingest`): then its form.
    KbIngest,
    /// The profile to pin this project to.
    Pin(String),
    /// This workspace's profile, or how it notifies.
    WorkspaceProfile,
    WorkspaceNotify,
    /// A stale worktree to clean up: ⏎ removes it, `C-k` keeps it from clean-up.
    Gc,
    /// The existing branch a new worktree checks out (UX1.10).
    Branch(Wizard),
}

/// What asked for a project when there was none (UX1.6): done once the project is added.
#[derive(Debug, Clone)]
pub enum AfterAdd {
    /// Re-run this action, in the new project's main checkout.
    Run(&'static str),
    /// The wizard's second step, in the new project.
    Targets(Box<Wizard>),
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    /// A command line; empty runs your shell.
    Command(Wizard),
    Rename(SessionId),
    /// One prompt for these sessions (`b`).
    Broadcast(Vec<SessionId>),
    /// A session's title (UX1.24): free text; empty clears it.
    Retitle(SessionId),
    /// A worktree's title (by id).
    RetitleWorktree(String),
    /// A PR or issue to start a worktree from (UX1.10).
    WorkSource(Wizard),
    /// A word for a `ptyro …` command being put together (UX1.21); `true`: `k=v` variables.
    CliAsk(Box<crate::cli_actions::CliJob>),
    /// A workspace's new name.
    RenameWorkspace(String),
    /// A worktree's new name (by id).
    RenameWorktree(String),
    /// A project's new slug (by id).
    RenameProject(String),
    /// A chat's title (by id); empty clears it (`chat.autotitle` makes a new one).
    ChatRetitle(String),
    /// One more folder this chat (`None`: the new chat being written) may read (`none`: none).
    ChatFolder(Option<String>),
    /// A message said differently: (chat, the message's node).
    ChatEdit(String, String),
    /// Find in this chat.
    ChatFindIn(String),
    /// A SHELLS shell's new number (SPEC §23.3).
    ShellMove(SessionId),
    /// The window a SHELLS shell joins as a pane (SPEC §23.17).
    ShellJoin(SessionId),
    /// The pattern a SHELLS shell is watched for (SPEC §23.9).
    ShellMatch(SessionId),
    /// A question for `chat find`.
    ChatFind,
    /// An address to open in the Library (`kb.show`), or to forget (`kb.forget`).
    KbShow,
    KbForget,
    /// Find in the Library's reader.
    KbFind,
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

/// A name as its own machine knows it: `laptop2:work` → `work` for a thing mirrored from
/// `laptop2` (MM1.B). A rename types and checks this, and the daemon renames it there.
pub(crate) fn bare<'a>(machine: Option<&str>, name: &'a str) -> &'a str {
    machine.and_then(|m| name.strip_prefix(m)?.strip_prefix(':')).unwrap_or(name)
}

/// `bare`'s inverse: `work` on `laptop2` is `laptop2:work` here.
fn on_machine(machine: Option<&str>, name: &str) -> String {
    machine.map_or_else(|| name.to_owned(), |m| format!("{m}:{name}"))
}

impl PromptKind {
    /// Free text that may run over several lines (UX1.26): a prompt for agents. Names, titles and
    /// command lines stay on one line.
    pub fn multiline(&self) -> bool {
        matches!(self, Self::Broadcast(_))
    }

    /// The text is a name (`[a-z0-9._-]`): bad characters are marked and Tab tidies it.
    pub fn is_name(&self) -> bool {
        matches!(
            self,
            Self::Rename(_)
                | Self::RenameWorkspace(_)
                | Self::RenameWorktree(_)
                | Self::RenameProject(_)
        )
    }

    /// Whether a name here may hold `c`: a project's slug is the narrower `[a-z0-9-]`.
    pub fn allowed(&self, c: char) -> bool {
        match self {
            Self::RenameProject(_) => ptyro_core::names::slug_allowed(c),
            _ => ptyro_core::names::allowed(c),
        }
    }

    /// The name Tab makes of `text`, when that fixes it.
    pub fn tidy(&self, text: &str) -> Option<String> {
        match self {
            Self::RenameProject(_) => ptyro_core::names::slug_tidy(text),
            _ => ptyro_core::names::tidy(text),
        }
    }

    /// What a name field says under itself.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::RenameProject(_) => ptyro_core::names::SLUG_HINT,
            _ => ptyro_core::names::TYPED_HINT,
        }
    }

    /// `text` as this prompt keeps it: a name or branch swaps what it can't hold for `-`.
    pub fn fit(&self, text: &str) -> String {
        match self {
            Self::RenameProject(_) => ptyro_core::names::slug_typed(text),
            _ if self.is_name() => ptyro_core::names::typed(text),
            _ => text.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Confirmed {
    Stop(SessionId),
    /// SIGKILL, now.
    Kill(SessionId),
    /// Stop it, then forget it (`rm --stop`).
    StopRemove(SessionId),
    /// Forget an ended session (`rm`); a dormant one was asked about first.
    Remove(SessionId),
    /// Resend with the guard / pin override acknowledged.
    Spawn(Box<SpawnSpec>, Option<Dir>),
    Worktree(Box<WorktreeSpec>, Option<Dir>),
    RemoveWorktree {
        id: String,
        force: bool,
    },
    /// "Pinned to X — keep it?" after adding a project; `n` unpins.
    KeepPin(String),
    /// Add a line to a file: a health fix (UX1.20).
    Append(std::path::PathBuf, String),
    /// Send this request, then show this.
    Send(Box<Request>, String),
    /// Forget this project (running sessions in it refuse, with a way through).
    Forget(String),
    /// Quit ptyro: stop every session and the daemon (`daemon stop --force`).
    Quit,
    /// Restart the daemon, stopping what runs (`daemon restart --force`).
    RestartDaemon,
    /// Switch meaning search on (SPEC §22.10, §25.13): `kb.meaning = true` in the config.
    MeaningOn,
    /// Send this request again, agreed to (`confirmed`), its reply going to this.
    Again(Box<Request>, Box<Pending>),
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
    /// The daemon's editors, for "open in" from a client on another computer (WIN3): its
    /// worktree paths are its own, so the daemon opens them (the worktree's id).
    RemoteEditors(String),
    /// A worktree opened by the daemon in this editor: a session to show, or what it said.
    EditorOpened(String),
    /// A workspace was renamed: the TUI follows it once the daemon has agreed.
    WorkspaceRenamed(String),
    /// A workspace was made: switch to it once the daemon has agreed.
    WorkspaceMade(String),
    /// A quick terminal to show in the drawer (UX1.25).
    Quick(ptyro_core::config::ShellStyle),
    List,
    Registry,
    Saved,
    /// Every chat not archived (SPEC §22.12).
    Chats,
    /// The archived ones, for the chat list.
    ArchivedChats,
    /// A new chat made by its first message (`text`), from the view with this token: follow it.
    ChatMade {
        text: String,
        token: u64,
    },
    /// A request the daemon may ask the person to agree to first (`NeedsConfirmation`: a chat's
    /// conversation going to another profile, SPEC §22.7); its reply is the inner one's.
    Confirm(Box<Request>, Box<Pending>),
    /// A message (`text`) sent to `chat`: put back if it was refused.
    ChatSent {
        chat: String,
        text: String,
    },
    /// A chat made (a find): open it, if what was shown when it was asked
    /// for (`App::chat_shown_key`) still is; else offer it.
    ChatOpen(String),
    /// A cited chat looked up by its address: open it at this node (as `ChatOpen`).
    ChatOpenAt(Option<String>, String),
    /// The chat list's preview of this chat.
    ChatPreview(String),
    /// The chat list's message hits, for this query.
    ChatHits(String),
    /// The Library's (SPEC §25.10): its list (the ask's number), the selected row's text (its
    /// address), an item for the reader (at this piece), the item whose session `r` goes to,
    /// how the knowledge base stands, and what a forgetting deleted.
    KbRows(u64),
    KbPreview(String),
    KbReader(Option<u32>),
    /// The reader's item again, after news of a change.
    KbReaderAgain,
    KbResume,
    KbStatus,
    KbForgotten,
    /// `kb.sync`: the notes folder was read again.
    KbSynced,
    /// The next page of the Library's history (the ask's number).
    KbOlder(u64),
    /// The Sources tab's (SPEC §25.10): the repositories found, for the ingest form's path
    /// picker; a root added (on this machine); a root being removed; the agents' history
    /// counted (and the request that reads it, unless it was only to count); it being read
    /// (what to say).
    KbPaths,
    KbIngested(Option<String>),
    KbRemoved,
    KbCounted(Option<Box<Request>>),
    KbBackfilled(String),
    /// A chat's Markdown, to write here.
    ChatExport(std::path::PathBuf),
    /// What quick-new would start in the context's worktree (UX3.2), for its label.
    Again(Option<String>),
    /// The daemon's agents out of date (BOOT5); `note`: the start-up note's too.
    Updates {
        note: bool,
    },
    /// Wizard title: the profile a spawn would get and why.
    Resolved,
    /// Show the trust dialog for these scripts, then do `then`.
    TrustDialog(AfterTrust),
    Trusted(AfterTrust),
    Removed(String),
    /// A project forgotten (`ProjectRemove`): running sessions in it are offered a way out.
    Forget(String),
    ProjectAdded(Option<AfterAdd>),
    /// A project moved to where its repository is now (SPEC §10.1): its slug.
    Moved(String),
    Discovered(Option<AfterAdd>),
    Scripts(String),
    /// Check the project's scripts are trusted before creating a worktree.
    PreCreate(Box<WorktreeSpec>, Option<Dir>),
    Peek(SessionId),
    Explain(SessionId),
    /// `app.quit`: the daemon is stopping.
    Quit,
    /// Older history for copy mode, from the recording (M8.2).
    DeepScrollback(PaneId, SessionId),
    /// The context worktree's scripts, for an open palette (UX1.18).
    PaletteScripts(String),
    /// Stale worktrees, for the clean-up picker.
    Gc,
    /// A worktree kept from clean-up: said, and the health badge looks again.
    GcKept,
    /// The project's branches, for the wizard (UX1.10).
    Branches(Box<Wizard>),
    /// The repositories for the first run, and for home (UX1.19).
    FirstRunRepos,
    Home,
    /// Home's view (`home.rs`).
    HomeView,
    /// The view a bare `ptyro` asks for as it starts: home opens on it if it has a row.
    HomeStart,
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
    /// A refusal with a way through, and the request that takes it (SPEC §6.6).
    Refused(Pending, Error, Box<Request>),
    /// A workspace's saved view, after switching to it.
    View(String, Result<Reply, Error>),
    /// What `ptyro doctor` found (UX1.20).
    Health(Vec<crate::health::Check>),
    /// What `ptyro about --json` said (UX2.1); `None` if it could not run.
    About(Option<serde_json::Value>),
    /// What this TUI's binary on disk answers to `--version`, after reaching a daemon of
    /// another version (`daemon`): an update may have replaced it (SPEC §24.8).
    Binary {
        on_disk: Option<String>,
        daemon: String,
    },
    /// What the setup checklist found (BOOT2); `None` if looking failed.
    SetupFound(Option<Box<SetupLook>>),
}

/// What [`crate::setup_list::look`] found.
pub type SetupLook = ptyro_agents::setup::Survey;

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
    /// Set a config key, as the settings panel would; toast `done`.
    Set { key: String, value: String, done: String },
}

/// An offer to run a TUI action.
pub(crate) fn run_offer(label: &str, action: &'static str) -> Offer {
    Offer { label: label.into(), then: Then::Run(Command { action, arg: None }) }
}

/// The move ptyro found for a project whose main checkout is gone (SPEC §10.1).
pub(crate) fn move_offer(id: &str, slug: &str, to: &std::path::Path) -> Offer {
    Offer {
        label: "move it there".into(),
        then: Then::Send(
            Box::new(Request::ProjectMove { addr: id.to_owned(), path: to.to_path_buf() }),
            format!("moved {slug} to {}", to.display()),
        ),
    }
}

/// An offer to run a session action on that session, whatever is selected when it's taken.
pub(crate) fn session_offer(label: &str, action: &'static str, id: SessionId) -> Offer {
    Offer { label: label.into(), then: Then::Run(Command { action, arg: Some(id.full()) }) }
}

/// The CLI commands error messages suggest, and the TUI action that does each (UX1.17): a
/// message that names one offers that action instead of leaving you to type it. Longest first.
const SUGGESTED: &[(&str, &str, &str)] = &[
    ("`ptyro project trust", "project.trust", "review and trust them"),
    ("`ptyro project add", "project.add", "add a project"),
    ("`ptyro status --explain", "session.explain", "see why"),
    ("`ptyro new --agent", "session.new", "start one"),
    ("`ptyro new", "session.new", "start one"),
    ("`ptyro events", "session.events", "see its events"),
    ("`ptyro config edit", "config.edit", "edit the config"),
];

/// What a message suggests doing, as a TUI action (see [`SUGGESTED`]). A missing agent or tool
/// (`` `ptyro install codex` installs it ``, `` `ptyro setup` installs one ``) opens the setup
/// checklist with it ticked (BOOT9).
pub fn offer_for(message: &str) -> Option<Offer> {
    if let Some(names) = ptyro_agents::locate::setup_offer(message) {
        let arg = Some(names.join(" ")).filter(|a| !a.is_empty());
        let label = if names.is_empty() { "set up agents" } else { "set it up" };
        return Some(Offer {
            label: format!("{label} (the checklist)"),
            then: Then::Run(Command { action: "app.setup", arg }),
        });
    }
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
    /// A profile being made in a split: what happens once it shows up (SPEC §11.5a).
    pub(crate) after_profile: Option<crate::form::AfterProfile>,
    /// Other machines whose workspaces this daemon mirrors (MM1.B), and how their links stand.
    pub machines: Vec<ptyro_core::model::MachineInfo>,
    /// The workspace on screen (SPEC §2.1).
    pub workspace: String,
    /// Collapsed project and worktree ids in the sidebar.
    pub collapsed: HashSet<String>,
    /// What was folded before `sidebar.fold_all` folded everything: the next one puts it back.
    pub folded_before: Option<HashSet<String>>,
    /// Sessions marked with Space in the sidebar, for `b` (broadcast).
    pub marked: Vec<SessionId>,
    /// Chats not archived (SPEC §22.12): the CHATS sections and the chat list.
    pub chat_list: Vec<ptyro_core::chats::ChatInfo>,
    /// The chat list or a chat, over the panes.
    pub chat_panel: Option<crate::chats::ChatPanel>,
    /// The chat (or list) a session took the place of, as it was: `session.last` brings it back.
    pub chat_aside: Option<crate::chats::ChatPanel>,
    /// Drafts of chats not shown, by chat id (`""`: the new chat's), for when they open again.
    pub chat_drafts: HashMap<String, String>,
    /// Show the agent that starts in this worktree as soon as it does (after setup).
    pub follow: Option<(String, PaneId)>,
    /// The wizard's profile line ("work — pinned by project").
    pub resolved: Option<String>,
    /// Usage per (profile, agent) (M4.17).
    pub usage: HashMap<(String, String), Usage>,
    /// The short name of the machine the daemon runs on (a remote daemon's, not ours).
    pub machine: Option<String>,
    /// Its computer's load average (`[ui] load`), as the daemon last said.
    pub host_load: Option<ptyro_protocol::HostLoad>,
    /// Whether our outer terminal window has focus (a watched session raises no attention).
    pub terminal_focused: bool,
    pub caches: HashMap<SessionId, ScreenCache>,
    subscribed: HashSet<SessionId>,
    pub(crate) sizes: HashMap<SessionId, TermSize>,
    pub layout: Layout,
    pub panes: HashMap<PaneId, PaneState>,
    pub focused: PaneId,
    pub zoom: bool,
    pub focus: Focus,
    pub sidebar: bool,
    pub sidebar_width: u16,
    /// The sidebar fills the screen with every row open: everything at once (UX1.2).
    pub sidebar_zoom: bool,
    /// Which sessions the sidebar lists: all, the running ones, or the active ones.
    pub show: Show,
    /// The tab picked over the sidebar: one machine and one profile, `None` for `all` (SPEC
    /// §11.2k). Kept (and saved) while it isn't a tab, a linked machine's mirror still empty
    /// after a restart: [`App::scope_shown`] is the one in effect, `all` meanwhile.
    pub scope: Option<ptyro_core::scope::Scope>,
    /// The tabs worked out once for the frame being drawn (`ui::draw`), so the strip, the tree
    /// and the hint don't each work them out again.
    pub(crate) tabs_frame: std::cell::RefCell<Option<Vec<Tab>>>,
    /// When the wheel over the strip last moved a tab: a trackpad's flick sends many at once.
    tab_wheel_at: Option<Instant>,
    /// `opts.workspace` has moved the tab once (at attach); a reload later leaves the tab alone.
    opened_on_workspace: bool,
    /// The quick terminal's drawer, when open, and the share of its host it takes (UX1.25).
    pub drawer: Option<Drawer>,
    pub drawer_share: f32,
    /// The split layout a full-sized panel replaced, kept whole for `` ` `` (UX3.1).
    pub kept: Option<Box<crate::screens::Screen>>,
    /// A session whose row is to be selected once the tree can place it (UX3.3), since when,
    /// and whether it may still switch tab (a tab picked meanwhile is yours, SPEC §11.2k).
    pub(crate) revealing: Option<(SessionId, Instant, bool)>,
    /// Tools (an editor) whose screen goes back to what it replaced when they exit (UX3.1).
    pub(crate) tool_back: HashSet<SessionId>,
    /// What quick new (`session.again`) starts, and in which worktree (UX3.7); `None`: not
    /// known yet (the default agent).
    pub again: Option<(Option<String>, ptyro_core::session::LastLaunch)>,
    /// The worktree quick new's label was last asked about.
    pub(crate) again_for: Option<Option<String>>,
    /// The selected sidebar row, and its index when last set (the fallback once it's gone).
    sidebar_row: Option<Row>,
    sidebar_sel: usize,
    pub prefix: bool,
    /// When the prefix was pressed (the which-key menu follows, UX1.14).
    pub prefix_at: Option<Instant>,
    /// The prefix key last pressed, for `prefix.send` (`C-Space q` sends `C-Space`).
    prefix_key: Option<KeyEvent>,
    pub overlay: Option<Overlay>,
    pub toast: Option<Toast>,
    pub buffer: Option<String>,
    pub controller: HashMap<SessionId, bool>,
    pub area: Rect,
    pub dirty: bool,
    pub quit: Option<AttachEnd>,
    /// `app.quit` was confirmed: the daemon going away ends the TUI instead of reconnecting
    /// (which would start a new one).
    quitting: bool,
    pub lost_since: Option<Instant>,
    /// The newer ptyro this TUI restarts into once it is idle (`restart.rs`).
    pub restart_to: Option<String>,
    /// It was restarted and isn't the version it was meant to be: no restart by itself again.
    pub no_restart: bool,
    /// The last key, paste or click (a restart waits for a quiet moment).
    pub last_input: Instant,
    view_dirty_since: Option<Instant>,
    last_list: Instant,
    /// The sessions the last `List` reply named (to tell "gone" from "newer than the reply").
    last_listed: HashSet<SessionId>,
    drag: Option<Drag>,
    reported_focus: Option<(SessionId, bool)>,
    /// Sessions this TUI stopped: their panes move on when they exit (`[ui] on_stop`, UX1.23).
    stopping: HashSet<SessionId>,
    /// While a refusal is handled: the request that takes its way through (SPEC §6.6).
    retry: Option<Box<Request>>,
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
    /// The name of the split that command runs in.
    pub last_split: Option<String>,
    /// The setup checklist's install running in a split, and what runs after it (BOOT2).
    pub setup_run: Option<crate::setup_list::SetupRun>,
    /// Addresses drawn on screen this frame, for the terminal to make links (OSC 8, UX2.1).
    pub links: std::cell::RefCell<Vec<crate::about::Link>>,
    /// Where the pane headers' SHELLS strips were drawn this frame, for clicks (SPEC §23.6).
    pub strip_hits: std::cell::RefCell<Vec<crate::shells::StripAt>>,
    /// The terminal has sent a key with Alt: Alt-1…9 reach ptyro here, whatever it's called.
    pub alt_seen: bool,
    /// Said once that Option-digit typed a character instead of Alt (SPEC §23.6).
    pub option_said: bool,
    /// The SHELLS window on screen and its layout as last agreed with the daemon (SPEC §23.17):
    /// what tells a change made here from one made elsewhere.
    pub(crate) window_seen:
        Option<(crate::windows::WindowKey, Option<ptyro_core::window::WindowLayout>)>,
    /// The layouts sent for it lately: their echoes aren't news from elsewhere.
    pub(crate) window_sent: Vec<ptyro_core::window::WindowLayout>,
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
        let theme = Theme::resolve(&theme_name, no_color());
        let (tx, rx) = mpsc::unbounded_channel();
        let mut panes = HashMap::new();
        panes.insert(1, PaneState::default());
        let workspace = opts.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into());
        let follow = opts.follow_worktree.clone().map(|w| (w, 1));
        Self {
            projects: Vec::new(),
            worktrees: Vec::new(),
            workspaces: Vec::new(),
            profiles: Vec::new(),
            after_profile: None,
            machines: Vec::new(),
            workspace,
            collapsed: HashSet::new(),
            folded_before: None,
            marked: Vec::new(),
            chat_list: Vec::new(),
            chat_panel: None,
            chat_aside: None,
            chat_drafts: HashMap::new(),
            follow,
            resolved: None,
            usage: HashMap::new(),
            machine: None,
            host_load: None,
            terminal_focused: true,
            sidebar: config.ui.sidebar && !opts.raw,
            sidebar_width: config.ui.sidebar_width.clamp(12, SIDEBAR_MAX),
            sidebar_zoom: false,
            show: Default::default(),
            scope: None,
            tabs_frame: std::cell::RefCell::new(None),
            tab_wheel_at: None,
            opened_on_workspace: false,
            drawer: None,
            drawer_share: crate::quick::DRAWER_SHARE,
            kept: None,
            revealing: None,
            tool_back: HashSet::new(),
            again: None,
            again_for: None,
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
            prefix_key: None,
            prefix_at: None,
            overlay: None,
            toast: None,
            buffer: None,
            controller: HashMap::new(),
            area,
            dirty: true,
            quit: None,
            quitting: false,
            lost_since: None,
            restart_to: None,
            no_restart: false,
            last_input: Instant::now(),
            view_dirty_since: None,
            last_list: Instant::now(),
            last_listed: HashSet::new(),
            drag: None,
            reported_focus: None,
            stopping: HashSet::new(),
            retry: None,
            after_add: None,
            context_hint: None,
            first_run: None,
            health: crate::health::Health::default(),
            last_cli: None,
            last_split: None,
            setup_run: None,
            links: std::cell::RefCell::new(Vec::new()),
            strip_hits: std::cell::RefCell::new(Vec::new()),
            alt_seen: false,
            option_said: false,
            window_seen: None,
            window_sent: Vec::new(),
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
        let sessions: Vec<SessionInfo> =
            sessions.into_iter().filter(|s| s.chat.is_none()).collect();
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
        // Nor of the chat shown: it is followed again, or it would never show another word.
        if let Some(crate::chats::ChatPanel::View(v)) = &self.chat_panel
            && let Some(id) = &v.id
        {
            let _ = client.send(ClientMsg::ChatSubscribe { chat: id.clone() });
        }
        // A daemon of another version: an update may have replaced this TUI's binary too.
        self.check_binary();
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
        if let Ok(Reply::Status { status }) = client.request(Request::Status).await {
            self.machine = status.machine;
            self.host_load = status.load;
        }
        if self.config.chat.enabled
            && let Ok(Reply::Chats { chats, .. }) = client
                .request(Request::ChatList {
                    workspace: None,
                    all: true,
                    pinned: false,
                    archived: false,
                    limit: None,
                })
                .await
        {
            self.chat_list = chats;
        }
        if let Ok(Reply::Usage { usage }) = client.request(Request::Usage).await {
            self.usage =
                usage.into_iter().map(|u| ((u.profile.clone(), u.agent.clone()), u)).collect();
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
        // `ptyro chat open`: that chat, over the panes.
        if let Some(id) = self.opts.chat.take() {
            self.open_chat(&id);
        }
    }

    /// The workspace a session shows up under in the sidebar (MM1.B: the first that has its
    /// project, else its home, else its machine's `main`).
    pub fn home_of(&self, s: &SessionInfo) -> String {
        self.tree().home(s)
    }

    /// Load the current workspace's saved view.
    async fn load_view(&mut self) {
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
            ordered_all.extend(self.sessions.iter().filter(|s| s.quick).map(|s| s.id));
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
            self.show = saved.show;
            self.scope = saved.scope;
            self.restore_drawer(saved.drawer, saved.drawer_share);
            self.restore_kept(saved.kept, &ordered_all);
        }
        let ordered = self.ordered();
        // `ptyro shell` asks for a quick terminal: it opens in the drawer (UX1.25).
        let focus = self.opts.focus.take();
        // Opened on a session (`ptyro attach NAME`) or a workspace outside the saved tab: the tab
        // that has it.
        if let Some(id) = focus {
            self.follow_tab_to_session(id);
        } else if let Some(w) = self.opts.workspace.clone().filter(|_| !self.opened_on_workspace) {
            self.opened_on_workspace = true;
            self.rows_with(&Row::Workspace(w));
        }
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

    pub(crate) fn client(&self) -> Option<&Arc<Client>> {
        self.client.as_ref()
    }

    pub(crate) fn send(&self, msg: ClientMsg) {
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
                    | Pending::Updates { .. }
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
        // Kept for a refusal's way through (SPEC §6.6): the same request, said again.
        let again = req.fixable().then(|| req.clone());
        tokio::spawn(async move {
            let res = client.request(req).await;
            let retry = match (&res, again) {
                (Err(e), Some(r)) => e.fix.as_ref().and_then(|f| r.with_fix(&f.again)),
                _ => None,
            };
            let _ = tx.send(match (res, retry) {
                (Err(e), Some(r)) => AppMsg::Refused(pending, e, Box::new(r)),
                (res, _) => AppMsg::Reply(pending, res),
            });
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
            show: self.show,
            scope: self.scope.clone(),
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
        self.maybe_restart();
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
        // Quick new's label follows the context to another worktree.
        if self.client.is_some() && self.again_for.as_ref() != Some(&self.context().1) {
            self.refresh_again();
        }
        self.health_tick(self.started);
        self.sources_tick();
        self.home_tick();
        self.setup_tick();
        self.sync_window();
    }

    fn start_prefix(&mut self, key: KeyEvent) {
        self.prefix = true;
        self.prefix_key = Some(key);
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

    /// What the daemon says: a pane that now shows another session (another device's window,
    /// a session gone and its pane refilled) tucks the drawer away, as a command does.
    pub fn on_server(&mut self, msg: ServerMsg) {
        let before = self.quick_stance();
        self.server_msg(msg);
        self.tuck_away(before);
    }

    fn server_msg(&mut self, msg: ServerMsg) {
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
            ServerMsg::ChatLive { chat, rev, reset, items } => {
                self.chat_live(chat, rev, reset, items);
            }
            ServerMsg::Goodbye { reason } => self.lost(&reason),
            _ => {}
        }
    }

    /// A `List` reply (every 2 s, and after events were lost): it is the truth about what
    /// exists, so a session it no longer names (whose removal event never came) is gone from
    /// panes and every per-session map. One made by an event after the reply was asked for
    /// isn't in it yet and stays.
    fn got_list(&mut self, mut listed: Vec<SessionInfo>) {
        listed.retain(|s| s.chat.is_none());
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
                if self.drawer_session() == Some(*id) {
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
        // A chat's engine is the chat's, never a row of its own (SPEC §22.2).
        if info.chat.is_some() {
            return;
        }
        match self.sessions.iter_mut().find(|s| s.id == info.id) {
            Some(s) => *s = info,
            None => self.sessions.push(info),
        }
    }

    fn on_event(&mut self, event: DomainEvent) {
        // Home shows all of it: whatever changed, it asks again shortly.
        self.home_news();
        match event {
            // We fell behind: what we know may be stale.
            DomainEvent::EventsLost { .. } => {
                self.request(Pending::List, Request::List);
                self.refresh_chats();
            }
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
                // `exit` in the quick terminal: its drawer closes (UX1.25).
                if self.drawer_session() == Some(id) {
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
            DomainEvent::Notification { .. } | DomainEvent::Alert { .. } => {}
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
            DomainEvent::HostLoad { load } => self.host_load = load,
            // The web's setup runs (the TUI runs its own, `ptyro setup` in a split).
            DomainEvent::KbUpdated { .. } => self.library_news(),
            // A pass over a root: the Sources tab shows how far it is.
            DomainEvent::KbProgress { root, done, of, .. } => {
                self.sources_progress(&root, done, of)
            }
            DomainEvent::SetupProgress { .. } => {}
            DomainEvent::ChatUpdated { info } => self.chat_updated(info),
            DomainEvent::ChatRemoved { id, machine } => self.chat_removed(&id, machine.as_deref()),
            DomainEvent::ProjectUpdated { info } => {
                // Found where its repository is now (SPEC §10.1): said once, the move one key away.
                let was = self.project(&info.id).and_then(|p| p.moved_to.clone());
                if let Some(to) = info.moved_to.clone().filter(|to| was.as_ref() != Some(to)) {
                    self.toast_offer(
                        format!("{} seems to have moved to {}", info.slug, to.display()),
                        false,
                        move_offer(&info.id, &info.slug, &to),
                    );
                }
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
                self.chat_machine_news(&info);
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
                // Locked now: kept, so no longer stale.
                if info.locked && self.worktree(&info.id).is_some_and(|w| !w.locked) {
                    self.stale_changed();
                }
                upsert_by(&mut self.worktrees, info, |w| w.id.clone());
                self.try_after_add();
            }
            DomainEvent::WorktreeRemoved { id } => {
                self.worktrees.retain(|w| w.id != id);
                self.stale_changed();
            }
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
                    // The knowledge base's switches may have moved.
                    self.library_news();
                }
            },
        }
    }

    /// Read the config again; `say` toasts that it was (the settings panel makes the change
    /// itself and says its own).
    pub(crate) fn reload_config_with(&mut self, say: bool) {
        let path = Config::path(&self.paths);
        match Config::load(&path) {
            Ok(config) => {
                let (keymap, errors) = Keymap::new(&config);
                self.keymap = keymap;
                self.theme_name = config.ui.theme.clone();
                self.theme = Theme::resolve(&self.theme_name, no_color());
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
                // Chats switched on: their list is fetched; off: the chat panel goes.
                match (old.chat.enabled, self.config.chat.enabled) {
                    (false, true) => self.refresh_chats(),
                    // (The Library is no chat: it stays.)
                    (true, false) => {
                        use crate::chats::ChatPanel::Library;
                        if !matches!(self.chat_panel, Some(Library(_))) {
                            self.close_chat();
                        }
                        if !matches!(self.chat_aside, Some(Library(_))) {
                            self.drop_chat_aside();
                        }
                    }
                    _ => {}
                }
                self.redress_chats();
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

    /// A reply: what it shows (a new shell, a session a command asked for) arrives after the
    /// command returned, so it tucks the drawer away here. Not a workspace's saved view, which
    /// brings back the drawer it was left with.
    pub fn on_reply(&mut self, msg: AppMsg) {
        if matches!(msg, AppMsg::View(..)) {
            return self.reply(msg);
        }
        let before = self.quick_stance();
        self.reply(msg);
        self.tuck_away(before);
    }

    fn reply(&mut self, msg: AppMsg) {
        self.dirty = true;
        let (pending, res, retry) = match msg {
            AppMsg::Reply(p, r) => (p, r, None),
            AppMsg::Refused(p, e, r) => (p, Err(e), Some(r)),
            AppMsg::Health(checks) => {
                self.got_health(checks);
                return;
            }
            AppMsg::SetupFound(found) => {
                self.setup_found(found);
                return;
            }
            AppMsg::Binary { on_disk, daemon } => {
                self.got_binary(on_disk, &daemon);
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
        // The setup checklist's split started, or couldn't (BOOT2).
        if let Pending::Spawned { spec, .. } = &pending
            && self.setup_run.is_some()
        {
            let name = spec.name.clone();
            // Asked to agree first: the Confirm overlay may still start it (a `y`), so the run
            // is kept until it does, or the person declines (BOOT2's M2).
            let asked = matches!(&res, Err(e) if e.code == ErrorCode::NeedsConfirmation);
            if !asked {
                self.setup_spawned(name.as_deref(), res.is_ok());
            }
        }
        // A reply that opens a dialog must not replace the box someone is typing in: it is
        // held aside while the reply is handled, then put back (and the reply said so).
        // (A peek's own screen arrives into the box it fills in: that one is not held.)
        let held = match &self.overlay {
            Some(
                Overlay::Prompt { .. }
                | Overlay::Task { .. }
                | Overlay::Peek { .. }
                | Overlay::Form(_),
            ) if !matches!(pending, Pending::Peek(_)) => self.overlay.take(),
            _ => None,
        };
        // A refusal's way through, for the reply's error to offer (SPEC §6.6).
        self.retry = retry;
        self.handle_reply(pending, res);
        self.retry = None;
        if let Some(h) = held {
            if self.overlay.is_some() {
                self.toast("a reply arrived while you were typing: ask again when you're done");
            }
            self.overlay = Some(h);
        }
    }

    fn handle_reply(&mut self, pending: Pending, res: Result<Reply, ptyro_core::Error>) {
        match (pending, res) {
            (Pending::Confirm(req, then), Err(e)) if e.code == ErrorCode::NeedsConfirmation => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!("{} [y/N]", e.message),
                    action: Confirmed::Again(req, then),
                });
            }
            (Pending::Confirm(_, then), r) => self.handle_reply(*then, r),
            (Pending::List, Ok(Reply::Sessions { sessions })) => self.got_list(sessions),
            (Pending::Updates { note: noting }, Ok(Reply::Updates { offered, note })) => {
                self.got_updates(offered, note, noting)
            }
            // An older daemon (before protocol 1.35) can't say: nothing to offer.
            (Pending::Updates { .. }, _) => {}
            (Pending::Chats, Ok(Reply::Chats { chats, .. })) => self.got_chats(chats),
            (Pending::ArchivedChats, Ok(Reply::Chats { chats, .. })) => self.got_archived(chats),
            (Pending::ChatMade { token, .. }, Ok(Reply::Chat { info })) => {
                self.chat_made(info, token)
            }
            (Pending::ChatMade { text, token }, Err(e)) => {
                self.chat_not_made(token, text, e.message)
            }
            (Pending::ChatSent { chat, text }, Err(e)) => {
                self.chat_sent(&chat, text, Some(e.message))
            }
            (Pending::ChatSent { chat, text }, Ok(_)) => self.chat_sent(&chat, text, None),
            (Pending::ChatOpen(origin), Ok(Reply::Chat { info })) => {
                self.chat_ready(info, &origin, None);
            }
            (Pending::ChatOpenAt(node, origin), Ok(Reply::ChatTranscript { info, .. })) => {
                self.chat_ready(info, &origin, node.as_deref());
            }
            (Pending::ChatPreview(id), Ok(Reply::ChatTranscript { nodes, .. })) => {
                self.got_preview(&id, nodes)
            }
            (Pending::ChatPreview(_), Err(_)) => {}
            (Pending::ChatHits(q), Ok(Reply::ChatHits { hits })) => self.got_hits(q, hits),
            (Pending::ChatHits(_), Err(e)) if e.code == ErrorCode::NeedsConfirmation => {
                self.offer_meaning(e.message);
            }
            // A query still being typed: its error is no news (a short prefix, a lone `-`).
            (Pending::ChatHits(_), Err(_)) => {}
            (Pending::KbRows(asked), res) => self.library_rows(asked, res),
            (Pending::KbStatus, Ok(Reply::KbStatus { status })) => self.library_status(status),
            (Pending::KbStatus, Err(_)) => {}
            (Pending::KbPreview(addr), Ok(Reply::KbDoc { item, chunks, at, .. })) => {
                self.library_previewed(&addr, item, chunks, at)
            }
            // The row went while its text was asked for (forgotten, read again).
            (Pending::KbPreview(_), Err(_)) => {}
            (Pending::KbReader(piece), Ok(Reply::KbDoc { item, chunks, .. })) => {
                self.library_read(piece, item, chunks)
            }
            (Pending::KbReaderAgain, Ok(Reply::KbDoc { item, chunks, .. })) => {
                self.library_read_again(item, chunks)
            }
            // The news was that it went (forgotten from here, or elsewhere).
            (Pending::KbReaderAgain, Err(_)) => {}
            (Pending::KbResume, Ok(Reply::KbDoc { item, .. })) => self.library_resumed(&item),
            (Pending::KbForgotten, Ok(Reply::KbForgotten { items, chunks })) => {
                self.library_forgot(items, chunks)
            }
            (Pending::KbSynced, Ok(Reply::KbSynced { notes, changed, removed, roots })) => {
                self.library_synced(notes, changed, removed, roots)
            }
            (Pending::KbOlder(asked), Ok(Reply::KbItems { items })) => {
                self.library_older_rows(asked, items)
            }
            (Pending::KbPaths, Ok(Reply::Paths { paths })) => self.ingest_paths(paths),
            (Pending::KbPaths, Err(_)) => self.ingest_paths(Vec::new()),
            (Pending::KbIngested(machine), Ok(Reply::KbIngested { root, plan })) => {
                self.ingested(machine, &root, &plan)
            }
            (Pending::KbRemoved, Ok(Reply::KbRemoved { name, items, chunks })) => {
                self.sources_removed(&name, items, chunks)
            }
            (Pending::KbCounted(read), Ok(Reply::KbHistory { plan })) => {
                self.backfill_counted(read, &plan)
            }
            (Pending::KbBackfilled(said), Ok(Reply::KbHistory { .. })) => {
                self.toast(said);
                self.library_news();
            }
            (Pending::ChatExport(path), Ok(Reply::Text { text })) => {
                match std::fs::write(&path, text) {
                    Ok(()) => self.toast(format!("written to {}", path.display())),
                    Err(e) => self.toast_err(format!("{}: {e}", path.display())),
                }
            }
            (Pending::Registry, Ok(Reply::Profiles { profiles })) => {
                self.profiles = profiles;
                self.profiles_arrived();
            }
            (Pending::RemoteEditors(wt), Ok(Reply::Editors { names, default })) => {
                self.editor_picker(wt, names, &default, true);
            }
            (Pending::EditorOpened(_), Ok(Reply::Session { info })) => self.handle_reply(
                Pending::Spawned { split: None, spec: Box::default(), back: false },
                Ok(Reply::Session { info }),
            ),
            (Pending::EditorOpened(name), Ok(Reply::Notes { notes })) => {
                self.toast(if notes.is_empty() {
                    format!("opened in {name}")
                } else {
                    notes.join(" · ")
                });
            }
            (Pending::Spawned { split, back, .. }, Ok(Reply::Session { info })) => {
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
            (Pending::Done(_), Ok(Reply::Notes { notes })) => {
                self.toast(notes.join(" · "));
            }
            (Pending::GcKept, Ok(Reply::Notes { notes })) => {
                self.toast(notes.join(" · "));
                self.health_again();
            }
            (Pending::Quit, Ok(_)) => self.quit = Some(AttachEnd::Stopped),
            (Pending::Quit, Err(e)) => {
                self.quitting = false;
                self.toast_err(format!("ptyro didn't stop: {}", e.message));
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
                        purpose: Purpose::Branch(*w),
                    });
                }
            }
            (Pending::Removed(_) | Pending::Forget(_), Ok(Reply::Removed { notes })) => {
                self.toast(notes.join(" · "))
            }
            (Pending::Removed(id), Err(e))
                if e.fix.as_ref().is_some_and(|f| f.again == ptyro_core::error::Again::Force) =>
            {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "{}\n\ny: remove it anyway ({}) · n: keep it",
                        e.message,
                        ptyro_core::removal::FORCED
                    ),
                    action: Confirmed::RemoveWorktree { id, force: true },
                });
            }
            (Pending::Gc, Ok(Reply::Gc { entries, scratch })) => {
                if entries.is_empty() && scratch.is_empty() {
                    self.toast("nothing stale: every worktree is in use");
                } else {
                    let mut items: Vec<Item> = entries
                        .iter()
                        .map(|e| Item {
                            label: e.worktree.qualified(),
                            detail: e
                                .size_label()
                                .into_iter()
                                .chain(e.reasons.iter().cloned())
                                .collect::<Vec<_>>()
                                .join(" · "),
                            key: None,
                            target: Target::Worktree(e.worktree.id.clone()),
                        })
                        .collect();
                    // Scratch shells' directories left behind (SPEC §23.8): ⏎ deletes, asked.
                    items.extend(scratch.iter().map(|l| Item {
                        label: format!(
                            "scratch {}",
                            l.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                        ),
                        detail: format!(
                            "{} · {} files · left behind by a scratch shell, changed {} ago · ⏎ deletes it",
                            ptyro_core::model::human_bytes(l.bytes),
                            l.files,
                            crate::ui::ago(l.modified_ms)
                        ),
                        key: None,
                        target: Target::Path(l.path.clone()),
                    }));
                    self.overlay = Some(Overlay::Picker {
                        palette: Palette::new(
                            "Stale worktrees, biggest first   ⏎ remove (asks first; refuses if work would be lost) · C-k keep it (not offered again until it changes) · Esc: not now",
                            items,
                        ),
                        purpose: Purpose::Gc,
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
            (Pending::Moved(_), Ok(Reply::Project { info, notes })) => {
                upsert_by(&mut self.projects, info, |p| p.id.clone());
                self.toast(notes.join(" · "));
            }
            (Pending::FirstRunRepos, Ok(Reply::Paths { paths })) => self.first_run_repos(paths),
            (Pending::Home, Ok(Reply::Paths { paths })) => self.show_home(paths),
            (Pending::Home, Err(_)) => self.show_home(Vec::new()),
            (Pending::HomeView, res) => self.home_got(res),
            (Pending::HomeStart, res) => self.home_started(res),
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
            // A retried refusal's reply says what it did (SPEC §6.6): its notes, not the label.
            (Pending::Done(_), Ok(Reply::Removed { notes })) if !notes.is_empty() => {
                self.toast(notes.join(" · "))
            }
            (Pending::Done(msg), Ok(_)) => {
                if !msg.is_empty() {
                    self.toast(msg);
                }
            }
            (Pending::Saved, Ok(_)) => {}
            (Pending::Again(wt), Ok(Reply::LastLaunch { launch })) => {
                if self.again_for.as_ref() == Some(&wt) {
                    self.again = launch.map(|l| (wt, l));
                    self.dirty = true;
                }
            }
            // Only a label: a worktree still being made says so elsewhere.
            (Pending::Again(_), Err(_)) => {}
            // A refusal's way through (SPEC §6.6), else what its words suggest.
            (_, Err(e)) => match (self.retry.take(), &e.fix) {
                (Some(req), Some(fix)) => {
                    let label = fix.label.to_lowercase();
                    let offer = Offer { label: label.clone(), then: Then::Send(req, label) };
                    self.toast_offer(e.message, true, offer)
                }
                _ => match offer_for(&e.message) {
                    Some(offer) => self.toast_offer(e.message, true, offer),
                    None => self.toast_err(e.message),
                },
            },
            _ => {}
        }
    }

    // ── showing sessions ────────────────────────────────────────────────────

    /// Show `id`: in a new split when one is asked for, else without a split (UX3.1, `place`):
    /// where it shows already, in the kept layout that has it, or full sized (the split it
    /// replaces is kept). For something new: a session just started, a tool.
    pub fn show(&mut self, id: SessionId, split: Option<Dir>) {
        self.show_in(id, split, false);
    }

    /// Show a session you picked (the sidebar, `/`, `1`–`9`, `j`/`k`): as [`Self::show`], but
    /// in a split it takes the focused pane instead of the whole screen (`place_picked`).
    pub fn pick(&mut self, id: SessionId) {
        self.show_in(id, None, true);
    }

    fn show_in(&mut self, id: SessionId, split: Option<Dir>, picked: bool) {
        // A session shown takes the place of the chat panel, which `session.last` brings back;
        // shown from the panes, it ends a way back to a chat.
        if self.chat_panel.is_some() {
            self.set_chat_aside();
        } else {
            self.drop_chat_aside();
        }
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
            None if picked => self.place_picked(id),
            None => self.place(id),
        };
        let p = self.panes.entry(pane).or_default();
        if p.session != Some(id) {
            // (An empty pane keeps the way back it was given: the session before a split went.)
            p.last = p.session.or(p.last).filter(|l| *l != id);
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
        self.pick(ordered[next]);
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
        // Not every key goes through `run` (a chat's, an overlay's pick): whatever it shows
        // tucks the drawer away too.
        let before = self.quick_stance();
        self.key_event(ck);
        self.tuck_away(before);
    }

    fn key_event(&mut self, ck: &crossterm::event::KeyEvent) {
        self.last_input = Instant::now();
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
        self.learn_alt(key);
        if self.first_run.is_some() && !self.prefix && !self.keymap.is_prefix(key) {
            self.first_run_key(key);
            return;
        }
        if self.overlay.is_some() {
            // The prefix always works: it closes whatever is open and starts a command.
            if self.keymap.is_prefix(key)
                && !matches!(self.overlay, Some(Overlay::Prompt { .. } | Overlay::Form(_)))
            {
                self.overlay = None;
                self.start_prefix(key);
                return;
            }
            // A prefix still waiting when the overlay came up (a dialog an event opened) must
            // not turn the overlay's closing key into a command afterwards.
            self.prefix = false;
            self.overlay_key(key);
            return;
        }
        // The chat panel has the keyboard while the panes have focus (the sidebar keeps its own).
        if self.chat_panel.is_some()
            && !self.prefix
            && self.focus == Focus::Pane
            && !self.full_view()
        {
            if self.keymap.is_prefix(key) {
                self.start_prefix(key);
            } else if let Some(cmd) = self.keymap.lookup(Table::Global, key).cloned() {
                // Global keys (`Alt-1`…, shells) mean anywhere, a chat that's typing included.
                self.run(&cmd);
            } else {
                self.chat_key(key);
            }
            return;
        }
        if self.prefix {
            self.prefix = false;
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
            self.start_prefix(key);
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
        self.forward(InputEvent::Key { key });
    }

    /// A full-screen view (the first run) is drawn over the panes:
    /// the mouse and pastes are not for what is hidden under it.
    fn full_view(&self) -> bool {
        self.first_run.is_some()
    }

    pub fn on_paste(&mut self, text: String) {
        self.last_input = Instant::now();
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
            Some(Overlay::Form(form)) => form.paste(&text),
            Some(Overlay::Picker { palette, .. }) => palette.paste(&text),
            Some(Overlay::Help { filter, scroll }) => {
                filter.push_str(&flat());
                *scroll = 0;
            }
            Some(_) => {}
            None if full => {}
            None if self.chat_panel.is_some() && self.focus == Focus::Pane => {
                self.chat_paste(&lines);
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
            "session.tab" | "session.pane" => {
                let target = if self.focus == Focus::Sidebar {
                    self.selected_row().and_then(|r| r.session())
                } else {
                    focused
                };
                let Some(id) = target else {
                    self.toast("no session here");
                    return;
                };
                let place = if cmd.action == "session.pane" {
                    crate::tab::Place::Pane
                } else {
                    crate::tab::Place::Tab
                };
                match crate::tab::open(place, &id.full(), self.connect.remote.as_deref()) {
                    Ok(said) => self.toast(said),
                    Err(e) => self.toast_err(e),
                }
            }
            "session.resume" | "session.hibernate" | "session.explain" | "session.mute"
            | "session.private" => {
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
                    "session.mute" => self.request(
                        Pending::Done("notifications muted for 1h".into()),
                        Request::Mute { target: addr, minutes: Some(60), unmute: false },
                    ),
                    // The daemon asks first, in its words (`kb forget --session`, §25.7a).
                    "session.private" => {
                        let req = Request::KbForget {
                            addrs: Vec::new(),
                            query: None,
                            confirmed: false,
                            session: Some(addr),
                        };
                        let done = Pending::Done("private: nothing of it is kept".into());
                        self.request(Pending::Confirm(Box::new(req.clone()), Box::new(done)), req);
                    }
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
                    // Every option on one form, a free name already in it (SPEC §11.5a).
                    Some(project) => {
                        let name = Some(self.free_worktree_name(&project, None));
                        self.open_session_form(Wizard {
                            project: Some(project),
                            name,
                            ..Wizard::default()
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
            "project.trust" => match self.context().0 {
                Some(p) => self.request(
                    Pending::TrustDialog(AfterTrust::Nothing),
                    Request::ProjectScripts { addr: p, worktree: None },
                ),
                None => self.pick_project(Some(AfterAdd::Run("project.trust"))),
            },
            "worktree.gc" => self.request(Pending::Gc, Request::Gc),
            "shell.new" => self.new_shell(),
            // SHELLS (SPEC §23.6).
            "shells.go" => match cmd.number() {
                Some(n) if n > 0 => self.shells_go(n as u32),
                _ => self.shells_menu(),
            },
            "shells.new" => self.shells_new(None, false),
            "shells.menu" => self.shells_menu(),
            a if a.starts_with("shells.") => self.shells_action(a),
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
                    Some(id) => self.pick(*id),
                    None => self.toast(format!("no session {n}")),
                }
            }
            "session.find" => self.open_finder("Show session", Purpose::Show, false),
            // Back and forth: from a chat to the session under it, and back to the chat; else
            // the previous session here.
            "session.last" => {
                if self.chat_panel.is_some() && !self.full_view() {
                    self.set_chat_aside();
                    self.focus = Focus::Pane;
                } else if !self.bring_chat_back() && !self.go_back() {
                    self.toast("no previous session or chat in this pane");
                }
            }
            "session.stop"
            | "session.kill"
            | "session.remove"
            | "session.restart"
            | "session.rename"
            | "session.take_control" => {
                // An offer names its session: it acts on that one, wherever the selection went.
                if let Some(id) = cmd.arg.as_deref().and_then(|a| a.parse().ok()) {
                    return self.session_action(cmd.action, id);
                }
                // On a chat row, the same keys mean the chat's own: `,` its title, `x` archive,
                // `X` delete (SPEC §22.12).
                let on_chat_row = self.focus == Focus::Sidebar
                    && matches!(self.selected_row(), Some(Row::Chat(_)));
                // With the chat panel over the panes (a chat, the list, a new chat), they mean
                // its chat, never the hidden session.
                let chat_shown = self.focus == Focus::Pane && self.chat_panel.is_some();
                // (The Library has no chat for them to mean.)
                let library = chat_shown
                    && matches!(self.chat_panel, Some(crate::chats::ChatPanel::Library(_)));
                if on_chat_row || chat_shown {
                    let a = match cmd.action {
                        "session.rename" if !library => "chat.rename",
                        "session.stop" if !library => "chat.archive",
                        "session.kill" if !library => "chat.delete",
                        _ if !chat_shown => return,
                        _ => {
                            let back = self.keymap.key_for("session.last").unwrap_or_default();
                            let what = if library { "Library" } else { "chat" };
                            return self.toast(format!(
                                "that's a session's: the {what} covers it ({back} shows the session)"
                            ));
                        }
                    };
                    return self.chat_action(a);
                }
                let target = if self.focus == Focus::Sidebar {
                    match self.selected_row() {
                        Some(Row::Session(id) | Row::Quick(id)) => Some(id),
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
                    self.toast_offer(
                        "no session here",
                        false,
                        run_offer("start one", "session.new"),
                    );
                    return;
                };
                self.session_action(cmd.action, id);
            }
            "prefix.send" => {
                let key = self.prefix_key.unwrap_or(self.keymap.prefix);
                self.forward(InputEvent::Key { key });
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
            "row.menu" if self.chat_panel.is_some() && self.focus == Focus::Pane => {
                self.chat_menu()
            }
            "row.menu" => self.open_row_menu(),
            "home" => self.toggle_home(),
            "home.add" => self.open_home(),
            "home.restore" => {
                self.request(
                    Pending::HomeView,
                    Request::Home { trail: None, dismiss: None, restore: true, here: true },
                );
                self.toast("everything put away on home is back");
            }
            "app.about" => self.open_about(),
            a if a.starts_with("kb.") => self.library_action(a),
            "health" => self.open_health(),
            "daemon.logs" => self.run_cli(&["daemon".into(), "logs".into(), "-f".into()]),
            "daemon.status" => self.run_cli(&["daemon".into(), "status".into()]),
            // Asked, as the web asks, in the same words (`removal::DAEMON_RESTART`).
            "daemon.restart" => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "Restart the daemon? {} [y/N]",
                        ptyro_core::removal::DAEMON_RESTART
                    ),
                    action: Confirmed::RestartDaemon,
                });
            }
            "daemon.upgrade" => self.run_cli(&["daemon".into(), "upgrade".into()]),
            "daemon.service" => self.run_cli(&["daemon".into(), "install-service".into()]),
            "daemon.unservice" => self.run_cli(&["daemon".into(), "uninstall-service".into()]),
            "doctor" => self.run_cli(&["doctor".into()]),
            "first.run" => self.open_first_run(),
            // With names: those ticked (a message's offer, BOOT9).
            "app.setup" => self.open_setup(
                cmd.arg.as_deref().map(|a| a.split_whitespace().map(str::to_owned).collect()),
            ),
            "app.update" => self.update_agents(
                cmd.arg.as_deref().unwrap_or("").split_whitespace().map(str::to_owned).collect(),
            ),
            "toast.act" => match self.toast.take().and_then(|t| t.offer) {
                Some(Offer { then: Then::Run(cmd), .. }) => self.run(&cmd),
                Some(Offer { then: Then::Send(req, done), .. }) => {
                    // A refusal's "Stop and remove" asks as Stop and remove does (SPEC §7.3).
                    let stop_remove = match &*req {
                        Request::Remove { addr, stop: true } => {
                            self.sessions.iter().find(|s| s.id.full() == *addr).map(|s| s.id)
                        }
                        _ => None,
                    };
                    match stop_remove {
                        Some(id) => self.ask_end(Confirmed::StopRemove(id)),
                        None => self.request(Pending::Done(done), *req),
                    }
                }
                Some(Offer { then: Then::Edit(path), .. }) => self.edit_file(path, None),
                Some(Offer { then: Then::Set { key, value, done }, .. }) => {
                    self.set_config(&key, &value, &done)
                }
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
            "sidebar.fold_all" => self.fold_all(),
            "sidebar.filter" => self.set_show(self.show.next()),
            "sidebar.show_all" => self.set_show(Show::All),
            "sidebar.show_running" => self.set_show(Show::Running),
            "sidebar.show_active" => self.set_show(Show::Active),
            "sidebar.tab_next" => self.cycle_tab(1),
            "sidebar.tab_prev" => self.cycle_tab(-1),
            "sidebar.tab_all" if !ptyro_core::scope::drawn(&self.tabs()) => self.toast(NO_TABS),
            "sidebar.tab_all" => self.set_scope(None),
            "sidebar.toggle" => {
                self.sidebar = !self.sidebar;
                if !self.sidebar {
                    self.focus = Focus::Pane;
                }
                self.view_changed();
            }
            "pane.split_right" => self.open_split_picker(Dir::Right),
            "pane.split_down" => self.open_split_picker(Dir::Down),
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
            "view.redraw" => {
                for id in self.subscribed.drain().collect::<Vec<_>>() {
                    self.send(ClientMsg::Unsubscribe { session: id });
                    self.sizes.remove(&id);
                }
                self.sync();
            }
            "config.panel" => self.open_settings(cmd.arg.as_deref()),
            "config.edit" => {
                let argv = vec![self.daemon_ptyro(), "config".into(), "edit".into()];
                self.spawn_tool(argv, None, true);
                if self.connect.remote.is_some() {
                    self.toast(self.daemon_file_note());
                }
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
            // From an offer: that chat, or that search.
            "chat.open" if cmd.arg.is_some() => self.open_chat(cmd.arg.as_deref().unwrap_or("")),
            "chat.search" if cmd.arg.is_some() => {
                self.chat_action("chat.search");
                if let Some(crate::chats::ChatPanel::List(l)) = &mut self.chat_panel {
                    l.query = cmd.arg.clone().unwrap_or_default();
                    l.typing = false;
                }
                self.search_chats();
            }
            a if a.starts_with("chat.") => self.chat_action(a),
            "nav.open" | "nav.collapse" | "nav.expand" => match self.selected_row() {
                Some(r @ (Row::Chat(_) | Row::ChatNew(_) | Row::ChatAll(_)))
                    if cmd.action == "nav.open" =>
                {
                    self.sidebar_zoom = false;
                    self.chat_row_open(&r);
                }
                Some(Row::Session(id)) if cmd.action == "nav.open" => {
                    self.sidebar_zoom = false;
                    self.pick(id)
                }
                Some(Row::Quick(id)) if cmd.action == "nav.open" => {
                    self.sidebar_zoom = false;
                    self.toggle_quick(Some(id), None, None);
                }
                Some(Row::ShellNew(_)) if cmd.action == "nav.open" => {
                    self.sidebar_zoom = false;
                    self.shells_new(None, false);
                }
                // A workspace, project, worktree or CHATS heading: ⏎ (and Space)
                // fold it, h/l close and open it; `all chats…` (or the chat list's key) lists
                // a workspace's chats.
                Some(r) if r.fold_key().is_some() => {
                    let key = r.fold_key().unwrap_or_default();
                    let collapse = match cmd.action {
                        "nav.collapse" => true,
                        "nav.expand" => false,
                        _ => !self.collapsed.contains(&key),
                    };
                    self.fold_by_hand(key, collapse);
                }
                _ => {}
            },
            other if crate::cli_actions::template(other).is_some() => self.start_cli(other),
            other => self.native_extra(other),
        }
    }

    /// The sidebar's width, kept within 12–80 and leaving the panes at least 21 columns, saved
    /// with the workspace's view (UX1.7, UX1.8).
    /// What the sidebar lists (the strip under its title), saved with the view. The sidebar is
    /// shown, so the change is seen; its selection stays on its row when that still shows.
    pub(crate) fn set_show(&mut self, show: Show) {
        self.show = show;
        self.sidebar = true;
        let n = self.sessions.iter().filter(|s| show.keeps(s)).count();
        self.toast(format!("sidebar: {} ({n})", show.label()));
        self.view_changed();
    }

    /// The tabs over the sidebar, after `all` (SPEC §11.2k): each machine and profile in use.
    pub fn tabs(&self) -> Vec<Tab> {
        if let Some(tabs) = self.tabs_frame.borrow().as_ref() {
            return tabs.clone();
        }
        let here = self.machine.as_deref().unwrap_or("this machine");
        ptyro_core::scope::tabs(&self.workspaces, &self.sessions, here)
    }

    /// The tab in effect: the one picked while it is a tab and the strip is drawn, else `None`
    /// (`all`). The pick itself stays, so a linked machine's tab comes back with its mirror.
    pub(crate) fn scope_shown(&self) -> Option<&Scope> {
        let picked = self.scope.as_ref()?;
        ptyro_core::scope::resolve(Some(picked), &self.tabs()).is_some().then_some(picked)
    }

    /// Pick a tab (`None`: `all`), saved with the view. The sidebar is shown, so the change is
    /// seen, as with the filter. A reveal still waiting for its row no longer switches tab.
    pub(crate) fn set_scope(&mut self, scope: Option<Scope>) {
        let label = scope_label(&self.tabs(), scope.as_ref());
        self.scope = scope;
        self.sidebar = true;
        if let Some(r) = &mut self.revealing {
            r.2 = false;
        }
        self.toast(format!("sidebar: {label}"));
        self.scope_changed();
    }

    /// After the tab changed: the sidebar's selection on a row the tab shows, so the workspace
    /// you're in is one of its; then the view is saved.
    fn scope_changed(&mut self) {
        if self.focus == Focus::Sidebar {
            let rows = self.rows();
            let i = self.sel_index(&rows);
            self.set_sel(&rows, i);
        }
        self.view_changed();
    }

    /// The next or previous tab from the one in effect, `all` among them, round.
    fn cycle_tab(&mut self, step: isize) {
        let tabs = self.tabs();
        if !ptyro_core::scope::drawn(&tabs) {
            self.toast(NO_TABS);
            return;
        }
        let shown = self.scope_shown().cloned();
        let every: Vec<Option<Scope>> =
            std::iter::once(None).chain(tabs.into_iter().map(|t| Some(t.scope))).collect();
        let cur = every.iter().position(|s| *s == shown).unwrap_or(0);
        let next = (cur as isize + step).rem_euclid(every.len() as isize) as usize;
        self.set_scope(every[next].clone());
    }

    /// The wheel over the strip: a tab per turn, not one per event of a trackpad's flick.
    fn wheel_tab(&mut self, step: isize) {
        if self.tab_wheel_at.is_some_and(|t| t.elapsed() < TAB_WHEEL_GAP) {
            return;
        }
        self.tab_wheel_at = Some(Instant::now());
        self.cycle_tab(step);
    }

    /// Going to something outside the tab (SPEC §11.2k): the tab `to` when it is one, else
    /// `all`, said in a short message. Nothing changes under `all`, or when `to` is the tab.
    pub(crate) fn follow_tab(&mut self, to: Option<Scope>) {
        let shown = self.scope_shown().cloned();
        if shown.is_none() || shown == to {
            return;
        }
        let tabs = self.tabs();
        self.scope = ptyro_core::scope::resolve(to.as_ref(), &tabs);
        self.toast(format!("showing {}", scope_label(&tabs, self.scope.as_ref())));
        self.scope_changed();
    }

    /// A session going on screen that the tab doesn't list: its own tab. A quick terminal goes
    /// with its worktree, so it moves nothing; nor does one the filter hides anyway.
    pub(crate) fn follow_tab_to_session(&mut self, id: SessionId) {
        let shown = self.scope_shown();
        let to = self
            .session(id)
            .filter(|s| !s.quick && self.show.keeps(s))
            .filter(|s| shown.is_some_and(|t| !t.keeps_session(s)))
            .map(Scope::of);
        if let Some(to) = to {
            self.follow_tab(Some(to));
        }
    }

    /// The sidebar's rows with `row` among them: under a tab that doesn't list it, the tab of
    /// the workspace it is in (where it shows whole), else `all`. When the filter hides it
    /// anyway, the tab stays.
    pub(crate) fn rows_with(&mut self, row: &Row) -> Vec<Row> {
        let rows = self.rows();
        if self.scope_shown().is_none() || rows.contains(row) {
            return rows;
        }
        let project = match row {
            Row::Project(p) => Some(p.clone()),
            Row::Worktree(w) => self.worktree(w).map(|w| w.project.clone()),
            _ => None,
        };
        let own = self
            .workspaces
            .iter()
            .find(|w| match (row, &project) {
                (Row::Workspace(name), _) => w.name == *name,
                (_, Some(p)) => w.projects.contains(p),
                _ => false,
            })
            .map(Scope::of_workspace);
        let lists =
            |scope: Option<&Scope>| tree::Inputs { scope, ..self.tree() }.rows().contains(row);
        let to = match own {
            Some(own) if lists(Some(&own)) => Some(own),
            _ if lists(None) => None,
            _ => return rows,
        };
        self.follow_tab(to);
        self.rows()
    }

    /// A click on the tab strip at column `col` of a sidebar `width` wide: the tab there, or
    /// the next one out of sight on a `‹` / `›`.
    pub(crate) fn tab_click(&mut self, col: u16, width: u16) {
        let Some(strip) = crate::ui::tab_strip(self, width) else { return };
        if strip.left == Some(col) {
            self.cycle_tab(-1);
        } else if strip.right == Some(col) {
            self.cycle_tab(1);
        } else if let Some(c) = strip.chips.into_iter().find(|c| (c.from..c.to).contains(&col)) {
            self.set_scope(c.scope);
        }
    }

    /// Fold or open one row the user picked. A change of their own makes the layout `Z` saved
    /// stale, so the next `Z` saves this one instead (opening a workspace to show a session
    /// doesn't: that is ptyro's, not theirs).
    fn fold_by_hand(&mut self, key: String, collapse: bool) {
        self.folded_before = None;
        if collapse {
            self.collapsed.insert(key);
        } else {
            self.collapsed.remove(&key);
        }
    }

    /// `Z` in the sidebar (`sidebar.fold_all`): fold every row that folds, so the sidebar is one
    /// line a workspace. Once every row on screen is folded, it puts back what was open before the
    /// first fold (with nothing saved, it opens the rows on screen); what arrived meanwhile under a
    /// folded row doesn't count, and opens with the rest. The selection moves to the row it folded
    /// into.
    fn fold_all(&mut self) {
        let before = self.rows();
        let shown: Vec<String> = before.iter().filter_map(Row::fold_key).collect();
        if shown.is_empty() {
            self.toast("nothing in the sidebar to fold");
            return;
        }
        let at = self.sel_index(&before);
        if shown.iter().all(|k| self.collapsed.contains(k)) {
            match self.folded_before.take() {
                Some(was) => {
                    self.collapsed = was;
                    self.toast("sidebar unfolded as it was");
                }
                // Folded by hand: open what shows, and keep the folds under it.
                None => {
                    for k in &shown {
                        self.collapsed.remove(k);
                    }
                    self.toast("sidebar unfolded");
                }
            }
        } else {
            // Folding again before it was unfolded (going to a session opened its workspace):
            // what was open before the first fold is still what comes back.
            if self.folded_before.is_none() {
                self.folded_before = Some(self.collapsed.clone());
            }
            self.collapsed.extend(self.tree().fold_keys());
            let again = self.keymap.key_in(Table::Nav, "sidebar.fold_all");
            self.toast(match again {
                Some(k) => format!("sidebar folded ({k} again unfolds it)"),
                None => "sidebar folded".to_owned(),
            });
        }
        // The row it was on, else the nearest one above it still shown: what it folded into.
        let rows = self.rows();
        let i = before
            .get(..=at.min(before.len().saturating_sub(1)))
            .unwrap_or_default()
            .iter()
            .rev()
            .find_map(|r| rows.iter().position(|x| x == r))
            .unwrap_or(0);
        self.set_sel(&rows, i);
    }

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

    /// Stop, kill, or stop and remove a running session: `ui.confirm_kill` asks first, in the
    /// web's words (`removal::STOP`, `KILL`, `STOP_REMOVE`; SPEC §7.3).
    fn ask_end(&mut self, end: Confirmed) {
        let (verb, what, id) = match &end {
            Confirmed::Stop(id) => ("Stop", ptyro_core::removal::STOP, *id),
            Confirmed::Kill(id) => ("Kill", ptyro_core::removal::KILL, *id),
            Confirmed::StopRemove(id) => ("Stop and remove", ptyro_core::removal::STOP_REMOVE, *id),
            _ => return self.confirmed(end),
        };
        if !self.config.ui.confirm_kill {
            return self.confirmed(end);
        }
        let name = self.session(id).map(|s| s.name.clone()).unwrap_or_default();
        self.overlay =
            Some(Overlay::Confirm { text: format!("{verb} {name}? {what} [y/N]"), action: end });
    }

    fn session_action(&mut self, action: &str, id: SessionId) {
        let Some(info) = self.session(id).cloned() else { return };
        let addr = id.full();
        match action {
            // `x` takes the row away: on an ended session it removes it (owner, 2026-11-10), a
            // dormant one asking first, as remove does.
            "session.stop" if !info.state.is_live() => self.session_action("session.remove", id),
            // Kill never forgets: an ended session isn't running, and remove is offered.
            "session.kill" if !info.state.is_live() => self.toast_offer(
                format!("{} isn't running", info.name),
                false,
                session_offer("remove it", "session.remove", id),
            ),
            // A dormant agent can still be resumed: forgetting it asks, as the CLI's rm does.
            "session.remove" if info.state == ProcessState::Dormant => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "Remove the dormant session {}? {} [y/N]",
                        info.name,
                        ptyro_core::removal::DORMANT
                    ),
                    action: Confirmed::Remove(id),
                });
            }
            "session.remove" if !info.state.is_live() => self.confirmed(Confirmed::Remove(id)),
            "session.stop" => self.ask_end(Confirmed::Stop(id)),
            "session.kill" => self.ask_end(Confirmed::Kill(id)),
            "session.remove" => self.ask_end(Confirmed::StopRemove(id)),
            "session.restart" if info.state.is_live() => self.toast_offer(
                format!("{} is still running", info.name),
                false,
                session_offer("stop it", "session.stop", id),
            ),
            "session.restart" => self.request(
                Pending::Done(format!("restarted {}", info.name)),
                Request::Restart { addr },
            ),
            "session.rename" => {
                self.overlay = Some(Overlay::Prompt {
                    title: format!(
                        "Rename {}: its name, the address (C-t: its title, what people read)",
                        info.name
                    ),
                    text: bare(info.machine.as_deref(), &info.name).to_owned(),
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

    /// A worktree's title prompt, filled with its title.
    fn open_retitle_worktree(&mut self, id: &str) {
        let Some(w) = self.worktree(id).cloned() else { return };
        self.overlay = Some(Overlay::Prompt {
            title: format!("Title of {}: what people read (empty clears it)", w.qualified()),
            text: w.title.clone().unwrap_or_default(),
            kind: PromptKind::RetitleWorktree(w.id.clone()),
        });
    }

    /// The title prompt (UX1.24), filled with the current title.
    fn open_retitle(&mut self, id: SessionId) {
        let Some(info) = self.session(id) else { return };
        let current = info.title.clone().filter(|t| info.window_title.as_ref() != Some(t));
        self.overlay = Some(Overlay::Prompt {
            title: format!("Title of {}: what people read (empty clears it)", info.name),
            text: current.unwrap_or_default(),
            kind: PromptKind::Retitle(id),
        });
    }

    pub(crate) fn client_env() -> BTreeMap<String, String> {
        ptyro_core::os::CLIENT_ENV
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
    pub(crate) fn free_workspace_name(&self) -> String {
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
            Some(n) => (2..)
                .map(|k| ptyro_core::names::numbered(n, k))
                .find(|c| !taken(c))
                .expect("infinite"),
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
            Focus::Pane if self.in_drawer() => Row::Quick(self.focused_session()?),
            Focus::Pane => Row::Session(self.focused_session()?),
        };
        let with = |action: &str, what: &str| self.hint_key(action).map(|k| format!("{k} {what}"));
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
                if self.attention_order(false).iter().any(|a| *a != id) {
                    return with("attention.next", "who needs you");
                }
                None
            }
            Row::Worktree(_) => with("session.new", "start a task here"),
            // Its main checkout is gone (SPEC §10.1): find it, or forget it.
            Row::Project(id) if self.project_missing(&id) => {
                let p = self.project(&id)?;
                let what = match &p.moved_to {
                    Some(to) => format!("moved to {}? ", to.display()),
                    None => "its directory is gone: ".into(),
                };
                Some(format!(
                    "{what}{}",
                    with("row.menu", "move it, or forget it").unwrap_or_default()
                ))
            }
            Row::Project(_) => with("worktree.new", "new worktree"),
            Row::Shells(_) => Some(match with("shells.new", "a new shell") {
                Some(l) => format!("⏎ fold · {l} · {} jumps", self.shells_jump_hint()),
                None => "⏎ fold".into(),
            }),
            Row::ShellNew(_) => Some("⏎ start a shell here".into()),
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
            Row::Chat(id) => {
                let c = self.chat(&id)?;
                if c.attention.is_some() {
                    return Some("⏎ open it: it needs you".into());
                }
                Some(format!(
                    "⏎ open · {}",
                    with("chat.pin", if c.pinned { "unpin" } else { "pin" }).unwrap_or_default()
                ))
            }
            Row::Chats(_) => Some(match with("chat.list", "every chat here") {
                Some(l) => format!("⏎ fold · {l}"),
                None => "⏎ fold".into(),
            }),
            Row::ChatAll(_) => with("chat.list", "every chat here").map(|l| format!("⏎ {l}")),
            Row::ChatNew(_) => Some("⏎ start a chat".into()),
            Row::Group(_) => None,
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
            "project.remove" | "project.rename" | "project.untrust" | "project.pin"
            | "project.unpin" | "project.leave" | "project.workspaces" | "project.move" => {
                let Some(p) = project.and_then(|p| self.project(&p).cloned()) else {
                    self.pick_project(None);
                    return;
                };
                match action {
                    "project.remove" => {
                        self.overlay = Some(Overlay::Confirm {
                            text: format!(
                                "Forget project {}? {} [y/N]",
                                p.slug,
                                ptyro_core::removal::PROJECT
                            ),
                            action: Confirmed::Forget(p.id),
                        });
                    }
                    "project.rename" => {
                        self.overlay = Some(Overlay::Prompt {
                            title: format!(
                                "Rename project {} (its address: {}/name finds its worktrees)",
                                p.slug, p.slug
                            ),
                            text: bare(p.machine.as_deref(), &p.slug).to_owned(),
                            kind: PromptKind::RenameProject(p.id),
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
                    "project.workspaces" => self.open_project_workspaces(&p),
                    "project.move" => self.open_project_move(&p),
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
            "worktree.rename" => match worktree {
                // The main checkout's name is always @root: only its title changes.
                Some(w) if w.is_root => self.open_retitle_worktree(&w.id),
                Some(w) => {
                    self.overlay = Some(Overlay::Prompt {
                        title: format!(
                            "Rename {}: its name, the address; branch and directory stay (C-t: its title, what people read)",
                            w.qualified()
                        ),
                        text: w.name.clone(),
                        kind: PromptKind::RenameWorktree(w.id),
                    });
                }
                None => need_wt(self),
            },
            "workspace.rename" => {
                let machine = self.workspace_machine(&self.workspace);
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Rename workspace {}", self.workspace),
                    text: bare(machine.as_deref(), &self.workspace).to_owned(),
                    kind: PromptKind::RenameWorkspace(self.workspace.clone()),
                });
            }
            "workspace.remove" => {
                let name = self.workspace.clone();
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "Remove workspace {name}? {} [y/N]",
                        ptyro_core::removal::WORKSPACE
                    ),
                    action: Confirmed::Send(
                        Box::new(Request::WorkspaceRemove { name }),
                        "workspace removed".into(),
                    ),
                });
            }
            // Every option `ptyro profile new` takes, on one screen (SPEC §11.5a).
            "profile.new" => self.open_profile_form("", None),
            // The CLI's creating commands, every option on one screen (SPEC §11.5a).
            "remote.pair" => self.open_pair_form(),
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
                        Pending::Done("muted for 1h (rows still show what wants you)".into()),
                        Request::Mute { target, minutes: Some(60), unmute: false },
                    ),
                    None => self.pick_project(None),
                }
            }
            other => self.toast(format!("{other}: not available yet")),
        }
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
                v.extend(["session.rename", "session.explain", "session.events"]);
                if s.kind == SessionKind::Agent && s.state.is_live() {
                    v.extend(["session.hibernate", "session.mute"]);
                }
                if !s.private {
                    v.push("session.private");
                }
                if s.kind == SessionKind::Agent {
                    v.push("kb.history");
                }
                if s.worktree.is_some() {
                    v.extend(["worktree.open", "script.run", "script.pick"]);
                }
                // SHELLS (SPEC §23.8): its number, a watch, back into a worktree; or there.
                if s.loose {
                    v.extend(["shells.move", "shells.watch", "shells.adopt"]);
                    // Its window (SPEC §23.17): out to a number of its own, or into another.
                    if s.layout.is_some() {
                        v.push("shells.break");
                    }
                    v.push("shells.join");
                } else if !s.quick && s.machine.is_none() {
                    v.push("shells.loose");
                }
                v.push("pane.split_right");
                if crate::tab::plan(crate::tab::Place::Tab, "", None, &crate::tab::here()).is_ok() {
                    v.extend(["session.tab", "session.pane"]);
                }
                if s.state.is_live() {
                    v.extend(["session.stop", "session.kill"]);
                }
                v.push("session.remove");
                v
            }
            Row::Worktree(w) => {
                let root = self.worktree(w).is_some_and(|w| w.is_root);
                let mut v =
                    vec!["session.new", "session.again", "shell.new", "shell.drawer", "shell.pane"];
                v.extend(["worktree.open", "script.run", "script.pick", "worktree.rename"]);
                v.push("kb.history");
                if !root {
                    v.push("worktree.remove");
                }
                v
            }
            // One whose main checkout is gone leads with finding it, or forgetting it (SPEC §10.1).
            Row::Project(id) if self.project_missing(id) => vec![
                "project.move",
                "project.remove",
                "project.show",
                "project.workspaces",
                "project.leave",
                "project.rename",
            ],
            Row::Project(_) => vec![
                "session.new",
                "session.again",
                "worktree.new",
                "shell.new",
                "shell.drawer",
                "shell.pane",
                "project.trust",
                "project.workspaces",
                "project.rename",
                "script.pick",
                "worktree.open",
                "project.move",
                "project.leave",
                "project.remove",
                "kb.history",
            ],
            // A quick terminal (UX1.25): show or hide it, and what a shell offers.
            Row::Quick(id) => {
                let Some(s) = self.session(*id) else { return Vec::new() };
                let mut v = vec!["shell.drawer", "shell.pane", "shell.new"];
                if s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked) {
                    v.push("peek");
                }
                v.extend(["session.rename", "session.explain", "session.stop"]);
                v
            }
            // A workspace (MM1.B): start something in it, set it up; another machine's adds what
            // its link can do.
            Row::Workspace(w) => {
                let mut v = vec!["session.new", "session.again", "worktree.new", "project.add"];
                if let Some(m) = self.machine_of_name(w) {
                    if m.state != ptyro_core::model::MachineState::Online {
                        v.insert(0, "machine.retry");
                    }
                    // On an older ptyro than this TUI's daemon: offered first (SPEC §24.8).
                    let ours = self.client.as_ref().map(|c| c.daemon_version.clone());
                    if let (Some(theirs), Some(ours)) = (m.version.as_deref(), ours)
                        && crate::restart::newer(&ours, theirs)
                    {
                        v.insert(0, "machine.update");
                    }
                    v.extend(["machine.hide", "machine.remove"]);
                } else {
                    v.extend(["workspace.rename", "workspace.profile", "workspace.notify"]);
                }
                v.extend(["machine.add", "machine.list"]);
                v
            }
            Row::Chat(id) => self.chat_actions_for(id),
            Row::Chats(_) | Row::ChatNew(_) | Row::ChatAll(_) => {
                vec!["chat.new", "chat.list"]
            }
            Row::Shells(_) | Row::ShellNew(_) => {
                vec!["shells.new", "shells.scratch", "shells.menu"]
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
            Row::Workspace(w) => Some(format!("Workspace {}", crate::ui::workspace_label(w))),
            Row::Chat(id) => self.chat(id).map(|c| format!("Chat {}", c.label())),
            Row::Chats(_) | Row::ChatNew(_) | Row::ChatAll(_) => Some("Chats".into()),
            Row::Shells(_) | Row::ShellNew(_) => Some("Shells".into()),
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
            // Every option on one form (SPEC §11.5a), what was typed carried along.
            KeyCode::Char { c: 'o' } if ctrl => {
                let prompt = (!text.trim().is_empty()).then(|| text.trim().to_owned());
                self.open_session_form(Wizard {
                    prompt,
                    name: name.filter(|n| !n.is_empty()),
                    ..w
                });
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
        if project.is_some() {
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
    pub(crate) fn wizard_done(&mut self, w: Wizard, target: Target) {
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

    pub(crate) fn start_in(&mut self, w: Wizard, worktree: Option<String>) {
        match w.agent.as_deref() {
            // The form gave the command line: run it; else ask for one.
            Some("@command") if w.command.is_some() => {
                let cwd = if worktree.is_some() { Default::default() } else { self.here() };
                let spec = SpawnSpec {
                    command: w.command.clone(),
                    worktree,
                    cwd,
                    profile: w.profile.clone(),
                    name: w.name.clone().filter(|n| !n.is_empty()),
                    title: w.title.clone(),
                    private: w.private,
                    ..SpawnSpec::default()
                };
                self.spawn_spec(spec, w.split);
            }
            Some("@command") => self.prompt_command(Wizard { worktree, ..w }),
            Some("@shell") | None => {
                let cwd = if worktree.is_some() { Default::default() } else { self.here() };
                let spec = SpawnSpec {
                    cwd,
                    worktree,
                    profile: w.profile.clone(),
                    name: w.name.clone().filter(|n| !n.is_empty()),
                    title: w.title.clone(),
                    private: w.private,
                    ..SpawnSpec::default()
                };
                self.spawn_spec(spec, w.split);
            }
            Some(a) => {
                let cwd = if worktree.is_some() { Default::default() } else { self.here() };
                self.spawn_spec(
                    SpawnSpec {
                        agent: Some(a.to_owned()),
                        cwd,
                        worktree,
                        prompt: w.prompt.clone(),
                        title: w.title.clone().or_else(|| w.prompt.clone()),
                        name: w.name.clone().filter(|n| !n.is_empty()),
                        profile: w.profile.clone(),
                        private: w.private,
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
        // Under a tab, its machine is the one picked (SPEC §11.2k).
        let at = match self.scope_shown().and_then(|s| s.machine.as_ref()) {
            Some(m) => self.machines.iter().position(|x| x.name == *m).map_or(0, |i| i + 1),
            None => 0,
        };
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("New workspace — on which machine?", items)
                .with_query(String::new(), at),
            purpose: Purpose::NewWorkspaceOn,
        });
    }

    fn open_editors(&mut self, wt: String) {
        // On another computer the paths are the daemon's: it lists its editors, and opens them.
        if self.connect.remote.is_some() {
            self.request(Pending::RemoteEditors(wt), Request::Editors);
            return;
        }
        let default = self.config.default_editor();
        let names = self.config.editor_names();
        self.editor_picker(wt, names, &default, false);
    }

    /// The "open in" list: `names` (the default first), then a new shell. `remote`: the daemon's
    /// editors, which open on its computer.
    fn editor_picker(&mut self, wt: String, mut names: Vec<String>, default: &str, remote: bool) {
        names.sort_by_key(|n| n != default);
        // Only what's installed (editor_names), then a new shell: `!` picks it, as it starts one
        // anywhere else.
        let mut items: Vec<Item> = names
            .into_iter()
            .filter_map(|n| {
                if remote {
                    let how = if n == ptyro_core::config::FILE_MANAGER {
                        "shows the folder, on the computer running the daemon"
                    } else {
                        "opens on the computer running the daemon"
                    };
                    return Some(Item {
                        label: n.clone(),
                        detail: how.into(),
                        key: None,
                        target: Target::Editor(n),
                    });
                }
                let def = self.config.editor(&n)?;
                let how = if def.inside {
                    "opens here, as a session"
                } else if n == ptyro_core::config::FILE_MANAGER {
                    "shows the folder in the file manager"
                } else if ptyro_core::config::TERMINAL_APPS.contains(&n.as_str()) {
                    "opens a terminal window there"
                } else {
                    "opens its own window"
                };
                Some(Item {
                    label: n.clone(),
                    detail: format!("{} — {how}", def.command.join(" ")),
                    key: None,
                    target: Target::Editor(n),
                })
            })
            .collect();
        items.push(Item {
            label: "shell".into(),
            detail: "a new shell in it, as a session".into(),
            key: Some("!".into()),
            target: Target::NewShell,
        });
        let title = format!(
            "Open {} in",
            self.worktree(&wt).map_or_else(String::new, WorktreeInfo::qualified)
        );
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items).with_hotkeys(),
            purpose: Purpose::Editor(wt),
        });
    }

    fn open_editor(&mut self, wt: &str, name: &str) {
        if self.connect.remote.is_some() {
            self.request(
                Pending::EditorOpened(name.to_owned()),
                Request::WorktreeOpen { addr: wt.to_owned(), editor: Some(name.to_owned()) },
            );
            return;
        }
        let (Some(def), Some(w)) = (self.config.editor(name), self.worktree(wt).cloned()) else {
            return;
        };
        let argv = def.argv(&w.path);
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
                let w = Wizard { project: Some(project), worktree: root, ..*w };
                self.resolve_for(&w);
                self.open_targets(w);
            }
        }
        self.context_hint = None;
    }

    /// Its main checkout is gone (its `@root` missing), or ptyro found it moved (SPEC §10.1).
    pub(crate) fn project_missing(&self, project: &str) -> bool {
        self.project(project).is_some_and(|p| p.moved_to.is_some())
            || self.worktrees.iter().any(|w| {
                w.project == project
                    && w.is_root
                    && w.state == ptyro_core::model::WorktreeState::Missing
            })
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
        if then.is_none() {
            items.push(Item {
                label: "+ another path…".into(),
                detail: "type where the repository is".into(),
                key: None,
                target: Target::NewProjectPath,
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
        let live: Vec<&SessionInfo> = self.sessions.iter().filter(|s| s.state.is_live()).collect();
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
                "Remove worktree {} ({})? {} {}{note} [y/N]",
                w.qualified(),
                w.branch.as_deref().unwrap_or("-"),
                ptyro_core::removal::WORKTREE,
                ptyro_core::removal::SAFE,
            ),
            action: Confirmed::RemoveWorktree { id: w.id.clone(), force: false },
        });
    }

    pub(crate) fn session_items(&self, include_new: bool) -> Vec<Item> {
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
        for c in self.chat_list.iter().filter(|c| c.workspace == self.workspace) {
            items.push(Item {
                label: format!("chat {}", c.label()),
                detail: format!("{} · {}", c.agent, crate::ui::ago(c.last_ms)),
                key: None,
                target: Target::Chat(c.id.clone()),
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
        // The tabs over the sidebar, while there is a strip (SPEC §11.2k).
        let tabs = self.tabs();
        if ptyro_core::scope::drawn(&tabs) {
            let scopes = std::iter::once(None).chain(tabs.iter().map(|t| Some(t.scope.clone())));
            for scope in scopes {
                let label = scope_label(&tabs, scope.as_ref());
                items.push(Item {
                    label: format!("tab {label}"),
                    detail: if scope.as_ref() == self.scope_shown() {
                        "the tab shown".into()
                    } else if scope.is_none() {
                        "the sidebar lists every machine and profile".into()
                    } else {
                        format!("the sidebar lists only {label}'s sessions")
                    },
                    key: None,
                    target: Target::Tab(scope),
                });
            }
        }
        // The built-in themes: one pick switches and keeps it (`ui.theme`).
        for t in crate::theme::BUILTIN {
            items.push(Item {
                label: format!("theme {t}"),
                detail: if *t == self.theme_name {
                    "the theme in use".into()
                } else {
                    "switch to it (kept as ui.theme)".into()
                },
                key: None,
                target: Target::Theme((*t).into()),
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
            Overlay::Peek { session, screen, reply } => self.peek_key(key, session, screen, reply),
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
            Overlay::Form(form) => self.form_key(form, key),
            // The clean-up list's C-k: keep that worktree; the list goes on without it.
            Overlay::Picker { mut palette, purpose: Purpose::Gc }
                if key.code == KeyCode::Char { c: 'k' } && key.mods.has(Mods::CTRL) =>
            {
                if let Some(Target::Worktree(id)) = palette.selected_target().cloned() {
                    self.request(Pending::GcKept, Request::GcKeep { worktree: id.clone() });
                    palette.retain(|it| it.target != Target::Worktree(id.clone()));
                }
                if !palette.filtered.is_empty() {
                    self.overlay = Some(Overlay::Picker { palette, purpose: Purpose::Gc });
                }
            }
            Overlay::Picker { mut palette, purpose } => match palette.key(key) {
                Picked::Stay => self.overlay = Some(Overlay::Picker { palette, purpose }),
                Picked::Close => {}
                Picked::Pick(target) => self.picked(target, purpose),
            },
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
                    let text = kind.tidy(&text).unwrap_or(text);
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
                    PromptKind::RenameWorktree(id) => self.open_retitle_worktree(&id),
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
                (KeyCode::Char { c: 'n' | 'N' }, Confirmed::KeepPin(id)) => {
                    let id = id.clone();
                    self.request(
                        Pending::Done("unpinned".into()),
                        Request::ProjectPin { addr: id, profile: None },
                    );
                }
                (KeyCode::Char { c: 'y' | 'Y' }, _) => self.confirmed(action),
                // A new chat's first message not agreed to: no chat, the message back to edit.
                (
                    KeyCode::Char { c: 'n' | 'N' } | KeyCode::Esc | KeyCode::Enter,
                    Confirmed::Again(_, then),
                ) => {
                    if let Pending::ChatMade { text, token } = then.as_ref() {
                        let why = "not sent: no chat was made (the message is back to edit)";
                        self.chat_not_made(*token, text.clone(), why.into());
                    }
                }
                // The setup checklist's split declined: it never starts, so the run is over.
                (
                    KeyCode::Char { c: 'n' | 'N' } | KeyCode::Esc | KeyCode::Enter,
                    Confirmed::Spawn(spec, _),
                ) => {
                    let name = spec.name.clone();
                    self.setup_spawned(name.as_deref(), false);
                }
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
        panel.remote = self.connect.remote.clone();
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
                A::Close => self.overlay = None,
                A::Reload => self.reload_config_with(false),
                A::Toast(s) => self.toast(s),
                A::ToastErr(s) => self.toast_err(s),
                A::Run(id) => {
                    self.overlay = None;
                    self.run(&Command { action: id, arg: None });
                }
                // Tools run where the daemon runs: a remote one's file is the one to edit there.
                A::EditFile(..) | A::Daemon(None) if self.connect.remote.is_some() => {
                    self.overlay = None;
                    self.spawn_tool(
                        vec![self.daemon_ptyro(), "config".into(), "edit".into()],
                        None,
                        true,
                    );
                    self.toast(self.daemon_file_note());
                }
                A::Daemon(at) => {
                    self.overlay = None;
                    let mut argv = vec![self.daemon_ptyro(), "config".into(), "edit".into()];
                    argv.extend(at);
                    self.spawn_tool(argv, None, true);
                    self.toast(self.daemon_file_note());
                }
                A::EditFile(path, line) => {
                    self.overlay = None;
                    self.edit_file(path, line);
                }
            }
        }
    }

    /// ptyro as the daemon's computer runs it, for a tool session (those run there): this binary
    /// when the daemon is this computer's, else that computer's own (`PTYRO_REMOTE_BIN`, as the
    /// proxy finds it), never this one's path.
    pub(crate) fn daemon_ptyro(&self) -> String {
        if self.connect.remote.is_some() {
            return std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
        }
        std::env::current_exe()
            .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned())
    }

    fn daemon_file_note(&self) -> String {
        format!(
            "the config file of the daemon on {}: what it reads (the screen's own settings stay in this computer's)",
            self.connect.remote.as_deref().unwrap_or("this computer")
        )
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
            Confirmed::Forget(id) => self.request(
                Pending::Forget(id.clone()),
                Request::ProjectRemove { addr: id, release: false },
            ),
            Confirmed::Again(mut req, then) => {
                if let Request::ChatTask { confirmed, .. }
                | Request::ChatUpdate { confirmed, .. }
                | Request::KbForget { confirmed, .. }
                | Request::KbRemove { confirmed, .. } = req.as_mut()
                {
                    *confirmed = true;
                }
                self.request(*then, *req);
            }
            Confirmed::MeaningOn => self.meaning_on(),
            Confirmed::RestartDaemon => {
                self.run_cli(&["daemon".into(), "restart".into(), "--force".into()]);
            }
            Confirmed::Quit => {
                self.quitting = true;
                self.toast("stopping ptyro…");
                self.request(Pending::Quit, Request::Shutdown { force: true });
            }
            Confirmed::Stop(id) | Confirmed::Kill(id) | Confirmed::StopRemove(id) => {
                let name = self.session(id).map(|s| s.name.clone()).unwrap_or_default();
                let addr = id.full();
                let (said, req) = match action {
                    Confirmed::Stop(_) => ("stopped", Request::Kill { addr, signal: None }),
                    Confirmed::Kill(_) => ("killed", Request::Kill { addr, signal: Some(9) }),
                    _ => ("stopped and removed", Request::Remove { addr, stop: true }),
                };
                self.stopping.insert(id);
                self.request(Pending::Done(format!("{said} {name}")), req);
            }
            Confirmed::Remove(id) => {
                let name = self.session(id).map(|s| s.name.clone()).unwrap_or_default();
                self.request(
                    Pending::Done(format!("removed {name}")),
                    Request::Remove { addr: id.full(), stop: false },
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
        }
    }

    pub(crate) fn picked(&mut self, target: Target, purpose: Purpose) {
        if let Purpose::Split(dir) = purpose {
            return match target {
                Target::Clone => self.split_clone(dir),
                Target::NewShell => self.split_shell(dir),
                Target::Session(id) => self.show(id, Some(dir)),
                _ => {}
            };
        }
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
                self.request(Pending::Branches(Box::new(w)), Request::Branches { project });
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
            (Target::NewWorkspace, _) => self.open_workspace_form(None),
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
                self.open_workspace_form((!m.is_empty()).then_some(m))
            }
            (Target::Editor(name), Purpose::Editor(wt)) => self.open_editor(&wt, &name),
            (Target::NewShell, Purpose::Editor(wt)) => {
                self.spawn_spec(SpawnSpec { worktree: Some(wt), ..SpawnSpec::default() }, None)
            }
            (Target::Script(name), Purpose::Script(wt)) => self.run_script(wt, Some(name)),
            (Target::Worktree(id), Purpose::Gc) => {
                if let Some(w) = self.worktree(&id).cloned() {
                    self.confirm_remove(&w);
                }
            }
            (Target::Path(path), Purpose::Gc) => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "Delete the scratch directory {}? {} [y/N]",
                        path.display(),
                        ptyro_core::removal::SCRATCH
                    ),
                    action: Confirmed::Send(
                        Box::new(Request::ShellScratchRemove { path }),
                        "deleted".into(),
                    ),
                });
            }
            (Target::Path(v), Purpose::Library(f)) => self.library_picked(f, &v.to_string_lossy()),
            (Target::Path(path), Purpose::KbIngest) => self.open_ingest_form(Some(path)),
            (Target::NewProjectPath, Purpose::KbIngest) => self.open_ingest_form(None),
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
            // Picked on its own: where it goes, on the form (SPEC §11.5a).
            (Target::Path(path), Purpose::AddProject(None)) => self.open_project_form(Some(path)),
            (Target::Path(path), purpose) => {
                let then = match purpose {
                    Purpose::AddProject(then) => then,
                    _ => None,
                };
                let workspace = Some(self.workspace.clone());
                self.request(
                    Pending::ProjectAdded(then),
                    Request::ProjectAdd { path, workspace, moved: false, separate: false },
                );
            }
            (Target::NewProjectPath, _) => self.open_project_form(None),
            (Target::AddProject, Purpose::Target(w)) => {
                self.pick_project(Some(AfterAdd::Targets(Box::new(w))))
            }
            (target, Purpose::Home) => self.home_picked(target),
            (Target::Fix(i), Purpose::Health) => self.fix(i),
            (Target::Agent(a), Purpose::ChatAgent(id)) => self.chat_picked(a, id),
            (Target::Chat(v), p @ (Purpose::ChatCite | Purpose::ChatSlash)) => self.chat_pick(p, v),
            (Target::Chat(id), _) => self.open_chat(&id),
            (Target::Agent(p), Purpose::ChatTaskProject(chat, t)) => {
                self.chat_task(chat, crate::chats::TaskArgs { project: Some(p), ..t })
            }
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
            (Target::Action(cmd), Purpose::ShellWatch(id)) if cmd.action == "shells.watch_set" => {
                self.shell_watch_picked(id, cmd.arg.as_deref().unwrap_or("done"));
            }
            (Target::Action(cmd), _) => self.run(&cmd),
            // From the palette: go to a worktree (its first session) or a project.
            (Target::Worktree(id), _) => {
                let rows = self.rows_with(&Row::Worktree(id.clone()));
                if let Some(i) = rows.iter().position(|r| *r == Row::Worktree(id.clone())) {
                    self.set_sel(&rows, i);
                }
                // One the tab (switched to the worktree's, if it had to) lists, so it doesn't
                // switch again.
                let shown = self.scope_shown();
                let mut there =
                    self.sessions.iter().filter(|s| s.worktree.as_deref() == Some(id.as_str()));
                let first = there.clone().next();
                match there.find(|s| shown.is_none_or(|t| t.keeps_session(s))).or(first) {
                    Some(s) => {
                        let sid = s.id;
                        self.show(sid, None);
                    }
                    None => self.focus = Focus::Sidebar,
                }
            }
            (Target::Project(id), _) => {
                self.collapsed.remove(&id);
                let rows = self.rows_with(&Row::Project(id.clone()));
                if let Some(i) = rows.iter().position(|r| *r == Row::Project(id.clone())) {
                    self.set_sel(&rows, i);
                }
                self.focus = Focus::Sidebar;
            }
            (Target::RunScript(wt, name), _) => self.run_script(wt, Some(name)),
            (Target::Session(id), _) => self.pick(id),
            (Target::Setting(key), _) => self.open_settings(Some(&key)),
            (Target::Tab(scope), _) => self.set_scope(scope),
            (Target::Theme(name), _) => {
                let done = if no_color() {
                    format!("theme {name} kept as ui.theme; NO_COLOR is set, so there is no colour")
                } else {
                    format!("theme {name} (kept as ui.theme)")
                };
                self.set_config("ui.theme", &name, &done);
            }
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
                let me = self.session(*id);
                let machine = me.and_then(|s| s.machine.as_deref());
                if t.is_empty() || me.is_some_and(|s| bare(machine, &s.name) == t) {
                    return Ok(Some("unchanged".into()));
                }
                name(t)?;
                // Unique per project (SPEC §2.3): another project's session may share it.
                let project = me.and_then(|s| s.project.clone());
                if self.sessions.iter().any(|s| {
                    s.id != *id
                        && s.machine.as_deref() == machine
                        && bare(machine, &s.name) == t
                        && s.project == project
                }) {
                    return Err(match project.as_deref().and_then(|p| self.project(p)) {
                        Some(p) => format!("a session named {t} exists in {}", p.slug),
                        None => format!("a session named {t} exists"),
                    });
                }
            }
            PromptKind::RenameWorkspace(old) => {
                let machine = self.workspace_machine(old);
                let old = bare(machine.as_deref(), old);
                if old == "main" {
                    return Err("the main workspace can't be renamed".into());
                }
                if t.is_empty() || t == old {
                    return Ok(Some("unchanged".into()));
                }
                name(t)?;
                if self.workspaces.iter().any(|w| w.name == on_machine(machine.as_deref(), t)) {
                    return Err(format!("workspace {t} exists"));
                }
            }
            // Unique among the project's worktrees (SPEC §2.3).
            PromptKind::RenameWorktree(id) => {
                let Some(wt) = self.worktree(id) else { return Ok(None) };
                if t.is_empty() || t == wt.name {
                    return Ok(Some("unchanged".into()));
                }
                name(t)?;
                if self
                    .worktrees
                    .iter()
                    .any(|w| w.id != wt.id && w.project == wt.project && w.name == t)
                {
                    return Err(format!("{}/{t} exists", wt.project_slug));
                }
            }
            PromptKind::RenameProject(id) => {
                let Some(p) = self.project(id) else { return Ok(None) };
                let machine = p.machine.as_deref();
                if t.is_empty() || t == bare(machine, &p.slug) {
                    return Ok(Some("unchanged".into()));
                }
                if let Some(p) = ptyro_core::names::slug_problem(t) {
                    return Err(p);
                }
                if self.projects.iter().any(|o| o.id != p.id && o.slug == on_machine(machine, t)) {
                    return Err(format!("a project named {t} exists"));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn prompt_done(&mut self, text: String, kind: PromptKind) {
        match kind {
            PromptKind::Command(w) => {
                // Run through the daemon's shell, so pipes, quotes and $VARS work as typed;
                // nothing typed is a shell.
                let command = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
                let cwd =
                    if w.worktree.is_some() { std::path::PathBuf::new() } else { self.here() };
                self.spawn_spec(
                    SpawnSpec { command, worktree: w.worktree, cwd, ..SpawnSpec::default() },
                    w.split,
                );
            }
            PromptKind::ChatEdit(chat, node) => {
                if !text.trim().is_empty() {
                    self.request(
                        Pending::Done("said differently: it and its answer are replaced".into()),
                        Request::ChatEdit { chat, node, text },
                    );
                }
            }
            PromptKind::ChatFindIn(chat) => self.find_in_chat(&chat, text.trim()),
            PromptKind::KbFind => self.library_find(&text),
            kind @ (PromptKind::KbShow | PromptKind::KbForget) => self.library_typed(&kind, &text),
            PromptKind::ChatFind => {
                if !text.trim().is_empty() {
                    self.request(
                        Pending::ChatOpen(self.chat_shown_key()),
                        Request::ChatFind {
                            question: text.trim().to_owned(),
                            agent: None,
                            workspace: None,
                        },
                    );
                }
            }
            PromptKind::ChatFolder(id) => {
                if !text.trim().is_empty() {
                    self.chat_folder(id, text.trim());
                }
            }
            PromptKind::ChatRetitle(id) => {
                self.request(
                    Pending::Done("retitled".into()),
                    Request::ChatUpdate {
                        chat: id,
                        title: Some(text.trim().to_owned()),
                        pinned: None,
                        archived: None,
                        agent: None,
                        folders: None,
                        confirmed: false,
                        auto_title: false,
                    },
                );
            }
            PromptKind::ShellMove(id) => self.shell_move_typed(id, &text),
            PromptKind::ShellJoin(id) => self.window_join_typed(id, &text),
            PromptKind::ShellMatch(id) => {
                let re = text.trim().to_owned();
                if !re.is_empty() {
                    self.set_watch(id, Some(ptyro_core::session::Watch::Match { re }));
                }
            }
            PromptKind::Retitle(id) => {
                let title = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
                let done = if title.is_some() { "retitled" } else { "title cleared" };
                self.request(
                    Pending::Done(done.into()),
                    Request::Retitle { addr: id.full(), title, worktree: false },
                );
            }
            PromptKind::CliAsk(job) => self.cli_answer(*job, &text),
            PromptKind::RenameWorkspace(old) => {
                let machine = self.workspace_machine(&old);
                let new_name = on_machine(machine.as_deref(), text.trim());
                if !text.trim().is_empty() && new_name != old {
                    // The name changes here once the daemon has accepted it: it refuses `main`
                    // and bad names, and a rename that failed must not leave this TUI on a
                    // workspace that doesn't exist.
                    self.request(
                        Pending::WorkspaceRenamed(new_name.clone()),
                        Request::WorkspaceRename { name: old, new_name },
                    );
                }
            }
            PromptKind::RenameWorktree(id) => {
                let name = text.trim().to_owned();
                if !name.is_empty() && self.worktree(&id).is_some_and(|w| w.name != name) {
                    self.request(
                        Pending::Done(format!("renamed to {name}")),
                        Request::WorktreeRename { addr: id, name },
                    );
                }
            }
            PromptKind::RenameProject(id) => {
                let name = text.trim().to_owned();
                if !name.is_empty()
                    && self
                        .project(&id)
                        .is_some_and(|p| bare(p.machine.as_deref(), &p.slug) != name)
                {
                    self.request(
                        Pending::Done(format!("renamed to {name}")),
                        Request::ProjectRename { addr: id, name },
                    );
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
                        once: None,
                    };
                    self.request(Pending::Done(String::new()), req);
                }
                self.toast(format!("sent to {} session(s), each once it's ready", ids.len()));
                self.marked.clear();
            }
            PromptKind::Rename(id) => {
                let name = text.trim().to_owned();
                let same =
                    self.session(id).is_some_and(|s| bare(s.machine.as_deref(), &s.name) == name);
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
        self.last_input = Instant::now();
        let before = self.quick_stance();
        self.mouse_event(m);
        self.tuck_away(before);
    }

    fn mouse_event(&mut self, m: &crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind as K;
        self.dirty = true;
        let (x, y) = (m.column, m.row);
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
        // A number on a shell's header strip shows that shell; its `＋` starts one.
        if matches!(m.kind, K::Down(crossterm::event::MouseButton::Left)) && self.strip_click(x, y)
        {
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
                    let head = crate::ui::sidebar_head(self);
                    // (Not the tabs, the title, the filter strip or the hint line: no row.)
                    let row = y
                        .checked_sub(side.y + head)
                        .filter(|l| *l < side.height.saturating_sub(head + 1))
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
                    let head = crate::ui::sidebar_head(self);
                    // The tab strip over the title picks a machine and profile (SPEC §11.2k).
                    if head > 2 && y == side.y {
                        self.tab_click(x - side.x, side.width);
                        return;
                    }
                    // The filter strip under the title picks what the sidebar lists.
                    if y == side.y + head - 1 {
                        let col = x - side.x;
                        if let Some((show, ..)) = crate::ui::filter_chips(self, side.width)
                            .into_iter()
                            .find(|(_, _, from, to)| (*from..*to).contains(&col))
                        {
                            self.set_show(show);
                        }
                        return;
                    }
                    // The title line has no row; a row may span several lines (UX1.2).
                    let row = y
                        .checked_sub(side.y + head)
                        .filter(|l| *l < side.height.saturating_sub(head + 1))
                        .and_then(|l| body.lines.get(l as usize + body.skip))
                        .map_or(usize::MAX, |(i, _)| *i);
                    match rows.get(row).cloned() {
                        Some(Row::Session(id)) => {
                            self.set_sel(&rows, row);
                            self.pick(id);
                        }
                        Some(Row::Quick(id)) => {
                            self.set_sel(&rows, row);
                            self.toggle_quick(Some(id), None, None);
                        }
                        Some(r @ (Row::Chat(_) | Row::ChatNew(_) | Row::ChatAll(_))) => {
                            self.set_sel(&rows, row);
                            self.chat_row_open(&r);
                        }
                        // SHELLS: its `＋` starts a shell in that workspace (SPEC §23.6).
                        Some(Row::Shells(_)) if x - side.x > crate::chats::HEADING_PLUS => {
                            self.set_sel(&rows, row);
                            self.focus = Focus::Sidebar;
                            self.shells_new(None, false);
                        }
                        Some(Row::ShellNew(_)) => {
                            self.set_sel(&rows, row);
                            self.focus = Focus::Sidebar;
                            self.shells_new(None, false);
                        }
                        // Its `＋` starts a chat in that workspace; the rest of it folds.
                        Some(Row::Chats(_)) if x - side.x >= crate::chats::HEADING_PLUS => {
                            self.set_sel(&rows, row);
                            self.focus = Focus::Sidebar;
                            self.new_chat();
                        }
                        Some(
                            r @ (Row::Chats(_)
                            | Row::Shells(_)
                            | Row::Workspace(_)
                            | Row::Project(_)
                            | Row::Worktree(_)),
                        ) => {
                            self.set_sel(&rows, row);
                            let key = r.fold_key().unwrap_or_default();
                            let collapse = !self.collapsed.contains(&key);
                            self.fold_by_hand(key, collapse);
                        }
                        _ => {}
                    }
                }
                // The wheel over the tab strip moves among the tabs.
                K::ScrollDown | K::ScrollUp if y == side.y && crate::ui::sidebar_head(self) > 2 => {
                    self.wheel_tab(if matches!(m.kind, K::ScrollDown) { 1 } else { -1 });
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
        // The chat panel covers the panes: the wheel scrolls it, nothing reaches what's behind.
        if self.chat_panel.is_some() && self.main_area().contains((x, y).into()) {
            self.focus = Focus::Pane;
            let area = self.main_area();
            match &mut self.chat_panel {
                Some(crate::chats::ChatPanel::View(v)) => match m.kind {
                    K::ScrollUp => v.view.scroll_by(3),
                    K::ScrollDown => v.view.scroll_by(-3),
                    _ => {}
                },
                // A row clicked opens it; the wheel moves the selection.
                Some(crate::chats::ChatPanel::List(_)) => match m.kind {
                    K::Down(crossterm::event::MouseButton::Left) => {
                        if let Some(i) = crate::chats::list_row_at(self, area, y) {
                            self.list_clicked(i);
                        }
                    }
                    K::ScrollUp => self.chat_key(KeyEvent::new(KeyCode::Up, Mods::NONE)),
                    K::ScrollDown => self.chat_key(KeyEvent::new(KeyCode::Down, Mods::NONE)),
                    _ => {}
                },
                Some(crate::chats::ChatPanel::Library(_)) => match m.kind {
                    K::Down(crossterm::event::MouseButton::Left) => self.library_mouse(y, None),
                    K::ScrollUp => self.library_mouse(y, Some(true)),
                    K::ScrollDown => self.library_mouse(y, Some(false)),
                    _ => {}
                },
                Some(crate::chats::ChatPanel::Home(_)) => match m.kind {
                    K::Down(crossterm::event::MouseButton::Left) => self.home_mouse(y, None),
                    K::ScrollUp => self.home_mouse(y, Some(true)),
                    K::ScrollDown => self.home_mouse(y, Some(false)),
                    _ => {}
                },
                None => {}
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
            show: self.show,
            scope: self.scope_shown(),
            chats: &self.chat_list,
            recent: self.config.chat.enabled.then_some(self.config.chat.recent as usize),
            shells: self.config.shell.section,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.tree().rows()
    }

    /// The workspace's sessions in tree order (collapsed ones included).
    pub fn ordered(&self) -> Vec<SessionId> {
        self.tree().sessions()
    }

    /// Visible session rows, in order: what `1`–`9` pick. SHELLS have their own numbers
    /// (`Alt-1…9`, SPEC §23.6), so they aren't counted.
    pub fn numbered(&self) -> Vec<SessionId> {
        self.numbered_from(&self.rows())
    }

    pub fn numbered_from(&self, rows: &[Row]) -> Vec<SessionId> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Session(id) if !self.session(*id).is_some_and(|s| s.loose) => Some(*id),
                _ => None,
            })
            .collect()
    }

    pub fn worktree(&self, id: &str) -> Option<&WorktreeInfo> {
        self.worktrees.iter().find(|w| w.id == id)
    }

    /// The machine a workspace (by name) is mirrored from, if any.
    fn workspace_machine(&self, name: &str) -> Option<String> {
        self.workspaces.iter().find(|w| w.name == name).and_then(|w| w.machine.clone())
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
                Row::Group(_)
                | Row::Chats(_)
                | Row::Chat(_)
                | Row::ChatNew(_)
                | Row::ChatAll(_)
                | Row::Shells(_)
                | Row::ShellNew(_) => return (None, None),
            }
        }
        if let Some(s) = self.focused_session().and_then(|id| self.session(id)) {
            if s.project.is_some() {
                return (s.project.clone(), s.worktree.clone());
            }
            // A shell of SHELLS is nobody's worktree (SPEC §23.6): no project here.
            if s.loose {
                return (None, None);
            }
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
        let rows = self.rows_with(&Row::Workspace(name.clone()));
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
            self.sessions.iter().filter(|s| s.quick).map(|s| s.id).collect();
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
            self.show = saved.show;
            self.scope = saved.scope;
            self.restore_drawer(saved.drawer, saved.drawer_share);
            let known: Vec<SessionId> = ordered.iter().chain(&quick).copied().collect();
            self.restore_kept(saved.kept, &known);
        }
        self.settle_empty_panes(&ordered);
        self.sync();
    }

    // ── attention, peek (SPEC §9.4–§9.6, M4.5) ──────────────────────────────

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
        self.pick(id);
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

    pub(crate) fn open_peek(&mut self, id: SessionId) {
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
                        once: None,
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
    // A project in several workspaces has a row under each: the one nearest where it was.
    if let Some(i) = anchor.and_then(|a| {
        (0..rows.len()).filter(|i| rows[*i] == *a).min_by_key(|i| i.abs_diff(fallback))
    }) {
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
        setup: !w.no_setup,
        fetch: w.no_fetch.then_some(false),
        base: w.base.clone(),
        profile: w.profile.clone(),
        // The prompt goes to the agent; with no agent it's still the title.
        prompt: w.prompt.clone().filter(|_| agent.is_some()),
        title: w.title.clone().or_else(|| w.prompt.clone()),
        agent,
        // Where the work comes from (UX1.10).
        pr: w.pr,
        issue: w.issue.clone(),
        existing: w.branch.is_some(),
        branch: w.branch.clone(),
        private: w.private,
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
        app.first_run = Some(crate::onboard::FirstRun::default());
        app.on_mouse(&mouse(crossterm::event::MouseEventKind::ScrollUp, 60, 10));
        let p = &app.panes[&1];
        assert!(p.copy.is_none() && !p.loading, "no copy mode started behind the first run");
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
        assert_eq!(task(&app).1.as_deref().map(|n| n.ends_with("x-y")), Some(true));

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
        // A name can't hold a space: it becomes - (the rename prompt is a name).
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "ab-c"));
    }

    #[test]
    fn a_name_prompt_swaps_what_it_cant_hold_for_a_dash() {
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
        assert!(matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "my-feat-2"));
        app.on_paste(" a b".into());
        assert!(
            matches!(&app.overlay, Some(Overlay::Prompt { text, .. }) if text == "my-feat-2-a-b")
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

    /// An update replaced this binary and upgraded the daemon: the TUI restarts into it, but
    /// only when nothing of its own is half done (SPEC §24.8).
    #[test]
    fn a_replaced_binary_restarts_the_tui_once_it_is_idle() {
        let mut app = app();
        let quiet = || Instant::now() - Duration::from_secs(60);
        app.last_input = quiet();
        // The binary on disk doesn't answer the daemon's version: nothing happens.
        app.got_binary(Some("9.9.8".into()), "9.9.9");
        app.tick();
        assert!(app.restart_to.is_none() && app.quit.is_none());
        // It does, but a dialog is open: it waits.
        app.run(&Command { action: "app.quit", arg: None });
        app.got_binary(Some("9.9.9".into()), "9.9.9");
        assert_eq!(app.restart_to.as_deref(), Some("9.9.9"));
        app.tick();
        assert!(app.quit.is_none(), "waits for the dialog");
        app.overlay = None;
        // A chat draft is kept too: it waits.
        app.chat_drafts.insert("c1".into(), "half a thought".into());
        app.tick();
        assert!(app.quit.is_none(), "waits for the draft");
        app.chat_drafts.clear();
        // Someone typed a moment ago: it waits for a quiet moment.
        app.last_input = Instant::now();
        app.tick();
        assert!(app.quit.is_none(), "waits while someone types");
        app.last_input = quiet();
        app.tick();
        let Some(AttachEnd::Restart { from, to, workspace, session }) = &app.quit else {
            panic!("no restart")
        };
        assert_eq!((from.as_str(), to.as_str()), (crate::restart::OURS, "9.9.9"));
        assert_eq!(workspace, &app.workspace);
        assert!(session.is_none(), "not raw: the workspace's view comes back");
        // Started as another version than it is: it says so and never restarts by itself again.
        let mut odd = super::tests::app();
        odd.opts.restarted = Some(("0.0.1".into(), "9.9.9".into()));
        assert!(odd.restarted());
        odd.check_binary();
        assert!(odd.no_restart);
        // The version it already runs is no restart.
        let mut same = super::tests::app();
        same.got_binary(Some(crate::restart::OURS.into()), crate::restart::OURS);
        assert!(same.restart_to.is_none());
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
        assert!(!has("fix the flaky"), "no title line: the pane's header says it: {lines:#?}");
        assert!(has("wants Bash: npm i"), "{lines:#?}");
        assert!(!has("y/a/n") && !has("⏎ open"), "the key only on the row you're on: {lines:#?}");
        assert!(has("last All tests pass."), "{lines:#?}");
        assert!(has("last cargo test ✗101"), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("2.1.")), "never a version");

        // Compact: one line each.
        app.config.ui.sidebar_detail = ptyro_core::config::SidebarDetail::Compact;
        assert!(!rows_say(&app, "now Bash") && !rows_say(&app, "wants"));
        assert!(rows_say(&app, "fixer"));
    }

    #[test]
    fn an_agent_started_in_a_shell_reads_shell_then_agent_and_says_its_status_is_a_guess() {
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
        assert!(lines.iter().any(|l| l.contains("◌ claude by hand, status")), "{lines:#?}");
        assert!(!app.row_actions(&Row::Session(app.sessions[0].id)).contains(&"session.adopt"));
    }

    #[test]
    fn when_rows_dont_fit_only_the_ones_in_focus_or_wanting_you_open_up() {
        let mut app = app();
        let mut many: Vec<SessionInfo> = (0..14)
            .map(|n| {
                let mut s = session(&format!("s{n:02}"));
                s.last_reply = Some(format!("reply number {n}"));
                s
            })
            .collect();
        status(&mut many[7], AgentStatus::Blocked, Some("question"), Some("which db?"));
        app.panes.get_mut(&1).unwrap().session = Some(many[3].id);
        app.sessions = many;
        // 14 rows and 16 detail lines don't fit in 27: the focused and blocked rows open.
        assert!(rows_say(&app, "last reply number 3"), "the focused one");
        assert!(rows_say(&app, "wants which db?"), "the blocked one");
        assert!(!rows_say(&app, "reply number 5"), "the rest stay one line");
        app.config.ui.sidebar_detail = ptyro_core::config::SidebarDetail::Full;
        assert!(rows_say(&app, "last reply number 0"));
    }

    #[test]
    fn what_a_session_last_said_takes_one_line_even_in_focus() {
        let mut app = app();
        let mut s = session("talker");
        s.last_reply = Some(format!("{} END", "word ".repeat(60)));
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.sessions = vec![s];
        let lines = sidebar(&app);
        let at = lines.iter().position(|l| l.contains("last word")).expect("the line");
        assert!(lines[at].trim_end().ends_with('…'), "cut, not wrapped: {lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("END")), "{lines:#?}");
        assert!(!lines.get(at + 1).is_some_and(|l| l.contains("word")), "{lines:#?}");
    }

    #[test]
    fn a_blocked_row_takes_one_line_and_offers_its_key_where_you_are() {
        let mut app = app();
        let mut s = session("asker");
        s.kind = SessionKind::Agent;
        status(&mut s, AgentStatus::Blocked, Some("permission"), Some("Bash: npm i"));
        app.sessions = vec![s.clone()];
        let lines = sidebar(&app);
        let at = lines.iter().position(|l| l.contains("wants Bash: npm i")).expect("the line");
        assert!(!lines.get(at + 1).is_some_and(|l| l.contains("y/a/n")), "{lines:#?}");
        // The row you're on: the key that answers it, under it.
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        let lines = sidebar(&app);
        let at = lines.iter().position(|l| l.contains("wants Bash: npm i")).expect("the line");
        assert!(lines[at + 1].contains("C-q p y/a/n"), "{lines:#?}");
        // Room for both: one line.
        app.sidebar_width = 60;
        let shown = screen(&app);
        assert!(shown.contains("wants Bash: npm i · C-q p y/a/n"), "{shown}");
    }

    #[test]
    fn a_click_on_a_detail_line_picks_its_session() {
        let mut app = app();
        let mut a = session("alpha");
        a.last_reply = Some("first reply".into());
        let b = session("beta");
        app.sessions = vec![a.clone(), b.clone()];
        let lines = sidebar(&app);
        let y = lines.iter().position(|l| l.contains("first reply")).unwrap() as u16;
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
                s.last_reply = Some(format!("the reply of session number {n}, which is long"));
                s
            })
            .collect();
        app.panes.get_mut(&1).unwrap().session = Some(sessions[0].id);
        app.sessions = sessions.clone();
        let main = app.main_area();
        app.run(&Command { action: "sidebar.zoom", arg: None });
        assert_eq!(app.main_area(), main, "the panes keep their size");
        let shown = screen(&app);
        assert!(shown.contains("the reply of session number 9, which is long"), "{shown}");
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
    fn open_in_offers_the_installed_editors_then_a_shell_on_bang() {
        let mut app = app();
        app.worktrees.push(root("p1"));
        app.open_editors("p1-root".into());
        let shown = screen(&app);
        assert!(shown.contains("a new shell in it") && shown.contains('!'), "{shown}");
        for n in app.config.editor_names() {
            assert!(shown.contains(&n), "{n} is installed: {shown}");
        }
        let Some(Overlay::Picker { palette, purpose: Purpose::Editor(_) }) = app.overlay.as_mut()
        else {
            panic!("no picker");
        };
        assert_eq!(palette.key(KeyEvent::char('!')), Picked::Pick(Target::NewShell));
    }

    #[test]
    fn tools_run_the_daemons_own_ptyro() {
        // A tool session runs on the daemon's computer: this binary's path means nothing there.
        let mut app = app();
        let here = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert_eq!(app.daemon_ptyro(), here);
        app.connect.remote = Some("me@devbox".into());
        let there = std::env::var("PTYRO_REMOTE_BIN").unwrap_or_else(|_| "ptyro".into());
        assert_eq!(app.daemon_ptyro(), there);
        assert!(app.daemon_file_note().contains("me@devbox"));
    }

    #[test]
    fn on_another_computer_open_in_lists_the_daemons_editors() {
        // WIN3: the paths are the daemon's, so are the editors (and it opens them).
        let mut app = app();
        app.connect.remote = Some("me@devbox".into());
        app.worktrees.push(root("p1"));
        app.handle_reply(
            Pending::RemoteEditors("p1-root".into()),
            Ok(Reply::Editors {
                names: vec!["files".into(), "code".into()],
                default: "code".into(),
            }),
        );
        let shown = screen(&app);
        assert!(shown.contains("on the computer running the daemon"), "{shown}");
        let Some(Overlay::Picker { palette, purpose: Purpose::Editor(wt) }) = app.overlay.as_ref()
        else {
            panic!("no picker");
        };
        assert_eq!(wt, "p1-root");
        let _ = palette;
        let at = |w: &str| shown.find(&format!(" {w} ")).unwrap_or_else(|| panic!("{w}: {shown}"));
        assert!(
            at("code") < at("files") && at("files") < at("shell"),
            "the default first: {shown}"
        );
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
            Some(Overlay::Form(f)) => {
                assert_eq!(f.get("where"), "new:p1");
                assert!(f.title.contains("myrepo"), "{}", f.title);
            }
            _ => panic!("the new-worktree form, in the new project"),
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
    fn an_agent_pane_header_names_the_model_once_the_agent_said() {
        let mut app = app();
        let mut agent = session("fixer");
        agent.kind = SessionKind::Agent;
        agent.agent = Some("claude".into());
        app.panes.get_mut(&1).unwrap().session = Some(agent.id);
        app.sessions = vec![agent.clone()];
        let shown = screen(&app);
        assert!(!shown.contains("opus"), "no model yet, none shown: {shown}");
        agent.model = Some("claude-opus-5-5".into());
        app.sessions = vec![agent.clone()];
        let shown = screen(&app);
        assert!(shown.contains("fixer · claude opus 5.5"), "{shown}");
        // A shell has no model to show, whatever it carries.
        agent.kind = SessionKind::Shell;
        app.sessions = vec![agent];
        assert!(!screen(&app).contains("opus 5.5"));
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
        // A space in the name becomes -; what no character fixes says why, and ⏎ keeps the box.
        key(&mut app, KeyCode::Tab, Mods::NONE);
        typed(&mut app, " y");
        assert!(screen(&app).contains("→ fix-flaky-test-x-y ·"), "{}", screen(&app));
        typed(&mut app, ".");
        assert!(screen(&app).contains("✗ name:"), "{}", screen(&app));
        key(&mut app, KeyCode::Enter, Mods::NONE);
        assert!(matches!(app.overlay, Some(Overlay::Task { .. })));
        // C-o: every option on one form, what was typed carried along (SPEC §11.5a).
        key(&mut app, KeyCode::Char { c: 'o' }, Mods(Mods::CTRL));
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the session form") };
        assert_eq!(f.get("prompt"), "Fix the flaky test in tests/api.rs now");
        assert_eq!(f.get("name"), "fix-flaky-test-x-y.");
        assert_eq!(f.get("agent"), "claude");
        assert_eq!(f.get("where"), "new:p1", "a new worktree of the project in context");
        assert_eq!(f.get("private"), "", "Private is on the form, off unless `kb.private`");
        // The bad name is said on ⏎; fixed, ⏎ creates the worktree with every option.
        key(&mut app, KeyCode::Enter, Mods::NONE);
        assert!(matches!(&app.overlay, Some(Overlay::Form(f)) if f.error.is_some()));
        typed(&mut app, "\x08");
        let Some(Overlay::Form(f)) = &mut app.overlay else { panic!() };
        f.choose("setup", "");
        f.choose("private", "yes");
        if let Some(crate::form::Field { kind: crate::form::Kind::Text { value, .. }, .. }) =
            f.fields.iter_mut().find(|x| x.key == "base")
        {
            *value = "origin/dev".into();
        }
        while app.rx.try_recv().is_ok() {}
        key(&mut app, KeyCode::Enter, Mods::NONE);
        let mut spec = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::PreCreate(s, _), _) = m {
                spec = Some(s);
            }
        }
        let spec = spec.expect("the worktree, asked for");
        assert_eq!(spec.name.as_deref(), Some("fix-flaky-test-x-y"));
        assert_eq!(spec.base.as_deref(), Some("origin/dev"));
        assert!(!spec.setup, "setup unticked");
        assert_eq!(spec.agent.as_deref(), Some("claude"));
        assert_eq!(spec.private, Some(true), "the form's Private switch (SPEC §25.7a)");
    }

    fn profile_info(name: &str) -> ProfileInfo {
        serde_json::from_value(serde_json::json!({
            "name": name, "label": name, "defined": name != "default", "isolate": true,
            "dirs": {}, "vars": [],
        }))
        .unwrap()
    }

    #[test]
    fn a_new_workspace_picks_its_profile_and_can_make_one_on_the_way() {
        let mut app = app();
        app.profiles = vec![profile_info("default"), profile_info("cl")];
        app.picked(Target::NewWorkspace, Purpose::Workspace);
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the form") };
        // The suggested name is the profile without a workspace, and that profile is chosen.
        assert_eq!(f.or_suggested("name"), "cl");
        assert_eq!(f.get("profile"), "cl");
        assert_eq!(f.get("notify"), "all");
        // Another name, and a profile that doesn't exist yet: made first, then back here.
        typed(&mut app, "acme\t");
        let Some(Overlay::Form(f)) = &mut app.overlay else { panic!() };
        f.choose("profile", crate::form::NEW_PROFILE);
        typed(&mut app, "\n");
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the profile form") };
        assert_eq!(f.title, "New profile");
        assert_eq!(f.get("name"), "acme", "named like the workspace");
        typed(&mut app, "\tAcme Corp\n");
        assert_eq!(
            app.last_cli.as_deref(),
            Some(&["profile", "new", "--label", "Acme Corp", "--", "acme"].map(String::from)[..])
        );
        assert!(app.after_profile.is_some(), "waits for it to show up");
        app.overlay = None;
        app.profiles.push(profile_info("acme"));
        app.profiles_arrived();
        assert_eq!(
            app.last_cli.as_deref(),
            Some(&["profile", "login", "acme", "claude"].map(String::from)[..]),
            "then logged in"
        );
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the workspace form, back") };
        assert_eq!(f.get("name"), "acme");
        assert_eq!(f.get("profile"), "acme", "the new profile, chosen");
        while app.rx.try_recv().is_ok() {}
        typed(&mut app, "\n");
        let mut made = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::WorkspaceMade(n), _) = m {
                made = Some(n);
            }
        }
        assert_eq!(made.as_deref(), Some("acme"));
    }

    #[test]
    fn a_projects_workspaces_are_ticked_and_saved_as_changes() {
        let mut app = app();
        with_project(&mut app);
        let mut side = app.workspaces[0].clone();
        side.name = "side".into();
        side.id = "ws-side".into();
        side.projects.clear();
        app.workspaces.push(side);
        let p = app.projects[0].clone();
        app.open_project_workspaces(&p);
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the form") };
        assert_eq!(f.get("workspaces"), "main", "ticked where it is");
        assert_eq!(f.verb, "save");
        // Tick side too (→ to it, space): one request, to add it there; main is left alone.
        typed(&mut app, " ");
        assert!(
            matches!(&app.overlay, Some(Overlay::Form(f)) if f.get("workspaces").is_empty()),
            "space unticked main"
        );
        typed(&mut app, " ");
        key(&mut app, KeyCode::Right, Mods::NONE);
        typed(&mut app, " ");
        let Some(Overlay::Form(f)) = &app.overlay else { panic!() };
        assert_eq!(f.get("workspaces"), "main,side");
        while app.rx.try_recv().is_ok() {}
        typed(&mut app, "\n");
        let mut asked = 0;
        while let Ok(m) = app.rx.try_recv() {
            if matches!(m, AppMsg::Reply(Pending::Done(_), _)) {
                asked += 1;
            }
        }
        assert!(app.overlay.is_none());
        assert_eq!(asked, 1, "only side changed");
        assert!(
            app.toast.as_ref().is_some_and(|t| t.text.contains("main, side")),
            "{:?}",
            app.toast.as_ref().map(|t| &t.text)
        );
    }

    #[test]
    fn a_row_shown_twice_keeps_the_copy_nearest_where_it_was() {
        let rows = vec![
            Row::Workspace("main".into()),
            Row::Project("p".into()),
            Row::Workspace("side".into()),
            Row::Project("p".into()),
        ];
        let p = Row::Project("p".into());
        assert_eq!(anchor_index(&rows, Some(&p), 3), 3);
        assert_eq!(anchor_index(&rows, Some(&p), 1), 1);
        assert_eq!(anchor_index(&rows, Some(&p), 0), 1);
    }

    #[test]
    fn create_dialogs_offer_a_name_that_enter_takes() {
        let mut app = app();
        with_project(&mut app);
        app.run(&Command { action: "worktree.new", arg: None });
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the form") };
        let offered = f.get("name");
        assert!(!offered.is_empty(), "a free name, already in it");
        assert_eq!(f.get("where"), "new:p1");
        let shown = screen(&app);
        assert!(shown.contains(&offered) && shown.contains("⏎ start"), "{shown}");
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
        assert!(!shown.contains("task Fix the flaky"), "no title line in the row: {shown}");
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

    /// BOOT9: an agent that isn't installed (a session or a chat refused for it) offers the setup
    /// checklist with it ticked, on `toast.act`; no agent at all offers the checklist's defaults.
    #[test]
    fn a_missing_agent_offers_the_setup_checklist() {
        let mut app = app();
        let missing = ptyro_agents::locate::not_installed("codex", "codex");
        let refused = ptyro_core::Error::new(ErrorCode::NotFound, missing.clone());
        app.handle_reply(
            Pending::Spawned { split: None, spec: Box::default(), back: false },
            Err(refused),
        );
        let toast = app.toast.take().unwrap();
        assert_eq!(toast.text, format!("{missing} · C-q u set it up (the checklist)"));
        let Some(Offer { then: Then::Run(cmd), .. }) = toast.offer else {
            panic!("{}", toast.text)
        };
        assert_eq!((cmd.action, cmd.arg.as_deref()), ("app.setup", Some("codex")));
        // A chat with no agent to answer it: the checklist as it opens by itself.
        app.chat_not_made(0, "hello".into(), ptyro_agents::locate::no_chat_agent());
        let toast = app.toast.take().unwrap();
        assert!(toast.text.ends_with("· C-q u set up agents (the checklist)"), "{}", toast.text);
        let Some(Offer { then: Then::Run(cmd), .. }) = toast.offer else {
            panic!("{}", toast.text)
        };
        assert_eq!((cmd.action, cmd.arg), ("app.setup", None));
    }

    /// No message the TUI writes tells you to type a command (UX1.17): it offers the fix.
    #[test]
    fn no_toast_names_a_command_to_type() {
        let lines: Vec<&str> = include_str!("app.rs").lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if !(l.contains("toast(") || l.contains("toast_err(") || l.contains("toast_offer(")) {
                continue;
            }
            // The call and the lines of its arguments.
            let call = lines[i..(i + 6).min(lines.len())].join(" ");
            let call = call.split(';').next().unwrap_or("");
            assert!(
                !call.contains("`ptyro ") && !call.contains(": project."),
                "app.rs:{}: a message names a command to type: {call}",
                i + 1
            );
        }
    }

    /// `C-q C-q` is `C-q Tab`; `C-q q` sends the prefix that was pressed to the program.
    #[test]
    fn the_prefix_twice_goes_to_the_sidebar_and_q_sends_it() {
        use crossterm::event::{KeyCode as CK, KeyEvent as CE, KeyModifiers as CM};
        let mut app = app();
        app.sessions = vec![session("a")];
        let ctrl = |c| CE::new(CK::Char(c), CM::CONTROL);
        app.on_key(&ctrl('q'));
        app.on_key(&ctrl('q'));
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(!app.prefix, "the second press ran a command, it didn't start another");
        app.on_key(&ctrl('q'));
        app.on_key(&ctrl('q'));
        assert_eq!(app.focus, Focus::Pane, "again: back to the pane");
        app.on_key(&ctrl(' '));
        app.on_key(&CE::new(CK::Char('q'), CM::NONE));
        assert!(!app.prefix);
        assert_eq!(
            app.prefix_key.map(keymap::normalize),
            Some(keymap::normalize(KeyEvent::new(KeyCode::Char { c: ' ' }, Mods(Mods::CTRL))))
        );
        assert!(app.toast.is_none(), "q is bound: {:?}", app.toast.as_ref().map(|t| &t.text));
        assert_eq!(app.focus, Focus::Pane);
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
        for want in
            ["n a task", "m chat", "a who needs you", "g chats", ". its actions", "? all keys"]
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
        assert!(live_actions.contains(&"session.stop") && live_actions.contains(&"session.events"));
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

    /// Stop, kill, and stop and remove each ask first while `ui.confirm_kill` is on (the web does
    /// the same), and none asks once it is off. An ended session is removed without asking.
    #[test]
    fn confirm_kill_asks_before_stop_kill_and_stop_and_remove() {
        for action in ["session.stop", "session.kill", "session.remove"] {
            let mut app = app();
            let live = session("live");
            app.sessions = vec![live.clone()];
            app.session_action(action, live.id);
            let Some(Overlay::Confirm { text, .. }) = &app.overlay else {
                panic!("{action} didn't ask: {:?}", app.overlay.is_some())
            };
            assert!(text.contains("live"), "{text}");
            // The web's sentence, word for word.
            let said = match action {
                "session.stop" => ptyro_core::removal::STOP,
                "session.kill" => ptyro_core::removal::KILL,
                _ => ptyro_core::removal::STOP_REMOVE,
            };
            assert!(text.contains(said), "{text}");
            assert!(!app.stopping.contains(&live.id), "{action} waits for the answer");

            app.overlay = None;
            app.config.ui.confirm_kill = false;
            app.session_action(action, live.id);
            assert!(app.overlay.is_none(), "{action} asked with confirm_kill off");
            assert!(app.stopping.contains(&live.id), "{action} ran");
        }
        let mut app = app();
        let mut gone = session("gone");
        gone.state = ProcessState::Exited { code: Some(0), signal: None };
        app.sessions = vec![gone.clone()];
        app.session_action("session.remove", gone.id);
        assert!(app.overlay.is_none(), "an ended session is removed without asking");
        let actions = app.row_actions(&Row::Session(gone.id));
        assert!(actions.contains(&"session.remove") && !actions.contains(&"session.stop"));
        // Kill never forgets: on an ended session it says it isn't running and offers remove.
        app.session_action("session.kill", gone.id);
        let t = app.toast.as_ref().unwrap();
        assert!(t.text.contains("isn't running") && t.offer.is_some(), "{}", t.text);
        // … for that session, wherever the selection goes before it's taken.
        let named = matches!(&t.offer, Some(Offer { then: Then::Run(Command { action: "session.remove", arg: Some(a) }), .. }) if *a == gone.id.full());
        assert!(named, "{:?}", t.offer);
        assert!(app.overlay.is_none());
        // A dormant one can be resumed: removing it asks.
        let mut dormant = session("sleepy");
        dormant.state = ProcessState::Dormant;
        app.sessions.push(dormant.clone());
        app.session_action("session.remove", dormant.id);
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("didn't ask") };
        assert!(text.contains(ptyro_core::removal::DORMANT), "{text}");
    }

    /// A refusal's "Stop and remove" asks first, as the action does (`ui.confirm_kill`).
    #[test]
    fn a_refusal_s_stop_and_remove_asks_first() {
        let mut app = app();
        let live = session("live");
        app.sessions = vec![live.clone()];
        let req = Request::Remove { addr: live.id.full(), stop: true };
        app.toast_offer(
            "live is still running",
            true,
            Offer {
                label: "stop and remove".into(),
                then: Then::Send(Box::new(req), String::new()),
            },
        );
        app.run(&Command { action: "toast.act", arg: None });
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("didn't ask") };
        assert!(text.contains(ptyro_core::removal::STOP_REMOVE), "{text}");
        assert!(!app.stopping.contains(&live.id));
    }

    /// A refusal's way through (SPEC §6.6) is a button: the same request with it said.
    #[test]
    fn a_refusal_s_fix_becomes_a_button() {
        use ptyro_core::error::Again;
        let mut app = app();
        let req = Request::Remove { addr: "x".into(), stop: false };
        let e = Error::conflict("x is still running: stop it first")
            .with_fix(Again::Stop, "Stop and remove");
        let retry = req.with_fix(&e.fix.as_ref().unwrap().again).unwrap();
        app.on_reply(AppMsg::Refused(Pending::Done(String::new()), e, Box::new(retry)));
        let t = app.toast.as_ref().unwrap();
        assert!(t.text.contains("stop it first"), "{}", t.text);
        let Some(Offer { label, then: Then::Send(req, _) }) = &t.offer else { panic!("no button") };
        assert_eq!(label, "stop and remove");
        assert!(matches!(**req, Request::Remove { stop: true, .. }));
        // Without one, its words still decide (or nothing is offered).
        let mut app = self::app();
        app.on_reply(AppMsg::Reply(Pending::Done(String::new()), Err(Error::conflict("nope"))));
        assert!(app.toast.as_ref().unwrap().offer.is_none());
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

    fn feature(app: &mut App) -> WorktreeInfo {
        let mut w = root("p1");
        w.id = "p1-feat".into();
        w.name = "feat".into();
        w.is_root = false;
        w.branch = Some("ptyro/feat".into());
        app.worktrees.push(w.clone());
        w
    }

    /// The palette lists the built-in themes; one pick switches to it and keeps it in the
    /// config (`ui.theme`), as the settings panel would.
    #[test]
    fn a_theme_picked_in_the_palette_is_switched_to_and_kept() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| h.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let area = Rect::new(0, 0, 100, 30);
        let mut app =
            App::new(Options::default(), connect, paths.clone(), Config::default(), false, area);
        app.open_palette();
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!("the palette") };
        let themes: Vec<(&str, &str)> = (0..palette.filtered.len())
            .map(|i| palette.item(i))
            .filter(|it| matches!(it.target, Target::Theme(_)))
            .map(|it| (it.label.as_str(), it.detail.as_str()))
            .collect();
        assert_eq!(
            themes,
            [
                ("theme ptyro-dark", "the theme in use"),
                ("theme ptyro-light", "switch to it (kept as ui.theme)"),
                ("theme high-contrast", "switch to it (kept as ui.theme)"),
            ]
        );
        app.overlay = None;
        app.picked(Target::Theme("ptyro-light".into()), Purpose::Palette);
        assert_eq!(app.theme_name, "ptyro-light", "switched at once");
        let file = std::fs::read_to_string(Config::path(&paths)).unwrap();
        assert!(file.contains("theme = \"ptyro-light\""), "kept: {file}");
        assert!(app.toast.as_ref().unwrap().text.starts_with("theme ptyro-light"));
    }

    /// The clean-up list's C-k keeps the worktree under the cursor (SPEC §10.8): it is asked of
    /// the daemon and leaves the list, which stays open for the rest; the last one closes it.
    #[test]
    fn clean_up_keeps_a_worktree_with_c_k() {
        let mut app = app();
        with_project(&mut app);
        let a = feature(&mut app);
        let mut b = a.clone();
        (b.id, b.name) = ("p1-old".into(), "old".into());
        app.worktrees.push(b.clone());
        let entry = |w: &WorktreeInfo| ptyro_core::model::GcEntry {
            worktree: w.clone(),
            reasons: vec!["no sessions for 30 days".into()],
            bytes: None,
            bytes_partial: false,
        };
        let entries = vec![entry(&a), entry(&b)];
        app.on_reply(AppMsg::Reply(Pending::Gc, Ok(Reply::Gc { entries, scratch: Vec::new() })));
        let Some(Overlay::Picker { palette, purpose: Purpose::Gc }) = &app.overlay else {
            panic!("the clean-up list")
        };
        assert!(palette.title.contains("C-k keep it"), "{}", palette.title);
        while app.rx.try_recv().is_ok() {}
        key(&mut app, KeyCode::Char { c: 'k' }, Mods(Mods::CTRL));
        // Not connected here, so the request comes back refused: it was asked, for `a`.
        assert!(matches!(app.rx.try_recv(), Ok(AppMsg::Reply(Pending::GcKept, _))));
        let Some(Overlay::Picker { palette, purpose: Purpose::Gc }) = &app.overlay else {
            panic!("the list stays open for the rest")
        };
        let left: Vec<&Target> =
            (0..palette.filtered.len()).map(|i| &palette.item(i).target).collect();
        assert_eq!(left, [&Target::Worktree(b.id.clone())]);
        key(&mut app, KeyCode::Char { c: 'k' }, Mods(Mods::CTRL));
        assert!(app.overlay.is_none(), "the last one kept closes it");
    }

    /// Worktrees and projects are renamed from their row menu (SPEC §2.3), with the session
    /// rename's live check: a taken or bad name is flagged as it is typed.
    #[test]
    fn worktrees_and_projects_are_renamed_with_a_live_check() {
        let mut app = app();
        with_project(&mut app);
        app.projects.push(project("p2", "lib"));
        let w = feature(&mut app);
        let mut other = w.clone();
        (other.id, other.name) = ("p1-other".into(), "other".into());
        app.worktrees.push(other);
        assert!(app.row_actions(&Row::Worktree(w.id.clone())).contains(&"worktree.rename"));
        assert!(!app.row_actions(&Row::Worktree("p1-root".into())).contains(&"worktree.remove"));
        assert!(app.row_actions(&Row::Project("p1".into())).contains(&"project.rename"));
        for a in ["worktree.rename", "project.rename"] {
            assert!(keymap::spec(a).is_some(), "{a} is in the palette");
        }

        app.focus = Focus::Sidebar;
        let select = |app: &mut App, row: Row| {
            let rows = app.rows();
            let i = rows.iter().position(|r| *r == row).unwrap();
            app.set_sel(&rows, i);
        };
        select(&mut app, Row::Worktree(w.id.clone()));
        app.run(&Command { action: "worktree.rename", arg: None });
        assert_eq!(prompt_text(&app), Some("feat"));
        assert!(screen(&app).contains("⏎ ok (unchanged)"), "{}", screen(&app));
        let kind = PromptKind::RenameWorktree(w.id.clone());
        assert_eq!(app.prompt_check(&kind, "other"), Err("p1/other exists".into()));
        assert!(app.prompt_check(&kind, "a/b").is_err(), "the daemon's rule");
        assert!(app.prompt_check(&kind, "login-fix").is_ok());
        // Typed: what a name can't hold is replaced, as in every name prompt.
        typed(&mut app, "\x08\x08\x08\x08Login Fix");
        let want = ptyro_core::names::typed("Login Fix");
        assert_eq!(prompt_text(&app), Some(want.as_str()));
        typed(&mut app, "\n");
        assert!(app.overlay.is_none(), "sent");

        // The main checkout is always @root: renaming it asks for its title only.
        select(&mut app, Row::Worktree("p1-root".into()));
        app.run(&Command { action: "worktree.rename", arg: None });
        assert!(matches!(
            &app.overlay,
            Some(Overlay::Prompt { kind: PromptKind::RetitleWorktree(_), .. })
        ));
        app.overlay = None;

        select(&mut app, Row::Project("p1".into()));
        app.run(&Command { action: "project.rename", arg: None });
        assert_eq!(prompt_text(&app), Some("myrepo"));
        let kind = PromptKind::RenameProject("p1".into());
        assert_eq!(app.prompt_check(&kind, "myrepo"), Ok(Some("unchanged".into())));
        assert_eq!(app.prompt_check(&kind, "lib"), Err("a project named lib exists".into()));
        assert!(app.prompt_check(&kind, "web").is_ok());
        // A slug is `[a-z0-9-]`, as the daemon refuses: `_` is marked, Tab offers `-`.
        assert!(app.prompt_check(&kind, "my_web").is_err());
        assert_eq!(kind.fit("My Web_2"), "my-web-2");
        assert_eq!(kind.tidy("my_web").as_deref(), Some("my-web"));
    }

    /// A project's row menu offers moving it, taking it out of this workspace and forgetting it
    /// (`project.move`, `project.leave`, `project.remove`, once palette-only). One whose main
    /// checkout is gone is marked in the sidebar, its hint says so, and its menu leads with
    /// finding it, then forgetting it (SPEC §10.1).
    #[test]
    fn a_project_moves_and_a_missing_one_says_so() {
        let mut app = app();
        with_project(&mut app);
        let menu = app.row_actions(&Row::Project("p1".into()));
        for a in ["project.move", "project.leave", "project.remove"] {
            assert!(menu.contains(&a), "{a} in {menu:?}");
            assert!(keymap::spec(a).is_some(), "{a} is in the palette");
        }
        assert!(!app.project_missing("p1"));
        assert!(!screen(&app).contains("missing"));

        // Its main checkout is gone.
        app.worktrees[0].state = ptyro_core::model::WorktreeState::Missing;
        assert!(app.project_missing("p1"));
        let menu = app.row_actions(&Row::Project("p1".into()));
        assert_eq!(&menu[..2], ["project.move", "project.remove"], "{menu:?}");
        assert!(!menu.contains(&"worktree.new"), "nothing starts where there's nothing");
        let shown = screen(&app);
        assert!(shown.contains("myrepo missing"), "{shown}");
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Project("p1".into())).unwrap();
        app.set_sel(&rows, i);
        let hint = app.context_hint().unwrap();
        assert!(
            hint.contains("its directory is gone") && hint.contains("move it, or forget it"),
            "{hint}"
        );

        // ptyro found where it went: said once, the move one key away, and on its row.
        let mut found = project("p1", "myrepo");
        found.moved_to = Some("/Volumes/X/dev/myrepo".into());
        app.on_event(DomainEvent::ProjectUpdated { info: found.clone() });
        let t = app.toast.as_ref().expect("said");
        assert!(
            t.text.starts_with("myrepo seems to have moved to /Volumes/X/dev/myrepo"),
            "{}",
            t.text
        );
        assert!(matches!(
            &t.offer,
            Some(Offer { then: Then::Send(req, _), .. })
                if matches!(&**req, Request::ProjectMove { addr, path } if addr == "p1" && path == std::path::Path::new("/Volumes/X/dev/myrepo"))
        ));
        app.toast = None;
        app.on_event(DomainEvent::ProjectUpdated { info: found });
        assert!(app.toast.is_none(), "said once");
        assert!(screen(&app).contains("myrepo missing →?"));
        assert!(app.context_hint().unwrap().starts_with("moved to /Volumes/X/dev/myrepo? "));

        // `project.move`: the form, the place found filled in; ⏎ sends the move.
        app.run(&Command { action: "project.move", arg: None });
        let Some(Overlay::Form(f)) = &app.overlay else { panic!("the move form") };
        assert_eq!(f.make, crate::form::Make::ProjectMove("p1".into()));
        assert_eq!(f.get("path"), "/Volumes/X/dev/myrepo");
        while app.rx.try_recv().is_ok() {}
        typed(&mut app, "\n");
        assert!(app.overlay.is_none(), "sent");
        assert!(
            matches!(app.rx.try_recv(), Ok(AppMsg::Reply(Pending::Moved(s), _)) if s == "myrepo")
        );

        // The doctor's finding opens the same form.
        let check = crate::health::Check {
            section: "projects".into(),
            name: "myrepo".into(),
            level: "warn".into(),
            text: "myrepo's directory (/r/myrepo) is gone: if you moved it, `ptyro project move myrepo NEWPATH`".into(),
        };
        let f = crate::health::finding(&check).unwrap();
        assert_eq!(
            f.fix,
            Some(crate::health::Fix::Move("myrepo".into())),
            "never NEWPATH run as is"
        );
    }

    /// Renaming something mirrored from another machine (MM1.B) types and checks the name it
    /// has there: `laptop2:app` is offered as `app`, never as `laptop2_app`.
    #[test]
    fn a_mirrored_thing_is_renamed_by_its_own_name() {
        let mut app = app();
        let mirrored = |id: &str, slug: &str| ProjectInfo {
            machine: Some("laptop2".into()),
            ..project(id, &format!("laptop2:{slug}"))
        };
        app.projects = vec![project("p1", "lib"), mirrored("m1", "app"), mirrored("m2", "tool")];
        let kind = PromptKind::RenameProject("m1".into());
        assert_eq!(app.prompt_check(&kind, "app"), Ok(Some("unchanged".into())));
        assert!(app.prompt_check(&kind, "lib").is_ok(), "this machine's lib is another one");
        assert_eq!(app.prompt_check(&kind, "tool"), Err("a project named tool exists".into()));
        app.workspaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "w", "name": "laptop2:work", "profile": "default", "projects": [],
                "notify": "all", "created_ms": 0, "machine": "laptop2",
            }))
            .unwrap(),
        );
        app.workspace = "laptop2:work".into();
        app.run(&Command { action: "workspace.rename", arg: None });
        assert_eq!(prompt_text(&app), Some("work"));
        let ws = PromptKind::RenameWorkspace("laptop2:work".into());
        assert_eq!(app.prompt_check(&ws, "work"), Ok(Some("unchanged".into())));
        let mut s = session("laptop2:fixer");
        s.machine = Some("laptop2".into());
        app.sessions = vec![s.clone()];
        app.overlay = None;
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.run(&Command { action: "session.rename", arg: None });
        assert_eq!(prompt_text(&app), Some("fixer"));
        assert_eq!(
            app.prompt_check(&PromptKind::Rename(s.id), "fixer"),
            Ok(Some("unchanged".into()))
        );
    }

    #[test]
    fn cleaning_up_a_worktree() {
        let mut app = app();
        with_project(&mut app);
        let w = feature(&mut app);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let i = rows.iter().position(|r| *r == Row::Worktree(w.id.clone())).unwrap();
        app.set_sel(&rows, i);
        assert!(app.context_hint().unwrap().contains("start a task here"));
        // `x` removes it (asking first); `F`, `D` and `Q` do nothing any more.
        let x = app.keymap.lookup(Table::Nav, KeyEvent::char('x')).cloned().unwrap();
        app.run(&x);
        assert!(
            matches!(
                &app.overlay,
                Some(Overlay::Confirm { action: Confirmed::RemoveWorktree { .. }, .. })
            ),
            "remove, asked first"
        );
        for k in ['F', 'D'] {
            assert!(app.keymap.lookup(Table::Nav, KeyEvent::char(k)).is_none(), "{k} is unbound");
        }
        for k in ['D', 'Q'] {
            assert!(
                app.keymap.lookup(Table::Prefix, KeyEvent::char(k)).is_none(),
                "{k} is unbound"
            );
        }
        // A removal that would lose work says what, and offers removing anyway or keeping it.
        app.overlay = None;
        app.on_reply(AppMsg::Reply(
            Pending::Removed(w.id.clone()),
            Err(Error::new(ErrorCode::DirtyWorktree, "refusing to remove p1/feat")
                .with_fix(ptyro_core::error::Again::Force, "Remove anyway")),
        ));
        let shown = screen(&app);
        assert!(shown.contains("y: remove it anyway") && shown.contains("n: keep it"), "{shown}");
        assert!(!shown.contains("archive"), "{shown}");
        key(&mut app, KeyCode::Char { c: 'n' }, Mods::NONE);
        assert!(app.overlay.is_none(), "kept: nothing went out");
        // The row menu has them all, the companion shell too.
        let actions = app.row_actions(&Row::Worktree(w.id.clone()));
        assert!(!actions.iter().any(|a| a.contains("archive")), "{actions:?}");
        for a in ["worktree.remove", "shell.drawer", "shell.new"] {
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
    fn work_can_come_from_a_pr_or_a_branch() {
        let mut app = app();
        with_project(&mut app);
        // The task box takes a pasted PR.
        app.run(&Command { action: "session.new", arg: None });
        typed(&mut app, "https://github.com/acme/app/pull/12");
        assert!(screen(&app).contains("→ pr-12 · claude · PR #12's branch, in a new worktree"));
        // The wizard's "where": a PR or issue, or a branch; never the main checkout's
        // uncommitted changes, even when it has some (taking them was removed, 2026-11-17).
        app.overlay = None;
        let st = ptyro_core::model::RepoStatus { modified: 2, ..Default::default() };
        app.worktrees[0].status = Some(st);
        app.open_targets(Wizard { project: Some("p1".into()), ..Wizard::default() });
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!() };
        let labels: Vec<&str> =
            (0..palette.filtered.len()).map(|i| palette.item(i).label.as_str()).collect();
        for want in ["+ from a PR or an issue…", "+ on an existing branch…"] {
            assert!(labels.contains(&want), "{want}: {labels:?}");
        }
        assert!(!labels.iter().any(|l| l.contains("uncommitted")), "{labels:?}");
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
            setup: crate::setup_list::Setup::looking(None),
            only_setup: false,
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
        assert!(shown.contains("agents and tools: looking at what's installed…"), "{shown}");
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
        // Still looking, so nothing installed: it says how to come back to the checklist.
        assert!(app.toast.as_ref().unwrap().text.contains("C-q U opens the setup checklist"));
    }

    /// BOOT2: the setup checklist is the first part of the first run: Agents, Tools and Already
    /// here, each with its line; a prerequisite ticks itself with the reason; ⏎ installs what's
    /// ticked in a split (`ptyro install … --confirmed`) and keeps the other choices.
    #[test]
    fn the_first_run_starts_with_the_setup_checklist_and_enter_installs_in_a_split() {
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
            Rect::new(0, 0, 160, 40),
        );
        use crate::onboard::FirstRun;
        app.first_run = Some(FirstRun {
            setup: crate::setup_list::tests::one(&[("claude", "2.1.319")]),
            ..FirstRun::default()
        });
        let shown = screen_at(&app, 140);
        for want in [
            " Agents",
            "[x] Codex",
            "OpenAI's coding agent",
            "then log in",
            "[ ] OpenCode",
            " Tools",
            "[x] git",
            "asks for admin",
            " Already here",
            "✓ Claude Code",
            "your default agent",
            "⏎ install 3 and use these",
            "s not now",
        ] {
            assert!(shown.contains(want), "{want:?}:\n{shown}");
        }
        let k = |app: &mut App, code| app.first_run_key(KeyEvent::new(code, Mods::NONE));
        // Down to OpenCode (after Codex), Space ticks it.
        k(&mut app, KeyCode::Char { c: 'j' });
        k(&mut app, KeyCode::Char { c: ' ' });
        let shown = screen_at(&app, 140);
        assert!(shown.contains("[x] OpenCode"), "{shown}");
        assert!(shown.contains("⏎ install 4 and use these"), "{shown}");
        k(&mut app, KeyCode::Enter);
        assert!(app.first_run.is_none());
        let words = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            app.last_cli,
            Some(words(&["install", "codex", "opencode", "git", "gh", "--confirmed"])),
            "what's ticked, as the CLI plans it"
        );
        assert!(Config::path(&app.paths).exists(), "the first run's config is written too");
    }

    /// BOOT5: the daemon's agents out of date (`Request::Updates`) are the badge's findings and
    /// the start-up note, with the key that updates them; the fix runs `ptyro update` in a split
    /// where the daemon is, one at a time like the checklist's installs; a daemon on another
    /// computer is named.
    #[tokio::test]
    async fn the_daemons_agents_out_of_date_are_noted_and_fixed_in_a_split() {
        use ptyro_agents::updates;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let area = Rect::new(0, 0, 100, 30);
        let mut app =
            App::new(Options::default(), connect, paths.clone(), Config::default(), false, area);
        let codex = updates::Outdated {
            id: "codex".into(),
            name: "Codex".into(),
            program: "codex".into(),
            version: "0.162.1".into(),
            latest: "0.170.0".into(),
            command: Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh".into()),
            via: None,
            admin: false,
            doc: String::new(),
            path: None,
            managed: None,
            move_to: None,
        };
        // The badge's own ask: findings, no note.
        app.got_updates(vec![codex.clone()], Vec::new(), false);
        assert!(app.toast.is_none());
        // The start-up note's: the line, with the key.
        app.got_updates(vec![codex.clone()], vec![codex.clone()], true);
        let t = app.toast.take().expect("the note");
        assert_eq!(t.text, "Codex 0.170.0 is out (you have 0.162.1) · C-q u updates it");
        // The doctor's run (this computer) keeps the daemon's finding, never its own `updates`.
        let local = crate::health::Check {
            section: "updates".into(),
            name: "claude".into(),
            level: "warn".into(),
            text: "Claude Code 2.1.400 is out (you have 2.1.300): `ptyro update claude` updates it"
                .into(),
        };
        let typo = crate::health::Check {
            section: "updates".into(),
            name: "ignore".into(),
            level: "warn".into(),
            text: "updates.ignore holds \"codx\", which isn't an agent ptyro updates".into(),
        };
        app.got_health(vec![local, typo]);
        assert!(
            app.health.findings.iter().any(|f| f.what == "updates ignore"),
            "the local doctor's other updates warnings stay"
        );
        app.got_updates(Vec::new(), Vec::new(), false);
        assert!(
            app.health.findings.iter().any(|f| f.what == "updates ignore"),
            "and stay when the daemon's answer comes"
        );
        app.got_updates(vec![codex.clone()], Vec::new(), false);
        let ups: Vec<&crate::health::Finding> =
            app.health.findings.iter().filter(|f| crate::health::agent_update(f)).collect();
        assert_eq!(ups.len(), 1, "only the daemon's");
        assert_eq!(ups[0].fix, Some(crate::health::Fix::Update(vec!["codex".into()])));
        assert!(ups[0].text.starts_with("Codex 0.170.0 is out"), "{}", ups[0].text);
        let i = app.health.findings.iter().position(|f| f.what == "updates codex").unwrap();
        app.fix(i);
        let words = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(app.last_cli, Some(words(&["update", "codex"])));
        assert!(app.setup_run.is_some(), "tracked like the checklist's installs");
        // One at a time: a second update shows the running one, in an update's words.
        app.last_cli = None;
        app.update_agents(Vec::new());
        assert!(app.last_cli.is_none());
        let said = &app.toast.as_ref().unwrap().text;
        assert!(said.contains("an update is still running in ptyro-update"), "{said}");
        // A daemon on another computer: its agents, named so.
        app.setup_run = None;
        app.connect.remote = Some("ssh://me@box".into());
        app.got_updates(vec![codex.clone()], vec![codex], true);
        let t = app.toast.take().unwrap();
        assert!(t.text.starts_with("on box: Codex 0.170.0 is out"), "{}", t.text);
        assert!(app.health.updates[0].text.starts_with("on box: "));
    }

    /// BOOT7: an agent installed another way and out of date: the badge offers its update and,
    /// beside it, the move (which opens the setup checklist with it ticked); kept, only the update.
    #[tokio::test]
    async fn an_agent_installed_another_way_is_offered_the_move_beside_its_update() {
        use ptyro_agents::updates;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let area = Rect::new(0, 0, 100, 30);
        let mut app =
            App::new(Options::default(), connect, paths.clone(), Config::default(), false, area);
        let mut codex = updates::Outdated {
            id: "codex".into(),
            name: "Codex".into(),
            program: "codex".into(),
            version: "0.162.1".into(),
            latest: "0.170.0".into(),
            command: Some("npm install -g @openai/codex@latest".into()),
            via: Some(ptyro_agents::catalog::Via::Npm),
            admin: false,
            doc: String::new(),
            path: None,
            managed: None,
            move_to: Some(ptyro_agents::catalog::Via::Vendor),
        };
        app.got_updates(vec![codex.clone()], Vec::new(), false);
        let mv = app.health.findings.iter().find(|f| f.what == "updates codex move").unwrap();
        assert!(
            mv.text.contains("Codex is installed with npm: moving it to the vendor's installer")
        );
        assert_eq!(mv.fix, Some(crate::health::Fix::Setup { names: vec!["codex".into()] }));
        // Asked again: replaced, never piled up.
        app.got_updates(vec![codex.clone()], Vec::new(), false);
        assert_eq!(app.health.findings.iter().filter(|f| f.what.ends_with(" move")).count(), 1);
        // Kept (the daemon clears `move_to`): the update alone.
        codex.move_to = None;
        app.got_updates(vec![codex], Vec::new(), false);
        assert!(!app.health.findings.iter().any(|f| f.what.ends_with(" move")));
        assert!(app.health.findings.iter().any(|f| f.what == "updates codex"));
    }

    /// BOOT6: ptyro itself is one more row: the badge's finding updates it with `ptyro update
    /// ptyro` in a split where the daemon is, and the note names it among the agents.
    #[tokio::test]
    async fn ptyro_itself_is_noted_and_fixed_like_an_agent() {
        use ptyro_agents::updates;
        let mut app = app();
        let me = updates::Outdated {
            id: "ptyro".into(),
            name: "ptyro".into(),
            program: "ptyro".into(),
            version: "0.0.63".into(),
            latest: "0.0.64".into(),
            command: None,
            via: None,
            admin: false,
            doc: String::new(),
            path: Some("/home/me/.local/bin/ptyro".into()),
            managed: None,
            move_to: None,
        };
        let codex = updates::Outdated {
            id: "codex".into(),
            name: "Codex".into(),
            program: "codex".into(),
            latest: "0.170.0".into(),
            version: "0.162.1".into(),
            path: None,
            ..me.clone()
        };
        app.got_updates(vec![me.clone(), codex.clone()], vec![me.clone(), codex], true);
        let t = app.toast.take().expect("the note");
        assert_eq!(
            t.text,
            "2 newer versions are out: ptyro 0.0.64, Codex 0.170.0 · C-q u updates them"
        );
        let f = app.health.findings.iter().find(|f| f.what == "updates ptyro").expect("found");
        assert!(f.text.contains("replaces /home/me/.local/bin/ptyro"), "{}", f.text);
        assert!(f.text.contains("upgrades the daemon live"), "{}", f.text);
        assert_eq!(f.fix, Some(crate::health::Fix::Update(vec!["ptyro".into()])));
        let i = app.health.findings.iter().position(|f| f.what == "updates ptyro").unwrap();
        app.fix(i);
        let words = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(app.last_cli, Some(words(&["update", "ptyro"])));
    }

    /// BOOT2: while the checklist's install runs, `U` (or a health fix, or `first.run`) never
    /// opens another: it says so.
    #[tokio::test]
    async fn a_running_setup_is_never_joined_by_a_second() {
        let mut app = app();
        app.setup_run =
            Some(crate::setup_list::SetupRun { split: "ptyro-install".into(), seen: false });
        app.open_setup(None);
        assert!(app.first_run.is_none(), "no second checklist");
        assert!(app.last_cli.is_none(), "no second install");
        assert!(
            app.toast.as_ref().unwrap().text.contains("setup is still running in ptyro-install")
        );
        app.open_first_run();
        assert!(app.first_run.is_none());
        // Still starting (not in the list yet): it waits.
        app.setup_tick();
        assert!(app.setup_run.is_some());
        // Seen, then gone: done.
        app.setup_run.as_mut().unwrap().seen = true;
        app.setup_tick();
        assert!(app.setup_run.is_none(), "done: the checklist can open again");
    }

    /// BOOT2: the checklist's split frees it as soon as it can't start, is closed, or ends;
    /// then the health badge looks again, after a run already in flight rather than not at all.
    #[tokio::test]
    async fn a_setup_split_that_fails_closes_or_dies_frees_the_checklist_and_the_badge_looks_again()
    {
        let mut app = app();
        let run =
            || Some(crate::setup_list::SetupRun { split: "ptyro-install".into(), seen: false });
        let spawned = || Pending::Spawned {
            split: None,
            spec: Box::new(SpawnSpec {
                name: Some("ptyro-install".into()),
                ..SpawnSpec::default()
            }),
            back: false,
        };
        // A doctor run is in flight, from before the install.
        app.health.running = true;
        // 1. The spawn failed: freed at once, no half-minute wait.
        app.setup_run = run();
        app.on_reply(AppMsg::Reply(
            spawned(),
            Err(ptyro_core::Error::new(ErrorCode::Internal, "no pty")),
        ));
        assert!(app.setup_run.is_none(), "a failed spawn frees it");
        // 1b. Asked to agree first: kept while the question is up (a `y` still starts it); a
        // `n` means it never starts, which frees it.
        app.setup_run = run();
        app.on_reply(AppMsg::Reply(
            spawned(),
            Err(ptyro_core::Error::new(ErrorCode::NeedsConfirmation, "another profile?")),
        ));
        assert!(app.setup_run.is_some(), "kept while the question is up");
        assert!(matches!(app.overlay, Some(Overlay::Confirm { action: Confirmed::Spawn(..), .. })));
        app.overlay_key(KeyEvent::new(KeyCode::Char { c: 'n' }, Mods::NONE));
        assert!(app.setup_run.is_none(), "declined: freed");
        // 2. Started, then closed by the person before the list ever showed it running.
        app.setup_run = run();
        let s = session("ptyro-install");
        app.on_reply(AppMsg::Reply(spawned(), Ok(Reply::Session { info: s.clone() })));
        assert!(app.setup_run.as_ref().unwrap().seen, "started");
        app.setup_tick();
        assert!(app.setup_run.is_some(), "still running");
        app.on_event(DomainEvent::SessionRemoved { id: s.id });
        app.setup_tick();
        assert!(app.setup_run.is_none(), "closed: freed");
        assert!(app.health.again, "the badge looks again once the run in flight is in");
        // 3. Started, then its program died.
        app.setup_run = run();
        let mut s = session("ptyro-install");
        app.on_reply(AppMsg::Reply(spawned(), Ok(Reply::Session { info: s.clone() })));
        s.state = ProcessState::Exited { code: Some(1), signal: None };
        app.upsert(s);
        app.setup_tick();
        assert!(app.setup_run.is_none(), "ended: freed");
        // The badge: the run in flight answers with what it saw before the install (claude
        // missing); it is looked at again, and that answer clears it.
        let stale = crate::health::Check {
            section: "agents".into(),
            name: "claude".into(),
            level: "problem".into(),
            text: "claude not found".into(),
        };
        app.got_health(vec![stale]);
        assert_eq!(app.health.findings.len(), 1);
        assert!(app.health.running && !app.health.again, "looking again now");
        app.got_health(Vec::new());
        assert!(app.health.findings.is_empty(), "the badge shows the install's result");
    }

    /// BOOT2: `app.setup` (`U`) is the checklist alone; `s` (not now) installs nothing and says
    /// how to come back.
    #[test]
    fn the_setup_checklist_alone_closes_with_not_now() {
        let mut app = app();
        assert_eq!(app.keymap.key_for("app.setup").as_deref(), Some("C-q U"));
        use crate::onboard::FirstRun;
        app.first_run = Some(FirstRun {
            setup: crate::setup_list::tests::one(&[]),
            only_setup: true,
            ..FirstRun::default()
        });
        let shown = screen(&app);
        assert!(shown.contains("Set up this computer"), "{shown}");
        assert!(shown.contains("⏎ install 4 · Space toggle"), "{shown}");
        assert!(!shown.contains("repositories"), "{shown}");
        app.first_run_key(KeyEvent::new(KeyCode::Char { c: 's' }, Mods::NONE));
        assert!(app.first_run.is_none());
        assert!(app.last_cli.is_none(), "nothing installed");
        assert!(app.toast.as_ref().unwrap().text.contains("C-q U opens the setup checklist"));
        assert!(!Config::path(&app.paths).exists(), "the checklist alone writes no config");
    }

    /// Setup running elsewhere (the machine's lock, held here as the web's run would): the
    /// checklist says so before anything is ticked, ⏎ counts nothing, and ⏎ starts no split.
    #[test]
    fn the_setup_checklist_says_when_setup_runs_elsewhere() {
        let mut app = app();
        // Its own state folder: the lock it holds would make every other test's checklist (all
        // in `app()`'s one folder, as are other test runs on this computer) see a setup running.
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        app.paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| h.clone())).unwrap();
        use crate::onboard::FirstRun;
        let held = ptyro_agents::setup::lock::Lock::take(&app.paths.state, "the web app").unwrap();
        let mut setup = crate::setup_list::tests::one(&[]);
        assert!(setup.check_busy(&app.paths.state));
        app.first_run = Some(FirstRun { setup, only_setup: true, ..FirstRun::default() });
        let shown = screen(&app);
        assert!(
            shown.contains("setup is already running on this computer (from the web app"),
            "{shown}"
        );
        assert!(shown.contains("⏎ done · Space toggle"), "{shown}");
        app.first_run_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        assert!(app.last_cli.is_none(), "no split");
        assert!(
            app.toast.as_ref().unwrap().text.contains("setup is already running (from the web app")
        );
        // Let go: the checklist installs again. Another test thread's fork can hold a copy of the
        // lock's descriptor until its exec, so wait for the lock to be free, not just dropped.
        drop(held);
        let free_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ptyro_agents::setup::lock::Lock::held_by(&app.paths.state).is_some() {
            assert!(std::time::Instant::now() < free_by, "the lock never came free");
            std::thread::yield_now();
        }
        app.first_run = Some(FirstRun {
            setup: crate::setup_list::tests::one(&[]),
            only_setup: true,
            ..FirstRun::default()
        });
        app.first_run_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        assert!(app.last_cli.is_some(), "a split once it's free");
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
        let shell = usize::from(crate::health::shell_integration_off(&app.config));
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
        // Shell integration switched off: the fix opens its setting, nothing is written.
        app.config.shell.integration = false;
        app.config.shell.command = vec!["/bin/zsh".into()];
        app.got_health(vec![check("terminal", "TERM", "ok", "xterm-256color")]);
        let f = app.health.findings.iter().find(|f| f.what == "shell integration");
        if let Some(f) = f {
            assert_eq!(f.fix, Some(crate::health::Fix::Setting("shell.integration")));
        }
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
        // The selected session, and a word typed.
        let s = session("worker");
        app.panes.get_mut(&1).unwrap().session = Some(s.id);
        app.sessions = vec![s];
        app.run(&Command { action: "checkpoint.diff", arg: None });
        typed(&mut app, "3\n");
        assert_eq!(app.last_cli.clone().unwrap(), ["checkpoint", "diff", "worker", "3"]);
        app.run(&Command { action: "session.events", arg: None });
        assert_eq!(app.last_cli.clone().unwrap(), ["events", "--session", "worker"]);
        // Nothing selected that it needs: it says what to pick.
        app.last_cli = None;
        app.run(&Command { action: "project.show", arg: None });
        assert!(app.last_cli.is_none());
        assert!(app.toast.as_ref().unwrap().text.contains("pick a project first"));
    }

    #[test]
    fn a_bad_name_is_flagged_as_it_is_typed_and_enter_keeps_it() {
        let mut app = app();
        let s = session("brisk-rook");
        app.sessions = vec![s.clone(), session("quiet-jay")];
        rename(&mut app, &s);
        assert!(screen(&app).contains("⏎ ok (unchanged)"));

        // What it can't hold becomes -, and the dialog says so.
        typed(&mut app, "/X");
        assert_eq!(prompt_text(&app), Some("brisk-rook-x"));
        assert!(screen(&app).contains("becomes - (a-z 0-9 . _ -)"), "{}", screen(&app));
        // What no one character fixes is flagged, and ⏎ doesn't throw the typing away.
        typed(&mut app, ".");
        let shown = screen(&app);
        assert!(shown.contains("✗ must not end with . or .lock"), "{shown}");
        typed(&mut app, "\n");
        assert_eq!(prompt_text(&app), Some("brisk-rook-x."));
        typed(&mut app, "\x08");
        assert_eq!(prompt_text(&app), Some("brisk-rook-x"));
        assert!(app.prompt_check(&PromptKind::Rename(s.id), "brisk-rook-x").is_ok());
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

        // Names are unique per project: another project's session may share it.
        let mut elsewhere = session("calm-crow");
        elsewhere.project = Some("p9".into());
        app.sessions.push(elsewhere);
        assert!(app.prompt_check(&PromptKind::Rename(s.id), "calm-crow").is_ok());
    }

    #[test]
    fn free_text_prompts_take_anything() {
        let app = app();
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
        let b = placed("zephyr", Some("p2"), Some("p2-root"));
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
        let (_agent, other) = with_agents(&mut app);
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
    }

    #[test]
    fn a_session_the_daemon_answers_with_later_tucks_the_drawer_away_too() {
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
        assert!(app.drawer.is_some());
        // `C-q C-c`: the new shell shows when the daemon answers, after the command returned.
        app.run(&Command { action: "shells.new", arg: None });
        assert!(app.drawer.is_some(), "nothing new on screen yet");
        let two = loose("zsh", 2);
        app.sessions.push(two.clone());
        app.on_reply(AppMsg::Reply(
            Pending::Spawned { split: None, spec: Box::default(), back: false },
            Ok(Reply::Session { info: two.clone() }),
        ));
        assert_eq!(app.focused_session(), Some(two.id));
        assert!(app.drawer.is_none(), "the drawer tucked away");
        assert!(app.sessions.iter().any(|s| s.id == shell.id), "its shell keeps running");
    }

    #[test]
    fn moving_focus_between_shown_panes_keeps_the_drawer() {
        let mut app = app();
        auto_shell(&mut app);
        let (_agent, _other) = with_agents(&mut app);
        let shell = quick("myrepo.shell", Some("p1-root"));
        app.sessions.push(shell.clone());
        app.run(&Command { action: "shell.here", arg: None });
        app.on_reply(AppMsg::Reply(
            Pending::Quick(app.config.ui.shell_style),
            Ok(Reply::Session { info: shell.clone() }),
        ));
        let host = app.drawer.expect("open").host;
        assert_ne!(app.focused, host);
        // Back to the agent above: the drawer stays.
        app.run(&Command { action: "pane.up", arg: None });
        assert_eq!(app.focused, host);
        assert!(app.drawer.is_some(), "the drawer stays while its host is used");
        app.run(&Command { action: "pane.down", arg: None });
        assert!(app.drawer.is_some());
    }

    /// The doctor's stale worktrees are the gc reminder: said once in so many words with the key
    /// that cleans up, ⏎ in the health list opens the clean-up list, and a worktree going asks
    /// the doctor again so the badge doesn't count what was just cleaned up.
    #[test]
    fn stale_worktrees_are_announced_once_and_rechecked_when_one_goes() {
        let mut app = app();
        let stale = || {
            vec![crate::health::Check {
                section: "worktrees".into(),
                name: "stale".into(),
                level: "warn".into(),
                text: "2 stale worktrees, taking 4.0 GiB (2 merged): `ptyro gc` removes the ones you say".into(),
            }]
        };
        app.got_health(stale());
        let toast = app.toast.as_ref().map(|t| t.text.clone()).unwrap_or_default();
        assert!(toast.starts_with("2 stale worktrees, taking 4.0 GiB (2 merged): "), "{toast}");
        assert!(toast.ends_with("G cleans up"), "{toast}");
        assert_eq!(app.health.findings[0].fix, Some(crate::health::Fix::Action("worktree.gc")));
        app.toast = None;
        app.got_health(stale());
        assert!(app.toast.is_none(), "not again while it stays");
        app.health.last = Some(std::time::Instant::now());
        app.on_event(DomainEvent::WorktreeRemoved { id: "p1-gone".into() });
        assert!(app.health.last.is_none(), "asked again soon");
        // The which-key menu shows both, with their keys (UX1.14).
        app.prefix = true;
        app.prefix_at = Some(Instant::now() - Duration::from_secs(5));
        let shown = screen(&app);
        assert!(shown.contains("G clean up") && shown.contains("f sidebar filter"), "{shown}");
    }

    #[test]
    fn the_sidebar_filter_cycles_by_key_and_click_and_is_saved_with_the_view() {
        use ptyro_core::status::{Confidence, SignalSource, StatusRecord};
        let mut app = app();
        let (a, b) = with_agents(&mut app);
        app.sessions.iter_mut().find(|s| s.id == a).unwrap().status =
            Some(StatusRecord::new(AgentStatus::Working, Confidence::High, SignalSource::Hook));
        let listed = |app: &App| app.rows().iter().filter_map(Row::session).collect::<Vec<_>>();
        assert_eq!(app.show, Show::All);
        assert_eq!(listed(&app), [a, b]);
        let strip = screen(&app);
        assert!(
            strip.contains("all 2") && strip.contains("running 2") && strip.contains("active 1"),
            "{strip}"
        );
        // The key: all → running → active → all.
        app.run(&Command { action: "sidebar.filter", arg: None });
        assert_eq!((app.show, listed(&app)), (Show::Running, vec![a, b]));
        app.run(&Command { action: "sidebar.filter", arg: None });
        assert_eq!((app.show, listed(&app)), (Show::Active, vec![a]));
        // A click on a chip, on the line under the title.
        let side = app.sidebar_area().unwrap();
        let chips = crate::ui::filter_chips(&app, side.width);
        let (_, _, from, _) = chips.iter().find(|c| c.0 == Show::Running).unwrap();
        app.on_mouse(&mouse(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            side.x + from + 1,
            side.y + 1,
        ));
        assert_eq!(app.show, Show::Running);
        // Saved with the view and put back.
        app.run(&Command { action: "sidebar.show_active", arg: None });
        let [Request::SaveView { view, .. }, _] = app.view_requests().unwrap() else { panic!() };
        app.show = Show::All;
        app.apply_view(Some(view));
        assert_eq!(app.show, Show::Active);
        // Nothing active: it says so, and how to get the rest back.
        app.sessions.iter_mut().for_each(|s| s.status = None);
        let empty = screen(&app);
        assert!(empty.contains("nothing active right now"), "{empty}");
        app.run(&Command { action: "sidebar.show_all", arg: None });
        assert_eq!(listed(&app), [a, b]);
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

    #[test]
    fn the_load_average_is_on_the_status_bar_while_the_daemon_sends_it() {
        let mut app = app();
        app.config.ui.status_bar = true;
        let load = ptyro_protocol::HostLoad { avg: [52, 148, 1240], cpus: 8 };
        app.on_event(DomainEvent::HostLoad { load: Some(load) });
        assert!(screen(&app).contains("0.52 1.48 12.40"), "{}", screen(&app));
        app.config.ui.load = false;
        assert!(!screen(&app).contains("0.52"), "off here hides it at once");
        app.config.ui.load = true;
        app.on_event(DomainEvent::HostLoad { load: None });
        assert!(!screen(&app).contains("0.52"), "off in the daemon: gone");
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

    /// A session you pick while split takes the focused pane: the split stays (owner,
    /// 2026-11-18). Something new still opens full sized, the split kept for ` (below).
    #[test]
    fn a_session_picked_in_a_split_takes_the_focused_pane() {
        let mut app = app();
        let (a, b, right) = split_two(&mut app);
        let c = placed("zephyr", Some("p1"), Some("p1-root"));
        app.sessions.push(c.clone());
        app.pick(c.id);
        assert_eq!(app.layout.panes().len(), 2, "still a split");
        assert_eq!((app.focused, app.focused_session()), (right, Some(c.id)));
        let shown: HashSet<SessionId> = app.panes.values().filter_map(|p| p.session).collect();
        assert_eq!(shown, HashSet::from([a, c.id]), "{b} made way, its neighbour stays");
        assert!(app.kept.is_none(), "nothing replaced the split");
        // ` goes back in that pane to what it showed.
        act(&mut app, "session.last");
        assert_eq!((app.layout.panes().len(), app.focused_session()), (2, Some(b)));
        // One already on screen is focused where it is.
        app.pick(a);
        assert_eq!((app.layout.panes().len(), app.focused_session()), (2, Some(a)));
    }

    /// `v` / `s` ask what goes in the new pane: ⏎ the clone, `+ new shell` there, or a session
    /// not on screen, so an agent and a shell sit side by side.
    #[test]
    fn a_split_offers_a_clone_a_shell_or_a_session() {
        let mut app = app();
        let (a, b) = with_agents(&mut app);
        let s = app.sessions.iter_mut().find(|s| s.id == a).unwrap();
        (s.kind, s.agent) = (SessionKind::Agent, Some("claude".into()));
        let wt = s.worktree.clone();
        app.show(a, None);
        act(&mut app, "pane.split_right");
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!("no picker") };
        let targets: Vec<Target> =
            (0..palette.filtered.len()).map(|i| palette.item(i).target.clone()).collect();
        assert_eq!(targets[0], Target::Clone, "⏎ is the clone: {targets:?}");
        assert!(targets.contains(&Target::NewShell), "{targets:?}");
        assert!(targets.contains(&Target::Session(b)), "{targets:?}");
        assert!(!targets.contains(&Target::Session(a)), "not what is on screen");
        // A new shell: in the agent's worktree, in the new pane.
        app.overlay = None;
        while app.rx.try_recv().is_ok() {}
        app.picked(Target::NewShell, Purpose::Split(Dir::Right));
        let mut sent = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::Spawned { split, spec, .. }, _) = m {
                sent = Some((split, spec));
            }
        }
        let (split, spec) = sent.expect("a shell was started");
        assert_eq!(split, Some(Dir::Right));
        assert!(spec.agent.is_none() && spec.argv.is_empty(), "a shell: {spec:?}");
        assert_eq!(spec.worktree, wt);
        // A session: beside it, both on screen.
        app.picked(Target::Session(b), Purpose::Split(Dir::Down));
        let shown: HashSet<SessionId> = app.panes.values().filter_map(|p| p.session).collect();
        assert_eq!(shown, HashSet::from([a, b]));
        assert_eq!(app.focused_session(), Some(b));
    }

    #[test]
    fn a_new_session_opens_full_sized_and_back_brings_the_split_whole() {
        let mut app = app();
        let (a, b, _) = split_two(&mut app);
        let fresh = placed("zephyr", Some("p1"), Some("p1-root"));
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
        let fresh = placed("zephyr", Some("p1"), Some("p1-root"));
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
        let fresh = placed("zephyr", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        let saved = serde_json::to_value(SavedView {
            v: 1,
            layout: app.layout.clone(),
            panes: app.panes.iter().map(|(k, p)| (*k, p.session.map(|s| s.full()))).collect(),
            focus: app.focused,
            zoom: false,
            sidebar: true,
            sidebar_width: 30,
            show: Default::default(),
            scope: None,
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
        // It asks what goes there; ⏎ takes the first choice, the clone.
        assert!(matches!(app.overlay, Some(Overlay::Picker { purpose: Purpose::Split(_), .. })));
        key(&mut app, KeyCode::Enter, Mods::NONE);
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
        assert!(spec.prompt.is_none(), "a fresh conversation");
        assert!(app.overlay.is_none(), "no picker");
    }

    #[test]
    fn an_editor_opens_full_sized_and_gives_the_screen_back_when_it_exits() {
        let mut app = app();
        let (_, b, _) = split_two(&mut app);
        let mut editor = placed("vim", Some("p1"), Some("p1-root"));
        editor.kind = SessionKind::Command;
        spawned(&mut app, &editor, true);
        assert_eq!(app.layout.panes().len(), 1);
        editor.state = ProcessState::Exited { code: Some(0), signal: None };
        app.on_event(DomainEvent::SessionExited { info: editor });
        assert_eq!(app.focused_session(), Some(b), "back to the session that had focus");
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
        let codex = ptyro_core::session::LastLaunch {
            agent: Some("codex".into()),
            profile: Some("work".into()),
            ..Default::default()
        };
        app.again = Some((Some("p1-root".into()), codex.clone()));
        assert_eq!(app.again_label(), "codex@work");
        // Another worktree's answer isn't this one's.
        app.again = Some((Some("p2-root".into()), codex));
        assert_eq!(app.again_label(), "claude");
        for a in ["shell.new", "session.again"] {
            assert!(app.keymap.key_for(a).is_some(), "{a} has a key");
        }
        // C-n is quick new, after the prefix and in the sidebar.
        for t in [Table::Prefix, Table::Nav] {
            assert_eq!(
                app.keymap
                    .lookup(t, ptyro_core::config::parse_key("C-n").unwrap())
                    .map(|c| c.action),
                Some("session.again")
            );
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
        // A session outside every project is in the workspace's SHELLS (SPEC §23.3).
        assert_eq!(rows[at + 1], Row::Shells("laptop2:work".into()));
        assert_eq!(rows[at + 2], Row::Session(remote.id), "its session under its header");
        let shown = screen(&app);
        assert!(shown.contains("laptop2 / work"), "{shown}");
        assert!(!shown.contains("laptop2:fix-flaky-test"), "no machine twice in its rows: {shown}");
        // Selecting there makes it the workspace you're in: `n` goes there.
        app.focus = Focus::Sidebar;
        app.set_sel(&rows, at + 1);
        assert_eq!(app.workspace, "laptop2:work");
        // The title, under the tab strip (two machines, SPEC §11.2k).
        assert!(screen(&app).lines().nth(1).unwrap().contains("laptop2 / work"));
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

    /// TABS (SPEC §11.2k): the strip over the sidebar, picked by a click, by keys and the
    /// palette; the tree follows it; saved with the view; going to something outside it
    /// switches tab; a new workspace defaults to it; a tab that goes falls back to `all`.
    #[test]
    fn the_tab_strip_picks_a_machine_and_profile_and_going_elsewhere_follows() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let mut app = app();
        app.machine = Some("laptop1".into());
        // One machine, one profile: no strip, nothing moves down.
        with_project(&mut app);
        let ours = placed("mine", Some("p1"), Some("p1-root"));
        app.sessions.push(ours.clone());
        assert!(crate::ui::tab_strip(&app, 30).is_none());
        assert_eq!(crate::ui::sidebar_head(&app), 2);
        act(&mut app, "sidebar.tab_all");
        assert!(app.toast.as_ref().is_some_and(|t| t.text.starts_with("no tabs")));
        app.workspaces.clear();
        app.projects.clear();
        app.worktrees.clear();
        let mut blocked = with_laptop2(&mut app, ptyro_core::model::MachineState::Offline);
        // Another machine's profile arrives with its name on (SPEC §16.4b).
        blocked.profile = "laptop2:default".into();
        blocked.kind = SessionKind::Agent;
        blocked.status = Some(ptyro_core::status::StatusRecord::new(
            AgentStatus::Blocked,
            ptyro_core::status::Confidence::High,
            ptyro_core::status::SignalSource::Hook,
        ));
        app.sessions.retain(|s| s.id != blocked.id);
        app.sessions.push(blocked.clone());
        let here = Scope::new(None, "default");
        let there = Scope::new(Some("laptop2".into()), "default");
        // The strip, first line: all, this machine, laptop2 with what waits there and its link.
        app.sidebar_width = 50;
        let shown = screen(&app);
        let first = shown.lines().next().unwrap();
        assert!(
            first.contains(" all ⚠1 ")
                && first.contains(" laptop1 ")
                && first.contains(" laptop2 ⚠1 offline "),
            "{first}"
        );
        // The keys are said where the sidebar's are.
        app.focus = Focus::Sidebar;
        assert!(screen(&app).contains("{} tab"), "{}", screen(&app));
        // A click on laptop2's tab: only its workspace is listed.
        let side = app.sidebar_area().unwrap();
        let strip = crate::ui::tab_strip(&app, side.width).unwrap();
        let chip = strip.chips.iter().find(|c| c.scope.as_ref() == Some(&there)).unwrap();
        app.on_mouse(&mouse(K::Down(MouseButton::Left), side.x + chip.from + 1, side.y));
        assert_eq!(app.scope.as_ref(), Some(&there));
        let rows = app.rows();
        assert!(!rows.contains(&Row::Workspace("main".into())), "{rows:?}");
        assert!(rows.contains(&Row::Session(blocked.id)));
        assert_eq!(app.ordered(), [blocked.id], "numbers and j/k follow the tab");
        // The filter chips moved a line down with the strip: a click there still filters.
        let chips = crate::ui::filter_chips(&app, side.width);
        let (_, _, from, _) = chips.iter().find(|c| c.0 == Show::Running).unwrap();
        app.on_mouse(&mouse(K::Down(MouseButton::Left), side.x + from + 1, side.y + 2));
        assert_eq!(app.show, Show::Running);
        app.show = Show::All;
        // Keys: round, `all` among them.
        act(&mut app, "sidebar.tab_next");
        assert_eq!(app.scope, None);
        act(&mut app, "sidebar.tab_next");
        assert_eq!(app.scope.as_ref(), Some(&here));
        act(&mut app, "sidebar.tab_prev");
        assert_eq!(app.scope, None);
        act(&mut app, "sidebar.tab_prev");
        assert_eq!(app.scope.as_ref(), Some(&there));
        // The palette has each tab.
        act(&mut app, "palette");
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!("no palette") };
        let tabs: Vec<String> = (0..palette.filtered.len())
            .map(|i| palette.item(i))
            .filter(|it| matches!(it.target, Target::Tab(_)))
            .map(|it| it.label.clone())
            .collect();
        assert_eq!(tabs, ["tab all", "tab laptop1", "tab laptop2"]);
        app.overlay = None;
        app.picked(Target::Tab(Some(here.clone())), Purpose::Palette);
        assert_eq!(app.scope.as_ref(), Some(&here));
        // Saved with the view; one that is gone by the next load is `all`.
        let saved = serde_json::to_value(app.view_requests().unwrap()[0].clone()).unwrap();
        let view = saved["view"].clone();
        assert_eq!(view["scope"], serde_json::json!({"profile": "default"}));
        app.scope = None;
        app.apply_view(Some(view.clone()));
        assert_eq!(app.scope.as_ref(), Some(&here));
        let mut gone = view.clone();
        gone["scope"] = serde_json::json!({"profile": "nobody"});
        app.apply_view(Some(gone));
        assert_eq!(app.scope_shown(), None);
        // Going to a session outside the tab: its tab.
        app.scope = Some(here.clone());
        app.pick(blocked.id);
        assert_eq!(app.scope.as_ref(), Some(&there));
        assert_eq!(app.selected_row(), Some(Row::Session(blocked.id)));
        // `w` to a workspace outside it: the workspace's tab.
        app.picked(Target::Workspace("main".into()), Purpose::Workspace);
        assert_eq!(app.scope.as_ref(), Some(&here));
        assert_eq!(app.selected_row(), Some(Row::Workspace("main".into())));
        // A new session that lands outside the tab (the wizard picked another profile): its
        // tab, as the daemon's answer shows it.
        app.scope = Some(there.clone());
        let fresh = placed("fresh", Some("p1"), Some("p1-root"));
        let spawned = Pending::Spawned { split: None, spec: Box::default(), back: false };
        app.on_reply(AppMsg::Reply(spawned, Ok(Reply::Session { info: fresh.clone() })));
        assert_eq!(app.scope.as_ref(), Some(&here));
        assert_eq!(app.selected_row(), Some(Row::Session(fresh.id)));
        // A new workspace under laptop2's tab: laptop2 is the machine picked.
        app.scope = Some(there.clone());
        app.picked(Target::NewWorkspace, Purpose::Workspace);
        let Some(Overlay::Picker { palette, purpose: Purpose::NewWorkspaceOn }) = &app.overlay
        else {
            panic!("no machine picker")
        };
        assert_eq!(palette.selected_target(), Some(&Target::Machine("laptop2".into())));
        app.overlay = None;
        // laptop2's last workspace and session go: its tab does, and `all` is shown.
        app.workspaces.retain(|w| w.machine.is_none());
        app.sessions.retain(|s| s.machine.is_none());
        assert_eq!(app.scope_shown(), None);
    }

    /// Two tabs, laptop1 and laptop2 (each machine's `default`), with laptop2's session.
    fn two_machines(app: &mut App) -> (Scope, Scope, SessionInfo) {
        app.machine = Some("laptop1".into());
        let mut far = with_laptop2(app, ptyro_core::model::MachineState::Online);
        app.sessions.retain(|s| s.id != far.id);
        far.profile = "laptop2:default".into();
        app.sessions.push(far.clone());
        (Scope::new(None, "default"), Scope::new(Some("laptop2".into()), "default"), far)
    }

    /// After a restart the linked machine's mirror is empty until its link is back: the tab
    /// saved on it shows `all` meanwhile, stays picked and saved, and comes back with it.
    #[test]
    fn a_saved_linked_machine_tab_waits_for_its_mirror_instead_of_being_lost() {
        let mut app = app();
        let (_, there, far) = two_machines(&mut app);
        app.set_scope(Some(there.clone()));
        let view =
            serde_json::to_value(app.view_requests().unwrap()[0].clone()).unwrap()["view"].clone();
        let (theirs, rest): (Vec<WorkspaceInfo>, Vec<WorkspaceInfo>) =
            app.workspaces.drain(..).partition(|w| w.machine.is_some());
        app.workspaces = rest;
        app.sessions.retain(|s| s.machine.is_none());
        app.scope = None;
        app.apply_view(Some(view));
        assert_eq!(app.scope.as_ref(), Some(&there), "still the one picked");
        assert_eq!(app.scope_shown(), None, "all, meanwhile");
        assert!(app.rows().contains(&Row::Workspace("main".into())));
        let _ = screen(&app);
        let saved = serde_json::to_value(app.view_requests().unwrap()[0].clone()).unwrap();
        assert_eq!(saved["view"]["scope"], serde_json::to_value(&there).unwrap(), "saved as is");
        // The mirror arrives: the tab is back.
        app.workspaces.extend(theirs);
        app.sessions.push(far.clone());
        assert_eq!(app.scope_shown(), Some(&there));
        let rows = app.rows();
        assert!(rows.contains(&Row::Session(far.id)), "{rows:?}");
        assert!(!rows.contains(&Row::Workspace("main".into())), "{rows:?}");
    }

    /// A tab change with the sidebar focused moves the workspace you're in to one it shows.
    #[test]
    fn a_tab_change_in_the_sidebar_moves_the_workspace_you_are_in_to_one_it_shows() {
        let mut app = app();
        let ws = |name: &str, profile: &str, projects: &[&str]| -> WorkspaceInfo {
            serde_json::from_value(serde_json::json!({
                "id": name, "name": name, "profile": profile, "projects": projects,
                "notify": "all", "created_ms": 0,
            }))
            .unwrap()
        };
        app.projects.push(project("p1", "myrepo"));
        app.worktrees.push(root("p1"));
        app.workspaces = vec![ws("job", "work", &["p1"]), ws("home", "me", &[])];
        let mut build = placed("build", Some("p1"), Some("p1-root"));
        build.profile = "work".into();
        app.sessions.push(build);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Workspace("job".into())).unwrap();
        app.set_sel(&rows, at);
        assert_eq!(app.workspace, "job");
        act(&mut app, "sidebar.tab_next");
        assert_eq!(app.scope_shown(), Some(&Scope::new(None, "me")));
        assert_eq!(app.workspace, "home", "a workspace `[me]` lists");
        assert_eq!(app.selected_row(), Some(Row::Workspace("home".into())));
    }

    /// Revealing a session the filter hides anyway doesn't switch tab (SPEC §11.2k).
    #[test]
    fn a_reveal_switches_no_tab_for_a_session_the_filter_hides() {
        let mut app = app();
        let (here, there, far) = two_machines(&mut app);
        app.sessions.iter_mut().find(|s| s.id == far.id).unwrap().state =
            ProcessState::Exited { code: Some(0), signal: None };
        app.scope = Some(here.clone());
        app.show = Show::Running;
        app.reveal(far.id);
        assert_eq!(app.scope_shown(), Some(&here), "hidden there too: the tab stays");
        app.show = Show::All;
        app.reveal(far.id);
        assert_eq!(app.scope_shown(), Some(&there));
    }

    /// A trackpad's flick over the strip moves one tab, not one per event.
    #[test]
    fn the_wheel_over_the_strip_moves_one_tab_per_turn() {
        use crossterm::event::MouseEventKind as K;
        let mut app = app();
        let (here, there, _) = two_machines(&mut app);
        let side = app.sidebar_area().unwrap();
        let wheel = |app: &mut App| app.on_mouse(&mouse(K::ScrollDown, side.x + 1, side.y));
        for _ in 0..5 {
            wheel(&mut app);
        }
        assert_eq!(app.scope_shown(), Some(&here), "one tab on from `all`");
        // A moment later the next turn moves on.
        app.tab_wheel_at = Instant::now().checked_sub(TAB_WHEEL_GAP * 2);
        wheel(&mut app);
        assert_eq!(app.scope_shown(), Some(&there));
    }

    /// A reveal waiting for its row doesn't take back a tab you picked meanwhile.
    #[test]
    fn a_pending_reveal_leaves_a_tab_picked_meanwhile_alone() {
        let mut app = app();
        let (here, there, far) = two_machines(&mut app);
        // laptop2's workspace isn't mirrored yet: its session is listed under ours under `all`,
        // so laptop2's tab can't place it.
        app.workspaces.retain(|w| w.machine.is_none());
        app.scope = Some(here.clone());
        app.reveal(far.id);
        assert_eq!(app.scope_shown(), Some(&there));
        assert!(app.revealing.is_some_and(|r| r.2), "waits, and may still switch");
        app.set_scope(Some(here.clone()));
        app.retry_reveal();
        assert_eq!(app.scope_shown(), Some(&here), "the tab picked stays");
        assert!(app.revealing.is_some_and(|r| !r.2), "it still selects its row once it can");
        // Without the pick, a retry would have switched.
        app.revealing.as_mut().unwrap().2 = true;
        app.retry_reveal();
        assert_eq!(app.scope_shown(), Some(&there));
    }

    /// The palette's worktree jump shows a session the tab lists, so it switches no tab; a
    /// linked machine's tab carries its usage, as this machine's do.
    #[test]
    fn a_worktree_jump_keeps_the_tab_and_every_tab_carries_its_usage() {
        let mut app = app();
        with_project(&mut app);
        let mut build = placed("build", Some("p1"), Some("p1-root"));
        build.profile = "work".into();
        let mut side = placed("side", Some("p1"), Some("p1-root"));
        side.profile = "me".into();
        app.sessions.extend([build, side.clone()]);
        let me = Scope::new(None, "me");
        app.scope = Some(me.clone());
        app.picked(Target::Worktree("p1-root".into()), Purpose::Palette);
        assert_eq!(app.scope_shown(), Some(&me));
        assert_eq!(app.focused_session(), Some(side.id));
        // Usage: laptop2's comes named as its tab names its profile.
        let mut app = app_with_laptop2_usage();
        app.sidebar_width = 60;
        let first = screen(&app).lines().next().unwrap().to_owned();
        assert!(first.contains(" laptop2 42% "), "{first}");
    }

    fn app_with_laptop2_usage() -> App {
        let mut app = app();
        two_machines(&mut app);
        let usage = ptyro_core::status::Usage {
            profile: "laptop2:default".into(),
            machine: Some("laptop2".into()),
            agent: "claude".into(),
            windows: vec![ptyro_core::status::UsageWindow {
                name: "5h".into(),
                used_percent: 42,
                resets_at_ms: None,
            }],
            ..Default::default()
        };
        app.usage.insert((usage.profile.clone(), usage.agent.clone()), usage);
        app
    }

    #[test]
    fn a_strip_wider_than_the_sidebar_keeps_the_selected_tab_in_sight() {
        let mut app = app();
        app.machine = Some("laptop1".into());
        for p in ["alpha", "bravo", "charlie", "delta", "echo"] {
            app.workspaces.push(
                serde_json::from_value(serde_json::json!({
                    "id": p, "name": p, "profile": p, "projects": [], "notify": "all",
                    "created_ms": 0,
                }))
                .unwrap(),
            );
        }
        let strip = crate::ui::tab_strip(&app, 20).unwrap();
        assert_eq!(strip.chips[0].scope, None);
        assert!(strip.left.is_none() && strip.right == Some(19), "more to the right");
        app.scope = Some(Scope::new(None, "echo"));
        let strip = crate::ui::tab_strip(&app, 20).unwrap();
        let last = strip.chips.last().unwrap();
        assert_eq!(last.scope, app.scope, "the selected one shows");
        assert!(last.to <= 20 && strip.left == Some(0) && strip.right.is_none());
        // `‹` moves back one.
        app.tab_click(0, 20);
        assert_eq!(app.scope, Some(Scope::new(None, "delta")));
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
        let Some(Overlay::Form(form)) = &app.overlay else { panic!("the form") };
        assert!(form.title.contains("on laptop2"), "{}", form.title);
        // Taken there already: said on ⏎, and the form stays.
        let typed = |app: &mut App, s: &str| {
            for c in s.chars() {
                app.overlay_key(KeyEvent::new(KeyCode::Char { c }, Mods::NONE));
            }
        };
        typed(&mut app, "work");
        app.overlay_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        let Some(Overlay::Form(form)) = &app.overlay else { panic!("still the form") };
        assert!(form.error.as_deref().unwrap_or("").contains("exists"), "{:?}", form.error);
        while app.rx.try_recv().is_ok() {}
        app.overlay_key(KeyEvent::new(KeyCode::Char { c: 'u' }, Mods(Mods::CTRL)));
        typed(&mut app, "spikes");
        app.overlay_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
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

    fn chat_info(id: &str) -> ptyro_core::chats::ChatInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "handle": id, "workspace": "main", "title": format!("chat {id}"),
            "title_src": "user", "agent": "claude", "profile": "default", "tools": "chat",
            "pinned": false, "archived": false, "incognito": false,
            "nodes": 2, "unread": 0, "created_ms": 0, "last_ms": 0, "cost_micro_usd": 0,
        }))
        .unwrap()
    }

    fn shown_chat(app: &App) -> Option<String> {
        match &app.chat_panel {
            Some(crate::chats::ChatPanel::View(v)) => v.id.clone(),
            _ => None,
        }
    }

    fn chat_token(app: &App) -> u64 {
        let Some(crate::chats::ChatPanel::View(v)) = &app.chat_panel else { panic!("no chat") };
        v.token
    }

    fn toast_text(app: &App) -> String {
        app.toast.as_ref().map(|t| t.text.clone()).unwrap_or_default()
    }

    #[test]
    fn a_reply_to_a_message_sent_in_one_chat_never_reopens_it_from_another() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa"), chat_info("bbbb")];
        app.open_chat("aaaa");
        // Sent in A, then B opened before the reply came: B stays.
        app.open_chat("bbbb");
        let sent = Pending::ChatSent { chat: "aaaa".into(), text: "hi".into() };
        app.handle_reply(sent, Ok(Reply::Chat { info: chat_info("aaaa") }));
        assert_eq!(shown_chat(&app).as_deref(), Some("bbbb"));
    }

    #[test]
    fn retry_replaces_the_last_answer_and_edit_says_the_last_message_again() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa")];
        app.open_chat("aaaa");
        let answer = |id: &str, text: &str| {
            ptyro_core::chat::ChatItem::new(
                id,
                ptyro_core::chat::ChatBody::Assistant { text: text.into(), streaming: false },
            )
        };
        app.chat_live(
            "aaaa".into(),
            1,
            true,
            vec![
                user_item("n1", "first"),
                answer("a1", "one"),
                user_item("n2", "second"),
                answer("a2", "two"),
            ],
        );
        // Edit: the last message, at once (no picker), its text ready to change.
        act(&mut app, "chat.edit");
        let Some(Overlay::Prompt { kind: PromptKind::ChatEdit(c, n), text, title }) = &app.overlay
        else {
            panic!("asks for the new words")
        };
        assert_eq!((c.as_str(), n.as_str(), text.as_str()), ("aaaa", "n2", "second"));
        assert!(title.contains("replaced"), "{title}");
        app.overlay = None;
        asked(&mut app);
        act(&mut app, "chat.retry");
        assert!(asked(&mut app).iter().any(
            |p| matches!(p, Pending::Done(m) if m == "asked again: the new answer replaces the last one")
        ));
    }

    /// A key typed at the terminal, through everything that routes it.
    fn press(app: &mut App, c: char, mods: crossterm::event::KeyModifiers) {
        app.on_key(&crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char(c), mods));
    }

    fn draft(app: &App) -> (String, usize) {
        let Some(crate::chats::ChatPanel::View(v)) = &app.chat_panel else { panic!("no chat") };
        (v.draft.clone(), v.at.min(v.draft.chars().count()))
    }

    #[test]
    fn the_chats_heading_folds_like_a_project_and_a_chat_opened_from_the_sidebar_takes_the_keys() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        with_project(&mut app);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Chats("main".into())).expect("CHATS");
        app.set_sel(&rows, at);
        let key = crate::tree::chats_key("main");
        app.run(&Command { action: "nav.open", arg: None });
        assert!(app.collapsed.contains(&key), "⏎ folds it");
        assert!(app.chat_panel.is_none() && app.focus == Focus::Sidebar, "and opens nothing");
        app.run(&Command { action: "nav.select", arg: None });
        assert!(!app.collapsed.contains(&key), "Space unfolds it");
        // `m` in the sidebar: the new chat has the keyboard, not the sidebar it came from.
        press(&mut app, 'm', M::NONE);
        assert!(matches!(app.chat_panel, Some(crate::chats::ChatPanel::View(_))));
        assert_eq!(app.focus, Focus::Pane);
        press(&mut app, 'j', M::NONE);
        assert_eq!(draft(&app).0, "j", "typed into the chat, not a sidebar move");
        // The list opened from there too.
        app.focus = Focus::Sidebar;
        app.run(&Command { action: "chat.list", arg: None });
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn z_folds_the_whole_sidebar_and_again_puts_back_what_was_open() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        with_project(&mut app);
        app.projects.push(project("p2", "other"));
        app.worktrees.push(root("p2"));
        app.workspaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "w2", "name": "side", "profile": "default", "projects": ["p2"],
                "notify": "all", "created_ms": 0,
            }))
            .unwrap(),
        );
        let a = placed("claude", Some("p1"), Some("p1-root"));
        let b = placed("codex", Some("p2"), Some("p2-root"));
        app.sessions = vec![a.clone(), b.clone()];
        // One thing folded by hand first: it is what comes back.
        app.collapsed.insert("p2-root".into());
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Session(a.id)).unwrap();
        app.set_sel(&rows, at);

        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(
            app.rows(),
            [Row::Workspace("main".into()), Row::Workspace("side".into())],
            "one line a workspace"
        );
        assert_eq!(app.selected_row(), Some(Row::Workspace("main".into())), "where it folded");
        // Every level is folded, so a workspace opened by hand shows its projects folded.
        app.collapsed.remove(&crate::tree::fold_key("main"));
        assert_eq!(
            app.rows(),
            [
                Row::Workspace("main".into()),
                Row::Project("p1".into()),
                Row::Shells("main".into()),
                Row::Chats("main".into()),
                Row::Workspace("side".into()),
            ]
        );
        app.collapsed.insert(crate::tree::fold_key("main"));

        press(&mut app, 'Z', M::SHIFT);
        let rows = app.rows();
        assert!(rows.contains(&Row::Session(a.id)), "open again: {rows:?}");
        assert!(rows.contains(&Row::Worktree("p2-root".into())));
        assert!(!rows.contains(&Row::Session(b.id)), "folded by hand before, still folded");
        assert_eq!(app.collapsed, HashSet::from(["p2-root".to_owned()]));
        assert_eq!(app.selected_row(), Some(Row::Workspace("main".into())));

        // After the prefix too, from a pane, and both help screens list it.
        app.focus = Focus::Pane;
        app.run(&Command { action: "sidebar.fold_all", arg: None });
        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.keymap.key_in(Table::Prefix, "sidebar.fold_all").as_deref(), Some("C-z"));
        app.prefix = true;
        app.prefix_at = Some(Instant::now() - Duration::from_secs(5));
        let shown = screen(&app);
        assert!(shown.contains("C-z fold all"), "the which-key menu: {shown}");
        app.prefix = false;
        app.prefix_at = None;
        let help: String = crate::ui::help_lines(&app, "fold_all")
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>() + "\n")
            .collect();
        // Each line starts with its key, in 14 columns.
        let keys: Vec<String> = help
            .lines()
            .filter(|l| l.contains("sidebar.fold_all"))
            .map(|l| l.chars().take(14).collect::<String>().trim().to_owned())
            .collect();
        assert!(keys.iter().any(|k| k == "Z"), "{help}");
        assert!(keys.iter().any(|k| k.ends_with(" C-z")), "{help}");
    }

    #[test]
    fn what_arrives_while_the_sidebar_is_folded_opens_with_the_rest() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        app.focus = Focus::Sidebar;
        press(&mut app, 'Z', M::SHIFT);
        let toast = app.toast.as_ref().map(|t| t.text.clone()).unwrap_or_default();
        assert_eq!(toast, "nothing in the sidebar to fold");
        with_project(&mut app);
        let a = placed("claude", Some("p1"), Some("p1-root"));
        app.sessions = vec![a.clone()];
        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(app.rows(), [Row::Workspace("main".into())]);
        // A worktree and chats turned on, under the folded workspace: still all folded, so `Z`
        // opens it all, those too.
        let mut wt = root("p1");
        wt.id = "p1-two".into();
        wt.is_root = false;
        app.worktrees.push(wt);
        app.config.chat.enabled = !app.config.chat.enabled;
        press(&mut app, 'Z', M::SHIFT);
        let rows = app.rows();
        assert!(rows.contains(&Row::Session(a.id)), "{rows:?}");
        assert!(rows.contains(&Row::Worktree("p1-two".into())), "{rows:?}");
        assert!(app.collapsed.is_empty(), "{:?}", app.collapsed);
        // A new session while it is folded: its workspace opens to show it, selected.
        press(&mut app, 'Z', M::SHIFT);
        let fresh = placed("codex", Some("p1"), Some("p1-root"));
        spawned(&mut app, &fresh, false);
        assert!(app.rows().contains(&Row::Session(fresh.id)), "{:?}", app.rows());
        assert_eq!(app.selected_row(), Some(Row::Session(fresh.id)));
        // Folded by hand, with nothing saved: `Z` opens what shows, and the folds under it stay.
        app.folded_before = None;
        app.collapsed = HashSet::from([crate::tree::fold_key("main"), "p1-two".to_owned()]);
        app.focus = Focus::Sidebar;
        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(app.collapsed, HashSet::from(["p1-two".to_owned()]));
        let toast = app.toast.as_ref().map(|t| t.text.clone()).unwrap_or_default();
        assert_eq!(toast, "sidebar unfolded");
    }

    #[test]
    fn after_z_a_workspace_opens_by_hand_and_a_fold_of_your_own_is_what_z_keeps() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        with_project(&mut app);
        let a = placed("claude", Some("p1"), Some("p1-root"));
        app.sessions = vec![a.clone()];
        app.focus = Focus::Sidebar;
        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(app.selected_row(), Some(Row::Workspace("main".into())));
        // ⏎ on its header opens it, its levels still folded; h folds it again, l opens it.
        app.run(&Command { action: "nav.open", arg: None });
        let open = [
            Row::Workspace("main".into()),
            Row::Project("p1".into()),
            Row::Shells("main".into()),
            Row::Chats("main".into()),
        ];
        assert_eq!(app.rows(), open);
        app.run(&Command { action: "nav.collapse", arg: None });
        assert_eq!(app.rows(), [Row::Workspace("main".into())]);
        app.run(&Command { action: "nav.expand", arg: None });
        assert_eq!(app.rows(), open);
        assert!(app.folded_before.is_none(), "a change of your own: nothing stale kept");
        // Going to the session, then a fold of your own: `Z`, `Z` gives back that fold, not
        // the layout from before the first `Z`.
        app.collapsed.clear();
        press(&mut app, 'Z', M::SHIFT);
        app.show(a.id, None);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let wt = rows.iter().position(|r| *r == Row::Worktree("p1-root".into())).unwrap();
        app.set_sel(&rows, wt);
        app.run(&Command { action: "nav.collapse", arg: None });
        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(app.rows(), [Row::Workspace("main".into())]);
        press(&mut app, 'Z', M::SHIFT);
        let rows = app.rows();
        assert!(rows.contains(&Row::Worktree("p1-root".into())), "{rows:?}");
        assert!(!rows.contains(&Row::Session(a.id)), "the hand fold stays: {rows:?}");
        // A click on a folded workspace's header opens it, as it does a project's.
        use crossterm::event::{MouseButton, MouseEventKind as K};
        press(&mut app, 'Z', M::SHIFT);
        let side = app.sidebar_area().unwrap();
        app.on_mouse(&mouse(K::Down(MouseButton::Left), side.x + 2, side.y + 2));
        assert!(app.rows().contains(&Row::Project("p1".into())), "{:?}", app.rows());
    }

    #[test]
    fn going_to_a_session_while_folded_still_lets_z_put_back_what_was_open() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        with_project(&mut app);
        app.projects.push(project("p2", "other"));
        app.worktrees.push(root("p2"));
        app.workspaces.push(
            serde_json::from_value(serde_json::json!({
                "id": "w2", "name": "side", "profile": "default", "projects": ["p2"],
                "notify": "all", "created_ms": 0,
            }))
            .unwrap(),
        );
        let a = placed("claude", Some("p1"), Some("p1-root"));
        let b = placed("codex", Some("p2"), Some("p2-root"));
        app.sessions = vec![a.clone(), b.clone()];
        app.focus = Focus::Sidebar;
        press(&mut app, 'Z', M::SHIFT);
        // Going to a session (`C-q n`, the palette, `a`) opens its workspace to show it.
        app.show(a.id, None);
        assert!(app.rows().contains(&Row::Session(a.id)));
        app.focus = Focus::Sidebar;
        press(&mut app, 'Z', M::SHIFT);
        assert_eq!(app.rows().len(), 2, "folded again: {:?}", app.rows());
        press(&mut app, 'Z', M::SHIFT);
        let rows = app.rows();
        assert!(
            rows.contains(&Row::Session(a.id)) && rows.contains(&Row::Session(b.id)),
            "{rows:?}"
        );
        assert!(app.collapsed.is_empty() && app.folded_before.is_none());
    }

    #[test]
    fn backtick_goes_back_and_forth_between_a_chat_and_the_session_under_it() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        let (a, b) = with_agents(&mut app);
        app.show(a, None);
        app.chat_list.push(chat_info("cccc"));
        app.open_chat("cccc");
        press(&mut app, 'h', M::NONE);
        press(&mut app, 'i', M::NONE);
        act(&mut app, "session.last");
        assert!(app.chat_panel.is_none(), "the session under it");
        assert_eq!((app.focused_session(), app.focus), (Some(a), Focus::Pane));
        act(&mut app, "session.last");
        assert_eq!(draft(&app).0, "hi", "the chat back, as it was left");
        assert_eq!(app.focus, Focus::Pane);
        // A session shown from the chat (the sidebar, a finder): ` brings the chat back too.
        app.show(b, None);
        assert!(app.chat_panel.is_none() && app.chat_aside.is_some());
        act(&mut app, "session.last");
        assert_eq!(draft(&app).0, "hi");
        // One shown from the panes ends the way back: ` is the previous session again.
        act(&mut app, "session.last");
        app.show(a, None);
        assert!(app.chat_aside.is_none());
        act(&mut app, "session.last");
        assert!(app.chat_panel.is_none());
        assert_eq!(app.focused_session(), Some(b));
    }

    #[test]
    fn a_slash_that_isnt_a_command_is_sent_or_kept_never_lost() {
        let mut app = app();
        let enter = |app: &mut App, text: &str| {
            app.new_chat();
            app.chat_paste(text);
            app.chat_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        };
        let sent = |app: &App| match &app.chat_panel {
            Some(crate::chats::ChatPanel::View(v)) => {
                v.view.items.iter().find_map(|i| match &i.body {
                    ptyro_core::chat::ChatBody::User { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        };
        // A path is a message; `//` sends one `/`.
        enter(&mut app, "/etc/hosts: what is it?");
        assert_eq!(sent(&app).as_deref(), Some("/etc/hosts: what is it?"));
        enter(&mut app, "//retry is a word");
        assert_eq!(sent(&app).as_deref(), Some("/retry is a word"));
        // A mistyped command stays in the composer, with how to send it.
        enter(&mut app, "/agnet codex");
        assert_eq!(sent(&app), None);
        assert_eq!(draft(&app).0, "/agnet codex");
        assert!(toast_text(&app).contains("//"), "{}", toast_text(&app));
    }

    #[test]
    fn an_at_in_the_composer_offers_the_chats_folders() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        let mut c = chat_info("cccc");
        c.folders = vec!["~/notes".into(), "/srv/docs".into()];
        app.chat_list.push(c);
        app.open_chat("cccc");
        for ch in "look in @".chars() {
            press(&mut app, ch, M::NONE);
        }
        let suggest = |app: &App| match &app.chat_panel {
            Some(crate::chats::ChatPanel::View(v)) => v.view.suggest.clone().unwrap_or_default(),
            _ => String::new(),
        };
        assert!(suggest(&app).contains("[~/notes]  /srv/docs"), "{}", suggest(&app));
        app.chat_key(KeyEvent::new(KeyCode::Down, Mods::NONE));
        assert!(suggest(&app).contains("[/srv/docs]"), "{}", suggest(&app));
        app.chat_key(KeyEvent::new(KeyCode::Tab, Mods::NONE));
        assert_eq!(draft(&app).0, "look in /srv/docs ");
        assert_eq!(suggest(&app), "", "done: no more suggestion");
        // Narrowed by what follows the `@`; none matching says so; ⏎ puts one in, not sends.
        for ch in "and @no".chars() {
            press(&mut app, ch, M::NONE);
        }
        assert!(suggest(&app).starts_with("[~/notes]"), "{}", suggest(&app));
        app.chat_key(KeyEvent::new(KeyCode::Enter, Mods::NONE));
        assert_eq!(draft(&app).0, "look in /srv/docs and ~/notes ");
        press(&mut app, '@', M::NONE);
        press(&mut app, 'z', M::NONE);
        assert!(suggest(&app).contains("no folder"), "{}", suggest(&app));
    }

    #[test]
    fn the_chat_composer_edits_like_a_shell_line() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        app.new_chat();
        for c in "world".chars() {
            press(&mut app, c, M::NONE);
        }
        press(&mut app, 'a', M::CONTROL);
        assert_eq!(draft(&app), ("world".into(), 0), "C-a: the start");
        for c in "hello ".chars() {
            press(&mut app, c, M::NONE);
        }
        assert_eq!(draft(&app), ("hello world".into(), 6), "typed at the cursor");
        press(&mut app, 'k', M::CONTROL);
        assert_eq!(draft(&app), ("hello ".into(), 6), "C-k: to the end");
        press(&mut app, 'w', M::CONTROL);
        assert_eq!(draft(&app), ("".into(), 0), "C-w: the word before");
        for c in "one two".chars() {
            press(&mut app, c, M::NONE);
        }
        app.chat_key(KeyEvent::new(KeyCode::Left, Mods::NONE));
        app.chat_key(KeyEvent::new(KeyCode::Left, Mods::NONE));
        app.chat_key(KeyEvent::new(KeyCode::Backspace, Mods::NONE));
        assert_eq!(draft(&app), ("one wo".into(), 4), "←, then ⌫ before the cursor");
        press(&mut app, 'e', M::CONTROL);
        // A second line: the keys keep to the cursor's line.
        app.chat_key(KeyEvent::new(KeyCode::Char { c: 'j' }, Mods(Mods::CTRL)));
        for c in "three".chars() {
            press(&mut app, c, M::NONE);
        }
        press(&mut app, 'a', M::CONTROL);
        assert_eq!(draft(&app), ("one wo\nthree".into(), 7), "C-a: this line's start");
        app.chat_key(KeyEvent::new(KeyCode::End, Mods::NONE));
        press(&mut app, 'u', M::CONTROL);
        assert_eq!(draft(&app), ("one wo\n".into(), 7), "C-u: this line only");
        // The cursor is drawn where it is: on the first line, after `one`.
        app.chat_key(KeyEvent::new(KeyCode::Backspace, Mods::NONE));
        press(&mut app, 'a', M::CONTROL);
        for _ in 0..3 {
            app.chat_key(KeyEvent::new(KeyCode::Right, Mods::NONE));
        }
        let Some(crate::chats::ChatPanel::View(v)) = &app.chat_panel else { panic!() };
        let area = ratatui::layout::Rect::new(0, 0, 40, 10);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let cur = crate::chat::draw(&v.view, &mut buf, area, &app.theme, app.depth, true);
        assert_eq!(cur, Some(ratatui::layout::Position::new(5, 9)), "`› one` then the cursor");
        // A paste goes in at the cursor.
        app.chat_paste("!");
        assert_eq!(draft(&app), ("one! wo".into(), 4));
    }

    #[test]
    fn the_chat_lists_search_operators_arent_title_text() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa"), chat_info("cccc")];
        app.open_chat_list(None);
        let count = |app: &App| match &app.chat_panel {
            Some(crate::chats::ChatPanel::List(l)) => app.list_rows(l).len(),
            _ => 0,
        };
        assert_eq!(count(&app), 2);
        if let Some(crate::chats::ChatPanel::List(l)) = &mut app.chat_panel {
            l.query = "by:meaning chat aaaa".into();
        }
        assert_eq!(count(&app), 1);
    }

    /// What the TUI asked the daemon since the last call (not connected: each comes back).
    fn asked(app: &mut App) -> Vec<Pending> {
        let mut out = Vec::new();
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(p, _) = m {
                out.push(p);
            }
        }
        out
    }

    fn view(app: &App) -> &crate::chats::ChatViewState {
        let Some(crate::chats::ChatPanel::View(v)) = &app.chat_panel else { panic!("no chat") };
        v
    }

    fn ckey(app: &mut App, code: KeyCode) {
        app.chat_key(KeyEvent::new(code, Mods::NONE));
    }

    fn user_item(id: &str, text: &str) -> ptyro_core::chat::ChatItem {
        ptyro_core::chat::ChatItem::new(id, ptyro_core::chat::ChatBody::User { text: text.into() })
    }

    #[test]
    fn a_new_chat_made_late_finds_its_own_composer_not_the_next_one() {
        let mut app = app();
        app.new_chat();
        app.chat_paste("hello");
        let first = chat_token(&app);
        ckey(&mut app, KeyCode::Enter);
        // Esc at once, and another new chat with its own choices.
        ckey(&mut app, KeyCode::Esc);
        assert!(app.chat_panel.is_none());
        app.new_chat();
        app.chat_picked("codex".into(), None);
        app.overlay = None;
        app.chat_paste("second");
        app.handle_reply(
            Pending::ChatMade { text: "hello".into(), token: first },
            Ok(Reply::Chat { info: chat_info("aaaa") }),
        );
        let v = view(&app);
        assert!(v.id.is_none() && v.agent.as_deref() == Some("codex") && v.draft == "second");
        assert!(toast_text(&app).contains("open it"), "{}", toast_text(&app));
        // A refusal of the first is no business of this composer either.
        app.chat_not_made(first, "hello".into(), "refused".into());
        assert_eq!(view(&app).draft, "second");
        // Its own reply makes it that chat, with what was typed meanwhile kept.
        let mine = chat_token(&app);
        app.handle_reply(
            Pending::ChatMade { text: "second".into(), token: mine },
            Ok(Reply::Chat { info: chat_info("bbbb") }),
        );
        assert_eq!(view(&app).id.as_deref(), Some("bbbb"));
        assert_eq!(view(&app).draft, "second", "the draft stays");
    }

    #[test]
    fn a_slow_reply_is_offered_and_each_chat_keeps_its_draft() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa"), chat_info("bbbb")];
        app.open_chat("aaaa");
        let origin = app.chat_shown_key();
        app.open_chat("bbbb");
        app.chat_paste("half a thought");
        // A fork asked for in A arrives while B is being typed in: B stays.
        app.handle_reply(Pending::ChatOpen(origin), Ok(Reply::Chat { info: chat_info("cccc") }));
        assert_eq!(shown_chat(&app).as_deref(), Some("bbbb"));
        assert_eq!(view(&app).draft, "half a thought");
        assert!(toast_text(&app).contains("ready"), "{}", toast_text(&app));
        act(&mut app, "toast.act");
        assert_eq!(shown_chat(&app).as_deref(), Some("cccc"), "the offer opens it");
        // B's draft waited for it.
        app.open_chat("bbbb");
        assert_eq!(view(&app).draft, "half a thought");
        // Asked from what is still shown: it opens at once.
        let origin = app.chat_shown_key();
        app.handle_reply(Pending::ChatOpen(origin), Ok(Reply::Chat { info: chat_info("dddd") }));
        assert_eq!(shown_chat(&app).as_deref(), Some("dddd"));
    }

    #[test]
    fn an_archived_chat_keeps_its_header_and_only_its_own_view_closes() {
        let mut app = app();
        with_project(&mut app);
        let mut a = chat_info("aaaa");
        a.cost_micro_usd = 12_000;
        app.chat_list = vec![a.clone(), chat_info("bbbb")];
        app.open_chat("aaaa");
        // Archived elsewhere (the CLI): it leaves the list but the view still knows it.
        let mut archived = a.clone();
        archived.archived = true;
        app.chat_updated(archived.clone());
        let shown = screen(&app);
        assert!(shown.contains("chat aaaa") && shown.contains("$0.012"), "{shown}");
        assert!(!shown.contains("new chat"), "{shown}");
        // `x` on another chat's row archives that one; this view stays.
        app.chat_updated(a);
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Chat("bbbb".into())).unwrap();
        app.set_sel(&rows, at);
        asked(&mut app);
        press(&mut app, 'x', crossterm::event::KeyModifiers::NONE);
        assert_eq!(shown_chat(&app).as_deref(), Some("aaaa"));
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::Done(m) if m == "archived")));
        // An archived chat's `x` brings it back, whatever list is shown.
        app.chat_updated(archived);
        app.focus = Focus::Pane;
        act(&mut app, "chat.archive");
        assert!(
            asked(&mut app)
                .iter()
                .any(|p| matches!(p, Pending::Done(m) if m == "back from the archive"))
        );
        assert!(app.chat_panel.is_none(), "its own view goes");
    }

    #[test]
    fn edit_on_a_chat_row_opens_it_then_asks_for_its_last_message_again() {
        let mut app = app();
        with_project(&mut app);
        app.chat_list = vec![chat_info("aaaa"), chat_info("bbbb")];
        app.open_chat("aaaa");
        app.focus = Focus::Sidebar;
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Chat("bbbb".into())).unwrap();
        app.set_sel(&rows, at);
        act(&mut app, "chat.edit");
        assert_eq!(shown_chat(&app).as_deref(), Some("bbbb"), "the row's chat, not the one open");
        assert!(app.overlay.is_none(), "its messages aren't here yet");
        app.chat_live("bbbb".into(), 1, true, vec![user_item("n1", "how do lifetimes work")]);
        let Some(Overlay::Prompt { kind: PromptKind::ChatEdit(c, n), text, .. }) = &app.overlay
        else {
            panic!("asks for the new words")
        };
        assert_eq!(
            (c.as_str(), n.as_str(), text.as_str()),
            ("bbbb", "n1", "how do lifetimes work")
        );
        // Find in a chat from the list: that chat opens and the answer is found there.
        app.overlay = None;
        app.open_chat_list(None);
        act(&mut app, "chat.find_in");
        let Some(Overlay::Prompt { kind: PromptKind::ChatFindIn(c), .. }) = &app.overlay else {
            panic!("asks what")
        };
        let c = c.clone();
        app.overlay = None;
        app.find_in_chat(&c, "lifetimes");
        assert!(view(&app).view.highlight.is_some());
    }

    #[test]
    fn the_chat_lists_message_hits_are_selected_scrolled_to_and_clicked() {
        let mut app = app();
        app.chat_list = (0..30).map(|i| chat_info(&format!("c{i:03}"))).collect();
        app.open_chat_list(None);
        let hit: ptyro_core::chats::ChatHit = serde_json::from_value(serde_json::json!({
            "chat": chat_info("c007"), "node": "n9", "kind": "user", "snippet": "the needle",
            "how": "words", "rank": 1, "at_ms": 0,
        }))
        .unwrap();
        if let Some(crate::chats::ChatPanel::List(l)) = &mut app.chat_panel {
            l.hits = vec![hit];
            l.sel = 30;
        }
        let shown = screen(&app);
        assert!(shown.contains("the needle"), "the hit is scrolled to: {shown}");
        let y = shown.lines().position(|l| l.contains("the needle")).unwrap() as u16;
        let area = app.main_area();
        assert_eq!(crate::chats::list_row_at(&app, area, y), Some(30));
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 4,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.on_mouse(&click);
        assert_eq!(shown_chat(&app).as_deref(), Some("c007"));
        assert_eq!(view(&app).focus.as_deref(), Some("n9"), "at that message");
    }

    #[test]
    fn a_machine_going_offline_is_said_once_and_coming_back_follows_again() {
        use ptyro_core::model::MachineState as S;
        let mut app = app();
        with_laptop2(&mut app, S::Online);
        let mut c = chat_info("rrrr");
        c.machine = Some("laptop2".into());
        c.workspace = "laptop2:work".into();
        app.chat_list = vec![c];
        app.open_chat("rrrr");
        let notices = |app: &App| view(app).view.items.len();
        for state in [S::Offline, S::Connecting, S::Offline, S::Connecting, S::Offline] {
            app.on_event(DomainEvent::MachineUpdated { info: machine("laptop2", state) });
        }
        assert_eq!(notices(&app), 1, "one notice for one outage");
        app.on_event(DomainEvent::MachineUpdated { info: machine("laptop2", S::Online) });
        app.on_event(DomainEvent::MachineUpdated { info: machine("laptop2", S::Offline) });
        assert_eq!(notices(&app), 2, "a new outage is news");
        // Its row in the list says since when.
        app.open_chat_list(None);
        assert!(screen(&app).contains("offline since"), "{}", screen(&app));
        app.open_chat("rrrr");
        // Hidden with its workspace there: said so, not "deleted".
        let mut m = machine("laptop2", S::Online);
        m.hidden = vec!["work".into()];
        app.on_event(DomainEvent::MachineUpdated { info: m });
        app.on_event(DomainEvent::ChatRemoved {
            id: "rrrr".into(),
            machine: Some("laptop2".into()),
        });
        assert!(shown_chat(&app).is_none(), "back to the list it was opened from");
        assert!(toast_text(&app).contains("hidden"), "{}", toast_text(&app));
    }

    #[test]
    fn session_keys_over_the_chat_panel_never_reach_the_hidden_session() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        let (a, _) = with_agents(&mut app);
        app.chat_list = vec![chat_info("aaaa")];
        app.open_chat_list(None);
        press(&mut app, 'q', M::CONTROL);
        press(&mut app, 'X', M::SHIFT);
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("asks") };
        assert!(text.contains("chat aaaa"), "the list's chat: {text}");
        app.overlay = None;
        app.new_chat();
        press(&mut app, 'q', M::CONTROL);
        press(&mut app, 'x', M::NONE);
        assert!(app.overlay.is_none(), "no stop asked for");
        assert!(app.session(a).is_some());
    }

    #[test]
    fn ctrl_c_stops_a_working_answer_whatever_the_draft_and_card_letters_are_typed() {
        use crossterm::event::KeyModifiers as M;
        let mut app = app();
        let mut c = chat_info("aaaa");
        c.status = Some(ptyro_core::status::AgentStatus::Working);
        app.chat_list = vec![c];
        app.open_chat("aaaa");
        app.chat_paste("next question");
        asked(&mut app);
        press(&mut app, 'c', M::CONTROL);
        assert_eq!(view(&app).draft, "next question", "kept");
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::Done(m) if m == "stopped")));
        // Idle: C-c clears the draft.
        app.chat_list[0].status = Some(ptyro_core::status::AgentStatus::Idle);
        press(&mut app, 'c', M::CONTROL);
        assert_eq!(view(&app).draft, "");
        // A card waiting: `y` is a letter, `a` answers.
        let card = ptyro_core::chat::ChatItem::new(
            "p",
            ptyro_core::chat::ChatBody::Permission {
                request: "r".into(),
                tool: "Bash".into(),
                title: "ls".into(),
                options: ptyro_core::chat::PermOption::standard(),
                decided: None,
            },
        );
        app.chat_live("aaaa".into(), 1, true, vec![card]);
        press(&mut app, 'y', M::NONE);
        assert_eq!(view(&app).draft, "y", "typing \"yes\" isn't an answer");
        ckey(&mut app, KeyCode::Esc);
        asked(&mut app);
        press(&mut app, 'a', M::NONE);
        assert_eq!(view(&app).draft, "");
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::Done(m) if m.is_empty())));
    }

    #[test]
    fn esc_goes_back_where_the_chat_was_opened_from() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa")];
        // A chat opened from the list: back to the list.
        app.open_chat_list(Some("main".into()));
        app.open_chat("aaaa");
        ckey(&mut app, KeyCode::Esc);
        assert!(matches!(app.chat_panel, Some(crate::chats::ChatPanel::List(_))));
        // A new chat started from the list: back to the list.
        app.open_chat_list(Some("main".into()));
        press(&mut app, 'm', crossterm::event::KeyModifiers::NONE);
        assert!(view(&app).id.is_none());
        ckey(&mut app, KeyCode::Esc);
        assert!(matches!(app.chat_panel, Some(crate::chats::ChatPanel::List(_))));
    }

    #[test]
    fn a_new_chat_takes_its_agent_title_and_a_folder_before_its_first_message() {
        let mut app = app();
        app.new_chat();
        for line in ["/agent codex", "/title release notes"] {
            app.chat_paste(line);
            ckey(&mut app, KeyCode::Enter);
        }
        assert_eq!(view(&app).agent.as_deref(), Some("codex"));
        assert_eq!(view(&app).title.as_deref(), Some("release notes"));
        // C-o: the agent, then a folder.
        press(&mut app, 'o', crossterm::event::KeyModifiers::CONTROL);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Picker { purpose: Purpose::ChatAgent(None), .. })
        ));
        app.overlay = None;
        app.chat_picked("claude".into(), None);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Prompt { kind: PromptKind::ChatFolder(None), .. })
        ));
        app.overlay = None;
        // Its first message: ⏎ waits for it.
        app.chat_paste("hi");
        ckey(&mut app, KeyCode::Enter);
        assert!(view(&app).sending);
    }

    #[test]
    fn the_chat_lists_paste_searches_and_its_filters_and_tab_keep_its_workspace() {
        let mut app = app();
        let mut pinned = chat_info("pppp");
        pinned.pinned = true;
        app.chat_list = vec![pinned, chat_info("aaaa")];
        app.open_chat_list(Some("main".into()));
        asked(&mut app);
        app.chat_paste("redis");
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::ChatHits(q) if q == "redis")));
        ckey(&mut app, KeyCode::Esc);
        let rows = |app: &App| match &app.chat_panel {
            Some(crate::chats::ChatPanel::List(l)) => app.list_rows(l).len(),
            _ => 0,
        };
        assert_eq!(rows(&app), 2);
        // `s`: needs you (none), working (none), unread (none), pinned (one).
        for _ in 0..4 {
            press(&mut app, 's', crossterm::event::KeyModifiers::NONE);
        }
        assert_eq!(rows(&app), 1);
        assert!(screen(&app).contains("pinned only"));
        // Tab: every workspace, then back to the one it was opened for.
        app.workspace = "elsewhere".into();
        let ws = |app: &App| match &app.chat_panel {
            Some(crate::chats::ChatPanel::List(l)) => l.workspace.clone(),
            _ => None,
        };
        ckey(&mut app, KeyCode::Tab);
        assert_eq!(ws(&app), None);
        ckey(&mut app, KeyCode::Tab);
        assert_eq!(ws(&app).as_deref(), Some("main"));
        // The selected chat's preview, under it.
        app.got_preview(
            "pppp",
            vec![
                serde_json::from_value(serde_json::json!({
                    "id": "n1", "at_ms": 0, "t": "user", "text": "what about redis"
                }))
                .unwrap(),
            ],
        );
        assert!(screen(&app).contains("› what about redis"), "{}", screen(&app));
        // ⏎ while typing opens what is selected.
        press(&mut app, '/', crossterm::event::KeyModifiers::NONE);
        ckey(&mut app, KeyCode::Enter);
        assert_eq!(shown_chat(&app).as_deref(), Some("pppp"));
    }

    #[test]
    fn the_chat_keys_are_rebindable_and_the_hints_follow() {
        let c = Config::parse(
            "[keys]\n\"chat:C-y\" = \"chat.find_in\"\n\"chat:C-f\" = \"none\"\n\"chatlist:p\" = \"chat.pin\"\n",
        )
        .unwrap();
        let (map, errors) = Keymap::new(&c);
        assert!(errors.is_empty(), "{errors:?}");
        let mut app = app();
        app.keymap = map;
        app.chat_list = vec![chat_info("aaaa")];
        app.open_chat("aaaa");
        assert!(screen(&app).contains("C-y find"), "{}", screen(&app));
        press(&mut app, 'y', crossterm::event::KeyModifiers::CONTROL);
        assert!(matches!(
            app.overlay,
            Some(Overlay::Prompt { kind: PromptKind::ChatFindIn(_), .. })
        ));
        app.overlay = None;
        app.open_chat_list(None);
        asked(&mut app);
        press(&mut app, 'p', crossterm::event::KeyModifiers::NONE);
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::Done(m) if m == "pinned")));
    }

    #[test]
    fn menus_offer_what_applies_and_toasts_offer_the_fix() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa")];
        let a = app.chat_actions_for("aaaa");
        for x in ["chat.retry", "chat.edit", "chat.cite", "chat.find_in", "chat.task"] {
            assert!(a.contains(&x), "{x}");
        }
        assert!(!a.contains(&"chat.stop"), "nothing to stop");
        // A new chat's menu: its own choices.
        app.new_chat();
        act(&mut app, "row.menu");
        let Some(Overlay::Picker { palette, .. }) = &app.overlay else { panic!("a menu") };
        assert_eq!(palette.item(0).label, "chat.agent");
        app.overlay = None;
        app.close_chat();
        // Nothing selected: the list is offered; chats off: switching them on is.
        act(&mut app, "chat.rename");
        assert!(matches!(
            app.toast.as_ref().and_then(|t| t.offer.as_ref()).map(|o| &o.then),
            Some(Then::Run(Command { action: "chat.list", .. }))
        ));
        app.config.chat.enabled = false;
        app.new_chat();
        assert!(matches!(
            app.toast.as_ref().and_then(|t| t.offer.as_ref()).map(|o| &o.then),
            Some(Then::Set { key, .. }) if key == "chat.enabled"
        ));
    }

    #[test]
    fn the_chats_headings_plus_starts_a_chat_there() {
        let mut app = app();
        with_project(&mut app);
        app.chat_list = vec![chat_info("ffff")];
        let y = sidebar(&app).iter().position(|l| l.contains("CHATS")).unwrap() as u16;
        let side = app.sidebar_area().unwrap();
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: side.x + crate::chats::HEADING_PLUS + 2,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        app.on_mouse(&click);
        assert!(view(&app).id.is_none(), "a new chat");
        assert_eq!(view(&app).workspace, "main");
        assert!(!app.collapsed.contains(&crate::tree::chats_key("main")), "not folded");
        // Empty, incognito: says nothing is kept.
        act(&mut app, "chat.incognito");
        assert!(screen(&app).contains("Incognito: nothing of it is kept"), "{}", screen(&app));
    }

    #[test]
    fn a_refused_message_goes_back_in_front_of_what_was_typed_since() {
        let mut app = app();
        app.chat_list = vec![chat_info("aaaa")];
        app.open_chat("aaaa");
        app.chat_paste("first");
        ckey(&mut app, KeyCode::Enter);
        app.chat_paste("more");
        app.chat_sent("aaaa", "first".into(), Some("refused".into()));
        assert_eq!(view(&app).draft, "first\nmore");
        // Refused while another chat is shown: kept for when it opens again.
        app.close_chat();
        app.chat_sent("aaaa", "again".into(), Some("refused".into()));
        app.open_chat("aaaa");
        assert_eq!(view(&app).draft, "again\nfirst\nmore");
    }

    #[test]
    fn chats_switched_off_or_on_in_the_file_take_effect_at_once() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| h.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        let mut app = App::new(
            Options::default(),
            connect,
            paths.clone(),
            Config::default(),
            false,
            Rect::new(0, 0, 100, 30),
        );
        let file = Config::path(&paths);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        app.chat_list = vec![chat_info("aaaa")];
        app.open_chat("aaaa");
        std::fs::write(&file, "[chat]\nenabled = false\n").unwrap();
        app.reload_config_with(false);
        assert!(app.chat_panel.is_none(), "off: the open chat goes");
        asked(&mut app);
        std::fs::write(&file, "[chat]\nenabled = true\n").unwrap();
        app.reload_config_with(false);
        assert!(asked(&mut app).iter().any(|p| matches!(p, Pending::Chats)), "on: fetched");
    }

    // ── SHELLS (SPEC §23.6, ROADMAP SH.2) ──────────────────────────────────────

    fn loose(name: &str, slot: u32) -> SessionInfo {
        let mut s = session(name);
        s.loose = true;
        s.slot = Some(slot);
        s.workspace = Some("main".into());
        s.argv = vec!["/bin/zsh".into()];
        s
    }

    /// The spawn the TUI asked for, if any (no daemon here: it comes back unanswered).
    fn spawn_asked(app: &mut App) -> Option<SpawnSpec> {
        let mut found = None;
        while let Ok(m) = app.rx.try_recv() {
            if let AppMsg::Reply(Pending::Spawned { spec, .. }, _) = m {
                found = Some(*spec);
            }
        }
        found
    }

    #[test]
    fn shells_are_a_section_by_number_and_keep_out_of_the_sidebar_s_numbers() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        let (two, one) = (loose("htop", 2), loose("dl", 1));
        app.sessions.extend([two.clone(), one.clone()]);
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Shells("main".into())).expect("SHELLS");
        assert_eq!(rows[at + 1], Row::Session(one.id), "by number, not by age");
        assert_eq!(rows[at + 2], Row::Session(two.id));
        assert!(
            rows.iter().position(|r| *r == Row::Chats("main".into())).is_none_or(|c| c > at),
            "before CHATS"
        );
        // `C-q 1…9` count the other sessions; a shell's number is its own.
        let numbered = app.numbered();
        assert_eq!(numbered.first(), Some(&agent));
        assert!(!numbered.contains(&one.id) && !numbered.contains(&two.id));
        let shown = screen(&app);
        assert!(shown.contains("SHELLS"), "{shown}");
        assert!(shown.contains(" 1 "), "{shown}");
        // `when-used` hides an empty section; `always` shows ＋ new shell in one.
        app.sessions.retain(|s| !s.loose);
        assert!(app.rows().contains(&Row::ShellNew("main".into())));
        app.config.shell.section = ptyro_core::config::ShellsSection::WhenUsed;
        assert!(!app.rows().iter().any(|r| matches!(r, Row::Shells(_) | Row::ShellNew(_))));
    }

    #[test]
    fn alt_n_shows_a_shell_and_an_empty_number_starts_one_there() {
        let mut app = app();
        with_agents(&mut app);
        let two = loose("htop", 2);
        app.sessions.push(two.clone());
        while app.rx.try_recv().is_ok() {}
        app.run(&Command { action: "shells.go", arg: Some("2".into()) });
        assert_eq!(app.focused_session(), Some(two.id));
        app.run(&Command { action: "shells.go", arg: Some("5".into()) });
        let spec = spawn_asked(&mut app).expect("a new shell was asked for");
        assert!(spec.loose && spec.slot == Some(5), "{spec:?}");
        assert_eq!(spec.workspace.as_deref(), Some("main"));
        // It starts in the pane's directory (the shown shell's).
        assert_eq!(spec.cwd, two.cwd);
        // The keys: Alt-1…9 by default, and off with `[shell] alt_digits = false`.
        let alt3 = KeyEvent::new(KeyCode::Char { c: '3' }, Mods(Mods::ALT));
        let c = app.keymap.lookup(Table::Global, alt3).expect("M-3");
        assert_eq!((c.action, c.number()), ("shells.go", Some(3)));
        // Global means anywhere: a chat that has the keyboard doesn't swallow them, and the
        // chat waits aside for `session.last`.
        app.run(&Command { action: "chat.new", arg: None });
        assert!(app.chat_panel.is_some() && app.focus == Focus::Pane);
        press(&mut app, '2', crossterm::event::KeyModifiers::ALT);
        assert!(app.chat_panel.is_none(), "the chat stepped aside");
        assert_eq!(app.focused_session(), Some(two.id));
        assert!(app.chat_aside.is_some(), "`session.last` brings it back");
        let mut config = Config::default();
        config.shell.alt_digits = false;
        let (off, _) = crate::keymap::Keymap::new(&config);
        assert!(off.lookup(Table::Global, alt3).is_none());
    }

    /// Shell 3 as a window of two panes (SPEC §23.17): `a` left, `b` right (30/70), `b` focused.
    fn window_of_two(app: &mut App) -> (SessionInfo, SessionInfo) {
        use ptyro_core::window::{PaneTree, SplitDir, WindowLayout};
        let layout = WindowLayout {
            root: PaneTree::Split {
                dir: SplitDir::Right,
                share: 300,
                a: Box::new(PaneTree::Pane { pane: 1 }),
                b: Box::new(PaneTree::Pane { pane: 2 }),
            },
            focus: 2,
            zoom: false,
        };
        let (mut a, mut b) = (loose("left", 3), loose("right", 3));
        (a.pane, b.pane) = (Some(1), Some(2));
        (a.layout, b.layout) = (Some(layout.clone()), Some(layout));
        app.sessions.extend([a.clone(), b.clone()]);
        (a, b)
    }

    #[test]
    fn a_split_of_a_shell_in_shells_asks_the_daemon_for_a_pane_of_its_window() {
        let mut app = app();
        with_agents(&mut app);
        let one = loose("dl", 1);
        app.sessions.push(one.clone());
        app.show(one.id, None);
        while app.rx.try_recv().is_ok() {}
        app.run(&Command { action: "pane.split_right", arg: None });
        key(&mut app, KeyCode::Enter, Mods::NONE);
        // `ShellSplit`: the daemon starts the copy where the shell is, and places it in the
        // window; no spawn in a directory that a worktree could claim (the old bug).
        let spec = spawn_asked(&mut app).expect("a pane was asked for");
        assert_eq!(spec, SpawnSpec::default(), "not a plain spawn: {spec:?}");
        assert_eq!(app.layout.panes().len(), 1, "the window's screen comes from the daemon");
    }

    #[test]
    fn showing_a_window_shows_its_whole_layout_and_back_is_the_session_before() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        let (a, b) = window_of_two(&mut app);
        app.show(agent, None);
        app.run(&Command { action: "shells.go", arg: Some("3".into()) });
        assert_eq!(app.focused_session(), Some(b.id), "the pane that had focus there");
        assert_eq!(app.layout.panes().len(), 2);
        let shown: Vec<_> = app.panes.values().filter_map(|p| p.session).collect();
        assert!(shown.contains(&a.id) && shown.contains(&b.id));
        let rects = app.pane_rects();
        let left = rects.iter().find(|(p, _)| app.panes[p].session == Some(a.id)).unwrap().1;
        let right = rects.iter().find(|(p, _)| app.panes[p].session == Some(b.id)).unwrap().1;
        assert!(left.width < right.width, "30/70: {left:?} {right:?}");
        // The agent's screen is kept: ` brings it back.
        app.run(&Command { action: "session.last", arg: None });
        assert_eq!(app.focused_session(), Some(agent));
        // A click on the window's first pane shows the window, that pane focused.
        app.show(a.id, None);
        assert_eq!((app.focused_session(), app.layout.panes().len()), (Some(a.id), 2));
        // The sidebar hangs pane 2 under the window's row.
        let rows = app.rows();
        let at = rows.iter().position(|r| *r == Row::Session(a.id)).unwrap();
        assert_eq!(rows[at + 1], Row::Session(b.id));
        let text = screen(&app);
        assert!(text.contains("└.2") && text.contains("⊞2"), "{text}");
    }

    #[test]
    fn a_window_s_arrangement_goes_to_the_daemon_and_comes_back_from_it() {
        let mut app = app();
        with_agents(&mut app);
        let (a, b) = window_of_two(&mut app);
        app.show(b.id, None);
        app.sync_window();
        let agreed = app.window_seen.clone().and_then(|(_, l)| l).expect("seen");
        // Focus moves here: sent (and taken as agreed until the daemon says otherwise).
        app.focused =
            app.panes.iter().find(|(_, p)| p.session == Some(a.id)).map(|(k, _)| *k).unwrap();
        while app.rx.try_recv().is_ok() {}
        app.sync_window();
        let sent = app.window_seen.clone().and_then(|(_, l)| l).expect("sent");
        assert_eq!(sent.focus, 1);
        assert_ne!(sent, agreed);
        assert!(!asked(&mut app).is_empty(), "a request went out");
        // The daemon echoes it: nothing more to send.
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(sent.clone());
        }
        app.sync_window();
        assert!(asked(&mut app).is_empty());
        // Two changes here before the first comes back: its echo doesn't undo the second.
        let mut l = sent.clone();
        l.zoom = true;
        app.zoom = true;
        app.sync_window();
        app.zoom = false;
        app.sync_window();
        assert!(!app.zoom);
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(l.clone());
        }
        app.sync_window();
        assert!(!app.zoom, "the echo of the first change is old news");
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(sent.clone());
        }
        app.sync_window();
        assert!(app.window_sent.is_empty(), "caught up");
        // Another client zooms the window: shown here.
        let mut zoomed = sent.clone();
        zoomed.zoom = true;
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(zoomed.clone());
        }
        app.sync_window();
        assert!(app.zoom, "another client's change is shown");
    }

    #[test]
    fn news_about_a_window_keeps_the_quick_terminal_s_drawer_and_the_kept_screen() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        app.show(agent, None);
        let (a, _) = window_of_two(&mut app);
        app.show(a.id, None);
        let kept = app.kept.clone();
        assert!(kept.is_some(), "the agent's screen is kept");
        // The quick terminal's drawer under pane 1.
        let host = app.focused;
        let quick = quick("dl.shell", None);
        app.sessions.push(quick.clone());
        let d = app.layout.split_at(host, Dir::Down, 0.7, crate::layout::MAX_WITH_DRAWER).unwrap();
        app.panes.insert(d, PaneState { session: Some(quick.id), ..PaneState::default() });
        app.drawer = Some(crate::quick::Drawer { host, pane: d });
        app.sync_window();
        // News: another client resized the window.
        let mut theirs = a.layout.clone().unwrap();
        if let ptyro_core::window::PaneTree::Split { share, .. } = &mut theirs.root {
            *share = 650;
        }
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(theirs.clone());
        }
        app.sync_window();
        let d = app.drawer.expect("the drawer stays open");
        assert_eq!(app.panes[&d.pane].session, Some(quick.id));
        assert_eq!(app.panes[&d.host].session, Some(a.id), "under the same pane");
        assert_eq!(app.kept, kept, "the kept screen is still the agent's");
    }

    #[test]
    fn another_client_s_focus_is_followed_not_fought() {
        let mut app = app();
        with_agents(&mut app);
        let (a, b) = window_of_two(&mut app);
        app.show(a.id, None);
        app.sync_window();
        // The daemon's layout says pane 2 has focus (another TUI, or a split from the CLI):
        // this one follows, and so has nothing to send back.
        let mut theirs = a.layout.clone().unwrap();
        // Something else changed too, so it is news: the shares.
        if let ptyro_core::window::PaneTree::Split { share, .. } = &mut theirs.root {
            *share = 600;
        }
        theirs.focus = 2;
        for s in app.sessions.iter_mut().filter(|s| s.slot == Some(3)) {
            s.layout = Some(theirs.clone());
        }
        while app.rx.try_recv().is_ok() {}
        app.sync_window();
        assert_eq!(app.focused_session(), Some(b.id));
        app.sync_window();
        assert!(asked(&mut app).is_empty(), "no layout sent back");
    }

    #[test]
    fn bang_with_no_worktree_around_starts_a_shell_in_shells() {
        let mut app = app();
        let (agent, _) = with_agents(&mut app);
        // In a worktree's agent, `!` is a shell in that worktree.
        app.show(agent, None);
        while app.rx.try_recv().is_ok() {}
        app.run(&Command { action: "shell.new", arg: None });
        let spec = spawn_asked(&mut app).expect("spawn");
        assert!(!spec.loose && spec.worktree.as_deref() == Some("p1-root"), "{spec:?}");
        // In a shell of SHELLS (no worktree around): another one there, not the first project's.
        let one = loose("dl", 1);
        app.sessions.push(one.clone());
        app.show(one.id, None);
        app.run(&Command { action: "shell.new", arg: None });
        let spec = spawn_asked(&mut app).expect("spawn");
        assert!(spec.loose && spec.worktree.is_none(), "{spec:?}");
    }

    #[test]
    fn a_shell_s_pane_header_is_the_strip_of_its_workspace_s_shells() {
        let mut app = app();
        with_project(&mut app);
        let (one, two) = (loose("dl", 1), loose("htop", 2));
        app.sessions.extend([one.clone(), two.clone()]);
        app.show(two.id, None);
        let shown = screen(&app);
        let header = shown.lines().find(|l| l.contains("SHELLS ")).unwrap_or_default();
        assert!(header.contains("[2 "), "the shown one in brackets: {shown}");
        assert!(header.contains("1 "), "{header}");
    }

    #[test]
    fn a_click_on_the_strip_shows_that_shell_and_plus_starts_one() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let mut app = app();
        with_project(&mut app);
        let (one, two) = (loose("dl", 1), loose("htop", 2));
        app.sessions.extend([one.clone(), two.clone()]);
        app.show(two.id, None);
        let shown = screen(&app);
        let (y, line) =
            shown.lines().enumerate().find(|(_, l)| l.contains("SHELLS ")).expect("the strip");
        // The column of `1 ` in the strip (after `SHELLS `), counted in cells.
        let at = |needle: &str| {
            let byte = line.find(needle).unwrap();
            line[..byte].chars().count() as u16
        };
        let x1 = at("SHELLS ") + "SHELLS ".len() as u16;
        app.on_mouse(&mouse(K::Down(MouseButton::Left), x1, y as u16));
        assert_eq!(app.focused_session(), Some(one.id), "shell 1 is shown");
        // Its `＋` asks for a new shell in the same workspace.
        let shown = screen(&app);
        let line = shown.lines().nth(y).unwrap();
        let plus = line[..line.find('＋').unwrap()].chars().count() as u16;
        while app.rx.try_recv().is_ok() {}
        app.on_mouse(&mouse(K::Down(MouseButton::Left), plus, y as u16));
        let spec = spawn_asked(&mut app).expect("a new shell");
        assert!(spec.loose && spec.workspace.as_deref() == Some("main"), "{spec:?}");
    }

    #[test]
    fn a_key_with_alt_proves_alt_reaches_and_the_finding_goes() {
        let mut app = app();
        app.health.findings.push(crate::health::Finding {
            problem: false,
            what: crate::shells::ALT_FINDING.into(),
            text: "Option-digit types ¡™£ here".into(),
            fix: None,
        });
        assert!(!app.alt_seen);
        // Alt-b (Option-Left in Terminal.app, with or without Option as Meta) proves nothing.
        app.learn_alt(KeyEvent::new(KeyCode::Char { c: 'b' }, Mods(Mods::ALT)));
        assert!(!app.alt_seen && app.health.findings.len() == 1);
        app.learn_alt(KeyEvent::new(KeyCode::Char { c: '3' }, Mods(Mods::ALT)));
        assert!(app.alt_seen && app.alt_digits_reach());
        assert!(app.health.findings.is_empty(), "it works after all: no finding");
    }

    #[test]
    fn forgetting_a_project_with_running_sessions_offers_to_let_them_go() {
        let mut app = app();
        app.projects = vec![project("p1", "app")];
        app.context_hint = Some("p1".into());
        app.run(&Command { action: "project.remove", arg: None });
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("asks first") };
        assert!(text.contains(ptyro_core::removal::PROJECT), "says what goes: {text}");
        app.overlay_key(KeyEvent::char('y'));
        let Ok(AppMsg::Reply(Pending::Forget(id), _)) = app.rx.try_recv() else {
            panic!("the project was asked to go")
        };
        // The daemon refuses: sessions run in it. Its way through is the same button as every
        // refusal's (SPEC §6.6), the web's and the CLI's offer too.
        let e = ptyro_core::Error::conflict("app has running sessions (a).")
            .with_fix(ptyro_core::error::Again::Release, "Let them go");
        let req = Request::ProjectRemove { addr: id.clone(), release: false };
        let retry = req.with_fix(&ptyro_core::error::Again::Release).expect("it can say that");
        app.on_reply(AppMsg::Refused(Pending::Forget(id), e, Box::new(retry)));
        let t = app.toast.as_ref().unwrap();
        let Some(Offer { label, then: Then::Send(req, _) }) = &t.offer else { panic!("no button") };
        assert_eq!(label, "let them go");
        assert!(matches!(&**req, Request::ProjectRemove { addr, release: true } if addr == "p1"));
    }

    #[test]
    fn removing_a_workspace_says_what_the_daemon_does() {
        let mut app = app();
        app.workspace = "side".into();
        app.run(&Command { action: "workspace.remove", arg: None });
        let Some(Overlay::Confirm { text, .. }) = &app.overlay else { panic!("asks first") };
        assert!(text.contains(ptyro_core::removal::WORKSPACE), "{text}");
    }
}
