//! KB1.12 (SPEC §25.11): the knowledge base across linked machines. Two daemons on this
//! computer stand for two laptops, linked over a fake ssh as in `tests/chat_machines.rs`; their
//! agents are the fakes, keeping transcripts as the real ones do. Each machine keeps its own
//! `kb.db`. A search from laptop1 finds laptop2's sessions beside its own, each hit under its
//! machine's name; a hit there is read, listed and forgotten there; an agent's `recall` reaches
//! laptop2 only when it asks and only within its profile; and a machine that is down is named
//! in the answer instead of failing the search.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::kb::Writer;
use ptyro_core::kb::search::{HitKind, KbHit, KbListed};
use ptyro_core::model::{MachineState, MachineTransport};
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_core::{ErrorCode, TermSize};
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

fn fake_ssh(dir: &Path, name: &str, env: &Path) -> PathBuf {
    ptyro_testkit::program::program(
        dir,
        name,
        &format!("exec env $(cat '{}') PTYRO_NO_AUTOSPAWN=1 '{BIN}' proxy", env.display()),
    )
}

fn write_env(path: &Path, d: &TempDaemon) {
    let line: Vec<String> = d.env().into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    std::fs::write(path, line.join(" ")).unwrap();
}

/// Both machines have the profiles `work` and `home`, and fake agents that keep transcripts.
fn config(machine: &str) -> String {
    let mut c = format!(
        "[daemon]\nmachine_name = \"{machine}\"\n[profiles.work]\nclaude = \"~/.claude-work\"\n[profiles.home]\nclaude = \"~/.claude-home\"\n[chat]\ntitle_command = [\"sh\", \"-c\", \"cat >/dev/null; echo {machine} title\"]\n"
    );
    for a in ["claude", "codex"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"{a}\"]\nenv = {{ PTYRO_FAKE_TRANSCRIPT = \"1\", PTYRO_FAKE_TURNS = \"1\" }}\n"
        ));
    }
    c
}

/// One laptop: its daemon and the home its agents keep their transcripts in.
struct Laptop {
    d: TempDaemon,
    home: PathBuf,
}

impl Laptop {
    fn new(root: &Path, name: &str, ssh: &str) -> Self {
        let home = root.join(format!("{name}-home"));
        std::fs::create_dir_all(&home).unwrap();
        let hs = home.to_string_lossy().into_owned();
        let env = [("PTYRO_SSH", ssh), ("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        Self { d: TempDaemon::start_with(BIN, &env, Some(&config(name))).unwrap(), home }
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", &self.home).current_dir(&self.home).output().unwrap()
    }

    fn out(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    }

    /// An agent session under `profile` that said `line`, its turn over and kept.
    async fn said(&self, c: &Client, name: &str, profile: &str, line: &str) -> SessionInfo {
        let info = self.agent(c, name, profile).await;
        let text = |text: &str| Request::SendText {
            addr: name.into(),
            text: text.into(),
            enter: true,
            wait_ready_ms: Some(15_000),
            once: None,
        };
        c.request(text(line)).await.unwrap();
        // Kept, its turn over: this machine's own search finds a word of it, on its card.
        let word = line.split(' ').next_back().unwrap().to_owned();
        until(&format!("{name}'s turn is kept"), || async {
            search(c, &word, false, None).await.is_ok_and(|(hits, _)| {
                hits.iter().any(|h| h.snippet.contains("Outcome") && h.item_session(&info))
            })
        })
        .await;
        info
    }

    async fn agent(&self, c: &Client, name: &str, profile: &str) -> SessionInfo {
        let spec = SpawnSpec {
            agent: Some("claude".into()),
            profile: Some(profile.into()),
            cwd: self.home.clone(),
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

    /// The session's agent calls a tool of the `ptyro` MCP server its launch gave it.
    async fn mcp(&self, c: &Client, name: &str, tool: &str, args: serde_json::Value) -> String {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let file = self.home.join(format!("mcp-{n}.txt"));
        let req = Request::SendText {
            addr: name.into(),
            text: format!("mcp {} {tool} {args}", file.display()),
            enter: true,
            wait_ready_ms: Some(15_000),
            once: None,
        };
        c.request(req).await.unwrap();
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

trait OfSession {
    fn item_session(&self, s: &SessionInfo) -> bool;
}

impl OfSession for KbHit {
    /// The hit is of that session's conversation.
    fn item_session(&self, s: &SessionInfo) -> bool {
        let c = s.conversation.as_deref().unwrap_or("-");
        self.uri.as_deref().is_some_and(|u| u.ends_with(c))
    }
}

async fn until<F: std::future::Future<Output = bool>>(what: &str, f: impl Fn() -> F) {
    let deadline = Instant::now() + WAIT;
    while !f().await {
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn online(c: &Client, want: MachineState) {
    until("laptop2's link state", || async {
        match c.request(Request::MachineList).await {
            Ok(Reply::Machines { machines }) => {
                machines.iter().any(|m| m.name == "laptop2" && m.state == want)
            }
            _ => false,
        }
    })
    .await;
}

/// A search: the hits, and who was left out.
async fn search(
    c: &Client,
    query: &str,
    machines: bool,
    machine: Option<&str>,
) -> Result<(Vec<KbHit>, Vec<String>), ptyro_core::Error> {
    let req = Request::KbSearch {
        query: query.into(),
        project: None,
        kinds: Vec::new(),
        since_ms: None,
        before_ms: None,
        limit: None,
        here: None,
        machines,
        machine: machine.map(Into::into),
        profile: None,
        session: None,
        worktree: None,
    };
    match c.request(req).await? {
        Reply::KbHits { hits, left_out, .. } => Ok((hits, left_out)),
        other => panic!("{other:?}"),
    }
}

/// Which machines' hits an answer holds: `here`, or the machine's name, each once, sorted.
fn machines_of(hits: &[KbHit]) -> Vec<String> {
    let mut out: Vec<String> =
        hits.iter().map(|h| h.machine.clone().unwrap_or_else(|| "here".into())).collect();
    out.sort();
    out.dedup();
    out
}

async fn read(c: &Client, addr: &str) -> Result<(KbListed, String), ptyro_core::Error> {
    let req = Request::KbRead { addr: addr.into(), before: None, after: None, profile: None };
    match c.request(req).await? {
        Reply::KbDoc { item, chunks, .. } => {
            Ok((item, chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n")))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_finds_another_machines_sessions_reads_them_there_and_keeps_the_wall() {
    let dir = tempfile::tempdir().unwrap();
    let root = ptyro_core::os::canonicalize(dir.path()).unwrap();
    let (env1, env2) = (root.join("laptop1.env"), root.join("laptop2.env"));
    let to1 = fake_ssh(&root, "ssh-to-laptop1", &env1).to_string_lossy().into_owned();
    let to2 = fake_ssh(&root, "ssh-to-laptop2", &env2).to_string_lossy().into_owned();
    let mut far = Laptop::new(&root, "laptop2", &to1);
    let near = Laptop::new(&root, "laptop1", &to2);
    write_env(&env1, &near.d);
    write_env(&env2, &far.d);
    let there = far.d.client().await.unwrap();
    let here = near.d.client().await.unwrap();

    // Each machine keeps its own: a work and a home session on laptop2, a work one here.
    let fw = far.said(&there, "fw", "work", "edit far.txt zqfarword").await;
    let fh = far.said(&there, "fh", "home", "edit farhome.txt zqfarword").await;
    let nw = near.said(&here, "nw", "work", "edit near.txt zqfarword").await;
    // Not linked yet: this machine's only, and nobody is left out.
    let (hits, left_out) = search(&here, "zqfarword", true, None).await.unwrap();
    assert_eq!((machines_of(&hits), left_out), (vec!["here".to_owned()], Vec::<String>::new()));

    here.request(Request::MachineAdd {
        name: None,
        transport: MachineTransport::Ssh { target: "laptop2".into() },
        token: None,
        force: false,
    })
    .await
    .unwrap();
    online(&here, MachineState::Online).await;

    // You search every machine: laptop2's hits under its name, its addresses under it too.
    let (hits, left_out) = search(&here, "zqfarword", true, None).await.unwrap();
    assert_eq!(machines_of(&hits), ["here", "laptop2"], "{hits:#?}");
    assert!(left_out.is_empty(), "{left_out:?}");
    for h in &hits {
        match &h.machine {
            Some(m) => assert!(h.addr.starts_with(&format!("{m}:k:")), "{}", h.addr),
            None => assert!(h.addr.starts_with("k:") && h.item_session(&nw), "{}", h.addr),
        }
    }
    for s in [&fw, &fh] {
        assert!(hits.iter().any(|h| h.machine.is_some() && h.item_session(s)), "{hits:#?}");
    }
    // Interleaved: the first two hits are one of each machine's.
    assert_eq!(machines_of(&hits[..2]), ["here", "laptop2"]);
    // Not asked to: this machine only. One machine by name, by the option and in the query.
    let only = |r: Result<(Vec<KbHit>, Vec<String>), ptyro_core::Error>| machines_of(&r.unwrap().0);
    assert_eq!(only(search(&here, "zqfarword", false, None).await), ["here"]);
    assert_eq!(only(search(&here, "zqfarword", true, Some("here")).await), ["here"]);
    assert_eq!(only(search(&here, "zqfarword", true, Some("laptop1")).await), ["here"]);
    assert_eq!(only(search(&here, "zqfarword", false, Some("laptop2")).await), ["laptop2"]);
    assert_eq!(only(search(&here, "zqfarword machine:laptop2", false, None).await), ["laptop2"]);
    let e = search(&here, "zqfarword machine:laptop9", true, None).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("say one of here, laptop2"), "{}", e.message);
    // Nothing of laptop1's is on laptop2 (links are one way, and nothing is copied).
    assert_eq!(only(search(&there, "zqfarword", true, None).await), ["here"]);

    // A hit there is read there, and its history is listed from here.
    let far_work = hits.iter().find(|h| h.item_session(&fw)).unwrap();
    let (item, text) = read(&here, &far_work.addr).await.unwrap();
    assert!(item.addr.starts_with("laptop2:k:") && far_work.addr.starts_with(&item.addr));
    assert!(text.contains("edit far.txt zqfarword"), "{text}");
    assert_eq!(item.item.session.as_deref(), Some(fw.id.full().as_str()));
    let recent = Request::KbRecent {
        kinds: Vec::new(),
        project: None,
        session: None,
        worktree: None,
        since_ms: None,
        before_ms: None,
        limit: None,
        machine: Some("laptop2".into()),
        writer: None,
    };
    let Reply::KbItems { items } = here.request(recent).await.unwrap() else { panic!() };
    assert_eq!(items.len(), 2, "{items:#?}");
    assert!(items.iter().all(|i| i.addr.starts_with("laptop2:k:")));

    // The commands: every machine unless told, the machine on the line, and `kb show` there.
    let all = near.out(&["recall", "zqfarword"]);
    assert!(
        all.lines().any(|l| l.starts_with("laptop2:k:") && l.contains(" · laptop2 · ")),
        "{all}"
    );
    assert!(all.lines().any(|l| l.starts_with("k:")), "{all}");
    let local = near.out(&["recall", "zqfarword", "--here"]);
    assert!(!local.contains("laptop2") && local.lines().any(|l| l.starts_with("k:")), "{local}");
    let theirs = near.out(&["recall", "--machine", "laptop2", "zqfarword"]);
    assert!(
        theirs.lines().all(|l| !l.starts_with("k:")) && theirs.contains("laptop2:k:"),
        "{theirs}"
    );
    let shown = near.out(&["kb", "show", &far_work.addr]);
    assert!(shown.starts_with(&item.addr) && shown.contains("edit far.txt zqfarword"), "{shown}");
    let listed = near.out(&["kb", "recent", "--machine", "laptop2"]);
    assert_eq!(listed.matches("laptop2:k:").count(), 2, "{listed}");

    // The wall holds across the link: a `home` session's tool finds laptop2's `home` session
    // only when it asks for other machines, and never anything of `work`, here or there.
    near.agent(&here, "nh", "home").await;
    let q = |machines: bool| serde_json::json!({ "query": "zqfarword", "machines": machines });
    let said = near.mcp(&here, "nh", "recall", q(false)).await;
    assert!(said.contains("nothing found"), "{said}");
    let said = near.mcp(&here, "nh", "recall", q(true)).await;
    let lines: Vec<&str> = said.lines().filter(|l| l.contains("k:")).collect();
    assert!(!lines.is_empty() && lines.iter().all(|l| l.starts_with("laptop2:k:")), "{said}");
    assert!(said.contains("farhome.txt") && !said.contains("far.txt zq"), "{said}");
    assert!(!said.contains("near.txt"), "{said}");
    let far_home = hits.iter().find(|h| h.item_session(&fh)).unwrap();
    let ask = |addr: &str| serde_json::json!({ "ref": addr });
    let said = near.mcp(&here, "nh", "recall_read", ask(&far_home.addr)).await;
    assert!(said.contains("edit farhome.txt zqfarword"), "{said}");
    let said = near.mcp(&here, "nh", "recall_read", ask(&far_work.addr)).await;
    assert!(said.starts_with("error: ") && !said.contains("zqfarword"), "{said}");

    // Forgetting one there: asked first in laptop2's words, then gone from its file.
    let forget = |confirmed| Request::KbForget {
        addrs: vec![item.addr.clone()],
        query: None,
        confirmed,
        session: None,
    };
    let e = here.request(forget(false)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(e.message.starts_with("Forget 1 item: session"), "{}", e.message);
    let Reply::KbForgotten { items, .. } = here.request(forget(true)).await.unwrap() else {
        panic!()
    };
    assert_eq!(items, 1);
    let (hits, _) = search(&there, "zqfarword", false, None).await.unwrap();
    assert!(hits.iter().all(|h| !h.item_session(&fw)) && !hits.is_empty(), "{hits:#?}");
    assert!(read(&here, &far_work.addr).await.is_err());

    // Notes (KB2) are each machine's own: one written on laptop2 is found from here as
    // anything there is, read there, behind the same wall, and forgotten there, which deletes
    // its file on that machine.
    let o = far.cli(&["kb", "note", "zqfarnote lives on laptop2", "--profile", "work"]);
    assert!(o.status.success(), "{o:?}");
    let file = far.d.home.path().join("data/kb/notes/work/_global/zqfarnote-lives-on-laptop2.md");
    assert!(file.exists());
    let (hits, _) = search(&here, "zqfarnote", true, None).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:#?}");
    let note = &hits[0];
    assert_eq!(
        (note.kind, note.machine.as_deref(), note.writer),
        (HitKind::Note, Some("laptop2"), Some(Writer::User))
    );
    let (item, text) = read(&here, &note.addr).await.unwrap();
    assert!(text.contains("zqfarnote lives on laptop2"), "{text}");
    let q = serde_json::json!({ "query": "zqfarnote", "machines": true });
    let said = near.mcp(&here, "nh", "recall", q).await;
    assert!(said.contains("nothing found"), "another profile's, there too: {said}");
    let o = near.cli(&["kb", "forget", &item.addr, "--yes"]);
    assert!(o.status.success(), "{o:?}");
    assert!(!file.exists(), "forgotten there: its file is deleted on laptop2");
    assert!(search(&here, "zqfarnote", true, None).await.unwrap().0.is_empty());

    // Roots (KB3.7) are each machine's own too: asked of laptop2 by its name, a folder on
    // its disk is read there, listed and found from here, looked at again and no longer
    // read there. Nothing of it is on this machine.
    let book = far.home.join("handbook");
    std::fs::create_dir_all(&book).unwrap();
    std::fs::write(book.join("Guide.md"), "# Guide\n\nThe zqfarbook is on laptop2.\n").unwrap();
    let said = near.out(&["ingest", &book.to_string_lossy(), "--machine", "laptop2"]);
    assert!(said.contains("read handbook: 1 file"), "{said}");
    let there_ls = near.out(&["kb", "ls", "--machine", "laptop2"]);
    assert!(
        there_ls.lines().any(|l| l.starts_with("handbook") && l.contains("folder")),
        "{there_ls}"
    );
    assert!(!near.out(&["kb", "ls"]).contains("handbook"));
    assert!(far.out(&["kb", "ls"]).contains("handbook"));
    let (hits, _) = search(&here, "zqfarbook", true, None).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:#?}");
    assert_eq!((hits[0].kind, hits[0].machine.as_deref()), (HitKind::Doc, Some("laptop2")));
    let said = near.out(&["kb", "sync", "handbook", "--machine", "laptop2"]);
    assert!(said.contains("handbook: 1 file"), "{said}");
    let e = here.request(Request::KbRoots { machine: Some("laptop9".into()) }).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
    // Its machine asks first, in its words; then it is gone there.
    let ask = |confirmed| Request::KbRemove {
        root: "handbook".into(),
        confirmed,
        machine: Some("laptop2".into()),
    };
    let e = here.request(ask(false)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation, "{e:?}");
    assert!(e.message.starts_with("Stop reading handbook"), "{}", e.message);
    let said = near.out(&["kb", "rm", "handbook", "--machine", "laptop2", "--yes"]);
    assert!(said.contains("handbook is no longer read"), "{said}");
    assert!(!far.out(&["kb", "ls"]).contains("handbook"));
    assert!(search(&here, "zqfarbook", true, None).await.unwrap().0.is_empty());

    // laptop2 goes down: the search is still an answer, and names it; asked for by name, it
    // is the error.
    far.d.stop();
    online(&here, MachineState::Offline).await;
    let (hits, left_out) = search(&here, "zqfarword", true, None).await.unwrap();
    assert_eq!(machines_of(&hits), ["here"]);
    assert_eq!(left_out.len(), 1);
    assert!(left_out[0].starts_with("laptop2 is offline"), "{left_out:?}");
    let e = search(&here, "zqfarword", true, Some("laptop2")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DaemonUnavailable);
    let said = near.cli(&["recall", "zqfarword"]);
    assert!(said.status.success());
    let err = String::from_utf8_lossy(&said.stderr);
    assert!(err.contains("(not searched: laptop2 is offline"), "{err}");
    assert!(String::from_utf8_lossy(&said.stdout).contains("k:"));
}
