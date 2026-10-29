//! OpenCode over its own server (M10.5): `opencode serve --port P --hostname 127.0.0.1` with
//! `OPENCODE_SERVER_PASSWORD`, driven over HTTP while `GET /event` streams what happens.
//! **[docs, unverified: OpenCode isn't installed here; tested against the fake]**
//! - `POST /session {}` → `{id}` (or continue a known `ses_…` id);
//! - `POST /session/:id/prompt_async {parts: [{type: "text", text}]}`;
//! - events: `message.part.updated {part: {type: text|reasoning|tool, …}}`,
//!   `message.updated {info: {cost, tokens}}`, `session.status`, `session.idle`,
//!   `permission.asked`/`permission.updated`, `permission.replied`, `session.error`;
//! - `POST /session/:id/permissions/:permissionID {response: once|always|reject}`;
//! - `POST /session/:id/abort`.

use ptyro_agents::Decision;
use ptyro_core::chat::{ChatBody, NoticeLevel, PermOption, ToolStatus};
use ptyro_core::status::{AgentStatus, Signal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{item, notice, one_line, status, text_of};
use crate::{Driver, Effect, Opts};

const CREATE: u32 = 1;
const PROMPT: u32 = 2;
const ANSWER: u32 = 3;
const ABORT: u32 = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenCode {
    session: Option<String>,
    /// Over `POST /session`'s body (a chat's permission rules).
    #[serde(default)]
    params: Option<Value>,
    queued: Vec<String>,
    /// Open permission: (permission id, item id, title).
    /// The open permission request: (id, chat item, title).
    pending: Option<(String, String, String)>,
    /// Requests that came while another was open: answered in turn, newest first.
    #[serde(default)]
    waiting: Vec<(String, String, String)>,
    /// The last assistant message's cost and tokens (a turn's end).
    cost: Option<f64>,
    tokens: (Option<u64>, Option<u64>),
    last_text: Option<String>,
    working: bool,
    n: u64,
}

impl OpenCode {
    /// After an answer: the next waiting request (blocked again, on that one), or work.
    fn next_or_working(&mut self) -> Vec<Effect> {
        match self.waiting.pop() {
            Some(n) => {
                let title = n.2.clone();
                self.pending = Some(n);
                vec![Effect::Signal(Signal::blocked("permission", Some(title)))]
            }
            None => vec![status(AgentStatus::Working)],
        }
    }

    pub fn new(opts: Opts) -> Self {
        Self { session: opts.resume, params: opts.params, ..Self::default() }
    }

    pub fn restore(v: Value) -> Option<Self> {
        serde_json::from_value(v).ok()
    }

    fn prompt_req(&self, text: &str) -> Effect {
        Effect::Http {
            method: "POST",
            path: format!("/session/{}/prompt_async", self.session.as_deref().unwrap_or("")),
            body: Some(json!({ "parts": [{ "type": "text", "text": text }] })),
            tag: PROMPT,
        }
    }

    fn turn_end(&mut self) -> Vec<Effect> {
        if !self.working {
            return vec![status(AgentStatus::Idle)];
        }
        self.working = false;
        self.n += 1;
        let mut out = vec![
            item(
                format!("turn-{}", self.n),
                ChatBody::TurnEnd {
                    stop: "end_turn".into(),
                    cost_micro_usd: self.cost.take().map(|c| (c * 1e6).round() as u64),
                    duration_ms: None,
                    input_tokens: self.tokens.0,
                    output_tokens: self.tokens.1,
                },
            ),
            status(AgentStatus::Idle),
            Effect::Signal(Signal::TurnEnd),
        ];
        if let Some(t) = self.last_text.take() {
            out.push(Effect::Signal(Signal::Notify { text: one_line(&t, 200) }));
        }
        out
    }

    fn part(&mut self, p: &Value) -> Vec<Effect> {
        let id = p["id"].as_str().unwrap_or("part").to_owned();
        match p["type"].as_str() {
            Some("text") => {
                let text = p["text"].as_str().unwrap_or("").to_owned();
                if text.is_empty() {
                    return Vec::new();
                }
                self.last_text = Some(text.clone());
                let done = p["time"]["end"].is_number();
                vec![item(id, ChatBody::Assistant { text, streaming: !done })]
            }
            Some("reasoning") => vec![item(
                id,
                ChatBody::Thinking { text: p["text"].as_str().unwrap_or("").to_owned() },
            )],
            Some("tool") => {
                let st = &p["state"];
                let status = match st["status"].as_str() {
                    Some("completed") => ToolStatus::Done,
                    Some("error") => ToolStatus::Failed,
                    Some("running") => ToolStatus::Running,
                    _ => ToolStatus::Pending,
                };
                let name = p["tool"].as_str().unwrap_or("tool").to_owned();
                let input = &st["input"];
                let title = st["title"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        ["command", "filePath", "path", "pattern", "url"]
                            .iter()
                            .find_map(|k| input[*k].as_str().map(str::to_owned))
                    })
                    .unwrap_or_else(|| name.clone());
                let files =
                    input["filePath"].as_str().map(|f| vec![f.to_owned()]).unwrap_or_default();
                let output = st["output"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| st["error"].as_str().map(str::to_owned));
                let diff = st["metadata"]["diff"].as_str().map(str::to_owned);
                let key = p["callID"].as_str().map_or(id, str::to_owned);
                let mut out = Vec::new();
                if status == ToolStatus::Running {
                    out.push(Effect::Signal(Signal::Activity {
                        text: format!("{name}: {}", one_line(&title, 120)),
                    }));
                }
                out.push(item(
                    key,
                    ChatBody::Tool {
                        name,
                        title: one_line(&title, 160),
                        status,
                        output,
                        diff,
                        files,
                    },
                ));
                out
            }
            _ => Vec::new(),
        }
    }

    fn mine(&self, props: &Value) -> bool {
        let sid = props["sessionID"]
            .as_str()
            .or_else(|| props["part"]["sessionID"].as_str())
            .or_else(|| props["info"]["sessionID"].as_str());
        sid.is_none() || sid == self.session.as_deref()
    }
}

impl Driver for OpenCode {
    fn protocol(&self) -> &'static str {
        "opencode"
    }

    fn start(&mut self) -> Vec<Effect> {
        match &self.session {
            Some(s) => vec![
                Effect::Signal(Signal::Conversation { id: s.clone() }),
                status(AgentStatus::Idle),
            ],
            None => vec![Effect::Http {
                method: "POST",
                path: "/session".into(),
                body: Some(crate::merged(json!({}), self.params.as_ref())),
                tag: CREATE,
            }],
        }
    }

    fn http_reply(&mut self, tag: u32, code: u16, body: &str) -> Vec<Effect> {
        if code >= 400 {
            self.n += 1;
            let what = match tag {
                CREATE => "starting a session",
                PROMPT => "sending the prompt",
                ANSWER => "answering",
                _ => "a request",
            };
            let mut out = vec![notice(
                format!("err-{}", self.n),
                NoticeLevel::Error,
                format!("OpenCode: {what} failed (HTTP {code}) {}", one_line(body, 160)),
            )];
            if tag == CREATE || tag == PROMPT {
                out.push(Effect::Signal(Signal::Status {
                    status: AgentStatus::Errored,
                    kind: Some(if code == 401 { "auth" } else { "error" }.into()),
                    detail: Some(format!("HTTP {code}")),
                    progress: None,
                }));
            }
            return out;
        }
        if tag != CREATE {
            return Vec::new();
        }
        let Ok(v) = serde_json::from_str::<Value>(body) else { return Vec::new() };
        let Some(id) = v["id"].as_str() else { return Vec::new() };
        self.session = Some(id.to_owned());
        let mut out = vec![Effect::Signal(Signal::Conversation { id: id.to_owned() })];
        let queued = std::mem::take(&mut self.queued);
        if queued.is_empty() {
            out.push(status(AgentStatus::Idle));
        }
        for q in queued {
            out.push(self.prompt_req(&q));
        }
        out
    }

    fn on_line(&mut self, line: &str) -> Vec<Effect> {
        let Ok(ev) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        let p = &ev["properties"];
        if !self.mine(p) {
            return Vec::new();
        }
        match ev["type"].as_str() {
            Some("message.part.updated") => self.part(&p["part"]),
            Some("message.updated") => {
                let info = &p["info"];
                if info["role"] == "assistant" {
                    if let Some(c) = info["cost"].as_f64() {
                        self.cost = Some(c);
                    }
                    self.tokens =
                        (info["tokens"]["input"].as_u64(), info["tokens"]["output"].as_u64());
                }
                Vec::new()
            }
            Some("session.status") => match p["status"]["type"].as_str() {
                Some("busy" | "retry") => {
                    self.working = true;
                    vec![status(AgentStatus::Working)]
                }
                Some("idle") => self.turn_end(),
                _ => Vec::new(),
            },
            Some("session.idle") => self.turn_end(),
            Some("permission.asked" | "permission.updated") => {
                let pid = p["id"].as_str().unwrap_or("").to_owned();
                let title = p["title"].as_str().unwrap_or("a tool call").to_owned();
                let key = format!("perm-{pid}");
                // An update of the open request replaces it; a new one queues the open one.
                let next = (pid.clone(), key.clone(), title.clone());
                match self.pending.replace(next) {
                    Some(earlier) if earlier.0 != pid => self.waiting.push(earlier),
                    _ => {}
                }
                self.waiting.retain(|w| w.0 != pid);
                vec![
                    item(
                        key,
                        ChatBody::Permission {
                            request: pid,
                            tool: p["type"].as_str().unwrap_or("tool").to_owned(),
                            title: title.clone(),
                            options: PermOption::standard(),
                            decided: None,
                        },
                    ),
                    Effect::Signal(Signal::blocked("permission", Some(title))),
                ]
            }
            Some("permission.replied") => {
                // Answered elsewhere (OpenCode's own UI): that one, wherever it waits.
                let pid = p["id"].as_str().or(p["permissionID"].as_str()).unwrap_or("").to_owned();
                self.waiting.retain(|w| w.0 != pid);
                if self.pending.as_ref().is_some_and(|x| pid.is_empty() || x.0 == pid) {
                    self.pending = None;
                    return self.next_or_working();
                }
                Vec::new()
            }
            Some("session.error") => {
                self.n += 1;
                let msg = text_of(&p["error"]["data"]["message"]);
                let msg = if msg.is_empty() {
                    p["error"]["name"].as_str().unwrap_or("error").to_owned()
                } else {
                    msg
                };
                vec![
                    notice(format!("err-{}", self.n), NoticeLevel::Error, msg.clone()),
                    Effect::Signal(Signal::Status {
                        status: AgentStatus::Errored,
                        kind: Some("error".into()),
                        detail: Some(msg),
                        progress: None,
                    }),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn prompt(&mut self, text: &str) -> Vec<Effect> {
        self.n += 1;
        self.working = true;
        let mut out = vec![
            item(format!("you-{}", self.n), ChatBody::User { text: text.to_owned() }),
            Effect::Signal(Signal::Prompt { text: text.to_owned() }),
            status(AgentStatus::Working),
        ];
        if self.session.is_some() {
            out.push(self.prompt_req(text));
        } else {
            self.queued.push(text.to_owned());
        }
        out
    }

    fn answer(&mut self, d: Decision) -> Vec<Effect> {
        let Some((pid, key, title)) = self.pending.take() else { return Vec::new() };
        let response = match d {
            Decision::Allow => "once",
            Decision::Always => "always",
            Decision::Deny => "reject",
        };
        let mut out = vec![
            Effect::Http {
                method: "POST",
                path: format!(
                    "/session/{}/permissions/{pid}",
                    self.session.as_deref().unwrap_or("")
                ),
                body: Some(json!({ "response": response })),
                tag: ANSWER,
            },
            item(
                key,
                ChatBody::Permission {
                    request: pid,
                    tool: String::new(),
                    title,
                    options: PermOption::standard(),
                    decided: Some(response.into()),
                },
            ),
        ];
        out.extend(self.next_or_working());
        out
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        match &self.session {
            Some(s) => vec![Effect::Http {
                method: "POST",
                path: format!("/session/{s}/abort"),
                body: None,
                tag: ABORT,
            }],
            None => Vec::new(),
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

    #[test]
    fn a_session_a_turn_a_permission() {
        let mut o = OpenCode::new(Opts::default());
        assert!(
            matches!(&o.start()[0], Effect::Http { path, tag: CREATE, .. } if path == "/session")
        );
        let p = o.prompt("hi");
        assert!(
            !p.iter().any(|e| matches!(e, Effect::Http { .. })),
            "queued until the session exists"
        );
        let fx = o.http_reply(CREATE, 200, r#"{"id":"ses_1","title":"x"}"#);
        assert!(fx.iter().any(
            |e| matches!(e, Effect::Http { path, .. } if path == "/session/ses_1/prompt_async")
        ));
        o.on_line(r#"{"type":"session.status","properties":{"sessionID":"ses_1","status":{"type":"busy"}}}"#);
        let t = o.on_line(r#"{"type":"message.part.updated","properties":{"part":{"id":"p1","sessionID":"ses_1","messageID":"m1","type":"tool","callID":"c1","tool":"bash","state":{"status":"running","input":{"command":"ls"}}}}}"#);
        assert!(t.iter().any(|e| matches!(e, Effect::Item(i) if i.id == "c1" && matches!(&i.body, ChatBody::Tool { status: ToolStatus::Running, title, .. } if title == "ls"))));
        let a = o.on_line(r#"{"type":"permission.asked","properties":{"id":"per_1","sessionID":"ses_1","title":"Run ls"}}"#);
        assert!(a.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Blocked, .. })
        )));
        let ans = o.answer(Decision::Always);
        assert!(
            matches!(&ans[0], Effect::Http { path, body: Some(b), .. } if path == "/session/ses_1/permissions/per_1" && b["response"] == "always")
        );
        o.on_line(r#"{"type":"message.part.updated","properties":{"part":{"id":"p2","sessionID":"ses_1","type":"text","text":"Done."}}}"#);
        o.on_line(r#"{"type":"message.updated","properties":{"info":{"id":"m1","sessionID":"ses_1","role":"assistant","cost":0.01,"tokens":{"input":10,"output":3}}}}"#);
        let end = o.on_line(r#"{"type":"session.idle","properties":{"sessionID":"ses_1"}}"#);
        assert!(end.iter().any(|e| matches!(e, Effect::Item(i) if matches!(i.body, ChatBody::TurnEnd { cost_micro_usd: Some(10_000), .. }))));
        assert!(end.contains(&Effect::Signal(Signal::Notify { text: "Done.".into() })));
        // Other sessions' events are ignored.
        assert!(
            o.on_line(r#"{"type":"session.idle","properties":{"sessionID":"ses_2"}}"#).is_empty()
        );
        assert!(matches!(&o.interrupt()[0], Effect::Http { path, .. } if path.ends_with("/abort")));
    }

    #[test]
    fn parallel_permission_requests_are_each_answered() {
        let mut o = OpenCode::new(Opts::default());
        o.http_reply(CREATE, 200, r#"{"id":"ses_1","title":"x"}"#);
        o.on_line(r#"{"type":"permission.asked","properties":{"id":"per_1","sessionID":"ses_1","title":"Run ls"}}"#);
        o.on_line(r#"{"type":"permission.asked","properties":{"id":"per_2","sessionID":"ses_1","title":"Run rm"}}"#);
        // Updating the open one doesn't queue it twice.
        o.on_line(r#"{"type":"permission.updated","properties":{"id":"per_2","sessionID":"ses_1","title":"Run rm x"}}"#);
        let first = o.answer(Decision::Deny);
        assert!(matches!(&first[0], Effect::Http { path, .. } if path.ends_with("/per_2")));
        assert!(first.iter().any(|e| matches!(e, Effect::Signal(Signal::Status { status: AgentStatus::Blocked, detail: Some(d), .. }) if d == "Run ls")));
        // Answered in OpenCode itself: nothing left open.
        let r = o.on_line(
            r#"{"type":"permission.replied","properties":{"id":"per_1","sessionID":"ses_1"}}"#,
        );
        assert!(r.iter().any(|e| matches!(
            e,
            Effect::Signal(Signal::Status { status: AgentStatus::Working, .. })
        )));
        assert!(!o.pending());
    }
}
