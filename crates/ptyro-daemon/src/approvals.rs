//! Remote approvals (SPEC §16.6, M9.5). While a device is paired, Claude's `PermissionRequest`
//! hook is held (bounded by `[remote] approval_wait`) so an answer from a phone reaches Claude as
//! an exact decision. Verified on Claude Code 2.1.305: Claude shows its own permission dialog
//! while the hook runs, and keeps it if the hook returns no decision. Whoever answers first
//! wins: a held hook's decision closes the dialog, and an answer in the dialog makes Claude
//! ignore the hook. So holding costs the person at the terminal nothing.

use std::sync::Arc;

use ptyro_agents::Decision;
use ptyro_core::SessionId;
use ptyro_core::config::parse_duration;
use ptyro_protocol::Reply;
use tokio::sync::oneshot;

use crate::daemon::Daemon;
use crate::session::Session;

/// One held hook: answering sends the decision to the waiting hook relay.
pub struct Held {
    tx: oneshot::Sender<Option<Decision>>,
    /// Which prompt: a newer one replaces it, and only its own waiter removes it.
    generation: u64,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A hold registered before the prompt's signals are applied (so an answer that arrives at
/// once already finds it), then awaited.
pub struct Hold {
    rx: oneshot::Receiver<Option<Decision>>,
    generation: u64,
    wait: std::time::Duration,
}

/// Claude's `PermissionRequest` hook output for a decision.
pub fn hook_output(d: Decision, by: &str) -> String {
    let decision = match d {
        Decision::Deny => {
            serde_json::json!({ "behavior": "deny", "message": format!("Denied by {by} (ptyro)") })
        }
        _ => serde_json::json!({ "behavior": "allow" }),
    };
    serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
        .to_string()
}

impl Daemon {
    /// Start holding a Claude permission prompt for a remote answer (`None`: not held).
    pub(crate) fn begin_hold(&self, s: &Arc<Session>) -> Option<Hold> {
        let wait = parse_duration(&self.config().remote.approval_wait).ok()?;
        // A device whose pairing code is still unclaimed can't answer yet.
        if wait.is_zero()
            || !self.devices.lock().expect("devices").iter().any(|d| d.pairing.is_none())
        {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // A newer prompt replaces an older one (its dialog is gone).
        if let Some(old) = self.held.lock().expect("held").insert(s.id, Held { tx, generation }) {
            let _ = old.tx.send(None);
        }
        Some(Hold { rx, generation, wait })
    }

    /// Wait for the held prompt's answer; `None` lets Claude's dialog decide.
    pub(crate) async fn finish_hold(&self, s: &Arc<Session>, h: Hold) -> Option<Reply> {
        let decision = tokio::time::timeout(h.wait, h.rx).await.ok().and_then(Result::ok).flatten();
        {
            let mut held = self.held.lock().expect("held");
            if held.get(&s.id).is_some_and(|x| x.generation == h.generation) {
                held.remove(&s.id);
            }
        }
        let d = decision?;
        Some(Reply::HookOutput { stdout: hook_output(d, "a paired device") })
    }

    /// Answer through the held hook when there is one (allow and deny; "always" needs the dialog).
    pub(crate) fn answer_held(&self, id: SessionId, d: Decision) -> bool {
        if matches!(d, Decision::Always) {
            return false;
        }
        match self.held.lock().expect("held").remove(&id) {
            Some(h) => h.tx.send(Some(d)).is_ok(),
            None => false,
        }
    }

    /// The prompt was answered elsewhere (in Claude's dialog): let the hook go.
    pub(crate) fn release_held(&self, id: SessionId) {
        if let Some(h) = self.held.lock().expect("held").remove(&id) {
            let _ = h.tx.send(None);
        }
    }

    /// For status: is a prompt held for an exact remote answer?
    pub(crate) fn is_held(&self, id: SessionId) -> bool {
        self.held.lock().expect("held").contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_outputs_match_claudes_schema() {
        let v: serde_json::Value =
            serde_json::from_str(&hook_output(Decision::Allow, "phone")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let v: serde_json::Value =
            serde_json::from_str(&hook_output(Decision::Deny, "phone")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(v["hookSpecificOutput"]["decision"]["message"].as_str().unwrap().contains("phone"));
    }
}
