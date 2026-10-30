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
            Row::Key(b) => Some(format!("keys.{}", b.name)),
            Row::AddKey(t) => Some(format!("+keys:{t}")),
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
    let mut t = T::new("[ui]\nprefix = \"C-a\"\n");
    assert_eq!(t.p.section().id, "interface");
    t.press("<Enter>");
    assert_eq!(t.selected().as_deref(), Some("ui.prefix"));
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
    // (It asks which program it runs; ⏎ skips that.)
    t.press("<Enter>aider<Enter><Enter>");
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
    t.p.jump("keys.y");
    // Jumping to a binding that isn't there lands on the section.
    assert_eq!(t.p.section().id, "keys");
    t.press("n");
    assert!(matches!(t.p.modal, Some(Modal::Capture { .. })));
    t.press("y");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("an action list") };
    assert!(p.items.len() > 100 && p.search);
    t.press("session.next");
    t.press("<Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert_eq!(c.keys["y"], "session.next", "{}", t.file());
    // It shows, and ⏎ changes what it runs; d unbinds it.
    t.p.jump("keys.y");
    assert_eq!(t.selected().as_deref(), Some("keys.y"));
    t.press("d");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty());
}

fn key_row(t: &T, name: &str) -> Binding {
    t.p.uview
        .bindings
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no key {name}"))
        .clone()
}

#[test]
fn the_key_bindings_section_lists_every_default_key_by_when_it_works() {
    let t = T::new("");
    let mut t = t;
    t.p.jump("keys.d");
    assert_eq!(t.selected().as_deref(), Some("keys.d"));
    let rows = t.p.rows();
    let titles: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Title(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(titles.iter().any(|s| s.starts_with("After the prefix key (C-q")), "{titles:?}");
    assert!(titles.iter().any(|s| s.starts_with("In the sidebar")), "{titles:?}");
    assert!(titles.iter().any(|s| s.starts_with("Anywhere")), "{titles:?}");
    let keys: Vec<&Binding> =
        rows.iter().filter_map(|r| if let Row::Key(b) = r { Some(b) } else { None }).collect();
    assert_eq!(keys.len(), keymap::default_keys().len() - 2 * 8, "the digits are one row each");
    let c = keys.iter().find(|b| b.name == "C").unwrap();
    assert_eq!((c.action.as_str(), c.standing), ("config.panel", Standing::Default));
    assert!(c.help.starts_with("Settings"), "{}", c.help);
    assert!(rows.iter().any(|r| matches!(r, Row::AddKey("global"))));
}

#[test]
fn searching_finds_a_key_by_what_it_does() {
    let mut t = T::new("");
    for q in ["detach", "settings", "shortcut"] {
        t.press(&format!("/{q}"));
        let found: Vec<String> =
            t.p.rows()
                .iter()
                .filter_map(|r| if let Row::Key(b) = r { Some(b.action.clone()) } else { None })
                .collect();
        match q {
            "detach" => assert!(found.contains(&"app.detach".to_owned()), "{found:?}"),
            "settings" => assert!(found.contains(&"config.panel".to_owned()), "{found:?}"),
            _ => assert!(found.len() > 50),
        }
        t.press("<Esc>");
    }
    // Choosing the hit goes to the key.
    t.press("/detach<Enter>");
    assert!(t.selected().is_some_and(|s| s.starts_with("keys.")), "{:?}", t.selected());
    assert_eq!(t.p.section().id, "keys");
}

#[test]
fn a_default_key_is_unbound_restored_and_given_another_action_with_one_undo_each() {
    let mut t = T::new("# mine\n[ui]\nmouse = false # keep\n");
    t.p.jump("keys.d");
    t.press("d");
    assert_eq!(Config::parse(&t.file()).unwrap().keys["d"], "none", "{}", t.file());
    assert!(t.file().contains("# mine") && t.file().contains("# keep"));
    assert_eq!(key_row(&t, "d").standing, Standing::Unbound);
    assert_eq!(t.selected().as_deref(), Some("keys.d"), "the cursor stays on the key");
    // An unbound default stays on the list; r brings it back.
    t.press("r");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty(), "{}", t.file());
    assert_eq!(key_row(&t, "d").standing, Standing::Default);
    // ⏎ is a menu: change what it runs.
    t.press("<Enter>");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("a menu") };
    let values: Vec<&str> = p.items.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(values, ["key", "action", "unbind"], "nothing to reset on a default");
    t.press("<Down><Enter>");
    t.press("session.next<Enter>");
    let b = key_row(&t, "d");
    assert_eq!((b.standing, b.action.as_str()), (Standing::Changed, "session.next"));
    assert_eq!(
        t.p.uview.bindings.iter().filter(|b| b.key == "d" && b.table == "prefix").count(),
        1
    );
    // Reset, from the menu this time.
    t.press("<Enter>");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("a menu") };
    assert!(p.items.iter().any(|(v, l)| v == "reset" && l.contains("Detach")), "{:?}", p.items);
    t.press("<Down><Down><Down><Enter>");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty(), "{}", t.file());
    // Undo works like any other change.
    t.press("u");
    assert_eq!(key_row(&t, "d").standing, Standing::Changed);
    t.press("uuu");
    assert_eq!(t.file(), "# mine\n[ui]\nmouse = false # keep\n");
}

#[test]
fn changing_a_key_moves_the_binding_and_a_taken_key_asks_before_swapping() {
    let mut t = T::new("");
    t.p.jump("keys.d");
    // A free key: the action moves, the old key is off.
    t.press("<Enter><Enter>");
    assert!(matches!(t.p.modal, Some(Modal::Capture { .. })));
    t.press("y");
    let keys = Config::parse(&t.file()).unwrap().keys;
    assert_eq!((keys["y"].as_str(), keys["d"].as_str()), ("app.detach", "none"), "{}", t.file());
    assert_eq!(t.selected().as_deref(), Some("keys.y"));
    // Putting it back is a reset in the file.
    t.press("<Enter><Enter>d");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty(), "{}", t.file());
    // A taken key: the person is asked, naming what is there; n changes nothing.
    t.p.jump("keys.d");
    t.press("<Enter><Enter>n");
    let Some(Modal::Confirm { text, .. }) = &t.p.modal else {
        panic!("a question: {:?}", t.p.modal)
    };
    assert!(text.contains("already runs") && text.contains("Swap them"), "{text}");
    t.press("n");
    assert!(t.file().is_empty() || Config::parse(&t.file()).unwrap().keys.is_empty());
    // y swaps: n runs detach and d runs what n did, in one undo.
    t.p.jump("keys.d");
    t.press("<Enter><Enter>ny");
    let keys = Config::parse(&t.file()).unwrap().keys;
    assert_eq!(
        (keys["n"].as_str(), keys["d"].as_str()),
        ("app.detach", "session.new"),
        "{}",
        t.file()
    );
    t.press("u");
    assert!(Config::parse(&t.file()).unwrap().keys.is_empty(), "{}", t.file());
}

#[test]
fn adding_a_key_that_is_taken_warns_and_a_nav_key_is_added_in_its_own_group() {
    let mut t = T::new("");
    t.p.jump("keys.d");
    t.press("n");
    t.press("C");
    let Some(Modal::Confirm { text, .. }) = &t.p.modal else { panic!("a question") };
    assert!(text.contains("Settings"), "names what is there: {text}");
    t.press("<Esc>");
    assert!(t.p.modal.is_none() && t.file().is_empty());
    // On a sidebar key, n adds a sidebar key.
    t.p.jump("keys.nav:j");
    t.press("n");
    t.press("y");
    t.press("session.next<Enter>");
    assert_eq!(Config::parse(&t.file()).unwrap().keys["nav:y"], "session.next", "{}", t.file());
    assert_eq!(key_row(&t, "nav:y").standing, Standing::Yours);
}

#[test]
fn the_nine_session_digits_are_one_row_that_says_how_to_change_them() {
    let mut t = T::new("");
    t.p.jump("keys.d");
    let rows = t.p.rows();
    let fixed: Vec<&Binding> = rows
        .iter()
        .filter_map(|r| if let Row::Key(b) = r { Some(b).filter(|b| b.fixed) } else { None })
        .collect();
    assert_eq!(fixed.len(), 2, "after the prefix and in the sidebar");
    let at = rows.iter().position(|r| matches!(r, Row::Key(b) if b.fixed)).unwrap();
    t.p.cursor = at;
    t.press("<Enter>");
    assert!(t.p.modal.is_none() && t.note().contains("session.focus"), "{}", t.note());
    t.press("d");
    assert!(t.file().is_empty());
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
    assert!(found.contains(&"remote.access".to_owned()), "{found:?}");
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

#[test]
fn the_themes_section_picks_the_theme_and_a_new_theme_starts_from_one_you_choose() {
    let mut t = T::new("");
    t.go("ui.theme");
    assert_eq!(t.p.section().id, "themes");
    t.press("<Enter>");
    assert!(matches!(t.p.modal, Some(Modal::Pick(_))));
    t.press("<Esc>");
    let col = settings::collection("themes").unwrap();
    t.p.add(col);
    for c in "mine".chars() {
        t.press(&c.to_string());
    }
    t.press("<Enter>");
    let Some(Modal::Pick(p)) = &t.p.modal else { panic!("no base picker") };
    assert!(p.title.contains("mine") && p.items.iter().any(|(v, _)| v == "ptyro-light"));
    t.press("j");
    t.press("<Enter>");
    assert!(
        t.file().contains("[themes.mine]") && t.file().contains("base = \"ptyro-light\""),
        "{}",
        t.file()
    );
}

#[test]
fn a_new_agent_asks_which_program_it_runs_and_a_new_theme_is_put_in_use() {
    let mut t = T::new("");
    t.p.add(settings::collection("agents").unwrap());
    t.press("pair<Enter>");
    assert!(
        matches!(&t.p.modal, Some(Modal::Input { hint, .. }) if hint.contains("⏎ to run it as"))
    );
    t.press("aider --model sonnet<Enter>");
    let c = Config::parse(&t.file()).unwrap();
    assert_eq!(c.agents["pair"].command.as_ref().unwrap().argv(), ["aider", "--model", "sonnet"]);

    t.p.add(settings::collection("themes").unwrap());
    t.press("mine<Enter><Enter>");
    assert!(t.file().contains("theme = \"mine\""), "{}", t.file());
    assert!(t.note().contains("now in use"), "{}", t.note());
    // Removing the theme in use says so, and the default comes back instead of a broken config.
    t.p.drill = None;
    t.p.ask_remove("themes", "mine");
    assert!(matches!(&t.p.modal, Some(Modal::Confirm { text, .. }) if text.contains("in use")));
    t.press("y");
    assert!(!t.file().contains("mine") && !t.file().contains("theme ="), "{}", t.file());
    assert!(t.note().contains("default theme is back"), "{}", t.note());
}

#[test]
fn an_empty_group_says_what_it_is_for_and_numbers_say_what_they_count() {
    let mut t = T::new("");
    t.go("ui.sidebar_width");
    let Some(Row::Setting(p, i)) = t.p.rows().get(t.p.cursor).cloned() else { panic!() };
    assert_eq!(t.p.entry(p, i).pretty(), "30 columns");
    assert_eq!(label_of(t.p.entry(p, i), false, false), "sidebar width");
    let agents = t.p.sections.iter().position(|s| s.id == "agents").unwrap();
    t.p.set_section(agents);
    let hints: Vec<String> =
        t.p.rows()
            .into_iter()
            .filter_map(|r| if let Row::Hint(h) = r { Some(h) } else { None })
            .collect();
    assert!(hints.join(" ").contains("work as they are"), "{hints:?}");
}

#[test]
fn search_finds_a_setting_by_the_words_people_type() {
    let mut t = T::new("");
    for word in ["color", "notify", "shortcut"] {
        t.p.search = Some(LineEdit::with(word));
        let n = t.p.rows().iter().filter(|r| matches!(r, Row::Setting(..))).count();
        assert!(n > 0, "{word} finds something");
    }
    t.p.search = Some(LineEdit::with("sound"));
    let keys: Vec<String> =
        t.p.rows()
            .iter()
            .filter_map(|r| {
                if let Row::Setting(p, i) = r { Some(t.p.entry(*p, *i).key.clone()) } else { None }
            })
            .collect();
    assert!(keys.contains(&"notifications.channels".to_owned()), "{keys:?}");
}

#[test]
fn a_draft_of_several_lines_is_edited_a_line_at_a_time() {
    use ptyro_core::input::{KeyCode, KeyEvent, Mods};
    let mut e = LineEdit::with("first line\nsecond one");
    // C-w keeps to the cursor's line, as C-u and C-k do.
    e.at = 11;
    assert!(e.key(KeyEvent::new(KeyCode::Char { c: 'w' }, Mods(Mods::CTRL))));
    assert_eq!(e.text, "first line\nsecond one", "nothing before it on its line");
    e.at = e.text.chars().count();
    e.key(KeyEvent::new(KeyCode::Char { c: 'w' }, Mods(Mods::CTRL)));
    assert_eq!(e.text, "first line\nsecond ");
    // Up and down keep the column as far as the line allows; none past the ends.
    e.at = 13;
    assert!(e.vertical(true));
    assert_eq!(e.at, 2);
    assert!(!e.vertical(true));
    e.at = 9;
    assert!(e.vertical(false));
    assert_eq!(e.at, e.text.chars().count(), "the shorter line's end");
    assert!(!e.vertical(false));
}
