//! Chats (SPEC §22): ptyro's own record of a plain conversation with an agent. A chat is a tree
//! of nodes, each one a transcript item ([`ChatBody`]) with a parent; the shown branch runs from
//! the root to the chat's `leaf`. The agent's conversation (an *engine*, a hidden structured
//! session) serves the record and can be replaced: when it is gone, can't continue, or is another
//! agent, the branch is replayed to a new one as a seed block (§22.6).

use serde::{Deserialize, Serialize};

use crate::chat::{ChatBody, ChatItem};
use crate::status::{AgentStatus, Attention};

/// A chat as clients list it (never its transcript).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInfo {
    /// A ULID, unique on every machine.
    pub id: String,
    /// The shortest unambiguous suffix of `id` (`7f3k`), shown as `c:7f3k`.
    pub handle: String,
    /// The workspace it belongs to; its machine's prefix when it is another machine's.
    pub workspace: String,
    /// The machine it lives on, when that is another one this daemon mirrors; `None`: here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `auto`, `agent` or `user` (a user's title is never replaced).
    #[serde(default)]
    pub title_src: String,
    pub agent: String,
    pub profile: String,
    /// The tool preset (`chat`, `read`, `none`, or the user's own).
    pub tools: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    /// Kept in memory only: nothing of it is written (§22.9).
    #[serde(default)]
    pub incognito: bool,
    /// Made by forking another chat.
    #[serde(default)]
    pub forked: bool,
    /// A side chat of this chat (SPEC §22.7): it reads the chat, never writes back, isn't
    /// listed with the others, and goes when closed unless kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_of: Option<String>,
    /// The tip of the shown branch.
    #[serde(default)]
    pub leaf: Option<String>,
    #[serde(default)]
    pub nodes: u32,
    /// Replies that arrived while nobody was looking.
    #[serde(default)]
    pub unread: u32,
    pub created_ms: u64,
    /// The last message.
    pub last_ms: u64,
    /// What it has cost so far, where the agent says (Claude).
    #[serde(default)]
    pub cost_micro_usd: u64,
    /// The engine's status while one runs (`None`: no engine, the chat costs nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    /// Set when it wants you (a permission request, an error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    /// The engine's session, while one runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Folders it may read, with a preset that reads (CHAT4, SPEC §22.5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// Its agent can't enforce the preset by itself, or only partly: ptyro's answer to each
    /// permission request stands behind it (`tools: … · best effort`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub best_effort: bool,
    /// Its preset gives the agent a shell (shown with ⚠, like a `dangerous` session).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shell: bool,
}

/// A tool preset as clients list it (`ChatPresets`, SPEC §22.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPreset {
    pub name: String,
    /// One line: what a chat with it may use.
    pub about: String,
    /// One of `chat`, `read`, `none` (and not changed in the config).
    #[serde(default)]
    pub builtin: bool,
    /// It reads folders (so `--folder` means something).
    #[serde(default)]
    pub read: bool,
    /// It gives the agent a shell (⚠).
    #[serde(default)]
    pub shell: bool,
}

impl ChatInfo {
    /// What a person calls it: its title, else "new chat".
    pub fn label(&self) -> &str {
        self.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("new chat")
    }

    /// The header's badge: `tools: read · best effort · ⚠ shell` (SPEC §22.12).
    pub fn tools_badge(&self) -> String {
        let mut s = format!("tools: {}", self.tools);
        if self.best_effort {
            s.push_str(" · best effort");
        }
        if self.shell {
            s.push_str(" · ⚠ shell");
        }
        s
    }

    /// `c:7f3k`, or `laptop2:c:7f3k` for another machine's.
    pub fn addr(&self) -> String {
        match &self.machine {
            Some(m) => format!("{m}:c:{}", self.handle),
            None => format!("c:{}", self.handle),
        }
    }

    /// The status glyph of its row (SPEC §11.1, §22.12).
    pub fn glyph(&self) -> &'static str {
        if self.attention.is_some() {
            return "◉";
        }
        match self.status {
            Some(AgentStatus::Working | AgentStatus::Starting) => "●",
            Some(AgentStatus::Blocked) => "◉",
            Some(AgentStatus::Errored) => "✗",
            _ if self.incognito => "◌",
            _ if self.pinned => "★",
            _ if self.side_of.is_some() => "↳",
            _ => " ",
        }
    }
}

/// One node of a chat's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatNode {
    /// A ULID.
    pub id: String,
    /// `None` for the first node.
    #[serde(default)]
    pub parent: Option<String>,
    pub at_ms: u64,
    #[serde(flatten)]
    pub body: ChatBody,
}

impl ChatNode {
    /// As a transcript item (the chat view's unit), whose id is the node's.
    pub fn item(&self) -> ChatItem {
        ChatItem { id: self.id.clone(), at_ms: self.at_ms, rev: 0, body: self.body.clone() }
    }
}

/// Where the shown branch has a choice (SPEC §22.7): `node` is the `at`-th (from 1) of `of`
/// siblings, `ids` all of them in order. Only nodes with siblings are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSiblings {
    pub node: String,
    pub at: u32,
    pub of: u32,
    pub ids: Vec<String>,
}

/// One search hit (SPEC §22.10): a message (or, with no `node`, a title) of a chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHit {
    pub chat: ChatInfo,
    /// The node it is in; `None`: the chat's title matched.
    #[serde(default)]
    pub node: Option<String>,
    /// `user`, `assistant`, `tool` or `title`.
    pub kind: String,
    /// The text around the match, each match between [`MARK_ON`] and [`MARK_OFF`].
    pub snippet: String,
    pub at_ms: u64,
    /// bm25 × 1000, lower is better (comparable within one machine only).
    pub rank: i64,
    /// How it was found (SPEC §22.10): `words`, `meaning` (shown `≈`: no word of the query is
    /// in it) or `both`; empty from a daemon before meaning search (words).
    #[serde(default)]
    pub how: String,
}

/// Around a match in a snippet (control characters, so no text can fake one).
pub const MARK_ON: char = '\u{2}';
pub const MARK_OFF: char = '\u{3}';

/// What a search asks (SPEC §22.10): the FTS5 expression, and the filters typed into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// For `MATCH`; empty when only filters were typed.
    pub fts: String,
    /// The plain words (for a title match).
    pub words: Vec<String>,
    pub agent: Option<String>,
    pub since_ms: Option<u64>,
    pub workspace: Option<String>,
    /// `by:words` or `by:meaning` (SPEC §22.10): only that kind of search; `None`: both, when
    /// meaning search is on.
    pub by: Option<String>,
    /// The words left out (`-word`), quoted for FTS5 and joined by `OR`; empty when none.
    pub not: String,
}

/// Parse a search: plain words (all must match), `"a phrase"`, `-word`, `prefix*`, and the
/// filters `agent:codex`, `since:7d`, `workspace:work`, `by:words|meaning`. Every term is quoted for FTS5, so no
/// typed character is an operator by accident.
pub fn parse_query(q: &str, now_ms: u64) -> Result<Query, String> {
    let mut out = Query::default();
    let (mut yes, mut no): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut rest = q.trim();
    while !rest.is_empty() {
        let (neg, r) = match rest.strip_prefix('-') {
            Some(r) if !r.starts_with(' ') && !r.is_empty() => (true, r),
            _ => (false, rest),
        };
        let (term, phrase, after) = if let Some(r) = r.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            (r[..end].to_owned(), true, r.get(end + 1..).unwrap_or(""))
        } else {
            let end = r.find(char::is_whitespace).unwrap_or(r.len());
            (r[..end].to_owned(), false, &r[end..])
        };
        rest = after.trim_start();
        if term.trim().is_empty() {
            continue;
        }
        if !phrase
            && !neg
            && let Some((k, v)) = term.split_once(':')
        {
            match k {
                "agent" => {
                    out.agent = Some(v.to_owned());
                    continue;
                }
                "workspace" | "ws" => {
                    out.workspace = Some(v.to_owned());
                    continue;
                }
                "by" if matches!(v, "words" | "meaning") => {
                    out.by = Some(v.to_owned());
                    continue;
                }
                "by" => return Err(format!("by:{v}: say by:words or by:meaning")),
                "since" => {
                    let d =
                        crate::config::parse_duration(v).map_err(|e| format!("since:{v}: {e}"))?;
                    out.since_ms = Some(now_ms.saturating_sub(d.as_millis() as u64));
                    continue;
                }
                _ => {}
            }
        }
        let prefix = !phrase && term.ends_with('*') && term.len() > 1;
        let bare = if prefix { &term[..term.len() - 1] } else { &term };
        let quoted = format!("\"{}\"{}", bare.replace('"', ""), if prefix { "*" } else { "" });
        if neg {
            no.push(quoted);
        } else {
            out.words.push(bare.to_owned());
            yes.push(quoted);
        }
    }
    if yes.is_empty() && !no.is_empty() {
        return Err("say what to look for, not only what to leave out".into());
    }
    out.fts = yes.join(" ");
    out.not = no.join(" OR ");
    for n in no {
        out.fts = format!("{} NOT {n}", out.fts);
    }
    Ok(out)
}

/// The `kind` column of a node (§22.4).
pub fn kind(b: &ChatBody) -> &'static str {
    match b {
        ChatBody::User { .. } => "user",
        ChatBody::Assistant { .. } => "assistant",
        ChatBody::Thinking { .. } => "thinking",
        ChatBody::Tool { .. } => "tool",
        ChatBody::Permission { .. } => "permission",
        ChatBody::Plan { .. } => "plan",
        ChatBody::Notice { .. } => "notice",
        ChatBody::TurnEnd { .. } => "turn_end",
    }
}

/// What is searched (§22.4): user and assistant text, a tool's title (with its output, capped,
/// when `tool_output`). Thinking and diffs never are.
pub fn search_text(b: &ChatBody, tool_output: bool) -> Option<String> {
    match b {
        ChatBody::User { text } | ChatBody::Assistant { text, .. } => Some(text.clone()),
        ChatBody::Tool { name, title, output, .. } => {
            let mut s = format!("{name} {title}");
            if let (true, Some(o)) = (tool_output, output) {
                s.push('\n');
                s.push_str(&clip(o, 2048));
            }
            Some(s)
        }
        _ => None,
    }
}

/// Still being written (a streaming answer).
pub fn streaming(b: &ChatBody) -> bool {
    matches!(b, ChatBody::Assistant { streaming: true, .. })
}

/// A new chat or node id (a ULID, lowercase like session ids).
pub fn new_id() -> String {
    crate::SessionId::new().full()
}

/// The shortest suffix of `id` (at least 4 characters) that no other id shares.
pub fn handle<'a>(id: &str, all: impl IntoIterator<Item = &'a str> + Clone) -> String {
    for len in crate::ids::MIN_HANDLE..id.len() {
        let suffix = &id[id.len() - len..];
        if !all.clone().into_iter().any(|o| o != id && o.ends_with(suffix)) {
            return suffix.to_owned();
        }
    }
    id.to_owned()
}

/// The first `n` bytes of `s`, cut at a character boundary, with `…` when cut.
pub fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_owned();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The opening line of a seed block (§22.6).
pub const SEED_HEAD: &str =
    "The conversation so far, saved by ptyro (continue it; do not reply to this block):";

/// A branch as a transcript the next engine is given ahead of the person's message (§22.6):
/// user and assistant text and tool calls (output cut to 500 bytes), newest kept first within
/// `budget_tokens` (4 bytes a token); the newest is always carried, cut to fit. Returns the block and how many messages it
/// carries; `None` when there is nothing to carry.
pub fn seed_block(branch: &[ChatNode], budget_tokens: usize) -> Option<(String, usize)> {
    let budget = budget_tokens.saturating_mul(4);
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut left_out = 0usize;
    for n in branch.iter().rev() {
        let line = match &n.body {
            ChatBody::User { text } => format!("[user] {}", text.trim()),
            ChatBody::Assistant { text, .. } if !text.trim().is_empty() => {
                format!("[assistant] {}", text.trim())
            }
            ChatBody::Tool { name, title, output, .. } => {
                let out = output.as_deref().map(|o| clip(o.trim(), 500)).unwrap_or_default();
                if out.is_empty() {
                    format!("[tool {name} {title:?}]")
                } else {
                    format!("[tool {name} {title:?}] → {out}")
                }
            }
            _ => continue,
        };
        // The newest message always goes (cut to the budget): a long last answer must not
        // leave the next agent with nothing.
        let line = if lines.is_empty() && line.len() > budget {
            clip(&line, budget.saturating_sub(3))
        } else {
            line
        };
        if left_out > 0 || used + line.len() > budget {
            left_out += 1;
            continue;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    let carried = lines.len();
    lines.reverse();
    let mut s = format!("{SEED_HEAD}\n<conversation>\n");
    if left_out > 0 {
        s.push_str(&format!("({left_out} earlier messages left out)\n"));
    }
    s.push_str(&lines.join("\n"));
    s.push_str("\n</conversation>\n\n");
    Some((s, carried))
}

/// The first line of a message, as a title of last resort (§22.8).
pub fn first_line_title(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 { format!("{}…", t.trim_end()) } else { t }
}

/// A title from an agent or a model: one line, no quotes or a trailing full stop, at most 60
/// characters. Empty when nothing is left.
pub fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches("Title:").trim();
    let line = line.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*' | '#')).trim();
    let line = line.trim_end_matches('.');
    first_line_title(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, body: ChatBody) -> ChatNode {
        ChatNode { id: id.into(), parent: None, at_ms: 0, body }
    }

    #[test]
    fn seeds_keep_the_newest_within_the_budget() {
        let branch = vec![
            n("1", ChatBody::User { text: "first question ".repeat(20) }),
            n("2", ChatBody::Assistant { text: "first answer".into(), streaming: false }),
            n("3", ChatBody::Thinking { text: "never sent".into() }),
            n(
                "4",
                ChatBody::Tool {
                    name: "WebSearch".into(),
                    title: "rust lifetimes".into(),
                    status: crate::chat::ToolStatus::Done,
                    output: Some("x".repeat(900)),
                    diff: None,
                    files: vec![],
                },
            ),
            n("5", ChatBody::User { text: "second".into() }),
        ];
        let (all, k) = seed_block(&branch, 30_000).unwrap();
        assert_eq!(k, 4);
        assert!(all.starts_with(SEED_HEAD) && all.contains("[assistant] first answer"));
        assert!(!all.contains("never sent"));
        assert!(all.contains("[tool WebSearch \"rust lifetimes\"] → ") && all.contains('…'));
        // A small budget keeps the newest and says what was left out.
        let (some, k) = seed_block(&branch, 160).unwrap();
        assert!(
            k < 4 && some.contains("[user] second") && some.contains("earlier messages left out")
        );
        assert!(seed_block(&branch[2..3], 100).is_none());
        // One message longer than the whole budget: still carried, cut.
        let long = vec![n("9", ChatBody::Assistant { text: "y".repeat(10_000), streaming: false })];
        let (block, k) = seed_block(&long, 100).unwrap();
        assert!(k == 1 && block.len() < 1000 && block.contains('…'));
    }

    #[test]
    fn queries() {
        let q =
            parse_query(r#"retry "back off" -redis migrat* agent:codex since:7d"#, 10 * 86_400_000)
                .unwrap();
        assert_eq!(q.fts, r#""retry" "back off" "migrat"* NOT "redis""#);
        assert_eq!(q.words, ["retry", "back off", "migrat"]);
        assert_eq!(q.agent.as_deref(), Some("codex"));
        assert_eq!(q.since_ms, Some(3 * 86_400_000));
        // Nothing typed is an operator by accident.
        assert_eq!(parse_query("a AND (b", 0).unwrap().fts, r#""a" "AND" "(b""#);
        assert!(parse_query("-only", 0).is_err());
        assert_eq!(parse_query("agent:claude", 0).unwrap().fts, "");
        assert!(parse_query("since:soon x", 0).is_err());
        // Meaning search's: which kind, and the words left out on their own.
        assert_eq!(q.not, r#""redis""#);
        let m = parse_query("cats -dog -bird by:meaning", 0).unwrap();
        assert_eq!((m.by.as_deref(), m.not.as_str()), (Some("meaning"), r#""dog" OR "bird""#));
        assert_eq!(m.words, ["cats"]);
        assert!(parse_query("x by:vibes", 0).is_err());
    }

    #[test]
    fn titles_and_handles() {
        assert_eq!(clean_title("\"Rust lifetimes explained.\"\n"), "Rust lifetimes explained");
        assert_eq!(clean_title("Title: Dinner ideas"), "Dinner ideas");
        assert_eq!(first_line_title("\n  hello there\nmore"), "hello there");
        assert!(first_line_title(&"a".repeat(80)).ends_with('…'));
        let ids = ["01aaaa7f3k", "01bbbb7f3k", "01cccc1234"];
        assert_eq!(handle(ids[0], ids), "a7f3k");
        assert_eq!(handle(ids[2], ids), "1234");
        assert_eq!(kind(&ChatBody::User { text: String::new() }), "user");
        assert_eq!(clip("héllo", 2), "h…");
    }
}
