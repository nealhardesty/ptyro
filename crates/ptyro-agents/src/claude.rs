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
    Adapter, Caps, Decision, LaunchCtx, LaunchPlan, StopStep, integration_dir, model_in, str_of,
    tool_summary, transcript_model, uuid_v4,
};
use crate::transcript::{Transcript, TranscriptAt};

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
                // A permission prompt may wait for a phone's answer (M9.5): give it time.
                json!([{ "hooks": [hook_entry(ctx.ptyro, &format!("hook claude {ev}"), if *ev == "PermissionRequest" { 3600 } else { 5 })] }]),
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
            let settings = json!({ "statusLine": { "type": "command", "command": format!("{} statusline-relay", hook_word(ctx.ptyro)) } });
            plan.argv.extend(["--settings".into(), settings.to_string()]);
        }
        // The knowledge base's tools (SPEC §25.9, §20.1): `ptyro mcp` beside the user's own
        // servers (no `--strict-mcp-config`), and its tools allowed, which adds to the user's
        // rules and leaves their permission mode alone. Claude restores neither on a resume:
        // both are here, so on every launch.
        if let Some(socket) = ctx.kb {
            let env: serde_json::Map<String, Value> = crate::adapter::kb_env(ctx, socket)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), Value::String(v)))
                .collect();
            let servers = json!({ "mcpServers": { "ptyro": {
                "type": "stdio", "command": ctx.ptyro, "args": ["mcp"], "env": env,
            } } });
            let allowed: Vec<String> =
                crate::adapter::KB_TOOLS.iter().map(|t| format!("mcp__ptyro__{t}")).collect();
            plan.argv.extend([
                "--mcp-config".into(),
                servers.to_string(),
                "--allowedTools".into(),
                allowed.join(","),
            ]);
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

/// An input line's text: `❯ text`, `> text` (inside the old box's `│`), or `""` for an empty one.
fn input_line(l: &str) -> Option<String> {
    let t = l.trim_start_matches(['│', ' ']);
    let rest = ["❯ ", "> "]
        .iter()
        .find_map(|m| t.strip_prefix(m))
        .or((t == "❯" || t == ">").then_some(""))?;
    Some(rest.trim_end_matches(['│', ' ']).to_owned())
}

/// The first-run question when Claude finds its Chrome extension (2.1.318, seen on Windows,
/// 2026-11-04): "No, keep browser tools off" is preselected.
fn chrome_dialog(screen: &str) -> bool {
    screen.contains("Claude in Chrome extension detected")
}

/// Dialogs that come before any hook: the folder trust question, the Chrome extension question
/// and the login screen.
fn on_screen(screen: &str) -> Option<Signal> {
    if trust_dialog(screen) {
        return Some(Signal::blocked("question", Some("trust this folder?".into())));
    }
    if chrome_dialog(screen) {
        return Some(Signal::blocked("question", Some("use your Chrome browser?".into())));
    }
    if screen.contains("Select login method") {
        return Some(Signal::blocked("auth", Some("log in".into())));
    }
    None
}

/// Quote a path for a hook `command` (run by a shell).
pub fn shell_word(s: &str) -> String {
    if plain(s) { s.to_owned() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

/// Nothing in it that any shell reads specially.
fn plain(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c))
}

/// ptyro's path as the program of a hook or status line `command`, which the agent runs in a
/// shell: [`shell_word`].
pub fn hook_word(ptyro: &str) -> String {
    shell_word(ptyro)
}

/// A Claude hook running `ptyro <args>`.
fn hook_entry(ptyro: &str, args: &str, timeout: u64) -> Value {
    json!({ "type": "command", "command": format!("{} {args}", hook_word(ptyro)), "timeout": timeout })
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
        // Only `SessionStart` names the model; what ran each turn is in the transcript, so a
        // `/model` switch shows once the turn that used it ends.
        out.extend(model_in(p));
        if event == "Stop" {
            out.extend(str_of(p, "transcript_path").and_then(transcript_model));
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
        // The prompt box: a `>` or `❯` input line right under a rule (`────`, or the old box's
        // `╭───`). Not a `❯ …` line of the conversation above it, which a resumed Claude shows
        // while it still loads, nor the chosen option of a dialog.
        let input = |l: &str| input_line(l).is_some();
        let rule = |l: &str| l.trim_start().starts_with(['─', '╭']);
        let lines: Vec<&str> = screen.lines().collect();
        // The last box: a dialog takes its place, so one counts only from there down (above it
        // is the conversation, answered dialogs included).
        let Some(at) = lines.windows(2).rposition(|w| rule(w[0]) && input(w[1])) else {
            return false;
        };
        let below = lines[at..].join("\n");
        on_screen(&below).is_none() && !below.contains("Do you want to proceed?")
    }

    fn ready_needs_screen(&self) -> bool {
        true
    }

    fn pending_input(&self, screen: &str) -> Option<String> {
        let lines: Vec<&str> = screen.lines().collect();
        let rule = |l: &str| l.trim_start().starts_with(['─', '╭']);
        let at = lines.windows(2).rposition(|w| rule(w[0]) && input_line(w[1]).is_some())?;
        input_line(lines[at + 1]).filter(|t| !t.is_empty())
    }

    fn on_screen(&self, screen: &str) -> Option<Signal> {
        on_screen(screen)
    }

    fn screen_status(&self, screen: &str) -> Option<Signal> {
        if let Some(s) = on_screen(screen) {
            return Some(s);
        }
        if screen.contains("Do you want to proceed?") || screen.contains("Do you want to make this")
        {
            // The dialog's first line names the tool: "Bash command", "Edit file".
            let what = screen
                .lines()
                .map(|l| l.trim_matches(['│', ' ', '╭', '╰', '─']))
                .find(|l| l.ends_with("command") || l.ends_with("file"))
                .map(str::to_owned);
            return Some(Signal::blocked("permission", what));
        }
        Some(if screen.to_lowercase().contains("esc to interrupt") {
            Signal::status(AgentStatus::Working)
        } else {
            Signal::status(AgentStatus::Idle)
        })
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
        if chrome_dialog(screen) {
            // Down to "Yes, use my browser", Enter; Enter alone keeps the tools off.
            return Some(match d {
                Decision::Allow | Decision::Always => b"\x1b[B\r".to_vec(),
                Decision::Deny => b"\r".to_vec(),
            });
        }
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

    fn transcript(&self, at: &TranscriptAt<'_>) -> Option<std::path::PathBuf> {
        crate::transcript::claude_path(at)
    }

    fn hook_transcript(&self, event: &str, p: &Value) -> (Option<std::path::PathBuf>, bool) {
        // (A compaction fires neither: its lines are read with the next turn's.)
        (str_of(p, "transcript_path").map(Into::into), matches!(event, "Stop" | "SessionEnd"))
    }

    fn read_transcript(&self, bytes: &[u8], from: u64) -> Option<Transcript> {
        Some(crate::transcript::claude(bytes, from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::config::AgentDef;
    use std::path::Path;

    #[test]
    fn a_hookless_claude_s_screen_says_working_blocked_or_idle() {
        let c = Claude;
        let st = |screen: &str| match c.screen_status(screen) {
            Some(Signal::Status { status, kind, detail, .. }) => (status, kind, detail),
            other => panic!("{other:?}"),
        };
        assert_eq!(st("✻ Pondering… (esc to interrupt · 12s)").0, AgentStatus::Working);
        assert_eq!(st("╭──╮\n│ > │\n╰──╯\n  ? for shortcuts").0, AgentStatus::Idle);
        let (s, kind, what) =
            st("│ Bash command │\n│   npm install │\n│ Do you want to proceed? │\n│ ❯ 1. Yes │");
        assert_eq!(
            (s, kind.as_deref(), what.as_deref()),
            (AgentStatus::Blocked, Some("permission"), Some("Bash command"))
        );
        assert_eq!(st("Do you trust the files in this folder?").0, AgentStatus::Blocked);
    }

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
            kb: None,
        }
    }

    /// SPEC §25.9, §20.1: the knowledge base's tools, at the launch and again on a resume
    /// (Claude restores neither flag), and none when the daemon gives none.
    #[test]
    fn the_knowledge_tools_are_given_at_launch_and_on_every_resume() {
        let def = AgentDef::default();
        let with = LaunchCtx {
            kb: Some(Path::new("/run/ptyro.sock")),
            ..ctx(&def, vec!["claude".into()])
        };
        for plan in [Claude.launch(&with).unwrap(), Claude.resume(&with, "c1").unwrap()] {
            let a = &plan.argv;
            let at = a.iter().position(|x| x == "--mcp-config").expect("--mcp-config");
            let cfg: Value = serde_json::from_str(&a[at + 1]).unwrap();
            assert_eq!(
                cfg,
                json!({ "mcpServers": { "ptyro": { "type": "stdio", "command": "/usr/bin/ptyro",
                    "args": ["mcp"],
                    "env": { "PTYRO_SESSION_ID": "01abc", "PTYRO_SOCKET": "/run/ptyro.sock" } } } })
            );
            let at = a.iter().position(|x| x == "--allowedTools").expect("--allowedTools");
            assert_eq!(a[at + 1], "mcp__ptyro__recall,mcp__ptyro__recall_read");
            // Beside the user's own servers and rules: nothing that replaces them.
            assert!(!a.iter().any(|x| x == "--strict-mcp-config" || x == "--permission-mode"));
        }
        let none = ctx(&def, vec!["claude".into()]);
        for plan in [Claude.launch(&none).unwrap(), Claude.resume(&none, "c1").unwrap()] {
            assert!(!plan.argv.iter().any(|x| x == "--mcp-config" || x == "--allowedTools"));
        }
    }

    /// A hook runs ptyro by its path in POSIX quoting, with no shell named.
    #[test]
    fn hook_commands_quote_ptyros_path() {
        let e = hook_entry("/opt/my apps/ptyro", "hook claude Stop", 5);
        assert_eq!(e["command"], "'/opt/my apps/ptyro' hook claude Stop");
        assert!(e.get("shell").is_none());
    }

    /// The input box is the `❯` line under a rule. A resumed Claude prints the conversation
    /// (its `❯` prompts) before the box: not ready yet (seen on Windows, 2026-11-04: an Enter
    /// typed then was lost).
    #[test]
    fn ready_means_the_input_box_not_a_prompt_in_the_conversation() {
        let loading = "❯ Remember the word BANANA. Reply with just OK.\n● OK\n✻ Baked for 1s\n";
        assert!(!Claude.ready(loading));
        let drawn =
            format!("{loading}──────────────── c1 ─\n❯ \n────────────────\n  ⏵⏵ auto mode on");
        assert!(Claude.ready(&drawn));
        assert!(Claude.ready("╭──────────╮\n│ > │\n╰──────────╯"), "the older box");
        let dialog = "──────────\n Do you want to proceed?\n ❯ 1. Yes\n   2. No";
        assert!(!Claude.ready(dialog));
        // A dialog answered earlier, above the box now: ready.
        assert!(Claude.ready(&format!("{dialog}\n● answer: yes\n{drawn}")));
        assert!(Claude.ready_needs_screen());
    }

    /// As Claude 2.1.318 drew it on Windows, right after the trust question.
    #[test]
    fn the_chrome_extension_question_blocks_and_takes_its_own_keys() {
        let screen = "  Claude in Chrome extension detected\n  Claude will use your Chrome browser by \
            default — navigating sites, filling forms, and capturing screenshots in your\n  existing \
            session.\n  ❯ No, keep browser tools off\n    Yes, use my browser\n  Enter to confirm · \
            Esc to keep browser tools off";
        let s = on_screen(screen).unwrap();
        assert!(
            matches!(&s, Signal::Status { status: AgentStatus::Blocked, detail: Some(d), .. } if d.contains("Chrome")),
            "{s:?}"
        );
        assert!(!Claude.ready(screen));
        assert_eq!(Claude.answer_screen(screen, Decision::Deny).unwrap(), b"\r");
        assert_eq!(Claude.answer_screen(screen, Decision::Allow).unwrap(), b"\x1b[B\r");
    }

    #[test]
    fn what_waits_in_the_input_box() {
        let unsent =
            "❯ earlier prompt\n● ok\n──────── c1 ─\n❯ Reply with PONG\n────────\n  ⏵⏵ auto";
        assert_eq!(Claude.pending_input(unsent).as_deref(), Some("Reply with PONG"));
        let sent = "❯ Reply with PONG\n● PONG\n──────── c1 ─\n❯ \n────────";
        assert_eq!(Claude.pending_input(sent), None, "the box is empty: it was taken");
        assert_eq!(Claude.pending_input("│ > hello │"), None, "no rule above: not the box");
        assert_eq!(Claude.pending_input("╭────╮\n│ > hello   │\n╰────╯").as_deref(), Some("hello"));
    }

    #[test]
    fn the_trust_dialog_takes_its_own_keys() {
        let trust = " Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder\n";
        assert_eq!(Claude.answer_screen(trust, Decision::Allow).unwrap(), b"\x1b[B\r");
        assert_eq!(Claude.answer_screen(trust, Decision::Deny).unwrap(), b"\x1b");
        assert_eq!(Claude.answer_screen("Do you want to proceed?", Decision::Allow).unwrap(), b"1");
    }

    #[test]
    fn the_model_comes_from_session_start_and_then_from_the_transcript() {
        let start = Claude.on_hook("SessionStart", &fixture("SessionStart"));
        assert!(start.contains(&Signal::Model { id: "claude-opus-5-5".into() }), "{start:?}");
        // Other events carry none, and a path that isn't a readable transcript adds none.
        let pre = Claude.on_hook("PreToolUse", &fixture("PreToolUse"));
        assert!(!pre.iter().any(|s| matches!(s, Signal::Model { .. })));
        // A turn that ended after `/model sonnet`: the last real assistant turn says so.
        let dir = std::env::temp_dir().join(format!("ptyro-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("t.jsonl");
        std::fs::write(
            &t,
            [
                r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[]}}"#,
                r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5-5","content":[]}}"#,
                r#"{"type":"assistant","message":{"model":"<synthetic>","content":[]}}"#,
                "not json",
            ]
            .join("\n"),
        )
        .unwrap();
        let stop = Claude
            .on_hook("Stop", &json!({"session_id": "s", "transcript_path": t.to_string_lossy()}));
        assert!(stop.contains(&Signal::Model { id: "claude-sonnet-5-5".into() }), "{stop:?}");
        let gone = Claude.on_hook("Stop", &json!({"transcript_path": "/nope/x.jsonl"}));
        assert!(!gone.iter().any(|s| matches!(s, Signal::Model { .. })));
        let odd = Claude.on_hook("Stop", &json!({"transcript_path": "relative.jsonl"}));
        assert!(!odd.iter().any(|s| matches!(s, Signal::Model { .. })));
        let _ = std::fs::remove_dir_all(dir);
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
        assert!(std::path::Path::new(&dir).starts_with("/data/integrations/claude"), "{dir}");
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
