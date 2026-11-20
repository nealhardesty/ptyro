//! What the agents kept before ptyro did (SPEC §25.6.1, `ptyro ingest --history`): every
//! conversation in each profile's Claude Code and Codex folders that the knowledge base
//! doesn't hold yet. It is how a session that ran before ptyro was installed, or that someone
//! started by hand in a shell, comes to be found.
//!
//! **Counted before anything is read** ([`Daemon::kb_history`]): the folders are listed, and
//! each conversation is one of kept already (or forgotten, or private), a chat's, too old
//! (`--since`, `kb.retain_days`), or new. The new ones are queued behind everything else
//! ([`Daemon::kb_enqueue_later`]) and read one by one ([`Daemon::kb_history_read`]) through
//! the gate and the one way in, as items written by `import`, under the profile whose folder
//! held them.
//!
//! **Never**: a conversation whose URI has a tombstone (forgotten, or a private session's: a
//! private session's conversation gets one as soon as its id is known,
//! [`Daemon::kb_keep_out`]); one that served a chat (`chat.db` knows its id) or ran under
//! `<data>/chat/` or `<data>/summaries/`; an agent with `kb = false`; a project that keeps
//! itself out. Only conversation lines are read, by the same readers as a live session's.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use ptyro_core::kb::card::Card;
use ptyro_core::kb::ingest::Stamp;
use ptyro_core::kb::{ItemKind, KbHistoryPlace, KbHistoryPlan, Writer};
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, lookup};

use super::{Facts, Job, Refused, store_err};
use crate::daemon::Daemon;
use crate::session::Session;

/// The agents whose transcripts are read.
const AGENTS: [&str; 2] = ["claude", "codex"];

/// One conversation to read.
#[derive(Debug, Clone)]
pub(crate) struct HistoryJob {
    pub uri: String,
    pub adapter: String,
    pub conversation: String,
    /// The profile whose folder held it.
    pub profile: String,
    /// The agent's config directory under that profile.
    pub home: PathBuf,
    pub path: PathBuf,
}

impl Daemon {
    /// A session the knowledge base keeps nothing of (private, or not recorded) is in a
    /// conversation: that conversation is kept out for good (`KbDb::keep_out`), so that
    /// nothing reads the agent's own copy of it later: not the history's backfill, and not
    /// the catch-up at the daemon's start once the session is gone. A tombstone when nothing
    /// was kept of it; when it was kept before (a private session carried an earlier
    /// conversation on), its item is closed: what it holds stays, and its file, which now
    /// holds the private turns too, is never read again. Once a conversation; only a URI.
    pub(crate) fn kb_keep_out(&self, s: &Session, conversation: Option<String>) {
        let Some(uri) = self.kb_session_uri_in(s, conversation) else { return };
        if self.kb.kept_out.locked().contains(&uri) {
            return;
        }
        // (Also while the knowledge base is off, when its file is there: it may be switched
        // on later, and what was private then must still be.)
        if !self.config().kb.enabled && !self.kb.path.exists() {
            return;
        }
        let origin = self.chats.db().ok().map(|d| d.machine_id.clone());
        let Ok(db) = self.kb.open(origin.as_deref()).cloned() else { return };
        let kept_out = Arc::clone(&self.kb.kept_out);
        tokio::spawn(async move {
            match db.keep_out(uri.clone(), ItemKind::Session).await {
                // (Remembered only once it is written: what failed is tried at the next turn.)
                Ok(()) => {
                    kept_out.locked().insert(uri);
                }
                Err(e) => tracing::warn!("knowledge base: {uri}: {}", store_err(e).message),
            }
        });
    }

    /// Every session the knowledge base keeps nothing of has its conversation kept out: at
    /// the daemon's start (a private session may have run while it was down, or its turn's
    /// end was never seen), and when the settings change (the knowledge base switched on).
    pub(crate) fn kb_keep_out_all(&self) {
        for s in self.sessions() {
            let out = {
                let m = s.meta.locked();
                m.kind == ptyro_core::session::SessionKind::Agent
                    && m.place.chat.is_none()
                    && (m.place.private || m.place.unrecorded)
            };
            if out {
                self.kb_keep_out(&s, None);
            }
        }
    }

    /// **`KbHistory`**: what the agents' folders hold, and (unless `dry_run`) the read of
    /// what is new in them, queued behind everything else.
    pub(super) async fn kb_history(
        self: &Arc<Self>,
        agent: Option<String>,
        profile: Option<String>,
        since_ms: Option<u64>,
        dry_run: bool,
    ) -> Result<KbHistoryPlan, Error> {
        let db = self.kb_db()?;
        let config = self.config();
        if !config.kb.sessions {
            return Err(Error::invalid(
                "agent sessions are not kept (`kb.sessions` is off), so their history isn't read either: `ptyro config set kb.sessions true`",
            ));
        }
        let agents: Vec<&str> = match agent.as_deref().map(lookup::key) {
            None => AGENTS.to_vec(),
            Some(a) => match AGENTS.iter().find(|x| **x == a) {
                Some(a) => vec![*a],
                None => {
                    return Err(Error::invalid(format!(
                        "{a}'s history can't be read: only {}",
                        AGENTS.join(" and ")
                    )));
                }
            },
        };
        let profiles = match self.check_profile(profile.filter(|p| !p.trim().is_empty()))? {
            Some(p) => vec![p],
            None => ptyro_agents::profile::names(&config),
        };
        // What is never read: kept, forgotten, a chat's, a session's that is kept out.
        let known: HashSet<String> = db
            .uris(ItemKind::Session)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(|(uri, _)| uri)
            .collect();
        let chats: HashSet<String> = match self.chats.db() {
            Ok(db) => db.conversation_ids().await.unwrap_or_default().into_iter().collect(),
            Err(_) => HashSet::new(),
        };
        let mut out_of: HashSet<String> = self.kb.kept_out.locked().clone();
        for s in self.sessions() {
            let m = s.meta.locked();
            let kept_out = m.place.private || m.place.unrecorded || m.place.chat.is_some();
            if let (true, Some(adapter), Some(c)) = (kept_out, &m.adapter, &m.conversation) {
                out_of.insert(format!("session://{adapter}/{c}"));
            }
        }
        let cut = self.kb_retain_cut().into_iter().chain(since_ms).max();
        let managed = self.paths.data.join("profiles");
        let mut plan = KbHistoryPlan { reading: !dry_run, ..KbHistoryPlan::default() };
        let mut jobs = Vec::new();
        let mut seen: HashMap<PathBuf, ()> = HashMap::new();
        for profile in &profiles {
            let dirs = ptyro_agents::profile::info(&config, profile, &managed).dirs;
            for adapter in &agents {
                if self.kb_agent_off(Some(adapter)) {
                    continue;
                }
                let Some(home) = dirs.get(*adapter).cloned() else { continue };
                // (Two profiles that share a folder: its conversations are the first one's.)
                let real = ptyro_core::os::canonicalize(&home).unwrap_or_else(|_| home.clone());
                if seen.insert(real, ()).is_some() {
                    continue;
                }
                let (name, dir) = ((*adapter).to_owned(), home.clone());
                let found = tokio::task::spawn_blocking(move || {
                    ptyro_agents::adapter(&name)
                        .transcripts(&dir)
                        .into_iter()
                        .map(|(id, path)| {
                            let at = std::fs::metadata(&path)
                                .and_then(|m| m.modified())
                                .ok()
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map_or(0, |d| d.as_millis() as u64);
                            (id, path, at)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
                let mut place = KbHistoryPlace {
                    agent: (*adapter).to_owned(),
                    profile: profile.clone(),
                    dir: home.display().to_string(),
                    found: 0,
                    new: 0,
                };
                for (id, path, at) in found {
                    if !crate::agents::conversation_id_ok(&id) {
                        continue;
                    }
                    place.found += 1;
                    let uri = format!("session://{adapter}/{id}");
                    if known.contains(&uri) || out_of.contains(&uri) {
                        plan.known += 1;
                    } else if chats.contains(&id) {
                        plan.chats += 1;
                    } else if cut.is_some_and(|cut| at < cut) {
                        plan.old += 1;
                    } else {
                        place.new += 1;
                        jobs.push(HistoryJob {
                            uri,
                            adapter: (*adapter).to_owned(),
                            conversation: id,
                            profile: profile.clone(),
                            home: home.clone(),
                            path,
                        });
                    }
                }
                plan.found += place.found;
                plan.new += place.new;
                if place.found > 0 {
                    plan.places.push(place);
                }
            }
        }
        if !dry_run {
            for job in jobs {
                self.kb_enqueue_later(Job::History(job));
            }
        }
        Ok(plan)
    }

    /// The actor's part of a [`HistoryJob`]: the gate, the read from the file's start, the
    /// write. A file longer than one read takes is carried on as any item is, from its
    /// cursor.
    pub(super) async fn kb_history_read(self: &Arc<Self>, job: HistoryJob) -> Result<(), Refused> {
        let db = self.kb_db()?;
        let mut facts = Facts {
            session: true,
            agent_off: self.kb_agent_off(Some(&job.adapter)),
            ..Facts::default()
        };
        // Asked before the file is opened: on, sessions kept, the agent not kept out, the
        // conversation not forgotten and not a private session's.
        self.kb_gate(&db, &job.uri, &facts).await?;
        // (Kept meanwhile, by a session that carried the conversation on.)
        if db.item_by_uri(job.uri.clone()).await.map_err(store_err)?.is_some() {
            return Ok(());
        }
        let read = {
            let job = job.clone();
            tokio::task::spawn_blocking(move || {
                super::sessions::import(&job.adapter, &job.home, &job.conversation, &job.path)
            })
            .await
            .map_err(|e| Error::internal(e.to_string()))?
        };
        let Some(piece) = read else { return Ok(()) };
        // Where it ran decides whose it is; a chat's engine and a summary's one-shot are
        // never kept (their folders are ptyro's own).
        let cwd = piece.cwd().map(PathBuf::from);
        let data = &self.paths.data;
        let ours = |dir: &str| {
            cwd.as_deref().is_some_and(|c| {
                let under = data.join(dir);
                c.starts_with(&under)
                    || ptyro_core::os::canonicalize(&under).is_ok_and(|u| c.starts_with(u))
            })
        };
        if ours("chat") || ours("summaries") {
            return Ok(());
        }
        let place = cwd.as_deref().and_then(|c| self.kb_place(c));
        let (project, worktree) = match &place {
            Some((p, w)) => (Some(p.id.clone()), Some(w.id.clone())),
            None => (None, None),
        };
        facts.project_off = self.kb_project_off(project.as_deref(), worktree.as_deref());
        let passed = self.kb_gate(&db, &job.uri, &facts).await?;
        let stamp = Stamp {
            project,
            worktree: worktree.filter(|_| place.as_ref().is_some_and(|(_, w)| !w.is_root)),
            agent: Some(job.adapter.clone()),
            ..Stamp::new(Writer::Import, Some(job.profile.clone()))
        };
        let card = Card {
            agent: Some(job.adapter.clone()),
            project: place.as_ref().map(|(p, _)| p.slug.clone()),
            worktree: place.as_ref().filter(|(_, w)| !w.is_root).map(|(_, w)| w.name.clone()),
            ..Card::default()
        };
        let more =
            self.kb_write_import(&db, passed, job.uri.clone(), stamp, facts, card, piece).await?;
        if more {
            self.kb_enqueue_later(Job::Item(job.uri));
        }
        Ok(())
    }

    /// The registered project a folder is in, and which of its checkouts.
    fn kb_place(
        &self,
        cwd: &Path,
    ) -> Option<(ptyro_core::model::ProjectInfo, ptyro_core::model::WorktreeInfo)> {
        let cwd = ptyro_core::os::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
        let reg = self.registry.locked();
        let w =
            crate::registry::innermost(reg.worktrees.iter().filter(|w| w.machine.is_none()), &cwd)?;
        let p = reg.projects.iter().find(|p| p.id == w.project)?;
        Some((p.clone(), w.clone()))
    }
}

#[cfg(test)]
mod tests;
