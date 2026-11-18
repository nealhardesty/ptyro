//! Stand-in programs on `PATH` for tests (WIN13): a fake `gh`, `claude`, `ssh` or `tailscale`
//! that runs something else, as a shell script.

use std::path::{Path, PathBuf};

/// A program `name` in `dir`: `script`, the body of a `#!/bin/sh` script.
pub fn program(dir: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `PATH` with `dir` first.
pub fn path_with(dir: &Path) -> String {
    let rest = std::env::var_os("PATH").unwrap_or_default();
    let all = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&rest));
    std::env::join_paths(all).unwrap().to_string_lossy().into_owned()
}
