//! MM1.B, many machines (SPEC §16.4b): two daemons on this computer stand for two laptops. The
//! first links to the second over "ssh" (`PTYRO_SSH` is a script that runs `ptyro proxy` against
//! the second's socket), and mirrors its workspaces beside its own.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::model::{MachineInfo, MachineState, MachineTransport};
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct Two {
    /// laptop1: the one you sit at.
    here: TempDaemon,
    /// laptop2: mirrored.
    there: TempDaemon,
    _dir: tempfile::TempDir,
    work: PathBuf,
}

/// A script standing in for ssh: whatever it's asked, it bridges to the daemon whose variables
/// are in `env` (written once that daemon runs).
fn fake_ssh(dir: &Path, name: &str, env: &Path) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nexec env $(cat '{}') PTYRO_NO_AUTOSPAWN=1 '{BIN}' proxy\n",
            env.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

fn write_env(path: &Path, d: &TempDaemon) {
    let line: Vec<String> = d.env().into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    std::fs::write(path, line.join(" ")).unwrap();
}

impl Two {
    /// Two daemons, each able to reach the other over its "ssh".
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (env1, env2) = (root.join("laptop1.env"), root.join("laptop2.env"));
        let to1 = fake_ssh(&root, "ssh-to-laptop1", &env1).to_string_lossy().into_owned();
        let to2 = fake_ssh(&root, "ssh-to-laptop2", &env2).to_string_lossy().into_owned();
        let there = TempDaemon::start_with(
            BIN,
            &[("PTYRO_SSH", to1.as_str())],
            Some("[daemon]\nmachine_name = \"laptop2\"\n"),
        )
        .unwrap();
        let here = TempDaemon::start_with(
            BIN,
            &[("PTYRO_SSH", to2.as_str())],
            Some("[daemon]\nmachine_name = \"laptop1\"\n"),
        )
        .unwrap();
        write_env(&env1, &here);
        write_env(&env2, &there);
        Self { here, there, _dir: dir, work }
    }
}

async fn spawn(c: &Client, spec: SpawnSpec) -> SessionInfo {
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

fn shell(name: &str, cwd: &Path, workspace: Option<&str>) -> SpawnSpec {
    SpawnSpec {
        argv: vec!["sh".into(), "-c".into(), "echo ready-$PTYRO_SESSION_ID; exec cat".into()],
        cwd: cwd.to_owned(),
        name: Some(name.into()),
        size: Some(TermSize::new(80, 12)),
        workspace: workspace.map(Into::into),
        ..SpawnSpec::default()
    }
}

async fn machines(c: &Client) -> Vec<MachineInfo> {
    match c.request(Request::MachineList).await.unwrap() {
        Reply::Machines { machines } => machines,
        other => panic!("{other:?}"),
    }
}

async fn until_state(c: &Client, name: &str, want: MachineState) -> MachineInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let m = machines(c).await.into_iter().find(|m| m.name == name);
        if let Some(m) = m.filter(|m| m.state == want) {
            return m;
        }
        assert!(Instant::now() < deadline, "{name} never became {want:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn listed(c: &Client) -> Vec<SessionInfo> {
    match c.request(Request::List).await.unwrap() {
        Reply::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

async fn workspaces(c: &Client) -> Vec<String> {
    match c.request(Request::WorkspaceList).await.unwrap() {
        Reply::Workspaces { workspaces } => workspaces.into_iter().map(|w| w.name).collect(),
        other => panic!("{other:?}"),
    }
}

async fn screen(c: &Client, addr: &str) -> Result<String, ptyro_core::Error> {
    match c
        .request(Request::ReadScreen {
            addr: addr.into(),
            format: ptyro_protocol::ScreenFormat::Plain,
        })
        .await?
    {
        Reply::Screen { text, .. } => Ok(text),
        other => panic!("{other:?}"),
    }
}

async fn until_screen(c: &Client, addr: &str, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(s) = screen(c, addr).await
            && s.contains(needle)
        {
            return s;
        }
        assert!(Instant::now() < deadline, "{addr} never showed {needle:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn another_machines_workspaces_sit_beside_ours_and_work_from_here() {
    let mut t = Two::new();
    let there = t.there.client().await.unwrap();
    let laptop2_session = spawn(&there, shell("fix-flaky-test", &t.work, None)).await;
    let mut here = t.here.client().await.unwrap();
    let mut events = here.pushes();
    here.send(ClientMsg::WatchEvents).unwrap();
    spawn(&here, shell("local-one", &t.work, None)).await;

    // Adding it checks it first and takes the name it gives itself.
    let ssh = MachineTransport::Ssh { target: "laptop2".into() };
    let info = match here
        .request(Request::MachineAdd {
            name: None,
            transport: ssh.clone(),
            token: None,
            force: false,
        })
        .await
        .unwrap()
    {
        Reply::Machine { info } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(info.name, "laptop2");
    assert_eq!(info.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    let info = until_state(&here, "laptop2", MachineState::Online).await;
    assert_eq!(info.workspaces, ["main"]);
    // Twice: refused, as is a machine named like this one.
    let again = here
        .request(Request::MachineAdd {
            name: None,
            transport: ssh.clone(),
            token: None,
            force: false,
        })
        .await
        .unwrap_err();
    assert!(again.message.contains("already added"), "{}", again.message);

    // Its things, with its name on them, beside ours.
    let all = listed(&here).await;
    let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"local-one") && names.contains(&"laptop2:fix-flaky-test"), "{names:?}");
    let mirrored = all.iter().find(|s| s.id == laptop2_session.id).unwrap();
    assert_eq!(mirrored.machine.as_deref(), Some("laptop2"));
    assert_eq!(mirrored.workspace.as_deref(), Some("laptop2:main"));
    let ws = workspaces(&here).await;
    assert!(ws.contains(&"main".to_owned()) && ws.contains(&"laptop2:main".to_owned()), "{ws:?}");
    let ws_there = workspaces(&there).await;
    assert_eq!(ws_there, ["main"], "nothing changed there");

    // Addressed as laptop2:name, by its id, or by a name only it has: it happens there.
    until_screen(&here, "laptop2:fix-flaky-test", "ready-").await;
    here.request(Request::SendText {
        addr: "laptop2:fix-flaky-test".into(),
        text: "typed-from-laptop1".into(),
        enter: true,
        wait_ready_ms: None,
    })
    .await
    .unwrap();
    until_screen(&there, "fix-flaky-test", "typed-from-laptop1").await;
    until_screen(&here, &laptop2_session.id.full(), "typed-from-laptop1").await;
    until_screen(&here, "fix-flaky-test", "typed-from-laptop1").await;

    // Started in its workspace: it runs there, and its news reaches us with its name on.
    let made = spawn(&here, shell("made-from-here", &t.work, Some("laptop2:main"))).await;
    assert_eq!(made.name, "laptop2:made-from-here");
    assert_eq!(made.machine.as_deref(), Some("laptop2"));
    assert!(listed(&there).await.iter().any(|s| s.name == "made-from-here"));
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
            Ok(Some(ServerMsg::Event { event: DomainEvent::SessionCreated { info } }))
                if info.name == "laptop2:made-from-here" =>
            {
                break;
            }
            _ => assert!(Instant::now() < deadline, "no news of laptop2's new session"),
        }
    }

    // Watching and typing go there and back over a connection of this client's own.
    here.send(ClientMsg::Subscribe {
        session: laptop2_session.id,
        size: Some(TermSize::new(80, 12)),
        control: true,
    })
    .unwrap();
    let deadline = Instant::now() + WAIT;
    let mut seq = None;
    while seq.is_none() {
        assert!(Instant::now() < deadline, "no frame from laptop2's session");
        if let Ok(Some(ServerMsg::Screen { frame })) =
            tokio::time::timeout(Duration::from_millis(500), events.recv()).await
            && frame.session == laptop2_session.id
        {
            assert!(frame.keyframe, "the first frame is whole");
            seq = Some(frame.seq);
        }
    }
    here.send(ClientMsg::Ack { session: laptop2_session.id, seq: seq.unwrap() }).unwrap();
    here.send(ClientMsg::Input {
        session: laptop2_session.id,
        event: ptyro_core::input::InputEvent::Paste { text: "keys-over-the-upstream\r".into() },
    })
    .unwrap();
    until_screen(&there, "fix-flaky-test", "keys-over-the-upstream").await;

    // Hidden here: gone from our lists, untouched there; shown again: back.
    here.request(Request::MachineHide {
        name: "laptop2".into(),
        workspace: "main".into(),
        hidden: true,
    })
    .await
    .unwrap();
    assert!(!workspaces(&here).await.contains(&"laptop2:main".to_owned()));
    assert!(!listed(&here).await.iter().any(|s| s.machine.is_some()));
    here.request(Request::MachineHide {
        name: "laptop2".into(),
        workspace: "main".into(),
        hidden: false,
    })
    .await
    .unwrap();
    assert!(workspaces(&here).await.contains(&"laptop2:main".to_owned()));

    // Offline: still listed as it was, and saying so when asked to do something.
    drop(there);
    t.there.stop();
    let off = until_state(&here, "laptop2", MachineState::Offline).await;
    assert!(off.since_ms.is_some() && off.fix.is_some(), "{off:?}");
    assert!(listed(&here).await.iter().any(|s| s.name == "laptop2:fix-flaky-test"));
    let refused = here
        .request(Request::SendText {
            addr: "laptop2:fix-flaky-test".into(),
            text: "x".into(),
            enter: false,
            wait_ready_ms: None,
        })
        .await
        .unwrap_err();
    assert!(refused.message.contains("laptop2 is offline"), "{}", refused.message);

    // Back: it reconnects by itself (the retry key makes it now, not after the backoff).
    t.there.restart().unwrap();
    here.request(Request::MachineRetry { name: "laptop2".into() }).await.unwrap();
    until_state(&here, "laptop2", MachineState::Online).await;

    // Removed: its things leave our lists.
    here.request(Request::MachineRemove { name: "laptop2".into() }).await.unwrap();
    assert!(machines(&here).await.is_empty());
    assert!(!listed(&here).await.iter().any(|s| s.machine.is_some()));
    assert_eq!(workspaces(&here).await, ["main"]);
}

/// Both ways at once: each mirrors only the other's own things, never its own back (no echoes).
#[tokio::test]
async fn two_machines_mirroring_each_other_never_echo() {
    let t = Two::new();
    let there = t.there.client().await.unwrap();
    let here = t.here.client().await.unwrap();
    spawn(&here, shell("one", &t.work, None)).await;
    spawn(&there, shell("two", &t.work, None)).await;
    let add = |target: &str| Request::MachineAdd {
        name: None,
        transport: MachineTransport::Ssh { target: target.into() },
        token: None,
        force: false,
    };
    here.request(add("laptop2")).await.unwrap();
    there.request(add("laptop1")).await.unwrap();
    until_state(&here, "laptop2", MachineState::Online).await;
    until_state(&there, "laptop1", MachineState::Online).await;
    let names = |v: Vec<SessionInfo>| v.into_iter().map(|s| s.name).collect::<Vec<_>>();
    let mine = names(listed(&here).await);
    assert!(mine.contains(&"laptop2:two".to_owned()), "{mine:?}");
    assert!(!mine.iter().any(|n| n.contains("laptop1:")), "nothing of ours comes back: {mine:?}");
    let theirs = names(listed(&there).await);
    assert!(theirs.contains(&"laptop1:one".to_owned()), "{theirs:?}");
    assert!(!theirs.iter().any(|n| n.contains("laptop2:")), "{theirs:?}");
}

/// A worktree of this machine, asked for in a workspace of another's: refused with the way out
/// (it used to be sent to the other machine, which said `no worktree matches "<id>"`).
#[tokio::test]
async fn a_worktree_here_in_a_workspace_there_is_refused_with_the_fix() {
    let t = Two::new();
    let here = t.here.client().await.unwrap();
    let repo = t.work.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    here.request(Request::ProjectAdd { path: repo, workspace: None }).await.unwrap();
    let root = match here.request(Request::WorktreeList { project: None }).await.unwrap() {
        Reply::Worktrees { worktrees } => worktrees.into_iter().find(|w| w.is_root).unwrap(),
        other => panic!("{other:?}"),
    };
    here.request(Request::MachineAdd {
        name: None,
        transport: MachineTransport::Ssh { target: "laptop2".into() },
        token: None,
        force: false,
    })
    .await
    .unwrap();
    until_state(&here, "laptop2", MachineState::Online).await;
    let mut spec = shell("x", &t.work, Some("laptop2:main"));
    spec.worktree = Some(root.id);
    let err = here.request(Request::Spawn { spec }).await.unwrap_err();
    assert!(err.message.contains("is on this machine"), "{}", err.message);
}
