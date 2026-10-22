//! M7 end-to-end tests: orchestration. The Done-when (a three-agent fan-out compared and the
//! best merged; a lead agent using `ptyro mcp` to start two workers, wait for them and report,
//! held to its policy), recipes with verified-done bounces, gates and safe outputs, runs that
//! survive a daemon restart, forks, the mailbox, file leases, the collision radar, checkpoints
//! and verified-done for an agent without a blocking Stop hook. Every agent is `ptyro
//! fake-agent` in the right adapter's mode.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        Self::with_agents(extra, |_| String::new())
    }

    /// Each agent's table gets `per_agent(name)` (an `env = {…}` line).
    fn with_agents(extra: &str, per_agent: impl Fn(&str) -> String) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m7-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config =
            String::from("[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n");
        for a in ["claude", "codex", "gemini"] {
            config.push_str(&format!(
                "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n{}\n",
                per_agent(a)
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

    /// A project cloned from a bare "remote", registered, with one pushed commit.
    fn project(&self, name: &str) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("f.txt"), "one\ntwo\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
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

    fn run(&self, id: &str) -> Value {
        self.json(&["run", "show", id, "--json"])
    }

    fn run_status(&self, id: &str) -> String {
        self.run(id)["state"]["status"].as_str().unwrap_or_default().to_owned()
    }

    fn wait_run(&self, id: &str, status: &str) -> Value {
        self.until(&format!("run {id} {status}"), || self.run_status(id) == status);
        self.run(id)
    }

    fn open_gate(&self) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let g = self.json(&["gate", "ls", "--json"]);
            if let Some(first) = g.as_array().and_then(|a| a.first()) {
                return first.clone();
            }
            assert!(Instant::now() < deadline, "no gate opened");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// The Done-when, part one: `ptyro fanout --agents claude,codex,gemini` makes three worktrees
/// from one base; the compare view ranks them by evidence (only claude's passes the tests; the
/// base doesn't); the best is merged and the rest discarded, safely.
#[test]
fn done_when_fanout_compare_and_merge_the_best() {
    let w = World::new("");
    let root = w.project("app");
    std::fs::write(w.home().join("task.md"), "sh echo $PTYRO_STEP_ID > answer.txt\n").unwrap();
    let out = w.ok(&[
        "fanout",
        "--agents",
        "claude,codex,gemini",
        "--prompt-file",
        w.home().join("task.md").to_str().unwrap(),
        "--project",
        "app",
        "--name",
        "ans",
        "--test",
        "grep -q claude answer.txt",
    ]);
    let id = out.lines().next().unwrap().trim().to_owned();
    assert!(id.starts_with("r-"), "{out}");
    let run = w.wait_run(&id, "succeeded");
    assert_eq!(run["state"]["kind"], "fanout");
    // Three worktrees from the same base, named after the prefix and the agent.
    let wts = w.json(&["wt", "ls", "--project", "app", "--json"]);
    let names: Vec<&str> =
        wts.as_array().unwrap().iter().filter_map(|x| x["name"].as_str()).collect();
    for n in ["ans-claude", "ans-codex", "ans-gemini"] {
        assert!(names.contains(&n), "{names:?}");
    }
    let doc = w.json(&["compare", &id, "--json"]);
    let c = doc["candidates"].as_array().unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[0]["step"], "claude", "{doc:#}");
    assert_eq!(c[0]["tests"], 0);
    assert_eq!(c[0]["rank"], 1);
    assert!(c[1]["tests"].as_i64().unwrap() != 0);
    assert_ne!(
        doc["base_tests"], 0,
        "the base fails the tests (grep: no file), so nothing broke them"
    );
    assert!(c.iter().all(|x| x["broke_tests"] == false));
    assert!(c[0]["reasons"].as_array().unwrap().iter().any(|r| r == "tests pass"), "{doc:#}");
    assert!(
        c.iter().all(|x| x["cluster"].as_u64().unwrap() > 0),
        "every candidate changed something"
    );
    assert_ne!(c[0]["cluster"], c[1]["cluster"], "different changes, different clusters");
    let human = w.ok(&["compare", &id]);
    assert!(human.contains("#1 claude") && human.contains("ptyro pick"), "{human}");
    // Merge the best: its uncommitted work is committed as the profile, then integrated.
    w.ok(&["pick", &id, "claude"]);
    assert_eq!(w.git(&root, &["show", "main:answer.txt"]).trim(), "claude");
    assert!(
        w.git(&root, &["log", "-1", "--format=%s", "main"])
            .contains(&format!("claude from run {id}"))
    );
    // Discarding refuses to lose the others' (untracked) work without --force…
    let kept = w.ok(&["discard", &id]);
    assert!(kept.contains("kept"), "{kept}");
    w.ok(&["discard", &id, "--force"]);
    let wts = w.json(&["wt", "ls", "--project", "app", "--json"]);
    let names: Vec<&str> =
        wts.as_array().unwrap().iter().filter_map(|x| x["name"].as_str()).collect();
    assert!(!names.contains(&"ans-codex") && !names.contains(&"ans-gemini"), "{names:?}");
    let st = w.run(&id)["state"].clone();
    assert_eq!(st["picked"][0][0], "claude");
    assert_eq!(st["discarded"].as_array().unwrap().len(), 2);
    // A fork keeps the candidates and ranks again.
    let fork =
        w.ok(&["run", "fork", &id, "--from", "rank"]).lines().next().unwrap().trim().to_owned();
    let f = w.wait_run(&fork, "succeeded");
    assert_eq!(f["state"]["forked_from"][0], id.as_str());
    assert_eq!(f["state"]["steps"]["claude"]["status"], "succeeded", "copied, not re-run");
    assert!(w.ok(&["run", "ls", "--tree"]).contains(&format!("└ {fork}")));
}

/// The Done-when, part two: a lead Claude session with `ptyro mcp` (injected through
/// `--mcp-config`) starts two workers, waits for them and reports back, and can't exceed its
/// policy, whether through MCP or by shelling out to `ptyro`.
#[test]
fn done_when_a_lead_coordinates_workers_within_its_policy() {
    let w = World::new("");
    let root = w.project("app");
    let lead = w.ok(&[
        "new",
        "-d",
        "--agent",
        "claude",
        "--lead",
        "--max-workers",
        "2",
        "-n",
        "lead",
        "-C",
        root.to_str().unwrap(),
    ]);
    assert!(!lead.trim().is_empty());
    w.wait_screen("lead", "fake claude");
    let runs = w.json(&["run", "ls", "--json"]);
    let run = runs.as_array().unwrap().iter().find(|r| r["state"]["kind"] == "lead").unwrap()["state"]["id"].as_str().unwrap().to_owned();
    w.ok(&[
        "send",
        "lead",
        r#"mcp spawn_session {"agent":"claude","name":"w1","prompt":"work 0.3 one"}"#,
    ]);
    w.wait_screen("lead", "mcp spawn_session ok");
    w.ok(&["send", "lead", r#"mcp spawn_session {"agent":"claude","name":"w2","prompt":"work 0.3 two","idempotency_key":"k2"}"#]);
    w.until("two workers", || w.screen("lead").matches("mcp spawn_session ok").count() >= 2);
    // A retry with the same key is the same worker, not a third.
    w.ok(&["send", "lead", r#"mcp spawn_session {"agent":"claude","name":"w2b","prompt":"work 0.3 two","idempotency_key":"k2"}"#]);
    w.wait_screen("lead", "\"reused\": true");
    // A third live worker passes the limit.
    w.ok(&[
        "send",
        "lead",
        r#"mcp spawn_session {"agent":"claude","name":"w3","prompt":"work 0.3 three"}"#,
    ]);
    w.wait_screen("lead", "at most 2 live workers");
    // Wait for both, then report back to the human.
    w.ok(&["send", "lead", r#"mcp wait_for {"sessions":["w1","w2"],"timeout_s":30}"#]);
    w.wait_screen("lead", "\"reached\": true");
    // Workers report themselves too: their turn ends reach the lead's mailbox.
    w.ok(&["send", "lead", r#"mcp check_messages {"wait_s":10}"#]);
    w.wait_screen("lead", "worker_done");
    // Summed up by the worker's reply ("one"), not its screen's last line ("worked: one").
    w.until("w1's turn summed up by its reply", || {
        w.screen("lead")
            .lines()
            .any(|l| l.starts_with("w1 finished turn") && l.trim_end().ends_with(": one"))
    });
    w.ok(&["send", "lead", r#"mcp post_message {"to":"@user","body":"both workers are done"}"#]);
    w.until("the report reached the inbox", || {
        w.json(&["inbox", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["body"].as_str().is_some_and(|b| b.contains("both workers are done")))
    });
    // The run shows the workers as steps.
    let st = w.run(&run)["state"].clone();
    assert!(st["steps"]["w1"].is_object() && st["steps"]["w2"].is_object(), "{st:#}");
    assert!(st["steps"].get("w3").is_none());
    // It may not touch sessions that aren't its workers…
    w.ok(&["new", "-d", "-n", "bystander", "--", "sh"]);
    w.ok(&["send", "lead", r#"mcp read_screen {"session":"bystander"}"#]);
    w.wait_screen("lead", "isn't one of this run's workers");
    // …nor get around the policy by running ptyro itself: spawning and answering gates are refused.
    w.ok(&["send", "lead", r#"sh "$PTYRO_BIN" new -d -- true; echo rc=$?"#]);
    w.wait_screen("lead", "ran: rc=1");
    // Asking the human works, and only a human answers.
    w.ok(&["send", "lead", r#"mcp request_gate {"question":"Merge w1?","options":["yes","no"]}"#]);
    let g = w.open_gate();
    let gid = g["gate"]["id"].as_str().unwrap().to_owned();
    w.ok(&["send", "w1", &format!(r#"sh "$PTYRO_BIN" gate resolve {gid} yes; echo rc=$?"#)]);
    w.wait_screen("w1", "ran: rc=1");
    assert!(
        w.json(&["gate", "ls", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["gate"]["id"] == gid.as_str()),
        "still open"
    );
    w.ok(&["gate", "resolve", &gid, "yes"]);
    w.wait_screen("lead", "\"choice\": \"yes\"");
    // Idempotent: the same answer again is fine; another one isn't.
    w.ok(&["gate", "resolve", &gid, "yes"]);
    assert!(!w.cli(&["gate", "resolve", &gid, "no"]).status.success());
}

/// Without any policy, an agent still can't answer for a person by another route: keys typed
/// into a blocked session (through `send` or a client's keystrokes) are refused, and so are
/// pairing a device and trusting a repository's scripts; a reparented or `setsid` caller is
/// still placed in its session. Typing into an idle agent stays allowed (orchestration).
#[test]
fn agents_cannot_answer_prompts_pair_devices_or_grant_trust() {
    let w = World::new(&format!(
        "[agents.peek]\ncommand = [\"{BIN}\", \"attach\", \"victim\"]\nenv = {{ PTYRO_ALLOW_NESTED = \"1\" }}\n"
    ));
    let root = w.project("app");
    std::fs::write(root.join(".ptyro.toml"), "[scripts]\nsetup = \"echo hi\"\n").unwrap();
    w.ok(&["new", "-d", "--agent", "claude", "-n", "victim"]);
    w.ok(&["new", "-d", "--agent", "claude", "-n", "intruder", "-C", root.to_str().unwrap()]);
    w.wait_screen("victim", "fake claude");
    w.wait_screen("intruder", "fake claude");
    let status = |name: &str| {
        let ls = w.json(&["ls", "--json"]);
        let s = ls.as_array().unwrap().iter().find(|s| s["name"] == name).unwrap();
        s["status"]["status"].as_str().unwrap_or_default().to_owned()
    };
    w.ok(&["send", "victim", "ask rm -rf /"]);
    w.until("victim blocked", || status("victim") == "blocked");
    let mut tries = vec![
        // `send --raw 1` would pick "Yes" in the dialog.
        r#"sh "$PTYRO_BIN" send victim --raw 1 2>&1; echo rc=$?"#,
        // Orphaned (double-forked): the ancestry walk can't place it, its POSIX session can.
        r#"sh sh -c '("$PTYRO_BIN" send victim --raw 1 >o 2>&1; echo rc=$? >>o) &'; sleep 2; cat o"#,
        r#"sh "$PTYRO_BIN" pair stolen 2>&1; echo rc=$?"#,
        r#"sh "$PTYRO_BIN" project trust app --yes 2>&1; echo rc=$?"#,
    ];
    // Its own POSIX session: the environment it was started with still names the session.
    if cfg!(target_os = "linux") {
        tries.push(r#"sh setsid -w "$PTYRO_BIN" send victim --raw 1 2>&1; echo rc=$?"#);
    }
    for (i, t) in tries.iter().enumerate() {
        w.ok(&["send", "intruder", t]);
        w.until(&format!("try {i} refused"), || w.screen("intruder").matches("rc=1").count() > i);
    }
    let screen = w.screen("intruder");
    assert_eq!(screen.matches("is an agent").count(), tries.len(), "{screen}");
    assert!(w.json(&["devices", "ls", "--json"]).as_array().unwrap().is_empty());
    // Keystrokes from a client running in an agent's pane get the same answer.
    w.ok(&["new", "-d", "--agent", "peek", "-n", "peek"]);
    w.wait_screen("peek", "victim");
    w.ok(&["send", "peek", "--raw", "1"]);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(status("victim"), "blocked");
    assert!(!w.screen("victim").contains("answer: yes"));
    // A person answers, from a plain shell.
    w.ok(&["send", "victim", "--raw", "1"]);
    w.wait_screen("victim", "answer: yes");
    w.until("victim idle", || status("victim") == "idle");
    // An idle agent may be typed into by another (a lead's `send`).
    w.ok(&[
        "send",
        "intruder",
        r#"sh "$PTYRO_BIN" send victim "work 0.1 from-intruder"; echo rc=$?"#,
    ]);
    w.wait_screen("victim", "from-intruder");
}

const RECIPE: &str = r#"
name = "ship-it"
vars = { task = { required = true } }

[[step]]
id = "impl"
spawn = { agent = "claude", worktree = "new", name = "impl" }
prompt = "{{vars.task}}"
done_when = { run = "test -f .second || (touch .second; exit 1)", max_bounces = 2 }
guard = { deny_cmds = ["git push"] }

[[step]]
id = "check"
needs = ["impl"]
run = "test -f fixed.txt && echo checked"
in = "impl"

[[step]]
id = "skipped"
needs = ["check"]
if = "steps.check.exit_code != 0"
run = "false"

[[step]]
id = "gate"
needs = ["check"]
gate = { ask = "Ship {{steps.impl.outputs.diffstat}}?", options = ["integrate", "discard"] }

[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'integrate'"
apply = { from = "impl", actions = ["commit", "integrate"], message = "Fix it ({{run.id}})" }
"#;

/// A recipe end to end: verified-done bounces the agent from its Stop hook until the check
/// passes, a run step checks the worktree, a condition skips a step, a gate waits for you, and
/// `apply` commits and integrates (only ptyro does that). Checkpoints follow every turn.
#[test]
fn recipe_with_verified_done_a_gate_and_safe_outputs() {
    let w = World::new("");
    let root = w.project("app");
    let file = w.home().join("ship.toml");
    std::fs::write(&file, RECIPE).unwrap();
    assert!(w.ok(&["recipe", "validate", file.to_str().unwrap()]).contains("ok (5 steps)"));
    assert!(w.ok(&["recipe", "graph", file.to_str().unwrap()]).contains("gate -.-> ship"));
    // A missing variable is refused up front.
    assert!(
        !w.cli(&["recipe", "run", file.to_str().unwrap(), "--project", "app"]).status.success()
    );
    let id = w
        .ok(&[
            "recipe",
            "run",
            file.to_str().unwrap(),
            "--project",
            "app",
            "--var",
            "task=edit fixed.txt done",
        ])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let g = w.open_gate();
    assert_eq!(g["run"], id.as_str());
    assert!(g["gate"]["ask"].as_str().unwrap().contains("file(s)"), "{g}");
    // Bounced once by the Stop hook, then verified.
    let screen = w.screen("impl");
    assert!(screen.contains("stop hook: [via ptyro: verify] Not done yet"), "{screen}");
    let st = w.run(&id)["state"].clone();
    assert_eq!(st["steps"]["impl"]["status"], "succeeded");
    assert_eq!(st["steps"]["impl"]["decided_by"], "done_when");
    assert_eq!(st["steps"]["impl"]["attempts"][0]["bounces"], 1);
    assert_eq!(st["steps"]["impl"]["outputs"]["verified"], true);
    assert_eq!(st["steps"]["check"]["outputs"]["exit_code"], 0);
    assert_eq!(st["steps"]["skipped"]["status"], "skipped");
    assert!(
        st["steps"]["impl"]["attempts"][0]["start_ref"]
            .as_str()
            .unwrap()
            .starts_with("refs/ptyro/runs/")
    );
    // The guard denies `git push` inside the step's session.
    w.ok(&["send", "impl", "sh git push origin main"]);
    w.wait_screen("impl", "denied: ptyro: this step's guard doesn't allow `git push`");
    // Checkpoints: one per turn that changed something.
    let ck = w.json(&["checkpoint", "ls", "impl", "--json"]);
    assert!(!ck.as_array().unwrap().is_empty(), "{ck}");
    // Answer the gate: the run commits and integrates.
    w.ok(&["gate", "resolve", g["gate"]["id"].as_str().unwrap(), "integrate"]);
    let run = w.wait_run(&id, "succeeded");
    assert_eq!(run["state"]["steps"]["ship"]["outputs"]["applied"][1], "integrate");
    assert_eq!(w.git(&root, &["show", "main:fixed.txt"]).trim(), "done");
    assert!(
        w.git(&root, &["log", "-1", "--format=%s", "main"]).contains(&format!("Fix it ({id})"))
    );
    // The event log has the story, in order.
    let ev = w.ok(&["run", "events", &id, "--json"]);
    for k in [
        "\"created\"",
        "\"bounced\"",
        "\"gate_opened\"",
        "\"gate_resolved\"",
        "\"intent\"",
        "\"outcome\"",
        "\"ended\"",
    ] {
        assert!(ev.contains(k), "{k} missing from\n{ev}");
    }
}

/// Runs are event-sourced: a daemon restart in the middle of a gate (and before a sleep) picks
/// the run up where it was, with the same gate id; a held step waits for `run retry`.
#[test]
fn runs_survive_a_daemon_restart() {
    let mut w = World::new("");
    w.project("app");
    let file = w.home().join("r.toml");
    std::fs::write(
        &file,
        "name = \"durable\"\n[[step]]\nid = \"a\"\nrun = \"echo first\"\n[[step]]\nid = \"g\"\nneeds = [\"a\"]\ngate = \"Continue?\"\n[[step]]\nid = \"nap\"\nneeds = [\"g\"]\nsleep = \"1s\"\n[[step]]\nid = \"b\"\nneeds = [\"nap\"]\nrun = \"echo second\"\n",
    )
    .unwrap();
    let id = w
        .ok(&["recipe", "run", file.to_str().unwrap(), "--project", "app"])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let gate = w.open_gate()["gate"]["id"].as_str().unwrap().to_owned();
    w.d.restart().unwrap();
    assert_eq!(w.run_status(&id), "waiting");
    assert_eq!(w.open_gate()["gate"]["id"], gate.as_str(), "the same gate after the restart");
    w.ok(&["gate", "resolve", &gate, "approve"]);
    let run = w.wait_run(&id, "succeeded");
    assert_eq!(
        run["state"]["steps"]["a"]["attempts"].as_array().unwrap().len(),
        1,
        "a done step never runs again"
    );
    assert_eq!(run["state"]["steps"]["b"]["outputs"]["exit_code"], 0);
    // A rejected gate fails the run; `run retry` gives the gate another chance.
    let id2 = w
        .ok(&["recipe", "run", file.to_str().unwrap(), "--project", "app"])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let g2 = w.open_gate()["gate"]["id"].as_str().unwrap().to_owned();
    w.ok(&["gate", "resolve", &g2, "reject"]);
    let r = w.wait_run(&id2, "failed");
    assert!(r["state"]["error"].as_str().unwrap().contains("step g"), "{r}");
    w.ok(&["run", "retry", &id2, "g"]);
    let g3 = w.open_gate()["gate"]["id"].as_str().unwrap().to_owned();
    assert_ne!(g3, g2);
    w.ok(&["gate", "resolve", &g3, "approve"]);
    w.wait_run(&id2, "succeeded");
}

/// Coordination between agents: leases deny edits to claimed files, the collision radar tells
/// both agents when their worktrees overlap, mail is delivered when an agent is idle (and seen),
/// checkpoints restore a worktree, and verified-done bounces an agent without a blocking Stop
/// hook (Codex) with a follow-up.
#[test]
fn leases_radar_mail_checkpoints_and_verify_by_follow_up() {
    let w = World::new("");
    w.project("app");
    w.ok(&["wt", "new", "a", "--project", "app"]);
    w.ok(&["wt", "new", "b", "--project", "app"]);
    w.ok(&["new", "-d", "--agent", "claude", "-n", "alice", "--wt", "app/a"]);
    w.ok(&["new", "-d", "--agent", "claude", "-n", "bob", "--wt", "app/b"]);
    w.wait_screen("alice", "fake claude");
    w.wait_screen("bob", "fake claude");
    // Alice leases the API file: Bob's edit of it is denied, with who and how to ask.
    let l = w.ok(&[
        "lease",
        "claim",
        "src/api.rs",
        "--exclusive",
        "--as",
        "alice",
        "--reason",
        "refactoring",
    ]);
    assert!(l.contains("src/api.rs") && l.contains("exclusive"), "{l}");
    assert_eq!(
        w.cli(&["lease", "claim", "src/**", "--as", "bob"]).status.code(),
        Some(5),
        "an overlapping claim is refused"
    );
    w.ok(&["send", "bob", "edit src/api.rs mine"]);
    let s = w.wait_screen("bob", "denied: ptyro: src/api.rs is leased by alice");
    assert!(s.contains("refactoring"), "{s}");
    w.ok(&["send", "bob", "edit src/other.rs fine"]);
    w.wait_screen("bob", "wrote src/other.rs");
    w.ok(&["lease", "release", "--as", "alice"]);
    // Both change the same file: the radar sees it, predicts the conflict and tells both.
    w.ok(&["send", "alice", "edit shared.txt from alice"]);
    w.wait_screen("alice", "wrote shared.txt");
    w.ok(&["send", "bob", "edit shared.txt from bob"]);
    w.wait_screen("bob", "wrote shared.txt");
    let radar = w.json(&["radar", "--project", "app", "--json"]);
    let o = &radar["overlaps"][0];
    assert!(o["files"].as_array().unwrap().iter().any(|f| f == "shared.txt"), "{radar:#}");
    assert!(o["conflicts"].as_array().unwrap().iter().any(|f| f == "shared.txt"), "{radar:#}");
    w.wait_screen("alice", "Collision radar");
    w.wait_screen("bob", "Collision radar");
    // Mail: queued, typed in once Bob is idle, and seen (his prompt hook carried the id).
    let sent =
        w.json(&["msg", "send", "bob", "please take src/other.rs", "--kind", "dispatch", "--json"]);
    let mid = sent["ids"][0].as_str().unwrap().to_owned();
    w.wait_screen("bob", &format!("<ptyro-message id={mid} from=@user kind=dispatch"));
    w.until("the message was seen", || {
        w.json(&["msg", "ls", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == mid.as_str() && (m["state"] == "seen" || m["state"] == "acked"))
    });
    // Checkpoints: the one that closed Alice's shared.txt turn has the file; restore it after a
    // scribble.
    let wt_a = w.json(&["wt", "status", "app/a", "--json"])["path"].as_str().unwrap().to_owned();
    let with_file = || -> Option<u64> {
        let ck = w.json(&["checkpoint", "ls", "alice", "--json"]);
        ck.as_array()?.iter().rev().find_map(|c| {
            let has = std::process::Command::new("git")
                .args([
                    "-C",
                    &wt_a,
                    "cat-file",
                    "-e",
                    &format!("{}:shared.txt", c["sha"].as_str()?),
                ])
                .status()
                .is_ok_and(|s| s.success());
            has.then(|| c["n"].as_u64()).flatten()
        })
    };
    w.until("a checkpoint with shared.txt", || with_file().is_some());
    let n = with_file().unwrap().to_string();
    std::fs::write(Path::new(&wt_a).join("shared.txt"), "scribbled\n").unwrap();
    let r = w.ok(&["checkpoint", "restore", "alice", &n, "--yes"]);
    assert!(r.contains("kept as refs/ptyro/ckpt/"), "{r}");
    assert_eq!(
        std::fs::read_to_string(Path::new(&wt_a).join("shared.txt")).unwrap(),
        "from alice\n"
    );
    // Verified-done by follow-up: Codex fails the check once, is told, tries again, passes.
    w.ok(&[
        "new",
        "-d",
        "--agent",
        "codex",
        "-n",
        "cx",
        "--wt",
        "app/b",
        "--verify",
        "test -f .ok || (touch .ok; exit 3)",
    ]);
    w.wait_screen("cx", "fake codex");
    w.ok(&["send", "cx", "work 0.2 hi"]);
    w.wait_screen("cx", "[via ptyro: verify] Not done yet");
    w.until("verified", || {
        w.json(&["status", "cx", "--json"])["session"]["verify"]["state"] == "passed"
    });
    let v = w.json(&["status", "cx", "--json"])["session"]["verify"].clone();
    assert_eq!(v["bounces"], 1);
    assert_eq!(v["last"]["exit_code"], 0);
}

/// The built-in `implement-and-review` recipe with fake agents that read prose: Claude
/// implements, Codex reviews in the same worktree and asks for changes (a typed verdict), the
/// follow-up goes back to Claude, and the gate ships it.
#[test]
fn builtin_implement_and_review_with_a_cross_vendor_reviewer() {
    let w = World::with_agents("", |a| match a {
        "codex" => "env = { PTYRO_FAKE_TURNS = \"1\", PTYRO_FAKE_VERDICT = \"changes\" }".into(),
        _ => "env = { PTYRO_FAKE_TURNS = \"1\" }".into(),
    });
    let root = w.project("app");
    let ls = w.ok(&["recipe", "ls", "--project", "app"]);
    for r in ["implement-and-review", "best-of-n", "test-fixer", "doc-writer", "overnight"] {
        assert!(ls.contains(r), "{ls}");
    }
    let id = w
        .ok(&[
            "recipe",
            "run",
            "implement-and-review",
            "--project",
            "app",
            "--var",
            "task=edit feature.txt v1",
            "--var",
            "test=true",
        ])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let g = w.open_gate();
    let st = w.run(&id)["state"].clone();
    assert_eq!(st["steps"]["review"]["outputs"]["output"]["decision"], "changes", "{st:#}");
    assert_eq!(st["steps"]["review"]["decided_by"], "output");
    assert_eq!(st["steps"]["fix"]["status"], "succeeded", "the issues went back to the writer");
    w.wait_screen("impl", "you said: - handle the empty case");
    // Same worktree: the reviewer worked where the writer did.
    assert_eq!(
        st["steps"]["review"]["attempts"][0]["worktree"],
        st["steps"]["impl"]["attempts"][0]["worktree"]
    );
    w.ok(&["gate", "resolve", g["gate"]["id"].as_str().unwrap(), "integrate"]);
    let run = w.wait_run(&id, "succeeded");
    assert_eq!(run["state"]["steps"]["pr"]["status"], "skipped");
    assert_eq!(w.git(&root, &["show", "main:feature.txt"]).trim(), "v1");
}

/// A judged fan-out: an advisory judge (another agent, in a clean directory with the diffs)
/// scores the candidates, and the ranking shows it.
#[test]
fn fanout_with_an_advisory_judge() {
    let w = World::new("");
    w.project("app");
    let id = w
        .ok(&[
            "fanout",
            "--agents",
            "claude,codex",
            "-p",
            "sh echo $PTYRO_STEP_ID > a.txt",
            "--project",
            "app",
            "--judge",
            "claude",
            "--test",
            "true",
        ])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    w.wait_run(&id, "succeeded");
    let doc = w.json(&["compare", &id, "--json"]);
    for c in doc["candidates"].as_array().unwrap() {
        assert_eq!(c["judge"], 6, "{doc:#}");
        assert!(c["reasons"].as_array().unwrap().iter().any(|r| r == "judge 6/10"), "{doc:#}");
    }
    assert!(
        doc["warnings"].as_array().unwrap().is_empty(),
        "two different agents, tests ran: {doc:#}"
    );
}

fn shape(v: &Value) -> Value {
    match v {
        // Known only once the agent reports it, which can be after a quick step ends.
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let s = if k == "conversation" { "string|null".into() } else { shape(v) };
                    (k.clone(), s)
                })
                .collect(),
        ),
        Value::Array(a) => match a.first() {
            Some(x) => Value::Array(vec![shape(x)]),
            None => Value::Array(vec![]),
        },
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Bool(_) => "bool".into(),
        Value::Null => Value::Null,
    }
}

/// The M7 `--json` shapes (docs/cli-json.md), pinned like M5's. `make snapshots` accepts changes.
#[test]
fn json_shapes() {
    let w = World::new("");
    w.project("app");
    let id = w
        .ok(&[
            "fanout",
            "--agents",
            "claude,codex",
            "-p",
            "sh echo $PTYRO_STEP_ID > a.txt",
            "--project",
            "app",
            "--test",
            "true",
            "--name",
            "j",
        ])
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let run = w.wait_run(&id, "succeeded");
    // The rank step ends just before the run does; it must stay "succeeded", never be
    // re-marked cancelled by the run's end.
    assert_eq!(run["state"]["steps"]["rank"]["status"], "succeeded", "{run:#}");
    let file = w.home().join("g.toml");
    std::fs::write(&file, "name = \"g\"\n[[step]]\nid = \"g\"\ngate = \"ok?\"\n").unwrap();
    w.ok(&["recipe", "run", file.to_str().unwrap(), "--project", "app"]);
    w.open_gate();
    // Checkpoints are written in the background after the turn.
    w.until("a checkpoint", || {
        !w.json(&["checkpoint", "ls", "j-claude", "--json"]).as_array().unwrap().is_empty()
    });
    w.ok(&["lease", "claim", "x.rs", "--as", "j-claude"]);
    w.ok(&["msg", "send", "j-claude", "hello"]);
    let mut all = serde_json::Map::new();
    for (name, args) in [
        ("run_ls", vec!["run", "ls", "--json"]),
        ("run_show", vec!["run", "show", &id, "--json"]),
        ("compare", vec!["compare", &id, "--json"]),
        ("gate_ls", vec!["gate", "ls", "--json"]),
        ("recipe_ls", vec!["recipe", "ls", "--project", "app", "--json"]),
        ("msg_ls", vec!["msg", "ls", "--json"]),
        ("lease_ls", vec!["lease", "ls", "--json"]),
        ("radar", vec!["radar", "--project", "app", "--json"]),
        ("checkpoint_ls", vec!["checkpoint", "ls", "j-claude", "--json"]),
    ] {
        all.insert(name.into(), shape(&w.json(&args)));
    }
    insta::assert_json_snapshot!("m7-cli-json", Value::Object(all));
}
