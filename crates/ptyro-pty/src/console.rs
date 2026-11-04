//! The terminal as a program inside a session sees it: what ptyro's fake agent (and any test
//! program) needs to read keys one at a time and know its size. Unix: termios on stdin/stdout.
//! Windows (WIN5): the console's modes, with VT input on, so query replies and keys arrive as the
//! bytes a Unix terminal sends.

use std::io;
use std::time::Duration;

/// Keys one at a time, unechoed, until dropped (`ICANON` and `ECHO` off; signals still work).
pub struct RawInput {
    #[cfg(unix)]
    saved: Option<rustix::termios::Termios>,
    #[cfg(windows)]
    saved: Option<u32>,
}

impl RawInput {
    #[cfg(unix)]
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

    #[cfg(windows)]
    pub fn new() -> Self {
        let saved = windows::input_mode();
        if let Some(mode) = saved {
            windows::set_input_mode(windows::raw(mode));
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
        #[cfg(unix)]
        if let Some(s) = &self.saved {
            let _ = rustix::termios::tcsetattr(
                rustix::stdio::stdin(),
                rustix::termios::OptionalActions::Now,
                s,
            );
        }
        #[cfg(windows)]
        if let Some(mode) = self.saved {
            windows::set_input_mode(mode);
        }
    }
}

/// The terminal's size as (columns, rows).
pub fn size() -> io::Result<(u16, u16)> {
    #[cfg(unix)]
    {
        let w = rustix::termios::tcgetwinsize(rustix::stdio::stdout())?;
        Ok((w.ws_col, w.ws_row))
    }
    #[cfg(windows)]
    {
        windows::size()
    }
}

/// Wait up to `timeout` for stdin to have something to read.
pub fn input_ready(timeout: Duration) -> bool {
    #[cfg(unix)]
    {
        let fd = rustix::stdio::stdin();
        let mut pfd = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        let t = rustix::event::Timespec {
            tv_sec: timeout.as_secs() as _,
            tv_nsec: timeout.subsec_nanos() as _,
        };
        rustix::event::poll(&mut pfd, Some(&t)).unwrap_or(0) > 0
    }
    #[cfg(windows)]
    {
        windows::input_ready(timeout)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // console calls on this process's own standard handles
mod windows {
    use std::io;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetConsoleScreenBufferInfo,
        GetNumberOfConsoleInputEvents, GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    fn stdin() -> HANDLE {
        // SAFETY: a standard handle of this process; not ours to close.
        unsafe { GetStdHandle(STD_INPUT_HANDLE) }
    }

    pub fn input_mode() -> Option<u32> {
        let mut mode: CONSOLE_MODE = 0;
        // SAFETY: `mode` is written on success; a non-console handle fails, which is the answer.
        (unsafe { GetConsoleMode(stdin(), &mut mode) } != 0).then_some(mode)
    }

    pub fn set_input_mode(mode: u32) {
        // SAFETY: the console input handle, a mode built from the one it had.
        unsafe { SetConsoleMode(stdin(), mode) };
    }

    /// No line editing or echo; keys and query replies as VT bytes. Ctrl+C stays a signal
    /// (`ENABLE_PROCESSED_INPUT` kept), as `ISIG` does on Unix.
    pub fn raw(mode: u32) -> u32 {
        (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT
    }

    pub fn size() -> io::Result<(u16, u16)> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is written on success.
        let ok =
            unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) } != 0;
        if !ok {
            return Err(io::Error::last_os_error());
        }
        let w = info.srWindow;
        Ok(((w.Right - w.Left + 1) as u16, (w.Bottom - w.Top + 1) as u16))
    }

    pub fn input_ready(timeout: Duration) -> bool {
        let h = stdin();
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: waiting on our own console input handle; the count is written on success.
        unsafe {
            if WaitForSingleObject(h, ms) != WAIT_OBJECT_0 {
                return false;
            }
            let mut n = 0u32;
            GetNumberOfConsoleInputEvents(h, &mut n) != 0 && n > 0
        }
    }
}
