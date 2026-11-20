//! KB3 (SPEC §25.6.3, §25.6.1): roots and the history's backfill, over a real daemon.
//!
//! - `ptyro ingest FOLDER` reads it and says what it read; a heading of one of its documents, a
//!   comment in its code and a file's name are each found from another project. A project
//!   registered in ptyro is searchable without a command. `kb ls`, `kb sync`, `kb rm`; a file
//!   that changes, is renamed or goes; the never list; a root under a profile behind the wall.
//! - A pass that the daemon's end interrupts is carried on at its next start.
//! - `ptyro ingest --history` counts, then brings in what the agents kept: never a private
//!   session's conversation, never one that was forgotten.
//! - KB3.7: `kb show REF --open` opens a file in your editor at the piece's line and a commit
//!   as `git show`; `ptyro gc` and the doctor name a root whose folder is gone; and the real
//!   TUI's Sources tab reads a folder through its form and stops reading it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_core::kb::{ItemKind, KbItem, RootSpec, Writer};
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::ItemFilter;

mod kbkit;
use kbkit::*;

fn profiles() -> String {
    format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\ncodex = \"~/.codex-work\"\n[profiles.home]\nclaude = \"~/.claude-home\"\n",
        agents("")
    )
}

fn put(dir: &Path, rel: &str, text: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

/// A file's time moves on even where the clock is coarse.
fn put_later(dir: &Path, rel: &str, text: &str) {
    let p = put(dir, rel, text);
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(later).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

impl World {
    /// The titles `ptyro recall` finds for `query`, run in `cwd`.
    fn found_in(&self, cwd: &Path, query: &str) -> Vec<String> {
        let o = self
            .d
            .cmd()
            .args(["recall", "--json", "--here", query])
            .env("HOME", self.home())
            .current_dir(cwd)
            .output()
            .unwrap();
        let hits: serde_json::Value = serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|e| panic!("{query}: {e}: {}", String::from_utf8_lossy(&o.stderr)));
        hits.as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn found(&self, query: &str) -> Vec<String> {
        self.found_in(&self.home(), query)
    }

    fn roots(&self) -> Vec<serde_json::Value> {
        self.json(&["kb", "ls", "--json"]).as_array().unwrap().clone()
    }

    /// Wait until the root called `name` is read: there, `ok`, and no pass over it.
    async fn read(&self, name: &str) -> serde_json::Value {
        let deadline = Instant::now() + WAIT * 3;
        loop {
            let roots = self.roots();
            let root = roots.iter().find(|r| r["name"] == name);
            if let Some(r) = root
                && r["state"] == "ok"
                && r["progress"].is_null()
            {
                return r.clone();
            }
            assert!(
                Instant::now() < deadline,
                "{name} was never read: {roots:#?}\n{}",
                self.log("knowledge base")
            );
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    async fn items(&self, kind: ItemKind) -> Vec<KbItem> {
        let f = ItemFilter { kinds: vec![kind], ..ItemFilter::default() };
        let mut items = self.db().list(f).await.unwrap();
        items.sort_by(|a, b| a.uri.cmp(&b.uri));
        items
    }
}

const LIMITER: &str = "/**\n * Limits how many requests one client may make in a minute (the zqthrottle rule).\n */\nexport class RateLimiter {}\n";

#[tokio::test(flavor = "multi_thread")]
async fn ingest_reads_a_folder_and_a_project_is_searchable_without_a_command() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    let home = w.home();

    // A project registered in ptyro is searchable without a command.
    let repo = home.join("feeder");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    put(&repo, "README.md", "# Feeder\n\nIt feeds the zqcapybara every morning.\n");
    put(&repo, ".gitignore", "build/\n");
    put(&repo, "build/out.md", "# Built\n\nzqignored by git\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "Feed the zqnarwhal first"]);
    let o = w.cli(&["project", "add", &repo.to_string_lossy()]);
    assert!(o.status.success(), "{o:?}");
    let root = w.read("feeder").await;
    assert_eq!((root["kind"].as_str(), root["commits"].as_u64()), (Some("project"), Some(1)));
    assert_eq!(w.found("zqcapybara"), ["README.md"]);
    assert_eq!(w.found("zqnarwhal"), ["Feed the zqnarwhal first"]);
    assert!(w.found("zqignored").is_empty());
    let o = w.cli(&["kb", "rm", "feeder", "--yes"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("kb.projects"), "{o:?}");

    // `ptyro ingest FOLDER` reads it and says what it read.
    let book = home.join("handbook");
    put(&book, "docs/DeployGuide.md", "# Deploying\n\nPush the tag and wait for the zqpelican.\n");
    put(&book, "src/RateLimiter.ts", LIMITER);
    put(&book, ".env", "TOKEN=zqsecretvalue123456\n");
    put(&book, "node_modules/x/index.js", "// zqdependency comment\n");
    std::fs::write(book.join("logo.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR zqbinary").unwrap();
    let o = w.cli(&["ingest", "handbook"]);
    let said =
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{said}");
    assert!(said.contains("3 files: 1 document, 1 source file (comments)"), "{said}");
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("read handbook: 2 files, "), "{said}");

    // A heading, a comment and a file's name, each found from another project.
    assert_eq!(w.found_in(&repo, "zqpelican"), ["docs/DeployGuide.md"]);
    assert_eq!(w.found_in(&repo, "zqthrottle"), ["src/RateLimiter.ts"]);
    assert_eq!(w.found_in(&repo, "rate limiter"), ["src/RateLimiter.ts"]);
    for never in ["zqsecretvalue", "zqdependency", "zqbinary"] {
        assert!(w.found(never).is_empty(), "{never}");
        assert!(!w.bytes_hold(never), "{never} is in the file");
    }
    let ls = w.out(&["kb", "ls"]);
    assert!(
        ls.lines()
            .any(|l| l.starts_with("handbook") && l.contains("folder") && l.contains("2 files")),
        "{ls}"
    );
    assert!(ls.lines().any(|l| l.starts_with("feeder") && l.contains("project")), "{ls}");
    assert!(w.out(&["kb"]).contains("2 roots are read"), "{}", w.out(&["kb"]));
    assert!(w.doctor_line().contains("; 2 roots"), "{}", w.doctor_line());

    // A change, a rename and a deletion are found by `kb sync`; the same path again is that root.
    put_later(
        &book,
        "docs/DeployGuide.md",
        "# Deploying\n\nPush the tag and wait for the zqtoucan.\n",
    );
    std::fs::rename(book.join("src/RateLimiter.ts"), book.join("src/Throttle.ts")).unwrap();
    let said = w.out(&["kb", "sync", "handbook"]);
    assert!(said.contains("handbook: 2 files"), "{said}");
    assert!(w.found("zqpelican").is_empty());
    assert_eq!(w.found("zqtoucan"), ["docs/DeployGuide.md"]);
    assert_eq!(w.found("zqthrottle"), ["src/Throttle.ts"]);
    let said = w.out(&["ingest", &book.join("docs").to_string_lossy(), "--json"]);
    assert!(said.contains("\"existing\": true"), "{said}");
    assert_eq!(w.roots().len(), 2);
    // `kb sync` with nothing named: the notes folder and every root.
    let said = w.out(&["kb", "sync"]);
    assert!(said.contains("0 notes in the folder") && said.contains("feeder: "), "{said}");

    // A root for one profile: its agents find it, another profile's never do.
    let plans = home.join("plans");
    put(&plans, "Launch.md", "# Launch\n\nShip the zqmarmot on Friday.\n");
    let o = w.cli(&[
        "ingest",
        &plans.to_string_lossy(),
        "--profile",
        "work",
        "--name",
        "work-plans",
        "--no-commits",
    ]);
    assert!(o.status.success(), "{o:?}");
    w.agent_in(&c, "aw", "claude", Some("work")).await;
    w.agent_in(&c, "ah", "claude", Some("home")).await;
    let args = serde_json::json!({ "query": "zqmarmot" });
    let work = w.mcp(&c, "aw", "recall", args.clone()).await;
    assert!(work.contains("Launch.md"), "{work}");
    let other = w.mcp(&c, "ah", "recall", args).await;
    assert!(!other.contains("Launch.md") && !other.contains("zqmarmot"), "{other}");

    // Removed: asked first (nobody to ask: refused), then gone with what was read of it.
    let o = w.cli(&["kb", "rm", "handbook"]);
    assert_eq!(o.status.code(), Some(5), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("Stop reading handbook"), "{o:?}");
    let said = w.out(&["kb", "rm", "handbook", "--yes"]);
    assert!(said.contains("handbook is no longer read: dropped 2 items"), "{said}");
    assert!(w.found("zqtoucan").is_empty() && !w.bytes_hold("zqtoucan"));
    assert_eq!(w.roots().len(), 2, "feeder and work-plans");
    // The project removed from ptyro: its root goes too.
    let o = w.cli(&["project", "rm", "feeder", "--yes"]);
    assert!(o.status.success(), "{o:?}");
    let deadline = Instant::now() + WAIT;
    while w.roots().iter().any(|r| r["name"] == "feeder") {
        assert!(Instant::now() < deadline, "feeder's root never went");
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    assert!(w.found("zqcapybara").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_that_the_daemons_end_interrupts_is_carried_on() {
    let mut w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    let dir = w.home().join("many");
    for n in 0..3000 {
        put(
            &dir,
            &format!("d{}/doc{n}.md", n / 100),
            &format!("# Doc {n}\n\nthe zqgecko number {n}\n"),
        );
    }
    // Asked for, and the daemon is killed while it reads.
    let spec = RootSpec { path: dir.to_string_lossy().into_owned(), ..RootSpec::default() };
    let Reply::KbIngested { plan, .. } =
        c.request(Request::KbIngest { spec, machine: None }).await.unwrap()
    else {
        panic!("not ingested")
    };
    assert_eq!(plan.files, 3000);
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(c);
    w.d.crash();
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    // (A request first: the socket accepts before the knowledge base has opened.)
    let _ = c.request(Request::KbStatus).await.unwrap();
    let root = w.read("many").await;
    assert_eq!(root["files"].as_u64(), Some(3000), "{root:#}");
    assert_eq!(w.items(ItemKind::File).await.len(), 3000);
    assert_eq!(w.found("zqgecko 2999"), ["d29/doc2999.md"]);
}

/// A Claude Code conversation of one turn, as the agent keeps it.
fn claude_turn(id: &str, cwd: &Path, asked: &str, answered: &str) -> String {
    let cwd = cwd.to_string_lossy();
    [
        serde_json::json!({"type": "api-request", "sessionId": id, "email": "zqsomeone@example.com"}),
        serde_json::json!({"type": "user", "uuid": format!("u-{id}"), "sessionId": id, "cwd": cwd,
            "timestamp": "2026-11-18T10:00:00.000Z", "message": {"role": "user", "content": asked}}),
        serde_json::json!({"type": "assistant", "uuid": format!("a-{id}"), "sessionId": id, "cwd": cwd,
            "timestamp": "2026-11-18T10:00:05.000Z",
            "message": {"role": "assistant", "model": "claude-test", "content": [{"type": "text", "text": answered}]}}),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_history_brings_in_what_the_agents_kept_and_never_a_private_session() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    let home = w.home();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ptyro-agents/tests/fixtures");

    // What the agents kept before ptyro did: under each profile's own folder.
    let a = "aaaaaaaa-0000-4000-8000-00000000000a";
    let b = "aaaaaaaa-0000-4000-8000-00000000000b";
    put(
        &home,
        &format!(".claude-work/projects/-x/{a}.jsonl"),
        &claude_turn(a, &home, "why does the zqhopper jam", "the zqbelt slips"),
    );
    put(
        &home,
        &format!(".claude/projects/-y/{b}.jsonl"),
        &claude_turn(b, &home, "an old zqmotor question", "it hums"),
    );
    let rollout =
        std::fs::read_to_string(fixtures.join("codex-0.167.1/transcripts/plain.jsonl")).unwrap();
    let thread = "01a1f5f6-b278-76d1-8a83-cfe1291c7e0d";
    put(
        &home,
        &format!(".codex-work/sessions/2026/11/19/rollout-2026-11-19T22-39-07-{thread}.jsonl"),
        &rollout,
    );
    // A private session of now: its agent keeps its own history, ptyro none of it.
    let o = w.cli(&["new", "--private", "--detach", "--agent", "claude", "--name", "sp"]);
    assert!(o.status.success(), "{o:?}");
    w.d.wait_for_text(&c, "sp", "resumed=", WAIT).await.unwrap();
    w.turn(&c, "sp", "edit notes.txt zqprivateword").await;
    let conv = w.info(&c, "sp").await.conversation.expect("its conversation");
    assert!(find_file(&home, &format!("{conv}.jsonl")).is_some(), "the agent's own copy");
    // And a session that is kept as it happens: known already.
    let kept = w.agent(&c, "sk", "claude").await;
    w.turn(&c, "sk", "edit notes.txt zqkeptword").await;
    let live = format!("session://claude/{}", kept.conversation.unwrap());
    w.kept(&live, "its turn", |k| k.chunks.len() > 1).await;

    // Counted first; with `--dry-run` nothing is read.
    let said = w.out(&["ingest", "--history", "--dry-run"]);
    assert!(said.starts_with("5 conversations in your agents' folders: 2 kept already"), "{said}");
    assert!(said.contains("3 to read") && said.contains("claude (work): 1 in"), "{said}");
    assert_eq!(w.sessions().await.len(), 1);
    let plan = w.json(&["ingest", "--history", "--dry-run", "--agent", "codex", "--json"]);
    assert_eq!((plan["found"].as_u64(), plan["reading"].as_bool()), (Some(1), Some(false)));

    // Read: each under the profile whose folder held it, written by `import`.
    let o = w.cli(&["ingest", "--history"]);
    let said =
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success() && said.contains("read 3 conversations"), "{said}");
    let all = w.sessions().await;
    let of = |uri: &str| {
        all.iter().find(|i| i.uri == uri).unwrap_or_else(|| panic!("{uri} in {all:#?}")).clone()
    };
    let (ia, ib, ic) = (
        of(&format!("session://claude/{a}")),
        of(&format!("session://claude/{b}")),
        of(&format!("session://codex/{thread}")),
    );
    assert_eq!((ia.writer, ia.profile.as_deref()), (Writer::Import, Some("work")));
    assert_eq!((ib.writer, ib.profile.as_deref()), (Writer::Import, Some("default")));
    assert_eq!(
        (ic.writer, ic.profile.as_deref(), ic.agent.as_deref()),
        (Writer::Import, Some("work"), Some("codex"))
    );
    assert_eq!(all.len(), 4, "the three and the live one; never the private one");
    assert_eq!(w.found("zqhopper"), ["why does the zqhopper jam"]);
    assert!(!w.bytes_hold("zqprivateword") && !w.bytes_hold("zqsomeone"));
    let recent = w.out(&["kb", "recent"]);
    assert!(recent.contains("why does the zqhopper jam"), "{recent}");

    // Again: nothing to read. And what is forgotten stays out.
    let said = w.out(&["ingest", "--history"]);
    assert!(said.contains("5 kept already"), "{said}");
    let addr =
        w.json(&["recall", "--json", "--here", "zqmotor"])[0]["addr"].as_str().unwrap().to_owned();
    let item = addr.split('/').next().unwrap().to_owned();
    assert!(w.cli(&["kb", "forget", &item, "--yes"]).status.success());
    w.out(&["ingest", "--history"]);
    assert_eq!(w.sessions().await.len(), 3);
    assert!(w.found("zqmotor").is_empty() && !w.bytes_hold("zqmotor"));

    // The private session removed from ptyro: its conversation still never comes in.
    w.out(&["rm", "sp", "--stop"]);
    w.out(&["ingest", "--history"]);
    assert_eq!(w.sessions().await.len(), 3);
    assert!(!w.bytes_hold("zqprivateword"));
}

/// KB3.7: what a search found is opened in its own tool, and a root that can no longer be
/// read is named where stale things are (`gc`, and the doctor that the TUI's mend list and
/// the web's Health page show).
#[tokio::test(flavor = "multi_thread")]
async fn a_file_and_a_commit_open_in_their_own_tools_and_a_root_that_is_gone_is_named() {
    let w = World::new(&agents(""));
    let _c = w.d.client().await.unwrap();
    let home = w.home();
    let book = home.join("handbook");
    std::fs::create_dir_all(&book).unwrap();
    git(&book, &["init", "-q"]);
    put(
        &book,
        "docs/Guide.md",
        "# Guide\n\nHow the belts run.\n\n## Tension\n\nTighten the zqbelt a quarter turn.\n",
    );
    put(
        &book,
        "src/feed.ts",
        "export const a = 1;\n\n// The zqhopper is fed every morning, never twice.\nexport function feed() {}\n",
    );
    git(&book, &["add", "."]);
    git(&book, &["commit", "-q", "-m", "Run the zqconveyor slower"]);
    let o = w.cli(&["ingest", "handbook"]);
    assert!(o.status.success(), "{o:?}");
    let addr = |word: &str| -> String {
        let hits = w.json(&["recall", "--json", "--here", word]);
        hits[0]["addr"].as_str().unwrap_or_else(|| panic!("{word}: {hits}")).to_owned()
    };
    // An editor that says what it was given (called nvim: one known to take a line).
    let bin = home.join("bin");
    let said = home.join("editor-said");
    let nvim = ptyro_testkit::program::program(
        &bin,
        "nvim",
        &format!("echo \"$@\" > '{}'", said.display()),
    );
    let open = |addr: &str| {
        w.d.cmd()
            .args(["kb", "show", addr, "--open"])
            .env("HOME", &home)
            .env("EDITOR", &nvim)
            .env_remove("VISUAL")
            .env("GIT_PAGER", "cat")
            .current_dir(&home)
            .output()
            .unwrap()
    };
    // A document's section: at its heading's line. A comment: at its own.
    let o = open(&addr("zqbelt"));
    assert!(o.status.success(), "{o:?}");
    let guide = book.join("docs/Guide.md");
    assert_eq!(std::fs::read_to_string(&said).unwrap().trim(), format!("+5 {}", guide.display()));
    let o = open(&addr("zqhopper"));
    assert!(o.status.success(), "{o:?}");
    let feed = book.join("src/feed.ts");
    assert_eq!(std::fs::read_to_string(&said).unwrap().trim(), format!("+3 {}", feed.display()));
    // A commit: `git show` of it, in its repository.
    let o = open(&addr("zqconveyor"));
    let shown = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{o:?}");
    assert!(
        shown.contains("Run the zqconveyor slower") && shown.contains("docs/Guide.md"),
        "{shown}"
    );

    // Nothing stale yet. Its folder goes: `gc` and the doctor name it, with what to do.
    assert!(!w.out(&["gc", "--dry-run"]).contains("knowledge base"));
    std::fs::rename(&book, home.join("elsewhere")).unwrap();
    w.cli(&["kb", "sync", "handbook"]);
    let deadline = Instant::now() + WAIT;
    while !w.roots().iter().any(|r| r["name"] == "handbook" && r["state"] == "missing") {
        assert!(Instant::now() < deadline, "never missing: {:#?}", w.roots());
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    let gc = w.out(&["gc", "--dry-run"]);
    assert!(gc.contains("knowledge base: handbook: ") && gc.contains("is gone"), "{gc}");
    assert!(gc.contains("`ptyro kb rm handbook`"), "{gc}");
    let checks = w.json(&["doctor", "--quick", "--json"]);
    let list = checks.as_array().or_else(|| checks["checks"].as_array()).unwrap();
    let named = list
        .iter()
        .find(|c| c["section"] == "knowledge base" && c["name"] == "root handbook")
        .unwrap_or_else(|| panic!("the doctor doesn't name it: {checks:#}"));
    assert_eq!(named["level"], "warn", "{named}");
    assert!(named["text"].as_str().unwrap().contains("`ptyro kb rm handbook`"), "{named}");
    // What was kept of it is still found (it may come back); opening the file says it is gone.
    let o = open(&addr("zqbelt"));
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("is no longer there"), "{o:?}");
}

/// KB3.7, in the real TUI over a real daemon: the Library's Sources tab starts empty, `i`
/// offers where the folder is and then the form with every option, the folder is read and
/// its row says what it holds, and `x` stops reading it through the daemon's question.
#[tokio::test(flavor = "multi_thread")]
async fn the_tuis_sources_tab_reads_a_folder_and_stops_reading_it() {
    let w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    w.agent_in(&c, "sb", "claude", None).await;
    let book = w.home().join("handbook");
    put(&book, "Guide.md", "# Guide\n\nTighten the zqtuibelt a quarter turn.\n");
    let script = w
        .tui(&["attach", "sb"])
        .send_when("sb", "\x11L")
        .send_when("3 Sources", "3")
        .send_when("nothing is read yet", "i")
        // Where it is: a path to type (the first choice), then the form.
        .send_when("Read which folder", "\r")
        .send_when("Only now", &format!("{}\r", book.display()))
        // Read: its row says what it holds. Then `x`, asked in the daemon's words.
        .send_when("1 file, 2 pieces", "x")
        .send_when("Stop reading handbook", "y")
        .until("handbook is no longer read: dropping 1 item");
    let r = ptyro_testkit::headless::run(script).await.expect("headless run");
    let shown = &r.screen;
    assert!(shown.contains("3 Sources"), "{shown}");
    let deadline = Instant::now() + WAIT;
    while !w.roots().is_empty() {
        assert!(Instant::now() < deadline, "never gone: {:#?}", w.roots());
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    assert!(w.found("zqtuibelt").is_empty());
    assert!(book.join("Guide.md").exists(), "the folder's own files aren't touched");
}
