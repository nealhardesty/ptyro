//! Finding an agent's program when it isn't on `PATH` (out of the box first, SPEC §1.3): a daemon
//! started by a service manager has a short `PATH`, and npm-installed agents live where nvm, an
//! npm prefix or `npx`'s cache put them. Gemini CLI run as `npx …` (a shell alias) is only ever in
//! that cache. ptyro never downloads an agent: it runs one that is already on disk, and when none
//! is, it says how to install it ([`install_hint`]).
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

/// How to install a built-in agent's program, for the fix ptyro offers when it is missing.
pub fn install_hint(program: &str) -> Option<String> {
    match program {
        "claude" if cfg!(windows) => Some("irm https://claude.ai/install.ps1 | iex".into()),
        "claude" => Some("curl -fsSL https://claude.ai/install.sh | bash".into()),
        "agy" => None,
        p => NPM.iter().find(|(n, _)| *n == p).map(|(_, pkg)| format!("npm i -g {pkg}")),
    }
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
    find_in(prog, path, &places)
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
        let real = f.canonicalize().unwrap_or_else(|_| f.clone());
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
        assert_eq!(install_hint("gemini").as_deref(), Some("npm i -g @google/gemini-cli"));
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
        let script = npm.join("node_modules/@openai/codex/bin/codex.js");
        write(&script, "#!/usr/bin/env node\n");
        write(&npm.join("codex"), "#!/bin/sh\n");
        write(&npm.join("codex.cmd"), &shim(r"node_modules\@openai\codex\bin\codex.js"));
        write(&npm.join("node.exe"), "MZ");
        let f = find_in("codex", &path, &places).unwrap();
        assert_eq!(
            f.argv,
            [npm.join("node.exe").to_string_lossy(), script.to_string_lossy()],
            "{f:?}"
        );
        assert_eq!((f.path, f.via.as_deref()), (script, Some("npm")));
        // A shim to a package's own `.exe` runs that `.exe`.
        let bin = npm.join("node_modules/opencode-ai/bin/opencode.exe");
        write(&bin, "MZ");
        write(&npm.join("opencode.cmd"), &shim(r"node_modules\opencode-ai\bin\opencode.exe"));
        assert_eq!(find_in("opencode", &path, &places).unwrap().argv, [bin.to_string_lossy()]);
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
        assert_eq!(f.argv, [on_path.join("node.exe").to_string_lossy(), gem.to_string_lossy()]);
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
