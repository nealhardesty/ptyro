//! Making ptyro an app Windows knows (WIN10, SPEC §20.12): its AppUserModelID, so its toasts say
//! "ptyro" with its icon, and its `ptyro:` link handler, so a toast's click opens the session.
//! Both live in the current user's registry (`HKCU\Software\Classes`), which is the documented way
//! for an app installed without a package, and needs no administrator.
//!
//! [`ensure`] is cheap (it reads, and writes only what differs), so the native daemon and the
//! Windows client call it as they start: out of the box, no step to remember; the WSL daemon's
//! toasts use it once `ptyro.exe` has run on Windows. `ptyro uninstall` takes it all away again.

// Windows' alone, but what it writes is tested everywhere: elsewhere only the tests use it.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

use ptyro_core::winapp::{AUMID, Link, aumid_key, scheme_key};

/// The icon toasts show (the web app's), written once to ptyro's data folder for Windows to read.
const ICON: &[u8] = include_bytes!("../../../web/dist/icon-192.png");

/// Where the icon goes.
pub fn icon_path(data: &Path) -> PathBuf {
    data.join("ptyro.png")
}

/// What opens `ptyro:` links: `ptyrow.exe` beside us (no window), else ptyro.exe itself.
fn handler(exe: &Path) -> String {
    let w = exe.with_file_name("ptyrow.exe");
    let prog = if w.is_file() { w } else { exe.to_path_buf() };
    format!("\"{}\" open-url \"%1\"", prog.display())
}

/// What Windows should have, as (key, value name, value): pure, so it's tested everywhere.
pub fn entries(exe: &Path, icon: &Path) -> Vec<(String, &'static str, String)> {
    let scheme = scheme_key();
    vec![
        (aumid_key(), "DisplayName", "ptyro".into()),
        (aumid_key(), "IconUri", icon.display().to_string()),
        (scheme.clone(), "", "URL:ptyro link".into()),
        (scheme.clone(), "URL Protocol", String::new()),
        (format!("{scheme}\\DefaultIcon"), "", format!("\"{}\",0", exe.display())),
        (format!("{scheme}\\shell\\open\\command"), "", handler(exe)),
    ]
}

/// Register ptyro (this `ptyro.exe`) with Windows when it isn't, or points elsewhere (an install
/// that moved). Best effort: a failure is logged, never fatal. Not from a sandbox (`PTYRO_HOME`,
/// as tests run): the registration is the computer's, and it names the installed ptyro.
#[cfg(windows)]
pub fn ensure(data: &Path) {
    if std::env::var_os("PTYRO_HOME").is_some_and(|h| !h.is_empty()) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = ptyro_core::os::plain(&exe);
    let icon = icon_path(data);
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        let _ = std::fs::create_dir_all(data);
        if let Err(e) = std::fs::write(&icon, ICON) {
            tracing::debug!("toast icon {}: {e}", icon.display());
        }
    }
    for (key, name, value) in entries(&exe, &icon) {
        if ptyro_pty::winreg::get_string(&key, name).as_deref() == Some(value.as_str()) {
            continue;
        }
        if let Err(e) = ptyro_pty::winreg::set_string(&key, name, &value) {
            tracing::debug!("registering ptyro with Windows: {e}");
            return;
        }
    }
}

/// Elsewhere there is nothing to register.
#[cfg(not(windows))]
pub fn ensure(_data: &Path) {}

/// What Windows has for ptyro now, for `doctor`: (registered at all, its links open with a
/// program that exists).
#[cfg(windows)]
pub fn status() -> (bool, Option<PathBuf>) {
    let named = ptyro_pty::winreg::get_string(&aumid_key(), "DisplayName").is_some();
    let opener =
        ptyro_pty::winreg::get_string(&format!("{}\\shell\\open\\command", scheme_key()), "")
            .and_then(|c| c.split('"').nth(1).map(PathBuf::from));
    (named, opener)
}

/// Take ptyro's registration away (`ptyro uninstall`): its AppUserModelID and its link handler.
#[cfg(windows)]
pub fn remove(data: &Path) -> std::io::Result<()> {
    ptyro_pty::winreg::delete_tree(&aumid_key())?;
    ptyro_pty::winreg::delete_tree(&scheme_key())?;
    let _ = std::fs::remove_file(icon_path(data));
    Ok(())
}

/// What a `ptyro:` link opens: a Windows Terminal tab (`wt` when it's installed) running
/// `ptyro [--remote R] attach <session>`, else that command in a console window of its own (the
/// `bool`: start it with a new console). Pure, so it's tested everywhere.
pub fn open_plan(link: &Link, exe: &Path, wt: Option<&Path>) -> (Vec<String>, bool) {
    if let Some(wt) = wt {
        let here = ptyro_tui::tab::Here {
            windows_terminal: true,
            windows: true,
            exe: exe.to_path_buf(),
            ..Default::default()
        };
        if let Ok(mut p) = ptyro_tui::tab::plan(
            ptyro_tui::tab::Place::Tab,
            &link.session,
            link.remote.as_deref(),
            &here,
        ) {
            p.argv[0] = wt.display().to_string();
            return (p.argv, false);
        }
    }
    let mut argv = vec![exe.display().to_string()];
    if let Some(r) = &link.remote {
        argv.extend(["--remote".to_owned(), r.clone()]);
    }
    argv.extend(["attach".to_owned(), link.session.clone()]);
    (argv, true)
}

/// Windows Terminal's `wt.exe`: on `PATH`, else where the Store puts it.
#[cfg(windows)]
fn wt_exe() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("wt.exe")).find(|p| p.is_file()))
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|d| PathBuf::from(d).join("Microsoft\\WindowsApps\\wt.exe"))
                .filter(|p| p.exists())
        })
}

/// `ptyro open-url <link>` (hidden; what Windows runs, through `ptyrow.exe`, when a toast or a
/// `ptyro:` link is clicked): check the link, then show its session. A link that isn't one ptyro
/// writes opens nothing.
#[cfg(windows)]
pub fn open_url(uri: &str) -> i32 {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let link = match ptyro_core::winapp::parse_link(uri) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ptyro: {e}");
            return 2;
        }
    };
    let Ok(exe) = std::env::current_exe() else { return 2 };
    let exe = ptyro_core::os::plain(&exe);
    let (argv, console) = open_plan(&link, &exe, wt_exe().as_deref());
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if console {
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    } else {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    match cmd.spawn() {
        Ok(_) => 0,
        Err(e) if !console => {
            // wt.exe is there but wouldn't start: a console window of its own, then.
            tracing::debug!("{}: {e}", argv[0]);
            let (argv, _) = open_plan(&link, &exe, None);
            let started = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn();
            i32::from(started.is_err()) * 2
        }
        Err(e) => {
            eprintln!("ptyro: {}: {e}", argv[0]);
            2
        }
    }
}

/// Elsewhere links are Windows' (a toast's click); say so.
#[cfg(not(windows))]
pub fn open_url(uri: &str) -> i32 {
    match ptyro_core::winapp::parse_link(uri) {
        Ok(l) => {
            eprintln!(
                "ptyro: links open on Windows; here, run `ptyro {}attach {}`",
                l.remote.map(|r| format!("--remote {r} ")).unwrap_or_default(),
                l.session
            );
            2
        }
        Err(e) => {
            eprintln!("ptyro: {e}");
            2
        }
    }
}

/// The AppUserModelID's key, for messages.
pub fn aumid() -> &'static str {
    AUMID
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handler quotes both paths, a space in the user's name included, and names the
    /// windowless `ptyrow.exe` when it's there.
    #[test]
    fn the_registration_names_ptyro_and_its_link_handler() {
        let d = tempfile::tempdir().unwrap();
        let exe = d.path().join("John Smith").join("ptyro.exe");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let icon = d.path().join("ptyro.png");
        let get = |e: &[(String, &str, String)], key: &str, name: &str| {
            e.iter().find(|(k, n, _)| k.ends_with(key) && *n == name).map(|(_, _, v)| v.clone())
        };
        let e = entries(&exe, &icon);
        assert_eq!(get(&e, "dev.ptyro.app", "DisplayName").as_deref(), Some("ptyro"));
        assert_eq!(get(&e, "Classes\\ptyro", "URL Protocol").as_deref(), Some(""));
        assert_eq!(
            get(&e, "shell\\open\\command", ""),
            Some(format!("\"{}\" open-url \"%1\"", exe.display())),
            "ptyro.exe itself without ptyrow.exe"
        );
        std::fs::write(exe.with_file_name("ptyrow.exe"), b"MZ").unwrap();
        let e = entries(&exe, &icon);
        assert_eq!(
            get(&e, "shell\\open\\command", ""),
            Some(format!("\"{}\" open-url \"%1\"", exe.with_file_name("ptyrow.exe").display()))
        );
        assert_eq!(aumid(), "dev.ptyro.app");
    }

    /// A link opens a Windows Terminal tab running ptyro.exe attach (the remote carried), else
    /// that command in a console of its own; a `;` in an argument can't start a second wt command.
    #[test]
    fn a_link_opens_a_terminal_tab_or_a_console() {
        let exe = Path::new(r"C:\Users\Ann Bo\ptyro\ptyro.exe");
        let wt = Path::new(r"C:\Users\Ann Bo\AppData\Local\Microsoft\WindowsApps\wt.exe");
        let link = Link { session: "01abc".into(), remote: Some("wsl://Ubuntu".into()) };
        let (argv, console) = open_plan(&link, exe, Some(wt));
        assert!(!console);
        assert_eq!(
            argv,
            [
                wt.display().to_string(),
                "-w".into(),
                "0".into(),
                "new-tab".into(),
                "--".into(),
                exe.display().to_string(),
                "--remote".into(),
                "wsl://Ubuntu".into(),
                "attach".into(),
                "01abc".into()
            ]
        );
        let (argv, console) = open_plan(&Link { session: "c1".into(), remote: None }, exe, None);
        assert!(console);
        assert_eq!(argv, [exe.display().to_string(), "attach".into(), "c1".into()]);
    }
}
