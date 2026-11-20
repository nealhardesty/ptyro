//! The knowledge base across linked machines (SPEC §25.11, ROADMAP KB1.12). Each machine has
//! its own `kb.db`; nothing is copied. A search asks every linked machine that is up as well
//! and interleaves the answers (each machine's own ranking, as chat search does, §22.10); an
//! address under a machine's name (`laptop2:k:7f3k/12`) is read, listed or forgotten there.
//! What a machine reads (its roots) is listed, added to, looked at again and removed there too,
//! when a request names it (`machine`, 1.59).
//!
//! The requests are never routed by `handle_as` (they are `here_only`): `handle_kb` sends them
//! on from here, because it knows who asks. An agent's search reaches another machine with its
//! profile by name, and that machine answers with its own items of that profile.

use std::time::Duration;

use ptyro_core::kb::search::KbHit;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};

use crate::daemon::Daemon;

/// How long one machine has to answer before the search goes on without it.
const FAR_WAIT: Duration = Duration::from_secs(5);

impl Daemon {
    /// The machine a filter names: `None` for this one (its own name, or `here`), a linked
    /// one's name, else an error that says which there are.
    pub(super) fn kb_machine(&self, name: &str) -> Result<Option<String>, Error> {
        let name = name.trim();
        if name == "here" || name == self.machine() {
            return Ok(None);
        }
        let links = self.links.locked();
        if links.contains_key(name) {
            return Ok(Some(name.to_owned()));
        }
        let mut known: Vec<&str> = links.keys().map(String::as_str).collect();
        known.insert(0, "here");
        Err(Error::new(
            ErrorCode::NotFound,
            format!("no linked machine is called {name:?}: say one of {}", known.join(", ")),
        ))
    }

    /// Every linked machine, up or not (one that is down is named in the answer).
    pub(super) fn kb_linked(&self) -> Vec<String> {
        self.links.locked().keys().cloned().collect()
    }

    /// An address as (the linked machine it is on, the address there): `laptop2:k:7f3k/12` is
    /// `k:7f3k/12` on laptop2. This machine's own name in front is taken off.
    pub(super) fn kb_split(&self, addr: &str) -> (Option<String>, String) {
        match addr.split_once(':') {
            // (A machine may be called `k`: `k:7f3k` alone is still an item here.)
            Some((m, rest))
                if self.links.locked().contains_key(m)
                    && (rest.contains(':') || !matches!(m, "k" | "c")) =>
            {
                (Some(m.to_owned()), rest.to_owned())
            }
            Some((m, rest)) if m == self.machine() && rest.contains(':') => (None, rest.to_owned()),
            _ => (None, addr.to_owned()),
        }
    }

    /// Ask one linked machine, for no longer than [`FAR_WAIT`].
    pub(super) async fn kb_far(&self, machine: &str, req: Request) -> Result<Reply, Error> {
        match tokio::time::timeout(FAR_WAIT, self.forward(machine, req)).await {
            Ok(reply) => reply,
            Err(_) => Err(Error::new(
                ErrorCode::DaemonUnavailable,
                format!("{machine} didn't answer within {}s", FAR_WAIT.as_secs()),
            )),
        }
    }

    /// A request about roots (`KbRoots`, `KbIngest`, `KbSync`, `KbRemove`, `KbHistory`) that
    /// names a linked machine: asked there, and its answer. `Err`: the request, which is this
    /// machine's own. Only you may: an agent's session (`agent`) is told so.
    pub(super) async fn kb_roots_far(
        &self,
        mut req: Request,
        agent: bool,
    ) -> Result<Result<Reply, Request>, Error> {
        let named = match &mut req {
            Request::KbRoots { machine }
            | Request::KbSync { machine, .. }
            | Request::KbIngest { machine, .. }
            | Request::KbRemove { machine, .. }
            | Request::KbHistory { machine, .. } => machine.take(),
            _ => None,
        };
        let Some(m) = named.map(|m| self.kb_machine(&m)).transpose()?.flatten() else {
            return Ok(Err(req));
        };
        if agent {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                format!("an agent's session asks about what this machine reads, not {m}"),
            ));
        }
        self.kb_far(&m, req).await.map(Ok)
    }

    /// The same search on each of `machines`, all at once. Returns each one's hits, best
    /// first, under its name (`machine`, and the address `laptop2:k:7f3k/12`), and who was
    /// left out with why. A machine that has no project of that name has nothing, and is not
    /// left out. `alone` (one machine was asked for by name): its error is the answer.
    pub(super) async fn kb_search_far(
        &self,
        machines: &[String],
        ask: &Request,
        alone: bool,
    ) -> Result<(Vec<Vec<KbHit>>, Vec<String>), Error> {
        let asks = machines.iter().map(|m| async move { (m, self.kb_far(m, ask.clone()).await) });
        let (mut lists, mut left_out) = (Vec::new(), Vec::new());
        for (m, answer) in futures::future::join_all(asks).await {
            match answer {
                Ok(Reply::KbHits { hits, .. }) => {
                    let hidden = self.hidden_of(m);
                    lists.push(
                        hits.into_iter()
                            .filter(|h| {
                                !h.workspace_name
                                    .as_ref()
                                    .is_some_and(|w| hidden.iter().any(|x| x == w))
                            })
                            .map(|mut h| {
                                h.addr = format!("{m}:{}", h.addr);
                                h.machine = Some(m.clone());
                                h
                            })
                            .collect(),
                    );
                }
                Ok(other) => {
                    tracing::warn!(machine = %m, "knowledge base search: not hits: {other:?}");
                    left_out.push(format!("{m} gave an answer this ptyro can't read"));
                }
                Err(e) if alone => return Err(e),
                Err(e) if e.code == ErrorCode::NotFound => {}
                Err(e) => {
                    tracing::warn!(machine = %m, "knowledge base search left it out: {}", e.message);
                    left_out.push(if e.message.starts_with(m.as_str()) {
                        e.message
                    } else {
                        format!("{m}: {}", e.message)
                    });
                }
            }
        }
        Ok((lists, left_out))
    }
}
