//! Process name and cwd probes (SPEC §5.1): `/proc` on Linux, libproc on macOS.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub fn process_name(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end().to_owned())
}

#[cfg(target_os = "linux")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // libproc has no safe wrapper; each call is bounded by the buffer we pass.
pub fn process_name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for `buf.len()` bytes and proc_name writes at most that many.
    let n = unsafe { libc::proc_name(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // see process_name
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    // SAFETY: `info` is a properly sized, zeroed proc_vnodepathinfo owned by this frame.
    let n = unsafe {
        libc::proc_pidinfo(pid as i32, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
    };
    if n != size {
        return None;
    }
    // SAFETY: the kernel NUL-terminates vip_path within its fixed-size buffer.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

/// The parent of `pid` (to tell which session a connecting process runs in).
#[cfg(target_os = "linux")]
pub fn parent_pid(pid: u32) -> Option<u32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state ppid …`: comm may hold spaces and parentheses, so cut at the last `)`.
    let rest = &s[s.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // see process_name
pub fn parent_pid(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is a properly sized, zeroed proc_bsdinfo owned by this frame.
    let n = unsafe {
        libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
    };
    (n == size).then_some(info.pbi_ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_name(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// Local wall-clock hour and minute (for the TUI's clock), honouring `TZ`.
#[allow(unsafe_code)] // localtime_r has no std equivalent; it writes only into `tm`.
pub fn local_time_hm() -> Option<(u8, u8)> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(now).ok()?;
    // SAFETY: `tm` is a zeroed out-parameter owned by this frame; localtime_r is reentrant.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    ok.then_some((tm.tm_hour as u8, tm.tm_min as u8))
}
