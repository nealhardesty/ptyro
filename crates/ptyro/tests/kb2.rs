//! KB2 (SPEC §25.6.4): notes, over a real daemon with the fake agents.
//!
//! - An agent told to remember writes a note with its `remember` tool: a Markdown file in the
//!   notes folder under its own profile, stamped with the agent and its session by the daemon,
//!   redacted before it is a file. The next session's `recall` returns it first; a session of
//!   another profile never sees it. The owner edits the file in an editor and the change is
//!   found; deletes it and it is gone.
//! - `remember`'s limits: 8 KiB, 20 a session a day, `kb.remember`, never over another note
//!   (`replaces` sets the old one aside in `_old/`), a private session named by nothing.
//! - `ptyro kb note`, `kb notes`, `kb show --open` (your editor), `kb sync`, `kb forget` (the
//!   file goes), `--wrote`; `about` and `uninstall` name the folder, and uninstall leaves it.
//! - The TUI's Library has the Notes tab.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_core::kb::ChunkKind;
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request};

mod kbkit;
use kbkit::*;

/// Two profiles, each with its fake agents.
fn profiles() -> String {
    format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\ncodex = \"~/.codex-work\"\n[profiles.work.secrets]\nDB_PASS = 'hunter2-value'\n[profiles.home]\nclaude = \"~/.claude-home\"\n",
        agents("")
    )
}

impl World {
    fn notes_dir(&self) -> PathBuf {
        self.d.home.path().join("data/kb/notes")
    }

    /// Wait until `pred` holds.
    async fn eventually(&self, what: &str, pred: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !pred() {
            assert!(
                Instant::now() < deadline,
                "never: {what}; the daemon said:\n{}",
                self.log("note")
            );
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    }

    /// The uris `ptyro recall` finds for `query`, best first.
    fn found(&self, query: &str) -> Vec<String> {
        self.json(&["recall", "--json", "--here", query])
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["uri"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

/// The `k:` lines of a `recall` answer.
fn hits(said: &str) -> Vec<&str> {
    said.lines().filter(|l| l.starts_with("k:")).collect()
}

/// The address `remember` answered with.
fn noted(said: &str) -> String {
    let rest = said.strip_prefix("noted as ").unwrap_or_else(|| panic!("{said}"));
    rest.split([',', '.']).next().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_remembers_a_note_the_next_session_recalls_it_first_and_an_edit_is_found() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    // A work session does something about a word, so that a session holds it too.
    let a = w.agent_in(&c, "wa", "claude", Some("work")).await;
    let session = format!("session://claude/{}", a.conversation.clone().unwrap());
    w.turn(&c, "wa", "edit retry.txt zqretry").await;
    w.kept(&session, "the turn", |k| k.has(ChunkKind::Assistant, "wrote retry.txt")).await;

    // The three tools, deferred; the instructions say when to remember.
    let listed = w.mcp(&c, "wa", "tools/list", serde_json::json!({})).await;
    assert!(
        listed.starts_with(
            "tools: recall, recall_read, remember\ninstructions: ptyro's knowledge base holds"
        ),
        "{listed}"
    );
    assert!(listed.contains("keep it with remember"), "{listed}");

    // "Remember that we chose X because Y."
    let said = w
        .mcp(
            &c,
            "wa",
            "remember",
            serde_json::json!({
                "title": "Retry policy",
                "text": "We chose zqretry backoff with three tries because the API throttles.\nexport API_TOKEN=abc123def4567 and the password is hunter2-value",
                "tags": ["decision", "api"],
            }),
        )
        .await;
    let addr = noted(&said);
    assert!(addr.starts_with("k:") && said.contains("“Retry policy”"), "{said}");
    assert!(said.contains("about no project") && said.contains("recall"), "{said}");

    // The file is in the notes folder, under the session's profile; who wrote it is the
    // daemon's word; what looks like a credential never reached the disk.
    let path = w.notes_dir().join("work/_global/retry-policy.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nby: claude"), "{text}");
    assert!(text.contains(&format!("\nsession: {}\n", a.id.full())), "{text}");
    assert!(text.contains("\ntags: [decision, api]\n"), "{text}");
    assert!(text.contains("# Retry policy\n\nWe chose zqretry backoff with three tries"), "{text}");
    assert!(text.contains("API_TOKEN=<redacted>") && text.contains("<secret:DB_PASS>"), "{text}");
    for never in ["abc123def4567", "hunter2-value"] {
        assert!(!text.contains(never) && !w.bytes_hold(never), "{never} was kept");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // The next session of that profile: `recall` returns the note first, before the session
    // that holds the word too, and says who wrote it.
    w.agent_in(&c, "wb", "claude", Some("work")).await;
    let said = w.mcp(&c, "wb", "recall", serde_json::json!({ "query": "zqretry" })).await;
    let found = hits(&said);
    assert!(found.len() >= 2, "{said}");
    assert!(
        found[0].starts_with(&addr) && found[0].contains(" · note · Retry policy · "),
        "{said}"
    );
    assert!(found[0].ends_with(" · written by the agent claude"), "{said}");
    assert!(found[1].contains(" · session · "), "{said}");
    let read = w.mcp(&c, "wb", "recall_read", serde_json::json!({ "ref": addr })).await;
    assert!(read.contains("We chose zqretry backoff") && read.contains("tags: decision, api"));
    // The session that wrote it finds its own note (its own session it never does).
    let said = w.mcp(&c, "wa", "recall", serde_json::json!({ "query": "zqretry" })).await;
    assert!(hits(&said).len() == 1 && hits(&said)[0].starts_with(&addr), "{said}");

    // The wall: another profile's session doesn't find it, can't read it, can't replace it;
    // and what it remembers lands under its own profile.
    w.agent_in(&c, "hb", "claude", Some("home")).await;
    let said = w.mcp(&c, "hb", "recall", serde_json::json!({ "query": "zqretry" })).await;
    assert!(said.contains("nothing found"), "{said}");
    let said = w.mcp(&c, "hb", "recall_read", serde_json::json!({ "ref": addr })).await;
    assert!(said.starts_with("error: no item"), "{said}");
    let said = w
        .mcp(&c, "hb", "remember", serde_json::json!({ "text": "zqhomefact", "replaces": addr }))
        .await;
    assert!(said.starts_with("error: no item") && path.exists(), "{said}");
    let said = w.mcp(&c, "hb", "remember", serde_json::json!({ "text": "zqhomefact" })).await;
    assert!(noted(&said).starts_with("k:"), "{said}");
    assert!(w.notes_dir().join("home/_global/zqhomefact.md").exists());
    let said = w.mcp(&c, "wb", "recall", serde_json::json!({ "query": "zqhomefact" })).await;
    assert!(said.contains("nothing found"), "{said}");
    // You see every note.
    assert_eq!(w.found("zqhomefact"), ["note://home/_global/zqhomefact"]);
    assert_eq!(w.found("zqretry")[0], "note://work/_global/retry-policy");

    // Codex has the tool too (its calls are approved), and writes as itself.
    w.agent_in(&c, "wx", "codex", Some("work")).await;
    let said = w.mcp(&c, "wx", "remember", serde_json::json!({ "text": "zqcodexfact" })).await;
    assert!(noted(&said).starts_with("k:"), "{said}");
    let text = std::fs::read_to_string(w.notes_dir().join("work/_global/zqcodexfact.md")).unwrap();
    assert!(text.starts_with("---\nby: codex"), "{text}");

    // The owner edits the file in an editor: the change is found, with no command.
    std::fs::write(&path, text_with(&std::fs::read_to_string(&path).unwrap(), "three", "zqfive"))
        .unwrap();
    w.eventually("the edit is found", || w.found("zqfive").len() == 1).await;
    assert_eq!(w.found("zqfive"), ["note://work/_global/retry-policy"]);
    // A file made by hand is a note of yours; deleted by hand, it is gone.
    let mine = w.notes_dir().join("work/_global/by-hand.md");
    std::fs::write(&mine, "# By hand\n\nzqbyhand\n").unwrap();
    w.eventually("the new file is found", || w.found("zqbyhand").len() == 1).await;
    let said = w.mcp(&c, "wb", "recall", serde_json::json!({ "query": "zqbyhand" })).await;
    assert!(hits(&said)[0].ends_with(" · written by you (the user)"), "{said}");
    std::fs::remove_file(&mine).unwrap();
    w.eventually("the deleted file is gone", || w.found("zqbyhand").is_empty()).await;
    // A folder of notes moved in whole is one event, and of a folder: its notes are found
    // (review D5).
    let aside = w.home().join("moved-in");
    std::fs::create_dir_all(&aside).unwrap();
    std::fs::write(aside.join("moved.md"), "# Moved\n\nzqmovedin\n").unwrap();
    std::fs::rename(&aside, w.notes_dir().join("work/moved-in")).unwrap();
    w.eventually("the folder moved in is read", || w.found("zqmovedin").len() == 1).await;
    assert_eq!(w.found("zqmovedin"), ["note://work/moved-in/moved"]);
}

fn text_with(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{text}");
    text.replace(from, to)
}

#[tokio::test(flavor = "multi_thread")]
async fn remember_has_its_limits_and_never_writes_over_a_note() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    let a = w.agent_in(&c, "wa", "claude", Some("work")).await;
    let session = format!("session://claude/{}", a.conversation.unwrap());
    let remember = |text: String| serde_json::json!({ "text": text });

    // At most 8 KiB. (More than a terminal's line takes: the agent runs the command, which
    // the daemon holds to the same limits as the tool.)
    let big = format!(
        "sh {BIN} kb note \"$(head -c 8200 /dev/zero | tr '\\0' x)\" 2>&1 | grep -c 'at most 8 KiB'"
    );
    w.turn(&c, "wa", &big).await;
    w.kept(&session, "the refusal", |k| k.has(ChunkKind::Assistant, "ran: 1")).await;
    // Nothing that is mostly credentials.
    let keys: String = (0..26).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect();
    let said = w.mcp(&c, "wa", "remember", remember(keys)).await;
    assert!(said.contains("things that look like credentials"), "{said}");
    assert!(
        !w.notes_dir().join("work").exists() || dir_len(&w.notes_dir().join("work/_global")) == 0
    );

    // `replaces`: a new note; the old file is kept aside, out of search.
    let said =
        w.mcp(&c, "wa", "remember", remember("# Deploy\n\nzqoldway with make ship".into())).await;
    let old = noted(&said);
    let said = w
        .mcp(
            &c,
            "wa",
            "remember",
            serde_json::json!({ "text": "# Deploy\n\nzqnewway with make release", "replaces": old }),
        )
        .await;
    let new = noted(&said);
    assert_ne!(new, old);
    let dir = w.notes_dir().join("work/_global");
    assert!(!dir.join("deploy.md").exists());
    assert!(std::fs::read_to_string(dir.join("_old/deploy.md")).unwrap().contains("zqoldway"));
    let text = std::fs::read_to_string(dir.join("deploy-2.md")).unwrap();
    assert!(text.contains("\nreplaces: note://work/_global/deploy\n"), "{text}");
    assert!(w.found("zqoldway").is_empty());
    assert_eq!(w.found("zqnewway"), ["note://work/_global/deploy-2"]);

    // An agent that runs the command writes as itself too, and can neither write over a note
    // nor forget one.
    w.turn(&c, "wa", &format!("sh {BIN} kb note zqfromcli; echo code=$?")).await;
    w.eventually("the command's note", || dir.join("zqfromcli.md").exists()).await;
    let text = std::fs::read_to_string(dir.join("zqfromcli.md")).unwrap();
    assert!(text.starts_with("---\nby: claude"), "{text}");
    w.turn(&c, "wa", &format!("sh {BIN} kb forget {new} --yes; echo code=$?")).await;
    w.kept(&session, "the forget", |k| k.has(ChunkKind::Assistant, "ran: code=1")).await;
    assert!(dir.join("deploy-2.md").exists());

    // 20 a session a day: a runaway loop is told so, and another session still writes.
    let so_far = 3;
    for n in so_far..20 {
        let said = w.mcp(&c, "wa", "remember", remember(format!("zqloop fact number {n}"))).await;
        assert!(said.starts_with("noted as k:"), "{n}: {said}");
    }
    let said = w.mcp(&c, "wa", "remember", remember("one too many".into())).await;
    assert!(said.starts_with("error: this session wrote 20 notes today"), "{said}");
    assert!(!dir.join("one-too-many.md").exists());
    w.agent_in(&c, "wb", "claude", Some("work")).await;
    let said = w.mcp(&c, "wb", "remember", remember("zqanother session writes".into())).await;
    assert!(said.starts_with("noted as k:"), "{said}");

    // A private session may remember (a deliberate act): the note names no session.
    let spec = SpawnSpec {
        agent: Some("claude".into()),
        profile: Some("work".into()),
        cwd: w.home(),
        name: Some("wp".into()),
        private: Some(true),
        size: Some(ptyro_core::TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    let Reply::Session { info: p } = c.request(Request::Spawn { spec }).await.unwrap() else {
        panic!()
    };
    w.d.wait_for_text(&c, "wp", "resumed=", WAIT).await.unwrap();
    let said = w.mcp(&c, "wp", "remember", remember("zqprivate fact".into())).await;
    assert!(said.starts_with("noted as k:"), "{said}");
    let text = std::fs::read_to_string(dir.join("zqprivate-fact.md")).unwrap();
    assert!(text.contains("\nsession: a private session\n"), "{text}");
    assert!(!text.contains(&p.id.full()), "{text}");

    // `kb.remember = false`: the tool is not given, not allowed, and refused by the daemon.
    let o = w.cli(&["config", "set", "kb.remember", "false"]);
    assert!(o.status.success(), "{o:?}");
    let deadline = Instant::now() + WAIT;
    let mut n = 0;
    loop {
        n += 1;
        let name = format!("off{n}");
        w.agent_in(&c, &name, "claude", Some("work")).await;
        let listed = w.mcp(&c, &name, "tools/list", serde_json::json!({})).await;
        if listed.starts_with("tools: recall, recall_read\n") {
            assert!(!listed.contains("remember"), "{listed}");
            let said = w.mcp(&c, &name, "remember", remember("zqnever".into())).await;
            assert_eq!(said, "refused: mcp__ptyro__remember is not allowed");
            break;
        }
        assert!(Instant::now() < deadline, "`kb.remember = false` never took the tool: {listed}");
    }
    // (A session started before still has the tool's name: the daemon says no.)
    let said = w.mcp(&c, "wb", "remember", remember("zqnever".into())).await;
    assert!(said.starts_with("error: agents don't write notes here"), "{said}");
    assert!(w.found("zqnever").is_empty());
    // You still write notes.
    let o = w.cli(&["kb", "note", "zqmine still"]);
    assert!(o.status.success(), "{o:?}");
}

fn dir_len(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.count())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commands_write_list_edit_sync_and_forget_notes_and_uninstall_keeps_them() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    // (The knowledge base is open once the daemon has answered something.)
    w.status("on", |s| s["enabled"] == true).await;

    // `kb note`: yours, global and everyone's unless told; its address and its file.
    let o = w.cli(&["kb", "note", "we deploy zqship with", "make ship", "--tag", "ops"]);
    assert!(o.status.success(), "{o:?}");
    let line = String::from_utf8_lossy(&o.stdout).into_owned();
    let path = w.notes_dir().join("_all/_global/we-deploy-zqship-with-make-ship.md");
    assert!(line.starts_with("k:") && line.trim_end().ends_with(path.to_str().unwrap()), "{line}");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("---\nby: you\ntags: [ops]\ncreated: "), "{text}");
    assert!(text.ends_with("---\n\nwe deploy zqship with make ship\n"), "{text}");
    // With a title, for one profile, from stdin, as JSON.
    let mut child =
        w.d.cmd()
            .args(["kb", "note", "-", "--title", "Belt tension", "--profile", "work", "--json"])
            .env("HOME", w.home())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"zqbelt forty newtons\n").unwrap();
    let o = child.wait_with_output().unwrap();
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let belt = j["item"]["addr"].as_str().unwrap().to_owned();
    assert_eq!(j["item"]["item"]["uri"], "note://work/_global/belt-tension");
    assert_eq!(j["item"]["item"]["writer"], "user");
    assert!(j["path"].as_str().unwrap().ends_with("work/_global/belt-tension.md"), "{j}");
    // What the TUI's note form sends for a text and a title that start with `-` (review B3):
    // the words last, after `--`, and the value joined to its flag.
    let o =
        w.cli(&["kb", "note", "--title=-zqdash-", "--json", "--", "-zqminus one is the offset"]);
    assert!(o.status.success(), "{o:?}");
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let dashed = j["path"].as_str().unwrap().to_owned();
    assert!(std::fs::read_to_string(&dashed).unwrap().contains("-zqminus one is the offset"));
    assert!(
        w.cli(&["kb", "forget", j["item"]["addr"].as_str().unwrap(), "--yes"]).status.success()
    );
    assert!(!std::path::Path::new(&dashed).exists());
    // With nothing to say and nobody to ask: how to say it.
    let o = w.cli(&["kb", "note"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("say what to note"), "{o:?}");
    let o = w.cli(&["kb", "note", "x", "--profile", "nobody"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("nobody"), "{o:?}");

    // An agent's, to tell them apart.
    w.agent_in(&c, "wa", "claude", Some("work")).await;
    let said = w.mcp(&c, "wa", "remember", serde_json::json!({ "text": "zqagentfact" })).await;
    assert!(said.starts_with("noted as k:"), "{said}");

    // `kb notes`: by project, who wrote each; `--wrote` and `--json`.
    let list = w.out(&["kb", "notes"]);
    assert!(list.starts_with("global\n  k:"), "{list}");
    assert!(
        list.contains("Belt tension · you · ") && list.contains("zqagentfact · claude · "),
        "{list}"
    );
    let mine = w.json(&["kb", "notes", "--wrote", "you", "--json"]);
    assert_eq!(mine.as_array().unwrap().len(), 2, "{mine}");
    let theirs = w.json(&["kb", "notes", "--wrote", "agent", "--json"]);
    assert_eq!(theirs[0]["item"]["uri"], "note://work/_global/zqagentfact");
    assert_eq!(w.json(&["kb", "notes", "-p", "_global", "--json"]).as_array().unwrap().len(), 3);
    // The same filter on a search and on the history.
    let hits = w.json(&["recall", "--json", "--wrote", "you", "zqbelt"]);
    assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
    assert!(
        w.json(&["recall", "--json", "--wrote", "agent", "zqbelt"]).as_array().unwrap().is_empty()
    );
    let recent = w.json(&["kb", "recent", "--json", "--kind", "note", "--wrote", "agent"]);
    assert_eq!(recent.as_array().unwrap().len(), 1, "{recent}");

    // `kb show --open`: the note in your editor, written back when it changed.
    let bin = w.home().join("bin");
    let editor = ptyro_testkit::program::program(
        &bin,
        "ed",
        "grep -q 'by: you' \"$1\" || exit 3\nprintf '%s\\n' '# Belt tension' '' 'zqwarm fifty-five newtons' > \"$1\"",
    );
    let o =
        w.d.cmd()
            .args(["kb", "show", &belt, "--open"])
            .env("HOME", w.home())
            .env("EDITOR", &editor)
            .env_remove("VISUAL")
            .output()
            .unwrap();
    assert!(o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("Belt tension"), "{o:?}");
    assert_eq!(w.found("zqwarm"), ["note://work/_global/belt-tension"]);
    assert!(w.found("zqbelt").is_empty(), "the old words are gone");
    // The file changed underneath while the editor was open (review D2): it is not written
    // over, and what you wrote is kept aside.
    let belt_file = w.notes_dir().join("work/_global/belt-tension.md");
    let slow = ptyro_testkit::program::program(
        &bin,
        "slow-ed",
        &format!(
            "printf '%s\\n' '# Belt tension' '' 'zqelsewhere sixty newtons' > '{}'\nprintf '%s\\n' '# Belt tension' '' 'zqmine seventy newtons' > \"$1\"",
            belt_file.display()
        ),
    );
    let o =
        w.d.cmd()
            .args(["kb", "show", &belt, "--open"])
            .env("HOME", w.home())
            .env("EDITOR", &slow)
            .env_remove("VISUAL")
            .output()
            .unwrap();
    let said = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(o.status.code(), Some(5), "{o:?}");
    assert!(said.contains("changed since it was read") && said.contains("is kept in "), "{said}");
    let aside = said.split("is kept in ").nth(1).unwrap().trim();
    assert!(std::fs::read_to_string(aside).unwrap().contains("zqmine"), "{said}");
    std::fs::remove_file(aside).unwrap();
    assert!(std::fs::read_to_string(&belt_file).unwrap().contains("zqelsewhere"));
    w.eventually("what was written elsewhere is found", || w.found("zqelsewhere").len() == 1).await;
    assert!(w.found("zqmine").is_empty());
    std::fs::write(&belt_file, "# Belt tension\n\nzqwarm fifty-five newtons\n").unwrap();
    w.eventually("the note is as it was", || w.found("zqwarm").len() == 1).await;
    // `kb show`: what is kept of it.
    let shown = w.out(&["kb", "show", &belt]);
    assert!(
        shown.contains("note · Belt tension") && shown.contains("zqwarm fifty-five"),
        "{shown}"
    );

    // `kb sync`: a file written by hand is read when asked, whatever the watcher saw.
    std::fs::write(w.notes_dir().join("work/_global/hand.md"), "zqhandmade\n").unwrap();
    let synced = w.out(&["kb", "sync"]);
    assert!(synced.starts_with("4 notes in the folder: "), "{synced}");
    assert_eq!(w.found("zqhandmade"), ["note://work/_global/hand"]);

    // `about` and the status name the folder.
    let about = w.out(&["about"]);
    assert!(about.lines().any(|l| l.starts_with("notes") && l.ends_with("kb/notes")), "{about}");
    let status = w.out(&["kb"]);
    assert!(
        status.contains("notes are Markdown files in ") && status.contains("4 notes"),
        "{status}"
    );

    // `kb forget`: the question says the file goes; then it has.
    let o = w.cli(&["kb", "forget", &belt]);
    assert_eq!(o.status.code(), Some(5), "nobody to ask: {o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("A note's file is deleted"), "{o:?}");
    let o = w.cli(&["kb", "forget", &belt, "--yes"]);
    assert!(o.status.success(), "{o:?}");
    assert!(!w.notes_dir().join("work/_global/belt-tension.md").exists());
    assert!(w.found("zqwarm").is_empty() && !w.bytes_hold("zqwarm"));

    // Uninstall: the plan says the notes stay, and they do; everything else of the data goes.
    let plan = w.out(&["uninstall", "--dry-run"]);
    assert!(plan.contains("3 notes you and your agents wrote"), "{plan}");
    assert!(plan.contains("--delete-notes removes them too"), "{plan}");
    let plan = w.out(&["uninstall", "--dry-run", "--delete-notes"]);
    assert!(plan.contains("your notes    3 notes you and your agents wrote"), "{plan}");
    drop(c);
    let o = w.cli(&["uninstall", "--yes"]);
    assert!(o.status.success(), "{o:?}");
    assert!(path.exists() && w.notes_dir().join("work/_global/hand.md").exists());
    assert!(!w.kb_db().exists(), "kb.db went with the data");
    let o = w.cli(&["uninstall", "--yes", "--delete-notes"]);
    assert!(o.status.success(), "{o:?}");
    assert!(!w.notes_dir().exists());
}

/// `ptyro mcp` lists `remember` only where the daemon gave it.
#[test]
fn ptyro_mcp_offers_remember_only_where_it_was_given() {
    use std::io::Write;
    let w = World::new("");
    let list = |env: &[(&str, &str)]| -> Vec<String> {
        let mut cmd = w.d.cmd();
        cmd.arg("mcp");
        for k in ["PTYRO_SESSION_ID", "PTYRO_CHAT_ID", "PTYRO_KB_TOOLS", "PTYRO_KB_REMEMBER"] {
            cmd.env_remove(k);
        }
        cmd.envs(env.iter().copied());
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for (id, method) in [(1, "initialize"), (2, "tools/list")] {
            let msg =
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": {} });
            writeln!(stdin, "{msg}").unwrap();
        }
        drop(stdin);
        let o = child.wait_with_output().unwrap();
        let answers: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        answers[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let always = t["_meta"]["anthropic/alwaysLoad"] == true;
                format!("{}{}", t["name"].as_str().unwrap(), if always { "!" } else { "" })
            })
            .collect()
    };
    let session = ("PTYRO_SESSION_ID", "s1");
    assert_eq!(list(&[session]), ["recall", "recall_read"]);
    assert_eq!(list(&[session, ("PTYRO_KB_REMEMBER", "1")]), ["recall", "recall_read", "remember"]);
    // A chat's: always loaded, and only with the knowledge base's tools.
    let chat = ("PTYRO_CHAT_ID", "c1");
    assert_eq!(
        list(&[session, chat, ("PTYRO_KB_REMEMBER", "1")]),
        ["chat_search!", "chat_read!", "chat_list!"]
    );
    assert_eq!(
        list(&[session, chat, ("PTYRO_KB_TOOLS", "1"), ("PTYRO_KB_REMEMBER", "1")]),
        ["recall!", "recall_read!", "remember!", "chat_search!", "chat_read!", "chat_list!"]
    );
}

/// The real TUI's Library over a real daemon: the Notes tab lists what was written, by what
/// it is about and who wrote it; `n` opens the note form with every option; `x` deletes a
/// note through the daemon's question, and its file is gone.
#[tokio::test(flavor = "multi_thread")]
async fn the_tuis_library_has_the_notes_tab() {
    let w = World::new(&profiles());
    let c = w.d.client().await.unwrap();
    w.agent_in(&c, "lb", "claude", Some("work")).await;
    let o = w.cli(&["kb", "note", "zqtuinote is here", "--title", "Tui note"]);
    assert!(o.status.success(), "{o:?}");
    let path = w.notes_dir().join("_all/_global/tui-note.md");
    assert!(path.exists());
    let script = w
        .tui(&["attach", "lb"])
        .send_when("lb", "\x11L")
        .send_when("2 Notes", "2")
        // By what it is about; `x` deletes through the daemon's question.
        .send_when("global (about no project)", "x")
        .send_when("Forget 1 item: note", "y")
        // The form: every option the command takes.
        .send_when("forgot 1 item", "n")
        // (Its last row: a screen read while the form is still being drawn has only the
        // rows above it.)
        .until("In your editor");
    let r = ptyro_testkit::headless::run(script).await.expect("headless run");
    let shown = &r.screen;
    assert!(shown.contains("Write a note") && shown.contains("In your editor"), "{shown}");
    assert!(shown.contains("About") && shown.contains("Tags"), "{shown}");
    assert!(!path.exists(), "the note's file is deleted");
    assert!(w.found("zqtuinote").is_empty());
}
