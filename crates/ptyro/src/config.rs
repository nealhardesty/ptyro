//! `ptyro config path|check|show|edit` (SPEC §13).

use std::io::{IsTerminal, Write};

use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};

use crate::ConfigCmd;

pub fn run(paths: &Paths, cmd: ConfigCmd) -> Result<i32, Error> {
    let path = Config::path(paths);
    match cmd {
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
        ConfigCmd::Edit => edit(&path),
    }
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
    let problems = ptyro_tui::keymap::validate(&config);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    if !ptyro_tui::theme::exists(&config, &config.ui.theme) {
        return Err(format!(
            "ui.theme: unknown theme {:?} (built in: {}; or define [themes.{}])",
            config.ui.theme,
            ptyro_tui::theme::BUILTIN.join(", "),
            config.ui.theme
        ));
    }
    if !path.exists() {
        notes.push(format!("create {} to change settings (`ptyro config edit`)", path.display()));
    }
    Ok(notes)
}

pub(crate) const STARTER: &str = r#"# ptyro configuration. Every key is optional; see SPEC §13 for all of them.
# Changes apply live (the daemon reloads this file when it changes).

[ui]
# prefix = ["C-q", "C-Space"] # the keys that start ptyro commands; one ("C-]") or several
# theme = "ptyro-dark"      # ptyro-dark, ptyro-light, high-contrast, or one of [themes.*]
# sidebar = true
# sidebar_width = 30
# mouse = true
# on_stop = "next"         # after you stop a pane's session: next (its neighbour) | stay
# sidebar_detail = "auto"  # lines under each session row: auto | compact | full
# which_key_delay = 400    # ms after the prefix before the menu of next keys; 0: never

[daemon]
# scrollback_lines = 5000

[summaries]
# enabled = true           # a `≈` line after each agent turn, from a small model on its account

[keys]
# Rebind after-prefix keys: "key" = "action [args]"; "none" unbinds. `C-q ?` lists actions.
# "a" = "session.next"
# "global:M-1" = "session.focus 1"   # global: works without the prefix
"#;

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

fn edit(path: &std::path::Path) -> Result<i32, Error> {
    if !path.exists() {
        write_private(path, STARTER)?;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    loop {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
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
