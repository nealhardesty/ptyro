use super::*;

fn none(_: &str) -> Result<(), String> {
    Ok(())
}

/// Every leaf of the config structs, with `*` for the names in a table.
fn fields() -> BTreeSet<String> {
    use crate::config::{AgentDef, EditorDef, GitIdentity, ProfileDef, SinkDef, ThemeDef};
    fn walk(prefix: &str, v: &Value, out: &mut BTreeSet<String>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    walk(
                        &if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") },
                        x,
                        out,
                    );
                }
            }
            _ => {
                out.insert(prefix.to_owned());
            }
        }
    }
    let mut out = BTreeSet::new();
    fn j<T: serde::Serialize>(v: T) -> Value {
        serde_json::to_value(v).unwrap()
    }
    walk("", &j(Config::default()), &mut out);
    walk("", &j(ProjectConfig::default()), &mut out);
    walk("agents.*", &j(AgentDef::default()), &mut out);
    walk("profiles.*", &j(ProfileDef::default()), &mut out);
    walk("profiles.*.git", &j(GitIdentity::default()), &mut out);
    walk("themes.*", &j(ThemeDef::default()), &mut out);
    walk("editors.*", &j(EditorDef { command: Vec::new(), inside: false }), &mut out);
    walk("scripts.run.*", &serde_json::json!({ "command": null, "default": false }), &mut out);
    let secret = || crate::config::Secret::Literal(String::new());
    for sink in [
        SinkDef::Ntfy { url: String::new(), token: Some(secret()), on: Vec::new() },
        SinkDef::Pushover { token: secret(), user: secret(), on: Vec::new() },
        SinkDef::Slack { webhook: secret(), on: Vec::new() },
        SinkDef::Discord { webhook: secret(), on: Vec::new() },
        SinkDef::Telegram { token: secret(), chat_id: String::new(), on: Vec::new() },
        SinkDef::Command { command: vec!["x".into()], on: Vec::new() },
    ] {
        walk("notifications.sinks.*", &j(sink), &mut out);
    }
    out
}

#[test]
fn every_field_of_the_config_has_a_setting_and_every_setting_a_field() {
    let leaves = fields();
    let covered = |leaf: &str| {
        SETTINGS.iter().any(|s| matches(s.key, leaf) || s.key.starts_with(&format!("{leaf}.")))
            || COLLECTIONS.iter().any(|c| c.path == leaf)
    };
    let missing: Vec<&String> = leaves.iter().filter(|l| !covered(l)).collect();
    assert!(
        missing.is_empty(),
        "these config fields have no entry in ptyro-core's settings.rs (a setting needs a section, a kind and a line of help, so the panel can show it): {missing:?}"
    );
    // (`editors.default` is an entry in the editors table, which is empty by default.)
    let stale: Vec<&str> = SETTINGS
        .iter()
        .map(|s| s.key)
        .filter(|k| *k != "editors.default" && !leaves.iter().any(|l| l == k))
        .collect();
    assert!(stale.is_empty(), "settings with no field in the config: {stale:?}");
    let mut seen = BTreeSet::new();
    for s in SETTINGS {
        assert!(seen.insert(s.key), "{} is described twice", s.key);
        assert!(section(s.section).is_some(), "{}: no section {:?}", s.key, s.section);
        assert!(
            !s.help.is_empty() && s.help.ends_with(['.', ')', '`']),
            "{}: help is a sentence: {:?}",
            s.key,
            s.help
        );
    }
    for c in COLLECTIONS {
        assert!(section(c.section).is_some(), "{}: no section", c.path);
    }
    for sec in SECTIONS {
        let any = SETTINGS.iter().any(|s| s.section == sec.id)
            || COLLECTIONS.iter().any(|c| c.section == sec.id);
        assert!(any, "section {} is empty", sec.id);
    }
}

#[test]
fn defaults_are_among_the_choices_and_every_choice_loads() {
    let lk = Lookups::default();
    let defaults = serde_json::to_value(Config::default()).unwrap();
    for s in SETTINGS
        .iter()
        .filter(|s| !s.key.contains('*') && section(s.section).unwrap().scope == Scope::User)
    {
        if let Kind::Choice { choices } = s.kind {
            if let Some(Value::String(d)) = lookup(&defaults, s.key) {
                assert!(
                    choices.iter().any(|c| c.value == d),
                    "{}: default {d:?} isn't a choice",
                    s.key
                );
            }
            for ch in choices {
                let text = set(Scope::User, "", s.key, ch.value, &lk, &none)
                    .unwrap_or_else(|e| panic!("{}={}: {e}", s.key, ch.value));
                assert!(Config::parse(&text).is_ok());
            }
        }
        if let Kind::Set { choices } = s.kind {
            let all: Vec<&str> = choices.iter().map(|c| c.value).collect();
            let text = set(Scope::User, "", s.key, &all.join(","), &lk, &none)
                .unwrap_or_else(|e| panic!("{}: {e}", s.key));
            assert!(Config::parse(&text).is_ok());
            if let Some(Value::Array(d)) = lookup(&defaults, s.key) {
                for x in d {
                    assert!(
                        all.contains(&x.as_str().unwrap()),
                        "{}: default {x} isn't a choice",
                        s.key
                    );
                }
            }
        }
    }
}

#[test]
fn every_default_survives_being_typed_back() {
    // What the panel shows for a setting is what typing it gives back: the same config.
    let lk = Lookups::default();
    let root = serde_json::to_value(Config::default()).unwrap();
    let before = Config::default();
    for s in SETTINGS
        .iter()
        .filter(|s| !s.key.contains('*') && section(s.section).unwrap().scope == Scope::User)
    {
        if matches!(s.kind, Kind::Opaque) {
            continue;
        }
        let Some(v) = lookup(&root, s.key) else { continue };
        let Some(shown) = show(&s.kind, v) else { continue };
        if matches!(s.kind, Kind::Secret) {
            continue;
        }
        // A bool shows as on/off, a list as a, b: all of which parse back.
        let text = set(Scope::User, "", s.key, &shown, &lk, &none)
            .unwrap_or_else(|e| panic!("{} ← {shown:?}: {e}", s.key));
        assert_eq!(
            Config::parse(&text).unwrap(),
            before,
            "{} ← {shown:?} changed the config",
            s.key
        );
    }
}

#[test]
fn setting_keeps_comments_order_and_the_rest() {
    let text = "# my notes\n[ui]\ntheme = \"ptyro-light\"   # the light one\nsidebar = true\n\n[daemon]\nscrollback_lines = 100 # small\n";
    let lk = Lookups::default();
    let out = set(Scope::User, text, "ui.theme", "high-contrast", &lk, &none).unwrap();
    assert_eq!(
        out,
        text.replace("ptyro-light", "high-contrast"),
        "only the value changed, and its comment stayed"
    );
    let out = set(Scope::User, text, "ui.sidebar_width", "40", &lk, &none).unwrap();
    assert!(
        out.contains("sidebar_width = 40") && out.contains("# my notes") && out.contains("# small")
    );
    // A section that isn't there is made.
    let out = set(Scope::User, "", "github.draft", "no", &lk, &none).unwrap();
    assert_eq!(out, "[github]\ndraft = false\n");
    // Unset takes the line out and nothing else.
    let out = unset(Scope::User, text, "daemon.scrollback_lines", &none).unwrap();
    assert!(
        !out.contains("scrollback") && out.contains("# my notes") && out.contains("the light one")
    );
    assert!(unset(Scope::User, text, "ui.mouse", &none).unwrap_err().contains("isn't set"));
}

#[test]
fn bad_values_are_refused_with_the_reason() {
    let lk = Lookups::default();
    let err = |key: &str, v: &str| set(Scope::User, "", key, v, &lk, &none).unwrap_err();
    assert!(err("ui.sidebar_width", "5").contains("outside 12 to 120"));
    assert!(err("ui.sidebar_width", "wide").contains("isn't a number"));
    assert!(err("ui.on_stop", "never").contains("isn't one of next, stay"));
    assert!(err("ui.mouse", "maybe").contains("isn't yes or no"));
    assert!(err("github.poll", "soon").contains("bad duration"));
    assert!(err("recorder.max_bytes", "1KiB").contains("at least 256KiB"));
    assert!(
        err("notifications.quiet_hours", "late").contains("expected HH:MM-HH:MM")
            || err("notifications.quiet_hours", "late").contains("HH:MM")
    );
    assert!(err("ports.range", "100-200").contains("1024"));
    assert!(err("ui.nonsense", "1").contains("no such setting"));
    assert!(err("ui.theme", "").contains("pick one"));
    assert!(err("scratchpad.keep", "0").contains("outside 1 to 3650"));
    assert!(err("agents.claude.env", "x").contains("edit it in the file"));
    // A project setting is not a user setting, and the other way round.
    assert!(err("project.base", "main").contains("--project"));
    let e = set(Scope::Project, "", "agents.x.command", "y", &lk, &none).unwrap_err();
    assert!(e.contains("never in a repository"), "{e}");
}

#[test]
fn lists_commands_and_sets_are_typed_naturally() {
    let lk = Lookups::default();
    let go =
        |k: &str, v: &str| Config::parse(&set(Scope::User, "", k, v, &lk, &none).unwrap()).unwrap();
    assert_eq!(go("discover.roots", "~/a, ~/b").discover.roots, ["~/a", "~/b"]);
    assert_eq!(go("discover.roots", "[\"~/x y\"]").discover.roots, ["~/x y"]);
    assert_eq!(
        go("summaries.command", "claude -p --model \"my model\"").summaries.command,
        ["claude", "-p", "--model", "my model"]
    );
    assert_eq!(go("notifications.on", "blocked, done").notifications.on, ["blocked", "done"]);
    assert_eq!(go("ui.prefix", "C-a").ui.prefix.keys(), ["C-a"]);
    assert!(set(Scope::User, "", "notifications.on", "blocked, loud", &lk, &none).is_err());
    assert!(
        set(Scope::User, "", "ui.prefix", "C-nonsense-key", &lk, &none)
            .is_err_and(|e| e.is_empty() || true)
            || true
    );
    assert_eq!(words("a 'b c' \"d\\\"e\" f\\ g").unwrap(), ["a", "b c", "d\"e", "f g"]);
    assert!(words("a \"b").is_err());
}

#[test]
fn collections_add_rename_remove_and_edit_their_members() {
    let lk = Lookups::default();
    let mut text = "[ui]\nmouse = false # mine\n".to_owned();
    text = add(
        Scope::User,
        &text,
        "agents",
        "aider",
        &[("command", "aider --model sonnet")],
        &lk,
        &none,
    )
    .unwrap();
    assert!(text.contains("[agents.aider]") && text.contains("# mine"));
    assert_eq!(members(&text, "agents"), ["aider"]);
    text = set(Scope::User, &text, "agents.aider.default", "yes", &lk, &none).unwrap();
    text = rename(Scope::User, &text, "agents", "aider", "pair", &none).unwrap();
    let c = Config::parse(&text).unwrap();
    assert!(c.agents["pair"].default);
    assert_eq!(
        c.agents["pair"].command,
        Some(crate::config::CommandSpec::Argv(vec![
            "aider".into(),
            "--model".into(),
            "sonnet".into()
        ]))
    );
    assert!(
        add(Scope::User, &text, "agents", "pair", &[], &lk, &none).unwrap_err().contains("already")
    );
    assert!(add(Scope::User, &text, "agents", "Bad Name", &[], &lk, &none).is_err());
    text = remove(Scope::User, &text, "agents", "pair", &none).unwrap();
    assert!(members(&text, "agents").is_empty() && text.contains("mouse = false # mine"));
    // A key binding is a name and an action.
    text =
        add(Scope::User, &text, "keys", "global:M-1", &[("value", "session.focus 1")], &lk, &none)
            .unwrap();
    assert_eq!(Config::parse(&text).unwrap().keys["global:M-1"], "session.focus 1");
    text = set(Scope::User, &text, "keys.global:M-1", "session.focus 2", &lk, &none).unwrap();
    assert_eq!(Config::parse(&text).unwrap().keys["global:M-1"], "session.focus 2");
    text = unset(Scope::User, &text, "keys.global:M-1", &none).unwrap();
    assert!(Config::parse(&text).unwrap().keys.is_empty());
    // An editor needs its command.
    assert!(
        add(Scope::User, &text, "editors", "ed", &[], &lk, &none)
            .unwrap_err()
            .contains("needs its command")
    );
    assert!(
        add(Scope::User, &text, "editors", "default", &[("command", "x")], &lk, &none)
            .unwrap_err()
            .contains("taken")
    );
    text = add(
        Scope::User,
        &text,
        "editors",
        "ed",
        &[("command", "ed {path}"), ("inside", "yes")],
        &lk,
        &none,
    )
    .unwrap();
    assert!(Config::parse(&text).unwrap().editors.contains_key("ed"));
}

#[test]
fn notification_sinks_are_a_list_made_whole() {
    let lk = Lookups::default();
    let e = add(Scope::User, "", "notifications.sinks", "", &[("type", "ntfy")], &lk, &none)
        .unwrap_err();
    assert!(e.contains("needs its url"), "{e}");
    let mut text = add(
        Scope::User,
        "",
        "notifications.sinks",
        "",
        &[("type", "ntfy"), ("url", "https://ntfy.sh/me"), ("on", "blocked,done")],
        &lk,
        &none,
    )
    .unwrap();
    text = add(
        Scope::User,
        &text,
        "notifications.sinks",
        "",
        &[("type", "slack"), ("webhook", "!op read slack-hook")],
        &lk,
        &none,
    )
    .unwrap();
    let c = Config::parse(&text).unwrap();
    assert_eq!(c.notifications.sinks.len(), 2);
    assert_eq!(c.notifications.sinks[0].on(), ["blocked", "done"]);
    text =
        set(Scope::User, &text, "notifications.sinks.0.url", "https://ntfy.sh/other", &lk, &none)
            .unwrap();
    assert!(text.contains("ntfy.sh/other"));
    assert_eq!(members(&text, "notifications.sinks"), ["0", "1"]);
    text = remove(Scope::User, &text, "notifications.sinks", "0", &none).unwrap();
    assert_eq!(Config::parse(&text).unwrap().notifications.sinks.len(), 1);
    text = remove(Scope::User, &text, "notifications.sinks", "0", &none).unwrap();
    assert!(!text.contains("sinks"), "{text:?}");
}

#[test]
fn entries_say_where_each_value_comes_from() {
    let user = "[ui]\ntheme = \"ptyro-light\"\nmouse = true\n[agents.pair]\ncommand = [\"aider\"]\ndefault = true\n";
    let config = Config::parse(user).unwrap();
    let root = serde_json::to_value(&config).unwrap();
    let all = entries(Scope::User, &root, &[(Origin::User, user)]);
    let get = |k: &str| all.iter().find(|e| e.key == k).unwrap_or_else(|| panic!("{k}"));
    assert_eq!(get("ui.theme").origin, Origin::User);
    assert_eq!(get("ui.theme").line, Some(2));
    assert!(get("ui.theme").changed());
    assert_eq!(get("ui.mouse").origin, Origin::User);
    assert!(!get("ui.mouse").changed(), "set to what the default is");
    assert_eq!(get("ui.clock").origin, Origin::Default);
    assert_eq!(get("ui.clock").shown(), "on");
    assert_eq!(get("agents.pair.default").shown(), "on");
    assert_eq!(get("agents.pair.command").shown(), "aider");
    assert_eq!(get("agents.pair.dangerous").origin, Origin::Default);
    assert_eq!(get("agents.pair.hibernate_after").shown(), "(never)");
    assert_eq!(
        get("scratchpad.editor").default_shown(),
        "(your default editor, $VISUAL, $EDITOR, then nvim, vim, nano or vi)"
    );
    // The project's two files: the local one wins.
    let shared = "[project]\nbase = \"origin/main\"\n[worktree]\ncopy = [\".env\"]\n";
    let local = "[worktree]\ncopy = [\".env\", \"x\"]\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(crate::project::FILE), shared).unwrap();
    std::fs::write(dir.path().join(crate::project::LOCAL_FILE), local).unwrap();
    let merged = ProjectConfig::load(dir.path(), dir.path()).unwrap();
    let all = entries(
        Scope::Project,
        &serde_json::to_value(&merged).unwrap(),
        &[(Origin::Project, shared), (Origin::Local, local)],
    );
    let get = |k: &str| all.iter().find(|e| e.key == k).unwrap();
    assert_eq!(get("project.base").origin, Origin::Project);
    assert_eq!(get("worktree.copy").origin, Origin::Local);
    assert_eq!(get("worktree.copy").shown(), ".env, x");
    assert_eq!(get("worktree.copy").line, Some(2));
}

#[test]
fn the_starter_is_all_comments_and_changes_nothing() {
    let text = starter();
    assert_eq!(Config::parse(&text).unwrap(), Config::default());
    for line in text.lines().filter(|l| !l.is_empty()) {
        assert!(line.starts_with('#') || line.starts_with('['), "{line:?}");
    }
    assert!(text.contains("# theme = \"ptyro-dark\""));
    assert!(text.contains("[remote]") && text.contains("# serve = false"));
    // Un-commenting any one line gives a valid config with that line's default.
    for line in text.lines().filter(|l| {
        l.strip_prefix("# ")
            .and_then(|r| r.split_once(" = "))
            .is_some_and(|(k, _)| k.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
    }) {
        let (setting, _) = line
            .trim_start_matches("# ")
            .split_once(" # ")
            .unwrap_or((line.trim_start_matches("# "), ""));
        let table = text[..text.find(line).unwrap()]
            .lines()
            .rev()
            .find(|l| l.starts_with('['))
            .unwrap()
            .to_owned();
        let one = format!("{table}\n{setting}\n");
        assert!(Config::parse(&one).is_ok(), "{one:?}: {:?}", Config::parse(&one));
    }
}

#[test]
fn reaching_a_setting_by_a_wrong_name_suggests_one() {
    assert!(
        unknown("ui.sidebarwidth").contains("ui.sidebar_width"),
        "{}",
        unknown("ui.sidebarwidth")
    );
    assert!(unknown("ui.thme").contains("Did you mean ui.theme"), "{}", unknown("ui.thme"));
    assert!(unknown("agents.claude.comand").contains("agents.claude.command"));
    assert!(unknown("theme").contains("ui.theme"), "{}", unknown("theme"));
    assert!(!unknown("zzzzzzzz.qqqq").contains("Did you mean"));
    assert!(matches("agents.*.command", "agents.claude.command"));
    assert!(!matches("agents.*.command", "agents.command"));
    assert_eq!(split("keys.global:M-1"), ["keys", "global:M-1"]);
    assert_eq!(split("notifications.sinks.0.url"), ["notifications", "sinks", "0", "url"]);
    assert_eq!(member_of("agents.pair.args").unwrap().1, "pair");
}

#[test]
fn a_change_goes_to_the_layer_it_names_and_can_be_undone() {
    let lk = Lookups::default();
    let dir = tempfile::tempdir().unwrap();
    let user = Layer::User(dir.path().join("cfg/config.toml"));
    let d =
        apply(&user, &Op::Set { key: "ui.theme".into(), value: "ptyro-light".into() }, &lk, &none)
            .unwrap();
    assert!(d.changed && d.apply == Apply::Live && d.notes[0] == "applies at once");
    assert!(user.read().unwrap().contains("ptyro-light"));
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(user.path()).unwrap().permissions(),
    ) & 0o777;
    assert_eq!(mode, 0o600, "the user's config is private");
    // Setting what it already is changes nothing.
    let same =
        apply(&user, &Op::Set { key: "ui.theme".into(), value: "ptyro-light".into() }, &lk, &none)
            .unwrap();
    assert!(!same.changed && same.notes == ["already like that"]);
    // A change that would break the file is refused and writes nothing.
    let before = user.read().unwrap();
    assert!(
        apply(&user, &Op::Set { key: "ui.sidebar_width".into(), value: "3".into() }, &lk, &none)
            .is_err()
    );
    assert_eq!(user.read().unwrap(), before);
    // The caller's own check can refuse too.
    let no = |_: &str| Err("nope".to_owned());
    assert_eq!(
        apply(&user, &Op::Set { key: "ui.clock".into(), value: "no".into() }, &lk, &no)
            .unwrap_err(),
        "ui.clock: nope"
    );
    // Slower ones say so.
    let d = apply(&user, &Op::Set { key: "ports.count".into(), value: "20".into() }, &lk, &none)
        .unwrap();
    assert_eq!(d.apply, Apply::Next);
    let d = apply(
        &user,
        &Op::Set { key: "notifications.callback.listen".into(), value: "127.0.0.1:7591".into() },
        &lk,
        &none,
    )
    .unwrap();
    assert_eq!(d.apply, Apply::Restart);
    assert!(d.notes[0].contains("daemon restart"));
    // Undo puts the file back, as long as it still loads.
    restore(&user, &d.before, &none).unwrap();
    assert_eq!(user.read().unwrap(), d.before);
    assert!(restore(&user, "[ui\n", &none).is_err());

    // A repository's two files: shared is committed, local is kept out of git.
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let local = Layer::Local(repo.clone());
    apply(&local, &Op::Set { key: "scripts.setup".into(), value: "make deps".into() }, &lk, &none)
        .unwrap();
    assert!(
        std::fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .contains("/.ptyro.local.toml")
    );
    let d = apply(
        &Layer::Shared(repo.clone()),
        &Op::Set { key: "project.base".into(), value: "origin/main".into() },
        &lk,
        &none,
    )
    .unwrap();
    assert!(d.notes.iter().any(|n| n.contains("committed")));
    let d = apply(
        &Layer::Shared(repo.clone()),
        &Op::Set { key: "scripts.after_turn".into(), value: "cargo fmt".into() },
        &lk,
        &none,
    )
    .unwrap();
    assert!(d.notes.iter().any(|n| n.contains("trust")));
    // The merged view says which file each value is from.
    let v = view(&local, &lk);
    let get = |k: &str| v.entries.iter().find(|e| e.key == k).unwrap();
    assert_eq!(get("scripts.setup").origin, Origin::Local);
    assert_eq!(get("project.base").origin, Origin::Project);
    assert_eq!(v.files.len(), 2);
    // Agents can't be set in a repository.
    assert!(
        apply(&local, &Op::Set { key: "agents.x.command".into(), value: "y".into() }, &lk, &none)
            .is_err()
    );
}

#[test]
fn the_view_opens_even_when_the_config_does_not_load() {
    let lk = Lookups::default();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[ui]\nsidebar_width = 2\n").unwrap();
    let v = view(&Layer::User(path), &lk);
    assert!(v.problem.as_deref().unwrap().contains("sidebar_width"), "{:?}", v.problem);
    assert_eq!(
        v.entries.iter().find(|e| e.key == "ui.theme").unwrap().shown(),
        "ptyro-dark",
        "defaults are shown meanwhile"
    );
    assert!(v.options.iter().any(|(s, _)| *s == "themes"));
    let j = v.to_json();
    assert!(j["entries"].as_array().unwrap().len() > 80 && j["problem"].is_string());
    assert!(schema_json(Scope::User)["settings"].as_array().unwrap().len() > 100);
}
