//! The machine the engine works on: what's on it, and running a step. [`RealHost`] is this
//! computer; a debug build with `PTYRO_TEST_SETUP` set gets the scripted fake instead
//! ([`super::fake`]), so tests drive the whole flow without installing anything.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ptyro_core::paths::Paths;

use crate::catalog::{Arch, Item, Method, Platform, Version, Via};

/// An item found on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Installed {
    pub version: Option<String>,
    /// Where, for people (`~/.local/bin/claude`).
    pub path: Option<String>,
    /// How it was installed, when its folder says (npm's, Homebrew's, WinGet's).
    pub via: Option<Via>,
}

/// Whether administrator rights can be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// Without a prompt: root, sudo's remembered password or a passwordless rule, an elevated
    /// Windows terminal.
    Ready,
    /// With one: sudo asks for a password, or Windows' UAC asks (each installer, itself).
    Prompt,
    /// Not at all: no sudo.
    Unavailable,
}

/// One step: an item and the method that installs it.
#[derive(Debug, Clone, Copy)]
pub struct Step<'a> {
    pub item: &'a Item,
    pub method: &'a Method,
    /// The method's update command, not its install.
    pub update: bool,
}

impl Step<'_> {
    /// The shell command it runs (`None` for nodejs.org's build, which ptyro fetches itself).
    pub fn command(&self) -> Option<&str> {
        if self.update {
            self.method.update.as_deref().or(self.method.install.as_deref())
        } else {
            self.method.install.as_deref()
        }
    }
}

/// What the engine needs of a machine.
pub trait Host: Sync {
    fn platform(&self) -> Platform;
    /// `program` is on `PATH` (with ptyro's tools folders first).
    fn has(&self, program: &str) -> bool;
    /// The item's program, found, and the version it prints.
    fn installed(&self, item: &Item) -> Option<Installed>;
    /// The Node.js version apt or dnf would install.
    fn candidate(&self, via: Via) -> Option<String>;
    fn elevation(&self) -> Elevation;
    /// Ask for administrator rights once, in the terminal (`sudo -v`). True when they're had.
    fn elevate(&self) -> bool;
    /// Run a step, each line of its output to `line`. `Err`: what went wrong, for people.
    fn run(&self, step: &Step<'_>, line: &mut dyn FnMut(&str)) -> Result<(), String>;
    /// Something went into ptyro's tools folders, so `PATH` needs them (shell-init's line).
    fn tools_used(&self) -> bool;
    /// Run a login's status command (BOOT4) with its output thrown away: whether it exited 0.
    /// `None`: it didn't start, or took too long.
    fn status(&self, argv: &[String]) -> Option<bool>;
    /// Run a login in the person's terminal (stdin, stdout and stderr its own) to its end.
    /// `Ok(None)`: it exited 0; `Ok(Some(how))`: it ended otherwise; `Err`: it didn't start.
    fn interact(&self, argv: &[String]) -> Result<Option<String>, String>;
    /// A newest-version source's reply (BOOT5), within 20 s: `Err` when it can't be had
    /// (offline, a proxy, the vendor down), which a version check takes quietly.
    fn latest_reply(&self, url: &str) -> Result<String, String>;
}

/// The machine for this process: the fake a test names (`PTYRO_TEST_SETUP`, debug builds), else
/// this computer.
pub fn host(paths: &Paths) -> Box<dyn Host + Send> {
    if let Some(dir) = ptyro_core::test_knob("PTYRO_TEST_SETUP") {
        return Box::new(super::fake::FakeHost::new(PathBuf::from(dir), paths));
    }
    Box::new(RealHost::new(paths))
}

/// ptyro's tools folders' program directories, for `PATH`: nodejs.org's Node.js and the npm
/// prefix used when npm's own needs admin rights. `bin` under each, except on Windows.
pub fn path_dirs(paths: &Paths) -> Vec<PathBuf> {
    let t = paths.tools();
    if cfg!(windows) {
        vec![t.join("node"), t.join("npm")]
    } else {
        vec![t.join("node").join("bin"), t.join("npm").join("bin")]
    }
}

/// `base` (a `PATH`) with `dirs` in front, each once.
pub fn path_with(dirs: &[PathBuf], base: &OsStr) -> OsString {
    let rest = std::env::split_paths(base).filter(|p| !dirs.contains(p));
    std::env::join_paths(dirs.iter().cloned().chain(rest)).unwrap_or_else(|_| base.to_owned())
}

/// `current` (a `PATH`) after an installer changed the stored one: every entry it had, then the
/// `fresh` entries it lacks, with ptyro's tools folders in front. Windows' installers (WinGet's
/// Node.js) add their folder only to the registry's `Path`, so the next step (`npm install -g`)
/// wouldn't find it without this.
pub fn refreshed_path(current: &OsStr, fresh: &[PathBuf], tools: &[PathBuf]) -> OsString {
    let same = |a: &Path, b: &Path| {
        let n = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_ascii_lowercase();
        if cfg!(windows) { n(a) == n(b) } else { a == b }
    };
    let mut all: Vec<PathBuf> = std::env::split_paths(current).collect();
    for f in fresh.iter().filter(|f| !f.as_os_str().is_empty()) {
        if !all.iter().any(|a| same(a, f)) {
            all.push(f.clone());
        }
    }
    let joined = std::env::join_paths(all).unwrap_or_else(|_| current.to_owned());
    path_with(tools, &joined)
}

/// `%NAME%` in a stored `Path` replaced by `var(NAME)` (one that isn't set stays as written), as
/// Windows expands REG_EXPAND_SZ.
pub fn expand_vars(s: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) if j > 0 => match var(&after[..j]) {
                Some(v) => {
                    out.push_str(&v);
                    rest = &after[j + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A stored `Path` (`;`-separated, `%VAR%` as written) with `dirs` in front, each once (case
/// doesn't matter, nor a trailing `\`); nothing else is changed or expanded.
pub fn prepend_entries(raw: &str, dirs: &[String]) -> String {
    let key = |e: &str| e.trim().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let mine: Vec<String> = dirs.iter().map(|d| key(d)).collect();
    let rest = raw.split(';').filter(|e| !e.trim().is_empty() && !mine.contains(&key(e)));
    dirs.iter().map(String::as_str).chain(rest).collect::<Vec<_>>().join(";")
}

/// The machine's and the user's stored `Path` on Windows, expanded: what a new terminal gets.
#[cfg(windows)]
pub fn registry_path() -> Vec<PathBuf> {
    use ptyro_pty::winreg::get_raw;
    let machine =
        get_raw(true, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
    let user = get_raw(false, "Environment", "Path");
    let var = |k: &str| std::env::var(k).ok();
    [machine, user]
        .into_iter()
        .flatten()
        .flat_map(|r| {
            let text = if r.expand { expand_vars(&r.text, var) } else { r.text };
            text.split(';').filter(|e| !e.trim().is_empty()).map(PathBuf::from).collect::<Vec<_>>()
        })
        .collect()
}

/// This computer.
pub struct RealHost {
    platform: Platform,
    /// `PATH` with the tools folders first: what every step and probe runs with. Refreshed from
    /// the registry after each step on Windows.
    path: Mutex<OsString>,
    tools: PathBuf,
    /// The tools folders' program directories ([`path_dirs`]), kept in front of `path`.
    #[cfg_attr(not(windows), allow(dead_code))]
    dirs: Vec<PathBuf>,
    used: AtomicBool,
}

impl RealHost {
    pub fn new(paths: &Paths) -> Self {
        let base = std::env::var_os("PATH").unwrap_or_default();
        Self {
            platform: Platform::current(),
            path: Mutex::new(path_with(&path_dirs(paths), &base)),
            tools: paths.tools(),
            dirs: path_dirs(paths),
            used: AtomicBool::new(false),
        }
    }

    fn path(&self) -> OsString {
        self.path.lock().map(|p| p.clone()).unwrap_or_default()
    }

    fn env(&self) -> Vec<(OsString, OsString)> {
        vec![("PATH".into(), self.path())]
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let names = ptyro_core::os::program_names(program);
        std::env::split_paths(&self.path())
            .flat_map(|d| names.iter().map(move |n| d.join(n)))
            .find(|p| p.is_file())
    }

    fn root(&self) -> bool {
        ptyro_core::os::uid() == Some(0)
    }

    /// npm's global prefix when the user can write to it; else ptyro's own, which is then used.
    fn npm_env(&self) -> Vec<(OsString, OsString)> {
        if cfg!(windows) {
            // npm's prefix on Windows is `%APPDATA%\npm`, the user's own.
            return Vec::new();
        }
        let prefix = capture(&argv(&["npm", "prefix", "-g"]), &self.env(), Duration::from_secs(20))
            .filter(|(ok, _)| *ok)
            .map(|(_, out)| PathBuf::from(out.trim()));
        if prefix.is_some_and(|p| writable(&p.join("lib")) || writable(&p)) {
            return Vec::new();
        }
        let own = self.tools.join("npm");
        let _ = std::fs::create_dir_all(own.join("bin"));
        self.used.store(true, Ordering::Relaxed);
        vec![("NPM_CONFIG_PREFIX".into(), own.into_os_string())]
    }

    /// nodejs.org's build of the newest LTS, checked against its SHASUMS256.txt, into
    /// `<tools>/node` (the old one replaced only once the new one is unpacked).
    fn node_dist(&self, step: &Step<'_>, line: &mut dyn FnMut(&str)) -> Result<(), String> {
        let latest = step.item.latest.as_ref().ok_or("the table names no source for Node.js")?;
        let arch = Arch::host();
        line("finding the newest Node.js LTS (nodejs.org/dist/index.json)");
        let index = self.fetch(&latest.url(self.platform, arch))?;
        let version = latest.parse(&index).map_err(|e| e.to_string())?;
        let url = step
            .method
            .download(&version, self.platform, arch)
            .ok_or("the table has no download for Node.js")?;
        let sums =
            step.method.sums_url(&version).ok_or("the table has no checksums for Node.js")?;
        let file = url.rsplit('/').next().unwrap_or("node").to_owned();
        let work = self.tools.join("node.download");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let archive = work.join(&file);
        line(&format!("downloading Node.js {version}: {url}"));
        let got = self.download(&url, &archive, line)?;
        let list = self.fetch(&sums)?;
        let want = list
            .lines()
            .find_map(|l| {
                l.split_whitespace()
                    .collect::<Vec<_>>()
                    .get(..2)
                    .filter(|w| w[1] == file)
                    .map(|w| w[0].to_owned())
            })
            .ok_or_else(|| format!("{file} isn't in {sums}"))?;
        if !want.eq_ignore_ascii_case(&got) {
            let _ = std::fs::remove_dir_all(&work);
            return Err(format!(
                "{file}'s checksum is {got}, but SHASUMS256.txt says {want}: not used"
            ));
        }
        line("its checksum matches SHASUMS256.txt");
        let tar = ["tar", "-xf", &archive.to_string_lossy(), "-C", &work.to_string_lossy()];
        stream(&argv(&tar), &self.env(), line)?;
        let stem = file.trim_end_matches(".tar.xz").trim_end_matches(".zip");
        let unpacked = work.join(stem);
        let dest = self.tools.join("node");
        let old = self.tools.join("node.old");
        let _ = std::fs::remove_dir_all(&old);
        if dest.exists() {
            std::fs::rename(&dest, &old).map_err(|e| format!("{}: {e}", dest.display()))?;
        }
        std::fs::rename(&unpacked, &dest).map_err(|e| format!("{}: {e}", unpacked.display()))?;
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&work);
        self.used.store(true, Ordering::Relaxed);
        line(&format!("Node.js {version} is in {}", dest.display()));
        Ok(())
    }

    /// A small text over HTTPS (curl: on every platform the table covers, Windows 10 too).
    fn fetch(&self, url: &str) -> Result<String, String> {
        match capture(&argv(&["curl", "-fsSL", url]), &self.env(), Duration::from_secs(120)) {
            Some((true, body)) => Ok(body),
            Some((false, err)) => Err(format!("couldn't fetch {url}: {}", err.trim())),
            None => Err(format!("couldn't fetch {url}: curl didn't run or took too long")),
        }
    }

    /// `url` into `to`, and its SHA-256.
    fn download(&self, url: &str, to: &Path, line: &mut dyn FnMut(&str)) -> Result<String, String> {
        let to_s = to.to_string_lossy();
        stream(
            &argv(&["curl", "-fL", "--no-progress-meter", "-o", &to_s, url]),
            &self.env(),
            line,
        )?;
        sha256_file(to)
    }
}

impl Host for RealHost {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn has(&self, program: &str) -> bool {
        self.which(program).is_some()
    }

    fn installed(&self, item: &Item) -> Option<Installed> {
        let found = crate::locate::find(&item.program, &self.path().to_string_lossy())?;
        let mut cmd = found.argv.clone();
        cmd.extend(item.version.iter().skip(1).cloned());
        let text = capture(&cmd, &self.env(), Duration::from_secs(15)).map(|(_, t)| t);
        Some(Installed {
            version: text.as_deref().and_then(Version::parse).map(|v| v.to_string()),
            path: Some(found.describe()),
            via: installed_via(&found.path),
        })
    }

    fn candidate(&self, via: Via) -> Option<String> {
        let (cmd, field): (&[&str], &str) = match via {
            Via::Apt => (&["apt-cache", "policy", "nodejs"], "Candidate"),
            // `-C`: from the cache only, never the network.
            Via::Dnf => (&["dnf", "-q", "-C", "info", "nodejs"], "Version"),
            _ => return None,
        };
        let (_, out) = capture(&argv(cmd), &self.env(), Duration::from_secs(20))?;
        package_version(&out, field)
    }

    fn elevation(&self) -> Elevation {
        if cfg!(windows) {
            // `net session` answers only an elevated (administrator) process.
            return match capture(&argv(&["net", "session"]), &self.env(), Duration::from_secs(10)) {
                Some((true, _)) => Elevation::Ready,
                _ => Elevation::Prompt,
            };
        }
        if self.root() {
            return Elevation::Ready;
        }
        if !self.has("sudo") {
            return Elevation::Unavailable;
        }
        match capture(&argv(&["sudo", "-n", "true"]), &self.env(), Duration::from_secs(10)) {
            Some((true, _)) => Elevation::Ready,
            _ => Elevation::Prompt,
        }
    }

    fn elevate(&self) -> bool {
        if cfg!(windows) || self.root() {
            // Windows: each installer that needs it raises its own UAC prompt.
            return true;
        }
        // The real terminal: sudo asks for the password itself, never through ptyro.
        Command::new("sudo").arg("-v").env("PATH", self.path()).status().is_ok_and(|s| s.success())
    }

    fn run(&self, step: &Step<'_>, line: &mut dyn FnMut(&str)) -> Result<(), String> {
        if step.method.via == Via::NodeDist {
            return self.node_dist(step, line);
        }
        let cmd = step.command().ok_or("the table has no command for this")?;
        let shell = step.method.shell.ok_or("the table names no shell for this")?;
        // As root without sudo (a fresh container), the table's `sudo …` runs as it is.
        let cmd = if cfg!(unix) && self.root() && !self.has("sudo") && cmd.contains("sudo ") {
            format!("sudo() {{ \"$@\"; }}; {cmd}")
        } else {
            cmd.to_owned()
        };
        let mut env = self.env();
        if step.method.via == Via::Npm {
            env.extend(self.npm_env());
        }
        let r = stream(&shell.argv(&cmd), &env, line);
        #[cfg(windows)]
        if r.is_ok()
            && let Ok(mut p) = self.path.lock()
        {
            *p = refreshed_path(&p, &registry_path(), &self.dirs);
        }
        r
    }

    fn tools_used(&self) -> bool {
        self.used.load(Ordering::Relaxed)
    }

    fn status(&self, argv: &[String]) -> Option<bool> {
        let cmd = self.resolve(argv)?;
        exit_ok(&cmd, &self.env(), Duration::from_secs(20))
    }

    fn interact(&self, argv: &[String]) -> Result<Option<String>, String> {
        let cmd = self
            .resolve(argv)
            .ok_or_else(|| format!("{} isn't found", argv.first().map_or("it", String::as_str)))?;
        let (prog, args) = cmd.split_first().ok_or("nothing to run")?;
        // The person's own terminal: no window of its own on Windows either, since it shares
        // this console (CREATE_NO_WINDOW would take the console away from it).
        let status = Command::new(prog)
            .args(args)
            .env("PATH", self.path())
            .status()
            .map_err(|e| format!("couldn't start {prog}: {e}"))?;
        Ok(match status.code() {
            Some(0) => None,
            Some(c) => Some(format!("it exited with {c}")),
            None => Some("it was stopped".into()),
        })
    }

    fn latest_reply(&self, url: &str) -> Result<String, String> {
        self.reply(url)
    }
}

impl RealHost {
    /// [`Host::latest_reply`]: curl, at most 20 s.
    fn reply(&self, url: &str) -> Result<String, String> {
        let words = argv(&["curl", "-fsSL", "--max-time", "20", url]);
        match capture(&words, &self.env(), Duration::from_secs(25)) {
            Some((true, body)) => Ok(body),
            Some((false, err)) => Err(format!("couldn't reach {url}: {}", err.trim())),
            None => Err(format!("couldn't reach {url}: curl didn't run or took too long")),
        }
    }

    /// `argv` with its program found as the survey finds it (npm's, nvm's, the vendors' folders).
    fn resolve(&self, argv: &[String]) -> Option<Vec<String>> {
        let (prog, rest) = argv.split_first()?;
        let found = crate::locate::find(prog, &self.path().to_string_lossy())?;
        let mut cmd = found.argv;
        cmd.extend(rest.iter().cloned());
        Some(cmd)
    }
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// How a program was installed, by the folder it is in.
fn installed_via(path: &Path) -> Option<Via> {
    let p = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
    if p.contains("/node_modules/") || p.contains("/npm/") || p.contains("/_npx/") {
        Some(Via::Npm)
    } else if p.contains("/cellar/") || p.contains("/caskroom/") || p.contains("/homebrew/") {
        Some(Via::Brew)
    } else if p.contains("/winget/") {
        Some(Via::Winget)
    } else {
        None
    }
}

/// The version in a package manager's `field` line: apt's `Candidate: 18.19.1+dfsg-6ubuntu5`
/// (an epoch `1:` and the Debian revision dropped), dnf's `Version : 22.11.0`.
pub(super) fn package_version(out: &str, field: &str) -> Option<String> {
    let value = out.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == field).then(|| v.trim().to_owned())
    })?;
    let value = value.split_once(':').map_or(value.as_str(), |(_, v)| v);
    let upstream = value.split(['+', '~', '-']).next()?;
    Version::parse(upstream).map(|v| v.to_string())
}

/// The user can write in `dir`.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ptyro-write-test-{}", std::process::id()));
    let ok = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    if ok {
        let _ = std::fs::remove_file(&probe);
    }
    ok
}

fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// No console window of its own on Windows (SPEC §20.12).
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn spawn(argv: &[String], env: &[(OsString, OsString)]) -> Result<std::process::Child, String> {
    let (prog, args) = argv.split_first().ok_or("nothing to run")?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    quiet(&mut cmd).spawn().map_err(|e| format!("couldn't start {prog}: {e}"))
}

/// Each line of `pipe` to `tx`. A progress bar's carriage returns keep only the last state.
fn lines_to(pipe: impl Read + Send + 'static, tx: std::sync::mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(pipe);
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim_end_matches(['\n', '\r']);
            let last = text.rsplit('\r').next().unwrap_or("").to_owned();
            if tx.send(last).is_err() {
                return;
            }
            buf.clear();
        }
    });
}

/// How long the output of a program that has ended is still read: a background process it left
/// holding its stdout (a daemon an installer started) would otherwise keep the read open forever.
const GRACE: Duration = Duration::from_millis(1000);

/// Read `child`'s stdout and stderr line by line to `line` until it ends (and [`GRACE`] after,
/// for what it printed last), or `limit` passes (then it is killed: `None`).
fn pump(
    mut child: std::process::Child,
    line: &mut dyn FnMut(&str),
    limit: Option<Duration>,
) -> Option<std::process::ExitStatus> {
    use std::sync::mpsc::RecvTimeoutError;
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(o) = child.stdout.take() {
        lines_to(o, tx.clone());
    }
    if let Some(e) = child.stderr.take() {
        lines_to(e, tx.clone());
    }
    drop(tx);
    let start = Instant::now();
    let mut ended: Option<(std::process::ExitStatus, Instant)> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(l) => line(&l),
            // Both pipes closed: what's left is its exit.
            Err(RecvTimeoutError::Disconnected) => {
                return match ended {
                    Some((s, _)) => Some(s),
                    None => child.wait().ok(),
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if ended.is_none()
            && let Ok(Some(s)) = child.try_wait()
        {
            ended = Some((s, Instant::now()));
        }
        match ended {
            Some((s, at)) if at.elapsed() >= GRACE => return Some(s),
            None if limit.is_some_and(|l| start.elapsed() > l) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            _ => {}
        }
    }
}

/// Run `argv`, each line of stdout and stderr to `line` as it comes. `Err`: its last lines and
/// how it ended.
pub(super) fn stream(
    argv: &[String],
    env: &[(OsString, OsString)],
    line: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let child = spawn(argv, env)?;
    let mut tail: VecDeque<String> = VecDeque::new();
    let status = pump(
        child,
        &mut |l| {
            line(l);
            if !l.trim().is_empty() {
                tail.push_back(l.to_owned());
                if tail.len() > 3 {
                    tail.pop_front();
                }
            }
        },
        None,
    )
    .ok_or("it couldn't be waited for")?;
    if status.success() {
        return Ok(());
    }
    let how = match status.code() {
        Some(c) => format!("it exited with {c}"),
        None => "it was stopped by a signal".to_owned(),
    };
    let said: Vec<String> = tail.into_iter().collect();
    Err(if said.is_empty() { how } else { format!("{how}: {}", said.join(" / ")) })
}

/// Run `argv` with no input and its output thrown away (never read: a status command may print
/// an account or a masked token), at most `limit`: whether it exited 0. `None`: it didn't start,
/// or took too long (then it is killed).
fn exit_ok(argv: &[String], env: &[(OsString, OsString)], limit: Duration) -> Option<bool> {
    let (prog, args) = argv.split_first()?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = quiet(&mut cmd).spawn().ok()?;
    let start = Instant::now();
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s.success());
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run `argv` to its end (at most `limit`): whether it succeeded, and its output, a line each.
/// `None`: it didn't start, or took too long.
pub(super) fn capture(
    argv: &[String],
    env: &[(OsString, OsString)],
    limit: Duration,
) -> Option<(bool, String)> {
    let child = spawn(argv, env).ok()?;
    let mut text = String::new();
    let status = pump(
        child,
        &mut |l| {
            text.push_str(l);
            text.push('\n');
        },
        Some(limit),
    )?;
    Some((status.success(), text))
}
