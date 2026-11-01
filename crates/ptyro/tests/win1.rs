//! ROADMAP WIN1: shell sessions get ptyro's shell integration with no setup, after the user's own
//! startup files, in bash, zsh, fish and PowerShell; a bare shell is a shell session; and it can be
//! switched off. Real shells: fish and pwsh are checked when installed, and said to be skipped
//! when not.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::session::{SessionInfo, SessionKind, SpawnSpec};
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

fn which(prog: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(prog))
        .find(|p| p.is_file())
}

/// A home whose startup files each leave a mark, so the test sees they still ran.
fn home_with_rc_files() -> tempfile::TempDir {
    let home = tempfile::Builder::new().prefix("ptyro-win1-home-").tempdir().unwrap();
    let h = home.path();
    let w = |rel: &str, text: &str| {
        let p = h.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(".bash_profile", "export RC_BASH=login\n");
    w(".bashrc", "export RC_BASH=rc\n");
    w(".zshenv", "export RC_ZSHENV=1\n");
    w(".zshrc", "export RC_ZSH=1\nHISTFILE_SEEN=$HISTFILE\n");
    w(".config/fish/config.fish", "set -gx RC_FISH 1\n");
    w(
        ".config/powershell/Microsoft.PowerShell_profile.ps1",
        "$env:RC_PWSH = '1'\nfunction global:prompt { \"MYPROMPT[$?]> \" }\n",
    );
    home
}

async fn spawn(c: &Client, name: &str, argv: &[&str], cwd: &Path) -> SessionInfo {
    let spec = SpawnSpec {
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd: cwd.to_owned(),
        name: Some(name.into()),
        size: Some(TermSize::new(120, 30)),
        ..SpawnSpec::default()
    };
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn get(c: &Client, name: &str) -> SessionInfo {
    match c.request(Request::Get { addr: name.into() }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn send(c: &Client, name: &str, text: &str) {
    let req = Request::SendText {
        addr: name.into(),
        text: text.into(),
        enter: true,
        wait_ready_ms: None,
    };
    c.request(req).await.unwrap();
}

async fn wait_for(
    c: &Client,
    name: &str,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let s = get(c, name).await;
        if pred(&s) {
            return s;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: never {what}; last: cwd {:?} exit {:?} command {:?} status {:?}",
            s.live_cwd,
            s.last_command_exit,
            s.command,
            s.status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The checks every integrated shell passes: its prompt marks arrive (a command's line and exit
/// code), it reports where it is, and its own startup file ran (`marker`, echoed by `show`).
async fn integrated(
    d: &TempDaemon,
    c: &Client,
    name: &str,
    argv: &[&str],
    show: &str,
    marker: &str,
) {
    let dir = tempfile::Builder::new().prefix("ptyro-win1-cwd-").tempdir().unwrap();
    let start = dir.path().canonicalize().unwrap();
    let sub = start.join("sub dir");
    std::fs::create_dir(&sub).unwrap();
    let info = spawn(c, name, argv, &start).await;
    assert_eq!(info.kind, SessionKind::Shell, "{name}: a bare shell is a shell session");
    // The first prompt reports the directory: the integration is in.
    wait_for(c, name, "reporting its directory", |s| {
        s.live_cwd.as_deref() == Some(start.as_path())
    })
    .await;
    // The prompt is drawn a moment before the line editor reads keys (PowerShell's PSReadLine);
    // typed before that, Enter arrives as a line feed, which it doesn't run.
    tokio::time::sleep(Duration::from_millis(700)).await;
    send(c, name, show).await;
    d.wait_for_text(c, name, marker, WAIT)
        .await
        .unwrap_or_else(|e| panic!("{name}: its own startup file: {e}"));
    send(c, name, "sh -c 'exit 3'").await;
    let s = wait_for(c, name, "a failed command", |s| s.last_command_exit == Some(3)).await;
    let text = s.command.and_then(|c| c.text).unwrap_or_default();
    assert_eq!(text, "sh -c 'exit 3'", "{name}: the command line came along");
    send(c, name, "cd 'sub dir'").await;
    wait_for(c, name, "following cd", |s| s.live_cwd.as_deref() == Some(sub.as_path())).await;
    wait_for(c, name, "a command that succeeded", |s| s.last_command_exit == Some(0)).await;
}

#[tokio::test]
async fn shells_get_their_integration_with_no_setup() {
    let home = home_with_rc_files();
    let h = home.path().to_str().unwrap();
    let d = TempDaemon::start_with_env(BIN, &[("HOME", h), ("SHELL", "/bin/bash")]).unwrap();
    let c = d.client().await.unwrap();

    // bash as a login shell reads ~/.bash_profile (not ~/.bashrc), as it would by itself.
    integrated(&d, &c, "bash", &["bash", "-l"], "echo bash=$RC_BASH", "bash=login").await;
    // And a non-login one reads ~/.bashrc.
    integrated(&d, &c, "bash2", &["bash"], "echo bash=$RC_BASH", "bash=rc").await;

    // zsh reads the user's files from their ZDOTDIR, and hands it back: ZDOTDIR unset as it was,
    // and the history where it belongs (macOS's /etc/zshrc sets HISTFILE from ZDOTDIR).
    integrated(&d, &c, "zsh", &["zsh", "-l"], "echo zsh=$RC_ZSHENV$RC_ZSH", "zsh=11").await;
    send(&c, "zsh", "echo zdotdir=${ZDOTDIR-unset} hist=${HISTFILE_SEEN:-none}").await;
    let screen = d.wait_for_text(&c, "zsh", "zdotdir=unset", WAIT).await.unwrap();
    assert!(!screen.contains("integrations/shell/zsh/.zsh_history"), "{screen}");

    if which("fish").is_some() {
        integrated(&d, &c, "fish", &["fish", "-l"], "echo fish=$RC_FISH", "fish=1").await;
    } else {
        eprintln!("win1: fish not installed, its integration not checked");
    }

    if which("pwsh").is_some() {
        // `ptyro new -- pwsh`: a shell session, the user's prompt kept (and still told `$?`).
        integrated(&d, &c, "pwsh", &["pwsh", "-Login"], "echo pwsh=$env:RC_PWSH", "pwsh=1").await;
        send(&c, "pwsh", "Get-Item /nonexistent -ErrorAction SilentlyContinue").await;
        d.wait_for_text(&c, "pwsh", "MYPROMPT[False]>", WAIT).await.unwrap();
        // A cmdlet that failed is exit status 1.
        wait_for(&c, "pwsh", "a failed cmdlet", |s| s.last_command_exit == Some(1)).await;
    } else {
        eprintln!("win1: pwsh not installed, PowerShell's integration not checked");
    }
}

#[tokio::test]
async fn shell_command_and_switching_the_integration_off() {
    let home = home_with_rc_files();
    let h = home.path().to_str().unwrap();
    // `[shell] command` picks what a new shell session runs.
    let config = "[shell]\ncommand = [\"zsh\", \"-l\"]\n";
    let d =
        TempDaemon::start_with(BIN, &[("HOME", h), ("SHELL", "/bin/bash")], Some(config)).unwrap();
    let c = d.client().await.unwrap();
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let info = spawn(&c, "default", &[], &dir).await;
    assert_eq!(info.kind, SessionKind::Shell);
    send(&c, "default", "echo shell=$ZSH_NAME").await;
    d.wait_for_text(&c, "default", "shell=zsh", WAIT).await.unwrap();
    wait_for(&c, "default", "integrated", |s| s.last_command_exit == Some(0)).await;
    // The session keeps what it was asked to run, not how ptyro started it.
    let launch = get(&c, "default").await;
    assert_eq!(launch.argv, vec!["zsh".to_owned(), "-l".to_owned()]);

    // Off: the shell runs as it is, and nothing reports its commands.
    d.write_config("[shell]\nintegration = false\n").unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await; // the daemon reloads the file
    spawn(&c, "plain", &["bash"], &dir).await;
    send(&c, "plain", "echo ready-$((40+2))").await;
    d.wait_for_text(&c, "plain", "ready-42", WAIT).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = get(&c, "plain").await;
    assert_eq!(s.last_command_exit, None, "no marks without the integration");
    assert!(s.command.is_none());
    // A shell with arguments is a command, not a shell session.
    let s = spawn(&c, "cmd", &["bash", "-c", "sleep 5"], &dir).await;
    assert_eq!(s.kind, SessionKind::Command);
}
