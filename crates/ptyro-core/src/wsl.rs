//! Running inside WSL (ROADMAP WIN2, SPEC §3.4): the one rule for "is this WSL, which version,
//! which distro, can it run Windows programs, and where are the Windows drives". About, the doctor
//! and the emulator's OSC 9;9 paths all ask here.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wsl {
    /// 1 or 2.
    pub version: u8,
    /// The distro's name as Windows knows it (`Ubuntu`); `None` if nothing said.
    pub distro: Option<String>,
    /// Windows programs (`cmd.exe`, `explorer.exe`) can be started from here.
    pub interop: bool,
    /// Where the Windows drives are mounted: `/mnt/` unless `/etc/wsl.conf` moves them.
    pub mount_root: String,
}

impl Wsl {
    /// `WSL2 (Ubuntu)`.
    pub fn describe(&self) -> String {
        match &self.distro {
            Some(d) => format!("WSL{} ({d})", self.version),
            None => format!("WSL{}", self.version),
        }
    }

    /// A path here as Windows programs name it: `/mnt/c/src` is `C:\src`, and a path inside the
    /// distro is `\\wsl.localhost\<distro>\home\me` (`None` when the distro is unknown).
    pub fn windows_path(&self, p: &Path) -> Option<String> {
        let s = p.to_str()?;
        if let Some(rest) = s.strip_prefix(&self.mount_root) {
            let mut c = rest.chars();
            if let Some(d) = c.next().filter(char::is_ascii_alphabetic) {
                let tail = c.as_str();
                if tail.is_empty() || tail.starts_with('/') {
                    let tail = tail.trim_start_matches('/').replace('/', "\\");
                    return Some(format!("{}:\\{tail}", d.to_ascii_uppercase()));
                }
            }
        }
        let distro = self.distro.as_deref()?;
        Some(format!(r"\\wsl.localhost\{distro}{}", s.replace('/', "\\")))
    }

    /// A Windows path (`C:\Users\me`) as a path here (`/mnt/c/Users/me`); `None` if it isn't one.
    pub fn linux_path(&self, w: &str) -> Option<PathBuf> {
        let mut c = w.trim().chars();
        let d = c.next().filter(char::is_ascii_alphabetic)?;
        let rest = c.as_str().strip_prefix(':')?;
        let rest = rest.trim_start_matches(['\\', '/']).replace('\\', "/");
        Some(PathBuf::from(format!("{}{}/{rest}", self.mount_root, d.to_ascii_lowercase())))
    }

    /// A Windows program (`explorer.exe`, `powershell.exe`): from `PATH`, else where Windows keeps
    /// it on C:. A shell rc that sets `PATH` outright drops the folders WSL appends, so `PATH`
    /// alone isn't enough.
    pub fn windows_exe(&self, name: &str) -> PathBuf {
        let on_path = std::env::var_os("PATH")
            .and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|p| p.is_file()));
        on_path.unwrap_or_else(|| {
            let c = Path::new(&self.mount_root).join("c/Windows");
            ["System32", "", "System32/WindowsPowerShell/v1.0"]
                .iter()
                .map(|d| c.join(d).join(name))
                .find(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from(name))
        })
    }

    /// The Windows user's home (`%USERPROFILE%`) as a path here, asked once; `None` without
    /// interop.
    pub fn windows_home(&self) -> Option<&'static Path> {
        static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            if !self.interop {
                return None;
            }
            let c = Path::new(&self.mount_root).join("c");
            let out = std::process::Command::new(self.windows_exe("cmd.exe"))
                .args(["/d", "/c", "echo %USERPROFILE%"])
                // A Linux folder makes cmd.exe complain about UNC paths first.
                .current_dir(if c.is_dir() { c } else { PathBuf::from("/") })
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            self.linux_path(&String::from_utf8_lossy(&out.stdout)).filter(|p| p.is_dir())
        })
        .as_deref()
    }
}

/// A Windows path as the WSL distro sees it, for a Windows client of the daemon in WSL (WIN3):
/// `C:\src\app` is `<mount_root>c/src/app`, `\\wsl.localhost\Ubuntu\home\me` (or
/// `\\wsl$\…`) is `/home/me`. The `\\?\` that canonical Windows paths start with is ignored.
/// `None` for a path WSL can't reach (a network share).
pub fn path_in_distro(windows: &str, mount_root: &str) -> Option<String> {
    let mut p = windows.trim();
    if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        return unc_in_distro(rest);
    }
    p = p.strip_prefix(r"\\?\").unwrap_or(p);
    if let Some(rest) = p.strip_prefix(r"\\") {
        return unc_in_distro(rest);
    }
    let w = Wsl { version: 2, distro: None, interop: true, mount_root: mount_root.to_owned() };
    w.linux_path(p).map(|l| l.to_string_lossy().into_owned())
}

/// `wsl.localhost\<distro>\rest` (or `wsl$\…`) as `/rest`.
fn unc_in_distro(unc: &str) -> Option<String> {
    let mut parts = unc.splitn(3, '\\');
    let host = parts.next()?;
    if !host.eq_ignore_ascii_case("wsl.localhost") && !host.eq_ignore_ascii_case("wsl$") {
        return None;
    }
    let _distro = parts.next()?;
    Some(format!("/{}", parts.next().unwrap_or("").replace('\\', "/")))
}

/// This machine, asked once; `None` when it isn't WSL.
pub fn detect() -> Option<&'static Wsl> {
    static WSL: std::sync::OnceLock<Option<Wsl>> = std::sync::OnceLock::new();
    WSL.get_or_init(|| {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let interop = ["WSLInterop", "WSLInterop-late"]
            .iter()
            .any(|f| std::path::Path::new("/proc/sys/fs/binfmt_misc").join(f).exists());
        let env = std::env::var("WSL_DISTRO_NAME").ok();
        let wsl = from_facts(&release, env.as_deref(), interop, "", "")?;
        // A systemd unit (how `ptyro service install` runs the daemon) doesn't get
        // WSL_DISTRO_NAME; `wslpath -w /` still names the distro.
        let unc = if wsl.distro.is_none() {
            std::process::Command::new("wslpath")
                .args(["-w", "/"])
                .stderr(std::process::Stdio::null())
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let conf = std::fs::read_to_string("/etc/wsl.conf").unwrap_or_default();
        from_facts(&release, env.as_deref(), interop, &unc, &conf)
    })
    .as_ref()
}

/// The rule, from what was read: the kernel release (`/proc/sys/kernel/osrelease`),
/// `WSL_DISTRO_NAME`, whether interop is registered, `wslpath -w /` and `/etc/wsl.conf`.
/// WSL1's kernel release ends in `-Microsoft` (`4.4.0-22621-Microsoft`); WSL2's says
/// `microsoft-standard` (`6.6.87.2-microsoft-standard-WSL2`).
pub fn from_facts(
    release: &str,
    distro_env: Option<&str>,
    interop: bool,
    wslpath_root: &str,
    wsl_conf: &str,
) -> Option<Wsl> {
    let distro_env = distro_env.filter(|d| !d.is_empty());
    let wsl = release.to_ascii_lowercase().contains("microsoft") || distro_env.is_some() || interop;
    if !wsl {
        return None;
    }
    let distro = distro_env.map(str::to_owned).or_else(|| {
        let root = wslpath_root.trim();
        [r"\\wsl.localhost\", r"\\wsl$\"]
            .iter()
            .find_map(|p| {
                root.get(..p.len()).filter(|h| h.eq_ignore_ascii_case(p)).map(|_| &root[p.len()..])
            })
            .map(|rest| rest.trim_end_matches('\\'))
            .filter(|d| !d.is_empty() && !d.contains('\\'))
            .map(str::to_owned)
    });
    Some(Wsl {
        version: if release.contains("-Microsoft") { 1 } else { 2 },
        distro,
        interop,
        mount_root: conf_root(wsl_conf),
    })
}

/// `[automount] root = …` from a `wsl.conf`, else `/mnt/`.
pub fn conf_root(conf: &str) -> String {
    let mut section = "";
    for line in conf.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(s) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = s.trim();
        } else if section.eq_ignore_ascii_case("automount")
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "root"
        {
            let v = v.trim().trim_matches('"');
            if v.starts_with('/') {
                return v.to_owned();
            }
        }
    }
    "/mnt/".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WSL2: &str = "6.6.87.2-microsoft-standard-WSL2\n";

    #[test]
    fn plain_linux_and_macos_are_not_wsl() {
        assert_eq!(from_facts("6.8.0-45-generic\n", None, false, "", ""), None);
        assert_eq!(from_facts("", None, false, "", ""), None, "macOS has no osrelease file");
        assert_eq!(
            from_facts("6.8.0", Some(""), false, "", ""),
            None,
            "an empty variable says nothing"
        );
    }

    #[test]
    fn the_kernel_release_tells_wsl1_from_wsl2() {
        let w = from_facts(WSL2, Some("Ubuntu"), true, "", "").unwrap();
        assert_eq!((w.version, w.distro.as_deref(), w.interop), (2, Some("Ubuntu"), true));
        assert_eq!(w.describe(), "WSL2 (Ubuntu)");
        assert_eq!(
            from_facts("5.15.153.1-microsoft-standard\n", None, false, "", "").unwrap().version,
            2
        );
        assert_eq!(
            from_facts("4.4.0-22621-Microsoft\n", Some("Debian"), true, "", "").unwrap().version,
            1
        );
        // A custom kernel: the distro variable or interop still say it's WSL.
        assert_eq!(
            from_facts("6.9.0-custom", Some("Arch"), false, "", "").unwrap().describe(),
            "WSL2 (Arch)"
        );
        assert!(from_facts("6.9.0-custom", None, true, "", "").is_some());
    }

    #[test]
    fn a_service_without_the_variable_learns_the_distro_from_wslpath() {
        let name = |unc: &str| from_facts(WSL2, None, true, unc, "").unwrap().distro;
        assert_eq!(name("\\\\wsl.localhost\\Ubuntu\\\n").as_deref(), Some("Ubuntu"));
        assert_eq!(name("\\\\wsl$\\Ubuntu-24.04\\").as_deref(), Some("Ubuntu-24.04"));
        assert_eq!(name(""), None);
        assert_eq!(name("C:\\"), None);
        assert_eq!(from_facts(WSL2, None, false, "", "").unwrap().describe(), "WSL2");
        // The variable wins.
        assert_eq!(
            from_facts(WSL2, Some("Dev"), true, "\\\\wsl.localhost\\Ubuntu\\", "")
                .unwrap()
                .distro
                .as_deref(),
            Some("Dev")
        );
    }

    #[test]
    fn paths_cross_to_windows_and_back() {
        let w = from_facts(WSL2, Some("Ubuntu"), true, "", "").unwrap();
        let win = |p: &str| w.windows_path(Path::new(p));
        assert_eq!(win("/mnt/c/Users/me/src").as_deref(), Some(r"C:\Users\me\src"));
        assert_eq!(win("/mnt/d").as_deref(), Some(r"D:\"));
        assert_eq!(win("/home/me/dev").as_deref(), Some(r"\\wsl.localhost\Ubuntu\home\me\dev"));
        assert_eq!(
            win("/mnt/cdrom").as_deref(),
            Some(r"\\wsl.localhost\Ubuntu\mnt\cdrom"),
            "not a drive"
        );
        let nameless = from_facts(WSL2, None, true, "", "").unwrap();
        assert_eq!(nameless.windows_path(Path::new("/home/me")), None);
        assert_eq!(nameless.windows_path(Path::new("/mnt/c/x")).as_deref(), Some(r"C:\x"));
        let moved = from_facts(WSL2, Some("U"), true, "", "[automount]\nroot = /win/\n").unwrap();
        assert_eq!(moved.windows_path(Path::new("/win/e/a/b")).as_deref(), Some(r"E:\a\b"));
        assert_eq!(moved.linux_path("E:\\a\\b\r\n"), Some(PathBuf::from("/win/e/a/b")));
        assert_eq!(w.linux_path(r"C:\Users\me"), Some(PathBuf::from("/mnt/c/Users/me")));
        assert_eq!(w.linux_path("%USERPROFILE%"), None, "cmd.exe echoes an unset variable");
        assert_eq!(w.linux_path(""), None);
    }

    #[test]
    fn windows_paths_become_the_distros() {
        let p = |w: &str| path_in_distro(w, "/mnt/");
        assert_eq!(p(r"C:\Users\me\src").as_deref(), Some("/mnt/c/Users/me/src"));
        assert_eq!(p(r"\\?\D:\work").as_deref(), Some("/mnt/d/work"), "canonical form");
        assert_eq!(p(r"\\wsl.localhost\Ubuntu\home\me\dev").as_deref(), Some("/home/me/dev"));
        assert_eq!(p(r"\\wsl$\Ubuntu-24.04\home\me").as_deref(), Some("/home/me"));
        assert_eq!(p(r"\\?\UNC\wsl.localhost\Ubuntu\srv").as_deref(), Some("/srv"));
        assert_eq!(p(r"\\wsl.localhost\Ubuntu").as_deref(), Some("/"));
        assert_eq!(p(r"\\fileserver\share\x"), None, "a network share");
        assert_eq!(path_in_distro(r"E:\a", "/win/").as_deref(), Some("/win/e/a"));
    }

    #[test]
    fn wsl_conf_moves_the_drive_mounts() {
        assert_eq!(conf_root(""), "/mnt/");
        assert_eq!(
            conf_root("[automount]\nenabled = true\nroot = /windir/ # drives\n"),
            "/windir/"
        );
        assert_eq!(
            conf_root("[boot]\nroot = /nope\n[automount]\noptions = \"metadata\"\n"),
            "/mnt/"
        );
        assert_eq!(
            from_facts(WSL2, None, true, "", "[automount]\nroot=/win/\n").unwrap().mount_root,
            "/win/"
        );
    }
}
