//! Tests against real repositories in temp dirs, with a bare repo as the "remote". git runs with
//! an isolated global config so the developer's settings and hooks never leak in.

use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    git: Git,
    remote: PathBuf,
    root: PathBuf,
    base: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let gitconfig = base.join("gitconfig");
    std::fs::write(
        &gitconfig,
        "[user]\n\tname = Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n",
    )
    .unwrap();
    let mut env: Vec<(OsString, OsString)> =
        std::env::vars_os().filter(|(k, _)| !k.to_string_lossy().starts_with("GIT_")).collect();
    env.push(("GIT_CONFIG_GLOBAL".into(), gitconfig.into_os_string()));
    env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
    let git = Git::with_env(env);
    let remote = base.join("remote.git");
    git.run(&base, &["init", "--bare", "-q", "remote.git"]).unwrap();
    git.run(&base, &["clone", "-q", "remote.git", "repo"]).unwrap();
    let root = base.join("repo");
    std::fs::write(root.join("README"), "hello\n").unwrap();
    std::fs::write(root.join(".gitignore"), ".env\nnode_modules/\n*.log\n").unwrap();
    git.run(&root, &["add", "."]).unwrap();
    git.run(&root, &["commit", "-q", "-m", "first"]).unwrap();
    git.run(&root, &["push", "-q", "origin", "main"]).unwrap();
    git.run(&root, &["remote", "set-head", "origin", "main"]).unwrap();
    Fixture { _dir: dir, git, remote, root, base }
}

fn commit(f: &Fixture, dir: &Path, file: &str, msg: &str) {
    std::fs::write(dir.join(file), msg).unwrap();
    f.git.run(dir, &["add", file]).unwrap();
    f.git.run(dir, &["commit", "-q", "-m", msg]).unwrap();
}

#[test]
fn registration_facts() {
    let f = fixture();
    assert_eq!(f.git.main_root(&f.root).unwrap(), f.root);
    std::fs::create_dir(f.root.join("sub")).unwrap();
    assert_eq!(f.git.main_root(&f.root.join("sub")).unwrap(), f.root);
    assert_eq!(f.git.default_branch(&f.root).as_deref(), Some("main"));
    assert_eq!(f.git.default_base(&f.root).as_deref(), Some("origin/main"));
    let r = f.git.main_remote(&f.root).unwrap();
    assert_eq!(r.name, "origin");
    assert!(r.url.ends_with("remote.git"));
    assert!(f.git.main_root(&f.base).is_err(), "not a repo");
    let _ = &f.remote;
}

#[test]
fn worktree_lifecycle_and_list() {
    let f = fixture();
    let wt = f.base.join("wts/brisk-rook");
    let src = Source::NewBranch { branch: "ptyro/brisk-rook".into(), base: "origin/main".into() };
    f.git.worktree_add(&f.root, &wt, &src).unwrap();
    assert!(wt.join("README").exists());
    // From inside a linked worktree, the main root is still found.
    assert_eq!(f.git.main_root(&wt).unwrap(), f.root);
    assert!(f.git.git_dir(&wt).unwrap().starts_with(f.root.join(".git/worktrees")));
    let err = f.git.worktree_add(&f.root, &f.base.join("wts/again"), &src).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");

    f.git.lock(&f.root, &wt, "ptyro:01abc").unwrap();
    f.git.lock(&f.root, &wt, "ptyro:01abc").unwrap();
    let list = f.git.worktree_list(&f.root).unwrap();
    assert_eq!(list.len(), 2);
    let e = list.iter().find(|e| e.path == wt).unwrap();
    assert_eq!(e.branch.as_deref(), Some("ptyro/brisk-rook"));
    assert_eq!(e.locked.as_deref(), Some("ptyro:01abc"));
    f.git.unlock(&f.root, &wt).unwrap();
    f.git.unlock(&f.root, &wt).unwrap();

    let s = f.git.status(&wt, Some("origin/main")).unwrap();
    assert_eq!(s.branch.as_deref(), Some("ptyro/brisk-rook"));
    assert_eq!(s.upstream, None, "--no-track");
    assert_eq!((s.dirty(), s.base_ahead), (0, 0));
    commit(&f, &wt, "a.txt", "one\ntwo\n");
    std::fs::write(wt.join("README"), "changed\n").unwrap();
    std::fs::write(wt.join("new.txt"), "x").unwrap();
    let s = f.git.status(&wt, Some("origin/main")).unwrap();
    assert_eq!((s.base_ahead, s.modified, s.untracked), (1, 1, 1));
    assert_eq!(s.insertions, 3, "2 committed + 1 changed line since the merge base");
    assert_eq!(s.subject.as_deref(), Some("one two"), "the first paragraph, joined");

    f.git.worktree_remove(&f.root, &wt, true).unwrap();
    assert!(!wt.exists());
    assert_eq!(f.git.worktree_list(&f.root).unwrap().len(), 1);
}

#[test]
fn existing_branches_and_remote_branches() {
    let f = fixture();
    f.git.run(&f.root, &["branch", "feature"]).unwrap();
    f.git
        .worktree_add(&f.root, &f.base.join("w1"), &Source::Existing { branch: "feature".into() })
        .unwrap();
    // A branch only on the remote is checked out tracking it.
    f.git.run(&f.root, &["push", "-q", "origin", "main:remote-only"]).unwrap();
    f.git.fetch(&f.root, "origin", "remote-only").unwrap();
    f.git.run(&f.root, &["fetch", "-q", "origin"]).unwrap();
    f.git
        .worktree_add(
            &f.root,
            &f.base.join("w2"),
            &Source::Existing { branch: "remote-only".into() },
        )
        .unwrap();
    let s = f.git.status(&f.base.join("w2"), None).unwrap();
    assert_eq!(s.upstream.as_deref(), Some("origin/remote-only"));
    let err = f.git.worktree_add(
        &f.root,
        &f.base.join("w3"),
        &Source::Existing { branch: "nope".into() },
    );
    assert!(err.is_err());
    assert!(f.git.check_branch_name("bad..name").is_err());
    assert!(f.git.check_branch_name("ptyro/ok-name").is_ok());
}

#[test]
fn safety_matrix() {
    let f = fixture();
    let wt = f.base.join("wt");
    let branch = "ptyro/wt";
    f.git
        .worktree_add(
            &f.root,
            &wt,
            &Source::NewBranch { branch: branch.into(), base: "origin/main".into() },
        )
        .unwrap();
    let check = || f.git.safety(&wt, Some(branch), Some("origin/main")).unwrap();
    assert!(check().is_clean(), "{:?}", check());

    // Ignored files are not at risk; untracked ones are.
    std::fs::write(wt.join("debug.log"), "x").unwrap();
    assert!(check().is_clean());
    std::fs::write(wt.join("notes.md"), "x").unwrap();
    assert_eq!(check().untracked, vec!["notes.md".to_owned()]);
    std::fs::remove_file(wt.join("notes.md")).unwrap();

    std::fs::write(wt.join("README"), "edited\n").unwrap();
    let s = check();
    assert_eq!(s.uncommitted.len(), 1);
    assert!(s.report().contains("1 uncommitted change"), "{}", s.report());
    f.git.run(&wt, &["checkout", "-q", "--", "README"]).unwrap();

    std::fs::write(wt.join("README"), "stashed\n").unwrap();
    f.git.run(&wt, &["stash", "-q"]).unwrap();
    assert_eq!(check().stashes.len(), 1, "{:?}", check());
    // A stash from another branch doesn't count.
    std::fs::write(f.root.join("README"), "main stash\n").unwrap();
    f.git.run(&f.root, &["stash", "-q"]).unwrap();
    assert_eq!(check().stashes.len(), 1);
    f.git.run(&wt, &["stash", "drop", "-q", "stash@{1}"]).unwrap();
    f.git.run(&wt, &["stash", "drop", "-q"]).unwrap();
    assert!(check().is_clean());

    commit(&f, &wt, "work.txt", "work");
    assert_eq!(check().unpushed.len(), 1);
    assert!(matches!(f.git.branch_fate(&f.root, branch, Some("origin/main")), BranchFate::Keep(_)));
    // Pushed: no longer at risk, and the branch may go because it's identical to its upstream.
    f.git.run(&wt, &["push", "-q", "-u", "origin", branch]).unwrap();
    assert!(check().is_clean(), "{:?}", check());
    assert!(
        matches!(f.git.branch_fate(&f.root, branch, Some("origin/main")), BranchFate::Delete(r) if r.contains("identical"))
    );
    assert!(f.git.is_published(&f.root, branch));
    // Ahead of its upstream again: kept.
    commit(&f, &wt, "more.txt", "more");
    match f.git.branch_fate(&f.root, branch, Some("origin/main")) {
        BranchFate::Keep(r) => assert!(r.contains("1 commit"), "{r}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn merged_local_branches_may_be_deleted() {
    let f = fixture();
    let wt = f.base.join("wt");
    f.git
        .worktree_add(
            &f.root,
            &wt,
            &Source::NewBranch { branch: "ptyro/m".into(), base: "main".into() },
        )
        .unwrap();
    commit(&f, &wt, "m.txt", "m");
    assert_eq!(f.git.safety(&wt, Some("ptyro/m"), Some("main")).unwrap().unpushed.len(), 1);
    f.git.run(&f.root, &["merge", "-q", "--ff-only", "ptyro/m"]).unwrap();
    assert!(
        f.git.safety(&wt, Some("ptyro/m"), Some("main")).unwrap().is_clean(),
        "merged into the base"
    );
    assert!(matches!(f.git.branch_fate(&f.root, "ptyro/m", Some("main")), BranchFate::Delete(_)));
    f.git.worktree_remove(&f.root, &wt, false).unwrap();
    f.git.delete_branch(&f.root, "ptyro/m").unwrap();
    assert!(!f.git.branch_exists(&f.root, "ptyro/m"));
    assert!(!f.git.is_published(&f.root, "main-only-local"));
}

#[test]
fn worktreeinclude_semantics() {
    let f = fixture();
    std::fs::write(f.root.join(".env"), "SECRET=1").unwrap();
    std::fs::create_dir_all(f.root.join("node_modules/x")).unwrap();
    std::fs::write(f.root.join("node_modules/x/i.js"), "").unwrap();
    std::fs::write(f.root.join("untracked.txt"), "").unwrap();
    std::fs::create_dir_all(f.root.join("config")).unwrap();
    std::fs::write(f.root.join("config/local.yml"), "").unwrap();
    // Matched and ignored: copied. Matched but not ignored (untracked.txt): not.
    let pats = vec![".env".to_owned(), "untracked.txt".to_owned()];
    assert_eq!(
        f.git.untracked_matching(&f.root, &pats, true).unwrap(),
        vec![PathBuf::from(".env")]
    );
    // `worktree.copy` takes untracked files whether ignored or not.
    let pats = vec!["config/*.yml".to_owned(), "untracked.txt".to_owned(), "README".to_owned()];
    assert_eq!(
        f.git.untracked_matching(&f.root, &pats, false).unwrap(),
        vec![PathBuf::from("config/local.yml"), PathBuf::from("untracked.txt")],
        "tracked files are never copied"
    );
    f.git.exclude(&f.root, "/.ptyro/").unwrap();
    f.git.exclude(&f.root, "/.ptyro/").unwrap();
    let ex = std::fs::read_to_string(f.root.join(".git/info/exclude")).unwrap();
    assert_eq!(ex.matches("/.ptyro/").count(), 1);
}

#[test]
fn many_ignored_paths_do_not_deadlock() {
    // Well past a pipe buffer each way (~64 KiB) through `check-ignore --stdin`.
    let f = fixture();
    let dir = f.root.join("logs");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..4000 {
        std::fs::write(dir.join(format!("a-rather-long-file-name-number-{i:05}.log")), "").unwrap();
    }
    let found = f.git.untracked_matching(&f.root, &["logs/".to_owned()], true).unwrap();
    assert_eq!(found.len(), 4000);
}

#[test]
fn porcelain_parsing() {
    let out = "worktree /a\nHEAD 1111\nbranch refs/heads/main\n\nworktree /b\nHEAD 2222\ndetached\nlocked ptyro:x\nprunable gitdir file points to non-existent location\n\n";
    let v = parse_worktree_list(out);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].branch.as_deref(), Some("main"));
    assert!(v[1].detached && v[1].prunable);
    assert_eq!(v[1].locked.as_deref(), Some("ptyro:x"));
}

#[test]
fn remote_normalization() {
    for url in [
        "git@github.com:Acme/repo.git",
        "https://github.com/acme/repo",
        "ssh://git@github.com/acme/repo.git",
        "ssh://git@github.com:22/acme/repo.git",
        "github.com:acme/repo",
    ] {
        assert_eq!(normalize_remote(url), "github.com/acme/repo", "{url}");
    }
    // scp style has no port: leading digits are the path.
    for url in [
        "git@github.com:1password/cli.git",
        "https://github.com/1password/cli",
        "ssh://git@github.com:2222/1password/cli.git",
    ] {
        assert_eq!(normalize_remote(url), "github.com/1password/cli", "{url}");
    }
}

#[test]
fn option_like_revisions_are_not_options() {
    let f = fixture();
    assert_eq!(f.git.rev(&f.root, "--all"), None);
    assert!(!f.git.ref_exists(&f.root, "--output=/tmp/x"));
    assert!(f.git.subjects_since(&f.root, "--all").is_empty());
    assert_eq!(f.git.subjects_since(&f.root, "HEAD"), Vec::<String>::new());
}

#[test]
fn clone_tree_copies() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("deep")).unwrap();
    std::fs::write(src.join("deep/f"), "data").unwrap();
    let dst = dir.path().join("out/dst");
    // CoW may or may not be available on the test file system; the full fallback always works.
    clone_tree(&src, &dst, true).unwrap();
    assert_eq!(std::fs::read_to_string(dst.join("deep/f")).unwrap(), "data");
}
