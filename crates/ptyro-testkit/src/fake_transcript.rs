//! The fake agent's transcript (`PTYRO_FAKE_TRANSCRIPT=1`): the file a real Claude Code or
//! Codex keeps of its conversation, written where and how the real one writes it, so that
//! the knowledge base's readers are tested end to end with no agent installed (SPEC §25.6.1;
//! the formats are §20.8's, after the fixtures in `crates/ptyro-agents/tests/fixtures/`).
//!
//! - **claude**: `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/<cwd, dashed>/<session id>.jsonl`.
//!   Besides the `user` and `assistant` lines it writes what a reader must *not* take: a
//!   `session-owner` line, an `attachment` with an account's email, the prompt again in
//!   `queue-operation` and `last-prompt` lines, `thinking`, and tool results. The turn's last
//!   assistant line is written `PTYRO_FAKE_TRANSCRIPT_LAG_MS` (default 15) after `Stop`
//!   fires, as the real one's is.
//! - **codex**: `<CODEX_HOME or ~/.codex>/sessions/2026/11/19/rollout-…-<thread id>.jsonl`:
//!   `session_meta`, Codex's own context messages, `response_item` messages, every tool as an
//!   `exec` call with what ran in an `event_msg` `item_completed` line, and the messages again
//!   as events.
//!
//! A resumed session appends to the file it finds.

use std::cell::Cell;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

/// An account's email, in a line no reader may take.
pub const ACCOUNT: &str = "fake-account@example.com";
/// What a tool printed: never kept.
pub const RESULT_MARK: &str = "FAKE-TOOL-RESULT";

pub struct Script {
    claude: bool,
    session: String,
    pub path: PathBuf,
    cwd: String,
    n: Cell<u64>,
    /// How long after `Stop` fires Claude's last line is written.
    pub lag: Duration,
    /// The file's length when this process's first turn ended (`rewind` goes back to it).
    first_turn: Cell<Option<u64>>,
}

/// `2026-11-19T22:38:48.772Z`.
fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        ms % 1000
    )
}

fn now() -> String {
    iso(ptyro_core::now_ms())
}

fn append(path: &std::path::Path, lines: &[Value]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

impl Script {
    /// The transcript of `session`, when the knob asks for one and `adapter` keeps one.
    pub fn open(adapter: &str, session: &str) -> Option<Self> {
        std::env::var_os("PTYRO_FAKE_TRANSCRIPT")?;
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let dir = |var: &str, default: &str| {
            std::env::var_os(var).map_or_else(|| home.join(default), PathBuf::from)
        };
        let cwd = std::env::current_dir().ok()?.to_string_lossy().into_owned();
        let (path, head) = match adapter {
            "claude" => {
                let slug: String =
                    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
                let path = dir("CLAUDE_CONFIG_DIR", ".claude")
                    .join("projects")
                    .join(slug)
                    .join(format!("{session}.jsonl"));
                let head = vec![
                    json!({ "type": "session-owner", "accountUuid": "0000", "sessionId": session }),
                    json!({ "type": "attachment", "uuid": format!("att-{session}"),
                            "attachment": { "type": "session_context", "email": ACCOUNT } }),
                ];
                (path, head)
            }
            "codex" => {
                let day = dir("CODEX_HOME", ".codex").join("sessions/2026/11/19");
                let end = format!("-{session}.jsonl");
                let old = std::fs::read_dir(&day).ok().and_then(|d| {
                    d.flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with(&end))
                });
                let path =
                    old.unwrap_or_else(|| day.join(format!("rollout-2026-11-19T00-00-00{end}")));
                let context = |role: &str, kind: &str, text: &str| {
                    json!({ "timestamp": now(), "type": "response_item", "payload": {
                        "type": "message", "id": format!("msg_ctx_{kind}"), "role": role,
                        "content": [{ "type": "input_text", "text": text }],
                        "internal_chat_message_metadata_passthrough": { "content_item_kinds": [kind] } } })
                };
                let head = vec![
                    json!({ "timestamp": now(), "type": "session_meta", "payload": {
                        "id": session, "session_id": session, "cwd": cwd,
                        "creator_account_id": ACCOUNT } }),
                    context("developer", "model.base_instructions", "You are a fake Codex."),
                    context(
                        "user",
                        "agents_md.instructions",
                        "# AGENTS.md\nthe fake's own context",
                    ),
                    json!({ "timestamp": now(), "type": "turn_context", "payload": {
                        "cwd": cwd, "model": "gpt-5.6-sol" } }),
                ];
                (path, head)
            }
            _ => return None,
        };
        let s = Self {
            claude: adapter == "claude",
            session: session.to_owned(),
            path,
            cwd,
            n: Cell::new(0),
            lag: Duration::from_millis(
                std::env::var("PTYRO_FAKE_TRANSCRIPT_LAG_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(15),
            ),
            first_turn: Cell::new(None),
        };
        if !s.path.exists() {
            if let Some(d) = s.path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            append(&s.path, &head);
        }
        Some(s)
    }

    /// An id no other line has, in this process or one before it.
    fn id(&self) -> String {
        self.n.set(self.n.get() + 1);
        format!(
            "{:012x}-{:04x}-{:04}",
            ptyro_core::now_ms(),
            std::process::id() & 0xffff,
            self.n.get()
        )
    }

    /// A Claude line of `kind` with `content` (and whatever `more` adds).
    fn claude_line(&self, kind: &str, content: Value, more: Value) -> Value {
        let mut v = json!({
            "parentUuid": null, "isSidechain": false, "type": kind, "uuid": self.id(),
            "timestamp": now(), "sessionId": self.session, "cwd": self.cwd,
            "message": { "role": kind, "content": content },
        });
        if kind == "assistant" {
            v["message"]["model"] = json!("claude-opus-5-5");
        }
        for (k, x) in more.as_object().into_iter().flatten() {
            v[k] = x.clone();
        }
        v
    }

    fn codex_item(&self, payload: Value) -> Value {
        json!({ "timestamp": now(), "type": "response_item", "payload": payload })
    }

    fn codex_event(&self, item: Value) -> Value {
        json!({ "timestamp": now(), "type": "event_msg",
                "payload": { "type": "item_completed", "thread_id": self.session, "item": item } })
    }

    /// The user's prompt.
    pub fn user(&self, prompt: &str) {
        let lines = if self.claude {
            vec![
                json!({ "type": "queue-operation", "operation": "enqueue", "content": prompt }),
                self.claude_line("user", json!(prompt), json!({})),
                json!({ "type": "last-prompt", "lastPrompt": prompt, "sessionId": self.session }),
            ]
        } else {
            vec![
                self.codex_item(json!({
                    "type": "message", "id": format!("msg_{}", self.id()), "role": "user",
                    "content": [{ "type": "input_text", "text": prompt }],
                    "internal_chat_message_metadata_passthrough": { "content_item_kinds": ["user.text"] } })),
                self.codex_event(json!({ "type": "UserMessage", "id": self.id(),
                    "content": [{ "type": "text", "text": prompt }] })),
            ]
        };
        append(&self.path, &lines);
    }

    /// What a prompt's hook added to the model's context (SPEC §25.9), where the real agents
    /// keep it **[verified 2026-11-20, Claude Code 2.1.329 and Codex 0.167.1; `make verify-kb
    /// ARGS=--offer`]**: Claude in an `attachment` of type `hook_additional_context` (its
    /// `content` a list, and again under `rendered`), Codex in a `developer` message. A reader
    /// must take neither for what the user or the agent said.
    pub fn context(&self, event: &str, text: &str) {
        let line = if self.claude {
            json!({ "type": "attachment", "sessionId": self.session,
                "attachment": { "type": "hook_additional_context", "content": [text],
                    "hookName": event, "hookEvent": event },
                "rendered": [{ "content": text }] })
        } else {
            self.codex_item(json!({
                "type": "message", "id": format!("msg_{}", self.id()), "role": "developer",
                "content": [{ "type": "input_text", "text": text }] }))
        };
        append(&self.path, &[line]);
    }

    /// A tool that ran: Claude's `tool_use` and its result, Codex's `exec` call, what it
    /// really ran, and its output.
    pub fn tool(&self, name: &str, input: &Value, output: &str) {
        let result = format!("{RESULT_MARK} {output}");
        let call = format!("call_{}", self.id());
        let lines = if self.claude {
            vec![
                self.claude_line(
                    "assistant",
                    json!([{ "type": "thinking", "thinking": "", "signature": "fake" }]),
                    json!({}),
                ),
                self.claude_line(
                    "assistant",
                    json!([{ "type": "tool_use", "id": call, "name": name, "input": input }]),
                    json!({}),
                ),
                self.claude_line(
                    "user",
                    json!([{ "type": "tool_result", "tool_use_id": call, "content": result }]),
                    json!({}),
                ),
            ]
        } else {
            let item = match input["file_path"].as_str() {
                Some(path) => json!({ "type": "FileChange", "id": format!("exec-{}", self.id()),
                    "changes": { path: { "type": "add", "content": input["content"] } },
                    "status": "completed" }),
                None => json!({ "type": "CommandExecution", "id": format!("exec-{}", self.id()),
                    "command": ["/bin/sh", "-lc", input["command"]], "exit_code": 0,
                    "aggregated_output": result }),
            };
            vec![
                self.codex_item(
                    json!({ "type": "custom_tool_call", "id": format!("ctc_{}", self.id()),
                    "call_id": call, "name": "exec",
                    "input": format!("const r = await tools.{name}({input});\ntext(r);\n") }),
                ),
                self.codex_event(item),
                self.codex_item(json!({ "type": "custom_tool_call_output", "call_id": call,
                    "output": [{ "type": "input_text", "text": result }] })),
            ]
        };
        append(&self.path, &lines);
    }

    /// The turn's last assistant message, as lines.
    fn reply_lines(&self, msg: &str) -> Vec<Value> {
        if self.claude {
            return vec![self.claude_line(
                "assistant",
                json!([{ "type": "text", "text": msg }]),
                json!({}),
            )];
        }
        let id = format!("msg_{}", self.id());
        vec![
            self.codex_event(json!({ "type": "AgentMessage", "id": id,
                "content": [{ "type": "Text", "text": msg }], "phase": "final_answer" })),
            self.codex_item(json!({ "type": "message", "id": id, "role": "assistant",
                "content": [{ "type": "output_text", "text": msg }], "phase": "final_answer",
                "internal_chat_message_metadata_passthrough": { "content_item_kinds": ["unknown"] } })),
        ]
    }

    /// The turn's reply, written now (Codex has it in the file before `Stop`).
    pub fn reply(&self, msg: &str) {
        append(&self.path, &self.reply_lines(msg));
        self.turn_ended();
    }

    /// The turn's reply, written [`Script::lag`] from now on another thread (Claude writes
    /// it after `Stop` has fired): join the handle once the hook is back.
    pub fn reply_late(&self, msg: &str) -> std::thread::JoinHandle<()> {
        let (path, lines, lag) = (self.path.clone(), self.reply_lines(msg), self.lag);
        std::thread::spawn(move || {
            std::thread::sleep(lag);
            append(&path, &lines);
        })
    }

    /// A turn is all in the file.
    pub fn turn_ended(&self) {
        if self.first_turn.get().is_none() {
            self.first_turn.set(std::fs::metadata(&self.path).ok().map(|m| m.len()));
        }
    }

    /// `/compact`, as Claude writes it (and fires no `Stop`): the boundary, the summary the
    /// model is given, then the command's own lines.
    pub fn compact(&self, summary: &str) {
        if !self.claude {
            return;
        }
        let text = format!(
            "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\nSummary:\n1. Primary Request and Intent:\n   {summary}\n\nIf you need specific details from before compaction (like exact code snippets, error messages, or content you generated), read the full transcript at: {}\nContinue the conversation from where it left off without asking the user any further questions.",
            self.path.display()
        );
        append(
            &self.path,
            &[
                json!({ "type": "system", "subtype": "compact_boundary", "uuid": self.id(),
                        "content": "Conversation compacted", "timestamp": now() }),
                self.claude_line(
                    "user",
                    json!(text),
                    json!({ "isCompactSummary": true, "isVisibleInTranscriptOnly": true }),
                ),
                self.claude_line(
                    "user",
                    json!("<local-command-caveat>not sent to the model</local-command-caveat>"),
                    json!({ "isMeta": true }),
                ),
                self.claude_line(
                    "user",
                    json!("<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>"),
                    json!({}),
                ),
                self.claude_line(
                    "user",
                    json!("<local-command-stdout>Compacted</local-command-stdout>"),
                    json!({}),
                ),
            ],
        );
    }

    /// Back to the end of this process's first turn: the file shrinks, and what follows is
    /// written after it.
    pub fn rewind(&self) {
        if let Some(len) = self.first_turn.get()
            && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&self.path)
        {
            let _ = f.set_len(len);
        }
    }
}
