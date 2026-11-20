//! The knowledge base in the daemon (SPEC §25.5): `kb.db`, and the ingest actor that fills it.
//!
//! Whatever wants something kept puts a [`Job`] on the queue ([`Daemon::kb_enqueue`]) and goes
//! on: a hook, a session's end, a sync. **Nothing of this is on a session's path.** One task
//! takes the jobs in order, a few at a time with a pause between, and for each:
//!
//! 1. asks the gate ([`Daemon::kb_gate`]: the one place that says whether a source may be
//!    kept), before the source is read;
//! 2. redacts, cuts and stamps it into a plan (`ptyro_core::kb::ingest::plan`), with the
//!    secrets of the source's profile and the user's own patterns;
//! 3. writes the plan (`KbDb::write`), one transaction an item.
//!
//! The queue is memory only. After a restart or a live upgrade it is rebuilt from what the
//! readers find past each item's cursor (`KbDb::cursors`, [`Daemon::kb_catch_up`]), so nothing
//! crosses the exec; a job that was queued and not yet written is found again that way.
//!
//! Agent sessions, the first source, are in [`sessions`]; notes, which are files, in [`notes`];
//! projects and folders in [`roots`], which has a task of its own (a folder of fifty thousand
//! files must not stand in a session's way); what the agents kept before ptyro did in
//! [`history`], which waits behind everything else on the queue; a shell's commands in
//! [`shells`].

#[cfg(test)]
mod golden;
mod history;
mod machines;
mod notes;
mod offer;
mod requests;
mod roots;
mod search;
mod sessions;
mod shells;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ptyro_core::kb::ingest::{self, Gate, Never, Passed, Skip, Source, Stamp};
use ptyro_core::kb::redact::Redactor;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode};
use ptyro_store::kb::{KbDb, Outcome, Written};
use tokio::sync::{Notify, watch};

use crate::daemon::Daemon;

/// Jobs taken before the actor pauses.
const BATCH: usize = 8;
/// The pause: the writer and the machine are for the sessions first.
const PAUSE: Duration = Duration::from_millis(50);
/// How many skipped sources are remembered for `ptyro kb status`.
const SKIPPED: usize = 100;

/// The knowledge base's part of the daemon.
pub struct Kb {
    path: PathBuf,
    /// Opened at start when `[kb] enabled`, else the first time it is wanted.
    db: OnceLock<Result<KbDb, String>>,
    queue: Mutex<VecDeque<Job>>,
    /// Jobs that wait until the queue is empty: the history's backfill, which is thousands
    /// of old conversations and must not stand before a session's turn.
    backlog: Mutex<VecDeque<Job>>,
    wake: Notify,
    /// Jobs queued or being worked on: 0 is idle.
    pending: watch::Sender<usize>,
    /// What was not kept, and why, newest last.
    skipped: Mutex<VecDeque<Skipped>>,
    /// `kb.redact` and `kb.exclude`, compiled for the lists they were made from.
    rules: Mutex<Option<Arc<Rules>>>,
    /// The last assistant message read of each session (its URI), hashed: how a read knows
    /// that the reply a `Stop` hook named was taken already (`sessions`).
    replies: Mutex<HashMap<String, u64>>,
    /// The notes folder's watcher (`notes`), kept for as long as the daemon runs.
    notes_watch: Mutex<Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>>,
    /// One reader or writer of the notes folder at a time.
    notes_lock: tokio::sync::Mutex<()>,
    /// Note files that were refused, each as it was then (its time and size): not read
    /// again until it changes.
    notes_refused: Mutex<HashMap<String, serde_json::Value>>,
    /// Notes each session's agent wrote today (the day, how many): `remember`'s limit.
    remembered: Mutex<HashMap<String, (u64, u32)>>,
    /// What was offered at prompts since the daemon started (`offer`).
    offers: Mutex<offer::Offers>,
    /// Projects and folders (`roots`).
    roots: roots::Roots,
    /// Conversations of sessions that nothing is kept of, which got their tombstone since
    /// the daemon started (`history`).
    kept_out: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Every profile's secret values together, for sources that are no profile's, and when
    /// they were read: asked for with every such source, so kept for [`EVERYONES`].
    everyones: Mutex<Option<(std::time::Instant, Secrets)>>,
}

/// Secret values, each with what is shown in its place.
type Secrets = Vec<(String, String)>;

/// How long every profile's secrets are kept in memory for redacting nobody's sources (a
/// secret that is a command is run again after it).
const EVERYONES: Duration = Duration::from_secs(60);

pub(crate) struct Rules {
    redact: Vec<String>,
    exclude: Vec<String>,
    redactor: Redactor,
    pub(crate) never: Never,
}

/// One thing to ingest. A job never holds text that the gate has not been asked about,
/// except one that was read for another reason already.
pub(crate) enum Job {
    /// A source that is already in memory, with what the daemon knows about it.
    #[allow(dead_code)] // (This module's tests; a note is ingested where it is written.)
    Prepared(Box<Prepared>),
    /// An agent session whose transcript may have grown (a turn's end, its exit).
    Session(sessions::SessionJob),
    /// A session's item whose session ptyro no longer has, by its URI (the daemon's start).
    Item(String),
    /// A conversation an agent kept that ptyro never saw (`ingest --history`).
    History(history::HistoryJob),
    /// A command that ended in a shell (`shells`).
    Shell(Box<shells::ShellJob>),
}

pub(crate) struct Prepared {
    pub source: Source,
    /// Who and where, as the daemon knows it (never from the source).
    pub stamp: Stamp,
    pub facts: Facts,
}

/// What the gate needs to know of a source beyond the settings: filled by whoever queues it,
/// from the session or the root it comes from.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    /// It is an agent's session (`kb.sessions` switches those off).
    pub session: bool,
    /// It is a shell's command (`kb.shell = "off"` switches those off).
    pub shell: bool,
    /// The source's agent has `kb = false`.
    pub agent_off: bool,
    /// The source's project has `[knowledge] enabled = false`.
    pub project_off: bool,
    /// Its session is not recorded (§7.6).
    pub unrecorded: bool,
    /// Its session is private (§25.7a).
    pub private: bool,
    /// The file it is read from, for the never list.
    pub path: Option<PathBuf>,
    /// That file's size, when `kb.max_file` applies to it.
    pub bytes: Option<u64>,
}

/// A source that was not kept (`ptyro kb status` names them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub uri: String,
    pub why: String,
    pub at_ms: u64,
}

impl Kb {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            db: OnceLock::new(),
            queue: Mutex::new(VecDeque::new()),
            backlog: Mutex::new(VecDeque::new()),
            wake: Notify::new(),
            pending: watch::Sender::new(0),
            skipped: Mutex::new(VecDeque::new()),
            rules: Mutex::new(None),
            replies: Mutex::new(HashMap::new()),
            notes_watch: Mutex::new(None),
            notes_lock: tokio::sync::Mutex::new(()),
            notes_refused: Mutex::new(HashMap::new()),
            remembered: Mutex::new(HashMap::new()),
            offers: Mutex::new(offer::Offers::default()),
            roots: roots::Roots::default(),
            kept_out: Arc::new(Mutex::new(std::collections::HashSet::new())),
            everyones: Mutex::new(None),
        }
    }

    /// `kb.db`, opened now if it wasn't; or why it can't be, in a line with the fix, as
    /// chats say it (SPEC §25.4). `origin`: `chat.db`'s machine id.
    fn open(&self, origin: Option<&str>) -> Result<&KbDb, Error> {
        self.db
            .get_or_init(|| {
                KbDb::open(&self.path, origin).map_err(|e| {
                    tracing::error!("kb.db: {e}");
                    let file = self.path.display().to_string();
                    match e.fix(&file) {
                        Some(fix) => format!("the knowledge base is unavailable: {fix}"),
                        None => format!(
                            "the knowledge base is unavailable: {file} can't be opened ({e}). `ptyro doctor` says more"
                        ),
                    }
                })
            })
            .as_ref()
            .map_err(|e| Error::new(ErrorCode::Internal, e.clone()))
    }

    fn skip(&self, uri: &str, why: String) {
        tracing::info!("knowledge base: {uri} not kept: {why}");
        let mut s = self.skipped.locked();
        s.retain(|k| k.uri != uri);
        if s.len() >= SKIPPED {
            s.pop_front();
        }
        s.push_back(Skipped { uri: uri.to_owned(), why, at_ms: ptyro_core::now_ms() });
    }

    fn done(&self) {
        self.pending.send_modify(|n| *n = n.saturating_sub(1));
    }
}

impl Daemon {
    /// `kb.db`, for everything that reads or writes the knowledge base. Off, or not openable:
    /// an error that says so and offers the fix.
    pub(crate) fn kb_db(&self) -> Result<KbDb, Error> {
        if !self.config().kb.enabled {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "the knowledge base is off: switch it on in the settings (Knowledge → enabled), then try again",
            ));
        }
        let origin = self.chats.db().ok().map(|d| d.machine_id.clone());
        self.kb.open(origin.as_deref()).cloned()
    }

    /// At the daemon's start: open `kb.db` when the knowledge base is on (a file that can't be
    /// opened is a warning, and the daemon runs), and start the ingest actor.
    pub(crate) fn start_kb(self: &Arc<Self>) {
        if self.config().kb.enabled
            && let Err(e) = self.kb_db()
        {
            self.warn(e.message);
        }
        let me = Arc::clone(self);
        tokio::spawn(async move { me.kb_actor().await });
        self.kb_output_switch();
        self.start_kb_retention();
        self.start_kb_notes();
        self.start_kb_roots();
    }

    /// A session's workspace as an item is stamped with it: its id. (A session knows its
    /// workspace by name, which a rename changes; a chat and the search's filters go by id.)
    pub(crate) fn kb_workspace_id(&self, name: Option<&str>) -> Option<String> {
        let name = name?;
        let reg = self.registry.locked();
        reg.workspaces.iter().find(|w| w.machine.is_none() && w.name == name).map(|w| w.id.clone())
    }

    /// Whether the agent called `name` (as `[agents.<name>]` has it) is kept out of the
    /// knowledge base: `kb = false` (SPEC §25.13).
    pub(crate) fn kb_agent_off(&self, name: Option<&str>) -> bool {
        // (`claude@work` is the agent `claude` under a profile.)
        name.and_then(|n| n.split('@').next())
            .is_some_and(|n| self.config().agents.get(n).and_then(|a| a.kb) == Some(false))
    }

    /// Whether a project keeps itself out of the knowledge base: `[knowledge] enabled = false`
    /// in its `.ptyro.toml` (or your `.ptyro.local.toml` there), read from the worktree the
    /// source is of, else the main checkout. A file that can't be read keeps nothing out.
    pub(crate) fn kb_project_off(&self, project: Option<&str>, worktree: Option<&str>) -> bool {
        let Some(p) = project.and_then(|id| self.project_by_id(id)) else { return false };
        let checkout =
            worktree.and_then(|id| self.worktree_by_id(id)).map_or(p.root.clone(), |w| w.path);
        ptyro_core::project::ProjectConfig::load(&p.root, &checkout)
            .is_ok_and(|c| c.knowledge.enabled == Some(false))
    }

    /// Queue something to ingest, and return at once. A session that is already waiting is
    /// not queued twice (a turn's end is often said by several signals).
    pub(crate) fn kb_enqueue(&self, job: Job) {
        {
            let mut q = self.kb.queue.locked();
            if let Job::Session(new) = &job
                && let Some(waiting) = q.iter_mut().find_map(|j| match j {
                    Job::Session(w) if w.same(new) => Some(w),
                    _ => None,
                })
            {
                waiting.expect = new.expect.clone().or(waiting.expect.take());
                // (More of a long file: the read that waits carries on where that one stopped.)
                waiting.from = new.from.clone().or(waiting.from.take());
                return;
            }
            self.kb.pending.send_modify(|n| *n += 1);
            q.push_back(job);
        }
        self.kb.wake.notify_one();
    }

    /// Queue something that waits for everything else: it is taken only while the queue
    /// itself is empty.
    pub(crate) fn kb_enqueue_later(&self, job: Job) {
        self.kb.pending.send_modify(|n| *n += 1);
        self.kb.backlog.locked().push_back(job);
        self.kb.wake.notify_one();
    }

    /// Wait until the queue is empty and nothing is being written or waiting to be tried
    /// again (a test, `kb sync`).
    #[allow(dead_code)] // (KB3's `kb sync`; this module's tests.)
    pub(crate) async fn kb_idle(&self) {
        let mut rx = self.kb.pending.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }

    /// Sources that were not kept, newest last, with why.
    pub(crate) fn kb_skipped(&self) -> Vec<Skipped> {
        self.kb.skipped.locked().iter().cloned().collect()
    }

    async fn kb_actor(self: Arc<Self>) {
        loop {
            let mut batch: Vec<Job> = {
                let mut q = self.kb.queue.locked();
                let n = q.len().min(BATCH);
                q.drain(..n).collect()
            };
            if batch.is_empty() {
                let mut later = self.kb.backlog.locked();
                let n = later.len().min(BATCH);
                batch = later.drain(..n).collect();
            }
            if batch.is_empty() {
                self.kb.wake.notified().await;
                continue;
            }
            for job in batch {
                let (uri, done) = match job {
                    Job::Prepared(p) => {
                        (p.source.uri.clone(), self.kb_ingest(*p).await.map(|_| ()))
                    }
                    Job::Session(j) => self.kb_session(j).await,
                    Job::Item(uri) => {
                        let done = self.kb_item(&uri).await;
                        (uri, done)
                    }
                    Job::History(j) => {
                        let uri = j.uri.clone();
                        (uri, self.kb_history_read(j).await)
                    }
                    Job::Shell(j) => {
                        let uri = j.uri.clone();
                        (uri, self.kb_shell(*j).await)
                    }
                };
                match done {
                    Ok(()) => {}
                    Err(Refused::Skip(why)) => self.kb.skip(&uri, why.to_string()),
                    // Off is not a source's trouble, and a file that can't be opened was
                    // said once at start.
                    Err(Refused::Error(e)) if e.code == ErrorCode::Unsupported => {}
                    Err(Refused::Error(e)) => {
                        tracing::warn!("knowledge base: {uri}: {}", e.message);
                    }
                }
                self.kb.done();
            }
            tokio::time::sleep(PAUSE).await;
        }
    }

    /// `kb.redact` and `kb.exclude` as they are now, compiled once for each change of them.
    pub(crate) fn kb_rules(&self) -> Arc<Rules> {
        let config = self.config();
        let mut cached = self.kb.rules.locked();
        if let Some(r) = cached.as_ref()
            && r.redact == config.kb.redact
            && r.exclude == config.kb.exclude
        {
            return Arc::clone(r);
        }
        // (A pattern that isn't a regex never loads: `Config` refuses the file.)
        let (redactor, _) = Redactor::new(Vec::new(), &config.kb.redact);
        let rules = Arc::new(Rules {
            never: Never::new(&config.kb.exclude),
            redactor,
            redact: config.kb.redact,
            exclude: config.kb.exclude,
        });
        *cached = Some(Arc::clone(&rules));
        rules
    }

    /// **The gate** (§25.5 step 1), the only one: every source is asked about here before it
    /// is read. The knowledge base on; the source's agent and project not kept out; its
    /// session recorded and not private; its URI not forgotten; its path not on the never
    /// list; its file not too big.
    pub(crate) async fn kb_gate(
        &self,
        db: &KbDb,
        uri: &str,
        facts: &Facts,
    ) -> Result<Passed, Refused> {
        let rules = self.kb_rules();
        let forgotten = db.tombstoned(uri.to_owned()).await.map_err(store_err)?;
        // Closed: a conversation that was kept, then carried on in a private session
        // (§25.7a). Its file holds the private turns, so nothing reads it again.
        let closed = db.closed(uri.to_owned()).await.map_err(store_err)?;
        let config = self.config();
        let gate = Gate {
            enabled: config.kb.enabled,
            sessions_off: facts.session && !config.kb.sessions,
            shells_off: facts.shell && !config.kb.shell_commands(),
            agent_off: facts.agent_off,
            project_off: facts.project_off,
            unrecorded: facts.unrecorded,
            private: facts.private || closed,
            forgotten,
            path: facts.path.as_deref(),
            never: &rules.never,
            bytes: facts.bytes,
            max_bytes: config.kb.max_file_bytes(),
        };
        ingest::gate(&gate).map_err(Refused::Skip)
    }

    /// One source through the way in: the gate, the plan (redacted with its profile's secrets
    /// and the user's patterns), the write.
    pub(crate) async fn kb_ingest(self: &Arc<Self>, p: Prepared) -> Result<Written, Refused> {
        let db = self.kb_db()?;
        let passed = self.kb_gate(&db, &p.source.uri, &p.facts).await?;
        self.kb_write(&db, passed, p).await
    }

    /// A source the gate let through (it was asked before the source was read): the plan and
    /// the write.
    async fn kb_write(
        self: &Arc<Self>,
        db: &KbDb,
        passed: Passed,
        p: Prepared,
    ) -> Result<Written, Refused> {
        let secrets = self.kb_secrets(p.stamp.profile.as_deref()).await?;
        let kind = p.source.kind;
        let w = self.kb_store(db, passed, p, secrets).await?;
        // Something new can be found, or something of it went: who shows it asks again.
        if w.added + w.changed + w.removed > 0 {
            self.kb_changed([kind]);
        }
        Ok(w)
    }

    /// A profile's secret values, for the redactor: without them a source of that profile is
    /// not kept, since redaction can't be skipped. (It is found again from its cursor when
    /// they can be read.) A source that is **no profile's** (an unpinned project, a folder
    /// read for everyone, your notes) is shown to every profile's agents, so it is redacted
    /// with every profile's secrets, and kept out by the same rule while any can't be read.
    pub(crate) async fn kb_secrets(
        self: &Arc<Self>,
        profile: Option<&str>,
    ) -> Result<Vec<(String, String)>, Error> {
        let of = async |profile: &str| {
            Ok::<_, Error>(crate::redact::secrets(&self.profile_plan(profile).await.map_err(|e| {
                Error::new(
                    ErrorCode::Invalid,
                    format!("not kept, because profile {profile}'s secrets can't be read to redact them: {}", e.message),
                )
            })?))
        };
        let Some(profile) = profile else {
            if let Some((at, all)) = self.kb.everyones.locked().as_ref()
                && at.elapsed() < EVERYONES
            {
                return Ok(all.clone());
            }
            let mut all = Vec::new();
            for profile in ptyro_agents::profile::names(&self.config()) {
                all.extend(of(&profile).await?);
            }
            all.sort();
            all.dedup();
            *self.kb.everyones.locked() = Some((std::time::Instant::now(), all.clone()));
            return Ok(all);
        };
        of(profile).await
    }

    /// The configuration was read again: every profile's secrets are asked for anew.
    pub(crate) fn kb_secrets_changed(&self) {
        *self.kb.everyones.locked() = None;
    }

    /// The plan and the write of a source the gate let through, redacted with `secrets` (its
    /// profile's) and the user's patterns. It tells nobody: a pass over a root writes
    /// thousands of items and says so now and then.
    pub(crate) async fn kb_store(
        self: &Arc<Self>,
        db: &KbDb,
        passed: Passed,
        p: Prepared,
        secrets: Vec<(String, String)>,
    ) -> Result<Written, Refused> {
        let rules = self.kb_rules();
        let Prepared { source, stamp, .. } = p;
        let uri = source.uri.clone();
        let plan = tokio::task::spawn_blocking(move || {
            ingest::plan(passed, source, stamp, &rules.redactor.with_secrets(secrets))
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(Refused::Skip)?;
        if plan.dropped > 0 {
            self.kb.skip(
                &uri,
                format!(
                    "{} message(s) of it held too many things that look like credentials",
                    plan.dropped
                ),
            );
        }
        let w = db.write(plan).await.map_err(store_err)?;
        if w.outcome == Outcome::Forgotten {
            // Forgotten between the gate and the write.
            return Err(Refused::Skip(Skip::Forgotten));
        }
        // The embed step: what was written is searchable by meaning at once, when the
        // model is here (§25.8).
        if w.added + w.changed > 0 {
            self.kb_embed(db, &w.item).await;
        }
        Ok(w)
    }

    /// Tell every watcher that the knowledge base changed (`KbUpdated`, SPEC §25.14): items
    /// of these kinds were written, grew, or went.
    pub(crate) fn kb_changed(&self, kinds: impl IntoIterator<Item = ptyro_core::kb::ItemKind>) {
        let mut kinds: Vec<_> = kinds.into_iter().collect();
        kinds.sort_by_key(|k| k.as_str());
        kinds.dedup();
        if !kinds.is_empty() {
            let _ = self.events.send(ptyro_protocol::DomainEvent::KbUpdated { kinds });
        }
    }

    /// Before a live upgrade's exec: what is being written is written (the queue itself is
    /// rebuilt by the next image).
    pub(crate) async fn kb_flush(&self) {
        if let Some(Ok(db)) = self.kb.db.get() {
            let _ = db.flush().await;
        }
    }

    /// `ptyro doctor`'s line about the knowledge base.
    pub(crate) async fn kb_doctor(&self) -> Option<String> {
        if !self.config().kb.enabled {
            return Some("knowledge base: off ([kb] enabled = false)".into());
        }
        let db = match self.kb_db() {
            Ok(db) => db,
            Err(e) => return Some(e.message),
        };
        let file = db.path.display();
        match db.doctor().await {
            Ok(d) if !d.fts5 => {
                Some("knowledge base: this SQLite has no FTS5, so its search won't work".into())
            }
            Ok(d) if d.integrity != "ok" => {
                Some(format!("knowledge base: {file} failed its integrity check: {}", d.integrity))
            }
            Ok(d) if !d.secure_delete => Some(format!(
                "knowledge base: {file} doesn't zero what is deleted (secure delete is off), so a forgotten item's text could be left in it"
            )),
            Ok(d) => {
                let kinds: Vec<String> = d
                    .status
                    .items
                    .iter()
                    .map(|(k, n, _)| format!("{n} {k}{}", if *n == 1 { "" } else { "s" }))
                    .collect();
                let chunks: u64 = d.status.items.iter().map(|(_, _, c)| c).sum();
                let roots = match db.roots().await {
                    Ok(roots) => roots::doctor(&roots),
                    Err(_) => String::new(),
                };
                Some(format!(
                    "knowledge base: {} in {chunks} piece(s), {}, {} in {file} (schema {}){roots}",
                    if kinds.is_empty() { "nothing yet".into() } else { kinds.join(", ") },
                    if d.status.forgotten == 0 {
                        "nothing forgotten".into()
                    } else {
                        format!("{} forgotten", d.status.forgotten)
                    },
                    crate::chats::human_bytes(d.status.bytes),
                    d.version
                ))
            }
            Err(e) => Some(format!("knowledge base: {e}")),
        }
    }
}

/// Why a source was not ingested: the gate or the rules said no, or something failed.
#[derive(Debug)]
pub(crate) enum Refused {
    Skip(Skip),
    Error(Error),
}

impl From<Error> for Refused {
    fn from(e: Error) -> Self {
        Refused::Error(e)
    }
}

pub(crate) fn store_err(e: ptyro_store::StoreError) -> Error {
    match e.fix("kb.db") {
        Some(fix) => Error::new(ErrorCode::Internal, fix),
        None => Error::new(ErrorCode::Internal, format!("kb.db: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use ptyro_core::kb::chunk::Part;
    use ptyro_core::kb::ingest::Body;
    use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
    use ptyro_store::Registry;
    use ptyro_store::kb::ItemFilter;

    use super::*;

    fn doc(uri: &str, text: &str) -> Prepared {
        Prepared {
            source: Source::new(
                ItemKind::File,
                uri,
                Body::Markdown { name: "a.md".into(), text: text.into(), at_ms: 1 },
            ),
            stamp: Stamp::new(Writer::File, None),
            facts: Facts::default(),
        }
    }

    fn set_config(d: &Daemon, toml: &str) {
        *d.config.write().unwrap() = ptyro_core::config::Config::parse(toml).unwrap();
    }

    #[tokio::test]
    async fn queued_sources_are_written_redacted_and_the_refused_ones_named() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        set_config(&d, "[kb]\nredact = ['ACME-\\d+']\nexclude = ['*.secret.md']\n");
        d.start_kb();
        assert!(d.paths.data.join("kb.db").exists(), "opened at start");
        // More than a batch, so the actor goes round.
        for n in 0..(BATCH * 2 + 3) {
            d.kb_enqueue(Job::Prepared(Box::new(doc(
                &format!("file:///repo/doc{n}.md"),
                &format!("# Doc {n}\n\nticket ACME-{n} and export API_TOKEN=abc123def456{n}\n"),
            ))));
        }
        let mut never = doc("file:///repo/.env.md", "# no");
        never.facts.path = Some("/repo/.env".into());
        d.kb_enqueue(Job::Prepared(Box::new(never)));
        let mut mine = doc("file:///repo/x.secret.md", "# no");
        mine.facts.path = Some("/repo/x.secret.md".into());
        d.kb_enqueue(Job::Prepared(Box::new(mine)));
        let mut big = doc("file:///repo/big.md", "# no");
        big.facts.bytes = Some(ingest::MAX_FILE + 1);
        d.kb_enqueue(Job::Prepared(Box::new(big)));
        let keys: String = (0..30).map(|i| format!("API_TOKEN=abc{i}def456\n")).collect();
        d.kb_enqueue(Job::Prepared(Box::new(doc("file:///repo/keys.md", &keys))));
        for (uri, facts) in [
            ("session://claude/private", Facts { private: true, ..Facts::default() }),
            ("session://claude/login", Facts { unrecorded: true, ..Facts::default() }),
            ("session://claude/agent-off", Facts { agent_off: true, ..Facts::default() }),
            ("session://claude/project-off", Facts { project_off: true, ..Facts::default() }),
        ] {
            let mut p = doc(uri, "# no");
            p.facts = facts;
            d.kb_enqueue(Job::Prepared(Box::new(p)));
        }
        d.kb_idle().await;

        let db = d.kb_db().unwrap();
        let items = db.list(ItemFilter::default()).await.unwrap();
        assert_eq!(items.len(), BATCH * 2 + 3);
        assert!(
            items.iter().all(|i| i.uri.starts_with("file:///repo/doc") && i.writer == Writer::File)
        );
        let one = db.item_by_uri("file:///repo/doc7.md".into()).await.unwrap().unwrap();
        let chunks = db.chunks(one.id, 0, 0).await.unwrap();
        assert_eq!(chunks[0].text, "ticket <redacted> and export API_TOKEN=<redacted>");
        let skipped: Vec<(String, String)> =
            d.kb_skipped().into_iter().map(|s| (s.uri, s.why)).collect();
        let why = |uri: &str| {
            skipped
                .iter()
                .find(|(u, _)| u == uri)
                .map(|(_, w)| w.as_str())
                .unwrap_or_else(|| panic!("{uri} in {skipped:?}"))
        };
        assert!(why("file:///repo/.env.md").contains("never read"));
        assert!(why("file:///repo/x.secret.md").contains("never read"));
        assert!(why("file:///repo/big.md").contains("too big"));
        assert!(why("file:///repo/keys.md").contains("30 things that look like credentials"));
        assert!(why("session://claude/private").contains("private"));
        assert!(why("session://claude/login").contains("not recorded"));
        assert!(why("session://claude/agent-off").contains("agent"));
        assert!(why("session://claude/project-off").contains("project"));
        assert_eq!(skipped.len(), 8);

        let line = d.kb_doctor().await.unwrap();
        assert!(line.starts_with(&format!("knowledge base: {} files in", BATCH * 2 + 3)), "{line}");
        assert!(line.contains("nothing forgotten") && line.contains("kb.db (schema 1)"), "{line}");
    }

    #[tokio::test]
    async fn a_forgotten_uri_and_a_switched_off_knowledge_base_take_nothing() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        d.start_kb();
        let turns = |text: &str| Prepared {
            source: Source {
                append: true,
                ..Source::new(
                    ItemKind::Session,
                    "session://claude/c1",
                    Body::Parts(vec![Part::new(ChunkKind::User, text, 1).anchor(text)]),
                )
            },
            stamp: Stamp::new(Writer::Agent, None),
            facts: Facts::default(),
        };
        let w = d.kb_ingest(turns("first")).await.unwrap();
        let db = d.kb_db().unwrap();
        db.forget(w.item.clone()).await.unwrap();
        assert!(matches!(d.kb_ingest(turns("second")).await, Err(Refused::Skip(Skip::Forgotten))));
        assert_eq!(db.item(w.item).await.unwrap().unwrap().chunks, 0);

        // Off: nothing is ingested, what is stored stays, and asking says how to switch it on.
        d.kb_ingest(doc("file:///a.md", "# A\n\nkept")).await.unwrap();
        set_config(&d, "[kb]\nenabled = false\n");
        d.kb_enqueue(Job::Prepared(Box::new(doc("file:///b.md", "# B\n\nnot kept"))));
        d.kb_idle().await;
        let e = d.kb_db().err().unwrap();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert!(e.message.contains("Knowledge → enabled"), "{}", e.message);
        assert!(d.kb_skipped().is_empty());
        assert_eq!(d.kb_doctor().await.unwrap(), "knowledge base: off ([kb] enabled = false)");
        assert_eq!(db.list(ItemFilter::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_opened_is_a_reason_and_the_daemon_runs() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        std::fs::create_dir_all(&d.paths.data).unwrap();
        std::fs::write(d.paths.data.join("kb.db"), "this is not a database, not at all").unwrap();
        d.start_kb();
        let e = d.kb_db().err().unwrap();
        assert!(e.message.starts_with("the knowledge base is unavailable: "), "{}", e.message);
        assert!(e.message.contains("is damaged: move it aside"), "{}", e.message);
        assert_eq!(d.kb_doctor().await.unwrap(), e.message);
        assert!(d.warnings.locked().iter().any(|w| w == &e.message));
        // A job is dropped quietly: the reason was given once.
        d.kb_enqueue(Job::Prepared(Box::new(doc("file:///a.md", "# A"))));
        d.kb_idle().await;
        d.kb_flush().await;
    }

    #[tokio::test]
    async fn a_profile_whose_secrets_cannot_be_read_keeps_its_source_out() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        set_config(
            &d,
            "[profiles.work.secrets]\nDB_PASS = 'hunter2-value'\n\
             [profiles.broken.secrets]\nKEY = { command = ['/nonexistent/ptyro-test-secret'] }\n",
        );
        d.start_kb();
        let mut p = doc("file:///w.md", "# W\n\nthe password is hunter2-value here");
        p.stamp.profile = Some("work".into());
        let w = d.kb_ingest(p).await.unwrap();
        let chunks = d.kb_db().unwrap().chunks(w.item, 0, 0).await.unwrap();
        assert_eq!(chunks[0].text, "the password is <secret:DB_PASS> here");
        let mut p = doc("file:///b.md", "# B\n\ntext");
        p.stamp.profile = Some("broken".into());
        let Err(Refused::Error(e)) = d.kb_ingest(p).await else { panic!("it was kept") };
        assert!(e.message.contains("profile broken's secrets can't be read"), "{}", e.message);
        assert!(d.kb_db().unwrap().item_by_uri("file:///b.md".into()).await.unwrap().is_none());
        // Nobody's source is every profile's to see: kept out too, while one can't be read.
        let p = doc("file:///n.md", "# N\n\nthe password is hunter2-value here");
        let Err(Refused::Error(e)) = d.kb_ingest(p).await else { panic!("it was kept") };
        assert!(e.message.contains("profile broken's secrets can't be read"), "{}", e.message);
    }

    /// C4: a source that is no profile's (an unpinned project, a folder read for everyone,
    /// your own note) is redacted with every profile's secrets.
    #[tokio::test]
    async fn a_source_of_no_profile_is_redacted_with_every_profiles_secrets() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        set_config(
            &d,
            "[profiles.work.secrets]\nDB_PASS = 'hunter2-value'\n\
             [profiles.home.secrets]\nWIFI = 'correct-horse-battery'\n",
        );
        d.start_kb();
        let p = doc("file:///n.md", "# N\n\nwork uses hunter2-value, home correct-horse-battery");
        assert_eq!(p.stamp.profile, None);
        let w = d.kb_ingest(p).await.unwrap();
        let chunks = d.kb_db().unwrap().chunks(w.item, 0, 0).await.unwrap();
        assert_eq!(chunks[0].text, "work uses <secret:DB_PASS>, home <secret:WIFI>");
        // A profile's own source still goes by its own.
        let mut p = doc("file:///w.md", "# W\n\nhunter2-value and correct-horse-battery");
        p.stamp.profile = Some("work".into());
        let w = d.kb_ingest(p).await.unwrap();
        let chunks = d.kb_db().unwrap().chunks(w.item, 0, 0).await.unwrap();
        assert_eq!(chunks[0].text, "<secret:DB_PASS> and correct-horse-battery");
    }

    /// E1: a conversation that was kept and then carried on by a private session is closed
    /// (`KbDb::keep_out`): the gate refuses it, whoever asks.
    #[tokio::test]
    async fn a_closed_conversation_is_refused_by_the_gate() {
        let t = tempfile::tempdir().unwrap();
        let d = crate::daemon::test_daemon(t.path(), Registry::default());
        d.start_kb();
        let turns = |text: &str| Prepared {
            source: Source {
                append: true,
                ..Source::new(
                    ItemKind::Session,
                    "session://claude/c1",
                    Body::Parts(vec![Part::new(ChunkKind::User, text, 1).anchor(text)]),
                )
            },
            stamp: Stamp::new(Writer::Agent, None),
            facts: Facts { session: true, ..Facts::default() },
        };
        let w = d.kb_ingest(turns("before")).await.unwrap();
        let db = d.kb_db().unwrap();
        db.keep_out("session://claude/c1".into(), ItemKind::Session).await.unwrap();
        assert!(matches!(d.kb_ingest(turns("private")).await, Err(Refused::Skip(Skip::Private))));
        let kept = db.chunks(w.item, 0, 0).await.unwrap();
        assert_eq!(kept.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["before"]);
    }
}
