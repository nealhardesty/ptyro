//! UX1 end-to-end tests (ROADMAP UX1): what the out-of-the-box experience needs from a real
//! daemon, with the fake agent standing in for the agents.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_core::status::AgentStatus;
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
// Generous: auto-registration runs git a few times, which takes seconds on a loaded machine;
// a wait ends as soon as what it waits for is true.
const WAIT: Duration = Duration::from_secs(45);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    /// A daemon whose `claude` is the fake agent, and whose `$SHELL` is `shell` (a script).
    fn new(shell: &str) -> Self {
        Self::with_config(shell, "")
    }

    /// The same, with `extra` appended to its config (whole sections).
    fn with_config(shell: &str, extra: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-ux1-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let sh = h.join("fake-shell");
        std::fs::write(&sh, format!("#!/bin/sh\n{shell}\n")).unwrap();
        std::fs::set_permissions(&sh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[worktrees]\nfetch = false\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n{extra}"
        );
        let (hs, ss) = (h.to_string_lossy().into_owned(), sh.to_string_lossy().into_owned());
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", ss.as_str())];
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }
}

/// ptyro under another name, so its process is called that: a hard link (the kernel names a
/// process after the path it was started by), else a copy.
fn named(path: &std::path::Path) {
    if std::fs::hard_link(BIN, path).is_err() {
        std::fs::copy(BIN, path).unwrap();
    }
}

async fn spawn(c: &Client, spec: SpawnSpec) -> SessionInfo {
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn wait_for(
    c: &Client,
    name: &str,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let s = match c.request(Request::Get { addr: name.into() }).await.unwrap() {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        if pred(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "{name}: never {what}; last: {s:#?}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// UX1.2: a session says what it was asked, what it answered, and a shell what it ran.
#[tokio::test]
async fn sessions_report_their_task_reply_and_shell_command() {
    let shell = r"printf '\033]133;A\007$ \033]133;B\007\033]133;C;cmdline=make test; echo ok\007'
sleep 0.4
printf '\033]133;D;2\007\033]133;A\007$ '
exec sleep 30";
    let w = World::new(shell);
    let c = w.d.client().await.unwrap();

    let sh = SpawnSpec {
        cwd: std::env::temp_dir(),
        name: Some("sh".into()),
        size: Some(TermSize::new(80, 20)),
        ..SpawnSpec::default()
    };
    spawn(&c, sh).await;
    let s = wait_for(&c, "sh", "its command ended", |s| {
        s.command.as_ref().is_some_and(|c| c.ended_ms.is_some())
    })
    .await;
    let cmd = s.command.unwrap();
    assert_eq!(cmd.text.as_deref(), Some("make test; echo ok"));
    assert_eq!(cmd.exit, Some(2));
    assert!(cmd.ended_ms.unwrap() >= cmd.started_ms + 300, "{cmd:?}");

    let agent = SpawnSpec {
        agent: Some("claude".into()),
        cwd: std::env::temp_dir(),
        name: Some("cl".into()),
        size: Some(TermSize::new(100, 20)),
        prompt: Some("work 0.3 first turn".into()),
        ..SpawnSpec::default()
    };
    spawn(&c, agent).await;
    let s = wait_for(&c, "cl", "idle after its turn, with a reply", |s| {
        s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Idle) && s.last_reply.is_some()
    })
    .await;
    assert_eq!(s.task.as_deref(), Some("work 0.3 first turn"));
    // The program is named, never a version (UX1.1).
    assert!(s.foreground.as_deref().is_none_or(|f| !f.starts_with(char::is_numeric)), "{s:?}");
}

fn git(home: &std::path::Path, dir: &std::path::Path, args: &[&str]) {
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

/// A repository with one commit, under the world's home.
fn repo(w: &World, name: &str) -> PathBuf {
    let dir = w.home().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&w.home(), &dir, &["init", "-q"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&w.home(), &dir, &["add", "."]);
    git(&w.home(), &dir, &["commit", "-qm", "first"]);
    dir
}

async fn projects(c: &Client) -> Vec<ptyro_core::model::ProjectInfo> {
    match c.request(Request::ProjectList).await.unwrap() {
        Reply::Projects { projects } => projects,
        other => panic!("{other:?}"),
    }
}

async fn workspace(c: &Client, name: &str) -> ptyro_core::model::WorkspaceInfo {
    match c.request(Request::WorkspaceList).await.unwrap() {
        Reply::Workspaces { workspaces } => {
            workspaces.into_iter().find(|w| w.name == name).expect("workspace")
        }
        other => panic!("{other:?}"),
    }
}

/// UX1.5: the repository a session runs in is a project, in that session's workspace; undoing
/// it lets the session go and keeps it from coming back; adding by hand lifts that.
#[tokio::test]
async fn the_repository_you_are_in_registers_itself_and_undo_sticks() {
    let w = World::new("exec sleep 60");
    let mut c = w.d.client().await.unwrap();
    let mut events = c.pushes();
    c.send(ptyro_protocol::ClientMsg::WatchEvents).unwrap();
    let dir = repo(&w, "myrepo");
    c.request(Request::WorkspaceNew { name: "side".into(), profile: None, notify: None })
        .await
        .unwrap();
    let spec = |name: &str| SpawnSpec {
        cwd: dir.clone(),
        name: Some(name.into()),
        workspace: Some("side".into()),
        size: Some(TermSize::new(80, 20)),
        ..SpawnSpec::default()
    };
    let s = spawn(&c, spec("one")).await;
    let ps = projects(&c).await;
    let p = ps.iter().find(|p| p.slug == "myrepo").expect("registered at spawn");
    assert_eq!(s.project.as_deref(), Some(p.id.as_str()), "and the session is in it");
    assert!(s.worktree.is_some(), "under its main checkout");
    assert!(workspace(&c, "side").await.projects.contains(&p.id), "in the session's workspace");
    // Clients hear that it happened (the TUI offers an undo).
    let deadline = Instant::now() + WAIT;
    loop {
        let msg = tokio::time::timeout_at(deadline.into(), events.recv()).await.unwrap().unwrap();
        if let ptyro_protocol::ServerMsg::Event {
            event: ptyro_protocol::DomainEvent::ProjectAutoAdded { info, workspace, .. },
        } = msg
        {
            assert_eq!((info.slug.as_str(), workspace.as_str()), ("myrepo", "side"));
            break;
        }
    }

    // Undo: the project goes, its running session lets go of it and keeps running.
    c.request(Request::ProjectRemove { addr: p.id.clone(), release: true }).await.unwrap();
    assert!(projects(&c).await.is_empty());
    let one = wait_for(&c, "one", "out of the project", |s| s.project.is_none()).await;
    assert!(one.state.is_live());
    // …and a new session there doesn't bring it back.
    let two = spawn(&c, spec("two")).await;
    assert!(two.project.is_none() && projects(&c).await.is_empty(), "declined stays declined");
    // Adding it by hand works, goes to the workspace the TUI showed last, and lifts the decline.
    c.request(Request::SaveView {
        name: "_tui".into(),
        view: serde_json::json!({ "workspace": "side" }),
    })
    .await
    .unwrap();
    let added = match c
        .request(Request::ProjectAdd { path: dir.clone(), workspace: None })
        .await
        .unwrap()
    {
        Reply::Project { info, .. } => info,
        other => panic!("{other:?}"),
    };
    assert!(workspace(&c, "side").await.projects.contains(&added.id));
    // The sessions already there move in within a sweep.
    wait_for(&c, "two", "back in the project", |s| s.project.as_deref() == Some(&added.id)).await;
}

/// A session started in a subdirectory of a project starts there, not at the worktree's root;
/// one started on a worktree by address starts at its root.
#[tokio::test]
async fn a_session_starts_in_the_directory_asked_for_inside_a_project() {
    let w = World::new("exec sleep 60");
    let c = w.d.client().await.unwrap();
    let dir = repo(&w, "myrepo");
    let sub = dir.join("deep/sub");
    std::fs::create_dir_all(&sub).unwrap();
    let spec = |name: &str, cwd: &Path, worktree: Option<&str>| SpawnSpec {
        cwd: cwd.to_path_buf(),
        name: Some(name.into()),
        worktree: worktree.map(str::to_owned),
        size: Some(TermSize::new(80, 20)),
        ..SpawnSpec::default()
    };
    let s = spawn(&c, spec("in-sub", &sub, None)).await;
    assert_eq!(s.cwd, sub, "where it was asked to start");
    assert!(s.worktree.is_some(), "still placed in the project's main checkout");
    let at_root = spawn(&c, spec("on-wt", &sub, Some("myrepo/@root"))).await;
    assert_eq!(at_root.cwd, dir, "a worktree by address starts at its root");
}

/// UX1.5: a shell that `cd`s into another repository puts that one in the sidebar too.
#[tokio::test]
async fn a_shell_that_moves_into_a_repository_registers_it() {
    let w = World::new("exec sleep 60");
    let other = repo(&w, "other");
    // OSC 7 says where the shell is now.
    let script =
        format!("sleep 0.5; printf '\\033]7;file://host{}\\007'; exec sleep 60", other.display());
    std::fs::write(w.home().join("fake-shell"), format!("#!/bin/sh\n{script}\n")).unwrap();
    let c = w.d.client().await.unwrap();
    spawn(
        &c,
        SpawnSpec {
            cwd: std::env::temp_dir(),
            name: Some("wanderer".into()),
            size: Some(TermSize::new(80, 20)),
            ..SpawnSpec::default()
        },
    )
    .await;
    let s =
        wait_for(&c, "wanderer", "placed in the repository it moved to", |s| s.project.is_some())
            .await;
    let ps = projects(&c).await;
    assert_eq!(ps.len(), 1);
    assert_eq!(s.project.as_deref(), Some(ps[0].id.as_str()));
    assert_eq!(ps[0].slug, "other");
}

/// UX1.4: Claude typed into a shell session isn't one long "working" command: the session
/// notes it (`zsh › claude`) and reads its status off its screen, at low confidence.
#[tokio::test]
async fn an_agent_started_by_hand_in_a_shell_reports_idle_blocked_and_working() {
    // A real process named `claude` (ptyro by another name), started from the shell after an
    // OSC 133 `C`, which used to mean "working" for good.
    let w = World::new("exec sleep 60");
    let claude = w.home().join("claude");
    named(&claude);
    let script = format!(
        "printf '\\033]133;C;cmdline=claude\\007'; exec {} fake-agent --no-probe --claude-screen",
        claude.display()
    );
    std::fs::write(w.home().join("fake-shell"), format!("#!/bin/sh\n{script}\n")).unwrap();
    let c = w.d.client().await.unwrap();
    let sh = SpawnSpec {
        cwd: std::env::temp_dir(),
        name: Some("sh".into()),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    spawn(&c, sh).await;
    let status = |s: &SessionInfo| s.status.as_ref().map(|r| (r.status, r.confidence, r.source));
    use ptyro_core::status::{Confidence, SignalSource};
    let s = wait_for(&c, "sh", "idle, read off Claude's screen", |s| {
        s.inner_agent.is_some()
            && status(s) == Some((AgentStatus::Idle, Confidence::Low, SignalSource::Screen))
    })
    .await;
    assert_eq!(s.inner_agent.as_deref(), Some("claude"));
    assert_eq!(s.foreground.as_deref(), Some("claude"));

    let type_in = |text: &'static str| {
        let c = &c;
        async move {
            c.request(Request::SendText {
                addr: "sh".into(),
                text: text.into(),
                enter: true,
                wait_ready_ms: None,
            })
            .await
            .unwrap();
        }
    };
    type_in("ask npm install").await;
    let s = wait_for(&c, "sh", "blocked on its permission dialog", |s| {
        status(s).is_some_and(|(st, _, _)| st == AgentStatus::Blocked)
    })
    .await;
    assert_eq!(s.status.unwrap().kind.as_deref(), Some("permission"));
    type_in("y").await;
    wait_for(&c, "sh", "idle once answered", |s| {
        status(s).is_some_and(|(st, _, _)| st == AgentStatus::Idle)
    })
    .await;
    type_in("work 2.5").await;
    wait_for(&c, "sh", "working while it says so", |s| {
        status(s).is_some_and(|(st, _, _)| st == AgentStatus::Working)
    })
    .await;
    wait_for(&c, "sh", "idle again after", |s| {
        status(s).is_some_and(|(st, _, _)| st == AgentStatus::Idle)
    })
    .await;
}

/// UX1.4: adopting the Claude started by hand: it exits, and a managed session resumes the
/// conversation it was in (its newest transcript for that directory).
#[tokio::test]
async fn adopting_a_hand_started_claude_resumes_its_conversation() {
    let w = World::new("exec sleep 60");
    let claude = w.home().join("claude");
    named(&claude);
    let work = w.home().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let script = format!("exec {} fake-agent --no-probe --claude-screen", claude.display());
    std::fs::write(w.home().join("fake-shell"), format!("#!/bin/sh\n{script}\n")).unwrap();
    // Claude's transcript of the conversation it's in, where Claude keeps them.
    let slug: String = work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let transcripts = w.home().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&transcripts).unwrap();
    std::fs::write(transcripts.join("conv-1234.jsonl"), "{}\n").unwrap();
    let c = w.d.client().await.unwrap();
    let sh = SpawnSpec {
        cwd: work.clone(),
        name: Some("sh".into()),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    spawn(&c, sh).await;
    wait_for(&c, "sh", "Claude in its foreground", |s| s.inner_agent.is_some()).await;
    let adopted = match c.request(Request::Adopt { addr: "sh".into() }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(adopted.agent.as_deref(), Some("claude"));
    assert_eq!(adopted.conversation.as_deref(), Some("conv-1234"));
    w.d.wait_for_text(&c, &adopted.name, "session=conv-1234", WAIT).await.unwrap();
    let screen = w.d.wait_for_text(&c, &adopted.name, "resumed=", WAIT).await.unwrap();
    assert!(screen.contains("resumed=true"), "{screen}");
}

/// UX1.24: a name says what it's for and a title says it in words; both survive a restart.
#[tokio::test]
async fn sessions_and_worktrees_are_named_and_titled_after_what_they_are_for() {
    let mut w = World::new("exec sleep 60");
    let dir = repo(&w, "myrepo");
    let c = w.d.client().await.unwrap();
    let agent = |name: Option<&str>, prompt: Option<&str>| SpawnSpec {
        agent: Some("claude".into()),
        cwd: std::env::temp_dir(),
        name: name.map(Into::into),
        prompt: prompt.map(Into::into),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    let s = spawn(&c, agent(None, Some("say fix the flaky test"))).await;
    assert_eq!(s.name, "say-fix-flaky", "named after its prompt");
    assert_eq!(s.title.as_deref(), Some("say fix the flaky test"));
    let plain = spawn(&c, agent(None, None)).await;
    assert!(
        plain.name.starts_with("claude-") && !plain.name.ends_with(char::is_numeric),
        "an agent and a corvid, not claude-2: {}",
        plain.name
    );
    // A worktree for a prompt: named and titled after it.
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        prompt: Some("say add a user tier".into()),
        agent: Some("claude".into()),
        fetch: Some(false),
        ..Default::default()
    };
    let wt = match c.request(Request::WorktreeNew { spec }).await.unwrap() {
        Reply::WorktreeCreated { info, .. } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(wt.name, "say-add-user");
    assert_eq!(wt.title.as_deref(), Some("say add a user tier"));
    // Retitle: the title changes, the name (the address) doesn't.
    let r = c
        .request(Request::Retitle {
            addr: s.name.clone(),
            title: Some("Flaky test, round 2".into()),
            worktree: false,
        })
        .await
        .unwrap();
    let Reply::Session { info } = r else { panic!("{r:?}") };
    assert_eq!(
        (info.name.as_str(), info.title.as_deref()),
        ("say-fix-flaky", Some("Flaky test, round 2"))
    );
    c.request(Request::Retitle {
        addr: "myrepo/say-add-user".into(),
        title: Some("Tiers".into()),
        worktree: true,
    })
    .await
    .unwrap();
    // Both are kept: after a daemon restart the agent comes back dormant with its title, and
    // the worktree keeps its own.
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let back = wait_for(&c, "say-fix-flaky", "back, dormant", |s| {
        s.state == ptyro_core::session::ProcessState::Dormant
    })
    .await;
    assert_eq!(back.title.as_deref(), Some("Flaky test, round 2"));
    let Reply::Worktrees { worktrees } =
        c.request(Request::WorktreeList { project: None }).await.unwrap()
    else {
        panic!()
    };
    let t = worktrees.iter().find(|x| x.name == "say-add-user").unwrap();
    assert_eq!(t.title.as_deref(), Some("Tiers"));
}

/// UX1.9, end to end from keys: `C-q n`, say what to do, ⏎: the default agent gets the prompt,
/// in a new worktree named after it, and the pane follows it.
#[tokio::test]
async fn the_task_box_starts_an_agent_on_what_you_typed() {
    let w = World::new("exec sleep 60");
    let dir = repo(&w, "proj");
    let c = w.d.client().await.unwrap();
    // A shell in the repository (which registers it) to attach to.
    spawn(
        &c,
        SpawnSpec {
            cwd: dir.clone(),
            name: Some("sh0".into()),
            size: Some(TermSize::new(80, 20)),
            ..SpawnSpec::default()
        },
    )
    .await;
    let mut script =
        ptyro_testkit::headless::Script::new(vec![BIN.to_owned(), "attach".into(), "sh0".into()])
            .size(110, 24)
            .timeout(11.0)
            .env("HOME", &w.home().to_string_lossy());
    for (k, v) in w.d.env() {
        script = script.env(&k, &v);
    }
    // Keys go in once the screen shows what they wait for: ⏎ only once the box previews the
    // name, agent and place it will use.
    let r = ptyro_testkit::headless::run(
        script
            .timeout(30.0)
            .send_when("WORKSPACE", "\\x11n")
            .send_when("What should it do?", "work 0.3")
            .send_when("→ work-0-3 · claude · new worktree in proj", "\\r")
            .until("> work 0.3"),
    )
    .await
    .expect("headless run");
    let s = wait_for(&c, "work-0-3", "the agent, given the prompt", |s| {
        s.task.as_deref() == Some("work 0.3")
    })
    .await;
    assert_eq!(s.title.as_deref(), Some("work 0.3"));
    let Reply::Worktrees { worktrees } =
        c.request(Request::WorktreeList { project: None }).await.unwrap()
    else {
        panic!()
    };
    let wt = worktrees.iter().find(|x| x.name == "work-0-3").expect("its worktree");
    assert_eq!(s.worktree.as_deref(), Some(wt.id.as_str()));
    assert_eq!(wt.title.as_deref(), Some("work 0.3"));
    assert!(r.screen.contains("> work 0.3"), "the pane follows the agent: {}", r.screen);
}

/// UX1.26: the task box takes several lines (Ctrl+J here; Shift+Enter and Alt+Enter too), and
/// the agent gets them as one prompt.
#[tokio::test]
async fn the_task_box_takes_several_lines_as_one_prompt() {
    // The fake agent takes pastes the way Claude and Codex do (bracketed, one prompt).
    let w = World::with_config("exec sleep 60", "[agents.claude.env]\nPTYRO_FAKE_PASTE = \"1\"\n");
    let dir = repo(&w, "proj");
    let c = w.d.client().await.unwrap();
    spawn(
        &c,
        SpawnSpec {
            cwd: dir.clone(),
            name: Some("sh0".into()),
            size: Some(TermSize::new(80, 20)),
            ..SpawnSpec::default()
        },
    )
    .await;
    let mut script =
        ptyro_testkit::headless::Script::new(vec![BIN.to_owned(), "attach".into(), "sh0".into()])
            .size(110, 24)
            .timeout(30.0)
            .env("HOME", &w.home().to_string_lossy());
    for (k, v) in w.d.env() {
        script = script.env(&k, &v);
    }
    let r = ptyro_testkit::headless::run(
        script
            .send_when("WORKSPACE", "\\x11n")
            .send_when("What should it do?", "fix the login")
            .send_when("fix the login", "\\x0a")
            .send_when("new line", "and the logout")
            .send_when("and the logout", "\\r")
            .until("prompt over 2 lines"),
    )
    .await
    .expect("headless run");
    assert!(
        r.screen.contains("prompt over 2 lines: fix the login ⏎ and the logout"),
        "{}",
        r.screen
    );
    let s = wait_for(&c, "fix-login-logout", "the agent", |s| s.task.is_some()).await;
    assert_eq!(s.task.as_deref(), Some("fix the login and the logout"));
}

/// UX1.12: archiving keeps everything, uncommitted and untracked work included; restoring puts
/// it back as it was.
#[tokio::test]
async fn archive_keeps_uncommitted_work_and_restore_brings_it_back() {
    let w = World::new("exec sleep 60");
    let dir = repo(&w, "myrepo");
    let c = w.d.client().await.unwrap();
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("feat".into()),
        fetch: Some(false),
        ..Default::default()
    };
    let wt = match c.request(Request::WorktreeNew { spec }).await.unwrap() {
        Reply::WorktreeCreated { info, .. } => info,
        other => panic!("{other:?}"),
    };
    std::fs::write(wt.path.join("README"), "changed\n").unwrap();
    std::fs::write(wt.path.join("notes.txt"), "untracked\n").unwrap();
    // Removing refuses (work would be lost); archiving doesn't, and loses nothing.
    let refused = c
        .request(Request::WorktreeRemove { addr: "myrepo/feat".into(), opts: Default::default() })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ptyro_core::ErrorCode::DirtyWorktree);
    let Reply::Notes { notes } = c
        .request(Request::WorktreeArchive { addr: "myrepo/feat".into(), stop_sessions: false })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(notes[0].contains("refs/ptyro/archive/feat"), "{notes:?}");
    assert!(!wt.path.exists());
    let Reply::Worktree { info } =
        c.request(Request::WorktreeRestore { addr: "feat".into() }).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(info.branch.as_deref(), Some("feat"));
    assert_eq!(std::fs::read_to_string(info.path.join("README")).unwrap(), "changed\n");
    assert_eq!(std::fs::read_to_string(info.path.join("notes.txt")).unwrap(), "untracked\n");
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&info.path)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.contains(" M README") && status.contains("?? notes.txt"),
        "uncommitted again: {status}"
    );
}

fn git_out(w: &World, dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", w.home())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// UX1.10: a new worktree can take the main checkout's uncommitted work, or check out an
/// existing branch.
#[tokio::test]
async fn work_moves_into_a_worktree_or_starts_from_a_branch() {
    let w = World::new("exec sleep 60");
    let dir = repo(&w, "myrepo");
    git(&w.home(), &dir, &["branch", "spike"]);
    std::fs::write(dir.join("README"), "edited in main\n").unwrap();
    std::fs::write(dir.join("todo.txt"), "new\n").unwrap();
    let c = w.d.client().await.unwrap();
    let new = |spec: ptyro_core::model::WorktreeSpec| {
        let c = &c;
        async move {
            match c.request(Request::WorktreeNew { spec }).await.unwrap() {
                Reply::WorktreeCreated { info, notes, .. } => (info, notes),
                other => panic!("{other:?}"),
            }
        }
    };
    let (wt, notes) = new(ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("moved".into()),
        take_changes: true,
        fetch: Some(false),
        ..Default::default()
    })
    .await;
    assert!(
        notes.iter().any(|n| n.contains("took the main checkout's uncommitted changes")),
        "{notes:?}"
    );
    assert_eq!(std::fs::read_to_string(wt.path.join("README")).unwrap(), "edited in main\n");
    assert!(wt.path.join("todo.txt").exists());
    assert!(git_out(&w, &dir, &["status", "--porcelain"]).trim().is_empty(), "main is clean now");
    assert!(
        !git_out(&w, &dir, &["rev-parse", "refs/ptyro/taken/moved"]).trim().is_empty(),
        "a copy"
    );
    // Nothing left to take: refused, nothing made.
    let e = c
        .request(Request::WorktreeNew {
            spec: ptyro_core::model::WorktreeSpec {
                project: dir.to_string_lossy().into_owned(),
                take_changes: true,
                fetch: Some(false),
                ..Default::default()
            },
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("nothing to take"), "{}", e.message);
    // Branches a worktree could check out: `spike` (main and `moved` have worktrees).
    let Reply::Branches { names } =
        c.request(Request::Branches { project: "myrepo".into() }).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(names, vec!["spike".to_owned()]);
}

/// UX1.11: a project's first worktree gets what its lockfile and `.env` imply, written to
/// `.ptyro.local.toml` after a yes, trusted, and never offered again.
#[tokio::test]
async fn a_first_worktree_is_offered_its_setup_once() {
    let w = World::new("exec sleep 60");
    let dir = repo(&w, "web");
    std::fs::write(dir.join("package-lock.json"), "{}").unwrap();
    std::fs::write(dir.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(dir.join(".env"), "TOKEN=1\n").unwrap();
    git(&w.home(), &dir, &["add", "package-lock.json", ".gitignore"]);
    git(&w.home(), &dir, &["commit", "-qm", "deps"]);
    let c = w.d.client().await.unwrap();
    c.request(Request::ProjectAdd { path: dir.clone(), workspace: None }).await.unwrap();
    let suggest = |apply: Option<bool>| {
        let c = &c;
        async move {
            match c.request(Request::ProjectSuggest { addr: "web".into(), apply }).await.unwrap() {
                Reply::Suggestions { suggestions } => suggestions,
                other => panic!("{other:?}"),
            }
        }
    };
    let s = suggest(None).await;
    assert_eq!(
        (s.setup.as_deref(), s.from.as_deref()),
        (Some("npm ci"), Some("package-lock.json"))
    );
    assert_eq!(s.copy, vec![".env".to_owned()]);
    assert!(!s.offered);
    let after = suggest(Some(true)).await;
    assert!(after.offered && after.setup.is_none(), "taken: nothing more to suggest: {after:?}");
    let local = std::fs::read_to_string(dir.join(".ptyro.local.toml")).unwrap();
    assert!(local.contains("setup = \"npm ci\"") && local.contains("copy = [\".env\"]"), "{local}");
    let Reply::Scripts { scripts } =
        c.request(Request::ProjectScripts { addr: "web".into(), worktree: None }).await.unwrap()
    else {
        panic!()
    };
    assert!(scripts.trusted, "the scripts you just approved are trusted");
}

/// UX1.19: on a fresh machine (no config, nothing running) `ptyro` opens the first run; once
/// it's done, never again.
#[tokio::test]
async fn a_fresh_ptyro_opens_the_first_run_once() {
    let d = TempDaemon::start(BIN).unwrap();
    let home = tempfile::tempdir().unwrap();
    let script = |d: &TempDaemon| {
        let mut s = ptyro_testkit::headless::Script::new(vec![BIN.to_owned()])
            .size(110, 30)
            .timeout(30.0)
            .env("HOME", &home.path().to_string_lossy());
        for (k, v) in d.env() {
            s = s.env(&k, &v);
        }
        s
    };
    // Esc only once the first run is on screen (it never is: the run times out); it writes a
    // config, so the next start skips it.
    let r = ptyro_testkit::headless::run(
        script(&d)
            .send_when("Welcome to ptyro", "\x1b")
            .send_when("WORKSPACE", "\\x11d")
            .until("[detached]"),
    )
    .await
    .unwrap();
    assert!(r.screen.contains("[detached]"), "{}", r.screen);
    assert!(d.paths.config.join("config.toml").exists(), "a config now, so it won't come back");
    // The second start: the TUI as soon as it's drawn, with no first run over it.
    let r = ptyro_testkit::headless::run(script(&d).until("WORKSPACE")).await.unwrap();
    assert!(r.screen.contains("WORKSPACE"), "{}", r.screen);
    assert!(!r.screen.contains("Welcome to ptyro"), "{}", r.screen);
}

/// UX1.3: with `[summaries]` on, a finished turn gets a line from the summarizer, which reads a
/// redacted prompt on stdin; `Summarize` asks for one now.
#[tokio::test]
async fn a_finished_turn_is_summarized_in_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let (prompt, script) = (dir.path().join("prompt.txt"), dir.path().join("summarizer"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat > '{}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"Fixing the flaky login test.\"}}'\n",
            prompt.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let extra = format!("[summaries]\nenabled = true\ncommand = [\"{}\"]\n", script.display());
    let w = World::with_config("exec sleep 30", &extra);
    let c = w.d.client().await.unwrap();
    let agent = SpawnSpec {
        agent: Some("claude".into()),
        cwd: std::env::temp_dir(),
        name: Some("cl".into()),
        size: Some(TermSize::new(100, 20)),
        prompt: Some("work 0.3 fix login, key sk-ant-api03-abcdefghijklmnopqrst".into()),
        ..SpawnSpec::default()
    };
    spawn(&c, agent).await;
    let s = wait_for(&c, "cl", "summarized", |s| s.summary.is_some()).await;
    let sm = s.summary.unwrap();
    assert_eq!(sm.text, "Fixing the flaky login test.");
    assert_eq!(sm.turn, s.turns);
    let sent = std::fs::read_to_string(&prompt).unwrap();
    assert!(sent.contains("Asked to: work 0.3 fix login"), "{sent}");
    assert!(sent.contains("<redacted>") && !sent.contains("sk-ant-api03"), "{sent}");

    // On request, too.
    std::fs::remove_file(&prompt).unwrap();
    match c.request(Request::Summarize { addr: "cl".into() }).await.unwrap() {
        Reply::Session { info } => assert!(info.summary.is_some()),
        other => panic!("{other:?}"),
    }
    assert!(prompt.exists());
}

async fn quick(
    c: &Client,
    worktree: Option<&str>,
    cwd: &Path,
    create: bool,
) -> Result<SessionInfo, ptyro_core::Error> {
    let req = Request::QuickShell {
        worktree: worktree.map(Into::into),
        cwd: cwd.to_owned(),
        create,
        env: Default::default(),
        size: Some(TermSize::new(80, 12)),
        workspace: None,
    };
    match c.request(req).await? {
        Reply::Session { info } => Ok(info),
        other => panic!("{other:?}"),
    }
}

async fn listed(c: &Client) -> Vec<SessionInfo> {
    match c.request(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

/// UX1.25: one quick terminal per worktree (only a worktree, UX3.4), reused; quiet but for a blocked
/// agent in it; at its prompt it goes with its worktree, running something it holds it up; and
/// `exit` forgets it.
#[tokio::test]
async fn a_quick_terminal_is_one_per_worktree_quiet_and_goes_with_it() {
    // An interactive shell with job control: a command runs as the foreground process group.
    let w = World::new("exec /bin/sh -i");
    let dir = repo(&w, "myrepo");
    let c = w.d.client().await.unwrap();
    let spec = ptyro_core::model::WorktreeSpec {
        project: dir.to_string_lossy().into_owned(),
        name: Some("feat".into()),
        fetch: Some(false),
        ..Default::default()
    };
    let wt = match c.request(Request::WorktreeNew { spec }).await.unwrap() {
        Reply::WorktreeCreated { info, .. } => info,
        other => panic!("{other:?}"),
    };
    let none = quick(&c, Some("myrepo/feat"), Path::new(""), false).await.unwrap_err();
    assert_eq!(none.code, ptyro_core::ErrorCode::NotFound, "finding never starts one");
    let shell = quick(&c, Some("myrepo/feat"), Path::new(""), true).await.unwrap();
    assert!(shell.quick);
    assert_eq!(
        (shell.name.as_str(), shell.worktree.as_deref()),
        ("feat.shell", Some(wt.id.as_str()))
    );
    // From anywhere in the worktree, by address or by directory: the same one.
    std::fs::create_dir_all(wt.path.join("src")).unwrap();
    assert_eq!(quick(&c, None, &wt.path.join("src"), true).await.unwrap().id, shell.id);
    assert_eq!(quick(&c, Some("myrepo/feat"), Path::new(""), true).await.unwrap().id, shell.id);
    // `ptyro shell`: the same one, as JSON or its screen (what an agent reads).
    let out =
        w.d.cmd().args(["shell", "--json"]).current_dir(wt.path.join("src")).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let info: SessionInfo = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info.id, shell.id);
    let out = w.d.cmd().args(["shell", "feat", "--read"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = w.d.cmd().args(["shell", "--read"]).current_dir(w.home()).output().unwrap();
    assert!(!out.status.success(), "--read never starts one");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no quick terminal"));
    // Outside any worktree there is none (UX3.4): it belongs to a worktree.
    let notes = w.home().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let refused = quick(&c, None, &notes, true).await.unwrap_err();
    assert!(refused.message.contains("belongs to a worktree"), "{}", refused.message);
    // The main checkout's is its own, named after the project.
    let loose = quick(&c, Some("myrepo/@root"), Path::new(""), true).await.unwrap();
    assert_ne!(loose.id, shell.id);
    assert_eq!(loose.name, "myrepo.shell");

    // Quiet: an error says nothing; an agent in it that waits for you does.
    let report = |state: &str| Request::Report {
        addr: shell.id.full(),
        state: state.into(),
        kind: None,
        msg: None,
        progress: None,
    };
    c.request(report("error")).await.unwrap();
    let s = wait_for(&c, &shell.name, "errored", |s| {
        s.status.as_ref().is_some_and(|r| r.status == AgentStatus::Errored)
    })
    .await;
    assert!(s.attention.is_none(), "{:?}", s.attention);
    c.request(report("blocked")).await.unwrap();
    wait_for(&c, &shell.name, "blocked raises attention", |s| s.attention.is_some()).await;

    // Running something, it holds up removing the worktree; at its prompt it goes with it.
    let send = |text: &str| Request::SendText {
        addr: shell.id.full(),
        text: text.into(),
        enter: true,
        wait_ready_ms: None,
    };
    c.request(send("sleep 30")).await.unwrap();
    wait_for(&c, &shell.name, "sleep in the foreground", |s| {
        s.foreground.as_deref() == Some("sleep")
    })
    .await;
    let remove =
        || Request::WorktreeRemove { addr: "myrepo/feat".into(), opts: Default::default() };
    let refused = c.request(remove()).await.unwrap_err();
    assert!(refused.message.contains("feat.shell"), "{}", refused.message);
    c.request(Request::SendBytes { addr: shell.id.full(), bytes: b"\x03".to_vec() }).await.unwrap();
    wait_for(&c, &shell.name, "back at its prompt", |s| {
        s.foreground.as_deref().is_some_and(|f| f != "sleep")
    })
    .await;
    c.request(remove()).await.unwrap();
    let deadline = Instant::now() + WAIT;
    while listed(&c).await.iter().any(|s| s.id == shell.id) {
        assert!(Instant::now() < deadline, "the quick terminal went with its worktree");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // `exit` forgets it, quietly; the next one is a fresh shell.
    c.request(Request::SendText {
        addr: loose.id.full(),
        text: "exit".into(),
        enter: true,
        wait_ready_ms: None,
    })
    .await
    .unwrap();
    let deadline = Instant::now() + WAIT;
    while listed(&c).await.iter().any(|s| s.id == loose.id) {
        assert!(Instant::now() < deadline, "exit forgets it");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let fresh = quick(&c, Some("myrepo/@root"), Path::new(""), true).await.unwrap();
    assert_ne!(fresh.id, loose.id);
    assert_eq!(fresh.name, "myrepo.shell", "its name is free again");
}

/// `C-q C-d` asks, and `y` stops every session and the daemon, then ends the TUI.
#[tokio::test]
async fn quitting_from_the_tui_stops_the_daemon() {
    let d = TempDaemon::start(BIN).unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(&d.paths.config).unwrap();
    std::fs::write(d.paths.config.join("config.toml"), "").unwrap();
    let mut s = ptyro_testkit::headless::Script::new(vec![BIN.to_owned()])
        .size(110, 30)
        .timeout(20.0)
        .env("HOME", &home.path().to_string_lossy());
    for (k, v) in d.env() {
        s = s.env(&k, &v);
    }
    // `y` only once the dialog says what stops: the shell this TUI opened ends for good.
    let r = ptyro_testkit::headless::run(
        s.send_when("WORKSPACE", "\\x11\\x04").send_when("1 session(s) end for good", "y"),
    )
    .await
    .unwrap();
    assert_eq!(r.exit, Some(0), "{}", r.screen);
    assert!(r.screen.contains("[ptyro stopped"), "{}", r.screen);
    assert!(d.client().await.is_err(), "the daemon is gone");
}

/// UX1.22: `ptyro tour` is the daily loop for real: a task, its permission prompt answered from
/// peek, its diff reviewed, finished into main, cleaned up. Each key goes in once the coach
/// asks for it. The tour runs in a ptyro of its own and deletes it afterwards.
#[tokio::test]
async fn the_tour_walks_the_loop_for_real_and_cleans_up() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
    let script = ptyro_testkit::headless::Script::new(vec![BIN.to_owned(), "tour".into()])
        .size(120, 32)
        .timeout(90.0)
        .env("HOME", &home.path().to_string_lossy())
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let r = ptyro_testkit::headless::run(
        script
            .send_when("Tour 1/5: start a task", "\\x11n")
            .send_when("What should it do?", "fix the typo in greeting.txt")
            .send_when("fix-typo-greeting", "\\r")
            .send_when("to peek, then y", "\\x11p")
            .send_when("y allow", "y")
            .send_when("Tour 3/5", "")
            .send_when("It's done", "\\x11D")
            .send_when("Tour 4/5", "q")
            .send_when("Tour 4/5", "\\x11\\t")
            .send_when("Tour 4/5", "F")
            .send_when("Finish ", "i")
            .send_when("Tour 5/5", "F")
            .send_when("snapshot", "\\r")
            .send_when("that's the loop", "\\x11d")
            .until("the tour is over"),
    )
    .await
    .expect("headless run");
    // Its last words come after its daemon stopped and its directory was deleted.
    assert!(r.screen.contains("the tour is over"), "{}", r.screen);
}

/// UX1's "Done when", as written: on a fresh machine with only `claude` installed and no
/// config, `cd myrepo && ptyro` registers the repo; `Ctrl-Q n`, a task, `⏎` puts Claude to work
/// in a new worktree named after it; its row shows the task and, while it's blocked, what it
/// wants and the keys that answer it; finishing commits and opens a PR, and once it merges one
/// key cleans up. By keys alone. (`claude` here is the pretend agent, `gh` the fake one.)
#[tokio::test]
async fn ux1_done_when_from_a_fresh_machine_to_a_merged_and_cleaned_up_pr() {
    let tmp = tempfile::Builder::new().prefix("ptyro-ux1-done-").tempdir().unwrap();
    let h = tmp.path().canonicalize().unwrap();
    std::fs::write(
        h.join(".gitconfig"),
        "[user]\n\tname = Owner\n\temail = owner@example.com\n[init]\n\tdefaultBranch = main\n",
    )
    .unwrap();
    // Only `claude` installed (the pretend one), and a `gh`.
    let bin = h.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = |name: &str, body: String| {
        std::fs::write(bin.join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(
            bin.join(name),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    };
    exe(
        "claude",
        format!(
            "PTYRO_FAKE_TOUR=1 PTYRO_FAKE_PASTE=1 exec \"{BIN}\" fake-agent --as claude \"$@\""
        ),
    );
    exe("gh", format!("exec \"{BIN}\" fake-gh \"$@\""));
    let gh = h.join("gh-state");
    std::fs::create_dir_all(&gh).unwrap();
    // myrepo, cloned from a "remote" (where the PR's branch is pushed).
    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &h)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&h, &["init", "-q", "--bare", "myrepo.git"]);
    git(&h, &["clone", "-q", "myrepo.git", "myrepo"]);
    let repo = h.join("myrepo");
    std::fs::write(repo.join("greeting.txt"), "Helo, world!\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "A greeting"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let (hs, gs) = (h.to_string_lossy().into_owned(), gh.to_string_lossy().into_owned());
    let env = [
        ("HOME", hs.as_str()),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("SHELL", "/bin/sh"),
        ("PATH", path.as_str()),
        ("PTYRO_FAKE_GH", gs.as_str()),
        ("PTYRO_TEST_FAST_POLL", "1"),
    ];
    // No config at all.
    let d = TempDaemon::start_with(BIN, &env, None).unwrap();
    assert!(!d.paths.config.join("config.toml").exists());
    // The PR merges on the forge once it's open.
    let merger = {
        let gh = gh.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                if let Ok(t) = std::fs::read_to_string(gh.join("pr.json"))
                    && let Ok(mut pr) = serde_json::from_str::<serde_json::Value>(&t)
                {
                    pr["state"] = serde_json::json!("MERGED");
                    std::fs::write(gh.join("pr.json"), pr.to_string()).unwrap();
                    return true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            false
        })
    };
    let mut script = ptyro_testkit::headless::Script::new(vec![BIN.to_owned()])
        .cwd(&repo)
        .size(120, 32)
        .timeout(120.0);
    for (k, v) in env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).chain(d.env()) {
        script = script.env(&k, &v);
    }
    let r = ptyro_testkit::headless::run(
        script
            // 1. The first run (no config): ⏎ keeps what it found, myrepo included.
            .send_when("Welcome to ptyro", "\\r")
            .send_when("WORKSPACE", "\\x11n")
            // 2. A task, ⏎.
            .send_when("What should it do?", "fix the typo in greeting.txt")
            .send_when("fix-typo-greeting", "\\r")
            // 3. Blocked: its row says what it wants, and the keys; answered from peek.
            .send_when("y/a/n answers", "\\x11p")
            .send_when("y allow", "y")
            // 4. Finish into a PR, from the sidebar.
            .send_when("Fixed the typo", "\\x11\\t")
            .send_when("Fixed the typo", "F")
            .send_when("a pull request", "p")
            // Merged: one key cleans up (F, as the hint says), ⏎ agrees.
            .send_when("clean up (merged)", "F")
            .send_when("snapshot", "\\r")
            .send_when("archived", "\\x11d")
            .until("[detached]"),
    )
    .await
    .expect("headless run");
    assert!(merger.join().unwrap(), "a PR was opened");
    assert!(r.screen.contains("[detached]"), "{}", r.screen);
    let c = d.client().await.unwrap();
    let Reply::Projects { projects } = c.request(Request::ProjectList).await.unwrap() else {
        panic!()
    };
    assert_eq!(projects.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(), ["myrepo"]);
    let Reply::Archives { entries } =
        c.request(Request::WorktreeArchives { project: None }).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(entries.len(), 1, "cleaned up: archived, its branch kept");
    assert_eq!(entries[0].name, "fix-typo-greeting");
}

async fn last_launch(c: &Client, ws: &str) -> Option<ptyro_core::session::LastLaunch> {
    match c.request(Request::LastLaunch { workspace: ws.into() }).await.unwrap() {
        Reply::LastLaunch { launch } => launch,
        other => panic!("{other:?}"),
    }
}

/// UX3.2 quick-new: `again` starts what the workspace last started (the default agent before
/// anything), a tool or a quick terminal doesn't count, and `ptyro new --again` does the same.
#[tokio::test]
async fn again_starts_what_the_workspace_last_started() {
    let w = World::new("exec /bin/sh -i");
    let dir = repo(&w, "myrepo");
    let c = w.d.client().await.unwrap();
    let again = || SpawnSpec { again: true, cwd: dir.clone(), ..SpawnSpec::default() };
    assert_eq!(last_launch(&c, "main").await, None);
    // Nothing yet: the default agent.
    let first = spawn(&c, again()).await;
    assert_eq!(first.agent.as_deref(), Some("claude"));
    assert_eq!(last_launch(&c, "main").await.map(|l| l.label()).as_deref(), Some("claude"));
    // A shell, then again: a shell.
    spawn(&c, SpawnSpec { cwd: dir.clone(), ..SpawnSpec::default() }).await;
    assert_eq!(last_launch(&c, "main").await.map(|l| l.label()).as_deref(), Some("shell"));
    let second = spawn(&c, again()).await;
    assert_eq!(second.kind, ptyro_core::session::SessionKind::Shell);
    // A tool and a quick terminal leave it as it was.
    spawn(
        &c,
        SpawnSpec {
            argv: vec!["true".into()],
            tool: true,
            cwd: dir.clone(),
            ..SpawnSpec::default()
        },
    )
    .await;
    quick(&c, Some("myrepo/@root"), Path::new(""), true).await.unwrap();
    assert_eq!(last_launch(&c, "main").await.map(|l| l.label()).as_deref(), Some("shell"));
    // The CLI.
    let out =
        w.d.cmd().args(["new", "--again", "-d", "--json"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let info: SessionInfo = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(info.kind, ptyro_core::session::SessionKind::Shell);
}
