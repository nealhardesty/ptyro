//! The tour (UX1.22, `ptyro tour`): ptyro's daily loop, done for real. A pretend agent works in
//! a throwaway repository under a ptyro of its own; a coach in the corner says what to do next,
//! with the keys as this keymap binds them, and moves on when it has been done: start a task,
//! answer the agent, review its change, finish it into main, clean up.
//!
//! The coach never takes keys: everything is done with the real ones, in the real TUI.

use ptyro_core::SessionId;
use ptyro_core::session::SessionKind;
use ptyro_core::status::AgentStatus;

use crate::app::{App, Focus};
use crate::keymap::Table;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Task,
    Answer,
    Review,
    Finish,
    CleanUp,
    Done,
}

#[derive(Debug, Clone)]
pub struct Tour {
    pub step: Step,
    /// The agent the tour's task started, and its worktree.
    agent: Option<SessionId>,
    worktree: Option<String>,
    blocked_once: bool,
}

impl Default for Tour {
    fn default() -> Self {
        Self { step: Step::Task, agent: None, worktree: None, blocked_once: false }
    }
}

const STEPS: usize = 5;

impl Tour {
    /// Move on as far as what's on screen allows; whether the step changed.
    pub fn advance(&mut self, app: &App) -> bool {
        let before = self.step;
        loop {
            let next = match self.step {
                Step::Task => app
                    .sessions
                    .iter()
                    .filter(|s| s.kind == SessionKind::Agent && s.task.is_some())
                    .filter(|s| {
                        app.worktree(s.worktree.as_deref().unwrap_or(""))
                            .is_some_and(|w| !w.is_root)
                    })
                    .max_by_key(|s| s.created_ms)
                    .map(|s| {
                        self.agent = Some(s.id);
                        self.worktree.clone_from(&s.worktree);
                        Step::Answer
                    }),
                Step::Answer => {
                    let status = self.agent_status(app);
                    self.blocked_once |= status == Some(AgentStatus::Blocked);
                    // Answered (it got going again), or its turn ended either way.
                    let idle = matches!(status, Some(AgentStatus::Idle | AgentStatus::Done));
                    let working = status == Some(AgentStatus::Working);
                    ((self.blocked_once && (working || idle)) || (idle && self.turned(app)))
                        .then_some(Step::Review)
                }
                Step::Review => app
                    .inspector
                    .as_ref()
                    .is_some_and(|i| Some(&i.worktree) == self.worktree.as_ref())
                    .then_some(Step::Finish),
                Step::Finish => self.shipped(app).then_some(Step::CleanUp),
                // Archived (or removed): gone from the list.
                Step::CleanUp => self
                    .worktree
                    .as_deref()
                    .is_none_or(|w| app.worktree(w).is_none())
                    .then_some(Step::Done),
                Step::Done => None,
            };
            match next {
                Some(n) => self.step = n,
                None => break,
            }
        }
        self.step != before
    }

    fn agent_status(&self, app: &App) -> Option<AgentStatus> {
        self.agent.and_then(|id| app.session(id)).and_then(|s| s.status.as_ref()).map(|r| r.status)
    }

    fn turned(&self, app: &App) -> bool {
        self.agent.and_then(|id| app.session(id)).is_some_and(|s| s.turns > 0)
    }

    fn shipped(&self, app: &App) -> bool {
        self.worktree.as_deref().and_then(|w| app.worktree(w)).is_some_and(|w| w.shipped())
    }

    /// The coach's title and lines, keys as bound (`C-q n`), or as they work where the focus is.
    pub fn coach(&self, app: &App) -> (String, Vec<String>) {
        let key = |action: &str| {
            let nav = (app.focus == Focus::Sidebar)
                .then(|| app.keymap.key_in(Table::Nav, action))
                .flatten();
            nav.or_else(|| app.keymap.key_for(action)).unwrap_or_else(|| format!("`{action}`"))
        };
        let nav = |action: &str| {
            app.keymap.key_in(Table::Nav, action).unwrap_or_else(|| format!("`{action}`"))
        };
        let sidebar = app.keymap.key_for("focus.toggle").unwrap_or_else(|| "C-q Tab".into());
        let n = match self.step {
            Step::Task => 1,
            Step::Answer => 2,
            Step::Review => 3,
            Step::Finish => 4,
            Step::CleanUp => 5,
            Step::Done => STEPS,
        };
        let (title, lines): (&str, Vec<String>) = match self.step {
            Step::Task => (
                "start a task",
                vec![
                    "This is a ptyro of its own, with a demo repository and a pretend agent: try anything.".into(),
                    format!("Press {} and say what to do,", key("session.new")),
                    "say: fix the typo in greeting.txt".into(),
                    "then ⏎. It gets a worktree and a branch of its own.".into(),
                ],
            ),
            Step::Answer => match self.agent_status(app) {
                Some(AgentStatus::Blocked) => (
                    "answer it",
                    vec![
                        "It wants to run a command and asks first: its row (⚠) says what.".into(),
                        format!("Press {} to peek, then y to allow it.", key("peek")),
                        "(In its pane you can answer it directly too.)".into(),
                    ],
                ),
                _ => (
                    "answer it",
                    vec![
                        "It's working: the row under its name says on what.".into(),
                        "It will ask before it runs a command. Watch for ⚠.".into(),
                    ],
                ),
            },
            Step::Review => (
                "review its change",
                vec![
                    match self.agent_status(app) {
                        Some(AgentStatus::Working) => "It's finishing; its row will say when.".into(),
                        _ => "It's done: its row says what it replied.".into(),
                    },
                    format!("Press {} to see what it changed.", key("inspector")),
                ],
            ),
            Step::Finish => (
                "finish it",
                vec![
                    "Ship it. Close the inspector (q), go to the sidebar".into(),
                    format!(
                        "({sidebar}), and press {} on it; pick integrate.",
                        nav("worktree.finish")
                    ),
                    "Its agent commits the change, then ptyro puts it on main.".into(),
                ],
            ),
            Step::CleanUp => (
                "clean up",
                vec![
                    "It's in main now: its row says so.".into(),
                    format!(
                        "Press {} on it again (or {}) to clean up: the worktree is archived,",
                        nav("worktree.finish"),
                        nav("session.stop")
                    ),
                    "its branch kept, and you can restore it.".into(),
                ],
            ),
            Step::Done => (
                "that's the loop",
                vec![
                    "Task, answer, review, finish, clean up: by keys alone.".into(),
                    format!(
                        "{} lists every key, {} finds any action.",
                        key("help"),
                        app.keymap.key_for("palette").unwrap_or_else(|| "C-q :".into())
                    ),
                    format!(
                        "{} leaves the tour; everything it made is deleted.",
                        app.keymap.key_for("app.detach").unwrap_or_else(|| "C-q d".into())
                    ),
                ],
            ),
        };
        (format!("Tour {n}/{STEPS}: {title}"), lines)
    }
}
