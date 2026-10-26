//! The About screen (ROADMAP UX2.1): which ptyro this is, where to report a problem, where its
//! files live, whether the daemon and the web app run, and one key that copies all of it for a
//! bug report. The facts are `ptyro about --json` (run as a process, like the doctor), so the
//! CLI, the TUI and the web app say the same. The repository and issues addresses are OSC 8
//! hyperlinks where the terminal has them; a key opens or copies each one anyway.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use ptyro_core::input::{KeyCode, KeyEvent};

use crate::app::{App, AppMsg, Overlay};
use crate::keymap::Command;
use crate::ui::fg;

impl App {
    /// Open the screen at once and fill it in when `ptyro about --json` has run.
    pub fn open_about(&mut self) {
        self.overlay = Some(Overlay::About { doc: None, sel: 0, scroll: 0 });
        let Ok(exe) = std::env::current_exe() else { return };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let out = tokio::process::Command::new(exe)
                .args(["about", "--json"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .await;
            let doc = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok());
            let _ = tx.send(AppMsg::About(doc));
        });
    }

    /// Keys on the About screen: pick a row, open or copy it, copy everything, switch the
    /// daemon to this version, or close.
    pub(crate) fn about_key(
        &mut self,
        key: KeyEvent,
        doc: Option<Value>,
        mut sel: usize,
        mut scroll: usize,
    ) {
        let Some(d) = doc else {
            // Still looking: Esc leaves, anything else waits.
            if !matches!(key.code, KeyCode::Esc | KeyCode::Char { c: 'q' }) {
                self.overlay = Some(Overlay::About { doc: None, sel, scroll });
            }
            return;
        };
        let rows = rows(&d);
        let picks = picks(&rows);
        let picked = picks.get(sel).and_then(|i| rows[*i].target.clone());
        match key.code {
            KeyCode::Esc | KeyCode::Char { c: 'q' } => return,
            KeyCode::Down | KeyCode::Char { c: 'j' } => {
                sel = (sel + 1).min(picks.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char { c: 'k' } => sel = sel.saturating_sub(1),
            KeyCode::PageDown => scroll += 10,
            KeyCode::PageUp => scroll = scroll.saturating_sub(10),
            KeyCode::Enter | KeyCode::Char { c: 'o' } => {
                if let Some(t) = picked {
                    if open(&t) {
                        self.toast(format!("opened {}", t.text()));
                    } else {
                        crate::osc52(t.text());
                        self.toast(format!("nothing to open it with: copied {}", t.text()));
                    }
                }
            }
            KeyCode::Char { c: 'y' } => {
                if let Some(t) = picked {
                    crate::osc52(t.text());
                    self.toast(format!("copied {}", t.text()));
                }
            }
            KeyCode::Char { c: 'c' } | KeyCode::Char { c: 'Y' } => {
                crate::osc52(d["report"].as_str().unwrap_or_default());
                self.toast("copied all of it: paste it into the bug report");
            }
            KeyCode::Char { c: 'U' } if d["daemon"]["same"] == false => {
                self.run(&Command { action: "daemon.upgrade", arg: None });
                return;
            }
            _ => {}
        }
        self.overlay = Some(Overlay::About { doc: Some(d), sel, scroll });
    }
}

/// A line of the screen: a label, a value, and what picking it does.
pub struct Row {
    pub label: String,
    pub value: String,
    /// What it opens or copies: an address, or a path.
    pub target: Option<Target>,
    /// Dim text under the row (the fix for a daemon that is not this version).
    pub note: Option<String>,
    pub heading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Url(String),
    Path(String),
}

impl Target {
    pub fn text(&self) -> &str {
        match self {
            Self::Url(s) | Self::Path(s) => s,
        }
    }
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

/// Everything shown, in order. `doc` is the JSON `ptyro about --json` prints.
pub fn rows(doc: &Value) -> Vec<Row> {
    let row = |label: &str, value: String, target: Option<Target>| Row {
        label: label.into(),
        value,
        target,
        note: None,
        heading: false,
    };
    let mut v = vec![row(
        "ptyro",
        format!("{} (protocol {})", text(doc, "version"), text(doc, "protocol")),
        None,
    )];
    let b = &doc["build"];
    let build: Vec<String> = [
        b["commit"].as_str().map(|c| format!("commit {c}")),
        b["date"].as_str().map(|d| format!("of {d}")),
        b["target"].as_str().filter(|t| !t.is_empty()).map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !build.is_empty() {
        v.push(row("build", build.join(" "), None));
    }
    v.push(row("system", text(doc, "system").into(), None));
    v.push(row(
        "source",
        text(doc, "repository").into(),
        Some(Target::Url(text(doc, "repository").into())),
    ));
    v.push(row("licence", text(doc, "license").into(), None));
    v.push(row(
        "report",
        text(doc, "issues").into(),
        Some(Target::Url(text(doc, "issues").into())),
    ));
    let d = &doc["daemon"];
    let mut daemon = if d["running"].as_bool() == Some(true) {
        row(
            "daemon",
            format!(
                "running · {} (protocol {}) · pid {}",
                text(d, "version"),
                text(d, "protocol"),
                d["pid"]
            ),
            None,
        )
    } else {
        row("daemon", "not running (it starts on demand)".into(), None)
    };
    if d["same"].as_bool() == Some(false) {
        daemon.value.push_str(" · NOT this version");
        daemon.note = d["fix"].as_str().map(str::to_owned);
    }
    v.push(daemon);
    if let Some(w) = doc["web"].as_object() {
        let line = w.get("text").and_then(|t| t.get(0)).and_then(Value::as_str).unwrap_or("");
        let url = w.get("url").and_then(Value::as_str).filter(|_| w["up"] == true);
        v.push(row("web app", line.into(), url.map(|u| Target::Url(u.into()))));
    }
    v.push(Row { heading: true, ..row("where things live", String::new(), None) });
    for p in doc["places"].as_array().into_iter().flatten() {
        let path = text(p, "path").to_owned();
        let missing = if p["exists"] == false { " (not made yet)" } else { "" };
        v.push(row(text(p, "name"), format!("{path}{missing}"), Some(Target::Path(path))));
    }
    v.push(Row { heading: true, ..row("tools", String::new(), None) });
    for t in doc["tools"].as_array().into_iter().flatten() {
        v.push(row(text(t, "name"), text(t, "text").into(), None));
    }
    v
}

/// The rows a key can land on.
pub fn picks(rows: &[Row]) -> Vec<usize> {
    rows.iter().enumerate().filter(|(_, r)| r.target.is_some()).map(|(i, _)| i).collect()
}

/// Open an address (or a folder) with what the system uses for it. `false`: nothing to open it
/// with, so the caller copies it instead.
pub fn open(t: &Target) -> bool {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let arg = match t {
        Target::Url(u) => u.clone(),
        // A file opens in its folder.
        Target::Path(p) => {
            let p = std::path::Path::new(p);
            if p.is_dir() { p } else { p.parent().unwrap_or(p) }.to_string_lossy().into_owned()
        }
    };
    std::process::Command::new(program)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// An address on screen: where it is drawn, for the terminal to make it a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub url: String,
}

/// The screen's lines, and the links in them. `x0`/`y0` are the first line's top-left corner
/// inside the box, `sel` the picked row.
pub fn lines(
    app: &App,
    rows: &[Row],
    sel: Option<usize>,
    scroll: usize,
    x0: u16,
    y0: u16,
    height: usize,
) -> (Vec<Line<'static>>, Vec<Link>) {
    let t = &app.theme;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut links = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.heading {
            out.push(Line::raw(""));
            out.push(Line::styled(
                format!("  {}", r.label),
                fg(app, t.dim).add_modifier(Modifier::ITALIC),
            ));
            continue;
        }
        let picked = sel == Some(i);
        let mark = if picked { "›" } else { " " };
        let value_style = match &r.target {
            Some(Target::Url(u)) if r.value == *u => {
                fg(app, t.accent).add_modifier(Modifier::UNDERLINED)
            }
            _ if r.value.contains("NOT this version") => fg(app, t.blocked),
            _ => Style::new(),
        };
        let value_style =
            if picked { value_style.add_modifier(Modifier::BOLD) } else { value_style };
        let label = format!("{mark} {:<12} ", r.label);
        if let Some(Target::Url(u)) = &r.target {
            // The address, wherever it sits in the value ("up at http://…").
            let y = out.len();
            if y >= scroll
                && y - scroll < height
                && let Some(at) = r.value.find(u.as_str())
            {
                links.push(Link {
                    x: x0 + (label.chars().count() + r.value[..at].chars().count()) as u16,
                    y: y0 + (y - scroll) as u16,
                    width: u.chars().count() as u16,
                    url: u.clone(),
                });
            }
        }
        out.push(Line::from(vec![
            Span::styled(label, fg(app, if picked { t.accent } else { t.dim })),
            Span::styled(r.value.clone(), value_style),
        ]));
        if let Some(n) = &r.note {
            out.push(Line::styled(format!("    {n}"), fg(app, t.dim)));
        }
    }
    (out, links)
}

/// The cells of the links on screen, each with the hyperlink escape in its symbol: the first
/// opens the link, the last closes it. Written over what is already drawn (the same text), so the
/// screen looks the same and the terminal knows it is a link (OSC 8).
pub fn link_cells(
    links: &[Link],
    buf: &ratatui::buffer::Buffer,
) -> Vec<(u16, u16, ratatui::buffer::Cell)> {
    let mut cells = Vec::new();
    for l in links {
        // Whatever the escape would carry must be a plain address.
        if l.width == 0 || l.url.chars().any(|c| c.is_control()) {
            continue;
        }
        for i in 0..l.width {
            let (x, y) = (l.x + i, l.y);
            let Some(c) = buf.cell((x, y)) else { continue };
            let mut c = c.clone();
            let mut sym = c.symbol().to_owned();
            if i == 0 {
                sym = format!("\x1b]8;;{}\x1b\\{sym}", l.url);
            }
            if i + 1 == l.width {
                sym.push_str("\x1b]8;;\x1b\\");
            }
            c.set_symbol(&sym);
            cells.push((x, y, c));
        }
    }
    cells
}

pub fn paint_links(
    tui: &mut ratatui::DefaultTerminal,
    cells: &[(u16, u16, ratatui::buffer::Cell)],
) {
    use ratatui::backend::Backend;
    let b = tui.backend_mut();
    let _ = b.draw(cells.iter().map(|(x, y, c)| (*x, *y, c)));
    let _ = Backend::flush(b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "version": "9.9.9", "protocol": "1.2", "system": "linux x86_64",
            "repository": "https://example.com/r", "issues": "https://example.com/r/issues",
            "license": "MIT",
            "build": { "commit": "abc", "date": "2026-10-26", "target": "x" },
            "daemon": { "running": true, "version": "9.9.8", "protocol": "1.2", "pid": 7,
                        "same": false, "fix": "run `ptyro daemon upgrade`" },
            "web": { "wanted": true, "up": true, "url": "http://h:1", "text": ["up at http://h:1"] },
            "places": [{ "name": "config", "path": "/c/config.toml", "exists": false }],
            "tools": [{ "name": "git", "kind": "git", "found": true, "text": "2.54.0" }],
        })
    }

    #[test]
    fn rows_say_the_same_things_the_cli_does() {
        let r = rows(&doc());
        let all: Vec<String> = r.iter().map(|r| format!("{} {}", r.label, r.value)).collect();
        let all = all.join("\n");
        assert!(all.contains("ptyro 9.9.9 (protocol 1.2)"), "{all}");
        assert!(all.contains("commit abc of 2026-10-26 x"), "{all}");
        assert!(all.contains("NOT this version"), "{all}");
        assert!(all.contains("config /c/config.toml (not made yet)"), "{all}");
        assert!(all.contains("git 2.54.0"), "{all}");
        let daemon = r.iter().find(|r| r.label == "daemon").unwrap();
        assert_eq!(daemon.note.as_deref(), Some("run `ptyro daemon upgrade`"));
        // The repository, issues, web app and each path can be picked.
        let picked: Vec<&str> = picks(&r).iter().map(|i| r[*i].label.as_str()).collect();
        assert_eq!(picked, ["source", "report", "web app", "config"]);
    }

    #[test]
    fn a_daemon_that_is_not_running_is_said_plainly() {
        let mut d = doc();
        d["daemon"] = json!({ "running": false });
        let r = rows(&d);
        let daemon = r.iter().find(|r| r.label == "daemon").unwrap();
        assert!(daemon.value.starts_with("not running"), "{}", daemon.value);
        assert!(daemon.note.is_none());
    }

    // ── on screen ──

    use crate::Options;
    use crate::app::AppMsg;
    use ptyro_client::ConnectOptions;
    use ptyro_core::config::Config;
    use ptyro_core::input::Mods;
    use ptyro_core::paths::Paths;
    use ratatui::layout::Rect;

    fn app() -> App {
        let home = std::env::temp_dir().join("ptyro-tui-about-test");
        let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| home.clone())).unwrap();
        let connect = ConnectOptions::new(paths.clone(), ptyro_protocol::ClientKind::Tui);
        App::new(
            Options::default(),
            connect,
            paths,
            Config::default(),
            false,
            Rect::new(0, 0, 120, 40),
        )
    }

    fn draw(app: &App) -> (String, ratatui::buffer::Buffer) {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let buf = t.backend().buffer().clone();
        let text = buf.content.chunks(buf.area.width as usize).fold(String::new(), |mut s, row| {
            s.extend(row.iter().map(|c| c.symbol()));
            s.push('\n');
            s
        });
        (text, buf)
    }

    #[test]
    fn it_has_a_key_and_a_place_in_the_palette_and_which_key() {
        let app = app();
        assert_eq!(app.keymap.key_for("app.about").as_deref(), Some("C-q I"));
        assert!(crate::keymap::ACTIONS.iter().any(|a| a.id == "app.about"));
    }

    #[test]
    fn the_screen_shows_the_facts_and_links_that_a_terminal_can_follow() {
        let mut app = app();
        app.overlay = Some(Overlay::About { doc: None, sel: 0, scroll: 0 });
        assert!(draw(&app).0.contains("Looking"), "waits for the answer");
        app.on_reply(AppMsg::About(Some(doc())));
        let (shown, buf) = draw(&app);
        for want in [
            "About ptyro",
            "9.9.9 (protocol 1.2)",
            "commit abc of 2026-10-26",
            "https://example.com/r",
            "NOT this version",
            "run `ptyro daemon upgrade`",
            "/c/config.toml (not made yet)",
            "U switch the daemon to this version",
            "c copy all, for a bug report",
        ] {
            assert!(shown.contains(want), "{want:?} in\n{shown}");
        }
        // The repository, issues and web app addresses are the links, drawn where they are.
        let links = app.links.borrow().clone();
        assert_eq!(links.len(), 3, "{links:?}");
        for l in &links {
            let drawn: String = (0..l.width)
                .map(|i| buf.cell((l.x + i, l.y)).unwrap().symbol().to_owned())
                .collect();
            assert_eq!(drawn, l.url, "the link sits on its own text");
        }
        // The cells carry the hyperlink escape around that same text.
        let cells = link_cells(&links, &buf);
        let first = cells.iter().find(|(x, y, _)| (*x, *y) == (links[0].x, links[0].y)).unwrap();
        assert!(
            first.2.symbol().starts_with("\x1b]8;;https://example.com/r\x1b\\h"),
            "{:?}",
            first.2.symbol()
        );
        let last = cells.iter().rfind(|(_, y, _)| *y == links[0].y).unwrap();
        assert!(last.2.symbol().ends_with("\x1b]8;;\x1b\\"), "{:?}", last.2.symbol());
        // A control character never rides into the escape.
        let mut bad = links.clone();
        bad[0].url = "https://x/\x1b]52;c;AAAA".into();
        assert_eq!(link_cells(&bad, &buf).len(), cells.len() - links[0].width as usize);
    }

    #[test]
    fn keys_pick_copy_and_close() {
        let mut app = app();
        app.overlay = Some(Overlay::About { doc: Some(doc()), sel: 0, scroll: 0 });
        let press = |app: &mut App, c: char| {
            app.overlay_key(KeyEvent::new(KeyCode::Char { c }, Mods::NONE))
        };
        press(&mut app, 'j');
        let Some(Overlay::About { sel, .. }) = &app.overlay else { panic!("open") };
        assert_eq!(*sel, 1, "the issues link");
        press(&mut app, 'y');
        assert!(app.toast.as_ref().unwrap().text.contains("https://example.com/r/issues"));
        press(&mut app, 'c');
        assert!(app.toast.as_ref().unwrap().text.contains("copied all of it"));
        assert!(matches!(app.overlay, Some(Overlay::About { .. })), "copying keeps it open");
        // Down past the last pick stays on it; Esc closes.
        for _ in 0..9 {
            press(&mut app, 'j');
        }
        let Some(Overlay::About { sel, .. }) = &app.overlay else { panic!("open") };
        assert_eq!(*sel, 3, "four picks");
        app.overlay_key(KeyEvent::new(KeyCode::Esc, Mods::NONE));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn the_upgrade_key_is_only_there_when_the_daemon_differs() {
        let mut same = doc();
        same["daemon"]["same"] = json!(true);
        let mut app = app();
        app.overlay = Some(Overlay::About { doc: Some(same), sel: 0, scroll: 0 });
        assert!(!draw(&app).0.contains("U switch"));
        app.overlay_key(KeyEvent::new(KeyCode::Char { c: 'U' }, Mods::NONE));
        assert!(matches!(app.overlay, Some(Overlay::About { .. })), "U does nothing then");
    }

    #[test]
    fn a_failed_lookup_says_how_to_see_why() {
        let mut app = app();
        app.overlay = Some(Overlay::About { doc: None, sel: 0, scroll: 0 });
        app.on_reply(AppMsg::About(None));
        assert!(draw(&app).0.contains("did not answer"));
    }

    #[test]
    fn help_says_the_version_and_the_sidebar_marks() {
        let mut app = app();
        app.run(&Command { action: "help", arg: None });
        let (shown, _) = draw(&app);
        // The footer is below the fold of a tall list: scroll it into view.
        let mut app2 = app;
        app2.overlay = Some(Overlay::Help { filter: String::new(), scroll: 10_000 });
        let (shown2, _) = draw(&app2);
        let all = format!("{shown}{shown2}");
        assert!(all.contains(&format!("ptyro {}", env!("CARGO_PKG_VERSION"))), "{all}");
        assert!(all.contains("C-q I About"), "{all}");
        assert!(all.contains("✓ pushed") && all.contains("not pushed ↑2"), "{all}");
    }

    fn wt(status: serde_json::Value) -> ptyro_core::model::WorktreeInfo {
        serde_json::from_value(json!({
            "id": "w1", "project": "p", "project_slug": "lib", "name": "feat", "path": "/x",
            "is_root": false, "state": "ready", "created_ms": 0, "locked": false,
            "status": status,
        }))
        .unwrap()
    }

    fn st(extra: serde_json::Value) -> serde_json::Value {
        let mut s = json!({
            "ahead": 0, "behind": 0, "base_ahead": 1, "base_behind": 0, "staged": 0,
            "modified": 0, "untracked": 0, "conflicts": 0, "insertions": 42, "deletions": 7,
            "at_ms": 0
        });
        for (k, v) in extra.as_object().unwrap() {
            s[k] = v.clone();
        }
        s
    }

    fn stat(w: &ptyro_core::model::WorktreeInfo) -> String {
        crate::ui::diffstat(&app(), w).iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn the_sidebar_row_says_what_is_left_to_do() {
        // Nothing of its own: nothing said.
        assert_eq!(stat(&wt(st(json!({"base_ahead": 0, "insertions": 0, "deletions": 0})))), "");
        // Uncommitted: the lines and the count.
        assert_eq!(stat(&wt(st(json!({"modified": 2, "untracked": 1})))), " +42 −7 ● 3");
        // Committed with no upstream: not pushed, still with the lines.
        assert_eq!(stat(&wt(st(json!({"unpushed": 2})))), " +42 −7 not pushed ↑2");
        // Ahead of an upstream, and dirty.
        assert_eq!(
            stat(&wt(st(json!({"upstream": "origin/x", "ahead": 2, "staged": 1})))),
            " +42 −7 ↑2 ● 1"
        );
        // Pushed and clean: done, and the line counts go quiet.
        assert_eq!(stat(&wt(st(json!({"upstream": "origin/x", "pushed": true})))), " ✓ pushed");
        // An open PR's badge says it for the row; a merged one says it too.
        let mut with_pr = wt(st(json!({"upstream": "origin/x", "pushed": true})));
        with_pr.pr = serde_json::from_value(json!({
            "number": 7, "state": "OPEN", "url": "u", "title": "t", "at_ms": 0
        }))
        .ok();
        assert_eq!(stat(&with_pr), "", "{:?}", with_pr.pr);
        let mut merged = wt(st(json!({"modified": 1})));
        merged.pr = serde_json::from_value(json!({
            "number": 7, "state": "MERGED", "url": "u", "title": "t", "at_ms": 0
        }))
        .ok();
        assert_eq!(stat(&merged), "", "shipped: no counts");
    }
}
