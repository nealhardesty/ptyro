//! A repository that moved (ROADMAP UX3.10, SPEC §10.1). `ProjectMove` points a project at
//! where its repository is now: it keeps its id, so its trust, pin, workspaces, sessions and
//! worktree history stay, and its worktrees are linked to the new place (`git worktree repair`).
//! A project registered at the new place already (auto-registration's `woa-2`) is folded in.
//!
//! Spotting: a project whose main checkout is gone, and a repository that is the same one (what
//! the repository's own records say, its root commits, its remote), make an offer
//! (`ProjectInfo.moved_to`) instead of a second project: auto-registration registers nothing,
//! `project add` asks, and every client offers the move.
//!
//! Safety, before anything changes: the new place is a repository's main checkout; it is the
//! same repository (never another one: no override); the old place is gone or holds something
//! else (two checkouts of one repository are a copy, not a move). Nothing is deleted, ever: the
//! only writes outside ptyro's database are git's own repairs of worktree links.

use ptyro_core::sync::MutexExt;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ptyro_core::model::{ProjectInfo, WorktreeInfo, WorktreeState};
use ptyro_core::{Error, lookup};
use ptyro_git::{Git, Linked};
use ptyro_protocol::{DomainEvent, Reply};
use ptyro_store::TrustRow;

use crate::daemon::Daemon;
use crate::registry::git_err;
use crate::worktrees::blocking;

/// kv: project id → its root commits, recorded while its checkout was there.
const ROOTS: &str = "repo:roots";
/// kv: `[project id, place]` pairs a move was declined for (added there as its own project).
const NOT_MOVED: &str = "move:declined";
/// kv: project id → where and when its checkout was last seen ([`Seen`]).
const SEEN: &str = "repo:seen";
/// kv: project id → the place it seems to have moved to (`moved_to`): an offer survives a restart.
const OFFERED: &str = "move:offered";

/// What ptyro can tell about a repository from its main checkout.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub root: PathBuf,
    /// Its root commits (empty: not looked at, or no commits).
    pub roots: Vec<String>,
    pub remote: Option<String>,
    /// The linked worktrees its own records list.
    pub linked: Vec<Linked>,
}

impl Facts {
    /// Everything, from git (blocking; the root commits walk the history).
    pub(crate) fn of(git: &Git, root: &Path) -> Self {
        Facts {
            root: root.to_path_buf(),
            roots: git.root_commits(root).unwrap_or_default(),
            remote: git.main_remote(root).map(|r| r.url),
            linked: git.linked(root),
        }
    }
}

/// How ptyro knows a repository is the one a project had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Proof {
    /// Only the remote is the same (a fresh clone is the same repository too).
    Remote,
    /// It has the project's history (a root commit in common), and no other remote.
    History,
    /// Its own records name the project's worktrees, or places under its old root: it is the
    /// very repository (moved, or copied).
    Records,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Same {
    Yes(Proof),
    /// Another repository, and why.
    No(String),
    /// Nothing to tell by.
    Unknown,
}

/// Whether `new` is the repository `p` had. `recorded`: `p`'s root commits, as recorded while
/// its checkout was there (none for a project registered before ptyro kept them).
pub(crate) fn same_repository(
    p: &ProjectInfo,
    recorded: &[String],
    worktrees: &[WorktreeInfo],
    new: &Facts,
) -> Same {
    let ours = new.linked.iter().any(|l| {
        l.project.as_deref() == Some(p.id.as_str())
            || l.path.as_ref().is_some_and(|lp| {
                ptyro_core::os::path_starts_with(lp, &p.root)
                    || worktrees.iter().any(|w| {
                        !w.is_root
                            && w.project == p.id
                            && ptyro_core::os::same_path(
                                &ptyro_core::os::canonicalize(&w.path).unwrap_or(w.path.clone()),
                                lp,
                            )
                    })
            })
    });
    if !recorded.is_empty()
        && !new.roots.is_empty()
        && !recorded.iter().any(|r| new.roots.contains(r))
    {
        return Same::No(format!("its history has no commit in common with {}'s", p.slug));
    }
    let remote = match (&p.remote_url, &new.remote) {
        (Some(a), Some(b)) => {
            Some(ptyro_git::normalize_remote(a) == ptyro_git::normalize_remote(b))
        }
        _ => None,
    };
    if remote == Some(false) && !ours {
        return Same::No(format!(
            "its remote is {}, and {}'s is {}",
            new.remote.as_deref().unwrap_or("-"),
            p.slug,
            p.remote_url.as_deref().unwrap_or("-")
        ));
    }
    if ours {
        Same::Yes(Proof::Records)
    } else if !recorded.is_empty() && !new.roots.is_empty() {
        Same::Yes(Proof::History)
    } else if remote == Some(true) {
        Same::Yes(Proof::Remote)
    } else {
        Same::Unknown
    }
}

/// Two checkouts of one repository (the old place still there): one history, else one remote.
fn one_repository(a: &Facts, b: &Facts) -> bool {
    if !a.roots.is_empty() && !b.roots.is_empty() {
        return a.roots.iter().any(|r| b.roots.contains(r));
    }
    match (&a.remote, &b.remote) {
        (Some(x), Some(y)) => ptyro_git::normalize_remote(x) == ptyro_git::normalize_remote(y),
        _ => false,
    }
}

/// What the drive checks ask of the file system (a fake one in the tests).
pub(crate) trait Disk {
    /// The device `path` is on, following symlinks; `None`: nothing is there.
    fn device(&self, path: &Path) -> Option<u64>;
    /// Whether `path` is a directory with nothing in it.
    fn empty(&self, path: &Path) -> bool;
}

/// The real file system.
pub(crate) struct RealDisk;

impl Disk for RealDisk {
    fn device(&self, path: &Path) -> Option<u64> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(meta.dev())
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            Some(0)
        }
    }
    fn empty(&self, path: &Path) -> bool {
        std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_none())
    }
}

/// Where a checkout was last seen: when, and the mount point of the drive it was on (kv
/// `repo:seen`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Seen {
    pub ms: u64,
    pub mount: PathBuf,
}

/// The mount point of the drive `path` is on: its highest ancestor on the same device.
pub(crate) fn mount_of(path: &Path, disk: &dyn Disk) -> Option<PathBuf> {
    let dev = disk.device(path)?;
    let mut top = path;
    for a in path.ancestors().skip(1) {
        if disk.device(a) != Some(dev) {
            break;
        }
        top = a;
    }
    Some(top.to_path_buf())
}

/// Whether a drive is mounted at `dir` now: it is on another device than its parent (`/` always
/// is). A mount point left behind (an empty directory) is on its parent's.
fn mounted_at(dir: &Path, disk: &dyn Disk) -> bool {
    match (disk.device(dir), dir.parent()) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(a), Some(p)) => disk.device(p).is_some_and(|b| b != a),
    }
}

/// Where drives are mounted: `/` (`/data`), `/mnt`, `/Volumes`, `/media`, `/run/media` and the
/// per-user directories inside the last two.
fn where_drives_mount(dir: &Path) -> bool {
    ["/", "/Volumes", "/media", "/mnt", "/run/media"].iter().any(|m| dir == Path::new(m))
        || dir.parent().is_some_and(|p| p == Path::new("/media") || p == Path::new("/run/media"))
}

/// The old place `root` was on a drive that isn't mounted now: its project may well come back, so
/// no move is offered on its own. `mount`: the mount point its drive had when its checkout was
/// last seen; no drive mounted there now (gone, or an empty directory left behind, as after an
/// unclean eject or an fstab mount that didn't happen) is unmounted. Never seen by this ptyro:
/// nothing of it left above where drives mount, or only an empty directory there that isn't a
/// mount point.
pub(crate) fn unmounted(root: &Path, mount: Option<&Path>, disk: &dyn Disk) -> bool {
    if let Some(m) = mount.filter(|m| ptyro_core::os::path_starts_with(root, m)) {
        return !mounted_at(m, disk);
    }
    let Some(left) = root.ancestors().skip(1).find(|d| disk.device(d).is_some()) else {
        return false;
    };
    where_drives_mount(left)
        || (disk.empty(left)
            && !mounted_at(left, disk)
            && left.parent().is_some_and(where_drives_mount))
}

/// `new` is `old` on its drive mounted again under another name (`/Volumes/EXT 1/dev/app` for
/// `/Volumes/EXT/dev/app`, as macOS mounts a drive whose mount point was left behind; `EXT1` as
/// udisks does): the same drive, not a move.
pub(crate) fn remounted(old: &Path, new: &Path) -> bool {
    let (a, b): (Vec<_>, Vec<_>) = (old.components().collect(), new.components().collect());
    if a.len() != b.len() {
        return false;
    }
    let mut differ = (0..a.len()).filter(|&i| a[i] != b[i]);
    let (Some(i), None) = (differ.next(), differ.next()) else { return false };
    let parent: PathBuf = a[..i].iter().collect();
    let (x, y) = (a[i].as_os_str().to_string_lossy(), b[i].as_os_str().to_string_lossy());
    let number = y.strip_prefix(&*x).map(|rest| rest.strip_prefix(' ').unwrap_or(rest));
    number.is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        && where_drives_mount(&parent)
}

/// `<parent>/<leaf>.worktrees`, where `[worktrees] location = "sibling"` puts them.
fn sibling(root: &Path) -> Option<PathBuf> {
    let leaf = root.file_name()?.to_string_lossy().into_owned();
    Some(root.parent()?.join(format!("{leaf}.worktrees")))
}

/// Where a worktree of a project moved from `old` to `new` is now: inside the old root, it moved
/// with it; beside it (`sibling`), where the same directory beside the new root exists; anywhere
/// else (ptyro's own worktrees directory), it stayed.
fn moved_path(path: &Path, old: &Path, new: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix(old) {
        return Some(new.join(rest));
    }
    let (from, to) = (sibling(old)?, sibling(new)?);
    let rest = path.strip_prefix(&from).ok()?;
    let there = to.join(rest);
    there.is_dir().then_some(there)
}

/// `old` → `new` in `path`, when `path` is `old` or inside it.
fn rebased(path: &Path, old: &Path, new: &Path) -> Option<PathBuf> {
    path.strip_prefix(old)
        .ok()
        .map(|rest| if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) })
}

/// The move's database write failed (SPEC §10.1). git's link repairs ran before it: the message
/// says which worktrees were linked to the new place already, and that the move, run again,
/// finishes (what is linked already is left as it is).
fn store_failed(error: &str, p: &ProjectInfo, to: &Path, repaired: &[String]) -> String {
    let mut text =
        format!("ptyro's database: {error}. {} still points at {}", p.slug, p.root.display());
    if repaired.is_empty() {
        text.push_str(": nothing was changed");
    } else {
        text.push_str(&format!(
            ", but git already linked {} to {}: {}. Move {} there again to finish",
            plural(repaired.len(), "worktree", "worktrees"),
            to.display(),
            repaired.join(", "),
            p.slug
        ));
    }
    text
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Daemon {
    // ── memory ──────────────────────────────────────────────────────────────

    pub(crate) async fn load_moves(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get(ROOTS.into()).await
            && let Ok(map) = serde_json::from_value::<HashMap<String, Vec<String>>>(v)
        {
            *self.repo_roots.locked() = map;
        }
        if let Ok(Some(v)) = store.kv_get(NOT_MOVED.into()).await
            && let Ok(list) = serde_json::from_value::<Vec<(String, PathBuf)>>(v)
        {
            *self.not_moved.locked() = list.into_iter().collect();
        }
        if let Ok(Some(v)) = store.kv_get(SEEN.into()).await
            && let Ok(map) = serde_json::from_value::<HashMap<String, Seen>>(v)
        {
            *self.repo_seen.locked() = map;
        }
        // The offers made before the restart: the look at start keeps the ones still true.
        if let Ok(Some(v)) = store.kv_get(OFFERED.into()).await
            && let Ok(map) = serde_json::from_value::<HashMap<String, PathBuf>>(v)
        {
            let mut reg = self.registry.locked();
            for p in reg.projects.iter_mut().filter(|p| p.machine.is_none()) {
                if let Some(to) = map.get(&p.id) {
                    p.moved_to = Some(to.clone());
                }
            }
        }
    }

    fn save_seen(&self) {
        let map = self.repo_seen.locked().clone();
        self.save_kv(SEEN, serde_json::to_value(map).unwrap_or_default());
    }

    /// The offers there are now (`moved_to`), kept for a restart.
    fn save_offered(&self) {
        let map: HashMap<String, PathBuf> = {
            let reg = self.registry.locked();
            reg.projects
                .iter()
                .filter(|p| p.machine.is_none())
                .filter_map(|p| Some((p.id.clone(), p.moved_to.clone()?)))
                .collect()
        };
        self.save_kv(OFFERED, serde_json::to_value(map).unwrap_or_default());
    }

    /// Something that changes what a look for moves would find happened: a look already under
    /// way changes nothing when it finishes.
    fn moves_changed(&self) {
        self.moves_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn save_roots(&self) {
        let map = self.repo_roots.locked().clone();
        self.save_kv(ROOTS, serde_json::to_value(map).unwrap_or_default());
    }

    fn save_not_moved(&self) {
        let list: Vec<(String, PathBuf)> = self.not_moved.locked().iter().cloned().collect();
        self.save_kv(NOT_MOVED, serde_json::to_value(list).unwrap_or_default());
    }

    /// Remember who the repository at `root` is (its root commits), off the async workers.
    pub(crate) fn record_roots_bg(self: &Arc<Self>, id: String, root: PathBuf) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let Ok(Ok(roots)) = blocking(move || Git::new().root_commits(&root)).await else {
                return;
            };
            if roots.is_empty() {
                return;
            }
            me.repo_roots.locked().insert(id, roots);
            me.save_roots();
        });
    }

    /// Every checkout that is there now is seen (when, and on which drive), before the request
    /// that registers a project answers: a clone registered beside a checkout that is there is
    /// then never taken for it, moved, however soon after that checkout goes.
    pub(crate) fn see_checkouts(&self) {
        let roots: Vec<(String, PathBuf)> = {
            let reg = self.registry.locked();
            reg.projects
                .iter()
                .filter(|p| p.machine.is_none())
                .map(|p| (p.id.clone(), p.root.clone()))
                .collect()
        };
        let now = ptyro_core::now_ms();
        let seen: Vec<(String, Seen)> = roots
            .into_iter()
            .filter(|(_, root)| root.join(".git").exists())
            .filter_map(|(id, root)| {
                Some((id, Seen { ms: now, mount: mount_of(&root, &RealDisk)? }))
            })
            .collect();
        if !seen.is_empty() {
            self.repo_seen.locked().extend(seen);
            self.save_seen();
        }
    }

    /// A project forgotten: what was kept about it goes too.
    pub(crate) fn forget_moves(&self, id: &str) {
        if self.repo_roots.locked().remove(id).is_some() {
            self.save_roots();
        }
        let before = self.not_moved.locked().len();
        self.not_moved.locked().retain(|(p, _)| p != id);
        if self.not_moved.locked().len() != before {
            self.save_not_moved();
        }
        if self.repo_seen.locked().remove(id).is_some() {
            self.save_seen();
        }
        self.save_offered();
    }

    /// At start: record the root commits of every project there is no record of yet (one at a
    /// time, in the background), then look for projects that moved.
    pub(crate) fn start_moves(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let todo: Vec<(String, PathBuf)> = {
                let known = me.repo_roots.locked().clone();
                let reg = me.registry.locked();
                reg.projects
                    .iter()
                    .filter(|p| p.machine.is_none() && !known.contains_key(&p.id))
                    .map(|p| (p.id.clone(), p.root.clone()))
                    .collect()
            };
            let mut got = Vec::new();
            for (id, root) in todo {
                let roots = blocking(move || {
                    if root.is_dir() {
                        Git::new().root_commits(&root).unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                })
                .await
                .unwrap_or_default();
                if !roots.is_empty() {
                    got.push((id, roots));
                }
            }
            if !got.is_empty() {
                me.repo_roots.locked().extend(got);
                me.save_roots();
            }
            me.spot_moves().await;
        });
    }

    // ── spotting ────────────────────────────────────────────────────────────

    /// Say on `p` that it seems to have moved to `to` (`None`: not, or no longer).
    fn set_moved_to(&self, id: &str, to: Option<PathBuf>) {
        let info = {
            let mut reg = self.registry.locked();
            let Some(p) = reg.projects.iter_mut().find(|p| p.id == id) else { return };
            if p.moved_to == to {
                return;
            }
            p.moved_to = to;
            p.clone()
        };
        if let Some(to) = &info.moved_to {
            tracing::info!(project = %info.slug, to = %to.display(), "seems to have moved");
        }
        self.save_offered();
        let _ = self.events.send(DomainEvent::ProjectUpdated { info });
    }

    /// The project whose main checkout is gone that `root` (a repository's main checkout, not a
    /// project) seems to be, moved. The best proof wins, then the one used last.
    pub(crate) async fn moved_here(&self, root: &Path) -> Option<ProjectInfo> {
        self.all_moved_here(root).await.into_iter().next()
    }

    /// Every project whose main checkout is gone that `root` seems to be, moved, the best first.
    /// None whose drive isn't mounted, none declined there, none for a drive mounted again
    /// under another name.
    pub(crate) async fn all_moved_here(&self, root: &Path) -> Vec<ProjectInfo> {
        let (missing, worktrees, roots, declined, seen) = {
            let reg = self.registry.locked();
            let missing: Vec<ProjectInfo> =
                reg.projects.iter().filter(|p| p.machine.is_none()).cloned().collect();
            (
                missing,
                reg.worktrees.clone(),
                self.repo_roots.locked().clone(),
                self.not_moved.locked().clone(),
                self.repo_seen.locked().clone(),
            )
        };
        if missing.is_empty() {
            return Vec::new();
        }
        let root = root.to_path_buf();
        blocking(move || {
            let missing: Vec<ProjectInfo> = missing
                .into_iter()
                .filter(|p| {
                    !p.root.is_dir()
                        && !unmounted(
                            &p.root,
                            seen.get(&p.id).map(|s| s.mount.as_path()),
                            &RealDisk,
                        )
                        && !remounted(&p.root, &root)
                })
                .filter(|p| !declined.contains(&(p.id.clone(), root.clone())))
                .collect();
            if missing.is_empty() {
                return Vec::new();
            }
            let facts = Facts::of(&Git::new(), &root);
            let mut found: Vec<(Proof, u64, ProjectInfo)> = missing
                .into_iter()
                .filter_map(|p| {
                    let recorded = roots.get(&p.id).cloned().unwrap_or_default();
                    match same_repository(&p, &recorded, &worktrees, &facts) {
                        Same::Yes(proof) => {
                            Some((proof, p.last_used_ms.unwrap_or(p.created_ms), p))
                        }
                        _ => None,
                    }
                })
                .collect();
            found.sort_by_key(|(proof, used, _)| std::cmp::Reverse((*proof, *used)));
            found.into_iter().map(|(_, _, p)| p).collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Offer the move: `p` seems to be at `root` now (every client shows it on `p`).
    pub(crate) fn offer_move(&self, p: &ProjectInfo, root: &Path) {
        self.moves_changed();
        self.set_moved_to(&p.id, Some(root.to_path_buf()));
    }

    /// A move was declined: `p` isn't offered `root` again.
    pub(crate) fn decline_move(&self, p: &ProjectInfo, root: &Path) {
        self.moves_changed();
        if self.not_moved.locked().insert((p.id.clone(), root.to_path_buf())) {
            self.save_not_moved();
        }
        let offered = self.project_by_id(&p.id).and_then(|p| p.moved_to);
        if offered.is_some_and(|t| ptyro_core::os::same_path(&t, root)) {
            self.set_moved_to(&p.id, None);
        }
    }

    pub(crate) fn spot_moves_bg(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move { me.spot_moves().await });
    }

    /// For every project whose main checkout is gone: a project registered since that is its
    /// repository (the duplicate auto-registration made before 1.53), or the place offered
    /// earlier while it still holds that repository. One whose checkout is back, or whose drive
    /// isn't mounted, is offered nothing. A checkout that is there is seen: when, and on which
    /// drive.
    pub(crate) async fn spot_moves(self: &Arc<Self>) {
        let generation = self.moves_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let (projects, worktrees, roots, declined, seen) = {
            let reg = self.registry.locked();
            let projects: Vec<ProjectInfo> =
                reg.projects.iter().filter(|p| p.machine.is_none()).cloned().collect();
            (
                projects,
                reg.worktrees.clone(),
                self.repo_roots.locked().clone(),
                self.not_moved.locked().clone(),
                self.repo_seen.locked().clone(),
            )
        };
        let Ok((changes, now_seen)) = blocking(move || {
            let git = Git::new();
            let now = ptyro_core::now_ms();
            let (missing, present): (Vec<_>, Vec<_>) =
                projects.into_iter().partition(|p| !p.root.is_dir());
            // Seen: its checkout (not merely a directory) is there.
            let now_seen: Vec<(String, Seen)> = present
                .iter()
                .filter(|p| p.root.join(".git").exists())
                .filter_map(|p| {
                    Some((p.id.clone(), Seen { ms: now, mount: mount_of(&p.root, &RealDisk)? }))
                })
                .collect();
            // One whose checkout is back is offered nothing.
            let mut changes: Vec<(String, Option<PathBuf>)> = present
                .iter()
                .filter(|p| p.moved_to.is_some())
                .map(|p| (p.id.clone(), None))
                .collect();
            if missing.is_empty() {
                return (changes, now_seen);
            }
            // The registered ones as their records say: no history walk for each.
            let mut known: Vec<(ProjectInfo, Facts)> = Vec::new();
            for d in &present {
                known.push((
                    d.clone(),
                    Facts {
                        root: d.root.clone(),
                        roots: roots.get(&d.id).cloned().unwrap_or_default(),
                        remote: d.remote_url.clone(),
                        linked: git.linked(&d.root),
                    },
                ));
            }
            for m in &missing {
                let recorded = roots.get(&m.id).cloned().unwrap_or_default();
                let last = seen.get(&m.id);
                let allowed = |to: &Path| {
                    !declined.contains(&(m.id.clone(), to.to_path_buf())) && !remounted(&m.root, to)
                };
                // Registered after its checkout was last seen (else after it was registered,
                // for one this ptyro never saw): one registered while it was there is a clone
                // of its own, kept on purpose.
                let since = m.created_ms.max(last.map_or(0, |s| s.ms));
                let want = if unmounted(&m.root, last.map(|s| s.mount.as_path()), &RealDisk) {
                    None
                } else if let Some((_, d)) = known
                    .iter()
                    .filter(|(d, _)| allowed(&d.root))
                    .filter_map(|(d, f)| match same_repository(m, &recorded, &worktrees, f) {
                        // Its own records: the very repository.
                        Same::Yes(Proof::Records) => Some((Proof::Records, d)),
                        // The same history or remote: any clone has them. Only a project
                        // registered since this one's checkout went can be it, moved.
                        Same::Yes(proof) if d.created_ms > since => Some((proof, d)),
                        _ => None,
                    })
                    .max_by_key(|(proof, d)| (*proof, d.created_ms))
                {
                    Some(d.root.clone())
                } else {
                    // A place offered before (not a project): kept while it is that repository.
                    m.moved_to.clone().filter(|to| {
                        allowed(to)
                            && !present.iter().any(|d| ptyro_core::os::same_path(&d.root, to))
                            && git.main_root(to).is_ok_and(|r| ptyro_core::os::same_path(&r, to))
                            && matches!(
                                same_repository(m, &recorded, &worktrees, &Facts::of(&git, to)),
                                Same::Yes(_)
                            )
                    })
                };
                if want != m.moved_to {
                    changes.push((m.id.clone(), want));
                }
            }
            (changes, now_seen)
        })
        .await
        else {
            return;
        };
        self.apply_spot(generation, changes, now_seen);
    }

    /// What a look for moves found, unless something newer happened while it looked (a move,
    /// an offer, a decline, a newer look): then that one's word stands.
    fn apply_spot(
        &self,
        generation: u64,
        changes: Vec<(String, Option<PathBuf>)>,
        now_seen: Vec<(String, Seen)>,
    ) {
        if self.moves_gen.load(std::sync::atomic::Ordering::SeqCst) != generation {
            return;
        }
        if !now_seen.is_empty() {
            self.repo_seen.locked().extend(now_seen);
            self.save_seen();
        }
        for (id, to) in changes {
            self.set_moved_to(&id, to);
        }
    }

    // ── the move ────────────────────────────────────────────────────────────

    /// `ProjectMove` (SPEC §10.1): see the module's documentation for what is checked first.
    pub(crate) async fn project_move(
        self: &Arc<Self>,
        addr: &str,
        path: PathBuf,
    ) -> Result<Reply, Error> {
        let p = self.find_project(addr)?;
        if let Some(m) = &p.machine {
            return Err(Error::invalid(format!("{} is on {m}: it moves there", p.slug)));
        }
        let typed = ptyro_core::config::expand_home(&path.to_string_lossy());
        let path = ptyro_core::os::canonicalize(&typed).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::invalid(format!("{} doesn't exist", typed.display()))
            } else {
                Error::invalid(format!("{}: {e}", typed.display()))
            }
        })?;
        // One add or move at a time: auto-registration must not register the place meanwhile.
        let _one = self.project_adding.lock().await;
        let p = self.project_by_id(&p.id).ok_or_else(|| Error::missing("project", addr))?;
        let worktrees: Vec<WorktreeInfo> = {
            let reg = self.registry.locked();
            reg.worktrees.iter().filter(|w| w.project == p.id).cloned().collect()
        };
        let recorded = self.repo_roots.locked().get(&p.id).cloned().unwrap_or_default();

        // What is at the new place, and what is left at the old one.
        enum Left {
            Gone,
            Checkout(Facts),
            Other,
        }
        let (old_root, probe) = (p.root.clone(), path.clone());
        let (new, old) = blocking(move || -> Result<(Facts, Left), Error> {
            let git = Git::new();
            let root = git.main_root(&probe).map_err(git_err)?;
            let root = ptyro_core::os::canonicalize(&root).unwrap_or(root);
            let new = Facts::of(&git, &root);
            let old = if !old_root.is_dir() || ptyro_core::os::same_path(&old_root, &root) {
                Left::Gone
            } else {
                match git.main_root(&old_root) {
                    Ok(r)
                        if ptyro_core::os::same_path(
                            &ptyro_core::os::canonicalize(&r).unwrap_or(r.clone()),
                            &old_root,
                        ) =>
                    {
                        Left::Checkout(Facts::of(&git, &old_root))
                    }
                    _ => Left::Other,
                }
            };
            Ok((new, old))
        })
        .await??;

        let mut notes = Vec::new();
        if !ptyro_core::os::same_path(&new.root, &path) {
            notes.push(format!(
                "{} is inside the repository at {}: {} moves there",
                path.display(),
                new.root.display(),
                p.slug
            ));
        }
        if ptyro_core::os::same_path(&new.root, &p.root) {
            return Ok(Reply::Project {
                info: p.clone(),
                notes: vec![format!("{} is already at {}", p.slug, p.root.display())],
            });
        }
        match &old {
            Left::Checkout(was) if one_repository(was, &new) => {
                return Err(Error::conflict(format!(
                    "{slug} is still at {old}, and {new} is the same repository: that's a copy, not a move. \
                     To use the copy, delete or rename {old} first (what it holds is yours: commit and push it, or carry it over); \
                     to keep both, add {new} as a project of its own",
                    slug = p.slug,
                    old = p.root.display(),
                    new = new.root.display(),
                )));
            }
            Left::Checkout(_) => notes
                .push(format!("{} holds another repository now: left as it is", p.root.display())),
            Left::Other => notes.push(format!(
                "{} is still there, but holds no git repository: left as it is",
                p.root.display()
            )),
            Left::Gone => {}
        }
        match same_repository(&p, &recorded, &worktrees, &new) {
            Same::Yes(_) => {}
            Same::No(why) => {
                return Err(Error::conflict(format!(
                    "{} isn't {}'s repository: {why}. A project is never pointed at another repository; \
                     to work on this one, add it as a project of its own",
                    new.root.display(),
                    p.slug
                )));
            }
            Same::Unknown => {
                return Err(Error::conflict(format!(
                    "ptyro can't tell that {new} is {slug}'s repository: there's no remote to compare (one of them has none), \
                     and ptyro kept no record of {slug}'s commits or worktrees (it was registered before ptyro kept them). \
                     If it is, forget {slug} and add {new} (its trust and pin start over)",
                    new = new.root.display(),
                    slug = p.slug
                )));
            }
        }
        let last_mount = self.repo_seen.locked().get(&p.id).map(|s| s.mount.clone());
        if remounted(&p.root, &new.root) {
            notes.push(format!(
                "{} looks like the drive {} was on, mounted again under another name: once it is back where it was, move {} back",
                new.root.display(),
                p.root.display(),
                p.slug
            ));
        } else if unmounted(&p.root, last_mount.as_deref(), &RealDisk) {
            notes.push(format!(
                "{} was on a drive that isn't mounted: if it comes back, that copy is no longer {}'s",
                p.root.display(),
                p.slug
            ));
        }

        // A project registered at the new place already: folded in.
        let dup = {
            let reg = self.registry.locked();
            reg.projects
                .iter()
                .find(|o| {
                    o.id != p.id
                        && o.machine.is_none()
                        && ptyro_core::os::same_path(&o.root, &new.root)
                })
                .cloned()
        };
        let dup_worktrees: Vec<WorktreeInfo> = match &dup {
            Some(d) => {
                let reg = self.registry.locked();
                reg.worktrees.iter().filter(|w| w.project == d.id).cloned().collect()
            }
            None => Vec::new(),
        };

        // Where each worktree is now.
        let root_id = worktrees.iter().find(|w| w.is_root).map(|w| w.id.clone());
        let mut moves: Vec<(PathBuf, PathBuf)> = vec![(p.root.clone(), new.root.clone())];
        let mut plan: Vec<WorktreeInfo> = Vec::new();
        for w in &worktrees {
            let mut w = w.clone();
            if w.is_root {
                w.path = new.root.clone();
            } else if let Some(there) = moved_path(&w.path, &p.root, &new.root) {
                if !ptyro_core::os::path_starts_with(&w.path, &p.root)
                    && let (Some(a), Some(b)) = (sibling(&p.root), sibling(&new.root))
                    && !moves.iter().any(|(o, _)| o == &a)
                {
                    moves.push((a, b));
                }
                w.path = there;
            }
            plan.push(w);
        }
        let mut dropped: Vec<(String, String)> = Vec::new();
        let mut carried: Vec<WorktreeInfo> = Vec::new();
        if let Some(d) = &dup {
            for w in &dup_worktrees {
                if w.is_root {
                    if let Some(r) = &root_id {
                        dropped.push((w.id.clone(), r.clone()));
                    }
                    continue;
                }
                if let Some(same) =
                    plan.iter().find(|o| ptyro_core::os::same_path(&o.path, &w.path))
                {
                    dropped.push((w.id.clone(), same.id.clone()));
                    continue;
                }
                let mut w = w.clone();
                w.project = p.id.clone();
                w.project_slug = p.slug.clone();
                let taken = |n: &str, plan: &[WorktreeInfo], carried: &[WorktreeInfo]| {
                    plan.iter().chain(carried.iter()).any(|o| lookup::same(&o.name, n))
                };
                if taken(&w.name, &plan, &carried) {
                    let mut k = 2;
                    let mut free = ptyro_core::names::numbered(&w.name, k);
                    while taken(&free, &plan, &carried) {
                        k += 1;
                        free = ptyro_core::names::numbered(&w.name, k);
                    }
                    notes.push(format!(
                        "{}'s worktree {} is {} now: {} has one by that name",
                        d.slug, w.name, free, p.slug
                    ));
                    w.name = free;
                }
                carried.push(w);
            }
        }

        // Link the worktrees to the new place (git's own repair), then see where each stands.
        let new_root = new.root.clone();
        let linked: Vec<(String, PathBuf)> = plan
            .iter()
            .chain(carried.iter())
            .filter(|w| {
                !w.is_root && matches!(w.state, WorktreeState::Ready | WorktreeState::Missing)
            })
            .map(|w| (w.qualified(), w.path.clone()))
            .collect();
        let records = new.linked.clone();
        let (repaired, broken, branch, default_branch) = blocking(move || {
            let git = Git::new();
            let (mut repaired, mut broken) = (Vec::new(), Vec::new());
            for (name, path) in linked {
                if !path.is_dir() || git.belongs(&path, &new_root) {
                    continue;
                }
                // The record its `.git` file names must be its own: never one another checkout
                // still uses (git would hand it over).
                let admin = ptyro_git::gitfile_target(&path)
                    .and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()));
                let record = admin.as_deref().and_then(|a| records.iter().find(|l| l.name == a));
                let canon = |p: &Path| ptyro_core::os::canonicalize(p).unwrap_or(p.to_path_buf());
                let taken = record.and_then(|l| l.path.clone()).filter(|lp| {
                    !ptyro_core::os::same_path(&canon(lp), &canon(&path))
                        && lp.join(".git").exists()
                });
                if record.is_none() || taken.is_some() {
                    let why = match taken {
                        Some(other) => format!(
                            "the repository's record of it belongs to {} now",
                            other.display()
                        ),
                        None => "the repository has no record of it any more".to_owned(),
                    };
                    broken.push((name, path, why));
                    continue;
                }
                let r = git.worktree_repair(&new_root, &path);
                if git.belongs(&path, &new_root) {
                    repaired.push(name);
                } else {
                    let why = r
                        .err()
                        .map_or_else(|| "it still points elsewhere".to_owned(), |e| e.to_string());
                    broken.push((name, path, why));
                }
            }
            (repaired, broken, git.current_branch(&new_root), git.default_branch(&new_root))
        })
        .await?;

        let ready = |w: &mut WorktreeInfo| {
            if matches!(w.state, WorktreeState::Ready | WorktreeState::Missing) {
                w.state =
                    if w.path.is_dir() { WorktreeState::Ready } else { WorktreeState::Missing };
            }
        };
        let mut missing = Vec::new();
        for w in plan.iter_mut().chain(carried.iter_mut()) {
            ready(w);
            if w.is_root {
                w.branch = branch.clone().or(w.branch.take());
                w.status = None;
            } else if w.state == WorktreeState::Missing {
                missing.push(format!("{} ({})", w.name, w.path.display()));
            }
        }

        // The project as it is now: what it had wins; a duplicate adds what it alone had.
        let mut np = p.clone();
        np.root = new.root.clone();
        np.remote_url = new.remote.clone().or(p.remote_url.clone());
        np.default_branch = default_branch.or(p.default_branch.clone());
        np.moved_to = None;
        let mut trust: Vec<TrustRow> = {
            let reg = self.registry.locked();
            reg.trust.iter().filter(|t| t.project == p.id).cloned().collect()
        };
        let mut gained = Vec::new();
        if let Some(d) = &dup {
            if np.pin.is_none() && d.pin.is_some() {
                np.pin.clone_from(&d.pin);
                gained.push(format!("its pin ({})", d.pin.as_deref().unwrap_or_default()));
            }
            np.last_used_ms = np.last_used_ms.max(d.last_used_ms);
            let theirs: Vec<TrustRow> = {
                let reg = self.registry.locked();
                reg.trust.iter().filter(|t| t.project == d.id).cloned().collect()
            };
            // What it had wins (owner, 2026-11-19): only a scope it has no approval for at all
            // takes the duplicate's.
            for t in theirs {
                if !trust.iter().any(|o| o.scope == t.scope) {
                    trust.push(TrustRow { project: p.id.clone(), ..t });
                    gained.push("its trust".to_owned());
                }
            }
        }
        let workspaces: Vec<ptyro_core::model::WorkspaceInfo> = match &dup {
            Some(d) => {
                let reg = self.registry.locked();
                reg.workspaces
                    .iter()
                    .filter(|w| w.projects.contains(&d.id))
                    .map(|w| {
                        let mut w = w.clone();
                        if w.projects.contains(&p.id) {
                            w.projects.retain(|x| *x != d.id);
                        } else {
                            gained.push(format!("its place in {}", w.name));
                            for x in &mut w.projects {
                                if *x == d.id {
                                    x.clone_from(&p.id);
                                }
                            }
                        }
                        w
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        let rows: Vec<WorktreeInfo> = plan.iter().chain(carried.iter()).cloned().collect();
        let as_text = |(a, b): &(PathBuf, PathBuf)| {
            (a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned())
        };

        // Written all at once, or not at all.
        if let Some(store) = &self.store {
            store
                .relocate(ptyro_store::Relocation {
                    project: np.clone(),
                    worktrees: rows.clone(),
                    absorbed: dup.as_ref().map(|d| d.id.clone()),
                    dropped: dropped.clone(),
                    trust: trust.clone(),
                    moves: moves.iter().map(as_text).collect(),
                })
                .await
                .map_err(|e| {
                    Error::internal(store_failed(&e.to_string(), &p, &new.root, &repaired))
                })?;
        }
        let old_ids: Vec<String> =
            worktrees.iter().chain(dup_worktrees.iter()).map(|w| w.id.clone()).collect();
        {
            let mut reg = self.registry.locked();
            if let Some(d) = &dup {
                reg.projects.retain(|o| o.id != d.id);
                reg.worktrees.retain(|w| w.project != d.id);
                reg.trust.retain(|t| t.project != d.id);
            }
            reg.trust.retain(|t| t.project != p.id);
            reg.trust.extend(trust);
            if let Some(o) = reg.projects.iter_mut().find(|o| o.id == np.id) {
                *o = np.clone();
            }
            for w in &rows {
                match reg.worktrees.iter_mut().find(|o| o.id == w.id) {
                    Some(o) => *o = w.clone(),
                    None => reg.worktrees.push(w.clone()),
                }
            }
            for w in &workspaces {
                if let Some(o) = reg.workspaces.iter_mut().find(|o| o.id == w.id) {
                    *o = w.clone();
                }
            }
        }
        // A look for moves under way saw it as it was.
        self.moves_changed();
        self.save_offered();
        if let Some(d) = &dup {
            let _ = self.events.send(DomainEvent::ProjectRemoved { id: d.id.clone() });
            for (from, _) in &dropped {
                let _ = self.events.send(DomainEvent::WorktreeRemoved { id: from.clone() });
                self.forget_launch(from).await;
                self.forget_kept(from).await;
            }
            self.forget_moves(&d.id);
        }
        let _ = self.events.send(DomainEvent::ProjectUpdated { info: np.clone() });
        for w in &rows {
            let _ = self.events.send(DomainEvent::WorktreeUpdated { info: w.clone() });
        }
        for w in workspaces {
            // Saved too: the duplicate's memberships went with it.
            self.put_workspace(w);
        }

        // Sessions: the duplicate's are this project's now; a directory they started in moved.
        let dropped_map: HashMap<&str, &str> =
            dropped.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let mut live = 0;
        let mut theirs = 0;
        let ours: HashSet<&str> = rows.iter().map(|w| w.id.as_str()).collect();
        for s in self.sessions() {
            let changed = {
                let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut changed = false;
                if let Some(d) = &dup
                    && m.place.project.as_deref() == Some(d.id.as_str())
                {
                    m.place.project = Some(p.id.clone());
                    theirs += 1;
                    changed = true;
                }
                if let Some(into) = m.place.worktree.as_deref().and_then(|w| dropped_map.get(w)) {
                    m.place.worktree = Some((*into).to_owned());
                    changed = true;
                }
                if let Some(cwd) = moves.iter().find_map(|(a, b)| rebased(&m.cwd, a, b)) {
                    m.cwd = cwd;
                    changed = true;
                }
                if m.state.is_live()
                    && m.place.worktree.as_deref().is_some_and(|w| ours.contains(w))
                {
                    live += 1;
                }
                changed
            };
            if changed {
                s.save(None);
                let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
            }
        }

        // What follows the new place: watches, status, the memory of who it is.
        for id in &old_ids {
            self.unwatch(id);
        }
        for w in &rows {
            if w.state == WorktreeState::Ready {
                self.watch_worktree_bg(w);
            }
            let _ = self.status_tx.send(w.id.clone());
        }
        self.undecline(&new.root);
        if !new.roots.is_empty() {
            self.repo_roots.locked().insert(p.id.clone(), new.roots.clone());
            self.save_roots();
        }
        {
            let before = self.not_moved.locked().len();
            self.not_moved.locked().retain(|(id, _)| *id != p.id);
            if self.not_moved.locked().len() != before {
                self.save_not_moved();
            }
        }
        self.spot_moves_bg();
        tracing::info!(project = %p.slug, from = %p.root.display(), to = %np.root.display(), "moved");

        notes.insert(0, format!("moved {} to {}", p.slug, np.root.display()));
        if let Some(d) = &dup {
            let mut what = vec![plural(carried.len(), "worktree", "worktrees")];
            if theirs > 0 {
                what.push(plural(theirs, "session", "sessions"));
            }
            what.extend(gained);
            notes.push(format!(
                "{} (registered there on its own) is folded into {}: {}",
                d.slug,
                p.slug,
                what.join(", ")
            ));
        }
        if !repaired.is_empty() {
            notes.push(format!(
                "linked {} to the new place: {}",
                plural(repaired.len(), "worktree", "worktrees"),
                repaired.join(", ")
            ));
        }
        for (name, path, why) in broken {
            notes.push(format!(
                "{name}: git couldn't link it to the new place ({why}); its files are still in {}",
                path.display()
            ));
        }
        if !missing.is_empty() {
            notes.push(format!(
                "not found where they were, so still missing: {}",
                missing.join(", ")
            ));
        }
        if live > 0 {
            notes.push(format!(
                "{} running where {} started; new ones start in {}",
                plural(live, "running session keeps", "running sessions keep"),
                if live == 1 { "it" } else { "they" },
                np.root.display()
            ));
        }
        Ok(Reply::Project { info: np, notes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &str, remote: Option<&str>) -> ProjectInfo {
        serde_json::from_value(serde_json::json!({
            "id": "p1", "slug": "woa", "root": root, "created_ms": 0, "remote_url": remote,
        }))
        .unwrap()
    }

    fn facts(roots: &[&str], remote: Option<&str>, linked: Vec<Linked>) -> Facts {
        Facts {
            root: "/new/woa".into(),
            roots: roots.iter().map(|r| (*r).to_owned()).collect(),
            remote: remote.map(str::to_owned),
            linked,
        }
    }

    /// The rules, one by one: the repository's own records prove it; a shared root commit or the
    /// same remote are enough; another history or another remote refuse; nothing to tell by is
    /// no.
    #[test]
    fn same_repository_needs_proof_and_no_contradiction() {
        let gh = Some("git@github.com:me/woa.git");
        let p = project("/old/woa", gh);
        let same = |recorded: &[&str], f: &Facts| {
            let rec: Vec<String> = recorded.iter().map(|r| (*r).to_owned()).collect();
            same_repository(&p, &rec, &[], f)
        };
        assert_eq!(same(&["a"], &facts(&["a"], gh, vec![])), Same::Yes(Proof::History));
        assert_eq!(
            same(&[], &facts(&["a"], Some("https://github.com/me/woa"), vec![])),
            Same::Yes(Proof::Remote),
            "the remote however it's spelled"
        );
        assert!(
            matches!(same(&["a"], &facts(&["b"], gh, vec![])), Same::No(w) if w.contains("history"))
        );
        assert!(
            matches!(same(&["a"], &facts(&["a"], Some("git@github.com:fork/woa"), vec![])), Same::No(w) if w.contains("remote")),
            "a fork shares the history, not the repository"
        );
        let marker =
            Linked { name: "feat".into(), project: Some("p1".into()), ..Linked::default() };
        assert_eq!(
            same(&[], &facts(&[], Some("git@github.com:renamed/woa"), vec![marker])),
            Same::Yes(Proof::Records),
            "its records name this project: the very repository, whatever its remote says now"
        );
        let inside = Linked {
            name: "fix".into(),
            path: Some("/old/woa/.ptyro/worktrees/fix".into()),
            ..Linked::default()
        };
        assert_eq!(same(&[], &facts(&[], None, vec![inside.clone()])), Same::Yes(Proof::Records));
        assert!(
            matches!(same(&["a"], &facts(&["b"], None, vec![inside])), Same::No(_)),
            "another history refuses even so"
        );
        let local = project("/old/woa", None);
        assert_eq!(same_repository(&local, &[], &[], &facts(&["a"], None, vec![])), Same::Unknown);
        assert_eq!(same_repository(&local, &[], &[], &facts(&["a"], gh, vec![])), Same::Unknown);
    }

    #[test]
    fn worktrees_follow_the_repository_when_they_were_in_it() {
        let (old, new) = (Path::new("/a/woa"), Path::new("/b/woa"));
        assert_eq!(
            moved_path(Path::new("/a/woa/.ptyro/worktrees/x"), old, new),
            Some(PathBuf::from("/b/woa/.ptyro/worktrees/x"))
        );
        assert_eq!(moved_path(Path::new("/data/worktrees/woa/x"), old, new), None, "stays");
        assert_eq!(moved_path(Path::new("/a/woa.worktrees/x"), old, new), None, "not there");
        assert_eq!(rebased(Path::new("/a/woa"), old, new), Some(PathBuf::from("/b/woa")));
        assert_eq!(rebased(Path::new("/a/woa/src"), old, new), Some(PathBuf::from("/b/woa/src")));
        assert_eq!(rebased(Path::new("/a/woa-2"), old, new), None, "a neighbour isn't inside");
        assert_eq!(sibling(old), Some(PathBuf::from("/a/woa.worktrees")));
    }

    /// A file system made up: each directory there, its device, and whether it's empty.
    struct Fake(HashMap<PathBuf, (u64, bool)>);

    impl Fake {
        fn new(dirs: &[(&str, u64, bool)]) -> Self {
            Fake(dirs.iter().map(|(p, d, e)| (PathBuf::from(p), (*d, *e))).collect())
        }
    }

    impl Disk for Fake {
        fn device(&self, path: &Path) -> Option<u64> {
            self.0.get(path).map(|(d, _)| *d)
        }
        fn empty(&self, path: &Path) -> bool {
            self.0.get(path).is_some_and(|(_, e)| *e)
        }
    }

    /// Unmounted is told by the drive: no drive mounted where the checkout's drive was (gone,
    /// or a mount point left behind on its parent's device) is unmounted; a drive there whose
    /// checkout is gone is a candidate for a move. The owner's: `/Volumes/WALRUS_EXTERNAL` left
    /// behind by an unclean eject, the drive at `/Volumes/WALRUS_EXTERNAL 1`.
    #[test]
    fn a_drive_that_isnt_mounted_is_told_by_its_device() {
        let root = Path::new("/Volumes/WALRUS_EXTERNAL/dev/app");
        let ext = Some(Path::new("/Volumes/WALRUS_EXTERNAL"));
        let base = [("/", 1, false), ("/Volumes", 1, false), ("/Users", 1, false)];
        let with = |more: &[(&str, u64, bool)]| {
            Fake::new(&base.iter().chain(more.iter()).copied().collect::<Vec<_>>())
        };
        // Mounted, the checkout gone from it: a move.
        let fs = with(&[
            ("/Volumes/WALRUS_EXTERNAL", 7, false),
            ("/Volumes/WALRUS_EXTERNAL/dev", 7, false),
        ]);
        assert!(!unmounted(root, ext, &fs));
        // Ejected cleanly: its mount point is gone.
        assert!(unmounted(root, ext, &with(&[])));
        // Ejected uncleanly: an empty directory left behind, on its parent's device.
        let fs = with(&[
            ("/Volumes/WALRUS_EXTERNAL", 1, true),
            ("/Volumes/WALRUS_EXTERNAL 1", 8, false),
        ]);
        assert!(unmounted(root, ext, &fs), "a mount point left behind");
        // An fstab mount that didn't happen: /mnt/data, /data are empty directories.
        let fs = with(&[("/mnt", 1, false), ("/mnt/data", 1, true), ("/data", 1, true)]);
        assert!(unmounted(Path::new("/mnt/data/src/app"), Some(Path::new("/mnt/data")), &fs));
        assert!(unmounted(Path::new("/data/src/app"), Some(Path::new("/data")), &fs));
        // On the system's own drive: `/` is always mounted.
        assert!(!unmounted(Path::new("/Users/me/dev/app"), Some(Path::new("/")), &with(&[])));

        // Never seen by this ptyro: by what is left.
        assert!(unmounted(root, None, &with(&[])), "nothing left above /Volumes");
        let fs = with(&[("/Volumes/WALRUS_EXTERNAL", 1, true)]);
        assert!(unmounted(root, None, &fs), "an empty mount point left behind");
        let fs = with(&[("/mnt", 1, false), ("/mnt/data", 1, true), ("/data", 1, true)]);
        assert!(unmounted(Path::new("/mnt/data/src/app"), None, &fs));
        assert!(unmounted(Path::new("/data/src/app"), None, &fs));
        assert!(unmounted(
            Path::new("/media/me/NOT-A-DRIVE/woa"),
            None,
            &with(&[("/media", 1, false)])
        ));
        let fs = with(&[("/Users/me", 1, false), ("/Users/me/dev", 1, true)]);
        assert!(
            !unmounted(Path::new("/Users/me/dev/app"), None, &fs),
            "a folder emptied by the move is no mount point"
        );
        let fs = with(&[
            ("/Volumes/WALRUS_EXTERNAL", 7, false),
            ("/Volumes/WALRUS_EXTERNAL/dev", 7, false),
        ]);
        assert!(!unmounted(root, None, &fs), "mounted, the checkout gone");

        // Where it was seen: the drive's mount point.
        let fs = with(&[
            ("/Volumes/WALRUS_EXTERNAL", 7, false),
            ("/Volumes/WALRUS_EXTERNAL/dev", 7, false),
            ("/Volumes/WALRUS_EXTERNAL/dev/app", 7, false),
        ]);
        assert_eq!(mount_of(root, &fs), Some(PathBuf::from("/Volumes/WALRUS_EXTERNAL")));
        let fs = with(&[("/Users/me", 1, false), ("/Users/me/app", 1, false)]);
        assert_eq!(mount_of(Path::new("/Users/me/app"), &fs), Some(PathBuf::from("/")));
        assert_eq!(mount_of(Path::new("/nowhere"), &fs), None);

        // The real file system: a temporary directory's drive is mounted.
        let tmp = tempfile::tempdir().unwrap();
        let m = mount_of(tmp.path(), &RealDisk).unwrap();
        assert!(!unmounted(&tmp.path().join("gone/woa"), Some(&m), &RealDisk));
    }

    /// A drive mounted again under another name is the same drive, not a move.
    #[test]
    fn a_drive_mounted_again_under_another_name_is_no_move() {
        let r = |a: &str, b: &str| remounted(Path::new(a), Path::new(b));
        assert!(r("/Volumes/WALRUS_EXTERNAL/dev/app", "/Volumes/WALRUS_EXTERNAL 1/dev/app"));
        assert!(r("/Volumes/EXT/app", "/Volumes/EXT 12/app"));
        assert!(r("/media/me/EXT/app", "/media/me/EXT1/app"), "udisks' way");
        assert!(r("/run/media/me/EXT/app", "/run/media/me/EXT2/app"));
        assert!(!r("/Volumes/EXT/dev/app", "/Volumes/OTHER/dev/app"), "another drive");
        assert!(!r("/Volumes/EXT/dev/app", "/Volumes/EXT 1/work/app"), "moved on it too");
        assert!(!r("/Users/me/app", "/Users/me/app 2"), "not where drives mount");
        assert!(!r("/Users/me/dev/app", "/Users/me/dev2/app"));
        assert!(!r("/Volumes/EXT/app", "/Volumes/EXT/app"));
        assert!(!r("/Volumes/EXT/app", "/Volumes/EXT backup/app"));
    }

    /// The database failing after git linked worktrees says so, and how to finish.
    #[test]
    fn a_failed_database_write_says_what_git_changed() {
        let p = project("/old/woa", None);
        let to = Path::new("/new/woa");
        let none = store_failed("disk full", &p, to, &[]);
        assert!(none.contains("disk full") && none.contains("nothing was changed"), "{none}");
        let some = store_failed("disk full", &p, to, &["woa/feat".into(), "woa/fix".into()]);
        assert!(!some.contains("nothing was changed"), "{some}");
        assert!(
            some.contains("woa still points at /old/woa")
                && some.contains("git already linked 2 worktrees to /new/woa: woa/feat, woa/fix")
                && some.contains("Move woa there again to finish"),
            "{some}"
        );
    }

    /// A look for moves that finishes after a move, an offer, a decline or a newer look changes
    /// nothing: what it saw is stale.
    #[tokio::test]
    async fn a_stale_look_for_moves_changes_nothing() {
        let reg = ptyro_store::Registry {
            projects: vec![project("/old/woa", None)],
            ..ptyro_store::Registry::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(dir.path(), reg);
        let started = d.moves_gen.load(std::sync::atomic::Ordering::SeqCst);
        let p = d.project_by_id("p1").unwrap();
        d.decline_move(&p, Path::new("/new/woa"));
        d.apply_spot(started, vec![("p1".into(), Some("/new/woa".into()))], Vec::new());
        assert_eq!(d.project_by_id("p1").unwrap().moved_to, None, "declined meanwhile");
        let now = d.moves_gen.load(std::sync::atomic::Ordering::SeqCst);
        d.apply_spot(now, vec![("p1".into(), Some("/new/woa".into()))], Vec::new());
        assert_eq!(d.project_by_id("p1").unwrap().moved_to, Some("/new/woa".into()), "current");
    }
}
