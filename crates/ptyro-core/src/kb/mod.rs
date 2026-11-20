//! The knowledge base's plain parts (SPEC §25.15): what is made of a source before it is stored.
//! Text in, pieces out; nothing here reads a file or talks to a database.
//!
//! - [`ingest`]: the one way in (§25.5). A [`ingest::Source`] is gated, redacted, chunked and
//!   hashed into an [`ingest::Plan`], which is the only thing `kb.db` writes.
//! - [`redact`]: secrets out of text (§25.7), also used for what a chat's titler is sent.
//! - [`card`]: a session's first chunk, made from what is known of it.
//! - [`chunk`]: the cutters: any text, Markdown by its headings, messages by their turns.
//! - [`comments`]: the comments of a source file.
//! - [`mod@file`]: what is made of a file under a root, and of a commit.
//! - [`note`]: a note's file: its header, its title, where it lives.
//! - [`shell`]: a shell's commands, and what is never kept of one.
//! - [`search`]: what a search asks and answers, and the ranking (§25.8).
//! - [`offer`]: what is offered at a prompt (§25.9): the gates on a prompt, the pointers, the cap.
//! - The types of what is stored: [`KbItem`], [`KbChunk`], [`KbRoot`].

use serde::{Deserialize, Serialize};

pub mod card;
pub mod chunk;
pub mod comments;
pub mod file;
pub mod ingest;
pub mod note;
pub mod redact;
pub mod shell;

/// Names that are stored as text (`item.kind`, `chunk.kind`…): each has the word the file
/// holds, and reads it back.
macro_rules! words {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $word:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vdoc])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The word stored for it.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $word),+ }
            }

            /// The kind a stored word names.
            pub fn parse(word: &str) -> Option<Self> {
                match word { $($word => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub mod offer;
pub mod search;

words! {
    /// What an item is (SPEC §25.3): one thing you can open.
    ItemKind {
        /// An agent's conversation: `session://<agent>/<conversation id>`.
        Session => "session",
        /// A file under a root: `file://<absolute path>`.
        File => "file",
        /// `commit://<root id>/<sha>`.
        Commit => "commit",
        /// `note://<profile>/<project or _global>/<slug>`.
        Note => "note",
        /// A shell's day: `shell://<session id>/<YYYY-MM-DD>`.
        Shell => "shell",
    }
}

words! {
    /// What a chunk is (SPEC §25.3): one searchable piece of an item.
    ChunkKind {
        /// A session's card: its task, outcome and files, chunk 0.
        Card => "card",
        User => "user",
        Assistant => "assistant",
        /// A tool call's title.
        Tool => "tool",
        /// A summary the agent wrote of its own session (a compaction).
        Summary => "summary",
        /// A document's or a note's section.
        Text => "text",
        /// A file's path and its name's parts; the files a commit changed.
        Path => "path",
        Comment => "comment",
        Code => "code",
        /// A commit's message.
        Message => "message",
        Command => "command",
        Output => "output",
    }
}

impl ChunkKind {
    /// Prose gets a vector (meaning search, §25.8); paths, code, tool titles, commands and
    /// output are searched by words alone.
    pub fn prose(self) -> bool {
        matches!(
            self,
            Self::Card
                | Self::User
                | Self::Assistant
                | Self::Summary
                | Self::Text
                | Self::Comment
                | Self::Message
        )
    }
}

words! {
    /// Who wrote an item, as the daemon knows it: never a caller's claim (SPEC §25.5).
    Writer {
        User => "user",
        Agent => "agent",
        /// Read from a file or a repository under a root.
        File => "file",
        /// Read from what an agent kept before ptyro did (`ingest --history`).
        Import => "import",
        Ptyro => "ptyro",
    }
}

words! {
    /// What a root is (SPEC §25.3).
    RootKind {
        Project => "project",
        Folder => "folder",
        File => "file",
    }
}

words! {
    RootState {
        Ok => "ok",
        Syncing => "syncing",
        /// Its path is gone; its items stay until it is back or removed.
        Missing => "missing",
        Error => "error",
    }
}

/// One thing in the knowledge base (`item`, SPEC §25.4), without its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbItem {
    /// A ULID.
    pub id: String,
    /// Its identity: writing the same URI again replaces or continues this item.
    pub uri: String,
    pub kind: ItemKind,
    /// The root it was read under (files and commits).
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Its path inside its root.
    #[serde(default)]
    pub rel: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    /// `None`: every profile may see it (§25.9).
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// ptyro's session id, when a ptyro session made it.
    #[serde(default)]
    pub session: Option<String>,
    pub writer: Writer,
    /// A session's touched files, a commit's changed files.
    #[serde(default)]
    pub files: Vec<String>,
    /// SHA-256 of what was stored, for an item that is replaced whole ([`ingest::Plan::hash`]).
    #[serde(default)]
    pub hash: Option<String>,
    /// Where reading stopped: `{dev, ino, size, offset}` or `{mtime, size}`.
    #[serde(default)]
    pub cursor: Option<serde_json::Value>,
    pub chunks: u32,
    #[serde(default)]
    pub started_ms: Option<u64>,
    #[serde(default)]
    pub ended_ms: Option<u64>,
    #[serde(default)]
    pub ingested_ms: Option<u64>,
    /// The machine that wrote it.
    pub origin: String,
    /// A tombstone: its text is gone and must not come back (§25.12).
    #[serde(default)]
    pub forgotten_ms: Option<u64>,
}

/// One searchable piece of an item (`chunk`, SPEC §25.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbChunk {
    /// This file's own number for it (the words' index and the vector go by it); not an
    /// address: that is the item and `idx`.
    pub seq: i64,
    pub item: String,
    pub idx: u32,
    pub kind: ChunkKind,
    /// A message's uuid, `L120-L161`, a heading's slug.
    #[serde(default)]
    pub anchor: Option<String>,
    /// `SPEC.md › 22.4 Storage`, or the line a comment stands on.
    #[serde(default)]
    pub heading: Option<String>,
    /// Redacted (§25.7).
    pub text: String,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
    pub at_ms: u64,
}

/// What a root reads (`root.options`, SPEC §25.6.3). The switches are `None` where the user's
/// settings decide.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RootOptions {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits: Option<bool>,
    /// Read now and never again.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub once: bool,
}

/// A place that was registered (`root`, SPEC §25.3): a project, or a folder or file given to
/// `ptyro ingest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbRoot {
    /// A ULID.
    pub id: String,
    /// What it is called (a project's slug, a folder's name), unique.
    pub name: String,
    pub kind: RootKind,
    /// Absolute and canonical, unique.
    pub path: String,
    /// The project in `ptyro.db`, when it is one or was given.
    #[serde(default)]
    pub project: Option<String>,
    /// `None`: every profile may see it.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub options: RootOptions,
    /// The newest commit ingested.
    #[serde(default)]
    pub head: Option<String>,
    pub state: RootState,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub added_ms: Option<u64>,
    #[serde(default)]
    pub synced_ms: Option<u64>,
}

/// What `ptyro ingest PATH` asks for (SPEC §25.6.3): a folder or a file to read and keep
/// reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RootSpec {
    /// Absolute, or under `~`.
    pub path: String,
    /// What it is called (default: the folder's name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The project it belongs to: a name, an id or a path in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Only this profile's agents may see it (default: every profile's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub options: RootOptions,
}

/// A root as a list shows it: the row, what it holds, and how far a pass over it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbRootInfo {
    #[serde(flatten)]
    pub root: KbRoot,
    /// What its project is called here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub commits: u64,
    /// The pieces of its files and commits.
    #[serde(default)]
    pub chunks: u64,
    /// It is being read now, or waits to be: how many files and commits of this pass are
    /// done, of how many (0 of 0 while it waits or lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<(u64, u64)>,
    /// It is being removed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removing: bool,
}

/// What a root's first read will take, said before it is read (`KbIngested`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KbPlan {
    /// The path was a root already, or inside one: that one is read again.
    pub existing: bool,
    /// Its files come from git (tracked, and untracked that nothing ignores).
    pub git: bool,
    /// The files that will be read (their paths are always kept).
    pub files: u64,
    /// Of those, the documents, and the source files whose comments (and code) are read.
    pub documents: u64,
    pub sources: u64,
    pub comments: bool,
    pub code: bool,
    pub commits: bool,
    /// How many files it holds when that is more than a root takes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over: Option<u64>,
}

impl KbPlan {
    /// The plan in a line: `212 files: 40 documents, 150 source files (comments), commits`.
    pub fn line(&self) -> String {
        let n = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = Vec::new();
        if self.documents > 0 {
            parts.push(n(self.documents, "document", "documents"));
        }
        if self.sources > 0 {
            let what = match (self.comments, self.code) {
                (true, true) => " (comments and code)",
                (true, false) => " (comments)",
                (false, true) => " (code)",
                (false, false) => " (names only)",
            };
            parts.push(format!("{}{what}", n(self.sources, "source file", "source files")));
        }
        if self.commits {
            parts.push("its commits".to_owned());
        }
        let mut line = n(self.files, "file", "files");
        if !parts.is_empty() {
            line = format!("{line}: {}", parts.join(", "));
        }
        if let Some(found) = self.over {
            line = format!(
                "{line} (it holds {found}; a root takes the first {}: `--include` narrows it)",
                self.files
            );
        }
        line
    }
}

/// What `ptyro ingest --history` found in the agents' own folders (`KbHistory`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KbHistoryPlan {
    /// Conversations found.
    pub found: u64,
    /// Of those: kept already (or forgotten, or private), too old to keep, a chat's, and the
    /// ones to read.
    pub known: u64,
    pub old: u64,
    pub chats: u64,
    pub new: u64,
    /// Where they were found: an agent under a profile, its folder, how many.
    pub places: Vec<KbHistoryPlace>,
    /// `false`: nothing was queued (`--dry-run`).
    pub reading: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbHistoryPlace {
    pub agent: String,
    pub profile: String,
    pub dir: String,
    pub found: u64,
    pub new: u64,
}

impl KbHistoryPlan {
    /// The count in words, as every client says it before anything is read: `12
    /// conversations in your agents' folders: 3 kept already…, 8 to read`, then a line a
    /// place.
    pub fn text(&self) -> String {
        if self.found == 0 {
            return "no conversations in your agents' folders\n".into();
        }
        let mut parts = Vec::new();
        for (n, what) in [
            (self.known, "kept already (or forgotten, or private)"),
            (self.chats, "a chat's (its text is in your chats)"),
            (self.old, "too old to keep"),
            (self.new, "to read"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        let mut out = format!(
            "{} conversation{} in your agents' folders: {}\n",
            self.found,
            if self.found == 1 { "" } else { "s" },
            parts.join(", ")
        );
        for place in &self.places {
            out.push_str(&format!(
                "  {} ({}): {} in {}, {} to read\n",
                place.agent, place.profile, place.found, place.dir, place.new
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_in_words() {
        assert_eq!(KbHistoryPlan::default().text(), "no conversations in your agents' folders\n");
        let plan = KbHistoryPlan {
            found: 12,
            known: 3,
            old: 1,
            chats: 0,
            new: 8,
            places: vec![KbHistoryPlace {
                agent: "claude".into(),
                profile: "work".into(),
                dir: "/h/.claude".into(),
                found: 12,
                new: 8,
            }],
            reading: false,
        };
        assert_eq!(
            plan.text(),
            "12 conversations in your agents' folders: 3 kept already (or forgotten, or private), 1 too old to keep, 8 to read\n  claude (work): 12 in /h/.claude, 8 to read\n"
        );
    }

    #[test]
    fn stored_words_read_back_and_match_serde() {
        for k in ChunkKind::ALL {
            assert_eq!(ChunkKind::parse(k.as_str()), Some(*k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        for k in ItemKind::ALL {
            assert_eq!(ItemKind::parse(k.as_str()), Some(*k));
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        for w in Writer::ALL {
            assert_eq!(Writer::parse(w.as_str()), Some(*w));
        }
        assert_eq!(ItemKind::parse("chat"), None);
        // §25.4: tool titles, paths, code, commands and output get no vector.
        let prose: Vec<_> =
            ChunkKind::ALL.iter().filter(|k| k.prose()).map(|k| k.as_str()).collect();
        assert_eq!(prose, ["card", "user", "assistant", "summary", "text", "comment", "message"]);
    }
}
