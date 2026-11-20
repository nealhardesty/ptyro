use ptyro_client::ConnectOptions;
use ptyro_core::config::Config;
use ptyro_core::kb::search::KbCount;
use ptyro_core::paths::Paths;
use ptyro_core::session::SessionInfo;
use ptyro_core::{Error, SessionId};
use ptyro_protocol::DomainEvent;
use serde_json::json;

use super::*;
use crate::Options;
use crate::app::AppMsg;
use crate::keymap::Command;

struct T {
    app: App,
    _home: tempfile::TempDir,
}

/// An app with its own home (the switch test writes its config), at `w` × `h`.
fn sized(w: u16, h: u16) -> T {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().to_path_buf();
    let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| dir.clone())).unwrap();
    let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
    let area = Rect::new(0, 0, w, h);
    let app = App::new(Options::default(), connect, paths, Config::default(), false, area);
    T { app, _home: home }
}

fn t() -> T {
    sized(140, 30)
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

/// What the TUI asked the daemon since the last call (not connected: each comes back).
fn asked(app: &mut App) -> Vec<Pending> {
    let mut out = Vec::new();
    while let Ok(m) = app.rx.try_recv() {
        if let AppMsg::Reply(p, _) = m {
            out.push(p);
        }
    }
    out
}

fn run(app: &mut App, action: &'static str) {
    app.run(&Command { action, arg: None });
}

/// `"jj<Enter>"`: a key per character, named keys in angle brackets.
fn press(app: &mut App, keys: &str) {
    let mut rest = keys;
    while !rest.is_empty() {
        let (name, n) = match rest.strip_prefix('<') {
            Some(r) => {
                let end = r.find('>').unwrap();
                (&r[..end], end + 2)
            }
            None => {
                let n = rest.chars().next().unwrap().len_utf8();
                (&rest[..n], n)
            }
        };
        let code = match name {
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            "Tab" => KeyCode::Tab,
            "BS" => KeyCode::Backspace,
            "Down" => KeyCode::Down,
            c => KeyCode::Char { c: c.chars().next().unwrap() },
        };
        // (Through the app's own routing: an overlay first, then the panel.)
        if app.overlay.is_some() {
            app.overlay_key(KeyEvent::new(code, Mods::NONE));
        } else {
            app.chat_key(KeyEvent::new(code, Mods::NONE));
        }
        rest = &rest[n..];
    }
}

fn lib(app: &App) -> &Library {
    app.library().expect("the Library is shown")
}

fn toast(app: &App) -> String {
    app.toast.as_ref().map(|t| t.text.clone()).unwrap_or_default()
}

fn status(enabled: bool) -> KbStatusInfo {
    KbStatusInfo {
        enabled,
        sessions: true,
        path: "/d/kb.db".into(),
        bytes: 4096,
        kinds: if enabled {
            vec![KbCount { kind: ItemKind::Session, items: 2, chunks: 7 }]
        } else {
            Vec::new()
        },
        ..KbStatusInfo::default()
    }
}

fn listed(handle: &str, title: &str, at_ms: u64, session: Option<&str>) -> KbListed {
    serde_json::from_value(json!({
        "addr": format!("k:{handle}"),
        "project_name": "feeder",
        "item": {
            "id": format!("01JB000000000000000000{handle}"), "uri": format!("session://claude/{handle}"),
            "kind": "session", "title": title, "project": "p1", "agent": "claude",
            "session": session, "writer": "agent", "chunks": 4, "ended_ms": at_ms,
            "origin": "m1",
        },
    }))
    .unwrap()
}

fn hit(handle: &str, idx: u32, title: &str, snippet: &str) -> KbHit {
    serde_json::from_value(json!({
        "addr": format!("k:{handle}/{idx}"), "item": format!("01JB000000000000000000{handle}"),
        "idx": idx, "uri": format!("session://claude/{handle}"), "kind": "session",
        "part": "user", "title": title, "snippet": snippet, "at_ms": now_ms() - 3 * 86_400_000,
        "project": "p1", "project_name": "feeder", "agent": "claude", "writer": "agent",
        "how": "words", "score": 1.0,
    }))
    .unwrap()
}

fn chat_hit() -> KbHit {
    serde_json::from_value(json!({
        "addr": "c:aaaa/01jbnode", "item": "aaaa", "node": "01jbnode", "kind": "chat",
        "part": "assistant", "title": "the retry chat",
        "snippet": format!("we said {MARK_ON}retry{MARK_OFF} three times"),
        "at_ms": now_ms(), "agent": "claude", "how": "meaning", "score": 0.5,
    }))
    .unwrap()
}

fn chunk(idx: u32, kind: &str, text: &str) -> KbChunk {
    serde_json::from_value(json!({
        "seq": idx, "item": "01JB0000000000000000007F3K", "idx": idx, "kind": kind,
        "text": text, "at_ms": 0,
    }))
    .unwrap()
}

fn doc(item: KbListed, at: Option<u32>) -> Reply {
    Reply::KbDoc {
        item,
        chunks: vec![
            chunk(0, "card", "Task: make the feeder retry"),
            chunk(1, "user", "add a retry policy to the feeder"),
            chunk(2, "tool", "Edit src/feeder.rs"),
            chunk(3, "assistant", "done: the feeder retries three times"),
        ],
        at,
        stale: false,
    }
}

fn reply(app: &mut App, p: Pending, r: Reply) {
    app.on_reply(AppMsg::Reply(p, Ok(r)));
}

/// Open the Library with the status and a history of two sessions answered.
fn opened(app: &mut App) {
    if app.library().is_none() {
        run(app, "kb.open");
    }
    let n = lib(app).asked;
    asked(app);
    reply(app, Pending::KbStatus, Reply::KbStatus { status: status(true) });
    let now = now_ms();
    let items = vec![
        listed("7F3K", "retry policy for the feeder", now, None),
        listed("8G4M", "the hopper jams", now - 86_400_000, None),
    ];
    reply(app, Pending::KbRows(n), Reply::KbItems { items });
}

/// Type a query and answer it with `hits`.
fn searched(app: &mut App, query: &str, hits: Vec<KbHit>, words_only: Option<&str>) {
    press(app, "/");
    press(app, query);
    let n = lib(app).asked;
    reply(
        app,
        Pending::KbRows(n),
        Reply::KbHits { hits, words_only: words_only.map(str::to_owned), left_out: Vec::new() },
    );
    press(app, "<Enter>");
    asked(app);
}

#[test]
fn it_has_a_key_and_a_place_in_the_palette_the_help_and_which_key() {
    let mut t = t();
    let app = &mut t.app;
    assert_eq!(app.keymap.key_for("kb.open").as_deref(), Some("C-q L"));
    assert_eq!(app.keymap.key_in(crate::keymap::Table::Nav, "kb.open").as_deref(), Some("L"));
    for a in ["kb.open", "kb.search", "kb.recent", "kb.history", "kb.show", "kb.forget"] {
        assert!(crate::keymap::spec(a).is_some(), "{a}");
    }
    for a in ["kb.status", "kb.enable"].iter().chain(ACTIONS) {
        assert!(crate::keymap::spec(a).is_some(), "{a}");
    }
    // None of them runs a command in a split any more: each is the Library's own.
    assert!(crate::cli_actions::TEMPLATES.iter().all(|(a, _)| !a.starts_with("kb.")));
    for a in ["kb.sources", "kb.ingest", "kb.backfill", "kb.remove"] {
        assert!(ACTIONS.contains(&a) && crate::keymap::spec(a).is_some(), "{a}");
    }
    app.overlay = Some(Overlay::Help { filter: "kb.open".into(), scroll: 0 });
    let shown = screen(app);
    assert!(shown.contains("C-q L") && shown.contains("The Library"), "{shown}");
    // The key again closes it.
    app.overlay = None;
    run(app, "kb.open");
    assert!(app.library().is_some() && app.focus == Focus::Pane);
    run(app, "kb.open");
    assert!(app.chat_panel.is_none());
}

#[test]
fn nothing_typed_is_the_history_by_day_with_the_selected_one_beside_it() {
    let mut t = t();
    let app = &mut t.app;
    run(app, "kb.open");
    let first = asked(app);
    assert!(first.iter().any(|p| matches!(p, Pending::KbStatus)), "{first:?}");
    assert!(first.iter().any(|p| matches!(p, Pending::KbRows(_))), "{first:?}");
    assert!(screen(app).contains("Looking…"));
    opened(app);
    // The first row's text was asked for: the item from its start.
    let then = asked(app);
    assert!(then.iter().any(|p| matches!(p, Pending::KbPreview(a) if a == "k:7F3K")), "{then:?}");
    let item = listed("7F3K", "retry policy for the feeder", now_ms(), None);
    reply(app, Pending::KbPreview("k:7F3K".into()), doc(item, None));
    let shown = screen(app);
    for want in [
        " Library ",
        " 1 Search ",
        " 2 Notes ",
        "2 sessions · 4.0 KiB",
        "/ search what was kept, and your chats",
        "kind: any",
        "project: any",
        "since: any",
        "wrote: any",
        " Today",
        " Yesterday",
        "session  retry policy for the feeder ",
        "feeder · claude · 4 pieces",
        "the hopper jams",
        // Beside the list: what it is, where it is from, and how it starts.
        "│",
        "k:7F3K · session://claude/7F3K",
        "Task: make the feeder retry",
        "add a retry policy to the feeder",
        // (The keys, on as many lines as they need.)
        "/ search · Tab filters · j/k move · ⏎ open · r its session · y address · Y text · d forget",
        "s status · Esc",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // The selection moves, and the other one's text is asked for.
    press(app, "j");
    assert_eq!(lib(app).sel, 1);
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbPreview(a) if a == "k:8G4M")));
    press(app, "jjk");
    assert_eq!(lib(app).sel, 0, "it stops at the last row");
    // Esc closes it.
    press(app, "<Esc>");
    assert!(app.chat_panel.is_none());
}

#[test]
fn what_is_asked_follows_the_box_and_the_filters() {
    let mut l = Library::new();
    // Nothing typed, no filter: the history, a page of it.
    assert_eq!(
        l.request(None),
        Some(Request::KbRecent {
            kinds: Vec::new(),
            project: None,
            session: None,
            worktree: None,
            since_ms: None,
            before_ms: None,
            limit: Some(100),
            machine: None,
            writer: None,
        })
    );
    // Two letters are not a search yet.
    l.query = "re".into();
    assert!(matches!(l.request(None), Some(Request::KbRecent { .. })));
    // The history under the filters: a hit's kind is its item's, since is a time.
    l.query.clear();
    (l.kind, l.since) = (Some(HitKind::Comment), Some("7d"));
    l.project = Some(("p1".into(), "feeder".into()));
    l.of = Some(Of::Worktree("w1".into(), "feeder/fix".into()));
    let Some(Request::KbRecent { kinds, project, worktree, session, since_ms, .. }) =
        l.request(None)
    else {
        panic!()
    };
    assert_eq!((kinds, project.as_deref()), (vec![ItemKind::File], Some("p1")));
    assert_eq!((worktree.as_deref(), session), (Some("w1"), None));
    let week = now_ms() - 7 * 86_400_000;
    assert!(since_ms.is_some_and(|s| s.abs_diff(week) < 5_000), "{since_ms:?}");
    l.of = Some(Of::Session("s1".into(), "brisk-rook".into()));
    assert!(
        matches!(l.request(None), Some(Request::KbRecent { session: Some(s), .. }) if s == "s1")
    );
    // A search: the box's words, `since:` in them, the kind and the project beside them, and
    // where you are. Within a row's history it asks for more (it keeps only that row's).
    l.query = "  retry policy ".into();
    assert_eq!(
        l.request(Some("p9".into())),
        Some(Request::KbSearch {
            query: "retry policy since:7d".into(),
            project: Some("p1".into()),
            kinds: vec![HitKind::Comment],
            since_ms: None,
            before_ms: None,
            limit: Some(100),
            here: Some("p9".into()),
            machines: true,
            machine: None,
            profile: None,
        })
    );
    l.of = None;
    assert!(matches!(l.request(None), Some(Request::KbSearch { limit: Some(30), .. })));
    // Chats have no history here.
    l.query.clear();
    l.kind = Some(HitKind::Chat);
    assert_eq!(l.request(None), None);
}

#[test]
fn a_search_lists_hits_with_what_they_are_and_says_when_it_was_by_words_alone() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    asked(app);
    // Typing: nothing is asked for two letters' difference from the history, then each
    // letter asks.
    press(app, "/re");
    assert!(lib(app).typing && screen(app).contains(" / re▏"));
    press(app, "t");
    let stale = lib(app).asked;
    press(app, "ry");
    let n = lib(app).asked;
    assert!(n > stale);
    // An answer to an older ask is dropped.
    let old = vec![hit("0000", 1, "an old answer", "old")];
    reply(
        app,
        Pending::KbRows(stale),
        Reply::KbHits { hits: old, words_only: None, left_out: Vec::new() },
    );
    assert!(matches!(lib(app).rows, Rows::History(_)));
    let hits = vec![
        hit(
            "7F3K",
            1,
            "retry policy for the feeder",
            &format!("add a {MARK_ON}retry{MARK_OFF} policy"),
        ),
        chat_hit(),
    ];
    let why = "meaning search is off: Knowledge → meaning switches it on";
    reply(
        app,
        Pending::KbRows(n),
        Reply::KbHits { hits, words_only: Some(why.into()), left_out: Vec::new() },
    );
    press(app, "<Enter>");
    assert!(!lib(app).typing);
    let shown = screen(app);
    for want in [
        " / retry",
        "session  retry policy for the feeder ",
        "retry policy for the feeder  3d",
        "feeder · claude · add a retry policy",
        "chat     the retry chat ",
        // ≈: found by meaning.
        "claude · ≈ we said retry three times",
        "(meaning search is off: Knowledge → meaning",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // The piece that matched, among its neighbours, the match marked.
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbPreview(a) if a == "k:7F3K/1")));
    let item = listed("7F3K", "retry policy for the feeder", now_ms(), None);
    reply(app, Pending::KbPreview("k:7F3K/1".into()), doc(item, Some(1)));
    let shown = screen(app);
    assert!(
        shown.contains("▶ user") && shown.contains("add a retry policy to the feeder"),
        "{shown}"
    );
    // A chat's hit shows its snippet, and how to open it.
    press(app, "j");
    assert!(screen(app).contains("⏎ opens the chat at this message"));
    // What the daemon says of a query instead of answering is said under the box.
    press(app, "/*");
    let n = lib(app).asked;
    app.on_reply(AppMsg::Reply(
        Pending::KbRows(n),
        Err(Error::invalid("a prefix needs at least 3 letters (`migr*`)")),
    ));
    let shown = screen(app);
    assert!(
        shown.contains("a prefix needs at least 3 letters") && !shown.contains("the retry chat")
    );
    // Esc empties the box: the history again.
    press(app, "<Esc>");
    assert!(lib(app).query.is_empty() && !lib(app).typing);
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
}

#[test]
fn switched_off_it_says_so_and_enter_switches_it_on() {
    let mut t = t();
    let app = &mut t.app;
    app.config.kb.enabled = false;
    run(app, "kb.open");
    let n = lib(app).asked;
    // The list's ask is refused; the status says the rest.
    app.on_reply(AppMsg::Reply(
        Pending::KbRows(n),
        Err(Error::new(ErrorCode::Unsupported, "the knowledge base is off: switch it on")),
    ));
    assert!(lib(app).off());
    reply(app, Pending::KbStatus, Reply::KbStatus { status: status(false) });
    let shown = screen(app);
    for want in [
        "The knowledge base is off.",
        "Nothing is kept and nothing is searched.",
        "What was kept before stays in /d/kb.db (4.0 KiB).",
        "⏎ switch it on: agent sessions are kept from then on",
        "Esc closes",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    assert!(!shown.contains("kind: any"), "no search to offer:\n{shown}");
    // Keys that would search do nothing; ⏎ writes the setting, as the panel would.
    press(app, "/x");
    assert!(lib(app).query.is_empty());
    asked(app);
    press(app, "<Enter>");
    assert!(app.config.kb.enabled, "{}", toast(app));
    let file = std::fs::read_to_string(Config::path(&app.paths)).unwrap();
    assert!(file.contains("[kb]") && file.contains("enabled = true"), "{file}");
    assert!(toast(app).contains("the knowledge base is on"));
    // It asks again; on, the panel is the Library.
    let again = asked(app);
    assert!(again.iter().any(|p| matches!(p, Pending::KbStatus)), "{again:?}");
    let n = lib(app).asked;
    reply(
        app,
        Pending::KbStatus,
        Reply::KbStatus { status: KbStatusInfo { kinds: Vec::new(), ..status(true) } },
    );
    reply(app, Pending::KbRows(n), Reply::KbItems { items: Vec::new() });
    let shown = screen(app);
    assert!(shown.contains("nothing is kept yet: agent sessions are, as"), "{shown}");
    run(app, "kb.enable");
    assert!(toast(app).contains("on already"));
}

#[test]
fn a_file_that_cannot_be_opened_says_why() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    let broken = KbStatusInfo {
        error: Some("the knowledge base is unavailable: move /d/kb.db aside".into()),
        ..status(true)
    };
    reply(app, Pending::KbStatus, Reply::KbStatus { status: broken });
    let shown = screen(app);
    assert!(shown.contains("The knowledge base can't be read."), "{shown}");
    assert!(shown.contains("move /d/kb.db aside") && !shown.contains("switch it on"), "{shown}");
}

#[test]
fn forgetting_asks_in_the_daemons_words_then_says_what_went() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    asked(app);
    press(app, "d");
    // Asked, not done: the request is unconfirmed, by the item's address.
    let sent = asked(app);
    let Some(Pending::Confirm(req, then)) =
        sent.into_iter().find(|p| matches!(p, Pending::Confirm(..)))
    else {
        panic!("nothing asked")
    };
    assert_eq!(
        *req,
        Request::KbForget {
            addrs: vec!["k:7F3K".into()],
            query: None,
            confirmed: false,
            session: None
        }
    );
    let words = "Forget 1 item: session “retry policy for the feeder”? It is deleted from the knowledge base for good";
    app.on_reply(AppMsg::Reply(
        Pending::Confirm(req, then),
        Err(Error::new(ErrorCode::NeedsConfirmation, words)),
    ));
    let shown = screen(app);
    assert!(shown.contains("Forget 1 item: session “retry policy for the feeder”?"), "{shown}");
    assert!(shown.contains("[y/N]") && app.library().is_some(), "over the Library:\n{shown}");
    // No: nothing is sent, and the Library is as it was.
    press(app, "n");
    assert!(app.overlay.is_none() && asked(app).is_empty());
    assert_eq!(lib(app).rows.len(), 2);
    // Yes: sent again, agreed to; then said, and the list asked for again.
    press(app, "d");
    let Some(Pending::Confirm(req, then)) =
        asked(app).into_iter().find(|p| matches!(p, Pending::Confirm(..)))
    else {
        panic!()
    };
    app.on_reply(AppMsg::Reply(
        Pending::Confirm(req, then),
        Err(Error::new(ErrorCode::NeedsConfirmation, words)),
    ));
    press(app, "y");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbForgotten)));
    reply(app, Pending::KbForgotten, Reply::KbForgotten { items: 1, chunks: 4 });
    assert_eq!(toast(app), "forgot 1 item (4 pieces): deleted for good, and never kept again");
    let again = asked(app);
    assert!(again.iter().any(|p| matches!(p, Pending::KbRows(_))), "{again:?}");
    // A chat is not the knowledge base's to forget.
    searched(app, "retry", vec![chat_hit()], None);
    press(app, "d");
    assert!(toast(app).contains("that is a chat"), "{}", toast(app));
    assert!(asked(app).iter().all(|p| !matches!(p, Pending::Confirm(..))));
}

#[test]
fn enter_reads_the_item_in_the_reader_and_slash_finds_in_it() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    let snippet = format!("add a {MARK_ON}retry{MARK_OFF} policy");
    searched(app, "retry", vec![hit("7F3K", 1, "retry policy for the feeder", &snippet)], None);
    press(app, "<Enter>");
    // The whole item, to be shown at the piece that matched.
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbReader(Some(1)))));
    let item = listed("7F3K", "retry policy for the feeder", now_ms(), None);
    reply(app, Pending::KbReader(Some(1)), doc(item, None));
    let shown = screen(app);
    for want in [
        " retry policy for the feeder ",
        "session · feeder · claude · ",
        "k:7F3K",
        "› add a retry policy to the feeder",
        "Edit src/feeder.rs",
        "done: the feeder retries three times",
        "read only · j/k scroll · / find · r its session · y address · Y text · d forget · Esc the list",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // Nothing is typed into it: no composer, no search box.
    assert!(!shown.contains("type a message") && !shown.contains("kind: any"), "{shown}");
    let r = lib(app).reader.as_ref().unwrap();
    assert_eq!(r.view.highlight.as_deref(), Some("retry"), "the match is highlighted");
    // `/` finds in it; n goes on.
    press(app, "/");
    assert!(matches!(&app.overlay, Some(Overlay::Prompt { kind: PromptKind::KbFind, .. })));
    press(app, "feeder<Enter>");
    assert!(toast(app).starts_with("1/4 · n next"), "{}", toast(app));
    press(app, "n");
    assert_eq!(lib(app).reader.as_ref().unwrap().found_at, 1);
    press(app, "/");
    press(app, "zebra<Enter>");
    assert!(toast(app).contains("not in it"));
    // y and Y copy; scrolling keys don't leave it; Esc goes back to the hits.
    press(app, "y");
    assert_eq!(toast(app), "copied its address");
    press(app, "Y");
    assert_eq!(toast(app), "copied its text");
    press(app, "jkgG");
    assert!(lib(app).reader.is_some());
    press(app, "<Esc>");
    assert!(lib(app).reader.is_none() && matches!(lib(app).rows, Rows::Hits(_)));
    // `kb.show` asks for an address when nothing is selected to open.
    run(app, "kb.open");
    run(app, "kb.show");
    let Some(Overlay::Prompt { kind: PromptKind::KbShow, .. }) = &app.overlay else {
        panic!("asks for an address")
    };
    press(app, "k:8g4m/2<Enter>");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbReader(Some(2)))));
}

fn chat_info(id: &str) -> ptyro_core::chats::ChatInfo {
    serde_json::from_value(json!({
        "id": id, "handle": id, "workspace": "main", "title": format!("chat {id}"),
        "title_src": "user", "agent": "claude", "profile": "default", "tools": "chat",
        "pinned": false, "archived": false, "incognito": false,
        "nodes": 2, "unread": 0, "created_ms": 0, "last_ms": 0, "cost_micro_usd": 0,
    }))
    .unwrap()
}

#[test]
fn a_chats_hit_opens_the_chat_at_its_message_and_esc_comes_back() {
    let mut t = t();
    let app = &mut t.app;
    app.chat_list = vec![chat_info("aaaa")];
    opened(app);
    searched(app, "retry", vec![chat_hit()], None);
    press(app, "<Enter>");
    let Some(ChatPanel::View(v)) = &app.chat_panel else { panic!("the chat") };
    assert_eq!((v.id.as_deref(), v.focus.as_deref()), (Some("aaaa"), Some("01jbnode")));
    // Esc from the chat: the Library, as it was.
    press(app, "<Esc>");
    assert_eq!(lib(app).query, "retry");
    assert!(matches!(&lib(app).rows, Rows::Hits(h) if h.len() == 1));
}

fn session(name: &str, state: ProcessState) -> SessionInfo {
    serde_json::from_value(json!({
        "id": SessionId::new(), "handle": name, "name": name, "kind": "agent",
        "argv": [], "cwd": "/", "state": state,
        "size": {"cols": 80, "rows": 24}, "created_ms": 0,
    }))
    .unwrap()
}

#[test]
fn r_goes_to_the_session_it_happened_in_and_backtick_comes_back() {
    let mut t = t();
    let app = &mut t.app;
    let s = session("brisk-rook", ProcessState::Dormant);
    app.sessions.push(s.clone());
    opened(app);
    asked(app);
    press(app, "r");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbResume)));
    // Its session is still in ptyro, dormant: resumed and shown, the Library put aside.
    let id = s.id.full().to_uppercase();
    let item = listed("7F3K", "retry policy for the feeder", now_ms(), Some(&id));
    reply(app, Pending::KbResume, doc(item, None));
    assert!(asked(app).iter().any(|p| matches!(p, Pending::Done(d) if d == "resumed")));
    assert_eq!(app.focused_session(), Some(s.id));
    assert!(app.chat_panel.is_none() && matches!(app.chat_aside, Some(ChatPanel::Library(_))));
    run(app, "session.last");
    assert_eq!(lib(app).rows.len(), 2, "the Library is back as it was");
    // A session ptyro no longer has: said, with what can still be done.
    press(app, "r");
    let gone = listed("7F3K", "retry policy for the feeder", now_ms(), Some("01jb0000gone"));
    reply(app, Pending::KbResume, doc(gone, None));
    assert!(
        toast(app).contains("no longer in ptyro: ⏎ reads what was kept of it"),
        "{}",
        toast(app)
    );
    assert!(app.library().is_some());
}

/// KB4: a shell's day goes to the shell its commands ran in, while ptyro still has it.
#[test]
fn r_on_a_shells_commands_goes_to_that_shell() {
    let mut t = t();
    let app = &mut t.app;
    let s = session("zsh-2", ProcessState::Running);
    app.sessions.push(s.clone());
    opened(app);
    asked(app);
    let day = |session: &str| {
        let mut item = listed("7F3K", "zsh-2 · 2026-11-20", now_ms(), Some(session));
        item.item.kind = ItemKind::Shell;
        item.item.uri = format!("shell://{session}/2026-11-20");
        item
    };
    press(app, "r");
    reply(app, Pending::KbResume, doc(day(&s.id.full()), None));
    assert_eq!(app.focused_session(), Some(s.id));
    assert!(!asked(app).iter().any(|p| matches!(p, Pending::Done(_))), "a shell is not resumed");
    run(app, "session.last");
    press(app, "r");
    reply(app, Pending::KbResume, doc(day("01jb0000gone"), None));
    assert_eq!(
        toast(app),
        "the shell those commands ran in is no longer in ptyro: ⏎ reads what was kept of them"
    );
}

#[test]
fn tab_reaches_the_filters_and_each_is_a_pick() {
    let mut t = t();
    let app = &mut t.app;
    app.projects = vec![
        serde_json::from_value(
            json!({"id": "p1", "slug": "feeder", "root": "/r/feeder", "created_ms": 0}),
        )
        .unwrap(),
    ];
    opened(app);
    asked(app);
    press(app, "<Tab>");
    assert_eq!(lib(app).chip, Some(Filter::Kind));
    assert!(screen(app).contains("←/→ or Tab the filters · ⏎ pick"));
    press(app, "<Enter>");
    // The kinds on offer: sessions and chats, and nothing that isn't kept yet.
    let Some(Overlay::Picker { palette, purpose: Purpose::Library(Filter::Kind) }) = &app.overlay
    else {
        panic!("a pick")
    };
    let offered: Vec<&str> = (0..3).map(|i| palette.item(i).label.as_str()).collect();
    assert_eq!(offered, ["any", "session", "chat"]);
    assert_eq!(palette.filtered.len(), 3, "no notes, documents or commits until they are kept");
    press(app, "session<Enter>");
    assert_eq!(lib(app).kind, Some(HitKind::Session));
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))), "asked again");
    // On to the project, then since.
    press(app, "<Tab>");
    assert_eq!(lib(app).chip, Some(Filter::Project));
    press(app, "<Enter>");
    assert!(screen(app).contains("/r/feeder"));
    press(app, "feeder<Enter>");
    assert_eq!(lib(app).project, Some(("p1".into(), "feeder".into())));
    press(app, "<Tab><Enter>");
    press(app, "7d<Enter>");
    assert_eq!(lib(app).since, Some("7d"));
    let shown = screen(app);
    for want in ["kind: session", "project: feeder", "since: 7d"] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // Backspace clears the one it is on; `any` clears by the pick.
    press(app, "<BS>");
    assert_eq!(lib(app).since, None);
    app.picked(Target::Path("any".into()), Purpose::Library(Filter::Kind));
    assert_eq!(lib(app).kind, None);
    // Who wrote it: you, or an agent. The history is asked for that writer's; a search says
    // it in the query's own word.
    press(app, "<Tab>");
    assert_eq!(lib(app).chip, Some(Filter::Wrote));
    press(app, "<Enter>");
    let Some(Overlay::Picker { palette, purpose: Purpose::Library(Filter::Wrote) }) = &app.overlay
    else {
        panic!("a pick")
    };
    let offered: Vec<&str> = (0..3).map(|i| palette.item(i).label.as_str()).collect();
    assert_eq!(offered, ["any", "you", "agent"]);
    press(app, "you<Enter>");
    assert_eq!(lib(app).wrote, Some(Writer::User));
    assert!(screen(app).contains("wrote: you"));
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbRecent { writer: Some(Writer::User), .. })
    ));
    lib_mut(app).query = "retry".into();
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbSearch { query, .. }) if query == "retry wrote:you"
    ));
    lib_mut(app).query.clear();
    app.picked(Target::Path("agent".into()), Purpose::Library(Filter::Wrote));
    assert_eq!(lib(app).wrote, Some(Writer::Agent));
    press(app, "<BS>");
    assert_eq!(lib(app).wrote, None);
    // Tab past the last one is the list again.
    press(app, "<Tab>");
    assert_eq!(lib(app).chip, None);
    // Chats have no history here: said, not asked.
    asked(app);
    app.picked(Target::Path("chat".into()), Purpose::Library(Filter::Kind));
    assert!(asked(app).is_empty());
    assert!(screen(app).contains("chats are in the chat list: type to search"));
}

#[test]
fn a_rows_history_is_the_library_with_only_that() {
    let mut t = t();
    let app = &mut t.app;
    let s = session("brisk-rook", ProcessState::Running);
    app.sessions.push(s.clone());
    app.projects = vec![
        serde_json::from_value(
            json!({"id": "p1", "slug": "feeder", "root": "/r/feeder", "created_ms": 0}),
        )
        .unwrap(),
    ];
    app.worktrees = vec![
        serde_json::from_value(json!({
            "id": "w1", "project": "p1", "project_slug": "feeder", "name": "fix", "path": "/r/fix",
            "is_root": false, "state": "ready", "created_ms": 0,
        }))
        .unwrap(),
    ];
    // In each one's menu.
    for row in [Row::Session(s.id), Row::Worktree("w1".into()), Row::Project("p1".into())] {
        assert!(app.row_actions(&row).contains(&"kb.history"), "{row:?}");
    }
    app.library_of(Some(Row::Session(s.id)));
    assert_eq!(lib(app).of, Some(Of::Session(s.id.full(), "brisk-rook".into())));
    let n = lib(app).asked;
    reply(app, Pending::KbRows(n), Reply::KbItems { items: Vec::new() });
    let shown = screen(app);
    assert!(shown.contains(" only session brisk-rook "), "{shown}");
    assert!(shown.contains("nothing is kept of session brisk-rook"), "{shown}");
    // A search within it keeps only what is that session's.
    let items = vec![listed("7F3K", "retry policy for the feeder", now_ms(), None)];
    reply(app, Pending::KbRows(n), Reply::KbItems { items });
    let hits = vec![hit("7F3K", 1, "retry policy", "retry"), hit("9999", 1, "another's", "retry")];
    searched(app, "retry", hits, None);
    assert!(matches!(&lib(app).rows, Rows::Hits(h) if h.len() == 1 && h[0].addr == "k:7F3K/1"));
    // Backspace: everything again.
    assert!(screen(app).contains("Backspace everything"));
    press(app, "<BS>");
    assert_eq!(lib(app).of, None);
    // A worktree's, and a project's (which is the project filter).
    app.library_of(Some(Row::Worktree("w1".into())));
    assert_eq!(lib(app).of, Some(Of::Worktree("w1".into(), "feeder/fix".into())));
    assert!(lib(app).query.is_empty(), "the history, not the last search");
    app.library_of(Some(Row::Project("p1".into())));
    assert_eq!(
        (lib(app).of.clone(), lib(app).project.clone()),
        (None, Some(("p1".into(), "feeder".into())))
    );
    // Nothing to be the history of.
    app.close_chat();
    app.library_of(None);
    assert!(app.chat_panel.is_none() && toast(app).contains("pick a session"));
}

#[test]
fn news_of_a_change_asks_again_and_the_selection_stays_on_its_row() {
    let mut t = t();
    let app = &mut t.app;
    // Not shown: nothing is asked.
    app.on_server(ptyro_protocol::ServerMsg::Event {
        event: DomainEvent::KbUpdated { kinds: vec![ItemKind::Session] },
    });
    assert!(asked(app).is_empty());
    opened(app);
    press(app, "j");
    asked(app);
    app.on_server(ptyro_protocol::ServerMsg::Event {
        event: DomainEvent::KbUpdated { kinds: vec![ItemKind::Session] },
    });
    let again = asked(app);
    assert!(again.iter().any(|p| matches!(p, Pending::KbStatus)), "{again:?}");
    assert!(again.iter().any(|p| matches!(p, Pending::KbRows(_))), "{again:?}");
    // A new session came in above: the selected one is still selected.
    let now = now_ms();
    let items = vec![
        listed("9Z9Z", "a new session", now, None),
        listed("7F3K", "retry policy for the feeder", now, None),
        listed("8G4M", "the hopper jams", now - 86_400_000, None),
    ];
    let n = lib(app).asked;
    reply(app, Pending::KbRows(n), Reply::KbItems { items });
    assert_eq!(lib(app).sel, 2);
    assert!(screen(app).contains("a new session"));
}

#[test]
fn the_status_is_the_commands_words() {
    let mut t = t();
    let app = &mut t.app;
    run(app, "kb.status");
    assert!(lib(app).show_status);
    let n = lib(app).asked;
    reply(app, Pending::KbStatus, Reply::KbStatus { status: status(true) });
    reply(app, Pending::KbRows(n), Reply::KbItems { items: Vec::new() });
    let shown = screen(app);
    for want in [
        "knowledge base: on · /d/kb.db (4.0 KiB)",
        "2 sessions · 7 pieces",
        "sessions are kept for good",
        "s the list",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    press(app, "s");
    assert!(!lib(app).show_status);
}

#[test]
fn a_narrow_terminal_has_the_list_alone_and_a_tiny_one_draws_nothing_wrong() {
    // 70 columns with the sidebar: the list alone, hits with their snippets.
    let mut t = sized(70, 20);
    let app = &mut t.app;
    opened(app);
    let snippet = format!("add a {MARK_ON}retry{MARK_OFF} policy");
    searched(
        app,
        "retry",
        vec![hit("7F3K", 1, "retry policy for the feeder", &snippet)],
        Some("by words alone"),
    );
    let shown = screen(app);
    assert!(!shown.contains('│') || app.sidebar, "{shown}");
    for want in
        ["Library", " / retry", "session  retry policy", "add a retry policy", "(by words alone)"]
    {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // The status takes the list's place there.
    press(app, "s");
    reply(app, Pending::KbStatus, Reply::KbStatus { status: status(true) });
    let shown = screen(app);
    assert!(
        shown.contains("knowledge base: on") && !shown.contains("session  retry policy"),
        "{shown}"
    );
    // The reader fits too.
    press(app, "s<Enter>");
    let item = listed("7F3K", "retry policy for the feeder", now_ms(), None);
    reply(app, Pending::KbReader(Some(1)), doc(item, None));
    assert!(screen(app).contains("› add a retry policy to the feeder"));
    // Hardly any room: nothing panics, whatever is shown.
    for (w, h) in [(24, 5), (12, 3), (40, 8), (200, 60)] {
        let mut t = sized(w, h);
        opened(&mut t.app);
        screen(&t.app);
        run(&mut t.app, "kb.status");
        screen(&t.app);
        t.app.config.kb.enabled = false;
        reply(&mut t.app, Pending::KbStatus, Reply::KbStatus { status: status(false) });
        screen(&t.app);
    }
}

#[test]
fn a_click_selects_the_wheel_moves_and_a_paste_is_a_search() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    let shown = screen(app);
    let y = shown.lines().position(|l| l.contains("the hopper jams")).unwrap() as u16;
    app.library_mouse(y, None);
    assert_eq!(lib(app).sel, 1);
    app.library_mouse(0, Some(true));
    assert_eq!(lib(app).sel, 0);
    app.library_mouse(0, Some(false));
    assert_eq!(lib(app).sel, 1);
    asked(app);
    app.on_paste("retry\npolicy".into());
    assert_eq!(lib(app).query, "retry policy");
    assert!(lib(app).typing && asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
}

#[test]
fn days_and_matches() {
    const DAY: u64 = 86_400_000;
    // 2026-11-19 is a Thursday.
    let now = 1_795_046_400_000 + 20 * 3_600_000;
    assert_eq!(minute(now), "2026-11-19 20:00");
    assert_eq!(day(now - 3_600_000, now), "Today");
    assert_eq!(day(now - DAY, now), "Yesterday");
    assert_eq!(day(now - 2 * DAY, now), "Tue 2026-11-17");
    assert_eq!(day(now - 4 * DAY, now), "Sun 2026-11-15");
    let snippet = format!(
        "a {MARK_ON}Retry{MARK_OFF} and a {MARK_ON}retry{MARK_OFF} {MARK_ON}policy{MARK_OFF}"
    );
    assert_eq!(matched(&snippet), ["retry", "policy"]);
    // A stored piece as the chat view draws it: a tool's name apart from what it did.
    let ChatBody::Tool { name, title, .. } =
        chat_item(&chunk(2, "tool", "Edit src/feeder.rs")).body
    else {
        panic!()
    };
    assert_eq!((name.as_str(), title.as_str()), ("Edit", "src/feeder.rs"));
    assert!(matches!(chat_item(&chunk(0, "card", "Task: x")).body, ChatBody::Notice { .. }));
}

/// KB1.12 (SPEC §25.11): with a linked machine there is a fourth filter. A search asks every
/// machine unless one is picked; a hit of another machine says which, is read and forgotten by
/// its address there; and a machine that didn't answer is named under the hits.
#[test]
fn with_a_linked_machine_the_search_asks_them_all_and_a_filter_picks_one() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    // No machine linked: four filters, and Tab past the fourth is the list.
    assert!(!screen(app).contains("machine:"));
    press(app, "<Tab><Tab><Tab><Tab><Tab>");
    assert_eq!(lib(app).chip, None);
    app.machines.push(
        serde_json::from_value(json!({
            "name": "laptop2", "transport": { "t": "ssh", "target": "laptop2" },
            "state": "online", "added_ms": 0,
        }))
        .unwrap(),
    );
    assert!(screen(app).contains("machine: any"));
    assert!(matches!(lib(app).request(None), Some(Request::KbRecent { machine: None, .. })));
    // A search: every machine. laptop2's hit carries its name, and one that was down is said.
    let mut far = hit("9H5N", 2, "the far retry", &format!("a far {MARK_ON}retry{MARK_OFF}"));
    (far.addr, far.machine) = ("laptop2:k:9H5N/2".into(), Some("laptop2".into()));
    press(app, "/retry");
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbSearch { machines: true, machine: None, .. })
    ));
    let n = lib(app).asked;
    let left_out = vec!["laptop3 is offline since 10:02".to_owned()];
    reply(app, Pending::KbRows(n), Reply::KbHits { hits: vec![far], words_only: None, left_out });
    press(app, "<Enter>");
    let shown = screen(app);
    assert!(shown.contains("laptop2 · feeder · claude"), "{shown}");
    assert!(shown.contains("(not searched: laptop3 is offline since 10:02)"), "{shown}");
    // Its text is asked for by its address there, and so is forgetting it.
    assert!(
        asked(app).iter().any(|p| matches!(p, Pending::KbPreview(a) if a == "laptop2:k:9H5N/2"))
    );
    assert_eq!(lib(app).selected(), Some(("laptop2:k:9H5N/2".into(), "laptop2:k:9H5N".into())));
    // The fifth filter: this computer, or one machine.
    press(app, "<Tab><Tab><Tab><Tab><Tab>");
    assert_eq!(lib(app).chip, Some(Filter::Machine));
    press(app, "<Enter>");
    let Some(Overlay::Picker { palette, purpose: Purpose::Library(Filter::Machine) }) =
        &app.overlay
    else {
        panic!("a pick")
    };
    let offered: Vec<&str> = (0..3).map(|i| palette.item(i).label.as_str()).collect();
    assert_eq!(offered, ["any", "here", "laptop2"]);
    press(app, "laptop2<Enter>");
    assert!(screen(app).contains("machine: laptop2"));
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbSearch { machines: false, machine: Some(m), .. }) if m == "laptop2"
    ));
    // Its history, with nothing typed; `here` is this computer's own.
    lib_mut(app).query.clear();
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbRecent { machine: Some(m), .. }) if m == "laptop2"
    ));
    app.picked(Target::Path("here".into()), Purpose::Library(Filter::Machine));
    assert!(matches!(lib(app).request(None), Some(Request::KbRecent { machine: None, .. })));
    lib_mut(app).query = "retry".into();
    assert!(matches!(
        lib(app).request(None),
        Some(Request::KbSearch { machines: false, machine: Some(m), .. }) if m == "here"
    ));
    // Backspace on it clears it.
    assert_eq!(lib(app).chip, Some(Filter::Machine));
    press(app, "<BS>");
    assert_eq!(lib(app).machine, None);
}

fn lib_mut(app: &mut App) -> &mut Library {
    app.library_mut().expect("the Library is shown")
}

/// A note as a list has it: yours unless an agent's, about `project` (`None`: global).
fn note(handle: &str, title: &str, project: Option<&str>, agent: Option<&str>) -> KbListed {
    serde_json::from_value(json!({
        "addr": format!("k:{handle}"),
        "project_name": project,
        "item": {
            "id": format!("01JB000000000000000000{handle}"),
            "uri": format!("note://_all/{}/{handle}", project.unwrap_or("_global")),
            "kind": "note", "title": title, "project": project.map(|_| "p1"), "agent": agent,
            "writer": if agent.is_some() { "agent" } else { "user" }, "chunks": 1,
            "ended_ms": now_ms() - 3 * 86_400_000, "origin": "m1",
        },
    }))
    .unwrap()
}

/// KB2 (SPEC §25.10): the Notes tab. The notes by what they are about, each with who wrote
/// it; `n` opens the note form with every option the command takes; `⏎` edits the note in
/// your editor (the command, in a split); `x` deletes it through the daemon's question; `/`
/// searches the notes.
#[test]
fn the_notes_tab_lists_by_project_and_writes_edits_and_deletes() {
    let mut t = t();
    let app = &mut t.app;
    app.projects = vec![
        serde_json::from_value(
            json!({"id": "p1", "slug": "feeder", "root": "/r/feeder", "created_ms": 0}),
        )
        .unwrap(),
    ];
    opened(app);
    let mut st = status(true);
    st.notes = "/d/kb/notes".into();
    reply(app, Pending::KbStatus, Reply::KbStatus { status: st });
    asked(app);
    press(app, "2");
    assert_eq!(lib(app).tab, Tab::Notes);
    assert_eq!(lib(app).request(None), Some(Request::KbNotes { project: None, writer: None }));
    assert!(screen(app).contains("Looking…"));
    let n = lib(app).asked;
    // (As the daemon lists them, newest first: the panel puts them under what they are about.)
    let items = vec![
        note("AAAA", "Hopper jams", Some("feeder"), Some("claude@work")),
        note("BBBB", "We deploy with make ship", None, None),
        note("CCCC", "Belt tension", Some("feeder"), None),
    ];
    reply(app, Pending::KbRows(n), Reply::KbItems { items });
    let shown = screen(app);
    for want in [
        " 2 Notes ",
        "notes are Markdown files in /d/kb/notes",
        "project: any",
        "wrote: any",
        " global (about no project)",
        "note     We deploy with make ship ",
        "you · 1 piece",
        " feeder",
        "note     Hopper jams ",
        "claude@work · 1 piece",
        "n new note · ⏎ edit in your editor · x delete · / search them",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    assert!(!shown.contains("kind: any") && !shown.contains("since: any"), "{shown}");
    let order: Vec<usize> = ["We deploy", "Hopper jams", "Belt tension"]
        .iter()
        .map(|t| shown.find(t).unwrap())
        .collect();
    assert!(order[0] < order[1] && order[1] < order[2], "{shown}");
    // The selected one's text is asked for, as in the history.
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbPreview(a) if a == "k:BBBB")));

    // Its two filters: the project, and who wrote it.
    press(app, "<Tab>");
    assert_eq!(lib(app).chip, Some(Filter::Project));
    press(app, "<Tab><Enter>agent<Enter>");
    assert_eq!(
        lib(app).request(None),
        Some(Request::KbNotes { project: None, writer: Some(Writer::Agent) })
    );
    press(app, "<BS><Tab>");
    assert_eq!((lib(app).wrote, lib(app).chip), (None, None));

    // `n`: the note form, every option, the project you are in already picked when there is one.
    press(app, "n");
    let Some(Overlay::Form(form)) = &app.overlay else { panic!("the note form") };
    assert_eq!(form.title, "Write a note");
    let keys: Vec<&str> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["text", "title", "project", "profile", "tags", "edit"]);
    let mut form = form.clone();
    let base = vec!["kb".to_owned(), "note".to_owned()];
    assert_eq!(form.cli_args(&base), ["kb", "note"], "nothing typed: your editor");
    for (key, value) in [("text", "we chose X because Y"), ("title", "X"), ("tags", "db, decision")]
    {
        let f = form.fields.iter_mut().find(|f| f.key == key).unwrap();
        *f = f.clone().with(value);
    }
    for (key, pick) in [("project", "feeder"), ("edit", "yes")] {
        let f = form.fields.iter_mut().find(|f| f.key == key).unwrap();
        let crate::form::Kind::Choice { options, sel } = &mut f.kind else { panic!("a pick") };
        *sel = options.iter().position(|o| o.0 == pick).unwrap();
    }
    assert_eq!(
        form.cli_args(&base),
        [
            "kb",
            "note",
            "we chose X because Y",
            "--title",
            "X",
            "--project",
            "feeder",
            "--tag",
            "db",
            "--tag",
            "decision",
            "--edit"
        ]
    );
    press(app, "<Esc>");

    // `⏎`: the note in your editor, by the command that edits one.
    press(app, "j<Enter>");
    assert_eq!(
        app.last_cli.as_deref(),
        Some(&["kb", "show", "k:AAAA", "--open"].map(str::to_owned)[..])
    );
    // `x`: forgotten, which for a note deletes its file; the daemon asks in its words.
    press(app, "x");
    let sent = asked(app);
    assert!(
        sent.iter().any(|p| matches!(p, Pending::Confirm(req, _)
            if matches!(&**req, Request::KbForget { addrs, confirmed: false, .. } if addrs == &["k:AAAA".to_owned()]))),
        "{sent:?}"
    );

    // `/`: the search, for notes; `1` and `2` go between the tabs.
    press(app, "/");
    assert_eq!(
        (lib(app).tab, lib(app).kind, lib(app).typing),
        (Tab::Search, Some(HitKind::Note), true)
    );
    press(app, "<Esc>2");
    assert_eq!(lib(app).tab, Tab::Notes);
    press(app, "1");
    assert_eq!(lib(app).tab, Tab::Search);
    assert!(matches!(lib(app).request(None), Some(Request::KbRecent { .. })));

    // No notes yet: how one is written.
    press(app, "2");
    let n = lib(app).asked;
    reply(app, Pending::KbRows(n), Reply::KbItems { items: Vec::new() });
    assert!(screen(app).contains("no notes yet: n writes one"), "{}", screen(app));
}

/// The actions: `kb.notes` opens the Library on its notes, `kb.note` the form, `kb.sync`
/// reads the folder again and says what it found; a note found by a search is edited by `⏎`.
#[test]
fn the_note_actions_and_a_notes_hit() {
    let mut t = t();
    let app = &mut t.app;
    run(app, "kb.notes");
    assert_eq!(lib(app).tab, Tab::Notes);
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
    run(app, "kb.note");
    assert!(matches!(&app.overlay, Some(Overlay::Form(f)) if f.title == "Write a note"));
    press(app, "<Esc>");
    run(app, "kb.sync");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbSynced)));
    reply(app, Pending::KbSynced, Reply::KbSynced { notes: 3, changed: 1, removed: 0, roots: 0 });
    assert_eq!(toast(app), "3 notes in the folder: 1 read again, 0 gone");
    for action in ["kb.notes", "kb.note", "kb.sync"] {
        assert!(ACTIONS.contains(&action), "{action} is in the Library's menu");
        assert!(crate::keymap::ACTIONS.iter().any(|a| a.id == action), "{action} has its words");
    }
    // `kb.search` is the Search tab again.
    run(app, "kb.search");
    assert_eq!((lib(app).tab, lib(app).typing), (Tab::Search, true));
    press(app, "<Esc>");
    let mut found = hit("AAAA", 0, "Hopper jams", &format!("the {MARK_ON}hopper{MARK_OFF} jams"));
    found.kind = HitKind::Note;
    searched(app, "hopper", vec![found], None);
    assert!(screen(app).contains("note     Hopper jams "));
    press(app, "<Enter>");
    assert_eq!(
        app.last_cli.as_deref(),
        Some(&["kb", "show", "k:AAAA", "--open"].map(str::to_owned)[..])
    );
}

// ── the Sources tab (KB3.7) ─────────────────────────────────────────────────────────────────

fn root(id: &str, name: &str, kind: &str, state: &str, extra: serde_json::Value) -> KbRootInfo {
    let mut v = json!({
        "id": id, "name": name, "kind": kind, "path": format!("/src/{name}"), "state": state,
        "added_ms": 1, "synced_ms": now_ms() - 120_000, "files": 12, "commits": 3, "chunks": 40,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    serde_json::from_value(v).unwrap()
}

/// The Sources tab with two roots answered: a project, and a folder that is gone.
fn sources(app: &mut App) {
    opened(app);
    press(app, "3");
    assert_eq!(lib(app).tab, Tab::Sources);
    assert_eq!(lib(app).request(None), Some(Request::KbRoots { machine: None }));
    let n = lib(app).asked;
    asked(app);
    let roots = vec![
        root("R1", "feeder", "project", "ok", json!({ "project": "p1", "project_name": "feeder" })),
        root(
            "R2",
            "handbook",
            "folder",
            "missing",
            json!({
                "error": "/src/handbook is gone", "profile": "work",
                "options": { "include": ["*.md"], "code": true, "once": true },
            }),
        ),
    ];
    reply(app, Pending::KbRows(n), Reply::KbRoots { roots });
}

#[test]
fn the_sources_tab_lists_what_is_read_how_it_stands_and_how_far_a_read_is() {
    let mut t = t();
    let app = &mut t.app;
    sources(app);
    let shown = screen(app);
    for want in [
        " 3 Sources ",
        "what is read: your projects by themselves, and the folders you add",
        "agent sessions are kept as they happen",
        "project  feeder ",
        "read 2m ago",
        "12 files, 3 commits, 40 pieces",
        "/src/feeder",
        "folder   handbook ",
        "missing",
        "profile work · /src/handbook",
        "/src/handbook is gone",
        // The selected one, in full, beside the list.
        "a project: read for as long as ptyro has it",
        "what your settings say",
        "i read a folder · s look again · S at all of them · x stop reading it",
        "b agents' history",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    assert!(!shown.contains("kind: any") && !shown.contains("machine:"), "{shown}");
    // The folder's own options are said beside it.
    press(app, "j");
    let shown = screen(app);
    for want in ["a folder you gave", "code · only *.md · read once", "seen by   work"] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    // A pass over the project: how far it is, from the event; nothing is asked for it.
    asked(app);
    let progress = |done, of| ptyro_protocol::ServerMsg::Event {
        event: DomainEvent::KbProgress { root: "R1".into(), name: "feeder".into(), done, of },
    };
    app.on_server(progress(3, 10));
    assert!(screen(app).contains("reading 3 of 10"), "{}", screen(app));
    assert!(asked(app).is_empty());
    // While something is being read the list is asked for again by itself, once a second.
    app.sources_tick();
    assert!(asked(app).is_empty(), "not before a second is over");
    app.library_mut().unwrap().polled -= std::time::Duration::from_secs(2);
    app.sources_tick();
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
    // The pass ended: what it holds now is asked for. The selection stays on its root.
    app.on_server(progress(10, 10));
    let n = lib(app).asked;
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
    let roots = vec![
        root("R0", "atlas", "project", "ok", json!({})),
        root("R1", "feeder", "project", "ok", json!({})),
        root("R2", "handbook", "folder", "missing", json!({})),
    ];
    reply(app, Pending::KbRows(n), Reply::KbRoots { roots });
    assert_eq!(lib(app).root().map(|r| r.root.name.as_str()), Some("handbook"));
    // Nothing more is being read: the tick asks nothing.
    app.library_mut().unwrap().polled -= std::time::Duration::from_secs(2);
    app.sources_tick();
    assert!(asked(app).is_empty());
    // `y` copies the path; `1` is the search again, and `3` comes back.
    press(app, "y");
    assert_eq!(toast(app), "copied its path");
    press(app, "1");
    assert_eq!(lib(app).tab, Tab::Search);
    run(app, "kb.sources");
    assert_eq!(lib(app).tab, Tab::Sources);
    // Narrow: the list alone.
    let mut small = sized(60, 20);
    sources(&mut small.app);
    let shown = screen(&small.app);
    assert!(shown.contains("folder   handbook ") && !shown.contains("it is "), "{shown}");
    // Nothing read yet says how to begin.
    let n = lib(&small.app).asked;
    reply(&mut small.app, Pending::KbRows(n), Reply::KbRoots { roots: Vec::new() });
    assert!(screen(&small.app).contains("nothing is read yet"), "{}", screen(&small.app));
}

#[test]
fn a_root_is_looked_at_again_and_removed_in_the_daemons_words() {
    let mut t = t();
    let app = &mut t.app;
    sources(app);
    // `s`: the selected one; `S`: every one, and the notes.
    press(app, "js");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbSynced)));
    reply(app, Pending::KbSynced, Reply::KbSynced { notes: 0, changed: 0, removed: 0, roots: 1 });
    assert_eq!(
        toast(app),
        "0 notes in the folder: 0 read again, 0 gone; 1 project or folder is looked at again"
    );
    asked(app);
    // `x`: asked, not done, by the root's id.
    press(app, "x");
    let Some(Pending::Confirm(req, then)) =
        asked(app).into_iter().find(|p| matches!(p, Pending::Confirm(..)))
    else {
        panic!("nothing asked")
    };
    assert_eq!(*req, Request::KbRemove { root: "R2".into(), confirmed: false, machine: None });
    let words = "Stop reading handbook (/src/handbook) and drop what was kept of it: 12 file(s) and 3 commit(s)? Its files and its repository aren't touched: ingest it again and it is back.";
    app.on_reply(AppMsg::Reply(
        Pending::Confirm(req, then),
        Err(Error::new(ErrorCode::NeedsConfirmation, words)),
    ));
    let shown = screen(app);
    assert!(shown.contains("Stop reading handbook (/src/handbook)"), "{shown}");
    assert!(shown.contains("[y/N]"), "{shown}");
    press(app, "y");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRemoved)));
    reply(
        app,
        Pending::KbRemoved,
        Reply::KbRemoved { name: "handbook".into(), items: 15, chunks: 40 },
    );
    assert_eq!(
        toast(app),
        "handbook is no longer read: dropping 15 items (40 pieces); your files aren't touched"
    );
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbRows(_))));
    // `kb.remove` from anywhere else shows where to pick one.
    press(app, "1");
    run(app, "kb.remove");
    assert_eq!(lib(app).tab, Tab::Sources);
    assert!(toast(app).contains("select what to stop reading"), "{}", toast(app));
    // `t`: whether agent sessions are kept, in your config.
    assert!(app.config.kb.sessions);
    press(app, "t");
    assert!(!app.config.kb.sessions, "{}", toast(app));
    assert!(toast(app).contains("no longer kept"));
}

/// KB4: the Sources tab says what is kept of your shells and switches it (`kb.shell`): `c`
/// the commands, `o` what they print, and `o` never switches the commands on by itself.
#[test]
fn the_sources_tab_says_and_switches_what_is_kept_of_shells() {
    let mut t = t();
    let app = &mut t.app;
    sources(app);
    assert!(!screen(app).contains("shell commands are kept"), "a daemon from before shells");
    let says = |app: &mut App, word: &str| {
        app.library_mut().unwrap().status.as_mut().unwrap().shell = word.into();
        screen(app)
    };
    let shown = says(app, "commands");
    // (Beside the list it is as wide as the list.)
    for want in [
        "shell commands are kept, not what they print",
        "c stops keeping them · o also keeps what they",
        "c shell commands · o their output",
    ] {
        assert!(shown.contains(want), "{want:?} in\n{shown}");
    }
    assert!(says(app, "off").contains("nothing of your shells is kept"));
    assert!(says(app, "output").contains("shell commands are kept, and what they print"));

    assert_eq!(app.config.kb.shell, "commands", "as it comes: commands, not what they print");
    press(app, "o");
    assert_eq!(app.config.kb.shell, "output");
    assert!(toast(app).contains("the first 2 KiB and the last 6 KiB of each"), "{}", toast(app));
    press(app, "o");
    assert_eq!(app.config.kb.shell, "commands");
    assert!(toast(app).contains("print is no longer kept"), "{}", toast(app));
    press(app, "c");
    assert_eq!(app.config.kb.shell, "off");
    assert!(toast(app).contains("nothing more of your shells is kept"), "{}", toast(app));
    // Off: `o` alone keeps nothing, and says which key does.
    press(app, "o");
    assert_eq!(app.config.kb.shell, "off");
    assert!(toast(app).contains("c keeps them, then o"), "{}", toast(app));
    press(app, "c");
    assert_eq!(app.config.kb.shell, "commands");
    assert!(toast(app).contains("not what they print"), "{}", toast(app));
    // From output, `c` stops both.
    press(app, "o");
    press(app, "c");
    assert_eq!(app.config.kb.shell, "off");
}

/// The form's fields are what `ptyro ingest PATH` takes (`crates/ptyro/src/ingest.rs`): the
/// path, `--name`, `--project`, `--profile`, `--include`, `--exclude`, `--no-comments`,
/// `--code`, `--no-commits`, `--once`, and `--machine` once one is linked.
#[test]
fn the_ingest_form_shows_every_option_and_asks_for_what_was_filled_in() {
    let mut t = t();
    let app = &mut t.app;
    sources(app);
    // `i`: the path picker first (here, what was found, or a path to type).
    press(app, "i");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbPaths)));
    reply(app, Pending::KbPaths, Reply::Paths { paths: vec!["/src/found-repo".into()] });
    let Some(Overlay::Picker { palette, purpose: Purpose::KbIngest }) = &app.overlay else {
        panic!("the path picker")
    };
    let labels: Vec<&str> = (0..3).map(|i| palette.item(i).label.as_str()).collect();
    assert_eq!(labels, ["+ a path…", "here", "found-repo"]);
    app.picked(Target::Path("/src/found-repo".into()), Purpose::KbIngest);
    let Some(Overlay::Form(form)) = &app.overlay else { panic!("the ingest form") };
    let keys: Vec<&str> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "path", "name", "project", "profile", "include", "exclude", "comments", "code",
            "commits", "once"
        ]
    );
    // The defaults are shown as the settings have them.
    assert_eq!(form.get("path"), "/src/found-repo");
    assert_eq!(
        (form.get("comments"), form.get("code"), form.get("commits")),
        ("yes".into(), "".into(), "yes".into())
    );
    // ⏎ takes the defaults: nothing but the path is sent.
    press(app, "<Enter>");
    let mut sent = None;
    while let Ok(m) = app.rx.try_recv() {
        if let AppMsg::Reply(Pending::KbIngested(m), _) = m {
            sent = Some(m);
        }
    }
    assert_eq!(sent, Some(None));
    // What was filled in is what is asked for.
    app.picked(Target::NewProjectPath, Purpose::KbIngest);
    let Some(Overlay::Form(mut form)) = app.overlay.take() else { panic!("the ingest form") };
    let set = |form: &mut crate::form::Form, key: &str, v: &str| {
        let f = form.fields.iter_mut().find(|f| f.key == key).unwrap();
        match &mut f.kind {
            crate::form::Kind::Text { value, .. } => *value = v.into(),
            crate::form::Kind::Choice { options, sel } => {
                *sel = options.iter().position(|o| o.0 == v).unwrap()
            }
            crate::form::Kind::Multi { .. } => unreachable!(),
        }
    };
    for (k, v) in [
        ("path", "~/handbook"),
        ("name", "handbook"),
        ("include", "*.md, guides/**"),
        ("exclude", "drafts"),
        ("comments", ""),
        ("code", "yes"),
        ("commits", ""),
        ("once", "yes"),
    ] {
        set(&mut form, k, v);
    }
    assert_eq!(
        sources::spec_of(&form, true),
        ptyro_core::kb::RootSpec {
            path: "~/handbook".into(),
            name: Some("handbook".into()),
            project: None,
            profile: None,
            options: ptyro_core::kb::RootOptions {
                include: vec!["*.md".into(), "guides/**".into()],
                exclude: vec!["drafts".into()],
                comments: Some(false),
                code: Some(true),
                commits: Some(false),
                once: true,
            },
        }
    );
    // Its answer is said as the command says it, on the Sources tab.
    let plan = ptyro_core::kb::KbPlan {
        files: 6,
        documents: 3,
        sources: 1,
        comments: true,
        commits: true,
        ..Default::default()
    };
    let made = root("R3", "handbook", "folder", "syncing", json!({}));
    app.on_reply(AppMsg::Reply(
        Pending::KbIngested(None),
        Ok(Reply::KbIngested { root: made, plan }),
    ));
    assert_eq!(
        toast(app),
        "handbook (/src/handbook): 6 files: 3 documents, 1 source file (comments), its commits"
    );
    assert_eq!(lib(app).tab, Tab::Sources);
    // With a machine linked the form has the pick, and the tab the filter.
    app.machines.push(
        serde_json::from_value(json!({
            "name": "laptop2", "transport": { "t": "ssh", "target": "laptop2" },
            "state": "online", "added_ms": 0,
        }))
        .unwrap(),
    );
    assert!(screen(app).contains("machine: any"));
    press(app, "<Tab><Enter>");
    app.picked(Target::Path("laptop2".into()), Purpose::Library(Filter::Machine));
    assert_eq!(lib(app).request(None), Some(Request::KbRoots { machine: Some("laptop2".into()) }));
    assert!(screen(app).contains("what laptop2 reads"), "{}", screen(app));
    // Its paths are its own: no picker of this computer's folders, and the machine chosen.
    app.overlay = None;
    press(app, "<Esc>");
    assert_eq!(lib(app).chip, None);
    asked(app);
    press(app, "i");
    let Some(Overlay::Form(form)) = &app.overlay else { panic!("the ingest form") };
    assert_eq!(form.fields.last().map(|f| f.key.as_str()), Some("machine"));
    assert_eq!(form.get("machine"), "laptop2");
}

#[test]
fn the_agents_history_is_counted_first_and_read_after_a_yes() {
    let mut t = t();
    let app = &mut t.app;
    sources(app);
    press(app, "b");
    let Some(Overlay::Form(form)) = &app.overlay else { panic!("the backfill form") };
    // What `ptyro ingest --history` takes: `--agent`, `--profile`, `--since`, `--dry-run`.
    let keys: Vec<&str> = form.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(keys, ["agent", "profile", "since", "dry"]);
    // A bad age is said on the form.
    press(app, "<Tab><Tab>soon<Enter>");
    let Some(Overlay::Form(form)) = &app.overlay else { panic!("the form stays") };
    assert!(form.error.as_deref().is_some_and(|e| e.contains("soon")), "{:?}", form.error);
    press(app, "<BS><BS><BS><BS>30d<Enter>");
    let Some(Pending::KbCounted(Some(read))) =
        asked(app).into_iter().find(|p| matches!(p, Pending::KbCounted(_)))
    else {
        panic!("not counted first")
    };
    assert!(
        matches!(&*read, Request::KbHistory { dry_run: false, since_ms: Some(_), agent: None, .. }),
        "{read:?}"
    );
    let plan = ptyro_core::kb::KbHistoryPlan { found: 12, known: 4, new: 8, ..Default::default() };
    app.on_reply(AppMsg::Reply(
        Pending::KbCounted(Some(read)),
        Ok(Reply::KbHistory { plan: plan.clone() }),
    ));
    let shown = screen(app);
    assert!(shown.contains("12 conversations in your agents' folders"), "{shown}");
    assert!(shown.contains("Read them? [y/N]"), "{shown}");
    press(app, "y");
    let Some(Pending::KbBackfilled(said)) =
        asked(app).into_iter().find(|p| matches!(p, Pending::KbBackfilled(_)))
    else {
        panic!("not read after the yes")
    };
    app.on_reply(AppMsg::Reply(Pending::KbBackfilled(said), Ok(Reply::KbHistory { plan })));
    assert!(toast(app).starts_with("reading 8 conversations behind everything else"));
    // Only counting, or nothing to read: the count is all there is.
    app.on_reply(AppMsg::Reply(
        Pending::KbCounted(None),
        Ok(Reply::KbHistory { plan: ptyro_core::kb::KbHistoryPlan::default() }),
    ));
    assert_eq!(toast(app), "no conversations in your agents' folders");
    assert!(app.overlay.is_none());
}

#[test]
fn the_history_brings_older_pages_at_its_end() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    let n = lib(app).asked;
    let now = now_ms();
    let page = |from: u64| -> Vec<KbListed> {
        (from..from + 100)
            .map(|i| listed(&format!("{i:04}"), &format!("session {i}"), now - i * 60_000, None))
            .collect()
    };
    reply(app, Pending::KbRows(n), Reply::KbItems { items: page(0) });
    asked(app);
    // (The day-old one that was listed before stays under the page: it is older than it.)
    let listed_now = lib(app).rows.len();
    assert_eq!(listed_now, 101);
    // Not at the end: nothing more is asked.
    press(app, "j");
    assert!(!asked(app).iter().any(|p| matches!(p, Pending::KbOlder(_))));
    assert!(lib(app).more);
    // At the end: the next page, from before the oldest listed.
    press(app, "G");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbOlder(a) if *a == n)));
    assert!(screen(app).contains("Looking for older ones…"), "{}", screen(app));
    // (Not asked for twice while it is on its way.)
    press(app, "j");
    assert!(!asked(app).iter().any(|p| matches!(p, Pending::KbOlder(_))));
    let mut older = page(100);
    older.truncate(3);
    older.push(listed("0099", "session 99", now - 99 * 60_000, None));
    app.on_reply(AppMsg::Reply(Pending::KbOlder(n), Ok(Reply::KbItems { items: older })));
    assert_eq!((lib(app).rows.len(), lib(app).more, lib(app).sel), (104, false, 100));
    // News of a change asks for the newest page again: the older ones stay under it.
    app.library_news();
    let n = lib(app).asked;
    reply(app, Pending::KbRows(n), Reply::KbItems { items: page(0) });
    assert_eq!(lib(app).rows.len(), 104);
}

#[test]
fn enter_on_a_file_or_a_commit_opens_it_in_its_own_tool() {
    let mut t = t();
    let app = &mut t.app;
    opened(app);
    let mut doc = hit("FFFF", 4, "docs/retry.md", &format!("the {MARK_ON}retry{MARK_OFF} policy"));
    doc.kind = HitKind::Doc;
    let mut commit =
        hit("CCCC", 0, "feeder: retry three times", &format!("{MARK_ON}retry{MARK_OFF}"));
    commit.kind = HitKind::Commit;
    let mut far = hit("EEEE", 1, "far.md", &format!("{MARK_ON}retry{MARK_OFF}"));
    (far.kind, far.machine, far.addr) =
        (HitKind::Doc, Some("laptop2".into()), "laptop2:k:EEEE/1".into());
    searched(app, "retry", vec![doc, commit, far], None);
    // A file: your editor at the piece's line, by the command that opens one.
    press(app, "<Enter>");
    assert_eq!(
        app.last_cli.as_deref(),
        Some(&["kb", "show", "k:FFFF/4", "--open"].map(str::to_owned)[..])
    );
    // A commit: `git show` of it.
    press(app, "j<Enter>");
    assert_eq!(
        app.last_cli.as_deref(),
        Some(&["kb", "show", "k:CCCC/0", "--open"].map(str::to_owned)[..])
    );
    // Another machine's file is not here to open: what was kept of it is read.
    asked(app);
    press(app, "j<Enter>");
    assert!(asked(app).iter().any(|p| matches!(p, Pending::KbReader(Some(1)))));
}
