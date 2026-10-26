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
            "[worktrees]\nfetch = false\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n[scratchpad]\neditor = \"{}\"\n{extra}",
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

    /// The same, run from `dir`.
    fn pad_in(&self, dir: &Path, args: &[&str]) -> (bool, String, String) {
        let out = self.d.cmd().current_dir(dir).arg("scratchpad").args(args).output().unwrap();
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
    match c.request(Request::ScratchGet { pad: Default::default() }).await.unwrap() {
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
            Ok(Some(ServerMsg::Event {
                event: DomainEvent::ScratchChanged { rev, open, by, .. },
            })) if want(&rev, open) => {
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
    let r = c
        .request(Request::ScratchPut {
            pad: Default::default(),
            base: base.rev.clone(),
            text: edited,
        })
        .await
        .unwrap();
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
            pad: Default::default(),
            base: base.rev.clone(),
            text: base.text.replacen("# Scratchpad", "# Other", 1),
        })
        .await
        .unwrap();
    let Reply::ScratchPut { doc, conflicts, .. } = r else { panic!("{r:?}") };
    assert_eq!(conflicts, 1);
    assert!(
        doc.text.contains("<<<<<<< on disk\n# My pad")
            && doc.text.contains("# Other — everywhere\n>>>>>>> your edit"),
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
        pad: Default::default(),
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
    let items = match c
        .request(Request::ScratchTodo { pad: Default::default(), all: false })
        .await
        .unwrap()
    {
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
            pad: Default::default(),
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
        pad_item: Some(
            match c
                .request(Request::ScratchTodo { pad: Default::default(), all: false })
                .await
                .unwrap()
            {
                Reply::ScratchItems { items, .. } => items[0].line,
                other => panic!("{other:?}"),
            },
        ),
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

/// NOTE1.3: what an agent adds is stamped with its name and where it works, so you can tell it
/// from your own notes.
#[tokio::test]
async fn an_agent_s_note_says_who_wrote_it() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let spec = SpawnSpec {
        agent: Some("claude".into()),
        cwd: w.home(),
        name: Some("scribe".into()),
        size: Some(TermSize::new(100, 20)),
        prompt: Some("work 5 busy".into()),
        ..SpawnSpec::default()
    };
    let Reply::Session { .. } = c.request(Request::Spawn { spec }).await.unwrap() else { panic!() };
    let r = c
        .request(Request::ScratchAdd {
            pad: Default::default(),
            text: "the retry loop never backs off".into(),
            section: Some("Found by agents".into()),
            note: false,
            stamp: true,
            session: Some("scribe".into()),
            cwd: None,
        })
        .await
        .unwrap();
    let Reply::Scratch { doc } = r else { panic!("{r:?}") };
    assert!(doc.text.contains("## Found by agents\n\n- [ ] "), "{}", doc.text);
    let Reply::ScratchItems { items, .. } =
        c.request(Request::ScratchTodo { pad: Default::default(), all: false }).await.unwrap()
    else {
        panic!()
    };
    let it = items.iter().find(|i| i.text.contains("retry loop")).expect("item");
    assert_eq!(it.by.as_deref(), Some("scribe"));
    assert!(it.whence.is_some() && it.at.is_some(), "{it:?}");
    assert_eq!(it.section.as_deref(), Some("Found by agents"));
}

/// A repository with one commit, as a project the daemon knows, and a worktree of it.
async fn project_with_worktree(
    w: &World,
    c: &Client,
) -> (PathBuf, ptyro_core::model::WorktreeInfo) {
    let dir = w.home().join("myrepo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&w.home(), &dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&w.home(), &dir, &["add", "."]);
    git(&w.home(), &dir, &["commit", "-qm", "first"]);
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("feature".into()),
        fetch: Some(false),
        ..Default::default()
    };
    let Reply::WorktreeCreated { info, .. } =
        c.request(Request::WorktreeNew { spec }).await.unwrap()
    else {
        panic!()
    };
    (dir, info)
}

/// NOTE1.7: inside a project's worktree the pad is the project's (`<repo>/.ptyro/scratchpad.md`
/// in the main checkout), kept out of git without touching `.gitignore`; elsewhere, and with
/// `--global`, it is the global one; and every command says which it used.
#[tokio::test]
async fn a_project_has_a_pad_of_its_own_that_follows_where_you_are() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let (repo, wt) = project_with_worktree(&w, &c).await;
    // A `.ptyro/` that already holds committed things (recipes) is left as it is.
    std::fs::create_dir_all(repo.join(".ptyro/recipes")).unwrap();
    std::fs::write(repo.join(".ptyro/recipes/r.toml"), "x = 1\n").unwrap();
    let global = w.file();
    let project_file = repo.join(".ptyro/scratchpad.md");

    // From the worktree (not the main checkout) the path is the main checkout's pad.
    let (ok, out, err) = w.pad_in(&wt.path, &["path"]);
    assert!(ok, "{err}");
    assert_eq!(
        PathBuf::from(out.trim()),
        project_file.canonicalize().unwrap_or(project_file.clone())
    );
    let text = std::fs::read_to_string(&project_file).unwrap();
    assert!(text.starts_with("# Scratchpad — myrepo (this project)"), "{text}");
    // From the main checkout too; from outside, the global one; and the flags force one.
    let (_, out, _) = w.pad_in(&repo, &["path"]);
    assert!(out.trim().ends_with(".ptyro/scratchpad.md"), "{out}");
    let (_, out, _) = w.pad_in(&w.home(), &["path"]);
    assert_eq!(PathBuf::from(out.trim()), global);
    let (_, out, _) = w.pad_in(&wt.path, &["--global", "path"]);
    assert_eq!(PathBuf::from(out.trim()), global);
    let (_, out, _) = w.pad_in(&wt.path, &["path", "--global"]);
    assert_eq!(PathBuf::from(out.trim()), global, "flags work after the subcommand too");
    let (_, out, _) = w.pad_in(&w.home(), &["--project=myrepo", "path"]);
    assert!(out.trim().ends_with("myrepo/.ptyro/scratchpad.md"), "{out}");
    let (ok, _, err) = w.pad_in(&w.home(), &["--project", "path"]);
    assert!(!ok && err.contains("not inside a project"), "{err}");
    let (ok, ..) = w.pad_in(&wt.path, &["--global", "--project", "path"]);
    assert!(!ok, "the two flags exclude each other");

    // add: from the worktree it lands in the project's pad and says so; `--global` in the other.
    let (ok, _, err) = w.pad_in(&wt.path, &["add", "project thing"]);
    assert!(ok, "{err}");
    assert!(err.contains("the myrepo project pad"), "{err}");
    let (ok, _, err) = w.pad_in(&wt.path, &["add", "--global", "global thing"]);
    assert!(ok, "{err}");
    assert!(err.contains("the global pad"), "{err}");
    let (ok, ..) = w.pad_in(&wt.path, &["add", "--project", "swallowed?"]);
    assert!(ok, "--project without a value doesn't take the words as its value");
    let project_text = std::fs::read_to_string(&project_file).unwrap();
    assert!(project_text.contains("project thing") && project_text.contains("swallowed?"));
    assert!(!project_text.contains("global thing"));
    let global_text = std::fs::read_to_string(&global).unwrap();
    assert!(global_text.contains("global thing") && !global_text.contains("project thing"));
    // The stamp says which worktree it was added from.
    assert!(project_text.contains("`myrepo/feature`"), "{project_text}");
    let (_, out, _) = w.pad_in(&wt.path, &["todo"]);
    assert!(out.contains("project thing") && !out.contains("global thing"), "{out}");
    let (_, out, _) = w.pad_in(&wt.path, &["--global", "todo"]);
    assert!(out.contains("global thing") && !out.contains("project thing"), "{out}");
    let (ok, ..) = w.pad_in(&wt.path, &["done", "project"]);
    assert!(ok);
    // log and restore are per pad: the project's copies live beside its pad.
    let (_, out, _) = w.pad_in(&wt.path, &["log", "--json"]);
    let snaps: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert!(!snaps.is_empty());
    assert!(repo.join(".ptyro/scratchpad.history").is_dir());
    let oldest = snaps.last().unwrap()["rev"].as_str().unwrap().to_owned();
    let (ok, _, err) = w.pad_in(&wt.path, &["restore", &oldest]);
    assert!(ok && err.contains("myrepo project pad"), "{err}");
    assert!(!std::fs::read_to_string(&project_file).unwrap().contains("project thing"));
    assert!(std::fs::read_to_string(&global).unwrap().contains("global thing"), "global untouched");

    // Git never offers it: the repository's own exclude file has the line once, `.gitignore`
    // was not made, and the committed recipes are not hidden.
    let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches("/.ptyro/scratchpad.*").count(), 1, "{exclude}");
    assert!(!repo.join(".gitignore").exists());
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .env("HOME", w.home())
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(!status.contains("scratchpad"), "{status}");
    assert!(
        status.contains(".ptyro/recipes/r.toml"),
        "other files in .ptyro/ still show: {status}"
    );
}

/// NOTE1.7: sessions of a project have `$PTYRO_PROJECT_SCRATCHPAD`; others don't; the global
/// variable is the same everywhere. The protocol names a pad, defaults to the global one, and
/// announces a project pad's changes (made by a file save too) with the pad they are about.
#[tokio::test]
async fn the_variable_the_protocol_and_the_events_name_the_pad() {
    let w = World::new("");
    let mut c = w.d.client().await.unwrap();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents).unwrap();
    let (repo, wt) = project_with_worktree(&w, &c).await;
    let global = w.file();
    let project_file = repo.join(".ptyro/scratchpad.md");

    let sh = |name: &str, cwd: PathBuf| SpawnSpec {
        cwd,
        name: Some(name.into()),
        argv: vec![
            "sh".into(),
            "-c".into(),
            "echo g=$PTYRO_SCRATCHPAD p=[$PTYRO_PROJECT_SCRATCHPAD]; sleep 60".into(),
        ],
        size: Some(TermSize::new(240, 20)),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec: sh("inproject", wt.path.clone()) }).await.unwrap();
    c.request(Request::Spawn { spec: sh("outside", w.home()) }).await.unwrap();
    let screen = w.d.wait_for_text(&c, "inproject", "p=[/", WAIT).await.unwrap();
    assert!(screen.contains(&format!("g={}", global.display())), "{screen}");
    assert!(screen.contains(&format!("p=[{}]", project_file.display())), "{screen}");
    let screen = w.d.wait_for_text(&c, "outside", "p=[", WAIT).await.unwrap();
    assert!(
        screen.contains("p=[]") && screen.contains(&format!("g={}", global.display())),
        "{screen}"
    );

    // A request without a pad is the global one; naming the project (by slug, id or a path in
    // it) is the project's; `Near` follows a directory.
    let get = |pad| async {
        match c.request(Request::ScratchGet { pad }).await.unwrap() {
            Reply::Scratch { doc } => doc,
            other => panic!("{other:?}"),
        }
    };
    use ptyro_core::scratchpad::PadSel;
    let g = get(PadSel::Global).await;
    assert_eq!(
        (g.pad.clone(), g.name.as_str(), g.path.clone()),
        (PadSel::Global, "everywhere", global.clone())
    );
    let by_slug = get(PadSel::Project("myrepo".into())).await;
    let PadSel::Project(id) = by_slug.pad.clone() else { panic!("{:?}", by_slug.pad) };
    assert_eq!(by_slug.name, "myrepo");
    assert!(by_slug.path.ends_with(".ptyro/scratchpad.md"));
    let by_path = get(PadSel::Project(wt.path.to_string_lossy().into_owned())).await;
    assert_eq!(by_path.path, by_slug.path, "a worktree resolves to its project's pad");
    assert_eq!(get(PadSel::Near(wt.path.clone())).await.pad, PadSel::Project(id.clone()));
    assert_eq!(get(PadSel::Near(w.home())).await.pad, PadSel::Global, "outside a project: global");
    let missing = c.request(Request::ScratchGet { pad: PadSel::Project("nope".into()) }).await;
    assert!(missing.is_err());
    // The list of pads: the global one and the project's, with counts.
    let Reply::ScratchPads { pads } = c.request(Request::ScratchPads).await.unwrap() else {
        panic!()
    };
    assert_eq!(pads.len(), 2, "{pads:?}");

    // An add to the project's pad is announced as the project's, and the global pad's count
    // is not touched by it.
    let r = c
        .request(Request::ScratchAdd {
            pad: PadSel::Project(id.clone()),
            text: "from the protocol".into(),
            section: None,
            note: false,
            stamp: false,
            session: None,
            cwd: None,
        })
        .await
        .unwrap();
    let Reply::Scratch { doc } = r else { panic!() };
    assert_eq!((doc.open, doc.pad.clone()), (1, PadSel::Project(id.clone())));
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            pushes.recv(),
        )
        .await
        {
            Ok(Some(ServerMsg::Event { event: DomainEvent::ScratchChanged { pad, open, .. } }))
                if pad != PadSel::Global =>
            {
                assert_eq!((pad, open), (PadSel::Project(id.clone()), 1));
                break;
            }
            Ok(Some(_)) => {}
            _ => panic!("no ScratchChanged for the project pad"),
        }
    }
    assert_eq!(get(PadSel::Global).await.open, 0);
    // An editor saving the project's file is noticed too, with the pad it is about.
    let edited =
        format!("{}- [ ] typed in an editor\n", std::fs::read_to_string(&project_file).unwrap());
    std::fs::write(&project_file, edited).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            pushes.recv(),
        )
        .await
        {
            Ok(Some(ServerMsg::Event {
                event: DomainEvent::ScratchChanged { pad: PadSel::Project(p), open: 2, by, .. },
            })) => {
                assert_eq!((p, by.as_deref()), (id.clone(), Some("file")));
                break;
            }
            Ok(Some(_)) => {}
            _ => panic!("no ScratchChanged for the saved file"),
        }
    }
    // Over the wire, the old shape (no `pad`) still works and means the global pad.
    let old: Request = serde_json::from_str(r#"{"t":"scratch_get"}"#).unwrap();
    let Reply::Scratch { doc } = c.request(old).await.unwrap() else { panic!() };
    assert_eq!(doc.path, global);
}

/// NOTE1.7: an item started from the project's pad is tagged in that pad, and ticked there
/// when its worktree is cleaned up; the item in the global pad is left alone.
#[tokio::test]
async fn an_item_of_the_project_pad_is_ticked_when_its_worktree_is_cleaned_up() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let (repo, wt) = project_with_worktree(&w, &c).await;
    let (ok, ..) = w.pad_in(&wt.path, &["add", "--no-stamp", "fix the project thing"]);
    assert!(ok);
    let (ok, ..) = w.pad_in(&wt.path, &["add", "--global", "--no-stamp", "global item"]);
    assert!(ok);
    let (_, out, _) = w.pad_in(&wt.path, &["todo", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let line = v["items"][0]["line"].as_u64().unwrap() as u32;
    let spec = ptyro_core::model::WorktreeSpec {
        project: repo.to_string_lossy().into_owned(),
        name: Some("fixit".into()),
        fetch: Some(false),
        pad_item: Some(line),
        pad_sel: ptyro_core::scratchpad::PadSel::Project(wt.project.clone()),
        ..Default::default()
    };
    let Reply::WorktreeCreated { info, .. } =
        c.request(Request::WorktreeNew { spec }).await.unwrap()
    else {
        panic!()
    };
    let project_file = repo.join(".ptyro/scratchpad.md");
    let text = std::fs::read_to_string(&project_file).unwrap();
    assert!(text.contains("- [ ] fix the project thing [wt:myrepo/fixit]"), "{text}");
    assert!(!std::fs::read_to_string(w.file()).unwrap().contains("[wt:"));
    c.request(Request::WorktreeRemove { addr: info.id, opts: Default::default() }).await.unwrap();
    let text = std::fs::read_to_string(&project_file).unwrap();
    assert!(text.contains("- [x] fix the project thing [wt:myrepo/fixit]"), "{text}");
    assert!(std::fs::read_to_string(w.file()).unwrap().contains("- [ ] global item"));
}
