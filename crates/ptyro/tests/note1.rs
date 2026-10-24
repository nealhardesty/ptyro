//! NOTE1 end-to-end tests (ROADMAP NOTE1): the scratchpad through a real daemon — the file and
//! `$PTYRO_SCRATCHPAD`, the CLI, merging a stale write, hearing about a change made outside,
//! the editor pane, and ticking an item when its worktree is cleaned up.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{ClientMsg, DomainEvent, PadMark, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(45);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-note1-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        // An editor that shows where it is and stays.
        let ed = h.join("fake-editor");
        std::fs::write(&ed, "#!/bin/sh\necho \"EDITING $*\"\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&ed, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let config = format!(
            "[worktrees]\nfetch = false\n[scratchpad]\neditor = \"{}\"\n{extra}",
            ed.display()
        );
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    /// `ptyro scratchpad …`, its output.
    fn pad(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.d.cmd().arg("scratchpad").args(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn file(&self) -> PathBuf {
        let (_, out, _) = self.pad(&["path"]);
        PathBuf::from(out.trim())
    }
}

async fn get(c: &Client) -> ptyro_protocol::PadDoc {
    match c.request(Request::ScratchGet).await.unwrap() {
        Reply::Scratch { doc } => doc,
        other => panic!("{other:?}"),
    }
}

/// The next `ScratchChanged` the client hears, within a while.
async fn changed(
    pushes: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    want: impl Fn(&str, u32) -> bool,
) -> (String, u32, Option<String>) {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, pushes.recv()).await {
            Ok(Some(ServerMsg::Event { event: DomainEvent::ScratchChanged { rev, open, by } }))
                if want(&rev, open) =>
            {
                return (rev, open, by);
            }
            Ok(Some(_)) => {}
            _ => panic!("no ScratchChanged event"),
        }
    }
}

/// NOTE1.0: the file is there from the start, sessions find it in `$PTYRO_SCRATCHPAD`, and
/// the CLI adds stamped lines to it, lists and ticks them, and brings an old copy back.
#[tokio::test]
async fn the_file_the_variable_and_the_cli() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let file = w.file();
    assert!(file.starts_with(w.d.paths.data.as_path()), "{file:?}");
    let deadline = Instant::now() + WAIT;
    while !file.exists() {
        assert!(Instant::now() < deadline, "the daemon makes the file at start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(std::fs::read_to_string(&file).unwrap().starts_with("# Scratchpad"));

    // A shell in a session sees it, and writes to it with the CLI's own variable.
    let spec = SpawnSpec {
        cwd: w.home(),
        name: Some("sh".into()),
        argv: vec!["sh".into(), "-c".into(), "echo pad=$PTYRO_SCRATCHPAD; sleep 60".into()],
        size: Some(TermSize::new(200, 20)),
        ..SpawnSpec::default()
    };
    let Reply::Session { .. } = c.request(Request::Spawn { spec }).await.unwrap() else { panic!() };
    let screen =
        w.d.wait_for_text(&c, "sh", &format!("pad={}", file.display()), WAIT).await.unwrap();
    assert!(screen.contains("pad=/"), "{screen}");

    // add: words, a section, a pipe, a note.
    let (ok, _, err) = w.pad(&["add", "login", "times out", "on safari"]);
    assert!(ok, "{err}");
    let (ok, ..) = w.pad(&["add", "--section", "Bugs", "second one"]);
    assert!(ok);
    let (ok, ..) = w.pad(&["add", "--note", "--no-stamp", "just a thought"]);
    assert!(ok);
    let mut piped = w.d.cmd();
    piped.args(["scratchpad", "add", "-"]).stdin(std::process::Stdio::piped());
    let mut child = piped.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"from a pipe\nsecond line\n").unwrap();
    }
    assert!(child.wait().unwrap().success());
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("login times out on safari"), "{text}");
    assert!(text.contains("## Bugs\n\n- [ ] "), "{text}");
    assert!(text.contains("- just a thought\n"), "{text}");
    assert!(text.contains("from a pipe\n  second line\n"), "{text}");

    // todo, --json, done by words and by line, undo.
    let (_, out, _) = w.pad(&["todo"]);
    assert!(out.contains("login times out on safari") && out.contains("second one"), "{out}");
    assert!(!out.contains("just a thought"), "a note isn't an item: {out}");
    let (_, out, _) = w.pad(&["todo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 3, "{out}");
    let (ok, ..) = w.pad(&["done", "safari"]);
    assert!(ok);
    let (_, out, _) = w.pad(&["todo"]);
    assert!(!out.contains("safari"), "{out}");
    let (ok, _, err) = w.pad(&["done", "second"]);
    assert!(ok, "{err}");
    let (_, out, _) = w.pad(&["todo", "--all"]);
    assert!(out.contains("✓ second one") && out.contains("✓ login times out"), "{out}");
    let line = out
        .lines()
        .find(|l| l.contains("second one"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let (ok, ..) = w.pad(&["done", "--undo", &line]);
    assert!(ok);
    let (ok, _, err) = w.pad(&["done", "no such thing"]);
    assert!(!ok && err.contains("no open item"), "{err}");

    // The history has the earlier copies; one comes back.
    let (_, out, _) = w.pad(&["log", "--json"]);
    let snaps: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert!(!snaps.is_empty());
    let header_only = snaps.iter().rev().find(|s| s["lines"].as_u64().unwrap() < 10).unwrap();
    let rev = header_only["rev"].as_str().unwrap().to_owned();
    let (ok, _, err) = w.pad(&["restore", &rev]);
    assert!(ok, "{err}");
    assert!(get(&c).await.text.len() < text.len());
    let (ok, _, err) = w.pad(&["restore", "nosuchrev"]);
    assert!(!ok && err.contains("no saved copy"), "{err}");
    // cat works, plain, with --tail.
    let (_, out, _) = w.pad(&["cat", "--tail", "2"]);
    assert!(out.lines().count() <= 2, "{out}");
}

/// NOTE1.4: every write is announced; a stale write merges with what changed meanwhile; a
/// change made outside (an editor's save) reaches clients too.
#[tokio::test]
async fn writes_merge_and_changes_from_outside_are_announced() {
    let w = World::new("");
    let mut c = w.d.client().await.unwrap();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();

    let base = get(&c).await;
    // The web edits from `base`, the CLI appends meanwhile.
    let (ok, ..) = w.pad(&["add", "--no-stamp", "from the cli"]);
    assert!(ok);
    let edited = base.text.replacen("# Scratchpad", "# My pad", 1);
    let r = c.request(Request::ScratchPut { base: base.rev.clone(), text: edited }).await.unwrap();
    let Reply::ScratchPut { doc, merged, conflicts } = r else { panic!("{r:?}") };
    assert!(merged && conflicts == 0, "{conflicts}: {}", doc.text);
    assert!(
        doc.text.starts_with("# My pad") && doc.text.contains("- [ ] from the cli"),
        "{}",
        doc.text
    );

    // The same line changed two ways: both kept.
    let r = c
        .request(Request::ScratchPut {
            base: base.rev.clone(),
            text: base.text.replacen("# Scratchpad", "# Other", 1),
        })
        .await
        .unwrap();
    let Reply::ScratchPut { doc, conflicts, .. } = r else { panic!("{r:?}") };
    assert_eq!(conflicts, 1);
    assert!(
        doc.text.contains("<<<<<<< on disk\n# My pad")
            && doc.text.contains("# Other\n>>>>>>> your edit"),
        "{}",
        doc.text
    );

    // An edit from outside, the way an editor saves: heard within a second or two.
    let cur = get(&c).await;
    let file = w.file();
    std::fs::write(&file, format!("{}- [ ] typed in vim\n", cur.text)).unwrap();
    let (rev, open, by) = changed(&mut pushes, |_, o| o == cur.open + 1).await;
    assert_ne!(rev, cur.rev);
    assert_eq!(by.as_deref(), Some("file"));
    assert_eq!(open, cur.open + 1);
}

/// NOTE1.1: the editor is one session, started on first use and reused, with the file on its
/// command line; it isn't a quick terminal or a sidebar session.
#[tokio::test]
async fn the_editor_is_one_session_in_a_pane() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let ask = |create| Request::ScratchPane {
        create,
        env: Default::default(),
        size: Some(TermSize::new(100, 20)),
        workspace: None,
    };
    let err = c.request(ask(false)).await.unwrap_err();
    assert!(err.to_string().contains("isn't open yet"), "{err}");
    let Reply::Session { info } = c.request(ask(true)).await.unwrap() else { panic!() };
    assert!(info.pad && !info.quick, "{info:?}");
    let file = w.file();
    let screen = w.d.wait_for_text(&c, &info.name, "EDITING", WAIT).await.unwrap();
    assert!(screen.contains(&file.display().to_string()), "{screen}");
    let Reply::Session { info: again } = c.request(ask(true)).await.unwrap() else { panic!() };
    assert_eq!(again.id, info.id, "one editor");
    // When the editor ends the session is forgotten and the next press starts a new one.
    c.request(Request::Kill { addr: info.name.clone(), signal: None }).await.unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Session { info: next } = c.request(ask(true)).await.unwrap() else { panic!() };
        if next.id != info.id {
            break;
        }
        assert!(Instant::now() < deadline, "a new editor starts after the old one is killed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// NOTE1.3: a worktree made from an item tags it, and removing the worktree ticks it.
#[tokio::test]
async fn an_item_started_as_a_task_is_ticked_when_its_worktree_is_cleaned_up() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let dir = w.home().join("myrepo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&w.home(), &dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&w.home(), &dir, &["add", "."]);
    git(&w.home(), &dir, &["commit", "-qm", "first"]);

    let (ok, ..) = w.pad(&["add", "--no-stamp", "fix the flaky test"]);
    assert!(ok);
    let items = match c.request(Request::ScratchTodo { all: false }).await.unwrap() {
        Reply::ScratchItems { items, .. } => items,
        other => panic!("{other:?}"),
    };
    let line = items[0].line;
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("flaky".into()),
        fetch: Some(false),
        pad_item: Some(line),
        ..Default::default()
    };
    let Reply::WorktreeCreated { info, .. } =
        c.request(Request::WorktreeNew { spec }).await.unwrap()
    else {
        panic!()
    };
    let text = get(&c).await.text;
    assert!(text.contains("- [ ] fix the flaky test [wt:myrepo/flaky]"), "{text}");
    let (_, out, _) = w.pad(&["todo"]);
    assert!(out.contains("→ myrepo/flaky"), "{out}");

    c.request(Request::WorktreeRemove { addr: info.id.clone(), opts: Default::default() })
        .await
        .unwrap();
    let text = get(&c).await.text;
    assert!(text.contains("- [x] fix the flaky test [wt:myrepo/flaky]"), "{text}");

    // And by hand: mark started / done / open over the protocol.
    let (ok, ..) = w.pad(&["add", "--no-stamp", "second task"]);
    assert!(ok);
    let r = c
        .request(Request::ScratchMark {
            sel: "second".into(),
            mark: PadMark::Done { pr: Some(42) },
        })
        .await
        .unwrap();
    let Reply::ScratchItems { items, .. } = r else { panic!() };
    assert_eq!((items[0].done, items[0].pr), (true, Some(42)));
}

/// `tick_on_finish = false` leaves the item for you.
#[tokio::test]
async fn ticking_on_finish_can_be_turned_off() {
    let w = World::new("tick_on_finish = false\n");
    let c = w.d.client().await.unwrap();
    let dir = w.home().join("myrepo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&w.home(), &dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&w.home(), &dir, &["add", "."]);
    git(&w.home(), &dir, &["commit", "-qm", "first"]);
    let (ok, ..) = w.pad(&["add", "--no-stamp", "keep me open"]);
    assert!(ok);
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("keep".into()),
        fetch: Some(false),
        pad_item: Some(match c.request(Request::ScratchTodo { all: false }).await.unwrap() {
            Reply::ScratchItems { items, .. } => items[0].line,
            other => panic!("{other:?}"),
        }),
        ..Default::default()
    };
    let Reply::WorktreeCreated { info, .. } =
        c.request(Request::WorktreeNew { spec }).await.unwrap()
    else {
        panic!()
    };
    c.request(Request::WorktreeRemove { addr: info.id, opts: Default::default() }).await.unwrap();
    assert!(get(&c).await.text.contains("- [ ] keep me open [wt:myrepo/keep]"));
}
