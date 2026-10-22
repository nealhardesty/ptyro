//! Codex CLI (SPEC §8.4, verified against 0.143.0).
//!
//! Everything goes through `-c`, nothing is written to `CODEX_HOME`:
//! - one `hooks.<Event>=[…]` per event (the session-flags config layer);
//! - one `hooks.state={…}` pre-trusting exactly those hooks by their hash;
//! - `notify=[ptyro hook codex notify (--chain <the user's notify>)]`;
//! - with `trust_worktrees`, `projects={"<worktree>"={trust_level="trusted"}}` when the project is
//!   already trusted in that Codex home.

use std::path::Path;
use std::time::Duration;

use ptyro_core::status::{AgentStatus, Signal};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::adapter::{
    Adapter, Caps, Decision, LaunchCtx, LaunchPlan, StopStep, str_of, tool_summary,
};
use crate::claude::shell_word;

/// Hook events (Codex's own names) → the snake-case names its trust keys use.
pub const EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PermissionRequest", "permission_request"),
    ("PostToolUse", "post_tool_use"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
    ("SubagentStart", "subagent_start"),
    ("SubagentStop", "subagent_stop"),
];

/// The detail ptyro reports when Codex holds our hooks for review: their trust hash no longer
/// matches this Codex version's (SPEC §8.4), so status falls back to `notify` and the title.
pub const HOOKS_REVIEW: &str = "review its hooks? (ptyro's trust hash didn't match this Codex)";

/// Codex's "hooks need review" screen ([unverified] wording: matched loosely).
fn hooks_need_review(screen: &str) -> bool {
    let l = screen.to_lowercase();
    l.contains("hook")
        && (l.contains("need review") || l.contains("needs review") || l.contains("review hooks"))
}

/// The trust hash Codex pins a command hook with (SPEC §8.4; Codex-internal, golden-tested).
pub fn trust_hash(event_snake: &str, command: &str) -> String {
    let timeout = if event_snake == "session_end" { 1 } else { 600 };
    // Keys written in sorted order, so the compact form is key-sorted even if some dependency
    // turns on serde_json's `preserve_order`.
    let v = json!({ "event_name": event_snake, "hooks": [{ "async": false, "command": command, "timeout": timeout, "type": "command" }] });
    let digest = Sha256::digest(v.to_string().as_bytes());
    format!("sha256:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// A TOML string literal.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// The user's own `notify` program from `<CODEX_HOME>/config.toml`, to chain to.
fn user_notify(home: Option<&Path>) -> Vec<String> {
    let Some(home) = home else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else { return Vec::new() };
    let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
    t.get("notify")
        .and_then(|n| n.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// Whether `root` is trusted in that Codex home.
fn project_trusted(home: Option<&Path>, root: &Path) -> bool {
    let Some(home) = home else { return false };
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else { return false };
    let Ok(t) = text.parse::<toml::Table>() else { return false };
    t.get("projects")
        .and_then(|p| p.get(root.to_string_lossy().as_ref()))
        .and_then(|p| p.get("trust_level"))
        .and_then(|l| l.as_str())
        == Some("trusted")
}

pub struct Codex;

impl Codex {
    fn flags(ctx: &LaunchCtx<'_>) -> Vec<String> {
        let mut a = Vec::new();
        // A lead's MCP servers, as `-c mcp_servers.<name>.…` (config keys, SPEC §20.2).
        if let Some(servers) = ctx.mcp.and_then(|m| serde_json::from_str::<Value>(m).ok()) {
            for (name, def) in servers["mcpServers"].as_object().into_iter().flatten() {
                if let Some(cmd) = def["command"].as_str() {
                    a.extend([
                        "-c".into(),
                        format!("mcp_servers.{name}.command={}", toml_str(cmd)),
                    ]);
                }
                let args: Vec<String> = def["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(toml_str)
                    .collect();
                a.extend(["-c".into(), format!("mcp_servers.{name}.args=[{}]", args.join(","))]);
                let env: Vec<String> = def["env"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| v.as_str().map(|v| format!("{k}={}", toml_str(v))))
                    .collect();
                if !env.is_empty() {
                    a.extend([
                        "-c".into(),
                        format!("mcp_servers.{name}.env={{{}}}", env.join(",")),
                    ]);
                }
            }
        }
        if !ctx.def.hooks() {
            return a;
        }
        let ptyro = shell_word(ctx.ptyro);
        let mut state = Vec::new();
        for (ev, snake) in EVENTS {
            let cmd = format!("{ptyro} hook codex {ev}");
            a.push("-c".into());
            a.push(format!(
                "hooks.{ev}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
                toml_str(&cmd)
            ));
            state.push(format!(
                "{}={{trusted_hash={}}}",
                toml_str(&format!("/<session-flags>/config.toml:{snake}:0:0")),
                toml_str(&trust_hash(snake, &cmd))
            ));
        }
        a.push("-c".into());
        a.push(format!("hooks.state={{{}}}", state.join(",")));
        let mut notify = vec![ctx.ptyro.to_owned(), "hook".into(), "codex".into(), "notify".into()];
        let chain = user_notify(ctx.agent_home.as_deref());
        if !chain.is_empty() {
            notify.push("--chain".into());
            notify.extend(chain);
        }
        a.push("-c".into());
        a.push(format!(
            "notify=[{}]",
            notify.iter().map(|s| toml_str(s)).collect::<Vec<_>>().join(",")
        ));
        if ctx.def.trust_worktrees
            && let Some(root) = ctx.project_root
            && project_trusted(ctx.agent_home.as_deref(), root)
        {
            a.push("-c".into());
            a.push(format!(
                "projects={{{}={{trust_level=\"trusted\"}}}}",
                toml_str(&ctx.cwd.to_string_lossy())
            ));
        }
        a
    }
}

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn caps(&self) -> Caps {
        Caps {
            preset_conversation: false,
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
        let mut argv = ctx.base_argv.clone();
        argv.extend(Self::flags(ctx));
        Ok(LaunchPlan { argv, ..LaunchPlan::default() })
    }

    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String> {
        let mut argv = ctx.base_argv.clone();
        argv.extend(Self::flags(ctx));
        argv.extend(["resume".into(), conversation.to_owned()]);
        Ok(LaunchPlan {
            argv,
            conversation: Some(conversation.to_owned()),
            ..LaunchPlan::default()
        })
    }

    fn on_hook(&self, event: &str, p: &Value) -> Vec<Signal> {
        let mut out = Vec::new();
        if let Some(id) = str_of(p, "session_id").or_else(|| str_of(p, "thread-id")) {
            out.push(Signal::Conversation { id: id.to_owned() });
        }
        let tool = || {
            tool_summary(
                str_of(p, "tool_name").unwrap_or("tool"),
                p.get("tool_input").unwrap_or(&Value::Null),
            )
        };
        match event {
            "SessionStart" | "Interrupt" => out.push(Signal::status(AgentStatus::Idle)),
            "UserPromptSubmit" => {
                out.push(Signal::status(AgentStatus::Working));
                if let Some(t) = str_of(p, "prompt") {
                    out.push(Signal::Prompt { text: t.to_owned() });
                }
            }
            "PreToolUse" => {
                out.push(Signal::status(AgentStatus::Working));
                out.push(Signal::Activity { text: tool() });
            }
            "PostToolUse" => out.push(Signal::status(AgentStatus::Working)),
            "PermissionRequest" => out.push(Signal::blocked("permission", Some(tool()))),
            "Stop" => {
                out.push(Signal::status(AgentStatus::Idle));
                out.push(Signal::TurnEnd);
            }
            // `notify` gets `{type: "agent-turn-complete", thread-id, last-assistant-message, …}`.
            "notify" if str_of(p, "type") == Some("agent-turn-complete") => {
                if let Some(m) = str_of(p, "last-assistant-message") {
                    out.push(Signal::Reply { text: m.to_owned() });
                }
                out.push(Signal::status(AgentStatus::Idle));
                out.push(Signal::TurnEnd);
                if let Some(m) = str_of(p, "last-assistant-message") {
                    out.push(Signal::Notify {
                        text: m.lines().next().unwrap_or(m).chars().take(200).collect(),
                    });
                }
            }
            "SubagentStart" | "SubagentStop" => out.push(Signal::Child {
                id: str_of(p, "agent_id")
                    .or_else(|| str_of(p, "subagent_id"))
                    .unwrap_or("subagent")
                    .to_owned(),
                kind: str_of(p, "agent_type").map(str::to_owned),
                running: event == "SubagentStart",
            }),
            _ => {}
        }
        out
    }

    fn on_title(&self, title: &str) -> Option<Signal> {
        // `tui.terminal_title` puts a braille spinner in front while a turn runs.
        let first = title.trim_start().chars().next()?;
        ('\u{2800}'..='\u{28ff}').contains(&first).then(|| Signal::status(AgentStatus::Working))
    }

    fn on_screen(&self, screen: &str) -> Option<Signal> {
        if screen.contains("Do you trust") {
            Some(Signal::blocked("question", Some("trust this folder?".into())))
        } else if screen.contains("Update available") {
            Some(Signal::blocked("question", Some("update now or skip?".into())))
        } else if screen.contains("Sign in with ChatGPT") {
            Some(Signal::blocked("auth", Some("log in".into())))
        } else if hooks_need_review(screen) {
            Some(Signal::blocked("question", Some(HOOKS_REVIEW.into())))
        } else {
            None
        }
    }

    fn ready(&self, screen: &str) -> bool {
        // The composer line; the update prompt and trust dialog come before it (SPEC §8.4).
        screen.lines().any(|l| l.trim_start().starts_with("› ") || l.trim() == "›")
            && !screen.contains("Update available")
            && !screen.contains("Do you trust")
    }

    fn answer(&self, d: Decision) -> Option<Vec<u8>> {
        // [unverified] The approval overlay: y yes · a always · Esc no.
        Some(match d {
            Decision::Allow => b"y".to_vec(),
            Decision::Always => b"a".to_vec(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::config::AgentDef;

    #[test]
    fn trust_hashes_match_codex_bit_for_bit() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex-0.143.0/trust-hashes.json");
        let f: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        let cmd = f["command"].as_str().unwrap();
        for v in f["vectors"].as_array().unwrap() {
            let ev = v["event_name"].as_str().unwrap();
            assert_eq!(trust_hash(ev, cmd), v["trusted_hash"].as_str().unwrap(), "{ev}");
        }
    }

    #[test]
    fn flags_and_chain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "notify = [\"my-notify\", \"--x\"]\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();
        let def = AgentDef { trust_worktrees: true, ..AgentDef::default() };
        let ctx = LaunchCtx {
            agent: "codex",
            def: &def,
            base_argv: vec!["codex".into()],
            session: "s",
            name: "n",
            cwd: Path::new("/wt/one"),
            integrations: Path::new("/i"),
            ptyro: "/bin/ptyro",
            port: None,
            agent_home: Some(dir.path().to_path_buf()),
            project_root: Some(Path::new("/repo")),
            mcp: None,
        };
        let a = Codex.launch(&ctx).unwrap().argv;
        assert!(a.iter().any(|x| x == "hooks.PermissionRequest=[{hooks=[{type=\"command\",command=\"/bin/ptyro hook codex PermissionRequest\"}]}]"), "{a:?}");
        let state = a.iter().find(|x| x.starts_with("hooks.state=")).unwrap();
        assert!(
            state.contains(
                "\"/<session-flags>/config.toml:permission_request:0:0\"={trusted_hash=\"sha256:"
            ),
            "{state}"
        );
        let notify = a.iter().find(|x| x.starts_with("notify=")).unwrap();
        assert_eq!(
            notify,
            "notify=[\"/bin/ptyro\",\"hook\",\"codex\",\"notify\",\"--chain\",\"my-notify\",\"--x\"]"
        );
        assert!(a.iter().any(|x| x == "projects={\"/wt/one\"={trust_level=\"trusted\"}}"));
        let r = Codex.resume(&ctx, "0199-abc").unwrap().argv;
        assert_eq!(&r[r.len() - 2..], ["resume", "0199-abc"]);
    }

    #[test]
    fn hooks_and_notify() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex-0.143.0/hooks/SessionStart.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        let s = Codex.on_hook("SessionStart", &v);
        assert!(
            s.contains(&Signal::Conversation { id: "01a15792-d764-7782-832b-0fa3ff006082".into() })
        );
        let n = Codex.on_hook("notify", &json!({"type": "agent-turn-complete", "thread-id": "t1", "last-assistant-message": "All tests pass.\nDetails"}));
        assert!(
            n.contains(&Signal::TurnEnd)
                && n.contains(&Signal::Notify { text: "All tests pass.".into() })
                && n.contains(&Signal::Reply { text: "All tests pass.\nDetails".into() })
        );
        assert_eq!(Codex.on_title("⠋ codex"), Some(Signal::status(AgentStatus::Working)));
        assert_eq!(Codex.on_title("codex"), None);
        assert!(
            Codex.ready("  › Ask Codex to do anything")
                && !Codex.ready("Update available!\n› 1. Update now")
        );
    }
}
