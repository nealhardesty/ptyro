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
    // It says where it is (its prompt's report, or on Linux the process's directory).
    wait_for(c, name, "reporting its directory", |s| {
        s.live_cwd.as_deref() == Some(start.as_path())
    })
    .await;
    // PowerShell's line editor (PSReadLine) drops the Enter of a line typed while it starts
    // (SPEC §8.7): about half a second after the prompt, and a cold start on a CI runner takes
    // seconds (the directory can come from /proc before any prompt). The line's text survives:
    // press Enter again until it runs. At a prompt that already ran it, Enter is an empty line.
    send(c, name, show).await;
    let deadline = Instant::now() + WAIT;
    loop {
        match d.wait_for_text(c, name, marker, Duration::from_secs(2)).await {
            Ok(_) => break,
            Err(e) if Instant::now() >= deadline => panic!("{name}: its own startup file: {e}"),
            Err(_) => send(c, name, "").await,
        }
    }
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
    // fish and pwsh read $XDG_CONFIG_HOME/… when it is set (GitHub's runners set it).
    let xdg = format!("{h}/.config");
    let d = TempDaemon::start_with_env(
        BIN,
        &[("HOME", h), ("SHELL", "/bin/bash"), ("XDG_CONFIG_HOME", &xdg)],
    )
    .unwrap();
    let c = d.client().await.unwrap();

    // bash as a login shell reads ~/.bash_profile (not ~/.bashrc), as it would by itself.
    integrated(&d, &c, "bash", &["bash", "-l"], "echo bash=$RC_BASH", "bash=login").await;
    // And a non-login one reads ~/.bashrc.
    integrated(&d, &c, "bash2", &["bash"], "echo bash=$RC_BASH", "bash=rc").await;

    // zsh reads the user's files from their ZDOTDIR, and hands it back: ZDOTDIR unset as it was,
    // and the history where it belongs (macOS's /etc/zshrc sets HISTFILE from ZDOTDIR).
    if which("zsh").is_some() {
        integrated(&d, &c, "zsh", &["zsh", "-l"], "echo zsh=$RC_ZSHENV$RC_ZSH", "zsh=11").await;
        send(&c, "zsh", "echo zdotdir=${ZDOTDIR-unset} hist=${HISTFILE_SEEN:-none}").await;
        let screen = d.wait_for_text(&c, "zsh", "zdotdir=unset", WAIT).await.unwrap();
        assert!(!screen.contains("integrations/shell/zsh/.zsh_history"), "{screen}");
    } else {
        eprintln!("win1: zsh not installed, its integration not checked");
    }

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
    // `[shell] command` picks what a new shell session runs (zsh where there is one, so it isn't
    // $SHELL; Linux runners have no zsh).
    let sh = if which("zsh").is_some() { "zsh" } else { "sh" };
    let config = format!("[shell]\ncommand = [\"{sh}\", \"-l\"]\n");
    let d =
        TempDaemon::start_with(BIN, &[("HOME", h), ("SHELL", "/bin/bash")], Some(&config)).unwrap();
    let c = d.client().await.unwrap();
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let info = spawn(&c, "default", &[], &dir).await;
    assert_eq!(info.kind, SessionKind::Shell);
    send(&c, "default", "echo shell=$0").await;
    d.wait_for_text(&c, "default", &format!("shell={sh}"), WAIT).await.unwrap();
    if sh == "zsh" {
        wait_for(&c, "default", "integrated", |s| s.last_command_exit == Some(0)).await;
    }
    // The session keeps what it was asked to run, not how ptyro started it.
    let launch = get(&c, "default").await;
    assert_eq!(launch.argv, vec![sh.to_owned(), "-l".to_owned()]);

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

/// With the integration off, the doctor names the startup file the shell really reads: fish
/// and PowerShell follow `$XDG_CONFIG_HOME` when it is set.
#[test]
fn the_doctor_names_the_startup_file_under_xdg_config_home() {
    let home = tempfile::Builder::new().prefix("ptyro-win1-home-").tempdir().unwrap();
    let xdg = tempfile::Builder::new().prefix("ptyro-win1-xdg-").tempdir().unwrap();
    let (h, x) = (home.path().to_str().unwrap(), xdg.path().to_str().unwrap());
    let profile = xdg.path().join("powershell/Microsoft.PowerShell_profile.ps1");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(&profile, "ptyro shell-init pwsh | Out-String | Invoke-Expression\n").unwrap();
    // Stand-ins, so the doctor finds the shell whether or not this machine has it.
    let bin = tempfile::Builder::new().prefix("ptyro-win1-bin-").tempdir().unwrap();
    for (shell, want) in [
        ("fish", format!("off, and not in {x}/fish/config.fish:")),
        ("pwsh", format!("off; {} sets it up instead", profile.display())),
    ] {
        let exe = bin.path().join(shell);
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let config = format!("[shell]\nintegration = false\ncommand = [\"{}\"]\n", exe.display());
        let d = TempDaemon::start_with(BIN, &[("HOME", h)], Some(&config)).unwrap();
        let out = d
            .cmd()
            .env("HOME", h)
            .env("XDG_CONFIG_HOME", x)
            .args(["doctor", "--quick", "--json"])
            .output()
            .unwrap();
        let doctor: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
        let check = doctor.as_array().unwrap().iter().find(|c| c["name"] == "integration");
        let text = check.and_then(|c| c["text"].as_str()).unwrap_or_default();
        assert!(text.starts_with(&want), "{shell}: {text:?}\n{doctor:#}");
    }
}
