//! Runs (SPEC §15, M7.1–M7.5, M7.12): recipes, fan-outs and lead agents, executed by one actor
//! per run.
//!
//! Every change is an event ([`RunEvent`]) appended through [`Daemon::emit`], which folds it into
//! the run's state and writes both, in one transaction, to the store. The actor asks the pure
//! engine (`ptyro_run::plan`) what to do next and starts step tasks; step tasks report back only
//! through events. So a run survives a daemon restart: [`Daemon::start_runs`] reloads the state
//! and picks every in-flight step up again (agents resume their own conversation when they can,
//! scripts count as crashed, gates and timers keep their absolute deadlines).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::model::{WorktreeInfo, WorktreeSpec};
use ptyro_core::session::{ProcessState, SpawnSpec};
use ptyro_core::status::{AgentStatus, AttentionKind, InboxEntry};
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{DomainEvent, Reply, Request};
use ptyro_run::recipe::{Kind, OnRestart, RetryMode, Timeout, TimeoutFull, WaitFor};
use ptyro_run::wire::{FanoutSpec, GateInfo, RecipeInfo, RunInfo};
use ptyro_run::{
    Action, Gate, Policy, Recipe, RunEvent, RunKind, RunState, RunStatus, Step, StepStatus,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::daemon::Daemon;
use crate::session::Session;
use crate::worktrees::blocking;

/// Tripwires a recipe step puts on its sessions (enforced by PreToolUse, `verify.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Guard {
    pub deny_paths: Vec<String>,
    pub deny_cmds: Vec<String>,
}

pub(crate) struct RunEntry {
    pub state: RunState,
    pub recipe: Option<Arc<Recipe>>,
    pub src: Option<String>,
    pub policy: Option<Policy>,
    /// Step tasks in flight.
    tasks: HashMap<String, AbortHandle>,
    pub wake: Arc<Notify>,
}

pub(crate) type Runs = HashMap<String, RunEntry>;

/// What a fork starts from: the run, the step, and the steps it copies.
pub(crate) type Forked = (String, String, Vec<(String, ptyro_run::StepState)>);

/// How an attempt ended.
#[derive(Debug, Clone, Default)]
pub(crate) struct StepResult {
    pub outcome: String,
    pub error: Option<String>,
    pub outputs: BTreeMap<String, Value>,
    pub decided_by: Option<String>,
    pub end_ref: Option<String>,
    /// Wait for `ptyro run retry` instead of failing.
    pub hold: bool,
}

impl StepResult {
    pub fn ok(decided_by: &str) -> Self {
        Self { outcome: "succeeded".into(), decided_by: Some(decided_by.into()), ..Self::default() }
    }

    pub fn fail(outcome: &str, error: impl Into<String>) -> Self {
        Self { outcome: outcome.into(), error: Some(error.into()), ..Self::default() }
    }

    fn with(mut self, k: &str, v: Value) -> Self {
        self.outputs.insert(k.into(), v);
        self
    }
}

fn dur(s: Option<&str>) -> Option<Duration> {
    s.and_then(|s| ptyro_core::config::parse_duration(s).ok()).filter(|d| !d.is_zero())
}

/// An agent may write its answer file a moment after its turn ends: give it a little time.
pub(crate) async fn wait_for_file(path: &std::path::Path, within: Duration) {
    let deadline = std::time::Instant::now() + within;
    while !path.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The last lines of a command's output.
pub(crate) fn tail(text: &str, lines: usize) -> String {
    let v: Vec<&str> = text.lines().collect();
    let start = v.len().saturating_sub(lines);
    v[start..].join("\n").chars().rev().take(4000).collect::<String>().chars().rev().collect()
}

impl Daemon {
    pub(crate) fn run_dir(&self, id: &str) -> PathBuf {
        self.paths.data.join("runs").join(id)
    }

    fn row(e: &RunEntry) -> ptyro_store::RunRow {
        let s = &e.state;
        ptyro_store::RunRow {
            id: s.id.clone(),
            kind: s.kind.label().into(),
            name: s.name.clone(),
            recipe_src: e.src.clone(),
            recipe_sha: Some(s.recipe_sha.clone()),
            status: s.status.label().into(),
            state: json!({ "state": s, "policy": e.policy }),
            project: s.project.clone(),
            lead: s.lead.clone(),
            forked_from: s.forked_from.as_ref().map(|f| f.0.clone()),
            fork_step: s.forked_from.as_ref().map(|f| f.1.clone()),
            created_ms: s.created_ms,
            ended_ms: s.ended_ms,
        }
    }

    pub(crate) fn info_of(&self, e: &RunEntry, all: &Runs) -> RunInfo {
        let mut order: Vec<String> = e
            .recipe
            .as_ref()
            .map(|r| r.steps.iter().map(|s| s.id.clone()).collect())
            .unwrap_or_default();
        for k in e.state.steps.keys() {
            if !order.contains(k) {
                order.push(k.clone());
            }
        }
        RunInfo {
            state: e.state.clone(),
            order,
            dir: self.run_dir(&e.state.id).to_string_lossy().into_owned(),
            children: all
                .values()
                .filter(|o| o.state.forked_from.as_ref().is_some_and(|f| f.0 == e.state.id))
                .map(|o| o.state.id.clone())
                .collect(),
        }
    }

    pub(crate) fn run_info(&self, id: &str) -> Option<RunInfo> {
        let runs = self.runs.lock().expect("runs");
        runs.get(id).map(|e| self.info_of(e, &runs))
    }

    /// Append events to a run: fold them into its state, persist both in one transaction,
    /// tell clients, wake the actor.
    pub(crate) fn emit(&self, id: &str, evs: Vec<RunEvent>) {
        if evs.is_empty() {
            return;
        }
        let at = now_ms();
        let (row, rows, info, wake) = {
            let mut runs = self.runs.lock().expect("runs");
            let Some(e) = runs.get_mut(id) else { return };
            let mut rows = Vec::new();
            for ev in evs {
                // Endings are final: a racing second "ended" (a cancel and the actor; a train
                // and the set's tick) or a late "cancelled" for a step that already finished
                // must not overwrite what happened.
                let late = match &ev {
                    RunEvent::Ended { .. } => e.state.status.is_over(),
                    RunEvent::StepEnded { step, .. } => {
                        e.state.steps.get(step).is_some_and(|s| s.status.is_over())
                    }
                    RunEvent::AttemptEnded { step, outcome, .. } if outcome == "cancelled" => {
                        e.state.steps.get(step).is_some_and(|s| s.status.is_over())
                    }
                    _ => false,
                };
                if late {
                    continue;
                }
                e.state.apply(&ev);
                rows.push((
                    e.state.seq,
                    at,
                    ev.kind().to_owned(),
                    ev.step().map(str::to_owned),
                    serde_json::to_value(&ev).unwrap_or_default(),
                ));
            }
            let e = &runs[id];
            (Self::row(e), rows, self.info_of(e, &runs), Arc::clone(&e.wake))
        };
        if let Some(store) = &self.store {
            store.run_append(row, rows);
        }
        let _ = self.events.send(DomainEvent::RunUpdated { run: info });
        wake.notify_waiters();
        wake.notify_one();
    }

    fn run_state(&self, id: &str) -> Option<RunState> {
        self.runs.lock().expect("runs").get(id).map(|e| e.state.clone())
    }

    fn run_recipe(&self, id: &str) -> Option<Arc<Recipe>> {
        self.runs.lock().expect("runs").get(id).and_then(|e| e.recipe.clone())
    }

    /// Resolve a run address: an id, a unique id prefix/suffix, or `@last`.
    pub(crate) fn find_run(&self, addr: &str) -> Result<String, Error> {
        let runs = self.runs.lock().expect("runs");
        if addr == "@last" {
            return runs
                .values()
                .max_by_key(|e| e.state.created_ms)
                .map(|e| e.state.id.clone())
                .ok_or_else(|| Error::not_found("no runs yet"));
        }
        if runs.contains_key(addr) {
            return Ok(addr.to_owned());
        }
        let bare = addr.trim_start_matches("r-");
        let m: Vec<&String> =
            runs.keys().filter(|k| k.trim_start_matches("r-").contains(bare)).collect();
        match m.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Error::missing("run", addr)),
            _ => {
                Err(Error::new(ErrorCode::Ambiguous, format!("{addr:?} matches {} runs", m.len())))
            }
        }
    }

    // ── creating runs ──────────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_run(
        self: &Arc<Self>,
        kind: RunKind,
        recipe: Option<Recipe>,
        src: Option<String>,
        vars: BTreeMap<String, String>,
        project: Option<String>,
        base: Option<String>,
        lead: Option<String>,
        policy: Option<Policy>,
        forked: Option<Forked>,
    ) -> String {
        let id = loop {
            let id = ptyro_run::short_id("r", 6);
            if !self.runs.lock().expect("runs").contains_key(&id) {
                break id;
            }
        };
        let name = recipe
            .as_ref()
            .map(|r| r.name.clone())
            .or_else(|| vars.get("set").cloned())
            .unwrap_or_else(|| kind.label().to_owned());
        let steps: Vec<String> = recipe
            .as_ref()
            .map(|r| r.steps.iter().map(|s| s.id.clone()).collect())
            .unwrap_or_default();
        let sha = src.as_deref().map(ptyro_run::recipe::sha).unwrap_or_default();
        let _ = std::fs::create_dir_all(self.run_dir(&id));
        let has_recipe = recipe.is_some();
        self.runs.lock().expect("runs").insert(
            id.clone(),
            RunEntry {
                state: RunState::new(&id),
                recipe: recipe.map(Arc::new),
                src,
                policy,
                tasks: HashMap::new(),
                wake: Arc::new(Notify::new()),
            },
        );
        let mut evs = vec![RunEvent::Created {
            kind,
            name,
            recipe_sha: sha,
            vars,
            project,
            base,
            lead: lead.clone(),
            forked_from: forked.as_ref().map(|(r, s, _)| (r.clone(), s.clone())),
            steps,
            at: now_ms(),
        }];
        if let Some((_, _, copied)) = forked {
            for (step, state) in copied {
                evs.push(RunEvent::StepCopied { step, state });
            }
        }
        if let Some(l) = lead {
            evs.push(RunEvent::SessionAdded { session: l });
        }
        self.emit(&id, evs);
        if has_recipe {
            self.start_actor(&id);
        }
        id
    }

    /// Bring back the runs an earlier daemon left running (call after dormant sessions are
    /// restored, so agents can resume into them).
    pub async fn start_runs(self: &Arc<Self>) {
        let Some(store) = self.store.clone() else { return };
        let rows = match store.runs(true, 500).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("runs: {e}");
                return;
            }
        };
        for row in rows {
            let state: Option<RunState> = serde_json::from_value(row.state["state"].clone()).ok();
            let state = match state {
                Some(s) => s,
                // The projection is a cache: rebuild it from the log.
                None => {
                    let evs = store.run_events(row.id.clone()).await.unwrap_or_default();
                    let evs: Vec<RunEvent> = evs
                        .into_iter()
                        .filter_map(|(_, _, p)| serde_json::from_value(p).ok())
                        .collect();
                    RunState::replay(&row.id, &evs)
                }
            };
            let policy: Option<Policy> = serde_json::from_value(row.state["policy"].clone()).ok();
            let recipe = row.recipe_src.as_deref().and_then(|s| Recipe::parse(s).ok());
            let has_recipe = recipe.is_some();
            self.runs.lock().expect("runs").insert(
                row.id.clone(),
                RunEntry {
                    state,
                    recipe: recipe.map(Arc::new),
                    src: row.recipe_src.clone(),
                    policy,
                    tasks: HashMap::new(),
                    wake: Arc::new(Notify::new()),
                },
            );
            self.emit(
                &row.id,
                vec![RunEvent::Note { text: "the daemon restarted; resuming".into() }],
            );
            // Sessions that came back dormant remember which step they work for.
            if let Some(st) = self.run_state(&row.id) {
                for sid in &st.sessions {
                    let step = if st.lead.as_deref() == Some(sid.as_str()) {
                        Some("lead".to_owned())
                    } else {
                        st.step_of_session(sid).map(str::to_owned)
                    };
                    if let (Some(step), Ok(s)) = (step, self.resolve(sid)) {
                        s.meta.lock().expect("meta").run = Some((row.id.clone(), step));
                    }
                }
            }
            if has_recipe {
                self.recover(&row.id).await;
                self.start_actor(&row.id);
            }
            if self.run_state(&row.id).is_some_and(|s| s.kind == RunKind::Changeset) {
                self.start_cs_actor(&row.id);
            }
            tracing::info!(run = %row.id, "run resumed");
        }
        // Finished runs stay listable.
        if let Ok(rows) = store.runs(false, 200).await {
            for row in rows {
                if self.runs.lock().expect("runs").contains_key(&row.id) {
                    continue;
                }
                let Ok(state) = serde_json::from_value::<RunState>(row.state["state"].clone())
                else {
                    continue;
                };
                let recipe = row.recipe_src.as_deref().and_then(|s| Recipe::parse(s).ok());
                self.runs.lock().expect("runs").insert(
                    row.id.clone(),
                    RunEntry {
                        state,
                        recipe: recipe.map(Arc::new),
                        src: row.recipe_src,
                        policy: serde_json::from_value(row.state["policy"].clone()).ok(),
                        tasks: HashMap::new(),
                        wake: Arc::new(Notify::new()),
                    },
                );
            }
        }
    }

    /// Pick up the steps that were in flight when the daemon stopped (SPEC §15.3, RESEARCH §II.3).
    async fn recover(self: &Arc<Self>, id: &str) {
        let (Some(st), Some(r)) = (self.run_state(id), self.run_recipe(id)) else { return };
        for (sid, ss) in &st.steps {
            if !matches!(ss.status, StepStatus::Running | StepStatus::Waiting) {
                continue;
            }
            let Some(step) = r.step(sid) else { continue };
            if ss.status == StepStatus::Waiting && ss.gate.is_none() {
                continue; // held: waits for `run retry`
            }
            match step.kind() {
                Some(Kind::Run) => {
                    self.emit(
                        id,
                        vec![RunEvent::AttemptEnded {
                            step: sid.clone(),
                            outcome: "crashed".into(),
                            error: Some("the daemon stopped while it ran".into()),
                            end_ref: None,
                            at: now_ms(),
                        }],
                    );
                    // A retry starts a new attempt: run it.
                    if self
                        .after_attempt(
                            id,
                            sid,
                            step,
                            StepResult::fail("crashed", "the daemon stopped while it ran"),
                        )
                        .await
                    {
                        self.spawn_step(id, sid, false);
                    }
                }
                Some(Kind::Spawn | Kind::Send) => {
                    let session = ss.attempt().and_then(|a| a.session.clone());
                    let s = session.as_deref().and_then(|x| self.resolve(x).ok());
                    let resumable = s.as_ref().is_some_and(|s| {
                        s.adapter().is_some_and(|a| ptyro_agents::adapter(&a).caps().resume)
                    });
                    let mode = step.on_restart.unwrap_or(if resumable {
                        OnRestart::NativeResume
                    } else {
                        OnRestart::Hold
                    });
                    match (mode, s) {
                        (OnRestart::NativeResume, Some(s)) => {
                            self.spawn_step(id, sid, true);
                            tracing::info!(run = id, step = %sid, session = %s.id, "resuming in place");
                        }
                        (OnRestart::Fresh, _) | (OnRestart::NativeResume, None) => {
                            self.emit(
                                id,
                                vec![
                                    RunEvent::AttemptEnded {
                                        step: sid.clone(),
                                        outcome: "crashed".into(),
                                        error: Some("the daemon stopped mid-step".into()),
                                        end_ref: None,
                                        at: now_ms(),
                                    },
                                    RunEvent::AttemptStarted {
                                        step: sid.clone(),
                                        n: ss.attempts.len() as u32 + 1,
                                        at: now_ms(),
                                    },
                                ],
                            );
                            self.spawn_step(id, sid, false);
                        }
                        (OnRestart::Hold, _) => self.emit(
                            id,
                            vec![RunEvent::StepHeld {
                                step: sid.clone(),
                                reason:
                                    "the daemon restarted mid-step; `ptyro run retry` to continue"
                                        .into(),
                            }],
                        ),
                    }
                }
                _ => self.spawn_step(id, sid, true),
            }
        }
    }

    // ── the actor ──────────────────────────────────────────────────────────

    fn start_actor(self: &Arc<Self>, id: &str) {
        let (me, id) = (Arc::clone(self), id.to_owned());
        tokio::spawn(async move { me.actor(id).await });
    }

    async fn actor(self: Arc<Self>, id: String) {
        loop {
            let Some((st, recipe, wake)) = ({
                let runs = self.runs.lock().expect("runs");
                runs.get(&id)
                    .and_then(|e| Some((e.state.clone(), e.recipe.clone()?, Arc::clone(&e.wake))))
            }) else {
                return;
            };
            if st.status.is_over() {
                return;
            }
            if let Some(wall) = dur(recipe.budget.wall.as_deref())
                && now_ms().saturating_sub(st.created_ms) > wall.as_millis() as u64
            {
                self.finish_run(
                    &id,
                    RunStatus::Failed,
                    Some(format!(
                        "budget: the run's {} wall clock ran out",
                        recipe.budget.wall.clone().unwrap_or_default()
                    )),
                )
                .await;
                return;
            }
            let dir = self.run_dir(&id).to_string_lossy().into_owned();
            let actions = ptyro_run::plan(&recipe, &st, &dir);
            let acted = !actions.is_empty();
            for a in actions {
                match a {
                    Action::Start { step } => {
                        if let Some(max) = recipe.budget.attempts
                            && st.attempts() >= max
                        {
                            self.emit(
                                &id,
                                vec![RunEvent::StepEnded {
                                    step,
                                    status: StepStatus::Failed,
                                    reason: Some(format!("budget: {max} attempts used")),
                                    decided_by: Some("budget".into()),
                                    at: now_ms(),
                                }],
                            );
                            continue;
                        }
                        self.emit(
                            &id,
                            vec![RunEvent::AttemptStarted {
                                step: step.clone(),
                                n: 1,
                                at: now_ms(),
                            }],
                        );
                        self.spawn_step(&id, &step, false);
                    }
                    Action::Skip { step, reason } => self.emit(
                        &id,
                        vec![RunEvent::StepEnded {
                            step,
                            status: StepStatus::Skipped,
                            reason: Some(reason),
                            decided_by: None,
                            at: now_ms(),
                        }],
                    ),
                    Action::Fail { step, reason } => self.emit(
                        &id,
                        vec![RunEvent::StepEnded {
                            step,
                            status: StepStatus::Failed,
                            reason: Some(reason),
                            decided_by: Some("join".into()),
                            at: now_ms(),
                        }],
                    ),
                    Action::Cancel { step, reason } => self.cancel_step(&id, &step, &reason).await,
                    Action::Finish { status, error } => {
                        self.finish_run(&id, status, error).await;
                        return;
                    }
                }
            }
            if acted {
                continue;
            }
            tokio::select! {
                () = wake.notified() => {}
                () = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    }

    pub(crate) async fn finish_run(
        self: &Arc<Self>,
        id: &str,
        status: RunStatus,
        error: Option<String>,
    ) {
        // Ended already (a racing cancel, a second finisher): once is enough.
        if self.run_state(id).is_none_or(|s| s.status.is_over()) {
            return;
        }
        // A step that has just ended may not have removed its task yet: it isn't cancelled.
        let tasks: Vec<(String, AbortHandle)> = {
            let mut runs = self.runs.lock().expect("runs");
            runs.get_mut(id)
                .map(|e| {
                    let over =
                        |step: &str| e.state.steps.get(step).is_some_and(|s| s.status.is_over());
                    e.tasks.drain().filter(|(step, _)| !over(step)).collect()
                })
                .unwrap_or_default()
        };
        for (step, t) in tasks {
            t.abort();
            self.emit(
                id,
                vec![RunEvent::StepEnded {
                    step,
                    status: StepStatus::Cancelled,
                    reason: Some("the run ended".into()),
                    decided_by: None,
                    at: now_ms(),
                }],
            );
        }
        self.emit(id, vec![RunEvent::Ended { status, error: error.clone(), at: now_ms() }]);
        let Some(st) = self.run_state(id) else { return };
        let mut body = format!("{} {}", st.name, status.label());
        let counts = st.steps.values().fold(BTreeMap::new(), |mut m: BTreeMap<&str, u32>, s| {
            *m.entry(s.status.label()).or_default() += 1;
            m
        });
        body.push_str(&format!(
            " — {}",
            counts.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", ")
        ));
        if let Some(e) = &error {
            body.push_str(&format!("\n{e}"));
        }
        body.push_str(&format!("\nptyro run show {id}"));
        self.notify_run(id, AttentionKind::Run, format!("run {id} {}", status.label()), body).await;
        tracing::info!(run = id, status = status.label(), "run ended");
    }

    /// An inbox entry and notification about a run (no session of its own).
    pub(crate) async fn notify_run(
        self: &Arc<Self>,
        id: &str,
        kind: AttentionKind,
        title: String,
        body: String,
    ) {
        let st = self.run_state(id);
        let session = st
            .as_ref()
            .and_then(|s| s.lead.clone().or_else(|| s.sessions.last().cloned()))
            .unwrap_or_else(|| format!("run:{id}"));
        let workspace = st
            .as_ref()
            .and_then(|s| s.project.as_deref())
            .and_then(|p| self.project_by_id(p))
            .and_then(|p| self.home_workspace(None, Some(&p)).ok());
        let mut entry = InboxEntry {
            id: 0,
            session: session.clone(),
            session_name: format!("run {id}"),
            at_ms: now_ms(),
            kind,
            title: title.clone(),
            body: body.clone(),
            read_ms: None,
            workspace: workspace.clone(),
            profile: None,
        };
        entry.id = match &self.store {
            Some(st) => st.inbox_add(entry.clone()).await.unwrap_or(0),
            None => {
                let mut mem = self.inbox_mem.lock().expect("inbox");
                let n = mem.len() as i64 + 1;
                mem.push(InboxEntry { id: n, ..entry.clone() });
                n
            }
        };
        let _ = self.events.send(DomainEvent::InboxAdded { entry });
        let config = self.config();
        let n = &config.notifications;
        if !n.on.iter().any(|k| k == kind.label()) {
            return;
        }
        // Quiet hours hold everything but breakthrough kinds (gates, by default).
        let breakthrough = config.attention.breakthrough.iter().any(|k| k == kind.label());
        if !breakthrough
            && let Some(q) =
                n.quiet_hours.as_deref().and_then(|q| ptyro_core::config::parse_quiet_hours(q).ok())
            && let Some((h, m)) = ptyro_pty::local_time_hm()
            && ptyro_core::config::in_quiet_hours(q, u16::from(h) * 60 + u16::from(m))
        {
            return;
        }
        let note = crate::notifier::Note {
            session,
            session_name: format!("run {id}"),
            kind,
            title,
            body,
            profile: ptyro_core::model::DEFAULT_PROFILE.into(),
            workspace,
            project: None,
            approve_url: None,
            deny_url: None,
        };
        self.deliver(&note, false).await;
    }

    async fn cancel_step(self: &Arc<Self>, id: &str, step: &str, reason: &str) {
        let task = self.runs.lock().expect("runs").get_mut(id).and_then(|e| e.tasks.remove(step));
        if let Some(t) = task {
            t.abort();
        }
        if let Some(s) = self
            .run_state(id)
            .and_then(|st| {
                st.steps.get(step).and_then(|s| s.attempt()).and_then(|a| a.session.clone())
            })
            .and_then(|s| self.resolve(&s).ok())
            .filter(|s| s.is_live())
        {
            self.stop_session(&s).await;
        }
        self.emit(
            id,
            vec![
                RunEvent::AttemptEnded {
                    step: step.into(),
                    outcome: "cancelled".into(),
                    error: Some(reason.into()),
                    end_ref: None,
                    at: now_ms(),
                },
                RunEvent::StepEnded {
                    step: step.into(),
                    status: StepStatus::Cancelled,
                    reason: Some(reason.into()),
                    decided_by: None,
                    at: now_ms(),
                },
            ],
        );
    }

    fn spawn_step(self: &Arc<Self>, id: &str, step: &str, resume: bool) {
        let (me, run, sid) = (Arc::clone(self), id.to_owned(), step.to_owned());
        let h = tokio::spawn(async move { me.exec_step(run, sid, resume).await });
        if let Some(e) = self.runs.lock().expect("runs").get_mut(id) {
            e.tasks.insert(step.to_owned(), h.abort_handle());
        }
    }

    async fn exec_step(self: Arc<Self>, run: String, step: String, mut resume: bool) {
        let Some(recipe) = self.run_recipe(&run) else { return };
        let Some(def) = recipe.step(&step).cloned() else { return };
        loop {
            let res = self.attempt(&run, &def, resume).await;
            resume = false;
            self.emit(
                &run,
                vec![
                    RunEvent::Outputs { step: step.clone(), outputs: res.outputs.clone() },
                    RunEvent::AttemptEnded {
                        step: step.clone(),
                        outcome: res.outcome.clone(),
                        error: res.error.clone(),
                        end_ref: res.end_ref.clone(),
                        at: now_ms(),
                    },
                ],
            );
            if !self.after_attempt(&run, &step, &def, res).await {
                break;
            }
        }
        if let Some(e) = self.runs.lock().expect("runs").get_mut(&run) {
            e.tasks.remove(&step);
        }
    }

    /// End the step, or start another attempt (`true`).
    async fn after_attempt(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        def: &Step,
        res: StepResult,
    ) -> bool {
        if res.outcome == "succeeded" {
            self.emit(
                run,
                vec![RunEvent::StepEnded {
                    step: step.into(),
                    status: StepStatus::Succeeded,
                    reason: None,
                    decided_by: res.decided_by,
                    at: now_ms(),
                }],
            );
            return false;
        }
        if res.hold {
            self.emit(
                run,
                vec![RunEvent::StepHeld {
                    step: step.into(),
                    reason: res.error.unwrap_or_else(|| "held".into()),
                }],
            );
            let _ = self
                .notify_run(
                    run,
                    AttentionKind::Gate,
                    format!("run {run}: step {step} is held"),
                    format!("ptyro run retry {run} {step} to continue"),
                )
                .await;
            return false;
        }
        let (retry, n) = {
            let runs = self.runs.lock().expect("runs");
            let Some(e) = runs.get(run) else { return false };
            let ss = e.state.steps.get(step).cloned().unwrap_or_default();
            let budget_ok = e
                .recipe
                .as_ref()
                .and_then(|r| r.budget.attempts)
                .is_none_or(|m| e.state.attempts() < m);
            (
                ptyro_run::should_retry(def, &ss, &res.outcome) && budget_ok,
                ss.attempts.len() as u32 + 1,
            )
        };
        if retry {
            self.emit(run, vec![RunEvent::AttemptStarted { step: step.into(), n, at: now_ms() }]);
            return true;
        }
        let status = match res.outcome.as_str() {
            "crashed" => StepStatus::Crashed,
            "cancelled" => StepStatus::Cancelled,
            _ => StepStatus::Failed,
        };
        self.emit(
            run,
            vec![RunEvent::StepEnded {
                step: step.into(),
                status,
                reason: res.error.or(Some(res.outcome)),
                decided_by: res.decided_by,
                at: now_ms(),
            }],
        );
        false
    }

    /// One attempt, under the step's total timeout.
    async fn attempt(self: &Arc<Self>, run: &str, def: &Step, resume: bool) -> StepResult {
        let t = def.timeout.as_ref().map(Timeout::parts).unwrap_or_default();
        let fut = self.attempt_inner(run, def, resume, &t);
        match dur(t.total.as_deref()) {
            Some(total) => match tokio::time::timeout(total, fut).await {
                Ok(r) => r,
                Err(_) => {
                    // Stop the agent; its worktree and conversation stay.
                    if let Some(s) = self.attempt_session(run, &def.id).filter(|s| s.is_live()) {
                        self.stop_session(&s).await;
                    }
                    StepResult::fail(
                        "timeout",
                        format!("still not done after {}", t.total.unwrap_or_default()),
                    )
                }
            },
            None => fut.await,
        }
    }

    fn attempt_session(&self, run: &str, step: &str) -> Option<Arc<Session>> {
        let id = self.run_state(run)?.steps.get(step)?.attempt()?.session.clone()?;
        self.resolve(&id).ok()
    }

    /// The context templates read, with `attempt.*` filled in.
    fn ctx(&self, run: &str, step: &str) -> Value {
        let st = self.run_state(run).unwrap_or_default();
        let mut c = st.context(&self.run_dir(run).to_string_lossy());
        let ss = st.steps.get(step);
        let n = ss.map_or(1, |s| s.attempts.len());
        let prev = ss.and_then(|s| s.attempts.iter().rev().nth(1)).and_then(|a| a.error.clone());
        c["attempt"] = json!({ "n": n, "error": prev });
        c
    }

    async fn attempt_inner(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        resume: bool,
        t: &TimeoutFull,
    ) -> StepResult {
        let r = match def.kind() {
            Some(Kind::Spawn) if def.loop_.is_some() => self.exec_loop(run, def, resume, t).await,
            Some(Kind::Spawn) => self.exec_spawn(run, def, resume, t).await,
            Some(Kind::Send) => self.exec_send(run, def, resume, t).await,
            Some(Kind::Run) => self.exec_run(run, def).await,
            Some(Kind::Gate) => self.exec_gate(run, def).await,
            Some(Kind::Apply) => self.exec_apply(run, def).await,
            Some(Kind::Sleep) => self.exec_sleep(run, def).await,
            Some(Kind::Verify) => self.exec_verify(run, def).await,
            None => Err(Error::invalid("a step without a kind")),
        };
        r.unwrap_or_else(|e| StepResult::fail("failed", e.message))
    }

    // ── spawn, send, loop ─────────────────────────────────────────────────

    /// The worktree a step works in, created when it says `new`.
    async fn step_worktree(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        ctx: &Value,
    ) -> Result<WorktreeInfo, Error> {
        let st = self.run_state(run).ok_or_else(|| Error::missing("run", run))?;
        let sp = def.spawn.clone().unwrap_or_default();
        let target = sp
            .worktree
            .as_deref()
            .map(|w| ptyro_run::template::render(w, ctx))
            .unwrap_or_else(|| "new".into());
        let project = || {
            st.project.clone().ok_or_else(|| {
                Error::invalid(format!(
                    "step {} needs a project: run it with --project (or from inside a repository)",
                    def.id
                ))
            })
        };
        match target.as_str() {
            "new" => {
                // A retry or the next loop iteration reuses the step's worktree.
                if let Some(w) = st
                    .steps
                    .get(&def.id)
                    .and_then(|s| s.attempts.iter().rev().find_map(|a| a.worktree.clone()))
                    .and_then(|w| self.worktree_by_id(&w))
                {
                    return Ok(w);
                }
                let project = project()?;
                let p = self
                    .project_by_id(&project)
                    .ok_or_else(|| Error::missing("project", &project))?;
                let n = sp
                    .name
                    .as_deref()
                    .map(|n| ptyro_run::template::render(n, ctx))
                    .unwrap_or_else(|| def.id.clone());
                // Fanout names are already unique (`<prefix>-<agent>`); recipes add the run.
                let want = if st.kind == RunKind::Fanout {
                    n
                } else {
                    format!("{}-{n}", run.trim_start_matches("r-"))
                };
                let name = self.free_worktree_name(&p.id, &ptyro_core::model::slugify(&want));
                let base = sp
                    .base
                    .as_deref()
                    .map(|b| ptyro_run::template::render(b, ctx))
                    .or(st.base.clone());
                let spec = WorktreeSpec {
                    project: p.id.clone(),
                    name: Some(name),
                    base,
                    profile: sp.profile.as_deref().map(|x| ptyro_run::template::render(x, ctx)),
                    confirmed: true,
                    ..WorktreeSpec::default()
                };
                let Reply::WorktreeCreated { info, setup, .. } = self.create_worktree(spec).await?
                else {
                    return Err(Error::internal("unexpected reply creating a worktree"));
                };
                if let Some(setup) = setup.and_then(|s| self.session(s.id)) {
                    // Agents start once setup has finished (and only if it worked).
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1200), setup.wait_exit()).await;
                    let state = setup.meta.lock().expect("meta").state.clone();
                    if !matches!(state, ProcessState::Exited { code: Some(0), .. }) {
                        return Err(Error::conflict(format!(
                            "{}'s setup script failed ({})",
                            info.qualified(),
                            state.label()
                        )));
                    }
                }
                Ok(info)
            }
            "root" | "@root" => {
                let project = project()?;
                self.root_worktree(&project)
                    .ok_or_else(|| Error::missing("root worktree", &project))
            }
            other => {
                if let Some(w) =
                    st.steps.get(other).and_then(|s| s.attempt()).and_then(|a| a.worktree.clone())
                {
                    return self.worktree_by_id(&w).ok_or_else(|| Error::missing("worktree", w));
                }
                self.find_worktree(other)
            }
        }
    }

    pub(crate) fn free_worktree_name(&self, project: &str, want: &str) -> String {
        let taken = |n: &str| {
            self.registry
                .lock()
                .expect("registry")
                .worktrees
                .iter()
                .any(|w| w.project == project && w.name == n)
        };
        if !taken(want) {
            return want.to_owned();
        }
        (2..).map(|k| format!("{want}-{k}")).find(|n| !taken(n)).expect("infinite")
    }

    /// `refs/ptyro/runs/<run>/<step>/<n>/<which>` for the worktree as it is now.
    async fn snapshot_ref(
        &self,
        wt: &WorktreeInfo,
        run: &str,
        step: &str,
        n: usize,
        which: &str,
    ) -> Option<String> {
        let path = wt.path.clone();
        let name = format!("refs/ptyro/runs/{run}/{step}/{n}/{which}");
        let msg = format!("ptyro run {run} step {step} attempt {n} {which}");
        blocking(move || {
            let g = ptyro_git::Git::new();
            let s = g.checkpoint(&path, &msg).ok()?;
            g.update_ref(&path, &name, &s.commit).ok()?;
            Some(name)
        })
        .await
        .ok()
        .flatten()
    }

    fn step_env(&self, run: &str, step: &str) -> BTreeMap<String, String> {
        let dir = self.run_dir(run).join(step);
        let _ = std::fs::create_dir_all(&dir);
        BTreeMap::from([
            ("PTYRO_RUN_ID".to_owned(), run.to_owned()),
            ("PTYRO_STEP_ID".to_owned(), step.to_owned()),
            ("PTYRO_RUN_DIR".to_owned(), self.run_dir(run).to_string_lossy().into_owned()),
            ("PTYRO_STEP_OUT".to_owned(), dir.join("out.json").to_string_lossy().into_owned()),
        ])
    }

    /// Start an agent for a step (in `wt`), typed `prompt` once it's ready.
    async fn start_agent(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        wt: &WorktreeInfo,
        prompt: String,
        ctx: &Value,
    ) -> Result<Arc<Session>, Error> {
        let sp = def.spawn.clone().unwrap_or_default();
        let agent = ptyro_run::template::render(&sp.agent, ctx);
        if let Some(max) = self.run_recipe(run).and_then(|r| r.budget.sessions)
            && self.run_state(run).is_some_and(|s| s.sessions.len() as u32 >= max)
        {
            return Err(Error::conflict(format!("budget: this run may start {max} sessions")));
        }
        let name = sp
            .name
            .as_deref()
            .map(|n| ptyro_run::template::render(n, ctx))
            .unwrap_or_else(|| def.id.clone());
        let name = self.free_name(&ptyro_core::model::slugify(&name));
        let mut env = self.step_env(run, &def.id);
        let out = PathBuf::from(&env["PTYRO_STEP_OUT"]);
        let _ = std::fs::remove_file(&out);
        let mut prompt = prompt;
        if let Some(o) = &def.output {
            prompt.push_str(&ptyro_run::output::instructions(&o.schema, &out.to_string_lossy()));
        }
        env.insert("PTYRO_STEP_PROMPT_CHARS".into(), prompt.chars().count().to_string());
        let spec = SpawnSpec {
            agent: Some(agent),
            cwd: wt.path.clone(),
            worktree: Some(wt.id.clone()),
            profile: sp.profile.as_deref().map(|x| ptyro_run::template::render(x, ctx)),
            prompt: Some(prompt),
            name: Some(name),
            env,
            confirmed: true,
            unqueued: true,
            structured: sp.structured,
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        self.enlist(run, &def.id, &s, def, ctx).await;
        Ok(s)
    }

    /// Tie a session to its step: run membership, verified-done, guard.
    async fn enlist(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        s: &Arc<Session>,
        def: &Step,
        ctx: &Value,
    ) {
        {
            let mut m = s.meta.lock().expect("meta");
            m.run = Some((run.to_owned(), step.to_owned()));
            m.guard = def.guard.as_ref().map(|g| Guard {
                deny_paths: g.deny_paths.clone(),
                deny_cmds: g.deny_cmds.clone(),
            });
        }
        if let Some(d) = &def.done_when {
            // Commands: every substituted value is one quoted shell word (agents set outputs).
            let check = ptyro_run::template::render_shell(&d.run, ctx);
            if let Err(e) = self.set_verify(s, Some(check), Some(d.max_bounces)) {
                tracing::warn!("done_when: {}", e.message);
            }
        }
        let (conversation, worktree) = {
            let m = s.meta.lock().expect("meta");
            (m.conversation.clone(), m.place.worktree.clone())
        };
        self.emit(
            run,
            vec![
                RunEvent::SessionAdded { session: s.id.full() },
                RunEvent::AttemptInfo {
                    step: step.into(),
                    session: Some(s.id.full()),
                    conversation,
                    worktree,
                    start_ref: None,
                },
            ],
        );
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
    }

    async fn exec_spawn(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        resume: bool,
        t: &TimeoutFull,
    ) -> Result<StepResult, Error> {
        let ctx = self.ctx(run, &def.id);
        let st = self.run_state(run).ok_or_else(|| Error::missing("run", run))?;
        let ss = st.steps.get(&def.id).cloned().unwrap_or_default();
        let n = ss.attempts.len().max(1);
        let started = now_ms();
        // After a live upgrade the agent never stopped: just keep waiting for its turn.
        if resume
            && self.handed_off.load(std::sync::atomic::Ordering::SeqCst)
            && let Some(s) = self.attempt_session(run, &def.id).filter(|s| s.is_live())
        {
            return self.finish_agent_step(run, def, &s, t, started, n).await;
        }
        // Resume in place after a restart: the same session, nudged to carry on.
        if resume && let Some(s) = self.attempt_session(run, &def.id) {
            if !s.is_live() {
                self.resume(&s.id.full()).await?;
            }
            let s = self.resolve(&s.id.full())?;
            self.enlist(run, &def.id, &s, def, &ctx).await;
            let prompt = def
                .prompt
                .as_deref()
                .map(|p| ptyro_run::template::render(p, &ctx))
                .unwrap_or_default();
            self.deliver_when_idle(&s, format!("[via ptyro: the daemon restarted while you worked on this; carry on where you left off]\n{prompt}"));
            return self.finish_agent_step(run, def, &s, t, started, n).await;
        }
        let mut prompt =
            def.prompt.as_deref().map(|p| ptyro_run::template::render(p, &ctx)).unwrap_or_default();
        let retry = def.retry.as_ref().filter(|_| n > 1);
        if let Some(r) = retry {
            let fb =
                r.feedback.as_deref().map(|f| ptyro_run::template::render(f, &ctx)).unwrap_or_else(
                    || {
                        format!(
                            "An earlier attempt at this failed: {}",
                            ctx["attempt"]["error"].as_str().unwrap_or("unknown")
                        )
                    },
                );
            // Same session: just the feedback, typed into the agent that already has the context.
            if r.mode == RetryMode::SameSession
                && let Some(s) = ss
                    .attempts
                    .iter()
                    .rev()
                    .nth(1)
                    .and_then(|a| a.session.clone())
                    .and_then(|x| self.resolve(&x).ok())
                    .filter(|s| s.is_live())
            {
                self.enlist(run, &def.id, &s, def, &ctx).await;
                self.reset_verify(&s);
                self.deliver_when_idle(&s, fb);
                return self.finish_agent_step(run, def, &s, t, started, n).await;
            }
            prompt.push_str(&format!("\n\n{fb}"));
        }
        let wt = self.step_worktree(run, def, &ctx).await?;
        let start_ref = self.snapshot_ref(&wt, run, &def.id, n, "start").await;
        self.emit(
            run,
            vec![RunEvent::AttemptInfo {
                step: def.id.clone(),
                session: None,
                conversation: None,
                worktree: Some(wt.id.clone()),
                start_ref,
            }],
        );
        let s = self.start_agent(run, def, &wt, prompt, &ctx).await?;
        self.finish_agent_step(run, def, &s, t, started, n).await
    }

    /// Wait for the turn, then gather outputs (and a typed answer, when the step wants one).
    async fn finish_agent_step(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        s: &Arc<Session>,
        t: &TimeoutFull,
        started: u64,
        n: usize,
    ) -> Result<StepResult, Error> {
        let wait = def.wait.unwrap_or(WaitFor::Idle);
        if let Err((outcome, why)) = self.wait_turn(s, wait, t).await {
            let hold = outcome == "guard_tripped";
            return Ok(StepResult { hold, ..StepResult::fail(&outcome, why) });
        }
        let mut res = StepResult::ok(if def.done_when.is_some() { "done_when" } else { "status" });
        res.outputs = self.agent_outputs(s, started).await;
        if let Some(o) = &def.output {
            let out = PathBuf::from(&self.step_env(run, &def.id)["PTYRO_STEP_OUT"]);
            let mut repaired = false;
            loop {
                wait_for_file(&out, Duration::from_secs(15)).await;
                let parsed = std::fs::read_to_string(&out)
                    .map_err(|e| format!("{} wasn't written ({e})", out.display()))
                    .and_then(|t| {
                        serde_json::from_str::<Value>(t.trim())
                            .map_err(|e| format!("it isn't JSON: {e}"))
                    });
                match parsed.and_then(|v| ptyro_run::output::check(&o.schema, &v).map(|()| v)) {
                    Ok(v) => {
                        res.outputs.insert("output".into(), v);
                        res.decided_by = Some("output".into());
                        break;
                    }
                    Err(e) if !repaired => {
                        repaired = true;
                        self.deliver_when_idle(
                            s,
                            format!(
                                "[via ptyro] Your answer in {} isn't valid: {e}. Write it again.{}",
                                out.display(),
                                ptyro_run::output::instructions(&o.schema, &out.to_string_lossy())
                            ),
                        );
                        if let Err((outcome, why)) = self.wait_turn(s, WaitFor::Idle, t).await {
                            return Ok(StepResult::fail(&outcome, why));
                        }
                    }
                    Err(e) => return Ok(StepResult::fail("failed", format!("output: {e}"))),
                }
            }
        }
        let wt = s.meta.lock().expect("meta").place.worktree.clone();
        if let Some(wt) = wt.and_then(|w| self.worktree_by_id(&w)) {
            res.end_ref = self.snapshot_ref(&wt, run, &def.id, n, "end").await;
        }
        Ok(res)
    }

    /// What every agent step reports: where, how much changed, how long, what it said.
    pub(crate) async fn agent_outputs(
        self: &Arc<Self>,
        s: &Arc<Session>,
        started: u64,
    ) -> BTreeMap<String, Value> {
        let info = self.info(s);
        let mut o = BTreeMap::new();
        o.insert("session".into(), json!(info.id.full()));
        o.insert("session_name".into(), json!(info.name));
        o.insert("duration_ms".into(), json!(now_ms().saturating_sub(started)));
        o.insert("turns".into(), json!(info.turns));
        if let Some(v) = &info.verify {
            o.insert("verified".into(), json!(v.state == ptyro_core::session::VerifyState::Passed));
        }
        o.insert("last_message".into(), json!(crate::review::last_message(s)));
        // A structured session knows exactly what it said and did (M10).
        if let Some(st) = &s.structured {
            let chat = st.chat.lock().expect("chat");
            if let Some(t) = chat.items.iter().rev().find_map(|i| match &i.body {
                ptyro_core::chat::ChatBody::Assistant { text, .. } if !text.trim().is_empty() => {
                    Some(text.clone())
                }
                _ => None,
            }) {
                o.insert(
                    "last_message".into(),
                    json!(t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()),
                );
            }
            o.insert("tools".into(), json!(crate::structured::tool_use(chat.tool_summary())));
        }
        if let Some(wt) = info.worktree.as_deref().and_then(|w| self.worktree_by_id(w)) {
            o.insert("worktree".into(), json!(wt.id));
            o.insert("worktree_name".into(), json!(wt.qualified()));
            o.insert("branch".into(), json!(wt.branch));
            let base = self.diff_base(&wt).await.unwrap_or_else(|_| "HEAD".into());
            o.insert("base".into(), json!(base));
            let path = wt.path.clone();
            if let Ok(Some(head)) = blocking(move || ptyro_git::Git::new().rev(&path, "HEAD")).await
            {
                o.insert("head".into(), json!(head));
            }
            if let Ok(doc) = self.worktree_diff(&wt.id, ptyro_core::model::DiffAgainst::Base).await
            {
                let (a, r): (u32, u32) =
                    doc.files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
                o.insert(
                    "diffstat".into(),
                    json!(format!("{} file(s) +{a} −{r}", doc.files.len())),
                );
                o.insert(
                    "files".into(),
                    json!(doc.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()),
                );
            }
        }
        o
    }

    /// Wait for the turn ptyro last typed to end (SPEC §15.3 `wait`), honouring verified-done
    /// and the start/stall timeouts. `Err((outcome, why))` when it can't end well.
    pub(crate) async fn wait_turn(
        self: &Arc<Self>,
        s: &Arc<Session>,
        wait: WaitFor,
        t: &TimeoutFull,
    ) -> Result<(), (String, String)> {
        let start = dur(t.start.as_deref());
        let stall = dur(t.stall.as_deref());
        let began = now_ms();
        let mut saw_work = false;
        loop {
            let info = self.info(s);
            let live = info.state.is_live();
            if wait == WaitFor::Exited {
                if !live && info.state != ProcessState::Starting {
                    return Ok(());
                }
            } else {
                if !live
                    && info.state != ProcessState::Starting
                    && info.state != ProcessState::Dormant
                {
                    return Err((
                        "failed".into(),
                        format!("{} exited ({}) before finishing", info.name, info.state.label()),
                    ));
                }
                let cond = if wait == WaitFor::Done { "done" } else { "idle" };
                if ptyro_core::session::reached(&info, cond) {
                    match &info.verify {
                        None => return Ok(()),
                        Some(v) => match v.state {
                            ptyro_core::session::VerifyState::Passed => return Ok(()),
                            ptyro_core::session::VerifyState::Failed => {
                                let tail =
                                    v.last.as_ref().map(|e| tail(&e.tail, 15)).unwrap_or_default();
                                return Err((
                                    "failed".into(),
                                    format!(
                                        "done_when: `{}` still fails after {} bounce(s):\n{tail}",
                                        v.check, v.bounces
                                    ),
                                ));
                            }
                            _ => {}
                        },
                    }
                }
                if ptyro_core::session::reached(&info, "errored") {
                    let why = info
                        .status
                        .as_ref()
                        .and_then(|r| r.detail.clone().or(r.kind.clone()))
                        .unwrap_or_else(|| "error".into());
                    return Err((
                        "failed".into(),
                        format!("the agent stopped with an error: {why}"),
                    ));
                }
            }
            let since_prompt = info.last_prompt_ms.unwrap_or(began);
            if info
                .status
                .as_ref()
                .is_some_and(|r| r.status == AgentStatus::Working && r.since_ms >= since_prompt)
            {
                saw_work = true;
            }
            if let Some(start) = start
                && !saw_work
                && !info.prompt_pending
                && now_ms().saturating_sub(since_prompt) > start.as_millis() as u64
            {
                return Err((
                    "timeout".into(),
                    format!("never started working within {}", t.start.clone().unwrap_or_default()),
                ));
            }
            if let Some(stall) = stall
                && info.status.as_ref().is_some_and(|r| r.status == AgentStatus::Working)
            {
                let last = info
                    .last_output_ms
                    .unwrap_or(0)
                    .max(info.status.as_ref().map_or(0, |r| r.since_ms));
                if now_ms().saturating_sub(last) > stall.as_millis() as u64 {
                    return Err((
                        "stall".into(),
                        format!(
                            "working with no output for {}",
                            t.stall.clone().unwrap_or_default()
                        ),
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn exec_send(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        resume: bool,
        t: &TimeoutFull,
    ) -> Result<StepResult, Error> {
        if resume
            && self.handed_off.load(std::sync::atomic::Ordering::SeqCst)
            && let Some(s) = self.attempt_session(run, &def.id).filter(|s| s.is_live())
        {
            // A live upgrade: the follow-up was already typed.
            let n = self
                .run_state(run)
                .and_then(|st| st.steps.get(&def.id).map(|x| x.attempts.len()))
                .unwrap_or(1);
            return self.finish_agent_step(run, def, &s, t, now_ms(), n).await;
        }
        let ctx = self.ctx(run, &def.id);
        let sd = def.send.clone().ok_or_else(|| Error::invalid("send step without send"))?;
        let st = self.run_state(run).ok_or_else(|| Error::missing("run", run))?;
        let n = st.steps.get(&def.id).map_or(1, |s| s.attempts.len().max(1));
        let target = st
            .steps
            .get(&sd.to)
            .and_then(|s| s.attempt())
            .and_then(|a| a.session.clone())
            .ok_or_else(|| Error::missing("session", format!("of step {}", sd.to)))?;
        let mut s = self.resolve(&target)?;
        if !s.is_live() {
            self.resume(&target).await?;
            s = self.resolve(&target)?;
        }
        let started = now_ms();
        self.enlist(run, &def.id, &s, def, &ctx).await;
        self.reset_verify(&s);
        // The follow-up keeps the original step's tie: its session still belongs to it.
        s.meta.lock().expect("meta").run = Some((run.to_owned(), sd.to.clone()));
        self.deliver_when_idle(&s, ptyro_run::template::render(&sd.text, &ctx));
        // Wait for delivery, so the idle before it doesn't count.
        let typed_by = now_ms() + 120_000;
        while !s.meta.lock().expect("meta").last_prompt_ms.is_some_and(|p| p >= started) {
            if now_ms() > typed_by || !s.is_live() {
                return Ok(StepResult::fail(
                    "failed",
                    "the follow-up was never typed (the agent didn't get ready)",
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.finish_agent_step(run, def, &s, t, started, n).await
    }

    /// `loop`: a fresh session per iteration in one worktree, a checkpoint each time, until the
    /// check passes, progress stops, or `max` runs out (R-ORC-25).
    async fn exec_loop(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
        resume: bool,
        t: &TimeoutFull,
    ) -> Result<StepResult, Error> {
        let l = def.loop_.clone().ok_or_else(|| Error::invalid("loop step without loop"))?;
        // Resuming (a restart or a live upgrade): carry on after the iterations already done,
        // and after an upgrade, with the iteration still running.
        let done: BTreeMap<String, Value> = if resume {
            self.run_state(run)
                .and_then(|st| st.steps.get(&def.id).map(|x| x.outputs.clone()))
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        let first = done.get("iterations").and_then(Value::as_u64).map_or(1, |n| n as u32 + 1);
        let mut carry = (resume && self.handed_off.load(std::sync::atomic::Ordering::SeqCst))
            .then(|| self.attempt_session(run, &def.id))
            .flatten()
            .filter(|s| s.is_live());
        let mut ctx = self.ctx(run, &def.id);
        let wt = self.step_worktree(run, def, &ctx).await?;
        self.emit(
            run,
            vec![RunEvent::AttemptInfo {
                step: def.id.clone(),
                session: None,
                conversation: None,
                worktree: Some(wt.id.clone()),
                start_ref: None,
            }],
        );
        let check = ptyro_run::template::render_shell(&l.until, &ctx);
        let started = now_ms();
        let mut last_check =
            done.get("last_check").and_then(Value::as_str).unwrap_or("(not run yet)").to_owned();
        let mut last_tree: Option<String> = None;
        let mut idle_rounds = 0;
        for i in first..=l.max.max(1) {
            ctx["loop"] = json!({ "iteration": i, "last_check": last_check });
            let s = match carry.take() {
                Some(s) => s,
                None => {
                    let prompt = def
                        .prompt
                        .as_deref()
                        .map(|p| ptyro_run::template::render(p, &ctx))
                        .unwrap_or_default();
                    self.start_agent(run, def, &wt, prompt, &ctx).await?
                }
            };
            let turn = self.wait_turn(&s, WaitFor::Idle, t).await;
            if s.is_live() {
                self.stop_session(&s).await;
            }
            if let Err((outcome, why)) = turn {
                return Ok(StepResult::fail(&outcome, format!("iteration {i}: {why}"))
                    .with("iterations", json!(i)));
            }
            let path = wt.path.clone();
            let msg = format!("ptyro loop {run}/{} iteration {i}", def.id);
            let snap = blocking(move || ptyro_git::Git::new().checkpoint(&path, &msg).ok()).await?;
            if let Some(sn) = &snap {
                let (p, name, c) = (
                    wt.path.clone(),
                    format!("refs/ptyro/runs/{run}/{}/loop/{i}", def.id),
                    sn.commit.clone(),
                );
                let _ = blocking(move || ptyro_git::Git::new().update_ref(&p, &name, &c)).await;
            }
            let ev = self
                .run_check(
                    &wt,
                    &check,
                    &self.run_dir(run).join(&def.id).join(format!("check-{i}.log")),
                    None,
                )
                .await?;
            last_check = format!("exit {}:\n{}", ev.exit_code, tail(&ev.tail, 30));
            self.emit(
                run,
                vec![
                    RunEvent::Note {
                        text: format!("{}: iteration {i}, check exit {}", def.id, ev.exit_code),
                    },
                    RunEvent::Outputs {
                        step: def.id.clone(),
                        outputs: BTreeMap::from([
                            ("iterations".into(), json!(i)),
                            ("last_check".into(), json!(last_check)),
                        ]),
                    },
                ],
            );
            if ev.exit_code == 0 {
                let mut res = StepResult::ok("loop.until");
                res.outputs = self.agent_outputs(&s, started).await;
                res.outputs.insert("iterations".into(), json!(i));
                res.outputs.insert("last_check".into(), json!(last_check));
                return Ok(res);
            }
            let tree = snap.map(|s| s.tree);
            if tree.is_some() && tree == last_tree {
                idle_rounds += 1;
                if idle_rounds >= l.no_progress.max(1) {
                    return Ok(StepResult::fail("failed", format!("no progress in {idle_rounds} iteration(s) in a row; the check still says:\n{last_check}")).with("iterations", json!(i)));
                }
            } else {
                idle_rounds = 0;
            }
            last_tree = tree;
        }
        Ok(StepResult::fail(
            "failed",
            format!("{} iterations and the check still fails:\n{last_check}", l.max),
        )
        .with("iterations", json!(l.max)))
    }

    // ── run, gate, apply, sleep ────────────────────────────────────────────

    async fn exec_run(self: &Arc<Self>, run: &str, def: &Step) -> Result<StepResult, Error> {
        let ctx = self.ctx(run, &def.id);
        let st = self.run_state(run).ok_or_else(|| Error::missing("run", run))?;
        let wt = match &def.in_step {
            Some(sid) => st
                .steps
                .get(sid)
                .and_then(|s| s.attempt())
                .and_then(|a| a.worktree.clone())
                .and_then(|w| self.worktree_by_id(&w))
                .ok_or_else(|| Error::missing("worktree", format!("of step {sid}")))?,
            None => {
                let p = st
                    .project
                    .clone()
                    .ok_or_else(|| Error::invalid("a run step needs a project (or `in`)"))?;
                self.root_worktree(&p).ok_or_else(|| Error::missing("root worktree", p))?
            }
        };
        let cmd = ptyro_run::template::render_shell(def.run.as_deref().unwrap_or_default(), &ctx);
        let n = st.steps.get(&def.id).map_or(1, |s| s.attempts.len());
        let log = self.run_dir(run).join(&def.id).join(format!("output-{n}.log"));
        let mut env = self.step_env(run, &def.id);
        env.insert("PTYRO_ATTEMPT".into(), n.to_string());
        let ev = self.run_check(&wt, &cmd, &log, Some(env)).await?;
        let mut res = if ev.exit_code == 0 {
            StepResult::ok("exit_code")
        } else {
            StepResult::fail("failed", format!("`{cmd}` exited {}", ev.exit_code))
        };
        res.outputs.insert("exit_code".into(), json!(ev.exit_code));
        res.outputs.insert("tail".into(), json!(tail(&ev.tail, 40)));
        res.outputs.insert("log".into(), json!(log));
        res.outputs.insert("duration_ms".into(), json!(ev.duration_ms));
        res.outputs.insert("worktree".into(), json!(wt.id));
        Ok(res)
    }

    async fn exec_gate(self: &Arc<Self>, run: &str, def: &Step) -> Result<StepResult, Error> {
        let ctx = self.ctx(run, &def.id);
        let g = def.gate.clone().ok_or_else(|| Error::invalid("gate step without gate"))?;
        // The gate this step opened before (a restart): reuse it, answered or not, so an answer
        // given just before the restart isn't asked for again.
        let open =
            self.run_state(run).and_then(|st| st.steps.get(&def.id).and_then(|s| s.gate.clone()));
        let gate = match open {
            Some(x) => x,
            None => {
                let id = loop {
                    let id = ptyro_run::short_id("g", 4);
                    if self.find_gate(&id).is_none() {
                        break id;
                    }
                };
                let gate = Gate {
                    id,
                    ask: ptyro_run::template::render(g.ask(), &ctx),
                    options: g.options(),
                    opened_ms: now_ms(),
                    deadline_ms: dur(g.timeout()).map(|d| now_ms() + d.as_millis() as u64),
                    ..Gate::default()
                };
                self.emit(
                    run,
                    vec![RunEvent::GateOpened { step: def.id.clone(), gate: gate.clone() }],
                );
                self.announce_gate(run, &gate).await;
                gate
            }
        };
        let wake = self.runs.lock().expect("runs").get(run).map(|e| Arc::clone(&e.wake));
        let mut deadline = gate.deadline_ms;
        loop {
            let now = self
                .run_state(run)
                .and_then(|st| st.steps.get(&def.id).and_then(|s| s.gate.clone()));
            if let Some(x) = now.as_ref().filter(|x| x.choice.is_some()) {
                let choice = x.choice.clone().unwrap_or_default();
                if g.fails(&choice) {
                    return Ok(StepResult {
                        decided_by: Some("gate".into()),
                        ..StepResult::fail(
                            "rejected",
                            format!("answered {choice:?} by {}", x.by.clone().unwrap_or_default()),
                        )
                    }
                    .with("choice", json!(choice)));
                }
                return Ok(StepResult::ok("gate")
                    .with("choice", json!(choice))
                    .with("note", json!(x.note)));
            }
            if let Some(d) = deadline
                && now_ms() >= d
            {
                match g.on_timeout() {
                    "escalate" => {
                        deadline = None;
                        self.notify_run(
                            run,
                            AttentionKind::Gate,
                            format!("still waiting: {}", gate.ask),
                            format!(
                                "gate {} has waited past its timeout\nptyro gate resolve {} {}",
                                gate.id,
                                gate.id,
                                gate.options.join("|")
                            ),
                        )
                        .await;
                    }
                    other => {
                        // The timeout's answer must be one of the options: `approve` means the
                        // first, anything else that isn't an option the last (the "no" side).
                        let choice = if gate.options.iter().any(|o| o.eq_ignore_ascii_case(other)) {
                            other.to_owned()
                        } else if other == "approve" {
                            gate.options.first().cloned().unwrap_or_default()
                        } else {
                            gate.options.last().cloned().unwrap_or_default()
                        };
                        let _ = self.resolve_gate(
                            &gate.id,
                            &choice,
                            Some("timed out".into()),
                            None,
                            "timeout",
                        );
                    }
                }
            }
            match &wake {
                Some(w) => {
                    let _ = tokio::time::timeout(Duration::from_millis(500), w.notified()).await;
                }
                None => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    }

    pub(crate) async fn announce_gate(self: &Arc<Self>, run: &str, gate: &Gate) {
        let body =
            format!("{}\nptyro gate resolve {} {}", gate.ask, gate.id, gate.options.join("|"));
        self.notify_run(run, AttentionKind::Gate, format!("gate {} · run {run}", gate.id), body)
            .await;
    }

    pub(crate) fn find_gate(&self, id: &str) -> Option<(String, String, Gate)> {
        let runs = self.runs.lock().expect("runs");
        runs.values().find_map(|e| {
            e.state.steps.iter().find_map(|(sid, s)| {
                s.gate
                    .as_ref()
                    .filter(|g| g.id == id)
                    .map(|g| (e.state.id.clone(), sid.clone(), g.clone()))
            })
        })
    }

    /// Answer a gate. A repeat with the same key or the same choice is a no-op; a different
    /// answer to a closed gate is a conflict.
    pub(crate) fn resolve_gate(
        &self,
        id: &str,
        choice: &str,
        note: Option<String>,
        key: Option<String>,
        by: &str,
    ) -> Result<GateInfo, Error> {
        let id = id.trim();
        let (run, step, g) = self.find_gate(id).ok_or_else(|| Error::missing("gate", id))?;
        if let Some(prev) = &g.choice {
            if key.is_some() && g.key == key || prev == choice {
                return Ok(GateInfo { run, step, gate: g });
            }
            return Err(Error::conflict(format!(
                "gate {id} was already answered {prev:?} by {}",
                g.by.unwrap_or_default()
            )));
        }
        let choice =
            g.options.iter().find(|o| o.eq_ignore_ascii_case(choice)).cloned().ok_or_else(
                || {
                    Error::invalid(format!(
                        "{choice:?} isn't an answer to gate {id} (one of: {})",
                        g.options.join(", ")
                    ))
                },
            )?;
        self.emit(
            &run,
            vec![RunEvent::GateResolved {
                step: step.clone(),
                choice,
                note,
                by: by.into(),
                key,
                at: now_ms(),
            }],
        );
        let (_, _, g) = self.find_gate(id).ok_or_else(|| Error::missing("gate", id))?;
        Ok(GateInfo { run, step, gate: g })
    }

    async fn exec_apply(self: &Arc<Self>, run: &str, def: &Step) -> Result<StepResult, Error> {
        let ctx = self.ctx(run, &def.id);
        let a = def.apply.clone().ok_or_else(|| Error::invalid("apply step without apply"))?;
        let st = self.run_state(run).ok_or_else(|| Error::missing("run", run))?;
        let wt = st
            .steps
            .get(&a.from)
            .and_then(|s| s.attempt())
            .and_then(|x| x.worktree.clone())
            .and_then(|w| self.worktree_by_id(&w))
            .ok_or_else(|| Error::missing("worktree", format!("of step {}", a.from)))?;
        let done: Vec<String> = st
            .steps
            .get(&def.id)
            .and_then(|s| s.outputs.get("applied"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let mut applied = done.clone();
        let mut notes = Vec::new();
        for action in &a.actions {
            if done.contains(action) {
                continue; // after a restart: already done (the outcome event says so)
            }
            self.emit(run, vec![RunEvent::Intent { step: def.id.clone(), action: action.clone() }]);
            let r: Result<String, Error> = match action.as_str() {
                "commit" => {
                    let path = wt.path.clone();
                    if blocking(move || ptyro_git::Git::new().dirty(&path)).await? {
                        let msg = a
                            .message
                            .as_deref()
                            .map(|m| ptyro_run::template::render(m, &ctx))
                            .unwrap_or_else(|| {
                                let p = self
                                    .run_recipe(run)
                                    .and_then(|r| r.step(&a.from).and_then(|s| s.prompt.clone()))
                                    .unwrap_or_default();
                                let first = ptyro_run::template::render(&p, &ctx)
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(70)
                                    .collect::<String>();
                                if first.is_empty() { format!("ptyro run {run}") } else { first }
                            });
                        match self
                            .handle_review(Request::WorktreeCommit {
                                addr: wt.id.clone(),
                                message: msg,
                                paths: vec![],
                                all: true,
                            })
                            .await?
                        {
                            Reply::Committed { sha } => {
                                Ok(format!("committed {}", &sha[..sha.len().min(10)]))
                            }
                            _ => Ok("committed".into()),
                        }
                    } else {
                        Ok("nothing to commit".into())
                    }
                }
                "push" => {
                    let env = self.tool_env(&wt).await?;
                    let (path, branch) = (wt.path.clone(), wt.branch.clone().unwrap_or_default());
                    blocking(move || {
                        let g = ptyro_git::Git::with_env(env);
                        let remote =
                            g.main_remote(&path).map(|r| r.name).unwrap_or_else(|| "origin".into());
                        g.push(&path, &remote, &branch)
                            .map(|()| format!("pushed {branch}"))
                            .map_err(crate::registry::git_err)
                    })
                    .await?
                }
                "pr" => self
                    .pr_create(
                        &wt.id,
                        a.draft.unwrap_or(self.config().github.draft),
                        None,
                        None,
                        None,
                    )
                    .await
                    .map(|r| match r {
                        Reply::Pr { info: Some(pr) } => format!("PR #{} {}", pr.number, pr.url),
                        _ => "PR opened".into(),
                    }),
                "integrate" => self
                    .handle_review(Request::WorktreeIntegrate {
                        addr: wt.id.clone(),
                        into: a.into.clone(),
                        cherry_pick: false,
                        push: false,
                    })
                    .await
                    .map(|r| match r {
                        Reply::Notes { notes } => notes.join("; "),
                        _ => "integrated".into(),
                    }),
                "automerge" => {
                    self.pr_automerge(&wt.id, None, false).await.map(|_| "auto-merge on".into())
                }
                other => Err(Error::invalid(format!("unknown apply action {other}"))),
            };
            match r {
                Ok(detail) => {
                    applied.push(action.clone());
                    notes.push(detail.clone());
                    self.emit(
                        run,
                        vec![
                            RunEvent::Outcome {
                                step: def.id.clone(),
                                action: action.clone(),
                                ok: true,
                                detail: Some(detail),
                            },
                            RunEvent::Outputs {
                                step: def.id.clone(),
                                outputs: BTreeMap::from([("applied".into(), json!(applied))]),
                            },
                        ],
                    );
                }
                Err(e) => {
                    self.emit(
                        run,
                        vec![RunEvent::Outcome {
                            step: def.id.clone(),
                            action: action.clone(),
                            ok: false,
                            detail: Some(e.message.clone()),
                        }],
                    );
                    return Ok(StepResult::fail("failed", format!("{action}: {}", e.message))
                        .with("applied", json!(applied)));
                }
            }
        }
        Ok(StepResult::ok("apply").with("applied", json!(applied)).with("notes", json!(notes)))
    }

    async fn exec_sleep(self: &Arc<Self>, run: &str, def: &Step) -> Result<StepResult, Error> {
        let d = dur(def.sleep.as_deref()).unwrap_or_default();
        let deadline = self
            .run_state(run)
            .and_then(|st| {
                st.steps
                    .get(&def.id)
                    .and_then(|s| s.outputs.get("deadline_ms"))
                    .and_then(Value::as_u64)
            })
            .unwrap_or_else(|| {
                let at = now_ms() + d.as_millis() as u64;
                self.emit(
                    run,
                    vec![RunEvent::Outputs {
                        step: def.id.clone(),
                        outputs: BTreeMap::from([("deadline_ms".into(), json!(at))]),
                    }],
                );
                at
            });
        let left = deadline.saturating_sub(now_ms());
        tokio::time::sleep(Duration::from_millis(left)).await;
        Ok(StepResult::ok("sleep"))
    }

    // ── requests ───────────────────────────────────────────────────────────

    /// Recipes by source, later ones replacing earlier ones of the same name: built-in, user,
    /// then the project's (trusted or not).
    pub(crate) fn recipes(&self, project: Option<&str>) -> Vec<(RecipeInfo, String)> {
        let mut out: Vec<(RecipeInfo, String)> = Vec::new();
        let mut add =
            |src: String, source: String, path: Option<String>, untrusted: Option<String>| {
                let (info_name, info) = match Recipe::parse(&src) {
                    Ok(r) => (
                        r.name.clone(),
                        RecipeInfo {
                            name: r.name.clone(),
                            description: r.description.clone(),
                            source,
                            path,
                            vars: r
                                .vars
                                .iter()
                                .map(|(k, v)| match (&v.default, v.required) {
                                    (_, true) => format!("{k} (required)"),
                                    (Some(d), _) => format!("{k} = {d}"),
                                    _ => k.clone(),
                                })
                                .collect(),
                            steps: r.steps.len(),
                            untrusted,
                            error: None,
                        },
                    ),
                    Err(e) => {
                        let name = path
                            .as_deref()
                            .and_then(|p| std::path::Path::new(p).file_stem())
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        (
                            name.clone(),
                            RecipeInfo {
                                name,
                                description: None,
                                source,
                                path,
                                vars: vec![],
                                steps: 0,
                                untrusted,
                                error: Some(e.to_string()),
                            },
                        )
                    }
                };
                out.retain(|(i, _)| i.name != info_name);
                out.push((info, src));
            };
        for (_, src) in ptyro_run::builtin::ALL {
            add(src.to_owned(), "builtin".into(), None, None);
        }
        let mut dirs = vec![(self.paths.config.join("recipes"), "user".to_owned(), None)];
        if let Some(p) = project.and_then(|p| self.find_project(p).ok()) {
            dirs.push((
                p.root.join(".ptyro/recipes"),
                format!("repo:{}", p.slug),
                Some(p.id.clone()),
            ));
        }
        for (dir, source, project) in dirs {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            files.sort();
            for f in files {
                let Ok(src) = std::fs::read_to_string(&f) else { continue };
                let untrusted = project.as_deref().and_then(|pid| {
                    let name = Recipe::parse(&src).map(|r| r.name).unwrap_or_default();
                    let fp = ptyro_run::recipe::sha(&src);
                    (!self.is_trusted_as(pid, &format!("recipe:{name}"), &fp)).then_some(fp)
                });
                add(src, source.clone(), Some(f.to_string_lossy().into_owned()), untrusted);
            }
        }
        out
    }

    fn project_for(
        &self,
        project: Option<&str>,
    ) -> Result<Option<ptyro_core::model::ProjectInfo>, Error> {
        project.map(|p| self.find_project(p)).transpose()
    }

    pub(crate) async fn handle_runs(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::RunList { active, limit } => {
                let runs = self.runs.lock().expect("runs");
                let mut v: Vec<RunInfo> = runs
                    .values()
                    .filter(|e| !active || !e.state.status.is_over())
                    .map(|e| self.info_of(e, &runs))
                    .collect();
                v.sort_by_key(|r| std::cmp::Reverse(r.state.created_ms));
                v.truncate(limit.unwrap_or(50) as usize);
                Ok(Reply::Runs { runs: v })
            }
            Request::RunGet { id } => {
                let id = self.find_run(&id)?;
                Ok(Reply::Run {
                    run: self.run_info(&id).ok_or_else(|| Error::missing("run", &id))?,
                })
            }
            Request::RunEvents { id } => {
                let id = self.find_run(&id)?;
                let store = self.store.as_ref().ok_or_else(|| {
                    Error::new(ErrorCode::Unsupported, "the store is unavailable")
                })?;
                store.flush().await.map_err(|e| Error::internal(e.to_string()))?;
                Ok(Reply::RunEvents {
                    events: store
                        .run_events(id)
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?,
                })
            }
            Request::RunCancel { id } => {
                let id = self.find_run(&id)?;
                if self.run_state(&id).is_some_and(|s| s.status.is_over()) {
                    return Err(Error::conflict(format!("run {id} has already ended")));
                }
                let live: Vec<String> = self
                    .run_state(&id)
                    .map(|st| {
                        st.steps
                            .iter()
                            .filter(|(_, s)| !s.status.is_over() && s.status != StepStatus::Pending)
                            .map(|(k, _)| k.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for step in live {
                    self.cancel_step(&id, &step, "the run was cancelled").await;
                }
                self.finish_run(&id, RunStatus::Cancelled, Some("cancelled".into())).await;
                Ok(Reply::Run {
                    run: self.run_info(&id).ok_or_else(|| Error::missing("run", &id))?,
                })
            }
            Request::RunRetry { id, step } => {
                let id = self.find_run(&id)?;
                let st = self.run_state(&id).ok_or_else(|| Error::missing("run", &id))?;
                let ss = st.steps.get(&step).ok_or_else(|| Error::missing("step", &step))?;
                if ss.status == StepStatus::Running {
                    return Err(Error::conflict(format!("step {step} is running")));
                }
                // A step waiting at a gate still has its task: stop it, or two would answer.
                if let Some(t) =
                    self.runs.lock().expect("runs").get_mut(&id).and_then(|e| e.tasks.remove(&step))
                {
                    t.abort();
                }
                let was_over = st.status.is_over();
                self.emit(&id, vec![RunEvent::StepReset { step: step.clone() }]);
                // Its dependents that were skipped because of it get another chance too.
                if let Some(r) = self.run_recipe(&id) {
                    let mut reset: Vec<String> = Vec::new();
                    for s in &r.steps {
                        if s.needs.contains(&step) || s.needs.iter().any(|n| reset.contains(n)) {
                            let ds = st.steps.get(&s.id).map(|x| x.status);
                            if matches!(
                                ds,
                                Some(
                                    StepStatus::Skipped
                                        | StepStatus::Failed
                                        | StepStatus::Cancelled
                                )
                            ) {
                                reset.push(s.id.clone());
                            }
                        }
                    }
                    self.emit(
                        &id,
                        reset.into_iter().map(|s| RunEvent::StepReset { step: s }).collect(),
                    );
                }
                if was_over {
                    self.start_actor(&id);
                }
                Ok(Reply::Run {
                    run: self.run_info(&id).ok_or_else(|| Error::missing("run", &id))?,
                })
            }
            Request::RunFork { id, from, set } => {
                let id = self.find_run(&id)?;
                let (st, recipe, src, policy) = {
                    let runs = self.runs.lock().expect("runs");
                    let e = runs.get(&id).ok_or_else(|| Error::missing("run", &id))?;
                    (e.state.clone(), e.recipe.clone(), e.src.clone(), e.policy.clone())
                };
                let recipe = recipe
                    .ok_or_else(|| Error::invalid("only recipe and fanout runs can be forked"))?;
                if recipe.step(&from).is_none() {
                    return Err(Error::missing("step", &from));
                }
                // Copy every step that `from` doesn't (transitively) lead to and that is over.
                let mut downstream = vec![from.clone()];
                for s in &recipe.steps {
                    if s.needs.iter().any(|n| downstream.contains(n)) {
                        downstream.push(s.id.clone());
                    }
                }
                let copied: Vec<(String, ptyro_run::StepState)> = st
                    .steps
                    .iter()
                    .filter(|(k, v)| !downstream.contains(k) && v.status.is_over())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let mut vars = st.vars.clone();
                for (k, v) in set {
                    if !recipe.vars.contains_key(&k) {
                        return Err(Error::invalid(format!("unknown variable {k:?}")));
                    }
                    vars.insert(k, v);
                }
                let new = self.create_run(
                    st.kind,
                    Some((*recipe).clone()),
                    src,
                    vars,
                    st.project.clone(),
                    st.base.clone(),
                    None,
                    policy,
                    Some((id.clone(), from.clone(), copied)),
                );
                Ok(Reply::Run {
                    run: self.run_info(&new).ok_or_else(|| Error::missing("run", &new))?,
                })
            }
            Request::RecipeList { project } => {
                let p = self.project_for(project.as_deref())?;
                Ok(Reply::Recipes {
                    recipes: self
                        .recipes(p.as_ref().map(|p| p.id.as_str()))
                        .into_iter()
                        .map(|(i, _)| i)
                        .collect(),
                })
            }
            Request::RecipeShow { name, project } => {
                let p = self.project_for(project.as_deref())?;
                let (info, src) = self
                    .recipes(p.as_ref().map(|p| p.id.as_str()))
                    .into_iter()
                    .find(|(i, _)| i.name == name)
                    .ok_or_else(|| Error::missing("recipe", &name))?;
                Ok(Reply::Recipe { info, src })
            }
            Request::RecipeRun { name, src, vars, project, base, trust } => {
                let p = self.project_for(project.as_deref())?;
                let (src, untrusted, rname) = match (src, name) {
                    (Some(s), _) => (s, None, None),
                    (None, Some(n)) => {
                        let (i, s) = self
                            .recipes(p.as_ref().map(|p| p.id.as_str()))
                            .into_iter()
                            .find(|(i, _)| i.name == n)
                            .ok_or_else(|| Error::missing("recipe", &n))?;
                        (s, i.untrusted.clone(), Some(i.name))
                    }
                    (None, None) => return Err(Error::invalid("name a recipe, or pass its file")),
                };
                if let Some(fp) = untrusted {
                    let pid = p.as_ref().map(|p| p.id.clone()).unwrap_or_default();
                    if trust.as_deref() == Some(fp.as_str()) {
                        self.trust_as(&pid, &format!("recipe:{}", rname.unwrap_or_default()), &fp);
                    } else {
                        return Err(Error::new(
                            ErrorCode::UntrustedProject,
                            format!(
                                "this recipe comes from the repository and isn't trusted; read it (`ptyro recipe show`), then run with --trust {fp}"
                            ),
                        ));
                    }
                }
                let recipe =
                    Recipe::parse(&src).map_err(|e| Error::invalid(format!("recipe: {e}")))?;
                let vars = recipe.bind(&vars).map_err(|e| Error::invalid(e.to_string()))?;
                let needs_project = recipe
                    .steps
                    .iter()
                    .any(|s| matches!(s.kind(), Some(Kind::Spawn | Kind::Run | Kind::Verify)));
                if needs_project && p.is_none() {
                    return Err(Error::invalid(
                        "this recipe starts agents or commands: say where with --project (or run it inside a repository)",
                    ));
                }
                let id = self.create_run(
                    RunKind::Recipe,
                    Some(recipe),
                    Some(src),
                    vars,
                    p.map(|p| p.id),
                    base,
                    None,
                    None,
                    None,
                );
                Ok(Reply::Run {
                    run: self.run_info(&id).ok_or_else(|| Error::missing("run", &id))?,
                })
            }
            Request::Fanout { spec } => self.fanout(spec).await,
            Request::GateList { all } => {
                let runs = self.runs.lock().expect("runs");
                let mut v: Vec<GateInfo> =
                    runs.values()
                        .flat_map(|e| {
                            e.state.steps.iter().filter_map(|(sid, s)| {
                                s.gate.clone().filter(|g| all || g.choice.is_none()).map(|g| {
                                    GateInfo { run: e.state.id.clone(), step: sid.clone(), gate: g }
                                })
                            })
                        })
                        .collect();
                v.sort_by_key(|g| g.gate.opened_ms);
                Ok(Reply::Gates { gates: v })
            }
            Request::GateResolve { id, choice, note, key, by } => {
                let g =
                    self.resolve_gate(&id, &choice, note, key, by.as_deref().unwrap_or("cli"))?;
                Ok(Reply::Gates { gates: vec![g] })
            }
            other => Err(Error::invalid(format!("unsupported request {other:?}"))),
        }
    }

    /// `ptyro fanout` (SPEC §15.2): one candidate step per agent, each in its own worktree from
    /// the same base, then a `rank` step that gathers the evidence for the compare view.
    async fn fanout(self: &Arc<Self>, spec: FanoutSpec) -> Result<Reply, Error> {
        if spec.agents.is_empty() {
            return Err(Error::invalid(
                "say which agents: --agents claude,codex,gemini (or -n 3 --agents claude)",
            ));
        }
        if spec.prompt.trim().is_empty() {
            return Err(Error::invalid("the prompt is empty"));
        }
        let config = self.config();
        for a in &spec.agents {
            let (name, _) = ptyro_agents::split_agent(a);
            if ptyro_agents::command(&config, name).is_none() {
                return Err(Error::missing(
                    "agent",
                    format!("{name} (known: {})", ptyro_agents::names(&config).join(", ")),
                ));
            }
        }
        let p = match spec.project.as_deref() {
            Some(x) => self.find_project(x)?,
            None => {
                return Err(Error::invalid(
                    "fanout needs a project: --project (or run it inside a repository)",
                ));
            }
        };
        let prefix = spec.name.clone().unwrap_or_else(|| {
            ptyro_core::model::slugify(&spec.prompt)
                .split('-')
                .filter(|w| !w.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join("-")
        });
        let prefix = if prefix.is_empty() { "fanout".to_owned() } else { prefix };
        // Candidate ids: the agent's name, numbered on repeats (claude, claude-2).
        let mut ids: Vec<(String, String)> = Vec::new();
        for a in &spec.agents {
            let base = ptyro_core::model::slugify(ptyro_agents::split_agent(a).0);
            let id = if ids.iter().any(|(i, _)| *i == base) {
                (2..)
                    .map(|k| format!("{base}-{k}"))
                    .find(|n| !ids.iter().any(|(i, _)| i == n))
                    .expect("infinite")
            } else {
                base
            };
            ids.push((id, a.clone()));
        }
        let mut src = format!(
            "name = \"fanout\"\ndescription = \"{} candidates\"\nvars = {{ prompt = {{ required = true }} }}\n",
            ids.len()
        );
        // JSON string escapes are valid TOML basic strings.
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
        for (id, agent) in &ids {
            src.push_str(&format!(
                "\n[[step]]\nid = {}\nspawn = {{ agent = {}, worktree = \"new\", name = {}{} }}\nprompt = \"{{{{vars.prompt}}}}\"\n",
                q(id), q(agent), q(&format!("{prefix}-{id}")), if spec.structured { ", structured = true" } else { "" }
            ));
            if let Some(v) = &spec.verify {
                src.push_str(&format!("done_when = {{ run = {}, max_bounces = 3 }}\n", q(v)));
            }
            if let Some(t) = &spec.timeout {
                src.push_str(&format!("timeout = {}\n", q(t)));
            }
        }
        src.push_str(&format!(
            "\n[[step]]\nid = \"rank\"\nneeds = [{}]\njoin = \"all_settled\"\nverify = {{ candidates = [{}]{}{} }}\n",
            ids.iter().map(|(i, _)| q(i)).collect::<Vec<_>>().join(", "),
            ids.iter().map(|(i, _)| q(i)).collect::<Vec<_>>().join(", "),
            spec.test.as_ref().map(|t| format!(", tests = {}", q(t))).unwrap_or_default(),
            spec.judge.as_ref().map(|j| format!(", judge = {}", q(j))).unwrap_or_default()
        ));
        let recipe =
            Recipe::parse(&src).map_err(|e| Error::internal(format!("fanout recipe: {e}")))?;
        // Fanout worktree names are the prefix and the candidate, not the run id.
        let vars = BTreeMap::from([("prompt".to_owned(), spec.prompt.clone())]);
        let id = self.create_run(
            RunKind::Fanout,
            Some(recipe),
            Some(src),
            vars,
            Some(p.id),
            spec.base.clone(),
            None,
            None,
            None,
        );
        if ids.len() > 6 {
            self.emit(
                &id,
                vec![RunEvent::Note {
                    text: "more than 6 candidates rarely finds a better answer; it mostly costs"
                        .into(),
                }],
            );
        }
        Ok(Reply::Run { run: self.run_info(&id).ok_or_else(|| Error::missing("run", &id))? })
    }

    /// A session ended a turn, exited or got blocked: a lead hears about its workers (M7.7).
    pub(crate) async fn run_member_changed(self: &Arc<Self>, s: &Arc<Session>) {
        let Some((run, step)) = s.meta.lock().expect("meta").run.clone() else { return };
        let (kind, ended) = {
            let runs = self.runs.lock().expect("runs");
            let Some(e) = runs.get(&run) else { return };
            e.wake.notify_one();
            (e.state.kind, e.state.steps.get(&step).is_none_or(|x| x.status.is_over()))
        };
        // A lead's worker step lasts as long as its session.
        if kind == RunKind::Lead && step != "lead" && !ended && !s.is_live() {
            let failed = matches!(s.meta.lock().expect("meta").state, ProcessState::Exited { code: Some(c), .. } if c != 0);
            self.emit(
                &run,
                vec![RunEvent::StepEnded {
                    step,
                    status: if failed { StepStatus::Failed } else { StepStatus::Succeeded },
                    reason: Some(s.meta.lock().expect("meta").state.label()),
                    decided_by: Some("status".into()),
                    at: now_ms(),
                }],
            );
        }
    }

    /// The lead is gone: its run is over (workers keep running; they're yours now).
    pub(crate) async fn lead_exited(self: &Arc<Self>, s: &Arc<Session>) {
        let Some((run, _, true)) = self.run_of_session(&s.id.full()) else { return };
        self.finish_run(&run, RunStatus::Succeeded, None).await;
    }

    /// Is this session one of a run's agents (lead or worker), with that run's policy?
    /// A session can be in two runs (a lead's fan-out worker is in the fan-out and the
    /// lead's run): the one with a policy wins, so the stricter rules always apply.
    pub(crate) fn run_of_session(&self, id: &str) -> Option<(String, Option<Policy>, bool)> {
        let runs = self.runs.lock().expect("runs");
        runs.values()
            .filter(|e| !e.state.status.is_over() && e.state.sessions.iter().any(|x| x == id))
            .max_by_key(|e| {
                (e.policy.is_some(), e.state.lead.as_deref() == Some(id), e.state.created_ms)
            })
            .map(|e| (e.state.id.clone(), e.policy.clone(), e.state.lead.as_deref() == Some(id)))
    }
}
