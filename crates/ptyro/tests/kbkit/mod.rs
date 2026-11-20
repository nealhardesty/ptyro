//! What the knowledge base's end-to-end tests share (`kb1.rs`, `kb2.rs`): a daemon in a home
//! of its own with fake agents that keep transcripts as the real ones do, and the ways to
//! drive it: the commands, an agent's session and its turns, the tools the session's agent
//! was given (`mcp`), `kb.db` read beside the daemon, and the TUI in the headless emulator.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::kb::{ChunkKind, ItemKind, KbChunk, KbItem};
use ptyro_core::session::{ProcessState, SessionInfo, SpawnSpec};
use ptyro_core::status::AgentStatus;
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::{ItemFilter, KbDb};
use ptyro_testkit::TempDaemon;

pub const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

pub struct World {
    pub home: tempfile::TempDir,
    pub d: TempDaemon,
}

impl World {
    pub fn new(config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-kb1-").tempdir().unwrap();
        let hs = ptyro_core::os::canonicalize(home.path()).unwrap().to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(config)).unwrap();
        Self { home, d }
    }

    pub fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    pub fn out(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    }

    pub fn kb_db(&self) -> PathBuf {
        self.d.home.path().join("data/kb.db")
    }

    /// `ptyro doctor`'s line about the knowledge base.
    pub fn doctor_line(&self) -> String {
        let all = self.out(&["doctor", "--quick"]);
        all.lines()
            .find(|l| l.contains("knowledge base"))
            .unwrap_or_else(|| panic!("no knowledge base line in:\n{all}"))
            .to_owned()
    }
}

pub const WAIT: Duration = Duration::from_secs(20);

/// Every agent is the fake one, keeping a transcript; `env` is more of its environment.
pub fn agents(env: &str) -> String {
    let mut c = String::from(
        "[chat]\ntitle_command = [\"sh\", \"-c\", \"cat >/dev/null; echo '\\\"A fake title.\\\"'\"]\n",
    );
    for a in ["claude", "codex", "opencode"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\n\
             env = {{ PTYRO_FAKE_TRANSCRIPT = \"1\", PTYRO_FAKE_TURNS = \"1\"{env} }}\n"
        ));
    }
    c
}

/// A session's item and its chunks, as `kb.db` holds them now.
pub struct Kept {
    pub item: KbItem,
    pub chunks: Vec<KbChunk>,
}

impl Kept {
    /// Each chunk as `kind| text` (its first line).
    pub fn shown(&self) -> Vec<String> {
        self.chunks
            .iter()
            .map(|c| format!("{}| {}", c.kind, c.text.lines().next().unwrap_or("")))
            .collect()
    }

    pub fn card(&self) -> &str {
        assert_eq!(self.chunks[0].kind, ChunkKind::Card, "the card is the first chunk");
        assert_eq!(self.chunks.iter().filter(|c| c.kind == ChunkKind::Card).count(), 1);
        &self.chunks[0].text
    }

    pub fn has(&self, kind: ChunkKind, text: &str) -> bool {
        self.chunks.iter().any(|c| c.kind == kind && c.text == text)
    }
}

impl World {
    /// `kb.db`, read beside the daemon (which is its only writer). Opened for each look and
    /// closed after it: this process also reads the file's bytes (`bytes_hold`), and closing
    /// any handle of a file gives up the locks a connection in the same process holds on it.
    pub fn db(&self) -> KbDb {
        KbDb::open(&self.kb_db(), None).unwrap()
    }

    pub async fn sessions(&self) -> Vec<KbItem> {
        let f = ItemFilter { kinds: vec![ItemKind::Session], ..ItemFilter::default() };
        self.db().list(f).await.unwrap()
    }

    /// Wait until the item at `uri` is there and `pred` holds of it.
    pub async fn kept(&self, uri: &str, what: &str, pred: impl Fn(&Kept) -> bool) -> Kept {
        let deadline = Instant::now() + WAIT;
        loop {
            let db = self.db();
            let found = match db.item_by_uri(uri.to_owned()).await.unwrap() {
                Some(item) => {
                    let chunks = db.chunks(item.id.clone(), 0, 0).await.unwrap();
                    Some(Kept { item, chunks })
                }
                None => None,
            };
            drop(db);
            match found {
                Some(k) if pred(&k) => return k,
                found => assert!(
                    Instant::now() < deadline,
                    "{uri}: never {what}; it holds {:?}; the daemon said:\n{}",
                    found.map(|k| k.shown()),
                    self.log("knowledge base")
                ),
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    pub async fn agent(&self, c: &Client, name: &str, agent: &str) -> SessionInfo {
        self.agent_in(c, name, agent, None).await
    }

    /// An agent session under `profile` (`None`: the default one).
    pub async fn agent_in(
        &self,
        c: &Client,
        name: &str,
        agent: &str,
        profile: Option<&str>,
    ) -> SessionInfo {
        let spec = SpawnSpec {
            agent: Some(agent.into()),
            profile: profile.map(Into::into),
            cwd: self.home(),
            name: Some(name.into()),
            size: Some(TermSize::new(100, 20)),
            ..SpawnSpec::default()
        };
        let info = match c.request(Request::Spawn { spec }).await.expect("spawn") {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        self.d.wait_for_text(c, name, "resumed=", WAIT).await.unwrap();
        info
    }

    /// Type a line into the session and wait until its turn is over.
    pub async fn turn(&self, c: &Client, name: &str, line: &str) {
        let before = self.info(c, name).await.turns;
        self.type_line(c, name, line).await;
        let deadline = Instant::now() + WAIT;
        while self.info(c, name).await.turns == before {
            assert!(Instant::now() < deadline, "{name}: {line:?} never ended its turn");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    /// The daemon's log lines that hold `word`.
    pub fn log(&self, word: &str) -> String {
        let mut lines = Vec::new();
        for e in std::fs::read_dir(self.d.paths.log_dir()).into_iter().flatten().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            lines.extend(text.lines().filter(|l| l.contains(word)).map(str::to_owned));
        }
        lines.join("\n")
    }

    /// Type a line into the session, once its agent takes input.
    pub async fn type_line(&self, c: &Client, name: &str, line: &str) {
        c.request(Request::SendText {
            addr: name.into(),
            text: line.into(),
            enter: true,
            wait_ready_ms: Some(15_000),
            once: None,
        })
        .await
        .unwrap();
    }

    /// Hibernate the session and bring it back: the same conversation, a new process.
    pub async fn resume(&self, c: &Client, name: &str) {
        self.out(&["hibernate", name]);
        self.dormant(c, name).await;
        self.wake(c, name).await;
    }

    pub async fn dormant(&self, c: &Client, name: &str) {
        let deadline = Instant::now() + WAIT;
        while self.info(c, name).await.state != ProcessState::Dormant {
            assert!(Instant::now() < deadline, "{name}: never dormant");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    /// Resume a dormant session and wait until its agent has started.
    pub async fn wake(&self, c: &Client, name: &str) {
        self.out(&["resume", name]);
        let deadline = Instant::now() + WAIT;
        loop {
            let i = self.info(c, name).await;
            if i.state == ProcessState::Running
                && i.status.as_ref().is_some_and(|r| r.status == AgentStatus::Idle)
            {
                return;
            }
            assert!(Instant::now() < deadline, "{name}: never back; {:?} {:?}", i.state, i.status);
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    pub async fn info(&self, c: &Client, name: &str) -> SessionInfo {
        match c.request(Request::Get { addr: name.into() }).await.unwrap() {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        }
    }

    /// Whether any file of the knowledge base holds `needle` (the file, its log, its index).
    pub fn bytes_hold(&self, needle: &str) -> bool {
        std::fs::read_dir(self.d.home.path().join("data"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("kb.db"))
            .any(|e| {
                let bytes = std::fs::read(e.path()).unwrap_or_default();
                bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            })
    }
}

impl World {
    /// A command's output as JSON.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let o = self.cli(args);
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\n{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
    }

    /// Wait until `ptyro kb status` says `pred`.
    pub async fn status(
        &self,
        what: &str,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let s = self.json(&["kb", "status", "--json"]);
            if pred(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "the status never said {what}: {s:#}");
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    }

    /// Wait until the knowledge base says why it did not keep `uri`.
    pub async fn skipped(&self, uri: &str, why: &str) {
        self.status(&format!("{uri} was not kept ({why})"), |s| {
            s["skipped"].as_array().is_some_and(|k| {
                k.iter()
                    .any(|k| k["uri"] == uri && k["why"].as_str().is_some_and(|w| w.contains(why)))
            })
        })
        .await;
    }

    /// An agent session under `profile`, in `cwd`.
    pub async fn agent_at(
        &self,
        c: &Client,
        name: &str,
        agent: &str,
        profile: Option<&str>,
        cwd: PathBuf,
    ) -> SessionInfo {
        let spec = SpawnSpec {
            agent: Some(agent.into()),
            profile: profile.map(Into::into),
            cwd,
            name: Some(name.into()),
            size: Some(TermSize::new(100, 20)),
            ..SpawnSpec::default()
        };
        let info = match c.request(Request::Spawn { spec }).await.expect("spawn") {
            Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        self.d.wait_for_text(c, name, "resumed=", WAIT).await.unwrap();
        info
    }
}

/// The file called `name` somewhere under `dir`.
pub fn find_file(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if e.file_name().to_string_lossy() == name {
            return Some(p);
        }
    }
    None
}

impl World {
    /// The session's agent calls a tool of the `ptyro` MCP server its launch gave it (the fake
    /// agent's `mcp`, which reads the launch's flags as the real agent does): what the tool said.
    pub async fn mcp(&self, c: &Client, name: &str, tool: &str, args: serde_json::Value) -> String {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = self.home().join(format!("mcp-{n}.txt"));
        self.type_line(c, name, &format!("mcp {} {tool} {args}", file.display())).await;
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(said) = std::fs::read_to_string(&file) {
                return said;
            }
            assert!(Instant::now() < deadline, "{name}: {tool} never answered");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

impl World {
    /// The TUI, inside the headless emulator.
    pub fn tui(&self, args: &[&str]) -> ptyro_testkit::headless::Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = ptyro_testkit::headless::Script::new(argv)
            .size(140, 34)
            .timeout(45.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }
}
