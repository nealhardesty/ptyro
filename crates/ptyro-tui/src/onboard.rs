//! The first run and home (ROADMAP UX1.19): what `ptyro init` asks, as a screen of checkboxes
//! the first time `ptyro` opens with no config; and home, one list of what to do next (who needs
//! you, PRs, recent projects, repositories found) when there's nothing to work on yet.
//!
//! The first run's first part is the setup checklist (BOOT2, `setup_list.rs`): the agents and
//! tools to install. The same screen with only that part is `app.setup` (`U`).

use std::path::{Path, PathBuf};

use ptyro_agents::setup::WslStatus;
use ptyro_core::config::{Config, expand_home};
use ptyro_core::input::{KeyCode, KeyEvent};
use ptyro_protocol::Request;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{AfterAdd, App, Offer, Overlay, Pending, Purpose, Then};
use crate::keymap::Command;
use crate::palette::{Item, Palette, Target};
use crate::setup_list::{Found, Setup};
use crate::ui::fg;

/// One line of the first-run screen that can be ticked.
#[derive(Debug, Clone)]
pub struct Choice {
    pub section: &'static str,
    pub label: String,
    pub detail: String,
    pub on: bool,
    pub kind: ChoiceKind,
}

#[derive(Debug, Clone)]
pub enum ChoiceKind {
    /// A `[profiles.<name>]` table found in your shell setup.
    Profile(String),
    Repo(PathBuf),
    Desktop,
}

#[derive(Debug, Clone, Default)]
pub struct FirstRun {
    /// The setup checklist: agents and tools to install (BOOT2).
    pub setup: Setup,
    /// Opened for the checklist alone (`app.setup`, a health fix): no accounts, repositories or
    /// notifications, and nothing written to the config.
    pub only_setup: bool,
    pub choices: Vec<Choice>,
    /// Over the checklist's rows, then the choices.
    pub sel: usize,
}

impl FirstRun {
    /// How many of the rows are the checklist's.
    fn setup_rows(&self) -> usize {
        self.setup.rows().len()
    }
}

/// Skip the first-run screen (scripted setups): `PTYRO_FIRST_RUN=0`.
fn first_run_wanted(paths: &ptyro_core::paths::Paths) -> bool {
    !Config::path(paths).exists() && std::env::var_os("PTYRO_FIRST_RUN").is_none_or(|v| v != "0")
}

impl App {
    /// After attaching: `ptyro` had to start the first session (`Options::welcome`). With no
    /// config yet, the first run; else, with no project here, home.
    pub fn welcome(&mut self) {
        if !self.opts.welcome || self.overlay.is_some() {
            return;
        }
        if first_run_wanted(&self.paths) {
            self.open_first_run();
        } else if self.tree().project_ids().is_empty() {
            self.open_home();
        }
    }

    pub fn open_first_run(&mut self) {
        let home = expand_home("~");
        let mut choices: Vec<Choice> =
            ptyro_agents::detect::detect(&home, &ptyro_core::config::config_home(&home))
                .into_iter()
                .filter(|s| !self.config.profiles.contains_key(&s.profile))
                .map(|s| Choice {
                    section: "accounts",
                    label: format!("profile {}", s.profile),
                    detail: s
                        .dirs
                        .iter()
                        .map(|(a, d)| format!("{a} {d}"))
                        .chain(s.git_email.iter().map(|e| format!("git {e}")))
                        .collect::<Vec<_>>()
                        .join(" · "),
                    on: true,
                    kind: ChoiceKind::Profile(s.to_toml()),
                })
                .collect();
        choices.push(Choice {
            section: "notifications",
            label: "desktop notifications".into(),
            detail: "when an agent needs you (the inbox and your terminal always tell you)".into(),
            on: false,
            kind: ChoiceKind::Desktop,
        });
        let setup = self.setup_look(None);
        self.first_run = Some(FirstRun { setup, only_setup: false, choices, sel: 0 });
        // The repositories join once discovery answers.
        self.request(Pending::FirstRunRepos, Request::Discover { refresh: false });
        self.dirty = true;
    }

    pub(crate) fn first_run_repos(&mut self, paths: Vec<PathBuf>) {
        let here = std::env::current_dir().ok();
        let known: Vec<PathBuf> = self.projects.iter().map(|p| p.root.clone()).collect();
        let Some(fr) = &mut self.first_run else { return };
        // Before the notifications choice, which stays last.
        let at = fr.choices.len().saturating_sub(1);
        let repos: Vec<Choice> = paths
            .into_iter()
            .filter(|p| !known.contains(p))
            .take(30)
            .map(|p| Choice {
                section: "repositories",
                label: p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                detail: p.display().to_string(),
                // The one you're in registers on its own; the rest is yours to pick.
                on: here.as_deref().is_some_and(|h| h.starts_with(&p)),
                kind: ChoiceKind::Repo(p),
            })
            .collect();
        fr.choices.splice(at..at, repos);
        self.dirty = true;
    }

    pub(crate) fn first_run_key(&mut self, key: KeyEvent) {
        let Some(fr) = &mut self.first_run else { return };
        let ns = fr.setup_rows();
        let n = ns + fr.choices.len();
        match key.code {
            KeyCode::Down | KeyCode::Char { c: 'j' } if n > 0 => fr.sel = (fr.sel + 1) % n,
            KeyCode::Up | KeyCode::Char { c: 'k' } if n > 0 => fr.sel = (fr.sel + n - 1) % n,
            KeyCode::Left | KeyCode::Char { c: 'h' } => fr.setup.side_step(-1),
            KeyCode::Right | KeyCode::Char { c: 'l' } => fr.setup.side_step(1),
            KeyCode::Char { c: ' ' } if fr.sel < ns => fr.setup.toggle(fr.sel),
            KeyCode::Char { c: ' ' } => {
                if let Some(c) = fr.choices.get_mut(fr.sel - ns) {
                    c.on = !c.on;
                }
            }
            KeyCode::Enter => {
                let fr = self.first_run.take().unwrap_or_default();
                if !fr.only_setup {
                    self.finish_first_run(&fr.choices);
                }
                self.setup_go(&fr.setup, fr.only_setup);
            }
            // Not now: no installs (the first run's other choices still count).
            KeyCode::Char { c: 's' } => {
                let fr = self.first_run.take().unwrap_or_default();
                if fr.only_setup {
                    self.toast(self.setup_later());
                } else {
                    self.finish_first_run(&fr.choices);
                }
            }
            KeyCode::Esc => {
                let only = self.first_run.take().is_some_and(|fr| fr.only_setup);
                if !only {
                    // A config, even an empty one, so this screen doesn't come back.
                    self.finish_first_run(&[]);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    // ── the setup checklist (BOOT2) ─────────────────────────────────────────

    /// `app.setup` (`U`), a health fix: the checklist on its own; `want` ticks those (item names,
    /// as `ptyro install` takes them) instead of the defaults. With a daemon on another computer,
    /// that computer's `ptyro setup`, in a split there.
    pub fn open_setup(&mut self, want: Option<Vec<String>>) {
        if crate::setup_list::elsewhere(self.connect.remote.as_deref(), cfg!(windows)).is_some() {
            return self.start_cli("app.setup");
        }
        if self.first_run.is_some() {
            return;
        }
        let want = want.map(|names| {
            names
                .iter()
                .filter_map(|n| ptyro_agents::setup::resolve(n).ok().map(|i| i.id.clone()))
                .collect()
        });
        let setup = self.setup_look(want);
        self.overlay = None;
        self.first_run = Some(FirstRun { setup, only_setup: true, choices: Vec::new(), sel: 0 });
        self.dirty = true;
    }

    /// Start looking at what's installed (in the background); the checklist fills in once it
    /// answers.
    fn setup_look(&mut self, want: Option<Vec<String>>) -> Setup {
        let remote = self.connect.remote.clone();
        if let Some(r) = crate::setup_list::elsewhere(remote.as_deref(), cfg!(windows)) {
            return Setup { found: Found::Elsewhere(r), ..Setup::looking(want) };
        }
        let (paths, tx) = (self.paths.clone(), self.tx.clone());
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                let found = crate::setup_list::look(paths, remote).await;
                let _ = tx.send(crate::app::AppMsg::SetupFound(found.map(Box::new)));
            });
        }
        Setup::looking(want)
    }

    /// What setup found, for the checklist on screen.
    pub(crate) fn setup_found(&mut self, found: Option<Box<crate::app::SetupLook>>) {
        let Some(fr) = &mut self.first_run else { return };
        if !matches!(fr.setup.found, Found::Looking) {
            return;
        }
        match found {
            Some(f) => {
                let (sides, place, note) = *f;
                fr.setup.found(sides, place, note);
            }
            None => fr.setup.found = Found::Failed,
        }
        // The checklist's rows come first now: the cursor on the first of them.
        fr.sel = 0;
        self.dirty = true;
    }

    /// ⏎: what's ticked installs in a split (`ptyro install … --confirmed`), a real terminal:
    /// live output, sudo's prompt, then the logins and the profiles line.
    fn setup_go(&mut self, setup: &Setup, only: bool) {
        let wsl_daemon =
            crate::cli_actions::where_offered(cfg!(windows), self.connect.remote.as_deref())
                == Some(true);
        match setup.commands(wsl_daemon, crate::cli_actions::exe_in_wsl().as_deref()) {
            Ok(runs) if runs.is_empty() => {
                if only {
                    self.toast(format!("nothing ticked to install; {}", self.setup_later()));
                }
            }
            Ok(runs) => {
                for (bin, args) in runs {
                    match bin {
                        Some(bin) => self.run_cli_with(bin, &args),
                        None => self.run_cli(&args),
                    }
                }
            }
            Err(e) => self.toast_err(e),
        }
    }

    /// How to come back to the checklist: its key, or the palette.
    fn setup_later(&self) -> String {
        match self.keymap.key_for("app.setup") {
            Some(k) => format!("{k} opens the setup checklist again"),
            None => "the palette's app.setup opens the setup checklist again".to_owned(),
        }
    }

    /// Write the config (profiles, notifications), register the repositories, offer the tour.
    fn finish_first_run(&mut self, choices: &[Choice]) {
        let path = Config::path(&self.paths);
        let on = |k: fn(&ChoiceKind) -> bool| choices.iter().filter(move |c| c.on && k(&c.kind));
        let profiles: Vec<String> = on(|k| matches!(k, ChoiceKind::Profile(_)))
            .filter_map(|c| match &c.kind {
                ChoiceKind::Profile(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        let desktop = on(|k| matches!(k, ChoiceKind::Desktop)).next().is_some();
        match write_first_config(&path, &profiles, desktop) {
            Ok(()) => self.request(Pending::Done(String::new()), Request::ReloadConfig),
            Err(e) => self.toast_err(format!("{}: {e}", path.display())),
        }
        let repos: Vec<PathBuf> = on(|k| matches!(k, ChoiceKind::Repo(_)))
            .filter_map(|c| match &c.kind {
                ChoiceKind::Repo(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        for path in &repos {
            let workspace = Some(self.workspace.clone());
            self.request(
                Pending::Done(String::new()),
                Request::ProjectAdd { path: path.clone(), workspace },
            );
        }
        let did = match (profiles.len(), repos.len()) {
            (0, 0) => "set up".to_owned(),
            (p, r) => format!("set up: {p} profile(s), {r} project(s)"),
        };
        self.toast_offer(
            did,
            false,
            Offer {
                label: "take the tour".into(),
                then: Then::Run(Command { action: "help.tour", arg: None }),
            },
        );
    }

    // ── home ────────────────────────────────────────────────────────────────

    /// Home (`H`): what to do next, in one list. Discovery fills in the repositories.
    pub fn open_home(&mut self) {
        self.request(Pending::Home, Request::Discover { refresh: false });
    }

    pub(crate) fn show_home(&mut self, discovered: Vec<PathBuf>) {
        let mut items: Vec<Item> = Vec::new();
        if self.pad_open > 0 {
            items.push(Item {
                label: format!("✎ scratchpad · {} open", self.pad_open),
                detail: "search the items, tick one, start one as a task".into(),
                key: self.keymap.key_for("scratchpad.items"),
                target: Target::Action(Command { action: "scratchpad.items", arg: None }),
            });
        }
        for id in self.attention_order(true) {
            let Some(s) = self.session(id) else { continue };
            let why = s.attention.as_ref().map(|a| a.text.clone()).unwrap_or_default();
            items.push(Item {
                label: format!("⚑ {}", s.name),
                detail: why,
                key: None,
                target: Target::Session(id),
            });
        }
        for w in self.worktrees.iter().filter(|w| w.pr.is_some()) {
            let pr = w.pr.as_ref().expect("filtered");
            let (pass, fail, pending) = pr.tally();
            let checks = match (fail, pending, pass) {
                (f, _, _) if f > 0 => format!("✗{f} failing"),
                (_, p, _) if p > 0 => "checks running".into(),
                (_, _, p) if p > 0 => "checks pass".into(),
                _ => pr.state.to_lowercase(),
            };
            items.push(Item {
                label: format!("#{} {}", pr.number, w.qualified()),
                detail: format!("{} · {checks}", pr.title),
                key: None,
                target: Target::Worktree(w.id.clone()),
            });
        }
        let mut projects = self.projects.clone();
        projects.sort_by_key(|p| std::cmp::Reverse(p.last_used_ms.unwrap_or(p.created_ms)));
        for p in &projects {
            items.push(Item {
                label: format!("▸ {}", p.slug),
                detail: format!("start a task here · {}", p.root.display()),
                key: None,
                target: Target::Project(p.id.clone()),
            });
        }
        let known: Vec<&Path> = self.projects.iter().map(|p| p.root.as_path()).collect();
        for path in discovered.iter().filter(|p| !known.contains(&p.as_path())).take(40) {
            items.push(Item {
                label: format!(
                    "+ {}",
                    path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default()
                ),
                detail: format!("add it, then start a task · {}", path.display()),
                key: None,
                target: Target::Path(path.clone()),
            });
        }
        items.push(Item {
            label: "a shell, here".into(),
            detail: self.here().display().to_string(),
            key: None,
            target: Target::NewShell,
        });
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Home: what next?", items),
            purpose: Purpose::Home,
        });
    }

    /// A home line picked.
    pub(crate) fn home_picked(&mut self, target: Target) {
        match target {
            Target::Project(id) => {
                let rows = self.rows();
                if let Some(i) =
                    rows.iter().position(|r| *r == crate::tree::Row::Project(id.clone()))
                {
                    self.set_sel(&rows, i);
                }
                self.focus = crate::app::Focus::Sidebar;
                self.run(&Command { action: "session.new", arg: None });
            }
            Target::Path(path) => {
                let workspace = Some(self.workspace.clone());
                self.request(
                    Pending::ProjectAdded(Some(AfterAdd::Run("session.new"))),
                    Request::ProjectAdd { path, workspace },
                );
            }
            other => self.picked(other, Purpose::Palette),
        }
    }
}

/// The config the first run writes: its comments, the chosen profiles, notifications.
fn write_first_config(path: &Path, profiles: &[String], desktop: bool) -> Result<(), String> {
    let mut text = String::from(
        "# ptyro configuration. Every key is optional; `ptyro config edit` explains them, and\n# changes apply live.\n",
    );
    if desktop {
        text.push_str(
            "\n[notifications]\nchannels = [\"inbox\", \"osc\", \"bell\", \"desktop\"]\n",
        );
    }
    for p in profiles {
        text.push('\n');
        text.push_str(p);
    }
    Config::parse(&text).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut f = ptyro_core::os::private(std::fs::OpenOptions::new().write(true).create_new(true))
        .open(path)
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| e.to_string())
}

pub fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(fr) = &app.first_run else { return };
    let t = &app.theme;
    let r = crate::ui::boxed(area, 110, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let bold = ratatui::style::Style::new().add_modifier(Modifier::BOLD);
    let head = if fr.only_setup {
        "Set up this computer: tick the agents and tools to install, then ⏎."
    } else {
        "Welcome to ptyro. Tick what to install and what to use, then ⏎."
    };
    let mut lines = vec![Line::styled(head, bold)];
    setup_lines(app, fr, r.width.saturating_sub(2) as usize, &mut lines);
    let ns = fr.setup_rows();
    let mut section = "";
    for (i, c) in fr.choices.iter().enumerate() {
        if c.section != section {
            section = c.section;
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(" {section}"),
                fg(app, t.dim).add_modifier(Modifier::ITALIC),
            ));
        }
        let mark = if c.on { "[x]" } else { "[ ]" };
        let mut style = ratatui::style::Style::new();
        if ns + i == fr.sel {
            style = style.bg(app.depth.fit(t.selection_bg));
        }
        lines.push(Line::from(vec![
            Span::styled(format!("   {mark} "), style.fg(app.depth.fit(t.accent))),
            Span::styled(c.label.clone(), style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", c.detail), style.fg(app.depth.fit(t.dim))),
        ]));
    }
    if !fr.only_setup && !fr.choices.iter().any(|c| c.section == "repositories") {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" repositories: looking…", fg(app, t.dim)));
    }
    lines.push(Line::raw(""));
    let steps = fr.setup.steps();
    let two = fr.setup.ready().is_some_and(|r| r.plan.sides().len() > 1);
    let side = if two { " · ←/→ side" } else { "" };
    let keys = if fr.only_setup {
        let go = if steps > 0 { format!("⏎ install {steps}") } else { "⏎ done".to_owned() };
        format!(" {go} · Space toggle · j/k move{side} · s not now")
    } else {
        let go = if steps > 0 {
            format!("⏎ install {steps} and use these")
        } else {
            "⏎ use these".to_owned()
        };
        format!(
            " {go} · Space ticks · j/k move{side} · s not now (installs nothing) · Esc skip   (the repository you're in is added on its own)"
        )
    };
    lines.push(Line::styled(keys, fg(app, t.accent)));
    let title = if fr.only_setup { " Setup " } else { " First run " };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(app, t.border_focus))
        .title(Span::styled(title, fg(app, t.accent).add_modifier(Modifier::BOLD)));
    // Keep the selection in view.
    let inner_h = r.height.saturating_sub(2) as usize;
    let sel_line = lines
        .iter()
        .position(|l| l.style.bg.is_some() || l.spans.iter().any(|s| s.style.bg.is_some()))
        .unwrap_or(0);
    let skip = (sel_line + 3).saturating_sub(inner_h);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()).block(block),
        r,
    );
}

/// The checklist's part (BOOT2): Agents and Tools to tick, each with what it is and how it
/// installs (a column per side on Windows with WSL), then Already here, then what WSL is.
fn setup_lines(app: &App, fr: &FirstRun, width: usize, lines: &mut Vec<Line<'static>>) {
    use crate::setup_list::{SECTIONS, about, cells, heads, programs_of};
    use ptyro_agents::setup::checklist::{already, mark};
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let italic = dim.add_modifier(Modifier::ITALIC);
    let r = match &fr.setup.found {
        Found::Ready(r) => r,
        Found::Looking => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(" agents and tools: looking at what's installed…", dim));
            return;
        }
        Found::Failed => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                " agents and tools: couldn't look at what's installed; `ptyro setup` in a terminal says why",
                fg(app, t.blocked),
            ));
            return;
        }
        Found::Elsewhere(remote) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!(
                    " agents and tools: the daemon is on {remote}, so they're set up there: `ptyro setup` on that computer"
                ),
                dim,
            ));
            return;
        }
    };
    let plan = &r.plan;
    let sides = plan.sides();
    let two = sides.len() > 1;
    let rows = fr.setup.rows();
    const NAME: usize = 13;
    const CELL: usize = 24;
    let programs = programs_of(r, sides.first().copied().unwrap_or_default());
    for (title, kind) in SECTIONS {
        let these: Vec<(usize, &&ptyro_agents::setup::Entry)> =
            rows.iter().enumerate().filter(|(_, e)| e.kind == kind).collect();
        if these.is_empty() {
            continue;
        }
        lines.push(Line::raw(""));
        let mut head = vec![Span::styled(format!(" {title}"), italic)];
        if two {
            let pad = (NAME + 6).saturating_sub(title.len() + 1);
            head.push(Span::raw(" ".repeat(pad)));
            for h in heads(r) {
                head.push(Span::styled(format!("{h:<CELL$} "), italic));
            }
        }
        lines.push(Line::from(head));
        for (i, e) in these {
            let mut style = ratatui::style::Style::new();
            if i == fr.sel {
                style = style.bg(app.depth.fit(t.selection_bg));
            }
            let accent = style.fg(app.depth.fit(t.accent));
            let mut spans = Vec::new();
            if two {
                spans.push(Span::styled(
                    format!("   {:<NAME$}  ", e.name),
                    style.add_modifier(Modifier::BOLD),
                ));
                for (c, text) in cells(plan, &e.id).into_iter().enumerate() {
                    let mut st = accent;
                    if i == fr.sel && c == fr.setup.col {
                        st = st.add_modifier(Modifier::REVERSED);
                    }
                    spans.push(Span::styled(format!("{text:<CELL$}"), st));
                    spans.push(Span::styled(" ", style));
                }
            } else {
                spans.push(Span::styled(format!("   {} ", mark(e)), accent));
                spans.push(Span::styled(
                    format!("{:<NAME$}", e.name),
                    style.add_modifier(Modifier::BOLD),
                ));
            }
            // The line wraps between its parts, under itself, rather than run off the box.
            let at: usize = spans.iter().map(|s| s.content.chars().count()).sum::<usize>() + 1;
            let text = about(plan, e, &programs, two);
            let mut wrapped = fit(&text, width.saturating_sub(at).max(20)).into_iter();
            let detail = style.fg(app.depth.fit(t.dim));
            spans.push(Span::styled(format!(" {}", wrapped.next().unwrap_or_default()), detail));
            lines.push(Line::from(spans));
            for more in wrapped {
                lines.push(Line::from(Span::styled(format!("{}{more}", " ".repeat(at)), detail)));
            }
            if two {
                // What a side's cell can't say: why it can't, what needs it.
                for s in &sides {
                    let Some(x) = plan.entry_on(&e.id, *s) else { continue };
                    let mut why = Vec::new();
                    if let Some(b) = &x.blocked {
                        why.push(format!("can't: {b}"));
                    }
                    if !x.needed_by.is_empty() {
                        let by: Vec<&str> = x
                            .needed_by
                            .iter()
                            .map(|id| plan.entry(id).map_or(id.as_str(), |y| y.name.as_str()))
                            .collect();
                        why.push(format!("ticked because {} needs it", by.join(", ")));
                    }
                    if !why.is_empty() {
                        lines.push(Line::styled(
                            format!("       {}: {}", r.sides.name(*s), why.join("; ")),
                            dim,
                        ));
                    }
                }
            }
        }
    }
    let here = already(plan);
    if !here.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" Already here", italic));
        let default = ptyro_agents::default_agent(&app.config);
        for e in here {
            let each: Vec<String> = sides
                .iter()
                .filter_map(|s| {
                    let x = plan.entry_on(&e.id, *s)?;
                    let at = x.state.path().map(|p| format!(" ({p})")).unwrap_or_default();
                    let words = x.state.words();
                    Some(if two {
                        format!("{} {words}", r.sides.name(*s))
                    } else {
                        format!("{words}{at}")
                    })
                })
                .collect();
            let def = if e.id == default { " · your default agent" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("   ✓ {}", e.name), fg(app, t.done)),
                Span::styled(format!("  {}{def}", each.join("; ")), dim),
            ]));
        }
    }
    let wsl = match &plan.wsl {
        Some(w @ (WslStatus::Unusable { .. } | WslStatus::Inside { .. })) => Some(w.words()),
        _ => None,
    };
    for note in [wsl, r.note.clone()].into_iter().flatten() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!(" {note}"), dim));
    }
    if let Some(said) = &fr.setup.said {
        lines.push(Line::styled(format!(" {said}"), fg(app, t.blocked)));
    }
}

/// `text` (parts joined by ` · `) in lines of at most `width` characters, broken between
/// parts (a part longer than a line is a line of its own).
fn fit(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(" · ") {
        match out.last_mut() {
            Some(l) if l.chars().count() + 3 + part.chars().count() <= width => {
                l.push_str(" · ");
                l.push_str(part);
            }
            _ => out.push(part.to_owned()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_checklist_line_wraps_between_its_parts() {
        let lines = super::fit("Google's coding agent · the vendor's installer · then log in", 30);
        assert_eq!(lines, ["Google's coding agent", "the vendor's installer", "then log in"]);
        assert_eq!(super::fit("a · b", 30), ["a · b"]);
    }

    #[cfg(unix)] // file modes
    #[test]
    fn the_first_config_is_valid_private_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ptyro/config.toml");
        let work = "[profiles.work]\nclaude = \"~/.claude-work\"\n".to_owned();
        super::write_first_config(&path, &[work], true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[profiles.work]") && text.contains("\"desktop\""), "{text}");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(super::write_first_config(&path, &[], false).is_err(), "never over a config");
    }
}
