//! Who sent a request, and what an agent may not ask for (SPEC §17).
//!
//! Requests are checked by who *sent* them: the connecting process's ancestry names the session
//! it runs in, so an agent that shells out to `ptyro` gets the same refusals whichever way it
//! asks.

use std::sync::Arc;

use ptyro_core::model::WorktreeSpec;
use ptyro_core::session::SessionKind;
use ptyro_core::status::AgentStatus;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{ClientKind, Request};

use crate::daemon::Daemon;
use crate::session::Session;

/// Who sent a request: the connecting process, as the kernel reports it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Caller {
    pub pid: Option<u32>,
    pub kind: Option<ClientKind>,
    /// A paired device and its scope (M9.2); `None` for local clients.
    pub grant: Option<crate::devices::Grant>,
    /// The protocol minor version the client speaks (`None`: one inside the daemon).
    pub minor: Option<u32>,
}

impl Caller {
    /// Another machine's daemon mirroring this one (MM1.B), over ssh or with its token.
    pub fn is_link(&self) -> bool {
        self.kind == Some(ClientKind::Machine) || self.grant.is_some_and(|g| g.machine)
    }

    /// The client is older than protocol 1.`minor`: it knows nothing that version added.
    pub fn older_than(&self, minor: u32) -> bool {
        self.minor.is_some_and(|m| m < minor)
    }
}

impl Daemon {
    /// The session a process runs in, by what the kernel says about it: the nearest ancestor
    /// that is a session's process; else the POSIX session it belongs to (every session's
    /// child leads its own, so a double-forked or `nohup`ed descendant is still placed); else,
    /// on Linux, the `PTYRO_SESSION_ID` it was exec'd with (`setsid` keeps that). A process
    /// that leaves all three (`setsid env -u …`, `systemd-run`, `ssh localhost`) isn't placed:
    /// it looks like a plain shell's CLI, which must keep working (SPEC §17).
    pub(crate) fn session_of_pid(&self, pid: u32) -> Option<Arc<Session>> {
        let sessions = self.sessions();
        let live = |f: &dyn Fn(&Session) -> bool| {
            sessions.iter().find(|s| s.is_live() && f(s)).map(Arc::clone)
        };
        let mut p = pid;
        for _ in 0..64 {
            if let Some(s) = live(&|s| s.pid() == Some(p)) {
                return Some(s);
            }
            match ptyro_pty::parent_pid(p) {
                Some(q) if q > 1 => p = q,
                _ => break,
            }
        }
        if let Some(leader) = ptyro_pty::session_leader(pid)
            && let Some(s) = live(&|s| s.pid() == Some(leader))
        {
            return Some(s);
        }
        let id = ptyro_pty::exec_env_var(pid, "PTYRO_SESSION_ID")?;
        live(&|s| s.id.full() == id)
    }

    /// Refuse what agents may not do, whichever way they ask.
    pub(crate) fn guard_caller(&self, req: &Request, caller: Caller) -> Result<(), Error> {
        // The hook relay runs inside the agent's process tree: exempt what it sends from the
        // agent's refusals, never a client that merely *says* it is a hook (`kind` comes from
        // the client's own Hello). What it sends is its own session's, though: a process placed
        // in one session can't report a status, a hook or a statusline for another, which would
        // forge what a notification or the sidebar believes.
        if let Request::Hook { session: target, .. } | Request::StatusLine { session: target, .. } =
            req
        {
            if caller.grant.is_none()
                && let Some(pid) = caller.pid
                && let Some(mine) = self.session_of_pid(pid)
                && !self.resolve(target).is_ok_and(|t| t.id == mine.id)
            {
                return Err(Error::new(
                    ErrorCode::PermissionDenied,
                    "a program in one session can only report for its own session",
                ));
            }
            return Ok(());
        }
        // A paired device is a person, held to its scope instead (`guard_device`), even when
        // `ptyro serve` itself was started inside an agent's pane.
        if caller.grant.is_some() {
            return Ok(());
        }
        let Some(pid) = caller.pid else { return Ok(()) };
        let Some(s) = self.session_of_pid(pid) else { return Ok(()) };
        self.guard_agent(req, &s)
    }

    /// What the agent in `s` may not ask for (nothing is refused to other kinds of session).
    pub(crate) fn guard_agent(&self, req: &Request, s: &Arc<Session>) -> Result<(), Error> {
        if s.kind() != SessionKind::Agent {
            return Ok(());
        }
        let name = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone();
        let refuse = |what: &str| {
            Err(Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "{name} is an agent: {what} (the TUI, the CLI outside agents, or your phone)"
                ),
            ))
        };
        // No agent answers for a person or extends what is trusted. These defaults stop an
        // agent's mistake or a casual prompt injection, not code set on getting around them
        // (SPEC §17).
        if let Some(why) = for_a_person(req) {
            return refuse(why);
        }
        match req {
            // Keys typed into a dialog answer it (Claude's preselects "Yes"): a blocked
            // session's prompt is answered with `answer`, by a person.
            Request::SendText { addr, .. } | Request::SendBytes { addr, .. }
                if self
                    .resolve_many(addr)
                    .is_ok_and(|v| v.iter().any(|t| t.status() == Some(AgentStatus::Blocked))) =>
            {
                refuse("it can't type into a session that waits on a person's answer")
            }
            _ => Ok(()),
        }
    }
}

/// What only a person asks for, whatever the session (`None`: an agent may ask too).
fn for_a_person(req: &Request) -> Option<&'static str> {
    match req {
        Request::Answer { .. } => Some("permission prompts are answered by a person"),
        // A token minted here would leave through the agent's output.
        Request::DevicePair { .. }
        | Request::DeviceRevoke { .. }
        | Request::PushSubscribe { .. }
        | Request::PushUnsubscribe { .. } => Some("devices are paired and managed by a person"),
        Request::ProjectTrust { .. }
        | Request::WorktreeNew { spec: WorktreeSpec { trust: Some(_), .. } } => {
            Some("repository scripts are trusted by a person")
        }
        // Where a project's repository is, and folding one project into another (SPEC §10.1).
        Request::ProjectMove { .. } | Request::ProjectAdd { moved: true, .. } => {
            Some("projects are moved by a person")
        }
        Request::Upgrade { binary: Some(_) } => {
            Some("the daemon is upgraded to another binary by a person")
        }
        Request::SelfUpdate | Request::MachineUpdate { .. } => Some("ptyro is updated by a person"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent may add a project, never move one or fold one into another; nor trust, pair,
    /// answer or update.
    #[test]
    fn agents_never_move_projects() {
        let add = |moved: bool| Request::ProjectAdd {
            path: "/r".into(),
            workspace: None,
            moved,
            separate: false,
        };
        let mv = Request::ProjectMove { addr: "app".into(), path: "/new/app".into() };
        assert_eq!(for_a_person(&mv), Some("projects are moved by a person"));
        assert_eq!(for_a_person(&add(true)), Some("projects are moved by a person"));
        assert_eq!(for_a_person(&add(false)), None);
        let trust = Request::ProjectTrust { addr: "app".into(), fingerprint: "f".into() };
        assert_eq!(for_a_person(&trust), Some("repository scripts are trusted by a person"));
        assert_eq!(for_a_person(&Request::SelfUpdate), Some("ptyro is updated by a person"));
        assert_eq!(for_a_person(&Request::ProjectList), None);
    }
}
