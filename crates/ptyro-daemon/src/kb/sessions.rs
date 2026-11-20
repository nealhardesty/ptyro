//! Agent sessions into the knowledge base (SPEC §25.6.1): every Claude Code and Codex session
//! that runs in ptyro is read from the agent's own transcript as it happens.
//!
//! **When.** A turn's end and the session's end as its hooks say them ([`Daemon::kb_hook`],
//! [`Daemon::kb_hooked`]),
//! a turn's end as ptyro itself counts it and the process's exit ([`Daemon::kb_session_changed`]),
//! and once at the daemon's start for every transcript that grew past its cursor
//! ([`Daemon::kb_catch_up`]). All of them only queue a [`SessionJob`].
//!
//! **What.** One item a conversation, `session://<adapter>/<conversation id>`, carried on
//! across a resume (`session://<adapter>/<ptyro's session id>` for an agent with no transcript). The transcript is read from the item's cursor (`{path, dev, ino, offset}`),
//! or from its start when it shrank or is another file; its parts are appended by anchor, so a
//! piece read twice doubles nothing. The first chunk is the session's card
//! (`ptyro_core::kb::card`), made again with each piece. An agent whose transcript isn't read
//! gets the card alone, from what the daemon knows of its session.
//!
//! **`Stop` is not proof the turn is in the file**: Claude writes the turn's last line about
//! 15 ms after the hook. A read that doesn't find the hook's reply as the last thing said is
//! tried again after [`RETRY`], in the background: the hook never waits.
//!
//! **Never**: a chat's engine (its text is in `chat.db`), a session that isn't an agent's, one
//! that isn't recorded, and (KB1.14) a private one.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ptyro_agents::{Adapter, Transcript, TranscriptAt};
use ptyro_core::kb::card::{self, Card};
use ptyro_core::kb::ingest::{Body, Source, Stamp};
use ptyro_core::kb::{ChunkKind, ItemKind, KbItem, Writer};
use ptyro_core::session::SessionKind;
use ptyro_core::status::{Signal, Sourced};
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, SessionId, now_ms};
use ptyro_store::kb::KbDb;
use serde_json::{Value, json};

use super::{Facts, Job, Prepared, Refused, store_err};
use crate::daemon::Daemon;
use crate::session::Session;

/// When a read that lacks the turn's reply is tried again: twice, then the next trigger has it.
const RETRY: [Duration; 2] = [Duration::from_millis(250), Duration::from_secs(2)];
/// The most of a transcript taken in one read; a longer one is read in several.
const READ_MAX: u64 = 32 << 20;

/// A session to read, as a trigger queued it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionJob {
    pub session: SessionId,
    /// The conversation it was in then (a `/clear` gives it another), when the trigger knew.
    pub conversation: Option<String>,
    /// The turn's last assistant message, as its `Stop` hook gave it: what the read must find.
    pub expect: Option<String>,
    /// How many times it was tried again.
    pub attempt: usize,
}

impl SessionJob {
    /// Whether `other` asks for the same read.
    pub(super) fn same(&self, other: &Self) -> bool {
        self.session == other.session && self.conversation == other.conversation
    }
}

/// What the daemon knows of a session to read: all of it from the session, the registry and
/// the configuration, none from the transcript.
struct Target {
    uri: String,
    adapter: String,
    conversation: String,
    cwd: PathBuf,
    /// The file a hook named.
    hook: Option<PathBuf>,
    stamp: Stamp,
    facts: Facts,
    /// The card as far as the daemon can fill it: who and where, and (for a session with no
    /// transcript) what it was asked and how it ended.
    card: Card,
    /// The session's own title.
    title: Option<String>,
}

/// What a read found.
struct Piece {
    transcript: Transcript,
    /// Where it stopped, for the item.
    cursor: Value,
    /// Read from the file's start: what the item holds of it is in the piece again.
    whole: bool,
    /// The file holds more than one read takes.
    more: bool,
    /// The title the agent keeps outside the file.
    title: Option<String>,
}

#[cfg(unix)]
fn file_id(m: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

#[cfg(not(unix))]
fn file_id(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Whether the file a cursor names is not where the cursor left it: longer, shorter, or
/// another file.
fn grew(cursor: &Value) -> bool {
    let Some(path) = cursor["path"].as_str() else { return false };
    let Ok(m) = std::fs::metadata(path) else { return false };
    let (dev, ino) = file_id(&m);
    Some(m.len()) != cursor["offset"].as_u64()
        || cursor["dev"].as_u64() != Some(dev)
        || cursor["ino"].as_u64() != Some(ino)
}

/// Read `path` on from `cursor`: from its offset when the file is the one it was and no
/// shorter, else from the start (a rewind, a file put in its place).
fn read(adapter: &dyn Adapter, path: &Path, cursor: Option<&Value>, max: u64) -> Option<Piece> {
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let (dev, ino) = file_id(&meta);
    let mut from = cursor
        .filter(|c| c["dev"].as_u64() == Some(dev) && c["ino"].as_u64() == Some(ino))
        .and_then(|c| c["offset"].as_u64())
        .filter(|at| *at <= meta.len())
        .unwrap_or(0);
    // A cursor stands after a line's end: where it doesn't, the file was cut back and has
    // grown past it again.
    if from > 0 {
        let mut before = [0u8; 1];
        file.seek(SeekFrom::Start(from - 1)).ok()?;
        if file.read_exact(&mut before).is_err() || before[0] != b'\n' {
            from = 0;
        }
    }
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    (&mut file).take(max).read_to_end(&mut bytes).ok()?;
    let mut transcript = adapter.read_transcript(&bytes, from)?;
    let full = bytes.len() as u64 == max;
    if full && transcript.to == from {
        // One line longer than a read: passed over, as any line too long is, once it ends.
        let mut rest = std::io::BufReader::new(&mut file);
        let mut line = Vec::new();
        let n = (&mut rest).take(1 << 30).read_until(b'\n', &mut line).ok()?;
        if line.last() == Some(&b'\n') {
            transcript.to = from + max + n as u64;
        }
    }
    Some(Piece {
        cursor: json!({ "path": path, "dev": dev, "ino": ino, "offset": transcript.to }),
        whole: from == 0,
        more: full && transcript.to > from && transcript.to < meta.len(),
        transcript,
        title: None,
    })
}

impl Daemon {
    /// A hook of `s` arrived (`signals` are what its adapter made of it): remember the
    /// transcript it names, and when it ends a turn or the session give the read to queue
    /// ([`Daemon::kb_hooked`], once the signals are applied). Only a lock: nothing here waits.
    pub(crate) fn kb_hook(
        &self,
        s: &Session,
        adapter: &str,
        event: &str,
        payload: &Value,
        signals: &[Sourced],
    ) -> Option<SessionJob> {
        let (path, ends) = ptyro_agents::adapter(adapter).hook_transcript(event, payload);
        if let Some(path) = path {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if m.transcript.as_ref() != Some(&path) {
                m.transcript = Some(path);
            }
        }
        if !ends {
            return None;
        }
        let said = |pick: fn(&Signal) -> Option<&String>| {
            signals.iter().find_map(|x| pick(&x.signal)).cloned()
        };
        Some(SessionJob {
            session: s.id,
            conversation: said(|x| match x {
                Signal::Conversation { id } => Some(id),
                _ => None,
            })
            .filter(|id| crate::agents::conversation_id_ok(id)),
            expect: said(|x| match x {
                Signal::Reply { text } => Some(text),
                _ => None,
            }),
            attempt: 0,
        })
    }

    /// Queue the read a hook asked for. (After its signals were applied: the session then
    /// knows the turn's reply, for its card.)
    pub(crate) fn kb_hooked(&self, s: &Session, job: SessionJob) {
        self.kb_queue_session(s, job);
    }

    /// A turn of `s` ended as ptyro counts it, or its process exited: queue a read.
    pub(crate) fn kb_session_changed(&self, s: &Session) {
        self.kb_queue_session(
            s,
            SessionJob { session: s.id, conversation: None, expect: None, attempt: 0 },
        );
    }

    fn kb_queue_session(&self, s: &Session, job: SessionJob) {
        let ours = {
            let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.kind == SessionKind::Agent && m.place.chat.is_none()
        };
        let kb = self.config().kb;
        if ours && kb.enabled && kb.sessions {
            self.kb_enqueue(Job::Session(job));
        }
    }

    /// At the daemon's start, once its sessions are back (a restart's dormant ones, a live
    /// upgrade's running ones): queue every session whose transcript is not where its item's
    /// cursor left it, or that has no item yet. The queue itself is never carried over.
    pub(crate) async fn kb_catch_up(self: &Arc<Self>) {
        let kb = self.config().kb;
        if !kb.enabled || !kb.sessions {
            return;
        }
        let Ok(db) = self.kb_db() else { return };
        let cursors: HashMap<String, Option<Value>> = match db.cursors(ItemKind::Session).await {
            Ok(c) => c.into_iter().map(|c| (c.uri, c.cursor)).collect(),
            Err(e) => return tracing::warn!("knowledge base: {}", store_err(e).message),
        };
        let mut ours = HashSet::new();
        for s in self.sessions() {
            let Some(t) = self.kb_target(&s, None) else { continue };
            let reads = ptyro_agents::adapter(&t.adapter).read_transcript(&[], 0).is_some();
            // (A session with no transcript has nothing that could have grown: its card is
            // made at its turns' ends.)
            let behind = match cursors.get(&t.uri) {
                Some(Some(cursor)) => grew(cursor),
                Some(None) | None => true,
            };
            if reads && behind {
                self.kb_session_changed(&s);
            }
            ours.insert(t.uri);
        }
        for (uri, cursor) in cursors {
            if !ours.contains(&uri) && cursor.as_ref().is_some_and(grew) {
                self.kb_enqueue(Job::Item(uri));
            }
        }
    }

    /// What the daemon knows of `s` as a source, or `None` when it is never one: not an
    /// agent's session, a chat's engine, or an agent whose conversation isn't known yet.
    fn kb_target(&self, s: &Session, conversation: Option<String>) -> Option<Target> {
        let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if m.kind != SessionKind::Agent || m.place.chat.is_some() {
            return None;
        }
        let adapter = m.adapter.clone()?;
        let reads = ptyro_agents::adapter(&adapter).read_transcript(&[], 0).is_some();
        // An agent with a transcript is found by its conversation, and by nothing else. One
        // with none is ptyro's session: that id is there from the start, where the agent's
        // own may be learned late or never.
        let conversation =
            if reads { conversation.or(m.conversation.clone())? } else { s.id.full() };
        let worktree = m.place.worktree.as_deref().and_then(|id| self.worktree_by_id(id));
        let project = m.place.project.as_deref().and_then(|id| self.project_by_id(id));
        let branch = worktree.as_ref().and_then(|w| w.branch.clone());
        let stamp = Stamp {
            project: m.place.project.clone(),
            workspace: self.kb_workspace_id(m.place.workspace.as_deref()),
            worktree: m.place.worktree.clone(),
            branch: branch.clone(),
            agent: m.place.agent.clone().or(Some(adapter.clone())),
            model: m.model.clone(),
            session: Some(s.id.full()),
            ..Stamp::new(Writer::Agent, Some(m.place.profile.clone()))
        };
        let card = Card {
            task: m.first_prompt.clone(),
            outcome: m.last_reply.clone(),
            files: Vec::new(),
            agent: stamp.agent.clone(),
            model: m.model.clone(),
            project: project.map(|p| p.slug).or(worktree.as_ref().map(|w| w.project_slug.clone())),
            worktree: worktree.map(|w| w.name),
            branch,
            started_ms: Some(s.id.created_ms()),
            ended_ms: m.ended_ms,
            turns: m.turns,
        };
        let agent_off = self.kb_agent_off(stamp.agent.as_deref());
        Some(Target {
            uri: format!("session://{adapter}/{conversation}"),
            adapter,
            conversation,
            cwd: m.cwd,
            hook: m.transcript,
            stamp,
            facts: Facts {
                session: true,
                agent_off,
                // (Its project's own switch is a file to read: asked when the session is
                // read, `kb_session`, never where a session is only looked at.)
                project_off: false,
                unrecorded: m.place.unrecorded,
                // (§25.7a: the session's flag arrives with KB1.14.)
                private: false,
                ..Facts::default()
            },
            card,
            title: m.title,
        })
    }

    /// The actor's part of a [`SessionJob`]: read, write, and come back if the turn's reply
    /// was not in the file yet. Gives the URI too, for what was refused.
    pub(super) async fn kb_session(
        self: &Arc<Self>,
        job: SessionJob,
    ) -> (String, Result<(), Refused>) {
        let target =
            self.session(job.session).and_then(|s| self.kb_target(&s, job.conversation.clone()));
        let Some(mut target) = target else { return (String::new(), Ok(())) };
        let uri = target.uri.clone();
        target.facts.project_off =
            self.kb_project_off(target.stamp.project.as_deref(), target.stamp.worktree.as_deref());
        let again = match self.kb_read_session(target, job.expect.as_deref()).await {
            Ok(again) => again,
            Err(e) => return (uri, Err(e)),
        };
        match again {
            Again::No => {}
            // (More of a long file: at once, and it is no retry.)
            Again::More => self.kb_enqueue(Job::Session(job)),
            Again::Later => {
                if let Some(wait) = RETRY.get(job.attempt) {
                    let me = Arc::clone(self);
                    // (Counted as pending while it waits, so that idle means done.)
                    self.kb.pending.send_modify(|n| *n += 1);
                    tokio::spawn(async move {
                        tokio::time::sleep(*wait).await;
                        me.kb_enqueue(Job::Session(SessionJob { attempt: job.attempt + 1, ..job }));
                        me.kb.done();
                    });
                }
            }
        }
        (uri, Ok(()))
    }

    /// The gate, the read from the item's cursor, the card, the write.
    async fn kb_read_session(
        self: &Arc<Self>,
        t: Target,
        expect: Option<&str>,
    ) -> Result<Again, Refused> {
        let db = self.kb_db()?;
        let passed = self.kb_gate(&db, &t.uri, &t.facts).await?;
        let item = db.item_by_uri(t.uri.clone()).await.map_err(store_err)?;
        // The agent's folder under the session's profile, as its launch found it.
        let home = match &t.stamp.profile {
            Some(profile) => {
                let plan = self.profile_plan(profile).await?;
                ptyro_agents::dir_var(&t.adapter)
                    .and_then(|var| plan.set.iter().find(|(k, _)| k == var))
                    .map(|(_, dir)| PathBuf::from(dir))
            }
            None => None,
        }
        .or_else(|| ptyro_agents::default_dir(&t.adapter));
        let piece = {
            let cursor = item.as_ref().and_then(|i| i.cursor.clone());
            let (adapter, conversation) = (t.adapter.clone(), t.conversation.clone());
            let (cwd, hook) = (t.cwd.clone(), t.hook.clone());
            tokio::task::spawn_blocking(move || {
                let adapter = ptyro_agents::adapter(&adapter);
                let home = home?;
                let at = TranscriptAt {
                    home: &home,
                    cwd: &cwd,
                    conversation: &conversation,
                    hook: hook.as_deref(),
                };
                let mut piece =
                    read(adapter.as_ref(), &adapter.transcript(&at)?, cursor.as_ref(), READ_MAX)?;
                piece.title = adapter.transcript_title(&at);
                Some(piece)
            })
            .await
            .map_err(|e| Error::internal(e.to_string()))?
        };
        let reads = ptyro_agents::adapter(&t.adapter).read_transcript(&[], 0).is_some();
        // The last thing the agent said, as read now or (nothing new) by the read before: two
        // signals often end one turn, and the second finds the reply already taken.
        let heard = |text: &str| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            text.trim().hash(&mut h);
            h.finish()
        };
        let found = {
            let mut replies = self.kb.replies.locked();
            match piece.as_ref().and_then(|p| p.transcript.last_reply()) {
                Some(reply) => {
                    replies.insert(t.uri.clone(), heard(reply));
                    Some(heard(reply))
                }
                None => replies.get(&t.uri).copied(),
            }
        };
        let again = match (&piece, expect) {
            (Some(p), _) if p.more => Again::More,
            (_, Some(reply)) if reads && found != Some(heard(reply)) => Again::Later,
            _ => Again::No,
        };
        self.kb_write_session(&db, passed, t, item, piece).await?;
        Ok(again)
    }

    /// One piece of a session (or none: the card alone) as a source, through the way in.
    async fn kb_write_session(
        self: &Arc<Self>,
        db: &KbDb,
        passed: ptyro_core::kb::ingest::Passed,
        t: Target,
        item: Option<KbItem>,
        piece: Option<Piece>,
    ) -> Result<(), Refused> {
        let Target { uri, mut stamp, facts, mut card, title, .. } = t;
        let mut source = Source::new(ItemKind::Session, uri, Body::Parts(Vec::new()));
        source.append = true;
        let mut parts = Vec::new();
        if let Some(Piece { transcript, cursor, whole, title: kept, .. }) = piece {
            if transcript.parts.is_empty() {
                // Nothing was said since: only where reading stopped moves.
                if item.as_ref().is_some_and(|i| i.cursor.as_ref() != Some(&cursor)) {
                    db.set_cursor(source.uri, Some(cursor)).await.map_err(store_err)?;
                }
                return Ok(());
            }
            // What the item holds already, unless this piece is all of the file again.
            let had = match &item {
                Some(i) if !whole => db.session_facts(i.id.clone()).await.map_err(store_err)?,
                _ => Default::default(),
            };
            let said = |kind| transcript.parts.iter().find(move |p| p.kind == kind);
            let users: HashSet<&str> = transcript
                .parts
                .iter()
                .filter(|p| p.kind == ChunkKind::User)
                .filter_map(|p| p.anchor.as_deref())
                .collect();
            card.task = had.task.or(said(ChunkKind::User).map(|p| p.text.clone())).or(card.task);
            // (The session's own last reply is this turn's, as its hook said it: newer than
            // what is stored when the file doesn't hold it yet.)
            card.outcome =
                transcript.last_reply().map(str::to_owned).or(card.outcome).or(had.outcome);
            card.turns = had.turns + users.len() as u32;
            card.files = item.iter().filter(|_| !whole).flat_map(|i| i.files.clone()).collect();
            for f in &transcript.files {
                if !card.files.contains(f) {
                    card.files.push(f.clone());
                }
            }
            card.files.truncate(card::FILES);
            let times = || transcript.parts.iter().map(|p| p.at_ms).filter(|t| *t > 0);
            let was = item.as_ref();
            source.started_ms = times().chain(was.and_then(|i| i.started_ms)).min();
            source.ended_ms = times().chain(was.and_then(|i| i.ended_ms)).max();
            (card.started_ms, card.ended_ms) = (source.started_ms, source.ended_ms);
            if let Some(model) = &transcript.model {
                stamp.model = Some(model.clone());
            }
            card.model = stamp.model.clone().or(was.and_then(|i| i.model.clone()));
            // The agent's own title, else (once) the session's, else its first prompt.
            source.title = transcript.title.or(kept).or_else(|| {
                was.is_none_or(|i| i.title.is_none())
                    .then(|| {
                        title.or(card.task.as_deref().map(|t| ptyro_core::session::brief(t, 120)))
                    })
                    .flatten()
            });
            // Too old to keep (`kb.retain_days`): the daily pass deleted it, or would at once.
            // A session that is carried on is read again from its start, and kept.
            if let (Some(cut), Some(ended)) = (self.kb_retain_cut(), source.ended_ms)
                && ended < cut
            {
                return Ok(());
            }
            source.cursor = Some(cursor);
            parts = transcript.parts;
        } else {
            // A transcript was read into this item and can't be found now (the agent removes
            // old ones): what is kept of it stays as it is.
            if item.as_ref().is_some_and(|i| i.cursor.is_some()) {
                return Ok(());
            }
            // No transcript: the card alone, from what the daemon knows of the session. It
            // forgets the prompt and the reply at a restart: the card it wrote says them.
            if let Some(i) = &item {
                let was = db.chunks(i.id.clone(), 0, 1).await.map_err(store_err)?;
                let (task, outcome) = was
                    .first()
                    .filter(|c| c.kind == ChunkKind::Card)
                    .map(|c| card::said(&c.text))
                    .unwrap_or_default();
                card.task = card.task.or(task);
                card.outcome = card.outcome.or(outcome);
            }
            // Nothing to say, or nothing new: no turn has ended since the daemon started.
            if card.turns == 0 && (item.is_some() || card.is_empty()) {
                return Ok(());
            }
            card.ended_ms = card.ended_ms.or(Some(now_ms()));
            (source.started_ms, source.ended_ms) = (card.started_ms, card.ended_ms);
            source.title =
                title.or(card.task.as_deref().map(|t| ptyro_core::session::brief(t, 120)));
        }
        source.files = card.files.clone();
        parts.insert(0, card.part());
        source.body = Body::Parts(parts);
        self.kb_write(db, passed, Prepared { source, stamp, facts }).await?;
        Ok(())
    }

    /// The actor's part of `Job::Item`: an item whose session is gone, read on from its
    /// cursor with what the item itself says of its session.
    pub(super) async fn kb_item(self: &Arc<Self>, uri: &str) -> Result<(), Refused> {
        let db = self.kb_db()?;
        let Some(item) = db.item_by_uri(uri.to_owned()).await.map_err(store_err)? else {
            return Ok(());
        };
        let facts = Facts {
            session: true,
            agent_off: self.kb_agent_off(item.agent.as_deref()),
            project_off: self.kb_project_off(item.project.as_deref(), item.worktree.as_deref()),
            ..Facts::default()
        };
        let passed = self.kb_gate(&db, uri, &facts).await?;
        let adapter = uri.strip_prefix("session://").and_then(|r| r.split('/').next());
        let (Some(adapter), Some(cursor)) = (adapter.map(str::to_owned), item.cursor.clone())
        else {
            return Ok(());
        };
        let piece = tokio::task::spawn_blocking(move || {
            let path = PathBuf::from(cursor["path"].as_str()?);
            read(ptyro_agents::adapter(&adapter).as_ref(), &path, Some(&cursor), READ_MAX)
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))?;
        let Some(piece) = piece else { return Ok(()) };
        let worktree = item.worktree.as_deref().and_then(|id| self.worktree_by_id(id));
        let project = item.project.as_deref().and_then(|id| self.project_by_id(id));
        let target = Target {
            uri: uri.to_owned(),
            adapter: String::new(),
            conversation: String::new(),
            cwd: PathBuf::new(),
            hook: None,
            // (What a piece doesn't say of the item stays as it is.)
            stamp: Stamp {
                session: item.session.clone(),
                ..Stamp::new(item.writer, item.profile.clone())
            },
            facts,
            card: Card {
                agent: item.agent.clone(),
                project: project.map(|p| p.slug),
                worktree: worktree.map(|w| w.name),
                branch: item.branch.clone(),
                ..Card::default()
            },
            title: None,
        };
        self.kb_write_session(&db, passed, target, Some(item), Some(piece)).await
    }
}

/// Whether a read is to be made again.
enum Again {
    No,
    /// At once: the file held more than one read takes.
    More,
    /// After a wait: the turn's reply was not in the file yet.
    Later,
}

#[cfg(test)]
mod tests;
