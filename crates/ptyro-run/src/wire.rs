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
