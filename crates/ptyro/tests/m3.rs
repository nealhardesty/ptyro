//! M3 end-to-end tests: projects, worktrees (includes, ports, setup, trust, locks, the removal
//! safety matrix), workspaces, profiles (pins, guards, environments, the profile CLI), scripts,
//! reconciliation and gc. Real daemon, real git, a fake `$HOME` so nothing of the developer's
//! leaks in.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::ErrorCode;
use ptyro_core::TermSize;
use ptyro_core::model::{RemoveOpts, WorktreeInfo, WorktreeSpec, WorktreeState};
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;
use ptyro_testkit::headless::{Report, Script};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(15);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-home-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(h.join(".gitconfig"), "[user]\n\tname = Test User\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n").unwrap();
        // A fake `claude` on PATH: prints where and as whom it runs.
        std::fs::create_dir_all(h.join("bin")).unwrap();
        let fake = h.join("bin/claude");
        std::fs::write(&fake, "#!/bin/sh\necho \"fake-claude wt=$PTYRO_WORKTREE_NAME profile=$PTYRO_PROFILE\"\nexec sleep 60\n").unwrap();
        std::os::unix::fs::PermissionsExt::set_mode(
            &mut std::fs::metadata(&fake).unwrap().permissions(),
            0o755,
        );
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let path = format!("{}/bin:{}", hs, std::env::var("PATH").unwrap_or_default());
        let env = [
            ("HOME", hs.as_str()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("SHELL", "/bin/sh"),
            ("PATH", path.as_str()),
        ];
        let d = TempDaemon::start_with(BIN, &env, (!config.is_empty()).then_some(config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A repo cloned from a bare "remote", with one pushed commit.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        for (f, text) in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        std::fs::write(root.join("README"), "hi\n").unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.git(&root, &["remote", "set-head", "origin", "main"]);
        root
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    /// The TUI, inside the headless emulator.
    fn tui(&self, args: &[&str]) -> Script {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        let mut s = Script::new(argv)
            .size(110, 22)
            .timeout(8.0)
            .env("HOME", &self.home().to_string_lossy());
        for (k, v) in self.d.env() {
            s = s.env(&k, &v);
        }
        s
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }
}

async fn spawn(c: &Client, spec: SpawnSpec) -> Result<SessionInfo, ptyro_core::Error> {
    match c.request(Request::Spawn { spec }).await? {
        Reply::Session { info } => Ok(info),
        other => panic!("{other:?}"),
    }
}

fn sh(script: &str, worktree: &str) -> SpawnSpec {
    SpawnSpec {
        argv: vec!["sh".into(), "-c".into(), script.into()],
        cwd: "/".into(),
        worktree: Some(worktree.into()),
        size: Some(TermSize::new(200, 30)),
        ..SpawnSpec::default()
    }
}

async fn screen_with(d: &TempDaemon, c: &Client, addr: &str, needle: &str) -> String {
    d.wait_for_text(c, addr, needle, WAIT).await.unwrap()
}

async fn wait_exit(c: &Client, addr: &str) -> SessionInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(Reply::Session { info }) = c.request(Request::Get { addr: addr.into() }).await
            && !info.state.is_live()
        {
            return info;
        }
        assert!(Instant::now() < deadline, "{addr} did not exit");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn worktree(c: &Client, addr: &str) -> WorktreeInfo {
    match c.request(Request::WorktreeGet { addr: addr.into() }).await.unwrap() {
        Reply::Worktree { info } => info,
        other => panic!("{other:?}"),
    }
}

async fn wait_worktree(
    c: &Client,
    addr: &str,
    pred: impl Fn(&WorktreeInfo) -> bool,
) -> WorktreeInfo {
    let deadline = Instant::now() + WAIT;
    loop {
        let w = worktree(c, addr).await;
        if pred(&w) {
            return w;
        }
        assert!(Instant::now() < deadline, "worktree {addr} never got there: {w:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn new_wt(c: &Client, spec: WorktreeSpec) -> Result<Reply, ptyro_core::Error> {
    c.request(Request::WorktreeNew { spec }).await
}

const SETUP_TOML: &str = r#"
[worktree]
copy = ["config/local.yml"]
share = ["node_modules"]

[scripts]
setup = "echo setup-ran port=$PTYRO_PORT name=$PTYRO_WORKTREE_NAME conductor=$CONDUCTOR_WORKSPACE_NAME > setup.out"
archive = "echo archived > $PTYRO_ROOT_PATH/archived.txt"

[scripts.run.dev]
command = "echo dev on $PTYRO_PORT; sleep 30"
default = true
"#;

#[tokio::test]
async fn the_worktree_lifecycle() {
    let w =
        World::new("[ports]\nrange = \"41000-41099\"\ncount = 5\n[compat]\nconductor_env = true\n");
    let root = w.repo(
        "app",
        &[
            (".gitignore", ".env\nconfig/local.yml\nsetup.out\nnode_modules/\n"),
            (".worktreeinclude", ".env\nREADME\n"),
            (".ptyro.toml", SETUP_TOML),
        ],
    );
    std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
    std::fs::write(root.join("node_modules/dep/index.js"), "module.exports = 1\n").unwrap();
    std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/local.yml"), "local: true\n").unwrap();

    let out = w.ok(&["project", "add", root.to_str().unwrap()]);
    assert!(out.contains("app"), "{out}");
    assert!(w.ok(&["project", "ls"]).contains("app"));
    assert!(w.ok(&["project", "show", "app"]).contains("NOT trusted"));

    // Untrusted scripts never run; the worktree is still made.
    let c = w.d.client().await.unwrap();
    let r = new_wt(
        &c,
        WorktreeSpec {
            project: "app".into(),
            name: Some("first".into()),
            setup: true,
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap();
    let Reply::WorktreeCreated { info, setup, untrusted, notes, .. } = r else { panic!() };
    assert!(setup.is_none() && untrusted.is_some(), "{notes:?}");
    assert_eq!(info.branch.as_deref(), Some("first"));
    assert!(info.path.join(".env").exists(), ".worktreeinclude: matched and ignored");
    assert!(!info.path.join("README").is_symlink());
    assert!(info.path.join("config/local.yml").exists(), "worktree.copy");
    // Shared: a copy-on-write clone where the file system can, else a symlink.
    assert_eq!(
        std::fs::read_to_string(info.path.join("node_modules/dep/index.js")).unwrap(),
        "module.exports = 1\n"
    );
    let ports = info.ports.expect("a port block");
    assert_eq!((ports.base, ports.count), (41000, 5));
    assert!(!info.path.join("setup.out").exists());

    w.ok(&["project", "trust", "app", "--yes"]);
    assert!(w.ok(&["project", "show", "app"]).contains("(trusted)"));
    let r = new_wt(
        &c,
        WorktreeSpec {
            project: "app".into(),
            name: Some("second".into()),
            setup: true,
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap();
    let Reply::WorktreeCreated { info: second, setup: Some(setup), .. } = r else {
        panic!("setup should run now")
    };
    assert_eq!(setup.script.as_deref(), Some("setup"));
    assert_eq!(wait_exit(&c, &setup.name).await.state.label(), "exited 0");
    let out = std::fs::read_to_string(second.path.join("setup.out")).unwrap();
    assert_eq!(
        out.trim(),
        "setup-ran port=41005 name=second conductor=second",
        "the second block, SPEC §10.6 env"
    );

    // Changing the scripts takes the trust away until approved again.
    std::fs::write(root.join(".ptyro.local.toml"), "[scripts]\nsetup = \"echo changed\"\n")
        .unwrap();
    assert!(w.ok(&["project", "show", "app"]).contains("NOT trusted"));
    std::fs::remove_file(root.join(".ptyro.local.toml")).unwrap();
    assert!(w.ok(&["project", "show", "app"]).contains("(trusted)"));

    // Sessions in the worktree get its environment, and lock it while they run.
    let s = spawn(
        &c,
        sh(
            "echo wt=$PTYRO_WORKTREE_NAME branch=$PTYRO_BRANCH project=$PTYRO_PROJECT; sleep 30",
            "app/second",
        ),
    )
    .await
    .unwrap();
    assert_eq!(s.worktree.as_deref(), Some(second.id.as_str()));
    screen_with(&w.d, &c, &s.name, "wt=second branch=second project=app").await;
    wait_worktree(&c, "app/second", |w| w.locked).await;
    assert!(
        w.git(&root, &["worktree", "list", "--porcelain"])
            .contains(&format!("locked ptyro:{}", second.id))
    );
    // `ptyro new` from inside a worktree directory lands in it.
    let inferred = spawn(
        &c,
        SpawnSpec {
            argv: vec!["true".into()],
            cwd: second.path.join("config"),
            ..SpawnSpec::default()
        },
    )
    .await;
    let inferred = inferred.unwrap_or_else(|_| panic!("spawn"));
    assert_eq!(inferred.worktree.as_deref(), Some(second.id.as_str()));

    // Removal: refused while sessions run …
    let e = c
        .request(Request::WorktreeRemove { addr: "app/second".into(), opts: RemoveOpts::default() })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("running"), "{}", e.message);
    c.request(Request::Kill { addr: s.name.clone(), signal: None }).await.unwrap();
    wait_worktree(&c, "app/second", |w| !w.locked).await;
    // … and with work that would be lost, with a precise report (CLI exit code 5).
    std::fs::write(second.path.join("README"), "edited\n").unwrap();
    std::fs::write(second.path.join("notes.md"), "mine\n").unwrap();
    let out = w.cli(&["wt", "rm", "app/second"]);
    assert_eq!(out.status.code(), Some(5));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("1 uncommitted change")
            && err.contains("1 untracked file")
            && err.contains("notes.md"),
        "{err}"
    );
    assert!(second.path.exists());
    // Committed but not pushed: still refused, and the branch would be kept.
    w.git(&second.path, &["add", "-A"]);
    w.git(&second.path, &["commit", "-q", "-m", "work"]);
    let e = c
        .request(Request::WorktreeRemove { addr: "app/second".into(), opts: RemoveOpts::default() })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorktree);
    assert!(e.message.contains("1 commit(s) not pushed"), "{}", e.message);
    // --force removes it but keeps the unmerged branch.
    let Reply::Removed { notes } = c
        .request(Request::WorktreeRemove {
            addr: "app/second".into(),
            opts: RemoveOpts { force: true, ..RemoveOpts::default() },
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let notes = notes.join("\n");
    assert!(notes.contains("kept branch second"), "{notes}");
    assert!(notes.contains("archive script exited 0"), "{notes}");
    assert!(root.join("archived.txt").exists(), "archive ran before removal");
    assert!(!second.path.exists());
    assert!(w.git(&root, &["branch"]).contains("second"));

    // A clean worktree whose branch is merged: removed together with its branch.
    w.git(&info.path, &["commit", "-q", "--allow-empty", "-m", "feature"]);
    w.git(&root, &["merge", "-q", "--ff-only", "first"]);
    w.git(&root, &["push", "-q", "origin", "main"]);
    let out = w.ok(&["wt", "rm", "app/first"]);
    assert!(out.contains("deleted branch first"), "{out}");
    assert!(out.contains("released ports 41000-41004"), "{out}");
    assert!(!w.git(&root, &["branch"]).contains("first"));
    // The port block is free again.
    let r = new_wt(
        &c,
        WorktreeSpec {
            project: "app".into(),
            name: Some("third".into()),
            setup: false,
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap();
    let Reply::WorktreeCreated { info: third, .. } = r else { panic!() };
    assert_eq!(third.ports.unwrap().base, 41000);
    // The main checkout can't be removed as a worktree.
    let e = c
        .request(Request::WorktreeRemove { addr: "app/@root".into(), opts: RemoveOpts::default() })
        .await
        .unwrap_err();
    assert!(e.message.contains("main checkout"), "{}", e.message);
}

#[tokio::test]
async fn scripts_status_and_run_modes() {
    let toml = "[scripts]\nrun_mode = \"exclusive\"\n[scripts.run.dev]\ncommand = \"echo dev-$PTYRO_WORKTREE_NAME; sleep 30\"\ndefault = true\n[scripts.run.once]\ncommand = \"echo once\"\n";
    let w = World::new("[worktrees]\nfetch = false\n");
    let root = w.repo("svc", &[(".ptyro.toml", toml)]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    let c = w.d.client().await.unwrap();
    let e = c
        .request(Request::WorktreeRun { addr: "svc/@root".into(), script: None, size: None })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UntrustedProject);
    w.ok(&["project", "trust", "svc", "--yes"]);
    for n in ["a", "b"] {
        new_wt(
            &c,
            WorktreeSpec { project: "svc".into(), name: Some(n.into()), ..WorktreeSpec::default() },
        )
        .await
        .unwrap();
    }
    let Reply::Session { info: a } = c
        .request(Request::WorktreeRun { addr: "svc/a".into(), script: None, size: None })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!((a.name.as_str(), a.script.as_deref()), ("a.dev", Some("run:dev")));
    screen_with(&w.d, &c, "a.dev", "dev-a").await;
    // Script sessions are read-only.
    c.send(ptyro_protocol::ClientMsg::Input {
        session: a.id,
        event: ptyro_core::input::InputEvent::Raw { bytes: b"typed\r".to_vec() },
    })
    .unwrap();
    // exclusive: starting b's run script stops a's.
    let Reply::Session { info: b } = c
        .request(Request::WorktreeRun {
            addr: "svc/b".into(),
            script: Some("dev".into()),
            size: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let a_after = wait_exit(&c, "a.dev").await;
    assert!(!a_after.state.is_live());
    assert!(!w.d.wait_for_text(&c, "a.dev", "typed", Duration::from_millis(200)).await.is_ok());
    screen_with(&w.d, &c, &b.name, "dev-b").await;
    // A named script, via the CLI.
    assert!(w.ok(&["wt", "run", "svc/a", "once"]).contains("a.once"));

    // Repository status follows file changes.
    let wt = worktree(&c, "svc/a").await;
    std::fs::write(wt.path.join("new.txt"), "x").unwrap();
    std::fs::write(wt.path.join("README"), "one\ntwo\nthree\n").unwrap();
    let wt = wait_worktree(&c, "svc/a", |w| {
        w.status.as_ref().is_some_and(|s| s.untracked == 1 && s.modified == 1)
    })
    .await;
    assert_eq!(wt.status.unwrap().insertions, 3);
    let out = w.ok(&["wt", "status", "svc/a"]);
    assert!(out.contains("1 modified, 1 untracked"), "{out}");
    // Rename the branch (not pushed, so allowed), from the first commit's subject.
    w.git(&wt.path, &["add", "-A"]);
    w.git(&wt.path, &["commit", "-q", "-m", "Add the new parser"]);
    let out = w.ok(&["wt", "rename-branch", "svc/a"]);
    assert!(out.contains("add-the-new-parser"), "{out}");
    w.git(&wt.path, &["push", "-q", "-u", "origin", "add-the-new-parser"]);
    let out = w.cli(&["wt", "rename-branch", "svc/a", "other"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("pushed"));
}

#[tokio::test]
async fn listening_ports_are_reported_per_session() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: no python3");
        return;
    }
    let w = World::new("[ports]\nrange = \"42000-42099\"\n");
    let root = w.repo("web", &[]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    let c = w.d.client().await.unwrap();
    new_wt(
        &c,
        WorktreeSpec {
            project: "web".into(),
            name: Some("x".into()),
            fetch: Some(false),
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap();
    let srv = "import os,socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', int(os.environ['PTYRO_PORT']))); s.listen(); print('listening', flush=True); time.sleep(30)";
    let s = spawn(
        &c,
        SpawnSpec { argv: vec!["python3".into(), "-c".into(), srv.into()], ..sh("", "web/x") },
    )
    .await
    .unwrap();
    screen_with(&w.d, &c, &s.name, "listening").await;
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Session { info } =
            c.request(Request::Get { addr: s.name.clone() }).await.unwrap()
        else {
            panic!()
        };
        if info.ports == vec![42000] {
            break;
        }
        assert!(Instant::now() < deadline, "ports never showed: {:?}", info.ports);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

const PROFILES: &str = r#"
[worktrees]
fetch = false

[profiles.work]
label = "work"
color = "blue"
claude = "~/.claude-work"
git = { name = "Work Me", email = "me@acme.example" }
match = { remotes = ["github.com:acme/*"] }
secrets = { WORK_TOKEN = { command = ["sh", "-c", "echo tok-from-command"] } }

[profiles.personal]
label = "me"
claude = "~/.claude"
git = { email = "me@home.example", ssh_key = "~/.ssh/id_me" }
match = { remotes = ["github.com/me/*"] }
guard = "refuse"
"#;

const ENV_DUMP: &str = "echo cfg=$CLAUDE_CONFIG_DIR email=$GIT_AUTHOR_EMAIL profile=$PTYRO_PROFILE tok=$WORK_TOKEN key=${ANTHROPIC_API_KEY:-none}; sleep 30";

#[tokio::test]
async fn profiles_pins_guards_and_environments() {
    let w = World::new(PROFILES);
    let h = w.home();
    let root = w.repo("acme-app", &[]);
    w.git(&root, &["remote", "set-url", "origin", "git@github.com:acme/app.git"]);
    let out = w.ok(&["project", "add", root.to_str().unwrap()]);
    assert!(out.contains("pinned to profile work"), "{out}");
    let c = w.d.client().await.unwrap();

    // Pinned: sessions run as work: its Claude dir, git author, resolved secret.
    let s = spawn(&c, sh(ENV_DUMP, "acme-app/@root")).await.unwrap();
    assert_eq!(s.profile, "work");
    let screen = screen_with(&w.d, &c, &s.name, "profile=work").await;
    assert!(screen.contains(&format!("cfg={}/.claude-work", h.display())), "{screen}");
    assert!(screen.contains("email=me@acme.example profile=work tok=tok-from-command"), "{screen}");
    assert!(screen.contains("key=none"), "credentials are scrubbed: {screen}");

    // An explicit contradicting choice needs confirmation; with it, the other identity applies.
    let mut spec = sh(ENV_DUMP, "acme-app/@root");
    spec.agent = None;
    spec.profile = Some("personal".into());
    let e = spawn(&c, spec.clone()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(e.message.contains("pinned to profile work"), "{}", e.message);
    spec.override_pin = true;
    let p = spawn(&c, spec).await.unwrap();
    assert_eq!(p.profile, "personal");
    screen_with(&w.d, &c, &p.name, "email=me@home.example profile=personal").await;
    // From the CLI without a terminal: refused with a hint.
    let out =
        w.cli(&["new", "-d", "--profile", "personal", "--wt", "acme-app/@root", "--", "true"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert!(
        w.cli(&[
            "new",
            "-d",
            "--profile",
            "personal",
            "--override-pin",
            "--wt",
            "acme-app/@root",
            "--",
            "true"
        ])
        .status
        .success()
    );

    // Unpinned, the guard of the profile whose rules match asks before running as default.
    w.ok(&["project", "unpin", "acme-app"]);
    let e = spawn(&c, sh("true", "acme-app/@root")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(e.message.contains("matches profile work"), "{}", e.message);
    // personal's guard refuses outright.
    let mine = w.repo("dots", &[]);
    w.git(&mine, &["remote", "set-url", "origin", "https://github.com/me/dots"]);
    w.ok(&["project", "add", mine.to_str().unwrap()]);
    w.ok(&["project", "unpin", "dots"]);
    let mut spec = sh("true", "dots/@root");
    spec.confirmed = true;
    let e = spawn(&c, spec).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "{}", e.message);

    // A worktree fixes its profile; restart keeps it.
    w.ok(&["project", "pin", "acme-app", "work"]);
    let Reply::WorktreeCreated { info: wt, notes, .. } = new_wt(
        &c,
        WorktreeSpec {
            project: "acme-app".into(),
            name: Some("feat".into()),
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(wt.profile.as_deref(), Some("work"));
    assert!(notes.iter().any(|n| n.contains("pinned by project")), "{notes:?}");
    let s = spawn(&c, sh("echo email=$GIT_AUTHOR_EMAIL", "acme-app/feat")).await.unwrap();
    wait_exit(&c, &s.name).await;
    c.request(Request::Restart { addr: s.name.clone() }).await.unwrap();
    screen_with(&w.d, &c, &s.name, "email=me@acme.example").await;
    // Commits made in it carry the work author.
    let s = spawn(
        &c,
        sh("git commit -q --allow-empty -m from-session && echo committed", "acme-app/feat"),
    )
    .await
    .unwrap();
    screen_with(&w.d, &c, &s.name, "committed").await;
    assert_eq!(w.git(&wt.path, &["log", "-1", "--format=%ae"]).trim(), "me@acme.example");

    // A shell under personal pushes with personal's key.
    let mut spec = sh("echo ssh=$GIT_SSH_COMMAND", "acme-app/@root");
    spec.profile = Some("personal".into());
    spec.override_pin = true;
    let s = spawn(&c, spec).await.unwrap();
    screen_with(
        &w.d,
        &c,
        &s.name,
        &format!("ssh=ssh -i {}/.ssh/id_me -o IdentitiesOnly=yes", h.display()),
    )
    .await;

    // `profile env` never resolves or shows secrets.
    let out = w.ok(&["profile", "env", "work"]);
    assert!(
        out.contains("WORK_TOKEN=<secret:WORK_TOKEN>") && !out.contains("tok-from-command"),
        "{out}"
    );
    assert!(out.contains("ANTHROPIC_API_KEY=<removed>"), "{out}");
    // The DB records the launch with the secret redacted.
    let db = std::fs::read(w.d.paths.data.join("ptyro.db")).unwrap_or_default();
    let wal = std::fs::read(w.d.paths.data.join("ptyro.db-wal")).unwrap_or_default();
    let has = |hay: &[u8], needle: &str| hay.windows(needle.len()).any(|x| x == needle.as_bytes());
    assert!(
        !has(&db, "tok-from-command") && !has(&wal, "tok-from-command"),
        "secret leaked into the store"
    );
}

#[tokio::test]
async fn the_profile_cli_edits_the_config() {
    let w = World::new("# my config\n[ui]\ntheme = \"ptyro-dark\"\n");
    w.ok(&[
        "profile",
        "new",
        "work",
        "--adopt",
        "claude=~/.claude-work",
        "--git-email",
        "me@acme.example",
        "--match",
        "github.com:acme/*",
        "--color",
        "blue",
    ]);
    let text = std::fs::read_to_string(w.d.paths.config.join("config.toml")).unwrap();
    assert!(text.starts_with("# my config"), "comments kept: {text}");
    assert!(
        text.contains("[profiles.work]") && text.contains("claude = \"~/.claude-work\""),
        "{text}"
    );
    let out = w.ok(&["profile", "ls"]);
    assert!(out.contains("work") && out.contains("me@acme.example"), "{out}");
    let out = w.cli(&["profile", "new", "work"]);
    assert_eq!(out.status.code(), Some(5), "exists");
    let out = w.cli(&["profile", "new", "x", "--color", "teal-ish"]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("invalid"),
        "validated before saving"
    );

    // In use → rm refused; rename rewrites the references.
    w.ok(&["ws", "new", "job", "--profile", "work"]);
    let out = w.cli(&["profile", "rm", "work"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("workspace job"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The daemon has to see the new config before it can accept the new name.
    w.ok(&["profile", "rename", "work", "acme"]);
    let ws = w.ok(&["ws", "ls"]);
    assert!(ws.contains("acme"), "{ws}");
    assert!(
        std::fs::read_to_string(w.d.paths.config.join("config.toml"))
            .unwrap()
            .contains("[profiles.acme]")
    );

    // detect finds the owner's alias setup.
    std::fs::write(
        w.home().join(".zshrc"),
        "alias claude-side='CLAUDE_CONFIG_DIR=~/.claude-side claude'\n",
    )
    .unwrap();
    let out = w.ok(&["profile", "detect"]);
    assert!(out.contains("[profiles.side]") && out.contains("~/.claude-side"), "{out}");
    let out = w.ok(&["profile", "doctor"]);
    assert!(out.contains("acme"), "{out}");
}

#[tokio::test]
async fn workspaces_reconcile_and_gc() {
    let w = World::new("[worktrees]\nfetch = false\nstale_days = 0\n");
    let root = w.repo("lib", &[]);
    w.ok(&["ws", "new", "side"]);
    w.ok(&["project", "add", root.to_str().unwrap(), "--workspace", "side"]);
    let out = w.ok(&["ws", "ls"]);
    assert!(out.lines().any(|l| l.starts_with("side") && l.contains("lib")), "{out}");
    assert!(out.lines().any(|l| l.starts_with("main")), "main always exists: {out}");
    w.ok(&["ws", "add", "main", "lib"]);
    w.ok(&["ws", "rename", "side", "aside"]);
    assert!(w.cli(&["ws", "rm", "main"]).status.code() == Some(2));
    let c = w.d.client().await.unwrap();
    let Reply::WorktreeCreated { info: gone, .. } = new_wt(
        &c,
        WorktreeSpec {
            project: "lib".into(),
            name: Some("gone".into()),
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    new_wt(
        &c,
        WorktreeSpec {
            project: "lib".into(),
            name: Some("keep".into()),
            ..WorktreeSpec::default()
        },
    )
    .await
    .unwrap();
    // Deleted behind ptyro's back: marked missing (never pruned automatically).
    std::fs::remove_dir_all(&gone.path).unwrap();
    wait_worktree(&c, "lib/gone", |w| w.state == WorktreeState::Missing).await;
    let out = w.ok(&["gc", "--dry-run"]);
    assert!(out.contains("lib/gone") && out.contains("directory is gone"), "{out}");
    assert!(
        worktree(&c, "lib/gone").await.state == WorktreeState::Missing,
        "gc --dry-run removes nothing"
    );

    // After a daemon restart: projects, worktrees, workspaces and trust are all still there.
    drop(c);
    let mut d = w;
    d.d.restart().unwrap();
    let c = d.d.client().await.unwrap();
    assert_eq!(worktree(&c, "lib/keep").await.state, WorktreeState::Ready);
    assert_eq!(worktree(&c, "lib/gone").await.state, WorktreeState::Missing);
    let out = d.ok(&["ws", "ls"]);
    assert!(out.contains("aside"), "{out}");
    // Removing a missing worktree just forgets it.
    let out = d.ok(&["wt", "rm", "lib/gone"]);
    assert!(out.contains("removed lib/gone"), "{out}");
}

async fn run(s: Script) -> Report {
    ptyro_testkit::headless::run(s).await.expect("headless run")
}

#[tokio::test]
async fn tui_wizard_fast_path_trusts_sets_up_and_follows_the_agent() {
    let w = World::new(
        "[worktrees]\nfetch = false\n[profiles.work]\nlabel = \"wrk\"\ncolor = \"blue\"\nmatch = { paths = [\"/**/proj\"] }\n",
    );
    let root =
        w.repo("proj", &[(".ptyro.toml", "[scripts]\nsetup = \"echo setup-ran; sleep 1\"\n")]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.cli(&[
        "new",
        "-d",
        "--wt",
        "proj/@root",
        "-n",
        "home-shell",
        "--",
        "sh",
        "-c",
        "echo at-root; sleep 60",
    ]);
    // n ⏎ (UX1.9, nothing typed): the default agent, in a new worktree with a random name; the
    // trust dialog comes first; y.
    let r = run(w
        .tui(&["attach", "home-shell"])
        .snapshot(1.4)
        .send(1.5, "\\x11n")
        .snapshot(2.2)
        .send(2.4, "\\r")
        .snapshot(3.4)
        .send(3.6, "y")
        .snapshot(6.5)
        .send(7.0, "\\x11d"))
    .await;
    let start = r.at(1.4);
    assert!(start.contains("▾ proj [wrk]"), "the tree, with the pin's label: {start}");
    assert!(start.contains("@root (main)") && start.contains("home-shell"), "{start}");
    let task = r.at(2.2);
    assert!(task.contains("What should it do? · profile wrk — pinned by project"), "{task}");
    assert!(task.contains("→ claude · new worktree in proj · a random name"), "{task}");
    let trust = r.at(3.4);
    assert!(
        trust.contains("Trust scripts?") && trust.contains("echo setup-ran; sleep 1"),
        "{trust}"
    );
    let done = r.at(6.5);
    assert!(
        done.contains("fake-claude wt="),
        "the pane follows the agent once setup is done: {done}"
    );
    assert!(done.contains("profile=work"), "{done}");
    // The branch is named like the worktree (no `ptyro/` since v0.0.13).
    let wt = done
        .split("fake-claude wt=")
        .nth(1)
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or_default()
        .to_owned();
    assert!(
        !wt.is_empty() && done.contains("[wrk]") && done.contains(&format!("· {wt}")),
        "identity and branch in the header: {done}"
    );
    // On the daemon side: setup ran (exited 0) before the agent started.
    let c = w.d.client().await.unwrap();
    let Reply::Sessions { sessions } = c.request(Request::List).await.unwrap() else { panic!() };
    let setup =
        sessions.iter().find(|s| s.script.as_deref() == Some("setup")).expect("setup session");
    let agent =
        sessions.iter().find(|s| s.agent.as_deref() == Some("claude")).expect("agent session");
    assert_eq!(setup.state.label(), "exited 0");
    assert!(setup.ended_ms.unwrap() <= agent.created_ms, "agent after setup");
    assert!(agent.name.starts_with("claude-"), "an agent and a corvid (UX1.24): {}", agent.name);
}

#[tokio::test]
async fn tui_workspaces_tree_folding_and_worktree_removal() {
    let w = World::new("[worktrees]\nfetch = false\n");
    let root = w.repo("lib", &[]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["wt", "new", "dirty", "--project", "lib"]);
    w.cli(&[
        "new",
        "-d",
        "--wt",
        "lib/dirty",
        "-n",
        "worker",
        "--",
        "sh",
        "-c",
        "echo working; sleep 60",
    ]);
    w.ok(&["ws", "new", "other"]);
    let c = w.d.client().await.unwrap();
    let dirty = worktree(&c, "lib/dirty").await;
    std::fs::write(dirty.path.join("scratch.txt"), "unsaved").unwrap();
    let r = run(w
        .tui(&["attach", "worker"])
        .snapshot(1.4)
        // Sidebar: up to the worktree row, fold it, unfold it.
        .send(1.5, "\\x11\\t")
        .send(1.7, "k")
        .send(1.9, "h")
        .snapshot(2.2)
        .send(2.4, "l")
        // x on the worktree row: remove? y → refused with the report, offering force.
        .send(2.6, "x")
        .snapshot(2.9)
        .send(3.0, "y")
        .snapshot(3.8)
        .send(4.0, "n")
        // Workspace switcher.
        .send(4.3, "\\x11w")
        .snapshot(4.7)
        .send(4.9, "other")
        .send(5.1, "\\r")
        // The switch asks the daemon for the workspace's view: room for a slow CI machine.
        .snapshot(6.8)
        .send(7.2, "\\x11d"))
    .await;
    assert!(r.at(1.4).contains("▾ dirty") && r.at(1.4).contains("worker"), "{}", r.at(1.4));
    let folded = r.at(2.2);
    assert!(folded.contains("▸ dirty") && !folded.contains("1 • worker"), "{folded}");
    assert!(r.at(2.9).contains("Remove worktree lib/dirty (dirty)?"), "{}", r.at(2.9));
    let refused = r.at(3.8);
    assert!(refused.contains("1 untracked file") && refused.contains("scratch.txt"), "{refused}");
    assert!(
        refused.contains("discard that work") && refused.contains("a: archive it instead"),
        "{refused}"
    );
    assert!(dirty.path.exists(), "nothing removed without consent");
    let switcher = r.at(4.7);
    assert!(
        switcher.contains("Workspaces") && switcher.contains("main") && switcher.contains("other"),
        "{switcher}"
    );
    // MM1.B: every workspace is in the sidebar; `w` goes to one's header (now the workspace you
    // are in), the others stay listed.
    let other = r.at(6.8);
    assert!(
        other.contains(" · other") && other.contains("▾ other") && other.contains("lib"),
        "{other}"
    );
}

#[tokio::test]
async fn editors_open_inside_or_outside() {
    let w = World::new(
        "[worktrees]\nfetch = false\n[editors]\ndefault = \"fake\"\nfake = { command = [\"sh\", \"-c\", \"echo editing $0; sleep 30\", \"{path}\"], inside = true }\nout = { command = [\"sh\", \"-c\", \"echo $$ > \\\"$0/editor.pid\\\"\", \"{path}\"] }\n",
    );
    let root = w.repo("ed", &[]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.ok(&["wt", "new", "e1", "--project", "ed"]);
    let out = w.ok(&["wt", "open", "ed/e1"]);
    let handle = out.lines().next().unwrap().trim().to_owned();
    let c = w.d.client().await.unwrap();
    let wt = worktree(&c, "ed/e1").await;
    screen_with(&w.d, &c, &handle, &format!("editing {}", wt.path.display())).await;
    let Reply::Session { info } = c.request(Request::Get { addr: handle }).await.unwrap() else {
        panic!()
    };
    assert_eq!(
        info.worktree.as_deref(),
        Some(wt.id.as_str()),
        "opened as a session in the worktree"
    );
    let out = w.cli(&["wt", "open", "ed/e1", "--editor", "nope"]);
    assert_eq!(out.status.code(), Some(3));
    // One the daemon starts on the computer (asked from the web) is reaped when it exits, not left
    // a zombie of the daemon. The CLI starts its own.
    c.request(Request::WorktreeOpen { addr: "ed/e1".into(), editor: Some("out".into()) })
        .await
        .unwrap();
    let pid_file = wt.path.join("editor.pid");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let gone = loop {
        let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
        if !pid.trim().is_empty() {
            let ps = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&ps.stdout).trim().to_owned();
            if stat.is_empty() {
                break true;
            }
            if std::time::Instant::now() > deadline {
                panic!("editor {} is still there: {stat}", pid.trim());
            }
        } else if std::time::Instant::now() > deadline {
            break false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(gone, "the editor never ran");
}

#[tokio::test]
async fn tui_new_worktree_wizard() {
    let w = World::new("[worktrees]\nfetch = false\n");
    let root = w.repo("tool", &[]);
    w.ok(&["project", "add", root.to_str().unwrap()]);
    w.cli(&["new", "-d", "--wt", "tool/@root", "-n", "sh0", "--", "sh", "-c", "sleep 60"]);
    // N, a name, ⏎, then the agent picker: ⏎ takes the default (claude).
    let r = run(w
        .tui(&["attach", "sh0"])
        .send(1.5, "\\x11N")
        .snapshot(1.9)
        .send(2.1, "spike")
        .send(2.3, "\\r")
        .snapshot(2.7)
        .send(2.9, "\\r")
        .snapshot(4.5)
        .send(5.0, "\\x11d"))
    .await;
    assert!(r.at(1.9).contains("New worktree in tool (new worktree)"), "{}", r.at(1.9));
    assert!(r.at(2.7).contains("claude") && r.at(2.7).contains("no agent"), "{}", r.at(2.7));
    let done = r.at(4.5);
    assert!(done.contains("fake-claude wt=spike"), "{done}");
    assert!(done.contains("▾ spike"), "{done}");
    let c = w.d.client().await.unwrap();
    assert_eq!(worktree(&c, "tool/spike").await.branch.as_deref(), Some("spike"));
}
