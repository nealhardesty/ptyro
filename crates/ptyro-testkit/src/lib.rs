//! Test support (SPEC §18.2): a fake agent, and a harness that runs a real daemon in a temporary
//! `PTYRO_HOME` so integration tests never touch the user's sessions.

pub mod fake_adapters;
pub mod fake_agent;
pub mod fake_gh;
pub mod fake_structured;
pub mod fuzz;
pub mod headless;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ptyro_client::{Client, ConnectOptions};
use ptyro_core::paths::Paths;
use ptyro_protocol::{ClientKind, Reply, Request, ScreenFormat};

/// Variables that would leak the *caller's* ptyro into the test daemon or its sessions.
const ISOLATE: &[&str] =
    &["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_HOME", "PTYRO_LOG", "PTYRO_CONFIG"];

pub struct TempDaemon {
    pub home: tempfile::TempDir,
    pub paths: Paths,
    pub bin: PathBuf,
    child: Option<Child>,
    /// Extra daemon environment, kept for restarts.
    daemon_env: Vec<(String, String)>,
}

impl TempDaemon {
    /// Start `bin daemon start --foreground` in a fresh home and wait until it accepts connections.
    pub fn start(bin: impl Into<PathBuf>) -> Result<Self> {
        Self::start_with_env(bin, &[])
    }

    /// Like [`start`](Self::start), with extra variables in the *daemon's* environment (which
    /// every session's environment is built from).
    pub fn start_with_env(bin: impl Into<PathBuf>, env: &[(&str, &str)]) -> Result<Self> {
        Self::start_with(bin, env, None)
    }

    /// Like [`start_with_env`](Self::start_with_env), with a user config in place before the
    /// daemon starts.
    pub fn start_with(
        bin: impl Into<PathBuf>,
        env: &[(&str, &str)],
        config: Option<&str>,
    ) -> Result<Self> {
        let home = tempfile::Builder::new().prefix("ptyro-test-").tempdir()?;
        let bin = bin.into();
        let paths = paths_for(home.path())?;
        let mut me = Self {
            home,
            paths,
            bin,
            child: None,
            daemon_env: env.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
        };
        if let Some(c) = config {
            me.write_config(c)?;
        }
        me.spawn_daemon()?;
        Ok(me)
    }

    fn spawn_daemon(&mut self) -> Result<()> {
        let mut cmd = Command::new(&self.bin);
        isolate(&mut cmd, self.home.path(), &self.bin);
        cmd.envs(self.daemon_env.iter().map(|(k, v)| (k, v)));
        let child = cmd
            .args(["daemon", "start", "--foreground"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("start {}", self.bin.display()))?;
        self.child = Some(child);
        self.wait_for_socket()
    }

    /// Stop the daemon and start a fresh one on the same home (store, config and views persist).
    pub fn restart(&mut self) -> Result<()> {
        self.stop();
        self.spawn_daemon()
    }

    /// Variables that point a program (e.g. the TUI under test) at this daemon.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("PTYRO_HOME".into(), self.home.path().display().to_string()),
            ("PTYRO_BIN".into(), self.bin.display().to_string()),
            ("PTYRO_SOCKET".into(), self.paths.socket.display().to_string()),
            ("PTYRO_ALLOW_NESTED".into(), "1".into()),
        ]
    }

    /// Write the user config file (the daemon hot-reloads it).
    pub fn write_config(&self, text: &str) -> Result<()> {
        std::fs::create_dir_all(&self.paths.config)?;
        Ok(std::fs::write(self.paths.config.join("config.toml"), text)?)
    }

    fn wait_for_socket(&self) -> Result<()> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if std::os::unix::net::UnixStream::connect(&self.paths.socket).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        bail!("test daemon did not start (socket {})", self.paths.socket.display())
    }

    /// A `ptyro` command bound to this daemon. It never starts a daemon of its own: if the test
    /// daemon died, the command fails instead of quietly bringing up an untracked one.
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        isolate(&mut cmd, self.home.path(), &self.bin);
        cmd.env("PTYRO_NO_AUTOSPAWN", "1");
        cmd
    }

    pub async fn client(&self) -> Result<Client> {
        let mut opts = ConnectOptions::new(self.paths.clone(), ClientKind::Test);
        opts.autospawn = false;
        Ok(Client::connect(&opts).await?)
    }

    /// Poll the session's screen until it contains `needle`; returns the screen.
    pub async fn wait_for_text(
        &self,
        client: &Client,
        addr: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<String> {
        let start = Instant::now();
        let mut last = String::new();
        while start.elapsed() < timeout {
            let reply = client
                .request(Request::ReadScreen { addr: addr.into(), format: ScreenFormat::Plain })
                .await?;
            if let Reply::Screen { text } = reply {
                if text.contains(needle) {
                    return Ok(text);
                }
                last = text;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        bail!("timed out waiting for {needle:?}; screen was:\n{last}")
    }

    /// Stop the daemon via SIGTERM and wait for it (also done on drop).
    pub fn stop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(child.id() as i32).expect("pid"),
            rustix::process::Signal::TERM,
        );
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Kill the daemon with SIGKILL, leaving its socket file behind (a crash).
    pub fn crash(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Whether the daemon process is still running.
    pub fn is_running(&mut self) -> bool {
        self.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
}

impl Drop for TempDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn paths_for(home: &Path) -> Result<Paths> {
    let home = home.to_path_buf();
    Ok(Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone()))?)
}

fn isolate(cmd: &mut Command, home: &Path, bin: &Path) {
    for k in ISOLATE {
        cmd.env_remove(k);
    }
    cmd.env("PTYRO_HOME", home).env("PTYRO_BIN", bin);
}
