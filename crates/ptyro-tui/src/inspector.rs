//! The inspector (`D`, SPEC §11.5, M6): one worktree's Diff, Commits, Checks, Ports and Events.
//!
//! In the Diff tab, `c` comments on the line under the cursor; the comments wait (in the
//! daemon) until `S` sends them to the worktree's agent as one prompt. `Space` marks files for a
//! commit, `C` commits them (or everything) as the worktree's profile, `A` asks the agent to
//! commit, `P` opens a draft PR, `I` integrates into the main branch.

use std::collections::BTreeSet;

use ptyro_core::SessionId;
use ptyro_core::input::{KeyCode, KeyEvent};
use ptyro_core::model::{CommitInfo, DiffAgainst, DiffDoc, DiffFile, LineKind, ReviewComment};
use ptyro_protocol::{EventRecord, Request};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Confirmed, Overlay, Pending, PromptKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Diff,
    Commits,
    Checks,
    Ports,
    Events,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Diff, "Diff"),
    (Tab::Commits, "Commits"),
    (Tab::Checks, "Checks"),
    (Tab::Ports, "Ports"),
    (Tab::Events, "Events"),
];

#[derive(Debug, Clone)]
pub struct Inspector {
    pub worktree: String,
    pub tab: Tab,
    pub against: DiffAgainst,
    pub diff: Option<DiffDoc>,
    pub commits: Option<Vec<CommitInfo>>,
    pub events: Vec<(String, EventRecord)>,
    pub comments: Vec<ReviewComment>,
    /// The selected file in the Diff tab.
    pub file: usize,
    /// The cursor row within the selected file.
    pub cursor: usize,
    pub split: bool,
    /// Files marked for the next commit.
    pub marked: BTreeSet<String>,
    pub error: Option<String>,
    /// Counts loads, so events from an older one are dropped (`r` twice).
    pub load: u64,
}

/// One displayed row of a file's diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DRow {
    Hunk(usize),
    Line(usize, usize),
}

pub fn rows(f: &DiffFile) -> Vec<DRow> {
    let mut v = Vec::new();
    for (h, hunk) in f.hunks.iter().enumerate() {
        v.push(DRow::Hunk(h));
        for l in 0..hunk.lines.len() {
            v.push(DRow::Line(h, l));
        }
    }
    v
}

impl Inspector {
    pub fn new(worktree: String) -> Self {
        Self {
            worktree,
            tab: Tab::Diff,
            against: DiffAgainst::Base,
            diff: None,
            commits: None,
            events: Vec::new(),
            comments: Vec::new(),
            file: 0,
            cursor: 0,
            split: false,
            marked: BTreeSet::new(),
            error: None,
            load: 0,
        }
    }

    fn current(&self) -> Option<&DiffFile> {
        self.diff.as_ref()?.files.get(self.file)
    }

    /// (path, line, code) under the cursor, for a comment.
    fn at_cursor(&self) -> Option<(String, u32, String)> {
        let f = self.current()?;
        match *rows(f).get(self.cursor)? {
            DRow::Line(h, l) => {
                let line = &f.hunks[h].lines[l];
                Some((f.path.clone(), line.new.or(line.old)?, line.text.clone()))
            }
            DRow::Hunk(h) => Some((f.path.clone(), f.hunks[h].new_start, String::new())),
        }
    }
}

/// Whether comment `c` is on this line: a new-file line by its number, a removed line by its
/// old number and its own text. A comment that remembers the line's code must match it, so a
/// comment on removed line 10 is not drawn (or deleted) on the added line 10.
fn comment_on(
    c: &ReviewComment,
    path: &str,
    new: Option<u32>,
    old: Option<u32>,
    text: &str,
) -> bool {
    c.path == path
        && match (new, old) {
            (Some(n), _) => c.line == n && c.code.as_deref().is_none_or(|x| x == text),
            (None, Some(o)) => c.line == o && c.code.as_deref() == Some(text),
            (None, None) => false,
        }
}

impl App {
    /// `D`: the worktree of the selected sidebar row or the focused session.
    pub fn open_inspector(&mut self) {
        let wt = match (self.focus, self.selected_row()) {
            (crate::app::Focus::Sidebar, Some(crate::tree::Row::Worktree(w))) => Some(w),
            (
                crate::app::Focus::Sidebar,
                Some(crate::tree::Row::Session(id) | crate::tree::Row::Quick(id)),
            ) => self.session(id).and_then(|s| s.worktree.clone()),
            _ => self
                .focused_session()
                .and_then(|id| self.session(id))
                .and_then(|s| s.worktree.clone()),
        };
        match wt {
            Some(w) => self.inspect(w),
            None => self.toast_offer(
                "no worktree here: the inspector shows a worktree's changes",
                false,
                crate::app::Offer {
                    label: "make one".into(),
                    then: crate::app::Then::Run(crate::keymap::Command {
                        action: "worktree.new",
                        arg: None,
                    }),
                },
            ),
        }
    }

    pub fn inspect(&mut self, worktree: String) {
        // One full-screen view at a time.
        self.orch = None;
        self.sets_view = None;
        self.inspector = Some(Inspector::new(worktree));
        self.inspector_load();
    }

    /// (Re)load what the current tab shows.
    pub fn inspector_load(&mut self) {
        let Some(i) = &mut self.inspector else { return };
        // An error belongs to the load that hit it.
        i.error = None;
        i.load += 1;
        let (addr, load) = (i.worktree.clone(), i.load);
        match i.tab {
            Tab::Diff => {
                let against = i.against;
                self.request(
                    Pending::InspectDiff { worktree: addr.clone(), against },
                    Request::WorktreeDiff { addr: addr.clone(), against },
                );
                self.request(Pending::InspectComments(addr.clone()), Request::ReviewList { addr });
            }
            Tab::Commits => {
                self.request(Pending::InspectCommits(addr.clone()), Request::WorktreeLog { addr })
            }
            Tab::Checks => {
                self.request(Pending::Done(String::new()), Request::PrGet { addr, refresh: true })
            }
            Tab::Ports => {}
            Tab::Events => {
                let ids: Vec<(SessionId, String)> = self
                    .sessions
                    .iter()
                    .filter(|s| s.worktree.as_deref() == Some(addr.as_str()))
                    .map(|s| (s.id, s.name.clone()))
                    .collect();
                if let Some(i) = &mut self.inspector {
                    i.events.clear();
                }
                for (id, name) in ids {
                    self.request(
                        Pending::InspectEvents { worktree: addr.clone(), load, name },
                        Request::Events { session: Some(id.full()), limit: Some(200) },
                    );
                }
            }
        }
    }

    pub fn inspector_key(&mut self, key: KeyEvent) {
        let Some(mut i) = self.inspector.take() else { return };
        let shift = key.mods.has(ptyro_core::input::Mods::SHIFT);
        let code = key.code;
        let ch = match code {
            KeyCode::Char { c } => Some(c),
            _ => None,
        };
        // Tabs.
        if let Some(n) = ch.and_then(|c| c.to_digit(10)).filter(|n| (1..=5).contains(n)) {
            i.tab = TABS[n as usize - 1].0;
            self.inspector = Some(i);
            self.inspector_load();
            return;
        }
        // Shift-Tab arrives as BackTab on most terminals, and as Tab with Shift on others.
        if code == KeyCode::Tab || code == KeyCode::BackTab {
            let at = TABS.iter().position(|(t, _)| *t == i.tab).unwrap_or(0);
            let back = shift || code == KeyCode::BackTab;
            i.tab = TABS[(at + if back { 4 } else { 1 }) % 5].0;
            self.inspector = Some(i);
            self.inspector_load();
            return;
        }
        match (code, ch) {
            (KeyCode::Esc, _) | (_, Some('q')) => return,
            (_, Some('r')) => {
                self.inspector = Some(i);
                self.inspector_load();
                return;
            }
            (_, Some('P')) => {
                let draft = self.config.github.draft;
                self.request(
                    Pending::Done(String::new()),
                    Request::PrCreate {
                        addr: i.worktree.clone(),
                        draft,
                        title: None,
                        body: None,
                        base: None,
                    },
                );
                self.toast("pushing and opening a PR…");
            }
            (_, Some('I')) => {
                let name = self
                    .worktree(&i.worktree)
                    .map_or_else(|| i.worktree.clone(), |w| w.qualified());
                self.overlay = Some(Overlay::Confirm {
                    text: format!("Rebase {name} onto the main branch and fast-forward it? [y/N]"),
                    action: Confirmed::Integrate(i.worktree.clone()),
                });
            }
            (_, Some('A')) => {
                let agent = self
                    .sessions
                    .iter()
                    .filter(|s| {
                        s.worktree.as_deref() == Some(i.worktree.as_str())
                            && s.agent.is_some()
                            && s.state.is_live()
                    })
                    .max_by_key(|s| s.created_ms)
                    .map(|s| (s.id, s.name.clone()));
                match agent {
                    Some((id, name)) => self.request(
                        Pending::Done(format!("asked {name} to commit (once its turn is over)")),
                        Request::SendText {
                            addr: id.full(),
                            text: "[via ptyro] Please commit your work now: stage what belongs together, and write a clear commit message (subject line under 72 characters, then why).".into(),
                            enter: true,
                            wait_ready_ms: Some(300_000),
                        },
                    ),
                    None => self.toast_offer(
                        "no agent runs in this worktree",
                        false,
                        crate::app::Offer {
                            label: "start one".into(),
                            then: crate::app::Then::Run(crate::keymap::Command {
                                action: "session.new",
                                arg: None,
                            }),
                        },
                    ),
                }
            }
            _ => {}
        }
        match i.tab {
            Tab::Diff => self.diff_key(&mut i, code, ch),
            Tab::Checks => match ch {
                Some('f') => self.request(
                    Pending::Done(String::new()),
                    Request::PrFix { addr: i.worktree.clone() },
                ),
                Some('m') => self.request(
                    Pending::Done("merges when the checks pass".into()),
                    Request::PrAutomerge { addr: i.worktree.clone(), method: None, off: false },
                ),
                Some('w') => self.request(
                    Pending::Done(
                        "watching: new failures and review comments go to the agent".into(),
                    ),
                    Request::PrWatch { addr: i.worktree.clone(), off: false },
                ),
                _ => {}
            },
            _ => {}
        }
        self.inspector = Some(i);
    }

    fn diff_key(&mut self, i: &mut Inspector, code: KeyCode, ch: Option<char>) {
        let files = i.diff.as_ref().map_or(0, |d| d.files.len());
        let n_rows = i.current().map_or(0, |f| rows(f).len());
        match (code, ch) {
            (KeyCode::Down, _) | (_, Some('j')) => {
                i.cursor = (i.cursor + 1).min(n_rows.saturating_sub(1))
            }
            (KeyCode::Up, _) | (_, Some('k')) => i.cursor = i.cursor.saturating_sub(1),
            (KeyCode::PageDown, _) | (_, Some('d')) => {
                i.cursor = (i.cursor + 20).min(n_rows.saturating_sub(1))
            }
            (KeyCode::PageUp, _) | (_, Some('u')) => i.cursor = i.cursor.saturating_sub(20),
            (_, Some('n' | ']' | 'J')) | (KeyCode::Right, _) => {
                i.file = (i.file + 1).min(files.saturating_sub(1));
                i.cursor = 0;
            }
            (_, Some('p' | '[' | 'K')) | (KeyCode::Left, _) => {
                i.file = i.file.saturating_sub(1);
                i.cursor = 0;
            }
            (_, Some('s')) => i.split = !i.split,
            (_, Some('a')) => {
                i.against = i.against.next();
                i.file = 0;
                i.cursor = 0;
                i.error = None;
                self.request(
                    Pending::InspectDiff { worktree: i.worktree.clone(), against: i.against },
                    Request::WorktreeDiff { addr: i.worktree.clone(), against: i.against },
                );
            }
            (_, Some(' ')) => {
                if let Some(f) = i.current().map(|f| f.path.clone())
                    && !i.marked.remove(&f)
                {
                    i.marked.insert(f);
                }
            }
            (_, Some('c')) | (KeyCode::Enter, _) => {
                if let Some((path, line, code)) = i.at_cursor() {
                    self.overlay = Some(Overlay::Prompt {
                        title: format!("Comment on {path}:{line}"),
                        text: String::new(),
                        kind: PromptKind::ReviewComment {
                            worktree: i.worktree.clone(),
                            path,
                            line,
                            code,
                        },
                    });
                }
            }
            (_, Some('x')) => {
                if let Some((path, line, code)) = i.at_cursor()
                    && let Some(c) = i.comments.iter().find(|c| {
                        c.path == path
                            && c.line == line
                            && c.code.as_deref().is_none_or(|x| x == code)
                    })
                {
                    self.request(
                        Pending::InspectComments(i.worktree.clone()),
                        Request::ReviewRemove { addr: i.worktree.clone(), id: Some(c.id) },
                    );
                }
            }
            (_, Some('S')) => {
                if i.comments.is_empty() {
                    self.toast("no comments yet: c on a line adds one");
                } else {
                    self.request(
                        Pending::InspectSent,
                        Request::ReviewSend { addr: i.worktree.clone(), session: None, note: None },
                    );
                }
            }
            (_, Some('E')) => {
                // `git commit` in a session: your editor, inside ptyro, as the worktree's profile.
                let Some(wt) = self.worktree(&i.worktree).cloned() else { return };
                let spec = ptyro_core::session::SpawnSpec {
                    // Stage the marked files (or everything) first, then the editor.
                    argv: if i.marked.is_empty() {
                        vec![
                            "sh".into(),
                            "-c".into(),
                            "git add --all && exec git commit --verbose".into(),
                        ]
                    } else {
                        ["sh", "-c", "git add -- \"$@\" && exec git commit --verbose", "sh"]
                            .iter()
                            .map(|x| (*x).to_owned())
                            .chain(i.marked.iter().cloned())
                            .collect()
                    },
                    cwd: wt.path.clone(),
                    worktree: Some(wt.id.clone()),
                    name: Some(format!("{}.commit", wt.name.trim_start_matches('@'))),
                    ..Default::default()
                };
                self.close_inspector_after = true;
                self.request(
                    Pending::Spawned { split: None, spec: Box::new(spec.clone()), back: false },
                    Request::Spawn { spec },
                );
            }
            (_, Some('C')) => {
                let what = if i.marked.is_empty() {
                    "everything".to_owned()
                } else {
                    format!("{} file(s)", i.marked.len())
                };
                self.overlay = Some(Overlay::Prompt {
                    title: format!("Commit {what}: message"),
                    text: String::new(),
                    kind: PromptKind::CommitMessage {
                        worktree: i.worktree.clone(),
                        paths: i.marked.iter().cloned().collect(),
                    },
                });
            }
            _ => {}
        }
    }
}

fn fg(app: &App, c: ratatui::style::Color) -> Style {
    crate::ui::fg(app, c)
}

pub fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(i) = &app.inspector else { return };
    let t = &app.theme;
    f.render_widget(Clear, area);
    let wt = app.worktree(&i.worktree);
    let title = wt.map_or_else(|| i.worktree.clone(), |w| w.qualified());
    let mut tabs: Vec<Span<'_>> =
        vec![Span::styled(format!(" {title} "), fg(app, t.accent).add_modifier(Modifier::BOLD))];
    for (n, (tab, name)) in TABS.iter().enumerate() {
        let st = if *tab == i.tab {
            fg(app, t.selection_fg).bg(app.depth.fit(t.selection_bg)).add_modifier(Modifier::BOLD)
        } else {
            fg(app, t.dim)
        };
        tabs.push(Span::styled(format!(" {} {name} ", n + 1), st));
    }
    if let Some(pr) = wt.and_then(|w| w.pr.as_ref()) {
        let (p, fl, w) = pr.tally();
        let (txt, c) = if fl > 0 {
            (format!(" PR #{} ✗{fl}", pr.number), t.error)
        } else if w > 0 {
            (format!(" PR #{} ◌{w}", pr.number), t.working)
        } else {
            (format!(" PR #{} ✓{p}", pr.number), t.done)
        };
        tabs.push(Span::styled(txt, fg(app, c)));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(app, t.border_focus))
        .title(Line::from(tabs));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 {
        return;
    }
    let body = Rect { height: inner.height - 1, ..inner };
    let foot = Rect { y: inner.bottom() - 1, height: 1, ..inner };
    let help = match i.tab {
        Tab::Diff => {
            "j/k move · n/p file · c comment · x drop · S send comments · a against · s split · Space mark · C commit · A agent commits · P PR · I integrate · q close"
        }
        Tab::Checks => {
            "f fix failing checks · m auto-merge · w watch · P open PR · r refresh · q close"
        }
        _ => "1-5 / Tab switch tabs · r refresh · q close",
    };
    f.render_widget(Paragraph::new(Line::styled(help, fg(app, t.dim))), foot);
    if let Some(e) = &i.error {
        f.render_widget(Paragraph::new(Line::styled(e.clone(), fg(app, t.error))), body);
        return;
    }
    match i.tab {
        Tab::Diff => draw_diff(app, i, f, body),
        Tab::Commits => {
            let lines: Vec<Line<'_>> = match &i.commits {
                None => vec![Line::styled("loading…", fg(app, t.dim))],
                Some(c) if c.is_empty() => {
                    vec![Line::styled("no commits beyond the base yet", fg(app, t.dim))]
                }
                Some(c) => c
                    .iter()
                    .map(|k| {
                        Line::from(vec![
                            Span::styled(
                                format!("{} ", &k.sha[..8.min(k.sha.len())]),
                                fg(app, t.accent),
                            ),
                            Span::raw(k.subject.clone()),
                            Span::styled(
                                format!(
                                    "  +{} −{} · {} · {}",
                                    k.added,
                                    k.removed,
                                    k.author,
                                    crate::ui::ago(k.at_ms)
                                ),
                                fg(app, t.dim),
                            ),
                        ])
                    })
                    .collect(),
            };
            f.render_widget(Paragraph::new(lines), body);
        }
        Tab::Checks => {
            let mut lines = Vec::new();
            match wt.and_then(|w| w.pr.as_ref()) {
                None => lines.push(Line::styled(
                    "no pull request yet: P pushes the branch and opens a draft",
                    fg(app, t.dim),
                )),
                Some(pr) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("#{} ", pr.number),
                            fg(app, t.accent).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(pr.title.clone()),
                        Span::styled(
                            format!(
                                "  {}{}{}{}",
                                pr.state.to_lowercase(),
                                if pr.draft { " · draft" } else { "" },
                                pr.review
                                    .as_deref()
                                    .map(|r| format!(" · {}", r.to_lowercase().replace('_', " ")))
                                    .unwrap_or_default(),
                                if pr.automerge { " · auto-merge" } else { "" }
                            ),
                            fg(app, t.dim),
                        ),
                    ]));
                    lines.push(Line::styled(pr.url.clone(), fg(app, t.dim)));
                    lines.push(Line::raw(""));
                    if pr.checks.is_empty() {
                        lines.push(Line::styled("no checks reported", fg(app, t.dim)));
                    }
                    for c in &pr.checks {
                        let (m, col) = match c.bucket.as_str() {
                            "pass" => ("✓", t.done),
                            "fail" | "cancel" => ("✗", t.error),
                            "pending" => ("◌", t.working),
                            _ => ("·", t.dim),
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{m} "), fg(app, col)),
                            Span::raw(c.name.clone()),
                            Span::styled(
                                c.link.as_deref().map(|l| format!("  {l}")).unwrap_or_default(),
                                fg(app, t.dim),
                            ),
                        ]));
                    }
                }
            }
            f.render_widget(Paragraph::new(lines), body);
        }
        Tab::Ports => {
            let mut lines = Vec::new();
            if let Some(b) = wt.and_then(|w| w.ports) {
                lines.push(Line::styled(
                    format!(
                        "port block {}–{} ($PTYRO_PORT)",
                        b.base,
                        b.base + b.count.saturating_sub(1)
                    ),
                    fg(app, t.dim),
                ));
            }
            for s in
                app.sessions.iter().filter(|s| s.worktree.as_deref() == Some(i.worktree.as_str()))
            {
                for p in &s.ports {
                    lines.push(Line::from(vec![
                        Span::styled(format!(":{p} "), fg(app, t.idle)),
                        Span::raw(format!("http://localhost:{p}")),
                        Span::styled(format!("  {}", s.name), fg(app, t.dim)),
                    ]));
                }
            }
            if lines.len() <= 1 {
                lines.push(Line::styled("nothing listens yet", fg(app, t.dim)));
            }
            f.render_widget(Paragraph::new(lines), body);
        }
        Tab::Events => {
            let mut ev: Vec<&(String, EventRecord)> = i.events.iter().collect();
            ev.sort_by_key(|(_, e)| e.at_ms);
            let lines: Vec<Line<'_>> = ev
                .iter()
                .rev()
                .take(body.height as usize)
                .rev()
                .map(|(who, e)| {
                    let what = match e.kind.as_str() {
                        "status" => format!(
                            "{} → {}{}",
                            e.detail["from"].as_str().unwrap_or("?"),
                            e.detail["to"].as_str().unwrap_or("?"),
                            e.detail["detail"]
                                .as_str()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default()
                        ),
                        "hook" => format!(
                            "{} {}",
                            e.detail["event"].as_str().unwrap_or(""),
                            e.detail["tool"].as_str().unwrap_or("")
                        ),
                        _ => e.detail.to_string(),
                    };
                    let col = match e.detail["to"].as_str() {
                        Some("blocked") => t.blocked,
                        Some("working") => t.working,
                        Some("idle" | "done") => t.idle,
                        Some("error") => t.error,
                        _ => t.text,
                    };
                    Line::from(vec![
                        Span::styled(format!("{:>5} ", crate::ui::ago(e.at_ms)), fg(app, t.dim)),
                        Span::styled(format!("{who:<14} "), fg(app, t.accent)),
                        Span::styled(format!("{:<9}", e.kind), fg(app, t.dim)),
                        Span::styled(what, fg(app, col)),
                    ])
                })
                .collect();
            let lines = if lines.is_empty() {
                vec![Line::styled("no events yet", fg(app, t.dim))]
            } else {
                lines
            };
            f.render_widget(Paragraph::new(lines), body);
        }
    }
}

fn draw_diff(app: &App, i: &Inspector, f: &mut Frame<'_>, area: Rect) {
    let t = &app.theme;
    let Some(doc) = &i.diff else {
        f.render_widget(Paragraph::new(Line::styled("loading…", fg(app, t.dim))), area);
        return;
    };
    if doc.files.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("no changes against {} (a switches)", i.against.label()),
                fg(app, t.dim),
            )),
            area,
        );
        return;
    }
    let list_w = (area.width / 4).clamp(18, 40);
    let list = Rect { width: list_w, ..area };
    let hunks =
        Rect { x: area.x + list_w + 1, width: area.width.saturating_sub(list_w + 1), ..area };
    // The file list.
    let mut lines: Vec<Line<'_>> = vec![Line::styled(
        format!("against {} ({})", i.against.label(), doc.files.len()),
        fg(app, t.dim),
    )];
    let start = i.file.saturating_sub(area.height.saturating_sub(3) as usize);
    for (n, file) in doc.files.iter().enumerate().skip(start) {
        let sel = n == i.file;
        let mark = if i.marked.contains(&file.path) { "◆" } else { " " };
        let st = match file.status.as_str() {
            "added" | "untracked" => t.done,
            "deleted" => t.error,
            _ => t.text,
        };
        let comments = i.comments.iter().filter(|c| c.path == file.path).count();
        let mut style = fg(app, st);
        if sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        lines.push(Line::from(vec![
            Span::styled(mark, fg(app, t.accent)),
            Span::styled(format!("{name:<w$}", w = (list_w as usize).saturating_sub(12)), style),
            Span::styled(format!(" +{}", file.added), fg(app, t.done)),
            Span::styled(format!("−{}", file.removed), fg(app, t.error)),
            Span::styled(
                if comments > 0 { format!(" 💬{comments}") } else { String::new() },
                fg(app, t.search),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), list);
    // The hunks of the selected file.
    let Some(file) = doc.files.get(i.file) else { return };
    let rs = rows(file);
    let h = hunks.height as usize;
    // The lines the rows take: the file's header, a note for a binary file, then each row and
    // the comments under it. The window is chosen so the cursor's row is always inside it.
    let room = h.saturating_sub(1 + usize::from(file.binary)).max(1);
    let height_of = |r: &DRow| -> usize {
        1 + match *r {
            DRow::Line(hk, l) => {
                let d = &file.hunks[hk].lines[l];
                i.comments
                    .iter()
                    .filter(|c| comment_on(c, &file.path, d.new, d.old, &d.text))
                    .count()
            }
            DRow::Hunk(_) => 0,
        }
    };
    let mut top = i.cursor.saturating_sub(room / 2).min(rs.len().saturating_sub(1));
    while top < i.cursor
        && rs[top..=i.cursor.min(rs.len().saturating_sub(1))].iter().map(height_of).sum::<usize>()
            > room
    {
        top += 1;
    }
    let mut out: Vec<Line<'_>> = Vec::new();
    let mut header =
        vec![Span::styled(file.path.clone(), fg(app, t.accent).add_modifier(Modifier::BOLD))];
    if let Some(o) = &file.old_path {
        header.push(Span::styled(format!(" (from {o})"), fg(app, t.dim)));
    }
    header.push(Span::styled(format!("  {}", file.status), fg(app, t.dim)));
    out.push(Line::from(header));
    if file.binary {
        out.push(Line::styled("binary file", fg(app, t.dim)));
    }
    let half = (hunks.width.saturating_sub(1) / 2) as usize;
    let mut used = 0;
    for (n, r) in rs.iter().enumerate().skip(top) {
        if used >= room {
            break;
        }
        used += height_of(r);
        let cursor = n == i.cursor;
        let line = match *r {
            DRow::Hunk(hk) => Line::styled(
                file.hunks[hk].header.clone(),
                fg(app, t.dim).add_modifier(Modifier::ITALIC),
            ),
            DRow::Line(hk, l) => {
                let d = &file.hunks[hk].lines[l];
                let (mark, col) = match d.kind {
                    LineKind::Added => ('+', t.done),
                    LineKind::Removed => ('-', t.error),
                    LineKind::Context => (' ', t.text),
                };
                let num =
                    |n: Option<u32>| n.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
                if i.split {
                    // Removed lines on the left, added on the right, context on both.
                    let text: String = d.text.chars().take(half.saturating_sub(6)).collect();
                    let cell = format!("{} {mark}{text}", num(d.old.or(d.new)));
                    let pad = format!("{cell:<half$}");
                    match d.kind {
                        LineKind::Removed => {
                            Line::from(vec![Span::styled(pad, fg(app, col)), Span::raw("│")])
                        }
                        LineKind::Added => Line::from(vec![
                            Span::raw(" ".repeat(half)),
                            Span::raw("│"),
                            Span::styled(cell, fg(app, col)),
                        ]),
                        LineKind::Context => Line::from(vec![
                            Span::styled(pad, fg(app, col)),
                            Span::raw("│"),
                            Span::styled(format!("{} {mark}{text}", num(d.new)), fg(app, col)),
                        ]),
                    }
                } else {
                    Line::from(vec![
                        Span::styled(format!("{} {} ", num(d.old), num(d.new)), fg(app, t.dim)),
                        Span::styled(format!("{mark}{}", d.text), fg(app, col)),
                    ])
                }
            }
        };
        let line =
            if cursor { line.style(Style::new().add_modifier(Modifier::REVERSED)) } else { line };
        out.push(line);
        // Comments under their line.
        if let DRow::Line(hk, l) = *r {
            let d = &file.hunks[hk].lines[l];
            let here = |c: &&ReviewComment| comment_on(c, &file.path, d.new, d.old, &d.text);
            for c in i.comments.iter().filter(here) {
                out.push(Line::styled(format!("          💬 {}", c.text), fg(app, t.search)));
            }
        }
    }
    f.render_widget(Paragraph::new(out), hunks);
}
