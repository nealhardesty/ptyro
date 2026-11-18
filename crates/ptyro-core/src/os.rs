//! ptyro's own files and processes, in one place, so the rest of the code says what it means ("a
//! file only its owner reads") rather than how: mode 0600 for a private file, the execute bits for
//! a program, the uid for who we are.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// Variables a client may pass along with a spawn so sessions see the *client's* SSH agent and
/// display, like tmux's `update-environment` (SPEC §5.1).
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

/// Open options that create the file with Unix mode `mode` (an rc file keeps its own).
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
/// not read: `O_NOFOLLOW` (and `O_NONBLOCK`, so a FIFO can't hold us). With `follow_links`, only
/// the FIFO guard.
pub fn no_follow(opts: &mut OpenOptions, follow_links: bool) -> &mut OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let nofollow = if follow_links { 0 } else { rustix::fs::OFlags::NOFOLLOW.bits() as i32 };
    opts.custom_flags(nofollow | rustix::fs::OFlags::NONBLOCK.bits() as i32)
}

/// The Tailscale CLI (TS1): `tailscale` on `PATH`.
pub fn tailscale() -> std::process::Command {
    std::process::Command::new("tailscale")
}

/// Start `cmd` on its own, so it outlives whatever started it and a terminal's signals don't
/// reach it: its own process group.
pub fn detach(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
}

/// Whether process `pid` is running (and ours to signal).
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

/// Make an existing file readable by its owner only (`chmod 600`).
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
/// under it).
pub const DEV_NULL: &str = "/dev/null";

/// `path` as it is on disk: [`std::fs::canonicalize`].
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// Whether `path` is `base` or inside it, component by component.
pub fn path_starts_with(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

/// Whether an address someone typed is a path rather than a name: `/…`, `./…`, `~/…` (a name
/// never looks like one).
pub fn looks_like_path(s: &str) -> bool {
    s.starts_with(['/', '.', '~'])
}

/// Whether two paths name the same place, as [`path_starts_with`] compares.
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_starts_with(a, b) && path_starts_with(b, a)
}

/// The disk a file takes: its allocated blocks on Unix (a sparse file counts what it uses).
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
/// it".
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
    false
}

/// The names a program `prog` may have on disk: itself.
pub fn program_names(prog: &str) -> Vec<String> {
    vec![prog.to_owned()]
}

/// This process's uid.
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
    fn paths_typed_are_paths() {
        for p in ["/srv/app", "./app", "../app", "~/app"] {
            assert!(looks_like_path(p), "{p}");
        }
        for n in ["app", "my-repo", "p:1a2b", "C:", "c:app"] {
            assert!(!looks_like_path(n), "{n}");
        }
    }

    #[test]
    fn paths_compare_component_by_component_and_keep_case() {
        assert!(path_starts_with(Path::new("/src/app/x"), Path::new("/src/app")));
        assert!(!path_starts_with(Path::new("/src/application"), Path::new("/src/app")));
        assert!(!path_starts_with(Path::new("/Src/app"), Path::new("/src")), "Unix keeps case");
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
