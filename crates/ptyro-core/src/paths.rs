//! File-system layout (SPEC §14.1): XDG on Linux *and* macOS; `PTYRO_HOME` puts everything under
//! one directory (tests, portable installs); `PTYRO_SOCKET` overrides only the socket.

use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;

/// `sun_path` is 104 bytes on macOS (108 on Linux); keep a margin for the NUL and safety.
pub const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub runtime: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    /// Resolve from the process environment.
    pub fn from_env() -> std::io::Result<Self> {
        Self::resolve(|k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from))
    }

    /// Resolve with an explicit variable lookup (testable without touching the real environment).
    pub fn resolve(var: impl Fn(&str) -> Option<PathBuf>) -> std::io::Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        let mut paths = if let Some(home) = var("PTYRO_HOME").map(absolute) {
            Self {
                config: home.join("config"),
                data: home.join("data"),
                state: home.join("state"),
                runtime: home.join("run"),
                socket: PathBuf::new(),
            }
        } else {
            let xdg = etcetera::base_strategy::Xdg::new().map_err(std::io::Error::other)?;
            let runtime = match var("XDG_RUNTIME_DIR") {
                Some(dir) => dir.join("ptyro"),
                None => var("TMPDIR")
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join(format!("ptyro-{uid}")),
            };
            Self {
                config: xdg.config_dir().join("ptyro"),
                data: xdg.data_dir().join("ptyro"),
                state: xdg.state_dir().unwrap_or_else(|| xdg.data_dir()).join("ptyro"),
                runtime,
                socket: PathBuf::new(),
            }
        };
        paths.socket = match var("PTYRO_SOCKET").map(absolute) {
            Some(s) if s.as_os_str().len() > MAX_SOCKET_PATH + 3 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "PTYRO_SOCKET is {} bytes, more than a socket path can be ({} at most): use a shorter one",
                        s.as_os_str().len(),
                        MAX_SOCKET_PATH + 3
                    ),
                ));
            }
            Some(s) => s,
            None => short_enough(paths.runtime.join("ptyro.sock"), uid),
        };
        Ok(paths)
    }

    pub fn lock_file(&self) -> PathBuf {
        self.state.join("daemon.lock")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state.clone()
    }
}

/// The user this process runs as.
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Relative to where this process is now, as an absolute path: the daemon detaches into its own
/// directory, so a relative `PTYRO_HOME` would otherwise mean two places.
fn absolute(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

/// A socket path that fits `sun_path`. Deep directories (macOS `$TMPDIR`, test temp dirs) fall
/// back to `/tmp/ptyro-<uid>-<hash>/ptyro.sock`, derived deterministically from the original path
/// so every client finds the same socket.
fn short_enough(sock: PathBuf, uid: u32) -> PathBuf {
    if sock.as_os_str().len() <= MAX_SOCKET_PATH {
        return sock;
    }
    let hash = fnv1a(sock.as_os_str().as_encoded_bytes());
    Path::new("/tmp").join(format!("ptyro-{uid}-{hash:08x}")).join("ptyro.sock")
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptyro_home_puts_everything_under_one_dir() {
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("/h"))).unwrap();
        assert_eq!(p.config, PathBuf::from("/h/config"));
        assert_eq!(p.socket, PathBuf::from("/h/run/ptyro.sock"));
    }

    #[test]
    fn long_socket_paths_fall_back_to_tmp() {
        let deep = PathBuf::from("/x".repeat(80));
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| deep.clone())).unwrap();
        assert!(p.socket.starts_with("/tmp"));
        assert!(p.socket.as_os_str().len() <= MAX_SOCKET_PATH);
        let again = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| deep.clone())).unwrap();
        assert_eq!(p.socket, again.socket);
    }

    #[test]
    fn a_relative_home_is_made_absolute_and_a_too_long_socket_is_refused() {
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("rel/home"))).unwrap();
        assert!(p.config.is_absolute() && p.socket.is_absolute(), "{p:?}");
        let long = PathBuf::from(format!("/{}", "s".repeat(120)));
        let e = Paths::resolve(|k| (k == "PTYRO_SOCKET").then(|| long.clone())).unwrap_err();
        assert!(e.to_string().contains("PTYRO_SOCKET"), "{e}");
    }

    #[test]
    fn socket_override_wins() {
        let p = Paths::resolve(|k| match k {
            "PTYRO_HOME" => Some(PathBuf::from("/h")),
            "PTYRO_SOCKET" => Some(PathBuf::from("/s.sock")),
            _ => None,
        })
        .unwrap();
        assert_eq!(p.socket, PathBuf::from("/s.sock"));
    }
}
