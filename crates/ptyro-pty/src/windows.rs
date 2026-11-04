//! Windows: each session in a pseudoconsole (ConPTY) and a Job Object (WIN5, SPEC §20.13).
//!
//! The ConPTY is the packaged one when its `conpty.dll` and `OpenConsole.exe` sit beside
//! `ptyro.exe` (WIN0.5 g: it passes the program's bytes through), else Windows' own. Its pipes are
//! anonymous, so they can't be overlapped: each is driven through tokio's blocking pool, as
//! `tokio::fs::File` does. The Job Object holds the program and everything it starts, so a stop
//! reaches the whole tree, and nothing outlives ptyro (`KILL_ON_JOB_CLOSE`).
//!
//! What Unix does with signals, Windows does differently ([`signal_group`]): `INT` is a Ctrl+C
//! typed into the console, `HUP` and `TERM` close the console (each program gets
//! `CTRL_CLOSE_EVENT`, and Windows ends it a few seconds later), `KILL` ends the job.
#![allow(unsafe_code)] // Win32 calls; each block says what it relies on.

use std::collections::HashMap;
use std::ffi::{OsStr, c_void};
use std::io::{self, Write as _};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use ptyro_core::TermSize;
use ptyro_core::sync::MutexExt;
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::cmdline;
use crate::{PtyChild, SpawnRequest};

// ── signals ───────────────────────────────────────────────────────────────────────────────

/// A signal, by its POSIX number (what a client sends); [`signal_group`] says what each does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signal(i32);

impl Signal {
    pub const HUP: Self = Self(1);
    pub const INT: Self = Self(2);
    pub const KILL: Self = Self(9);
    pub const TERM: Self = Self(15);
    pub const CONT: Self = Self(18);
    pub const WINCH: Self = Self(28);

    /// The signal numbered `n`, if ptyro knows it.
    pub fn from_named_raw(n: i32) -> Option<Self> {
        [Self::HUP, Self::INT, Self::KILL, Self::TERM, Self::CONT, Self::WINCH]
            .into_iter()
            .find(|s| s.0 == n)
    }

    pub fn as_raw(self) -> i32 {
        self.0
    }
}

// ── handles ───────────────────────────────────────────────────────────────────────────────

/// A handle this crate owns, closed on drop.
#[derive(Debug)]
struct Owned(HANDLE);

// SAFETY: a kernel handle may be used from any thread; each `Owned` is closed exactly once.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: we own the handle and nothing uses it after this.
        unsafe { CloseHandle(self.0) };
    }
}

fn last_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

// ── which ConPTY ──────────────────────────────────────────────────────────────────────────

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

#[derive(Clone, Copy)]
struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

/// The packaged ConPTY's `conpty.dll` in use, or `None`: Windows' own.
struct Chosen {
    api: Api,
    packaged: Option<PathBuf>,
}

static CONPTY: OnceLock<Chosen> = OnceLock::new();

fn conpty() -> &'static Chosen {
    CONPTY.get_or_init(|| {
        let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
        match beside.as_deref().and_then(|d| load_packaged(d).ok()) {
            Some((api, dll)) => Chosen { api, packaged: Some(dll) },
            None => Chosen {
                api: Api {
                    create: CreatePseudoConsole,
                    resize: ResizePseudoConsole,
                    close: ClosePseudoConsole,
                },
                packaged: None,
            },
        }
    })
}

/// Microsoft's packaged ConPTY from `dir`: `conpty.dll`, which starts the `OpenConsole.exe`
/// beside it.
fn load_packaged(dir: &Path) -> io::Result<(Api, PathBuf)> {
    let dll = dir.join("conpty.dll");
    if !dll.is_file() || !dir.join("OpenConsole.exe").is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no packaged ConPTY here"));
    }
    // SAFETY: a full path, so no search order is involved; the functions are looked up by name
    // and given the types microsoft/terminal declares them with (`conpty.h`).
    unsafe {
        let h = LoadLibraryW(wide(dll.as_os_str()).as_ptr());
        if h.is_null() {
            return Err(last_error(&format!("loading {}", dll.display())));
        }
        let get = |name: &[u8]| {
            GetProcAddress(h, name.as_ptr()).ok_or_else(|| {
                io::Error::other(format!(
                    "{} lacks {}",
                    dll.display(),
                    String::from_utf8_lossy(name)
                ))
            })
        };
        let create = get(b"ConptyCreatePseudoConsole\0")?;
        let resize = get(b"ConptyResizePseudoConsole\0")?;
        let close = get(b"ConptyClosePseudoConsole\0")?;
        let api = Api {
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateFn>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, ResizeFn>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, CloseFn>(close),
        };
        Ok((api, dll))
    }
}

/// Which ConPTY sessions get: the packaged one's `conpty.dll` (beside `ptyro.exe`), or `None`
/// for Windows' own (for `doctor` and About).
pub fn packaged_conpty() -> Option<&'static Path> {
    conpty().packaged.as_deref()
}

// ── the pseudoconsole ─────────────────────────────────────────────────────────────────────

/// A pseudoconsole, closed once: by a stop, when its last program is gone, or on drop.
struct Pty {
    hpc: Mutex<Option<HPCON>>,
    api: Api,
}

// SAFETY: an HPCON is a pointer to three handles that ConPTY's functions take from any thread;
// the mutex makes closing it exclusive.
unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

impl Pty {
    fn resize(&self, size: TermSize) -> io::Result<()> {
        let Some(hpc) = *self.hpc.locked() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the console is closed"));
        };
        // SAFETY: `hpc` is open (we hold the lock that closing takes).
        let hr = unsafe { (self.api.resize)(hpc, coord(size)) };
        if hr == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!("ResizePseudoConsole: {hr:#x}")))
        }
    }

    /// Close it: every program attached gets `CTRL_CLOSE_EVENT`, and its output pipe ends once
    /// the console host has written what it has. `ClosePseudoConsole` can wait for the console
    /// host, and so for a program that ignores the close (seen: minutes, on CI), so it runs on a
    /// thread of its own: a stop or a drop never blocks the caller.
    fn close(&self) {
        if let Some(hpc) = self.hpc.locked().take() {
            let close = self.api.close;
            let hpc = Hpc(hpc);
            let run = move || {
                let hpc = hpc;
                // SAFETY: taken out from under the lock, so this is its one close.
                unsafe { close(hpc.0) };
            };
            if std::thread::Builder::new().name("conpty-close".into()).spawn(run).is_err() {
                run();
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
    }
}

/// An HPCON on its way to the thread that closes it.
#[derive(Clone, Copy)]
struct Hpc(HPCON);
// SAFETY: ConPTY's functions take an HPCON from any thread; this one is closed exactly once.
unsafe impl Send for Hpc {}

fn coord(size: TermSize) -> COORD {
    COORD { X: size.cols.min(i16::MAX as u16) as i16, Y: size.rows.min(i16::MAX as u16) as i16 }
}

// ── the session's process and job ─────────────────────────────────────────────────────────

/// What a running session has: its program, its job, and (in a console) the console and a way
/// to type Ctrl+C. A program on plain pipes (a structured agent) has neither.
struct Proc {
    process: Owned,
    job: Owned,
    pty: Option<Arc<Pty>>,
    input: Option<Mutex<std::fs::File>>,
}

impl Proc {
    /// How many processes are in the job, the program included.
    fn active(&self) -> u32 {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is the struct this information class fills, and its size is given.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.0,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { 1 } else { info.ActiveProcesses }
    }

    fn running(&self) -> bool {
        // SAFETY: a zero timeout only asks.
        unsafe { WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 }
    }

    fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: the process handle is open; `code` is written.
        let ok = unsafe { GetExitCodeProcess(self.process.0, &mut code) } != 0;
        (ok && code != STILL_ACTIVE as u32).then_some(code)
    }
}

/// Every session's process by pid, so the pid-based functions (signals, waits, the group's
/// size) reach it. A pid isn't reused while its process handle is open, which we hold.
fn procs() -> &'static Mutex<HashMap<u32, Arc<Proc>>> {
    static PROCS: OnceLock<Mutex<HashMap<u32, Arc<Proc>>>> = OnceLock::new();
    PROCS.get_or_init(Mutex::default)
}

fn proc_of(pid: u32) -> Option<Arc<Proc>> {
    procs().locked().get(&pid).cloned()
}

/// Wait for `proc`'s program to exit, on the blocking pool; its exit code.
async fn wait_exit(proc: Arc<Proc>) -> io::Result<u32> {
    tokio::task::spawn_blocking(move || {
        // SAFETY: the handle stays open: `proc` is held across the wait.
        unsafe { WaitForSingleObject(proc.process.0, INFINITE) };
        proc.exit_code().ok_or_else(|| last_error("GetExitCodeProcess"))
    })
    .await
    .map_err(io::Error::other)?
}

/// A session's program, as `tokio::process::Child` is on Unix.
pub struct Child {
    pid: u32,
    proc: Arc<Proc>,
}

impl Child {
    pub fn id(&self) -> Option<u32> {
        Some(self.pid)
    }

    /// Wait for the program to exit.
    pub async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        use std::os::windows::process::ExitStatusExt;
        Ok(std::process::ExitStatus::from_raw(wait_exit(self.proc.clone()).await?))
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        procs().locked().remove(&self.pid);
    }
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Child").field("pid", &self.pid).finish()
    }
}

// ── reading, writing, resizing ────────────────────────────────────────────────────────────

/// The pseudoconsole's output.
#[derive(Debug)]
pub struct Reader(tokio::fs::File);

impl tokio::io::AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match ready!(Pin::new(&mut self.0).poll_read(cx, buf)) {
            // The console host closed its end: the output is over, as a PTY's EOF is.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Poll::Ready(Ok(())),
            other => Poll::Ready(other),
        }
    }
}

/// The pseudoconsole's input; it also resizes. A write is done once it reached the pipe
/// (`tokio::fs::File` alone would report it done while it still sits in its buffer).
pub struct Writer {
    file: tokio::fs::File,
    pty: Arc<Pty>,
    pending: Option<usize>,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Writer")
    }
}

impl tokio::io::AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let n = match self.pending {
            Some(n) => n,
            None => {
                let n = ready!(Pin::new(&mut self.file).poll_write(cx, buf))?;
                self.pending = Some(n);
                n
            }
        };
        ready!(Pin::new(&mut self.file).poll_flush(cx))?;
        self.pending = None;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_shutdown(cx)
    }
}

/// The pseudoconsole, for the probes (WIN7 reads its modes and process list through it).
#[derive(Clone)]
pub struct Master(#[allow(dead_code)] Arc<Pty>);

impl std::fmt::Debug for Master {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Master")
    }
}

impl Master {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(self.clone())
    }
}

/// Tell the pseudoconsole (and so its program) its new size.
pub fn resize(writer: &Writer, size: TermSize) -> io::Result<()> {
    writer.pty.resize(size)
}

// ── spawning ──────────────────────────────────────────────────────────────────────────────

/// System32's `cmd.exe`, which runs batch files (never `%COMSPEC%`, which anyone can point
/// elsewhere).
fn system_cmd() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer and its length are given; it writes at most that many.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let dir = if n > 0 && n < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
    } else {
        PathBuf::from(r"C:\Windows\System32")
    };
    dir.join("cmd.exe")
}

fn pipe() -> io::Result<(Owned, Owned)> {
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: both out-parameters are written on success; no security attributes, so neither
    // end is inheritable.
    if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) } == 0 {
        return Err(last_error("CreatePipe"));
    }
    Ok((Owned(r), Owned(w)))
}

/// A Job Object that ends everything in it when its last handle closes.
fn job() -> io::Result<Owned> {
    // SAFETY: an unnamed job, no security attributes.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(last_error("CreateJobObject"));
    }
    let job = Owned(job);
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the struct this information class takes, with its size.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    if ok == 0 {
        return Err(last_error("SetInformationJobObject"));
    }
    Ok(job)
}

/// Programs ptyro starts handle Ctrl+C normally, whatever ptyro was started with. A process
/// started with `CREATE_NEW_PROCESS_GROUP` (a detached daemon: `ptyro_core::os::detach`; CI's
/// runners) ignores Ctrl+C, and its children inherit that, so a session's Ctrl+C would do
/// nothing. Undoing it here makes the children inherit normal handling; ptyro's own Ctrl+C
/// handler (`--foreground`) is unaffected.
/// Done before every spawn (it costs nothing): whatever set the flag since, children start
/// clear of it.
fn ctrl_c_for_children() {
    // SAFETY: no handler is added or removed, only the process's "ignore Ctrl+C" flag cleared.
    unsafe { SetConsoleCtrlHandler(None, 0) };
}

/// Start `argv` in a new pseudoconsole of `size`, in a job of its own.
pub fn spawn(req: SpawnRequest<'_>) -> io::Result<PtyChild> {
    ctrl_c_for_children();
    let Some((prog, args)) = req.argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    // Found on the child's PATH, with its PATHEXT, as its shell would.
    let program = cmdline::resolve(
        prog,
        cmdline::env_var(&req.env, "PATH"),
        cmdline::env_var(&req.env, "PATHEXT"),
    )?;
    let cmd = system_cmd();
    let mut line = wide(OsStr::new(&cmdline::command_line(&program, args, &cmd)?));
    let app = if cmdline::is_batch(&program) { cmd } else { program.clone() };
    let mut env = cmdline::env_block(&req.env);
    let cwd = wide(req.cwd.as_os_str());

    let (in_r, in_w) = pipe()?;
    let (out_r, out_w) = pipe()?;
    let chosen = conpty();
    let mut hpc: HPCON = 0;
    // SAFETY: both pipe ends are open; the console host duplicates them, so ours close after.
    let hr = unsafe { (chosen.api.create)(coord(req.size), in_r.0, out_w.0, 0, &mut hpc) };
    drop((in_r, out_w));
    if hr != 0 {
        return Err(io::Error::other(format!("CreatePseudoConsole: HRESULT {:#010x}", hr as u32)));
    }
    let pty = Arc::new(Pty { hpc: Mutex::new(Some(hpc)), api: chosen.api });

    let job = job()?;
    let pi = start(&app, &mut line, &mut env, &cwd, hpc)
        .map_err(|e| io::Error::new(e.kind(), format!("{prog}: {e}")))?;
    let process = Owned(pi.hProcess);
    let thread = Owned(pi.hThread);
    // SAFETY: the process is suspended, so nothing it starts escapes the job; resumed after.
    unsafe {
        if AssignProcessToJobObject(job.0, process.0) == 0 {
            let e = last_error("AssignProcessToJobObject");
            TerminateProcess(process.0, 1);
            return Err(e);
        }
        ResumeThread(thread.0);
    }
    drop(thread);

    // SAFETY: each handle is ours alone from here, given to exactly one `File`.
    let (output, input) = unsafe {
        (std::fs::File::from_raw_handle(out_r.0), std::fs::File::from_raw_handle(in_w.0))
    };
    std::mem::forget(out_r);
    std::mem::forget(in_w);
    let ctrl_c = input.try_clone()?;
    let pid = pi.dwProcessId;
    let proc =
        Arc::new(Proc { process, job, pty: Some(pty.clone()), input: Some(Mutex::new(ctrl_c)) });
    procs().locked().insert(pid, proc.clone());
    watch_until_empty(proc.clone());

    Ok(PtyChild {
        pid,
        child: Child { pid, proc },
        reader: Reader(tokio::fs::File::from_std(output)),
        writer: Writer { file: tokio::fs::File::from_std(input), pty: pty.clone(), pending: None },
        master: Master(pty),
    })
}

/// `CreateProcessW`, suspended, in the pseudoconsole `hpc`, inheriting nothing.
fn start(
    app: &Path,
    line: &mut [u16],
    env: &mut [u16],
    cwd: &[u16],
    hpc: HPCON,
) -> io::Result<PROCESS_INFORMATION> {
    let app = wide(app.as_os_str());
    // SAFETY: the attribute list lives in `buf` for the whole call and is deleted after; the
    // pseudoconsole attribute takes the HPCON by value (its size is given); the standard
    // handles are left empty on purpose (STARTF_USESTDHANDLES with none), or a child of a daemon
    // whose stdout is a pipe would write there instead of to its console (SPEC §20.13).
    unsafe {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut buf = vec![0u8; size];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }
        let set = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            hpc as *const c_void,
            std::mem::size_of::<HPCON>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if set == 0 {
            let e = last_error("UpdateProcThreadAttribute");
            DeleteProcThreadAttributeList(list);
            return Err(e);
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            env.as_mut_ptr().cast(),
            cwd.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        );
        let e = (ok == 0).then(|| last_error("CreateProcess"));
        DeleteProcThreadAttributeList(list);
        match e {
            Some(e) => Err(e),
            None => Ok(pi),
        }
    }
}

/// Close the console once the program and everything it started are gone. Its output pipe
/// doesn't end before that (the console host holds it), so this is what ends the output, as
/// the last process holding a PTY does on Unix.
fn watch_until_empty(proc: Arc<Proc>) {
    let _ = std::thread::Builder::new().name("conpty-watch".into()).spawn(move || {
        // SAFETY: the handle stays open: `proc` is held.
        unsafe { WaitForSingleObject(proc.process.0, INFINITE) };
        while proc.active() > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(pty) = &proc.pty {
            pty.close();
        }
    });
}

// ── signals and waits ─────────────────────────────────────────────────────────────────────

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no such process group")
}

/// What a signal does to a session: `INT` types Ctrl+C into its console; `HUP` and `TERM`
/// close the console (each program gets `CTRL_CLOSE_EVENT`, and Windows ends it soon after);
/// `KILL` ends everything in its job; `CONT` and `WINCH` have nothing to do. `NotFound` once
/// the job is empty, as `ESRCH` is on Unix.
pub fn signal_group(pid: u32, sig: Signal) -> io::Result<()> {
    let proc = proc_of(pid).ok_or_else(gone)?;
    if proc.active() == 0 {
        return Err(gone());
    }
    match sig {
        Signal::KILL => {
            // SAFETY: the job handle is open.
            if unsafe { TerminateJobObject(proc.job.0, 1) } == 0 {
                return Err(last_error("TerminateJobObject"));
            }
        }
        Signal::HUP | Signal::TERM => match &proc.pty {
            Some(pty) => pty.close(),
            // No console to close: a program on pipes is ended.
            None => {
                // SAFETY: the job handle is open.
                if unsafe { TerminateJobObject(proc.job.0, 1) } == 0 {
                    return Err(last_error("TerminateJobObject"));
                }
            }
        },
        Signal::INT => {
            if let Some(input) = &proc.input {
                input.locked().write_all(b"\x03")?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A program started without a console (a structured agent, on pipes) in a job of its own, so
/// a stop reaches everything it starts, as [`spawn`]'s are. What it started before this returns
/// isn't in the job: it is called right after the spawn.
pub fn contain(child: tokio::process::Child) -> io::Result<Child> {
    let pid = child.id().ok_or_else(|| io::Error::other("the program exited at once"))?;
    let raw = child.raw_handle().ok_or_else(|| io::Error::other("no process handle"))?;
    let job = job()?;
    let mut own: HANDLE = std::ptr::null_mut();
    // SAFETY: `raw` is the child's open process handle (`child` is alive for this call); we keep
    // a duplicate of our own, so dropping `child` afterwards leaves ours open.
    unsafe {
        if AssignProcessToJobObject(job.0, raw as HANDLE) == 0 {
            return Err(last_error("AssignProcessToJobObject"));
        }
        let me = GetCurrentProcess();
        if DuplicateHandle(me, raw as HANDLE, me, &mut own, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
            return Err(last_error("DuplicateHandle"));
        }
    }
    let proc = Arc::new(Proc { process: Owned(own), job, pty: None, input: None });
    procs().locked().insert(pid, proc.clone());
    drop(child);
    Ok(Child { pid, proc })
}

/// Linux only (`PR_SET_PDEATHSIG`); nothing elsewhere. A session's job ends with ptyro anyway.
pub fn die_with_parent(_: &mut std::process::Command) {}

/// Unknown on Windows (a console has no termios); WIN7 looks at the console's modes.
pub fn is_raw(_: &Master) -> Option<bool> {
    None
}

/// Wait for `pid` (a session's program) to exit: (exit code, no signal). Its pid stays its own
/// while ptyro holds its handle, so nothing needs to stay unreaped.
pub async fn exited_unreaped(pid: u32) -> io::Result<(Option<i32>, Option<i32>)> {
    let proc = proc_of(pid).ok_or_else(gone)?;
    let code = wait_exit(proc).await?;
    Ok((Some(code as i32), None))
}

/// How many processes are in `pgid`'s job besides the program itself (while it runs).
pub fn others_in_group(pgid: u32) -> usize {
    let Some(proc) = proc_of(pgid) else { return 0 };
    let active = proc.active() as usize;
    active.saturating_sub(usize::from(proc.running()))
}

/// Unknown on Windows until WIN7 (the console's process list).
pub fn foreground_pid(_: &Master) -> Option<u32> {
    None
}

/// How a child that was waited for ended: (exit code, no signal on Windows).
pub fn exit_parts(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), None)
}

/// Start `cmd` in a process group of its own (`CREATE_NEW_PROCESS_GROUP`), so a console Ctrl+C
/// aimed at ptyro doesn't reach it.
pub fn own_group(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{child_env, terminate, until_group_empty};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn start(argv: &[&str]) -> PtyChild {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
        spawn(SpawnRequest {
            argv: &argv,
            cwd: &std::env::temp_dir(),
            env: child_env(std::env::vars_os(), []),
            size: TermSize::new(80, 24),
        })
        .unwrap()
    }

    /// Read until `want` shows up in the output (ConPTY paints, so it may come in pieces).
    async fn read_until(r: &mut Reader, want: &str) -> String {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&out).contains(want) {
            let n = tokio::time::timeout_at(deadline, r.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no {want:?} in {:?}", String::from_utf8_lossy(&out)))
                .unwrap();
            assert!(n > 0, "the output ended before {want:?}: {:?}", String::from_utf8_lossy(&out));
            out.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn signals_keep_their_posix_numbers() {
        assert_eq!(Signal::from_named_raw(9), Some(Signal::KILL));
        assert_eq!(Signal::from_named_raw(15).map(Signal::as_raw), Some(15));
        assert_eq!(Signal::from_named_raw(64), None);
    }

    #[tokio::test]
    async fn a_program_runs_in_the_console_and_its_exit_code_comes_back() {
        let mut c = start(&["cmd.exe", "/d", "/c", "echo ptyro-%PTYRO%& exit /b 7"]);
        read_until(&mut c.reader, "ptyro-1").await;
        let (code, signal) = exited_unreaped(c.pid).await.unwrap();
        assert_eq!((code, signal), (Some(7), None));
        assert_eq!(c.child.wait().await.unwrap().code(), Some(7));
    }

    #[tokio::test]
    async fn typing_and_resizing_reach_the_program() {
        let mut c = start(&["cmd.exe", "/d", "/k", "prompt $G"]);
        read_until(&mut c.reader, ">").await;
        resize(&c.writer, TermSize::new(100, 30)).unwrap();
        c.writer.write_all(b"mode con\r").await.unwrap();
        // `mode con` reports the console's size.
        read_until(&mut c.reader, "100").await;
        c.writer.write_all(b"exit 3\r").await.unwrap();
        assert_eq!(exited_unreaped(c.pid).await.unwrap().0, Some(3));
    }

    #[tokio::test]
    async fn a_stop_ends_the_whole_tree() {
        // cmd starts a second, long-lived process in the same console, then waits too.
        let c = start(&[
            "cmd.exe",
            "/d",
            "/c",
            "start /b ping -n 600 127.0.0.1 >nul & ping -n 600 127.0.0.1 >nul",
        ]);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while others_in_group(c.pid) == 0 {
            assert!(tokio::time::Instant::now() < deadline, "the background ping never started");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let pid = c.pid;
        let started = std::time::Instant::now();
        terminate(
            pid,
            async {
                let _ = exited_unreaped(pid).await;
            },
            (Duration::from_millis(500), Duration::from_millis(500)),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), until_group_empty(pid)).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(12));
        assert!(signal_group(pid, Signal::KILL).is_err(), "the job is empty");
    }

    #[tokio::test]
    async fn a_batch_file_on_the_path_runs_with_its_arguments_intact() {
        let dir = tempfile::tempdir().unwrap();
        // `%1` keeps the argument's quotes, so the batch file's own echo doesn't run the `&`.
        std::fs::write(dir.path().join("say.cmd"), "@echo off\r\necho [%1]\r\n").unwrap();
        let path =
            format!("{};{}", dir.path().display(), std::env::var("PATH").unwrap_or_default());
        let mut env = child_env(std::env::vars_os(), []);
        env.retain(|(k, _)| !k.eq_ignore_ascii_case("PATH"));
        env.push(("PATH".into(), path.into()));
        let argv = vec!["say".to_owned(), "50% & more".to_owned()];
        let mut c =
            spawn(SpawnRequest { argv: &argv, cwd: dir.path(), env, size: TermSize::new(80, 24) })
                .unwrap();
        read_until(&mut c.reader, "[\"50% & more\"]").await;
    }

    #[tokio::test]
    async fn ctrl_c_reaches_the_program() {
        // As CI's runners and a detached daemon start ptyro: ignoring Ctrl+C, which children
        // inherit unless ptyro undoes it.
        // SAFETY: sets this test process's "ignore Ctrl+C" flag; no handler involved.
        unsafe { SetConsoleCtrlHandler(None, 1) };
        let mut c = start(&["ping.exe", "-n", "600", "127.0.0.1"]);
        read_until(&mut c.reader, "127.0.0.1").await;
        signal_group(c.pid, Signal::INT).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(10), exited_unreaped(c.pid)).await;
        assert!(ended.is_ok(), "ping ignored Ctrl+C");
    }

    /// With the packaged ConPTY beside the binary (`make conpty INTO=…`; CI's windows job sets
    /// `PTYRO_TEST_PACKAGED_CONPTY=1`), it is the one used, and an OSC stays where the program
    /// wrote it: Windows' own sends it ahead of the text before it (SPEC §20.13).
    #[tokio::test]
    async fn the_packaged_conpty_keeps_an_osc_in_its_place() {
        let want = std::env::var_os("PTYRO_TEST_PACKAGED_CONPTY").is_some();
        if packaged_conpty().is_none() {
            assert!(
                !want,
                "PTYRO_TEST_PACKAGED_CONPTY is set but no conpty.dll is beside the test"
            );
            eprintln!("skipped: no packaged ConPTY beside {:?}", std::env::current_exe());
            return;
        }
        let script =
            "[Console]::Out.Write('before-' + [char]27 + ']9;ptyro-osc' + [char]7 + '-after')";
        let mut c = start(&["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script]);
        let out = read_until(&mut c.reader, "-after").await;
        let (b, o, a) = (out.find("before-"), out.find("\x1b]9;ptyro-osc"), out.find("-after"));
        assert!(b < o && o < a, "out of order: {out:?}");
    }

    #[test]
    fn a_missing_program_is_named() {
        let argv = vec!["no-such-program-ptyro".to_owned()];
        let e = spawn(SpawnRequest {
            argv: &argv,
            cwd: &std::env::temp_dir(),
            env: child_env(std::env::vars_os(), []),
            size: TermSize::DEFAULT,
        })
        .err()
        .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("no-such-program-ptyro"), "{e}");
    }
}
