//! Structured sessions' transcripts (SPEC §7.1, M10): what an agent driven over a structured
//! protocol (Claude stream-json, the Codex app-server, ACP, OpenCode's server) said and did, as
//! items a chat view renders: messages, thinking, tool calls with their output and diffs,
//! permission requests, plans, and turn ends with their cost.
//!
//! Every change bumps the transcript's revision and the item's own, so a client that has seen
//! revision `r` gets only what changed since (streamed text grows one item in place).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatItem {
    /// Stable within the session (`msg_…:0`, a tool call id, a request id).
    pub id: String,
    pub at_ms: u64,
    /// The transcript revision that last changed it.
    #[serde(default)]
    pub rev: u64,
    #[serde(flatten)]
    pub body: ChatBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ChatBody {
    User {
        text: String,
    },
    Assistant {
        text: String,
        /// More text is still arriving.
        #[serde(default)]
        streaming: bool,
    },
    Thinking {
        text: String,
    },
    Tool {
        /// `Bash`, `Edit`, `read_file`, `github.search`…
        name: String,
        /// One line: the command, the file, the query.
        title: String,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// A unified diff for edits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
    },
    Permission {
        /// The protocol's request id (answers go back with it).
        request: String,
        tool: String,
        title: String,
        options: Vec<PermOption>,
        /// The option chosen, `cancelled`, or nothing yet.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided: Option<String>,
    },
    Plan {
        steps: Vec<PlanStep>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    TurnEnd {
        /// `end_turn`, `cancelled`, `failed`, `max_tokens`…
        #[serde(default)]
        stop: String,
        /// What the turn cost, where the protocol says (Claude): millionths of a dollar.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_micro_usd: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
    Declined,
}

impl ToolStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Pending => "◌",
            Self::Running => "⟳",
            Self::Done => "✓",
            Self::Failed => "✗",
            Self::Declined => "⊘",
        }
    }
}

/// What a person can answer a permission request with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermOption {
    pub id: String,
    pub label: String,
    pub kind: PermKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermOption {
    /// The usual three: allow, always, deny.
    pub fn standard() -> Vec<Self> {
        vec![
            Self { id: "allow".into(), label: "Allow".into(), kind: PermKind::AllowOnce },
            Self { id: "always".into(), label: "Always".into(), kind: PermKind::AllowAlways },
            Self { id: "deny".into(), label: "Deny".into(), kind: PermKind::RejectOnce },
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub text: String,
    /// `pending`, `in_progress`, `completed`.
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// A session's transcript, as the daemon keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub items: Vec<ChatItem>,
    pub rev: u64,
}

/// At most this many items stay in memory (the recording keeps everything).
pub const MAX_ITEMS: usize = 2000;
/// Tool output kept per item.
pub const MAX_OUTPUT: usize = 32 * 1024;

impl Transcript {
    /// Insert or replace by id.
    pub fn upsert(&mut self, mut item: ChatItem) -> &ChatItem {
        self.rev += 1;
        item.rev = self.rev;
        clip(&mut item.body);
        let i = match self.items.iter().position(|x| x.id == item.id) {
            Some(i) => {
                item.at_ms = self.items[i].at_ms;
                self.items[i] = item;
                i
            }
            None => {
                self.items.push(item);
                if self.items.len() > MAX_ITEMS {
                    self.items.drain(..self.items.len() - MAX_ITEMS);
                }
                self.items.len() - 1
            }
        };
        &self.items[i]
    }

    /// Change an item in place (returns false when there's none with that id).
    pub fn update(&mut self, id: &str, f: impl FnOnce(&mut ChatBody)) -> bool {
        let Some(i) = self.items.iter().position(|x| x.id == id) else { return false };
        self.rev += 1;
        f(&mut self.items[i].body);
        clip(&mut self.items[i].body);
        self.items[i].rev = self.rev;
        true
    }

    pub fn get(&self, id: &str) -> Option<&ChatItem> {
        self.items.iter().find(|x| x.id == id)
    }

    /// Items changed after revision `rev`.
    pub fn since(&self, rev: u64) -> Vec<ChatItem> {
        self.items.iter().filter(|x| x.rev > rev).cloned().collect()
    }

    /// The open permission request, if any.
    pub fn pending_permission(&self) -> Option<&ChatItem> {
        self.items
            .iter()
            .rev()
            .find(|x| matches!(&x.body, ChatBody::Permission { decided: None, .. }))
    }

    /// Tool calls so far: (count by tool, failed, files touched, commands run, cost).
    pub fn tool_summary(&self) -> ToolSummary {
        let mut s = ToolSummary::default();
        for it in &self.items {
            match &it.body {
                ChatBody::Tool { name, title, status, files, .. } => {
                    s.calls += 1;
                    *s.by_tool.entry(name.clone()).or_default() += 1;
                    if matches!(status, ToolStatus::Failed) {
                        s.failed += 1;
                    }
                    for f in files {
                        if !s.files.contains(f) {
                            s.files.push(f.clone());
                        }
                    }
                    if is_command(name) && s.commands.len() < 20 {
                        s.commands.push(title.clone());
                    }
                }
                ChatBody::TurnEnd { cost_micro_usd: Some(c), .. } => {
                    *s.cost_micro_usd.get_or_insert(0) += c;
                }
                _ => {}
            }
        }
        s
    }
}

fn is_command(tool: &str) -> bool {
    matches!(tool, "Bash" | "command" | "shell" | "run_shell_command" | "execute" | "bash")
}

fn clip(b: &mut ChatBody) {
    if let ChatBody::Tool { output: Some(o), .. } = b
        && o.len() > MAX_OUTPUT
    {
        let cut = o
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i <= o.len() - MAX_OUTPUT)
            .last()
            .unwrap_or(0);
        o.replace_range(..cut, "…\n");
    }
}

/// What a session's tool calls add up to (the compare view, M10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSummary {
    pub calls: u32,
    pub by_tool: std::collections::BTreeMap<String, u32>,
    pub failed: u32,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
}

impl ChatItem {
    pub fn new(id: impl Into<String>, body: ChatBody) -> Self {
        Self { id: id.into(), at_ms: crate::now_ms(), rev: 0, body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_track_changes() {
        let mut t = Transcript::default();
        t.upsert(ChatItem::new("u1", ChatBody::User { text: "hi".into() }));
        t.upsert(ChatItem::new("a1", ChatBody::Assistant { text: "he".into(), streaming: true }));
        let r = t.rev;
        assert!(t.update("a1", |b| if let ChatBody::Assistant { text, .. } = b {
            text.push_str("llo")
        }));
        let changed = t.since(r);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].body, ChatBody::Assistant { text: "hello".into(), streaming: true });
        assert!(!t.update("nope", |_| {}));
        let j = serde_json::to_value(&t.items[1]).unwrap();
        assert_eq!(j["t"], "assistant");
        assert_eq!(j["id"], "a1");
    }

    #[test]
    fn summaries_and_pending_permissions() {
        let mut t = Transcript::default();
        for (id, name, title, st) in [
            ("1", "Bash", "cargo test", ToolStatus::Failed),
            ("2", "Edit", "src/a.rs", ToolStatus::Done),
            ("3", "Bash", "ls", ToolStatus::Done),
        ] {
            t.upsert(ChatItem::new(
                id,
                ChatBody::Tool {
                    name: name.into(),
                    title: title.into(),
                    status: st,
                    output: None,
                    diff: None,
                    files: if name == "Edit" { vec![title.into()] } else { vec![] },
                },
            ));
        }
        t.upsert(ChatItem::new(
            "p",
            ChatBody::Permission {
                request: "r1".into(),
                tool: "Bash".into(),
                title: "rm".into(),
                options: PermOption::standard(),
                decided: None,
            },
        ));
        t.upsert(ChatItem::new(
            "e",
            ChatBody::TurnEnd {
                stop: "end_turn".into(),
                cost_micro_usd: Some(500_000),
                duration_ms: None,
                input_tokens: None,
                output_tokens: None,
            },
        ));
        let s = t.tool_summary();
        assert_eq!((s.calls, s.failed, s.by_tool["Bash"]), (3, 1, 2));
        assert_eq!(s.files, ["src/a.rs"]);
        assert_eq!(s.commands, ["cargo test", "ls"]);
        assert_eq!(s.cost_micro_usd, Some(500_000));
        assert_eq!(t.pending_permission().unwrap().id, "p");
    }

    #[test]
    fn long_output_is_clipped_from_the_front() {
        let mut t = Transcript::default();
        let big = "x".repeat(MAX_OUTPUT * 2) + "TAIL";
        t.upsert(ChatItem::new(
            "t",
            ChatBody::Tool {
                name: "Bash".into(),
                title: "yes".into(),
                status: ToolStatus::Done,
                output: Some(big),
                diff: None,
                files: vec![],
            },
        ));
        let ChatBody::Tool { output: Some(o), .. } = &t.items[0].body else { panic!() };
        assert!(o.len() <= MAX_OUTPUT + 4 && o.ends_with("TAIL"));
    }
}
