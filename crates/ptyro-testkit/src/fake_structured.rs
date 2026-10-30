//! The fake agent's structured modes (M10 tests): the same scripted commands as the interactive
//! fake, spoken over each structured protocol. The mode comes from the arguments ptyro passes:
//!
//! | arguments | protocol |
//! |---|---|
//! | `-p --input-format stream-json …` | Claude stream-json |
//! | `app-server` | Codex app-server (JSON-RPC, no `jsonrpc` field) |
//! | `--acp` | ACP (Gemini) |
//! | `serve --port P` | OpenCode's HTTP server + server-sent events |
//!
//! Prompts: `work <secs> <text>` (a Bash tool call, then `worked: <text>`), `ask <cmd>` (a
//! permission request; `answer: yes|no`), `edit <file> <text>` (writes the file; an edit with a
//! diff), `sh <cmd>` (runs it; a Bash tool call with its output), `fail` (the turn errors),
//! anything else (`you said: …`).

use ptyro_core::sync::MutexExt;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

/// One step of a scripted turn.
enum Step {
    Text(String),
    ToolStart {
        id: String,
        title: String,
    },
    ToolEnd {
        id: String,
        title: String,
        ok: bool,
        output: String,
    },
    Edit {
        id: String,
        path: String,
        old: String,
        new: String,
    },
    /// Ask permission for the tool `id`; the answer decides what follows.
    Ask {
        id: String,
        title: String,
    },
    Sleep(f64),
    Fail(String),
}

fn plan(text: &str, n: u64) -> Vec<Step> {
    let (cmd, rest) = text.split_once(' ').unwrap_or((text, ""));
    let id = format!("tool_{n}");
    match cmd {
        "work" => {
            let (secs, what) = rest.split_once(' ').unwrap_or((rest, ""));
            vec![
                Step::ToolStart { id: id.clone(), title: format!("sleep {secs}") },
                Step::Sleep(secs.parse().unwrap_or(0.5)),
                Step::ToolEnd {
                    id: id.clone(),
                    title: format!("sleep {secs}"),
                    ok: true,
                    output: format!("slept {secs}"),
                },
                Step::Text(format!("worked: {what}")),
            ]
        }
        "ask" => vec![
            Step::ToolStart { id: id.clone(), title: rest.to_owned() },
            Step::Ask { id, title: rest.to_owned() },
        ],
        "edit" => {
            let (path, body) = rest.split_once(' ').unwrap_or((rest, ""));
            let old = std::fs::read_to_string(path).unwrap_or_default();
            let new = format!("{body}\n");
            let _ = std::fs::write(path, &new);
            vec![
                Step::Edit { id, path: path.to_owned(), old, new },
                Step::Text(format!("edited {path}")),
            ]
        }
        "sh" => {
            let out = std::process::Command::new("sh").args(["-c", rest]).output();
            let (ok, output) = match out {
                Ok(o) => (
                    o.status.success(),
                    format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                ),
                Err(e) => (false, e.to_string()),
            };
            vec![
                Step::ToolStart { id: id.clone(), title: rest.to_owned() },
                Step::ToolEnd { id, title: rest.to_owned(), ok, output },
                Step::Text(format!("ran: {rest}")),
            ]
        }
        "fail" => vec![Step::Text("failing".into()), Step::Fail("fake failure".into())],
        _ => vec![Step::Text(format!("you said: {text}"))],
    }
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// `Some(exit code)` when `args` ask for a structured mode.
pub fn main(args: &[String]) -> Option<i32> {
    if args.iter().any(|a| a == "--input-format") && args.iter().any(|a| a == "stream-json") {
        return Some(claude(args));
    }
    if args.last().is_some_and(|a| a == "app-server") {
        return Some(codex());
    }
    if args.iter().any(|a| a == "--acp") {
        return Some(acp());
    }
    if args.iter().any(|a| a == "serve") && args.iter().any(|a| a == "--port") {
        return Some(opencode(arg_after(args, "--port").and_then(|p| p.parse().ok()).unwrap_or(0)));
    }
    None
}

fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn lines() -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for l in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&l)
                && tx.send(v).is_err()
            {
                return;
            }
        }
    });
    rx
}

// ── Claude stream-json ────────────────────────────────────────────────────────────────────────

fn claude(args: &[String]) -> i32 {
    let session = arg_after(args, "--resume")
        .or_else(|| arg_after(args, "--session-id"))
        .unwrap_or_else(|| "fake-session".into());
    send(
        &json!({ "type": "system", "subtype": "init", "session_id": session, "model": "fake", "cwd": std::env::current_dir().ok() }),
    );
    let rx = lines();
    let mut n = 0u64;
    while let Ok(m) = rx.recv() {
        if m["type"] != "user" {
            continue;
        }
        let text = m["message"]["content"].as_str().unwrap_or("").to_owned();
        let mut last = String::new();
        let mut failed = None;
        let mut steps = plan(&text, n).into_iter();
        while let Some(step) = steps.next() {
            n += 1;
            match step {
                Step::Text(t) => {
                    let id = format!("msg_{n}");
                    send(
                        &json!({ "type": "stream_event", "event": { "type": "message_start", "message": { "id": id } } }),
                    );
                    send(
                        &json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } } }),
                    );
                    let half =
                        t.char_indices().nth(t.chars().count() / 2).map_or(t.len(), |(i, _)| i);
                    // Paced like a real model, so a viewer sees the text grow.
                    for piece in [&t[..half], &t[half..]] {
                        send(
                            &json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": piece } } }),
                        );
                        std::thread::sleep(Duration::from_millis(80));
                    }
                    send(
                        &json!({ "type": "assistant", "message": { "id": id, "content": [{ "type": "text", "text": t }] }, "parent_tool_use_id": null }),
                    );
                    last = t;
                }
                Step::ToolStart { id, title } => {
                    send(
                        &json!({ "type": "assistant", "message": { "id": format!("msg_{n}"), "content": [{ "type": "tool_use", "id": id, "name": "Bash", "input": { "command": title } }] } }),
                    );
                }
                Step::ToolEnd { id, output, ok, .. } => {
                    send(
                        &json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": output, "is_error": !ok }] } }),
                    );
                }
                Step::Edit { id, path, old, new } => {
                    send(
                        &json!({ "type": "assistant", "message": { "id": format!("msg_{n}"), "content": [{ "type": "tool_use", "id": id, "name": "Edit", "input": { "file_path": path, "old_string": old, "new_string": new } }] } }),
                    );
                    send(
                        &json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": "edited", "is_error": false }] } }),
                    );
                }
                Step::Ask { id, title } => {
                    let req = format!("req_{n}");
                    send(
                        &json!({ "type": "control_request", "request_id": req, "request": { "subtype": "can_use_tool", "tool_name": "Bash", "input": { "command": title }, "tool_use_id": id } }),
                    );
                    let allowed = loop {
                        let Ok(r) = rx.recv() else { return 0 };
                        if r["type"] == "control_response"
                            && r["response"]["request_id"] == req.as_str()
                        {
                            break r["response"]["response"]["behavior"] == "allow";
                        }
                    };
                    let (out, word) = if allowed {
                        (format!("ran {title}"), "yes")
                    } else {
                        ("The user denied this".into(), "no")
                    };
                    send(
                        &json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": out, "is_error": !allowed }] } }),
                    );
                    let t = format!("answer: {word}");
                    send(
                        &json!({ "type": "assistant", "message": { "id": format!("msg_{n}a"), "content": [{ "type": "text", "text": t }] } }),
                    );
                    last = t;
                    let _ = steps.by_ref().count();
                }
                Step::Sleep(secs) => std::thread::sleep(Duration::from_secs_f64(secs)),
                Step::Fail(why) => failed = Some(why),
            }
        }
        match failed {
            Some(why) => send(
                &json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "result": why, "session_id": session }),
            ),
            None => send(
                &json!({ "type": "result", "subtype": "success", "is_error": false, "result": last, "total_cost_usd": 0.0012, "duration_ms": 900, "usage": { "input_tokens": 10, "output_tokens": 5 }, "session_id": session }),
            ),
        }
    }
    0
}

// ── Codex app-server ──────────────────────────────────────────────────────────────────────────

fn codex() -> i32 {
    let rx = lines();
    let mut thread = String::new();
    let mut n = 0u64;
    while let Ok(m) = rx.recv() {
        let id = m["id"].clone();
        match m["method"].as_str() {
            Some("initialize") => send(
                &json!({ "id": id, "result": { "userAgent": "fake-codex/0.143.0", "codexHome": "/tmp", "platformFamily": "unix", "platformOs": "linux" } }),
            ),
            Some("thread/start") => {
                thread = format!("th-{}", std::process::id());
                send(
                    &json!({ "id": id, "result": { "thread": { "id": thread, "status": { "type": "idle" } } } }),
                );
                send(
                    &json!({ "method": "thread/started", "params": { "thread": { "id": thread } } }),
                );
            }
            Some("thread/resume") => {
                thread = m["params"]["threadId"].as_str().unwrap_or("th-x").to_owned();
                send(&json!({ "id": id, "result": { "thread": { "id": thread } } }));
            }
            Some("turn/start") => {
                n += 1;
                let turn = format!("tu-{n}");
                let text = m["params"]["input"][0]["text"].as_str().unwrap_or("").to_owned();
                send(
                    &json!({ "id": id, "result": { "turn": { "id": turn, "status": "inProgress", "items": [] } } }),
                );
                send(
                    &json!({ "method": "turn/started", "params": { "threadId": thread, "turn": { "id": turn, "status": "inProgress", "items": [] } } }),
                );
                let p = |extra: Value| {
                    let mut v = json!({ "threadId": thread, "turnId": turn });
                    for (k, x) in extra.as_object().cloned().unwrap_or_default() {
                        v[k] = x;
                    }
                    v
                };
                let mut status = "completed";
                let mut steps = plan(&text, n).into_iter();
                while let Some(step) = steps.next() {
                    n += 1;
                    match step {
                        Step::Text(t) => {
                            let item = format!("msg-{n}");
                            send(
                                &json!({ "method": "item/started", "params": p(json!({ "item": { "type": "agentMessage", "id": item, "text": "" } })) }),
                            );
                            send(
                                &json!({ "method": "item/agentMessage/delta", "params": p(json!({ "itemId": item, "delta": t })) }),
                            );
                            send(
                                &json!({ "method": "item/completed", "params": p(json!({ "item": { "type": "agentMessage", "id": item, "text": t } })) }),
                            );
                        }
                        Step::ToolStart { id, title } => send(
                            &json!({ "method": "item/started", "params": p(json!({ "item": { "type": "commandExecution", "id": id, "command": title, "commandActions": [], "cwd": "/", "status": "inProgress" } })) }),
                        ),
                        Step::ToolEnd { id, title, ok, output, .. } => {
                            send(
                                &json!({ "method": "item/commandExecution/outputDelta", "params": p(json!({ "itemId": id, "delta": output })) }),
                            );
                            send(
                                &json!({ "method": "item/completed", "params": p(json!({ "item": { "type": "commandExecution", "id": id, "command": title, "commandActions": [], "cwd": "/", "status": if ok { "completed" } else { "failed" }, "exitCode": if ok { 0 } else { 1 } } })) }),
                            );
                        }
                        Step::Edit { id, path, old, new } => {
                            let diff =
                                format!("@@ -1 +1 @@\n-{}\n+{}\n", old.trim_end(), new.trim_end());
                            let item = json!({ "type": "fileChange", "id": id, "status": "completed", "changes": [{ "path": path, "kind": { "type": "update" }, "diff": diff }] });
                            send(
                                &json!({ "method": "item/started", "params": p(json!({ "item": item })) }),
                            );
                            send(
                                &json!({ "method": "item/completed", "params": p(json!({ "item": item })) }),
                            );
                        }
                        Step::Ask { id, title } => {
                            let req = format!("a-{n}");
                            send(
                                &json!({ "id": req, "method": "item/commandExecution/requestApproval", "params": p(json!({ "itemId": id, "command": title, "startedAtMs": 1 })) }),
                            );
                            let decision = loop {
                                let Ok(r) = rx.recv() else { return 0 };
                                if r["id"] == req.as_str() && r.get("method").is_none() {
                                    break r["result"]["decision"]
                                        .as_str()
                                        .unwrap_or("decline")
                                        .to_owned();
                                }
                            };
                            let allowed = decision.starts_with("accept");
                            send(
                                &json!({ "method": "item/completed", "params": p(json!({ "item": { "type": "commandExecution", "id": id, "command": title, "commandActions": [], "cwd": "/", "status": if allowed { "completed" } else { "declined" }, "aggregatedOutput": if allowed { format!("ran {title}") } else { String::new() } } })) }),
                            );
                            let t = format!("answer: {}", if allowed { "yes" } else { "no" });
                            let item = format!("msg-{n}a");
                            send(
                                &json!({ "method": "item/completed", "params": p(json!({ "item": { "type": "agentMessage", "id": item, "text": t } })) }),
                            );
                            let _ = steps.by_ref().count();
                        }
                        Step::Sleep(secs) => std::thread::sleep(Duration::from_secs_f64(secs)),
                        Step::Fail(_) => status = "failed",
                    }
                }
                send(
                    &json!({ "method": "turn/completed", "params": { "threadId": thread, "turn": { "id": turn, "status": status, "items": [], "error": if status == "failed" { json!({ "message": "fake failure" }) } else { Value::Null } } } }),
                );
            }
            Some("turn/interrupt") => send(&json!({ "id": id, "result": {} })),
            Some(_) if !id.is_null() => send(&json!({ "id": id, "result": {} })),
            _ => {}
        }
    }
    0
}

// ── ACP (Gemini) ──────────────────────────────────────────────────────────────────────────────

fn acp() -> i32 {
    let rx = lines();
    let mut session = String::new();
    let mut n = 100u64;
    // The MCP servers `session/new` or `session/load` brought: the prompt `mcp` names them.
    let mut servers: Vec<String> = Vec::new();
    let rpc = |v: Value| {
        let mut v = v;
        v["jsonrpc"] = json!("2.0");
        send(&v);
    };
    while let Ok(m) = rx.recv() {
        let id = m["id"].clone();
        match m["method"].as_str() {
            Some("initialize") => rpc(
                json!({ "id": id, "result": { "protocolVersion": 1, "agentCapabilities": { "loadSession": true }, "authMethods": [], "agentInfo": { "name": "fake-gemini", "version": "0.64.0" } } }),
            ),
            Some(method @ ("session/new" | "session/load"))
                if !m["params"]["mcpServers"].is_null() =>
            {
                servers = m["params"]["mcpServers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        // ACP's stdio shape: a name, a command, args and env as name/value pairs.
                        let ok = s["command"].is_string()
                            && s["args"].is_array()
                            && s["env"]
                                .as_array()
                                .is_some_and(|e| e.iter().all(|v| v["name"].is_string()));
                        format!(
                            "{}{}",
                            s["name"].as_str().unwrap_or("?"),
                            if ok { "" } else { "(malformed)" }
                        )
                    })
                    .collect();
                if method == "session/new" {
                    session = format!("{:08x}-fake", std::process::id());
                    rpc(json!({ "id": id, "result": { "sessionId": session } }));
                } else {
                    session = m["params"]["sessionId"].as_str().unwrap_or("").to_owned();
                    rpc(json!({ "id": id, "result": null }));
                }
            }
            Some("session/new") => {
                session = format!("{:08x}-fake", std::process::id());
                rpc(json!({ "id": id, "result": { "sessionId": session } }));
            }
            Some("session/load") => {
                session = m["params"]["sessionId"].as_str().unwrap_or("").to_owned();
                rpc(json!({ "id": id, "result": null }));
            }
            Some("session/prompt") => {
                let text = m["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_owned();
                let up = |u: Value| json!({ "method": "session/update", "params": { "sessionId": session, "update": u } });
                if text.trim() == "mcp" {
                    let t = format!(
                        "mcp servers: {}",
                        if servers.is_empty() { "none".into() } else { servers.join(", ") }
                    );
                    rpc(up(
                        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": t } }),
                    ));
                    rpc(json!({ "id": id, "result": { "stopReason": "end_turn" } }));
                    continue;
                }
                let mut stop = "end_turn";
                let mut steps = plan(&text, n).into_iter();
                while let Some(step) = steps.next() {
                    n += 1;
                    match step {
                        Step::Text(t) => rpc(up(
                            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": t } }),
                        )),
                        Step::ToolStart { id, title } => rpc(up(
                            json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title, "kind": "execute", "status": "in_progress" }),
                        )),
                        Step::ToolEnd { id, ok, output, .. } => rpc(up(
                            json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": if ok { "completed" } else { "failed" }, "content": [{ "type": "content", "content": { "type": "text", "text": output } }] }),
                        )),
                        Step::Edit { id, path, old, new } => {
                            rpc(up(
                                json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("Write {path}"), "kind": "edit", "status": "in_progress", "locations": [{ "path": path }] }),
                            ));
                            rpc(up(
                                json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": "completed", "content": [{ "type": "diff", "path": path, "oldText": old, "newText": new }] }),
                            ));
                        }
                        Step::Ask { id, title } => {
                            let req = n;
                            rpc(
                                json!({ "id": req, "method": "session/request_permission", "params": { "sessionId": session, "toolCall": { "toolCallId": id, "title": title, "kind": "execute" }, "options": [
                                { "optionId": "proceed_once", "name": "Allow", "kind": "allow_once" },
                                { "optionId": "proceed_always", "name": "Always allow", "kind": "allow_always" },
                                { "optionId": "cancel", "name": "Reject", "kind": "reject_once" }] } }),
                            );
                            let chosen = loop {
                                let Ok(r) = rx.recv() else { return 0 };
                                if r["id"] == req && r.get("method").is_none() {
                                    break r["result"]["outcome"]["optionId"]
                                        .as_str()
                                        .unwrap_or("cancel")
                                        .to_owned();
                                }
                            };
                            let allowed = chosen.starts_with("proceed");
                            rpc(up(
                                json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": if allowed { "completed" } else { "failed" } }),
                            ));
                            rpc(up(
                                json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": format!("answer: {}", if allowed { "yes" } else { "no" }) } }),
                            ));
                            let _ = steps.by_ref().count();
                        }
                        Step::Sleep(secs) => std::thread::sleep(Duration::from_secs_f64(secs)),
                        Step::Fail(_) => stop = "refusal",
                    }
                }
                rpc(json!({ "id": id, "result": { "stopReason": stop } }));
            }
            Some(_) if !id.is_null() => rpc(json!({ "id": id, "result": null })),
            _ => {}
        }
    }
    0
}

// ── OpenCode serve ────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Oc {
    session: String,
    clients: Vec<TcpStream>,
    pending: Option<(String, mpsc::Sender<String>)>,
    n: u64,
}

fn event(st: &Arc<Mutex<Oc>>, v: Value) {
    let mut s = st.locked();
    let msg = format!("data: {v}\n\n");
    s.clients.retain_mut(|c| c.write_all(msg.as_bytes()).and_then(|()| c.flush()).is_ok());
}

fn opencode(port: u16) -> i32 {
    let Ok(l) = TcpListener::bind(("127.0.0.1", port)) else { return 1 };
    let password = std::env::var("OPENCODE_SERVER_PASSWORD").unwrap_or_default();
    let st = Arc::new(Mutex::new(Oc {
        session: format!("ses_{}", std::process::id()),
        ..Oc::default()
    }));
    eprintln!("opencode server listening on http://127.0.0.1:{port}");
    for conn in l.incoming().flatten() {
        let st = Arc::clone(&st);
        let password = password.clone();
        std::thread::spawn(move || handle(conn, &st, &password));
    }
    0
}

fn reply(mut c: &TcpStream, code: &str, body: &str) {
    let _ = write!(
        c,
        "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn handle(conn: TcpStream, st: &Arc<Mutex<Oc>>, password: &str) {
    let mut r = std::io::BufReader::new(conn.try_clone().expect("clone"));
    let mut head = String::new();
    let mut len = 0usize;
    let mut auth = String::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 {
            return;
        }
        if l == "\r\n" {
            break;
        }
        let lower = l.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            auth = l.trim().to_owned();
        }
        if head.is_empty() {
            head = l.trim().to_owned();
        }
    }
    let mut body = vec![0u8; len];
    let _ = r.read_exact(&mut body);
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let want = format!("opencode:{password}");
    let ok_auth =
        password.is_empty() || auth.ends_with(&crate::fake_structured::b64(want.as_bytes()));
    if !ok_auth {
        return reply(&conn, "401 Unauthorized", "{}");
    }
    let mut parts = head.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let sid = st.locked().session.clone();
    match (method, path) {
        ("POST", "/session") => reply(&conn, "200 OK", &json!({ "id": sid }).to_string()),
        ("GET", "/event") => {
            let mut c = conn;
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n"
            );
            let _ = c.flush();
            st.locked().clients.push(c);
        }
        ("POST", p) if p.ends_with("/prompt_async") => {
            let text = body["parts"][0]["text"].as_str().unwrap_or("").to_owned();
            reply(&conn, "204 No Content", "");
            let st = Arc::clone(st);
            std::thread::spawn(move || oc_turn(&st, &text));
        }
        ("POST", p) if p.contains("/permissions/") => {
            let pending = st.locked().pending.take();
            if let Some((_, tx)) = pending {
                let _ = tx.send(body["response"].as_str().unwrap_or("reject").to_owned());
            }
            reply(&conn, "200 OK", "true");
        }
        ("POST", p) if p.ends_with("/abort") => reply(&conn, "200 OK", "true"),
        _ => reply(&conn, "404 Not Found", "{}"),
    }
}

fn oc_turn(st: &Arc<Mutex<Oc>>, text: &str) {
    let sid = st.locked().session.clone();
    event(
        st,
        json!({ "type": "session.status", "properties": { "sessionID": sid, "status": { "type": "busy" } } }),
    );
    let n = {
        let mut s = st.locked();
        s.n += 1;
        s.n
    };
    let part = |p: Value| json!({ "type": "message.part.updated", "properties": { "part": p } });
    let mut steps = plan(text, n * 100).into_iter();
    let mut k = 0;
    while let Some(step) = steps.next() {
        k += 1;
        match step {
            Step::Text(t) => event(
                st,
                part(
                    json!({ "id": format!("prt_{n}_{k}"), "sessionID": sid, "type": "text", "text": t, "time": { "end": 1 } }),
                ),
            ),
            Step::ToolStart { id, title } => event(
                st,
                part(
                    json!({ "id": format!("prt_{n}_{k}"), "sessionID": sid, "type": "tool", "callID": id, "tool": "bash", "state": { "status": "running", "input": { "command": title } } }),
                ),
            ),
            Step::ToolEnd { id, title, ok, output, .. } => event(
                st,
                part(
                    json!({ "id": format!("prt_{n}_{k}"), "sessionID": sid, "type": "tool", "callID": id, "tool": "bash", "state": { "status": if ok { "completed" } else { "error" }, "input": { "command": title }, "output": output } }),
                ),
            ),
            Step::Edit { id, path, .. } => event(
                st,
                part(
                    json!({ "id": format!("prt_{n}_{k}"), "sessionID": sid, "type": "tool", "callID": id, "tool": "edit", "state": { "status": "completed", "input": { "filePath": path }, "output": "edited" } }),
                ),
            ),
            Step::Ask { id, title } => {
                let pid = format!("per_{n}");
                let (tx, rx) = mpsc::channel();
                st.locked().pending = Some((pid.clone(), tx));
                event(
                    st,
                    json!({ "type": "permission.asked", "properties": { "id": pid, "sessionID": sid, "title": title } }),
                );
                let answer = rx.recv().unwrap_or_else(|_| "reject".into());
                event(
                    st,
                    json!({ "type": "permission.replied", "properties": { "id": pid, "sessionID": sid } }),
                );
                let allowed = answer != "reject";
                event(
                    st,
                    part(
                        json!({ "id": format!("prt_{n}_{k}t"), "sessionID": sid, "type": "tool", "callID": id, "tool": "bash", "state": { "status": if allowed { "completed" } else { "error" }, "input": { "command": title } } }),
                    ),
                );
                event(
                    st,
                    part(
                        json!({ "id": format!("prt_{n}_{k}a"), "sessionID": sid, "type": "text", "text": format!("answer: {}", if allowed { "yes" } else { "no" }), "time": { "end": 1 } }),
                    ),
                );
                let _ = steps.by_ref().count();
            }
            Step::Sleep(secs) => std::thread::sleep(Duration::from_secs_f64(secs)),
            Step::Fail(why) => event(
                st,
                json!({ "type": "session.error", "properties": { "sessionID": sid, "error": { "name": "Fake", "data": { "message": why } } } }),
            ),
        }
    }
    event(
        st,
        json!({ "type": "message.updated", "properties": { "info": { "id": format!("msg_{n}"), "sessionID": sid, "role": "assistant", "cost": 0.002, "tokens": { "input": 12, "output": 4 } } } }),
    );
    event(st, json!({ "type": "session.idle", "properties": { "sessionID": sid } }));
}

pub(crate) fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            s.push(if i <= chunk.len() {
                T[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    s
}
