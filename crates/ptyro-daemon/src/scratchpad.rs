//! The scratchpad (NOTE1, SPEC §13.6): one notes file the daemon keeps, serves to every client
//! and tells them about when it changes. Writing it is `ptyro_core::scratchpad` (atomic, with a
//! history and a three-way merge); this is what the daemon adds: the path from `[scratchpad]`,
//! `$PTYRO_SCRATCHPAD` in every session, a watcher that notices an editor's save, the editor as
//! a session in a pane, and ticking an item once the worktree started from it is finished.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ptyro_core::scratchpad::{self as pad, Entry, Pad};
use ptyro_core::session::{SessionKind, SpawnSpec};
use ptyro_core::{Error, ErrorCode, TermSize};
use ptyro_protocol::{DomainEvent, PadDoc, PadMark, Reply};

use crate::daemon::Daemon;
use crate::session::Session;

/// How many recent revisions' texts to keep as merge bases.
const RING: usize = 64;
/// How often the file is looked at for a change made outside.
const POLL: Duration = Duration::from_millis(700);

/// The pad's revisions (merge bases) and the lock that serialises read-modify-write.
#[derive(Default)]
pub struct PadState {
    lock: Mutex<()>,
    ring: Mutex<VecDeque<(String, String)>>,
    /// The revision last announced, so a write isn't announced twice.
    seen: Mutex<String>,
}

/// One pane at a time starts the editor.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Daemon {
    /// Where the pad is: `[scratchpad] path`, else `scratchpad.md` in the data directory.
    pub(crate) fn pad_path(&self) -> PathBuf {
        self.config().scratchpad_path(&self.paths.data)
    }

    pub(crate) fn pad(&self) -> Pad {
        Pad::new(
            self.pad_path(),
            self.paths.data.join("scratchpad.history"),
            self.config().scratchpad.keep,
        )
    }

    fn pad_err(e: impl std::fmt::Display) -> Error {
        Error::internal(format!("scratchpad: {e}"))
    }

    fn remember(&self, text: &str) {
        let r = pad::rev(text);
        let mut ring = self.pad_state.ring.lock().expect("pad ring");
        if ring.iter().any(|(k, _)| *k == r) {
            return;
        }
        if ring.len() >= RING {
            ring.pop_front();
        }
        ring.push_back((r, text.to_owned()));
    }

    fn base_of(&self, rev: &str) -> Option<String> {
        let found = self
            .pad_state
            .ring
            .lock()
            .expect("pad ring")
            .iter()
            .find(|(k, _)| k == rev)
            .map(|(_, t)| t.clone());
        found.or_else(|| self.pad().snapshot_text(rev))
    }

    fn doc(&self, text: String) -> PadDoc {
        PadDoc::of(self.pad_path(), text)
    }

    /// Remember `text`, and tell every client when it is a revision they haven't heard of.
    fn pad_changed(&self, text: &str, by: Option<String>) {
        self.remember(text);
        let doc = self.doc(text.to_owned());
        {
            let mut seen = self.pad_state.seen.lock().expect("pad seen");
            if *seen == doc.rev {
                return;
            }
            seen.clone_from(&doc.rev);
        }
        let _ = self.events.send(DomainEvent::ScratchChanged { rev: doc.rev, open: doc.open, by });
    }

    /// Make the file, and watch it: a save from an editor, a `git pull` into a synced folder or
    /// an `echo >>` all reach every client as `ScratchChanged`.
    pub(crate) fn start_pad_watcher(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut last: Option<(std::time::SystemTime, u64, PathBuf)> = None;
            loop {
                let pad = me.pad();
                let stat = std::fs::metadata(&pad.path)
                    .ok()
                    .and_then(|m| Some((m.modified().ok()?, m.len(), pad.path.clone())));
                if stat.is_none() {
                    if let Err(e) = pad.ensure() {
                        tracing::warn!("scratchpad {}: {e}", pad.path.display());
                    }
                } else if stat != last {
                    last = stat;
                    let _lock = me.pad_state.lock.lock().expect("pad lock");
                    if let Ok(text) = pad.read() {
                        let known = me.pad_state.seen.lock().expect("pad seen").clone();
                        if known.is_empty() {
                            // First look after start: nothing to announce, but a base to merge from.
                            me.remember(&text);
                            *me.pad_state.seen.lock().expect("pad seen") = pad::rev(&text);
                        } else if known != pad::rev(&text) {
                            let _ = pad.snapshot_lazy(&text);
                            me.pad_changed(&text, Some("file".into()));
                        }
                    }
                }
                tokio::time::sleep(POLL).await;
            }
        });
    }

    pub(crate) fn scratch_get(&self) -> Result<Reply, Error> {
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let text = self.pad().read().map_err(Self::pad_err)?;
        self.remember(&text);
        Ok(Reply::Scratch { doc: self.doc(text) })
    }

    pub(crate) fn scratch_put(
        &self,
        base: &str,
        text: &str,
        by: Option<String>,
    ) -> Result<Reply, Error> {
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let pad = self.pad();
        let cur = pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let base_text = self.base_of(base);
        let m = pad.put(base, base_text.as_deref(), text).map_err(Self::pad_err)?;
        self.pad_changed(&m.text, by);
        let merged = m.text != text;
        Ok(Reply::ScratchPut { doc: self.doc(m.text), merged, conflicts: m.conflicts })
    }

    /// Who is adding: the stamp's worktree (or directory) and, for an agent, its name.
    fn whence(
        &self,
        session: Option<&str>,
        cwd: Option<&std::path::Path>,
    ) -> (Option<String>, Option<String>) {
        if let Some(s) = session.and_then(|a| self.resolve(a).ok()) {
            let (wt, name, agent, dir) = {
                let m = s.meta.lock().expect("meta");
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

    pub(crate) fn scratch_add(
        &self,
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
        let (whence, by) = if stamp { self.whence(session, cwd) } else { (None, None) };
        let entry = Entry {
            text: text.to_owned(),
            section,
            note,
            at: if stamp { ptyro_pty::local_datetime() } else { None },
            whence,
            by: by.clone(),
        };
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let pad = self.pad();
        let cur = pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let text = pad.add(&entry).map_err(Self::pad_err)?;
        self.pad_changed(&text, by.or(Some("cli".into())));
        Ok(Reply::Scratch { doc: self.doc(text) })
    }

    pub(crate) fn scratch_todo(&self, all: bool) -> Result<Reply, Error> {
        let text = self.pad().read().map_err(Self::pad_err)?;
        let items = pad::items(&text).into_iter().filter(|i| all || !i.done).collect();
        Ok(Reply::ScratchItems { rev: pad::rev(&text), items })
    }

    pub(crate) fn scratch_mark(&self, sel: &str, mark: &PadMark) -> Result<Reply, Error> {
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let pad = self.pad();
        let cur = pad.read().map_err(Self::pad_err)?;
        self.remember(&cur);
        let item = pad::select(&cur, sel).map_err(Error::invalid)?;
        let text = pad
            .modify(|t| match mark {
                PadMark::Done { pr } => pad::tick(t, item.line, *pr),
                PadMark::Open => pad::untick(t, item.line),
                PadMark::Started { worktree } => pad::tag_worktree(t, item.line, worktree),
            })
            .map_err(Error::invalid)?;
        self.pad_changed(&text, Some("cli".into()));
        let item = pad::items(&text).into_iter().find(|i| i.line == item.line);
        Ok(Reply::ScratchItems { rev: pad::rev(&text), items: item.into_iter().collect() })
    }

    pub(crate) fn scratch_history(&self) -> Result<Reply, Error> {
        Ok(Reply::ScratchLog { snapshots: self.pad().history() })
    }

    pub(crate) fn scratch_restore(&self, rev: &str) -> Result<Reply, Error> {
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let text = self.pad().restore(rev).map_err(Error::invalid)?;
        self.pad_changed(&text, Some("cli".into()));
        Ok(Reply::Scratch { doc: self.doc(text) })
    }

    /// The items started from worktree `qualified` are ticked (`tick_on_finish`).
    pub(crate) fn pad_finished(&self, qualified: &str, pr: Option<u64>) {
        if !self.config().scratchpad.tick_on_finish {
            return;
        }
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let pad = self.pad();
        let Ok(cur) = pad.read() else { return };
        let mine: Vec<u32> = pad::items(&cur)
            .into_iter()
            .filter(|i| !i.done && i.worktree.as_deref() == Some(qualified))
            .map(|i| i.line)
            .collect();
        if mine.is_empty() {
            return;
        }
        self.remember(&cur);
        let done = pad.modify(|t| {
            let mut t = t.to_owned();
            for line in &mine {
                t = pad::tick(&t, *line, pr)?;
            }
            Ok(t)
        });
        match done {
            Ok(text) => self.pad_changed(&text, Some(format!("finished {qualified}"))),
            Err(e) => tracing::warn!("scratchpad: ticking items of {qualified}: {e}"),
        }
    }

    /// The item on `line` starts in worktree `qualified` (`WorktreeSpec::pad_item`).
    pub(crate) fn pad_started(&self, line: u32, qualified: &str) {
        let _l = self.pad_state.lock.lock().expect("pad lock");
        let pad = self.pad();
        if let Ok(cur) = pad.read() {
            self.remember(&cur);
        }
        match pad.modify(|t| pad::tag_worktree(t, line, qualified)) {
            Ok(text) => self.pad_changed(&text, Some(format!("started {qualified}"))),
            Err(e) => tracing::warn!("scratchpad: tagging line {line}: {e}"),
        }
    }

    /// The live editor session, if there is one.
    fn pad_session(&self) -> Option<Arc<Session>> {
        self.sessions().into_iter().find(|s| {
            let m = s.meta.lock().expect("meta");
            m.place.pad && m.state.is_live()
        })
    }

    /// `ScratchPane`: the pad in a terminal editor, one session, started on first use and kept.
    pub(crate) async fn scratch_pane(
        self: &Arc<Self>,
        create: bool,
        env: std::collections::BTreeMap<String, String>,
        size: Option<TermSize>,
        workspace: Option<String>,
    ) -> Result<Reply, Error> {
        let _one = if create { Some(STARTING.lock().await) } else { None };
        if let Some(s) = self.pad_session() {
            return Ok(Reply::Session { info: self.info(&s) });
        }
        if !create {
            return Err(Error::new(
                ErrorCode::NotFound,
                "the scratchpad isn't open yet; `ptyro scratchpad --pane` opens it",
            ));
        }
        let pad = self.pad();
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
            name: Some(self.free_name("scratchpad")),
            env,
            size,
            workspace,
            pad: true,
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
