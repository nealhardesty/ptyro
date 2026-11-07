//! ptyro updates itself (ROADMAP BOOT6, SPEC §24.8): ptyro is one more row of the update
//! machinery ([`crate::updates`], BOOT5), under the id `ptyro`. Its newest version comes from
//! GitHub's releases; the finding is the installed binary's version against it; and the update,
//! run only after the person's yes and under the machine's setup lock, downloads the release for
//! this machine, checks it against the release's `SHA256SUMS` (and its signed build provenance
//! when `gh` is here), exactly as `scripts/install.sh` and `install.ps1` do, and puts it in place
//! of the old binary atomically, keeping the old one until the new one has answered `--version`
//! (else it is put back). The caller then live-upgrades the daemon (`daemon upgrade`).
//!
//! A ptyro a package manager owns (Homebrew, cargo, a system package) is never overwritten:
//! Homebrew's is updated with `brew upgrade ptyro`, the others are said, with how. A build in a
//! cargo `target` folder is never offered at all.
//!
//! Nothing here reaches the network unasked: the daily look fetches the release's metadata
//! only (as for the agents); the binary is fetched by [`update`], which only runs after a yes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::catalog::{Latest, Mark, Source, Version, Via};
use crate::setup::host::{capture, quiet, sha256_file, stream, writable};
use crate::setup::{Admin, Event};
use crate::updates::Outdated;

/// The row's id, name and program: `ptyro update ptyro`.
pub const ID: &str = "ptyro";
/// Where the releases are.
pub const REPO: &str = "nealhardesty/ptyro";
/// The newest release's metadata (`tag_name` = `v<version>`): what `install.sh` reads too.
pub const LATEST_URL: &str = "https://api.github.com/repos/nealhardesty/ptyro/releases/latest";
/// The releases' page, for people.
pub const RELEASES: &str = "https://github.com/nealhardesty/ptyro/releases";

/// The newest version's source: GitHub's latest release, `v` dropped from its tag.
pub fn source() -> Latest {
    Latest::new(Source::GithubRelease, LATEST_URL, Some("v"), Mark::Verified)
}

/// How the ptyro on this computer was installed, so how it updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// A release binary (`install.sh`, `install.ps1`, or copied by hand) in a folder the user
    /// can write: downloaded, checked and replaced here.
    Release,
    /// The same, in a folder only an administrator can write (`/usr/local/bin` owned by root):
    /// replaced with sudo, in a terminal; elsewhere the command is shown.
    Admin,
    /// Homebrew's: `brew upgrade ptyro`.
    Brew,
    /// Another manager's (cargo, a system package, WinGet, Scoop): updated its way, never here.
    Managed(String),
    /// A build in a cargo `target` folder: rebuilt, never offered an update.
    Build,
}

/// The ptyro binary on this computer, and how it updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub path: PathBuf,
    pub how: How,
}

impl Install {
    /// The binary on this computer (a debug build's `PTYRO_TEST_SELF_EXE` names another, so a
    /// test never replaces the build it runs).
    pub fn here() -> Self {
        let path = binary();
        let how = classify(&path);
        Self { path, how }
    }

    /// The binary that runs after an update: Homebrew's link in its `bin` (the keg's folder
    /// changes with each version), else the same file.
    pub fn after(&self) -> PathBuf {
        if self.how == How::Brew
            && let Some(prefix) = brew_prefix(&self.path)
        {
            let link = prefix.join("bin").join(exe_name());
            if link.exists() {
                return link;
            }
        }
        self.path.clone()
    }
}

/// `ptyro`, or `ptyro.exe` on Windows.
fn exe_name() -> &'static str {
    if cfg!(windows) { "ptyro.exe" } else { "ptyro" }
}

/// This ptyro's binary (on Linux even after it was replaced on disk), or a test's.
pub fn binary() -> PathBuf {
    if let Some(p) = ptyro_core::test_knob("PTYRO_TEST_SELF_EXE") {
        return PathBuf::from(p);
    }
    let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(exe_name()));
    let s = p.to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(orig) => PathBuf::from(orig),
        None => p,
    }
}

/// How the binary at `path` was installed, by where it is (as `ptyro uninstall` tells).
pub fn classify(path: &Path) -> How {
    let s = path.to_string_lossy().replace('\\', "/");
    let low = s.to_ascii_lowercase();
    // `target/debug`, `target/release`, or a cross build's `target/<triple>/debug`.
    let parts: Vec<&str> = low.split('/').collect();
    let profile = |p: Option<&&str>| matches!(p, Some(&"debug" | &"release"));
    if parts
        .iter()
        .enumerate()
        .any(|(i, p)| *p == "target" && (profile(parts.get(i + 1)) || profile(parts.get(i + 2))))
    {
        return How::Build;
    }
    if ["/cellar/", "/homebrew/", "/linuxbrew/"].iter().any(|b| low.contains(b)) {
        return How::Brew;
    }
    if low.contains("/.cargo/") {
        return How::Managed("cargo (`cargo install`)".into());
    }
    if low.contains("/winget/") {
        return How::Managed("WinGet (`winget upgrade`)".into());
    }
    if low.contains("/scoop/") {
        return How::Managed("Scoop (`scoop update ptyro`)".into());
    }
    if ["/usr/bin/", "/bin/", "/usr/sbin/", "/sbin/", "/snap/"].iter().any(|d| s.starts_with(d)) {
        return How::Managed("the system's package manager (apt, dnf, pacman, snap…)".into());
    }
    match path.parent() {
        Some(dir) if writable(dir) => How::Release,
        _ => How::Admin,
    }
}

/// Homebrew's prefix from a path in it: `/opt/homebrew/Cellar/ptyro/0.0.63/bin/ptyro` →
/// `/opt/homebrew`; `/home/linuxbrew/.linuxbrew/bin/ptyro` → `/home/linuxbrew/.linuxbrew`.
fn brew_prefix(path: &Path) -> Option<PathBuf> {
    let mut p = path.parent();
    while let Some(dir) = p {
        let name = dir.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name == "cellar" {
            return dir.parent().map(Path::to_path_buf);
        }
        if name == "bin" && !path.to_string_lossy().to_ascii_lowercase().contains("/cellar/") {
            return dir.parent().map(Path::to_path_buf);
        }
        p = dir.parent();
    }
    None
}

/// What the binary at `path` says its version is (`ptyro 0.0.63`): `None` when it doesn't run.
pub fn version_of(path: &Path) -> Option<String> {
    let argv = vec![path.to_string_lossy().into_owned(), "--version".to_owned()];
    let (ok, out) = capture(&argv, &[], Duration::from_secs(20))?;
    let line = out.lines().find(|l| l.trim().starts_with("ptyro "))?;
    let v = line.trim().trim_start_matches("ptyro ").split_whitespace().next()?.to_owned();
    (ok && Version::parse(&v).is_some()).then_some(v)
}

/// The finding for ptyro itself: the installed binary older than `latest` (the newest release),
/// with how it updates. `None`: up to date, a build, the binary doesn't answer, or no newest
/// version is known.
pub fn finding(install: &Install, latest: Option<&str>) -> Option<Outdated> {
    if install.how == How::Build {
        return None;
    }
    let latest = latest?;
    let version = version_of(&install.path)?;
    if Version::parse(&version)? >= Version::parse(latest)? {
        return None;
    }
    let (command, managed) = match &install.how {
        How::Brew => (Some("brew upgrade ptyro".to_owned()), None),
        How::Admin => (admin_command(&install.path, latest), None),
        How::Managed(m) => (None, Some(m.clone())),
        How::Release | How::Build => (None, None),
    };
    Some(Outdated {
        id: ID.into(),
        name: "ptyro".into(),
        program: ID.into(),
        version,
        latest: latest.to_owned(),
        command,
        via: (install.how == How::Brew).then_some(Via::Brew),
        admin: install.how == How::Admin,
        doc: RELEASES.into(),
        path: Some(install.path.to_string_lossy().into_owned()),
        managed,
    })
}

/// The command that installs `version` over an administrator's binary at `path`, to run on the
/// computer (the web shows it; `--yes` reports it): the release's own installer, as
/// administrator, into the same prefix. `None` when the binary isn't in a `bin` folder (the
/// installers put it in `<prefix>/bin`).
pub fn admin_command(path: &Path, version: &str) -> Option<String> {
    let bin = path.parent()?;
    if !bin.file_name()?.to_string_lossy().eq_ignore_ascii_case("bin") {
        return None;
    }
    let prefix = bin.parent()?.to_string_lossy().into_owned();
    let base = format!("https://github.com/{REPO}/releases/download/v{version}");
    Some(if cfg!(windows) {
        format!(
            "$env:PTYRO_PREFIX = '{prefix}'; $env:PTYRO_VERSION = '{version}'; irm {base}/install.ps1 | iex   (in PowerShell run as administrator)"
        )
    } else {
        format!(
            "curl -fsSL {base}/install.sh | sudo env PTYRO_PREFIX={prefix} PTYRO_VERSION={version} sh"
        )
    })
}

/// The release's name for this machine: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
/// `x86_64-pc-windows-msvc`… (the four tarballs and two zips `make dist` builds).
pub fn triple() -> &'static str {
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(windows) {
        if arm { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
    } else if cfg!(target_os = "macos") {
        if arm { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" }
    } else if arm {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// The release's archive for this machine: `ptyro-0.0.64-x86_64-unknown-linux-gnu.tar.gz`
/// (Windows: `.zip`).
pub fn archive(version: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("ptyro-{version}-{}.{ext}", triple())
}

/// Where `version`'s files are: GitHub's release, or a debug build's `PTYRO_TEST_RELEASE_URL`
/// (a test's fake release server, as `install.sh`'s `PTYRO_RELEASE_URL`).
pub fn release_base(version: &str) -> String {
    match ptyro_core::test_knob("PTYRO_TEST_RELEASE_URL") {
        Some(u) => u.trim_end_matches('/').to_owned(),
        None => format!("https://github.com/{REPO}/releases/download/v{version}"),
    }
}

/// What the update does, in a line, for the question and the progress: "downloads ptyro
/// 0.0.64, checks it against SHA256SUMS, and replaces ~/.local/bin/ptyro".
pub fn describe(install: &Install, version: &str) -> String {
    match &install.how {
        How::Brew => "brew upgrade ptyro".into(),
        How::Admin => format!(
            "download ptyro {version} ({}), check it against SHA256SUMS, and replace {} as administrator (sudo)",
            archive(version),
            install.path.display()
        ),
        _ => format!(
            "download ptyro {version} ({}), check it against SHA256SUMS, and replace {}",
            archive(version),
            install.path.display()
        ),
    }
}

/// Why an update didn't happen, and the fix; nothing was replaced (or the old binary is back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error: String,
    pub fix: String,
}

fn fail(error: impl Into<String>, fix: impl Into<String>) -> Failure {
    Failure { error: error.into(), fix: fix.into() }
}

/// Install ptyro `version` over `install`, each step to `on` (as setup's events, under the id
/// `ptyro`): the release downloaded, checked against `SHA256SUMS` (and its provenance with
/// `gh`), and swapped in, the old binary kept until the new one answers `--version`; Homebrew's
/// through `brew upgrade ptyro`. Ends with `Done` or `Failed`. The new version, or why not.
/// The caller holds the machine's setup lock, and had the person's yes.
pub fn update(
    install: &Install,
    version: &str,
    admin: Admin,
    on: &mut dyn FnMut(Event),
) -> Result<String, Failure> {
    let admin_step = install.how == How::Admin;
    on(Event::Started {
        id: ID.into(),
        name: "ptyro".into(),
        command: describe(install, version),
        admin: admin_step,
    });
    let r = match &install.how {
        How::Build => Err(fail(
            format!("{} is a build in a cargo target folder", install.path.display()),
            "build it again (`make build`); `ptyro update` updates installed copies only",
        )),
        How::Managed(m) => Err(fail(
            format!("this ptyro ({}) was installed with {m}", install.path.display()),
            format!("update it with {m}; ptyro never replaces a binary a package manager owns"),
        )),
        How::Brew => brew(install, on),
        How::Admin if admin != Admin::Ask || cfg!(windows) => Err(fail(
            format!("{} can only be replaced as administrator", install.path.display()),
            match admin_command(&install.path, version) {
                Some(c) => format!(
                    "run `ptyro update ptyro` in a terminal on this computer (it asks for your password), or `{c}`"
                ),
                None => "run `ptyro update ptyro` in a terminal on this computer (it asks for your password)".into(),
            },
        )),
        How::Release | How::Admin => release(install, version, admin_step, on),
    };
    match &r {
        Ok(v) => on(Event::Done { id: ID.into(), version: Some(v.clone()) }),
        Err(f) => on(Event::Failed {
            id: ID.into(),
            error: f.error.clone(),
            fix: f.fix.clone(),
            in_use: false,
        }),
    }
    r
}

/// `brew upgrade ptyro`, live, then the version Homebrew's link answers.
fn brew(install: &Install, on: &mut dyn FnMut(Event)) -> Result<String, Failure> {
    let argv = vec!["brew".to_owned(), "upgrade".to_owned(), "ptyro".to_owned()];
    stream(&argv, &[], &mut |l| on(Event::Line { id: ID.into(), text: l.to_owned() })).map_err(
        |e| {
            fail(format!("`brew upgrade ptyro` failed: {e}"), "run `brew upgrade ptyro` to see why")
        },
    )?;
    version_of(&install.after()).ok_or_else(|| {
        fail(
            "Homebrew upgraded it, but the new ptyro doesn't answer `--version`",
            "run `brew reinstall ptyro`",
        )
    })
}

/// A folder of its own for the download (untrusted until checked), gone when dropped.
struct Work(PathBuf);

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The release: download, check, unpack, swap in, check it runs.
fn release(
    install: &Install,
    version: &str,
    admin: bool,
    on: &mut dyn FnMut(Event),
) -> Result<String, Failure> {
    let mut say = |t: &str| on(Event::Line { id: ID.into(), text: t.to_owned() });
    let again = "nothing was replaced; `ptyro update ptyro` tries again";
    let base = release_base(version);
    let file = archive(version);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let work =
        Work(std::env::temp_dir().join(format!("ptyro-update-{}-{nanos}", std::process::id())));
    std::fs::create_dir_all(&work.0)
        .map_err(|e| fail(format!("{}: {e}", work.0.display()), again))?;
    let download = work.0.join(&file);
    let sums = work.0.join("SHA256SUMS");
    say(&format!("downloading {base}/{file}"));
    let curl = |url: &str, to: &Path| -> Vec<String> {
        [
            "curl",
            "-fL",
            "--no-progress-meter",
            "--connect-timeout",
            "10",
            "-o",
            &to.to_string_lossy(),
            url,
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
    };
    stream(&curl(&format!("{base}/{file}"), &download), &[], &mut say).map_err(|e| {
        fail(
            format!("couldn't download {base}/{file}: {e}"),
            format!("{again} (offline? is {version} released for {}?)", triple()),
        )
    })?;
    stream(&curl(&format!("{base}/SHA256SUMS"), &sums), &[], &mut say)
        .map_err(|e| fail(format!("couldn't download {base}/SHA256SUMS: {e}"), again))?;
    let list = std::fs::read_to_string(&sums).unwrap_or_default();
    let want = checksum_for(&list, &file).ok_or_else(|| {
        fail(format!("the release's SHA256SUMS has no entry for {file}: not installing"), again)
    })?;
    let got = sha256_file(&download).map_err(|e| fail(e, again))?;
    if !want.eq_ignore_ascii_case(&got) {
        return Err(fail(
            format!(
                "the download's checksum is {got}, but the release's SHA256SUMS says {want}: it was changed or broken on the way, so it isn't installed"
            ),
            format!(
                "{again} (if it keeps failing, don't install it by hand either: tell the maintainers, https://github.com/{REPO}/issues)"
            ),
        ));
    }
    say("checksum ok (SHA256SUMS)");
    provenance(&download, &mut say)?;
    let unpack = work.0.join("unpacked");
    std::fs::create_dir_all(&unpack)
        .map_err(|e| fail(format!("{}: {e}", unpack.display()), again))?;
    let tar = vec![
        tar_program(),
        "-xf".to_owned(),
        download.to_string_lossy().into_owned(),
        "-C".to_owned(),
        unpack.to_string_lossy().into_owned(),
    ];
    stream(&tar, &[], &mut say).map_err(|e| fail(format!("couldn't unpack {file}: {e}"), again))?;
    let stem = file.trim_end_matches(".tar.gz").trim_end_matches(".zip");
    let from = unpack.join(stem);
    let new = from.join(exe_name());
    if !new.is_file() {
        return Err(fail(format!("{file} has no {} in it: not installing", exe_name()), again));
    }
    let dir = install
        .path
        .parent()
        .ok_or_else(|| fail(format!("{} has no folder", install.path.display()), again))?;
    let target_name = install
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| exe_name().to_owned());
    // The binary under the installed one's name; on Windows also what the zip carries beside it
    // (`ptyrow.exe`, the packaged ConPTY), as install.ps1 puts them.
    let mut files: Vec<(PathBuf, String)> = vec![(new, target_name.clone())];
    if cfg!(windows) {
        for f in ["ptyrow.exe", "conpty.dll", "OpenConsole.exe"] {
            if from.join(f).is_file() {
                files.push((from.join(f), f.to_owned()));
            }
        }
    }
    let check = |p: &Path| -> Result<(), String> {
        match version_of(p) {
            Some(v) if v == version => Ok(()),
            Some(v) => Err(format!("it says it is {v}, not {version}")),
            None => Err("it doesn't answer `ptyro --version` (Windows' Smart App Control, or a broken download?)".into()),
        }
    };
    say(&format!("replacing {}", install.path.display()));
    let swapped = if admin {
        sudo_swap(&files[0].0, &install.path, &check)
    } else {
        swap(dir, &files, &target_name, &check)
    };
    swapped.map_err(|e| {
        fail(
            format!("the new ptyro {version} didn't run ({e}), so the old one is back in place"),
            format!(
                "the old ptyro still works; `ptyro update ptyro` tries again, or see {RELEASES}"
            ),
        )
    })?;
    completions(&from, dir);
    say(&format!("ptyro {version} is in {}", install.path.display()));
    Ok(version.to_owned())
}

/// The checksum `SHA256SUMS` gives `file` (`<hex>  <file>`, or `*<file>` in binary mode).
pub fn checksum_for(list: &str, file: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        let (sum, name) = (w.next()?, w.next()?);
        (name == file || name.strip_prefix('*') == Some(file)).then(|| sum.to_owned())
    })
}

/// The signed build provenance, when `gh` is here (as the installers: `PTYRO_VERIFY=0` skips it;
/// `PTYRO_REQUIRE_ATTESTATION=1` refuses without it). A provenance that can't be checked (not
/// logged in, an old release) is a note: the checksum matched.
fn provenance(download: &Path, say: &mut dyn FnMut(&str)) -> Result<(), Failure> {
    let require = std::env::var("PTYRO_REQUIRE_ATTESTATION").is_ok_and(|v| v == "1");
    let skip = std::env::var("PTYRO_VERIFY").is_ok_and(|v| v == "0");
    let gh = (!skip).then(|| which("gh")).flatten();
    let Some(gh) = gh else {
        if require {
            return Err(fail(
                "PTYRO_REQUIRE_ATTESTATION=1 needs the GitHub CLI (gh) to check the provenance",
                "install gh (`ptyro install gh`), or unset PTYRO_REQUIRE_ATTESTATION; nothing was replaced",
            ));
        }
        return Ok(());
    };
    let argv = vec![
        gh.to_string_lossy().into_owned(),
        "attestation".into(),
        "verify".into(),
        download.to_string_lossy().into_owned(),
        "--repo".into(),
        REPO.into(),
    ];
    match capture(&argv, &[], Duration::from_secs(90)) {
        Some((true, _)) => {
            say(&format!("provenance ok (built by {REPO}'s release workflow)"));
            Ok(())
        }
        _ if require => Err(fail(
            "the provenance didn't verify (`gh attestation verify`), and PTYRO_REQUIRE_ATTESTATION=1 asks for it",
            "nothing was replaced; `gh auth login`, then `ptyro update ptyro` again",
        )),
        _ => {
            say(
                "note: couldn't verify the provenance with gh (not logged in, or an old release); the checksum matched",
            );
            Ok(())
        }
    }
}

/// `program` on `PATH`.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{program}.exe"), format!("{program}.cmd")]
    } else {
        vec![program.to_owned()]
    };
    std::env::split_paths(&path).find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
}

/// Windows' own `tar` (bsdtar, in System32 since Windows 10 1803), which unpacks a zip; Git's
/// GNU tar earlier on `PATH` can't. Elsewhere, `tar`.
fn tar_program() -> String {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let t = PathBuf::from(root).join("System32").join("tar.exe");
        if t.is_file() {
            return t.to_string_lossy().into_owned();
        }
    }
    "tar".into()
}

/// Put `files` (each a new file and the name it goes under) into `dir` atomically, then
/// `check` the binary `main`; on failure every file is put back as it was. The old files are
/// kept until the check passes: on Windows renamed aside (a running program can't be overwritten,
/// but it can be renamed; the leftover `.old` is removed by [`sweep`] on a later start), elsewhere
/// hard-linked aside (so the name is never missing).
pub fn swap(
    dir: &Path,
    files: &[(PathBuf, String)],
    main: &str,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let pid = std::process::id();
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    // 1. Each new file beside its place (the same folder, so the move is atomic).
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (src, name) in files {
        let tmp = dir.join(format!(".{name}.new.{pid}"));
        if let Err(e) = std::fs::copy(src, &tmp) {
            for (t, _) in &staged {
                let _ = std::fs::remove_file(t);
            }
            return Err(io(&tmp, e));
        }
        executable(&tmp);
        staged.push((tmp, dir.join(name)));
    }
    // 2. The old ones aside, 3. the new ones in.
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let undo = |done: &[(PathBuf, Option<PathBuf>)]| {
        for (target, old) in done.iter().rev() {
            match old {
                Some(old) => {
                    let _ = std::fs::rename(old, target);
                }
                None => {
                    let _ = std::fs::remove_file(target);
                }
            }
        }
    };
    for (tmp, target) in &staged {
        let old = if target.exists() {
            let old = aside(target, pid);
            if let Err(e) = keep_old(target, &old) {
                undo(&done);
                for (t, _) in &staged {
                    let _ = std::fs::remove_file(t);
                }
                return Err(io(target, e));
            }
            Some(old)
        } else {
            None
        };
        if let Err(e) = std::fs::rename(tmp, target) {
            if let Some(old) = &old {
                // Windows moved it aside: back; elsewhere it was only linked: the link goes.
                let _ = if cfg!(windows) {
                    std::fs::rename(old, target)
                } else {
                    std::fs::remove_file(old)
                };
            }
            undo(&done);
            for (t, _) in &staged {
                let _ = std::fs::remove_file(t);
            }
            return Err(io(target, e));
        }
        done.push((target.clone(), old));
    }
    // 4. The new one must run.
    if let Err(e) = check(&dir.join(main)) {
        undo(&done);
        return Err(e);
    }
    for (_, old) in &done {
        if let Some(old) = old {
            // Windows: still running (the daemon, a TUI) can't be removed: a later start does.
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

/// Where the old `target` is kept meanwhile: `ptyro.exe.old` on Windows (install.ps1's name; a
/// leftover one still running is renamed further aside), `.ptyro.old.<pid>` elsewhere.
fn aside(target: &Path, pid: u32) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if cfg!(windows) {
        let old = target.with_file_name(format!("{name}.old"));
        if old.exists() && std::fs::remove_file(&old).is_err() {
            let _ = std::fs::rename(&old, target.with_file_name(format!("{name}.old.{pid}")));
        }
        old
    } else {
        target.with_file_name(format!(".{name}.old.{pid}"))
    }
}

/// Keep the old file at `old`: renamed there on Windows (a running program can be renamed, not
/// overwritten), hard-linked there elsewhere (copied where links aren't possible).
fn keep_old(target: &Path, old: &Path) -> std::io::Result<()> {
    if cfg!(windows) {
        std::fs::rename(target, old)
    } else {
        let _ = std::fs::remove_file(old);
        std::fs::hard_link(target, old).or_else(|_| std::fs::copy(target, old).map(|_| ()))
    }
}

#[cfg(unix)]
fn executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn executable(_: &Path) {}

/// Leftovers of an update the files were still running for (Windows: `ptyro.exe.old`,
/// `ptyro.exe.old.<pid>`, and the same for what the zip carries), removed once nothing runs them:
/// called as ptyro starts. Quietly does nothing for one still in use.
pub fn sweep(binary: &Path) {
    let Some(dir) = binary.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let names = ["ptyro.exe", "ptyrow.exe", "conpty.dll", "OpenConsole.exe"];
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let old = names.iter().any(|f| {
            n.strip_prefix(f).is_some_and(|rest| {
                rest == ".old"
                    || rest
                        .strip_prefix(".old.")
                        .is_some_and(|p| p.chars().all(|c| c.is_ascii_digit()))
            })
        });
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// An administrator's binary (Unix): the new file put in place with sudo, which asks for the
/// password in this terminal (never through ptyro), the old one kept beside until the new one
/// runs, then removed (or put back).
fn sudo_swap(
    new: &Path,
    target: &Path,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let place = r#"set -e; d=$(dirname "$2"); n=$(basename "$2"); cp "$1" "$d/.$n.new"; chmod 755 "$d/.$n.new"; cp -p "$2" "$d/.$n.old"; mv -f "$d/.$n.new" "$2""#;
    let back = r#"d=$(dirname "$1"); n=$(basename "$1"); if [ "$2" = ok ]; then rm -f "$d/.$n.old"; else mv -f "$d/.$n.old" "$1"; fi"#;
    let sudo = |script: &str, args: &[&Path], extra: Option<&str>| -> Result<(), String> {
        let mut cmd = Command::new("sudo");
        cmd.args(["sh", "-c", script, "sh"]).args(args);
        if let Some(x) = extra {
            cmd.arg(x);
        }
        let s = quiet(&mut cmd)
            .stdin(Stdio::inherit())
            .status()
            .map_err(|e| format!("couldn't run sudo: {e}"))?;
        if s.success() {
            Ok(())
        } else {
            Err(format!("sudo exited with {}", s.code().unwrap_or(1)))
        }
    };
    sudo(place, &[new, target], None)?;
    match check(target) {
        Ok(()) => {
            let _ = sudo(back, &[target], Some("ok"));
            Ok(())
        }
        Err(e) => {
            let _ = sudo(back, &[target], Some("undo"));
            Err(e)
        }
    }
}

/// The release's shell completions beside an install in `<prefix>/bin`, as `install.sh` puts
/// them (`<prefix>/share/ptyro/completions`), when that folder is there already.
fn completions(from: &Path, bin: &Path) {
    let (Some(prefix), Ok(list)) = (bin.parent(), std::fs::read_dir(from.join("completions")))
    else {
        return;
    };
    let to = prefix.join("share/ptyro/completions");
    if !to.is_dir() {
        return;
    }
    for e in list.flatten() {
        let _ = std::fs::copy(e.path(), to.join(e.file_name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_it_is_says_how_it_updates() {
        let c = |p: &str| classify(Path::new(p));
        assert_eq!(c("/src/ptyro/target/debug/ptyro"), How::Build);
        assert_eq!(c("/src/ptyro/target/debug/deps/ptyro-1a2b"), How::Build);
        assert_eq!(c(r"C:\src\ptyro\target\x86_64-pc-windows-msvc\release\ptyro.exe"), How::Build);
        assert_ne!(c("/src/ptyro/target/tmp/t1/bin/ptyro"), How::Build, "a test's own folder");
        assert_eq!(c("/opt/homebrew/Cellar/ptyro/0.0.4/bin/ptyro"), How::Brew);
        assert_eq!(c("/home/linuxbrew/.linuxbrew/bin/ptyro"), How::Brew);
        assert!(matches!(c("/home/me/.cargo/bin/ptyro"), How::Managed(m) if m.contains("cargo")));
        assert!(matches!(c("/usr/bin/ptyro"), How::Managed(_)));
        assert!(matches!(c("/snap/bin/ptyro"), How::Managed(_)));
        assert!(matches!(
            c(r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\x\ptyro.exe"),
            How::Managed(m) if m.contains("WinGet")
        ));
        let d = tempfile::tempdir().unwrap();
        assert_eq!(classify(&d.path().join("ptyro")), How::Release, "a folder the user can write");
        assert_eq!(
            brew_prefix(Path::new("/opt/homebrew/Cellar/ptyro/0.0.4/bin/ptyro")),
            Some(PathBuf::from("/opt/homebrew"))
        );
        assert_eq!(
            brew_prefix(Path::new("/home/linuxbrew/.linuxbrew/bin/ptyro")),
            Some(PathBuf::from("/home/linuxbrew/.linuxbrew"))
        );
    }

    #[test]
    fn the_checksum_line_is_found_in_either_mode() {
        let list = "aa11  ptyro-1.0.0-x.tar.gz\nbb22 *ptyro-1.0.0-y.zip\ncc33  SHA256SUMS.sig\n";
        assert_eq!(checksum_for(list, "ptyro-1.0.0-x.tar.gz").as_deref(), Some("aa11"));
        assert_eq!(checksum_for(list, "ptyro-1.0.0-y.zip").as_deref(), Some("bb22"));
        assert_eq!(checksum_for(list, "ptyro-1.0.0-z.tar.gz"), None);
    }

    #[test]
    fn an_administrators_install_gets_the_installers_command() {
        let c = admin_command(Path::new("/usr/local/bin/ptyro"), "0.0.64").unwrap();
        if cfg!(windows) {
            assert!(c.contains("install.ps1"), "{c}");
        } else {
            assert_eq!(
                c,
                "curl -fsSL https://github.com/nealhardesty/ptyro/releases/download/v0.0.64/install.sh | sudo env PTYRO_PREFIX=/usr/local PTYRO_VERSION=0.0.64 sh"
            );
        }
        assert_eq!(admin_command(Path::new("/opt/tools/ptyro"), "0.0.64"), None);
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_swap_keeps_the_old_files_until_the_new_one_runs() {
        let d = tempfile::tempdir().unwrap();
        let (dir, src) = (d.path().join("bin"), d.path().join("src"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(dir.join("ptyro"), "old").unwrap();
        std::fs::write(src.join("ptyro"), "new").unwrap();
        std::fs::write(src.join("helper"), "new helper").unwrap();
        let files =
            vec![(src.join("ptyro"), "ptyro".to_owned()), (src.join("helper"), "helper".into())];
        // The check sees the new file in place.
        let seen = std::cell::RefCell::new(String::new());
        swap(&dir, &files, "ptyro", &|p| {
            *seen.borrow_mut() = std::fs::read_to_string(p).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(*seen.borrow(), "new");
        assert_eq!(std::fs::read_to_string(dir.join("ptyro")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dir.join("helper")).unwrap(), "new helper");
        assert_eq!(names(&dir), ["helper", "ptyro"], "nothing left beside");
    }

    #[test]
    fn a_new_binary_that_doesnt_run_is_rolled_back() {
        let d = tempfile::tempdir().unwrap();
        let (dir, src) = (d.path().join("bin"), d.path().join("src"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(dir.join("ptyro"), "old").unwrap();
        std::fs::write(src.join("ptyro"), "broken").unwrap();
        std::fs::write(src.join("helper"), "new helper").unwrap();
        let files =
            vec![(src.join("ptyro"), "ptyro".to_owned()), (src.join("helper"), "helper".into())];
        let e = swap(&dir, &files, "ptyro", &|_| Err("it doesn't run".into())).unwrap_err();
        assert_eq!(e, "it doesn't run");
        assert_eq!(std::fs::read_to_string(dir.join("ptyro")).unwrap(), "old", "the old one back");
        assert_eq!(names(&dir), ["ptyro"], "a file that wasn't there before is gone again");
    }

    fn mine(command: Option<&str>, admin: bool, managed: Option<&str>) -> Outdated {
        Outdated {
            id: ID.into(),
            name: "ptyro".into(),
            program: ID.into(),
            version: "0.0.63".into(),
            latest: "0.0.64".into(),
            command: command.map(str::to_owned),
            via: None,
            admin,
            doc: RELEASES.into(),
            path: Some("/home/me/.local/bin/ptyro".into()),
            managed: managed.map(str::to_owned),
        }
    }

    #[test]
    fn ptyros_finding_says_how_it_updates_and_can_be_skipped() {
        assert_eq!(
            mine(None, false, None).finding(),
            "ptyro 0.0.64 is out (you have 0.0.63): `ptyro update ptyro` updates it (it downloads the release, checks it against SHA256SUMS, replaces /home/me/.local/bin/ptyro and upgrades the daemon live)"
        );
        assert!(
            mine(Some("brew upgrade ptyro"), false, None)
                .finding()
                .ends_with("(it runs `brew upgrade ptyro`, then upgrades the daemon live)")
        );
        assert!(mine(Some("curl …"), true, None).finding().contains("as administrator"));
        assert!(
            mine(None, false, Some("cargo (`cargo install`)"))
                .finding()
                .contains("it was installed with cargo (`cargo install`), so update it that way")
        );
        let o = mine(None, false, None);
        let ig = |l: &[&str]| {
            crate::updates::ignored(&l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), &o)
        };
        assert!(ig(&["ptyro"]));
        assert!(ig(&["PTYRO@0.0.64"]));
        assert!(!ig(&["ptyro@0.0.65"]));
        assert!(!ig(&["codex"]));
        let codex = Outdated { id: "codex".into(), name: "Codex".into(), path: None, ..o.clone() };
        assert_eq!(
            crate::updates::note_line(&[o, codex]).as_deref(),
            Some("2 newer versions are out: ptyro 0.0.64, Codex 0.0.64")
        );
    }

    #[test]
    fn leftovers_are_swept_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        for f in ["ptyro.exe", "ptyro.exe.old", "ptyro.exe.old.123", "conpty.dll.old", "notes.old"]
        {
            std::fs::write(d.path().join(f), "x").unwrap();
        }
        sweep(&d.path().join("ptyro.exe"));
        assert_eq!(names(d.path()), ["notes.old", "ptyro.exe"]);
    }
}
