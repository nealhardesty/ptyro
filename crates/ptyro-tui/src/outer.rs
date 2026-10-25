//! What we learn about the user's real terminal at startup: its default colours (OSC 10/11), so
//! the daemon can answer programs that pick a theme from them (SPEC §5.6). Codex and Gemini do.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use ptyro_protocol::OuterTerm;

/// Must run in raw mode and before crossterm's event reader starts. The reply to the DA1 query
/// we append marks the end, so terminals that ignore OSC 10/11 cost one round trip, not a timeout.
pub fn query() -> OuterTerm {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c").and_then(|()| out.flush()).is_err() {
        return OuterTerm::default();
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
    OuterTerm {
        fg: color(&reply, b"\x1b]10;"),
        bg: color(&reply, b"\x1b]11;"),
        kitty_keyboard: false,
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
}
