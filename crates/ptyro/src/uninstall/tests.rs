use super::*;

fn env(home: &str, ptyro_home: Option<&str>) -> Env {
    Env {
        home: Some(PathBuf::from(home)),
        xdg_config_home: None,
        ptyro_home: ptyro_home.map(PathBuf::from),
        exe: PathBuf::from("/x/ptyro"),
        in_session: false,
    }
}

fn class(exe: &str) -> BinaryAction {
    classify_binary(Path::new(exe), &env("/home/me", None), false)
}

#[test]
fn the_binary_is_removed_only_where_it_was_put_by_hand() {
    assert_eq!(class("/home/me/.local/bin/ptyro"), BinaryAction::Remove);
    assert_eq!(class("/usr/local/bin/ptyro"), BinaryAction::Remove);
    assert_eq!(class("/opt/tools/ptyro"), BinaryAction::Remove);
    assert_eq!(class("/home/me/.cargo/bin/ptyro"), BinaryAction::ViaCargo);
    assert_eq!(
        class("/opt/homebrew/Cellar/ptyro/0.0.4/bin/ptyro"),
        BinaryAction::Manual { command: "brew uninstall ptyro".into() }
    );
    assert_eq!(
        class("/usr/local/Cellar/ptyro/0.0.4/bin/ptyro"),
        BinaryAction::Manual { command: "brew uninstall ptyro".into() }
    );
    assert_eq!(
        class("/home/linuxbrew/.linuxbrew/bin/ptyro"),
        BinaryAction::Manual { command: "brew uninstall ptyro".into() }
    );
    assert_eq!(
        class("/home/me/.cargo/registry/src/x/ptyro/ptyro"),
        BinaryAction::Manual { command: "cargo uninstall ptyro".into() }
    );
    assert!(matches!(class("/usr/bin/ptyro"), BinaryAction::Manual { .. }), "a package's file");
    // A build is never removed.
    assert!(matches!(class("/src/ptyro/target/debug/ptyro"), BinaryAction::Keep { .. }));
    assert!(matches!(class("/src/ptyro/target/release/ptyro"), BinaryAction::Keep { .. }));
    // Nor when told not to, nor in a sandbox.
    assert!(matches!(
        classify_binary(Path::new("/home/me/.local/bin/ptyro"), &env("/home/me", None), true),
        BinaryAction::Keep { .. }
    ));
    assert!(matches!(
        classify_binary(
            Path::new("/home/me/.local/bin/ptyro"),
            &env("/home/me", Some("/tmp/h")),
            false
        ),
        BinaryAction::Keep { .. }
    ));
}

#[test]
fn only_the_lines_ptyro_wrote_leave_an_rc_file() {
    let rc = "export PATH=$HOME/bin:$PATH\n\
              \n\
              # ptyro completions (live session and worktree names)\n\
              source <(ptyro completions zsh)\n\
              alias ll='ls -l'\n\
              eval \"$(ptyro shell-init zsh)\"\n\
              if command -v ptyro >/dev/null; then eval \"$(ptyro shell-init zsh)\"; fi\n\
              # my note about ptyro\n\
              ptyro shell-init fish | source\n";
    let (out, gone) = strip_rc(rc);
    assert_eq!(
        out,
        "export PATH=$HOME/bin:$PATH\n\
         alias ll='ls -l'\n\
         if command -v ptyro >/dev/null; then eval \"$(ptyro shell-init zsh)\"; fi\n\
         # my note about ptyro\n"
    );
    assert!(gone.iter().any(|l| l.contains("ptyro completions zsh")));
    assert!(gone.iter().any(|l| l.starts_with("# ptyro completions")));
    assert_eq!(
        gone.iter().filter(|l| l.trim().is_empty()).count(),
        1,
        "the blank line before the comment"
    );
    // What it left but mentions ptyro's commands is pointed out.
    assert_eq!(
        rc_mentions(rc),
        vec![
            r#"if command -v ptyro >/dev/null; then eval "$(ptyro shell-init zsh)"; fi"#.to_owned()
        ]
    );
    // A file ptyro never touched is unchanged, byte for byte (no trailing newline too).
    for text in ["", "plain\n", "no newline at the end", "a\n\n\nb\n"] {
        assert_eq!(strip_rc(text), (text.to_owned(), Vec::new()), "{text:?}");
    }
    // A line at the very top, and the last line without its newline.
    assert_eq!(strip_rc("eval \"$(ptyro shell-init bash)\"").0, "");
    assert_eq!(strip_rc("a\nsource <(ptyro completions bash)").0, "a");
}

#[test]
fn a_plan_never_guesses_where_ptyro_lives() {
    let paths = Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("/h"))).unwrap();
    let d = dirs_of(&paths, &Opts::default());
    assert_eq!(
        d.iter().map(|d| (d.name, d.path.display().to_string())).collect::<Vec<_>>(),
        vec![
            ("config", "/h/config".to_owned()),
            ("data", "/h/data".to_owned()),
            ("state", "/h/state".to_owned()),
            ("runtime", "/h/run".to_owned()),
        ]
    );
    let keep = dirs_of(&paths, &Opts { keep_config: true, keep_data: true, ..Opts::default() });
    assert_eq!(
        keep.iter().filter(|d| d.keep).map(|d| d.name).collect::<Vec<_>>(),
        ["config", "data"]
    );
    // State that is the data directory is listed once.
    let same = Paths { state: paths.data.clone(), ..paths.clone() };
    assert_eq!(dirs_of(&same, &Opts::default()).iter().filter(|d| d.path == paths.data).count(), 1);
    // A socket pushed into /tmp by a long path is ptyro's directory too.
    let long =
        Paths::resolve(|k| (k == "PTYRO_HOME").then(|| PathBuf::from("/x".repeat(60)))).unwrap();
    assert!(dirs_of(&long, &Opts::default()).iter().any(|d| d.name == "socket"));
}

#[test]
fn sizes_read_as_people_do_and_links_are_not_followed() {
    assert_eq!(human(0), "0 B");
    assert_eq!(human(1023), "1023 B");
    assert_eq!(human(1536), "1.5 KiB");
    assert_eq!(human(5 << 20), "5.0 MiB");
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a"), vec![0u8; 100]).unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("sub/b"), vec![0u8; 50]).unwrap();
    std::os::unix::fs::symlink("/", d.path().join("link")).unwrap();
    assert_eq!(dir_size(d.path()), 150, "a link to / is not walked");
    assert_eq!(dir_size(&d.path().join("missing")), 0);
}

fn plan_with(worktrees: Vec<Worktree>) -> Plan {
    Plan {
        sandbox: None,
        daemon_running: true,
        sessions_live: Some(2),
        dirs: vec![Dir {
            name: "data",
            path: "/home/me/.local/share/ptyro".into(),
            bytes: 3 << 20,
            keep: false,
        }],
        logins: true,
        chats: Some(4096),
        uncommitted_total: worktrees.iter().filter(|w| w.uncommitted > 0).count(),
        worktrees,
        service: None,
        integrations: vec![Integration { agent: "gemini".into(), profile: None }],
        rc: Vec::new(),
        tailscale: None,
        refs: vec![Refs { repo: "/r".into(), archive: 2, taken: 1, other: 0 }],
        purge_refs: false,
        binary: Binary { path: "/home/me/.local/bin/ptyro".into(), action: BinaryAction::Remove },
        kept: vec!["your repositories".into(), "3 snapshot ref(s) under refs/ptyro/".into()],
        command: "ptyro uninstall".into(),
    }
}

#[test]
fn the_plan_says_what_goes_what_is_lost_and_what_stays() {
    let w = Worktree {
        path: "/home/me/.local/share/ptyro/worktrees/app/fix".into(),
        repo: Some("/home/me/app".into()),
        branch: Some("ptyro/fix".into()),
        uncommitted: 3,
        unpushed: 1,
        locked: true,
    };
    let p = plan_with(vec![w]);
    let text = render(&p, &env("/home/me", None));
    for want in [
        "2 running",
        "~/.local/share/ptyro (3.0 MiB)",
        "agent logins",
        "ptyro/fix",
        "3 uncommitted change(s) will be lost",
        "1 commit(s) not pushed",
        "(locked)",
        "gemini",
        "~/.local/bin/ptyro",
        "Kept:",
        "3 snapshot ref(s)",
    ] {
        assert!(text.contains(want), "{want:?} in\n{text}");
    }
    // The same, machine-readably, with the lost work counted.
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(j["uncommitted_total"], 1);
    assert_eq!(j["binary"]["action"], "remove");
    assert_eq!(j["worktrees"][0]["uncommitted"], 3);
    // Not run on a Homebrew install: the command that does it.
    let mut brew = plan_with(Vec::new());
    brew.binary.action = BinaryAction::Manual { command: "brew uninstall ptyro".into() };
    assert!(render(&brew, &env("/home/me", None)).contains("`brew uninstall ptyro`"));
}

#[test]
fn rc_files_follow_xdg_and_home() {
    let mut e = env("/home/me", None);
    let files = rc_files(&e);
    assert!(files.contains(&PathBuf::from("/home/me/.zshrc")));
    assert!(files.contains(&PathBuf::from("/home/me/.config/fish/config.fish")));
    e.xdg_config_home = Some("/cfg".into());
    assert!(rc_files(&e).contains(&PathBuf::from("/cfg/fish/config.fish")));
    e.home = None;
    assert!(rc_files(&e).is_empty());
}

#[test]
fn integrations_are_found_where_ptyro_put_them() {
    let home = tempfile::tempdir().unwrap();
    let e = Env { home: Some(home.path().to_path_buf()), ..env("/x", None) };
    let cfg = Config::default();
    assert!(integrations_found(&cfg, &e).is_empty());
    let g = home.path().join(".gemini");
    std::fs::create_dir_all(g.join("extensions/ptyro")).unwrap();
    std::fs::create_dir_all(g.join("config")).unwrap();
    std::fs::write(
        g.join("config/hooks.json"),
        r#"{"hooks":{"x":[{"command":"ptyro hook antigravity"}]}}"#,
    )
    .unwrap();
    let found: Vec<String> = integrations_found(&cfg, &e).into_iter().map(|i| i.agent).collect();
    assert_eq!(found, ["gemini", "antigravity"]);
    // Someone else's hooks file isn't ours.
    std::fs::write(g.join("config/hooks.json"), r#"{"hooks":{}}"#).unwrap();
    std::fs::remove_dir_all(g.join("extensions")).unwrap();
    assert!(integrations_found(&cfg, &e).is_empty());
}
