//! M6 end-to-end tests that outlived review, shipping and pair mode (removed 2026-11-17):
//! starting work from a GitHub issue or pull request through a fake `gh`.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(20);

struct World {
    home: tempfile::TempDir,
    gh: PathBuf,
    d: TempDaemon,
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m6-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        // A fake `gh` on the daemon's PATH.
        let bin = h.join("fakebin");
        ptyro_testkit::program::program(&bin, "gh", &format!("exec \"{BIN}\" fake-gh \"$@\""));
        let gh = h.join("gh-state");
        std::fs::create_dir_all(&gh).unwrap();
        let path = ptyro_testkit::program::path_with(&bin);
        let (hs, gs) = (h.to_string_lossy().into_owned(), gh.to_string_lossy().into_owned());
        let env = [
            ("HOME", hs.as_str()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("SHELL", "/bin/sh"),
            ("PATH", path.as_str()),
            ("PTYRO_FAKE_GH", gs.as_str()),
        ];
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n[agents.claude]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"claude\"]\n"
        );
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, gh, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d
            .cmd()
            .args(args)
            .env("HOME", self.home())
            .env("PTYRO_FAKE_GH", &self.gh)
            .current_dir(self.home())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(out.status.success(), "ptyro {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A project cloned from a bare "remote", registered, with one pushed commit.
    fn project(&self, name: &str) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("f.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.ok(&["project", "add", root.to_str().unwrap()]);
        root
    }

    fn gh_file(&self, f: &str, v: &Value) {
        std::fs::write(self.gh.join(f), v.to_string()).unwrap();
    }

    fn screen(&self, s: &str) -> String {
        self.ok(&["read", s, "--scrollback", "200"])
    }

    fn wait_screen(&self, s: &str, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let t = self.screen(s);
            if t.contains(needle) {
                return t;
            }
            assert!(Instant::now() < deadline, "{s} never showed {needle:?}:\n{t}");
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

#[test]
fn worktrees_from_issues_and_prs() {
    let w = World::new();
    w.project("iss");
    w.gh_file("issue.json", &json!({"number": 42, "title": "Crash on empty input", "body": "Steps: run with ''", "url": "https://github.com/acme/app/issues/42"}));
    w.ok(&[
        "new",
        "-d",
        "--issue",
        "https://github.com/acme/app/issues/42",
        "--project",
        "iss",
        "--agent",
        "claude",
    ]);
    let wt = w.json(&["wt", "status", "iss/issue-42", "--json"]);
    assert_eq!(wt["branch"], "issue-42-crash-on-empty-input");
    let ls = w.json(&["ls", "--json"]);
    let agent = ls
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["worktree"] == wt["id"])
        .unwrap_or_else(|| panic!("no agent in the issue's worktree: {ls}"));
    w.wait_screen(
        agent["name"].as_str().unwrap(),
        "Work on GitHub issue #42: Crash on empty input",
    );
    // A worktree made for a PR reads as the PR: its title, asked of `gh` (git fetches the PR).
    let root = w.home().join("iss");
    w.git(&root, &["checkout", "-q", "-b", "the-fix"]);
    std::fs::write(root.join("fix.txt"), "fixed\n").unwrap();
    w.git(&root, &["add", "-A"]);
    w.git(&root, &["commit", "-q", "-m", "The fix"]);
    w.git(&root, &["push", "-q", "origin", "the-fix:refs/pull/9/head"]);
    w.git(&root, &["checkout", "-q", "main"]);
    w.gh_file("pr.json", &json!({"number": 9, "title": "Fix the crash"}));
    w.ok(&["wt", "new", "from-pr", "--pr", "9", "--project", "iss"]);
    let deadline = Instant::now() + WAIT;
    loop {
        let wt = w.json(&["wt", "status", "iss/from-pr", "--json"]);
        if wt["title"] == "#9 Fix the crash" {
            assert!(w.home().join("gh-state/calls.log").exists());
            break;
        }
        assert!(Instant::now() < deadline, "the PR's title never came: {wt}");
        std::thread::sleep(Duration::from_millis(150));
    }
    // A PR URL works where a number does.
    let bad =
        w.cli(&["wt", "new", "--pr", "https://github.com/acme/app/pull/x", "--project", "iss"]);
    assert_eq!(bad.status.code(), Some(2));
}
