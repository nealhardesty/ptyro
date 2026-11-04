//! Where clients connect (SPEC §17): a Unix socket only its user can reach, or on Windows a named
//! pipe only its user can open (WIN5). Both hand `accept` a connection from this user alone, with
//! the client's pid when the platform says it.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

/// After a failed accept (out of descriptors, say), wait before the next: it would fail again
/// at once, and the loop would spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::bail;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tokio::net::UnixListener;

    pub(crate) type Stream = tokio::net::UnixStream;

    pub(crate) struct Listener {
        inner: UnixListener,
        uid: u32,
        /// The socket file's identity, to remove it on the way out only if it is still ours.
        id: Option<(u64, u64)>,
        path: std::path::PathBuf,
    }

    impl Listener {
        pub(crate) fn bind(socket: &Path) -> Result<Self> {
            let inner = bind(socket)?;
            Ok(Self::wrap(inner, socket))
        }

        /// A listening socket carried over by a live upgrade.
        pub(crate) fn inherited(fd: std::os::fd::OwnedFd, socket: &Path) -> Result<Self> {
            let std = std::os::unix::net::UnixListener::from(fd);
            std.set_nonblocking(true)?;
            Ok(Self::wrap(UnixListener::from_std(std)?, socket))
        }

        fn wrap(inner: UnixListener, socket: &Path) -> Self {
            Self {
                inner,
                uid: rustix::process::getuid().as_raw(),
                id: file_id(socket),
                path: socket.to_path_buf(),
            }
        }

        /// The listening descriptor, for a live upgrade to carry.
        pub(crate) fn raw(&self) -> i32 {
            std::os::fd::AsRawFd::as_raw_fd(&self.inner)
        }

        /// The next connection from this user, and its pid.
        pub(crate) async fn accept(&self) -> (Stream, Option<u32>) {
            loop {
                let stream = match self.inner.accept().await {
                    Ok((s, _)) => s,
                    Err(e) => {
                        tracing::warn!("accept failed: {e}");
                        tokio::time::sleep(ACCEPT_PAUSE).await;
                        continue;
                    }
                };
                match stream.peer_cred() {
                    Ok(cred) if cred.uid() == self.uid => {
                        return (stream, cred.pid().and_then(|p| u32::try_from(p).ok()));
                    }
                    Ok(cred) => tracing::warn!(
                        peer_uid = cred.uid(),
                        "rejected connection from another user"
                    ),
                    Err(e) => tracing::warn!("peer credentials unavailable: {e}"),
                }
            }
        }

        /// Stop listening, and remove the socket if it is still the one we made.
        pub(crate) fn close(self) {
            let Self { inner, id, path, .. } = self;
            drop(inner);
            if id.is_some() && file_id(&path) == id {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// The socket's directory may live in a shared /tmp: insist it is ours and private.
    pub(crate) fn prepare(socket: &Path) -> Result<()> {
        let dir = socket.parent().context("socket path has no parent")?;
        ptyro_core::os::create_private_dirs(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        let meta = std::fs::metadata(dir)?;
        let uid = rustix::process::getuid().as_raw();
        if meta.uid() != uid {
            bail!(
                "{} is owned by uid {}, not us ({uid}); refusing to use it",
                dir.display(),
                meta.uid()
            );
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn file_id(path: &Path) -> Option<(u64, u64)> {
        std::fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }

    pub(crate) fn bind(socket: &Path) -> Result<UnixListener> {
        if socket.exists() {
            // Only a refused connection proves nobody listens. Any other failure (a sandbox that
            // forbids connecting, say) leaves the socket alone: it may be another daemon's.
            match std::os::unix::net::UnixStream::connect(socket) {
                Ok(_) => bail!("{} is in use by another process", socket.display()),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(socket)
                        .with_context(|| format!("remove stale {}", socket.display()))?;
                }
                Err(e) => bail!(
                    "{} exists and can't be checked ({e}); not replacing it (another daemon may use it)",
                    socket.display()
                ),
            }
        }
        let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
        let listener = UnixListener::bind(socket);
        rustix::process::umask(old);
        let listener = listener.with_context(|| format!("bind {}", socket.display()))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use tokio::net::windows::named_pipe::NamedPipeServer;

    pub(crate) type Stream = NamedPipeServer;

    /// A named pipe: one instance waits for the next client, and a new one takes its place as
    /// soon as one connects (pipes have no backlog: no instance means "nobody listens").
    pub(crate) struct Listener {
        name: String,
        next: tokio::sync::Mutex<NamedPipeServer>,
    }

    impl Listener {
        pub(crate) fn bind(socket: &Path) -> Result<Self> {
            let name = socket.to_string_lossy().into_owned();
            let first = ptyro_pty::pipe::server(&name, true).with_context(|| {
                format!("{name} is in use by another process (another ptyro daemon?)")
            })?;
            Ok(Self { name, next: tokio::sync::Mutex::new(first) })
        }

        /// Nothing to carry on Windows until its live upgrade (WIN9).
        pub(crate) fn raw(&self) -> i32 {
            -1
        }

        /// The next connection (from this user: the pipe admits no one else), and its pid.
        pub(crate) async fn accept(&self) -> (Stream, Option<u32>) {
            let mut next = self.next.lock().await;
            loop {
                if let Err(e) = next.connect().await {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(ACCEPT_PAUSE).await;
                    continue;
                }
                match ptyro_pty::pipe::server(&self.name, false) {
                    Ok(fresh) => {
                        let stream = std::mem::replace(&mut *next, fresh);
                        let pid = ptyro_pty::pipe::client_pid(&stream);
                        return (stream, pid);
                    }
                    Err(e) => {
                        // Without a next instance this one has to go back to waiting.
                        tracing::warn!("can't make the pipe's next instance: {e}");
                        let _ = next.disconnect();
                        tokio::time::sleep(ACCEPT_PAUSE).await;
                    }
                }
            }
        }

        pub(crate) fn close(self) {}
    }

    /// A pipe has no directory to prepare.
    pub(crate) fn prepare(_socket: &Path) -> Result<()> {
        Ok(())
    }
}
