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
use std::path::{Path, PathBuf};

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

/// Open options that create the file with Unix mode `mode` (an rc file keeps its own); on Windows
/// the folder's access list applies.
pub fn with_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        opts
    }
}

/// Create `path` and any missing parents, each new one only its owner can enter (0700 on Unix).
/// An existing directory is left as it is.
pub fn create_private_dirs(path: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)
}

/// Create one new directory only its owner can enter; it must not exist yet.
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(path)
}

/// Open without following a link at the last step, so a link swapped in after a check is refused,
/// not read: `O_NOFOLLOW` on Unix (and `O_NONBLOCK`, so a FIFO can't hold us),
/// `FILE_FLAG_OPEN_REPARSE_POINT` on Windows (the link itself opens, and isn't a regular file).
/// With `follow_links`, only the FIFO guard.
pub fn no_follow(opts: &mut OpenOptions, follow_links: bool) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let nofollow = if follow_links { 0 } else { rustix::fs::OFlags::NOFOLLOW.bits() as i32 };
        opts.custom_flags(nofollow | rustix::fs::OFlags::NONBLOCK.bits() as i32)
    }
    #[cfg(windows)]
    {
        if !follow_links {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        opts
    }
}

/// Start `cmd` on its own, so it outlives whatever started it and a terminal's signals don't
/// reach it: its own process group on Unix; on Windows a new process group with no console.
pub fn detach(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
    }
}

/// Whether process `pid` is running (and ours to signal). Windows: never asked there, since
/// nothing ptyro starts runs natively yet; `false`.
pub fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
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

/// The null device, as a path a program is given (`core.hooksPath`: nothing is ever found
/// under it): `/dev/null`, and `NUL` on Windows (WIN7).
pub const DEV_NULL: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

/// `path` as it is on disk: [`std::fs::canonicalize`], and on Windows without the `\\?\`
/// prefix it puts in front (`\\?\C:\src` is `C:\src`, `\\?\UNC\nas\x` is `\\nas\x`):
/// what people and other programs write, and what ptyro shows (WIN7). The standard library adds
/// the prefix back itself where a path is too long for the plain form.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(path)?;
    Ok(if cfg!(windows) { plain(&c) } else { c })
}

/// A Windows path without its verbatim prefix (unchanged anywhere else, and when it has none).
pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match s.strip_prefix(r"\\?\") {
        // Only a drive path: `\\?\Volume{…}` has no plain form.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// Whether `path` is `base` or inside it, component by component; on Windows without regard
/// to case (`C:\Src\app` is inside `c:\src`) or to which slash, as Windows reads paths.
pub fn path_starts_with(path: &Path, base: &Path) -> bool {
    if !cfg!(windows) {
        return path.starts_with(base);
    }
    let words = |p: &Path| -> Vec<String> {
        p.to_string_lossy()
            .split(['\\', '/'])
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (p, b) = (words(&plain(path)), words(&plain(base)));
    p.len() >= b.len() && p[..b.len()] == b[..]
}

/// Whether two paths name the same place, as [`path_starts_with`] compares.
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_starts_with(a, b) && path_starts_with(b, a)
}

/// How [`symlink`] made `link`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linked {
    Symlink,
    /// Windows, a folder, without the right to make symbolic links: a junction (`mklink /J`).
    Junction,
    /// Windows, a file, without that right: a hard link (the same file, on the same drive).
    HardLink,
    /// Windows, a file on another drive: a copy (changes to one don't reach the other).
    Copy,
}

impl Linked {
    /// For notes: "linked", "linked (a junction)", "copied".
    pub fn words(self) -> &'static str {
        match self {
            Self::Symlink => "linked",
            Self::Junction => "linked (a junction)",
            Self::HardLink => "linked (a hard link)",
            Self::Copy => "copied (this computer can't link it)",
        }
    }
}

/// A link at `link` to `target`. Unix: a symbolic link. Windows: a symbolic link when this user
/// may make one (Developer Mode, or an administrator); otherwise a junction for a folder, and
/// for a file a hard link, or a copy across drives (WIN7). It says which.
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<Linked> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map(|()| Linked::Symlink)
    }
    #[cfg(windows)]
    {
        // A relative target is relative to the link's own folder, as the link will read it.
        let seen = link.parent().map_or_else(|| target.to_path_buf(), |d| d.join(target));
        let made = if seen.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        };
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        match made {
            Ok(()) => Ok(Linked::Symlink),
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
                if seen.is_dir() {
                    junction(&seen, link).map(|()| Linked::Junction)
                } else if std::fs::hard_link(&seen, link).is_ok() {
                    Ok(Linked::HardLink)
                } else {
                    std::fs::copy(&seen, link).map(|_| Linked::Copy)
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// A junction at `link` to the folder `target` (made absolute: a junction holds a full path),
/// with cmd's `mklink /J`, which needs no privilege.
#[cfg(windows)]
fn junction(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let target = std::path::absolute(target)?;
    let line = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
    let out = std::process::Command::new("cmd.exe")
        .raw_arg(format!("/d /s /c \"{line}\""))
        .stdin(std::process::Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "mklink /J: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The disk a file takes: its allocated blocks on Unix (a sparse file counts what it uses);
/// its length on Windows.
pub fn allocated(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
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

    #[test]
    fn windows_paths_lose_their_verbatim_prefix_and_compare_without_case() {
        assert_eq!(plain(Path::new(r"\\?\C:\src\app")), PathBuf::from(r"C:\src\app"));
        assert_eq!(plain(Path::new(r"\\?\UNC\nas\share")), PathBuf::from(r"\\nas\share"));
        assert_eq!(plain(Path::new(r"\\?\Volume{x}\a")), PathBuf::from(r"\\?\Volume{x}\a"));
        assert_eq!(plain(Path::new("/home/me")), PathBuf::from("/home/me"));
        if cfg!(windows) {
            assert!(path_starts_with(Path::new(r"C:\Src\App\x"), Path::new(r"c:\src\app")));
            assert!(path_starts_with(Path::new("C:/src/app"), Path::new(r"\\?\C:\SRC")));
            assert!(!path_starts_with(Path::new(r"C:\src\application"), Path::new(r"C:\src\app")));
            assert!(same_path(Path::new(r"C:\src\"), Path::new("c:/SRC")));
        } else {
            assert!(path_starts_with(Path::new("/src/app/x"), Path::new("/src/app")));
            assert!(!path_starts_with(Path::new("/Src/app"), Path::new("/src")), "Unix keeps case");
        }
    }

    /// Whatever this user may make (a symbolic link with Developer Mode; else a junction or a
    /// hard link), what's linked is reachable through it, and it says which it made.
    #[cfg(windows)]
    #[test]
    fn a_link_is_made_with_whatever_windows_allows() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("node_modules");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("x.js"), "x").unwrap();
        let how = symlink(&dir, &d.path().join("shared")).unwrap();
        assert!(matches!(how, Linked::Symlink | Linked::Junction), "{how:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("shared").join("x.js")).unwrap(), "x");
        let file = d.path().join(".env");
        std::fs::write(&file, "A=1").unwrap();
        let how = symlink(&file, &d.path().join("env-link")).unwrap();
        assert!(matches!(how, Linked::Symlink | Linked::HardLink), "{how:?}");
        assert_eq!(std::fs::read_to_string(d.path().join("env-link")).unwrap(), "A=1");
    }

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
