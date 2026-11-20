//! What a search of the knowledge base asks and answers, and the ranking (SPEC §25.8). No SQL
//! and no model here: `ptyro-store` finds the lists, `ptyro_meaning::fuse_lists` merges them
//! by rank, and [`rank`] orders what they found by what it is, how old it is and where you
//! are, and keeps one item from filling the page.

use serde::{Deserialize, Serialize};

use super::{ChunkKind, ItemKind, KbChunk, KbItem, Writer};

words! {
    /// What a hit is, as a search names it (`kind:` in the query, SPEC §25.8).
    HitKind {
        /// Anything of an agent's session: its card, its messages, its tools' titles.
        Session => "session",
        /// A message of a chat, or a chat's title (`chat.db`).
        Chat => "chat",
        Note => "note",
        /// A section of a document.
        Doc => "doc",
        Comment => "comment",
        Code => "code",
        /// A file's path.
        Path => "path",
        /// A commit's message, and the files it changed.
        Commit => "commit",
        /// A shell's command, and what it printed.
        Command => "command",
    }
}

impl HitKind {
    /// What a chunk of an item is to a search.
    pub fn of(item: ItemKind, chunk: ChunkKind) -> Self {
        match (item, chunk) {
            (ItemKind::Session, _) => Self::Session,
            (ItemKind::Note, _) => Self::Note,
            (ItemKind::Commit, _) => Self::Commit,
            (ItemKind::Shell, _) => Self::Command,
            (ItemKind::File, ChunkKind::Comment) => Self::Comment,
            (ItemKind::File, ChunkKind::Code) => Self::Code,
            (ItemKind::File, ChunkKind::Path) => Self::Path,
            (ItemKind::File, _) => Self::Doc,
        }
    }

    /// The items a kind is found in, and of those the chunks (empty: all of them). `None`:
    /// not in `kb.db` (a chat).
    pub fn stored(self) -> Option<(ItemKind, &'static [ChunkKind])> {
        Some(match self {
            Self::Chat => return None,
            Self::Session => (ItemKind::Session, &[]),
            Self::Note => (ItemKind::Note, &[]),
            Self::Commit => (ItemKind::Commit, &[]),
            Self::Command => (ItemKind::Shell, &[]),
            Self::Comment => (ItemKind::File, &[ChunkKind::Comment]),
            Self::Code => (ItemKind::File, &[ChunkKind::Code]),
            Self::Path => (ItemKind::File, &[ChunkKind::Path]),
            Self::Doc => (ItemKind::File, &[ChunkKind::Text]),
        })
    }

    /// `kind:`'s words, for an error that names them.
    pub fn words() -> String {
        Self::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// One search (SPEC §25.8, §25.14's `KbSearch`). The query is the search box's
/// (`chats::parse_kb_query`); the rest is what a caller's options add to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KbSearch {
    pub query: String,
    /// Only this project's (its id, name or path): a filter.
    pub project: Option<String>,
    /// Only these kinds; empty: every kind.
    pub kinds: Vec<HitKind>,
    pub since_ms: Option<u64>,
    pub before_ms: Option<u64>,
    /// 0: [`LIMIT`].
    pub limit: u32,
    /// The project you are in (its id): its hits rank higher. A boost, never a filter.
    pub here: Option<String>,
}

/// Hits a search returns when it isn't told how many.
pub const LIMIT: u32 = 20;
/// The most it returns.
pub const MAX_LIMIT: u32 = 100;

/// One hit (SPEC §25.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KbHit {
    /// Its address: `k:7f3k/12` (an item's handle and the chunk's index), or a chat's
    /// message `7f3k/01JB…` (a chat's title: `7f3k`).
    pub addr: String,
    /// The item's id; for a chat's hit, the chat's.
    pub item: String,
    /// The chunk's index in its item (`None`: a chat's hit).
    #[serde(default)]
    pub idx: Option<u32>,
    /// A chat's message (`None`: its title, or not a chat's hit).
    #[serde(default)]
    pub node: Option<String>,
    /// The item's URI (`None`: a chat's hit).
    #[serde(default)]
    pub uri: Option<String>,
    pub kind: HitKind,
    /// What the piece is: a chunk's kind (`card`, `user`, `text`…), or a chat's (`user`,
    /// `assistant`, `tool`, `title`).
    pub part: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Where in its item: `SPEC.md › 22.4 Storage`, the line a comment stands on.
    #[serde(default)]
    pub heading: Option<String>,
    /// The text around the match, each match between `chats::MARK_ON` and `chats::MARK_OFF`;
    /// for a hit by meaning alone, the piece's opening.
    pub snippet: String,
    pub at_ms: u64,
    /// A project's id.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    /// Who wrote the item (`None`: a chat's hit).
    #[serde(default)]
    pub writer: Option<Writer>,
    /// The machine it is on, when not this one.
    #[serde(default)]
    pub machine: Option<String>,
    /// `words`, `meaning` (shown `≈`: no word of the query is in it) or `both`.
    pub how: String,
    /// What it was ranked by, higher is better (comparable within one answer only).
    pub score: f64,
}

/// What a search answers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KbHits {
    pub hits: Vec<KbHit>,
    /// Why the search was by words alone when meaning was not asked to stay out: it is off,
    /// its model isn't here yet. `None`: meaning was searched too (or `by:words` was asked).
    #[serde(default)]
    pub words_only: Option<String>,
}

/// A piece of an item with its neighbours (SPEC §25.14's `KbDoc`): what an address opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KbDoc {
    pub item: KbItem,
    /// In the item's order.
    pub chunks: Vec<KbChunk>,
    /// The chunk the address named (`None`: it named the item).
    #[serde(default)]
    pub at: Option<u32>,
    /// The address named a chunk the item no longer has (a file read again since): the
    /// item is shown from its start.
    #[serde(default)]
    pub stale: bool,
}

// ── ranking ─────────────────────────────────────────────────────────────────────────────────

/// A hit in the project you are in ranks this much higher.
pub const HERE: f64 = 1.2;
/// What happened loses up to this share of its rank with age…
const AGE_SHARE: f64 = 0.15;
/// …half of it every this long.
const HALF_LIFE_MS: f64 = 180.0 * 86_400_000.0;
/// At most this many hits of one item (or chat) in an answer.
pub const PER_ITEM: usize = 2;
/// A hit takes in at most this many neighbouring chunks of its item.
const NEIGHBOURS: u32 = 2;

/// How much a hit counts for what it is (the golden set tunes these, §25.16).
pub fn kind_weight(kind: HitKind, part: &str, writer: Option<Writer>) -> f64 {
    match (kind, part) {
        (HitKind::Note, _) if writer == Some(Writer::User) => 1.25,
        (HitKind::Note, _) => 1.15,
        (HitKind::Session, "card" | "summary") => 1.15,
        (HitKind::Session, "tool") => 0.7,
        (HitKind::Session, _) => 0.9,
        (HitKind::Doc | HitKind::Chat, _) => 1.0,
        (HitKind::Comment, _) => 0.95,
        (HitKind::Commit, "path") => 0.8,
        (HitKind::Commit, _) => 0.85,
        (HitKind::Path, _) => 0.8,
        (HitKind::Code, _) => 0.75,
        (HitKind::Command, "output") => 0.6,
        (HitKind::Command, _) => 0.7,
    }
}

/// How much a hit counts for its age: what happened (a session, a chat, a commit, a command)
/// fades to 0.85 with a half-life of 180 days; what is current by being read again (a note,
/// a document, a comment, code, a path) has no age.
pub fn age_weight(kind: HitKind, at_ms: u64, now_ms: u64) -> f64 {
    match kind {
        HitKind::Session | HitKind::Chat | HitKind::Commit | HitKind::Command => {
            let age = now_ms.saturating_sub(at_ms) as f64;
            (1.0 - AGE_SHARE) + AGE_SHARE * 0.5f64.powf(age / HALF_LIFE_MS)
        }
        _ => 1.0,
    }
}

fn both(a: &str, b: &str) -> String {
    if a == b { a.to_owned() } else { "both".to_owned() }
}

/// Order what the lists found (each hit's `score` is its fused rank score): by kind, by age
/// and by place; then at most [`PER_ITEM`] hits an item, neighbouring chunks of one item as
/// one hit with both snippets; the best `limit`.
pub fn rank(mut hits: Vec<KbHit>, now_ms: u64, here: Option<&str>, limit: usize) -> Vec<KbHit> {
    for h in &mut hits {
        h.score *= kind_weight(h.kind, &h.part, h.writer) * age_weight(h.kind, h.at_ms, now_ms);
        if here.is_some() && h.project.as_deref() == here {
            h.score *= HERE;
        }
    }
    // (A stable sort: ties stay in the fused order.)
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    // A kept hit, and the chunks it covers.
    let mut out: Vec<(KbHit, u32, u32)> = Vec::new();
    for h in hits {
        // (An item's chunks may be of several kinds: a file's path, its comments, its code.)
        let chat = h.kind == HitKind::Chat;
        let of_item =
            |o: &(KbHit, u32, u32)| o.0.item == h.item && (o.0.kind == HitKind::Chat) == chat;
        if let Some(idx) = h.idx
            && let Some((kept, lo, hi)) = out.iter_mut().find(|o| {
                of_item(o) && (idx + 1 == o.1 || idx == o.2 + 1) && o.2 - o.1 < NEIGHBOURS
            })
        {
            if idx < *lo {
                kept.snippet = format!("{} … {}", h.snippet, kept.snippet);
                *lo = idx;
            } else {
                kept.snippet = format!("{} … {}", kept.snippet, h.snippet);
                *hi = idx;
            }
            kept.how = both(&kept.how, &h.how);
            continue;
        }
        if out.len() >= limit || out.iter().filter(|o| of_item(o)).count() >= PER_ITEM {
            continue;
        }
        let at = h.idx.unwrap_or(0);
        out.push((h, at, at));
    }
    out.into_iter().map(|(h, ..)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    fn hit(item: &str, idx: u32, kind: HitKind, part: &str, score: f64) -> KbHit {
        KbHit {
            addr: format!("k:{item}/{idx}"),
            item: item.into(),
            idx: (kind != HitKind::Chat).then_some(idx),
            node: None,
            uri: None,
            kind,
            part: part.into(),
            title: None,
            heading: None,
            snippet: format!("{item}{idx}"),
            at_ms: 1000 * DAY,
            project: None,
            workspace: None,
            profile: None,
            agent: None,
            writer: Some(Writer::Agent),
            machine: None,
            how: "words".into(),
            score,
        }
    }

    fn order(hits: &[KbHit]) -> Vec<&str> {
        hits.iter().map(|h| h.snippet.as_str()).collect()
    }

    #[test]
    fn every_kind_has_its_words_and_its_place() {
        for k in HitKind::ALL {
            assert_eq!(HitKind::parse(k.as_str()), Some(*k));
            if let Some((item, chunks)) = k.stored() {
                // What a kind is stored as reads back as that kind.
                let chunk = chunks.first().copied().unwrap_or(match item {
                    ItemKind::Session => ChunkKind::User,
                    ItemKind::Commit => ChunkKind::Message,
                    ItemKind::Shell => ChunkKind::Command,
                    _ => ChunkKind::Text,
                });
                assert_eq!(HitKind::of(item, chunk), *k);
            }
        }
        assert_eq!(HitKind::Chat.stored(), None);
        assert_eq!(HitKind::of(ItemKind::Commit, ChunkKind::Path), HitKind::Commit);
        assert_eq!(HitKind::of(ItemKind::Shell, ChunkKind::Output), HitKind::Command);
        assert!(HitKind::words().starts_with("session, chat, note, doc"));
    }

    #[test]
    fn ranked_by_kind_age_and_place() {
        // The same fused score: the kind decides (§25.8's table, in its order).
        let w = |k, part, writer| kind_weight(k, part, writer);
        let table = [
            w(HitKind::Note, "text", Some(Writer::User)),
            w(HitKind::Note, "text", Some(Writer::Agent)),
            w(HitKind::Doc, "text", None),
            w(HitKind::Comment, "comment", None),
            w(HitKind::Session, "assistant", None),
            w(HitKind::Commit, "message", None),
            w(HitKind::Path, "path", None),
            w(HitKind::Code, "code", None),
            w(HitKind::Command, "command", None),
            w(HitKind::Command, "output", None),
        ];
        assert!(table.windows(2).all(|p| p[0] > p[1]), "{table:?}");
        assert_eq!(w(HitKind::Session, "card", None), w(HitKind::Session, "summary", None));
        assert_eq!(w(HitKind::Chat, "user", None), 1.0);
        assert!(w(HitKind::Session, "tool", None) < w(HitKind::Session, "user", None));

        // Age: what happened fades to 0.85, by half of the rest in 180 days; a note doesn't.
        let now = 2000 * DAY;
        assert_eq!(age_weight(HitKind::Session, now, now), 1.0);
        assert!((age_weight(HitKind::Chat, now - 180 * DAY, now) - 0.925).abs() < 1e-9);
        assert!(age_weight(HitKind::Commit, 0, now) < 0.851);
        assert_eq!(age_weight(HitKind::Note, 0, now), 1.0);
        assert_eq!(age_weight(HitKind::Session, now + DAY, now), 1.0, "a clock ahead");

        let mut old = hit("old", 0, HitKind::Session, "user", 1.0);
        old.at_ms = 0;
        let mut here = hit("here", 0, HitKind::Session, "user", 0.9);
        here.project = Some("p1".into());
        let hits = vec![
            hit("code", 0, HitKind::Code, "code", 1.0),
            old,
            hit("new", 0, HitKind::Session, "user", 1.0),
            hit("note", 0, HitKind::Note, "text", 1.0),
            here,
        ];
        let ranked = rank(hits.clone(), 1000 * DAY, None, 10);
        assert_eq!(order(&ranked), ["note0", "new0", "here0", "old0", "code0"]);
        // Where you are lifts a hit, and filters nothing.
        let ranked = rank(hits, 1000 * DAY, Some("p1"), 10);
        assert_eq!(order(&ranked), ["note0", "here0", "new0", "old0", "code0"]);
        assert!((ranked[1].score - 0.9 * 0.9 * HERE).abs() < 1e-9);
    }

    #[test]
    fn two_hits_an_item_and_neighbours_as_one() {
        let s = |idx, score| hit("s", idx, HitKind::Session, "user", score);
        // One long session matching everywhere can't fill the page: two hits of it.
        let hits = vec![
            s(10, 0.9),
            s(20, 0.8),
            s(30, 0.7),
            s(40, 0.6),
            hit("d", 0, HitKind::Doc, "text", 0.1),
        ];
        assert_eq!(order(&rank(hits, 0, None, 10)), ["s10", "s20", "d0"]);
        // Neighbouring chunks are one hit, its snippets in the item's order, found both ways
        // when either was; it counts once against the item's two.
        let mut near = s(11, 0.5);
        near.how = "meaning".into();
        let hits = vec![s(10, 0.9), s(20, 0.8), near, s(9, 0.4), s(30, 0.3)];
        let ranked = rank(hits, 0, None, 10);
        assert_eq!(order(&ranked), ["s9 … s10 … s11", "s20"]);
        assert_eq!((ranked[0].how.as_str(), ranked[0].idx), ("both", Some(10)));
        assert_eq!(ranked[1].how, "words");
        // A hit takes in two neighbours and no more: the next one is a hit of its own.
        let hits = vec![s(1, 0.9), s(2, 0.8), s(3, 0.7), s(4, 0.6), s(5, 0.5)];
        assert_eq!(order(&rank(hits, 0, None, 10)), ["s1 … s2 … s3", "s4 … s5"]);
        // Whatever kinds its chunks are of: a file's path, comment and code are one item.
        let f = |idx, kind, part: &str, score| hit("f", idx, kind, part, score);
        let hits = vec![
            f(0, HitKind::Path, "path", 0.9),
            f(4, HitKind::Comment, "comment", 0.8),
            f(8, HitKind::Code, "code", 0.7),
            f(5, HitKind::Code, "code", 0.6),
        ];
        let ranked = rank(hits, 0, None, 10);
        assert_eq!(order(&ranked), ["f4 … f5", "f0"]);
        assert_eq!(ranked[0].kind, HitKind::Comment, "the better chunk's kind");
        // Chats too: two hits a chat; messages have no neighbours to join.
        let c = |n: u32, score| hit("c", n, HitKind::Chat, "user", score);
        assert_eq!(rank(vec![c(1, 0.9), c(2, 0.8), c(3, 0.7)], 0, None, 10).len(), 2);
        // The limit counts hits, after joining.
        let hits = vec![
            s(1, 0.9),
            s(2, 0.8),
            hit("a", 0, HitKind::Doc, "text", 0.7),
            hit("b", 0, HitKind::Doc, "text", 0.6),
        ];
        assert_eq!(order(&rank(hits, 0, None, 2)), ["s1 … s2", "a0"]);
        assert!(rank(Vec::new(), 0, None, 5).is_empty());
    }
}
