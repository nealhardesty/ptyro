//! CHAT5's import end-to-end (SPEC §22.10; ROADMAP CHAT5): hand-written histories of Claude
//! Code, Codex and Gemini in a temporary home (in their real shapes, nothing copied from a real
//! one) become read-only chats; search finds them; importing again changes nothing; a grown
//! session gains only what is new; a message to an imported chat continues it as a fork;
//! ptyro's own engines are left out; a dry run writes nothing; a deleted one stays deleted; and
//! `[chat] import` keeps doing it by itself.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant, SystemTime};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(40);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const CLAUDE_ID: &str = "5b1d7a52-0000-4000-8000-00000000c1a0";
const CODEX_ID: &str = "01a17172-da66-7120-83a8-564b2ed106e4";

fn claude_session(cwd: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"{CLAUDE_ID}","cwd":"{cwd}","timestamp":"2026-10-20T09:00:00.000Z","isSidechain":false,"message":{{"role":"user","content":"explain a monad like I am five"}}}}
{{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"{CLAUDE_ID}","cwd":"{cwd}","timestamp":"2026-10-20T09:00:05.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"WebSearch","input":{{"query":"monad for kids"}}}}]}}}}
{{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"{CLAUDE_ID}","cwd":"{cwd}","timestamp":"2026-10-20T09:00:06.000Z","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":"results"}}]}}}}
{{"type":"assistant","uuid":"a2","parentUuid":"u2","sessionId":"{CLAUDE_ID}","cwd":"{cwd}","timestamp":"2026-10-20T09:00:09.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"text","text":"A monad is a lunchbox for values."}}]}}}}
{{"type":"custom-title","customTitle":"Monads for kids","sessionId":"{CLAUDE_ID}"}}
"#
    )
}

fn claude_more() -> String {
    format!(
        r#"{{"type":"user","uuid":"u3","parentUuid":"a2","sessionId":"{CLAUDE_ID}","timestamp":"2026-10-20T09:10:00.000Z","isSidechain":false,"message":{{"role":"user","content":"and a burrito?"}}}}
{{"type":"assistant","uuid":"a3","parentUuid":"u3","sessionId":"{CLAUDE_ID}","timestamp":"2026-10-20T09:10:04.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"text","text":"Burritos wrap fillings the same way."}}]}}}}
"#
    )
}

fn codex_session() -> String {
    format!(
        r#"{{"timestamp":"2026-10-21T05:05:24.025Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/tmp/elsewhere","originator":"codex_cli_rs"}}}}
{{"timestamp":"2026-10-21T05:05:24.358Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>"}}]}}}}
{{"timestamp":"2026-10-21T05:05:25.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"what does the zebra striping in tables help with?"}}]}}}}
{{"timestamp":"2026-10-21T05:05:28.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Zebra stripes guide the eye along rows."}}]}}}}
"#
    )
}

const GEMINI: &str = r#"{"sessionId":"9e3b0c44-0000-4000-8000-00000000ge01","projectHash":"abc","startTime":"2026-10-22T08:00:00.000Z","lastUpdated":"2026-10-22T08:00:09.000Z","kind":"main"}
{"id":"g-m1","timestamp":"2026-10-22T08:00:01.000Z","type":"user","content":[{"text":"suggest a name for a walrus"}]}
{"id":"g-m2","timestamp":"2026-10-22T08:00:04.000Z","type":"gemini","content":"How about Wally the walrus?"}
{"$set":{"summary":"Walrus names"}}
"#;

impl World {
    fn new(extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-import-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        // The histories, as each agent leaves them.
        let repo = h.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let slug = repo.to_string_lossy().replace('/', "-");
        write(
            &h.join(".claude/projects").join(&slug).join(format!("{CLAUDE_ID}.jsonl")),
            &claude_session(&repo.to_string_lossy()),
        );
        write(
            &h.join(format!(
                ".codex/sessions/2026/10/21/rollout-2026-10-21T05-05-14-{CODEX_ID}.jsonl"
            )),
            &codex_session(),
        );
        write(
            &h.join(".codex/session_index.jsonl"),
            &format!(
                "{{\"id\":\"{CODEX_ID}\",\"thread_name\":\"Zebra striping\",\"updated_at\":\"x\"}}\n"
            ),
        );
        write(&h.join(".gemini/tmp/abc/.project_root"), &repo.to_string_lossy());
        write(&h.join(".gemini/tmp/abc/chats/session-2026-10-22T08-00-9e3b0c44.jsonl"), GEMINI);
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let mut config = String::from(
            "[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n",
        );
        config.push_str(extra);
        for a in ["claude", "codex", "gemini", "opencode"] {
            config.push_str(&format!(
                "\n[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
            ));
        }
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn claude_file(&self) -> PathBuf {
        let slug = self.home().join("repo").to_string_lossy().replace('/', "-");
        self.home().join(".claude/projects").join(slug).join(format!("{CLAUDE_ID}.jsonl"))
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

    fn imported(&self) -> Vec<Value> {
        self.json(&["chat", "ls", "--imported", "--json"]).as_array().cloned().unwrap_or_default()
    }

    fn by_source(&self, prefix: &str) -> Value {
        self.imported()
            .into_iter()
            .find(|c| c["source"].as_str().is_some_and(|s| s.starts_with(prefix)))
            .unwrap_or_else(|| panic!("no chat from {prefix}"))
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// `(new, grown, unchanged, skipped, messages)` of one agent's line of a report.
fn counts(report: &Value, agent: &str) -> (u64, u64, u64, u64, u64) {
    let s = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["agent"] == agent && s["profile"] == "default")
        .unwrap_or_else(|| panic!("no {agent} in {report}"));
    let n = |k: &str| s[k].as_u64().unwrap_or(0);
    (n("new"), n("grown"), n("unchanged"), n("skipped"), n("messages"))
}

#[test]
fn histories_become_searchable_read_only_chats() {
    let w = World::new("");
    // One of ptyro's own chat engines left a Claude session: it isn't history.
    let own = w.d.paths.data.join("chat").join("01own");
    write(
        &w.home().join(".claude/projects/-own").join("own-1.jsonl"),
        &format!(
            r#"{{"type":"user","uuid":"o1","sessionId":"own-1","cwd":"{}","timestamp":"2026-10-20T09:00:00Z","message":{{"role":"user","content":"a chat engine's turn"}}}}
"#,
            own.to_string_lossy()
        ),
    );
    // A dry run counts, and writes nothing.
    let dry = w.json(&["chat", "import", "--dry-run", "--json"]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(counts(&dry, "claude").0, 1, "{dry}");
    assert_eq!(counts(&dry, "claude").3, 1, "ptyro's own engine is skipped: {dry}");
    assert_eq!(counts(&dry, "codex").0, 1);
    assert_eq!(counts(&dry, "gemini").0, 1);
    assert!(dry["notes"].to_string().contains("opencode"), "{dry}");
    assert!(w.imported().is_empty(), "a dry run writes nothing");
    // The import: three chats, titled as their agents titled them, marked as imported.
    let r = w.json(&["chat", "import", "--json"]);
    assert_eq!(counts(&r, "claude"), (1, 0, 0, 1, 3), "{r}");
    assert_eq!(counts(&r, "codex").0, 1);
    let claude = w.by_source("claude:");
    assert_eq!(claude["title"], "Monads for kids");
    assert_eq!(claude["source"], format!("claude:{CLAUDE_ID}"));
    assert_eq!(claude["source_dir"], w.home().join("repo").to_string_lossy().as_ref());
    assert_eq!(claude["agent"], "claude");
    assert_eq!(w.by_source("codex:")["title"], "Zebra striping");
    assert_eq!(w.by_source("gemini:")["title"], "Walrus names");
    let addr = format!("c:{}", claude["handle"].as_str().unwrap());
    let shown = w.ok(&["chat", "show", &addr]);
    assert!(shown.contains("imported from claude"), "{shown}");
    assert!(shown.contains("› explain a monad") && shown.contains("lunchbox"), "{shown}");
    assert!(shown.contains("▸ WebSearch monad for kids ✓"), "{shown}");
    // The message times are the session's, not the import's.
    let chat = w.json(&["chat", "show", &addr, "--format", "json"]);
    assert_eq!(chat["nodes"][0]["at_ms"], 1_792_486_800_000u64, "{chat}");
    // Search finds them.
    let hits = w.json(&["chat", "search", "lunchbox", "--json"]);
    assert_eq!(hits[0]["chat"]["id"], claude["id"], "{hits}");
    let hits = w.json(&["chat", "search", "zebra", "--json"]);
    assert!(!hits.as_array().unwrap().is_empty(), "{hits}");
    // Again: nothing new, nothing doubled.
    let again = w.json(&["chat", "import", "--json"]);
    assert_eq!(counts(&again, "claude"), (0, 0, 1, 1, 0), "{again}");
    // The session grows: only what is new comes.
    let mut text = std::fs::read_to_string(w.claude_file()).unwrap();
    text.push_str(&claude_more());
    std::fs::write(w.claude_file(), text).unwrap();
    let grown = w.json(&["chat", "import", "--agent", "claude", "--json"]);
    assert_eq!(counts(&grown, "claude"), (0, 1, 0, 1, 2), "{grown}");
    let shown = w.ok(&["chat", "show", &addr]);
    assert_eq!(shown.matches("lunchbox").count(), 1, "{shown}");
    assert!(shown.trim_end().ends_with("Burritos wrap fillings the same way."), "{shown}");
    // Read-only: a retry is refused, and a message carries on in a fork that says so.
    let retry = w.cli(&["chat", "retry", &addr]);
    assert!(!retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stderr).contains("continue it as a fork"));
    // Nor is it switched to another agent, picked from, or made a task (each would write into
    // it); pinning it is the person's.
    for args in [
        vec!["chat", "set", addr.as_str(), "--agent", "codex"],
        vec!["chat", "pick", addr.as_str()],
        vec!["chat", "task", addr.as_str()],
    ] {
        let out = w.cli(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("as a fork"), "{args:?}");
    }
    w.ok(&["chat", "pin", &addr]);
    let out = w.cli(&["chat", "send", &addr, "and a list?"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("continued as a fork"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("and a list?"));
    assert!(!w.ok(&["chat", "show", &addr]).contains("and a list?"), "the import is unchanged");
    let all = w.json(&["chat", "ls", "--json"]);
    let fork = all
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["forked"] == true)
        .expect("the fork is listed")
        .clone();
    let fork_addr = format!("c:{}", fork["handle"].as_str().unwrap());
    let shown = w.ok(&["chat", "show", &fork_addr]);
    assert!(shown.contains("lunchbox") && shown.contains("continued as a fork"), "{shown}");
    // A chat you delete stays deleted, whatever the next import finds.
    w.ok(&["chat", "rm", &format!("c:{}", w.by_source("gemini:")["handle"].as_str().unwrap())]);
    let after = w.json(&["chat", "import", "--json"]);
    assert_eq!(counts(&after, "gemini"), (0, 0, 0, 1, 0), "{after}");
    assert!(w.imported().iter().all(|c| !c["source"].as_str().unwrap().starts_with("gemini:")));
    // A session in one of ptyro's worktrees (under its data folder too) is history.
    let wt = w.d.paths.data.join("worktrees/repo/feature");
    write(
        &w.home().join(".claude/projects/-wt").join("wt-1.jsonl"),
        &format!(
            r#"{{"type":"user","uuid":"w1","sessionId":"wt-1","cwd":"{}","timestamp":"2026-10-21T09:00:00Z","message":{{"role":"user","content":"fix the flaky test"}}}}
"#,
            wt.to_string_lossy()
        ),
    );
    let r = w.json(&["chat", "import", "--agent", "claude", "--json"]);
    assert_eq!(counts(&r, "claude").0, 1, "{r}");
}

#[test]
fn since_leaves_out_old_sessions_and_import_keeps_importing() {
    // `[chat] import = true`: the daemon imports by itself, at once and as sessions grow.
    let w = World::new("import = true\n");
    w.until("the histories are imported", || w.imported().len() == 3);
    // An old session is left out of `--since`.
    let f = std::fs::File::options().write(true).open(w.claude_file()).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(60 * 86_400)).unwrap();
    drop(f);
    let r = w.json(&["chat", "import", "--since", "30d", "--dry-run", "--json"]);
    assert_eq!(counts(&r, "claude").3, 1, "{r}");
    // An unknown agent is refused with what it can do.
    let bad = w.cli(&["chat", "import", "--agent", "cursor"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("claude, codex and gemini"));
}
