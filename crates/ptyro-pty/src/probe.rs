//! Process name and cwd probes (SPEC §5.1): `/proc` on Linux, libproc on macOS.

use std::path::PathBuf;

/// The name to show for `pid`: the kernel's, unless that is a version number. Claude Code's
/// installer runs it as `~/.local/share/claude/versions/2.1.309` through a `claude` symlink, so
/// the kernel calls it `2.1.309` while argv[0] still says `claude` (SPEC §20.5); then argv[0]
/// wins.
pub fn process_name(pid: u32) -> Option<String> {
    let name = kernel_name(pid)?;
    Some(if looks_like_version(&name) { shown_name(name, argv0(pid)) } else { name })
}

/// The kernel's name, unless argv[0] names something better than a version.
fn shown_name(kernel: String, arg0: Option<String>) -> String {
    arg0.filter(|a| !a.is_empty() && !looks_like_version(a)).unwrap_or(kernel)
}

/// `2.1.309`, `v1.4`: digits and dots (a leading `v` allowed), at least one dot.
pub fn looks_like_version(s: &str) -> bool {
    let s = s.strip_prefix('v').unwrap_or(s);
    s.starts_with(|c: char| c.is_ascii_digit())
        && s.contains('.')
        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The basename of argv[0].
fn basename(arg0: &[u8]) -> String {
    let s = String::from_utf8_lossy(arg0);
    s.rsplit('/').next().unwrap_or(&s).trim_start_matches('-').to_owned()
}

#[cfg(target_os = "linux")]
fn kernel_name(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end().to_owned())
}

#[cfg(target_os = "linux")]
fn argv0(pid: u32) -> Option<String> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    cmdline.split(|b| *b == 0).next().map(basename)
}

#[cfg(target_os = "linux")]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // libproc has no safe wrapper; each call is bounded by the buffer we pass.
fn kernel_name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for `buf.len()` bytes and proc_name writes at most that many.
    let n = unsafe { libc::proc_name(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// argv[0] from `KERN_PROCARGS2`: an `int` argc, the executable's path, NUL padding, then the
/// arguments. Only our own user's processes are readable, which is all a PTY's are.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // sysctl has no safe wrapper; it writes at most `len` bytes into `buf`.
fn argv0(pid: u32) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = buf.len();
    // SAFETY: `mib` holds 3 ints, `buf` is valid for `len` bytes; no new value is set.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len <= 4 {
        return None;
    }
    let rest = &buf[4..len.min(buf.len())];
    let exe_end = rest.iter().position(|b| *b == 0)?;
    let args = &rest[exe_end..];
    let start = args.iter().position(|b| *b != 0)?;
    args[start..].split(|b| *b == 0).next().map(basename)
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
fn kernel_name(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn argv0(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// The POSIX session `pid` belongs to (its session leader's pid). Every ptyro session's child
/// leads its own, so a descendant that was reparented (double-fork, `nohup … &`, `disown`)
/// still names it; only `setsid` leaves.
pub fn session_leader(pid: u32) -> Option<u32> {
    let pid = rustix::process::Pid::from_raw(pid as i32)?;
    rustix::process::getsid(Some(pid)).ok().map(|p| p.as_raw_nonzero().get() as u32)
}

/// One variable from the environment `pid` was started with, as the kernel recorded it at exec
/// (not what the process claims now). Linux only; `None` elsewhere or when unreadable.
#[cfg(target_os = "linux")]
pub fn exec_env_var(pid: u32, name: &str) -> Option<String> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let prefix = format!("{name}=");
    env.split(|b| *b == 0)
        .find_map(|kv| kv.strip_prefix(prefix.as_bytes()))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

#[cfg(not(target_os = "linux"))]
pub fn exec_env_var(_pid: u32, _name: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    #[test]
    fn versions_are_recognised() {
        for v in ["2.1.309", "v1.4", "0.143.0"] {
            assert!(super::looks_like_version(v), "{v}");
        }
        for n in ["claude", "node", "zsh", "python3.12", "2fa", "123", "v", ""] {
            assert!(!super::looks_like_version(n), "{n}");
        }
        assert_eq!(super::basename(b"/usr/bin/claude"), "claude");
        assert_eq!(super::basename(b"-zsh"), "zsh", "a login shell's dash");
    }

    #[test]
    fn a_version_name_gives_way_to_argv0() {
        let shown = |k: &str, a: Option<&str>| super::shown_name(k.into(), a.map(Into::into));
        assert_eq!(shown("2.1.309", Some("claude")), "claude");
        assert_eq!(shown("2.1.309", Some("")), "2.1.309");
        assert_eq!(shown("2.1.309", Some("2.1.309")), "2.1.309");
        assert_eq!(shown("2.1.309", None), "2.1.309");
    }

    /// argv[0] parsing (`/proc/<pid>/cmdline`, macOS `KERN_PROCARGS2`) on this very process.
    #[test]
    fn argv0_reads_our_own() {
        let me = std::env::args().next().unwrap();
        let want = me.rsplit('/').next().unwrap().to_owned();
        assert_eq!(super::argv0(std::process::id()), Some(want));
    }
}
