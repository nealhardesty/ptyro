use std::process::Command;

use ptyro_core::kb::{ChunkKind, KbChunk, KbItem, KbRootInfo};
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::ItemFilter;

use super::super::requests::tests::{ask, daemon, search};
use super::*;

fn put(dir: &Path, rel: &str, text: impl AsRef<[u8]>) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

/// A file's time moves on even where the clock is coarse.
fn put_later(dir: &Path, rel: &str, text: &str) {
    let p = put(dir, rel, text);
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(later).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn spec(path: &Path) -> RootSpec {
    RootSpec { path: path.to_string_lossy().into_owned(), ..RootSpec::default() }
}

async fn ingest(d: &Arc<Daemon>, spec: RootSpec) -> (KbRootInfo, KbPlan) {
    let said = match ask(d, Request::KbIngest { spec, machine: None }).await.unwrap() {
        Reply::KbIngested { root, plan } => (root, plan),
        r => panic!("{r:?}"),
    };
    d.kb_idle().await;
    said
}

async fn sync(d: &Arc<Daemon>, roots: &[&str]) -> u32 {
    let roots = roots.iter().map(|r| (*r).to_owned()).collect();
    let n = match ask(d, Request::KbSync { roots, machine: None }).await.unwrap() {
        Reply::KbSynced { roots, .. } => roots,
        r => panic!("{r:?}"),
    };
    d.kb_idle().await;
    n
}

async fn roots(d: &Arc<Daemon>) -> Vec<KbRootInfo> {
    match ask(d, Request::KbRoots { machine: None }).await.unwrap() {
        Reply::KbRoots { roots } => roots,
        r => panic!("{r:?}"),
    }
}

/// The files kept, by their paths inside their roots.
async fn files(d: &Arc<Daemon>) -> Vec<String> {
    let f = ItemFilter { kinds: vec![ItemKind::File], ..ItemFilter::default() };
    let mut rels: Vec<String> =
        d.kb_db().unwrap().list(f).await.unwrap().into_iter().filter_map(|i| i.rel).collect();
    rels.sort();
    rels
}

async fn item(d: &Arc<Daemon>, path: &Path) -> Option<(KbItem, Vec<KbChunk>)> {
    let db = d.kb_db().unwrap();
    let item = db.item_by_uri(file_uri(path)).await.unwrap()?;
    let chunks = db.chunks(item.id.clone(), 0, 0).await.unwrap();
    Some((item, chunks))
}

/// What a search finds, as each hit's item's path or title.
async fn found(d: &Arc<Daemon>, query: &str) -> Vec<String> {
    match ask(d, search(query)).await.unwrap() {
        Reply::KbHits { hits, .. } => hits.into_iter().filter_map(|h| h.title).collect(),
        r => panic!("{r:?}"),
    }
}

fn canon(t: &tempfile::TempDir) -> PathBuf {
    ptyro_core::os::canonicalize(t.path()).unwrap()
}

const LIMITER: &str = "/**\n * Limits how many requests one client may make in a minute.\n */\nexport class RateLimiter {}\n";

#[tokio::test]
async fn a_folder_is_read_by_its_tiers_and_kept_current() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\nmax_file = '4KiB'\n");
    let dir = canon(&home).join("handbook");
    put(&dir, "docs/DeployGuide.md", "# Deploying\n\nPush the tag and wait for the pelican.\n");
    put(&dir, "src/RateLimiter.ts", LIMITER);
    put(&dir, "data.json", "{\"walrus\": 1}");
    put(&dir, ".env", "TOKEN=abc123def456ghi\n");
    put(&dir, "certs/server.pem", "-----BEGIN CERTIFICATE-----\n");
    put(&dir, "logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
    put(&dir, "big.md", "# Big\n\n".to_owned() + &"lots of ostrich text\n".repeat(400));
    put(&dir, "node_modules/x/index.js", "// A comment in a dependency nobody wrote here.\n");
    put(&dir, ".cache/notes.md", "# hidden heron\n");
    let keys: String = (0..30).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect();
    put(&dir, "fixtures.txt", keys);

    let (root, plan) = ingest(&d, spec(&dir)).await;
    assert_eq!((root.root.name.as_str(), root.root.kind), ("handbook", RootKind::Folder));
    assert_eq!((root.root.profile.as_deref(), root.root.project.as_deref()), (None, None));
    assert!(!plan.existing && !plan.git && plan.comments && !plan.code && !plan.commits);
    // (The never list is applied before the count: `.env` and the certificate are not in it.)
    assert_eq!((plan.files, plan.documents, plan.sources), (6, 3, 1), "{plan:?}");
    assert_eq!(plan.line(), "6 files: 3 documents, 1 source file (comments)");

    // The path of every text file under the size limit; never the keys, the binary, the
    // hidden folder, the dependency, the big one or the file of credentials.
    assert_eq!(files(&d).await, ["data.json", "docs/DeployGuide.md", "src/RateLimiter.ts"]);
    let skipped: Vec<(String, String)> =
        d.kb_skipped().into_iter().map(|s| (s.uri, s.why)).collect();
    let why = |name: &str| {
        skipped.iter().find(|(u, _)| u.ends_with(name)).map(|(_, w)| w.as_str()).unwrap_or("")
    };
    assert!(why("big.md").contains("too big"), "{skipped:?}");
    assert!(why("fixtures.txt").contains("credentials"), "{skipped:?}");
    assert_eq!(skipped.len(), 2, "{skipped:?}");

    // A heading, a comment and a file's name are each found.
    assert_eq!(found(&d, "pelican").await, ["docs/DeployGuide.md"]);
    assert_eq!(found(&d, "requests one client").await, ["src/RateLimiter.ts"]);
    assert_eq!(found(&d, "rate limiter").await, ["src/RateLimiter.ts"]);
    assert_eq!(found(&d, "deploy guide").await, ["docs/DeployGuide.md"]);
    assert!(found(&d, "ostrich").await.is_empty() && found(&d, "heron").await.is_empty());
    assert!(found(&d, "dependency nobody").await.is_empty());
    let (it, chunks) = item(&d, &dir.join("src/RateLimiter.ts")).await.unwrap();
    assert_eq!((it.writer, it.root.as_deref()), (Writer::File, Some(root.root.id.as_str())));
    assert_eq!(
        chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChunkKind::Path, ChunkKind::Comment]
    );
    assert_eq!(chunks[1].anchor.as_deref(), Some("L1-L3"));
    assert_eq!(chunks[1].heading.as_deref(), Some("export class RateLimiter {}"));
    let listed = &roots(&d).await[0];
    assert_eq!((listed.files, listed.commits, listed.root.state), (3, 0, RootState::Ok));
    assert!(listed.root.synced_ms.is_some() && listed.progress.is_none());

    // Nothing changed: a pass reads nothing. Then a change, a rename and a deletion.
    let before = item(&d, &dir.join("data.json")).await.unwrap().0.ingested_ms;
    assert_eq!(sync(&d, &["handbook"]).await, 1);
    assert_eq!(item(&d, &dir.join("data.json")).await.unwrap().0.ingested_ms, before);
    put_later(
        &dir,
        "docs/DeployGuide.md",
        "# Deploying\n\nPush the tag and wait for the toucan.\n",
    );
    std::fs::rename(dir.join("src/RateLimiter.ts"), dir.join("src/Throttle.ts")).unwrap();
    std::fs::remove_file(dir.join("data.json")).unwrap();
    sync(&d, &[]).await;
    assert_eq!(files(&d).await, ["docs/DeployGuide.md", "src/Throttle.ts"]);
    assert!(found(&d, "pelican").await.is_empty());
    assert_eq!(found(&d, "toucan").await, ["docs/DeployGuide.md"]);
    assert_eq!(found(&d, "requests one client").await, ["src/Throttle.ts"]);

    // The same path again is that root, with the options given: code, and only the sources.
    let mut again = spec(&dir.join("src"));
    again.options.code = Some(true);
    let (same, plan) = ingest(&d, again).await;
    assert!(plan.existing && same.root.id == root.root.id && same.root.options.code.is_none());
    let mut again = spec(&dir);
    again.options =
        RootOptions { code: Some(true), include: vec!["src".into()], ..RootOptions::default() };
    let (same, plan) = ingest(&d, again).await;
    assert!(plan.existing && plan.code, "{plan:?}");
    assert_eq!(same.root.options.include, ["src"]);
    assert_eq!(files(&d).await, ["src/Throttle.ts"]);
    let (_, chunks) = item(&d, &dir.join("src/Throttle.ts")).await.unwrap();
    assert_eq!(
        chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChunkKind::Path, ChunkKind::Comment, ChunkKind::Code]
    );
    assert_eq!(found(&d, "export class").await, ["src/Throttle.ts"]);

    // Its folder gone: missing, and what was read of it stays until it is back or removed.
    let aside = canon(&home).join("elsewhere");
    std::fs::rename(&dir, &aside).unwrap();
    sync(&d, &[]).await;
    let r = &roots(&d).await[0];
    assert_eq!((r.root.state, r.files), (RootState::Missing, 1));
    assert!(d.kb_status().await.failing[0].starts_with("handbook: "), "{:?}", d.kb_status().await);
    assert!(d.kb_doctor().await.unwrap().contains("; 1 root (handbook: "));
    std::fs::rename(&aside, &dir).unwrap();
    sync(&d, &[]).await;
    assert_eq!(roots(&d).await[0].root.state, RootState::Ok);

    // Removed: it asks first, then every item of it goes, and the root.
    let e = ask(&d, Request::KbRemove { root: "handbook".into(), confirmed: false, machine: None })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsConfirmation);
    assert!(e.message.contains("1 file(s)") && e.message.contains(ptyro_core::removal::KB_ROOT));
    let gone =
        ask(&d, Request::KbRemove { root: "handbook".into(), confirmed: true, machine: None })
            .await;
    assert!(matches!(gone, Ok(Reply::KbRemoved { items: 1, chunks: 3, .. })), "{gone:?}");
    d.kb_idle().await;
    assert!(roots(&d).await.is_empty() && files(&d).await.is_empty());
    let e = ask(&d, Request::KbRemove { root: "handbook".into(), confirmed: true, machine: None })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn what_is_refused_and_one_file_alone() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    let dir = canon(&home);
    std::fs::create_dir_all(d.paths.data.join("worktrees")).unwrap();
    for (path, why) in [
        (dir.join("nope"), "No such file"),
        (PathBuf::from("relative/path"), "not a whole path"),
        (d.kb_own_dir().join("worktrees"), "ptyro's own folder"),
    ] {
        let e = ask(&d, Request::KbIngest { spec: spec(&path), machine: None }).await.unwrap_err();
        assert!(e.message.contains(why), "{}: {}", path.display(), e.message);
    }
    put(&dir, ".ssh/config", "Host x\n");
    let e = ask(&d, Request::KbIngest { spec: spec(&dir.join(".ssh")), machine: None })
        .await
        .unwrap_err();
    assert!(e.message.contains("never read"), "{}", e.message);
    let mut other = spec(&dir);
    other.profile = Some("nobody".into());
    assert_eq!(
        ask(&d, Request::KbIngest { spec: other, machine: None }).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    // A file by itself is a root; under a name, for one profile, about a project.
    let file = put(&dir, "notes/Plan.md", "# Plan\n\nShip the marmot on Friday.\n");
    let mut one = spec(&file);
    (one.name, one.profile, one.project) =
        (Some("plan".into()), Some("work".into()), Some("feeder".into()));
    let (root, plan) = ingest(&d, one.clone()).await;
    assert_eq!((root.root.kind, plan.files, plan.documents), (RootKind::File, 1, 1));
    assert_eq!(root.project_name.as_deref(), Some("feeder"));
    let (it, _) = item(&d, &file).await.unwrap();
    assert_eq!(
        (it.profile.as_deref(), it.project.as_deref(), it.rel.as_deref()),
        (Some("work"), Some("p1"), Some("Plan.md"))
    );
    // The wall: another profile's agent neither finds it nor sees the root.
    for (profile, sees) in [("work", 1), ("home", 0)] {
        let wall = KbScope { profile: Some(profile.into()), ..KbScope::default() };
        assert_eq!(d.kb_roots(Some(&wall)).await.unwrap().len(), sees, "{profile}");
        let q = ptyro_core::kb::search::KbSearch { query: "marmot".into(), ..Default::default() };
        assert_eq!(d.kb_search(q, Some(&wall)).await.unwrap().hits.len(), sees, "{profile}");
    }
    // A name is one root's; another profile needs the root removed first.
    let other = put(&dir, "notes/Other.md", "# Other\n");
    let mut clash = spec(&other);
    clash.name = Some("plan".into());
    assert_eq!(
        ask(&d, Request::KbIngest { spec: clash, machine: None }).await.unwrap_err().code,
        ErrorCode::Conflict
    );
    one.profile = Some("home".into());
    let e = ask(&d, Request::KbIngest { spec: one, machine: None }).await.unwrap_err();
    assert!(e.message.contains("ptyro kb rm plan"), "{}", e.message);

    // `--once`: read now, and not by the pass that looks at every root; by name it is.
    let mut once = spec(&other);
    once.options.once = true;
    ingest(&d, once).await;
    assert_eq!(sync(&d, &[]).await, 1, "only the plan");
    assert_eq!(sync(&d, &["Other.md"]).await, 1);
    let e =
        ask(&d, Request::KbSync { roots: vec!["nope".into()], machine: None }).await.unwrap_err();
    assert!(e.message.contains("plan"), "{}", e.message);
    // (1.60) Asked of this machine by its word, it is this machine's; a machine that isn't
    // linked is said, with the ones there are.
    let here = ask(&d, Request::KbRoots { machine: Some("here".into()) }).await.unwrap();
    assert!(matches!(here, Reply::KbRoots { roots } if roots.len() == 2), "this machine's own");
    let e = ask(&d, Request::KbRoots { machine: Some("laptop9".into()) }).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("no linked machine is called \"laptop9\""), "{}", e.message);
}

#[tokio::test]
async fn a_repository_by_gits_own_list_with_its_commits() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    let dir = canon(&home).join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    put(&dir, ".gitignore", "target/\n*.log\n");
    put(&dir, "README.md", "# Feeder\n\nIt feeds the capybara.\n");
    put(
        &dir,
        "src/retry.rs",
        "/// Waits longer after every failed try, up to a minute.\nfn backoff() {}\n",
    );
    put(
        &dir,
        "src/again.rs",
        "/// Waits longer after every failed try, up to a minute.\nfn backoff_again() {}\n",
    );
    put(&dir, "target/debug/build.rs", "/// Generated by the build, ignored by git.\nfn x() {}\n");
    put(&dir, "run.log", "ignored\n");
    git(&dir, &["add", "."]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "Feed the capybara",
            "-m",
            "It was hungry: see the narwhal ticket.",
        ],
    );
    put(&dir, "untracked.md", "# Draft\n\nNot added yet, not ignored: the quokka.\n");

    let (root, plan) = ingest(&d, spec(&dir)).await;
    assert!(plan.git && plan.commits, "{plan:?}");
    assert_eq!(
        files(&d).await,
        [".gitignore", "README.md", "src/again.rs", "src/retry.rs", "untracked.md"]
    );
    assert_eq!(found(&d, "quokka").await, ["untracked.md"]);
    assert!(found(&d, "generated by the build").await.is_empty());
    // A comment two files repeat is stored once.
    assert_eq!(found(&d, "waits longer after every failed").await, ["src/again.rs"]);
    // The commit: its message, the files it changed, its time.
    assert_eq!(found(&d, "narwhal").await, ["Feed the capybara"]);
    let db = d.kb_db().unwrap();
    let commits = db
        .list(ItemFilter { kinds: vec![ItemKind::Commit], ..ItemFilter::default() })
        .await
        .unwrap();
    assert_eq!(commits.len(), 1);
    let c = &commits[0];
    assert!(c.uri.starts_with(&format!("commit://{}/", root.root.id)) && c.uri.len() > 60);
    assert_eq!(c.files, [".gitignore", "README.md", "src/again.rs", "src/retry.rs"]);
    assert!(c.started_ms.is_some_and(|t| t > 1_600_000_000_000));
    let chunks = db.chunks(c.id.clone(), 0, 0).await.unwrap();
    assert_eq!(
        chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChunkKind::Message, ChunkKind::Path]
    );
    let listed = &roots(&d).await[0];
    assert_eq!((listed.files, listed.commits), (5, 1));
    let head = listed.root.head.clone().unwrap();
    assert!(c.uri.ends_with(&head));

    // Only the new commits afterwards; a side branch's are not the history.
    git(&dir, &["add", "untracked.md"]);
    git(&dir, &["commit", "-q", "-m", "Add the quokka draft for the wombat"]);
    git(&dir, &["checkout", "-q", "-b", "side"]);
    put(&dir, "side.md", "# Side\n");
    git(&dir, &["add", "side.md"]);
    git(&dir, &["commit", "-q", "-m", "A side branch's okapi"]);
    sync(&d, &["repo"]).await;
    let listed = &roots(&d).await[0];
    assert_eq!((listed.files, listed.commits), (6, 2));
    assert_ne!(listed.root.head.as_deref(), Some(head.as_str()));
    assert!(found(&d, "okapi").await.is_empty());
    assert_eq!(found(&d, "wombat").await, ["Add the quokka draft for the wombat"]);

    // Commits switched off: they go, and come back when it is on again.
    super::super::search::tests::set_config(&d, "[kb]\nprojects = false\ncommits = false\n");
    sync(&d, &[]).await;
    let listed = &roots(&d).await[0];
    assert_eq!((listed.commits, listed.root.head.as_deref()), (0, None));
    super::super::search::tests::set_config(&d, "[kb]\nprojects = false\n");
    sync(&d, &[]).await;
    assert_eq!(roots(&d).await[0].commits, 2);
}

#[tokio::test]
async fn projects_are_roots_by_themselves() {
    let home = tempfile::tempdir().unwrap();
    let t = tempfile::tempdir().unwrap();
    // (The project `feeder` is at the daemon's own folder here: its data directory is in it.)
    let d = daemon(&t, "");
    let project = canon(&t);
    put(&project, "README.md", "# Feeder\n\nIt feeds the axolotl.\n");
    put(
        &project,
        "src/lib.rs",
        "/// Counts what the feeder has left in its hopper.\nfn left() {}\n",
    );
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    let all = roots(&d).await;
    assert_eq!(all.len(), 1);
    let r = &all[0];
    assert_eq!((r.root.name.as_str(), r.root.kind), ("feeder", RootKind::Project));
    assert_eq!((r.root.project.as_deref(), r.root.profile.as_deref()), (Some("p1"), None));
    // Nothing of ptyro's own data directory (`kb.db` is in it).
    assert_eq!(files(&d).await, ["README.md", "src/lib.rs"]);
    assert_eq!(found(&d, "axolotl").await, ["README.md"]);
    let (it, _) = item(&d, &project.join("README.md")).await.unwrap();
    assert_eq!(it.project.as_deref(), Some("p1"));
    // Again: nothing new. Not removed by hand, and a path inside it is that root.
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.len(), 1);
    let e = ask(&d, Request::KbRemove { root: "feeder".into(), confirmed: true, machine: None })
        .await
        .unwrap_err();
    assert!(e.message.contains("kb.projects"), "{}", e.message);
    let (same, plan) = ingest(&d, spec(&project.join("src"))).await;
    assert!(plan.existing && same.root.id == r.root.id);

    // The project's own file: its switches narrow, and `code` may be switched on.
    put_later(
        &project,
        ".ptyro.toml",
        "[knowledge]\ncomments = false\ncode = true\nexclude = ['README.md']\n",
    );
    sync(&d, &[]).await;
    assert_eq!(files(&d).await, [".ptyro.toml", "src/lib.rs"]);
    let (_, chunks) = item(&d, &project.join("src/lib.rs")).await.unwrap();
    assert_eq!(
        chunks.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ChunkKind::Path, ChunkKind::Code]
    );

    // Pinned to a profile: read again as that profile's.
    d.registry.locked().projects[0].pin = Some("work".into());
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    let all = roots(&d).await;
    assert_eq!((all.len(), all[0].root.profile.as_deref()), (1, Some("work")));
    assert_ne!(all[0].root.id, r.root.id);
    let (it, _) = item(&d, &project.join("src/lib.rs")).await.unwrap();
    assert_eq!(it.profile.as_deref(), Some("work"));

    // A folder ingested by hand stays whatever the projects do.
    let dir = canon(&home).join("docs");
    put(&dir, "a.md", "# A\n\nthe ibis\n");
    ingest(&d, spec(&dir)).await;
    // Kept out by its own file: the root goes, with what was read.
    put_later(&project, ".ptyro.toml", "[knowledge]\nenabled = false\n");
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.iter().map(|r| r.root.name.as_str()).collect::<Vec<_>>(), ["docs"]);
    assert_eq!(files(&d).await, ["a.md"]);
    // Back, then `kb.projects` off: gone again.
    std::fs::remove_file(project.join(".ptyro.toml")).unwrap();
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.len(), 2);
    super::super::search::tests::set_config(&d, "[kb]\nprojects = false\n");
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.len(), 1);
    // The project removed from ptyro: its root goes too.
    super::super::search::tests::set_config(&d, "");
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.len(), 2);
    d.registry.locked().projects.clear();
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(roots(&d).await.len(), 1);
}

#[tokio::test]
async fn a_pass_that_was_interrupted_is_carried_on_and_a_turn_has_a_project_looked_at() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    let dir = canon(&home).join("half");
    for n in 0..40 {
        put(&dir, &format!("doc{n}.md"), format!("# Doc {n}\n\nthe gecko number {n}\n"));
    }
    // As a restart leaves it: the root is there and `syncing`, some of its files are read.
    let db = d.kb_db().unwrap();
    let root = KbRoot {
        id: new_id(),
        name: "half".into(),
        kind: RootKind::Folder,
        path: dir.to_string_lossy().into_owned(),
        project: Some("p1".into()),
        profile: None,
        options: RootOptions::default(),
        head: None,
        state: RootState::Syncing,
        error: None,
        added_ms: Some(1),
        synced_ms: Some(1),
    };
    db.put_root(root.clone()).await.unwrap();
    d.kb_roots_resume().await;
    d.kb_idle().await;
    assert_eq!(files(&d).await.len(), 40);
    assert_eq!(roots(&d).await[0].root.state, RootState::Ok);
    // Nothing is read twice: a second look writes nothing.
    let before = item(&d, &dir.join("doc7.md")).await.unwrap().0.ingested_ms;
    d.kb_roots_resume().await;
    d.kb_idle().await;
    assert_eq!(item(&d, &dir.join("doc7.md")).await.unwrap().0.ingested_ms, before);

    // A turn's end in the project: looked at once, and not again within five minutes.
    put(&dir, "new.md", "# New\n\nthe agent just wrote the tapir\n");
    d.kb_root_turn(Some("p1"));
    d.kb_roots_turned().await;
    d.kb_idle().await;
    assert_eq!(found(&d, "tapir").await, ["new.md"]);
    put(&dir, "newer.md", "# Newer\n\nthe agent then wrote the vole\n");
    d.kb_root_turn(Some("p1"));
    d.kb_root_turn(Some("another"));
    assert_eq!(*d.kb.roots.turns.locked(), ["another"]);

    // Off: nothing is read, and no pass is asked for.
    super::super::search::tests::set_config(&d, "[kb]\nenabled = false\n");
    assert!(d.kb_roots_sync(&[]).await.is_err());
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
}

#[test]
fn a_listing_takes_what_the_rules_take_up_to_the_cap() {
    let t = tempfile::tempdir().unwrap();
    let dir = ptyro_core::os::canonicalize(t.path()).unwrap();
    for rel in ["a.md", "b.md", "docs/c.md", "docs/d.txt", "src/e.rs", "own/f.md", ".git/config"] {
        put(&dir, rel, "x");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hosts", dir.join("link.md")).unwrap();
    let root = KbRoot {
        id: "r".into(),
        name: "r".into(),
        kind: RootKind::Folder,
        path: dir.to_string_lossy().into_owned(),
        project: None,
        profile: None,
        options: RootOptions::default(),
        head: None,
        state: RootState::Ok,
        error: None,
        added_ms: None,
        synced_ms: None,
    };
    let opts = |include: &[&str], exclude: &[&str]| Opts {
        tiers: Tiers { comments: true, code: false },
        commits: false,
        include: if include.is_empty() {
            Vec::new()
        } else {
            vec![Never::only(&include.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())]
        },
        exclude: Never::only(&exclude.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()),
        off: false,
    };
    let never = Never::default();
    let rels = |o: &Opts, cap: usize| {
        let l = list(&root, o, &never, &[dir.join("own")], cap).unwrap();
        (l.files.iter().map(|f| f.rel.clone()).collect::<Vec<_>>(), l.total)
    };
    assert_eq!(
        rels(&opts(&[], &[]), 100),
        (
            vec![
                "a.md".into(),
                "b.md".into(),
                "docs/c.md".into(),
                "docs/d.txt".into(),
                "src/e.rs".into()
            ],
            5
        )
    );
    assert_eq!(rels(&opts(&["*.md"], &["docs"]), 100).0, ["a.md", "b.md"]);
    assert_eq!(rels(&opts(&["docs/**"], &[]), 100).0, ["docs/c.md", "docs/d.txt"]);
    // Past the cap: the first ones, and how many there are.
    let (first, total) = rels(&opts(&["*.md"], &[]), 2);
    assert_eq!(first, ["a.md", "b.md"]);
    assert!(total >= 3);
    assert!(
        list(
            &KbRoot { path: "/nonexistent/ptyro".into(), ..root.clone() },
            &opts(&[], &[]),
            &never,
            &[],
            9
        )
        .is_err()
    );
}

/// E3: roots inside one another. The innermost root owns a file, so a project pinned to a
/// profile is behind that profile's wall although a folder around it is read for everyone;
/// and a project that keeps itself out is in no root.
#[tokio::test]
async fn the_innermost_root_owns_a_file_and_a_project_kept_out_is_in_no_root() {
    use super::super::search::tests::set_config;
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let work = canon(&home).join("work");
    let acme = work.join("acme");
    put(&work, "notes.md", "# Notes\n\nthe marmot's own\n");
    put(&acme, "README.md", "# Acme\n\nthe pangolin contract\n");
    put(&acme, "src/lib.rs", "/// Bills the pangolin by the hour.\nfn bill() {}\n");
    let registry = ptyro_store::Registry {
        projects: vec![ptyro_core::model::ProjectInfo {
            id: "p1".into(),
            machine: None,
            slug: "acme".into(),
            root: acme.clone(),
            default_branch: None,
            remote_url: None,
            pin: Some("acme".into()),
            created_ms: 0,
            last_used_ms: None,
            moved_to: None,
        }],
        ..ptyro_store::Registry::default()
    };
    let d = crate::daemon::test_daemon(t.path(), registry);
    const PROFILE: &str = "[profiles.acme]\nlabel = \"acme\"\n";
    set_config(&d, &format!("[kb]\nprojects = false\n{PROFILE}"));
    let whose = async |path: &Path| {
        let (it, _) = item(&d, path).await?;
        let root = d.kb_db().unwrap().root(it.root.clone()?).await.unwrap()?;
        Some((root.name, it.profile, it.id))
    };

    // The folder around it, read for every profile, before the project is a root.
    ingest(&d, spec(&work)).await;
    assert_eq!(files(&d).await, ["acme/README.md", "acme/src/lib.rs", "notes.md"]);
    let (name, profile, _) = whose(&acme.join("README.md")).await.unwrap();
    assert_eq!((name.as_str(), profile), ("work", None));

    // The project is a root of its own: its files are its, behind its profile's wall.
    set_config(&d, PROFILE);
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    let (name, profile, id) = whose(&acme.join("README.md")).await.unwrap();
    assert_eq!((name.as_str(), profile.as_deref()), ("acme", Some("acme")));
    let (name, profile, _) = whose(&work.join("notes.md")).await.unwrap();
    assert_eq!((name.as_str(), profile), ("work", None));
    let counts: Vec<(String, u64)> =
        roots(&d).await.into_iter().map(|r| (r.root.name, r.files)).collect();
    assert_eq!(counts, [("acme".to_owned(), 2), ("work".to_owned(), 1)]);
    // And they stay its: another pass over both changes nothing and reads nothing again.
    sync(&d, &[]).await;
    sync(&d, &[]).await;
    assert_eq!(whose(&acme.join("README.md")).await.unwrap().2, id);
    let (name, profile, _) = whose(&acme.join("src/lib.rs")).await.unwrap();
    assert_eq!((name.as_str(), profile.as_deref()), ("acme", Some("acme")));

    // It keeps itself out: its root goes, and the folder around it does not read it either.
    put_later(&acme, ".ptyro.toml", "[knowledge]\nenabled = false\n");
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    sync(&d, &[]).await;
    assert_eq!(files(&d).await, ["notes.md"]);
    assert!(found(&d, "pangolin").await.is_empty());

    // No longer kept out, then no longer a project: the folder around it reads it again.
    std::fs::remove_file(acme.join(".ptyro.toml")).unwrap();
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    assert_eq!(whose(&acme.join("README.md")).await.unwrap().0, "acme");
    d.registry.locked().projects.clear();
    d.kb_roots_reconcile().await;
    d.kb_idle().await;
    let (name, profile, _) = whose(&acme.join("README.md")).await.unwrap();
    assert_eq!((name.as_str(), profile), ("work", None));
    assert_eq!(files(&d).await, ["acme/README.md", "acme/src/lib.rs", "notes.md"]);
}

/// C3, E2: what you forgot under a root stays forgotten when the root is removed and its
/// folder ingested again (a file by its path, a commit by its repository's).
#[tokio::test]
async fn what_was_forgotten_under_a_root_stays_forgotten_when_it_is_read_again() {
    let t = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let d = daemon(&t, "[kb]\nprojects = false\n");
    let dir = canon(&home).join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    put(&dir, "README.md", "# Feeder\n\nIt feeds the capybara.\n");
    put(&dir, "PLAN.md", "# Plan\n\nThe tapir comes next.\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Feed the capybara and the narwhal"]);
    put(&dir, "MORE.md", "# More\n\nThe dugong.\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Add the dugong"]);
    ingest(&d, spec(&dir)).await;
    assert_eq!(found(&d, "narwhal").await, ["Feed the capybara and the narwhal"]);

    let db = d.kb_db().unwrap();
    let (readme, _) = item(&d, &dir.join("README.md")).await.unwrap();
    let commits = db
        .list(ItemFilter { kinds: vec![ItemKind::Commit], ..ItemFilter::default() })
        .await
        .unwrap();
    let narwhal =
        commits.iter().find(|c| c.title.as_deref().is_some_and(|t| t.contains("narwhal")));
    db.forget(readme.id).await.unwrap();
    db.forget(narwhal.unwrap().id.clone()).await.unwrap();

    for _ in 0..2 {
        ask(&d, Request::KbRemove { root: "repo".into(), confirmed: true, machine: None })
            .await
            .unwrap();
        d.kb_idle().await;
        assert!(roots(&d).await.is_empty());
        ingest(&d, spec(&dir)).await;
        assert_eq!(files(&d).await, ["MORE.md", "PLAN.md"]);
        assert!(found(&d, "capybara").await.is_empty());
        assert!(found(&d, "narwhal").await.is_empty());
        assert_eq!(found(&d, "dugong").await.len(), 2, "the file and the other commit are back");
        assert_eq!(db.status().await.unwrap().forgotten, 2);
    }
}

/// Review B4: a failing root's line carries the command that mends it, and the root is one
/// word of that command whatever it is called (the clients run it as it stands).
#[test]
fn a_failing_roots_line_names_it_in_one_word() {
    let root = |name: &str, state, error: Option<&str>| KbRoot {
        id: "01ROOT".into(),
        name: name.into(),
        kind: RootKind::Folder,
        path: "/src/x".into(),
        project: None,
        profile: None,
        options: RootOptions::default(),
        head: None,
        state,
        error: error.map(str::to_owned),
        added_ms: None,
        synced_ms: None,
    };
    let lines = failing(&[
        root("docs", RootState::Ok, None),
        root("docs", RootState::Missing, None),
        root("x -y", RootState::Missing, None),
        root("my docs", RootState::Error, Some("no")),
        root("--yes", RootState::Error, None),
        root("own", RootState::Error, Some("locked (`ptyro doctor` says more)")),
    ]);
    assert_eq!(
        lines,
        [
            "docs: /src/x is gone (`ptyro kb rm docs` drops what was kept of it)",
            "x -y: /src/x is gone (`ptyro kb rm 'x -y'` drops what was kept of it)",
            "my docs: no (`ptyro kb sync 'my docs'` reads it again)",
            "--yes: it could not be read (`ptyro kb sync 01ROOT` reads it again)",
            "own: locked (`ptyro doctor` says more)",
        ]
    );
}
