//! What clients see of orchestration: the shapes the protocol carries (and `--json` prints, see
//! `docs/cli-json.md`). Plain data; the daemon fills them in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::state::{Gate, RunState};

/// A run, as listed and shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInfo {
    pub state: RunState,
    /// Steps in recipe order (the state's map is sorted by id).
    #[serde(default)]
    pub order: Vec<String>,
    /// Where the run keeps artifacts (`$PTYRO_RUN_DIR`).
    #[serde(default)]
    pub dir: String,
    /// Runs forked from this one.
    #[serde(default)]
    pub children: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `builtin`, `user` or `repo:<project>`.
    pub source: String,
    #[serde(default)]
    pub path: Option<String>,
    /// `name (required)` / `name = default`.
    #[serde(default)]
    pub vars: Vec<String>,
    pub steps: usize,
    /// Repo recipes need trust: set when this one isn't trusted (the fingerprint to approve).
    #[serde(default)]
    pub untrusted: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// An open or resolved gate, wherever it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInfo {
    pub run: String,
    pub step: String,
    pub gate: Gate,
}

/// `ptyro fanout`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanoutSpec {
    /// One candidate per entry (`claude`, `codex@work`); repeats are allowed.
    pub agents: Vec<String>,
    pub prompt: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    /// Worktree name prefix (default: from the prompt).
    #[serde(default)]
    pub name: Option<String>,
    /// Run this after each agent finishes (`scripts.run.test`, or a command).
    #[serde(default)]
    pub test: Option<String>,
    /// Verified-done for every candidate (`done_when`).
    #[serde(default)]
    pub verify: Option<String>,
    /// An advisory judge (another agent) scores the candidates.
    #[serde(default)]
    pub judge: Option<String>,
    #[serde(default)]
    pub timeout: Option<String>,
    /// Workers run as structured sessions (M10): full tool-call visibility.
    #[serde(default)]
    pub structured: bool,
}

/// What a structured candidate's tool calls add up to (M10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUse {
    pub calls: u32,
    pub by_tool: std::collections::BTreeMap<String, u32>,
    pub failed: u32,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    /// Exact, where the protocol reports it (Claude), in millionths of a dollar.
    #[serde(default)]
    pub cost_micro_usd: Option<u64>,
}

/// One candidate in the compare view (M7.3/M7.10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub step: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub worktree_name: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    pub status: String,
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    pub hunks: u32,
    /// The step's time, start to end (or now).
    pub duration_ms: u64,
    #[serde(default)]
    pub last_message: Option<String>,
    /// The test command's exit code here (`None`: not run).
    #[serde(default)]
    pub tests: Option<i32>,
    /// Tests pass on the base but not here.
    #[serde(default)]
    pub broke_tests: bool,
    /// The repro fails on the base and passes here.
    #[serde(default)]
    pub repro: Option<bool>,
    /// Other candidates' test files that pass here: `(passed, of)`.
    #[serde(default)]
    pub cross: Option<(u32, u32)>,
    /// Candidates with the same change (whitespace and comments aside) share a cluster.
    pub cluster: u32,
    #[serde(default)]
    pub verified: Option<bool>,
    /// The judge's 0–10, advisory.
    #[serde(default)]
    pub judge: Option<u8>,
    #[serde(default)]
    pub judge_note: Option<String>,
    /// 1 is best.
    pub rank: u32,
    /// Why it ranks where it does, best signal first.
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub picked: bool,
    #[serde(default)]
    pub discarded: bool,
    /// Its tool calls, for structured sessions (M10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolUse>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareDoc {
    pub run: String,
    #[serde(default)]
    pub base: Option<String>,
    /// The base's test exit code, when tests ran.
    #[serde(default)]
    pub base_tests: Option<i32>,
    pub candidates: Vec<Candidate>,
    /// `git diff --stat` between the two best candidates.
    #[serde(default)]
    pub top_difference: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickMode {
    /// Rebase onto the target and fast-forward it (default).
    #[default]
    Merge,
    CherryPick,
    /// Push and open a pull request.
    Pr,
    /// Copy only the chosen files or hunks into the target worktree, uncommitted.
    Apply,
}

/// A message as a recipient sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub run: Option<String>,
    pub at_ms: u64,
    /// A session name, `@user` or `@ptyro`.
    pub from: String,
    pub to: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    pub priority: String,
    /// `queued`, `delivered`, `seen` or `acked`.
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub holder: String,
    pub holder_name: String,
    pub pattern: String,
    pub exclusive: bool,
    #[serde(default)]
    pub reason: Option<String>,
    pub expires_ms: u64,
}

/// Predicted collisions between live worktrees of a project (M7.13).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Radar {
    pub project: String,
    /// The worktrees compared, with the files each changed against its base.
    pub worktrees: Vec<(String, Vec<String>)>,
    pub overlaps: Vec<Overlap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlap {
    pub a: String,
    pub b: String,
    /// Files both changed.
    pub files: Vec<String>,
    /// Files `git merge-tree` says would conflict.
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub n: u32,
    pub session: String,
    pub at_ms: u64,
    /// The agent turn it closed.
    pub turn: u32,
    /// The checkpoint commit (HEAD plus uncommitted work); `ref` names it.
    pub sha: String,
    #[serde(rename = "ref")]
    pub refname: String,
    pub head: String,
    pub files: u32,
}

/// What `ptyro mcp` asks for on a lead agent's behalf. Every op is checked against the run's
/// policy by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LeadOp {
    ListSessions,
    Spawn {
        agent: String,
        /// A worktree of this run (name), `new` (default) or `root`.
        #[serde(default)]
        worktree: Option<String>,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        name: Option<String>,
        prompt: String,
        #[serde(default)]
        key: Option<String>,
    },
    Send {
        session: String,
        text: String,
    },
    Read {
        session: String,
        #[serde(default)]
        scrollback: Option<u32>,
    },
    Wait {
        sessions: Vec<String>,
        /// `idle` (default), `blocked`, `exited` or `any_change`.
        #[serde(default)]
        until: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Diff {
        worktree: String,
    },
    Stop {
        session: String,
    },
    Gate {
        question: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Fanout {
        agents: Vec<String>,
        prompt: String,
        #[serde(default)]
        test: Option<String>,
    },
    Compare {
        run: String,
    },
    Policy,
}

/// The lead's answer to a [`LeadOp`].
pub type LeadResult = serde_json::Value;

/// One session seen by a lead agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub session: String,
    pub name: String,
    pub agent: Option<String>,
    pub status: String,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    pub turns: u32,
}

pub type Vars = BTreeMap<String, String>;

// ── change sets (M11) ───────────────────────────────────────────────────────

/// `ptyro cs new`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsNewSpec {
    pub name: String,
    /// Project addresses; with a template, extra members.
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub template: Option<String>,
    /// The lead's agent (default: the configured default agent).
    #[serde(default)]
    pub lead: Option<String>,
    /// `lead` (default), `solo`, `manual`.
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    /// A GitHub issue (number in the first member's repository, or a URL): the goal.
    #[serde(default)]
    pub from_issue: Option<String>,
    /// member → its dependencies (default: from the template, else suggested by manifests).
    #[serde(default)]
    pub depends: BTreeMap<String, Vec<String>>,
    /// member → agent.
    #[serde(default)]
    pub agents: BTreeMap<String, String>,
    /// member → base ref.
    #[serde(default)]
    pub bases: BTreeMap<String, String>,
    /// Where the human approves (default: the template's, else all three).
    #[serde(default)]
    pub gates: Option<Vec<String>>,
    #[serde(default)]
    pub integration: Vec<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// Turn on the overlays manifests suggest.
    #[serde(default)]
    pub overlay: bool,
    /// The client's SSH agent and display variables (for git and agents).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The fingerprint of a hub template the user just approved.
    #[serde(default)]
    pub trust: Option<String>,
}

/// What to do with a set's contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum ContractAction {
    /// A revision's text (default: the latest).
    Show {
        #[serde(default)]
        rev: Option<u32>,
    },
    History,
    /// Revision `a` → `b` (default: the last two); or the latest against a file (`file`, a path
    /// on the daemon's machine, e.g. a regenerated OpenAPI spec).
    Diff {
        #[serde(default)]
        a: Option<u32>,
        #[serde(default)]
        b: Option<u32>,
        #[serde(default)]
        file: Option<String>,
    },
    /// Record CONTRACT.md as a revision now; the first one goes to the contract gate.
    Submit {
        #[serde(default)]
        summary: Option<String>,
    },
    /// Replace the contract (`text`) or patch it (`diff`, unified), as a new revision.
    /// `affects` overrides the members it is for.
    Revise {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        diff: Option<String>,
        #[serde(default)]
        affects: Vec<String>,
        #[serde(default)]
        summary: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CsOp {
    New {
        spec: Box<CsNewSpec>,
    },
    List,
    Show {
        set: String,
    },
    /// The ledger: set events and messages, oldest first, after `since` (a `seq`).
    Log {
        set: String,
        #[serde(default)]
        since: Option<u64>,
    },
    /// Who the calling session is in which set (`ptyro mcp` asks).
    Whoami,
    Msg {
        set: String,
        to: String,
        text: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        interrupt: bool,
    },
    Ready {
        set: String,
        #[serde(default)]
        member: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    Blocked {
        set: String,
        on: String,
        #[serde(default)]
        member: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    /// The lead gives a member a task (an `assign` message).
    Assign {
        set: String,
        member: String,
        task: String,
    },
    Contract {
        set: String,
        action: ContractAction,
    },
    Diff {
        set: String,
        member: String,
    },
    /// Integration and contract checks.
    Check {
        set: String,
    },
    Checkpoint {
        set: String,
        #[serde(default)]
        label: Option<String>,
    },
    Restore {
        set: String,
        n: u32,
        #[serde(default)]
        force: bool,
    },
    Overlay {
        set: String,
        #[serde(default)]
        member: Option<String>,
        /// A kind (`go`…), `auto`, or `off`.
        mode: String,
    },
    Commit {
        set: String,
        message: String,
    },
    Push {
        set: String,
        #[serde(default)]
        yes: bool,
    },
    Pr {
        set: String,
        #[serde(default)]
        draft: bool,
    },
    Land {
        set: String,
        #[serde(default)]
        yes: bool,
    },
    Add {
        set: String,
        project: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        depends_on: Vec<String>,
    },
    Drop {
        set: String,
        member: String,
        #[serde(default)]
        force: bool,
    },
    Rm {
        set: String,
        #[serde(default)]
        keep: Vec<String>,
        #[serde(default)]
        force: bool,
    },
    /// The lead asks the human (a gate in the set's run).
    Gate {
        set: String,
        question: String,
        #[serde(default)]
        options: Vec<String>,
    },
    Templates,
}

/// A set, as shown: its state, what the daemon sees now, and where it stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetInfo {
    pub run: String,
    /// The run's status (`running`, `waiting`, `succeeded`, `cancelled`…).
    pub status: String,
    pub set: crate::changeset::ChangeSet,
    pub report: crate::changeset::Report,
    #[serde(default)]
    pub live: BTreeMap<String, crate::changeset::MemberLive>,
    /// The lead's session name.
    #[serde(default)]
    pub lead: Option<String>,
    /// member → its agent's session name.
    #[serde(default)]
    pub sessions: BTreeMap<String, String>,
    /// Open gates in the set (`contract`, `push`, `land:<member>`, the lead's questions).
    #[serde(default)]
    pub gates: Vec<GateInfo>,
    /// A push or land in progress.
    #[serde(default)]
    pub job: Option<String>,
}

/// One line of a set's ledger: a set event or a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub seq: u64,
    pub at_ms: u64,
    /// `event` or `message`.
    pub t: String,
    /// The event or message kind.
    pub kind: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub text: String,
}

/// A change-set template, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `user` or `hub:<project>`.
    pub source: String,
    pub path: String,
    pub members: Vec<String>,
    #[serde(default)]
    pub untrusted: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}
