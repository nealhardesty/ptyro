//! Orchestration as data (SPEC §15): recipes, their validation and templates, the
//! event-sourced run state, and the pure engine that decides what runs next. The daemon
//! (`ptyro-daemon/src/runs.rs`) executes; this crate only decides, so it does no I/O.

pub mod builtin;
pub mod changeset;
pub mod engine;
pub mod expr;
pub mod output;
pub mod recipe;
pub mod state;
pub mod template;
pub mod wire;

pub use engine::{Action, plan, should_retry};
pub use recipe::{Kind, Policy, Recipe, Step};
pub use state::{Attempt, Gate, RunEvent, RunKind, RunState, RunStatus, StepState, StepStatus};

/// A short id with a prefix: `r-7f3a2c` for runs, `g-7f3a` for gates (phone-typeable).
pub fn short_id(prefix: &str, len: usize) -> String {
    let full = ptyro_core::SessionId::new().full();
    format!("{prefix}-{}", &full[full.len() - len..])
}
