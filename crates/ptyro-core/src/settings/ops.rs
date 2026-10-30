//! Changing settings in files, and what the panel shows of them (ROADMAP CFG1): the layer a
//! change goes to, one [`Op`] applied to it, and a [`View`] of every setting with where its value
//! comes from. The CLI, the TUI and the web all call these, so they agree.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Apply, Entry, Extra, Lookups, Origin, Scope, Source};
use crate::config::Config;
use crate::project::{self, ProjectConfig};

/// A file a change goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    /// Your config (`$PTYRO_CONFIG`, or `~/.config/ptyro/config.toml`).
    User(PathBuf),
    /// `<repo>/.ptyro.toml`: committed, shared with everyone who clones it.
    Shared(PathBuf),
    /// `<repo>/.ptyro.local.toml`: yours alone, kept out of git.
    Local(PathBuf),
}

impl Layer {
    pub fn scope(&self) -> Scope {
        match self {
            Self::User(_) => Scope::User,
            Self::Shared(_) | Self::Local(_) => Scope::Project,
        }
    }

    pub fn origin(&self) -> Origin {
        match self {
            Self::User(_) => Origin::User,
            Self::Shared(_) => Origin::Project,
            Self::Local(_) => Origin::Local,
        }
    }

    pub fn path(&self) -> PathBuf {
        match self {
            Self::User(p) => p.clone(),
            Self::Shared(root) => root.join(project::FILE),
            Self::Local(root) => root.join(project::LOCAL_FILE),
        }
    }

    /// A repository's file is a file of its own: a link there (the repository is someone
    /// else's) is never followed, read or written.
    fn check_plain(&self) -> Result<(), String> {
        if let Self::Shared(_) | Self::Local(_) = self
            && std::fs::symlink_metadata(self.path()).is_ok_and(|m| !m.is_file())
        {
            return Err(format!(
                "{} is not a plain file (a link?): edit it by hand",
                self.path().display()
            ));
        }
        Ok(())
    }

    /// The file's text; a file that isn't there yet is empty.
    pub fn read(&self) -> Result<String, String> {
        self.check_plain()?;
        match std::fs::read_to_string(self.path()) {
            Ok(t) => Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("{}: {e}", self.path().display())),
        }
    }

    /// Write the file whole (the user's config private to them; the local one kept out of git).
    pub fn write(&self, text: &str) -> Result<(), String> {
        let path = self.path();
        self.check_plain()?;
        match self {
            Self::User(_) => {
                crate::config::write_private(&path, text).map_err(|e| e.to_string())?
            }
            Self::Shared(_) | Self::Local(_) => {
                // Whole or not at all: a daemon reading it meanwhile never sees half.
                let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
                std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
                std::fs::rename(&tmp, &path).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    format!("{}: {e}", path.display())
                })?;
            }
        }
        if let Self::Local(root) = self {
            exclude_from_git(root);
        }
        Ok(())
    }

    /// Hold the file's lock while it is read, changed and written, so two changes made at the
    /// same moment (the web, the TUI, the CLI) both land. Released when dropped.
    fn lock(&self) -> Option<std::fs::File> {
        let path = self.path();
        let name = format!("{}.lock", path.file_name()?.to_string_lossy());
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_file_name(name))
            .ok()?;
        f.lock().ok()?;
        Some(f)
    }

    /// The repository's two files, or the user's.
    pub fn files(&self) -> Vec<Layer> {
        match self {
            Self::User(_) => vec![self.clone()],
            Self::Shared(r) | Self::Local(r) => {
                vec![Self::Shared(r.clone()), Self::Local(r.clone())]
            }
        }
    }
}

/// `.ptyro.local.toml` is never committed by accident: `.git/info/exclude`, never `.gitignore`.
fn exclude_from_git(root: &Path) {
    let git = root.join(".git");
    if !git.is_dir() {
        return;
    }
    let info = git.join("info");
    let _ = std::fs::create_dir_all(&info);
    let path = info.join("exclude");
    let line = format!("/{}", project::LOCAL_FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == line) {
        return;
    }
    let mut new = text;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(&line);
    new.push('\n');
    let _ = std::fs::write(path, new);
}

/// One change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    Add {
        path: String,
        name: String,
        fields: Vec<(String, String)>,
    },
    Remove {
        path: String,
        name: String,
    },
    Rename {
        path: String,
        from: String,
        to: String,
    },
    /// Key bindings, several at once as one change (a swap is one undo): each `[keys]` name gets
    /// `Some(action)` (`"none"` unbinds) or `None` (back to its default). See [`super::bind`].
    Bind {
        changes: Vec<(String, Option<String>)>,
    },
}

impl Op {
    /// The settings it touches, for when it takes effect.
    fn keys(&self) -> Vec<String> {
        match self {
            Self::Set { key, .. } | Self::Unset { key } => vec![key.clone()],
            Self::Add { path, .. } | Self::Remove { path, .. } | Self::Rename { path, .. } => {
                vec![path.clone()]
            }
            Self::Bind { .. } => vec!["keys".to_owned()],
        }
    }
}

/// What a change did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// The file as it was, to put back (undo).
    pub before: String,
    pub after: String,
    /// The file's text did change (a value set to what it was isn't a change).
    pub changed: bool,
    /// Things to tell the person: when it applies, what to do next.
    pub notes: Vec<String>,
    /// The slowest way any of it applies.
    pub apply: Apply,
}

/// How a change applies: the slowest of its settings; a collection's members apply as their
/// fields do.
fn applies(keys: &[String]) -> Apply {
    let mut worst = Apply::Live;
    for k in keys {
        let a = super::find(k)
            .map(|s| s.apply)
            .or_else(|| {
                super::collection(k).and_then(|c| super::fields_of(c).next().map(|s| s.apply))
            })
            .or_else(|| {
                super::member_of(k).and_then(|(c, _)| super::fields_of(c).next().map(|s| s.apply))
            })
            .unwrap_or(Apply::Live);
        worst = match (worst, a) {
            (Apply::Restart, _) | (_, Apply::Restart) => Apply::Restart,
            (Apply::Next, _) | (_, Apply::Next) => Apply::Next,
            _ => Apply::Live,
        };
    }
    worst
}

/// The file `op` would make from `before`, checked, without writing it.
fn compute(
    layer: &Layer,
    before: &str,
    op: &Op,
    lk: &Lookups,
    extra: Extra<'_>,
) -> Result<String, String> {
    let scope = layer.scope();
    // The caller's own check judges the change, not what was already wrong: a config with one
    // bad key binding can still have its other settings changed.
    let broken_before = extra(before).is_err();
    let extra = |new: &str| if broken_before { Ok(()) } else { extra(new) };
    // The theme in use goes with its theme: back to the default when it is removed, to the new
    // name when it is renamed (otherwise the config would name a theme that isn't there).
    let followed = theme_follow(scope, before, op, lk)?;
    let before = followed.as_deref().unwrap_or(before);
    let after = match op {
        Op::Set { key, value } => super::set(scope, before, key, value, lk, &extra),
        Op::Unset { key } => super::unset(scope, before, key, &extra),
        Op::Add { path, name, fields } => {
            let f: Vec<(&str, &str)> =
                fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            super::add(scope, before, path, name, &f, lk, &extra)
        }
        Op::Remove { path, name } => super::remove(scope, before, path, name, &extra),
        Op::Rename { path, from, to } => super::rename(scope, before, path, from, to, &extra),
        Op::Bind { changes } => super::bind(scope, before, changes, lk, &extra),
    }?;
    // A repository's two files are one config: a partial override is fine when the other file
    // completes it, and the merge is what must load.
    if let Layer::Shared(_) | Layer::Local(_) = layer {
        let other = match layer {
            Layer::Shared(r) => Layer::Local(r.clone()),
            _ => Layer::Shared(match layer {
                Layer::Local(r) => r.clone(),
                _ => unreachable!("matched above"),
            }),
        };
        let other_text = other.read()?;
        let (shared, local) = if matches!(layer, Layer::Shared(_)) {
            (&after, &other_text)
        } else {
            (&other_text, &after)
        };
        check_merged(shared, local)?;
    }
    Ok(after)
}

/// The theme `ui.theme` names in `text`, if the file sets it.
fn theme_in_use(text: &str) -> Option<String> {
    let doc = super::parse_doc(text).ok()?;
    let item = super::item_at(&doc, &["ui".to_owned(), "theme".to_owned()])?;
    item.as_str().map(str::to_owned)
}

/// `before` with `ui.theme` moved off the theme `op` removes or renames, or `None` when it isn't
/// the one in use.
fn theme_follow(
    scope: Scope,
    before: &str,
    op: &Op,
    lk: &Lookups,
) -> Result<Option<String>, String> {
    let none = |_: &str| Ok(());
    let moved = match op {
        Op::Remove { path, name } if scope == Scope::User && path == "themes" => {
            (theme_in_use(before).as_deref() == Some(name)).then_some(None)
        }
        Op::Rename { path, from, to } if scope == Scope::User && path == "themes" => {
            (theme_in_use(before).as_deref() == Some(from)).then(|| Some(to.trim()))
        }
        _ => None,
    };
    Ok(match moved {
        None => None,
        Some(None) => Some(super::unset(scope, before, "ui.theme", &none)?),
        Some(Some(to)) => Some(super::set(scope, before, "ui.theme", to, lk, &none)?),
    })
}

/// What a theme change did to `ui.theme`, to tell the person.
fn theme_note(before: &str, op: &Op) -> Option<String> {
    let used = theme_in_use(before)?;
    match op {
        Op::Remove { path, name } if path == "themes" && *name == used => {
            Some(format!("{name} was the theme in use, so the default theme is back"))
        }
        Op::Rename { path, from, to } if path == "themes" && *from == used => {
            Some(format!("{to} is still the theme in use"))
        }
        _ => None,
    }
}

/// Whether `.ptyro.toml` and `.ptyro.local.toml`, merged, make a project config that loads.
fn check_merged(shared: &str, local: &str) -> Result<(), String> {
    fn merge(into: &mut toml::Table, from: toml::Table) {
        for (k, v) in from {
            match (into.get_mut(&k), v) {
                (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge(a, b),
                (_, v) => {
                    into.insert(k, v);
                }
            }
        }
    }
    let mut merged = ProjectConfig::parse(shared)?;
    merge(&mut merged, ProjectConfig::parse(local)?);
    let c: ProjectConfig =
        merged.try_into().map_err(|e: toml::de::Error| e.to_string().trim_end().to_owned())?;
    if c.project.base.as_deref().is_some_and(|b| b.trim_start().starts_with('-')) {
        return Err("project.base: must not start with `-`".into());
    }
    if let Some(t) = &c.worktree.branch_template {
        crate::model::check_branch_template(t)
            .map_err(|e| format!("worktree.branch_template: {e}"))?;
    }
    Ok(())
}

/// Whether `op` would be accepted, and the file it would make: nothing is written. A form
/// checks as you type with this.
pub fn preview(layer: &Layer, op: &Op, lk: &Lookups, extra: Extra<'_>) -> Result<String, String> {
    compute(layer, &layer.read()?, op, lk, extra)
}

/// Make `op` in `layer`'s file and write it. Nothing is written unless the whole new file is
/// valid (`extra` is the caller's own check of it).
pub fn apply(layer: &Layer, op: &Op, lk: &Lookups, extra: Extra<'_>) -> Result<Done, String> {
    // The user's config is shared with the daemon's own edits and every other client; a
    // repository's files are not given a stray lock file.
    let _lock = if matches!(layer, Layer::User(_)) { layer.lock() } else { None };
    let before = layer.read()?;
    let after = compute(layer, &before, op, lk, extra)?;
    let changed = after != before;
    let keys = op.keys();
    let apply = applies(&keys);
    let mut notes = Vec::new();
    if changed {
        layer.write(&after)?;
        notes.push(apply.says().to_owned());
        if matches!(layer, Layer::Shared(_)) {
            notes.push(format!(
                "written to {}, which is committed: commit it to share it",
                project::FILE
            ));
        }
        notes.extend(theme_note(&before, op));
        if keys.iter().any(|k| k.starts_with("scripts.")) {
            notes.push(
                "its scripts changed, so ptyro asks you to trust them again before they run"
                    .to_owned(),
            );
        }
    } else {
        notes.push("already like that".to_owned());
    }
    Ok(Done { before, after, changed, notes, apply })
}

/// Put a file back as it was (undo), after checking it still loads. With `expect`, only when
/// the file still reads exactly that (what the change being undone left).
pub fn restore(
    layer: &Layer,
    text: &str,
    expect: Option<&str>,
    extra: Extra<'_>,
) -> Result<(), String> {
    // Only what was made is undone: a file someone else has changed since is left alone.
    if let Some(expect) = expect
        && layer.read()? != expect
    {
        return Err(format!(
            "{} changed since that change, so it isn't put back over what is there now",
            layer.path().display()
        ));
    }
    super::check(layer.scope(), text)?;
    extra(text)?;
    layer.write(text)
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Themes => "themes",
            Self::BuiltinThemes => "builtin_themes",
            Self::Agents => "agents",
            Self::Editors => "editors",
            Self::Actions => "actions",
            Self::RunScripts => "run_scripts",
            Self::ChatPresets => "chat_presets",
        }
    }
}

const SOURCES: [Source; 7] = [
    Source::Themes,
    Source::BuiltinThemes,
    Source::Agents,
    Source::Editors,
    Source::Actions,
    Source::RunScripts,
    Source::ChatPresets,
];

/// Everything the panel needs to show a family of settings.
#[derive(Debug, Clone)]
pub struct View {
    pub scope: Scope,
    pub entries: Vec<Entry>,
    /// The bindings or prompts of each map collection: `(name, value)`.
    pub maps: Vec<(String, Vec<(String, String)>)>,
    /// Every key binding, defaults included (your config only).
    pub bindings: Vec<super::Binding>,
    /// The names in each table collection, in the file's order.
    pub members: Vec<(String, Vec<String>)>,
    /// What each dynamic choice offers: `(name, what it is)`.
    pub options: Vec<(&'static str, Vec<(String, String)>)>,
    /// The files, and whether each is there.
    pub files: Vec<(Origin, PathBuf, bool)>,
    /// Why the config doesn't load, if it doesn't: the panel still opens, on defaults.
    pub problem: Option<String>,
}

/// Read the files of `layer`'s family and list every setting with its value and where it is from.
pub fn view(layer: &Layer, lk: &Lookups) -> View {
    let scope = layer.scope();
    let layers = layer.files();
    let texts: Vec<String> = layers.iter().map(|l| l.read().unwrap_or_default()).collect();
    let files: Vec<(Origin, &str)> =
        layers.iter().zip(&texts).map(|(l, t)| (l.origin(), t.as_str())).collect();
    let mut problem = None;
    let (root, config, pconfig) = match layer {
        Layer::User(path) => {
            let c = Config::load(path).unwrap_or_else(|e| {
                problem = Some(e.to_string());
                Config::default()
            });
            (serde_json::to_value(&c).unwrap_or_default(), c, None)
        }
        Layer::Shared(r) | Layer::Local(r) => {
            let p = ProjectConfig::load(r, r).unwrap_or_else(|e| {
                problem = Some(e);
                ProjectConfig::default()
            });
            (serde_json::to_value(&p).unwrap_or_default(), Config::default(), Some(p))
        }
    };
    let entries = super::entries(scope, &root, &files);
    let in_scope = |sec: &str| super::section(sec).is_some_and(|s| s.scope == scope);
    let mut maps = Vec::new();
    let mut members = Vec::new();
    // The names, in the order the files have them (the merged value sorts them).
    for col in super::COLLECTIONS.iter().filter(|c| in_scope(c.section)) {
        match col.layout {
            super::Layout::Map(_) => {
                maps.push((col.path.to_owned(), super::map_members(&root, col.path)))
            }
            _ => {
                let mut names = Vec::new();
                for t in &texts {
                    for n in super::members(t, col.path) {
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                }
                members.push((col.path.to_owned(), names));
            }
        }
    }
    let bindings = if in_scope("keys") { super::bindings(&root, lk) } else { Vec::new() };
    let options = SOURCES
        .iter()
        .map(|s| (s.name(), super::options(*s, &config, pconfig.as_ref(), lk)))
        .collect();
    let files = layers.iter().map(|l| (l.origin(), l.path(), l.path().exists())).collect();
    View { scope, entries, maps, bindings, members, options, files, problem }
}

impl Entry {
    /// For the web: the entry without the schema it points at (the client has that already).
    pub fn to_json(&self) -> Value {
        // A token isn't sent where it is only shown: a program that prints it is (that isn't one).
        let value = if matches!(self.setting.kind, super::Kind::Secret) {
            match &self.value {
                Value::String(_) => json!("••••"),
                other => other.clone(),
            }
        } else {
            self.value.clone()
        };
        json!({
            "key": self.key,
            "setting": self.setting.key,
            "value": value,
            "default": self.default,
            "shown": self.shown(),
            "default_shown": self.default_shown(),
            "origin": self.origin,
            "changed": self.changed(),
            "line": self.line,
            "member": self.member,
        })
    }
}

impl View {
    pub fn to_json(&self) -> Value {
        json!({
            "scope": self.scope,
            "entries": self.entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
            "maps": self.maps.iter().map(|(p, m)| json!({ "path": p, "items": m })).collect::<Vec<_>>(),
            "bindings": self.bindings,
            "members": self.members.iter().map(|(p, m)| json!({ "path": p, "names": m })).collect::<Vec<_>>(),
            "options": self.options.iter().map(|(s, o)| json!({ "source": s, "options": o })).collect::<Vec<_>>(),
            "files": self.files.iter().map(|(o, p, e)| json!({ "origin": o, "path": p, "exists": e })).collect::<Vec<_>>(),
            "problem": self.problem,
        })
    }
}

/// The schema as JSON for a client that builds its own forms (the web app, other tools).
pub fn schema_json(scope: Scope) -> Value {
    json!({
        "sections": super::SECTIONS.iter().filter(|s| s.scope == scope).collect::<Vec<_>>(),
        "settings": super::settings(scope).collect::<Vec<_>>(),
        "collections": super::COLLECTIONS
            .iter()
            .filter(|c| super::section(c.section).is_some_and(|s| s.scope == scope))
            .collect::<Vec<_>>(),
    })
}
