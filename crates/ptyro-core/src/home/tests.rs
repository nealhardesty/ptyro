use super::*;
use crate::model::RepoStatus;
use crate::status::{Attention, Confidence, SignalSource, StatusRecord, UsageWindow};
use crate::{SessionId, TermSize};

const NOW: u64 = 1_800_000_000_000;
const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;

/// An agent's session in `project`, as the wire carries one.
fn session(name: &str, project: Option<&str>) -> SessionInfo {
    serde_json::from_value(serde_json::json!({
        "id": SessionId::new().full(),
        "handle": "",
        "name": name,
        "kind": "agent",
        "argv": [],
        "cwd": "/",
        "state": {"t": "running"},
        "size": TermSize::new(80, 24),
        "created_ms": NOW - DAY,
        "profile": "default",
        "project": project,
        "agent": "claude",
    }))
    .expect("a session")
}

fn status(s: &mut SessionInfo, status: AgentStatus, kind: Option<&str>, ago: u64) {
    let mut r = StatusRecord::new(status, Confidence::High, SignalSource::Hook);
    r.kind = kind.map(Into::into);
    r.since_ms = NOW - ago;
    s.status = Some(r);
}

fn attention(s: &mut SessionInfo, kind: AttentionKind, text: &str, ago: u64) {
    s.attention = Some(Attention { kind, since_ms: NOW - ago, text: text.into() });
}

fn seen(info: SessionInfo) -> Seen {
    let asks = info.last_reply.as_deref().and_then(question);
    Seen { info, asks }
}

fn project(id: &str, used_ago: u64) -> ProjectInfo {
    ProjectInfo {
        id: id.into(),
        machine: None,
        slug: id.into(),
        root: format!("/src/{id}").into(),
        default_branch: None,
        remote_url: None,
        pin: None,
        created_ms: 0,
        last_used_ms: Some(NOW - used_ago),
        moved_to: None,
    }
}

fn worktree(id: &str, project: &str, st: RepoStatus) -> WorktreeInfo {
    WorktreeInfo {
        id: id.into(),
        machine: None,
        project: project.into(),
        project_slug: project.into(),
        name: id.into(),
        path: format!("/wt/{id}").into(),
        branch: Some(id.into()),
        base: None,
        is_root: false,
        profile: None,
        state: WorktreeState::Ready,
        created_ms: NOW - 3 * DAY,
        removed_ms: None,
        last_used_ms: Some(NOW - 2 * DAY),
        status: Some(st),
        locked: false,
        title: None,
    }
}

fn inputs(projects: &[&str]) -> Inputs {
    Inputs {
        now_ms: NOW,
        projects: projects.iter().map(|p| project(p, DAY)).collect(),
        memory: Memory::On,
        ..Inputs::default()
    }
}

fn order(v: &HomeView) -> Vec<(&str, Section, Next)> {
    v.rows.iter().map(|r| (r.name.as_str(), r.section, r.next().next)).collect()
}

#[test]
fn a_question_is_the_last_line_when_it_ends_in_one() {
    assert_eq!(
        question("Done. I tightened the intro.\n\nPublish as draft, or wait for the images?"),
        Some("Publish as draft, or wait for the images?".into())
    );
    // Markdown's closing marks don't hide it, and the sentence is taken from where it starts.
    assert_eq!(
        question("All green. **Should I run the migration against kb.db?**"),
        Some("Should I run the migration against kb.db?".into())
    );
    // A sentence too short to stand alone keeps its line.
    assert_eq!(question("The tests pass. Ready?"), Some("The tests pass. Ready?".into()));
    assert_eq!(question("Why did it fail? Because the lock was held."), None);
    assert_eq!(question("Is it done?\n\nYes: all 12 tests pass."), None);
    assert_eq!(question(""), None);
}

#[test]
fn rows_are_ranked_by_whose_move_it_is() {
    let mut inp = inputs(&["blocked", "failed", "finished", "busy", "asked", "open", "quiet"]);
    let mut a = session("a", Some("blocked"));
    status(&mut a, AgentStatus::Blocked, Some("permission"), 14 * MIN);
    attention(&mut a, AttentionKind::Blocked, "wants permission: Bash(rm -rf target)", 14 * MIN);
    let mut b = session("b", Some("failed"));
    status(&mut b, AgentStatus::Errored, Some("auth"), HOUR);
    attention(&mut b, AttentionKind::Errored, "stopped (auth)", HOUR);
    let mut c = session("c", Some("finished"));
    status(&mut c, AgentStatus::Idle, None, 5 * MIN);
    attention(&mut c, AttentionKind::Idle, "finished", 5 * MIN);
    c.last_reply = Some("Added retry to the uploader.".into());
    let mut d = session("d", Some("busy"));
    status(&mut d, AgentStatus::Working, None, MIN);
    d.activity = Some("Bash: cargo test".into());
    d.turns = 5;
    // Seen (no attention), idle, and its last word was a question.
    let mut e = session("e", Some("asked"));
    status(&mut e, AgentStatus::Idle, None, 3 * HOUR);
    e.last_reply = Some("Done. Publish as draft, or wait for the images?".into());
    // Seen, idle a minute: your turn, nothing forgotten.
    let mut f = session("f", Some("open"));
    status(&mut f, AgentStatus::Idle, None, MIN);
    f.last_reply = Some("Renamed it everywhere.".into());
    inp.sessions = [a, b, c, d, e, f].into_iter().map(seen).collect();

    let v = build(&inp);
    assert_eq!(
        order(&v),
        [
            ("blocked", Section::Needs, Next::Answer),
            ("failed", Section::Needs, Next::Failed),
            ("finished", Section::Needs, Next::Review),
            ("asked", Section::Hanging, Next::Asked),
            ("busy", Section::Working, Next::Working),
            ("open", Section::Working, Next::Open),
            ("quiet", Section::Quiet, Next::Nothing),
        ]
    );
    assert_eq!(v.row("blocked").unwrap().next().text, "wants permission: Bash(rm -rf target)");
    assert_eq!(v.row("finished").unwrap().next().text, "finished: Added retry to the uploader.");
    assert_eq!(v.row("busy").unwrap().next().text, "Bash: cargo test (turn 6)");
    assert_eq!(v.row("asked").unwrap().next().text, "Publish as draft, or wait for the images?");
    assert_eq!(v.headline(), "3 need you · 1 left hanging · 1 working");
    let c = v.counts();
    assert_eq!((c.needs, c.hanging, c.working, c.quiet), (3, 1, 1, 1));
}

#[test]
fn a_blocked_agent_still_needs_you_after_you_looked() {
    let mut inp = inputs(&["p"]);
    let mut s = session("s", Some("p"));
    status(&mut s, AgentStatus::Blocked, Some("question"), HOUR);
    inp.sessions = vec![seen(s)];
    assert_eq!(order(&build(&inp)), [("p", Section::Needs, Next::Answer)]);
}

#[test]
fn an_agent_idle_for_hours_is_left_hanging() {
    let mut inp = inputs(&["p"]);
    let mut s = session("s", Some("p"));
    status(&mut s, AgentStatus::Idle, None, STALE_AFTER_MS + MIN);
    s.last_reply = Some("All 40 tests pass.".into());
    inp.sessions = vec![seen(s)];
    let v = build(&inp);
    assert_eq!(order(&v), [("p", Section::Hanging, Next::Stale)]);
    assert_eq!(v.rows[0].next().text, "All 40 tests pass.");
}

#[test]
fn a_usage_limit_is_not_yours_to_answer() {
    let mut inp = inputs(&["p"]);
    let mut s = session("s", Some("p"));
    status(&mut s, AgentStatus::Errored, Some("rate_limit"), 10 * MIN);
    attention(&mut s, AttentionKind::Errored, "stopped (rate_limit)", 10 * MIN);
    inp.sessions = vec![seen(s)];
    inp.usage = vec![Usage {
        profile: "default".into(),
        agent: "claude".into(),
        limited_until_ms: Some(NOW + 40 * MIN),
        at_ms: NOW,
        ..Usage::default()
    }];
    let v = build(&inp);
    assert_eq!(order(&v), [("p", Section::Working, Next::Limit)]);
    assert_eq!(v.rows[0].next().text, "waiting for the usage limit · resets in 40m");
    assert_eq!(v.counts().needs, 0);
}

#[test]
fn work_nobody_is_running_is_left_hanging_and_work_in_use_is_not() {
    let mut inp = inputs(&["p", "q", "r"]);
    let dirty = RepoStatus { modified: 2, insertions: 84, deletions: 12, ..RepoStatus::default() };
    let ahead = RepoStatus {
        upstream: Some("origin/x".into()),
        ahead: 3,
        subject: Some("post: first draft".into()),
        ..RepoStatus::default()
    };
    inp.worktrees = vec![
        worktree("dirty", "p", dirty.clone()),
        worktree("ahead", "q", ahead),
        worktree("inuse", "r", dirty),
    ];
    let mut s = session("s", Some("r"));
    s.worktree = Some("inuse".into());
    status(&mut s, AgentStatus::Working, None, MIN);
    inp.sessions = vec![seen(s)];
    let v = build(&inp);
    assert_eq!(
        order(&v),
        [
            ("p", Section::Hanging, Next::Commit),
            ("q", Section::Hanging, Next::Push),
            ("r", Section::Working, Next::Working),
        ]
    );
    assert_eq!(v.rows[0].next().text, "2 files not committed (+84 −12), nothing running");
    assert_eq!(v.rows[1].next().text, "3 commits to push: post: first draft");
    assert_eq!(v.rows[1].next().left.as_deref(), Some("↑3"));
    // The session's own row says what is open in its worktree.
    assert_eq!(v.rows[2].next().left.as_deref(), Some("● 2"));
}

#[test]
fn memory_says_what_was_left_hanging_and_where_things_were_left_off() {
    let mut inp = inputs(&["asked", "cut", "old", "planned"]);
    let tail = |project: &str, last: Last, reply: &str, ago: u64| Tail {
        project: project.into(),
        addr: Some(format!("k:{project}")),
        at_ms: NOW - ago,
        you: Some("tighten the intro".into()),
        reply: Some(reply.into()),
        last: Some(last),
        asks: question(reply),
        week: vec![0, 0, 3, 9, 0, 0, 1],
        ..Tail::default()
    };
    let mut planned = tail("planned", Last::Reply, "Merged.", 2 * DAY);
    planned.planned =
        Some(Planned { addr: "k:note".into(), text: "wire the new cert".into(), at_ms: NOW - DAY });
    inp.tails = vec![
        tail("asked", Last::Reply, "Done. Publish as draft, or wait for the images?", 9 * DAY),
        tail("cut", Last::You, "", 3 * DAY),
        // Too old to be hanging: only where it was left off.
        tail("old", Last::Reply, "Shall I go on with the second half?", 45 * DAY),
        planned,
    ];
    let v = build(&inp);
    assert_eq!(
        order(&v),
        [
            ("asked", Section::Hanging, Next::Asked),
            ("cut", Section::Hanging, Next::CutOff),
            ("planned", Section::Hanging, Next::Planned),
            ("old", Section::Quiet, Next::LeftOff),
        ]
    );
    assert_eq!(v.row("cut").unwrap().next().text, "you: tighten the intro (no reply)");
    assert_eq!(v.row("planned").unwrap().next().text, "wire the new cert");
    assert_eq!(v.row("old").unwrap().next().text, "tighten the intro");
    assert_eq!(v.row("asked").unwrap().next().item.as_deref(), Some("k:asked"));
    assert_eq!(v.row("asked").unwrap().week, [0, 0, 3, 9, 0, 0, 1]);
}

#[test]
fn a_session_still_listed_speaks_for_itself_not_its_memory() {
    let mut inp = inputs(&["p"]);
    let mut s = session("s", Some("p"));
    status(&mut s, AgentStatus::Working, None, MIN);
    inp.tails = vec![Tail {
        project: "p".into(),
        addr: Some("k:1".into()),
        session: Some(s.id.full()),
        at_ms: NOW - HOUR,
        reply: Some("Shall I continue with the rest?".into()),
        last: Some(Last::Reply),
        asks: Some("Shall I continue with the rest?".into()),
        ..Tail::default()
    }];
    inp.sessions = vec![seen(s)];
    let v = build(&inp);
    assert_eq!(order(&v), [("p", Section::Working, Next::Working)]);
    assert_eq!(v.rows[0].things.len(), 1);
}

#[test]
fn what_is_put_away_stays_away_until_it_changes() {
    let mut inp = inputs(&["p"]);
    let st = RepoStatus { modified: 2, ..RepoStatus::default() };
    inp.worktrees = vec![worktree("w", "p", st)];
    let v = build(&inp);
    let t = v.rows[0].next().clone();
    assert_eq!(t.next, Next::Commit);
    assert!(t.next.dismissable());
    inp.dismissed.insert(t.key.clone(), t.stamp.clone());
    assert_eq!(order(&build(&inp)), [("p", Section::Quiet, Next::Nothing)]);
    // A third file: it is back.
    inp.worktrees[0].status.as_mut().unwrap().modified = 3;
    assert_eq!(order(&build(&inp)), [("p", Section::Hanging, Next::Commit)]);
}

#[test]
fn sessions_of_no_project_get_a_row_only_when_there_is_something_to_say() {
    let mut inp = inputs(&[]);
    let mut shell = session("sh", None);
    shell.kind = SessionKind::Shell;
    shell.agent = None;
    inp.sessions = vec![seen(shell.clone())];
    assert!(build(&inp).rows.is_empty());
    attention(&mut shell, AttentionKind::Notification, "make finished (exit 0)", MIN);
    inp.sessions = vec![seen(shell)];
    let v = build(&inp);
    assert_eq!(order(&v), [("no project", Section::Needs, Next::Look)]);
    assert_eq!(v.rows[0].key, "~");
}

#[test]
fn the_oldest_wait_comes_first_among_what_needs_you() {
    let mut inp = inputs(&["new", "old"]);
    let mut a = session("a", Some("new"));
    attention(&mut a, AttentionKind::Blocked, "asks", MIN);
    let mut b = session("b", Some("old"));
    attention(&mut b, AttentionKind::Blocked, "asks", HOUR);
    let mut c = session("c", Some("old"));
    attention(&mut c, AttentionKind::Idle, "finished", 2 * HOUR);
    inp.sessions = [a, b, c].into_iter().map(seen).collect();
    let v = build(&inp);
    assert_eq!(v.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["old", "new"]);
    // A project's own things: the most urgent first, whatever their age.
    let old = v.row("old").unwrap();
    assert_eq!(old.things.iter().map(|t| t.next).collect::<Vec<_>>(), [Next::Answer, Next::Review]);
}

#[test]
fn the_strip_shows_every_window_of_every_meter() {
    let mut s = session("s", Some("p"));
    s.agent = Some("codex".into());
    s.profile = "work".into();
    let usage = vec![
        Usage {
            profile: "work".into(),
            agent: "claude".into(),
            windows: vec![
                UsageWindow {
                    name: "5h".into(),
                    used_percent: 42,
                    resets_at_ms: Some(NOW + 130 * MIN),
                },
                UsageWindow { name: "7d".into(), used_percent: 85, resets_at_ms: Some(NOW + DAY) },
            ],
            at_ms: NOW,
            ..Usage::default()
        },
        Usage {
            profile: "personal".into(),
            agent: "claude".into(),
            windows: vec![UsageWindow {
                name: "5h".into(),
                used_percent: 100,
                resets_at_ms: Some(NOW + 40 * MIN),
            }],
            limited_until_ms: Some(NOW + 40 * MIN),
            at_ms: NOW,
            ..Usage::default()
        },
    ];
    let m = meters(&usage, &[seen(s)], NOW);
    let text: Vec<String> = m.iter().map(Meter::text).collect();
    assert_eq!(
        text,
        [
            "personal/claude limited ↻40m · 5h 100% ↻40m",
            "work/claude 5h 42% ↻2h10m · 7d 85% ↻1d",
            // Runs here and reports nothing: said, not left out.
            "work/codex ?",
        ]
    );
    assert_eq!(
        m.iter().map(|m| m.level).collect::<Vec<_>>(),
        [Level::Limited, Level::Warn, Level::Unknown]
    );
}

#[test]
fn a_meter_says_when_a_window_runs_out_early_and_when_a_reading_is_old() {
    // 80% used one hour into a five-hour window: full in 15 minutes, hours before it resets.
    let fast = Usage {
        profile: "default".into(),
        agent: "claude".into(),
        windows: vec![UsageWindow {
            name: "5h".into(),
            used_percent: 80,
            resets_at_ms: Some(NOW + 4 * HOUR),
        }],
        at_ms: NOW,
        ..Usage::default()
    };
    assert_eq!(meters(&[fast], &[], NOW)[0].text(), "claude 5h 80% ↻4h full ~15m");
    // On pace to last: nothing more is said.
    let slow = Usage {
        profile: "default".into(),
        agent: "claude".into(),
        windows: vec![UsageWindow {
            name: "5h".into(),
            used_percent: 60,
            resets_at_ms: Some(NOW + HOUR),
        }],
        at_ms: NOW,
        ..Usage::default()
    };
    assert_eq!(meters(&[slow], &[], NOW)[0].text(), "claude 5h 60% ↻1h");
    let old = Usage {
        profile: "default".into(),
        agent: "claude".into(),
        windows: vec![
            UsageWindow { name: "5h".into(), used_percent: 30, resets_at_ms: Some(NOW - HOUR) },
            UsageWindow { name: "7d".into(), used_percent: 12, resets_at_ms: Some(NOW + DAY) },
        ],
        at_ms: NOW - 8 * HOUR,
        ..Usage::default()
    };
    let m = &meters(&[old], &[], NOW)[0];
    assert_eq!(m.text(), "claude 5h reset · 7d 12% ↻1d · (8h old)");
    assert_eq!(m.level, Level::Old);
}

#[test]
fn the_away_line_counts_what_happened_and_goes_after_a_while() {
    let away = Away {
        since_ms: NOW - 150 * MIN,
        back_ms: NOW - 20 * MIN,
        finished: 3,
        failed: 1,
        asked: 0,
        commits: 2,
    };
    assert_eq!(away.line(), "away 2h10m: 3 finished, 1 failed, 2 commits");
    let mut inp = inputs(&["p"]);
    inp.away = Some(away.clone());
    assert!(build(&inp).away.is_some());
    inp.away = Some(Away { back_ms: NOW - AWAY_SHOWN_MS - MIN, ..away });
    assert!(build(&inp).away.is_none());
    assert_eq!(
        Away { since_ms: 0, back_ms: HOUR, ..Away::default() }.line(),
        "away 1h: nothing happened"
    );
}

#[test]
fn a_week_is_seven_marks() {
    assert_eq!(spark(&[0, 0, 3, 9, 0, 0, 1]), "··▃█··▁");
    assert_eq!(spark(&[0; 7]), "");
    assert_eq!(spark(&[]), "");
}

#[test]
fn the_text_is_what_the_command_prints() {
    let mut inp = inputs(&["blog", "ptyro"]);
    let mut a = session("fix-kb-lock", Some("ptyro"));
    attention(&mut a, AttentionKind::Blocked, "run the migration against kb.db?", 14 * MIN);
    inp.sessions = vec![seen(a)];
    inp.tails = vec![Tail {
        project: "blog".into(),
        addr: Some("k:7f3k".into()),
        at_ms: NOW - 9 * DAY,
        you: Some("tighten the intro".into()),
        reply: Some("Done. Publish as draft, or wait for the images?".into()),
        last: Some(Last::Reply),
        asks: Some("Publish as draft, or wait for the images?".into()),
        ..Tail::default()
    }];
    inp.memory = Memory::Off;
    let mut v = build(&inp);
    v.trail = Some(Trail {
        key: "blog".into(),
        left_off: Some(LeftOff {
            you: Some("tighten the intro".into()),
            reply: Some("Done. Publish as draft, or wait for the images?".into()),
            agent: Some("claude".into()),
            at_ms: NOW - 9 * DAY,
            addr: Some("k:7f3k".into()),
            session: None,
        }),
        entries: vec![TrailEntry {
            kind: "session".into(),
            title: "worktrees post".into(),
            detail: "claude · 14 turns · 6 files".into(),
            at_ms: NOW - 9 * DAY,
            addr: "k:7f3k".into(),
            session: None,
        }],
        hot: vec![Hot { path: "content/worktrees.md".into(), count: 9 }],
    });
    assert_eq!(
        text(&v),
        "1 needs you · 1 left hanging

NEEDS YOU
  ptyro  answer     run the migration against kb.db?  · fix-kb-lock · 14m

LEFT HANGING
  blog   it asked   Publish as draft, or wait for the images?  · 9d
      left off   you: tighten the intro
                 claude: Done. Publish as draft, or wait for the images?
        9d ago   session worktrees post · claude · 14 turns · 6 files  [k:7f3k]
      hot files  content/worktrees.md ×9

claude ?

The knowledge base is off: only what is live is shown (Settings → Knowledge → enabled).
"
    );
}

#[test]
fn a_row_is_found_by_its_name_or_the_start_of_it() {
    let v = build(&inputs(&["blog", "blender", "ptyro"]));
    assert_eq!(v.find("ptyro").map(|r| r.key.as_str()), Some("ptyro"));
    assert_eq!(v.find("pt").map(|r| r.key.as_str()), Some("ptyro"));
    assert_eq!(v.find("bl").map(|r| r.key.as_str()), None);
    assert_eq!(v.find("blog").map(|r| r.key.as_str()), Some("blog"));
}

#[test]
fn the_view_crosses_the_wire() {
    let mut inp = inputs(&["p"]);
    let mut s = session("s", Some("p"));
    attention(&mut s, AttentionKind::Blocked, "asks", MIN);
    inp.sessions = vec![seen(s)];
    let v = build(&inp);
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(serde_json::from_str::<HomeView>(&json).unwrap(), v);
    assert!(json.contains(r#""section":"needs""#) && json.contains(r#""next":"answer""#));
}
