//! A creation form (SPEC §11.5a): every option of the thing being made, on one screen, each
//! one shown with its default and changeable where it stands. ⏎ creates from any field, so the
//! quick path (type a name, ⏎) stays one keystroke; Tab/↑↓ move; ←→ or space change a choice.
//!
//! The rule it serves (AGENTS.md): whatever the TUI creates, it shows every option the CLI's
//! command takes, as a pick where the answers are known.

use ptyro_core::input::{KeyCode, KeyEvent, Mods};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;
use crate::ui::{boxed, fg};

/// What the form makes; [`App::form_submit`] reads the values by key.
#[derive(Debug, Clone, PartialEq)]
pub enum Make {
    /// A workspace; on another machine when `Some` (MM1.B).
    Workspace(Option<String>),
    /// A profile; then, when `Some`, the workspace form it was opened from carries on.
    Profile(Option<Box<Form>>),
    /// `chat import`, asked first (SPEC §22.10): what to bring in.
    ChatImport,
    /// A fan-out: the same task to several agents.
    Fanout,
    /// A PR for this worktree (by id).
    PrCreate(String),
    /// A project, into the chosen workspace.
    ProjectAdd,
    /// Which workspaces this project (by id) is in.
    ProjectWorkspaces(String),
    /// A shell (or a command) in a workspace's SHELLS.
    Shell,
    /// A session (and maybe its new worktree): what the task box carried.
    Session(Box<crate::app::Wizard>),
    /// `ptyro <these words>` and then each field's own argument ([`Arg`]), in a split.
    Cli(Vec<String>),
}

/// How a field goes on the command line of a [`Make::Cli`] form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// Not on it: the form reads it itself.
    None,
    /// As a word of its own, where it stands; left out when empty.
    Pos,
    /// `--flag value`; left out when empty.
    Flag(&'static str),
    /// `--flag a --flag b` for `a, b`.
    Each(&'static str),
    /// `--flag` alone when the field has a value (a yes/no choice: yes is non-empty).
    Switch(&'static str),
    /// `--flag <prefix><value>` (`--adopt claude=~/.claude-work`).
    Pair(&'static str, String),
    /// Ticks: all of them, nothing (the default); some, `--flag a,b`; none, the second flag.
    Subset(&'static str, &'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub title: String,
    pub fields: Vec<Field>,
    pub sel: usize,
    pub make: Make,
    /// What went wrong on the last ⏎, shown until the next key.
    pub error: Option<String>,
    /// What ⏎ does, as the keys line says it: `create`, `save`, `run`.
    pub verb: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: String,
    pub label: String,
    /// One line about it, shown while it's selected.
    pub hint: String,
    pub kind: Kind,
    pub arg: Arg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Typed. `name`: held to what a name may be, as it's typed. `placeholder`: what an empty
    /// one means, dimmed.
    Text { value: String, placeholder: String, name: bool, required: bool },
    /// One of these `(value, shown as)`.
    Choice { options: Vec<(String, String)>, sel: usize },
    /// Any of these, ticked with space; ←→ move between them. Its value: the ticked ones'
    /// values, comma separated.
    Multi { options: Vec<(String, String)>, on: Vec<bool>, at: usize },
}

impl Field {
    pub fn text(key: &str, label: &str, placeholder: &str, hint: &str) -> Self {
        Field {
            key: key.into(),
            label: label.into(),
            hint: hint.into(),
            kind: Kind::Text {
                value: String::new(),
                placeholder: placeholder.into(),
                name: false,
                required: false,
            },
            arg: Arg::None,
        }
    }

    /// Something that must be typed (a target, a goal).
    pub fn required(key: &str, label: &str, placeholder: &str, hint: &str) -> Self {
        let mut f = Field::text(key, label, placeholder, hint);
        if let Kind::Text { required, .. } = &mut f.kind {
            *required = true;
        }
        f
    }

    /// Ticks: `on` says which start ticked.
    pub fn multi(
        key: &str,
        label: &str,
        options: Vec<(String, String)>,
        on: impl Fn(&str) -> bool,
        hint: &str,
    ) -> Self {
        let on = options.iter().map(|o| on(&o.0)).collect();
        Field {
            key: key.into(),
            label: label.into(),
            hint: hint.into(),
            kind: Kind::Multi { options, on, at: 0 },
            arg: Arg::None,
        }
    }

    /// A yes/no; yes is `yes`, no is empty.
    pub fn yes_no(key: &str, label: &str, yes: bool, hint: &str) -> Self {
        Field::choice(
            key,
            label,
            vec![("".into(), "no".into()), ("yes".into(), "yes".into())],
            usize::from(yes),
            hint,
        )
    }

    /// How it goes on the command line.
    pub fn arg(mut self, arg: Arg) -> Self {
        self.arg = arg;
        self
    }

    /// Its starting text.
    pub fn with(mut self, v: impl Into<String>) -> Self {
        if let Kind::Text { value, .. } = &mut self.kind {
            *value = v.into();
        }
        self
    }

    /// A name (a-z 0-9 . _ -). `suggested`, dimmed, is what ⏎ takes when nothing is typed;
    /// without one, a name is needed.
    pub fn name(key: &str, label: &str, suggested: &str, hint: &str) -> Self {
        Field {
            key: key.into(),
            label: label.into(),
            hint: hint.into(),
            kind: Kind::Text {
                value: String::new(),
                placeholder: suggested.into(),
                name: true,
                required: suggested.is_empty(),
            },
            arg: Arg::None,
        }
    }

    /// A pick; `sel` is the one that's chosen to begin with.
    pub fn choice(
        key: &str,
        label: &str,
        options: Vec<(String, String)>,
        sel: usize,
        hint: &str,
    ) -> Self {
        let sel = sel.min(options.len().saturating_sub(1));
        Field {
            key: key.into(),
            label: label.into(),
            hint: hint.into(),
            kind: Kind::Choice { options, sel },
            arg: Arg::None,
        }
    }

    pub fn value(&self) -> String {
        match &self.kind {
            Kind::Text { value, .. } => value.clone(),
            Kind::Choice { options, sel } => options.get(*sel).map_or("", |o| o.0.as_str()).into(),
            Kind::Multi { options, on, .. } => options
                .iter()
                .zip(on)
                .filter(|(_, on)| **on)
                .map(|(o, _)| o.0.as_str())
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Keep,
    Cancel,
    Submit,
}

impl Form {
    pub fn new(title: impl Into<String>, fields: Vec<Field>, make: Make) -> Self {
        Form { title: title.into(), fields, sel: 0, make, error: None, verb: "create" }
    }

    /// The value of `key` (empty when it has none).
    pub fn get(&self, key: &str) -> String {
        self.fields.iter().find(|f| f.key == key).map(Field::value).unwrap_or_default()
    }

    /// The command line of a [`Make::Cli`] form: `base`, then each field's [`Arg`].
    pub fn cli_args(&self, base: &[String]) -> Vec<String> {
        let mut args = base.to_vec();
        for f in &self.fields {
            let v = f.value();
            let v = v.trim();
            if let (Arg::Subset(flag, none), Kind::Multi { on, .. }) = (&f.arg, &f.kind) {
                if on.iter().all(|x| *x) {
                } else if v.is_empty() {
                    args.push((*none).to_owned());
                } else {
                    args.extend([(*flag).to_owned(), v.to_owned()]);
                }
                continue;
            }
            if v.is_empty() {
                continue;
            }
            match &f.arg {
                Arg::None => {}
                Arg::Pos => args.push(v.to_owned()),
                Arg::Flag(flag) => args.extend([(*flag).to_owned(), v.to_owned()]),
                Arg::Each(flag) => {
                    for one in v.split(',').map(str::trim).filter(|x| !x.is_empty()) {
                        args.extend([(*flag).to_owned(), one.to_owned()]);
                    }
                }
                Arg::Switch(flag) => args.push((*flag).to_owned()),
                Arg::Pair(flag, prefix) => {
                    args.extend([(*flag).to_owned(), format!("{prefix}{v}")])
                }
                Arg::Subset(..) => {}
            }
        }
        args
    }

    /// A text field's value, else what its placeholder offers (a suggested name).
    pub fn or_suggested(&self, key: &str) -> String {
        match self.fields.iter().find(|f| f.key == key).map(|f| &f.kind) {
            Some(Kind::Text { value, placeholder, .. }) if value.trim().is_empty() => {
                placeholder.clone()
            }
            _ => self.get(key).trim().to_owned(),
        }
    }

    /// Points at `key` with what's wrong with it, for [`App::form_submit`]'s own checks.
    pub fn fail(&mut self, key: &str, error: impl Into<String>) {
        if let Some(i) = self.fields.iter().position(|f| f.key == key) {
            self.sel = i;
        }
        self.error = Some(error.into());
    }

    /// The value of `key`, `None` when empty.
    pub fn opt(&self, key: &str) -> Option<String> {
        Some(self.get(key).trim().to_owned()).filter(|v| !v.is_empty())
    }

    /// Chooses `value` in the choice `key`, when it's one of the options.
    pub fn choose(&mut self, key: &str, value: &str) {
        if let Some(Field { kind: Kind::Choice { options, sel }, .. }) =
            self.fields.iter_mut().find(|f| f.key == key)
            && let Some(i) = options.iter().position(|o| o.0 == value)
        {
            *sel = i;
        }
    }

    /// The first thing that stops it being made: a required field left empty, a bad name.
    pub fn problem(&self) -> Option<(usize, String)> {
        self.fields.iter().enumerate().find_map(|(i, f)| match &f.kind {
            Kind::Text { value, required: true, .. } if value.trim().is_empty() => {
                Some((i, format!("{} is needed", f.label.to_lowercase())))
            }
            Kind::Text { value, name: true, .. } if !value.is_empty() => {
                ptyro_core::names::problem(value).map(|e| (i, format!("{}: {e}", f.label)))
            }
            _ => None,
        })
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        self.error = None;
        let ctrl = key.mods.has(Mods::CTRL);
        let n = self.fields.len();
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char { c: 'c' } if ctrl => return Outcome::Cancel,
            KeyCode::Enter | KeyCode::Char { c: 's' } if key.code == KeyCode::Enter || ctrl => {
                if let Some((i, e)) = self.problem() {
                    self.sel = i;
                    self.error = Some(e);
                    return Outcome::Keep;
                }
                return Outcome::Submit;
            }
            KeyCode::Tab | KeyCode::Down => self.sel = (self.sel + 1) % n.max(1),
            KeyCode::BackTab | KeyCode::Up => self.sel = (self.sel + n.max(1) - 1) % n.max(1),
            _ => {}
        }
        let Some(field) = self.fields.get_mut(self.sel) else { return Outcome::Keep };
        match &mut field.kind {
            Kind::Choice { options, sel } => {
                let len = options.len().max(1);
                match key.code {
                    KeyCode::Right | KeyCode::Char { c: ' ' } => *sel = (*sel + 1) % len,
                    KeyCode::Left => *sel = (*sel + len - 1) % len,
                    // A letter jumps to the next option that starts with it.
                    KeyCode::Char { c } if !ctrl => {
                        let c = c.to_ascii_lowercase();
                        if let Some(i) = (1..=len).map(|k| (*sel + k) % len).find(|&i| {
                            options[i].1.to_lowercase().starts_with(c)
                                || options[i].0.to_lowercase().starts_with(c)
                        }) {
                            *sel = i;
                        }
                    }
                    _ => {}
                }
            }
            Kind::Multi { options, on, at } => {
                let len = options.len().max(1);
                match key.code {
                    KeyCode::Right => *at = (*at + 1) % len,
                    KeyCode::Left => *at = (*at + len - 1) % len,
                    KeyCode::Char { c: ' ' } => {
                        if let Some(x) = on.get_mut(*at) {
                            *x = !*x;
                        }
                    }
                    // `a`: all, or none when all are ticked.
                    KeyCode::Char { c: 'a' } if !ctrl => {
                        let all = on.iter().all(|x| *x);
                        on.iter_mut().for_each(|x| *x = !all);
                    }
                    _ => {}
                }
            }
            Kind::Text { value, name, .. } => match key.code {
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char { c: 'u' } if ctrl => value.clear(),
                KeyCode::Char { c } if !ctrl => {
                    value.push(c);
                    if *name {
                        *value = ptyro_core::names::typed(value);
                    }
                }
                _ => {}
            },
        }
        Outcome::Keep
    }

    /// Pasted text goes into the selected text field, on one line.
    pub fn paste(&mut self, text: &str) {
        if let Some(Field { kind: Kind::Text { value, name, .. }, .. }) =
            self.fields.get_mut(self.sel)
        {
            value.push_str(&text.replace(['\r', '\n'], " "));
            if *name {
                *value = ptyro_core::names::typed(value);
            }
        }
    }
}

const WIDTH: u16 = 78;

pub fn draw(app: &App, form: &Form, f: &mut Frame<'_>, area: Rect) {
    let t = &app.theme;
    let label_w = form.fields.iter().map(|x| x.label.chars().count()).max().unwrap_or(0) + 1;
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut cursor = None;
    for (i, field) in form.fields.iter().enumerate() {
        let on = i == form.sel;
        let mark = if on { "› " } else { "  " };
        let label_style =
            if on { fg(app, t.accent).add_modifier(Modifier::BOLD) } else { fg(app, t.dim) };
        let mut spans = vec![
            Span::styled(mark, fg(app, t.accent)),
            Span::styled(format!("{:<label_w$} ", field.label), label_style),
        ];
        match &field.kind {
            Kind::Text { value, placeholder, .. } => {
                if on {
                    let col = 2 + label_w + 1 + value.chars().count();
                    cursor = Some((col as u16, lines.len() as u16));
                }
                if value.is_empty() {
                    spans.push(Span::styled(placeholder.clone(), fg(app, t.dim)));
                } else {
                    spans.push(Span::raw(value.clone()));
                }
            }
            Kind::Multi { options, on: ticked, at } => {
                if options.is_empty() {
                    spans.push(Span::styled("(none to pick from)", fg(app, t.dim)));
                }
                for (j, (o, x)) in options.iter().zip(ticked).enumerate() {
                    let tick = if *x { "[x] " } else { "[ ] " };
                    let style = if on && j == *at {
                        Style::reversed(app)
                    } else if *x {
                        ratatui::style::Style::new()
                    } else {
                        fg(app, t.dim)
                    };
                    spans.push(Span::styled(format!("{tick}{}", o.1), style));
                    spans.push(Span::raw("  "));
                }
            }
            Kind::Choice { options, sel } => {
                let shown = options.get(*sel).map_or("", |o| o.1.as_str());
                if on {
                    spans.push(Span::styled("‹ ", fg(app, t.accent)));
                    spans.push(Span::styled(shown.to_owned(), Style::reversed(app)));
                    spans.push(Span::styled(" ›", fg(app, t.accent)));
                    if options.len() > 1 {
                        spans.push(Span::styled(
                            format!("  {}/{}", sel + 1, options.len()),
                            fg(app, t.dim),
                        ));
                    }
                } else {
                    spans.push(Span::raw(shown.to_owned()));
                }
            }
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    let hint = form.fields.get(form.sel).map(|x| x.hint.clone()).unwrap_or_default();
    lines.push(match &form.error {
        Some(e) => Line::styled(format!("✗ {e}"), fg(app, t.error)),
        None => Line::styled(hint, fg(app, t.dim)),
    });
    let keys = match form.fields.get(form.sel).map(|x| &x.kind) {
        Some(Kind::Choice { .. }) => "←→ or a letter change it · Tab/↑↓ next · Esc cancel",
        Some(Kind::Multi { .. }) => "space ticks · ←→ move · a all · Tab/↑↓ next · Esc",
        _ => "Tab/↑↓ next · C-u clear · Esc cancel",
    };
    let keys = format!("⏎ {} · {keys}", form.verb);
    lines.push(Line::styled(keys, fg(app, t.dim)));
    let r = boxed(area, WIDTH, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    let block = Block::default().borders(Borders::ALL).border_style(fg(app, t.border_focus)).title(
        Span::styled(format!(" {} ", form.title), fg(app, t.accent).add_modifier(Modifier::BOLD)),
    );
    f.render_widget(
        Paragraph::new(lines).block(block).wrap(ratatui::widgets::Wrap { trim: false }),
        r,
    );
    if let Some((col, row)) = cursor {
        let col = col.min(r.width.saturating_sub(2));
        f.set_cursor_position(Position::new(r.x + 1 + col, r.y + 1 + row));
    }
}

/// The chosen option's look: the selection colours.
struct Style;
impl Style {
    fn reversed(app: &App) -> ratatui::style::Style {
        let t = &app.theme;
        ratatui::style::Style::new()
            .fg(app.depth.fit(t.selection_fg))
            .bg(app.depth.fit(t.selection_bg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::NONE)
    }

    fn form() -> Form {
        Form::new(
            "New thing",
            vec![
                Field::name("name", "Name", "", "its name"),
                Field::choice(
                    "profile",
                    "Profile",
                    vec![("default".into(), "default".into()), ("work".into(), "work".into())],
                    0,
                    "which account",
                ),
                Field::text("label", "Label", "(its name)", "shown in the sidebar"),
            ],
            Make::Workspace(None),
        )
    }

    #[test]
    fn enter_creates_from_any_field_once_whats_needed_is_there() {
        let mut f = form();
        f.sel = 1;
        // The name is needed: ⏎ goes to it and says so.
        assert_eq!(f.key(key(KeyCode::Enter)), Outcome::Keep);
        assert_eq!(f.sel, 0);
        assert!(f.error.as_deref().unwrap().contains("needed"));
        for c in "My Ws".chars() {
            f.key(key(KeyCode::Char { c }));
        }
        // Typed into a name: what a name can't hold becomes _.
        assert_eq!(f.get("name"), "my_ws");
        assert_eq!(f.key(key(KeyCode::Enter)), Outcome::Submit);
    }

    #[test]
    fn choices_change_in_place_and_by_letter() {
        let mut f = form();
        f.key(key(KeyCode::Tab));
        assert_eq!(f.get("profile"), "default");
        f.key(key(KeyCode::Right));
        assert_eq!(f.get("profile"), "work");
        f.key(key(KeyCode::Right));
        assert_eq!(f.get("profile"), "default");
        f.key(key(KeyCode::Char { c: 'w' }));
        assert_eq!(f.get("profile"), "work");
        f.key(key(KeyCode::Left));
        assert_eq!(f.get("profile"), "default");
        f.choose("profile", "work");
        assert_eq!(f.get("profile"), "work");
        // Up from the top wraps to the bottom.
        f.sel = 0;
        f.key(key(KeyCode::Up));
        assert_eq!(f.sel, 2);
        assert_eq!(f.opt("label"), None);
        assert_eq!(f.key(key(KeyCode::Esc)), Outcome::Cancel);
    }

    fn set(f: &mut Form, k: &str, v: &str) {
        if let Some(Field { kind: Kind::Text { value, .. }, .. }) =
            f.fields.iter_mut().find(|x| x.key == k)
        {
            *value = v.into();
        }
    }

    fn tick(f: &mut Form, k: &str, which: &[bool]) {
        if let Some(Field { kind: Kind::Multi { on, .. }, .. }) =
            f.fields.iter_mut().find(|x| x.key == k)
        {
            *on = which.to_vec();
        }
    }

    #[test]
    fn a_cli_form_builds_the_commands_words_leaving_out_what_is_empty() {
        let ticks = || opts(["contract", "push", "land"].map(|g| (g.to_owned(), g.to_owned())));
        let mut f = Form::new(
            "New change set",
            vec![
                Field::name("name", "Name", "", "").arg(Arg::Pos),
                Field::multi(
                    "projects",
                    "Projects",
                    opts([("a".into(), "a".into()), ("b".into(), "b".into())]),
                    |_| true,
                    "",
                )
                .arg(Arg::Flag("--projects")),
                Field::text("agents", "Member agents", "", "").arg(Arg::Each("--agent")),
                Field::text("dir", "Claude dir", "", "")
                    .arg(Arg::Pair("--adopt", "claude=".into())),
                Field::yes_no("overlay", "Overlay", true, "").arg(Arg::Switch("--overlay")),
                Field::yes_no("quiet", "Quiet", false, "").arg(Arg::Switch("--quiet")),
                Field::multi("gates", "Approvals", ticks(), |_| true, "")
                    .arg(Arg::Subset("--gates", "--no-gates")),
                Field::text("unused", "Unused", "", "").arg(Arg::Flag("--never")),
            ],
            Make::Cli(vec!["cs".into(), "new".into()]),
        );
        set(&mut f, "name", "billing");
        set(&mut f, "agents", "api=codex, web=claude");
        set(&mut f, "dir", "~/.claude-work");
        let base = ["cs".to_owned(), "new".to_owned()];
        assert_eq!(
            f.cli_args(&base),
            [
                "cs",
                "new",
                "billing",
                "--projects",
                "a,b",
                "--agent",
                "api=codex",
                "--agent",
                "web=claude",
                "--adopt",
                "claude=~/.claude-work",
                "--overlay"
            ],
            "all gates ticked: the default, nothing said"
        );
        tick(&mut f, "gates", &[true, false, true]);
        assert!(f.cli_args(&base).ends_with(&["--gates".to_owned(), "contract,land".to_owned()]));
        tick(&mut f, "gates", &[false, false, false]);
        assert!(f.cli_args(&base).ends_with(&["--no-gates".to_owned()]));
    }

    #[test]
    fn a_fanout_cycles_the_ticked_agents_as_the_profile_asked_for() {
        let mut f = Form::new(
            "Fan out",
            vec![
                Field::required("prompt", "Task", "", ""),
                Field::multi(
                    "agents",
                    "Agents",
                    opts(["claude", "codex", "gemini"].map(|a| (a.to_owned(), a.to_owned()))),
                    |a| a != "gemini",
                    "",
                ),
                Field::text("n", "Candidates", "", ""),
                Field::choice(
                    "profile",
                    "As profile",
                    opts([("".into(), "own".into()), ("work".into(), "work".into())]),
                    1,
                    "",
                ),
                Field::yes_no("structured", "Structured", true, ""),
            ],
            Make::Fanout,
        );
        set(&mut f, "prompt", "fix it");
        set(&mut f, "n", "3");
        let spec = fanout_spec(&f).unwrap();
        assert_eq!(spec.agents, ["claude@work", "codex@work", "claude@work"]);
        assert!(spec.structured);
        assert_eq!(spec.prompt, "fix it");
        set(&mut f, "n", "lots");
        assert_eq!(fanout_spec(&f).unwrap_err().0, "n");
        tick(&mut f, "agents", &[false, false, false]);
        assert_eq!(fanout_spec(&f).unwrap_err().0, "agents");
    }
}

// ── what the TUI makes with it ──────────────────────────────────────────────────────────────

use ptyro_protocol::Request;

use crate::app::{Overlay, Pending};

/// The option in a profile choice that makes a new one first.
pub const NEW_PROFILE: &str = "\0new";

/// The colours a profile can take, as the config names them (`parse_color`).
const COLOURS: &[&str] = &["blue", "green", "yellow", "magenta", "cyan", "red", "white"];

/// The agents (and `gh`) a profile keeps a directory for: field, label, `--adopt` prefix.
const DIRS: &[(&str, &str, &str)] = &[
    ("dir_claude", "Claude dir", "claude="),
    ("dir_codex", "Codex dir", "codex="),
    ("dir_gemini", "Gemini dir", "gemini="),
    ("dir_opencode", "OpenCode dir", "opencode="),
    ("dir_gh", "gh dir", "gh="),
];

/// A profile being made in a split (`ptyro profile new`): once it shows up, what comes next.
#[derive(Debug, Clone)]
pub struct AfterProfile {
    pub name: String,
    /// Log it in to this, in a split.
    pub login: Option<String>,
    /// The workspace form it was made from, back with it chosen.
    pub workspace: Option<Box<Form>>,
}

impl App {
    /// `+ new workspace`: its name, its profile (a new one too) and its notifications.
    pub(crate) fn open_workspace_form(&mut self, machine: Option<String>) {
        let on = machine.as_ref().map(|m| format!(" on {m}")).unwrap_or_default();
        let suggested = if machine.is_none() { self.free_workspace_name() } else { String::new() };
        let mut form = Form::new(
            format!("New workspace{on}"),
            vec![
                Field::name(
                    "name",
                    "Name",
                    &suggested,
                    "A space or other odd character becomes _ (a-z 0-9 . _ -)",
                ),
                self.profile_field(machine.as_deref(), &suggested),
                Field::choice(
                    "notify",
                    "Notifications",
                    vec![
                        ("all".into(), "everything".into()),
                        ("badge".into(), "a badge in the status bar only".into()),
                        ("mute".into(), "nothing".into()),
                    ],
                    0,
                    "What its sessions tell you about",
                ),
            ],
            Make::Workspace(machine),
        );
        form.sel = 0;
        self.overlay = Some(Overlay::Form(Box::new(form)));
    }

    /// The profiles on `machine` (here when `None`), `prefer` chosen when it is one, and
    /// `+ new profile…` last.
    fn profile_field(&self, machine: Option<&str>, prefer: &str) -> Field {
        let mut options: Vec<(String, String)> = self
            .profiles
            .iter()
            .filter(|p| p.machine.as_deref() == machine)
            .map(|p| {
                let bare = p.name.rsplit(':').next().unwrap_or(&p.name).to_owned();
                let account = p
                    .dirs
                    .get("claude")
                    .map(|d| format!(" · claude in {}", d.display()))
                    .unwrap_or_default();
                let shown = if p.label != bare {
                    format!("{bare} [{}]{account}", p.label)
                } else {
                    format!("{bare}{account}")
                };
                (bare, shown)
            })
            .collect();
        if !options.iter().any(|o| o.0 == ptyro_core::model::DEFAULT_PROFILE) {
            options.insert(0, (ptyro_core::model::DEFAULT_PROFILE.into(), "default".into()));
        }
        let sel = options.iter().position(|o| o.0 == prefer).unwrap_or(0);
        if machine.is_none() {
            options.push((NEW_PROFILE.into(), "+ new profile… (another account)".into()));
        }
        Field::choice(
            "profile",
            "Profile",
            options,
            sel,
            "The accounts, git author and keys its sessions use (unless a worktree or project says otherwise)",
        )
    }

    /// `profile.new`: everything `ptyro profile new` takes, and a login after.
    pub(crate) fn open_profile_form(&mut self, name: &str, workspace: Option<Box<Form>>) {
        let data = self.paths.data.join("profiles");
        let mut fields = vec![
            Field::name("name", "Name", "", "e.g. work: what you'll pick it by (a-z 0-9 . _ -)")
                .with(name)
                .arg(Arg::Pos),
            Field::text(
                "label",
                "Label",
                "(its name)",
                "Shown in the sidebar and on notifications",
            )
            .arg(Arg::Flag("--label")),
            Field::choice(
                "color",
                "Colour",
                std::iter::once(("".to_owned(), "none".to_owned()))
                    .chain(COLOURS.iter().map(|c| ((*c).to_owned(), (*c).to_owned())))
                    .collect(),
                0,
                "Its label's colour (any other, or #rrggbb, in the config panel)",
            )
            .arg(Arg::Flag("--color")),
        ];
        for (key, label, prefix) in DIRS {
            let agent = prefix.trim_end_matches('=');
            fields.push(
                Field::text(
                    key,
                    label,
                    &format!("a fresh one: {}", data.join("<name>").join(agent).display()),
                    &format!("Adopt a {agent} directory you already log in with (e.g. ~/.{agent}-work); empty makes a fresh one"),
                )
                .arg(Arg::Pair("--adopt", (*prefix).to_owned())),
            );
        }
        fields.extend([
            Field::text("git_name", "Git name", "(your global git's)", "The author of its commits")
                .arg(Arg::Flag("--git-name")),
            Field::text("git_email", "Git email", "(your global git's)", "The author's email")
                .arg(Arg::Flag("--git-email")),
            Field::text("ssh_key", "SSH key", "(your usual keys)", "e.g. ~/.ssh/id_work: the only key its git uses")
                .arg(Arg::Flag("--ssh-key")),
            Field::text(
                "matches",
                "Matches remotes",
                "github.com:acme/*",
                "Projects whose remote fits are pinned to it; several: comma separated",
            )
            .arg(Arg::Each("--match")),
            Field::choice(
                "login",
                "Then log in to",
                ["claude", "codex", "gemini", "opencode", "gh"]
                    .iter()
                    .map(|a| ((*a).to_owned(), format!("{a}, now")))
                    .chain(std::iter::once(("".to_owned(), "nothing yet".to_owned())))
                    .collect(),
                0,
                "Runs the tool's own sign-in in a split once it's made; ptyro never sees the credentials",
            ),
        ]);
        let form = Form::new("New profile", fields, Make::Profile(workspace));
        self.overlay = Some(Overlay::Form(Box::new(form)));
    }

    pub(crate) fn form_key(&mut self, mut form: Box<Form>, key: KeyEvent) {
        match form.key(key) {
            Outcome::Keep => self.overlay = Some(Overlay::Form(form)),
            Outcome::Cancel => {}
            Outcome::Submit => self.form_submit(form),
        }
    }

    /// ⏎: made, or the form back with what's wrong.
    pub(crate) fn form_submit(&mut self, mut form: Box<Form>) {
        match form.make.clone() {
            Make::Cli(base) => return self.run_cli(&form.cli_args(&base)),
            Make::Fanout => match fanout_spec(&form) {
                Ok(spec) => {
                    let done = format!(
                        "fan-out started ({} O)",
                        ptyro_core::config::key_label(&self.keymap.prefix)
                    );
                    return self.request(Pending::Done(done), Request::Fanout { spec });
                }
                Err((key, e)) => form.fail(key, e),
            },
            Make::PrCreate(addr) => {
                self.toast("pushing and opening a PR…");
                let req = Request::PrCreate {
                    addr,
                    draft: !form.get("draft").is_empty(),
                    title: form.opt("title"),
                    body: form.opt("body"),
                    base: form.opt("base"),
                };
                return self.request(Pending::Done(String::new()), req);
            }
            Make::ProjectAdd => {
                let path = form.get("path").trim().to_owned();
                let path = match path.strip_prefix("~/") {
                    Some(rest) => std::env::var_os("HOME").map_or_else(
                        || path.clone().into(),
                        |h| std::path::PathBuf::from(h).join(rest),
                    ),
                    None => path.clone().into(),
                };
                let req = Request::ProjectAdd { path, workspace: form.opt("workspace") };
                return self.request(Pending::ProjectAdded(None), req);
            }
            Make::ProjectWorkspaces(id) => {
                let want: Vec<String> = form
                    .get("workspaces")
                    .split(',')
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned)
                    .collect();
                let mut changed = 0;
                for w in self.workspaces.iter().filter(|w| w.machine.is_none()) {
                    let was = w.projects.contains(&id);
                    let now = want.contains(&w.name);
                    if was != now {
                        changed += 1;
                        self.request(
                            Pending::Done(String::new()),
                            Request::WorkspaceMember {
                                name: w.name.clone(),
                                project: id.clone(),
                                remove: was,
                            },
                        );
                    }
                }
                if changed == 0 {
                    self.toast("no change");
                } else {
                    self.toast(format!(
                        "in {}",
                        if want.is_empty() {
                            "no workspace (it stays registered)".to_owned()
                        } else {
                            want.join(", ")
                        }
                    ));
                }
                return;
            }
            Make::Shell => {
                let slot = match form.opt("slot").map(|n| n.parse::<u32>()) {
                    None => None,
                    Some(Ok(n)) if n > 0 => Some(n),
                    Some(_) => {
                        form.fail("slot", "a number, like 3 (empty: the lowest free)");
                        self.overlay = Some(Overlay::Form(form));
                        return;
                    }
                };
                let ws = form.get("workspace");
                let scratch = !form.get("scratch").is_empty();
                let cwd = match form.opt("dir") {
                    Some(d) => ptyro_core::config::expand_home(&d),
                    None => self.shell_dir(&ws),
                };
                let spec = ptyro_core::session::SpawnSpec {
                    cwd,
                    command: form.opt("command"),
                    loose: true,
                    slot,
                    scratch,
                    name: form.opt("name"),
                    title: form.opt("title"),
                    workspace: Some(ws),
                    ..Default::default()
                };
                return self.spawn_spec(spec, None);
            }
            Make::Session(w) => match session_wizard(&form, *w) {
                Ok((w, None)) => return self.start_in(w, None),
                Ok((w, Some(Where::Worktree(id)))) => return self.start_in(w, Some(id)),
                Ok((w, Some(Where::New(project)))) => {
                    return self.wizard_done(w, crate::palette::Target::NewWorktree(project));
                }
                Err((key, e)) => form.fail(key, e),
            },
            Make::ChatImport => {
                let req = Request::ChatImport {
                    agent: form.opt("agent"),
                    profile: form.opt("profile"),
                    since: form.opt("since"),
                    dry_run: !form.get("dry_run").is_empty(),
                };
                return self.chat_import_request(req);
            }
            Make::Workspace(machine) => {
                let name = form.or_suggested("name");
                let full = machine.as_ref().map_or_else(|| name.clone(), |m| format!("{m}:{name}"));
                if name.is_empty() {
                    form.fail("name", "a name is needed");
                } else if self.workspaces.iter().any(|w| w.name == full) {
                    form.fail(
                        "name",
                        format!("workspace {} exists", crate::ui::workspace_label(&full)),
                    );
                } else if form.get("profile") == NEW_PROFILE {
                    // The profile first, named like the workspace; then this form, back.
                    return self.open_profile_form(&name, Some(form));
                } else {
                    let req = Request::WorkspaceNew {
                        name: full.clone(),
                        profile: form.opt("profile"),
                        notify: form.opt("notify"),
                    };
                    return self.request(Pending::WorkspaceMade(full), req);
                }
            }
            Make::Profile(workspace) => {
                let name = form.get("name").trim().to_owned();
                if self.profiles.iter().any(|p| p.machine.is_none() && p.name == name) {
                    form.fail("name", format!("profile {name} exists"));
                } else {
                    let color = form.opt("color").unwrap_or_default();
                    if !color.is_empty()
                        && let Err(e) = ptyro_core::config::parse_color(&color)
                    {
                        form.fail("color", e);
                    } else {
                        self.run_cli(&profile_new_args(&form));
                        self.after_profile =
                            Some(AfterProfile { name, login: form.opt("login"), workspace });
                        return;
                    }
                }
            }
        }
        self.overlay = Some(Overlay::Form(form));
    }

    /// The profiles arrived: one being made has shown up, so what comes next happens.
    pub(crate) fn profiles_arrived(&mut self) {
        let Some(after) = self.after_profile.take() else { return };
        if !self.profiles.iter().any(|p| p.machine.is_none() && p.name == after.name) {
            self.after_profile = Some(after);
            return;
        }
        if let Some(agent) = &after.login {
            self.run_cli(&["profile".into(), "login".into(), after.name.clone(), agent.clone()]);
        }
        if let Some(mut ws) = after.workspace {
            // The workspace form again, with the new profile in its list and chosen.
            let machine = match &ws.make {
                Make::Workspace(m) => m.clone(),
                _ => None,
            };
            if let Some(i) = ws.fields.iter().position(|f| f.key == "profile") {
                ws.fields[i] = self.profile_field(machine.as_deref(), &after.name);
            }
            ws.error = None;
            self.toast(format!("profile {} made: ⏎ makes the workspace with it", after.name));
            self.overlay = Some(Overlay::Form(ws));
        } else {
            self.toast(format!("profile {} made", after.name));
        }
    }
}

/// `ptyro profile new …` with every field that was filled in.
pub fn profile_new_args(form: &Form) -> Vec<String> {
    form.cli_args(&["profile".to_owned(), "new".to_owned()])
}

// ── the CLI's other creating commands, as forms (SPEC §11.5a) ────────────────────────────────

fn opts<I: IntoIterator<Item = (String, String)>>(i: I) -> Vec<(String, String)> {
    i.into_iter().collect()
}

impl App {
    /// The agents: the default first, then the rest (`none`: an empty first option that means
    /// "the default", labelled so).
    pub(crate) fn agent_options(&self, none: Option<&str>) -> Vec<(String, String)> {
        let default = ptyro_agents::default_agent(&self.config);
        let mut names = ptyro_agents::names(&self.config);
        names.sort_by_key(|a| *a != default);
        let mut v: Vec<(String, String)> =
            none.map(|n| (String::new(), n.to_owned())).into_iter().collect();
        v.extend(names.into_iter().map(|a| {
            let shown = if a == default { format!("{a} (default)") } else { a.clone() };
            (a, shown)
        }));
        v
    }

    fn project_options(&self) -> Vec<(String, String)> {
        opts(
            self.projects
                .iter()
                .filter(|p| p.machine.is_none())
                .map(|p| (p.slug.clone(), p.slug.clone())),
        )
    }

    /// This machine's workspaces, `current` first.
    fn workspace_options(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .workspaces
            .iter()
            .filter(|w| w.machine.is_none())
            .map(|w| (w.name.clone(), w.name.clone()))
            .collect();
        v.sort_by_key(|w| w.0 != self.workspace);
        v
    }

    fn profile_options(&self, none: &str) -> Vec<(String, String)> {
        std::iter::once((String::new(), none.to_owned()))
            .chain(
                self.profiles
                    .iter()
                    .filter(|p| p.machine.is_none())
                    .map(|p| (p.name.clone(), format!("{} [{}]", p.name, p.label))),
            )
            .collect()
    }

    /// The project in context, for preselecting it.
    fn context_project_slug(&self) -> Option<String> {
        self.context().0.and_then(|p| self.project(&p).map(|p| p.slug.clone()))
    }

    fn open(&mut self, form: Form) {
        self.overlay = Some(Overlay::Form(Box::new(form)));
    }

    /// `sets.new`: `ptyro cs new` and everything it takes.
    pub(crate) fn open_set_form(&mut self) {
        let here = self.context_project_slug();
        let templates = self.changeset_templates();
        let fields = vec![
            Field::name("name", "Name", "", "The change set's name (a-z 0-9 . _ -)").arg(Arg::Pos),
            Field::multi(
                "projects",
                "Projects",
                self.project_options(),
                |p| Some(p) == here.as_deref(),
                "Its member repositories (with a template: extra members)",
            )
            .arg(Arg::Flag("--projects")),
            Field::text("goal", "Goal", "what the change is, for the lead and every member", "The goal, for the lead and every member")
                .arg(Arg::Flag("--prompt")),
            Field::text("issue", "From issue", "(none)", "The goal from a GitHub issue instead: a number in the first member's repo, or a URL")
                .arg(Arg::Flag("--from-issue")),
            Field::choice(
                "template",
                "Template",
                std::iter::once((String::new(), "none".to_owned()))
                    .chain(templates.into_iter().map(|t| (t.clone(), t)))
                    .collect(),
                0,
                "A template from ~/.config/ptyro/changesets/ (a hub repo's: by name in the CLI)",
            )
            .arg(Arg::Flag("--template")),
            Field::choice(
                "layout",
                "Layout",
                opts([
                    ("lead".into(), "lead: one agent per member, and a lead".into()),
                    ("solo".into(), "solo: one agent for all of them".into()),
                    ("manual".into(), "manual: no agents, you drive".into()),
                ]),
                0,
                "Who works on it",
            )
            .arg(Arg::Flag("--layout")),
            Field::choice("lead", "Lead agent", self.agent_options(Some("your default agent")), 0, "The lead's agent")
                .arg(Arg::Flag("--lead")),
            Field::text("agents", "Member agents", "api=codex, web=claude", "A member's own agent; several: comma separated")
                .arg(Arg::Each("--agent")),
            Field::text("bases", "Member bases", "web=origin/develop", "A member's own base; several: comma separated")
                .arg(Arg::Each("--base")),
            Field::text("depends", "Depends", "(from the template, else the manifests)", "`sdk:api, web:api+sdk`: who builds on whom")
                .arg(Arg::Each("--depends")),
            Field::multi(
                "gates",
                "Approvals",
                opts(["contract", "push", "land"].map(|g| (g.to_owned(), g.to_owned()))),
                |_| true,
                "Where it waits for you to approve; untick all for none",
            )
            .arg(Arg::Subset("--gates", "--no-gates")),
            Field::text("integration", "Integration check", "(none)", "A check run in the set's root; several: comma separated")
                .arg(Arg::Each("--integration")),
            Field::yes_no("overlay", "Build against siblings", false, "Turn on the overlays the manifests suggest")
                .arg(Arg::Switch("--overlay")),
            Field::choice("workspace", "Workspace", self.workspace_options(), 0, "Where its sessions live")
                .arg(Arg::Flag("--workspace")),
        ];
        let mut form =
            Form::new("New change set", fields, Make::Cli(vec!["cs".into(), "new".into()]));
        form.sel = 0;
        self.open(form);
    }

    /// The templates in `~/.config/ptyro/changesets/` (none when the daemon is elsewhere).
    fn changeset_templates(&self) -> Vec<String> {
        if self.connect.remote.is_some() {
            return Vec::new();
        }
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            return Vec::new();
        };
        let dir =
            ptyro_core::config::config_home(std::path::Path::new(&home)).join("ptyro/changesets");
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "toml" {
                    return None;
                }
                Some(p.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        v.sort();
        v
    }

    /// `set.add`: a project into the selected change set, with its agent and what it needs.
    pub(crate) fn open_set_add_form(&mut self, set: String) {
        let fields = vec![
            Field::choice("project", "Project", self.project_options(), 0, "The repository to add")
                .arg(Arg::Pos),
            Field::choice("agent", "Agent", self.agent_options(Some("the set's")), 0, "Its agent")
                .arg(Arg::Flag("--agent")),
            Field::text("depends", "Depends on", "(none)", "Members it builds on, comma separated")
                .arg(Arg::Flag("--depends-on")),
        ];
        let mut form = Form::new(
            format!("Add to change set {set}"),
            fields,
            Make::Cli(vec!["cs".into(), "add".into(), set]),
        );
        form.verb = "add";
        self.open(form);
    }

    /// `session.lead`: lead the selected session's fan-out of workers.
    pub(crate) fn open_lead_form(&mut self, session: String) {
        let fields = vec![
            Field::text(
                "max",
                "Workers at once",
                "(the config's)",
                "How many workers it may run at the same time",
            )
            .arg(Arg::Flag("--max-workers")),
            Field::multi(
                "agents",
                "Workers' agents",
                self.agent_options(None),
                |_| false,
                "The agents it may start; none ticked: any",
            )
            .arg(Arg::Each("--agent")),
        ];
        let mut form = Form::new(
            format!("Make {session} a lead"),
            fields,
            Make::Cli(vec!["run".into(), "lead".into(), session]),
        );
        form.verb = "start";
        self.open(form);
    }

    /// `remote.pair`: a phone or browser, its name and what it may do.
    pub(crate) fn open_pair_form(&mut self) {
        let fields = vec![
            Field::text("name", "Name", "phone", "What to call it in the devices list")
                .arg(Arg::Pos),
            Field::choice(
                "scope",
                "May",
                opts([
                    ("full".into(), "full: everything (the whole web app needs it)".into()),
                    ("control".into(), "control: start, stop and type into sessions".into()),
                    ("approve".into(), "approve: answer agents' questions".into()),
                    ("read".into(), "read: look only".into()),
                ]),
                0,
                "What the device may do",
            )
            .arg(Arg::Flag("--scope")),
            Field::text(
                "url",
                "Address",
                "(remote.public_url, Tailscale's name, or the listen address)",
                "Where the QR code points",
            )
            .arg(Arg::Flag("--url")),
        ];
        let mut form = Form::new("Pair a device", fields, Make::Cli(vec!["pair".into()]));
        form.verb = "pair";
        self.open(form);
    }

    /// `machine.add`: another machine whose workspaces show here.
    pub(crate) fn open_machine_form(&mut self) {
        let fields = vec![
            Field::required(
                "target",
                "Machine",
                "ssh://HOST, or the code `machine pair` printed on it",
                "How to reach it",
            )
            .arg(Arg::Pos),
            Field::text(
                "name",
                "Name",
                "(what it calls itself)",
                "What its names carry here (laptop2:work)",
            )
            .arg(Arg::Flag("--name")),
            Field::yes_no(
                "force",
                "Keep if silent",
                false,
                "Keep it even if it doesn't answer now",
            )
            .arg(Arg::Switch("--force")),
        ];
        let mut form =
            Form::new("Add a machine", fields, Make::Cli(vec!["machine".into(), "add".into()]));
        form.verb = "add";
        self.open(form);
    }

    /// `machine.pair`: a code for another machine, and the address it reaches this one at.
    pub(crate) fn open_machine_pair_form(&mut self) {
        let fields = vec![
            Field::text(
                "url",
                "Address",
                "(this web app server's)",
                "The address the other machine reaches this one at",
            )
            .arg(Arg::Flag("--url")),
        ];
        let mut form = Form::new(
            "A code for another machine",
            fields,
            Make::Cli(vec!["machine".into(), "pair".into()]),
        );
        form.verb = "make the code";
        self.open(form);
    }

    /// `chat.import`, asked first: whose histories, since when, or only count them.
    pub(crate) fn open_chat_import_form(&mut self) {
        let fields = vec![
            Field::choice(
                "agent",
                "Whose",
                opts([
                    ("".into(), "every agent's".into()),
                    ("claude".into(), "claude".into()),
                    ("codex".into(), "codex".into()),
                    ("gemini".into(), "gemini".into()),
                ]),
                0,
                "Your agents' own histories become read-only, searchable chats (their text is copied into chat.db)",
            ),
            Field::choice(
                "profile",
                "Profile",
                self.profile_options("every profile's"),
                0,
                "One profile's histories",
            ),
            Field::text(
                "since",
                "Since",
                "(all of it)",
                "Only sessions touched in this long: 30d, 12h",
            ),
            Field::yes_no(
                "dry_run",
                "Only count them",
                false,
                "Count what would come; write nothing",
            ),
        ];
        let mut form = Form::new("Import your agents' histories", fields, Make::ChatImport);
        form.verb = "import";
        self.open(form);
    }

    /// `recipe.run`, step one: which recipe (the daemon lists them).
    pub(crate) fn ask_recipes(&mut self) {
        let project = self.context().0;
        self.request(Pending::RecipeForm, Request::RecipeList { project });
    }

    pub(crate) fn recipe_picker(&mut self, recipes: Vec<ptyro_run::wire::RecipeInfo>) {
        if recipes.is_empty() {
            return self.toast_err(
                "no recipes (built-in, ~/.config/ptyro/recipes/ or the repo's .ptyro/recipes/)",
            );
        }
        let items = recipes
            .iter()
            .map(|r| crate::palette::Item {
                label: r.name.clone(),
                detail: format!(
                    "{} · {} step(s) · {}",
                    r.description.clone().unwrap_or_default(),
                    r.steps,
                    r.source
                ),
                key: None,
                target: crate::palette::Target::Path(r.name.clone().into()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: crate::palette::Palette::new("Run which recipe?", items),
            purpose: crate::app::Purpose::Recipe(recipes),
        });
    }

    /// `recipe.run`, step two: its variables, one field each, and where.
    pub(crate) fn open_recipe_form(&mut self, r: ptyro_run::wire::RecipeInfo) {
        let mut fields = Vec::new();
        for v in &r.vars {
            // `name (required)` / `name = default`.
            let (name, rest) = match v.split_once(" = ") {
                Some((n, d)) => (n.trim().to_owned(), format!("default: {d}")),
                None => (v.trim_end_matches("(required)").trim().to_owned(), String::new()),
            };
            let required = v.ends_with("(required)");
            let mut f = if required {
                Field::required(&format!("var_{name}"), &name, "", "Needed by the recipe")
            } else {
                Field::text(&format!("var_{name}"), &name, &rest, "A variable of the recipe")
            };
            f.arg = Arg::Pair("--var", format!("{name}="));
            fields.push(f);
        }
        let here = self.context_project_slug();
        let mut projects = vec![(String::new(), "(the one it names, or where you are)".to_owned())];
        projects.extend(self.project_options());
        let sel = projects.iter().position(|p| Some(&p.0) == here.as_ref()).unwrap_or(0);
        fields.push(
            Field::choice("project", "Project", projects, sel, "Where it runs")
                .arg(Arg::Flag("--project")),
        );
        fields.push(
            Field::text("base", "Base", "(the project's)", "Base for its new worktrees")
                .arg(Arg::Flag("--base")),
        );
        if let Some(fp) = &r.untrusted {
            fields.push(
                Field::choice(
                    "trust",
                    "Trust it",
                    vec![
                        (String::new(), "no: show me first".into()),
                        (fp.clone(), format!("yes ({fp})")),
                    ],
                    0,
                    "A repository's recipe runs only once you trust it",
                )
                .arg(Arg::Flag("--trust")),
            );
        }
        let mut form = Form::new(
            format!("Run recipe {}", r.name),
            fields,
            Make::Cli(vec!["recipe".into(), "run".into(), r.name.clone()]),
        );
        form.verb = "run";
        self.open(form);
    }
}

/// The fan-out a form describes: each ticked agent (as the profile, when one is chosen), cycled
/// to the number of candidates asked for.
pub fn fanout_spec(form: &Form) -> Result<ptyro_run::wire::FanoutSpec, (&'static str, String)> {
    let profile = form.opt("profile");
    let ticked: Vec<String> = form
        .get("agents")
        .split(',')
        .filter(|a| !a.is_empty())
        .map(|a| match &profile {
            Some(p) => format!("{a}@{p}"),
            None => a.to_owned(),
        })
        .collect();
    if ticked.is_empty() {
        return Err(("agents", "tick at least one agent".into()));
    }
    let agents = match form.opt("n") {
        None => ticked,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if n > 0 => ticked.iter().cycle().take(n).cloned().collect(),
            _ => return Err(("n", format!("{n}: a number of candidates, like 3"))),
        },
    };
    Ok(ptyro_run::wire::FanoutSpec {
        agents,
        prompt: form.get("prompt").trim().to_owned(),
        project: form.opt("project"),
        base: form.opt("base"),
        name: form.opt("name"),
        test: form.opt("test"),
        verify: form.opt("verify"),
        judge: form.opt("judge"),
        timeout: form.opt("timeout"),
        structured: !form.get("structured").is_empty(),
    })
}

impl App {
    /// `F` in the runs view: a fan-out, every option of `ptyro fanout` on one screen.
    pub(crate) fn open_fanout_form(&mut self, project: String) {
        let default = ptyro_agents::default_agent(&self.config);
        let agents: Vec<(String, String)> =
            self.agent_options(None).into_iter().map(|(a, _)| (a.clone(), a)).collect();
        let projects = self.project_options();
        let sel = projects.iter().position(|p| p.0 == project).unwrap_or(0);
        let fields = vec![
            Field::required(
                "prompt",
                "Task",
                "the same task for every candidate",
                "What each agent should do",
            ),
            Field::multi(
                "agents",
                "Agents",
                agents,
                |a| a == default,
                "One candidate each, in its own worktree",
            ),
            Field::text(
                "n",
                "Candidates",
                "(one per ticked agent)",
                "More than the agents ticked: they take turns",
            ),
            Field::choice(
                "profile",
                "As profile",
                self.profile_options("each one's own (worktree, pin, workspace)"),
                0,
                "The account they all run as",
            ),
            Field::choice("project", "Project", projects, sel, "The repository they work in"),
            Field::text("base", "Base", "(the project's)", "What their worktrees branch from"),
            Field::text(
                "name",
                "Name prefix",
                "(from the task)",
                "Their worktrees' names start with it",
            ),
            Field::text(
                "test",
                "Rank by",
                "(the project's test script)",
                "The check run when each is done, to rank them",
            ),
            Field::text(
                "verify",
                "Verify",
                "(none)",
                "Verified-done: bounce a candidate until this passes",
            ),
            Field::choice(
                "judge",
                "Judge",
                self.agent_options(Some("none")),
                0,
                "An agent that scores their diffs (0–10) in a clean context",
            ),
            Field::text("timeout", "Timeout", "(none)", "Per candidate: 45m"),
            Field::yes_no(
                "structured",
                "Structured",
                false,
                "Their tool calls show in the comparison",
            ),
        ];
        let mut form = Form::new("Fan out", fields, Make::Fanout);
        form.verb = "start";
        self.open(form);
    }
}

impl App {
    /// `P` in the inspector: push and open a PR, its title, body, base and whether a draft.
    pub(crate) fn open_pr_form(&mut self, worktree: String) {
        let name = self.worktree(&worktree).map_or_else(|| worktree.clone(), |w| w.qualified());
        let fields = vec![
            Field::text("title", "Title", "(from the commits)", "The PR's title"),
            Field::text("body", "Body", "(from the commits)", "What it says"),
            Field::text("base", "Into", "(the base's branch)", "The branch it merges into"),
            Field::yes_no(
                "draft",
                "Draft",
                self.config.github.draft,
                "A draft isn't ready for review yet (the default: github.draft)",
            ),
        ];
        let mut form = Form::new(format!("Open a PR for {name}"), fields, Make::PrCreate(worktree));
        form.verb = "push and open";
        self.open(form);
    }
}

impl App {
    /// Adding a project: where it is (`path`, when picked) and which workspace it joins.
    pub(crate) fn open_project_form(&mut self, path: Option<std::path::PathBuf>) {
        let fields = vec![
            Field::required("path", "Repository", "~/src/my-repo", "Where its main checkout is")
                .with(path.map(|p| p.display().to_string()).unwrap_or_default()),
            Field::choice(
                "workspace",
                "Workspace",
                self.workspace_options(),
                0,
                "The workspace it joins (more later: the project's menu → workspaces)",
            ),
        ];
        let mut form = Form::new("Add a project", fields, Make::ProjectAdd);
        form.verb = "add";
        // Picked already: ⏎ is all that's left; the workspace is the one to look at.
        if !form.get("path").is_empty() {
            form.sel = 1;
        }
        self.open(form);
    }

    /// `project.workspaces`: every workspace, the ones it's in ticked.
    pub(crate) fn open_project_workspaces(&mut self, p: &ptyro_core::model::ProjectInfo) {
        let ws: Vec<&ptyro_core::model::WorkspaceInfo> =
            self.workspaces.iter().filter(|w| w.machine.is_none()).collect();
        let options = ws.iter().map(|w| (w.name.clone(), w.name.clone())).collect();
        let field = Field::multi(
            "workspaces",
            "Workspaces",
            options,
            |name| ws.iter().any(|w| w.name == name && w.projects.contains(&p.id)),
            "It shows under each one ticked; its sessions under the one they started in",
        );
        let mut form = Form::new(
            format!("{} is in…", p.slug),
            vec![field],
            Make::ProjectWorkspaces(p.id.clone()),
        );
        form.verb = "save";
        self.open(form);
    }
}

impl App {
    /// `shells.command`: a shell in SHELLS with every option `ptyro shells new` takes.
    pub(crate) fn open_shell_form(&mut self) {
        let ws = self.shells_ws();
        let dir = self.shell_dir(&ws);
        let mut wss = self.workspace_options();
        wss.sort_by_key(|w| w.0 != ws);
        let fields = vec![
            Field::text(
                "command",
                "Command",
                "(your shell)",
                "What it runs: htop, ssh prod, npm run dev",
            ),
            Field::text("dir", "Directory", &dir.display().to_string(), "Where it starts"),
            Field::text(
                "slot",
                "Number",
                "(the lowest free)",
                "Its number in SHELLS: Alt-N shows it",
            ),
            Field::text("name", "Name", "(a random one)", "Its address (ptyro send NAME …)"),
            Field::text("title", "Title", "(none)", "What the sidebar says it is"),
            Field::yes_no(
                "scratch",
                "Scratch dir",
                false,
                "A new, empty directory of its own, which goes with it",
            ),
            Field::choice("workspace", "Workspace", wss, 0, "Whose SHELLS it is in"),
        ];
        let mut form = Form::new("New shell", fields, Make::Shell);
        form.verb = "start";
        self.open(form);
    }
}

/// Where a session from the form goes.
#[derive(Debug, PartialEq)]
pub enum Where {
    /// A new worktree of this project (id).
    New(String),
    /// This worktree (id).
    Worktree(String),
}

/// The values of "Where": a new worktree in each project, each worktree, here.
const NEW_IN: &str = "new:";
const IN_WT: &str = "wt:";

/// The session form's answers as a wizard, and where it goes (`None`: here).
pub fn session_wizard(
    form: &Form,
    w: crate::app::Wizard,
) -> Result<(crate::app::Wizard, Option<Where>), (&'static str, String)> {
    let agent = form.get("agent");
    let command = form.opt("command");
    if agent == "@command" && command.is_none() {
        return Err(("command", "type the command to run (or pick a shell)".into()));
    }
    let place = form.get("where");
    let target = if let Some(p) = place.strip_prefix(NEW_IN) {
        Some(Where::New(p.to_owned()))
    } else {
        place.strip_prefix(IN_WT).map(|id| Where::Worktree(id.to_owned()))
    };
    let new = matches!(target, Some(Where::New(_)));
    if new && agent == "@command" {
        return Err(("where", "a command runs in a worktree that exists, or here".into()));
    }
    let which = form.opt("which");
    let (mut pr, mut issue, mut branch, mut take) = (None, None, None, false);
    match form.get("from").as_str() {
        "" => {}
        _ if !new => return Err(("from", "only a new worktree starts from somewhere".into())),
        "pr" => match which
            .as_deref()
            .map(|t| crate::app::work_source(t).unwrap_or((t.parse().ok(), None)))
        {
            Some((Some(n), _)) => pr = Some(n),
            _ => return Err(("which", "the PR: its number or GitHub URL".into())),
        },
        "issue" => match which {
            Some(i) => issue = Some(i),
            None => return Err(("which", "the issue: its number or GitHub URL".into())),
        },
        "branch" => match which {
            Some(b) => branch = Some(b),
            None => return Err(("which", "the branch to check out".into())),
        },
        _ => take = true,
    }
    let name = form.opt("name");
    let prompt = form.opt("prompt");
    // A new worktree is named after the task when no name was given (as the task box does).
    let name = match (&name, new) {
        (None, true) => prompt.as_deref().and_then(ptyro_core::names::from_text),
        _ => name,
    };
    let wizard = crate::app::Wizard {
        agent: Some(agent),
        prompt,
        name,
        pr,
        issue,
        branch,
        take,
        profile: form.opt("profile"),
        base: form.opt("base"),
        no_setup: form.get("setup").is_empty(),
        no_fetch: form.get("fetch").is_empty(),
        verify: form.opt("verify"),
        structured: !form.get("structured").is_empty(),
        command,
        title: form.opt("title"),
        project: match &target {
            Some(Where::New(p)) => Some(p.clone()),
            _ => w.project.clone(),
        },
        ..w
    };
    Ok((wizard, target))
}

impl App {
    /// The task box's `C-o`: every option of `ptyro new` (and `wt new`) on one form.
    pub(crate) fn open_session_form(&mut self, w: crate::app::Wizard) {
        self.resolve_for(&w);
        let mut agents = self.agent_options(None);
        agents.push(("@shell".into(), "a shell".into()));
        agents.push(("@command".into(), "a command…".into()));
        let agent_sel =
            w.agent.as_ref().and_then(|a| agents.iter().position(|o| o.0 == *a)).unwrap_or(0);
        // Where: a new worktree of each project (this one first), its worktrees, here.
        let here_project = w.project.clone();
        let mut places: Vec<(String, String)> = Vec::new();
        let mut projects: Vec<&ptyro_core::model::ProjectInfo> =
            self.projects.iter().filter(|p| p.machine.is_none()).collect();
        projects.sort_by_key(|p| Some(&p.id) != here_project.as_ref());
        for p in &projects {
            places.push((format!("{NEW_IN}{}", p.id), format!("a new worktree of {}", p.slug)));
        }
        let mut wts: Vec<&ptyro_core::model::WorktreeInfo> = self
            .worktrees
            .iter()
            .filter(|x| x.state == ptyro_core::model::WorktreeState::Ready && x.machine.is_none())
            .collect();
        wts.sort_by_key(|x| (Some(&x.project) != here_project.as_ref(), !x.is_root, x.qualified()));
        for x in wts {
            let what = if x.is_root { " (main checkout)" } else { "" };
            places.push((format!("{IN_WT}{}", x.id), format!("in {}{what}", x.qualified())));
        }
        places.push((String::new(), format!("here, in {}", self.here().display())));
        // As the task box does: a project in context means a new worktree of it.
        let place_sel = match (&w.project, &w.worktree) {
            (Some(p), _) => places.iter().position(|x| x.0 == format!("{NEW_IN}{p}")),
            (None, Some(id)) => places.iter().position(|p| p.0 == format!("{IN_WT}{id}")),
            _ => None,
        }
        .unwrap_or(places.len() - 1);
        let auto = match &self.resolved {
            Some(r) => format!("automatic: {r}"),
            None => "automatic (worktree → project pin → workspace)".to_owned(),
        };
        let from = opts([
            ("".into(), "the base (a new branch)".into()),
            ("pr".into(), "a pull request".into()),
            ("issue".into(), "an issue".into()),
            ("branch".into(), "a branch that exists".into()),
            ("take".into(), "my uncommitted changes in the main checkout".into()),
        ]);
        let from_sel = if w.pr.is_some() {
            1
        } else if w.issue.is_some() {
            2
        } else if w.branch.is_some() {
            3
        } else {
            usize::from(w.take) * 4
        };
        let which =
            w.pr.map(|n| n.to_string())
                .or_else(|| w.issue.clone())
                .or_else(|| w.branch.clone())
                .unwrap_or_default();
        let fields = vec![
            Field::text(
                "prompt",
                "Task",
                "(none: it waits for you)",
                "Its first prompt; also its title and, for a new worktree, its name",
            )
            .with(w.prompt.clone().unwrap_or_default()),
            Field::choice(
                "agent",
                "Run",
                agents,
                agent_sel,
                "The agent (the default first), a shell, or a command",
            ),
            Field::text(
                "command",
                "Command",
                "(only for a command)",
                "The command line, run through your shell: pipes and $VARS work",
            ),
            Field::choice(
                "profile",
                "Profile",
                std::iter::once((String::new(), auto))
                    .chain(self.profile_options("").into_iter().skip(1))
                    .collect(),
                0,
                "Which accounts it runs as; another than the automatic one may ask first",
            ),
            Field::choice(
                "where",
                "Where",
                places,
                place_sel,
                "A new worktree is an isolated checkout on its own branch",
            ),
            Field::choice("from", "Start from", from, from_sel, "A new worktree's starting point"),
            Field::text(
                "which",
                "Which",
                "the PR or issue number or URL, or the branch",
                "For a PR, an issue or a branch",
            )
            .with(which),
            Field::text("base", "Base", "(the project's)", "What a new worktree branches from"),
            Field::name(
                "name",
                "Name",
                "(from the task, else a random one)",
                "Its name, a new worktree's too (a-z 0-9 . _ -)",
            )
            .with(w.name.clone().unwrap_or_default()),
            Field::text("title", "Title", "(the task)", "What the sidebar says it is"),
            Field::text(
                "verify",
                "Verify",
                "(none)",
                "Verified-done: its turns count as done only when this check passes",
            ),
            Field::yes_no(
                "structured",
                "Structured",
                false,
                "The chat view: its tool calls shown as they happen",
            ),
            Field::yes_no(
                "setup",
                "Setup script",
                true,
                "A new worktree runs its project's setup script (when trusted)",
            ),
            Field::yes_no("fetch", "Fetch first", true, "A new worktree fetches its base first"),
        ];
        let title = if w.project.is_some() || w.worktree.is_some() {
            crate::app::wizard_title(&w, &self.projects, &self.worktrees, None)
        } else {
            "New session".to_owned()
        };
        // `N` (a name, no task): the name is what it's about, so it's selected.
        let at_name = w.name.is_some() && w.prompt.is_none();
        let mut form = Form::new(title, fields, Make::Session(Box::new(w)));
        form.verb = "start";
        if at_name {
            form.sel = form.fields.iter().position(|f| f.key == "name").unwrap_or(0);
        }
        self.open(form);
    }
}
