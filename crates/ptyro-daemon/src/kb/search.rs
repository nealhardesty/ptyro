//! The knowledge base's search in the daemon (SPEC §25.8): one search over `kb.db` and
//! `chat.db`, for `ptyro recall`, the Library, the web and the agents' `recall` tool.
//!
//! [`Daemon::kb_search`] parses the query, asks each file for its two lists (by words, and by
//! meaning when that is on and its model is here), merges the four by reciprocal-rank fusion
//! and ranks what they found (`ptyro_core::kb::search::rank`). Chats stay in `chat.db`: a hit
//! in one keeps its chat address.
//!
//! **The wall** (§25.9) is a [`KbScope`]: what a caller that is a model may see. A client's
//! search passes none: you see everything.

use std::collections::HashMap;

use ptyro_core::chats::{self, Query};
use ptyro_core::kb::search::{HitKind, KbDoc, KbHit, KbHits, KbSearch, LIMIT, MAX_LIMIT, rank};
use ptyro_core::kb::{ItemKind, KbItem, Writer};
use ptyro_core::{Error, lookup, now_ms};
use ptyro_meaning::{Found, MODEL_ID};
use ptyro_store::chat::{RawHit, SearchFilter};
use ptyro_store::kb::{ItemFilter, KbDb, KbFilter};

use super::store_err;
use crate::daemon::Daemon;

/// Each list is brought up to this many times the answer's length before they are merged
/// (§25.8): what the ranking moves up, and what two hits an item leaves out, has to be there.
const LISTS: usize = 4;

/// The wall between profiles (SPEC §25.9): what a caller that is a model may see, and what
/// is never shown to it because it is its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KbScope {
    /// Only this profile's items and those of no profile.
    pub profile: Option<String>,
    /// Only this workspace's items (its id).
    pub workspace: Option<String>,
    /// The caller's own ptyro session: what it is saying now is not found for it.
    pub session: Option<String>,
    /// The caller's own chat.
    pub chat: Option<String>,
}

impl KbScope {
    /// What a caller of `profile` in `workspace` may see under `scope` (`kb.scope`):
    /// `profile` (its profile's and nobody's), `workspace` (of those, its workspace's) or
    /// `all`.
    pub(crate) fn of(scope: &str, profile: &str, workspace: Option<&str>) -> Self {
        Self {
            profile: (scope != "all").then(|| profile.to_owned()),
            workspace: (scope == "workspace").then(|| workspace.map(str::to_owned)).flatten(),
            session: None,
            chat: None,
        }
    }

    fn sees(&self, i: &KbItem) -> bool {
        self.profile.as_ref().is_none_or(|p| i.profile.as_ref().is_none_or(|ip| ip == p))
            && self.workspace.as_ref().is_none_or(|w| i.workspace.as_ref() == Some(w))
            && (self.session.is_none() || i.session != self.session)
    }
}

/// What a history is narrowed to beyond its kinds and project (`KbRecent`, 1.55): one ptyro
/// session's items, one worktree's, and nothing older than a time.
#[derive(Debug, Clone, Default)]
pub(crate) struct Of {
    pub session: Option<String>,
    pub worktree: Option<String>,
    pub since_ms: Option<u64>,
}

/// What a hit is in the lists: the file, the item (or chat), the chunk (or message).
type Key = (bool, String, String);

fn key(h: &KbHit) -> Key {
    let part = match (&h.idx, &h.node) {
        (Some(i), _) => i.to_string(),
        (None, Some(n)) => n.clone(),
        (None, None) => String::new(),
    };
    (h.kind == HitKind::Chat, h.item.clone(), part)
}

/// A chat's hit as the knowledge base's: its address is the chat's (`c:7f3k/01JB…`).
fn chat_hit(h: RawHit, ids: &[String], how: &str) -> KbHit {
    let handle = lookup::handle(&h.chat.id, ids.iter().map(String::as_str));
    KbHit {
        addr: match &h.node {
            Some(n) => format!("c:{handle}/{n}"),
            None => format!("c:{handle}"),
        },
        item: h.chat.id,
        idx: None,
        node: h.node,
        uri: None,
        kind: HitKind::Chat,
        part: h.kind,
        title: h.chat.title,
        heading: None,
        snippet: h.snippet,
        at_ms: h.at_ms,
        project: None,
        project_name: None,
        workspace_name: None,
        workspace: Some(h.chat.workspace),
        profile: Some(h.chat.profile),
        agent: Some(h.chat.agent),
        writer: None,
        machine: None,
        how: how.to_owned(),
        score: 0.0,
    }
}

impl Daemon {
    /// **Search the knowledge base and the chats** (SPEC §25.8), on this machine. `scope` is
    /// the wall for a caller that is a model (an agent's tool); `None` for you.
    ///
    /// The answer says when it was by words alone for want of the model
    /// ([`KbHits::words_only`]). Errors: the knowledge base is off or can't be opened, the
    /// query asks nothing or names what there isn't, `by:meaning` while it can't be done.
    pub(crate) async fn kb_search(
        &self,
        ask: KbSearch,
        scope: Option<&KbScope>,
    ) -> Result<KbHits, Error> {
        let db = self.kb_db()?;
        let now = now_ms();
        let q = chats::parse_kb_query(&ask.query, now).map_err(Error::invalid)?;
        if q.fts.is_empty() {
            return Err(Error::invalid("say what to look for (words, \"a phrase\", prefix*)"));
        }
        // A one- or two-letter prefix matches half the index: wait for more (as chats do).
        if q.fts
            .split_whitespace()
            .any(|t| t.ends_with('*') && t.trim_matches(['"', '*']).chars().count() < 3)
        {
            return Err(Error::invalid("a prefix needs at least 3 letters (`migr*`)"));
        }
        if let Some(m) = &q.machine {
            return Err(Error::invalid(format!(
                "machine:{m}: this search looks on this machine only"
            )));
        }
        // Kinds asked both ways narrow each other.
        let kinds: Vec<HitKind> = match (ask.kinds.is_empty(), q.kinds.is_empty()) {
            (true, _) => q.kinds.clone(),
            (false, true) => ask.kinds.clone(),
            (false, false) => {
                let both: Vec<HitKind> =
                    q.kinds.iter().copied().filter(|k| ask.kinds.contains(k)).collect();
                if both.is_empty() {
                    return Ok(KbHits::default());
                }
                both
            }
        };
        let project = match ask.project.as_deref().or(q.project.as_deref()) {
            Some(p) => Some(self.find_project(p)?.id),
            None => None,
        };
        let root = match &q.root {
            Some(r) => Some(
                db.root(r.clone())
                    .await
                    .map_err(store_err)?
                    .ok_or_else(|| Error::missing("root", r))?
                    .id,
            ),
            None => None,
        };
        let workspace = match &q.workspace {
            Some(w) => Some(self.find_workspace(w)?.id),
            None => None,
        };
        let since_ms = ask.since_ms.max(q.since_ms);
        let before_ms = match (ask.before_ms, q.before_ms) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let filter = KbFilter {
            kinds: kinds.clone(),
            project: project.clone(),
            root: root.clone(),
            profile: scope.and_then(|s| s.profile.clone()),
            workspace: scope.and_then(|s| s.workspace.clone()).or(workspace.clone()),
            agent: q.agent.clone(),
            writer: q.wrote.as_deref().map(|w| match w {
                "you" => Writer::User,
                _ => Writer::Agent,
            }),
            file: q.file.clone(),
            since_ms,
            before_ms,
            not_session: scope.and_then(|s| s.session.clone()),
        };
        let limit = match ask.limit {
            0 => LIMIT,
            n => n.min(MAX_LIMIT),
        } as usize;
        let list = LISTS * limit;
        let (vector, words_only) = self.meaning_vector(&q).await?;

        // `kb.db`'s two lists.
        let kb_words = match q.by.as_deref() {
            Some("meaning") => Vec::new(),
            _ => db.search_words(q.fts.clone(), filter.clone(), list).await.map_err(store_err)?,
        };
        let kb_near = match &vector {
            Some(v) => db
                .search_meaning(
                    v.clone(),
                    MODEL_ID.into(),
                    q.not.clone(),
                    ptyro_meaning::MIN_SCORE,
                    filter,
                    list,
                )
                .await
                .map_err(store_err)?,
            None => Vec::new(),
        };
        // `chat.db`'s two, unless the query asks what no chat has.
        let chats_too = (kinds.is_empty() || kinds.contains(&HitKind::Chat))
            && project.is_none()
            && root.is_none()
            && q.file.is_none()
            && q.wrote.is_none();
        let (chat_words, chat_near) = match self.meaning_chats() {
            Some(chat_db) if chats_too => {
                self.kb_chat_lists(
                    &chat_db,
                    &q,
                    vector,
                    scope,
                    workspace,
                    (since_ms, before_ms),
                    list,
                )
                .await?
            }
            _ => (Vec::new(), Vec::new()),
        };

        // Merged by rank (the scores of two files can't be compared), then ranked.
        let lists = [
            (Found::Words, &kb_words),
            (Found::Meaning, &kb_near),
            (Found::Words, &chat_words),
            (Found::Meaning, &chat_near),
        ];
        let keys: Vec<(Found, Vec<Key>)> =
            lists.iter().map(|(how, l)| (*how, l.iter().map(key).collect())).collect();
        let order = ptyro_meaning::fuse_lists(
            &keys.iter().map(|(how, l)| (*how, l.as_slice())).collect::<Vec<_>>(),
        );
        // Found both ways: the hit by words, whose snippet shows them.
        let mut by_key: HashMap<Key, KbHit> = HashMap::new();
        for h in [kb_near, chat_near, kb_words, chat_words].into_iter().flatten() {
            by_key.insert(key(&h), h);
        }
        let fused: Vec<KbHit> = order
            .into_iter()
            .filter_map(|(k, how, score)| {
                let mut h = by_key.remove(&k)?;
                h.how = how.as_str().to_owned();
                h.score = score;
                Some(h)
            })
            .collect();
        let mut hits = rank(fused, now, ask.here.as_deref(), limit);
        // Addresses as handles (`k:7f3k/12`).
        if hits.iter().any(|h| h.kind != HitKind::Chat) {
            let ids = db.ids().await.map_err(store_err)?;
            for h in hits.iter_mut().filter(|h| h.kind != HitKind::Chat) {
                let handle = lookup::handle(&h.item, ids.iter().map(String::as_str));
                h.addr = format!("k:{handle}/{}", h.idx.unwrap_or(0));
            }
        }
        Ok(KbHits { hits, words_only })
    }

    /// `chat.db`'s two lists as hits, under the same wall and the same times.
    #[allow(clippy::too_many_arguments)]
    async fn kb_chat_lists(
        &self,
        db: &ptyro_store::chat::ChatDb,
        q: &Query,
        vector: Option<Vec<f32>>,
        scope: Option<&KbScope>,
        workspace: Option<String>,
        (since_ms, before_ms): (Option<u64>, Option<u64>),
        list: usize,
    ) -> Result<(Vec<KbHit>, Vec<KbHit>), Error> {
        let filter = SearchFilter {
            fts: q.fts.clone(),
            words: q.words.clone(),
            not_words: q.not_words.clone(),
            chat: None,
            workspace: scope.and_then(|s| s.workspace.clone()).or(workspace),
            agent: q.agent.clone(),
            profile: scope.and_then(|s| s.profile.clone()),
            since_ms,
            pinned: false,
            archived: false,
            limit: list,
        };
        let (words, near) = self.chat_lists(db, filter, q, vector).await?;
        let ids = db.ids().await.map_err(|e| Error::internal(format!("chat.db: {e}")))?;
        let own = scope.and_then(|s| s.chat.as_deref());
        let hits = |raw: Vec<RawHit>, how: &str| -> Vec<KbHit> {
            raw.into_iter()
                .filter(|h| {
                    before_ms.is_none_or(|b| h.at_ms < b) && Some(h.chat.id.as_str()) != own
                })
                .map(|h| chat_hit(h, &ids, how))
                .collect()
        };
        Ok((hits(words, "words"), hits(near, "meaning")))
    }

    /// The item an address names (`k:7f3k`, `k:7f3k/12`, an id), and the chunk if it names
    /// one. Never a forgotten item; never one behind the wall.
    pub(super) async fn kb_find(
        &self,
        db: &KbDb,
        addr: &str,
        scope: Option<&KbScope>,
    ) -> Result<(KbItem, Option<u32>), Error> {
        let typed = lookup::key(addr);
        let typed = typed.strip_prefix("k:").unwrap_or(&typed);
        let (id, idx) = match typed.split_once('/') {
            Some((id, n)) => {
                let n = n.parse::<u32>().map_err(|_| {
                    Error::invalid(format!(
                        "{addr}: after the / comes a piece's number (`k:7f3k/12`)"
                    ))
                })?;
                (id, Some(n))
            }
            None => (typed, None),
        };
        let ids = db.ids().await.map_err(store_err)?;
        let found: Vec<&String> = ids.iter().filter(|i| lookup::id_matches(i, id)).collect();
        let missing = || Error::missing("item", addr);
        let id = lookup::one("item", addr, found, |i| format!("k:{i}"))?.ok_or_else(missing)?;
        let item = db.item(id.clone()).await.map_err(store_err)?.ok_or_else(missing)?;
        if item.forgotten_ms.is_some() || scope.is_some_and(|s| !s.sees(&item)) {
            return Err(missing());
        }
        Ok((item, idx))
    }

    /// **Open an address** (SPEC §25.14's `KbRead`): the chunk it names with `before` and
    /// `after` of its neighbours, or the item from its start. A chat's address is the
    /// chats' to open (`ChatRead`).
    pub(crate) async fn kb_read(
        &self,
        addr: &str,
        before: u32,
        after: u32,
        scope: Option<&KbScope>,
    ) -> Result<KbDoc, Error> {
        let db = self.kb_db()?;
        let (item, idx) = self.kb_find(&db, addr, scope).await?;
        // A chunk the item no longer has (a file read again): the item from its start.
        let stale = idx.is_some_and(|i| i >= item.chunks);
        let at = idx.filter(|_| !stale);
        let from = at.unwrap_or(0).saturating_sub(before);
        let limit = (at.unwrap_or(0) - from).saturating_add(after).saturating_add(1);
        let chunks = db.chunks(item.id.clone(), from, limit).await.map_err(store_err)?;
        Ok(KbDoc { item, chunks, at, stale })
    }

    /// **The history** (SPEC §25.14's `KbRecent`): items newest first, by kind and project
    /// and what [`Of`] names, `before_ms` for the next page.
    pub(crate) async fn kb_recent(
        &self,
        kinds: Vec<ItemKind>,
        project: Option<String>,
        of: Of,
        before_ms: Option<u64>,
        limit: u32,
        scope: Option<&KbScope>,
    ) -> Result<Vec<KbItem>, Error> {
        let db = self.kb_db()?;
        let project = match project {
            Some(p) => Some(self.find_project(&p)?.id),
            None => None,
        };
        let limit = match limit {
            0 => LIMIT,
            n => n.min(MAX_LIMIT),
        } as usize;
        // (The wall's narrower parts are checked here: ask for more than the page.)
        let narrowed = scope.is_some_and(|s| s.workspace.is_some() || s.session.is_some());
        let items = db
            .list(ItemFilter {
                kinds,
                project,
                profile: scope.and_then(|s| s.profile.clone()),
                session: of.session,
                worktree: of.worktree,
                since_ms: of.since_ms,
                before_ms,
                limit: if narrowed { 0 } else { limit },
                ..ItemFilter::default()
            })
            .await
            .map_err(store_err)?;
        Ok(items.into_iter().filter(|i| scope.is_none_or(|s| s.sees(i))).take(limit).collect())
    }
}

#[cfg(test)]
pub(crate) mod tests;
