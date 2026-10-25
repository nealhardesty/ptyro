//! The `Emulator` trait (SPEC §5.2) and its alacritty_terminal backend, including the query
//! responder (SPEC §5.6): replies ptyro owns are produced by the side-channel scanner *before*
//! the emulator sees the batch, so they precede the emulator's own replies (7501 before DA1).

use ptyro_core::sync::MutexExt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AColor, CursorShape as ACursorShape, NamedColor, Processor, Rgb,
};
use ptyro_core::TermSize;
use ptyro_core::screen::{Color, CursorShape, CursorState, Style, StyleFlags};

use crate::modes::TermModes;
use crate::sniff::{PSP_REPLY, Psp, PspReport, Seq, Sniffer, State, parse_psp};

/// Combining characters reported per cell at most (alacritty itself keeps any number).
pub const MAX_COMBINING: usize = 16;

/// The largest OSC 52 clipboard write passed on (decoded bytes); bigger ones are dropped.
pub const MAX_CLIPBOARD: usize = 1 << 20;

/// One cell of the visible screen, as the frame builder and text renderers need it.
#[derive(Debug, Clone, PartialEq)]
pub struct CellData {
    pub c: char,
    /// Zero-width characters combined with `c` (rare), at most [`MAX_COMBINING`].
    pub zerowidth: Option<Vec<char>>,
    pub style: Style,
    /// First half of a double-width character.
    pub wide: bool,
    /// Second half of a double-width character: carries no content of its own.
    pub spacer: bool,
    /// Part of a shell prompt (OSC 133;A … B), tracked through the scrollback (see `feed`).
    pub prompt: bool,
}

/// Something the program did that isn't screen content.
#[derive(Debug, Clone, PartialEq)]
pub enum TermEvent {
    Title(Option<String>),
    Bell,
    /// OSC 52 copy request; forwarded only to the controlling client (SPEC §17).
    Clipboard(String),
    /// OSC 7501 report (SPEC §9.2).
    ProgramStatus(PspReport),
    /// OSC 9 / 777 / 99 desktop notification text (raw body).
    Notification(String),
    /// OSC 7: the shell reports its working directory.
    Cwd(std::path::PathBuf),
    /// OSC 133 shell-integration mark: `A` prompt start, `B` input start, `C` output start,
    /// `D` command finished (with its exit status when given).
    PromptMark {
        mark: char,
        exit: Option<i32>,
        /// `C;cmdline=…` (or kitty's percent-encoded `cmdline_url=…`): the command line.
        cmdline: Option<String>,
    },
    /// OSC 9;4 (ConEmu) progress: state 0 clear, 1 normal, 2 error, 3 indeterminate, 4 paused.
    Progress {
        state: u8,
        percent: Option<u8>,
    },
    /// RIS: a full terminal reset (drops OSC 7501 records).
    Reset,
    /// Another program-status vocabulary (iTerm2 OSC 21337): the state word and an optional
    /// message. Low confidence (SPEC §9.2).
    ForeignStatus {
        state: String,
        msg: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct FeedOutput {
    /// Bytes to write back to the child, in order.
    pub replies: Vec<u8>,
    pub events: Vec<TermEvent>,
}

/// Rows changed since the last `take_damage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    All,
    Rows(Vec<u16>),
}

pub trait Emulator: Send {
    fn feed(&mut self, bytes: &[u8], out: &mut FeedOutput);
    fn resize(&mut self, size: TermSize);
    fn size(&self) -> TermSize;
    fn take_damage(&mut self) -> Damage;
    /// Cells of `line`, replacing `out`'s contents: 0 is the top screen row, negative lines are
    /// history (-1 the newest).
    fn line(&self, line: i32, out: &mut Vec<CellData>);
    fn line_wrapped(&self, line: i32) -> bool;
    /// Cells of visible row `y` (top = 0), replacing `out`'s contents.
    fn row(&self, y: u16, out: &mut Vec<CellData>) {
        self.line(i32::from(y), out);
    }
    fn row_wrapped(&self, y: u16) -> bool {
        self.line_wrapped(i32::from(y))
    }
    fn cursor(&self) -> CursorState;
    fn modes(&self) -> TermModes;
    fn title(&self) -> Option<&str>;
    /// Lines of history above the screen.
    fn scrollback_len(&self) -> usize;
    /// When an open synchronized-output batch (DEC 2026) must be flushed regardless.
    fn sync_deadline(&self) -> Option<Instant>;
    fn expire_sync(&mut self, out: &mut FeedOutput);
    /// The viewer's real default colours, used to answer OSC 10/11 (SPEC §5.6).
    fn set_default_colors(&mut self, fg: Option<[u8; 3]>, bg: Option<[u8; 3]>);
}

/// Cell size in pixels reported until a real client tells us (SPEC §5.6).
const CELL_W: u16 = 9;
const CELL_H: u16 = 18;
/// Theme defaults for OSC 10/11/12 when the outer terminal's colours are unknown.
const DEFAULT_FG: Rgb = Rgb { r: 0xd8, g: 0xd8, b: 0xd8 };
const DEFAULT_BG: Rgb = Rgb { r: 0x1c, g: 0x1c, b: 0x1c };
/// alacritty answers DA1 as a VT102; we claim VT220 with ANSI colour (SPEC §5.6).
const ALACRITTY_DA1: &str = "\x1b[?6c";
const PTYRO_DA1: &str = "\x1b[?62;22c";

#[derive(Clone, Copy)]
struct Dims(TermSize);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.0.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.0.rows as usize
    }
    fn columns(&self) -> usize {
        self.0.cols as usize
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.locked().push(event);
    }
}

pub struct AlacrittyEmulator {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    sniffer: Sniffer,
    seqs: Vec<Seq>,
    ends: Vec<usize>,
    /// A prompt mark is open (OSC 133;A seen, B not yet).
    mark_open: bool,
    size: TermSize,
    title: Option<String>,
    fg: Rgb,
    bg: Rgb,
    /// History lines kept (the config's `scrolling_history`).
    scrollback: usize,
    marks: MarkLimit,
}

/// Bounds what alacritty stores per cell: it keeps every zero-width character it is sent, so a
/// program printing `a` and a million U+0301 would grow one cell without end. At most
/// [`MAX_COMBINING`] zero-width characters pass per printed base character (escape sequences
/// don't reset the count, so they can't be used to start a fresh allowance on the same cell).
#[derive(Default)]
struct MarkLimit {
    /// Zero-width characters since the last base character.
    run: usize,
    /// The start of a UTF-8 sequence split across reads.
    carry: Vec<u8>,
    /// Where an escape sequence is (its bytes aren't text: they never reset the count).
    esc: Esc,
    /// Bytes of the OSC/DCS/APC string being read.
    str_len: usize,
    /// `str_len` went past [`MAX_STRING`] during this read.
    over: bool,
}

/// How much of one OSC/DCS/APC string reaches the parser: vte (with `std`) buffers a whole OSC
/// payload before dispatching it, so a program that never ends one would grow it without limit.
/// Past this the rest of the string is dropped, until its terminator, which still passes. It
/// holds the largest clipboard write ([`MAX_CLIPBOARD`] as base64) and then some, so a payload
/// cut here is still over that limit and is refused, never delivered short.
const MAX_STRING: usize = 2 << 20;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Esc {
    #[default]
    Ground,
    /// After ESC.
    Start,
    /// A CSI's parameters, until its final byte.
    Csi,
    /// OSC, DCS, APC, PM, SOS: until BEL or ST.
    Str,
    /// ESC inside a string (ST, if `\\` follows).
    StrStop,
}

impl MarkLimit {
    /// The state after the byte that follows an ESC.
    fn after_esc(b: u8) -> Esc {
        match b {
            b'[' => Esc::Csi,
            b']' | b'P' | b'_' | b'^' | b'X' => Esc::Str,
            0x20..=0x2f => Esc::Start,
            0x1b => Esc::Start,
            _ => Esc::Ground,
        }
    }

    /// Track an ASCII or control byte as vte's parser does; `true` when it is printed text.
    fn ascii(&mut self, b: u8) -> bool {
        let before = self.esc;
        self.esc = match (before, b) {
            // CAN and SUB end whatever is open, wherever it is.
            (_, 0x18 | 0x1a) => Esc::Ground,
            (Esc::Str, 0x07) | (Esc::StrStop, b'\\') => Esc::Ground,
            (Esc::Str | Esc::StrStop, 0x1b) => Esc::StrStop,
            (Esc::Str, _) => Esc::Str,
            // ESC inside a string ends it and starts the next sequence with this byte.
            (Esc::StrStop, b) => Self::after_esc(b),
            (_, 0x1b) => Esc::Start,
            (Esc::Start, b) => Self::after_esc(b),
            (Esc::Csi, 0x40..=0x7e) => Esc::Ground,
            (Esc::Csi, _) => Esc::Csi,
            (Esc::Ground, _) => return (0x20..0x7f).contains(&b),
        };
        if self.esc == Esc::Str {
            self.str_len = if before == Esc::Str { self.str_len + 1 } else { 0 };
            self.over |= self.str_len > MAX_STRING;
        }
        false
    }

    /// Whether the byte at `i` of the string being read is dropped (a payload past the cap).
    fn drops(&self, b: u8) -> bool {
        self.esc == Esc::Str && self.str_len > MAX_STRING && !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b)
    }

    fn filter<'a>(&mut self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        use std::borrow::Cow;
        self.over = false;
        // Combining marks start at U+0300 (lead byte 0xCC): ASCII and Latin-1 text only needs
        // the escape state followed.
        if self.carry.is_empty() && !bytes.iter().any(|b| *b >= 0xCC) {
            // Nothing to reset and no sequence open: only the escapes from the first ESC on
            // change anything.
            let from = if self.run == 0 && self.esc == Esc::Ground {
                bytes.iter().position(|b| *b == 0x1b).unwrap_or(bytes.len())
            } else {
                0
            };
            let saved = (self.run, self.esc, self.str_len);
            for &b in &bytes[from..] {
                if b < 0x80 && self.ascii(b) {
                    self.run = 0;
                } else if b >= 0xC0 && self.esc == Esc::Ground {
                    self.run = 0; // a Latin-1 letter: a base character
                }
            }
            if !self.over {
                return Cow::Borrowed(bytes);
            }
            // A string went past the cap in this read: go again, dropping what is past it.
            (self.run, self.esc, self.str_len) = saved;
            self.over = false;
        }
        let mut input = std::mem::take(&mut self.carry);
        input.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            if b < 0x80 {
                let drop = self.drops(b) && self.esc == Esc::Str;
                if self.ascii(b) {
                    self.run = 0;
                }
                if !drop {
                    out.push(b);
                }
                i += 1;
                continue;
            }
            let mut len = match b {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => 1,
            };
            // A lead byte not followed by continuation bytes is one invalid byte: what follows
            // is read on its own (it may be the ESC or the BEL that ends a string).
            let have = (1..len)
                .take_while(|k| input.get(i + k).is_some_and(|c| (0x80..=0xBF).contains(c)))
                .count();
            if i + len > input.len() && have == input.len() - i - 1 {
                // Split across reads: finish it next time.
                self.carry = input[i..].to_vec();
                break;
            }
            if have + 1 < len {
                len = 1;
            }
            let chunk = &input[i..i + len];
            i += len;
            if self.esc == Esc::Ground
                && let Some(c) = std::str::from_utf8(chunk).ok().and_then(|s| s.chars().next())
                && !c.is_control()
            {
                if unicode_width::UnicodeWidthChar::width(c) == Some(0) {
                    self.run += 1;
                    if self.run > MAX_COMBINING {
                        continue;
                    }
                } else {
                    self.run = 0;
                }
            }
            if self.esc == Esc::Str {
                self.str_len += len;
                if self.str_len > MAX_STRING {
                    continue;
                }
            }
            out.extend_from_slice(chunk);
        }
        Cow::Owned(out)
    }
}

/// A title is for a tab or a header: a few hundred bytes of it, never a program's whole output.
const MAX_TITLE: usize = 512;

fn clip_title(mut t: String) -> String {
    if t.len() > MAX_TITLE {
        let mut end = MAX_TITLE;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        t.truncate(end);
    }
    t
}

impl AlacrittyEmulator {
    pub fn new(size: TermSize, scrollback: usize) -> Self {
        // Sizes come off the wire and from files: alacritty panics on 0 and allocates whatever
        // it is given.
        let size = size.clamped();
        let events = Listener::default();
        let config =
            Config { kitty_keyboard: true, scrolling_history: scrollback, ..Config::default() };
        Self {
            term: Term::new(config, &Dims(size), events.clone()),
            parser: Processor::new(),
            events,
            sniffer: Sniffer::default(),
            seqs: Vec::new(),
            ends: Vec::new(),
            mark_open: false,
            size,
            title: None,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            scrollback,
            marks: MarkLimit::default(),
        }
    }

    fn observe(&mut self, seq: &Seq, out: &mut FeedOutput) {
        match seq {
            Seq::Osc(body) => match parse_psp(body) {
                Some(Psp::Probe) => out.replies.extend_from_slice(PSP_REPLY),
                Some(Psp::Report(r)) => out.events.push(TermEvent::ProgramStatus(r)),
                None => {
                    if let Some(ev) = side_channel_osc(body) {
                        out.events.push(ev);
                    }
                }
            },
            Seq::Csi { params, final_byte } => match (params.as_slice(), final_byte) {
                // XTVERSION
                ([b'>', ..], b'q') => {
                    let v = env!("CARGO_PKG_VERSION");
                    out.replies.extend_from_slice(format!("\x1bP>|ptyro {v}\x1b\\").as_bytes());
                }
                // XTWINOPS 16: cell size in pixels (alacritty only answers 14 and 18).
                (b"16", b't') => {
                    out.replies.extend_from_slice(format!("\x1b[6;{CELL_H};{CELL_W}t").as_bytes())
                }
                _ => {}
            },
            Seq::Dcs(body) => {
                if let Some(names) = body.strip_prefix(b"+q") {
                    xtgettcap(names, &mut out.replies);
                }
            }
            Seq::Ris => out.events.push(TermEvent::Reset),
        }
    }

    fn drain_events(&mut self, out: &mut FeedOutput) {
        let events = std::mem::take(&mut *self.events.0.locked());
        for ev in events {
            match ev {
                Event::PtyWrite(s) if s == ALACRITTY_DA1 => {
                    out.replies.extend_from_slice(PTYRO_DA1.as_bytes());
                }
                Event::PtyWrite(s) => out.replies.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(idx, fmt) => {
                    let rgb = self.term.colors()[idx].unwrap_or_else(|| self.default_color(idx));
                    out.replies.extend_from_slice(fmt(rgb).as_bytes());
                }
                Event::TextAreaSizeRequest(fmt) => {
                    let ws = WindowSize {
                        num_lines: self.size.rows,
                        num_cols: self.size.cols,
                        cell_width: CELL_W,
                        cell_height: CELL_H,
                    };
                    out.replies.extend_from_slice(fmt(ws).as_bytes());
                }
                Event::Title(t) => {
                    let t = clip_title(t);
                    self.title = Some(t.clone());
                    out.events.push(TermEvent::Title(Some(t)));
                }
                Event::ResetTitle => {
                    self.title = None;
                    out.events.push(TermEvent::Title(None));
                }
                Event::Bell => out.events.push(TermEvent::Bell),
                Event::ClipboardStore(_, text) if text.len() <= MAX_CLIPBOARD => {
                    out.events.push(TermEvent::Clipboard(text));
                }
                // Clipboard *reads* are denied (SPEC §17); the rest is renderer plumbing.
                _ => {}
            }
        }
    }

    fn default_color(&self, idx: usize) -> Rgb {
        if idx == NamedColor::Background as usize {
            self.bg
        } else if idx == NamedColor::Foreground as usize || idx == NamedColor::Cursor as usize {
            self.fg
        } else {
            xterm_palette(idx)
        }
    }

    fn map_color(&self, c: AColor) -> Color {
        let colors = self.term.colors();
        let rgb = |c: Rgb| Color::Rgb { r: c.r, g: c.g, b: c.b };
        match c {
            AColor::Spec(c) => rgb(c),
            AColor::Indexed(i) => colors[i as usize].map_or(Color::Indexed { i }, rgb),
            AColor::Named(n) => {
                let idx = n as usize;
                if let Some(c) = colors[idx] {
                    return rgb(c);
                }
                match idx {
                    0..=15 => Color::Indexed { i: idx as u8 },
                    _ if is_dim(n) => {
                        Color::Indexed { i: (idx - NamedColor::DimBlack as usize) as u8 }
                    }
                    _ => Color::Default,
                }
            }
        }
    }
}

/// Prompt marks ride along as an OSC 8 hyperlink with this URI: alacritty keeps hyperlinks per
/// cell through scrolling and reflow, which is exactly what marks need and line numbers can't give
/// (they shift once history is full). Our renderers never show hyperlinks, so it stays invisible.
/// Caveat: a program's *own* hyperlink open at that moment is ended by the mark.
const MARK_URI: &str = "ptyro:prompt";
const MARK_OPEN: &[u8] = b"\x1b]8;id=ptyro-prompt;ptyro:prompt\x1b\\";
const MARK_CLOSE: &[u8] = b"\x1b]8;;\x1b\\";

/// `Some(true)` opens a prompt mark (OSC 133;A), `Some(false)` closes it (B, C, D).
fn prompt_mark(seq: &Seq) -> Option<bool> {
    let Seq::Osc(body) = seq else { return None };
    let rest = body.strip_prefix(b"133;")?;
    match rest.first()? {
        b'A' => Some(true),
        b'B' | b'C' | b'D' => Some(false),
        _ => None,
    }
}

impl AlacrittyEmulator {
    fn set_mark(&mut self, open: bool) {
        if self.mark_open {
            self.parser.advance(&mut self.term, MARK_CLOSE);
        }
        if open {
            self.parser.advance(&mut self.term, MARK_OPEN);
        }
        self.mark_open = open;
    }
}

fn is_dim(n: NamedColor) -> bool {
    let i = n as usize;
    (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i)
        || n == NamedColor::DimForeground
}

impl Emulator for AlacrittyEmulator {
    fn feed(&mut self, bytes: &[u8], out: &mut FeedOutput) {
        let filtered = self.marks.filter(bytes);
        let bytes: &[u8] = &filtered;
        let mut seqs = std::mem::take(&mut self.seqs);
        let mut ends = std::mem::take(&mut self.ends);
        let start = self.sniffer.state();
        self.sniffer.scan_with_ends(bytes, &mut seqs, Some(&mut ends));
        // Our replies first (they must precede the emulator's, e.g. 7501 before DA1)…
        for seq in &seqs {
            self.observe(seq, out);
        }
        // …then the bytes, split where prompt marks open or close.
        let mut pos = 0;
        for (seq, &end) in seqs.iter().zip(&ends) {
            let Some(open) = prompt_mark(seq) else { continue };
            self.parser.advance(&mut self.term, &bytes[pos..end]);
            pos = end;
            self.set_mark(open);
        }
        // After a sequence the scanner is in ground state; at the start of the batch it may not be.
        let from = if pos == 0 { start } else { State::Ground };
        if self.mark_open
            && let Some(nl) = Sniffer::ground_newline(from, &bytes[pos..])
        {
            // No OSC 133;B: a prompt never spans lines, so end the mark at the newline (one
            // outside any escape sequence: the mark's own OSC would cut that sequence short).
            self.parser.advance(&mut self.term, &bytes[pos..pos + nl]);
            pos += nl;
            self.set_mark(false);
        }
        self.parser.advance(&mut self.term, &bytes[pos..]);
        seqs.clear();
        ends.clear();
        self.seqs = seqs;
        self.ends = ends;
        self.drain_events(out);
    }

    fn resize(&mut self, size: TermSize) {
        let size = size.clamped();
        self.size = size;
        self.term.resize(Dims(size));
    }

    fn size(&self) -> TermSize {
        self.size
    }

    fn take_damage(&mut self) -> Damage {
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::All,
            TermDamage::Partial(lines) => Damage::Rows(lines.map(|l| l.line as u16).collect()),
        };
        self.term.reset_damage();
        damage
    }

    fn line(&self, line: i32, out: &mut Vec<CellData>) {
        out.clear();
        let grid = self.term.grid();
        if line >= grid.screen_lines() as i32 || line < -(grid.history_size() as i32) {
            return;
        }
        let row = &grid[Line(line)];
        for x in 0..grid.columns() {
            let cell = &row[Column(x)];
            let f = cell.flags;
            let mut flags = 0u16;
            for (af, sf) in [
                (Flags::BOLD, StyleFlags::BOLD),
                (Flags::DIM, StyleFlags::DIM),
                (Flags::ITALIC, StyleFlags::ITALIC),
                (Flags::UNDERLINE, StyleFlags::UNDERLINE),
                (Flags::DOUBLE_UNDERLINE, StyleFlags::DOUBLE_UNDERLINE),
                (Flags::UNDERCURL, StyleFlags::CURLY_UNDERLINE),
                (Flags::DOTTED_UNDERLINE, StyleFlags::DOTTED_UNDERLINE),
                (Flags::DASHED_UNDERLINE, StyleFlags::DASHED_UNDERLINE),
                (Flags::INVERSE, StyleFlags::INVERSE),
                (Flags::HIDDEN, StyleFlags::HIDDEN),
                (Flags::STRIKEOUT, StyleFlags::STRIKE),
            ] {
                if f.contains(af) {
                    flags |= sf;
                }
            }
            if matches!(cell.fg, AColor::Named(n) if is_dim(n)) {
                flags |= StyleFlags::DIM;
            }
            let style = Style {
                fg: self.map_color(cell.fg),
                bg: self.map_color(cell.bg),
                ul: cell.underline_color().map(|c| self.map_color(c)),
                flags: StyleFlags(flags),
            };
            out.push(CellData {
                c: cell.c,
                zerowidth: cell.zerowidth().map(|z| z[..z.len().min(MAX_COMBINING)].to_vec()),
                style,
                wide: f.contains(Flags::WIDE_CHAR),
                spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                prompt: cell.extra.is_some()
                    && cell.hyperlink().is_some_and(|h| h.uri() == MARK_URI),
            });
        }
    }

    fn line_wrapped(&self, line: i32) -> bool {
        let grid = self.term.grid();
        if line >= grid.screen_lines() as i32
            || line < -(grid.history_size() as i32)
            || grid.columns() == 0
        {
            return false;
        }
        grid[Line(line)][Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE)
    }

    fn cursor(&self) -> CursorState {
        let p = self.term.grid().cursor.point;
        let style = self.term.cursor_style();
        let shape = match style.shape {
            ACursorShape::Underline => CursorShape::Underline,
            ACursorShape::Beam => CursorShape::Bar,
            _ => CursorShape::Block,
        };
        CursorState {
            x: p.column.0 as u16,
            y: p.line.0.max(0) as u16,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && style.shape != ACursorShape::Hidden,
            shape,
            blinking: style.blinking,
        }
    }

    fn modes(&self) -> TermModes {
        TermModes::from_alacritty(*self.term.mode())
    }

    fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }

    fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    fn expire_sync(&mut self, out: &mut FeedOutput) {
        self.parser.stop_sync(&mut self.term);
        self.drain_events(out);
    }

    fn set_default_colors(&mut self, fg: Option<[u8; 3]>, bg: Option<[u8; 3]>) {
        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
        self.fg = fg.map_or(DEFAULT_FG, rgb);
        self.bg = bg.map_or(DEFAULT_BG, rgb);
    }
}

/// OSCs we observe but alacritty doesn't surface (SPEC §5.7).
fn side_channel_osc(body: &[u8]) -> Option<TermEvent> {
    let text = std::str::from_utf8(body).ok()?;
    let (num, rest) = text.split_once(';').unwrap_or((text, ""));
    match num {
        // ConEmu progress shares OSC 9 with notifications: `9;4;<state>[;<percent>]`.
        "9" if rest.starts_with("4;") || rest == "4" => {
            let mut parts = rest.split(';').skip(1);
            let state = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let percent = parts.next().and_then(|p| p.parse::<u8>().ok()).map(|p| p.min(100));
            Some(TermEvent::Progress { state, percent })
        }
        // Shown on the desktop and in lists: no control characters, bounded length.
        "9" | "777" | "99" => Some(TermEvent::Notification(
            text.chars().filter(|c| !c.is_control()).take(1024).collect(),
        )),
        "7" => {
            // file://host/path, percent-encoded.
            let path = rest.strip_prefix("file://")?;
            let path = &path[path.find('/')?..];
            // The program says where it is; a path that isn't absolute or holds a NUL isn't one.
            let path = percent_decode(path);
            (path.starts_with('/') && !path.contains('\0')).then(|| TermEvent::Cwd(path.into()))
        }
        "133" => {
            let mut parts = rest.splitn(2, ';');
            let mark = parts.next()?.chars().next()?;
            let params = parts.next().unwrap_or("");
            let exit = if mark == 'D' {
                params.split(';').next().and_then(|e| e.parse().ok())
            } else {
                None
            };
            // The command line is the last parameter, so it may hold `;` itself.
            let cmdline = (mark == 'C')
                .then(|| {
                    params.find("cmdline=").map(|i| params[i + 8..].to_owned()).or_else(|| {
                        params.find("cmdline_url=").map(|i| {
                            percent_decode(params[i + 12..].split(';').next().unwrap_or(""))
                        })
                    })
                })
                .flatten()
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty());
            matches!(mark, 'A' | 'B' | 'C' | 'D').then_some(TermEvent::PromptMark {
                mark,
                exit,
                cmdline,
            })
        }
        // iTerm2's session status: `21337;key=value;…` with a state-like key [unverified shape].
        "21337" => {
            let mut state = None;
            let mut msg = None;
            for kv in rest.split(';') {
                match kv.split_once('=') {
                    Some(("state" | "status", v)) => state = Some(v.to_owned()),
                    Some(("msg" | "message" | "detail", v)) => {
                        msg = Some(v.chars().filter(|c| !c.is_control()).take(200).collect())
                    }
                    None if state.is_none() && !kv.is_empty() => state = Some(kv.to_owned()),
                    _ => {}
                }
            }
            state.map(|state| TermEvent::ForeignStatus { state, msg })
        }
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// XTGETTCAP (`DCS + q <hex>;<hex> ST`): answer the few capabilities programs actually ask for.
fn xtgettcap(names: &[u8], replies: &mut Vec<u8>) {
    for hex in names.split(|&b| b == b';') {
        let name = decode_hex(hex);
        let value = match name.as_deref() {
            Some("TN" | "name") => Some("ptyro"),
            Some("Co" | "colors") => Some("256"),
            Some("RGB") => Some("8/8/8"),
            _ => None,
        };
        // Never echo what isn't hex: the reply goes to the program's input, and the request came
        // from its output (a `cat`ed file could otherwise type into the shell).
        let hex = if hex.iter().all(u8::is_ascii_hexdigit) {
            String::from_utf8_lossy(hex).into_owned()
        } else {
            String::new()
        };
        match value {
            Some(v) => replies
                .extend_from_slice(format!("\x1bP1+r{hex}={}\x1b\\", encode_hex(v)).as_bytes()),
            None => replies.extend_from_slice(format!("\x1bP0+r{hex}\x1b\\").as_bytes()),
        }
    }
}

fn decode_hex(hex: &[u8]) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = hex
        .chunks(2)
        .map(|p| {
            p.iter()
                .all(u8::is_ascii_hexdigit)
                .then(|| std::str::from_utf8(p).ok().and_then(|s| u8::from_str_radix(s, 16).ok()))
                .flatten()
        })
        .collect();
    String::from_utf8(bytes?).ok()
}

fn encode_hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02X}")).collect()
}

/// The standard xterm 256-colour palette.
pub fn xterm_palette(idx: usize) -> Rgb {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match idx {
        0..=15 => {
            let (r, g, b) = ANSI[idx];
            Rgb { r, g, b }
        }
        16..=231 => {
            let i = idx - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
        }
        232..=255 => {
            let v = (8 + (idx - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ => DEFAULT_FG,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(emu: &mut AlacrittyEmulator, bytes: &[u8]) -> FeedOutput {
        let mut out = FeedOutput::default();
        emu.feed(bytes, &mut out);
        out
    }

    #[test]
    fn clipboard_writes_are_capped() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 4), 100);
        let out = feed(&mut emu, b"\x1b]52;c;aGk=\x07");
        assert!(out.events.contains(&TermEvent::Clipboard("hi".into())), "{:?}", out.events);
        // 1 MiB + 3 bytes of 'A's, base64: dropped.
        let big = "QUFB".repeat(MAX_CLIPBOARD / 3 + 1);
        let out = feed(&mut emu, format!("\x1b]52;c;{big}\x07").as_bytes());
        assert!(!out.events.iter().any(|e| matches!(e, TermEvent::Clipboard(_))));
    }

    #[test]
    fn psp_reply_precedes_da1_and_da1_is_vt220() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 4), 100);
        let out = feed(&mut emu, b"\x1b]7501;?\x1b\\\x1b[c");
        let r = String::from_utf8(out.replies).unwrap();
        let psp = r.find("\x1b]7501;?").unwrap();
        let da = r.find(PTYRO_DA1).unwrap();
        assert!(psp < da, "{r:?}");
        assert!(!r.contains(ALACRITTY_DA1));
    }

    #[test]
    fn answers_xtversion_cell_size_and_xtgettcap() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 4), 100);
        let r = String::from_utf8(feed(&mut emu, b"\x1b[>q\x1b[16t\x1bP+q544e;5858\x1b\\").replies)
            .unwrap();
        assert!(r.starts_with("\x1bP>|ptyro "), "{r:?}");
        assert!(r.contains("\x1b[6;18;9t"));
        assert!(r.contains("\x1bP1+r544e=707479726F\x1b\\"), "{r:?}"); // TN=ptyro
        assert!(r.contains("\x1bP0+r5858\x1b\\"));
    }

    #[test]
    fn osc_colour_queries_use_viewer_colours() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 4), 100);
        emu.set_default_colors(Some([0xff, 0xff, 0xff]), Some([0, 0, 0x10]));
        let r = String::from_utf8(feed(&mut emu, b"\x1b]10;?\x07\x1b]11;?\x07").replies).unwrap();
        assert!(r.contains("10;rgb:ffff/ffff/ffff"), "{r:?}");
        assert!(r.contains("11;rgb:0000/0000/1010"), "{r:?}");
    }

    #[test]
    fn prompt_marks_survive_scrolling_into_history() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 3), 100);
        feed(
            &mut emu,
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\nout1\r\n\x1b]133;A\x07$ \x07\r\nx\r\ny\r\nz",
        );
        let mut row = vec![];
        let mut prompts = vec![];
        for line in -(emu.scrollback_len() as i32)..3 {
            emu.line(line, &mut row);
            if row.iter().any(|c| c.prompt) {
                prompts.push(line);
            }
            if line == -(emu.scrollback_len() as i32) {
                assert!(
                    row[0].prompt && row[1].prompt && !row[2].prompt,
                    "only the prompt text is marked"
                );
            }
        }
        // Both prompt lines are in history now; the second (no OSC 133;B) ended at its newline.
        assert_eq!(prompts.len(), 2, "{prompts:?}");
        assert!(prompts.iter().all(|&l| l < 0));
    }

    #[test]
    fn side_channel_oscs() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let ev = feed(
            &mut emu,
            b"\x1b]7;file://host/tmp/a%20b\x07\x1b]133;D;2\x07\x1b]9;4;1;42\x07\x1b]9;done\x07",
        )
        .events;
        assert_eq!(
            ev,
            vec![
                TermEvent::Cwd("/tmp/a b".into()),
                TermEvent::PromptMark { mark: 'D', exit: Some(2), cmdline: None },
                TermEvent::Progress { state: 1, percent: Some(42) },
                TermEvent::Notification("9;done".into()),
            ]
        );
    }

    #[test]
    fn a_command_mark_carries_its_command_line() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let marks = |emu: &mut AlacrittyEmulator, bytes: &[u8]| {
            feed(emu, bytes)
                .events
                .into_iter()
                .filter_map(|e| match e {
                    TermEvent::PromptMark { mark, cmdline, .. } => Some((mark, cmdline)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // Ours (raw, `;` kept), kitty's percent-encoded one, and none at all.
        assert_eq!(
            marks(&mut emu, b"\x1b]133;C;cmdline=make test; echo ok\x07"),
            vec![('C', Some("make test; echo ok".into()))]
        );
        assert_eq!(
            marks(&mut emu, b"\x1b]133;C;cmdline_url=cargo%20t\x07"),
            vec![('C', Some("cargo t".into()))]
        );
        assert_eq!(
            marks(&mut emu, b"\x1b]133;C\x07\x1b]133;C;cmdline=\x07"),
            vec![('C', None), ('C', None)]
        );
    }

    #[test]
    fn rows_report_styles_wide_chars_and_events() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let out = feed(&mut emu, "\x1b[1;31mA\x1b[0m漢\x07\x1b]0;hi\x07".as_bytes());
        assert!(out.events.contains(&TermEvent::Bell));
        assert!(out.events.contains(&TermEvent::Title(Some("hi".into()))));
        let mut row = vec![];
        emu.row(0, &mut row);
        assert_eq!(row[0].c, 'A');
        assert_eq!(row[0].style.fg, Color::Indexed { i: 1 });
        assert!(row[0].style.flags.has(StyleFlags::BOLD));
        assert!(row[1].wide && row[2].spacer);
        assert_eq!(emu.cursor().x, 3);
    }

    #[test]
    fn sizes_are_clamped() {
        // Built directly (as serde did before it clamped): 0 would panic alacritty.
        let mut emu = AlacrittyEmulator::new(TermSize { cols: 0, rows: 0 }, 10);
        assert_eq!(emu.size(), TermSize::MIN);
        feed(&mut emu, b"hello\r\nworld");
        emu.resize(TermSize { cols: u16::MAX, rows: u16::MAX });
        assert_eq!(emu.size(), TermSize::MAX);
        emu.resize(TermSize { cols: 0, rows: 3 });
        assert_eq!(emu.size(), TermSize::new(2, 3));
        feed(&mut emu, b"more");
    }

    #[test]
    fn the_grid_never_stores_more_marks_than_it_reports() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
        // Split mid-character, and interleaved with escapes (which don't reset the allowance).
        let flood = format!("e{}", "\u{301}".repeat(200));
        let (a, b) = flood.as_bytes().split_at(4);
        feed(&mut emu, a);
        feed(&mut emu, b);
        feed(&mut emu, format!("\x1b[m{}", "\u{301}".repeat(200)).as_bytes());
        let cell = &emu.term.grid()[Line(0)][Column(0)];
        assert_eq!(cell.zerowidth().map_or(0, <[char]>::len), MAX_COMBINING);
        // A new base character gets its own allowance; ordinary text is untouched.
        feed(&mut emu, "x\u{301}y".as_bytes());
        let mut row = vec![];
        emu.row(0, &mut row);
        assert_eq!(row[1].c, 'x');
        assert_eq!(row[1].zerowidth.as_ref().map(Vec::len), Some(1));
        assert_eq!(row[2].c, 'y');
    }

    #[test]
    fn a_cancelled_sequence_does_not_hide_the_marks_that_follow() {
        // vte ends an OSC at CAN/SUB; so must the limit, or the marks after it are never counted.
        for end in [0x18u8, 0x1a] {
            let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
            let mut bytes = b"\x1b]0;t".to_vec();
            bytes.push(end);
            bytes.extend_from_slice(format!("e{}", "\u{301}".repeat(1000)).as_bytes());
            feed(&mut emu, &bytes);
            let mut row = vec![];
            emu.row(0, &mut row);
            assert_eq!(row[0].zerowidth.as_ref().map(Vec::len), Some(MAX_COMBINING), "{end:#x}");
        }
        // The same after a CSI cut short by an ASCII byte that follows a lead byte.
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
        let mut bytes = b"\x1b[1;\xc3m".to_vec();
        bytes.extend_from_slice(format!("e{}", "\u{301}".repeat(1000)).as_bytes());
        feed(&mut emu, &bytes);
        let mut row = vec![];
        emu.row(0, &mut row);
        let last = row.iter().find(|c| c.c == 'e').expect("an e");
        assert_eq!(last.zerowidth.as_ref().map(Vec::len), Some(MAX_COMBINING));
    }

    #[test]
    fn a_string_that_never_ends_is_not_buffered_without_end() {
        let mut m = MarkLimit::default();
        let mut big = b"\x1b]52;c;".to_vec();
        big.extend(std::iter::repeat_n(b'A', 3_000_000));
        let out = m.filter(&big).into_owned();
        assert!(out.len() <= MAX_STRING + 16, "{}", out.len());
        // Still inside it on the next read, still dropping; the terminator passes.
        let out = m.filter(&[b'B'; 1000]).into_owned();
        assert!(out.is_empty());
        let out = m.filter(b"\x07ok").into_owned();
        assert_eq!(out, b"\x07ok");
        // Short ones are untouched, and so is ordinary output.
        let mut m = MarkLimit::default();
        assert_eq!(m.filter(b"\x1b]0;title\x07 hi").as_ref(), b"\x1b]0;title\x07 hi");
        // An OSC 8 hyperlink longer than the cap on a line is cut, the text after it is not.
        let mut m = MarkLimit::default();
        let mut link = b"\x1b]8;;".to_vec();
        link.extend(std::iter::repeat_n(b'x', 3_000_000));
        link.extend_from_slice(b"\x1b\\text");
        let out = m.filter(&link).into_owned();
        assert!(out.ends_with(b"\x1b\\text") && out.len() < MAX_STRING + 100, "{}", out.len());
    }

    #[test]
    fn titles_are_clipped_and_osc7_must_be_an_absolute_path() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
        feed(&mut emu, format!("\x1b]0;{}\x07", "é".repeat(2000)).as_bytes());
        assert!(emu.title().unwrap().len() <= 512);
        assert!(emu.title().unwrap().chars().all(|c| c == 'é'));
        let out = feed(&mut emu, b"\x1b]7;file://host/tmp/x%20y\x07");
        assert!(
            out.events
                .iter()
                .any(|e| matches!(e, TermEvent::Cwd(p) if p.as_os_str() == "/tmp/x y"))
        );
        let out = feed(&mut emu, b"\x1b]7;file://host/tmp/a%00b\x07");
        assert!(!out.events.iter().any(|e| matches!(e, TermEvent::Cwd(_))));
    }

    #[test]
    fn combining_marks_per_cell_are_capped() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
        let text = format!("e{}", "\u{301}".repeat(1000));
        feed(&mut emu, text.as_bytes());
        let mut row = vec![];
        emu.row(0, &mut row);
        assert_eq!(row[0].zerowidth.as_ref().map(Vec::len), Some(MAX_COMBINING));
    }

    #[test]
    fn xtgettcap_never_echoes_non_hex() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 4), 100);
        let r = feed(&mut emu, b"\x1bP+qrm -rf ~\r;544e\x1b\\").replies;
        let r = String::from_utf8(r).unwrap();
        assert!(r.contains("\x1bP0+r\x1b\\"), "{r:?}");
        assert!(!r.contains("rm") && !r.contains('\r'), "{r:?}");
        assert!(r.contains("\x1bP1+r544e="), "{r:?}");
    }

    #[test]
    fn notifications_drop_controls_and_are_bounded() {
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 3), 100);
        let long = "x".repeat(5000);
        let ev = feed(&mut emu, format!("\x1b]9;a\u{9b}2Jb\x7f\x07\x1b]777;{long}\x07").as_bytes())
            .events;
        assert_eq!(ev[0], TermEvent::Notification("9;a2Jb".into()));
        let TermEvent::Notification(n) = &ev[1] else { panic!("{ev:?}") };
        assert_eq!(n.chars().count(), 1024);
    }

    #[test]
    fn prompt_mark_closes_only_outside_sequences() {
        // The newline inside the OSC title must not get the mark's own OSC spliced in (that
        // would end the title early and print the rest).
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 3), 100);
        feed(&mut emu, b"\x1b]133;A\x07$ \x1b]2;t\nitle\x07x\r\ny");
        assert_eq!(emu.title(), Some("title"));
        let mut row = vec![];
        emu.row(0, &mut row);
        assert_eq!(row.iter().map(|c| c.c).collect::<String>().trim_end(), "$ x");
        assert!(row[0].prompt && !row[3].prompt);
        emu.row(1, &mut row);
        assert_eq!(row[0].c, 'y');
        assert!(!row[0].prompt);
        // A sequence split across reads: the newline in its second half isn't ground either.
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 3), 100);
        feed(&mut emu, b"\x1b]133;A\x07$ \x1b]2;a");
        feed(&mut emu, b"\nb\x07z\r\n");
        assert_eq!(emu.title(), Some("ab"));
    }
}

// ── handoff images (SPEC §4.5, M8.1) ─────────────────────────────────────────

/// An emulator's state, for a live upgrade: both screens with their scrollback (alacritty's own
/// serde), the modes, the cursor and the title. Versioned by [`EMU_IMAGE_VERSION`]. What isn't
/// kept: a half-parsed escape sequence, the scroll region, tab stops, charsets and the palette.
/// The daemon asks each program to redraw after a handoff (SIGWINCH), which restores those.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmuImage {
    pub version: u32,
    pub size: TermSize,
    pub scrollback: usize,
    /// The primary screen and its history.
    pub primary: alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
    /// The alternate screen, when it is the one showing.
    pub alt: Option<alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>>,
    /// `TermMode` bits.
    pub mode: u32,
    pub cursor: (i32, usize),
    pub cursor_wrap: bool,
    pub cursor_template: alacritty_terminal::term::cell::Cell,
    /// The primary screen's DECSC cursor (where leaving the alternate screen returns to).
    pub saved_cursor: (i32, usize),
    /// DECSCUSR: `(shape, blinking)` with shape 0 block, 1 underline, 2 bar, 3 hidden.
    pub cursor_style: (u8, bool),
    pub title: Option<String>,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

pub const EMU_IMAGE_VERSION: u32 = 1;

/// Modes a fresh terminal has on (alacritty's defaults).
fn default_mode() -> TermMode {
    TermMode::SHOW_CURSOR
        | TermMode::LINE_WRAP
        | TermMode::ALTERNATE_SCROLL
        | TermMode::URGENCY_HINTS
}

/// The escape sequences that set `mode` on a fresh terminal (kitty flags excluded).
fn mode_sequences(mode: TermMode) -> String {
    let d = default_mode();
    let mut s = String::new();
    let mut private = |flag: TermMode, n: u16| {
        let on = mode.contains(flag);
        if on != d.contains(flag) {
            s.push_str(&format!("\x1b[?{n}{}", if on { 'h' } else { 'l' }));
        }
    };
    private(TermMode::APP_CURSOR, 1);
    private(TermMode::ORIGIN, 6);
    private(TermMode::LINE_WRAP, 7);
    private(TermMode::SHOW_CURSOR, 25);
    private(TermMode::MOUSE_REPORT_CLICK, 1000);
    private(TermMode::MOUSE_DRAG, 1002);
    private(TermMode::MOUSE_MOTION, 1003);
    private(TermMode::FOCUS_IN_OUT, 1004);
    private(TermMode::SGR_MOUSE, 1006);
    private(TermMode::ALTERNATE_SCROLL, 1007);
    private(TermMode::URGENCY_HINTS, 1042);
    private(TermMode::BRACKETED_PASTE, 2004);
    if mode.contains(TermMode::APP_KEYPAD) {
        s.push_str("\x1b=");
    }
    if mode.contains(TermMode::INSERT) {
        s.push_str("\x1b[4h");
    }
    if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
        s.push_str("\x1b[20h");
    }
    s
}

fn kitty_flags(mode: TermMode) -> u8 {
    let mut f = 0;
    for (i, flag) in [
        TermMode::DISAMBIGUATE_ESC_CODES,
        TermMode::REPORT_EVENT_TYPES,
        TermMode::REPORT_ALTERNATE_KEYS,
        TermMode::REPORT_ALL_KEYS_AS_ESC,
        TermMode::REPORT_ASSOCIATED_TEXT,
    ]
    .into_iter()
    .enumerate()
    {
        if mode.contains(flag) {
            f |= 1 << i;
        }
    }
    f
}

impl AlacrittyEmulator {
    /// Capture everything a live upgrade carries over. Leaves the emulator as it was.
    pub fn image(&mut self) -> EmuImage {
        let mode = *self.term.mode();
        let cursor = self.term.grid().cursor.clone();
        let alt_on = mode.contains(TermMode::ALT_SCREEN);
        let (primary, alt) = if alt_on {
            let alt = self.term.grid().clone();
            // To the primary screen and back; returning resets the alternate screen, so put it
            // (and its cursor) back by hand.
            self.term.swap_alt();
            let primary = self.term.grid().clone();
            self.term.swap_alt();
            *self.term.grid_mut() = alt.clone();
            self.term.grid_mut().cursor = cursor.clone();
            (primary, Some(alt))
        } else {
            (self.term.grid().clone(), None)
        };
        let saved = primary.saved_cursor.point;
        let style = self.term.cursor_style();
        let shape = match style.shape {
            ACursorShape::Underline => 1,
            ACursorShape::Beam => 2,
            ACursorShape::Hidden => 3,
            _ => 0,
        };
        EmuImage {
            version: EMU_IMAGE_VERSION,
            size: self.size,
            scrollback: primary.history_size().max(self.scrollback_limit()),
            primary,
            alt,
            mode: mode.bits(),
            cursor: (cursor.point.line.0, cursor.point.column.0),
            cursor_wrap: cursor.input_needs_wrap,
            cursor_template: cursor.template,
            saved_cursor: (saved.line.0, saved.column.0),
            cursor_style: (shape, style.blinking),
            title: self.title.clone(),
            fg: [self.fg.r, self.fg.g, self.fg.b],
            bg: [self.bg.r, self.bg.g, self.bg.b],
        }
    }

    /// History lines kept at most.
    pub fn scrollback_limit(&self) -> usize {
        self.scrollback
    }

    /// Rebuild an emulator from an image.
    pub fn from_image(img: EmuImage) -> Self {
        let mut emu = Self::new(img.size, img.scrollback);
        let mode = TermMode::from_bits_truncate(img.mode);
        let mut sink = FeedOutput::default();
        *emu.term.grid_mut() = img.primary;
        let primary_saved = alacritty_terminal::index::Point::new(
            Line(img.saved_cursor.0),
            Column(img.saved_cursor.1),
        );
        emu.term.grid_mut().saved_cursor.point = primary_saved;
        if let Some(alt) = img.alt {
            // Entering the alternate screen saves the cursor: make it the one to come back to.
            emu.term.grid_mut().cursor.point = primary_saved;
            emu.feed(b"\x1b[?1049h", &mut sink);
            *emu.term.grid_mut() = alt;
        }
        // Modes, then the keyboard protocol (pushed, so the program's own pop still works).
        emu.feed(mode_sequences(mode).as_bytes(), &mut sink);
        let kitty = kitty_flags(mode);
        if kitty != 0 {
            emu.feed(format!("\x1b[>{kitty}u").as_bytes(), &mut sink);
        }
        let (shape, blink) = img.cursor_style;
        let n = match (shape, blink) {
            (1, true) => 3,
            (1, false) => 4,
            (2, true) => 5,
            (2, false) => 6,
            (_, true) => 1,
            _ => 2,
        };
        if shape != 0 || blink {
            emu.feed(format!("\x1b[{n} q").as_bytes(), &mut sink);
        }
        // An image from before sizes were bounded: fit its grids to the clamped size.
        if img.size != emu.size {
            emu.term.resize(Dims(emu.size));
        }
        let g = emu.term.grid_mut();
        let lines = g.screen_lines() as i32;
        let cols = g.columns();
        g.cursor.point = alacritty_terminal::index::Point::new(
            Line(img.cursor.0.clamp(0, (lines - 1).max(0))),
            Column(img.cursor.1.min(cols.saturating_sub(1))),
        );
        g.cursor.input_needs_wrap = img.cursor_wrap;
        g.cursor.template = img.cursor_template;
        emu.title = img.title;
        emu.fg = Rgb { r: img.fg[0], g: img.fg[1], b: img.fg[2] };
        emu.bg = Rgb { r: img.bg[0], g: img.bg[1], b: img.bg[2] };
        emu
    }
}
