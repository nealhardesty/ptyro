//! Drawing (SPEC §11.1): sidebar, panes with headers, dividers, status bar and overlays.

use ptyro_core::SessionId;
use ptyro_core::config::key_label;
use ptyro_core::session::{SessionInfo, SessionKind};
use ptyro_core::status::AgentStatus;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Focus, Overlay};
use crate::keymap::{ACTIONS, Table};
use crate::layout::Dir;
use crate::paint;
use crate::tree::Row;

/// The status glyph and its colour (SPEC §11.1).
/// The status glyph and its colour (SPEC §11.1): `●` working (pulsing), `◉` blocked, `○` idle,
/// `✓` done, `✗` errored, `■` exited, `☾` dormant; a guess from the activity heuristic is `◌`.
pub fn glyph(app: &App, s: &SessionInfo) -> (&'static str, Color) {
    use ptyro_core::session::ProcessState;
    use ptyro_core::status::{AgentStatus, Confidence};
    let t = &app.theme;
    if s.state == ProcessState::Dormant {
        return ("☾", t.dim);
    }
    if !s.state.is_live() {
        return ("■", t.exited);
    }
    let Some(r) = &s.status else { return ("•", t.dim) };
    let color = match r.status {
        AgentStatus::Working => t.working,
        AgentStatus::Blocked => t.blocked,
        AgentStatus::Done => t.done,
        AgentStatus::Errored => t.error,
        AgentStatus::Idle => t.idle,
        AgentStatus::Starting | AgentStatus::Unknown => t.dim,
    };
    if r.confidence == Confidence::Low {
        return ("◌", color);
    }
    let g = match r.status {
        // A slow pulse while working.
        AgentStatus::Working => {
            if (ptyro_core::now_ms() / 500).is_multiple_of(2) {
                "●"
            } else {
                "◍"
            }
        }
        AgentStatus::Blocked => "◉",
        AgentStatus::Done => "✓",
        AgentStatus::Errored => "✗",
        AgentStatus::Idle => "○",
        AgentStatus::Starting | AgentStatus::Unknown => "•",
    };
    (g, color)
}

/// `▕███░░▏ 42%`
fn progress_bar(p: u8) -> String {
    let filled = (usize::from(p.min(100)) * 6).div_ceil(100);
    format!("▕{}{}▏{p}%", "█".repeat(filled), "░".repeat(6 - filled))
}

fn what(s: &SessionInfo) -> String {
    if let Some(role) = &s.script {
        return role.trim_start_matches("run:").to_owned();
    }
    s.foreground.clone().unwrap_or_else(|| {
        s.argv.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_owned()).unwrap_or_default()
    })
}

pub(crate) fn fg(app: &App, c: Color) -> Style {
    Style::new().fg(app.depth.fit(c))
}

pub fn draw(app: &App, f: &mut Frame<'_>) {
    let area = f.area();
    if app.opts.raw {
        draw_raw(app, f, area);
    } else {
        if let Some(side) = app.sidebar_area().filter(|_| !app.sidebar_zoom) {
            draw_sidebar(app, f, side);
        }
        draw_panes(app, f);
        if let Some(side) = app.sidebar_area().filter(|_| app.sidebar_zoom) {
            f.render_widget(Clear, Rect { width: side.width + 1, ..side });
            draw_sidebar(app, f, side);
        }
        if app.inspector.is_some() {
            let h =
                if app.config.ui.status_bar { area.height.saturating_sub(1) } else { area.height };
            crate::inspector::draw(app, f, Rect { height: h, ..area });
        }
        if app.orch.is_some() {
            let h =
                if app.config.ui.status_bar { area.height.saturating_sub(1) } else { area.height };
            crate::runs::draw(app, f, Rect { height: h, ..area });
        }
        if app.sets_view.is_some() {
            let h =
                if app.config.ui.status_bar { area.height.saturating_sub(1) } else { area.height };
            crate::sets::draw(app, f, Rect { height: h, ..area });
        }
        if app.config.ui.status_bar && area.height > 2 {
            draw_status(app, f, Rect { y: area.bottom() - 1, height: 1, ..area });
        }
    }
    if let Some(o) = &app.overlay {
        draw_overlay(app, f, o, area);
    }
}

fn draw_raw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(id) = app.focused_session() else { return };
    let Some(cache) = app.caches.get(&id) else { return };
    let exited = app.exit_state(id).is_some();
    if let Some(c) = app.panes.get(&app.focused).and_then(|p| p.copy.as_ref()) {
        paint::copy(c, f.buffer_mut(), area, &app.theme, app.depth);
        return;
    }
    paint::screen(cache, f.buffer_mut(), area, app.depth, exited);
    if app.overlay.is_none()
        && !app.prefix
        && !exited
        && let Some(p) = paint::cursor(cache, area)
    {
        f.set_cursor_position(p);
    }
}

/// A profile's colour, if it has one (SPEC §10.10 visibility).
pub fn profile_color(app: &App, profile: &str) -> Option<Color> {
    let c = app.profile(profile)?.color.as_deref()?;
    match ptyro_core::config::parse_color(c).ok()? {
        ptyro_core::config::ConfigColor::Default => None,
        ptyro_core::config::ConfigColor::Indexed(i) => Some(Color::Indexed(i)),
        ptyro_core::config::ConfigColor::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// `[work]` in the profile's colour; nothing for an implicit `default`.
fn profile_badge(app: &App, profile: &str) -> Option<Span<'static>> {
    let info = app.profile(profile);
    if profile == ptyro_core::model::DEFAULT_PROFILE
        && !info.is_some_and(|p| p.defined)
        && app.profiles.len() <= 1
    {
        return None;
    }
    let label = info.map_or_else(|| profile.to_owned(), |p| p.label.clone());
    let style = match profile_color(app, profile) {
        Some(c) if !app.theme.monochrome => {
            Style::new().fg(app.depth.fit(c)).add_modifier(Modifier::BOLD)
        }
        _ => Style::new().add_modifier(Modifier::BOLD),
    };
    Some(Span::styled(format!("[{label}]"), style))
}

fn diffstat(app: &App, w: &ptyro_core::model::WorktreeInfo) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mut v = Vec::new();
    if let Some(s) = &w.status {
        if s.insertions > 0 {
            v.push(Span::styled(format!(" +{}", s.insertions), fg(app, t.done)));
        }
        if s.deletions > 0 {
            v.push(Span::styled(format!(" −{}", s.deletions), fg(app, t.error)));
        }
        if s.dirty() > 0 {
            v.push(Span::styled(" ●", fg(app, t.working)));
        }
    }
    v
}

fn draw_sidebar(app: &App, f: &mut Frame<'_>, area: Rect) {
    let t = &app.theme;
    let mut title = vec![
        Span::styled(" WORKSPACE ", fg(app, t.dim)),
        Span::styled(app.workspace.clone(), fg(app, t.accent).add_modifier(Modifier::BOLD)),
    ];
    if let Some(b) = profile_badge(app, &app.workspace_profile()) {
        title.push(Span::raw(" "));
        title.push(b);
    }
    // The workspace profile's tightest rate-limit window (M4.17).
    if let Some(w) = app
        .usage_for(&app.workspace_profile())
        .into_iter()
        .flat_map(|u| u.windows.iter())
        .max_by_key(|w| w.used_percent)
    {
        let hot = w.used_percent >= 80;
        title.push(Span::styled(
            format!(" {} {}%", w.name, w.used_percent),
            fg(app, if hot { t.blocked } else { t.dim }),
        ));
    }
    let body = sidebar_body(app, area);
    let mut lines = vec![Line::from(title)];
    lines.extend(body.lines.into_iter().skip(body.skip).map(|(_, l)| l));
    if body.empty {
        let prefix = key_label(&app.keymap.prefix);
        lines.push(Line::styled(format!("  none yet: {prefix} n"), fg(app, t.dim)));
        if app.projects.is_empty() {
            lines.push(Line::styled(
                format!("  add a repo: {prefix} : project.add"),
                fg(app, t.dim),
            ));
        }
    }
    let mut hint = if app.sidebar_zoom {
        " everything at once · ⏎ open · p peek · z or Esc back".to_string()
    } else if app.focus == Focus::Sidebar {
        " ⏎ open/fold · n new · x stop · Esc".to_string()
    } else {
        format!(" {} Tab: sidebar · w workspaces", key_label(&app.keymap.prefix))
    };
    truncate_width(&mut hint, area.width as usize);
    f.render_widget(Paragraph::new(lines), Rect { height: area.height.saturating_sub(1), ..area });
    f.render_widget(
        Paragraph::new(Line::styled(hint, fg(app, t.dim))),
        Rect { y: area.bottom().saturating_sub(1), height: 1, ..area },
    );
    // The divider between sidebar and panes.
    let div = Rect { x: area.right(), width: 1, ..area };
    let col = if app.focus == Focus::Sidebar { t.border_focus } else { t.border };
    for y in div.top()..div.bottom() {
        if let Some(c) = f.buffer_mut().cell_mut(Position::new(div.x, y)) {
            c.set_symbol("│").set_style(fg(app, col));
        }
    }
}

/// The sidebar's tree as screen lines, each with the index of its row in `app.rows()` (a
/// session row may take several, UX1.2), and how many lines scroll off the top to keep the
/// selection in view. Drawing and clicking share it.
pub(crate) struct SidebarBody {
    pub lines: Vec<(usize, Line<'static>)>,
    pub skip: usize,
    pub empty: bool,
}

pub(crate) fn sidebar_body(app: &App, area: Rect) -> SidebarBody {
    let t = &app.theme;
    let shown: Vec<SessionId> = app.panes.values().filter_map(|p| p.session).collect();
    let focused = app.focused_session();
    // Built once per frame; numbers and sessions looked up by id.
    let rows = app.rows();
    let sel = app.sel_index(&rows);
    let numbered: std::collections::HashMap<SessionId, usize> =
        App::numbered_from(&rows).into_iter().enumerate().map(|(n, id)| (id, n)).collect();
    let sessions: std::collections::HashMap<SessionId, &SessionInfo> =
        app.sessions.iter().map(|s| (s.id, s)).collect();
    let width = area.width as usize;
    // Less the title and the hint line.
    let room = area.height.saturating_sub(2) as usize;
    let mut firsts: Vec<(usize, Line<'static>)> = Vec::new();
    // Each session row's detail lines, as wide (the row in focus) and as narrow.
    let mut details: Vec<Vec<Line<'static>>> = Vec::new();
    let mut close: Vec<bool> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut detail = Vec::new();
        let mut near = false;
        let line = match row {
            Row::Project(id) => {
                let Some(p) = app.project(id) else { continue };
                let arrow = if app.collapsed.contains(id) { "▸" } else { "▾" };
                let mut spans = vec![
                    Span::styled(format!(" {arrow} "), fg(app, t.dim)),
                    Span::styled(p.slug.clone(), fg(app, t.text).add_modifier(Modifier::BOLD)),
                ];
                if let Some(pin) = &p.pin
                    && let Some(b) = profile_badge(app, pin)
                {
                    spans.push(Span::raw(" "));
                    spans.push(b);
                }
                Line::from(spans)
            }
            Row::Worktree(id) => {
                let Some(w) = app.worktree(id) else { continue };
                let arrow = if app.collapsed.contains(id) { "▸" } else { "▾" };
                let name_style = match w.state {
                    ptyro_core::model::WorktreeState::Ready => fg(app, t.text),
                    _ => fg(app, t.exited),
                };
                let mut spans = vec![
                    Span::styled(format!("   {arrow} "), fg(app, t.dim)),
                    Span::styled(w.name.clone(), name_style),
                ];
                match (&w.branch, w.is_root) {
                    (Some(b), true) => spans.push(Span::styled(format!(" ({b})"), fg(app, t.dim))),
                    (Some(b), false) if *b != format!("ptyro/{}", w.name) => {
                        spans.push(Span::styled(format!(" {b}"), fg(app, t.dim)))
                    }
                    _ => {}
                }
                if w.state != ptyro_core::model::WorktreeState::Ready {
                    spans.push(Span::styled(format!(" {}", w.state.label()), fg(app, t.error)));
                }
                spans.extend(diffstat(app, w));
                // Its PR and checks (M6.5).
                if let Some(pr) = &w.pr {
                    let (pass, fail, pending) = pr.tally();
                    let (mark, c) = if pr.state == "MERGED" {
                        ("merged".to_owned(), t.done)
                    } else if fail > 0 {
                        (format!("✗{fail}"), t.error)
                    } else if pending > 0 {
                        ("◌".to_owned(), t.working)
                    } else if pass > 0 {
                        ("✓".to_owned(), t.done)
                    } else {
                        (String::new(), t.dim)
                    };
                    spans.push(Span::styled(
                        format!(" #{}{}{mark}", pr.number, if mark.is_empty() { "" } else { " " }),
                        fg(app, c),
                    ));
                }
                Line::from(spans)
            }
            Row::Session(id) => {
                let Some(s) = sessions.get(id).copied() else { continue };
                let (g, gc) = glyph(app, s);
                let num = match numbered.get(id) {
                    Some(n) if *n < 9 => format!("{} ", n + 1),
                    _ => "  ".into(),
                };
                let indent = if s.worktree.is_some()
                    && app.worktree(s.worktree.as_deref().unwrap_or("")).is_some()
                {
                    "     "
                } else {
                    " "
                };
                let mut name_style = fg(app, if s.state.is_live() { t.text } else { t.exited });
                if Some(s.id) == focused {
                    name_style =
                        name_style.add_modifier(Modifier::BOLD).fg(app.depth.fit(t.accent));
                } else if shown.contains(&s.id) {
                    name_style = name_style.add_modifier(Modifier::BOLD);
                }
                let marked = app.marked.contains(&s.id);
                let mut spans = vec![
                    Span::styled(indent, Style::new()),
                    if marked {
                        Span::styled("◆ ", fg(app, t.accent).add_modifier(Modifier::BOLD))
                    } else {
                        Span::styled(num, fg(app, t.dim))
                    },
                    Span::styled(format!("{g} "), fg(app, gc)),
                    // Under its worktree, `brisk-rook.setup` is just `setup`.
                    Span::styled(
                        s.worktree
                            .as_deref()
                            .and_then(|w| app.worktree(w))
                            .and_then(|w| {
                                s.name.strip_prefix(&format!("{}.", w.name.trim_start_matches('@')))
                            })
                            .unwrap_or(&s.name)
                            .to_owned(),
                        name_style,
                    ),
                ];
                if s.script.is_none() && s.agent.as_deref() != Some(s.name.as_str()) {
                    spans.push(Span::styled(format!(" {}", what(s)), fg(app, t.dim)));
                }
                if let Some(r) = &s.status
                    && r.status == ptyro_core::status::AgentStatus::Blocked
                {
                    spans.push(Span::styled(
                        format!(" ⚠ {}", r.kind.as_deref().unwrap_or("needs you")),
                        fg(app, t.blocked),
                    ));
                }
                // How long it has been working or waiting for you.
                if let Some(r) = s.status.as_ref().filter(|r| {
                    s.state.is_live()
                        && matches!(r.status, AgentStatus::Working | AgentStatus::Blocked)
                        && ptyro_core::now_ms().saturating_sub(r.since_ms) >= 10_000
                }) {
                    spans.push(Span::styled(format!(" {}", ago(r.since_ms)), fg(app, t.dim)));
                }
                if s.attention.is_some()
                    && s.status
                        .as_ref()
                        .is_none_or(|r| r.status != ptyro_core::status::AgentStatus::Blocked)
                {
                    spans.push(Span::styled(" ⚑", fg(app, t.search).add_modifier(Modifier::BOLD)));
                }
                let running = s.children.iter().filter(|c| c.running).count();
                if running > 0 {
                    spans.push(Span::styled(format!(" +{running}"), fg(app, t.dim)));
                }
                // Verified-done (M7.9): done and proven (✔), being checked, sent back, or only
                // claimed (✘: the check still fails).
                if let Some(v) = &s.verify {
                    use ptyro_core::session::VerifyState as V;
                    let (mark, c) = match v.state {
                        V::Passed => (" ✔".to_owned(), t.done),
                        V::Checking => (" ◌".to_owned(), t.working),
                        V::Bounced => (format!(" ↩{}", v.bounces), t.blocked),
                        V::Failed => (" ✘".to_owned(), t.error),
                        V::Pending => (String::new(), t.dim),
                    };
                    if !mark.is_empty() {
                        spans.push(Span::styled(mark, fg(app, c)));
                    }
                }
                if s.dangerous {
                    spans.push(Span::styled(" ⚠", fg(app, t.error)));
                }
                if app.config.ui.show_ports && !s.ports.is_empty() {
                    let ports: Vec<String> =
                        s.ports.iter().take(3).map(|p| format!(":{p}")).collect();
                    spans.push(Span::styled(format!(" {}", ports.join(" ")), fg(app, t.idle)));
                }
                // A session under another profile than its place's stands out.
                let place_profile = s
                    .worktree
                    .as_deref()
                    .and_then(|w| app.worktree(w))
                    .and_then(|w| w.profile.clone());
                if place_profile.as_deref().is_some_and(|p| p != s.profile)
                    && let Some(b) = profile_badge(app, &s.profile)
                {
                    spans.push(Span::raw(" "));
                    spans.push(b);
                }
                // In focus, or wanting you: always opened up, with room to wrap.
                near = Some(s.id) == focused
                    || (app.focus == Focus::Sidebar && i == sel)
                    || s.attention.is_some()
                    || s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Blocked);
                let lead = indent.len() + 4;
                detail = session_details(app, s, lead, width, near || app.sidebar_zoom);
                Line::from(spans)
            }
            Row::Group(label) => {
                Line::styled(format!(" {label}"), fg(app, t.dim).add_modifier(Modifier::ITALIC))
            }
            // A change set (M11.11): its state and readiness; amber when it wants you.
            Row::Set(run) => {
                let Some(r) = app.runs.iter().find(|r| r.state.id == *run) else { continue };
                let Some(c) = &r.state.changeset else { continue };
                let (label, attention) = app.set_label(r);
                let arrow = if app.collapsed.contains(run) { "▸" } else { "▾" };
                Line::from(vec![
                    Span::styled(format!(" {arrow} "), fg(app, t.dim)),
                    Span::styled("◆ ", fg(app, if attention { t.blocked } else { t.accent })),
                    Span::styled(c.name.clone(), fg(app, t.text).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!(" {label}"),
                        fg(app, if attention { t.blocked } else { t.dim }),
                    ),
                ])
            }
        };
        firsts.push((i, line));
        details.push(detail);
        close.push(near);
    }
    use ptyro_core::config::SidebarDetail;
    let all: usize = firsts.len() + details.iter().map(Vec::len).sum::<usize>();
    let open_all = app.sidebar_zoom
        || match app.config.ui.sidebar_detail {
            SidebarDetail::Full => true,
            SidebarDetail::Compact => false,
            SidebarDetail::Auto => all <= room,
        };
    let compact = app.config.ui.sidebar_detail == SidebarDetail::Compact && !app.sidebar_zoom;
    let mut lines = Vec::new();
    let (mut sel_first, mut sel_last) = (0, 0);
    for (((i, first), detail), near) in firsts.into_iter().zip(details).zip(close) {
        if i == sel {
            sel_first = lines.len();
        }
        lines.push((i, first));
        if !compact && (open_all || near) {
            lines.extend(detail.into_iter().map(|l| (i, l)));
        }
        if i == sel {
            sel_last = lines.len().saturating_sub(1);
        }
    }
    if app.focus == Focus::Sidebar {
        let style = if t.monochrome {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().bg(app.depth.fit(t.selection_bg))
        };
        for (i, l) in &mut lines {
            if *i == sel {
                *l = std::mem::take(l).style(style);
            }
        }
    }
    // Keep the selection in view in a long tree: all of it, else its first line.
    let skip = if app.focus == Focus::Sidebar && sel_last >= room {
        (sel_last + 1 - room).min(sel_first)
    } else {
        0
    };
    SidebarBody { empty: rows.is_empty(), lines, skip }
}

/// The lines under a session's first line (UX1.2), starting at column `lead`: what it wants,
/// what it's on, its task and last reply, or a shell's command. `wide` lets the long ones wrap
/// once, for the row in focus.
fn session_details(
    app: &App,
    s: &SessionInfo,
    lead: usize,
    width: usize,
    wide: bool,
) -> Vec<Line<'static>> {
    use ptyro_core::session::ProcessState;
    let t = &app.theme;
    let key = |action: &str| {
        let k = app.keymap.key_for(action).unwrap_or_default();
        // In the sidebar its keys need no prefix.
        if app.focus == Focus::Sidebar { k.rsplit(' ').next().unwrap_or(&k).to_owned() } else { k }
    };
    let mut items: Vec<(&str, String, Color)> = Vec::new();
    let live = s.state.is_live();
    if s.state == ProcessState::Dormant {
        items.push(("☾", "dormant · ⏎ resumes it".into(), t.dim));
    } else if !live {
        let when = s.ended_ms.map(|m| format!(" {} ago", ago(m))).unwrap_or_default();
        items.push(("■", format!("{}{when} · ⏎ restart", s.state.label()), t.exited));
    }
    if live && let Some(r) = &s.status {
        match r.status {
            AgentStatus::Blocked => {
                let want = r
                    .detail
                    .clone()
                    .or_else(|| s.attention.as_ref().map(|a| a.text.clone()))
                    .or_else(|| r.kind.clone())
                    .unwrap_or_else(|| "you".into());
                items.push(("wants", want, t.blocked));
                let answer =
                    if r.kind.as_deref() == Some("permission") { "y/a/n answers" } else { "reply" };
                items.push(("", format!("{} {answer} · ⏎ open", key("peek")), t.dim));
            }
            AgentStatus::Errored => {
                let why = r.detail.clone().or_else(|| r.kind.clone()).unwrap_or_default();
                items.push(("✗", why, t.error));
            }
            AgentStatus::Working if s.kind != SessionKind::Shell => {
                let now = s.activity.clone().or_else(|| r.detail.clone());
                items.push(("now", now.unwrap_or_else(|| "working".into()), t.working));
            }
            _ => {}
        }
    }
    if let Some(task) = &s.task {
        items.push(("task", task.clone(), t.text));
    }
    let working = s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Working);
    if let Some(reply) = s.last_reply.as_ref().filter(|_| !working) {
        items.push(("last", reply.clone(), t.dim));
    }
    if s.kind == SessionKind::Shell
        && let Some(c) = &s.command
    {
        let text = c.text.clone().or_else(|| s.foreground.clone()).unwrap_or_default();
        match c.ended_ms {
            None if live => {
                let took = dur(ptyro_core::now_ms().saturating_sub(c.started_ms));
                items.push(("now", format!("{text} · {took}"), t.working));
            }
            None => {}
            Some(end) => {
                let (mark, color) = match c.exit {
                    Some(0) | None => ("✓".to_owned(), t.done),
                    Some(e) => (format!("✗{e}"), t.error),
                };
                let took = dur(end.saturating_sub(c.started_ms));
                items.push(("last", format!("{text} {mark} · {took} · {} ago", ago(end)), color));
            }
        }
    }
    let mut out = Vec::new();
    let pad = " ".repeat(lead);
    for (label, text, color) in items {
        let label = if label.is_empty() { String::new() } else { format!("{label} ") };
        let room = width.saturating_sub(lead + label.chars().count()).max(4);
        let mut rest = text;
        for n in 0..if wide { 2 } else { 1 } {
            let mut part = rest.clone();
            truncate_width(&mut part, room);
            let more = part.len() < rest.len();
            // The last line we may use: cut with an ellipsis.
            if more && (n == 1 || !wide) {
                truncate_width(&mut part, room.saturating_sub(1));
                part.push('…');
            }
            let head = if n == 0 { label.clone() } else { " ".repeat(label.chars().count()) };
            out.push(Line::from(vec![
                Span::raw(pad.clone()),
                Span::styled(head, fg(app, t.dim)),
                Span::styled(part.clone(), fg(app, color)),
            ]));
            if !more || part.ends_with('…') {
                break;
            }
            rest = rest[part.len()..].trim_start().to_owned();
        }
    }
    out
}

fn draw_panes(app: &App, f: &mut Frame<'_>) {
    let t = &app.theme;
    let tint = app
        .config
        .ui
        .tint_borders
        .then(|| {
            app.focused_session()
                .and_then(|id| app.session(id))
                .and_then(|s| profile_color(app, &s.profile))
        })
        .flatten();
    if !app.zoom {
        for d in app.layout.dividers(app.main_area()) {
            let symbol = match d.dir {
                Dir::Right => "│",
                Dir::Down => "─",
            };
            for y in d.rect.top()..d.rect.bottom() {
                for x in d.rect.left()..d.rect.right() {
                    if let Some(c) = f.buffer_mut().cell_mut(Position::new(x, y)) {
                        c.set_symbol(symbol).set_style(fg(app, tint.unwrap_or(t.border)));
                    }
                }
            }
        }
    }
    let mut cursor = None;
    for (pane, rect) in app.pane_rects() {
        let focused = pane == app.focused && app.focus == Focus::Pane;
        let state = app.panes.get(&pane);
        let session = state.and_then(|p| p.session);
        let info = session.and_then(|id| app.session(id));
        let inner = app.inner(rect);
        draw_header(app, f, Rect { height: 1, ..rect }, inner, info, pane, focused);
        let Some(id) = session else {
            let lines = empty_pane(app);
            let h = (lines.len() as u16).min(inner.height);
            let r = Rect { y: inner.y + inner.height.saturating_sub(h) / 2, height: h, ..inner };
            f.render_widget(Paragraph::new(lines).centered(), r);
            continue;
        };
        if let Some(c) = state.and_then(|p| p.copy.as_ref()) {
            paint::copy(c, f.buffer_mut(), inner, t, app.depth);
            continue;
        }
        if app.chat_shown(id) {
            let empty = crate::chat::ChatView::default();
            let view = app.chats.get(&id).unwrap_or(&empty);
            let c = crate::chat::draw(
                view,
                f.buffer_mut(),
                inner,
                &app.theme,
                app.depth,
                focused && app.overlay.is_none() && !app.prefix,
            );
            if c.is_some() {
                cursor = c;
            }
            continue;
        }
        let Some(cache) = app.caches.get(&id) else { continue };
        let exited = app.exit_state(id);
        paint::screen(cache, f.buffer_mut(), inner, app.depth, exited.is_some());
        if let Some(state) = &exited {
            let action = if *state == ptyro_core::session::ProcessState::Dormant {
                "resume"
            } else {
                "restart"
            };
            let text = format!(
                " {} {} — ⏎ {action} · {} x remove ",
                info.map_or("", |i| i.name.as_str()),
                state.label(),
                key_label(&app.keymap.prefix)
            );
            let bar = Rect { y: inner.bottom().saturating_sub(1), height: 1, ..inner };
            f.render_widget(
                Paragraph::new(Line::styled(text, fg(app, t.bar_fg).bg(app.depth.fit(t.bar_bg)))),
                bar,
            );
        } else if state.is_some_and(|p| p.loading) {
            let bar = Rect { y: inner.y, height: 1, ..inner };
            f.render_widget(
                Paragraph::new(
                    Line::styled(" loading scrollback… ", fg(app, t.accent)).right_aligned(),
                ),
                bar,
            );
        } else if focused && app.overlay.is_none() && !app.prefix {
            cursor = paint::cursor(cache, inner);
        }
    }
    if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
}

/// What an empty pane offers (UX1.23): the next thing to do, with the keys that do it.
fn empty_pane(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let key = |action: &str| app.keymap.key_for(action);
    let mut lines = vec![
        Line::styled(
            format!("Nothing running in {} yet", app.workspace),
            fg(app, t.text).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    let mut offer = |k: Option<String>, what: &str| {
        if let Some(k) = k {
            lines.push(Line::from(vec![
                Span::styled(format!("{k:>9}  "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{what:<34}"), fg(app, t.dim)),
            ]));
        }
    };
    offer(key("session.new"), "start an agent or a shell");
    if !app.tree().project_ids().is_empty() {
        offer(key("worktree.new"), "a new worktree with an agent");
    }
    if app.sessions.iter().any(|s| app.home_of(s) != app.workspace) {
        offer(key("session.find"), "a session from another workspace");
    }
    if app.layout.panes().len() > 1 {
        offer(key("pane.close"), "close this pane");
    }
    offer(key("help"), "every key");
    lines
}

fn draw_header(
    app: &App,
    f: &mut Frame<'_>,
    r: Rect,
    inner: Rect,
    info: Option<&SessionInfo>,
    pane: crate::layout::PaneId,
    focused: bool,
) {
    let t = &app.theme;
    let base = if focused {
        Style::new().fg(app.depth.fit(t.bar_fg)).bg(app.depth.fit(t.bar_bg))
    } else {
        Style::new().fg(app.depth.fit(t.dim))
    };
    let mut left: Vec<Span<'_>> = Vec::new();
    let mut right: Vec<Span<'_>> = Vec::new();
    match info {
        Some(s) => {
            if app.config.ui.tint_borders
                && let Some(c) = profile_color(app, &s.profile)
            {
                left.push(Span::styled("▌", base.fg(app.depth.fit(c))));
            }
            let (g, gc) = glyph(app, s);
            left.push(Span::styled(format!(" {g} "), base.fg(app.depth.fit(gc))));
            let name = if focused {
                base.fg(app.depth.fit(t.accent)).add_modifier(Modifier::BOLD)
            } else {
                base.add_modifier(Modifier::BOLD)
            };
            left.push(Span::styled(s.name.clone(), name));
            left.push(Span::styled(format!(" · {}", what(s)), base));
            if let Some(b) = profile_badge(app, &s.profile) {
                left.push(Span::styled(" · ", base));
                left.push(Span { style: base.patch(b.style), ..b });
            }
            if let Some(w) = s.worktree.as_deref().and_then(|w| app.worktree(w)) {
                let branch = w.branch.clone().unwrap_or_else(|| "(detached)".into());
                left.push(Span::styled(format!(" · {branch}"), base));
                for d in diffstat(app, w) {
                    left.push(Span { style: base.patch(d.style), ..d });
                }
            }
            if app.config.ui.show_ports && !s.ports.is_empty() {
                let ports: Vec<String> = s.ports.iter().map(|p| format!(":{p}")).collect();
                left.push(Span::styled(format!(" · {}", ports.join(" ")), base));
            }
            if let Some(r) = &s.status {
                // What it's doing ("Bash: cargo test"), else what the status says.
                let kind = r.kind.as_deref().map(|k| format!("/{k}")).unwrap_or_default();
                let what = s
                    .activity
                    .clone()
                    .or_else(|| r.detail.clone())
                    .map(|m| format!(" {m}"))
                    .unwrap_or_default();
                left.push(Span::styled(
                    format!(" · {}{kind}{what}", r.status.label()),
                    base.fg(app.depth.fit(gc)),
                ));
                let progress = r.progress.or_else(|| s.progress.and_then(|p| p.percent));
                if let Some(p) = progress {
                    left.push(Span::styled(
                        format!(" {}", progress_bar(p)),
                        base.fg(app.depth.fit(gc)),
                    ));
                }
            } else if let Some(p) = s.progress.and_then(|p| p.percent) {
                left.push(Span::styled(format!(" · {}", progress_bar(p)), base));
            }
            if s.dangerous {
                left.push(Span::styled(
                    " · ⚠ no permission prompts",
                    base.fg(app.depth.fit(t.error)),
                ));
            }
            if let Some(cache) = app.caches.get(&s.id)
                && let Some(title) =
                    cache.title.as_deref().filter(|t| !t.is_empty() && *t != s.name)
            {
                left.push(Span::styled(format!(" · {title}"), base.fg(app.depth.fit(t.dim))));
            }
            if let Some(cache) = app.caches.get(&s.id) {
                let (w, h) = (cache.size.cols, cache.size.rows);
                if w > inner.width || h > inner.height {
                    right.push(Span::styled(
                        format!(" viewing at {w}x{h} "),
                        Style::new().black().on_cyan(),
                    ));
                } else if !app.is_controller(s.id) && s.state.is_live() {
                    right.push(Span::styled(" sized by another client ", base));
                }
            }
        }
        None => left.push(Span::styled(" (empty pane)", base)),
    }
    if let Some(c) = app.panes.get(&pane).and_then(|p| p.copy.as_ref()) {
        let note = c.note.as_deref().map(|n| format!(" {n}")).unwrap_or_default();
        let input = c
            .input
            .as_ref()
            .map(|(fwd, q)| format!(" {}{q}", if *fwd { "/" } else { "?" }))
            .unwrap_or_default();
        right.push(Span::styled(
            format!(" COPY {}{note}{input} ", c.position()),
            Style::new().black().bg(app.depth.fit(t.search)),
        ));
    }
    if app.zoom && pane == app.focused {
        right.push(Span::styled(" ZOOM ", base.add_modifier(Modifier::REVERSED)));
    }
    if app.opts.view_only {
        right.push(Span::styled(" VIEW ONLY ", base.add_modifier(Modifier::REVERSED)));
    }
    let used: usize = right.iter().map(Span::width).sum();
    let left_w = (r.width as usize).saturating_sub(used);
    f.render_widget(
        Paragraph::new(Line::from(left)).style(base),
        Rect { width: left_w as u16, ..r },
    );
    f.render_widget(
        Paragraph::new(Line::from(right)).style(base).right_aligned(),
        Rect { x: r.x + left_w as u16, width: r.width.saturating_sub(left_w as u16), ..r },
    );
}

fn draw_status(app: &App, f: &mut Frame<'_>, r: Rect) {
    let t = &app.theme;
    let bar = Style::new().fg(app.depth.fit(t.bar_fg)).bg(app.depth.fit(t.bar_bg));
    let badge = |s: &str, c: Color| {
        Span::styled(
            format!(" {s} "),
            Style::new().black().bg(app.depth.fit(c)).add_modifier(Modifier::BOLD),
        )
    };
    let mode = if app.lost_since.is_some() {
        badge("RECONNECTING", t.blocked)
    } else if app.prefix {
        badge("PREFIX", t.search)
    } else if app.panes.get(&app.focused).is_some_and(|p| p.copy.is_some()) {
        badge("COPY", t.search)
    } else if app.focus == Focus::Sidebar {
        badge("SIDEBAR", t.accent)
    } else if app.opts.view_only {
        badge("VIEW ONLY", t.idle)
    } else {
        Span::styled(" ptyro ", bar.add_modifier(Modifier::BOLD))
    };
    let mut left = vec![mode, Span::raw(" ")];
    if let Some(toast) = &app.toast {
        left.push(Span::styled(
            toast.text.clone(),
            if toast.error {
                bar.fg(app.depth.fit(t.error))
            } else {
                bar.fg(app.depth.fit(t.accent))
            },
        ));
    } else if app.prefix {
        left.push(Span::styled(prefix_hint(app), bar));
    }
    let (live, working, blocked) = app.counts();
    let mut right = Vec::new();
    if !app.workspaces.is_empty()
        && (app.workspaces.len() > 1 || app.workspace != ptyro_core::model::MAIN_WORKSPACE)
    {
        right.push(Span::styled(format!("{} ", app.workspace), bar.add_modifier(Modifier::BOLD)));
    }
    if let Some(p) = app.focused_session().and_then(|id| app.session(id)).map(|s| s.profile.clone())
        && let Some(b) = profile_badge(app, &p)
    {
        right.push(Span { style: bar.patch(b.style), ..b });
        right.push(Span::styled(" ", bar));
    }
    // Rate limits for the focused session's profile (M4.17).
    if let Some(s) = app.focused_session().and_then(|id| app.session(id)) {
        for u in app.usage_for(&s.profile) {
            if let Some(until) = u.limited_until_ms.filter(|t| *t > ptyro_core::now_ms()) {
                let mins = (until - ptyro_core::now_ms()) / 60_000;
                right.push(Span::styled(
                    format!("{} limited {}h{:02}m ", u.agent, mins / 60, mins % 60),
                    bar.fg(app.depth.fit(t.error)),
                ));
                continue;
            }
            let w: Vec<String> =
                u.windows.iter().map(|w| format!("{} {}%", w.name, w.used_percent)).collect();
            if !w.is_empty() {
                let hot = u.windows.iter().any(|w| w.used_percent >= 80);
                right.push(Span::styled(
                    format!("{} ", w.join(" ")),
                    if hot {
                        bar.fg(app.depth.fit(t.blocked))
                    } else {
                        bar.fg(app.depth.fit(t.dim))
                    },
                ));
            }
        }
    }
    for (ws, n) in app.other_workspace_attention() {
        right.push(Span::styled(format!("{ws} ⚑{n} "), bar.fg(app.depth.fit(t.search))));
    }
    let gates = app.open_gates();
    if gates > 0 {
        right.push(Span::styled(
            format!("⏸ {gates} gate(s) "),
            bar.fg(app.depth.fit(t.blocked)).add_modifier(Modifier::BOLD),
        ));
    }
    let unread = app.unread();
    if unread > 0 {
        right.push(Span::styled(
            format!("✉ {unread} "),
            bar.fg(app.depth.fit(t.accent)).add_modifier(Modifier::BOLD),
        ));
    }
    right.push(Span::styled(format!("{live} live"), bar));
    if working > 0 {
        right.push(Span::styled(format!(" · {working} working"), bar.fg(app.depth.fit(t.working))));
    }
    if blocked > 0 {
        right.push(Span::styled(format!(" · {blocked} blocked"), bar.fg(app.depth.fit(t.blocked))));
    }
    right.push(Span::styled(
        format!("  {} ? help ", key_label(&app.keymap.prefix)),
        bar.fg(app.depth.fit(t.dim)),
    ));
    if app.config.ui.clock
        && let Some((h, m)) = ptyro_pty::local_time_hm()
    {
        right.push(Span::styled(format!("{h:02}:{m:02} "), bar));
    }
    let used: u16 = right.iter().map(|s| s.width() as u16).sum();
    f.render_widget(
        Paragraph::new(Line::from(left)).style(bar),
        Rect { width: r.width.saturating_sub(used), ..r },
    );
    f.render_widget(
        Paragraph::new(Line::from(right)).style(bar).right_aligned(),
        Rect { x: r.x + r.width.saturating_sub(used), width: used.min(r.width), ..r },
    );
}

/// What the prefix table offers, as the user's keymap binds it: `d detach · n new · …`.
fn prefix_hint(app: &App) -> String {
    let prefix = format!("{} ", key_label(&app.keymap.prefix));
    let key = |action: &str| {
        app.keymap.key_for(action).map(|k| k.strip_prefix(&prefix).unwrap_or(&k).to_owned())
    };
    [
        (&["app.detach"][..], "detach"),
        (&["session.new"], "new"),
        (&["worktree.new"], "worktree"),
        (&["workspace.switch"], "workspace"),
        (&["session.find"], "find"),
        (&["pane.split_right", "pane.split_down"], "split"),
        (&["copy.enter"], "copy"),
        (&["palette"], "palette"),
        (&["help"], "help"),
    ]
    .iter()
    .filter_map(|(actions, label)| {
        let keys: Vec<String> = actions.iter().filter_map(|a| key(a)).collect();
        (!keys.is_empty()).then(|| format!("{} {label}", keys.join("/")))
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

/// Cut `s` to at most `width` columns, on a character boundary.
pub(crate) fn truncate_width(s: &mut String, width: usize) {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0;
    let cut = s.char_indices().find(|(_, c)| {
        used += c.width().unwrap_or(0);
        used > width
    });
    if let Some((i, _)) = cut {
        s.truncate(i);
    }
}

pub(crate) fn boxed(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4)).max(10.min(area.width));
    let h = h.min(area.height.saturating_sub(2)).max(3.min(area.height));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 3,
        width: w,
        height: h,
    }
}

fn draw_overlay(app: &App, f: &mut Frame<'_>, o: &Overlay, area: Rect) {
    let t = &app.theme;
    let border = fg(app, t.border_focus);
    let block = |title: String| {
        Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(
            format!(" {title} "),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        ))
    };
    match o {
        Overlay::Help { filter, scroll } => {
            let r = boxed(area, 88, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let lines = help_lines(app, filter);
            let total = ACTIONS.len();
            let title = if filter.is_empty() {
                format!("Keys ({total} actions) — type to filter, Esc closes")
            } else {
                format!("Keys matching {filter:?}")
            };
            let inner_h = r.height.saturating_sub(2) as usize;
            let start = (*scroll).min(lines.len().saturating_sub(inner_h));
            f.render_widget(Paragraph::new(lines[start..].to_vec()).block(block(title)), r);
        }
        Overlay::Picker { palette, .. } => {
            let rows = palette.filtered.len().min(14) as u16;
            let r = boxed(area, 76, rows + 4);
            f.render_widget(Clear, r);
            let mut lines = vec![Line::from(vec![
                Span::styled("› ", fg(app, t.accent)),
                Span::raw(palette.query.clone()),
            ])];
            let inner_w = r.width.saturating_sub(2) as usize;
            let first = palette.selected.saturating_sub(13);
            for i in first..palette.filtered.len().min(first + 14) {
                let item = palette.item(i);
                let key = item.key.clone().unwrap_or_default();
                let mut text = format!(" {}  {}", item.label, item.detail);
                let room = inner_w.saturating_sub(key.chars().count() + 2);
                if text.chars().count() > room {
                    text = text.chars().take(room.saturating_sub(1)).collect::<String>() + "…";
                }
                let pad = inner_w.saturating_sub(text.chars().count() + key.chars().count() + 1);
                let style = if i == palette.selected {
                    if t.monochrome {
                        Style::new().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::new()
                            .bg(app.depth.fit(t.selection_bg))
                            .fg(app.depth.fit(t.selection_fg))
                    }
                } else {
                    Style::new()
                };
                lines.push(Line::from(vec![
                    Span::styled(text, style),
                    Span::styled(
                        format!("{}{key} ", " ".repeat(pad)),
                        style.fg(app.depth.fit(t.dim)),
                    ),
                ]));
            }
            if palette.filtered.is_empty() {
                lines.push(Line::styled(" no matches", fg(app, t.dim)));
            }
            f.render_widget(Paragraph::new(lines).block(block(palette.title.clone())), r);
            f.set_cursor_position(Position::new(
                r.x + 3 + palette.query.chars().count() as u16,
                r.y + 1,
            ));
        }
        Overlay::Prompt { title, text, kind } => {
            let r = boxed(area, 70, 4);
            f.render_widget(Clear, r);
            // Checked as it's typed: characters a name can't hold are marked where they are.
            let check = app.prompt_check(kind, text);
            let bad = fg(app, t.error).add_modifier(Modifier::UNDERLINED);
            let mut field = vec![Span::styled("› ", fg(app, t.accent))];
            if kind.is_name() && check.is_err() {
                field.extend(text.chars().map(|c| {
                    let style = if ptyro_core::names::allowed(c) { Style::new() } else { bad };
                    Span::styled(c.to_string(), style)
                }));
            } else {
                field.push(Span::raw(text.clone()));
            }
            let help = match check {
                Err(e) => {
                    let fix = kind
                        .is_name()
                        .then(|| ptyro_core::names::tidy(text))
                        .flatten()
                        .map(|n| format!(" · Tab → {n}"))
                        .unwrap_or_default();
                    Line::styled(format!("✗ {e}{fix}"), fg(app, t.error))
                }
                Ok(note) => Line::styled(
                    match note {
                        Some(n) => format!("⏎ ok ({n}) · Esc cancel · C-u clear"),
                        None => "⏎ ok · Esc cancel · C-u clear".into(),
                    },
                    fg(app, t.dim),
                ),
            };
            let lines = vec![Line::from(field), help];
            f.render_widget(Paragraph::new(lines).block(block(title.clone())), r);
            f.set_cursor_position(Position::new(r.x + 3 + text.chars().count() as u16, r.y + 1));
        }
        Overlay::Confirm { text, .. } => {
            let widest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
            let r = boxed(area, widest + 6, text.lines().count() as u16 + 2);
            f.render_widget(Clear, r);
            let lines: Vec<Line<'_>> = text.lines().map(|l| Line::raw(l.to_owned())).collect();
            f.render_widget(Paragraph::new(lines).block(block("Confirm".into())), r);
        }
        Overlay::Inbox { selected, tab } => {
            let r = boxed(area, 100, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let entries = app.inbox_tab(tab.as_deref());
            let mut lines: Vec<Line<'_>> = Vec::new();
            let width = r.width.saturating_sub(4) as usize;
            let first = (*selected).saturating_sub(r.height.saturating_sub(5) as usize);
            for (i, e) in entries.iter().enumerate().skip(first) {
                let when = crate::ui::ago(e.at_ms);
                let mark = if e.read_ms.is_none() { "●" } else { " " };
                let mut text =
                    format!("{mark} {:<12} {:<6} {} — {}", e.kind.label(), when, e.title, e.body);
                if text.chars().count() > width {
                    text = text.chars().take(width.saturating_sub(1)).collect::<String>() + "…";
                }
                let mut style = match e.kind {
                    ptyro_core::status::AttentionKind::Blocked => fg(app, t.blocked),
                    ptyro_core::status::AttentionKind::Errored => fg(app, t.error),
                    _ => Style::new(),
                };
                if e.read_ms.is_some() {
                    style = style.add_modifier(Modifier::DIM);
                }
                if i == *selected {
                    style = style.bg(app.depth.fit(t.selection_bg));
                }
                lines.push(Line::styled(text, style));
            }
            if entries.is_empty() {
                lines.push(Line::styled(" nothing here", fg(app, t.dim)));
            }
            let name = tab.clone().unwrap_or_else(|| "all workspaces".into());
            let title = format!(
                "Inbox · {name} — ⏎ go · p peek · r read · R all read · Tab next tab · Esc"
            );
            f.render_widget(Paragraph::new(lines).block(block(title)), r);
        }
        Overlay::Peek { session, screen, reply } => {
            let r = boxed(area, 104, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let info = app.session(*session);
            let name = info.map_or_else(String::new, |s| s.name.clone());
            let mut lines: Vec<Line<'_>> = Vec::new();
            if let Some(s) = info {
                let (g, gc) = glyph(app, s);
                let status = s.status.as_ref().map_or_else(|| s.state.label(), |r| r.describe());
                lines.push(Line::from(vec![
                    Span::styled(format!("{g} "), fg(app, gc)),
                    Span::styled(status, Style::new().add_modifier(Modifier::BOLD)),
                ]));
                if let Some(a) = &s.attention {
                    lines.push(Line::styled(format!("⚑ {}", a.text), fg(app, t.search)));
                }
            }
            let body_h = r.height.saturating_sub(6) as usize;
            let shown: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
            for l in &shown[shown.len().saturating_sub(body_h)..] {
                lines.push(Line::styled(format!(" {l}"), fg(app, t.dim)));
            }
            let blocked = info
                .and_then(|s| s.status.as_ref())
                .is_some_and(|r| r.status == ptyro_core::status::AgentStatus::Blocked);
            let hint = if blocked && reply.is_empty() {
                "y allow · a always · n deny · type a reply · ⏎ open · Esc"
            } else {
                "type a reply, ⏎ sends it · empty ⏎ opens the session · Esc"
            };
            f.render_widget(Paragraph::new(lines).block(block(format!("Peek {name}"))), r);
            let input = Rect {
                y: r.bottom().saturating_sub(3),
                height: 2,
                x: r.x + 1,
                width: r.width.saturating_sub(2),
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("› ", fg(app, t.accent)),
                        Span::raw(reply.clone()),
                    ]),
                    Line::styled(hint, fg(app, t.dim)),
                ]),
                input,
            );
            f.set_cursor_position(Position::new(
                input.x + 2 + reply.chars().count() as u16,
                input.y,
            ));
        }
        Overlay::Tour { page } => {
            let pages = tour_pages(app);
            let (title, body) = &pages[(*page).min(pages.len() - 1)];
            let r = boxed(area, 76, (body.len() as u16 + 6).min(area.height));
            f.render_widget(Clear, r);
            let mut lines: Vec<Line<'_>> = vec![Line::raw("")];
            for l in body {
                // `key` spans in backticks are drawn in the accent colour.
                let mut spans = Vec::new();
                for (i, part) in l.split('`').enumerate() {
                    spans.push(if i % 2 == 1 {
                        Span::styled(
                            part.to_owned(),
                            fg(app, t.accent).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw(part.to_owned())
                    });
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("{}/{} · → or ⏎ next · ← back · q leave", page + 1, pages.len()),
                fg(app, t.dim),
            ));
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(block(format!("Tour: {title}"))),
                r,
            );
        }
        Overlay::ReviewQueue { items, selected } => {
            let r =
                boxed(area, 100, (items.len() as u16 * 2 + 4).min(area.height.saturating_sub(2)));
            f.render_widget(Clear, r);
            let mut lines: Vec<Line<'_>> = Vec::new();
            for (n, it) in items.iter().enumerate() {
                let sel = n == *selected;
                let style =
                    if sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>2}. {:<28}", n + 1, it.worktree.qualified()),
                        style.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", it.reasons.join(" · ")), fg(app, t.dim)),
                    Span::styled(format!("  {} ago", dur(it.waiting_ms)), fg(app, t.dim)),
                ]));
                lines.push(Line::styled(
                    format!("      {}", it.last_message.as_deref().unwrap_or("")),
                    fg(app, t.dim),
                ));
            }
            f.render_widget(
                Paragraph::new(lines)
                    .block(block("Ready for review — ⏎ inspect · d reviewed · Esc".into())),
                r,
            );
        }
        Overlay::Explain { session, lines: ex, scroll } => {
            let r = boxed(area, 110, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let name = app.session(*session).map_or_else(String::new, |s| s.name.clone());
            let inner_h = r.height.saturating_sub(2) as usize;
            let start = (*scroll).min(ex.len().saturating_sub(inner_h));
            let lines: Vec<Line<'_>> = ex
                .iter()
                .skip(start)
                .map(|l| {
                    let style = if l.applied { Style::new() } else { fg(app, t.dim) };
                    Line::styled(
                        format!(
                            "{:<6} {:<22} {:<34} {}",
                            crate::ui::ago(l.at_ms),
                            l.source,
                            l.signal,
                            l.reason
                        ),
                        style,
                    )
                })
                .collect();
            f.render_widget(
                Paragraph::new(lines)
                    .block(block(format!("Why is {name} in this state? (newest last) — j/k, Esc"))),
                r,
            );
        }
        Overlay::Trust { scripts, scroll, .. } => {
            let r = boxed(area, 90, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let mut lines: Vec<Line<'_>> = vec![
                Line::styled(
                    format!(
                        "{} wants to run these scripts (from its .ptyro.toml unless marked).",
                        scripts.project
                    ),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    "They run as you. Trust them only if you trust this repository.",
                    fg(app, t.dim),
                ),
                Line::raw(""),
            ];
            for l in scripts.text.lines() {
                let style = if l.starts_with("── ") { fg(app, t.accent) } else { Style::new() };
                lines.push(Line::styled(format!("  {l}"), style));
            }
            let inner_h = r.height.saturating_sub(4) as usize;
            let start = (*scroll).min(lines.len().saturating_sub(inner_h));
            let mut body: Vec<Line<'_>> = lines[start..].to_vec();
            body.truncate(inner_h);
            f.render_widget(Paragraph::new(body).block(block("Trust scripts?".into())), r);
            let foot = Rect {
                y: r.bottom().saturating_sub(2),
                height: 1,
                x: r.x + 2,
                width: r.width.saturating_sub(4),
            };
            f.render_widget(
                Paragraph::new(Line::styled(
                    "y trust (asks again if they change) · n not now · j/k scroll",
                    fg(app, t.accent),
                )),
                foot,
            );
        }
    }
}

/// The help overlay's lines for a filter (its key handler clamps scrolling to their count).
pub(crate) fn help_lines<'a>(app: &'a App, filter: &str) -> Vec<Line<'a>> {
    let t = &app.theme;
    let prefix = key_label(&app.keymap.prefix);
    let mut lines: Vec<Line<'_>> = Vec::new();
    let needle = filter.to_lowercase();
    let mut last_table = None;
    for (table, key, cmd) in &app.keymap.bindings() {
        let (table, key) = (*table, key.clone());
        let desc = crate::keymap::spec(cmd.action).map_or("", |s| s.desc);
        let arg = cmd.arg.as_deref().map(|a| format!(" {a}")).unwrap_or_default();
        let keys = match table {
            Table::Prefix => format!("{prefix} {key}"),
            Table::Global | Table::Nav => key.clone(),
        };
        let text = format!("{keys:<14} {}{arg}  {desc}", cmd.action);
        if !needle.is_empty() && !text.to_lowercase().contains(&needle) {
            continue;
        }
        if last_table != Some(table) {
            lines.push(Line::styled(format!("── {} ──", table.label()), fg(app, t.dim)));
            last_table = Some(table);
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{keys:<14} "), fg(app, t.accent)),
            Span::styled(format!("{}{arg}", cmd.action), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {desc}"), fg(app, t.dim)),
        ]));
    }
    if needle.is_empty() || "copy mode".contains(&needle) {
        lines.push(Line::styled("── in copy mode ──", fg(app, t.dim)));
        for l in [
            "h j k l / arrows   move        w b e   words       0 ^ $   line start/end",
            "gg G H M L         jump        C-u C-d half page   C-b C-f page",
            "[ ]                previous / next shell prompt (needs `ptyro shell-init`)",
            "/ ?  n N           search forward / backward, next / previous match",
            "v V C-v            select chars / lines / block    o  other end",
            "y or Enter         copy (OSC 52 to your terminal)   Y  copy line   q Esc  leave",
            "mouse              drag to select (copies on release), wheel scrolls",
        ] {
            lines.push(Line::raw(format!("  {l}")));
        }
    }
    if needle.is_empty() {
        let prefixes: Vec<String> = app.keymap.prefixes.iter().map(key_label).collect();
        lines.push(Line::styled(
            format!("── prefix: {} (twice sends it to the program) ──", prefixes.join(" or ")),
            fg(app, t.dim),
        ));
        lines.push(Line::styled(
            format!(
                "── this terminal: kitty keyboard {} · colours {:?} · theme {} ──",
                if app.kitty {
                    "on (Shift+Enter works)"
                } else {
                    "off (Shift+Enter falls back to Enter)"
                },
                app.depth,
                app.theme_name
            ),
            fg(app, t.dim),
        ));
    }
    lines
}

/// `12s`, `4m`, `3h`, `2d` ago.
pub fn ago(ms: u64) -> String {
    dur(ptyro_core::now_ms().saturating_sub(ms))
}

/// A duration: `12s`, `4m`, `3h`, `2d`.
pub fn dur(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// The tour's pages: (title, lines), keys as the user's keymap binds them.
pub fn tour_pages(app: &App) -> Vec<(String, Vec<String>)> {
    let k = |action: &str| app.keymap.key_for(action).unwrap_or_else(|| "(unbound)".into());
    let prefix = ptyro_core::config::key_label(&app.keymap.prefix);
    vec![
        ("welcome".into(), vec![
            "ptyro keeps your agents and shells running in a background daemon,".into(),
            "each in its own git worktree if you like, and tells you which one needs you.".into(),
            String::new(),
            format!("Everything starts with the prefix, `{prefix}`, then a key."),
            format!("`{}` lists every key; `{}` runs any action by name.", k("help"), k("palette")),
        ]),
        ("sessions".into(), vec![
            format!("`{}`  new session: pick an agent and where it runs (⏎ ⏎ = default agent in a new worktree)", k("session.new")),
            format!("`{}`  new worktree with an agent", k("worktree.new")),
            format!("`{}` / `{}`  next / previous session · `{}` find one", k("session.next"), k("session.prev"), k("session.find")),
            format!("`{}`  the sidebar: j/k move, ⏎ opens, x stops, Space marks", k("focus.toggle")),
            format!("`{}`  detach: everything keeps running; `ptyro` comes back", k("app.detach")),
        ]),
        ("who needs you".into(), vec![
            "● working   ⚠ blocked (on what)   ○ idle   ✓ done   ✗ error   ☾ dormant   ◌ a guess".into(),
            "⚑ wants your attention (it finished, or asks something)".into(),
            "✔ done and verified   ↩ sent back to fix a failing check   ✘ claims done, check fails".into(),
            String::new(),
            format!("`{}`  jump to the next session that needs you", k("attention.next")),
            format!("`{}`  peek at it and answer without switching (y / a / n, or type a reply)", k("peek")),
            format!("`{}`  the inbox · `{}`  why does it show this status?", k("inbox"), k("session.explain")),
        ]),
        ("panes".into(), vec![
            format!("`{}` / `{}`  split beside / below with another session", k("pane.split_right"), k("pane.split_down")),
            format!("`{}`  zoom · `{}`  close the pane (the session keeps running)", k("pane.zoom"), k("pane.close")),
            format!("`{}`  copy mode: scroll back, / search, v select, y copy", k("copy.enter")),
            format!("Mark sessions with Space in the sidebar, then `{}` types one prompt into all of them.", k("broadcast")),
        ]),
        ("many agents".into(), vec![
            format!("`{}`  runs: fan-outs and recipes, their steps and gates (1-9 answers a gate)", k("runs")),
            "  c compares a fan-out's candidates, ranked by tests and evidence; m merges the best".into(),
            "`ptyro fanout --agents claude,codex,gemini --prompt-file task.md --test` from the shell".into(),
            "`ptyro recipe ls` · `ptyro new --agent claude --lead` gives an agent workers of its own".into(),
        ]),
        ("from scripts".into(), vec![
            "Everything works from the shell too, for scripts and other agents:".into(),
            String::new(),
            "`ptyro new -d --agent claude --new-wt --prompt \"fix the flaky test\"`".into(),
            "`ptyro wait @working --for idle` · `ptyro read NAME` · `ptyro send NAME TEXT --wait`".into(),
            "`ptyro doctor` checks agents, logins and integrations · `ptyro tour` shows this again".into(),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn truncation_counts_columns_not_bytes() {
        let mut s = " ⏎ open/fold · n new".to_owned();
        super::truncate_width(&mut s, 2);
        assert_eq!(s, " ⏎");
        let mut s = "日本語".to_owned();
        super::truncate_width(&mut s, 3);
        assert_eq!(s, "日", "a wide char that doesn't fit is left out");
        let mut s = "short".to_owned();
        super::truncate_width(&mut s, 80);
        assert_eq!(s, "short");
    }
}
