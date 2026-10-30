//! The recipe schema (SPEC §15.3): declarative TOML workflows of steps with dependencies.
//!
//! A recipe is data. The daemon validates it, pins its source and sha per run, and executes it
//! step by step; nothing in it is code. Each step is exactly one kind: `spawn` (+ `prompt`),
//! `send`, `run`, `gate`, `apply`, `sleep` or `verify`; a `loop` rides on a `spawn`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::expr;
use crate::template;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, VarDef>,
    #[serde(default)]
    pub budget: Budget,
    /// Limits for a lead agent driving this run through `ptyro mcp` (SPEC §15.4).
    #[serde(default)]
    pub policy: Option<Policy>,
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VarDef {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Run-wide limits. Cost (`usd`, tokens) waits for a trustworthy per-agent signal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Wall-clock limit for the whole run (`3h`).
    #[serde(default)]
    pub wall: Option<String>,
    /// Sessions the run may start, all steps together.
    #[serde(default)]
    pub sessions: Option<u32>,
    /// Attempts, all steps together (retries count).
    #[serde(default)]
    pub attempts: Option<u32>,
}

/// What a lead agent may do through `ptyro mcp`. Enforced by the daemon, never by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Live worker sessions at once.
    #[serde(default = "four")]
    pub max_sessions: u32,
    /// Worker sessions over the run's life.
    #[serde(default)]
    pub max_spawns: Option<u32>,
    /// Agents it may start (empty: any configured agent).
    #[serde(default)]
    pub agents: Vec<String>,
    /// Where workers may run: `new` (fresh worktrees), `own` (worktrees of this run), `root`.
    #[serde(default = "default_targets")]
    pub worktrees: Vec<String>,
    /// Commit, push, integrate, remove worktrees, open PRs. Off by default.
    #[serde(default)]
    pub destructive: bool,
    /// The chat history tools (`chat_search`, `chat_read`, `chat_list`; SPEC §22.10). Off by
    /// default.
    #[serde(default)]
    pub chat: bool,
}

fn four() -> u32 {
    4
}

fn default_targets() -> Vec<String> {
    vec!["new".into(), "own".into()]
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_sessions: four(),
            max_spawns: None,
            agents: Vec::new(),
            worktrees: default_targets(),
            destructive: false,
            chat: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub join: Join,
    /// With `join = "first_success"`: what happens to the other needs once one succeeds.
    #[serde(default)]
    pub cancel_siblings: Option<CancelSiblings>,
    /// Run only when this holds (`steps.review.outputs.decision == 'changes'`).
    #[serde(rename = "if", default)]
    pub when: Option<String>,

    // ── kinds ──
    #[serde(default)]
    pub spawn: Option<SpawnDef>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub send: Option<SendDef>,
    /// `scripts.run.<name>` (the project's run script) or a shell command.
    #[serde(default)]
    pub run: Option<String>,
    /// The step whose worktree a `run` uses (default: the project's root).
    #[serde(rename = "in", default)]
    pub in_step: Option<String>,
    #[serde(default)]
    pub gate: Option<GateDef>,
    #[serde(default)]
    pub apply: Option<ApplyDef>,
    #[serde(default)]
    pub sleep: Option<String>,
    #[serde(default)]
    pub verify: Option<VerifyDef>,
    #[serde(rename = "loop", default)]
    pub loop_: Option<LoopDef>,

    // ── how it ends ──
    #[serde(default)]
    pub wait: Option<WaitFor>,
    #[serde(default)]
    pub done_when: Option<DoneWhen>,
    #[serde(default)]
    pub timeout: Option<Timeout>,
    #[serde(default)]
    pub retry: Option<Retry>,
    #[serde(default)]
    pub guard: Option<Guard>,
    #[serde(default)]
    pub output: Option<OutputDef>,
    #[serde(default)]
    pub on_restart: Option<OnRestart>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// Every need succeeded (a failed or skipped need skips this step).
    #[default]
    All,
    /// Any need ended, however.
    Any,
    /// The first need to succeed; fails when all of them failed.
    FirstSuccess,
    /// Every need ended, however.
    AllSettled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelSiblings {
    Interrupt,
    Keep,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnDef {
    pub agent: String,
    /// `new` (default), `root`, another step's id (its worktree), or a worktree address.
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    /// Base for a new worktree (default: the run's base).
    #[serde(default)]
    pub base: Option<String>,
    /// Drive the agent over its structured protocol (M10): every tool call shows in the
    /// transcript and the compare view.
    #[serde(default)]
    pub structured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendDef {
    /// The step whose session gets the text.
    pub to: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GateDef {
    Ask(String),
    Full(GateFull),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateFull {
    pub ask: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub timeout: Option<String>,
    /// `reject`, `approve`, or `escalate` (notify again and keep waiting). Default `reject`.
    #[serde(default)]
    pub on_timeout: Option<String>,
    /// Choices that fail the step (default `["reject"]`).
    #[serde(default)]
    pub fail_on: Option<Vec<String>>,
}

impl GateDef {
    pub fn ask(&self) -> &str {
        match self {
            Self::Ask(a) => a,
            Self::Full(f) => &f.ask,
        }
    }

    pub fn options(&self) -> Vec<String> {
        match self {
            Self::Full(f) if !f.options.is_empty() => f.options.clone(),
            _ => vec!["approve".into(), "reject".into()],
        }
    }

    pub fn timeout(&self) -> Option<&str> {
        match self {
            Self::Full(f) => f.timeout.as_deref(),
            Self::Ask(_) => None,
        }
    }

    pub fn on_timeout(&self) -> &str {
        match self {
            Self::Full(f) => f.on_timeout.as_deref().unwrap_or("reject"),
            Self::Ask(_) => "reject",
        }
    }

    pub fn fails(&self, choice: &str) -> bool {
        match self {
            Self::Full(GateFull { fail_on: Some(f), .. }) => f.iter().any(|c| c == choice),
            _ => choice == "reject",
        }
    }
}

/// Safe outputs: only ptyro pushes, opens PRs or merges, after policy (SPEC §15.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyDef {
    /// The step whose worktree to ship.
    pub from: String,
    /// In order: `commit`, `push`, `pr`, `integrate`, `automerge`.
    pub actions: Vec<String>,
    /// Commit message (default: from the step's prompt).
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
    /// `integrate` target (default: the project's default branch).
    #[serde(default)]
    pub into: Option<String>,
}

pub const APPLY_ACTIONS: [&str; 5] = ["commit", "push", "pr", "integrate", "automerge"];

/// Rank candidate worktrees by evidence (R-ORC-04).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyDef {
    /// Steps whose worktrees compete (default: every `spawn` step it needs).
    #[serde(default)]
    pub candidates: Vec<String>,
    /// `scripts.run.<name>` or a shell command: passes or not, per candidate and on the base.
    #[serde(default)]
    pub tests: Option<String>,
    /// A command that must fail on the base and pass on a candidate.
    #[serde(default)]
    pub repro: Option<String>,
    /// A glob of test files: each candidate's tests run against every other candidate.
    #[serde(default)]
    pub cross_tests: Option<String>,
    /// An advisory judge: another agent with a clean context, returning rubric JSON.
    #[serde(default)]
    pub judge: Option<String>,
}

/// Iterate with a fresh session each time until a check passes (R-ORC-25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopDef {
    /// `scripts.run.<name>` or a shell command; exit 0 ends the loop successfully.
    pub until: String,
    /// Iterations at most (always capped).
    #[serde(default = "five")]
    pub max: u32,
    /// Stop after this many iterations in a row that changed nothing.
    #[serde(default = "two")]
    pub no_progress: u32,
}

fn five() -> u32 {
    5
}

fn two() -> u32 {
    2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitFor {
    /// The turn the prompt started ended (idle or done).
    Idle,
    Done,
    Exited,
}

/// Verified-done (R-ORC-23): the turn only counts when this check passes; otherwise the agent
/// is bounced back with the failures, up to `max_bounces` times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoneWhen {
    pub run: String,
    #[serde(default = "three")]
    pub max_bounces: u32,
}

fn three() -> u32 {
    3
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Timeout {
    Total(String),
    Full(TimeoutFull),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutFull {
    #[serde(default)]
    pub total: Option<String>,
    /// Until the agent starts working on the prompt.
    #[serde(default)]
    pub start: Option<String>,
    /// Working with no hook event or output for this long.
    #[serde(default)]
    pub stall: Option<String>,
}

impl Timeout {
    pub fn parts(&self) -> TimeoutFull {
        match self {
            Self::Total(t) => TimeoutFull { total: Some(t.clone()), ..TimeoutFull::default() },
            Self::Full(f) => f.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub max: u32,
    /// Outcomes that retry (default: failed, timeout, stall, crashed).
    #[serde(default)]
    pub on: Vec<String>,
    #[serde(default)]
    pub not_on: Vec<String>,
    #[serde(default)]
    pub mode: RetryMode,
    /// Sent with the retry: `{{attempt.error}}` is why the last one failed.
    #[serde(default)]
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryMode {
    /// A new session (in the same worktree).
    #[default]
    Fresh,
    /// The feedback goes to the same session.
    SameSession,
}

pub const OUTCOMES: [&str; 7] =
    ["failed", "timeout", "stall", "crashed", "guard_tripped", "cancelled", "rejected"];

/// Tripwires that run alongside a step (enforced through PreToolUse denials).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    #[serde(default)]
    pub deny_paths: Vec<String>,
    /// Command prefixes the agent may not run (`git push`).
    #[serde(default)]
    pub deny_cmds: Vec<String>,
    #[serde(default)]
    pub max_diff_lines: Option<u32>,
    /// `pause` (default: a gate asks you) or `fail`.
    #[serde(default)]
    pub on_trip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDef {
    /// `builtin:verdict`, `builtin:subtasks`, or `{ "required": ["key", …] }` as JSON.
    pub schema: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnRestart {
    /// Resume the agent's own conversation (when its adapter can).
    NativeResume,
    /// A new attempt.
    Fresh,
    /// Wait for `ptyro run retry`.
    Hold,
}

/// What a step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Spawn,
    Send,
    Run,
    Gate,
    Apply,
    Sleep,
    Verify,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Send => "send",
            Self::Run => "run",
            Self::Gate => "gate",
            Self::Apply => "apply",
            Self::Sleep => "sleep",
            Self::Verify => "verify",
        }
    }
}

impl Step {
    pub fn kind(&self) -> Option<Kind> {
        let kinds = self.kinds();
        (kinds.len() == 1).then(|| kinds[0])
    }

    fn kinds(&self) -> Vec<Kind> {
        let mut k = Vec::new();
        if self.spawn.is_some() {
            k.push(Kind::Spawn);
        }
        if self.send.is_some() {
            k.push(Kind::Send);
        }
        if self.run.is_some() {
            k.push(Kind::Run);
        }
        if self.gate.is_some() {
            k.push(Kind::Gate);
        }
        if self.apply.is_some() {
            k.push(Kind::Apply);
        }
        if self.sleep.is_some() {
            k.push(Kind::Sleep);
        }
        if self.verify.is_some() {
            k.push(Kind::Verify);
        }
        k
    }

    /// The step whose worktree this one works in, if it borrows one.
    pub fn worktree_step(&self) -> Option<&str> {
        match self.kind()? {
            Kind::Spawn => self.spawn.as_ref()?.worktree.as_deref(),
            Kind::Send => self.send.as_ref().map(|s| s.to.as_str()),
            Kind::Run => self.in_step.as_deref(),
            Kind::Apply => self.apply.as_ref().map(|a| a.from.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .0.join("\n"))]
pub struct Invalid(pub Vec<String>);

impl Recipe {
    pub fn parse(src: &str) -> Result<Self, Invalid> {
        let r: Self = toml::from_str(src).map_err(|e| Invalid(vec![e.to_string()]))?;
        r.validate()?;
        Ok(r)
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// Every step `id` (transitively) needs.
    pub fn ancestors(&self, id: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut todo: Vec<String> = self.step(id).map(|s| s.needs.clone()).unwrap_or_default();
        while let Some(n) = todo.pop() {
            if out.insert(n.clone())
                && let Some(s) = self.step(&n)
            {
                todo.extend(s.needs.iter().cloned());
            }
        }
        out
    }

    /// Everything `ptyro recipe validate` checks: one kind per step, known references,
    /// no cycles, templates and conditions that only read what exists by then.
    pub fn validate(&self) -> Result<(), Invalid> {
        let mut errs = Vec::new();
        if self.name.trim().is_empty() {
            errs.push("name is empty".into());
        }
        if self.steps.is_empty() {
            errs.push("a recipe needs at least one [[step]]".into());
        }
        let ids: BTreeSet<&str> = self.steps.iter().map(|s| s.id.as_str()).collect();
        if ids.len() != self.steps.len() {
            errs.push("step ids must be unique".into());
        }
        for s in &self.steps {
            let at = format!("step {:?}", s.id);
            if s.id.is_empty()
                || !s.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                errs.push(format!("{at}: ids use letters, digits, - and _"));
            }
            match s.kinds().as_slice() {
                [] => errs.push(format!(
                    "{at}: say what it does: spawn, send, run, gate, apply, sleep or verify"
                )),
                [_] => {}
                many => errs.push(format!(
                    "{at}: one kind per step, not {}",
                    many.iter().map(|k| k.label()).collect::<Vec<_>>().join(" + ")
                )),
            }
            for n in &s.needs {
                if !ids.contains(n.as_str()) {
                    errs.push(format!("{at}: needs unknown step {n:?}"));
                }
                if n == &s.id {
                    errs.push(format!("{at}: needs itself"));
                }
            }
            if s.cancel_siblings.is_some() && s.join != Join::FirstSuccess {
                errs.push(format!("{at}: cancel_siblings only goes with join = \"first_success\""));
            }
            if s.prompt.is_some() && s.spawn.is_none() {
                errs.push(format!("{at}: prompt goes with spawn (use send to follow up)"));
            }
            if s.spawn.is_some() && s.prompt.is_none() {
                errs.push(format!("{at}: a spawn step needs a prompt"));
            }
            if s.loop_.is_some() && s.spawn.is_none() {
                errs.push(format!("{at}: loop goes with spawn"));
            }
            if s.done_when.is_some() && s.spawn.is_none() && s.send.is_none() {
                errs.push(format!("{at}: done_when goes with spawn or send"));
            }
            if s.in_step.is_some() && s.run.is_none() {
                errs.push(format!("{at}: in goes with run"));
            }
            if let Some(a) = &s.apply {
                for act in &a.actions {
                    if !APPLY_ACTIONS.contains(&act.as_str()) {
                        errs.push(format!(
                            "{at}: unknown apply action {act:?} (one of {})",
                            APPLY_ACTIONS.join(", ")
                        ));
                    }
                }
                if a.actions.is_empty() {
                    errs.push(format!("{at}: apply needs actions"));
                }
            }
            if let Some(GateDef::Full(g)) = &s.gate
                && !["reject", "approve", "escalate"]
                    .contains(&g.on_timeout.as_deref().unwrap_or("reject"))
                && !g.options.iter().any(|o| Some(o.as_str()) == g.on_timeout.as_deref())
            {
                errs.push(format!("{at}: on_timeout is reject, approve, escalate or an option"));
            }
            if let Some(r) = &s.retry {
                for o in r.on.iter().chain(&r.not_on) {
                    if !OUTCOMES.contains(&o.as_str()) {
                        errs.push(format!(
                            "{at}: unknown outcome {o:?} in retry ({})",
                            OUTCOMES.join(", ")
                        ));
                    }
                }
            }
            if let Some(o) = &s.output
                && !o.schema.starts_with("builtin:")
                && serde_json::from_str::<serde_json::Value>(&o.schema).is_err()
            {
                errs.push(format!(
                    "{at}: output schema is builtin:verdict, builtin:subtasks or inline JSON"
                ));
            }
            if let Some(o) = &s.output
                && let Some(name) = o.schema.strip_prefix("builtin:")
                && !["verdict", "subtasks"].contains(&name)
            {
                errs.push(format!("{at}: unknown builtin schema {name:?}"));
            }
            let mut durations: Vec<&str> = Vec::new();
            durations.extend(s.sleep.as_deref());
            durations.extend(s.gate.as_ref().and_then(|g| g.timeout()));
            let parts = s.timeout.as_ref().map(Timeout::parts);
            if let Some(p) = &parts {
                durations.extend(
                    [&p.total, &p.start, &p.stall].into_iter().flatten().map(String::as_str),
                );
            }
            for d in durations {
                if ptyro_core::config::parse_duration(d).is_err() {
                    errs.push(format!("{at}: bad duration {d:?} (30s, 10m, 2h)"));
                }
            }
            // Steps it borrows a worktree or session from must exist, and come first.
            let ancestors = self.ancestors(&s.id);
            let borrowed = s.worktree_step().filter(|w| !["new", "root", "@root"].contains(w));
            if let Some(w) = borrowed {
                let from_spawn = s.spawn.is_some();
                if ids.contains(w) {
                    if !ancestors.contains(w) {
                        errs.push(format!("{at}: uses step {w:?}'s worktree, so it must need it"));
                    }
                } else if !from_spawn {
                    errs.push(format!("{at}: unknown step {w:?}"));
                }
            }
            if let Some(v) = &s.verify {
                for c in &v.candidates {
                    if !ancestors.contains(c) {
                        errs.push(format!("{at}: candidate {c:?} must be a step it needs"));
                    }
                }
            }
            // Templates and conditions.
            let mut texts: Vec<(&str, &str)> = Vec::new();
            if let Some(p) = &s.prompt {
                texts.push(("prompt", p));
            }
            if let Some(sd) = &s.send {
                texts.push(("send.text", &sd.text));
            }
            if let Some(r) = &s.run {
                texts.push(("run", r));
            }
            if let Some(g) = &s.gate {
                texts.push(("gate", g.ask()));
            }
            if let Some(a) = s.apply.as_ref().and_then(|a| a.message.as_deref()) {
                texts.push(("apply.message", a));
            }
            if let Some(f) = s.retry.as_ref().and_then(|r| r.feedback.as_deref()) {
                texts.push(("retry.feedback", f));
            }
            if let Some(sp) = &s.spawn {
                texts.push(("spawn.agent", &sp.agent));
                for (f, v) in [
                    ("spawn.name", &sp.name),
                    ("spawn.base", &sp.base),
                    ("spawn.profile", &sp.profile),
                ] {
                    if let Some(v) = v {
                        texts.push((f, v));
                    }
                }
            }
            if let Some(d) = &s.done_when {
                texts.push(("done_when.run", &d.run));
            }
            if let Some(l) = &s.loop_ {
                texts.push(("loop.until", &l.until));
            }
            if let Some(v) = &s.verify {
                for (f, x) in [
                    ("verify.tests", &v.tests),
                    ("verify.repro", &v.repro),
                    ("verify.judge", &v.judge),
                ] {
                    if let Some(x) = x {
                        texts.push((f, x));
                    }
                }
            }
            // Text run by `sh -c`: a placeholder there must stand bare, so its quoting holds.
            for (field, text) in &texts {
                if matches!(
                    *field,
                    "run" | "done_when.run" | "loop.until" | "verify.tests" | "verify.repro"
                ) {
                    for p in template::shell_problems(text) {
                        errs.push(format!("{at}: {field}: {p}"));
                    }
                }
            }
            for (field, text) in texts {
                match template::references(text) {
                    Ok(refs) => {
                        for r in refs {
                            if let Err(e) = self.check_ref(&r, &ancestors, s) {
                                errs.push(format!("{at}: {field}: {e}"));
                            }
                        }
                    }
                    Err(e) => errs.push(format!("{at}: {field}: {e}")),
                }
            }
            if let Some(cond) = &s.when {
                match expr::parse(cond) {
                    Ok(e) => {
                        for r in e.paths() {
                            if let Err(e) = self.check_ref(&r, &ancestors, s) {
                                errs.push(format!("{at}: if: {e}"));
                            }
                        }
                    }
                    Err(e) => errs.push(format!("{at}: if: {e}")),
                }
            }
        }
        if let Some(cycle) = self.cycle() {
            errs.push(format!("needs form a cycle: {}", cycle.join(" → ")));
        }
        if let Some(w) = &self.budget.wall
            && ptyro_core::config::parse_duration(w).is_err()
        {
            errs.push(format!("budget.wall: bad duration {w:?}"));
        }
        if errs.is_empty() { Ok(()) } else { Err(Invalid(errs)) }
    }

    /// A reference (`vars.task`, `steps.impl.outputs.head`) is readable from step `s`.
    fn check_ref(&self, path: &str, ancestors: &BTreeSet<String>, s: &Step) -> Result<(), String> {
        let parts: Vec<&str> = path.split('.').collect();
        match parts.as_slice() {
            ["vars", name, ..] => {
                if self.vars.contains_key(*name) {
                    Ok(())
                } else {
                    Err(format!("unknown variable {name:?} (declare it under vars)"))
                }
            }
            ["steps", id, rest @ ..] => {
                if self.step(id).is_none() {
                    return Err(format!("unknown step {id:?}"));
                }
                if !ancestors.contains(*id) {
                    return Err(format!("reads step {id:?}, so it must need it"));
                }
                match rest {
                    [] => {
                        Err(format!("say what of step {id:?}: status, exit_code or outputs.<key>"))
                    }
                    ["status" | "exit_code" | "error" | "session" | "worktree" | "attempts"] => {
                        Ok(())
                    }
                    ["outputs", _, ..] => Ok(()),
                    _ => Err(format!(
                        "steps.{id}: use status, exit_code, error, session, worktree or outputs.<key>"
                    )),
                }
            }
            ["gates", id, rest @ ..] => {
                if !self.step(id).is_some_and(|g| g.gate.is_some()) {
                    return Err(format!("{id:?} is not a gate step"));
                }
                if !ancestors.contains(*id) {
                    return Err(format!("reads gate {id:?}, so it must need it"));
                }
                match rest {
                    ["choice" | "note" | "by"] => Ok(()),
                    _ => Err(format!("gates.{id}: use choice, note or by")),
                }
            }
            ["run", "id" | "dir" | "base" | "project" | "name"] => Ok(()),
            ["loop", "iteration" | "last_check"] if s.loop_.is_some() => Ok(()),
            ["attempt", "n" | "error"] => Ok(()),
            ["item"] => Ok(()),
            _ => Err(format!(
                "unknown reference {path:?} (vars.*, steps.<id>.*, gates.<id>.*, run.*, attempt.*)"
            )),
        }
    }

    fn cycle(&self) -> Option<Vec<String>> {
        // Depth-first with colours: 1 = on the stack, 2 = done.
        fn visit(
            r: &Recipe,
            id: &str,
            colour: &mut BTreeMap<String, u8>,
            stack: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            match colour.get(id) {
                Some(2) => return None,
                Some(1) => {
                    let from = stack.iter().position(|s| s == id).unwrap_or(0);
                    let mut c = stack[from..].to_vec();
                    c.push(id.to_owned());
                    return Some(c);
                }
                _ => {}
            }
            colour.insert(id.to_owned(), 1);
            stack.push(id.to_owned());
            for n in r.step(id).map(|s| s.needs.clone()).unwrap_or_default() {
                if r.step(&n).is_some()
                    && let Some(c) = visit(r, &n, colour, stack)
                {
                    return Some(c);
                }
            }
            stack.pop();
            colour.insert(id.to_owned(), 2);
            None
        }
        let mut colour = BTreeMap::new();
        for s in &self.steps {
            let mut stack = Vec::new();
            if let Some(c) = visit(self, &s.id, &mut colour, &mut stack) {
                return Some(c);
            }
        }
        None
    }

    /// Resolve `--var k=v` against the declared variables: defaults filled in, required ones
    /// present, unknown ones refused.
    pub fn bind(
        &self,
        given: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, Invalid> {
        let mut out = BTreeMap::new();
        let mut errs = Vec::new();
        for k in given.keys() {
            if !self.vars.contains_key(k) {
                errs.push(format!(
                    "unknown variable {k:?} (this recipe takes: {})",
                    self.vars.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        for (k, d) in &self.vars {
            match given.get(k).or(d.default.as_ref()) {
                Some(v) => {
                    out.insert(k.clone(), v.clone());
                }
                None if d.required => errs.push(format!("missing --var {k}=…")),
                None => {
                    out.insert(k.clone(), String::new());
                }
            }
        }
        if errs.is_empty() { Ok(out) } else { Err(Invalid(errs)) }
    }

    /// The step graph as Mermaid or Graphviz dot (`ptyro recipe graph`).
    pub fn graph(&self, format: &str) -> Result<String, String> {
        let label = |s: &Step| {
            let k = s.kind().map_or("?", Kind::label);
            let extra = match (&s.spawn, &s.gate) {
                (Some(sp), _) => format!(" {}", sp.agent),
                (_, Some(g)) => {
                    // Templates read badly in a graph: `{{steps.impl.outputs.x}}` → `…`.
                    let mut ask = String::new();
                    let mut rest = g.ask();
                    while let Some(i) = rest.find("{{") {
                        ask.push_str(&rest[..i]);
                        ask.push('…');
                        rest = rest[i..].find("}}").map_or("", |j| &rest[i + j + 2..]);
                    }
                    ask.push_str(rest);
                    format!(" {:?}", ask.chars().take(30).collect::<String>())
                }
                _ => String::new(),
            };
            format!("{}: {k}{extra}", s.id).replace('"', "'")
        };
        match format {
            "mermaid" => {
                let mut o = String::from("flowchart TD\n");
                for s in &self.steps {
                    o.push_str(&format!("  {}[\"{}\"]\n", s.id, label(s)));
                }
                for s in &self.steps {
                    for n in &s.needs {
                        let arrow = if s.when.is_some() { "-.->" } else { "-->" };
                        o.push_str(&format!("  {n} {arrow} {}\n", s.id));
                    }
                }
                Ok(o)
            }
            "dot" => {
                let mut o = format!("digraph \"{}\" {{\n  rankdir=TB;\n", self.name);
                for s in &self.steps {
                    o.push_str(&format!("  \"{}\" [label=\"{}\"];\n", s.id, label(s)));
                }
                for s in &self.steps {
                    for n in &s.needs {
                        let style = if s.when.is_some() { " [style=dashed]" } else { "" };
                        o.push_str(&format!("  \"{n}\" -> \"{}\"{style};\n", s.id));
                    }
                }
                o.push_str("}\n");
                Ok(o)
            }
            other => Err(format!("unknown format {other:?} (mermaid or dot)")),
        }
    }
}

/// The sha a run pins its recipe by.
pub fn sha(src: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(src.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "implement-and-review"
vars = { task = { required = true } }
[budget]
wall = "3h"
sessions = 4

[[step]]
id = "impl"
spawn = { agent = "claude", worktree = "new", name = "impl" }
prompt = "Implement: {{vars.task}}."
done_when = { run = "scripts.run.test", max_bounces = 3 }
timeout = { total = "45m", stall = "10m" }

[[step]]
id = "review"
needs = ["impl"]
spawn = { agent = "codex", worktree = "impl" }
prompt = "Review {{steps.impl.outputs.base}}..{{steps.impl.outputs.head}}"
output = { schema = "builtin:verdict" }

[[step]]
id = "fix"
needs = ["review"]
if = "steps.review.outputs.decision == 'changes'"
send = { to = "impl", text = "Reviewer found:\n{{steps.review.outputs.issues}}" }

[[step]]
id = "gate"
needs = ["review", "fix"]
join = "all_settled"
gate = { ask = "Ship impl?", options = ["pr", "iterate", "discard"], timeout = "8h" }

[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'pr'"
apply = { from = "impl", actions = ["push", "pr"] }
"#;

    #[test]
    fn a_good_recipe_parses() {
        let r = Recipe::parse(GOOD).unwrap();
        assert_eq!(r.steps.len(), 5);
        assert_eq!(r.step("gate").unwrap().kind(), Some(Kind::Gate));
        assert_eq!(r.ancestors("ship").len(), 4);
        assert!(r.graph("mermaid").unwrap().contains("gate -.-> ship"));
        assert!(r.graph("dot").unwrap().contains("\"impl\" -> \"review\""));
    }

    fn errors(src: &str) -> String {
        Recipe::parse(src).unwrap_err().to_string()
    }

    #[test]
    fn validation_explains_mistakes() {
        let e = errors(
            r#"name = "x"
[[step]]
id = "a"
spawn = { agent = "claude" }
prompt = "{{vars.nope}} {{steps.b.outputs.x}}"
[[step]]
id = "b"
needs = ["a", "ghost"]
run = "true"
gate = "both?"
"#,
        );
        assert!(e.contains("unknown variable \"nope\""), "{e}");
        assert!(e.contains("reads step \"b\", so it must need it"), "{e}");
        assert!(e.contains("needs unknown step \"ghost\""), "{e}");
        assert!(e.contains("one kind per step, not run + gate"), "{e}");
        let e = errors(
            "name = \"x\"\n[[step]]\nid = \"a\"\nneeds = [\"b\"]\nrun = \"true\"\n[[step]]\nid = \"b\"\nneeds = [\"a\"]\nrun = \"true\"\n",
        );
        assert!(e.contains("cycle"), "{e}");
        let e = errors("name = \"x\"\n[[step]]\nid = \"a\"\nsleep = \"5 minutes\"\n");
        assert!(e.contains("bad duration"), "{e}");
        let e = errors("name = \"x\"\n[[step]]\nid = \"a\"\nrun = \"true\"\nbogus = 1\n");
        assert!(e.contains("unknown field"), "{e}");
        // Shell text: a placeholder inside quotes would let a value end them.
        let e = errors(
            "name = \"x\"\nvars = { v = { default = \"1\" } }\n[[step]]\nid = \"a\"\nrun = \"echo '{{vars.v}}'\"\n[[step]]\nid = \"b\"\nneeds = [\"a\"]\nrun = \"test \\\"{{steps.a.outputs.x}}\\\"\"\n",
        );
        assert!(e.contains("step \"a\": run: {{vars.v}} stands inside quotes"), "{e}");
        assert!(e.contains("step \"b\": run: {{steps.a.outputs.x}} stands inside quotes"), "{e}");
        // Bare is what is wanted (and prompts, which no shell reads, may quote freely).
        assert!(Recipe::parse(
            "name = \"x\"\nvars = { v = { default = \"1\" } }\n[[step]]\nid = \"a\"\nrun = \"echo {{vars.v}} && true\"\n[[step]]\nid = \"b\"\nneeds = [\"a\"]\nspawn = { agent = \"claude\" }\nprompt = \"say '{{vars.v}}'\"\n"
        )
        .unwrap()
        .validate()
        .is_ok());
        let e = errors(
            "name = \"x\"\n[[step]]\nid = \"a\"\nrun = \"true\"\n[[step]]\nid = \"b\"\nif = \"steps.a.status == \"\nrun = \"x\"\nneeds=[\"a\"]\n",
        );
        assert!(e.contains("if:"), "{e}");
    }

    #[test]
    fn variables_bind_with_defaults() {
        let r = Recipe::parse(
            "name = \"x\"\nvars = { a = { required = true }, b = { default = \"2\" } }\n[[step]]\nid = \"s\"\nrun = \"echo {{vars.a}} {{vars.b}}\"\n",
        )
        .unwrap();
        let got = r.bind(&BTreeMap::from([("a".to_owned(), "1".to_owned())])).unwrap();
        assert_eq!(got["b"], "2");
        assert!(r.bind(&BTreeMap::new()).unwrap_err().to_string().contains("missing --var a"));
        assert!(
            r.bind(&BTreeMap::from([("a".into(), "1".into()), ("c".into(), "3".into())]))
                .unwrap_err()
                .to_string()
                .contains("unknown variable \"c\"")
        );
    }
}
