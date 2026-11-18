//! The terminal as a program inside a session sees it: what ptyro's fake agent (and any test
//! program) needs to read keys one at a time and know its size: termios on stdin/stdout.

use std::io;
use std::time::Duration;

/// Keys one at a time, unechoed, until dropped (`ICANON` and `ECHO` off; signals still work).
pub struct RawInput {
    saved: Option<rustix::termios::Termios>,
}

impl RawInput {
    pub fn new() -> Self {
        use rustix::termios::{self, LocalModes, OptionalActions};
        let fd = rustix::stdio::stdin();
        let saved = termios::tcgetattr(fd).ok();
        if let Some(s) = &saved {
            let mut raw = s.clone();
            raw.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO);
            let _ = termios::tcsetattr(fd, OptionalActions::Now, &raw);
        }
        Self { saved }
    }

    /// Lines as typed, but unechoed, until dropped (`ECHO` off): for a program that draws what
    /// it took itself, as an agent's input box does.
    pub fn unechoed() -> Self {
        use rustix::termios::{self, LocalModes, OptionalActions};
        let fd = rustix::stdio::stdin();
        let saved = termios::tcgetattr(fd).ok();
        if let Some(s) = &saved {
            let mut quiet = s.clone();
            quiet.local_modes.remove(LocalModes::ECHO);
            let _ = termios::tcsetattr(fd, OptionalActions::Now, &quiet);
        }
        Self { saved }
    }

    /// Whether stdin is a terminal it could change.
    pub fn is_raw(&self) -> bool {
        self.saved.is_some()
    }
}

impl Default for RawInput {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawInput {
    fn drop(&mut self) {
        if let Some(s) = &self.saved {
            let _ = rustix::termios::tcsetattr(
                rustix::stdio::stdin(),
                rustix::termios::OptionalActions::Now,
                s,
            );
        }
    }
}

/// The terminal's size as (columns, rows).
pub fn size() -> io::Result<(u16, u16)> {
    let w = rustix::termios::tcgetwinsize(rustix::stdio::stdout())?;
    Ok((w.ws_col, w.ws_row))
}

/// Wait up to `timeout` for stdin to have something to read.
pub fn input_ready(timeout: Duration) -> bool {
    let fd = rustix::stdio::stdin();
    let mut pfd = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
    let t = rustix::event::Timespec {
        tv_sec: timeout.as_secs() as _,
        tv_nsec: timeout.subsec_nanos() as _,
    };
    rustix::event::poll(&mut pfd, Some(&t)).unwrap_or(0) > 0
}
