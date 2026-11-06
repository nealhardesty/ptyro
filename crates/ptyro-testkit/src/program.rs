//! Stand-in programs on `PATH` for tests (WIN13): a fake `gh`, `claude`, `ssh` or `tailscale`
//! that runs something else. On Unix it is a shell script; Windows can't run one by name (and
//! Rust's `Command` finds only `.exe` files on `PATH`), so there it is the debug `ptyro.exe`,
//! hard-linked (else copied) under the program's name, with what to run beside it in
//! `<name>.shim`: [`run_shim`] (called first thing by a debug `ptyro.exe`) runs that.

use std::path::{Path, PathBuf};

/// A program `name` in `dir`: on Unix `script` (the body of a `#!/bin/sh` script); on Windows
/// `argv`, with the program's own arguments after it, and `env` set. `ptyro` is the debug
/// `ptyro.exe` (`env!("CARGO_BIN_EXE_ptyro")`), which Windows' stand-in is made of.
pub fn program(
    dir: &Path,
    name: &str,
    ptyro: &str,
    script: &str,
    argv: &[&str],
    env: &[(&str, &str)],
) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(unix)]
    {
        let _ = (ptyro, argv, env);
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(not(unix))]
    {
        let _ = script;
        let path = dir.join(format!("{name}.exe"));
        if std::fs::hard_link(ptyro, &path).is_err() {
            std::fs::copy(ptyro, &path).unwrap();
        }
        set_shim(&path, argv, env);
        path
    }
}

/// Windows: what the stand-in at `exe` runs (written again when it changes, e.g. once the
/// daemon it reaches is running). An `env` value of `None`-like `"\0"` unsets the variable.
pub fn set_shim(exe: &Path, argv: &[&str], env: &[(&str, &str)]) {
    let doc = serde_json::json!({
        "argv": argv,
        "env": env.iter().map(|(k, v)| (k.to_string(), serde_json::Value::from(*v))).collect::<serde_json::Map<_, _>>(),
    });
    std::fs::write(exe.with_extension("shim"), doc.to_string()).unwrap();
}

/// What a stand-in that only talks says, as its `argv` for [`program`]: it prints `words` (each
/// `${NAME}` in them the variable's value) and its own arguments on a line, then waits `secs`
/// seconds (a fake editor, a fake shell).
pub fn say_then_wait(words: &str, secs: u32) -> Vec<String> {
    vec![format!("say:{words}"), secs.to_string()]
}

/// `PATH` with `dir` first, joined the platform's way (`:`; `;` on Windows).
pub fn path_with(dir: &Path) -> String {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let all = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&rest));
    std::env::join_paths(all).unwrap().to_string_lossy().into_owned()
}

/// A debug `ptyro.exe` started under another name with a `.shim` beside it is a stand-in
/// ([`program`]): run what the shim says, with our arguments after it, and give back its exit
/// code. `None` for ptyro itself.
pub fn run_shim() -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let stem = exe.file_stem()?.to_string_lossy().to_ascii_lowercase();
    if stem == "ptyro" || stem == "ptyrow" {
        return None;
    }
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(exe.with_extension("shim")).ok()?).ok()?;
    let argv: Vec<String> =
        doc["argv"].as_array()?.iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
    let (prog, args) = argv.split_first()?;
    if let Some(words) = prog.strip_prefix("say:") {
        let rest: Vec<String> = std::env::args().skip(1).collect();
        let mut said = words.to_owned();
        while let Some(i) = said.find("${") {
            let Some(j) = said[i..].find('}') else { break };
            let value = std::env::var(&said[i + 2..i + j]).unwrap_or_default();
            said.replace_range(i..=i + j, &value);
        }
        println!("{said} {}", rest.join(" "));
        let secs = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_secs(secs));
        return Some(0);
    }
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).args(std::env::args_os().skip(1));
    for (k, v) in doc["env"].as_object().into_iter().flatten() {
        match v.as_str() {
            Some("\0") => cmd.env_remove(k),
            v => cmd.env(k, v.unwrap_or_default()),
        };
    }
    Some(match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("{stem} (a test's stand-in): {prog}: {e}");
            127
        }
    })
}
