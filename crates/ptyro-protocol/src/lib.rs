//! The client/daemon protocol (SPEC §6): message types, framing and encoding.
//!
//! Frames are `u32` big-endian length + payload (max 16 MiB). Payloads are MessagePack with named
//! fields, or JSON for debugging. Each frame identifies its own encoding: every message is a map,
//! so a JSON payload starts with `{` while a MessagePack map never does. The daemon answers each
//! client in the encoding of its `Hello`.

use std::path::PathBuf;

use ptyro_core::input::InputEvent;
use ptyro_core::model::{
    CommitInfo, DiffAgainst, DiffDoc, GcEntry, PrInfo, ProfileInfo, ProfileReason, ProjectInfo,
    RemoveOpts, ReviewComment, ReviewItem, ScriptsInfo, WorkspaceInfo, WorktreeInfo, WorktreeSpec,
};
use ptyro_core::scratchpad::{Item, Snapshot};
use ptyro_core::screen::{ScreenDelta, ScrollbackDoc};
use ptyro_core::session::{HistoryEntry, SessionInfo, SpawnSpec};
use ptyro_core::status::{AttentionKind, InboxEntry, Usage};
use ptyro_core::{ClientId, Error, SessionId, TermSize};
use ptyro_run::wire::{
    Checkpoint, CompareDoc, FanoutSpec, GateInfo, LeadOp, Lease, Message, PickMode, Radar,
    RecipeInfo, RunInfo,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// This build's protocol version. A different `major` is rejected (SPEC §6.3).
pub const PROTO: Version = Version { major: 1, minor: 18 };

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Tui,
    Cli,
    Hook,
    Web,
    Mobile,
    Proxy,
    Test,
}

/// What the client knows about the terminal it runs in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OuterTerm {
    /// Default colours, queried with OSC 10/11 at startup (SPEC §5.6).
    #[serde(default)]
    pub fg: Option<[u8; 3]>,
    #[serde(default)]
    pub bg: Option<[u8; 3]>,
    #[serde(default)]
    pub kitty_keyboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub version: String,
    pub pid: u32,
    #[serde(default)]
    pub outer: Option<OuterTerm>,
    /// A paired device's token (M9.2). Required for `web` and `mobile` clients, which the
    /// daemon then holds to the device's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// How the client reached the daemon, for the audit trail: `ssh://host` (`--remote`),
    /// `ws:<peer>` (set by `ptyro serve`, never by the browser).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// What a paired device may do (M9.2). Each scope includes the ones before it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Watch: sessions, screens, status, the inbox, runs.
    Read,
    /// Also answer permission prompts and gates, clear attention, mute.
    Approve,
    /// Also type into sessions and stop, resume or start configured agents.
    Control,
    /// Everything a local client can do (spawn any command, change config, upgrade). The
    /// default when a pairing names no scope: the whole web app needs it, and every place that
    /// pairs says so in plain words.
    #[default]
    Full,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Approve => "approve",
            Self::Control => "control",
            Self::Full => "full",
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "read" => Self::Read,
            "approve" => Self::Approve,
            "control" => Self::Control,
            "full" => Self::Full,
            _ => return Err(format!("unknown scope {s:?} (read, approve, control, full)")),
        })
    }
}

/// A paired device (M9.2); its token is stored only as a hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub created_ms: u64,
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    #[serde(default)]
    pub last_via: Option<String>,
    /// Web Push subscriptions it registered.
    #[serde(default)]
    pub push: u32,
    /// Its pairing code hasn't been claimed yet (it has no token).
    #[serde(default)]
    pub pending: bool,
}

/// A browser's Web Push subscription (`PushSubscription.toJSON()`, flattened).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSub {
    pub endpoint: String,
    /// The browser's P-256 public key, base64url.
    pub p256dh: String,
    /// The 16-byte auth secret, base64url.
    pub auth: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenFormat {
    #[default]
    Plain,
    Ansi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Request {
    Spawn {
        spec: SpawnSpec,
    },
    List,
    Get {
        addr: String,
    },
    /// Stop the session's process group (default: the graceful HUP→TERM→KILL sequence).
    Kill {
        addr: String,
        #[serde(default)]
        signal: Option<i32>,
    },
    /// Forget an exited session.
    Remove {
        addr: String,
    },
    Rename {
        addr: String,
        name: String,
    },
    /// Bytes to a session's input exactly as given: no paste brackets, no Enter (`send --raw`).
    SendBytes {
        addr: String,
        bytes: Vec<u8>,
    },
    SendText {
        addr: String,
        text: String,
        #[serde(default)]
        enter: bool,
        /// Wait (up to this many ms) until an agent is ready for input before typing: the reply
        /// comes once it was typed, or `timeout` (SPEC §8.2 readiness).
        #[serde(default)]
        wait_ready_ms: Option<u64>,
    },
    ReadScreen {
        addr: String,
        #[serde(default)]
        format: ScreenFormat,
    },
    Status,
    /// `force`: stop even with live sessions.
    Shutdown {
        #[serde(default)]
        force: bool,
    },
    /// Live upgrade (SPEC §4.5): re-exec into `binary` (default: the ptyro on disk), keeping
    /// every session. Answered before the handoff starts.
    Upgrade {
        #[serde(default)]
        binary: Option<PathBuf>,
    },
    /// Pair a device (M9.2): from this machine, or from a device with `full` scope. Answers a pairing
    /// code (`Pairing`): single-use, short-lived, what `ptyro pair` prints. The device's token
    /// comes from claiming it.
    DevicePair {
        name: String,
        #[serde(default)]
        scope: Scope,
    },
    /// Exchange a pairing code for the device's token (`Paired`), once. `ptyro serve` sends it
    /// for the web app's `POST /api/pair`; `via` is where the device came from.
    DeviceClaim {
        code: String,
        #[serde(default)]
        via: Option<String>,
    },
    DeviceList,
    DeviceRevoke {
        id: String,
    },
    /// The daemon's VAPID public key for `PushManager.subscribe` (M9.4).
    PushKey,
    /// Register (or replace) this device's push subscription.
    PushSubscribe {
        sub: PushSub,
    },
    PushUnsubscribe {
        endpoint: String,
    },
    /// Send a test notification to this device (or, locally, to every subscription).
    PushTest,
    /// Switch an agent between its terminal UI and structured mode, continuing the same
    /// conversation (M10.6). `force` interrupts a turn in progress.
    Convert {
        addr: String,
        structured: bool,
        #[serde(default)]
        force: bool,
    },
    /// Change what a session (`worktree`: a worktree) is titled, for people to read; `None`
    /// clears it (UX1.24). Its name, the address, stays.
    Retitle {
        addr: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        worktree: bool,
    },
    /// A summary of what a session is doing, from `[summaries] command`, now (UX1.3); replies
    /// `Session` with it in `summary`.
    Summarize {
        addr: String,
    },
    /// An agent started by hand in this shell session becomes a session of its own, managed like
    /// `new --agent` (hooks, hibernation, mail), resuming the same conversation (UX1.4).
    Adopt {
        addr: String,
    },
    /// The quick terminal (UX1.25) of `worktree` (an address), else of the worktree `cwd` is in,
    /// else of `cwd`: one shell each, reused. With `create`, one starts when there's none
    /// (spawned as `Spawn` would, `env` and `size` included). Replies `Session`.
    QuickShell {
        #[serde(default)]
        worktree: Option<String>,
        cwd: PathBuf,
        #[serde(default)]
        create: bool,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        size: Option<TermSize>,
        /// Its home workspace, for one outside any project (default `main`).
        #[serde(default)]
        workspace: Option<String>,
    },
    /// The scratchpad (NOTE1): its text and revision. Replies `Scratch`.
    ScratchGet,
    /// Write `text`, edited from the revision `base`; if the file moved on meanwhile the two are
    /// merged by lines (both kept between markers where they clash). Replies `ScratchPut`.
    ScratchPut {
        base: String,
        text: String,
    },
    /// Add an entry (a stamped `- [ ]` item, or a note); `session` says who is adding, for the
    /// stamp's worktree and agent. Replies `Scratch`.
    ScratchAdd {
        text: String,
        #[serde(default)]
        section: Option<String>,
        #[serde(default)]
        note: bool,
        #[serde(default = "yes")]
        stamp: bool,
        /// The adding session (address), else `whence` is taken from this directory.
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    /// The checkbox items (the open ones, unless `all`). Replies `ScratchItems`.
    ScratchTodo {
        #[serde(default)]
        all: bool,
    },
    /// Tick, untick or tag the item `sel` (a line number, or words in one open item). Replies
    /// `ScratchItems` with that item.
    ScratchMark {
        sel: String,
        mark: PadMark,
    },
    /// Saved copies of the pad, newest first. Replies `ScratchLog`.
    ScratchHistory,
    /// Put a saved copy back (the current text is saved first). Replies `Scratch`.
    ScratchRestore {
        rev: String,
    },
    /// The scratchpad's editor session: one, in a pane. With `create`, started when there is
    /// none (spawned as `Spawn` would, `env` and `size` included). Replies `Session`.
    ScratchPane {
        #[serde(default)]
        create: bool,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        size: Option<TermSize>,
        #[serde(default)]
        workspace: Option<String>,
    },
    /// A structured session's whole transcript (M10).
    ChatGet {
        addr: String,
    },
    /// Expand a selector (SPEC §2.3): names, handles, globs (`build-*`), `@all`, `@live`,
    /// `@exited`, `@focused`, `@last`, `@working`/`@blocked`/`@idle`/`@done`; `,` joins several.
    Resolve {
        addr: String,
    },
    /// Start an exited session's command again, in place (same id, name, cwd, size).
    Restart {
        addr: String,
    },
    /// The last `lines` lines of history plus the screen, frozen (copy mode, `read --scrollback`).
    ReadScrollback {
        addr: String,
        #[serde(default)]
        lines: Option<u32>,
    },
    /// Persist a client's view (layout, sidebar…) under `name` (SPEC §11).
    SaveView {
        name: String,
        view: serde_json::Value,
    },
    LoadView {
        name: String,
    },
    /// Re-read the config file now (after a CLI edit), instead of waiting for the file watch.
    ReloadConfig,
    /// Sessions the store remembers, newest first, including earlier daemon runs.
    History {
        #[serde(default)]
        limit: Option<u32>,
    },

    // ── projects (SPEC §10.1) ──
    /// Register the repository containing `path`; added to `workspace` (default: the one the
    /// TUI showed last, else `main`).
    ProjectAdd {
        path: PathBuf,
        #[serde(default)]
        workspace: Option<String>,
    },
    ProjectList,
    ProjectGet {
        addr: String,
    },
    /// Forget a project (its worktrees stay on disk). It is never registered on its own again
    /// (UX1.5). `release`: let its running sessions go instead of refusing (they keep running,
    /// outside any project), as undoing an automatic registration does.
    ProjectRemove {
        addr: String,
        #[serde(default)]
        release: bool,
    },
    /// Pin to a profile, or unpin with `None` (SPEC §10.10).
    ProjectPin {
        addr: String,
        #[serde(default)]
        profile: Option<String>,
    },
    /// The lifecycle scripts as they'd run in `worktree` (default `@root`), with trust state.
    ProjectScripts {
        addr: String,
        #[serde(default)]
        worktree: Option<String>,
    },
    /// Trust exactly the scripts with this fingerprint (what the user was shown).
    ProjectTrust {
        addr: String,
        fingerprint: String,
    },
    ProjectUntrust {
        addr: String,
    },
    /// Git repositories under `discover.roots` (cached; `refresh` rescans).
    Discover {
        #[serde(default)]
        refresh: bool,
    },

    // ── workspaces (SPEC §2.1, §9.6) ──
    WorkspaceList,
    WorkspaceNew {
        name: String,
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        notify: Option<String>,
    },
    WorkspaceSet {
        name: String,
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        notify: Option<String>,
    },
    WorkspaceRename {
        name: String,
        new_name: String,
    },
    WorkspaceRemove {
        name: String,
    },
    /// Add a project to a workspace, or (with `remove`) take it out.
    WorkspaceMember {
        name: String,
        project: String,
        #[serde(default)]
        remove: bool,
    },

    // ── worktrees (SPEC §10.2–10.9) ──
    WorktreeList {
        #[serde(default)]
        project: Option<String>,
    },
    WorktreeGet {
        addr: String,
    },
    WorktreeNew {
        spec: WorktreeSpec,
    },
    WorktreeRemove {
        addr: String,
        #[serde(default)]
        opts: RemoveOpts,
    },
    /// Recompute repository status now.
    WorktreeRefresh {
        addr: String,
    },
    /// Run a lifecycle script in a `script` session: `setup`, `archive`, `after_turn`, a run
    /// script name, or `None` for the default run script.
    WorktreeRun {
        addr: String,
        #[serde(default)]
        script: Option<String>,
        #[serde(default)]
        size: Option<TermSize>,
    },
    WorktreeLock {
        addr: String,
        lock: bool,
    },
    /// Rename the branch (refused once pushed); `None` derives a name from the first commit.
    WorktreeRenameBranch {
        addr: String,
        #[serde(default)]
        branch: Option<String>,
    },
    /// Stale worktrees (never removes anything).
    Gc,
    /// A project's branches a worktree could check out (UX1.10): local ones not checked out
    /// anywhere, then the main remote's without a local branch.
    Branches {
        project: String,
    },
    /// What a project's first worktree could use (UX1.11): a setup script from its lockfile,
    /// `.env` files to copy. `apply`: `Some(true)` writes them to `.ptyro.local.toml`,
    /// `Some(false)` declines; either way they're never offered again.
    ProjectSuggest {
        addr: String,
        #[serde(default)]
        apply: Option<bool>,
    },
    /// Archive a worktree (UX1.12): snapshot its files, keep its branch, remove it. Never loses
    /// work, so it doesn't refuse the way removing does. `stop_sessions` stops what runs in it.
    WorktreeArchive {
        addr: String,
        #[serde(default)]
        stop_sessions: bool,
    },
    /// Bring an archived worktree back as it was (`name`, or `project/name`).
    WorktreeRestore {
        addr: String,
    },
    /// Archived worktrees, newest first.
    WorktreeArchives {
        #[serde(default)]
        project: Option<String>,
    },
    /// Finish a worktree (UX1.12): commit what's left (its agent asked for the message, else
    /// its title is the message), then open a PR, or `integrate` into the main branch.
    WorktreeFinish {
        addr: String,
        #[serde(default)]
        integrate: bool,
    },

    // ── profiles (SPEC §10.10) ──
    ProfileList,
    /// The environment a session would get, secrets redacted.
    ProfileEnv {
        name: String,
    },
    /// Rewrite every stored reference to a profile (the CLI edits the config file).
    ProfileRename {
        old: String,
        new: String,
    },
    /// Which profile a spawn would get, and why (the wizard shows it).
    ProfileResolve {
        spec: SpawnSpec,
    },

    // ── agents (SPEC §8, §9) ──
    /// From `ptyro hook <adapter> <event>` inside a session (SPEC §8.3).
    Hook {
        /// `PTYRO_SESSION_ID`.
        session: String,
        adapter: String,
        event: String,
        #[serde(default)]
        payload: serde_json::Value,
        /// Selected variables of the hook's environment (Claude's messaging socket).
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
    },
    /// Claude's statusline JSON, from `ptyro statusline-relay` (usage meters, M4.17).
    StatusLine {
        session: String,
        payload: serde_json::Value,
    },
    /// A program reporting its own state (`ptyro report`, herdr-compatible; SPEC §9.2).
    Report {
        /// A session address; `PTYRO_SESSION_ID` from inside one.
        addr: String,
        state: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        msg: Option<String>,
        #[serde(default)]
        progress: Option<u8>,
    },
    /// The status engine's recent decisions for a session ("why is it blocked?").
    StatusExplain {
        addr: String,
    },
    /// Relaunch a dormant (or exited, resumable) agent into its conversation (SPEC §7.4).
    Resume {
        addr: String,
    },
    /// Stop an idle agent and keep it resumable.
    Hibernate {
        addr: String,
    },
    ClearAttention {
        addr: String,
    },
    /// Answer the agent's permission prompt: `allow`, `always` or `deny`.
    Answer {
        addr: String,
        decision: String,
        /// The prompt this answers: the blocked status's `since_ms` the client showed. A
        /// different prompt pending now is refused (a stale notification never approves a newer
        /// request). `None`: whatever is pending.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<u64>,
    },
    Inbox {
        #[serde(default)]
        workspace: Option<String>,
        #[serde(default)]
        unread_only: bool,
        #[serde(default)]
        limit: Option<u32>,
    },
    InboxMark {
        #[serde(default)]
        ids: Vec<i64>,
        /// Every entry (of `session`, when given).
        #[serde(default)]
        all: bool,
        #[serde(default)]
        session: Option<String>,
        #[serde(default = "yes")]
        read: bool,
    },
    /// Silence notifications (never the inbox) for a session, `project:<slug>` or
    /// `workspace:<name>`, for `minutes` (default: until unmuted).
    Mute {
        target: String,
        #[serde(default)]
        minutes: Option<u32>,
        #[serde(default)]
        unmute: bool,
    },
    Usage,
    /// The agents a session can run (built-in and configured) and which is the default, for
    /// clients that don't read the config themselves (the web app's new-session form).
    Agents,
    /// The editors a worktree can be opened in (built in and configured) and the default.
    Editors,
    /// Open a worktree in an editor (default: `editors.default`): a terminal editor starts as a
    /// session (replies `Session`), any other on the computer running the daemon (`Notes`).
    WorktreeOpen {
        addr: String,
        #[serde(default)]
        editor: Option<String>,
    },
    /// A project's configuration as TOML, with where each value comes from (`project config
    /// --explain`), as `Text`.
    ProjectConfig {
        addr: String,
    },
    /// A session's recording as screen frames, to play back (`ptyro replay`, for the web).
    Replay {
        addr: String,
    },
    /// A session's recording as `asciicast` or `text` (`ptyro export`, for the web).
    Export {
        addr: String,
        format: String,
    },
    /// The web app's server, when the daemon runs it (`[remote] serve`, 1.16): `None` asks how
    /// it is; `Some(true)` or `Some(false)` turns the setting on or off (edits the config file)
    /// and starts or stops it. `via` says how phones reach it when turning it on: `tailscale`,
    /// `lan` (this network) or `here` (this computer only); left out, a choice is made if none
    /// is set. Replies `Web`. Turning it on or off is `full` scope.
    Web {
        #[serde(default)]
        on: Option<bool>,
        #[serde(default)]
        via: Option<String>,
    },
    /// Send a test notification through every configured channel and sink.
    NotifyTest,

    // ── scripting and diagnostics (SPEC §12, M5) ──
    /// Persisted session events, newest last (`ptyro events` without `--follow`).
    Events {
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    /// What the daemon can tell `ptyro doctor` about itself.
    Doctor,
    /// Run `ptyro fake-agent --as <adapter>` through the real integration and report which
    /// signals arrived (`ptyro doctor`'s hook self-test).
    SelfTest {
        adapter: String,
    },

    // ── review and ship (SPEC §10.11, M6) ──
    /// The worktree's changes against `against` (its working tree included).
    WorktreeDiff {
        addr: String,
        #[serde(default)]
        against: DiffAgainst,
    },
    /// Commits since the base, newest first.
    WorktreeLog {
        addr: String,
    },
    /// Stage (`all`, or `paths`) and commit as the worktree's profile.
    WorktreeCommit {
        addr: String,
        message: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        all: bool,
    },
    /// Rebase onto the base and fast-forward `into` (default: the project's default branch), or
    /// cherry-pick the commits there.
    WorktreeIntegrate {
        addr: String,
        #[serde(default)]
        into: Option<String>,
        #[serde(default)]
        cherry_pick: bool,
        /// Push the target branch afterwards.
        #[serde(default)]
        push: bool,
    },
    /// Pair mode: show the worktree's state in the project's main checkout, and (`off`) put the
    /// main checkout back, carrying edits made there into the worktree.
    WorktreePair {
        addr: String,
        #[serde(default)]
        off: bool,
        /// Re-sync the main checkout with the worktree.
        #[serde(default)]
        refresh: bool,
    },
    /// The worktree's pull request (`refresh` asks `gh` now).
    PrGet {
        addr: String,
        #[serde(default)]
        refresh: bool,
    },
    /// Push the branch and open a pull request.
    PrCreate {
        addr: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        base: Option<String>,
    },
    /// Send the failing checks' logs to the worktree's agent.
    PrFix {
        addr: String,
    },
    /// `gh pr merge --auto` (or turn it off).
    PrAutomerge {
        addr: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        off: bool,
    },
    /// Feed the PR's new failures and review comments to the worktree's agent (`off` stops).
    PrWatch {
        addr: String,
        #[serde(default)]
        off: bool,
    },
    /// Reply on a review thread (or the PR) with "via ptyro".
    PrReply {
        addr: String,
        #[serde(default)]
        comment: Option<u64>,
        text: String,
    },
    ReviewAdd {
        addr: String,
        path: String,
        line: u32,
        #[serde(default)]
        code: Option<String>,
        text: String,
    },
    ReviewList {
        addr: String,
    },
    /// Remove one comment, or all of them.
    ReviewRemove {
        addr: String,
        #[serde(default)]
        id: Option<u32>,
    },
    /// Batch the comments into one prompt for the worktree's agent (or `session`).
    ReviewSend {
        addr: String,
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    /// Worktrees ready for review, ranked.
    ReviewQueue,
    /// Mark a worktree reviewed (its current turn).
    ReviewDone {
        addr: String,
    },
    // ── orchestration (SPEC §15, M7) ──
    RunList {
        #[serde(default)]
        active: bool,
        #[serde(default)]
        limit: Option<u32>,
    },
    RunGet {
        id: String,
    },
    /// The run's event log, oldest first.
    RunEvents {
        id: String,
    },
    RunCancel {
        id: String,
    },
    /// Put a finished, failed or held step back to pending (a fresh attempt).
    RunRetry {
        id: String,
        step: String,
    },
    /// A new run that copies every step before `from` and runs from there (`--set` vars).
    RunFork {
        id: String,
        from: String,
        #[serde(default)]
        set: BTreeMap<String, String>,
    },
    /// Recipes: built-ins, `~/.config/ptyro/recipes/`, and the project's `.ptyro/recipes/`.
    RecipeList {
        #[serde(default)]
        project: Option<String>,
    },
    RecipeShow {
        name: String,
        #[serde(default)]
        project: Option<String>,
    },
    /// Start a recipe by name, or from `src` (a file the CLI read).
    RecipeRun {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        src: Option<String>,
        #[serde(default)]
        vars: BTreeMap<String, String>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        base: Option<String>,
        /// The fingerprint of a repo recipe the user just approved.
        #[serde(default)]
        trust: Option<String>,
    },
    Fanout {
        spec: FanoutSpec,
    },
    GateList {
        #[serde(default)]
        all: bool,
    },
    /// Answer a gate. Idempotent per `key`: a second tap with the same answer is a no-op.
    GateResolve {
        id: String,
        choice: String,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        by: Option<String>,
    },
    /// The compare view of a fanout (or any run with candidates); `refresh` re-runs evidence.
    Compare {
        run: String,
        #[serde(default)]
        refresh: bool,
    },
    /// Take a candidate: merge/cherry-pick into `into`, open a PR, or apply chosen paths/hunks.
    Pick {
        run: String,
        candidate: String,
        #[serde(default)]
        mode: PickMode,
        #[serde(default)]
        into: Option<String>,
        /// Only these files (apply mode).
        #[serde(default)]
        paths: Vec<String>,
        /// Only these hunks, `FILE:N` (1-based, apply mode).
        #[serde(default)]
        hunks: Vec<String>,
    },
    /// Remove candidates' worktrees safely (every one not picked, without `candidate`).
    Discard {
        run: String,
        #[serde(default)]
        candidate: Option<String>,
        #[serde(default)]
        force: bool,
    },
    /// A follow-up for one candidate's agent (or all of them).
    Iterate {
        run: String,
        #[serde(default)]
        candidate: Option<String>,
        text: String,
    },
    /// Post a message: `to` is a session, `@lead`, `@user`, `@all`, `@idle`, `@<agent>`,
    /// `@run:<id>`; groups expand to one delivery per recipient now.
    MsgPost {
        #[serde(default)]
        from: Option<String>,
        to: String,
        #[serde(default)]
        kind: Option<String>,
        body: String,
        #[serde(default)]
        run: Option<String>,
        #[serde(default)]
        in_reply_to: Option<String>,
        /// `interrupt` delivers mid-turn where the agent allows it.
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        key: Option<String>,
    },
    /// Messages for `me` after `since` (a message id), waiting up to `wait_ms` for one.
    MsgCheck {
        me: String,
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        wait_ms: Option<u64>,
    },
    MsgAck {
        me: String,
        ids: Vec<String>,
    },
    /// The ledger (of a run).
    MsgList {
        #[serde(default)]
        run: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
    },
    LeaseClaim {
        holder: String,
        patterns: Vec<String>,
        #[serde(default)]
        exclusive: bool,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Release leases by id or pattern (all of the holder's, without either).
    LeaseRelease {
        holder: String,
        #[serde(default)]
        patterns: Vec<String>,
    },
    LeaseList,
    /// Predicted collisions between the project's live worktrees.
    Radar {
        #[serde(default)]
        project: Option<String>,
    },
    /// Checkpoints of a session (or of every session in a worktree).
    CheckpointList {
        addr: String,
    },
    CheckpointDiff {
        addr: String,
        n: u32,
        /// Against this checkpoint (default: the working tree now).
        #[serde(default)]
        to: Option<u32>,
    },
    /// Put the worktree back to checkpoint `n` (or the one that closed turn `turn`); the state
    /// before is checkpointed first.
    CheckpointRestore {
        addr: String,
        #[serde(default)]
        n: Option<u32>,
        #[serde(default)]
        turn: Option<u32>,
    },
    /// Verified-done for a session (`None` turns it off).
    SetVerify {
        addr: String,
        #[serde(default)]
        check: Option<String>,
        #[serde(default)]
        max_bounces: Option<u32>,
    },
    /// Make a session the lead of a new run (it may then use `ptyro mcp`).
    LeadStart {
        addr: String,
        #[serde(default)]
        policy: Option<ptyro_run::Policy>,
    },
    /// A lead agent's request through `ptyro mcp` (policy-checked).
    Lead {
        lead: String,
        op: LeadOp,
    },
    /// Change sets (M11): `ptyro cs …` and a set lead's `cs_*` tools. Agents may only do what
    /// their role allows; shipping is the human's.
    Cs {
        op: ptyro_run::wire::CsOp,
    },
}

/// One persisted session event (`session_event`, SPEC §14.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub session: String,
    pub at_ms: u64,
    pub kind: String,
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// The daemon's side of `ptyro doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonDoctor {
    /// Account-selecting variables set in the daemon's own environment; they would leak into
    /// every `default`-profile session (SPEC §10.10).
    #[serde(default)]
    pub leaked_vars: Vec<String>,
    /// `PATH` as sessions see it (a service manager's PATH can be short).
    #[serde(default)]
    pub path: String,
    /// Problems the daemon noticed while running (an agent's hooks needing review, …).
    #[serde(default)]
    pub warnings: Vec<String>,
    /// The daemon's config failed to load and it runs on defaults.
    #[serde(default)]
    pub config_error: Option<String>,
}

/// The result of a hook self-test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfTestResult {
    pub adapter: String,
    pub ok: bool,
    /// The statuses seen, in order, with their source (`working via hook`).
    pub seen: Vec<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub proto: Version,
    pub pid: u32,
    pub started_ms: u64,
    pub sessions: u32,
    pub live_sessions: u32,
    pub clients: u32,
    pub socket: PathBuf,
    pub log_dir: PathBuf,
    /// Live upgrades since the daemon started (M8.1).
    #[serde(default)]
    pub generation: u32,
    #[serde(default)]
    pub upgraded_ms: Option<u64>,
    /// The last upgrade was refused by the new binary and rolled back.
    #[serde(default)]
    pub rolled_back: bool,
    /// Health (M8.5).
    #[serde(default)]
    pub metrics: Option<Metrics>,
    /// The web app's server (1.16).
    #[serde(default)]
    pub web: Option<WebServe>,
    /// The short name of the computer the daemon runs on (`[daemon] machine_name`, else its host
    /// name): what a client of a remote daemon shows, not its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// The web app's server, `ptyro serve`, as the daemon sees it (`[remote] serve`, SPEC §16.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebServe {
    /// `[remote] serve` is on: the daemon runs the server and brings it back when it dies.
    pub wanted: bool,
    /// It is listening now (it has said where).
    pub up: bool,
    pub pid: Option<u32>,
    /// What phones open (the address `ptyro pair` puts in its link).
    pub url: Option<String>,
    /// Where it listens, as configured.
    pub listen: String,
    /// SHA-256 of the certificate authority ptyro made, to check it against when trusting it.
    pub ca_fingerprint: Option<String>,
    pub since_ms: Option<u64>,
    /// Times the daemon started it again after it died.
    pub restarts: u32,
    /// How the last run ended, and the end of what it said then.
    pub last_exit: Option<String>,
    /// A `ptyro serve` someone started by hand is running (its pid): the daemon's can't share
    /// its port, and it won't come back after a restart.
    pub by_hand: Option<u32>,
    /// The server's log.
    pub log: PathBuf,
}

/// An agent a session can run (`Request::Agents`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentChoice {
    pub name: String,
    /// The program it runs.
    pub program: String,
    /// The program is on `PATH`.
    pub installed: bool,
}

/// One step of a replay: the screen at `at_ms` into the recording, as a keyframe or the changes
/// since the step before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStep {
    pub at_ms: u64,
    pub keyframe: bool,
    pub delta: ScreenDelta,
}

/// A recording to play back (`Request::Replay`), sampled to a few hundred steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayDoc {
    pub size: TermSize,
    pub duration_ms: u64,
    pub steps: Vec<ReplayStep>,
    /// The recording was longer than what fits in one reply: this is its start.
    #[serde(default)]
    pub truncated: bool,
}

/// What the daemon's watchdog measures (M8.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    pub rss_kib: u64,
    pub open_fds: u32,
    pub tasks_alive: u32,
    /// The runtime's scheduling delay: the watchdog ticks every second and measures how late.
    pub loop_lag_ms_max: u64,
    pub loop_lag_ms_p99: u64,
    pub pty_readers: u32,
    pub bytes_read: u64,
    pub frames_sent: u64,
    /// Panics caught (crash reports in `state/crashes/`).
    pub crashes: u32,
    pub scrollback_bytes: u64,
    /// Per session: `(name, approximate bytes of terminal state)`.
    pub sessions: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Short-lived wire messages: boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Reply {
    Session {
        info: SessionInfo,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    /// The scratchpad (NOTE1).
    Scratch {
        doc: PadDoc,
    },
    /// After `ScratchPut`: what was written (the merge, when the file had moved on).
    ScratchPut {
        doc: PadDoc,
        /// The text differs from what was put: another write was merged in.
        merged: bool,
        /// Regions kept both ways between `<<<<<<<` markers.
        conflicts: u32,
    },
    ScratchItems {
        rev: String,
        items: Vec<Item>,
    },
    ScratchLog {
        snapshots: Vec<Snapshot>,
    },
    Screen {
        text: String,
    },
    Status {
        status: DaemonStatus,
    },
    Scrollback {
        doc: ScrollbackDoc,
    },
    View {
        #[serde(default)]
        view: Option<serde_json::Value>,
    },
    History {
        entries: Vec<HistoryEntry>,
    },
    Project {
        info: ProjectInfo,
        /// What happened on the way (auto-pin, discovered default branch).
        #[serde(default)]
        notes: Vec<String>,
    },
    Projects {
        projects: Vec<ProjectInfo>,
    },
    Scripts {
        scripts: ScriptsInfo,
    },
    Paths {
        paths: Vec<PathBuf>,
    },
    Workspace {
        info: WorkspaceInfo,
    },
    Workspaces {
        workspaces: Vec<WorkspaceInfo>,
    },
    Worktree {
        info: WorktreeInfo,
    },
    Worktrees {
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeCreated {
        info: WorktreeInfo,
        /// The setup script's session, when it runs.
        #[serde(default)]
        setup: Option<SessionInfo>,
        /// The agent, if it started right away.
        #[serde(default)]
        agent: Option<SessionInfo>,
        /// The agent starts when setup finishes.
        #[serde(default)]
        agent_pending: bool,
        /// Scripts that did not run because they aren't trusted.
        #[serde(default)]
        untrusted: Option<ScriptsInfo>,
        #[serde(default)]
        notes: Vec<String>,
        /// From an issue with no agent in the spec: the first prompt for the agent the client
        /// starts itself (`ptyro new --issue`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue_prompt: Option<String>,
    },
    /// What a removal did (kept branches, archive script, released ports).
    Removed {
        notes: Vec<String>,
    },
    Gc {
        entries: Vec<GcEntry>,
    },
    Archives {
        entries: Vec<ptyro_core::model::ArchiveEntry>,
    },
    Branches {
        names: Vec<String>,
    },
    Suggestions {
        suggestions: ptyro_core::model::SetupSuggestions,
    },
    Profiles {
        profiles: Vec<ProfileInfo>,
    },
    Env {
        vars: Vec<(String, String)>,
    },
    Explain {
        lines: Vec<ExplainLine>,
    },
    Inbox {
        entries: Vec<InboxEntry>,
    },
    Usage {
        usage: Vec<Usage>,
    },
    Agents {
        agents: Vec<AgentChoice>,
        default: String,
    },
    /// How the web app's server is, and what a change did (`Request::Web`).
    Web {
        web: WebServe,
        notes: Vec<String>,
    },
    Replay {
        replay: ReplayDoc,
    },
    Editors {
        names: Vec<String>,
        default: String,
    },
    /// A document as text (an export, a file).
    Text {
        text: String,
    },
    /// What a notification test did, per channel.
    Notified {
        results: Vec<String>,
    },
    Events {
        events: Vec<EventRecord>,
    },
    Diff {
        doc: DiffDoc,
    },
    Commits {
        commits: Vec<CommitInfo>,
    },
    Committed {
        sha: String,
    },
    Notes {
        notes: Vec<String>,
    },
    Pr {
        #[serde(default)]
        info: Option<PrInfo>,
    },
    Comments {
        comments: Vec<ReviewComment>,
    },
    ReviewQueue {
        items: Vec<ReviewItem>,
    },
    /// The spawn waits: `max_unreviewed` worktrees wait for review (M6.9).
    Queued {
        position: u32,
        unreviewed: u32,
    },
    Doctor {
        report: DaemonDoctor,
    },
    SelfTest {
        result: SelfTestResult,
    },
    Runs {
        runs: Vec<RunInfo>,
    },
    Run {
        run: RunInfo,
    },
    RunEvents {
        events: Vec<(u64, u64, serde_json::Value)>,
    },
    Recipes {
        recipes: Vec<RecipeInfo>,
    },
    Recipe {
        info: RecipeInfo,
        src: String,
    },
    Gates {
        gates: Vec<GateInfo>,
    },
    Compare {
        doc: CompareDoc,
    },
    Messages {
        messages: Vec<Message>,
    },
    Posted {
        ids: Vec<String>,
        /// Recipients by name.
        to: Vec<String>,
    },
    Leases {
        leases: Vec<Lease>,
        /// Patterns refused because another session holds them.
        #[serde(default)]
        refused: Vec<String>,
    },
    Radar {
        radar: Radar,
    },
    Checkpoints {
        checkpoints: Vec<Checkpoint>,
    },
    /// A lead op's answer (JSON, as `ptyro mcp` returns it).
    LeadResult {
        value: serde_json::Value,
    },
    /// A change-set op's answer (`ChangeSetInfo`, `[LedgerEntry]`, notes…, as JSON).
    Cs {
        value: serde_json::Value,
    },
    /// What a hook should print for the agent (a Stop bounce, a PreToolUse denial).
    HookOutput {
        stdout: String,
    },
    /// A device waiting for its pairing code to be claimed (until `expires_ms`).
    Pairing {
        device: DeviceInfo,
        code: String,
        expires_ms: u64,
        /// A device with this name is already paired: claiming this code replaces it (its
        /// token stops working, its connections close, this scope is the one that counts).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<DeviceInfo>,
    },
    /// A claimed pairing: the device's token, returned only this once.
    Paired {
        device: DeviceInfo,
        token: String,
        /// The earlier device of the same name this claim replaced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced: Option<DeviceInfo>,
    },
    Transcript {
        items: Vec<ptyro_core::chat::ChatItem>,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    PushKey {
        /// Uncompressed P-256 point, base64url.
        key: String,
    },
    /// The handoff starts now; the connection will close.
    Upgrading {
        from: String,
        to: String,
        binary: PathBuf,
        sessions: u32,
    },
    Resolved {
        profile: String,
        reason: ProfileReason,
        /// Set when spawning would ask first (guard or pin override).
        #[serde(default)]
        question: Option<String>,
    },
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Short-lived wire messages: boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Outcome {
    Ok { reply: Reply },
    Err { error: Error },
}

impl From<Result<Reply, Error>> for Outcome {
    fn from(r: Result<Reply, Error>) -> Self {
        match r {
            Ok(reply) => Self::Ok { reply },
            Err(error) => Self::Err { error },
        }
    }
}

impl From<Outcome> for Result<Reply, Error> {
    fn from(o: Outcome) -> Self {
        match o {
            Outcome::Ok { reply } => Ok(reply),
            Outcome::Err { error } => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Short-lived wire messages, like `ServerMsg`: boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        proto: Version,
        client: ClientInfo,
        #[serde(default)]
        features: Vec<String>,
    },
    Request {
        id: u32,
        req: Request,
    },
    /// Start receiving frames. `control`: become the controller (its size wins, SPEC §5.8).
    Subscribe {
        session: SessionId,
        #[serde(default)]
        size: Option<TermSize>,
        #[serde(default)]
        control: bool,
    },
    Unsubscribe {
        session: SessionId,
    },
    /// Finished painting up to `seq` (one unacked frame per subscription, SPEC §6.5).
    Ack {
        session: SessionId,
        seq: u64,
    },
    Input {
        session: SessionId,
        event: InputEvent,
    },
    /// Honoured only from the controller.
    Resize {
        session: SessionId,
        size: TermSize,
    },
    WatchEvents,
    /// The session this client's user is looking at (`@focused`), and whether the client's
    /// terminal window has focus: a session shown in a focused terminal raises no attention.
    Focus {
        session: SessionId,
        #[serde(default = "yes")]
        terminal_focused: bool,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenFrame {
    pub session: SessionId,
    /// Applying this frame brings the client up to `seq`.
    pub seq: u64,
    /// Discard the cache (and style table) first.
    pub keyframe: bool,
    pub delta: ScreenDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DomainEvent {
    SessionCreated {
        info: SessionInfo,
    },
    SessionUpdated {
        info: SessionInfo,
    },
    SessionExited {
        info: SessionInfo,
    },
    SessionRemoved {
        id: SessionId,
    },
    ControllerChanged {
        session: SessionId,
        #[serde(default)]
        controller: Option<ClientId>,
    },
    Bell {
        session: SessionId,
    },
    /// OSC 52 copy from the program; sent only to the session's controller (SPEC §17).
    Clipboard {
        session: SessionId,
        text: String,
    },
    /// The session's own process was suspended (Ctrl-Z) with no shell to `fg` it, so the daemon
    /// resumed it.
    SessionResumed {
        session: SessionId,
    },
    /// OSC 9/777/99 notification from the program.
    Notification {
        session: SessionId,
        text: String,
    },
    /// The session's command was started again (same id); viewers should resubscribe.
    SessionRestarted {
        info: SessionInfo,
    },
    /// The user config file changed; `error` is set if it no longer parses (the old one stays).
    ConfigReloaded {
        #[serde(default)]
        error: Option<String>,
    },
    ProjectUpdated {
        info: ProjectInfo,
    },
    ProjectRemoved {
        id: String,
    },
    /// A repository a session runs in was registered on its own (UX1.5), in `workspace`.
    ProjectAutoAdded {
        info: ProjectInfo,
        workspace: String,
        #[serde(default)]
        notes: Vec<String>,
    },
    WorkspaceUpdated {
        info: WorkspaceInfo,
    },
    WorkspaceRemoved {
        id: String,
    },
    /// Created, changed state, or new repository status.
    WorktreeUpdated {
        info: WorktreeInfo,
    },
    WorktreeRemoved {
        id: String,
    },
    /// A step of worktree creation or removal (SPEC §10.3: progress streamed as events).
    WorktreeProgress {
        worktree: String,
        step: String,
    },
    InboxAdded {
        entry: InboxEntry,
    },
    /// The scratchpad changed, from anywhere (a client, an agent, an editor).
    ScratchChanged {
        rev: String,
        /// Open `- [ ]` items.
        open: u32,
        /// Who: a device, an agent's session, `editor` or `file` for a change made outside.
        #[serde(default)]
        by: Option<String>,
    },
    /// Show this notification (TUIs: OSC 9/777 to the outer terminal, and a bell).
    Notify {
        session: SessionId,
        kind: AttentionKind,
        title: String,
        body: String,
        /// Ring the bell too.
        bell: bool,
    },
    UsageUpdated {
        usage: Usage,
    },
    /// A run changed (a step started or ended, a gate opened or closed).
    RunUpdated {
        run: RunInfo,
    },
    /// This client fell behind and missed `count` events (only it gets this): re-read state.
    EventsLost {
        count: u64,
    },
}

/// What to do to a scratchpad item (`ScratchMark`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadMark {
    /// Tick it, with the pull request that finished it.
    Done {
        #[serde(default)]
        pr: Option<u64>,
    },
    /// Untick it.
    Open,
    /// Note the worktree it is being worked in.
    Started { worktree: String },
}

/// The scratchpad as clients see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadDoc {
    pub path: PathBuf,
    pub rev: String,
    pub text: String,
    /// Open and ticked items.
    pub open: u32,
    pub done: u32,
}

impl PadDoc {
    pub fn of(path: PathBuf, text: String) -> Self {
        let items = ptyro_core::scratchpad::items(&text);
        let done = items.iter().filter(|i| i.done).count();
        Self {
            path,
            rev: ptyro_core::scratchpad::rev(&text),
            open: u32::try_from(items.len() - done).unwrap_or(u32::MAX),
            done: u32::try_from(done).unwrap_or(u32::MAX),
            text,
        }
    }
}

fn yes() -> bool {
    true
}

/// One status-engine decision, for `ptyro status --explain` (SPEC §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainLine {
    pub at_ms: u64,
    pub source: String,
    pub signal: String,
    pub applied: bool,
    /// Why it was or wasn't applied.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Short-lived wire messages: boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        proto: Version,
        daemon_version: String,
        client_id: ClientId,
        #[serde(default)]
        features: Vec<String>,
        started_ms: u64,
        /// What a paired device may do (1.15); absent for a local client, which may do anything.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<Scope>,
        /// The daemon's machine (see `DaemonStatus::machine`), so a client can show it at once.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    Reject {
        reason: String,
        daemon_version: String,
    },
    Response {
        id: u32,
        outcome: Outcome,
    },
    Screen {
        frame: ScreenFrame,
    },
    Event {
        event: DomainEvent,
    },
    /// The daemon is going away; clients may reconnect.
    Goodbye {
        reason: String,
    },
    Pong,
    /// A structured session's transcript (M10): everything on subscribe (`reset`), then what
    /// changed. `draft` is what's being typed into it.
    Chat {
        session: SessionId,
        rev: u64,
        #[serde(default)]
        reset: bool,
        items: Vec<ptyro_core::chat::ChatItem>,
        #[serde(default)]
        draft: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    #[default]
    MsgPack,
    Json,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("msgpack encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("empty frame")]
    Empty,
    #[error("zstd: {0}")]
    Zstd(std::io::Error),
}

/// The feature both sides list to compress frames (M8.7).
pub const FEATURE_ZSTD: &str = "zstd";
/// Frames smaller than this aren't worth compressing.
pub const COMPRESS_MIN: usize = 512;
/// A zstd frame's magic number; no MessagePack map or JSON object starts with it, so a payload
/// says itself whether it is compressed.
pub const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// [`encode`], then zstd (level 1: fast; screens compress ~5–10×) when it pays.
pub fn encode_compressed<T: Serialize>(msg: &T, enc: Encoding) -> Result<bytes::Bytes, CodecError> {
    let raw = encode(msg, enc)?;
    if raw.len() < COMPRESS_MIN {
        return Ok(raw);
    }
    let z = zstd::bulk::compress(&raw, 1).map_err(CodecError::Zstd)?;
    Ok(if z.len() < raw.len() { z.into() } else { raw })
}

pub fn encode<T: Serialize>(msg: &T, enc: Encoding) -> Result<bytes::Bytes, CodecError> {
    Ok(match enc {
        Encoding::MsgPack => rmp_serde::to_vec_named(msg)?.into(),
        Encoding::Json => serde_json::to_vec(msg)?.into(),
    })
}

/// Decode a payload, reporting which encoding it used.
pub fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<(T, Encoding), CodecError> {
    if payload.starts_with(&ZSTD_MAGIC) {
        let raw = zstd::bulk::decompress(payload, MAX_FRAME).map_err(CodecError::Zstd)?;
        if raw.starts_with(&ZSTD_MAGIC) {
            return Err(CodecError::Empty);
        }
        return decode(&raw);
    }
    match payload.first() {
        None => Err(CodecError::Empty),
        Some(b'{') => Ok((serde_json::from_slice(payload)?, Encoding::Json)),
        Some(_) => Ok((rmp_serde::from_slice(payload)?, Encoding::MsgPack)),
    }
}

/// The length-delimited framing shared by every transport.
pub fn framing() -> tokio_util::codec::LengthDelimitedCodec {
    tokio_util::codec::LengthDelimitedCodec::builder()
        .length_field_type::<u32>()
        .big_endian()
        .max_frame_length(MAX_FRAME)
        .new_codec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::input::{KeyCode, KeyEvent, Mods};
    use ptyro_core::screen::{Color, RowUpdate, Run, Style, StyleDef};

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(m: T) {
        for enc in [Encoding::MsgPack, Encoding::Json] {
            let bytes = encode(&m, enc).unwrap();
            let (back, got): (T, _) = decode(&bytes).unwrap();
            assert_eq!(back, m);
            assert_eq!(got, enc);
        }
    }

    /// There is one scratchpad again (the per-project pads of 0.0.8/0.0.9 were dropped): a peer
    /// that still sends a `pad` selector, or reads one, is understood and acts on the global pad.
    #[test]
    fn a_leftover_pad_selector_is_ignored_and_means_the_global_pad() {
        let old = |json: &str| -> Request { serde_json::from_str(json).unwrap() };
        assert!(matches!(
            old(r#"{"t":"scratch_get","pad":{"project":"p1"}}"#),
            Request::ScratchGet
        ));
        assert!(matches!(
            old(r#"{"t":"scratch_history","pad":"global"}"#),
            Request::ScratchHistory
        ));
        assert!(matches!(
            old(r#"{"t":"scratch_add","pad":{"near":"/r"},"text":"x"}"#),
            Request::ScratchAdd { stamp: true, .. }
        ));
        assert!(matches!(
            old(r#"{"t":"scratch_todo","pad":"global"}"#),
            Request::ScratchTodo { all: false }
        ));
        // A document and an event from a 0.0.8 daemon carry `pad` and `name`; a new one has neither.
        let doc: PadDoc = serde_json::from_str(
            r#"{"pad":{"project":"p"},"name":"p","path":"/p","rev":"r","text":"t","open":0,"done":0}"#,
        )
        .unwrap();
        assert_eq!(doc.path, std::path::Path::new("/p"));
        let ev: DomainEvent = serde_json::from_str(
            r#"{"t":"scratch_changed","pad":{"project":"p"},"rev":"r","open":2}"#,
        )
        .unwrap();
        assert!(matches!(ev, DomainEvent::ScratchChanged { open: 2, .. }));
        let out = serde_json::to_value(&doc).unwrap();
        assert!(out.get("pad").is_none() && out.get("name").is_none(), "{out}");
    }

    #[test]
    fn client_messages_round_trip() {
        let id = SessionId::new();
        round_trip(ClientMsg::Hello {
            proto: PROTO,
            client: ClientInfo {
                kind: ClientKind::Tui,
                version: "0".into(),
                pid: 1,
                outer: Some(OuterTerm { fg: Some([1, 2, 3]), bg: None, kitty_keyboard: true }),
                token: Some("ptd_x".into()),
                via: Some("ws:100.64.0.7:51000".into()),
            },
            features: vec![],
        });
        round_trip(ClientMsg::Request {
            id: 8,
            req: Request::PushSubscribe {
                sub: PushSub {
                    endpoint: "https://push.example/x".into(),
                    p256dh: "k".into(),
                    auth: "a".into(),
                },
            },
        });
        round_trip(ClientMsg::Request {
            id: 9,
            req: Request::DevicePair { name: "phone".into(), scope: Scope::Approve },
        });
        round_trip(ClientMsg::Request {
            id: 10,
            req: Request::DeviceClaim { code: "ptp_x".into(), via: Some("ws:127.0.0.1".into()) },
        });
        round_trip(ClientMsg::Request {
            id: 7,
            req: Request::Spawn {
                spec: SpawnSpec {
                    argv: vec!["sh".into()],
                    cwd: "/".into(),
                    ..SpawnSpec::default()
                },
            },
        });
        round_trip(ClientMsg::Input {
            session: id,
            event: InputEvent::Key {
                key: KeyEvent::new(KeyCode::Char { c: 'x' }, Mods(Mods::CTRL)),
            },
        });
        round_trip(ClientMsg::Subscribe {
            session: id,
            size: Some(TermSize::new(80, 24)),
            control: true,
        });
    }

    #[test]
    fn server_messages_round_trip() {
        round_trip(ServerMsg::Screen {
            frame: ScreenFrame {
                session: SessionId::new(),
                seq: u64::MAX - 1,
                keyframe: true,
                delta: ScreenDelta {
                    size: Some(TermSize::new(10, 2)),
                    styles: vec![StyleDef {
                        id: 1,
                        style: Style { fg: Color::Indexed { i: 3 }, ..Style::default() },
                    }],
                    rows: vec![RowUpdate {
                        y: 0,
                        runs: vec![Run {
                            style: 1,
                            text: "é漢".into(),
                            wide: false,
                            clusters: Some(vec![1, 1]),
                        }],
                        wrapped: true,
                        prompt: true,
                    }],
                    title: Some(String::new()),
                    ..ScreenDelta::default()
                },
            },
        });
        round_trip(ServerMsg::Response {
            id: 1,
            outcome: Outcome::Err { error: Error::not_found("x") },
        });
    }

    #[test]
    fn compressed_frames_decode_like_plain_ones() {
        let big = ServerMsg::Event {
            event: DomainEvent::Notification { session: SessionId::new(), text: "x".repeat(5000) },
        };
        for enc in [Encoding::MsgPack, Encoding::Json] {
            let z = encode_compressed(&big, enc).unwrap();
            assert!(z.starts_with(&ZSTD_MAGIC) && z.len() < 1000, "{}", z.len());
            let (back, got): (ServerMsg, _) = decode(&z).unwrap();
            assert_eq!(back, big);
            assert_eq!(got, enc);
        }
        // Small ones stay plain.
        assert!(
            !encode_compressed(&ServerMsg::Pong, Encoding::MsgPack)
                .unwrap()
                .starts_with(&ZSTD_MAGIC)
        );
        // A bomb is refused, not allocated.
        let bomb = zstd::bulk::compress(&vec![b'{'; MAX_FRAME + 10], 1).unwrap();
        assert!(decode::<ServerMsg>(&bomb).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let json = br#"{"t":"ping","future_field":42}"#;
        let (m, _): (ClientMsg, _) = decode(json).unwrap();
        assert_eq!(m, ClientMsg::Ping);
    }
}
