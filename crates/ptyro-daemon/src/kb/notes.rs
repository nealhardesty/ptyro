//! Notes in the daemon (SPEC §25.6.4): Markdown files under `<data>/kb/notes/`, and their index
//! in `kb.db`.
//!
//! **The files are the truth.** A note is written as a file first, by you (`ptyro kb note`,
//! the Library, any editor in the folder) or by an agent (`remember`), and then read into
//! `kb.db` through the one way in, as an item of kind `note`. [`Daemon::kb_notes_sync`] makes
//! the index agree with the folder: a file that is new or changed (its time and size, then
//! its hash) is read, and an item whose file is gone is deleted. It runs at the daemon's
//! start, when the folder changes (it is watched), when the settings change, and on `kb sync`.
//!
//! **Who wrote a note is the daemon's word** ([`Daemon::kb_note`]): a request from inside an
//! agent's session is that agent's, under its session's profile, within the limits (8 KiB, 20
//! a session a day, `kb.remember`), its text redacted before the file is written. An agent
//! never overwrites or deletes: `replaces` moves the old file to `_old/`, out of search.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ptyro_core::kb::ingest::Stamp;
use ptyro_core::kb::note::{self, Header};
use ptyro_core::kb::redact::MAX_HITS;
use ptyro_core::kb::{ItemKind, KbItem, Writer};
use ptyro_core::session::SessionKind;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, SessionId, now_ms};
use ptyro_protocol::Reply;
use ptyro_store::kb::{ItemFilter, KbDb, Outcome, Written};
use serde_json::json;

use super::search::KbScope;
use super::{Facts, Prepared, Refused, store_err};
use crate::daemon::Daemon;
use crate::session::Session;

/// How long the folder is quiet before it is read again.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// What a note's header says of a private session: nothing that names it (§25.7a).
const PRIVATE: &str = "a private session";
const INCOGNITO: &str = "an incognito chat";

/// `KbNote`, as it was asked.
#[derive(Debug, Default)]
pub(crate) struct NoteAsk {
    pub text: String,
    pub title: Option<String>,
    pub project: Option<String>,
    pub profile: Option<String>,
    pub tags: Vec<String>,
    pub replaces: Option<String>,
    pub addr: Option<String>,
}

/// One note's file, as the folder has it.
struct Found {
    uri: String,
    path: PathBuf,
    mtime_ms: u64,
    size: u64,
}

impl Found {
    /// What the item keeps of the file, to know it changed: stat first, then the hash.
    fn cursor(&self) -> serde_json::Value {
        json!({ "mtime": self.mtime_ms, "size": self.size })
    }
}

fn stat(uri: String, path: PathBuf) -> Option<Found> {
    let m = std::fs::metadata(&path).ok()?;
    let mtime_ms = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    m.is_file().then_some(Found { uri, path, mtime_ms, size: m.len() })
}

/// The entries of `dir` that `keep` takes, by name; a name that starts with `.` never is.
fn entries(dir: &Path, keep: impl Fn(&std::fs::FileType) -> bool) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| keep(&t)))
        .filter_map(|e| Some((e.file_name().to_str()?.to_owned(), e.path())))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    out.sort();
    out
}

/// Every note in the folder: `<profile>/<project>/<slug>.md`. What is under `_old/` was
/// replaced and is not one.
fn scan(root: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    for (profile, dir) in entries(root, |t| t.is_dir()) {
        for (project, dir) in entries(&dir, |t| t.is_dir()) {
            for (name, path) in entries(&dir, |t| t.is_file()) {
                if let Some(slug) = name.strip_suffix(".md").filter(|s| !s.is_empty()) {
                    out.extend(stat(format!("note://{profile}/{project}/{slug}"), path));
                }
            }
        }
    }
    out
}

/// Write `text` to `path` (0600, its folders 0700), whole or not at all.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    ptyro_core::os::create_private_dirs(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = ptyro_core::os::private(
        std::fs::OpenOptions::new().write(true).create(true).truncate(true),
    )
    .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn io(what: &str, path: &Path, e: std::io::Error) -> Error {
    Error::new(ErrorCode::Internal, format!("{what} {}: {e}", path.display()))
}

impl Daemon {
    /// `<data>/kb/notes`.
    pub(crate) fn kb_notes_dir(&self) -> PathBuf {
        self.paths.data.join("kb").join("notes")
    }

    /// Where a note's file is.
    fn kb_note_path(&self, uri: &str) -> Result<PathBuf, Error> {
        match note::rel(uri) {
            Some(rel) if !rel.contains("..") => Ok(self.kb_notes_dir().join(rel)),
            _ => Err(Error::internal(format!("{uri} is not a note's"))),
        }
    }

    /// The agent's session a request comes from, by the kernel's word (§17): `None` for you.
    pub(super) fn kb_agent(&self, caller: crate::guard::Caller) -> Option<Arc<Session>> {
        if caller.grant.is_some() || caller.is_link() {
            return None;
        }
        let s = self.session_of_pid(caller.pid?)?;
        (s.kind() == SessionKind::Agent).then_some(s)
    }

    /// At the daemon's start: make the folder, read it, and read it again whenever something
    /// in it changes or the settings do (the knowledge base may have been switched on).
    pub(crate) fn start_kb_notes(self: &Arc<Self>) {
        let root = self.kb_notes_dir();
        if let Err(e) = ptyro_core::os::create_private_dirs(&root) {
            tracing::warn!("notes folder {}: {e}", root.display());
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        // (Opening a folder to list it is itself an event where opens are reported (inotify):
        // only what isn't a folder that is there counts, or reading would never end.)
        let watcher = notify_debouncer_mini::new_debouncer(
            DEBOUNCE,
            move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res
                    && events.iter().any(|e| !e.path.is_dir())
                {
                    let _ = tx.send(());
                }
            },
        )
        .and_then(|mut w| {
            w.watcher().watch(&root, notify::RecursiveMode::Recursive)?;
            Ok(w)
        });
        match watcher {
            Ok(w) => *self.kb.notes_watch.locked() = Some(w),
            Err(e) => tracing::warn!(
                "the notes folder isn't watched ({e}): `ptyro kb sync` reads what you change in it"
            ),
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut events = me.events.subscribe();
            loop {
                if let Err(e) = me.kb_notes_sync().await
                    && e.code != ErrorCode::Unsupported
                {
                    tracing::warn!("notes: {}", e.message);
                }
                loop {
                    tokio::select! {
                        changed = rx.recv() => match changed {
                            Some(()) => break,
                            // (Not watched: the settings and `kb sync` still read it.)
                            None => std::future::pending::<()>().await,
                        },
                        e = events.recv() => match e {
                            Ok(ptyro_protocol::DomainEvent::ConfigReloaded { .. }) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            _ => {}
                        },
                    }
                }
            }
        });
    }

    /// Make the index agree with the folder: read every note that is new or changed, and
    /// delete the items whose files are gone. Returns how many notes there are, how many were
    /// read and changed something, and how many items went.
    pub(crate) async fn kb_notes_sync(self: &Arc<Self>) -> Result<(u32, u32, u32), Error> {
        let db = self.kb_db()?;
        let _one = self.kb.notes_lock.lock().await;
        let root = self.kb_notes_dir();
        let found = tokio::task::spawn_blocking(move || scan(&root))
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        let filter = ItemFilter { kinds: vec![ItemKind::Note], ..ItemFilter::default() };
        let kept = db.list(filter).await.map_err(store_err)?;
        let (mut changed, mut stale) = (0, false);
        for f in &found {
            let had = kept.iter().find(|i| i.uri == f.uri);
            let cursor = f.cursor();
            // As it was when it was read, or when it was refused: not read again.
            if had.is_some_and(|i| i.cursor.as_ref() == Some(&cursor))
                || self.kb.notes_refused.locked().get(&f.uri) == Some(&cursor)
            {
                continue;
            }
            match self.kb_note_ingest(f, had, None).await {
                Ok(w) => {
                    self.kb.notes_refused.locked().remove(&f.uri);
                    changed += u32::from(w.outcome != Outcome::Unchanged);
                }
                Err(why) => {
                    let why = match why {
                        Refused::Skip(s) => s.to_string(),
                        Refused::Error(e) => e.message,
                    };
                    self.kb.skip(&f.uri, why);
                    self.kb.notes_refused.locked().insert(f.uri.clone(), cursor);
                    // What it held before is no longer what the file says: not found either.
                    if let Some(old) = had {
                        self.kb_note_drop(&db, old).await?;
                        stale = true;
                    }
                }
            }
        }
        let mut removed = 0;
        for i in kept.iter().filter(|i| !found.iter().any(|f| f.uri == i.uri)) {
            self.kb_note_drop(&db, i).await?;
            removed += 1;
        }
        self.kb.notes_refused.locked().retain(|uri, _| found.iter().any(|f| &f.uri == uri));
        if removed > 0 || stale {
            self.kb_changed([ItemKind::Note]);
        }
        Ok((found.len() as u32, changed, removed))
    }

    /// A note's item goes, text and all, with no tombstone: its file is the truth, and a file
    /// of that name written later is a note again.
    async fn kb_note_drop(&self, db: &KbDb, item: &KbItem) -> Result<usize, Error> {
        let chunks = db.forget(item.id.clone()).await.map_err(store_err)?.unwrap_or(0);
        db.delete_item(item.id.clone()).await.map_err(store_err)?;
        tracing::info!("knowledge base: {} is gone", item.uri);
        Ok(chunks)
    }

    /// Forget a note (§25.12): its file is deleted, then its item. Returns its pieces.
    pub(super) async fn kb_note_forget(&self, db: &KbDb, item: &KbItem) -> Result<usize, Error> {
        let path = self.kb_note_path(&item.uri)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("can't delete", &path, e)),
        }
        self.kb_note_drop(db, item).await
    }

    /// One note's file through the way in (§25.5). The folder says whose it is and what it is
    /// about; its header says who wrote it. `had`: its item as it was; `workspace`: where an
    /// agent wrote it just now (a file doesn't say, so the item keeps what it had).
    async fn kb_note_ingest(
        self: &Arc<Self>,
        f: &Found,
        had: Option<&KbItem>,
        workspace: Option<String>,
    ) -> Result<Written, Refused> {
        let text = std::fs::read_to_string(&f.path).map_err(|e| io("can't read", &f.path, e))?;
        let (mut source, header) = note::source(&f.uri, &text, f.mtime_ms);
        source.cursor = Some(f.cursor());
        // A file with no date of its own was written when it was first seen.
        if header.created_ms.is_none()
            && let Some(t) = had.and_then(|i| i.started_ms)
        {
            source.started_ms = Some(t.min(f.mtime_ms));
        }
        let (profile, project, _) = note::place(&f.uri)
            .ok_or_else(|| Error::internal(format!("{} is not a note's", f.uri)))?;
        let project = project.and_then(|slug| {
            let reg = self.registry.locked();
            reg.projects
                .iter()
                .find(|p| p.machine.is_none() && p.slug == slug)
                .map(|p| p.id.clone())
        });
        let writer = if header.agent().is_some() { Writer::Agent } else { Writer::User };
        let stamp = Stamp {
            project,
            workspace: workspace.or_else(|| had.and_then(|i| i.workspace.clone())),
            agent: header.agent().map(str::to_owned),
            // (The header's word for a session ptyro ran; a chat's or a private one's isn't.)
            session: header.session.clone().filter(|s| s.parse::<SessionId>().is_ok()),
            ..Stamp::new(writer, profile.map(str::to_owned))
        };
        // (No path for the never list: `credentials-rotation.md` is a note like any other.)
        let facts = Facts { bytes: Some(f.size), ..Facts::default() };
        self.kb_ingest(Prepared { source, stamp, facts }).await
    }

    /// A project a note is about, as it was asked: `None` for none (`_global`, or nothing).
    fn kb_note_project(&self, asked: Option<&str>) -> Result<Option<(String, String)>, Error> {
        match asked.map(str::trim).filter(|p| !p.is_empty() && *p != note::GLOBAL) {
            None => Ok(None),
            Some(p) => {
                let p = self.find_project(p)?;
                if p.machine.is_some() {
                    return Err(Error::invalid(format!(
                        "{} is another machine's project: a note about it is written there",
                        p.slug
                    )));
                }
                Ok(Some((p.id, p.slug)))
            }
        }
    }

    /// **`KbNote`**: write a note's file and index it. From inside an agent's session it is
    /// `remember`: that agent's note, in its profile, within the limits.
    pub(super) async fn kb_note(
        self: &Arc<Self>,
        ask: NoteAsk,
        caller: crate::guard::Caller,
        scope: Option<&KbScope>,
    ) -> Result<Reply, Error> {
        let db = self.kb_db()?;
        let agent = self.kb_agent(caller);
        if ask.text.trim().is_empty() {
            return Err(Error::invalid("a note needs its text"));
        }
        if let Some(addr) = &ask.addr {
            if agent.is_some() {
                return Err(Error::new(
                    ErrorCode::PermissionDenied,
                    "an agent never writes over a note: write a new one with `replaces`, and the old one is kept aside",
                ));
            }
            let (item, _) = self.kb_find(&db, addr, None).await?;
            if item.kind != ItemKind::Note {
                return Err(Error::invalid(format!("{addr} is a {}, not a note", item.kind)));
            }
            let path = self.kb_note_path(&item.uri)?;
            let _one = self.kb.notes_lock.lock().await;
            write(&path, &ask.text).map_err(|e| io("can't write", &path, e))?;
            return self.kb_noted(&db, item.uri.clone(), path, Some(&item), None).await;
        }

        let config = self.config();
        let mut header = Header { created_ms: Some(now_ms()), ..Header::default() };
        let (profile, project, workspace);
        match &agent {
            Some(s) => {
                let m = s.meta.locked().clone();
                let name = m.place.agent.clone().or(m.adapter.clone());
                if !config.kb.remember || self.kb_agent_off(name.as_deref()) {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        "agents don't write notes here (`kb.remember` is off): say what is worth keeping, and the user can note it",
                    ));
                }
                if ask.text.len() > note::AGENT_MAX {
                    return Err(Error::invalid(format!(
                        "a note is at most {} KiB and this is {} bytes: keep the decision and its reason, and leave the rest out",
                        note::AGENT_MAX / 1024,
                        ask.text.len()
                    )));
                }
                self.kb_remember_count(&s.id.full(), false)?;
                profile = Some(m.place.profile.clone());
                project = match &ask.project {
                    Some(p) => self.kb_note_project(Some(p))?,
                    None => m
                        .place
                        .project
                        .as_deref()
                        .and_then(|id| self.project_by_id(id))
                        .map(|p| (p.id, p.slug)),
                };
                workspace = self.kb_workspace_id(m.place.workspace.as_deref());
                header.by = Some(name.unwrap_or_else(|| "agent".into()));
                // A private session is named by nothing (§25.7a); a chat by its address.
                header.session = Some(match &m.place.chat {
                    _ if m.place.private => PRIVATE.to_owned(),
                    Some(chat) if self.chat_incognito(chat).await => INCOGNITO.to_owned(),
                    Some(chat) => format!("c:{chat}"),
                    None => s.id.full(),
                });
            }
            None => {
                let asked = ask.profile.filter(|p| !p.trim().is_empty() && p != note::EVERYONE);
                profile = self.check_profile(asked)?;
                project = self.kb_note_project(ask.project.as_deref())?;
                workspace = None;
                header.by = Some(note::YOU.to_owned());
            }
        }
        header.tags = ask.tags.iter().filter_map(|t| note::tag(t)).collect();
        // What it takes the place of: a note the caller may see.
        let old = match &ask.replaces {
            Some(addr) => {
                let (item, _) = self.kb_find(&db, addr, scope).await?;
                if item.kind != ItemKind::Note {
                    return Err(Error::invalid(format!(
                        "{addr} is a {}: only a note is replaced by a note",
                        item.kind
                    )));
                }
                header.replaces = Some(item.uri.clone());
                Some(item)
            }
            None => None,
        };
        let body = note::compose(&ask.text, ask.title.as_deref());
        let mut text = note::render(&header, &body);
        // An agent's words are redacted before they are a file (§25.7): the profile's secrets,
        // the credential shapes, your patterns.
        if agent.is_some() {
            let secrets = match profile.as_deref() {
                Some(p) => crate::redact::secrets(&self.profile_plan(p).await?),
                None => Vec::new(),
            };
            let r = self.kb_rules().redactor.with_secrets(secrets).redact(&text);
            if r.hits > MAX_HITS {
                return Err(Error::invalid(format!(
                    "it holds {} things that look like credentials: a note is for decisions and facts, never for keys",
                    r.hits
                )));
            }
            text = r.text;
        }
        let title = note::title(&body).0.unwrap_or_default();
        let dir = self
            .kb_notes_dir()
            .join(profile.as_deref().unwrap_or(note::EVERYONE))
            .join(project.as_ref().map_or(note::GLOBAL, |(_, slug)| slug.as_str()));
        let _one = self.kb.notes_lock.lock().await;
        // A name of its own: `retry-policy`, then `retry-policy-2`.
        let base = note::slug(&title);
        let (mut slug, mut k) = (base.clone(), 1);
        while dir.join(format!("{slug}.md")).exists() {
            k += 1;
            slug = format!("{base}-{k}");
        }
        let path = dir.join(format!("{slug}.md"));
        write(&path, &text).map_err(|e| io("can't write", &path, e))?;
        let uri =
            note::uri(profile.as_deref(), project.as_ref().map(|(_, slug)| slug.as_str()), &slug);
        let reply = self.kb_noted(&db, uri, path, None, workspace).await?;
        if let Some(s) = &agent {
            self.kb_remember_count(&s.id.full(), true)?;
        }
        // The one it replaces is kept on disk, aside, and is out of search.
        if let Some(old) = old {
            let from = self.kb_note_path(&old.uri)?;
            let aside = from.parent().unwrap_or(Path::new(".")).join(note::OLD);
            let name = from.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let mut to = aside.join(&name);
            if to.exists() {
                to = aside.join(format!("{}-{}.md", name.trim_end_matches(".md"), now_ms()));
            }
            ptyro_core::os::create_private_dirs(&aside).map_err(|e| io("can't make", &aside, e))?;
            match std::fs::rename(&from, &to) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io("can't set aside", &from, e)),
            }
            self.kb_note_drop(&db, &old).await?;
            self.kb_changed([ItemKind::Note]);
        }
        Ok(reply)
    }

    /// A note's file was written: index it now (the watcher would, a moment later), and
    /// answer with its item.
    async fn kb_noted(
        self: &Arc<Self>,
        db: &KbDb,
        uri: String,
        path: PathBuf,
        had: Option<&KbItem>,
        workspace: Option<String>,
    ) -> Result<Reply, Error> {
        let found = stat(uri, path.clone())
            .ok_or_else(|| Error::internal(format!("{} is gone", path.display())))?;
        let w = match self.kb_note_ingest(&found, had, workspace).await {
            Ok(w) => w,
            Err(why) => {
                // Not kept: no file is left that the index doesn't have (a new one); one
                // that was written again stays as you wrote it, and is no longer found.
                match had {
                    None => {
                        let _ = std::fs::remove_file(&path);
                    }
                    Some(old) => {
                        self.kb_note_drop(db, old).await?;
                        self.kb_changed([ItemKind::Note]);
                    }
                }
                return Err(match why {
                    Refused::Skip(s) => Error::invalid(format!("the note is not kept: {s}")),
                    Refused::Error(e) => e,
                });
            }
        };
        let item = db
            .item(w.item)
            .await
            .map_err(store_err)?
            .ok_or_else(|| Error::internal("the note was written and is not in the index"))?;
        let item = self.kb_listed(db, vec![item]).await?.remove(0);
        Ok(Reply::KbNoted { item, path: path.display().to_string() })
    }

    /// How many notes a session's agent wrote today: refused past the limit (a runaway
    /// loop), counted when `wrote`.
    fn kb_remember_count(&self, session: &str, wrote: bool) -> Result<(), Error> {
        let today = now_ms() / 86_400_000;
        let mut all = self.kb.remembered.locked();
        all.retain(|_, (day, _)| *day == today);
        let n = all.entry(session.to_owned()).or_insert((today, 0));
        if n.1 >= note::AGENT_A_DAY {
            return Err(Error::new(
                ErrorCode::Busy,
                format!(
                    "this session wrote {} notes today, which is the most one may: no more are kept until tomorrow",
                    note::AGENT_A_DAY
                ),
            ));
        }
        n.1 += u32::from(wrote);
        Ok(())
    }

    /// **`KbNotes`**: the notes a caller may see, newest first: a project's (`_global`: those
    /// about none), one writer's, or all.
    pub(super) async fn kb_notes(
        &self,
        project: Option<String>,
        writer: Option<Writer>,
        scope: Option<&KbScope>,
    ) -> Result<Vec<KbItem>, Error> {
        let db = self.kb_db()?;
        let project = match project.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(p) => Some(self.kb_note_project(Some(p))?.map(|(id, _)| id)),
        };
        let filter = ItemFilter {
            kinds: vec![ItemKind::Note],
            profile: scope.and_then(|s| s.profile.clone()),
            writer,
            ..ItemFilter::default()
        };
        let items = db.list(filter).await.map_err(store_err)?;
        Ok(items
            .into_iter()
            .filter(|i| scope.is_none_or(|s| s.sees(i)))
            .filter(|i| project.as_ref().is_none_or(|p| &i.project == p))
            .collect())
    }

    /// **`KbNoteRead`**: a note's file as it is.
    pub(super) async fn kb_note_read(
        &self,
        addr: &str,
        scope: Option<&KbScope>,
    ) -> Result<Reply, Error> {
        let db = self.kb_db()?;
        let (item, _) = self.kb_find(&db, addr, scope).await?;
        if item.kind != ItemKind::Note {
            return Err(Error::invalid(format!(
                "{addr} is a {}, not a note: `ptyro kb show` reads it",
                item.kind
            )));
        }
        let path = self.kb_note_path(&item.uri)?;
        let text = std::fs::read_to_string(&path).map_err(|e| io("can't read", &path, e))?;
        let item = self.kb_listed(&db, vec![item]).await?.remove(0);
        Ok(Reply::KbNoteText { item, text, path: path.display().to_string() })
    }
}

#[cfg(test)]
mod tests;
