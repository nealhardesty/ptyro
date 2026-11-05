//! SHELLS in the TUI (SPEC §23.6, ROADMAP SH.2): a workspace's numbered terminals. `Alt-1…9`
//! shows shell N (an empty number starts one there), `C-q C-c` starts one in the pane's
//! directory, `C-q e` is the shells menu (a digit picks), and a loose session's pane header
//! carries the strip, tmux's status line. The sidebar's SHELLS section is drawn here too.

use std::path::PathBuf;

use ptyro_core::SessionId;
use ptyro_core::config::key_label;
use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ptyro_core::session::{SessionInfo, SpawnSpec, Watch};
use ptyro_protocol::Request;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::{App, Focus, Overlay, Pending, PromptKind, Purpose};
use crate::keymap::{Command, Table};
use crate::palette::{Item, Palette, Target};
use crate::tree::Row;

impl App {
    /// The workspace SHELLS keys act on: the selected row's (in the sidebar), else the focused
    /// session's, else the one shown.
    pub(crate) fn shells_ws(&self) -> String {
        if self.focus == Focus::Sidebar {
            let rows = self.rows();
            if let Some(w) = crate::tree::Inputs::workspace_of(&rows, self.sel_index(&rows)) {
                return w;
            }
        }
        self.focused_session()
            .and_then(|id| self.session(id))
            .map(|s| self.tree().home(s))
            .unwrap_or_else(|| self.workspace.clone())
    }

    /// A workspace's loose sessions, by number.
    pub(crate) fn shells_in(&self, ws: &str) -> Vec<&SessionInfo> {
        let tree = self.tree();
        let mut v: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| s.loose && s.slot.is_some() && tree.home(s) == ws)
            .collect();
        v.sort_by_key(|s| s.slot);
        v
    }

    /// Where a new shell starts (SPEC §23.4): the selected row's directory (in the sidebar), else
    /// the focused session's, else home. `~` stands for home on another machine, where the
    /// daemon puts it in its own home.
    pub(crate) fn shell_dir(&self, ws: &str) -> PathBuf {
        let machine = crate::tree::machine_and_name(ws).0.map(str::to_owned);
        let mine = |s: &SessionInfo| s.machine == machine;
        let of_session = |s: &SessionInfo| s.live_cwd.clone().unwrap_or_else(|| s.cwd.clone());
        if self.focus == Focus::Sidebar {
            match self.selected_row() {
                Some(Row::Session(id) | Row::Quick(id)) => {
                    if let Some(s) = self.session(id).filter(|s| mine(s)) {
                        return of_session(s);
                    }
                }
                Some(Row::Worktree(w)) => {
                    if let Some(w) = self.worktree(&w).filter(|w| w.machine == machine) {
                        return w.path.clone();
                    }
                }
                Some(Row::Project(p)) => {
                    if let Some(p) = self.project(&p).filter(|p| p.machine == machine) {
                        return p.root.clone();
                    }
                }
                _ => {}
            }
        }
        if let Some(s) = self.focused_session().and_then(|id| self.session(id)).filter(|s| mine(s))
        {
            return of_session(s);
        }
        match machine {
            Some(_) => PathBuf::from("~"),
            None => ptyro_core::config::expand_home("~"),
        }
    }

    /// `shells.new` (`C-q C-c`): a new shell in the workspace's SHELLS, full sized; `slot`: in
    /// that number; `scratch`: in a new, empty directory of its own.
    pub(crate) fn shells_new(&mut self, slot: Option<u32>, scratch: bool) {
        let ws = self.shells_ws();
        self.shells_new_in(ws, slot, scratch);
    }

    /// `shells_new` in a given workspace (a click on another workspace's strip).
    pub(crate) fn shells_new_in(&mut self, ws: String, slot: Option<u32>, scratch: bool) {
        let cwd = self.shell_dir(&ws);
        let spec = SpawnSpec {
            cwd,
            loose: true,
            slot,
            scratch,
            workspace: Some(ws),
            ..SpawnSpec::default()
        };
        self.spawn_spec(spec, None);
    }

    /// `shells.go N` (`Alt-N`): show shell N; one lost in a restart starts again; an empty number
    /// gets a new shell, here.
    pub(crate) fn shells_go(&mut self, n: u32) {
        let ws = self.shells_ws();
        self.shells_go_in(ws, n);
    }

    /// `shells_go` in a given workspace.
    pub(crate) fn shells_go_in(&mut self, ws: String, n: u32) {
        let found = self.shells_in(&ws).into_iter().find(|s| s.slot == Some(n)).cloned();
        match found {
            Some(s) => {
                if s.lost {
                    self.request(
                        Pending::Done(format!("shell {n} started again")),
                        Request::Restart { addr: s.id.full() },
                    );
                }
                self.show(s.id, None);
            }
            None => {
                self.toast(format!("shell {n} started here"));
                self.shells_new_in(ws, Some(n), false);
            }
        }
    }

    /// `shells.menu` (`C-q e`): the workspace's SHELLS, a digit picks one, `c` makes one, `s` a
    /// scratch one. It works where `Alt` doesn't: `C-q e 3`.
    pub(crate) fn shells_menu(&mut self) {
        let ws = self.shells_ws();
        let mut items: Vec<Item> = self
            .shells_in(&ws)
            .into_iter()
            .map(|s| {
                let n = s.slot.unwrap_or_default();
                Item {
                    label: format!("{n}  {}", label(s)),
                    detail: doing(s).unwrap_or_else(|| state_word(s)),
                    key: (n <= 9).then(|| n.to_string()),
                    target: Target::Action(Command {
                        action: "shells.go",
                        arg: Some(n.to_string()),
                    }),
                }
            })
            .collect();
        items.push(Item {
            label: "new shell".into(),
            detail: "here, in the pane's directory".into(),
            key: Some("c".into()),
            target: Target::Action(Command { action: "shells.new", arg: None }),
        });
        items.push(Item {
            label: "a command…".into(),
            detail: "htop, ssh prod, a dev server: in SHELLS, with a number".into(),
            key: Some("!".into()),
            target: Target::Action(Command { action: "shells.command", arg: None }),
        });
        items.push(Item {
            label: "scratch shell".into(),
            detail: "in a new, empty directory that goes with it".into(),
            key: Some("s".into()),
            target: Target::Action(Command { action: "shells.scratch", arg: None }),
        });
        let (_, short) = crate::tree::machine_and_name(&ws);
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(format!("Shells of {short}"), items).with_hotkeys(),
            purpose: Purpose::Palette,
        });
    }

    /// A click on a pane header's strip, at (`x`, `y`): `true` when it hit one.
    pub(crate) fn strip_click(&mut self, x: u16, y: u16) -> bool {
        let hit = self
            .strip_hits
            .borrow()
            .iter()
            .find(|h| h.y == y && (h.x0..h.x1).contains(&x))
            .map(|h| h.hit.clone());
        match hit {
            Some(StripHit::Go(ws, n)) => self.shells_go_in(ws, n),
            Some(StripHit::New(ws)) => self.shells_new_in(ws, None, false),
            None => return false,
        }
        true
    }

    /// The loose session an action is about: the selected row's (in the sidebar), else the
    /// focused one.
    fn shell_target(&self) -> Option<&SessionInfo> {
        let id = match (self.focus, self.selected_row()) {
            (Focus::Sidebar, Some(Row::Session(id))) => Some(id),
            _ => self.focused_session(),
        }?;
        self.session(id)
    }

    /// `shells.move`, `.watch`, `.loose`, `.adopt`, `.scratch`.
    pub(crate) fn shells_action(&mut self, action: &str) {
        if action == "shells.scratch" {
            return self.shells_new(None, true);
        }
        if action == "shells.command" {
            self.overlay = Some(Overlay::Prompt {
                title: "A command in SHELLS (htop, ssh prod…; it gets the next free number)".into(),
                text: String::new(),
                kind: PromptKind::ShellCommand,
            });
            return;
        }
        let Some(s) = self.shell_target().cloned() else {
            return self.toast("pick a session first");
        };
        match action {
            "shells.loose" if s.loose => self.toast(format!("{} is in SHELLS already", s.name)),
            "shells.loose" => self.request(
                Pending::Done(format!("{} is in SHELLS now", s.name)),
                Request::ShellPlace { addr: s.id.full(), loose: true, worktree: None },
            ),
            _ if !s.loose => self.toast_offer(
                format!("{} isn't in SHELLS", s.name),
                false,
                crate::app::run_offer("move it there", "shells.loose"),
            ),
            "shells.adopt" => self.request(
                Pending::Done(format!("{} joined its worktree", s.name)),
                Request::ShellPlace { addr: s.id.full(), loose: false, worktree: None },
            ),
            "shells.move" => {
                self.overlay = Some(Overlay::Prompt {
                    title: format!(
                        "New number for shell {} (one already there swaps)",
                        s.slot.unwrap_or_default()
                    ),
                    text: String::new(),
                    kind: PromptKind::ShellMove(s.id),
                });
            }
            "shells.watch" => self.open_shell_watch(&s),
            _ => {}
        }
    }

    fn open_shell_watch(&mut self, s: &SessionInfo) {
        let choice = |label: &str, detail: &str, key: &str, arg: &str| Item {
            label: label.into(),
            detail: detail.into(),
            key: Some(key.into()),
            target: Target::Action(Command { action: "shells.watch_set", arg: Some(arg.into()) }),
        };
        let mut items = vec![
            choice("done", "when its command ends, however short", "d", "done"),
            choice("output", "when new output arrives while you aren't looking", "o", "output"),
            choice("silence 30s", "when it has printed nothing for 30 seconds", "s", "silence 30s"),
            choice("silence 5m", "when it has printed nothing for 5 minutes", "5", "silence 5m"),
            choice("match…", "when a line on its screen matches a pattern", "m", "match"),
        ];
        if s.watch.is_some() {
            items.push(choice("off", "stop watching it", "x", "off"));
        }
        let now = s.watch.as_ref().map_or_else(String::new, |w| format!(" (now: {})", w.label()));
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(
                format!("Watch shell {}{now}", s.slot.unwrap_or_default()),
                items,
            )
            .with_hotkeys(),
            purpose: Purpose::ShellWatch(s.id),
        });
    }

    /// A watch picked from the list (`match` asks for its pattern first).
    pub(crate) fn shell_watch_picked(&mut self, id: SessionId, choice: &str) {
        if choice == "match" {
            self.overlay = Some(Overlay::Prompt {
                title: "Tell me when a line matches (a regular expression, like ERROR|listening)"
                    .into(),
                text: String::new(),
                kind: PromptKind::ShellMatch(id),
            });
            return;
        }
        let watch = if choice == "off" { None } else { Watch::parse(choice).ok() };
        self.set_watch(id, watch);
    }

    pub(crate) fn set_watch(&mut self, id: SessionId, watch: Option<Watch>) {
        let msg = watch
            .as_ref()
            .map_or_else(|| "not watched".into(), |w| format!("watching: {}", w.label()));
        self.request(Pending::Done(msg), Request::ShellWatch { addr: id.full(), watch });
    }

    /// What was typed for a command in SHELLS: it runs through the daemon's shell, here.
    pub(crate) fn shell_command_typed(&mut self, text: &str) {
        let line = text.trim();
        if line.is_empty() {
            return self.shells_new(None, false);
        }
        let ws = self.shells_ws();
        let cwd = self.shell_dir(&ws);
        let spec = SpawnSpec {
            cwd,
            command: Some(line.to_owned()),
            loose: true,
            workspace: Some(ws),
            ..SpawnSpec::default()
        };
        self.spawn_spec(spec, None);
    }

    /// What was typed for `shells.move`.
    pub(crate) fn shell_move_typed(&mut self, id: SessionId, text: &str) {
        match text.trim().parse::<u32>() {
            Ok(n) if n > 0 => self.request(
                Pending::Done(format!("it's shell {n} now")),
                Request::ShellMove { addr: id.full(), slot: n },
            ),
            _ => self.toast_err("a number from 1 up"),
        }
    }

    /// Whether this terminal sends `Alt` with a digit, or types a character for it instead
    /// (macOS's Terminal.app and iTerm2, unless Option is set to act as Meta, which no program
    /// can see until a key says so; the kitty keyboard protocol reports `Alt` either way).
    pub(crate) fn alt_digits_reach(&self) -> bool {
        self.alt_seen || self.kitty || !option_types_characters()
    }

    /// What each key press says about Alt: Alt with a *digit* proves Alt-digits reach ptyro
    /// (Option-Left arrives as Alt-b in Terminal.app even without Option as Meta, so any other Alt
    /// key proves nothing); Option-digit arriving as `¡™£…` is said once, with what to do (SPEC
    /// §23.6). The character still goes where it was going: it may be meant.
    pub(crate) fn learn_alt(&mut self, key: KeyEvent) {
        let digit = matches!(key.code, KeyCode::Char { c } if c.is_ascii_digit());
        if key.mods.has(Mods::ALT) && digit {
            if !self.alt_seen {
                self.alt_seen = true;
                self.dirty = true;
                // It works after all (Option set as Meta): the badge stops saying otherwise.
                self.health.findings.retain(|f| f.what != ALT_FINDING);
                self.health.announced = self.health.findings.len();
            }
            return;
        }
        let KeyCode::Char { c } = key.code else { return };
        let Some(n) = option_digit(c) else { return };
        if self.option_said || self.alt_seen || !self.config.shell.alt_digits {
            return;
        }
        self.option_said = true;
        let menu = self.keymap.key_for("shells.menu").unwrap_or_else(|| "C-q e".into());
        self.toast(format!(
            "that was Option-{n}? {} typed {c} instead of Alt-{n}: {} (until then, {menu} {n})",
            terminal_name(),
            option_as_meta_steps()
        ));
    }

    /// The jump keys as hints say them: `M-1…9` when they're bound and reach the TUI, else
    /// `C-q e 1…9` (the shells menu, which always works).
    pub(crate) fn shells_jump_hint(&self) -> String {
        let alt1 = KeyEvent::new(KeyCode::Char { c: '1' }, Mods(Mods::ALT));
        let bound = self
            .keymap
            .lookup(Table::Global, alt1)
            .is_some_and(|c| c.action == "shells.go" && c.number() == Some(1));
        if bound && self.alt_digits_reach() {
            return "M-1…9".into();
        }
        match self.keymap.key_for("shells.menu") {
            Some(k) => format!("{k} 1…9"),
            None => key_label(&self.keymap.prefix),
        }
    }
}

/// The health finding about Option-digit typing characters (`health.rs`).
pub(crate) const ALT_FINDING: &str = "shells alt keys";

/// macOS's Terminal.app and iTerm2 type a character for Option-digit unless told otherwise.
pub(crate) fn option_types_characters() -> bool {
    matches!(std::env::var("TERM_PROGRAM").as_deref(), Ok("Apple_Terminal" | "iTerm.app"))
}

/// The digit a US-layout Option-digit character stands for (`¡` is Option-1).
fn option_digit(c: char) -> Option<u8> {
    "º¡™£¢∞§¶•ª".chars().position(|x| x == c).map(|i| i as u8)
}

fn terminal_name() -> &'static str {
    match std::env::var("TERM_PROGRAM").as_deref() {
        Ok("iTerm.app") => "iTerm2",
        Ok("Apple_Terminal") => "Terminal.app",
        _ => "this terminal",
    }
}

/// Where this terminal's "Option as Meta" lives.
pub(crate) fn option_as_meta_steps() -> &'static str {
    match std::env::var("TERM_PROGRAM").as_deref() {
        Ok("iTerm.app") => "turn on Esc+ for the left Option key (Settings → Profiles → Keys)",
        _ => "turn on \"Use Option as Meta key\" (Settings → Profiles → Keyboard)",
    }
}

/// A shell's label (SPEC §23.4): `ptyro_core::session::shell_label`.
pub(crate) fn label(s: &SessionInfo) -> String {
    let home = ptyro_core::config::expand_home("~");
    ptyro_core::session::shell_label(s, Some(&home))
}

/// What its row says it's doing: the command running, else the foreground program, else the
/// last command with ✓/✗ (as a quick terminal's line does).
pub(crate) fn doing(s: &SessionInfo) -> Option<String> {
    crate::ui::quick_doing(s)
}

fn state_word(s: &SessionInfo) -> String {
    if s.lost { "lost in a restart: ⏎ starts it again".into() } else { s.state.label() }
}

/// A sidebar row of the SHELLS section (its heading, `＋ new shell`, a loose session), or `None`
/// when `row` isn't one.
pub(crate) fn sidebar_line(
    app: &App,
    row: &Row,
    focused: Option<SessionId>,
    shown: &[SessionId],
) -> Option<Line<'static>> {
    let t = &app.theme;
    let fg = |c| Style::new().fg(app.depth.fit(c));
    Some(match row {
        Row::Shells(ws) => {
            let arrow =
                if app.collapsed.contains(&crate::tree::shells_key(ws)) { "▸" } else { "▾" };
            Line::from(vec![
                Span::styled(format!(" {arrow} "), fg(t.dim)),
                Span::styled("SHELLS", fg(t.dim).add_modifier(Modifier::BOLD)),
                Span::styled("  ＋", fg(t.accent)),
                Span::styled(format!("  {}", app.shells_jump_hint()), fg(t.dim)),
            ])
        }
        Row::ShellNew(_) => Line::from(vec![
            Span::raw("     "),
            Span::styled(
                format!("＋ new shell {}", app.keymap.key_for("shells.new").unwrap_or_default()),
                fg(t.dim),
            ),
        ]),
        Row::Session(id) => {
            let s = app.session(*id).filter(|s| s.loose)?;
            let (g, gc) = crate::ui::glyph(app, s);
            let g = if s.lost { "↻" } else { g };
            let live = s.state.is_live();
            let mut name = fg(if live { t.text } else { t.exited });
            if Some(s.id) == focused {
                name = name.add_modifier(Modifier::BOLD).fg(app.depth.fit(t.accent));
            } else if shown.contains(&s.id) {
                name = name.add_modifier(Modifier::BOLD);
            }
            let marked = app.marked.contains(&s.id);
            let num = s.slot.map_or_else(|| "  ".into(), |n| format!("{n:>2} "));
            let mut spans = vec![
                Span::raw(" "),
                if marked {
                    Span::styled(" ◆ ", fg(t.accent).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(num, fg(t.accent))
                },
                Span::styled(format!("{g} "), fg(gc)),
                Span::styled(label(s), name),
            ];
            if s.scratch {
                spans.push(Span::styled(" ◌", fg(t.dim)));
            }
            if let Some(w) = &s.watch {
                spans.push(Span::styled(format!(" ◎ {}", w.label()), fg(t.dim)));
            }
            let blocked =
                s.status.as_ref().filter(|r| r.status == ptyro_core::status::AgentStatus::Blocked);
            if s.lost {
                spans.push(Span::styled(" ⏎ starts it again", fg(t.dim)));
            } else if let Some(r) = blocked {
                let why = r.kind.as_deref().unwrap_or("needs you");
                spans.push(Span::styled(format!(" ⚠ {why}"), fg(t.blocked)));
            } else if let Some(a) = &s.attention {
                spans.push(Span::styled(format!(" {}", a.text), fg(t.blocked)));
            } else if let Some(d) = doing(s) {
                spans.push(Span::styled(format!(" {d}"), fg(t.dim)));
            }
            Line::from(spans)
        }
        _ => return None,
    })
}

/// The strip in a loose session's pane header (SPEC §23.6), tmux's status line: every shell of
/// its workspace by number, the shown one in brackets, one that wants you with its glyph.
pub(crate) fn strip(
    app: &App,
    s: &SessionInfo,
    base: Style,
    focused: bool,
) -> Vec<(Span<'static>, Option<StripHit>)> {
    let t = &app.theme;
    let ws = app.tree().home(s);
    let mut out = vec![(Span::styled("SHELLS ", base.add_modifier(Modifier::DIM)), None)];
    for o in app.shells_in(&ws) {
        let n = o.slot.unwrap_or_default();
        let hit = Some(StripHit::Go(ws.clone(), n));
        if o.id == s.id {
            let style = if focused {
                base.fg(app.depth.fit(t.accent)).add_modifier(Modifier::BOLD)
            } else {
                base.add_modifier(Modifier::BOLD)
            };
            out.push((Span::styled(format!("[{n} {}]", label(o)), style), hit));
        } else {
            let mark = if o.attention.is_some() {
                " ◉"
            } else if o.lost {
                " ↻"
            } else {
                ""
            };
            let word = ptyro_core::session::brief(&label(o), 12);
            out.push((Span::styled(format!("{n} {word}{mark}"), base), hit));
        }
        out.push((Span::styled(" ", base), None));
    }
    out.push((Span::styled("＋", base.fg(app.depth.fit(t.accent))), Some(StripHit::New(ws))));
    out
}

/// What a click on the strip does: show shell N of a workspace, or start one there (`＋`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StripHit {
    Go(String, u32),
    New(String),
}

/// Where a strip's number was drawn this frame (`App.strip_hits`): columns `x0..x1` of row `y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripAt {
    pub x0: u16,
    pub x1: u16,
    pub y: u16,
    pub hit: StripHit,
}
