//! A scriptable stand-in for a coding agent (SPEC §18.2), run as `ptyro fake-agent`.
//!
//! At startup it probes for OSC 7501 the way Claude Code does (probe, then DA1; a DA1 reply that
//! arrives first means "unsupported"), so it also tests the daemon's query responder end to end.
//! Then it reads commands, one per line:
//!
//! | command | effect |
//! |---|---|
//! | `ask <question>` | reports `blocked/question`, waits for a `y`/`n` line |
//! | `work <seconds>` | reports `working`, sleeps, prints `worked`, reports `done` |
//! | `alt` | draws on the alternate screen until the next line |
//! | `title <text>` / `bell` / `notify <text>` | OSC 0 / BEL / OSC 9 |
//! | `env <VAR>` | prints the variable |
//! | `size` | prints `size COLSxROWS` |
//! | `exit <code>` | reports `clear` and exits |
//! | anything else | echoes `you said: …` |

use std::io::{BufRead, Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

pub fn main(args: &[String]) -> i32 {
    if let Some(code) = crate::fake_structured::main(args) {
        return code;
    }
    if args.iter().any(|a| a == "--as") {
        return crate::fake_adapters::main(args);
    }
    let quiet = args.iter().any(|a| a == "--no-probe");
    let stdin = std::io::stdin();
    let psp = !quiet && probe_psp(&stdin);
    let mut out = std::io::stdout();
    let report = |out: &mut std::io::Stdout, pairs: &str| {
        if psp {
            let _ = write!(out, "\x1b]7501;{pairs}:app=fake-agent\x1b\\");
            // A report must leave now, not when the next line flushes stdout.
            let _ = out.flush();
        }
    };
    let _ = writeln!(
        out,
        "\x1b[1;32mfake-agent\x1b[0m ready (osc7501={})",
        if psp { "yes" } else { "no" }
    );
    let _ = writeln!(
        out,
        "\x1b[31mred\x1b[0m \x1b[38;5;208morange\x1b[0m \x1b[38;2;0;128;255mrgb\x1b[0m 漢字"
    );
    report(&mut out, "state=idle");
    let mut lines = stdin.lock().lines();
    loop {
        let _ = write!(out, "> ");
        let _ = out.flush();
        let Some(Ok(line)) = lines.next() else { return 0 };
        let (cmd, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        match cmd {
            "ask" => {
                report(&mut out, "state=blocked:kind=question");
                let _ = write!(out, "{rest} [y/n] ");
                let _ = out.flush();
                let answer = lines.next().and_then(Result::ok).unwrap_or_default();
                let _ = writeln!(out, "answer: {}", answer.trim());
                report(&mut out, "state=idle");
            }
            "work" => {
                report(&mut out, "state=working:msg=d29ya2luZw");
                std::thread::sleep(Duration::from_secs_f64(rest.parse().unwrap_or(1.0)));
                let _ = writeln!(out, "worked");
                report(&mut out, "state=done");
            }
            "alt" => {
                let _ = write!(out, "\x1b[?1049h\x1b[2J\x1b[H┌──────┐\r\n│ alt  │\r\n└──────┘\r\n");
                let _ = out.flush();
                let _ = lines.next();
                let _ = write!(out, "\x1b[?1049l");
            }
            "title" => {
                let _ = write!(out, "\x1b]0;{rest}\x07");
            }
            "bell" => {
                let _ = write!(out, "\x07");
            }
            "notify" => {
                let _ = write!(out, "\x1b]9;{rest}\x07");
            }
            "env" => {
                let _ = writeln!(out, "{rest}={}", std::env::var(rest).unwrap_or_default());
            }
            "size" => {
                let size = rustix::termios::tcgetwinsize(std::io::stdout().as_fd())
                    .map(|w| format!("{}x{}", w.ws_col, w.ws_row))
                    .unwrap_or_else(|_| "unknown".into());
                let _ = writeln!(out, "size {size}");
            }
            // OSC 7501 conformance (rev 0.4): what a terminal must discard, and records by id.
            "conformance" => {
                let raw = |out: &mut std::io::Stdout, body: &str| {
                    let _ = write!(out, "\x1b]7501;{body}\x1b\\");
                    let _ = out.flush();
                };
                if psp {
                    raw(&mut out, "state=working:id=a//b"); // invalid id: ignored whole
                    raw(&mut out, "state=blocked:msg=YQpi"); // "a\nb": control character
                    raw(&mut out, "state=working:id=build:app=make:msg=Y29tcGlsaW5n"); // "compiling"
                    raw(&mut out, "state=working:id=build/test"); // inherits app=make
                    raw(&mut out, "state=clear:id=build"); // the subtree goes
                    raw(&mut out, "state=working:id=lint:msg=bGludGluZw"); // "linting"
                }
                let _ = writeln!(out, "conformance sent");
            }
            "exit" => {
                report(&mut out, "state=clear");
                let _ = out.flush();
                return rest.parse().unwrap_or(0);
            }
            "" => {}
            _ => {
                let _ = writeln!(out, "you said: {}", line.trim());
            }
        }
    }
}

/// Probe OSC 7501 then DA1 in raw mode; supported iff the 7501 reply arrives before DA1's.
fn probe_psp(stdin: &std::io::Stdin) -> bool {
    use rustix::termios::{self, LocalModes, OptionalActions};
    let fd = stdin.as_fd();
    let Ok(saved) = termios::tcgetattr(fd) else { return false };
    let mut raw = saved.clone();
    raw.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO);
    if termios::tcsetattr(fd, OptionalActions::Now, &raw).is_err() {
        return false;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b]7501;?\x1b\\\x1b[c");
    let _ = out.flush();
    let mut reply = Vec::new();
    let start = Instant::now();
    let mut buf = [0u8; 256];
    let mut lock = stdin.lock();
    while start.elapsed() < Duration::from_secs(2) {
        let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 100_000_000 };
        if rustix::event::poll(&mut fds, Some(&timeout)).unwrap_or(0) == 0 {
            continue;
        }
        let Ok(n) = lock.read(&mut buf) else { break };
        reply.extend_from_slice(&buf[..n]);
        // DA1 reply: ESC [ ? ... c
        if let Some(pos) = find(&reply, b"\x1b[?")
            && reply[pos..].contains(&b'c')
        {
            break;
        }
    }
    drop(lock);
    let _ = termios::tcsetattr(fd, OptionalActions::Now, &saved);
    match (find(&reply, b"\x1b]7501;?"), find(&reply, b"\x1b[?")) {
        (Some(p), Some(d)) => p < d,
        (Some(_), None) => true,
        _ => false,
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
