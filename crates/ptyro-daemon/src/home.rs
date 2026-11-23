//! Home (SPEC §26): the daemon's one look at everything. It gathers what it already knows
//! (every session, project and worktree, here and on linked machines; each profile's usage;
//! what the knowledge base remembers of each project) and `ptyro_core::home::build` ranks it.
//!
//! Three things are kept for it:
//! - **what was put away** (`x` on a thing left hanging): its key and how it looked then, in
//!   the store's `kv` (`home:dismissed`), so it stays away until it changes;
//! - **the last sign of you**: a person's client typing or looking at a session, or home asked
//!   for by hand (`Request::Home.here`). After
//!   [`AWAY_AFTER_MS`](ptyro_core::home::AWAY_AFTER_MS) without one, the next is coming back,
//!   and home says what happened in between. Memory only: a restarted daemon starts over;
//! - nothing else. The view is built when it is asked for; there is no cache to go stale.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ptyro_core::home::{
    self, Away, Hot, Inputs, Last, LeftOff, Memory, Planned, Seen, Tail, Trail, TrailEntry,
};
use ptyro_core::kb::{ChunkKind, ItemKind, KbItem};
use ptyro_core::status::AttentionKind;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, lookup, now_ms};
use ptyro_protocol::{ClientKind, HomeDismiss, Reply};
use ptyro_store::kb::{ItemFilter, KbDb};

use crate::daemon::Daemon;

/// Where the things put away are kept.
const DISMISSED: &str = "home:dismissed";
/// How many lines a trail has.
const TRAIL: usize = 14;
/// How many hot files a trail names.
const HOT: usize = 5;
/// The longest line of what was said.
const SAID: usize = 240;

/// What the daemon keeps for home.
#[derive(Debug, Default)]
pub(crate) struct HomeState {
    /// The last sign of you (0: none yet).
    pub(crate) seen_ms: u64,
    /// The last time you were away: (the last sign before, when you came back).
    away: Option<(u64, u64)>,
    /// What was put away; `None` until it is read from the store.
    dismissed: Option<BTreeMap<String, String>>,
}

impl Daemon {
    /// A sign of you: a person's client typed, or looked at a session.
    pub(crate) fn human_seen(&self, kind: Option<ClientKind>) {
        if !matches!(kind, Some(ClientKind::Tui | ClientKind::Web | ClientKind::Mobile)) {
            return;
        }
        self.human_here();
    }

    /// A person is looking (`Request::Home.here`, or [`human_seen`](Self::human_seen)).
    pub(crate) fn human_here(&self) {
        let now = now_ms();
        let mut h = self.home.locked();
        if h.seen_ms > 0 && now.saturating_sub(h.seen_ms) >= home::AWAY_AFTER_MS {
            h.away = Some((h.seen_ms, now));
        }
        h.seen_ms = now;
    }

    async fn home_dismissed(&self) -> BTreeMap<String, String> {
        if let Some(d) = self.home.locked().dismissed.clone() {
            return d;
        }
        let read = match &self.store {
            Some(store) => store
                .kv_get(DISMISSED.into())
                .await
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            None => BTreeMap::new(),
        };
        self.home.locked().dismissed.get_or_insert(read).clone()
    }

    async fn home_put_away(&self, change: impl FnOnce(&mut BTreeMap<String, String>)) {
        let mut d = self.home_dismissed().await;
        change(&mut d);
        self.home.locked().dismissed = Some(d.clone());
        if let Some(store) = &self.store
            && let Err(e) =
                store.kv_set(DISMISSED.into(), serde_json::to_value(&d).unwrap_or_default()).await
        {
            tracing::warn!("home: what was put away isn't saved: {e}");
        }
    }

    /// Every session, project, worktree, workspace and usage meter: this machine's and every
    /// linked machine's that isn't hidden here.
    fn home_world(&self) -> Inputs {
        let mut inp = Inputs { now_ms: now_ms(), ..Inputs::default() };
        for s in self.sessions().iter() {
            // The whole last reply is here; the wire carries only its start.
            let asks = s.meta.locked().last_reply.as_deref().and_then(home::question);
            inp.sessions.push(Seen { info: self.info(s), asks });
        }
        {
            let reg = self.registry.locked();
            inp.projects = reg.projects.clone();
            inp.worktrees = reg.worktrees.clone();
            inp.workspaces = reg.workspaces.clone();
        }
        inp.usage = self.usage.locked().values().cloned().collect();
        let links = self.links.locked();
        for l in links.values() {
            let m = crate::machines::filter_hidden(l.mirror.clone(), &l.record.hidden);
            inp.sessions.extend(m.sessions.into_iter().map(|info| {
                let asks = info.last_reply.as_deref().and_then(home::question);
                Seen { info, asks }
            }));
            inp.projects.extend(m.projects);
            inp.worktrees.extend(m.worktrees);
            inp.workspaces.extend(m.workspaces);
            inp.usage.extend(m.usage);
        }
        inp
    }

    /// What happened between the last sign of you and your coming back.
    async fn home_away(&self, inp: &Inputs, db: Option<&KbDb>) -> Option<Away> {
        let (since_ms, back_ms) = self.home.locked().away?;
        let mut a = Away { since_ms, back_ms, ..Away::default() };
        for s in &inp.sessions {
            let Some(at) = s.info.attention.as_ref().filter(|a| a.since_ms > since_ms) else {
                continue;
            };
            match at.kind {
                AttentionKind::Blocked => a.asked += 1,
                AttentionKind::Errored => a.failed += 1,
                AttentionKind::Exited
                    if !matches!(
                        s.info.state,
                        ptyro_core::session::ProcessState::Exited { code: Some(0), .. }
                    ) =>
                {
                    a.failed += 1
                }
                AttentionKind::Done | AttentionKind::Idle | AttentionKind::Exited => {
                    a.finished += 1
                }
                AttentionKind::Notification => {}
            }
        }
        if let Some(db) = db {
            let filter = ItemFilter {
                kinds: vec![ItemKind::Commit],
                since_ms: Some(since_ms),
                before_ms: Some(back_ms),
                limit: 500,
                ..ItemFilter::default()
            };
            a.commits = db.list(filter).await.map(|c| c.len() as u32).unwrap_or(0);
        }
        Some(a)
    }

    /// What the knowledge base remembers of this machine's projects.
    async fn home_tails(&self, db: &KbDb, inp: &Inputs) -> Vec<Tail> {
        let projects: Vec<String> =
            inp.projects.iter().filter(|p| p.machine.is_none()).map(|p| p.id.clone()).collect();
        let tails = match db.home_tails(projects, inp.now_ms).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("home: the knowledge base didn't answer: {e}");
                return Vec::new();
            }
        };
        let ids = db.ids().await.unwrap_or_default();
        let addr = |id: &str| format!("k:{}", lookup::handle(id, ids.iter().map(String::as_str)));
        tails
            .into_iter()
            .map(|t| Tail {
                project: t.project,
                addr: t.item.as_ref().map(|i| addr(&i.id)),
                session: t.item.as_ref().and_then(|i| i.session.clone()),
                agent: t.item.as_ref().and_then(|i| i.agent.clone()),
                at_ms: t.at_ms,
                you: t.you.as_deref().map(|y| home::clean(y, SAID)),
                reply: t.reply.as_deref().map(|r| home::clean(r, SAID)),
                last: match t.last {
                    Some(ChunkKind::User) => Some(Last::You),
                    Some(ChunkKind::Assistant) => Some(Last::Reply),
                    _ => None,
                },
                asks: t.reply_end.as_deref().and_then(home::question),
                // (A week of nothing is no week: the column is only drawn when one has something.)
                week: if t.week.iter().any(|n| *n > 0) { t.week } else { Vec::new() },
                planned: t.planned.map(|(item, text)| Planned {
                    addr: addr(&item.id),
                    text: home::clean(&text, SAID),
                    at_ms: item.ended_ms.or(item.started_ms).or(item.ingested_ms).unwrap_or(0),
                }),
            })
            .collect()
    }

    /// A project unfolded: where it was left off, its newest sessions, commits and notes, and
    /// the files they touched most.
    async fn home_trail(&self, db: &KbDb, key: &str, tail: Option<&Tail>, inp: &Inputs) -> Trail {
        let mut trail = Trail { key: key.to_owned(), ..Trail::default() };
        if let Some(t) = tail.filter(|t| t.addr.is_some()) {
            let listed = |id: &String| inp.sessions.iter().any(|s| s.info.id.full() == *id);
            trail.left_off = Some(LeftOff {
                you: t.you.clone(),
                reply: t.reply.clone(),
                agent: t.agent.clone(),
                at_ms: t.at_ms,
                addr: t.addr.clone(),
                session: t.session.clone().filter(listed),
            });
        }
        let items = match db.home_trail(key.to_owned(), 3 * TRAIL).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("home: the knowledge base didn't answer: {e}");
                return trail;
            }
        };
        let ids = db.ids().await.unwrap_or_default();
        let root = inp.projects.iter().find(|p| p.id == key).map(|p| p.root.clone());
        let mut hot: HashMap<String, u32> = HashMap::new();
        for t in &items {
            for f in &t.item.files {
                let rel = root
                    .as_deref()
                    .and_then(|r| std::path::Path::new(f).strip_prefix(r).ok())
                    .map_or_else(|| f.clone(), |p| p.to_string_lossy().into_owned());
                *hot.entry(rel).or_default() += 1;
            }
        }
        let mut hot: Vec<Hot> = hot
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(path, count)| Hot { path, count })
            .collect();
        hot.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
        hot.truncate(HOT);
        trail.hot = hot;
        trail.entries = items
            .into_iter()
            .take(TRAIL)
            .map(|t| {
                let i: &KbItem = &t.item;
                let at_ms = i.ended_ms.or(i.started_ms).or(i.ingested_ms).unwrap_or(0);
                let mut detail: Vec<String> = Vec::new();
                match i.kind {
                    ItemKind::Session => {
                        detail.extend(i.agent.clone());
                        if t.turns > 0 {
                            detail.push(format!(
                                "{} turn{}",
                                t.turns,
                                if t.turns == 1 { "" } else { "s" }
                            ));
                        }
                        if !i.files.is_empty() {
                            let n = i.files.len();
                            detail.push(format!("{n} file{}", if n == 1 { "" } else { "s" }));
                        }
                    }
                    ItemKind::Commit => {
                        let sha = i.uri.rsplit('/').next().unwrap_or("");
                        detail.push(sha.chars().take(7).collect());
                    }
                    _ => detail.push(i.writer.to_string()),
                }
                TrailEntry {
                    kind: i.kind.to_string(),
                    title: home::clean(i.title.as_deref().unwrap_or("(untitled)"), SAID),
                    detail: detail.join(" · "),
                    at_ms,
                    addr: format!("k:{}", lookup::handle(&i.id, ids.iter().map(String::as_str))),
                    session: i
                        .session
                        .clone()
                        .filter(|id| inp.sessions.iter().any(|s| s.info.id.full() == *id)),
                }
            })
            .collect();
        trail
    }

    /// `Request::Home`.
    pub(crate) async fn home(
        self: &Arc<Self>,
        trail: Option<String>,
        dismiss: Option<HomeDismiss>,
        restore: bool,
        caller: crate::guard::Caller,
    ) -> Result<Reply, Error> {
        if restore {
            self.home_put_away(BTreeMap::clear).await;
        }
        if let Some(d) = dismiss {
            let known = self.home_view(None, caller).await;
            let thing = known
                .rows
                .iter()
                .flat_map(|r| &r.things)
                .find(|t| t.key == d.key)
                .ok_or_else(|| Error::missing("thing", &d.key))?;
            if !thing.next.dismissable() {
                return Err(Error::invalid(format!(
                    "\"{}\" isn't something to put away: only what was left hanging is",
                    thing.next.verb()
                )));
            }
            self.home_put_away(|m| {
                m.insert(d.key, d.stamp);
            })
            .await;
        }
        Ok(Reply::Home { home: Box::new(self.home_view(trail, caller).await) })
    }

    async fn home_view(
        self: &Arc<Self>,
        trail: Option<String>,
        caller: crate::guard::Caller,
    ) -> home::HomeView {
        let mut inp = self.home_world();
        inp.dismissed = self.home_dismissed().await;
        // An agent asking (its terminal, its tools) gets what the lists already tell it, and
        // nothing of what was said in other sessions (§25.9).
        let db = if self.kb_agent(caller).is_some() {
            inp.memory = Memory::Withheld;
            None
        } else {
            let db = self.kb_db().ok();
            inp.memory = if db.is_some() { Memory::On } else { Memory::Off };
            db
        };
        if let Some(db) = &db {
            inp.tails = self.home_tails(db, &inp).await;
        }
        inp.away = self.home_away(&inp, db.as_ref()).await;
        let mut view = home::build(&inp);
        view.at_start = self.config().ui.home_at_start;
        if let Some(word) = trail
            && let Some(row) = view.find(&word)
        {
            let key = row.key.clone();
            view.trail = Some(match &db {
                Some(db) if row.project.is_some() && row.machine.is_none() => {
                    let tail = inp.tails.iter().find(|t| t.project == key);
                    self.home_trail(db, &key, tail, &inp).await
                }
                _ => Trail { key, ..Trail::default() },
            });
        }
        view
    }
}

#[cfg(test)]
mod tests;
