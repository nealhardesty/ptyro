//! Home (SPEC §26): one look at everything. What needs you, what was left hanging, what is being
//! worked on, and what is quiet: a row per project, ranked by the next thing to do there.
//!
//! This module is the one rule. The daemon gathers what it knows ([`Inputs`]: every session,
//! project and worktree, each profile's usage, and what the knowledge base remembers of each
//! project) and [`build`] ranks it; the CLI, the TUI and the web only draw the [`HomeView`].
//! Nothing here asks a model: every line is something that was typed, answered or committed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{ProjectInfo, WorkspaceInfo, WorktreeInfo, WorktreeState};
use crate::session::{ProcessState, SessionInfo, SessionKind, brief};
use crate::status::{AgentStatus, AttentionKind, Usage};

/// A seen, idle agent whose last reply was a question is left hanging after this long.
pub const ASKED_AFTER_MS: u64 = 10 * 60_000;
/// A seen, idle agent is left hanging after this long, whatever it said last.
pub const STALE_AFTER_MS: u64 = 2 * 3_600_000;
/// What only the knowledge base remembers stops being "left hanging" after this long: older
/// than this it is where the project was left off.
pub const MEMORY_HANGS_MS: u64 = 30 * 86_400_000;
/// A usage reading older than this is shown as old.
pub const USAGE_OLD_MS: u64 = 6 * 3_600_000;
/// A usage window at or over this is worth a colour.
pub const USAGE_WARN: u8 = 80;
/// How long the "while you were away" line stays after you come back.
pub const AWAY_SHOWN_MS: u64 = 30 * 60_000;
/// No sign of you for this long is being away.
pub const AWAY_AFTER_MS: u64 = 20 * 60_000;
/// The longest text of a thing.
const TEXT: usize = 160;

/// Where a row stands. The order is the order on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// Someone waits for you, or something finished or failed that you haven't looked at.
    Needs,
    /// Started and not finished, with nothing running: a question nobody answered, a prompt
    /// with no reply, work that isn't committed or pushed, an agent idle for hours.
    Hanging,
    /// Being worked on now.
    Working,
    /// Nothing open.
    Quiet,
}

impl Section {
    pub const ALL: [Self; 4] = [Self::Needs, Self::Hanging, Self::Working, Self::Quiet];

    pub fn title(self) -> &'static str {
        match self {
            Self::Needs => "NEEDS YOU",
            Self::Hanging => "LEFT HANGING",
            Self::Working => "WORKING",
            Self::Quiet => "QUIET",
        }
    }
}

/// The next thing to do somewhere, most urgent first: the ladder every row is ranked by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Next {
    /// An agent is blocked on you: a permission, a question.
    Answer,
    /// Something stopped with an error.
    Failed,
    /// A turn finished and you haven't looked.
    Review,
    /// A shell you asked to be told about has something.
    Look,
    /// Its last reply was a question, and nobody answered.
    Asked,
    /// Your last prompt got no reply.
    CutOff,
    /// A note tagged `next`: what you (or an agent) said comes next here.
    Planned,
    /// Uncommitted work with nothing running in it.
    Commit,
    /// Commits that are nowhere but here, with nothing running.
    Push,
    /// An agent you have seen, idle for hours.
    Stale,
    /// An agent at work.
    Working,
    /// Stopped by a usage limit: it goes on by itself, or when the window resets.
    Limit,
    /// An agent you have seen, waiting for your next prompt.
    Open,
    /// Where the project was left off, with nothing open.
    LeftOff,
    /// Nothing is known of it.
    Nothing,
}

impl Next {
    pub fn section(self) -> Section {
        match self {
            Self::Answer | Self::Failed | Self::Review | Self::Look => Section::Needs,
            Self::Asked
            | Self::CutOff
            | Self::Planned
            | Self::Commit
            | Self::Push
            | Self::Stale => Section::Hanging,
            Self::Working | Self::Limit | Self::Open => Section::Working,
            Self::LeftOff | Self::Nothing => Section::Quiet,
        }
    }

    /// The word a row leads with.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Failed => "failed",
            Self::Review => "review",
            Self::Look => "look",
            Self::Asked => "it asked",
            Self::CutOff => "cut off",
            Self::Planned => "next",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::Stale => "idle",
            Self::Working => "working",
            Self::Limit => "limit",
            Self::Open => "your turn",
            Self::LeftOff => "left off",
            Self::Nothing => "",
        }
    }

    /// Whether `x` can put it away until something changes: only what was left hanging.
    pub fn dismissable(self) -> bool {
        self.section() == Section::Hanging
    }
}

/// One thing somewhere: what to do, why, since when, and where ⏎ goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thing {
    pub next: Next,
    /// Why it is here, in the words that were said: the question, the reply, the counts.
    pub text: String,
    /// Since when.
    pub at_ms: u64,
    /// The session it is about (its id), when one still exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// The worktree it is in (its id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
    /// The knowledge base's item behind it (`k:7f3k`), when that is all that is left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// What is uncommitted and unpushed there, as every surface words it (`↑3 · ● 2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<String>,
    /// What names it for [`dismiss`](Inputs::dismissed), and what it looked like then: put
    /// away, it stays away until the stamp changes.
    pub key: String,
    pub stamp: String,
}

impl Thing {
    /// Where it is, as a row's last column says: the session's name, else the worktree's.
    pub fn place(&self) -> Option<&str> {
        self.session_name.as_deref().or(self.worktree_name.as_deref())
    }
}

/// A project's row: its things, the most urgent first. The first is its next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeRow {
    /// The project's id; `~` (with the machine's name, when another's) for the sessions of
    /// no project.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub section: Section,
    /// Never empty.
    pub things: Vec<Thing>,
    /// Its live agents, and how many of them are at work.
    pub live: u32,
    pub working: u32,
    /// When anything last happened in it.
    pub last_ms: u64,
    /// Prompts a day over the last seven days, oldest first, from the knowledge base; empty
    /// when it remembers nothing of the project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub week: Vec<u16>,
}

impl HomeRow {
    pub fn next(&self) -> &Thing {
        &self.things[0]
    }
}

/// How loud a usage meter is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Quiet,
    /// Nothing is known: its feed is off, or it hasn't reported.
    Unknown,
    /// The reading is old.
    Old,
    Warn,
    Limited,
}

/// One part of a meter: a window (`5h 62% ↻2h`), or `limited ↻40m`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterPart {
    pub text: String,
    pub level: Level,
}

/// One profile's usage of one agent, as the strip shows it: every window it reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meter {
    /// `work/claude`, `laptop2:work/codex`; the profile is left out when there is only one.
    pub label: String,
    pub profile: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub parts: Vec<MeterPart>,
    /// Its loudest part.
    pub level: Level,
    /// Rate-limited until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_until_ms: Option<u64>,
}

impl Meter {
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self.parts.iter().map(|p| p.text.as_str()).collect();
        format!("{} {}", self.label, parts.join(" · "))
    }
}

/// What happened while nobody was looking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Away {
    /// The last sign of you before you came back, and when you did.
    pub since_ms: u64,
    pub back_ms: u64,
    /// Turns that finished, things that failed, and agents that came to wait for an answer.
    pub finished: u32,
    pub failed: u32,
    pub asked: u32,
    /// Commits made (from the knowledge base; 0 without it).
    pub commits: u32,
}

impl Away {
    /// `away 2h10m: 3 finished, 1 failed, 2 commits`, or `away 2h10m: nothing happened`.
    pub fn line(&self) -> String {
        let mut v = Vec::new();
        let mut say = |n: u32, one: &str, many: &str| {
            if n > 0 {
                v.push(format!("{n} {}", if n == 1 { one } else { many }));
            }
        };
        say(self.finished, "finished", "finished");
        say(self.failed, "failed", "failed");
        say(self.asked, "asks you", "ask you");
        say(self.commits, "commit", "commits");
        let what = if v.is_empty() { "nothing happened".to_owned() } else { v.join(", ") };
        format!("away {}: {what}", span(self.back_ms.saturating_sub(self.since_ms)))
    }
}

/// Whether the knowledge base stands behind the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Memory {
    On,
    /// Switched off (`kb.enabled`): live state only.
    #[default]
    Off,
    /// The one asking is an agent: it gets live state only (SPEC §25.9).
    Withheld,
}

/// One line of a project's trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrailEntry {
    /// `session`, `commit`, `note`, `shell`.
    pub kind: String,
    pub title: String,
    /// `claude · 14 turns · 6 files`, a commit's short hash, who wrote a note.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub at_ms: u64,
    /// Its address in the knowledge base (`k:7f3k`).
    pub addr: String,
    /// The ptyro session behind it, when it is still listed (its id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// Where a project was left off: the last thing you said there and the last thing said back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftOff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub you: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// A file and how many of the trail's sessions and commits touched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hot {
    pub path: String,
    pub count: u32,
}

/// A project unfolded: where it was left off, what happened lately, and where the work is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trail {
    /// The row's key.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_off: Option<LeftOff>,
    /// Newest first.
    #[serde(default)]
    pub entries: Vec<TrailEntry>,
    #[serde(default)]
    pub hot: Vec<Hot>,
}

/// Everything, ranked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeView {
    pub at_ms: u64,
    /// By section, then by what is next, then as each section orders its own.
    pub rows: Vec<HomeRow>,
    pub meters: Vec<Meter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away: Option<Away>,
    #[serde(default)]
    pub memory: Memory,
    /// The trail of the row that was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<Trail>,
    /// `[ui] home_at_start`: a client that opens with nowhere to go opens here (the web app
    /// asks; the TUI reads its own config).
    #[serde(default)]
    pub at_start: bool,
}

/// How many things stand in each section, and how many agents are at work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub needs: u32,
    pub hanging: u32,
    pub working: u32,
    pub quiet: u32,
}

impl HomeView {
    /// Things that need you and things left hanging (every one, not a row each), agents at
    /// work, and quiet projects.
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.rows {
            c.working += r.working;
            if r.section == Section::Quiet {
                c.quiet += 1;
            }
            for t in &r.things {
                match t.next.section() {
                    Section::Needs => c.needs += 1,
                    Section::Hanging => c.hanging += 1,
                    _ => {}
                }
            }
        }
        c
    }

    /// `2 need you · 1 left hanging · 3 working`, or `all quiet`.
    pub fn headline(&self) -> String {
        let c = self.counts();
        let mut v = Vec::new();
        if c.needs > 0 {
            v.push(format!("{} need{} you", c.needs, if c.needs == 1 { "s" } else { "" }));
        }
        if c.hanging > 0 {
            v.push(format!("{} left hanging", c.hanging));
        }
        if c.working > 0 {
            v.push(format!("{} working", c.working));
        }
        if v.is_empty() { "all quiet".into() } else { v.join(" · ") }
    }

    pub fn row(&self, key: &str) -> Option<&HomeRow> {
        self.rows.iter().find(|r| r.key == key)
    }

    /// The row a word names: its key, else its name (`laptop2:myrepo` for another machine's),
    /// exactly, else the only one starting with it.
    pub fn find(&self, word: &str) -> Option<&HomeRow> {
        let named = |r: &&HomeRow| {
            r.name == word || r.machine.as_ref().is_some_and(|m| format!("{m}:{}", r.name) == word)
        };
        self.row(word).or_else(|| self.rows.iter().find(named)).or_else(|| {
            let mut starts = self.rows.iter().filter(|r| r.name.starts_with(word));
            let first = starts.next()?;
            starts.next().is_none().then_some(first)
        })
    }
}

/// A session, and the question its last reply ends with (the daemon has the whole reply; a
/// [`SessionInfo`] only its start).
#[derive(Debug, Clone)]
pub struct Seen {
    pub info: SessionInfo,
    pub asks: Option<String>,
}

/// Which of the two spoke last in a remembered session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    You,
    Reply,
}

/// A note tagged `next` in a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub addr: String,
    pub text: String,
    pub at_ms: u64,
}

/// What the knowledge base remembers of a project: its newest session's end, and its week.
#[derive(Debug, Clone, Default)]
pub struct Tail {
    /// The project's id.
    pub project: String,
    /// The newest session kept of it: its address, the ptyro session it was, when it ended.
    pub addr: Option<String>,
    pub session: Option<String>,
    pub agent: Option<String>,
    pub at_ms: u64,
    /// The start of the last thing you said, and of the last reply.
    pub you: Option<String>,
    pub reply: Option<String>,
    pub last: Option<Last>,
    /// The question the last reply ends with.
    pub asks: Option<String>,
    pub week: Vec<u16>,
    pub planned: Option<Planned>,
}

/// Everything [`build`] ranks.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub now_ms: u64,
    pub sessions: Vec<Seen>,
    pub projects: Vec<ProjectInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub usage: Vec<Usage>,
    pub tails: Vec<Tail>,
    /// What was put away: a thing's key → its stamp then.
    pub dismissed: BTreeMap<String, String>,
    pub away: Option<Away>,
    pub memory: Memory,
}

/// At most `max` characters of `text` on one line, without control characters: what was said
/// came from an agent or a file, and goes to a terminal.
pub fn clean(text: &str, max: usize) -> String {
    let plain: String = text.chars().filter(|c| c.is_whitespace() || !c.is_control()).collect();
    brief(&plain, max)
}

/// The question a text ends with: its last line, when that ends in `?`, from where the
/// sentence starts. Markdown's closing marks don't hide it.
pub fn question(text: &str) -> Option<String> {
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let bare = line.trim_end_matches(['*', '_', '`', '"', '\'', ')', ' ', '”', '’']);
    if !bare.ends_with('?') {
        return None;
    }
    let body = &bare[..bare.len() - 1];
    let start = [". ", "! ", "? ", ": "]
        .iter()
        .filter_map(|sep| body.rfind(sep).map(|i| i + sep.len()))
        .max()
        .unwrap_or(0);
    let q = bare[start..].trim_start_matches(['*', '_', '`', '"', '\'', '(', ' ', '-', '>', '#']);
    // A sentence too short to mean anything by itself ("Ready?") keeps the line.
    let q = if q.chars().count() < 12 { bare } else { q };
    Some(clean(q, TEXT))
}

/// A duration as a headline says it: `45m`, `2h10m`, `3d`.
pub fn span(ms: u64) -> String {
    let m = ms / 60_000;
    match m {
        0..60 => format!("{m}m"),
        60..1440 if m.is_multiple_of(60) => format!("{}h", m / 60),
        60..1440 => format!("{}h{:02}m", m / 60, m % 60),
        _ => format!("{}d", m / 1440),
    }
}

/// A window's length from its name (`5h`, `7d`, `300m`).
fn window_ms(name: &str) -> Option<u64> {
    let (n, unit) = name.split_at(name.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    Some(match unit {
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => return None,
    })
}

/// When a window runs out at the pace it has been used, if that is before it resets: only
/// said once it is half used, and at least five minutes early.
fn full_at(name: &str, used: u8, resets_at_ms: u64, now: u64) -> Option<u64> {
    if !(50..100).contains(&used) || resets_at_ms <= now {
        return None;
    }
    let len = window_ms(name)?;
    let start = resets_at_ms.checked_sub(len)?;
    let elapsed = now.checked_sub(start).filter(|e| *e > 0)?;
    let full = start + elapsed * 100 / u64::from(used);
    (full + 5 * 60_000 < resets_at_ms).then_some(full)
}

/// The usage strip: every profile's every agent with every window it reports, and a `?` for
/// an agent that runs here and reports nothing. Complete: nothing is left out for room.
pub fn meters(usage: &[Usage], sessions: &[Seen], now: u64) -> Vec<Meter> {
    let mut all: Vec<Usage> = usage.to_vec();
    for s in sessions {
        let i = &s.info;
        let Some(agent) = i.agent.as_deref().filter(|_| i.state.is_live()) else { continue };
        if !all.iter().any(|u| u.profile == i.profile && u.agent == agent && u.machine == i.machine)
        {
            all.push(Usage {
                profile: i.profile.clone(),
                machine: i.machine.clone(),
                agent: agent.to_owned(),
                ..Usage::default()
            });
        }
    }
    all.sort_by(|a, b| {
        (a.machine.is_some(), &a.machine, &a.profile, &a.agent).cmp(&(
            b.machine.is_some(),
            &b.machine,
            &b.profile,
            &b.agent,
        ))
    });
    let one_profile = all.iter().all(|u| u.profile == all[0].profile && u.machine.is_none());
    all.iter()
        .map(|u| {
            let mut parts = Vec::new();
            let limited = u.limited_until_ms.filter(|t| *t > now);
            if let Some(t) = limited {
                parts.push(MeterPart {
                    text: format!("limited ↻{}", span(t - now)),
                    level: Level::Limited,
                });
            }
            let old = u.at_ms > 0 && now.saturating_sub(u.at_ms) > USAGE_OLD_MS;
            for w in &u.windows {
                // A window that has reset since the reading says nothing about now.
                if w.resets_at_ms.is_some_and(|t| t <= now) {
                    parts
                        .push(MeterPart { text: format!("{} reset", w.name), level: Level::Quiet });
                    continue;
                }
                let mut text = format!("{} {}%", w.name, w.used_percent);
                if let Some(t) = w.resets_at_ms {
                    text.push_str(&format!(" ↻{}", span(t - now)));
                    if let Some(full) = full_at(&w.name, w.used_percent, t, now) {
                        text.push_str(&format!(" full ~{}", span(full.saturating_sub(now))));
                    }
                }
                let level = match w.used_percent {
                    100.. => Level::Limited,
                    USAGE_WARN.. => Level::Warn,
                    _ if old => Level::Old,
                    _ => Level::Quiet,
                };
                parts.push(MeterPart { text, level });
            }
            if parts.is_empty() {
                parts.push(MeterPart { text: "?".into(), level: Level::Unknown });
            } else if old {
                parts.push(MeterPart {
                    text: format!("({} old)", crate::dur(now - u.at_ms)),
                    level: Level::Old,
                });
            }
            let who =
                if one_profile { u.agent.clone() } else { format!("{}/{}", u.profile, u.agent) };
            Meter {
                label: match &u.machine {
                    Some(m) if !who.starts_with(&format!("{m}:")) => format!("{m}:{who}"),
                    _ => who,
                },
                profile: u.profile.clone(),
                agent: u.agent.clone(),
                machine: u.machine.clone(),
                level: parts.iter().map(|p| p.level).max().unwrap_or(Level::Quiet),
                parts,
                limited_until_ms: limited,
            }
        })
        .collect()
}

/// What a live or listed session is, as a thing. `None`: nothing to say of it (a plain shell,
/// an ended session you have seen).
fn of_session(seen: &Seen, wt: Option<&WorktreeInfo>, usage: &[Usage], now: u64) -> Option<Thing> {
    let s = &seen.info;
    // A chat's own agent and a worktree's quick terminal are not work of their own.
    if s.chat.is_some() || s.quick {
        return None;
    }
    let live = s.state.is_live();
    let agent = s.kind == SessionKind::Agent;
    let status = s.status.as_ref();
    let said = || {
        s.last_reply
            .clone()
            .or_else(|| s.task.clone())
            .or_else(|| s.title.clone())
            .unwrap_or_default()
    };
    let limited = |kind: Option<&str>| {
        kind == Some("rate_limit")
            || usage.iter().any(|u| {
                u.profile == s.profile
                    && u.machine == s.machine
                    && s.agent.as_deref() == Some(u.agent.as_str())
                    && u.limited_until_ms.is_some_and(|t| t > now)
            }) && kind.is_none_or(|k| k == "rate_limit" || k == "overloaded")
    };
    let resumes = || {
        usage
            .iter()
            .find(|u| {
                u.profile == s.profile
                    && u.machine == s.machine
                    && s.agent.as_deref() == Some(u.agent.as_str())
            })
            .and_then(|u| u.limited_until_ms)
            .filter(|t| *t > now)
            .map(|t| format!("waiting for the usage limit · resets in {}", span(t - now)))
            .unwrap_or_else(|| "stopped by a usage limit".into())
    };
    let since = status.map(|r| r.since_ms).unwrap_or(s.created_ms);
    let (next, text, at_ms) = if let Some(a) = &s.attention {
        match a.kind {
            AttentionKind::Blocked => (Next::Answer, a.text.clone(), a.since_ms),
            AttentionKind::Errored if limited(status.and_then(|r| r.kind.as_deref())) => {
                (Next::Limit, resumes(), a.since_ms)
            }
            AttentionKind::Errored => (Next::Failed, a.text.clone(), a.since_ms),
            AttentionKind::Exited => match &s.state {
                ProcessState::Exited { code: Some(0), .. } => {
                    (Next::Review, format!("exited: {}", said()), a.since_ms)
                }
                _ => (Next::Failed, a.text.clone(), a.since_ms),
            },
            AttentionKind::Done | AttentionKind::Idle if agent => {
                (Next::Review, format!("finished: {}", said()), a.since_ms)
            }
            AttentionKind::Done | AttentionKind::Idle | AttentionKind::Notification => {
                (Next::Look, a.text.clone(), a.since_ms)
            }
        }
    } else if live && agent {
        match status.map(|r| r.status) {
            Some(AgentStatus::Blocked) => (
                Next::Answer,
                status.and_then(|r| r.detail.clone()).unwrap_or_else(|| "waits for you".into()),
                since,
            ),
            Some(AgentStatus::Errored) if limited(status.and_then(|r| r.kind.as_deref())) => {
                (Next::Limit, resumes(), since)
            }
            Some(AgentStatus::Errored) => (
                Next::Failed,
                status
                    .and_then(|r| r.detail.clone().or_else(|| r.kind.clone()))
                    .unwrap_or_else(|| "stopped by an error".into()),
                since,
            ),
            Some(AgentStatus::Working | AgentStatus::Starting) => {
                let doing = s
                    .activity
                    .clone()
                    .or_else(|| status.and_then(|r| r.detail.clone()))
                    .or_else(|| s.task.clone())
                    .unwrap_or_else(|| "working".into());
                let turn =
                    if s.turns > 0 { format!(" (turn {})", s.turns + 1) } else { String::new() };
                (Next::Working, format!("{doing}{turn}"), since)
            }
            _ => {
                let idle = now.saturating_sub(since);
                match &seen.asks {
                    Some(q) if idle >= ASKED_AFTER_MS => (Next::Asked, q.clone(), since),
                    _ if idle >= STALE_AFTER_MS => (Next::Stale, said(), since),
                    _ => (Next::Open, said(), since),
                }
            }
        }
    } else if s.state == ProcessState::Dormant && agent {
        // Asleep, and it can go on: hanging when it asked, else where things were left.
        let at = s.ended_ms.unwrap_or(since);
        match &seen.asks {
            Some(q) => (Next::Asked, q.clone(), at),
            None => (Next::LeftOff, said(), at),
        }
    } else {
        return None;
    };
    Some(Thing {
        next,
        text: clean(&text, TEXT),
        at_ms,
        session: Some(s.id.full()),
        session_name: Some(s.name.clone()),
        worktree: s.worktree.clone(),
        worktree_name: wt.filter(|w| !w.is_root).map(|w| w.name.clone()),
        item: None,
        left: wt
            .and_then(|w| w.status.as_ref())
            .map(|r| r.left())
            .filter(|l| l.open())
            .map(|l| l.words()),
        key: format!("{}:{}", verb_key(next), s.id.full()),
        stamp: at_ms.to_string(),
    })
}

fn verb_key(n: Next) -> &'static str {
    match n {
        Next::Asked => "asked",
        Next::CutOff => "cutoff",
        Next::Planned => "planned",
        Next::Commit => "commit",
        Next::Push => "push",
        Next::Stale => "stale",
        _ => "thing",
    }
}

/// Work in a worktree that nothing is running in: uncommitted, or committed and nowhere else.
fn of_worktree(w: &WorktreeInfo) -> Option<Thing> {
    if !matches!(w.state, WorktreeState::Ready) {
        return None;
    }
    let st = w.status.as_ref()?;
    let left = st.left();
    let (next, text) = if left.uncommitted > 0 {
        let lines = if st.insertions + st.deletions > 0 {
            format!(" (+{} −{})", st.insertions, st.deletions)
        } else {
            String::new()
        };
        let n = left.uncommitted;
        (
            Next::Commit,
            format!(
                "{n} file{} not committed{lines}, nothing running",
                if n == 1 { "" } else { "s" }
            ),
        )
    } else if left.unpushed > 0 {
        let n = left.unpushed;
        let subject = st.subject.as_deref().map(|s| format!(": {s}")).unwrap_or_default();
        (
            Next::Push,
            format!(
                "{n} commit{} {}{subject}",
                if n == 1 { "" } else { "s" },
                if left.not_pushed { "not pushed anywhere" } else { "to push" }
            ),
        )
    } else {
        return None;
    };
    Some(Thing {
        next,
        text: clean(&text, TEXT),
        at_ms: w.last_used_ms.unwrap_or(w.created_ms),
        session: None,
        session_name: None,
        worktree: Some(w.id.clone()),
        worktree_name: Some(if w.is_root {
            st.branch.clone().unwrap_or_else(|| "main checkout".into())
        } else {
            w.name.clone()
        }),
        item: None,
        left: Some(left.words()),
        key: format!("{}:{}", verb_key(next), w.id),
        // Put away, it comes back when the work there changes.
        stamp: format!(
            "{}:{}:{}",
            left.uncommitted,
            left.unpushed,
            st.head.as_deref().unwrap_or("")
        ),
    })
}

/// What only the knowledge base knows of a project: a question nobody answered, a prompt with
/// no reply, a note that says what is next, or just where it was left off.
fn of_tail(t: &Tail, listed: bool, now: u64) -> Vec<Thing> {
    let mut out = Vec::new();
    let thing = |next: Next, text: &str, at_ms: u64, item: Option<String>, id: &str| Thing {
        next,
        text: clean(text, TEXT),
        at_ms,
        session: None,
        session_name: None,
        worktree: None,
        worktree_name: None,
        item,
        left: None,
        key: format!("{}:{id}", verb_key(next)),
        stamp: at_ms.to_string(),
    };
    if let Some(p) = &t.planned {
        out.push(thing(Next::Planned, &p.text, p.at_ms, Some(p.addr.clone()), &p.addr));
    }
    // A session that is still listed speaks for itself (`of_session`).
    if let Some(addr) = t.addr.as_ref().filter(|_| !listed) {
        let fresh = now.saturating_sub(t.at_ms) < MEMORY_HANGS_MS;
        let said = t.you.as_deref().or(t.reply.as_deref()).unwrap_or("");
        let (next, text) = match (t.last, &t.asks) {
            (Some(Last::Reply), Some(q)) if fresh => (Next::Asked, q.clone()),
            (Some(Last::You), _) if fresh => (Next::CutOff, format!("you: {said} (no reply)")),
            _ => (Next::LeftOff, said.to_owned()),
        };
        if !text.is_empty() {
            out.push(thing(next, &text, t.at_ms, Some(addr.clone()), addr));
        }
    }
    out
}

/// Rank everything: a row per project (and one for the sessions of none), each with its things
/// in the ladder's order, the rows by section and then by what is next.
pub fn build(inp: &Inputs) -> HomeView {
    let now = inp.now_ms;
    let wt_of = |id: &str| inp.worktrees.iter().find(|w| w.id == id);
    let mut rows: BTreeMap<String, HomeRow> = BTreeMap::new();
    for p in &inp.projects {
        let workspace =
            inp.workspaces.iter().find(|w| w.projects.contains(&p.id)).map(|w| w.name.clone());
        rows.insert(
            p.id.clone(),
            HomeRow {
                key: p.id.clone(),
                project: Some(p.id.clone()),
                name: p.slug.clone(),
                workspace,
                machine: p.machine.clone(),
                section: Section::Quiet,
                things: Vec::new(),
                live: 0,
                working: 0,
                last_ms: p.last_used_ms.unwrap_or(p.created_ms),
                week: Vec::new(),
            },
        );
    }
    // Sessions: each is a thing, or nothing.
    let mut busy: Vec<&str> = Vec::new(); // worktrees with a live session
    let mut listed: Vec<String> = Vec::new(); // sessions ptyro still lists
    for seen in &inp.sessions {
        let s = &seen.info;
        listed.push(s.id.full());
        if s.state.is_live()
            && let Some(w) = s.worktree.as_deref()
            && !s.quick
        {
            busy.push(w);
        }
        let wt = s.worktree.as_deref().and_then(wt_of);
        let Some(thing) = of_session(seen, wt, &inp.usage, now) else { continue };
        let key = match s.project.as_ref().filter(|p| rows.contains_key(*p)) {
            Some(p) => p.clone(),
            None => match &s.machine {
                Some(m) => format!("~{m}"),
                None => "~".into(),
            },
        };
        let row = rows.entry(key.clone()).or_insert_with(|| HomeRow {
            key,
            project: None,
            name: "no project".into(),
            workspace: None,
            machine: s.machine.clone(),
            section: Section::Quiet,
            things: Vec::new(),
            live: 0,
            working: 0,
            last_ms: 0,
            week: Vec::new(),
        });
        if s.state.is_live() && s.kind == SessionKind::Agent {
            row.live += 1;
            row.working += u32::from(thing.next == Next::Working);
        }
        row.last_ms = row.last_ms.max(thing.at_ms).max(s.last_output_ms.unwrap_or(0));
        row.things.push(thing);
    }
    // Worktrees with work in them and nothing running.
    for w in &inp.worktrees {
        if busy.contains(&w.id.as_str()) {
            continue;
        }
        if let Some(row) = rows.get_mut(&w.project)
            && let Some(thing) = of_worktree(w)
        {
            row.things.push(thing);
        }
    }
    // What the knowledge base remembers.
    for t in &inp.tails {
        let Some(row) = rows.get_mut(&t.project) else { continue };
        row.week = t.week.clone();
        row.last_ms = row.last_ms.max(t.at_ms);
        let known = t.session.as_ref().is_some_and(|s| listed.contains(s));
        row.things.extend(of_tail(t, known, now));
    }
    let mut rows: Vec<HomeRow> = rows
        .into_values()
        .filter_map(|mut r| {
            r.things.retain(|t| inp.dismissed.get(&t.key) != Some(&t.stamp));
            // The oldest wait first among what needs you; the newest first everywhere else.
            r.things.sort_by(|a, b| {
                a.next.cmp(&b.next).then_with(|| match a.next.section() {
                    Section::Needs => a.at_ms.cmp(&b.at_ms),
                    _ => b.at_ms.cmp(&a.at_ms),
                })
            });
            // Where it was left off is only said when nothing else is.
            if r.things.len() > 1 {
                r.things.retain(|t| t.next != Next::LeftOff);
            }
            if r.things.is_empty() {
                // A project with nothing to say is still a project; a row of no project isn't.
                r.project.as_ref()?;
                r.things.push(Thing {
                    next: Next::Nothing,
                    text: "nothing open".into(),
                    at_ms: r.last_ms,
                    session: None,
                    session_name: None,
                    worktree: None,
                    worktree_name: None,
                    item: None,
                    left: None,
                    key: format!("thing:{}", r.key),
                    stamp: String::new(),
                });
            }
            r.section = r.things[0].next.section();
            Some(r)
        })
        .collect();
    rows.sort_by(|a, b| {
        let (x, y) = (a.next(), b.next());
        a.section
            .cmp(&b.section)
            .then_with(|| match a.section {
                Section::Needs => x.next.cmp(&y.next).then(x.at_ms.cmp(&y.at_ms)),
                Section::Hanging => x.next.cmp(&y.next).then(y.at_ms.cmp(&x.at_ms)),
                Section::Working => x.next.cmp(&y.next).then(b.last_ms.cmp(&a.last_ms)),
                Section::Quiet => b.last_ms.cmp(&a.last_ms),
            })
            .then_with(|| a.name.cmp(&b.name))
    });
    HomeView {
        at_ms: now,
        rows,
        meters: meters(&inp.usage, &inp.sessions, now),
        away: inp.away.clone().filter(|a| now.saturating_sub(a.back_ms) < AWAY_SHOWN_MS),
        memory: inp.memory,
        trail: None,
        at_start: false,
    }
}

/// A week of prompts as eight heights, one character a day: `▁▃▇▅▁▁▂`. Empty stays empty.
pub fn spark(week: &[u16]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = week.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    week.iter()
        .map(|&n| match n {
            0 => '·',
            n => BARS[(usize::from(n) * 8).div_ceil(usize::from(max)).clamp(1, 8) - 1],
        })
        .collect()
}

/// A row's one line, without colour: `ptyro  answer  "run it?"  fix-kb-lock  14m`.
/// `name_w`: the widest project name.
pub fn line(r: &HomeRow, name_w: usize, now: u64) -> String {
    let t = r.next();
    let mut s = format!("{:<name_w$}  {:<9}  {}", r.name, t.next.verb(), t.text);
    let mut tail: Vec<String> = Vec::new();
    if let Some(p) = t.place() {
        tail.push(p.to_owned());
    }
    if let Some(l) = &t.left {
        tail.push(l.clone());
    }
    if r.things.len() > 1 {
        tail.push(format!("+{} more", r.things.len() - 1));
    }
    if t.at_ms > 0 {
        tail.push(crate::dur(now.saturating_sub(t.at_ms)));
    }
    if !tail.is_empty() {
        s.push_str("  · ");
        s.push_str(&tail.join(" · "));
    }
    s.trim_end().to_owned()
}

/// The whole view as plain text: what `ptyro home` prints. With a trail, that project
/// unfolded under its row.
pub fn text(v: &HomeView) -> String {
    let now = v.at_ms;
    let mut out = format!("{}\n", v.headline());
    if let Some(a) = &v.away {
        out.push_str(&format!("{}\n", a.line()));
    }
    let name_w = v.rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(0);
    let mut section = None;
    for r in &v.rows {
        if section != Some(r.section) {
            section = Some(r.section);
            out.push_str(&format!("\n{}\n", r.section.title()));
        }
        out.push_str(&format!("  {}\n", line(r, name_w, now)));
        let Some(trail) = v.trail.as_ref().filter(|t| t.key == r.key) else { continue };
        for t in r.things.iter().skip(1) {
            let place = t.place().map(|p| format!("  · {p}")).unwrap_or_default();
            out.push_str(&format!("      {:<9}  {}{place}\n", t.next.verb(), t.text));
        }
        if let Some(l) = &trail.left_off {
            if let Some(you) = &l.you {
                out.push_str(&format!("      left off   you: {you}\n"));
            }
            if let Some(reply) = &l.reply {
                let who = l.agent.as_deref().unwrap_or("agent");
                out.push_str(&format!("                 {who}: {reply}\n"));
            }
        }
        for e in &trail.entries {
            let detail =
                if e.detail.is_empty() { String::new() } else { format!(" · {}", e.detail) };
            out.push_str(&format!(
                "      {:>4} ago   {:<7} {}{detail}  [{}]\n",
                crate::dur(now.saturating_sub(e.at_ms)),
                e.kind,
                e.title,
                e.addr
            ));
        }
        if !trail.hot.is_empty() {
            let hot: Vec<String> =
                trail.hot.iter().map(|h| format!("{} ×{}", h.path, h.count)).collect();
            out.push_str(&format!("      hot files  {}\n", hot.join(" · ")));
        }
    }
    if v.rows.is_empty() {
        out.push_str(
            "\nNo projects yet: run `ptyro` in a repository, or `ptyro project add <path>`.\n",
        );
    }
    if !v.meters.is_empty() {
        let meters: Vec<String> = v.meters.iter().map(Meter::text).collect();
        out.push_str(&format!("\n{}\n", meters.join("   ")));
    }
    match v.memory {
        Memory::On => {}
        Memory::Off => out.push_str(
            "\nThe knowledge base is off: only what is live is shown (Settings → Knowledge → enabled).\n",
        ),
        Memory::Withheld => out.push_str("\nAsked from an agent's session: only what is live is shown.\n"),
    }
    out
}

#[cfg(test)]
mod tests;
