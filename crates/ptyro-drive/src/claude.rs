//! Claude Code over stream-json (M10.3): `claude -p --input-format stream-json --output-format
//! stream-json --verbose --include-partial-messages --permission-prompt-tool stdio`.
//! Verified on 2.1.305 (one real turn, fixture in tests/fixtures/claude-2.1.305.jsonl):
//! - in: `{"type":"user","message":{"role":"user","content":TEXT}}`, one per line;
//! - out: `system/init` (the session id), `stream_event` (partial text), `assistant`
//!   (text, thinking, `tool_use`), `user` (`tool_result`), `result` (the turn's end, its cost);
//! - permissions: `control_request` `can_use_tool` → our `control_response` with
//!   `{behavior: allow, updatedInput}` or `{behavior: deny, message}`;
//! - `control_request` `interrupt` ends a turn.

use std::collections::BTreeMap;

use ptyro_agents::Decision;
use ptyro_agents::adapter::tool_summary;
use ptyro_core::chat::{ChatBody, NoticeLevel, PermOption, ToolStatus};
use ptyro_core::status::{AgentStatus, Signal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{item, notice, one_line, status, text_of};
use crate::{Driver, Effect, Opts};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Pending {
    request: String,
    tool: String,
    input: Value,
    suggestions: Option<Value>,
    item: String,
    /// What the request is about (shown again when it comes back to the top).
    #[serde(default)]
    detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Claude {
    session: Option<String>,
    /// The streaming message: its id, and each content block's kind and text so far.
    message: Option<String>,
    blocks: BTreeMap<u64, (String, String)>,
    /// tool_use id → (name, title, diff, files), for results.
    tools: BTreeMap<String, (String, String, Option<String>, Vec<String>)>,
    pending: Option<Pending>,
    /// Requests that came while another was pending (parallel tool calls): each is answered
    /// in turn, newest first, never dropped.
    #[serde(default)]
    waiting: Vec<Pending>,
    n: u64,
}

impl Claude {
    pub fn new(opts: Opts) -> Self {
        Self { session: opts.resume.or(opts.session_id), ..Self::default() }
    }

    pub fn restore(v: Value) -> Option<Self> {
        serde_json::from_value(v).ok()
    }

    fn id(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}{}", self.n)
    }

    fn assistant(&mut self, m: &Value) -> Vec<Effect> {
        let mut out = Vec::new();
        let mid = m["message"]["id"].as_str().unwrap_or("msg").to_owned();
        let sub = m["parent_tool_use_id"].as_str().is_some();
        for (i, block) in m["message"]["content"].as_array().into_iter().flatten().enumerate() {
            let id = format!("{mid}:{i}");
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("").to_owned();
                    if !text.is_empty() && !sub {
                        out.push(item(id, ChatBody::Assistant { text, streaming: false }));
                    }
                }
                Some("thinking") => {
                    let text = block["thinking"].as_str().unwrap_or("").to_owned();
                    if !text.is_empty() {
                        out.push(item(id, ChatBody::Thinking { text }));
                    }
                }
                Some("tool_use") => {
                    let tid = block["id"].as_str().unwrap_or(&id).to_owned();
                    let name = block["name"].as_str().unwrap_or("tool").to_owned();
                    let input = &block["input"];
                    let title = title(&name, input);
                    let files = files(input);
                    let diff = edit_diff(input);
                    self.tools.insert(
                        tid.clone(),
                        (name.clone(), title.clone(), diff.clone(), files.clone()),
                    );
                    out.push(Effect::Signal(Signal::Activity { text: tool_summary(&name, input) }));
                    out.push(item(
                        tid,
                        ChatBody::Tool {
                            name,
                            title,
                            status: ToolStatus::Running,
                            output: None,
                            diff,
                            files,
                        },
                    ));
                }
                _ => {}
            }
        }
        if self.message.as_deref() == Some(mid.as_str()) {
            self.message = None;
            self.blocks.clear();
        }
        out
    }

    fn stream(&mut self, ev: &Value) -> Vec<Effect> {
        match ev["type"].as_str() {
            Some("message_start") => {
                self.message = ev["message"]["id"].as_str().map(str::to_owned);
                self.blocks.clear();
                Vec::new()
            }
            Some("content_block_start") => {
                let kind = ev["content_block"]["type"].as_str().unwrap_or("").to_owned();
                self.blocks.insert(ev["index"].as_u64().unwrap_or(0), (kind, String::new()));
                Vec::new()
            }
            Some("content_block_delta") => {
                let i = ev["index"].as_u64().unwrap_or(0);
                let d = &ev["delta"];
                let piece = d["text"].as_str().or_else(|| d["thinking"].as_str()).unwrap_or("");
                let Some(mid) = self.message.clone() else { return Vec::new() };
                let Some((kind, text)) = self.blocks.get_mut(&i) else { return Vec::new() };
                text.push_str(piece);
                let id = format!("{mid}:{i}");
                match kind.as_str() {
                    "text" if !piece.is_empty() => {
                        vec![item(id, ChatBody::Assistant { text: text.clone(), streaming: true })]
                    }
                    "thinking" if !piece.is_empty() => {
                        vec![item(id, ChatBody::Thinking { text: text.clone() })]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn tool_result(&mut self, m: &Value) -> Vec<Effect> {
        let mut out = Vec::new();
        for block in m["message"]["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let tid = block["tool_use_id"].as_str().unwrap_or("").to_owned();
            let Some((name, title, diff, files)) = self.tools.get(&tid).cloned() else { continue };
            let failed = block["is_error"].as_bool().unwrap_or(false);
            let output = text_of(&block["content"]);
            let declined = failed && output.contains("denied");
            let status = if declined {
                ToolStatus::Declined
            } else if failed {
                ToolStatus::Failed
            } else {
                ToolStatus::Done
            };
            out.push(item(
                tid,
                ChatBody::Tool { name, title, status, output: Some(output), diff, files },
            ));
        }
        out
    }

    fn permission(&mut self, m: &Value) -> Vec<Effect> {
        let r = &m["request"];
        let request = m["request_id"].as_str().unwrap_or("").to_owned();
        let tool = r["tool_name"].as_str().unwrap_or("tool").to_owned();
        let input = r["input"].clone();
        let detail = tool_summary(&tool, &input);
        // Claude's own description of the call, then what exactly it runs.
        let shown = match r["description"].as_str() {
            Some(d) => format!("{d}\n{detail}"),
            None => detail.clone(),
        };
        let id = format!("perm-{request}");
        let next = Pending {
            request: request.clone(),
            tool: tool.clone(),
            input,
            suggestions: r.get("permission_suggestions").cloned().filter(|v| !v.is_null()),
            item: id.clone(),
            detail: detail.clone(),
        };
        if let Some(earlier) = self.pending.replace(next) {
            self.waiting.push(earlier);
        }
        vec![
            item(
                id,
                ChatBody::Permission {
                    request,
                    tool,
                    title: shown,
                    options: PermOption::standard(),
                    decided: None,
                },
            ),
            Effect::Signal(Signal::blocked("permission", Some(detail))),
        ]
    }

    /// After an answer: the next waiting request (it's blocked again, on that one), or work.
    fn next_or_working(&mut self) -> Vec<Effect> {
        match self.waiting.pop() {
            Some(p) => {
                let detail = p.detail.clone();
                self.pending = Some(p);
                vec![Effect::Signal(Signal::blocked("permission", Some(detail)))]
            }
            None => vec![status(AgentStatus::Working)],
        }
    }

    fn result(&mut self, m: &Value) -> Vec<Effect> {
        let is_error = m["is_error"].as_bool().unwrap_or(false);
        let stop = m["subtype"].as_str().unwrap_or("success").to_owned();
        let id = self.id("turn-");
        let mut out = vec![item(
            id,
            ChatBody::TurnEnd {
                stop: m["stop_reason"].as_str().map(str::to_owned).unwrap_or_else(|| stop.clone()),
                cost_micro_usd: m["total_cost_usd"].as_f64().map(|c| (c * 1e6).round() as u64),
                duration_ms: m["duration_ms"].as_u64(),
                input_tokens: m["usage"]["input_tokens"].as_u64(),
                output_tokens: m["usage"]["output_tokens"].as_u64(),
            },
        )];
        if is_error {
            let why =
                m["result"].as_str().map(|r| one_line(r, 200)).unwrap_or_else(|| stop.clone());
            out.push(notice(self.id("err-"), NoticeLevel::Error, why.clone()));
            out.push(Effect::Signal(Signal::Status {
                status: AgentStatus::Errored,
                kind: Some(stop),
                detail: Some(why),
                progress: None,
            }));
        } else {
            out.push(status(AgentStatus::Idle));
        }
        out.push(Effect::Signal(Signal::TurnEnd));
        if let Some(r) = m["result"].as_str().filter(|r| !r.trim().is_empty()) {
            out.push(Effect::Signal(Signal::Notify { text: one_line(r, 200) }));
        }
        out
    }
}

fn title(tool: &str, input: &Value) -> String {
    for k in ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"] {
        if let Some(v) = input[k].as_str() {
            return one_line(v, 160);
        }
    }
    tool.to_owned()
}

fn files(input: &Value) -> Vec<String> {
    input["file_path"].as_str().map(|f| vec![f.to_owned()]).unwrap_or_default()
}

/// Edit / Write inputs as a diff.
fn edit_diff(input: &Value) -> Option<String> {
    let path = input["file_path"].as_str()?;
    if let (Some(old), Some(new)) = (input["old_string"].as_str(), input["new_string"].as_str()) {
        return Some(crate::util::diff(path, Some(old), new));
    }
    input["content"].as_str().map(|c| crate::util::diff(path, None, c))
}

impl Driver for Claude {
    fn protocol(&self) -> &'static str {
        "stream-json"
    }

    fn start(&mut self) -> Vec<Effect> {
        // Claude starts when spoken to; until then it's ready for a prompt.
        let mut out = vec![status(AgentStatus::Idle)];
        if let Some(s) = &self.session {
            out.push(Effect::Signal(Signal::Conversation { id: s.clone() }));
        }
        out
    }

    fn on_line(&mut self, line: &str) -> Vec<Effect> {
        let Ok(m) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        match m["type"].as_str() {
            Some("system") if m["subtype"] == "init" => {
                let mut out = Vec::new();
                if let Some(s) = m["session_id"].as_str() {
                    self.session = Some(s.to_owned());
                    out.push(Effect::Signal(Signal::Conversation { id: s.to_owned() }));
                }
                out
            }
            Some("stream_event") => self.stream(&m["event"]),
            Some("assistant") => self.assistant(&m),
            Some("user") => self.tool_result(&m),
            Some("control_request") if m["request"]["subtype"] == "can_use_tool" => {
                self.permission(&m)
            }
            Some("control_request") => {
                // Requests we don't handle (hooks, MCP messages): say so, so Claude doesn't wait.
                let id = m["request_id"].as_str().unwrap_or("");
                vec![Effect::Send(json!({ "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": "not supported by ptyro" } }).to_string())]
            }
            Some("control_cancel_request") => {
                // The one named, wherever it waits.
                let id = m["request_id"].as_str().unwrap_or("");
                if let Some(i) = self.waiting.iter().position(|p| p.request == id) {
                    let p = self.waiting.remove(i);
                    return vec![item(
                        p.item,
                        ChatBody::Permission {
                            request: p.request,
                            tool: p.tool,
                            title: "cancelled".into(),
                            options: Vec::new(),
                            decided: Some("cancelled".into()),
                        },
                    )];
                }
                let Some(p) = self.pending.take() else { return Vec::new() };
                let mut out = vec![item(
                    p.item,
                    ChatBody::Permission {
                        request: p.request,
                        tool: p.tool,
                        title: "cancelled".into(),
                        options: Vec::new(),
                        decided: Some("cancelled".into()),
                    },
                )];
                out.extend(self.next_or_working());
                out
            }
            Some("result") => self.result(&m),
            _ => Vec::new(),
        }
    }

    fn prompt(&mut self, text: &str) -> Vec<Effect> {
        let id = self.id("you-");
        vec![
            item(id, ChatBody::User { text: text.to_owned() }),
            Effect::Send(
                json!({ "type": "user", "message": { "role": "user", "content": text } })
                    .to_string(),
            ),
            Effect::Signal(Signal::Prompt { text: text.to_owned() }),
            status(AgentStatus::Working),
        ]
    }

    fn answer(&mut self, d: Decision) -> Vec<Effect> {
        let Some(p) = self.pending.take() else { return Vec::new() };
        let (response, decided) = match d {
            Decision::Deny => {
                (json!({ "behavior": "deny", "message": "The user denied this in ptyro." }), "deny")
            }
            Decision::Always => {
                let mut r = json!({ "behavior": "allow", "updatedInput": p.input });
                if let Some(s) = &p.suggestions {
                    r["updatedPermissions"] = s.clone();
                }
                (r, "always")
            }
            Decision::Allow => (json!({ "behavior": "allow", "updatedInput": p.input }), "allow"),
        };
        let mut out = vec![
            Effect::Send(json!({ "type": "control_response", "response": { "subtype": "success", "request_id": p.request, "response": response } }).to_string()),
            item(p.item, ChatBody::Permission { request: p.request.clone(), tool: p.tool, title: String::new(), options: PermOption::standard(), decided: Some(decided.into()) }),
        ];
        out.extend(self.next_or_working());
        out
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        let id = self.id("ptyro-");
        vec![Effect::Send(json!({ "type": "control_request", "request_id": id, "request": { "subtype": "interrupt" } }).to_string())]
    }

    fn pending(&self) -> bool {
        self.pending.is_some()
    }

    fn save(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(effects: &[Effect]) -> Vec<ChatBody> {
        effects
            .iter()
            .filter_map(|e| if let Effect::Item(i) = e { Some(i.body.clone()) } else { None })
            .collect()
    }

    /// The real 2.1.305 turn: init, a Bash tool_use, can_use_tool, our allow, the result.
    #[test]
    fn a_real_turn() {
        let mut c = Claude::new(Opts::default());
        let mut all = Vec::new();
        let mut sent = Vec::new();
        for line in include_str!("../tests/fixtures/claude-2.1.305.jsonl").lines() {
            let fx = c.on_line(line);
            if fx.iter().any(|e| {
                matches!(e, Effect::Signal(Signal::Status { status: AgentStatus::Blocked, .. }))
            }) {
                assert!(c.pending());
                let a = c.answer(Decision::Allow);
                sent.extend(
                    a.iter().filter_map(|e| {
                        if let Effect::Send(s) = e { Some(s.clone()) } else { None }
                    }),
                );
                all.extend(a);
            }
            all.extend(fx);
        }
        assert!(all.contains(&Effect::Signal(Signal::Conversation {
            id: "3c80e6c8-4aef-4d44-bf4d-a41396596694".into()
        })));
        let bodies = items(&all);
        assert!(bodies.iter().any(|b| matches!(b, ChatBody::Tool { name, title, status: ToolStatus::Running, .. } if name == "Bash" && title == "touch probe.txt")));
        assert!(bodies.iter().any(|b| matches!(b, ChatBody::Tool { status: ToolStatus::Done, output: Some(o), .. } if o.contains("no output"))));
        assert!(
            bodies.iter().any(|b| matches!(b, ChatBody::Assistant { text, .. } if text == "done"))
        );
        assert!(
            bodies
                .iter()
                .any(|b| matches!(b, ChatBody::TurnEnd { cost_micro_usd: Some(c), .. } if *c > 0))
        );
        let reply: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(reply["response"]["request_id"], "022aff6e-ccf3-4ca5-91ca-19fe4f030558");
        assert_eq!(reply["response"]["response"]["behavior"], "allow");
        assert_eq!(reply["response"]["response"]["updatedInput"]["command"], "touch probe.txt");
        assert!(all.contains(&Effect::Signal(Signal::TurnEnd)));
        assert!(all.contains(&status(AgentStatus::Idle)));
    }

    #[test]
    fn streaming_prompting_denying_interrupting() {
        let mut c = Claude::new(Opts { session_id: Some("s1".into()), ..Opts::default() });
        assert!(c.start().contains(&Effect::Signal(Signal::Conversation { id: "s1".into() })));
        let p = c.prompt("hi");
        assert!(matches!(&p[1], Effect::Send(s) if s.contains(r#""content":"hi""#)));
        c.on_line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
        );
        c.on_line(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#);
        let a = c.on_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#);
        let b = c.on_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}"#);
        assert_eq!(items(&a), [ChatBody::Assistant { text: "Hel".into(), streaming: true }]);
        assert_eq!(items(&b), [ChatBody::Assistant { text: "Hello".into(), streaming: true }]);
        let done = c.on_line(r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#);
        assert!(
            matches!(&done[0], Effect::Item(i) if i.id == "m1:0" && i.body == ChatBody::Assistant { text: "Hello".into(), streaming: false })
        );
        c.on_line(r#"{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf /"}}}"#);
        let d = c.answer(Decision::Deny);
        assert!(matches!(&d[0], Effect::Send(s) if s.contains(r#""behavior":"deny""#)));
        assert!(!c.pending() && c.answer(Decision::Allow).is_empty());
        assert!(
            matches!(&c.interrupt()[0], Effect::Send(s) if s.contains(r#""subtype":"interrupt""#))
        );
        let saved = c.save();
        let back = Claude::restore(saved).unwrap();
        assert_eq!(back.session.as_deref(), Some("s1"));
        let err = c.on_line(r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"too many turns"}"#);
        assert!(err.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Errored, .. })
        )));
    }

    #[test]
    fn parallel_permission_requests_are_each_answered() {
        let mut c = Claude::new(Opts::default());
        c.on_line(r#"{"type":"control_request","request_id":"a","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}"#);
        c.on_line(r#"{"type":"control_request","request_id":"b","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm x"}}}"#);
        // The newest first (what the status shows), then the earlier one comes back.
        let first = c.answer(Decision::Deny);
        assert!(matches!(&first[0], Effect::Send(s) if s.contains(r#""request_id":"b""#)));
        assert!(first.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Blocked, .. })
        )));
        let second = c.answer(Decision::Allow);
        assert!(matches!(&second[0], Effect::Send(s) if s.contains(r#""request_id":"a""#)));
        assert!(!c.pending());
        // A cancel names its request, wherever it waits.
        c.on_line(r#"{"type":"control_request","request_id":"c","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"a"}}}"#);
        c.on_line(r#"{"type":"control_request","request_id":"d","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"b"}}}"#);
        c.on_line(r#"{"type":"control_cancel_request","request_id":"c"}"#);
        let last = c.answer(Decision::Allow);
        assert!(matches!(&last[0], Effect::Send(s) if s.contains(r#""request_id":"d""#)));
        assert!(!c.pending());
    }
}
