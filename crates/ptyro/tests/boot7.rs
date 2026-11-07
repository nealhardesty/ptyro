//! Moves to the recommended installer (ROADMAP BOOT7, SPEC §24.9), end to end on BOOT1's fake
//! machine (`PTYRO_TEST_SETUP`, `ptyro fake-installer`): an agent installed another way (Codex
//! through npm) is offered the move; the move installs the recommended way, checks the new copy
//! runs, and only then removes the old one. A failed install leaves the old one; a failed removal
//! says the command that finishes it; an old copy that still comes first is said with its fix;
//! and a dismissed offer stays dismissed, the agent updating the way it's installed. Nothing
//! reaches the network or installs anything real.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ptyro_agents::setup::{Plan, State};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    /// Linux with Codex 0.162.1 installed through npm, logged in already; a move's new copy
    /// installs as 0.170.0.
    fn new(extra: Value) -> Self {
        let dir = tempfile::Builder::new().prefix("ptyro-boot7-").tempdir().unwrap();
        for d in ["fake", "home", "ptyro", "registry"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        let mut world = json!({
            "platform": "linux",
            "programs": ["curl", "npm"],
            "installed": { "codex": "0.162.1" },
            "other": { "codex": "npm" },
            "versions": { "codex": "0.170.0" },
            "logged_in": ["codex"],
        });
        for (k, v) in extra.as_object().unwrap() {
            world[k] = v.clone();
        }
        std::fs::write(dir.path().join("fake/world.json"), world.to_string()).unwrap();
        Self { dir }
    }

    fn fake(&self) -> PathBuf {
        self.dir.path().join("fake")
    }

    fn paths(&self) -> ptyro_core::paths::Paths {
        ptyro_testkit::paths_for(&self.dir.path().join("ptyro")).unwrap()
    }

    /// What removes npm's Codex here: npm's uninstall in the prefix it is in.
    fn rm(&self) -> String {
        format!(
            "npm uninstall -g --prefix '{}' @openai/codex",
            self.fake().join("npm-prefix").display()
        )
    }

    fn publish_codex(&self, version: &str) {
        let body = json!({ "tag_name": format!("rust-v{version}") }).to_string();
        std::fs::write(self.dir.path().join("registry/codex"), body).unwrap();
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("PTYRO_TEST_SETUP", self.fake().display().to_string()),
            ("PTYRO_TEST_LATEST", self.dir.path().join("registry").display().to_string()),
        ]
    }

    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("PTYRO_HOME", self.dir.path().join("ptyro"))
            .env("HOME", self.dir.path().join("home"))
            .env("USERPROFILE", self.dir.path().join("home"))
            .env("SHELL", "/bin/bash")
            .env("PTYRO_WSL", self.dir.path().join("no-wsl"))
            .env("PTYRO_NO_AUTOSPAWN", "1");
        for (k, v) in self.env() {
            c.env(k, v);
        }
        for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE"] {
            c.env_remove(v);
        }
        let mut child =
            c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn calls(&self) -> Vec<String> {
        lines(&self.fake().join("calls.log"))
    }

    /// `ptyro install codex --dry-run --json`: the plan, as it stands.
    fn plan(&self) -> Plan {
        let o = self.run(&["install", "codex", "--dry-run", "--json"], "");
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)))
    }

    /// `doctor --quick --json`'s `updates` checks.
    fn doctor(&self) -> Vec<Value> {
        let o = self.run(&["doctor", "--quick", "--json"], "");
        let all: Vec<Value> =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        all.into_iter().filter(|c| c["section"] == "updates").collect()
    }
}

fn lines(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file).unwrap_or_default().lines().map(str::to_owned).collect()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const INSTALL: &str = "install codex via vendor admin=false";
const REMOVE: &str = "uninstall codex via npm admin=false";

#[test]
fn a_move_installs_the_recommended_way_checks_it_then_removes_the_old() {
    let m = Machine::new(json!({}));
    // Offered: a row of the checklist with the move, not "already here".
    let plan = m.plan();
    let e = plan.entry("codex").unwrap();
    let mv = e.moves.as_ref().expect("the move is offered");
    assert_eq!(mv.uninstall, m.rm());
    assert!(e.action.as_ref().is_some_and(|a| a.moving));
    let o = m.run(&["setup", "--dry-run"], "");
    let t = text(&o);
    assert!(
        t.contains(
            "   2 [ ] Codex  (installed 0.162.1 through npm; move to the recommended installer?)"
        ),
        "{t}"
    );
    // `ptyro install codex` says what the move does, and asks.
    let o = m.run(&["install", "codex"], "y\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains(&format!("Codex: move from npm to the vendor's installer ((fake npm) codex): install it through the vendor's installer, check it runs, then `{}`; your settings and logins (~/.codex) stay", m.rm())),
        "{t}"
    );
    assert!(
        t.contains("Install? [Y/n, or k to keep Codex with npm and not be asked again]"),
        "{t}"
    );
    assert_eq!(m.calls(), [INSTALL, REMOVE], "installed first, then the old removed");
    assert!(t.contains("▸ Codex: installing it through the vendor's installer\n  $ curl -fsSL https://chatgpt.com/codex/install.sh | sh"), "{t}");
    assert!(t.contains(&format!("▸ Codex: removing npm's copy\n  $ {}", m.rm())), "{t}");
    // Against the prefix the old copy is in, not npm's of the moment.
    assert_eq!(lines(&m.fake().join("uninstall.log")), [m.rm()]);
    assert!(t.contains("✓ Codex 0.170.0"), "{t}");
    // Its login is where it was: still logged in.
    assert!(t.contains("Codex") && t.contains("logged in"), "{t}");
    // Now the recommended one, offered nothing.
    let plan = m.plan();
    let e = plan.entry("codex").unwrap();
    assert!(
        matches!(&e.state, State::Installed { version: Some(v), .. } if v == "0.170.0"),
        "{:?}",
        e.state
    );
    assert!(e.moves.is_none());
    assert!(!m.doctor().iter().any(|c| c["name"] == "codex"));
}

#[test]
fn a_new_install_that_fails_leaves_the_old_one_working() {
    let m = Machine::new(json!({ "outcomes": { "codex": "fail" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert_eq!(m.calls(), [INSTALL], "nothing removed");
    assert!(t.contains("✗ Codex: the install through the vendor's installer failed"), "{t}");
    assert!(
        t.contains("fix: nothing was removed: Codex through npm is still installed and works"),
        "{t}"
    );
    let plan = m.plan();
    let e = plan.entry("codex").unwrap();
    assert!(
        matches!(&e.state, State::Other { version: Some(v), .. } if v == "0.162.1"),
        "the old one, as it was: {:?}",
        e.state
    );
    assert!(e.moves.is_some(), "still offered");
}

#[test]
fn a_removal_that_fails_says_how_to_finish_and_trying_again_only_removes() {
    let m = Machine::new(json!({ "uninstalls": { "codex": "fail" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert_eq!(m.calls(), [INSTALL, REMOVE]);
    assert!(
        t.contains("✗ Codex: Codex 0.170.0 through the vendor's installer works ((fake) codex), but removing npm's copy ((fake npm) codex) failed"),
        "{t}"
    );
    assert!(
        t.contains(&format!("fix: finish the move by removing it yourself: `{}`", m.rm())),
        "{t}"
    );
    // Once it can be removed: the new copy is here already, so only the removal runs.
    let mut world: Value =
        serde_json::from_str(&std::fs::read_to_string(m.fake().join("world.json")).unwrap())
            .unwrap();
    world["uninstalls"] = json!({});
    std::fs::write(m.fake().join("world.json"), world.to_string()).unwrap();
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(m.calls(), [INSTALL, REMOVE, REMOVE]);
    assert!(t.contains("Codex through the vendor's installer is here already ((fake) codex): only npm's copy is left to remove"), "{t}");
}

#[test]
fn a_removal_that_leaves_the_old_copy_says_so_with_the_exact_fix() {
    let m = Machine::new(json!({ "uninstalls": { "codex": "leftover" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains(&format!("but npm's copy ((fake npm) codex) is still there after `{}`", m.rm())),
        "{t}"
    );
    // Never "run it again": the same removal would do the same.
    assert!(t.contains("fix: remove it yourself: delete (fake npm) codex"), "{t}");
    assert!(!t.contains("fix: `ptyro install codex`"), "{t}");
}

#[test]
fn another_old_copy_still_first_on_path_is_said_with_its_fix() {
    let m = Machine::new(json!({ "second": ["codex"] }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains(
            "but npm's codex ((fake second) codex) comes first on PATH, so it is the one that runs"
        ),
        "{t}"
    );
    assert!(t.contains("fix: delete (fake second) codex (or uninstall it with the tool that put it there), or put (fake) codex's folder ahead of it on PATH"), "{t}");
}

#[test]
fn a_removal_that_takes_the_new_copy_puts_it_back_or_says_none_runs() {
    let m = Machine::new(json!({ "uninstalls": { "codex": "both" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert_eq!(m.calls(), [INSTALL, REMOVE, INSTALL], "put back at once");
    assert!(t.contains("Codex: the removal took the new copy too, installing it again"), "{t}");
    assert!(t.contains("✓ Codex 0.170.0"), "{t}");
    // Putting it back fails: said plainly, never Done.
    let m =
        Machine::new(json!({ "uninstalls": { "codex": "both" }, "outcomes": { "codex": "once" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("no Codex runs now"), "{t}");
    assert!(t.contains("fix: install it again: `ptyro install codex`"), "{t}");
    assert!(!t.contains("✓ Codex"), "{t}");
}

#[test]
fn a_third_installers_copy_isnt_the_new_one_and_unattended_never_moves() {
    // Homebrew's codex beside npm's: the vendor's still installs before npm's goes.
    let m = Machine::new(json!({ "third": { "codex": "brew" } }));
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.calls(), [INSTALL, REMOVE]);
    // `--yes` alone: offered, never done unasked; left as it is.
    let m = Machine::new(json!({}));
    let o = m.run(&["install", "codex", "--yes"], "");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(m.calls().is_empty(), "{:?}", m.calls());
    assert!(t.contains("Codex is installed with npm: moving it to the recommended installer needs a yes (`ptyro install codex` in a terminal, or --move); left as it is"), "{t}");
    let o = m.run(&["setup", "--yes", "--agents", "codex", "--tools", "none"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert!(m.calls().is_empty(), "{:?}", m.calls());
    // Bun's global folder isn't npm's to remove from: no move offered.
    let m = Machine::new(json!({ "no_prefix": ["codex"] }));
    assert!(m.plan().entry("codex").unwrap().moves.is_none());
}

#[test]
fn a_dismissed_offer_stays_dismissed_and_it_updates_the_way_its_installed() {
    let m = Machine::new(json!({}));
    m.publish_codex("0.170.0");
    // Offered beside its update, in doctor (and so the badge and the web's Health page).
    let o = m.run(&["doctor", "--json"], "");
    let checks: Vec<Value> = serde_json::from_slice(&o.stdout).unwrap();
    let codex = checks.iter().find(|c| c["section"] == "updates" && c["name"] == "codex").unwrap();
    assert_eq!(codex["data"]["move_to"], "vendor");
    assert!(
        codex["text"]
            .as_str()
            .unwrap()
            .contains("; or `ptyro install codex` moves it from npm to the vendor's installer"),
        "{codex}"
    );
    // `k`: kept, written to the config, nothing installed.
    let o = m.run(&["install", "codex"], "k\n");
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("Kept Codex the way it's installed"), "{t}");
    let cfg = std::fs::read_to_string(m.paths().config.join("config.toml")).unwrap();
    assert!(cfg.contains("keep_installer = [\"codex\"]"), "{cfg}");
    assert!(m.calls().is_empty(), "nothing ran: {:?}", m.calls());
    // Not offered again anywhere: the plan, doctor's finding.
    let plan = m.plan();
    let e = plan.entry("codex").unwrap();
    assert!(e.moves.is_none());
    let checks = m.doctor();
    let codex = checks.iter().find(|c| c["name"] == "codex").unwrap();
    assert!(codex["data"].get("move_to").is_none(), "{codex}");
    assert!(!codex["text"].as_str().unwrap().contains("moves it"), "{codex}");
    // Ticked again, it updates with npm.
    let o = m.run(&["install", "codex", "--yes", "--move"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.calls(), ["update codex via npm admin=false"]);
}

#[test]
fn up_to_date_its_a_note_and_update_never_moves() {
    let m = Machine::new(json!({}));
    m.publish_codex("0.162.1");
    let o = m.run(&["doctor", "--json"], "");
    let checks: Vec<Value> = serde_json::from_slice(&o.stdout).unwrap();
    let codex = checks.iter().find(|c| c["section"] == "updates" && c["name"] == "codex").unwrap();
    assert_eq!(codex["level"], "note", "a normal state: never a warning");
    assert_eq!(codex["data"]["move"]["uninstall"], m.rm());
    assert!(
        codex["text"].as_str().unwrap().starts_with(
            "Codex is installed with npm: `ptyro install codex` moves it to the vendor's installer"
        ),
        "{codex}"
    );
    // `ptyro update` updates the way it's installed, never moves.
    m.publish_codex("0.170.0");
    let o = m.run(&["update", "--yes"], "");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(m.calls(), ["update codex via npm admin=false"]);
}

/// The web's checklist and Health page, through the daemon: the move offered and run; an update
/// asked to keep the installer runs npm's own; a move whose old copy needs an administrator is
/// left for the computer, with its command.
#[tokio::test]
async fn the_web_moves_through_the_daemon_and_keeps_admin_moves_for_the_computer() {
    let m = Machine::new(
        json!({ "installed": { "codex": "0.162.1", "claude": "2.1.300" }, "other": { "codex": "npm", "claude": "npm" }, "system": ["claude"] }),
    );
    m.publish_codex("0.170.0");
    let mut env = m.env();
    env.push(("PTYRO_TEST_UPDATES_FIRST_MS", "0".into()));
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let d = TempDaemon::start_with_env(BIN, &env).unwrap();
    // Its first look finds Codex 0.170.0 out (so the Health page's update has one to run).
    let versions = d.paths.state.join("versions.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !std::fs::read_to_string(&versions).is_ok_and(|t| t.contains("0.170.0")) {
        assert!(Instant::now() < deadline, "the daemon's first look");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut c = d.client().await.unwrap();
    let Reply::SetupPlan { plan, lines, .. } =
        c.request(Request::SetupPlan { picks: Some(vec![]), wsl_picks: None }).await.unwrap()
    else {
        panic!("not a plan");
    };
    assert!(plan.entry("codex").unwrap().moves.is_some());
    let claude = plan.entry("claude").unwrap().moves.clone().unwrap();
    let prefix = m.fake().join("npm-prefix");
    assert_eq!(
        claude.uninstall,
        format!("sudo npm uninstall -g --prefix '{}' @anthropic-ai/claude-code", prefix.display())
    );
    assert!(claude.admin);
    let about = &lines.iter().find(|l| l.id == "codex").unwrap().about;
    assert!(about.contains("installed with npm · move to the recommended installer?"), "{about}");
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let run = |picks: &[&str], moves: &[&str]| Request::SetupRun {
        picks: picks.iter().map(|s| (*s).to_owned()).collect(),
        wsl_picks: None,
        moves: moves.iter().map(|s| (*s).to_owned()).collect(),
    };
    // The Health page's update (or any client that names no move, an older one too): npm's
    // own update, never a move.
    let report = finish(&mut c, &mut pushes, run(&["codex"], &[]), &m.fake()).await;
    assert_eq!(report.done, ["codex"]);
    assert_eq!(m.calls(), ["update codex via npm admin=false"]);
    // The checklist's tick: the move, and Claude Code's left for the computer.
    let report =
        finish(&mut c, &mut pushes, run(&["codex", "claude"], &["codex", "claude"]), &m.fake())
            .await;
    assert_eq!(report.done, ["codex"]);
    assert_eq!(report.skipped, ["claude"], "an administrator's move never runs from the web");
    assert_eq!(&m.calls()[1..], [INSTALL, REMOVE]);
}

async fn finish(
    c: &mut ptyro_client::Client,
    pushes: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    req: Request,
    fake: &Path,
) -> ptyro_agents::setup::Report {
    let Reply::SetupStarted { run, .. } = c.request(req).await.unwrap() else {
        panic!("not started");
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "no report: {:?}", lines(&fake.join("calls.log")));
        let got = tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await;
        let Ok(Some(ServerMsg::Event { event: DomainEvent::SetupProgress { run: r, event, .. } })) =
            got
        else {
            continue;
        };
        if r != run {
            continue;
        }
        if let ptyro_agents::setup::Event::Finished { report } = event {
            // Let the run let go of its place before the next.
            for _ in 0..100 {
                if let Ok(Reply::SetupPlan { running: None, .. }) =
                    c.request(Request::SetupPlan { picks: Some(vec![]), wsl_picks: None }).await
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            return report;
        }
    }
}
