//! Live upgrade plumbing (SPEC §4.5, M8.1): keep file descriptors across `execve`, adopt them in
//! the new image, carry the handoff image in an anonymous file, and reap children that tokio
//! didn't spawn (after the re-exec, ours still, but unknown to the new runtime).

use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

use pty_process::{OwnedReadPty, OwnedWritePty};
use rustix::io::FdFlags;

/// Let `fd` survive `execve` (clear `FD_CLOEXEC`); returns its number for the image.
pub fn inheritable(fd: impl AsFd) -> io::Result<RawFd> {
    let flags = rustix::io::fcntl_getfd(&fd)?;
    rustix::io::fcntl_setfd(&fd, flags - FdFlags::CLOEXEC)?;
    Ok(fd.as_fd().as_raw_fd())
}

/// [`inheritable`] by number, for a descriptor owned elsewhere (the daemon's listening socket).
#[allow(unsafe_code)] // borrowing by number; the caller vouches the fd stays open meanwhile
pub fn inheritable_raw(raw: RawFd) -> io::Result<RawFd> {
    // SAFETY: only fcntl flags change; the fd is owned (and kept open) by the caller.
    inheritable(unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) })
}

/// Undo [`inheritable`]: close-on-exec again (an upgrade that was called off).
pub fn private(fd: impl AsFd) -> io::Result<()> {
    let flags = rustix::io::fcntl_getfd(&fd)?;
    rustix::io::fcntl_setfd(&fd, flags | FdFlags::CLOEXEC)?;
    Ok(())
}

/// [`private`] by number.
#[allow(unsafe_code)] // borrowing by number; the caller vouches the fd stays open meanwhile
pub fn private_raw(raw: RawFd) -> io::Result<()> {
    // SAFETY: only fcntl flags change; the fd is owned (and kept open) by the caller.
    private(unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) })
}

/// Take ownership of an inherited descriptor, after checking it is open. It is marked
/// close-on-exec again, so it can't leak into children.
#[allow(unsafe_code)] // turning an inherited number into an owned fd is inherently unsafe
pub fn adopt(raw: RawFd) -> io::Result<OwnedFd> {
    if raw < 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative fd"));
    }
    // SAFETY: fcntl(F_GETFD) on a raw number only queries it; EBADF tells us it isn't open.
    let flags = rustix::io::fcntl_getfd(unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) })?;
    // SAFETY: the fd is open (checked above), was handed to this process by the previous image
    // for exactly this purpose, and nothing else in this process owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    rustix::io::fcntl_setfd(&fd, flags | FdFlags::CLOEXEC)?;
    Ok(fd)
}

/// A PTY master inherited from the previous image: its async read and write halves, plus a
/// duplicate kept for probes and the next handoff.
#[allow(unsafe_code)] // pty_process::Pty::from_fd is unsafe: it trusts the fd to be a PTY master
pub fn adopt_pty(master: OwnedFd) -> io::Result<(OwnedReadPty, OwnedWritePty, OwnedFd)> {
    let dup = rustix::io::fcntl_dupfd_cloexec(&master, 0)?;
    // A master is a tty: tcgetpgrp answers (or ENOTTY says it isn't one).
    rustix::termios::tcgetattr(&master)?;
    // SAFETY: `master` is a PTY master we own (checked to be a tty), used by nothing else.
    let pty = unsafe { pty_process::Pty::from_fd(master) }.map_err(io::Error::other)?;
    let (r, w) = pty.into_split();
    Ok((r, w, dup))
}

/// An anonymous file holding `bytes`, rewound, close-on-exec cleared by the caller as needed:
/// a memfd on Linux, an unlinked file in `dir` elsewhere.
pub fn image_file(dir: &Path, bytes: &[u8]) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let fd = {
        let _ = dir;
        rustix::fs::memfd_create("ptyro-handoff", rustix::fs::MemfdFlags::CLOEXEC)?
    };
    #[cfg(not(target_os = "linux"))]
    let fd = {
        use std::os::unix::fs::OpenOptionsExt;
        let path = dir.join(format!("handoff-{}.img", std::process::id()));
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        std::fs::remove_file(&path)?;
        OwnedFd::from(f)
    };
    let mut f = std::fs::File::from(fd);
    f.write_all(bytes)?;
    f.rewind()?;
    Ok(OwnedFd::from(f))
}

/// Everything in an inherited image file, from the start.
pub fn read_image(fd: OwnedFd) -> io::Result<Vec<u8>> {
    let mut f = std::fs::File::from(fd);
    f.rewind()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v)?;
    Ok(v)
}

/// How a reaped child ended: `(exit code, terminating signal)`.
pub type Ended = (Option<i32>, Option<i32>);

/// Reap `pid` if it has exited (never blocks). `Err` when it isn't our child any more.
pub fn try_reap(pid: u32) -> io::Result<Option<Ended>> {
    let p = rustix::process::Pid::from_raw(pid as i32)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad pid"))?;
    match rustix::process::waitpid(Some(p), rustix::process::WaitOptions::NOHANG)? {
        None => Ok(None),
        Some((_, st)) => Ok(Some((st.exit_status(), st.terminating_signal()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_files_and_fd_adoption() {
        let dir = std::env::temp_dir();
        let fd = image_file(&dir, b"hello image").unwrap();
        let raw = inheritable(&fd).unwrap();
        assert!(!rustix::io::fcntl_getfd(&fd).unwrap().contains(FdFlags::CLOEXEC));
        std::mem::forget(fd); // ownership passes to `adopt`, as across an exec
        let back = adopt(raw).unwrap();
        assert!(rustix::io::fcntl_getfd(&back).unwrap().contains(FdFlags::CLOEXEC));
        assert_eq!(read_image(back).unwrap(), b"hello image");
        assert!(adopt(987_654).is_err(), "a closed number is refused");
    }

    #[tokio::test]
    async fn an_adopted_pty_keeps_working_and_its_child_is_reaped() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let argv: Vec<String> =
            ["sh", "-c", "read x; echo got:$x; exit 3"].map(String::from).into();
        let c = crate::spawn(crate::SpawnRequest {
            argv: &argv,
            cwd: Path::new("/"),
            env: crate::child_env(std::env::vars_os(), []),
            size: ptyro_core::TermSize::new(40, 5),
        })
        .unwrap();
        let pid = c.pid;
        // Hand the master over as a new image would; the old halves go away.
        let master = c.master;
        std::mem::forget(c.child); // tokio must not reap it: the "new image" does
        drop(c.reader);
        drop(c.writer);
        let (mut r, mut w, _dup) = adopt_pty(master).unwrap();
        w.write_all(b"hi\n").await.unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = r.read(&mut buf).await {
            out.extend_from_slice(&buf[..n]);
            if String::from_utf8_lossy(&out).contains("got:hi") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&out).contains("got:hi"));
        let mut ended = None;
        for _ in 0..100 {
            if let Some(e) = try_reap(pid).unwrap() {
                ended = Some(e);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        assert_eq!(ended, Some((Some(3), None)));
    }
}
