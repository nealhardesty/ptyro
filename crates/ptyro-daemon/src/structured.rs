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
use tokio::sync::{mpsc, watch};

/// The agent's three pipes, as each platform drives them. On Unix, duplicates of their
/// descriptors are kept for a live upgrade, which carries them by number.
#[cfg(unix)]
mod pipes {
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd};
    use tokio::net::unix::pipe;

    pub type In = pipe::Sender;
    pub type Out = pipe::Receiver;
    pub type ErrOut = pipe::Receiver;
    /// Duplicates for the next image.
    pub type Kept = [OwnedFd; 3];

    fn dup(f: &OwnedFd) -> io::Result<OwnedFd> {
        rustix::io::fcntl_dupfd_cloexec(f, 0).map_err(io::Error::from)
    }

    fn open(i: OwnedFd, o: OwnedFd, e: OwnedFd) -> io::Result<(In, Out, ErrOut, Option<Kept>)> {
        let kept = [dup(&i)?, dup(&o)?, dup(&e)?];
        Ok((
            pipe::Sender::from_owned_fd(i)?,
            pipe::Receiver::from_owned_fd(o)?,
            pipe::Receiver::from_owned_fd(e)?,
            Some(kept),
        ))
    }

    pub fn from_child(
        c: &mut tokio::process::Child,
    ) -> io::Result<(In, Out, ErrOut, Option<Kept>)> {
        let missing = |what| io::Error::other(format!("no {what}"));
        let i = c.stdin.take().ok_or_else(|| missing("stdin"))?.into_owned_fd()?;
        let o = c.stdout.take().ok_or_else(|| missing("stdout"))?.into_owned_fd()?;
        let e = c.stderr.take().ok_or_else(|| missing("stderr"))?.into_owned_fd()?;
        open(i, o, e)
    }

    /// The previous image's pipes, by number.
    pub fn adopt(fds: [i32; 3]) -> io::Result<(In, Out, ErrOut, Option<Kept>)> {
        let [i, o, e] = fds;
        open(
            ptyro_pty::handoff::adopt(i)?,
            ptyro_pty::handoff::adopt(o)?,
            ptyro_pty::handoff::adopt(e)?,
        )
    }

    pub fn numbers(k: &Kept) -> [i32; 3] {
        [k[0].as_raw_fd(), k[1].as_raw_fd(), k[2].as_raw_fd()]
    }

    /// Nothing to stop: an async pipe's read that's dropped reads nothing.
    #[allow(clippy::unnecessary_wraps)]
    pub fn quiesce(_out: &mut Out) -> io::Result<Vec<u8>> {
        Ok(Vec::new())
    }

    pub fn unquiesce(_out: &Out) {}
}

/// Windows: plain anonymous pipes, made here (std's are overlapped, which a blocking read can't
/// drive), read on a thread a handover can stop (`winread`, WIN9). A handover copies their
/// handles into the next daemon, which takes them by value.
#[cfg(windows)]
mod pipes {
    use std::io;
    use std::os::windows::io::AsRawHandle;

    pub type In = tokio::fs::File;
    pub type Out = ptyro_pty::winread::PipeReader;
    pub type ErrOut = ptyro_pty::winread::PipeReader;
    /// The three pipes' handles, by value (`In`, `Out` and `ErrOut` own them), for a handover.
    pub type Kept = [usize; 3];

    /// The child's ends (stdin, stdout, stderr), for its `Stdio`, and ours.
    #[allow(clippy::type_complexity)]
    pub fn for_child() -> io::Result<([std::fs::File; 3], (In, Out, ErrOut, Option<Kept>))> {
        let (in_r, in_w) = ptyro_pty::anon_pipe()?;
        let (out_r, out_w) = ptyro_pty::anon_pipe()?;
        let (err_r, err_w) = ptyro_pty::anon_pipe()?;
        let kept = [
            in_w.as_raw_handle() as usize,
            out_r.as_raw_handle() as usize,
            err_r.as_raw_handle() as usize,
        ];
        let ours = (
            tokio::fs::File::from_std(in_w),
            ptyro_pty::winread::PipeReader::new(out_r, "agent-stdout")?,
            ptyro_pty::winread::PipeReader::new(err_r, "agent-stderr")?,
            Some(kept),
        );
        Ok(([in_r, out_w, err_w], ours))
    }

    /// The previous daemon's pipes, by the values it copied into this process.
    pub fn adopt(fds: [i32; 3]) -> io::Result<(In, Out, ErrOut, Option<Kept>)> {
        let [i, o, e] = fds.map(|h| h as u32 as usize);
        let input = ptyro_pty::adopt_pipe(i)?;
        let out = ptyro_pty::winread::adopt(o, "agent-stdout")?;
        let err = ptyro_pty::winread::adopt(e, "agent-stderr")?;
        Ok((tokio::fs::File::from_std(input), out, err, Some([i, o, e])))
    }

    /// The values a handover writes: handle values fit in 32 bits (Windows keeps them so, for
    /// 32-bit programs).
    pub fn numbers(k: &Kept) -> [i32; 3] {
        k.map(|h| h as i32)
    }

    /// Stop reading for a handover, and what was read but not yet taken.
    pub fn quiesce(out: &mut Out) -> io::Result<Vec<u8>> {
        tokio::task::block_in_place(|| out.pause())?;
        Ok(out.take_pending())
    }

    /// Read again (a handover that was called off).
    pub fn unquiesce(out: &Out) {
        out.resume();
    }
}

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
    fds: Mutex<Option<pipes::Kept>>,
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
    stdin: pipes::In,
    stdout: pipes::Out,
    stderr: Option<pipes::ErrOut>,
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

    /// A prompt, as it is (a chat's message, with its seed block).
    pub fn prompt(&self, text: String) {
        let _ = self.tx.send(Msg::Prompt(text));
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
            // By number: the upgrade carries them over a socket (H2.7).
            self.fds.locked().as_ref().map(pipes::numbers)
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
            (Some(fds), true) => {
                let driver = ptyro_drive::restore(&si.protocol, si.driver)
                    .ok_or_else(|| std::io::Error::other("driver state didn't restore"))?;
                let (stdin, stdout, stderr, kept) = pipes::adopt(fds)?;
                *st.fds.locked() = kept;
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
}

/// Start the agent with pipes, in its own process group (so stopping it reaches its children).
pub fn spawn_child(
    s: &Spawned,
    st: &Structured,
    start: Start,
) -> std::io::Result<(ptyro_pty::Child, Resume)> {
    let (exe, args) = s.argv.split_first().ok_or_else(|| std::io::Error::other("no command"))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(args).current_dir(&s.cwd).env_clear().envs(s.env.iter().map(|(k, v)| (k, v)));
    ptyro_pty::own_group(&mut cmd);
    #[cfg(unix)]
    let (child, (stdin, stdout, stderr, kept)) = {
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        let p = pipes::from_child(&mut child)?;
        (child, p)
    };
    #[cfg(windows)]
    let (child, (stdin, stdout, stderr, kept)) = {
        let ([i, o, e], ours) = pipes::for_child()?;
        cmd.stdin(i).stdout(o).stderr(e);
        let child = cmd.spawn()?;
        // Our copies of the child's ends close with `cmd`, so its output ends when it exits.
        drop(cmd);
        (child, ours)
    };
    *st.fds.locked() = kept;
    Ok((
        ptyro_pty::contain(child)?,
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
    let mut r = r;
    if let Some(e) = r.stderr.take() {
        tokio::spawn(drain_stderr(Arc::clone(&st), e));
    }
    tokio::spawn(driver_loop(session, st, r));
}

async fn drain_stderr(st: Arc<Structured>, rx: pipes::ErrOut) {
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
    let (mut stdin, mut stdout) = (r.stdin, r.stdout);
    let io = session.io_control();
    let mut pause = io.pause.clone();
    io.readers.fetch_add(1, Ordering::SeqCst);
    let (http_tx, mut http_rx) = mpsc::unbounded_channel::<(u32, u16, String)>();
    let (sse_tx, mut sse_rx) = mpsc::unbounded_channel::<String>();
    let opencode = st.protocol == "opencode";
    // OpenCode tells only who is listening: its requests wait until `/event` is.
    let (sse_up, sse_ready) = tokio::sync::watch::channel(false);
    if opencode && let Some((port, pw)) = st.http.clone() {
        tokio::spawn(follow_sse(Arc::clone(&session), port, pw, sse_tx, sse_up));
    }
    let mut ctx = Ctx { session: &session, st: &st, http_tx, sse_ready };
    if r.fresh {
        let fx = driver.start();
        ctx.apply(fx, &mut stdin).await;
    }
    let mut buf = r.buffer;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if *pause.borrow_and_update() {
            // Windows: the reader's thread stops, and what it read joins the buffer (WIN9).
            match pipes::quiesce(&mut stdout) {
                Ok(rest) => buf.extend_from_slice(&rest),
                Err(e) => tracing::warn!(session = %session.id, "pausing the agent's output: {e}"),
            }
            *st.parked.locked() = Some((driver.save(), buf.clone()));
            io.parked.fetch_add(1, Ordering::SeqCst);
            let _ = pause.wait_for(|p| !*p).await;
            io.parked.fetch_sub(1, Ordering::SeqCst);
            *st.parked.locked() = None;
            pipes::unquiesce(&stdout);
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
    /// OpenCode's event stream is connected (its first event has come).
    sse_ready: tokio::sync::watch::Receiver<bool>,
}

impl Ctx<'_> {
    async fn apply(&mut self, effects: Vec<Effect>, stdin: &mut pipes::In) {
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
                    let mut ready = self.sse_ready.clone();
                    tokio::spawn(async move {
                        // Not before the event stream: what a turn says before it listens is
                        // lost, its end too, and the chat would be answering for good. (Not
                        // for ever either: a server that never streams still gets the request.)
                        let _ =
                            tokio::time::timeout(Duration::from_secs(5), ready.wait_for(|r| *r))
                                .await;
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
    up: tokio::sync::watch::Sender<bool>,
) {
    while session.is_live() {
        up.send_replace(false);
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
                    } else if l.is_empty() && !data.is_empty() {
                        // The first event (`server.connected`): it is listening now.
                        up.send_replace(true);
                        if tx.send(std::mem::take(&mut data)).is_err() {
                            return;
                        }
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
