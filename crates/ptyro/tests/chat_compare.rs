//! CHAT5's compare end-to-end (SPEC §22.7; ROADMAP CHAT5) against a real daemon whose agents are
//! the fakes: claude, codex and gemini answer one message as three siblings, each labelled with
//! its agent; picking one switches the chat to that agent; an agent that fails doesn't stop the
//! others; stop stops them all; and no helper is ever listed or left behind.

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(40);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    /// Every agent is the fake, but those in `broken`, which exit at once.
    fn new(broken: &[&str]) -> Self {
        Self::with(broken, &[])
    }

    /// The same, and those in `missing` aren't installed.
    fn with(broken: &[&str], missing: &[&str]) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-compare-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n\
             [profiles.work]\nlabel = \"work\"\n[profiles.personal]\nlabel = \"personal\"\n",
        );
        for a in ["claude", "codex", "gemini", "opencode"] {
            if missing.contains(&a) {
                config.push_str(&format!(
                    "\n[agents.{a}]\ncommand = [\"/nonexistent/ptyro-test/{a}\"]\n"
                ));
            } else if broken.contains(&a) {
                config.push_str(&format!(
                    "\n[agents.{a}]\ncommand = [\"/bin/sh\", \"-c\", \"echo not logged in >&2; exit 3\"]\n"
                ));
            } else {
                config.push_str(&format!(
                    "\n[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
                ));
            }
        }
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("ptyro {args:?}: {e}\n{out}"))
    }

    /// A chat on claude, answered once: its address.
    fn chat(&self) -> String {
        self.ok(&["chat", "new", "--agent", "claude", "hello"]);
        let list = self.json(&["chat", "ls", "--json"]);
        let c = &list.as_array().unwrap()[0];
        format!("c:{}", c["handle"].as_str().unwrap())
    }

    fn read(&self, addr: &str) -> Value {
        self.json(&["chat", "show", addr, "--all", "--format", "json"])
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// The chats' engines that run now.
    fn engines(&self) -> Vec<Value> {
        self.json(&["ls", "--chats", "--json"]).as_array().cloned().unwrap_or_default()
    }

    /// Nothing of the helpers is left: one chat listed, no scratch folder but the chat's.
    fn no_helpers_left(&self, id: &str) {
        assert_eq!(self.json(&["chat", "ls", "--json"]).as_array().unwrap().len(), 1);
        self.until("the helpers' engines are gone", || {
            self.engines().iter().all(|e| e["chat"].as_str() == Some(id))
        });
        let dirs: Vec<String> = std::fs::read_dir(self.d.paths.data.join("chat"))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        assert!(dirs.iter().all(|d| d == id), "only the chat's own scratch folder: {dirs:?}");
    }
}

fn agents_of(chat: &Value, message: &str) -> Vec<String> {
    // The siblings under the message: each with its agent.
    let sib = chat["siblings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            let first = s["ids"][0].as_str().unwrap();
            chat["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["id"] == first && n["parent"] == message)
        })
        .cloned()
        .unwrap_or(Value::Null);
    let mut a: Vec<String> = sib["agents"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    a.sort();
    a
}

#[test]
fn three_agents_answer_as_labelled_branches_and_a_pick_switches_the_agent() {
    let w = World::new(&[]);
    let addr = w.chat();
    let out = w.json(&[
        "chat",
        "compare",
        &addr,
        "--agents",
        "claude,codex,gemini",
        "--json",
        "what is a monad?",
    ]);
    let answers = out["answers"].as_array().unwrap();
    let mut who: Vec<&str> = answers.iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude", "codex", "gemini"], "{out}");
    for a in answers {
        assert!(a["text"].as_str().unwrap().contains("what is a monad?"), "{a}");
    }
    // The others were told the conversation so far (seeded); the chat's own agent carried on.
    let codex = answers.iter().find(|a| a["agent"] == "codex").unwrap();
    assert!(codex["text"].as_str().unwrap().contains("The conversation so far"));
    let claude = answers.iter().find(|a| a["agent"] == "claude").unwrap();
    assert!(!claude["text"].as_str().unwrap().contains("The conversation so far"));
    assert!(out["agents"].as_array().unwrap().iter().all(|a| a["state"] == "done"), "{out}");
    let message = out["message"].as_str().unwrap().to_owned();
    // Three labelled siblings in the tree, the navigator's label in `show --branches`.
    let chat = w.read(&addr);
    assert_eq!(agents_of(&chat, &message), ["claude", "codex", "gemini"]);
    let shown = w.ok(&["chat", "show", &addr, "--branches"]);
    assert!(shown.contains("‹ 1/3 claude ›"), "{shown}");
    assert!(shown.contains("compared: claude ✓"), "{shown}");
    let id = chat["chat"]["id"].as_str().unwrap().to_owned();
    w.no_helpers_left(&id);
    // Keep codex's answer: its branch is shown, and the chat is codex's from now on.
    let node = codex["node"].as_str().unwrap();
    w.ok(&["chat", "pick", &addr, &node[node.len() - 8..]]);
    let info = w.read(&addr)["chat"].clone();
    assert_eq!(info["agent"], "codex");
    let shown = w.ok(&["chat", "show", &addr, "--branches"]);
    assert!(shown.contains("codex ›"), "{shown}");
    assert!(shown.contains("kept codex's answer"), "{shown}");
    // The next message goes to codex, seeded with that branch.
    let reply = w.ok(&["chat", "send", &addr, "and a functor?"]);
    assert!(reply.contains("you said:") && reply.contains("and a functor?"), "{reply}");
    let e = w.engines();
    let argv = e.iter().find(|e| e["chat"] == id.as_str()).unwrap()["argv"].to_string();
    assert!(argv.contains("codex"), "{argv}");
    // Asked again with no text: only the agents that haven't answered it.
    let err = w.cli(&["chat", "compare", &addr, "--agents", "codex"]);
    assert!(!err.status.success());
    assert!(String::from_utf8_lossy(&err.stderr).contains("answered it already"));
}

#[test]
fn a_failing_agent_does_not_stop_the_others() {
    let w = World::new(&["gemini"]);
    let addr = w.chat();
    let out = w.json(&[
        "chat",
        "compare",
        &addr,
        "--agents",
        "claude,codex,gemini",
        "--json",
        "name a colour",
    ]);
    let mut who: Vec<&str> =
        out["answers"].as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude", "codex"], "{out}");
    let gemini =
        out["agents"].as_array().unwrap().iter().find(|a| a["agent"] == "gemini").unwrap().clone();
    assert_eq!(gemini["state"], "failed", "{out}");
    let shown = w.ok(&["chat", "show", &addr]);
    assert!(shown.contains("gemini ✗"), "{shown}");
    let id = w.read(&addr)["chat"]["id"].as_str().unwrap().to_owned();
    w.no_helpers_left(&id);
    // The chat is free again.
    let reply = w.ok(&["chat", "send", &addr, "thanks"]);
    assert!(reply.contains("you said: thanks"), "{reply}");
}

#[test]
fn an_agent_that_isnt_installed_is_a_cross_and_the_others_answer() {
    let w = World::with(&[], &["codex"]);
    let addr = w.chat();
    let out = w.json(&[
        "chat",
        "compare",
        &addr,
        "--agents",
        "claude,codex,gemini",
        "--json",
        "name a fruit",
    ]);
    let mut who: Vec<&str> =
        out["answers"].as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude", "gemini"], "{out}");
    let codex =
        out["agents"].as_array().unwrap().iter().find(|a| a["agent"] == "codex").unwrap().clone();
    assert_eq!(codex["state"], "failed", "{out}");
    assert!(codex["note"].as_str().unwrap().contains("isn't installed"), "{out}");
    let shown = w.ok(&["chat", "show", &addr]);
    assert!(shown.contains("codex ✗"), "{shown}");
    // A name no agent has is still refused.
    let bad = w.cli(&["chat", "compare", &addr, "--agents", "claude,nosuch", "x"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("no agent named"), "{bad:?}");
}

#[test]
fn stop_stops_every_agent_and_a_compare_is_busy_until_then() {
    let w = World::new(&[]);
    let addr = w.chat();
    // Neither is the chat's own agent: both answer in helpers, slowly.
    w.ok(&["chat", "compare", &addr, "--agents", "codex,gemini", "--no-wait", "work 60 slowly"]);
    let comparing = || !w.read(&addr)["chat"]["comparing"].is_null();
    assert!(comparing(), "{}", w.ok(&["chat", "show", &addr]));
    // One answer at a time: a message waits for the compare.
    let busy = w.cli(&["chat", "send", &addr, "--no-wait", "meanwhile"]);
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("comparing"));
    // Nor is an answer kept, or the chat made a task, mid-compare.
    for args in [["chat", "pick", addr.as_str()], ["chat", "task", addr.as_str()]] {
        let out = w.cli(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("comparing"), "{args:?}: {out:?}");
    }
    w.until("both helpers run", || w.engines().len() >= 2);
    w.ok(&["chat", "stop", &addr]);
    w.until("the compare is over", || !comparing());
    let shown = w.ok(&["chat", "show", &addr]);
    assert!(shown.contains("codex ⊘") && shown.contains("gemini ⊘"), "{shown}");
    let id = w.read(&addr)["chat"]["id"].as_str().unwrap().to_owned();
    w.no_helpers_left(&id);
}

#[test]
fn a_new_question_goes_to_several_agents_and_the_first_answer_is_shown() {
    let w = World::new(&[]);
    let addr = w.chat();
    // The chat's own agent isn't asked: the first answer in is the one shown.
    let out = w.json(&["chat", "compare", &addr, "--agents", "codex,gemini", "--json", "hi both"]);
    assert_eq!(out["answers"].as_array().unwrap().len(), 2, "{out}");
    let chat = w.read(&addr);
    let leaf = chat["chat"]["leaf"].as_str().unwrap();
    let nodes = chat["nodes"].as_array().unwrap();
    let tip = nodes.iter().find(|n| n["id"] == leaf).unwrap();
    assert_eq!(tip["t"], "notice", "the outcome closes the shown branch: {tip}");
    assert_eq!(agents_of(&chat, out["message"].as_str().unwrap()), ["codex", "gemini"]);
    // A compare needs two agents.
    let one = w.cli(&["chat", "compare", &addr, "--agents", "codex", "x"]);
    assert!(!one.status.success());
    // A new chat's first message, to two agents at once.
    let out = w.json(&["chat", "new", "--compare", "claude,gemini", "--json", "first of all"]);
    let mut who: Vec<&str> =
        out["answers"].as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude", "gemini"], "{out}");
    assert_eq!(w.json(&["chat", "ls", "--json"]).as_array().unwrap().len(), 2);
}

#[test]
fn a_compare_keeps_to_the_chats_profile() {
    let w = World::new(&[]);
    // A chat of profile `personal` in a workspace of profile `work`.
    w.ok(&["ws", "new", "job", "--profile", "work"]);
    w.ok(&["chat", "new", "-w", "job", "--agent", "claude@personal", "hello"]);
    let list = w.json(&["chat", "ls", "--json"]);
    let c = &list.as_array().unwrap()[0];
    assert_eq!(c["profile"], "personal", "{c}");
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    // Bare names are the chat's profile's, never the workspace's: its history stays there.
    let out = w.json(&["chat", "compare", &addr, "--agents", "claude,codex", "--json", "q one"]);
    let mut who: Vec<&str> =
        out["answers"].as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude@personal", "codex@personal"], "{out}");
    // `claude` is the chat's own agent: it carried on in its conversation, unseeded.
    let claude =
        out["answers"].as_array().unwrap().iter().find(|a| a["agent"] == "claude@personal");
    assert!(!claude.unwrap()["text"].as_str().unwrap().contains("The conversation so far"));
    // Another profile, named: asked first (no terminal here, so refused with the flag to pass).
    let other = w.cli(&["chat", "compare", &addr, "--agents", "claude,codex@work", "q two"]);
    assert!(!other.status.success());
    let err = String::from_utf8_lossy(&other.stderr);
    assert!(
        err.contains("codex@work")
            && err.contains("another profile")
            && err.contains("--yes")
            && !err.contains("override-pin"),
        "{err}"
    );
    assert_eq!(w.read(&addr)["chat"]["comparing"], Value::Null, "nothing was asked");
    // Agreed to: it goes there.
    let out = w.json(&[
        "chat",
        "compare",
        &addr,
        "--agents",
        "claude,codex@work",
        "--yes",
        "--json",
        "q two",
    ]);
    let mut who: Vec<&str> =
        out["answers"].as_array().unwrap().iter().map(|a| a["agent"].as_str().unwrap()).collect();
    who.sort();
    assert_eq!(who, ["claude@personal", "codex@work"], "{out}");
}

#[test]
fn another_agent_or_a_fork_keeps_to_the_chats_profile_and_asks_before_another() {
    let w = World::new(&[]);
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
    let out = w.cli(&["chat", "set", &addr, "--agent", "claude@work", "--preset", "none"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another profile") && err.contains("--yes"), "{err}");
    assert_eq!(who(&w, &addr), ("codex".into(), "personal".into()));
    assert_eq!(w.read(&addr)["chat"]["tools"], "chat", "nothing of it was applied");
    w.ok(&["chat", "set", &addr, "--agent", "claude@work", "--yes"]);
    assert_eq!(who(&w, &addr), ("claude".into(), "work".into()));
    // A fork: the same.
    let out = w.cli(&["chat", "fork", &addr, "--agent", "codex@personal"]);
    assert!(!out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = w.json(&["chat", "fork", &addr, "--agent", "codex", "--json"]);
    let out = &out["chat"];
    assert_eq!((out["agent"].as_str(), out["profile"].as_str()), (Some("codex"), Some("work")));
}

#[test]
fn a_compare_of_one_agent_twice_is_refused_before_a_chat_is_made() {
    let w = World::new(&[]);
    let asked: [&[&str]; 3] = [
        &["--agent", "claude", "--compare", "claude,claude"],
        &["--agent", "claude", "--compare", "claude,claude@default"],
        &["--compare", "claude,claude"],
    ];
    for agents in asked {
        // Refused at once: never a wait for an answer nobody was asked for.
        let t = Instant::now();
        let out = w.cli(&[&["chat", "new"], agents, &["hi"]].concat());
        assert!(t.elapsed() < Duration::from_secs(20), "{agents:?}: {:?}", t.elapsed());
        assert!(!out.status.success(), "{agents:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("two different agents"), "{out:?}");
    }
    assert!(w.json(&["chat", "ls", "--json"]).as_array().unwrap().is_empty(), "no chat made");
}

#[test]
fn an_opencode_chat_answers_alone_and_in_a_compare() {
    // Its server says only to who is listening: a prompt sent before ptyro was left the chat
    // answering for good (a compare waited out the helper's 10 minutes).
    let w = World::new(&[]);
    let t = Instant::now();
    let out =
        w.json(&["chat", "new", "--agent", "opencode", "--json", "--timeout", "30", "on its own"]);
    let texts = |v: &Value| -> Vec<String> {
        v["answers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["text"].as_str().unwrap_or("").to_owned())
            .collect()
    };
    assert!(texts(&out).iter().any(|t| t.contains("on its own")), "{out}");
    let out = w.json(&[
        "chat",
        "new",
        "--compare",
        "claude,opencode",
        "--json",
        "--timeout",
        "30",
        "both of you",
    ]);
    let answered = texts(&out);
    assert_eq!(answered.len(), 2, "{out}");
    assert!(answered.iter().all(|t| t.contains("both of you")), "{out}");
    assert!(t.elapsed() < Duration::from_secs(30), "took {:?}", t.elapsed());
}
