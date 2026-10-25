//! Change sets (`prefix S`, M11.11): every set, and a set's view: one swimlane per member plus
//! the lead (agent status, readiness, commits, PR, issues), the open gates, and the ledger as a
//! timeline where messages are arrows between lanes. Air traffic control for a multi-repo
//! change.
//!
//! The sets come from the runs the TUI already follows (`RunUpdated` carries each set's state);
//! the ledger is asked for (`Cs Log`) when a set opens and whenever its run changes.

use std::collections::BTreeMap;

use ptyro_core::input::{KeyCode, KeyEvent};
use ptyro_protocol::Request;
use ptyro_run::changeset::{ChangeSet, MemberLive, Report};
use ptyro_run::wire::{CsOp, LedgerEntry, RunInfo};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Overlay, Pending, PromptKind};

#[derive(Debug, Clone, Default)]
pub struct Sets {
    /// The open set's run, or the list.
    pub run: Option<String>,
    pub sel: usize,
    pub ledger: Vec<LedgerEntry>,
}

/// What a prompt typed in the set view is for.
#[derive(Debug, Clone)]
pub enum SetsInput {
    Message { set: String, to: String },
}

/// A sidebar node: a set, with its lead (who lives in the set's root, not a worktree).
#[derive(Debug, Clone)]
pub struct SetNode {
    pub run: String,
    /// The lead's session id (full), matched against the sessions when the tree is built.
    pub lead: Option<String>,
}

impl App {
    pub(crate) fn set_runs(&self) -> Vec<&RunInfo> {
        let mut v: Vec<&RunInfo> =
            self.runs.iter().filter(|r| r.state.changeset.is_some()).collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.state.created_ms));
        v
    }

    /// Sets for the sidebar: the ones still going.
    pub(crate) fn refresh_set_nodes(&mut self) {
        self.set_nodes = self
            .set_runs()
            .into_iter()
            .filter(|r| !r.state.status.is_over())
            .map(|r| SetNode { run: r.state.id.clone(), lead: r.state.lead.clone() })
            .collect();
    }

    /// What the daemon would say about each member, from what the TUI sees.
    pub(crate) fn set_live(&self, set: &ChangeSet) -> BTreeMap<String, MemberLive> {
        set.members
            .iter()
            .map(|m| {
                let s = m
                    .session
                    .as_deref()
                    .and_then(|x| self.sessions.iter().find(|s| s.id.full() == x));
                let status = s.map(|s| match &s.state {
                    ptyro_core::session::ProcessState::Running => s
                        .status
                        .as_ref()
                        .map_or_else(|| "running".to_owned(), |r| r.status.label().to_owned()),
                    _ => "exited".to_owned(),
                });
                let st = m
                    .worktree
                    .as_deref()
                    .and_then(|w| self.worktree(w))
                    .and_then(|w| w.status.clone());
                (
                    m.name.clone(),
                    MemberLive {
                        status,
                        dirty: st.as_ref().is_some_and(|s| s.modified + s.staged + s.untracked > 0),
                        ahead: st.as_ref().map_or(0, |s| s.base_ahead),
                        ..MemberLive::default()
                    },
                )
            })
            .collect()
    }

    pub(crate) fn set_report(&self, r: &RunInfo) -> Option<Report> {
        let set = r.state.changeset.as_ref()?;
        let gate = r.state.open_gates().iter().any(|(s, _)| s.starts_with("contract"));
        Some(set.report(&self.set_live(set), gate))
    }

    /// `◆ tier  working · 2/3 ready · r2`, for the sidebar and the list.
    pub(crate) fn set_label(&self, r: &RunInfo) -> (String, bool) {
        let Some(set) = &r.state.changeset else { return (String::new(), false) };
        let rep = self.set_report(r).unwrap_or_default();
        let attention = !r.state.open_gates().is_empty()
            || rep.issues.iter().any(|i| i.state != "no_remote" && i.state != "out_of_fence");
        let rev = set.contract_head().map(|c| format!(" · r{}", c.n)).unwrap_or_default();
        (format!("{} {}/{} ready{rev}", rep.state, rep.ready.0, rep.ready.1), attention)
    }

    pub fn open_sets(&mut self, run: Option<String>) {
        // One full-screen view at a time.
        self.inspector = None;
        self.orch = None;
        self.sets_view = Some(Sets { run: run.clone(), sel: 0, ledger: vec![] });
        self.request(Pending::Runs, Request::RunList { active: false, limit: Some(100) });
        if let Some(r) = run {
            self.request_ledger(&r);
        }
    }

    fn request_ledger(&self, run: &str) {
        if let Some(name) = self
            .runs
            .iter()
            .find(|r| r.state.id == run)
            .and_then(|r| r.state.changeset.as_ref())
            .map(|c| c.name.clone())
        {
            self.request(
                Pending::CsLog(run.to_owned()),
                Request::Cs { op: CsOp::Log { set: name, since: None } },
            );
        }
    }

    /// A set's run changed: refresh the open ledger.
    pub(crate) fn set_run_changed(&mut self, run: &str) {
        if self.sets_view.as_ref().and_then(|v| v.run.as_deref()) == Some(run) {
            self.request_ledger(run);
        }
    }

    pub(crate) fn got_ledger(&mut self, run: &str, entries: Vec<LedgerEntry>) {
        if let Some(v) = &mut self.sets_view
            && v.run.as_deref() == Some(run)
        {
            v.ledger = entries;
        }
    }

    pub fn sets_input(&mut self, input: SetsInput, text: String) {
        match input {
            SetsInput::Message { set, to } => {
                if text.trim().is_empty() {
                    return;
                }
                self.request(
                    Pending::Done(format!("queued for {to} (delivered between its turns)")),
                    Request::Cs { op: CsOp::Msg { set, to, text, kind: None, interrupt: false } },
                );
            }
        }
    }

    pub fn sets_key(&mut self, key: KeyEvent) {
        let Some(mut v) = self.sets_view.take() else { return };
        let ch = match key.code {
            KeyCode::Char { c } => Some(c),
            _ => None,
        };
        let down = matches!(key.code, KeyCode::Down) || ch == Some('j');
        let up = matches!(key.code, KeyCode::Up) || ch == Some('k');
        match v.run.clone() {
            None => {
                let ids: Vec<String> = self.set_runs().iter().map(|r| r.state.id.clone()).collect();
                match (key.code, ch) {
                    (KeyCode::Esc, _) | (_, Some('q')) => return,
                    _ if down => v.sel = (v.sel + 1).min(ids.len().saturating_sub(1)),
                    _ if up => v.sel = v.sel.saturating_sub(1),
                    (KeyCode::Enter, _) => {
                        if let Some(id) = ids.get(v.sel) {
                            v.run = Some(id.clone());
                            v.sel = 0;
                            self.request_ledger(id);
                        }
                    }
                    _ => {}
                }
            }
            Some(run) => {
                let Some(r) = self.runs.iter().find(|x| x.state.id == run).cloned() else {
                    v.run = None;
                    self.sets_view = Some(v);
                    return;
                };
                let Some(set) = r.state.changeset.clone() else {
                    self.sets_view = Some(v);
                    return;
                };
                // Lanes: the lead, then the members.
                let lanes: Vec<(String, Option<String>)> =
                    std::iter::once(("lead".to_owned(), r.state.lead.clone()))
                        .filter(|(_, s)| s.is_some())
                        .chain(set.members.iter().map(|m| (m.name.clone(), m.session.clone())))
                        .collect();
                let gates = r.state.open_gates();
                match (key.code, ch) {
                    (KeyCode::Esc, _) => {
                        v.run = None;
                        v.sel = 0;
                    }
                    (_, Some('q')) => return,
                    _ if down => v.sel = (v.sel + 1).min(lanes.len().saturating_sub(1)),
                    _ if up => v.sel = v.sel.saturating_sub(1),
                    (KeyCode::Enter, _) => {
                        if let Some(id) = lanes
                            .get(v.sel)
                            .and_then(|(_, s)| s.as_deref())
                            .and_then(|s| self.sessions.iter().find(|x| x.id.full() == s))
                            .map(|s| s.id)
                        {
                            self.sets_view = None;
                            self.show(id, None);
                            return;
                        }
                        self.toast("no agent in that lane yet");
                    }
                    (_, Some(c @ '1'..='9')) => {
                        let i = (c as u8 - b'1') as usize;
                        if let Some((_, g)) = gates.first() {
                            if let Some(choice) = g.options.get(i) {
                                self.request(
                                    Pending::Done(format!("gate {}: {choice}", g.id)),
                                    Request::GateResolve {
                                        id: g.id.clone(),
                                        choice: choice.clone(),
                                        note: None,
                                        key: None,
                                        by: Some("tui".into()),
                                    },
                                );
                            }
                        } else {
                            self.toast("no gate waits in this set");
                        }
                    }
                    (_, Some('m')) => {
                        if let Some((lane, _)) = lanes.get(v.sel) {
                            let to = if lane == "lead" { "@lead".to_owned() } else { lane.clone() };
                            self.sets_view = Some(v);
                            self.overlay = Some(Overlay::Prompt {
                                title: format!("Message {to} in {}", set.name),
                                text: String::new(),
                                kind: PromptKind::Sets(SetsInput::Message {
                                    set: set.name.clone(),
                                    to,
                                }),
                            });
                            return;
                        }
                    }
                    (_, Some('c')) => {
                        self.toast("running the set's checks…");
                        self.request(
                            Pending::Done("checks done (results in the ledger)".into()),
                            Request::Cs { op: CsOp::Check { set: set.name.clone() } },
                        );
                    }
                    (_, Some('p')) => {
                        self.request(
                            Pending::Done("set checkpoint taken".into()),
                            Request::Cs {
                                op: CsOp::Checkpoint {
                                    set: set.name.clone(),
                                    label: Some("from the TUI".into()),
                                },
                            },
                        );
                    }
                    (_, Some('r')) => self.request_ledger(&run),
                    _ => {}
                }
            }
        }
        self.sets_view = Some(v);
    }
}

fn fg(app: &App, c: ratatui::style::Color) -> Style {
    crate::ui::fg(app, c)
}

fn word_color(app: &App, w: &str) -> Style {
    let t = &app.theme;
    fg(
        app,
        match w {
            "ready" | "landed" | "idle" | "done" => t.done,
            "working" | "running" => t.working,
            "failed" | "conflict" | "missing" | "errored" => t.error,
            x if x.starts_with("blocked") || x == "stale" => t.blocked,
            _ => t.dim,
        },
    )
}

pub fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(v) = &app.sets_view else { return };
    let t = &app.theme;
    f.render_widget(Clear, area);
    let run = v.run.as_deref().and_then(|id| app.runs.iter().find(|r| r.state.id == id));
    let (title, help) = match run.and_then(|r| r.state.changeset.as_ref()) {
        Some(c) => (
            format!(" Change set {} ", c.name),
            "j/k lane · ⏎ show its agent · m message · 1-9 answer the gate · c checks · p checkpoint · r refresh · Esc back",
        ),
        None => (
            " Change sets ".to_owned(),
            "j/k · ⏎ open · q close   (ptyro cs new NAME --projects a,b starts one)",
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
    // The line the selected row is on: the view scrolls to keep it in sight.
    let mut sel_line = 0usize;
    match run {
        None => {
            let sets = app.set_runs();
            if sets.is_empty() {
                lines.push(Line::styled("No change sets yet.", fg(app, t.dim)));
            }
            for (i, r) in sets.iter().enumerate() {
                let Some(c) = &r.state.changeset else { continue };
                let (label, attention) = app.set_label(r);
                let mut l = Line::from(vec![
                    Span::styled(" ◆ ", fg(app, if attention { t.blocked } else { t.accent })),
                    Span::styled(
                        format!("{:<20}", c.name),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {label:<32}"),
                        fg(app, if attention { t.blocked } else { t.text }),
                    ),
                    Span::styled(
                        format!(
                            " {} · {}",
                            c.members
                                .iter()
                                .map(|m| m.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            r.state.status.label()
                        ),
                        fg(app, t.dim),
                    ),
                ]);
                if i == v.sel {
                    l = l.style(sel_style);
                    sel_line = lines.len();
                }
                lines.push(l);
            }
        }
        Some(r) => {
            let Some(c) = &r.state.changeset else { return };
            let rep = app.set_report(r).unwrap_or_default();
            let (label, attention) = app.set_label(r);
            lines.push(Line::from(vec![
                Span::styled(" ◆ ", fg(app, if attention { t.blocked } else { t.accent })),
                Span::styled(format!("{label}  "), Style::new().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(
                        "{} layout · order {}",
                        c.layout.label(),
                        c.order().map(|o| o.join(" → ")).unwrap_or_default()
                    ),
                    fg(app, t.dim),
                ),
            ]));
            if let Some(g) = &c.goal {
                lines.push(Line::styled(
                    format!("   {}", g.lines().next().unwrap_or("")),
                    fg(app, t.dim),
                ));
            }
            lines.push(Line::raw(""));
            // Swimlanes.
            let mut lanes: Vec<(String, String, String)> = Vec::new(); // name, agent status, detail
            if let Some(l) = &r.state.lead {
                let s = app.sessions.iter().find(|s| s.id.full() == *l);
                let st = s
                    .and_then(|s| s.status.as_ref().map(|x| x.status.label().to_owned()))
                    .unwrap_or_else(|| "—".into());
                lanes.push(("lead".into(), st, s.map(|s| s.name.clone()).unwrap_or_default()));
            }
            let live = app.set_live(c);
            for m in &c.members {
                let word = rep.members.get(&m.name).cloned().unwrap_or_default();
                let l = live.get(&m.name).cloned().unwrap_or_default();
                let mut bits = vec![m.branch.clone()];
                if l.ahead > 0 {
                    bits.push(format!("+{}", l.ahead));
                }
                if l.dirty {
                    bits.push("uncommitted".into());
                }
                if let Some(p) = m.pr {
                    bits.push(format!("PR #{p}"));
                }
                if let Some(x) = &m.landed {
                    bits.push(x.clone());
                }
                lanes.push((m.name.clone(), word, bits.join(" · ")));
            }
            for (i, (name, word, detail)) in lanes.iter().enumerate() {
                let mut l = Line::from(vec![
                    Span::styled(
                        format!(" {:<10}", name),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {:<18}", word), word_color(app, word)),
                    Span::styled(format!(" {detail}"), fg(app, t.dim)),
                ]);
                if i == v.sel {
                    l = l.style(sel_style);
                    sel_line = lines.len();
                }
                lines.push(l);
            }
            for (step, g) in r.state.open_gates() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" ⏸ gate {} ({step}): ", g.id),
                        fg(app, t.blocked).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(g.ask.clone()),
                    Span::styled(
                        format!(
                            "  {}",
                            g.options
                                .iter()
                                .enumerate()
                                .map(|(i, o)| format!("{} {o}", i + 1))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        ),
                        fg(app, t.dim),
                    ),
                ]));
            }
            for x in rep.issues.iter().take(4) {
                lines.push(Line::from(vec![
                    Span::styled(format!(" ✗ {}: ", x.state), fg(app, t.error)),
                    Span::raw(x.detail.clone()),
                    Span::styled(format!("  → {}", x.action), fg(app, t.dim)),
                ]));
            }
            lines.push(Line::raw(""));
            // The ledger as a timeline: one column per lane, messages as arrows between them.
            let names: Vec<String> = lanes.iter().map(|(n, _, _)| n.clone()).collect();
            let col = |who: &str| -> Option<usize> {
                let w = who.trim_start_matches('@');
                let w = w.strip_prefix(&format!("{}-", c.name)).unwrap_or(w);
                names.iter().position(|n| n == w)
            };
            let cw = 8usize;
            let mut header = String::from("        ");
            for n in &names {
                header.push_str(&format!("{:<cw$}", n.chars().take(cw - 1).collect::<String>()));
            }
            lines.push(Line::styled(header, fg(app, t.dim).add_modifier(Modifier::UNDERLINED)));
            let room = (body.height as usize).saturating_sub(lines.len());
            let start = v.ledger.len().saturating_sub(room);
            for e in &v.ledger[start..] {
                let mut cells = vec![' '; names.len() * cw];
                fn mark(cells: &mut [char], cw: usize, i: usize, ch: char) {
                    if let Some(x) = cells.get_mut(i * cw + 1) {
                        *x = ch;
                    }
                }
                match e.t.as_str() {
                    "message" => {
                        let (a, b) =
                            (e.from.as_deref().and_then(col), e.to.as_deref().and_then(col));
                        if let (Some(a), Some(b)) = (a, b) {
                            let (lo, hi) = (a.min(b), a.max(b));
                            for c in cells.iter_mut().take(hi * cw + 1).skip(lo * cw + 2) {
                                *c = '─';
                            }
                            mark(&mut cells, cw, a, '●');
                            mark(&mut cells, cw, b, if b > a { '▶' } else { '◀' });
                        } else if let Some(b) = b {
                            mark(&mut cells, cw, b, '▶');
                        }
                    }
                    _ => {
                        let who = names.iter().position(|n| {
                            e.text.starts_with(&format!("{n} "))
                                || e.text.starts_with(&format!("{n}:"))
                                || e.text.starts_with(&format!("{n}'s"))
                        });
                        if let Some(i) = who {
                            mark(&mut cells, cw, i, '◆');
                        }
                    }
                }
                let text: String = e.text.lines().next().unwrap_or("").chars().take(100).collect();
                let label = if e.t == "message" { format!("[{}] {text}", e.kind) } else { text };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {:>5} ", crate::ui::ago(e.at_ms)), fg(app, t.dim)),
                    Span::styled(cells.into_iter().collect::<String>(), fg(app, t.accent)),
                    Span::styled(label, fg(app, if e.t == "message" { t.text } else { t.dim })),
                ]));
            }
        }
    }
    let h = body.height as usize;
    let skip = lines.len().saturating_sub(h).min(sel_line.saturating_sub(h / 2));
    f.render_widget(Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()), body);
}
