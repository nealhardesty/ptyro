//! Setup on Windows: WSL, Windows, or both (ROADMAP BOOT8), end to end on any machine. Windows'
//! side is the fake machine `PTYRO_TEST_SETUP` names (its `world.json` says `windows-x64`); WSL
//! is `ptyro fake-wsl` standing in for `wsl.exe` (`PTYRO_WSL`), which runs this same ptyro as the
//! distro's, on a fake Linux machine of its own. Nothing real is installed, and neither real WSL
//! nor the network is used.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_agents::setup::Side;

use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

/// Windows with WinGet; WSL's distro (when there is one) a Linux with apt, sudo and curl.
struct Machine {
    dir: tempfile::TempDir,
    wsl: PathBuf,
}

impl Machine {
    fn new(windows: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot8-").tempdir().unwrap();
        for d in ["fake", "home", "ptyro", "wsl"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("fake/world.json"), windows.to_string()).unwrap();
        let wsl = ptyro_testkit::program::program(
            &dir.path().join("bin"),
            "wsl",
            BIN,
            &format!("exec \"{BIN}\" fake-wsl \"$@\""),
            &[BIN, "fake-wsl"],
            &[],
        );
        Self { dir, wsl }
    }

    /// Windows, with WSL's distro `Ubuntu` running ptyro on `linux` (merged into a fresh one).
    fn with_ubuntu(linux: Value) -> Self {
        let m = Self::new(windows());
        m.distro("Ubuntu", Some(linux));
        m
    }

    /// Add a distro; `None`: without ptyro in it.
    fn distro(&self, name: &str, linux: Option<Value>) {
        let w = self.dir.path().join("wsl");
        let list = std::fs::read_to_string(w.join("distros")).unwrap_or_default();
        std::fs::write(w.join("distros"), format!("{list}{name}\n")).unwrap();
        for d in ["fake", "home"] {
            std::fs::create_dir_all(w.join(name).join(d)).unwrap();
        }
        if let Some(linux) = linux {
            std::fs::create_dir_all(w.join(name).join("ptyro")).unwrap();
            let mut world = json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"] });
            world.as_object_mut().unwrap().extend(linux.as_object().unwrap().clone());
            std::fs::write(w.join(name).join("fake/world.json"), world.to_string()).unwrap();
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_TEST_SETUP", self.dir.path().join("fake"))
            .env("PTYRO_HOME", self.dir.path().join("ptyro"))
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_WSL", &self.wsl)
            .env("PTYRO_FAKE_WSL", self.dir.path().join("wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
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

    fn plan(&self, args: &[&str]) -> Value {
        let mut a = vec!["setup", "--dry-run", "--json"];
        a.extend(args);
        let o = self.run(&a, "");
        assert!(o.status.success(), "{}", text(&o));
        serde_json::from_slice(&o.stdout).unwrap()
    }

    /// The daemon is WSL's (`client.default_remote`), as Windows' first run keeps it.
    fn daemon_in(&self, distro: &str) {
        let c = self.dir.path().join("ptyro/config");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(
            c.join("config.toml"),
            format!("[client]\ndefault_remote = \"wsl://{distro}\"\n"),
        )
        .unwrap();
    }

    fn calls(&self, distro: Option<&str>) -> Vec<String> {
        let f = match distro {
            None => self.dir.path().join("fake/calls.log"),
            Some(d) => self.dir.path().join("wsl").join(d).join("fake/calls.log"),
        };
        std::fs::read_to_string(f).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn wsl_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("wsl/wsl.log")).unwrap_or_default()
    }
}

fn windows() -> Value {
    json!({ "platform": "windows-x64", "programs": ["winget"] })
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// `(side, id)` of each ticked entry with something to run.
fn runs(plan: &Value) -> Vec<(String, String)> {
    plan["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["ticked"] == true && !e["action"].is_null())
        .map(|e| (e["side"].as_str().unwrap().to_owned(), e["id"].as_str().unwrap().to_owned()))
        .collect()
}

fn entry<'a>(plan: &'a Value, side: &str, id: &str) -> &'a Value {
    plan["entries"].as_array().unwrap().iter().find(|e| e["side"] == side && e["id"] == id).unwrap()
}

fn pair(side: &str, id: &str) -> (String, String) {
    (side.to_owned(), id.to_owned())
}

#[test]
fn without_a_usable_wsl_only_windows_is_offered_and_the_plan_says_why() {
    // No wsl.exe at all.
    let m = Machine::new(windows());
    std::fs::remove_file(&m.wsl).unwrap();
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["state"], "unusable", "{p}");
    assert!(p["wsl"]["reason"].as_str().unwrap().contains("WSL isn't installed"), "{p}");
    assert!(p["wsl"]["fix"].as_str().unwrap().contains("wsl --install"), "{p}");
    assert!(p["entries"].as_array().unwrap().iter().all(|e| e["side"] == "windows"), "{p}");
    // WSL with no distro: `wsl -l -q` fails, as Windows 11's inbox stub does without WSL too.
    let m = Machine::new(windows());
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["reason"], "WSL isn't installed or has no distro", "{p}");
    // Only a tool's own distro: WSL works, with nothing to work in.
    let m = Machine::new(windows());
    m.distro("docker-desktop", Some(json!({})));
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["reason"], "WSL has no Linux distro", "{p}");
    // A distro without ptyro: the fix is the line that puts it there; setup never runs it.
    let m = Machine::new(windows());
    m.distro("docker-desktop", Some(json!({})));
    m.distro("Ubuntu", None);
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["reason"], "ptyro isn't installed in Ubuntu (WSL)", "{p}");
    let fix = p["wsl"]["fix"].as_str().unwrap();
    assert!(fix.contains("wsl -d Ubuntu -- sh -c \"curl -fsSL https://github.com/"), "{fix}");
    assert!(fix.contains("install.sh | sh\""), "{fix}");
    assert!(!m.wsl_log().contains("install.sh"), "never installed by setup");
    assert_eq!(
        runs(&p),
        [
            pair("windows", "claude"),
            pair("windows", "codex"),
            pair("windows", "git"),
            pair("windows", "gh")
        ]
    );
    // The checklist says so too, and asking for WSL is refused with the same fix.
    let o = m.run(&["setup", "--dry-run"], "");
    assert!(text(&o).contains("WSL: ptyro isn't installed in Ubuntu (WSL), so this sets up Windows only. For WSL too: put it there"), "{}", text(&o));
    let o = m.run(&["setup", "--yes", "--where", "wsl"], "");
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains(
            "--where wsl: ptyro isn't installed in Ubuntu (WSL): put it there: `wsl -d Ubuntu"
        ),
        "{}",
        text(&o)
    );
    assert!(m.calls(None).is_empty());
    // A ptyro from before setup: WSL isn't usable for it, and the fix updates it.
    let m = Machine::with_ubuntu(json!({}));
    std::fs::write(m.dir.path().join("wsl/Ubuntu/old"), "").unwrap();
    let p = m.plan(&[]);
    assert!(p["wsl"]["reason"].as_str().unwrap().contains("older than setup"), "{p}");
    assert!(p["wsl"]["fix"].as_str().unwrap().starts_with("update it: `wsl -d Ubuntu"), "{p}");
}

#[test]
fn both_sides_are_one_plan_and_the_default_is_where_the_daemon_runs() {
    let m = Machine::with_ubuntu(json!({ "installed": { "git": "2.43.0" } }));
    // No daemon of WSL's: Windows by default, WSL offered beside it.
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["state"], "usable", "{p}");
    assert_eq!(p["wsl"]["distro"], "Ubuntu");
    assert!(p["wsl"]["version"].as_str().unwrap().starts_with("ptyro "), "{p}");
    assert!(runs(&p).iter().all(|(s, _)| s == "windows"), "{p}");
    assert_eq!(entry(&p, "wsl", "git")["state"], "installed", "WSL's own survey");
    assert_eq!(entry(&p, "windows", "git")["state"], "missing");
    // The daemon in a distro setup can't reach: Windows by default, and it says why, though
    // another distro has ptyro.
    m.distro("Debian", None);
    m.daemon_in("Debian");
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["distro"], "Ubuntu", "{p}");
    assert!(runs(&p).iter().all(|(s, _)| s == "windows"), "{p}");
    let o = m.run(&["setup", "--dry-run"], "");
    assert!(
        text(&o).contains("your daemon is in Debian, which setup can't reach; Ubuntu has ptyro"),
        "{}",
        text(&o)
    );
    // WSL's daemon: WSL by default.
    m.daemon_in("Ubuntu");
    let p = m.plan(&[]);
    assert_eq!(
        runs(&p),
        [pair("wsl", "claude"), pair("wsl", "codex"), pair("wsl", "gh")],
        "git is there"
    );
    // Both, by name: each side's own methods.
    let p = m.plan(&["--where", "both", "--agents", "claude", "--tools", "gh"]);
    assert_eq!(
        runs(&p),
        [
            pair("windows", "claude"),
            pair("windows", "gh"),
            pair("wsl", "claude"),
            pair("wsl", "gh")
        ]
    );
    assert_eq!(entry(&p, "windows", "gh")["action"]["via"], "winget");
    assert_eq!(entry(&p, "wsl", "gh")["action"]["via"], "apt");
    // The text checklist has a column each.
    let o = m.run(
        &["setup", "--dry-run", "--where", "both", "--agents", "claude", "--tools", "none"],
        "",
    );
    let t = text(&o);
    assert!(t.contains("Agents") && t.contains("Windows") && t.contains("WSL (Ubuntu)"), "{t}");
    assert!(
        t.contains("   1 Claude Code   [x] the vendor's installer  [x] the vendor's installer"),
        "{t}"
    );
    assert!(t.contains("Claude Code (WSL):"), "{t}");
    // Only Windows: WSL isn't even asked.
    let before = m.wsl_log().lines().count();
    let p = m.plan(&["--where", "windows"]);
    assert!(p.get("wsl").is_none(), "{p}");
    assert_eq!(m.wsl_log().lines().count(), before);
}

#[test]
fn both_sides_install_with_each_event_saying_its_side() {
    let m = Machine::with_ubuntu(json!({ "outcomes": { "gh": "fail" } }));
    let o = m.run(
        &["setup", "--yes", "--json", "--where", "both", "--agents", "claude", "--tools", "gh"],
        "",
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "WSL's gh failed: {t}");
    assert_eq!(
        m.calls(None),
        ["install claude via vendor admin=false", "install gh via winget admin=true"]
    );
    assert_eq!(
        m.calls(Some("Ubuntu")),
        ["install claude via vendor admin=false", "install gh via apt admin=true"]
    );
    let events: Vec<Value> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let done: Vec<(String, String)> = events
        .iter()
        .filter(|e| e["event"] == "done")
        .map(|e| (e["side"].as_str().unwrap().into(), e["id"].as_str().unwrap().into()))
        .collect();
    assert_eq!(done, [pair("windows", "claude"), pair("windows", "gh"), pair("wsl", "claude")]);
    let failed = events.iter().find(|e| e["event"] == "failed").unwrap();
    assert_eq!((failed["side"].as_str(), failed["id"].as_str()), (Some("wsl"), Some("gh")));
    assert!(failed["fix"].as_str().unwrap().contains("ptyro install gh"), "{failed}");
    let finished: Vec<&Value> = events.iter().filter(|e| e["event"] == "finished").collect();
    assert_eq!(finished.len(), 2, "one report a side");
    // WSL's half ran through wsl.exe as the distro's own `ptyro install`, asked nothing.
    assert!(
        m.wsl_log().contains("ptyro install --json --admin=silent claude gh"),
        "{}",
        m.wsl_log()
    );
    // In words: a heading a side.
    let m = Machine::with_ubuntu(json!({}));
    let o = m.run(&["install", "claude", "--yes", "--where", "both"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("── Windows ──") && t.contains("── WSL (Ubuntu) ──"), "{t}");
    assert!(t.contains("✓ Claude Code 1.0.0"), "{t}");
}

#[test]
fn admin_in_wsl_is_asked_for_in_the_terminal_through_wsl_exe() {
    let m = Machine::with_ubuntu(json!({ "elevation": "prompt" }));
    m.daemon_in("Ubuntu");
    let o = m.run(&["install", "gh"], "y\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("  GitHub CLI (WSL): install through apt (as administrator)"), "{t}");
    assert_eq!(m.calls(Some("Ubuntu")), ["elevate", "install gh via apt admin=true"]);
    assert!(t.contains("sudo asks for your password once, here"), "{t}");
    assert!(m.wsl_log().contains("ptyro install --json --admin=ask gh"), "{}", m.wsl_log());
    // --yes never prompts: the step is reported with its command.
    let m = Machine::with_ubuntu(json!({ "elevation": "prompt" }));
    let o = m.run(&["install", "gh", "--yes", "--where", "wsl"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("would ask for a password (--yes asks nothing)"), "{}", text(&o));
    assert_eq!(m.calls(Some("Ubuntu")), Vec::<String>::new());
    // And WSL's ptyro stays quiet: what's installed is Windows' to say.
    assert!(m.wsl_log().contains("ptyro install --json --admin=silent gh"), "{}", m.wsl_log());
    assert!(
        !text(&o).contains("Looking at what's installed…\nLooking at what's installed…"),
        "{}",
        text(&o)
    );
    assert_eq!(text(&o).matches("Looking at what's installed").count(), 1, "{}", text(&o));
}

#[test]
fn the_checklist_ticks_each_side() {
    let m = Machine::with_ubuntu(json!({}));
    // Defaults on Windows; Claude Code (1) moved to WSL, gh (7: the tools follow the five
    // agents) to both; then install.
    let o = m.run(&["setup"], "1:wsl 7:both\n\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("A number ticks it on Windows; 3:wsl, 3:windows or 3:both puts item 3 there."),
        "{t}"
    );
    assert_eq!(
        m.calls(None),
        [
            "install codex via vendor admin=false",
            "install git via winget admin=true",
            "install gh via winget admin=true"
        ],
        "{t}"
    );
    assert_eq!(
        m.calls(Some("Ubuntu")),
        ["install claude via vendor admin=false", "install gh via apt admin=true"]
    );
}

#[test]
fn elsewhere_sides_dont_apply() {
    let m = Machine::new(json!({ "platform": "linux", "programs": ["apt-get", "sudo", "curl"] }));
    let o = m.run(&["setup", "--dry-run", "--where", "both"], "");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("--where both is for Windows"), "{}", text(&o));
    let o = m.run(&["install", "gh", "--dry-run", "--where", "local"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(m.wsl_log().is_empty(), "WSL is never asked");
    let m = Machine::new(json!({ "platform": "wsl", "programs": ["apt-get", "sudo", "curl"] }));
    let o = m.run(&["setup", "--dry-run", "--where", "windows"], "");
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("this is WSL"), "{}", text(&o));
    let p = m.plan(&[]);
    assert_eq!(p["wsl"]["state"], "inside", "{p}");
}

/// This machine as it is, with no platform faked: on Windows (CI's windows job) the sides are
/// real; elsewhere `--where wsl` is refused.
#[test]
fn this_machine_gets_sides_only_on_windows() {
    let m = Machine::with_ubuntu(json!({}));
    std::fs::write(
        m.dir.path().join("fake/world.json"),
        json!({ "programs": ["winget"] }).to_string(),
    )
    .unwrap();
    let o = m.run(
        &[
            "setup",
            "--dry-run",
            "--json",
            "--where",
            "wsl",
            "--agents",
            "claude",
            "--tools",
            "none",
        ],
        "",
    );
    if cfg!(windows) {
        assert!(o.status.success(), "{}", text(&o));
        let p: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(p["wsl"]["distro"], "Ubuntu", "{p}");
        assert_eq!(runs(&p), [pair("wsl", "claude")]);
        assert!(p["entries"].as_array().unwrap().iter().any(|e| e["side"] == "windows"));
    } else {
        assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    }
}

/// A daemon on Windows (for the web) plans and installs both sides; WSL's admin steps show
/// their command, as Windows' do.
#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_on_windows_does_both_sides_without_admin() {
    let m = Machine::with_ubuntu(json!({}));
    let fake = m.dir.path().join("fake").display().to_string();
    let fake_wsl = m.dir.path().join("wsl").display().to_string();
    let wsl = m.wsl.display().to_string();
    let env = [
        ("PTYRO_TEST_SETUP", fake.as_str()),
        ("PTYRO_WSL", wsl.as_str()),
        ("PTYRO_FAKE_WSL", fake_wsl.as_str()),
    ];
    let d = TempDaemon::start_with_env(BIN, &env).unwrap();
    let mut c = d.client().await.unwrap();
    let both = Some(vec!["claude".to_owned(), "gh".to_owned()]);
    let req = Request::SetupPlan { picks: Some(vec!["claude".into()]), wsl_picks: both.clone() };
    let Reply::SetupPlan { plan, .. } = c.request(req).await.unwrap() else { panic!("not a plan") };
    let p = serde_json::to_value(&plan).unwrap();
    assert_eq!(p["wsl"]["state"], "usable", "{p}");
    assert_eq!(runs(&p), [pair("windows", "claude"), pair("wsl", "claude"), pair("wsl", "gh")]);
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let req = Request::SetupRun { picks: vec!["claude".into()], wsl_picks: both };
    let Reply::SetupStarted { run, .. } = c.request(req).await.unwrap() else {
        panic!("not started")
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen: Vec<(Side, Value)> = Vec::new();
    while seen.iter().filter(|(_, e)| e["event"] == "finished").count() < 2 {
        assert!(Instant::now() < deadline, "no reports: {seen:?}");
        let Ok(Some(ServerMsg::Event {
            event: DomainEvent::SetupProgress { run: r, event, side, .. },
            ..
        })) = tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
        else {
            continue;
        };
        assert_eq!(r, run);
        seen.push((side, serde_json::to_value(&event).unwrap()));
    }
    let (side, skipped) = seen.iter().find(|(_, e)| e["event"] == "skipped").unwrap();
    assert_eq!((*side, skipped["id"].as_str()), (Side::Wsl, Some("gh")));
    assert!(skipped["fix"].as_str().unwrap().contains("on the computer itself"), "{skipped}");
    assert!(seen.iter().any(|(s, e)| *s == Side::Windows && e["event"] == "done"));
    assert!(
        m.wsl_log().contains("ptyro install --json --admin=never claude gh"),
        "{}",
        m.wsl_log()
    );
    assert_eq!(m.calls(Some("Ubuntu")), ["install claude via vendor admin=false"]);
}
