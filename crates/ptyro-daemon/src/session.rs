//! One session: a child in a PTY plus its terminal state (SPEC §4.3, §7).
//!
//! Three tasks per session:
//! - the **reader** always drains the PTY into the emulator, whether or not anyone is watching,
//!   and queues query replies;
//! - the **writer** owns the PTY's write half: input, replies and resizes, in order;
//! - the **waiter** records the exit status.
//!
//! Clients never block any of them; they read the terminal under a short lock to build frames.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::cache::ScreenCache;
use ptyro_core::input::{InputEvent, KeyCode, KeyKind, Mods};
use ptyro_core::session::{ProcessState, ProgramStatus, Progress, SessionInfo, SessionKind};
use ptyro_core::status::{
    AgentStatus, Attention, Child, Confidence, Signal, SignalSource, Sourced,
};
use ptyro_core::{ClientId, SessionId, TermSize, now_ms};

use crate::engine::{Engine, PspRecords};
use ptyro_protocol::{DomainEvent, ScreenFormat};
use ptyro_store::{SessionRecord, Store};
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable, TermEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};

/// Graceful stop timings (SPEC §5.1): SIGHUP, 3 s, SIGTERM, 2 s, SIGKILL.
pub const STOP_GRACE: (Duration, Duration) = (Duration::from_secs(3), Duration::from_secs(2));

/// Lifecycle scripts stop with SIGTERM, then SIGKILL after 5 s (SPEC §10.5).
const SCRIPT_STOP: &[(ptyro_pty::Signal, Duration)] = &[
    (ptyro_pty::Signal::TERM, Duration::from_secs(5)),
    (ptyro_pty::Signal::KILL, Duration::from_secs(5)),
];

/// Where a session belongs and who it runs as (SPEC §7.2, §10.10).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub profile: String,
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub workspace: Option<String>,
    pub agent: Option<String>,
    pub script: Option<String>,
}

pub struct Term {
    pub emu: AlacrittyEmulator,
    pub tracker: FrameTracker,
}

/// Serializable for live upgrades (M8.1): the handoff image carries it whole.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// A structured session's protocol (M10); kept so a resume or restart stays structured.
    #[serde(default)]
    pub structured: Option<String>,
    pub name: String,
    pub kind: SessionKind,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub state: ProcessState,
    pub pid: Option<u32>,
    pub ended_ms: Option<u64>,
    /// Clients come back after a handoff and take control again.
    #[serde(skip)]
    pub controller: Option<ClientId>,
    #[serde(skip)]
    pub viewers: u32,
    pub program_status: Option<ProgramStatus>,
    /// Working directory the shell reported over OSC 7.
    pub reported_cwd: Option<PathBuf>,
    pub progress: Option<Progress>,
    /// Exit status of the last shell command (OSC 133;D).
    pub last_command_exit: Option<i32>,
    /// The spawning client's variables (SSH agent, display), kept for `restart`.
    pub client_env: BTreeMap<String, String>,
    pub place: Place,
    /// Ports its processes listen on, refreshed by the daemon's port scan.
    pub ports: Vec<u16>,
    /// The adapter driving it (`claude`, `generic`…), for agent sessions.
    pub adapter: Option<String>,
    pub conversation: Option<String>,
    pub attention: Option<Attention>,
    pub children: Vec<Child>,
    pub dangerous: bool,
    /// The first prompt typed into it (branch names, M3.16).
    pub first_prompt: Option<String>,
    /// When the shell's current command started (OSC 133 `C`).
    pub command_since_ms: Option<u64>,
    /// The shell's current or last command (UX1.2).
    #[serde(default)]
    pub command: Option<ptyro_core::session::ShellCommand>,
    /// An agent started by hand in this shell, in the foreground now (UX1.4).
    #[serde(default)]
    pub inner_agent: Option<String>,
    /// Its latest summary (UX1.3), and whether one is being made.
    pub summary: Option<ptyro_core::session::Summary>,
    pub summarizing: bool,
    /// Its title, for people to read (UX1.24): `--title`, `retitle`, else its first prompt.
    #[serde(default)]
    pub title: Option<String>,
    /// Claude's messaging socket, from the hook environment (M4.18).
    pub messaging: Option<String>,
    /// A prompt waits to be typed once the agent is ready.
    pub prompt_pending: bool,
    /// When ptyro last typed a prompt (with Enter) into it.
    pub last_prompt_ms: Option<u64>,
    /// Completed turns (working → idle/done).
    pub turns: u32,
    /// The worktree's state when the current (or last) turn started (`git stash create`).
    pub turn_base: Option<String>,
    pub turn_started_ms: u64,
    /// The agent's last message of the current turn, once it ended (`Signal::Reply`).
    pub last_reply: Option<String>,
    /// Verified-done (M7.9).
    pub verify: Option<ptyro_core::session::Verify>,
    /// `(run, step)` it works for (M7.1).
    pub run: Option<(String, String)>,
    /// Tripwires from its recipe step (M7.4): paths and commands PreToolUse denies.
    pub guard: Option<crate::runs::Guard>,
}

/// What sessions tell the daemon's signal processor (`agents.rs`).
pub enum Feed {
    Signals(SessionId, Vec<Sourced>),
    /// Signals whose sender waits until they're applied (a hook's reply depends on them).
    Apply(SessionId, Vec<Sourced>, tokio::sync::oneshot::Sender<()>),
    /// Apply a deferred status whose tie window passed.
    Flush(SessionId),
    Exited(SessionId),
}

fn sourced(signal: Signal, source: SignalSource, confidence: Confidence) -> Sourced {
    Sourced { signal, source, confidence }
}

enum WriterMsg {
    Bytes(Vec<u8>),
    Resize(TermSize),
}

pub struct Session {
    pub id: SessionId,
    pub meta: Mutex<Meta>,
    pub term: Mutex<Term>,
    /// Bumped whenever the screen changes or the process state does; subscriptions wait on it.
    pub changed: watch::Sender<u64>,
    exited: watch::Sender<bool>,
    /// Unbounded on purpose: the reader must never wait for the writer (a child that stops
    /// reading input while still writing output would otherwise deadlock both).
    writer: mpsc::UnboundedSender<WriterMsg>,
    master: Option<OwnedFd>,
    events: broadcast::Sender<DomainEvent>,
    store: Option<Store>,
    run_id: String,
    pub engine: Mutex<Engine>,
    psp: Mutex<PspRecords>,
    feed: mpsc::UnboundedSender<Feed>,
    /// Stopping to hibernate: the exit makes it dormant, not exited.
    pub hibernating: AtomicBool,
    /// When the program last wrote visible output, and how much (the activity heuristic, §9.3).
    pub last_output_ms: AtomicU64,
    pub output_bytes: AtomicU64,
    /// One typist at a time: a prompt (text, pause, Enter) is never interleaved with another
    /// one (mail arriving while a script sends, M7.7).
    typing: tokio::sync::Mutex<()>,
    /// OSC 7501 `done` means "turn over" for agents, "task complete" for tools.
    done_means_idle: bool,
    io: IoControl,
    /// The recorder (M8.2) and where its files are.
    recorder: Mutex<Option<ptyro_term::record::Recorder>>,
    record_dir: Mutex<Option<PathBuf>>,
    /// Driven over a structured protocol instead of a terminal (M10).
    pub structured: Option<Arc<crate::structured::Structured>>,
}

pub struct Spawned {
    pub argv: Vec<String>,
    pub kind: SessionKind,
    pub name: String,
    pub cwd: PathBuf,
    pub size: TermSize,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub client_env: BTreeMap<String, String>,
    pub scrollback: usize,
    pub place: Place,
    /// The launch as stored: argv and env diff with secrets redacted (SPEC §14.2).
    pub launch: serde_json::Value,
    pub adapter: Option<String>,
    pub conversation: Option<String>,
    pub dangerous: bool,
    pub stale_after_ms: u64,
    /// Record its output here, ring-capped at this many bytes (M8.2).
    pub record: Option<(PathBuf, u64)>,
}

/// Everything about a session that isn't its process: for dormant sessions brought back from the
/// store.
pub struct DormantSpec {
    pub name: String,
    pub kind: SessionKind,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub place: Place,
    pub adapter: Option<String>,
    pub conversation: Option<String>,
    pub ended_ms: Option<u64>,
    pub structured: Option<String>,
    pub title: Option<String>,
}

/// The shared plumbing every session gets.
#[derive(Clone)]
pub struct Hub {
    pub events: broadcast::Sender<DomainEvent>,
    pub store: Option<Store>,
    pub run_id: String,
    pub feed: mpsc::UnboundedSender<Feed>,
    pub io: IoControl,
}

/// Pausing every PTY reader at once, for a live upgrade (M8.1): readers park, and say so.
#[derive(Clone)]
pub struct IoControl {
    pub pause: watch::Receiver<bool>,
    /// Readers running / parked right now.
    pub readers: Arc<AtomicU64>,
    pub parked: Arc<AtomicU64>,
}

/// A session as the handoff image carries it (M8.1).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SessionImage {
    pub id: String,
    pub meta: Meta,
    pub status: Option<ptyro_core::status::StatusRecord>,
    pub activity: Option<String>,
    /// The emulator (`EmuImage`), MessagePack.
    pub emu: Vec<u8>,
    /// The PTY master, inherited across `execve` (live sessions only).
    pub master_fd: Option<i32>,
    pub last_output_ms: u64,
    pub output_bytes: u64,
    pub done_means_idle: bool,
    pub stale_after_ms: u64,
    /// The status engine's clocks ([`Engine::clocks`]).
    #[serde(default)]
    pub engine_clocks: [Option<u64>; 3],
    /// A structured session's driver, transcript and pipes (M10).
    #[serde(default)]
    pub structured: Option<crate::structured::StructuredImage>,
}

fn new_meta(
    name: String,
    kind: SessionKind,
    argv: Vec<String>,
    cwd: PathBuf,
    place: Place,
) -> Meta {
    Meta {
        structured: None,
        name,
        kind,
        argv,
        cwd,
        state: ProcessState::Starting,
        pid: None,
        ended_ms: None,
        controller: None,
        viewers: 0,
        program_status: None,
        reported_cwd: None,
        progress: None,
        last_command_exit: None,
        client_env: BTreeMap::new(),
        place,
        ports: Vec::new(),
        adapter: None,
        conversation: None,
        attention: None,
        children: Vec::new(),
        dangerous: false,
        first_prompt: None,
        command_since_ms: None,
        command: None,
        inner_agent: None,
        summary: None,
        summarizing: false,
        title: None,
        messaging: None,
        prompt_pending: false,
        last_prompt_ms: None,
        turns: 0,
        turn_base: None,
        turn_started_ms: 0,
        last_reply: None,
        verify: None,
        run: None,
        guard: None,
    }
}

impl Session {
    pub fn spawn(id: SessionId, s: Spawned, hub: Hub) -> std::io::Result<Arc<Self>> {
        let child = ptyro_pty::spawn(ptyro_pty::SpawnRequest {
            argv: &s.argv,
            cwd: &s.cwd,
            env: s.env,
            size: s.size,
        })?;
        let mut emu = AlacrittyEmulator::new(s.size, s.scrollback);
        let tracker = FrameTracker::new(&mut emu);
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let recorder = open_recorder(s.record.as_ref(), s.size);
        let mut meta = new_meta(s.name, s.kind, s.argv, s.cwd, s.place);
        meta.state = ProcessState::Running;
        meta.pid = Some(child.pid);
        meta.client_env = s.client_env;
        meta.adapter = s.adapter.clone();
        meta.conversation = s.conversation;
        meta.dangerous = s.dangerous;
        let done_means_idle =
            s.adapter.as_deref().is_some_and(|a| ptyro_agents::adapter(a).done_means_idle());
        let mut engine = Engine::new(s.stale_after_ms);
        if s.kind == SessionKind::Agent {
            engine.apply(
                crate::engine::Input {
                    source: SignalSource::Process,
                    confidence: Confidence::Low,
                    status: AgentStatus::Starting,
                    kind: None,
                    detail: None,
                    progress: None,
                },
                now_ms(),
            );
        }
        let session = Arc::new(Self {
            id,
            meta: Mutex::new(meta),
            term: Mutex::new(Term { emu, tracker }),
            changed: watch::Sender::new(0),
            exited: watch::Sender::new(false),
            writer: writer_tx,
            master: Some(child.master),
            events: hub.events,
            store: hub.store,
            run_id: hub.run_id,
            engine: Mutex::new(engine),
            psp: Mutex::new(PspRecords::default()),
            feed: hub.feed,
            hibernating: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(0),
            typing: tokio::sync::Mutex::new(()),
            output_bytes: AtomicU64::new(0),
            done_means_idle,
            io: hub.io.clone(),
            recorder: Mutex::new(recorder),
            record_dir: Mutex::new(s.record.map(|(d, _)| d)),
            structured: None,
        });
        session.save(Some(s.launch));
        tokio::spawn(writer_task(child.writer, writer_rx));
        tokio::spawn(Arc::clone(&session).reader_task(child.reader));
        tokio::spawn(Arc::clone(&session).waiter_task(child.child));
        Ok(session)
    }

    /// A session with no process: resumable into its conversation (SPEC §7.4).
    pub fn dormant(
        id: SessionId,
        d: DormantSpec,
        size: TermSize,
        scrollback: usize,
        hub: Hub,
    ) -> Arc<Self> {
        let mut emu = AlacrittyEmulator::new(size, scrollback);
        let tracker = FrameTracker::new(&mut emu);
        let (writer_tx, _) = mpsc::unbounded_channel();
        let mut meta = new_meta(d.name, d.kind, d.argv, d.cwd, d.place);
        meta.state = ProcessState::Dormant;
        meta.ended_ms = d.ended_ms;
        meta.adapter = d.adapter;
        meta.conversation = d.conversation;
        meta.structured = d.structured;
        meta.title = d.title;
        let exited = watch::Sender::new(true);
        Arc::new(Self {
            id,
            meta: Mutex::new(meta),
            term: Mutex::new(Term { emu, tracker }),
            changed: watch::Sender::new(0),
            exited,
            writer: writer_tx,
            master: None,
            events: hub.events,
            store: hub.store,
            run_id: hub.run_id,
            engine: Mutex::new(Engine::default()),
            psp: Mutex::new(PspRecords::default()),
            feed: hub.feed,
            hibernating: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(0),
            typing: tokio::sync::Mutex::new(()),
            output_bytes: AtomicU64::new(0),
            done_means_idle: true,
            io: hub.io.clone(),
            recorder: Mutex::new(None),
            record_dir: Mutex::new(None),
            structured: None,
        })
    }

    pub fn kind(&self) -> SessionKind {
        self.meta.lock().expect("meta").kind
    }

    pub fn adapter(&self) -> Option<String> {
        self.meta.lock().expect("meta").adapter.clone()
    }

    pub fn status(&self) -> Option<AgentStatus> {
        self.engine.lock().expect("engine").record.as_ref().map(|r| r.status)
    }

    /// Raw bytes to the program (answers, stop keys).
    pub fn write_raw(&self, bytes: &[u8]) {
        self.write_input(bytes.to_vec(), bytes == b"\x1a");
    }

    /// Write input to the program; for Ctrl-Z, watch whether it suspends itself.
    fn write_input(&self, bytes: Vec<u8>, ctrl_z: bool) {
        let suspending = ctrl_z.then(|| self.watch_suspend()).flatten();
        if !bytes.is_empty() {
            let _ = self.writer.send(WriterMsg::Bytes(bytes));
        }
        if let Some(watch) = suspending {
            tokio::spawn(watch);
        }
    }

    pub(crate) fn signal(&self, list: Vec<Sourced>) {
        if !list.is_empty() {
            let _ = self.feed.send(Feed::Signals(self.id, list));
        }
    }

    pub fn info(&self, handle: String) -> SessionInfo {
        let meta = self.meta.lock().expect("meta");
        let (size, title) = {
            let term = self.term.lock().expect("term");
            (term.emu.size(), term.emu.title().map(str::to_owned))
        };
        let fg = meta
            .state
            .is_live()
            .then(|| self.master.as_ref().and_then(ptyro_pty::foreground))
            .flatten();
        let live_cwd =
            meta.reported_cwd.clone().or_else(|| fg.as_ref().and_then(|(_, cwd)| cwd.clone()));
        let foreground = fg.map(|(name, _)| name);
        // One guard: two `lock()`s in the struct literal below would deadlock on each other.
        let (status, activity) = {
            let e = self.engine.lock().expect("engine");
            (e.record.clone(), e.activity.clone())
        };
        SessionInfo {
            id: self.id,
            handle,
            name: meta.name.clone(),
            kind: meta.kind,
            argv: meta.argv.clone(),
            cwd: meta.cwd.clone(),
            state: meta.state.clone(),
            size,
            pid: meta.pid,
            created_ms: self.id.created_ms(),
            ended_ms: meta.ended_ms,
            viewers: meta.viewers,
            controller: meta.controller,
            title: meta.title.clone().or_else(|| title.clone()),
            window_title: title,
            foreground,
            live_cwd,
            program_status: meta.program_status.clone(),
            progress: meta.progress,
            last_command_exit: meta.last_command_exit,
            profile: meta.place.profile.clone(),
            worktree: meta.place.worktree.clone(),
            project: meta.place.project.clone(),
            workspace: meta.place.workspace.clone(),
            agent: meta.place.agent.clone(),
            script: meta.place.script.clone(),
            ports: meta.ports.clone(),
            status,
            attention: meta.attention.clone(),
            activity,
            conversation: meta.conversation.clone(),
            children: meta.children.clone(),
            dangerous: meta.dangerous,
            prompt_pending: meta.prompt_pending,
            last_prompt_ms: meta.last_prompt_ms,
            turns: meta.turns,
            last_output_ms: Some(self.last_output_ms.load(Ordering::Relaxed)).filter(|t| *t > 0),
            verify: meta.verify.clone(),
            run: meta.run.as_ref().map(|(r, step)| format!("{r}/{step}")),
            held: false,
            structured: self.structured.as_ref().map(|s| s.protocol.clone()),
            task: meta.first_prompt.as_deref().map(|p| ptyro_core::session::brief(p, 200)),
            last_reply: meta.last_reply.as_deref().map(|r| ptyro_core::session::brief(r, 300)),
            command: meta.command.clone(),
            inner_agent: meta.inner_agent.clone(),
            summary: meta.summary.clone(),
        }
    }

    /// Remember this session in the store (fire and forget); `launch` only when it changes.
    pub fn save(&self, launch: Option<serde_json::Value>) {
        let Some(store) = &self.store else { return };
        let m = self.meta.lock().expect("meta").clone();
        store.save_session(SessionRecord {
            id: self.id.full(),
            name: m.name,
            kind: m.kind,
            argv: m.argv,
            cwd: m.cwd.to_string_lossy().into_owned(),
            state: m.state,
            pid: m.pid,
            daemon_run: self.run_id.clone(),
            created_ms: self.id.created_ms(),
            ended_ms: m.ended_ms,
            profile: m.place.profile,
            worktree: m.place.worktree,
            project: m.place.project,
            workspace: m.place.workspace,
            agent: m.place.agent,
            script: m.place.script,
            launch,
            conversation: m.conversation,
            title: m.title,
        });
    }

    pub fn record_event(&self, kind: &'static str, detail: serde_json::Value) {
        if let Some(store) = &self.store {
            store.event(self.id.full(), kind, detail);
        }
    }

    pub fn rename(&self, name: String) {
        self.meta.lock().expect("meta").name = name.clone();
        self.record_event("rename", serde_json::json!({ "name": name }));
        self.save(None);
    }

    pub fn size(&self) -> TermSize {
        self.term.lock().expect("term").emu.size()
    }

    /// The last `lines` lines of history plus the screen, frozen (SPEC §5.4).
    pub fn scrollback(&self, lines: u32) -> ptyro_core::screen::ScrollbackDoc {
        let term = self.term.lock().expect("term");
        ptyro_term::frame::scrollback_doc(&term.emu, lines)
    }

    /// Like [`scrollback`](Self::scrollback), reaching past what memory keeps: the recording is
    /// replayed into a scratch emulator with room for `lines` (M8.2, M8.3).
    pub fn scrollback_deep(&self, lines: u32) -> ptyro_core::screen::ScrollbackDoc {
        let (have, size) = {
            let term = self.term.lock().expect("term");
            (term.emu.scrollback_len(), term.emu.size())
        };
        let dir = self.record_dir.lock().expect("record_dir").clone();
        if lines as usize <= have || dir.is_none() {
            return self.scrollback(lines);
        }
        let Ok(recs) = ptyro_term::record::read_dir(&dir.unwrap_or_default()) else {
            return self.scrollback(lines);
        };
        let mut emu = AlacrittyEmulator::new(size, (lines as usize).min(1_000_000));
        ptyro_term::record::replay(&recs, &mut emu);
        if emu.size() != size {
            emu.resize(size);
        }
        // The recording ring may hold less than memory: take the deeper of the two.
        if emu.scrollback_len() <= have {
            return self.scrollback(lines);
        }
        ptyro_term::frame::scrollback_doc(&emu, lines)
    }

    /// Bring a session's last screen back from its recording (a dormant session after a
    /// restart, M8.2).
    pub fn load_recording(&self, dir: PathBuf) {
        if let Ok(recs) = ptyro_term::record::read_dir(&dir) {
            let mut term = self.term.lock().expect("term");
            let term = &mut *term;
            ptyro_term::record::replay(&recs, &mut term.emu);
            term.tracker.update(&mut term.emu);
        }
        *self.record_dir.lock().expect("record_dir") = Some(dir);
        self.bump();
    }

    fn record(&self, bytes: &[u8], term: &mut Term) {
        let mut rec = self.recorder.lock().expect("recorder");
        let Some(r) = rec.as_mut() else { return };
        if let Err(e) = r.output(bytes, || keyframe(term)) {
            // A full disk mustn't cost the session its output: stop recording it.
            tracing::warn!(session = %self.id, "recorder off: {e}");
            *rec = None;
        }
    }

    /// Feed the emulator, surviving its panics (M8.5): a crash report with the bytes that did
    /// it, then a fresh emulator. The program never notices; its screen is redrawn.
    fn feed_isolated(&self, term: &mut Term, bytes: &[u8], out: &mut FeedOutput) {
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| term.emu.feed(bytes, out)));
        if r.is_ok() {
            return;
        }
        let tail = &bytes[bytes.len().saturating_sub(4096)..];
        // `term` is held: never wait on `meta` here (`info()` takes meta, then term).
        let name = self.meta.try_lock().map(|m| m.name.clone()).unwrap_or_default();
        let path = crate::watchdog::report(
            &format!("the emulator panicked on output from session {name} ({})", self.id),
            &format!("last {} bytes fed:\n{}", tail.len(), crate::watchdog::escape(tail)),
        );
        let size = term.emu.size();
        let scrollback = term.emu.scrollback_limit();
        term.emu = AlacrittyEmulator::new(size, scrollback);
        term.tracker = FrameTracker::new(&mut term.emu);
        *out = FeedOutput::default();
        tracing::error!(session = %self.id, "emulator reset after a panic");
        let text = format!(
            "ptyro's terminal emulator crashed on this program's output, so its screen was reset (the program kept running){}",
            path.map(|p| format!("; report: {}", p.display())).unwrap_or_default()
        );
        let _ = self.feed.send(Feed::Signals(
            self.id,
            vec![sourced(Signal::Notify { text }, SignalSource::Process, Confidence::High)],
        ));
        self.nudge_redraw();
    }

    /// Roughly what its terminal state costs in memory (alacritty keeps ≈ 24 bytes a cell).
    pub fn memory_estimate(&self) -> u64 {
        let term = self.term.lock().expect("term");
        let size = term.emu.size();
        ((term.emu.scrollback_len() + size.rows as usize) * size.cols as usize * 24) as u64
    }

    pub fn recording_bytes(&self) -> u64 {
        self.recorder.lock().expect("recorder").as_ref().map_or(0, |r| r.bytes())
    }

    pub fn is_live(&self) -> bool {
        self.meta.lock().expect("meta").state.is_live()
    }

    /// The PTY's foreground process group (a running tool, or the agent itself).
    pub fn foreground_pid(&self) -> Option<u32> {
        self.master.as_ref().and_then(ptyro_pty::foreground_pid)
    }

    pub fn pid(&self) -> Option<u32> {
        self.meta.lock().expect("meta").pid
    }

    pub(crate) fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    async fn reader_task(self: Arc<Self>, mut reader: ptyro_pty::OwnedReadPty) {
        let mut buf = vec![0u8; 64 * 1024];
        let mut pause = self.io.pause.clone();
        self.io.readers.fetch_add(1, Ordering::SeqCst);
        loop {
            // A live upgrade: stop between reads (the kernel buffers output meanwhile), so the
            // emulator image misses nothing. Unparks if the upgrade is called off.
            if *pause.borrow_and_update() {
                self.io.parked.fetch_add(1, Ordering::SeqCst);
                let _ = pause.wait_for(|p| !*p).await;
                self.io.parked.fetch_sub(1, Ordering::SeqCst);
            }
            let deadline = self.term.lock().expect("term").emu.sync_deadline();
            let read = tokio::select! {
                r = reader.read(&mut buf) => Some(r),
                () = sleep_until(deadline) => None,
                _ = pause.changed() => continue,
            };
            let mut out = FeedOutput::default();
            let mut n_read = 0;
            let changed = {
                let mut term = self.term.lock().expect("term");
                let term = &mut *term;
                match read {
                    // Linux reports EIO once the child side is gone: any error is EOF.
                    Some(Ok(0) | Err(_)) => break,
                    Some(Ok(n)) => {
                        n_read = n;
                        crate::watchdog::BYTES_READ.fetch_add(n as u64, Ordering::Relaxed);
                        self.record(&buf[..n], term);
                        self.feed_isolated(term, &buf[..n], &mut out)
                    }
                    None => term.emu.expire_sync(&mut out),
                }
                term.tracker.update(&mut term.emu)
            };
            // Visible change from a real amount of output (a spinner frame is a few bytes).
            if changed && n_read >= 64 {
                self.last_output_ms.store(now_ms(), Ordering::Relaxed);
                self.output_bytes.fetch_add(n_read as u64, Ordering::Relaxed);
            }
            if !out.replies.is_empty() {
                let _ = self.writer.send(WriterMsg::Bytes(out.replies));
            }
            self.handle_events(out.events);
            if changed {
                self.bump();
            }
        }
        self.io.readers.fetch_sub(1, Ordering::SeqCst);
        tracing::debug!(session = %self.id, "pty closed");
    }

    /// The effective OSC 7501 record as signals.
    fn psp_signals(&self, r: Option<&ptyro_term::PspReport>, was_active: bool) -> Vec<Signal> {
        let Some(r) = r else {
            return if was_active { vec![Signal::status(AgentStatus::Idle)] } else { Vec::new() };
        };
        let st = |status| Signal::Status {
            status,
            kind: r.kind.clone(),
            detail: r.msg.clone(),
            progress: r.progress,
        };
        match r.state.as_str() {
            "working" => vec![st(AgentStatus::Working)],
            "blocked" => vec![st(AgentStatus::Blocked)],
            "idle" => vec![Signal::status(AgentStatus::Idle)],
            "done" if self.done_means_idle => {
                vec![Signal::status(AgentStatus::Idle), Signal::TurnEnd]
            }
            "done" => vec![st(AgentStatus::Done)],
            "error" => vec![st(AgentStatus::Errored)],
            _ => Vec::new(),
        }
    }

    fn psp_raw(&self) {
        let p = self.psp.lock().expect("psp");
        let raw = p.effective().map(|r| ProgramStatus {
            state: r.state.clone(),
            kind: r.kind.clone(),
            msg: r.msg.clone(),
            app: r.app.clone(),
            progress: r.progress,
            at_ms: now_ms(),
        });
        drop(p);
        self.meta.lock().expect("meta").program_status = raw;
    }

    fn was_active(&self) -> bool {
        self.psp
            .lock()
            .expect("psp")
            .effective()
            .is_some_and(|r| r.state == "working" || r.state == "blocked")
    }

    fn handle_events(&self, events: Vec<TermEvent>) {
        let session = self.id;
        let kind = self.kind();
        let mut signals: Vec<Sourced> = Vec::new();
        let high = |s| sourced(s, SignalSource::Osc, Confidence::High);
        for ev in events {
            let domain = match ev {
                TermEvent::Bell => DomainEvent::Bell { session },
                TermEvent::Clipboard(text) => DomainEvent::Clipboard { session, text },
                TermEvent::Notification(text) => {
                    let body = text.split_once(';').map_or(text.as_str(), |(_, b)| b).to_owned();
                    signals.push(sourced(
                        Signal::Notify { text: body },
                        SignalSource::Notification,
                        Confidence::Low,
                    ));
                    DomainEvent::Notification { session, text }
                }
                TermEvent::ProgramStatus(r) => {
                    self.record_event("osc7501", serde_json::json!({ "state": r.state, "id": r.id, "kind": r.kind, "msg": r.msg }));
                    let was = self.was_active();
                    let changed = self.psp.lock().expect("psp").report(r);
                    self.psp_raw();
                    if !changed {
                        continue;
                    }
                    let eff = self.psp.lock().expect("psp").effective().cloned();
                    signals.extend(self.psp_signals(eff.as_ref(), was).into_iter().map(high));
                    DomainEvent::SessionUpdated { info: self.info(String::new()) }
                }
                TermEvent::Reset => {
                    self.psp.lock().expect("psp").reset();
                    self.psp_raw();
                    continue;
                }
                TermEvent::ForeignStatus { state, msg } => {
                    if let Some(status) = AgentStatus::parse(&state) {
                        signals.push(sourced(
                            Signal::Status { status, kind: None, detail: msg, progress: None },
                            SignalSource::Osc,
                            Confidence::Low,
                        ));
                    }
                    continue;
                }
                TermEvent::Cwd(path) => {
                    let mut meta = self.meta.lock().expect("meta");
                    if meta.reported_cwd.as_ref() == Some(&path) {
                        continue;
                    }
                    meta.reported_cwd = Some(path);
                    drop(meta);
                    DomainEvent::SessionUpdated { info: self.info(String::new()) }
                }
                TermEvent::Progress { state, percent } => {
                    // OSC 9;4 stops counting once the program speaks OSC 7501 (spec rev 0.4).
                    if self.psp.lock().expect("psp").seen {
                        continue;
                    }
                    self.meta.lock().expect("meta").progress =
                        (state != 0).then_some(Progress { state, percent });
                    DomainEvent::SessionUpdated { info: self.info(String::new()) }
                }
                TermEvent::PromptMark { mark, exit, cmdline } => {
                    match mark {
                        'A' => {
                            let was = self.was_active();
                            if self.psp.lock().expect("psp").drop_active() {
                                self.psp_raw();
                                let eff = self.psp.lock().expect("psp").effective().cloned();
                                signals.extend(
                                    self.psp_signals(eff.as_ref(), was).into_iter().map(high),
                                );
                            }
                        }
                        'C' if kind == SessionKind::Shell => {
                            let mut m = self.meta.lock().expect("meta");
                            // An agent started by hand says what it's doing itself (UX1.4).
                            let inner = m.inner_agent.is_some();
                            m.command_since_ms = Some(now_ms());
                            m.command = Some(ptyro_core::session::ShellCommand {
                                text: cmdline.map(|c| ptyro_core::session::brief(&c, 200)),
                                started_ms: now_ms(),
                                ended_ms: None,
                                exit: None,
                            });
                            drop(m);
                            if !inner {
                                signals.push(high(Signal::status(AgentStatus::Working)));
                            }
                        }
                        'D' => {
                            let mut m = self.meta.lock().expect("meta");
                            m.last_command_exit = exit;
                            if let Some(c) = m.command.as_mut().filter(|c| c.ended_ms.is_none()) {
                                c.ended_ms = Some(now_ms());
                                c.exit = exit;
                            }
                            let ran = m.command_since_ms.take();
                            drop(m);
                            if kind == SessionKind::Shell && ran.is_some() {
                                let detail = exit.map(|e| format!("exit {e}"));
                                signals.push(high(Signal::Status {
                                    status: AgentStatus::Idle,
                                    kind: None,
                                    detail,
                                    progress: None,
                                }));
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                TermEvent::Title(Some(t)) => {
                    // An agent started by hand in a shell: its title too, at low confidence.
                    let inner = self.meta.lock().expect("meta").inner_agent.clone();
                    let (adapter, confidence) = match (self.adapter(), inner) {
                        (Some(a), _) => (Some(a), Confidence::Medium),
                        (None, Some(a)) => (Some(a), Confidence::Low),
                        (None, None) => (None, Confidence::Medium),
                    };
                    if let Some(sig) = adapter.and_then(|a| ptyro_agents::adapter(&a).on_title(&t))
                    {
                        signals.push(sourced(sig, SignalSource::Title, confidence));
                    }
                    continue;
                }
                // Titles travel in screen frames too.
                TermEvent::Title(None) => continue,
            };
            let _ = self.events.send(domain);
        }
        self.signal(signals);
    }

    async fn waiter_task(self: Arc<Self>, mut child: tokio::process::Child) {
        let status = child.wait().await;
        let state = match status {
            Ok(s) => {
                use std::os::unix::process::ExitStatusExt;
                ProcessState::Exited { code: s.code(), signal: s.signal() }
            }
            Err(e) => ProcessState::Failed { error: e.to_string() },
        };
        self.finish(state);
    }

    /// A child inherited from the previous daemon image: the reaper tells us how it ended.
    async fn reaped_task(
        self: Arc<Self>,
        ended: tokio::sync::oneshot::Receiver<ptyro_pty::handoff::Ended>,
    ) {
        let state = match ended.await {
            Ok((code, signal)) => ProcessState::Exited { code, signal },
            Err(_) => ProcessState::Failed { error: "lost track of the process".into() },
        };
        self.finish(state);
    }

    pub(crate) fn finish(self: &Arc<Self>, state: ProcessState) {
        {
            let mut meta = self.meta.lock().expect("meta");
            // Hibernated agents stay resumable (SPEC §7.4).
            meta.state = if self.hibernating.load(Ordering::Relaxed) && meta.conversation.is_some()
            {
                ProcessState::Dormant
            } else {
                state
            };
            meta.ended_ms = Some(now_ms());
            meta.pid = None;
        }
        self.psp.lock().expect("psp").drop_active();
        self.psp_raw();
        tracing::info!(session = %self.id, "exited");
        let state = self.meta.lock().expect("meta").state.clone();
        self.record_event("process", serde_json::to_value(&state).unwrap_or_default());
        self.save(None);
        // send_replace: `send` drops the value when nobody is waiting yet, and a later
        // `wait_exit` would then wait forever.
        self.exited.send_replace(true);
        self.bump();
        let _ = self.feed.send(Feed::Exited(self.id));
    }

    pub async fn wait_exit(&self) {
        let mut rx = self.exited.subscribe();
        let _ = rx.wait_for(|e| *e).await;
    }

    pub async fn input(&self, event: &InputEvent) {
        if let Some(st) = &self.structured {
            st.key(self, event);
            return;
        }
        let bytes = {
            let term = self.term.lock().expect("term");
            ptyro_term::input::encode(event, &term.emu.modes())
        };
        self.write_input(bytes, is_ctrl_z(event));
    }

    /// Ctrl-Z is about to reach the session's own process, in raw mode. It leads the PTY's session,
    /// so its process group is orphaned and the kernel drops the SIGTSTP it sends itself. A
    /// program that suspends itself (Claude) then waits, with the terminal back in cooked mode,
    /// for a SIGCONT that no shell will ever send: send it, and say so. A job in a shell session
    /// isn't the session's own process; its shell can `fg` it.
    fn watch_suspend(&self) -> Option<impl std::future::Future<Output = ()> + use<>> {
        let pid = self.pid()?;
        let master = self.master.as_ref()?;
        if self.foreground_pid() != Some(pid) || ptyro_pty::is_raw(master) != Some(true) {
            return None;
        }
        let (master, events, session) = (master.try_clone().ok()?, self.events.clone(), self.id);
        Some(async move {
            let deadline = tokio::time::Instant::now() + SUSPEND_WAIT;
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(25)).await;
                if ptyro_pty::foreground_pid(&master) != Some(pid) {
                    return;
                }
                if ptyro_pty::is_raw(&master) == Some(false) {
                    if ptyro_pty::signal_group(pid, ptyro_pty::Signal::CONT).is_ok() {
                        tracing::info!(%session, "resumed a suspended session");
                        let _ = events.send(DomainEvent::SessionResumed { session });
                    }
                    return;
                }
            }
        })
    }

    /// Type text. Multi-line text is pasted (bracketed when the program asked for it) so a shell
    /// or agent doesn't execute it line by line; `enter` then submits.
    pub async fn send_text(&self, text: &str, enter: bool) {
        if let Some(st) = &self.structured {
            st.send_text(self, text, enter);
            return;
        }
        let _typist = self.typing.lock().await;
        let bytes = if text.contains('\n') {
            let term = self.term.lock().expect("term");
            ptyro_term::input::encode_paste(text, &term.emu.modes())
        } else {
            text.as_bytes().to_vec()
        };
        if !bytes.is_empty() {
            let _ = self.writer.send(WriterMsg::Bytes(bytes));
        }
        if enter {
            // Enter as a real key press, after a pause: agents treat a burst ending in `\r` as
            // pasted text, and with the kitty keyboard protocol on (Claude) a bare `\r` byte isn't
            // Enter at all (verified with Claude Code 2.1.304, SPEC §20.1).
            if !text.is_empty() {
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            let key = ptyro_core::input::KeyEvent::new(
                ptyro_core::input::KeyCode::Enter,
                ptyro_core::input::Mods::NONE,
            );
            self.input(&InputEvent::Key { key }).await;
        }
    }

    pub async fn resize(&self, size: TermSize) {
        let changed = {
            let mut term = self.term.lock().expect("term");
            let term = &mut *term;
            if term.emu.size() == size {
                return;
            }
            term.emu.resize(size);
            term.tracker.update(&mut term.emu)
        };
        if let Some(r) = self.recorder.lock().expect("recorder").as_mut() {
            let _ = r.resize(size);
        }
        let _ = self.writer.send(WriterMsg::Resize(size));
        if changed {
            self.bump();
        }
    }

    pub fn set_default_colors(&self, fg: Option<[u8; 3]>, bg: Option<[u8; 3]>) {
        self.term.lock().expect("term").emu.set_default_colors(fg, bg);
    }

    pub fn read_screen(&self, format: ScreenFormat) -> String {
        let mut term = self.term.lock().expect("term");
        let term = &mut *term;
        let mut cache = ScreenCache::new(term.emu.size());
        let delta = term.tracker.build(&term.emu, None, &mut StyleTable::default());
        cache.apply(true, term.tracker.seq(), &delta);
        match format {
            ScreenFormat::Plain => cache.plain_text(),
            ScreenFormat::Ansi => cache.ansi_text(),
        }
    }

    /// Stop the process group: a specific signal, or the graceful sequence (waits for exit).
    pub async fn kill(&self, signal: Option<i32>) -> std::io::Result<()> {
        let Some(pid) = self.pid().filter(|_| self.is_live()) else { return Ok(()) };
        match signal {
            Some(n) => {
                let sig = ptyro_pty::Signal::from_named_raw(n).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad signal {n}"))
                })?;
                ptyro_pty::signal_group(pid, sig)
            }
            None if self.meta.lock().expect("meta").kind == SessionKind::Script => {
                ptyro_pty::terminate_with(pid, self.wait_exit(), SCRIPT_STOP).await;
                Ok(())
            }
            None => {
                ptyro_pty::terminate(pid, self.wait_exit(), STOP_GRACE).await;
                Ok(())
            }
        }
    }
}

fn open_recorder(
    r: Option<&(PathBuf, u64)>,
    size: TermSize,
) -> Option<ptyro_term::record::Recorder> {
    let (dir, max) = r?;
    ptyro_term::record::Recorder::open(dir, size, *max)
        .map_err(|e| tracing::warn!("recorder {}: {e}", dir.display()))
        .ok()
}

/// The screen as ANSI that redraws it: where a ring segment starts (M8.2).
fn keyframe(term: &mut Term) -> (Vec<u8>, TermSize) {
    let size = term.emu.size();
    let mut cache = ScreenCache::new(size);
    let delta = term.tracker.build(&term.emu, None, &mut StyleTable::default());
    cache.apply(true, term.tracker.seq(), &delta);
    let c = term.emu.cursor();
    let mut out = b"\x1b[0m\x1b[H\x1b[2J".to_vec();
    out.extend_from_slice(cache.ansi_text().replace('\n', "\r\n").as_bytes());
    out.extend_from_slice(format!("\x1b[0m\x1b[{};{}H", c.y + 1, c.x + 1).as_bytes());
    (out, size)
}

// ── live upgrade (M8.1) ─────────────────────────────────────────────────────

impl Session {
    /// Close-on-exec again for the PTY master (an upgrade that was called off).
    pub(crate) fn master_private(&self) {
        if let Some(m) = &self.master {
            let _ = ptyro_pty::handoff::private(m);
        }
    }

    /// This session for the handoff image. Call with readers parked. A live session's PTY
    /// master is made inheritable, so it stays open across `execve`.
    pub fn image(&self) -> std::io::Result<SessionImage> {
        let emu = {
            let mut term = self.term.lock().expect("term");
            // Compact MessagePack (cells as arrays, not maps), then zstd: screens are repetitive,
            // so a 10k-line history shrinks ~50×.
            let raw = rmp_serde::to_vec(&term.emu.image()).map_err(std::io::Error::other)?;
            zstd::bulk::compress(&raw, 1)?
        };
        let (status, activity, stale, engine_clocks) = {
            let e = self.engine.lock().expect("engine");
            (e.record.clone(), e.activity.clone(), e.stale_after_ms, e.clocks())
        };
        // Before the struct literal: its `self.meta.lock()` temporary lives to the end of the
        // statement, and `is_live` locks `meta` too.
        let structured = match &self.structured {
            Some(st) => Some(st.image(self.is_live())?),
            None => None,
        };
        let master_fd = match (&self.master, self.is_live()) {
            (Some(m), true) => Some(ptyro_pty::handoff::inheritable(m)?),
            _ => None,
        };
        Ok(SessionImage {
            id: self.id.full(),
            meta: self.meta.lock().expect("meta").clone(),
            status,
            activity,
            emu,
            master_fd,
            last_output_ms: self.last_output_ms.load(Ordering::Relaxed),
            output_bytes: self.output_bytes.load(Ordering::Relaxed),
            done_means_idle: self.done_means_idle,
            stale_after_ms: stale,
            engine_clocks,
            structured,
        })
    }

    /// A session from the previous daemon image: its PTY and child carry on where they were
    /// (live), or just its screen and history (exited, dormant).
    /// The emulator inside an image (the CPU-heavy half of [`adopt`](Self::adopt); run it on
    /// many threads).
    pub fn decode_emulator(img: &SessionImage) -> std::io::Result<AlacrittyEmulator> {
        let raw = zstd::stream::decode_all(img.emu.as_slice())?;
        let emu: ptyro_term::emulator::EmuImage =
            rmp_serde::from_slice(&raw).map_err(std::io::Error::other)?;
        Ok(AlacrittyEmulator::from_image(emu))
    }

    pub fn adopt(
        img: SessionImage,
        emu: AlacrittyEmulator,
        hub: Hub,
        reaper: &crate::handoff::Reaper,
        record: Option<(PathBuf, u64)>,
    ) -> std::io::Result<Arc<Self>> {
        let mut img = img;
        let id: SessionId = img.id.parse().map_err(|_| std::io::Error::other("bad session id"))?;
        let structured_img = img.structured.take();
        let mut emu = emu;
        let size = emu.size();
        let tracker = FrameTracker::new(&mut emu);
        let mut engine = Engine::new(img.stale_after_ms);
        engine.record = img.status;
        engine.activity = img.activity;
        engine.restore_clocks(img.engine_clocks);
        let mut meta = img.meta;
        let piped =
            structured_img.as_ref().is_some_and(crate::structured::StructuredImage::has_pipes);
        let live = (img.master_fd.is_some() || piped) && meta.state.is_live() && meta.pid.is_some();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let mut halves = None;
        // Always take an inherited master: one we don't use is closed, not left inheritable.
        let inherited = img.master_fd.map(ptyro_pty::handoff::adopt).transpose()?;
        let master = match inherited.filter(|_| live) {
            Some(fd) => {
                let (r, w, dup) = ptyro_pty::handoff::adopt_pty(fd)?;
                halves = Some((r, w));
                Some(dup)
            }
            None => {
                if meta.state.is_live() && !live {
                    // It was running but its PTY (or pipes) didn't make it: it is gone for us.
                    meta.state = ProcessState::Failed { error: "lost in a daemon upgrade".into() };
                    meta.pid = None;
                }
                None
            }
        };
        let (structured, resume) = match structured_img {
            Some(si) => {
                let (st, r) = crate::structured::Structured::from_image(si, live)?;
                (Some(st), r)
            }
            None => (None, None),
        };
        let session = Arc::new(Self {
            id,
            meta: Mutex::new(meta),
            term: Mutex::new(Term { emu, tracker }),
            changed: watch::Sender::new(0),
            exited: watch::Sender::new(!live),
            writer: writer_tx,
            master,
            events: hub.events,
            store: hub.store,
            run_id: hub.run_id,
            engine: Mutex::new(engine),
            psp: Mutex::new(PspRecords::default()),
            feed: hub.feed,
            hibernating: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(img.last_output_ms),
            typing: tokio::sync::Mutex::new(()),
            output_bytes: AtomicU64::new(img.output_bytes),
            done_means_idle: img.done_means_idle,
            io: hub.io,
            recorder: Mutex::new(
                record.as_ref().filter(|_| live).and_then(|r| open_recorder(Some(r), size)),
            ),
            record_dir: Mutex::new(record.map(|(d, _)| d)),
            structured: structured.clone(),
        });
        if let Some((r, w)) = halves {
            let pid = session.pid().unwrap_or_default();
            tokio::spawn(writer_task(w, writer_rx));
            tokio::spawn(Arc::clone(&session).reader_task(r));
            tokio::spawn(Arc::clone(&session).reaped_task(reaper.watch(pid)));
        }
        if let Some(r) = resume {
            let pid = session.pid().unwrap_or_default();
            crate::structured::run(Arc::clone(&session), r);
            tokio::spawn(Arc::clone(&session).reaped_task(reaper.watch(pid)));
        }
        Ok(session)
    }

    /// A structured session (M10): the agent runs with pipes, not a PTY; its driver speaks the
    /// protocol and renders the transcript into this session's emulator, so every terminal view
    /// (`read`, the recorder, the web app's canvas) shows it too. The caller starts the driver
    /// (`structured::run`) once the session is registered: the driver's first status goes
    /// through the signal feed, which drops signals for sessions it doesn't know yet.
    pub fn spawn_structured(
        id: SessionId,
        s: Spawned,
        st: Arc<crate::structured::Structured>,
        start: crate::structured::Start,
        hub: Hub,
    ) -> std::io::Result<(Arc<Self>, crate::structured::Resume)> {
        let (child, resume) = crate::structured::spawn_child(&s, &st, start)?;
        let mut emu = AlacrittyEmulator::new(s.size, s.scrollback);
        let tracker = FrameTracker::new(&mut emu);
        let (writer_tx, _) = mpsc::unbounded_channel();
        let recorder = open_recorder(s.record.as_ref(), s.size);
        let mut meta = new_meta(s.name, s.kind, s.argv, s.cwd, s.place);
        meta.state = ProcessState::Running;
        meta.pid = child.id();
        meta.client_env = s.client_env;
        meta.adapter = s.adapter.clone();
        meta.conversation = s.conversation;
        meta.dangerous = s.dangerous;
        meta.structured = Some(st.protocol.clone());
        let mut engine = Engine::new(s.stale_after_ms);
        engine.apply(
            crate::engine::Input {
                source: SignalSource::Process,
                confidence: Confidence::Low,
                status: AgentStatus::Starting,
                kind: None,
                detail: None,
                progress: None,
            },
            now_ms(),
        );
        let session = Arc::new(Self {
            id,
            meta: Mutex::new(meta),
            term: Mutex::new(Term { emu, tracker }),
            changed: watch::Sender::new(0),
            exited: watch::Sender::new(false),
            writer: writer_tx,
            master: None,
            events: hub.events,
            store: hub.store,
            run_id: hub.run_id,
            engine: Mutex::new(engine),
            psp: Mutex::new(PspRecords::default()),
            feed: hub.feed,
            hibernating: AtomicBool::new(false),
            last_output_ms: AtomicU64::new(0),
            typing: tokio::sync::Mutex::new(()),
            output_bytes: AtomicU64::new(0),
            done_means_idle: true,
            io: hub.io.clone(),
            recorder: Mutex::new(recorder),
            record_dir: Mutex::new(s.record.map(|(d, _)| d)),
            structured: Some(st),
        });
        session.save(Some(s.launch));
        tokio::spawn(Arc::clone(&session).waiter_task(child));
        Ok((session, resume))
    }

    pub(crate) fn io_control(&self) -> IoControl {
        self.io.clone()
    }

    /// Show bytes in the session's terminal view (a structured session's rendering): the same
    /// path PTY output takes, recorder included.
    pub(crate) fn view(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let changed = {
            let mut term = self.term.lock().expect("term");
            let term = &mut *term;
            self.record(bytes, term);
            let mut out = FeedOutput::default();
            self.feed_isolated(term, bytes, &mut out);
            term.tracker.update(&mut term.emu)
        };
        if changed {
            self.last_output_ms.store(now_ms(), Ordering::Relaxed);
            self.output_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            self.bump();
        }
    }

    /// Ask the program in front to redraw (SIGWINCH, size unchanged): state the image can't
    /// carry (scroll regions, the palette) comes back with its next frame.
    pub fn nudge_redraw(&self) {
        if let Some(fg) = self.foreground_pid() {
            let _ = ptyro_pty::signal_group(fg, ptyro_pty::Signal::WINCH);
        }
    }
}

/// How long after Ctrl-Z a program gets to leave raw mode before it counts as not suspending.
const SUSPEND_WAIT: Duration = Duration::from_secs(1);

fn is_ctrl_z(event: &InputEvent) -> bool {
    match event {
        InputEvent::Key { key } => {
            key.kind == KeyKind::Press
                && key.mods == Mods(Mods::CTRL)
                && matches!(key.code, KeyCode::Char { c: 'z' | 'Z' })
        }
        InputEvent::Raw { bytes } => bytes == b"\x1a",
        _ => false,
    }
}

async fn sleep_until(t: Option<std::time::Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
    }
}

async fn writer_task(mut w: ptyro_pty::OwnedWritePty, mut rx: mpsc::UnboundedReceiver<WriterMsg>) {
    while let Some(msg) = rx.recv().await {
        let ok = match msg {
            WriterMsg::Bytes(b) => w.write_all(&b).await.is_ok(),
            WriterMsg::Resize(s) => w.resize(ptyro_pty::pty_size(s)).is_ok(),
        };
        if !ok {
            // The child is gone; keep draining so senders never block.
            continue;
        }
    }
}
