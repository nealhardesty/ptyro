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
        if self.setup_running() {
            return;
        }
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
            // Keep an agent installed another way as it is (BOOT7): no move, now or later.
            KeyCode::Char { c: 'K' } if fr.sel < ns => match fr.setup.keep(fr.sel) {
                Ok(Some((id, name, via))) => {
                    let path = ptyro_core::config::Config::path(&self.paths);
                    let value = keep_value(&path, &self.config.updates.keep_installer, &id);
                    self.set_config(
                        "updates.keep_installer",
                        &value,
                        &format!(
                            "{name} stays installed with {via}: it updates that way, and the move isn't offered again (updates.keep_installer)"
                        ),
                    );
                }
                Ok(None) => {}
                Err(why) => self.toast(why),
            },
            KeyCode::Char { c: ' ' } => {
                if let Some(c) = fr.choices.get_mut(fr.sel - ns) {
                    c.on = !c.on;
                }
            }
            KeyCode::Enter => {
                let fr = self.first_run.take().unwrap_or_default();
                // Nothing to install yet (still looking, or nothing ticked): say how to come back.
                let none = fr.setup.steps() == 0;
                if !fr.only_setup {
                    self.finish_first_run(&fr.choices, none);
                }
                self.setup_go(&fr.setup, fr.only_setup);
            }
            // Not now: no installs (the first run's other choices still count).
            KeyCode::Char { c: 's' } => {
                let fr = self.first_run.take().unwrap_or_default();
                if fr.only_setup {
                    self.toast(self.setup_later());
                } else {
                    self.finish_first_run(&fr.choices, true);
                }
            }
            KeyCode::Esc => {
                let only = self.first_run.take().is_some_and(|fr| fr.only_setup);
                if only {
                    self.toast(self.setup_later());
                } else {
                    // A config, even an empty one, so this screen doesn't come back.
                    self.finish_first_run(&[], true);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    // ── the setup checklist (BOOT2) ─────────────────────────────────────────

    /// `app.setup` (`U`), a health fix: the checklist on its own; `want` ticks those (item names,
    /// as `ptyro install` takes them) instead of the defaults, on `place`'s side when it names
    /// one (`--where`). With a daemon on another computer, that computer's `ptyro setup` (or
    /// `ptyro install` of `want`), in a split there. While an install the checklist started
    /// still runs, its split is shown instead: never a second install beside it.
    pub fn open_setup(&mut self, want: Option<Vec<String>>, place: Option<String>) {
        if self.setup_running() || self.first_run.is_some() {
            return;
        }
        if crate::setup_list::elsewhere(self.connect.remote.as_deref(), cfg!(windows)).is_some() {
            let Some(names) = want.filter(|w| !w.is_empty()) else {
                return self.start_cli("app.setup");
            };
            let mut args: Vec<String> =
                std::iter::once("install".to_owned()).chain(names).collect();
            if let Some(p) = place {
                args.extend(["--where".to_owned(), p]);
            }
            return self.run_cli(&args);
        }
        let want_place = place.and_then(|p| p.parse::<ptyro_agents::setup::Where>().ok());
        let want = want.map(|names| {
            names
                .iter()
                .filter_map(|n| ptyro_agents::setup::resolve(n).ok().map(|i| i.id.clone()))
                .collect()
        });
        let mut setup = self.setup_look(want);
        setup.want_place = want_place;
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
                fr.setup.check_busy(&self.paths.state);
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
        // Started elsewhere meanwhile (the web, a terminal): say so rather than open a split
        // that would only refuse.
        if let Some(by) = ptyro_agents::setup::lock::Lock::held_by(&self.paths.state) {
            return self.toast_err(ptyro_agents::setup::lock::busy(&by));
        }
        let wsl_daemon =
            crate::cli_actions::where_offered(cfg!(windows), self.connect.remote.as_deref())
                == Some(true);
        match setup.commands(wsl_daemon, crate::cli_actions::exe_in_wsl().as_deref()) {
            Ok(runs) if runs.is_empty() => {
                if only {
                    self.toast(format!("nothing ticked to install; {}", self.setup_later()));
                }
            }
            Ok(mut runs) => {
                // One split at a time: the next side's starts once this one ends (`setup_tick`).
                let first = runs.remove(0);
                self.setup_start(first, runs);
            }
            Err(e) => self.toast_err(e),
        }
    }

    /// `app.update`, the health badge's fix for an agent out of date, the start-up note's offer
    /// (BOOT5): `ptyro update [names…]` in a split, a real terminal where it shows each update's
    /// command and asks first, as the checklist's installs run: one at a time (never beside a
    /// running install), and the badge looks again once it ends. It runs where the daemon is,
    /// whose agents the findings are (`Request::Updates`): this computer's ptyro, WSL's own (its
    /// split runs in WSL), or a remote computer's.
    pub fn update_agents(&mut self, names: Vec<String>) {
        if self.first_run.is_some() {
            return self.toast("close the setup checklist first, then update (it runs in a split)");
        }
        if self.setup_running() {
            return;
        }
        let args: Vec<String> = std::iter::once("update".to_owned()).chain(names).collect();
        // This computer's lock, said before the split opens; elsewhere, `ptyro update` there
        // says so itself.
        if self.update_place().is_none()
            && let Some(by) = ptyro_agents::setup::lock::Lock::held_by(&self.paths.state)
        {
            return self.toast_err(ptyro_agents::setup::lock::busy(&by));
        }
        self.setup_start((None, args), Vec::new());
    }

    /// Where the daemon's agents are, for words: `None` this computer; `WSL` (Windows with WSL's
    /// daemon); or the remote computer's name.
    pub(crate) fn update_place(&self) -> Option<String> {
        let remote = self.connect.remote.as_deref();
        if let Some(r) = crate::setup_list::elsewhere(remote, cfg!(windows)) {
            let host = r.strip_prefix("ssh://").unwrap_or(&r);
            let host = host.rsplit_once('@').map_or(host, |(_, h)| h).to_owned();
            return Some(host);
        }
        (crate::cli_actions::where_offered(cfg!(windows), remote) == Some(true))
            .then(|| "WSL".to_owned())
    }

    /// Run one of the checklist's splits, `queue` after it.
    fn setup_start(
        &mut self,
        (bin, args): crate::setup_list::Split,
        queue: Vec<crate::setup_list::Split>,
    ) {
        match bin {
            Some(bin) => self.run_cli_with(bin, &args),
            None => self.run_cli(&args),
        }
        let Some(split) = self.last_split.clone() else { return };
        self.setup_run = Some(crate::setup_list::SetupRun { split, seen: false, queue });
    }

    /// The checklist's install still runs: show its split and say so (true), rather than open
    /// the checklist again.
    fn setup_running(&mut self) -> bool {
        let Some(run) = &self.setup_run else { return false };
        let split = run.split.clone();
        if let Some(id) = self.sessions.iter().find(|s| s.name == split).map(|s| s.id) {
            self.show(id, None);
        }
        self.toast(if split.starts_with("ptyro-update") {
            format!("an update is still running in {split}: try again once it's done")
        } else {
            format!("setup is still running in {split}: the checklist opens again once it's done")
        });
        true
    }

    /// The reply to a spawn: the checklist's split started (it is in the session list from now
    /// on), or it couldn't, which ends the run at once (and the splits queued after it: the
    /// failure is said, and the checklist can open again).
    pub(crate) fn setup_spawned(&mut self, name: Option<&str>, ok: bool) {
        let Some(run) = &mut self.setup_run else { return };
        if name != Some(run.split.as_str()) {
            return;
        }
        if ok {
            run.seen = true;
        } else {
            self.setup_run = None;
        }
    }

    /// From `tick`: once the running split ends, the next side's starts; after the last, the
    /// health badge looks again, so it no longer offers what was just installed.
    pub(crate) fn setup_tick(&mut self) {
        let Some(run) = &mut self.setup_run else { return };
        let ended = match self.sessions.iter().find(|s| s.name == run.split) {
            Some(s) if s.state.is_live() => {
                run.seen = true;
                false
            }
            Some(_) => true,
            // Gone after it started (closed, or removed); not started yet: its spawn's reply
            // comes (`setup_spawned`), and a failure clears this at once.
            None => run.seen,
        };
        if !ended {
            return;
        }
        let mut queue = std::mem::take(&mut run.queue);
        self.setup_run = None;
        if queue.is_empty() {
            self.health_again();
        } else {
            let next = queue.remove(0);
            self.setup_start(next, queue);
        }
        self.dirty = true;
    }

    /// How to come back to the checklist: its key, or the palette.
    fn setup_later(&self) -> String {
        match self.keymap.key_for("app.setup") {
            Some(k) => format!("{k} opens the setup checklist (agents and tools) any time"),
            None => "the palette's app.setup opens the setup checklist (agents and tools) any time"
                .to_owned(),
        }
    }

    /// Write the config (profiles, notifications), register the repositories, offer the tour;
    /// `later`: nothing installs, so say how to open the setup checklist again.
    fn finish_first_run(&mut self, choices: &[Choice], later: bool) {
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
        let did = if later { format!("{did}; {}", self.setup_later()) } else { did };
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
/// `updates.keep_installer` with `id` added (BOOT7's `K`), from the file as it is now (a CLI `k`
/// or the web's Keep may have added one since this TUI read it; `held` if it doesn't read).
pub(crate) fn keep_value(path: &Path, held: &[String], id: &str) -> String {
    let mut list = ptyro_core::config::Config::load(path)
        .map(|c| c.updates.keep_installer)
        .unwrap_or_else(|_| held.to_vec());
    if !list.iter().any(|k| k.eq_ignore_ascii_case(id)) {
        list.push(id.to_owned());
    }
    format!("[{}]", list.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(", "))
}

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
        let keep = if fr.setup.offers_moves() { " · K keep as installed" } else { "" };
        format!(" {go} · Space toggle · j/k move{side}{keep} · s not now")
    } else {
        let go = if steps > 0 {
            format!("⏎ install {steps} and use these")
        } else {
            "⏎ use these".to_owned()
        };
        let keep = if fr.setup.offers_moves() { " · K keep as installed" } else { "" };
        format!(
            " {go} · Space ticks · j/k move{side}{keep} · s not now (installs nothing) · Esc skip   (the repository you're in is added on its own)"
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
    const CELL: usize = 26;
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
                    spans.push(Span::styled(format!("{:<CELL$}", clip(&text, CELL)), st));
                    spans.push(Span::styled(" ", style));
                }
            } else {
                spans.push(Span::styled(format!("   {} ", mark(e)), accent));
                spans.push(Span::styled(
                    format!("{:<NAME$}", e.name),
                    style.add_modifier(Modifier::BOLD),
                ));
            }
            // The line wraps between its parts, under itself, rather than run off the box; when
            // the cells leave too little room beside them (80 columns, two sides), it goes under
            // the row instead.
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum::<usize>() + 1;
            let beside = width.saturating_sub(used) >= 30;
            let at = if beside { used } else { 7 };
            let text = about(plan, e, &programs, two);
            let mut wrapped = fit(&text, width.saturating_sub(at + 1).max(20)).into_iter();
            let detail = style.fg(app.depth.fit(t.dim));
            if beside {
                spans
                    .push(Span::styled(format!(" {}", wrapped.next().unwrap_or_default()), detail));
            }
            lines.push(Line::from(spans));
            for more in wrapped {
                lines.push(Line::from(Span::styled(format!("{}{more}", " ".repeat(at)), detail)));
            }
            if two {
                // What a side's cell can't say: why it can't, what needs it.
                for s in &sides {
                    let Some(x) = plan.entry_on(&e.id, *s) else { continue };
                    let why = ptyro_agents::setup::checklist::why(plan, x);
                    if !why.is_empty() {
                        let text = format!("{}: {}", r.sides.name(*s), why.join("; "));
                        for l in wrap_words(&text, width.saturating_sub(8)) {
                            lines.push(Line::styled(format!("       {l}"), dim));
                        }
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
                        format!("{} {words}{at}", r.sides.name(*s))
                    } else {
                        format!("{words}{at}")
                    })
                })
                .collect();
            let def = if e.id == default { " · your default agent" } else { "" };
            let head = format!("   ✓ {}", e.name);
            let at = head.chars().count() + 2;
            let text = format!("{}{def}", each.join(" · "));
            let mut wrapped = fit(&text, width.saturating_sub(at + 1).max(20)).into_iter();
            lines.push(Line::from(vec![
                Span::styled(head, fg(app, t.done)),
                Span::styled(format!("  {}", wrapped.next().unwrap_or_default()), dim),
            ]));
            for more in wrapped {
                lines.push(Line::styled(format!("{}{more}", " ".repeat(at)), dim));
            }
        }
    }
    let wsl = match &plan.wsl {
        Some(w @ (WslStatus::Unusable { .. } | WslStatus::Inside { .. })) => Some(w.words()),
        _ => None,
    };
    for note in [wsl, r.note.clone()].into_iter().flatten() {
        lines.push(Line::raw(""));
        for l in wrap_words(&note, width.saturating_sub(2)) {
            lines.push(Line::styled(format!(" {l}"), dim));
        }
    }
    if let Some(by) = &fr.setup.busy {
        let text = format!(
            "setup is already running on this computer (from {by}): ⏎ installs nothing now; open the checklist again once it's done"
        );
        lines.push(Line::raw(""));
        for l in wrap_words(&text, width.saturating_sub(2)) {
            lines.push(Line::styled(format!(" {l}"), fg(app, t.blocked)));
        }
    }
    if let Some(said) = &fr.setup.said {
        for l in wrap_words(said, width.saturating_sub(2)) {
            lines.push(Line::styled(format!(" {l}"), fg(app, t.blocked)));
        }
    }
}

/// `text` in lines of at most `width` characters, broken between words (a word longer than a
/// line, a URL say, is broken where the line ends, so all of it stays on screen).
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        for piece in chars.chunks(width) {
            let piece: String = piece.iter().collect();
            match out.last_mut() {
                Some(l) if l.chars().count() + 1 + piece.chars().count() <= width => {
                    l.push(' ');
                    l.push_str(&piece);
                }
                _ => out.push(piece),
            }
        }
    }
    out
}

/// `text` in at most `width` characters, an ellipsis ending what's cut.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `text` (parts joined by ` · `) in lines of at most `width` characters, broken between
/// parts (a part longer than a line is broken between its words, on lines of its own).
fn fit(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(" · ") {
        if part.chars().count() > width {
            // Longer than a line on its own: by words, on lines of its own.
            out.extend(wrap_words(part, width));
            continue;
        }
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
    fn k_keeps_what_the_file_holds_now_not_what_was_read_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Written by a CLI `k` after this TUI read its config (which held none).
        std::fs::write(&path, "[updates]\nkeep_installer = [\"claude\"]\n").unwrap();
        assert_eq!(super::keep_value(&path, &[], "codex"), r#"["claude", "codex"]"#);
        assert_eq!(super::keep_value(&path, &[], "claude"), r#"["claude"]"#, "once");
        // A file that doesn't read: what was held.
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "[updates\n").unwrap();
        assert_eq!(super::keep_value(&bad, &["gemini".into()], "codex"), r#"["gemini", "codex"]"#);
    }

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
