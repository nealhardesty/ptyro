//! The first run and home (ROADMAP UX1.19): what `ptyro init` asks, as a screen of checkboxes
//! the first time `ptyro` opens with no config; and home, one list of what to do next (who needs
//! you, PRs, recent projects, repositories found) when there's nothing to work on yet.

use std::path::{Path, PathBuf};

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
    /// Agents on PATH: (name, program, found).
    pub agents: Vec<(String, String, bool)>,
    pub choices: Vec<Choice>,
    pub sel: usize,
}

/// `prog` on `PATH`.
fn on_path(prog: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
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
        let agents = ptyro_agents::BUILTIN
            .iter()
            .map(|(n, p)| ((*n).to_owned(), (*p).to_owned(), on_path(p)))
            .collect();
        let home = expand_home("~");
        let mut choices: Vec<Choice> = ptyro_agents::detect::detect(&home)
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
        self.first_run = Some(FirstRun { agents, choices, sel: 0 });
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
        let n = fr.choices.len();
        match key.code {
            KeyCode::Down | KeyCode::Char { c: 'j' } if n > 0 => fr.sel = (fr.sel + 1) % n,
            KeyCode::Up | KeyCode::Char { c: 'k' } if n > 0 => fr.sel = (fr.sel + n - 1) % n,
            KeyCode::Char { c: ' ' } => {
                if let Some(c) = fr.choices.get_mut(fr.sel) {
                    c.on = !c.on;
                }
            }
            KeyCode::Enter => {
                let fr = self.first_run.take().unwrap_or_default();
                self.finish_first_run(&fr.choices);
            }
            KeyCode::Esc => {
                self.first_run = None;
                // A config, even an empty one, so this screen doesn't come back.
                self.finish_first_run(&[]);
            }
            _ => {}
        }
        self.dirty = true;
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
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| e.to_string())
}

pub fn draw(app: &App, f: &mut Frame<'_>, area: Rect) {
    let Some(fr) = &app.first_run else { return };
    let t = &app.theme;
    let r = crate::ui::boxed(area, 92, area.height.saturating_sub(2));
    f.render_widget(Clear, r);
    let mut lines = vec![
        Line::styled(
            "Welcome to ptyro. Here's what it found; tick what to use, then ⏎.",
            ratatui::style::Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(" agents", fg(app, t.dim).add_modifier(Modifier::ITALIC)),
    ];
    let default = ptyro_agents::default_agent(&app.config);
    let mut any = false;
    for (name, prog, found) in &fr.agents {
        if !found {
            continue;
        }
        any = true;
        let def = if *name == default { " (default)" } else { "" };
        lines.push(Line::styled(
            format!("   ✓ {name}{def}  `{prog}` on your PATH"),
            fg(app, t.done),
        ));
    }
    if !any {
        lines.push(Line::styled(
            "   none found: install Claude Code, Codex, Gemini CLI or OpenCode; shells work meanwhile",
            fg(app, t.blocked),
        ));
    }
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
        if i == fr.sel {
            style = style.bg(app.depth.fit(t.selection_bg));
        }
        lines.push(Line::from(vec![
            Span::styled(format!("   {mark} "), style.fg(app.depth.fit(t.accent))),
            Span::styled(c.label.clone(), style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", c.detail), style.fg(app.depth.fit(t.dim))),
        ]));
    }
    if !fr.choices.iter().any(|c| c.section == "repositories") {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" repositories: looking…", fg(app, t.dim)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " Space ticks · j/k move · ⏎ use these · Esc skip   (the repository you're in is added on its own)",
        fg(app, t.accent),
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(app, t.border_focus))
        .title(Span::styled(" First run ", fg(app, t.accent).add_modifier(Modifier::BOLD)));
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

#[cfg(test)]
mod tests {
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
