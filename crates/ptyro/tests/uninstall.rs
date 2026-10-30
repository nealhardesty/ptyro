//! `ptyro uninstall` and `ptyro backup` end to end (ROADMAP H2.3). Everything runs in a fake
//! `$HOME` and a throwaway ptyro home (or fake XDG directories), so nothing of the machine's is
//! touched: the pure planning and classification are unit-tested in the crate.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ptyro_core::model::WorktreeSpec;
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-uninst-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let d = TempDaemon::start_with(
            BIN,
            &[("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")],
            None,
        )
        .unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    /// A repository cloned from a bare "remote" with one pushed commit, registered as a project.
    fn project(&self, name: &str) -> PathBuf {
        let h = self.home();
        git(&h, &h, &["init", "-q", "--bare", &format!("{name}.git")]);
        git(&h, &h, &["clone", "-q", &format!("{name}.git"), name]);
        let root = h.join(name);
        std::fs::write(root.join("README"), "hi\n").unwrap();
        git(&h, &root, &["add", "-A"]);
        git(&h, &root, &["commit", "-q", "-m", "init"]);
        git(&h, &root, &["push", "-q", "origin", "main"]);
        git(&h, &root, &["remote", "set-head", "origin", "main"]);
        self.ok(&["project", "add", root.to_str().unwrap()]);
        root
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(out.status.success(), "ptyro {args:?}: {}", text(&out));
        text(&out)
    }

    async fn worktree(&self, project: &str, name: &str) -> PathBuf {
        let c = self.d.client().await.unwrap();
        let spec = WorktreeSpec {
            project: project.into(),
            name: Some(name.into()),
            setup: false,
            fetch: Some(false),
            ..WorktreeSpec::default()
        };
        match c.request(Request::WorktreeNew { spec }).await.unwrap() {
            Reply::WorktreeCreated { info, .. } => info.path,
            other => panic!("{other:?}"),
        }
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// The plan names every directory, flags the worktree that has uncommitted work, and removes
/// nothing; asked inside a session it still answers (it only reads).
#[tokio::test]
async fn the_plan_flags_lost_work_and_a_dry_run_removes_nothing() {
    let w = World::new();
    let root = w.project("app");
    let wt = w.worktree("app", "fix").await;
    std::fs::write(wt.join("notes.md"), "mine\n").unwrap();
    std::fs::write(wt.join("README"), "edited\n").unwrap();

    let out =
        w.d.cmd()
            .args(["uninstall", "--dry-run", "--json"])
            .env("HOME", w.home())
            .env("PTYRO_SESSION_ID", "abc")
            .output()
            .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let dirs: Vec<&str> =
        plan["dirs"].as_array().unwrap().iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(dirs.contains(&"config") || dirs.contains(&"data"), "{dirs:?}");
    assert_eq!(plan["worktrees"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(plan["worktrees"][0]["uncommitted"], 2);
    assert_eq!(plan["uncommitted_total"], 1);
    assert_eq!(plan["worktrees"][0]["branch"], "fix");
    // A sandbox keeps the machine's things and the binary.
    assert_eq!(plan["binary"]["action"], "keep");
    assert!(plan["service"].is_null() && plan["integrations"].as_array().unwrap().is_empty());
    assert_eq!(plan["daemon_running"], true);
    // The human form says it plainly.
    let said =
        text(&w.d.cmd().args(["uninstall", "--dry-run"]).env("HOME", w.home()).output().unwrap());
    assert!(
        said.contains("2 uncommitted change(s) will be lost") && said.contains("a dry run"),
        "{said}"
    );
    // Nothing went.
    assert!(wt.join("notes.md").exists() && root.exists() && w.d.paths.data.exists());
    assert!(w.d.client().await.is_ok(), "the daemon was left running");
}

/// `--yes` is not enough to lose work: it takes `--delete-work` (or typing `delete`). Then the
/// worktree, its directory, the daemon and ptyro's home are gone; the repository, its branch and
/// its snapshot refs stay.
#[tokio::test]
async fn lost_work_needs_delete_and_then_only_ptyros_own_things_go() {
    let w = World::new();
    let root = w.project("app");
    let wt = w.worktree("app", "fix").await;
    std::fs::write(wt.join("notes.md"), "mine\n").unwrap();
    git(&w.home(), &root, &["update-ref", "refs/ptyro/archive/old", "HEAD"]);
    let data = w.d.paths.data.clone();

    // A script that only said --yes is refused, and nothing is touched.
    let out = w.cli(&["uninstall", "--yes"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("uncommitted work would be lost"), "{}", text(&out));
    assert!(wt.join("notes.md").exists() && data.exists());
    assert!(w.d.client().await.is_ok());
    // Not a terminal and no --yes at all: refused with the reason.
    let out = w.cli(&["uninstall", "--delete-work"]);
    assert!(text(&out).contains("not a terminal"), "{}", text(&out));
    assert!(wt.exists());

    let out = w.cli(&["uninstall", "--yes", "--delete-work", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("ptyro is uninstalled"), "{}", text(&out));
    assert!(!wt.exists(), "its worktree is gone");
    assert!(!data.exists() && !w.d.paths.config.exists());
    assert!(w.d.client().await.is_err(), "the daemon is stopped");
    // The repository is as it was, with its branch and the snapshot ref.
    assert!(root.join("README").exists() && root.join(".git").exists());
    let branches = git(&w.home(), &root, &["branch", "--list", "fix"]);
    assert!(branches.contains("fix"), "the branch stays: {branches:?}");
    assert!(
        git(&w.home(), &root, &["worktree", "list"]).lines().count() == 1,
        "git forgot the worktree"
    );
    assert!(
        git(&w.home(), &root, &["for-each-ref", "refs/ptyro"]).contains("refs/ptyro/archive/old")
    );
}

/// `--purge-refs` is the only way the snapshots go.
#[tokio::test]
async fn snapshot_refs_are_kept_unless_purged() {
    let w = World::new();
    let root = w.project("app");
    git(&w.home(), &root, &["update-ref", "refs/ptyro/archive/a", "HEAD"]);
    git(&w.home(), &root, &["update-ref", "refs/ptyro/taken/b", "HEAD"]);
    let plan: serde_json::Value =
        serde_json::from_slice(&w.cli(&["uninstall", "--dry-run", "--json"]).stdout).unwrap();
    assert_eq!(
        (plan["refs"][0]["archive"].clone(), plan["refs"][0]["taken"].clone()),
        (1.into(), 1.into()),
        "{plan}"
    );
    assert!(plan["kept"].to_string().contains("snapshot ref"), "{}", plan["kept"]);
    let out = w.cli(&["uninstall", "--yes", "--purge-refs", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(git(&w.home(), &root, &["for-each-ref", "refs/ptyro"]).trim().is_empty());
    assert!(
        git(&w.home(), &root, &["log", "-1", "--format=%s"]).contains("init"),
        "the history is untouched"
    );
}

/// On a machine (no PTYRO_HOME): the config, data and runtime directories under the XDG
/// variables, the lines ptyro put in the rc file (and only those) and the binary it was run from
/// when that is a hand-installed copy. All of it in a fake home.
#[test]
fn a_machine_install_loses_its_files_its_shell_lines_and_its_binary() {
    let base = tempfile::Builder::new().prefix("ptyro-machine-").tempdir().unwrap();
    let h = base.path().canonicalize().unwrap();
    let (config, data, state, run) =
        (h.join("xdg/config"), h.join("xdg/data"), h.join("xdg/state"), h.join("xdg/run"));
    for d in [&config, &data, &state, &run] {
        std::fs::create_dir_all(d.join("ptyro")).unwrap();
    }
    std::fs::write(config.join("ptyro/config.toml"), "[ui]\nmouse = false\n").unwrap();
    std::fs::write(data.join("ptyro/scratchpad.md"), "# Scratchpad\n").unwrap();
    // A hand-installed copy of the binary, the one that runs.
    std::fs::create_dir_all(h.join(".local/bin")).unwrap();
    let exe = h.join(".local/bin/ptyro");
    std::fs::copy(BIN, &exe).unwrap();
    let zshrc = h.join(".zshrc");
    std::fs::write(&zshrc, "export A=1\n\n# ptyro completions (live session and worktree names)\nsource <(ptyro completions zsh)\nalias ll='ls -l'\neval \"$(ptyro shell-init zsh)\"\n").unwrap();
    let repo = h.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(".ptyro.toml"), "[project]\nname = \"x\"\n").unwrap();

    let run_it = |args: &[&str]| -> Output {
        // Linux: a child another test forks while the copy is open for writing holds that
        // descriptor until it execs, and running the copy meanwhile is "Text file busy".
        for _ in 0..50 {
            let out = Command::new(&exe)
                .args(args)
                .env_clear()
                .env("HOME", &h)
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("XDG_CONFIG_HOME", &config)
                .env("XDG_DATA_HOME", &data)
                .env("XDG_STATE_HOME", &state)
                .env("XDG_RUNTIME_DIR", &run)
                .output();
            match out {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                out => return out.unwrap(),
            }
        }
        panic!("{} stayed busy", exe.display());
    };
    let dry = run_it(&["uninstall", "--dry-run", "--json"]);
    assert!(dry.status.success(), "{}", text(&dry));
    let plan: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(plan["binary"]["action"], "remove", "{plan}");
    assert_eq!(
        plan["rc"][0]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| !l.as_str().unwrap().trim().is_empty())
            .count(),
        3,
        "{plan}"
    );
    assert!(
        zshrc.exists() && config.join("ptyro/config.toml").exists(),
        "a dry run changes nothing"
    );

    let out = run_it(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !config.join("ptyro").exists()
            && !data.join("ptyro").exists()
            && !run.join("ptyro").exists()
    );
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), "export A=1\nalias ll='ls -l'\n");
    assert!(!exe.exists(), "the binary it was run from is gone");
    assert!(repo.join(".ptyro.toml").exists(), "a repository's own file stays");
}

/// Inside a ptyro session an uninstall of the machine's ptyro is refused before anything is read
/// or changed (the daemon it would stop is the one this shell runs in). The fake XDG
/// directories make sure that even a regression here could only touch the test's own.
#[test]
fn it_refuses_inside_a_session() {
    let base = tempfile::tempdir().unwrap();
    let h = base.path().canonicalize().unwrap();
    let out = Command::new(BIN)
        .args(["uninstall", "--yes"])
        .env_clear()
        .env("HOME", &h)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("XDG_CONFIG_HOME", h.join("c"))
        .env("XDG_DATA_HOME", h.join("d"))
        .env("XDG_STATE_HOME", h.join("s"))
        .env("XDG_RUNTIME_DIR", h.join("r"))
        .env("PTYRO_SESSION_ID", "abc")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        text(&out).contains("inside a ptyro session") && text(&out).contains("Ctrl-Q d"),
        "{}",
        text(&out)
    );
}

fn list(file: &Path) -> Vec<String> {
    let out = Command::new("tar").arg("-tzf").arg(file).output().unwrap();
    assert!(out.status.success(), "tar: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect()
}

/// A backup is a private tar.gz with a database that opens by itself, the config and the
/// scratchpad; no agent logins; and it never replaces a file.
#[tokio::test]
async fn a_backup_holds_the_state_not_the_logins_and_never_replaces_a_file() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new();
    w.project("app");
    w.d.write_config("[ui]\nmouse = false # mine\n").unwrap();
    // A managed profile's login: never in a backup.
    let login = w.d.paths.data.join("profiles/work/claude");
    std::fs::create_dir_all(&login).unwrap();
    std::fs::write(login.join(".credentials.json"), "{\"secret\":1}").unwrap();
    std::fs::create_dir_all(w.d.paths.data.join("sessions/s1")).unwrap();
    std::fs::write(w.d.paths.data.join("sessions/s1/pty.log"), "screen text").unwrap();

    let file = w.home().join("out/b.tar.gz");
    let said = w.ok(&["backup", "-o", file.to_str().unwrap()]);
    assert!(said.contains("data/ptyro.db") && said.contains("agent logins"), "{said}");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let names = list(&file);
    for want in [
        "ptyro-backup/README.txt",
        "ptyro-backup/manifest.json",
        "ptyro-backup/config/config.toml",
        "ptyro-backup/data/ptyro.db",
    ] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains("credentials") || n.contains("profiles") || n.contains("pty.log")),
        "{names:?}"
    );
    // The database opens on its own and has the project.
    let x = tempfile::tempdir().unwrap();
    assert!(
        Command::new("tar")
            .arg("-xzf")
            .arg(&file)
            .arg("-C")
            .arg(x.path())
            .status()
            .unwrap()
            .success()
    );
    let store = ptyro_store::Store::open(&x.path().join("ptyro-backup/data/ptyro.db")).unwrap();
    let reg = store.load_registry().await.unwrap();
    assert!(reg.projects.iter().any(|p| p.slug == "app"), "{:?}", reg.projects);
    assert!(
        std::fs::read_to_string(x.path().join("ptyro-backup/config/config.toml"))
            .unwrap()
            .contains("# mine")
    );
    // It never replaces a file.
    std::fs::write(&file, "mine").unwrap();
    let again = w.cli(&["backup", "-o", file.to_str().unwrap()]);
    assert!(!again.status.success() && text(&again).contains("never replace"), "{}", text(&again));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    // The recordings come only when asked.
    let with = w.home().join("out/c.tar.gz");
    w.ok(&["backup", "-o", with.to_str().unwrap(), "--with-recordings", "--json"]);
    assert!(list(&with).iter().any(|n| n.ends_with("sessions/s1/pty.log")));
    // And an uninstall that is asked for a backup first makes it, before anything goes.
    let kept = w.home().join("out/before.tar.gz");
    let out = w.cli(&["uninstall", "--yes", "--keep-binary", "--backup", kept.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(kept.exists() && !w.d.paths.data.exists());
}
