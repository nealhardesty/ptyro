//! Home end to end (SPEC §26): `ptyro home` against a real daemon, a real repository and the
//! fake agent keeping a transcript.
//!
//! - A turn that finished unseen puts its project under "needs you", with what was said.
//! - Once the session is gone, the knowledge base still knows it ended on a question: the
//!   project is "left hanging", with the question, and unfolds into where it was left off.
//! - Put away, it stays away; `--restore` brings it back.
//! - Work nobody is running in the project's checkout is left hanging too.
//! - A project that isn't there is refused with the ones that are.
#![cfg(unix)]

use std::time::{Duration, Instant};

use ptyro_core::TermSize;
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request};
use serde_json::Value;

mod kbkit;
use kbkit::*;

/// `ptyro home --json`'s row for the project `name`, once `pred` holds of it.
async fn row(w: &World, name: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let v = w.json(&["home", "--json"]);
        let found =
            v["rows"].as_array().and_then(|r| r.iter().find(|r| r["name"] == name)).cloned();
        match found {
            Some(r) if pred(&r) => return r,
            r => assert!(Instant::now() < deadline, "{name} never was {what}: {r:#?}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn nexts(row: &Value) -> Vec<&str> {
    row["things"].as_array().unwrap().iter().filter_map(|t| t["next"].as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn home_ranks_a_project_by_what_is_live_and_then_by_what_is_remembered() {
    let w = World::new(&format!("[attention]\nmin_work = \"0s\"\n{}", agents("")));
    let repo = w.home().join("blog");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let ok =
            std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap();
        assert!(ok.success(), "git {args:?}");
    }
    let added = w.out(&["project", "add", &repo.to_string_lossy()]);
    assert!(added.contains("blog"), "{added}");
    // Nothing yet: the project is there, and quiet.
    let quiet = row(&w, "blog", "listed", |_| true).await;
    assert_eq!((quiet["section"].as_str(), nexts(&quiet)), (Some("quiet"), vec!["nothing"]));

    let c = w.d.client().await.unwrap();
    let spec = SpawnSpec {
        agent: Some("claude".into()),
        cwd: repo.clone(),
        name: Some("post".into()),
        size: Some(TermSize::new(100, 20)),
        ..SpawnSpec::default()
    };
    match c.request(Request::Spawn { spec }).await.expect("spawn") {
        Reply::Session { .. } => {}
        other => panic!("{other:?}"),
    }
    w.d.wait_for_text(&c, "post", "resumed=", WAIT).await.unwrap();
    w.turn(&c, "post", "work 1 Done. Publish as draft, or wait for the images?").await;

    // Its turn is over and nobody looked: review it, and the row says what it said.
    let r = row(&w, "blog", "to review", |r| r["section"] == "needs").await;
    assert_eq!(nexts(&r), ["review"]);
    let said = r["things"][0]["text"].as_str().unwrap();
    assert!(said.starts_with("finished: Done. Publish as draft"), "{said}");
    assert_eq!(r["things"][0]["session_name"], "post");
    let text = w.out(&["home"]);
    assert!(
        text.starts_with("1 needs you\n") && text.contains("\nNEEDS YOU\n  blog  review"),
        "{text}"
    );

    // The session goes; what it ended on stays known.
    let gone = w.out(&["rm", "--stop", "-y", "post"]);
    assert!(gone.contains("post"), "{gone}");
    let r = row(&w, "blog", "left hanging", |r| r["section"] == "hanging").await;
    assert_eq!(nexts(&r), ["asked"]);
    assert_eq!(r["things"][0]["text"], "Publish as draft, or wait for the images?");
    assert!(r["things"][0]["item"].as_str().is_some_and(|a| a.starts_with("k:")), "{r:#}");
    assert!(
        r["week"].as_array().is_some_and(|w| w.len() == 7 && w[6].as_u64() >= Some(1)),
        "{r:#}"
    );

    // Unfolded (by the start of its name): where it was left off, and its trail.
    let text = w.out(&["home", "bl"]);
    assert!(text.contains("\nLEFT HANGING\n  blog  it asked   Publish as draft"), "{text}");
    assert!(text.contains("left off   you: work 1 Done."), "{text}");
    assert!(text.contains("claude: Done. Publish as draft, or wait for the images?"), "{text}");
    assert!(text.contains("session") && text.contains("[k:"), "{text}");

    // Put away, it stays away; brought back, it is there again.
    let put = w.out(&["home", "blog", "--put-away"]);
    assert!(put.contains("put away until it changes"), "{put}");
    let r = row(&w, "blog", "quiet again", |r| r["section"] == "quiet").await;
    assert_eq!(nexts(&r), ["nothing"]);
    let refused = w.cli(&["home", "blog", "--put-away"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("only what was left hanging"));
    w.out(&["home", "--restore"]);
    row(&w, "blog", "hanging again", |r| r["section"] == "hanging").await;

    // Work in the checkout that nothing is running: left hanging too, under the question.
    std::fs::write(repo.join("draft.md"), "# a draft\n").unwrap();
    let r = row(&w, "blog", "with work to commit", |r| nexts(r).contains(&"commit")).await;
    assert_eq!(nexts(&r), ["asked", "commit"]);
    let commit = &r["things"][1];
    assert!(commit["text"].as_str().unwrap().starts_with("1 file not committed"), "{commit:#}");
    assert_eq!(commit["left"], "● 1");

    // A project that isn't there: refused, naming the ones that are.
    let no = w.cli(&["home", "nosuch"]);
    assert!(!no.status.success());
    let err = String::from_utf8_lossy(&no.stderr).into_owned();
    assert!(err.contains("no project \"nosuch\"") && err.contains("blog"), "{err}");
}
