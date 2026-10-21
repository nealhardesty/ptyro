//! Codex over `codex app-server` (M10.4): JSON-RPC over stdio, one message per line, without
//! the `jsonrpc` field. Written against the schema Codex 0.143.0 generates
//! (`codex app-server generate-json-schema`); the handshake and `thread/start` were run live:
//! - `initialize {clientInfo}` → result, then the `initialized` notification;
//! - `thread/start {cwd, approvalPolicy, sandbox}` (or `thread/resume {threadId}`) → `thread.id`;
//! - `turn/start {threadId, input: [{type: "text", text}]}`; the turn streams `item/started`,
//!   `item/agentMessage/delta`, `item/commandExecution/outputDelta`, `item/completed`, and ends
//!   with `turn/completed {turn: {status}}`;
//! - approvals are server requests (`item/commandExecution/requestApproval`,
//!   `item/fileChange/requestApproval`) answered `{decision: accept|acceptForSession|decline}`;
//! - `turn/interrupt {threadId, turnId}` stops a turn.

use std::collections::BTreeMap;

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, NoticeLevel, PermOption, PlanStep, ToolStatus};
use ptyro_core::status::{AgentStatus, Signal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{item, notice, one_line, status, text_of};
use crate::{Driver, Effect, Opts};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    id: Value,
    item: String,
    title: String,
    /// The blocked status's text, shown again when it comes back to the top.
    #[serde(default)]
    detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Codex {
    cwd: String,
    resume: Option<String>,
    next: u64,
    thread: Option<String>,
    turn: Option<String>,
    /// Prompts typed before the thread exists.
    queued: Vec<String>,
    /// item id → its current body (deltas grow it).
    items: BTreeMap<String, ChatBody>,
    pending: Option<Pending>,
    /// Approvals that came while another was open (parallel commands): answered in turn,
    /// newest first, never dropped.
    #[serde(default)]
    waiting: Vec<Pending>,
    /// Our request ids → what they were.
    asked: BTreeMap<u64, String>,
    /// The last error shown (a failed turn repeats the `error` notification's text).
    last_error: Option<String>,
    n: u64,
}

impl Codex {
    pub fn new(opts: Opts) -> Self {
        Self { cwd: opts.cwd, resume: opts.resume, ..Self::default() }
    }

    pub fn restore(v: Value) -> Option<Self> {
        serde_json::from_value(v).ok()
    }

    fn request(&mut self, method: &str, params: Value) -> Effect {
        self.next += 1;
        self.asked.insert(self.next, method.to_owned());
        Effect::Send(json!({ "id": self.next, "method": method, "params": params }).to_string())
    }

    fn turn_start(&mut self, text: &str) -> Effect {
        let thread = self.thread.clone().unwrap_or_default();
        self.request("turn/start", json!({ "threadId": thread, "input": [{ "type": "text", "text": text, "text_elements": [] }] }))
    }

    fn body_of(item: &Value) -> Option<(String, ChatBody)> {
        let id = item["id"].as_str()?.to_owned();
        let st = |s: &Value| match s.as_str() {
            Some("completed") => ToolStatus::Done,
            Some("failed") => ToolStatus::Failed,
            Some("declined") => ToolStatus::Declined,
            _ => ToolStatus::Running,
        };
        let body = match item["type"].as_str()? {
            "agentMessage" => ChatBody::Assistant {
                text: item["text"].as_str().unwrap_or("").to_owned(),
                streaming: false,
            },
            "reasoning" => {
                let text = text_of(&item["summary"]);
                if text.is_empty() {
                    return None;
                }
                ChatBody::Thinking { text }
            }
            "commandExecution" => ChatBody::Tool {
                name: "Bash".into(),
                title: one_line(item["command"].as_str().unwrap_or(""), 160),
                status: st(&item["status"]),
                output: item["aggregatedOutput"].as_str().map(str::to_owned),
                diff: None,
                files: Vec::new(),
            },
            "fileChange" => {
                let changes = item["changes"].as_array().cloned().unwrap_or_default();
                let files: Vec<String> =
                    changes.iter().filter_map(|c| c["path"].as_str().map(str::to_owned)).collect();
                let diff = changes
                    .iter()
                    .map(|c| {
                        format!(
                            "--- {p}\n+++ {p}\n{}",
                            c["diff"].as_str().unwrap_or(""),
                            p = c["path"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ChatBody::Tool {
                    name: "Edit".into(),
                    title: files.join(", "),
                    status: st(&item["status"]),
                    output: None,
                    diff: Some(diff),
                    files,
                }
            }
            "mcpToolCall" | "dynamicToolCall" => ChatBody::Tool {
                name: match item["server"].as_str() {
                    Some(s) => format!("{s}.{}", item["tool"].as_str().unwrap_or("")),
                    None => item["tool"].as_str().unwrap_or("tool").to_owned(),
                },
                title: one_line(&item["arguments"].to_string(), 160),
                status: st(&item["status"]),
                output: item["result"]
                    .as_object()
                    .map(|r| text_of(&r.get("content").cloned().unwrap_or_default())),
                diff: None,
                files: Vec::new(),
            },
            "webSearch" => ChatBody::Tool {
                name: "web_search".into(),
                title: item["query"].as_str().unwrap_or("").to_owned(),
                status: ToolStatus::Done,
                output: None,
                diff: None,
                files: Vec::new(),
            },
            "plan" => ChatBody::Plan {
                steps: item["text"]
                    .as_str()
                    .unwrap_or("")
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| PlanStep { text: l.trim().to_owned(), status: "pending".into() })
                    .collect(),
            },
            "contextCompaction" => {
                ChatBody::Notice { level: NoticeLevel::Info, text: "context compacted".into() }
            }
            _ => return None,
        };
        Some((id, body))
    }

    fn put(&mut self, id: String, body: ChatBody) -> Vec<Effect> {
        let mut out = Vec::new();
        if let ChatBody::Tool { name, title, status: ToolStatus::Running, .. } = &body {
            out.push(Effect::Signal(Signal::Activity { text: format!("{name}: {title}") }));
        }
        self.items.insert(id.clone(), body.clone());
        out.push(item(id, body));
        out
    }

    fn grow(&mut self, id: &str, piece: &str, kind: &str) -> Vec<Effect> {
        let body = self.items.entry(id.to_owned()).or_insert_with(|| match kind {
            "thinking" => ChatBody::Thinking { text: String::new() },
            _ => ChatBody::Assistant { text: String::new(), streaming: true },
        });
        match body {
            ChatBody::Assistant { text, streaming } => {
                text.push_str(piece);
                *streaming = true;
            }
            ChatBody::Thinking { text } => text.push_str(piece),
            ChatBody::Tool { output, .. } => output.get_or_insert_with(String::new).push_str(piece),
            _ => return Vec::new(),
        }
        let body = body.clone();
        vec![item(id, body)]
    }

    fn notification(&mut self, method: &str, p: &Value) -> Vec<Effect> {
        match method {
            "item/started" | "item/completed" => {
                let Some((id, mut body)) = Self::body_of(&p["item"]) else { return Vec::new() };
                // A completed command keeps the output its deltas brought, if the item has none.
                if let (
                    ChatBody::Tool { output: o @ None, .. },
                    Some(ChatBody::Tool { output: Some(prev), .. }),
                ) = (&mut body, self.items.get(&id))
                {
                    *o = Some(prev.clone());
                }
                if method == "item/completed"
                    && let ChatBody::Assistant { streaming, .. } = &mut body
                {
                    *streaming = false;
                }
                self.put(id, body)
            }
            "item/agentMessage/delta" => self.grow(
                p["itemId"].as_str().unwrap_or(""),
                p["delta"].as_str().unwrap_or(""),
                "text",
            ),
            "item/reasoning/summaryTextDelta" => self.grow(
                p["itemId"].as_str().unwrap_or(""),
                p["delta"].as_str().unwrap_or(""),
                "thinking",
            ),
            "item/commandExecution/outputDelta" => self.grow(
                p["itemId"].as_str().unwrap_or(""),
                p["delta"].as_str().unwrap_or(""),
                "tool",
            ),
            "turn/started" => {
                self.turn = p["turn"]["id"].as_str().map(str::to_owned);
                vec![status(AgentStatus::Working)]
            }
            "turn/plan/updated" => {
                let steps = p["plan"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| PlanStep {
                        text: s["step"].as_str().unwrap_or("").to_owned(),
                        status: s["status"].as_str().unwrap_or("pending").to_owned(),
                    })
                    .collect();
                vec![item(
                    format!("plan-{}", p["turnId"].as_str().unwrap_or("")),
                    ChatBody::Plan { steps },
                )]
            }
            "turn/completed" => {
                let t = &p["turn"];
                let st = t["status"].as_str().unwrap_or("completed").to_owned();
                self.turn = None;
                self.n += 1;
                let mut out = vec![item(
                    format!("turn-{}", self.n),
                    ChatBody::TurnEnd {
                        stop: st.clone(),
                        cost_micro_usd: None,
                        duration_ms: t["durationMs"].as_u64(),
                        input_tokens: None,
                        output_tokens: None,
                    },
                )];
                if st == "failed" {
                    let why =
                        t["error"]["message"].as_str().unwrap_or("the turn failed").to_owned();
                    if self.last_error.as_deref() != Some(why.as_str()) {
                        out.push(notice(
                            format!("err-{}", self.n),
                            NoticeLevel::Error,
                            why.clone(),
                        ));
                    }
                    out.push(Effect::Signal(Signal::Status {
                        status: AgentStatus::Errored,
                        kind: Some(error_kind(&why).into()),
                        detail: Some(why),
                        progress: None,
                    }));
                } else {
                    out.push(status(AgentStatus::Idle));
                }
                out.push(Effect::Signal(Signal::TurnEnd));
                if let Some(last) = self.items.values().rev().find_map(|b| {
                    if let ChatBody::Assistant { text, .. } = b { Some(text.clone()) } else { None }
                }) {
                    out.push(Effect::Signal(Signal::Notify { text: one_line(&last, 200) }));
                }
                out
            }
            "error" => {
                self.n += 1;
                let msg = p["error"]["message"].as_str().unwrap_or("error").to_owned();
                self.last_error = Some(msg.clone());
                vec![notice(
                    format!("err-{}", self.n),
                    if p["willRetry"].as_bool() == Some(true) {
                        NoticeLevel::Warn
                    } else {
                        NoticeLevel::Error
                    },
                    msg,
                )]
            }
            _ => Vec::new(),
        }
    }

    fn server_request(&mut self, id: Value, method: &str, p: &Value) -> Vec<Effect> {
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let item_id = p["itemId"].as_str().unwrap_or("").to_owned();
                let (tool, title) = if method.contains("command") {
                    ("Bash".to_owned(), p["command"].as_str().map(str::to_owned).or_else(|| self.title_of(&item_id)).unwrap_or_default())
                } else {
                    ("Edit".to_owned(), self.title_of(&item_id).unwrap_or_else(|| "file changes".into()))
                };
                let reason = p["reason"].as_str().map(|r| format!("\n{r}")).unwrap_or_default();
                let key = format!("perm-{}", id.to_string().trim_matches('"'));
                let detail = format!("{tool}: {}", one_line(&title, 120));
                let next = Pending { id: id.clone(), item: key.clone(), title: title.clone(), detail: detail.clone() };
                if let Some(earlier) = self.pending.replace(next) {
                    self.waiting.push(earlier);
                }
                vec![
                    item(key, ChatBody::Permission { request: id.to_string(), tool: tool.clone(), title: format!("{title}{reason}"), options: PermOption::standard(), decided: None }),
                    Effect::Signal(Signal::blocked("permission", Some(detail))),
                ]
            }
            // Questions to the user and the rest: decline, so the turn doesn't hang.
            _ => vec![Effect::Send(json!({ "id": id, "error": { "code": -32601, "message": format!("{method} is not supported by ptyro") } }).to_string())],
        }
    }

    fn title_of(&self, item: &str) -> Option<String> {
        match self.items.get(item)? {
            ChatBody::Tool { title, .. } => Some(title.clone()),
            _ => None,
        }
    }

    fn response(&mut self, id: u64, result: &Value, error: &Value) -> Vec<Effect> {
        let method = self.asked.remove(&id).unwrap_or_default();
        if !error.is_null() {
            let msg = error["message"].as_str().unwrap_or("error").to_owned();
            self.n += 1;
            let mut out = vec![notice(
                format!("err-{}", self.n),
                NoticeLevel::Error,
                format!("{method}: {msg}"),
            )];
            if method.starts_with("thread/") || method == "initialize" {
                let kind = error_kind(&msg);
                out.push(Effect::Signal(Signal::Status {
                    status: AgentStatus::Errored,
                    kind: Some(kind.into()),
                    detail: Some(msg),
                    progress: None,
                }));
            }
            return out;
        }
        match method.as_str() {
            "initialize" => {
                let mut out = vec![Effect::Send(json!({ "method": "initialized" }).to_string())];
                let params = json!({ "cwd": self.cwd, "approvalPolicy": "on-request", "sandbox": "workspace-write" });
                out.push(match self.resume.clone() {
                    Some(t) => self.request("thread/resume", json!({ "threadId": t, "cwd": self.cwd, "approvalPolicy": "on-request", "sandbox": "workspace-write" })),
                    None => self.request("thread/start", params),
                });
                out
            }
            "thread/start" | "thread/resume" => {
                let Some(t) = result["thread"]["id"].as_str() else { return Vec::new() };
                self.thread = Some(t.to_owned());
                let mut out = vec![Effect::Signal(Signal::Conversation { id: t.to_owned() })];
                let queued = std::mem::take(&mut self.queued);
                if queued.is_empty() {
                    out.push(status(AgentStatus::Idle));
                }
                for q in queued {
                    out.push(self.turn_start(&q));
                }
                out
            }
            "turn/start" => {
                self.turn = result["turn"]["id"].as_str().map(str::to_owned).or(self.turn.take());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

/// `auth` for login problems (so attention says "needs you to log in"), else `error`.
fn error_kind(msg: &str) -> &'static str {
    let m = msg.to_lowercase();
    if ["auth", "401", "log in", "login", "sign in", "access token"].iter().any(|k| m.contains(k)) {
        "auth"
    } else {
        "error"
    }
}

impl Driver for Codex {
    fn protocol(&self) -> &'static str {
        "app-server"
    }

    fn start(&mut self) -> Vec<Effect> {
        vec![self.request("initialize", json!({ "clientInfo": { "name": "ptyro", "title": "ptyro", "version": env!("CARGO_PKG_VERSION") } }))]
    }

    fn on_line(&mut self, line: &str) -> Vec<Effect> {
        let Ok(m) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        match (m.get("id"), m["method"].as_str()) {
            (Some(id), Some(method)) => self.server_request(id.clone(), method, &m["params"]),
            (None, Some(method)) => self.notification(method, &m["params"]),
            (Some(id), None) => match id.as_u64() {
                Some(n) => self.response(n, &m["result"], &m["error"]),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn prompt(&mut self, text: &str) -> Vec<Effect> {
        self.n += 1;
        let mut out = vec![
            item(format!("you-{}", self.n), ChatBody::User { text: text.to_owned() }),
            Effect::Signal(Signal::Prompt { text: text.to_owned() }),
            status(AgentStatus::Working),
        ];
        if self.thread.is_some() {
            out.push(self.turn_start(text));
        } else {
            self.queued.push(text.to_owned());
        }
        out
    }

    fn answer(&mut self, d: Decision) -> Vec<Effect> {
        let Some(p) = self.pending.take() else { return Vec::new() };
        let decision = match d {
            Decision::Allow => "accept",
            Decision::Always => "acceptForSession",
            Decision::Deny => "decline",
        };
        let mut out = vec![
            Effect::Send(json!({ "id": p.id, "result": { "decision": decision } }).to_string()),
            item(
                p.item,
                ChatBody::Permission {
                    request: p.id.to_string(),
                    tool: String::new(),
                    title: p.title,
                    options: PermOption::standard(),
                    decided: Some(decision.into()),
                },
            ),
        ];
        // The next open approval (blocked again, on that one), or back to work.
        match self.waiting.pop() {
            Some(n) => {
                out.push(Effect::Signal(Signal::blocked("permission", Some(n.detail.clone()))));
                self.pending = Some(n);
            }
            None => out.push(status(AgentStatus::Working)),
        }
        out
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        match (self.thread.clone(), self.turn.clone()) {
            (Some(thread), Some(turn)) => {
                vec![self.request("turn/interrupt", json!({ "threadId": thread, "turnId": turn }))]
            }
            _ => Vec::new(),
        }
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

    fn sent(fx: &[Effect]) -> Vec<Value> {
        fx.iter()
            .filter_map(
                |e| if let Effect::Send(s) = e { serde_json::from_str(s).ok() } else { None },
            )
            .collect()
    }

    #[test]
    fn handshake_turn_approval_and_completion() {
        let mut c = Codex::new(Opts { cwd: "/w".into(), ..Opts::default() });
        let init = sent(&c.start());
        assert_eq!(init[0]["method"], "initialize");
        assert!(init[0].get("jsonrpc").is_none());
        // The live 0.143.0 answer.
        let fx = c.on_line(r#"{"id":1,"result":{"userAgent":"ptyro/0.143.0","codexHome":"/h/.codex","platformFamily":"unix","platformOs":"linux"}}"#);
        let s = sent(&fx);
        assert_eq!(s[0]["method"], "initialized");
        assert_eq!(s[1]["method"], "thread/start");
        assert_eq!(s[1]["params"]["approvalPolicy"], "on-request");
        // A prompt before the thread exists waits for it.
        let p = c.prompt("fix it");
        assert!(sent(&p).is_empty());
        let fx =
            c.on_line(r#"{"id":2,"result":{"thread":{"id":"th-1","status":{"type":"idle"}}}}"#);
        assert!(fx.contains(&Effect::Signal(Signal::Conversation { id: "th-1".into() })));
        let turn = sent(&fx);
        assert_eq!(turn[0]["method"], "turn/start");
        assert_eq!(turn[0]["params"]["input"][0]["text"], "fix it");
        c.on_line(r#"{"method":"turn/started","params":{"threadId":"th-1","turn":{"id":"tu-1","items":[],"status":"inProgress"}}}"#);
        c.on_line(r#"{"method":"item/started","params":{"threadId":"th-1","turnId":"tu-1","startedAtMs":1,"item":{"type":"commandExecution","id":"c1","command":"cargo test","commandActions":[],"cwd":"/w","status":"inProgress"}}}"#);
        let d = c.on_line(r#"{"method":"item/commandExecution/outputDelta","params":{"threadId":"th-1","turnId":"tu-1","itemId":"c1","delta":"running 3 tests\n"}}"#);
        assert!(
            matches!(&d[0], Effect::Item(i) if matches!(&i.body, ChatBody::Tool { output: Some(o), .. } if o.contains("3 tests")))
        );
        let ap = c.on_line(r#"{"id":"a-7","method":"item/commandExecution/requestApproval","params":{"threadId":"th-1","turnId":"tu-1","itemId":"c1","startedAtMs":2,"command":"cargo test","reason":"needs network"}}"#);
        assert!(ap.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Blocked, .. })
        )));
        let ans = sent(&c.answer(Decision::Always));
        assert_eq!(ans[0], json!({ "id": "a-7", "result": { "decision": "acceptForSession" } }));
        let done = c.on_line(r#"{"method":"item/completed","params":{"threadId":"th-1","turnId":"tu-1","completedAtMs":3,"item":{"type":"commandExecution","id":"c1","command":"cargo test","commandActions":[],"cwd":"/w","status":"completed","exitCode":0}}}"#);
        assert!(
            matches!(&done.last().unwrap(), Effect::Item(i) if matches!(&i.body, ChatBody::Tool { status: ToolStatus::Done, output: Some(o), .. } if o.contains("3 tests")))
        );
        c.on_line(r#"{"method":"item/agentMessage/delta","params":{"threadId":"th-1","turnId":"tu-1","itemId":"m1","delta":"All "}}"#);
        c.on_line(r#"{"method":"item/agentMessage/delta","params":{"threadId":"th-1","turnId":"tu-1","itemId":"m1","delta":"green."}}"#);
        let end = c.on_line(r#"{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"tu-1","items":[],"status":"completed"}}}"#);
        assert!(
            end.contains(&status(AgentStatus::Idle))
                && end.contains(&Effect::Signal(Signal::TurnEnd))
        );
        assert!(end.contains(&Effect::Signal(Signal::Notify { text: "All green.".into() })));
        // An unknown server request is refused rather than left hanging.
        let q = sent(&c.on_line(r#"{"id":9,"method":"item/tool/requestUserInput","params":{}}"#));
        assert_eq!(q[0]["error"]["code"], -32601);
        assert!(Codex::restore(c.save()).unwrap().thread.as_deref() == Some("th-1"));
    }

    #[test]
    fn file_changes_and_auth_errors() {
        let mut c = Codex::new(Opts { resume: Some("th-9".into()), ..Opts::default() });
        c.start();
        let fx = c.on_line(r#"{"id":1,"result":{}}"#);
        assert_eq!(sent(&fx)[1]["method"], "thread/resume");
        let fx = c.on_line(
            r#"{"id":2,"error":{"code":-32000,"message":"401 Unauthorized: please log in"}}"#,
        );
        assert!(fx.iter().any(|e| matches!(e, Effect::Signal(Signal::Status { status: AgentStatus::Errored, kind: Some(k), .. }) if k == "auth")));
        let fc = c.on_line(r#"{"method":"item/started","params":{"item":{"type":"fileChange","id":"f1","status":"inProgress","changes":[{"path":"src/a.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-a\n+b\n"}]}}}"#);
        assert!(fc.iter().any(|e| matches!(e, Effect::Item(i) if matches!(&i.body, ChatBody::Tool { name, files, diff: Some(d), .. } if name == "Edit" && files == &["src/a.rs"] && d.contains("+b")))));
        // What the real 0.143.0 says when logged out: one notice, attention for logging in.
        let msg = "Your access token could not be refreshed. Please log out and sign in again.";
        let e = c.on_line(&json!({ "method": "error", "params": { "error": { "message": msg }, "willRetry": false } }).to_string());
        let t = c.on_line(&json!({ "method": "turn/completed", "params": { "turn": { "id": "x", "status": "failed", "error": { "message": msg } } } }).to_string());
        let notices = e
            .iter()
            .chain(&t)
            .filter(|x| matches!(x, Effect::Item(i) if matches!(i.body, ChatBody::Notice { .. })))
            .count();
        assert_eq!(notices, 1);
        assert!(t.iter().any(
            |x| matches!(x, Effect::Signal(Signal::Status { kind: Some(k), .. }) if k == "auth")
        ));
    }

    #[test]
    fn parallel_approvals_are_each_answered() {
        let mut c = Codex::new(Opts::default());
        c.on_line(r#"{"id":"a-1","method":"item/commandExecution/requestApproval","params":{"itemId":"c1","command":"ls"}}"#);
        c.on_line(r#"{"id":"a-2","method":"item/commandExecution/requestApproval","params":{"itemId":"c2","command":"rm x"}}"#);
        let first = c.answer(Decision::Deny);
        assert!(
            matches!(&first[0], Effect::Send(s) if s.contains(r#""id":"a-2""#) && s.contains("decline"))
        );
        assert!(first.iter().any(|e| matches!(e, Effect::Signal(Signal::Status { status: AgentStatus::Blocked, detail: Some(d), .. }) if d.contains("ls"))));
        let second = c.answer(Decision::Allow);
        assert!(
            matches!(&second[0], Effect::Send(s) if s.contains(r#""id":"a-1""#) && s.contains("accept"))
        );
        assert!(!c.pending());
    }
}
