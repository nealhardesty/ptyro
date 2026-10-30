//! Change sets (SPEC §15.5, M11): one coordinated change across several repositories.
//!
//! A set is a run of kind `changeset` whose state carries a [`ChangeSet`] (`ptyro-run`), changed
//! only by [`CsEvent`]s through [`Daemon::emit`]: the run's log is the set's memory, and it
//! survives restarts like every run. This module does the I/O:
//! - **Creation** (`cs new`): members from projects or a template, one profile for all, the
//!   dependency order (cycles refused, manifests suggest it), member worktrees side by side in
//!   the set's root (all or nothing), `CHANGESET.md`, a `CONTRACT.md` skeleton, the overlay.
//! - **Agents** by layout: a lead (cwd the root, reads every member, `ptyro mcp` with the `cs_*`
//!   tools) and, once the contract is approved, one agent per member with its brief.
//! - **The actor** (one per set): records `CONTRACT.md` revisions when its editor is idle, runs
//!   the contract gate, propagates revisions by `affects:`, watches members (failed, exited,
//!   mail waiting for an exited agent), flags writes outside a role's fence that hooks can't see,
//!   and notices merges made outside the train.
//! - **Fences**: PreToolUse denials for edits outside a role's paths (`verify.rs` asks
//!   [`Daemon::cs_fence`]).
//! - **Messages**: the mailbox with typed kinds, readiness and `blocked_on` wake-ups.
//! - **The overlay**, **checks**, **set checkpoints**, and **lockstep git**: `cs commit` with
//!   trailers, dependency-ordered resumable `cs push`, linked PRs with a live sibling table, and
//!   the gated `cs land` merge train that stops with a precise partial-state report.

use ptyro_core::sync::MutexExt;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{RemoveOpts, WorktreeInfo, WorktreeSpec, WorktreeState};
use ptyro_core::session::{ProcessState, SessionKind, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{Reply, Request};
use ptyro_run::changeset::{
    self as cs, ChangeSet, CheckResult, ContractRev, CsEvent, Layout, Member, MemberLive, Phase,
    SetCheckpoint, Template, Train,
};
use ptyro_run::wire::{
    ChangeSetInfo, ContractAction, CsNewSpec, CsOp, GateInfo, LedgerEntry, TemplateInfo,
};
use ptyro_run::{Gate, Policy, RunEvent, RunKind, RunStatus, StepStatus};
use serde_json::{Value, json};

use crate::daemon::Daemon;

/// How long `land_pr` waits for a PR's first check runs to appear before taking "none" to mean
/// "this repository has no CI".
const NO_CHECKS_GRACE_MS: u64 = 120_000;

/// The grace period for a PR with no checks; tests shorten it with `PTYRO_TEST_LAND_GRACE_MS`.
fn no_checks_grace_ms() -> u64 {
    ptyro_core::test_knob("PTYRO_TEST_LAND_GRACE_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(NO_CHECKS_GRACE_MS)
}
use crate::mail::Caller;
use crate::session::Session;
use crate::worktrees::blocking;

/// What the actor remembers between ticks (not persisted: it is re-derived after a restart).
#[derive(Default)]
pub(crate) struct CsLive {
    /// A push or land running, and what it is.
    pub job: Option<String>,
    /// The gate step that job waits at, while it does (H2.7: `cs rm` may call it off).
    waiting_at: Option<String>,
    /// `cs rm` called the waiting job off: its gate gives up at once.
    call_off: bool,
    /// The lead's turn in progress: when it began, and each member's fingerprint then.
    lead_turn: Option<(u64, BTreeMap<String, String>)>,
    /// member → when its own agent was last seen working.
    last_work: BTreeMap<String, u64>,
    /// member → what someone else changed in it (writes hooks can't see).
    out_of_fence: BTreeMap<String, Vec<String>>,
    /// Sessions resumed for waiting mail, and when.
    resumed: BTreeMap<String, u64>,
    /// Gates already acted on.
    handled: BTreeSet<String>,
    /// Sessions whose turn an interrupt stopped, and when (one Esc per message, not a stream).
    interrupted: BTreeMap<String, u64>,
    last_merge_scan: u64,
    actor: bool,
}

/// A role in a set, for whoever is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Role {
    Human,
    Lead,
    Member(String),
    /// An agent of another set, or of none.
    Outsider(String),
}

fn cs_err(e: impl std::fmt::Display) -> Error {
    Error::internal(e.to_string())
}

fn sha_of(text: &str) -> String {
    ptyro_run::recipe::sha(text)
}

fn denied(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::PermissionDenied, msg.into())
}

/// The member's base branch name (`origin/main` → `main`).
fn base_branch(base: Option<&str>, fallback: Option<&str>) -> String {
    base.map(|b| b.rsplit_once('/').map_or(b, |(_, x)| x).to_owned())
        .or_else(|| fallback.map(str::to_owned))
        .unwrap_or_else(|| "main".into())
}

/// `path` with its longest existing ancestor canonicalized (symlinks resolved); the rest, not
/// created yet, appended as is.
pub(crate) fn real_path(path: &Path) -> PathBuf {
    let mut base = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, x: &std::ffi::OsString| acc.join(x));
        }
        match (base.file_name().map(std::ffi::OsStr::to_os_string), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                base = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `a/./b/../c` → `a/c`, without touching the file system.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

impl Daemon {
    // ── finding sets ───────────────────────────────────────────────────────

    /// A set by name, run id, or `@last`.
    pub(crate) fn find_set(&self, addr: &str) -> Result<String, Error> {
        let runs = self.runs.locked();
        let sets: Vec<(&String, &ChangeSet, u64)> = runs
            .iter()
            .filter_map(|(id, e)| e.state.changeset.as_ref().map(|c| (id, c, e.state.created_ms)))
            .collect();
        if addr == "@last" {
            return sets
                .iter()
                .max_by_key(|(_, _, t)| *t)
                .map(|(id, _, _)| (*id).clone())
                .ok_or_else(|| Error::not_found("no change sets yet"));
        }
        let live = |c: &ChangeSet| c.phase != Phase::Removed;
        if let Some((id, _, _)) =
            sets.iter().filter(|(_, c, _)| c.name == addr).max_by_key(|(_, c, t)| (live(c), *t))
        {
            return Ok((*id).clone());
        }
        if sets.iter().any(|(id, _, _)| *id == addr) {
            return Ok(addr.to_owned());
        }
        Err(Error::missing("change set", addr))
    }

    pub(crate) fn set_state(&self, run: &str) -> Result<ChangeSet, Error> {
        self.runs
            .locked()
            .get(run)
            .and_then(|e| e.state.changeset.clone())
            .ok_or_else(|| Error::missing("change set", run))
    }

    fn cs_emit(&self, run: &str, evs: Vec<CsEvent>) {
        self.emit(run, evs.into_iter().map(RunEvent::Cs).collect());
    }

    fn cs_note(&self, run: &str, text: impl Into<String>) {
        self.emit(run, vec![RunEvent::Note { text: text.into() }]);
    }

    /// The set a session belongs to, and its role there.
    fn set_of_session(&self, sid: &str) -> Option<(String, Role)> {
        let runs = self.runs.locked();
        runs.iter().find_map(|(id, e)| {
            let c = e.state.changeset.as_ref()?;
            if e.state.status.is_over() {
                return None;
            }
            if e.state.lead.as_deref() == Some(sid) {
                return Some((id.clone(), Role::Lead));
            }
            c.members
                .iter()
                .find(|m| m.session.as_deref() == Some(sid))
                .map(|m| (id.clone(), Role::Member(m.name.clone())))
        })
    }

    /// Who is asking, in `run`: the human (any non-agent client), its lead, a member, or an
    /// agent from elsewhere.
    fn role(&self, run: Option<&str>, caller: Caller) -> (Role, Option<Arc<Session>>) {
        let Some(s) = caller.pid.and_then(|p| self.session_of_pid(p)) else {
            return (Role::Human, None);
        };
        if s.kind() != SessionKind::Agent {
            return (Role::Human, None);
        }
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        match self.set_of_session(&s.id.full()) {
            Some((r, role)) if run.is_none_or(|x| x == r) => (role, Some(s)),
            _ => (Role::Outsider(name), Some(s)),
        }
    }

    // ── the request ────────────────────────────────────────────────────────

    pub(crate) async fn handle_cs(
        self: &Arc<Self>,
        op: CsOp,
        caller: Caller,
    ) -> Result<Reply, Error> {
        let value = self.cs_op(op, caller).await?;
        Ok(Reply::Cs { value })
    }

    async fn cs_op(self: &Arc<Self>, op: CsOp, caller: Caller) -> Result<Value, Error> {
        // Ops on one set: resolve it, and who is asking.
        let set_addr = match &op {
            CsOp::Show { set }
            | CsOp::Log { set, .. }
            | CsOp::Msg { set, .. }
            | CsOp::Ready { set, .. }
            | CsOp::Blocked { set, .. }
            | CsOp::Assign { set, .. }
            | CsOp::Contract { set, .. }
            | CsOp::Diff { set, .. }
            | CsOp::Check { set }
            | CsOp::Checkpoint { set, .. }
            | CsOp::Restore { set, .. }
            | CsOp::Overlay { set, .. }
            | CsOp::Commit { set, .. }
            | CsOp::Push { set, .. }
            | CsOp::Pr { set, .. }
            | CsOp::Land { set, .. }
            | CsOp::Add { set, .. }
            | CsOp::Drop { set, .. }
            | CsOp::Rm { set, .. }
            | CsOp::Gate { set, .. } => Some(set.clone()),
            CsOp::New { .. } | CsOp::List | CsOp::Whoami | CsOp::Templates => None,
        };
        let run = set_addr.as_deref().map(|a| self.find_set(a)).transpose()?;
        let (role, me) = self.role(run.as_deref(), caller);
        // Shipping and structure are the human's; agents coordinate (SPEC §15.5.2).
        let human_only = matches!(
            op,
            CsOp::New { .. }
                | CsOp::Restore { .. }
                | CsOp::Commit { .. }
                | CsOp::Push { .. }
                | CsOp::Pr { .. }
                | CsOp::Land { .. }
                | CsOp::Add { .. }
                | CsOp::Drop { .. }
                | CsOp::Rm { .. }
                | CsOp::Overlay { .. }
        );
        let lead_only = matches!(
            op,
            CsOp::Assign { .. }
                | CsOp::Checkpoint { .. }
                | CsOp::Gate { .. }
                | CsOp::Contract {
                    action: ContractAction::Revise { .. } | ContractAction::Submit { .. },
                    ..
                }
        );
        let who = match &role {
            Role::Human => "you".to_owned(),
            Role::Lead => "the lead".to_owned(),
            Role::Member(m) => format!("the {m} member"),
            Role::Outsider(n) => n.clone(),
        };
        if human_only && role != Role::Human {
            return Err(denied(format!(
                "{who} is an agent: change-set shipping and structure (commit, push, pr, land, restore, add, drop, rm, overlay) are the human's (ptyro cs … outside agents)"
            )));
        }
        if lead_only && !matches!(role, Role::Human | Role::Lead) {
            return Err(denied(format!(
                "only the set's lead (or you) may do that; {who} can ask the lead: ptyro cs msg {} @lead \"…\"",
                set_addr.clone().unwrap_or_default()
            )));
        }
        if matches!(role, Role::Outsider(_))
            && !matches!(
                op,
                CsOp::List | CsOp::Show { .. } | CsOp::Log { .. } | CsOp::Whoami | CsOp::Templates
            )
        {
            return Err(denied(format!("{who} isn't part of this change set")));
        }
        let me_id = me.as_ref().map(|s| s.id.full());
        match op {
            CsOp::New { spec } => {
                let run = self.cs_new(*spec).await?;
                Ok(json!(self.cs_info(&run)?))
            }
            CsOp::List => {
                let ids: Vec<String> = {
                    let runs = self.runs.locked();
                    let mut v: Vec<(u64, String)> = runs
                        .iter()
                        .filter(|(_, e)| e.state.changeset.is_some())
                        .map(|(id, e)| (e.state.created_ms, id.clone()))
                        .collect();
                    v.sort();
                    v.into_iter().rev().map(|(_, id)| id).collect()
                };
                Ok(json!(ids.iter().filter_map(|id| self.cs_info(id).ok()).collect::<Vec<_>>()))
            }
            CsOp::Show { .. } => Ok(json!(self.cs_info(run.as_deref().unwrap_or_default())?)),
            CsOp::Log { since, .. } => {
                Ok(json!(self.ledger(run.as_deref().unwrap_or_default(), since).await?))
            }
            CsOp::Whoami => {
                let role = match &role {
                    Role::Lead => "lead".to_owned(),
                    Role::Member(m) => format!("member:{m}"),
                    _ => "none".to_owned(),
                };
                let set = me_id
                    .as_deref()
                    .and_then(|id| self.set_of_session(id))
                    .and_then(|(r, _)| self.set_state(&r).ok())
                    .map(|c| c.name);
                Ok(json!({ "set": set, "role": role }))
            }
            CsOp::Msg { to, text, kind, interrupt, .. } => {
                let run = run.unwrap_or_default();
                self.cs_msg(&run, me_id.as_deref(), &role, &to, &text, kind.as_deref(), interrupt)
                    .await
            }
            CsOp::Ready { member, note, .. } => {
                let run = run.unwrap_or_default();
                let m = self.member_for(&run, &role, member)?;
                self.cs_ready(&run, &m, note).await
            }
            CsOp::Blocked { on, member, note, .. } => {
                let run = run.unwrap_or_default();
                let m = self.member_for(&run, &role, member)?;
                self.cs_blocked(&run, &m, &on, note).await
            }
            CsOp::Assign { member, task, .. } => {
                let run = run.unwrap_or_default();
                let set = self.set_state(&run)?;
                set.member(&member).ok_or_else(|| Error::missing("member", &member))?;
                self.cs_msg(&run, me_id.as_deref(), &role, &member, &task, Some("assign"), false)
                    .await
            }
            CsOp::Contract { action, .. } => {
                let run = run.unwrap_or_default();
                // A person may compare the contract with a file of theirs; an agent would be
                // asking the daemon to read any file it can, past its own permission prompts.
                if matches!(action, ContractAction::Diff { file: Some(_), .. })
                    && !matches!(role, Role::Human)
                {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        "an agent can't diff the contract against a file; the user can",
                    ));
                }
                let by = match &role {
                    Role::Lead => "lead".to_owned(),
                    Role::Member(m) => m.clone(),
                    _ => "user".to_owned(),
                };
                self.cs_contract(&run, action, &by).await
            }
            CsOp::Diff { member, .. } => {
                let run = run.unwrap_or_default();
                self.cs_diff(&run, &member).await
            }
            CsOp::Check { .. } => {
                let run = run.unwrap_or_default();
                Ok(json!(self.cs_check(&run).await?))
            }
            CsOp::Checkpoint { label, .. } => {
                let run = run.unwrap_or_default();
                Ok(json!(self.cs_checkpoint(&run, label, "manual").await?))
            }
            CsOp::Restore { n, force, .. } => {
                self.cs_restore(&run.unwrap_or_default(), n, force).await
            }
            CsOp::Overlay { member, mode, .. } => {
                self.cs_overlay(&run.unwrap_or_default(), member.as_deref(), &mode).await
            }
            CsOp::Commit { message, .. } => {
                self.cs_commit(&run.unwrap_or_default(), &message).await
            }
            CsOp::Push { yes, .. } => self.cs_push_request(&run.unwrap_or_default(), yes).await,
            CsOp::Pr { draft, .. } => self.cs_pr(&run.unwrap_or_default(), draft).await,
            CsOp::Land { yes, .. } => self.cs_land_request(&run.unwrap_or_default(), yes).await,
            CsOp::Add { project, agent, depends_on, .. } => {
                self.cs_add(&run.unwrap_or_default(), &project, agent, depends_on).await
            }
            CsOp::Drop { member, force, .. } => {
                self.cs_drop(&run.unwrap_or_default(), &member, force).await
            }
            CsOp::Rm { keep, force, .. } => {
                self.cs_rm(&run.unwrap_or_default(), &keep, force).await
            }
            CsOp::Gate { question, options, .. } => {
                let run = run.unwrap_or_default();
                let opts = if options.is_empty() {
                    vec!["approve".into(), "reject".into()]
                } else {
                    options
                };
                let gate =
                    self.cs_open_gate(&run, &format!("ask-{}", now_ms()), &question, opts).await;
                Ok(json!({ "gate": gate.id, "note": "the answer comes back to you as a message" }))
            }
            CsOp::Templates => {
                Ok(json!(self.cs_templates().into_iter().map(|(i, _)| i).collect::<Vec<_>>()))
            }
        }
    }

    /// The member a readiness or blocked report is about: the caller's own, or the named one
    /// (the human or the lead may report for a member).
    fn member_for(&self, run: &str, role: &Role, named: Option<String>) -> Result<String, Error> {
        match (role, named) {
            (Role::Member(m), Some(n)) if *m != n => {
                Err(denied(format!("the {m} member can only report for itself")))
            }
            (Role::Member(m), _) => Ok(m.clone()),
            (_, Some(n)) => {
                self.set_state(run)?.member(&n).ok_or_else(|| Error::missing("member", &n))?;
                Ok(n)
            }
            (_, None) => Err(Error::invalid("which member? (--member NAME)")),
        }
    }

    // ── creating a set ─────────────────────────────────────────────────────

    fn cs_root(&self, name: &str) -> PathBuf {
        self.paths.data.join("changesets").join(name)
    }

    async fn cs_new(self: &Arc<Self>, spec: CsNewSpec) -> Result<String, Error> {
        let name = ptyro_core::model::slugify(&spec.name);
        if name.is_empty() {
            return Err(Error::invalid("a change set needs a name"));
        }
        if let Ok(r) = self.find_set(&name)
            && self.set_state(&r).is_ok_and(|c| c.phase != Phase::Removed)
        {
            return Err(Error::conflict(format!(
                "change set {name} exists (ptyro cs show {name})"
            )));
        }
        // The template, if any.
        let template: Option<Template> = match &spec.template {
            Some(t) => {
                let (info, src) = self
                    .cs_templates()
                    .into_iter()
                    .find(|(i, _)| i.name == *t)
                    .ok_or_else(|| Error::missing("change-set template", t))?;
                if let Some(e) = info.error {
                    return Err(Error::invalid(format!("template {t}: {e}")));
                }
                if let Some(fp) = info.untrusted {
                    let hub = self.find_project(info.source.trim_start_matches("hub:"))?;
                    if spec.trust.as_deref() == Some(fp.as_str()) {
                        self.trust_as(&hub.id, &format!("changeset:{t}"), &fp);
                    } else {
                        return Err(Error::new(
                            ErrorCode::UntrustedProject,
                            format!(
                                "template {t} comes from the {} repository and isn't trusted; read {}, then run with --trust {fp}",
                                hub.slug, info.path
                            ),
                        ));
                    }
                }
                Some(Template::parse(&src).map_err(Error::invalid)?)
            }
            None => None,
        };
        let mut want: Vec<String> =
            template.iter().flat_map(|t| t.members.iter().map(|m| m.project.clone())).collect();
        for p in &spec.projects {
            if !want.contains(p) {
                want.push(p.clone());
            }
        }
        if want.is_empty() {
            return Err(Error::invalid(
                "a change set needs members: --projects api,sdk,web (or --template)",
            ));
        }
        let projects: Vec<ptyro_core::model::ProjectInfo> =
            want.iter().map(|p| self.find_project(p)).collect::<Result<_, _>>()?;
        let mut seen = BTreeSet::new();
        for p in &projects {
            if !seen.insert(p.slug.clone()) {
                return Err(Error::invalid(format!("{} is listed twice", p.slug)));
            }
        }
        // One workspace and one profile for every member (no accidental cross-account sets).
        let workspace = match &spec.workspace {
            Some(w) => self.home_workspace(Some(w), None)?,
            None => self.home_workspace(None, projects.first())?,
        };
        let mut profile: Option<(String, String)> = None;
        for p in &projects {
            let ws = self.home_workspace(None, Some(p))?;
            if spec.workspace.is_none() && ws != workspace {
                return Err(Error::invalid(format!(
                    "a change set lives in one workspace: {} is in {ws}, {} in {workspace} (--workspace picks one)",
                    p.slug, projects[0].slug
                )));
            }
            let root_wt = self
                .root_worktree(&p.id)
                .ok_or_else(|| Error::missing("worktree", format!("{}/@root", p.slug)))?;
            let res =
                self.resolve_profile(None, Some(&(root_wt, p.clone())), &workspace, false, true)?;
            match &profile {
                Some((prof, first)) if *prof != res.profile => {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "a change set runs under one profile: {first} resolves to {prof}, {} to {}. Pin them alike (ptyro project pin) or split the set",
                            p.slug, res.profile
                        ),
                    ));
                }
                Some(_) => {}
                None => profile = Some((res.profile.clone(), p.slug.clone())),
            }
        }
        let profile =
            profile.map(|(p, _)| p).unwrap_or_else(|| ptyro_core::model::DEFAULT_PROFILE.into());
        // Members: agents, dependencies, overlays.
        let config = self.config();
        let manifests: BTreeMap<String, cs::Manifest> = {
            let roots: Vec<(String, PathBuf)> =
                projects.iter().map(|p| (p.slug.clone(), p.root.clone())).collect();
            blocking(move || roots.into_iter().map(|(s, r)| (s, read_manifest(&r))).collect())
                .await?
        };
        let suggested = cs::suggest_deps(&manifests);
        let tmember = |slug: &str, id: &str| {
            template.as_ref().and_then(|t| {
                t.members.iter().find(|m| m.project == slug || m.project == id).cloned()
            })
        };
        let mut notes = Vec::new();
        let mut members = Vec::new();
        for p in &projects {
            let tm = tmember(&p.slug, &p.id);
            let cfg =
                ptyro_core::project::ProjectConfig::load(&p.root, &p.root).unwrap_or_default();
            let agent = spec
                .agents
                .get(&p.slug)
                .cloned()
                .or_else(|| tm.as_ref().and_then(|t| t.agent.clone()))
                .or(cfg.project.default_agent.clone())
                .unwrap_or_else(|| ptyro_agents::default_agent(&config));
            let explicit = spec
                .depends
                .get(&p.slug)
                .cloned()
                .or_else(|| tm.as_ref().map(|t| t.depends_on.clone()));
            let depends_on = match explicit {
                Some(d) => d,
                None => {
                    let d = suggested.get(&p.slug).cloned().unwrap_or_default();
                    if !d.is_empty() {
                        notes.push(format!(
                            "{} depends on {} (from its manifests)",
                            p.slug,
                            d.join(", ")
                        ));
                    }
                    d
                }
            };
            let overlay = tm.as_ref().and_then(|t| t.overlay.clone()).or_else(|| {
                spec.overlay.then(|| cs::suggest_overlay(&p.slug, &manifests)).flatten()
            });
            members.push(Member {
                name: p.slug.clone(),
                project: p.id.clone(),
                agent: Some(agent),
                depends_on,
                overlay,
                base: spec
                    .bases
                    .get(&p.slug)
                    .cloned()
                    .or_else(|| tm.as_ref().and_then(|t| t.base.clone())),
                contract_check: template
                    .as_ref()
                    .and_then(|t| t.contract_checks.get(&p.slug).cloned()),
                ..Member::default()
            });
        }
        let names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
        for m in &members {
            for d in &m.depends_on {
                if !names.contains(d) {
                    return Err(Error::invalid(format!(
                        "{} depends on {d}, which isn't a member",
                        m.name
                    )));
                }
            }
        }
        let layout =
            match spec.layout.as_deref().or(template.as_ref().and_then(|t| t.layout.as_deref())) {
                Some(l) => Layout::parse(l).map_err(Error::invalid)?,
                None => Layout::Lead,
            };
        let gates = spec
            .gates
            .clone()
            .or_else(|| template.as_ref().and_then(|t| t.gates.clone()))
            .unwrap_or_else(|| cs::GATES.iter().map(|g| (*g).to_owned()).collect());
        for g in &gates {
            if !cs::GATES.contains(&g.as_str()) {
                return Err(Error::invalid(format!("gate {g:?}: contract, push or land")));
            }
        }
        let mut goal = spec.goal.clone();
        if let Some(issue) = &spec.from_issue
            && let Some(wt) = self.root_worktree(&projects[0].id)
        {
            let n = crate::github::number_of(issue)
                .map(|n| n.to_string())
                .unwrap_or_else(|| issue.clone());
            let out = self
                .gh(
                    &wt,
                    vec![
                        "issue".into(),
                        "view".into(),
                        n,
                        "--json".into(),
                        "title,body,url".into(),
                    ],
                )
                .await?;
            let v: Value = serde_json::from_str(&out).map_err(cs_err)?;
            goal = Some(format!(
                "{}{}\n\n(from {})",
                v["title"].as_str().unwrap_or_default(),
                v["body"]
                    .as_str()
                    .filter(|b| !b.trim().is_empty())
                    .map(|b| format!("\n\n{}", b.trim()))
                    .unwrap_or_default(),
                v["url"].as_str().unwrap_or(issue)
            ));
        }
        let root = self.cs_root(&name);
        if root.exists() && std::fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some()) {
            return Err(Error::conflict(format!("{} exists and isn't empty", root.display())));
        }
        let mut set = ChangeSet {
            name: name.clone(),
            root: root.to_string_lossy().into_owned(),
            layout,
            workspace: Some(workspace.clone()),
            profile: profile.clone(),
            goal,
            template: spec.template.clone(),
            lead_agent: spec
                .lead
                .clone()
                .or_else(|| {
                    template.as_ref().and_then(|t| t.lead.as_ref().and_then(|l| l.agent.clone()))
                })
                .or_else(|| Some(ptyro_agents::default_agent(&config))),
            gates,
            integration: if spec.integration.is_empty() {
                template.as_ref().map(|t| t.integration.clone()).unwrap_or_default()
            } else {
                spec.integration.clone()
            },
            members,
            ..ChangeSet::default()
        };
        if let Err(cycle) = set.order() {
            return Err(Error::invalid(format!("depends_on has a cycle: {}", cycle.join(" → "))));
        }
        std::fs::create_dir_all(root.join(".ptyro")).map_err(cs_err)?;
        // A run holds it; agents in it are held to a policy (no ad-hoc workers, no shipping).
        let policy = Policy {
            max_sessions: 0,
            max_spawns: Some(0),
            agents: vec![],
            worktrees: vec![],
            destructive: false,
            chat: false,
        };
        let run = self.create_run(
            RunKind::Changeset,
            None,
            None,
            BTreeMap::from([("set".to_owned(), name.clone())]),
            Some(projects[0].id.clone()),
            None,
            None,
            Some(policy),
            None,
        );
        self.cs_emit(&run, vec![CsEvent::Created { set: Box::new(set.clone()) }]);
        // Every member's worktree, side by side in the root, or none (M11.2).
        let mut made: Vec<WorktreeInfo> = Vec::new();
        for m in &set.members {
            match self.cs_worktree(&set, m, &spec.env).await {
                Ok(wt) => {
                    self.cs_emit(
                        &run,
                        vec![CsEvent::MemberInfo {
                            name: m.name.clone(),
                            worktree: Some(wt.id.clone()),
                            path: Some(wt.path.to_string_lossy().into_owned()),
                            branch: wt.branch.clone(),
                            session: None,
                            overlay: None,
                            start: ptyro_git::Git::new().rev(&wt.path, "HEAD"),
                        }],
                    );
                    made.push(wt);
                }
                Err(e) => {
                    for w in made.iter().rev() {
                        let _ = self
                            .remove_worktree(
                                &w.id,
                                RemoveOpts { force: true, stop_sessions: true, keep_branch: false },
                            )
                            .await;
                    }
                    let _ = std::fs::remove_dir_all(&root);
                    self.finish_run(
                        &run,
                        RunStatus::Cancelled,
                        Some(format!("creating {}: {}", m.name, e.message)),
                    )
                    .await;
                    self.cs_emit(&run, vec![CsEvent::Phase { phase: Phase::Removed }]);
                    return Err(Error::new(
                        e.code,
                        format!(
                            "couldn't create {}'s worktree, so nothing was created: {}",
                            m.name, e.message
                        ),
                    ));
                }
            }
        }
        set = self.set_state(&run)?;
        self.write_brief(&set);
        let contract = root.join("CONTRACT.md");
        if !contract.exists() {
            std::fs::write(&contract, cs::contract_skeleton(&set)).map_err(cs_err)?;
        }
        if set.members.iter().any(|m| m.overlay.is_some()) {
            let n = self.apply_overlays(&run).await;
            notes.extend(n);
        }
        for n in notes {
            self.cs_note(&run, n);
        }
        match set.layout {
            Layout::Lead => {
                self.spawn_lead(&run).await?;
            }
            Layout::Solo => {
                self.cs_emit(&run, vec![CsEvent::Phase { phase: Phase::Working }]);
                self.spawn_lead(&run).await?;
            }
            Layout::Manual => self.cs_emit(&run, vec![CsEvent::Phase { phase: Phase::Working }]),
        }
        self.start_cs_actor(&run);
        Ok(run)
    }

    /// A member's worktree, inside the set's root.
    async fn cs_worktree(
        self: &Arc<Self>,
        set: &ChangeSet,
        m: &Member,
        env: &BTreeMap<String, String>,
    ) -> Result<WorktreeInfo, Error> {
        let spec = WorktreeSpec {
            project: m.project.clone(),
            name: Some(self.free_worktree_name(&m.project, &set.name)),
            base: m.base.clone(),
            path: Some(Path::new(&set.root).join(&m.name)),
            profile: Some(set.profile.clone()),
            workspace: set.workspace.clone(),
            env: env.clone(),
            confirmed: true,
            ..WorktreeSpec::default()
        };
        match self.create_worktree(spec).await? {
            Reply::WorktreeCreated { info, .. } => Ok(info),
            _ => Err(Error::internal("unexpected reply creating a worktree")),
        }
    }

    fn write_brief(&self, set: &ChangeSet) {
        let _ = std::fs::write(Path::new(&set.root).join("CHANGESET.md"), cs::brief(set));
    }

    // ── agents ─────────────────────────────────────────────────────────────

    fn agent_env(&self, run: &str, set: &ChangeSet, step: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("PTYRO_CS".to_owned(), set.name.clone()),
            ("PTYRO_CS_ROOT".to_owned(), set.root.clone()),
            ("PTYRO_RUN_ID".to_owned(), run.to_owned()),
            ("PTYRO_STEP_ID".to_owned(), step.to_owned()),
            ("PTYRO_RUN_DIR".to_owned(), self.run_dir(run).to_string_lossy().into_owned()),
        ])
    }

    /// The lead (or the solo agent): cwd the root, reading every member.
    async fn spawn_lead(self: &Arc<Self>, run: &str) -> Result<Arc<Session>, Error> {
        let set = self.set_state(run)?;
        let agent =
            set.lead_agent.clone().unwrap_or_else(|| ptyro_agents::default_agent(&self.config()));
        let (aname, _) = ptyro_agents::split_agent(&agent);
        let mut argv = Vec::new();
        if aname == "claude" {
            for m in &set.members {
                argv.extend(["--add-dir".to_owned(), m.path.clone()]);
            }
        }
        let prompt = match set.layout {
            Layout::Solo => solo_prompt(&set),
            _ => cs::lead_prompt(&set),
        };
        let step = if set.layout == Layout::Solo { "solo" } else { "lead" };
        let spec = SpawnSpec {
            agent: Some(agent.clone()),
            argv,
            cwd: PathBuf::from(&set.root),
            profile: Some(set.profile.clone()),
            workspace: set.workspace.clone(),
            prompt: Some(prompt),
            name: Some(self.free_name(&format!("{}-{step}", set.name))),
            env: self.agent_env(run, &set, step),
            confirmed: true,
            unqueued: true,
            mcp: set.layout == Layout::Lead,
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        let sid = s.id.full();
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).run =
            Some((run.to_owned(), "lead".into()));
        self.emit(
            run,
            vec![
                RunEvent::LeadSet { session: sid.clone() },
                RunEvent::StepAdded { step: "lead".into() },
                RunEvent::AttemptStarted { step: "lead".into(), n: 1, at: now_ms() },
                RunEvent::AttemptInfo {
                    step: "lead".into(),
                    session: Some(sid),
                    conversation: None,
                    worktree: None,
                    start_ref: None,
                },
            ],
        );
        Ok(s)
    }

    /// A member's agent, with its brief and the approved contract.
    async fn spawn_member(self: &Arc<Self>, run: &str, name: &str) -> Result<Arc<Session>, Error> {
        let set = self.set_state(run)?;
        let m = set.member(name).ok_or_else(|| Error::missing("member", name))?.clone();
        let wt = m
            .worktree
            .clone()
            .ok_or_else(|| Error::invalid(format!("{name} has no worktree yet")))?;
        let agent = m.agent.clone().unwrap_or_else(|| ptyro_agents::default_agent(&self.config()));
        let (aname, _) = ptyro_agents::split_agent(&agent);
        let mut argv = Vec::new();
        if aname == "claude" {
            for o in set.members.iter().filter(|o| o.name != m.name) {
                argv.extend(["--add-dir".to_owned(), o.path.clone()]);
            }
        }
        let approved = set.approved().cloned();
        let spec = SpawnSpec {
            agent: Some(agent.clone()),
            argv,
            cwd: PathBuf::from(&m.path),
            worktree: Some(wt.clone()),
            profile: Some(set.profile.clone()),
            workspace: set.workspace.clone(),
            prompt: Some(cs::member_prompt(&set, &m, approved.as_ref())),
            name: Some(self.free_name(&format!("{}-{}", set.name, m.name))),
            env: self.agent_env(run, &set, &m.name),
            confirmed: true,
            unqueued: true,
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        let sid = s.id.full();
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).run =
            Some((run.to_owned(), m.name.clone()));
        let n = self
            .run_info(run)
            .and_then(|i| i.state.steps.get(&m.name).map(|x| x.attempts.len() as u32))
            .unwrap_or(0)
            + 1;
        self.emit(
            run,
            vec![
                RunEvent::StepAdded { step: m.name.clone() },
                RunEvent::AttemptStarted { step: m.name.clone(), n, at: now_ms() },
                RunEvent::AttemptInfo {
                    step: m.name.clone(),
                    session: Some(sid.clone()),
                    conversation: None,
                    worktree: Some(wt),
                    start_ref: None,
                },
                RunEvent::SessionAdded { session: sid.clone() },
            ],
        );
        self.cs_emit(
            run,
            vec![
                CsEvent::MemberInfo {
                    name: m.name.clone(),
                    worktree: None,
                    path: None,
                    branch: None,
                    session: Some(sid),
                    overlay: None,
                    start: None,
                },
                CsEvent::Delivered { name: m.name.clone(), rev: approved.map_or(0, |r| r.n) },
            ],
        );
        Ok(s)
    }

    // ── info and the ledger ───────────────────────────────────────────────

    fn member_live(&self, set: &ChangeSet) -> BTreeMap<String, MemberLive> {
        let extra =
            self.cs.locked().get(&set.name).map(|l| l.out_of_fence.clone()).unwrap_or_default();
        set.members
            .iter()
            .map(|m| {
                let s = m.session.as_deref().and_then(|x| self.resolve(x).ok());
                let status = s.as_ref().map(|s| {
                    if s.is_live() {
                        s.status().map_or_else(|| "running".to_owned(), |st| st.label().to_owned())
                    } else {
                        "exited".to_owned()
                    }
                });
                let wt = m.worktree.as_deref().and_then(|w| self.worktree_by_id(w));
                let missing = wt
                    .as_ref()
                    .is_none_or(|w| w.state == WorktreeState::Missing || !w.path.exists());
                let st = wt.as_ref().and_then(|w| w.status.clone());
                (
                    m.name.clone(),
                    MemberLive {
                        status,
                        dirty: st.as_ref().is_some_and(|s| s.modified + s.staged + s.untracked > 0),
                        ahead: st.as_ref().map_or(0, |s| s.base_ahead),
                        out_of_fence: extra.get(&m.name).cloned().unwrap_or_default(),
                        missing: m.worktree.is_some() && missing,
                    },
                )
            })
            .collect()
    }

    pub(crate) fn cs_info(&self, run: &str) -> Result<ChangeSetInfo, Error> {
        let info = self.run_info(run).ok_or_else(|| Error::missing("change set", run))?;
        let set = info.state.changeset.clone().ok_or_else(|| Error::missing("change set", run))?;
        let live = self.member_live(&set);
        let gates: Vec<GateInfo> = info
            .state
            .open_gates()
            .into_iter()
            .map(|(step, g)| GateInfo {
                run: run.to_owned(),
                step: step.to_owned(),
                gate: g.clone(),
            })
            .collect();
        let contract_gate = gates.iter().any(|g| g.step.starts_with("contract"));
        let report = set.report(&live, contract_gate);
        let name_of = |id: &str| {
            self.resolve(id)
                .map(|s| {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone()
                })
                .ok()
        };
        Ok(ChangeSetInfo {
            run: run.to_owned(),
            status: info.state.status.label().to_owned(),
            lead: info.state.lead.as_deref().and_then(name_of),
            sessions: set
                .members
                .iter()
                .filter_map(|m| Some((m.name.clone(), name_of(m.session.as_deref()?)?)))
                .collect(),
            job: self.cs.locked().get(&set.name).and_then(|l| l.job.clone()),
            set,
            report,
            live,
            gates,
        })
    }

    async fn ledger(
        self: &Arc<Self>,
        run: &str,
        since: Option<u64>,
    ) -> Result<Vec<LedgerEntry>, Error> {
        let mut out = Vec::new();
        if let Some(store) = &self.store {
            store.flush().await.map_err(cs_err)?;
            for (seq, at, payload) in store.run_events(run.to_owned()).await.map_err(cs_err)? {
                if since.is_some_and(|s| seq <= s) {
                    continue;
                }
                let Ok(ev) = serde_json::from_value::<RunEvent>(payload) else { continue };
                // The mailbox notes each message on the run; the ledger lists the messages
                // themselves (with names), so skip the notes.
                if let RunEvent::Note { text } = &ev
                    && text.contains(" → ")
                    && text.contains("): ")
                {
                    continue;
                }
                if let Some((kind, text)) = describe(&ev) {
                    out.push(LedgerEntry {
                        seq,
                        at_ms: at,
                        t: "event".into(),
                        kind,
                        from: None,
                        to: None,
                        text,
                    });
                }
            }
            let since_ms = since.and_then(|s| out.iter().find(|e| e.seq > s).map(|e| e.at_ms));
            for m in store.messages(Some(run.to_owned()), 2000).await.map_err(cs_err)? {
                if since.is_some() && since_ms.is_none_or(|t| m.at_ms < t) {
                    continue;
                }
                let v = self.message_view(&m);
                out.push(LedgerEntry {
                    seq: 0,
                    at_ms: v.at_ms,
                    t: "message".into(),
                    kind: v.kind,
                    from: Some(v.from),
                    to: Some(v.to),
                    text: v.body,
                });
            }
        }
        out.sort_by_key(|e| (e.at_ms, e.seq));
        Ok(out)
    }

    // ── messages, readiness, blocked_on ─────────────────────────────────────

    /// The session id a set address names: a member, `@lead`, or anything the mailbox knows.
    fn cs_address(&self, set: &ChangeSet, run: &str, to: &str) -> Result<String, Error> {
        let t = to.trim().trim_start_matches('@');
        if let Some(m) = set.member(t) {
            return m.session.clone().ok_or_else(|| {
                Error::conflict(format!(
                    "{t} has no agent yet (it starts once the contract is approved)"
                ))
            });
        }
        if t == "lead" {
            return self
                .run_info(run)
                .and_then(|i| i.state.lead)
                .ok_or_else(|| Error::invalid("this set has no lead (layout solo or manual)"));
        }
        Ok(to.to_owned())
    }

    #[allow(clippy::too_many_arguments)]
    async fn cs_msg(
        self: &Arc<Self>,
        run: &str,
        from: Option<&str>,
        role: &Role,
        to: &str,
        text: &str,
        kind: Option<&str>,
        interrupt: bool,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let kind = kind.unwrap_or(match role {
            Role::Lead | Role::Human => "assign",
            _ => "question",
        });
        if !cs::KINDS.contains(&kind) {
            return Err(Error::invalid(format!("kind {kind:?}: one of {}", cs::KINDS.join(", "))));
        }
        let targets: Vec<String> = if to.trim() == "@all" {
            let mut v: Vec<String> = set.members.iter().filter_map(|m| m.session.clone()).collect();
            v.extend(self.run_info(run).and_then(|i| i.state.lead));
            v.retain(|x| Some(x.as_str()) != from);
            v
        } else {
            vec![self.cs_address(&set, run, to)?]
        };
        // An agent writes to the set (its members and lead) or to the user: not to any session
        // of yours it can name.
        if !matches!(role, Role::Human) {
            let own: Vec<String> = set
                .members
                .iter()
                .filter_map(|m| m.session.clone())
                .chain(self.run_info(run).and_then(|i| i.state.lead))
                .collect();
            for t in &targets {
                let id = self.resolve(t).map_or_else(|_| t.clone(), |s| s.id.full());
                if t != "@user" && !own.contains(&id) {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "{t} isn't in {}: members write to the set, its lead or the user",
                            set.name
                        ),
                    ));
                }
            }
        }
        if targets.is_empty() {
            return Err(Error::missing(
                "recipient",
                format!("{to} (no agents in {} yet)", set.name),
            ));
        }
        let mut ids = Vec::new();
        let mut names = Vec::new();
        for t in targets {
            let (i, n) = self
                .post(
                    from,
                    &t,
                    kind,
                    text,
                    Some(run.to_owned()),
                    None,
                    Some(if interrupt { "interrupt" } else { "normal" }.into()),
                    None,
                    None,
                )
                .await?;
            ids.extend(i);
            names.extend(n);
        }
        // A contract-change request also tells the human.
        if kind == "contract_change" {
            self.notify_run(
                run,
                AttentionKind::Run,
                format!("{}: contract change requested", set.name),
                text.to_owned(),
            )
            .await;
        }
        Ok(json!({ "ids": ids, "to": names }))
    }

    async fn cs_ready(
        self: &Arc<Self>,
        run: &str,
        member: &str,
        note: Option<String>,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let rev = set.contract_head().map_or(0, |r| r.n);
        self.cs_emit(run, vec![CsEvent::Ready { name: member.to_owned(), at: now_ms(), rev }]);
        let from = set.member(member).and_then(|m| m.session.clone());
        let text = format!(
            "{member} is ready for integration (contract r{rev}){}",
            note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
        );
        if let Some(lead) = self.run_info(run).and_then(|i| i.state.lead)
            && Some(&lead) != from.as_ref()
        {
            let _ = self
                .post(
                    from.as_deref().or(Some("@ptyro")),
                    &lead,
                    "ready_for_integration",
                    &text,
                    Some(run.to_owned()),
                    None,
                    None,
                    None,
                    None,
                )
                .await;
        }
        // Wake whoever waited on it.
        let mut woken = Vec::new();
        for m in set.members.iter().filter(|m| m.blocked_on.iter().any(|b| b == member)) {
            self.cs_emit(
                run,
                vec![CsEvent::Unblocked { name: m.name.clone(), on: member.to_owned() }],
            );
            if let Some(sid) = &m.session {
                let _ = self
                    .post(
                        Some("@ptyro"),
                        sid,
                        "ready_for_integration",
                        &format!(
                            "{text}. You were blocked on it: carry on (its code is at ../{member})."
                        ),
                        Some(run.to_owned()),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await;
            }
            woken.push(m.name.clone());
        }
        // Everyone ready: run the integration checks once.
        let set = self.set_state(run)?;
        if !set.integration.is_empty() && set.members.iter().all(|m| m.ready.is_some()) {
            let me = Arc::clone(self);
            let run = run.to_owned();
            tokio::spawn(async move {
                let _ = me.cs_check(&run).await;
            });
        }
        Ok(json!({ "member": member, "rev": rev, "woke": woken }))
    }

    async fn cs_blocked(
        self: &Arc<Self>,
        run: &str,
        member: &str,
        on: &str,
        note: Option<String>,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let on = on.trim().trim_start_matches('@');
        if on == member {
            return Err(Error::invalid("a member can't be blocked on itself"));
        }
        let target = set.member(on).ok_or_else(|| Error::missing("member", on))?;
        self.cs_emit(run, vec![CsEvent::BlockedOn { name: member.to_owned(), on: on.to_owned() }]);
        let text = format!(
            "{member} is blocked on {on}{}",
            note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
        );
        let from = set.member(member).and_then(|m| m.session.clone());
        if let Some(lead) = self.run_info(run).and_then(|i| i.state.lead) {
            let _ = self
                .post(
                    from.as_deref().or(Some("@ptyro")),
                    &lead,
                    "blocked_on",
                    &text,
                    Some(run.to_owned()),
                    None,
                    None,
                    None,
                    None,
                )
                .await;
        }
        self.notify_run(
            run,
            AttentionKind::Blocked,
            format!("{}: {text}", set.name),
            format!("ptyro cs status {}", set.name),
        )
        .await;
        // Already ready: wake it at once.
        if target.ready.is_some() {
            self.cs_emit(
                run,
                vec![CsEvent::Unblocked { name: member.to_owned(), on: on.to_owned() }],
            );
            if let Some(sid) = &from {
                let _ = self
                    .post(
                        Some("@ptyro"),
                        sid,
                        "ready_for_integration",
                        &format!("{on} is ready already: carry on (its code is at ../{on})."),
                        Some(run.to_owned()),
                        None,
                        None,
                        None,
                        None,
                    )
                    .await;
            }
            return Ok(json!({ "member": member, "on": on, "woken": true }));
        }
        Ok(json!({ "member": member, "on": on, "woken": false }))
    }

    // ── the contract ───────────────────────────────────────────────────────

    fn contract_path(set: &ChangeSet) -> PathBuf {
        Path::new(&set.root).join("CONTRACT.md")
    }

    /// `git diff --no-index` between two texts, without the header.
    async fn text_diff(&self, a: &str, b: &str) -> String {
        let dir = self.paths.state.join("tmp");
        let (a, b) = (a.to_owned(), b.to_owned());
        blocking(move || {
            let _ = std::fs::create_dir_all(&dir);
            let n = now_ms();
            let (pa, pb) =
                (dir.join(format!("contract-a-{n}")), dir.join(format!("contract-b-{n}")));
            let _ = std::fs::write(&pa, a);
            let _ = std::fs::write(&pb, b);
            let out = std::process::Command::new("git")
                .args(["diff", "--no-index", "--no-color", "-U3"])
                .arg(&pa)
                .arg(&pb)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let _ = std::fs::remove_file(&pa);
            let _ = std::fs::remove_file(&pb);
            out.lines().skip_while(|l| !l.starts_with("@@")).collect::<Vec<_>>().join("\n")
        })
        .await
        .unwrap_or_default()
    }

    /// Record `text` as the next revision: diff, affected members, a checkpoint first (after
    /// r1), then the gate (r1) or the propagation (later ones).
    async fn record_revision(
        self: &Arc<Self>,
        run: &str,
        text: String,
        by: &str,
        affects: Vec<String>,
        summary: Option<String>,
    ) -> Result<ContractRev, Error> {
        let set = self.set_state(run)?;
        let prev = set.contract_head().cloned();
        if prev.as_ref().is_some_and(|p| p.sha == sha_of(&text)) {
            return Err(Error::conflict(format!(
                "the contract is unchanged since r{}",
                prev.map_or(0, |p| p.n)
            )));
        }
        let members: Vec<String> = set.members.iter().map(|m| m.name.clone()).collect();
        let (diff, hit) = match &prev {
            Some(p) => {
                let (hit, _) = cs::affected(&p.text, &text, &members);
                (self.text_diff(&p.text, &text).await, hit)
            }
            None => (String::new(), members.clone()),
        };
        let affects = if affects.is_empty() { hit } else { affects };
        if prev.is_some() && set.phase == Phase::Working {
            let _ = self
                .cs_checkpoint(
                    run,
                    Some(format!("before contract r{}", set.contract.len() + 1)),
                    "contract",
                )
                .await;
        }
        let rev = ContractRev {
            n: set.contract.len() as u32 + 1,
            at_ms: now_ms(),
            by: by.to_owned(),
            sha: sha_of(&text),
            text,
            diff,
            affects: if affects.len() == members.len() { vec![] } else { affects },
            summary,
            approved: None,
        };
        let _ = std::fs::write(Self::contract_path(&set), &rev.text);
        self.cs_emit(run, vec![CsEvent::Contract { rev: Box::new(rev.clone()) }]);
        match set.phase {
            Phase::Drafting if set.gated("contract") => {
                let ask = format!(
                    "Approve {}'s contract r{}? (ptyro cs contract {} show)",
                    set.name, rev.n, set.name
                );
                self.cs_open_gate(
                    run,
                    &format!("contract-r{}", rev.n),
                    &ask,
                    vec!["approve".into(), "revise".into(), "reject".into()],
                )
                .await;
            }
            Phase::Drafting => self.approve_contract(run, rev.n, "auto").await,
            Phase::Working | Phase::Landing => self.propagate(run, &rev).await,
            _ => {}
        }
        Ok(rev)
    }

    /// Tell members about a revision: those it affects at once, the rest at their next idle.
    async fn propagate(self: &Arc<Self>, run: &str, rev: &ContractRev) {
        let Ok(set) = self.set_state(run) else { return };
        let text = cs::revision_message(rev);
        for m in &set.members {
            let Some(sid) = &m.session else { continue };
            let urgent = rev.affects_member(&m.name) && !rev.affects.is_empty();
            let res = self
                .post(
                    Some("@ptyro"),
                    sid,
                    "contract_revised",
                    &text,
                    Some(run.to_owned()),
                    None,
                    Some(if urgent { "interrupt" } else { "normal" }.into()),
                    None,
                    Some(format!("r{}", rev.n)),
                )
                .await;
            if res.is_ok() {
                self.cs_emit(run, vec![CsEvent::Delivered { name: m.name.clone(), rev: rev.n }]);
            }
        }
    }

    async fn approve_contract(self: &Arc<Self>, run: &str, n: u32, by: &str) {
        self.cs_emit(run, vec![CsEvent::ContractApproved { n, by: by.to_owned() }]);
        let Ok(set) = self.set_state(run) else { return };
        if set.layout != Layout::Lead {
            return;
        }
        for m in set.members.iter().filter(|m| m.session.is_none()) {
            if let Err(e) = self.spawn_member(run, &m.name).await {
                self.cs_emit(
                    run,
                    vec![CsEvent::Failed {
                        name: m.name.clone(),
                        error: Some(format!("couldn't start its agent: {}", e.message)),
                    }],
                );
            }
        }
        if let Some(lead) = self.run_info(run).and_then(|i| i.state.lead) {
            let _ = self
                .post(
                    Some("@ptyro"),
                    &lead,
                    "answer",
                    &format!("Contract r{n} is approved; the member agents are starting with their briefs. Coordinate them now (cs_status, cs_send)."),
                    Some(run.to_owned()),
                    None,
                    None,
                    None,
                    Some(format!("approved-r{n}")),
                )
                .await;
        }
    }

    async fn cs_contract(
        self: &Arc<Self>,
        run: &str,
        action: ContractAction,
        by: &str,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        match action {
            ContractAction::Show { rev } => {
                let r = match rev {
                    Some(n) => set.contract.iter().find(|r| r.n == n).cloned(),
                    None => set.contract_head().cloned(),
                };
                match r {
                    Some(r) => Ok(json!(r)),
                    // Not submitted yet: the draft on disk.
                    None => Ok(json!({ "n": 0, "text": std::fs::read_to_string(Self::contract_path(&set)).unwrap_or_default(), "draft": true })),
                }
            }
            ContractAction::History => Ok(json!(set
                .contract
                .iter()
                .map(|r| json!({ "n": r.n, "at_ms": r.at_ms, "by": r.by, "affects": r.affects, "summary": r.summary, "approved": r.approved, "sha": r.sha }))
                .collect::<Vec<_>>())),
            ContractAction::Diff { a, b, file } => {
                let head = set.contract_head().ok_or_else(|| Error::invalid("no contract revision yet"))?;
                if let Some(f) = file {
                    let other = std::fs::read_to_string(&f).map_err(|e| Error::invalid(format!("{f}: {e}")))?;
                    let d = self.text_diff(&head.text, &other).await;
                    return Ok(json!({ "a": head.n, "b": f, "diff": d, "same": d.trim().is_empty() }));
                }
                let b = b.unwrap_or(head.n);
                let a = a.unwrap_or(b.saturating_sub(1));
                let text = |n: u32| set.contract.iter().find(|r| r.n == n).map(|r| r.text.clone());
                let ta = if a == 0 { String::new() } else { text(a).ok_or_else(|| Error::missing("revision", format!("r{a}")))? };
                let tb = text(b).ok_or_else(|| Error::missing("revision", format!("r{b}")))?;
                Ok(json!({ "a": a, "b": b, "diff": self.text_diff(&ta, &tb).await }))
            }
            ContractAction::Submit { summary } => {
                let text = std::fs::read_to_string(Self::contract_path(&set)).map_err(cs_err)?;
                if text.trim().is_empty() || text == cs::contract_skeleton(&set) {
                    return Err(Error::invalid("CONTRACT.md is still the skeleton: write the interfaces first"));
                }
                let rev = self.record_revision(run, text, by, vec![], summary).await?;
                Ok(json!({ "rev": rev.n, "affects": rev.affects, "gate": set.phase == Phase::Drafting && set.gated("contract") }))
            }
            ContractAction::Revise { text, diff, affects, summary } => {
                let current = std::fs::read_to_string(Self::contract_path(&set)).unwrap_or_default();
                let new = match (text, diff) {
                    (Some(t), _) => t,
                    (None, Some(d)) => apply_patch(&current, &d).await?,
                    (None, None) => current,
                };
                for a in &affects {
                    set.member(a).ok_or_else(|| Error::missing("member", a))?;
                }
                let rev = self.record_revision(run, new, by, affects, summary).await?;
                Ok(json!({ "rev": rev.n, "affects": rev.affects }))
            }
        }
    }

    // ── gates ──────────────────────────────────────────────────────────────

    /// Open a gate on a step of the set's run (`contract-r1`, `push`, `land-api`, `ask-…`).
    async fn cs_open_gate(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        ask: &str,
        options: Vec<String>,
    ) -> Gate {
        if let Some(g) = self
            .run_info(run)
            .and_then(|i| i.state.steps.get(step).and_then(|s| s.gate.clone()))
            .filter(|g| g.choice.is_none())
        {
            return g;
        }
        let id = loop {
            let id = ptyro_run::short_id("g", 4);
            if self.find_gate(&id).is_none() {
                break id;
            }
        };
        let gate =
            Gate { id, ask: ask.to_owned(), options, opened_ms: now_ms(), ..Gate::default() };
        self.emit(
            run,
            vec![
                RunEvent::StepAdded { step: step.into() },
                RunEvent::GateOpened { step: step.into(), gate: gate.clone() },
            ],
        );
        self.announce_gate(run, &gate).await;
        gate
    }

    /// Wait for a gate's answer (`None`: the run ended, or `cs rm` called it off).
    async fn wait_gate(self: &Arc<Self>, run: &str, step: &str) -> Option<String> {
        let set = self.set_state(run).ok().map(|s| s.name);
        self.waiting_at(set.as_deref(), Some(step));
        let r = self.wait_gate_inner(run, step, set.as_deref()).await;
        self.waiting_at(set.as_deref(), None);
        r
    }

    /// Note (or clear) the gate step the set's job waits at.
    fn waiting_at(&self, set: Option<&str>, step: Option<&str>) {
        if let Some(n) = set
            && let Some(l) = self.cs.locked().get_mut(n)
        {
            l.waiting_at = step.map(str::to_owned);
        }
    }

    async fn wait_gate_inner(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        set: Option<&str>,
    ) -> Option<String> {
        // A gate nobody answers must not hold the set's one job slot (and with it `cs push` and
        // `cs land`) for ever: a day, then it is as if the run had ended. `cs rm` calls it off
        // at once (H2.7).
        let give_up = now_ms().saturating_add(24 * 60 * 60 * 1000);
        loop {
            let info = self.run_info(run)?;
            if info.state.status.is_over() || now_ms() > give_up {
                return None;
            }
            let called_off =
                set.is_some_and(|n| self.cs.locked().get(n).is_some_and(|l| l.call_off));
            if called_off {
                self.emit(
                    run,
                    vec![RunEvent::StepEnded {
                        step: step.into(),
                        status: StepStatus::Cancelled,
                        reason: Some("the set is being removed".into()),
                        decided_by: Some("cs rm".into()),
                        at: now_ms(),
                    }],
                );
                return None;
            }
            if let Some(c) = info
                .state
                .steps
                .get(step)
                .and_then(|s| s.gate.as_ref())
                .and_then(|g| g.choice.clone())
            {
                self.emit(
                    run,
                    vec![RunEvent::StepEnded {
                        step: step.into(),
                        status: StepStatus::Succeeded,
                        reason: Some(c.clone()),
                        decided_by: Some("gate".into()),
                        at: now_ms(),
                    }],
                );
                return Some(c);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    // ── the actor ──────────────────────────────────────────────────────────

    pub(crate) fn start_cs_actor(self: &Arc<Self>, run: &str) {
        let Ok(set) = self.set_state(run) else { return };
        {
            let mut cs = self.cs.locked();
            let l = cs.entry(set.name.clone()).or_default();
            if l.actor {
                return;
            }
            l.actor = true;
        }
        let (me, run) = (Arc::clone(self), run.to_owned());
        tokio::spawn(async move {
            while let Some(info) = me.run_info(&run) {
                if info.state.status.is_over() {
                    break;
                }
                me.cs_tick(&run).await;
                let wake = me.runs.locked().get(&run).map(|e| Arc::clone(&e.wake));
                match wake {
                    Some(w) => {
                        let _ =
                            tokio::time::timeout(Duration::from_millis(1500), w.notified()).await;
                    }
                    None => break,
                }
            }
            if let Ok(set) = me.set_state(&run)
                && let Some(l) = me.cs.locked().get_mut(&set.name)
            {
                l.actor = false;
            }
        });
    }

    async fn cs_tick(self: &Arc<Self>, run: &str) {
        let Ok(set) = self.set_state(run) else { return };
        let Some(info) = self.run_info(run) else { return };
        let lead = info.state.lead.as_deref().and_then(|l| self.resolve(l).ok());
        let lead_busy =
            lead.as_ref().is_some_and(|s| s.is_live() && s.status() == Some(AgentStatus::Working));
        // 1. CONTRACT.md saved while its editor is idle → a revision.
        if matches!(set.phase, Phase::Drafting | Phase::Working) && !lead_busy {
            let path = Self::contract_path(&set);
            if let Ok(text) = std::fs::read_to_string(&path) {
                let changed = set.contract_head().is_none_or(|h| h.sha != sha_of(&text));
                let skeleton = text == cs::contract_skeleton(&set) || text.trim().is_empty();
                // r1 waits for the lead's first turn to end (or an explicit submit).
                let lead_spoke = lead.as_ref().is_none_or(|s| {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).turns > 0
                });
                let gate_open =
                    info.state.open_gates().iter().any(|(s, _)| s.starts_with("contract"));
                if changed && !skeleton && lead_spoke && !gate_open {
                    let by = if set.layout == Layout::Lead { "lead" } else { "user" };
                    if let Err(e) = self.record_revision(run, text, by, vec![], None).await {
                        tracing::debug!("contract revision: {}", e.message);
                    }
                }
            }
        }
        // 2. The contract gate's answer.
        if set.phase == Phase::Drafting {
            for (step, ss) in &info.state.steps {
                let Some(g) = &ss.gate else { continue };
                let Some(choice) = &g.choice else { continue };
                // Acted on already (its step ended), possibly before a restart.
                if !step.starts_with("contract-r")
                    || ss.status.is_over()
                    || !self.cs_mark(&set.name, &g.id)
                {
                    continue;
                }
                let n: u32 = step.trim_start_matches("contract-r").parse().unwrap_or(0);
                self.emit(
                    run,
                    vec![RunEvent::StepEnded {
                        step: step.clone(),
                        status: StepStatus::Succeeded,
                        reason: Some(choice.clone()),
                        decided_by: Some("gate".into()),
                        at: now_ms(),
                    }],
                );
                match choice.as_str() {
                    "approve" => {
                        self.approve_contract(run, n, g.by.as_deref().unwrap_or("user")).await
                    }
                    other => {
                        if let Some(l) = &info.state.lead {
                            let text = format!(
                                "The human answered {other:?} to contract r{n}{}. Revise CONTRACT.md; the next save goes back to them.",
                                g.note.as_ref().map(|x| format!(": {x}")).unwrap_or_default()
                            );
                            let _ = self
                                .post(
                                    Some("@user"),
                                    l,
                                    "answer",
                                    &text,
                                    Some(run.to_owned()),
                                    None,
                                    None,
                                    None,
                                    Some(g.id.clone()),
                                )
                                .await;
                        }
                    }
                }
            }
        }
        // The lead's own questions: the answer goes back to it.
        for (step, ss) in &info.state.steps {
            let Some(g) = &ss.gate else { continue };
            let (Some(choice), true) = (&g.choice, step.starts_with("ask-")) else { continue };
            if ss.status.is_over() || !self.cs_mark(&set.name, &g.id) {
                continue;
            }
            self.emit(
                run,
                vec![RunEvent::StepEnded {
                    step: step.clone(),
                    status: StepStatus::Succeeded,
                    reason: Some(choice.clone()),
                    decided_by: Some("gate".into()),
                    at: now_ms(),
                }],
            );
            if let Some(l) = &info.state.lead {
                let text = format!(
                    "The human answered {choice:?} to: {}{}",
                    g.ask,
                    g.note.as_ref().map(|x| format!(" ({x})")).unwrap_or_default()
                );
                let _ = self
                    .post(
                        Some("@user"),
                        l,
                        "answer",
                        &text,
                        Some(run.to_owned()),
                        None,
                        None,
                        None,
                        Some(g.id.clone()),
                    )
                    .await;
            }
        }
        // 3. Members: errors, exits, waiting mail, out-of-fence writes.
        let now = now_ms();
        for m in &set.members {
            let Some(s) = m.session.as_deref().and_then(|x| self.resolve(x).ok()) else { continue };
            let live = s.is_live();
            let status = s.status();
            if live && status == Some(AgentStatus::Working) {
                self.cs
                    .locked()
                    .entry(set.name.clone())
                    .or_default()
                    .last_work
                    .insert(m.name.clone(), now);
            }
            let error = if live && status == Some(AgentStatus::Errored) {
                Some(
                    s.engine
                        .locked()
                        .record
                        .as_ref()
                        .and_then(|r| r.detail.clone())
                        .unwrap_or_else(|| "its agent errored".into()),
                )
            } else if !live {
                match &s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state {
                    ProcessState::Exited { code: Some(c), .. } if *c != 0 => {
                        Some(format!("its agent exited with {c}"))
                    }
                    ProcessState::Failed { error } => Some(error.clone()),
                    _ => None,
                }
            } else {
                None
            };
            match (&m.failed, error) {
                (None, Some(e)) => {
                    self.cs_emit(
                        run,
                        vec![CsEvent::Failed { name: m.name.clone(), error: Some(e.clone()) }],
                    );
                    self.notify_run(
                        run,
                        AttentionKind::Errored,
                        format!("{}: {} failed", set.name, m.name),
                        format!("{e}\nptyro cs status {}", set.name),
                    )
                    .await;
                    if let Some(l) = &info.state.lead {
                        let _ = self
                            .post(
                                Some("@ptyro"),
                                l,
                                "heartbeat",
                                &format!("{} failed: {e}", m.name),
                                Some(run.to_owned()),
                                None,
                                None,
                                None,
                                Some(format!("failed-{}-{now}", m.name)),
                            )
                            .await;
                    }
                }
                (Some(_), None)
                    if live
                        && matches!(
                            status,
                            Some(AgentStatus::Working | AgentStatus::Idle | AgentStatus::Done)
                        ) =>
                {
                    self.cs_emit(run, vec![CsEvent::Failed { name: m.name.clone(), error: None }]);
                }
                _ => {}
            }
        }
        // Mail waiting for an agent that exited cleanly: bring it back (once a minute at most).
        let waiting: BTreeSet<String> = self
            .mail
            .locked()
            .iter()
            .filter(|x| x.run.as_deref() == Some(run) && x.delivered_ms.is_none())
            .map(|x| x.recipient.clone())
            .collect();
        for sid in waiting {
            let Ok(s) = self.resolve(&sid) else { continue };
            if s.is_live() || s.kind() != SessionKind::Agent {
                continue;
            }
            let clean = matches!(
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state,
                ProcessState::Exited { code: Some(0) | None, .. }
            );
            let recent = self
                .cs
                .locked()
                .get(&set.name)
                .and_then(|l| l.resumed.get(&sid).copied())
                .is_some_and(|t| now.saturating_sub(t) < 60_000);
            if clean && !recent {
                self.cs
                    .locked()
                    .entry(set.name.clone())
                    .or_default()
                    .resumed
                    .insert(sid.clone(), now);
                if let Err(e) = self.resume(&sid).await {
                    tracing::debug!("resume for mail: {}", e.message);
                }
            }
        }
        // 4. The lead's turns: what changed in members while their own agents were idle.
        if set.layout == Layout::Lead
            && let Some(l) = &lead
        {
            let busy = l.is_live() && l.status() == Some(AgentStatus::Working);
            let open = self.cs.locked().get(&set.name).and_then(|x| x.lead_turn.clone());
            match (busy, open) {
                (true, None) => {
                    let fp = fingerprints(&set).await;
                    self.cs.locked().entry(set.name.clone()).or_default().lead_turn =
                        Some((now, fp));
                }
                (false, Some((start, before))) => {
                    let after = fingerprints(&set).await;
                    let mut cs = self.cs.locked();
                    let l = cs.entry(set.name.clone()).or_default();
                    l.lead_turn = None;
                    for m in &set.members {
                        let own = l.last_work.get(&m.name).is_some_and(|t| *t >= start);
                        if !own && before.get(&m.name) != after.get(&m.name) {
                            let v = l.out_of_fence.entry(m.name.clone()).or_default();
                            let what = "changed during the lead's turn (the lead may not edit or commit member code)".to_owned();
                            if !v.contains(&what) {
                                v.push(what);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // 5. Merges made outside the train (every 5 s).
        if matches!(set.phase, Phase::Working | Phase::Landing) {
            let due = {
                let mut cs = self.cs.locked();
                let l = cs.entry(set.name.clone()).or_default();
                let due = now.saturating_sub(l.last_merge_scan) > 5_000;
                if due {
                    l.last_merge_scan = now;
                }
                due
            };
            if due && self.cs.locked().get(&set.name).is_some_and(|l| l.job.is_none()) {
                self.scan_merges(run).await;
            }
        }
        if set.phase != Phase::Landed
            && !set.members.is_empty()
            && set.members.iter().all(|m| m.landed.is_some())
        {
            self.cs_emit(run, vec![CsEvent::Phase { phase: Phase::Landed }]);
            self.finish_run(run, RunStatus::Succeeded, None).await;
        }
    }

    /// Interrupt mail for a working agent with no inbox socket: stop its turn (the adapter's
    /// interrupt keys, Esc for most); the mailbox types the message once it is idle. Only for
    /// change sets, whose interrupts are for work in flight that is now wrong (SPEC §15.5.4).
    pub(crate) fn cs_interrupt(&self, s: &Arc<Session>, msgs: &[ptyro_store::MessageRow]) {
        let Some(run) =
            msgs.iter().filter(|m| m.priority == "interrupt").find_map(|m| m.run.clone())
        else {
            return;
        };
        let Ok(set) = self.set_state(&run) else { return };
        let sid = s.id.full();
        let now = now_ms();
        {
            let mut cs = self.cs.locked();
            let l = cs.entry(set.name.clone()).or_default();
            if l.interrupted.get(&sid).is_some_and(|t| now.saturating_sub(*t) < 15_000) {
                return;
            }
            l.interrupted.insert(sid, now);
        }
        let Some(adapter) = s.adapter() else { return };
        let keys = ptyro_agents::adapter(&adapter).stop().into_iter().find_map(|st| match st {
            ptyro_agents::StopStep::InterruptKeys(k) => Some(k),
            _ => None,
        });
        if let Some(k) = keys {
            s.write_raw(&k);
            s.record_event("interrupt", json!({ "set": set.name, "for": "mail" }));
        }
    }

    /// Remember a gate as handled; `false` if it already was.
    fn cs_mark(&self, set: &str, gate: &str) -> bool {
        self.cs.locked().entry(set.to_owned()).or_default().handled.insert(gate.to_owned())
    }

    /// A member merged into its base by someone else: landed, and out of order if a dependency
    /// hasn't.
    async fn scan_merges(self: &Arc<Self>, run: &str) {
        let Ok(set) = self.set_state(run) else { return };
        let mut merged: Vec<(String, String)> = Vec::new();
        for m in set.members.iter().filter(|m| m.landed.is_none()) {
            let Some(wt) = m.worktree.as_deref().and_then(|w| self.worktree_by_id(w)) else {
                continue;
            };
            if let Some(pr) = wt.pr.as_ref().filter(|p| p.state == "MERGED") {
                merged
                    .push((m.name.clone(), format!("PR #{} merged outside the train", pr.number)));
                continue;
            }
            let base = base_branch(m.base.as_deref(), None);
            let Some(start) = m.start.clone() else { continue };
            let (path, branch, base_c) = (wt.path.clone(), m.branch.clone(), base.clone());
            // Merged: the branch gained commits, and they're all in the base.
            let is_merged = blocking(move || {
                let g = ptyro_git::Git::new();
                let Some(head) = g.rev(&path, &branch) else { return false };
                let Some(b) =
                    g.rev(&path, &format!("origin/{base_c}")).or_else(|| g.rev(&path, &base_c))
                else {
                    return false;
                };
                head != start && g.is_ancestor(&path, &head, &b)
            })
            .await
            .unwrap_or(false);
            if is_merged {
                merged.push((m.name.clone(), format!("merged into {base} outside the train")));
            }
        }
        for (name, how) in merged {
            let set = match self.set_state(run) {
                Ok(s) => s,
                Err(_) => return,
            };
            let pending: Vec<String> = set
                .member(&name)
                .map(|m| {
                    m.depends_on
                        .iter()
                        .filter(|d| set.member(d).is_some_and(|x| x.landed.is_none()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let mut evs = vec![CsEvent::Landed { name: name.clone(), how: how.clone() }];
            if !pending.is_empty() {
                evs.push(CsEvent::OutOfOrder {
                    name: name.clone(),
                    detail: Some(format!("was merged before {} ({how})", pending.join(", "))),
                });
            }
            self.cs_emit(run, evs);
            if !pending.is_empty() {
                self.notify_run(
                    run,
                    AttentionKind::Errored,
                    format!("{}: {name} merged out of order", set.name),
                    format!(
                        "{name} merged before {}\nptyro cs status {}",
                        pending.join(", "),
                        set.name
                    ),
                )
                .await;
            }
        }
    }

    // ── fences ─────────────────────────────────────────────────────────────

    /// Should a write to `files` by this session be denied (its role's fence)? The reason, if
    /// so. Relative paths are taken from the session's directory.
    pub(crate) fn cs_fence(&self, s: &Arc<Session>, files: &[String]) -> Option<String> {
        if files.is_empty() {
            return None;
        }
        let (run, role) = self.set_of_session(&s.id.full())?;
        let set = self.set_state(&run).ok()?;
        let cwd = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cwd.clone();
        let root = real_path(Path::new(&set.root));
        let tmp = real_path(&std::env::temp_dir());
        let homes: Vec<(String, PathBuf)> =
            set.members.iter().map(|m| (m.name.clone(), real_path(Path::new(&m.path)))).collect();
        for f in files {
            let p = PathBuf::from(f);
            // Where the write really lands: symlinks resolved (the node overlay links a
            // member's node_modules/<pkg> into its sibling).
            let abs = real_path(&normalize(&if p.is_absolute() { p } else { cwd.join(p) }));
            // Scratch files in the temp directory are fine, unless that is where the set lives.
            if abs.starts_with(&tmp)
                && !abs.starts_with(&root)
                && !homes.iter().any(|(_, h)| abs.starts_with(h))
            {
                continue;
            }
            let owner = homes.iter().find(|(_, h)| abs.starts_with(h)).map(|(n, _)| n.clone());
            let rel = abs
                .strip_prefix(&root)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_else(|_| abs.to_string_lossy().into_owned());
            let ok = match (&role, &owner) {
                (Role::Lead, None) if set.layout == Layout::Solo => abs.starts_with(&root),
                (Role::Lead, Some(_)) if set.layout == Layout::Solo => true,
                // The lead owns the root's files (CONTRACT.md, CHANGESET.md, notes), never members'.
                (Role::Lead, None) => {
                    abs.starts_with(&root) && !abs.starts_with(root.join(".ptyro"))
                }
                (Role::Lead, Some(_)) => false,
                (Role::Member(me), Some(o)) => me == o,
                (Role::Member(_), None) => false,
                _ => true,
            };
            if ok {
                continue;
            }
            let who = match &role {
                Role::Lead => "the lead".to_owned(),
                Role::Member(m) => m.clone(),
                _ => "this agent".to_owned(),
            };
            return Some(match (&role, owner) {
                (Role::Lead, Some(o)) => format!(
                    "ptyro: {who} of change set {} may not edit {rel} ({o}'s code). Ask {o}: ptyro cs msg {} {o} \"…\" (or revise the contract)",
                    set.name, set.name
                ),
                (_, Some(o)) => format!(
                    "ptyro: {who} may not edit {rel}: it belongs to {o}. Ask the lead: ptyro cs msg {} @lead \"…\"",
                    set.name
                ),
                (Role::Member(_), None) if rel == "CONTRACT.md" => format!(
                    "ptyro: the contract is the lead's. Ask for a change: ptyro cs msg {} @lead --kind contract_change \"…\"",
                    set.name
                ),
                _ => format!(
                    "ptyro: {who} of change set {} may only edit its own directory, not {rel}. Ask the lead: ptyro cs msg {} @lead \"…\"",
                    set.name, set.name
                ),
            });
        }
        None
    }

    // ── diff, checks ───────────────────────────────────────────────────────

    async fn cs_diff(self: &Arc<Self>, run: &str, member: &str) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let m = set.member(member).ok_or_else(|| Error::missing("member", member))?;
        let wt = m
            .worktree
            .as_deref()
            .and_then(|w| self.worktree_by_id(w))
            .ok_or_else(|| Error::missing("worktree", member))?;
        let base = self.diff_base(&wt).await?;
        let path = wt.path.clone();
        let text = blocking(move || {
            let g = ptyro_git::Git::new();
            let snap = g.snapshot(&path).unwrap_or_else(|| "HEAD".into());
            g.diff_between(&path, &base, &snap, false, &[]).unwrap_or_default()
        })
        .await?;
        let stat: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with("diff --git"))
            .map(|l| l.rsplit(" b/").next().unwrap_or(l).to_owned())
            .collect();
        let max = 60_000;
        Ok(json!({
            "member": member,
            "files": stat,
            "diff": if text.len() > max { format!("{}\n… ({} more bytes)", &text[..text.floor_char_boundary(max)], text.len() - max) } else { text },
        }))
    }

    async fn run_cmd(
        &self,
        dir: &Path,
        cmd: &str,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> CheckResult {
        let started = now_ms();
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd).current_dir(dir).env_clear().envs(env);
        // Its output goes to a file as it is written (never held in memory), and the group is
        // stopped on a timeout or when it writes too much.
        let log =
            self.paths.state.join(format!("cs-check-{}.log", ptyro_agents::adapter::random_hex()));
        let ran = crate::verify::run_to_log(
            c,
            &log,
            Duration::from_secs(30 * 60),
            crate::verify::CHECK_LOG_CAP,
        )
        .await;
        let _ = std::fs::remove_file(&log);
        let (ok, code, text) = match ran {
            Ok(r) => match r.stopped {
                Some(why) => (false, None, format!("{why}\n{}", r.text)),
                None => (r.success, r.code, r.text),
            },
            Err(e) => (false, None, e.to_string()),
        };
        CheckResult {
            command: cmd.to_owned(),
            ok,
            exit_code: code,
            tail: crate::runs::tail(&text, 40),
            at_ms: now_ms(),
            duration_ms: now_ms().saturating_sub(started),
        }
    }

    async fn set_env(
        self: &Arc<Self>,
        set: &ChangeSet,
    ) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut env = match self.profile_plan(&set.profile).await {
            Ok(plan) => plan.apply(self.base_env.iter().cloned()),
            Err(_) => self.base_env.clone(),
        };
        env.push(("PTYRO_CS".into(), set.name.clone().into()));
        env.push(("PTYRO_CS_ROOT".into(), set.root.clone().into()));
        env
    }

    /// Integration checks in the root, then each member's contract check.
    async fn cs_check(self: &Arc<Self>, run: &str) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let env = self.set_env(&set).await;
        let mut integration = Vec::new();
        for c in &set.integration {
            integration.push(self.run_cmd(Path::new(&set.root), c, env.clone()).await);
        }
        self.cs_emit(run, vec![CsEvent::Checked { name: None, results: integration.clone() }]);
        let mut members = BTreeMap::new();
        for m in &set.members {
            let Some(c) = &m.contract_check else { continue };
            let r = self.run_cmd(Path::new(&m.path), c, env.clone()).await;
            self.cs_emit(
                run,
                vec![CsEvent::Checked { name: Some(m.name.clone()), results: vec![r.clone()] }],
            );
            members.insert(m.name.clone(), r);
        }
        let ok = integration.iter().all(|c| c.ok) && members.values().all(|c| c.ok);
        if !ok && let Some(l) = self.run_info(run).and_then(|i| i.state.lead) {
            let failed: Vec<String> = integration
                .iter()
                .chain(members.values())
                .filter(|c| !c.ok)
                .map(|c| format!("`{}` (exit {:?})\n{}", c.command, c.exit_code, c.tail))
                .collect();
            let _ = self
                .post(
                    Some("@ptyro"),
                    &l,
                    "heartbeat",
                    &format!("Checks failed:\n{}", failed.join("\n\n")),
                    Some(run.to_owned()),
                    None,
                    None,
                    None,
                    None,
                )
                .await;
        }
        Ok(json!({ "ok": ok, "integration": integration, "members": members }))
    }

    // ── checkpoints ────────────────────────────────────────────────────────

    async fn cs_checkpoint(
        self: &Arc<Self>,
        run: &str,
        label: Option<String>,
        reason: &str,
    ) -> Result<SetCheckpoint, Error> {
        let set = self.set_state(run)?;
        let n = set.checkpoints.iter().map(|c| c.n).max().unwrap_or(0) + 1;
        let mut members = BTreeMap::new();
        for m in &set.members {
            let (path, name, setname) = (PathBuf::from(&m.path), m.name.clone(), set.name.clone());
            let label = label.clone().unwrap_or_default();
            let r = blocking(move || -> Result<(String, String), Error> {
                let g = ptyro_git::Git::new();
                let head = g.rev(&path, "HEAD").unwrap_or_default();
                let msg = format!("ptyro change-set checkpoint {n} of {setname} ({name})\n\nset: {setname}\nmember: {name}\nlabel: {label}\nat: {}\n", now_ms());
                let snap = g.checkpoint(&path, &msg).map_err(crate::registry::git_err)?;
                g.update_ref(&path, &format!("refs/ptyro/cs/{setname}/{n}"), &snap.commit).map_err(crate::registry::git_err)?;
                Ok((head, snap.commit))
            })
            .await??;
            members.insert(m.name.clone(), r);
        }
        let ck = SetCheckpoint { n, label, at_ms: now_ms(), reason: reason.to_owned(), members };
        self.cs_emit(run, vec![CsEvent::Checkpoint { ckpt: Box::new(ck.clone()) }]);
        Ok(ck)
    }

    async fn cs_restore(self: &Arc<Self>, run: &str, n: u32, force: bool) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let ck = set
            .checkpoints
            .iter()
            .find(|c| c.n == n)
            .cloned()
            .ok_or_else(|| Error::missing("set checkpoint", format!("#{n}")))?;
        let busy: Vec<String> = set
            .members
            .iter()
            .filter(|m| {
                m.session
                    .as_deref()
                    .and_then(|s| self.resolve(s).ok())
                    .is_some_and(|s| s.is_live() && s.status() == Some(AgentStatus::Working))
            })
            .map(|m| m.name.clone())
            .collect();
        if !busy.is_empty() {
            return Err(Error::conflict(format!(
                "agents are working in {}: wait for their turns to end (or stop them)",
                busy.join(", ")
            )));
        }
        // Nothing newer may be lost: every member's present must match a recorded checkpoint.
        let mut unrecorded = Vec::new();
        for m in &set.members {
            let known: Vec<String> = set
                .checkpoints
                .iter()
                .filter_map(|c| c.members.get(&m.name).map(|x| x.1.clone()))
                .collect();
            let path = PathBuf::from(&m.path);
            let lost = blocking(move || {
                let g = ptyro_git::Git::new();
                let Ok(now) = g.checkpoint(&path, "ptyro restore check") else { return true };
                !known.iter().any(|k| g.tree_of(&path, k).as_deref() == Some(now.tree.as_str()))
            })
            .await?;
            if lost {
                unrecorded.push(m.name.clone());
            }
        }
        let mut notes = Vec::new();
        if !unrecorded.is_empty() {
            if !force {
                return Err(Error::new(
                    ErrorCode::DirtyWorktree,
                    format!(
                        "{} changed since the last set checkpoint: restoring would lose that. `ptyro cs checkpoint {}` first, or --force (which checkpoints the present first)",
                        unrecorded.join(", "),
                        set.name
                    ),
                ));
            }
            let safety =
                self.cs_checkpoint(run, Some(format!("before restoring #{n}")), "restore").await?;
            notes.push(format!("the present is kept as set checkpoint #{}", safety.n));
        }
        for m in &set.members {
            let Some((_, snap)) = ck.members.get(&m.name).cloned() else {
                notes.push(format!("{} joined after #{n}; left as it is", m.name));
                continue;
            };
            let path = PathBuf::from(&m.path);
            blocking(move || ptyro_git::Git::new().restore(&path, &snap))
                .await?
                .map_err(crate::registry::git_err)?;
            if let Some(w) = &m.worktree {
                let _ = self.refresh_status(w).await;
            }
        }
        notes.insert(0, format!("restored every member of {} to set checkpoint #{n}", set.name));
        self.cs_note(run, notes.join("; "));
        // The agents' view of their worktree just changed.
        for m in &set.members {
            if let Some(sid) = &m.session {
                let _ = self.post(Some("@ptyro"), sid, "heartbeat", &format!("Your directory was restored to set checkpoint #{n} by the human. Look at `git status` before you go on."), Some(run.to_owned()), None, None, None, None).await;
            }
        }
        Ok(json!({ "notes": notes }))
    }

    // ── overlays (M11.7) ───────────────────────────────────────────────────

    async fn cs_overlay(
        self: &Arc<Self>,
        run: &str,
        member: Option<&str>,
        mode: &str,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let targets: Vec<String> = match member {
            Some(m) => vec![set.member(m).ok_or_else(|| Error::missing("member", m))?.name.clone()],
            None => set.members.iter().map(|m| m.name.clone()).collect(),
        };
        let manifests = self.set_manifests(&set).await;
        for t in &targets {
            let kind = match mode {
                "off" => String::new(),
                "auto" => cs::suggest_overlay(t, &manifests).unwrap_or_default(),
                k if cs::OVERLAYS.contains(&k) => k.to_owned(),
                k => {
                    return Err(Error::invalid(format!(
                        "overlay {k:?}: {}, auto or off",
                        cs::OVERLAYS.join(", ")
                    )));
                }
            };
            self.cs_emit(
                run,
                vec![CsEvent::MemberInfo {
                    name: t.clone(),
                    worktree: None,
                    path: None,
                    branch: None,
                    session: None,
                    overlay: Some(kind),
                    start: None,
                }],
            );
        }
        let notes = self.apply_overlays(run).await;
        self.write_brief(&self.set_state(run)?);
        Ok(json!({ "notes": notes }))
    }

    async fn set_manifests(&self, set: &ChangeSet) -> BTreeMap<String, cs::Manifest> {
        let roots: Vec<(String, PathBuf)> =
            set.members.iter().map(|m| (m.name.clone(), PathBuf::from(&m.path))).collect();
        blocking(move || roots.into_iter().map(|(n, r)| (n, read_manifest(&r))).collect())
            .await
            .unwrap_or_default()
    }

    /// Write (or remove) every generated overlay file. Nothing lands in a repository: root files
    /// live outside every member, and files inside a member are git-excluded.
    async fn apply_overlays(self: &Arc<Self>, run: &str) -> Vec<String> {
        let Ok(set) = self.set_state(run) else { return vec![] };
        let manifests = self.set_manifests(&set).await;
        let set2 = set.clone();
        match blocking(move || overlay_files(&set2, &manifests)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) | Err(e) => vec![e.message],
        }
    }

    // ── lockstep git (M11.9) ───────────────────────────────────────────────

    fn member_wt(&self, m: &Member) -> Result<WorktreeInfo, Error> {
        m.worktree
            .as_deref()
            .and_then(|w| self.worktree_by_id(w))
            .ok_or_else(|| Error::missing("worktree", &m.name))
    }

    async fn cs_commit(self: &Arc<Self>, run: &str, message: &str) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        if message.trim().is_empty() {
            return Err(Error::invalid("a commit needs a message"));
        }
        let order = set.order().map_err(|c| Error::invalid(format!("cycle: {}", c.join(" → "))))?;
        let mut before: Vec<(String, String)> = Vec::new();
        let mut done = Vec::new();
        for name in &order {
            let Some(m) = set.member(name) else { continue };
            let wt = self.member_wt(m)?;
            let env = self.tool_env(&wt).await?;
            let path = wt.path.clone();
            let msg = format!("{}\n\n{}", message.trim(), cs::trailers(&set.name, &before));
            let r = blocking(move || {
                let g = ptyro_git::Git::with_env(env);
                if !g.dirty(&path) {
                    return Ok(None);
                }
                g.commit(&path, &msg, &[], true).map(Some)
            })
            .await?;
            match r {
                Ok(Some(sha)) => {
                    self.cs_emit(
                        run,
                        vec![CsEvent::Committed { name: name.clone(), sha: sha.clone() }],
                    );
                    before.push((name.clone(), sha.clone()));
                    done.push(json!({ "member": name, "sha": sha }));
                    let _ = self.refresh_status(&wt.id).await;
                }
                Ok(None) => {
                    // Clean: its HEAD is still what the next members were built against.
                    if let Some(h) = blocking({
                        let p = wt.path.clone();
                        move || ptyro_git::Git::new().rev(&p, "HEAD")
                    })
                    .await?
                    {
                        before.push((name.clone(), h));
                    }
                }
                Err(e) => {
                    return Err(Error::conflict(format!(
                        "committing {name} failed (members before it are committed: {}): {e}",
                        done.len()
                    )));
                }
            }
        }
        if done.is_empty() {
            return Err(Error::conflict("nothing to commit in any member"));
        }
        Ok(json!({ "committed": done }))
    }

    async fn cs_push_request(self: &Arc<Self>, run: &str, yes: bool) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        self.claim_job(&set.name, "push")?;
        if set.gated("push") && !yes {
            let gate = self
                .cs_open_gate(
                    run,
                    &format!("push-{}", now_ms()),
                    &format!(
                        "Push {}'s {} members in dependency order?",
                        set.name,
                        set.members.len()
                    ),
                    vec!["push".into(), "cancel".into()],
                )
                .await;
            let (me, run2, name) = (Arc::clone(self), run.to_owned(), set.name.clone());
            let step = self
                .run_info(run)
                .and_then(|i| {
                    i.state
                        .steps
                        .iter()
                        .find(|(_, s)| s.gate.as_ref().is_some_and(|g| g.id == gate.id))
                        .map(|(k, _)| k.clone())
                })
                .unwrap_or_default();
            tokio::spawn(async move {
                if me.wait_gate(&run2, &step).await.as_deref() == Some("push") {
                    let _ = me.cs_push(&run2).await;
                }
                me.release_job(&name);
            });
            return Ok(json!({ "gate": gate.id, "waiting": true }));
        }
        let r = self.cs_push(run).await;
        self.release_job(&set.name);
        r
    }

    fn claim_job(&self, set: &str, what: &str) -> Result<(), Error> {
        let mut cs = self.cs.locked();
        let l = cs.entry(set.to_owned()).or_default();
        if let Some(j) = &l.job {
            return Err(Error::conflict(format!("{set} is busy: {j} in progress")));
        }
        l.job = Some(what.to_owned());
        Ok(())
    }

    fn release_job(&self, set: &str) {
        if let Some(l) = self.cs.locked().get_mut(set) {
            l.job = None;
            l.call_off = false;
        }
    }

    /// `cs rm` needs the set's job slot: a push or land only waiting at a gate is called off (its
    /// gate closes, cancelled) and the slot taken once it lets go; one actually running still
    /// refuses (it would merge into worktrees being removed).
    async fn claim_for_rm(&self, set: &str) -> Result<(), Error> {
        let waiting = {
            let mut cs = self.cs.locked();
            let l = cs.entry(set.to_owned()).or_default();
            let waiting = l.job.is_some() && l.waiting_at.is_some();
            if waiting {
                l.call_off = true;
            }
            waiting
        };
        if !waiting {
            return self.claim_job(set, "rm");
        }
        let deadline = now_ms() + 5_000;
        loop {
            match self.claim_job(set, "rm") {
                Ok(()) => return Ok(()),
                Err(e) if now_ms() > deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Push every member, dependencies first. A failure stops the train and says exactly who
    /// pushed; running it again resumes (members already pushed are skipped).
    async fn cs_push(self: &Arc<Self>, run: &str) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let order = set.order().map_err(|c| Error::invalid(format!("cycle: {}", c.join(" → "))))?;
        let mut pushed = Vec::new();
        let mut skipped = Vec::new();
        for name in &order {
            let Some(m) = set.member(name) else { continue };
            let wt = self.member_wt(m)?;
            let env = self.tool_env(&wt).await?;
            let (path, branch) = (wt.path.clone(), m.branch.clone());
            let r = blocking(move || -> Result<Option<String>, String> {
                let g = ptyro_git::Git::with_env(env);
                let Some(remote) = g.main_remote(&path).map(|r| r.name) else {
                    return Err("no-remote".into());
                };
                let head = g.rev(&path, "HEAD").ok_or("no HEAD")?;
                if g.rev(&path, &format!("{remote}/{branch}")).as_deref() == Some(head.as_str()) {
                    return Ok(None);
                }
                g.push(&path, &remote, &branch).map_err(|e| e.to_string())?;
                Ok(Some(head))
            })
            .await?;
            match r {
                Ok(Some(sha)) => {
                    self.cs_emit(
                        run,
                        vec![CsEvent::Pushed { name: name.clone(), sha: sha.clone() }],
                    );
                    pushed.push(name.clone());
                    let _ = self.refresh_status(&wt.id).await;
                }
                Ok(None) => skipped.push(name.clone()),
                Err(e) => {
                    if e == "no-remote" {
                        self.cs_emit(run, vec![CsEvent::NoRemote { name: name.clone(), on: true }]);
                    }
                    let not: Vec<String> =
                        order.iter().skip_while(|x| *x != name).cloned().collect();
                    let report = format!(
                        "push stopped at {name}: {}. Pushed: {}; already up to date: {}; not pushed: {}. Fix it and run `ptyro cs push {}` again to resume.",
                        if e == "no-remote" {
                            "the repository has no remote".to_owned()
                        } else {
                            e
                        },
                        if pushed.is_empty() { "none".into() } else { pushed.join(", ") },
                        if skipped.is_empty() { "none".into() } else { skipped.join(", ") },
                        not.join(", "),
                        set.name
                    );
                    self.cs_note(run, report.clone());
                    return Err(Error::conflict(report));
                }
            }
        }
        self.cs_note(
            run,
            format!(
                "pushed {} (up to date: {})",
                if pushed.is_empty() { "nothing".into() } else { pushed.join(", ") },
                if skipped.is_empty() { "none".into() } else { skipped.join(", ") }
            ),
        );
        Ok(json!({ "pushed": pushed, "up_to_date": skipped }))
    }

    /// One PR per member (dependencies first), each carrying the sibling table.
    async fn cs_pr(self: &Arc<Self>, run: &str, draft: bool) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let order = set.order().map_err(|c| Error::invalid(format!("cycle: {}", c.join(" → "))))?;
        let mut opened = Vec::new();
        for name in &order {
            let set = self.set_state(run)?;
            let Some(m) = set.member(name) else { continue };
            if m.pr.is_some() {
                continue;
            }
            let wt = self.member_wt(m)?;
            let body = format!(
                "{}\n\n{}",
                set.goal.clone().unwrap_or_else(|| format!("Part of change set `{}`.", set.name)),
                cs::pr_table(&set, name, &BTreeMap::new())
            );
            let title = set.goal.as_deref().and_then(|g| g.lines().next()).map_or_else(
                || format!("{} ({name})", set.name.replace('-', " ")),
                |g| format!("{g} ({name})"),
            );
            match self
                .pr_create(
                    &wt.id,
                    draft,
                    Some(title),
                    Some(body),
                    Some(base_branch(m.base.as_deref(), None)),
                )
                .await
            {
                Ok(Reply::Pr { info: Some(p) }) => {
                    self.cs_emit(
                        run,
                        vec![
                            CsEvent::Pr {
                                name: name.clone(),
                                number: p.number,
                                url: p.url.clone(),
                            },
                            CsEvent::Pushed { name: name.clone(), sha: String::new() },
                        ],
                    );
                    opened.push(json!({ "member": name, "number": p.number, "url": p.url }));
                }
                Ok(_) => {}
                Err(e) => {
                    let msg = format!(
                        "opening {name}'s PR failed: {} (opened before it: {})",
                        e.message,
                        opened.len()
                    );
                    self.cs_note(run, msg.clone());
                    return Err(Error::new(e.code, msg));
                }
            }
        }
        self.refresh_pr_tables(run).await;
        Ok(json!({ "opened": opened }))
    }

    /// Rewrite the sibling table in every member's PR body (CI states, merged).
    pub(crate) async fn refresh_pr_tables(self: &Arc<Self>, run: &str) {
        let Ok(set) = self.set_state(run) else { return };
        let mut ci = BTreeMap::new();
        for m in &set.members {
            let Some(wt) = m.worktree.as_deref().and_then(|w| self.worktree_by_id(w)) else {
                continue;
            };
            let state = wt.pr.as_ref().map(|p| {
                if p.checks.is_empty() {
                    "none"
                } else if p.checks.iter().any(|c| c.bucket == "fail" || c.bucket == "cancel") {
                    "failing"
                } else if p.checks.iter().any(|c| c.bucket == "pending") {
                    "pending"
                } else {
                    "passing"
                }
            });
            if let Some(s) = state {
                ci.insert(m.name.clone(), s.to_owned());
            }
        }
        for m in &set.members {
            let (Some(n), Some(wt)) =
                (m.pr, m.worktree.as_deref().and_then(|w| self.worktree_by_id(w)))
            else {
                continue;
            };
            let Ok(out) = self
                .gh(
                    &wt,
                    vec!["pr".into(), "view".into(), n.to_string(), "--json".into(), "body".into()],
                )
                .await
            else {
                continue;
            };
            let body = serde_json::from_str::<Value>(&out)
                .ok()
                .and_then(|v| v["body"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let new = cs::with_table(&body, &cs::pr_table(&set, &m.name, &ci));
            if new != body {
                let _ = self
                    .gh(&wt, vec!["pr".into(), "edit".into(), n.to_string(), "--body".into(), new])
                    .await;
            }
        }
    }

    async fn cs_land_request(self: &Arc<Self>, run: &str, yes: bool) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        if set.phase == Phase::Landed {
            return Err(Error::conflict(format!("{} has landed already", set.name)));
        }
        let order = set.order().map_err(|c| Error::invalid(format!("cycle: {}", c.join(" → "))))?;
        self.claim_job(&set.name, "land")?;
        let todo: Vec<String> = order
            .iter()
            .filter(|n| set.member(n).is_some_and(|m| m.landed.is_none()))
            .cloned()
            .collect();
        let train = Train {
            order: order.clone(),
            done: set
                .members
                .iter()
                .filter(|m| m.landed.is_some())
                .map(|m| m.name.clone())
                .collect(),
            stopped: None,
            started_ms: now_ms(),
        };
        self.cs_emit(
            run,
            vec![CsEvent::Train { train: Some(train) }, CsEvent::Phase { phase: Phase::Landing }],
        );
        let (me, run2, name) = (Arc::clone(self), run.to_owned(), set.name.clone());
        tokio::spawn(async move {
            let r = me.cs_land(&run2, yes).await;
            if let Err((member, why)) = r {
                if let Ok(s) = me.set_state(&run2)
                    && let Some(mut t) = s.train.clone()
                {
                    t.stopped = Some((member.clone(), why.clone()));
                    me.cs_emit(
                        &run2,
                        vec![
                            CsEvent::Train { train: Some(t) },
                            CsEvent::Phase { phase: Phase::Working },
                        ],
                    );
                }
                let report = me
                    .set_state(&run2)
                    .ok()
                    .map(|s| s.report(&BTreeMap::new(), false))
                    .and_then(|r| r.issues.into_iter().find(|i| i.state == "land_stopped"))
                    .map(|i| i.detail)
                    .unwrap_or(why);
                me.cs_note(&run2, report.clone());
                me.notify_run(
                    &run2,
                    AttentionKind::Errored,
                    format!("{name}: the merge train stopped"),
                    format!("{report}\nptyro cs status {name}"),
                )
                .await;
            }
            me.release_job(&name);
        });
        Ok(json!({ "landing": todo, "gated": set.gated("land") && !yes }))
    }

    /// The merge train: integration checks, a checkpoint, then each member in order — its gate,
    /// its checks (CI for a PR, else its contract check), the merge. Stops at the first failure.
    async fn cs_land(self: &Arc<Self>, run: &str, yes: bool) -> Result<(), (String, String)> {
        let set = self.set_state(run).map_err(|e| ("set".to_owned(), e.message))?;
        let first = set
            .order()
            .unwrap_or_default()
            .into_iter()
            .find(|n| set.member(n).is_some_and(|m| m.landed.is_none()))
            .unwrap_or_default();
        if !set.integration.is_empty() {
            let env = self.set_env(&set).await;
            let mut results = Vec::new();
            for c in &set.integration {
                results.push(self.run_cmd(Path::new(&set.root), c, env.clone()).await);
            }
            self.cs_emit(run, vec![CsEvent::Checked { name: None, results: results.clone() }]);
            if let Some(f) = results.iter().find(|r| !r.ok) {
                return Err((
                    first,
                    format!(
                        "the integration check `{}` failed (exit {:?}), so nothing was merged",
                        f.command, f.exit_code
                    ),
                ));
            }
        }
        let _ = self.cs_checkpoint(run, Some("before land".into()), "land").await;
        for name in set.order().unwrap_or_default() {
            let set = self.set_state(run).map_err(|e| (name.clone(), e.message))?;
            let Some(m) = set.member(&name).cloned() else { continue };
            if m.landed.is_some() {
                continue;
            }
            if let Some(d) =
                m.depends_on.iter().find(|d| set.member(d).is_some_and(|x| x.landed.is_none()))
            {
                return Err((name, format!("its dependency {d} hasn't landed")));
            }
            if set.gated("land") && !yes {
                let gate = self
                    .cs_open_gate(
                        run,
                        &format!("land-{name}"),
                        &format!(
                            "Merge {name} ({}) into {}?",
                            m.branch,
                            base_branch(m.base.as_deref(), None)
                        ),
                        vec!["merge".into(), "stop".into()],
                    )
                    .await;
                let choice = self.wait_gate(run, &format!("land-{name}")).await;
                if choice.as_deref() != Some("merge") {
                    return Err((name, format!("stopped at gate {}", gate.id)));
                }
            }
            let wt = self.member_wt(&m).map_err(|e| (name.clone(), e.message))?;
            let how = if let Some(n) = m.pr {
                self.land_pr(&wt, n).await.map_err(|e| (name.clone(), e))?
            } else {
                if let Some(c) = &m.contract_check {
                    let env = self.set_env(&set).await;
                    let r = self.run_cmd(&wt.path, c, env).await;
                    self.cs_emit(
                        run,
                        vec![CsEvent::Checked {
                            name: Some(name.clone()),
                            results: vec![r.clone()],
                        }],
                    );
                    if !r.ok {
                        return Err((
                            name,
                            format!("its check failed: `{}` exited {:?}", r.command, r.exit_code),
                        ));
                    }
                }
                let into = base_branch(
                    m.base.as_deref(),
                    self.project_by_id(&m.project).and_then(|p| p.default_branch).as_deref(),
                );
                match self
                    .handle_review(Request::WorktreeIntegrate {
                        addr: wt.id.clone(),
                        into: Some(into.clone()),
                        cherry_pick: false,
                        push: !m.no_remote,
                    })
                    .await
                {
                    Ok(_) => format!("merged into {into}"),
                    Err(e) => {
                        if e.message.contains("conflict") {
                            self.cs_emit(
                                run,
                                vec![CsEvent::Conflict {
                                    name: name.clone(),
                                    detail: Some(format!("rebasing onto {into} conflicts")),
                                }],
                            );
                        }
                        return Err((name, e.message));
                    }
                }
            };
            self.cs_emit(run, vec![CsEvent::Landed { name: name.clone(), how }]);
            self.refresh_pr_tables(run).await;
        }
        let set = self.set_state(run).map_err(|e| ("set".to_owned(), e.message))?;
        if let Some(mut t) = set.train.clone() {
            t.stopped = None;
            self.cs_emit(run, vec![CsEvent::Train { train: Some(t) }]);
        }
        self.cs_emit(run, vec![CsEvent::Phase { phase: Phase::Landed }]);
        self.cs_note(run, format!("{} landed: every member merged in order", set.name));
        self.finish_run(run, RunStatus::Succeeded, None).await;
        Ok(())
    }

    /// Wait for a PR's checks, then merge it.
    async fn land_pr(self: &Arc<Self>, wt: &WorktreeInfo, n: u32) -> Result<String, String> {
        let started = now_ms();
        let deadline = started + 60 * 60 * 1000;
        loop {
            let pr = self.refresh_pr(wt).await.map_err(|e| e.message)?.ok_or("its PR is gone")?;
            if pr.state == "MERGED" {
                return Ok(format!("PR #{n} merged"));
            }
            if pr.state == "CLOSED" {
                return Err(format!("PR #{n} is closed"));
            }
            let fail: Vec<&str> = pr
                .checks
                .iter()
                .filter(|c| c.bucket == "fail" || c.bucket == "cancel")
                .map(|c| c.name.as_str())
                .collect();
            if !fail.is_empty() {
                return Err(format!("CI failed on PR #{n}: {}", fail.join(", ")));
            }
            // A PR pushed a moment ago has no check runs yet, and `gh` answers with none when it
            // can't ask either: that is "not known yet", not "green". Repositories with no CI
            // at all get a couple of minutes' grace and are then merged on the rest of their
            // state.
            let unknown =
                pr.checks.is_empty() && now_ms().saturating_sub(started) < no_checks_grace_ms();
            if !unknown && !pr.checks.iter().any(|c| c.bucket == "pending") {
                break;
            }
            if now_ms() > deadline {
                return Err(format!("PR #{n}'s checks were still pending after an hour"));
            }
            let wait =
                if ptyro_core::test_knob("PTYRO_TEST_FAST_POLL").is_some() { 300 } else { 15_000 };
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
        let method = format!("--{}", self.config().github.merge_method);
        let mut args = vec!["pr".into(), "merge".into(), n.to_string(), method];
        // Merge the commit whose checks were looked at: a push since then is merged unseen
        // otherwise.
        if let Ok(v) =
            self.gh_json(wt, &["pr", "view", &n.to_string(), "--json", "headRefOid"]).await
            && let Some(sha) = v["headRefOid"].as_str().filter(|s| !s.is_empty())
        {
            args.extend(["--match-head-commit".into(), sha.to_owned()]);
        }
        self.gh(wt, args).await.map_err(|e| e.message)?;
        Ok(format!("PR #{n} merged"))
    }

    // ── membership and removal ─────────────────────────────────────────────

    async fn cs_add(
        self: &Arc<Self>,
        run: &str,
        project: &str,
        agent: Option<String>,
        depends_on: Vec<String>,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let p = self.find_project(project)?;
        if set.member(&p.slug).is_some() {
            return Err(Error::conflict(format!("{} is a member already", p.slug)));
        }
        for d in &depends_on {
            set.member(d).ok_or_else(|| Error::missing("member", d))?;
        }
        let root_wt = self
            .root_worktree(&p.id)
            .ok_or_else(|| Error::missing("worktree", format!("{}/@root", p.slug)))?;
        let ws = set.workspace.clone().unwrap_or_default();
        let res = self.resolve_profile(None, Some(&(root_wt, p.clone())), &ws, false, true)?;
        if res.profile != set.profile {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "{} resolves to profile {}, the set runs under {}",
                    p.slug, res.profile, set.profile
                ),
            ));
        }
        let cfg = ptyro_core::project::ProjectConfig::load(&p.root, &p.root).unwrap_or_default();
        let m = Member {
            name: p.slug.clone(),
            project: p.id.clone(),
            agent: agent
                .or(cfg.project.default_agent)
                .or_else(|| Some(ptyro_agents::default_agent(&self.config()))),
            depends_on,
            ..Member::default()
        };
        let mut probe = set.clone();
        probe.members.push(m.clone());
        if let Err(c) = probe.order() {
            return Err(Error::invalid(format!(
                "depends_on would make a cycle: {}",
                c.join(" → ")
            )));
        }
        let wt = self.cs_worktree(&set, &m, &BTreeMap::new()).await?;
        self.cs_emit(
            run,
            vec![
                CsEvent::MemberAdded { member: Box::new(m.clone()) },
                CsEvent::MemberInfo {
                    name: m.name.clone(),
                    worktree: Some(wt.id.clone()),
                    path: Some(wt.path.to_string_lossy().into_owned()),
                    branch: wt.branch.clone(),
                    session: None,
                    overlay: None,
                    start: ptyro_git::Git::new().rev(&wt.path, "HEAD"),
                },
            ],
        );
        let set = self.set_state(run)?;
        self.write_brief(&set);
        if set.phase == Phase::Working && set.layout == Layout::Lead {
            self.spawn_member(run, &m.name).await?;
        }
        if let Some(l) = self.run_info(run).and_then(|i| i.state.lead) {
            let _ = self.post(Some("@ptyro"), &l, "heartbeat", &format!("{} joined the set (at ./{}). CHANGESET.md is updated; the contract may need a section for it.", m.name, m.name), Some(run.to_owned()), None, None, None, None).await;
        }
        Ok(json!({ "member": m.name, "worktree": wt.qualified() }))
    }

    async fn cs_drop(
        self: &Arc<Self>,
        run: &str,
        member: &str,
        force: bool,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        let m = set.member(member).ok_or_else(|| Error::missing("member", member))?.clone();
        let users: Vec<&str> = set
            .members
            .iter()
            .filter(|x| x.depends_on.contains(&m.name))
            .map(|x| x.name.as_str())
            .collect();
        if !users.is_empty() && !force {
            return Err(Error::conflict(format!(
                "{} depend on {member}: drop them first, or --force",
                users.join(", ")
            )));
        }
        if let Some(w) = &m.worktree {
            self.remove_worktree(w, RemoveOpts { force, stop_sessions: true, keep_branch: false })
                .await?;
        }
        self.cs_emit(run, vec![CsEvent::MemberDropped { name: m.name.clone() }]);
        self.write_brief(&self.set_state(run)?);
        Ok(json!({ "dropped": m.name }))
    }

    /// Remove the set: every member through the safe removal; one member with work that would
    /// be lost blocks all of them (or `--keep` it).
    async fn cs_rm(
        self: &Arc<Self>,
        run: &str,
        keep: &[String],
        force: bool,
    ) -> Result<Value, Error> {
        let set = self.set_state(run)?;
        // Not under a running push or land (it would merge into removed worktrees); one waiting at
        // a gate is called off.
        self.claim_for_rm(&set.name).await?;
        let r = self.cs_rm_inner(run, &set, keep, force).await;
        self.release_job(&set.name);
        r
    }

    async fn cs_rm_inner(
        self: &Arc<Self>,
        run: &str,
        set: &ChangeSet,
        keep: &[String],
        force: bool,
    ) -> Result<Value, Error> {
        let set = set.clone();
        for k in keep {
            set.member(k).ok_or_else(|| Error::missing("member", k))?;
        }
        let going: Vec<&Member> = set.members.iter().filter(|m| !keep.contains(&m.name)).collect();
        if !force {
            let mut unsafe_ = Vec::new();
            for m in &going {
                let Some(wt) = m.worktree.as_deref().and_then(|w| self.worktree_by_id(w)) else {
                    continue;
                };
                if !wt.path.exists() {
                    continue;
                }
                let (path, branch, base) = (wt.path.clone(), wt.branch.clone(), wt.base.clone());
                let s = blocking(move || {
                    ptyro_git::Git::new().safety(&path, branch.as_deref(), base.as_deref())
                })
                .await?;
                if let Ok(s) = s
                    && !s.is_clean()
                    && m.landed.is_none()
                {
                    unsafe_.push(format!("{}: {}", m.name, s.report()));
                }
            }
            if !unsafe_.is_empty() {
                return Err(Error::new(
                    ErrorCode::DirtyWorktree,
                    format!(
                        "refusing to remove {}: work would be lost in {}. Ship it, `--keep <member>`, or --force",
                        set.name,
                        unsafe_.join("; ")
                    ),
                ));
            }
        }
        // The agents first.
        let mut sessions: Vec<String> = set
            .members
            .iter()
            .filter(|m| !keep.contains(&m.name))
            .filter_map(|m| m.session.clone())
            .collect();
        sessions.extend(self.run_info(run).and_then(|i| i.state.lead));
        for sid in sessions {
            if let Ok(s) = self.resolve(&sid)
                && s.is_live()
            {
                self.stop_session(&s).await;
            }
        }
        let mut removed = Vec::new();
        for m in going {
            if let Some(w) = &m.worktree {
                match self
                    .remove_worktree(
                        w,
                        RemoveOpts {
                            force: true,
                            stop_sessions: true,
                            keep_branch: m.landed.is_none() && !force,
                        },
                    )
                    .await
                {
                    Ok(_) => removed.push(m.name.clone()),
                    Err(e) => self.cs_note(run, format!("removing {}: {}", m.name, e.message)),
                }
            }
        }
        self.cs_emit(run, vec![CsEvent::Phase { phase: Phase::Removed }]);
        let landed = set.phase == Phase::Landed;
        self.finish_run(
            run,
            if landed { RunStatus::Succeeded } else { RunStatus::Cancelled },
            (!landed).then(|| "removed".to_owned()),
        )
        .await;
        if keep.is_empty() {
            let _ = std::fs::remove_dir_all(&set.root);
        }
        self.cs.locked().remove(&set.name);
        Ok(json!({ "removed": removed, "kept": keep }))
    }

    // ── templates (M11.12) ─────────────────────────────────────────────────

    /// Templates: `~/.config/ptyro/changesets/*.toml`, then trusted hub repositories (a
    /// project's `.ptyro/changesets/*.toml`, trusted per template like repo recipes).
    pub(crate) fn cs_templates(&self) -> Vec<(TemplateInfo, String)> {
        let mut out: Vec<(TemplateInfo, String)> = Vec::new();
        let mut scan = |dir: &Path, source: &str, project: Option<&str>| {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            files.sort();
            for f in files {
                let src = std::fs::read_to_string(&f).unwrap_or_default();
                let stem =
                    f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let parsed = Template::parse(&src);
                let name = parsed.as_ref().map(|t| t.name.clone()).unwrap_or(stem);
                let untrusted = project.and_then(|p| {
                    let fp = ptyro_run::recipe::sha(&src);
                    (!self.is_trusted_as(p, &format!("changeset:{name}"), &fp)).then_some(fp)
                });
                let info = TemplateInfo {
                    name: name.clone(),
                    description: parsed.as_ref().ok().and_then(|t| t.description.clone()),
                    source: source.to_owned(),
                    path: f.to_string_lossy().into_owned(),
                    members: parsed
                        .as_ref()
                        .map(|t| t.members.iter().map(|m| m.project.clone()).collect())
                        .unwrap_or_default(),
                    untrusted,
                    error: parsed.err(),
                };
                out.retain(|(i, _)| i.name != name);
                out.push((info, src));
            }
        };
        scan(&self.paths.config.join("changesets"), "user", None);
        let projects: Vec<ptyro_core::model::ProjectInfo> = self.registry.locked().projects.clone();
        for p in projects {
            scan(
                &p.root.join(".ptyro").join("changesets"),
                &format!("hub:{}", p.slug),
                Some(&p.id),
            );
        }
        out
    }
}

/// The solo agent's first prompt: every member is its own.
fn solo_prompt(set: &ChangeSet) -> String {
    format!(
        "You are the only agent of change set {name}: one change across {n} repositories ({members}), each a directory here on branch {branch}. Read CHANGESET.md. You may edit every member. Keep CONTRACT.md as the record of the interfaces between them (ptyro records each saved version). Commit in each member when its tests pass. Never push: shipping is the human's (`ptyro cs push|pr|land`).{goal}",
        name = set.name,
        n = set.members.len(),
        members = set.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "),
        branch = set.members.first().map(|m| m.branch.as_str()).unwrap_or("?"),
        goal = set.goal.as_ref().map(|g| format!("\n\nThe goal: {g}")).unwrap_or_default(),
    )
}

/// A one-line text for a run event in the ledger (`None`: not worth a line).
fn describe(ev: &RunEvent) -> Option<(String, String)> {
    let e = match ev {
        RunEvent::Cs(e) => e,
        RunEvent::Note { text } => return Some(("note".into(), text.clone())),
        RunEvent::GateOpened { gate, .. } => {
            return Some((
                "gate_opened".into(),
                format!("gate {}: {} ({})", gate.id, gate.ask, gate.options.join("/")),
            ));
        }
        RunEvent::GateResolved { choice, by, note, .. } => {
            return Some((
                "gate_resolved".into(),
                format!(
                    "answered {choice} by {by}{}",
                    note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()
                ),
            ));
        }
        RunEvent::LeadSet { .. } => return Some(("lead".into(), "the lead started".into())),
        RunEvent::Ended { status, error, .. } => {
            return Some((
                "ended".into(),
                format!(
                    "the set's run ended {}{}",
                    status.label(),
                    error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
                ),
            ));
        }
        _ => return None,
    };
    let text = match e {
        CsEvent::Created { set } => format!(
            "created with {} ({} layout, profile {})",
            set.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "),
            set.layout.label(),
            set.profile
        ),
        CsEvent::MemberAdded { member } => format!("{} joined", member.name),
        CsEvent::MemberDropped { name } => format!("{name} was dropped"),
        CsEvent::MemberInfo { name, session: Some(_), .. } => format!("{name}'s agent started"),
        CsEvent::MemberInfo { name, overlay: Some(o), .. } => {
            if o.is_empty() {
                format!("{name}: overlay off")
            } else {
                format!("{name}: overlay {o}")
            }
        }
        CsEvent::MemberInfo { .. } | CsEvent::Delivered { .. } | CsEvent::Train { .. } => {
            return None;
        }
        CsEvent::Contract { rev } => format!(
            "contract r{} by {}{}{}",
            rev.n,
            rev.by,
            if rev.affects.is_empty() {
                String::new()
            } else {
                format!(", affects {}", rev.affects.join(", "))
            },
            rev.summary.as_ref().map(|s| format!(": {s}")).unwrap_or_default()
        ),
        CsEvent::ContractApproved { n, by } => format!("contract r{n} approved by {by}"),
        CsEvent::Ready { name, rev, .. } => format!("{name} is ready for integration (r{rev})"),
        CsEvent::Unready { name } => format!("{name} is no longer ready"),
        CsEvent::BlockedOn { name, on } => format!("{name} is blocked on {on}"),
        CsEvent::Unblocked { name, on } => format!("{name} is no longer blocked on {on}"),
        CsEvent::Failed { name, error: Some(e) } => format!("{name} failed: {e}"),
        CsEvent::Failed { name, error: None } => format!("{name} recovered"),
        CsEvent::Conflict { name, detail: Some(d) } => format!("{name}: {d}"),
        CsEvent::Conflict { name, detail: None } => format!("{name}: conflict resolved"),
        CsEvent::Checked { name, results } => format!(
            "{} {}",
            name.as_deref()
                .map_or("integration checks".to_owned(), |n| format!("{n}'s contract check")),
            if results.iter().all(|r| r.ok) {
                "passed".to_owned()
            } else {
                format!(
                    "failed ({})",
                    results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.command.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ),
        CsEvent::Checkpoint { ckpt } => format!(
            "set checkpoint #{} ({}{})",
            ckpt.n,
            ckpt.reason,
            ckpt.label.as_ref().map(|l| format!(": {l}")).unwrap_or_default()
        ),
        CsEvent::Committed { name, sha } => {
            format!("{name} committed {}", &sha[..sha.len().min(7)])
        }
        CsEvent::Pushed { name, sha } if sha.is_empty() => format!("{name} pushed"),
        CsEvent::Pushed { name, sha } => format!("{name} pushed {}", &sha[..sha.len().min(7)]),
        CsEvent::NoRemote { name, on } => {
            if *on {
                format!("{name} has no remote")
            } else {
                format!("{name} has a remote")
            }
        }
        CsEvent::Pr { name, number, url } => format!("{name}: PR #{number} {url}"),
        CsEvent::Landed { name, how } => format!("{name} landed: {how}"),
        CsEvent::OutOfOrder { name, detail: Some(d) } => format!("{name} {d}"),
        CsEvent::OutOfOrder { .. } => return None,
        CsEvent::Phase { phase } => format!("now {}", phase.label()),
    };
    Some((e.kind().to_owned(), text))
}

/// The manifests at a member's root.
fn read_manifest(root: &Path) -> cs::Manifest {
    let mut files = BTreeMap::new();
    for f in [
        "go.mod",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "compose.yml",
        "compose.yaml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ] {
        if let Ok(t) = std::fs::read_to_string(root.join(f)) {
            files.insert(f.to_owned(), t);
        }
    }
    cs::manifest(&files)
}

/// Each member's state, cheaply: HEAD plus `git status --porcelain`.
async fn fingerprints(set: &ChangeSet) -> BTreeMap<String, String> {
    let paths: Vec<(String, PathBuf)> =
        set.members.iter().map(|m| (m.name.clone(), PathBuf::from(&m.path))).collect();
    blocking(move || {
        paths
            .into_iter()
            .map(|(n, p)| {
                let g = ptyro_git::Git::new();
                let head = g.rev(&p, "HEAD").unwrap_or_default();
                let st = g
                    .run(&p, &["status", "--porcelain", "--untracked-files=all"])
                    .unwrap_or_default();
                (n, format!("{head}\n{}", sha_of(&st)))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Apply a unified diff to `text` (`git apply` in a private scratch directory: random name,
/// 0700, so no other user can plant a symlink in it).
async fn apply_patch(text: &str, diff: &str) -> Result<String, Error> {
    let (text, diff) = (text.to_owned(), diff.to_owned());
    blocking(move || -> Result<String, Error> {
        let scratch =
            tempfile::Builder::new().prefix("ptyro-contract-").tempdir().map_err(cs_err)?;
        let dir = scratch.path();
        (|| {
            let f = dir.join("CONTRACT.md");
            std::fs::write(&f, &text).map_err(cs_err)?;
            // Accept diffs against CONTRACT.md, a/CONTRACT.md, or with no names at all.
            let mut patch = diff.clone();
            if !patch.contains("--- ") {
                patch = format!("--- a/CONTRACT.md\n+++ b/CONTRACT.md\n{patch}");
            }
            if !patch.ends_with('\n') {
                patch.push('\n');
            }
            let pf = dir.join("p.diff");
            std::fs::write(&pf, &patch).map_err(cs_err)?;
            let out = std::process::Command::new("git")
                .args(["apply", "--recount", "--unidiff-zero", "-p1"])
                .arg(&pf)
                .current_dir(dir)
                .output()
                .map_err(cs_err)?;
            if !out.status.success() {
                return Err(Error::invalid(format!(
                    "the diff doesn't apply to the current contract: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            std::fs::read_to_string(&f).map_err(cs_err)
        })()
    })
    .await?
}

/// What the overlay wrote, so `off` can take it back.
const OVERLAY_RECORD: &str = ".ptyro/overlay.json";

/// Generate (and clean up) the overlay files for every member's `overlay` setting.
fn overlay_files(
    set: &ChangeSet,
    manifests: &BTreeMap<String, cs::Manifest>,
) -> Result<Vec<String>, Error> {
    let root = PathBuf::from(&set.root);
    let record_path = root.join(OVERLAY_RECORD);
    let previous: Vec<String> = std::fs::read_to_string(&record_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Undo what was there; regenerate from the settings.
    for p in &previous {
        let path = PathBuf::from(p);
        if let Some(backup) = p.strip_suffix(".ptyro-link") {
            let link = PathBuf::from(backup);
            if std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = std::fs::remove_file(&link);
            }
            let aside = link.with_file_name(format!(
                ".{}.ptyro-aside",
                link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            if aside.exists() {
                let _ = std::fs::rename(&aside, &link);
            }
        } else if path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
    let mut written: Vec<String> = Vec::new();
    let mut notes = Vec::new();
    let on = |kind: &str| set.members.iter().any(|m| m.overlay.as_deref() == Some(kind));
    let provider = |eco: &str, name: &str| -> Option<&Member> {
        set.members.iter().find(|m| {
            manifests
                .get(&m.name)
                .is_some_and(|x| x.provides.iter().any(|(e, n)| e == eco && n == name))
        })
    };
    // Go: one go.work in the root (Go finds it from inside every member).
    if on("go") {
        let go: Vec<String> = set
            .members
            .iter()
            .filter(|m| manifests.get(&m.name).is_some_and(|x| x.kinds.iter().any(|k| k == "go")))
            .map(|m| m.name.clone())
            .collect();
        let version = set
            .members
            .iter()
            .find_map(|m| {
                std::fs::read_to_string(Path::new(&m.path).join("go.mod"))
                    .ok()?
                    .lines()
                    .find_map(|l| l.strip_prefix("go ").map(|v| v.trim().to_owned()))
            })
            .unwrap_or_else(|| "1.22".into());
        let f = root.join("go.work");
        std::fs::write(&f, cs::go_work(&version, &go)).map_err(cs_err)?;
        written.push(f.to_string_lossy().into_owned());
        notes.push(format!("go.work in the root uses {}", go.join(", ")));
    }
    // Rust: [patch] in the root's .cargo/config.toml (cargo reads it from every member).
    if on("rust") {
        let mut patches = Vec::new();
        for m in set.members.iter().filter(|m| m.overlay.as_deref() == Some("rust")) {
            let Some(me) = manifests.get(&m.name) else { continue };
            let cargo =
                std::fs::read_to_string(Path::new(&m.path).join("Cargo.toml")).unwrap_or_default();
            for (eco, dep) in me.requires.iter().filter(|(e, _)| e == "rust") {
                let Some(p) = provider(eco, dep).filter(|p| p.name != m.name) else { continue };
                let git = cs::cargo_git_source(&cargo, dep);
                patches.push((
                    git.unwrap_or_else(|| "crates-io".into()),
                    dep.clone(),
                    p.path.clone(),
                ));
            }
        }
        patches.sort();
        patches.dedup();
        if !patches.is_empty() {
            let dir = root.join(".cargo");
            std::fs::create_dir_all(&dir).map_err(cs_err)?;
            let f = dir.join("config.toml");
            std::fs::write(&f, cs::cargo_patch(&patches)).map_err(cs_err)?;
            written.push(f.to_string_lossy().into_owned());
            notes.push(format!(
                "cargo [patch] for {}",
                patches.iter().map(|p| p.1.clone()).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    for m in &set.members {
        let Some(kind) = m.overlay.as_deref() else { continue };
        let Some(me) = manifests.get(&m.name) else { continue };
        let mp = PathBuf::from(&m.path);
        match kind {
            // Node: node_modules/<pkg> → the sibling (what `npm link` does), the installed copy
            // set aside and put back on `off`.
            "node" => {
                for (eco, dep) in me.requires.iter().filter(|(e, _)| e == "node") {
                    let Some(p) = provider(eco, dep).filter(|p| p.name != m.name) else { continue };
                    let link = mp.join("node_modules").join(dep);
                    if let Some(parent) = link.parent() {
                        std::fs::create_dir_all(parent).map_err(cs_err)?;
                        // `node_modules` (or a scope folder) that is a link out of the member
                        // would have the swap below happen somewhere else.
                        let inside = std::fs::canonicalize(parent)
                            .ok()
                            .zip(std::fs::canonicalize(&mp).ok())
                            .is_some_and(|(p, root)| p.starts_with(root));
                        if !inside {
                            notes.push(format!(
                                "{}: node_modules leads out of the member; skipped",
                                m.name
                            ));
                            continue;
                        }
                    }
                    if std::fs::symlink_metadata(&link).is_ok() {
                        let aside = link.with_file_name(format!(
                            ".{}.ptyro-aside",
                            link.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        ));
                        let _ = std::fs::remove_dir_all(&aside);
                        std::fs::rename(&link, &aside).map_err(cs_err)?;
                    }
                    std::os::unix::fs::symlink(&p.path, &link).map_err(cs_err)?;
                    written.push(format!("{}.ptyro-link", link.to_string_lossy()));
                    notes.push(format!("{}: node_modules/{dep} → ../{}", m.name, p.name));
                }
                let _ = ptyro_git::Git::new().exclude(&mp, "/node_modules/");
            }
            // Python: a .pth in the venv puts the sibling on the path (an editable install).
            "python" => {
                let venv = [".venv", "venv"].iter().map(|v| mp.join(v)).find(|v| v.is_dir());
                let Some(venv) = venv else {
                    notes.push(format!("{}: no .venv or venv to put siblings in", m.name));
                    continue;
                };
                let site = std::fs::read_dir(venv.join("lib")).ok().and_then(|rd| {
                    rd.flatten().map(|e| e.path().join("site-packages")).find(|p| p.is_dir())
                });
                let Some(site) = site else { continue };
                for (eco, dep) in me.requires.iter().filter(|(e, _)| e == "python") {
                    let Some(p) = provider(eco, dep).filter(|p| p.name != m.name) else { continue };
                    let src = if Path::new(&p.path).join("src").is_dir() {
                        format!("{}/src", p.path)
                    } else {
                        p.path.clone()
                    };
                    let f = site.join(format!("ptyro-cs-{}.pth", p.name));
                    std::fs::write(&f, format!("{src}\n")).map_err(cs_err)?;
                    written.push(f.to_string_lossy().into_owned());
                    notes.push(format!("{}: {dep} from ../{}", m.name, p.name));
                }
            }
            // Compose: siblings' worktrees as build contexts, in a git-excluded override.
            "compose" => {
                let svcs: Vec<(String, String)> = me
                    .services
                    .iter()
                    .filter(|s| set.member(s).is_some_and(|x| x.name != m.name))
                    .map(|s| (s.clone(), format!("../{s}")))
                    .collect();
                if svcs.is_empty() {
                    continue;
                }
                let f = mp.join("compose.override.yml");
                if f.exists()
                    && !std::fs::read_to_string(&f)
                        .unwrap_or_default()
                        .starts_with("# Generated by ptyro")
                {
                    notes.push(format!(
                        "{}: compose.override.yml exists and isn't ours; left alone",
                        m.name
                    ));
                    continue;
                }
                std::fs::write(&f, cs::compose_override(&svcs)).map_err(cs_err)?;
                let _ = ptyro_git::Git::new().exclude(&mp, "/compose.override.yml");
                written.push(f.to_string_lossy().into_owned());
                notes.push(format!(
                    "{}: compose builds {} from siblings",
                    m.name,
                    svcs.iter().map(|s| s.0.clone()).collect::<Vec<_>>().join(", ")
                ));
            }
            _ => {}
        }
    }
    std::fs::write(&record_path, serde_json::to_string_pretty(&written).unwrap_or_default())
        .map_err(cs_err)?;
    if notes.is_empty() && !previous.is_empty() {
        notes.push("overlay removed".into());
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_normalize_lexically() {
        assert_eq!(normalize(Path::new("/r/web/../api/x.go")), PathBuf::from("/r/api/x.go"));
        assert_eq!(normalize(Path::new("/r/./a")), PathBuf::from("/r/a"));
        assert_eq!(base_branch(Some("origin/develop"), None), "develop");
        assert_eq!(base_branch(None, Some("trunk")), "trunk");
    }

    #[tokio::test]
    async fn contract_patches_apply_in_a_private_scratch_dir() {
        let out = apply_patch("one\ntwo\n", "@@ -2 +2 @@\n-two\n+2\n").await.unwrap();
        assert_eq!(out, "one\n2\n");
        assert!(apply_patch("one\n", "@@ -1 +1 @@\n-zzz\n+y\n").await.is_err());
    }

    #[test]
    fn overlays_write_and_take_back_their_files() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        for (m, files) in [
            (
                "api",
                vec![
                    ("go.mod", "module example.com/api\n\ngo 1.23\n"),
                    ("package.json", r#"{"name":"@acme/api"}"#),
                ],
            ),
            ("sdk", vec![("go.mod", "module example.com/sdk\nrequire example.com/api v0.0.0\n")]),
            (
                "web",
                vec![
                    ("package.json", r#"{"name":"web","dependencies":{"@acme/api":"1"}}"#),
                    ("compose.yml", "services:\n  api:\n    build: x\n"),
                ],
            ),
        ] {
            std::fs::create_dir_all(root.join(m)).unwrap();
            for (f, t) in files {
                std::fs::write(root.join(m).join(f), t).unwrap();
            }
        }
        std::fs::create_dir_all(root.join("web/node_modules/@acme/api")).unwrap();
        std::fs::write(root.join("web/node_modules/@acme/api/installed"), "x").unwrap();
        std::fs::create_dir_all(root.join(".ptyro")).unwrap();
        let mk = |n: &str, o: Option<&str>| Member {
            name: n.into(),
            path: root.join(n).to_string_lossy().into_owned(),
            overlay: o.map(str::to_owned),
            ..Member::default()
        };
        let mut set = ChangeSet {
            name: "t".into(),
            root: root.to_string_lossy().into_owned(),
            members: vec![mk("api", None), mk("sdk", Some("go")), mk("web", Some("node"))],
            ..ChangeSet::default()
        };
        let manifests: BTreeMap<String, cs::Manifest> = set
            .members
            .iter()
            .map(|m| (m.name.clone(), read_manifest(Path::new(&m.path))))
            .collect();
        let notes = overlay_files(&set, &manifests).unwrap();
        let work = std::fs::read_to_string(root.join("go.work")).unwrap();
        assert!(
            work.contains("go 1.23") && work.contains("\t./api\n") && work.contains("\t./sdk\n"),
            "{work}"
        );
        let link = root.join("web/node_modules/@acme/api");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "{notes:?}");
        assert_eq!(std::fs::read_link(&link).unwrap(), root.join("api"));
        // Off: everything comes back as it was.
        for m in &mut set.members {
            m.overlay = None;
        }
        overlay_files(&set, &manifests).unwrap();
        assert!(!root.join("go.work").exists());
        assert!(root.join("web/node_modules/@acme/api/installed").exists());
        set.members[2].overlay = Some("compose".into());
        overlay_files(&set, &manifests).unwrap();
        assert!(
            std::fs::read_to_string(root.join("web/compose.override.yml"))
                .unwrap()
                .contains("context: \"../api\"")
        );
    }
}
