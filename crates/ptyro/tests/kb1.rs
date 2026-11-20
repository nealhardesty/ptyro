//! KB1 end to end (SPEC §25): the knowledge base's file as a daemon keeps it. KB1.1: `kb.db` is
//! made when the daemon starts, private; `ptyro doctor`, `ptyro about` and the uninstall plan
//! name it; it is closed before a live upgrade's exec and opened again after; switched off, it
//! is not made, and a file that can't be opened is a reason with its fix while the daemon runs.
//! KB1.4: an agent's session is kept as it happens, from the agent's own transcript.
//! KB1.7: `ptyro recall` finds it, `ptyro kb show`, `recent` and `status` read it, `kb forget`
//! deletes it for good, and an agent that runs them itself stays behind the wall.
//! KB1.11: each switch keeps a session out, and retention deletes an old one.
//! KB1.14: a private session leaves no byte, by any trigger, and stays private.

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::kb::ingest::{self, Body, Gate, Never, Source, Stamp};
use ptyro_core::kb::redact::Redactor;
use ptyro_core::kb::{ChunkKind, ItemKind, KbChunk, KbItem, Writer};
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_core::status::AgentStatus;
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::{ItemFilter, KbDb};
use ptyro_testkit::{TempDaemon, fake_transcript};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-kb1-").tempdir().unwrap();
        let hs = ptyro_core::os::canonicalize(home.path()).unwrap().to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn out(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    }

    fn kb_db(&self) -> PathBuf {
        self.d.home.path().join("data/kb.db")
    }

    /// `ptyro doctor`'s line about the knowledge base.
    fn doctor_line(&self) -> String {
        let all = self.out(&["doctor", "--quick"]);
        all.lines()
            .find(|l| l.contains("knowledge base"))
            .unwrap_or_else(|| panic!("no knowledge base line in:\n{all}"))
            .to_owned()
    }
}

/// A document written into `kb.db` from outside the daemon (nothing in KB1.1 to KB1.3 gives a
/// client a way in yet), through the same plan and writer the daemon uses.
async fn write_doc(path: &std::path::Path, uri: &str, text: &str) {
    let db = KbDb::open(path, None).unwrap();
    let never = Never::default();
    let passed = ingest::gate(&Gate::open(&never)).unwrap();
    let source = Source::new(
        ItemKind::File,
        uri,
        Body::Markdown { name: "a.md".into(), text: text.into(), at_ms: 1 },
    );
    let plan =
        ingest::plan(passed, source, Stamp::new(Writer::File, None), &Redactor::default()).unwrap();
    db.write(plan).await.unwrap();
    db.checkpoint().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_keeps_kb_db_and_doctor_about_and_uninstall_name_it() {
    let w = World::new("");
    let db = w.kb_db();
    // (The socket takes a connection before the daemon has finished starting; an answer
    // means it has.)
    let pid = w.out(&["daemon", "status", "--json"]);
    assert!(db.exists(), "made when the daemon starts, before anything asks for it");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&db).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let line = w.doctor_line();
    assert!(
        line.contains("knowledge base: nothing yet in 0 piece(s), nothing forgotten"),
        "{line}"
    );
    assert!(line.contains("kb.db (schema 1)"), "{line}");

    let about = w.out(&["about"]);
    assert!(about.lines().any(|l| l.starts_with("knowledge") && l.ends_with("kb.db")), "{about}");
    let json: serde_json::Value = serde_json::from_str(&w.out(&["about", "--json"])).unwrap();
    let place =
        json["places"].as_array().unwrap().iter().find(|p| p["name"] == "knowledge").unwrap();
    assert_eq!(place["exists"], true);

    let plan = w.out(&["uninstall", "--dry-run"]);
    assert!(
        plan.contains("knowledge     what the knowledge base kept of your work, kb.db ("),
        "{plan}"
    );
    assert!(db.exists(), "a dry run removes nothing");

    // Across a live upgrade: the same file, opened again by the new image.
    write_doc(&db, "file:///repo/a.md", "# A\n\nkept across the upgrade").await;
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: 1 file in 1 piece(s)"), "{line}");
    let up = w.cli(&["daemon", "upgrade"]);
    assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));
    let after = w.out(&["daemon", "status", "--json"]);
    let field = |s: &str, k: &str| serde_json::from_str::<serde_json::Value>(s).unwrap()[k].clone();
    assert_eq!(field(&pid, "pid"), field(&after, "pid"), "an upgrade in place");
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: 1 file in 1 piece(s)"), "{line}");
    // And the new image's writer has it: another write from outside is seen at once.
    write_doc(&db, "file:///repo/b.md", "# B\n\nwritten after").await;
    assert!(w.doctor_line().contains("knowledge base: 2 files in 2 piece(s)"));
}

#[test]
fn switched_off_it_is_not_made_and_a_file_that_cannot_be_opened_says_why() {
    let mut w = World::new("[kb]\nenabled = false\n");
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: off ([kb] enabled = false)"), "{line}");
    assert!(!w.kb_db().exists(), "nothing is made while it is off");
    let plan = w.out(&["uninstall", "--dry-run"]);
    assert!(!plan.contains("kb.db"), "{plan}");

    // Switched on over a file that is no database: the daemon runs, sessions start as ever,
    // and the reason comes with what to do. The file is left as it was.
    std::fs::write(w.kb_db(), "not a database at all, just some text").unwrap();
    w.d.write_config("").unwrap();
    w.d.restart().unwrap();
    let o = w.cli(&["new", "-d", "-n", "plain", "--", "sh", "-c", "sleep 30"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let line = w.doctor_line();
    assert!(line.contains("the knowledge base is unavailable"), "{line}");
    assert!(line.contains("is damaged: move it aside"), "{line}");
    assert_eq!(
        std::fs::read_to_string(w.kb_db()).unwrap(),
        "not a database at all, just some text"
    );
}

// ── KB1.4: agent sessions (SPEC §25.6.1) ────────────────────────────────────────────────────
//
// The fake agent keeps a transcript as the real Claude Code and Codex do
// (`PTYRO_FAKE_TRANSCRIPT=1`, `ptyro_testkit::fake_transcript`) and fires their hooks; nothing
// else is done, and `kb.db` then holds the session.

const WAIT: Duration = Duration::from_secs(20);

/// Every agent is the fake one, keeping a transcript; `env` is more of its environment.
fn agents(env: &str) -> String {
    let mut c = String::from(
        "[chat]\ntitle_command = [\"sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n",
    );
    for a in ["claude", "codex", "opencode"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n\
             env = {{ PTYRO_FAKE_TRANSCRIPT = \"1\", PTYRO_FAKE_TURNS = \"1\"{env} }}\n"
        ));
    }
    c
}

/// A session's item and its chunks, as `kb.db` holds them now.
struct Kept {
    item: KbItem,
    chunks: Vec<KbChunk>,
}

impl Kept {
    /// Each chunk as `kind| text` (its first line).
    fn shown(&self) -> Vec<String> {
        self.chunks
            .iter()
            .map(|c| format!("{}| {}", c.kind, c.text.lines().next().unwrap_or("")))
            .collect()
    }

    fn card(&self) -> &str {
        assert_eq!(self.chunks[0].kind, ChunkKind::Card, "the card is the first chunk");
        assert_eq!(self.chunks.iter().filter(|c| c.kind == ChunkKind::Card).count(), 1);
        &self.chunks[0].text
    }

    fn has(&self, kind: ChunkKind, text: &str) -> bool {
        self.chunks.iter().any(|c| c.kind == kind && c.text == text)
    }
}

impl World {
    /// `kb.db`, read beside the daemon (which is its only writer). Opened for each look and
    /// closed after it: this process also reads the file's bytes (`bytes_hold`), and closing
    /// any handle of a file gives up the locks a connection in the same process holds on it.
    fn db(&self) -> KbDb {
        KbDb::open(&self.kb_db(), None).unwrap()
    }

    async fn sessions(&self) -> Vec<KbItem> {
        let f = ItemFilter { kinds: vec![ItemKind::Session], ..ItemFilter::default() };
        self.db().list(f).await.unwrap()
    }

    /// Wait until the item at `uri` is there and `pred` holds of it.
    async fn kept(&self, uri: &str, what: &str, pred: impl Fn(&Kept) -> bool) -> Kept {
        let deadline = Instant::now() + WAIT;
        loop {
            let db = self.db();
            let found = match db.item_by_uri(uri.to_owned()).await.unwrap() {
                Some(item) => {
                    let chunks = db.chunks(item.id.clone(), 0, 0).await.unwrap();
                    Some(Kept { item, chunks })
                }
                None => None,
            };
            drop(db);
            match found {
                Some(k) if pred(&k) => return k,
                found => assert!(
                    Instant::now() < deadline,
                    "{uri}: never {what}; it holds {:?}; the daemon said:\n{}",
                    found.map(|k| k.shown()),
                    self.log("knowledge base")
                ),
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn agent(&self, c: &Client, name: &str, agent: &str) -> SessionInfo {
        self.agent_in(c, name, agent, None).await
    }

    /// An agent session under `profile` (`None`: the default one).
    async fn agent_in(
        &self,
        c: &Client,
        name: &str,
        agent: &str,
        profile: Option<&str>,
    ) -> SessionInfo {
        let spec = SpawnSpec {
            agent: Some(agent.into()),
            profile: profile.map(Into::into),
            cwd: self.home(),
            name: Some(name.into()),
            size: Some(TermSize::new(100, 20)),
            ..SpawnSpec::default()
        };
        let info = match c.request(Request::Spawn { spec }).await.expect("spawn") {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        self.d.wait_for_text(c, name, "resumed=", WAIT).await.unwrap();
        info
    }

    /// Type a line into the session and wait until its turn is over.
    async fn turn(&self, c: &Client, name: &str, line: &str) {
        let before = self.info(c, name).await.turns;
        self.type_line(c, name, line).await;
        let deadline = Instant::now() + WAIT;
        while self.info(c, name).await.turns == before {
            assert!(Instant::now() < deadline, "{name}: {line:?} never ended its turn");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    /// The daemon's log lines that hold `word`.
    fn log(&self, word: &str) -> String {
        let mut lines = Vec::new();
        for e in std::fs::read_dir(self.d.paths.log_dir()).into_iter().flatten().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            lines.extend(text.lines().filter(|l| l.contains(word)).map(str::to_owned));
        }
        lines.join("\n")
    }

    /// Type a line into the session, once its agent takes input.
    async fn type_line(&self, c: &Client, name: &str, line: &str) {
        c.request(Request::SendText {
            addr: name.into(),
            text: line.into(),
            enter: true,
            wait_ready_ms: Some(15_000),
            once: None,
        })
        .await
        .unwrap();
    }

    /// Hibernate the session and bring it back: the same conversation, a new process.
    async fn resume(&self, c: &Client, name: &str) {
        self.out(&["hibernate", name]);
        self.dormant(c, name).await;
        self.wake(c, name).await;
    }

    async fn dormant(&self, c: &Client, name: &str) {
        let deadline = Instant::now() + WAIT;
        while self.info(c, name).await.state != ProcessState::Dormant {
            assert!(Instant::now() < deadline, "{name}: never dormant");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    /// Resume a dormant session and wait until its agent has started.
    async fn wake(&self, c: &Client, name: &str) {
        self.out(&["resume", name]);
        let deadline = Instant::now() + WAIT;
        loop {
            let i = self.info(c, name).await;
            if i.state == ProcessState::Running
                && i.status.as_ref().is_some_and(|r| r.status == AgentStatus::Idle)
            {
                return;
            }
            assert!(Instant::now() < deadline, "{name}: never back; {:?} {:?}", i.state, i.status);
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn info(&self, c: &Client, name: &str) -> SessionInfo {
        match c.request(Request::Get { addr: name.into() }).await.unwrap() {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        }
    }

    /// Whether any file of the knowledge base holds `needle` (the file, its log, its index).
    fn bytes_hold(&self, needle: &str) -> bool {
        std::fs::read_dir(self.d.home.path().join("data"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("kb.db"))
            .any(|e| {
                let bytes = std::fs::read(e.path()).unwrap_or_default();
                bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            })
    }
}

/// Two lines appended to a Claude transcript by hand: what the agent wrote while nobody was
/// told (the daemon down, a hook lost).
fn claude_lines(path: &std::path::Path, conversation: &str, tag: &str, prompt: &str, reply: &str) {
    use std::io::Write;
    let line = |kind: &str, content: serde_json::Value| {
        format!(
            "{}\n",
            serde_json::json!({ "type": kind, "uuid": format!("{tag}-{kind}"),
                "sessionId": conversation, "timestamp": "2026-11-19T23:00:00.000Z",
                "message": { "role": kind, "content": content } })
        )
    };
    let text = line("user", serde_json::json!(prompt))
        + &line("assistant", serde_json::json!([{ "type": "text", "text": reply }]));
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_session_is_kept_as_it_happens_across_a_resume_a_restart_and_an_upgrade() {
    let mut w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    let s = w.agent(&c, "kc", "claude").await;
    let conv = s.conversation.clone().unwrap();
    let uri = format!("session://claude/{conv}");
    let notes = w.home().join("notes.txt").to_string_lossy().into_owned();

    // One turn: the item, its card and its messages, with nothing asked of anyone.
    w.turn(&c, "kc", "edit notes.txt hello").await;
    let k =
        w.kept(&uri, "the first turn", |k| k.has(ChunkKind::Assistant, "wrote notes.txt")).await;
    assert_eq!(
        k.shown()[1..],
        [
            "user| edit notes.txt hello".to_owned(),
            format!("tool| Write: {notes}"),
            "assistant| wrote notes.txt".to_owned()
        ]
    );
    let card = k.card();
    for line in [
        "Task: edit notes.txt hello",
        "Outcome: wrote notes.txt",
        &format!("Files changed: {notes}"),
        "Agent: claude (claude-opus-5-5)",
    ] {
        assert!(card.lines().any(|l| l == line), "{line:?} in:\n{card}");
    }
    assert!(card.contains("Started 20") && card.ends_with(", 1 turn"), "{card}");
    let i = &k.item;
    assert_eq!((i.kind, i.writer), (ItemKind::Session, Writer::Agent));
    assert_eq!(i.session.as_deref(), Some(s.id.full().as_str()), "ptyro's session");
    assert_eq!((i.agent.as_deref(), i.model.as_deref()), (Some("claude"), Some("claude-opus-5-5")));
    assert_eq!(i.profile.as_deref(), Some("default"));
    assert_eq!(i.title.as_deref(), Some("edit notes.txt hello"));
    assert_eq!(i.files, std::slice::from_ref(&notes));
    assert!(i.started_ms.is_some() && i.started_ms <= i.ended_ms, "{i:?}");
    let transcript = PathBuf::from(i.cursor.as_ref().unwrap()["path"].as_str().unwrap());
    assert!(transcript.ends_with(format!("{conv}.jsonl")), "{transcript:?}");
    assert_eq!(
        i.cursor.as_ref().unwrap()["offset"],
        std::fs::metadata(&transcript).unwrap().len(),
        "read to its end"
    );
    // What a transcript holds beside the conversation never arrives: the account's email
    // (an `attachment` line), a tool's result, thinking.
    let raw = std::fs::read_to_string(&transcript).unwrap();
    assert!(raw.contains(fake_transcript::ACCOUNT) && raw.contains(fake_transcript::RESULT_MARK));
    for never in [fake_transcript::ACCOUNT, fake_transcript::RESULT_MARK, "queue-operation"] {
        assert!(!w.bytes_hold(never), "{never} is in kb.db");
    }

    // A second turn appends; the card is made again, in its place.
    w.turn(&c, "kc", "sh echo two").await;
    let k = w.kept(&uri, "the second turn", |k| k.has(ChunkKind::Assistant, "ran: two")).await;
    assert_eq!(
        k.shown()[4..],
        ["user| sh echo two", "tool| Bash: echo two", "assistant| ran: two"]
    );
    assert!(k.card().contains("Task: edit notes.txt hello\nOutcome: ran: two\n"), "{}", k.card());
    assert!(k.card().ends_with(", 2 turns"), "{}", k.card());

    // A compaction fires no hook: its summary arrives with the next turn.
    w.type_line(&c, "kc", "compact what was done so far").await;
    w.d.wait_for_text(&c, "kc", "compacted", WAIT).await.unwrap();
    w.turn(&c, "kc", "sh echo three").await;
    let k = w.kept(&uri, "the third turn", |k| k.has(ChunkKind::Assistant, "ran: three")).await;
    assert_eq!(
        k.shown()[7..],
        [
            "summary| 1. Primary Request and Intent:",
            "user| /compact",
            "user| sh echo three",
            "tool| Bash: echo three",
            "assistant| ran: three"
        ]
    );
    let summary = &k.chunks[7].text;
    assert!(summary.ends_with("what was done so far"), "{summary}");
    assert!(!summary.contains("This session is being continued"), "{summary}");

    // A resume carries on in the same item.
    w.resume(&c, "kc").await;
    w.turn(&c, "kc", "sh echo four").await;
    let k = w
        .kept(&uri, "the turn after the resume", |k| k.has(ChunkKind::Assistant, "ran: four"))
        .await;
    assert_eq!(k.item.id, i.id);
    assert_eq!(k.shown().len(), 15);
    assert_eq!(w.sessions().await.len(), 1, "one item a conversation");

    // Lines written while the daemon is down: at its start it finds that the transcript
    // grew past its cursor.
    drop(c);
    w.d.stop();
    claude_lines(&transcript, &conv, "down", "said while the daemon was down", "heard later");
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    w.dormant(&c, "kc").await;
    let k = w
        .kept(&uri, "what grew while it was down", |k| k.has(ChunkKind::Assistant, "heard later"))
        .await;
    assert!(k.has(ChunkKind::User, "said while the daemon was down"));
    assert_eq!(k.shown().len(), 17);
    assert!(k.card().contains("Outcome: heard later\n"), "{}", k.card());
    assert!(k.card().contains("Task: edit notes.txt hello\n"), "the task is kept: {}", k.card());

    // A live upgrade in the middle of the session: the queue is not carried over, and what
    // was not read yet is found from the cursor; the session's next turn arrives as ever.
    w.wake(&c, "kc").await;
    w.turn(&c, "kc", "sh echo five").await;
    w.kept(&uri, "the fifth turn", |k| k.has(ChunkKind::Assistant, "ran: five")).await;
    claude_lines(&transcript, &conv, "exec", "said just before the upgrade", "heard after it");
    let up = w.cli(&["daemon", "upgrade"]);
    assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));
    let k = w
        .kept(&uri, "what was unread at the upgrade", |k| {
            k.has(ChunkKind::Assistant, "heard after it")
        })
        .await;
    assert_eq!(k.shown().len(), 22);
    let c = w.d.client().await.unwrap();
    w.turn(&c, "kc", "sh echo six").await;
    let k = w
        .kept(&uri, "the turn after the upgrade", |k| k.has(ChunkKind::Assistant, "ran: six"))
        .await;
    // Nothing twice, nothing lost: every message once, in the file's order.
    let said: Vec<&str> = k
        .chunks
        .iter()
        .filter(|c| matches!(c.kind, ChunkKind::User | ChunkKind::Assistant))
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(
        said,
        [
            "edit notes.txt hello",
            "wrote notes.txt",
            "sh echo two",
            "ran: two",
            "/compact",
            "sh echo three",
            "ran: three",
            "sh echo four",
            "ran: four",
            "said while the daemon was down",
            "heard later",
            "sh echo five",
            "ran: five",
            "said just before the upgrade",
            "heard after it",
            "sh echo six",
            "ran: six",
        ]
    );
    assert_eq!(w.sessions().await.len(), 1);
    assert!(
        w.doctor_line().contains("knowledge base: 1 session in 25 piece(s)"),
        "{}",
        w.doctor_line()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_session_is_kept_with_what_its_tools_really_ran() {
    let w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    w.agent(&c, "kx", "codex").await;
    w.turn(&c, "kx", "edit plan.md first draft").await;
    let conv = w.info(&c, "kx").await.conversation.unwrap();
    let uri = format!("session://codex/{conv}");
    let plan = w.home().join("plan.md").to_string_lossy().into_owned();
    let k = w.kept(&uri, "the first turn", |k| k.has(ChunkKind::Assistant, "wrote plan.md")).await;
    // The tool's title is what ran (the `item_completed` line), not the `exec` program.
    assert_eq!(
        k.shown()[1..],
        [
            "user| edit plan.md first draft".to_owned(),
            format!("tool| patch: {plan}"),
            "assistant| wrote plan.md".to_owned()
        ]
    );
    assert_eq!(
        (k.item.agent.as_deref(), k.item.model.as_deref()),
        (Some("codex"), Some("gpt-5.6-sol"))
    );
    assert_eq!(k.item.files, std::slice::from_ref(&plan));
    assert!(k.card().contains(&format!("Files changed: {plan}\n")), "{}", k.card());
    for never in
        [fake_transcript::ACCOUNT, fake_transcript::RESULT_MARK, "AGENTS.md", "tools.Write"]
    {
        assert!(!w.bytes_hold(never), "{never} is in kb.db");
    }

    // A resume appends to the same rollout, and so to the same item.
    w.resume(&c, "kx").await;
    w.turn(&c, "kx", "sh echo again").await;
    let k = w
        .kept(&uri, "the turn after the resume", |k| k.has(ChunkKind::Assistant, "ran: again"))
        .await;
    assert_eq!(
        k.shown()[4..],
        ["user| sh echo again", "tool| shell: echo again", "assistant| ran: again"]
    );
    assert!(k.card().ends_with(", 2 turns"), "{}", k.card());
    assert_eq!(w.sessions().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_written_after_stop_is_read_without_another_turn() {
    // Claude writes the turn's last line after `Stop` has fired (15 ms; here most of a
    // second, so that the first read and the one 250 ms later both come too early).
    let w = World::new(&agents(", PTYRO_FAKE_TRANSCRIPT_LAG_MS = \"800\""));
    let c = w.d.client().await.unwrap();
    let s = w.agent(&c, "late", "claude").await;
    let uri = format!("session://claude/{}", s.conversation.unwrap());
    w.turn(&c, "late", "sh echo late").await;
    let early = w.kept(&uri, "the prompt", |k| k.has(ChunkKind::User, "sh echo late")).await;
    assert!(!early.has(ChunkKind::Assistant, "ran: late"), "the reply was not in the file yet");
    // (Its card has the outcome already: the hook said it.)
    assert!(early.card().contains("Outcome: ran: late\n"), "{}", early.card());
    let k = w.kept(&uri, "the reply", |k| k.has(ChunkKind::Assistant, "ran: late")).await;
    assert!(k.card().contains("Outcome: ran: late\n"), "{}", k.card());
    assert_eq!(k.shown().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rewind_doubles_nothing() {
    let w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    let s = w.agent(&c, "rw", "claude").await;
    let uri = format!("session://claude/{}", s.conversation.unwrap());
    w.turn(&c, "rw", "sh echo kept").await;
    w.turn(&c, "rw", "sh echo a-branch-that-is-rewound-and-is-longer-than-what-follows").await;
    let before = w.kept(&uri, "two turns", |k| k.shown().len() == 7).await;
    // Back to the end of the first turn, then another way on: the file is shorter than the
    // cursor, so it is read from its start, and its first turn is found by its anchors.
    w.type_line(&c, "rw", "rewind").await;
    w.d.wait_for_text(&c, "rw", "rewound", WAIT).await.unwrap();
    w.turn(&c, "rw", "sh echo b").await;
    let k = w.kept(&uri, "the new branch", |k| k.has(ChunkKind::Assistant, "ran: b")).await;
    assert_eq!(
        k.shown()[1..],
        [
            "user| sh echo kept",
            "tool| Bash: echo kept",
            "assistant| ran: kept",
            // The rewound branch stays: this is a record.
            "user| sh echo a-branch-that-is-rewound-and-is-longer-than-what-follows",
            "tool| Bash: echo a-branch-that-is-rewound-and-is-longer-than-what-follows",
            "assistant| ran: a-branch-that-is-rewound-and-is-longer-than-what-follows",
            "user| sh echo b",
            "tool| Bash: echo b",
            "assistant| ran: b",
        ]
    );
    // The rows of the first turn are the rows they were (their vectors stay with them).
    for n in 1..4 {
        assert_eq!(k.chunks[n].seq, before.chunks[n].seq);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_with_no_transcript_gets_its_card_and_a_chats_engine_gets_nothing() {
    let w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    // A chat: its engine is a session of an agent, and its text is `chat.db`'s alone.
    let chat = w.cli(&["chat", "new", "what is kept of a chat"]);
    assert!(chat.status.success(), "{}", String::from_utf8_lossy(&chat.stderr));

    // OpenCode's sessions are not read: the item is its card, from what ptyro saw.
    w.agent(&c, "oc", "opencode").await;
    w.turn(&c, "oc", "sh echo from-opencode").await;
    let uri = format!("session://opencode/{}", w.info(&c, "oc").await.id.full());
    let k = w.kept(&uri, "its card", |k| !k.chunks.is_empty()).await;
    assert_eq!(k.chunks.len(), 1);
    assert!(k.card().starts_with("Agent: opencode\nStarted 20"), "{}", k.card());
    assert!(k.card().ends_with(", 1 turn"), "{}", k.card());
    assert_eq!((k.item.cursor.clone(), k.item.agent.as_deref()), (None, Some("opencode")));

    // By now the chat's turn is long over: nothing of it is here.
    let all = w.sessions().await;
    assert_eq!(all.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(), [uri.as_str()]);
    assert!(!w.bytes_hold("what is kept of a chat"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profiles_secrets_and_what_looks_like_a_key_never_reach_the_file() {
    let config = format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\n[profiles.work.secrets]\nDB_PASS = 'hunter2-value'\n",
        agents("")
    );
    let w = World::new(&config);
    let c = w.d.client().await.unwrap();
    let s = w.agent_in(&c, "sec", "claude", Some("work")).await;
    let uri = format!("session://claude/{}", s.conversation.unwrap());
    // Said by the user, run by a tool and said back by the agent: the profile's secret by its
    // value, and a token by its shape.
    w.turn(&c, "sec", "sh echo hunter2-value and API_TOKEN=abc123def456").await;
    let k =
        w.kept(&uri, "the turn", |k| k.chunks.iter().any(|c| c.kind == ChunkKind::Assistant)).await;
    assert_eq!(
        k.shown()[1..],
        [
            "user| sh echo <secret:DB_PASS> and API_TOKEN=<redacted>",
            "tool| Bash: echo <secret:DB_PASS> and API_TOKEN=<redacted>",
            "assistant| ran: <secret:DB_PASS> and API_TOKEN=<redacted>",
        ]
    );
    assert!(k.card().starts_with("Task: sh echo <secret:DB_PASS> and API_TOKEN=<redacted>\n"));
    assert_eq!(k.item.title.as_deref(), Some("sh echo <secret:DB_PASS> and API_TOKEN=<redacted>"));
    // The item is the profile's, and its transcript was found in the profile's own folder.
    assert_eq!(k.item.profile.as_deref(), Some("work"));
    let path = k.item.cursor.as_ref().unwrap()["path"].as_str().unwrap().to_owned();
    assert!(path.contains("/.claude-work/projects/"), "{path}");
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("hunter2-value"),
        "it was there to redact"
    );
    for never in ["hunter2-value", "abc123def456"] {
        assert!(!w.bytes_hold(never), "{never} is in kb.db");
    }
}

// ── KB1.7, the commands, and KB1.11, the switches ───────────────────────────────────────────

impl World {
    /// A command's output as JSON.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let o = self.cli(args);
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\n{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
    }

    /// Wait until `ptyro kb status` says `pred`.
    async fn status(
        &self,
        what: &str,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let s = self.json(&["kb", "status", "--json"]);
            if pred(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "the status never said {what}: {s:#}");
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    }

    /// Wait until the knowledge base says why it did not keep `uri`.
    async fn skipped(&self, uri: &str, why: &str) {
        self.status(&format!("{uri} was not kept ({why})"), |s| {
            s["skipped"].as_array().is_some_and(|k| {
                k.iter()
                    .any(|k| k["uri"] == uri && k["why"].as_str().is_some_and(|w| w.contains(why)))
            })
        })
        .await;
    }

    /// An agent session under `profile`, in `cwd`.
    async fn agent_at(
        &self,
        c: &Client,
        name: &str,
        agent: &str,
        profile: Option<&str>,
        cwd: PathBuf,
    ) -> SessionInfo {
        let spec = SpawnSpec {
            agent: Some(agent.into()),
            profile: profile.map(Into::into),
            cwd,
            name: Some(name.into()),
            size: Some(TermSize::new(100, 20)),
            ..SpawnSpec::default()
        };
        let info = match c.request(Request::Spawn { spec }).await.expect("spawn") {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        self.d.wait_for_text(c, name, "resumed=", WAIT).await.unwrap();
        info
    }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn recall_finds_a_session_kb_shows_it_and_forgetting_is_for_good() {
    let w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    let s = w.agent(&c, "rc", "claude").await;
    let uri = format!("session://claude/{}", s.conversation.clone().unwrap());
    w.turn(&c, "rc", "edit notes.txt zqrecallme").await;
    w.kept(&uri, "the turn", |k| k.has(ChunkKind::Assistant, "wrote notes.txt")).await;

    // `recall`: one hit a line where nothing is a terminal, the match marked.
    let found = w.cli(&["recall", "zqrecallme"]);
    assert!(found.status.success(), "{found:?}");
    let out = String::from_utf8_lossy(&found.stdout).into_owned();
    let line = out.lines().next().unwrap_or_else(|| panic!("no hit: {found:?}"));
    assert!(
        line.starts_with("k:") && line.contains("  session · edit notes.txt zqrecallme · "),
        "{line}"
    );
    assert!(line.contains(" · claude · ") && line.contains("*zqrecallme*"), "{line}");
    // `--json` is the list of hits.
    let hits = w.json(&["recall", "--json", "zqrecallme"]);
    let hit = &hits[0];
    assert_eq!(
        (&hit["uri"], &hit["kind"], &hit["how"]),
        (&uri.clone().into(), &"session".into(), &"words".into())
    );
    assert_eq!(hit["agent"], "claude");
    // Its workspace by id, and what that is called: `workspace:` finds it by either.
    assert_eq!(hit["workspace_name"], "main");
    assert_ne!(hit["workspace"], "main", "an item is stamped with its workspace's id");
    assert_eq!(w.json(&["recall", "--json", "zqrecallme", "workspace:main"])[0]["uri"], hit["uri"]);
    let addr = hit["addr"].as_str().unwrap().to_owned();
    // The best hit is the session's card, which says the task; the message is its neighbour.
    let (item, piece) = addr.split_once('/').unwrap();
    assert_eq!((piece, hit["part"].as_str()), ("0", Some("card")));
    let message = format!("{item}/1");
    // The filters, as flags and in the query; flags typed after the words are flags still.
    for args in [
        &["recall", "zqrecallme", "--kind", "session", "-l", "1", "--since", "1d", "--words"][..],
        &["recall", "-k", "session", "zqrecallme", "kind:session", "agent:claude"],
    ] {
        let o = w.cli(args);
        assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 1, "{args:?}: {o:?}");
    }
    for args in [
        &["recall", "zqrecallme", "--kind", "chat"][..],
        &["recall", "zqrecallme", "--before", "1d"],
        &["recall", "zqrecallme", "agent:codex"],
        &["recall", "zqnothing"],
    ] {
        let o = w.cli(args);
        assert!(o.status.success() && o.stdout.is_empty(), "{args:?}: {o:?}");
        assert!(String::from_utf8_lossy(&o.stderr).contains("nothing matches"), "{o:?}");
    }
    let e = w.out(&["recall", "zqrecallme", "--kind", "sesion"]);
    assert!(e.contains("no kind is called \"sesion\": session, chat, note,"), "{e}");
    // By meaning while it is off: it says how to switch it on, by its name now.
    let o = w.cli(&["recall", "zqrecallme", "--meaning"]);
    let e = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(!o.status.success() && e.contains("meaning search is off"), "{o:?}");
    assert!(e.contains("ptyro config set kb.meaning true"), "{e}");

    // `kb show`: the piece with its neighbours, the one named marked; an item, all of it.
    let shown = w.out(&["kb", "show", &message]);
    assert!(
        shown.starts_with(&format!("{item}  session · edit notes.txt zqrecallme · ")),
        "{shown}"
    );
    assert!(shown.contains(&uri), "{shown}");
    assert!(
        shown.contains(&format!("▶ {message}  user\n    edit notes.txt zqrecallme\n")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("\n  {addr}  card\n    Task: edit notes.txt zqrecallme\n")),
        "{shown}"
    );
    assert!(shown.contains("  assistant\n    wrote notes.txt\n"), "{shown}");
    let one = w.out(&["kb", "show", &message, "--context", "0"]);
    assert!(one.contains("zqrecallme\n") && !one.contains("wrote notes.txt"), "{one}");
    let doc = w.json(&["kb", "show", item, "--json"]);
    assert_eq!(
        (&doc["item"]["item"]["uri"], &doc["item"]["addr"]),
        (&uri.clone().into(), &item.into())
    );
    assert_eq!(doc["chunks"].as_array().unwrap().len(), 4, "{doc:#}");
    assert_eq!(doc["chunks"][1]["text"], "edit notes.txt zqrecallme");
    let e = w.cli(&["kb", "show", "k:zzzzzz"]);
    assert_eq!(e.status.code(), Some(3), "{e:?}");

    // `kb recent`: by day; `kb`: what it holds.
    let recent = w.out(&["kb", "recent"]);
    let mut lines = recent.lines();
    assert!(lines.next().is_some_and(|d| d.len() == 10 && d.starts_with("20")), "{recent}");
    let row = lines.next().unwrap();
    assert!(row.contains(&format!("  {item}  session · edit notes.txt zqrecallme · ")), "{recent}");
    assert!(row.ends_with(" · claude · 4 pieces"), "{recent}");
    let listed = w.json(&["kb", "recent", "--kind", "session", "--json"]);
    assert_eq!(
        (&listed[0]["addr"], &listed[0]["item"]["uri"]),
        (&item.into(), &uri.clone().into())
    );
    assert!(w.out(&["kb", "recent", "--kind", "note"]).contains("nothing is kept yet"));
    let status = w.out(&["kb"]);
    assert!(status.starts_with("knowledge base: on · ") && status.contains("kb.db ("), "{status}");
    assert!(status.contains("\n  1 session · 4 pieces\n"), "{status}");
    assert!(status.contains("\n  sessions are kept for good"), "{status}");
    let s = w.json(&["kb", "--json"]);
    assert_eq!(
        (&s["enabled"], &s["kinds"][0]["items"], &s["forgotten"]),
        (&true.into(), &1.into(), &0.into())
    );

    // `kb forget` asks first: with nobody to ask, nothing goes, and it says what would.
    let o = w.cli(&["kb", "forget", item]);
    let e = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(o.status.code(), Some(5), "{o:?}");
    assert!(e.contains("Forget 1 item: session “edit notes.txt zqrecallme”?"), "{e}");
    assert!(
        e.contains("deleted from the knowledge base for good") && e.contains("pass --yes"),
        "{e}"
    );
    assert!(w.bytes_hold("zqrecallme"));
    let o = w.cli(&["kb", "forget", "--query", "zqrecallme"]);
    assert_eq!(o.status.code(), Some(5), "{o:?}");
    // Agreed to: its text is in no byte of the file, and no search finds it.
    let o = w.cli(&["kb", "forget", "--query", "zqrecallme", "--yes"]);
    let e = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success() && e.contains("forgot 1 item (4 pieces)"), "{o:?}");
    assert!(!w.bytes_hold("zqrecallme"), "the forgotten text is still in kb.db");
    assert!(w.cli(&["recall", "zqrecallme"]).stdout.is_empty());
    assert!(w.cli(&["kb", "show", item]).status.code() == Some(3));
    // The session goes on, and nothing of it is kept again: not its next turn, not after a
    // restart of the daemon's reading (the transcript still holds all of it).
    w.turn(&c, "rc", "edit notes.txt zqagain").await;
    w.skipped(&uri, "forgotten").await;
    assert!(w.cli(&["recall", "zqagain"]).stdout.is_empty());
    assert!(!w.bytes_hold("zqagain") && !w.bytes_hold("zqrecallme"));
    let status = w.out(&["kb"]);
    assert!(status.contains("\n  1 forgotten: never kept again\n"), "{status}");
    assert!(status.contains(&format!("    {uri}: it was forgotten (")), "{status}");
    assert!(status.contains("nothing is kept yet"), "{status}");
    // Forgetting what isn't there, and a chat's address, say so.
    assert!(w.out(&["kb", "forget", "c:7f3k", "--yes"]).contains("ptyro chat rm"));
    assert!(
        w.out(&["kb", "forget", "--query", "zqnothing"])
            .contains("nothing in the knowledge base matches")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_switch_keeps_a_session_out() {
    // Codex is kept out by its agent's switch.
    let config = agents("").replace("[agents.codex]\n", "[agents.codex]\nkb = false\n");
    let w = World::new(&config);
    let c = w.d.client().await.unwrap();
    w.agent(&c, "sx", "codex").await;
    w.turn(&c, "sx", "sh echo zqcodexword").await;
    let codex = format!("session://codex/{}", w.info(&c, "sx").await.conversation.unwrap());
    w.skipped(&codex, "its agent is kept out").await;

    // A repository keeps itself out, with `[knowledge] enabled = false` in its `.ptyro.toml`.
    let repo = w.home().join("quiet");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".ptyro.toml"), "[knowledge]\nenabled = false\n").unwrap();
    git(&repo, &["add", ".ptyro.toml"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    assert!(w.cli(&["project", "add", &repo.to_string_lossy()]).status.success());
    let q = w.agent_at(&c, "sq", "claude", None, repo.clone()).await;
    assert!(q.project.is_some(), "the session is the project's: {q:?}");
    w.turn(&c, "sq", "edit notes.txt zqquietword").await;
    let quiet = format!("session://claude/{}", q.conversation.unwrap());
    w.skipped(&quiet, "its project is kept out").await;

    // A session of neither is kept.
    let s = w.agent(&c, "sk", "claude").await;
    let uri = format!("session://claude/{}", s.conversation.clone().unwrap());
    w.turn(&c, "sk", "edit notes.txt zqkeptword").await;
    w.kept(&uri, "the first turn", |k| k.has(ChunkKind::Assistant, "wrote notes.txt")).await;
    assert_eq!(w.sessions().await.len(), 1);
    for never in ["zqcodexword", "zqquietword"] {
        assert!(!w.bytes_hold(never) && w.cli(&["recall", never]).stdout.is_empty(), "{never}");
    }
    let status = w.out(&["kb"]);
    assert!(
        status.contains(&format!("    {codex}: its agent is kept out of the knowledge base (")),
        "{status}"
    );
    assert!(
        status.contains(&format!("    {quiet}: its project is kept out of the knowledge base (")),
        "{status}"
    );

    // `kb.sessions` off: no turn is read while it is; what was kept stays, and can be found.
    assert!(w.cli(&["config", "set", "kb.sessions", "false"]).status.success());
    w.status("sessions are off", |s| s["sessions"] == false).await;
    w.turn(&c, "sk", "edit notes.txt zqwhileoff").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!w.bytes_hold("zqwhileoff"));
    assert!(w.out(&["kb"]).contains("agent sessions are not kept (`kb.sessions` is off)"));
    assert!(!w.cli(&["recall", "zqkeptword"]).stdout.is_empty());
    // On again: a session that goes on is read from where reading stopped.
    assert!(w.cli(&["config", "set", "kb.sessions", "true"]).status.success());
    w.status("sessions are on", |s| s["sessions"] == true).await;
    w.turn(&c, "sk", "edit notes.txt zqbackon").await;
    let k =
        w.kept(&uri, "the turn after", |k| k.has(ChunkKind::User, "edit notes.txt zqbackon")).await;
    assert!(k.has(ChunkKind::User, "edit notes.txt zqwhileoff"), "{:?}", k.shown());

    // The knowledge base off: nothing is read, nothing is searched, and every command says
    // so and how to switch it on. What is stored stays in the file.
    assert!(w.cli(&["config", "set", "kb.enabled", "false"]).status.success());
    w.status("it is off", |s| s["enabled"] == false).await;
    w.turn(&c, "sk", "edit notes.txt zqalloff").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!w.bytes_hold("zqalloff") && w.bytes_hold("zqkeptword"));
    let status = w.out(&["kb"]);
    assert!(
        status.starts_with("knowledge base: off. Nothing is kept and nothing is searched.\n"),
        "{status}"
    );
    assert!(
        status.contains("what was kept before stays in ")
            && status.contains("ptyro config set kb.enabled true"),
        "{status}"
    );
    for args in [
        &["recall", "zqkeptword"][..],
        &["kb", "recent"],
        &["kb", "show", "k:7f3k"],
        &["kb", "forget", "k:7f3k", "--yes"],
    ] {
        let o = w.cli(args);
        let e = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(!o.status.success() && o.stdout.is_empty(), "{args:?}: {o:?}");
        assert_eq!(
            e.trim(),
            "ptyro: the knowledge base is off: `ptyro config set kb.enabled true` switches it on",
            "{args:?}"
        );
    }
    assert!(w.doctor_line().contains("knowledge base: off"));
    // And on again.
    assert!(w.cli(&["config", "set", "kb.enabled", "true"]).status.success());
    w.status("it is on", |s| s["enabled"] == true).await;
    assert!(!w.cli(&["recall", "zqkeptword"]).stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_deletes_old_sessions_once_it_is_set() {
    let w = World::new("");
    let _ = w.out(&["daemon", "status", "--json"]);
    // Kept long ago, written beside the daemon through the same plan and writer: a session
    // of 40 days ago, one of yesterday, and a document as old as the first.
    let day = 86_400_000;
    let now = ptyro_core::now_ms();
    {
        let db = w.db();
        let never = Never::default();
        for (kind, uri, at) in [
            (ItemKind::Session, "session://claude/old", now - 40 * day),
            (ItemKind::Session, "session://claude/new", now - day),
            (ItemKind::File, "file:///old.md", now - 40 * day),
        ] {
            let passed = ingest::gate(&Gate::open(&never)).unwrap();
            let text = format!("zqretention {uri}");
            let mut source =
                Source::new(kind, uri, Body::Plain { name: "x".into(), text, at_ms: at });
            (source.started_ms, source.ended_ms) = (Some(at), Some(at));
            let writer = if kind == ItemKind::Session { Writer::Agent } else { Writer::File };
            let plan = ingest::plan(passed, source, Stamp::new(writer, None), &Redactor::default())
                .unwrap();
            db.write(plan).await.unwrap();
        }
        db.checkpoint().await.unwrap();
    }
    let found = |w: &World| {
        let hits = w.json(&["recall", "--json", "zqretention"]);
        let mut uris: Vec<String> = hits
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["uri"].as_str().unwrap().to_owned())
            .collect();
        uris.sort();
        uris
    };
    assert_eq!(found(&w).len(), 3, "kept for good while `kb.retain_days` is 0");
    assert!(w.out(&["kb"]).contains("sessions are kept for good"));

    // Set, it acts at once (and then once a day): the old session goes, and nothing else.
    assert!(w.cli(&["config", "set", "kb.retain_days", "30"]).status.success());
    w.status("the old session is gone", |s| {
        s["retain_days"] == 30 && s["kinds"].to_string().contains("\"items\":1")
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while found(&w).len() != 2 {
        assert!(Instant::now() < deadline, "the old session was never deleted: {:?}", found(&w));
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert_eq!(found(&w), ["file:///old.md", "session://claude/new"]);
    assert!(!w.bytes_hold("zqretention session://claude/old"), "its text is still in kb.db");
    let status = w.out(&["kb"]);
    assert!(status.contains("\n  sessions older than 30 days are deleted\n"), "{status}");
    // No tombstone: it is counted as deleted, not as forgotten.
    assert_eq!(w.json(&["kb", "--json"])["forgotten"], 0);
    assert!(w.log("knowledge base").contains("1 item(s) older than 30 day(s) deleted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_runs_recall_sees_only_its_own_profiles_and_never_forgets() {
    let config = format!(
        "{}[profiles.work]\nclaude = \"~/.claude-work\"\n[profiles.home]\nclaude = \"~/.claude-home\"\n",
        agents("")
    );
    let w = World::new(&config);
    let c = w.d.client().await.unwrap();
    // A work session says a word.
    let a = w.agent_in(&c, "wa", "claude", Some("work")).await;
    let work = format!("session://claude/{}", a.conversation.unwrap());
    w.turn(&c, "wa", "edit notes.txt zqwallword").await;
    w.kept(&work, "the work turn", |k| k.has(ChunkKind::Assistant, "wrote notes.txt")).await;
    let addr = w.json(&["recall", "--json", "zqwallword"])[0]["addr"].as_str().unwrap().to_owned();

    // An agent of another profile runs `ptyro recall` in its terminal: the daemon knows whose
    // session the command comes from, and answers within the wall. (Its own session, which
    // holds the word it typed, is never found for it.)
    let b = w.agent_in(&c, "hb", "claude", Some("home")).await;
    let home = format!("session://claude/{}", b.conversation.unwrap());
    w.turn(&c, "hb", &format!("sh {BIN} recall --json zqwallword")).await;
    let k = w
        .kept(&home, "the home turn", |k| k.chunks.iter().any(|c| c.kind == ChunkKind::Assistant))
        .await;
    assert!(k.has(ChunkKind::Assistant, "ran: []"), "{:?}", k.shown());
    // Nor can it read the work session by its address, list it, or forget it.
    w.turn(&c, "hb", &format!("sh {BIN} kb show {addr}; echo code=$?")).await;
    w.kept(&home, "the show", |k| k.has(ChunkKind::Assistant, "ran: code=3")).await;
    w.turn(&c, "hb", &format!("sh {BIN} kb recent --json")).await;
    w.kept(&home, "the list", |k| k.chunks.iter().filter(|c| c.text == "ran: []").count() == 2)
        .await;
    w.turn(&c, "hb", &format!("sh {BIN} kb forget {addr} --yes; echo code=$?")).await;
    w.kept(&home, "the forget", |k| k.has(ChunkKind::Assistant, "ran: code=1")).await;

    // An agent of the same profile finds it, and not the other profile's session, which
    // holds the word too by now.
    let s = w.agent_in(&c, "wc", "claude", Some("work")).await;
    let same = format!("session://claude/{}", s.conversation.unwrap());
    w.turn(&c, "wc", &format!("sh {BIN} recall --json zqwallword")).await;
    let k = w
        .kept(&same, "the same profile's turn", |k| {
            k.chunks.iter().any(|c| c.kind == ChunkKind::Assistant)
        })
        .await;
    let said: String = k
        .chunks
        .iter()
        .filter(|c| c.kind == ChunkKind::Assistant)
        .map(|c| c.text.as_str())
        .collect();
    assert!(said.contains(&work) && !said.contains(&home) && !said.contains(&same), "{said}");
    // It may not forget either: what an agent could forget, it could hide.
    w.turn(&c, "wc", &format!("sh {BIN} kb forget {addr} --yes; echo code=$?")).await;
    w.kept(&same, "the forget", |k| k.has(ChunkKind::Assistant, "ran: code=1")).await;
    // You see all of it.
    let mine = w.json(&["recall", "--json", "zqwallword", "-l", "50"]);
    let uris: Vec<&str> =
        mine.as_array().unwrap().iter().filter_map(|h| h["uri"].as_str()).collect();
    for uri in [&work, &home, &same] {
        assert!(uris.contains(&uri.as_str()), "{uri} in {uris:?}");
    }
}

/// KB1.14 (SPEC §25.7a): a private session leaves no byte in `kb.db` by any trigger (a turn, a
/// resume, the daemon's start, a live upgrade), says so wherever it is listed, and stays
/// private; `kb.private` is the default a new session takes; and `kb forget --session` makes
/// a kept one private for good.
#[tokio::test(flavor = "multi_thread")]
async fn a_private_session_leaves_no_byte_and_stays_private() {
    let mut w = World::new(&agents(""));
    let c = w.d.client().await.unwrap();
    let o = w.cli(&["new", "--private", "--detach", "--agent", "claude", "--name", "sp"]);
    assert!(o.status.success(), "{o:?}");
    w.d.wait_for_text(&c, "sp", "resumed=", WAIT).await.unwrap();
    assert!(w.info(&c, "sp").await.private);
    w.turn(&c, "sp", "edit notes.txt zqprivateword").await;
    let private = w.info(&c, "sp").await;
    let conv = private.conversation.clone().expect("its conversation is known");
    let transcript = find_file(&w.home(), &format!("{conv}.jsonl")).expect("its transcript");
    assert!(
        std::fs::read_to_string(&transcript).unwrap().contains("zqprivateword"),
        "the agent keeps its own history"
    );

    // A session beside it is kept: the reader ran, and passed the private one by.
    let kept = w.agent(&c, "sk", "claude").await;
    let uri = format!("session://claude/{}", kept.conversation.clone().unwrap());
    w.turn(&c, "sk", "edit notes.txt zqkeptword").await;
    w.kept(&uri, "the first turn", |k| k.has(ChunkKind::Assistant, "wrote notes.txt")).await;
    let nothing_of_it = |w: &World, when: &str| {
        assert!(!w.bytes_hold("zqprivate"), "{when}: a byte of the private session is kept");
        assert!(w.cli(&["recall", "zqprivateword"]).stdout.is_empty(), "{when}");
    };
    nothing_of_it(&w, "after its turn");
    assert_eq!(w.sessions().await.len(), 1, "no item, and no card either");
    // Nothing was queued, so nothing was refused: the status names no skipped source.
    let status = w.json(&["kb", "status", "--json"]);
    assert_eq!(status["skipped"].as_array().map(Vec::len), Some(0), "{status:#}");

    // It shows: the list's mark, and `private` in its JSON.
    let ls = w.out(&["ls"]);
    let row = ls.lines().find(|l| l.contains(" sp ")).unwrap_or_else(|| panic!("{ls}"));
    assert!(row.contains("◌ private"), "{row}");
    assert!(!ls.lines().find(|l| l.contains(" sk ")).unwrap().contains('◌'), "{ls}");
    let rows = w.json(&["ls", "--json"]);
    let of = |name: &str| {
        rows.as_array().unwrap().iter().find(|s| s["name"] == name).unwrap()["private"].clone()
    };
    assert_eq!((of("sp"), of("sk")), (true.into(), false.into()));

    // A resume: the same conversation, still private.
    w.resume(&c, "sp").await;
    assert!(w.info(&c, "sp").await.private);
    w.turn(&c, "sp", "sh echo zqprivatetwo").await;
    nothing_of_it(&w, "after a resume");

    // The daemon's start reads every transcript that grew: not this one.
    drop(c);
    w.d.stop();
    claude_lines(&transcript, &conv, "down", "zqprivatedown said", "zqprivatedown heard");
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    w.dormant(&c, "sp").await;
    assert!(w.info(&c, "sp").await.private, "kept in ptyro.db across a daemon restart");
    // (The kept session's catch-up has run by the time the status answers with nothing queued.)
    w.status("nothing is queued", |s| s["queued"] == 0).await;
    nothing_of_it(&w, "after the daemon restarted");

    // A live upgrade, then a turn.
    w.wake(&c, "sp").await;
    claude_lines(&transcript, &conv, "exec", "zqprivateexec said", "zqprivateexec heard");
    let up = w.cli(&["daemon", "upgrade"]);
    assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));
    let c = w.d.client().await.unwrap();
    assert!(w.info(&c, "sp").await.private, "carried across a live upgrade");
    w.turn(&c, "sp", "sh echo zqprivatethree").await;
    w.status("nothing is queued", |s| s["queued"] == 0).await;
    nothing_of_it(&w, "after a live upgrade");
    assert_eq!(w.sessions().await.len(), 1);

    // A shell can be private too, and a pane split off it is.
    let o = w.cli(&["shells", "new", "--private", "--detach", "--name", "shp"]);
    assert!(o.status.success(), "{o:?}");
    assert!(w.info(&c, "shp").await.private);
    let pane = match c
        .request(Request::ShellSplit {
            addr: "shp".into(),
            dir: Default::default(),
            env: Default::default(),
            size: None,
        })
        .await
        .unwrap()
    {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    };
    assert!(pane.private, "a split of a private shell is private");

    // `kb.private`: what a new session takes when it doesn't say; `--no-private` keeps one.
    assert!(w.cli(&["config", "set", "kb.private", "true"]).status.success());
    let deadline = Instant::now() + WAIT;
    loop {
        match c.request(Request::Agents).await.unwrap() {
            Reply::Agents { private: true, .. } => break,
            _ => assert!(Instant::now() < deadline, "the daemon never said private is the default"),
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(w.agent(&c, "sd", "claude").await.private, "the default");
    let o = w.cli(&["new", "--no-private", "--detach", "--agent", "claude", "--name", "sn"]);
    assert!(o.status.success(), "{o:?}");
    assert!(!w.info(&c, "sn").await.private);
    assert!(w.cli(&["config", "set", "kb.private", "false"]).status.success());

    // `kb forget --session`: what was kept goes, and the session is private from then on.
    assert!(w.bytes_hold("zqkeptword"));
    let asked = w.cli(&["kb", "forget", "--session", "sk"]);
    assert!(!asked.status.success(), "it asks first, and nobody answered: {asked:?}");
    assert!(w.bytes_hold("zqkeptword") && !w.info(&c, "sk").await.private);
    let o = w.cli(&["kb", "forget", "--session", "sk", "--yes"]);
    let said = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success() && said.starts_with("sk is private now: forgot 1 item"), "{said}");
    assert!(w.info(&c, "sk").await.private);
    // (It has been dormant since the daemon restarted: resumed, it is still private.)
    w.wake(&c, "sk").await;
    assert!(w.info(&c, "sk").await.private);
    w.turn(&c, "sk", "sh echo zqkeptafter").await;
    w.status("nothing is queued", |s| s["queued"] == 0).await;
    for gone in ["zqkeptword", "zqkeptafter"] {
        assert!(!w.bytes_hold(gone), "{gone}");
    }
    assert!(w.sessions().await.is_empty(), "{:?}", w.sessions().await);
    // One nothing was read of yet: its conversation is never kept either.
    let o = w.cli(&["kb", "forget", "--session", "sn", "--yes"]);
    assert!(o.status.success(), "{o:?}");
    assert!(w.info(&c, "sn").await.private);
}

/// The file called `name` somewhere under `dir`.
fn find_file(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if e.file_name().to_string_lossy() == name {
            return Some(p);
        }
    }
    None
}
