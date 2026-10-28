//! H2.7: small confirmed fixes, end to end against a real daemon.

use std::time::Duration;

use ptyro_core::TermSize;
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request, ScreenFormat};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

fn repo(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README"), "hi\n").unwrap();
    git(&["add", "README"]);
    git(&["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "-m", "init"]);
}

async fn workspace_projects(c: &ptyro_client::Client, name: &str) -> Vec<String> {
    let Reply::Workspaces { workspaces } = c.request(Request::WorkspaceList).await.unwrap() else {
        panic!()
    };
    workspaces.into_iter().find(|w| w.name == name).map(|w| w.projects).unwrap_or_default()
}

/// A workspace's name is where its saved view, the TUI's note of the last workspace and what quick
/// new starts there are kept: they follow a rename.
#[tokio::test]
async fn what_is_kept_under_a_workspace_s_name_follows_a_rename() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let new = |name: &str| Request::WorkspaceNew { name: name.into(), profile: None, notify: None };
    c.request(new("work")).await.unwrap();
    let view = serde_json::json!({ "v": 1, "mark": "mine" });
    c.request(Request::SaveView { name: "work".into(), view: view.clone() }).await.unwrap();
    let last = serde_json::json!({ "workspace": "work" });
    c.request(Request::SaveView { name: "_tui".into(), view: last }).await.unwrap();
    let spec = SpawnSpec {
        argv: vec!["sh".into(), "-c".into(), "sleep 30".into()],
        cwd: std::env::temp_dir(),
        workspace: Some("work".into()),
        size: Some(TermSize::new(80, 12)),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    c.request(Request::WorkspaceRename { name: "work".into(), new_name: "job".into() })
        .await
        .unwrap();
    let load = |name: &str| Request::LoadView { name: name.into() };
    let Reply::View { view: moved } = c.request(load("job")).await.unwrap() else { panic!() };
    assert_eq!(moved, Some(view), "the view moved with it");
    let Reply::View { view: gone } = c.request(load("work")).await.unwrap() else { panic!() };
    assert_eq!(gone, None, "nothing is left under the old name");
    let Reply::View { view: Some(tui) } = c.request(load("_tui")).await.unwrap() else { panic!() };
    assert_eq!(tui["workspace"], "job", "the last workspace is the renamed one");
    let launch = |ws: &str| Request::LastLaunch { workspace: ws.into() };
    let Reply::LastLaunch { launch: kept } = c.request(launch("job")).await.unwrap() else {
        panic!()
    };
    assert!(kept.is_some_and(|l| l.argv.first().is_some_and(|a| a == "sh")), "quick new kept it");
}

/// A removed workspace's sessions already went to main; its projects that no other workspace has
/// go there too, instead of showing nowhere.
#[tokio::test]
async fn a_removed_workspace_s_projects_go_to_main() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (only, shared) = (dir.path().join("only"), dir.path().join("shared"));
    repo(&only);
    repo(&shared);
    c.request(Request::WorkspaceNew { name: "side".into(), profile: None, notify: None })
        .await
        .unwrap();
    let add = |p: &std::path::Path, ws: &str| Request::ProjectAdd {
        path: p.to_owned(),
        workspace: Some(ws.into()),
    };
    let id = |r: Reply| match r {
        Reply::Project { info, .. } => info.id,
        other => panic!("{other:?}"),
    };
    let only_id = id(c.request(add(&only, "side")).await.unwrap());
    let shared_id = id(c.request(add(&shared, "side")).await.unwrap());
    c.request(Request::WorkspaceMember {
        name: "main".into(),
        project: shared_id.clone(),
        remove: false,
    })
    .await
    .unwrap();
    c.request(Request::WorkspaceMember {
        name: "main".into(),
        project: only_id.clone(),
        remove: true,
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!workspace_projects(&c, "main").await.contains(&only_id));
    c.request(Request::WorkspaceRemove { name: "side".into() }).await.unwrap();
    let main = workspace_projects(&c, "main").await;
    assert!(main.contains(&only_id), "its own project went to main: {main:?}");
    assert_eq!(main.iter().filter(|p| **p == shared_id).count(), 1, "a shared one stays once");
}

/// Raw bytes sent while a prompt is being typed wait for its Enter: they never land between its
/// text and the Enter that submits it.
#[tokio::test]
async fn raw_bytes_wait_for_a_prompt_being_typed() {
    let d = TempDaemon::start(BIN).unwrap();
    let c = d.client().await.unwrap();
    let spec = SpawnSpec {
        argv: vec!["cat".into()],
        cwd: std::env::temp_dir(),
        name: Some("cat".into()),
        size: Some(TermSize::new(80, 12)),
        ..SpawnSpec::default()
    };
    c.request(Request::Spawn { spec }).await.unwrap();
    let text = c.request(Request::SendText {
        addr: "cat".into(),
        text: "first".into(),
        enter: true,
        wait_ready_ms: None,
    });
    let raw = async {
        // Inside the pause between the text and its Enter.
        tokio::time::sleep(Duration::from_millis(20)).await;
        c.request(Request::SendBytes { addr: "cat".into(), bytes: b"second\r".to_vec() }).await
    };
    let (a, b) = tokio::join!(text, raw);
    a.unwrap();
    b.unwrap();
    // The tty's echo and cat's copy of each line, in whatever order they interleave: four whole
    // lines, the first before the second each time, never run together.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let lines = loop {
        let Reply::Screen { text } = c
            .request(Request::ReadScreen { addr: "cat".into(), format: ScreenFormat::Plain })
            .await
            .unwrap()
        else {
            panic!()
        };
        let lines: Vec<String> =
            text.lines().map(str::trim_end).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        if lines.len() >= 4 || std::time::Instant::now() > deadline {
            break lines;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert!(lines.iter().all(|l| l == "first" || l == "second"), "{lines:?}");
    let firsts: Vec<usize> = (0..lines.len()).filter(|&i| lines[i] == "first").collect();
    let seconds: Vec<usize> = (0..lines.len()).filter(|&i| lines[i] == "second").collect();
    assert_eq!((firsts.len(), seconds.len()), (2, 2), "{lines:?}");
    assert!(firsts[0] < seconds[0] && firsts[1] < seconds[1], "{lines:?}");
}
