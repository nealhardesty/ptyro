//! What we learn about the user's real terminal at startup: its default colours (OSC 10/11), so
//! the daemon can answer programs that pick a theme from them (SPEC §5.6). Codex does. And
//! whether a copy reaches its clipboard (OSC 52), so a copy's message can say where it went.
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
/// Also asked, first, where it is safe: XTGETTCAP for `Ms`, the capability that sets the clipboard
/// (OSC 52). The Linux console reads `ESC P` as a palette change, so it isn't asked there.
const ASK_MS: &[u8] = b"\x1bP+q4d73\x1b\\";

/// Whether a copy (OSC 52) reaches the clipboard of the terminal the TUI runs in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Clipboard {
    /// The terminal said it takes clipboard writes (XTGETTCAP `Ms`, `52` in its device
    /// attributes, or tmux's `clipboard` feature for the terminal outside it).
    Yes,
    /// Certainly not, with why and the fix: tmux drops the passthrough the copy travels in.
    No(String),
    /// Nothing said either way: many terminals that take clipboard writes don't say so.
    #[default]
    Unknown,
}

/// What the replies to [`QUERY`] (and [`ASK_MS`]) say about the clipboard: only a yes. A terminal
/// answering `0+r` (no `Ms`) may still take OSC 52, so that stays unknown.
pub fn clipboard_from(reply: &[u8]) -> Clipboard {
    if find(reply, b"\x1bP1+r4d73").is_some() {
        return Clipboard::Yes;
    }
    // `CSI ? 6x ; … c`: `52` is clipboard access (xterm's list).
    if let Some(p) = find(reply, b"\x1b[?") {
        let rest = &reply[p + 3..];
        if let Some(end) = rest.iter().position(|&b| b == b'c')
            && rest[..end].split(|&b| b == b';').any(|v| v == b"52")
        {
            return Clipboard::Yes;
        }
    }
    Clipboard::Unknown
}

/// Inside tmux the answer is tmux's: the copy travels in its passthrough (`allow-passthrough`;
/// off drops it), and tmux knows whether the terminal outside it takes the clipboard (its
/// `clipboard` feature). `show` is `tmux show -gv allow-passthrough`, `features` is `tmux display
/// -p '#{client_termfeatures}'`; `None` where tmux didn't answer.
pub fn clipboard_in_tmux(show: Option<&str>, features: Option<&str>) -> Clipboard {
    if show.map(str::trim) == Some("off") {
        return Clipboard::No(
            "tmux drops it (allow-passthrough is off): add `set -g allow-passthrough on` to \
             ~/.tmux.conf"
                .into(),
        );
    }
    if features.is_some_and(|f| f.trim().split(',').any(|x| x == "clipboard")) {
        return Clipboard::Yes;
    }
    Clipboard::Unknown
}

/// Ask tmux (see [`clipboard_in_tmux`]).
pub fn tmux_clipboard() -> Clipboard {
    let ask = |args: &[&str]| {
        std::process::Command::new("tmux")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let show = ask(&["show", "-gv", "allow-passthrough"]);
    let features = ask(&["display", "-p", "#{client_termfeatures}"]);
    clipboard_in_tmux(show.as_deref(), features.as_deref())
}

/// Must run in raw mode and before crossterm's event reader starts. The reply to the DA1 query
/// we append marks the end, so terminals that ignore OSC 10/11 cost one round trip, not a timeout.
/// Also returns the keys typed meanwhile, for the caller to replay.
#[cfg(unix)]
pub fn query() -> (OuterTerm, Clipboard, Vec<KeyEvent>) {
    let tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
    let console = std::env::var("TERM").is_ok_and(|t| t == "linux");
    let mut out = std::io::stdout();
    let ask = if tmux || console { Vec::new() } else { ASK_MS.to_vec() };
    if out.write_all(&[ask.as_slice(), QUERY].concat()).and_then(|()| out.flush()).is_err() {
        return (OuterTerm::default(), Clipboard::Unknown, Vec::new());
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
    let clipboard = if tmux {
        tmux_clipboard()
    } else if console {
        Clipboard::No("the Linux console has no clipboard".into())
    } else {
        clipboard_from(&reply)
    };
    (outer, clipboard, typed(&reply))
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
                // A DCS answer (XTGETTCAP's), up to ST.
                Some(b'P') => {
                    while i < bytes.len() && !bytes[i..].starts_with(b"\x1b\\") {
                        i += 1;
                    }
                    i += 2;
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
    fn a_clipboard_is_known_only_when_the_terminal_says_so() {
        let ms = b"\x1bP1+r4d73=\x1b\\\x1b[?62;22c";
        assert_eq!(clipboard_from(ms), Clipboard::Yes);
        assert_eq!(clipboard_from(b"\x1b[?64;1;4;52;22c"), Clipboard::Yes);
        // No `Ms` in its table, or nothing said: maybe, so unknown, never a no.
        assert_eq!(clipboard_from(b"\x1bP0+r4d73\x1b\\\x1b[?62;22c"), Clipboard::Unknown);
        assert_eq!(clipboard_from(b"\x1b[?62;22c"), Clipboard::Unknown);
        assert_eq!(clipboard_from(b"\x1b[?6c"), Clipboard::Unknown);
        assert_eq!(clipboard_from(b"\x1b[?62;152c"), Clipboard::Unknown);
    }

    #[test]
    fn inside_tmux_its_passthrough_and_features_decide() {
        assert!(matches!(clipboard_in_tmux(Some("off\n"), Some("clipboard")), Clipboard::No(_)));
        let feats = Some("256,bpaste,clipboard,ccolour\n");
        assert_eq!(clipboard_in_tmux(Some("on\n"), feats), Clipboard::Yes);
        assert_eq!(clipboard_in_tmux(Some("on"), Some("256,bpaste")), Clipboard::Unknown);
        assert_eq!(clipboard_in_tmux(None, None), Clipboard::Unknown);
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
        // XTGETTCAP's answer is no keys either (not Alt-P and its text).
        assert_eq!(
            typed(b"\x1bP1+r4d73=\x1b\\x\x1bP0+r4d73\x1b\\"),
            vec![k(KeyCode::Char('x'), none)]
        );
    }
}
