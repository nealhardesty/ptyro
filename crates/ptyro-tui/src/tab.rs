//! Open a session in a new tab or pane of the terminal ptyro runs in (ROADMAP WIN3): `ptyro attach
//! S --tab|--pane`, the TUI's `session.tab` / `session.pane`, and the web's "open on the computer".
//!
//! The innermost one that can be asked wins: tmux (a window, or a split), else Windows Terminal
//! (`WT_SESSION`; a tab, or a split pane) when ptyro runs in WSL inside it. Anywhere else there
//! is no one to ask, and the error says what to run instead.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Tab,
    Pane,
}

/// What runs where: the new tab's own `ptyro [--remote R] attach <addr>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub argv: Vec<String>,
    /// For the message: "a new Windows Terminal tab".
    pub what: String,
}

/// The facts `plan` decides from (read from the environment by [`here`]).
#[derive(Debug, Clone, Default)]
pub struct Here {
    pub tmux: bool,
    pub windows_terminal: bool,
    /// Inside WSL: the distro, and where `wt.exe` is.
    pub wsl_distro: Option<String>,
    pub wt_exe: Option<PathBuf>,
    /// This ptyro.
    pub exe: PathBuf,
}

pub fn here() -> Here {
    let wsl = ptyro_core::wsl::detect().filter(|w| w.interop);
    Here {
        tmux: std::env::var_os("TMUX").is_some_and(|v| !v.is_empty()),
        windows_terminal: std::env::var_os("WT_SESSION").is_some_and(|v| !v.is_empty()),
        wsl_distro: wsl.and_then(|w| w.distro.clone()),
        wt_exe: wsl.and_then(wt_in_wsl),
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ptyro")),
    }
}

/// `wt.exe` as WSL can run it: on `PATH`, else in the Windows user's `WindowsApps` (where the
/// Store puts it, often off WSL's `PATH`).
fn wt_in_wsl(w: &ptyro_core::wsl::Wsl) -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("wt.exe")).find(|p| p.is_file()));
    on_path.or_else(|| {
        w.windows_home()
            .map(|h| h.join("AppData/Local/Microsoft/WindowsApps/wt.exe"))
            .filter(|p| p.exists())
    })
}

/// How to open `ptyro attach <addr>` (with `remote`, so it reaches the same daemon) in a new tab or
/// pane here.
pub fn plan(place: Place, addr: &str, remote: Option<&str>, h: &Here) -> Result<Plan, String> {
    let mut ptyro = vec![h.exe.to_string_lossy().into_owned()];
    if let Some(r) = remote {
        ptyro.extend(["--remote".to_owned(), r.to_owned()]);
    }
    ptyro.extend(["attach".to_owned(), addr.to_owned()]);
    if h.tmux {
        let verb = match place {
            Place::Tab => "new-window",
            Place::Pane => "split-window",
        };
        let mut argv = vec!["tmux".to_owned(), verb.to_owned(), "--".to_owned()];
        argv.extend(ptyro);
        let what = match place {
            Place::Tab => "a new tmux window",
            Place::Pane => "a new tmux pane",
        };
        return Ok(Plan { argv, what: what.into() });
    }
    if h.windows_terminal {
        let Some(wt) = &h.wt_exe else {
            return Err(
                "Windows Terminal's wt.exe isn't reachable from WSL (is interop on?)".into()
            );
        };
        let verb = match place {
            Place::Tab => "new-tab",
            Place::Pane => "split-pane",
        };
        // `-w 0`: this window. `--`: what follows is the command, whatever it holds. The tab
        // runs wsl.exe, back into this distro, then ptyro.
        let mut argv = vec![
            wt.to_string_lossy().into_owned(),
            "-w".into(),
            "0".into(),
            verb.into(),
            "--".into(),
            "wsl.exe".into(),
        ];
        if let Some(d) = &h.wsl_distro {
            argv.extend(["-d".to_owned(), d.clone()]);
        }
        argv.extend(["--cd".to_owned(), "~".to_owned(), "--".to_owned()]);
        // wt splits its command line at every `;` (its own separator, even after `--`); `\;`
        // keeps one in an argument.
        let n = argv.len();
        argv.extend(ptyro);
        for a in &mut argv[n..] {
            *a = a.replace(';', "\\;");
        }
        let what = match place {
            Place::Tab => "a new Windows Terminal tab",
            Place::Pane => "a new Windows Terminal pane",
        };
        return Ok(Plan { argv, what: what.into() });
    }
    Err(format!(
        "this terminal can't be asked for a {} (ptyro knows tmux and Windows Terminal): open one and run `{}`",
        match place {
            Place::Tab => "tab",
            Place::Pane => "pane",
        },
        shown(&ptyro)
    ))
}

/// The command as someone would type it (for a message, not to run).
fn shown(argv: &[String]) -> String {
    argv.iter()
        .enumerate()
        .map(|(i, a)| {
            // The binary is named by its file name: a path full of temp folders helps nobody.
            let a = if i == 0 {
                Path::new(a).file_name().map_or(a.clone(), |n| n.to_string_lossy().into_owned())
            } else {
                a.clone()
            };
            if a.contains(' ') { format!("'{a}'") } else { a }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run the plan (without waiting for the new tab): `Ok` says where it opened.
pub fn open(place: Place, addr: &str, remote: Option<&str>) -> Result<String, String> {
    let p = plan(place, addr, remote, &here())?;
    let (exe, args) = p.argv.split_first().ok_or("nothing to run")?;
    let status = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("{exe}: {e}"))?;
    if status.status.success() {
        Ok(format!("opened in {}", p.what))
    } else {
        let said = String::from_utf8_lossy(&status.stderr).trim().to_owned();
        Err(format!(
            "{exe} didn't open it ({}){}",
            status.status,
            if said.is_empty() { String::new() } else { format!(": {said}") }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> Here {
        Here { exe: PathBuf::from("/home/me/.local/bin/ptyro"), ..Here::default() }
    }

    #[test]
    fn tmux_wins_inside_windows_terminal() {
        let here = Here { tmux: true, windows_terminal: true, ..h() };
        let p = plan(Place::Tab, "01abc", None, &here).unwrap();
        assert_eq!(
            p.argv,
            ["tmux", "new-window", "--", "/home/me/.local/bin/ptyro", "attach", "01abc"]
        );
        let p = plan(Place::Pane, "01abc", Some("box"), &here).unwrap();
        assert_eq!(&p.argv[..2], ["tmux", "split-window"]);
        assert!(p.argv.ends_with(&[
            "--remote".into(),
            "box".into(),
            "attach".into(),
            "01abc".into()
        ]));
    }

    #[test]
    fn windows_terminal_from_wsl_goes_back_into_the_distro() {
        let here = Here {
            windows_terminal: true,
            wsl_distro: Some("Ubuntu".into()),
            wt_exe: Some(PathBuf::from(
                "/mnt/c/Users/me/AppData/Local/Microsoft/WindowsApps/wt.exe",
            )),
            ..h()
        };
        let p = plan(Place::Tab, "01abc", None, &here).unwrap();
        assert_eq!(
            p.argv,
            [
                "/mnt/c/Users/me/AppData/Local/Microsoft/WindowsApps/wt.exe",
                "-w",
                "0",
                "new-tab",
                "--",
                "wsl.exe",
                "-d",
                "Ubuntu",
                "--cd",
                "~",
                "--",
                "/home/me/.local/bin/ptyro",
                "attach",
                "01abc"
            ]
        );
        assert_eq!(p.what, "a new Windows Terminal tab");
        assert_eq!(plan(Place::Pane, "x", None, &here).unwrap().argv[3], "split-pane");
        let no_wt = Here { wt_exe: None, ..here };
        assert!(plan(Place::Tab, "x", None, &no_wt).unwrap_err().contains("wt.exe"));
    }

    #[test]
    fn a_semicolon_reaches_windows_terminal_escaped() {
        let here = Here {
            windows_terminal: true,
            wt_exe: Some(PathBuf::from("/mnt/c/wt.exe")),
            exe: PathBuf::from("/odd;name/ptyro"),
            ..Here::default()
        };
        let p = plan(Place::Tab, "a;b", None, &here).unwrap();
        assert_eq!(p.argv[9], r"/odd\;name/ptyro");
        assert_eq!(p.argv.last().map(String::as_str), Some(r"a\;b"));
    }

    #[test]
    fn elsewhere_it_says_what_to_run() {
        let e = plan(Place::Tab, "01abc", Some("me@box"), &h()).unwrap_err();
        assert!(
            e.contains("can't be asked for a tab")
                && e.contains("`ptyro --remote me@box attach 01abc`"),
            "{e}"
        );
    }
}
