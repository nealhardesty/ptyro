//! ptyro updates itself (ROADMAP BOOT6, SPEC §24.8), end to end, with no network: ptyro's newest
//! version comes from the fixture registry (`PTYRO_TEST_LATEST/ptyro`, GitHub's release JSON),
//! its release from a fake release server on 127.0.0.1 (`PTYRO_TEST_RELEASE_URL`, as
//! `install.sh`'s `PTYRO_RELEASE_URL`), and the binary it replaces is one of the test's own
//! (`PTYRO_TEST_SELF_EXE`), never the build under test. WSL is `ptyro fake-wsl`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
/// The version under test: what a release built from this tree says it is.
const V: &str = env!("CARGO_PKG_VERSION");
const EXE: &str = if cfg!(windows) { "ptyro.exe" } else { "ptyro" };

/// A fake GitHub release: files served from a folder, each request logged.
struct Server {
    url: String,
    log: Arc<Mutex<Vec<String>>>,
}

fn serve(dir: PathBuf) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&log);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let name = first.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/');
            seen.lock().unwrap().push(name.to_owned());
            let mut s = s;
            match std::fs::read(dir.join(name)) {
                Ok(body) => {
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(&body);
                }
                Err(_) => {
                    let _ = s.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });
    Server { url, log }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A computer with an installed ptyro of the test's own, a registry and a release server.
struct Box_ {
    dir: tempfile::TempDir,
    server: Server,
    extra: Vec<(String, String)>,
}

impl Box_ {
    /// `world`: the fake machine (its platform decides whether there is another half).
    fn new(world: Value) -> Self {
        // In the target folder, so the build can be hard-linked rather than copied.
        let dir = tempfile::Builder::new()
            .prefix("boot6-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .unwrap();
        for d in ["bin", "fake", "home", "ptyro", "registry", "release", "stage", "wsl"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("fake/world.json"), world.to_string()).unwrap();
        let server = serve(dir.path().join("release"));
        Self { dir, server, extra: Vec::new() }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// The installed ptyro (what `ptyro update ptyro` replaces).
    fn installed(&self) -> PathBuf {
        self.path("bin").join(EXE)
    }

    /// This build, as the installed ptyro (it says it is `V`).
    fn install_this_build(&self) {
        if std::fs::hard_link(BIN, self.installed()).is_err() {
            std::fs::copy(BIN, self.installed()).unwrap();
        }
    }

    /// The newest release the registry says.
    fn publish(&self, version: &str) {
        std::fs::write(
            self.path("registry/ptyro"),
            json!({ "tag_name": format!("v{version}") }).to_string(),
        )
        .unwrap();
    }

    /// The release's archive for this machine holding `binary` as ptyro, and its SHA256SUMS;
    /// `tamper`: the sums say another file's checksum.
    fn release(&self, version: &str, binary: &[u8], tamper: bool) {
        let file = ptyro_agents::own::archive(version);
        let stem = file.trim_end_matches(".tar.gz").trim_end_matches(".zip").to_owned();
        let stage = self.path("stage").join(&stem);
        std::fs::create_dir_all(&stage).unwrap();
        let bin = stage.join(EXE);
        std::fs::write(&bin, binary).unwrap();
        executable(&bin);
        let out = self.path("release").join(&file);
        let tar = if cfg!(windows) {
            PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32").join("tar.exe")
        } else {
            PathBuf::from("tar")
        };
        let flags = if cfg!(windows) { "-a" } else { "-z" };
        let ok = Command::new(tar)
            .args([flags, "-cf"])
            .arg(&out)
            .arg("-C")
            .arg(self.path("stage"))
            .arg(&stem)
            .status()
            .unwrap();
        assert!(ok.success(), "tar");
        let bytes = std::fs::read(&out).unwrap();
        let sum = if tamper { sha256(b"something else") } else { sha256(&bytes) };
        std::fs::write(self.path("release/SHA256SUMS"), format!("{sum}  {file}\n")).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_HOME", self.path("ptyro"))
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_TEST_SETUP", self.path("fake"))
            .env("PTYRO_TEST_LATEST", self.path("registry"))
            .env("PTYRO_TEST_RELEASE_URL", &self.server.url)
            .env("PTYRO_TEST_SELF_EXE", self.installed())
            .env("PTYRO_WSL", self.path("no-wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1")
            .env("PTYRO_VERIFY", "0");
        for v in [
            "PTYRO_SOCKET",
            "PTYRO_SESSION_ID",
            "PTYRO_CONFIG",
            "PTYRO_REMOTE",
            "PTYRO_TEST_WINDOWS_PTYRO",
        ] {
            c.env_remove(v);
        }
        for (k, v) in &self.extra {
            c.env(k, v);
        }
        c
    }

    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    /// What was fetched from the release server.
    fn fetched(&self) -> Vec<String> {
        self.server.log.lock().unwrap().clone()
    }

    /// The files in the installed ptyro's folder.
    fn bin_files(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.path("bin"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[cfg(unix)]
fn executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn executable(_: &Path) {}

/// What `--version` of the binary at `p` says.
fn version_of(p: &Path) -> String {
    let o = Command::new(p).arg("--version").output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// A shell script standing in for a ptyro of `version`: it answers `--version`, and runs this
/// build for anything else (so a daemon can be handed over to it).
#[cfg(unix)]
fn script(version: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo \"ptyro {version}\"; exit 0; fi\nexec \"{BIN}\" \"$@\"\n"
    )
    .into_bytes()
}

/// The `updates` checks of `doctor --json`.
fn doctor_updates(b: &Box_) -> Vec<Value> {
    let o = b.run(&["doctor", "--json"], "");
    let all: Vec<Value> =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
    all.into_iter().filter(|c| c["section"] == "updates").collect()
}

#[test]
fn a_newer_ptyro_is_offered_and_nothing_is_fetched_without_a_yes() {
    let b = Box_::new(json!({ "platform": "linux" }));
    b.install_this_build();
    b.publish("999.0.0");
    // `--check` looks the newest release up (the registry), and says the update.
    let o = b.run(&["update", "--check", "--json"], "");
    assert!(o.status.success(), "{}", text(&o));
    let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
    let mine = doc["outdated"].as_array().unwrap().iter().find(|o| o["id"] == "ptyro").cloned();
    let mine = mine.unwrap_or_else(|| panic!("ptyro is offered: {doc}"));
    assert_eq!(mine["version"], V);
    assert_eq!(mine["latest"], "999.0.0");
    assert_eq!(mine["path"], b.installed().to_string_lossy().as_ref());
    // doctor says it, with the one-step update and what it does.
    let checks = doctor_updates(&b);
    let c = checks.iter().find(|c| c["name"] == "ptyro").expect("doctor's finding");
    assert_eq!(c["level"], "warn");
    let words = c["text"].as_str().unwrap();
    assert!(words.contains("ptyro 999.0.0 is out"), "{words}");
    assert!(words.contains("`ptyro update ptyro` updates it"), "{words}");
    assert!(words.contains("SHA256SUMS"), "{words}");
    assert_eq!(c["data"]["id"], "ptyro", "the web reads the finding's data");
    // The question, answered no: nothing downloaded, nothing replaced.
    let o = b.run(&["update", "ptyro"], "n\n");
    assert!(o.status.success(), "{}", text(&o));
    let said = text(&o);
    assert!(said.contains(&format!("ptyro {V} → 999.0.0")), "{said}");
    assert!(said.contains("upgraded live"), "{said}");
    assert!(said.contains("Nothing updated."), "{said}");
    assert!(b.fetched().is_empty(), "the binary is fetched only after a yes: {:?}", b.fetched());
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"));
    // Skipped by updates.ignore: `ptyro update` leaves it.
    let cfg = ptyro_testkit::paths_for(&b.path("ptyro")).unwrap().config;
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "[updates]\nignore = [\"ptyro\"]\n").unwrap();
    let o = b.run(&["update", "--yes"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("ptyro 999.0.0 is out"), "{}", text(&o));
    assert!(text(&o).contains("skipped (updates.ignore)"), "{}", text(&o));
    assert!(b.fetched().is_empty(), "{:?}", b.fetched());
}

#[test]
fn a_tampered_download_is_refused_and_nothing_is_replaced() {
    let b = Box_::new(json!({ "platform": "linux" }));
    b.install_this_build();
    let before = std::fs::metadata(b.installed()).unwrap().len();
    b.publish("999.0.0");
    b.release("999.0.0", b"not ptyro at all", true);
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains("checksum"), "{said}");
    assert!(said.contains("changed or broken on the way, so it isn't installed"), "{said}");
    assert!(said.contains("nothing was replaced"), "{said}");
    let file = ptyro_agents::own::archive("999.0.0");
    assert!(b.fetched().contains(&file), "{:?}", b.fetched());
    assert_eq!(std::fs::metadata(b.installed()).unwrap().len(), before, "untouched");
    assert_eq!(b.bin_files(), [EXE], "nothing left beside it");
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"));
}

#[test]
fn a_new_ptyro_that_doesnt_run_is_rolled_back() {
    let b = Box_::new(json!({ "platform": "linux" }));
    b.install_this_build();
    b.publish("999.0.0");
    // Checksummed correctly, but not a program.
    b.release("999.0.0", b"\x7fELF? no: a broken download", false);
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains("checksum ok"), "{said}");
    assert!(said.contains("didn't run"), "{said}");
    assert!(said.contains("the old one is back in place"), "{said}");
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"), "the old one runs");
    assert_eq!(b.bin_files(), [EXE], "nothing left beside it");
    // Still offered: it wasn't updated.
    let o = b.run(&["update", "--check", "--json"], "");
    let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(doc["outdated"].as_array().unwrap().iter().any(|o| o["id"] == "ptyro"), "{doc}");
}

#[test]
fn a_build_is_never_offered_or_looked_up() {
    let b = Box_::new(json!({ "platform": "linux" }));
    b.publish("999.0.0");
    let mut c = b.cmd(&["update", "ptyro"]);
    c.env_remove("PTYRO_TEST_SELF_EXE");
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("is a build"), "{}", text(&o));
    let asked = std::fs::read_to_string(b.path("registry/asked.log")).unwrap_or_default();
    assert!(!asked.lines().any(|l| l == "ptyro"), "never looked up: {asked}");
}

#[cfg(unix)]
#[test]
fn a_yes_installs_it_and_the_daemon_upgrades_live_with_every_session() {
    use ptyro_testkit::TempDaemon;
    let b = Box_::new(json!({ "platform": "linux" }));
    std::fs::write(b.installed(), script("0.0.1")).unwrap();
    executable(&b.installed());
    b.publish(V);
    b.release(V, &script(V), false);
    let d = TempDaemon::start_with_env(BIN, &[("SHELL", "/bin/sh")]).unwrap();
    let daemon = |args: &[&str]| d.cmd().args(args).output().unwrap();
    let new = daemon(&["new", "-d", "-n", "keep", "--", "sh", "-c", "echo up; sleep 120"]);
    assert!(new.status.success(), "{}", text(&new));
    let status = |out: Output| -> Value { serde_json::from_slice(&out.stdout).unwrap() };
    let before = status(daemon(&["daemon", "status", "--json"]));
    // The update, run against that daemon.
    let mut c = b.cmd(&["update", "ptyro"]);
    for (k, v) in d.env() {
        if k != "PTYRO_TEST_LATEST" && k != "PTYRO_HOME" {
            c.env(k, v);
        }
    }
    c.env("PTYRO_HOME", d.home.path());
    let mut child =
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"y\n").unwrap();
    let o = child.wait_with_output().unwrap();
    let said = text(&o);
    assert!(o.status.success(), "{said}");
    for step in ["checksum ok", &format!("ptyro {V} is in"), "upgraded in", "session(s)"] {
        assert!(said.contains(step), "{step:?} in {said}");
    }
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"));
    assert_eq!(b.bin_files(), [EXE], "the old one isn't kept once the new one runs");
    let file = ptyro_agents::own::archive(V);
    assert_eq!(b.fetched(), [file, "SHA256SUMS".to_owned()]);
    // The daemon was handed over, the session with it.
    let after = status(daemon(&["daemon", "status", "--json"]));
    assert!(after["generation"].as_u64() > before["generation"].as_u64(), "{after}");
    let ls: Value = status(daemon(&["ls", "--json"]));
    let keep = ls.as_array().unwrap().iter().find(|s| s["name"] == "keep").expect("kept");
    assert!(ptyro_pty::alive(keep["pid"].as_u64().unwrap() as u32), "{keep}");
    // Nothing offered now.
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(
            ptyro_testkit::paths_for(d.home.path()).unwrap().state.join("versions.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(!v["outdated"].as_array().unwrap().iter().any(|o| o["id"] == "ptyro"), "{v}");
}

/// Windows with WSL: Windows' ptyro updates itself, then asks the distro's ptyro to update
/// itself too (through `wsl.exe`), so both halves are the same version.
#[cfg(unix)]
#[test]
fn on_windows_wsls_half_is_brought_along() {
    let b = Box_::new(json!({ "platform": "windows-x64" }));
    std::fs::write(b.installed(), script("0.0.1")).unwrap();
    executable(&b.installed());
    b.publish(V);
    b.release(V, &script(V), false);
    // fake-wsl: a distro Ubuntu with ptyro, its own installed binary 0.0.1.
    let wsl = b.path("wsl");
    std::fs::write(wsl.join("distros"), "Ubuntu\n").unwrap();
    for d in ["ptyro", "fake", "home", "bin"] {
        std::fs::create_dir_all(wsl.join("Ubuntu").join(d)).unwrap();
    }
    std::fs::write(wsl.join("Ubuntu/fake/world.json"), json!({"platform": "linux"}).to_string())
        .unwrap();
    let theirs = wsl.join("Ubuntu/bin/ptyro");
    std::fs::write(&theirs, script("0.0.1")).unwrap();
    executable(&theirs);
    let stand_in = ptyro_testkit::program::program(
        &b.path("tools"),
        "wsl",
        BIN,
        &format!("exec \"{BIN}\" fake-wsl \"$@\""),
        &[BIN, "fake-wsl"],
        &[],
    );
    let mut b = b;
    b.extra = vec![
        ("PTYRO_WSL".into(), stand_in.display().to_string()),
        ("PTYRO_FAKE_WSL".into(), wsl.display().to_string()),
    ];
    let o = b.run(&["update", "ptyro"], "y\n");
    let said = text(&o);
    assert!(o.status.success(), "{said}");
    assert!(said.contains("and WSL's ptyro (Ubuntu) 0.0.1"), "said in the question: {said}");
    assert!(said.contains(&format!("✓ WSL's ptyro (Ubuntu) {V}")), "{said}");
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"));
    assert_eq!(version_of(&theirs), format!("ptyro {V}"), "WSL's half too");
    let log = std::fs::read_to_string(wsl.join("wsl.log")).unwrap();
    assert!(log.lines().any(|l| l.ends_with("update ptyro --yes")), "{log}");
    // Asked again: both the newest, nothing to do, and the halves agree.
    let o = b.run(&["update", "ptyro"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains(&format!("✓ WSL's ptyro (Ubuntu) is {V} too")), "{}", text(&o));
}

/// Inside WSL: Windows' ptyro.exe is asked to update itself too; one too old to know how says
/// the fix.
#[cfg(unix)]
#[test]
fn in_wsl_windows_half_is_brought_along_or_its_fix_said() {
    let mut b = Box_::new(json!({ "platform": "wsl" }));
    std::fs::write(b.installed(), script("0.0.1")).unwrap();
    executable(&b.installed());
    b.publish(V);
    b.release(V, &script(V), false);
    let win = b.path("win.exe");
    let log = b.path("win.log");
    let done = b.path("win.done");
    let old = b.path("win.old");
    std::fs::write(&old, "").unwrap();
    std::fs::write(
        &win,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\nif [ \"$1\" = --version ]; then if [ -f '{done}' ]; then echo 'ptyro {V}'; else echo 'ptyro 0.0.1'; fi; exit 0; fi\nif [ -f '{old}' ]; then echo \"error: ptyro isn't something ptyro installs\" >&2; exit 3; fi\ntouch '{done}'\n",
            log = log.display(),
            done = done.display(),
            old = old.display()
        ),
    )
    .unwrap();
    executable(&win);
    b.extra = vec![("PTYRO_TEST_WINDOWS_PTYRO".into(), win.display().to_string())];
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains(&format!("ptyro {V} is in")), "this half updated: {said}");
    assert!(said.contains("✗ Windows' ptyro.exe wasn't updated"), "{said}");
    assert!(said.contains("install.ps1"), "the fix for one from before: {said}");
    // A ptyro.exe that knows how.
    std::fs::remove_file(&old).unwrap();
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert!(o.status.success(), "{said}");
    assert!(said.contains(&format!("✓ Windows' ptyro.exe {V}")), "{said}");
    let asked = std::fs::read_to_string(&log).unwrap();
    assert!(asked.lines().any(|l| l == "update ptyro --yes"), "{asked}");
}

/// Homebrew's ptyro is updated through Homebrew, never overwritten.
#[cfg(unix)]
#[test]
fn homebrews_ptyro_updates_through_homebrew() {
    let mut b = Box_::new(json!({ "platform": "macos" }));
    let keg = b.path("brew/Cellar/ptyro/0.0.1/bin");
    std::fs::create_dir_all(&keg).unwrap();
    std::fs::create_dir_all(b.path("brew/bin")).unwrap();
    let mine = keg.join("ptyro");
    std::fs::write(&mine, script("0.0.1")).unwrap();
    executable(&mine);
    std::os::unix::fs::symlink(&mine, b.path("brew/bin/ptyro")).unwrap();
    b.publish(V);
    // `brew upgrade ptyro`: a new keg, the link moved to it.
    let tools = b.path("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let newkeg = b.path("brew/Cellar/ptyro/new/bin");
    let brew_log = b.path("brew.log");
    std::fs::write(
        tools.join("brew"),
        format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\nmkdir -p '{nk}'\nprintf '%s' '{body}' > '{nk}/ptyro'\nchmod 755 '{nk}/ptyro'\nln -sfn '{nk}/ptyro' '{link}'\necho '==> Upgrading ptyro'\n",
            log = brew_log.display(),
            nk = newkeg.display(),
            body = String::from_utf8(script(V)).unwrap(),
            link = b.path("brew/bin/ptyro").display()
        ),
    )
    .unwrap();
    executable(&tools.join("brew"));
    b.extra = vec![
        ("PTYRO_TEST_SELF_EXE".into(), mine.display().to_string()),
        ("PATH".into(), ptyro_testkit::program::path_with(&tools)),
    ];
    let o = b.run(&["update", "--check", "--json"], "");
    let doc: Value = serde_json::from_slice(&o.stdout).unwrap();
    let f = doc["outdated"].as_array().unwrap().iter().find(|o| o["id"] == "ptyro").cloned();
    assert_eq!(f.expect("offered")["command"], "brew upgrade ptyro", "{doc}");
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert!(o.status.success(), "{said}");
    assert!(said.contains("brew upgrade ptyro"), "{said}");
    assert!(said.contains("Upgrading ptyro"), "brew's own output: {said}");
    assert_eq!(std::fs::read_to_string(&brew_log).unwrap().trim(), "upgrade ptyro");
    assert_eq!(version_of(&mine), "ptyro 0.0.1", "Homebrew's file is never overwritten");
    assert_eq!(version_of(&b.path("brew/bin/ptyro")), format!("ptyro {V}"));
    assert!(b.fetched().is_empty(), "nothing downloaded by ptyro: {:?}", b.fetched());
}

/// A ptyro cargo installed is said, with how, and never replaced.
#[test]
fn a_package_managers_ptyro_is_said_not_replaced() {
    let mut b = Box_::new(json!({ "platform": "linux" }));
    let cargo = b.path("home/.cargo/bin");
    std::fs::create_dir_all(&cargo).unwrap();
    let mine = cargo.join(EXE);
    if std::fs::hard_link(BIN, &mine).is_err() {
        std::fs::copy(BIN, &mine).unwrap();
    }
    b.extra = vec![("PTYRO_TEST_SELF_EXE".into(), mine.display().to_string())];
    b.publish("999.0.0");
    let o = b.run(&["update", "--yes"], "");
    assert!(o.status.success(), "`ptyro update` doesn't fail for it: {}", text(&o));
    assert!(text(&o).contains("installed with cargo"), "{}", text(&o));
    let o = b.run(&["update", "ptyro", "--yes"], "");
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("never replaces a binary a package manager owns"), "{}", text(&o));
    assert!(b.fetched().is_empty());
}

/// `POST path` with JSON (and a bearer token) → the JSON body.
#[cfg(unix)]
fn http(port: u16, path: &str, token: Option<&str>, body: &Value) -> Value {
    use std::io::Read;
    let body = body.to_string();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null)
}

/// The web's "Update ptyro" (`updates.self`): `ptyro update ptyro --yes` in a session on the
/// daemon, whose progress the page shows; the daemon it runs in is upgraded live under it.
#[cfg(unix)]
#[test]
fn the_web_runs_it_in_a_session_the_daemons_live_upgrade_keeps() {
    use ptyro_testkit::TempDaemon;
    use std::time::{Duration, Instant};
    let b = Box_::new(json!({ "platform": "linux" }));
    std::fs::write(b.installed(), script("0.0.1")).unwrap();
    executable(&b.installed());
    b.publish(V);
    b.release(V, &script(V), false);
    let s = |p: PathBuf| p.display().to_string();
    let env = [
        ("SHELL".to_owned(), "/bin/sh".to_owned()),
        ("HOME".into(), s(b.path("home"))),
        ("PTYRO_TEST_SETUP".into(), s(b.path("fake"))),
        ("PTYRO_TEST_LATEST".into(), s(b.path("registry"))),
        ("PTYRO_TEST_RELEASE_URL".into(), b.server.url.clone()),
        ("PTYRO_TEST_SELF_EXE".into(), s(b.installed())),
        ("PTYRO_VERIFY".into(), "0".into()),
        ("PTYRO_WSL".into(), s(b.path("no-wsl"))),
    ];
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let d = TempDaemon::start_with_env(BIN, &pairs).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut serve = d
        .cmd()
        .args(["serve", "--loopback-http", "--port", &port.to_string()])
        .envs(pairs.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    let o = d.cmd().args(["pair", "phone", "--scope", "full", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    let token = http(port, "/api/pair", None, &json!({ "code": code }))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let before: Value = serde_json::from_slice(
        &d.cmd().args(["daemon", "status", "--json"]).output().unwrap().stdout,
    )
    .unwrap();
    let v = http(port, "/api/host", Some(&token), &json!({ "op": "updates.self" }));
    let session = v["reply"]["session"].as_str().unwrap_or_else(|| panic!("{v}")).to_owned();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let o = d.cmd().args(["read", &session]).output().unwrap();
        let screen = String::from_utf8_lossy(&o.stdout).into_owned();
        if screen.contains("upgraded in") {
            assert!(screen.contains("checksum ok"), "{screen}");
            break;
        }
        assert!(Instant::now() < deadline, "the update never finished:\n{screen}");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(version_of(&b.installed()), format!("ptyro {V}"));
    let after: Value = serde_json::from_slice(
        &d.cmd().args(["daemon", "status", "--json"]).output().unwrap().stdout,
    )
    .unwrap();
    assert!(after["generation"].as_u64() > before["generation"].as_u64(), "{after}");
    let _ = serve.kill();
    let _ = serve.wait();
}

/// Homebrew's formula behind the release (or its auto-update off): `brew upgrade` changes
/// nothing, and that is said, never reported as an update.
#[cfg(unix)]
#[test]
fn homebrew_behind_the_release_is_said_not_claimed() {
    let mut b = Box_::new(json!({ "platform": "macos" }));
    let keg = b.path("brew/Cellar/ptyro/0.0.1/bin");
    std::fs::create_dir_all(&keg).unwrap();
    std::fs::create_dir_all(b.path("brew/bin")).unwrap();
    let mine = keg.join("ptyro");
    std::fs::write(&mine, script("0.0.1")).unwrap();
    executable(&mine);
    std::os::unix::fs::symlink(&mine, b.path("brew/bin/ptyro")).unwrap();
    b.publish(V);
    let tools = b.path("tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(
        tools.join("brew"),
        "#!/bin/sh\necho 'Warning: ptyro 0.0.1 already installed'\n",
    )
    .unwrap();
    executable(&tools.join("brew"));
    b.extra = vec![
        ("PTYRO_TEST_SELF_EXE".into(), mine.display().to_string()),
        ("PATH".into(), ptyro_testkit::program::path_with(&tools)),
    ];
    let o = b.run(&["update", "ptyro", "--yes"], "");
    let said = text(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains(&format!("Homebrew's formula isn't at {V} yet")), "{said}");
    assert!(said.contains("brew update && brew upgrade ptyro"), "{said}");
    assert!(!said.contains("upgraded in"), "no daemon upgrade onto the old version: {said}");
}
