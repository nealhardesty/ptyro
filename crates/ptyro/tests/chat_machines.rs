//! CHAT1.9 (SPEC §22.11): chats in the workspaces of linked machines. Two daemons on this
//! computer stand for two laptops, linked over a fake ssh as in `tests/mm1.rs`; their agents are
//! the fakes. laptop1's clients list laptop2's chats beside their own, open, follow and answer
//! them there, make one in its workspace, hide them with its workspace, and still list them, with
//! a clear refusal, when laptop2 is down.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::chat::ChatBody;
use ptyro_core::chats::ChatInfo;
use ptyro_core::model::{MachineState, MachineTransport};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

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

fn config(machine: &str) -> String {
    let mut c = format!(
        "[daemon]\nmachine_name = \"{machine}\"\n[chat]\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo {machine} chat\"]\n"
    );
    for a in ["claude", "codex"] {
        c.push_str(&format!(
            "[agents.{a}]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"{a}\"]\n"
        ));
    }
    c
}

struct Two {
    here: TempDaemon,
    there: TempDaemon,
    _dir: tempfile::TempDir,
}

impl Two {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (env1, env2) = (root.join("laptop1.env"), root.join("laptop2.env"));
        let to1 = fake_ssh(&root, "ssh-to-laptop1", &env1).to_string_lossy().into_owned();
        let to2 = fake_ssh(&root, "ssh-to-laptop2", &env2).to_string_lossy().into_owned();
        let there =
            TempDaemon::start_with(BIN, &[("PTYRO_SSH", to1.as_str())], Some(&config("laptop2")))
                .unwrap();
        let here =
            TempDaemon::start_with(BIN, &[("PTYRO_SSH", to2.as_str())], Some(&config("laptop1")))
                .unwrap();
        write_env(&env1, &here);
        write_env(&env2, &there);
        Self { here, there, _dir: dir }
    }
}

async fn chats(c: &Client) -> Vec<ChatInfo> {
    let req = Request::ChatList {
        workspace: None,
        all: true,
        pinned: false,
        archived: false,
        limit: None,
    };
    match c.request(req).await.unwrap() {
        Reply::Chats { chats } => chats,
        other => panic!("{other:?}"),
    }
}

async fn new_chat(c: &Client, workspace: Option<&str>, prompt: &str) -> ChatInfo {
    let req = Request::ChatNew {
        workspace: workspace.map(Into::into),
        agent: None,
        preset: None,
        title: None,
        prompt: Some(prompt.into()),
        incognito: None,
        folders: Vec::new(),
        compare: Vec::new(),
        confirmed: false,
    };
    match c.request(req).await.unwrap() {
        Reply::Chat { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn answers(c: &Client, chat: &str) -> Vec<String> {
    match c
        .request(Request::ChatRead { chat: chat.into(), all: false, from: None, limit: None })
        .await
    {
        Ok(Reply::ChatTranscript { nodes, .. }) => nodes
            .into_iter()
            .filter_map(|n| match n.body {
                ChatBody::Assistant { text, streaming: false } => Some(text),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
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

#[tokio::test]
async fn another_machines_chats_are_listed_used_and_made_from_here() {
    let mut t = Two::new();
    let there = t.there.client().await.unwrap();
    let theirs = new_chat(&there, None, "hello from laptop2").await;
    until("laptop2 answers its own chat", || async {
        answers(&there, &theirs.id).await == ["you said: hello from laptop2"]
    })
    .await;
    let mut here = t.here.client().await.unwrap();
    let mut pushes = here.pushes();
    here.send(ClientMsg::WatchEvents).unwrap();
    let ours = new_chat(&here, None, "hello from laptop1").await;
    here.request(Request::MachineAdd {
        name: None,
        transport: MachineTransport::Ssh { target: "laptop2".into() },
        token: None,
        force: false,
    })
    .await
    .unwrap();
    online(&here, MachineState::Online).await;

    // Listed beside ours, with the machine on it; the ids stay as they are.
    let listed = chats(&here).await;
    let mirrored = listed.iter().find(|c| c.id == theirs.id).expect("laptop2's chat is listed");
    assert_eq!(mirrored.machine.as_deref(), Some("laptop2"));
    assert_eq!(mirrored.workspace, "laptop2:main");
    assert_eq!(mirrored.addr(), format!("laptop2:c:{}", theirs.handle));
    assert!(listed.iter().any(|c| c.id == ours.id && c.machine.is_none()));
    assert_eq!(chats(&there).await.len(), 1, "nothing of laptop1's goes there (no echoes)");
    // Its workspace's list is its own.
    let req = Request::ChatList {
        workspace: Some("laptop2:main".into()),
        all: false,
        pinned: false,
        archived: false,
        limit: None,
    };
    let Reply::Chats { chats: only } = here.request(req).await.unwrap() else { panic!() };
    assert_eq!(only.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), [theirs.id.as_str()]);

    // Sent from here (by its id), answered there; read from here.
    here.request(Request::ChatSend { chat: theirs.id.clone(), text: "and from laptop1".into() })
        .await
        .unwrap();
    until("the answer, read from here", || async {
        answers(&here, &theirs.id).await.last().map(String::as_str)
            == Some("you said: and from laptop1")
    })
    .await;
    // By its address with the machine, and by its handle alone (only laptop2 has it).
    let addr = format!("laptop2:c:{}", theirs.handle);
    for a in [addr.as_str(), theirs.handle.as_str()] {
        let r = here
            .request(Request::ChatRead { chat: a.into(), all: false, from: None, limit: None })
            .await
            .unwrap();
        assert!(matches!(r, Reply::ChatTranscript { info, .. } if info.id == theirs.id), "{a}");
    }

    // Followed live over this client's own upstream: the branch, then what is new.
    here.send(ClientMsg::ChatSubscribe { chat: theirs.id.clone() }).unwrap();
    here.request(Request::ChatSend { chat: addr.clone(), text: "streamed".into() }).await.unwrap();
    let deadline = Instant::now() + WAIT;
    let (mut reset, mut streamed) = (false, false);
    while !(reset && streamed) {
        assert!(Instant::now() < deadline, "the chat never streamed here (reset {reset})");
        if let Ok(Some(ServerMsg::ChatLive { chat, reset: r, items, .. })) =
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
            && chat == theirs.id
        {
            reset |= r;
            streamed |= items.iter().any(|i| {
                matches!(&i.body, ChatBody::Assistant { text, .. } if text == "you said: streamed")
            });
        }
    }
    here.send(ClientMsg::ChatUnsubscribe { chat: theirs.id.clone() }).unwrap();
    // By its handle alone (only laptop2 has it): followed there too.
    here.send(ClientMsg::ChatSubscribe { chat: theirs.handle.clone() }).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the chat followed by its handle never came");
        if let Ok(Some(ServerMsg::ChatLive { chat, reset: true, items, .. })) =
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
            && chat == theirs.id
            && items.iter().any(|i| matches!(&i.body, ChatBody::User { .. }))
        {
            break;
        }
    }
    // And stopped by its id.
    here.send(ClientMsg::ChatUnsubscribe { chat: theirs.id.clone() }).unwrap();

    // Made in its workspace from here: it lives there, and its news reaches us.
    let made = new_chat(&here, Some("laptop2:main"), "made from laptop1").await;
    assert_eq!(made.machine.as_deref(), Some("laptop2"));
    assert!(chats(&there).await.iter().any(|c| c.id == made.id), "it lives on laptop2");
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await {
            Ok(Some(ServerMsg::Event { event: DomainEvent::ChatUpdated { info } }))
                if info.id == made.id && info.machine.as_deref() == Some("laptop2") =>
            {
                break;
            }
            _ => assert!(Instant::now() < deadline, "no news of the chat made on laptop2"),
        }
    }
    // A pin there shows here.
    there
        .request(Request::ChatUpdate {
            chat: made.id.clone(),
            title: None,
            pinned: Some(true),
            archived: None,
            agent: None,
            preset: None,
            keep: None,
            folders: None,
            confirmed: false,
        })
        .await
        .unwrap();
    until("the pin, seen here", || async {
        chats(&here).await.iter().any(|c| c.id == made.id && c.pinned)
    })
    .await;

    // Its workspace hidden here: its chats too; shown again: back.
    let hide =
        |hidden| Request::MachineHide { name: "laptop2".into(), workspace: "main".into(), hidden };
    here.request(hide(true)).await.unwrap();
    assert!(chats(&here).await.iter().all(|c| c.machine.is_none()));
    here.request(hide(false)).await.unwrap();
    assert!(chats(&here).await.iter().any(|c| c.id == theirs.id));

    // Archived there: listed here with the archived ones, and brought back from here.
    there
        .request(Request::ChatUpdate {
            chat: theirs.id.clone(),
            title: None,
            pinned: None,
            archived: Some(true),
            agent: None,
            preset: None,
            keep: None,
            folders: None,
            confirmed: false,
        })
        .await
        .unwrap();
    until("archived there, gone from here", || async {
        !chats(&here).await.iter().any(|c| c.id == theirs.id)
    })
    .await;
    let req = Request::ChatList {
        workspace: None,
        all: true,
        pinned: false,
        archived: true,
        limit: Some(1),
    };
    let Reply::Chats { chats: archived } = here.request(req).await.unwrap() else { panic!() };
    assert_eq!(archived.len(), 1, "the limit holds across machines");
    here.request(Request::ChatUpdate {
        chat: theirs.id.clone(),
        title: None,
        pinned: None,
        archived: Some(false),
        agent: None,
        preset: None,
        keep: None,
        folders: None,
        confirmed: false,
    })
    .await
    .expect("a remote archived chat is routed there");
    until("back from the archive", || async {
        chats(&here).await.iter().any(|c| c.id == theirs.id)
    })
    .await;
    // A machine can't be called `c`: `c:7f3k` is a chat's address.
    let e = here
        .request(Request::MachineAdd {
            name: Some("c".into()),
            transport: MachineTransport::Ssh { target: "laptop2".into() },
            token: None,
            force: true,
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("can't name a machine"), "{}", e.message);

    // laptop2 goes down: its chats stay listed as they were; using one says why.
    t.there.stop();
    online(&here, MachineState::Offline).await;
    assert!(chats(&here).await.iter().any(|c| c.id == theirs.id), "listed while it is offline");
    let e = here
        .request(Request::ChatSend { chat: theirs.id.clone(), text: "anyone?".into() })
        .await
        .unwrap_err();
    assert!(e.message.contains("laptop2 is offline"), "{}", e.message);
    here.send(ClientMsg::ChatSubscribe { chat: theirs.id.clone() }).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "no word that laptop2 is offline");
        if let Ok(Some(ServerMsg::ChatLive { chat, items, .. })) =
            tokio::time::timeout(Duration::from_millis(500), pushes.recv()).await
            && chat == theirs.id
            && items.iter().any(|i| {
                matches!(&i.body, ChatBody::Notice { text, .. } if text.contains("laptop2 is offline"))
            })
        {
            break;
        }
    }
    // Ours are untouched.
    assert_eq!(answers(&here, &ours.id).await, ["you said: hello from laptop1"]);
}
