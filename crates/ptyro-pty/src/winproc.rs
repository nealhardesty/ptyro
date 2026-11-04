//! Windows process facts (WIN7, R-WIN-12): what `/proc`, libproc and `lsof` say on Unix.
//!
//! - every process and its parent (Toolhelp32), a Job Object's own processes;
//! - a process's name (its image, or for an interpreter the script it runs), command line, start
//!   time and current directory (the last read from its PEB, as Windows has no call for it);
//! - which TCP ports processes listen on (`GetExtendedTcpTable`, IPv4 and IPv6);
//! - which processes have a file open (Restart Manager).
//!
//! Everything here answers "unknown" (`None`, empty) rather than fail: a process may exit
//! between two calls, or belong to someone ptyro may not look at.
#![allow(unsafe_code)] // Win32 calls; each block says what it relies on.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    JobObjectBasicProcessIdList, QueryInformationJobObject,
};
use windows_sys::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
    RmStartSession,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ, QueryFullProcessImageNameW,
};

/// A process handle opened for looking, closed on drop.
struct Opened(HANDLE);

impl Opened {
    fn new(pid: u32, rights: u32) -> Option<Self> {
        // SAFETY: opening by pid only asks; a null handle is the refusal.
        let h = unsafe { OpenProcess(rights, 0, pid) };
        (!h.is_null()).then_some(Self(h))
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: ours, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn wide_str(buf: &[u16]) -> String {
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// One process, as a snapshot sees it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub pid: u32,
    pub parent: u32,
    /// Its image's file name (`pwsh.exe`).
    pub exe: String,
}

/// Every process now.
pub fn snapshot() -> Vec<Entry> {
    let mut out = Vec::new();
    // SAFETY: a process snapshot, walked with a correctly sized entry and closed after.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        while more {
            out.push(Entry {
                pid: e.th32ProcessID,
                parent: e.th32ParentProcessID,
                exe: wide_str(&e.szExeFile),
            });
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

/// parent pid → child pids, from a snapshot.
pub fn children(snap: &[Entry]) -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in snap.iter().filter(|e| e.pid != 0 && e.pid != e.parent) {
        map.entry(e.parent).or_default().push(e.pid);
    }
    map
}

/// The processes in `job` now.
pub(crate) fn job_pids(job: HANDLE) -> Vec<u32> {
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_PROCESS_ID_LIST as List;
    let ids_at = std::mem::offset_of!(List, ProcessIdList);
    let mut room = 64usize;
    loop {
        // usize-aligned: the two counts, then room for `room` ids.
        let mut buf = vec![0usize; ids_at / std::mem::size_of::<usize>() + room];
        let size = (buf.len() * std::mem::size_of::<usize>()) as u32;
        // SAFETY: `buf` is `size` bytes, aligned for the struct this class fills; a full one
        // still gets its counts (ERROR_MORE_DATA).
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr().cast(),
                size,
                std::ptr::null_mut(),
            )
        } != 0;
        // SAFETY: the counts start the buffer; `listed` ids follow at `ids_at`, within it.
        let (assigned, listed) = unsafe {
            let p = buf.as_ptr().cast::<u32>();
            (*p as usize, (*p.add(1) as usize).min(room))
        };
        if assigned > room {
            room = assigned + 16;
            continue;
        }
        if !ok {
            return Vec::new();
        }
        let ids = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr().cast::<u8>().add(ids_at).cast::<usize>(),
                listed,
            )
        };
        return ids.iter().map(|p| *p as u32).collect();
    }
}

/// The full path of `pid`'s image.
pub fn image(pid: u32) -> Option<PathBuf> {
    let h = Opened::new(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer and its length are given; `len` comes back as what was written.
    let ok = unsafe { QueryFullProcessImageNameW(h.0, 0, buf.as_mut_ptr(), &mut len) } != 0;
    ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// When `pid` started (100 ns since 1601), to tell an older process from a newer one.
pub fn started(pid: u32) -> Option<u64> {
    let h = Opened::new(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exit, mut kernel, mut user) = (z, z, z, z);
    // SAFETY: four out-parameters, written on success.
    let ok = unsafe { GetProcessTimes(h.0, &mut created, &mut exit, &mut kernel, &mut user) } != 0;
    ok.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// `UNICODE_STRING` as a 64-bit process lays it out.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Unicode64 {
    length: u16,
    _max: u16,
    _pad: u32,
    buffer: u64,
}

/// `pid`'s command line, as it was started.
pub fn command_line(pid: u32) -> Option<String> {
    let h = Opened::new(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u64; 4096];
    let mut len = 0u32;
    // SAFETY: this class writes a UNICODE_STRING whose buffer points into `buf` itself (it is
    // copied into our process), at most `buf`'s size.
    unsafe {
        let st = NtQueryInformationProcess(
            h.0,
            ProcessCommandLineInformation,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut len,
        );
        if st < 0 {
            return None;
        }
        let s = *(buf.as_ptr() as *const Unicode64);
        let chars = std::slice::from_raw_parts(s.buffer as *const u16, usize::from(s.length) / 2);
        Some(String::from_utf16_lossy(chars))
    }
}

/// Read a `T` from `pid`'s memory at `addr`.
///
/// # Safety
/// `T` must be valid for any bit pattern.
unsafe fn read<T: Default + Copy>(h: HANDLE, addr: u64) -> Option<T> {
    let mut v = T::default();
    let mut got = 0usize;
    // SAFETY: we write at most size_of::<T>() bytes into `v`; the other process's memory is
    // only read.
    let ok = unsafe {
        ReadProcessMemory(
            h,
            addr as *const c_void,
            (&raw mut v).cast(),
            std::mem::size_of::<T>(),
            &mut got,
        )
    } != 0;
    (ok && got == std::mem::size_of::<T>()).then_some(v)
}

/// `pid`'s current directory, from its PEB: `PEB.ProcessParameters` (offset 0x20) and there
/// `CurrentDirectory.DosPath` (0x38), the layout of every 64-bit Windows since XP x64. `None`
/// for a 32-bit process or one ptyro may not read.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    let h = Opened::new(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    // SAFETY: the basic information struct, sized; then plain reads of the other process's
    // memory at the documented offsets, each checked.
    unsafe {
        let mut pbi: PROCESS_BASIC_INFORMATION = std::mem::zeroed();
        let st = NtQueryInformationProcess(
            h.0,
            ProcessBasicInformation,
            (&raw mut pbi).cast(),
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        if st < 0 || pbi.PebBaseAddress.is_null() {
            return None;
        }
        let params: u64 = read(h.0, pbi.PebBaseAddress as u64 + 0x20)?;
        let dir: Unicode64 = read(h.0, params + 0x38)?;
        let n = usize::from(dir.length) / 2;
        if n == 0 || n > 32_768 {
            return None;
        }
        let mut chars = vec![0u16; n];
        let mut got = 0usize;
        let ok = ReadProcessMemory(
            h.0,
            dir.buffer as *const c_void,
            chars.as_mut_ptr().cast(),
            n * 2,
            &mut got,
        ) != 0;
        if !ok || got != n * 2 {
            return None;
        }
        let s = String::from_utf16_lossy(&chars);
        // Its own form ends with `\` (`C:\src\`); a drive's root keeps it.
        let s = if s.len() > 3 { s.trim_end_matches('\\').to_owned() } else { s };
        Some(PathBuf::from(s))
    }
}

/// Interpreters whose name says nothing: the script they run names the program.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "pythonw", "py", "ruby", "perl"];

/// The name to show for `pid`: its image's stem, lowercase as Windows ignores case (`claude`
/// for `Claude.exe`), or for an
/// interpreter the script it runs (`codex` for `node …\codex.js`; for a package's `cli.js` or
/// `index.js`, the package's folder).
pub fn name(pid: u32) -> Option<String> {
    let image = image(pid)?;
    let stem = image.file_stem()?.to_string_lossy().to_lowercase();
    if !INTERPRETERS.iter().any(|i| stem.eq_ignore_ascii_case(i)) {
        return Some(stem);
    }
    let line = command_line(pid).unwrap_or_default();
    Some(crate::cmdline::script_name(&line).unwrap_or(stem))
}

/// pid → the TCP ports it listens on (IPv4 and IPv6), for these pids.
pub fn listening(pids: &[u32]) -> HashMap<u32, Vec<u16>> {
    let wanted: std::collections::HashSet<u32> = pids.iter().copied().collect();
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut add = |pid: u32, port: u32| {
        if wanted.contains(&pid) {
            // The port is in network byte order in the low 16 bits.
            out.entry(pid).or_default().push(u16::from_be((port & 0xffff) as u16));
        }
    };
    for af in [AF_INET, AF_INET6] {
        let Some(buf) = tcp_table(u32::from(af)) else { continue };
        // SAFETY: the table starts with its entry count, then that many rows of the kind `af`
        // asked for, all within `buf` (the size the call itself gave).
        unsafe {
            let n = *buf.as_ptr() as usize;
            let rows = buf.as_ptr().cast::<u8>().add(std::mem::size_of::<u32>());
            if af == AF_INET {
                let rows = std::slice::from_raw_parts(rows.cast::<MIB_TCPROW_OWNER_PID>(), n);
                rows.iter().for_each(|r| add(r.dwOwningPid, r.dwLocalPort));
            } else {
                let rows = std::slice::from_raw_parts(rows.cast::<MIB_TCP6ROW_OWNER_PID>(), n);
                rows.iter().for_each(|r| add(r.dwOwningPid, r.dwLocalPort));
            }
        }
    }
    out
}

/// The listening-socket table for one address family, as a u32-aligned buffer.
fn tcp_table(af: u32) -> Option<Vec<u32>> {
    let mut size = 0u32;
    for _ in 0..4 {
        let mut buf = vec![0u32; (size as usize).div_ceil(4).max(1)];
        // SAFETY: `buf` is `size` bytes; a too-small one is answered with the size it needs.
        let r = unsafe {
            GetExtendedTcpTable(
                buf.as_mut_ptr().cast(),
                &mut size,
                0,
                af,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match r {
            0 => return Some(buf),
            122 => continue, // ERROR_INSUFFICIENT_BUFFER: `size` says how much
            _ => return None,
        }
    }
    None
}

/// The programs that have `path` open (pid, name), as Restart Manager tells.
pub fn holders(path: &Path) -> Vec<(u32, String)> {
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    let mut session = 0u32;
    // SAFETY: a session that is ended below; the file name is NUL-ended and lives across the
    // registration; the process list is sized by the first call.
    unsafe {
        if RmStartSession(&mut session, 0, key.as_mut_ptr()) != 0 {
            return Vec::new();
        }
        let wide: Vec<u16> =
            path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let files = [wide.as_ptr()];
        let mut out = Vec::new();
        if RmRegisterResources(session, 1, files.as_ptr(), 0, std::ptr::null(), 0, std::ptr::null())
            == 0
        {
            let (mut needed, mut n, mut reasons) = (0u32, 0u32, 0u32);
            let r = RmGetList(session, &mut needed, &mut n, std::ptr::null_mut(), &mut reasons);
            if (r == 234 || r == 0) && needed > 0 {
                // ERROR_MORE_DATA: room for them now.
                let mut list: Vec<RM_PROCESS_INFO> = vec![std::mem::zeroed(); needed as usize];
                n = needed;
                if RmGetList(session, &mut needed, &mut n, list.as_mut_ptr(), &mut reasons) == 0 {
                    for p in &list[..n as usize] {
                        out.push((p.Process.dwProcessId, wide_str(&p.strAppName)));
                    }
                }
            }
        }
        RmEndSession(session);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_known() {
        let me = std::process::id();
        let snap = snapshot();
        let entry = snap.iter().find(|e| e.pid == me).expect("in the snapshot");
        assert!(entry.exe.to_lowercase().ends_with(".exe"), "{entry:?}");
        assert!(image(me).is_some());
        assert!(started(me).is_some());
        let line = command_line(me).unwrap();
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_lowercase();
        assert!(line.to_lowercase().contains(&stem), "{line} lacks {stem}");
        let here = std::env::current_dir().unwrap();
        let seen = cwd(me).unwrap();
        assert_eq!(seen.to_string_lossy().to_lowercase(), here.to_string_lossy().to_lowercase());
    }

    #[test]
    fn a_listener_of_ours_is_found() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id();
        assert!(listening(&[me]).get(&me).is_some_and(|p| p.contains(&port)));
        let l6 = std::net::TcpListener::bind("[::1]:0");
        if let Ok(l6) = l6 {
            let p6 = l6.local_addr().unwrap().port();
            assert!(listening(&[me]).get(&me).is_some_and(|p| p.contains(&p6)));
        }
    }

    #[test]
    fn a_file_we_hold_open_names_us() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("held.db");
        let _held = std::fs::File::create(&f).unwrap();
        let who = holders(&f);
        assert!(who.iter().any(|(p, _)| *p == std::process::id()), "{who:?}");
    }
}
