//! File-system layout (SPEC §14.1): XDG on Linux *and* macOS; on Windows, the config under
//! `%APPDATA%\ptyro` and the rest under `%LOCALAPPDATA%\ptyro` (WIN3), unless the XDG variables
//! are set, which win there too (WIN4); `PTYRO_HOME` puts
//! everything under one directory (tests, portable installs); `PTYRO_SOCKET` overrides only the
//! socket, and under a `PTYRO_HOME` only with one of that home's.

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
    /// [`Paths::tools`].
    pub tools: PathBuf,
}

impl Paths {
    /// Resolve from the process environment.
    pub fn from_env() -> std::io::Result<Self> {
        Self::resolve(|k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from))
    }

    /// Resolve with an explicit variable lookup (testable without touching the real environment).
    pub fn resolve(var: impl Fn(&str) -> Option<PathBuf>) -> std::io::Result<Self> {
        let uid = crate::os::uid().unwrap_or(0);
        let home = var("PTYRO_HOME").map(absolute);
        let mut paths = if let Some(home) = &home {
            Self {
                config: home.join("config"),
                data: home.join("data"),
                state: home.join("state"),
                runtime: home.join("run"),
                socket: PathBuf::new(),
                tools: home.join("ptyro-tools"),
            }
        } else if cfg!(windows) {
            // Windows' own places: roaming config, the rest local to this computer, unless an
            // XDG variable names one (a user who keeps one layout across machines, Git Bash's).
            // The daemon's "socket" is a named pipe, set below.
            let w = etcetera::base_strategy::Windows::new().map_err(std::io::Error::other)?;
            let local = w.cache_dir().join("ptyro");
            let xdg = |k: &str| var(k).filter(|p| p.is_absolute()).map(|p| p.join("ptyro"));
            Self {
                config: xdg("XDG_CONFIG_HOME").unwrap_or_else(|| w.config_dir().join("ptyro")),
                data: xdg("XDG_DATA_HOME").unwrap_or_else(|| local.join("data")),
                state: xdg("XDG_STATE_HOME").unwrap_or_else(|| local.join("state")),
                runtime: xdg("XDG_RUNTIME_DIR").unwrap_or_else(|| local.join("run")),
                socket: PathBuf::new(),
                tools: var("XDG_DATA_HOME")
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| w.cache_dir())
                    .join("ptyro-tools"),
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
                tools: xdg.data_dir().join("ptyro-tools"),
            }
        };
        // Every session's environment carries its daemon's `PTYRO_SOCKET`. A `PTYRO_HOME` set on
        // top means another ptyro, so a socket that isn't that home's is the outer daemon's and
        // is ignored: a sandbox's `daemon stop` must never reach the daemon it runs in.
        let default = if cfg!(windows) {
            pipe_for(&paths.runtime)
        } else {
            short_enough(paths.runtime.join("ptyro.sock"), uid)
        };
        let given = var("PTYRO_SOCKET").filter(|s| match &home {
            None => true,
            Some(h) => {
                let s = absolute(s.clone());
                s == default || (!cfg!(windows) && s.starts_with(h))
            }
        });
        if cfg!(windows) {
            paths.socket = match given {
                Some(p) if p.to_string_lossy().starts_with(PIPE_PREFIX) => p,
                Some(p) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "PTYRO_SOCKET is {}, but on Windows it names a pipe: {PIPE_PREFIX}<name>",
                            p.display()
                        ),
                    ));
                }
                None => default,
            };
            return Ok(paths);
        }
        paths.socket = match given.map(absolute) {
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
            None => default,
        };
        Ok(paths)
    }

    pub fn lock_file(&self) -> PathBuf {
        self.state.join("daemon.lock")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state.clone()
    }

    /// Where `ptyro setup` puts the tools it installs without admin rights (BOOT1, SPEC §24.2):
    /// nodejs.org's Node.js and an npm prefix. Beside the data directory, not in it, because the
    /// tools are the user's and `ptyro uninstall` leaves them: `~/.local/share/ptyro-tools`,
    /// `%LOCALAPPDATA%\ptyro-tools`, `$PTYRO_HOME/ptyro-tools`.
    pub fn tools(&self) -> PathBuf {
        self.tools.clone()
    }
}

/// Where every Windows named pipe's name starts.
pub const PIPE_PREFIX: &str = r"\\.\pipe\";

/// The daemon's pipe on Windows (WIN5): one per user and per `PTYRO_HOME`, named after the
/// runtime folder (under the user's own profile), so two users or two homes never meet. The
/// pipe admits only its user, and a client checks who serves it (`ptyro_pty::pipe`).
fn pipe_for(runtime: &Path) -> PathBuf {
    let key = runtime.to_string_lossy().to_lowercase();
    PathBuf::from(format!("{PIPE_PREFIX}ptyro-{:08x}", fnv1a(key.as_bytes())))
}

/// Whether `socket` names a Windows pipe rather than a file.
pub fn is_pipe(socket: &Path) -> bool {
    socket.to_string_lossy().starts_with(PIPE_PREFIX)
}

/// The user this process runs as (0 on Windows, which has no uids: `crate::os::uid`).
pub fn current_uid() -> u32 {
    crate::os::uid().unwrap_or(0)
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

    #[cfg(windows)]
    #[test]
    fn windows_keeps_the_config_roaming_and_the_rest_local() {
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from(r"C:\h"))).unwrap();
        assert_eq!(p.config, PathBuf::from(r"C:\h\config"));
        let p = Paths::resolve(|_| None).unwrap();
        let roaming = p.config.to_string_lossy().to_ascii_lowercase();
        assert!(roaming.ends_with(r"appdata\roaming\ptyro"), "{roaming}");
        let local = p.data.to_string_lossy().to_ascii_lowercase();
        assert!(local.ends_with(r"appdata\local\ptyro\data"), "{local}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_a_pipe_per_user_and_home() {
        let plain = Paths::resolve(|_| None).unwrap();
        assert!(is_pipe(&plain.socket), "{}", plain.socket.display());
        let home = |h: &str| {
            let h = PathBuf::from(h);
            Paths::resolve(move |k| (k == "PTYRO_HOME").then(|| h.clone())).unwrap().socket
        };
        assert_ne!(home(r"C:\a"), home(r"C:\b"));
        assert_eq!(home(r"C:\a"), home(r"C:\A"), "Windows paths ignore case");
        let given =
            Paths::resolve(|k| (k == "PTYRO_SOCKET").then(|| PathBuf::from(r"\\.\pipe\mine")))
                .unwrap();
        assert_eq!(given.socket, PathBuf::from(r"\\.\pipe\mine"));
        let e = Paths::resolve(|k| (k == "PTYRO_SOCKET").then(|| PathBuf::from(r"C:\x.sock")))
            .unwrap_err();
        assert!(e.to_string().contains("names a pipe"), "{e}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_takes_the_xdg_variables_when_they_are_set() {
        let p = Paths::resolve(|k| match k {
            "XDG_CONFIG_HOME" => Some(PathBuf::from(r"D:\cfg")),
            "XDG_STATE_HOME" => Some(PathBuf::from(r"D:\state")),
            "XDG_DATA_HOME" => Some(PathBuf::from("relative")),
            _ => None,
        })
        .unwrap();
        assert_eq!(p.config, PathBuf::from(r"D:\cfg\ptyro"));
        assert_eq!(p.state, PathBuf::from(r"D:\state\ptyro"));
        let data = p.data.to_string_lossy().to_ascii_lowercase();
        assert!(data.ends_with(r"appdata\local\ptyro\data"), "a relative one is ignored: {data}");
    }

    #[cfg(unix)] // Unix paths
    #[test]
    fn ptyro_home_puts_everything_under_one_dir() {
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("/h"))).unwrap();
        assert_eq!(p.config, PathBuf::from("/h/config"));
        assert_eq!(p.socket, PathBuf::from("/h/run/ptyro.sock"));
        // Setup's tools sit beside the data, inside the home (BOOT1).
        assert_eq!(p.tools(), PathBuf::from("/h/ptyro-tools"));
        // Even when the home is itself named ptyro: never outside it.
        let p = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("/t/ptyro"))).unwrap();
        assert_eq!(p.tools(), PathBuf::from("/t/ptyro/ptyro-tools"));
    }

    #[cfg(unix)] // a socket's path; Windows names a pipe
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

    #[cfg(unix)] // Unix paths
    #[test]
    fn socket_override_wins() {
        let p =
            Paths::resolve(|k| (k == "PTYRO_SOCKET").then(|| PathBuf::from("/s.sock"))).unwrap();
        assert_eq!(p.socket, PathBuf::from("/s.sock"));
        // Inside the home: still the home's ptyro, so it wins there too.
        let p = Paths::resolve(|k| match k {
            "PTYRO_HOME" => Some(PathBuf::from("/h")),
            "PTYRO_SOCKET" => Some(PathBuf::from("/h/other.sock")),
            _ => None,
        })
        .unwrap();
        assert_eq!(p.socket, PathBuf::from("/h/other.sock"));
    }

    #[cfg(unix)] // Unix paths
    #[test]
    fn a_home_ignores_the_outer_sessions_socket() {
        // An agent in a ptyro session sets PTYRO_HOME to sandbox itself; the session's
        // PTYRO_SOCKET is still in its environment. `daemon stop` there once stopped the real
        // daemon (2026-11-09).
        let p = Paths::resolve(|k| match k {
            "PTYRO_HOME" => Some(PathBuf::from("/h")),
            "PTYRO_SOCKET" => Some(PathBuf::from("/tmp/ptyro-501/ptyro.sock")),
            _ => None,
        })
        .unwrap();
        assert_eq!(p.socket, PathBuf::from("/h/run/ptyro.sock"));
        // A deep home's socket lives in /tmp: its own daemon's sessions still find it.
        let deep = PathBuf::from("/x".repeat(80));
        let own = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| deep.clone())).unwrap().socket;
        let p = Paths::resolve(|k| match k {
            "PTYRO_HOME" => Some(deep.clone()),
            "PTYRO_SOCKET" => Some(own.clone()),
            _ => None,
        })
        .unwrap();
        assert_eq!(p.socket, own);
    }
}
