//! A session's card (SPEC §25.6.1): the first chunk of every session, made again at each
//! ingest from what is known, with no model. What it was asked, how it ended, the files it
//! changed, and who and where: a finished agent's last message is usually its own summary,
//! which is why this reads well without a summariser.

use super::ChunkKind;
use super::chunk::Part;

/// The card's anchor: sent again with every piece of its session, it stands in for itself.
pub const ANCHOR: &str = "card";
/// The most of the first prompt a card keeps.
pub const TASK: usize = 500;
/// The most of the last answer.
pub const OUTCOME: usize = 1000;
/// The most files a card (and `item.files`) names.
pub const FILES: usize = 30;

/// What a card says. Names here are for reading (a project's slug, a worktree's name), not ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    /// The first prompt.
    pub task: Option<String>,
    /// The last assistant text.
    pub outcome: Option<String>,
    /// The files it changed, the first [`FILES`] of them.
    pub files: Vec<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub project: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
    pub turns: u32,
}

/// `text` on one line (the card says each thing on a line of its own, and the message itself
/// is a chunk beside it), at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    text.chars().take(max - 1).chain(['…']).collect::<String>()
}

const TASK_LINE: &str = "Task: ";
const OUTCOME_LINE: &str = "Outcome: ";

/// The task and the outcome that a card's text says: what a later card of a session with no
/// transcript keeps, when the daemon no longer knows them (it forgets at a restart).
pub fn said(text: &str) -> (Option<String>, Option<String>) {
    let line = |label: &str| text.lines().find_map(|l| l.strip_prefix(label)).map(str::to_owned);
    (line(TASK_LINE), line(OUTCOME_LINE))
}

/// `2026-11-19 22:38` (UTC).
fn minute(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // The civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rest / 3600, rest % 3600 / 60)
}

impl Card {
    /// Whether there is anything to say of the session yet.
    pub fn is_empty(&self) -> bool {
        self.task.is_none() && self.outcome.is_none() && self.files.is_empty()
    }

    /// The card as text: a line (or a paragraph) for each thing known, nothing for the rest.
    pub fn text(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        if let Some(t) = self.task.as_deref().filter(|t| !t.trim().is_empty()) {
            out.push(format!("{TASK_LINE}{}", clip(t, TASK)));
        }
        if let Some(o) = self.outcome.as_deref().filter(|o| !o.trim().is_empty()) {
            out.push(format!("{OUTCOME_LINE}{}", clip(o, OUTCOME)));
        }
        if !self.files.is_empty() {
            let files: Vec<&str> = self.files.iter().take(FILES).map(String::as_str).collect();
            out.push(format!("Files changed: {}", files.join(", ")));
        }
        let known = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(agent) = known(&self.agent) {
            out.push(match known(&self.model) {
                Some(model) => format!("Agent: {agent} ({model})"),
                None => format!("Agent: {agent}"),
            });
        }
        let place: Vec<String> = [
            known(&self.project).map(|p| format!("Project: {p}")),
            known(&self.worktree).map(|w| format!("worktree {w}")),
            known(&self.branch).map(|b| format!("branch {b}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !place.is_empty() {
            out.push(place.join(", "));
        }
        let turns = match self.turns {
            1 => "1 turn".to_owned(),
            n => format!("{n} turns"),
        };
        out.push(match (self.started_ms, self.ended_ms) {
            (Some(a), Some(b)) => {
                format!("Started {} UTC, ended {}, {turns}", minute(a), minute(b))
            }
            (Some(a), None) | (None, Some(a)) => format!("Started {} UTC, {turns}", minute(a)),
            (None, None) => turns,
        });
        out.join("\n")
    }

    /// The card as its session's part: kind `card`, anchor [`ANCHOR`].
    pub fn part(&self) -> Part {
        let at = self.ended_ms.or(self.started_ms).unwrap_or(0);
        Part::new(ChunkKind::Card, self.text(), at).anchor(ANCHOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_says_what_is_known_and_nothing_else() {
        let mut c = Card {
            task: Some(format!("  fix the login\nand its test {}", "x".repeat(600))),
            outcome: Some("Fixed: the cookie was read before it was set.".into()),
            files: (0..40).map(|i| format!("src/f{i}.rs")).collect(),
            agent: Some("claude".into()),
            model: Some("claude-opus-5-5".into()),
            project: Some("shop".into()),
            worktree: Some("fix-login".into()),
            branch: Some("ptyro/fix-login".into()),
            // 2026-11-19T22:38:48Z and a minute on.
            started_ms: Some(1_795_127_928_772),
            ended_ms: Some(1_795_127_990_000),
            turns: 3,
        };
        let text = c.text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("Task: fix the login and its test xxx"), "one line");
        assert_eq!(lines[0].chars().count(), "Task: ".len() + TASK);
        assert_eq!(
            said(&text),
            (
                Some(lines[0][6..].to_owned()),
                Some("Fixed: the cookie was read before it was set.".into())
            )
        );
        assert_eq!(said("Agent: x\n1 turn"), (None, None));
        assert!(text.contains("\nOutcome: Fixed: the cookie was read before it was set.\n"));
        let files = lines.iter().find(|l| l.starts_with("Files changed: ")).unwrap();
        assert_eq!(files.matches(", ").count(), FILES - 1);
        assert!(files.ends_with("src/f29.rs"));
        assert_eq!(
            lines[lines.len() - 3..],
            [
                "Agent: claude (claude-opus-5-5)",
                "Project: shop, worktree fix-login, branch ptyro/fix-login",
                "Started 2026-11-19 22:38 UTC, ended 2026-11-19 22:39, 3 turns",
            ]
        );
        let part = c.part();
        assert_eq!((part.kind, part.anchor.as_deref()), (ChunkKind::Card, Some(ANCHOR)));
        assert_eq!(part.at_ms, 1_795_127_990_000);

        c = Card { task: Some("hello".into()), turns: 1, ..Card::default() };
        assert_eq!(c.text(), "Task: hello\n1 turn");
        assert!(!c.is_empty() && Card::default().is_empty());
        assert_eq!(minute(0), "1970-01-01 00:00");
        assert_eq!(minute(951_782_400_000), "2000-02-29 00:00");
    }
}
