//! Meaning search in the daemon (SPEC §22.10 tier 3, §25.8), opt-in with `[kb] meaning = true`
//! (`[chat] meaning`, its old name, switches it on too): one switch, one model and one task for
//! chats (`chat.db`'s `node_vec`) and the knowledge base (`kb.db`'s `chunk_vec`).
//!
//! A background task does everything, at low priority: when meaning search is switched on it
//! downloads the model (31 MB, once, resumable, checked) into `<data>/models/potion-base-8M/`,
//! then embeds every message and every prose chunk that has no vector yet, newest first, a
//! batch at a time, and after that each message as it is written. (What an ingest writes to
//! the knowledge base is embedded by the ingest itself, [`Daemon::kb_embed`]; the task's pass
//! is for what was there before the model was.) Switched off, it stops: what was downloaded
//! and embedded stays, so switching it on again is instant. A search embeds its words and
//! merges the nearest messages with the keyword hits by reciprocal-rank fusion
//! ([`ptyro_meaning::fuse`]).
//!
//! The model is loaded only while it is used: it is dropped after [`IDLE`] without a search or a
//! message to embed (it is 30 MB of memory), and loading it again takes a few milliseconds.

use ptyro_core::sync::MutexExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ptyro_core::chats::Query;
use ptyro_core::{Error, ErrorCode};
use ptyro_meaning::{FILES, Found, MODEL_ID, Model};
use ptyro_protocol::DomainEvent;
use ptyro_store::chat::{ChatDb, RawHit, SearchFilter};
use ptyro_store::kb::KbDb;
use tokio::sync::Notify;

use crate::daemon::Daemon;

/// An unused model is dropped after this long.
const IDLE: Duration = Duration::from_secs(5 * 60);
/// Messages embedded at a time; between batches the chat database is left to others.
const BATCH: usize = 128;
/// A pass over every message this often; the passes between read only what is new.
const FULL_PASS: Duration = Duration::from_secs(3600);
/// After a failed download, the next try (switching it off and on tries at once).
const RETRY: Duration = Duration::from_secs(10 * 60);
/// Where the model comes from; `PTYRO_MEANING_URL` replaces it (a mirror, or a test's
/// `file://` folder laid out as `<repo>/resolve/<commit>/<file>`).
const SOURCE: &str = "https://huggingface.co";

/// Meaning search's state, kept on [`crate::chats::Chats`].
#[derive(Default)]
pub(crate) struct Meaning {
    state: Mutex<State>,
    model: Mutex<Option<(Arc<Model>, Instant)>>,
    /// A message was written, or the config changed: the task looks again.
    wake: Notify,
}

#[derive(Debug, Clone, Default)]
enum State {
    #[default]
    Off,
    Fetching {
        got: u64,
    },
    Failed(String),
    Embedding {
        done: u64,
        all: u64,
    },
    Ready,
}

/// Where the backfill is: messages up to `top` (a `node.seq`) have been looked at, with tools
/// covered or not.
#[derive(Debug, Default)]
struct Pass {
    top: i64,
    tools: Option<bool>,
    /// When the last pass over every message began.
    full: Option<Instant>,
}

fn mb(n: u64) -> String {
    format!("{:.0} MB", n as f64 / 1_000_000.0)
}

/// Where the knowledge base's backfill is: chunks up to `top` (a `chunk.seq`) have been
/// looked at.
#[derive(Debug, Default)]
struct KbPass {
    top: i64,
    full: Option<Instant>,
}

impl Daemon {
    pub(crate) fn meaning_on(&self) -> bool {
        let c = self.config();
        (c.chat.enabled || c.kb.enabled) && c.kb.meaning
    }

    fn meaning_dir(&self) -> PathBuf {
        self.paths.data.join("models").join(ptyro_meaning::NAME)
    }

    fn set_meaning(&self, s: State) {
        *self.chats.meaning.state.locked() = s;
    }

    /// Start the task (at the daemon's start): it waits until meaning search is on, and then
    /// works on whichever of `chat.db` and `kb.db` is there.
    pub(crate) fn start_meaning(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move { me.meaning_task().await });
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut events = me.events.subscribe();
            loop {
                match events.recv().await {
                    Ok(DomainEvent::ChatUpdated { .. } | DomainEvent::ConfigReloaded { .. })
                    | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        me.chats.meaning.wake.notify_one();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    /// `chat.db`, when chats are on and it opened.
    pub(crate) fn meaning_chats(&self) -> Option<ChatDb> {
        if !self.config().chat.enabled {
            return None;
        }
        self.chats.db().ok().cloned()
    }

    async fn meaning_task(self: Arc<Self>) {
        let mut failed: Option<Instant> = None;
        let mut was_on = false;
        let mut pass = Pass::default();
        let mut kb_pass = KbPass::default();
        loop {
            let on = self.meaning_on();
            if !on {
                self.set_meaning(State::Off);
                *self.chats.meaning.model.locked() = None;
                was_on = false;
                failed = None;
                self.meaning_wait(None).await;
                continue;
            }
            if !was_on {
                was_on = true;
                failed = None;
                pass = Pass::default();
                kb_pass = KbPass::default();
                let mut n = 0;
                if let Some(db) = self.meaning_chats() {
                    n += db.drop_other_vecs(MODEL_ID.into()).await.unwrap_or(0);
                }
                if let Ok(kb) = self.kb_db() {
                    n += kb.drop_other_vecs(MODEL_ID.into()).await.unwrap_or(0);
                }
                if n > 0 {
                    tracing::info!("meaning search: {n} vector(s) of an older model dropped");
                }
            }
            let dir = self.meaning_dir();
            if !have_model(&dir) {
                if let Some(at) = failed
                    && at.elapsed() < RETRY
                {
                    self.meaning_wait(Some(RETRY - at.elapsed())).await;
                    continue;
                }
                match self.meaning_fetch(&dir).await {
                    Ok(true) => tracing::info!("meaning search: its model is in {}", dir.display()),
                    // Switched off meanwhile: what came down stays for next time.
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::warn!("meaning search: {e}");
                        self.set_meaning(State::Failed(e));
                        failed = Some(Instant::now());
                        continue;
                    }
                }
            }
            let model = match self.meaning_model().await {
                Ok(m) => m,
                Err(e) => {
                    // A model that won't load is fetched again (after a pause).
                    for f in FILES {
                        let _ = std::fs::remove_file(dir.join(f.name));
                    }
                    let e = format!("its model couldn't be read ({e}); it is fetched again");
                    tracing::warn!("meaning search: {e}");
                    self.set_meaning(State::Failed(e));
                    failed = Some(Instant::now());
                    continue;
                }
            };
            if let Some(db) = self.meaning_chats()
                && let Err(e) = self.meaning_backfill(&db, Arc::clone(&model), &mut pass).await
            {
                tracing::warn!("meaning search: {e}");
            }
            if let Ok(kb) = self.kb_db()
                && let Err(e) = self.kb_backfill(&kb, model, &mut kb_pass).await
            {
                tracing::warn!("meaning search: {e}");
            }
            // Idle: drop the model when nothing has used it for a while.
            let idle = {
                let mut m = self.chats.meaning.model.locked();
                match m.as_ref().map(|(_, used)| used.elapsed()) {
                    Some(e) if e >= IDLE => {
                        *m = None;
                        None
                    }
                    Some(e) => Some(IDLE - e),
                    None => None,
                }
            };
            self.meaning_wait(idle.map(|d| d + Duration::from_secs(1))).await;
        }
    }

    /// Until something changes (or `at_most` passes). A burst of writes (a streaming answer)
    /// wakes it once.
    async fn meaning_wait(&self, at_most: Option<Duration>) {
        let wake = self.chats.meaning.wake.notified();
        match at_most {
            Some(d) => {
                let _ = tokio::time::timeout(d, wake).await;
            }
            None => wake.await,
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// One pass: embed every message above `pass.top` that has no vector, newest first, a batch
    /// at a time, until none is left or it is switched off; then `pass.top` is where the next
    /// pass starts (the first pass reads the whole table once; later ones only what is new).
    async fn meaning_backfill(
        &self,
        db: &ChatDb,
        model: Arc<Model>,
        pass: &mut Pass,
    ) -> Result<(), String> {
        let tools = self.config().chat.index_tools;
        // Every message is looked at again when tools are covered now, or no longer, and once an
        // hour (a message whose text changed after its pass, a tool's output, is caught then).
        if pass.tools != Some(tools) || pass.full.is_none_or(|t| t.elapsed() > FULL_PASS) {
            *pass = Pass { top: 0, tools: Some(tools), full: Some(Instant::now()) };
        }
        let e = |e: ptyro_store::StoreError| e.to_string();
        let top = db.vec_top(pass.top).await.map_err(e)?;
        // Progress is counted on the first pass (the long one).
        let (mut done, all) = if pass.top == 0 {
            db.vec_counts(MODEL_ID.into(), tools).await.map_err(e)?
        } else {
            (0, 0)
        };
        let mut below = top + 1;
        while self.meaning_on() {
            let todo =
                db.vec_todo(MODEL_ID.into(), tools, (pass.top, below), BATCH).await.map_err(e)?;
            let Some(last) = todo.last() else { break };
            below = last.seq;
            if all > 0 {
                self.set_meaning(State::Embedding { done, all: all.max(done) });
            }
            let n = todo.len() as u64;
            let m = Arc::clone(&model);
            let rows = tokio::task::spawn_blocking(move || {
                todo.into_iter()
                    .map(|t| {
                        let v = ptyro_meaning::quantize(&m.embed(&t.text));
                        (t.node, t.text, v)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| e.to_string())?;
            db.put_vecs(MODEL_ID.into(), rows).await.map_err(e)?;
            done += n;
            self.touch_model();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if self.meaning_on() {
            pass.top = top;
            self.set_meaning(State::Ready);
        }
        Ok(())
    }

    /// The knowledge base's pass: embed every prose chunk above `pass.top` that has no
    /// vector, newest first, a batch at a time. The first pass (and one an hour) reads the
    /// whole table; the others only what is new.
    async fn kb_backfill(
        &self,
        db: &KbDb,
        model: Arc<Model>,
        pass: &mut KbPass,
    ) -> Result<(), String> {
        if pass.full.is_none_or(|t| t.elapsed() > FULL_PASS) {
            *pass = KbPass { top: 0, full: Some(Instant::now()) };
        }
        let e = |e: ptyro_store::StoreError| e.to_string();
        let top = db.vec_top().await.map_err(e)?;
        let mut below = top + 1;
        while self.meaning_on() && top > pass.top {
            let todo = db.vec_todo(MODEL_ID.into(), (pass.top, below), BATCH).await.map_err(e)?;
            let Some(last) = todo.last() else { break };
            below = last.seq;
            let m = Arc::clone(&model);
            let rows = tokio::task::spawn_blocking(move || {
                todo.into_iter()
                    .map(|t| {
                        let v = ptyro_meaning::quantize(&m.embed(&t.text));
                        (t.seq, t.text, v)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| e.to_string())?;
            db.put_vecs(MODEL_ID.into(), rows).await.map_err(e)?;
            self.touch_model();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if self.meaning_on() {
            pass.top = top;
        }
        Ok(())
    }

    /// The embed step of an ingest (SPEC §25.5): the prose chunks of `item` that have no
    /// vector get one, when meaning search is on and its model is here. Without the model
    /// nothing is done, and the task's pass embeds them when it arrives.
    pub(crate) async fn kb_embed(&self, db: &KbDb, item: &str) {
        if !self.meaning_on() || !have_model(&self.meaning_dir()) {
            return;
        }
        let Ok(model) = self.meaning_model().await else { return };
        let Ok(todo) = db.vec_todo_of(item.to_owned(), MODEL_ID.into()).await else { return };
        if todo.is_empty() {
            return;
        }
        let rows = tokio::task::spawn_blocking(move || {
            todo.into_iter()
                .map(|t| {
                    let v = ptyro_meaning::quantize(&model.embed(&t.text));
                    (t.seq, t.text, v)
                })
                .collect::<Vec<_>>()
        })
        .await;
        if let Ok(rows) = rows
            && let Err(e) = db.put_vecs(MODEL_ID.into(), rows).await
        {
            tracing::warn!("meaning search: {e}");
        }
    }

    fn touch_model(&self) {
        if let Some((_, used)) = self.chats.meaning.model.locked().as_mut() {
            *used = Instant::now();
        }
    }

    /// The model, loaded if it isn't.
    pub(crate) async fn meaning_model(&self) -> Result<Arc<Model>, String> {
        if let Some((m, used)) = self.chats.meaning.model.locked().as_mut() {
            *used = Instant::now();
            return Ok(Arc::clone(m));
        }
        let dir = self.meaning_dir();
        let m = tokio::task::spawn_blocking(move || Model::load(&dir))
            .await
            .map_err(|e| e.to_string())??;
        let m = Arc::new(m);
        *self.chats.meaning.model.locked() = Some((Arc::clone(&m), Instant::now()));
        // (The task times its unloading.)
        self.chats.meaning.wake.notify_one();
        Ok(m)
    }

    /// Download the model into `dir`, showing its progress. `Ok(false)`: switched off meanwhile.
    async fn meaning_fetch(&self, dir: &Path) -> Result<bool, String> {
        let base = std::env::var("PTYRO_MEANING_URL").unwrap_or_else(|_| SOURCE.to_owned());
        let go_on = |got: u64| {
            self.set_meaning(State::Fetching { got });
            self.meaning_on()
        };
        fetch(dir, &base, &FILES, &go_on).await
    }

    /// `ptyro doctor`'s line about meaning search.
    pub(crate) async fn meaning_doctor(&self) -> Option<String> {
        let config = self.config();
        if !config.chat.enabled && !config.kb.enabled {
            return None;
        }
        let dir = self.meaning_dir();
        let state = self.chats.meaning.state.locked().clone();
        let total = mb(ptyro_meaning::total_bytes());
        Some(match state {
            State::Off if have_model(&dir) => format!(
                "meaning search: off ([kb] meaning); its model ({total}) stays in {} until you delete it",
                dir.display()
            ),
            State::Off => format!(
                "meaning search: off ([kb] meaning = true finds things by what they are about; it downloads {total} once)"
            ),
            State::Fetching { got } => {
                format!("meaning search: downloading its model, {} of {total}", mb(got))
            }
            State::Failed(e) => format!("meaning search: {e}"),
            State::Embedding { done, all } => {
                format!("meaning search: {done} of {all} messages embedded so far")
            }
            State::Ready => {
                let tools = self.config().chat.index_tools;
                let n = match self.meaning_chats() {
                    Some(db) => db.vec_counts(MODEL_ID.into(), tools).await.ok(),
                    None => None,
                };
                let kb = match self.kb_db() {
                    Ok(db) => db.vec_counts(MODEL_ID.into()).await.ok(),
                    Err(_) => None,
                };
                let (done, all) = n.unwrap_or((0, 0));
                let (kb_done, kb_all) = kb.unwrap_or((0, 0));
                if done < all || kb_done < kb_all {
                    format!(
                        "meaning search: {done} of {all} messages and {kb_done} of {kb_all} pieces of the knowledge base embedded so far"
                    )
                } else if n.is_none() && kb.is_none() {
                    "meaning search: ready".into()
                } else {
                    format!(
                        "meaning search: ready, {done} messages and {kb_done} pieces of the knowledge base ({} in {})",
                        ptyro_meaning::NAME,
                        dir.display()
                    )
                }
            }
        })
    }

    /// The query's vector, when it is to be searched by meaning too. `Ok((None, why))`: by
    /// words alone, and `why` says so when that was not the query's own choice (meaning search
    /// is off, or its model isn't here yet). `by:meaning` while it can't be done is an error.
    pub(crate) async fn meaning_vector(
        &self,
        q: &Query,
    ) -> Result<(Option<Vec<f32>>, Option<String>), Error> {
        let by = q.by.as_deref();
        let on = self.meaning_on();
        if by == Some("meaning") {
            self.meaning_ready(on)?;
        }
        if by == Some("words") {
            return Ok((None, None));
        }
        if !on {
            return Ok((None, Some("by words only: meaning search is off (Knowledge → meaning in the settings switches it on)".into())));
        }
        if !have_model(&self.meaning_dir()) {
            return Ok((None, Some("by words only: meaning search's model isn't here yet".into())));
        }
        let text = q.words.join(" ");
        if text.is_empty() {
            return Ok((None, None));
        }
        match self.meaning_model().await {
            Ok(m) => {
                let v = m.embed(&text);
                // (No word of the query is one the model knows: nothing is near it.)
                Ok(((!v.iter().all(|x| *x == 0.0)).then_some(v), None))
            }
            Err(e) if by == Some("meaning") => {
                Err(Error::new(ErrorCode::Internal, format!("meaning search: {e}")))
            }
            Err(e) => Ok((
                None,
                Some(format!("by words only: meaning search's model couldn't be read ({e})")),
            )),
        }
    }

    /// A chat search's two lists (SPEC §22.10), each best first: the keyword hits, and when
    /// `near` is the query's vector, the messages nearest in meaning. `by:meaning` in the
    /// query leaves the first empty.
    pub(crate) async fn chat_lists(
        &self,
        db: &ChatDb,
        filter: SearchFilter,
        q: &Query,
        near: Option<Vec<f32>>,
    ) -> Result<(Vec<RawHit>, Vec<RawHit>), Error> {
        let words = if q.by.as_deref() == Some("meaning") {
            Vec::new()
        } else {
            db.search(filter.clone()).await.map_err(store_err)?
        };
        let near = match near {
            Some(v) => db
                .meaning(filter, v, MODEL_ID.into(), q.not.clone(), ptyro_meaning::MIN_SCORE)
                .await
                .map_err(store_err)?,
            None => Vec::new(),
        };
        Ok((words, near))
    }

    /// A search's hits (SPEC §22.10): the keyword hits, and when meaning search is on, the
    /// messages nearest in meaning, merged by reciprocal-rank fusion; each with how it was found.
    /// `by:words` or `by:meaning` in the query asks for one kind only.
    pub(crate) async fn search_hits(
        &self,
        db: &ChatDb,
        filter: SearchFilter,
        q: &Query,
    ) -> Result<Vec<(RawHit, Found)>, Error> {
        let limit = filter.limit;
        let (vector, _) = self.meaning_vector(q).await?;
        let (words, near) = self.chat_lists(db, filter, q, vector).await?;
        if near.is_empty() {
            return Ok(words.into_iter().map(|h| (h, Found::Words)).collect());
        }
        let key = |h: &RawHit| (h.chat.id.clone(), h.node.clone());
        let order = ptyro_meaning::fuse(
            &words.iter().map(key).collect::<Vec<_>>(),
            &near.iter().map(key).collect::<Vec<_>>(),
        );
        let mut by_key: std::collections::HashMap<_, RawHit> =
            near.into_iter().map(|h| (key(&h), h)).collect();
        // Found both ways: the keyword hit, whose snippet shows the words.
        by_key.extend(words.into_iter().map(|h| (key(&h), h)));
        // Each kind brought up to `limit`; the best `limit` of both are the answer.
        let limit = if limit == 0 { usize::MAX } else { limit };
        Ok(order
            .into_iter()
            .filter_map(|(k, how, score)| {
                let mut h = by_key.remove(&k)?;
                h.rank = -score * 1000.0;
                Some((h, how))
            })
            .take(limit)
            .collect())
    }

    /// `by:meaning` needs it on, and its model here.
    fn meaning_ready(&self, on: bool) -> Result<(), Error> {
        let total = mb(ptyro_meaning::total_bytes());
        if !on {
            return Err(Error::new(
                ErrorCode::NeedsConfirmation,
                format!(
                    "meaning search is off. Switching it on downloads a {total} model once (Knowledge → meaning in the settings)"
                ),
            ));
        }
        if have_model(&self.meaning_dir()) {
            return Ok(());
        }
        let msg = match self.chats.meaning.state.locked().clone() {
            State::Fetching { got } => {
                format!(
                    "meaning search is getting ready: its model is downloading, {} of {total}",
                    mb(got)
                )
            }
            State::Failed(e) => format!("meaning search: {e}"),
            _ => "meaning search is getting ready: its model is about to download".into(),
        };
        Err(Error::new(ErrorCode::Busy, msg))
    }
}

/// Download `files` into `dir`, each to `<file>.part` (resumed where an earlier try stopped),
/// checked, then renamed into place. `go_on` hears the bytes so far, often, and says whether to
/// carry on. `Ok(false)`: it said stop (the parts stay for next time).
async fn fetch(
    dir: &Path,
    base: &str,
    files: &[ptyro_meaning::File],
    go_on: &(dyn Fn(u64) -> bool + Sync),
) -> Result<bool, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut before = 0;
    for f in files {
        let path = dir.join(f.name);
        if path.exists() {
            before += f.bytes;
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let have = std::fs::metadata(&part).map_or(0, |m| m.len());
        // A file of no known size starts over (it is small); a known one resumes, unless the
        // part is all there already or longer than the file is.
        if f.bytes == 0 || have > f.bytes {
            let _ = std::fs::remove_file(&part);
        }
        if f.bytes == 0 || have != f.bytes {
            let url = ptyro_meaning::url(base, f.name);
            if !curl(&url, &part, f.bytes, before, go_on).await? {
                return Ok(false);
            }
        }
        check(&part, *f).await?;
        std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        before += f.bytes;
    }
    Ok(true)
}

/// One file with curl (as notifications are sent). `Ok(false)`: `go_on` said stop, and curl is
/// stopped.
async fn curl(
    url: &str,
    part: &Path,
    max: u64,
    before: u64,
    go_on: &(dyn Fn(u64) -> bool + Sync),
) -> Result<bool, String> {
    let proto = if url.starts_with("file://") { "=file" } else { "=https" };
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-sS", "-fL", "--proto", proto, "--proto-redir", proto])
        .args(["--retry", "3", "--connect-timeout", "20", "-C", "-"])
        // A stalled transfer (under 1 KB/s for a minute) fails, and is retried, resuming.
        .args(["--speed-limit", "1024", "--speed-time", "60"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if max > 0 {
        cmd.args(["--max-filesize", &max.to_string()]);
    }
    cmd.arg("-o").arg(part).arg(url);
    let mut child =
        cmd.spawn().map_err(|e| format!("can't run curl to download its model: {e}"))?;
    let mut stderr = child.stderr.take();
    loop {
        if !go_on(before + std::fs::metadata(part).map_or(0, |m| m.len())) {
            let _ = child.kill().await;
            return Ok(false);
        }
        match tokio::time::timeout(Duration::from_millis(300), child.wait()).await {
            Ok(Ok(st)) if st.success() => return Ok(true),
            Ok(Ok(st)) => {
                let mut err = String::new();
                if let Some(s) = stderr.as_mut() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_string(&mut err).await;
                }
                let why = err.lines().next().unwrap_or("").trim().to_owned();
                return Err(format!(
                    "its model didn't download ({}); it is tried again in 10 minutes, or switch it off and on",
                    if why.is_empty() { st.to_string() } else { why },
                ));
            }
            Ok(Err(e)) => return Err(format!("curl: {e}")),
            Err(_) => {}
        }
    }
}

/// Every file of the model is here (a file is only renamed into place once checked).
fn have_model(dir: &Path) -> bool {
    FILES.iter().all(|f| dir.join(f.name).is_file())
}

/// A downloaded file is the one pinned: its size, and the matrix's SHA-256.
async fn check(part: &Path, f: ptyro_meaning::File) -> Result<(), String> {
    let len = std::fs::metadata(part).map_or(0, |m| m.len());
    if f.bytes > 0 && len != f.bytes {
        let _ = std::fs::remove_file(part);
        return Err(format!(
            "{} came down as {len} bytes, not {}; it is fetched again next time",
            f.name, f.bytes
        ));
    }
    if let Some(want) = f.sha256 {
        let p = part.to_path_buf();
        let got = tokio::task::spawn_blocking(move || sha256_file(&p))
            .await
            .map_err(|e| e.to_string())??;
        if got != want {
            let _ = std::fs::remove_file(part);
            return Err(format!(
                "{} came down damaged (its checksum doesn't match); it is fetched again next time",
                f.name
            ));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn store_err(e: ptyro_store::StoreError) -> Error {
    Error::new(ErrorCode::Internal, format!("chat.db: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_meaning::File;

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[tokio::test]
    async fn the_download_resumes_is_checked_and_stops_when_asked() {
        // A source laid out as Hugging Face's, with a tiny model: nothing leaves this machine.
        let src = tempfile::tempdir().unwrap();
        let repo = src.path().join(ptyro_meaning::REPO).join("resolve").join(ptyro_meaning::COMMIT);
        ptyro_meaning::synthetic::write(&repo, &[("cat", &[1.0, 0.0]), ("dog", &[0.0, 1.0])])
            .unwrap();
        let size = |n: &str| std::fs::metadata(repo.join(n)).unwrap().len();
        let sha = leak(sha256_file(&repo.join("model.safetensors")).unwrap());
        let files = [
            File { name: "model.safetensors", bytes: size("model.safetensors"), sha256: Some(sha) },
            File { name: "tokenizer.json", bytes: size("tokenizer.json"), sha256: None },
            File { name: "config.json", bytes: 0, sha256: None },
        ];
        let base = format!("file://{}", src.path().display());

        // An interrupted try left part of the matrix: the rest is fetched, and it all checks.
        let dest = tempfile::tempdir().unwrap();
        let whole = std::fs::read(repo.join("model.safetensors")).unwrap();
        std::fs::write(dest.path().join("model.safetensors.part"), &whole[..20]).unwrap();
        let seen = std::sync::atomic::AtomicU64::new(0);
        let go_on = |got: u64| {
            seen.fetch_max(got, std::sync::atomic::Ordering::Relaxed);
            true
        };
        assert!(fetch(dest.path(), &base, &files, &go_on).await.unwrap());
        assert_eq!(std::fs::read(dest.path().join("model.safetensors")).unwrap(), whole);
        assert!(!dest.path().join("model.safetensors.part").exists());
        let m = Model::load(dest.path()).unwrap();
        assert!(m.embed("a cat and a dog").iter().any(|x| *x != 0.0));
        assert!(seen.load(std::sync::atomic::Ordering::Relaxed) > 0, "progress is reported");
        // Done is done: nothing is fetched again.
        assert!(fetch(dest.path(), "file:///nowhere", &files, &|_| true).await.unwrap());

        // Switched off: it stops, and says so.
        let off = tempfile::tempdir().unwrap();
        assert!(!fetch(off.path(), &base, &files, &|_| false).await.unwrap());
        assert!(!off.path().join("model.safetensors").exists());

        // A file that isn't the pinned one is refused and removed (fetched again next time).
        let bad = [File { sha256: Some("00"), ..files[0] }];
        let d = tempfile::tempdir().unwrap();
        let e = fetch(d.path(), &base, &bad, &|_| true).await.unwrap_err();
        assert!(e.contains("damaged"), "{e}");
        assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
        let long = [File { bytes: files[1].bytes - 1, ..files[1] }];
        assert!(fetch(d.path(), &base, &long, &|_| true).await.is_err());
        // No source: curl's own words.
        let e = fetch(d.path(), "file:///nowhere/at/all", &files, &|_| true).await.unwrap_err();
        assert!(e.contains("didn't download"), "{e}");
    }
}
