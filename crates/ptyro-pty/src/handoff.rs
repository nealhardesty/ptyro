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

// ── carrying descriptors across `execve` (H2.7) ───────────────────────────────
//
// Clearing close-on-exec on every descriptor an upgrade keeps would let any child spawned meanwhile
// (a git, an editor, a sink's curl: they come from many places) inherit every PTY master, the
// listening socket and the daemon lock. Instead the previous image puts them, by `SCM_RIGHTS`, into
// a socket pair whose receiving end is the only descriptor inherited; the new image drains it and
// [`adopt`] finds each by the number it had before. A child that catches that one end holds
// nothing it can use.

/// Names the receiving end of the socket the previous image put its descriptors in.
pub const ENV_SOCK: &str = "PTYRO_HANDOFF_SOCK";

/// Descriptors per message (Linux takes at most 253).
const BATCH: usize = 32;

/// Received from the previous image, by the number each had there.
static CARRIED: std::sync::Mutex<Option<std::collections::HashMap<RawFd, OwnedFd>>> =
    std::sync::Mutex::new(None);

/// The previous image's side: put `fds` (by number; repeats once) into a new socket pair. Returns
/// the receiving end, inheritable, for the next image (named by [`ENV_SOCK`]); the rest of this
/// process keeps its descriptors close-on-exec.
#[allow(unsafe_code)] // borrowing by number: the caller owns these and keeps them open
pub fn carry(fds: &[RawFd]) -> io::Result<OwnedFd> {
    use rustix::net::{
        AddressFamily, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags,
        SocketType, sendmsg, socketpair,
    };
    let mut fds: Vec<RawFd> = fds.iter().copied().filter(|f| *f >= 0).collect();
    fds.sort_unstable();
    fds.dedup();
    // (macOS has no SOCK_CLOEXEC: both ends are made close-on-exec straight after.)
    let (tx, rx) = socketpair(AddressFamily::UNIX, SocketType::DGRAM, SocketFlags::empty(), None)?;
    private(&tx)?;
    private(&rx)?;
    // Everything waits in the receiver's queue until the next image reads it.
    let room = 1 << 20;
    let _ = rustix::net::sockopt::set_socket_send_buffer_size(&tx, room);
    let _ = rustix::net::sockopt::set_socket_recv_buffer_size(&rx, room);
    for chunk in fds.chunks(BATCH) {
        // SAFETY: the caller owns every descriptor named and keeps it open across this call.
        let borrowed: Vec<std::os::fd::BorrowedFd<'_>> =
            chunk.iter().map(|f| unsafe { std::os::fd::BorrowedFd::borrow_raw(*f) }).collect();
        let numbers: Vec<u8> = chunk.iter().flat_map(|f| f.to_le_bytes()).collect();
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(BATCH))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !control.push(SendAncillaryMessage::ScmRights(&borrowed)) {
            return Err(io::Error::other("descriptors don't fit the control buffer"));
        }
        let sent = sendmsg(&tx, &[io::IoSlice::new(&numbers)], &mut control, SendFlags::empty())?;
        if sent != numbers.len() {
            return Err(io::Error::other("a short send of carried descriptors"));
        }
    }
    inheritable(&rx)?;
    Ok(rx)
}

/// The new image's side, first thing: read every descriptor the previous image carried over
/// (when [`ENV_SOCK`] names the socket). Afterwards [`adopt`] hands them out by their old number.
/// Returns how many arrived.
pub fn receive_carried() -> io::Result<usize> {
    let Some(raw) = std::env::var(ENV_SOCK).ok().and_then(|v| v.parse::<RawFd>().ok()) else {
        return Ok(0);
    };
    let got = drain(adopt_inherited(raw)?)?;
    let n = got.len();
    *CARRIED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(got);
    Ok(n)
}

/// Everything waiting in `sock`: each descriptor by the number it had in the previous image.
fn drain(sock: OwnedFd) -> io::Result<std::collections::HashMap<RawFd, OwnedFd>> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
    rustix::fs::fcntl_setfl(&sock, rustix::fs::OFlags::NONBLOCK)?;
    let mut got = std::collections::HashMap::new();
    loop {
        let mut numbers = [0u8; BATCH * 4];
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(BATCH))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let r = recvmsg(
            &sock,
            &mut [io::IoSliceMut::new(&mut numbers)],
            &mut control,
            RecvFlags::empty(),
        );
        let n = match r {
            Ok(m) if m.bytes > 0 => m.bytes,
            Ok(_) => break,
            // Nothing more; macOS says the sender is gone (it closed at the exec) once the queue
            // is empty.
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::CONNRESET) => break,
            Err(e) => return Err(e.into()),
        };
        let olds: Vec<RawFd> =
            numbers[..n].as_chunks::<4>().0.iter().map(|b| RawFd::from_le_bytes(*b)).collect();
        let mut fds = Vec::new();
        for msg in control.drain() {
            if let RecvAncillaryMessage::ScmRights(it) = msg {
                fds.extend(it);
            }
        }
        if fds.len() != olds.len() {
            return Err(io::Error::other("carried descriptors and their numbers don't match"));
        }
        for (old, fd) in olds.into_iter().zip(fds) {
            let flags = rustix::io::fcntl_getfd(&fd)?;
            rustix::io::fcntl_setfd(&fd, flags | FdFlags::CLOEXEC)?;
            got.insert(old, fd);
        }
    }
    Ok(got)
}

/// Take a carried descriptor by its old number, if one came that way.
fn take_carried(raw: RawFd) -> Option<OwnedFd> {
    CARRIED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut()?.remove(&raw)
}

/// Descriptors were carried (a new-style handoff), so none is inherited by number.
pub fn carried() -> bool {
    CARRIED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

/// Rolling back to an older binary, which inherits by number: put every carried descriptor back
/// at the number it had, inheritable (two steps, so none lands on another's number first).
pub fn restore_carried() -> io::Result<()> {
    let all: Vec<(RawFd, OwnedFd)> = CARRIED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .map(|m| m.into_iter().collect())
        .unwrap_or_default();
    let top = all.iter().map(|(o, _)| *o).max().unwrap_or(0) + 1;
    let high: Vec<(RawFd, OwnedFd)> = all
        .into_iter()
        .map(|(o, fd)| Ok((o, rustix::io::fcntl_dupfd_cloexec(&fd, top)?)))
        .collect::<io::Result<_>>()?;
    for (old, fd) in high {
        let mut target = adopt_number(old);
        rustix::io::dup2(&fd, &mut target)?;
        inheritable(&target)?;
        // The number is the older binary's to adopt now.
        std::mem::forget(target);
    }
    Ok(())
}

#[allow(unsafe_code)] // naming a number we are about to make ours with dup2
fn adopt_number(raw: RawFd) -> OwnedFd {
    // SAFETY: only used as dup2's target, which replaces whatever is (or isn't) there; the
    // result is then deliberately leaked for the next image.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// Take ownership of a descriptor from the previous image, after checking it is open: one it
/// carried over (by its number there), else one inherited by number (the listening socket, or
/// anything from an older ptyro). It is marked close-on-exec again, so it can't leak into
/// children.
pub fn adopt(raw: RawFd) -> io::Result<OwnedFd> {
    if let Some(fd) = take_carried(raw) {
        return Ok(fd);
    }
    adopt_inherited(raw)
}

/// [`adopt`] for a descriptor inherited by number.
#[allow(unsafe_code)] // turning an inherited number into an owned fd is inherently unsafe
fn adopt_inherited(raw: RawFd) -> io::Result<OwnedFd> {
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
    fn descriptors_cross_in_a_socket_and_only_its_end_is_inheritable() {
        use std::io::{Read, Write};
        // More than one batch, the same one twice, and a pipe to prove they are the same files.
        let (r, w) = std::io::pipe().unwrap();
        let (r, w) = (OwnedFd::from(r), OwnedFd::from(w));
        let mut extra: Vec<OwnedFd> =
            (0..BATCH + 3).map(|_| rustix::io::fcntl_dupfd_cloexec(&w, 0).unwrap()).collect();
        let mut numbers: Vec<RawFd> = extra.iter().map(|f| f.as_raw_fd()).collect();
        numbers.push(w.as_raw_fd());
        numbers.push(w.as_raw_fd());
        let rx = carry(&numbers).unwrap();
        assert!(
            !rustix::io::fcntl_getfd(&rx).unwrap().contains(FdFlags::CLOEXEC),
            "the one inherited"
        );
        for f in [&w, &extra[0]] {
            assert!(
                rustix::io::fcntl_getfd(f).unwrap().contains(FdFlags::CLOEXEC),
                "the rest stay private"
            );
        }
        let mut got = drain(rx).unwrap();
        assert_eq!(got.len(), BATCH + 4, "each once");
        let again = got.remove(&w.as_raw_fd()).unwrap();
        assert!(rustix::io::fcntl_getfd(&again).unwrap().contains(FdFlags::CLOEXEC));
        std::fs::File::from(again).write_all(b"across").unwrap();
        drop(w);
        extra.clear();
        drop(got);
        let mut s = String::new();
        std::fs::File::from(r).read_to_string(&mut s).unwrap();
        assert_eq!(s, "across");
    }

    #[test]
    fn a_carried_listening_socket_still_talks() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let raw = l.as_raw_fd();
        let rx = carry(&[raw]).unwrap();
        drop(l);
        let mut got = drain(rx).unwrap();
        let l = std::os::unix::net::UnixListener::from(got.remove(&raw).unwrap());
        let mut c = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let (mut s, _) = l.accept().unwrap();
        c.write_all(b"ping").unwrap();
        let mut b = [0u8; 4];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        s.write_all(b"pong").unwrap();
        c.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"pong");
        // Still open both ways: a second exchange, after the queue was empty.
        s.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
        let mut one = [0u8; 1];
        let idle = s.read(&mut one);
        assert!(idle.is_err(), "nothing to read is a wait, not the end: {idle:?}");
        c.write_all(b"again").unwrap();
        let mut five = [0u8; 5];
        s.read_exact(&mut five).unwrap();
        assert_eq!(&five, b"again");
    }

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
