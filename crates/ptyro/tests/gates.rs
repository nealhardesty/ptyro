//! Gates end to end (SPEC §15): the first answer to a gate stands whoever races, a repeat with
//! the same key is a no-op, a wrong choice is refused with the valid ones, and a gate of a run
//! that has been cancelled can no longer be answered. Gates are opened by a lead agent's
//! `request_gate`; every agent is `ptyro fake-agent`.

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
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-gates-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n"
        );
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
        assert!(!out.status.success(), "ptyro {args:?} should fail");
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(out.status.success(), "ptyro {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_screen(&self, s: &str, needle: &str) {
        self.until(&format!("{s} shows {needle:?}"), || {
            self.ok(&["read", s, "--scrollback", "400"]).contains(needle)
        });
    }

    /// A lead session in a project, and the run it made.
    fn lead(&self, name: &str) -> String {
        let root = self.home().join("app");
        if !root.exists() {
            self.git(&self.home(), &["init", "-q", "app"]);
            std::fs::write(root.join("f.txt"), "one\n").unwrap();
            self.git(&root, &["add", "-A"]);
            self.git(&root, &["commit", "-q", "-m", "init"]);
            self.ok(&["project", "add", root.to_str().unwrap()]);
        }
        self.ok(&[
            "new",
            "-d",
            "--agent",
            "claude",
            "--lead",
            "-n",
            name,
            "-C",
            root.to_str().unwrap(),
        ]);
        self.wait_screen(name, "fake claude");
        self.until("the lead's run", || self.run_of(name).is_some());
        self.run_of(name).unwrap()
    }

    fn run_of(&self, _lead: &str) -> Option<String> {
        // One lead per world.
        self.json(&["run", "ls", "--json"]).as_array()?.iter().find_map(|r| {
            (r["state"]["kind"] == "lead").then(|| r["state"]["id"].as_str().unwrap().to_owned())
        })
    }

    /// Ask the human something as the lead; returns the gate's id once it is open.
    fn ask(&self, lead: &str, question: &str) -> String {
        let before = self.open_gates().len();
        self.ok(&[
            "send",
            lead,
            &format!(r#"mcp request_gate {{"question":"{question}","options":["yes","no"]}}"#),
        ]);
        self.until("the gate opens", || self.open_gates().len() > before);
        self.open_gates()
            .into_iter()
            .find(|g| g["gate"]["ask"].as_str().is_some_and(|a| a.contains(question)))
            .unwrap()["gate"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn open_gates(&self) -> Vec<Value> {
        self.json(&["gate", "ls", "--json"]).as_array().cloned().unwrap_or_default()
    }

    fn gate_choice(&self, run: &str, id: &str) -> Option<String> {
        let st = self.json(&["run", "show", run, "--json"])["state"].clone();
        st["steps"].as_object()?.values().find_map(|s| {
            (s["gate"]["id"] == id).then(|| s["gate"]["choice"].as_str().map(str::to_owned))?
        })
    }
}

/// Many answers at once, different choices: exactly one is recorded and the rest are told it
/// was answered already. The same idempotency key again is the same answer, never a second one.
#[test]
fn the_first_answer_to_a_gate_stands_when_several_race() {
    let w = World::new();
    let run = w.lead("boss");
    let id = w.ask("boss", "ship it?");
    let results: Vec<(&str, bool, String)> = std::thread::scope(|sc| {
        let hs: Vec<_> = ["yes", "no", "yes", "no", "yes", "no"]
            .into_iter()
            .map(|choice| {
                let (w, id) = (&w, &id);
                sc.spawn(move || {
                    let out = w.cli(&["gate", "resolve", id, choice]);
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    (choice, out.status.success(), text)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winner = w.gate_choice(&run, &id).expect("one answer was recorded");
    for (choice, ok, text) in &results {
        if *choice == winner {
            assert!(ok, "the same answer as the winner is a no-op: {text}");
        } else {
            assert!(!ok && text.contains("already answered"), "{choice}: {text}");
        }
    }
    // It stays what it was.
    assert_eq!(w.gate_choice(&run, &id).as_deref(), Some(winner.as_str()));
    // The lead heard exactly the winner.
    w.wait_screen("boss", &format!("\"choice\": \"{winner}\""));
}

/// A choice that isn't an option is refused and names the options; the gate stays open and a
/// later valid answer (in any case) goes through. An unknown gate is not found.
#[test]
fn a_wrong_choice_is_refused_and_leaves_the_gate_open() {
    let w = World::new();
    let run = w.lead("boss");
    let id = w.ask("boss", "deploy?");
    let text = w.fails(&["gate", "resolve", &id, "maybe"]);
    assert!(text.contains("yes, no"), "{text}");
    assert_eq!(w.gate_choice(&run, &id), None);
    assert!(w.open_gates().iter().any(|g| g["gate"]["id"] == id.as_str()));
    w.ok(&["gate", "resolve", &id, "YES", "--key", "tap-1"]);
    assert_eq!(w.gate_choice(&run, &id).as_deref(), Some("yes"));
    // A resend with the same key, even with another choice, changes nothing and is not an error.
    w.ok(&["gate", "resolve", &id, "no", "--key", "tap-1"]);
    assert_eq!(w.gate_choice(&run, &id).as_deref(), Some("yes"));
    assert!(w.fails(&["gate", "resolve", "g-nope", "yes"]).contains("g-nope"));
}

/// Cancelling the run closes its gate: answering it afterwards is refused, and nothing is
/// recorded, so a late tap on a phone can't reopen work that was stopped.
#[test]
fn a_gate_of_a_cancelled_run_cannot_be_answered() {
    let w = World::new();
    let run = w.lead("boss");
    let id = w.ask("boss", "still wanted?");
    w.ok(&["run", "cancel", &run]);
    w.until("the run is over", || {
        let s = w.json(&["run", "show", &run, "--json"])["state"]["status"].clone();
        matches!(s.as_str(), Some("cancelled" | "failed" | "succeeded"))
    });
    let text = w.fails(&["gate", "resolve", &id, "yes"]);
    assert!(text.contains("already ended") || text.contains("nothing left"), "{text}");
    assert_eq!(w.gate_choice(&run, &id), None);
}
