//! Windows: the seam for ConPTY sessions and Job Objects (WIN5). Until then nothing can be
//! spawned, so the PTY types have no values and every call that needs a session says so.
//! What doesn't need one (a process group for a command, exit codes) works already.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use ptyro_core::TermSize;

use crate::{PtyChild, SpawnRequest};

/// A signal, by its POSIX number (what a client sends). WIN5 maps each to Windows: Ctrl+C,
/// closing the console, terminating the Job Object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signal(i32);

impl Signal {
    pub const HUP: Self = Self(1);
    pub const INT: Self = Self(2);
    pub const KILL: Self = Self(9);
    pub const TERM: Self = Self(15);
    pub const CONT: Self = Self(18);
    pub const WINCH: Self = Self(28);

    /// The signal numbered `n`, if ptyro knows it.
    pub fn from_named_raw(n: i32) -> Option<Self> {
        [Self::HUP, Self::INT, Self::KILL, Self::TERM, Self::CONT, Self::WINCH]
            .into_iter()
            .find(|s| s.0 == n)
    }

    pub fn as_raw(self) -> i32 {
        self.0
    }
}

/// The pseudoconsole's output pipe (WIN5).
#[derive(Debug)]
pub enum Reader {}
/// The pseudoconsole's input pipe; it also resizes (WIN5).
#[derive(Debug)]
pub enum Writer {}
/// The pseudoconsole itself, for the probes (WIN5).
#[derive(Debug)]
pub enum Master {}

impl Master {
    pub fn try_clone(&self) -> io::Result<Self> {
        match *self {}
    }
}

impl tokio::io::AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match *self {}
    }
}

impl tokio::io::AsyncWrite for Writer {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        match *self {}
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }
}

fn not_yet() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "sessions on Windows itself aren't built yet (ConPTY, WIN5): run ptyro's daemon in WSL",
    )
}

pub fn spawn(_: SpawnRequest<'_>) -> io::Result<PtyChild> {
    Err(not_yet())
}

pub fn resize(writer: &Writer, _: TermSize) -> io::Result<()> {
    match *writer {}
}

pub fn signal_group(_pid: u32, _sig: Signal) -> io::Result<()> {
    Err(not_yet())
}

/// Linux only (`PR_SET_PDEATHSIG`); nothing elsewhere. WIN5's Job Objects kill on close.
pub fn die_with_parent(_: &mut std::process::Command) {}

pub fn is_raw(master: &Master) -> Option<bool> {
    match *master {}
}

pub async fn exited_unreaped(_pid: u32) -> io::Result<(Option<i32>, Option<i32>)> {
    Err(not_yet())
}

pub fn foreground_pid(master: &Master) -> Option<u32> {
    match *master {}
}

/// How a child that was waited for ended: (exit code, no signal on Windows).
pub fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), None)
}

/// Start `cmd` in a process group of its own (`CREATE_NEW_PROCESS_GROUP`), so a console Ctrl+C
/// aimed at ptyro doesn't reach it.
pub fn own_group(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_keep_their_posix_numbers() {
        assert_eq!(Signal::from_named_raw(9), Some(Signal::KILL));
        assert_eq!(Signal::from_named_raw(15).map(Signal::as_raw), Some(15));
        assert_eq!(Signal::from_named_raw(64), None);
    }

    #[test]
    fn spawning_says_what_to_do_instead() {
        let e = spawn(SpawnRequest {
            argv: &["cmd.exe".into()],
            cwd: std::path::Path::new(r"C:\"),
            env: Vec::new(),
            size: TermSize::DEFAULT,
        })
        .err()
        .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
        assert!(e.to_string().contains("WSL"), "{e}");
    }
}
