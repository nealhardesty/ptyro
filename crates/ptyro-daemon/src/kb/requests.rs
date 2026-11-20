//! The knowledge base's requests (SPEC §25.14): what `ptyro recall`, `ptyro kb`, the Library and
//! the agents' tools ask. Search, read and the history answer through [`super::search`];
//! forgetting really deletes (§25.12); the status is answered when the knowledge base is off too.
//!
//! **Who asks decides what is seen** (§25.9). You see everything. A request that comes from
//! inside an agent's session (the agent running `ptyro recall` in its terminal, or its tools)
//! is answered within the wall: `kb.scope`, the session's profile and workspace, and never
//! that session's own item. Which session that is comes from the kernel (the caller's
//! process), never from the request.

use ptyro_core::kb::search::{
    HitKind, KbCount, KbHit, KbListed, KbSearch, KbSkipped, KbStatusInfo, LIMIT, MAX_LIMIT,
};
use ptyro_core::kb::{ItemKind, KbItem};
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, lookup};
use ptyro_protocol::{Reply, Request};
use ptyro_store::kb::KbDb;

use super::search::KbScope;
use super::store_err;
use crate::daemon::Daemon;

/// Neighbours shown each side of the piece an address names, when the request doesn't say.
const AROUND: u32 = 2;
/// How many titles a question about forgetting names before it counts the rest.
const NAMED: usize = 8;

impl Daemon {
    /// The wall for whoever sent a request (SPEC §25.9): `None` for you (a client, a paired
    /// device, a plain shell), and for an agent's session what `kb.scope` lets its profile
    /// and workspace see, without its own session and chat.
    pub(crate) fn kb_caller_scope(&self, caller: crate::guard::Caller) -> Option<KbScope> {
        let s = self.kb_agent(caller)?;
        let place = s.meta.locked().place.clone();
        let workspace = self.kb_workspace_id(place.workspace.as_deref());
        let mut scope = KbScope::of(&self.config().kb.scope, &place.profile, workspace.as_deref());
        scope.session = Some(s.id.full());
        scope.chat = place.chat;
        Some(scope)
    }

    /// What an item's project and workspace are called here, if they still are.
    fn kb_names(&self, project: Option<&str>, workspace: Option<&str>) -> [Option<String>; 2] {
        let reg = self.registry.locked();
        [
            project.and_then(|id| reg.projects.iter().find(|p| p.id == id)).map(|p| p.slug.clone()),
            workspace
                .and_then(|id| reg.workspaces.iter().find(|w| w.id == id))
                .map(|w| w.name.clone()),
        ]
    }

    fn kb_named(&self, hits: &mut [KbHit]) {
        for h in hits {
            [h.project_name, h.workspace_name] =
                self.kb_names(h.project.as_deref(), h.workspace.as_deref());
        }
    }

    /// Items as a list shows them: each with its handle and its place's names.
    pub(super) async fn kb_listed(
        &self,
        db: &KbDb,
        items: Vec<KbItem>,
    ) -> Result<Vec<KbListed>, Error> {
        let ids = db.ids().await.map_err(store_err)?;
        Ok(items
            .into_iter()
            .map(|item| {
                let [project_name, workspace_name] =
                    self.kb_names(item.project.as_deref(), item.workspace.as_deref());
                KbListed {
                    addr: format!("k:{}", lookup::handle(&item.id, ids.iter().map(String::as_str))),
                    item,
                    project_name,
                    workspace_name,
                }
            })
            .collect())
    }

    pub(crate) async fn handle_kb(
        self: &std::sync::Arc<Self>,
        req: Request,
        caller: crate::guard::Caller,
    ) -> Result<Reply, Error> {
        let scope = self.kb_caller_scope(caller);
        // Another machine asking for an agent of its own (§25.11): that profile's, by name.
        // What the request says only narrows: a caller's own wall comes first.
        let asked = match &req {
            Request::KbSearch { profile: Some(p), .. }
            | Request::KbRead { profile: Some(p), .. } => {
                Some(KbScope { profile: Some(p.clone()), ..KbScope::default() })
            }
            _ => None,
        };
        let scope = scope.as_ref().or(asked.as_ref());
        // A wall that is a workspace is one of this machine's: nothing is asked elsewhere.
        let walled = scope.is_some_and(|s| s.workspace.is_some());
        let stays = |m: &str| {
            Error::invalid(format!(
                "this session sees its own workspace only (`kb.scope`): {m} isn't asked"
            ))
        };
        // Roots are managed on their own machine (§25.11): one that names a linked machine
        // is answered there.
        let req = match self.kb_roots_far(req, scope.is_some()).await? {
            Ok(reply) => return Ok(reply),
            Err(req) => req,
        };
        match req {
            Request::KbSearch {
                query,
                project,
                kinds,
                since_ms,
                before_ms,
                limit,
                here,
                machines,
                machine,
                profile: _,
            } => {
                // Where to look: one machine by name (the option, or the query's `machine:`),
                // every linked one too (`machines`), else here.
                let (query, named) = ptyro_core::chats::take_machine(&query);
                let only = match machine.or(named) {
                    Some(m) => Some(self.kb_machine(&m)?),
                    None => None,
                };
                let far = match &only {
                    Some(Some(m)) if walled => return Err(stays(m)),
                    Some(Some(m)) => vec![m.clone()],
                    Some(None) => Vec::new(),
                    None if machines && !walled => self.kb_linked(),
                    None => Vec::new(),
                };
                let alone = matches!(only, Some(Some(_)));
                let (mut lists, mut words_only, mut missing) = (Vec::new(), None, None);
                if !alone {
                    // Where the caller is, as a project: a boost, so a place that is none is
                    // nothing.
                    let here = here.and_then(|h| self.find_project(&h).ok()).map(|p| p.id);
                    let ask = KbSearch {
                        query: query.clone(),
                        project: project.clone(),
                        kinds: kinds.clone(),
                        since_ms,
                        before_ms,
                        limit: limit.unwrap_or(0),
                        here,
                    };
                    match self.kb_search(ask, scope).await {
                        Ok(mut found) => {
                            self.kb_named(&mut found.hits);
                            words_only = found.words_only;
                            lists.push(found.hits);
                        }
                        // A project this machine doesn't have may be another's.
                        Err(e) if e.code == ErrorCode::NotFound && !far.is_empty() => {
                            missing = Some(e);
                        }
                        Err(e) => return Err(e),
                    }
                }
                if far.is_empty() {
                    let hits = lists.pop().unwrap_or_default();
                    return Ok(Reply::KbHits { hits, words_only, left_out: Vec::new() });
                }
                // Each machine searches its own by its own settings. A project goes by its
                // name (ids are each machine's own).
                let ask = Request::KbSearch {
                    query,
                    project: project.map(|p| self.find_project(&p).map_or(p, |p| p.slug)),
                    kinds,
                    since_ms,
                    before_ms,
                    limit,
                    here: None,
                    machines: false,
                    machine: None,
                    profile: scope.and_then(|s| s.profile.clone()),
                };
                let (theirs, left_out) = self.kb_search_far(&far, &ask, alone).await?;
                if let (Some(e), true) = (missing, theirs.is_empty()) {
                    return Err(e);
                }
                lists.extend(theirs);
                // Interleaved by each machine's own rank: scores don't compare across them.
                let mut hits = crate::chats::interleave_by(lists, |h| h.at_ms);
                hits.truncate(limit.map_or(LIMIT, |n| n.min(MAX_LIMIT)) as usize);
                Ok(Reply::KbHits { hits, words_only, left_out })
            }
            Request::KbRead { addr, before, after, profile: _ } => {
                let (machine, addr) = self.kb_split(&addr);
                if lookup::key(&addr).starts_with("c:") {
                    return Err(Error::invalid(format!(
                        "{addr} is a chat's: `ptyro chat show` opens it"
                    )));
                }
                // Another machine's is read there, within the same wall.
                if let Some(m) = machine {
                    if walled {
                        return Err(stays(&m));
                    }
                    let profile = scope.and_then(|s| s.profile.clone());
                    let req = Request::KbRead { addr, before, after, profile };
                    return match self.kb_far(&m, req).await? {
                        Reply::KbDoc { mut item, chunks, at, stale } => {
                            item.addr = format!("{m}:{}", item.addr);
                            Ok(Reply::KbDoc { item, chunks, at, stale })
                        }
                        other => Err(Error::internal(format!("{m} answered {other:?}"))),
                    };
                }
                // A piece with its neighbours; an item from its start, all of it unless told.
                let piece = addr.contains('/');
                let doc = self
                    .kb_read(
                        &addr,
                        before.unwrap_or(if piece { AROUND } else { 0 }),
                        after.unwrap_or(if piece { AROUND } else { u32::MAX }),
                        scope,
                    )
                    .await?;
                let db = self.kb_db()?;
                let item = self.kb_listed(&db, vec![doc.item]).await?.remove(0);
                Ok(Reply::KbDoc { item, chunks: doc.chunks, at: doc.at, stale: doc.stale })
            }
            Request::KbRecent {
                kinds,
                project,
                session,
                worktree,
                since_ms,
                before_ms,
                limit,
                machine,
                writer,
            } => {
                // A linked machine's history, asked for by name: you may, a model may not
                // (its wall is its profile here, and a history has no profile to be given).
                if let Some(m) = machine.map(|m| self.kb_machine(&m)).transpose()?.flatten() {
                    if scope.is_some() {
                        return Err(Error::new(
                            ErrorCode::PermissionDenied,
                            format!("an agent's session lists this machine's history, not {m}'s"),
                        ));
                    }
                    let req = Request::KbRecent {
                        kinds,
                        project: project.map(|p| self.find_project(&p).map_or(p, |p| p.slug)),
                        session,
                        worktree,
                        since_ms,
                        before_ms,
                        limit,
                        machine: None,
                        writer,
                    };
                    return match self.kb_far(&m, req).await? {
                        Reply::KbItems { mut items } => {
                            for i in &mut items {
                                i.addr = format!("{m}:{}", i.addr);
                            }
                            Ok(Reply::KbItems { items })
                        }
                        other => Err(Error::internal(format!("{m} answered {other:?}"))),
                    };
                }
                let of = super::search::Of { session, worktree, since_ms, writer };
                let items = self
                    .kb_recent(kinds, project, of, before_ms, limit.unwrap_or(0), scope)
                    .await?;
                let db = self.kb_db()?;
                Ok(Reply::KbItems { items: self.kb_listed(&db, items).await? })
            }
            Request::KbForget { session: Some(session), confirmed, .. } => {
                self.kb_forget_session(&session, confirmed).await
            }
            Request::KbForget { addrs, query, confirmed, session: None } => {
                // Another machine's items are forgotten there (its file, its tombstones).
                let split: Vec<(Option<String>, String)> =
                    addrs.iter().map(|a| self.kb_split(a)).collect();
                if let Some(m) = split.iter().find_map(|(m, _)| m.clone()) {
                    if split.iter().any(|(x, _)| x.as_ref() != Some(&m)) {
                        return Err(Error::invalid(format!(
                            "these are on more than one machine ({m} and another): one at a time"
                        )));
                    }
                    let addrs = split.into_iter().map(|(_, a)| a).collect();
                    let req = Request::KbForget { addrs, query: None, confirmed, session: None };
                    return self.kb_far(&m, req).await;
                }
                let addrs = split.into_iter().map(|(_, a)| a).collect();
                self.kb_forget(addrs, query, confirmed).await
            }
            Request::KbStatus => Ok(Reply::KbStatus { status: self.kb_status().await }),
            Request::KbNote { text, title, project, profile, tags, replaces, addr } => {
                let ask =
                    super::notes::NoteAsk { text, title, project, profile, tags, replaces, addr };
                self.kb_note(ask, caller, scope).await
            }
            Request::KbNotes { project, writer } => {
                let items = self.kb_notes(project, writer, scope).await?;
                let db = self.kb_db()?;
                Ok(Reply::KbItems { items: self.kb_listed(&db, items).await? })
            }
            Request::KbNoteRead { addr } => self.kb_note_read(&addr, scope).await,
            Request::KbSync { roots, machine: _ } => {
                // Every root and the notes folder; or only the roots named.
                let (notes, changed, removed) =
                    if roots.is_empty() { self.kb_notes_sync().await? } else { (0, 0, 0) };
                let roots = self.kb_roots_sync(&roots).await?;
                Ok(Reply::KbSynced { notes, changed, removed, roots })
            }
            Request::KbRoots { machine: _ } => {
                Ok(Reply::KbRoots { roots: self.kb_roots(scope).await? })
            }
            // What is read, and what is dropped, is yours to say: an agent's session could
            // read another profile's folder in, or drop what you chose to keep.
            Request::KbIngest { .. } | Request::KbRemove { .. } | Request::KbHistory { .. }
                if scope.is_some() =>
            {
                Err(Error::new(
                    ErrorCode::PermissionDenied,
                    "an agent's session doesn't add or remove what the knowledge base reads: the user does, with `ptyro ingest` and `ptyro kb rm`",
                ))
            }
            Request::KbIngest { spec, machine: _ } => self.kb_ingest_root(spec).await,
            Request::KbRemove { root, confirmed, machine: _ } => {
                self.kb_remove_root(&root, confirmed).await
            }
            Request::KbHistory { agent, profile, since_ms, dry_run, machine: _ } => {
                let plan = self.kb_history(agent, profile, since_ms, dry_run).await?;
                Ok(Reply::KbHistory { plan })
            }
            other => Err(Error::internal(format!("not a knowledge base request: {other:?}"))),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;

impl Daemon {
    /// `KbForget` (§25.12): the items the addresses name and those the query finds. Unless
    /// `confirmed`, nothing goes and the answer says what would.
    async fn kb_forget(
        &self,
        addrs: Vec<String>,
        query: Option<String>,
        confirmed: bool,
    ) -> Result<Reply, Error> {
        let db = self.kb_db()?;
        let mut items: Vec<KbItem> = Vec::new();
        for addr in &addrs {
            if lookup::key(addr).starts_with("c:") {
                return Err(Error::invalid(format!(
                    "{addr} is a chat: `ptyro chat rm` deletes one"
                )));
            }
            let (item, _) = self.kb_find(&db, addr, None).await?;
            if !items.iter().any(|i| i.id == item.id) {
                items.push(item);
            }
        }
        let mut more = false;
        if let Some(query) = query.filter(|q| !q.trim().is_empty()) {
            let ask = KbSearch { query, limit: MAX_LIMIT, ..KbSearch::default() };
            let found = self.kb_search(ask, None).await?;
            more = found.hits.len() >= MAX_LIMIT as usize;
            for h in found.hits.iter().filter(|h| h.kind != HitKind::Chat) {
                if !items.iter().any(|i| i.id == h.item)
                    && let Some(item) = db.item(h.item.clone()).await.map_err(store_err)?
                {
                    items.push(item);
                }
            }
            if items.is_empty() {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    "nothing in the knowledge base matches (a chat is deleted with `ptyro chat rm`)",
                ));
            }
        }
        if items.is_empty() {
            return Err(Error::invalid(
                "say what to forget: an address a search gave (`k:7f3k`), or a query",
            ));
        }
        if !confirmed {
            let mut named: Vec<String> = items
                .iter()
                .take(NAMED)
                .map(|i| format!("{} “{}”", i.kind, i.title.as_deref().unwrap_or(i.uri.as_str())))
                .collect();
            if items.len() > NAMED {
                named.push(format!("{} more", items.len() - NAMED));
            }
            return Err(Error::new(
                ErrorCode::NeedsConfirmation,
                format!(
                    "Forget {}: {}?{} {}",
                    if items.len() == 1 {
                        "1 item".into()
                    } else {
                        format!("{} items", items.len())
                    },
                    named.join(", "),
                    if more {
                        " (The search found more than it lists: ask again for the rest.)"
                    } else {
                        ""
                    },
                    // A note is its file: that goes.
                    if items.iter().all(|i| i.kind == ItemKind::Note) {
                        ptyro_core::removal::KB_NOTE.to_owned()
                    } else if items.iter().any(|i| i.kind == ItemKind::Note) {
                        format!("{} {}", ptyro_core::removal::KB, ptyro_core::removal::KB_NOTE)
                    } else {
                        ptyro_core::removal::KB.to_owned()
                    }
                ),
            ));
        }
        let mut chunks = 0;
        for item in &items {
            if item.kind == ItemKind::Note {
                chunks += self.kb_note_forget(&db, item).await?;
                continue;
            }
            chunks += db.forget(item.id.clone()).await.map_err(store_err)?.unwrap_or(0);
            tracing::info!("knowledge base: {} forgotten", item.uri);
        }
        self.kb_changed(items.iter().map(|i| i.kind));
        Ok(Reply::KbForgotten { items: items.len() as u32, chunks: chunks as u32 })
    }

    /// `KbForget` of a session (§25.7a): everything kept of it goes (each conversation it
    /// had), the conversation it is in is never kept (a tombstone, when nothing was read of
    /// it yet), and the session is private from then on. Private is never switched off again.
    async fn kb_forget_session(&self, addr: &str, confirmed: bool) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        let db = self.kb_db()?;
        let filter = ptyro_store::kb::ItemFilter {
            session: Some(s.id.full()),
            ..ptyro_store::kb::ItemFilter::default()
        };
        let items = db.list(filter).await.map_err(store_err)?;
        if !confirmed {
            let name = s.meta.locked().name.clone();
            return Err(Error::new(
                ErrorCode::NeedsConfirmation,
                format!(
                    "Forget what was kept of the session {name} ({}), and keep nothing more of it? It is private from then on, and that can't be switched off again. {}",
                    match items.len() {
                        0 => "nothing yet".into(),
                        1 => "1 item".into(),
                        n => format!("{n} items"),
                    },
                    ptyro_core::removal::KB
                ),
            ));
        }
        // Private first: nothing is read of it while its items go.
        s.meta.locked().place.private = true;
        s.save(None);
        let _ =
            self.events.send(ptyro_protocol::DomainEvent::SessionUpdated { info: self.info(&s) });
        let mut chunks = 0;
        for item in &items {
            chunks += db.forget(item.id.clone()).await.map_err(store_err)?.unwrap_or(0);
            tracing::info!("knowledge base: {} forgotten", item.uri);
        }
        if let Some(uri) = self.kb_session_uri(&s) {
            db.tombstone(uri, ptyro_core::kb::ItemKind::Session).await.map_err(store_err)?;
        }
        self.kb_changed(items.iter().map(|i| i.kind));
        Ok(Reply::KbForgotten { items: items.len() as u32, chunks: chunks as u32 })
    }

    /// `KbStatus`: what the knowledge base holds. Off: what the settings say, and the file's
    /// size; the file is not opened for the asking.
    pub(crate) async fn kb_status(&self) -> KbStatusInfo {
        let config = self.config();
        let path = self.paths.data.join("kb.db");
        let mut out = KbStatusInfo {
            enabled: config.kb.enabled,
            sessions: config.kb.sessions,
            shell: config.kb.shell.clone(),
            retain_days: config.kb.retain_days,
            path: path.display().to_string(),
            notes: self.kb_notes_dir().display().to_string(),
            bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
            queued: *self.kb.pending.borrow() as u64,
            skipped: self
                .kb_skipped()
                .into_iter()
                .map(|s| KbSkipped { uri: s.uri, why: s.why, at_ms: s.at_ms })
                .collect(),
            ..KbStatusInfo::default()
        };
        if !config.kb.enabled {
            return out;
        }
        out.meaning = self.meaning_doctor().await;
        let db = match self.kb_db() {
            Ok(db) => db,
            Err(e) => {
                out.error = Some(e.message);
                return out;
            }
        };
        match db.status().await {
            Ok(s) => {
                out.bytes = s.bytes;
                out.kinds = s
                    .items
                    .into_iter()
                    .map(|(kind, items, chunks)| KbCount { kind, items, chunks })
                    .collect();
                out.vectors = s.vectors;
                out.roots = s.roots;
                if let Ok(roots) = db.roots().await {
                    out.failing = super::roots::failing(&roots);
                }
                out.forgotten = s.forgotten;
            }
            Err(e) => out.error = Some(store_err(e).message),
        }
        out
    }

    // ── retention (§25.12) ───────────────────────────────────────────────────────────────

    /// Before when an agent's session is too old to keep (`kb.retain_days`); `None`: never.
    pub(crate) fn kb_retain_cut(&self) -> Option<u64> {
        match self.config().kb.retain_days {
            0 => None,
            days => Some(ptyro_core::now_ms().saturating_sub(u64::from(days) * 86_400_000)),
        }
    }

    /// One retention pass: sessions and shells' days older than `kb.retain_days` go, with no
    /// tombstones, and what commands printed that is older than `kb.shell_output_days`.
    /// Returns how many items went.
    pub(crate) async fn kb_retention_pass(&self) -> usize {
        if !self.config().kb.enabled {
            return 0;
        }
        let Ok(db) = self.kb_db() else { return 0 };
        self.kb_shell_output_pass(&db).await;
        let Some(cut) = self.kb_retain_cut() else { return 0 };
        match db.expire(vec![ItemKind::Session, ItemKind::Shell], cut).await {
            Ok(n) => {
                if n > 0 {
                    self.kb_changed([ItemKind::Session, ItemKind::Shell]);
                    tracing::info!(
                        "knowledge base: {n} item(s) older than {} day(s) deleted",
                        self.config().kb.retain_days
                    );
                }
                n
            }
            Err(e) => {
                tracing::warn!("knowledge base retention: {}", store_err(e).message);
                0
            }
        }
    }

    /// The daily pass: at the daemon's start, once a day, and at once when `kb.retain_days`
    /// or `kb.shell_output_days` changes (a setting that waited a day to act would look
    /// broken).
    pub(crate) fn start_kb_retention(self: &std::sync::Arc<Self>) {
        let me = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            let mut events = me.events.subscribe();
            loop {
                let days = |me: &Self| {
                    let kb = me.config().kb;
                    (kb.retain_days, kb.shell_output_days)
                };
                let was = days(&me);
                me.kb_retention_pass().await;
                let day = tokio::time::sleep(std::time::Duration::from_secs(24 * 3600));
                tokio::pin!(day);
                loop {
                    tokio::select! {
                        () = &mut day => break,
                        e = events.recv() => match e {
                            Ok(ptyro_protocol::DomainEvent::ConfigReloaded { .. })
                                if days(&me) != was => break,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            _ => {}
                        },
                    }
                }
            }
        });
    }
}
