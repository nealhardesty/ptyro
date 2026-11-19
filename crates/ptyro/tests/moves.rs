//! A repository that moved (ROADMAP UX3.10, SPEC §10.1), end to end: `project move` after a
//! real `mv` (through a symlinked path, as `~/external` → `/Volumes/…`), keeping the project's
//! id, trust, pin, workspaces and worktrees, its worktrees linked to the new place; the refusals
//! (another repository, a copy); auto-registration offering the move instead of registering a
//! duplicate; `project add` asking; an existing duplicate folded in; all of it across a restart.
//! Real daemon, real git, a fake `$HOME`.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::TermSize;
use ptyro_core::model::{ProjectInfo, WorktreeInfo, WorktreeSpec, WorktreeState};
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(15);

const CONFIG: &str = r#"
[worktrees]
fetch = false

[profiles.work]
label = "work"
"#;

const SCRIPTS: &str = "[scripts]\nshell = \"bash\"\nsetup = \"echo set up\"\n";

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-home-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        std::fs::write(h.join(".gitconfig"), "[user]\n\tname = Test User\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n").unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(CONFIG)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
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

    /// A repository cloned from a bare "remote" `<name>.git`, with one pushed commit.
    fn repo(&self, name: &str) -> PathBuf {
        let h = self.home();
        self.git(&h, &["init", "-q", "--bare", &format!("{name}.git")]);
        self.git(&h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("README"), format!("{name}\n")).unwrap();
        std::fs::write(root.join(".ptyro.toml"), SCRIPTS).unwrap();
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        self.git(&root, &["push", "-q", "origin", "main"]);
        self.git(&root, &["remote", "set-head", "origin", "main"]);
        root
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "ptyro {args:?} failed: {text}");
        text
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "ptyro {args:?} should fail: {text}");
        text
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// `mv`, making the parent first.
fn mv(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(from, to).unwrap();
}

async fn project(c: &Client, addr: &str) -> ProjectInfo {
    match c.request(Request::ProjectGet { addr: addr.into() }).await.unwrap() {
        Reply::Project { info, .. } => info,
        other => panic!("{other:?}"),
    }
}

async fn projects(c: &Client) -> Vec<ProjectInfo> {
    match c.request(Request::ProjectList).await.unwrap() {
        Reply::Projects { projects } => projects,
        other => panic!("{other:?}"),
    }
}

async fn worktrees(c: &Client, project: &str) -> Vec<WorktreeInfo> {
    match c.request(Request::WorktreeList { project: Some(project.into()) }).await.unwrap() {
        Reply::Worktrees { worktrees } => worktrees,
        other => panic!("{other:?}"),
    }
}

async fn new_wt(c: &Client, project: &str, name: &str) -> WorktreeInfo {
    let spec = WorktreeSpec {
        project: project.into(),
        name: Some(name.into()),
        ..WorktreeSpec::default()
    };
    match c.request(Request::WorktreeNew { spec }).await.unwrap() {
        Reply::WorktreeCreated { info, .. } => info,
        other => panic!("{other:?}"),
    }
}

async fn spawn(c: &Client, spec: SpawnSpec) -> SessionInfo {
    match c.request(Request::Spawn { spec }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

fn sleeper(cwd: &Path, worktree: Option<&str>) -> SpawnSpec {
    SpawnSpec {
        argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        cwd: cwd.to_path_buf(),
        worktree: worktree.map(str::to_owned),
        size: Some(TermSize::new(100, 30)),
        ..SpawnSpec::default()
    }
}

async fn session(c: &Client, addr: &str) -> SessionInfo {
    match c.request(Request::Get { addr: addr.into() }).await.unwrap() {
        Reply::Session { info } => info,
        other => panic!("{other:?}"),
    }
}

/// Wait until `f` holds (the daemon's sweeps and refreshes take their time).
async fn until<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn trusted(w: &World, slug: &str) -> bool {
    w.ok(&["project", "show", slug]).contains("(trusted)")
}

/// The owner's case, kept whole: a repository moved with `mv` to another disk reached through a
/// symlink. The project keeps its id, trust, pin and workspaces; the worktree outside it (ptyro's
/// worktrees directory) is linked to the new place by git's repair, its unsaved work intact; the
/// one inside moved with it and is found there; a running session keeps running. After a
/// restart, everything is where the move put it.
#[tokio::test]
async fn a_moved_repository_keeps_its_project() {
    let mut w = World::new();
    let h = w.home();
    let root = w.repo("app");
    w.ok(&["project", "add", s(&root)]);
    w.ok(&["project", "trust", "app", "--yes"]);
    w.ok(&["project", "pin", "app", "work"]);
    w.ok(&["ws", "new", "side"]);
    w.ok(&["ws", "add-project", "side", "app"]);
    let c = w.d.client().await.unwrap();
    let id = project(&c, "app").await.id;
    let outside = new_wt(&c, "app", "feat").await;
    assert!(!outside.path.starts_with(&root), "in ptyro's worktrees directory: {outside:?}");
    // One inside the repository too (`[worktrees] location = "in-repo"`).
    w.d.write_config(&CONFIG.replace("fetch = false\n", "fetch = false\nlocation = \"in-repo\"\n"))
        .unwrap();
    c.request(Request::ReloadConfig).await.unwrap();
    let inside = new_wt(&c, "app", "fix").await;
    assert!(inside.path.starts_with(&root), "{inside:?}");
    std::fs::write(outside.path.join("unsaved.txt"), "mine\n").unwrap();
    let running = spawn(&c, sleeper(&outside.path, Some("app/feat"))).await;

    // Moved to another disk, reached through a symlink (`~/external` → `/Volumes/…`).
    let volume = h.join("Volumes/EXT");
    std::os::unix::fs::symlink(&volume, h.join("external")).unwrap();
    mv(&root, &volume.join("dev/app"));
    let typed = h.join("external/dev/app");
    let out = w.ok(&["project", "move", "app", s(&typed)]);
    let new_root = volume.join("dev/app");
    assert!(out.contains(&format!("moved app to {}", new_root.display())), "{out}");
    assert!(out.contains("linked 2 worktrees to the new place"), "{out}");
    assert!(out.contains("1 running session keeps running where it started"), "{out}");

    let p = project(&c, "app").await;
    assert_eq!(p.id, id, "the same project");
    assert_eq!(p.root, new_root, "resolved, as registration resolves");
    assert_eq!(p.pin.as_deref(), Some("work"));
    assert!(trusted(&w, "app"), "its trust stayed");
    assert!(
        w.ok(&["ws", "ls"]).lines().any(|l| l.contains("side") && l.contains("app")),
        "{}",
        w.ok(&["ws", "ls"])
    );
    let wts = worktrees(&c, "app").await;
    let feat = wts.iter().find(|x| x.name == "feat").unwrap();
    let fix = wts.iter().find(|x| x.name == "fix").unwrap();
    let root_wt = wts.iter().find(|x| x.is_root).unwrap();
    assert_eq!(feat.path, outside.path, "outside the repository: it stayed");
    assert_eq!(fix.path, new_root.join(".ptyro/worktrees/fix"), "inside: it moved with it");
    assert_eq!((root_wt.path.as_path(), root_wt.state), (new_root.as_path(), WorktreeState::Ready));
    assert!(wts.iter().all(|x| x.state == WorktreeState::Ready), "{wts:?}");
    // git works in both again, the work where it was.
    assert!(w.git(&outside.path, &["status", "--porcelain"]).contains("unsaved.txt"));
    assert_eq!(w.git(&fix.path, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "fix");
    let listed = w.git(&new_root, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("prunable"), "{listed}");
    assert!(session(&c, &running.name).await.state.is_live(), "it kept running");
    // A new worktree goes from the new place.
    let more = new_wt(&c, "app", "more").await;
    assert!(more.path.starts_with(&new_root), "{more:?}");

    // Moving it where it is says so.
    assert!(w.ok(&["project", "move", "app", s(&new_root)]).contains("already at"));

    // A restart: the move was kept.
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let p = project(&c, "app").await;
    assert_eq!((p.id.as_str(), p.root.as_path()), (id.as_str(), new_root.as_path()));
    let wts = worktrees(&c, "app").await;
    assert_eq!(
        wts.iter().find(|x| x.name == "fix").unwrap().path,
        new_root.join(".ptyro/worktrees/fix")
    );
    assert!(wts.iter().all(|x| x.state == WorktreeState::Ready), "{wts:?}");
    assert!(trusted(&w, "app"));
    assert!(!w.ok(&["project", "ls"]).contains("gone"));
}

/// Never another repository, never a copy: refused with why, nothing changed. A path inside the
/// repository finds its main checkout; a path that isn't one says so.
#[tokio::test]
async fn another_repository_or_a_copy_is_refused() {
    let w = World::new();
    let h = w.home();
    let root = w.repo("app");
    let other = w.repo("other");
    w.ok(&["project", "add", s(&root)]);
    let c = w.d.client().await.unwrap();
    let before = project(&c, "app").await;

    // The old one still there, a copy beside it.
    let copy = h.join("copy/app");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    let cp = std::process::Command::new("cp").arg("-R").arg(&root).arg(&copy).status().unwrap();
    assert!(cp.success());
    let out = w.fails(&["project", "move", "app", s(&copy)]);
    assert!(out.contains("that's a copy, not a move"), "{out}");

    // Gone, and another repository offered in its place.
    mv(&root, &h.join("away/app"));
    std::fs::remove_dir_all(&copy).unwrap();
    let out = w.fails(&["project", "move", "app", s(&other)]);
    assert!(out.contains("isn't app's repository"), "{out}");
    assert!(out.contains("history"), "{out}");
    // A fork: the same history, another remote.
    let fork = h.join("fork");
    w.git(&h, &["clone", "-q", "app.git", "fork"]);
    w.git(&fork, &["remote", "set-url", "origin", "git@github.com:someone/fork.git"]);
    let out = w.fails(&["project", "move", "app", s(&fork)]);
    assert!(out.contains("remote"), "{out}");
    // Not a repository at all.
    std::fs::create_dir_all(h.join("plain")).unwrap();
    let out = w.fails(&["project", "move", "app", s(&h.join("plain"))]);
    assert!(out.contains("not inside a git repository"), "{out}");
    let out = w.fails(&["project", "move", "app", s(&h.join("nowhere"))]);
    assert!(out.contains("nowhere"), "{out}");
    assert_eq!(project(&c, "app").await, before, "nothing changed");

    // A path inside the moved repository finds its main checkout.
    let sub = h.join("away/app/src");
    std::fs::create_dir_all(&sub).unwrap();
    let out = w.ok(&["project", "move", "app", s(&sub)]);
    assert!(out.contains("is inside the repository at"), "{out}");
    assert_eq!(project(&c, "app").await.root, h.join("away/app"));
}

/// A session starts in the moved repository: auto-registration registers no `app-2`, it offers
/// the move on `app` (`moved_to`, and `project ls` says so). `project add` there asks, and says
/// both ways when it can't ask; `--moved` moves it, and the session joins the project.
#[tokio::test]
async fn auto_registration_offers_the_move_instead_of_a_duplicate() {
    let w = World::new();
    let h = w.home();
    let root = w.repo("app");
    w.ok(&["project", "add", s(&root)]);
    let c = w.d.client().await.unwrap();
    let id = project(&c, "app").await.id;
    let moved = h.join("work/app");
    mv(&root, &moved);

    let s1 = spawn(&c, sleeper(&moved, None)).await;
    let offered = until("the move is offered", async || {
        let p = project(&c, "app").await;
        p.moved_to.clone().map(|_| p)
    })
    .await;
    assert_eq!(offered.moved_to.as_deref(), Some(moved.as_path()));
    assert_eq!(projects(&c).await.len(), 1, "no duplicate");
    assert!(session(&c, &s1.name).await.project.is_none(), "not until it's moved");
    let ls = w.ok(&["project", "ls"]);
    assert!(
        ls.contains("gone: it seems to have moved to") && ls.contains("ptyro project move app"),
        "{ls}"
    );
    let v: serde_json::Value =
        serde_json::from_slice(&w.cli(&["doctor", "--quick", "--json"]).stdout).unwrap();
    let checks = v.as_array().cloned().or_else(|| v["checks"].as_array().cloned()).unwrap();
    let found = checks
        .iter()
        .find(|c| c["section"] == "projects" && c["name"] == "app")
        .expect("the doctor says so");
    assert_eq!(found["level"], "warn");
    assert_eq!(found["data"]["move"]["path"], serde_json::json!(moved));

    // `project add` there, with no terminal to ask on: both ways, nothing registered.
    let out = w.fails(&["project", "add", s(&moved)]);
    assert!(out.contains("it seems app moved there"), "{out}");
    assert!(out.contains("--moved") && out.contains("--separate"), "{out}");
    assert_eq!(projects(&c).await.len(), 1);
    let e = c
        .request(Request::ProjectAdd {
            path: moved.clone(),
            workspace: None,
            moved: false,
            separate: false,
        })
        .await
        .unwrap_err();
    assert_eq!(e.fix.map(|f| f.again), Some(ptyro_core::error::Again::Moved), "its way through");

    let out = w.ok(&["project", "add", s(&moved), "--moved"]);
    assert!(out.contains(&format!("moved app to {}", moved.display())), "{out}");
    let p = project(&c, "app").await;
    assert_eq!(
        (p.id.as_str(), p.root.as_path(), p.moved_to.as_ref()),
        (id.as_str(), moved.as_path(), None)
    );
    until("the session joins the project", async || {
        (session(&c, &s1.name).await.project.as_deref() == Some(id.as_str())).then_some(())
    })
    .await;
    assert!(!w.ok(&["project", "ls"]).contains("gone"));
}

/// The duplicate an older ptyro made (the owner's `woa` and `woa-2`): spotted and offered, then
/// `project move app NEWPATH` (through a symlinked path) folds `app-2` into `app`. `app` keeps
/// its id and pin; it gains what only `app-2` had: its trust, its place in a workspace, a
/// worktree made there, a session started there. `app-2` is gone, and stays gone after a
/// restart. Keeping them apart (`--separate`) is remembered: never offered again.
#[tokio::test]
async fn an_existing_duplicate_is_folded_in() {
    let mut w = World::new();
    let h = w.home();
    let root = w.repo("app");
    w.ok(&["project", "add", s(&root)]);
    w.ok(&["project", "pin", "app", "work"]);
    let c = w.d.client().await.unwrap();
    let id = project(&c, "app").await.id;
    let old_feat = new_wt(&c, "app", "feat").await;

    // Moved, and registered again at the new place while something stood at the old one (as a
    // ptyro before 1.53 would have, on its own).
    let volume = h.join("Volumes/EXT");
    std::os::unix::fs::symlink(&volume, h.join("external")).unwrap();
    let new_root = volume.join("dev/app");
    mv(&root, &new_root);
    std::fs::create_dir_all(&root).unwrap();
    w.ok(&["project", "add", s(&new_root)]);
    std::fs::remove_dir(&root).unwrap();
    let dup = project(&c, "app-2").await;
    w.ok(&["project", "trust", "app-2", "--yes"]);
    w.ok(&["ws", "new", "side"]);
    w.ok(&["ws", "add-project", "side", "app-2"]);
    let made_there = new_wt(&c, "app-2", "later").await;
    let dup_root = worktrees(&c, "app-2").await.into_iter().find(|x| x.is_root).unwrap();
    let there = spawn(&c, sleeper(&new_root, Some(&dup_root.id))).await;
    assert_eq!(there.project.as_deref(), Some(dup.id.as_str()));
    let app_root = worktrees(&c, "app").await.into_iter().find(|x| x.is_root).unwrap();

    // Spotted, once the main checkout is noticed gone.
    c.request(Request::WorktreeRefresh { addr: app_root.id.clone() }).await.unwrap();
    until("the duplicate is spotted", async || project(&c, "app").await.moved_to).await;
    let ls = w.ok(&["project", "ls"]);
    assert!(ls.contains(&format!("seems to have moved to {}", new_root.display())), "{ls}");

    let out = w.ok(&["project", "move", "app", s(&h.join("external/dev/app"))]);
    assert!(out.contains("app-2 (registered there on its own) is folded into app"), "{out}");
    assert!(
        out.contains("1 worktree")
            && out.contains("its trust")
            && out.contains("its place in side"),
        "{out}"
    );
    assert!(out.contains("linked 1 worktree to the new place: app/feat"), "{out}");

    // `live`: its session is still running (a restart ends a plain shell).
    let check = async |c: &Client, w: &World, live: bool| {
        let all = projects(c).await;
        assert_eq!(all.len(), 1, "{all:?}");
        let p = &all[0];
        assert_eq!(
            (p.id.as_str(), p.slug.as_str(), p.root.as_path()),
            (id.as_str(), "app", new_root.as_path())
        );
        assert_eq!(p.pin.as_deref(), Some("work"), "what it had wins");
        assert!(trusted(w, "app"), "app-2's trust is app's");
        assert!(w.ok(&["ws", "ls"]).lines().any(|l| l.contains("side") && l.contains("app")));
        let wts = worktrees(c, "app").await;
        let names: Vec<&str> = wts.iter().map(|x| x.name.as_str()).collect();
        assert!(
            names.contains(&"@root") && names.contains(&"feat") && names.contains(&"later"),
            "{names:?}"
        );
        assert_eq!(wts.iter().filter(|x| x.is_root).count(), 1);
        let later = wts.iter().find(|x| x.name == "later").unwrap();
        assert_eq!(
            (later.id.as_str(), later.path.as_path()),
            (made_there.id.as_str(), made_there.path.as_path())
        );
        assert!(wts.iter().all(|x| x.state == WorktreeState::Ready), "{wts:?}");
        if live {
            let s = session(c, &there.name).await;
            assert_eq!(s.project.as_deref(), Some(id.as_str()), "its session is app's");
            assert_eq!(s.worktree.as_deref(), Some(app_root.id.as_str()), "in app's main checkout");
        }
    };
    check(&c, &w, true).await;
    assert!(w.git(&old_feat.path, &["status", "--short", "--branch"]).contains("feat"));
    assert!(w.fails(&["project", "show", "app-2"]).contains("app-2"));

    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    check(&c, &w, false).await;

    // Kept apart on purpose: remembered, never offered again.
    let elsewhere = h.join("elsewhere/app");
    mv(&new_root, &elsewhere);
    c.request(Request::WorktreeRefresh { addr: app_root.id.clone() }).await.unwrap();
    let out = w.ok(&["project", "add", s(&elsewhere), "--separate"]);
    assert!(out.contains("app-2"), "{out}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let p = project(&c, "app").await;
    assert_eq!(p.moved_to, None, "declined: {p:?}");
}
