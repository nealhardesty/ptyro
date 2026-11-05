//! SHELLS (SPEC §23, ROADMAP SH): a workspace's numbered terminals. A *loose* session belongs to
//! its workspace instead of a worktree; it has a number (its slot) that stays until it goes, is
//! never placed under the worktree its directory is in, and outlives every worktree. Here: the
//! numbers, `%N` addresses, moving between SHELLS and worktrees, watches and long commands (a
//! one-second tick), scratch directories, and what comes back after a daemon restart.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::config::ShellsRestore;
use ptyro_core::model::{MAIN_WORKSPACE, WorktreeInfo};
use ptyro_core::session::{ProcessState, SessionKind, Watch};
use ptyro_core::status::AttentionKind;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, SessionId, TermSize, now_ms};
use ptyro_protocol::{DomainEvent, Reply, Request, ScreenFormat};
use serde::{Deserialize, Serialize};

use crate::daemon::Daemon;
use crate::session::{Session, Shelf};

/// The kv key of the loose sessions to offer again after a restart (SPEC §23.10).
const KEPT: &str = "shells:kept";
const TICK: Duration = Duration::from_secs(1);

/// Numbers handed out but whose session isn't in the list yet, `(workspace, slot)`, so two
/// shells started at once never get the same one.
static RESERVED: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
/// A new session's place in SHELLS, waiting for `launch` to put it on the session before anyone
/// can see it (so the auto-register sweep never finds it unplaced).
static PENDING: Mutex<BTreeMap<SessionId, Shelf>> = Mutex::new(BTreeMap::new());

/// A number held for a session being started; given back when dropped (started or not).
pub(crate) struct Reservation {
    key: (String, u32),
}

impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.locked().retain(|k| *k != self.key);
    }
}

/// A scratch directory made for a spawn that may still be refused: dropped while still set, it
/// goes again (only while empty: `remove_dir` never takes what someone put there).
pub(crate) struct ScratchGuard(pub(crate) Option<PathBuf>);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Everything under `dir` (not following links): (entries, bytes, newest change in ms). Stops
/// counting after 100,000 entries: a leftover that big is reported as at least that.
fn tally(dir: &Path) -> (u64, u64, u64) {
    let (mut files, mut bytes, mut newest) = (0u64, 0u64, 0u64);
    let mut todo = vec![dir.to_path_buf()];
    let stamp = |m: &std::fs::Metadata| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64)
    };
    if let Ok(m) = std::fs::symlink_metadata(dir) {
        newest = stamp(&m);
    }
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.path().symlink_metadata() else { continue };
            files += 1;
            bytes += m.len();
            newest = newest.max(stamp(&m));
            if m.is_dir() {
                todo.push(e.path());
            }
            if files >= 100_000 {
                return (files, bytes, newest);
            }
        }
    }
    (files, bytes, newest)
}

/// The lowest number from 1 that isn't in `taken`.
fn lowest_free(taken: &[u32]) -> u32 {
    let mut n = 1;
    while taken.contains(&n) {
        n += 1;
    }
    n
}

/// `launch` takes the shelf a new session was given (`None`: not loose).
pub(crate) fn take_pending(id: SessionId) -> Option<Shelf> {
    PENDING.locked().remove(&id)
}

/// What a loose session was, kept so a restart can offer it again (SPEC §23.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    id: String,
    name: String,
    #[serde(default)]
    title: Option<String>,
    kind: SessionKind,
    argv: Vec<String>,
    /// Where it was last: the shell's report, else where it started.
    cwd: PathBuf,
    workspace: String,
    profile: String,
    slot: u32,
    #[serde(default)]
    client_env: BTreeMap<String, String>,
    #[serde(default)]
    pinned: bool,
}

/// What the tick remembers about one loose session between looks.
#[derive(Default)]
struct Seen {
    /// The end of the last command it has told about (`ShellCommand.ended_ms`).
    ended: Option<u64>,
    /// Output (bytes) when last looked at, for `output` and `match`.
    out: u64,
    /// When `silence` last fired (`last_output_ms` then), so it fires once per quiet spell.
    quiet_at: Option<u64>,
    re: Option<(String, regex::Regex)>,
}

impl Daemon {
    /// The workspace a bare `%N` means and a loose session goes to when nothing says: the one
    /// the TUI showed last, else `main`.
    pub(crate) fn context_workspace(&self) -> String {
        let reg = self.registry.locked();
        self.shown_workspace
            .locked()
            .clone()
            .filter(|n| reg.workspaces.iter().any(|w| w.name == *n))
            .unwrap_or_else(|| MAIN_WORKSPACE.into())
    }

    /// Every loose session of `workspace` (live, exited, lost), with its number.
    pub(crate) fn shells_of(&self, workspace: &str) -> Vec<(Arc<Session>, u32)> {
        let mut out: Vec<(Arc<Session>, u32)> = self
            .sessions()
            .into_iter()
            .filter_map(|s| {
                let slot = {
                    let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let ws = m.place.workspace.as_deref().unwrap_or(MAIN_WORKSPACE);
                    m.shelf.as_ref().filter(|_| ws == workspace).map(|f| f.slot)
                };
                slot.map(|n| (s, n))
            })
            .collect();
        out.sort_by_key(|(_, n)| *n);
        out
    }

    /// The loose session numbered `slot` in `workspace`.
    pub(crate) fn shell_at(&self, workspace: &str, slot: u32) -> Option<Arc<Session>> {
        self.shells_of(workspace).into_iter().find(|(_, n)| *n == slot).map(|(s, _)| s)
    }

    /// The lowest number free in `workspace` (counting ones being handed out).
    fn free_slot(&self, workspace: &str, reserved: &[(String, u32)]) -> u32 {
        let taken: Vec<u32> = self
            .shells_of(workspace)
            .into_iter()
            .map(|(_, n)| n)
            .chain(reserved.iter().filter(|(w, _)| w == workspace).map(|(_, n)| *n))
            .collect();
        lowest_free(&taken)
    }

    /// Hold a number for a session about to start in `workspace`: `want`, else the lowest free.
    /// A taken `want` is refused, naming the shell that has it.
    pub(crate) fn reserve_slot(
        &self,
        workspace: &str,
        want: Option<u32>,
    ) -> Result<Reservation, Error> {
        let mut reserved = RESERVED.locked();
        let slot = match want {
            Some(0) => return Err(Error::invalid("shells are numbered from 1")),
            Some(n) => {
                let held = reserved.iter().any(|k| k.0 == workspace && k.1 == n);
                if let Some(s) = self.shell_at(workspace, n) {
                    let name = s
                        .meta
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .name
                        .clone();
                    return Err(Error::conflict(format!(
                        "{workspace} has a shell {n} already ({name}): `ptyro shells go {n}` shows it"
                    )));
                }
                if held {
                    return Err(Error::conflict(format!("shell {n} of {workspace} is starting")));
                }
                n
            }
            None => self.free_slot(workspace, &reserved),
        };
        let key = (workspace.to_owned(), slot);
        reserved.push(key.clone());
        Ok(Reservation { key })
    }

    /// `launch` will give session `id` this place in SHELLS.
    pub(crate) fn pend_shelf(
        &self,
        id: SessionId,
        r: &Reservation,
        scratch: Option<PathBuf>,
        pinned: bool,
    ) {
        PENDING.locked().insert(id, Shelf { slot: r.key.1, scratch, pinned, ..Shelf::default() });
    }

    /// Make a session loose in its home workspace with the next free number (SPEC §23.3): it
    /// lost its place (its project let go, its worktree gone), or was set loose by hand.
    /// Quick terminals, the scratchpad's editor and chats' engines never are. `false`: it isn't.
    pub(crate) fn shelve(&self, s: &Arc<Session>, pinned: bool) -> bool {
        let ws = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(f) = m.shelf.as_mut() {
                f.pinned |= pinned;
                return true;
            }
            if m.place.quick || m.place.pad || m.place.chat.is_some() {
                return false;
            }
            m.place.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into())
        };
        let Ok(r) = self.reserve_slot(&ws, None) else { return false };
        {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.shelf = Some(Shelf { slot: r.key.1, pinned, ..Shelf::default() });
            m.place.worktree = None;
            m.place.project = None;
            if m.place.workspace.is_none() {
                m.place.workspace = Some(ws.clone());
            }
        }
        drop(r);
        s.save(None);
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        tracing::info!("{name} is shell {} of {ws} now", s.slot().unwrap_or_default());
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        true
    }

    /// The shells that just moved into another workspace (a removed workspace's, to `main`) keep
    /// their numbers where those are free there and take the lowest free one where not; the
    /// shells already there never change (SPEC §23.3).
    pub(crate) fn reslot(&self, moved: &[SessionId]) {
        let _held = RESERVED.locked();
        let mut movers: Vec<Arc<Session>> =
            self.sessions().into_iter().filter(|s| moved.contains(&s.id)).collect();
        movers.sort_by_key(|s| s.id.created_ms());
        for s in movers {
            let (ws, slot) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let ws = m.place.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into());
                match m.shelf.as_ref() {
                    Some(f) => (ws, f.slot),
                    None => continue,
                }
            };
            // Taken by a shell that is settled there: every other one, less the movers still to
            // come (their numbers aren't theirs yet).
            let taken: Vec<u32> = self
                .shells_of(&ws)
                .into_iter()
                .filter(|(o, _)| o.id != s.id)
                .filter(|(o, _)| !moved.contains(&o.id) || o.id.created_ms() < s.id.created_ms())
                .map(|(_, n)| n)
                .collect();
            if !taken.contains(&slot) {
                continue;
            }
            let n = lowest_free(&taken);
            if let Some(f) =
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf.as_mut()
            {
                f.slot = n;
            }
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        }
    }

    /// `%N`, `ws%N` (SPEC §23.5): shell N of the context (or named) workspace. `None`: `addr`
    /// isn't one of those.
    pub(crate) fn resolve_slot(&self, addr: &str) -> Option<Result<Arc<Session>, Error>> {
        let (ws, n) = addr.rsplit_once('%')?;
        let n: u32 = n.parse().ok()?;
        let ws = match ws {
            "" => self.context_workspace(),
            // `s:<id>%N` (the CLI inside a session): that session's workspace.
            w if w.starts_with("s:") && self.find_workspace(w).is_err() => match self.resolve(w) {
                Ok(s) => s
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .place
                    .workspace
                    .clone()
                    .unwrap_or_else(|| MAIN_WORKSPACE.into()),
                Err(e) => return Some(Err(e)),
            },
            w => w.to_owned(),
        };
        Some(self.shell_at(&ws, n).ok_or_else(|| {
            Error::new(
                ErrorCode::NotFound,
                format!("no shell {n} in {ws}: `ptyro shells new --slot {n}` starts one"),
            )
        }))
    }

    /// A bare `%N` in a request from a command running inside a session (the CLI, an agent's
    /// tool call) is that session's workspace's shell N: rewritten to `s:<id>%N`, which
    /// `resolve_slot` reads (SPEC §23.5). Others, and clients outside any session, are left as
    /// they are: there `%N` is the workspace the TUI shows.
    pub(crate) fn scope_slots(&self, req: Request, caller: &crate::mail::Caller) -> Request {
        use ptyro_protocol::ClientKind as K;
        let Some(pid) =
            caller.pid.filter(|_| matches!(caller.kind, Some(K::Cli | K::Hook | K::Test)))
        else {
            return req;
        };
        let Ok(mut v) = serde_json::to_value(&req) else { return req };
        fn bare(s: &str) -> bool {
            s.strip_prefix('%')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        let mut found = false;
        if let Some(o) = v.as_object() {
            for k in ["addr", "session", "target"] {
                found |= o.get(k).and_then(|x| x.as_str()).is_some_and(bare);
            }
        }
        if !found {
            return req;
        }
        let Some(me) = self.session_of_pid(pid) else { return req };
        if let Some(o) = v.as_object_mut() {
            for k in ["addr", "session", "target"] {
                if let Some(x) = o.get_mut(k)
                    && let Some(s) = x.as_str().filter(|s| bare(s))
                {
                    *x = serde_json::Value::String(format!("s:{}{s}", me.id.full()));
                }
            }
        }
        serde_json::from_value(v).unwrap_or(req)
    }

    /// Sessions that keep a worktree from going (SPEC §10.8, §23.8): its own that are busy, and
    /// any loose one whose directory is inside it now.
    pub(crate) fn busy_in(&self, wt: &WorktreeInfo) -> Vec<Arc<Session>> {
        let mut out: Vec<Arc<Session>> =
            self.sessions_in(&wt.id).into_iter().filter(|s| s.busy()).collect();
        // A shell reports its directory as the system sees it (macOS: /private/var for /var).
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let root = canon(&wt.path);
        for s in self.sessions() {
            let loose =
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf.is_some();
            if !loose || !s.is_live() || out.iter().any(|o| o.id == s.id) {
                continue;
            }
            let info = self.info(&s);
            let here = info.live_cwd.as_deref().unwrap_or(&info.cwd);
            if canon(here).starts_with(&root) {
                out.push(s);
            }
        }
        out
    }

    /// A new, empty scratch directory for a shell (SPEC §23.8), 0700.
    pub(crate) fn make_scratch(&self, id: SessionId) -> Result<PathBuf, Error> {
        let dir = self.paths.data.join("scratch").join(id.full());
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::internal(format!("scratch directory {}: {e}", dir.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(dir)
    }

    /// A removed shell's scratch directory goes with it, unless something is left in it: then it
    /// stays, and the log says where (never deleting what someone made).
    pub(crate) fn drop_scratch(&self, dir: &Path) {
        if !dir.starts_with(self.paths.data.join("scratch")) {
            return;
        }
        let empty = std::fs::read_dir(dir).map(|mut d| d.next().is_none()).unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(dir);
        } else {
            tracing::info!("kept the scratch directory {}: it still has files", dir.display());
        }
    }

    /// Scratch directories no shell uses any more (SPEC §23.8): what `gc` lists, newest first.
    pub(crate) async fn scratch_leftovers(&self) -> Vec<ptyro_core::model::ScratchLeftover> {
        let root = self.paths.data.join("scratch");
        let used: Vec<PathBuf> = self
            .sessions()
            .iter()
            .filter_map(|s| {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                m.shelf.as_ref().and_then(|f| f.scratch.clone())
            })
            .collect();
        let mut out = tokio::task::spawn_blocking(move || {
            let Ok(dirs) = std::fs::read_dir(&root) else { return Vec::new() };
            dirs.flatten()
                .map(|d| d.path())
                .filter(|p| p.is_dir() && !used.contains(p))
                .map(|path| {
                    let (files, bytes, modified_ms) = tally(&path);
                    ptyro_core::model::ScratchLeftover { path, files, bytes, modified_ms }
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        out.sort_by_key(|l| std::cmp::Reverse(l.modified_ms));
        out
    }

    /// `ShellScratchRemove`: a leftover scratch directory, and what's in it, deleted (asked
    /// first by every client). Only directly under `<data>/scratch`, and never one in use.
    fn scratch_remove(&self, path: &Path) -> Result<Reply, Error> {
        let root = self.paths.data.join("scratch");
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let (root, dir) = (canon(&root), canon(path));
        if dir.parent() != Some(root.as_path()) || !dir.is_dir() {
            return Err(Error::invalid(format!(
                "{} isn't a scratch shell's directory (they live in {})",
                path.display(),
                root.display()
            )));
        }
        let in_use = self.sessions().iter().any(|s| {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.shelf.as_ref().and_then(|f| f.scratch.as_deref()).is_some_and(|d| canon(d) == dir)
        });
        if in_use {
            return Err(Error::conflict(format!(
                "{} is a running scratch shell's directory: stop that shell first",
                dir.display()
            )));
        }
        let (files, bytes, _) = tally(&dir);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| Error::internal(format!("deleting {}: {e}", dir.display())))?;
        Ok(Reply::Removed {
            notes: vec![format!(
                "deleted {} ({files} files, {})",
                dir.display(),
                ptyro_core::model::human_bytes(bytes)
            )],
        })
    }

    /// `ShellMove`, `ShellPlace`, `ShellWatch`.
    pub(crate) async fn handle_shells(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::ShellMove { addr, slot } => self.shell_move(&addr, slot),
            Request::ShellPlace { addr, loose, worktree } => {
                self.shell_place(&addr, loose, worktree.as_deref())
            }
            Request::ShellWatch { addr, watch } => self.shell_watch(&addr, watch),
            Request::ShellScratchRemove { path } => self.scratch_remove(&path),
            _ => Err(Error::internal("not a shells request")),
        }
    }

    /// Give a loose session another number; one already there swaps with it.
    fn shell_move(&self, addr: &str, slot: u32) -> Result<Reply, Error> {
        if slot == 0 {
            return Err(Error::invalid("shells are numbered from 1"));
        }
        let s = self.resolve(addr)?;
        let (ws, from) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(f) = &m.shelf else {
                return Err(Error::invalid(format!(
                    "{} isn't in SHELLS: `ptyro shells loose {}` moves it there",
                    m.name, m.name
                )));
            };
            (m.place.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into()), f.slot)
        };
        if from == slot {
            return Ok(Reply::Session { info: self.info(&s) });
        }
        let held = RESERVED.locked();
        if held.iter().any(|k| k.0 == ws && k.1 == slot) {
            return Err(Error::conflict(format!("shell {slot} of {ws} is starting: try again")));
        }
        let other = self.shell_at(&ws, slot);
        let set = |t: &Arc<Session>, n: u32| {
            if let Some(f) =
                t.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf.as_mut()
            {
                f.slot = n;
            }
        };
        set(&s, slot);
        if let Some(o) = &other {
            set(o, from);
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(o) });
        }
        drop(held);
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// Set loose, or adopt into a worktree (SPEC §23.8).
    fn shell_place(&self, addr: &str, loose: bool, worktree: Option<&str>) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let (name, is_loose, special) = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                m.name.clone(),
                m.shelf.is_some(),
                m.place.quick || m.place.pad || m.place.chat.is_some(),
            )
        };
        if loose {
            if special {
                return Err(Error::invalid(format!(
                    "{name} is a quick terminal or the scratchpad's editor: it stays where it is"
                )));
            }
            self.shelve(&s, true);
            return Ok(Reply::Session { info: self.info(&s) });
        }
        if !is_loose {
            return Err(Error::invalid(format!(
                "{name} isn't in SHELLS: it has a worktree already"
            )));
        }
        let info = self.info(&s);
        let here = info.live_cwd.clone().unwrap_or_else(|| info.cwd.clone());
        let (wt, project) = match worktree {
            Some(a) => {
                let w = self.find_worktree(a)?;
                let p = self.project_by_id(&w.project).ok_or_else(|| Error::missing("project", &w.project))?;
                (w, p)
            }
            None => self.target(None, &here)?.ok_or_else(|| {
                Error::invalid(format!(
                    "{name} is in {}, which is in no project: name the worktree (`ptyro shells adopt {name} myrepo/brisk-rook`)",
                    here.display()
                ))
            })?,
        };
        // Its profile is fixed for its life (SPEC §10.10): adopting can't change it.
        let ws = self.home_workspace(None, Some(&project))?;
        let theirs = self
            .resolve_profile(None, Some(&(wt.clone(), project.clone())), &ws, true, true)
            .map(|r| r.profile)
            .unwrap_or_default();
        if !theirs.is_empty() && theirs != info.profile {
            return Err(Error::conflict(format!(
                "{name} runs under the profile {} and {} under {theirs}; a session's profile never changes, so start a shell there instead (`ptyro new --wt {}`)",
                info.profile,
                wt.qualified(),
                wt.qualified()
            )));
        }
        let scratch = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let scratch = m.shelf.take().and_then(|f| f.scratch);
            m.place.worktree = Some(wt.id.clone());
            m.place.project = Some(project.id.clone());
            m.place.workspace = Some(ws);
            scratch
        };
        if let Some(dir) = scratch {
            tracing::info!("{name} left SHELLS: its scratch directory {} stays", dir.display());
        }
        s.save(None);
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        Ok(Reply::Session { info: self.info(&s) })
    }

    fn shell_watch(&self, addr: &str, watch: Option<Watch>) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        // What already matches on screen now is the baseline: only a new line is a hit.
        let seen = match &watch {
            Some(Watch::Match { re }) => {
                let r = regex::Regex::new(re)
                    .map_err(|e| Error::invalid(format!("match {re:?}: {e}")))?;
                s.read_screen(ScreenFormat::Plain)
                    .lines()
                    .filter(|l| r.is_match(l))
                    .map(|l| l.trim().to_owned())
                    .collect()
            }
            _ => Vec::new(),
        };
        {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let name = m.name.clone();
            let Some(f) = m.shelf.as_mut() else {
                return Err(Error::invalid(format!(
                    "{name} isn't in SHELLS: watches are for SHELLS (`ptyro shells loose {name}` moves it there)"
                )));
            };
            f.watch = watch;
            f.seen = seen;
        }
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// The one-second tick: long commands and watches (SPEC §23.9), and the list a restart
    /// offers again (SPEC §23.10).
    pub(crate) fn start_shells(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut seen: HashMap<SessionId, Seen> = HashMap::new();
            let mut kept: Option<Vec<Kept>> = None;
            loop {
                if *me.shutdown.borrow() {
                    return;
                }
                me.look(&mut seen).await;
                let now = me.kept_now();
                if kept.as_ref() != Some(&now) {
                    me.save_kv(KEPT, serde_json::to_value(&now).unwrap_or_default());
                    kept = Some(now);
                }
                tokio::time::sleep(TICK).await;
            }
        });
    }

    /// Write what a restart would offer back now (the tick does so too, once a second).
    /// Not while the daemon stops: its shells end then, and are what a restart offers back.
    pub(crate) fn save_kept(&self) {
        if *self.shutdown.borrow() {
            return;
        }
        self.save_kv(KEPT, serde_json::to_value(self.kept_now()).unwrap_or_default());
    }

    /// The live loose sessions, as a restart would offer them.
    fn kept_now(&self) -> Vec<Kept> {
        self.sessions()
            .into_iter()
            .filter_map(|s| {
                // Kept: running, offered back and not yet started (a second restart must not
                // lose it), or a dormant agent (its number comes back with it). Not one that
                // ended for good, nor a scratch shell, whose directory went with it.
                let keep = {
                    let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    m.shelf.as_ref().is_some_and(|f| {
                        f.scratch.is_none()
                            && (m.state.is_live() || f.lost || m.state == ProcessState::Dormant)
                    })
                };
                if !keep {
                    return None;
                }
                let live_cwd = s.is_live().then(|| self.info(&s).live_cwd).flatten();
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let f = m.shelf.as_ref()?;
                Some(Kept {
                    id: s.id.full(),
                    name: m.name.clone(),
                    title: m.title.clone(),
                    kind: m.kind,
                    argv: m.argv.clone(),
                    cwd: live_cwd.unwrap_or_else(|| m.cwd.clone()),
                    workspace: m.place.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into()),
                    profile: m.place.profile.clone(),
                    slot: f.slot,
                    client_env: m.client_env.clone(),
                    pinned: f.pinned,
                })
            })
            .collect()
    }

    async fn look(self: &Arc<Self>, seen: &mut HashMap<SessionId, Seen>) {
        let notify_after = self.config().shell.notify_after().map(|d| d.as_millis() as u64);
        let now = now_ms();
        let mut live = Vec::new();
        for s in self.sessions() {
            let (shelf, command) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (m.shelf.clone(), m.command.clone())
            };
            let Some(shelf) = shelf else { continue };
            if !s.is_live() {
                continue;
            }
            live.push(s.id);
            let out = s.output_bytes.load(std::sync::atomic::Ordering::Relaxed);
            let first = !seen.contains_key(&s.id);
            let e = seen.entry(s.id).or_default();
            if first {
                // What happened before it was first looked at isn't news.
                e.ended = command.as_ref().and_then(|c| c.ended_ms);
                e.out = out;
            }
            // A command ended: long ones say so, and a `done` watch fires (SPEC §23.9).
            if let Some(c) = command.as_ref().filter(|c| c.ended_ms.is_some())
                && e.ended != c.ended_ms
            {
                e.ended = c.ended_ms;
                let ran = c.ended_ms.unwrap_or(now).saturating_sub(c.started_ms);
                let watched = shelf.watch == Some(Watch::Done);
                if watched || notify_after.is_some_and(|d| ran >= d) {
                    let what = c.text.clone().unwrap_or_else(|| "the command".into());
                    let (kind, text) = match c.exit {
                        Some(0) | None => (
                            AttentionKind::Done,
                            format!("✓ {what} ({})", crate::agents::human(ran)),
                        ),
                        Some(code) => (
                            AttentionKind::Errored,
                            format!("✗{code} {what} ({})", crate::agents::human(ran)),
                        ),
                    };
                    self.raise(&s, kind, text).await;
                    if watched {
                        self.watch_fired(&s);
                    }
                }
            }
            let watched_now = self.watched(s.id);
            match &shelf.watch {
                Some(Watch::Output) => {
                    let quiet = s
                        .meta
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .attention
                        .is_none();
                    if out > e.out && !watched_now && quiet {
                        self.raise(&s, AttentionKind::Notification, "new output".into()).await;
                    }
                    e.out = out;
                }
                Some(Watch::Silence { ms }) => {
                    let last = s.last_output_ms.load(std::sync::atomic::Ordering::Relaxed);
                    let since = last.max(s.id.created_ms());
                    if now.saturating_sub(since) >= *ms && e.quiet_at != Some(since) {
                        e.quiet_at = Some(since);
                        let text = format!("silent for {}", crate::agents::human(*ms));
                        self.raise(&s, AttentionKind::Notification, text).await;
                    }
                }
                Some(Watch::Match { re }) => {
                    // Every tick: a screen that changed without counted output (a redraw) can
                    // show the line too, and one watched shell's screen is cheap to read.
                    if e.re.as_ref().is_none_or(|(p, _)| p != re) {
                        e.re = regex::Regex::new(re).ok().map(|r| (re.clone(), r));
                    }
                    e.out = out;
                    let screen = s.read_screen(ScreenFormat::Plain);
                    let matching: Vec<String> = e.re.as_ref().map_or_else(Vec::new, |(_, r)| {
                        screen
                            .lines()
                            .filter(|l| r.is_match(l))
                            .map(|l| l.trim().to_owned())
                            .collect()
                    });
                    let hit = matching.into_iter().find(|l| !shelf.seen.contains(l));
                    if let Some(line) = hit {
                        let text = ptyro_core::session::brief(&format!("matched: {line}"), 160);
                        self.raise(&s, AttentionKind::Notification, text).await;
                        self.watch_fired(&s);
                    }
                }
                _ => e.out = out,
            }
        }
        seen.retain(|id, _| live.contains(id));
    }

    /// A once-only watch (`done`, `match`) clears when it fires.
    fn watch_fired(&self, s: &Arc<Session>) {
        let cleared = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.shelf.as_mut().is_some_and(|f| {
                let once = f.watch.as_ref().is_some_and(Watch::once);
                if once {
                    f.watch = None;
                }
                once
            })
        };
        if cleared {
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        }
    }

    /// At start: where `%N` points, and SHELLS lost in the last restart (`[shell] restore`,
    /// SPEC §23.10). A live upgrade carries them over running, so only the missing come back.
    pub(crate) async fn restore_shells(self: &Arc<Self>) {
        *self.shown_workspace.locked() = Some(self.last_workspace().await);
        let Some(store) = self.store.clone() else { return };
        let mode = self.config().shell.restore;
        let list: Vec<Kept> = match store.kv_get(KEPT.into()).await {
            Ok(Some(v)) => serde_json::from_value(v).unwrap_or_default(),
            _ => Vec::new(),
        };
        if mode == ShellsRestore::Forget || list.is_empty() {
            return;
        }
        let scrollback = self.config().daemon.scrollback_lines;
        let mut back = 0;
        for k in list {
            let Ok(id) = k.id.parse::<SessionId>() else { continue };
            if let Some(s) = self.session(id) {
                // Already back (a live upgrade's, or a dormant agent `restore_dormant` brought
                // back without its number): give it its number again.
                let bare = s
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .shelf
                    .is_none();
                if bare
                    && let Ok(r) = self
                        .reserve_slot(&k.workspace, Some(k.slot))
                        .or_else(|_| self.reserve_slot(&k.workspace, None))
                {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf =
                        Some(Shelf { slot: r.key.1, pinned: k.pinned, ..Shelf::default() });
                }
                continue;
            }
            let ws = if self.find_workspace(&k.workspace).is_ok() {
                k.workspace.clone()
            } else {
                MAIN_WORKSPACE.into()
            };
            let Ok(r) =
                self.reserve_slot(&ws, Some(k.slot)).or_else(|_| self.reserve_slot(&ws, None))
            else {
                continue;
            };
            let cwd =
                if k.cwd.is_dir() { k.cwd.clone() } else { ptyro_core::config::expand_home("~") };
            let spec = crate::session::DormantSpec {
                name: self.free_name(&k.name),
                kind: k.kind,
                argv: k.argv.clone(),
                cwd: cwd.clone(),
                place: crate::session::Place {
                    profile: k.profile.clone(),
                    workspace: Some(ws.clone()),
                    ..crate::session::Place::default()
                },
                adapter: None,
                conversation: None,
                ended_ms: Some(now_ms()),
                title: k.title.clone(),
                structured: None,
            };
            let s = Session::dormant(id, spec, TermSize::DEFAULT, scrollback, self.hub());
            {
                let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                m.state = ProcessState::Failed { error: "lost when ptyro restarted".into() };
                m.client_env = k.client_env.clone();
                m.shelf =
                    Some(Shelf { slot: r.key.1, lost: true, pinned: true, ..Shelf::default() });
            }
            let dir = self.paths.data.join("sessions").join(id.full());
            if dir.is_dir() {
                s.load_recording(dir);
            }
            self.push_session(Arc::clone(&s));
            drop(r);
            back += 1;
            // `start`: shells start again by themselves; a command (ssh, a server) only when
            // asked, since running it again might do something.
            if mode == ShellsRestore::Start && k.kind == SessionKind::Shell {
                let me = Arc::clone(self);
                let addr = id.full();
                tokio::spawn(async move {
                    if let Err(e) = me.restart(&addr).await {
                        tracing::warn!("starting shell {addr} again: {}", e.message);
                    }
                });
            }
        }
        if back > 0 {
            tracing::info!("{back} shell(s) of SHELLS offered again after the restart");
        }
    }
}
