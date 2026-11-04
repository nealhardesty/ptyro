//! The Windows side: the host that opens pseudoconsoles and records what comes out, the child
//! that plays a case inside one and logs what reaches it, and WIN0.3's takeover pair.
//!
//! An `HPCON` is a pointer to three handles, `{ hSignal, hPtyReference, hConPtyProcess }`
//! (microsoft/terminal `src/winconpty/winconpty.h`): resizing writes a packet to the signal pipe,
//! closing closes all three and frees the struct from the process heap. The takeover test leans
//! on that: the new process gets the three handles and builds its own.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, HANDLE_FLAG_INHERIT, STILL_ACTIVE,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, COORD, ClosePseudoConsole, CreatePseudoConsole,
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
    HPCON, ResizePseudoConsole, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP,
    SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetExitCodeProcess, GetProcessId, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::cases::{self, Case};

/// `PSEUDOCONSOLE_PASSTHROUGH_MODE` (microsoft/terminal; the newer conpty.dll honours it).
const PASSTHROUGH: u32 = 0x8;

/// A handle that may cross threads (the spike owns its handles and never shares one at once).
#[derive(Clone, Copy)]
struct H(HANDLE);
unsafe impl Send for H {}

type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

/// Whose ConPTY: Windows' own (kernel32, the conhost in System32) or a conpty.dll beside an
/// OpenConsole.exe (Microsoft's NuGet package).
#[derive(Clone, Copy)]
struct Api {
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

fn inbox() -> Api {
    Api { create: CreatePseudoConsole, resize: ResizePseudoConsole, close: ClosePseudoConsole }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn conpty_dll(path: &Path) -> Result<Api, String> {
    unsafe {
        let h = LoadLibraryW(wide(&path.to_string_lossy()).as_ptr());
        if h.is_null() {
            return Err(format!(
                "LoadLibraryW({}) failed: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let get = |name: &[u8]| {
            GetProcAddress(h, name.as_ptr())
                .ok_or_else(|| String::from_utf8_lossy(name).into_owned())
        };
        let create = get(b"ConptyCreatePseudoConsole\0")?;
        let resize = get(b"ConptyResizePseudoConsole\0")?;
        let close = get(b"ConptyClosePseudoConsole\0")?;
        Ok(Api {
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateFn>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, ResizeFn>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, CloseFn>(close),
        })
    }
}

#[derive(Clone)]
struct Variant {
    name: String,
    api: Api,
    flags: u32,
    /// The conpty.dll, for processes that have to load it again (the takeover's new side).
    dll: Option<PathBuf>,
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD { X: cols as i16, Y: rows as i16 }
}

fn pipe() -> (HANDLE, HANDLE) {
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) };
    (r, w)
}

/// A pseudoconsole and our ends of its pipes.
struct Pty {
    hpc: HPCON,
    input: HANDLE,
    output: HANDLE,
}

fn open(v: &Variant, cols: u16, rows: u16) -> Result<Pty, String> {
    let (in_r, in_w) = pipe();
    let (out_r, out_w) = pipe();
    let mut hpc: HPCON = 0;
    let hr = unsafe { (v.api.create)(coord(cols, rows), in_r, out_w, v.flags, &mut hpc) };
    // conhost keeps its own copies.
    unsafe {
        CloseHandle(in_r);
        CloseHandle(out_w);
    }
    if hr != 0 {
        unsafe {
            CloseHandle(in_w);
            CloseHandle(out_r);
        }
        return Err(format!("CreatePseudoConsole failed: HRESULT {:#010x}", hr as u32));
    }
    Ok(Pty { hpc, input: in_w, output: out_r })
}

/// Start `cmdline`, attached to `hpc` when given, inheriting exactly `inherit` (an empty list:
/// nothing). Its standard handles are left empty on purpose: otherwise a child of a process whose
/// stdout is a pipe writes there instead of to its console.
fn spawn(
    cmdline: &str,
    hpc: Option<HPCON>,
    inherit: &[HANDLE],
    flags: u32,
) -> Result<PROCESS_INFORMATION, String> {
    unsafe {
        let n = u32::from(hpc.is_some()) + u32::from(!inherit.is_empty());
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), n.max(1), 0, &mut size);
        let mut buf = vec![0u8; size];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, n.max(1), 0, &mut size) == 0 {
            return Err(format!(
                "InitializeProcThreadAttributeList: {}",
                std::io::Error::last_os_error()
            ));
        }
        if let Some(hpc) = hpc
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) == 0
        {
            return Err(format!("attribute pseudoconsole: {}", std::io::Error::last_os_error()));
        }
        if !inherit.is_empty()
            && UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherit.as_ptr() as *const c_void,
                std::mem::size_of_val(inherit),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) == 0
        {
            return Err(format!("attribute handle list: {}", std::io::Error::last_os_error()));
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let mut cmd = wide(cmdline);
        let ok = CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(!inherit.is_empty()),
            EXTENDED_STARTUPINFO_PRESENT | flags,
            std::ptr::null(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(list);
        if ok == 0 {
            return Err(format!("CreateProcessW({cmdline}): {}", std::io::Error::last_os_error()));
        }
        Ok(pi)
    }
}

fn write_all(h: HANDLE, bytes: &[u8]) -> bool {
    let mut done = 0usize;
    while done < bytes.len() {
        let mut n = 0u32;
        let chunk = &bytes[done..(done + 4096).min(bytes.len())];
        if unsafe { WriteFile(h, chunk.as_ptr(), chunk.len() as u32, &mut n, std::ptr::null_mut()) }
            == 0
        {
            return false;
        }
        done += n as usize;
    }
    true
}

/// Everything read from `h` until it breaks, with when (ms since `t0`).
type Chunks = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

fn reader(h: HANDLE, t0: Instant) -> (Chunks, mpsc::Receiver<()>) {
    let chunks: Chunks = Arc::default();
    let (tx, rx) = mpsc::channel();
    let (c, h) = (chunks.clone(), H(h));
    std::thread::spawn(move || {
        let h = h;
        let mut buf = vec![0u8; 65536];
        loop {
            let mut n = 0u32;
            let ok = unsafe {
                ReadFile(h.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut())
            };
            if ok == 0 || n == 0 {
                break;
            }
            c.lock().unwrap().push((t0.elapsed().as_millis() as u64, buf[..n as usize].to_vec()));
        }
        let _ = tx.send(());
    });
    (chunks, rx)
}

fn exit_code(h: HANDLE) -> Option<u32> {
    let mut code = 0u32;
    (unsafe { GetExitCodeProcess(h, &mut code) } != 0 && code != STILL_ACTIVE as u32)
        .then_some(code)
}

fn wait(h: HANDLE, ms: u32) -> bool {
    unsafe { WaitForSingleObject(h, ms) == WAIT_OBJECT_0 }
}

fn image(h: HANDLE) -> String {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

fn exe() -> String {
    std::env::current_exe().unwrap().to_string_lossy().into_owned()
}

fn ms(t0: Instant) -> u64 {
    t0.elapsed().as_millis() as u64
}

// ── the host ──────────────────────────────────────────────────────────────────────────────

/// `run <out> [conpty.dll]`: every case under every variant, then the takeover tests.
pub fn run(out: &Path, dll: Option<&Path>, fixtures: &Path) -> Result<(), String> {
    let mut variants = vec![
        Variant { name: "inbox".into(), api: inbox(), flags: 0, dll: None },
        Variant { name: "inbox-passthrough".into(), api: inbox(), flags: PASSTHROUGH, dll: None },
    ];
    if let Some(d) = dll {
        let api = conpty_dll(d)?;
        variants.push(Variant { name: "nuget".into(), api, flags: 0, dll: Some(d.to_owned()) });
        variants.push(Variant {
            name: "nuget-passthrough".into(),
            api,
            flags: PASSTHROUGH,
            dll: Some(d.to_owned()),
        });
    }
    let cases = cases::all(&cases::fixtures(fixtures));
    let bins = out.join("cases");
    std::fs::create_dir_all(&bins).map_err(|e| e.to_string())?;
    for c in &cases {
        std::fs::write(bins.join(format!("{}.bin", c.name)), &c.output)
            .map_err(|e| e.to_string())?;
    }
    for v in &variants {
        let dir = out.join(&v.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut meta = serde_json::Map::new();
        for c in &cases {
            eprintln!("{} / {}", v.name, c.name);
            let m = run_case(v, c, &bins, &dir).unwrap_or_else(|e| json!({ "error": e }));
            meta.insert(c.name.clone(), m);
        }
        std::fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&Value::Object(meta)).unwrap(),
        )
        .map_err(|e| e.to_string())?;
    }
    // WIN0.3: once per ConPTY (the flags don't change who owns what).
    for v in variants.iter().filter(|v| v.flags == 0) {
        for mode in ["none", "inherit", "dup"] {
            eprintln!("takeover {} / {mode}", v.name);
            let dir = out.join("takeover").join(&v.name).join(mode);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let r = takeover(v, mode, &dir).unwrap_or_else(|e| json!({ "error": e }));
            std::fs::write(dir.join("result.json"), serde_json::to_string_pretty(&r).unwrap())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn run_case(v: &Variant, c: &Case, bins: &Path, dir: &Path) -> Result<Value, String> {
    let t0 = Instant::now();
    let pty = open(v, c.cols, c.rows)?;
    let (chunks, done) = reader(pty.output, t0);
    let log = dir.join(format!("{}.child.log", c.name));
    let cmd = format!(
        "\"{}\" {} \"{}\" \"{}\" {} {}{}",
        exe(),
        if c.records { "child-records" } else { "child" },
        bins.join(format!("{}.bin", c.name)).display(),
        log.display(),
        c.read_ms,
        u8::from(c.processed),
        if c.echo { "e" } else { "" }
    );
    let pi = spawn(&cmd, Some(pty.hpc), &[], 0)?;
    let mut timeline = vec![json!({ "at": ms(t0), "what": "spawned" })];
    let mut close_ms = None;
    if let Some(after) = c.close_after_ms {
        std::thread::sleep(Duration::from_millis(after));
        timeline.push(json!({ "at": ms(t0), "what": "close" }));
        let t = Instant::now();
        unsafe { (v.api.close)(pty.hpc) };
        close_ms = Some(t.elapsed().as_millis() as u64);
        timeline.push(json!({ "at": ms(t0), "what": "closed" }));
    } else {
        // Typing starts once the child has had time to start and write.
        std::thread::sleep(Duration::from_millis(300));
        for (i, (w, bytes)) in c.input.iter().enumerate() {
            std::thread::sleep(Duration::from_millis(*w));
            let ok = write_all(pty.input, bytes);
            timeline
                .push(json!({ "at": ms(t0), "epoch": epoch(), "what": "input", "i": i, "ok": ok }));
        }
        for (w, cols, rows) in &c.resizes {
            std::thread::sleep(Duration::from_millis(*w));
            let hr = unsafe { (v.api.resize)(pty.hpc, coord(*cols, *rows)) };
            timeline.push(
                json!({ "at": ms(t0), "what": "resize", "cols": cols, "rows": rows, "hr": hr }),
            );
        }
    }
    let exited = wait(pi.hProcess, c.read_ms + 6000);
    timeline.push(json!({ "at": ms(t0), "what": if exited { "child exited" } else { "child still running" } }));
    let code = exit_code(pi.hProcess);
    if !exited {
        unsafe { TerminateProcess(pi.hProcess, 99) };
    }
    if close_ms.is_none() {
        // Let conhost finish painting, then close it.
        std::thread::sleep(Duration::from_millis(300));
        let t = Instant::now();
        unsafe { (v.api.close)(pty.hpc) };
        close_ms = Some(t.elapsed().as_millis() as u64);
        timeline.push(json!({ "at": ms(t0), "what": "closed" }));
    }
    let drained = done.recv_timeout(Duration::from_secs(5)).is_ok();
    unsafe {
        CloseHandle(pty.input);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    let chunks = chunks.lock().unwrap().clone();
    let all: Vec<u8> = chunks.iter().flat_map(|(_, b)| b.clone()).collect();
    std::fs::write(dir.join(format!("{}.out", c.name)), &all).map_err(|e| e.to_string())?;
    let at: Vec<Value> = {
        let mut off = 0usize;
        chunks
            .iter()
            .map(|(t, b)| {
                off += b.len();
                json!([t, off])
            })
            .collect()
    };
    Ok(json!({
        "cols": c.cols, "rows": c.rows, "timeline": timeline, "exit_code": code,
        "close_ms": close_ms, "drained": drained, "chunks": at, "bytes": all.len(),
    }))
}

// ── the child ─────────────────────────────────────────────────────────────────────────────

static LOG: OnceLock<Mutex<(File, Instant)>> = OnceLock::new();

/// Milliseconds since the epoch: one clock for the host and the child.
fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn log(line: &str) {
    if let Some(l) = LOG.get() {
        let mut g = l.lock().unwrap();
        let _ = writeln!(g.0, "{} {line}", epoch());
        let _ = g.0.flush();
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
    log(&format!("ctrl {kind}"));
    // Ctrl+C and Ctrl+Break handled (so we live to log the rest); close and others: default.
    i32::from(kind <= 1)
}

/// `child <case.bin> <log> <read_ms> <processed>`: write the case, then log what comes in.
pub fn child(args: &[String]) -> ! {
    let [bin, logp, read_ms, processed] = args else { std::process::exit(2) };
    let _ = LOG.set(Mutex::new((File::create(logp).unwrap(), Instant::now())));
    let read_ms: u64 = read_ms.parse().unwrap_or(600);
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut m = 0u32;
        GetConsoleMode(hin, &mut m);
        log(&format!("input mode was {m:#x}"));
        let want = ENABLE_VIRTUAL_TERMINAL_INPUT
            | if processed.starts_with('1') { ENABLE_PROCESSED_INPUT } else { 0 };
        let echo = processed.ends_with('e');
        let ok = SetConsoleMode(hin, want);
        log(&format!("input mode set {want:#x} ok={ok}"));
        GetConsoleMode(hout, &mut m);
        log(&format!("output mode was {m:#x}"));
        SetConsoleMode(hout, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN);
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
        let bytes = std::fs::read(bin).unwrap_or_default();
        let ok = write_all(hout, &bytes);
        log(&format!("wrote {} ok={ok}", bytes.len()));
        let (hin, hout) = (H(hin), H(hout));
        std::thread::spawn(move || {
            let (hin, hout) = (hin, hout);
            let mut buf = [0u8; 512];
            loop {
                let mut n = 0u32;
                if ReadFile(hin.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut())
                    == 0
                {
                    log(&format!("read failed: {}", std::io::Error::last_os_error()));
                    break;
                }
                if echo {
                    write_all(hout.0, &buf[..n as usize]);
                }
                log(&format!("in {}", hex(&buf[..n as usize])));
            }
        });
    }
    std::thread::sleep(Duration::from_millis(read_ms));
    log("done");
    std::process::exit(7)
}

/// `child-records <case.bin> <log> <read_ms> <processed>`: write the case, then log the console
/// input records that come in, with VT input off (as PSReadLine reads them).
pub fn child_records(args: &[String]) -> ! {
    use windows_sys::Win32::System::Console::{INPUT_RECORD, KEY_EVENT, ReadConsoleInputW};
    let [bin, logp, read_ms, _] = args else { std::process::exit(2) };
    let _ = LOG.set(Mutex::new((File::create(logp).unwrap(), Instant::now())));
    let read_ms: u64 = read_ms.parse().unwrap_or(600);
    unsafe {
        SetConsoleOutputCP(65001);
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut m = 0u32;
        GetConsoleMode(hin, &mut m);
        log(&format!("input mode was {m:#x}"));
        // Raw, records: no line input, no echo, no processed input, no VT input.
        SetConsoleMode(hin, 0);
        write_all(hout, &std::fs::read(bin).unwrap_or_default());
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(read_ms) {
            if WaitForSingleObject(hin, 100) != WAIT_OBJECT_0 {
                continue;
            }
            let mut recs: [INPUT_RECORD; 16] = std::mem::zeroed();
            let mut n = 0u32;
            if ReadConsoleInputW(hin, recs.as_mut_ptr(), 16, &mut n) == 0 {
                break;
            }
            for r in &recs[..n as usize] {
                if u32::from(r.EventType) == KEY_EVENT {
                    let k = r.Event.KeyEvent;
                    log(&format!(
                        "key {} vk={} sc={} ch={} state={:#x}",
                        if k.bKeyDown != 0 { "down" } else { "up" },
                        k.wVirtualKeyCode,
                        k.wVirtualScanCode,
                        k.uChar.UnicodeChar,
                        k.dwControlKeyState
                    ));
                } else {
                    log(&format!("event {}", r.EventType));
                }
            }
        }
    }
    log("done");
    std::process::exit(7)
}

/// `child-echo <log>`: a heartbeat with its window size every 200 ms, and `got:` for each read,
/// for 30 s: what the takeover tests watch.
pub fn child_echo(args: &[String]) -> ! {
    let [logp] = args else { std::process::exit(2) };
    let _ = LOG.set(Mutex::new((File::create(logp).unwrap(), Instant::now())));
    unsafe {
        SetConsoleOutputCP(65001);
        let hin = GetStdHandle(STD_INPUT_HANDLE);
        let hout = GetStdHandle(STD_OUTPUT_HANDLE);
        SetConsoleMode(hin, ENABLE_VIRTUAL_TERMINAL_INPUT);
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
        let out = H(hout);
        let say = move |s: &str| {
            let out = &out;
            write_all(out.0, s.as_bytes());
            log(s.trim_end());
        };
        let (hin, say2) = (H(hin), say);
        std::thread::spawn(move || {
            let hin = hin;
            let mut buf = [0u8; 256];
            loop {
                let mut n = 0u32;
                if ReadFile(hin.0, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut())
                    == 0
                {
                    break;
                }
                let text = String::from_utf8_lossy(&buf[..n as usize]).replace(['\r', '\n'], "");
                say2(&format!("got:{text}\r\n"));
            }
        });
        for i in 0..150 {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            GetConsoleScreenBufferInfo(hout, &mut info);
            let w = &info.srWindow;
            say(&format!("beat {i} {}x{}\r\n", w.Right - w.Left + 1, w.Bottom - w.Top + 1));
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    std::process::exit(5)
}

// ── WIN0.3: taking a pseudoconsole over ───────────────────────────────────────────────────

/// Run `takeover-old` for `mode` and gather what both sides found.
fn takeover(v: &Variant, mode: &str, dir: &Path) -> Result<Value, String> {
    let cmd = format!(
        "\"{}\" takeover-old \"{}\" {} {} \"{}\"",
        exe(),
        dir.display(),
        mode,
        v.flags,
        v.dll.as_ref().map_or("-".into(), |d| d.display().to_string())
    );
    let pi = spawn(&cmd, None, &[], 0)?;
    wait(pi.hProcess, 15000);
    let old: Value = std::fs::read_to_string(dir.join("old.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({ "error": "the old side wrote nothing" }));
    let mut r = json!({ "old": old });
    if mode == "none" {
        // Nobody took it: is the child (and its conhost) still there 2 s later?
        std::thread::sleep(Duration::from_secs(2));
        let pids = [
            ("child", r["old"]["child_pid"].as_u64()),
            ("conhost", r["old"]["conhost_pid"].as_u64()),
        ];
        for (k, pid) in pids {
            let Some(pid) = pid else { continue };
            let h = unsafe {
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, 0, pid as u32)
            };
            let alive = !h.is_null() && exit_code(h).is_none();
            r[format!("{k}_alive_2s_later")] = json!(alive);
            if alive && k == "child" {
                unsafe { TerminateProcess(h, 98) };
            }
            if !h.is_null() {
                unsafe { CloseHandle(h) };
            }
        }
    } else {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) && !dir.join("new.json").exists() {
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_millis(200));
        r["new"] = std::fs::read_to_string(dir.join("new.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(json!({ "error": "the new side wrote nothing in 20 s" }));
    }
    r["child_log"] = json!(std::fs::read_to_string(dir.join("child.log")).unwrap_or_default());
    Ok(r)
}

/// `takeover-old <dir> <mode> <flags> <dll|->`: open a pseudoconsole with an echoing child, hand
/// it over (or not), and exit without closing anything.
pub fn takeover_old(args: &[String]) -> ! {
    let r = (|| -> Result<Value, String> {
        let [dir, mode, flags, dll] = args else { return Err("args".into()) };
        let dir = PathBuf::from(dir);
        let dll = (dll != "-").then(|| PathBuf::from(dll));
        let api = match &dll {
            Some(d) => conpty_dll(d)?,
            None => inbox(),
        };
        let v = Variant {
            name: String::new(),
            api,
            flags: flags.parse().unwrap_or(0),
            dll: dll.clone(),
        };
        let t0 = Instant::now();
        let pty = open(&v, 80, 24)?;
        let parts = unsafe { *(pty.hpc as *const [HANDLE; 3]) };
        let conhost_pid = unsafe { GetProcessId(parts[2]) };
        let cmd = format!("\"{}\" child-echo \"{}\"", exe(), dir.join("child.log").display());
        let pi = spawn(&cmd, Some(pty.hpc), &[], 0)?;
        let (chunks, _) = reader(pty.output, t0);
        std::thread::sleep(Duration::from_millis(1000));
        let seen: Vec<u8> = chunks.lock().unwrap().iter().flat_map(|(_, b)| b.clone()).collect();
        let mut r = json!({
            "conhost_pid": conhost_pid,
            "conhost_image": image(parts[2]),
            "child_pid": pi.dwProcessId,
            "beats_before": String::from_utf8_lossy(&seen).matches("beat ").count(),
            "beats_seen": String::from_utf8_lossy(&seen)
                .split("beat ")
                .skip(1)
                .filter_map(|t| t.split(' ').next()?.parse::<u64>().ok())
                .collect::<Vec<_>>(),
            "hpcon_parts": parts.iter().map(|h| format!("{:#x}", *h as usize)).collect::<Vec<_>>(),
        });
        let handles = [pty.input, pty.output, parts[0], parts[1], parts[2], pi.hProcess];
        let names = ["input", "output", "signal", "reference", "conhost", "child"];
        match mode.as_str() {
            "none" => {}
            "inherit" => {
                for h in handles {
                    unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                }
                let vals: Vec<String> =
                    handles.iter().map(|h| format!("{:x}", *h as usize)).collect();
                let cmd = format!(
                    "\"{}\" takeover-new \"{}\" {} \"{}\" {} {}",
                    exe(),
                    dir.display(),
                    std::process::id(),
                    dll.as_ref().map_or("-".into(), |d| d.display().to_string()),
                    v.flags,
                    vals.join(",")
                );
                let new = spawn(&cmd, None, &handles, DETACHED_PROCESS)?;
                r["new_pid"] = json!(new.dwProcessId);
            }
            "dup" => {
                // The new process isn't ours to start with handles: it is already running, and
                // we put copies into it (as one daemon could for another it found).
                let cmd = format!(
                    "\"{}\" takeover-new \"{}\" {} \"{}\" {} wait",
                    exe(),
                    dir.display(),
                    std::process::id(),
                    dll.as_ref().map_or("-".into(), |d| d.display().to_string()),
                    v.flags
                );
                let new = spawn(&cmd, None, &[], DETACHED_PROCESS)?;
                let mut vals = Vec::new();
                for h in handles {
                    let mut t: HANDLE = std::ptr::null_mut();
                    let ok = unsafe {
                        DuplicateHandle(
                            GetCurrentProcess(),
                            h,
                            new.hProcess,
                            &mut t,
                            0,
                            0,
                            DUPLICATE_SAME_ACCESS,
                        )
                    };
                    if ok == 0 {
                        return Err(format!(
                            "DuplicateHandle: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    vals.push(format!("{:x}", t as usize));
                }
                std::fs::write(dir.join("handles.txt"), vals.join(","))
                    .map_err(|e| e.to_string())?;
                r["new_pid"] = json!(new.dwProcessId);
            }
            m => return Err(format!("mode {m}")),
        }
        r["handed"] = json!(names);
        Ok(r)
    })();
    let dir = args.first().map(PathBuf::from).unwrap_or_default();
    let r = r.unwrap_or_else(|e| json!({ "error": e }));
    let _ = std::fs::write(dir.join("old.json"), serde_json::to_string_pretty(&r).unwrap());
    // Exit without ClosePseudoConsole, as a daemon replaced by a new one would.
    std::process::exit(0)
}

/// `takeover-new <dir> <old pid> <dll|-> <flags> <handles|wait>`: once the old process is gone,
/// drive the pseudoconsole it left: read, type, resize, close.
pub fn takeover_new(args: &[String]) -> ! {
    let dir = args.first().map(PathBuf::from).unwrap_or_default();
    let r = (|| -> Result<Value, String> {
        let [_, old_pid, dll, _flags, handles] = args else { return Err("args".into()) };
        let handles = if handles == "wait" {
            let t = Instant::now();
            loop {
                if let Ok(s) = std::fs::read_to_string(dir.join("handles.txt")) {
                    break s;
                }
                if t.elapsed() > Duration::from_secs(10) {
                    return Err("no handles.txt".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        } else {
            handles.clone()
        };
        let h: Vec<HANDLE> = handles
            .trim()
            .split(',')
            .map(|x| usize::from_str_radix(x, 16).unwrap_or(0) as HANDLE)
            .collect();
        let [input, output, signal, reference, conhost, child] = h[..] else {
            return Err("handles".into());
        };
        let api = if dll == "-" { inbox() } else { conpty_dll(Path::new(dll))? };
        // The old process must be gone first: that is the point.
        let old = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, old_pid.parse().unwrap_or(0)) };
        let old_gone = old.is_null() || wait(old, 5000);
        let t0 = Instant::now();
        let alive_after_old = exit_code(child).is_none();
        let (chunks, done) = reader(output, t0);
        std::thread::sleep(Duration::from_millis(1200));
        let text = |c: &Chunks| {
            String::from_utf8_lossy(
                &c.lock().unwrap().iter().flat_map(|(_, b)| b.clone()).collect::<Vec<u8>>(),
            )
            .into_owned()
        };
        let beats_after = text(&chunks).matches("beat ").count();
        let typed = write_all(input, b"ping\r");
        std::thread::sleep(Duration::from_millis(800));
        let echoed = text(&chunks).contains("got:ping");
        // Our own HPCON from the three handles, on the process heap (ClosePseudoConsole frees it).
        let hpc = unsafe {
            let p = HeapAlloc(GetProcessHeap(), 0, std::mem::size_of::<[HANDLE; 3]>())
                as *mut [HANDLE; 3];
            *p = [signal, reference, conhost];
            p as HPCON
        };
        let hr = unsafe { (api.resize)(hpc, coord(70, 20)) };
        std::thread::sleep(Duration::from_millis(800));
        let resized = text(&chunks).contains("70x20");
        let t = Instant::now();
        unsafe { (api.close)(hpc) };
        let close_ms = t.elapsed().as_millis() as u64;
        let child_exited = wait(child, 5000);
        let code = exit_code(child);
        let drained = done.recv_timeout(Duration::from_secs(3)).is_ok();
        unsafe { CloseHandle(input) };
        let all: Vec<u8> = chunks.lock().unwrap().iter().flat_map(|(_, b)| b.clone()).collect();
        let _ = std::fs::write(dir.join("new.out"), &all);
        let seen: Vec<u64> = String::from_utf8_lossy(&all)
            .split("beat ")
            .skip(1)
            .filter_map(|t| t.split(' ').next()?.parse().ok())
            .collect();
        Ok(json!({
            "old_gone": old_gone,
            "child_alive_after_old": alive_after_old,
            "beats_after_takeover": beats_after,
            "typed": typed,
            "echoed": echoed,
            "resize_hr": hr,
            "resized": resized,
            "close_ms": close_ms,
            "child_exited_after_close": child_exited,
            "child_exit_code": code,
            "output_ended": drained,
            "beats_seen": seen,
            "tail": text(&chunks).chars().rev().take(300).collect::<String>().chars().rev().collect::<String>(),
        }))
    })();
    let r = r.unwrap_or_else(|e| json!({ "error": e }));
    let _ = std::fs::write(dir.join("new.json"), serde_json::to_string_pretty(&r).unwrap());
    std::process::exit(0)
}
