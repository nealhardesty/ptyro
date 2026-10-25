use super::*;

struct T {
    p: Panel,
    dir: tempfile::TempDir,
}

fn key(name: &str) -> KeyEvent {
    let code = match name {
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Tab" => KeyCode::Tab,
        "BS" => KeyCode::Backspace,
        "Space" => KeyCode::Char { c: ' ' },
        c if c.chars().count() == 1 => KeyCode::Char { c: c.chars().next().unwrap() },
        other => panic!("no key {other}"),
    };
    KeyEvent::new(code, Mods::NONE)
}

impl T {
    fn new(config: &str) -> Self {
        Self::with(config, false)
    }

    fn with(config: &str, project: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        if !config.is_empty() {
            std::fs::write(&path, config).unwrap();
        }
        let proj = project.then(|| {
            let root = dir.path().join("repo");
            std::fs::create_dir_all(root.join(".git")).unwrap();
            Proj { name: "repo".into(), root }
        });
        Self { p: Panel::new(path, proj), dir }
    }

    /// `"jj<Enter>"`: a key per character, named keys in angle brackets.
    fn press(&mut self, keys: &str) -> Vec<Action> {
        let mut out = Vec::new();
        let mut rest = keys;
        while !rest.is_empty() {
            let (k, n) = match rest.strip_prefix('<') {
                Some(r) => {
                    let end = r.find('>').unwrap();
                    (r[..end].to_owned(), end + 2)
                }
                None => {
                    let c = rest.chars().next().unwrap();
                    (c.to_string(), c.len_utf8())
                }
            };
            out.extend(self.p.key(key(&k)));
            rest = &rest[n..];
        }
        out
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config.toml")).unwrap_or_default()
    }

    fn local(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("repo/.ptyro.local.toml")).unwrap_or_default()
    }

    /// Open the section named `title` with the cursor on `key`.
    fn go(&mut self, key: &str) {
        self.p.jump(key);
        assert_eq!(self.selected().as_deref(), Some(key), "the cursor is on {key}");
    }

    fn selected(&self) -> Option<String> {
        match self.p.rows().get(self.p.cursor)? {
            Row::Setting(pr, i) => Some(self.p.entry(*pr, *i).key.clone()),
            Row::Member { path, name, .. } => Some(format!("{path}.{name}")),
            Row::Binding { path, name, .. } => Some(format!("{path}.{name}")),
            Row::Add { path, .. } => Some(format!("+{path}")),
            _ => None,
        }
    }

    fn note(&self) -> String {
        self.p.note.clone().map(|(m, _)| m).unwrap_or_default()
    }

    fn err(&self) -> String {
        match &self.p.note {
            Some((m, true)) => m.clone(),
            _ => String::new(),
        }
    }
}

#[test]
fn it_opens_on_the_first_section_and_lists_its_settings_with_where_they_come_from() {
    let mut t = T::new("[ui]\ntheme = \"ptyro-light\"\n");
    assert_eq!(t.p.section().id, "interface");
    t.press("<Enter>");
    assert_eq!(t.selected().as_deref(), Some("ui.theme"));
    let rows = t.p.rows();
    let Row::Setting(pr, i) = rows[0] else { panic!() };
    let e = t.p.entry(pr, i);
    assert!(e.changed() && e.origin == Origin::User);
    assert_eq!(t.p.uview.entries.iter().filter(|e| e.changed()).count(), 1);
}

#[test]
fn a_switch_toggles_and_r_puts_it_back() {
    let mut t = T::new("# mine\n[ui]\nmouse = true # keep\n");
    t.go("ui.clock");
    let acts = t.press("<Enter>");
    assert_eq!(acts, [Action::Reload]);
    assert!(
        t.file().contains("clock = false")
            && t.file().contains("# mine")
            && t.file().contains("# keep"),
        "{}",
        t.file()
    );
    assert!(t.note().contains("applies at once"));
    t.press("r");
    assert!(!t.file().contains("clock"), "{}", t.file());
    // Resetting the default again says so and writes nothing.
    t.press("r");
    assert!(t.note().contains("already the default"));
}

#[test]
fn numbers_step_with_plus_and_minus_and_stay_in_range() {
    let mut t = T::new("");
    t.go("ui.sidebar_width");
    t.press("+");
    assert!(t.file().contains("sidebar_width = 31"), "{}", t.file());
    t.press("---------------------------------------------");
    assert!(t.file().contains("sidebar_width = 12"), "stops at the lowest: {}", t.file());
}

#[test]
fn a_text_box_says_why_a_value_is_wrong_as_you_type_and_saves_a_good_one() {
    let mut t = T::new("");
    t.go("github.poll");
    t.press("<Enter>");
    // The box starts on the current value; clear it and type something that isn't a duration.
    t.press("<BS><BS>soon");
    let Some(Modal::Input { check, .. }) = &t.p.modal else { panic!("a box") };
    assert!(check.as_deref().unwrap().contains("bad duration"), "{check:?}");
    t.press("<Enter>");
    assert!(t.err().contains("bad duration"), "{}", t.err());
    assert!(t.file().is_empty(), "nothing written");
    assert!(t.p.modal.is_some(), "the box stays open with what was typed");
    // Fix it in place.
    t.press("<BS><BS><BS><BS><BS>2m<Enter>");
    assert!(t.file().contains("poll = \"2m\""), "{}", t.file());
}

#[test]
fn a_choice_opens_a_list_and_one_pick_is_saved() {
    let mut t = T::new("");
    t.go("ui.on_stop");
    t.press("<Enter>");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("a list") };
    assert_eq!(p.items.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["next", "stay"]);
    t.press("<Down><Enter>");
    assert!(t.file().contains("on_stop = \"stay\""));
    // Esc leaves it as it was.
    t.press("<Enter><Up><Esc>");
    assert!(t.file().contains("on_stop = \"stay\""));
}

#[test]
fn a_set_toggles_each_choice_and_saves_them_together() {
    let mut t = T::new("");
    t.go("notifications.channels");
    t.press("<Enter>");
    // inbox, osc and bell are on by default; turn bell off and desktop on.
    t.press("<Down><Space><Down><Down><Space><Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert!(
        c.notifications.channels.contains(&"desktop".to_owned())
            && !c.notifications.channels.contains(&"bell".to_owned()),
        "{:?}",
        c.notifications.channels
    );
}

#[test]
fn the_theme_shows_as_the_cursor_moves_over_it_and_goes_back_on_esc() {
    let mut t = T::new("");
    t.go("ui.theme");
    let acts = t.press("<Enter>");
    assert_eq!(acts, [Action::Theme(Some("ptyro-dark".into()))]);
    let acts = t.press("<Down>");
    assert_eq!(acts, [Action::Theme(Some("ptyro-light".into()))]);
    let acts = t.press("<Esc>");
    assert_eq!(acts, [Action::Theme(None)]);
    assert!(t.file().is_empty());
}

#[test]
fn undo_puts_the_file_back_exactly() {
    let mut t = T::new("[ui]\nmouse = false\n");
    t.go("ui.clock");
    t.press("<Enter>");
    t.go("ui.sidebar");
    t.press("<Enter>");
    assert!(t.file().contains("sidebar = false"));
    t.press("u");
    assert!(!t.file().contains("sidebar") && t.file().contains("clock = false"));
    t.press("u");
    assert_eq!(t.file(), "[ui]\nmouse = false\n");
    t.press("u");
    assert!(t.note().contains("nothing to undo"));
}

#[test]
fn a_change_the_semantic_check_refuses_is_refused_with_the_reason() {
    let mut t = T::new("");
    // (The themes list only offers real ones; a typed name goes through the same check.)
    let e = t.p.find_entry("ui.theme").unwrap();
    let acts = t.p.set(&e, "no-such-theme");
    assert!(acts.is_empty() && t.err().contains("unknown theme"), "{}", t.err());
    assert!(t.file().is_empty());
    // A key bound to an action that doesn't exist.
    let layer = t.p.user.clone();
    let r = settings::apply(
        &layer,
        &Op::Set { key: "keys.x".into(), value: "no.such.action".into() },
        &t.p.lk,
        &Panel::extra_for(&layer),
    );
    assert!(r.is_err());
}

#[test]
fn an_agent_is_added_edited_renamed_and_removed_without_touching_the_file() {
    let mut t = T::new("# keep me\n[ui]\nmouse = false\n");
    t.press("<Down><Down><Down>");
    assert_eq!(t.p.section().id, "agents");
    t.press("<Enter>");
    assert_eq!(t.selected().as_deref(), Some("+agents"));
    t.press("<Enter>aider<Enter>");
    assert!(t.file().contains("[agents.aider]") && t.file().contains("# keep me"), "{}", t.file());
    assert_eq!(
        t.p.drill.as_ref().map(|(p, n)| (*p, n.as_str())),
        Some(("agents", "aider")),
        "it opens on the new agent"
    );
    t.go("agents.aider.command");
    t.press("<Enter>aider --model sonnet<Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert_eq!(c.agents["aider"].command.as_ref().unwrap().argv(), ["aider", "--model", "sonnet"]);
    t.press("<Esc>");
    assert!(t.p.drill.is_none());
    t.press("R");
    t.press("<BS><BS><BS><BS><BS>pair<Enter>");
    assert!(Config::parse(&t.file()).unwrap().agents.contains_key("pair"));
    t.press("d");
    assert!(matches!(t.p.modal, Some(Modal::Confirm { .. })));
    t.press("n");
    assert!(t.file().contains("[agents.pair]"));
    t.press("d");
    t.press("y");
    assert!(!t.file().contains("agents") && t.file().contains("# keep me"), "{}", t.file());
}

#[test]
fn a_key_binding_is_made_by_pressing_the_key_and_picking_what_it_runs() {
    let mut t = T::new("");
    t.p.jump("keys.x");
    // Jumping to a binding that isn't there lands on the section.
    assert_eq!(t.p.section().id, "keys");
    t.press("n");
    assert!(matches!(t.p.modal, Some(Modal::Capture { .. })));
    t.press("x");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("an action list") };
    assert!(p.items.len() > 100 && p.search);
    t.press("session.next");
    t.press("<Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert_eq!(c.keys["x"], "session.next", "{}", t.file());
    // It shows, and ⏎ changes what it runs; d unbinds it.
    t.p.jump("keys.x");
    assert_eq!(t.selected().as_deref(), Some("keys.x"));
    t.press("d");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty());
}

#[test]
fn the_prefix_is_set_by_pressing_the_keys() {
    let mut t = T::new("");
    t.go("ui.prefix");
    t.press("<Enter>");
    assert!(matches!(t.p.modal, Some(Modal::Capture { .. })));
    let ctrl = |c| KeyEvent::new(KeyCode::Char { c }, Mods(Mods::CTRL));
    t.p.key(ctrl('a'));
    t.p.key(ctrl('b'));
    t.press("<Enter>");
    assert_eq!(Config::parse(&t.file()).unwrap().ui.prefix.keys(), ["C-a", "C-b"], "{}", t.file());
}

#[test]
fn a_notification_service_asks_for_what_it_needs_and_is_written_whole() {
    let mut t = T::new("");
    t.p.jump("notifications.on");
    t.press("jjjjjjjj");
    while !matches!(t.p.rows().get(t.p.cursor), Some(Row::Add { .. })) {
        t.press("j");
    }
    t.press("<Enter>");
    let Some(Modal::Pick(_)) = &t.p.modal else { panic!("a type list") };
    t.press("<Enter>");
    // ntfy needs a url.
    let Some(Modal::Input { title, .. }) = &t.p.modal else { panic!("asks for the url") };
    assert!(title.starts_with("url"), "{title}");
    t.press("https://ntfy.sh/me<Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert_eq!(c.notifications.sinks.len(), 1, "{}", t.file());
    // The service's own page shows only what ntfy uses.
    t.p.jump("notifications.sinks.0.url");
    let keys: Vec<String> =
        t.p.rows()
            .iter()
            .filter_map(|r| match r {
                Row::Setting(pr, i) => Some(t.p.entry(*pr, *i).key.clone()),
                _ => None,
            })
            .collect();
    assert!(
        keys.contains(&"notifications.sinks.0.url".to_owned())
            && !keys.contains(&"notifications.sinks.0.webhook".to_owned()),
        "{keys:?}"
    );
}

#[test]
fn search_finds_by_key_help_or_value_and_jumps_there() {
    let mut t = T::new("");
    t.press("/");
    t.press("sidebar width");
    let rows = t.p.rows();
    assert!(matches!(rows[0], Row::Setting(..)));
    let Row::Setting(pr, i) = rows[0] else { panic!() };
    assert_eq!(t.p.entry(pr, i).key, "ui.sidebar_width");
    t.press("<Enter>");
    assert!(t.p.search.is_none());
    assert_eq!(t.selected().as_deref(), Some("ui.sidebar_width"));
    // By what it does.
    t.press("/");
    t.press("tailscale");
    let found: Vec<String> =
        t.p.rows()
            .iter()
            .filter_map(|r| match r {
                Row::Setting(pr, i) => Some(t.p.entry(*pr, *i).key.clone()),
                _ => None,
            })
            .collect();
    assert!(found.contains(&"remote.tailscale".to_owned()), "{found:?}");
    t.press("<Esc>");
    assert!(t.p.search.is_none());
    let mut u = T::new("");
    u.press("/zzzzzz");
    assert!(matches!(u.p.rows()[0], Row::Hint(_)));
}

#[test]
fn modified_only_and_advanced_filter_the_list() {
    let mut t = T::new("[daemon]\nframe_interval_ms = 20\n");
    t.press("<Enter>");
    t.p.jump("daemon.scrollback_lines");
    let n = t.p.rows().len();
    t.press("m");
    let only: Vec<String> =
        t.p.rows()
            .iter()
            .filter_map(|r| match r {
                Row::Setting(pr, i) => Some(t.p.entry(*pr, *i).key.clone()),
                _ => None,
            })
            .collect();
    assert!(only.is_empty() || only == ["daemon.frame_interval_ms"], "{only:?}");
    t.press("m");
    assert_eq!(t.p.rows().len(), n);
}

#[test]
fn a_setting_that_needs_a_restart_offers_one() {
    let mut t = T::new("");
    t.go("notifications.callback.listen");
    t.press("<Enter>127.0.0.1:7591<Enter>");
    assert!(t.note().contains("daemon restart"), "{}", t.note());
    assert!(t.p.restart.contains("notifications.callback.listen"));
    let acts = t.press("!");
    assert_eq!(acts, [Action::Run("daemon.restart")]);
    assert!(t.p.restart.is_empty());
}

#[test]
fn a_config_that_does_not_load_still_opens_with_the_reason() {
    let t = T::new("[ui]\nsidebar_width = 2\n");
    assert!(t.p.uview.problem.as_deref().unwrap().contains("sidebar_width"));
}

#[test]
fn a_repositorys_settings_go_to_your_local_file_or_the_shared_one() {
    let mut t = T::with("", true);
    // The repository's sections follow yours.
    t.p.sec = t.p.sections.iter().position(|s| s.id == "project").unwrap();
    t.p.focus = Pane::Items;
    t.p.cursor = 0;
    t.go("project.base");
    t.press("<Enter>origin/main<Enter>");
    assert!(t.local().contains("base = \"origin/main\""), "{}", t.local());
    assert!(t.note().contains("applies at once"));
    t.press("s");
    t.go("project.name");
    t.press("<Enter>demo<Enter>");
    let shared = std::fs::read_to_string(t.dir.path().join("repo/.ptyro.toml")).unwrap();
    assert!(shared.contains("name = \"demo\""), "{shared}");
    assert!(t.note().contains("committed"), "{}", t.note());
    // The value in effect says which file it came from; reset removes it where it is.
    let e = t.p.find_entry("project.base").unwrap();
    assert_eq!(e.origin, Origin::Local);
    t.go("project.base");
    t.press("r");
    assert!(!t.local().contains("base"), "{}", t.local());
    // Without a repository the sections say so.
    let mut n = T::new("");
    n.p.sec = n.p.sections.iter().position(|s| s.id == "project").unwrap();
    assert!(matches!(n.p.rows()[0], Row::Hint(_)));
}

#[test]
fn profiles_are_made_by_the_profile_commands() {
    let mut t = T::new("");
    t.p.sec = t.p.sections.iter().position(|s| s.id == "profiles").unwrap();
    t.p.focus = Pane::Items;
    t.p.cursor = t.p.first_selectable(0);
    assert_eq!(t.selected().as_deref(), Some("+profiles"));
    assert_eq!(t.press("<Enter>"), [Action::Run("profile.new")]);
}

#[test]
fn structured_settings_open_the_file_at_their_table() {
    let mut t = T::new("[agents.x]\nenv = { A = \"1\" }\n");
    t.go("agents.x.env");
    let acts = t.press("<Enter>");
    assert!(
        matches!(&acts[..], [Action::EditFile(p, Some(2))] if p.ends_with("config.toml")),
        "{acts:?}"
    );
}

#[test]
fn escape_walks_back_out_and_the_mouse_picks_rows() {
    let mut t = T::new("");
    assert_eq!(t.press("<Esc>"), [Action::Close]);
    let mut t = T::new("");
    t.press("<Enter>");
    assert_eq!(t.p.focus, Pane::Items);
    t.press("<Esc>");
    assert_eq!(t.p.focus, Pane::Sections);
    // Wheel moves the cursor in the items; a click outside closes.
    t.press("<Enter>");
    let before = t.p.cursor;
    *t.p.hits.borrow_mut() =
        Hits { panel: ratatui::layout::Rect::new(0, 0, 100, 30), ..Hits::default() };
    t.p.mouse(crossterm::event::MouseEventKind::ScrollDown, 50, 10);
    assert_eq!(t.p.cursor, before + 3);
    let acts = t.p.mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        150,
        50,
    );
    assert_eq!(acts, [Action::Close]);
}
