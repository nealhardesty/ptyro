//! UX1 end-to-end tests (ROADMAP UX1): what the out-of-the-box experience needs from a real
//! daemon, with the fake agent standing in for the agents.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_core::status::AgentStatus;
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(15);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    /// A daemon whose `claude` is the fake agent, and whose `$SHELL` is `shell` (a script).
    fn new(shell: &str) -> Self {
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
            "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n"
        );
        let (hs, ss) = (h.to_string_lossy().into_owned(), sh.to_string_lossy().into_owned());
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", ss.as_str())];
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    #[allow(dead_code)]
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
