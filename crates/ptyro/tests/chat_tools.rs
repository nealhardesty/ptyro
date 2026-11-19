//! CHAT4 end-to-end (SPEC §22.5, §22.7; ROADMAP CHAT4) against a real daemon whose agents are the
//! fakes: a chat given a folder reads it and nothing else; a chat without one searches the web
//! and can't touch a file; an ACP agent's chat gets ptyro's MCP server in `session/new`; the tool
//! presets' old settings still load; and a
//! chat becomes a task with its context.

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
    /// `extra`: more config at the end.
    fn new(extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-chat4-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[chat]\ntitle_command = [\"sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n",
        );
        config.push_str(extra);
        for a in ["claude", "codex", "opencode"] {
            config.push_str(&format!(
                "\n[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        // An agent of the user's that speaks ACP.
        config.push_str(&format!(
            "\n[agents.acp]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"acp\", \"--acp\"]\nprotocol = \"acp\"\n"
        ));
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

    /// A new chat with `args`, answered: its row.
    fn chat(&self, args: &[&str]) -> Value {
        let mut a = vec!["chat", "new"];
        a.extend(args);
        self.ok(&a);
        let list = self.json(&["chat", "ls", "--json"]);
        list.as_array().unwrap().iter().max_by_key(|c| c["created_ms"].as_u64()).unwrap().clone()
    }

    /// The engine's command line of the chat `id`.
    fn argv(&self, id: &str) -> Vec<String> {
        let engines = self.json(&["ls", "--chats", "--json"]);
        let e = engines.as_array().unwrap().iter().find(|e| e["chat"] == id).expect("its engine");
        e["argv"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect()
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn after<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    argv.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
}

fn texts(w: &World, addr: &str) -> String {
    w.ok(&["chat", "show", addr])
}

#[test]
fn a_chat_reads_its_folder_and_one_without_only_searches() {
    let w = World::new("");
    let notes = w.home().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("plan.md"), "ship on friday\n").unwrap();
    // Given a folder (relative to here): it reads, that folder only.
    let c = w.chat(&["--agent", "claude", "--folder", "notes", "what is the plan?"]);
    let notes_s = notes.to_string_lossy().into_owned();
    assert_eq!(c["tools"], "chat");
    assert_eq!(c["folders"], serde_json::json!([notes_s]));
    let a = w.argv(c["id"].as_str().unwrap());
    assert_eq!(after(&a, "--tools"), ["WebSearch,WebFetch,Read,Glob,Grep"]);
    assert_eq!(after(&a, "--add-dir"), [notes_s.as_str()], "its folder, and nothing else");
    // No allow for the read tools: a bare allow reads anywhere (CHAT0).
    assert!(
        after(&a, "--allowedTools")[0].split(',').all(|t| !matches!(t, "Read" | "Glob" | "Grep"))
    );
    // A chat without a folder: web search, no file tool, no folder, no shell.
    let d = w.chat(&["--agent", "claude", "hello"]);
    assert_eq!((d["tools"].as_str(), d.get("folders")), (Some("chat"), None));
    let a = w.argv(d["id"].as_str().unwrap());
    assert_eq!(after(&a, "--tools"), ["WebSearch,WebFetch"]);
    assert!(after(&a, "--add-dir").is_empty() && !a.iter().any(|x| x.contains("Bash")));
    // Folders change later; with none it reads nothing (the next message starts it again).
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    w.ok(&["chat", "set", &addr, "--no-folders"]);
    w.ok(&["chat", "send", &addr, "and now?"]);
    w.until("restarted without the folder", || {
        after(&w.argv(c["id"].as_str().unwrap()), "--add-dir").is_empty()
    });
    // A folder that isn't there is refused, with the reason.
    let missing = if cfg!(windows) { r"C:\no\such\folder" } else { "/no/such/folder" };
    let out = w.cli(&["chat", "set", &addr, "--folder", missing]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no such folder"));
    // Codex: the chat's folder in its permission profile (best effort: outside $HOME).
    let x = w.chat(&["--agent", "codex", "--folder", &notes_s, "hi"]);
    assert_eq!(x["best_effort"], true);
    assert_eq!(d.get("best_effort"), None, "claude enforces it itself");
}

/// An ACP agent's chat: ptyro's MCP server arrives in `session/new`, in ACP's shape, and the
/// agent is told nothing it wouldn't understand.
#[test]
fn an_acp_agent_gets_ptyros_mcp_server_in_session_new() {
    let w = World::new("");
    // The fake answers `mcp` with the servers `session/new` brought it, checked for ACP's shape.
    let c = w.chat(&["--agent", "acp", "mcp"]);
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    w.until("its answer", || texts(&w, &addr).contains("mcp servers:"));
    assert!(texts(&w, &addr).contains("mcp servers: ptyro"), "{}", texts(&w, &addr));
    assert!(!texts(&w, &addr).contains("malformed"));
    let a = w.argv(c["id"].as_str().unwrap());
    assert!(after(&a, "--admin-policy").is_empty(), "{a:?}");
    assert_eq!(c["best_effort"], true, "nothing tells an ACP agent what it may use");
}

/// A config from before presets went (2026-11-17) still loads: its chats work as every chat
/// does, and `config check` names each old key with the command that takes it out.
#[test]
fn the_presets_old_settings_load_and_are_reported() {
    let w = World::new(
        "preset = \"read\"\nimport = true\n\n[chat.tools.ops]\nweb = true\nshell = true\nmcp = [\"github\"]\n",
    );
    let c = w.chat(&["--agent", "claude", "hi"]);
    assert_eq!(c["tools"], "chat");
    let a = w.argv(c["id"].as_str().unwrap());
    assert_eq!(after(&a, "--tools"), ["WebSearch,WebFetch"], "no shell: {a:?}");
    let check = w.cli(&["config", "check"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(check.status.success(), "{said}");
    for key in ["chat.preset", "chat.import", "chat.tools"] {
        assert!(said.contains(key) && said.contains("is no longer used"), "{key}: {said}");
    }
    assert!(said.contains("`ptyro config unset chat.preset`"), "{said}");
    // The command is gone too, and says so rather than asking an agent "presets".
    let out = w.cli(&["chat", "presets"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("`chat presets` was removed on 2026-11-17"), "{err}");
    let out = w.cli(&["chat", "fork", "c:abcd"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("retry and edit replace the last turn"), "{err}");
    // Retitling is rename's now: said, never a chat asking an agent "retitle c:abcd t".
    let out = w.cli(&["chat", "retitle", "c:abcd", "t"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("`chat retitle` was removed")
            && err.contains("`ptyro chat rename CHAT TITLE`"),
        "{err}"
    );
    assert!(w.json(&["chat", "ls", "--json"]).as_array().unwrap().len() == 1, "no chat made");
}

/// A repository with one commit.
fn repo(home: &Path, name: &str) -> PathBuf {
    let dir = home.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [&["init", "-q"][..], &["add", "."], &["commit", "-qm", "first"]] {
        if args[0] == "add" {
            std::fs::write(dir.join("README"), "hi\n").unwrap();
        }
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    dir
}

#[test]
fn a_chat_becomes_a_task_with_its_context() {
    let w = World::new("");
    let c = w.chat(&["--agent", "claude", "plan the retry policy"]);
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    w.until("its answer", || texts(&w, &addr).contains("you said: plan the retry policy"));
    // No project yet: the error says how to have one.
    let out = w.cli(&["chat", "task", &addr]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("add the repository"), "{out:?}");
    // Two projects: it asks which, naming them.
    let a = repo(&w.home(), "alpha");
    let b = repo(&w.home(), "beta");
    w.ok(&["project", "add", &a.to_string_lossy()]);
    w.ok(&["project", "add", &b.to_string_lossy()]);
    let out = w.cli(&["chat", "task", &addr]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Name one of") && err.contains("alpha") && err.contains("beta"), "{err}");
    let t = w.json(&["chat", "task", &addr, "--project", "alpha", "--json"]);
    assert_eq!(t["worktree"]["project_slug"], "alpha");
    let session = t["session"]["name"].as_str().expect("its agent started").to_owned();
    // Its first prompt, as the agent got it: the conversation, then the last answer to carry
    // out (the fake shows what was typed).
    let screen = || w.ok(&["read", &session]);
    w.until("the agent's first prompt", || screen().contains("Carry this out in this repository."));
    let screen = screen();
    let at = screen.find("The conversation so far, saved by ptyro").expect("the seed block");
    let prompt = &screen[at..];
    assert!(prompt.contains("[user] plan the retry policy"), "{screen}");
    assert!(prompt.contains("[assistant] you said: plan the retry policy"), "{screen}");
    assert!(prompt.contains(&format!("(chat {addr}, its last answer)")), "{screen}");
    // The chat says where it went.
    w.until("the chat's notice", || texts(&w, &addr).contains(&format!("session {session}")));
}

#[test]
fn a_task_keeps_incognito_and_profiles_apart() {
    let w = World::new("[profiles.work]\nlabel = \"work\"\n");
    let a = repo(&w.home(), "alpha");
    w.ok(&["project", "add", &a.to_string_lossy()]);
    // Incognito keeps nothing: it can't become a task, and the error offers the way out.
    let c = w.chat(&["--agent", "claude", "--incognito", "a secret plan"]);
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    w.until("its answer", || texts(&w, &addr).contains("you said: a secret plan"));
    let out = w.cli(&["chat", "task", &addr, "--project", "alpha"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("incognito") && err.contains("chat export"), "{err}");
    let wts = w.json(&["wt", "ls", "--json"]);
    assert!(
        wts.as_array().unwrap().iter().all(|t| t["name"] == "@root" || t["is_root"] == true),
        "no worktree made: {wts}"
    );
    // A chat of the default profile, and a project pinned to work: asked first.
    w.ok(&["project", "pin", "alpha", "work"]);
    let c = w.chat(&["--agent", "claude", "plan the retry policy"]);
    let addr = format!("c:{}", c["handle"].as_str().unwrap());
    assert_eq!(c["profile"], "default");
    w.until("its answer", || texts(&w, &addr).contains("you said: plan the retry policy"));
    let out = w.cli(&["chat", "task", &addr, "--project", "alpha"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("profile default") && err.contains("under work"), "{err}");
    assert!(!texts(&w, &addr).contains("made a task"), "nothing was made");
    // Agreed to: the task runs under the project's profile.
    let t = w.json(&["chat", "task", &addr, "--project", "alpha", "--yes", "--json"]);
    assert_eq!(t["worktree"]["project_slug"], "alpha");
    assert_eq!(t["worktree"]["profile"], "work", "{t}");
}
