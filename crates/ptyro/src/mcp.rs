//! `ptyro mcp`: an MCP server (stdio, newline-delimited JSON-RPC) with the chat history tools
//! (SPEC §22.10). A chat's agent is started with it (`--mcp-config`); the daemon decides what its
//! calls may see, from the connecting process's ancestry. Nothing here is trusted.
//!
//! A long call emits `notifications/progress` every 20 s when the client sent a progress token
//! (Claude Code backgrounds a call after 2 minutes; progress keeps it alive).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ptyro_client::Client;
use ptyro_core::Error;
use ptyro_protocol::{Reply, Request};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::cli::Ctx;

/// MCP revisions we speak; the newest the client asks for wins (2026-07-28 is stateless and
/// works without `initialize`, so it is fine too).
const VERSIONS: [&str; 5] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const CHAT_INSTRUCTIONS: &str = "Your user's other chats with you are searchable here: chat_search finds messages (words, \"phrases\", prefix*, -word, agent:codex, since:7d), chat_read reads a chat around a message, chat_list lists the recent ones. Use them when the user refers to something said before (\"as we discussed\", \"last week\"), and cite what you use as c:HANDLE/NODE.";

/// The chat history tools (SPEC §22.10): what a chat's agent may use. The daemon decides what
/// they see (`[chat] history_scope`).
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
            let Reply::ChatTranscript { info, nodes, .. } =
                client.request(Request::ChatRead { chat: c, from: None, limit: None }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            // The conversation up to the node asked about (or all of it).
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
            let Reply::Chats { chats, .. } = client.request(req).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            Ok(json!({ "chats": chats.iter().filter(|c| !c.incognito).map(|c| json!({
                "chat": c.addr(), "title": c.label(), "agent": c.agent, "last_ms": c.last_ms,
            })).collect::<Vec<_>>() }))
        }
        other => Err(Error::invalid(format!("no tool named {other}"))),
    }
}

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

fn arg<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn need(a: &Value, k: &str) -> Result<String, Error> {
    arg(a, k).map(str::to_owned).ok_or_else(|| Error::invalid(format!("missing {k:?}")))
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub async fn serve(ctx: &Ctx) -> Result<i32, Error> {
    if std::env::var_os("PTYRO_SESSION_ID").is_none() {
        return Err(Error::invalid(
            "`ptyro mcp` serves the chat history tools to a chat's agent, which starts it for you: there is nothing to run it for here",
        ));
    }
    let client = Arc::new(ctx.connect(false).await?);
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
                        "instructions": CHAT_INSTRUCTIONS,
                    }),
                ));
            }
            "ping" => {
                let _ = tx.send(reply(&id, json!({})));
            }
            "tools/list" => {
                let _ = tx.send(reply(&id, json!({ "tools": chat_tools() })));
            }
            "tools/call" => {
                let (client, tx) = (Arc::clone(&client), tx.clone());
                let key = id.to_string();
                let task = tokio::spawn(async move {
                    let name = params["name"].as_str().unwrap_or_default().to_owned();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    let token = params["_meta"]["progressToken"].clone();
                    // Long calls report progress so the client doesn't give up on them.
                    let call = chat_call(&client, &name, &args);
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
        well_formed(&chat_tools());
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
