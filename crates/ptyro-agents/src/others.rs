//! The other adapters: OpenCode (SPEC §8.6), and the generic and shell adapters (§8.7).

use std::time::Duration;

use ptyro_core::status::{AgentStatus, Signal};
use serde_json::Value;

use crate::adapter::{Adapter, Caps, LaunchCtx, LaunchPlan, Sidecar, StopStep, random_hex, str_of};

// ── OpenCode ──────────────────────────────────────────────────────────────

/// OpenCode's TUI embeds an HTTP server: ptyro gives it a port and a password and follows its
/// SSE stream (SPEC §8.6). Answers go through its permissions API.
pub struct OpenCode;

impl Adapter for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn caps(&self) -> Caps {
        Caps {
            preset_conversation: false,
            learn_conversation: true,
            resume: true,
            hooks: false,
            osc_status: true,
            title_status: false,
            permission_signal: true,
            turn_end_signal: true,
            answer: true,
        }
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String> {
        let port = ctx.port.ok_or("no free port for OpenCode's server")?;
        let password = random_hex();
        let mut argv = ctx.base_argv.clone();
        argv.extend(["--port".into(), port.to_string(), "--hostname".into(), "127.0.0.1".into()]);
        Ok(LaunchPlan {
            argv,
            env: vec![("OPENCODE_SERVER_PASSWORD".into(), password.clone())],
            sidecar: Some(Sidecar::OpenCode { port, password }),
            ..LaunchPlan::default()
        })
    }

    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String> {
        let mut plan = self.launch(ctx)?;
        plan.argv.extend(["--session".into(), conversation.to_owned()]);
        plan.conversation = Some(conversation.to_owned());
        Ok(plan)
    }

    fn stop(&self) -> Vec<StopStep> {
        vec![StopStep::InterruptKeys(b"\x1b".to_vec()), StopStep::Wait(Duration::from_millis(500))]
    }
}

/// One OpenCode SSE event (`{type, properties}`) as signals.
pub fn opencode_event(ev: &Value) -> Vec<Signal> {
    let props = ev.get("properties").cloned().unwrap_or(Value::Null);
    let mut out = Vec::new();
    if let Some(id) = str_of(&props, "sessionID").or_else(|| {
        props
            .get("info")
            .and_then(|i| str_of(i, "id"))
            .filter(|_| str_of(ev, "type") == Some("session.created"))
    }) {
        out.push(Signal::Conversation { id: id.to_owned() });
    }
    match str_of(ev, "type").unwrap_or("") {
        "session.status" => match props
            .get("status")
            .and_then(|s| str_of(s, "type"))
            .or_else(|| str_of(&props, "status"))
        {
            Some("busy") => out.push(Signal::status(AgentStatus::Working)),
            Some("retry") => out.push(Signal::Status {
                status: AgentStatus::Working,
                kind: None,
                detail: props
                    .get("status")
                    .and_then(|s| str_of(s, "message"))
                    .map(|m| format!("retrying: {m}")),
                progress: None,
            }),
            Some("idle") => out.push(Signal::status(AgentStatus::Idle)),
            _ => {}
        },
        "session.idle" => {
            out.push(Signal::status(AgentStatus::Idle));
            out.push(Signal::TurnEnd);
        }
        "permission.asked" | "permission.updated" => {
            let detail =
                str_of(&props, "title").or_else(|| str_of(&props, "type")).map(str::to_owned);
            out.push(Signal::blocked("permission", detail));
        }
        "permission.replied" => out.push(Signal::status(AgentStatus::Working)),
        "session.error" => out.push(Signal::Status {
            status: AgentStatus::Errored,
            kind: props.get("error").and_then(|e| str_of(e, "name")).map(str::to_owned),
            detail: props
                .get("error")
                .and_then(|e| e.get("data"))
                .and_then(|d| str_of(d, "message"))
                .map(str::to_owned),
            progress: None,
        }),
        "tool.execute.before" => {
            let t = str_of(&props, "tool").unwrap_or("tool");
            out.push(Signal::Activity { text: t.to_owned() });
        }
        _ => {}
    }
    out
}

// ── generic and shell ─────────────────────────────────────────────────────

/// Any CLI. OSC 7501 from it is trusted like any other emitter's (R-STA-01); otherwise titles,
/// notifications and the activity heuristic.
pub struct Generic;

impl Adapter for Generic {
    fn id(&self) -> &'static str {
        "generic"
    }

    fn caps(&self) -> Caps {
        Caps { osc_status: true, ..Caps::default() }
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String> {
        Ok(LaunchPlan { argv: ctx.base_argv.clone(), ..LaunchPlan::default() })
    }

    fn resume(&self, _ctx: &LaunchCtx<'_>, _conversation: &str) -> Result<LaunchPlan, String> {
        Err("this agent can't resume a conversation".into())
    }

    fn done_means_idle(&self) -> bool {
        false
    }

    fn stop(&self) -> Vec<StopStep> {
        vec![StopStep::Wait(Duration::ZERO)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn opencode_events() {
        let busy = opencode_event(
            &json!({"type": "session.status", "properties": {"sessionID": "ses_1", "status": {"type": "busy"}}}),
        );
        assert_eq!(
            busy,
            vec![Signal::Conversation { id: "ses_1".into() }, Signal::status(AgentStatus::Working)]
        );
        let ask = opencode_event(
            &json!({"type": "permission.asked", "properties": {"sessionID": "ses_1", "title": "Run npm install"}}),
        );
        assert!(ask.contains(&Signal::blocked("permission", Some("Run npm install".into()))));
        let idle =
            opencode_event(&json!({"type": "session.idle", "properties": {"sessionID": "ses_1"}}));
        assert!(idle.contains(&Signal::TurnEnd));
        let plan = OpenCode
            .launch(&LaunchCtx {
                agent: "opencode",
                def: &Default::default(),
                base_argv: vec!["opencode".into()],
                session: "s",
                name: "n",
                cwd: Path::new("/"),
                integrations: Path::new("/i"),
                ptyro: "ptyro",
                port: Some(23000),
                agent_home: None,
                project_root: None,
            })
            .unwrap();
        assert!(plan.argv.windows(2).any(|w| w == ["--port", "23000"]));
        let Some(Sidecar::OpenCode { password, .. }) = plan.sidecar else { panic!() };
        assert_eq!(plan.env, vec![("OPENCODE_SERVER_PASSWORD".to_owned(), password)]);
    }
}
