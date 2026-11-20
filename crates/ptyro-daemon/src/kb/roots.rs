//! Projects and folders in the knowledge base (SPEC §25.6.3): the roots, and the pass that
//! makes `kb.db` agree with what is on disk under each.
//!
//! **A root** is a folder or a file that was given to `ptyro ingest`, or a project ptyro
//! knows (while `kb.projects` is on: [`Daemon::kb_roots_reconcile`] makes and removes those).
//! It is the checkout at that path, never a project's worktrees.
//!
//! **A pass** ([`Daemon::kb_roots_kick`]) lists the root's files (git's own list in a
//! repository, a walk elsewhere), stats them, and reads only those whose time or size is not
//! what their item's cursor says: each through the gate and the one way in, as an item of
//! kind `file` with its path, its document text or its comments, and its code when that is
//! switched on. An item whose file is gone is deleted. Then the repository's commits: the
//! newest 5,000 of its main branch at first, and after that only the new ones.
//!
//! **The innermost root owns a file.** Roots may lie inside one another (`ptyro ingest
//! ~/work`, and a project in it pinned to a profile). A pass leaves alone every folder inside
//! its root that is another root's, or a project's that keeps itself out
//! ([`Daemon::kb_root_not`]): a file is one root's, stamped with that root's profile, which
//! is the wall it is behind. A root made or removed inside another has that one looked at
//! again, so what it kept there changes hands.
//!
//! **One pass at a time, on a task of its own**, never on the ingest queue: a folder of
//! fifty thousand files is read between the sessions' turns, not before them. A pass that a
//! restart interrupts leaves its root `syncing`, and the next start carries on: what was read
//! is not read again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ptyro_core::kb::file::{self, Tiers};
use ptyro_core::kb::ingest::{Never, Skip, Source, Stamp};
use ptyro_core::kb::{
    ItemKind, KbPlan, KbRoot, KbRootInfo, RootKind, RootOptions, RootSpec, RootState, Writer,
};
use ptyro_core::model::new_id;
use ptyro_core::project::ProjectConfig;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_git::Git;
use ptyro_protocol::{DomainEvent, Reply};
use ptyro_store::kb::KbDb;
use serde_json::{Value, json};

use super::search::KbScope;
use super::{Facts, Prepared, Refused, store_err};
use crate::daemon::Daemon;

/// The most files a root takes (§25.6.3); past that it says so and `--include` narrows it.
pub(crate) const MAX_FILES: usize = 50_000;
/// A walk stops looking after this many files: the count it reports is then "at least".
const WALK_MAX: usize = 500_000;
/// The commits read at a root's first pass.
const COMMITS_FIRST: usize = 5_000;
/// A root is looked at again when an agent's turn ends in its project, at most this often.
const TURN_GAP: Duration = Duration::from_secs(300);
/// After the daemon's start, before the roots are looked at (so the start stays fast).
const START_DELAY: Duration = Duration::from_secs(60);
/// How often a pass says how far it is, and that something new can be found.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const NEWS_EVERY: Duration = Duration::from_secs(2);
/// Folders a walk outside a repository never enters, beside the hidden ones.
const SKIP_DIRS: [&str; 6] = ["node_modules", "target", "dist", "build", "vendor", ".venv"];
/// How a file is read, in its cursor: a change of it reads every file again.
const READER: u32 = 1;

/// The roots' part of the daemon's knowledge base.
#[derive(Default)]
pub(super) struct Roots {
    /// One pass or removal at a time.
    one: tokio::sync::Mutex<()>,
    /// Roots waiting for a pass or in one, by id.
    passes: Mutex<HashMap<String, Pass>>,
    /// Roots being removed.
    removing: Mutex<HashSet<String>>,
    /// Files that were not kept, each as it was then (its cursor): not read again until it
    /// changes.
    refused: Mutex<HashMap<String, Value>>,
    /// Per root, each comment block's hash and the file that holds it: a block that another
    /// file of the root repeats is stored once.
    repeats: Mutex<HashMap<String, HashMap<u64, u64>>>,
    /// When a turn's end last had each project looked at.
    turned: Mutex<HashMap<String, Instant>>,
    /// Projects a turn just ended in, for the roots' task to look at.
    turns: Mutex<Vec<String>>,
    wake: tokio::sync::Notify,
}

#[derive(Default, Clone, Copy)]
struct Pass {
    done: u64,
    of: u64,
    running: bool,
    /// Asked for again while it ran: what changed meanwhile is read by one more pass.
    again: bool,
}

/// One file of a root, as the disk has it.
struct Found {
    rel: String,
    path: PathBuf,
    uri: String,
    mtime_ms: u64,
    size: u64,
}

struct Listing {
    files: Vec<Found>,
    /// How many files the root holds (more than `files` past the cap).
    total: usize,
    git: bool,
}

/// What is read of a root: the user's settings, the root's own options, its project's file.
struct Opts {
    tiers: Tiers,
    commits: bool,
    /// Only files that match each of these lists are read (the root's, its project's).
    include: Vec<Never>,
    exclude: Never,
    /// Its project keeps itself out (`[knowledge] enabled = false`).
    off: bool,
}

impl Opts {
    fn cursor(&self, f: &Found) -> Value {
        json!({
            "mtime": f.mtime_ms,
            "size": f.size,
            "read": format!("{READER}{}{}", u8::from(self.tiers.comments), u8::from(self.tiers.code)),
        })
    }

    fn takes(&self, rel: &str, never: &Never) -> bool {
        let rel = Path::new(rel);
        !never.is_never(rel)
            && !self.exclude.is_never(rel)
            && self.include.iter().all(|i| i.is_never(rel))
    }
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// `file://` and the path.
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy().replace('\\', "/"))
}

/// The files under `dir`, by a walk: no hidden folder, none of [`SKIP_DIRS`], no link.
fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(dir.join(&rel)) else { continue };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if kind.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                    todo.push(rel.join(name));
                }
            } else if kind.is_file() {
                out.push(rel.join(name).to_string_lossy().replace('\\', "/"));
                if out.len() >= WALK_MAX {
                    return out;
                }
            }
        }
    }
    out
}

/// What a root holds now: its files that the rules take (at most `cap`), each with its time
/// and size. Nothing under one of `not` (ptyro's data directory, another root inside this
/// one, a project that keeps itself out) is ever one of them.
fn list(
    root: &KbRoot,
    opts: &Opts,
    never: &Never,
    not: &[PathBuf],
    cap: usize,
) -> Result<Listing, String> {
    let path = Path::new(&root.path);
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", root.path))?;
    let (dir, rels, git) = if meta.is_file() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (path.parent().unwrap_or(Path::new("/")).to_owned(), vec![name], false)
    } else {
        let git = Git::new();
        // In a repository, git's own list: tracked, and untracked that nothing ignores. (A
        // folder git lists nothing of is ignored there, or empty: walked.)
        match git.toplevel(path).and_then(|_| git.ls_files(path)) {
            Ok(rels) if !rels.is_empty() => (path.to_owned(), rels, true),
            _ => (path.to_owned(), walk(path), false),
        }
    };
    let mut rels: Vec<String> = rels
        .into_iter()
        .filter(|r| opts.takes(r, never))
        .filter(|r| {
            let path = dir.join(r);
            !not.iter().any(|n| path.starts_with(n))
        })
        .collect();
    rels.sort_unstable();
    rels.dedup();
    let total = rels.len();
    rels.truncate(cap);
    let files = rels
        .into_iter()
        .filter_map(|rel| {
            let path = dir.join(&rel);
            // (Not through a link: what it points at may be outside the root.)
            let m = std::fs::symlink_metadata(&path).ok().filter(|m| m.is_file())?;
            let mtime_ms = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
            Some(Found { uri: file_uri(&path), rel, path, mtime_ms, size: m.len() })
        })
        .collect();
    Ok(Listing { files, total, git })
}

/// What reading one file gave.
enum Read {
    Source(Box<Source>),
    /// Not text.
    Binary,
    /// It went, or can't be read, between the listing and now.
    Gone,
}

fn read(f: &Found, tiers: Tiers, cursor: Value, repeats: &mut HashMap<u64, u64>) -> Read {
    let Ok(bytes) = std::fs::read(&f.path) else { return Read::Gone };
    if file::is_binary(&bytes) {
        return Read::Binary;
    }
    let text = String::from_utf8_lossy(&bytes);
    let holder = hash(&f.uri);
    let mut source = file::source(f.uri.clone(), &f.rel, &text, f.mtime_ms, tiers, |block| {
        match repeats.get(&hash(block)) {
            Some(other) if *other != holder => true,
            _ => {
                repeats.insert(hash(block), holder);
                false
            }
        }
    });
    source.cursor = Some(cursor);
    Read::Source(Box::new(source))
}

/// `ptyro doctor`'s words about the roots: how many, and each that is missing or failing.
pub(super) fn doctor(roots: &[KbRoot]) -> String {
    if roots.is_empty() {
        return String::new();
    }
    let bad: Vec<String> = failing(roots);
    let n = roots.len();
    let mut out = format!("; {n} root{}", if n == 1 { "" } else { "s" });
    if !bad.is_empty() {
        out.push_str(&format!(" ({})", bad.join("; ")));
    }
    out
}

/// Each root that is missing or failing, in a line, with the command that mends it. The
/// clients run that command as it stands, so the root is one word of it whatever it is
/// called (`ptyro_core::kb::root_word`).
pub(super) fn failing(roots: &[KbRoot]) -> Vec<String> {
    roots
        .iter()
        .filter(|r| matches!(r.state, RootState::Missing | RootState::Error))
        .map(|r| {
            let word = ptyro_core::kb::root_word(&r.name, &r.id);
            let again = format!("(`ptyro kb sync {word}` reads it again)");
            match (&r.state, &r.error) {
                (RootState::Missing, _) => format!(
                    "{}: {} is gone (`ptyro kb rm {word}` drops what was kept of it)",
                    r.name, r.path
                ),
                // (An error that names its own way through keeps it.)
                (_, Some(e)) if e.contains("`ptyro ") => format!("{}: {e}", r.name),
                (_, Some(e)) => format!("{}: {e} {again}", r.name),
                _ => format!("{}: it could not be read {again}", r.name),
            }
        })
        .collect()
}

fn blocking(e: tokio::task::JoinError) -> Error {
    Error::internal(e.to_string())
}

impl Daemon {
    // ── when ─────────────────────────────────────────────────────────────────────────────────

    /// At the daemon's start: make the projects' roots, carry on with any pass a restart
    /// interrupted, look at every root after a minute and then every `kb.sync_every`, and at
    /// once whenever the projects or the settings that say what is read change.
    pub(crate) fn start_kb_roots(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut events = me.events.subscribe();
            let read_by = |d: &Daemon| {
                let kb = d.config().kb;
                (kb.enabled, kb.projects, kb.comments, kb.code, kb.commits, kb.exclude, kb.max_file)
            };
            let mut was = read_by(&me);
            me.kb_roots_reconcile().await;
            me.kb_roots_resume().await;
            let mut next = tokio::time::Instant::now() + START_DELAY;
            loop {
                tokio::select! {
                    () = me.kb.roots.wake.notified() => me.kb_roots_turned().await,
                    () = tokio::time::sleep_until(next) => {
                        me.kb_roots_reconcile().await;
                        me.kb_roots_sync(&[]).await.ok();
                        next = tokio::time::Instant::now() + me.config().kb.sync_every();
                    }
                    e = events.recv() => match e {
                        Ok(DomainEvent::ConfigReloaded { .. }) => {
                            me.kb_roots_reconcile().await;
                            let now = read_by(&me);
                            if now != was {
                                was = now;
                                me.kb_roots_sync(&[]).await.ok();
                            }
                        }
                        Ok(
                            DomainEvent::ProjectUpdated { .. }
                            | DomainEvent::ProjectRemoved { .. }
                            | DomainEvent::ProjectAutoAdded { .. },
                        ) => me.kb_roots_reconcile().await,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        _ => {}
                    },
                }
            }
        });
    }

    /// An agent's turn ended in `project`: its roots are looked at again (a document the
    /// agent just wrote becomes findable), at most once in five minutes.
    pub(crate) fn kb_root_turn(&self, project: Option<&str>) {
        let Some(project) = project else { return };
        if !self.config().kb.enabled {
            return;
        }
        {
            let mut turned = self.kb.roots.turned.locked();
            if turned.get(project).is_some_and(|at| at.elapsed() < TURN_GAP) {
                return;
            }
            turned.insert(project.to_owned(), Instant::now());
        }
        self.kb.roots.turns.locked().push(project.to_owned());
        self.kb.roots.wake.notify_one();
    }

    /// The roots of the projects a turn just ended in: a pass over each.
    async fn kb_roots_turned(self: &Arc<Self>) {
        let projects = std::mem::take(&mut *self.kb.roots.turns.locked());
        let Ok(db) = self.kb_db() else { return };
        let Ok(roots) = db.roots().await else { return };
        let ids = roots
            .into_iter()
            .filter(|r| r.project.as_ref().is_some_and(|p| projects.contains(p)))
            .filter(|r| !r.options.once)
            .map(|r| r.id)
            .collect();
        self.kb_roots_kick(ids);
    }

    /// Roots a restart left in the middle of a pass, or that were never read: now.
    async fn kb_roots_resume(self: &Arc<Self>) {
        let Ok(db) = self.kb_db() else { return };
        let Ok(roots) = db.roots().await else { return };
        let ids = roots
            .into_iter()
            .filter(|r| r.state == RootState::Syncing || r.synced_ms.is_none())
            .map(|r| r.id)
            .collect();
        self.kb_roots_kick(ids);
    }

    /// `KbSync`'s part for roots: look again at those named (by name, id or path), or at
    /// every one. A root read `--once` is looked at again only when it is named. Returns how
    /// many passes were asked for.
    pub(crate) async fn kb_roots_sync(self: &Arc<Self>, named: &[String]) -> Result<u32, Error> {
        let db = self.kb_db()?;
        let ids: Vec<String> = if named.is_empty() {
            let roots = db.roots().await.map_err(store_err)?;
            roots
                .into_iter()
                .filter(|r| !(r.options.once && r.synced_ms.is_some()))
                .map(|r| r.id)
                .collect()
        } else {
            let mut ids = Vec::new();
            for name in named {
                ids.push(self.kb_root(&db, name).await?.id);
            }
            ids
        };
        Ok(self.kb_roots_kick(ids))
    }

    /// Ask for a pass over each of these roots, and return at once: how many were asked for
    /// (one that waits already is not asked for twice; one that is being read is read once
    /// more after).
    pub(crate) fn kb_roots_kick(self: &Arc<Self>, ids: Vec<String>) -> u32 {
        let mut n = 0;
        for id in ids {
            if self.kb.roots.removing.locked().contains(&id) {
                continue;
            }
            n += 1;
            {
                let mut passes = self.kb.roots.passes.locked();
                if let Some(p) = passes.get_mut(&id) {
                    p.again |= p.running;
                    continue;
                }
                passes.insert(id.clone(), Pass::default());
            }
            // (Counted as pending while it waits and runs, so that idle means read.)
            self.kb.pending.send_modify(|n| *n += 1);
            let me = Arc::clone(self);
            tokio::spawn(async move {
                let one = me.kb.roots.one.lock().await;
                loop {
                    if let Some(p) = me.kb.roots.passes.locked().get_mut(&id) {
                        *p = Pass { running: true, ..Pass::default() };
                    }
                    me.kb_root_pass(&id).await;
                    let again = me.kb.roots.passes.locked().get(&id).is_some_and(|p| p.again);
                    if !again {
                        break;
                    }
                }
                me.kb.roots.passes.locked().remove(&id);
                drop(one);
                me.kb.done();
            });
        }
        n
    }

    // ── the projects' own roots ──────────────────────────────────────────────────────────────

    /// Make the roots agree with the projects (§25.6.3): every project of this machine is a
    /// root while `kb.projects` is on and its own file doesn't keep it out, under the profile
    /// it is pinned to; a project that is gone, or kept out, loses its root and what was read
    /// of it. Only a project's own checkout: never a worktree.
    pub(crate) async fn kb_roots_reconcile(self: &Arc<Self>) {
        let config = self.config();
        if !config.kb.enabled {
            return;
        }
        let Ok(db) = self.kb_db() else { return };
        let Ok(roots) = db.roots().await else { return };
        let projects: Vec<_> = {
            let reg = self.registry.locked();
            reg.projects.iter().filter(|p| p.machine.is_none()).cloned().collect()
        };
        let never = self.kb_rules();
        let wanted: Vec<_> = projects
            .into_iter()
            .filter(|_| config.kb.projects)
            .filter(|p| !self.kb_project_off(Some(&p.id), None))
            .map(|p| {
                let path = ptyro_core::os::canonicalize(&p.root).unwrap_or_else(|_| p.root.clone());
                (p, path.to_string_lossy().into_owned())
            })
            .filter(|(_, path)| !never.never.is_never(Path::new(path)))
            .collect();
        let removing = self.kb.roots.removing.locked().clone();
        let mut names: HashSet<String> = roots.iter().map(|r| r.name.clone()).collect();
        let mut made = Vec::new();
        for (p, path) in &wanted {
            match roots.iter().find(|r| &r.path == path) {
                // A folder that was ingested by hand and is a project now: it says so.
                Some(r) if r.project.is_none() && !removing.contains(&r.id) => {
                    let root = KbRoot { project: Some(p.id.clone()), ..r.clone() };
                    let _ = db.put_root(root).await;
                }
                Some(_) => {}
                None => {
                    let mut name = p.slug.clone();
                    let mut k = 1;
                    while names.contains(&name) {
                        k += 1;
                        name = format!("{}-{k}", p.slug);
                    }
                    names.insert(name.clone());
                    let root = KbRoot {
                        id: new_id(),
                        name,
                        kind: RootKind::Project,
                        path: path.clone(),
                        project: Some(p.id.clone()),
                        profile: p.pin.clone(),
                        options: RootOptions::default(),
                        head: None,
                        state: RootState::Syncing,
                        error: None,
                        added_ms: Some(now_ms()),
                        synced_ms: None,
                    };
                    match db.put_root(root.clone()).await {
                        Ok(()) => {
                            // (Those around it first: what they kept of it is this one's.)
                            made.extend(Self::kb_roots_around(&roots, &root.path));
                            made.push(root.id);
                        }
                        Err(e) => tracing::warn!(
                            "knowledge base: project {} is not read: {}",
                            p.slug,
                            store_err(e).message
                        ),
                    }
                }
            }
        }
        // A project's root whose project is gone, kept out, moved, or pinned to another
        // profile (its items carry the old one: it is read again under the new).
        for r in roots.iter().filter(|r| r.kind == RootKind::Project) {
            let stands = wanted.iter().any(|(p, path)| {
                r.project.as_deref() == Some(&p.id) && &r.path == path && r.profile == p.pin
            });
            if !stands && !removing.contains(&r.id) {
                self.kb_root_drop(r.clone());
            }
        }
        if !made.is_empty() {
            self.kb_roots_kick(made);
        }
    }

    // ── one pass ─────────────────────────────────────────────────────────────────────────────

    /// ptyro's own data directory, as paths under it resolve: nothing in it is ever read
    /// (its worktrees are checkouts of projects that are read where they are).
    fn kb_own_dir(&self) -> PathBuf {
        ptyro_core::os::canonicalize(&self.paths.data).unwrap_or_else(|_| self.paths.data.clone())
    }

    /// What a pass over `root` leaves alone: ptyro's own data directory, and every folder
    /// (or file) inside the root that is another root's, or the checkout of a project that
    /// keeps itself out (`[knowledge] enabled = false`). The innermost root owns a file.
    async fn kb_root_not(&self, db: &KbDb, root: &KbRoot) -> Result<Vec<PathBuf>, Error> {
        let here = Path::new(&root.path);
        let mut inner: Vec<PathBuf> = db
            .roots()
            .await
            .map_err(store_err)?
            .into_iter()
            .filter(|r| r.id != root.id)
            .map(|r| PathBuf::from(r.path))
            .collect();
        let projects: Vec<_> = {
            let reg = self.registry.locked();
            reg.projects.iter().filter(|p| p.machine.is_none()).cloned().collect()
        };
        for p in projects {
            if self.kb_project_off(Some(&p.id), None) {
                inner.push(ptyro_core::os::canonicalize(&p.root).unwrap_or(p.root));
            }
        }
        inner.retain(|p| p != here && ptyro_core::os::path_starts_with(p, here));
        inner.push(self.kb_own_dir());
        Ok(inner)
    }

    /// The roots that `path` lies inside (their ids): what they kept under it changes hands
    /// when a root is made or removed there.
    fn kb_roots_around(roots: &[KbRoot], path: &str) -> Vec<String> {
        let path = Path::new(path);
        roots
            .iter()
            .filter(|r| {
                let outer = Path::new(&r.path);
                outer != path && ptyro_core::os::path_starts_with(path, outer)
            })
            .map(|r| r.id.clone())
            .collect()
    }

    /// What is read of `root`: your settings, its own options, and its project's file (which
    /// can only narrow, except `code`, which a repository may switch on for itself).
    fn kb_root_opts(&self, root: &KbRoot) -> Opts {
        let kb = self.config().kb;
        let project = root
            .project
            .as_deref()
            .and_then(|id| self.project_by_id(id))
            .and_then(|p| ProjectConfig::load(&p.root, &p.root).ok())
            .map(|c| c.knowledge)
            .unwrap_or_default();
        let o = &root.options;
        let mut include = Vec::new();
        for globs in [&o.include, &project.include] {
            if !globs.is_empty() {
                include.push(Never::only(globs));
            }
        }
        let exclude: Vec<String> = o.exclude.iter().chain(&project.exclude).cloned().collect();
        Opts {
            tiers: Tiers {
                comments: o.comments.unwrap_or(kb.comments) && project.comments != Some(false),
                code: o.code.or(project.code).unwrap_or(kb.code),
            },
            commits: o.commits.unwrap_or(kb.commits) && project.commits != Some(false),
            include,
            exclude: Never::only(&exclude),
            off: project.enabled == Some(false),
        }
    }

    fn kb_root_progress(&self, root: &KbRoot, done: u64, of: u64) {
        if let Some(p) = self.kb.roots.passes.locked().get_mut(&root.id) {
            (p.done, p.of) = (done, of);
        }
        let _ = self.events.send(DomainEvent::KbProgress {
            root: root.id.clone(),
            name: root.name.clone(),
            done,
            of,
        });
    }

    /// One pass over a root; what goes wrong is the root's state.
    async fn kb_root_pass(self: &Arc<Self>, id: &str) {
        let Ok(db) = self.kb_db() else { return };
        let Ok(Some(root)) = db.root(id.to_owned()).await else { return };
        if let Err(e) = self.kb_root_read(&db, &root).await
            && e.code != ErrorCode::Unsupported
        {
            tracing::warn!("knowledge base: {}: {}", root.name, e.message);
            let _ =
                db.set_root_state(root.id.clone(), RootState::Error, Some(e.message), None).await;
        }
    }

    async fn kb_root_read(self: &Arc<Self>, db: &KbDb, root: &KbRoot) -> Result<(), Error> {
        let path = PathBuf::from(&root.path);
        if !path.exists() {
            // Its items stay until it is back or removed.
            let why = format!("{} is gone", root.path);
            db.set_root_state(root.id.clone(), RootState::Missing, Some(why), None)
                .await
                .map_err(store_err)?;
            return Ok(());
        }
        let opts = Arc::new(self.kb_root_opts(root));
        let rules = self.kb_rules();
        let listing = if opts.off {
            Listing { files: Vec::new(), total: 0, git: false }
        } else {
            let not = self.kb_root_not(db, root).await?;
            let (root, opts, rules) = (root.clone(), Arc::clone(&opts), Arc::clone(&rules));
            tokio::task::spawn_blocking(move || list(&root, &opts, &rules.never, &not, MAX_FILES))
                .await
                .map_err(blocking)?
                .map_err(Error::internal)?
        };
        let had = db.root_items(root.id.clone()).await.map_err(store_err)?;
        let kept: HashMap<&str, &ptyro_store::kb::RootItem> =
            had.iter().map(|i| (i.uri.as_str(), i)).collect();
        // What changed: a file whose time or size is not its item's, nor what it was when it
        // was refused.
        let todo: Vec<&Found> = {
            let refused = self.kb.roots.refused.locked();
            listing
                .files
                .iter()
                .filter(|f| {
                    let cursor = opts.cursor(f);
                    kept.get(f.uri.as_str()).and_then(|i| i.cursor.as_ref()) != Some(&cursor)
                        && refused.get(&f.uri) != Some(&cursor)
                })
                .collect()
        };
        let there: HashSet<&str> = listing.files.iter().map(|f| f.uri.as_str()).collect();
        let gone: Vec<_> = had
            .iter()
            .filter(|i| match i.kind {
                ItemKind::Commit => !opts.commits || opts.off,
                _ => !there.contains(i.uri.as_str()),
            })
            .collect();
        // The repository's history, when it moved.
        let commits = if opts.commits && listing.git {
            let (dir, head) = (path.clone(), root.head.clone());
            tokio::task::spawn_blocking(move || new_commits(&dir, head.as_deref()))
                .await
                .map_err(blocking)?
        } else {
            None
        };
        let n_commits = commits.as_ref().map_or(0, |(_, c)| c.len());
        if todo.is_empty() && gone.is_empty() && n_commits == 0 {
            return self.kb_root_done(db, root, &listing, &opts, commits.map(|(tip, _)| tip)).await;
        }
        db.set_root_state(root.id.clone(), RootState::Syncing, None, None)
            .await
            .map_err(store_err)?;
        let secrets = self.kb_secrets(root.profile.as_deref()).await?;
        let stamp = Stamp {
            root: Some(root.id.clone()),
            project: root.project.clone(),
            ..Stamp::new(Writer::File, root.profile.clone())
        };
        let of = (todo.len() + n_commits) as u64;
        let mut done = 0u64;
        let mut repeats = self.kb.roots.repeats.locked().remove(&root.id).unwrap_or_default();
        let (mut said, mut news) = (Instant::now(), Instant::now());
        let mut changed: HashSet<ItemKind> = HashSet::new();
        self.kb_root_progress(root, 0, of);

        for item in &gone {
            self.kb_item_drop(db, &item.id, &item.uri).await?;
            let holder = hash(&item.uri);
            repeats.retain(|_, h| *h != holder);
            changed.insert(item.kind);
        }
        for f in todo {
            let cursor = opts.cursor(f);
            let old = kept.get(f.uri.as_str());
            let facts = Facts {
                // (Its path inside the root: where the root itself is was your choice.)
                path: Some(PathBuf::from(&f.rel)),
                bytes: Some(f.size),
                ..Facts::default()
            };
            let wrote = match self.kb_gate(db, &f.uri, &facts).await {
                Ok(passed) => {
                    let (tiers, at) = (opts.tiers, cursor.clone());
                    let one = Found {
                        rel: f.rel.clone(),
                        path: f.path.clone(),
                        uri: f.uri.clone(),
                        mtime_ms: f.mtime_ms,
                        size: f.size,
                    };
                    let (read, back) = tokio::task::spawn_blocking(move || {
                        let read = read(&one, tiers, at, &mut repeats);
                        (read, repeats)
                    })
                    .await
                    .map_err(blocking)?;
                    repeats = back;
                    match read {
                        Read::Source(source) => {
                            let p = Prepared { source: *source, stamp: stamp.clone(), facts };
                            self.kb_store(db, passed, p, secrets.clone()).await.map(Some)
                        }
                        Read::Binary => Ok(None),
                        // (Looked at again by the next pass.)
                        Read::Gone => {
                            done += 1;
                            continue;
                        }
                    }
                }
                Err(why) => Err(why),
            };
            match wrote {
                Ok(Some(w)) => {
                    if w.added + w.changed + w.removed > 0 {
                        changed.insert(ItemKind::File);
                    }
                    self.kb.roots.refused.locked().remove(&f.uri);
                }
                // The knowledge base was switched off under the pass: it stops.
                Err(Refused::Error(e)) if e.code == ErrorCode::Unsupported => {
                    self.kb.roots.repeats.locked().insert(root.id.clone(), repeats);
                    return Err(e);
                }
                Err(Refused::Error(e)) => {
                    self.kb.skip(&f.uri, e.message);
                }
                not_kept => {
                    // Not text says nothing; a refusal is named once, with why.
                    if let Err(Refused::Skip(why)) = not_kept
                        && why != Skip::Never
                    {
                        self.kb.skip(&f.uri, why.to_string());
                    }
                    self.kb.roots.refused.locked().insert(f.uri.clone(), cursor);
                    // What it held before is no longer what the file says.
                    if let Some(old) = old {
                        self.kb_item_drop(db, &old.id, &old.uri).await?;
                        changed.insert(ItemKind::File);
                    }
                }
            }
            done += 1;
            if said.elapsed() >= PROGRESS_EVERY {
                said = Instant::now();
                self.kb_root_progress(root, done, of);
            }
            if news.elapsed() >= NEWS_EVERY && !changed.is_empty() {
                news = Instant::now();
                self.kb_changed(changed.drain());
            }
            tokio::task::yield_now().await;
        }
        self.kb.roots.repeats.locked().insert(root.id.clone(), repeats);

        let mut head = None;
        if let Some((tip, commits)) = commits {
            // A commit forgotten under a root that was here before stays forgotten (this
            // root's id, and so its commits' URIs, are new).
            let forgotten: HashSet<String> = db
                .forgotten_commits(root.path.clone())
                .await
                .map_err(store_err)?
                .into_iter()
                .collect();
            // Oldest first, so that the history grows in its own order.
            for c in commits.into_iter().rev().filter(|c| !forgotten.contains(&c.sha)) {
                let uri = format!("commit://{}/{}", root.id, c.sha);
                let source = file::commit(uri.clone(), &c.subject, &c.body, &c.files, c.at_ms);
                let facts = Facts::default();
                let wrote = match self.kb_gate(db, &uri, &facts).await {
                    Ok(passed) => {
                        let p = Prepared { source, stamp: stamp.clone(), facts };
                        self.kb_store(db, passed, p, secrets.clone()).await
                    }
                    Err(why) => Err(why),
                };
                match wrote {
                    Ok(w) if w.added + w.changed + w.removed > 0 => {
                        changed.insert(ItemKind::Commit);
                    }
                    Ok(_) => {}
                    Err(Refused::Error(e)) if e.code == ErrorCode::Unsupported => return Err(e),
                    Err(Refused::Error(e)) => self.kb.skip(&uri, e.message),
                    Err(Refused::Skip(Skip::Forgotten)) => {}
                    Err(Refused::Skip(why)) => self.kb.skip(&uri, why.to_string()),
                }
                done += 1;
                if said.elapsed() >= PROGRESS_EVERY {
                    said = Instant::now();
                    self.kb_root_progress(root, done, of);
                }
                tokio::task::yield_now().await;
            }
            head = Some(tip);
        }
        self.kb_root_progress(root, of, of);
        if !changed.is_empty() {
            self.kb_changed(changed);
        }
        self.kb_root_done(db, root, &listing, &opts, head).await
    }

    /// A pass ended well: the root is `ok`, as of now, with what there is to say of it.
    async fn kb_root_done(
        &self,
        db: &KbDb,
        root: &KbRoot,
        listing: &Listing,
        opts: &Opts,
        head: Option<String>,
    ) -> Result<(), Error> {
        let note = if opts.off {
            Some("its project keeps itself out (`[knowledge] enabled = false`)".to_owned())
        } else if listing.total > MAX_FILES {
            Some(format!(
                "it holds {}{} files and the first {MAX_FILES} are read: reading only some of them (include) narrows it",
                if listing.total >= WALK_MAX { "at least " } else { "" },
                listing.total
            ))
        } else {
            None
        };
        // (Its row as it is now: its options may have been changed under the pass.)
        let Some(now) = db.root(root.id.clone()).await.map_err(store_err)? else { return Ok(()) };
        let head = if opts.commits && !opts.off { head.or(now.head.clone()) } else { None };
        let root =
            KbRoot { state: RootState::Ok, error: note, head, synced_ms: Some(now_ms()), ..now };
        db.put_root(root).await.map_err(store_err)
    }

    /// An item of a root goes, text and all, with no tombstone: its file is the truth, and a
    /// file of that name written later is read again.
    async fn kb_item_drop(&self, db: &KbDb, id: &str, uri: &str) -> Result<(), Error> {
        db.forget(id.to_owned()).await.map_err(store_err)?;
        db.delete_item(id.to_owned()).await.map_err(store_err)?;
        tracing::debug!("knowledge base: {uri} is gone");
        Ok(())
    }

    // ── requests ─────────────────────────────────────────────────────────────────────────────

    /// A root by what it was called: its name, its id, or its path.
    async fn kb_root(&self, db: &KbDb, which: &str) -> Result<KbRoot, Error> {
        let which = which.trim();
        let mut tries = vec![which.to_owned()];
        if ptyro_core::os::looks_like_path(which) {
            let path = ptyro_core::config::expand_home(which);
            let path = ptyro_core::os::canonicalize(&path).unwrap_or(path);
            tries.insert(0, path.to_string_lossy().into_owned());
        }
        for t in tries {
            if let Some(root) = db.root(t).await.map_err(store_err)? {
                return Ok(root);
            }
        }
        let names: Vec<String> =
            db.roots().await.map_err(store_err)?.into_iter().map(|r| r.name).collect();
        Err(ptyro_core::lookup::not_found_among("root", which, &names))
    }

    /// **`KbRoots`**: every root a caller may see, with what it holds and how far its pass is.
    pub(super) async fn kb_roots(&self, scope: Option<&KbScope>) -> Result<Vec<KbRootInfo>, Error> {
        let db = self.kb_db()?;
        let roots = db.roots().await.map_err(store_err)?;
        let counts = db.root_counts().await.map_err(store_err)?;
        let wall = scope.and_then(|s| s.profile.as_deref());
        Ok(roots
            .into_iter()
            .filter(|r| wall.is_none_or(|p| r.profile.as_deref().is_none_or(|own| own == p)))
            .map(|root| self.kb_root_info(root, &counts))
            .collect())
    }

    fn kb_root_info(&self, root: KbRoot, counts: &[(String, ItemKind, u64, u64)]) -> KbRootInfo {
        let of = |kind: ItemKind| {
            counts
                .iter()
                .find(|(id, k, ..)| id == &root.id && *k == kind)
                .map_or((0, 0), |(_, _, items, chunks)| (*items, *chunks))
        };
        let ((files, a), (commits, b)) = (of(ItemKind::File), of(ItemKind::Commit));
        let progress = self.kb.roots.passes.locked().get(&root.id).map(|p| (p.done, p.of));
        let project_name =
            root.project.as_deref().and_then(|id| self.project_by_id(id)).map(|p| p.slug);
        KbRootInfo {
            removing: self.kb.roots.removing.locked().contains(&root.id),
            root,
            project_name,
            files,
            commits,
            chunks: a + b,
            progress,
        }
    }

    /// **`KbIngest`** (`ptyro ingest PATH`): add a root and read it; a path that is a root
    /// already, or inside one, is read again instead.
    pub(super) async fn kb_ingest_root(self: &Arc<Self>, spec: RootSpec) -> Result<Reply, Error> {
        let db = self.kb_db()?;
        let asked = spec.path.trim();
        if asked.is_empty() {
            return Err(Error::invalid("say which folder or file to read"));
        }
        let path = ptyro_core::config::expand_home(asked);
        if !path.is_absolute() {
            return Err(Error::invalid(format!("{asked} is not a whole path (`/…` or `~/…`)")));
        }
        let path = ptyro_core::os::canonicalize(&path)
            .map_err(|e| Error::new(ErrorCode::NotFound, format!("{}: {e}", path.display())))?;
        let data = self.kb_own_dir();
        if ptyro_core::os::path_starts_with(&path, &data) {
            return Err(Error::invalid(format!(
                "{} is ptyro's own folder: a project's worktrees are never read, its own checkout is",
                path.display()
            )));
        }
        let rules = self.kb_rules();
        if rules.never.is_never(&path) {
            return Err(Error::invalid(format!(
                "{} is never read: its path is one of keys and credentials",
                path.display()
            )));
        }
        let profile = self.check_profile(spec.profile.clone().filter(|p| !p.trim().is_empty()))?;
        let project = match spec.project.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => {
                let p = self.find_project(p)?;
                if p.machine.is_some() {
                    return Err(Error::invalid(format!(
                        "{} is another machine's project: a folder there is ingested there",
                        p.slug
                    )));
                }
                Some(p.id)
            }
            None => None,
        };
        let roots = db.roots().await.map_err(store_err)?;
        let text = path.to_string_lossy().into_owned();
        // Already a root, or inside one (the innermost): that one is read again.
        let inside = roots
            .iter()
            .filter(|r| ptyro_core::os::path_starts_with(&path, Path::new(&r.path)))
            .max_by_key(|r| r.path.len());
        let (root, existing) = match inside {
            Some(r) => {
                if self.kb.roots.removing.locked().contains(&r.id) {
                    return Err(Error::new(
                        ErrorCode::Busy,
                        format!("{} is being removed: try again in a moment", r.name),
                    ));
                }
                let mut root = r.clone();
                // Its own path, with options: they are its options from now on.
                if r.path == text {
                    let mut changed = false;
                    if spec.options != RootOptions::default() && spec.options != r.options {
                        root.options = spec.options.clone();
                        changed = true;
                    }
                    if r.kind != RootKind::Project {
                        if project.is_some() && project != r.project {
                            root.project = project.clone();
                            changed = true;
                        }
                        if profile.is_some() && profile != r.profile {
                            return Err(Error::invalid(format!(
                                "{} is read for {}: `ptyro kb rm {}`, then ingest it for another profile",
                                r.name,
                                r.profile
                                    .as_deref()
                                    .map_or("every profile".to_owned(), |p| format!("profile {p}")),
                                ptyro_core::kb::root_word(&r.name, &r.id)
                            )));
                        }
                    }
                    if changed {
                        db.put_root(root.clone()).await.map_err(store_err)?;
                    }
                }
                (root, true)
            }
            None => {
                let meta = std::fs::metadata(&path)
                    .map_err(|e| Error::new(ErrorCode::NotFound, format!("{text}: {e}")))?;
                // (A name is shown in lists and said in messages: no control character of a
                // folder's name, or of what was typed, is part of it.)
                let clean = |n: &str| -> String { n.chars().filter(|c| !c.is_control()).collect() };
                let given = spec.name.as_deref().map(clean);
                let name = match given.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    Some(n) if roots.iter().any(|r| r.name == n) => {
                        return Err(Error::new(
                            ErrorCode::Conflict,
                            format!("a root is called {n} already: give this one another name"),
                        ));
                    }
                    Some(n) => n.to_owned(),
                    None => {
                        let base = path
                            .file_name()
                            .map(|n| clean(&n.to_string_lossy()))
                            .filter(|n| !n.trim().is_empty())
                            .unwrap_or_else(|| "root".into());
                        let (mut name, mut k) = (base.clone(), 1);
                        while roots.iter().any(|r| r.name == name) {
                            k += 1;
                            name = format!("{base}-{k}");
                        }
                        name
                    }
                };
                let root = KbRoot {
                    id: new_id(),
                    name,
                    kind: if meta.is_file() { RootKind::File } else { RootKind::Folder },
                    path: text,
                    project,
                    profile,
                    options: spec.options.clone(),
                    head: None,
                    state: RootState::Syncing,
                    error: None,
                    added_ms: Some(now_ms()),
                    synced_ms: None,
                };
                db.put_root(root.clone()).await.map_err(store_err)?;
                // (The roots around it: what they kept of this folder is this root's now.)
                self.kb_roots_kick(Self::kb_roots_around(&roots, &root.path));
                (root, false)
            }
        };
        // What will be read, said before it is.
        let opts = Arc::new(self.kb_root_opts(&root));
        let listing = {
            let not = self.kb_root_not(&db, &root).await?;
            let (root, opts, rules) = (root.clone(), Arc::clone(&opts), Arc::clone(&rules));
            tokio::task::spawn_blocking(move || list(&root, &opts, &rules.never, &not, MAX_FILES))
                .await
                .map_err(blocking)?
                .map_err(Error::internal)?
        };
        let (mut documents, mut sources) = (0, 0);
        for f in &listing.files {
            if file::document(&f.rel).is_some() {
                documents += 1;
            } else if ptyro_core::kb::comments::marks(&f.rel, "").is_some() {
                sources += 1;
            }
        }
        let plan = KbPlan {
            existing,
            git: listing.git,
            files: listing.files.len() as u64,
            documents,
            sources,
            comments: opts.tiers.comments,
            code: opts.tiers.code,
            commits: opts.commits && listing.git,
            over: (listing.total > MAX_FILES).then_some(listing.total as u64),
        };
        self.kb_roots_kick(vec![root.id.clone()]);
        let counts = db.root_counts().await.map_err(store_err)?;
        Ok(Reply::KbIngested { root: self.kb_root_info(root, &counts), plan })
    }

    /// **`KbRemove`** (`ptyro kb rm`): stop reading a root and drop what was kept of it.
    pub(super) async fn kb_remove_root(
        self: &Arc<Self>,
        which: &str,
        confirmed: bool,
    ) -> Result<Reply, Error> {
        let db = self.kb_db()?;
        let root = self.kb_root(&db, which).await?;
        if root.kind == RootKind::Project {
            return Err(Error::invalid(format!(
                "{} is a project's own root, read because `kb.projects` is on: `[knowledge] enabled = false` in its .ptyro.toml keeps that project out, `ptyro config set kb.projects false` all of them",
                root.name
            )));
        }
        let counts = db.root_counts().await.map_err(store_err)?;
        let info = self.kb_root_info(root.clone(), &counts);
        if !confirmed {
            return Err(Error::new(
                ErrorCode::NeedsConfirmation,
                format!(
                    "Stop reading {} ({}) and drop what was kept of it: {} file(s) and {} commit(s)? {}",
                    root.name,
                    root.path,
                    info.files,
                    info.commits,
                    ptyro_core::removal::KB_ROOT
                ),
            ));
        }
        self.kb_root_drop(root.clone());
        Ok(Reply::KbRemoved {
            name: root.name,
            items: info.files + info.commits,
            chunks: info.chunks,
        })
    }

    /// Remove a root and its items, in the background and in batches (§25.12): no tombstones
    /// are made, and those there are (what you forgot under it) stay.
    fn kb_root_drop(self: &Arc<Self>, root: KbRoot) {
        if !self.kb.roots.removing.locked().insert(root.id.clone()) {
            return;
        }
        self.kb.pending.send_modify(|n| *n += 1);
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let one = me.kb.roots.one.lock().await;
            if let Ok(db) = me.kb_db() {
                match db.remove_root(root.id.clone(), |_| {}).await {
                    Ok(chunks) => {
                        let _ = db.checkpoint().await;
                        tracing::info!(
                            "knowledge base: root {} removed ({chunks} piece(s))",
                            root.name
                        );
                    }
                    Err(e) => tracing::warn!(
                        "knowledge base: removing root {}: {}",
                        root.name,
                        store_err(e).message
                    ),
                }
            }
            let under = format!("{}/", file_uri(Path::new(&root.path)));
            me.kb.roots.refused.locked().retain(|uri, _| !uri.starts_with(&under));
            me.kb.roots.repeats.locked().remove(&root.id);
            me.kb.roots.removing.locked().remove(&root.id);
            drop(one);
            me.kb_changed([ItemKind::File, ItemKind::Commit]);
            // A root around it reads that folder itself from now on.
            if let Ok(db) = me.kb_db()
                && let Ok(roots) = db.roots().await
            {
                me.kb_roots_kick(Self::kb_roots_around(&roots, &root.path));
            }
            // (A project whose root went because its pin changed is made again.)
            if root.kind == RootKind::Project {
                me.kb_roots_reconcile().await;
            }
            me.kb.done();
        });
    }
}

/// The commits to read of the repository at `dir`: its main branch's tip, and what `head`
/// (the newest commit read) doesn't have of its history; the newest [`COMMITS_FIRST`] when
/// nothing was read yet or `head` is no longer a commit here. `None`: nothing new, or no
/// repository to ask.
fn new_commits(dir: &Path, head: Option<&str>) -> Option<(String, Vec<ptyro_git::log::Commit>)> {
    let git = Git::new();
    let top = git.toplevel(dir).ok()?;
    let (_, tip) = git.history_tip(dir)?;
    if head == Some(tip.as_str()) {
        return None;
    }
    let since = head.filter(|h| git.rev(dir, h).is_some());
    let here = ptyro_core::os::canonicalize(&top).ok() != ptyro_core::os::canonicalize(dir).ok();
    let commits = git.log(dir, &tip, since, COMMITS_FIRST, here).ok()?;
    Some((tip, commits))
}

#[cfg(test)]
mod tests;
