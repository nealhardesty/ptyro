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

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ptyro_core::paths::Paths;
use serde::Deserialize;

use super::host::{Elevation, Host, Installed, Step, stream};
use crate::catalog::{Item, Platform, Via};

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
    /// Per item: `ok` (default), `fail`, `slow` (three lines, 300 ms apart, then ok), or
    /// `quiet` (ok, but nothing to find afterwards: an installer that put it off `PATH`).
    pub outcomes: BTreeMap<String, String>,
    /// The version an item installs as (default `1.0.0`; Node.js `26.13.0`).
    pub versions: BTreeMap<String, String>,
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
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("calls.log"))
    {
        let _ = writeln!(f, "{what}");
    }
}

pub(super) struct FakeHost {
    dir: PathBuf,
    world: World,
    used: AtomicBool,
}

impl FakeHost {
    pub(super) fn new(dir: PathBuf, _paths: &Paths) -> Self {
        let world = World::read(&dir);
        Self { dir, world, used: AtomicBool::new(false) }
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
        let version = self.version_of(&item.id)?;
        Some(Installed {
            version: Some(version).filter(|v| !v.is_empty()),
            path: Some(format!("(fake) {}", item.program)),
            via: self.world.other.get(&item.id).copied(),
        })
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
        let env = [("PTYRO_TEST_SETUP".into(), self.dir.clone().into_os_string())];
        let r = stream(&argv, &env, line);
        if r.is_ok() && step.method.via == Via::NodeDist {
            self.used.store(true, Ordering::Relaxed);
        }
        r
    }

    fn tools_used(&self) -> bool {
        self.used.load(Ordering::Relaxed)
    }
}

/// `ptyro fake-installer ID VIA install|update ADMIN` (hidden, debug builds): what each fake
/// step runs. Its outcome comes from `world.json`.
pub fn installer(args: &[String]) -> i32 {
    let Some(dir) = ptyro_core::test_knob("PTYRO_TEST_SETUP").map(PathBuf::from) else {
        eprintln!("fake-installer: PTYRO_TEST_SETUP is not set");
        return 2;
    };
    let [id, via, what, admin] = args else {
        eprintln!("fake-installer: expected ID VIA install|update ADMIN");
        return 2;
    };
    let world = World::read(&dir);
    log(&dir, &format!("{what} {id} via {via} admin={admin}"));
    let outcome = world.outcomes.get(id).map_or("ok", String::as_str);
    println!("fake {via}: {what} {id}");
    match outcome {
        "fail" => {
            eprintln!("fake {via}: E: could not reach the server for {id}");
            return 1;
        }
        "slow" => {
            for n in 1..=3 {
                println!("fake {via}: step {n} of 3");
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        _ => {}
    }
    if outcome != "quiet" {
        let default = if id == "node" { "26.13.0" } else { "1.0.0" };
        let v = world.versions.get(id).map_or(default, String::as_str);
        let d = dir.join("installed");
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(id), v);
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
