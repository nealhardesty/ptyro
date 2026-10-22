//! Claude Code (SPEC §8.2, verified against 2.1.304).
//!
//! Launch: `claude --session-id <uuid> -n <name> --plugin-dir <ptyro plugin>`. The plugin's
//! `hooks/hooks.json` calls `ptyro hook claude <Event>` for every event that carries status; the
//! plugin dir lives under `data/integrations/claude/<hash>/`, never in the user's config or the
//! worktree. With usage meters on, `--settings` points `statusLine` at `ptyro statusline-relay`,
//! which chains to the user's own statusline (M4.17).

use std::time::Duration;

use ptyro_core::status::{AgentStatus, Signal};
use serde_json::{Value, json};

use crate::adapter::{
    Adapter, Caps, Decision, LaunchCtx, LaunchPlan, StopStep, integration_dir, str_of,
    tool_summary, uuid_v4,
};

/// The hook events ptyro listens to.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "TeammateIdle",
    "TaskCompleted",
    "PreCompact",
    "SessionEnd",
];

pub struct Claude;

impl Claude {
    /// The plugin: `.claude-plugin/plugin.json` and `hooks/hooks.json`.
    fn plugin(ctx: &LaunchCtx<'_>) -> (std::path::PathBuf, Vec<(std::path::PathBuf, String)>) {
        let mut hooks = serde_json::Map::new();
        for ev in EVENTS {
            hooks.insert(
                (*ev).to_owned(),
                // A Stop may run a verified-done check (M7.9): give it time.
                json!([{ "hooks": [{ "type": "command", "command": format!("{} hook claude {ev}", shell_word(ctx.ptyro)), "timeout": match *ev { "Stop" => 900, "PermissionRequest" => 3600, _ => 5 } }] }]),
            );
        }
        let hooks = serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap_or_default();
        let manifest = serde_json::to_string_pretty(&json!({
            "name": "ptyro",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "ptyro session status (installed per session by ptyro; inert outside it)",
        }))
        .unwrap_or_default();
        let dir = integration_dir(ctx.integrations, "claude", &format!("{manifest}{hooks}"));
        let files = vec![
            (dir.join(".claude-plugin/plugin.json"), manifest),
            (dir.join("hooks/hooks.json"), hooks),
        ];
        (dir, files)
    }

    fn common(ctx: &LaunchCtx<'_>, plan: &mut LaunchPlan) {
        plan.argv.extend(["-n".into(), ctx.name.into()]);
        if ctx.def.hooks() {
            let (dir, files) = Self::plugin(ctx);
            plan.argv.extend(["--plugin-dir".into(), dir.to_string_lossy().into_owned()]);
            plan.files.extend(files);
        }
        if ctx.def.usage.unwrap_or(true) {
            let settings = json!({ "statusLine": { "type": "command", "command": format!("{} statusline-relay", shell_word(ctx.ptyro)) } });
            plan.argv.extend(["--settings".into(), settings.to_string()]);
        }
        if let Some(m) = ctx.mcp {
            plan.argv.extend(["--mcp-config".into(), m.into()]);
        }
        // We implement DEC 2026 and OSC 8 (SPEC §8.2).
        plan.env.push(("CLAUDE_CODE_FORCE_SYNC_OUTPUT".into(), "1".into()));
        plan.env.push(("FORCE_HYPERLINK".into(), "1".into()));
    }
}

/// The folder-trust question (verified on 2.1.304 and 2.1.306): "No, exit" is preselected and
/// it has no number keys.
fn trust_dialog(screen: &str) -> bool {
    screen.contains("Is this a project you created or one you trust")
        || screen.contains("Do you trust the files in this folder")
}

/// Dialogs that come before any hook: the folder trust question and the login screen.
fn on_screen(screen: &str) -> Option<Signal> {
    if trust_dialog(screen) {
        return Some(Signal::blocked("question", Some("trust this folder?".into())));
    }
    if screen.contains("Select login method") {
        return Some(Signal::blocked("auth", Some("log in".into())));
    }
    None
}

/// Quote a path for a hook `command` (run by a shell).
pub fn shell_word(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

impl Adapter for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn caps(&self) -> Caps {
        Caps {
            preset_conversation: true,
            learn_conversation: true,
            resume: true,
            hooks: true,
            osc_status: true,
            title_status: false,
            permission_signal: true,
            turn_end_signal: true,
            answer: true,
        }
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String> {
        let id = uuid_v4();
        let mut plan = LaunchPlan {
            argv: ctx.base_argv.clone(),
            conversation: Some(id.clone()),
            ..LaunchPlan::default()
        };
        // A user who pinned a conversation in their args keeps it.
        if !ctx.base_argv.iter().any(|a| {
            a == "--session-id" || a == "--resume" || a == "-r" || a == "--continue" || a == "-c"
        }) {
            plan.argv.extend(["--session-id".into(), id]);
        } else {
            plan.conversation = None;
        }
        Self::common(ctx, &mut plan);
        Ok(plan)
    }

    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String> {
        // `--resume` restores the conversation but none of --plugin-dir/--settings: re-pass them.
        let mut plan = LaunchPlan {
            argv: ctx.base_argv.clone(),
            conversation: Some(conversation.to_owned()),
            ..LaunchPlan::default()
        };
        plan.argv.extend(["--resume".into(), conversation.to_owned()]);
        Self::common(ctx, &mut plan);
        Ok(plan)
    }

    fn on_hook(&self, event: &str, p: &Value) -> Vec<Signal> {
        let mut out = Vec::new();
        if let Some(id) = str_of(p, "session_id") {
            out.push(Signal::Conversation { id: id.to_owned() });
        }
        let tool = || {
            tool_summary(
                str_of(p, "tool_name").unwrap_or("tool"),
                p.get("tool_input").unwrap_or(&Value::Null),
            )
        };
        match event {
            "SessionStart" => out.push(Signal::status(AgentStatus::Idle)),
            "UserPromptSubmit" => {
                out.push(Signal::status(AgentStatus::Working));
                if let Some(text) = str_of(p, "prompt") {
                    out.push(Signal::Prompt { text: text.to_owned() });
                }
            }
            "PreToolUse" => {
                out.push(Signal::status(AgentStatus::Working));
                out.push(Signal::Activity { text: tool() });
            }
            // After an approved tool ran: back to work.
            "PostToolUse" => out.push(Signal::status(AgentStatus::Working)),
            "PermissionRequest" => out.push(Signal::blocked("permission", Some(tool()))),
            // A denial ends the turn without `Stop` (SPEC §8.2).
            "PermissionDenied" => out.push(Signal::status(AgentStatus::Idle)),
            "Notification" => {
                let msg = str_of(p, "message").map(str::to_owned);
                match str_of(p, "notification_type").unwrap_or("") {
                    // Its message is generic ("Claude needs your permission"); PermissionRequest
                    // already said exactly what.
                    "permission_prompt" => out.push(Signal::blocked("permission", None)),
                    "elicitation_dialog" | "agent_needs_input" => {
                        out.push(Signal::blocked("question", msg.clone()))
                    }
                    "agent_completed" => out.push(Signal::TurnEnd),
                    // Delayed by 60 s by default: Stop already said so (SPEC §20.1).
                    "idle_prompt" => {}
                    _ => {}
                }
                if let Some(m) = msg {
                    out.push(Signal::Notify { text: m });
                }
            }
            "Stop" => {
                if let Some(m) = str_of(p, "last_assistant_message") {
                    out.push(Signal::Reply { text: m.to_owned() });
                }
                out.push(Signal::status(AgentStatus::Idle));
                out.push(Signal::TurnEnd);
            }
            "StopFailure" => out.push(Signal::Status {
                status: AgentStatus::Errored,
                kind: str_of(p, "error").map(str::to_owned).or_else(|| Some("error".into())),
                detail: str_of(p, "error_details")
                    .or_else(|| str_of(p, "message"))
                    .map(str::to_owned),
                progress: None,
            }),
            "SubagentStart" | "SubagentStop" => out.push(Signal::Child {
                id: str_of(p, "agent_id").unwrap_or("subagent").to_owned(),
                kind: str_of(p, "agent_type").map(str::to_owned),
                running: event == "SubagentStart",
            }),
            "TeammateIdle" => out.push(Signal::Child {
                id: str_of(p, "teammate_name")
                    .or_else(|| str_of(p, "agent_id"))
                    .unwrap_or("teammate")
                    .to_owned(),
                kind: Some("teammate".into()),
                running: false,
            }),
            "TaskCompleted" => {
                let what = str_of(p, "task_subject")
                    .or_else(|| str_of(p, "task_description"))
                    .unwrap_or("a task");
                out.push(Signal::Activity { text: format!("completed {what}") });
            }
            "PreCompact" => {
                out.push(Signal::Activity { text: "Compacting the conversation".into() })
            }
            _ => {}
        }
        out
    }

    fn ready(&self, screen: &str) -> bool {
        // The prompt box: a `>` or `❯` input line between rules — but `❯` also marks the chosen
        // option of its dialogs, so never while one is up.
        on_screen(screen).is_none()
            && !screen.contains("Do you want to proceed?")
            && screen.lines().any(|l| {
                let t = l.trim_start_matches(['│', ' ']);
                t.starts_with("> ") || t.starts_with("❯ ") || t == ">" || t == "❯"
            })
    }

    fn on_screen(&self, screen: &str) -> Option<Signal> {
        on_screen(screen)
    }

    fn answer(&self, d: Decision) -> Option<Vec<u8>> {
        // The permission dialog: 1 Yes · 2 Yes, don't ask again · Esc No (SPEC §20.1).
        Some(match d {
            Decision::Allow => b"1".to_vec(),
            Decision::Always => b"2".to_vec(),
            Decision::Deny => b"\x1b".to_vec(),
        })
    }

    fn answer_screen(&self, screen: &str, d: Decision) -> Option<Vec<u8>> {
        if !trust_dialog(screen) {
            return self.answer(d);
        }
        // Down to "Yes, I trust this folder", Enter; Esc leaves (and Claude exits).
        Some(match d {
            Decision::Allow | Decision::Always => b"\x1b[B\r".to_vec(),
            Decision::Deny => b"\x1b".to_vec(),
        })
    }

    fn stop(&self) -> Vec<StopStep> {
        vec![
            // Esc ends a running turn; Ctrl-U clears whatever was typed; then /exit.
            StopStep::InterruptKeys(b"\x1b".to_vec()),
            StopStep::Wait(Duration::from_millis(800)),
            StopStep::Keys(b"\x15/exit\r".to_vec()),
            StopStep::Wait(Duration::from_secs(5)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::config::AgentDef;
    use std::path::Path;

    fn fixture(name: &str) -> Value {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-2.1.304/hooks")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn ctx<'a>(def: &'a AgentDef, base: Vec<String>) -> LaunchCtx<'a> {
        LaunchCtx {
            agent: "claude",
            def,
            base_argv: base,
            session: "01abc",
            name: "brisk-rook",
            cwd: Path::new("/w"),
            integrations: Path::new("/data/integrations"),
            ptyro: "/usr/bin/ptyro",
            port: None,
            agent_home: None,
            project_root: None,
            mcp: None,
        }
    }

    #[test]
    fn the_trust_dialog_takes_its_own_keys() {
        let trust = " Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder\n";
        assert_eq!(Claude.answer_screen(trust, Decision::Allow).unwrap(), b"\x1b[B\r");
        assert_eq!(Claude.answer_screen(trust, Decision::Deny).unwrap(), b"\x1b");
        assert_eq!(Claude.answer_screen("Do you want to proceed?", Decision::Allow).unwrap(), b"1");
    }

    #[test]
    fn fixtures_map_to_signals() {
        let c = Claude;
        let perm = c.on_hook("PermissionRequest", &fixture("PermissionRequest"));
        assert!(
            perm.contains(&Signal::Conversation {
                id: "5a3f1e86-5393-4dd9-93ce-49d50dd228f9".into()
            })
        );
        assert!(
            perm.contains(&Signal::blocked(
                "permission",
                Some("Bash: touch hook-marker.txt".into())
            )),
            "{perm:?}"
        );
        let pre = c.on_hook("PreToolUse", &fixture("PreToolUse"));
        assert!(pre.contains(&Signal::Activity { text: "Bash: touch hook-marker.txt".into() }));
        assert!(
            c.on_hook("SessionStart", &fixture("SessionStart"))
                .contains(&Signal::status(AgentStatus::Idle))
        );
        let prompt = c.on_hook("UserPromptSubmit", &fixture("UserPromptSubmit"));
        assert!(prompt.contains(&Signal::status(AgentStatus::Working)));
        assert!(prompt.iter().any(|s| matches!(s, Signal::Prompt { .. })), "{prompt:?}");
        let fail = c.on_hook(
            "StopFailure",
            &json!({"error": "rate_limit", "error_details": "5-hour limit"}),
        );
        assert_eq!(
            fail[0],
            Signal::Status {
                status: AgentStatus::Errored,
                kind: Some("rate_limit".into()),
                detail: Some("5-hour limit".into()),
                progress: None
            }
        );
        let n = c.on_hook(
            "Notification",
            &json!({"notification_type": "agent_needs_input", "message": "Which file?"}),
        );
        assert!(n.contains(&Signal::blocked("question", Some("Which file?".into()))));
        assert!(c.on_hook("Stop", &json!({})).contains(&Signal::TurnEnd));
        // 2.1.308's Stop carries the turn's last message.
        let stop = c.on_hook("Stop", &json!({"last_assistant_message": "All tests pass."}));
        assert!(stop.contains(&Signal::Reply { text: "All tests pass.".into() }));
    }

    #[test]
    fn launch_and_resume_plans() {
        let def = AgentDef::default();
        let plan = Claude.launch(&ctx(&def, vec!["claude".into()])).unwrap();
        let conv = plan.conversation.clone().unwrap();
        let a = &plan.argv;
        assert_eq!(a[0], "claude");
        assert!(a.windows(2).any(|w| w[0] == "--session-id" && w[1] == conv));
        assert!(a.windows(2).any(|w| w[0] == "-n" && w[1] == "brisk-rook"));
        let dir = a.windows(2).find(|w| w[0] == "--plugin-dir").map(|w| w[1].clone()).unwrap();
        assert!(dir.starts_with("/data/integrations/claude/"));
        let hooks = &plan.files.iter().find(|(p, _)| p.ends_with("hooks/hooks.json")).unwrap().1;
        assert!(hooks.contains("/usr/bin/ptyro hook claude PermissionRequest"));
        assert!(a.iter().any(|x| x.contains("statusline-relay")));
        // Resume re-passes the integration and never pre-assigns.
        let r = Claude.resume(&ctx(&def, vec!["claude".into()]), &conv).unwrap();
        assert!(r.argv.windows(2).any(|w| w[0] == "--resume" && w[1] == conv));
        assert!(!r.argv.contains(&"--session-id".to_owned()));
        assert!(r.argv.contains(&"--plugin-dir".to_owned()));
        // Hooks and usage can be turned off.
        let off = AgentDef { hooks: Some(false), usage: Some(false), ..AgentDef::default() };
        let p = Claude.launch(&ctx(&off, vec!["claude".into()])).unwrap();
        assert!(
            !p.argv.contains(&"--plugin-dir".to_owned())
                && !p.argv.contains(&"--settings".to_owned())
        );
        // A user's own --resume wins.
        let p = Claude.launch(&ctx(&def, vec!["claude".into(), "--continue".into()])).unwrap();
        assert!(p.conversation.is_none() && !p.argv.contains(&"--session-id".to_owned()));
    }
}
