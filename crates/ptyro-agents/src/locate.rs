//! Finding an agent's program when it isn't on `PATH` (out of the box first, SPEC §1.3): a daemon
//! started by a service manager has a short `PATH`, and npm-installed agents live where nvm, an
//! npm prefix or `npx`'s cache put them. Gemini CLI run as `npx …` (a shell alias) is only ever in
//! that cache. ptyro never downloads an agent: it runs one that is already on disk, and when none
//! is, it says how to install it ([`install_hint`]).

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

/// How to install a built-in agent's program, for the fix ptyro offers when it is missing.
pub fn install_hint(program: &str) -> Option<String> {
    match program {
        "claude" => Some("curl -fsSL https://claude.ai/install.sh | bash".into()),
        "agy" => None,
        p => NPM.iter().find(|(n, _)| *n == p).map(|(_, pkg)| format!("npm i -g {pkg}")),
    }
}

/// `prog` on `path`, else where npm, nvm and `npx` put it under `$HOME`.
pub fn find(prog: &str, path: &str) -> Option<Found> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)?;
    let nvm = std::env::var_os("NVM_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".nvm"));
    let mut dirs: Vec<(PathBuf, String)> = Vec::new();
    if let Some(p) = std::env::var_os("NPM_CONFIG_PREFIX") {
        dirs.push((PathBuf::from(p).join("bin"), "npm prefix".into()));
    }
    for d in [".npm-global/bin", ".local/bin", ".volta/bin", ".bun/bin", ".claude/local"] {
        dirs.push((home.join(d), format!("~/{d}")));
    }
    for d in ["/opt/homebrew/bin", "/usr/local/bin"] {
        dirs.push((PathBuf::from(d), d.to_owned()));
    }
    find_in(prog, path, &home, &dirs, &nvm)
}

/// `dirs`: where to look after `PATH`, each with how to say it; then nvm, then `npx`'s cache.
fn find_in(
    prog: &str,
    path: &str,
    home: &Path,
    dirs: &[(PathBuf, String)],
    nvm: &Path,
) -> Option<Found> {
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return p.is_file().then(|| Found { argv: vec![prog.to_owned()], path: p, via: None });
    }
    let on_path = |p: &str| std::env::split_paths(path).map(|d| d.join(p)).find(|f| f.is_file());
    if let Some(p) = on_path(prog) {
        return Some(Found { argv: vec![prog.to_owned()], path: p, via: None });
    }
    let node_bins = nvm_bins(nvm);
    let mut dirs = dirs.to_vec();
    dirs.extend(node_bins.iter().map(|d| (d.clone(), "nvm".to_owned())));
    // A Node script runs with a `node` from PATH, else the one beside it, else nvm's newest.
    let node = |beside: &Path| -> Option<PathBuf> {
        on_path("node")
            .or_else(|| Some(beside.join("node")).filter(|n| n.is_file()))
            .or_else(|| node_bins.iter().map(|d| d.join("node")).find(|n| n.is_file()))
    };
    for (dir, via) in &dirs {
        let f = dir.join(prog);
        if !f.is_file() {
            continue;
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
    let script = npx_cached(&home.join(".npm/_npx"), pkg, prog)?;
    let n = node(script.parent()?)?;
    Some(Found {
        argv: vec![n.to_string_lossy().into_owned(), script.to_string_lossy().into_owned()],
        path: script,
        via: Some("npx cache".into()),
    })
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
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&format!("{h}/")) => format!("~{}", &s[h.len()..]),
        _ => s.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
        let f = find_in("codex", &path, home, &[], &nvm).unwrap();
        assert_eq!((f.argv, f.via), (vec!["codex".to_owned()], None));
        // Nowhere: nothing (and never a download).
        assert!(find_in("gemini", &path, home, &[], &nvm).is_none());
        // In nvm's newest node: a Node script runs with the `node` beside it (none on PATH).
        let old = nvm.join("versions/node/v9.0.0/bin");
        let new = nvm.join("versions/node/v24.18.0/bin");
        exe(&old.join("gemini"), "#!/usr/bin/env node\n");
        exe(&new.join("gemini"), "#!/usr/bin/env node\n");
        exe(&new.join("node"), "");
        let f = find_in("gemini", &path, home, &[], &nvm).unwrap();
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
        let f = find_in("gemini", &path, home, &[], &nvm).unwrap();
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
        assert_eq!(find_in(&full, "", home, &[], &nvm).unwrap().argv, [full]);
        assert_eq!(install_hint("gemini").as_deref(), Some("npm i -g @google/gemini-cli"));
    }
}
