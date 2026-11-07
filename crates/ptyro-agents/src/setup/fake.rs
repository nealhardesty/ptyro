//! The fake machine tests drive the engine with (debug builds only: `PTYRO_TEST_SETUP` names a
//! directory, read through `ptyro_core::test_knob`, so a release ignores it). Nothing real is
//! installed or asked: each step runs `ptyro fake-installer`, a real child process whose output
//! streams like an installer's, and which "installs" by writing a file the next survey finds.
//!
//! | file | role |
//! |---|---|
//! | `world.json` | the machine (read): see [`World`] |
//! | `installed/<id>` | an item installed since (written by the fake installer): its version |
//! | `calls.log` | each step run and each elevation asked, one per line (written) |
//! | `stored-path` | folders installers "added to the registry's Path", one per line (written) |
//! | `logged-in/<id>` | an item logged in since (written by the fake login, BOOT4) |
//! | `logins.log` | each login run and each status asked, one per line (written) |
//! | `new/<id>` | an item installed the recommended way beside its `other` copy (a move, BOOT7) |
//! | `removed/<id>` | its `other` copy removed (a move's uninstall) |
//!
//! An item with an `other` installer has two copies once a move installs the new one: the old
//! comes first (as an npm prefix early on `PATH` would) until it is removed.
//!
//! A login runs `ptyro fake-installer login ID` in the person's terminal: it prints a device
//! code as a real one would and "logs in" by writing `logged-in/<id>`. A status command is
//! answered from those files and `world.json`'s `logged_in`, for the items whose row has one.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ptyro_core::paths::Paths;
use serde::Deserialize;

use super::host::{Elevation, Host, Installed, Step, stream};
use crate::catalog::{Item, Method, Platform, Via};

/// `world.json`. Every field is optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct World {
    /// The platform to plan for (`linux`, `windows-x64`…); default: this one.
    pub platform: Option<Platform>,
    /// Programs on `PATH` besides the installed items' own: `apt-get`, `brew`, `sudo`…
    pub programs: Vec<String>,
    /// Installed items and their versions (`{"git": "2.43.0"}`).
    pub installed: BTreeMap<String, String>,
    /// Installed items that came another way (`{"claude": "npm"}`).
    pub other: BTreeMap<String, Via>,
    /// apt's or dnf's Node.js (`{"apt": "18.19.1"}`).
    pub candidates: BTreeMap<String, String>,
    /// `ready` (default), `prompt` or `unavailable`.
    pub elevation: Option<String>,
    /// What asking for it (`sudo -v`) gets: default yes.
    pub elevate: Option<bool>,
    /// Per item: `ok` (default), `fail`, `busy` (fails once as Windows does replacing a running
    /// program, then ok), `slow` (three lines, 300 ms apart, then ok), `wait` (a
    /// line, then ok once the fake's directory holds a file `go`, or after a minute: a run that
    /// is still going for as long as a test needs), or `quiet` (ok, but nothing to find
    /// afterwards: an installer that put it off `PATH`).
    pub outcomes: BTreeMap<String, String>,
    /// The version an item installs as (default `1.0.0`; Node.js `26.13.0`).
    pub versions: BTreeMap<String, String>,
    /// Per item: a folder (under the fake's directory) its installer adds only to the stored
    /// `Path`, as WinGet's Node.js does on Windows.
    pub path_adds: BTreeMap<String, String>,
    /// Per item: a folder that must be on the `PATH` its step runs with, or it fails as `npm`
    /// not found would.
    pub path_needs: BTreeMap<String, String>,
    /// Items logged in already (BOOT4).
    pub logged_in: Vec<String>,
    /// Per item, what its login does: `ok` (default: logs in), `fail` (exits 1, still logged
    /// out: a failure, or Ctrl+C), `nothing` (exits 0, still logged out: the person closed the
    /// page) or `slow` (the code, then two seconds before it logs in: time for a test's Ctrl+C).
    pub logins: BTreeMap<String, String>,
    /// Items whose `other` copy is in an administrator's folder (a system npm prefix, BOOT7).
    pub system: Vec<String>,
    /// Per item, what removing its `other` copy does (BOOT7): `ok` (default), `fail` (exits 1,
    /// the copy stays), `leftover` (exits 0, but the copy stays: npm acting on another prefix) or
    /// `both` (removes the new copy too, as a shared bin folder would).
    pub uninstalls: BTreeMap<String, String>,
    /// Per item, a second copy of its `other` installer, elsewhere on `PATH` ahead of the new one
    /// (another npm prefix): never removed by a move.
    pub second: Vec<String>,
    /// Per item, a copy from a third installer (`{"codex": "brew"}`), after the new one.
    pub third: BTreeMap<String, Via>,
    /// Items whose npm copy is in no npm prefix (Bun's global folder): no move is offered.
    pub no_prefix: Vec<String>,
}

impl World {
    fn read(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("world.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).map_err(|e| eprintln!("world.json: {e}")).ok())
            .unwrap_or_default()
    }
}

fn log(dir: &Path, what: &str) {
    log_to(dir, "calls.log", what);
}

fn log_to(dir: &Path, file: &str, what: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(file)) {
        let _ = writeln!(f, "{what}");
    }
}

pub(super) struct FakeHost {
    dir: PathBuf,
    world: World,
    used: AtomicBool,
    /// The `PATH` steps run with, refreshed from `stored-path` after each, as the real host does
    /// from the registry on Windows.
    path: Mutex<OsString>,
    dirs: Vec<PathBuf>,
}

impl FakeHost {
    pub(super) fn new(dir: PathBuf, paths: &Paths) -> Self {
        let world = World::read(&dir);
        let dirs = super::path_dirs(paths);
        let path = super::path_with(&dirs, &std::env::var_os("PATH").unwrap_or_default());
        Self { dir, world, used: AtomicBool::new(false), path: Mutex::new(path), dirs }
    }

    fn version_of(&self, id: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join("installed").join(id))
            .ok()
            .map(|v| v.trim().to_owned())
            .or_else(|| self.world.installed.get(id).cloned())
    }
}

impl Host for FakeHost {
    fn platform(&self) -> Platform {
        self.world.platform.unwrap_or_else(Platform::current)
    }

    fn has(&self, program: &str) -> bool {
        self.world.programs.iter().any(|p| p == program)
            || crate::catalog::items()
                .iter()
                .any(|i| i.program == program && self.version_of(&i.id).is_some())
            || (program == "npm" && self.version_of("node").is_some())
    }

    fn installed(&self, item: &Item) -> Option<Installed> {
        self.copies(item).into_iter().next()
    }

    fn copies(&self, item: &Item) -> Vec<Installed> {
        let id = &item.id;
        let via = self.world.other.get(id).copied();
        let mut out = Vec::new();
        if let Some(version) = self.version_of(id)
            && !self.dir.join("removed").join(id).exists()
        {
            out.push(Installed {
                version: Some(version).filter(|v| !v.is_empty()),
                path: Some(match via {
                    Some(v) => format!("(fake {}) {}", v.name(), item.program),
                    None => format!("(fake) {}", item.program),
                }),
                via,
                system: via.is_some() && self.world.system.contains(id),
                prefix: (via == Some(Via::Npm) && !self.world.no_prefix.contains(id))
                    .then(|| self.dir.join("npm-prefix").display().to_string()),
            });
        }
        if self.world.second.contains(id)
            && let Some(version) = self.version_of(id)
        {
            out.push(Installed {
                version: Some(version).filter(|v| !v.is_empty()),
                path: Some(format!("(fake second) {}", item.program)),
                via,
                system: false,
                prefix: None,
            });
        }
        if via.is_some()
            && let Ok(v) = std::fs::read_to_string(self.dir.join("new").join(id))
        {
            out.push(Installed {
                version: Some(v.trim().to_owned()).filter(|v| !v.is_empty()),
                path: Some(format!("(fake) {}", item.program)),
                via: None,
                system: false,
                prefix: None,
            });
        }
        if let Some(t) = self.world.third.get(id) {
            out.push(Installed {
                version: Some("0.1.0".into()),
                path: Some(format!("(fake {}) {}", t.name(), item.program)),
                via: Some(*t),
                system: false,
                prefix: None,
            });
        }
        out
    }

    fn uninstall(
        &self,
        item: &Item,
        method: &Method,
        command: &str,
        line: &mut dyn FnMut(&str),
    ) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let admin = self.world.system.contains(&item.id);
        let argv = vec![
            exe.to_string_lossy().into_owned(),
            "fake-installer".to_owned(),
            item.id.clone(),
            method.via.name().to_owned(),
            "uninstall".to_owned(),
            admin.to_string(),
            command.to_owned(),
        ];
        let env = [("PTYRO_TEST_SETUP".into(), self.dir.clone().into_os_string())];
        stream(&argv, &env, line)
    }

    fn candidate(&self, via: Via) -> Option<String> {
        self.world.candidates.get(via.name()).cloned()
    }

    fn elevation(&self) -> Elevation {
        match self.world.elevation.as_deref() {
            Some("prompt") => Elevation::Prompt,
            Some("unavailable") => Elevation::Unavailable,
            _ => Elevation::Ready,
        }
    }

    fn elevate(&self) -> bool {
        log(&self.dir, "elevate");
        self.world.elevate.unwrap_or(true)
    }

    fn run(&self, step: &Step<'_>, line: &mut dyn FnMut(&str)) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let argv = vec![
            exe.to_string_lossy().into_owned(),
            "fake-installer".to_owned(),
            step.item.id.clone(),
            step.method.via.name().to_owned(),
            if step.update { "update" } else { "install" }.to_owned(),
            step.method.admin.to_string(),
        ];
        let path = self.path.lock().map(|p| p.clone()).unwrap_or_default();
        let env =
            [("PTYRO_TEST_SETUP".into(), self.dir.clone().into_os_string()), ("PATH".into(), path)];
        let r = stream(&argv, &env, line);
        if r.is_ok() && step.method.via == Via::NodeDist {
            self.used.store(true, Ordering::Relaxed);
        }
        if r.is_ok()
            && let Ok(mut p) = self.path.lock()
        {
            let stored: Vec<PathBuf> = std::fs::read_to_string(self.dir.join("stored-path"))
                .unwrap_or_default()
                .lines()
                .map(PathBuf::from)
                .collect();
            *p = super::refreshed_path(&p, &stored, &self.dirs);
        }
        r
    }

    fn tools_used(&self) -> bool {
        self.used.load(Ordering::Relaxed)
    }

    fn status(&self, argv: &[String]) -> Option<bool> {
        let item = crate::catalog::items()
            .iter()
            .find(|i| i.login.as_ref().and_then(|l| l.check.as_ref()) == Some(&argv.to_vec()))?;
        log_to(&self.dir, "logins.log", &format!("status {}", item.id));
        Some(
            self.dir.join("logged-in").join(&item.id).exists()
                || self.world.logged_in.contains(&item.id),
        )
    }

    fn interact(&self, argv: &[String]) -> Result<Option<String>, String> {
        let item = crate::catalog::items()
            .iter()
            .find(|i| i.login.as_ref().map(|l| &l.start) == Some(&argv.to_vec()))
            .ok_or("the fake knows no login like that")?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let status = std::process::Command::new(exe)
            .args(["fake-installer", "login", &item.id])
            .env("PTYRO_TEST_SETUP", &self.dir)
            .status()
            .map_err(|e| e.to_string())?;
        Ok((!status.success()).then(|| "it exited with 1".to_owned()))
    }

    /// The fake machine has no network: its newest versions come from a fixture registry
    /// (`PTYRO_TEST_LATEST`, [`crate::updates`]) or not at all.
    fn latest_reply(&self, url: &str) -> Result<String, String> {
        Err(format!("couldn't reach {url}: the fake machine is offline"))
    }
}

/// `ptyro fake-installer ID VIA install|update ADMIN` (hidden, debug builds): what each fake
/// step runs, and `ptyro fake-installer login ID` each fake login (BOOT4). Its outcome comes
/// from `world.json`.
pub fn installer(args: &[String]) -> i32 {
    let Some(dir) = ptyro_core::test_knob("PTYRO_TEST_SETUP").map(PathBuf::from) else {
        eprintln!("fake-installer: PTYRO_TEST_SETUP is not set");
        return 2;
    };
    if let [login, id] = args
        && login == "login"
    {
        return fake_login(&dir, id);
    }
    let (id, via, what, admin, command) = match args {
        [id, via, what, admin] => (id, via, what, admin, None),
        [id, via, what, admin, command] => (id, via, what, admin, Some(command)),
        _ => {
            eprintln!("fake-installer: expected ID VIA install|update|uninstall ADMIN [COMMAND]");
            return 2;
        }
    };
    let world = World::read(&dir);
    log(&dir, &format!("{what} {id} via {via} admin={admin}"));
    if what == "uninstall" {
        log_to(&dir, "uninstall.log", command.map_or("", String::as_str));
        return fake_uninstall(&dir, &world, id, via);
    }
    let outcome = world.outcomes.get(id).map_or("ok", String::as_str);
    println!("fake {via}: {what} {id}");
    if let Some(need) = world.path_needs.get(id) {
        let want = dir.join(need);
        let path = std::env::var_os("PATH").unwrap_or_default();
        if !std::env::split_paths(&path).any(|p| p == want) {
            eprintln!("fake {via}: {need} isn't on PATH (npm: command not found)");
            return 1;
        }
    }
    match outcome {
        "fail" => {
            eprintln!("fake {via}: E: could not reach the server for {id}");
            return 1;
        }
        // Windows refusing to replace a running program, the first time only (BOOT5).
        "busy" if !dir.join(format!("busy-{id}")).exists() => {
            let _ = std::fs::write(dir.join(format!("busy-{id}")), "");
            eprintln!(
                "fake {via}: The process cannot access the file because it is being used by another process."
            );
            return 1;
        }
        // Installs once; every install after fails (a reinstall that doesn't take).
        "once" if dir.join(format!("once-{id}")).exists() => {
            eprintln!("fake {via}: E: could not reach the server for {id}");
            return 1;
        }
        "once" => {
            let _ = std::fs::write(dir.join(format!("once-{id}")), "");
        }
        "slow" => {
            for n in 1..=3 {
                println!("fake {via}: step {n} of 3");
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        "wait" => {
            println!("fake {via}: waiting for go");
            let _ = std::io::stdout().flush();
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while !dir.join("go").exists() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        _ => {}
    }
    if outcome != "quiet" {
        let default = if id == "node" { "26.13.0" } else { "1.0.0" };
        let v = world.versions.get(id).map_or(default, String::as_str);
        // Installed the recommended way beside a copy that came another way: a second copy (a
        // move, BOOT7); else the item's one copy.
        let beside = what == "install" && world.other.get(id).is_some_and(|o| o.name() != via);
        let d = dir.join(if beside { "new" } else { "installed" });
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(id), v);
        if let Some(add) = world.path_adds.get(id)
            && let Ok(mut f) =
                std::fs::OpenOptions::new().create(true).append(true).open(dir.join("stored-path"))
        {
            let _ = writeln!(f, "{}", dir.join(add).display());
        }
        // nodejs.org's build lands in ptyro's tools folder, as the real one does.
        if via == "node-dist"
            && let Ok(paths) = Paths::from_env()
            && let Some(bin) = super::path_dirs(&paths).first()
        {
            let _ = std::fs::create_dir_all(bin);
        }
    }
    println!("fake {via}: {id} is installed");
    0
}

/// A move's removal of the `other` copy (BOOT7), by `world.json`'s `uninstalls`.
fn fake_uninstall(dir: &Path, world: &World, id: &str, via: &str) -> i32 {
    println!("fake {via}: removing {id}");
    match world.uninstalls.get(id).map_or("ok", String::as_str) {
        "fail" => {
            eprintln!("fake {via}: EACCES: permission denied, unlink '{id}'");
            1
        }
        "leftover" => {
            println!("fake {via}: removed 0 packages");
            0
        }
        "both" => {
            let _ = std::fs::create_dir_all(dir.join("removed"));
            let _ = std::fs::write(dir.join("removed").join(id), "");
            let _ = std::fs::remove_file(dir.join("new").join(id));
            println!("fake {via}: removed {id}");
            0
        }
        _ => {
            let d = dir.join("removed");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join(id), "");
            println!("fake {via}: removed {id}");
            0
        }
    }
}

/// `ptyro fake-installer login ID`: a login as the person sees it, a device code and a link,
/// then (by `world.json`'s `logins`) logged in, failed, or nothing done.
fn fake_login(dir: &Path, id: &str) -> i32 {
    let world = World::read(dir);
    log_to(dir, "logins.log", &format!("login {id}"));
    println!("fake login: open https://example.invalid/device and enter the code FAKE-{id}");
    match world.logins.get(id).map_or("ok", String::as_str) {
        "fail" => {
            eprintln!("fake login: the code expired");
            1
        }
        "nothing" => 0,
        "slow" => {
            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::thread::sleep(std::time::Duration::from_secs(2));
            let d = dir.join("logged-in");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join(id), "");
            println!("fake login: logged in");
            0
        }
        _ => {
            let d = dir.join("logged-in");
            let _ = std::fs::create_dir_all(&d);
            let _ = std::fs::write(d.join(id), "");
            println!("fake login: logged in");
            0
        }
    }
}
