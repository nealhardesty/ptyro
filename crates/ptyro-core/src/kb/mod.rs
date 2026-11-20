//! The knowledge base's plain parts (SPEC §25.15): what is made of a source before it is stored.
//! Text in, pieces out; nothing here reads a file or talks to a database.
//!
//! - [`ingest`]: the one way in (§25.5). A [`ingest::Source`] is gated, redacted, chunked and
//!   hashed into an [`ingest::Plan`], which is the only thing `kb.db` writes.
//! - [`redact`]: secrets out of text (§25.7), also used for what a chat's titler is sent.
//! - [`card`]: a session's first chunk, made from what is known of it.
//! - [`chunk`]: the cutters: any text, Markdown by its headings, messages by their turns.
//! - [`comments`]: the comments of a source file.
//! - [`note`]: a note's file: its header, its title, where it lives.
//! - [`search`]: what a search asks and answers, and the ranking (§25.8).
//! - The types of what is stored: [`KbItem`], [`KbChunk`], [`KbRoot`].

use serde::{Deserialize, Serialize};

pub mod card;
pub mod chunk;
pub mod comments;
pub mod ingest;
pub mod note;
pub mod redact;

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

#[cfg(test)]
mod tests {
    use super::*;

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
