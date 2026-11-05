//! ROADMAP SH (SPEC §23): a workspace's SHELLS against a real daemon: numbers that stay, `%N`,
//! never placed under a worktree, sessions that lose their place joining, removal safety, set
//! loose and adopt, long commands and watches, scratch shells, and what a restart offers back.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec, Watch};
use ptyro_core::status::AttentionKind;
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(45);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-sh-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        // Quiet startup files: the shell's prompt is ptyro's integration and nothing else.
        std::fs::write(h.join(".bashrc"), "PS1='$ '\n").unwrap();
        std::fs::write(h.join(".bash_profile"), "PS1='$ '\n").unwrap();
        let config = format!("[worktrees]\nfetch = false\n[shell]\ncommand = [\"bash\"]\n{extra}");
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/bash")];
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.home().join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn repo(w: &World, name: &str) -> PathBuf {
    let dir = w.dir(name);
    git(&w.home(), &dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&w.home(), &dir, &["add", "."]);
    git(&w.home(), &dir, &["commit", "-qm", "first"]);
    dir
}

async fn try_spawn(c: &Client, spec: SpawnSpec) -> Result<SessionInfo, ptyro_core::Error> {
    match c.request(Request::Spawn { spec }).await? {
        Reply::Session { info } => Ok(info),
        other => panic!("{other:?}"),
    }
}

/// A loose shell (SPEC §23) in `cwd`, asking for `slot` when given.
async fn shell(c: &Client, cwd: &Path, slot: Option<u32>) -> SessionInfo {
    let spec = SpawnSpec {
        cwd: cwd.to_owned(),
        loose: true,
        slot,
        size: Some(TermSize::new(100, 30)),
        ..SpawnSpec::default()
    };
    try_spawn(c, spec).await.expect("spawn a loose shell")
}

async fn get(c: &Client, addr: &str) -> Option<SessionInfo> {
    match c.request(Request::Get { addr: addr.into() }).await {
        Ok(Reply::Session { info }) => Some(info),
        _ => None,
    }
}

async fn list(c: &Client) -> Vec<SessionInfo> {
    match c.request(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

async fn wait_for(
    c: &Client,
    addr: &str,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let s = get(c, addr).await;
        if let Some(s) = s.as_ref().filter(|s| pred(s)) {
            return s.clone();
        }
        assert!(Instant::now() < deadline, "{addr}: never {what}; last: {s:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn send(c: &Client, addr: &str, text: &str) {
    c.request(Request::SendText {
        addr: addr.into(),
        text: text.into(),
        enter: true,
        wait_ready_ms: None,
    })
    .await
    .unwrap();
}

/// Kill a shell and wait until it is gone (a shell that exits is removed by itself).
async fn kill_and_wait(c: &Client, s: &SessionInfo) {
    c.request(Request::Kill { addr: s.id.full(), signal: None }).await.unwrap();
    let deadline = Instant::now() + WAIT;
    while get(c, &s.id.full()).await.is_some() {
        assert!(Instant::now() < deadline, "{} never went", s.name);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn ready(c: &Client, s: &SessionInfo) {
    // The integration's first prompt: the shell reads what is typed from here.
    wait_for(c, &s.id.full(), "at its prompt", |s| s.live_cwd.is_some()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn numbers_stay_put_and_address_their_shell() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let home = w.home();
    let one = shell(&c, &home, None).await;
    let two = shell(&c, &home, None).await;
    let three = shell(&c, &home, None).await;
    assert_eq!([one.slot, two.slot, three.slot], [Some(1), Some(2), Some(3)]);
    assert!(one.loose && one.worktree.is_none() && one.project.is_none());
    assert_eq!(one.workspace.as_deref(), Some("main"));

    // `%N` and `ws%N` name them; an empty number says how to start one.
    assert_eq!(get(&c, "%3").await.unwrap().id, three.id);
    assert_eq!(get(&c, "main%1").await.unwrap().id, one.id);
    let e = c.request(Request::Get { addr: "%9".into() }).await.unwrap_err();
    assert!(e.message.contains("no shell 9 in main"), "{}", e.message);
    assert!(e.message.contains("--slot 9"), "the error offers the fix: {}", e.message);

    // Removing 2 leaves 1 and 3 where they are; the next one takes 2.
    kill_and_wait(&c, &two).await;
    assert_eq!(get(&c, "%3").await.unwrap().id, three.id);
    let again = shell(&c, &home, None).await;
    assert_eq!(again.slot, Some(2));

    // Asking for a number that is taken is refused, naming the shell there.
    let spec = SpawnSpec { cwd: home.clone(), loose: true, slot: Some(3), ..SpawnSpec::default() };
    let e = try_spawn(&c, spec).await.unwrap_err();
    assert!(e.message.contains("shell 3"), "{}", e.message);
    // A free one is given.
    assert_eq!(shell(&c, &home, Some(7)).await.slot, Some(7));

    // Moving onto a taken number swaps the two.
    let r = c.request(Request::ShellMove { addr: "%3".into(), slot: 1 }).await.unwrap();
    let Reply::Session { info } = r else { panic!("{r:?}") };
    assert_eq!(info.slot, Some(1));
    assert_eq!(get(&c, &one.id.full()).await.unwrap().slot, Some(3));
    assert_eq!(get(&c, "%1").await.unwrap().id, three.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_loose_shell_is_never_placed_and_sessions_outside_projects_join() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let code = repo(&w, "code");
    c.request(Request::ProjectAdd { path: code.clone(), workspace: None }).await.unwrap();

    // Started inside a project's checkout, a loose shell stays in SHELLS.
    let s = shell(&c, &code, None).await;
    assert!(s.loose && s.worktree.is_none(), "{s:#?}");

    // A plain session outside every project joins SHELLS with a number.
    let spec = SpawnSpec { cwd: w.dir("elsewhere"), ..SpawnSpec::default() };
    let plain = try_spawn(&c, spec).await.unwrap();
    assert!(plain.loose, "{plain:#?}");
    assert_eq!(plain.slot, Some(2));
    // One started in the project is placed there, as before.
    let spec = SpawnSpec { cwd: code.clone(), ..SpawnSpec::default() };
    let placed = try_spawn(&c, spec).await.unwrap();
    assert!(!placed.loose && placed.worktree.is_some(), "{placed:#?}");

    // A loose shell that `cd`s into a repository nobody added: the repository joins the
    // sidebar, the shell stays where it is.
    let other = repo(&w, "other");
    ready(&c, &s).await;
    send(&c, &s.id.full(), &format!("cd {}", other.display())).await;
    let deadline = Instant::now() + WAIT;
    loop {
        let r = c.request(Request::ProjectList).await.unwrap();
        let Reply::Projects { projects } = r else { panic!() };
        if projects.iter().any(|p| p.root == other) {
            break;
        }
        assert!(Instant::now() < deadline, "the repository was never registered");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let s = get(&c, &s.id.full()).await.unwrap();
    assert!(s.loose && s.worktree.is_none() && s.slot == Some(1), "{s:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_that_lose_their_place_join_and_removal_stays_safe() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let code = repo(&w, "code");
    c.request(Request::ProjectAdd { path: code.clone(), workspace: None }).await.unwrap();
    let spec = SpawnSpec { cwd: code.clone(), ..SpawnSpec::default() };
    let s = try_spawn(&c, spec).await.unwrap();
    assert!(s.worktree.is_some());
    // Let the project go: its running session keeps running, in SHELLS.
    c.request(Request::ProjectRemove { addr: "code".into(), release: true }).await.unwrap();
    let s = wait_for(&c, &s.id.full(), "in SHELLS", |s| s.loose).await;
    assert_eq!(s.slot, Some(1));
    assert!(s.worktree.is_none() && s.project.is_none());

    // A worktree with a loose shell standing in it isn't removed from under it.
    let code2 = repo(&w, "code2");
    c.request(Request::ProjectAdd { path: code2.clone(), workspace: None }).await.unwrap();
    let r = c
        .request(Request::WorktreeNew {
            spec: ptyro_core::model::WorktreeSpec {
                project: "code2".into(),
                name: Some("wt".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let Reply::WorktreeCreated { info: wt, .. } = r else { panic!("{r:?}") };
    let inside = shell(&c, &wt.path, None).await;
    assert!(inside.worktree.is_none());
    let e = c
        .request(Request::WorktreeRemove { addr: wt.id.clone(), opts: Default::default() })
        .await
        .unwrap_err();
    assert!(e.message.contains(&inside.name), "{}", e.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn set_loose_and_adopt() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let code = repo(&w, "code");
    c.request(Request::ProjectAdd { path: code.clone(), workspace: None }).await.unwrap();
    let spec = SpawnSpec { cwd: code.clone(), ..SpawnSpec::default() };
    let s = try_spawn(&c, spec).await.unwrap();
    let wt = s.worktree.clone().unwrap();

    let r = c
        .request(Request::ShellPlace { addr: s.id.full(), loose: true, worktree: None })
        .await
        .unwrap();
    let Reply::Session { info } = r else { panic!("{r:?}") };
    assert!(info.loose && info.slot == Some(1) && info.worktree.is_none(), "{info:#?}");

    // Back into the worktree its directory is in, giving up its number.
    let r = c
        .request(Request::ShellPlace { addr: "%1".into(), loose: false, worktree: None })
        .await
        .unwrap();
    let Reply::Session { info } = r else { panic!("{r:?}") };
    assert!(!info.loose && info.slot.is_none());
    assert_eq!(info.worktree.as_deref(), Some(wt.as_str()));
    // Not loose: nothing to adopt.
    let e = c
        .request(Request::ShellPlace { addr: s.id.full(), loose: false, worktree: None })
        .await
        .unwrap_err();
    assert!(e.message.contains("isn't in SHELLS"), "{}", e.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn long_commands_and_watches_say_so() {
    let w = World::new("notify_after = \"1s\"\n");
    let c = w.d.client().await.unwrap();
    let s = shell(&c, &w.home(), None).await;
    let id = s.id.full();
    ready(&c, &s).await;

    // A short command says nothing; a long one that fails says so with its exit code.
    send(&c, &id, "true").await;
    wait_for(&c, &id, "ran true", |s| s.last_command_exit == Some(0)).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(get(&c, &id).await.unwrap().attention.is_none(), "a short command is no news");
    send(&c, &id, "sleep 2; (exit 3)").await;
    let s = wait_for(&c, &id, "told about the long command", |s| s.attention.is_some()).await;
    let a = s.attention.unwrap();
    assert_eq!(a.kind, AttentionKind::Errored, "{a:?}");
    assert!(a.text.starts_with("✗3 "), "{}", a.text);
    c.request(Request::ClearAttention { addr: id.clone() }).await.unwrap();

    // A match watch fires once, when its pattern shows up, and clears.
    let watch = Some(Watch::Match { re: "HELLO-4[0-9]".into() });
    c.request(Request::ShellWatch { addr: id.clone(), watch }).await.unwrap();
    assert!(get(&c, &id).await.unwrap().watch.is_some());
    send(&c, &id, "echo HELLO-$((40+2))").await;
    let s = wait_for(&c, &id, "matched", |s| s.attention.is_some()).await;
    assert!(s.attention.unwrap().text.contains("HELLO-42"));
    assert!(s.watch.is_none(), "a match watch fires once");

    // A bad pattern is refused when it is set, not later.
    let watch = Some(Watch::Match { re: "(".into() });
    assert!(c.request(Request::ShellWatch { addr: id.clone(), watch }).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scratch_shell_takes_its_directory_with_it() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let spec = SpawnSpec { cwd: w.home(), scratch: true, ..SpawnSpec::default() };
    let s = try_spawn(&c, spec).await.unwrap();
    assert!(s.loose && s.scratch, "{s:#?}");
    assert!(s.cwd.components().any(|p| p.as_os_str() == "scratch"), "{}", s.cwd.display());
    assert!(s.cwd.is_dir());
    let dir = s.cwd.clone();
    kill_and_wait(&c, &s).await;
    assert!(!dir.exists(), "an empty scratch directory goes with its shell");

    // One with something left in it stays.
    let spec = SpawnSpec { cwd: w.home(), scratch: true, ..SpawnSpec::default() };
    let s = try_spawn(&c, spec).await.unwrap();
    std::fs::write(s.cwd.join("notes.txt"), "keep me\n").unwrap();
    let dir = s.cwd.clone();
    kill_and_wait(&c, &s).await;
    assert!(dir.join("notes.txt").is_file(), "never delete what someone made");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_offers_shells_back_in_their_numbers() {
    let mut w = World::new("");
    let c = w.d.client().await.unwrap();
    let work = w.dir("work");
    let s = shell(&c, &w.home(), Some(4)).await;
    ready(&c, &s).await;
    send(&c, &s.id.full(), &format!("cd {}", work.display())).await;
    wait_for(&c, &s.id.full(), "in work", |s| s.live_cwd.as_deref() == Some(&work)).await;
    // The tick keeps the list once a second.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let back = list(&c).await.into_iter().find(|x| x.id == s.id).expect("offered back");
    assert!(back.lost && back.loose && back.slot == Some(4), "{back:#?}");
    assert!(matches!(back.state, ProcessState::Failed { .. }), "{:?}", back.state);
    assert_eq!(back.cwd, work, "where it was last");
    // ⏎ (a restart) starts it again there, in its number.
    c.request(Request::Restart { addr: "%4".into() }).await.unwrap();
    let again = wait_for(&c, "%4", "running again", |s| s.state.is_live()).await;
    assert!(!again.lost && again.slot == Some(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_forget_brings_nothing_back() {
    let mut w = World::new("restore = \"forget\"\n");
    let c = w.d.client().await.unwrap();
    let s = shell(&c, &w.home(), None).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    assert!(!list(&c).await.iter().any(|x| x.id == s.id));
}

/// Review fixes (2026-11-04): only a shell asked for in SHELLS falls back to home; any other
/// spawn in a missing directory is still refused, as it always was.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_directory_is_refused_unless_a_shell_asked_for_shells() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let gone = w.home().join("no-such-dir");
    let spec = SpawnSpec { cwd: gone.clone(), argv: vec!["true".into()], ..SpawnSpec::default() };
    let e = try_spawn(&c, spec).await.unwrap_err();
    assert!(e.message.contains("not a directory"), "{}", e.message);
    let s = shell(&c, &gone, None).await;
    assert_eq!(s.cwd, w.home(), "a SHELLS shell starts at home instead");
}

/// A `%N` typed inside a session means that session's workspace, whatever the TUI shows; and
/// a workspace's removal moves its shells to main without renumbering main's own.
#[tokio::test(flavor = "multi_thread")]
async fn percent_n_is_the_caller_s_workspace_and_a_removed_workspace_s_shells_fit_in() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    c.request(Request::WorkspaceNew { name: "work".into(), profile: None, notify: None })
        .await
        .unwrap();
    let home = w.home();
    let spawn_in = |ws: &str, slot: u32| SpawnSpec {
        cwd: home.clone(),
        loose: true,
        slot: Some(slot),
        workspace: Some(ws.into()),
        ..SpawnSpec::default()
    };
    let main1 = try_spawn(&c, spawn_in("main", 1)).await.unwrap();
    let main2 = try_spawn(&c, spawn_in("main", 2)).await.unwrap();
    let work1 = try_spawn(&c, spawn_in("work", 1)).await.unwrap();
    let work2 = try_spawn(&c, spawn_in("work", 2)).await.unwrap();
    // From inside work's shell 2, `%1` is work's shell 1, not main's.
    ready(&c, &work2).await;
    ready(&c, &work1).await;
    let socket = w.d.env().into_iter().find(|(k, _)| k == "PTYRO_SOCKET").unwrap().1;
    send(
        &c,
        &work2.id.full(),
        &format!("PTYRO_SOCKET={socket} PTYRO_NO_AUTOSPAWN=1 {BIN} send %1 'echo marker-work'"),
    )
    .await;
    w.d.wait_for_text(&c, &work1.id.full(), "marker-work", WAIT)
        .await
        .expect("work's shell 1 got it");
    let main_screen = match c
        .request(Request::ReadScreen { addr: main1.id.full(), format: Default::default() })
        .await
        .unwrap()
    {
        Reply::Screen { text } => text,
        r => panic!("{r:?}"),
    };
    assert!(!main_screen.contains("marker-work"), "main's shell 1 didn't: {main_screen}");
    // Removing `work`: its shells go to main; main's 1 and 2 stay, the newcomers take 3 and 4.
    c.request(Request::WorkspaceRemove { name: "work".into() }).await.unwrap();
    let mut slots = Vec::new();
    for s in [&main1, &main2, &work1, &work2] {
        slots.push(get(&c, &s.id.full()).await.unwrap().slot);
    }
    assert_eq!(slots[..2], [Some(1), Some(2)], "main's own keep theirs");
    let mut moved = slots[2..].to_vec();
    moved.sort();
    assert_eq!(moved, [Some(3), Some(4)]);
}

/// A match watch fires for a new line, never for one already on screen when it was set.
#[tokio::test(flavor = "multi_thread")]
async fn a_match_watch_ignores_what_was_already_there() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let s = shell(&c, &w.home(), None).await;
    let id = s.id.full();
    ready(&c, &s).await;
    // Quoted, so only its output (not the line typed) says OLD-ERROR.
    send(&c, &id, "echo OLD-ERR''OR").await;
    w.d.wait_for_text(&c, &id, "\nOLD-ERROR", WAIT).await.unwrap();
    let watch = Some(Watch::Match { re: "ERROR".into() });
    c.request(Request::ShellWatch { addr: id.clone(), watch }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    send(&c, &id, "true").await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(get(&c, &id).await.unwrap().attention.is_none(), "old news is no hit");
    send(&c, &id, "echo NEW-ERR''OR").await;
    let s = wait_for(&c, &id, "matched the new line", |s| s.attention.is_some()).await;
    assert!(s.attention.unwrap().text.contains("NEW-ERROR"));
}

/// Shells offered back after a restart are still offered after another one, untouched.
#[tokio::test(flavor = "multi_thread")]
async fn lost_shells_survive_a_second_restart() {
    let mut w = World::new("");
    let c = w.d.client().await.unwrap();
    let s = shell(&c, &w.home(), Some(3)).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    drop(c);
    w.d.restart().unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let back = list(&c).await.into_iter().find(|x| x.id == s.id).expect("still offered");
    assert!(back.lost && back.slot == Some(3), "{back:#?}");
}

/// `--stop-sessions` stops a loose shell standing in the worktree too, before it goes.
#[tokio::test(flavor = "multi_thread")]
async fn stop_sessions_stops_a_loose_shell_in_the_worktree() {
    let w = World::new("");
    let c = w.d.client().await.unwrap();
    let code = repo(&w, "code");
    c.request(Request::ProjectAdd { path: code.clone(), workspace: None }).await.unwrap();
    let r = c
        .request(Request::WorktreeNew {
            spec: ptyro_core::model::WorktreeSpec {
                project: "code".into(),
                name: Some("wt".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let Reply::WorktreeCreated { info: wt, .. } = r else { panic!("{r:?}") };
    let inside = shell(&c, &wt.path, None).await;
    let opts = ptyro_core::model::RemoveOpts { stop_sessions: true, ..Default::default() };
    c.request(Request::WorktreeRemove { addr: wt.id.clone(), opts }).await.expect("removed");
    let deadline = Instant::now() + WAIT;
    while get(&c, &inside.id.full()).await.is_some_and(|s| s.state.is_live()) {
        assert!(Instant::now() < deadline, "the loose shell kept running in a deleted directory");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
