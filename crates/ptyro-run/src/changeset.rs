//! Change sets (SPEC §15.5, M11): one coordinated change across several repositories, as data.
//!
//! A change set is a run of kind `changeset`. Its [`ChangeSet`] lives in the run's state and
//! changes only through [`CsEvent`]s (folded by [`RunState::apply`](crate::RunState::apply)), so
//! it is event-sourced and survives restarts like every run. The daemon
//! (`ptyro-daemon/src/changesets.rs`) does the I/O: worktrees, agents, git, checks. This module
//! decides, and stays pure:
//! - the set and its members, contract revisions, set checkpoints and the merge train;
//! - the dependency order (cycles are refused), and suggestions read from manifests;
//! - which members a contract revision affects (`affects:` lines);
//! - the set's state and its failure modes, each with a suggested action (§15.5.11);
//! - templates, the generated brief (`CHANGESET.md`), agents' prompts, the sibling-PR table and
//!   commit trailers;
//! - the integration overlay's generated files (`go.work`, cargo `[patch]`, compose overrides).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Where the human must approve (`gates = [...]`).
pub const GATES: [&str; 3] = ["contract", "push", "land"];
/// Integration overlays ptyro knows how to generate.
pub const OVERLAYS: [&str; 5] = ["go", "rust", "node", "python", "compose"];
/// Typed ledger messages (§15.5.4).
pub const KINDS: [&str; 10] = [
    "assign",
    "question",
    "answer",
    "contract_revised",
    "contract_change",
    "ready_for_integration",
    "blocked_on",
    "handoff",
    "heartbeat",
    "gate_request",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// A lead agent plus one agent per member (the default).
    #[default]
    Lead,
    /// One agent across every member.
    Solo,
    /// No agents: the linked worktrees and lockstep git only.
    Manual,
}

impl Layout {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Solo => "solo",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "lead" => Ok(Self::Lead),
            "solo" => Ok(Self::Solo),
            "manual" => Ok(Self::Manual),
            o => Err(format!("layout {o:?}: lead, solo or manual")),
        }
    }
}

/// The set's lifecycle; finer states are derived ([`ChangeSet::report`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The lead writes the contract (members aren't started yet).
    #[default]
    Drafting,
    /// Members work against the approved contract.
    Working,
    /// `cs land` is merging.
    Landing,
    /// Every member merged.
    Landed,
    /// `cs rm`: the worktrees are gone.
    Removed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Drafting => "drafting",
            Self::Working => "working",
            Self::Landing => "landing",
            Self::Landed => "landed",
            Self::Removed => "removed",
        }
    }
}

/// One repository of the set: a project of the workspace, with its own worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The project's slug: unique in the set, and the member's directory in the root.
    pub name: String,
    /// The project id.
    pub project: String,
    /// Its agent (`codex`, `claude@work`); default: the project's `default_agent`.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// `go`, `rust`, `node`, `python` or `compose`: build against siblings (opt-in).
    #[serde(default)]
    pub overlay: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    /// A command that checks this member against the contract (in its worktree).
    #[serde(default)]
    pub contract_check: Option<String>,
    // ── set by ptyro ──
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub branch: String,
    /// The commit its branch started from (a merge elsewhere is noticed against it).
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    /// The contract revision it last received.
    #[serde(default)]
    pub contract_rev: u32,
    /// When it said `ready_for_integration`, and against which revision.
    #[serde(default)]
    pub ready: Option<u64>,
    #[serde(default)]
    pub ready_rev: u32,
    #[serde(default)]
    pub blocked_on: Vec<String>,
    /// Its agent errored out, or a step on it failed (cleared by the next good turn).
    #[serde(default)]
    pub failed: Option<String>,
    /// A rebase or merge conflict.
    #[serde(default)]
    pub conflict: Option<String>,
    /// The last contract check.
    #[serde(default)]
    pub check: Option<CheckResult>,
    /// The last set-wide commit (`cs commit`).
    #[serde(default)]
    pub commit: Option<String>,
    /// What `cs push` pushed.
    #[serde(default)]
    pub pushed: Option<String>,
    #[serde(default)]
    pub pr: Option<u32>,
    #[serde(default)]
    pub pr_url: Option<String>,
    /// How it landed (`merged a1b2c3d`, `pr #12 merged`).
    #[serde(default)]
    pub landed: Option<String>,
    /// It merged before a dependency did.
    #[serde(default)]
    pub out_of_order: Option<String>,
    /// The repository has no remote to push to.
    #[serde(default)]
    pub no_remote: bool,
}

/// One saved version of `CONTRACT.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractRev {
    pub n: u32,
    pub at_ms: u64,
    /// `lead`, a member, `user`.
    pub by: String,
    pub sha: String,
    pub text: String,
    /// Against the revision before (unified).
    #[serde(default)]
    pub diff: String,
    /// Members the change is for: told at once (interrupt). Others hear at their next idle.
    #[serde(default)]
    pub affects: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub approved: Option<String>,
}

/// A consistent snapshot of the whole set (`refs/ptyro/cs/<set>/<n>` in every member).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetCheckpoint {
    pub n: u32,
    #[serde(default)]
    pub label: Option<String>,
    pub at_ms: u64,
    /// `manual`, `contract`, `land`, `restore`.
    pub reason: String,
    /// member → (HEAD, snapshot commit).
    pub members: BTreeMap<String, (String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub command: String,
    pub ok: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub tail: String,
    pub at_ms: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

/// `cs land`: the merge train's progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Train {
    pub order: Vec<String>,
    #[serde(default)]
    pub done: Vec<String>,
    /// Why it stopped, and at which member.
    #[serde(default)]
    pub stopped: Option<(String, String)>,
    pub started_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub name: String,
    /// `<data>/changesets/<name>`: CHANGESET.md, CONTRACT.md, the members, `.ptyro/`.
    pub root: String,
    pub layout: Layout,
    #[serde(default)]
    pub workspace: Option<String>,
    pub profile: String,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub lead_agent: Option<String>,
    #[serde(default)]
    pub gates: Vec<String>,
    /// Commands run in the root against the overlay.
    #[serde(default)]
    pub integration: Vec<String>,
    pub members: Vec<Member>,
    #[serde(default)]
    pub contract: Vec<ContractRev>,
    #[serde(default)]
    pub checkpoints: Vec<SetCheckpoint>,
    /// The last integration run.
    #[serde(default)]
    pub checks: Vec<CheckResult>,
    #[serde(default)]
    pub phase: Phase,
    #[serde(default)]
    pub train: Option<Train>,
}

/// What changed in a set. Wrapped in [`RunEvent::Cs`](crate::RunEvent::Cs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "c", rename_all = "snake_case")]
pub enum CsEvent {
    Created {
        set: Box<ChangeSet>,
    },
    MemberAdded {
        member: Box<Member>,
    },
    MemberDropped {
        name: String,
    },
    /// Facts learned about a member (any field may be missing).
    MemberInfo {
        name: String,
        #[serde(default)]
        worktree: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        overlay: Option<String>,
        #[serde(default)]
        start: Option<String>,
    },
    Contract {
        rev: Box<ContractRev>,
    },
    ContractApproved {
        n: u32,
        by: String,
    },
    /// A revision reached a member.
    Delivered {
        name: String,
        rev: u32,
    },
    Ready {
        name: String,
        at: u64,
        rev: u32,
    },
    Unready {
        name: String,
    },
    BlockedOn {
        name: String,
        on: String,
    },
    Unblocked {
        name: String,
        on: String,
    },
    Failed {
        name: String,
        #[serde(default)]
        error: Option<String>,
    },
    Conflict {
        name: String,
        #[serde(default)]
        detail: Option<String>,
    },
    /// A member's contract check (`name`), or the integration checks (`None`).
    Checked {
        #[serde(default)]
        name: Option<String>,
        results: Vec<CheckResult>,
    },
    Checkpoint {
        ckpt: Box<SetCheckpoint>,
    },
    Committed {
        name: String,
        sha: String,
    },
    Pushed {
        name: String,
        sha: String,
    },
    NoRemote {
        name: String,
        on: bool,
    },
    Pr {
        name: String,
        number: u32,
        url: String,
    },
    Landed {
        name: String,
        how: String,
    },
    OutOfOrder {
        name: String,
        #[serde(default)]
        detail: Option<String>,
    },
    Phase {
        phase: Phase,
    },
    Train {
        #[serde(default)]
        train: Option<Train>,
    },
}

impl CsEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::MemberAdded { .. } => "member_added",
            Self::MemberDropped { .. } => "member_dropped",
            Self::MemberInfo { .. } => "member_info",
            Self::Contract { .. } => "contract",
            Self::ContractApproved { .. } => "contract_approved",
            Self::Delivered { .. } => "delivered",
            Self::Ready { .. } => "ready",
            Self::Unready { .. } => "unready",
            Self::BlockedOn { .. } => "blocked_on",
            Self::Unblocked { .. } => "unblocked",
            Self::Failed { .. } => "failed",
            Self::Conflict { .. } => "conflict",
            Self::Checked { .. } => "checked",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Committed { .. } => "committed",
            Self::Pushed { .. } => "pushed",
            Self::NoRemote { .. } => "no_remote",
            Self::Pr { .. } => "pr",
            Self::Landed { .. } => "landed",
            Self::OutOfOrder { .. } => "out_of_order",
            Self::Phase { .. } => "phase",
            Self::Train { .. } => "train",
        }
    }
}

impl ChangeSet {
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    fn member_mut(&mut self, name: &str) -> Option<&mut Member> {
        self.members.iter_mut().find(|m| m.name == name)
    }

    /// The latest revision, and the latest approved one.
    pub fn contract_head(&self) -> Option<&ContractRev> {
        self.contract.last()
    }

    pub fn approved(&self) -> Option<&ContractRev> {
        self.contract.iter().rev().find(|r| r.approved.is_some())
    }

    pub fn gated(&self, gate: &str) -> bool {
        self.gates.iter().any(|g| g == gate)
    }

    /// Fold one event (see [`RunState::apply`](crate::RunState::apply)).
    pub fn apply(&mut self, e: &CsEvent) {
        match e {
            CsEvent::Created { set } => *self = (**set).clone(),
            CsEvent::MemberAdded { member } => {
                if self.member(&member.name).is_none() {
                    self.members.push((**member).clone());
                }
            }
            CsEvent::MemberDropped { name } => {
                self.members.retain(|m| &m.name != name);
                for m in &mut self.members {
                    m.depends_on.retain(|d| d != name);
                    m.blocked_on.retain(|d| d != name);
                }
            }
            CsEvent::MemberInfo { name, worktree, path, branch, session, overlay, start } => {
                if let Some(m) = self.member_mut(name) {
                    if start.is_some() {
                        m.start = start.clone();
                    }
                    if worktree.is_some() {
                        m.worktree = worktree.clone();
                    }
                    if let Some(p) = path {
                        m.path = p.clone();
                    }
                    if let Some(b) = branch {
                        m.branch = b.clone();
                    }
                    if session.is_some() {
                        m.session = session.clone();
                    }
                    if let Some(o) = overlay {
                        m.overlay = (!o.is_empty()).then(|| o.clone());
                    }
                }
            }
            CsEvent::Contract { rev } => self.contract.push((**rev).clone()),
            CsEvent::ContractApproved { n, by } => {
                if let Some(r) = self.contract.iter_mut().find(|r| r.n == *n) {
                    r.approved = Some(by.clone());
                }
                if self.phase == Phase::Drafting {
                    self.phase = Phase::Working;
                }
            }
            CsEvent::Delivered { name, rev } => {
                if let Some(m) = self.member_mut(name) {
                    m.contract_rev = m.contract_rev.max(*rev);
                }
            }
            CsEvent::Ready { name, at, rev } => {
                if let Some(m) = self.member_mut(name) {
                    m.ready = Some(*at);
                    m.ready_rev = *rev;
                    m.failed = None;
                }
            }
            CsEvent::Unready { name } => {
                if let Some(m) = self.member_mut(name) {
                    m.ready = None;
                }
            }
            CsEvent::BlockedOn { name, on } => {
                if let Some(m) = self.member_mut(name)
                    && !m.blocked_on.contains(on)
                {
                    m.blocked_on.push(on.clone());
                }
            }
            CsEvent::Unblocked { name, on } => {
                if let Some(m) = self.member_mut(name) {
                    m.blocked_on.retain(|x| x != on);
                }
            }
            CsEvent::Failed { name, error } => {
                if let Some(m) = self.member_mut(name) {
                    m.failed = error.clone();
                }
            }
            CsEvent::Conflict { name, detail } => {
                if let Some(m) = self.member_mut(name) {
                    m.conflict = detail.clone();
                }
            }
            CsEvent::Checked { name: Some(n), results } => {
                if let Some(m) = self.member_mut(n) {
                    m.check = results.last().cloned();
                }
            }
            CsEvent::Checked { name: None, results } => self.checks = results.clone(),
            CsEvent::Checkpoint { ckpt } => self.checkpoints.push((**ckpt).clone()),
            CsEvent::Committed { name, sha } => {
                if let Some(m) = self.member_mut(name) {
                    m.commit = Some(sha.clone());
                }
            }
            CsEvent::Pushed { name, sha } => {
                if let Some(m) = self.member_mut(name) {
                    m.pushed = Some(sha.clone());
                    m.no_remote = false;
                }
            }
            CsEvent::NoRemote { name, on } => {
                if let Some(m) = self.member_mut(name) {
                    m.no_remote = *on;
                }
            }
            CsEvent::Pr { name, number, url } => {
                if let Some(m) = self.member_mut(name) {
                    m.pr = Some(*number);
                    m.pr_url = Some(url.clone());
                }
            }
            CsEvent::Landed { name, how } => {
                if let Some(m) = self.member_mut(name) {
                    m.landed = Some(how.clone());
                    m.conflict = None;
                }
                if let Some(t) = &mut self.train
                    && !t.done.contains(name)
                {
                    t.done.push(name.clone());
                }
            }
            CsEvent::OutOfOrder { name, detail } => {
                if let Some(m) = self.member_mut(name) {
                    m.out_of_order = detail.clone();
                }
            }
            CsEvent::Phase { phase } => self.phase = *phase,
            CsEvent::Train { train } => self.train = train.clone(),
        }
    }

    /// Members in dependency order (dependencies first; ties by name).
    pub fn order(&self) -> Result<Vec<String>, Vec<String>> {
        let deps: BTreeMap<String, Vec<String>> =
            self.members.iter().map(|m| (m.name.clone(), m.depends_on.clone())).collect();
        topo(&deps)
    }

    /// Members affected by a change to `name` (it and everything that depends on it).
    pub fn dependents(&self, name: &str) -> Vec<String> {
        let mut out = BTreeSet::new();
        let mut todo = vec![name.to_owned()];
        while let Some(n) = todo.pop() {
            for m in &self.members {
                if m.depends_on.contains(&n) && out.insert(m.name.clone()) {
                    todo.push(m.name.clone());
                }
            }
        }
        out.into_iter().collect()
    }
}

/// Topological order, or the cycle that prevents one.
pub fn topo(deps: &BTreeMap<String, Vec<String>>) -> Result<Vec<String>, Vec<String>> {
    let mut done: Vec<String> = Vec::new();
    let mut state: BTreeMap<&str, u8> = BTreeMap::new(); // 1 visiting, 2 done
    fn visit<'a>(
        n: &'a str,
        deps: &'a BTreeMap<String, Vec<String>>,
        state: &mut BTreeMap<&'a str, u8>,
        path: &mut Vec<&'a str>,
        done: &mut Vec<String>,
    ) -> Result<(), Vec<String>> {
        match state.get(n) {
            Some(2) => return Ok(()),
            Some(1) => {
                let at = path.iter().position(|p| *p == n).unwrap_or(0);
                let mut cycle: Vec<String> = path[at..].iter().map(|s| (*s).to_owned()).collect();
                cycle.push(n.to_owned());
                return Err(cycle);
            }
            _ => {}
        }
        state.insert(n, 1);
        path.push(n);
        for d in deps.get(n).map(Vec::as_slice).unwrap_or_default() {
            if deps.contains_key(d) {
                visit(d, deps, state, path, done)?;
            }
        }
        path.pop();
        state.insert(n, 2);
        done.push(n.to_owned());
        Ok(())
    }
    for n in deps.keys() {
        visit(n, deps, &mut state, &mut Vec::new(), &mut done)?;
    }
    Ok(done)
}

// ── state and failure modes (§15.5.11) ─────────────────────────────────────

/// What the daemon sees of a member right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLive {
    /// `working`, `idle`, `blocked`, `done`, `error`, `exited`, or nothing (no agent).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    /// Commits on its branch since the base.
    #[serde(default)]
    pub ahead: u32,
    /// Changed files outside its fence (agents without hooks).
    #[serde(default)]
    pub out_of_fence: Vec<String>,
    /// The worktree is missing.
    #[serde(default)]
    pub missing: bool,
}

/// One thing wrong (or to do), with the action that fixes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// `member_failed`, `member_conflict`, `stale_contract`, `out_of_order`, `no_remote`,
    /// `blocked_on`, `out_of_fence`, `check_failed`, `integration_failed`, `missing_worktree`,
    /// `land_stopped`.
    pub state: String,
    #[serde(default)]
    pub member: Option<String>,
    pub detail: String,
    pub action: String,
}

/// The set at a glance: a state word, every issue, and per-member states.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The most pressing issue's state, else the phase's (`drafting`, `contract_review`,
    /// `working`, `ready`, `landing`, `landed`, `removed`).
    pub state: String,
    pub issues: Vec<Issue>,
    /// member → `working`, `idle`, `blocked_on:api`, `ready`, `landed`, `failed`…
    pub members: BTreeMap<String, String>,
    /// `3/4 ready`.
    pub ready: (usize, usize),
}

/// Most pressing first.
const SEVERITY: [&str; 12] = [
    "land_stopped",
    "member_conflict",
    "member_failed",
    "out_of_order",
    "missing_worktree",
    "integration_failed",
    "check_failed",
    "stale_contract",
    "out_of_fence",
    "no_remote",
    "blocked_on",
    "unchecked",
];

impl ChangeSet {
    /// Where the set stands: derived from its facts and what the daemon sees now. Nothing here
    /// says "done" unless every member is.
    pub fn report(&self, live: &BTreeMap<String, MemberLive>, contract_gate_open: bool) -> Report {
        let head = self.contract_head().map_or(0, |r| r.n);
        let mut issues = Vec::new();
        let mut members = BTreeMap::new();
        let issue = |state: &str, m: Option<&str>, detail: String, action: String| Issue {
            state: state.into(),
            member: m.map(str::to_owned),
            detail,
            action,
        };
        let set = &self.name;
        for m in &self.members {
            let l = live.get(&m.name).cloned().unwrap_or_default();
            let n = m.name.as_str();
            let mut word = l.status.clone().unwrap_or_else(|| "no agent".into());
            if l.missing {
                issues.push(issue(
                    "missing_worktree",
                    Some(n),
                    format!("{n}'s worktree is gone ({})", m.path),
                    format!("ptyro cs drop {set} {n}, then ptyro cs add {set} {n}"),
                ));
                word = "missing".into();
            }
            if let Some(c) = &m.conflict {
                issues.push(issue(
                    "member_conflict",
                    Some(n),
                    format!("{n}: {c}"),
                    format!(
                        "resolve it in {} (or ask its agent: ptyro cs msg {set} {n} \"…\"), commit, then ptyro cs land {set} again",
                        m.path
                    ),
                ));
                word = "conflict".into();
            } else if let Some(f) = &m.failed {
                issues.push(issue(
                    "member_failed",
                    Some(n),
                    format!("{n}: {f}"),
                    format!("look at it (ptyro attach {set}-{n}), then ptyro resume it or ptyro cs msg {set} {n} \"…\""),
                ));
                word = "failed".into();
            }
            if let Some(o) = &m.out_of_order {
                issues.push(issue(
                    "out_of_order",
                    Some(n),
                    format!("{n} {o}"),
                    format!(
                        "land its dependencies now (ptyro cs land {set}) or revert the merge; check that the base still builds"
                    ),
                ));
            }
            if self.phase == Phase::Working
                && m.ready.is_some()
                && m.ready_rev < head
                && self.contract.iter().any(|r| r.n > m.ready_rev && r.affects_member(n))
            {
                issues.push(issue(
                    "stale_contract",
                    Some(n),
                    format!("{n} said ready against r{}, the contract is at r{head}", m.ready_rev),
                    format!("ptyro cs msg {set} {n} \"re-check against r{head}\" (ptyro cs contract {set} diff shows what changed)"),
                ));
                word = "stale".into();
            } else if m.ready.is_some() {
                word = "ready".into();
            }
            if let Some(c) = m.check.as_ref().filter(|c| !c.ok) {
                issues.push(issue(
                    "check_failed",
                    Some(n),
                    format!("{n}'s contract check failed: {} (exit {:?})", c.command, c.exit_code),
                    format!("ptyro cs check {set} after a fix; the output tail is in ptyro cs status {set} --json"),
                ));
            }
            if !l.out_of_fence.is_empty() {
                issues.push(issue(
                    "out_of_fence",
                    Some(n),
                    format!(
                        "{n} changed files outside its fence: {}",
                        l.out_of_fence.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                    ),
                    "revert them, or move the change to the member that owns it".into(),
                ));
            }
            if m.no_remote {
                issues.push(issue(
                    "no_remote",
                    Some(n),
                    format!("{n}'s repository has no remote"),
                    format!("git -C {} remote add origin <url>, then ptyro cs push {set}", m.path),
                ));
            }
            if !m.blocked_on.is_empty() {
                issues.push(issue(
                    "blocked_on",
                    Some(n),
                    format!("{n} is blocked on {}", m.blocked_on.join(", ")),
                    format!(
                        "it wakes when {} posts ready_for_integration; or answer it: ptyro cs msg {set} {n} \"…\"",
                        m.blocked_on.join(", ")
                    ),
                ));
                word = format!("blocked_on:{}", m.blocked_on.join(","));
            }
            if m.landed.is_some() {
                word = "landed".into();
            }
            members.insert(m.name.clone(), word);
        }
        if let Some(c) = self.checks.iter().find(|c| !c.ok) {
            issues.push(issue(
                "integration_failed",
                None,
                format!("integration check failed: {} (exit {:?})", c.command, c.exit_code),
                format!("fix it, then ptyro cs check {set}"),
            ));
        }
        if let Some(t) = &self.train
            && let Some((m, why)) = &t.stopped
        {
            issues.push(issue(
                "land_stopped",
                Some(m),
                format!(
                    "the merge train stopped at {m}: {why}. Landed: {}; not landed: {}",
                    if t.done.is_empty() { "none".into() } else { t.done.join(", ") },
                    t.order
                        .iter()
                        .filter(|x| !t.done.contains(x))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!("fix {m}, then ptyro cs land {set} resumes from it"),
            ));
        }
        issues.sort_by_key(|i| SEVERITY.iter().position(|s| *s == i.state).unwrap_or(99));
        let total = self.members.len();
        let ready = self.members.iter().filter(|m| m.ready.is_some() || m.landed.is_some()).count();
        let base = match self.phase {
            Phase::Drafting if contract_gate_open => "contract_review",
            Phase::Drafting => "drafting",
            Phase::Working if total > 0 && ready == total => "ready",
            Phase::Working => "working",
            Phase::Landing => "landing",
            Phase::Landed => "landed",
            Phase::Removed => "removed",
        };
        // Blocked members and missing remotes are things to know, not the set's state.
        let state = issues
            .iter()
            .find(|i| !matches!(i.state.as_str(), "blocked_on" | "no_remote" | "out_of_fence"))
            .filter(|_| !matches!(self.phase, Phase::Landed | Phase::Removed))
            .map_or_else(|| base.to_owned(), |i| i.state.clone());
        Report { state, issues, members, ready: (ready, total) }
    }
}

// ── the contract ───────────────────────────────────────────────────────────

/// A `##` section of CONTRACT.md, with the members its `affects:` line names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub body: String,
    /// `None`: no `affects:` line (it concerns everyone).
    pub affects: Option<Vec<String>>,
}

/// Split a contract into its `##` sections (text before the first is section "").
pub fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut cur = Section { title: String::new(), body: String::new(), affects: None };
    for line in text.lines() {
        if let Some(t) = line.strip_prefix("## ") {
            out.push(std::mem::replace(
                &mut cur,
                Section { title: t.trim().to_owned(), body: String::new(), affects: None },
            ));
            continue;
        }
        let l = line.trim().trim_start_matches(['-', '*', '>', ' ']).trim();
        if let Some(rest) =
            l.strip_prefix("affects:").or_else(|| l.strip_prefix("**affects:**")).map(str::trim)
        {
            let v: Vec<String> = rest
                .split([',', ' '])
                .map(|s| s.trim().trim_matches(['`', '*', '@']).to_owned())
                .filter(|s| !s.is_empty() && s != "—" && s != "-")
                .collect();
            cur.affects.get_or_insert_with(Vec::new).extend(v);
        }
        cur.body.push_str(line);
        cur.body.push('\n');
    }
    out.push(cur);
    out.retain(|s| !(s.title.is_empty() && s.body.trim().is_empty()));
    out
}

/// Who a revision from `old` to `new` concerns: `(affected, everyone_else)`. Members named by
/// a changed section's `affects:` are affected; a changed section without one affects all.
pub fn affected(old: &str, new: &str, members: &[String]) -> (Vec<String>, Vec<String>) {
    let (a, b) = (sections(old), sections(new));
    let mut hit: BTreeSet<String> = BTreeSet::new();
    let mut all = false;
    let find = |v: &[Section], t: &str| v.iter().find(|s| s.title == t).cloned();
    let titles: BTreeSet<String> = a.iter().chain(&b).map(|s| s.title.clone()).collect();
    for t in titles {
        let (x, y) = (find(&a, &t), find(&b, &t));
        if x.as_ref().map(|s| s.body.trim()) == y.as_ref().map(|s| s.body.trim()) {
            continue;
        }
        let aff: Option<Vec<String>> = match (x.and_then(|s| s.affects), y.and_then(|s| s.affects))
        {
            (None, None) => None,
            (p, q) => Some(p.into_iter().chain(q).flatten().collect()),
        };
        // No `affects:` line: the members the title names (M11.0: leads title sections
        // "api (Go)" or "Wire format (api -> everyone)" on their own). `affects: everyone`
        // (or `all`) means all; names match members ignoring case.
        let aff = aff.filter(|v| !v.iter().any(|a| means_everyone(a)));
        match aff.or_else(|| title_affects(&t, members)) {
            Some(v) => hit.extend(
                members.iter().filter(|m| v.iter().any(|a| a.eq_ignore_ascii_case(m))).cloned(),
            ),
            None => all = true,
        }
    }
    if all {
        return (members.to_vec(), Vec::new());
    }
    let rest = members.iter().filter(|m| !hit.contains(*m)).cloned().collect();
    (hit.into_iter().collect(), rest)
}

fn means_everyone(w: &str) -> bool {
    ["everyone", "all", "everybody"].iter().any(|e| w.eq_ignore_ascii_case(e))
}

/// Members a section title names, unless it says it is for everyone (`None`: everyone).
fn title_affects(title: &str, members: &[String]) -> Option<Vec<String>> {
    let words: Vec<String> = title
        .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.iter().any(|w| means_everyone(w) || w == "shared") {
        return None;
    }
    let named: Vec<String> =
        members.iter().filter(|m| words.contains(&m.to_ascii_lowercase())).cloned().collect();
    (!named.is_empty()).then_some(named)
}

impl ContractRev {
    /// Does this revision concern `member` (named in `affects`, ignoring case, or affecting
    /// all: `affects` empty or naming `everyone`/`all`)?
    pub fn affects_member(&self, member: &str) -> bool {
        self.affects.is_empty()
            || self.affects.iter().any(|a| a.eq_ignore_ascii_case(member) || means_everyone(a))
    }
}

/// The skeleton a lead fills in.
pub fn contract_skeleton(set: &ChangeSet) -> String {
    let order = set.order().unwrap_or_default();
    let mut t = format!(
        "# Contract: {}\n\n<!-- Written by the lead before any member writes code. Each `##` section may name the\n     members it concerns with an `affects:` line; a revision tells those members at once. -->\n\n",
        set.name
    );
    if let Some(g) = &set.goal {
        t.push_str(&format!("Goal: {g}\n\n"));
    }
    for m in &set.members {
        let others = set.dependents(&m.name);
        t.push_str(&format!(
            "## {} interface\n\naffects: {}\n\nTODO: what {} provides to its siblings (endpoints and payloads, types, events, versions).\n\n",
            m.name,
            std::iter::once(m.name.as_str())
                .chain(others.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(", "),
            m.name
        ));
    }
    t.push_str(&format!(
        "## Merge and deploy order\n\n{}\n",
        order
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}. {m}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    ));
    t
}

// ── briefs and prompts ─────────────────────────────────────────────────────

/// `CHANGESET.md`: the generated brief every agent of the set reads.
pub fn brief(set: &ChangeSet) -> String {
    let order =
        set.order().unwrap_or_else(|_| set.members.iter().map(|m| m.name.clone()).collect());
    let mut t = format!("# Change set: {}\n\n", set.name);
    t.push_str("<!-- Generated by ptyro; it is rewritten when the set changes. -->\n\n");
    if let Some(g) = &set.goal {
        t.push_str(&format!("**Goal:** {g}\n\n"));
    }
    t.push_str(&format!(
        "Layout `{}`, profile `{}`. Every member is a git worktree on its own branch, next to this file.\n\n",
        set.layout.label(),
        set.profile
    ));
    t.push_str("| member | directory | branch | agent | depends on | overlay |\n|---|---|---|---|---|---|\n");
    for m in &set.members {
        t.push_str(&format!(
            "| {} | ./{} | {} | {} | {} | {} |\n",
            m.name,
            m.name,
            m.branch,
            m.agent.as_deref().unwrap_or("—"),
            if m.depends_on.is_empty() { "—".into() } else { m.depends_on.join(", ") },
            m.overlay.as_deref().unwrap_or("—"),
        ));
    }
    t.push_str(&format!("\n**Merge order:** {}\n\n", order.join(" → ")));
    t.push_str("## Roles\n\n");
    t.push_str("- **Lead:** reads every member; owns CONTRACT.md and this file; never edits member code.\n");
    t.push_str(
        "- **Member:** edits only its own directory; reads its siblings; talks to the lead.\n",
    );
    t.push_str("- **Human:** approves the contract and the ship train (push, land).\n\n");
    t.push_str("Edits outside your directory are denied by ptyro. Push, PRs and merges go through ptyro\n(`ptyro cs push|pr|land`), never by hand.\n\n");
    t.push_str("## Talking\n\n");
    t.push_str(&format!(
        "- `ptyro cs msg {s} @lead \"…\"` (or a member's name, `@all`); `--kind question|answer|handoff|contract_change`\n- `ptyro cs ready {s}` when your part works against the contract (wakes members blocked on you)\n- `ptyro cs blocked {s} <member> \"why\"` when you need a sibling first\n- `ptyro cs log {s}` is the set's memory: who decided what, and why\n\nMessages arrive between your turns, never in the middle of one.\n",
        s = set.name
    ));
    if !set.integration.is_empty() {
        t.push_str(&format!(
            "\n## Integration checks\n\nRun in this directory by `ptyro cs check {}`:\n\n{}\n",
            set.name,
            set.integration.iter().map(|c| format!("- `{c}`")).collect::<Vec<_>>().join("\n")
        ));
    }
    t
}

/// The lead's first prompt.
pub fn lead_prompt(set: &ChangeSet) -> String {
    let approve = if set.gated("contract") {
        "The human approves the first revision before any member starts."
    } else {
        "Members start as soon as the first revision is saved."
    };
    format!(
        "You are the lead of change set {name}: one change across {n} repositories ({members}). Read CHANGESET.md (the brief) and the members' code (each is a directory here; you may read but not edit them).\n\nYour first job is the contract: rewrite CONTRACT.md with the interfaces between the members (endpoints and payloads, types, events, versions, flags) and the merge order. Give each `##` section an `affects:` line naming the members it concerns. {approve} When it is ready, run `ptyro cs contract {name} submit`.\n\nThen ptyro starts one agent per member with its part of the contract. Coordinate them through the set's tools (cs_status, cs_send, cs_contract_revise, cs_member_diff, cs_integration_check, cs_checkpoint) or the CLI (`ptyro cs status|msg|check {name}`). Revise the contract when a seam changes: the members it affects hear at once. You never push or merge: shipping is the human's train.{goal}",
        name = set.name,
        n = set.members.len(),
        members = set.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "),
        goal = set.goal.as_ref().map(|g| format!("\n\nThe goal: {g}")).unwrap_or_default(),
    )
}

/// A member agent's first prompt: its part, the approved contract, the rules.
pub fn member_prompt(set: &ChangeSet, m: &Member, contract: Option<&ContractRev>) -> String {
    let siblings: Vec<String> =
        set.members.iter().filter(|x| x.name != m.name).map(|x| format!("../{}", x.name)).collect();
    let mut t = format!(
        "You are the {name} member of change set {set}: one change across {n} repositories. You work in this directory only (branch {branch}); your siblings are readable at {sib}. Edits outside this directory are denied.\n",
        name = m.name,
        set = set.name,
        n = set.members.len(),
        branch = m.branch,
        sib = if siblings.is_empty() { "—".into() } else { siblings.join(", ") },
    );
    if let Some(g) = &set.goal {
        t.push_str(&format!("\nThe goal: {g}\n"));
    }
    if !m.depends_on.is_empty() {
        t.push_str(&format!("\nYou depend on: {}.", m.depends_on.join(", ")));
    }
    if let Some(c) = contract {
        t.push_str(&format!(
            "\n\nThe contract (r{}, approved) — implement your side of it exactly:\n\n{}\n",
            c.n,
            c.text.trim()
        ));
    }
    t.push_str(&format!(
        "\nCommit your work in this repository when its tests pass. When your part works against the contract, run `ptyro cs ready {s}`. If you need a sibling first: `ptyro cs blocked {s} <member> \"why\"` (you're woken when it's ready). Questions or contract problems go to the lead: `ptyro cs msg {s} @lead \"…\"` (`--kind contract_change` to ask for a change). Never push.",
        s = set.name
    ));
    t
}

/// What a member hears when the contract changes.
pub fn revision_message(rev: &ContractRev) -> String {
    format!(
        "The contract is now r{}{}. What changed:\n\n```diff\n{}\n```\n\nCheck your side against it (CONTRACT.md in the change set's root, `ptyro cs contract show`).",
        rev.n,
        rev.summary.as_ref().map(|s| format!(" ({s})")).unwrap_or_default(),
        rev.diff.trim()
    )
}

/// Trailers for a set-wide commit: the members committed before this one (its dependencies
/// first), so each commit names the sibling commits it was built against.
pub fn trailers(set: &str, before: &[(String, String)]) -> String {
    let mut t = format!("Change-Set: {set}");
    if !before.is_empty() {
        t.push_str(&format!(
            "\nChange-Set-Members: {}",
            before
                .iter()
                .map(|(m, sha)| format!("{m}@{}", &sha[..sha.len().min(7)]))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    t
}

const TABLE_START: &str = "<!-- ptyro:changeset -->";
const TABLE_END: &str = "<!-- /ptyro:changeset -->";

/// The "change set" table each member's PR carries: siblings, their PRs, CI and merge order.
/// `ci`: member → `passing`, `failing`, `pending`, `none`.
pub fn pr_table(set: &ChangeSet, current: &str, ci: &BTreeMap<String, String>) -> String {
    let order = set.order().unwrap_or_default();
    let mut t = format!(
        "{TABLE_START}\n**Change set `{}`** — one change across {} repositories; merge in this order:\n\n| # | member | PR | CI | state |\n|---|---|---|---|---|\n",
        set.name,
        set.members.len()
    );
    for (i, name) in order.iter().enumerate() {
        let Some(m) = set.member(name) else { continue };
        let pr = match (&m.pr_url, m.pr) {
            (Some(u), Some(n)) => format!("[#{n}]({u})"),
            _ => "—".into(),
        };
        t.push_str(&format!(
            "| {} | {}{} | {pr} | {} | {} |\n",
            i + 1,
            name,
            if name == current { " (this PR)" } else { "" },
            ci.get(name).map_or("—", String::as_str),
            if m.landed.is_some() { "merged" } else { "open" },
        ));
    }
    t.push_str(&format!(
        "\nMerged by `ptyro cs land`, which waits for each member's checks.\n{TABLE_END}"
    ));
    t
}

/// Put `table` into a PR body, replacing an earlier one.
pub fn with_table(body: &str, table: &str) -> String {
    match (body.find(TABLE_START), body.find(TABLE_END)) {
        (Some(a), Some(b)) if b > a => {
            format!("{}{table}{}", &body[..a], &body[b + TABLE_END.len()..])
        }
        _ if body.trim().is_empty() => table.to_owned(),
        _ => format!("{}\n\n{table}", body.trim_end()),
    }
}

// ── manifests: dependency suggestions and overlay detection ────────────────

/// What a repository's manifests say: the names it provides and the names it requires, per
/// ecosystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// `(ecosystem, name)`: `("go", "example.com/api")`, `("node", "@acme/sdk")`.
    pub provides: Vec<(String, String)>,
    pub requires: Vec<(String, String)>,
    /// Ecosystems found (`go`, `rust`, `node`, `python`, `compose`).
    pub kinds: Vec<String>,
    /// compose service names.
    pub services: Vec<String>,
}

/// Read the manifests at a repository's root: `files` maps a file name (`go.mod`,
/// `package.json`, `Cargo.toml`, `pyproject.toml`, `compose.yml`…) to its text.
pub fn manifest(files: &BTreeMap<String, String>) -> Manifest {
    let mut m = Manifest::default();
    if let Some(t) = files.get("go.mod") {
        m.kinds.push("go".into());
        let mut in_req = false;
        for l in t.lines() {
            let l = l.split("//").next().unwrap_or("").trim();
            if let Some(name) = l.strip_prefix("module ") {
                m.provides.push(("go".into(), name.trim().to_owned()));
            } else if l.starts_with("require (") {
                in_req = true;
            } else if in_req && l == ")" {
                in_req = false;
            } else if let Some(r) = l.strip_prefix("require ").or(in_req.then_some(l))
                && let Some(name) = r.split_whitespace().next()
            {
                m.requires.push(("go".into(), name.to_owned()));
            }
        }
    }
    if let Some(v) =
        files.get("package.json").and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
    {
        m.kinds.push("node".into());
        if let Some(n) = v["name"].as_str() {
            m.provides.push(("node".into(), n.to_owned()));
        }
        for k in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
            if let Some(o) = v[k].as_object() {
                m.requires.extend(o.keys().map(|d| ("node".to_owned(), d.clone())));
            }
        }
    }
    if let Some(v) = files.get("Cargo.toml").and_then(|t| t.parse::<toml::Table>().ok()) {
        m.kinds.push("rust".into());
        if let Some(n) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            m.provides.push(("rust".into(), n.to_owned()));
        }
        let mut deps = |t: Option<&toml::Value>| {
            if let Some(t) = t.and_then(toml::Value::as_table) {
                for (k, v) in t {
                    let name = v.get("package").and_then(|p| p.as_str()).unwrap_or(k);
                    m.requires.push(("rust".into(), name.to_owned()));
                }
            }
        };
        for k in ["dependencies", "dev-dependencies", "build-dependencies"] {
            deps(v.get(k));
        }
        if let Some(ws) = v.get("workspace") {
            deps(ws.get("dependencies"));
        }
    }
    if let Some(v) = files.get("pyproject.toml").and_then(|t| t.parse::<toml::Table>().ok()) {
        m.kinds.push("python".into());
        let project = v.get("project");
        if let Some(n) = project.and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
            m.provides.push(("python".into(), py_norm(n)));
        }
        if let Some(a) = project.and_then(|p| p.get("dependencies")).and_then(|d| d.as_array()) {
            for d in a.iter().filter_map(|d| d.as_str()) {
                let name: String = d
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    .collect();
                m.requires.push(("python".into(), py_norm(&name)));
            }
        }
    }
    for f in ["compose.yml", "compose.yaml", "docker-compose.yml", "docker-compose.yaml"] {
        if let Some(t) = files.get(f) {
            if !m.kinds.iter().any(|k| k == "compose") {
                m.kinds.push("compose".into());
            }
            m.services.extend(compose_services(t));
        }
    }
    m
}

fn py_norm(s: &str) -> String {
    s.to_ascii_lowercase().replace(['_', '.'], "-")
}

/// Top-level service names of a compose file (a small YAML subset: `services:` then two-space
/// keys).
pub fn compose_services(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut indent = None;
    for l in text.lines() {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let lead = l.len() - l.trim_start().len();
        if lead == 0 {
            inside = l.trim_end() == "services:";
            indent = None;
            continue;
        }
        if inside && let Some(k) = l.trim().strip_suffix(':').filter(|k| !k.contains(' ')) {
            let i = *indent.get_or_insert(lead);
            if lead == i {
                out.push(k.trim_matches(['"', '\'']).to_owned());
            }
        }
    }
    out
}

/// `member → depends_on` suggested by manifests: B depends on A when B requires a name A
/// provides (same ecosystem).
pub fn suggest_deps(manifests: &BTreeMap<String, Manifest>) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (b, mb) in manifests {
        for (a, ma) in manifests {
            if a == b {
                continue;
            }
            if mb.requires.iter().any(|r| ma.provides.contains(r)) {
                out.entry(b.clone()).or_default().push(a.clone());
            }
        }
    }
    out
}

/// The overlay a member would use, from its manifests: its own ecosystem, if it consumes a
/// sibling there.
pub fn suggest_overlay(name: &str, manifests: &BTreeMap<String, Manifest>) -> Option<String> {
    let me = manifests.get(name)?;
    for (other, m) in manifests {
        if other == name {
            continue;
        }
        if let Some((eco, _)) = me.requires.iter().find(|r| m.provides.contains(r)) {
            return Some(eco.clone());
        }
        if me.services.iter().any(|s| s == other) {
            return Some("compose".into());
        }
    }
    None
}

// ── overlay files ──────────────────────────────────────────────────────────

/// `go.work` for the root: every Go member (paths relative to the root).
pub fn go_work(go_version: &str, members: &[String]) -> String {
    let mut t = format!(
        "// Generated by ptyro for this change set: build against siblings' in-progress code.\n// Never committed (it lives outside every member). `ptyro cs overlay <set> off` removes it.\ngo {go_version}\n\nuse (\n"
    );
    for m in members {
        t.push_str(&format!("\t./{m}\n"));
    }
    t.push_str(")\n");
    t
}

/// The root's `.cargo/config.toml`: `[patch]` path overrides for sibling crates. `patches`:
/// `(source, crate, path)` where source is `crates-io` or a git URL.
pub fn cargo_patch(patches: &[(String, String, String)]) -> String {
    let mut by: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (src, krate, path) in patches {
        by.entry(src.as_str()).or_default().push((krate.as_str(), path.as_str()));
    }
    let mut t = String::from("# Generated by ptyro for this change set (never committed).\n");
    for (src, v) in by {
        if src == "crates-io" {
            t.push_str("\n[patch.crates-io]\n");
        } else {
            t.push_str(&format!("\n[patch.{src:?}]\n"));
        }
        for (k, p) in v {
            t.push_str(&format!("{k} = {{ path = {p:?} }}\n"));
        }
    }
    t
}

/// The git URL a Cargo.toml takes `dep` from (`None`: crates.io or a path).
pub fn cargo_git_source(cargo_toml: &str, dep: &str) -> Option<String> {
    let t: toml::Table = cargo_toml.parse().ok()?;
    ["dependencies", "dev-dependencies", "build-dependencies"].iter().find_map(|k| {
        t.get(*k)?
            .as_table()?
            .iter()
            .find(|(name, v)| {
                *name == dep || v.get("package").and_then(|x| x.as_str()) == Some(dep)
            })
            .and_then(|(_, v)| v.get("git"))
            .and_then(|g| g.as_str())
            .map(str::to_owned)
    })
}

/// A consumer's `compose.override.yml`: siblings' worktrees as build contexts.
pub fn compose_override(services: &[(String, String)]) -> String {
    let mut t = String::from(
        "# Generated by ptyro for this change set (git-excluded, never committed).\nservices:\n",
    );
    for (svc, ctx) in services {
        t.push_str(&format!("  {svc}:\n    build:\n      context: {ctx}\n"));
    }
    t
}

// ── templates (§15.5.8) ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateMember {
    pub project: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub overlay: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateLead {
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub members: Vec<TemplateMember>,
    #[serde(default)]
    pub lead: Option<TemplateLead>,
    #[serde(default)]
    pub layout: Option<String>,
    /// Default: every gate.
    #[serde(default)]
    pub gates: Option<Vec<String>>,
    #[serde(default)]
    pub integration: Vec<String>,
    #[serde(default)]
    pub contract_checks: BTreeMap<String, String>,
}

impl Template {
    /// Parse and validate, reporting every mistake at once.
    pub fn parse(src: &str) -> Result<Self, String> {
        let t: Self = toml::from_str(src).map_err(|e| e.to_string())?;
        let mut errs = Vec::new();
        let names: Vec<&str> = t.members.iter().map(|m| m.project.as_str()).collect();
        if t.members.is_empty() {
            errs.push("a change set needs at least one member".to_owned());
        }
        let mut seen = BTreeSet::new();
        for m in &t.members {
            if !seen.insert(&m.project) {
                errs.push(format!("{} is listed twice", m.project));
            }
            for d in &m.depends_on {
                if !names.contains(&d.as_str()) {
                    errs.push(format!("{} depends on {d}, which isn't a member", m.project));
                }
            }
            if let Some(o) = &m.overlay
                && !OVERLAYS.contains(&o.as_str())
            {
                errs.push(format!(
                    "{}: overlay {o:?} isn't one of {}",
                    m.project,
                    OVERLAYS.join(", ")
                ));
            }
        }
        for g in t.gates.iter().flatten() {
            if !GATES.contains(&g.as_str()) {
                errs.push(format!("gate {g:?} isn't one of {}", GATES.join(", ")));
            }
        }
        if let Some(l) = &t.layout
            && let Err(e) = Layout::parse(l)
        {
            errs.push(e);
        }
        for k in t.contract_checks.keys() {
            if !names.contains(&k.as_str()) {
                errs.push(format!("contract_checks.{k}: not a member"));
            }
        }
        let deps: BTreeMap<String, Vec<String>> =
            t.members.iter().map(|m| (m.project.clone(), m.depends_on.clone())).collect();
        if let Err(c) = topo(&deps) {
            errs.push(format!("depends_on has a cycle: {}", c.join(" → ")));
        }
        if errs.is_empty() { Ok(t) } else { Err(errs.join("; ")) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> ChangeSet {
        let m = |n: &str, d: &[&str]| Member {
            name: n.into(),
            project: format!("p-{n}"),
            depends_on: d.iter().map(|s| (*s).to_owned()).collect(),
            branch: "ptyro/t".into(),
            ..Member::default()
        };
        ChangeSet {
            name: "t".into(),
            root: "/r".into(),
            profile: "default".into(),
            gates: GATES.iter().map(|s| (*s).to_owned()).collect(),
            members: vec![m("web", &["api", "sdk"]), m("api", &[]), m("sdk", &["api"])],
            ..ChangeSet::default()
        }
    }

    #[test]
    fn order_puts_dependencies_first_and_finds_cycles() {
        let s = set();
        assert_eq!(s.order().unwrap(), ["api", "sdk", "web"]);
        assert_eq!(s.dependents("api"), ["sdk", "web"]);
        let mut c = s.clone();
        c.members[1].depends_on = vec!["web".into()];
        let cycle = c.order().unwrap_err();
        assert_eq!(cycle.first(), cycle.last());
        assert!(cycle.len() >= 3, "{cycle:?}");
    }

    #[test]
    fn events_fold_and_round_trip() {
        let mut s = ChangeSet::default();
        let evs = vec![
            CsEvent::Created { set: Box::new(set()) },
            CsEvent::Contract {
                rev: Box::new(ContractRev { n: 1, text: "x".into(), ..ContractRev::default() }),
            },
            CsEvent::ContractApproved { n: 1, by: "cli".into() },
            CsEvent::BlockedOn { name: "web".into(), on: "api".into() },
            CsEvent::Ready { name: "api".into(), at: 5, rev: 1 },
            CsEvent::Unblocked { name: "web".into(), on: "api".into() },
            CsEvent::Train { train: Some(Train { order: vec!["api".into()], ..Train::default() }) },
            CsEvent::Landed { name: "api".into(), how: "merged".into() },
            CsEvent::MemberDropped { name: "sdk".into() },
        ];
        for e in &evs {
            let j = serde_json::to_string(e).unwrap();
            assert_eq!(&serde_json::from_str::<CsEvent>(&j).unwrap(), e, "{j}");
            s.apply(e);
        }
        assert_eq!(s.phase, Phase::Working);
        assert!(s.approved().is_some());
        assert!(s.member("web").unwrap().blocked_on.is_empty());
        assert_eq!(s.member("web").unwrap().depends_on, ["api"]);
        assert_eq!(s.train.as_ref().unwrap().done, ["api"]);
        assert!(s.member("sdk").is_none());
    }

    #[test]
    fn affects_lines_route_revisions() {
        let members: Vec<String> = ["api", "sdk", "web"].iter().map(|s| (*s).to_owned()).collect();
        let old = "# C\n\n## Users\naffects: api, sdk\n\nfield tier\n\n## Badge\n- affects: `web`\n\nPRO\n";
        let new = old.replace("field tier", "field tier: free|pro");
        assert_eq!(
            affected(old, &new, &members),
            (vec!["api".into(), "sdk".into()], vec!["web".into()])
        );
        // A section without affects: concerns everyone.
        let new2 = format!("{old}\n## Order\n1. api\n");
        assert_eq!(affected(old, &new2, &members).0, members);
        assert_eq!(affected(old, old, &members), (vec![], members.clone()));
        // Titles stand in for a missing affects line.
        let t1 = "## sdk (Go, example.com/sdk)\nIsPro\n## Wire format (api -> everyone)\ntier\n";
        let t2 = t1.replace("IsPro", "IsPro, IsFree");
        assert_eq!(affected(t1, &t2, &members).0, ["sdk"]);
        let t3 = t1.replace("tier", "tier: free|pro");
        assert_eq!(affected(t1, &t3, &members).0, members);
        assert_eq!(
            sections(old)[1].affects.as_deref(),
            Some(&["api".to_owned(), "sdk".to_owned()][..])
        );
    }

    #[test]
    fn affects_lines_ignore_case_and_know_everyone() {
        let members: Vec<String> = ["api", "sdk", "web"].iter().map(|s| (*s).to_owned()).collect();
        let old = "## Users\naffects: API, Sdk\n\nfield tier\n\n## Wire\naffects: everyone\n\nv1\n";
        let new = old.replace("field tier", "field tier: free|pro");
        assert_eq!(
            affected(old, &new, &members),
            (vec!["api".into(), "sdk".into()], vec!["web".into()])
        );
        let new = old.replace("v1", "v2");
        assert_eq!(affected(old, &new, &members), (members.clone(), vec![]));
        let r = ContractRev { affects: vec!["API".into()], ..ContractRev::default() };
        assert!(r.affects_member("api") && !r.affects_member("web"));
        let r = ContractRev { affects: vec!["All".into()], ..ContractRev::default() };
        assert!(r.affects_member("web"));
        assert!(ContractRev::default().affects_member("web"), "empty is everyone");
    }

    #[test]
    fn report_names_the_most_pressing_failure_with_an_action() {
        let mut s = set();
        let live = BTreeMap::new();
        assert_eq!(s.report(&live, false).state, "drafting");
        assert_eq!(s.report(&live, true).state, "contract_review");
        s.phase = Phase::Working;
        s.contract = vec![
            ContractRev { n: 1, ..ContractRev::default() },
            ContractRev { n: 2, affects: vec!["api".into()], ..ContractRev::default() },
        ];
        for m in &mut s.members {
            m.ready = Some(1);
            m.ready_rev = 2;
        }
        assert_eq!(s.report(&live, false).state, "ready");
        s.members[1].ready_rev = 1; // api, ready against r1; r2 affects api
        let r = s.report(&live, false);
        assert_eq!(r.state, "stale_contract");
        assert_eq!(r.members["api"], "stale");
        s.members[2].ready_rev = 1; // sdk: r2 doesn't affect it
        assert_eq!(
            s.report(&live, false).issues.iter().filter(|i| i.state == "stale_contract").count(),
            1
        );
        s.members[0].failed = Some("rate limited".into());
        s.members[2].blocked_on = vec!["api".into()];
        let r = s.report(&live, false);
        assert_eq!(r.state, "member_failed");
        assert!(r.issues.iter().all(|i| !i.action.is_empty()));
        assert_eq!(r.members["sdk"], "blocked_on:api");
        s.train = Some(Train {
            order: vec!["api".into(), "sdk".into(), "web".into()],
            done: vec!["api".into()],
            stopped: Some(("sdk".into(), "CI failed".into())),
            started_ms: 0,
        });
        let r = s.report(&live, false);
        assert_eq!(r.state, "land_stopped");
        assert!(
            r.issues[0].detail.contains("Landed: api; not landed: sdk, web"),
            "{}",
            r.issues[0].detail
        );
    }

    #[test]
    fn manifests_suggest_dependencies_and_overlays() {
        let f = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        let mut ms = BTreeMap::new();
        ms.insert("api".into(), manifest(&f(&[("go.mod", "module example.com/api\n\ngo 1.22\n")])));
        ms.insert(
            "sdk".into(),
            manifest(&f(&[(
                "go.mod",
                "module example.com/sdk\n\nrequire (\n\texample.com/api v0.0.0 // indirect\n\tgolang.org/x/net v0.1.0\n)\n",
            )])),
        );
        ms.insert(
            "web".into(),
            manifest(&f(&[
                ("package.json", r#"{"name":"web","dependencies":{"@acme/ui":"1"}}"#),
                ("compose.yml", "services:\n  api:\n    image: x\n  web:\n    build: .\n"),
            ])),
        );
        ms.insert("ui".into(), manifest(&f(&[("package.json", r#"{"name":"@acme/ui"}"#)])));
        let s = suggest_deps(&ms);
        assert_eq!(s["sdk"], ["api"]);
        assert_eq!(s["web"], ["ui"]);
        assert_eq!(suggest_overlay("sdk", &ms).as_deref(), Some("go"));
        assert_eq!(ms["web"].services, ["api", "web"]);
        let r = manifest(&f(&[(
            "Cargo.toml",
            "[package]\nname = \"cli\"\n[dependencies]\ncore = { path = \"x\", package = \"acme-core\" }\nserde = \"1\"\n",
        )]));
        assert_eq!(r.requires[0], ("rust".to_owned(), "acme-core".to_owned()));
        let p = manifest(&f(&[(
            "pyproject.toml",
            "[project]\nname = \"Acme_SDK\"\ndependencies = [\"acme.api>=1\"]\n",
        )]));
        assert_eq!(p.provides[0].1, "acme-sdk");
        assert_eq!(p.requires[0].1, "acme-api");
    }

    #[test]
    fn templates_validate_everything_at_once() {
        let ok = Template::parse(
            "name = \"x\"\nmembers = [{ project = \"api\" }, { project = \"web\", depends_on = [\"api\"], overlay = \"node\" }]\ngates = [\"contract\", \"land\"]\nintegration = [\"make e2e\"]\ncontract_checks = { api = \"make openapi\" }\n",
        )
        .unwrap();
        assert_eq!(ok.members.len(), 2);
        let e = Template::parse(
            "name = \"x\"\nmembers = [{ project = \"a\", depends_on = [\"b\"] }, { project = \"b\", depends_on = [\"a\"], overlay = \"perl\" }]\ngates = [\"merge\"]\n",
        )
        .unwrap_err();
        assert!(e.contains("cycle") && e.contains("perl") && e.contains("merge"), "{e}");
        assert!(Template::parse("name = \"x\"\nmembers = []\nbogus = 1\n").is_err());
    }

    #[test]
    fn pr_tables_replace_themselves_and_trailers_name_siblings() {
        let mut s = set();
        s.members[1].pr = Some(7);
        s.members[1].pr_url = Some("https://x/7".into());
        let ci = BTreeMap::from([("api".to_owned(), "passing".to_owned())]);
        let t = pr_table(&s, "sdk", &ci);
        assert!(t.contains("| 1 | api | [#7](https://x/7) | passing | open |"), "{t}");
        assert!(t.contains("sdk (this PR)"));
        let body = with_table("Adds tiers.", &t);
        let again = with_table(&body, &pr_table(&s, "sdk", &BTreeMap::new()));
        assert_eq!(again.matches(TABLE_START).count(), 1);
        assert!(again.starts_with("Adds tiers."));
        assert_eq!(
            trailers("t", &[("api".into(), "a1b2c3d4e5".into())]),
            "Change-Set: t\nChange-Set-Members: api@a1b2c3d"
        );
    }

    #[test]
    fn briefs_and_overlays_render() {
        let mut s = set();
        s.goal = Some("users get a tier".into());
        let b = brief(&s);
        assert!(b.contains("api → sdk → web") && b.contains("users get a tier"), "{b}");
        let c = contract_skeleton(&s);
        assert!(
            sections(&c)
                .iter()
                .any(|x| x.affects.as_ref().is_some_and(|a| a.contains(&"sdk".to_owned())))
        );
        let p = member_prompt(&s, &s.members[2], None);
        assert!(p.contains("../web") && p.contains("ptyro cs ready t"));
        assert!(go_work("1.22", &["api".into(), "sdk".into()]).contains("\t./sdk\n"));
        let cp = cargo_patch(&[
            ("crates-io".into(), "acme-core".into(), "/r/core".into()),
            ("https://g/x".into(), "x".into(), "/r/x".into()),
        ]);
        assert!(
            cp.contains("[patch.crates-io]\nacme-core = { path = \"/r/core\" }")
                && cp.contains("[patch.\"https://g/x\"]")
        );
        assert!(compose_override(&[("api".into(), "../api".into())]).contains("context: ../api"));
    }
}
