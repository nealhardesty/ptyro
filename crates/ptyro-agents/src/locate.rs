//! Finding an agent's program when it isn't on `PATH` (out of the box first, SPEC §1.3): a daemon
//! started by a service manager has a short `PATH`, and npm-installed agents live where nvm, an
//! npm prefix or `npx`'s cache put them. Gemini CLI run as `npx …` (a shell alias) is only ever in
//! that cache. ptyro never downloads an agent: it runs one that is already on disk, and when none
//! is, it offers setup to install it ([`install_fix`], BOOT9; the vendor's own command is
//! [`install_hint`]).
//!
//! On Windows (WIN8) a program has an extension from `PATHEXT` (`claude.exe`, `codex.cmd`), the
//! home is `%USERPROFILE%`, and the places are Windows' own: Claude's native installer
//! (`~\.local\bin`), npm's prefix (`%APPDATA%\npm`), WinGet's links, Scoop's shims. An npm
//! `.cmd` shim is read and what it starts runs instead (a Node script with `node`, or the `.exe`
//! a package links): through the shim, `cmd.exe` would stand between ptyro and the agent, ask
//! "Terminate batch job (Y/N)?" at Ctrl+C, and be the session's program.

use std::path::{Component, Path, PathBuf};

/// Where a program was found, and how to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// What runs in place of the bare program name: `[path]`, or `[node, script]` when the
    /// program is a Node script and no `node` is on `PATH` (nvm's bin folder, `npx`'s cache).
    pub argv: Vec<String>,
    /// The program itself.
    pub path: PathBuf,
    /// Where it came from when that isn't `PATH`: `npx cache`, `nvm`, `~/.local/bin`…
    pub via: Option<String>,
}

impl Found {
    /// For people: `/usr/local/bin/codex`, or `~/.npm/_npx/…/gemini.js (npx cache)`.
    pub fn describe(&self) -> String {
        let p = tilde(&self.path);
        match &self.via {
            Some(v) => format!("{p} ({v})"),
            None => p,
        }
    }
}

/// The npm package and its command for the built-in agents that npm ships.
const NPM: &[(&str, &str)] = &[
    ("gemini", "@google/gemini-cli"),
    ("codex", "@openai/codex"),
    ("opencode", "opencode-ai"),
    ("claude", "@anthropic-ai/claude-code"),
];

/// The built-in command an npm package's folder stands for (`claude-code` → `claude`): WIN7
/// names a Node script by its package's folder when the script is a generic `cli.js`.
pub fn program_of_package(folder: &str) -> Option<&'static str> {
    NPM.iter().find(|(_, pkg)| pkg.rsplit('/').next() == Some(folder)).map(|(n, _)| *n)
}

/// How to install a built-in agent's program, for the fix ptyro offers when it is missing: the
/// vendor's recommended command on this platform, from the version table ([`crate::catalog`]).
pub fn install_hint(program: &str) -> Option<String> {
    hint_on(program, crate::catalog::Platform::current())
}

fn hint_on(program: &str, platform: crate::catalog::Platform) -> Option<String> {
    crate::catalog::by_program(program)?.method(platform, |_| true)?.install.clone()
}

/// Which agent to install, where a message finds none (BOOT9): Antigravity for a Google account,
/// Gemini CLI kept for the accounts it still serves (SPEC §24.10).
pub const WHICH_AGENT: &str = "Claude Code, Codex, or Antigravity for a Google account; Gemini CLI is for Gemini API keys and enterprise accounts";

/// No coding agent installed (BOOT9), in every client's words: `ptyro setup` is the fix, which
/// the TUI offers as its checklist (`C-q U`) and the web as its setup page.
pub fn no_agent() -> String {
    format!("No coding agent is installed: `ptyro setup` installs one ({WHICH_AGENT})")
}

/// No agent that can chat (BOOT9). Antigravity has no structured mode (SPEC §22.5), so it is
/// named as what a Google account runs in a session, not as a fix.
pub fn no_chat_agent() -> String {
    "No agent that can chat is installed: `ptyro setup` installs one (Claude Code, Codex or OpenCode; Gemini CLI with a Gemini API key; Antigravity, for a Google account, runs in sessions but can't chat)".into()
}

/// The fix for a missing agent or tool setup installs here: `` `ptyro install codex` installs
/// it ``. `None` for a program the version table doesn't know (an agent of your own) or has no
/// way to install on this platform.
pub fn install_fix(program: &str) -> Option<String> {
    let item = crate::catalog::by_program(program)?;
    item.method(crate::catalog::Platform::current(), |_| true)?;
    Some(format!("`ptyro install {}` installs it", item.program))
}

/// An agent whose program isn't anywhere ptyro looks (BOOT9), with the fix: `ptyro install` for
/// one setup installs, else its `[agents.NAME] command`. Sessions and chats both say it.
pub fn not_installed(agent: &str, program: &str) -> String {
    let fix = install_fix(program)
        .unwrap_or_else(|| format!("install it, or set [agents.{agent}] command to its full path"));
    format!("{agent} isn't installed ({program} is nowhere ptyro looks): {fix}")
}

/// What a message's setup offer installs (BOOT9), for a client that does it instead of leaving it
/// to be typed: `` `ptyro setup` installs `` is the checklist's defaults (`Some([])`), and
/// `` `ptyro install codex agy` installs `` those names. Only the offers above: a lock held by
/// `` `ptyro install` `` or a move (`` `ptyro install codex` moves it ``) is something else.
pub fn setup_offer(text: &str) -> Option<Vec<String>> {
    text.match_indices("`ptyro ").find_map(|(at, open)| {
        let rest = &text[at + open.len()..];
        let end = rest.find('`')?;
        if !rest[end + 1..].starts_with(" installs") {
            return None;
        }
        let mut words = rest[..end].split_whitespace();
        match words.next()? {
            "setup" => Some(Vec::new()),
            "install" => {
                Some(words.map(str::to_owned).collect()).filter(|n: &Vec<_>| !n.is_empty())
            }
            _ => None,
        }
    })
}

/// The user's home: `$HOME`, else `%USERPROFILE%` (Windows sets only that).
fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|v| std::env::var_os(v).filter(|h| !h.is_empty()))
        .map(PathBuf::from)
}

/// Where to look besides `PATH`, and how.
struct Places {
    /// Folders to look in after `PATH`, each with how to say it.
    dirs: Vec<(PathBuf, String)>,
    /// nvm's folder (`versions/node/*/bin` under it).
    nvm: PathBuf,
    /// `npx`'s cache (`~/.npm/_npx`; `%LOCALAPPDATA%\npm-cache\_npx` on Windows).
    npx: PathBuf,
    /// Windows: `PATHEXT`, the extensions a program may have. `None`: a program is its name.
    exts: Option<String>,
}

/// `prog` on `path`, else where npm, nvm and `npx` put it under the home.
pub fn find(prog: &str, path: &str) -> Option<Found> {
    find_in(prog, path, &places()?)
}

/// Every copy of `prog`, in the order [`find`] would look: each `PATH` folder's, then npm's,
/// nvm's and the vendors' folders', then `npx`'s cache; one per file (BOOT7: after a move to the
/// recommended installer, the new copy, and whether the old one still comes first).
pub fn find_all(prog: &str, path: &str) -> Vec<Found> {
    let Some(places) = places() else { return Vec::new() };
    let none = PathBuf::from(NO_PATH);
    let only = |dirs: Vec<(PathBuf, String)>| Places {
        dirs,
        nvm: none.clone(),
        npx: none.clone(),
        exts: places.exts.clone(),
    };
    let mut out: Vec<Found> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut add = |f: Option<Found>| {
        let Some(f) = f else { return };
        let real = ptyro_core::os::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
        if !seen.contains(&real) {
            seen.push(real);
            out.push(f);
        }
    };
    for d in std::env::split_paths(path) {
        let one = std::env::join_paths([&d]).map(|p| p.to_string_lossy().into_owned());
        add(one.ok().and_then(|p| find_in(prog, &p, &only(Vec::new()))));
    }
    let mut dirs = places.dirs.clone();
    dirs.extend(nvm_bins(&places.nvm).into_iter().map(|d| (d, "nvm".to_owned())));
    for d in dirs {
        add(find_in(prog, NO_PATH, &only(vec![d])));
    }
    add(find_in(prog, NO_PATH, &Places { dirs: Vec::new(), ..places }));
    out
}

/// A `PATH` that finds nothing (an empty one would be the current folder).
const NO_PATH: &str = "/nonexistent/ptyro-none";

/// Where to look besides `PATH`, on this platform.
fn places() -> Option<Places> {
    let home = home()?;
    let var = |v: &str| std::env::var_os(v).filter(|x| !x.is_empty()).map(PathBuf::from);
    let mut dirs: Vec<(PathBuf, String)> = Vec::new();
    let places = if cfg!(windows) {
        // npm puts a global package's commands in its prefix itself on Windows, not in `bin`.
        if let Some(p) = var("NPM_CONFIG_PREFIX") {
            dirs.push((p, "npm prefix".into()));
        }
        if let Some(a) = var("APPDATA") {
            dirs.push((a.join("npm"), "npm".into()));
        }
        // Claude Code's native installer (`irm https://claude.ai/install.ps1 | iex`).
        dirs.push((home.join(".local").join("bin"), "~\\.local\\bin".into()));
        let local = var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"));
        dirs.push((local.join("Microsoft").join("WinGet").join("Links"), "WinGet".into()));
        // The Antigravity CLI's installer (`install.ps1`) puts `agy.exe` here.
        dirs.push((local.join("agy").join("bin"), "%LOCALAPPDATA%\\agy\\bin".into()));
        let scoop = var("SCOOP").unwrap_or_else(|| home.join("scoop"));
        dirs.push((scoop.join("shims"), "Scoop".into()));
        // nvm-windows: the active Node's folder, where its global packages' commands are.
        if let Some(n) = var("NVM_SYMLINK") {
            dirs.push((n, "nvm".into()));
        }
        dirs.push((home.join(".bun").join("bin"), "~\\.bun\\bin".into()));
        Places {
            dirs,
            nvm: home.join(".nvm"),
            npx: local.join("npm-cache").join("_npx"),
            exts: Some(std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())),
        }
    } else {
        if let Some(p) = var("NPM_CONFIG_PREFIX") {
            dirs.push((p.join("bin"), "npm prefix".into()));
        }
        for d in [".npm-global/bin", ".local/bin", ".volta/bin", ".bun/bin", ".claude/local"] {
            dirs.push((home.join(d), format!("~/{d}")));
        }
        for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
            dirs.push((PathBuf::from(d), d.to_owned()));
        }
        Places {
            dirs,
            nvm: var("NVM_DIR").unwrap_or_else(|| home.join(".nvm")),
            npx: home.join(".npm/_npx"),
            exts: None,
        }
    };
    let mut places = places;
    for d in vendor_bins(crate::catalog::Platform::current()) {
        if !places.dirs.iter().any(|(p, _)| *p == d.0) {
            places.dirs.push(d);
        }
    }
    Some(places)
}

/// The folders the version table's vendor installers put their programs in on `platform` (each
/// method's `bin`), expanded here, each with how to say it: what they installed is found before
/// the daemon or a shell has the `PATH` they added (the final review's I2: one list, so `locate`
/// and the table can't drift apart).
fn vendor_bins(platform: crate::catalog::Platform) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    for m in crate::catalog::items()
        .iter()
        .flat_map(|i| &i.methods)
        .filter(|m| m.via == crate::catalog::Via::Vendor && m.on(platform))
    {
        for b in &m.bin {
            let Some(dir) = crate::setup::expand_place(b) else { continue };
            let dir = PathBuf::from(dir);
            let say = if platform.is_windows() { b.replace('/', "\\") } else { b.clone() };
            if !out.iter().any(|(p, _)| *p == dir) {
                out.push((dir, say));
            }
        }
    }
    out
}

/// After `PATH`: `places.dirs`, then nvm, then `npx`'s cache.
fn find_in(prog: &str, path: &str, places: &Places) -> Option<Found> {
    let exts = places.exts.as_deref();
    // The names `p` may have on disk: itself; on Windows, with each runnable extension (and
    // never npm's extensionless `sh` shim beside its `.cmd`).
    let names = |p: &str| -> Vec<String> {
        match exts {
            None => vec![p.to_owned()],
            Some(e) => ptyro_core::os::windows_program_names(p, e)
                .into_iter()
                .filter(|n| ptyro_core::os::windows_runnable(Path::new(n), e))
                .collect(),
        }
    };
    // An npm shim whose package is gone (removed, or an install that rolled back) runs nothing:
    // it isn't found, so the agent reads as not installed, with how to install it.
    let in_dir = |d: &Path, p: &str| {
        names(p).into_iter().map(|n| d.join(n)).find(|f| f.is_file() && !broken_shim(f))
    };
    if prog.contains('/') || (exts.is_some() && prog.contains('\\')) {
        let p = PathBuf::from(prog);
        return p.is_file().then(|| Found { argv: vec![prog.to_owned()], path: p, via: None });
    }
    let on_path = |p: &str| std::env::split_paths(path).find_map(|d| in_dir(&d, p));
    let node_bins = nvm_bins(&places.nvm);
    // A Node script runs with a `node` from PATH, else the one beside it, else nvm's newest.
    let node = |beside: &Path| -> Option<PathBuf> {
        on_path("node")
            .or_else(|| in_dir(beside, "node"))
            .or_else(|| node_bins.iter().find_map(|d| in_dir(d, "node")))
    };
    // An npm `.cmd` shim runs what it starts, without cmd.exe in between.
    let unshim = |f: &Path| -> Option<(Vec<String>, PathBuf)> {
        let target = shim_target(f)?;
        let is_js = target.extension().is_some_and(|e| e.eq_ignore_ascii_case("js"));
        if is_js || node_script(&target) {
            let n = node(f.parent()?)?;
            let argv =
                vec![n.to_string_lossy().into_owned(), target.to_string_lossy().into_owned()];
            return Some((argv, target));
        }
        Some((vec![target.to_string_lossy().into_owned()], target))
    };
    if let Some(p) = on_path(prog) {
        if let Some((argv, real)) = unshim(&p) {
            return Some(Found { argv, path: real, via: None });
        }
        return Some(Found { argv: vec![prog.to_owned()], path: p, via: None });
    }
    let mut dirs = places.dirs.clone();
    dirs.extend(node_bins.iter().map(|d| (d.clone(), "nvm".to_owned())));
    for (dir, via) in &dirs {
        let Some(f) = in_dir(dir, prog) else { continue };
        if let Some((argv, real)) = unshim(&f) {
            return Some(Found { argv, path: real, via: Some(via.clone()) });
        }
        let real = ptyro_core::os::canonicalize(&f).unwrap_or_else(|_| f.clone());
        let argv = if node_script(&real) && on_path("node").is_none() {
            let Some(n) = node(dir) else { continue };
            vec![n.to_string_lossy().into_owned(), real.to_string_lossy().into_owned()]
        } else {
            vec![f.to_string_lossy().into_owned()]
        };
        return Some(Found { argv, path: f, via: Some(via.clone()) });
    }
    // `npx <package>` leaves the package in npm's cache: run the newest copy there.
    let (_, pkg) = NPM.iter().find(|(n, _)| *n == prog)?;
    let script = npx_cached(&places.npx, pkg, prog)?;
    let n = node(script.parent()?)?;
    Some(Found {
        argv: vec![n.to_string_lossy().into_owned(), script.to_string_lossy().into_owned()],
        path: script,
        via: Some("npx cache".into()),
    })
}

/// An npm `.cmd` shim whose target isn't on disk: it would fail when run.
fn broken_shim(f: &Path) -> bool {
    let is_cmd = f.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    is_cmd
        && std::fs::read_to_string(f).is_ok_and(|t| t.contains("\"%dp0%\\node_modules\\"))
        && shim_target(f).is_none()
}

/// What an npm `.cmd` shim starts, when `f` is one and that is on disk: the last quoted
/// `"%dp0%\…"` in it, a path from the shim's folder (`%dp0%\node_modules\@openai\codex\bin\
/// codex.js`). npm's shims (cmd-shim) end by running it, after picking a `node`.
fn shim_target(f: &Path) -> Option<PathBuf> {
    let is_cmd = f.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    if !is_cmd {
        return None;
    }
    let text = std::fs::read_to_string(f).ok()?;
    let at = text.rfind("\"%dp0%\\")? + "\"%dp0%\\".len();
    let rel = &text[at..at + text[at..].find('"')?];
    // Inside the shim's folder only: a plain relative path, never climbing out.
    let parts: Vec<&str> = rel.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.iter().any(|p| *p == ".." || p.contains(':') || p.contains('%')) {
        return None;
    }
    let target = parts.iter().fold(f.parent()?.to_path_buf(), |d, p| d.join(p));
    target.is_file().then_some(target)
}

/// nvm's `versions/node/*/bin`, newest version first.
fn nvm_bins(nvm: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(nvm.join("versions/node")) else { return Vec::new() };
    let mut v: Vec<(Vec<u64>, PathBuf)> = rd
        .flatten()
        .map(|e| (version(&e.file_name().to_string_lossy()), e.path().join("bin")))
        .filter(|(_, p)| p.is_dir())
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, p)| p).collect()
}

/// `v24.18.0` → `[24, 18, 0]`.
fn version(s: &str) -> Vec<u64> {
    s.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Starts `#!/usr/bin/env node` (or names a `node` in its first line).
fn node_script(p: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 64];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let line = String::from_utf8_lossy(&head[..n]);
    line.starts_with("#!") && line.lines().next().is_some_and(|l| l.contains("node"))
}

/// The newest cached copy of `pkg`'s `prog` script under `~/.npm/_npx`.
fn npx_cached(cache: &Path, pkg: &str, prog: &str) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for e in std::fs::read_dir(cache).ok()?.flatten() {
        let dir = e.path().join("node_modules").join(pkg);
        let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        // The package itself, not another one installed under its name (an npm alias).
        if v["name"].as_str() != Some(pkg) {
            continue;
        }
        let bin = match &v["bin"] {
            serde_json::Value::String(s) => Some(s.as_str()),
            b => b[prog].as_str(),
        };
        // A script inside the package: relative, never climbing out of it.
        let Some(bin) = bin.map(Path::new).filter(|b| {
            b.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        }) else {
            continue;
        };
        let script = dir.join(bin);
        let inside = match (script.canonicalize(), dir.canonicalize()) {
            (Ok(s), Ok(d)) => s.starts_with(d),
            _ => false,
        };
        let ver = version(v["version"].as_str().unwrap_or("0"));
        if inside && script.is_file() && best.as_ref().is_none_or(|(b, _)| ver > *b) {
            best = Some((ver, script));
        }
    }
    best.map(|(_, p)| p)
}

fn tilde(p: &Path) -> String {
    let s = p.to_string_lossy();
    match home().map(|h| h.to_string_lossy().into_owned()) {
        Some(h) if s.starts_with(&format!("{h}/")) || s.starts_with(&format!("{h}\\")) => {
            format!("~{}", &s[h.len()..])
        }
        _ => s.into_owned(),
    }
}

#[cfg(test)]
mod vendor_tests {
    use super::*;

    /// Every folder the version table says a vendor installer puts its program in is one
    /// `locate` searches (the final review's I2: `~/.opencode/bin` and Codex's Windows folder
    /// were missing), so `find` sees a fresh install.
    #[test]
    fn every_vendor_bin_in_the_table_is_searched() {
        let platform = crate::catalog::Platform::current();
        let dirs: Vec<PathBuf> = places().expect("a home").dirs.into_iter().map(|d| d.0).collect();
        let mut checked = 0;
        for i in crate::catalog::items() {
            for m in
                i.methods.iter().filter(|m| m.via == crate::catalog::Via::Vendor && m.on(platform))
            {
                for b in &m.bin {
                    let Some(d) = crate::setup::expand_place(b) else { continue };
                    assert!(
                        dirs.contains(&PathBuf::from(&d)),
                        "{}: {b} ({d}) isn't searched: {dirs:?}",
                        i.id
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no vendor bin on {platform:?}");
        if !platform.is_windows() {
            let home = home().unwrap();
            assert!(dirs.contains(&home.join(".opencode").join("bin")), "{dirs:?}");
        }
    }
}

#[cfg(test)]
#[cfg(unix)] // fakes agents with execute bits
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn places(home: &Path, nvm: &Path) -> Places {
        Places { dirs: Vec::new(), nvm: nvm.to_owned(), npx: home.join(".npm/_npx"), exts: None }
    }

    fn exe(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn found_on_path_then_npm_places_then_npx_cache() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let bin = home.join("bin");
        exe(&bin.join("codex"), "#!/bin/sh\n");
        let path = bin.to_string_lossy().into_owned();
        let nvm = home.join(".nvm");
        // On PATH: run by name.
        let f = find_in("codex", &path, &places(home, &nvm)).unwrap();
        assert_eq!((f.argv, f.via), (vec!["codex".to_owned()], None));
        // Nowhere: nothing (and never a download).
        assert!(find_in("gemini", &path, &places(home, &nvm)).is_none());
        // In nvm's newest node: a Node script runs with the `node` beside it (none on PATH).
        let old = nvm.join("versions/node/v9.0.0/bin");
        let new = nvm.join("versions/node/v24.18.0/bin");
        exe(&old.join("gemini"), "#!/usr/bin/env node\n");
        exe(&new.join("gemini"), "#!/usr/bin/env node\n");
        exe(&new.join("node"), "");
        let f = find_in("gemini", &path, &places(home, &nvm)).unwrap();
        assert_eq!(f.via.as_deref(), Some("nvm"));
        assert_eq!(f.argv[0], new.join("node").to_string_lossy());
        assert_eq!(f.argv[1], new.join("gemini").canonicalize().unwrap().to_string_lossy());
        // Only in npx's cache: its newest copy, run with node.
        std::fs::remove_dir_all(nvm.join("versions/node/v9.0.0")).unwrap();
        std::fs::remove_file(new.join("gemini")).unwrap();
        // Newer copies that aren't the package (an alias under its name) or whose script is
        // outside it are passed over.
        for (h, name, v, bin) in [
            ("a1", "@google/gemini-cli", "0.60.0", "bundle/gemini.js"),
            ("b2", "@google/gemini-cli", "0.66.0", "bundle/gemini.js"),
            ("c3", "not-gemini", "9.0.0", "bundle/gemini.js"),
            ("d4", "@google/gemini-cli", "9.0.0", "../../x.js"),
        ] {
            let pkg = home.join(".npm/_npx").join(h).join("node_modules/@google/gemini-cli");
            exe(&pkg.join("bundle/gemini.js"), "#!/usr/bin/env node\n");
            exe(&home.join(".npm/_npx").join(h).join("node_modules/x.js"), "#!/usr/bin/env node\n");
            std::fs::write(
                pkg.join("package.json"),
                format!(
                    "{{\"name\":\"{name}\",\"version\":\"{v}\",\"bin\":{{\"gemini\":\"{bin}\"}}}}"
                ),
            )
            .unwrap();
        }
        let f = find_in("gemini", &path, &places(home, &nvm)).unwrap();
        assert_eq!(f.via.as_deref(), Some("npx cache"));
        assert!(f.path.to_string_lossy().contains("/b2/"), "{f:?}");
        assert_eq!(
            f.argv,
            [
                new.join("node").to_string_lossy().into_owned(),
                f.path.to_string_lossy().into_owned()
            ]
        );
        // A configured full path is taken as it is.
        let full = bin.join("codex").to_string_lossy().into_owned();
        assert_eq!(find_in(&full, "", &places(home, &nvm)).unwrap().argv, [full]);
        assert_eq!(install_hint("gemini").as_deref(), Some("npm install -g @google/gemini-cli"));
    }

    /// The hint is the version table's recommended command for the platform.
    #[test]
    fn install_hints_come_from_the_table() {
        use crate::catalog::Platform::{Linux, Macos, WindowsArm64, WindowsX64, Wsl};
        let h = |p, on| hint_on(p, on);
        assert_eq!(
            h("claude", Wsl).as_deref(),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
        assert_eq!(
            h("claude", WindowsX64).as_deref(),
            Some("irm https://claude.ai/install.ps1 | iex")
        );
        assert_eq!(
            h("codex", Macos).as_deref(),
            Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh")
        );
        assert_eq!(
            h("agy", WindowsArm64).as_deref(),
            Some("irm https://antigravity.google/cli/install.ps1 | iex")
        );
        assert_eq!(
            h("opencode", Linux).as_deref(),
            Some("curl -fsSL https://opencode.ai/install | bash")
        );
        assert_eq!(h("opencode", WindowsX64).as_deref(), Some("npm install -g opencode-ai"));
        assert_eq!(h("my-own-agent", Linux), None);
    }

    /// BOOT9: a missing agent offers setup, not a command to copy, and the no-agent words name
    /// Antigravity for a Google account (Gemini CLI only for the accounts it still serves).
    #[test]
    fn missing_agents_offer_setup() {
        assert_eq!(install_fix("codex").as_deref(), Some("`ptyro install codex` installs it"));
        assert_eq!(install_fix("agy").as_deref(), Some("`ptyro install agy` installs it"));
        assert_eq!(install_fix("git").as_deref(), Some("`ptyro install git` installs it"));
        assert_eq!(install_fix("my-own-agent"), None);
        assert_eq!(
            not_installed("codex", "codex"),
            "codex isn't installed (codex is nowhere ptyro looks): `ptyro install codex` installs it"
        );
        assert_eq!(
            not_installed("mine", "/opt/mine"),
            "mine isn't installed (/opt/mine is nowhere ptyro looks): install it, or set [agents.mine] command to its full path"
        );
        let none = no_agent();
        assert!(none.contains("`ptyro setup` installs one"), "{none}");
        assert!(none.contains("Antigravity for a Google account"), "{none}");
        assert!(
            none.contains("Gemini CLI is for Gemini API keys and enterprise accounts"),
            "{none}"
        );
        assert!(
            none.find("Claude Code") < none.find("Gemini CLI"),
            "Gemini CLI isn't first: {none}"
        );
        let chat = no_chat_agent();
        assert!(chat.contains("`ptyro setup` installs one"), "{chat}");
        assert!(
            chat.contains("Antigravity, for a Google account, runs in sessions but can't chat")
        );
        // The offers parse back to what they install; other mentions of the commands don't.
        assert_eq!(setup_offer(&none), Some(vec![]));
        assert_eq!(setup_offer(&chat), Some(vec![]));
        let fix = format!("codex isn't installed: {}", install_fix("codex").unwrap());
        assert_eq!(setup_offer(&fix), Some(vec!["codex".to_owned()]));
        assert_eq!(
            setup_offer("`ptyro install codex gh` installs them"),
            Some(vec!["codex".into(), "gh".into()])
        );
        assert_eq!(setup_offer("setup is running: `ptyro install` (process 42)"), None);
        assert_eq!(setup_offer("`ptyro install` installs"), None, "no names");
        assert_eq!(setup_offer("`ptyro install codex` moves it to the vendor's installer"), None);
        assert_eq!(setup_offer("`ptyro update codex` updates it"), None);
        assert_eq!(
            setup_offer("PR features (`wt new --pr`) need it; `ptyro install gh` installs it"),
            Some(vec!["gh".to_owned()]),
            "the offer, wherever it is in the message"
        );
        assert_eq!(setup_offer("nothing to see"), None);
    }
}

/// Windows' rules, which hold on any system: a program has an extension from `PATHEXT`, and an
/// npm `.cmd` shim runs what it starts.
#[cfg(test)]
mod windows_rules {
    use super::*;

    /// npm's shim (cmd-shim) for a package command, as `npm i -g` writes it on Windows.
    fn shim(target: &str) -> String {
        format!(
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\n\
             SETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  \
             SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n)\r\n\r\n\
             endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \
             \"%dp0%\\{target}\" %*\r\n"
        )
    }

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn programs_have_extensions_and_npm_shims_run_what_they_start() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let npm = home.join("AppData/Roaming/npm");
        let local_bin = home.join(".local/bin");
        let places = Places {
            dirs: vec![(npm.clone(), "npm".into()), (local_bin.clone(), "~\\.local\\bin".into())],
            nvm: home.join(".nvm"),
            npx: home.join("npm-cache/_npx"),
            exts: Some(".COM;.EXE;.BAT;.CMD".into()),
        };
        let path = home.join("bin").to_string_lossy().into_owned();
        // Claude's native installer: `claude.exe`, run as it is.
        write(&local_bin.join("claude.exe"), "MZ");
        let f = find_in("claude", &path, &places).unwrap();
        assert_eq!(f.argv, [local_bin.join("claude.exe").to_string_lossy()]);
        assert_eq!(f.via.as_deref(), Some("~\\.local\\bin"));
        // npm's three files for codex: the extensionless `sh` shim is passed over, and the
        // `.cmd` runs its script with the `node.exe` beside it (no node on PATH).
        let script =
            npm.join("node_modules").join("@openai").join("codex").join("bin").join("codex.js");
        write(&script, "#!/usr/bin/env node\n");
        write(&npm.join("codex"), "#!/bin/sh\n");
        write(&npm.join("codex.cmd"), &shim(r"node_modules\@openai\codex\bin\codex.js"));
        write(&npm.join("node.exe"), "MZ");
        let f = find_in("codex", &path, &places).unwrap();
        assert_eq!(
            f.argv.iter().map(PathBuf::from).collect::<Vec<_>>(),
            [npm.join("node.exe"), script.clone()],
            "{f:?}"
        );
        assert_eq!((f.path, f.via.as_deref()), (script, Some("npm")));
        // A shim to a package's own `.exe` runs that `.exe`.
        let bin = npm.join("node_modules/opencode-ai/bin/opencode.exe");
        write(&bin, "MZ");
        write(&npm.join("opencode.cmd"), &shim(r"node_modules\opencode-ai\bin\opencode.exe"));
        let as_paths = |argv: &[String]| argv.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(as_paths(&find_in("opencode", &path, &places).unwrap().argv), [bin]);
        // On PATH too: what the shim starts, found by `PATHEXT`.
        let on_path = home.join("bin");
        let gem = on_path.join("node_modules/@google/gemini-cli/bundle/gemini.js");
        write(&gem, "#!/usr/bin/env node\n");
        write(
            &on_path.join("gemini.cmd"),
            &shim(r"node_modules\@google\gemini-cli\bundle\gemini.js"),
        );
        write(&on_path.join("node.exe"), "MZ");
        let f = find_in("gemini", &path, &places).unwrap();
        assert_eq!(as_paths(&f.argv), [on_path.join("node.exe"), gem]);
        assert_eq!(f.via, None);
        // A shim whose target climbs out of its folder, or isn't there, runs as itself.
        write(&npm.join("agy.cmd"), &shim(r"..\..\evil.exe"));
        write(&home.join("AppData/evil.exe"), "MZ");
        assert_eq!(
            find_in("agy", &path, &places).unwrap().argv,
            [npm.join("agy.cmd").to_string_lossy()]
        );
        assert_eq!(shim_target(&npm.join("codex")), None, "only a .cmd is a shim");
        // A shim left behind by an install that rolled back (its package gone): not found.
        write(&npm.join("gemini.cmd"), &shim(r"node_modules\@google\gemini-cli\bundle\gemini.js"));
        std::fs::remove_dir_all(home.join("bin")).unwrap();
        assert!(find_in("gemini", &path, &places).is_none(), "a broken shim runs nothing");
    }
}
