//! What is offered at a prompt (SPEC §25.9, `kb.inject`; off unless you switch it on): the
//! answer to an agent's prompt hook, with pointers to what the knowledge base holds about
//! the prompt. The plain parts (which prompt asks for anything, the search's words, the text
//! and its cap) are `ptyro_core::kb::offer`; here are the gates that need the daemon.
//!
//! **Silence is the rule.** Nothing is offered unless every one of these holds:
//! - `kb.inject` and `kb.enabled` are on, and the agent has the tools that open a pointer
//!   (`kb.tools`, and not `[agents.X] kb = false`);
//! - the session is an agent's own (not a chat's engine), is not private, and its project
//!   has not kept itself out (`[knowledge] enabled = false`);
//! - the hook is one whose answer the agent adds to the prompt (`Adapter::hook_prompt`:
//!   Claude Code and Codex), and the prompt asks for something (`offer::telling`);
//! - a piece holds enough of the prompt's own words, and was found by meaning as well when
//!   meaning search is on ([`Daemon::kb_pointers`]), behind the caller's wall
//!   ([`Daemon::kb_session_scope`]), never of its own session, never one it was offered
//!   before;
//! - the search answered within [`offer::CUT_OFF_MS`].
//!
//! **Never out of sight.** Every offer is in the session's events (`kb_offer`, the
//! addresses) and in [`Daemon::kb_offered`], which `ptyro kb` and the Libraries show.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::chats::Query;
use ptyro_core::kb::offer::{self, KbOffer, KbPointer};
use ptyro_core::kb::search::{KbHit, KbSearch};
use ptyro_core::session::SessionKind;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, now_ms};
use serde_json::{Value, json};

use super::search::KbScope;
use crate::daemon::Daemon;
use crate::session::Session;

/// How many offers are remembered for the status.
const LOG: usize = 20;
/// How many hits are looked through for three that pass.
const LOOKED_AT: u32 = 12;

/// What was offered since the daemon started.
#[derive(Default)]
pub(crate) struct Offers {
    /// The items each session (its id) was offered: none is offered to it twice.
    seen: HashMap<String, HashSet<String>>,
    /// The last [`LOG`] offers, newest last, each with its pointers' items.
    log: VecDeque<(KbOffer, Vec<String>)>,
}

impl Daemon {
    /// **What a prompt is offered**: the hits that pass the floor, best first, at most
    /// [`offer::MAX`]. `scope` is the wall, `here` the project the prompt was given in, and
    /// `seen` the items not to offer again. Empty: nothing is good enough, which is the
    /// usual answer.
    ///
    /// The floor: a piece must hold at least two of the prompt's telling words *and* be
    /// found by meaning too; where meaning search is off or its model isn't here, at least
    /// three of them. One pointer an item.
    pub(crate) async fn kb_pointers(
        &self,
        prompt: &str,
        scope: Option<&KbScope>,
        here: Option<String>,
        seen: &HashSet<String>,
    ) -> Result<Vec<KbHit>, Error> {
        let Some(words) = offer::telling(prompt) else { return Ok(Vec::new()) };
        let ask = KbSearch {
            kinds: offer::KINDS.to_vec(),
            limit: LOOKED_AT,
            here,
            ..KbSearch::default()
        };
        // With meaning: found both ways. (Asked first by words alone when the model can't
        // be asked, so that the stricter count is in the search itself.)
        let probe = Query { words: words.clone(), ..Query::default() };
        let (vector, _) = self.meaning_vector(&probe).await?;
        let need = match vector {
            Some(_) => offer::NEED_WITH_MEANING,
            None => offer::NEED_BY_WORDS,
        };
        let Some(fts) = offer::query(&words, need) else { return Ok(Vec::new()) };
        let both = vector.is_some();
        let hits = self.kb_search_for(Query { fts, words, ..Query::default() }, ask, scope).await?;
        let mut items = HashSet::new();
        Ok(hits
            .hits
            .into_iter()
            .filter(|h| if both { h.how == "both" } else { h.how == "words" })
            .filter(|h| !seen.contains(&h.item) && items.insert(h.item.clone()))
            .take(offer::MAX)
            .collect())
    }

    /// A hook of the agent in `s` arrived: what it prints for the agent, when this is a
    /// prompt and there is something to offer for it (the module's gates). `None` otherwise,
    /// and whenever anything goes wrong: a prompt is never held up or failed by this.
    pub(crate) async fn kb_offer(
        self: &Arc<Self>,
        s: &Arc<Session>,
        adapter: &str,
        event: &str,
        payload: &Value,
    ) -> Option<String> {
        let config = self.config();
        if !(config.kb.enabled && config.kb.inject && config.kb.tools) {
            return None;
        }
        let a = ptyro_agents::adapter(adapter);
        let prompt = a.hook_prompt(event, payload)?;
        let (place, name, kind) = {
            let m = s.meta.locked();
            (m.place.clone(), m.name.clone(), m.kind)
        };
        if kind != SessionKind::Agent
            || place.chat.is_some()
            || place.private
            || self.kb_agent_off(place.agent.as_deref().or(Some(adapter)))
            || offer::telling(&prompt).is_none_or(|w| w.len() < offer::NEED_WITH_MEANING)
            || self.kb_project_off(place.project.as_deref(), place.worktree.as_deref())
        {
            return None;
        }
        let id = s.id.full();
        let scope = self.kb_session_scope(s);
        let seen = self.kb.offers.locked().seen.get(&id).cloned().unwrap_or_default();
        let search = self.kb_pointers(&prompt, Some(&scope), place.project.clone(), &seen);
        let hits =
            match tokio::time::timeout(Duration::from_millis(offer::CUT_OFF_MS), search).await {
                Ok(Ok(hits)) => hits,
                Ok(Err(e)) => {
                    tracing::debug!("nothing offered to {name}: {}", e.message);
                    return None;
                }
                Err(_) => {
                    tracing::debug!("nothing offered to {name}: the search took too long");
                    return None;
                }
            };
        let pointers: Vec<KbPointer> = hits
            .iter()
            .map(|h| KbPointer {
                addr: h.addr.clone(),
                kind: h.kind,
                title: h.title.clone().or_else(|| h.heading.clone()),
                at_ms: h.at_ms,
                project: self
                    .project_by_id(h.project.as_deref().unwrap_or_default())
                    .map(|p| p.slug),
            })
            .collect();
        let (text, held) = offer::render(&pointers);
        let stdout = a.hook_context(event, &text).filter(|_| held > 0)?;
        let pointers: Vec<KbPointer> = pointers.into_iter().take(held).collect();
        let items: Vec<String> = hits.into_iter().take(held).map(|h| h.item).collect();
        s.record_event(
            "kb_offer",
            json!({ "addrs": pointers.iter().map(|p| &p.addr).collect::<Vec<_>>(), "bytes": text.len() }),
        );
        let mut all = self.kb.offers.locked();
        // (A session that is gone is offered nothing more: its list goes too.)
        all.seen.retain(|session, _| {
            session == &id || session.parse().ok().and_then(|i| self.session(i)).is_some()
        });
        all.seen.entry(id.clone()).or_default().extend(items.iter().cloned());
        all.log.push_back((KbOffer { at_ms: now_ms(), session: id, name, pointers }, items));
        while all.log.len() > LOG {
            all.log.pop_front();
        }
        Some(stdout)
    }

    /// What was offered at prompts since the daemon started, newest first (the last
    /// [`LOG`]): the trace. A pointer to what was forgotten since is left out.
    pub(crate) async fn kb_offered(&self) -> Vec<KbOffer> {
        let log: Vec<(KbOffer, Vec<String>)> =
            self.kb.offers.locked().log.iter().rev().cloned().collect();
        if log.is_empty() {
            return Vec::new();
        }
        let Ok(db) = self.kb_db() else { return Vec::new() };
        let Ok(kept) = db.ids().await else { return Vec::new() };
        let kept: HashSet<String> = kept.into_iter().collect();
        log.into_iter()
            .filter_map(|(mut o, items)| {
                let mut items = items.iter();
                o.pointers.retain(|_| items.next().is_some_and(|i| kept.contains(i)));
                (!o.pointers.is_empty()).then_some(o)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
