//! `ptyro mcp`: an MCP server (stdio, newline-delimited JSON-RPC) that gives a lead agent ptyro's
//! orchestration tools (SPEC §15.4, M7.6).
//!
//! ptyro injects it into a lead session (`ptyro new --agent claude --lead` → `--mcp-config`), so it
//! inherits `PTYRO_SESSION_ID`: every call is made as that lead and checked against its run's
//! policy **by the daemon**, which also verifies (from the connecting process's ancestry) that the
//! call really comes from inside that session. Nothing here is trusted.
//!
//! Waits block, emitting `notifications/progress` every 20 s when the client sent a progress
//! token (Claude Code backgrounds a call after 2 minutes; progress keeps it alive). Every create
//! takes an `idempotency_key`, so a retried call never starts a second worker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ptyro_client::Client;
use ptyro_core::Error;
use ptyro_protocol::{Reply, Request};
use ptyro_run::wire::LeadOp;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::cli::Ctx;

/// MCP revisions we speak; the newest the client asks for wins (2026-07-28 is stateless and
/// works without `initialize`, so it is fine too).
const VERSIONS: [&str; 5] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const CHAT_INSTRUCTIONS: &str = "Your user's other chats with you are searchable here: chat_search finds messages (words, \"phrases\", prefix*, -word, agent:codex, since:7d), chat_read reads a chat around a message, chat_list lists the recent ones. Use them when the user refers to something said before (\"as we discussed\", \"last week\"), and cite what you use as c:HANDLE/NODE.";

/// The chat history tools (SPEC §22.10): what a chat's agent (and a lead whose policy says
/// `chat = true`) may use. The daemon decides what they see (`[chat] history_scope`).
fn chat_tools() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "integer", "description": d });
    let b = |d: &str| json!({ "type": "boolean", "description": d });
    vec![
        tool(
            "chat_search",
            "Search the user's chats: words (all must match), \"a phrase\", prefix*, -word, agent:codex, since:7d. Returns the best messages with a snippet and a citation (c:HANDLE/NODE) each.",
            json!({
                "query": s("What to look for"),
                "chat": s("Only this chat (c:HANDLE)"),
                "limit": n("At most this many (default 10)"),
                "machines": b("Every linked machine's chats too"),
            }),
            &["query"],
            true,
        ),
        tool(
            "chat_read",
            "Read a chat: its messages around one (a citation c:HANDLE/NODE), or its latest ones.",
            json!({
                "chat": s("c:HANDLE, or a citation c:HANDLE/NODE to read around"),
                "around": n("Messages before and after it (default 6)"),
            }),
            &["chat"],
            true,
        ),
        tool(
            "chat_list",
            "The user's recent chats: handle, title, agent, when.",
            json!({ "limit": n("At most this many (default 20)"), "pinned": b("Only pinned ones") }),
            &[],
            true,
        ),
    ]
}

/// A snippet's match marks as Markdown bold.
fn marked(s: &str) -> String {
    s.replace([ptyro_core::chats::MARK_ON, ptyro_core::chats::MARK_OFF], "**")
}

/// Run a chat tool.
async fn chat_call(client: &Client, name: &str, a: &Value) -> Result<Value, Error> {
    use ptyro_core::chat::ChatBody;
    match name {
        "chat_search" => {
            let req = Request::ChatSearch {
                query: need(a, "query")?,
                chat: arg(a, "chat").map(str::to_owned),
                workspace: None,
                agent: None,
                pinned: false,
                archived: true,
                limit: Some(a["limit"].as_u64().unwrap_or(10).min(50) as u32),
                machines: a["machines"].as_bool().unwrap_or(false),
                profile: None,
            };
            let Reply::ChatHits { hits } = client.request(req).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            Ok(json!({ "hits": hits.iter().map(|h| json!({
                "cite": match &h.node {
                    Some(n) => format!("{}/{}", h.chat.addr(), n),
                    None => h.chat.addr(),
                },
                "chat": h.chat.label(),
                "kind": h.kind,
                "snippet": marked(&h.snippet),
                "at_ms": h.at_ms,
            })).collect::<Vec<_>>() }))
        }
        "chat_read" => {
            let chat = need(a, "chat")?;
            let (c, node) = match chat.split_once('/') {
                Some((c, n)) => (c.to_owned(), Some(n.to_owned())),
                None => (chat.clone(), None),
            };
            let around = a["around"].as_u64().unwrap_or(6).min(40) as usize;
            let Reply::ChatTranscript { info, nodes, .. } = client
                .request(Request::ChatRead {
                    chat: c,
                    all: node.is_some(),
                    from: None,
                    limit: None,
                })
                .await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            // The branch through the node asked about (or the shown one's end).
            let path: Vec<&ptyro_core::chats::ChatNode> = match &node {
                Some(n) => {
                    let by_id: std::collections::HashMap<&str, &ptyro_core::chats::ChatNode> =
                        nodes.iter().map(|x| (x.id.as_str(), x)).collect();
                    let mut p = Vec::new();
                    let mut at = nodes.iter().find(|x| {
                        ptyro_core::lookup::id_matches(&x.id, &ptyro_core::lookup::key(n))
                    });
                    while let Some(x) = at {
                        p.push(x);
                        at = x.parent.as_deref().and_then(|q| by_id.get(q).copied());
                    }
                    p.reverse();
                    p
                }
                None => nodes.iter().collect(),
            };
            let msgs: Vec<&&ptyro_core::chats::ChatNode> = path
                .iter()
                .filter(|x| matches!(x.body, ChatBody::User { .. } | ChatBody::Assistant { .. }))
                .collect();
            let end = msgs.len();
            let start = end.saturating_sub(around * 2 + 1);
            Ok(json!({
                "chat": info.addr(),
                "title": info.label(),
                "messages": msgs[start..].iter().map(|x| {
                    let (who, text) = match &x.body {
                        ChatBody::User { text } => ("user", text.as_str()),
                        ChatBody::Assistant { text, .. } => ("assistant", text.as_str()),
                        _ => ("", ""),
                    };
                    json!({ "cite": format!("{}/{}", info.addr(), x.id), "from": who, "text": ptyro_core::chats::clip(text, 4000) })
                }).collect::<Vec<_>>(),
            }))
        }
        "chat_list" => {
            let req = Request::ChatList {
                workspace: None,
                all: true,
                pinned: a["pinned"].as_bool().unwrap_or(false),
                archived: false,
                limit: Some(a["limit"].as_u64().unwrap_or(20).min(100) as u32),
            };
            let Reply::Chats { chats } = client.request(req).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            Ok(json!({ "chats": chats.iter().filter(|c| !c.incognito).map(|c| json!({
                "chat": c.addr(), "title": c.label(), "agent": c.agent, "last_ms": c.last_ms,
            })).collect::<Vec<_>>() }))
        }
        other => Err(Error::invalid(format!("no tool named {other}"))),
    }
}

const INSTRUCTIONS: &str = "ptyro runs your workers: each in its own git worktree, under a policy the daemon enforces (how many, which agents, where). Start workers with spawn_session, give them follow-ups with send, wait_for them to finish their turn, read their screens or worktree_diff their changes, and check_messages for what they report (they reach you with `ptyro msg send @lead`). Ask the human with request_gate; you can't approve anything yourself. Leases (claim) stop two workers editing the same files.";

fn tool(name: &str, description: &str, props: Value, required: &[&str], always: bool) -> Value {
    let mut t = json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
    });
    if always {
        t["_meta"] = json!({ "anthropic/alwaysLoad": true });
    }
    t
}

/// The tools, in a fixed order (prompt caches like stable tool lists).
fn tools() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "integer", "description": d });
    vec![
        tool(
            "list_sessions",
            "Your workers: name, agent, status (working/idle/blocked/exited), worktree, last message.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "spawn_session",
            "Start a worker agent in a fresh git worktree (or one of this run's) with a first prompt. Policy-checked: allowed agents, how many, where.",
            json!({
                "agent": s("claude, codex, gemini, opencode… (optionally agent@profile)"),
                "prompt": s("What the worker should do; be complete, it has no other context"),
                "worktree": s("new (default), root, or a worktree of this run"),
                "base": s("Base ref for a new worktree (default: the project's)"),
                "name": s("A name for the worker"),
                "idempotency_key": s("Retrying with the same key returns the same worker"),
            }),
            &["agent", "prompt"],
            true,
        ),
        tool(
            "send",
            "Send a worker a follow-up (delivered once its current turn is over).",
            json!({ "session": s("worker name"), "text": s("the message") }),
            &["session", "text"],
            true,
        ),
        tool(
            "wait_for",
            "Block until the workers reach a state: idle (turn over; default), blocked, exited, or any_change.",
            json!({
                "sessions": { "type": "array", "items": { "type": "string" }, "description": "worker names" },
                "until": s("idle | blocked | exited | any_change"),
                "timeout_s": n("give up after this long (default 600)"),
            }),
            &["sessions"],
            true,
        ),
        tool(
            "read_screen",
            "A worker's screen now (or the last N lines of its scrollback).",
            json!({ "session": s("worker name"), "scrollback": n("lines of history instead of the screen") }),
            &["session"],
            false,
        ),
        tool(
            "worktree_diff",
            "A worker's changes against its base (by worker or worktree name).",
            json!({ "worktree": s("worker or worktree name") }),
            &["worktree"],
            false,
        ),
        tool(
            "stop_session",
            "Stop a worker (its worktree stays).",
            json!({ "session": s("worker name") }),
            &["session"],
            false,
        ),
        tool(
            "post_message",
            "Message a worker (or @all, @idle, @<agent>, @user). Agents' messages never count as permission.",
            json!({
                "to": s("worker name, @all, @idle, @codex, @user"),
                "body": s("the message"),
                "kind": s("dispatch | question | status | escalation | note"),
                "in_reply_to": s("message id"),
                "priority": s("normal | interrupt"),
                "idempotency_key": s("sending again with this key doesn't send twice"),
            }),
            &["to", "body"],
            false,
        ),
        tool(
            "scratchpad_read",
            "The owner's scratchpad: one Markdown notes file for ideas and bugs (also $PTYRO_SCRATCHPAD). Its text and revision.",
            json!({}),
            &[],
            false,
        ),
        tool(
            "scratchpad_todo",
            "The scratchpad's open `- [ ]` items with line numbers (`all` for ticked ones too).",
            json!({ "all": json!({ "type": "boolean", "description": "ticked items too" }) }),
            &[],
            false,
        ),
        tool(
            "scratchpad_add",
            "Add a line to the owner's scratchpad, stamped with your name and worktree: a `- [ ]` item (a bug you found, a follow-up), or a plain note. Don't edit the file otherwise.",
            json!({ "text": s("what to note"), "section": s("under this heading"), "note": json!({ "type": "boolean", "description": "a plain bullet, not an item" }) }),
            &["text"],
            false,
        ),
        tool(
            "check_messages",
            "Messages for you (workers report with `ptyro msg send @lead`), after a cursor; can wait for the first one.",
            json!({ "since": s("last message id you saw"), "wait_s": n("wait up to this long for one") }),
            &[],
            true,
        ),
        tool(
            "ack",
            "Mark messages handled.",
            json!({ "ids": { "type": "array", "items": { "type": "string" } } }),
            &["ids"],
            false,
        ),
        tool(
            "request_gate",
            "Ask the human a question and wait for the answer (shown in their inbox, TUI and phone).",
            json!({
                "question": s("what to decide"),
                "options": { "type": "array", "items": { "type": "string" }, "description": "answers (default approve/reject)" },
                "timeout_s": n("reject after this long"),
            }),
            &["question"],
            false,
        ),
        tool(
            "claim",
            "Lease files for a worker so others' edits to them are denied.",
            json!({
                "session": s("the worker who gets them"),
                "paths": { "type": "array", "items": { "type": "string" }, "description": "paths or globs (src/api/**)" },
                "exclusive": { "type": "boolean" },
                "ttl_s": n("how long (default 30 min)"),
                "reason": s("why"),
            }),
            &["session", "paths"],
            false,
        ),
        tool(
            "release",
            "Release a worker's leases (all, or these paths).",
            json!({ "session": s("worker"), "paths": { "type": "array", "items": { "type": "string" } } }),
            &["session"],
            false,
        ),
        tool(
            "fanout",
            "The same task to several agents at once, each in its own worktree; compare them after.",
            json!({
                "agents": { "type": "array", "items": { "type": "string" } },
                "prompt": s("the task"),
                "test": s("a check to rank them by (scripts.run.test or a command)"),
            }),
            &["agents", "prompt"],
            false,
        ),
        tool(
            "compare",
            "A fan-out's candidates ranked by evidence (tests, repro, diff size, time), with reasons.",
            json!({ "run": s("the fan-out's run id") }),
            &["run"],
            false,
        ),
        tool(
            "policy",
            "What you may do: limits, allowed agents and worktrees, and what you've used.",
            json!({}),
            &[],
            false,
        ),
    ]
}

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn need(a: &Value, k: &str) -> Result<String, Error> {
    arg(a, k).map(str::to_owned).ok_or_else(|| Error::invalid(format!("missing {k:?}")))
}

fn strings(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn secs(a: &Value, k: &str) -> Option<u64> {
    a.get(k).and_then(Value::as_u64).map(|s| s.saturating_mul(1000))
}

struct Server {
    client: Client,
    lead: String,
    /// The chat history tools: only them (a chat's agent), or beside a lead's (its policy).
    chat: ChatTools,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChatTools {
    None,
    Also,
    Only,
}

impl Server {
    async fn lead(&self, op: LeadOp) -> Result<Value, Error> {
        match self.client.request(Request::Lead { lead: self.lead.clone(), op }).await? {
            Reply::LeadResult { value } => Ok(value),
            other => Err(crate::cli::unexpected(other)),
        }
    }

    async fn call(&self, name: &str, a: &Value) -> Result<Value, Error> {
        if name.starts_with("chat_") && self.chat != ChatTools::None {
            return chat_call(&self.client, name, a).await;
        }
        if self.chat == ChatTools::Only {
            return Err(Error::invalid(format!("no tool named {name}")));
        }
        match name {
            "list_sessions" => self.lead(LeadOp::ListSessions).await,
            "policy" => self.lead(LeadOp::Policy).await,
            "spawn_session" => {
                self.lead(LeadOp::Spawn {
                    agent: need(a, "agent")?,
                    prompt: need(a, "prompt")?,
                    worktree: arg(a, "worktree").map(str::to_owned),
                    base: arg(a, "base").map(str::to_owned),
                    name: arg(a, "name").map(str::to_owned),
                    key: arg(a, "idempotency_key").map(str::to_owned),
                })
                .await
            }
            "send" => {
                self.lead(LeadOp::Send { session: need(a, "session")?, text: need(a, "text")? })
                    .await
            }
            "wait_for" => {
                self.lead(LeadOp::Wait {
                    sessions: strings(a, "sessions"),
                    until: arg(a, "until").map(str::to_owned),
                    timeout_ms: secs(a, "timeout_s"),
                })
                .await
            }
            "read_screen" => {
                self.lead(LeadOp::Read {
                    session: need(a, "session")?,
                    scrollback: a.get("scrollback").and_then(Value::as_u64).map(|n| n as u32),
                })
                .await
            }
            "worktree_diff" => self.lead(LeadOp::Diff { worktree: need(a, "worktree")? }).await,
            "stop_session" => self.lead(LeadOp::Stop { session: need(a, "session")? }).await,
            "request_gate" => {
                self.lead(LeadOp::Gate {
                    question: need(a, "question")?,
                    options: strings(a, "options"),
                    timeout_ms: secs(a, "timeout_s"),
                })
                .await
            }
            "fanout" => {
                self.lead(LeadOp::Fanout {
                    agents: strings(a, "agents"),
                    prompt: need(a, "prompt")?,
                    test: arg(a, "test").map(str::to_owned),
                })
                .await
            }
            "compare" => self.lead(LeadOp::Compare { run: need(a, "run")? }).await,
            "post_message" => {
                let r = self
                    .client
                    .request(Request::MsgPost {
                        from: Some(self.lead.clone()),
                        to: need(a, "to")?,
                        kind: arg(a, "kind").map(str::to_owned),
                        body: need(a, "body")?,
                        run: None,
                        in_reply_to: arg(a, "in_reply_to").map(str::to_owned),
                        priority: arg(a, "priority").map(str::to_owned),
                        ttl_ms: None,
                        key: arg(a, "idempotency_key").map(str::to_owned),
                    })
                    .await?;
                match r {
                    Reply::Posted { ids, to } => Ok(json!({ "ids": ids, "to": to })),
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            "scratchpad_read" => match self.client.request(Request::ScratchGet).await? {
                Reply::Scratch { doc } => {
                    Ok(json!({ "path": doc.path, "rev": doc.rev, "text": doc.text }))
                }
                other => Err(crate::cli::unexpected(other)),
            },
            "scratchpad_todo" => {
                let all = a.get("all").and_then(Value::as_bool).unwrap_or(false);
                match self.client.request(Request::ScratchTodo { all }).await? {
                    Reply::ScratchItems { rev, items } => Ok(json!({ "rev": rev, "items": items })),
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            "scratchpad_add" => {
                let req = Request::ScratchAdd {
                    text: need(a, "text")?,
                    section: arg(a, "section").map(str::to_owned),
                    note: a.get("note").and_then(Value::as_bool).unwrap_or(false),
                    stamp: true,
                    session: Some(self.lead.clone()),
                    cwd: None,
                };
                match self.client.request(req).await? {
                    Reply::Scratch { doc } => Ok(json!({ "added": true, "open": doc.open })),
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            "check_messages" => {
                let r = self
                    .client
                    .request(Request::MsgCheck {
                        me: self.lead.clone(),
                        since: arg(a, "since").map(str::to_owned),
                        wait_ms: secs(a, "wait_s"),
                    })
                    .await?;
                match r {
                    Reply::Messages { messages } => {
                        let cursor = messages.last().map(|m| m.id.clone());
                        Ok(json!({ "messages": messages, "cursor": cursor }))
                    }
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            "ack" => {
                self.client
                    .request(Request::MsgAck { me: self.lead.clone(), ids: strings(a, "ids") })
                    .await?;
                Ok(json!({ "ok": true }))
            }
            "claim" => {
                let r = self
                    .client
                    .request(Request::LeaseClaim {
                        holder: need(a, "session")?,
                        patterns: strings(a, "paths"),
                        exclusive: a.get("exclusive").and_then(Value::as_bool).unwrap_or(true),
                        ttl_ms: secs(a, "ttl_s"),
                        reason: arg(a, "reason").map(str::to_owned),
                    })
                    .await?;
                match r {
                    Reply::Leases { leases, refused } => {
                        Ok(json!({ "leases": leases, "refused": refused }))
                    }
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            "release" => {
                let r = self
                    .client
                    .request(Request::LeaseRelease {
                        holder: need(a, "session")?,
                        patterns: strings(a, "paths"),
                    })
                    .await?;
                match r {
                    Reply::Leases { leases, .. } => Ok(json!({ "leases": leases })),
                    other => Err(crate::cli::unexpected(other)),
                }
            }
            other => Err(Error::invalid(format!("no tool named {other:?}"))),
        }
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub async fn serve(ctx: &Ctx, chat_only: bool) -> Result<i32, Error> {
    let lead = std::env::var("PTYRO_SESSION_ID").map_err(|_| {
        Error::invalid("`ptyro mcp` runs inside a ptyro lead session (ptyro new --agent claude --lead), or a chat's agent, which start it for you")
    })?;
    let client = ctx.connect(false).await?;
    let chat = if chat_only {
        ChatTools::Only
    } else if std::env::var("PTYRO_CHAT_TOOLS").as_deref() == Ok("1") {
        ChatTools::Also
    } else {
        ChatTools::None
    };
    let server = Arc::new(Server { client, lead, chat });
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(v) = rx.recv().await {
            let mut line = v.to_string();
            line.push('\n');
            if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
                return;
            }
        }
    });
    // Tool calls in flight, by request id: cancelled one by one, or all when the client leaves.
    let mut calls: HashMap<String, tokio::task::AbortHandle> = HashMap::new();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        calls.retain(|_, h| !h.is_finished());
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(error(&Value::Null, -32700, &format!("parse error: {e}")));
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or_default().to_owned();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = id else {
            // A notification (initialized, cancelled…): a cancelled call gets no reply.
            if method == "notifications/cancelled"
                && let Some(h) = calls.remove(&params["requestId"].to_string())
            {
                h.abort();
            }
            continue;
        };
        match method.as_str() {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let version =
                    VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or("2025-06-18");
                let _ = tx.send(reply(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "ptyro", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": match server.chat {
                            ChatTools::Only => CHAT_INSTRUCTIONS,
                            _ => INSTRUCTIONS,
                        },
                    }),
                ));
            }
            "ping" => {
                let _ = tx.send(reply(&id, json!({})));
            }
            "tools/list" => {
                let mut list = match server.chat {
                    ChatTools::Only => Vec::new(),
                    _ => tools(),
                };
                if server.chat != ChatTools::None {
                    list.extend(chat_tools());
                }
                let _ = tx.send(reply(&id, json!({ "tools": list })));
            }
            "tools/call" => {
                let (server, tx) = (Arc::clone(&server), tx.clone());
                let key = id.to_string();
                let task = tokio::spawn(async move {
                    let name = params["name"].as_str().unwrap_or_default().to_owned();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let token = params["_meta"]["progressToken"].clone();
                    // Long calls report progress so the client doesn't give up on them.
                    let call = server.call(&name, &args);
                    tokio::pin!(call);
                    let mut n = 0;
                    let result = loop {
                        tokio::select! {
                            r = &mut call => break r,
                            _ = tokio::time::sleep(Duration::from_secs(20)), if !token.is_null() => {
                                n += 1;
                                let _ = tx.send(json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": token, "progress": n, "message": "still waiting" } }));
                            }
                        }
                    };
                    let out = match result {
                        Ok(v) => json!({
                            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                            "structuredContent": if v.is_object() { v.clone() } else { json!({ "result": v }) },
                            "isError": false,
                        }),
                        // Tool errors are results the model reads, not protocol errors.
                        Err(e) => {
                            json!({ "content": [{ "type": "text", "text": e.message }], "isError": true })
                        }
                    };
                    let _ = tx.send(reply(&id, out));
                });
                calls.insert(key, task.abort_handle());
            }
            "resources/list" => {
                let _ = tx.send(reply(&id, json!({ "resources": [] })));
            }
            "prompts/list" => {
                let _ = tx.send(reply(&id, json!({ "prompts": [] })));
            }
            other => {
                let _ = tx.send(error(&id, -32601, &format!("method not found: {other}")));
            }
        }
    }
    // The client is gone: nobody will read what calls still waiting would say.
    for h in calls.into_values() {
        h.abort();
    }
    drop(tx);
    let _ = writer.await;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_are_well_formed() {
        for t in [tools(), chat_tools()] {
            well_formed(&t);
        }
        let mut all = tools();
        all.extend(chat_tools());
        well_formed(&all);
        assert_eq!(marked("a \u{2}hit\u{3} b"), "a **hit** b");
    }

    fn well_formed(t: &[Value]) {
        let names: Vec<&str> = t.iter().map(|x| x["name"].as_str().unwrap()).collect();
        for x in t {
            assert!(x["description"].as_str().unwrap().len() < 4096);
            assert_eq!(x["inputSchema"]["type"], "object");
            for r in x["inputSchema"]["required"].as_array().unwrap() {
                assert!(
                    x["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(),
                    "{} requires {r}",
                    x["name"]
                );
            }
        }
        let mut sorted = names.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "unique names");
    }
}
