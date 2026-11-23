use ptyro_client::ConnectOptions;
use ptyro_core::config::Config;
use ptyro_core::home::{Inputs, Last, LeftOff, Tail, Trail, TrailEntry, build};
use ptyro_core::model::ProjectInfo;
use ptyro_core::paths::Paths;

use super::*;
use crate::Options;
use crate::app::AppMsg;

const DAY: u64 = 86_400_000;

struct T {
    app: App,
    _home: tempfile::TempDir,
}

fn with(opts: Options, config: Config) -> T {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().to_path_buf();
    let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| dir.clone())).unwrap();
    let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
    let area = Rect::new(0, 0, 120, 30);
    T { app: App::new(opts, connect, paths, config, false, area), _home: home }
}

fn t() -> T {
    with(Options::default(), Config::default())
}

fn screen(app: &App) -> String {
    let (w, h) = (app.area.width, app.area.height);
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::ui::draw(app, f)).unwrap();
    let buf = t.backend().buffer();
    buf.content.chunks(buf.area.width as usize).fold(String::new(), |mut s, row| {
        s.extend(row.iter().map(|c| c.symbol()));
        s.push('\n');
        s
    })
}

/// The requests for home's view the TUI made since the last call (not connected: each comes
/// back as an error).
fn asked(app: &mut App) -> usize {
    let mut n = 0;
    while let Ok(m) = app.rx.try_recv() {
        if matches!(m, AppMsg::Reply(Pending::HomeView, _)) {
            n += 1;
        }
    }
    n
}

fn key(app: &mut App, code: KeyCode) {
    app.chat_key(KeyEvent::new(code, Mods::NONE));
}

fn ch(app: &mut App, c: char) {
    key(app, KeyCode::Char { c });
}

fn project(id: &str) -> ProjectInfo {
    ProjectInfo {
        id: id.into(),
        machine: None,
        slug: id.into(),
        root: format!("/src/{id}").into(),
        default_branch: None,
        remote_url: None,
        pin: None,
        created_ms: 0,
        last_used_ms: Some(now_ms() - DAY),
        moved_to: None,
    }
}

/// `blog` was left on a question, `dots` on a prompt nobody answered; `infra` is quiet.
fn view(asked_ago: u64) -> HomeView {
    let now = now_ms();
    let tail = |project: &str, last: Last, reply: &str, ago: u64| Tail {
        project: project.into(),
        addr: Some(format!("k:{project}")),
        at_ms: now - ago,
        you: Some("tighten the intro".into()),
        reply: Some(reply.into()),
        last: Some(last),
        asks: ptyro_core::home::question(reply),
        ..Tail::default()
    };
    build(&Inputs {
        now_ms: now,
        projects: vec![project("blog"), project("dots"), project("infra")],
        tails: vec![
            tail("blog", Last::Reply, "Done. Publish as draft, or wait for the images?", asked_ago),
            tail("dots", Last::You, "", 2 * DAY),
        ],
        memory: Memory::On,
        ..Inputs::default()
    })
}

fn show(app: &mut App, v: HomeView) {
    app.home_got(Ok(Reply::Home { home: Box::new(v) }));
}

fn selected(app: &App) -> Option<Sel> {
    app.home_screen().and_then(HomeScreen::current)
}

#[test]
fn home_is_a_panel_of_rows_by_section_with_the_next_thing_on_each() {
    let mut t = t();
    t.app.toggle_home();
    assert_eq!(asked(&mut t.app), 1, "asked for as it opens");
    assert!(screen(&t.app).contains("Home"), "{}", screen(&t.app));
    show(&mut t.app, view(DAY));
    let s = screen(&t.app);
    assert!(s.contains("2 left hanging"), "{s}");
    let at = |what: &str| s.find(what).unwrap_or_else(|| panic!("no {what:?} in\n{s}"));
    assert!(at("LEFT HANGING") < at("blog") && at("blog") < at("dots") && at("dots") < at("QUIET"));
    assert!(s.contains("it asked   Publish as draft, or wait for the images?"), "{s}");
    assert!(s.contains("cut off    you: tighten the intro (no reply)"), "{s}");
    assert!(s.contains("nothing open"), "{s}");
    // The keys it has are said, and `x` only where it applies.
    assert!(s.contains("⏎ go · → unfold") && s.contains("x put away"), "{s}");
    assert_eq!(selected(&t.app), Some(Sel::Row("blog".into())));
    // `H` again closes it.
    t.app.toggle_home();
    assert!(t.app.chat_panel.is_none());
}

#[test]
fn the_cursor_stays_on_its_row_when_the_rows_move() {
    let mut t = t();
    t.app.toggle_home();
    show(&mut t.app, view(DAY));
    ch(&mut t.app, 'j');
    assert_eq!(selected(&t.app), Some(Sel::Row("dots".into())));
    // `blog`'s question is now too old to be hanging: it falls to the quiet ones. The cursor
    // is still on `dots`, which is now the first row.
    show(&mut t.app, view(45 * DAY));
    let s = screen(&t.app);
    assert!(s.find("dots").unwrap() < s.find("blog").unwrap(), "{s}");
    assert_eq!(selected(&t.app), Some(Sel::Row("dots".into())));
    ch(&mut t.app, 'j');
    assert_eq!(selected(&t.app), Some(Sel::Row("blog".into())));
    ch(&mut t.app, 'G');
    assert_eq!(selected(&t.app), Some(Sel::Row("infra".into())));
}

#[test]
fn a_row_unfolds_into_its_trail_and_folds_again() {
    let mut t = t();
    t.app.toggle_home();
    show(&mut t.app, view(DAY));
    asked(&mut t.app);
    key(&mut t.app, KeyCode::Right);
    assert_eq!(asked(&mut t.app), 1, "its trail is asked for");
    assert_eq!(t.app.home_screen().unwrap().open.as_deref(), Some("blog"));
    let mut v = view(DAY);
    v.trail = Some(Trail {
        key: "blog".into(),
        left_off: Some(LeftOff {
            you: Some("tighten the intro".into()),
            reply: Some("Done. Publish as draft, or wait for the images?".into()),
            agent: Some("claude".into()),
            at_ms: now_ms() - DAY,
            addr: Some("k:blog".into()),
            session: None,
        }),
        entries: vec![TrailEntry {
            kind: "session".into(),
            title: "worktrees post".into(),
            detail: "claude · 14 turns · 6 files".into(),
            at_ms: now_ms() - DAY,
            addr: "k:blog".into(),
            session: None,
        }],
        hot: vec![ptyro_core::home::Hot { path: "content/worktrees.md".into(), count: 9 }],
    });
    show(&mut t.app, v);
    let s = screen(&t.app);
    assert!(s.contains("left off   you: tighten the intro"), "{s}");
    assert!(s.contains("claude: Done. Publish as draft"), "{s}");
    assert!(
        s.contains("session  worktrees post") && s.contains("claude · 14 turns · 6 files"),
        "{s}"
    );
    assert!(s.contains("hot files  content/worktrees.md ×9"), "{s}");
    // Down goes through the trail's lines, each once (the two "left off" lines are one stop).
    ch(&mut t.app, 'j');
    assert_eq!(selected(&t.app), Some(Sel::Left("blog".into())));
    ch(&mut t.app, 'j');
    assert_eq!(selected(&t.app), Some(Sel::Entry("k:blog".into())));
    ch(&mut t.app, 'j');
    assert_eq!(selected(&t.app), Some(Sel::Row("dots".into())));
    // ← folds whatever is unfolded, and the cursor is on its row.
    key(&mut t.app, KeyCode::Left);
    assert_eq!(t.app.home_screen().unwrap().open, None);
    assert_eq!(selected(&t.app), Some(Sel::Row("blog".into())));
    assert!(!screen(&t.app).contains("hot files"));
}

#[test]
fn only_what_was_left_hanging_is_put_away() {
    let mut t = t();
    t.app.toggle_home();
    show(&mut t.app, view(DAY));
    asked(&mut t.app);
    ch(&mut t.app, 'x');
    assert_eq!(asked(&mut t.app), 1, "the daemon is asked to put it away");
    ch(&mut t.app, 'G');
    ch(&mut t.app, 'x');
    assert_eq!(asked(&mut t.app), 0, "a quiet row has nothing to put away");
    assert!(t.app.toast.as_ref().is_some_and(|t| t.text.contains("can't be put away")));
    // Esc closes the panel.
    key(&mut t.app, KeyCode::Esc);
    assert!(t.app.chat_panel.is_none());
}

/// The requests a start made for home's first view.
fn started(app: &mut App) -> usize {
    let mut n = 0;
    while let Ok(m) = app.rx.try_recv() {
        if matches!(m, AppMsg::Reply(Pending::HomeStart, _)) {
            n += 1;
        }
    }
    n
}

#[test]
fn a_bare_ptyro_opens_on_home_when_there_is_something_to_show() {
    let mut t = with(Options { home: true, ..Options::default() }, Config::default());
    t.app.home_at_start();
    assert_eq!(started(&mut t.app), 1, "the view is asked for first");
    assert!(t.app.chat_panel.is_none(), "nothing covers the panes until it is known");
    // Nothing to show (no project yet): the panes stay.
    t.app.home_started(Ok(Reply::Home { home: Box::default() }));
    assert!(t.app.chat_panel.is_none());
    // An older daemon doesn't know home: the panes, and nothing is said.
    t.app.home_started(Err(ptyro_core::Error::new(ptyro_core::ErrorCode::Unsupported, "?")));
    assert!(t.app.chat_panel.is_none() && t.app.toast.is_none());
    t.app.home_started(Ok(Reply::Home { home: Box::new(view(DAY)) }));
    assert!(t.app.home_screen().is_some());
    assert!(screen(&t.app).contains("2 left hanging"), "{}", screen(&t.app));
    // An attach to a session, or a start that isn't a bare `ptyro`: nothing is asked.
    let mut t = with(Options::default(), Config::default());
    t.app.home_at_start();
    assert_eq!(started(&mut t.app), 0);
    let mut off = Config::default();
    off.ui.home_at_start = false;
    let mut t = with(Options { home: true, ..Options::default() }, off);
    t.app.home_at_start();
    assert_eq!(started(&mut t.app), 0);
}

#[test]
fn an_older_daemon_is_said_with_the_fix() {
    let mut t = t();
    t.app.toggle_home();
    t.app.home_got(Err(ptyro_core::Error::new(
        ptyro_core::ErrorCode::Unsupported,
        "unknown request",
    )));
    assert!(screen(&t.app).contains("ptyro daemon upgrade"), "{}", screen(&t.app));
}
