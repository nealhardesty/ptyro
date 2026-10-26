//! UX2 end-to-end: `ptyro about` says the same version as `--version`, and a worktree's status
//! follows its work to done: uncommitted, committed and not pushed, pushed (a quiet `✓ pushed`),
//! and an unpushed commit on top of an upstream. Real daemon, real git, a bare "remote".

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::Script;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-home-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Test User\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn repo(&self, name: &str) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("README"), "hi\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.git(&root, &["remote", "set-head", "origin", "main"]);
        root
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `wt ls --json` for one worktree.
    fn wt(&self, name: &str) -> Value {
        let list: Value = serde_json::from_str(&self.ok(&["wt", "ls", "--json"])).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .find(|w| w["name"] == name)
            .unwrap_or_else(|| panic!("no worktree {name} in {list}"))
            .clone()
    }

    /// Wait until `wt ls` says `want` of the worktree's status, and return the plain listing.
    fn until(&self, name: &str, what: &str, want: impl Fn(&Value) -> bool) -> Value {
        let t = Instant::now();
        loop {
            let w = self.wt(name);
            if want(&w["status"]) {
                return w;
            }
            assert!(t.elapsed() < Duration::from_secs(8), "{what}: still {}", w["status"]);
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The TUI, inside the headless emulator.
    fn tui(&self, args: &[&str]) -> Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = Script::new(argv)
            .size(120, 40)
            .timeout(14.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }

    fn plain(&self, name: &str) -> String {
        self.ok(&["wt", "ls"]).lines().find(|l| l.contains(name)).unwrap_or_default().to_owned()
    }
}

#[test]
fn a_worktree_follows_its_work_to_pushed() {
    let w = World::new("[worktrees]\nfetch = false\nwatch = true\n");
    let root = w.repo("lib");
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["wt", "new", "feat", "--project", "lib"]);
    let path = PathBuf::from(w.wt("feat")["path"].as_str().unwrap());

    // Nothing of its own yet: quiet.
    let st = w.wt("feat")["status"].clone();
    assert!(st["pushed"].is_null() && st["unpushed"] == 0, "{st}");
    assert!(!w.plain("lib/feat").contains("pushed"), "{}", w.plain("lib/feat"));

    // An uncommitted file: `● 1`, with the line counts.
    std::fs::write(path.join("a.txt"), "one\ntwo\n").unwrap();
    w.until("feat", "uncommitted", |s| s["untracked"] == 1);
    let line = w.plain("lib/feat");
    assert!(line.contains("● 1") && !line.contains("pushed"), "{line}");

    // Committed, no upstream yet: not pushed.
    w.git(&path, &["add", "a.txt"]);
    w.git(&path, &["commit", "-q", "-m", "one"]);
    let s = w.until("feat", "not pushed", |s| s["unpushed"] == 1)["status"].clone();
    assert!(s["upstream"].is_null() && s["pushed"].is_null(), "{s}");
    let line = w.plain("lib/feat");
    assert!(line.contains("not pushed ↑1") && line.contains("+2 -0"), "{line}");

    // Pushed, the way an agent does it: the row turns to done without anyone asking.
    w.git(&path, &["push", "-q", "-u", "origin", "HEAD"]);
    let s = w.until("feat", "pushed", |s| s["pushed"] == true)["status"].clone();
    assert_eq!(s["ahead"], 0, "{s}");
    let line = w.plain("lib/feat");
    assert!(line.contains("✓ pushed") && !line.contains("+2"), "the line counts go quiet: {line}");
    let info = w.ok(&["wt", "status", "lib/feat"]);
    assert!(info.contains("to do") && info.contains("✓ pushed"), "{info}");

    // Another commit on top of the upstream: ahead of it.
    std::fs::write(path.join("b.txt"), "x\n").unwrap();
    w.git(&path, &["add", "b.txt"]);
    w.git(&path, &["commit", "-q", "-m", "two"]);
    let s = w.until("feat", "ahead", |s| s["ahead"] == 1)["status"].clone();
    assert!(s["pushed"].is_null(), "{s}");
    assert!(w.plain("lib/feat").contains("↑1"), "{}", w.plain("lib/feat"));
}

#[test]
fn about_is_one_document_and_agrees_with_version() {
    let w = World::new("[worktrees]\nfetch = false\n");
    let version = w.ok(&["--version"]);
    let version = version.trim().trim_start_matches("ptyro ");
    let doc: Value = serde_json::from_str(&w.ok(&["about", "--json"])).unwrap();
    assert_eq!(doc["version"], version);
    assert_eq!(doc["repository"], "https://github.com/nealhardesty/ptyro");
    assert_eq!(doc["issues"], "https://github.com/nealhardesty/ptyro/issues");
    assert_eq!(doc["license"], "MIT");
    assert!(doc["protocol"].as_str().unwrap().contains('.'));
    assert!(doc["build"]["target"].as_str().is_some_and(|t| !t.is_empty()), "{doc}");
    let names: Vec<&str> =
        doc["places"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!(names, ["config", "data", "logs", "socket"]);
    // The test's daemon runs: its version is this one, so no fix is offered.
    assert_eq!(doc["daemon"]["running"], true, "{doc}");
    assert_eq!(doc["daemon"]["same"], true);
    assert!(doc["daemon"]["fix"].is_null());
    assert!(doc["tools"].as_array().unwrap().iter().any(|t| t["name"] == "git"), "{doc}");
    // The text is what is printed, and it is what to paste (the daemon's uptime moves on, and an
    // agent's `--version` can time out in one run and not the next: only the fixed facts).
    let text = w.ok(&["about"]);
    fn steady(t: &str) -> Vec<&str> {
        let fixed = [
            "ptyro ", "build ", "binary ", "system ", "source ", "licence ", "report ", "config ",
            "data ", "logs ", "socket ",
        ];
        t.lines().filter(|l| fixed.iter().any(|f| l.starts_with(f))).collect()
    }
    assert_eq!(steady(&text), steady(doc["report"].as_str().unwrap()));
    for want in
        [version, "https://github.com/nealhardesty/ptyro/issues", "running, version", "config "]
    {
        assert!(text.contains(want), "{want} in\n{text}");
    }
}

#[tokio::test]
async fn the_tui_shows_a_pushed_worktree_as_done_and_opens_about() {
    let w = World::new("[worktrees]\nfetch = false\nwatch = true\n");
    let root = w.repo("lib");
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["wt", "new", "feat", "--project", "lib"]);
    w.ok(&["new", "-d", "--wt", "lib/feat", "-n", "worker", "--", "sh", "-c", "sleep 60"]);
    let path = PathBuf::from(w.wt("feat")["path"].as_str().unwrap());
    std::fs::write(path.join("a.txt"), "one\n").unwrap();
    w.git(&path, &["add", "a.txt"]);
    w.git(&path, &["commit", "-q", "-m", "one"]);
    w.git(&path, &["push", "-q", "-u", "origin", "HEAD"]);
    w.until("feat", "pushed", |s| s["pushed"] == true);
    let r = ptyro_testkit::headless::run(
        w.tui(&["attach", "worker"])
            .snapshot(1.5)
            .send(1.8, "\x11I")
            .snapshot(1.9)
            .until("https://github.com/nealhardesty/ptyro/issues")
            .snapshot(9.0)
            .send(9.2, "\x1b")
            .snapshot(9.8)
            .send(10.0, "\x11d"),
    )
    .await
    .expect("headless run");
    let sidebar = r.at(1.5);
    assert!(
        sidebar.lines().any(|l| l.contains("feat") && l.contains("✓ pushed") && !l.contains("+1")),
        "{sidebar}"
    );
    let about = r.at(9.0);
    for want in [
        "About ptyro",
        concat!(env!("CARGO_PKG_VERSION"), " (protocol"),
        "https://github.com/nealhardesty/ptyro",
        "where things live",
        "j/k pick",
    ] {
        assert!(about.contains(want), "{want:?} in\n{about}");
    }
    assert!(!r.at(9.8).contains("About ptyro"), "Esc closes it");
}
