//! Drawing (SPEC §11.1): sidebar, panes with headers, dividers, status bar and overlays.

use ptyro_core::SessionId;
use ptyro_core::config::key_label;
use ptyro_core::session::{SessionInfo, SessionKind, Show};
use ptyro_core::status::AgentStatus;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

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

/// The title people gave a session (UX1.24), not one its program put in the window title.
pub(crate) fn session_title(s: &SessionInfo) -> Option<&str> {
    s.title.as_deref().filter(|t| s.window_title.as_deref() != Some(*t) && *t != s.name)
}

fn what(s: &SessionInfo) -> String {
    if let Some(role) = &s.script {
        return role.trim_start_matches("run:").to_owned();
    }
    let program =
        s.argv.first().map(|a| a.rsplit('/').next().unwrap_or(a).to_owned()).unwrap_or_default();
    // An agent started by hand in a shell (UX1.4): `zsh › claude`.
    if let Some(agent) = &s.inner_agent {
        return format!("{program} › {}", s.foreground.as_deref().unwrap_or(agent));
    }
    // An agent session is its agent, whatever its process is called (a wrapper script, a
    // launcher, the tour's pretend one).
    if s.kind == SessionKind::Agent
        && let Some(agent) = &s.agent
    {
        return agent.clone();
    }
    s.foreground.clone().unwrap_or(program)
}

pub(crate) fn fg(app: &App, c: Color) -> Style {
    Style::new().fg(app.depth.fit(c))
}

pub fn draw(app: &App, f: &mut Frame<'_>) {
    app.links.borrow_mut().clear();
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
        if app.chat_panel.is_some() && app.inspector.is_none() {
            crate::chats::draw(app, f, app.main_area());
        }
        if app.inspector.is_some() {
            let h =
                if app.config.ui.status_bar { area.height.saturating_sub(1) } else { area.height };
            crate::inspector::draw(app, f, Rect { height: h, ..area });
        }
        if let Some(id) = app.pad_popup {
            draw_pad_popup(app, f, area, id);
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
        if let Some(tour) = &app.tour {
            draw_coach(app, f, area, tour);
        }
    }
    if app.first_run.is_some() {
        crate::onboard::draw(app, f, area);
    }
    if let Some(o) = &app.overlay {
        draw_overlay(app, f, o, area);
    } else if app.which_key_shown() {
        draw_which_key(app, f, area);
    }
}

/// The scratchpad's editor as a window over everything (NOTE1.1, `[scratchpad] mode = "popup"`).
fn draw_pad_popup(app: &App, f: &mut Frame<'_>, area: Rect, id: SessionId) {
    let t = &app.theme;
    let r = crate::pad::popup_rect(area);
    f.render_widget(Clear, r);
    let mut path = app.config.scratchpad_path(&app.paths.data).display().to_string();
    // The end of a long path is the part that says which file.
    let room = (r.width as usize).saturating_sub(34).max(12);
    if path.chars().count() > room {
        let tail: String =
            path.chars().rev().take(room - 1).collect::<Vec<_>>().into_iter().rev().collect();
        path = format!("…{tail}");
    }
    let close = app.pad_key().map(|k| format!(" · {k} closes")).unwrap_or_default();
    let block = Block::default().borders(Borders::ALL).border_style(fg(app, t.border_focus)).title(
        Span::styled(
            format!(" ✎ Scratchpad · {path}{close} "),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        ),
    );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let Some(cache) = app.caches.get(&id) else { return };
    paint::screen(cache, f.buffer_mut(), inner, app.depth, false);
    if app.overlay.is_none()
        && !app.prefix
        && let Some(p) = paint::cursor(cache, inner)
    {
        f.set_cursor_position(p);
    }
}

/// The tour's coach (UX1.22): bottom right, above the status bar, never in the way of keys.
fn draw_coach(app: &App, f: &mut Frame<'_>, area: Rect, tour: &crate::tour::Tour) {
    let t = &app.theme;
    let (title, lines) = tour.coach(app);
    let width = 58.min(area.width.saturating_sub(2));
    let inner = width.saturating_sub(2) as usize;
    let mut rows: Vec<Line<'_>> = Vec::new();
    for l in &lines {
        for row in textwrap_simple(l, inner) {
            rows.push(Line::raw(row));
        }
    }
    let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let bottom = area.bottom().saturating_sub(u16::from(app.config.ui.status_bar));
    let r = Rect {
        x: area.right().saturating_sub(width + 1),
        y: bottom.saturating_sub(height),
        width,
        height,
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(rows).block(
            Block::default().borders(Borders::ALL).border_style(fg(app, t.accent)).title(
                Span::styled(format!(" {title} "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
            ),
        ),
        r,
    );
}

/// `text` wrapped at spaces into rows of at most `width` columns.
fn textwrap_simple(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    for word in text.split(' ') {
        let row = rows.last_mut().expect("a row");
        if !row.is_empty() && row.width() + 1 + word.width() > width {
            rows.push(word.to_owned());
        } else {
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(word);
        }
    }
    rows
}

/// The menu after the prefix (UX1.14): what each next key does, grouped, as bound now.
pub(crate) const WHICH_KEY: &[(&str, &[(&str, &str)])] = &[
    (
        "start",
        &[
            ("session.new", "a task"),
            ("session.again", "+ again"),
            ("worktree.new", "worktree"),
            ("project.add", "project"),
            ("workspace.switch", "workspace"),
            ("config.panel", "settings"),
            ("broadcast", "broadcast"),
            ("chat.new", "chat"),
            ("palette", "any action"),
        ],
    ),
    (
        "go",
        &[
            ("attention.next", "who needs you"),
            ("peek", "peek & answer"),
            ("inbox", "inbox"),
            ("session.find", "find"),
            ("session.next", "next"),
            ("session.last", "last session"),
            ("chat.list", "chats"),
            ("home", "home"),
            ("sidebar.zoom", "everything"),
        ],
    ),
    (
        "panes",
        &[
            ("pane.split_right", "split │"),
            ("pane.split_down", "split ─"),
            ("pane.zoom", "zoom"),
            ("pane.close", "close"),
            ("focus.toggle", "sidebar"),
            ("sidebar.filter", "sidebar filter"),
            ("copy.enter", "scroll back"),
        ],
    ),
    (
        "work",
        &[
            ("inspector", "diff & ship"),
            ("review.queue", "to review"),
            ("runs", "runs"),
            ("sets", "change sets"),
            ("script.run", "run script"),
            ("worktree.open", "editor"),
            ("worktree.gc", "clean up"),
            ("shell.new", "new shell"),
            ("shell.drawer", "quick shell"),
            ("scratchpad.toggle", "scratchpad"),
        ],
    ),
    (
        "this",
        &[
            ("health", "mend"),
            ("row.menu", "its actions"),
            ("session.rename", "rename"),
            ("session.stop", "stop"),
            ("toast.act", "undo / fix"),
            ("app.detach", "detach"),
            ("app.quit", "quit all"),
            ("app.about", "about"),
            ("help", "all keys"),
        ],
    ),
];

fn draw_which_key(app: &App, f: &mut Frame<'_>, area: Rect) {
    let t = &app.theme;
    let prefix = format!("{} ", key_label(&app.keymap.prefix));
    let key = |action: &str| {
        app.keymap.key_for(action).and_then(|k| k.strip_prefix(&prefix).map(str::to_owned))
    };
    // Quick-new says what it starts: `+ claude`.
    let label = |a: &str, label: &str| match a {
        "session.again" => format!("+ {}", app.again_label()),
        _ => label.to_owned(),
    };
    let columns: Vec<(&str, Vec<(String, String)>)> = WHICH_KEY
        .iter()
        .map(|(group, items)| {
            (*group, items.iter().filter_map(|(a, l)| Some((key(a)?, label(a, l)))).collect())
        })
        .collect();
    let rows = columns.iter().map(|(_, v)| v.len()).max().unwrap_or(0) as u16;
    let col_w = 19u16;
    let w = (col_w * columns.len() as u16 + 2).min(area.width);
    let h = (rows + 3).min(area.height.saturating_sub(1));
    let r = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.bottom().saturating_sub(h + 1),
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::default().borders(Borders::ALL).border_style(fg(app, t.border_focus)).title(
        Span::styled(
            format!(" {}then… (Esc: never mind) ", prefix),
            fg(app, t.accent).add_modifier(Modifier::BOLD),
        ),
    );
    f.render_widget(block, r);
    for (i, (group, items)) in columns.iter().enumerate() {
        let x = r.x + 1 + col_w * i as u16;
        let mut lines =
            vec![Line::styled(format!(" {group}"), fg(app, t.dim).add_modifier(Modifier::ITALIC))];
        for (k, label) in items {
            lines.push(Line::from(vec![
                Span::styled(format!(" {k:>3} "), fg(app, t.accent).add_modifier(Modifier::BOLD)),
                Span::styled(label.clone(), fg(app, t.text)),
            ]));
        }
        let col = Rect {
            x,
            y: r.y + 1,
            width: col_w.min(r.right().saturating_sub(x + 1)),
            height: h.saturating_sub(2),
        };
        f.render_widget(Paragraph::new(lines), col);
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
    // A machine with no profiles of its own beyond the implicit `default` shows none; another
    // machine's (MM1.B, `laptop2:default`) counts on that machine alone.
    let (machine, bare) = crate::tree::machine_and_name(profile);
    let on_that_machine = app.profiles.iter().filter(|p| p.machine.as_deref() == machine).count();
    if bare == ptyro_core::model::DEFAULT_PROFILE
        && !info.is_some_and(|p| p.defined)
        && on_that_machine <= 1
    {
        return None;
    }
    let label = info.map_or_else(|| bare.to_owned(), |p| p.label.clone());
    let style = match profile_color(app, profile) {
        Some(c) if !app.theme.monochrome => {
            Style::new().fg(app.depth.fit(c)).add_modifier(Modifier::BOLD)
        }
        _ => Style::new().add_modifier(Modifier::BOLD),
    };
    Some(Span::styled(format!("[{label}]"), style))
}

/// What is left to do with a worktree's work (UX2.2): `↑2` unpushed commits (or `not pushed`
/// when there is no upstream yet), `● 3` uncommitted files, the `+N −N` line counts only while
/// either is true, and a quiet `✓ pushed` when committed work is all on the remote. A merged or
/// integrated one says so elsewhere in the row, and an open PR's badge stands in for `✓ pushed`.
pub(crate) fn diffstat(app: &App, w: &ptyro_core::model::WorktreeInfo) -> Vec<Span<'static>> {
    let t = &app.theme;
    let mut v = Vec::new();
    let Some(s) = &w.status else { return v };
    if w.shipped() {
        return v;
    }
    let l = s.left();
    if l.open() {
        if s.insertions > 0 {
            v.push(Span::styled(format!(" +{}", s.insertions), fg(app, t.done)));
        }
        if s.deletions > 0 {
            v.push(Span::styled(format!(" −{}", s.deletions), fg(app, t.error)));
        }
    }
    if l.unpushed > 0 {
        let what = if l.not_pushed {
            format!(" not pushed ↑{}", l.unpushed)
        } else {
            format!(" ↑{}", l.unpushed)
        };
        v.push(Span::styled(what, fg(app, t.accent)));
    }
    if l.uncommitted > 0 {
        v.push(Span::styled(format!(" ● {}", l.uncommitted), fg(app, t.working)));
    }
    if l.pushed && w.pr.is_none() {
        v.push(Span::styled(" ✓ pushed", fg(app, t.dim)));
    }
    v
}

/// A dot in a machine's colour, the same for the same name everywhere (the web page uses the
/// same hash): a second cue beside the name, never the only one.
fn machine_dot(name: &str) -> Span<'static> {
    let (r, g, b) = hsl_rgb(f32::from(ptyro_core::machine::hue(name)), 0.55, 0.6);
    Span::styled("\u{25cf} ", Style::default().fg(Color::Rgb(r, g, b)))
}

/// `laptop2:work` → `laptop2 / work`; this machine's `work` stays `work`.
pub fn workspace_label(name: &str) -> String {
    match crate::tree::machine_and_name(name) {
        (Some(m), w) => format!("{m} / {w}"),
        (None, w) => w.to_owned(),
    }
}

/// `14:02`, local time, of a moment in ms.
pub fn clock(ms: u64) -> String {
    ptyro_pty::local_time_hm_at(i64::try_from(ms / 1000).unwrap_or(0))
        .map_or_else(String::new, |(h, m)| format!("{h:02}:{m:02}"))
}

/// HSL (hue in degrees) to 8-bit RGB.
fn hsl_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round() as u8;
    (to(r), to(g), to(b))
}

fn draw_sidebar(app: &App, f: &mut Frame<'_>, area: Rect) {
    let t = &app.theme;
    // Which machine this is, first (MM1.B: other machines' workspaces are below too), then the
    // workspace you're in.
    let mut title = vec![Span::raw(" ")];
    let workspace = workspace_label(&app.workspace);
    if let Some(m) = &app.machine {
        // A long host name gives way to the workspace: " ● " + name + " · " + workspace.
        let room = (area.width.saturating_sub(2) as usize)
            .saturating_sub(UnicodeWidthStr::width(workspace.as_str()) + 6)
            .max(6);
        let mut name = m.clone();
        if UnicodeWidthStr::width(name.as_str()) > room {
            truncate_width(&mut name, room - 1);
            name.push('\u{2026}');
        }
        title.push(machine_dot(m));
        title.push(Span::styled(name, fg(app, t.text).add_modifier(Modifier::BOLD)));
        title.push(Span::styled(" · ", fg(app, t.dim)));
    }
    title.push(Span::styled(workspace, fg(app, t.accent).add_modifier(Modifier::BOLD)));
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
    let mut lines = vec![Line::from(title), filter_line(app, area.width)];
    lines.extend(body.lines.into_iter().skip(body.skip).map(|(_, l)| l));
    if body.empty {
        let key = |a: &str| app.keymap.key_for(a).unwrap_or_else(|| a.to_owned());
        if app.show != Show::All {
            // Filtered to nothing: say so, and how to get the rest back.
            lines.push(Line::styled(
                format!("  nothing {} right now", app.show.label()),
                fg(app, t.dim),
            ));
            lines.push(Line::styled(
                format!("  {} shows all", key("sidebar.filter")),
                fg(app, t.dim),
            ));
        } else {
            lines.push(Line::styled(format!("  none yet: {}", key("session.new")), fg(app, t.dim)));
        }
        if app.show == Show::All && app.tree().project_ids().is_empty() {
            lines.push(Line::styled(
                format!("  add a repo: {}", key("project.add")),
                fg(app, t.dim),
            ));
        }
    }
    let mut hint = if app.sidebar_zoom {
        " everything at once · ⏎ open · p peek · z or Esc back".to_string()
    } else if app.focus == Focus::Sidebar {
        // The keys as they are bound now, not as they were by default.
        let nav = |a: &str| app.keymap.key_in(Table::Nav, a).unwrap_or_else(|| a.to_owned());
        format!(
            " ⏎ open/fold · {} new · {} stop · {} filter · Esc",
            nav("session.new"),
            nav("session.stop"),
            nav("sidebar.filter")
        )
    } else {
        let any = |a: &str| app.keymap.key_for(a).unwrap_or_else(|| a.to_owned());
        format!(" {} sidebar · {} workspaces", any("focus.toggle"), any("workspace.switch"))
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

/// The chips of the filter strip under the sidebar's title: each with its label and the columns
/// (from the sidebar's left edge) it covers. As many words as fit: `all 14  running 6  active 2`,
/// else `all  run 6  act 2`. Counts are of real sessions (not quick terminals).
pub(crate) fn filter_chips(app: &App, width: u16) -> Vec<(Show, String, u16, u16)> {
    let count = |s: Show| app.sessions.iter().filter(|x| x.is_real() && s.keeps(x)).count();
    let tiers: [[String; 3]; 2] = [
        [
            format!("all {}", count(Show::All)),
            format!("running {}", count(Show::Running)),
            format!("active {}", count(Show::Active)),
        ],
        [
            "all".into(),
            format!("run {}", count(Show::Running)),
            format!("act {}", count(Show::Active)),
        ],
    ];
    let w = |l: &[String; 3]| {
        l.iter().map(|x| UnicodeWidthStr::width(x.as_str()) + 2).sum::<usize>() + 1
    };
    let labels = if w(&tiers[0]) <= width as usize { &tiers[0] } else { &tiers[1] };
    let mut x = 1u16;
    Show::EVERY
        .iter()
        .zip(labels)
        .map(|(s, l)| {
            // A space of padding each side, so the chosen one reads as a button.
            let end = x + UnicodeWidthStr::width(l.as_str()) as u16 + 2;
            let chip = (*s, format!(" {l} "), x, end);
            x = end + 1;
            chip
        })
        .collect()
}

fn filter_line(app: &App, width: u16) -> Line<'static> {
    let t = &app.theme;
    let mut spans = vec![Span::raw(" ")];
    for (i, (s, label, ..)) in filter_chips(app, width).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if s == app.show {
            fg(app, t.accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if s == Show::Active
            && app.sessions.iter().any(|x| x.is_real() && Show::Active.keeps(x))
        {
            fg(app, t.working)
        } else {
            fg(app, t.dim)
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
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
    // Less the title, the filter strip and the hint line.
    let room = area.height.saturating_sub(3) as usize;
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
                    (Some(b), false) if !w.default_branch() => {
                        spans.push(Span::styled(format!(" {b}"), fg(app, t.dim)))
                    }
                    _ => {}
                }
                if w.state != ptyro_core::model::WorktreeState::Ready {
                    spans.push(Span::styled(format!(" {}", w.state.label()), fg(app, t.error)));
                }
                spans.extend(diffstat(app, w));
                // Integrated into the main branch, and nothing new since (UX1.12).
                if w.shipped() && w.pr.is_none() {
                    spans.push(Span::styled(" in main", fg(app, t.done)));
                }
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
                // What it's for (UX1.24), last so a narrow sidebar cuts it first, and only when
                // its name or PR doesn't already say it.
                let pr_title = w.pr.as_ref().map(|pr| format!("#{} {}", pr.number, pr.title));
                if let Some(title) = w.title.as_deref().filter(|t| {
                    Some(*t) != pr_title.as_deref()
                        && (ptyro_core::names::from_text(t).as_deref() != Some(w.name.as_str())
                            || t.split_whitespace().count() > 3)
                }) {
                    spans.push(Span::styled(format!(" — {title}"), fg(app, t.dim)));
                }
                Line::from(spans)
            }
            Row::Session(id) => {
                let Some(s) = sessions.get(id).copied() else { continue };
                let bare = s
                    .machine
                    .as_deref()
                    .and_then(|m| s.name.strip_prefix(&format!("{m}:")))
                    .unwrap_or(&s.name);
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
                    // Under its worktree, `brisk-rook.setup` is just `setup`, and the agent
                    // named after the worktree (`fix-flaky-test`) is just its program. Under
                    // another machine's workspace, its name goes without the machine (MM1.B).
                    Span::styled(
                        s.worktree
                            .as_deref()
                            .and_then(|w| app.worktree(w))
                            .and_then(|w| {
                                let wt = w.name.trim_start_matches('@');
                                if bare == wt {
                                    return Some(what(s));
                                }
                                bare.strip_prefix(&format!("{wt}.")).map(str::to_owned)
                            })
                            .unwrap_or_else(|| bare.to_owned()),
                        name_style,
                    ),
                ];
                let same_as_worktree = s
                    .worktree
                    .as_deref()
                    .and_then(|w| app.worktree(w))
                    .is_some_and(|w| w.name == bare);
                if s.script.is_none() && !same_as_worktree && s.agent.as_deref() != Some(bare) {
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
            // A quick terminal (UX1.25): one quiet line, first under its worktree, saying what
            // runs in it; bright while its drawer is open.
            Row::Quick(id) => {
                let Some(s) = sessions.get(id).copied() else { continue };
                let indent = if s.worktree.as_deref().and_then(|w| app.worktree(w)).is_some() {
                    "     "
                } else {
                    " "
                };
                let open = app.drawer_session() == Some(s.id);
                let mut name_style = fg(app, if open { t.text } else { t.dim });
                if Some(s.id) == focused {
                    name_style =
                        name_style.add_modifier(Modifier::BOLD).fg(app.depth.fit(t.accent));
                }
                let mut spans = vec![
                    Span::styled(format!("{indent}  "), Style::new()),
                    Span::styled("›_ ", fg(app, if open { t.accent } else { t.dim })),
                    Span::styled("quick shell", name_style),
                ];
                if let Some(r) = &s.status
                    && r.status == AgentStatus::Blocked
                {
                    spans.push(Span::styled(
                        format!(" ⚠ {}", r.kind.as_deref().unwrap_or("needs you")),
                        fg(app, t.blocked),
                    ));
                }
                if let Some(doing) = quick_doing(s) {
                    spans.push(Span::styled(format!(" {doing}"), fg(app, t.dim)));
                }
                // Open: the whole line tinted, so what is showing in the drawer is plain.
                let line = Line::from(spans);
                if open && !t.monochrome {
                    line.style(Style::new().bg(app.depth.fit(t.bar_bg)))
                } else {
                    line
                }
            }
            Row::Group(label) => {
                Line::styled(format!(" {label}"), fg(app, t.dim).add_modifier(Modifier::ITALIC))
            }
            Row::Chats(_) | Row::Chat(_) | Row::ChatNew(_) | Row::ChatAll(_) => {
                let shown = match &app.chat_panel {
                    Some(crate::chats::ChatPanel::View(v)) => v.id.as_deref(),
                    _ => None,
                };
                let Some(l) = crate::chats::sidebar_line(app, row, shown) else { continue };
                l
            }
            // A workspace (MM1.B): its machine in that machine's colour when it is another's, and
            // how its link stands when that isn't well.
            Row::Workspace(name) => {
                let folded = app.collapsed.contains(&crate::tree::fold_key(name));
                let arrow = if folded { "▸" } else { "▾" };
                let (machine, short) = crate::tree::machine_and_name(name);
                let current = *name == app.workspace;
                let mut spans = vec![Span::styled(format!("{arrow} "), fg(app, t.dim))];
                if let Some(m) = machine {
                    spans.push(machine_dot(m));
                    spans.push(Span::styled(format!("{m} / "), fg(app, t.dim)));
                }
                let style =
                    fg(app, if current { t.accent } else { t.text }).add_modifier(Modifier::BOLD);
                spans.push(Span::styled(short.to_owned(), style));
                // How its machine's link stands first (it's what matters), then its profile.
                let badge = app
                    .workspaces
                    .iter()
                    .find(|w| w.name == *name)
                    .and_then(|w| profile_badge(app, &w.profile));
                if let Some(m) = app.machine_of_name(name) {
                    use ptyro_core::model::MachineState as S;
                    let since = m.since_ms.map(clock).unwrap_or_default();
                    match m.state {
                        S::Online => {}
                        S::Offline => {
                            spans.push(Span::styled(format!(" offline {since}"), fg(app, t.error)))
                        }
                        S::Mismatch => {
                            spans.push(Span::styled(" other version", fg(app, t.error)));
                            if let Some(f) = &m.fix {
                                detail.push(Line::styled(format!("     → {f}"), fg(app, t.dim)));
                            }
                        }
                        S::Connecting => spans.push(Span::styled(" connecting…", fg(app, t.dim))),
                    }
                }
                if let Some(b) = badge {
                    spans.push(Span::raw(" "));
                    spans.push(b);
                }
                Line::from(spans)
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
/// What a quick terminal's line says it's doing: the command running (`cargo test`, `vim`),
/// else the last one and how it went (`✓ make` / `✗ make`), else nothing.
fn quick_doing(s: &SessionInfo) -> Option<String> {
    let shell = s.argv.first().map(|a| a.rsplit('/').next().unwrap_or(a).trim_start_matches('-'));
    match &s.command {
        Some(c) if c.ended_ms.is_none() => Some(
            c.text
                .as_deref()
                .map_or_else(|| "running".into(), |t| ptyro_core::session::brief(t, 40)),
        ),
        _ if s.foreground.is_some()
            && s.foreground.as_deref().map(|f| f.trim_start_matches('-')) != shell =>
        {
            s.foreground.clone()
        }
        Some(c) => {
            let mark = if c.exit.unwrap_or(0) == 0 { "✓" } else { "✗" };
            c.text.as_deref().map(|t| format!("{mark} {}", ptyro_core::session::brief(t, 40)))
        }
        None => None,
    }
}

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
    // An agent started by hand in a shell: a guess off its screen, and what adopting gives.
    if s.inner_agent.as_deref() == Some("claude") {
        items.push(("◌", "by hand · A adopts it: hooks, resume".into(), t.dim));
    } else if let Some(a) = &s.inner_agent {
        items.push(("◌", format!("{a} by hand, status off its screen"), t.dim));
    }
    // Its title (`--title`, `retitle`, else the first prompt), not a window title (UX1.24).
    if let Some(task) = session_title(s).or(s.task.as_deref()) {
        items.push(("task", task.to_owned(), t.text));
    }
    let working = s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Working);
    // A small model's line on it (UX1.3): this turn's says what its last reply would.
    let summary = s.summary.as_ref().filter(|_| !working && live);
    if let Some(sm) = summary {
        items.push(("≈", sm.text.clone(), t.text));
    }
    let summarized = summary.is_some_and(|sm| sm.turn >= s.turns);
    if let Some(reply) = s.last_reply.as_ref().filter(|_| !working && !summarized) {
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
    // Zoomed with its drawer (UX1.25): the one divider between them.
    let rects = app.pane_rects();
    if app.zoom
        && let [(_, top), _] = rects[..]
    {
        for x in top.left()..top.right() {
            if let Some(c) = f.buffer_mut().cell_mut(Position::new(x, top.bottom())) {
                c.set_symbol("─").set_style(fg(app, tint.unwrap_or(t.border)));
            }
        }
    }
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
    for (pane, rect) in rects {
        let focused = pane == app.focused && app.focus == Focus::Pane;
        let state = app.panes.get(&pane);
        let session = state.and_then(|p| p.session);
        let info = session.and_then(|id| app.session(id));
        let inner = app.inner(rect);
        // (A pane too short for a header has none: `inner` is all of it, and a header drawn
        // anyway would sit on the session's first row.)
        if inner.y > rect.y {
            draw_header(app, f, Rect { height: 1, ..rect }, inner, info, pane, focused);
        }
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
            let found = crate::about::urls_in(f.buffer_mut(), inner);
            app.links.borrow_mut().extend(found);
            continue;
        }
        let Some(cache) = app.caches.get(&id) else { continue };
        let exited = app.exit_state(id);
        paint::screen(cache, f.buffer_mut(), inner, app.depth, exited.is_some());
        let found = crate::about::urls_in(f.buffer_mut(), inner);
        app.links.borrow_mut().extend(found);
        if let Some(state) = &exited {
            let action = if *state == ptyro_core::session::ProcessState::Dormant {
                "resume"
            } else {
                "restart"
            };
            let text = format!(
                " {} {} — ⏎ {action} · {} remove ",
                info.map_or("", |i| i.name.as_str()),
                state.label(),
                app.keymap.key_for("session.stop").unwrap_or_else(|| "session.stop".into())
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
    offer(key("session.new"), "start a task: say what to do");
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
            let drawer = app.drawer.is_some_and(|d| d.pane == pane);
            let g = if s.pad {
                "✎"
            } else if drawer || s.quick {
                "›_"
            } else {
                g
            };
            left.push(Span::styled(format!(" {g} "), base.fg(app.depth.fit(gc))));
            if s.pad
                && let Some(k) = app.pad_key()
            {
                right.push(Span::styled(
                    format!(" {k} {} ", if focused { "hide" } else { "focus" }),
                    base.fg(app.depth.fit(t.dim)),
                ));
            } else if (drawer || s.quick)
                && let Some(k) = app.quick_key()
            {
                // A shell in a panel of its own goes back to what it replaced (UX2.3).
                let hide = if drawer { "hide" } else { "back" };
                right.push(Span::styled(
                    format!(" {k} {} ", if focused { hide } else { "focus" }),
                    base.fg(app.depth.fit(t.dim)),
                ));
            }
            let name = if focused {
                base.fg(app.depth.fit(t.accent)).add_modifier(Modifier::BOLD)
            } else {
                base.add_modifier(Modifier::BOLD)
            };
            // Another machine's session (MM1.B): its machine's dot, and its state when it's down.
            if let Some(m) = &s.machine {
                left.push(Span { style: base.patch(machine_dot(m).style), ..machine_dot(m) });
                if let Some(mi) = app.machines.iter().find(|x| x.name == *m)
                    && mi.state != ptyro_core::model::MachineState::Online
                {
                    let state = match (mi.state, mi.since_ms) {
                        (ptyro_core::model::MachineState::Offline, Some(t)) => {
                            format!("offline since {}", clock(t))
                        }
                        (s, _) => s.label().to_owned(),
                    };
                    right.push(Span::styled(
                        format!(" {m} {state} "),
                        base.fg(app.depth.fit(t.error)),
                    ));
                }
            }
            left.push(Span::styled(s.name.clone(), name));
            left.push(Span::styled(format!(" · {}", what(s)), base));
            // The model its agent runs (`opus 5.5`), once the agent has said.
            if let Some(m) = s.model.as_deref().filter(|_| s.kind == SessionKind::Agent) {
                left.push(Span::styled(
                    format!(" {}", ptyro_core::session::model_label(m)),
                    base.fg(app.depth.fit(t.accent)),
                ));
            }
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
            // Its title, else the program's window title.
            let window = app.caches.get(&s.id).and_then(|c| c.title.as_deref());
            if let Some(title) =
                session_title(s).or(window).filter(|t| !t.is_empty() && *t != s.name)
            {
                left.push(Span::styled(format!(" · {title}"), base.fg(app.depth.fit(t.dim))));
            }
            if let Some(cache) = app.caches.get(&s.id).filter(|_| !s.pad) {
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
    } else if let Some(hint) = app.context_hint() {
        // What you'd do next with what's selected (UX1.16).
        left.push(Span::styled(hint, bar.fg(app.depth.fit(t.accent))));
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
    // What the doctor found (UX1.20): `M` mends it.
    if !app.health.findings.is_empty() {
        let bad = app.health.findings.iter().any(|f| f.problem);
        right.push(Span::styled(
            format!("⚕ {} ", app.health.findings.len()),
            bar.fg(app.depth.fit(if bad { t.error } else { t.blocked }))
                .add_modifier(Modifier::BOLD),
        ));
    }
    let gates = app.open_gates();
    if gates > 0 {
        right.push(Span::styled(
            format!("⏸ {gates} gate(s) "),
            bar.fg(app.depth.fit(t.blocked)).add_modifier(Modifier::BOLD),
        ));
    }
    if app.pad_open > 0 {
        right.push(Span::styled(
            format!("✎ {} ", app.pad_open),
            bar.fg(app.depth.fit(t.accent)).add_modifier(Modifier::BOLD),
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
    // The left side (the mode, a message, the prefix hint) keeps room to be read: on a narrow
    // terminal the right side gives up its first, least urgent items (workspace, profile, limits)
    // and keeps the counts and the clock.
    let min_left = 24u16.min(r.width / 2);
    let mut used: u16 = right.iter().map(|s| s.width() as u16).sum();
    while used > r.width.saturating_sub(min_left) && right.len() > 1 {
        used -= right.remove(0).width() as u16;
    }
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
        (&["session.new"], "task"),
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

/// How wide a line of text can be in a box that asked for `want` columns, `chrome` of them
/// borders and padding: `boxed` squeezes a box on a narrow terminal, and text wrapped for the
/// width it asked for would run out of it.
pub(crate) fn fit_width(area: Rect, want: u16, chrome: u16) -> usize {
    want.min(area.width.saturating_sub(4)).saturating_sub(chrome).max(8) as usize
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
        Overlay::Settings(p) => crate::settings::draw(app, p, f, area),
        Overlay::Help { filter, scroll } => {
            let r = boxed(area, 88, area.height.saturating_sub(4));
            f.render_widget(Clear, r);
            let lines = help_lines(app, filter);
            // (Only what has a key is listed; the palette, `C-q :`, runs the rest.)
            let total = app.keymap.bindings().len();
            let title = if filter.is_empty() {
                format!(
                    "Keys ({total} bindings) — type to filter, Esc closes; the rest of the {} actions are in the palette",
                    ACTIONS.len()
                )
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
                // Display columns, not characters: a wide name takes two.
                let room = inner_w.saturating_sub(UnicodeWidthStr::width(key.as_str()) + 2);
                if UnicodeWidthStr::width(text.as_str()) > room {
                    truncate_width(&mut text, room.saturating_sub(1));
                    text.push('…');
                }
                let pad = inner_w.saturating_sub(
                    UnicodeWidthStr::width(text.as_str())
                        + UnicodeWidthStr::width(key.as_str())
                        + 1,
                );
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
                r.x + 3 + UnicodeWidthStr::width(palette.query.as_str()) as u16,
                r.y + 1,
            ));
        }
        Overlay::Suggest { suggestions: s, .. } => {
            let mut lines = vec![
                Line::styled(
                    format!("{}'s first worktree. ptyro found what it needs:", s.project),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
            ];
            if let Some(cmd) = &s.setup {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {} → setup: ", s.from.as_deref().unwrap_or("?")),
                        fg(app, t.dim),
                    ),
                    Span::styled(cmd.clone(), fg(app, t.accent)),
                ]));
            }
            if !s.copy.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} → ", s.copy.join(", ")), fg(app, t.dim)),
                    Span::styled("copied into each worktree (git ignores them)", fg(app, t.accent)),
                ]));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Written to .ptyro.local.toml: yours, not committed. Asked only once.",
                fg(app, t.dim),
            ));
            lines.push(Line::styled(
                "y use them · n not now (and not again) · Esc cancels",
                fg(app, t.accent),
            ));
            let r = boxed(area, 84, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(block("Set up worktrees?".into())), r);
        }
        Overlay::Task { text, name, naming, wizard } => {
            let (do_rows, do_cursor) = edit_rows(
                text,
                "   do ",
                fit_width(area, 76, 4),
                (area.height / 3).max(1) as usize,
            );
            let do_h = if text.is_empty() { 1 } else { do_rows.len() as u16 };
            let r = boxed(area, 76, do_h + 5);
            f.render_widget(Clear, r);
            let (slug, agent, place) = app.task_plan(text, name.as_deref(), wizard);
            let caret = fg(app, t.accent);
            let field = |label: &str, value: &str, on: bool, placeholder: &str| {
                let mut v = vec![Span::styled(
                    format!("{label:>5} "),
                    if on { caret.add_modifier(Modifier::BOLD) } else { fg(app, t.dim) },
                )];
                if value.is_empty() {
                    v.push(Span::styled(placeholder.to_owned(), fg(app, t.dim)));
                } else {
                    v.push(Span::raw(value.to_owned()));
                }
                Line::from(v)
            };
            let shown_name = name.clone().or_else(|| slug.clone()).unwrap_or_default();
            let bad =
                name.as_deref().filter(|n| !n.is_empty()).and_then(ptyro_core::names::problem);
            let preview = match (&bad, text.trim().is_empty() && name.is_none()) {
                (Some(e), _) => Line::styled(format!("✗ name: {e}"), fg(app, t.error)),
                (None, true) => {
                    Line::styled(format!("→ {agent} · {place} · a random name"), fg(app, t.dim))
                }
                (None, false) => Line::styled(
                    format!(
                        "→ {} · {agent} · {place}",
                        if shown_name.is_empty() {
                            "a random name".into()
                        } else {
                            shown_name.clone()
                        }
                    ),
                    fg(app, t.done),
                ),
            };
            let what: Vec<Line<'_>> = if text.is_empty() {
                vec![field("do", text, !naming, "fix the flaky test in tests/api.rs")]
            } else {
                let on = if *naming { fg(app, t.dim) } else { caret.add_modifier(Modifier::BOLD) };
                do_rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| match i {
                        0 if row.starts_with("   do ") => Line::from(vec![
                            Span::styled("   do ", on),
                            Span::raw(row["   do ".len()..].to_owned()),
                        ]),
                        _ => Line::raw(row.clone()),
                    })
                    .collect()
            };
            let mut lines = what;
            lines.extend([
                field(
                    "name",
                    name.as_deref().unwrap_or(""),
                    *naming,
                    slug.as_deref().unwrap_or("(from what you type)"),
                ),
                preview,
                Line::styled(
                    format!(
                        "⏎ start · {} · Tab name · C-o pick agent & where · Esc",
                        newline_hint(app)
                    ),
                    fg(app, t.dim),
                ),
            ]);
            // Which profile it runs under, and why (SPEC §10.10), as the wizard says it.
            let profile =
                app.resolved.as_deref().map(|p| format!(" · profile {p}")).unwrap_or_default();
            f.render_widget(
                Paragraph::new(lines).block(block(format!("What should it do?{profile}"))),
                r,
            );
            let (col, row) = if *naming {
                (7 + name.as_deref().unwrap_or("").chars().count() as u16, 1 + do_h)
            } else if text.is_empty() {
                (7, 1)
            } else {
                (1 + do_cursor.0, 1 + do_cursor.1)
            };
            f.set_cursor_position(Position::new(r.x + col, r.y + row));
        }
        Overlay::Prompt { title, text, kind } => {
            // Checked as it's typed: characters a name can't hold are marked where they are.
            let check = app.prompt_check(kind, text);
            let bad = fg(app, t.error).add_modifier(Modifier::UNDERLINED);
            let simple = !(kind.is_name() && check.is_err())
                && !(text.is_empty() && kind.suggestion().is_some());
            let (rows, cursor) =
                edit_rows(text, "› ", fit_width(area, 70, 4), (area.height / 3).max(1) as usize);
            let rows_h = if simple { rows.len() as u16 } else { 1 };
            let r = boxed(area, 70, rows_h + 3);
            f.render_widget(Clear, r);
            let mut field = vec![Span::styled("› ", fg(app, t.accent))];
            if kind.is_name() && check.is_err() {
                field.extend(text.chars().map(|c| {
                    let style = if ptyro_core::names::allowed(c) { Style::new() } else { bad };
                    Span::styled(c.to_string(), style)
                }));
            } else if text.is_empty()
                && let Some(s) = kind.suggestion()
            {
                // The name it offers, dimmed: ⏎ takes it, typing replaces it (UX1.24).
                field.push(Span::styled(s.to_owned(), fg(app, t.dim)));
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
                Ok(note) => {
                    let nl = if kind.multiline() {
                        format!(" · {}", newline_hint(app))
                    } else if kind.is_name() {
                        " · a space or other odd character becomes _ (a-z 0-9 . _ -)".to_owned()
                    } else {
                        String::new()
                    };
                    Line::styled(
                        match (note, text.is_empty().then(|| kind.suggestion()).flatten()) {
                            (_, Some(s)) => format!("⏎ takes {s} · or type another · Esc cancel"),
                            (Some(n), _) => format!("⏎ ok ({n}){nl} · Esc cancel · C-u clear"),
                            (None, _) => format!("⏎ ok{nl} · Esc cancel · C-u clear"),
                        },
                        fg(app, t.dim),
                    )
                }
            };
            let mut lines: Vec<Line<'_>> = if simple {
                rows.iter()
                    .enumerate()
                    .map(|(i, row)| match i {
                        0 if row.starts_with("› ") => Line::from(vec![
                            Span::styled("› ", fg(app, t.accent)),
                            Span::raw(row["› ".len()..].to_owned()),
                        ]),
                        _ => Line::raw(row.clone()),
                    })
                    .collect()
            } else {
                vec![Line::from(field)]
            };
            lines.push(help);
            f.render_widget(Paragraph::new(lines).block(block(title.clone())), r);
            let (col, row) = if simple { cursor } else { (2 + text.chars().count() as u16, 0) };
            f.set_cursor_position(Position::new(r.x + 1 + col, r.y + 1 + row));
        }
        Overlay::Confirm { text, .. } => {
            let widest = text.lines().map(UnicodeWidthStr::width).max().unwrap_or(0) as u16;
            // Wrapped to the box (it is narrower than the text on a small terminal, and the
            // `[y/N]` at the end must stay in view): as many rows as the wrapped lines take.
            let inner =
                widest.saturating_add(6).min(area.width.saturating_sub(4)).saturating_sub(2).max(8)
                    as usize;
            let rows: usize =
                text.lines().map(|l| UnicodeWidthStr::width(l).div_ceil(inner).max(1)).sum();
            let r = boxed(area, widest + 6, rows as u16 + 2);
            f.render_widget(Clear, r);
            let lines: Vec<Line<'_>> = text.lines().map(|l| Line::raw(l.to_owned())).collect();
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(block("Confirm".into())),
                r,
            );
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
                if UnicodeWidthStr::width(text.as_str()) > width {
                    truncate_width(&mut text, width.saturating_sub(1));
                    text.push('…');
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
                "Inbox · {name} — ⏎ go · p peek · n note it · r read · R all read · Tab next tab · Esc"
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
            let (reply_rows, reply_cursor) = edit_rows(
                reply,
                "› ",
                r.width.saturating_sub(2) as usize,
                (area.height / 3).max(1) as usize,
            );
            let input_h = reply_rows.len() as u16 + 1;
            let body_h = r.height.saturating_sub(5 + input_h) as usize;
            let shown: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
            for l in &shown[shown.len().saturating_sub(body_h)..] {
                lines.push(Line::styled(format!(" {l}"), fg(app, t.dim)));
            }
            let blocked = info.and_then(|s| s.status.as_ref()).is_some_and(|r| {
                r.status == ptyro_core::status::AgentStatus::Blocked
                    && r.kind.as_deref() == Some("permission")
            });
            let hint = if blocked && reply.is_empty() {
                "y allow · a always · n deny · type a reply · ⏎ open · Esc".to_owned()
            } else {
                format!(
                    "type a reply, ⏎ sends it, {} · empty ⏎ opens the session · Esc",
                    newline_hint(app)
                )
            };
            f.render_widget(Paragraph::new(lines).block(block(format!("Peek {name}"))), r);
            let input = Rect {
                y: r.bottom().saturating_sub(1 + input_h),
                height: input_h,
                x: r.x + 1,
                width: r.width.saturating_sub(2),
            };
            let mut field: Vec<Line<'_>> = reply_rows
                .iter()
                .enumerate()
                .map(|(i, row)| match i {
                    0 if row.starts_with("› ") => Line::from(vec![
                        Span::styled("› ", fg(app, t.accent)),
                        Span::raw(row["› ".len()..].to_owned()),
                    ]),
                    _ => Line::raw(row.clone()),
                })
                .collect();
            field.push(Line::styled(hint, fg(app, t.dim)));
            f.render_widget(Paragraph::new(field), input);
            f.set_cursor_position(Position::new(
                input.x + reply_cursor.0,
                input.y + reply_cursor.1,
            ));
        }
        Overlay::ReviewQueue { items, selected } => {
            let r =
                boxed(area, 100, (items.len() as u16 * 2 + 4).min(area.height.saturating_sub(2)));
            f.render_widget(Clear, r);
            let mut lines: Vec<Line<'_>> = Vec::new();
            // Two lines an item: scroll so the selected one is always in view.
            let per = (r.height.saturating_sub(2) / 2).max(1) as usize;
            let first = (*selected + 1).saturating_sub(per);
            for (n, it) in items.iter().enumerate().skip(first).take(per) {
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
        Overlay::About { doc, sel, scroll } => {
            let Some(d) = doc else {
                let r = boxed(area, 60, 3);
                f.render_widget(Clear, r);
                f.render_widget(
                    Paragraph::new(Line::styled(" Looking…", fg(app, t.dim)))
                        .block(block("About ptyro".into())),
                    r,
                );
                return;
            };
            if d["failed"] == true {
                let r = boxed(area, 70, 4);
                f.render_widget(Clear, r);
                f.render_widget(
                    Paragraph::new(Line::styled(
                        " `ptyro about` did not answer: run it in a terminal to see why.",
                        fg(app, t.error),
                    ))
                    .block(block("About ptyro — Esc closes".into())),
                    r,
                );
                return;
            }
            let rows = crate::about::rows(d);
            let picks = crate::about::picks(&rows);
            let sel_row = picks.get(*sel).copied();
            let (all, _) = crate::about::lines(app, &rows, sel_row, 0, 0, 0, usize::MAX);
            let r = boxed(area, 110, all.len() as u16 + 4);
            f.render_widget(Clear, r);
            let inner_h = r.height.saturating_sub(4) as usize;
            let start = (*scroll).min(all.len().saturating_sub(inner_h));
            let (lines, links) =
                crate::about::lines(app, &rows, sel_row, start, r.x + 1, r.y + 1, inner_h);
            let mismatch = d["daemon"]["same"] == false;
            let footer = format!(
                " j/k pick · ⏎ open · y copy it · c copy all, for a bug report{} · Esc",
                if mismatch { " · U switch the daemon to this version" } else { "" }
            );
            let mut shown: Vec<Line<'_>> = lines.into_iter().skip(start).take(inner_h).collect();
            while shown.len() < inner_h {
                shown.push(Line::raw(""));
            }
            shown.push(Line::styled(footer, fg(app, t.dim)));
            f.render_widget(Paragraph::new(shown).block(block("About ptyro".into())), r);
            *app.links.borrow_mut() = links;
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
                    "y trust (asks again if they change) · n go on without them · Esc cancel · j/k scroll",
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
            format!(
                "── prefix: {} (twice: the sidebar; {}: send it to the program) ──",
                prefixes.join(" or "),
                app.keymap
                    .key_in(Table::Prefix, "prefix.send")
                    .unwrap_or_else(|| "prefix.send".into())
            ),
            fg(app, t.dim),
        ));
        lines.push(Line::styled(
            format!(
                "── change any of these: {} {} (Settings → Key bindings) ──",
                key_label(&app.keymap.prefix),
                app.keymap.key_for("config.panel").map_or_else(
                    || "the palette: setting keys".to_owned(),
                    |k| k
                        .strip_prefix(&format!("{} ", key_label(&app.keymap.prefix)))
                        .unwrap_or(&k)
                        .to_owned()
                )
            ),
            fg(app, t.dim),
        ));
        lines.push(Line::styled(
            "── a worktree's row: ↑2 two commits not pushed · not pushed ↑2 no upstream yet ──"
                .to_owned(),
            fg(app, t.dim),
        ));
        lines.push(Line::styled(
            "──   ● 3 three files not committed · ✓ pushed all done · +N −N only while work is left ──".to_owned(),
            fg(app, t.dim),
        ));
        lines.push(Line::styled(
            format!(
                "── ptyro {} (protocol {}.{}) · {} About: version, links, a bug report to copy ──",
                env!("CARGO_PKG_VERSION"),
                ptyro_protocol::PROTO.major,
                ptyro_protocol::PROTO.minor,
                app.keymap.key_for("app.about").unwrap_or_else(|| "the palette: about".to_owned()),
            ),
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

/// Text being typed, laid out for a box `width` columns wide (UX1.26): split at its new lines
/// and wrapped, each row behind `first` (the first row) or as many spaces, at most `max_rows`
/// rows, scrolled so the end (where the cursor is) stays in view. Returns the rows and the
/// cursor's column and row within them.
pub(crate) fn edit_rows(
    text: &str,
    first: &str,
    width: usize,
    max_rows: usize,
) -> (Vec<String>, (u16, u16)) {
    use unicode_width::UnicodeWidthChar;
    let indent = unicode_width::UnicodeWidthStr::width(first);
    let room = width.saturating_sub(indent).max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut col = 0;
    for line in text.split('\n') {
        let mut row = String::new();
        col = 0;
        for c in line.chars() {
            let w = c.width().unwrap_or(0);
            if col + w > room {
                rows.push(std::mem::take(&mut row));
                col = 0;
            }
            row.push(c);
            col += w;
        }
        rows.push(row);
    }
    // The cursor sits after the text; on a full row it starts the next one.
    if col >= room {
        rows.push(String::new());
        col = 0;
    }
    let skip = rows.len().saturating_sub(max_rows.max(1));
    let pad = " ".repeat(indent);
    let shown: Vec<String> = rows
        .into_iter()
        .enumerate()
        .skip(skip)
        .map(|(i, r)| format!("{}{r}", if i == 0 { first } else { &pad }))
        .collect();
    let cursor = ((indent + col) as u16, shown.len().saturating_sub(1) as u16);
    (shown, cursor)
}

/// The key that starts a new line, as this terminal can send it.
pub(crate) fn newline_hint(app: &App) -> &'static str {
    if app.kitty { "⇧⏎ new line" } else { "C-j new line" }
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_text_wraps_splits_at_new_lines_and_keeps_its_end_in_view() {
        use super::edit_rows;
        // One row, cursor after it.
        assert_eq!(edit_rows("hi", "› ", 10, 3), (vec!["› hi".into()], (4, 0)));
        // New lines split; later rows are indented under the first.
        let (rows, cur) = edit_rows("one\ntwo", "› ", 10, 3);
        assert_eq!(rows, ["› one", "  two"]);
        assert_eq!(cur, (5, 1));
        // A row fills (8 columns of room) and wraps; a full last row puts the cursor on the next.
        let (rows, cur) = edit_rows("abcdefghij", "› ", 10, 3);
        assert_eq!(rows, ["› abcdefgh", "  ij"]);
        assert_eq!(cur, (4, 1));
        let (rows, cur) = edit_rows("abcdefgh", "› ", 10, 3);
        assert_eq!(rows, ["› abcdefgh", "  "]);
        assert_eq!(cur, (2, 1));
        // Too many rows: the end stays in view. Wide characters count two columns.
        let (rows, cur) = edit_rows("1\n2\n3\n4", "› ", 10, 2);
        assert_eq!(rows, ["  3", "  4"]);
        assert_eq!(cur, (3, 1));
        assert_eq!(edit_rows("日本語日本", "› ", 10, 3).0, ["› 日本語日", "  本"]);
    }

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
