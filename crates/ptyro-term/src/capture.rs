//! What a shell's command printed, as plain text (SPEC §25.6.5): the bytes between its
//! OSC 133 `C` and `D` marks, with the escape sequences taken out, kept in a bounded space
//! while the command runs. Only switched on where the user asked for a shell's output to be
//! kept (`kb.shell = "output"`); otherwise nothing is collected.
//!
//! **Bounded**: the first [`HEAD`] bytes and the last [`TAIL`], whole lines, with how many
//! lines were left out between them. A line is at most [`LINE`] bytes.
//!
//! **Never**: a command that drew the screen instead of printing (the alternate screen: an
//! editor, a pager, `htop`; or the cursor sent to a place on the screen), whose "output" is
//! a picture, not text. What was typed without echo (a password) was never printed, so it is
//! never here; and the daemon keeps nothing of a command that was typed into at all.

use std::collections::VecDeque;

/// The first bytes of a command's output that are kept.
pub const HEAD: usize = 2 << 10;
/// The last bytes that are kept.
pub const TAIL: usize = 6 << 10;
/// The most of one line.
pub const LINE: usize = 1 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Esc,
    /// `ESC` and an intermediate (`ESC ( B`): one more byte.
    EscOne,
    Csi,
    /// OSC, DCS, SOS, PM, APC: until BEL or ST.
    Str,
    StrEsc,
}

/// The output of the command that is running, while it is collected.
#[derive(Debug, Default)]
pub struct Capture {
    on: bool,
    /// Between a `C` and its `D`.
    running: bool,
    /// The command drew the screen: nothing of it is kept.
    drew: bool,
    state: State,
    /// A CSI's parameters, for the few that matter.
    csi: Vec<u8>,
    /// A character's bytes so far (UTF-8).
    utf8: Vec<u8>,
    line: String,
    /// A carriage return was the last thing on the line: what comes next overwrites it.
    cr: bool,
    head: String,
    head_full: bool,
    tail: VecDeque<String>,
    tail_bytes: usize,
    left_out: usize,
}

impl Capture {
    /// Switch collecting on or off (off drops what was collected).
    pub fn set(&mut self, on: bool) {
        if self.on != on {
            *self = Self { on, ..Self::default() };
        }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    /// A command starts (`133;C`).
    pub fn start(&mut self) {
        let on = self.on;
        *self = Self { on, running: on, ..Self::default() };
    }

    /// The command ended (`133;D`, or the next prompt): what it printed, if it printed
    /// anything and didn't draw the screen.
    pub fn finish(&mut self) -> Option<String> {
        if !self.running {
            return None;
        }
        self.end_line(false);
        let on = self.on;
        let done = std::mem::replace(self, Self { on, ..Self::default() });
        if done.drew {
            return None;
        }
        let mut text = done.head;
        if done.left_out > 0 {
            let s = if done.left_out == 1 { "" } else { "s" };
            text.push_str(&format!("… {} line{s} left out …\n", done.left_out));
        }
        text.extend(done.tail);
        let text = text.trim_end().to_owned();
        (!text.trim().is_empty()).then_some(text)
    }

    /// Bytes the program wrote.
    pub fn feed(&mut self, bytes: &[u8]) {
        if !self.running || self.drew {
            return;
        }
        for &b in bytes {
            self.step(b);
            if self.drew {
                // Nothing of it is kept: nothing more is looked at.
                self.head.clear();
                self.tail.clear();
                self.line.clear();
                return;
            }
        }
    }

    fn step(&mut self, b: u8) {
        match self.state {
            State::Ground => match b {
                0x1b => self.state = State::Esc,
                b'\n' => self.end_line(true),
                b'\r' => self.cr = true,
                0x08 => {
                    self.line.pop();
                }
                b'\t' => self.push('\t'),
                0x00..=0x1f | 0x7f => {}
                0x20..=0x7e => self.push(b as char),
                _ => self.utf8(b),
            },
            State::Esc => {
                self.state = match b {
                    b'[' => {
                        self.csi.clear();
                        State::Csi
                    }
                    b']' | b'P' | b'X' | b'^' | b'_' => State::Str,
                    0x20..=0x2f => State::EscOne,
                    _ => State::Ground,
                }
            }
            State::EscOne => self.state = State::Ground,
            State::Csi => match b {
                0x40..=0x7e => {
                    self.csi_end(b);
                    self.state = State::Ground;
                }
                0x1b => self.state = State::Esc,
                _ if self.csi.len() < 32 => self.csi.push(b),
                _ => {}
            },
            State::Str => match b {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::StrEsc,
                _ => {}
            },
            State::StrEsc => self.state = if b == b'\\' { State::Ground } else { State::Str },
        }
    }

    /// The alternate screen (`?47`, `?1047`, `?1049`), or the cursor put at a place on the
    /// screen (`H`, `f`): the command draws, it doesn't print.
    fn csi_end(&mut self, last: u8) {
        let alt = last == b'h'
            && self.csi.first() == Some(&b'?')
            && self.csi[1..].split(|b| *b == b';').any(|p| matches!(p, b"47" | b"1047" | b"1049"));
        if alt || matches!(last, b'H' | b'f') {
            self.drew = true;
        }
    }

    fn utf8(&mut self, b: u8) {
        if b >= 0xc0 {
            self.utf8.clear();
        }
        self.utf8.push(b);
        if let Ok(s) = std::str::from_utf8(&self.utf8) {
            if let Some(c) = s.chars().next() {
                self.push(c);
            }
            self.utf8.clear();
        } else if self.utf8.len() >= 4 {
            self.utf8.clear();
        }
    }

    fn push(&mut self, c: char) {
        if std::mem::take(&mut self.cr) {
            // (A progress line that writes itself over: its last state is what was printed.)
            self.line.clear();
        }
        if self.line.len() + c.len_utf8() <= LINE {
            self.line.push(c);
        }
    }

    fn end_line(&mut self, newline: bool) {
        self.cr = false;
        let mut line = std::mem::take(&mut self.line);
        line.truncate(line.trim_end().len());
        if !newline && line.is_empty() {
            return;
        }
        line.push('\n');
        if !self.head_full && self.head.len() + line.len() <= HEAD {
            self.head.push_str(&line);
            return;
        }
        self.head_full = true;
        self.tail_bytes += line.len();
        self.tail.push_back(line);
        while self.tail_bytes > TAIL {
            let Some(gone) = self.tail.pop_front() else { break };
            self.tail_bytes -= gone.len();
            self.left_out += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(bytes: &[u8]) -> Option<String> {
        let mut c = Capture::default();
        c.set(true);
        c.start();
        // (Split anywhere: a sequence may be cut by a read.)
        for piece in bytes.chunks(3) {
            c.feed(piece);
        }
        c.finish()
    }

    #[test]
    fn what_was_printed_is_kept_as_plain_text() {
        let out =
            ran(b"\x1b[1;32mCompiling\x1b[0m ptyro\r\n\x1b]0;a title\x07warning: caf\xc3\xa9\r\n")
                .unwrap();
        assert_eq!(out, "Compiling ptyro\nwarning: café");
    }

    #[test]
    fn a_line_that_writes_itself_over_is_its_last_state() {
        let out = ran(b"10%\r50%\r100%\ndone\n").unwrap();
        assert_eq!(out, "100%\ndone");
        assert_eq!(ran(b"abc\x08\x08x\n").unwrap(), "ax");
    }

    #[test]
    fn nothing_is_collected_when_off_or_outside_a_command() {
        let mut c = Capture::default();
        c.start();
        c.feed(b"hello\n");
        assert_eq!(c.finish(), None, "off");
        c.set(true);
        c.feed(b"at the prompt\n");
        assert_eq!(c.finish(), None, "no command is running");
        c.start();
        c.feed(b"\n  \n");
        assert_eq!(c.finish(), None, "nothing but blank lines");
        c.start();
        c.feed(b"one\n");
        c.set(false);
        assert_eq!(c.finish(), None, "switched off while it ran");
    }

    #[test]
    fn a_command_that_drew_the_screen_keeps_nothing() {
        assert_eq!(ran(b"before\n\x1b[?1049hthe editor\x1b[?1049lafter\n"), None);
        assert_eq!(ran(b"\x1b[?47h"), None);
        assert_eq!(ran(b"\x1b[2J\x1b[Htop - 12:00\n"), None);
        assert_eq!(ran(b"\x1b[10;4fat a place\n"), None);
        // Colours, erasing a line and moving along it are printing.
        assert_eq!(ran(b"\x1b[?25l\x1b[2K\x1b[1Gok\x1b[?25h\n").unwrap(), "ok");
    }

    #[test]
    fn the_head_and_the_tail_are_kept_and_the_middle_counted() {
        let mut bytes = Vec::new();
        for n in 0..2000 {
            bytes.extend_from_slice(format!("line number {n:04}\n").as_bytes());
        }
        let out = ran(&bytes).unwrap();
        assert!(out.len() <= HEAD + TAIL + 64, "{}", out.len());
        assert!(out.starts_with("line number 0000\n"), "{out}");
        assert!(out.ends_with("line number 1999"), "{out}");
        let head = HEAD / 17;
        let tail = TAIL / 17;
        assert!(out.contains(&format!("… {} lines left out …", 2000 - head - tail)), "{out}");
        // One very long line is cut, not kept whole.
        let long = ran(&[b"x".repeat(LINE * 3), b"\n".to_vec()].concat()).unwrap();
        assert_eq!(long.len(), LINE);
    }
}
