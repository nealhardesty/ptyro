//! A run's state, event-sourced (R-ORC-33): an append-only list of [`RunEvent`]s, and the
//! [`RunState`] they fold into. The store keeps both in one transaction; the events are the
//! truth, the state a projection anyone can rebuild with [`RunState::replay`].
//!
//! Agents can't be replayed (re-running a turn costs money and gives another answer), so a run
//! is never re-executed from its events: it only *resumes* from them, and time travel is a fork
//! (`ptyro run fork --from <step>`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// A recipe (`ptyro recipe run`).
    #[default]
    Recipe,
    /// `ptyro fanout`: N agents on one prompt, then compare.
    Fanout,
    /// A lead agent coordinating workers through `ptyro mcp`.
    Lead,
    /// A change set: one change across several repositories (M11).
    Changeset,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recipe => "recipe",
            Self::Fanout => "fanout",
            Self::Lead => "lead",
            Self::Changeset => "changeset",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    #[default]
    Running,
    /// Waiting on a person (a gate, or a held step).
    Waiting,
    Succeeded,
    /// A step failed: the agent or a check said no.
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_over(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    /// A gate waits for its answer, or a step is held for `run retry`.
    Waiting,
    Succeeded,
    /// The agent or a check said no (it ran).
    Failed,
    /// The infrastructure gave way (the daemon stopped under a script).
    Crashed,
    Skipped,
    Cancelled,
}

impl StepStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_over(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Crashed | Self::Skipped | Self::Cancelled
        )
    }
}

/// One try at a step: the unit of retry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub n: u32,
    pub started_ms: u64,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    /// The ptyro session.
    #[serde(default)]
    pub session: Option<String>,
    /// The agent's own session id, from SessionStart (native resume).
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    /// `refs/ptyro/runs/<run>/<step>/<n>/start` and `…/end`: the worktree before and after.
    #[serde(default)]
    pub start_ref: Option<String>,
    #[serde(default)]
    pub end_ref: Option<String>,
    /// `succeeded`, `failed`, `timeout`, `stall`, `crashed`, `guard_tripped`, `cancelled`.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    /// Verified-done bounces in this attempt.
    #[serde(default)]
    pub bounces: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    /// Stable and short (`g-7f3a`): resolvable from the TUI, CLI, a phone or a lead agent.
    pub id: String,
    pub ask: String,
    pub options: Vec<String>,
    pub opened_ms: u64,
    /// Absolute, so it survives restarts.
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub choice: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// `cli`, `tui`, `phone`, `lead:<session>`, `timeout`.
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub resolved_ms: Option<u64>,
    /// The idempotency key it was resolved with (a double tap resolves once).
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub status: StepStatus,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    /// Built-ins (`session`, `worktree`, `branch`, `base`, `head`, `diffstat`, `last_message`,
    /// `duration_ms`, `exit_code`) and the step's own (`output`).
    #[serde(default)]
    pub outputs: BTreeMap<String, Value>,
    #[serde(default)]
    pub gate: Option<Gate>,
    #[serde(default)]
    pub started_ms: Option<u64>,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    /// Why it ended the way it did.
    #[serde(default)]
    pub reason: Option<String>,
    /// What decided success: `status`, `output`, `done_when`, `exit_code`, `gate`, `user`.
    #[serde(default)]
    pub decided_by: Option<String>,
}

impl StepState {
    pub fn attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    pub id: String,
    pub kind: RunKind,
    /// The recipe's name, or `fanout`.
    pub name: String,
    /// The pinned recipe (its source is stored beside the run).
    pub recipe_sha: String,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    pub status: RunStatus,
    pub created_ms: u64,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub steps: BTreeMap<String, StepState>,
    /// Every session the run started (budgets and policy count them).
    #[serde(default)]
    pub sessions: Vec<String>,
    /// The lead agent's session, for `lead` runs.
    #[serde(default)]
    pub lead: Option<String>,
    /// `(run, step)` this one was forked from.
    #[serde(default)]
    pub forked_from: Option<(String, String)>,
    /// Candidates taken from the compare view, and how (`merge`, `pr`…).
    #[serde(default)]
    pub picked: Vec<(String, String)>,
    /// Candidates whose worktrees were removed.
    #[serde(default)]
    pub discarded: Vec<String>,
    /// A change set's members, contract and train (`changeset` runs, M11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset: Option<crate::changeset::ChangeSet>,
    /// The last event applied.
    pub seq: u64,
}

/// What happened, in order. Each event is small and self-describing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "e", rename_all = "snake_case")]
pub enum RunEvent {
    Created {
        kind: RunKind,
        name: String,
        recipe_sha: String,
        vars: BTreeMap<String, String>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        lead: Option<String>,
        #[serde(default)]
        forked_from: Option<(String, String)>,
        steps: Vec<String>,
        at: u64,
    },
    /// A step added later (a lead's worker, a fanout's compare).
    StepAdded {
        step: String,
    },
    /// A step copied from the run it was forked from.
    StepCopied {
        step: String,
        state: StepState,
    },
    AttemptStarted {
        step: String,
        n: u32,
        at: u64,
    },
    /// Facts learned during an attempt (any field may be missing).
    AttemptInfo {
        step: String,
        #[serde(default)]
        session: Option<String>,
        #[serde(default)]
        conversation: Option<String>,
        #[serde(default)]
        worktree: Option<String>,
        #[serde(default)]
        start_ref: Option<String>,
    },
    Bounced {
        step: String,
        detail: String,
    },
    GateOpened {
        step: String,
        gate: Gate,
    },
    GateResolved {
        step: String,
        choice: String,
        #[serde(default)]
        note: Option<String>,
        by: String,
        #[serde(default)]
        key: Option<String>,
        at: u64,
    },
    Outputs {
        step: String,
        outputs: BTreeMap<String, Value>,
    },
    AttemptEnded {
        step: String,
        outcome: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        end_ref: Option<String>,
        at: u64,
    },
    /// Held: waiting for `ptyro run retry` (on_restart = hold, guard trips that pause).
    StepHeld {
        step: String,
        reason: String,
    },
    StepEnded {
        step: String,
        status: StepStatus,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        decided_by: Option<String>,
        at: u64,
    },
    /// `ptyro run retry --step`: back to pending, attempts kept.
    StepReset {
        step: String,
    },
    /// The at-most-once boundary for `apply` (push, PR, merge): written before acting.
    Intent {
        step: String,
        action: String,
    },
    /// …and after.
    Outcome {
        step: String,
        action: String,
        ok: bool,
        #[serde(default)]
        detail: Option<String>,
    },
    SessionAdded {
        session: String,
    },
    Picked {
        step: String,
        mode: String,
    },
    Discarded {
        step: String,
    },
    Note {
        text: String,
    },
    Ended {
        status: RunStatus,
        #[serde(default)]
        error: Option<String>,
        at: u64,
    },
    /// The run's lead session, when it starts after the run (a change set's lead).
    LeadSet {
        session: String,
    },
    /// A change set changed (M11).
    Cs(crate::changeset::CsEvent),
}

impl RunEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::StepAdded { .. } => "step_added",
            Self::StepCopied { .. } => "step_copied",
            Self::AttemptStarted { .. } => "attempt_started",
            Self::AttemptInfo { .. } => "attempt_info",
            Self::Bounced { .. } => "bounced",
            Self::GateOpened { .. } => "gate_opened",
            Self::GateResolved { .. } => "gate_resolved",
            Self::Outputs { .. } => "outputs",
            Self::AttemptEnded { .. } => "attempt_ended",
            Self::StepHeld { .. } => "step_held",
            Self::StepEnded { .. } => "step_ended",
            Self::StepReset { .. } => "step_reset",
            Self::Intent { .. } => "intent",
            Self::Outcome { .. } => "outcome",
            Self::SessionAdded { .. } => "session_added",
            Self::Picked { .. } => "picked",
            Self::Discarded { .. } => "discarded",
            Self::Note { .. } => "note",
            Self::Ended { .. } => "ended",
            Self::LeadSet { .. } => "lead_set",
            Self::Cs(_) => "cs",
        }
    }

    pub fn step(&self) -> Option<&str> {
        match self {
            Self::StepAdded { step }
            | Self::StepCopied { step, .. }
            | Self::AttemptStarted { step, .. }
            | Self::AttemptInfo { step, .. }
            | Self::Bounced { step, .. }
            | Self::GateOpened { step, .. }
            | Self::GateResolved { step, .. }
            | Self::Outputs { step, .. }
            | Self::AttemptEnded { step, .. }
            | Self::StepHeld { step, .. }
            | Self::StepEnded { step, .. }
            | Self::StepReset { step }
            | Self::Intent { step, .. }
            | Self::Outcome { step, .. }
            | Self::Picked { step, .. }
            | Self::Discarded { step } => Some(step),
            _ => None,
        }
    }
}

impl RunState {
    pub fn new(id: &str) -> Self {
        Self { id: id.to_owned(), ..Self::default() }
    }

    /// Fold every event, in order, from nothing.
    pub fn replay(id: &str, events: &[RunEvent]) -> Self {
        let mut s = Self::new(id);
        for e in events {
            s.apply(e);
        }
        s
    }

    pub fn step_mut(&mut self, id: &str) -> &mut StepState {
        self.steps.entry(id.to_owned()).or_default()
    }

    pub fn apply(&mut self, e: &RunEvent) {
        self.seq += 1;
        match e {
            RunEvent::Created {
                kind,
                name,
                recipe_sha,
                vars,
                project,
                base,
                lead,
                forked_from,
                steps,
                at,
            } => {
                self.kind = *kind;
                self.name = name.clone();
                self.recipe_sha = recipe_sha.clone();
                self.vars = vars.clone();
                self.project = project.clone();
                self.base = base.clone();
                self.lead = lead.clone();
                self.forked_from = forked_from.clone();
                self.created_ms = *at;
                self.status = RunStatus::Running;
                for s in steps {
                    self.step_mut(s);
                }
            }
            RunEvent::StepAdded { step } => {
                self.step_mut(step);
            }
            RunEvent::StepCopied { step, state } => {
                self.steps.insert(step.clone(), state.clone());
            }
            RunEvent::AttemptStarted { step, n, at } => {
                let s = self.step_mut(step);
                s.status = StepStatus::Running;
                s.started_ms.get_or_insert(*at);
                s.attempts.push(Attempt { n: *n, started_ms: *at, ..Attempt::default() });
            }
            RunEvent::AttemptInfo { step, session, conversation, worktree, start_ref } => {
                let s = self.step_mut(step);
                if let Some(a) = s.attempts.last_mut() {
                    if session.is_some() {
                        a.session = session.clone();
                    }
                    if conversation.is_some() {
                        a.conversation = conversation.clone();
                    }
                    if worktree.is_some() {
                        a.worktree = worktree.clone();
                    }
                    if start_ref.is_some() {
                        a.start_ref = start_ref.clone();
                    }
                }
            }
            RunEvent::Bounced { step, .. } => {
                if let Some(a) = self.step_mut(step).attempts.last_mut() {
                    a.bounces += 1;
                }
            }
            RunEvent::GateOpened { step, gate } => {
                let s = self.step_mut(step);
                s.status = StepStatus::Waiting;
                s.gate = Some(gate.clone());
                self.status = RunStatus::Waiting;
            }
            RunEvent::GateResolved { step, choice, note, by, key, at } => {
                let s = self.step_mut(step);
                if let Some(g) = &mut s.gate {
                    g.choice = Some(choice.clone());
                    g.note = note.clone();
                    g.by = Some(by.clone());
                    g.key = key.clone();
                    g.resolved_ms = Some(*at);
                }
                s.status = StepStatus::Running;
                self.refresh_waiting();
            }
            RunEvent::Outputs { step, outputs } => {
                let s = self.step_mut(step);
                for (k, v) in outputs {
                    s.outputs.insert(k.clone(), v.clone());
                }
            }
            RunEvent::AttemptEnded { step, outcome, error, end_ref, at } => {
                if let Some(a) = self.step_mut(step).attempts.last_mut() {
                    a.outcome = Some(outcome.clone());
                    a.error = error.clone();
                    a.end_ref = end_ref.clone();
                    a.ended_ms = Some(*at);
                }
            }
            RunEvent::StepHeld { step, reason } => {
                let s = self.step_mut(step);
                s.status = StepStatus::Waiting;
                s.reason = Some(reason.clone());
                self.status = RunStatus::Waiting;
            }
            RunEvent::StepEnded { step, status, reason, decided_by, at } => {
                let s = self.step_mut(step);
                s.status = *status;
                s.reason = reason.clone();
                s.decided_by = decided_by.clone();
                s.ended_ms = Some(*at);
                self.refresh_waiting();
            }
            RunEvent::StepReset { step } => {
                let s = self.step_mut(step);
                s.status = StepStatus::Pending;
                s.reason = None;
                s.decided_by = None;
                s.ended_ms = None;
                s.gate = None;
                if self.status.is_over() {
                    self.status = RunStatus::Running;
                    self.ended_ms = None;
                    self.error = None;
                }
                self.refresh_waiting();
            }
            RunEvent::Intent { .. } | RunEvent::Outcome { .. } | RunEvent::Note { .. } => {}
            RunEvent::SessionAdded { session } => {
                if !self.sessions.contains(session) {
                    self.sessions.push(session.clone());
                }
            }
            RunEvent::Picked { step, mode } => self.picked.push((step.clone(), mode.clone())),
            RunEvent::Discarded { step } => {
                if !self.discarded.contains(step) {
                    self.discarded.push(step.clone());
                }
            }
            RunEvent::Ended { status, error, at } => {
                self.status = *status;
                self.error = error.clone();
                self.ended_ms = Some(*at);
            }
            RunEvent::LeadSet { session } => {
                self.lead = Some(session.clone());
                if !self.sessions.contains(session) {
                    self.sessions.push(session.clone());
                }
            }
            RunEvent::Cs(e) => self.changeset.get_or_insert_with(Default::default).apply(e),
        }
    }

    fn refresh_waiting(&mut self) {
        if self.status.is_over() {
            return;
        }
        let waiting = self.steps.values().any(|s| s.status == StepStatus::Waiting);
        self.status = if waiting { RunStatus::Waiting } else { RunStatus::Running };
    }

    /// Attempts across all steps (the `attempts` budget).
    pub fn attempts(&self) -> u32 {
        self.steps.values().map(|s| s.attempts.len() as u32).sum()
    }

    /// The step whose latest attempt runs in `session`.
    pub fn step_of_session(&self, session: &str) -> Option<&str> {
        self.steps.iter().find_map(|(id, s)| {
            s.attempts.iter().any(|a| a.session.as_deref() == Some(session)).then_some(id.as_str())
        })
    }

    /// Open gates, oldest first.
    pub fn open_gates(&self) -> Vec<(&str, &Gate)> {
        let mut v: Vec<(&str, &Gate)> = self
            .steps
            .iter()
            .filter_map(|(id, s)| {
                s.gate.as_ref().filter(|g| g.choice.is_none()).map(|g| (id.as_str(), g))
            })
            .collect();
        v.sort_by_key(|(_, g)| g.opened_ms);
        v
    }

    /// What templates and conditions read (SPEC §15.3): `vars`, `steps`, `gates`, `run`.
    pub fn context(&self, run_dir: &str) -> Value {
        let mut steps = serde_json::Map::new();
        let mut gates = serde_json::Map::new();
        for (id, s) in &self.steps {
            let a = s.attempt();
            let mut v = serde_json::json!({
                "status": s.status.label(),
                "outputs": s.outputs,
                "attempts": s.attempts.len(),
                "error": a.and_then(|a| a.error.clone()),
                "session": a.and_then(|a| a.session.clone()),
                "worktree": a.and_then(|a| a.worktree.clone()),
            });
            if let Some(code) = s.outputs.get("exit_code") {
                v["exit_code"] = code.clone();
            }
            steps.insert(id.clone(), v);
            if let Some(g) = &s.gate {
                gates.insert(
                    id.clone(),
                    serde_json::json!({ "choice": g.choice, "note": g.note, "by": g.by }),
                );
            }
        }
        serde_json::json!({
            "vars": self.vars,
            "steps": steps,
            "gates": gates,
            "run": { "id": self.id, "dir": run_dir, "base": self.base, "project": self.project, "name": self.name },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_fold_into_state() {
        let evs = vec![
            RunEvent::Created {
                kind: RunKind::Recipe,
                name: "r".into(),
                recipe_sha: "abc".into(),
                vars: BTreeMap::from([("task".into(), "x".into())]),
                project: Some("p".into()),
                base: None,
                lead: None,
                forked_from: None,
                steps: vec!["a".into(), "g".into()],
                at: 1,
            },
            RunEvent::AttemptStarted { step: "a".into(), n: 1, at: 2 },
            RunEvent::AttemptInfo {
                step: "a".into(),
                session: Some("s1".into()),
                conversation: None,
                worktree: Some("w1".into()),
                start_ref: None,
            },
            RunEvent::SessionAdded { session: "s1".into() },
            RunEvent::Bounced { step: "a".into(), detail: "tests failed".into() },
            RunEvent::Outputs {
                step: "a".into(),
                outputs: BTreeMap::from([("exit_code".into(), serde_json::json!(0))]),
            },
            RunEvent::AttemptEnded {
                step: "a".into(),
                outcome: "succeeded".into(),
                error: None,
                end_ref: Some("e".into()),
                at: 3,
            },
            RunEvent::StepEnded {
                step: "a".into(),
                status: StepStatus::Succeeded,
                reason: None,
                decided_by: Some("done_when".into()),
                at: 3,
            },
            RunEvent::GateOpened {
                step: "g".into(),
                gate: Gate {
                    id: "g-1".into(),
                    ask: "ok?".into(),
                    options: vec!["approve".into(), "reject".into()],
                    opened_ms: 4,
                    ..Gate::default()
                },
            },
        ];
        let s = RunState::replay("r1", &evs);
        assert_eq!(s.status, RunStatus::Waiting);
        assert_eq!(s.seq, evs.len() as u64);
        assert_eq!(s.steps["a"].attempts[0].bounces, 1);
        assert_eq!(s.step_of_session("s1"), Some("a"));
        assert_eq!(s.open_gates().len(), 1);
        let ctx = s.context("/tmp/r1");
        assert_eq!(ctx["steps"]["a"]["exit_code"], 0);
        assert_eq!(ctx["steps"]["a"]["worktree"], "w1");
        let mut s2 = s.clone();
        s2.apply(&RunEvent::GateResolved {
            step: "g".into(),
            choice: "approve".into(),
            note: None,
            by: "cli".into(),
            key: None,
            at: 5,
        });
        assert_eq!(s2.status, RunStatus::Running);
        assert!(s2.open_gates().is_empty());
        // Round-trips as JSON (the store keeps events as JSON).
        for e in &evs {
            let back: RunEvent = serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap();
            assert_eq!(&back, e);
        }
    }
}
