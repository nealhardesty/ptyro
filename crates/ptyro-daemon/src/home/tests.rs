use std::sync::Arc;

use ptyro_core::ErrorCode;
use ptyro_core::home::{HomeView, Memory, Next, Section};
use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::Stamp;
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use ptyro_core::sync::MutexExt;
use ptyro_protocol::{ClientKind, HomeDismiss, Reply};

use crate::daemon::Daemon;
use crate::guard::Caller;
use crate::kb::requests::tests::daemon;
use crate::kb::search::tests::{prepared, set_config};
use crate::kb::{Facts, Prepared};

const MIN: u64 = 60_000;
const DAY: u64 = 86_400_000;

/// A session in project `p1` (`feeder`) that was left on a question, `ago` before now.
fn left_asking(uri: &str, ago: u64) -> Prepared {
    let at = ptyro_core::now_ms() - ago;
    let mut p = prepared(
        ItemKind::Session,
        uri,
        "fix the hopper",
        vec![
            Part::new(ChunkKind::User, "fix the hopper jam", at - 30).anchor("u1"),
            Part::new(ChunkKind::Assistant, "Found it: the gate sticks.", at - 20).anchor("a1"),
            Part::new(ChunkKind::User, "replace the gate spring", at - 10).anchor("u2"),
            Part::new(
                ChunkKind::Assistant,
                "Replaced.\n\nShall I also recalibrate the feeder motor?",
                at,
            )
            .anchor("a2"),
        ],
        Stamp {
            project: Some("p1".into()),
            workspace: Some("w1".into()),
            agent: Some("claude".into()),
            session: Some("gone".into()),
            ..Stamp::new(Writer::Agent, Some("work".into()))
        },
    );
    p.source.append = true;
    p.source.files = vec!["src/gate.rs".into(), "src/motor.rs".into()];
    (p.source.started_ms, p.source.ended_ms) = (Some(at - 30), Some(at));
    p.facts = Facts { session: true, ..Facts::default() };
    p
}

async fn home(d: &Arc<Daemon>, trail: Option<&str>) -> HomeView {
    match d.home(trail.map(Into::into), None, false, Caller::default()).await.unwrap() {
        Reply::Home { home } => *home,
        r => panic!("{r:?}"),
    }
}

#[tokio::test]
async fn a_project_nothing_runs_in_is_ranked_by_what_memory_holds() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    // Nothing kept yet: the project is there, and quiet.
    let v = home(&d, None).await;
    assert_eq!(v.memory, Memory::On);
    assert_eq!((v.rows[0].name.as_str(), v.rows[0].section), ("feeder", Section::Quiet));
    assert_eq!(v.rows[0].workspace.as_deref(), Some("main"));

    d.kb_ingest(left_asking("session://claude/c1", 2 * DAY)).await.unwrap();
    d.kb_ingest(left_asking("session://claude/c0", 5 * DAY)).await.unwrap();
    let v = home(&d, Some("feeder")).await;
    let row = &v.rows[0];
    assert_eq!((row.section, row.next().next), (Section::Hanging, Next::Asked));
    assert_eq!(row.next().text, "Shall I also recalibrate the feeder motor?");
    assert!(row.next().item.as_deref().is_some_and(|a| a.starts_with("k:")));
    assert_eq!(row.week.iter().sum::<u16>(), 4);
    assert_eq!(v.headline(), "1 left hanging");

    // Unfolded: where it was left off, what happened, and the files both sessions touched.
    let trail = v.trail.expect("a trail");
    let left = trail.left_off.expect("left off");
    assert_eq!(left.you.as_deref(), Some("replace the gate spring"));
    assert_eq!(left.reply.as_deref(), Some("Replaced. Shall I also recalibrate the feeder motor?"));
    assert_eq!(left.agent.as_deref(), Some("claude"));
    // (The session is gone from ptyro's list: only the knowledge base's address is left.)
    assert_eq!(left.session, None);
    assert_eq!(trail.entries.len(), 2);
    assert_eq!(trail.entries[0].kind, "session");
    assert_eq!(trail.entries[0].detail, "claude · 2 turns · 2 files");
    let hot: Vec<(&str, u32)> = trail.hot.iter().map(|h| (h.path.as_str(), h.count)).collect();
    assert_eq!(hot, [("src/gate.rs", 2), ("src/motor.rs", 2)]);
}

#[tokio::test]
async fn put_away_it_stays_away_and_comes_back_when_asked() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    d.kb_ingest(left_asking("session://claude/c1", DAY)).await.unwrap();
    let thing = home(&d, None).await.rows[0].next().clone();
    let put = HomeDismiss { key: thing.key.clone(), stamp: thing.stamp.clone() };
    let Reply::Home { home: v } = d.home(None, Some(put), false, Caller::default()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(v.rows[0].section, Section::Quiet);
    assert_eq!(home(&d, None).await.rows[0].section, Section::Quiet);
    // Something that isn't there can't be put away.
    let nothing = HomeDismiss { key: "asked:nothing".into(), stamp: String::new() };
    let e = d.home(None, Some(nothing), false, Caller::default()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let Reply::Home { home: v } = d.home(None, None, true, Caller::default()).await.unwrap() else {
        panic!()
    };
    assert_eq!(v.rows[0].section, Section::Hanging);
}

#[tokio::test]
async fn with_the_knowledge_base_off_home_shows_what_is_live() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    d.kb_ingest(left_asking("session://claude/c1", DAY)).await.unwrap();
    set_config(&d, "[kb]\nenabled = false\n");
    let v = home(&d, Some("feeder")).await;
    assert_eq!(v.memory, Memory::Off);
    assert_eq!(v.rows[0].section, Section::Quiet);
    assert!(v.trail.is_some_and(|t| t.left_off.is_none() && t.entries.is_empty()));
}

#[tokio::test]
async fn coming_back_after_a_while_is_said_once() {
    let t = tempfile::tempdir().unwrap();
    let d = daemon(&t, "");
    // A script's connection is no sign of anyone.
    d.human_seen(Some(ClientKind::Cli));
    assert!(home(&d, None).await.away.is_none());
    d.human_seen(Some(ClientKind::Tui));
    assert!(home(&d, None).await.away.is_none());
    // The last sign of you was two hours ago; now you type.
    d.home.locked().seen_ms -= 120 * MIN;
    d.human_seen(Some(ClientKind::Web));
    let away = home(&d, None).await.away.expect("away");
    assert_eq!(away.line(), "away 2h: nothing happened");
    // Typing on doesn't move it.
    d.human_seen(Some(ClientKind::Tui));
    assert_eq!(home(&d, None).await.away, Some(away));
}
