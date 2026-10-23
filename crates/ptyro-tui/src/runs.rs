//! Runs (`O`, M7): every run with its steps, attempts and gates; the compare view of a fan-out
//! (candidates ranked by evidence, pick / discard / iterate); and answering gates in place.
//!
//! Lists come from `RunList` and stay current through `RunUpdated` events; the compare view asks
//! the daemon (`Compare`), which keeps its evidence on the run.

use ptyro_core::SessionId;
use ptyro_core::input::{KeyCode, KeyEvent};
use ptyro_protocol::Request;
use ptyro_run::wire::{CompareDoc, PickMode, RunInfo};
use ptyro_run::{RunStatus, StepStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Confirmed, Overlay, Pending, PromptKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    List,
    Run(String),
    Compare(String),
}

#[derive(Debug, Clone)]
pub struct Orch {
    pub view: View,
    pub sel: usize,
    pub compare: Option<CompareDoc>,
    pub loading: bool,
}

/// What a prompt typed in the runs view is for.
#[derive(Debug, Clone)]
pub enum OrchInput {
    Iterate { run: String, candidate: Option<String> },
    FanoutAgents { project: String },
    FanoutPrompt { project: String, agents: Vec<String> },
    GateNote { gate: String, choice: String },
}

fn step_icon(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "·",
        StepStatus::Running => "●",
        StepStatus::Waiting => "⏸",
        StepStatus::Succeeded => "✓",
        StepStatus::Failed => "✗",
        StepStatus::Crashed => "↯",
        StepStatus::Skipped => "–",
        StepStatus::Cancelled => "⊘",
    }
}

fn run_icon(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Running => "●",
        RunStatus::Waiting => "⏸",
        RunStatus::Succeeded => "✓",
        RunStatus::Failed => "✗",
        RunStatus::Cancelled => "⊘",
    }
}

impl App {
    pub(crate) fn run_updated(&mut self, run: RunInfo) {
        let opened = run.state.open_gates().len();
        let before = self
            .runs
            .iter()
            .find(|r| r.state.id == run.state.id)
            .map_or(0, |r| r.state.open_gates().len());
        if opened > before
            && let Some((_, g)) = run.state.open_gates().last()
        {
            self.toast_offer(
                format!("gate {}: {}", g.id, g.ask),
                false,
                crate::app::Offer {
                    label: "answer it".into(),
                    then: crate::app::Then::Run(crate::keymap::Command {
                        action: "runs",
                        arg: None,
                    }),
                },
            );
        }
        let id = run.state.id.clone();
        let set = run.state.changeset.is_some();
        self.keeping_run_selection(|app| {
            match app.runs.iter_mut().find(|r| r.state.id == run.state.id) {
                Some(r) => *r = run,
                None => app.runs.insert(0, run),
            }
            app.runs.sort_by_key(|r| std::cmp::Reverse(r.state.created_ms));
        });
        if set {
            self.refresh_set_nodes();
            self.set_run_changed(&id);
        }
    }

    /// Change `runs`, keeping the runs list and the set list on the run they had selected.
    pub(crate) fn keeping_run_selection(&mut self, change: impl FnOnce(&mut Self)) {
        let orch = self
            .orch
            .as_ref()
            .filter(|o| o.view == View::List)
            .and_then(|o| self.runs.get(o.sel))
            .map(|r| r.state.id.clone());
        let set = self
            .sets_view
            .as_ref()
            .filter(|v| v.run.is_none())
            .and_then(|v| self.set_runs().get(v.sel).map(|r| r.state.id.clone()));
        change(self);
        if let Some(id) = orch
            && let Some(i) = self.runs.iter().position(|r| r.state.id == id)
            && let Some(o) = &mut self.orch
        {
            o.sel = i;
        }
        if let Some(id) = set
            && let Some(i) = self.set_runs().iter().position(|r| r.state.id == id)
            && let Some(v) = &mut self.sets_view
        {
            v.sel = i;
        }
    }

    pub fn open_runs(&mut self) {
        // One full-screen view at a time.
        self.inspector = None;
        self.sets_view = None;
        self.orch = Some(Orch { view: View::List, sel: 0, compare: None, loading: true });
        self.request(Pending::Runs, Request::RunList { active: false, limit: Some(100) });
    }

    /// Open a run's page (from the inbox's gate entries, too).
    pub fn open_run(&mut self, id: String) {
        self.inspector = None;
        self.sets_view = None;
        self.orch = Some(Orch { view: View::Run(id), sel: 0, compare: None, loading: false });
        self.request(Pending::Runs, Request::RunList { active: false, limit: Some(100) });
    }

    fn open_compare(&mut self, run: String, refresh: bool) {
        if let Some(o) = &mut self.orch {
            o.view = View::Compare(run.clone());
            o.sel = 0;
            o.loading = true;
            if refresh {
                o.compare = None;
            }
        }
        if refresh {
            self.toast("gathering evidence (tests, repro, cross-tests)…");
        }
        self.request(Pending::Compare(run.clone()), Request::Compare { run, refresh });
    }

    /// A compare doc arrived: only for the run whose compare view is still open.
    pub(crate) fn got_compare(&mut self, run: &str, doc: CompareDoc) {
        if let Some(o) = &mut self.orch
            && o.view == View::Compare(run.to_owned())
        {
            o.compare = Some(doc);
            o.loading = false;
        }
    }

    fn run_by_id(&self, id: &str) -> Option<&RunInfo> {
        self.runs.iter().find(|r| r.state.id == id)
    }

    /// The project new orchestration would use: the focused session's, else the only one.
    fn current_project(&self) -> Option<String> {
        self.focused_session()
            .and_then(|id| self.session(id))
            .and_then(|s| s.project.clone())
            .or_else(|| (self.projects.len() == 1).then(|| self.projects[0].id.clone()))
    }

    pub fn orch_input(&mut self, input: OrchInput, text: String) {
        match input {
            OrchInput::Iterate { run, candidate } => {
                if !text.trim().is_empty() {
                    self.request(
                        Pending::Done(String::new()),
                        Request::Iterate { run, candidate, text },
                    );
                }
            }
            OrchInput::FanoutAgents { project } => {
                let agents: Vec<String> = text
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect();
                if agents.is_empty() {
                    return;
                }
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Fan out to {}: the prompt", agents.join(", ")),
                    text: String::new(),
                    kind: PromptKind::Orch(OrchInput::FanoutPrompt { project, agents }),
                });
            }
            OrchInput::FanoutPrompt { project, agents } => {
                if text.trim().is_empty() {
                    return;
                }
                let spec = ptyro_run::wire::FanoutSpec {
                    agents,
                    prompt: text,
                    project: Some(project),
                    ..Default::default()
                };
                self.request(
                    Pending::Done(format!(
                        "fan-out started ({} O)",
                        ptyro_core::config::key_label(&self.keymap.prefix)
                    )),
                    Request::Fanout { spec },
                );
            }
            OrchInput::GateNote { gate, choice } => {
                let note = (!text.trim().is_empty()).then_some(text);
                self.request(
                    Pending::Done(format!("gate {gate}: {choice}")),
                    Request::GateResolve {
                        id: gate,
                        choice,
                        note,
                        key: None,
                        by: Some("tui".into()),
                    },
                );
            }
        }
    }

    pub fn orch_key(&mut self, key: KeyEvent) {
        let Some(mut o) = self.orch.take() else { return };
        let ch = match key.code {
            KeyCode::Char { c } => Some(c),
            _ => None,
        };
        let down = matches!(key.code, KeyCode::Down) || ch == Some('j');
        let up = matches!(key.code, KeyCode::Up) || ch == Some('k');
        match o.view.clone() {
            View::List => {
                let n = self.runs.len();
                let cur = self.runs.get(o.sel).map(|r| r.state.id.clone());
                match (key.code, ch) {
                    (KeyCode::Esc, _) | (_, Some('q')) => return,
                    _ if down => o.sel = (o.sel + 1).min(n.saturating_sub(1)),
                    _ if up => o.sel = o.sel.saturating_sub(1),
                    (KeyCode::Enter, _) => {
                        if let Some(id) = cur {
                            o.view = View::Run(id);
                            o.sel = 0;
                        }
                    }
                    (_, Some('c')) => {
                        if let Some(id) = cur {
                            self.orch = Some(o);
                            self.open_compare(id, false);
                            return;
                        }
                    }
                    (_, Some('x')) => {
                        if let Some(id) = cur.filter(|id| {
                            self.run_by_id(id).is_some_and(|r| !r.state.status.is_over())
                        }) {
                            self.orch = Some(o);
                            self.overlay = Some(Overlay::Confirm {
                                text: format!(
                                    "Cancel run {id}? Its running agents are stopped (worktrees stay). [y/N]"
                                ),
                                action: Confirmed::Send(
                                    Box::new(Request::RunCancel { id: id.clone() }),
                                    format!("run {id} cancelled"),
                                ),
                            });
                            return;
                        }
                    }
                    (_, Some('F')) => {
                        self.orch = Some(o);
                        match self.current_project() {
                            Some(project) => {
                                self.overlay = Some(Overlay::Prompt {
                                    title: "Fan out: agents (comma-separated)".into(),
                                    text: "claude,codex,gemini".into(),
                                    kind: PromptKind::Orch(OrchInput::FanoutAgents { project }),
                                })
                            }
                            None => self.toast_offer(
                                "a fan-out needs a project: focus a session in one",
                                false,
                                crate::app::Offer {
                                    label: "add a project".into(),
                                    then: crate::app::Then::Run(crate::keymap::Command {
                                        action: "project.add",
                                        arg: None,
                                    }),
                                },
                            ),
                        }
                        return;
                    }
                    _ => {}
                }
            }
            View::Run(id) => {
                let Some(run) = self.run_by_id(&id).cloned() else {
                    o.view = View::List;
                    self.orch = Some(o);
                    return;
                };
                let n = run.order.len();
                let step = run.order.get(o.sel).cloned();
                let ss = step.as_ref().and_then(|s| run.state.steps.get(s)).cloned();
                match (key.code, ch) {
                    (KeyCode::Esc, _) | (_, Some('q')) => {
                        o.view = View::List;
                        o.sel = self.runs.iter().position(|r| r.state.id == id).unwrap_or(0);
                    }
                    _ if down => o.sel = (o.sel + 1).min(n.saturating_sub(1)),
                    _ if up => o.sel = o.sel.saturating_sub(1),
                    (KeyCode::Enter, _) => {
                        if let Some(sid) = ss
                            .as_ref()
                            .and_then(|s| s.attempt())
                            .and_then(|a| a.session.clone())
                            .and_then(|s| s.parse::<SessionId>().ok())
                            .filter(|s| self.session(*s).is_some())
                        {
                            self.show(sid, None);
                            return;
                        }
                        self.toast("no session for this step");
                    }
                    (_, Some(d @ '1'..='9')) => {
                        if let Some(g) =
                            ss.as_ref().and_then(|s| s.gate.clone()).filter(|g| g.choice.is_none())
                        {
                            let i = d as usize - '1' as usize;
                            if let Some(choice) = g.options.get(i).cloned() {
                                self.request(
                                    Pending::Done(format!("gate {}: {choice}", g.id)),
                                    Request::GateResolve {
                                        id: g.id.clone(),
                                        choice,
                                        note: None,
                                        key: None,
                                        by: Some("tui".into()),
                                    },
                                );
                            }
                        }
                    }
                    (_, Some('n')) => {
                        if let Some(g) =
                            ss.as_ref().and_then(|s| s.gate.clone()).filter(|g| g.choice.is_none())
                        {
                            let choice = g.options.first().cloned().unwrap_or_default();
                            self.orch = Some(o);
                            self.overlay = Some(Overlay::Prompt {
                                title: format!("Gate {}: answer {choice:?} with a note", g.id),
                                text: String::new(),
                                kind: PromptKind::Orch(OrchInput::GateNote { gate: g.id, choice }),
                            });
                            return;
                        }
                    }
                    (_, Some('r')) => {
                        if let Some(s) = step.filter(|_| {
                            ss.as_ref().is_some_and(|x| x.status != StepStatus::Running)
                        }) {
                            self.request(
                                Pending::Done(format!("retrying {s}")),
                                Request::RunRetry { id: id.clone(), step: s },
                            );
                        }
                    }
                    (_, Some('c')) => {
                        self.orch = Some(o);
                        self.open_compare(id, false);
                        return;
                    }
                    (_, Some('D')) => {
                        if let Some(w) = ss
                            .as_ref()
                            .and_then(|s| s.attempts.iter().rev().find_map(|a| a.worktree.clone()))
                        {
                            self.orch = None;
                            self.inspect(w);
                            return;
                        }
                    }
                    (_, Some('x')) if !run.state.status.is_over() => {
                        self.orch = Some(o);
                        self.overlay = Some(Overlay::Confirm {
                            text: format!("Cancel run {id}? [y/N]"),
                            action: Confirmed::Send(
                                Box::new(Request::RunCancel { id: id.clone() }),
                                format!("run {id} cancelled"),
                            ),
                        });
                        return;
                    }
                    _ => {}
                }
            }
            View::Compare(id) => {
                let n = o.compare.as_ref().map_or(0, |d| d.candidates.len());
                let cand = o.compare.as_ref().and_then(|d| d.candidates.get(o.sel)).cloned();
                match (key.code, ch) {
                    (KeyCode::Esc, _) | (_, Some('q')) => {
                        o.view = View::Run(id);
                        o.sel = 0;
                    }
                    _ if down => o.sel = (o.sel + 1).min(n.saturating_sub(1)),
                    _ if up => o.sel = o.sel.saturating_sub(1),
                    (KeyCode::Enter, _) => {
                        if let Some(sid) = cand
                            .as_ref()
                            .and_then(|c| c.session.clone())
                            .and_then(|s| s.parse::<SessionId>().ok())
                            .filter(|s| self.session(*s).is_some())
                        {
                            self.show(sid, None);
                            return;
                        }
                    }
                    (_, Some('D')) => {
                        if let Some(w) = cand.as_ref().and_then(|c| c.worktree.clone()) {
                            self.orch = None;
                            self.inspect(w);
                            return;
                        }
                    }
                    (_, Some(m @ ('m' | 'p' | 'C'))) => {
                        if let Some(c) = cand {
                            let (mode, what) = match m {
                                'm' => (
                                    PickMode::Merge,
                                    "Merge (rebase + fast-forward) into the main branch",
                                ),
                                'C' => (
                                    PickMode::CherryPick,
                                    "Cherry-pick its commits onto the main branch",
                                ),
                                _ => (PickMode::Pr, "Push it and open a pull request"),
                            };
                            self.orch = Some(o);
                            self.overlay = Some(Overlay::Confirm {
                                text: format!(
                                    "{what}: {}? Uncommitted work is committed first. [y/N]",
                                    c.step
                                ),
                                action: Confirmed::Send(
                                    Box::new(Request::Pick {
                                        run: id.clone(),
                                        candidate: c.step.clone(),
                                        mode,
                                        into: None,
                                        paths: vec![],
                                        hunks: vec![],
                                    }),
                                    format!("picked {}", c.step),
                                ),
                            });
                            return;
                        }
                    }
                    (_, Some('d')) => {
                        if let Some(c) = cand {
                            self.orch = Some(o);
                            self.overlay = Some(Overlay::Confirm {
                                text: format!(
                                    "Discard {} (stop its agent, remove its worktree; refused if work would be lost)? [y/N]",
                                    c.step
                                ),
                                action: Confirmed::Send(
                                    Box::new(Request::Discard {
                                        run: id.clone(),
                                        candidate: Some(c.step.clone()),
                                        force: false,
                                    }),
                                    String::new(),
                                ),
                            });
                            return;
                        }
                    }
                    (_, Some('X')) => {
                        self.orch = Some(o);
                        self.overlay = Some(Overlay::Confirm {
                            text: "Discard every candidate you didn't pick? [y/N]".into(),
                            action: Confirmed::Send(
                                Box::new(Request::Discard {
                                    run: id.clone(),
                                    candidate: None,
                                    force: false,
                                }),
                                String::new(),
                            ),
                        });
                        return;
                    }
                    (_, Some(i @ ('i' | 'I'))) => {
                        let candidate = if i == 'i' { cand.map(|c| c.step) } else { None };
                        let title = match &candidate {
                            Some(c) => format!("Follow-up for {c}"),
                            None => "Follow-up for every candidate".into(),
                        };
                        self.orch = Some(o);
                        self.overlay = Some(Overlay::Prompt {
                            title,
                            text: String::new(),
                            kind: PromptKind::Orch(OrchInput::Iterate { run: id, candidate }),
                        });
                        return;
                    }
                    (_, Some('R')) => {
                        self.orch = Some(o);
                        self.open_compare(id, true);
                        return;
                    }
                    _ => {}
                }
            }
        }
        self.orch = Some(o);
    }

    /// Open gates across runs (the status bar's ⏸ count).
    pub fn open_gates(&self) -> usize {
        self.runs.iter().map(|r| r.state.open_gates().len()).sum()
    }
}

fn fg(app: &App, c: ratatui::style::Color) -> Style {
    crate::ui::fg(app, c)
}

fn status_color(app: &App, label: &str) -> Style {
    let t = &app.theme;
    fg(
        app,
        match label {
            "running" => t.working,
            "waiting" => t.blocked,
            "succeeded" => t.done,
            "failed" | "crashed" => t.error,
            _ => t.dim,
        },
    )
}

pub fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(o) = &app.orch else { return };
    let t = &app.theme;
    f.render_widget(Clear, area);
    let (title, help) = match &o.view {
        View::List => {
            (" Runs ".to_owned(), "j/k · ⏎ open · c compare · x cancel · F fan out · q close")
        }
        View::Run(id) => (
            format!(" Run {id} "),
            "j/k · ⏎ show session · 1-9 answer gate · n answer with a note · r retry step · c compare · D inspect · x cancel · Esc back",
        ),
        View::Compare(id) => (
            format!(" Compare {id} "),
            "j/k · ⏎ session · D inspect · m merge · C cherry-pick · p PR · d discard · X discard the rest · i/I iterate one/all · R re-run evidence · Esc back",
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(app, t.border_focus))
        .title(Span::styled(title, fg(app, t.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let body = Rect { height: inner.height - 1, ..inner };
    let foot = Rect { y: inner.bottom() - 1, height: 1, ..inner };
    f.render_widget(Paragraph::new(Line::styled(help, fg(app, t.dim))), foot);
    let sel_style = fg(app, t.selection_fg).bg(app.depth.fit(t.selection_bg));
    let mut lines: Vec<Line<'_>> = Vec::new();
    match &o.view {
        View::List => {
            if app.runs.is_empty() {
                lines.push(Line::styled(
                    if o.loading {
                        "loading…"
                    } else {
                        "No runs yet: F gives one task to several agents and compares what they do"
                    },
                    fg(app, t.dim),
                ));
            }
            for (i, r) in app.runs.iter().enumerate() {
                let s = &r.state;
                let counts = s.steps.values().fold((0, 0, 0), |(d, f, a), x| match x.status {
                    StepStatus::Succeeded | StepStatus::Skipped => (d + 1, f, a),
                    StepStatus::Failed | StepStatus::Crashed => (d, f + 1, a),
                    StepStatus::Running | StepStatus::Waiting => (d, f, a + 1),
                    _ => (d, f, a),
                });
                let gates = s.open_gates().len();
                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", run_icon(s.status)),
                        status_color(app, s.status.label()),
                    ),
                    Span::styled(format!("{:<9}", s.id), Style::new().add_modifier(Modifier::BOLD)),
                    Span::raw(format!(
                        " {:<22}",
                        format!("{} {}", s.kind.label(), s.name)
                            .chars()
                            .take(22)
                            .collect::<String>()
                    )),
                    Span::styled(
                        format!(" {:<10}", s.status.label()),
                        status_color(app, s.status.label()),
                    ),
                    Span::styled(
                        format!(" {}✓ {}✗ {}● of {}", counts.0, counts.1, counts.2, s.steps.len()),
                        fg(app, t.dim),
                    ),
                    Span::styled(format!("  {} ago", crate::ui::ago(s.created_ms)), fg(app, t.dim)),
                ];
                if gates > 0 {
                    spans.push(Span::styled(
                        format!("  ⏸ {gates} gate(s)"),
                        fg(app, t.blocked).add_modifier(Modifier::BOLD),
                    ));
                }
                if let Some(e) = &s.error {
                    spans.push(Span::styled(
                        format!("  {}", e.lines().next().unwrap_or("")),
                        fg(app, t.error),
                    ));
                }
                let mut l = Line::from(spans);
                if i == o.sel {
                    l = l.style(sel_style);
                }
                lines.push(l);
            }
        }
        View::Run(id) => {
            let Some(r) = app.runs.iter().find(|r| &r.state.id == id) else {
                lines.push(Line::raw("this run is gone"));
                f.render_widget(Paragraph::new(lines), body);
                return;
            };
            let s = &r.state;
            let vars: Vec<String> = s
                .vars
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.lines().next().unwrap_or("").chars().take(40).collect::<String>()
                    )
                })
                .collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {} ", run_icon(s.status), s.status.label()),
                    status_color(app, s.status.label()),
                ),
                Span::raw(format!("{} {}  ", s.kind.label(), s.name)),
                Span::styled(vars.join("  "), fg(app, t.dim)),
            ]));
            if let Some(e) = &s.error {
                lines.push(Line::styled(format!("  {e}"), fg(app, t.error)));
            }
            if let Some((from, step)) = &s.forked_from {
                lines.push(Line::styled(format!("  forked from {from} at {step}"), fg(app, t.dim)));
            }
            lines.push(Line::raw(""));
            for (i, sid) in r.order.iter().enumerate() {
                let Some(ss) = s.steps.get(sid) else { continue };
                let a = ss.attempt();
                let who = a
                    .and_then(|a| a.session.as_deref())
                    .and_then(|x| x.parse::<SessionId>().ok())
                    .and_then(|x| app.session(x))
                    .map(|x| x.name.clone());
                let dur = ss.started_ms.map(|st| {
                    crate::ui::dur(
                        ss.ended_ms.unwrap_or_else(ptyro_core::now_ms).saturating_sub(st),
                    )
                });
                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", step_icon(ss.status)),
                        status_color(app, ss.status.label()),
                    ),
                    Span::styled(format!("{:<14}", sid), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(" {:<10}", ss.status.label()),
                        status_color(app, ss.status.label()),
                    ),
                ];
                if ss.attempts.len() > 1 {
                    spans.push(Span::styled(
                        format!(" try {}", ss.attempts.len()),
                        fg(app, t.blocked),
                    ));
                }
                if let Some(b) = a.map(|a| a.bounces).filter(|b| *b > 0) {
                    spans.push(Span::styled(format!(" ↩{b}"), fg(app, t.blocked)));
                }
                if let Some(w) = who {
                    spans.push(Span::styled(format!(" {w}"), fg(app, t.accent)));
                }
                if let Some(d) = dur {
                    spans.push(Span::styled(format!(" {d}"), fg(app, t.dim)));
                }
                if let Some(x) = ss.outputs.get("diffstat").and_then(|v| v.as_str()) {
                    spans.push(Span::styled(format!("  {x}"), fg(app, t.dim)));
                }
                if let Some(x) = ss.outputs.get("exit_code") {
                    spans.push(Span::styled(format!("  exit {x}"), fg(app, t.dim)));
                }
                if let Some(x) = ss.outputs.get("best").and_then(|v| v.as_str()) {
                    spans.push(Span::styled(format!("  best: {x}"), fg(app, t.done)));
                }
                let mut l = Line::from(spans);
                if i == o.sel {
                    l = l.style(sel_style);
                }
                lines.push(l);
                if let Some(g) = &ss.gate {
                    match &g.choice {
                        None => {
                            lines.push(Line::styled(
                                format!("     ⏸ {} — {}", g.id, g.ask),
                                fg(app, t.blocked).add_modifier(Modifier::BOLD),
                            ));
                            let opts: Vec<String> = g
                                .options
                                .iter()
                                .enumerate()
                                .map(|(n, x)| format!("{} {x}", n + 1))
                                .collect();
                            lines.push(Line::styled(
                                format!("       {}", opts.join("   ")),
                                fg(app, t.accent),
                            ));
                        }
                        Some(c) => lines.push(Line::styled(
                            format!(
                                "     answered {c:?} by {}{}",
                                g.by.clone().unwrap_or_default(),
                                g.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default()
                            ),
                            fg(app, t.dim),
                        )),
                    }
                }
                if let Some(reason) =
                    ss.reason.as_deref().filter(|_| ss.status != StepStatus::Succeeded)
                {
                    for rl in reason.lines().take(4) {
                        lines.push(Line::styled(
                            format!("     {rl}"),
                            fg(app, if ss.status == StepStatus::Skipped { t.dim } else { t.error }),
                        ));
                    }
                }
            }
        }
        View::Compare(_) => match &o.compare {
            None => lines.push(Line::styled(
                if o.loading { "comparing…" } else { "nothing to compare" },
                fg(app, t.dim),
            )),
            Some(d) => {
                if let Some(b) = d.base_tests {
                    lines.push(Line::styled(
                        format!(
                            " base {}: tests {}",
                            d.base.clone().unwrap_or_default(),
                            if b == 0 { "pass".to_owned() } else { format!("fail (exit {b})") }
                        ),
                        fg(app, t.dim),
                    ));
                }
                for w in &d.warnings {
                    lines.push(Line::styled(format!(" ⚠ {w}"), fg(app, t.blocked)));
                }
                lines.push(Line::raw(""));
                for (i, c) in d.candidates.iter().enumerate() {
                    let mark = if c.picked {
                        " ★"
                    } else if c.discarded {
                        " ⊘"
                    } else {
                        ""
                    };
                    let tests = match c.tests {
                        Some(0) => Span::styled(" tests ✓", fg(app, t.done)),
                        Some(_) => Span::styled(" tests ✗", fg(app, t.error)),
                        None => Span::raw(""),
                    };
                    let mut l = Line::from(vec![
                        Span::styled(
                            format!(" #{} ", c.rank),
                            Style::new().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{:<14}", format!("{}{mark}", c.step)),
                            fg(app, t.accent).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!(" {:<9}", c.status), status_color(app, &c.status)),
                        tests,
                        Span::styled(
                            format!("  +{} −{} in {} file(s)", c.added, c.removed, c.files),
                            fg(app, t.dim),
                        ),
                        Span::styled(
                            format!("  {}", crate::ui::dur(c.duration_ms)),
                            fg(app, t.dim),
                        ),
                        Span::styled(
                            c.agent.as_deref().map(|a| format!("  {a}")).unwrap_or_default(),
                            fg(app, t.dim),
                        ),
                    ]);
                    if i == o.sel {
                        l = l.style(sel_style);
                    }
                    lines.push(l);
                    lines.push(Line::styled(
                        format!("      {}", c.reasons.join(" · ")),
                        fg(app, t.dim),
                    ));
                    if let Some(m) = &c.last_message {
                        lines.push(Line::styled(
                            format!("      “{}”", m.chars().take(120).collect::<String>()),
                            fg(app, t.dim),
                        ));
                    }
                    // Structured candidates (M10): their tool calls.
                    if let Some(u) = &c.tools {
                        let by: Vec<String> =
                            u.by_tool.iter().map(|(k, v)| format!("{k} {v}")).collect();
                        let mut line = format!("      ⚒ {} calls ({})", u.calls, by.join(", "));
                        if u.failed > 0 {
                            line.push_str(&format!(", {} failed", u.failed));
                        }
                        if let Some(cost) = u.cost_micro_usd {
                            line.push_str(&format!(" · ${:.4}", cost as f64 / 1e6));
                        }
                        lines.push(Line::styled(line, fg(app, t.accent)));
                        for cmd in u.commands.iter().take(3) {
                            lines.push(Line::styled(format!("        $ {cmd}"), fg(app, t.dim)));
                        }
                    }
                }
                if let Some(td) = &d.top_difference {
                    lines.push(Line::raw(""));
                    lines.push(Line::styled(" between the top two:", fg(app, t.dim)));
                    for l in td.lines().take(12) {
                        lines.push(Line::styled(format!("   {l}"), fg(app, t.dim)));
                    }
                }
            }
        },
    }
    // Keep the selection in view.
    let h = body.height as usize;
    let skip = lines.len().saturating_sub(h).min(o.sel.saturating_sub(h / 2));
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), body);
}
