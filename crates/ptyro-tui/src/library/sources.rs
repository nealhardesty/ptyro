//! The Library's Sources tab (SPEC §25.10, ROADMAP KB3.7): what is read into the knowledge
//! base. Every project is, by itself; a folder or a file is once it is given (`i`, the ingest
//! form, which takes what `ptyro ingest` takes). Each row says what it holds, when it was
//! last read, what went wrong, and how far a read of it is (`KbRoots`, asked again while
//! something is being read; `KbProgress` between the asks). `s` looks at one again, `S` at
//! all of them, `x` stops reading one (the daemon asks first, in its words), `b` reads what
//! the agents kept before ptyro did, `t` switches the keeping of agent sessions, `c` that of
//! shell commands and `o` that of what they print (`kb.shell`).
//!
//! With machines linked, the machine filter says whose roots are listed and acted on: roots
//! are each machine's own (SPEC §25.11).

use std::time::{Duration, Instant};

use ptyro_core::kb::{KbPlan, KbRootInfo, RootKind, RootOptions, RootSpec, RootState};

use super::*;
use crate::form::{Field, Form, Make};

/// How often the tab asks again while something is being read or removed.
const POLL: Duration = Duration::from_secs(1);

impl Library {
    /// The selected root.
    pub(super) fn root(&self) -> Option<&KbRootInfo> {
        self.sources.get(self.sel).filter(|_| self.tab == Tab::Sources)
    }

    /// The linked machine whose roots are listed (`None`: this computer's).
    pub(super) fn far(&self) -> Option<String> {
        self.machine.clone().filter(|m| m != "here" && self.tab == Tab::Sources)
    }

    /// Something is being read or removed: the list is asked for again until it is done.
    fn busy(&self) -> bool {
        self.sources.iter().any(|r| r.progress.is_some() || r.removing)
            || self.status.as_ref().is_some_and(|s| s.queued > 0)
    }
}

/// How a root stands, in a word or two.
fn state_of(r: &KbRootInfo) -> String {
    match (r.removing, r.progress, r.root.state) {
        (true, ..) => "being removed".to_owned(),
        (_, Some((done, of)), _) if of > 0 => format!("reading {done} of {of}"),
        (_, Some(_), _) => "waiting to be read".to_owned(),
        (.., RootState::Missing) => "missing".to_owned(),
        (.., RootState::Error) => "failing".to_owned(),
        (.., RootState::Syncing) => "being read".to_owned(),
        (.., RootState::Ok) => match r.root.synced_ms {
            Some(t) => format!("read {} ago", crate::ui::ago(t)),
            None => "not read yet".to_owned(),
        },
    }
}

/// What a root holds: `12 files, 40 commits, 300 pieces`.
fn holds(r: &KbRootInfo) -> String {
    let n = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = vec![n(r.files, "file")];
    if r.commits > 0 {
        parts.push(n(r.commits, "commit"));
    }
    parts.push(pieces(r.chunks));
    parts.join(", ")
}

/// What is read of a root's files, where it says so itself (else the settings decide).
fn tiers(o: &RootOptions) -> Vec<String> {
    let mut out = Vec::new();
    for (what, v) in [("comments", o.comments), ("code", o.code), ("commits", o.commits)] {
        match v {
            Some(true) => out.push(what.to_owned()),
            Some(false) => out.push(format!("no {what}")),
            None => {}
        }
    }
    if !o.include.is_empty() {
        out.push(format!("only {}", o.include.join(", ")));
    }
    if !o.exclude.is_empty() {
        out.push(format!("not {}", o.exclude.join(", ")));
    }
    if o.once {
        out.push("read once".to_owned());
    }
    out
}

/// The line over the list: what the tab is, and whether agent sessions are kept.
pub(super) fn about(app: &App, l: &Library) -> Line<'static> {
    let dim = fg(app, app.theme.dim);
    let sessions = match l.status.as_ref().map(|s| s.sessions) {
        Some(false) => "agent sessions are not kept (t keeps them)",
        _ => "agent sessions are kept as they happen",
    };
    let whose = match l.far() {
        Some(m) => format!("what {m} reads"),
        None => "what is read: your projects by themselves, and the folders you add".to_owned(),
    };
    Line::styled(format!(" {whose} · {sessions}"), dim)
}

/// What is kept of your shells, and the keys that change it (`kb.shell`). `None`: a daemon
/// from before shells were kept, or a linked machine's sources (the switches are this
/// computer's).
pub(super) fn shells(l: &Library) -> Option<&'static str> {
    if l.far().is_some() {
        return None;
    }
    match l.status.as_ref()?.shell.as_str() {
        "off" => Some("nothing of your shells is kept · c keeps the commands you run in them"),
        "commands" => Some(
            "shell commands are kept, not what they print · c stops keeping them · o also keeps what they print",
        ),
        "output" => Some(
            "shell commands are kept, and what they print · o stops keeping what they print · c stops keeping both",
        ),
        _ => None,
    }
}

/// The keys, for the foot of the panel.
pub(super) fn keys(app: &App, l: &Library) -> String {
    let mut keys = vec!["i read a folder", "s look again", "S at all of them", "x stop reading it"];
    keys.extend(["b agents' history", "t keep sessions", "c shell commands", "o their output"]);
    keys.extend(["j/k move", "y path"]);
    if !app.machines.is_empty() {
        keys.push("Tab machine");
    }
    keys.push(if l.show_status { "v the list" } else { "v status" });
    keys.extend(["1 Search", "2 Notes", "Esc"]);
    keys.join(" · ")
}

/// The roots: what each is called and how it stands, then where it is and what it holds, then
/// what went wrong with it.
pub(super) fn draw_rows(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let width = area.width as usize;
    let mut out: Vec<(Line<'static>, Option<usize>)> = Vec::new();
    let mut sel_line = 0;
    const INDENT: &str = "          ";
    let say = |out: &mut Vec<(Line<'static>, Option<usize>)>, text: &str, st: Style| {
        for line in wrapped(text, width.saturating_sub(2)) {
            out.push((Line::styled(format!(" {line}"), st), None));
        }
    };
    if let Some(note) = &l.note {
        say(&mut out, note, dim);
    }
    if let Some(shells) = shells(l) {
        say(&mut out, shells, dim);
    }
    if l.loading && l.sources.is_empty() {
        say(&mut out, "Looking…", dim);
    } else if l.sources.is_empty() && l.note.is_none() {
        say(
            &mut out,
            "nothing is read yet: a project is, once ptyro knows it; i reads a folder or a file",
            dim,
        );
    }
    for (i, r) in l.sources.iter().enumerate() {
        if i == l.sel {
            sel_line = out.len();
        }
        let state = state_of(r);
        let mut name = r.root.name.clone();
        crate::ui::truncate_width(&mut name, width.saturating_sub(13 + state.chars().count()));
        let mut st = fg(app, t.text);
        if i == l.sel {
            st = st.add_modifier(Modifier::REVERSED);
        }
        let bad = matches!(r.root.state, RootState::Missing | RootState::Error);
        out.push((
            Line::from(vec![
                Span::styled(format!(" {:<8}", r.root.kind.as_str()), fg(app, t.accent)),
                Span::styled(format!(" {name} "), st),
                Span::styled(format!(" {state}"), if bad { fg(app, t.error) } else { dim }),
            ]),
            Some(i),
        ));
        let mut about = vec![holds(r)];
        about.extend(r.root.profile.as_ref().map(|p| format!("profile {p}")));
        about.push(r.root.path.clone());
        for line in wrapped(&about.join(" · "), width.saturating_sub(INDENT.len() + 1)) {
            out.push((Line::styled(format!("{INDENT}{line}"), dim), Some(i)));
        }
        if let Some(e) = &r.root.error {
            for line in wrapped(e, width.saturating_sub(INDENT.len() + 1)) {
                let st = if bad { fg(app, t.error) } else { dim };
                out.push((Line::styled(format!("{INDENT}{line}"), st), Some(i)));
            }
        }
    }
    // The agents' own history, while it is read behind everything else.
    if let Some(n) = l.status.as_ref().map(|s| s.queued).filter(|n| *n > 0) {
        out.push((Line::raw(""), None));
        say(&mut out, &format!("{n} waiting to be read (agent sessions, and their history)"), dim);
    }
    let room = area.height as usize;
    let skip = sel_line.saturating_sub(room.saturating_sub(4));
    let mut drawn = l.drawn.borrow_mut();
    let mut lines = Vec::new();
    for (n, (line, row)) in out.into_iter().skip(skip).take(room).enumerate() {
        if let Some(r) = row {
            drawn.push((area.y + n as u16, r));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The selected root, in full, beside the list.
pub(super) fn draw_root(app: &App, f: &mut Frame<'_>, area: Rect, l: &Library) {
    let t = &app.theme;
    let dim = fg(app, t.dim);
    let Some(r) = l.root() else { return };
    let mut lines =
        vec![Line::styled(r.root.name.clone(), fg(app, t.text).add_modifier(Modifier::BOLD))];
    let fact = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:<10}"), dim),
            Span::styled(value, fg(app, t.text)),
        ])
    };
    let kind = match r.root.kind {
        RootKind::Project => "a project: read for as long as ptyro has it",
        RootKind::Folder => "a folder you gave",
        RootKind::File => "a file you gave",
    };
    lines.push(fact("it is", kind.to_owned()));
    lines.push(fact("at", r.root.path.clone()));
    lines.push(fact("state", state_of(r)));
    lines.push(fact("holds", holds(r)));
    if let Some(p) = &r.project_name {
        lines.push(fact("project", p.clone()));
    }
    let seen = r.root.profile.clone().unwrap_or_else(|| "every profile's agents".to_owned());
    lines.push(fact("seen by", seen));
    let own = tiers(&r.root.options);
    let reads = if own.is_empty() {
        "what your settings say (Knowledge: comments, code, commits)".to_owned()
    } else {
        own.join(" · ")
    };
    lines.push(fact("reads", reads));
    if let Some(at) = r.root.added_ms {
        lines.push(fact("since", when(at)));
    }
    if let Some(at) = r.root.synced_ms {
        lines.push(fact("last read", when(at)));
    }
    if let Some(e) = &r.root.error {
        lines.push(Line::raw(""));
        let bad = matches!(r.root.state, RootState::Missing | RootState::Error);
        lines.push(Line::styled(e.clone(), if bad { fg(app, t.error) } else { dim }));
    }
    lines.push(Line::raw(""));
    let stop = match r.root.kind {
        RootKind::Project => "it stops being read when the project is removed from ptyro",
        _ => "x stops reading it and drops what was kept of it (your files aren't touched)",
    };
    lines.push(Line::styled(format!("s looks at it again now · {stop}"), dim));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// What the ingest form asks for, as `ptyro ingest` would: a switch says something only where
/// the command has a flag for it (`--no-comments`, `--code`, `--no-commits`), so that one left
/// as it stands leaves the settings deciding. `here`: the path is this computer's, so one
/// that is not whole is made whole (another computer's paths are its own).
pub(super) fn spec_of(form: &Form, here: bool) -> RootSpec {
    let globs = |key: &str| -> Vec<String> {
        form.get(key).split(',').map(str::trim).filter(|g| !g.is_empty()).map(Into::into).collect()
    };
    let on = |key: &str| !form.get(key).is_empty();
    let mut path = form.get("path").trim().to_owned();
    if here
        && !path.starts_with('~')
        && std::path::Path::new(&path).is_relative()
        && let Ok(dir) = std::env::current_dir()
    {
        path = dir.join(&path).display().to_string();
    }
    RootSpec {
        path,
        name: form.opt("name"),
        project: form.opt("project"),
        profile: form.opt("profile"),
        options: RootOptions {
            include: globs("include"),
            exclude: globs("exclude"),
            comments: (!on("comments")).then_some(false),
            code: on("code").then_some(true),
            commits: (!on("commits")).then_some(false),
            once: on("once"),
        },
    }
}

/// What a read that was asked for will take, as the command says it.
fn planned(name: &str, path: &str, plan: &KbPlan) -> String {
    let again = if plan.existing { " (read already: looked at again)" } else { "" };
    format!("{name} ({path}): {}{again}", plan.line())
}

impl App {
    /// A key on the Sources tab's list.
    pub(super) fn sources_key(&mut self, mut l: Box<Library>, key: KeyEvent) {
        let ch = match key.code {
            KeyCode::Char { c } if !key.mods.has(Mods::CTRL) => Some(c),
            _ => None,
        };
        let last = l.sources.len().saturating_sub(1);
        let mut act = None;
        match (key.code, ch) {
            (KeyCode::Esc, _) | (_, Some('q')) => return,
            (KeyCode::Down, _) | (_, Some('j')) => l.sel = (l.sel + 1).min(last),
            (KeyCode::Up, _) | (_, Some('k')) => l.sel = l.sel.saturating_sub(1),
            (KeyCode::PageDown, _) => l.sel = (l.sel + PAGE).min(last),
            (KeyCode::PageUp, _) => l.sel = l.sel.saturating_sub(PAGE),
            (_, Some('g')) => l.sel = 0,
            (_, Some('G')) => l.sel = last,
            (_, Some('v')) => l.show_status = !l.show_status,
            (KeyCode::Tab, _) => {
                l.chip = Filter::shown(l.tab, !self.machines.is_empty()).first().copied();
            }
            (KeyCode::Enter, _) => act = Some('s'),
            (
                _,
                Some(
                    c @ ('1' | '2' | '/' | 'i' | 'a' | 's' | 'S' | 'x' | 'd' | 'b' | 't' | 'c'
                    | 'o' | 'y'),
                ),
            ) => act = Some(c),
            _ => {}
        }
        let path = l.root().map(|r| r.root.path.clone());
        self.chat_panel = Some(ChatPanel::Library(l));
        match act {
            Some('1') => self.library_tab(Tab::Search),
            Some('2') => self.library_tab(Tab::Notes),
            Some('/') => self.library_action("kb.search"),
            Some('i' | 'a') => self.ingest_pick(),
            Some('s') => self.sources_sync(false),
            Some('S') => self.sources_sync(true),
            Some('x' | 'd') => self.sources_remove(),
            Some('b') => self.open_backfill_form(),
            Some('t') => self.sessions_switch(),
            Some('c') => self.shells_switch(false),
            Some('o') => self.shells_switch(true),
            Some('y') => {
                if let Some(p) = path {
                    crate::osc52(&p);
                    self.toast("copied its path");
                }
            }
            _ => {}
        }
    }

    /// The Sources tab, asked for again while something is being read or removed (from the
    /// app's tick).
    pub(crate) fn sources_tick(&mut self) {
        let Some(l) = self.library_mut() else { return };
        if l.tab != Tab::Sources || !l.busy() || l.polled.elapsed() < POLL {
            return;
        }
        l.polled = Instant::now();
        self.library_news();
    }

    /// `KbProgress`: how far the pass over a root is, between two asks.
    pub(crate) fn sources_progress(&mut self, root: &str, done: u64, of: u64) {
        let Some(l) = self.library_mut() else { return };
        if l.tab != Tab::Sources || l.far().is_some() {
            return;
        }
        match l.sources.iter_mut().find(|r| r.root.id == root) {
            // The pass ended: what it holds now is asked for.
            Some(_) if done >= of => {
                l.polled = Instant::now();
                self.library_news();
            }
            Some(r) => {
                r.progress = Some((done, of));
                self.dirty = true;
            }
            // A root this list doesn't have yet (a project just added).
            None => self.library_news(),
        }
    }

    /// `s` and `S`: look at the selected root again, or at every one (and the notes).
    fn sources_sync(&mut self, all: bool) {
        let Some(l) = self.library() else { return };
        let machine = l.far();
        let roots = match (all, l.root()) {
            (true, _) => Vec::new(),
            (false, Some(r)) => vec![r.root.id.clone()],
            (false, None) => return self.toast("nothing is read yet: i reads a folder"),
        };
        self.request(Pending::KbSynced, Request::KbSync { roots, machine });
    }

    /// `x`, `kb.remove`: stop reading the selected root. The daemon says what goes, and asks.
    pub(super) fn sources_remove(&mut self) {
        let shown = self.library().is_some_and(|l| l.tab == Tab::Sources);
        let Some(r) = self.library().and_then(Library::root).cloned() else {
            if !shown {
                self.library_tab(Tab::Sources);
            }
            return self.toast("select what to stop reading here, then x");
        };
        let machine = self.library().and_then(Library::far);
        let req = Request::KbRemove { root: r.root.id.clone(), confirmed: false, machine };
        let then = Pending::Confirm(Box::new(req.clone()), Box::new(Pending::KbRemoved));
        self.request(then, req);
    }

    /// A root is being removed: said as the command says it.
    pub(crate) fn sources_removed(&mut self, name: &str, items: u64, chunks: u64) {
        self.toast(format!(
            "{name} is no longer read: dropping {items} item{} ({}); your files aren't touched",
            if items == 1 { "" } else { "s" },
            pieces(chunks)
        ));
        self.library_news();
    }

    /// `t`: whether agent sessions are kept as they happen (`kb.sessions`).
    fn sessions_switch(&mut self) {
        let on = !self.config.kb.sessions;
        let said = if on {
            "agent sessions are kept from here on"
        } else {
            "agent sessions are no longer kept (what was kept stays)"
        };
        self.set_config("kb.sessions", if on { "true" } else { "false" }, said);
        self.library_news();
    }

    /// `c` and `o`: what is kept of your shells (`kb.shell`). `c` switches the commands
    /// (`off` ↔ `commands`; from `output` it is off too); `o` switches what they print
    /// (`commands` ↔ `output`), and never switches the commands on by itself.
    fn shells_switch(&mut self, output: bool) {
        let (to, said) = match (self.config.kb.shell.as_str(), output) {
            ("off", false) => (
                "commands",
                "shell commands are kept from here on: the lines you run, not what they print (a private shell keeps nothing)",
            ),
            (_, false) => (
                "off",
                "nothing more of your shells is kept (what was kept stays until you forget it)",
            ),
            ("off", true) => {
                return self.toast(
                    "shell commands are not kept: c keeps them, then o keeps what they print too",
                );
            }
            ("output", true) => (
                "commands",
                "what shell commands print is no longer kept (the commands still are; what was kept stays)",
            ),
            (_, true) => (
                "output",
                "what shell commands print is kept from here on: the first 2 KiB and the last 6 KiB of each, never a full-screen program's or a command's you typed into · o stops it",
            ),
        };
        self.set_config("kb.shell", to, said);
        self.library_news();
    }

    // ── reading a folder (`kb.ingest`) ──────────────────────────────────────────────────────

    /// `i`, `kb.ingest`: which folder. The path picker (here, the repositories found on this
    /// computer, or a path to type), then the form; a linked machine's paths are typed.
    pub(crate) fn ingest_pick(&mut self) {
        let far = self.library().and_then(Library::far);
        if far.is_some() || self.connect.remote.is_some() {
            return self.open_ingest_form(None);
        }
        self.request(Pending::KbPaths, Request::Discover { refresh: false });
    }

    /// The repositories found on this computer arrived: the picker.
    pub(crate) fn ingest_paths(&mut self, paths: Vec<std::path::PathBuf>) {
        let mut items = vec![Item {
            label: "+ a path…".into(),
            detail: "type where the folder or the file is".into(),
            key: None,
            target: Target::NewProjectPath,
        }];
        if let Ok(here) = std::env::current_dir() {
            items.push(Item {
                label: "here".into(),
                detail: here.display().to_string(),
                key: None,
                target: Target::Path(here),
            });
        }
        // (A project is read by itself: only what isn't one yet is offered.)
        for p in paths.into_iter().filter(|p| !self.projects.iter().any(|x| x.root == *p)) {
            let label = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            items.push(Item {
                label,
                detail: p.display().to_string(),
                key: None,
                target: Target::Path(p),
            });
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Read which folder into the knowledge base?", items),
            purpose: Purpose::KbIngest,
        });
    }

    /// The linked machines, as a form's pick: this computer first, the one the Sources tab
    /// lists chosen. `None`: no machine is linked, so there is nothing to pick.
    fn machine_field(&self, hint: &str) -> Option<Field> {
        if self.machines.is_empty() {
            return None;
        }
        let mut options = vec![(String::new(), "this computer".to_owned())];
        options.extend(self.machines.iter().map(|m| (m.name.clone(), m.name.clone())));
        let far = self.library().and_then(Library::far);
        let at = far.and_then(|m| options.iter().position(|o| o.0 == m)).unwrap_or(0);
        Some(Field::choice("machine", "On", options, at, hint))
    }

    /// The ingest form: everything `ptyro ingest PATH` takes, defaults shown.
    pub(crate) fn open_ingest_form(&mut self, path: Option<std::path::PathBuf>) {
        let kb = &self.config.kb;
        let mut projects = vec![(String::new(), "none".to_owned())];
        projects.extend(
            self.projects
                .iter()
                .filter(|p| p.machine.is_none())
                .map(|p| (p.slug.clone(), p.slug.clone())),
        );
        let mut profiles = vec![(String::new(), "every profile's agents".to_owned())];
        profiles.extend(
            self.profiles
                .iter()
                .filter(|p| p.machine.is_none())
                .map(|p| (p.name.clone(), format!("{} [{}]", p.name, p.label))),
        );
        let mut fields = vec![
            Field::required(
                "path",
                "Folder or file",
                "~/notes",
                "What to read and keep reading: a whole path, or one under ~",
            )
            .with(path.map(|p| p.display().to_string()).unwrap_or_default()),
            Field::text("name", "Name", "(the folder's name)", "What the Sources tab calls it"),
            Field::choice("project", "Project", projects, 0, "The project it belongs to"),
            Field::choice(
                "profile",
                "Seen by",
                profiles,
                0,
                "Whose agents may find it with recall (you always find it)",
            ),
            Field::text(
                "include",
                "Only",
                "(every file)",
                "Only files that match: globs like docs/** or *.md, comma separated",
            ),
            Field::text(
                "exclude",
                "Not",
                "(none)",
                "Not the files that match: globs, comma separated",
            ),
            Field::yes_no(
                "comments",
                "Comments",
                kb.comments,
                "Read source files' comments (no: only their names)",
            ),
            Field::yes_no(
                "code",
                "Code",
                kb.code,
                "Read source code itself too, found by its words (it makes the knowledge base large)",
            ),
            Field::yes_no(
                "commits",
                "Commits",
                kb.commits,
                "Read the repository's commit messages",
            ),
            Field::yes_no("once", "Only now", false, "Read it now and never look at it again"),
        ];
        fields.extend(self.machine_field(
            "The computer whose folder it is: read there, into its knowledge base (a project or a profile goes by its name there)",
        ));
        let mut form = Form::new("Read a folder into the knowledge base", fields, Make::KbIngest);
        form.verb = "read it";
        self.overlay = Some(Overlay::Form(Box::new(form)));
    }

    /// The ingest form's ⏎: `KbIngest`, with what was filled in.
    pub(crate) fn ingest_submit(&mut self, form: &Form) {
        let machine = form.opt("machine");
        let spec = spec_of(form, machine.is_none() && self.connect.remote.is_none());
        self.request(Pending::KbIngested(machine.clone()), Request::KbIngest { spec, machine });
    }

    /// A root was added (or was one already) and is being read: said as the command says
    /// it, and shown on the Sources tab.
    pub(crate) fn ingested(&mut self, machine: Option<String>, root: &KbRootInfo, plan: &KbPlan) {
        self.show_library(|l| {
            (l.typing, l.reader, l.show_status, l.chip) = (false, None, false, None);
            (l.tab, l.of) = (Tab::Sources, None);
            if machine.is_some() {
                l.machine = machine;
            } else if l.machine.as_deref().is_some_and(|m| m != "here") {
                l.machine = None;
            }
        });
        self.toast(planned(&root.root.name, &root.root.path, plan));
        self.library_news();
    }

    // ── the agents' own history (`kb.backfill`) ─────────────────────────────────────────────

    /// The backfill's form: everything `ptyro ingest --history` takes.
    pub(crate) fn open_backfill_form(&mut self) {
        let agents = vec![
            (String::new(), "Claude Code's and Codex's".to_owned()),
            ("claude".to_owned(), "Claude Code's".to_owned()),
            ("codex".to_owned(), "Codex's".to_owned()),
        ];
        let mut profiles = vec![(String::new(), "every profile's folders".to_owned())];
        profiles.extend(
            self.profiles
                .iter()
                .filter(|p| p.machine.is_none())
                .map(|p| (p.name.clone(), format!("{} [{}]", p.name, p.label))),
        );
        let mut fields = vec![
            Field::choice("agent", "Whose", agents, 0, "Which agent's own folders to read"),
            Field::choice("profile", "Profile", profiles, 0, "Only this profile's folders"),
            Field::text(
                "since",
                "Since",
                "(everything not too old to keep)",
                "Only conversations written to since then: 30d, 12h, 2w",
            ),
            Field::yes_no("dry", "Only count", false, "Say how many there are, and read nothing"),
        ];
        fields.extend(self.machine_field("The computer whose agents' folders are read"));
        let mut form =
            Form::new("Read what your agents kept before ptyro did", fields, Make::KbBackfill);
        form.verb = "count them";
        self.overlay = Some(Overlay::Form(Box::new(form)));
    }

    /// The backfill form's ⏎: counted first (nothing is read before you saw how many).
    /// `Err`: what is wrong with a field.
    pub(crate) fn backfill_submit(&mut self, form: &Form) -> Result<(), (&'static str, String)> {
        let since_ms = match form.opt("since") {
            Some(age) => {
                let d = ptyro_core::config::parse_duration(&age)
                    .map_err(|e| ("since", format!("{age}: {e} (like 30d, 12h, 2w)")))?;
                Some(now_ms().saturating_sub(d.as_millis() as u64))
            }
            None => None,
        };
        let req = |dry_run| Request::KbHistory {
            agent: form.opt("agent"),
            profile: form.opt("profile"),
            since_ms,
            dry_run,
            machine: form.opt("machine"),
        };
        let read = form.get("dry").is_empty().then(|| Box::new(req(false)));
        self.request(Pending::KbCounted(read), req(true));
        Ok(())
    }

    /// The count arrived. With something to read and not only counting: the question, then
    /// the read (`read`); else the count is all there is to say.
    pub(crate) fn backfill_counted(
        &mut self,
        read: Option<Box<Request>>,
        plan: &ptyro_core::kb::KbHistoryPlan,
    ) {
        let text = plan.text();
        let first = text.lines().next().unwrap_or_default().to_owned();
        match read {
            Some(req) if plan.new > 0 => {
                let said = format!(
                    "reading {} conversation{} behind everything else: the Library's history lists them",
                    plan.new,
                    if plan.new == 1 { "" } else { "s" }
                );
                self.overlay = Some(Overlay::Confirm {
                    text: format!("{}\nRead them? [y/N]", text.trim_end()),
                    action: Confirmed::Again(req, Box::new(Pending::KbBackfilled(said))),
                });
            }
            _ => self.toast(first),
        }
    }
}
