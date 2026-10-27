//! M11 end-to-end tests: change sets. The Done-when (a three-member set whose lead's contract
//! you approve, members on two different agents, building against each other through the
//! overlay, a contract revision that reaches exactly the affected member, a restored set
//! checkpoint, and a merge train landing every member in dependency order), and every failure
//! mode of SPEC §15.5.11 with a scripted test each: a failing check stopping the train with a
//! precise partial-state report, a member without a remote, a member agent that errors out, a
//! contract revised after a member said ready, a sibling merged out of order by a human, a
//! rebase conflict, and creation that fails halfway. Every agent is `ptyro fake-agent`; every
//! repository has a bare "remote".

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::Script;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        Self::build(extra, false)
    }

    /// With a fake `gh` on the daemon's PATH: one PR per repository.
    fn with_gh() -> Self {
        Self::build("", true)
    }

    fn gh_state(&self, repo: &str) -> PathBuf {
        let d = self.home().join("gh-state").join(repo);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn build(extra: &str, gh: bool) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m11-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let mut env: Vec<(String, String)> = vec![
            ("HOME".into(), hs.clone()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("SHELL".into(), "/bin/sh".into()),
        ];
        if gh {
            use std::os::unix::fs::PermissionsExt;
            let bin = h.join("fakebin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("gh"), format!("#!/bin/sh\nexec \"{BIN}\" fake-gh \"$@\"\n"))
                .unwrap();
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            std::fs::create_dir_all(h.join("gh-state")).unwrap();
            env.push((
                "PATH".into(),
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
            ));
            env.push(("PTYRO_FAKE_GH".into(), h.join("gh-state").to_string_lossy().into_owned()));
            env.push(("PTYRO_FAKE_GH_PER_REPO".into(), "1".into()));
            env.push(("PTYRO_TEST_FAST_POLL".into(), "1".into()));
        }
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut config =
            String::from("[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n");
        for a in ["claude", "codex"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        config.push_str(extra);
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
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

    fn fails(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            !out.status.success(),
            "ptyro {args:?} should fail: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(out.status.success(), "ptyro {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
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

    /// A project cloned from a bare "remote" (or with no remote), registered, with one commit.
    fn project(&self, name: &str, files: &[(&str, &str)], remote: bool) -> PathBuf {
        let h = self.home();
        let root = h.join(name);
        if remote {
            self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
            self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        } else {
            self.git(&h, &["init", "-q", name]);
        }
        std::fs::write(root.join("README.md"), format!("# {name}\n")).unwrap();
        for (f, t) in files {
            std::fs::write(root.join(f), t).unwrap();
        }
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        if remote {
            self.git(&root, &["push", "-q", "origin", "main"]);
        }
        self.ok(&["project", "add", root.to_str().unwrap()]);
        root
    }

    fn screen(&self, s: &str) -> String {
        self.ok(&["read", s, "--scrollback", "400"])
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

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn show(&self, set: &str) -> Value {
        self.json(&["cs", "show", set, "--json"])
    }

    /// Wait until the set's agents are idle with no mail still to read. A revision reaches the
    /// members it doesn't affect at their next idle, so its turn can start under the next step.
    fn settle(&self, set: &str) {
        let info = self.show(set);
        let run = s(&info["run"]).to_owned();
        let mut agents: Vec<String> = info["sessions"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values().map(|v| s(v).to_owned()))
            .collect();
        agents.extend(info["lead"].as_str().map(str::to_owned));
        self.until("the set's agents settle", || {
            // Seen means the prompt hook ran, which also marked that turn working.
            let unread =
                self.json(&["msg", "ls", "--run", &run, "--json"]).as_array().is_some_and(|a| {
                    a.iter().any(|m| {
                        !s(&m["to"]).starts_with('@') && !matches!(s(&m["state"]), "seen" | "acked")
                    })
                });
            !unread
                && agents.iter().all(|a| {
                    self.json(&["status", a, "--json"])["session"]["status"]["status"] != "working"
                })
        });
    }

    fn member<'a>(&self, info: &'a Value, name: &str) -> &'a Value {
        info["set"]["members"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap()
    }

    fn tui(&self, args: &[&str]) -> Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = Script::new(argv)
            .size(140, 34)
            .timeout(12.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }

    fn template(&self, name: &str, text: &str) {
        let dir = self.d.paths.config.join("changesets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), text).unwrap();
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

/// The Done-when (SPEC §15.5, ROADMAP M11): a lead drafts the contract you approve; three
/// member agents (one Codex) are fenced to their own repositories and build against each other
/// through the overlay; readiness wakes a blocked member; a revision reaches exactly the member
/// it affects; a set checkpoint undoes a bad turn across repositories; `cs commit` links the
/// commits with trailers; `cs land` merges all three in dependency order.
#[test]
fn done_when_a_set_drafts_approves_builds_restores_and_lands() {
    let w = World::new("");
    w.project("api", &[("package.json", r#"{"name":"@acme/api"}"#)], true);
    w.project(
        "sdk",
        &[("package.json", r#"{"name":"@acme/sdk","dependencies":{"@acme/api":"1"}}"#)],
        true,
    );
    w.project(
        "web",
        &[("package.json", r#"{"name":"web","dependencies":{"@acme/api":"1","@acme/sdk":"1"}}"#)],
        true,
    );
    // Manifests decide the order; the overlay links siblings; api runs Codex.
    let i = w.json(&[
        "cs",
        "new",
        "tier",
        "--projects",
        "api,sdk,web",
        "--agent",
        "api=codex",
        "--overlay",
        "-p",
        "users get a tier: free or pro",
        "--json",
    ]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    assert_eq!(w.member(&i, "sdk")["depends_on"], serde_json::json!(["api"]), "{i:#}");
    assert_eq!(w.member(&i, "web")["depends_on"], serde_json::json!(["api", "sdk"]));
    assert_eq!(i["report"]["state"], "drafting");
    for m in ["api", "sdk", "web"] {
        assert!(root.join(m).join("README.md").exists(), "{m}'s worktree is in the root");
        assert_eq!(w.git(&root.join(m), &["branch", "--show-current"]).trim(), "tier");
    }
    let brief = std::fs::read_to_string(root.join("CHANGESET.md")).unwrap();
    assert!(brief.contains("api → sdk → web") && brief.contains("users get a tier"), "{brief}");
    let link = root.join("web/node_modules/@acme/api");
    assert_eq!(std::fs::read_link(&link).unwrap(), root.join("api"), "web builds against api");
    assert_eq!(
        std::fs::read_link(root.join("sdk/node_modules/@acme/api")).unwrap(),
        root.join("api")
    );
    assert!(
        w.git(&root.join("web"), &["status", "--porcelain"]).trim().is_empty(),
        "the overlay is never committed"
    );
    w.wait_screen("tier-lead", "fake claude");
    // The lead writes the contract (it owns the root's files, never members' code).
    w.ok(&["send", "tier-lead", "edit api/hack.txt nope"]);
    w.wait_screen("tier-lead", "may not edit api/hack.txt");
    std::fs::write(
        root.join("CONTRACT.md"),
        "# Contract: tier\n\n## api interface\naffects: api, sdk, web\n\nGET /users/{id} adds \"tier\": \"free\" | \"pro\".\n\n## sdk interface\naffects: sdk\n\nIsPro(user) bool\n\n## Merge order\n1. api 2. sdk 3. web\n",
    )
    .unwrap();
    w.ok(&["send", "tier-lead", "edit notes.md drafted"]);
    w.wait_screen("tier-lead", "wrote notes.md");
    // Its turn ended with a new contract: r1 waits for you.
    w.until("the contract gate", || {
        w.show("tier")["gates"]
            .as_array()
            .is_some_and(|g| g.iter().any(|g| s(&g["step"]) == "contract-r1"))
    });
    assert_eq!(w.show("tier")["report"]["state"], "contract_review");
    assert!(w.ok(&["cs", "contract", "tier", "show"]).contains("IsPro(user) bool"));
    w.ok(&["cs", "contract", "tier", "approve"]);
    w.until("three member agents", || {
        w.show("tier")["sessions"].as_object().is_some_and(|m| m.len() == 3)
    });
    let info = w.show("tier");
    assert_eq!(info["report"]["state"], "working");
    assert_eq!(s(&info["sessions"]["api"]), "tier-api");
    w.wait_screen("tier-api", "fake codex");
    w.wait_screen("tier-sdk", "IsPro(user) bool");
    // Fences: each member edits only its own directory (Claude and Codex alike).
    w.ok(&["send", "tier-sdk", "edit ../api/x.txt nope"]);
    w.wait_screen("tier-sdk", "it belongs to api");
    w.ok(&["send", "tier-api", "edit ../web/y.txt nope"]);
    w.wait_screen("tier-api", "it belongs to web");
    w.ok(&["send", "tier-sdk", "edit ../CONTRACT.md mine"]);
    w.wait_screen("tier-sdk", "the contract is the lead's");
    assert!(!root.join("api/x.txt").exists() && !root.join("web/y.txt").exists());
    w.ok(&["send", "tier-api", "edit tier.txt free|pro"]);
    w.wait_screen("tier-api", "wrote tier.txt");
    // Agents can't ship.
    w.ok(&["send", "tier-api", r#"sh "$PTYRO_BIN" cs push "$PTYRO_CS" --yes 2>&1; echo rc=$?"#]);
    w.wait_screen("tier-api", "is an agent");
    // blocked_on → the lead hears; ready → the blocked member is woken.
    w.ok(&["send", "tier-sdk", r#"sh "$PTYRO_BIN" cs blocked "$PTYRO_CS" api need-the-field"#]);
    w.until("sdk blocked on api", || {
        w.show("tier")["report"]["members"]["sdk"] == "blocked_on:api"
    });
    w.wait_screen("tier-lead", "kind=blocked_on");
    w.ok(&["send", "tier-api", r#"sh "$PTYRO_BIN" cs ready "$PTYRO_CS" field-is-in"#]);
    w.wait_screen("tier-sdk", "kind=ready_for_integration");
    let info = w.show("tier");
    assert_eq!(info["report"]["members"]["api"], "ready");
    assert!(w.member(&info, "sdk")["blocked_on"].as_array().unwrap().is_empty());
    // A revision that changes only the sdk section reaches sdk at once, the others later.
    let r2 = w.home().join("r2.md");
    std::fs::write(
        &r2,
        std::fs::read_to_string(root.join("CONTRACT.md"))
            .unwrap()
            .replace("IsPro(user) bool", "IsPro(user) bool; IsFree(user) bool"),
    )
    .unwrap();
    let out = w.ok(&["cs", "contract", "tier", "revise", r2.to_str().unwrap(), "-m", "add IsFree"]);
    assert!(out.contains("contract r2 (affects sdk)"), "{out}");
    w.until("the revision is delivered", || {
        w.json(&["msg", "ls", "--run", s(&info["run"]), "--json"]).as_array().unwrap().iter().any(
            |m| {
                m["kind"] == "contract_revised"
                    && s(&m["to"]) == "tier-sdk"
                    && m["priority"] == "interrupt"
            },
        )
    });
    let msgs = w.json(&["msg", "ls", "--run", s(&info["run"]), "--json"]);
    let revised: Vec<&Value> =
        msgs.as_array().unwrap().iter().filter(|m| m["kind"] == "contract_revised").collect();
    assert!(
        revised.iter().all(|m| s(&m["to"]) == "tier-sdk" || m["priority"] == "normal"),
        "{revised:#?}"
    );
    w.wait_screen("tier-sdk", "IsFree");
    // api said ready against r1, but r2 doesn't affect it: not stale.
    assert_ne!(w.show("tier")["report"]["state"], "stale_contract");
    // A set checkpoint, a bad turn across two repositories, and a restore (which needs every
    // member idle: api and web read r2 at their own pace).
    w.settle("tier");
    std::fs::write(root.join("sdk/sdk.txt"), "IsPro\n").unwrap();
    std::fs::write(root.join("web/web.txt"), "PRO badge\n").unwrap();
    let ck = w.ok(&["cs", "checkpoint", "tier", "good"]);
    let n: String =
        ck.split('#').nth(1).unwrap().chars().take_while(char::is_ascii_digit).collect();
    std::fs::remove_file(root.join("api/tier.txt")).unwrap();
    std::fs::write(root.join("web/web.txt"), "broken\n").unwrap();
    let refused = w.fails(&["cs", "restore", "tier", &n]);
    assert!(refused.contains("api, web changed since the last set checkpoint"), "{refused}");
    w.ok(&["cs", "restore", "tier", &n, "--force"]);
    assert_eq!(std::fs::read_to_string(root.join("api/tier.txt")).unwrap(), "free|pro\n");
    assert_eq!(std::fs::read_to_string(root.join("web/web.txt")).unwrap(), "PRO badge\n");
    // One commit per member, linked by trailers.
    w.ok(&["cs", "commit", "tier", "-m", "Add user tier"]);
    let body = w.git(&root.join("web"), &["log", "-1", "--format=%B"]);
    assert!(
        body.contains("Change-Set: tier")
            && body.contains("Change-Set-Members: api@")
            && body.contains(" sdk@"),
        "{body}"
    );
    assert!(!w.git(&root.join("web"), &["show", "--stat", "HEAD"]).contains("node_modules"));
    // The train: every member merged into its main, in order. (It won't start while an agent is
    // in a turn: the restore above reached them, and a slow machine is still reading it.)
    w.settle("tier");
    w.ok(&["cs", "land", "tier", "--yes"]);
    let info = w.show("tier");
    assert_eq!(info["set"]["phase"], "landed", "{info:#}");
    assert_eq!(info["status"], "succeeded");
    for m in ["api", "sdk", "web"] {
        let log = w.git(&w.home().join(format!("{m}.git")), &["log", "main", "--format=%s"]);
        assert!(log.starts_with("Add user tier"), "{m}: {log}");
    }
    let ledger = w.ok(&["cs", "log", "tier"]);
    for needle in [
        "created with api, sdk, web",
        "contract r1 by lead",
        "contract r1 approved",
        "sdk is blocked on api",
        "api is ready for integration",
        "contract r2 by user, affects sdk",
        "restored every member",
        "api landed",
        "web landed",
    ] {
        assert!(ledger.contains(needle), "the ledger lacks {needle:?}:\n{ledger}");
    }
}

/// A template sets the members, order, gates and checks; the train runs integration checks,
/// stops at a member whose check fails, and says exactly what landed. A hub template needs
/// trust first.
#[test]
fn a_failing_check_stops_the_train_with_a_partial_state_report() {
    let w = World::new("");
    w.project("api", &[], true);
    w.project("sdk", &[], true);
    w.project("web", &[], true);
    w.template(
        "trio",
        "name = \"trio\"\ndescription = \"api, sdk, web\"\nlayout = \"manual\"\ngates = []\nmembers = [\n  { project = \"api\" },\n  { project = \"sdk\", depends_on = [\"api\"] },\n  { project = \"web\", depends_on = [\"api\", \"sdk\"] },\n]\nintegration = [\"test -d api && test -d sdk\"]\ncontract_checks = { sdk = \"test -f fixed.txt\" }\n",
    );
    let t = w.json(&["cs", "templates", "--json"]);
    assert_eq!(t[0]["members"], serde_json::json!(["api", "sdk", "web"]));
    let i = w.json(&["cs", "new", "ship", "--template", "trio", "--json"]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    assert_eq!(i["set"]["phase"], "working", "manual: no agents, no contract gate");
    assert!(i["lead"].is_null());
    for m in ["api", "sdk", "web"] {
        std::fs::write(root.join(m).join("change.txt"), format!("{m}\n")).unwrap();
    }
    w.ok(&["cs", "commit", "ship", "-m", "Ship it"]);
    let check = w.cli(&["cs", "check", "ship"]);
    assert!(!check.status.success(), "sdk's contract check fails");
    let out = w.cli(&["cs", "land", "ship", "--yes"]);
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("stopped at sdk"), "{text}");
    let info = w.show("ship");
    let issue = info["report"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["state"] == "land_stopped")
        .unwrap()
        .clone();
    assert!(s(&issue["detail"]).contains("Landed: api; not landed: sdk, web"), "{issue:#}");
    assert!(s(&issue["action"]).contains("ptyro cs land ship"));
    assert_eq!(info["report"]["state"], "land_stopped");
    assert!(
        w.git(&w.home().join("api.git"), &["log", "main", "--format=%s"]).starts_with("Ship it")
    );
    assert!(
        !w.git(&w.home().join("sdk.git"), &["log", "main", "--format=%s"]).starts_with("Ship it")
    );
    // Fix it; the train resumes from sdk.
    std::fs::write(root.join("sdk/fixed.txt"), "ok\n").unwrap();
    w.ok(&["cs", "commit", "ship", "-m", "Fix sdk"]);
    w.ok(&["cs", "land", "ship", "--yes"]);
    assert_eq!(w.show("ship")["set"]["phase"], "landed");
    // A hub repo's template is refused until trusted.
    let hub = w.project("hub", &[], true);
    std::fs::create_dir_all(hub.join(".ptyro/changesets")).unwrap();
    std::fs::write(
        hub.join(".ptyro/changesets/pair.toml"),
        "name = \"pair\"\nlayout = \"manual\"\nmembers = [{ project = \"api\" }]\n",
    )
    .unwrap();
    let refused = w.fails(&["cs", "new", "p2", "--template", "pair"]);
    let fp = refused.rsplit("--trust ").next().unwrap().trim().to_owned();
    assert!(refused.contains("isn't trusted"), "{refused}");
    w.ok(&["cs", "new", "p2", "--template", "pair", "--trust", &fp]);
}

/// The failure modes: no remote, a failed member agent, a stale contract, an out-of-order merge,
/// a rebase conflict, and creation that fails halfway (nothing is left behind). Each one has a
/// state and an action, never a silent "done".
#[test]
fn every_failure_mode_has_a_state_and_an_action() {
    let w = World::new("");
    w.project("api", &[], true);
    w.project("sdk", &[], true);
    w.project("local", &[], false);
    // Cycles are refused at creation.
    let cyc = w.fails(&[
        "cs",
        "new",
        "loop",
        "--projects",
        "api,sdk",
        "--depends",
        "api:sdk",
        "--depends",
        "sdk:api",
        "--layout",
        "manual",
    ]);
    assert!(cyc.contains("cycle"), "{cyc}");
    // ── no remote: the push stops there and says who pushed.
    let i = w.json(&[
        "cs",
        "new",
        "nr",
        "--projects",
        "api,local",
        "--depends",
        "local:api",
        "--layout",
        "manual",
        "--no-gates",
        "--json",
    ]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    for m in ["api", "local"] {
        std::fs::write(root.join(m).join("x.txt"), "x\n").unwrap();
    }
    w.ok(&["cs", "commit", "nr", "-m", "x"]);
    let out = w.fails(&["cs", "push", "nr", "--yes"]);
    assert!(out.contains("push stopped at local") && out.contains("Pushed: api"), "{out}");
    let info = w.show("nr");
    assert!(
        info["report"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["state"] == "no_remote" && s(&x["action"]).contains("remote add")),
        "{info:#}"
    );
    // Resuming skips what's pushed.
    let out = w.fails(&["cs", "push", "nr", "--yes"]);
    assert!(out.contains("already up to date: api"), "{out}");
    w.ok(&["cs", "rm", "nr", "--force"]);
    // ── a member agent that errors out; a contract revised after a member said ready.
    let i = w.json(&["cs", "new", "fm", "--projects", "api,sdk", "--no-gates", "--json"]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    w.wait_screen("fm-lead", "fake claude");
    std::fs::write(
        root.join("CONTRACT.md"),
        "# C\n\n## api\naffects: api\n\nv1\n\n## sdk\naffects: sdk\n\nv1\n",
    )
    .unwrap();
    w.ok(&["cs", "contract", "fm", "submit"]);
    w.until("members started (no contract gate)", || {
        w.show("fm")["sessions"].as_object().is_some_and(|m| m.len() == 2)
    });
    w.ok(&["cs", "ready", "fm", "--member", "api"]);
    w.ok(&["cs", "ready", "fm", "--member", "sdk"]);
    assert_eq!(w.show("fm")["report"]["state"], "ready");
    let r2 = w.home().join("fm2.md");
    std::fs::write(&r2, "# C\n\n## api\naffects: api\n\nv2\n\n## sdk\naffects: sdk\n\nv1\n")
        .unwrap();
    w.ok(&["cs", "contract", "fm", "revise", r2.to_str().unwrap()]);
    let info = w.show("fm");
    assert_eq!(info["report"]["state"], "stale_contract", "{:#}", info["report"]);
    assert_eq!(info["report"]["members"]["api"], "stale");
    assert_eq!(info["report"]["members"]["sdk"], "ready", "r2 doesn't concern sdk");
    w.wait_screen("fm-sdk", "fake claude");
    // sdk reads r2 at its next idle: a good turn that would clear the failure below.
    w.settle("fm");
    w.ok(&["send", "fm-sdk", "fail"]);
    w.until("sdk failed", || w.show("fm")["report"]["state"] == "member_failed");
    let info = w.show("fm");
    let x = info["report"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["state"] == "member_failed")
        .unwrap()
        .clone();
    assert!(s(&x["detail"]).starts_with("sdk:") && !s(&x["action"]).is_empty(), "{x:#}");
    assert!(
        w.json(&["inbox", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .any(|e| s(&e["title"]).contains("sdk failed"))
    );
    // Recovers on its next good turn.
    w.ok(&["send", "fm-sdk", "work 0.2 again"]);
    w.until("sdk recovered", || w.member(&w.show("fm"), "sdk")["failed"].is_null());
    w.ok(&["cs", "rm", "fm", "--force"]);
    // ── out of order: a human merges sdk before api.
    let i = w.json(&[
        "cs",
        "new",
        "oo",
        "--projects",
        "api,sdk",
        "--depends",
        "sdk:api",
        "--layout",
        "manual",
        "--no-gates",
        "--json",
    ]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    for m in ["api", "sdk"] {
        std::fs::write(root.join(m).join("oo.txt"), "x\n").unwrap();
    }
    w.ok(&["cs", "commit", "oo", "-m", "oo"]);
    w.git(&root.join("sdk"), &["push", "-q", "origin", "HEAD:main"]);
    w.git(&root.join("sdk"), &["fetch", "-q", "origin"]);
    w.until("the out-of-order merge is noticed", || {
        w.show("oo")["report"]["state"] == "out_of_order"
    });
    let x = w.show("oo")["report"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["state"] == "out_of_order")
        .unwrap()
        .clone();
    assert!(s(&x["detail"]).contains("before api"), "{x:#}");
    // Landing the rest puts it right.
    w.ok(&["cs", "land", "oo", "--yes"]);
    assert_eq!(w.show("oo")["set"]["phase"], "landed");
    // ── a conflict: main moved under a member's change.
    let i = w.json(&[
        "cs",
        "new",
        "cf",
        "--projects",
        "api",
        "--layout",
        "manual",
        "--no-gates",
        "--json",
    ]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    std::fs::write(root.join("api/README.md"), "# mine\n").unwrap();
    w.ok(&["cs", "commit", "cf", "-m", "mine"]);
    let main = w.home().join("api");
    w.git(&main, &["pull", "-q", "origin", "main"]);
    std::fs::write(main.join("README.md"), "# theirs\n").unwrap();
    w.git(&main, &["commit", "-qam", "theirs"]);
    let out = w.cli(&["cs", "land", "cf", "--yes"]);
    assert!(!out.status.success());
    let info = w.show("cf");
    assert_eq!(info["report"]["state"], "land_stopped", "{:#}", info["report"]);
    assert!(
        info["report"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["state"] == "member_conflict")
    );
    // ── creation that fails halfway leaves nothing behind.
    w.git(&w.home().join("sdk"), &["branch", "half"]);
    w.git(&w.home().join("sdk"), &["checkout", "-q", "half"]);
    let out = w.fails(&["cs", "new", "half", "--projects", "api,sdk", "--layout", "manual"]);
    assert!(out.contains("nothing was created"), "{out}");
    assert!(!w.d.paths.data.join("changesets/half").exists());
    let wts = w.ok(&["wt", "ls"]);
    assert!(!wts.contains("api/half"), "{wts}");
}

/// The lead's `cs_*` tools through `ptyro mcp`: status, assign, the contract, a member's diff,
/// checks, a checkpoint and a question for the human. No shipping tools exist. The set survives
/// a daemon restart.
#[test]
fn a_set_lead_uses_its_tools_and_the_set_survives_a_restart() {
    let mut w = World::new("");
    w.project("api", &[], true);
    w.project("web", &[], true);
    let i = w.json(&["cs", "new", "mc", "--projects", "api,web", "--gates", "contract", "--json"]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    w.wait_screen("mc-lead", "fake claude");
    // The contract is written before the lead's first turn ends: once the lead has spoken, the
    // actor records a CONTRACT.md saved while it's idle as r1 itself, racing the submit.
    std::fs::write(root.join("CONTRACT.md"), "# C\n\n## web\naffects: web\n\nbadge\n").unwrap();
    w.ok(&["send", "mc-lead", r#"mcp cs_contract_submit {"summary":"first"}"#]);
    w.wait_screen("mc-lead", "\"rev\": 1");
    w.ok(&["send", "mc-lead", "mcp cs_status {}"]);
    w.wait_screen("mc-lead", "\"state\": \"contract_review\"");
    w.ok(&["cs", "contract", "mc", "approve"]);
    w.until("members", || w.show("mc")["sessions"].as_object().is_some_and(|m| m.len() == 2));
    w.ok(&["send", "mc-lead", r#"mcp cs_assign {"member":"web","task":"render the badge"}"#]);
    w.wait_screen("mc-web", "render the badge");
    w.ok(&["send", "mc-web", "edit badge.txt PRO"]);
    w.wait_screen("mc-web", "wrote badge.txt");
    w.ok(&["send", "mc-lead", r#"mcp cs_member_diff {"member":"web"}"#]);
    w.wait_screen("mc-lead", "badge.txt");
    w.ok(&["send", "mc-lead", r##"mcp cs_contract_revise {"text":"# C\n\n## web\naffects: web\n\nbadge, gold\n","summary":"gold"}"##]);
    w.wait_screen("mc-lead", "\"rev\": 2");
    w.wait_screen("mc-web", "contract is now r2 (gold)");
    w.ok(&["send", "mc-lead", r#"mcp cs_checkpoint {"label":"after badge"}"#]);
    w.wait_screen("mc-lead", "after badge");
    w.ok(&["send", "mc-lead", r#"mcp cs_request_gate {"question":"ship the gold badge?"}"#]);
    w.until("the lead's question", || {
        w.show("mc")["gates"]
            .as_array()
            .is_some_and(|g| g.iter().any(|g| s(&g["gate"]["ask"]) == "ship the gold badge?"))
    });
    let g = w.show("mc")["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| s(&g["gate"]["ask"]) == "ship the gold badge?")
        .unwrap()["gate"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    w.ok(&["gate", "resolve", &g, "approve"]);
    w.wait_screen("mc-lead", "answered \"approve\"");
    // The lead can't approve its own gates, edit members, or ship.
    w.ok(&["send", "mc-lead", "mcp cs_push {}"]);
    w.wait_screen("mc-lead", "no tool named cs_push");
    // A restart keeps everything, and the actor picks up again.
    w.d.restart().unwrap();
    let info = w.show("mc");
    assert_eq!(info["set"]["contract"].as_array().unwrap().len(), 2);
    assert_eq!(info["set"]["phase"], "working");
    std::fs::write(
        root.join("CONTRACT.md"),
        "# C\n\n## web\naffects: web\n\nbadge, gold, silver\n",
    )
    .unwrap();
    w.ok(&["cs", "contract", "mc", "submit", "-m", "silver"]);
    assert_eq!(w.show("mc")["set"]["contract"].as_array().unwrap().len(), 3);
    w.ok(&["cs", "rm", "mc", "--force"]);
    assert!(!root.exists());
}

/// Linked PRs: one per member, each carrying a live table of its siblings; the train merges
/// through gates answered here, waits for each PR's CI, and a CI failure in sdk stops it with
/// what landed and what didn't. Fixing CI resumes it.
#[test]
fn linked_prs_and_a_ci_failure_stopping_the_train() {
    let w = World::with_gh();
    for p in ["api", "sdk", "web"] {
        w.project(p, &[], true);
    }
    let i = w.json(&[
        "cs",
        "new",
        "prs",
        "--projects",
        "api,sdk,web",
        "--depends",
        "sdk:api",
        "--depends",
        "web:api+sdk",
        "--layout",
        "manual",
        "--gates",
        "land",
        "-p",
        "Add the tier field",
        "--json",
    ]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    for m in ["api", "sdk", "web"] {
        std::fs::write(root.join(m).join("tier.txt"), format!("{m}\n")).unwrap();
    }
    w.ok(&["cs", "commit", "prs", "-m", "Add the tier field"]);
    let out = w.ok(&["cs", "pr", "prs"]);
    assert_eq!(out.matches("https://github.com/acme/app/pull/").count(), 3, "{out}");
    let info = w.show("prs");
    let n = |m: &str| w.member(&info, m)["pr"].as_u64().unwrap();
    let body = std::fs::read_to_string(w.gh_state("web").join("body.txt")).unwrap();
    assert!(
        body.contains(&format!("| 1 | api | [#{}](", n("api"))) && body.contains("web (this PR)"),
        "{body}"
    );
    assert!(
        body.contains(&format!("| 2 | sdk | [#{}](", n("sdk"))),
        "every PR knows its siblings: {body}"
    );
    let pass = r#"[{"name":"build","bucket":"pass","link":null,"workflow":"ci"}]"#;
    std::fs::write(w.gh_state("api").join("checks.json"), pass).unwrap();
    std::fs::write(w.gh_state("web").join("checks.json"), pass).unwrap();
    std::fs::write(
        w.gh_state("sdk").join("checks.json"),
        r#"[{"name":"build","bucket":"fail","link":null,"workflow":"ci"}]"#,
    )
    .unwrap();
    // Not a terminal: the train waits on gates answered elsewhere.
    w.ok(&["cs", "land", "prs", "--no-wait"]);
    for m in ["api", "sdk"] {
        w.until(&format!("the {m} gate"), || {
            w.show("prs")["gates"]
                .as_array()
                .is_some_and(|g| g.iter().any(|g| s(&g["step"]) == format!("land-{m}")))
        });
        let g = w.show("prs")["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| s(&g["step"]) == format!("land-{m}"))
            .unwrap()["gate"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        w.ok(&["gate", "resolve", &g, "merge"]);
    }
    w.until("the train stops at sdk", || w.show("prs")["report"]["state"] == "land_stopped");
    let info = w.show("prs");
    let x = info["report"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["state"] == "land_stopped")
        .unwrap()
        .clone();
    assert!(s(&x["detail"]).contains(&format!("CI failed on PR #{}: build", n("sdk"))), "{x:#}");
    assert!(s(&x["detail"]).contains("Landed: api; not landed: sdk, web"), "{x:#}");
    assert!(std::fs::read_to_string(w.gh_state("api").join("pr.json")).unwrap().contains("MERGED"));
    assert!(
        !std::fs::read_to_string(w.gh_state("sdk").join("pr.json")).unwrap().contains("MERGED")
    );
    // The tables say what merged.
    let body = std::fs::read_to_string(w.gh_state("sdk").join("body.txt")).unwrap();
    assert!(body.contains("| 1 | api |") && body.contains("| merged |"), "{body}");
    // CI fixed: the train resumes from sdk.
    std::fs::write(w.gh_state("sdk").join("checks.json"), pass).unwrap();
    w.ok(&["cs", "land", "prs", "--yes"]);
    let info = w.show("prs");
    assert_eq!(info["set"]["phase"], "landed", "{:#}", info["report"]);
    for m in ["sdk", "web"] {
        assert!(
            std::fs::read_to_string(w.gh_state(m).join("pr.json")).unwrap().contains("MERGED"),
            "{m}"
        );
    }
}

/// The TUI: the set is a sidebar node above its lead, amber while it waits for you; `S` lists
/// sets and opens one as swimlanes (lead and members), its gate, and the ledger as a timeline.
#[tokio::test]
async fn the_tui_shows_a_set_as_swimlanes_with_its_ledger() {
    let w = World::new("");
    w.project("api", &[], true);
    w.project("web", &[], true);
    let i = w.json(&["cs", "new", "tv", "--projects", "api,web", "--json"]);
    let root = PathBuf::from(s(&i["set"]["root"]));
    w.wait_screen("tv-lead", "fake claude");
    std::fs::write(root.join("CONTRACT.md"), "# C\n\n## web\naffects: web\n\nbadge\n").unwrap();
    w.ok(&["cs", "contract", "tv", "submit"]);
    let r = ptyro_testkit::headless::run(
        w.tui(&["attach", "tv-lead"])
            .snapshot(1.5)
            .send(1.7, "\\x11S")
            .snapshot(2.5)
            .send(2.7, "\\r")
            .snapshot(3.6)
            .send(3.8, "q")
            .send(4.1, "\\x11d"),
    )
    .await
    .unwrap();
    let side = r.at(1.5);
    assert!(side.contains("◆ tv") && side.contains("contract_review"), "{side}");
    assert!(r.at(2.5).contains("Change sets"), "{}", r.at(2.5));
    let view = r.at(3.6);
    for needle in ["Change set tv", "lead", "api", "web", "gate", "contract r1 by user"] {
        assert!(view.contains(needle), "the set view lacks {needle:?}:\n{view}");
    }
}
