//! `ptyro config path|check|show|edit|list|get|set|unset|add|rm|rename|schema` (SPEC §13, CFG1).

use std::io::{IsTerminal, Write};

use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_core::settings::{self, Layer, Lookups, Op, Scope};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};

use crate::ConfigCmd;
use crate::cli::Ctx;

/// What the settings schema can't know: the themes and actions the TUI has.
pub(crate) fn lookups() -> Lookups {
    Lookups {
        themes: ptyro_tui::theme::BUILTIN.iter().map(|s| (*s).to_owned()).collect(),
        actions: ptyro_tui::keymap::ACTIONS
            .iter()
            .map(|a| (a.id.to_owned(), a.desc.to_owned()))
            .collect(),
    }
}

/// The checks only the TUI can make of a whole user config: key bindings and themes.
pub(crate) fn semantic(text: &str) -> Result<(), String> {
    let config = Config::parse(text)?;
    semantic_config(&config)
}

fn semantic_config(config: &Config) -> Result<(), String> {
    let problems = ptyro_tui::keymap::validate(config);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    if !ptyro_tui::theme::exists(config, &config.ui.theme) {
        return Err(format!(
            "ui.theme: unknown theme {:?} (built in: {}; or define [themes.{}])",
            config.ui.theme,
            ptyro_tui::theme::BUILTIN.join(", "),
            config.ui.theme
        ));
    }
    Ok(())
}

/// The file a change goes to: yours, or with `--project` the repository's (`.ptyro.local.toml`,
/// or `.ptyro.toml` with `--shared`).
async fn layer(ctx: &Ctx, project: Option<&str>, shared: bool) -> Result<Layer, Error> {
    let Some(project) = project else { return Ok(Layer::User(Config::path(&ctx.paths))) };
    let addr = if project.is_empty() {
        std::env::current_dir()
            .map_err(|e| Error::internal(format!("the current directory: {e}")))?
            .to_string_lossy()
            .into_owned()
    } else {
        project.to_owned()
    };
    let c = ctx.connect(false).await?;
    let Reply::Project { info, .. } = c.request(Request::ProjectGet { addr }).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    Ok(if shared { Layer::Shared(info.root) } else { Layer::Local(info.root) })
}

fn invalid(e: String) -> Error {
    Error::new(ErrorCode::Invalid, e)
}

/// A value from the command line: several words make a list or a command.
fn value_of(key: &str, words: &[String]) -> String {
    let listy = settings::find(key).is_some_and(|s| {
        matches!(
            s.kind,
            settings::Kind::List
                | settings::Kind::Argv
                | settings::Kind::Keys
                | settings::Kind::Set { .. }
        )
    });
    if listy && words.len() > 1 {
        serde_json::to_string(words).unwrap_or_default()
    } else {
        words.join(" ")
    }
}

/// Make the change and say what happened and when it applies.
fn change(layer: &Layer, op: &Op, what: &str) -> Result<(), Error> {
    let done = settings::apply(layer, op, &lookups(), &extra_for(layer)).map_err(invalid)?;
    if done.changed {
        eprintln!("{what}");
    }
    for n in &done.notes {
        eprintln!("  {n}");
    }
    if done.apply == settings::Apply::Restart && done.changed {
        eprintln!("  restart it with `ptyro daemon restart`");
    }
    Ok(())
}

fn extra_for(layer: &Layer) -> impl Fn(&str) -> Result<(), String> + use<> {
    let user = layer.scope() == Scope::User;
    move |text: &str| if user { semantic(text) } else { Ok(()) }
}

pub async fn run(ctx: &Ctx, cmd: ConfigCmd) -> Result<i32, Error> {
    run_paths(&ctx.paths, cmd, ctx).await
}

async fn run_paths(paths: &Paths, cmd: ConfigCmd, ctx: &Ctx) -> Result<i32, Error> {
    let path = Config::path(paths);
    match cmd {
        ConfigCmd::List { section, changed, project, json } => {
            let layer = layer(ctx, project.as_deref(), false).await?;
            list(&layer, section.as_deref(), changed, json)
        }
        ConfigCmd::Get { key, explain, project, json } => {
            let layer = layer(ctx, project.as_deref(), false).await?;
            get(&layer, &key, explain, json)
        }
        ConfigCmd::Set { key, value, project, shared } => {
            let layer = layer(ctx, project.as_deref(), shared).await?;
            let v = value_of(&key, &value);
            change(&layer, &Op::Set { key: key.clone(), value: v }, &format!("{key} is set"))?;
            Ok(0)
        }
        ConfigCmd::Unset { keys, project, shared } => {
            let layer = layer(ctx, project.as_deref(), shared).await?;
            for key in keys {
                change(
                    &layer,
                    &Op::Unset { key: key.clone() },
                    &format!("{key} is back to its default"),
                )?;
            }
            Ok(0)
        }
        ConfigCmd::Add { collection, name, fields, project, shared } => {
            let layer = layer(ctx, project.as_deref(), shared).await?;
            // A sink has no name: whatever stands where it would is its first field.
            let (name, fields) = match (&name, settings::collection(&collection).map(|c| c.layout))
            {
                (Some(n), Some(settings::Layout::Array)) => {
                    (None, std::iter::once(n.clone()).chain(fields).collect::<Vec<_>>())
                }
                _ => (name, fields),
            };
            let mut f = Vec::new();
            for x in &fields {
                let (k, v) = x.split_once('=').ok_or_else(|| {
                    invalid(format!(
                        "{x:?}: say FIELD=VALUE, like command=\"aider --model sonnet\""
                    ))
                })?;
                f.push((k.to_owned(), v.to_owned()));
            }
            let name = name.unwrap_or_default();
            change(
                &layer,
                &Op::Add { path: collection.clone(), name: name.clone(), fields: f },
                &format!("added {collection} {name}"),
            )?;
            Ok(0)
        }
        ConfigCmd::Rm { collection, name, project, shared } => {
            let layer = layer(ctx, project.as_deref(), shared).await?;
            change(
                &layer,
                &Op::Remove { path: collection.clone(), name: name.clone() },
                &format!("removed {collection} {name}"),
            )?;
            Ok(0)
        }
        ConfigCmd::Rename { collection, from, to, project, shared } => {
            let layer = layer(ctx, project.as_deref(), shared).await?;
            change(
                &layer,
                &Op::Rename { path: collection.clone(), from: from.clone(), to: to.clone() },
                &format!("{from} is now {to}"),
            )?;
            Ok(0)
        }
        ConfigCmd::Schema { project } => {
            let scope = if project.is_some() { Scope::Project } else { Scope::User };
            println!(
                "{}",
                serde_json::to_string_pretty(&settings::schema_json(scope)).unwrap_or_default()
            );
            Ok(0)
        }
        ConfigCmd::Path => {
            println!("{}", path.display());
            Ok(0)
        }
        ConfigCmd::Check { json: true } => {
            let (ok, notes, error) = match check(&path) {
                Ok(n) => (true, n, None),
                Err(e) => (false, Vec::new(), Some(e)),
            };
            let v = serde_json::json!({ "path": path, "exists": path.exists(), "ok": ok, "notes": notes, "error": error });
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(if ok { 0 } else { ErrorCode::Invalid.exit_code() })
        }
        ConfigCmd::Check { json: false } => match check(&path) {
            Ok(notes) => {
                let what = if path.exists() {
                    path.display().to_string()
                } else {
                    "no config file (all defaults)".into()
                };
                println!("ok: {what}");
                for n in notes {
                    println!("note: {n}");
                }
                Ok(0)
            }
            Err(e) => Err(Error::new(ErrorCode::Invalid, e)),
        },
        ConfigCmd::Show { effective: _, json } => {
            let config =
                Config::load(&path).map_err(|e| Error::new(ErrorCode::Invalid, e.to_string()))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&config).unwrap_or_default());
                return Ok(0);
            }
            let text =
                toml::to_string_pretty(&config).map_err(|e| Error::internal(e.to_string()))?;
            println!("# effective configuration ({}; defaults filled in)\n{text}", path.display());
            Ok(0)
        }
        ConfigCmd::Edit { key } => edit(&path, key.as_deref()),
    }
}

fn list(layer: &Layer, section: Option<&str>, changed: bool, json: bool) -> Result<i32, Error> {
    let view = settings::view(layer, &lookups());
    let mut entries: Vec<&settings::Entry> = view.entries.iter().collect();
    if let Some(s) = section {
        entries.retain(|e| {
            e.setting.section == s || e.key == s || e.key.starts_with(&format!("{s}."))
        });
        if entries.is_empty() && view.maps.iter().all(|(p, _)| p != s) {
            let known: Vec<&str> = settings::SECTIONS
                .iter()
                .filter(|x| x.scope == layer.scope())
                .map(|x| x.id)
                .collect();
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("nothing under {s:?}; sections: {}", known.join(", ")),
            ));
        }
    }
    if changed {
        entries.retain(|e| e.changed());
    }
    if json {
        let v = serde_json::json!({
            "problem": view.problem,
            "files": view.to_json()["files"],
            "settings": entries.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "maps": view.maps.iter().filter(|(p, _)| section.is_none_or(|s| s == p || settings::collection(p).is_some_and(|c| c.section == s))).map(|(p, m)| serde_json::json!({ "path": p, "items": m })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(0);
    }
    if let Some(p) = &view.problem {
        eprintln!(
            "note: the config doesn't load ({p}); defaults are shown. Fix it with `ptyro config edit`."
        );
    }
    let width = entries.iter().map(|e| e.key.len()).max().unwrap_or(0);
    let mut last = "";
    for e in &entries {
        if e.setting.section != last {
            last = e.setting.section;
            let title = settings::section(last).map_or(last, |s| s.title);
            println!("\n{title}");
        }
        let from = if e.changed() { format!("  ← {}", e.origin.says()) } else { String::new() };
        println!("  {:<width$}  {}{from}", e.key, e.shown());
    }
    for (path, items) in &view.maps {
        if section
            .is_some_and(|s| s != path && settings::collection(path).is_none_or(|c| c.section != s))
            || items.is_empty()
        {
            continue;
        }
        println!("\n{path}");
        for (k, v) in items {
            println!("  {k} = {v}");
        }
    }
    println!();
    Ok(0)
}

fn get(layer: &Layer, key: &str, explain: bool, json: bool) -> Result<i32, Error> {
    let view = settings::view(layer, &lookups());
    let split = settings::split(key);
    // A binding or a prompt.
    if let Some((c, name)) = settings::member_of(key)
        && matches!(c.layout, settings::Layout::Map(_))
    {
        let value = view
            .maps
            .iter()
            .find(|(p, _)| p == c.path)
            .and_then(|(_, m)| m.iter().find(|(k, _)| *k == name))
            .map(|(_, v)| v.clone());
        let Some(value) = value else {
            return Err(Error::new(ErrorCode::NotFound, format!("{key} isn't set")));
        };
        println!(
            "{}",
            if json { serde_json::to_string(&value).unwrap_or_default() } else { value }
        );
        return Ok(0);
    }
    let Some(e) = view.entries.iter().find(|e| e.key == key) else {
        if settings::find(key).is_some() && split.len() > 2 {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "{key}: there is no such {}",
                    settings::member_of(key).map_or("entry", |(c, _)| c.noun)
                ),
            ));
        }
        return Err(Error::new(ErrorCode::NotFound, settings::unknown(key)));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&e.to_json()).unwrap_or_default());
        return Ok(0);
    }
    if explain {
        println!("{} = {}", e.key, e.shown());
        println!("  {}", e.setting.help);
        println!("  default: {}", e.default_shown());
        let from = match (e.origin, e.line) {
            (settings::Origin::Default, _) => "the default".to_owned(),
            (o, Some(l)) => format!("{} line {l}", o.says()),
            (o, None) => o.says().to_owned(),
        };
        println!("  from:    {from}");
        println!("  {}", e.setting.apply.says());
        if let settings::Kind::Choice { choices } | settings::Kind::Set { choices } = e.setting.kind
        {
            for c in choices {
                println!("    {:<14} {}", c.value, c.help);
            }
        }
        return Ok(0);
    }
    // For scripts: the bare value, a list one per line.
    match &e.value {
        serde_json::Value::Null => {}
        serde_json::Value::String(s) => println!("{s}"),
        serde_json::Value::Array(a) => {
            for x in a {
                println!("{}", x.as_str().map_or_else(|| x.to_string(), str::to_owned));
            }
        }
        serde_json::Value::Bool(b) => println!("{b}"),
        other => println!("{other}"),
    }
    Ok(0)
}

/// Validate the file, including key bindings and action names; returns notes.
fn check(path: &std::path::Path) -> Result<Vec<String>, String> {
    check_config(Config::load(path).map_err(|e| e.to_string())?, path)
}

/// `check` for text that isn't saved yet (the web app's editor).
pub(crate) fn check_text(text: &str, path: &std::path::Path) -> Result<Vec<String>, String> {
    check_config(Config::parse(text).map_err(|e| e.to_string())?, path)
}

fn check_config(config: Config, path: &std::path::Path) -> Result<Vec<String>, String> {
    let mut notes: Vec<String> = config
        .reserved_in_use()
        .into_iter()
        .map(|(section, milestone)| {
            format!("[{section}] is accepted but not used until {milestone}")
        })
        .collect();
    semantic_config(&config)?;
    if !path.exists() {
        notes.push(format!("create {} to change settings (`ptyro config edit`)", path.display()));
    }
    Ok(notes)
}

/// Write the config (or any file that may hold secrets) atomically: the file 0600, directories
/// created for it 0700. Replacing a file also tightens an older, looser one.
pub(crate) fn write_private(path: &std::path::Path, text: &str) -> Result<(), Error> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let err =
        |p: &std::path::Path, e: std::io::Error| Error::internal(format!("{}: {e}", p.display()));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| err(dir, e))?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| err(&tmp, e))?;
    // (A leftover temp file keeps its old mode through `open`.)
    f.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| err(&tmp, e))?;
    f.write_all(text.as_bytes()).map_err(|e| err(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| err(path, e))
}

/// The file's mode when group or others can read or write it.
pub(crate) fn too_open(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode() & 0o777;
    (mode & 0o066 != 0).then_some(mode)
}

/// The editor's own way to open a file at a line: `+N` for the vi family, nano and emacs.
fn at_line(editor: &str, line: usize) -> String {
    let prog = editor.split_whitespace().next().unwrap_or("");
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    match prog {
        "vi" | "vim" | "nvim" | "nano" | "emacs" | "emacsclient" | "less" | "view" => {
            format!(" +{line}")
        }
        _ => String::new(),
    }
}

fn edit(path: &std::path::Path, key: Option<&str>) -> Result<i32, Error> {
    if !path.exists() {
        write_private(path, &settings::starter())?;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let jump = match key {
        Some(k) => {
            let view = settings::view(&Layer::User(path.to_path_buf()), &lookups());
            match view.entries.iter().find(|e| e.key == k) {
                Some(e) => e.line.map(|l| at_line(&editor, l)).unwrap_or_default(),
                None if settings::member_of(k).is_some() => String::new(),
                None => return Err(Error::new(ErrorCode::NotFound, settings::unknown(k))),
            }
        }
        None => String::new(),
    };
    loop {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor}{jump} \"$1\""))
            .arg("sh")
            .arg(path)
            .status()
            .map_err(|e| Error::internal(format!("{editor}: {e}")))?;
        if !status.success() {
            return Err(Error::internal(format!("{editor} exited with {status}")));
        }
        match check(path) {
            Ok(notes) => {
                println!("ok: {}", path.display());
                for n in notes {
                    println!("note: {n}");
                }
                return Ok(0);
            }
            Err(e) => {
                eprintln!("ptyro: {e}");
                if !std::io::stdin().is_terminal() {
                    return Ok(ErrorCode::Invalid.exit_code());
                }
                eprint!("edit again? [Y/n] ");
                let _ = std::io::stderr().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                if answer.trim().eq_ignore_ascii_case("n") {
                    return Ok(ErrorCode::Invalid.exit_code());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn configs_are_written_private() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("new/ptyro/config.toml");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        write_private(&path, "[ui]\n").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&d.path().join("new")), 0o700);
        assert_eq!(too_open(&path), None);
        // An older, world-readable file (and a stale temp file) get tightened.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(too_open(&path), Some(0o644));
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, "").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();
        write_private(&path, "[ui]\nmouse = false\n").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui]\nmouse = false\n");
    }
}
