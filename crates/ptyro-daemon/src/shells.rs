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
use ptyro_core::session::{ProcessState, SessionKind, SpawnSpec, Watch};
use ptyro_core::status::AttentionKind;
use ptyro_core::sync::MutexExt;
use ptyro_core::window::{MAX_PANES, SplitDir, WindowLayout};
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

/// Scratch directories made for shells still starting: never a leftover, never deleted.
static STARTING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A scratch directory made for a spawn that may still be refused. While it lives, `gc` leaves
/// the directory alone (`STARTING`); dropped unkept, the directory goes again (only while empty:
/// `remove_dir` never takes what someone put there).
pub(crate) struct ScratchGuard {
    dir: Option<PathBuf>,
    keep: bool,
}

impl ScratchGuard {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        if let Some(d) = &dir {
            STARTING.locked().push(d.clone());
        }
        Self { dir, keep: false }
    }

    /// The shell started: its directory is its own now.
    pub(crate) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            STARTING.locked().retain(|d| *d != dir);
            if !self.keep {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
}

/// What a leftover holds, measured at most every half hour (as worktrees' sizes are).
/// (files, bytes, newest change in ms) of a directory, as `tally` counts them.
type Tally = (u64, u64, u64);
static TALLIES: Mutex<BTreeMap<PathBuf, (Tally, std::time::Instant)>> = Mutex::new(BTreeMap::new());

/// The files under `dir` (not following links; directories aren't counted): (files, bytes,
/// newest change in ms). Gives up after three seconds or 100,000 files: a leftover that big
/// shows at least that.
fn tally(dir: &Path) -> Tally {
    const FRESH: Duration = Duration::from_secs(1800);
    if let Some((t, at)) = TALLIES.locked().get(dir)
        && at.elapsed() < FRESH
    {
        return *t;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let (mut files, mut bytes, mut newest) = (0u64, 0u64, 0u64);
    let stamp = |m: &std::fs::Metadata| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64)
    };
    if let Ok(m) = std::fs::symlink_metadata(dir) {
        newest = stamp(&m);
    }
    let mut todo = vec![dir.to_path_buf()];
    'walk: while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.path().symlink_metadata() else { continue };
            newest = newest.max(stamp(&m));
            if m.is_dir() {
                todo.push(e.path());
                continue;
            }
            files += 1;
            bytes += m.len();
            if files >= 100_000 || std::time::Instant::now() > deadline {
                break 'walk;
            }
        }
    }
    let t = (files, bytes, newest);
    TALLIES.locked().insert(dir.to_path_buf(), (t, std::time::Instant::now()));
    t
}

/// The lowest number from 1 that isn't in `taken`.
fn lowest_free(taken: &[u32]) -> u32 {
    let mut n = 1;
    while taken.contains(&n) {
        n += 1;
    }
    n
}

/// Held while a window's layout is read, changed and written back (`Daemon::rearrange`), so two
/// changes at once (two splits finishing together) never lose one. Taken before any `meta`.
static ARRANGING: Mutex<()> = Mutex::new(());

/// The number a new pane of a window takes (SPEC §23.17): the lowest free one above its first
/// pane, so a copy never becomes the window's first pane (`%N`, its row) ahead of the original.
fn next_pane_number(taken: &[u32]) -> u32 {
    let mut n = taken.iter().min().map_or(1, |m| m + 1);
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
    /// Its pane in its window, and the window's layout (SPEC §23.17).
    #[serde(default)]
    pane: u32,
    #[serde(default)]
    layout: Option<WindowLayout>,
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

    /// Every loose session of `workspace` (live, exited, lost), with its number, by number
    /// and pane (a window's panes share its number, SPEC §23.17).
    pub(crate) fn shells_of(&self, workspace: &str) -> Vec<(Arc<Session>, u32)> {
        let mut out: Vec<(Arc<Session>, u32, u32)> = self
            .sessions()
            .into_iter()
            .filter_map(|s| {
                let place = {
                    let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let ws = m.place.workspace.as_deref().unwrap_or(MAIN_WORKSPACE);
                    m.shelf.as_ref().filter(|_| ws == workspace).map(|f| (f.slot, f.pane()))
                };
                place.map(|(n, p)| (s, n, p))
            })
            .collect();
        out.sort_by_key(|(_, n, p)| (*n, *p));
        out.into_iter().map(|(s, n, _)| (s, n)).collect()
    }

    /// The loose session numbered `slot` in `workspace`: its window's first pane.
    pub(crate) fn shell_at(&self, workspace: &str, slot: u32) -> Option<Arc<Session>> {
        self.shells_of(workspace).into_iter().find(|(_, n)| *n == slot).map(|(s, _)| s)
    }

    /// Window `slot`'s panes in `workspace`, with their pane numbers, in order.
    pub(crate) fn panes_of(&self, workspace: &str, slot: u32) -> Vec<(Arc<Session>, u32)> {
        self.shells_of(workspace)
            .into_iter()
            .filter(|(_, n)| *n == slot)
            .map(|(s, _)| {
                let pane = s
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .shelf
                    .as_ref()
                    .map_or(1, Shelf::pane);
                (s, pane)
            })
            .collect()
    }

    /// A loose session's workspace and number (for code outside this module).
    pub(crate) fn window_of_shelf(&self, s: &Session) -> Option<(String, u32)> {
        Self::window_of(s)
    }

    /// A loose session's workspace and number.
    fn window_of(s: &Session) -> Option<(String, u32)> {
        let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ws = m.place.workspace.clone().unwrap_or_else(|| MAIN_WORKSPACE.into());
        m.shelf.as_ref().map(|f| (ws, f.slot))
    }

    /// The pane numbers window `slot` of `workspace` has, with those still starting (`pending`
    /// is `PENDING`, held by the caller, so the count and the claim are one step).
    fn panes_taken(
        &self,
        workspace: &str,
        slot: u32,
        pending: &BTreeMap<SessionId, Shelf>,
    ) -> Vec<u32> {
        let mut taken: Vec<u32> =
            self.panes_of(workspace, slot).into_iter().map(|(_, p)| p).collect();
        // A starting pane's workspace isn't known here: one of that number anywhere counts.
        taken.extend(pending.values().filter(|f| f.slot == slot && f.pane > 0).map(Shelf::pane));
        taken
    }

    /// `launch` will make session `id` a new pane of the window loose session `src` is in
    /// (`ShellSplit`); refused at [`MAX_PANES`], counting panes still starting.
    pub(crate) fn pend_pane(&self, src: SessionId, id: SessionId) -> Result<(), Error> {
        let s = self.session(src).ok_or_else(|| Error::not_found(src.full()))?;
        let Some((ws, slot)) = Self::window_of(&s) else {
            let name =
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
            return Err(Error::invalid(format!("{name} isn't in SHELLS")));
        };
        let mut pending = PENDING.locked();
        let taken = self.panes_taken(&ws, slot, &pending);
        if taken.len() >= MAX_PANES {
            return Err(Error::conflict(format!(
                "shell {slot} has {MAX_PANES} panes, the most a window holds: break one out to a number of its own first"
            )));
        }
        let pane = next_pane_number(&taken);
        pending.insert(id, Shelf { slot, pane, pinned: true, ..Shelf::default() });
        Ok(())
    }

    /// Change window `slot`'s layout as it stands (`edit`) and write it to every pane, as one
    /// step.
    fn rearrange(&self, workspace: &str, slot: u32, edit: impl FnOnce(&mut WindowLayout)) {
        let _one = ARRANGING.locked();
        let mut l = self.layout_of(workspace, slot);
        edit(&mut l);
        self.relayout_held(workspace, slot, Some(l));
    }

    /// Window `slot` of `workspace` changed its panes (or `base` is its new arrangement): every
    /// pane gets the same layout, fitted to the panes there are, and clients hear of each one
    /// whose layout changed. One pane left: no layout.
    pub(crate) fn relayout(&self, workspace: &str, slot: u32, base: Option<WindowLayout>) {
        let _one = ARRANGING.locked();
        self.relayout_held(workspace, slot, base);
    }

    /// `relayout`, with `ARRANGING` held.
    fn relayout_held(&self, workspace: &str, slot: u32, base: Option<WindowLayout>) {
        let panes = self.panes_of(workspace, slot);
        let base = base.or_else(|| {
            panes.iter().find_map(|(s, _)| {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                m.shelf.as_ref().and_then(|f| f.layout.clone())
            })
        });
        let numbers: Vec<u32> = panes.iter().map(|(_, p)| *p).collect();
        let layout = (numbers.len() > 1).then(|| {
            let mut l = base.unwrap_or_else(|| WindowLayout::single(numbers[0]));
            l.fit(&numbers);
            l
        });
        for (s, _) in &panes {
            let changed = {
                let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                m.shelf.as_mut().is_some_and(|f| {
                    let changed = f.layout != layout;
                    f.layout = layout.clone();
                    changed
                })
            };
            if changed {
                let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
            }
        }
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
                        "{workspace} has a shell {n} already ({name}): go to it instead"
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
        // A window's panes move together: the number its first mover got is theirs too.
        let mut renumbered: HashMap<(String, u32), u32> = HashMap::new();
        let mut settled: Vec<SessionId> = Vec::new();
        for s in movers {
            let Some((ws, slot)) = Self::window_of(&s) else { continue };
            settled.push(s.id);
            let n = match renumbered.get(&(ws.clone(), slot)) {
                Some(&n) => n,
                None => {
                    // Taken by a shell that is settled there: every one that didn't move, and
                    // the movers already placed (the others' numbers aren't theirs yet).
                    let taken: Vec<u32> = self
                        .shells_of(&ws)
                        .into_iter()
                        .filter(|(o, _)| o.id != s.id)
                        .filter(|(o, _)| !moved.contains(&o.id) || settled.contains(&o.id))
                        .map(|(_, n)| n)
                        .collect();
                    let n = if taken.contains(&slot) { lowest_free(&taken) } else { slot };
                    renumbered.insert((ws, slot), n);
                    n
                }
            };
            if n == slot {
                continue;
            }
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
        let (n, pane) = ptyro_core::window::parse_address(n)?;
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
        let Some(first) = self.shell_at(&ws, n) else {
            return Some(Err(Error::new(
                ErrorCode::NotFound,
                format!("no shell {n} in {ws}: start one there first"),
            )));
        };
        // `%3.2`: pane 2 of shell 3 (SPEC §23.17); `%3` is its first pane.
        let Some(p) = pane else { return Some(Ok(first)) };
        let panes = self.panes_of(&ws, n);
        Some(panes.iter().find(|(_, q)| *q == p).map(|(s, _)| Arc::clone(s)).ok_or_else(|| {
            let have: Vec<String> = panes.iter().map(|(_, q)| format!("%{n}.{q}")).collect();
            Error::new(
                ErrorCode::NotFound,
                format!("shell {n} of {ws} has no pane {p} (it has {})", have.join(", ")),
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
            s.strip_prefix('%').is_some_and(|n| ptyro_core::window::parse_address(n).is_some())
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
        let canon = |p: &Path| ptyro_core::os::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
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
        let used = self.scratch_in_use();
        let mut out = tokio::task::spawn_blocking(move || {
            let Ok(dirs) = std::fs::read_dir(&root) else { return Vec::new() };
            let starting = STARTING.locked().clone();
            let mut out = Vec::new();
            for path in dirs.flatten().map(|d| d.path()) {
                if !path.is_dir()
                    || starting.contains(&path)
                    || used.iter().any(|u| u.starts_with(&path))
                {
                    continue;
                }
                // Empty: nothing of anyone's in it, so it simply goes (ptyro's own tidying).
                if std::fs::remove_dir(&path).is_ok() {
                    continue;
                }
                let (files, bytes, modified_ms) = tally(&path);
                out.push(ptyro_core::model::ScratchLeftover { path, files, bytes, modified_ms });
            }
            TALLIES.locked().retain(|p, _| out.iter().any(|l| l.path == *p));
            out
        })
        .await
        .unwrap_or_default();
        out.sort_by_key(|l| std::cmp::Reverse(l.modified_ms));
        out
    }

    /// `ShellScratchRemove`: a leftover scratch directory, and what's in it, deleted (asked
    /// first by every client). Only directly under `<data>/scratch`, and never one in use.
    async fn scratch_remove(&self, path: &Path) -> Result<Reply, Error> {
        let root = self.paths.data.join("scratch");
        let canon = |p: &Path| ptyro_core::os::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let (root, dir) = (canon(&root), canon(path));
        if dir.parent() != Some(root.as_path()) || !dir.is_dir() {
            return Err(Error::invalid(format!(
                "{} isn't a scratch shell's directory (they live in {})",
                path.display(),
                root.display()
            )));
        }
        let starting = STARTING.locked().iter().any(|d| canon(d) == dir);
        if starting || self.scratch_in_use().iter().any(|u| canon(u).starts_with(&dir)) {
            return Err(Error::conflict(format!(
                "{} is a scratch shell's directory still in use: stop that shell first",
                dir.display()
            )));
        }
        let gone = dir.clone();
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&gone))
            .await
            .map_err(|e| Error::internal(e.to_string()))?
            .map_err(|e| Error::internal(format!("deleting {}: {e}", dir.display())))?;
        TALLIES.locked().remove(&dir);
        Ok(Reply::Removed { notes: vec![format!("deleted {}", dir.display())] })
    }

    /// Every directory a session has in `scratch/` now: its own scratch directory, where it was
    /// started, and where it is (a scratch shell adopted into a worktree still runs there).
    fn scratch_in_use(&self) -> Vec<PathBuf> {
        let root = self.paths.data.join("scratch");
        let mut used = Vec::new();
        for s in self.sessions() {
            let (scratch, cwd) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (m.shelf.as_ref().and_then(|f| f.scratch.clone()), m.cwd.clone())
            };
            used.extend(scratch);
            used.push(cwd);
            if s.is_live()
                && let Some(here) = self.info(&s).live_cwd
            {
                used.push(here);
            }
        }
        // A shell reports where it is as the system sees it (macOS: /private/var for /var):
        // both spellings of the root count.
        let real = ptyro_core::os::canonicalize(&root).unwrap_or_else(|_| root.clone());
        used.into_iter()
            .filter_map(|p| {
                if p.starts_with(&root) {
                    Some(p)
                } else {
                    p.strip_prefix(&real).ok().map(|rest| root.join(rest))
                }
            })
            .collect()
    }

    /// `ShellMove`, `ShellPlace`, `ShellWatch`.
    pub(crate) async fn handle_shells(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::ShellMove { addr, slot } => self.shell_move(&addr, slot),
            Request::ShellSplit { addr, dir, env, size } => {
                self.shell_split(&addr, dir, env, size).await
            }
            Request::ShellJoin { addr, into, dir } => self.shell_join(&addr, into, dir),
            Request::ShellLayout { addr, layout } => self.shell_layout(&addr, layout),
            Request::ShellPlace { addr, loose, worktree } => {
                self.shell_place(&addr, loose, worktree.as_deref())
            }
            Request::ShellWatch { addr, watch } => self.shell_watch(&addr, watch),
            Request::ShellScratchRemove { path } => self.scratch_remove(&path).await,
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
                    "{} isn't in SHELLS: move it there first",
                    m.name
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
        // A pane starting in either window would land in the number it was split from.
        if let Some(n) = PENDING
            .locked()
            .values()
            .find(|f| f.pane > 0 && (f.slot == from || f.slot == slot))
            .map(|f| f.slot)
        {
            return Err(Error::conflict(format!("a pane of shell {n} is starting: try again")));
        }
        // Whole windows move: every pane of each (SPEC §23.17).
        let mine = self.panes_of(&ws, from);
        let theirs = self.panes_of(&ws, slot);
        let set = |t: &Arc<Session>, n: u32| {
            if let Some(f) =
                t.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf.as_mut()
            {
                f.slot = n;
            }
        };
        for (t, _) in &mine {
            set(t, slot);
        }
        for (t, _) in &theirs {
            set(t, from);
        }
        drop(held);
        for (t, _) in mine.iter().chain(&theirs) {
            let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(t) });
        }
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
                    "{name} is in {}, which is in no project: name the worktree to move it into (like myrepo/brisk-rook)",
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
        let window = Self::window_of(&s);
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
        // The panes it leaves behind close the gap.
        if let Some((ws, slot)) = window {
            self.relayout(&ws, slot, None);
        }
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// `ShellSplit` (SPEC §23.17): a copy of a loose session as a new pane of its window: a
    /// shell (whatever the source runs, as tmux's `split-window`) in its live directory, in its
    /// workspace, under its profile, beside or below it.
    async fn shell_split(
        self: &Arc<Self>,
        addr: &str,
        dir: SplitDir,
        env: BTreeMap<String, String>,
        size: Option<TermSize>,
    ) -> Result<Reply, Error> {
        let src = self.resolve(addr)?;
        let info = self.info(&src);
        let Some((ws, _)) = Self::window_of(&src) else {
            return Err(Error::invalid(format!(
                "{} isn't in SHELLS: split it in its pane instead (`v`/`s` in the TUI)",
                info.name
            )));
        };
        let here = info.live_cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(|| info.cwd.clone());
        let env = if env.is_empty() {
            src.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).client_env.clone()
        } else {
            env
        };
        let spec = SpawnSpec {
            cwd: here,
            env,
            size: size.or(Some(info.size)),
            workspace: Some(ws.clone()),
            profile: Some(info.profile.clone()),
            pane_of: Some(src.id),
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        let at = self.session(src.id).map_or(info.pane.unwrap_or(1), |o| {
            o.meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .shelf
                .as_ref()
                .map_or(1, Shelf::pane)
        });
        let new = s
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shelf
            .as_ref()
            .map(Shelf::pane);
        // Where the new pane is now (its window may have moved while it started).
        if let (Some((ws, slot)), Some(new)) = (Self::window_of(&s), new) {
            self.rearrange(&ws, slot, |l| l.insert(at, new, dir));
        }
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// Window `slot`'s layout as it stands (one pane: that pane alone).
    fn layout_of(&self, ws: &str, slot: u32) -> WindowLayout {
        let panes = self.panes_of(ws, slot);
        let stored = panes.iter().find_map(|(s, _)| {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.shelf.as_ref().and_then(|f| f.layout.clone())
        });
        let first = panes.first().map_or(1, |(_, p)| *p);
        stored.unwrap_or_else(|| WindowLayout::single(first))
    }

    /// `ShellJoin` (SPEC §23.17): into another window as a new pane (`join-pane`), or with no
    /// `into` out of its window to the lowest free number (`break-pane`).
    fn shell_join(&self, addr: &str, into: Option<u32>, dir: SplitDir) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        let Some((ws, from)) = Self::window_of(&s) else {
            return Err(Error::invalid(format!("{name} isn't in SHELLS: move it there first")));
        };
        let set = |slot: u32, pane: u32| {
            if let Some(f) =
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf.as_mut()
            {
                (f.slot, f.pane, f.layout) = (slot, pane, None);
            }
        };
        let to = match into {
            None => {
                if self.panes_of(&ws, from).len() < 2 {
                    return Err(Error::invalid(format!(
                        "shell {from} has one pane: it has a number of its own already"
                    )));
                }
                let r = self.reserve_slot(&ws, None)?;
                set(r.key.1, 1);
                r.key.1
            }
            Some(n) if n == from => {
                return Err(Error::invalid(format!("{name} is in shell {n} already")));
            }
            Some(n) => {
                let target = self.panes_of(&ws, n);
                if target.is_empty() {
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        format!("no shell {n} in {ws} to join"),
                    ));
                }
                let pane = {
                    let pending = PENDING.locked();
                    let taken = self.panes_taken(&ws, n, &pending);
                    if taken.len() >= MAX_PANES {
                        return Err(Error::conflict(format!(
                            "shell {n} has {MAX_PANES} panes, the most a window holds"
                        )));
                    }
                    let pane = next_pane_number(&taken);
                    set(n, pane);
                    pane
                };
                self.rearrange(&ws, n, |l| {
                    let at = l.focus;
                    l.insert(at, pane, dir);
                });
                n
            }
        };
        self.relayout(&ws, from, None);
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
        tracing::info!("{name} is in shell {to} of {ws} now");
        Ok(Reply::Session { info: self.info(&s) })
    }

    /// `ShellLayout` (SPEC §23.17): a client rearranged a window (shares, focus, zoom).
    fn shell_layout(&self, addr: &str, layout: WindowLayout) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let Some((ws, slot)) = Self::window_of(&s) else {
            let name =
                s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
            return Err(Error::invalid(format!("{name} isn't in SHELLS")));
        };
        self.relayout(&ws, slot, Some(layout));
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
                    "{name} isn't in SHELLS: watches are for SHELLS (move it there first)"
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
                    pane: f.pane(),
                    layout: f.layout.clone(),
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
                    // Cleared before the attention is raised: `raise` awaits the desktop
                    // notification (slow inside WSL, through Windows' interop), and a client
                    // must never see a fired once-only watch still set (WSL CI, 2026-11-07).
                    if watched {
                        self.watch_fired(&s);
                    }
                    self.raise(&s, kind, text).await;
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
                        // Cleared first, as `done`'s above.
                        self.watch_fired(&s);
                        self.raise(&s, AttentionKind::Notification, text).await;
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
        // A window's panes come back together, in the number its first pane got (SPEC §23.17).
        // Keyed by the workspace it was saved in (one that's gone goes to main, where it must
        // not merge with main's own window of that number), to (where it is, its number).
        let mut windows: HashMap<(String, u32), (String, u32)> = HashMap::new();
        // Panes already back with their number (a live upgrade's) hold their window's.
        for k in &list {
            let Some(s) = k.id.parse::<SessionId>().ok().and_then(|id| self.session(id)) else {
                continue;
            };
            if let Some((ws, n)) = Self::window_of(&s) {
                windows.insert((k.workspace.clone(), k.slot), (ws, n));
            }
        }
        let mut number = |me: &Self, ws: &str, k: &Kept| -> Option<(u32, Option<Reservation>)> {
            if let Some((_, n)) = windows.get(&(k.workspace.clone(), k.slot)) {
                return Some((*n, None));
            }
            let r =
                me.reserve_slot(ws, Some(k.slot)).or_else(|_| me.reserve_slot(ws, None)).ok()?;
            windows.insert((k.workspace.clone(), k.slot), (ws.to_owned(), r.key.1));
            Some((r.key.1, Some(r)))
        };
        let shelf = |slot: u32, k: &Kept, lost: bool| Shelf {
            slot,
            pinned: k.pinned || lost,
            lost,
            pane: k.pane,
            layout: k.layout.clone(),
            ..Shelf::default()
        };
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
                if bare && let Some((n, r)) = number(self, &k.workspace, &k) {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shelf =
                        Some(shelf(n, &k, false));
                    drop(r);
                }
                continue;
            }
            let ws = if self.find_workspace(&k.workspace).is_ok() {
                k.workspace.clone()
            } else {
                MAIN_WORKSPACE.into()
            };
            let Some((slot, r)) = number(self, &ws, &k) else { continue };
            let cwd =
                if k.cwd.is_dir() { k.cwd.clone() } else { ptyro_core::config::expand_home("~") };
            let spec = crate::session::DormantSpec {
                name: self.free_name(None, &k.name),
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
                m.shelf = Some(shelf(slot, &k, true));
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
        for (ws, n) in windows.into_values() {
            self.relayout(&ws, n, None);
        }
        if back > 0 {
            tracing::info!("{back} shell(s) of SHELLS offered again after the restart");
        }
    }
}
