//! What the daemon tells `ptyro doctor` (M5.6): its environment, the problems it noticed, and a
//! hook self-test that runs the fake agent through an adapter's real integration.

use ptyro_core::sync::MutexExt;
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::session::SessionKind;
use ptyro_core::status::{AgentStatus, SignalSource};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize, now_ms};
use ptyro_protocol::{DaemonDoctor, SelfTestResult};

use crate::daemon::Daemon;
use crate::launch::{AgentStart, Launch};
use crate::session::Place;

/// Adapters whose integration ptyro wires in per launch, so a self-test needs nothing installed.
const SELF_TESTABLE: &[&str] = &["claude", "codex", "opencode"];

impl Daemon {
    /// Remember a problem for `ptyro doctor` (once).
    pub(crate) fn warn(&self, text: String) {
        let mut w = self.warnings.locked();
        if !w.contains(&text) {
            tracing::warn!("{text}");
            w.push(text);
        }
    }

    pub(crate) fn doctor(&self) -> DaemonDoctor {
        let var = |k: &str| {
            self.base_env
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let leaked_vars = ptyro_agents::profile::MANAGED_VARS
            .iter()
            .filter(|v| **v != "PTYRO_PROFILE")
            .filter(|v| var(v).is_some())
            .map(|v| (*v).to_owned())
            .collect();
        let warnings = self.warnings.locked().clone();
        let config_error =
            warnings.iter().find(|w| w.starts_with("the config didn't load")).cloned();
        DaemonDoctor {
            leaked_vars,
            path: var("PATH").unwrap_or_default(),
            warnings,
            config_error,
            chats: None,
        }
    }

    /// Start `ptyro fake-agent --as <adapter>` through the adapter, give it one short turn, and
    /// report the statuses that arrived and where from. It passes when the turn's working and
    /// idle both came from the integration (hooks or the sidecar), not from guessing.
    pub(crate) async fn self_test(
        self: &Arc<Self>,
        adapter: &str,
    ) -> Result<SelfTestResult, Error> {
        if !SELF_TESTABLE.contains(&adapter) {
            return Err(Error::invalid(format!(
                "the self-test covers {} (the others need an integration installed first)",
                SELF_TESTABLE.join(", ")
            )));
        }
        let exe = std::env::current_exe().map_err(|e| Error::internal(e.to_string()))?;
        let dir = self.paths.data.join("selftest");
        std::fs::create_dir_all(&dir).map_err(|e| Error::internal(e.to_string()))?;
        let id = SessionId::new();
        let name = self.free_name(&format!("selftest-{adapter}"));
        let s = self
            .launch(Launch {
                id,
                argv: Vec::new(),
                kind: SessionKind::Agent,
                name,
                cwd: dir,
                size: TermSize::new(100, 30),
                client_env: Default::default(),
                place: Place {
                    profile: ptyro_core::model::DEFAULT_PROFILE.into(),
                    agent: Some(adapter.to_owned()),
                    ..Place::default()
                },
                replace: false,
                agent: Some(AgentStart {
                    name: adapter.to_owned(),
                    resume: None,
                    prompt: None,
                    command: Some(vec![
                        exe.to_string_lossy().into_owned(),
                        "fake-agent".into(),
                        "--as".into(),
                        adapter.into(),
                    ]),
                    mcp: false,
                    structured: false,
                    params: None,
                    fork: false,
                    chat_tools: false,
                }),
            })
            .await?;
        let mut seen: Vec<(AgentStatus, SignalSource)> = Vec::new();
        let mut sent = false;
        let start = now_ms();
        let integrated =
            |src: SignalSource| matches!(src, SignalSource::Hook | SignalSource::Sidecar);
        // Passes when the turn's start came from the integration (hooks or the sidecar).
        let ok = loop {
            if let Some(r) = s.engine.locked().record.clone()
                && seen.last() != Some(&(r.status, r.source))
            {
                seen.push((r.status, r.source));
            }
            // Whether or not it won the race against OSC 7501, the integration must have spoken.
            let worked = s.engine.locked().explain.iter().any(|l| {
                l.signal.starts_with("working")
                    && (l.source.starts_with("hook") || l.source.starts_with("sidecar"))
            });
            // The end of the turn may come from OSC 7501 first (the agent's own report).
            let settled = seen.last().is_some_and(|(st, src)| {
                *st == AgentStatus::Idle && (integrated(*src) || *src == SignalSource::Osc)
            });
            if worked && settled {
                break true;
            }
            // A sidecar must be listening before the turn, or its events are lost.
            let listening = adapter != "opencode" || self.sidecar_live.locked().contains(&s.id);
            if !sent && listening && now_ms().saturating_sub(start) > 300 {
                s.send_text("work 0.2 ptyro self-test", true).await;
                sent = true;
            }
            if !s.is_live() || now_ms().saturating_sub(start) > 15_000 {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if s.is_live() {
            s.send_text("exit", true).await;
            let _ = tokio::time::timeout(Duration::from_secs(3), s.wait_exit()).await;
            let _ = s.kill(None).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), s.wait_exit()).await;
        self.forget_session(s.id);
        let detail = (!ok).then(|| {
            if seen.is_empty() {
                "no status arrived at all: the integration didn't reach the daemon".to_owned()
            } else {
                "the turn's status didn't come from the integration (hooks); ptyro would be guessing"
                    .to_owned()
            }
        });
        if ok {
            Ok(SelfTestResult {
                adapter: adapter.into(),
                ok,
                seen: seen
                    .iter()
                    .map(|(st, src)| format!("{} via {}", st.label(), src.label()))
                    .collect(),
                detail,
            })
        } else {
            Err(Error::new(
                ErrorCode::Internal,
                format!(
                    "{adapter} self-test failed: {} (seen: {})",
                    detail.unwrap_or_default(),
                    seen.iter()
                        .map(|(st, src)| format!("{} via {}", st.label(), src.label()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ))
        }
    }
}
