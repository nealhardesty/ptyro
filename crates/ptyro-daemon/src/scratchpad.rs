//! The scratchpad (NOTE1, SPEC §13.6): the notes files the daemon keeps, serves to every client
//! and tells them about when they change. Writing one is `ptyro_core::scratchpad` (atomic, with a
//! history and a three-way merge); this is what the daemon adds: which pad (the one for
//! everywhere, from `[scratchpad]`, or a project's, NOTE1.7), `$PTYRO_SCRATCHPAD` in every
//! session, a watcher that notices an editor's save, the editor as a session in a pane, and
//! ticking an item once the worktree started from it is finished.

use ptyro_core::sync::MutexExt;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::model::{ProjectInfo, WorktreeInfo};
use ptyro_core::scratchpad::{self as pad, Entry, Pad, PadSel};
use ptyro_core::session::{SessionKind, SpawnSpec};
use ptyro_core::{Error, ErrorCode, TermSize};
use ptyro_protocol::{DomainEvent, PadDoc, PadInfo, PadMark, Reply};

use crate::daemon::Daemon;
use crate::session::Session;

/// How many recent revisions' texts to keep as merge bases.
const RING: usize = 64;
/// How often the files are looked at for a change made outside.
const POLL: Duration = Duration::from_millis(700);

/// The pads' revisions (merge bases) and the lock that serialises read-modify-write.
#[derive(Default)]
pub struct PadState {
    lock: Mutex<()>,
    /// Revisions are hashes of the text, so one ring serves every pad.
    ring: Mutex<VecDeque<(String, String)>>,
    /// The revision last announced for each pad file, so a write isn't announced twice.
    seen: Mutex<HashMap<PathBuf, String>>,
}

/// One pad, resolved: which, what it is called, and the file with its history.
pub(crate) struct Scope {
    /// `Global`, or `Project(id)`.
    pub sel: PadSel,
    /// `everywhere`, or the project's name.
    pub name: String,
    pub pad: Pad,
}

/// One pane at a time starts the editor.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Daemon {
    /// Where the global pad is: `[scratchpad] path`, else `scratchpad.md` in the data directory.
    pub(crate) fn pad_path(&self) -> PathBuf {
        self.config().scratchpad_path(&self.paths.data)
    }

    fn global_scope(&self) -> Scope {
        Scope {
            sel: PadSel::Global,
            name: "everywhere".into(),
            pad: Pad::new(
                self.pad_path(),
                self.paths.data.join("scratchpad.history"),
                self.config().scratchpad.keep,
            ),
        }
    }

    /// A project's pad: `<main checkout>/.ptyro/scratchpad.md`, shared by all its worktrees.
    /// With `make`, a missing file is created and the repository told to ignore it.
    fn project_scope(&self, p: &ProjectInfo, make: bool) -> Scope {
        let pad = Pad::project(&p.root, &p.slug, self.config().scratchpad.keep);
        if make && !pad.path.exists() {
            match pad.ensure() {
                Ok(()) => self.exclude_pad(p),
                Err(e) => tracing::warn!("scratchpad {}: {e}", pad.path.display()),
            }
        }
        Scope { sel: PadSel::Project(p.id.clone()), name: p.slug.clone(), pad }
    }

    /// Keep git from offering the pad for a commit: `.git/info/exclude` of the repository (shared
    /// by its worktrees, never the tracked `.gitignore`). Best effort: the pad works without.
    fn exclude_pad(&self, p: &ProjectInfo) {
        if let Err(e) = ptyro_git::Git::new().exclude(&p.root, pad::EXCLUDE_LINE) {
            tracing::warn!("scratchpad: excluding it from git in {}: {e}", p.root.display());
        }
    }

    /// The pad a request names. `Near` is the project the directory is in, else the global pad.
    pub(crate) fn scope(&self, sel: &PadSel) -> Result<Scope, Error> {
        let project = match sel {
            PadSel::Global => None,
            PadSel::Project(addr) => Some(self.find_project(addr)?),
            PadSel::Near(dir) => self.target(None, dir).ok().flatten().map(|(_, p)| p),
        };
        Ok(match project {
            Some(p) => self.project_scope(&p, true),
            None => self.global_scope(),
        })
    }

    /// The pads that exist: the global one, and each project's that has a file.
    fn existing_scopes(&self) -> Vec<Scope> {
        let projects = self.registry.locked().projects.clone();
        let mut v = vec![self.global_scope()];
        v.extend(
            projects.iter().map(|p| self.project_scope(p, false)).filter(|sc| sc.pad.path.exists()),
        );
        v
    }

    fn pad_err(e: impl std::fmt::Display) -> Error {
        Error::internal(format!("scratchpad: {e}"))
    }

    fn remember(&self, text: &str) {
        let r = pad::rev(text);
        let mut ring = self.pad_state.ring.locked();
        if ring.iter().any(|(k, _)| *k == r) {
            return;
        }
        if ring.len() >= RING {
            ring.pop_front();
        }
        ring.push_back((r, text.to_owned()));
    }

    fn base_of(&self, sc: &Scope, rev: &str) -> Option<String> {
        let found =
            self.pad_state.ring.locked().iter().find(|(k, _)| k == rev).map(|(_, t)| t.clone());
        found.or_else(|| sc.pad.snapshot_text(rev))
    }

    fn doc(&self, sc: &Scope, text: String) -> PadDoc {
        PadDoc {
            pad: sc.sel.clone(),
            name: sc.name.clone(),
            ..PadDoc::of(sc.pad.path.clone(), text)
        }
    }

    /// Remember `text`, and tell every client when it is a revision they haven't heard of.
    fn pad_changed(&self, sc: &Scope, text: &str, by: Option<String>) {
        self.remember(text);
        let doc = self.doc(sc, text.to_owned());
        {
            let mut seen = self.pad_state.seen.locked();
            if seen.get(&sc.pad.path) == Some(&doc.rev) {
                return;
            }
            seen.insert(sc.pad.path.clone(), doc.rev.clone());
        }
        let _ = self.events.send(DomainEvent::ScratchChanged {
            pad: sc.sel.clone(),
            rev: doc.rev,
            open: doc.open,
            by,
        });
    }

    /// Make the global file, and watch every pad: a save from an editor, a `git pull` into a
    /// synced folder or an `echo >>` all reach every client as `ScratchChanged`.
    pub(crate) fn start_pad_watcher(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut last: HashMap<PathBuf, (std::time::SystemTime, u64)> = HashMap::new();
            loop {
                for sc in me.existing_scopes() {
                    let path = sc.pad.path.clone();
                    let stat = std::fs::metadata(&path)
                        .ok()
                        .and_then(|m| Some((m.modified().ok()?, m.len())));
                    let Some(stat) = stat else {
                        if sc.sel.is_global()
                            && let Err(e) = sc.pad.ensure()
                        {
                            tracing::warn!("scratchpad {}: {e}", path.display());
                        }
                        continue;
                    };
                    if last.get(&path) == Some(&stat) {
                        continue;
                    }
                    last.insert(path.clone(), stat);
                    let _lock = me.pad_state.lock.locked();
                    if let Ok(text) = sc.pad.read() {
                        let known = me.pad_state.seen.locked().get(&path).cloned();
                        match known {
                            // First look after start: nothing to announce, but a base to merge from.
                            None => {
                                me.remember(&text);
                                me.pad_state.seen.locked().insert(path, pad::rev(&text));
                            }
                            Some(k) if k != pad::rev(&text) => {
                                let _ = sc.pad.snapshot_lazy(&text);
                                me.pad_changed(&sc, &text, Some("file".into()));
                            }
                            Some(_) => {}
                        }
                    }
                }
                tokio::time::sleep(POLL).await;
            }
        });
    }

    pub(crate) fn scratch_get(&self, sel: &PadSel) -> Result<Reply, Error> {
        let sc = self.scope(sel)?;
        let _l = self.pad_state.lock.locked();
        let text = sc.pad.read().map_err(Self::pad_err)?;
        self.remember(&text);
        Ok(Reply::Scratch { doc: self.doc(&sc, text) })
    }

    /// Every pad that exists, with its open count.
    pub(crate) fn scratch_pads(&self) -> Reply {
        let pads = self
            .existing_scopes()
            .into_iter()
            .map(|sc| {
                let open = sc.pad.read().map_or(0, |t| {
                    pad::items(&t).iter().filter(|i| !i.done).count().try_into().unwrap_or(u32::MAX)
                });
                PadInfo { pad: sc.sel, name: sc.name, path: sc.pad.path, open }
            })
            .collect();
        Reply::ScratchPads { pads }
    }

    pub(crate) fn scratch_put(
        &self,
        sel: &PadSel,
        base: &str,
        text: &str,
        by: Option<String>,
    ) -> Result<Reply, Error> {
        if text.len() > ptyro_core::scratchpad::MAX_PAD_BYTES {
            return Err(Error::invalid(format!(
                "the scratchpad is limited to {} MiB",
                ptyro_core::scratchpad::MAX_PAD_BYTES >> 20
            )));
        }
        let sc = self.scope(sel)?;
        let _l = self.pad_state.lock.locked();
        let cur = sc.pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let base_text = self.base_of(&sc, base);
        let m = sc.pad.put(base, base_text.as_deref(), text).map_err(Self::pad_err)?;
        self.pad_changed(&sc, &m.text, by);
        let merged = m.text != text;
        Ok(Reply::ScratchPut { doc: self.doc(&sc, m.text), merged, conflicts: m.conflicts })
    }

    /// Who is adding: the stamp's worktree (or directory) and, for an agent, its name.
    fn whence(
        &self,
        session: Option<&str>,
        cwd: Option<&std::path::Path>,
    ) -> (Option<String>, Option<String>) {
        if let Some(s) = session.and_then(|a| self.resolve(a).ok()) {
            let (wt, name, agent, dir) = {
                let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    m.place.worktree.clone(),
                    m.name.clone(),
                    m.kind == SessionKind::Agent,
                    m.cwd.clone(),
                )
            };
            let whence = wt
                .and_then(|id| self.worktree_by_id(&id))
                .map(|w| w.qualified())
                .or_else(|| dir_label(&dir));
            return (whence, agent.then_some(name));
        }
        let Some(dir) = cwd else { return (None, None) };
        let whence = self
            .target(None, dir)
            .ok()
            .flatten()
            .map(|(w, _)| w.qualified())
            .or_else(|| dir_label(dir));
        (whence, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scratch_add(
        &self,
        sel: &PadSel,
        text: &str,
        section: Option<String>,
        note: bool,
        stamp: bool,
        session: Option<&str>,
        cwd: Option<&std::path::Path>,
    ) -> Result<Reply, Error> {
        if text.trim().is_empty() {
            return Err(Error::invalid("nothing to add"));
        }
        let sc = self.scope(sel)?;
        let (whence, by) = if stamp { self.whence(session, cwd) } else { (None, None) };
        let entry = Entry {
            text: text.to_owned(),
            section,
            note,
            at: if stamp { ptyro_pty::local_datetime() } else { None },
            whence,
            by: by.clone(),
        };
        let _l = self.pad_state.lock.locked();
        let cur = sc.pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let text = sc.pad.add(&entry).map_err(Self::pad_err)?;
        self.pad_changed(&sc, &text, by.or(Some("cli".into())));
        Ok(Reply::Scratch { doc: self.doc(&sc, text) })
    }

    pub(crate) fn scratch_todo(&self, sel: &PadSel, all: bool) -> Result<Reply, Error> {
        let sc = self.scope(sel)?;
        let text = sc.pad.read().map_err(Self::pad_err)?;
        let items = pad::items(&text).into_iter().filter(|i| all || !i.done).collect();
        Ok(Reply::ScratchItems { rev: pad::rev(&text), items })
    }

    pub(crate) fn scratch_mark(
        &self,
        sel: &PadSel,
        item: &str,
        mark: &PadMark,
    ) -> Result<Reply, Error> {
        let sc = self.scope(sel)?;
        let _l = self.pad_state.lock.locked();
        let cur = sc.pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let item = pad::select(&cur, item).map_err(Error::invalid)?;
        let text = sc
            .pad
            .modify(|t| match mark {
                PadMark::Done { pr } => pad::tick(t, item.line, *pr),
                PadMark::Open => pad::untick(t, item.line),
                PadMark::Started { worktree } => pad::tag_worktree(t, item.line, worktree),
            })
            .map_err(Error::invalid)?;
        self.pad_changed(&sc, &text, Some("cli".into()));
        let item = pad::items(&text).into_iter().find(|i| i.line == item.line);
        Ok(Reply::ScratchItems { rev: pad::rev(&text), items: item.into_iter().collect() })
    }

    pub(crate) fn scratch_history(&self, sel: &PadSel) -> Result<Reply, Error> {
        Ok(Reply::ScratchLog { snapshots: self.scope(sel)?.pad.history() })
    }

    pub(crate) fn scratch_restore(&self, sel: &PadSel, rev: &str) -> Result<Reply, Error> {
        let sc = self.scope(sel)?;
        let _l = self.pad_state.lock.locked();
        let text = sc.pad.restore(rev).map_err(Error::invalid)?;
        self.pad_changed(&sc, &text, Some("cli".into()));
        Ok(Reply::Scratch { doc: self.doc(&sc, text) })
    }

    /// The items started from worktree `wt` are ticked (`tick_on_finish`), in the pad for
    /// everywhere and in its project's.
    pub(crate) fn pad_finished(&self, wt: &WorktreeInfo, pr: Option<u64>) {
        if !self.config().scratchpad.tick_on_finish {
            return;
        }
        let qualified = wt.qualified();
        let mut scopes = vec![self.global_scope()];
        scopes.extend(self.project_by_id(&wt.project).map(|p| self.project_scope(&p, false)));
        let _l = self.pad_state.lock.locked();
        for sc in scopes.iter().filter(|sc| sc.pad.path.exists()) {
            let Ok(cur) = sc.pad.read() else { continue };
            let mine: Vec<u32> = pad::items(&cur)
                .into_iter()
                .filter(|i| !i.done && i.worktree.as_deref() == Some(qualified.as_str()))
                .map(|i| i.line)
                .collect();
            if mine.is_empty() {
                continue;
            }
            self.remember(&cur);
            let done = sc.pad.modify(|t| {
                let mut t = t.to_owned();
                for line in &mine {
                    t = pad::tick(&t, *line, pr)?;
                }
                Ok(t)
            });
            match done {
                Ok(text) => self.pad_changed(sc, &text, Some(format!("finished {qualified}"))),
                Err(e) => tracing::warn!("scratchpad: ticking items of {qualified}: {e}"),
            }
        }
    }

    /// The item on `line` of the pad `sel` starts in worktree `qualified`
    /// (`WorktreeSpec::pad_item`).
    pub(crate) fn pad_started(&self, sel: &PadSel, line: u32, qualified: &str) {
        let Ok(sc) = self.scope(sel) else { return };
        let _l = self.pad_state.lock.locked();
        if let Ok(cur) = sc.pad.read() {
            self.remember(&cur);
        }
        match sc.pad.modify(|t| pad::tag_worktree(t, line, qualified)) {
            Ok(text) => self.pad_changed(&sc, &text, Some(format!("started {qualified}"))),
            Err(e) => tracing::warn!("scratchpad: tagging line {line}: {e}"),
        }
    }

    /// The live editor session of a pad (`project`: its id; none: the global pad), if any.
    fn pad_session(&self, project: Option<&str>) -> Option<Arc<Session>> {
        self.sessions().into_iter().find(|s| {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.place.pad && m.state.is_live() && m.place.pad_project.as_deref() == project
        })
    }

    /// `ScratchPane`: a pad in a terminal editor, one session each, started on first use and kept.
    pub(crate) async fn scratch_pane(
        self: &Arc<Self>,
        sel: &PadSel,
        create: bool,
        env: std::collections::BTreeMap<String, String>,
        size: Option<TermSize>,
        workspace: Option<String>,
    ) -> Result<Reply, Error> {
        let sc = self.scope(sel)?;
        let project = match &sc.sel {
            PadSel::Project(id) => Some(id.clone()),
            _ => None,
        };
        let _one = if create { Some(STARTING.lock().await) } else { None };
        if let Some(s) = self.pad_session(project.as_deref()) {
            return Ok(Reply::Session { info: self.info(&s) });
        }
        if !create {
            return Err(Error::new(
                ErrorCode::NotFound,
                "the scratchpad isn't open yet; `ptyro scratchpad --pane` opens it",
            ));
        }
        let pad = sc.pad;
        pad.ensure().map_err(Self::pad_err)?;
        let command = self.config().scratchpad_editor(true).ok_or_else(|| {
            Error::invalid(
                "no terminal editor found for the scratchpad: install nvim, vim or nano, or set [scratchpad] editor",
            )
        })?;
        let argv = pad::editor_argv(&command, &pad.path);
        let cwd = pad
            .path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.paths.data.clone());
        let spec = SpawnSpec {
            cwd,
            argv,
            name: Some(self.free_name(&match &project {
                Some(_) => format!("scratchpad-{}", sc.name),
                None => "scratchpad".to_owned(),
            })),
            env,
            size,
            workspace,
            pad: true,
            pad_project: project,
            ..SpawnSpec::default()
        };
        let s = self.spawn(spec).await?;
        Ok(Reply::Session { info: self.info(&s) })
    }
}

/// A directory's last component, as the stamp's "where" when it isn't in a worktree.
fn dir_label(dir: &std::path::Path) -> Option<String> {
    dir.file_name().map(|n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_labels_by_its_last_part() {
        assert_eq!(dir_label(std::path::Path::new("/a/b/notes")).as_deref(), Some("notes"));
        assert_eq!(dir_label(std::path::Path::new("/")), None);
    }
}
