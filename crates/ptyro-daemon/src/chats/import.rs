//! Import the agents' own histories as chats (CHAT5, SPEC §22.10, RESEARCH H6 / R-CHAT-12).
//!
//! Each agent keeps its sessions in its config folder, one per profile: Claude Code
//! `projects/<slug>/<session>.jsonl`, Codex `sessions/YYYY/MM/DD/rollout-*.jsonl` (and
//! `archived_sessions`), Gemini `tmp/<slug>/chats/session-*.jsonl` (or a legacy `.json`). Every
//! file is read, never written. A session becomes a read-only chat (`source` = `claude:<id>`):
//! its user and assistant text as nodes, tool calls as tool nodes (their output only with
//! `[chat] index_tools`), timestamps kept. Node ids are made from the source's own ids, so an
//! import is idempotent, and an append-only file is read on from where the last import
//! stopped (at a point where no tool call waits for its result). OpenCode is not imported:
//! its storage isn't installed here to read, and ptyro doesn't guess a format.
//!
//! The parsers are pure (a reader in, messages out) and stream line by line, skipping lines
//! too large to be text worth keeping (a pasted image, a huge tool result).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ptyro_core::chat::{ChatBody, NoticeLevel, ToolStatus};
use ptyro_core::chats::{self, ChatNode, ImportReport, ImportSource};
use ptyro_core::model::MAIN_WORKSPACE;
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, now_ms};
use ptyro_store::chat::ImportBatch;
use serde_json::Value;

use super::store_err;
use crate::daemon::Daemon;

/// The agents whose history ptyro reads, in the order an import goes.
pub(crate) const AGENTS: [&str; 3] = ["claude", "codex", "gemini"];
/// A line longer than this is skipped unread (base64 images, giant tool output).
const MAX_LINE: usize = 4 << 20;
/// Tool output kept with `index_tools` (as much as the index takes).
const MAX_OUTPUT: usize = 2048;
/// A file read whole (Gemini's) larger than this is skipped, with a note.
const MAX_WHOLE: u64 = 64 << 20;
/// How much of a line too long to read is searched for the tool call it answers.
const HEAD: usize = 64 << 10;
/// A tool call still unanswered this many lines on no longer holds back where the next import
/// starts (its result, if it comes, is past it).
const STALE_LINES: u64 = 1000;
/// How often `[chat] import` looks for new and grown sessions.
pub(crate) const EVERY: Duration = Duration::from_secs(600);

/// One message of a session, before it is a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Msg {
    /// Stable within the session (the source's own id), so a node's id is too.
    pub key: String,
    pub at_ms: u64,
    pub body: ChatBody,
}

/// A session as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub id: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub msgs: Vec<Msg>,
    /// Bytes read up to the last point where nothing waits for a tool's result: the next
    /// import of a grown file starts there (`0`: read it whole again).
    pub offset: u64,
    /// How many of `msgs` come before `offset`.
    pub safe: usize,
}

/// What a parser is told.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Opts {
    /// Keep tool output (capped), as `[chat] index_tools` asks.
    pub outputs: bool,
}

// ── shared helpers ──────────────────────────────────────────────────────────────────────────

/// An RFC 3339 time (`2026-10-25T05:05:24.025Z`, `…+02:00`) as Unix milliseconds.
pub(crate) fn iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        rest = &frac[digits.len()..];
        let three: String = digits.chars().chain("000".chars()).take(3).collect();
        ms = three.parse().ok()?;
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6).or_else(|| rest.get(3..5))?.parse().ok()?;
            let o = (hh * 60 + mm) * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset;
    u64::try_from(secs * 1000 + ms).ok()
}

/// A node id made from a session and one of its messages: the same every import (26 Crockford
/// base32 characters, as a ULID's).
pub(crate) fn node_id(source: &str, key: &str) -> String {
    use sha2::Digest;
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let h = sha2::Sha256::digest(format!("{source}\0{key}").as_bytes());
    let mut n = u128::from_be_bytes(h[..16].try_into().unwrap_or_default());
    let mut out = [0u8; 26];
    for c in out.iter_mut().rev() {
        *c = ALPHABET[(n & 31) as usize];
        n >>= 5;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Text that is a wrapper an agent put around something the person didn't type: `<command-name>`,
/// `<local-command-stdout>`, `<system-reminder>`, `<environment_context>`…
fn wrapper(text: &str) -> bool {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix('<') else { return false };
    let name: String =
        rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    !name.is_empty() && rest[name.len()..].starts_with('>')
}

/// One line of what a tool was asked: its command, file, pattern, query or URL.
fn tool_title(name: &str, input: &Value) -> String {
    let input = match input {
        // Codex passes its arguments as a JSON string.
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
        v => v.clone(),
    };
    let pick =
        ["command", "cmd", "file_path", "path", "pattern", "query", "url", "description", "prompt"]
            .iter()
            .find_map(|k| match &input[k] {
                Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                Value::Array(a) if !a.is_empty() => {
                    Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                }
                _ => None,
            });
    let raw = match (pick, &input) {
        (Some(p), _) => p,
        (None, Value::String(s)) => s.clone(),
        (None, Value::Null) => name.to_owned(),
        (None, v) => v.to_string(),
    };
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    // A command line can carry a credential (`curl -H "Authorization: Bearer …"`).
    chats::clip(&crate::summary::redact(line, &[]), 120)
}

/// A tool's output as text (a string, or blocks with `text`).
fn output_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str().or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => {
            o.get("output").or_else(|| o.get("content")).map(output_text).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// A line of a session file.
enum Line<'a> {
    Text(&'a str),
    /// Too long to read: its first bytes.
    Skipped(&'a [u8]),
}

/// Lines of a reader from `start`, each with where it ends; a line too long is passed over
/// (only its start is shown), and the last one when it has no newline yet (still being
/// written) is left for the next read.
fn lines(r: impl Read, start: u64, mut each: impl FnMut(Line<'_>, u64)) {
    let mut r = BufReader::with_capacity(1 << 16, r);
    let mut at = start;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = match (&mut r).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if buf.last() != Some(&b'\n') {
            if n > MAX_LINE {
                // Too long: skip to its end.
                let head = buf[..HEAD.min(buf.len())].to_vec();
                let mut skipped = n as u64;
                loop {
                    buf.clear();
                    match (&mut r).take(1 << 20).read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(k) => {
                            skipped += k as u64;
                            if buf.last() == Some(&b'\n') {
                                break;
                            }
                        }
                    }
                }
                at += skipped;
                each(Line::Skipped(&head), at);
                continue;
            }
            return; // a line still being written
        }
        at += n as u64;
        if let Ok(s) = std::str::from_utf8(&buf) {
            each(Line::Text(s.trim_end()), at);
        }
    }
}

/// Messages, and where tool calls wait for their results.
#[derive(Default)]
struct Acc {
    p: Parsed,
    /// Tool call id → its message's index, and the line it was on.
    open: HashMap<String, (usize, u64)>,
    /// Lines read.
    line: u64,
    title_rank: u8,
}

impl Acc {
    fn push(&mut self, key: String, at_ms: u64, body: ChatBody) {
        self.p.msgs.push(Msg { key, at_ms, body });
    }

    fn tool(&mut self, call: String, key: String, at_ms: u64, name: &str, title: String) {
        self.open.insert(call, (self.p.msgs.len(), self.line));
        let body = ChatBody::Tool {
            name: name.to_owned(),
            title,
            status: ToolStatus::Running,
            output: None,
            diff: None,
            files: Vec::new(),
        };
        self.push(key, at_ms, body);
    }

    fn result(&mut self, call: &str, failed: bool, output: String, o: Opts) {
        let Some((i, _)) = self.open.remove(call) else { return };
        if let Some(Msg { body: ChatBody::Tool { status, output: out, .. }, .. }) =
            self.p.msgs.get_mut(i)
        {
            *status = if failed { ToolStatus::Failed } else { ToolStatus::Done };
            if o.outputs && !output.trim().is_empty() {
                *out = Some(chats::clip(output.trim(), MAX_OUTPUT));
            }
        }
    }

    fn title(&mut self, rank: u8, t: &str) {
        let t = chats::clean_title(t);
        if !t.is_empty() && rank >= self.title_rank {
            self.title_rank = rank;
            self.p.title = Some(t);
        }
    }

    /// A line ended at `at`: a safe place to read on from, when no tool waits (one unanswered
    /// for `STALE_LINES` doesn't hold it back: an interrupted session never answers it).
    fn line_end(&mut self, at: u64) {
        self.line += 1;
        if self.open.values().all(|(_, l)| self.line - l > STALE_LINES) {
            self.p.offset = at;
            self.p.safe = self.p.msgs.len();
        }
    }

    /// A line too long to read ended at `at`: the tool calls it names (their results, too large
    /// to keep) are done.
    fn skipped(&mut self, head: &[u8], at: u64) {
        let named: Vec<String> = self
            .open
            .keys()
            .filter(|id| {
                let needle = format!("\"{id}\"");
                head.windows(needle.len()).any(|w| w == needle.as_bytes())
            })
            .cloned()
            .collect();
        for id in named {
            if let Some((i, _)) = self.open.remove(&id)
                && let Some(Msg { body: ChatBody::Tool { status, output, .. }, .. }) =
                    self.p.msgs.get_mut(i)
            {
                *status = ToolStatus::Done;
                *output = Some("(its output was too large to import)".into());
            }
        }
        self.line_end(at);
    }
}

// ── Claude Code ─────────────────────────────────────────────────────────────────────────────

/// A Claude Code session (`<config dir>/projects/<slug>/<id>.jsonl`), read from `start`.
/// Lines carry `type` (`user`, `assistant`, and titles: `custom-title`, `ai-title`, `summary`),
/// `uuid`, `timestamp`, `cwd`, `sessionId`; `message.content` is text or blocks (`text`,
/// `tool_use`, `tool_result`, `thinking`). Side chains, meta lines and compaction summaries are
/// left out, as are the wrappers around slash commands and their output.
pub(crate) fn parse_claude(r: impl Read, start: u64, o: Opts) -> Parsed {
    let mut a = Acc::default();
    a.p.offset = start;
    lines(r, start, |line, at| {
        let line = match line {
            Line::Text(t) => t,
            Line::Skipped(head) => return a.skipped(head, at),
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else { return a.line_end(at) };
        if a.p.id.is_empty()
            && let Some(id) = v["sessionId"].as_str()
        {
            a.p.id = id.to_owned();
        }
        if a.p.cwd.is_none()
            && let Some(c) = v["cwd"].as_str().filter(|c| !c.is_empty())
        {
            a.p.cwd = Some(c.to_owned());
        }
        let ts = v["timestamp"].as_str().and_then(iso_ms).unwrap_or(0);
        let uuid = v["uuid"].as_str().unwrap_or_default().to_owned();
        let skip = v["isSidechain"].as_bool() == Some(true)
            || v["isMeta"].as_bool() == Some(true)
            || v["isCompactSummary"].as_bool() == Some(true);
        match v["type"].as_str().unwrap_or_default() {
            "custom-title" => a.title(3, v["customTitle"].as_str().unwrap_or_default()),
            "ai-title" => a.title(2, v["aiTitle"].as_str().unwrap_or_default()),
            "summary" => a.title(2, v["summary"].as_str().unwrap_or_default()),
            "user" if !skip && !uuid.is_empty() => {
                let content = &v["message"]["content"];
                let mut texts: Vec<String> = Vec::new();
                match content {
                    Value::String(s) if !wrapper(s) => texts.push(s.clone()),
                    Value::Array(blocks) => {
                        for b in blocks {
                            match b["type"].as_str() {
                                Some("text") => {
                                    let t = b["text"].as_str().unwrap_or_default();
                                    if !wrapper(t) && !t.trim().is_empty() {
                                        texts.push(t.to_owned());
                                    }
                                }
                                Some("tool_result") => {
                                    let call = b["tool_use_id"].as_str().unwrap_or_default();
                                    let failed = b["is_error"].as_bool() == Some(true);
                                    a.result(call, failed, output_text(&b["content"]), o);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                let text = texts.join("\n");
                if !text.trim().is_empty() {
                    a.push(uuid, ts, ChatBody::User { text });
                }
            }
            "assistant" if !skip && !uuid.is_empty() => {
                let Value::Array(blocks) = &v["message"]["content"] else { return a.line_end(at) };
                for (i, b) in blocks.iter().enumerate() {
                    let key = format!("{uuid}/{i}");
                    match b["type"].as_str() {
                        Some("text") => {
                            let text = b["text"].as_str().unwrap_or_default().to_owned();
                            if text.trim().is_empty() {
                                continue;
                            }
                            let body = if v["isApiErrorMessage"].as_bool() == Some(true) {
                                ChatBody::Notice { level: NoticeLevel::Error, text }
                            } else {
                                ChatBody::Assistant { text, streaming: false }
                            };
                            a.push(key, ts, body);
                        }
                        Some("tool_use") => {
                            let name = b["name"].as_str().unwrap_or("tool");
                            let call = b["id"].as_str().unwrap_or(&key).to_owned();
                            let title = tool_title(name, &b["input"]);
                            a.tool(call, key, ts, name, title);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        a.line_end(at);
    });
    a.p
}

// ── Codex ───────────────────────────────────────────────────────────────────────────────────

/// A Codex rollout (`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`), read from
/// `start`: `session_meta` (its `id`, `cwd`), then `response_item`s: a `message` with role
/// `user` (what the person typed; Codex's own context, `<environment_context>` and the AGENTS.md
/// it reads, is left out) or `assistant`, tool calls (`function_call`, `custom_tool_call`,
/// `local_shell_call`, `web_search_call`) and their outputs by `call_id`. `event_msg` lines repeat
/// what these say and are passed over.
pub(crate) fn parse_codex(r: impl Read, start: u64, o: Opts) -> Parsed {
    let mut a = Acc::default();
    a.p.offset = start;
    lines(r, start, |line, at| {
        let line = match line {
            Line::Text(t) => t,
            Line::Skipped(head) => return a.skipped(head, at),
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else { return a.line_end(at) };
        let ts = v["timestamp"].as_str().and_then(iso_ms).unwrap_or(0);
        let p = &v["payload"];
        let key = p["id"].as_str().map_or_else(|| format!("L{at}"), str::to_owned);
        match (v["type"].as_str().unwrap_or_default(), p["type"].as_str().unwrap_or_default()) {
            ("session_meta", _) => {
                if let Some(id) = p["id"].as_str().or_else(|| p["session_id"].as_str()) {
                    a.p.id = id.to_owned();
                }
                if let Some(c) = p["cwd"].as_str().filter(|c| !c.is_empty()) {
                    a.p.cwd = Some(c.to_owned());
                }
            }
            ("response_item", "message") => {
                let role = p["role"].as_str().unwrap_or_default();
                let kinds = &p["internal_chat_message_metadata_passthrough"]["content_item_kinds"];
                let typed = match kinds.as_array() {
                    Some(k) => k.iter().any(|x| x.as_str() == Some("user.text")),
                    None => true,
                };
                let text: Vec<&str> = p["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["text"].as_str())
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                let text = text.join("\n");
                match role {
                    "user" if typed && !wrapper(&text) && !text.starts_with("# AGENTS.md") => {
                        if !text.trim().is_empty() {
                            a.push(key, ts, ChatBody::User { text });
                        }
                    }
                    "assistant" if !text.trim().is_empty() => {
                        a.push(key, ts, ChatBody::Assistant { text, streaming: false });
                    }
                    _ => {}
                }
            }
            (
                "response_item",
                kind @ ("function_call" | "custom_tool_call" | "local_shell_call"),
            ) => {
                let name = p["name"].as_str().unwrap_or(if kind == "local_shell_call" {
                    "shell"
                } else {
                    "tool"
                });
                let input = if p["arguments"].is_null() {
                    if p["input"].is_null() { &p["action"] } else { &p["input"] }
                } else {
                    &p["arguments"]
                };
                let call = p["call_id"].as_str().unwrap_or(&key).to_owned();
                a.tool(call, key, ts, name, tool_title(name, input));
            }
            ("response_item", "function_call_output" | "custom_tool_call_output") => {
                let call = p["call_id"].as_str().unwrap_or_default();
                let out = output_text(&p["output"]);
                let failed = p["output"]["success"].as_bool() == Some(false);
                a.result(call, failed, out, o);
            }
            ("response_item", "web_search_call") => {
                let title = tool_title("web_search", &p["action"]);
                let body = ChatBody::Tool {
                    name: "web_search".into(),
                    title,
                    status: ToolStatus::Done,
                    output: None,
                    diff: None,
                    files: Vec::new(),
                };
                a.push(key, ts, body);
            }
            _ => {}
        }
        a.line_end(at);
    });
    a.p
}

/// Codex's titles: `session_index.jsonl` lines `{id, thread_name}`.
pub(crate) fn codex_titles(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v["id"].as_str()?.to_owned(), v["thread_name"].as_str()?.to_owned())))
        .collect()
}

// ── Gemini ──────────────────────────────────────────────────────────────────────────────────

/// A Gemini CLI session (`<gemini dir>/tmp/<slug>/chats/session-*.jsonl`, read from 0.66.0's
/// source [S]): a metadata record (`sessionId`, `projectHash`, `startTime`, `summary`), message
/// records (`id`, `timestamp`, `type` `user`/`gemini`/`info`/`error`/`warning`, `content` as
/// text or parts, `toolCalls`), a later record with the same id replacing it, `$rewindTo`
/// (drops that message and what follows), `$patch` (new content, tool results, `removeIds`),
/// `$set` (metadata, or all the messages). A legacy `.json` file is one object with
/// `messages`. Read whole every time (patches change earlier messages).
pub(crate) fn parse_gemini(text: &str, o: Opts) -> Parsed {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut meta = serde_json::Map::new();
    let put = |order: &mut Vec<String>, by_id: &mut HashMap<String, Value>, m: Value| {
        if let Some(id) = m["id"].as_str().map(str::to_owned) {
            if !by_id.contains_key(&id) {
                order.push(id.clone());
            }
            by_id.insert(id, m);
        }
    };
    let messages = |v: &mut Value| match v.get_mut("messages").map(Value::take) {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let whole =
        serde_json::from_str::<Value>(text.trim()).ok().filter(|v| v["messages"].is_array());
    if let Some(mut v) = whole {
        for m in messages(&mut v) {
            put(&mut order, &mut by_id, m);
        }
        if let Value::Object(o) = v {
            meta = o;
        }
    } else {
        for line in text.lines().filter(|l| !l.trim().is_empty() && l.len() <= MAX_LINE) {
            let Ok(mut r) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(to) = r["$rewindTo"].as_str() {
                if let Some(i) = order.iter().position(|x| x == to) {
                    for gone in order.drain(i..) {
                        by_id.remove(&gone);
                    }
                } else {
                    order.clear();
                    by_id.clear();
                }
            } else if r["$patch"].is_object() {
                let p = &r["$patch"];
                let mut each: Vec<&Value> = vec![p];
                each.extend(p["updates"].as_array().into_iter().flatten());
                for u in each {
                    let Some(m) = u["id"].as_str().and_then(|id| by_id.get_mut(id)) else {
                        continue;
                    };
                    if !u["content"].is_null() {
                        m["content"] = u["content"].clone();
                    }
                    for tc in u["toolCalls"].as_array().into_iter().flatten() {
                        let calls = m["toolCalls"].as_array_mut();
                        if let Some(t) = calls
                            .into_iter()
                            .flatten()
                            .find(|t| t["id"] == tc["id"] && !tc["result"].is_null())
                        {
                            t["result"] = tc["result"].clone();
                        }
                    }
                }
                for id in p["removeIds"].as_array().into_iter().flatten().filter_map(Value::as_str)
                {
                    order.retain(|x| x != id);
                    by_id.remove(id);
                }
            } else if let Some(set) = r.get_mut("$set").filter(|v| v.is_object()) {
                if set["messages"].is_array() {
                    order.clear();
                    by_id.clear();
                    for m in messages(set) {
                        put(&mut order, &mut by_id, m);
                    }
                }
                if let Value::Object(o) = set.take() {
                    meta.extend(o.into_iter().filter(|(k, _)| k != "messages"));
                }
            } else if r["id"].is_string() {
                put(&mut order, &mut by_id, r);
            } else if r["sessionId"].is_string() {
                for m in messages(&mut r) {
                    put(&mut order, &mut by_id, m);
                }
                if let Value::Object(o) = r {
                    meta.extend(o.into_iter().filter(|(k, _)| k != "messages"));
                }
            }
        }
    }
    let mut a = Acc::default();
    a.p.id = meta.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
    if let Some(s) = meta.get("summary").and_then(Value::as_str) {
        a.title(2, s);
    }
    for id in &order {
        let Some(m) = by_id.get(id) else { continue };
        let ts = m["timestamp"].as_str().and_then(iso_ms).unwrap_or(0);
        let text = gemini_text(&m["content"]);
        match m["type"].as_str().unwrap_or_default() {
            "user" if !text.trim().is_empty() => {
                a.push(id.clone(), ts, ChatBody::User { text });
            }
            "gemini" => {
                for (i, tc) in m["toolCalls"].as_array().into_iter().flatten().enumerate() {
                    let name = tc["displayName"]
                        .as_str()
                        .or_else(|| tc["name"].as_str())
                        .unwrap_or("tool");
                    let call =
                        tc["id"].as_str().map_or_else(|| format!("{id}/t{i}"), str::to_owned);
                    let status = match tc["status"].as_str() {
                        Some("error") => ToolStatus::Failed,
                        Some("cancelled") => ToolStatus::Declined,
                        _ => ToolStatus::Done,
                    };
                    let out = if o.outputs {
                        let s = output_text(&tc["resultDisplay"]);
                        let s = if s.is_empty() { gemini_result(&tc["result"]) } else { s };
                        Some(chats::clip(s.trim(), MAX_OUTPUT)).filter(|s| !s.is_empty())
                    } else {
                        None
                    };
                    a.push(
                        format!("{id}/{call}"),
                        tc["timestamp"].as_str().and_then(iso_ms).unwrap_or(ts),
                        ChatBody::Tool {
                            name: name.to_owned(),
                            title: tool_title(name, &tc["args"]),
                            status,
                            output: out,
                            diff: None,
                            files: Vec::new(),
                        },
                    );
                }
                if !text.trim().is_empty() {
                    a.push(id.clone(), ts, ChatBody::Assistant { text, streaming: false });
                }
            }
            "error" if !text.trim().is_empty() => {
                a.push(id.clone(), ts, ChatBody::Notice { level: NoticeLevel::Error, text });
            }
            _ => {}
        }
    }
    a.p
}

/// A Gemini message's content: text, or parts with `text`.
fn gemini_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")
        }
        Value::Object(_) => v["text"].as_str().unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

/// A tool's result as Gemini records it (`functionResponse.response.output`, in parts).
fn gemini_result(v: &Value) -> String {
    let parts: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        v => vec![v],
    };
    parts
        .iter()
        .filter_map(|p| {
            let r = &p["functionResponse"]["response"];
            r["output"].as_str().or_else(|| r["error"].as_str()).map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── finding the sessions ────────────────────────────────────────────────────────────────────

/// One session file of an agent's history.
#[derive(Debug, Clone)]
struct Found {
    path: PathBuf,
    size: u64,
    mtime: u64,
    /// The folder it ran in, when the layout says (Gemini's `.project_root`).
    cwd: Option<String>,
}

fn found(path: PathBuf, cwd: Option<String>) -> Option<Found> {
    let m = std::fs::metadata(&path).ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(Found { path, size: m.len(), mtime, cwd })
}

fn files_in(dir: &Path, ext: &[&str]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| ext.iter().any(|e| x == *e)))
        .collect();
    v.sort();
    v
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth > 0 {
            walk(&p, depth - 1, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

/// An agent's session files under its folder `dir`.
fn sessions(agent: &str, dir: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    match agent {
        "claude" => {
            let Ok(rd) = std::fs::read_dir(dir.join("projects")) else { return out };
            for e in rd.flatten().filter(|e| e.path().is_dir()) {
                out.extend(
                    files_in(&e.path(), &["jsonl"]).into_iter().filter_map(|p| found(p, None)),
                );
            }
        }
        "codex" => {
            let mut v = Vec::new();
            walk(&dir.join("sessions"), 4, &mut v);
            walk(&dir.join("archived_sessions"), 4, &mut v);
            v.sort();
            out.extend(v.into_iter().filter_map(|p| found(p, None)));
        }
        "gemini" => {
            let Ok(rd) = std::fs::read_dir(dir.join("tmp")) else { return out };
            for e in rd.flatten().filter(|e| e.path().is_dir()) {
                let root = std::fs::read_to_string(e.path().join(".project_root"))
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty());
                for p in files_in(&e.path().join("chats"), &["jsonl", "json"]) {
                    out.extend(found(p, root.clone()));
                }
            }
        }
        _ => {}
    }
    out
}

/// Read one session file (from `start`, for the append-only formats).
fn read_session(agent: &str, path: &Path, start: u64, o: Opts) -> Option<Parsed> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut p = match agent {
        "claude" | "codex" => {
            f.seek(std::io::SeekFrom::Start(start)).ok()?;
            if agent == "claude" { parse_claude(f, start, o) } else { parse_codex(f, start, o) }
        }
        "gemini" => {
            let mut text = String::new();
            (&mut f).take(MAX_WHOLE).read_to_string(&mut text).ok()?;
            let mut p = parse_gemini(&text, o);
            p.offset = 0;
            p.safe = p.msgs.len();
            p
        }
        _ => return None,
    };
    if p.id.is_empty() {
        // Claude's and Codex's file names carry the session id.
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        let tail = stem.len().checked_sub(36).and_then(|i| stem.get(i..));
        p.id = match (agent, tail) {
            ("codex", Some(id)) => id.to_owned(),
            _ => stem,
        };
    }
    Some(p)
}

// ── the import ──────────────────────────────────────────────────────────────────────────────

impl Daemon {
    /// The folder `agent` keeps its config in under `profile` (where its history is), as the
    /// profile's plan points it, without resolving the profile's secrets.
    fn history_dir(&self, agent: &str, profile: &str) -> Option<PathBuf> {
        let config = self.config();
        let managed = self.paths.data.join("profiles");
        let dir = config
            .profiles
            .get(profile)
            .and_then(|d| d.account(agent))
            .and_then(|a| a.dir())
            .map(ptyro_core::config::expand_home)
            .or_else(|| {
                (profile != ptyro_core::model::DEFAULT_PROFILE)
                    .then(|| managed.join(profile).join(agent))
            });
        match (agent, dir) {
            // `GEMINI_CLI_HOME` is the home that holds `.gemini`.
            ("gemini", Some(h)) => Some(h.join(".gemini")),
            (_, Some(d)) => Some(d),
            (_, None) => ptyro_agents::default_dir(agent),
        }
    }

    /// The workspace a session belongs to: the one with the project (or worktree) its folder
    /// is in, else `main`.
    fn import_workspace(&self, cwd: Option<&str>) -> String {
        let reg = self.registry.locked();
        let main = || {
            reg.workspaces
                .iter()
                .find(|w| w.name == MAIN_WORKSPACE)
                .map(|w| w.id.clone())
                .unwrap_or_else(|| MAIN_WORKSPACE.to_owned())
        };
        let Some(cwd) = cwd.map(Path::new) else { return main() };
        let roots = reg
            .projects
            .iter()
            .filter(|p| p.machine.is_none())
            .map(|p| (p.root.clone(), p.id.clone()))
            .chain(
                reg.worktrees
                    .iter()
                    .filter(|w| w.machine.is_none())
                    .map(|w| (w.path.clone(), w.project.clone())),
            );
        let best = roots
            .filter(|(r, _)| cwd.starts_with(r))
            .max_by_key(|(r, _)| r.as_os_str().len())
            .map(|(_, p)| p);
        best.and_then(|p| {
            reg.workspaces.iter().find(|w| w.projects.contains(&p)).map(|w| w.id.clone())
        })
        .unwrap_or_else(main)
    }

    /// Import every agent's history (or `agent`'s), under every profile (or `profile`), of
    /// sessions touched within `since` (SPEC §22.10). `dry_run`: count, write nothing.
    pub(crate) async fn chat_import(
        self: &Arc<Self>,
        agent: Option<String>,
        profile: Option<String>,
        since: Option<String>,
        dry_run: bool,
    ) -> Result<ImportReport, Error> {
        let db = self.chat_db()?;
        let config = self.config();
        let agents: Vec<String> = match agent.as_deref().map(str::trim) {
            None | Some("" | "all") => {
                AGENTS.iter().chain(["opencode"].iter()).map(|s| (*s).to_owned()).collect()
            }
            Some(a) if AGENTS.contains(&a) || a == "opencode" => vec![a.to_owned()],
            Some(a) => {
                return Err(Error::invalid(format!(
                    "{a:?}: ptyro imports claude, codex and gemini histories (or all)"
                )));
            }
        };
        let profiles = match profile {
            Some(p) if ptyro_agents::profile::exists(&config, &p) => vec![p],
            Some(p) => {
                return Err(Error::invalid(format!(
                    "no profile named {p:?} ({})",
                    ptyro_agents::profile::names(&config).join(", ")
                )));
            }
            None => ptyro_agents::profile::names(&config),
        };
        let since_ms = match since.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(s) => {
                let d = ptyro_core::config::parse_duration(s)
                    .map_err(|e| Error::invalid(format!("--since {s}: {e}")))?;
                now_ms().saturating_sub(d.as_millis() as u64)
            }
            None => 0,
        };
        // One import at a time (the periodic one and yours).
        let mut passed = self.chats.importing.lock().await;
        let mut report = ImportReport { dry_run, ..ImportReport::default() };
        if agents.iter().any(|a| a == "opencode") {
            report.notes.push(
                "opencode: not imported: its storage format isn't verified (it isn't installed here to read), and ptyro doesn't guess one".into(),
            );
        }
        let states = db.import_states().await.map_err(store_err)?;
        let by_path: HashMap<&str, (&String, &ptyro_store::chat::ImportState)> =
            states.iter().map(|(k, v)| (v.path.as_str(), (k, v))).collect();
        // ptyro's own chat engines and title/summary one-shots run in these: not history (its
        // worktrees, under the data folder too, are).
        let own: Vec<PathBuf> = ["chat", "summaries"]
            .iter()
            .flat_map(|d| {
                let p = self.paths.data.join(d);
                let real = p.canonicalize().ok();
                [Some(p), real]
            })
            .flatten()
            .collect();
        let opts = Opts { outputs: config.chat.index_tools };
        let mut seen_dirs: Vec<PathBuf> = Vec::new();
        for agent in agents.iter().filter(|a| AGENTS.contains(&a.as_str())) {
            for profile in &profiles {
                let Some(dir) = self.history_dir(agent, profile) else { continue };
                // Two profiles pointing at one folder: read once, under the first.
                if seen_dirs.contains(&dir) || !dir.is_dir() {
                    continue;
                }
                seen_dirs.push(dir.clone());
                let mut src = ImportSource {
                    agent: agent.clone(),
                    profile: profile.clone(),
                    dir: dir.to_string_lossy().into_owned(),
                    ..ImportSource::default()
                };
                let (a, d) = (agent.clone(), dir.clone());
                let listed = tokio::task::spawn_blocking(move || {
                    let titles = match a.as_str() {
                        "codex" => std::fs::read_to_string(d.join("session_index.jsonl"))
                            .map(|t| codex_titles(&t))
                            .unwrap_or_default(),
                        _ => HashMap::new(),
                    };
                    (sessions(&a, &d), titles)
                })
                .await;
                // One agent's folder that can't be read doesn't stop the others'.
                let (files, titles) = match listed {
                    Ok(l) => l,
                    Err(e) => {
                        report.notes.push(format!("{agent} ({profile}): not read: {e}"));
                        report.sources.push(src);
                        continue;
                    }
                };
                for f in files {
                    if f.mtime < since_ms {
                        src.skipped += 1;
                        continue;
                    }
                    let path_s = f.path.to_string_lossy().into_owned();
                    // Passed over before and unchanged since: not read again.
                    if passed.get(&path_s) == Some(&(f.size, f.mtime)) {
                        src.skipped += 1;
                        continue;
                    }
                    if agent == "gemini" && f.size > MAX_WHOLE {
                        report.notes.push(format!(
                            "{}: skipped: {} MB is more than a session ptyro reads whole",
                            f.path.display(),
                            f.size >> 20
                        ));
                        passed.insert(path_s, (f.size, f.mtime));
                        src.skipped += 1;
                        continue;
                    }
                    let prior = by_path.get(path_s.as_str()).map(|(_, s)| (*s).clone());
                    if let Some(s) = &prior {
                        if s.deleted {
                            src.skipped += 1;
                            continue;
                        }
                        if s.size == f.size && s.mtime == f.mtime {
                            src.unchanged += 1;
                            continue;
                        }
                    }
                    // An append-only file that grew is read on from where it was left.
                    let start = match &prior {
                        Some(s) if f.size >= s.size && agent != "gemini" => s.offset,
                        _ => 0,
                    };
                    let (a, p) = (agent.clone(), f.path.clone());
                    let read =
                        tokio::task::spawn_blocking(move || read_session(&a, &p, start, opts))
                            .await;
                    // A file that can't be read (or breaks the parser) is passed over, never
                    // the whole import.
                    let parsed = match read {
                        Ok(Some(p)) => p,
                        Ok(None) => {
                            passed.insert(path_s, (f.size, f.mtime));
                            src.skipped += 1;
                            continue;
                        }
                        Err(e) => {
                            report.notes.push(format!("{}: skipped: {e}", f.path.display()));
                            passed.insert(path_s, (f.size, f.mtime));
                            src.skipped += 1;
                            continue;
                        }
                    };
                    let cwd = parsed.cwd.clone().or(f.cwd.clone());
                    if cwd
                        .as_deref()
                        .is_some_and(|c| own.iter().any(|o| Path::new(c).starts_with(o)))
                    {
                        passed.insert(path_s, (f.size, f.mtime));
                        src.skipped += 1;
                        continue;
                    }
                    let source = format!("{agent}:{}", parsed.id);
                    let known = states.get(&source).cloned().or(prior.clone());
                    if known.as_ref().is_some_and(|k| k.deleted) {
                        src.skipped += 1;
                        continue;
                    }
                    let start_tail =
                        if start > 0 { known.as_ref().and_then(|k| k.tail.clone()) } else { None };
                    let mut parent = start_tail.clone();
                    let mut tail = start_tail;
                    let nodes: Vec<ChatNode> = parsed
                        .msgs
                        .iter()
                        .enumerate()
                        .map(|(i, m)| {
                            let id = node_id(&source, &m.key);
                            let n = ChatNode {
                                id: id.clone(),
                                parent: parent.clone(),
                                at_ms: m.at_ms,
                                body: m.body.clone(),
                            };
                            parent = Some(id.clone());
                            if i < parsed.safe {
                                tail = Some(id);
                            }
                            n
                        })
                        .collect();
                    if nodes.is_empty() && known.is_none() {
                        // Nothing said in it (a session opened and left): read again once it
                        // changes.
                        passed.insert(path_s, (f.size, f.mtime));
                        src.skipped += 1;
                        continue;
                    }
                    let grown = known.is_some();
                    if dry_run {
                        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
                        let total = ids.len();
                        let old = db.known_nodes(ids).await.map_err(store_err)?;
                        if total > old || !grown {
                            if grown {
                                src.grown += 1
                            } else {
                                src.new += 1
                            }
                            src.messages += (total - old) as u64;
                        } else {
                            src.unchanged += 1;
                        }
                        continue;
                    }
                    let title = parsed
                        .title
                        .clone()
                        .or_else(|| titles.get(&parsed.id).map(|t| chats::clean_title(t)))
                        .filter(|t| !t.is_empty())
                        .or_else(|| {
                            (!grown).then(|| {
                                parsed.msgs.iter().find_map(|m| match &m.body {
                                    ChatBody::User { text } => Some(chats::first_line_title(text)),
                                    _ => None,
                                })
                            })?
                        });
                    let leaf = nodes.last().map(|n| n.id.clone());
                    let batch = ImportBatch {
                        source: source.clone(),
                        chat: chats::new_id(),
                        workspace: self.import_workspace(cwd.as_deref()),
                        title,
                        agent: agent.clone(),
                        profile: profile.clone(),
                        tools: config.chat.preset.clone(),
                        source_dir: cwd,
                        path: path_s,
                        size: f.size,
                        mtime: f.mtime,
                        offset: parsed.offset,
                        nodes,
                        leaf,
                        tail,
                        index_tools: opts.outputs,
                    };
                    let r = db.import_batch(batch).await.map_err(store_err)?;
                    match (&r.chat, r.created, r.added) {
                        (None, _, _) => src.skipped += 1,
                        (Some(_), true, n) => {
                            src.new += 1;
                            src.messages += n as u64;
                        }
                        (Some(_), false, 0) => src.unchanged += 1,
                        (Some(_), false, n) => {
                            src.grown += 1;
                            src.messages += n as u64;
                        }
                    }
                    if let Some(c) = r.chat.filter(|_| r.created || r.added > 0) {
                        // (Its `ChatUpdated` also wakes the meaning backfill.)
                        self.chat_changed(&c).await;
                    }
                }
                tracing::info!(
                    agent = %src.agent,
                    profile = %src.profile,
                    "chat import: {} new, {} grown, {} unchanged, {} skipped ({} messages)",
                    src.new,
                    src.grown,
                    src.unchanged,
                    src.skipped,
                    src.messages
                );
                report.sources.push(src);
            }
        }
        Ok(report)
    }

    /// A message to an imported chat: it carries on as a fork, which says so (the import stays
    /// as the agent wrote it, so a re-import can still add to it). The next message carries on
    /// in the same fork (its newest one), not in another.
    pub(super) async fn continue_imported(
        self: &Arc<Self>,
        info: &chats::ChatInfo,
    ) -> Result<chats::ChatInfo, Error> {
        // One at a time: two messages at once make one fork.
        let lock = self.chats.lock_for(&info.id);
        let _import = lock.lock().await;
        if let Some(id) =
            self.chat_db()?.newest_fork(info.id.clone()).await.map_err(super::store_err)?
        {
            return self.chat_info_now(&id).await;
        }
        let fork = self.chat_fork(info, None, None, false).await?;
        let agent = info.source.as_deref().and_then(|s| s.split(':').next()).unwrap_or("its agent");
        let text = format!(
            "continued as a fork of the imported {} ({}): that one stays as {agent} wrote it",
            info.addr(),
            info.label()
        );
        let lock = self.chats.lock_for(&fork.id);
        let _one = lock.lock().await;
        let live = self.chats.live(&fork.id);
        self.reload_branch(&live).await;
        self.chat_notice(&live, &fork.id, NoticeLevel::Info, &text).await;
        Ok(fork)
    }

    /// `[chat] import`: every 10 minutes (and at once when it is switched on), import what is
    /// new and grown.
    pub(crate) fn start_chat_import(self: &Arc<Self>) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut last: Option<std::time::Instant> = None;
            let mut was_on = false;
            loop {
                let on = me.config().chat.import && me.config().chat.enabled;
                let due = on && (!was_on || last.is_none_or(|l| l.elapsed() >= EVERY));
                was_on = on;
                if due {
                    last = Some(std::time::Instant::now());
                    match me.chat_import(None, None, None, false).await {
                        Ok(r) => tracing::info!("chat import: {}", r.summary()),
                        Err(e) => tracing::warn!("chat import: {}", e.message),
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bodies(p: &Parsed) -> Vec<String> {
        p.msgs
            .iter()
            .map(|m| match &m.body {
                ChatBody::User { text } => format!("user: {text}"),
                ChatBody::Assistant { text, .. } => format!("assistant: {text}"),
                ChatBody::Tool { name, title, status, output, .. } => {
                    format!("tool {name} {title} {status:?} {}", output.as_deref().unwrap_or("-"))
                }
                ChatBody::Notice { text, .. } => format!("notice: {text}"),
                b => format!("{b:?}"),
            })
            .collect()
    }

    #[test]
    fn times() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2026-10-25T05:05:24.025Z"), Some(1_792_904_724_025));
        assert_eq!(iso_ms("2026-10-25T07:05:24.025+02:00"), Some(1_792_904_724_025));
        assert_eq!(iso_ms("2026-10-25T05:05:24.0251234Z"), Some(1_792_904_724_025));
        assert_eq!(iso_ms("yesterday"), None);
    }

    #[test]
    fn ids_are_stable_and_shaped_like_ulids() {
        let a = node_id("claude:s1", "u1/0");
        assert_eq!(a, node_id("claude:s1", "u1/0"));
        assert_ne!(a, node_id("claude:s2", "u1/0"));
        assert_eq!(a.len(), 26);
        assert!(a.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()));
    }

    const CLAUDE: &str = r#"{"type":"user","uuid":"u1","sessionId":"s1","cwd":"/home/me/repo","timestamp":"2026-10-25T05:00:00Z","isSidechain":false,"message":{"role":"user","content":"how do I list files?"}}
{"type":"user","uuid":"m1","sessionId":"s1","cwd":"/home/me/repo","timestamp":"2026-10-25T05:00:00Z","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat</local-command-caveat>"}}
{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-10-25T05:00:01Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"}]}}
{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-10-25T05:00:02Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la\nmore"}}]}}
{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"2026-10-25T05:00:03Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"a.txt\nb.txt","is_error":false}]}}
{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-10-25T05:00:04Z","message":{"role":"assistant","content":[{"type":"text","text":"Use `ls`."}]}}
{"type":"user","uuid":"u3","sessionId":"s1","timestamp":"2026-10-25T05:00:05Z","isSidechain":true,"message":{"role":"user","content":"side"}}
{"type":"user","uuid":"u4","sessionId":"s1","timestamp":"2026-10-25T05:00:06Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}
{"type":"ai-title","aiTitle":"Listing files","sessionId":"s1"}
{"type":"custom-title","customTitle":"My ls question","sessionId":"s1"}
"#;

    #[test]
    fn claude_sessions() {
        let p = parse_claude(CLAUDE.as_bytes(), 0, Opts { outputs: true });
        assert_eq!(p.id, "s1");
        assert_eq!(p.cwd.as_deref(), Some("/home/me/repo"));
        assert_eq!(p.title.as_deref(), Some("My ls question"), "a title you gave wins");
        assert_eq!(
            bodies(&p),
            [
                "user: how do I list files?",
                "tool Bash ls -la Done a.txt\nb.txt",
                "assistant: Use `ls`."
            ]
        );
        assert_eq!(p.msgs[0].at_ms, iso_ms("2026-10-25T05:00:00Z").unwrap());
        assert_eq!(p.offset, CLAUDE.len() as u64);
        // Without index_tools a tool keeps no output.
        let p = parse_claude(CLAUDE.as_bytes(), 0, Opts::default());
        assert_eq!(bodies(&p)[1], "tool Bash ls -la Done -");
        // Read on from the middle: only what follows, and no tool waits there.
        let cut = CLAUDE.find("{\"type\":\"assistant\",\"uuid\":\"a3\"").unwrap();
        let rest = parse_claude(&CLAUDE.as_bytes()[cut..], cut as u64, Opts::default());
        assert_eq!(bodies(&rest), ["assistant: Use `ls`."]);
        // A tool still waiting for its result: the safe point is before it.
        let upto = CLAUDE.find("{\"type\":\"user\",\"uuid\":\"u2\"").unwrap();
        let half = parse_claude(&CLAUDE.as_bytes()[..upto], 0, Opts::default());
        assert_eq!((half.msgs.len(), half.safe), (2, 1));
        assert!(half.offset < upto as u64);
        // A last line still being written is left for next time.
        let p = parse_claude(&CLAUDE.as_bytes()[..CLAUDE.len() - 5], 0, Opts::default());
        assert!(p.offset < CLAUDE.len() as u64 - 5);
    }

    const CODEX: &str = r#"{"timestamp":"2026-10-25T05:05:24.025Z","type":"session_meta","payload":{"id":"01a17172-da66-7120-83a8-564b2ed106e4","cwd":"/home/me/proj","originator":"codex"}}
{"timestamp":"2026-10-25T05:05:24.358Z","type":"response_item","payload":{"type":"message","id":"msg_1","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/home/me</cwd>\n</environment_context>"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["environments.environment_context"]}}}
{"timestamp":"2026-10-25T05:05:24.400Z","type":"response_item","payload":{"type":"message","id":"msg_d","role":"developer","content":[{"type":"input_text","text":"rules"}]}}
{"timestamp":"2026-10-25T05:05:25.000Z","type":"response_item","payload":{"type":"message","id":"msg_2","role":"user","content":[{"type":"input_text","text":"what does TTY mean?"}],"internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}}}
{"timestamp":"2026-10-25T05:05:26.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":[\"man\",\"tty\"]}","call_id":"call_1"}}
{"timestamp":"2026-10-25T05:05:27.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"TTY(1)"}}
{"timestamp":"2026-10-25T05:05:28.000Z","type":"event_msg","payload":{"type":"agent_message","message":"A teletype."}}
{"timestamp":"2026-10-25T05:05:28.000Z","type":"response_item","payload":{"type":"message","id":"msg_3","role":"assistant","content":[{"type":"output_text","text":"A teletype."}]}}
{"timestamp":"2026-10-25T05:05:29.000Z","type":"response_item","payload":{"type":"web_search_call","action":{"type":"search","query":"tty history"}}}
"#;

    #[test]
    fn codex_rollouts() {
        let p = parse_codex(CODEX.as_bytes(), 0, Opts::default());
        assert_eq!(p.id, "01a17172-da66-7120-83a8-564b2ed106e4");
        assert_eq!(p.cwd.as_deref(), Some("/home/me/proj"));
        assert_eq!(
            bodies(&p),
            [
                "user: what does TTY mean?",
                "tool exec_command man tty Done -",
                "assistant: A teletype.",
                "tool web_search tty history Done -",
            ]
        );
        // A line with no id of its own gets one from where it is: the same every time.
        assert_eq!(p.msgs[3].key, parse_codex(CODEX.as_bytes(), 0, Opts::default()).msgs[3].key);
        let t = codex_titles(
            "{\"id\":\"01a17172-da66-7120-83a8-564b2ed106e4\",\"thread_name\":\"TTY meaning\",\"updated_at\":\"x\"}\n",
        );
        assert_eq!(t["01a17172-da66-7120-83a8-564b2ed106e4"], "TTY meaning");
    }

    const GEMINI: &str = r#"{"sessionId":"g1","projectHash":"h","startTime":"2026-10-25T05:00:00Z","lastUpdated":"2026-10-25T05:00:00Z","kind":"main"}
{"id":"m1","timestamp":"2026-10-25T05:00:01Z","type":"user","content":[{"text":"hello gemini"}]}
{"id":"m2","timestamp":"2026-10-25T05:00:02Z","type":"gemini","content":"","toolCalls":[{"id":"t1","name":"google_web_search","displayName":"GoogleSearch","args":{"query":"weather"},"status":"success","result":[{"functionResponse":{"response":{"output":"sunny"}}}]}]}
{"id":"m3","timestamp":"2026-10-25T05:00:03Z","type":"gemini","content":"It is sunny."}
{"id":"m4","timestamp":"2026-10-25T05:00:04Z","type":"user","content":"a mistake"}
{"$rewindTo":"m4"}
{"id":"m5","timestamp":"2026-10-25T05:00:05Z","type":"user","content":"and tomorrow?"}
{"id":"m6","timestamp":"2026-10-25T05:00:06Z","type":"gemini","content":"draft"}
{"$patch":{"id":"m6","content":"Rain."}}
{"id":"m7","timestamp":"2026-10-25T05:00:07Z","type":"info","content":"switched model"}
{"$set":{"summary":"Weather questions"}}
"#;

    #[test]
    fn gemini_sessions() {
        let p = parse_gemini(GEMINI, Opts { outputs: true });
        assert_eq!(p.id, "g1");
        assert_eq!(p.title.as_deref(), Some("Weather questions"));
        assert_eq!(
            bodies(&p),
            [
                "user: hello gemini",
                "tool GoogleSearch weather Done sunny",
                "assistant: It is sunny.",
                "user: and tomorrow?",
                "assistant: Rain.",
            ]
        );
        // The legacy single-object file.
        let legacy = r#"{"sessionId":"g2","projectHash":"h","messages":[{"id":"a","timestamp":"2026-10-25T05:00:01Z","type":"user","content":"hi"},{"id":"b","timestamp":"2026-10-25T05:00:02Z","type":"gemini","content":"hello"}]}"#;
        let p = parse_gemini(legacy, Opts::default());
        assert_eq!((p.id.as_str(), bodies(&p).len()), ("g2", 2));
    }

    #[test]
    fn wrappers_are_not_what_you_typed() {
        assert!(wrapper("<command-name>/clear</command-name>"));
        assert!(wrapper("  <environment_context>\n"));
        assert!(!wrapper("<3 thanks"));
        assert!(!wrapper("why does <div> collapse?"));
    }

    #[test]
    fn long_lines_are_passed_over() {
        let big = format!(
            "{{\"type\":\"user\",\"uuid\":\"x\",\"message\":{{\"content\":\"{}\"}}}}\n",
            "a".repeat(MAX_LINE + 10)
        );
        let text = format!("{big}{}", CLAUDE);
        let p = parse_claude(text.as_bytes(), 0, Opts::default());
        assert_eq!(p.msgs.len(), 3);
        assert_eq!(p.offset, text.len() as u64);
    }

    #[test]
    fn a_result_too_large_closes_its_call_and_the_offset_moves_on() {
        let call = r#"{"type":"assistant","uuid":"a1","sessionId":"s","message":{"content":[{"type":"tool_use","id":"toolu_9","name":"Bash","input":{"command":"cat big.log"}}]}}"#;
        let result = format!(
            r#"{{"type":"user","uuid":"u2","message":{{"content":[{{"type":"tool_result","tool_use_id":"toolu_9","content":"{}"}}]}}}}"#,
            "x".repeat(MAX_LINE + 10)
        );
        let after = r#"{"type":"user","uuid":"u3","message":{"content":"thanks"}}"#;
        let text = format!("{call}\n{result}\n{after}\n");
        let p = parse_claude(text.as_bytes(), 0, Opts::default());
        let tool = p.msgs.iter().find_map(|m| match &m.body {
            ChatBody::Tool { status, output, .. } => Some((*status, output.clone())),
            _ => None,
        });
        let (status, output) = tool.unwrap();
        assert_eq!(status, ToolStatus::Done, "not Running forever");
        assert!(output.unwrap().contains("too large"));
        assert_eq!(p.offset, text.len() as u64, "the next import starts after it");
        // A call never answered holds the offset back only so long.
        let mut text = format!("{call}\n");
        for i in 0..=STALE_LINES {
            text.push_str(&format!(
                r#"{{"type":"user","uuid":"n{i}","message":{{"content":"more"}}}}"#
            ));
            text.push('\n');
        }
        let p = parse_claude(text.as_bytes(), 0, Opts::default());
        assert_eq!(p.offset, text.len() as u64);
    }

    #[test]
    fn titles_keep_no_credentials() {
        let t = tool_title(
            "Bash",
            &serde_json::json!({ "command": "curl -H 'Authorization: Bearer abcdef0123456789' https://x" }),
        );
        assert!(!t.contains("abcdef0123456789"), "{t}");
        let t = tool_title(
            "Bash",
            &serde_json::json!({ "command": "export GITHUB_TOKEN=s3cr3t-v4lue" }),
        );
        assert!(!t.contains("s3cr3t"), "{t}");
        assert_eq!(
            tool_title("Read", &serde_json::json!({ "file_path": "src/main.rs" })),
            "src/main.rs"
        );
    }

    #[test]
    fn an_odd_file_name_is_no_panic() {
        let t = tempfile::tempdir().unwrap();
        // No session id inside, and a name whose last 36 bytes start mid-character.
        let f = t.path().join(format!("rollout-{}z.jsonl", "é".repeat(20)));
        std::fs::write(&f, "{\"type\":\"event_msg\"}\n").unwrap();
        let p = read_session("codex", &f, 0, Opts::default()).unwrap();
        assert!(p.id.starts_with("rollout-"), "{}", p.id);
        let short = t.path().join("é.jsonl");
        std::fs::write(&short, "\n").unwrap();
        assert_eq!(read_session("codex", &short, 0, Opts::default()).unwrap().id, "é");
    }
}
