//! Run a program in ptyro's PTY + emulator with scripted input and record what it shows: the
//! engine behind `ptyro-headless` and the TUI tests (which run `ptyro` itself inside it).

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable, TermEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct Script {
    pub argv: Vec<String>,
    pub size: TermSize,
    pub timeout: Duration,
    /// (delay since start, bytes)
    pub sends: Vec<(Duration, Vec<u8>)>,
    pub resizes: Vec<(Duration, TermSize)>,
    pub snapshots: Vec<Duration>,
    pub record: Option<PathBuf>,
    /// Snapshots and the final screen with SGR colours.
    pub ansi: bool,
    /// Extra variables for the program (on top of ptyro's child environment policy).
    pub env: Vec<(String, String)>,
    /// Stop as soon as the screen contains this text (after the last send).
    pub until: Option<String>,
}

impl Script {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            size: TermSize::new(120, 40),
            timeout: Duration::from_secs(5),
            sends: vec![],
            resizes: vec![],
            snapshots: vec![],
            record: None,
            ansi: false,
            env: vec![],
            until: None,
        }
    }

    /// Send `text` (with `\r \e \xHH` escapes) at `secs` after start.
    pub fn send(mut self, secs: f64, text: &str) -> Self {
        self.sends.push((Duration::from_secs_f64(secs), unescape(text).expect("valid escapes")));
        self
    }

    pub fn snapshot(mut self, secs: f64) -> Self {
        self.snapshots.push(Duration::from_secs_f64(secs));
        self
    }

    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.size = TermSize::new(cols, rows);
        self
    }

    pub fn timeout(mut self, secs: f64) -> Self {
        self.timeout = Duration::from_secs_f64(secs);
        self
    }

    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub snapshots: Vec<(Duration, String)>,
    pub screen: String,
    pub exit: Option<i32>,
    pub title: Option<String>,
    pub modes: String,
    /// OSC 7501 statuses, notifications, bells… with timestamps.
    pub log: Vec<String>,
    pub elapsed: Duration,
}

impl Report {
    /// The snapshot taken closest after `secs`.
    pub fn at(&self, secs: f64) -> &str {
        let t = Duration::from_secs_f64(secs);
        self.snapshots.iter().find(|(d, _)| *d >= t).map_or("", |(_, s)| s.as_str())
    }
}

/// `\r \n \t \e \\ \xHH` escapes, so scripted keystrokes fit on a command line.
pub fn unescape(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match it.next() {
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'e') => out.push(0x1b),
            Some(b'\\') => out.push(b'\\'),
            Some(b'x') => {
                let h: Vec<u8> = it.by_ref().take(2).collect();
                out.push(u8::from_str_radix(std::str::from_utf8(&h)?, 16)?);
            }
            other => bail!("unknown escape \\{}", other.map_or(' ', char::from)),
        }
    }
    Ok(out)
}

fn render(emu: &AlacrittyEmulator, tracker: &mut FrameTracker, ansi: bool) -> String {
    let mut cache = ScreenCache::new(emu.size());
    cache.apply(true, tracker.seq(), &tracker.build(emu, None, &mut StyleTable::default()));
    if ansi { cache.ansi_text() } else { cache.plain_text() }
}

async fn until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
    }
}

pub async fn run(mut s: Script) -> Result<Report> {
    s.sends.sort_by_key(|(t, _)| *t);
    s.resizes.sort_by_key(|(t, _)| *t);
    s.snapshots.sort();
    let extra: Vec<(String, std::ffi::OsString)> =
        s.env.iter().map(|(k, v)| (k.clone(), v.into())).collect();
    let env = ptyro_pty::child_env(
        std::env::vars_os(),
        extra.iter().map(|(k, v)| (k.as_str(), v.as_os_str())),
    );
    let cwd = std::env::current_dir()?;
    let mut child =
        ptyro_pty::spawn(ptyro_pty::SpawnRequest { argv: &s.argv, cwd: &cwd, env, size: s.size })?;
    let mut emu = AlacrittyEmulator::new(s.size, 1000);
    let mut tracker = FrameTracker::new(&mut emu);
    let mut record = match &s.record {
        Some(p) => {
            Some(std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?)
        }
        None => None,
    };
    let start = Instant::now();
    let deadline = start + s.timeout;
    let last_send = s.sends.last().map_or(Duration::ZERO, |(t, _)| *t);
    let mut sends = s.sends.into_iter().peekable();
    let mut resizes = s.resizes.into_iter().peekable();
    let mut snapshots = s.snapshots.into_iter().peekable();
    let mut report = Report::default();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let next_send = sends.peek().map(|(d, _)| start + *d);
        let next_resize = resizes.peek().map(|(d, _)| start + *d);
        let next_snap = snapshots.peek().map(|d| start + *d);
        let mut out = FeedOutput::default();
        tokio::select! {
            r = child.reader.read(&mut buf) => match r {
                Ok(n @ 1..) => {
                    if let Some(f) = record.as_mut() {
                        f.write_all(&buf[..n])?;
                    }
                    emu.feed(&buf[..n], &mut out);
                }
                _ => {
                    report.exit = Some(child.child.wait().await?.code().unwrap_or(-1));
                    break;
                }
            },
            () = until(next_send) => {
                let (_, bytes) = sends.next().expect("peeked");
                child.writer.write_all(&bytes).await?;
            },
            () = until(next_resize) => {
                let (_, size) = resizes.next().expect("peeked");
                emu.resize(size);
                child.writer.resize(ptyro_pty::pty_size(size))?;
            },
            () = until(next_snap) => {
                let t = snapshots.next().expect("peeked");
                tracker.update(&mut emu);
                report.snapshots.push((t, render(&emu, &mut tracker, s.ansi)));
            },
            () = until(emu.sync_deadline()) => emu.expire_sync(&mut out),
            () = tokio::time::sleep_until(deadline.into()) => break,
        }
        if !out.replies.is_empty() {
            child.writer.write_all(&out.replies).await?;
        }
        let t = start.elapsed().as_secs_f32();
        for ev in out.events {
            match ev {
                TermEvent::ProgramStatus(r) => report.log.push(format!("{t:>7.3}s osc7501 {r:?}")),
                TermEvent::Notification(n) => report.log.push(format!("{t:>7.3}s notify {n}")),
                TermEvent::Bell => report.log.push(format!("{t:>7.3}s bell")),
                TermEvent::Clipboard(c) => report.log.push(format!("{t:>7.3}s clipboard {c:?}")),
                TermEvent::Cwd(p) => report.log.push(format!("{t:>7.3}s cwd {}", p.display())),
                TermEvent::PromptMark { mark, exit } => {
                    report.log.push(format!("{t:>7.3}s osc133 {mark} {exit:?}"))
                }
                TermEvent::Progress { state, percent } => {
                    report.log.push(format!("{t:>7.3}s progress {state} {percent:?}"))
                }
                TermEvent::Reset => report.log.push(format!("{t:>7.3}s reset")),
                TermEvent::ForeignStatus { state, msg } => {
                    report.log.push(format!("{t:>7.3}s status {state} {msg:?}"))
                }
                TermEvent::Title(_) => {}
            }
        }
        if let Some(needle) = &s.until
            && start.elapsed() >= last_send
        {
            tracker.update(&mut emu);
            if render(&emu, &mut tracker, false).contains(needle.as_str()) {
                break;
            }
        }
    }
    if report.exit.is_none() {
        let _ = ptyro_pty::signal_group(child.pid, ptyro_pty::Signal::KILL);
    }
    tracker.update(&mut emu);
    report.screen = render(&emu, &mut tracker, s.ansi);
    report.title = emu.title().map(str::to_owned);
    report.modes = format!("{:?}", emu.modes());
    report.elapsed = start.elapsed();
    Ok(report)
}
