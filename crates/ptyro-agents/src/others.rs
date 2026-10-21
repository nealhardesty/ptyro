//! The other adapters: Gemini CLI (SPEC §8.5), OpenCode (§8.6), Antigravity CLI (M4.21), and the
//! generic and shell adapters (§8.7).

use std::time::Duration;

use ptyro_core::status::{AgentStatus, Signal};
use serde_json::{Value, json};

use crate::adapter::{
    Adapter, Caps, Decision, LaunchCtx, LaunchPlan, Sidecar, StopStep, random_hex, str_of,
    tool_summary, uuid_v4,
};
use crate::claude::shell_word;

// ── Gemini CLI ────────────────────────────────────────────────────────────

/// Gemini's hooks come from a linked ptyro extension (opt-in: `ptyro integrations install
/// gemini`), because Gemini has no per-launch hook flag (SPEC §8.5).
pub struct Gemini;

pub const GEMINI_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "BeforeAgent",
    "AfterAgent",
    "BeforeTool",
    "AfterTool",
    "Notification",
];

/// The extension: `gemini-extension.json` + `hooks/hooks.json`, calling `ptyro hook gemini …`.
pub fn gemini_extension(ptyro: &str) -> Vec<(&'static str, String)> {
    let mut hooks = serde_json::Map::new();
    for ev in GEMINI_EVENTS {
        hooks.insert((*ev).into(), json!([{ "hooks": [{ "type": "command", "command": format!("{} hook gemini {ev}", shell_word(ptyro)) }] }]));
    }
    vec![
        (
            "gemini-extension.json",
            serde_json::to_string_pretty(&json!({ "name": "ptyro", "version": env!("CARGO_PKG_VERSION"), "description": "ptyro session status (inert outside ptyro)" })).unwrap_or_default(),
        ),
        ("hooks/hooks.json", serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default()),
    ]
}

impl Adapter for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn caps(&self) -> Caps {
        Caps {
            preset_conversation: true,
            learn_conversation: true,
            resume: true,
            hooks: true,
            osc_status: false,
            title_status: true,
            permission_signal: true,
            turn_end_signal: true,
            answer: true,
        }
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String> {
        let id = uuid_v4();
        let mut argv = ctx.base_argv.clone();
        argv.extend(["--session-id".into(), id.clone()]);
        Ok(LaunchPlan { argv, conversation: Some(id), ..LaunchPlan::default() })
    }

    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String> {
        let mut argv = ctx.base_argv.clone();
        argv.extend(["--resume".into(), conversation.to_owned()]);
        Ok(LaunchPlan {
            argv,
            conversation: Some(conversation.to_owned()),
            ..LaunchPlan::default()
        })
    }

    fn on_hook(&self, event: &str, p: &Value) -> Vec<Signal> {
        let mut out = Vec::new();
        if let Some(id) = str_of(p, "session_id") {
            out.push(Signal::Conversation { id: id.to_owned() });
        }
        match event {
            "SessionStart" => out.push(Signal::status(AgentStatus::Idle)),
            "BeforeAgent" => {
                out.push(Signal::status(AgentStatus::Working));
                if let Some(t) = str_of(p, "prompt") {
                    out.push(Signal::Prompt { text: t.to_owned() });
                }
            }
            "BeforeTool" => {
                out.push(Signal::status(AgentStatus::Working));
                let tool = tool_summary(
                    str_of(p, "tool_name").unwrap_or("tool"),
                    p.get("tool_input").unwrap_or(&Value::Null),
                );
                out.push(Signal::Activity { text: tool });
            }
            "AfterTool" => out.push(Signal::status(AgentStatus::Working)),
            "AfterAgent" => {
                out.push(Signal::status(AgentStatus::Idle));
                out.push(Signal::TurnEnd);
            }
            "Notification" if str_of(p, "notification_type") == Some("ToolPermission") => {
                let detail = str_of(p, "message").map(str::to_owned).or_else(|| {
                    p.get("details").and_then(|d| str_of(d, "title")).map(str::to_owned)
                });
                out.push(Signal::blocked("permission", detail));
            }
            _ => {}
        }
        out
    }

    fn on_title(&self, title: &str) -> Option<Signal> {
        // `◇  Ready (<folder>)`, `Action Required`, `Working…` (ui.dynamicWindowTitle).
        let t = title.trim_start_matches(|c: char| !c.is_alphabetic());
        if t.starts_with("Ready") {
            Some(Signal::status(AgentStatus::Idle))
        } else if t.starts_with("Action Required") {
            Some(Signal::blocked("permission", None))
        } else if t.starts_with("Working") || t.starts_with("Thinking") {
            Some(Signal::status(AgentStatus::Working))
        } else {
            None
        }
    }

    fn answer(&self, d: Decision) -> Option<Vec<u8>> {
        // [unverified] The confirmation list: 1 allow once · 2 allow always · Esc cancel.
        Some(match d {
            Decision::Allow => b"1".to_vec(),
            Decision::Always => b"2".to_vec(),
            Decision::Deny => b"\x1b".to_vec(),
        })
    }

    fn stop(&self) -> Vec<StopStep> {
        vec![
            StopStep::InterruptKeys(b"\x1b".to_vec()),
            StopStep::Wait(Duration::from_millis(800)),
            StopStep::Keys(b"\x15/quit\r".to_vec()),
            StopStep::Wait(Duration::from_secs(5)),
        ]
    }
}

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

// ── Antigravity CLI (`agy`) ───────────────────────────────────────────────

/// Gemini CLI's successor for consumer accounts (2026-06-18). Hooks from its docs
/// ([unverified]: not installed here): `SessionStart`, `PreInvocation`, `PreToolUse`,
/// `PostToolUse`, `PostInvocation`, `Stop{fullyIdle, terminationReason, error}`, camelCase
/// fields. It has no permission hook: blocked comes from OSC 7501 or nothing.
pub struct Antigravity;

pub const AGY_EVENTS: &[&str] =
    &["SessionStart", "PreInvocation", "PreToolUse", "PostToolUse", "PostInvocation", "Stop"];

/// `hooks.json` for `~/.gemini/config/` (opt-in, `ptyro integrations install antigravity`).
pub fn agy_hooks(ptyro: &str) -> String {
    let mut hooks = serde_json::Map::new();
    for ev in AGY_EVENTS {
        hooks.insert((*ev).into(), json!([{ "hooks": [{ "type": "command", "command": format!("{} hook antigravity {ev}", shell_word(ptyro)) }] }]));
    }
    serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default()
}

impl Adapter for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn caps(&self) -> Caps {
        Caps {
            preset_conversation: false,
            learn_conversation: true,
            resume: true,
            hooks: true,
            osc_status: true,
            title_status: false,
            permission_signal: false,
            turn_end_signal: true,
            answer: false,
        }
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String> {
        Ok(LaunchPlan { argv: ctx.base_argv.clone(), ..LaunchPlan::default() })
    }

    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String> {
        let mut argv = ctx.base_argv.clone();
        argv.extend(["--resume".into(), conversation.to_owned()]);
        Ok(LaunchPlan {
            argv,
            conversation: Some(conversation.to_owned()),
            ..LaunchPlan::default()
        })
    }

    fn on_hook(&self, event: &str, p: &Value) -> Vec<Signal> {
        let mut out = Vec::new();
        if let Some(id) = str_of(p, "conversationId") {
            out.push(Signal::Conversation { id: id.to_owned() });
        }
        match event {
            "SessionStart" => out.push(Signal::status(AgentStatus::Idle)),
            "PreInvocation" => out.push(Signal::status(AgentStatus::Working)),
            "PreToolUse" => {
                out.push(Signal::status(AgentStatus::Working));
                let tool = tool_summary(
                    str_of(p, "toolName").or_else(|| str_of(p, "tool_name")).unwrap_or("tool"),
                    p.get("toolInput").or_else(|| p.get("tool_input")).unwrap_or(&Value::Null),
                );
                out.push(Signal::Activity { text: tool });
            }
            "PostToolUse" => out.push(Signal::status(AgentStatus::Working)),
            "Stop" => {
                if let Some(e) = str_of(p, "error") {
                    out.push(Signal::Status {
                        status: AgentStatus::Errored,
                        kind: str_of(p, "terminationReason").map(str::to_owned),
                        detail: Some(e.to_owned()),
                        progress: None,
                    });
                } else if p.get("fullyIdle").and_then(Value::as_bool) != Some(false) {
                    out.push(Signal::status(AgentStatus::Idle));
                    out.push(Signal::TurnEnd);
                }
            }
            _ => {}
        }
        out
    }
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
    use std::path::Path;

    #[test]
    fn gemini_fixtures_and_titles() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gemini-0.64.0/hooks/SessionStart.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        let s = Gemini.on_hook("SessionStart", &v);
        assert!(
            s.contains(&Signal::Conversation { id: "b0583f04-e3e3-4cac-a366-568e31891152".into() })
        );
        assert_eq!(Gemini.on_title("◇  Ready (proj)"), Some(Signal::status(AgentStatus::Idle)));
        assert_eq!(
            Gemini.on_title("✋  Action Required (proj)"),
            Some(Signal::blocked("permission", None))
        );
        let n = Gemini.on_hook(
            "Notification",
            &json!({"notification_type": "ToolPermission", "message": "Allow shell: ls"}),
        );
        assert!(n.contains(&Signal::blocked("permission", Some("Allow shell: ls".into()))));
        // The extension matches the M0.5 template's hook set.
        let ext = gemini_extension("ptyro");
        let hooks: Value = serde_json::from_str(&ext[1].1).unwrap();
        let tmpl: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/gemini-0.64.0/extension/hooks/hooks.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(hooks, tmpl);
    }

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
                mcp: None,
            })
            .unwrap();
        assert!(plan.argv.windows(2).any(|w| w == ["--port", "23000"]));
        let Some(Sidecar::OpenCode { password, .. }) = plan.sidecar else { panic!() };
        assert_eq!(plan.env, vec![("OPENCODE_SERVER_PASSWORD".to_owned(), password)]);
    }

    #[test]
    fn antigravity_hooks() {
        let s = Antigravity.on_hook("Stop", &json!({"conversationId": "c1", "fullyIdle": true}));
        assert_eq!(
            s,
            vec![
                Signal::Conversation { id: "c1".into() },
                Signal::status(AgentStatus::Idle),
                Signal::TurnEnd
            ]
        );
        assert!(
            Antigravity.on_hook("Stop", &json!({"fullyIdle": false})).is_empty(),
            "background work continues"
        );
        assert!(
            Antigravity
                .on_hook("PreInvocation", &json!({}))
                .contains(&Signal::status(AgentStatus::Working))
        );
        assert!(agy_hooks("/p").contains("/p hook antigravity Stop"));
    }
}
