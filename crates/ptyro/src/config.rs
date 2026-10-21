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
    let config = Config::load(path).map_err(|e| e.to_string())?;
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

const STARTER: &str = r#"# ptyro configuration. Every key is optional; see SPEC §13 for all of them.
# Changes apply live (the daemon reloads this file when it changes).

[ui]
# prefix = "C-Space"        # the key that starts ptyro commands, e.g. "C-a", "C-]"
# theme = "ptyro-dark"      # ptyro-dark, ptyro-light, high-contrast, or one of [themes.*]
# sidebar = true
# sidebar_width = 30
# mouse = true

[daemon]
# scrollback_lines = 10000

[keys]
# Rebind after-prefix keys: "key" = "action [args]"; "none" unbinds. `C-Space ?` lists actions.
# "a" = "session.next"
# "global:M-1" = "session.focus 1"   # global: works without the prefix
"#;

fn edit(path: &std::path::Path) -> Result<i32, Error> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::internal(e.to_string()))?;
        }
        std::fs::write(path, STARTER).map_err(|e| Error::internal(e.to_string()))?;
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
