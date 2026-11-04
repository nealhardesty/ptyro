//! What we learn about the user's real terminal at startup: its default colours (OSC 10/11), so
//! the daemon can answer programs that pick a theme from them (SPEC §5.6). Codex and Gemini do.
//! Keys typed while we wait for the answer arrive in the same reads: they are kept and replayed.

#[cfg(unix)]
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ptyro_protocol::OuterTerm;

/// What we ask: both default colours, then the device attributes, which every terminal answers.
const QUERY: &[u8] = b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c";

/// Must run in raw mode and before crossterm's event reader starts. The reply to the DA1 query
/// we append marks the end, so terminals that ignore OSC 10/11 cost one round trip, not a timeout.
/// Also returns the keys typed meanwhile, for the caller to replay.
#[cfg(unix)]
pub fn query() -> (OuterTerm, Vec<KeyEvent>) {
    let mut out = std::io::stdout();
    if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
        return (OuterTerm::default(), Vec::new());
    }
    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let mut reply = Vec::new();
    let mut buf = [0u8; 512];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 50_000_000 };
        if rustix::event::poll(&mut fds, Some(&timeout)).unwrap_or(0) == 0 {
            continue;
        }
        let Ok(n) = stdin.lock().read(&mut buf) else { break };
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&buf[..n]);
        if let Some(p) = find(&reply, b"\x1b[?")
            && reply[p..].contains(&b'c')
        {
            break;
        }
    }
    let outer = OuterTerm {
        fg: color(&reply, b"\x1b]10;"),
        bg: color(&reply, b"\x1b]11;"),
        kitty_keyboard: false,
    };
    (outer, typed(&reply))
}

/// Windows: a console has no descriptor to poll, and the answers come in as key presses, one per
/// character with no modifiers and the ESCs dropped (Windows Terminal, seen 2026-11-03, SPEC
/// §20.11): `]10;rgb:cccc/cccc/cccc\` `]11;rgb:0c0c/0c0c/0c0c\` `[?61;4;6;…c`. crossterm's own
/// reader takes them, and [`console_replies`] tells them from the keys typed meanwhile.
#[cfg(windows)]
pub fn query() -> (OuterTerm, Vec<KeyEvent>) {
    use crossterm::event::{Event, poll, read};
    let mut out = std::io::stdout();
    if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
        return (OuterTerm::default(), Vec::new());
    }
    let mut keys = Vec::new();
    let start = Instant::now();
    while let Some(left) = Duration::from_millis(300).checked_sub(start.elapsed()) {
        if !poll(left).unwrap_or(false) {
            break;
        }
        match read() {
            Ok(Event::Key(k)) => keys.push(k),
            Ok(_) => {}
            Err(_) => break,
        }
        if console_replies(&keys).answered {
            break;
        }
    }
    let r = console_replies(&keys);
    (OuterTerm { fg: r.fg, bg: r.bg, kitty_keyboard: false }, r.typed)
}

/// What [`console_replies`] found among a Windows console's key presses.
#[cfg(any(windows, test))]
struct ConsoleReplies {
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
    /// The device attributes answer came, so nothing more is on its way.
    answered: bool,
    typed: Vec<KeyEvent>,
}

/// Picks the terminal's answers out of the key presses (see the Windows [`query`]); every other
/// key, a lone `]` or `[` included, is the user's.
#[cfg(any(windows, test))]
fn console_replies(keys: &[KeyEvent]) -> ConsoleReplies {
    use crossterm::event::KeyEventKind;
    let ch = |i: usize| match keys.get(i) {
        Some(k) if k.kind == KeyEventKind::Press && k.modifiers.is_empty() => match k.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        },
        _ => None,
    };
    // The length of the run from `i` whose characters all pass `ok`.
    let run =
        |i: usize, ok: &dyn Fn(char) -> bool| (i..).take_while(|&j| ch(j).is_some_and(ok)).count();
    let mut r = ConsoleReplies { fg: None, bg: None, answered: false, typed: Vec::new() };
    let mut i = 0;
    while i < keys.len() {
        let osc = (ch(i) == Some(']') && ch(i + 1) == Some('1') && ch(i + 3) == Some(';'))
            .then(|| ch(i + 2))
            .flatten()
            .filter(|n| matches!(n, '0' | '1'));
        if let Some(n) = osc {
            let len =
                run(i + 4, &|c| c.is_ascii_hexdigit() || matches!(c, 'r' | 'g' | 'b' | ':' | '/'));
            let body: String = (i + 4..i + 4 + len).filter_map(ch).collect();
            let rgb =
                color(format!("\x1b]1{n};{body}\x07").as_bytes(), format!("\x1b]1{n};").as_bytes());
            if let Some(rgb) = rgb {
                *(if n == '0' { &mut r.fg } else { &mut r.bg }) = Some(rgb);
                i += 4 + len;
                if ch(i) == Some('\\') {
                    i += 1;
                }
                continue;
            }
        }
        if ch(i) == Some('[') && ch(i + 1) == Some('?') {
            let len = run(i + 2, &|c| c.is_ascii_digit() || c == ';');
            if ch(i + 2 + len) == Some('c') {
                r.answered = true;
                i += 3 + len;
                continue;
            }
        }
        r.typed.push(keys[i]);
        i += 1;
    }
    r
}

/// The keys among the replies: OSC answers and `CSI ? … c` are dropped, the rest decoded as a
/// terminal without the kitty protocol sends them (it is turned on only after this).
fn typed(bytes: &[u8]) -> Vec<KeyEvent> {
    let key = |code, mods| KeyEvent::new(code, mods);
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        let k = match b {
            0x1b => match bytes.get(i) {
                None => key(KeyCode::Esc, KeyModifiers::NONE),
                // An OSC answer, up to BEL or ST.
                Some(b']') => {
                    while i < bytes.len() && bytes[i] != 0x07 && !bytes[i..].starts_with(b"\x1b\\")
                    {
                        i += 1;
                    }
                    i += if bytes.get(i) == Some(&0x07) { 1 } else { 2 };
                    continue;
                }
                Some(b'[') => {
                    let start = i + 1;
                    let Some(end) = bytes[start..].iter().position(|b| (0x40..=0x7e).contains(b))
                    else {
                        break;
                    };
                    let (params, fin) = (&bytes[start..start + end], bytes[start + end]);
                    i = start + end + 1;
                    match csi_key(params, fin) {
                        Some(k) => k,
                        // The device attributes answer, or a key we don't know.
                        None => continue,
                    }
                }
                Some(b'O') if i + 1 < bytes.len() => {
                    i += 2;
                    match csi_key(b"", bytes[i - 1]) {
                        Some(k) => k,
                        None => continue,
                    }
                }
                Some(_) => {
                    // Alt and the next key.
                    let mut rest = typed(&bytes[i..i + utf8_len(bytes[i]).min(bytes.len() - i)]);
                    i += utf8_len(bytes[i]).min(bytes.len() - i);
                    let Some(mut k) = rest.pop() else { continue };
                    k.modifiers |= KeyModifiers::ALT;
                    k
                }
            },
            b'\r' | b'\n' => key(KeyCode::Enter, KeyModifiers::NONE),
            b'\t' => key(KeyCode::Tab, KeyModifiers::NONE),
            0x7f | 0x08 => key(KeyCode::Backspace, KeyModifiers::NONE),
            0x00 => key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            0x01..=0x1a => key(KeyCode::Char((b - 1 + b'a') as char), KeyModifiers::CONTROL),
            0x1c..=0x1f => key(KeyCode::Char((b - 0x1c + b'4') as char), KeyModifiers::CONTROL),
            _ => {
                let len = utf8_len(b).min(bytes.len() - (i - 1));
                let s = std::str::from_utf8(&bytes[i - 1..i - 1 + len]).ok();
                i += len - 1;
                match s.and_then(|s| s.chars().next()) {
                    Some(c) => key(KeyCode::Char(c), KeyModifiers::NONE),
                    None => continue,
                }
            }
        };
        keys.push(k);
    }
    keys
}

/// A cursor or editing key: `CSI 1;5 A` (modifiers as 1 + shift 1, alt 2, ctrl 4) or `CSI 3 ~`.
fn csi_key(params: &[u8], fin: u8) -> Option<KeyEvent> {
    let params = std::str::from_utf8(params).ok()?;
    if params.starts_with(['?', '>', '<', '=']) {
        return None;
    }
    let mut nums = params.split(';').map(|p| p.parse::<u8>().unwrap_or(1));
    let first = nums.next().unwrap_or(1);
    let m = nums.next().unwrap_or(1).saturating_sub(1);
    let mut mods = KeyModifiers::NONE;
    for (bit, flag) in
        [(1, KeyModifiers::SHIFT), (2, KeyModifiers::ALT), (4, KeyModifiers::CONTROL)]
    {
        if m & bit != 0 {
            mods |= flag;
        }
    }
    let code = match (fin, first) {
        (b'A', _) => KeyCode::Up,
        (b'B', _) => KeyCode::Down,
        (b'C', _) => KeyCode::Right,
        (b'D', _) => KeyCode::Left,
        (b'H', _) | (b'~', 1 | 7) => KeyCode::Home,
        (b'F', _) | (b'~', 4 | 8) => KeyCode::End,
        (b'Z', _) => return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        (b'P'..=b'S', _) => KeyCode::F(fin - b'P' + 1),
        (b'~', 2) => KeyCode::Insert,
        (b'~', 3) => KeyCode::Delete,
        (b'~', 5) => KeyCode::PageUp,
        (b'~', 6) => KeyCode::PageDown,
        _ => return None,
    };
    Some(KeyEvent::new(code, mods))
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Parse `ESC ] 1x ; rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel).
fn color(reply: &[u8], prefix: &[u8]) -> Option<[u8; 3]> {
    let start = find(reply, prefix)? + prefix.len();
    let rest = reply[start..].strip_prefix(b"rgb:")?;
    let end = rest.iter().position(|&b| b == 0x07 || b == 0x1b).unwrap_or(rest.len());
    let s = std::str::from_utf8(&rest[..end]).ok()?;
    let mut parts = s.split('/').map(|p| {
        // 1 to 4 hex digits: more is not a colour (and `1 << 32` would overflow).
        if p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * p.len() as u32)) - 1;
        Some((v * 255 / max.max(1)) as u8)
    });
    Some([parts.next()??, parts.next()??, parts.next()??])
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_osc_colour_replies() {
        let r = b"\x1b]10;rgb:ffff/8080/0000\x1b\\\x1b]11;rgb:1c/1c/1c\x07\x1b[?62;22c";
        assert_eq!(color(r, b"\x1b]10;"), Some([255, 128, 0]));
        assert_eq!(color(r, b"\x1b]11;"), Some([0x1c, 0x1c, 0x1c]));
        assert_eq!(color(b"\x1b[?6c", b"\x1b]10;"), None);
        // Too many digits, a sign, nothing: no colour (and no overflow).
        assert_eq!(color(b"\x1b]10;rgb:ffffffff/00000000/00000000\x07", b"\x1b]10;"), None);
        assert_eq!(color(b"\x1b]10;rgb:+fff/0/0\x07", b"\x1b]10;"), None);
        assert_eq!(color(b"\x1b]10;rgb:/0/0\x07", b"\x1b]10;"), None);
    }

    #[test]
    fn windows_console_replies_are_told_from_typed_keys() {
        use crossterm::event::KeyEventKind;
        let press = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let keys = |s: &str| s.chars().map(press).collect::<Vec<_>>();
        // What Windows Terminal sent on 2026-11-03 (Campbell), ESCs already gone.
        let wt = "]10;rgb:cccc/cccc/cccc\\]11;rgb:0c0c/0c0c/0c0c\\[?61;4;6;7;14;21;22;23;24;28;32;42;52c";
        let r = console_replies(&keys(wt));
        assert_eq!((r.fg, r.bg, r.answered), (Some([0xcc; 3]), Some([0x0c; 3]), true));
        assert!(r.typed.is_empty());
        // Partway, the device attributes not in yet: keep waiting.
        assert!(!console_replies(&keys(&wt[..30])).answered);
        // Typed before, between and after, a Ctrl key and a release among them: all kept, in order.
        let mut ks = keys("a]");
        ks.push(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        ks.extend(keys("]11;rgb:1c/1c/1c\\"));
        ks.push(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        ks.extend(keys("[?1;0c[x"));
        let r = console_replies(&ks);
        assert_eq!((r.fg, r.bg, r.answered), (None, Some([0x1c; 3]), true));
        let typed: Vec<_> = r.typed.iter().map(|k| (k.code, k.modifiers, k.kind)).collect();
        assert_eq!(
            typed,
            vec![
                (KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Press),
                (KeyCode::Char(']'), KeyModifiers::NONE, KeyEventKind::Press),
                (KeyCode::Char('c'), KeyModifiers::CONTROL, KeyEventKind::Press),
                (KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Release),
                (KeyCode::Char('['), KeyModifiers::NONE, KeyEventKind::Press),
                (KeyCode::Char('x'), KeyModifiers::NONE, KeyEventKind::Press),
            ]
        );
        // A terminal that ignores OSC 10/11 answers only the attributes.
        let r = console_replies(&keys("[?1;0c"));
        assert_eq!((r.fg, r.bg, r.answered, r.typed.len()), (None, None, true, 0));
    }

    #[test]
    fn keys_typed_among_the_replies_are_kept() {
        let none = KeyModifiers::NONE;
        let k = KeyEvent::new;
        let r = "ls\x1b]10;rgb:ffff/8080/0000\x1b\\é\x1b]11;rgb:1c/1c/1c\x07\r\x1b[?62;22c\x1b[1;5A\x7f\x01\x1bx\x1b[3~\x1bOP\x1b";
        assert_eq!(
            typed(r.as_bytes()),
            vec![
                k(KeyCode::Char('l'), none),
                k(KeyCode::Char('s'), none),
                k(KeyCode::Char('é'), none),
                k(KeyCode::Enter, none),
                k(KeyCode::Up, KeyModifiers::CONTROL),
                k(KeyCode::Backspace, none),
                k(KeyCode::Char('a'), KeyModifiers::CONTROL),
                k(KeyCode::Char('x'), KeyModifiers::ALT),
                k(KeyCode::Delete, none),
                k(KeyCode::F(1), none),
                k(KeyCode::Esc, none),
            ]
        );
        // Replies alone: nothing typed.
        assert!(
            typed(b"\x1b]10;rgb:ffff/8080/0000\x1b\\\x1b]11;rgb:1c/1c/1c\x07\x1b[?6c").is_empty()
        );
    }
}
