//! CFG1 end-to-end tests (ROADMAP CFG1): the settings panel in a real terminal against a live
//! daemon, and `ptyro config get|set|unset|list|add|rm` on the same files.

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

const MINE: &str = "# my settings\n[ui]\nmouse = false # I use my terminal's selection\n\n[daemon]\nscrollback_lines = 5000\n";

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The panel is `C-q C`: a switch, a choice, a number and a list are changed from it. The file
/// keeps its comments and the rest, and the running daemon uses the new list at once.
#[tokio::test]
async fn the_panel_changes_a_switch_a_choice_a_number_and_a_list_and_the_daemon_uses_them() {
    let home = tempfile::Builder::new().prefix("ptyro-cfg1-").tempdir().unwrap();
    let h = home.path().canonicalize().unwrap();
    let hs = h.to_string_lossy().into_owned();
    // A repository under a folder discovery doesn't look in yet.
    let work = h.join("work/app");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    let d = TempDaemon::start_with(BIN, &[("HOME", hs.as_str())], Some(MINE)).unwrap();
    let discover = |d: &TempDaemon| {
        let o = d.cmd().args(["project", "discover", "--refresh", "--json"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert!(!discover(&d).contains("work/app"), "not found under the default folders");

    let mut s = ptyro_testkit::headless::Script::new(vec![BIN.to_owned()])
        .size(120, 36)
        .timeout(60.0)
        .env("HOME", &hs);
    for (k, v) in d.env() {
        s = s.env(&k, &v);
    }
    let r = ptyro_testkit::headless::run(
        s.send_when("WORKSPACE", "\x11C")
            // The panel; ⏎ moves to the settings. Down to `clock` and ⏎ turns it off.
            .send_when("Settings — your config", "\rjjjjj\r")
            // `on_stop` is five below it: pick `stay` from its list.
            .send_when("ui.clock = off", "jjjjj\rj\r")
            // A number: search for it, jump there, and step it.
            .send_when("ui.on_stop = stay", "/sidebar_width\r+")
            // A list: search, jump, ⏎ opens the box on the current value; clear it and retype.
            .send_when("ui.sidebar_width = 31", "/discover.roots\r\r\x15~/work, ~/play\r")
            .until("discover.roots = ~/work, ~/play"),
    )
    .await
    .unwrap();
    assert!(r.screen.contains("discover.roots = ~/work, ~/play"), "{}", r.screen);

    let file = std::fs::read_to_string(d.paths.config.join("config.toml")).unwrap();
    // Changed.
    assert!(file.contains("clock = false"), "{file}");
    assert!(file.contains("on_stop = \"stay\""), "{file}");
    assert!(file.contains("sidebar_width = 31"), "{file}");
    assert!(file.contains("roots = [\"~/work\", \"~/play\"]"), "{file}");
    // Kept: the comments, the order, the values it didn't touch.
    assert!(
        file.starts_with("# my settings\n[ui]\nmouse = false # I use my terminal's selection\n"),
        "{file}"
    );
    assert!(file.contains("[daemon]\nscrollback_lines = 5000\n"), "{file}");
    // The running daemon reloaded the file and uses the new folders.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !discover(&d).contains("work/app") {
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon never used the new discover.roots"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `config set` and friends: checked, commented files kept, the reason for a refusal, the way
/// the setting applies, and a repository's own file with `--project`.
#[tokio::test]
async fn config_get_set_unset_list_add_and_rm_on_the_command_line() {
    let home = tempfile::Builder::new().prefix("ptyro-cfg1-").tempdir().unwrap();
    let h = home.path().canonicalize().unwrap();
    let hs = h.to_string_lossy().into_owned();
    let repo = h.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "a"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let d = TempDaemon::start_with(BIN, &[("HOME", hs.as_str())], Some(MINE)).unwrap();
    let ptyro = |args: &[&str]| d.cmd().args(args).current_dir(&repo).output().unwrap();
    let file = || std::fs::read_to_string(d.paths.config.join("config.toml")).unwrap();

    // set: the file keeps its comments; the note says when it applies.
    let o = ptyro(&["config", "set", "ui.theme", "ptyro-light"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("applies at once"), "{}", text(&o));
    assert!(
        file().contains("theme = \"ptyro-light\"")
            && file().contains("# I use my terminal's selection"),
        "{}",
        file()
    );
    // get: the bare value, for scripts; --explain says why.
    assert_eq!(
        String::from_utf8_lossy(&ptyro(&["config", "get", "ui.theme"]).stdout),
        "ptyro-light\n"
    );
    let o = ptyro(&["config", "get", "ui.theme", "--explain"]);
    assert!(
        text(&o).contains("default: ptyro-dark") && text(&o).contains("your config line"),
        "{}",
        text(&o)
    );
    assert_eq!(String::from_utf8_lossy(&ptyro(&["config", "get", "ui.mouse"]).stdout), "false\n");
    assert_eq!(
        String::from_utf8_lossy(&ptyro(&["config", "get", "ui.clock"]).stdout),
        "true\n",
        "the default, when nothing sets it"
    );
    // A refusal says why and writes nothing.
    let before = file();
    for (args, why) in [
        (vec!["config", "set", "ui.sidebar_width", "3"], "outside 12 to 120"),
        (vec!["config", "set", "ui.on_stop", "never"], "isn't one of next, stay"),
        (vec!["config", "set", "ui.thme", "x"], "Did you mean ui.theme"),
        (vec!["config", "set", "ui.theme", "nonesuch"], "unknown theme"),
        (vec!["config", "set", "scripts.setup", "x"], "--project"),
    ] {
        let o = ptyro(&args);
        assert!(!o.status.success() && text(&o).contains(why), "{args:?}: {}", text(&o));
    }
    assert_eq!(file(), before);
    // A restart-only setting says so.
    let o = ptyro(&["config", "set", "notifications.callback.listen", "127.0.0.1:7591"]);
    assert!(
        text(&o).contains("needs a daemon restart") && text(&o).contains("ptyro daemon restart"),
        "{}",
        text(&o)
    );
    // A list and a command from several words.
    assert!(ptyro(&["config", "set", "discover.roots", "~/a", "~/b"]).status.success());
    assert!(file().contains("roots = [\"~/a\", \"~/b\"]"), "{}", file());
    assert!(
        ptyro(&["config", "set", "summaries.command", "--", "claude", "-p", "--model", "haiku"])
            .status
            .success()
    );
    // list --changed and --json.
    let o = ptyro(&["config", "list", "--changed"]);
    let shown = text(&o);
    assert!(
        shown.contains("ui.theme")
            && shown.contains("← your config")
            && !shown.contains("ui.sidebar "),
        "{shown}"
    );
    let o = ptyro(&["config", "list", "daemon", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["settings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["key"] == "daemon.scrollback_lines" && s["origin"] == "user"),
        "{v}"
    );
    // unset puts it back.
    assert!(ptyro(&["config", "unset", "ui.theme"]).status.success());
    assert!(!file().contains("theme"), "{}", file());
    // Every key is listed, the defaults too; one is unbound with `none`, and unset puts it back.
    let o = ptyro(&["config", "list", "keys"]);
    let shown = text(&o);
    assert!(
        shown.contains("keys after the prefix")
            && shown.contains("keys in the sidebar")
            && shown.lines().any(|l| l.starts_with("  d ")
                && l.contains("app.detach")
                && l.contains("(default)")),
        "{shown}"
    );
    assert_eq!(
        String::from_utf8_lossy(&ptyro(&["config", "get", "keys.d"]).stdout),
        "app.detach\n"
    );
    let o = ptyro(&["config", "set", "keys.d", "none"]);
    assert!(o.status.success() && text(&o).contains("unbound"), "{}", text(&o));
    assert!(file().contains("d = \"none\"") && file().contains("# my settings"), "{}", file());
    assert_eq!(String::from_utf8_lossy(&ptyro(&["config", "get", "keys.d"]).stdout), "none\n");
    let o = ptyro(&["config", "list", "keys", "--changed"]);
    let shown = text(&o);
    assert!(
        shown.contains("← unbound (default: app.detach)") && !shown.contains("(default)\n"),
        "{shown}"
    );
    let o = ptyro(&["config", "set", "keys.q", "palette"]);
    assert!(text(&o).contains("q ran app.detach"), "names what the key ran: {}", text(&o));
    assert!(ptyro(&["config", "unset", "keys.d", "keys.q"]).status.success());
    assert!(!file().contains("[keys]") || !file().contains("none"), "{}", file());
    assert!(ptyro_core::config::Config::parse(&file()).unwrap().keys.is_empty(), "{}", file());
    // An unknown action is refused, with nothing written.
    let before = file();
    let o = ptyro(&["config", "set", "keys.d", "no.such.action"]);
    assert!(!o.status.success() && file() == before, "{}", text(&o));
    // An agent, a binding and a service are added whole, and removed.
    assert!(
        ptyro(&["config", "add", "agents", "aider", "command=aider --model sonnet"])
            .status
            .success()
    );
    assert!(ptyro(&["config", "add", "keys", "x", "value=session.next"]).status.success());
    assert!(
        ptyro(&["config", "add", "notifications.sinks", "type=ntfy", "url=https://ntfy.sh/me"])
            .status
            .success()
    );
    let o = ptyro(&["config", "add", "notifications.sinks", "type=slack"]);
    assert!(text(&o).contains("needs its webhook"), "{}", text(&o));
    let o = ptyro(&["config", "add", "agents", "bad", "dangerous=maybe"]);
    assert!(!o.status.success(), "{}", text(&o));
    let c = ptyro_core::config::Config::parse(&file()).unwrap();
    assert!(
        c.agents.contains_key("aider")
            && c.keys["x"] == "session.next"
            && c.notifications.sinks.len() == 1
    );
    assert!(ptyro(&["config", "rename", "agents", "aider", "pair"]).status.success());
    assert!(ptyro(&["config", "rm", "agents", "pair"]).status.success());
    assert!(ptyro(&["config", "rm", "notifications.sinks", "0"]).status.success());
    assert!(ptyro(&["config", "unset", "keys.x"]).status.success());
    let c = ptyro_core::config::Config::parse(&file()).unwrap();
    assert!(
        c.agents.is_empty() && c.keys.is_empty() && c.notifications.sinks.is_empty(),
        "{}",
        file()
    );
    assert!(file().starts_with("# my settings\n"), "comments survived all of it: {}", file());

    // The repository's settings: --project goes to .ptyro.local.toml; --shared to .ptyro.toml.
    assert!(ptyro(&["project", "add", "."]).status.success());
    let o = ptyro(&["config", "set", "scripts.setup", "make deps", "--project"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("trust"), "scripts changing asks for trust again: {}", text(&o));
    let local = std::fs::read_to_string(repo.join(".ptyro.local.toml")).unwrap();
    assert!(local.contains("setup = \"make deps\""), "{local}");
    assert!(
        std::fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .contains(".ptyro.local.toml")
    );
    let o = ptyro(&["config", "set", "project.base", "origin/main", "--project", "--shared"]);
    assert!(o.status.success() && text(&o).contains("committed"), "{}", text(&o));
    assert!(
        std::fs::read_to_string(repo.join(".ptyro.toml"))
            .unwrap()
            .contains("base = \"origin/main\"")
    );
    assert_eq!(
        String::from_utf8_lossy(&ptyro(&["config", "get", "scripts.setup", "--project"]).stdout),
        "make deps\n"
    );
    let o = ptyro(&["config", "list", "scripts", "--project", "--changed"]);
    assert!(text(&o).contains("← .ptyro.local.toml"), "{}", text(&o));
    // Agents can't be set in a repository.
    let o = ptyro(&["config", "add", "agents", "x", "--project"]);
    assert!(!o.status.success(), "{}", text(&o));
    // The schema, for tools.
    let o = ptyro(&["config", "schema"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["settings"].as_array().unwrap().len() > 100
            && v["sections"].as_array().unwrap().len() >= 14
    );
}
