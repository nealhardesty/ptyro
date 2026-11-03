//! What differs between Unix and Windows for ptyro's own files and processes (ROADMAP WIN3,
//! SPEC §20.11), in one place, so the rest of the code says what it means ("a file only its owner
//! reads") rather than how one platform does it.
//!
//! - **Private files:** mode 0600 on Unix. On Windows a file under the user's profile
//!   (`%APPDATA%`, `%LOCALAPPDATA%`, where ptyro keeps its own) inherits an access list that only
//!   that user (and administrators) can read, so there is nothing to set.
//! - **Executables:** the mode's execute bits on Unix; on Windows a file whose extension is in
//!   `PATHEXT` (`.exe`, `.cmd`…).
//! - **Who am I:** a uid on Unix. Windows has none (the client there talks to a WSL daemon, whose
//!   uid is the one that counts).

use std::fs::OpenOptions;
use std::path::Path;

/// Variables a client may pass along with a spawn so sessions see the *client's* SSH agent and
/// display, like tmux's `update-environment` (SPEC §5.1). Here, not in `ptyro-pty`, because a
/// client on Windows has no PTYs.
pub const CLIENT_ENV: &[&str] =
    &["SSH_AUTH_SOCK", "SSH_CONNECTION", "DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"];

/// Open options for a file only its owner reads (create it with this).
pub fn private(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
    }
    #[cfg(not(unix))]
    opts
}

/// Make an existing file readable by its owner only (`chmod 600`); nothing to do on Windows.
pub fn make_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// A directory only its owner can enter (`0700` on Unix), made if missing.
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The permission bits as Unix writes them (`0o644`), for messages like "is 644: others can read
/// it". `None` on Windows, where access lists aren't modes.
pub fn mode(meta: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// Whether `p` is a file this user can run.
pub fn is_executable(p: &Path) -> bool {
    let Ok(meta) = p.metadata() else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        windows_runnable(p, &exts)
    }
}

/// On Windows, a file runs when its extension is one `PATHEXT` lists (case doesn't matter).
pub fn windows_runnable(p: &Path, pathext: &str) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else { return false };
    pathext.split(';').any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(ext))
}

/// The names a program `prog` may have on disk: itself on Unix; on Windows also with each
/// `PATHEXT` extension (`code` is `code.cmd`), unless it has one already.
pub fn program_names(prog: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        vec![prog.to_owned()]
    }
    #[cfg(not(unix))]
    {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        windows_program_names(prog, &exts)
    }
}

/// [`program_names`] on Windows, with `PATHEXT` given.
pub fn windows_program_names(prog: &str, pathext: &str) -> Vec<String> {
    let mut v = vec![prog.to_owned()];
    if Path::new(prog).extension().is_none() {
        v.extend(
            pathext
                .split(';')
                .filter(|x| !x.is_empty())
                .map(|x| format!("{prog}{}", x.to_ascii_lowercase())),
        );
    }
    v
}

/// This process's uid; `None` on Windows.
pub fn uid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(rustix::process::getuid().as_raw())
    }
    #[cfg(not(unix))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_runs_files_by_their_extension() {
        let exts = ".COM;.EXE;.BAT;.CMD;.PS1";
        assert!(windows_runnable(Path::new(r"C:\bin\code.cmd"), exts));
        assert!(windows_runnable(Path::new("ptyro.EXE"), exts));
        assert!(!windows_runnable(Path::new("notes.txt"), exts));
        assert!(!windows_runnable(Path::new("ptyro"), exts));
        assert_eq!(windows_program_names("code", ".EXE;.CMD"), ["code", "code.exe", "code.cmd"]);
        assert_eq!(windows_program_names("wsl.exe", ".EXE"), ["wsl.exe"]);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_mode_600_on_unix() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        private(OpenOptions::new().write(true).create_new(true)).open(&f).unwrap();
        assert_eq!(mode(&f.metadata().unwrap()), Some(0o600));
        std::fs::write(d.path().join("g"), "").unwrap();
        make_private(&d.path().join("g")).unwrap();
        assert_eq!(mode(&d.path().join("g").metadata().unwrap()), Some(0o600));
        assert!(!is_executable(&f));
        assert!(uid().is_some());
    }
}
