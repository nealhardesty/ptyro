//! Other tools' committed repository config, read as setup/run sources (M5.10, R-WT-03). Teams
//! that already wrote a Conductor, Superset, Codex, Claude Desktop, Cursor or Copilot setup get
//! it in ptyro for free: their commands fill whatever `.ptyro.toml` leaves unset, show in the
//! trust dialog with where they came from, and are fingerprinted like ptyro's own (SPEC §10.5).
//!
//! Formats as documented on 2026-10-20 (SPEC §20.7). Read-only; nothing here runs anything.
//! `[project] import = false` in `.ptyro.toml` turns it off.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::project::{RunMode, RunScript, Scripts};

/// What one file contributes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Imported {
    /// The tool: `conductor`, `superset`, `codex`, `claude`, `cursor`, `copilot`.
    pub tool: &'static str,
    /// Repository-relative path of the file read.
    pub file: String,
    pub scripts: Scripts,
    /// What couldn't be carried over, for the user.
    pub notes: Vec<String>,
}

/// In order of precedence when two define the same script.
pub const FILES: &[(&str, &str)] = &[
    ("conductor", ".conductor/settings.toml"),
    ("conductor", "conductor.json"),
    ("superset", ".superset/config.json"),
    ("codex", ".codex/environments/"),
    ("claude", ".claude/launch.json"),
    ("cursor", ".cursor/environment.json"),
    ("copilot", ".github/workflows/copilot-setup-steps.yml"),
];

/// Everything importable in `checkout`, in precedence order.
pub fn discover(checkout: &Path) -> Vec<Imported> {
    let mut out = Vec::new();
    let conductor_toml = checkout.join(".conductor/settings.toml");
    if conductor_toml.is_file() {
        out.extend(read(checkout, ".conductor/settings.toml", conductor));
    } else {
        // The legacy file is ignored once settings.toml exists (Conductor's own rule).
        out.extend(read(checkout, "conductor.json", conductor_json));
    }
    out.extend(read(checkout, ".superset/config.json", |t| superset(checkout, t)));
    if let Some(f) = codex_file(checkout) {
        out.extend(read(checkout, &f, codex));
    }
    out.extend(read(checkout, ".claude/launch.json", claude_launch));
    out.extend(read(checkout, ".cursor/environment.json", cursor));
    out.extend(read(checkout, ".github/workflows/copilot-setup-steps.yml", copilot));
    out.retain(|i| !i.scripts.is_empty() || !i.notes.is_empty());
    out
}

/// The most a repository's config or another tool's setup file may be: they are small, and what
/// a cloned repository sends us is not to be read without bounds.
pub const MAX_FILE: u64 = 1 << 20;
/// The most a YAML file may be (a YAML parser can expand aliases into a very large tree).
const MAX_YAML: usize = 256 << 10;

/// Read a file a repository (or you) put in a checkout: never through a symlink a clone brought
/// (`.ptyro.toml -> /dev/zero`, `.cursor/environment.json -> ~/.aws/credentials`), only a regular
/// file, at most [`MAX_FILE`], and inside `checkout`. `Ok(None)` when it isn't there. With
/// `follow_links` (your own `.ptyro.local.toml`, which a dotfiles manager may link) a link is
/// followed, but what it leads to must still be a regular file.
pub fn read_repo_file(
    checkout: &Path,
    path: &Path,
    follow_links: bool,
) -> Result<Option<String>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let shown = path.strip_prefix(checkout).unwrap_or(path).display().to_string();
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{shown}: {e}")),
    };
    if meta.file_type().is_symlink() && !follow_links {
        return Err(format!(
            "{shown} is a symbolic link; ptyro doesn't follow links in a repository's files (copy the file in instead)"
        ));
    }
    if !follow_links {
        // A directory on the way may be the link (`.github -> ~/somewhere`).
        let (a, b) = (std::fs::canonicalize(checkout), std::fs::canonicalize(path));
        if let (Ok(a), Ok(b)) = (a, b)
            && !b.starts_with(&a)
        {
            return Err(format!("{shown} leads outside the repository"));
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // NONBLOCK so a FIFO can't hold us; it is refused below anyway.
    opts.custom_flags(opts_flags(follow_links) | rustix::fs::OFlags::NONBLOCK.bits() as i32);
    let file = opts.open(path).map_err(|e| format!("{shown}: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("{shown}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{shown} is larger than {} KiB", MAX_FILE >> 10));
    }
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{shown}: {e} (it must be UTF-8 text)"))?;
    if text.len() as u64 > MAX_FILE {
        return Err(format!("{shown} is larger than {} KiB", MAX_FILE >> 10));
    }
    Ok(Some(text))
}

fn opts_flags(follow_links: bool) -> i32 {
    if follow_links { 0 } else { rustix::fs::OFlags::NOFOLLOW.bits() as i32 }
}

fn read(
    checkout: &Path,
    rel: &str,
    parse: impl FnOnce(&str) -> Result<Imported, String>,
) -> Option<Imported> {
    let not_imported = |why: String| Imported {
        tool: tool_of(rel),
        file: rel.to_owned(),
        notes: vec![format!("{rel} not imported: {why}")],
        ..Imported::default()
    };
    let text = match read_repo_file(checkout, &checkout.join(rel), false) {
        Ok(Some(t)) => t,
        Ok(None) => return None,
        Err(e) => return Some(not_imported(e)),
    };
    Some(match parse(&text) {
        Ok(mut i) => {
            i.file = rel.to_owned();
            i
        }
        // A parser's message can quote the offending line, which may be someone's secret
        // (a file that isn't what its name says): the first line says where, that's enough.
        Err(e) => not_imported(e.lines().next().unwrap_or("it can't be read").to_owned()),
    })
}

fn tool_of(rel: &str) -> &'static str {
    FILES.iter().find(|(_, f)| rel.starts_with(f)).map_or("other", |(t, _)| t)
}

/// `.codex/environments/environment.toml`, else the first `*.toml` there.
fn codex_file(checkout: &Path) -> Option<String> {
    let dir = checkout.join(".codex/environments");
    if dir.join("environment.toml").is_file() {
        return Some(".codex/environments/environment.toml".into());
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".toml"))
        .collect();
    names.sort();
    names.first().map(|n| format!(".codex/environments/{n}"))
}

/// A word in double quotes in which only the port variable expands (`$PTYRO_PORT`,
/// `$((PTYRO_PORT + 1))`): everything else in it, `$(…)`, backticks, quotes and backslashes,
/// stays text, as it was in the file.
fn dq_port(a: &str) -> String {
    let mut out = String::from("\"");
    let mut rest = a;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("$PTYRO_PORT") {
            out.push_str("$PTYRO_PORT");
            rest = &rest["$PTYRO_PORT".len()..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("$((PTYRO_PORT + ")
            && let Some(end) = r.find("))")
            && r[..end].bytes().all(|b| b.is_ascii_digit())
        {
            let n = end + "$((PTYRO_PORT + ".len() + 2;
            out.push_str(&rest[..n]);
            rest = &rest[n..];
            continue;
        }
        if matches!(c, '\\' | '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.push('"');
    out
}

/// Quote one word for `sh`.
pub fn sh_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@,%".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn in_dir(cwd: Option<&str>, cmd: String) -> String {
    match cwd.map(str::trim).filter(|c| !c.is_empty() && *c != "." && *c != "${workspaceFolder}") {
        Some(c) => {
            let c = c.trim_start_matches("${workspaceFolder}/");
            format!("cd {} && {cmd}", sh_word(c))
        }
        None => cmd,
    }
}

fn env_prefix(env: &BTreeMap<String, String>) -> String {
    env.iter().map(|(k, v)| format!("export {k}={}\n", sh_word(v))).collect()
}

fn slug(name: &str) -> String {
    let s = crate::model::slugify(name);
    if s.is_empty() { "run".into() } else { s }
}

fn add_run(i: &mut Imported, name: &str, command: String, default: bool) {
    let mut n = slug(name);
    let mut k = 2;
    while i.scripts.run.contains_key(&n) {
        n = format!("{}-{k}", slug(name));
        k += 1;
    }
    i.scripts.run.insert(n, RunScript { command, default });
}

fn str_table(v: Option<&toml::Value>) -> BTreeMap<String, String> {
    v.and_then(toml::Value::as_table)
        .map(|t| {
            t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned()))).collect()
        })
        .unwrap_or_default()
}

/// `.conductor/settings.toml`: `scripts.setup|archive` strings, `scripts.run` a string or a
/// table of `{command, args, options.cwd, default, available_in}`, `run_mode`,
/// `[environment_variables]` (+ `.local`).
pub fn conductor(text: &str) -> Result<Imported, String> {
    let t: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut i = Imported { tool: "conductor", ..Imported::default() };
    let mut env = str_table(t.get("environment_variables"));
    env.extend(str_table(t.get("environment_variables").and_then(|e| e.get("local"))));
    let pre = env_prefix(&env);
    let scripts = t.get("scripts").and_then(toml::Value::as_table);
    let s = |k: &str| scripts.and_then(|s| s.get(k)).and_then(toml::Value::as_str);
    i.scripts.setup = s("setup").map(|c| format!("{pre}{c}"));
    i.scripts.archive = s("archive").map(|c| format!("{pre}{c}"));
    if s("run_mode") == Some("nonconcurrent") {
        i.scripts.run_mode = RunMode::Exclusive;
    }
    match scripts.and_then(|s| s.get("run")) {
        Some(toml::Value::String(c)) => add_run(&mut i, "run", format!("{pre}{c}"), true),
        Some(toml::Value::Table(runs)) => {
            for (id, r) in runs {
                let Some(cmd) = r.get("command").and_then(toml::Value::as_str) else { continue };
                let local = match r.get("available_in") {
                    Some(toml::Value::String(a)) => a == "local",
                    Some(toml::Value::Array(a)) => a.iter().any(|x| x.as_str() == Some("local")),
                    _ => true,
                };
                if !local {
                    i.notes.push(format!("run.{id} is cloud-only: skipped"));
                    continue;
                }
                let mut cmd = cmd.to_owned();
                for a in r.get("args").and_then(toml::Value::as_array).into_iter().flatten() {
                    if let Some(a) = a.as_str() {
                        cmd.push(' ');
                        cmd.push_str(&sh_word(a));
                    }
                }
                let cwd = r.get("options").and_then(|o| o.get("cwd")).and_then(toml::Value::as_str);
                let default = r.get("default").and_then(toml::Value::as_bool).unwrap_or(false);
                add_run(&mut i, id, format!("{pre}{}", in_dir(cwd, cmd)), default);
            }
        }
        _ => {}
    }
    Ok(i)
}

/// Legacy `conductor.json`: `{"scripts":{"setup","run","archive"},"runScriptMode"}`.
pub fn conductor_json(text: &str) -> Result<Imported, String> {
    let v: Value = serde_json::from_str(&strip_json_comments(text)).map_err(|e| e.to_string())?;
    let mut i = Imported { tool: "conductor", ..Imported::default() };
    let s = |k: &str| v["scripts"][k].as_str().filter(|c| !c.trim().is_empty()).map(str::to_owned);
    i.scripts.setup = s("setup");
    i.scripts.archive = s("archive");
    if let Some(r) = s("run") {
        add_run(&mut i, "run", r, true);
    }
    if v["runScriptMode"].as_str() == Some("nonconcurrent") {
        i.scripts.run_mode = RunMode::Exclusive;
    }
    Ok(i)
}

/// One command per line, stopping at the first failure (Superset runs them in order).
fn script_of(lines: &[String], cwd: Option<&str>) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let body = format!("set -e\n{}", lines.join("\n"));
    Some(match cwd {
        Some(c) if !c.is_empty() && c != "." => format!("cd {}\n{body}", sh_word(c)),
        _ => body,
    })
}

/// `.superset/config.json`: `setup`, `teardown`, `run` arrays of shell commands (strict: a
/// string is invalid), `cwd`; `.superset/config.local.json` replaces a key with an array or wraps
/// it with `{before, after}`; a missing key falls back to `.superset/<key>.sh`.
pub fn superset(checkout: &Path, text: &str) -> Result<Imported, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let local: Value =
        read_repo_file(checkout, &checkout.join(".superset/config.local.json"), false)
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
    let mut i = Imported { tool: "superset", ..Imported::default() };
    let list = |v: &Value| -> Option<Vec<String>> {
        v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
    };
    let key = |k: &str| -> Result<Vec<String>, String> {
        let mut cmds = match &v[k] {
            Value::Null => Vec::new(),
            x => list(x).ok_or_else(|| format!("`{k}` must be an array of commands"))?,
        };
        match &local[k] {
            Value::Array(_) => cmds = list(&local[k]).unwrap_or_default(),
            Value::Object(o) => {
                let before = o.get("before").and_then(list).unwrap_or_default();
                let after = o.get("after").and_then(list).unwrap_or_default();
                cmds = before.into_iter().chain(cmds).chain(after).collect();
            }
            _ => {}
        }
        if cmds.is_empty() && checkout.join(format!(".superset/{k}.sh")).is_file() {
            cmds.push(format!("./.superset/{k}.sh"));
        }
        Ok(cmds)
    };
    let cwd = v["cwd"].as_str().map(str::to_owned);
    let setup = key("setup")?;
    let teardown = key("teardown")?;
    let run = key("run")?;
    i.scripts.setup = script_of(&setup, cwd.as_deref());
    i.scripts.archive = script_of(&teardown, cwd.as_deref());
    if let Some(r) = script_of(&run, cwd.as_deref()) {
        add_run(&mut i, "run", r, true);
    }
    Ok(i)
}

/// `.codex/environments/*.toml`: `[setup] script` (with `[setup.<platform>]` overrides),
/// `[[actions]] {name, command}`, `[cleanup] script`.
pub fn codex(text: &str) -> Result<Imported, String> {
    let t: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut i = Imported { tool: "codex", ..Imported::default() };
    let platform = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let script = |section: &str| -> Option<String> {
        let s = t.get(section)?;
        s.get(platform)
            .and_then(|p| p.get("script"))
            .or_else(|| s.get("script"))
            .and_then(toml::Value::as_str)
            .filter(|x| !x.trim().is_empty())
            .map(str::to_owned)
    };
    i.scripts.setup = script("setup");
    i.scripts.archive = script("cleanup");
    let actions = t.get("actions").and_then(toml::Value::as_array).cloned().unwrap_or_default();
    let has_run =
        actions.iter().any(|a| a.get("name").and_then(toml::Value::as_str) == Some("Run"));
    for (n, a) in actions.iter().enumerate() {
        let Some(cmd) = a.get("command").and_then(toml::Value::as_str) else { continue };
        if let Some(p) = a.get("platform").and_then(toml::Value::as_str)
            && p != platform
            && !(p == "macos" && platform == "darwin")
        {
            continue;
        }
        let name = a.get("name").and_then(toml::Value::as_str).unwrap_or("action");
        let default = if has_run { name == "Run" } else { n == 0 };
        add_run(&mut i, name, cmd.to_owned(), default);
    }
    Ok(i)
}

/// Replace `--port N`, `--port=N` and `-p N` in an argv with `port`.
fn retarget_port(args: &[String], port: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if (a == "--port" || a == "-p") && it.peek().is_some() {
            out.push(a.clone());
            out.push(port.to_owned());
            it.next();
        } else if a.starts_with("--port=") {
            out.push(format!("--port={port}"));
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// `.claude/launch.json` (Claude Code desktop): `configurations[]` of `{name,
/// runtimeExecutable, runtimeArgs, program, args, cwd, env, port, autoPort}`. With `autoPort`
/// (the default) the server gets a port from the worktree's block in `PORT`, and `--port`/`-p`
/// values are rewritten to it, as Claude does; entries with only a `url` attach to a server
/// that's already running, so there's nothing to run.
pub fn claude_launch(text: &str) -> Result<Imported, String> {
    let v: Value = serde_json::from_str(&strip_json_comments(text)).map_err(|e| e.to_string())?;
    let mut i = Imported { tool: "claude", ..Imported::default() };
    let strs = |x: &Value| -> Vec<String> {
        x.as_array()
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let mut n = 0;
    for c in v["configurations"].as_array().into_iter().flatten() {
        let name = c["name"].as_str().unwrap_or("server");
        let mut argv: Vec<String> = if let Some(exe) = c["runtimeExecutable"].as_str() {
            std::iter::once(exe.to_owned()).chain(strs(&c["runtimeArgs"])).collect()
        } else if let Some(p) = c["program"].as_str() {
            ["node".to_owned(), p.to_owned()].into_iter().chain(strs(&c["args"])).collect()
        } else {
            i.notes
                .push(format!("{name}: attaches to a running server (url only), nothing to run"));
            continue;
        };
        let mut env: BTreeMap<String, String> = c["env"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let mut prefix = String::new();
        if c["autoPort"].as_bool() != Some(false) {
            // One port per server from the worktree's block: $PTYRO_PORT, +1, +2…
            let port =
                if n == 0 { "$PTYRO_PORT".to_owned() } else { format!("$((PTYRO_PORT + {n}))") };
            argv =
                std::iter::once(argv[0].clone()).chain(retarget_port(&argv[1..], &port)).collect();
            prefix = format!("PORT=\"{port}\" ");
            env.remove("PORT");
            n += 1;
        } else if let Some(p) = c["port"].as_u64() {
            i.notes.push(format!(
                "{name}: fixed port {p} (autoPort = false): worktrees will collide on it"
            ));
        }
        let words: Vec<String> = argv
            .iter()
            .map(|a| if a.contains("PTYRO_PORT") { dq_port(a) } else { sh_word(a) })
            .collect();
        let cmd = format!("{}{prefix}{}", env_prefix(&env), words.join(" "));
        let first = i.scripts.run.is_empty();
        add_run(&mut i, name, in_dir(c["cwd"].as_str(), cmd), first);
    }
    Ok(i)
}

/// `.cursor/environment.json` (cloud agents): `install` → setup, `start` (services started on
/// every boot) and each of `terminals[] {name, command}` → run scripts.
pub fn cursor(text: &str) -> Result<Imported, String> {
    let v: Value = serde_json::from_str(&strip_json_comments(text)).map_err(|e| e.to_string())?;
    let mut i = Imported { tool: "cursor", ..Imported::default() };
    i.scripts.setup = v["install"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned);
    if let Some(s) = v["start"].as_str().filter(|s| !s.trim().is_empty()) {
        add_run(&mut i, "start", s.to_owned(), false);
    }
    fn terminals(v: &Value, out: &mut Vec<(String, String)>) {
        for t in v.as_array().into_iter().flatten() {
            if t.is_array() {
                terminals(t, out); // the schema allows nesting
            } else if let Some(c) = t["command"].as_str() {
                out.push((t["name"].as_str().unwrap_or("terminal").to_owned(), c.to_owned()));
            }
        }
    }
    let mut ts = Vec::new();
    terminals(&v["terminals"], &mut ts);
    // The first terminal is the dev server, typically; `start` only brings up services.
    for (k, (name, cmd)) in ts.into_iter().enumerate() {
        add_run(&mut i, &name, cmd, k == 0);
    }
    if !v["build"].is_null() || !v["snapshot"].is_null() || !v["image"].is_null() {
        i.notes
            .push("its container image or snapshot isn't used: scripts run on this machine".into());
    }
    Ok(i)
}

/// Whether a YAML text is safe to hand to the parser: small, no anchors (`&a`) for aliases (`*a`)
/// to expand (a few lines of nested aliases become billions of nodes), and not nested so deeply
/// that the parser's recursion could overflow the stack.
fn yaml_is_plain(text: &str) -> Result<(), String> {
    if text.len() > MAX_YAML {
        return Err(format!("larger than {} KiB", MAX_YAML >> 10));
    }
    let (mut depth, mut deepest) = (0usize, 0usize);
    let mut prev = ' ';
    for c in text.chars() {
        match c {
            '[' | '{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ']' | '}' => depth = depth.saturating_sub(1),
            // `&name` after a space, a colon or a bracket is an anchor.
            c if prev == '&' && (c.is_alphanumeric() || c == '_' || c == '-') => {
                return Err("it uses YAML anchors and aliases, which aren't imported".into());
            }
            _ => {}
        }
        // (`&` alone, as in `a && b`, is a shell operator, not an anchor: it needs a name after.)
        prev = if c == '&' && matches!(prev, ' ' | '\t' | '\n' | ':' | '[' | '{' | ',' | '-') {
            '&'
        } else {
            c
        };
    }
    if deepest > 64 {
        return Err("nested too deeply".into());
    }
    Ok(())
}

/// `copilot-setup-steps.yml`: the `copilot-setup-steps` job's `run:` steps, in order, with
/// their `working-directory`. `uses:` steps (checkout, setup-node…) and steps with `${{ }}`
/// expressions can't run outside GitHub Actions, so they're listed in the notes instead.
pub fn copilot(text: &str) -> Result<Imported, String> {
    use yaml_rust2::{Yaml, YamlLoader};
    yaml_is_plain(text)?;
    let docs = YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    let doc = docs.first().ok_or("empty file")?;
    let job = &doc["jobs"]["copilot-setup-steps"];
    if job.is_badvalue() {
        return Err("no `copilot-setup-steps` job (Copilot ignores the file then too)".into());
    }
    let mut i = Imported { tool: "copilot", ..Imported::default() };
    let mut lines = vec!["set -e".to_owned()];
    let Yaml::Array(steps) = &job["steps"] else { return Ok(i) };
    for step in steps {
        let label = step["name"].as_str().map(str::to_owned);
        if let Some(uses) = step["uses"].as_str() {
            if !uses.starts_with("actions/checkout") {
                i.notes.push(format!("skipped `uses: {uses}` (a GitHub Action)"));
            }
            continue;
        }
        let Some(run) = step["run"].as_str() else { continue };
        if run.contains("${{") || step["if"].as_str().is_some() {
            i.notes.push(format!(
                "skipped step {:?}: it needs GitHub Actions expressions",
                label.unwrap_or_else(|| run.lines().next().unwrap_or("").to_owned())
            ));
            continue;
        }
        if let Some(l) = &label {
            lines.push(format!("# {l}"));
        }
        let body = run.trim_end().to_owned();
        lines.push(match step["working-directory"].as_str() {
            Some(d) => format!("(cd {} && {body})", sh_word(d)),
            None => body,
        });
    }
    if lines.len() > 1 {
        i.scripts.setup = Some(lines.join("\n"));
    }
    Ok(i)
}

/// JSON with `//` and `/* */` comments (Claude's and Cursor's files allow them).
pub fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod quoting {
    use super::*;

    #[test]
    fn only_the_port_variable_expands_in_an_imported_argument() {
        assert_eq!(dq_port("--port=$PTYRO_PORT"), "\"--port=$PTYRO_PORT\"");
        assert_eq!(dq_port("--p=$((PTYRO_PORT + 2))"), "\"--p=$((PTYRO_PORT + 2))\"");
        assert_eq!(dq_port("x$(curl evil)$PTYRO_PORT"), "\"x\\$(curl evil)$PTYRO_PORT\"");
        assert_eq!(dq_port("a\"b`c\\d$PTYRO_PORT"), "\"a\\\"b\\`c\\\\d$PTYRO_PORT\"");
        // The shell reads back exactly the text.
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("PTYRO_PORT=7; printf %s {}", dq_port("q\"`id`$(id)$PTYRO_PORT\\")),
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "q\"`id`$(id)7\\");
    }
}

#[cfg(test)]
mod hostile_files {
    use super::*;
    use crate::project::ProjectConfig;

    #[test]
    fn a_yaml_alias_bomb_is_refused_before_it_is_parsed() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let mut y = String::from("a: &a [x]\n");
        for (n, prev) in ('b'..='j').zip('a'..) {
            y.push_str(&format!("{n}: &{n} [{}]\n", format!("*{prev},").repeat(9)));
        }
        std::fs::write(dir.join("copilot-setup-steps.yml"), y).unwrap();
        let t = std::time::Instant::now();
        let found = discover(d.path());
        assert!(t.elapsed().as_secs() < 2, "refused, not expanded");
        assert!(found.iter().any(|i| i.notes.iter().any(|n| n.contains("anchors"))), "{found:?}");
        // Ordinary workflows (shell `&&`, globs) still import.
        let ok = "jobs:\n  copilot-setup-steps:\n    steps:\n      - run: make a && make b\n      - run: ls *.txt\n";
        std::fs::write(dir.join("copilot-setup-steps.yml"), ok).unwrap();
        let found = discover(d.path());
        assert!(
            found.iter().any(|i| i
                .scripts
                .setup
                .as_deref()
                .is_some_and(|s| s.contains("make a && make b"))),
            "{found:?}"
        );
        // And deep nesting is refused.
        std::fs::write(dir.join("copilot-setup-steps.yml"), format!("x: {}\n", "[".repeat(5000)))
            .unwrap();
        assert!(discover(d.path()).iter().any(|i| i.notes.iter().any(|n| n.contains("deeply"))));
    }

    #[test]
    fn repository_files_are_never_read_through_links_or_without_bounds() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("repo");
        std::fs::create_dir_all(root.join(".cursor")).unwrap();
        let secret = d.path().join("secret.json");
        std::fs::write(&secret, "{\"AWS_SECRET\": \"hunter2\" nope").unwrap();
        // A link to a file elsewhere: refused, and nothing it holds is quoted.
        std::os::unix::fs::symlink(&secret, root.join(".cursor/environment.json")).unwrap();
        let found = discover(&root);
        let notes = found.iter().flat_map(|i| i.notes.clone()).collect::<Vec<_>>().join("\n");
        assert!(notes.contains("symbolic link") && !notes.contains("hunter2"), "{notes}");
        // A real file that doesn't parse: only where, never the line.
        std::fs::remove_file(root.join(".cursor/environment.json")).unwrap();
        std::fs::write(root.join(".cursor/environment.json"), "{\"AWS_SECRET\": \"hunter2\" nope")
            .unwrap();
        let notes =
            discover(&root).iter().flat_map(|i| i.notes.clone()).collect::<Vec<_>>().join("\n");
        assert!(notes.contains("not imported") && !notes.contains("hunter2"), "{notes}");
        // The committed config: a link is refused; the local one may be a link to a file.
        std::fs::write(root.join(".git"), "").unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.join(".ptyro.toml")).unwrap();
        let e = ProjectConfig::load(&root, &root).unwrap_err();
        assert!(e.contains("symbolic link"), "{e}");
        std::fs::remove_file(root.join(".ptyro.toml")).unwrap();
        std::fs::write(root.join(".ptyro.toml"), vec![b'#'; (MAX_FILE + 10) as usize]).unwrap();
        assert!(ProjectConfig::load(&root, &root).unwrap_err().contains("larger than"));
        std::fs::remove_file(root.join(".ptyro.toml")).unwrap();
        std::fs::write(d.path().join("mine.toml"), "[project]\nbase = \"origin/dev\"\n").unwrap();
        std::os::unix::fs::symlink(d.path().join("mine.toml"), root.join(".ptyro.local.toml"))
            .unwrap();
        assert_eq!(
            ProjectConfig::load(&root, &root).unwrap().project.base.as_deref(),
            Some("origin/dev")
        );
        std::fs::remove_file(root.join(".ptyro.local.toml")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.join(".ptyro.local.toml")).unwrap();
        assert!(ProjectConfig::load(&root, &root).unwrap_err().contains("not a regular file"));
        // A directory on the way that is a link out of the repository.
        std::fs::remove_file(root.join(".ptyro.local.toml")).unwrap();
        let out = d.path().join("elsewhere");
        std::fs::create_dir_all(out.join("workflows")).unwrap();
        std::fs::write(out.join("workflows/copilot-setup-steps.yml"), "jobs: {}\n").unwrap();
        std::os::unix::fs::symlink(&out, root.join(".github")).unwrap();
        let notes =
            discover(&root).iter().flat_map(|i| i.notes.clone()).collect::<Vec<_>>().join("\n");
        assert!(notes.contains("outside the repository"), "{notes}");
    }
}
