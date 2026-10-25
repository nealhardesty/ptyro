//! `land_pr` and the merge train against a fake `gh` (SPEC §15.5): a closed PR, a PR with no
//! checks yet (the grace period), checks that are pending and then pass, and the head commit
//! the merge is pinned to. Every repository has a bare "remote"; `gh` is `ptyro fake-gh`.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);
const PASS: &str = r#"[{"name":"build","bucket":"pass","link":null,"workflow":"ci"}]"#;
const GRACE_MS: u64 = 4000;

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-land-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let bin = h.join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("gh"), format!("#!/bin/sh\nexec \"{BIN}\" fake-gh \"$@\"\n"))
            .unwrap();
        std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(h.join("gh-state")).unwrap();
        let env = [
            ("HOME".to_owned(), h.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("SHELL".into(), "/bin/sh".into()),
            (
                "PATH".into(),
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
            ),
            ("PTYRO_FAKE_GH".into(), h.join("gh-state").to_string_lossy().into_owned()),
            ("PTYRO_FAKE_GH_PER_REPO".into(), "1".into()),
            ("PTYRO_TEST_FAST_POLL".into(), "1".into()),
            ("PTYRO_TEST_LAND_GRACE_MS".into(), GRACE_MS.to_string()),
        ];
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let config = "[worktrees]\nfetch = false\n";
        let d = TempDaemon::start_with(BIN, &env, Some(config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
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
        serde_json::from_slice(&out.stdout).unwrap()
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

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn gh_state(&self, repo: &str) -> PathBuf {
        let d = self.home().join("gh-state").join(repo);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pr(&self, repo: &str) -> Value {
        let t = std::fs::read_to_string(self.gh_state(repo).join("pr.json")).unwrap();
        serde_json::from_str(&t).unwrap()
    }

    /// Rewrite fields of the fake PR, as GitHub would have changed them.
    fn edit_pr(&self, repo: &str, f: impl FnOnce(&mut Value)) {
        let mut pr = self.pr(repo);
        f(&mut pr);
        std::fs::write(self.gh_state(repo).join("pr.json"), pr.to_string()).unwrap();
    }

    fn gh_calls(&self, repo: &str) -> String {
        std::fs::read_to_string(self.gh_state(repo).join("calls.log")).unwrap_or_default()
    }

    /// A one-member set whose commit has a PR, and a land gate waiting for you.
    fn set_with_pr(&self, repo: &str) {
        let h = self.home();
        let root = h.join(repo);
        self.git(&h, &["init", "-q", "--bare", &format!("{repo}.git")]);
        self.git(&h, &["clone", "-q", &format!("{repo}.git"), repo]);
        std::fs::write(root.join("README.md"), "# r\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.ok(&["project", "add", root.to_str().unwrap()]);
        let i = self.json(&[
            "cs",
            "new",
            repo,
            "--projects",
            repo,
            "--layout",
            "manual",
            "--gates",
            "land",
            "-p",
            "land it",
            "--json",
        ]);
        let wt = PathBuf::from(i["set"]["root"].as_str().unwrap()).join(repo);
        std::fs::write(wt.join("change.txt"), "x\n").unwrap();
        self.ok(&["cs", "commit", repo, "-m", "Change"]);
        self.ok(&["cs", "pr", repo]);
    }

    /// Start the train without waiting, and answer its land gate "merge".
    fn start_train(&self, set: &str) {
        self.ok(&["cs", "land", set, "--no-wait"]);
        let step = format!("land-{set}");
        let gate = std::cell::RefCell::new(String::new());
        self.until("the land gate", || {
            let g = self.json(&["cs", "show", set, "--json"])["gates"]
                .as_array()
                .and_then(|g| g.iter().find(|g| g["step"] == step.as_str()).cloned());
            let id = g.and_then(|g| g["gate"]["id"].as_str().map(str::to_owned));
            *gate.borrow_mut() = id.clone().unwrap_or_default();
            id.is_some()
        });
        self.ok(&["gate", "resolve", &gate.borrow(), "merge"]);
    }

    fn report(&self, set: &str) -> Value {
        self.json(&["cs", "show", set, "--json"])
    }

    fn stopped_detail(&self, set: &str) -> String {
        self.until("the train stops", || self.report(set)["report"]["state"] == "land_stopped");
        self.report(set)["report"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["state"] == "land_stopped")
            .map(|x| x["detail"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    }
}

/// A PR closed without merging stops the train and says so; nothing is merged.
#[test]
fn a_closed_pr_stops_the_train() {
    let w = World::new();
    w.set_with_pr("api");
    let n = w.pr("api")["number"].as_u64().unwrap();
    std::fs::write(w.gh_state("api").join("checks.json"), PASS).unwrap();
    w.edit_pr("api", |p| p["state"] = "CLOSED".into());
    w.start_train("api");
    let detail = w.stopped_detail("api");
    assert!(detail.contains(&format!("PR #{n} is closed")), "{detail}");
    assert!(!w.gh_calls("api").contains("pr merge"), "{}", w.gh_calls("api"));
}

/// A PR with no check runs yet is "not known yet", not green: the train waits out the grace
/// period and then merges on the rest of its state.
#[test]
fn a_pr_with_no_checks_waits_out_the_grace_period() {
    let w = World::new();
    w.set_with_pr("api");
    let started = Instant::now();
    w.start_train("api");
    assert!(!w.gh_calls("api").contains("pr merge"), "merged before the grace period");
    w.until("the merge", || w.pr("api")["state"] == "MERGED");
    assert!(
        started.elapsed() >= Duration::from_millis(GRACE_MS - 500),
        "merged after only {:?}",
        started.elapsed()
    );
    w.until("landed", || w.report("api")["set"]["phase"] == "landed");
}

/// Pending checks hold the merge; when they pass, it goes ahead, pinned to the commit whose
/// checks were looked at.
#[test]
fn pending_checks_hold_the_merge_and_it_is_pinned_to_the_head_commit() {
    let w = World::new();
    w.set_with_pr("api");
    w.edit_pr("api", |p| p["headRefOid"] = "abc123".into());
    std::fs::write(w.gh_state("api").join("head.txt"), "abc123\n").unwrap();
    std::fs::write(
        w.gh_state("api").join("checks.json"),
        r#"[{"name":"build","bucket":"pending","link":null,"workflow":"ci"}]"#,
    )
    .unwrap();
    w.start_train("api");
    // Several polls go by while the check is pending, and nothing is merged.
    w.until("the checks are polled again", || w.gh_calls("api").matches("pr checks").count() >= 3);
    assert!(!w.gh_calls("api").contains("pr merge"));
    std::fs::write(w.gh_state("api").join("checks.json"), PASS).unwrap();
    w.until("the merge", || w.pr("api")["state"] == "MERGED");
    assert!(
        w.gh_calls("api")
            .lines()
            .any(|l| l.starts_with("pr merge") && l.ends_with("--match-head-commit abc123")),
        "{}",
        w.gh_calls("api")
    );
}

/// A push after the checks were looked at changes the head: GitHub refuses the pinned merge,
/// the train stops with that as the reason, and the PR is not merged unseen.
#[test]
fn a_push_after_the_checks_were_read_is_not_merged_unseen() {
    let w = World::new();
    w.set_with_pr("api");
    std::fs::write(w.gh_state("api").join("checks.json"), PASS).unwrap();
    w.edit_pr("api", |p| p["headRefOid"] = "abc123".into());
    std::fs::write(w.gh_state("api").join("head.txt"), "def456\n").unwrap();
    w.start_train("api");
    let detail = w.stopped_detail("api");
    assert!(detail.contains("Head branch was modified"), "{detail}");
    assert_eq!(w.pr("api")["state"], "OPEN");
}
