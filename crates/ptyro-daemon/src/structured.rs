//! Structured sessions in the daemon (SPEC §7.1, M10): the agent runs with pipes instead of a
//! PTY, a driver (`ptyro-drive`) speaks its protocol, and this module does the I/O:
//! - each driver effect becomes a transcript item (sent to chat views), a status signal (source
//!   `protocol`, high confidence), or a line or HTTP request for the agent;
//! - the transcript is also rendered, append-only, into the session's own emulator, so `read`,
//!   `wait --match`, the recorder and every terminal view show it;
//! - keys typed into the pane go through a small line editor: Enter sends, `a`/`A`/`d` answer an
//!   open permission request, Ctrl-C interrupts;
//! - a live upgrade parks the driver between lines and carries its state, the transcript and
//!   the pipes into the next image, like a PTY session's.

use ptyro_core::sync::MutexExt;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, ChatItem, NoticeLevel, ToolStatus, Transcript};
use ptyro_core::input::{InputEvent, KeyCode, KeyKind, Mods};
use ptyro_core::status::{Confidence, SignalSource, Sourced};
use ptyro_drive::{Driver, Effect};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::unix::pipe;
use tokio::sync::{mpsc, watch};

use crate::session::{Session, Spawned};

/// What a person (or ptyro) asks of the driver.
pub enum Msg {
    Prompt(String),
    Answer(Decision),
    Interrupt,
}

/// What's been shown in the terminal view, so the rendering stays append-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Render {
    /// Bytes of streamed text already printed, per item.
    printed: BTreeMap<String, usize>,
    /// Items printed once and never again (tool starts, permission prompts, turn ends).
    seen: BTreeSet<String>,
    /// Tools and permissions whose outcome is printed.
    settled: BTreeSet<String>,
    /// The cursor isn't at the start of a line (a message is still streaming).
    mid_line: bool,
    /// The draft line is showing at the bottom.
    draft_shown: bool,
    plan: String,
}

pub struct Structured {
    pub protocol: String,
    tx: mpsc::UnboundedSender<Msg>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Msg>>>,
    pub chat: Mutex<Transcript>,
    /// Bumped with the transcript's revision: chat subscriptions wait on it.
    pub chat_rev: watch::Sender<u64>,
    pub draft: Mutex<String>,
    pending: AtomicBool,
    render: Mutex<Render>,
    /// Duplicates of the agent's stdin, stdout and stderr, for the next image.
    fds: Mutex<Option<[OwnedFd; 3]>>,
    /// The driver's state and unread bytes while parked for an upgrade.
    parked: Mutex<Option<(serde_json::Value, Vec<u8>)>>,
    /// OpenCode's server: port and password.
    pub http: Option<(u16, String)>,
    stderr_tail: Mutex<VecDeque<String>>,
}

/// A new structured session's driver.
pub struct Start {
    pub driver: Box<dyn Driver>,
}

/// Everything the driver task needs.
pub struct Resume {
    driver: Box<dyn Driver>,
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: Option<OwnedFd>,
    buffer: Vec<u8>,
    fresh: bool,
}

/// A structured session as the handoff image carries it.
#[derive(Serialize, Deserialize)]
pub struct StructuredImage {
    pub protocol: String,
    pub driver: serde_json::Value,
    pub chat: Transcript,
    pub draft: String,
    pub render: Render,
    pub http: Option<(u16, String)>,
    pub fds: Option<[i32; 3]>,
    pub buffer: Vec<u8>,
}

impl StructuredImage {
    pub fn has_pipes(&self) -> bool {
        self.fds.is_some()
    }
}

impl Structured {
    pub fn new(protocol: &str, http: Option<(u16, String)>) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            protocol: protocol.to_owned(),
            tx,
            rx: Mutex::new(Some(rx)),
            chat: Mutex::new(Transcript::default()),
            chat_rev: watch::Sender::new(0),
            draft: Mutex::new(String::new()),
            pending: AtomicBool::new(false),
            render: Mutex::new(Render::default()),
            fds: Mutex::new(None),
            parked: Mutex::new(None),
            http,
            stderr_tail: Mutex::new(VecDeque::new()),
        })
    }

    pub fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }

    pub fn answer(&self, d: Decision) -> bool {
        self.has_pending() && self.tx.send(Msg::Answer(d)).is_ok()
    }

    pub fn interrupt(&self) {
        let _ = self.tx.send(Msg::Interrupt);
    }

    pub fn send_text(&self, s: &Session, text: &str, enter: bool) {
        if enter {
            let mut d = self.draft.locked();
            let full = format!("{}{text}", std::mem::take(&mut *d));
            drop(d);
            self.clear_draft_line(s);
            if !full.trim().is_empty() {
                let _ = self.tx.send(Msg::Prompt(full));
            }
        } else {
            self.draft.locked().push_str(text);
            self.redraw_draft(s);
        }
    }

    /// The line editor (keys typed into a structured pane).
    pub fn key(&self, s: &Session, ev: &InputEvent) {
        match ev {
            InputEvent::Paste { text } => {
                self.draft.locked().push_str(text);
                self.redraw_draft(s);
            }
            InputEvent::Key { key } if key.kind != KeyKind::Release => {
                let ctrl = key.mods.has(Mods::CTRL);
                if self.has_pending() && self.draft.locked().is_empty() {
                    let d = match key.code {
                        // (Not Enter: a return typed ahead, or a double tap, must not approve a
                        // tool call the person hasn't read.)
                        KeyCode::Char { c: 'a' | 'y' | '1' } if !ctrl => Some(Decision::Allow),
                        KeyCode::Char { c: 'A' | '2' } if !ctrl => Some(Decision::Always),
                        KeyCode::Char { c: 'd' | 'n' | '3' } | KeyCode::Esc if !ctrl => {
                            Some(Decision::Deny)
                        }
                        _ => None,
                    };
                    if let Some(d) = d {
                        self.answer(d);
                        return;
                    }
                }
                match key.code {
                    KeyCode::Char { c: 'c' } if ctrl => {
                        let had = !self.draft.locked().is_empty();
                        if had {
                            self.draft.locked().clear();
                            self.redraw_draft(s);
                        } else {
                            self.interrupt();
                        }
                    }
                    KeyCode::Enter if key.mods.has(Mods::SHIFT) || key.mods.has(Mods::ALT) => {
                        self.draft.locked().push('\n');
                        self.redraw_draft(s);
                    }
                    KeyCode::Enter => self.send_text(s, "", true),
                    KeyCode::Backspace => {
                        self.draft.locked().pop();
                        self.redraw_draft(s);
                    }
                    KeyCode::Esc => {
                        self.draft.locked().clear();
                        self.redraw_draft(s);
                    }
                    KeyCode::Char { c } if !ctrl && !key.mods.has(Mods::ALT) => {
                        self.draft.locked().push(c);
                        self.redraw_draft(s);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        self.bump_chat();
    }

    fn bump_chat(&self) {
        let rev = self.chat.locked().rev;
        self.chat_rev.send_modify(|r| *r = rev.max(*r + 1));
    }

    fn draft_line(&self) -> Vec<u8> {
        let d = self.draft.locked().replace('\n', "⏎");
        format!("\r\x1b[2K\x1b[1;36m› \x1b[0m{d}").into_bytes()
    }

    fn redraw_draft(&self, s: &Session) {
        let mut r = self.render.locked();
        if r.mid_line {
            return;
        }
        r.draft_shown = true;
        drop(r);
        s.view(&self.draft_line());
    }

    fn clear_draft_line(&self, s: &Session) {
        let mut r = self.render.locked();
        if r.draft_shown {
            r.draft_shown = false;
            drop(r);
            s.view(b"\r\x1b[2K");
        }
    }

    /// A transcript item from the driver: keep it, show it.
    fn on_item(&self, s: &Session, item: ChatItem) {
        let bytes = {
            let mut chat = self.chat.locked();
            let it = chat.upsert(item).clone();
            let mut r = self.render.locked();
            render(&mut r, &it)
        };
        if !bytes.is_empty() {
            let mut out = Vec::new();
            let redraw = {
                let mut r = self.render.locked();
                if r.draft_shown {
                    out.extend_from_slice(b"\r\x1b[2K");
                }
                let was = r.draft_shown;
                r.draft_shown = false;
                was && !r.mid_line
            };
            out.extend_from_slice(&bytes);
            let draft_wanted = !self.draft.locked().is_empty();
            if redraw || (draft_wanted && !self.render.locked().mid_line) {
                out.extend_from_slice(&self.draft_line());
                self.render.locked().draft_shown = true;
            }
            s.view(&out);
        }
        self.bump_chat();
    }

    pub fn image(&self, live: bool) -> std::io::Result<StructuredImage> {
        let (driver, buffer) = self.parked.locked().clone().unwrap_or_default();
        let fds = if live {
            match self.fds.locked().as_ref() {
                Some(f) => Some([
                    ptyro_pty::handoff::inheritable(&f[0])?,
                    ptyro_pty::handoff::inheritable(&f[1])?,
                    ptyro_pty::handoff::inheritable(&f[2])?,
                ]),
                None => None,
            }
        } else {
            None
        };
        Ok(StructuredImage {
            protocol: self.protocol.clone(),
            driver,
            chat: self.chat.locked().clone(),
            draft: self.draft.locked().clone(),
            render: self.render.locked().clone(),
            http: self.http.clone(),
            fds,
            buffer,
        })
    }

    /// Back from an image: the state, and (when it was running) what the driver task needs.
    pub fn from_image(
        si: StructuredImage,
        live: bool,
    ) -> std::io::Result<(Arc<Self>, Option<Resume>)> {
        let st = Self::new(&si.protocol, si.http.clone());
        *st.chat.locked() = si.chat;
        *st.draft.locked() = si.draft;
        *st.render.locked() = si.render;
        let resume = match (si.fds, live) {
            (Some([i, o, e]), true) => {
                let driver = ptyro_drive::restore(&si.protocol, si.driver)
                    .ok_or_else(|| std::io::Error::other("driver state didn't restore"))?;
                let (stdin, stdout, stderr) = (
                    ptyro_pty::handoff::adopt(i)?,
                    ptyro_pty::handoff::adopt(o)?,
                    ptyro_pty::handoff::adopt(e)?,
                );
                st.keep_fds(&stdin, &stdout, &stderr)?;
                Some(Resume {
                    driver,
                    stdin,
                    stdout,
                    stderr: Some(stderr),
                    buffer: si.buffer,
                    fresh: false,
                })
            }
            _ => None,
        };
        Ok((st, resume))
    }

    fn keep_fds(&self, i: &OwnedFd, o: &OwnedFd, e: &OwnedFd) -> std::io::Result<()> {
        let dup = |f: &OwnedFd| rustix::io::fcntl_dupfd_cloexec(f, 0).map_err(std::io::Error::from);
        *self.fds.locked() = Some([dup(i)?, dup(o)?, dup(e)?]);
        Ok(())
    }
}

/// Start the agent with pipes, in its own process group (so stopping it reaches its children).
pub fn spawn_child(
    s: &Spawned,
    st: &Structured,
    start: Start,
) -> std::io::Result<(tokio::process::Child, Resume)> {
    let (exe, args) = s.argv.split_first().ok_or_else(|| std::io::Error::other("no command"))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args)
        .current_dir(&s.cwd)
        .env_clear()
        .envs(s.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn()?;
    let stdin: OwnedFd =
        child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?.into_owned_fd()?;
    let stdout: OwnedFd =
        child.stdout.take().ok_or_else(|| std::io::Error::other("no stdout"))?.into_owned_fd()?;
    let stderr: OwnedFd =
        child.stderr.take().ok_or_else(|| std::io::Error::other("no stderr"))?.into_owned_fd()?;
    st.keep_fds(&stdin, &stdout, &stderr)?;
    Ok((
        child,
        Resume {
            driver: start.driver,
            stdin,
            stdout,
            stderr: Some(stderr),
            buffer: Vec::new(),
            fresh: true,
        },
    ))
}

/// Run the driver (and the side tasks) for a session.
pub fn run(session: Arc<Session>, r: Resume) {
    let Some(st) = session.structured.clone() else { return };
    if let Some(e) = r.stderr.as_ref().and_then(|f| rustix::io::fcntl_dupfd_cloexec(f, 0).ok()) {
        tokio::spawn(drain_stderr(Arc::clone(&st), e));
    }
    tokio::spawn(driver_loop(session, st, r));
}

async fn drain_stderr(st: Arc<Structured>, fd: OwnedFd) {
    let Ok(rx) = pipe::Receiver::from_owned_fd(fd) else { return };
    let mut lines = BufReader::new(rx).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        let mut t = st.stderr_tail.locked();
        t.push_back(l.chars().take(300).collect());
        if t.len() > 20 {
            t.pop_front();
        }
    }
}

fn high(signal: ptyro_core::status::Signal) -> Sourced {
    Sourced { signal, source: SignalSource::Protocol, confidence: Confidence::High }
}

async fn driver_loop(session: Arc<Session>, st: Arc<Structured>, r: Resume) {
    let Some(mut rx) = st.rx.locked().take() else { return };
    let mut driver = r.driver;
    let Ok(mut stdin) = pipe::Sender::from_owned_fd(r.stdin) else { return };
    let Ok(mut stdout) = pipe::Receiver::from_owned_fd(r.stdout) else { return };
    let io = session.io_control();
    let mut pause = io.pause.clone();
    io.readers.fetch_add(1, Ordering::SeqCst);
    let (http_tx, mut http_rx) = mpsc::unbounded_channel::<(u32, u16, String)>();
    let (sse_tx, mut sse_rx) = mpsc::unbounded_channel::<String>();
    let opencode = st.protocol == "opencode";
    if opencode && let Some((port, pw)) = st.http.clone() {
        tokio::spawn(follow_sse(Arc::clone(&session), port, pw, sse_tx));
    }
    let mut ctx = Ctx { session: &session, st: &st, http_tx };
    if r.fresh {
        let fx = driver.start();
        ctx.apply(fx, &mut stdin).await;
    }
    let mut buf = r.buffer;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if *pause.borrow_and_update() {
            *st.parked.locked() = Some((driver.save(), buf.clone()));
            io.parked.fetch_add(1, Ordering::SeqCst);
            let _ = pause.wait_for(|p| !*p).await;
            io.parked.fetch_sub(1, Ordering::SeqCst);
            *st.parked.locked() = None;
        }
        // Whole lines only; a partial one waits for the rest.
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            if opencode {
                continue; // the server's log, not the protocol
            }
            let text = String::from_utf8_lossy(&line);
            let fx = driver.on_line(text.trim_end());
            ctx.apply(fx, &mut stdin).await;
        }
        if buf.len() > ptyro_protocol::MAX_FRAME {
            buf.clear(); // a runaway line: drop it rather than grow forever
        }
        st.pending.store(driver.pending(), Ordering::Relaxed);
        tokio::select! {
            n = stdout.read(&mut chunk) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            },
            Some(line) = sse_rx.recv() => {
                let fx = driver.on_line(&line);
                ctx.apply(fx, &mut stdin).await;
            }
            Some(msg) = rx.recv() => {
                let fx = match msg {
                    Msg::Prompt(t) => driver.prompt(&t),
                    Msg::Answer(d) => driver.answer(d),
                    Msg::Interrupt => driver.interrupt(),
                };
                ctx.apply(fx, &mut stdin).await;
            }
            Some((tag, code, body)) = http_rx.recv() => {
                let fx = driver.http_reply(tag, code, &body);
                ctx.apply(fx, &mut stdin).await;
            }
            _ = pause.changed() => {}
        }
    }
    io.readers.fetch_sub(1, Ordering::SeqCst);
    // Whatever the agent said on its way out (not logged in, a bad flag) is worth showing.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tail: Vec<String> = st.stderr_tail.locked().iter().cloned().collect();
    let failed = !matches!(
        session.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state,
        ptyro_core::session::ProcessState::Exited { code: Some(0), .. }
    );
    if failed && !tail.is_empty() {
        let text = tail[tail.len().saturating_sub(5)..].join("\n");
        st.on_item(
            &session,
            ChatItem::new("stderr", ChatBody::Notice { level: NoticeLevel::Error, text }),
        );
    }
    tracing::debug!(session = %session.id, "structured driver ended");
}

struct Ctx<'a> {
    session: &'a Arc<Session>,
    st: &'a Arc<Structured>,
    http_tx: mpsc::UnboundedSender<(u32, u16, String)>,
}

impl Ctx<'_> {
    async fn apply(&mut self, effects: Vec<Effect>, stdin: &mut pipe::Sender) {
        let mut signals = Vec::new();
        for e in effects {
            match e {
                Effect::Item(it) => self.st.on_item(self.session, it),
                Effect::Signal(sig) => signals.push(high(sig)),
                Effect::Send(line) => {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    if let Err(e) = stdin.write_all(&bytes).await {
                        tracing::debug!(session = %self.session.id, "agent stdin: {e}");
                    }
                }
                Effect::Http { method, path, body, tag } => {
                    let Some((port, pw)) = self.st.http.clone() else { continue };
                    let tx = self.http_tx.clone();
                    tokio::spawn(async move {
                        let (code, text) = http(port, &pw, method, &path, body).await;
                        let _ = tx.send((tag, code, text));
                    });
                }
            }
        }
        if !signals.is_empty() {
            self.session.signal(signals);
        }
    }
}

/// One request to OpenCode's server on loopback (`Connection: close`, small bodies).
async fn http(
    port: u16,
    password: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, String) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        crate::sidecar::basic(password),
        body.len()
    );
    // The server comes up a moment after the process: retry the connection briefly.
    let mut sock = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)).await {
            sock = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(mut sock) = sock else { return (0, "OpenCode's server isn't answering".into()) };
    if sock.write_all(req.as_bytes()).await.is_err() {
        return (0, String::new());
    }
    let mut resp = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(30), sock.read_to_end(&mut resp)).await;
    let text = String::from_utf8_lossy(&resp).into_owned();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default();
    (code, body)
}

/// OpenCode's `GET /event` stream, reconnecting while the session lives.
async fn follow_sse(
    session: Arc<Session>,
    port: u16,
    password: String,
    tx: mpsc::UnboundedSender<String>,
) {
    while session.is_live() {
        if let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)).await {
            let req = format!(
                "GET /event HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {}\r\nAccept: text/event-stream\r\n\r\n",
                crate::sidecar::basic(&password)
            );
            if sock.write_all(req.as_bytes()).await.is_ok() {
                let mut r = BufReader::new(sock);
                let mut line = String::new();
                let mut data = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let l = line.trim_end_matches(['\r', '\n']);
                    if let Some(d) = l.strip_prefix("data:") {
                        data.push_str(d.trim_start());
                    } else if l.is_empty()
                        && !data.is_empty()
                        && tx.send(std::mem::take(&mut data)).is_err()
                    {
                        return;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ── the terminal view of a transcript ─────────────────────────────────────────────────────────

/// Text from an agent or a tool as the pane may draw it: no control characters, so it can't
/// move the cursor, erase the line above, or open an OSC (a tool title that rewrote the
/// "wants permission" line would be a spoofed prompt).
fn plain(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect::<String>()
        .replace('\n', "\r\n")
}

/// The bytes that show `it` (given what's been shown before).
fn render(r: &mut Render, it: &ChatItem) -> Vec<u8> {
    let mut out = String::new();
    let newline = |r: &mut Render, out: &mut String| {
        if r.mid_line {
            out.push_str("\r\n");
            r.mid_line = false;
        }
    };
    match &it.body {
        ChatBody::User { text } => {
            if r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                out.push_str(&format!("\r\n\x1b[1;36m› \x1b[0m\x1b[1m{}\x1b[0m\r\n", crlf(text)));
            }
        }
        ChatBody::Assistant { text, streaming } => {
            let done = r.printed.get(&it.id).copied().unwrap_or(0);
            if done == 0 && !text.is_empty() {
                newline(r, &mut out);
            }
            if text.len() > done && text.is_char_boundary(done) {
                out.push_str(&crlf(&text[done..]));
                r.printed.insert(it.id.clone(), text.len());
                r.mid_line = !text.ends_with('\n');
            }
            if !streaming && r.seen.insert(it.id.clone()) && r.mid_line {
                out.push_str("\r\n");
                r.mid_line = false;
            }
        }
        ChatBody::Thinking { .. } => {
            if r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                out.push_str("\x1b[2;3m· thinking…\x1b[0m\r\n");
            }
        }
        ChatBody::Tool { name, title, status, output, diff, .. } => {
            if r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                out.push_str(&format!(
                    "\x1b[33m▸ {}\x1b[0m {}\r\n",
                    plain(name),
                    plain(title.lines().next().unwrap_or(""))
                ));
            }
            let settled =
                matches!(status, ToolStatus::Done | ToolStatus::Failed | ToolStatus::Declined);
            if settled && r.settled.insert(it.id.clone()) {
                newline(r, &mut out);
                let (color, word) = match status {
                    ToolStatus::Done => ("32", "done"),
                    ToolStatus::Failed => ("31", "failed"),
                    _ => ("31", "declined"),
                };
                out.push_str(&format!("  \x1b[{color}m{} {word}\x1b[0m\r\n", status.glyph()));
                if let Some(d) = diff {
                    for l in d
                        .lines()
                        .filter(|l| !l.starts_with("---") && !l.starts_with("+++"))
                        .take(12)
                    {
                        let c = if l.starts_with('+') {
                            "32"
                        } else if l.starts_with('-') {
                            "31"
                        } else {
                            "2"
                        };
                        out.push_str(&format!("    \x1b[{c}m{}\x1b[0m\r\n", plain(l)));
                    }
                }
                if let Some(o) = output.as_deref().filter(|o| !o.trim().is_empty()) {
                    let lines: Vec<&str> = o.lines().collect();
                    let show: Vec<&str> = if matches!(status, ToolStatus::Failed) {
                        lines[lines.len().saturating_sub(4)..].to_vec()
                    } else {
                        lines.iter().take(3).copied().collect()
                    };
                    for l in show {
                        out.push_str(&format!(
                            "    \x1b[2m{}\x1b[0m\r\n",
                            plain(&l.chars().take(200).collect::<String>())
                        ));
                    }
                    if lines.len() > 4 {
                        out.push_str(&format!("    \x1b[2m… {} lines\x1b[0m\r\n", lines.len()));
                    }
                }
            }
        }
        ChatBody::Permission { tool, title, decided, .. } => {
            if decided.is_none() && r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                out.push_str(&format!(
                    "\x1b[1;31m⚠ wants permission\x1b[0m {}: {}\r\n  \x1b[2m[a] allow  [A] always  [d] deny\x1b[0m\r\n",
                    plain(tool),
                    plain(title.lines().next().unwrap_or(""))
                ));
            }
            if let Some(d) = decided
                && r.settled.insert(it.id.clone())
            {
                newline(r, &mut out);
                out.push_str(&format!("  \x1b[2m→ {}\x1b[0m\r\n", plain(&d.to_string())));
            }
        }
        ChatBody::Plan { steps } => {
            let done = steps.iter().filter(|s| s.status == "completed").count();
            let now = steps
                .iter()
                .find(|s| s.status == "in_progress")
                .map(|s| plain(&s.text))
                .unwrap_or_default();
            let line = format!(
                "☰ plan {done}/{}{}",
                steps.len(),
                if now.is_empty() { String::new() } else { format!(": {now}") }
            );
            if line != r.plan {
                newline(r, &mut out);
                out.push_str(&format!("\x1b[35m{line}\x1b[0m\r\n"));
                r.plan = line;
            }
        }
        ChatBody::Notice { level, text } => {
            if r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                let c = match level {
                    NoticeLevel::Info => "2",
                    NoticeLevel::Warn => "33",
                    NoticeLevel::Error => "31",
                };
                out.push_str(&format!("\x1b[{c}m! {}\x1b[0m\r\n", crlf(text)));
            }
        }
        ChatBody::TurnEnd { stop, cost_micro_usd, duration_ms, .. } => {
            if r.seen.insert(it.id.clone()) {
                newline(r, &mut out);
                let mut parts = vec![plain(stop)];
                if let Some(c) = cost_micro_usd {
                    parts.push(format!("${:.4}", *c as f64 / 1e6));
                }
                if let Some(d) = duration_ms {
                    parts.push(format!("{:.1}s", *d as f64 / 1000.0));
                }
                out.push_str(&format!("\x1b[2m─ {} ─\x1b[0m\r\n", parts.join(" · ")));
            }
        }
    }
    out.into_bytes()
}

/// A transcript's tool calls, as the compare view shows them.
pub fn tool_use(t: ptyro_core::chat::ToolSummary) -> ptyro_run::wire::ToolUse {
    ptyro_run::wire::ToolUse {
        calls: t.calls,
        by_tool: t.by_tool,
        failed: t.failed,
        files: t.files,
        commands: t.commands,
        cost_micro_usd: t.cost_micro_usd,
    }
}

impl Session {
    /// The transcript changes after revision `rev`, and the draft.
    pub fn chat_since(&self, rev: u64) -> Option<(u64, Vec<ChatItem>, String)> {
        let st = self.structured.as_ref()?;
        let chat = st.chat.locked();
        Some((chat.rev, chat.since(rev), st.draft.locked().clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::chat::PermOption;

    fn shown(r: &mut Render, items: &[ChatItem]) -> String {
        items.iter().map(|i| String::from_utf8(render(r, i)).unwrap()).collect()
    }

    #[test]
    fn rendering_is_append_only() {
        let mut r = Render::default();
        let a =
            |t: &str, s| ChatItem::new("m1", ChatBody::Assistant { text: t.into(), streaming: s });
        let out = shown(
            &mut r,
            &[
                ChatItem::new("u", ChatBody::User { text: "fix it".into() }),
                a("Look", true),
                a("Looking", true),
                ChatItem::new(
                    "t1",
                    ChatBody::Tool {
                        name: "Bash".into(),
                        title: "cargo test".into(),
                        status: ToolStatus::Running,
                        output: None,
                        diff: None,
                        files: vec![],
                    },
                ),
                ChatItem::new(
                    "t1",
                    ChatBody::Tool {
                        name: "Bash".into(),
                        title: "cargo test".into(),
                        status: ToolStatus::Failed,
                        output: Some("a\nb\nc\nd\ne\n1 failed".into()),
                        diff: None,
                        files: vec![],
                    },
                ),
                ChatItem::new(
                    "p",
                    ChatBody::Permission {
                        request: "r".into(),
                        tool: "Bash".into(),
                        title: "rm x".into(),
                        options: PermOption::standard(),
                        decided: None,
                    },
                ),
                ChatItem::new(
                    "p",
                    ChatBody::Permission {
                        request: "r".into(),
                        tool: "Bash".into(),
                        title: "rm x".into(),
                        options: PermOption::standard(),
                        decided: Some("deny".into()),
                    },
                ),
                ChatItem::new(
                    "e",
                    ChatBody::TurnEnd {
                        stop: "end_turn".into(),
                        cost_micro_usd: Some(12_300),
                        duration_ms: Some(2500),
                        input_tokens: None,
                        output_tokens: None,
                    },
                ),
            ],
        );
        let plain = String::from_utf8(out.bytes().filter(|_| true).collect()).unwrap();
        // "Look" then "ing": the second update printed only the new part.
        assert!(plain.contains("Looking") && plain.matches("Look").count() == 1, "{plain:?}");
        assert!(
            plain.contains("▸ Bash\x1b[0m cargo test")
                && plain.contains("✗ failed")
                && plain.contains("1 failed")
        );
        assert!(plain.contains("⚠ wants permission") && plain.contains("→ deny"));
        assert!(plain.contains("end_turn · $0.0123 · 2.5s"));
        // The streamed message's open line was closed before the tool line.
        assert!(plain.contains("Looking\r\n\x1b[33m▸"), "{plain:?}");
        // Seen once: the same item again prints nothing.
        assert!(
            render(&mut r, &ChatItem::new("u", ChatBody::User { text: "fix it".into() }))
                .is_empty()
        );
    }
}
