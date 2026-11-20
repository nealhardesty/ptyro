//! Reading the agents' own transcripts for the knowledge base (SPEC §25.6.1; the formats are
//! §20.8's, checked against Claude Code 2.1.329 and Codex 0.167.1).
//!
//! A reader is given a file's bytes from an offset and gives back what was said in them as
//! parts (`ptyro_core::kb::chunk::Part`): the user's own words, the assistant's text, a title
//! for each tool call, a compaction's summary. **Never** thinking or reasoning, never a tool's
//! result, never the lines that are not conversation (Claude's hold the account's email and the
//! system prompt). It also says where the complete lines end, so the next read starts there: a
//! last line with no newline yet is still being written and is never parsed.
//!
//! The readers are pure and never panic, whatever the bytes ([`claude`], [`codex`]); finding
//! the file is the adapter's ([`crate::Adapter::transcript`]).

use std::path::{Path, PathBuf};

use ptyro_core::kb::ChunkKind;
use ptyro_core::kb::chunk::Part;
use serde_json::Value;

/// A line longer than this is passed over unread (a pasted image, a huge tool result).
pub const MAX_LINE: usize = 4 << 20;
/// The most of a command, a path or a query that a tool's title keeps.
const TITLE: usize = 200;

/// What a piece of a transcript said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    /// In the file's order. A part's anchor is its message's own id (Claude's `uuid`, Codex's
    /// `id`), so a piece read twice is the same parts.
    pub parts: Vec<Part>,
    /// Where the complete lines end, as an offset in the file: the next read's `from`.
    pub to: u64,
    /// The conversation's id, when the piece names it.
    pub conversation: Option<String>,
    /// A title the agent keeps in the transcript (Claude's `custom-title`).
    pub title: Option<String>,
    /// The model of the piece's last assistant turn.
    pub model: Option<String>,
    /// The files the piece's tool calls changed, each once, in order.
    pub files: Vec<String>,
}

impl Transcript {
    /// The piece's last assistant text: what `Stop`'s `last_assistant_message` should be.
    pub fn last_reply(&self) -> Option<&str> {
        self.parts.iter().rev().find(|p| p.kind == ChunkKind::Assistant).map(|p| p.text.as_str())
    }

    fn file(&mut self, path: &str) {
        let path = path.trim();
        if !path.is_empty() && !self.files.iter().any(|f| f == path) {
            self.files.push(path.to_owned());
        }
    }

    fn push(&mut self, kind: ChunkKind, anchor: &str, text: &str, at_ms: u64) {
        let text = text.trim();
        if !text.is_empty() {
            self.parts.push(Part::new(kind, text, at_ms).anchor(anchor));
        }
    }
}

/// Where to look for a conversation's transcript.
#[derive(Debug, Clone, Copy)]
pub struct TranscriptAt<'a> {
    /// The agent's config directory under the session's profile (`CLAUDE_CONFIG_DIR`,
    /// `CODEX_HOME`, or their defaults).
    pub home: &'a Path,
    /// The session's working directory.
    pub cwd: &'a Path,
    pub conversation: &'a str,
    /// The `transcript_path` a hook named, if one did. It is taken only when it is this
    /// conversation's file inside `home`: a hook can't point the reader at another file.
    pub hook: Option<&'a Path>,
}

/// Whether `path` is inside `dir` (as written, or once both are resolved).
pub(crate) fn inside(path: &Path, dir: &Path) -> bool {
    path.is_absolute()
        && (path.starts_with(dir)
            || matches!(
                (std::fs::canonicalize(path), std::fs::canonicalize(dir)),
                (Ok(p), Ok(d)) if p.starts_with(&d)
            ))
}

/// The complete lines of `bytes`, each with the offset in the file it starts at (`bytes`
/// starts at `from`); the offset after the last of them. A line that is too long or not
/// UTF-8 is passed over.
fn lines(bytes: &[u8], from: u64, mut each: impl FnMut(&str, u64)) -> u64 {
    let mut at = 0;
    while let Some(n) = bytes[at..].iter().position(|b| *b == b'\n') {
        let line = &bytes[at..at + n];
        if line.len() <= MAX_LINE
            && let Ok(s) = std::str::from_utf8(line)
            && !s.trim().is_empty()
        {
            each(s.trim(), from + at as u64);
        }
        at += n + 1;
    }
    from + at as u64
}

/// An RFC 3339 time (`2026-10-25T05:05:24.025Z`, `…+02:00`) as Unix milliseconds.
fn iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok().filter(|n| *n >= 0);
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = s.get(19..)?;
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

fn stamp(v: &Value) -> u64 {
    v["timestamp"].as_str().and_then(iso_ms).unwrap_or(0)
}

/// The first line of `s`, at most [`TITLE`] characters.
fn telling(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= TITLE {
        return line.to_owned();
    }
    line.chars().take(TITLE - 1).chain(['…']).collect()
}

/// A tool call in a line: its name and its one telling input (a command, a path, a pattern, a
/// URL, a query).
fn tool_title(name: &str, input: &Value) -> String {
    let input = match input {
        // (Codex passes a function's arguments as a JSON string.)
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        v => v.clone(),
    };
    const KEYS: [&str; 10] = [
        "command",
        "cmd",
        "file_path",
        "notebook_path",
        "pattern",
        "query",
        "url",
        "path",
        "description",
        "prompt",
    ];
    let detail = KEYS.iter().find_map(|k| match &input[k] {
        Value::String(s) if !s.trim().is_empty() => Some(telling(s)),
        Value::Array(a) if !a.is_empty() => Some(telling(&command_line(&input[k]))),
        _ => None,
    });
    match detail {
        Some(d) if !d.is_empty() => format!("{name}: {d}"),
        _ => name.to_owned(),
    }
}

/// A command as one line: an argv's last word when it is a shell's `-c`, else its words.
fn command_line(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let words: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            match words.as_slice() {
                [_, flag, script] if matches!(*flag, "-c" | "-lc") => (*script).to_owned(),
                _ => words.join(" "),
            }
        }
        _ => String::new(),
    }
}

/// Text an agent wrapped around something the user didn't type: a known tag opens it.
fn wrapped(text: &str, tags: &[&str]) -> bool {
    let t = text.trim_start();
    tags.iter().any(|tag| t.starts_with(tag))
}

/// Text that opens with any `<tag>` at all (for formats that don't say whose a message is).
fn any_tag(text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('<') else { return false };
    let name: String =
        rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    !name.is_empty() && rest[name.len()..].starts_with('>')
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = text.split_once(open)?.1;
    Some(rest.split_once(close)?.0)
}

// ── Claude Code ─────────────────────────────────────────────────────────────────────────────

/// What Claude Code wraps around things the user didn't type (§20.8).
const CLAUDE_WRAPS: [&str; 6] = [
    "<local-command-",
    "<bash-",
    "<task-notification>",
    "<system-reminder>",
    "<command-message>",
    "<user-prompt-submit-hook>",
];
/// Tools whose call changes the file it names.
const CLAUDE_EDITS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The user's own text in a `user` line's string: itself, a slash command as `/name args`, or
/// nothing when it is a wrapper.
fn claude_typed(s: &str) -> Option<String> {
    if let Some(name) = between(s, "<command-name>", "</command-name>") {
        let args = between(s, "<command-args>", "</command-args>").unwrap_or("").trim();
        return Some(format!("{} {args}", name.trim()).trim().to_owned());
    }
    (!wrapped(s, &CLAUDE_WRAPS)).then(|| s.to_owned())
}

/// A compaction's summary without what is said to the model around it.
fn compact_summary(text: &str) -> &str {
    let mut t = text;
    if let Some(i) = t.find("\nIf you need specific details from before compaction") {
        t = &t[..i];
    }
    if t.starts_with("This session is being continued") {
        t = t.split_once("\n\n").map_or("", |(_, rest)| rest);
    }
    t = t.trim_start();
    if let Some(rest) = t.strip_prefix("<system-reminder>") {
        t = rest.split_once("</system-reminder>").map_or(rest, |(_, after)| after);
    }
    let t = t.trim_start();
    t.strip_prefix("Summary:").unwrap_or(t).trim()
}

/// A piece of a Claude Code transcript (`<config dir>/projects/<cwd>/<id>.jsonl`), `bytes`
/// being the file from offset `from`.
///
/// Only `user` and `assistant` lines are read, and the title lines; a line of any other type
/// is dropped as soon as its type is known. Kept: a `user` line's string when it is the
/// user's (not `isMeta`, not a wrapper; a slash command as `/name args`), an
/// `isCompactSummary` line as a summary, an `assistant` line's `text` blocks, and a title
/// for each `tool_use` but `ToolSearch`. Side chains and API error lines are left out.
pub fn claude(bytes: &[u8], from: u64) -> Transcript {
    #[derive(serde::Deserialize)]
    struct Head<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<std::borrow::Cow<'a, str>>,
    }
    let mut t = Transcript::default();
    let mut rank = 0u8;
    t.to = lines(bytes, from, |line, _| {
        let Ok(head) = serde_json::from_str::<Head<'_>>(line) else { return };
        let kind = head.kind.unwrap_or_default();
        let title = match kind.as_ref() {
            "user" | "assistant" => None,
            "custom-title" => Some((3, "customTitle")),
            "ai-title" => Some((2, "aiTitle")),
            "summary" => Some((2, "summary")),
            "agent-name" => Some((1, "agentName")),
            _ => return,
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if let Some((r, key)) = title {
            let name = v[key].as_str().unwrap_or("").trim();
            if r >= rank && !name.is_empty() {
                rank = r;
                t.title = Some(telling(name));
            }
            return;
        }
        let Some(uuid) = v["uuid"].as_str().filter(|u| !u.is_empty()) else { return };
        if v["isSidechain"] == true || v["isMeta"] == true || v["isApiErrorMessage"] == true {
            return;
        }
        if t.conversation.is_none() {
            t.conversation = v["sessionId"].as_str().map(str::to_owned);
        }
        let at = stamp(&v);
        let content = &v["message"]["content"];
        if kind == "user" {
            if v["isCompactSummary"] == true {
                let text = content.as_str().unwrap_or("");
                return t.push(ChunkKind::Summary, uuid, compact_summary(text), at);
            }
            let typed: Vec<String> = match content {
                Value::String(s) => claude_typed(s).into_iter().collect(),
                // (With an image pasted, the words are a block beside it; a tool's result
                // is a block too, and is never read.)
                Value::Array(blocks) => blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| claude_typed(b["text"].as_str()?))
                    .collect(),
                _ => Vec::new(),
            };
            return t.push(ChunkKind::User, uuid, &typed.join("\n"), at);
        }
        if let Some(model) = v["message"]["model"].as_str().filter(|m| !m.starts_with('<')) {
            t.model = Some(model.to_owned());
        }
        for b in content.as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => {
                    t.push(ChunkKind::Assistant, uuid, b["text"].as_str().unwrap_or(""), at)
                }
                Some("tool_use") => {
                    let name = b["name"].as_str().unwrap_or("tool");
                    // (It only loads another tool.)
                    if name == "ToolSearch" {
                        continue;
                    }
                    if CLAUDE_EDITS.contains(&name) {
                        let input = &b["input"];
                        let path = input["file_path"].as_str().or(input["notebook_path"].as_str());
                        t.file(path.unwrap_or(""));
                    }
                    t.push(ChunkKind::Tool, uuid, &tool_title(name, &b["input"]), at);
                }
                _ => {}
            }
        }
    });
    t
}

/// Claude Code's file for a conversation: the hook's when it is that file, else
/// `<home>/projects/<cwd, every other character a dash>/<id>.jsonl`, else the one of that
/// name in any project folder (the working directory may have been another).
pub(crate) fn claude_path(at: &TranscriptAt<'_>) -> Option<PathBuf> {
    let name = format!("{}.jsonl", at.conversation);
    let projects = at.home.join("projects");
    if let Some(hook) = at.hook
        && hook.file_name().is_some_and(|n| n == name.as_str())
        && inside(hook, &projects)
    {
        return Some(hook.to_owned());
    }
    let slug: String = at
        .cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let own = projects.join(slug).join(&name);
    if own.is_file() {
        return Some(own);
    }
    std::fs::read_dir(&projects).ok()?.flatten().map(|d| d.path().join(&name)).find(|p| p.is_file())
}

// ── Codex ───────────────────────────────────────────────────────────────────────────────────

/// The paths a patch names (`*** Add File: a`, `*** Update File: b`, `*** Delete File: c`).
fn patch_files(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|l| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .find_map(|p| l.strip_prefix(p))
        })
        .map(str::trim)
        .collect()
}

/// A piece of a Codex rollout (`<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`),
/// `bytes` being the file from offset `from`.
///
/// Kept: `response_item` messages of the user (whose kinds include `user.text`) and of the
/// assistant, and a title for each tool that ran. **What ran is in the `event_msg` lines of
/// type `item_completed`** (`CommandExecution`, `FileChange`, `McpToolCall`): today's Codex
/// makes every call through one `exec` tool whose input is a program. A piece with none of
/// those items (an older Codex) gives a title per `function_call`, `custom_tool_call` and
/// `local_shell_call` instead. Never `reasoning`, an output, or Codex's own context messages.
pub fn codex(bytes: &[u8], from: u64) -> Transcript {
    let mut t = Transcript::default();
    // Titles from the calls themselves, kept only when no item says what ran.
    let mut calls: Vec<(usize, Part, Vec<String>)> = Vec::new();
    let mut items = false;
    t.to = lines(bytes, from, |line, start| {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let p = &v["payload"];
        let at = stamp(&v);
        let id = |v: &Value| v.as_str().map_or_else(|| format!("L{start}"), str::to_owned);
        match (v["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
            ("session_meta", _) => {
                t.conversation = p["id"].as_str().or(p["session_id"].as_str()).map(str::to_owned);
            }
            ("turn_context", _) => {
                if let Some(m) = p["model"].as_str().filter(|m| !m.is_empty()) {
                    t.model = Some(m.to_owned());
                }
            }
            ("response_item", "message") => {
                let kinds = p["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                    .as_array();
                let texts: Vec<&str> = p["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["text"].as_str())
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                let text = texts.join("\n");
                let typed = match kinds {
                    Some(k) => k.iter().any(|x| x == "user.text"),
                    // (Before the kinds were written: by what the text looks like.)
                    None => !any_tag(&text) && !text.starts_with("# AGENTS.md"),
                };
                match p["role"].as_str() {
                    Some("user") if typed => t.push(ChunkKind::User, &id(&p["id"]), &text, at),
                    Some("assistant") => t.push(ChunkKind::Assistant, &id(&p["id"]), &text, at),
                    _ => {}
                }
            }
            ("event_msg", "item_completed") => {
                let item = &p["item"];
                let title = match item["type"].as_str() {
                    Some("CommandExecution") => {
                        format!("shell: {}", telling(&command_line(&item["command"])))
                    }
                    Some("FileChange") => {
                        let paths: Vec<&str> = item["changes"]
                            .as_object()
                            .map(|c| c.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        for path in &paths {
                            t.file(path);
                        }
                        format!("patch: {}", telling(&paths.join(", ")))
                    }
                    Some("McpToolCall") => tool_title(
                        &format!(
                            "{}.{}",
                            item["server"].as_str().unwrap_or("mcp"),
                            item["tool"].as_str().unwrap_or("tool")
                        ),
                        &item["arguments"],
                    ),
                    _ => return,
                };
                items = true;
                t.push(ChunkKind::Tool, &id(&item["id"]), &title, at);
            }
            (
                "response_item",
                kind @ ("function_call" | "custom_tool_call" | "local_shell_call"
                | "web_search_call"),
            ) => {
                let name = p["name"].as_str().unwrap_or(match kind {
                    "local_shell_call" => "shell",
                    "web_search_call" => "web_search",
                    _ => "tool",
                });
                // (The wrapper every call goes through now: its input is a program.)
                if kind == "custom_tool_call" && name == "exec" {
                    return;
                }
                let input = ["arguments", "input", "action"]
                    .iter()
                    .map(|k| &p[*k])
                    .find(|v| !v.is_null())
                    .unwrap_or(&Value::Null);
                let patch = input.as_str().filter(|s| s.contains("*** Begin Patch"));
                let files: Vec<String> = patch
                    .map(patch_files)
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect();
                let title = match patch {
                    Some(_) => format!("{name}: {}", telling(&files.join(", "))),
                    None => tool_title(name, input),
                };
                let anchor = id(if p["id"].is_string() { &p["id"] } else { &p["call_id"] });
                let part = Part::new(ChunkKind::Tool, title, at).anchor(anchor);
                calls.push((t.parts.len(), part, files));
            }
            _ => {}
        }
    });
    if !items {
        for (n, (at, part, files)) in calls.into_iter().enumerate() {
            for f in files {
                t.file(&f);
            }
            t.parts.insert(at + n, part);
        }
    }
    t
}

/// Codex's file for a conversation: the hook's when it is that thread's rollout, else the
/// newest `rollout-*-<id>.jsonl` under `<home>/sessions/YYYY/MM/DD/`, then
/// `<home>/archived_sessions/`.
pub(crate) fn codex_path(at: &TranscriptAt<'_>) -> Option<PathBuf> {
    let end = format!("-{}.jsonl", at.conversation);
    let ours = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&end))
    };
    let sessions = at.home.join("sessions");
    let archived = at.home.join("archived_sessions");
    if let Some(hook) = at.hook
        && ours(hook)
        && (inside(hook, &sessions) || inside(hook, &archived))
    {
        return Some(hook.to_owned());
    }
    // Newest first: a session's rollout is almost always today's.
    fn newest(dir: &Path, depth: usize, ours: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        if depth == 0 {
            return entries.into_iter().find(|p| ours(p));
        }
        entries.iter().find_map(|d| newest(d, depth - 1, ours))
    }
    newest(&sessions, 3, &ours).or_else(|| newest(&archived, 0, &ours))
}

/// The title Codex keeps for a thread: the last `{id, thread_name}` line of
/// `<home>/session_index.jsonl` that names it.
pub(crate) fn codex_title(at: &TranscriptAt<'_>) -> Option<String> {
    let text = std::fs::read_to_string(at.home.join("session_index.jsonl")).ok()?;
    text.lines()
        .rev()
        .filter(|l| l.contains(at.conversation))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == at.conversation)
        .and_then(|v| Some(telling(v["thread_name"].as_str()?)))
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests;
