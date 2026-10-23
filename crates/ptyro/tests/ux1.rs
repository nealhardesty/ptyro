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
