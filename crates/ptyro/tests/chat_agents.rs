//! A chat's agent against a real daemon whose agents are the fakes: one that isn't installed
//! offers setup, not the OS's error, and switching a chat's agent keeps to its profile, asking
//! before another (SPEC §22.7).

use std::path::PathBuf;
use std::process::Output;

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    /// Every agent is the fake.
    fn new() -> Self {
        Self::with(&[])
    }

    /// The same, but those in `missing` aren't installed.
    fn with(missing: &[&str]) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-chat-agents-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[chat]\ntitle_command = [\"sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n\
             [profiles.work]\nlabel = \"work\"\n[profiles.personal]\nlabel = \"personal\"\n",
        );
        for a in ["claude", "codex", "gemini", "opencode"] {
            if missing.contains(&a) {
                config.push_str(&format!(
                    "\n[agents.{a}]\ncommand = [\"/nonexistent/ptyro-test/{a}\"]\n"
                ));
            } else {
                config.push_str(&format!(
                    "\n[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n"
                ));
            }
        }
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("ptyro {args:?}: {e}\n{out}"))
    }

    fn read(&self, addr: &str) -> Value {
        self.json(&["chat", "show", addr, "--format", "json"])
    }
}

#[test]
fn a_missing_agent_offers_setup_not_the_os_error() {
    let w = World::with(&["claude", "codex", "gemini", "opencode"]);
    let out = w.cli(&["chat", "new", "hello"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("No agent that can chat is installed: `ptyro setup` installs one"),
        "{err}"
    );
    assert!(err.contains("Antigravity, for a Google account, runs in sessions but can't chat"));
    assert!(!err.contains("npm i -g"), "a command to copy, no longer: {err}");
    // A session of an agent that isn't there: what it is, not "No such file or directory".
    let out = w.cli(&["new", "-d", "--agent", "codex"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("codex isn't installed (/nonexistent/ptyro-test/codex is nowhere ptyro looks): install it, or set [agents.codex] command to its full path"),
        "{err}"
    );
}

#[test]
fn another_agent_keeps_to_the_chats_profile_and_asks_before_another() {
    let w = World::new();
    w.ok(&["ws", "new", "job", "--profile", "work"]);
    w.ok(&["chat", "new", "-w", "job", "--agent", "claude@personal", "hello"]);
    let c = w.json(&["chat", "ls", "--json"]).as_array().unwrap()[0].clone();
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    let who = |w: &World, addr: &str| {
        let c = w.read(addr)["chat"].clone();
        (c["agent"].as_str().unwrap().to_owned(), c["profile"].as_str().unwrap().to_owned())
    };
    // A bare name is the chat's profile's, not the workspace's (`work`).
    w.ok(&["chat", "set", &addr, "--agent", "codex"]);
    assert_eq!(who(&w, &addr), ("codex".into(), "personal".into()));
    // Another profile, named: asked first, and refused changes nothing (no terminal here).
    let out = w.cli(&["chat", "set", &addr, "--agent", "claude@work", "--no-folders"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another profile") && err.contains("--yes"), "{err}");
    assert_eq!(who(&w, &addr), ("codex".into(), "personal".into()));
    w.ok(&["chat", "set", &addr, "--agent", "claude@work", "--yes"]);
    assert_eq!(who(&w, &addr), ("claude".into(), "work".into()));
}
