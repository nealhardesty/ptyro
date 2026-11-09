//! Session selectors and the state words they share (SPEC §2.3). One list, used by the daemon's
//! `@word` selectors and its "unknown selector" hint, `ptyro list --status`, `ptyro wait --for`
//! and shell completion, so a word means the same thing everywhere (owner, 2026-11-09).

use crate::session::ProcessState;
use crate::status::AgentStatus;

/// A session's state or status, as a selector or a `--status` word names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The process runs (starting or running): not exited, not dormant.
    Live,
    /// Busy now: an agent working or waiting for you, a shell running a command.
    Active,
    /// Ended and not resumable: exited or failed. Never dormant.
    Exited,
    /// Hibernated: not running, but resumes its conversation.
    Dormant,
    /// The status engine's verdict.
    Status(AgentStatus),
}

impl State {
    /// `active` is asked only for [`State::Active`], since it costs more to work out.
    pub fn matches(
        self,
        state: &ProcessState,
        status: Option<AgentStatus>,
        active: impl FnOnce() -> bool,
    ) -> bool {
        match self {
            Self::Live => state.is_live(),
            Self::Active => active(),
            Self::Exited => {
                matches!(state, ProcessState::Exited { .. } | ProcessState::Failed { .. })
            }
            Self::Dormant => *state == ProcessState::Dormant,
            Self::Status(want) => status == Some(want),
        }
    }

    /// A state word or one of its aliases (`errored` is `error`), without the `@`.
    pub fn parse(word: &str) -> Option<Self> {
        SELECTORS.iter().find(|s| s.is(word)).and_then(|s| s.state)
    }
}

/// One `@` selector.
#[derive(Debug, Clone, Copy)]
pub struct Selector {
    /// Without the `@`.
    pub name: &'static str,
    /// Other spellings that mean the same.
    pub aliases: &'static [&'static str],
    /// What it picks, in a few words.
    pub help: &'static str,
    /// `None` for the ones that pick by something else than state (`@all`, `@last`, `@focused`).
    pub state: Option<State>,
}

impl Selector {
    /// `word` (without the `@`) is this one's name or an alias.
    pub fn is(&self, word: &str) -> bool {
        self.name == word || self.aliases.contains(&word)
    }
}

const fn sel(
    name: &'static str,
    aliases: &'static [&'static str],
    help: &'static str,
    state: Option<State>,
) -> Selector {
    Selector { name, aliases, help, state }
}

/// Every selector, in the order help and hints list them.
pub const SELECTORS: &[Selector] = &[
    sel("all", &[], "every session, exited and dormant ones too", None),
    sel("last", &[], "the session started last", None),
    sel("focused", &[], "the session focused in your most recently used TUI", None),
    sel("live", &["running"], "running now", Some(State::Live)),
    sel(
        "active",
        &[],
        "busy now: working, waiting for you, or running a command",
        Some(State::Active),
    ),
    sel("exited", &[], "ended and can't resume (exited or failed)", Some(State::Exited)),
    sel("dormant", &[], "hibernated: resumes its conversation", Some(State::Dormant)),
    sel("working", &[], "an agent working", Some(State::Status(AgentStatus::Working))),
    sel("blocked", &[], "an agent waiting for you", Some(State::Status(AgentStatus::Blocked))),
    sel("idle", &[], "an agent between turns", Some(State::Status(AgentStatus::Idle))),
    sel("done", &[], "an agent that reported it's done", Some(State::Status(AgentStatus::Done))),
    sel(
        "error",
        &["errored"],
        "an agent stopped by an error",
        Some(State::Status(AgentStatus::Errored)),
    ),
];

/// The selector `word` names (with or without its `@`), by name or alias.
pub fn find(word: &str) -> Option<&'static Selector> {
    let word = word.strip_prefix('@').unwrap_or(word);
    SELECTORS.iter().find(|s| s.is(word))
}

/// `@all, @last, …`: every selector, for hints.
pub fn list() -> String {
    SELECTORS.iter().map(|s| format!("@{}", s.name)).collect::<Vec<_>>().join(", ")
}

/// `live, active, …`: the state words `ptyro list --status` takes.
pub fn state_words() -> String {
    SELECTORS.iter().filter(|s| s.state.is_some()).map(|s| s.name).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_name_the_same_state() {
        assert_eq!(State::parse("errored"), State::parse("error"));
        assert_eq!(State::parse("error"), Some(State::Status(AgentStatus::Errored)));
        assert_eq!(State::parse("running"), Some(State::Live));
        assert_eq!(find("@errored").map(|s| s.name), Some("error"));
        assert_eq!(State::parse("all"), None, "not a state");
        assert_eq!(State::parse("nonsense"), None);
    }

    #[test]
    fn exited_never_means_dormant() {
        let exited = State::parse("exited").unwrap();
        let dormant = State::parse("dormant").unwrap();
        let live = State::parse("live").unwrap();
        let gone = ProcessState::Exited { code: Some(0), signal: None };
        let failed = ProcessState::Failed { error: "no such file".into() };
        assert!(exited.matches(&gone, None, || false));
        assert!(exited.matches(&failed, None, || false));
        assert!(!exited.matches(&ProcessState::Dormant, None, || false));
        assert!(dormant.matches(&ProcessState::Dormant, None, || false));
        assert!(!dormant.matches(&gone, None, || false));
        assert!(!live.matches(&ProcessState::Dormant, None, || false));
        assert!(live.matches(&ProcessState::Running, None, || false));
    }

    #[test]
    fn names_and_aliases_are_unique_and_the_hint_lists_them_all() {
        let mut seen = std::collections::HashSet::new();
        for s in SELECTORS {
            for w in std::iter::once(&s.name).chain(s.aliases) {
                assert!(seen.insert(*w), "{w} twice");
            }
        }
        let hint = list();
        for w in ["@all", "@dormant", "@error", "@exited", "@live", "@focused", "@last"] {
            assert!(hint.contains(w), "{hint}");
        }
    }

    /// The status words name the status engine's own labels.
    #[test]
    fn status_words_are_the_engines_labels() {
        for s in SELECTORS {
            if let Some(State::Status(st)) = s.state {
                assert_eq!(st.label(), s.name);
            }
        }
    }

    /// docs/commands.md lists every selector, and only these.
    #[test]
    fn the_docs_list_the_same_selectors() {
        let doc = include_str!("../../../docs/commands.md");
        let line = doc
            .lines()
            .skip_while(|l| !l.contains("**`SESSIONS…`**"))
            .take_while(|l| !l.trim_start().starts_with("- **`W`**"))
            .collect::<Vec<_>>()
            .join(" ");
        // Every `@word` in code spans (the odd pieces between backticks).
        let mut named: Vec<&str> = line
            .split('`')
            .skip(1)
            .step_by(2)
            .filter_map(|w| w.strip_prefix('@'))
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()))
            .collect();
        named.sort_unstable();
        named.dedup();
        let mut ours: Vec<&str> = SELECTORS
            .iter()
            .flat_map(|s| std::iter::once(s.name).chain(s.aliases.iter().copied()))
            .collect();
        ours.sort_unstable();
        assert_eq!(named, ours, "docs/commands.md's SESSIONS… item drifted from SELECTORS");
    }
}
