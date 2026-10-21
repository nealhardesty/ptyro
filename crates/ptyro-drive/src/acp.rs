//! The Agent Client Protocol (M10.2): JSON-RPC 2.0 over stdio, as Gemini CLI speaks it with
//! `--acp` (and any other ACP agent). The handshake was run live against Gemini 0.64.0:
//! `initialize {protocolVersion: 1, clientCapabilities}` → `{protocolVersion, authMethods,
//! agentCapabilities: {loadSession, …}}`; `session/new {cwd, mcpServers}` → `{sessionId,
//! modes, models}`, then `session/update` notifications. ptyro offers no file system or terminal
//! capabilities: the agent works on its own files, and ptyro only shows what it does.
//! - prompt: `session/prompt {sessionId, prompt: [{type: text, text}]}`, answered with
//!   `{stopReason}` when the turn ends;
//! - updates: `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`
//!   (content, diffs), `plan`;
//! - permissions: the agent's `session/request_permission {toolCall, options}` request,
//!   answered `{outcome: {outcome: "selected", optionId}}` (or `cancelled`);
//! - interrupt: the `session/cancel` notification.

use std::collections::BTreeMap;

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, NoticeLevel, PermKind, PermOption, PlanStep, ToolStatus};
use ptyro_core::status::{AgentStatus, Signal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{diff, item, notice, one_line, status, text_of};
use crate::{Driver, Effect, Opts};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    id: Value,
    item: String,
    options: Vec<PermOption>,
    title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Acp {
    cwd: String,
    resume: Option<String>,
    next: u64,
    session: Option<String>,
    load_session: bool,
    queued: Vec<String>,
    /// The message being streamed (a new one starts after any other update).
    current: Option<(String, String)>,
    thought: Option<(String, String)>,
    tools: BTreeMap<String, ChatBody>,
    pending: Option<Pending>,
    /// Requests that came while another was open: answered in turn, newest first.
    #[serde(default)]
    waiting: Vec<Pending>,
    asked: BTreeMap<u64, String>,
    /// The prompt request in flight (its answer ends the turn).
    prompting: Option<u64>,
    n: u64,
}

impl Acp {
    pub fn new(opts: Opts) -> Self {
        Self { cwd: opts.cwd, resume: opts.resume, ..Self::default() }
    }

    pub fn restore(v: Value) -> Option<Self> {
        serde_json::from_value(v).ok()
    }

    fn request(&mut self, method: &str, params: Value) -> (u64, Effect) {
        self.next += 1;
        self.asked.insert(self.next, method.to_owned());
        (
            self.next,
            Effect::Send(
                json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params })
                    .to_string(),
            ),
        )
    }

    fn send_prompt(&mut self, text: &str) -> Effect {
        let s = self.session.clone().unwrap_or_default();
        let (id, e) = self.request(
            "session/prompt",
            json!({ "sessionId": s, "prompt": [{ "type": "text", "text": text }] }),
        );
        self.prompting = Some(id);
        e
    }

    fn fresh(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}{}", self.n)
    }

    fn tool(&mut self, u: &Value, update: bool) -> Vec<Effect> {
        let id = u["toolCallId"].as_str().unwrap_or("tool").to_owned();
        let prev = self.tools.get(&id).cloned();
        let (mut name, mut title, mut st, mut output, mut dif, mut files) = match prev {
            Some(ChatBody::Tool { name, title, status, output, diff, files }) => {
                (name, title, status, output, diff, files)
            }
            _ => (
                u["kind"].as_str().unwrap_or("tool").to_owned(),
                String::new(),
                ToolStatus::Pending,
                None,
                None,
                Vec::new(),
            ),
        };
        if let Some(t) = u["title"].as_str() {
            title = one_line(t, 160);
        }
        if !update && let Some(k) = u["kind"].as_str() {
            name = k.to_owned();
        }
        st = match u["status"].as_str() {
            Some("pending") => ToolStatus::Pending,
            Some("in_progress") => ToolStatus::Running,
            Some("completed") => ToolStatus::Done,
            Some("failed") => ToolStatus::Failed,
            _ => st,
        };
        for c in u["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("diff") => {
                    let path = c["path"].as_str().unwrap_or("").to_owned();
                    let d = diff(&path, c["oldText"].as_str(), c["newText"].as_str().unwrap_or(""));
                    dif = Some(match dif.take() {
                        Some(prev) => format!("{prev}\n{d}"),
                        None => d,
                    });
                    if !files.contains(&path) {
                        files.push(path);
                    }
                }
                _ => {
                    let t = text_of(&c["content"]);
                    if !t.is_empty() {
                        output = Some(match output.take() {
                            Some(prev) => format!("{prev}\n{t}"),
                            None => t,
                        });
                    }
                }
            }
        }
        for l in u["locations"].as_array().into_iter().flatten() {
            if let Some(p) = l["path"].as_str()
                && !files.iter().any(|f| f == p)
            {
                files.push(p.to_owned());
            }
        }
        let body = ChatBody::Tool {
            name: name.clone(),
            title: title.clone(),
            status: st,
            output,
            diff: dif,
            files,
        };
        self.tools.insert(id.clone(), body.clone());
        self.current = None;
        self.thought = None;
        let mut out = vec![item(id, body)];
        if matches!(st, ToolStatus::Running | ToolStatus::Pending) && !update {
            out.push(Effect::Signal(Signal::Activity { text: format!("{name}: {title}") }));
        }
        out
    }

    fn update(&mut self, u: &Value) -> Vec<Effect> {
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let piece = text_of(&u["content"]);
                if self.current.is_none() {
                    let id = self.fresh("msg-");
                    self.current = Some((id, String::new()));
                }
                let (id, text) = self.current.as_mut().expect("current");
                text.push_str(&piece);
                vec![item(id.clone(), ChatBody::Assistant { text: text.clone(), streaming: true })]
            }
            Some("agent_thought_chunk") => {
                let piece = text_of(&u["content"]);
                if self.thought.is_none() {
                    let id = self.fresh("thought-");
                    self.thought = Some((id, String::new()));
                }
                let (id, text) = self.thought.as_mut().expect("thought");
                text.push_str(&piece);
                vec![item(id.clone(), ChatBody::Thinking { text: text.clone() })]
            }
            Some("tool_call") => self.tool(u, false),
            Some("tool_call_update") => self.tool(u, true),
            Some("plan") => {
                let steps = u["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| PlanStep {
                        text: e["content"].as_str().unwrap_or("").to_owned(),
                        status: e["status"].as_str().unwrap_or("pending").to_owned(),
                    })
                    .collect();
                vec![item("plan", ChatBody::Plan { steps })]
            }
            _ => Vec::new(),
        }
    }

    fn finish_message(&mut self) -> Vec<Effect> {
        match self.current.take() {
            Some((id, text)) => vec![item(id, ChatBody::Assistant { text, streaming: false })],
            None => Vec::new(),
        }
    }
}

impl Driver for Acp {
    fn protocol(&self) -> &'static str {
        "acp"
    }

    fn start(&mut self) -> Vec<Effect> {
        let (_, e) = self.request(
            "initialize",
            json!({ "protocolVersion": 1, "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false }, "clientInfo": { "name": "ptyro", "version": env!("CARGO_PKG_VERSION") } }),
        );
        vec![e]
    }

    fn on_line(&mut self, line: &str) -> Vec<Effect> {
        let Ok(m) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        match (m.get("id"), m["method"].as_str()) {
            (Some(id), Some("session/request_permission")) => {
                let p = &m["params"];
                let options: Vec<PermOption> = p["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| PermOption {
                        id: o["optionId"].as_str().unwrap_or("").to_owned(),
                        label: o["name"].as_str().unwrap_or("").to_owned(),
                        kind: match o["kind"].as_str() {
                            Some("allow_always") => PermKind::AllowAlways,
                            Some("reject_once") => PermKind::RejectOnce,
                            Some("reject_always") => PermKind::RejectAlways,
                            _ => PermKind::AllowOnce,
                        },
                    })
                    .collect();
                let tc = &p["toolCall"];
                let mut out =
                    self.tool(tc, self.tools.contains_key(tc["toolCallId"].as_str().unwrap_or("")));
                let title = tc["title"]
                    .as_str()
                    .map(|t| one_line(t, 160))
                    .unwrap_or_else(|| "a tool call".into());
                let key = format!("perm-{}", id.to_string().trim_matches('"'));
                let next = Pending {
                    id: id.clone(),
                    item: key.clone(),
                    options: options.clone(),
                    title: title.clone(),
                };
                if let Some(earlier) = self.pending.replace(next) {
                    self.waiting.push(earlier);
                }
                out.push(item(
                    key,
                    ChatBody::Permission {
                        request: id.to_string(),
                        tool: tc["kind"].as_str().unwrap_or("tool").to_owned(),
                        title: title.clone(),
                        options,
                        decided: None,
                    },
                ));
                out.push(Effect::Signal(Signal::blocked("permission", Some(title))));
                out
            }
            (Some(id), Some(method)) => {
                // File system and terminal requests (we offered neither) and anything else.
                vec![Effect::Send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} is not supported by ptyro") } }).to_string())]
            }
            (None, Some("session/update")) => self.update(&m["params"]["update"]),
            (Some(id), None) => {
                let n = id.as_u64().unwrap_or(0);
                let method = self.asked.remove(&n).unwrap_or_default();
                if let Some(e) = m.get("error").filter(|e| !e.is_null()) {
                    let msg = e["message"].as_str().unwrap_or("error").to_owned();
                    let auth = msg.to_lowercase().contains("auth") || e["code"] == -32000;
                    let id = self.fresh("err-");
                    let mut out = vec![notice(
                        id,
                        NoticeLevel::Error,
                        if auth {
                            format!("{msg}: log in with the agent's own CLI first")
                        } else {
                            msg.clone()
                        },
                    )];
                    if method != "session/prompt" || auth {
                        out.push(Effect::Signal(Signal::Status {
                            status: AgentStatus::Errored,
                            kind: Some(if auth { "auth" } else { "error" }.into()),
                            detail: Some(msg),
                            progress: None,
                        }));
                    } else {
                        self.prompting = None;
                        out.extend(self.finish_message());
                        out.push(status(AgentStatus::Idle));
                        out.push(Effect::Signal(Signal::TurnEnd));
                    }
                    return out;
                }
                let r = &m["result"];
                match method.as_str() {
                    "initialize" => {
                        self.load_session =
                            r["agentCapabilities"]["loadSession"].as_bool().unwrap_or(false);
                        let mcp: Vec<Value> = Vec::new();
                        let (_, e) = match (self.resume.clone(), self.load_session) {
                            (Some(s), true) => self.request(
                                "session/load",
                                json!({ "sessionId": s, "cwd": self.cwd, "mcpServers": mcp }),
                            ),
                            _ => self.request(
                                "session/new",
                                json!({ "cwd": self.cwd, "mcpServers": mcp }),
                            ),
                        };
                        vec![e]
                    }
                    "session/new" | "session/load" => {
                        let sid =
                            r["sessionId"].as_str().map(str::to_owned).or(self.resume.clone());
                        let Some(sid) = sid else { return Vec::new() };
                        self.session = Some(sid.clone());
                        let mut out = vec![Effect::Signal(Signal::Conversation { id: sid })];
                        let queued = std::mem::take(&mut self.queued);
                        if queued.is_empty() {
                            out.push(status(AgentStatus::Idle));
                        }
                        for q in queued {
                            out.push(self.send_prompt(&q));
                        }
                        out
                    }
                    "session/prompt" => {
                        self.prompting = None;
                        let stop = r["stopReason"].as_str().unwrap_or("end_turn").to_owned();
                        let last = self.current.as_ref().map(|(_, t)| one_line(t, 200));
                        let mut out = self.finish_message();
                        let id = self.fresh("turn-");
                        out.push(item(
                            id,
                            ChatBody::TurnEnd {
                                stop,
                                cost_micro_usd: None,
                                duration_ms: None,
                                input_tokens: None,
                                output_tokens: None,
                            },
                        ));
                        out.push(status(AgentStatus::Idle));
                        out.push(Effect::Signal(Signal::TurnEnd));
                        if let Some(t) = last.filter(|t| !t.is_empty()) {
                            out.push(Effect::Signal(Signal::Notify { text: t }));
                        }
                        out
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn prompt(&mut self, text: &str) -> Vec<Effect> {
        let id = self.fresh("you-");
        self.current = None;
        let mut out = vec![
            item(id, ChatBody::User { text: text.to_owned() }),
            Effect::Signal(Signal::Prompt { text: text.to_owned() }),
            status(AgentStatus::Working),
        ];
        if self.session.is_some() {
            out.push(self.send_prompt(text));
        } else {
            self.queued.push(text.to_owned());
        }
        out
    }

    fn answer(&mut self, d: Decision) -> Vec<Effect> {
        let Some(p) = self.pending.take() else { return Vec::new() };
        let want: &[PermKind] = match d {
            Decision::Allow => &[PermKind::AllowOnce, PermKind::AllowAlways],
            Decision::Always => &[PermKind::AllowAlways, PermKind::AllowOnce],
            Decision::Deny => &[PermKind::RejectOnce, PermKind::RejectAlways],
        };
        let choice = want.iter().find_map(|k| p.options.iter().find(|o| o.kind == *k));
        let outcome = match choice {
            Some(o) => json!({ "outcome": "selected", "optionId": o.id }),
            None => json!({ "outcome": "cancelled" }),
        };
        let decided = choice.map_or_else(|| "cancelled".to_owned(), |o| o.id.clone());
        let mut out = vec![
            Effect::Send(
                json!({ "jsonrpc": "2.0", "id": p.id, "result": { "outcome": outcome } })
                    .to_string(),
            ),
            item(
                p.item,
                ChatBody::Permission {
                    request: p.id.to_string(),
                    tool: String::new(),
                    title: p.title,
                    options: p.options,
                    decided: Some(decided),
                },
            ),
        ];
        // The next open request (blocked again, on that one), or back to work.
        match self.waiting.pop() {
            Some(n) => {
                out.push(Effect::Signal(Signal::blocked("permission", Some(n.title.clone()))));
                self.pending = Some(n);
            }
            None => out.push(status(AgentStatus::Working)),
        }
        out
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        // Open permission requests are answered "cancelled" first (the protocol requires it).
        for p in self.pending.take().into_iter().chain(std::mem::take(&mut self.waiting)) {
            out.push(Effect::Send(json!({ "jsonrpc": "2.0", "id": p.id, "result": { "outcome": { "outcome": "cancelled" } } }).to_string()));
        }
        if let Some(s) = &self.session {
            out.push(Effect::Send(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": s } }).to_string()));
        }
        out
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
    fn gemini_handshake_a_turn_and_a_permission() {
        let mut a = Acp::new(Opts { cwd: "/w".into(), ..Opts::default() });
        let init = sent(&a.start());
        assert_eq!(init[0]["jsonrpc"], "2.0");
        assert_eq!(init[0]["params"]["clientCapabilities"]["terminal"], false);
        // Gemini 0.64.0's answer (trimmed).
        let fx = a.on_line(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"oauth-personal","name":"Log in with Google"}],"agentInfo":{"name":"gemini-cli","version":"0.64.0"},"agentCapabilities":{"loadSession":true}}}"#);
        assert_eq!(sent(&fx)[0]["method"], "session/new");
        let fx = a.on_line(r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"94496331-6614-4bc8-b236-c933977aff64","modes":{"currentModeId":"default"}}}"#);
        assert!(fx.contains(&Effect::Signal(Signal::Conversation {
            id: "94496331-6614-4bc8-b236-c933977aff64".into()
        })));
        assert_eq!(sent(&a.prompt("hello"))[0]["method"], "session/prompt");
        a.on_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}}"#);
        a.on_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I'll "}}}}"#);
        let fx = a.on_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"edit it."}}}}"#);
        assert!(
            matches!(&fx[0], Effect::Item(i) if i.body == ChatBody::Assistant { text: "I'll edit it.".into(), streaming: true })
        );
        a.on_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Write a.txt","kind":"edit","status":"pending","locations":[{"path":"/w/a.txt"}]}}}"#);
        let fx = a.on_line(r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"t1","title":"Write a.txt","kind":"edit"},"options":[{"optionId":"proceed_always","name":"Always","kind":"allow_always"},{"optionId":"proceed_once","name":"Yes","kind":"allow_once"},{"optionId":"cancel","name":"No","kind":"reject_once"}]}}"#);
        assert!(fx.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Blocked, .. })
        )));
        let ans = sent(&a.answer(Decision::Allow));
        assert_eq!(
            ans[0],
            json!({ "jsonrpc": "2.0", "id": 0, "result": { "outcome": { "outcome": "selected", "optionId": "proceed_once" } } })
        );
        let fx = a.on_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"diff","path":"/w/a.txt","oldText":"a\n","newText":"b\n"}]}}}"#);
        assert!(
            matches!(&fx[0], Effect::Item(i) if matches!(&i.body, ChatBody::Tool { status: ToolStatus::Done, diff: Some(d), files, .. } if d.contains("-a\n+b") && files == &["/w/a.txt"]))
        );
        let end = a.on_line(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#);
        assert!(
            end.contains(&status(AgentStatus::Idle))
                && end.contains(&Effect::Signal(Signal::TurnEnd))
        );
        // Unoffered capabilities are refused.
        assert_eq!(
            sent(
                &a.on_line(r#"{"jsonrpc":"2.0","id":5,"method":"fs/read_text_file","params":{}}"#)
            )[0]["error"]["code"],
            -32601
        );
        assert!(Acp::restore(a.save()).unwrap().session.is_some());
    }

    #[test]
    fn resume_loads_and_cancel_answers_the_open_request() {
        let mut a = Acp::new(Opts { resume: Some("old".into()), ..Opts::default() });
        a.start();
        let fx = a.on_line(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}"#);
        assert_eq!(sent(&fx)[0]["method"], "session/load");
        a.on_line(r#"{"jsonrpc":"2.0","id":2,"result":null}"#);
        a.on_line(r#"{"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"toolCall":{"toolCallId":"t"},"options":[]}}"#);
        let c = sent(&a.interrupt());
        assert_eq!(c[0]["result"]["outcome"]["outcome"], "cancelled");
        assert_eq!(c[1]["method"], "session/cancel");
        let err = a.on_line(r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32000,"message":"Authentication required"}}"#);
        assert!(err.iter().any(
            |e| matches!(e, Effect::Signal(Signal::Status { kind: Some(k), .. }) if k == "auth")
        ));
    }

    #[test]
    fn parallel_permission_requests_are_each_answered() {
        let mut a = Acp::new(Opts::default());
        let opts = r#""options":[{"optionId":"y","name":"Yes","kind":"allow_once"},{"optionId":"n","name":"No","kind":"reject_once"}]"#;
        a.on_line(&format!(r#"{{"jsonrpc":"2.0","id":1,"method":"session/request_permission","params":{{"toolCall":{{"toolCallId":"t1","title":"Read a"}},{opts}}}}}"#));
        a.on_line(&format!(r#"{{"jsonrpc":"2.0","id":2,"method":"session/request_permission","params":{{"toolCall":{{"toolCallId":"t2","title":"Write b"}},{opts}}}}}"#));
        let first = a.answer(Decision::Deny);
        assert!(
            matches!(&first[0], Effect::Send(s) if s.contains(r#""id":2"#) && s.contains(r#""optionId":"n""#))
        );
        assert!(first.iter().any(|e| matches!(e, Effect::Signal(Signal::Status { status: AgentStatus::Blocked, detail: Some(d), .. }) if d.contains("Read a"))));
        // An interrupt cancels whatever is still open.
        a.on_line(&format!(r#"{{"jsonrpc":"2.0","id":3,"method":"session/request_permission","params":{{"toolCall":{{"toolCallId":"t3","title":"c"}},{opts}}}}}"#));
        let i = a.interrupt();
        assert_eq!(
            i.iter().filter(|e| matches!(e, Effect::Send(s) if s.contains("cancelled"))).count(),
            2
        );
        assert!(!a.pending());
    }
}
