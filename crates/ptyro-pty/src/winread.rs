//! Reading a Windows pipe that a live upgrade can stop without losing a byte (WIN9, SPEC §4.5).
//!
//! An anonymous pipe can't be overlapped, so a read blocks a thread. Through tokio's blocking pool
//! (`tokio::fs::File`) a read that's under way when its future is dropped carries on regardless:
//! what it then reads lands in a buffer the next owner never sees, and a handover lost a chunk
//! that way (WIN0.3, SPEC §20.13). So each pipe gets a thread of its own here, and what it reads
//! goes to the async side over a channel, where dropping a future loses nothing. A handover
//! [`pause`](PipeReader::pause)s the thread (its read is cancelled, `CancelSynchronousIo`, and it
//! starts no other) and [`take_pending`](PipeReader::take_pending)s what it read and nobody took
//! yet; the next daemon reads the pipe from there.
#![allow(unsafe_code)] // Win32 calls; each block says what it relies on.

use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use ptyro_core::sync::MutexExt;
use tokio::sync::mpsc;
use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_OPERATION_ABORTED, HANDLE};
use windows_sys::Win32::System::IO::CancelSynchronousIo;

/// How long a pause waits for the thread to stop reading.
const PAUSE_WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
struct State {
    /// Asked to stop reading (a handover).
    pause: bool,
    /// Not in a read now, and starts none while `pause` holds.
    idle: bool,
    /// The pipe ended (or failed): the thread is gone.
    done: bool,
}

/// What the reading thread and its reader share.
struct Ctl {
    state: Mutex<State>,
    cv: Condvar,
    /// The thread, for `CancelSynchronousIo` (a std thread handle has the access it needs).
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The pipe (the thread owns it), by value, for a handover to copy.
    handle: usize,
}

/// A pipe's read end, read on a thread of its own: `AsyncRead`, and stoppable.
pub struct PipeReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    /// The rest of a chunk a read didn't have room for.
    rest: Vec<u8>,
    ctl: Arc<Ctl>,
}

impl std::fmt::Debug for PipeReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PipeReader")
    }
}

impl PipeReader {
    /// Read `file` (a pipe's read end, ours alone from here) on a thread named `name`.
    pub fn new(file: std::fs::File, name: &str) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        let handle = file.as_raw_handle() as usize;
        let ctl = Arc::new(Ctl {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            thread: Mutex::new(None),
            handle,
        });
        let c = Arc::clone(&ctl);
        let t = std::thread::Builder::new().name(name.into()).spawn(move || run(file, &c, &tx))?;
        *ctl.thread.locked() = Some(t);
        Ok(Self { rx, rest: Vec::new(), ctl })
    }

    /// The pipe's handle in this process (the thread holds it open), for a handover to copy.
    pub fn raw(&self) -> HANDLE {
        self.ctl.handle as HANDLE
    }

    /// Stop reading: a read under way is cancelled, and no other starts until [`resume`]. What
    /// was read is kept for [`take_pending`]. Blocks up to a few seconds; an error if the thread
    /// didn't stop, so a handover is called off rather than lose output.
    ///
    /// [`resume`]: Self::resume
    /// [`take_pending`]: Self::take_pending
    pub fn pause(&self) -> io::Result<()> {
        let deadline = Instant::now() + PAUSE_WAIT;
        self.ctl.state.locked().pause = true;
        loop {
            {
                let s = self.ctl.state.locked();
                if s.idle || s.done {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "a pipe's reader didn't stop for the handover",
                ));
            }
            // A read the thread started just after an earlier cancel isn't cancelled by it:
            // ask again until the thread says it's idle.
            if let Some(t) = self.ctl.thread.locked().as_ref() {
                // SAFETY: the thread's handle is open while its JoinHandle is held; cancelling
                // when it isn't reading does nothing.
                unsafe { CancelSynchronousIo(t.as_raw_handle() as HANDLE) };
            }
            let s = self.ctl.state.locked();
            let _ = self.ctl.cv.wait_timeout(s, Duration::from_millis(20));
        }
    }

    /// Read again after a [`pause`](Self::pause) (a handover that was called off).
    pub fn resume(&self) {
        let mut s = self.ctl.state.locked();
        s.pause = false;
        drop(s);
        self.ctl.cv.notify_all();
    }

    /// What was read and not yet taken: a paused reader's last bytes, for the emulator before
    /// the handover's image is made.
    pub fn take_pending(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.rest);
        while let Ok(Ok(chunk)) = self.rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }
}

/// The thread: read until the pipe ends, stopping while paused.
fn run(mut file: std::fs::File, ctl: &Ctl, tx: &mpsc::Sender<io::Result<Vec<u8>>>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        {
            let mut s = ctl.state.locked();
            while s.pause {
                s.idle = true;
                ctl.cv.notify_all();
                s = ctl.cv.wait(s).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            s.idle = false;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            // Cancelled by a pause: loop round, where the pause holds us.
            Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {}
            // The writer closed its end (the console host, the child): the output is over.
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => break,
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                break;
            }
        }
    }
    let mut s = ctl.state.locked();
    s.done = true;
    s.idle = true;
    drop(s);
    ctl.cv.notify_all();
}

impl tokio::io::AsyncRead for PipeReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.rest.is_empty() {
            match self.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                // The thread ended: the output is over.
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(Some(Ok(chunk))) => self.rest = chunk,
            }
        }
        let n = self.rest.len().min(buf.remaining());
        buf.put_slice(&self.rest[..n]);
        self.rest.drain(..n);
        Poll::Ready(Ok(()))
    }
}

/// A pipe's read end a handover gave this process, by its value (checked to be an open pipe).
pub fn adopt(h: usize, name: &str) -> io::Result<PipeReader> {
    PipeReader::new(crate::windows::adopt_pipe(h)?, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    use tokio::io::AsyncReadExt;

    fn pipe() -> (std::fs::File, std::fs::File) {
        let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: both out-parameters are written on success.
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Pipes::CreatePipe(&mut r, &mut w, std::ptr::null(), 0)
            },
            0
        );
        // SAFETY: each handle is ours alone, given to one File.
        unsafe { (std::fs::File::from_raw_handle(r), std::fs::File::from_raw_handle(w)) }
    }

    /// Paused while it waits in a read, it stops; what came before is taken, nothing after is
    /// read (it stays in the pipe for the next owner); resumed, it reads on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_paused_reader_loses_nothing() {
        let (r, mut w) = pipe();
        let mut reader = PipeReader::new(r, "test-read").unwrap();
        w.write_all(b"before ").unwrap();
        let mut got = vec![0u8; 7];
        reader.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"before ");
        // Now it waits in a read: the pause cancels it.
        w.write_all(b"taken").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        tokio::task::block_in_place(|| reader.pause()).unwrap();
        assert_eq!(reader.take_pending(), b"taken");
        w.write_all(b"kept in the pipe").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(reader.take_pending().is_empty(), "nothing is read while paused");
        reader.resume();
        let mut rest = vec![0u8; 16];
        reader.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"kept in the pipe");
        drop(w);
        let mut end = Vec::new();
        reader.read_to_end(&mut end).await.unwrap();
        assert!(end.is_empty(), "the pipe's end is EOF");
    }

    /// Pausing a reader whose pipe is idle (blocked in a read with nothing to come) returns
    /// at once with nothing pending.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_reader_pauses_at_once() {
        let (r, _w) = pipe();
        let mut reader = PipeReader::new(r, "test-idle").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        tokio::task::block_in_place(|| reader.pause()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert!(reader.take_pending().is_empty());
    }
}
