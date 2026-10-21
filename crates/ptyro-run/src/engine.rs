//! The engine is a pure function: `(recipe, state) → next actions`. The daemon's run actor
//! carries the actions out and appends what happened as events; then it asks again. Nothing
//! here does I/O or reads a clock, so every scheduling rule is a unit test.

use crate::recipe::{CancelSiblings, Join, Recipe, Step};
use crate::state::{RunState, RunStatus, StepState, StepStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start the step's next attempt.
    Start {
        step: String,
    },
    Skip {
        step: String,
        reason: String,
    },
    Fail {
        step: String,
        reason: String,
    },
    /// Stop a running sibling (`cancel_siblings = "interrupt"`).
    Cancel {
        step: String,
        reason: String,
    },
    /// Every step is over.
    Finish {
        status: RunStatus,
        error: Option<String>,
    },
}

fn status(st: &RunState, id: &str) -> StepStatus {
    st.steps.get(id).map_or(StepStatus::Pending, |s| s.status)
}

/// Is `s` ready, skipped, failed, or still waiting on its needs?
fn readiness(s: &Step, st: &RunState) -> Option<Result<(), (bool, String)>> {
    if s.needs.is_empty() {
        return Some(Ok(()));
    }
    let deps: Vec<(&str, StepStatus)> =
        s.needs.iter().map(|n| (n.as_str(), status(st, n))).collect();
    let over = deps.iter().all(|(_, d)| d.is_over());
    let ok = |d: &StepStatus| *d == StepStatus::Succeeded;
    match s.join {
        Join::All => {
            if let Some((n, d)) = deps.iter().find(|(_, d)| d.is_over() && !ok(d)) {
                return Some(Err((false, format!("needs {n}, which {}", d.label()))));
            }
            deps.iter().all(|(_, d)| ok(d)).then_some(Ok(()))
        }
        Join::Any => deps.iter().any(|(_, d)| d.is_over()).then_some(Ok(())),
        Join::FirstSuccess => {
            if deps.iter().any(|(_, d)| ok(d)) {
                Some(Ok(()))
            } else if over {
                Some(Err((true, format!("none of {} succeeded", s.needs.join(", ")))))
            } else {
                None
            }
        }
        Join::AllSettled => over.then_some(Ok(())),
    }
}

pub fn plan(r: &Recipe, st: &RunState, run_dir: &str) -> Vec<Action> {
    if st.status.is_over() {
        return Vec::new();
    }
    let ctx = st.context(run_dir);
    let mut out = Vec::new();
    for s in &r.steps {
        if status(st, &s.id) != StepStatus::Pending {
            continue;
        }
        match readiness(s, st) {
            None => {}
            Some(Err((fail, reason))) => out.push(if fail {
                Action::Fail { step: s.id.clone(), reason }
            } else {
                Action::Skip { step: s.id.clone(), reason }
            }),
            Some(Ok(())) => {
                let go = match &s.when {
                    None => true,
                    Some(cond) => crate::expr::parse(cond).is_ok_and(|e| e.holds(&ctx)),
                };
                out.push(if go {
                    Action::Start { step: s.id.clone() }
                } else {
                    Action::Skip {
                        step: s.id.clone(),
                        reason: format!("condition is false: {}", s.when.as_deref().unwrap_or("")),
                    }
                });
            }
        }
    }
    // A winner stops the race.
    for s in &r.steps {
        if s.join != Join::FirstSuccess || s.cancel_siblings != Some(CancelSiblings::Interrupt) {
            continue;
        }
        let Some(winner) = s.needs.iter().find(|n| status(st, n) == StepStatus::Succeeded) else {
            continue;
        };
        for n in &s.needs {
            if !status(st, n).is_over() {
                out.push(Action::Cancel {
                    step: n.clone(),
                    reason: format!("{winner} succeeded first"),
                });
            }
        }
    }
    if out.is_empty() && r.steps.iter().all(|s| status(st, &s.id).is_over()) {
        out.push(finish(r, st));
    }
    out
}

/// A failure fails the run unless a step that tolerates it (any join but `all`) depends on it.
fn finish(r: &Recipe, st: &RunState) -> Action {
    let absorbed =
        |id: &str| r.steps.iter().any(|d| d.join != Join::All && d.needs.iter().any(|n| n == id));
    for s in &r.steps {
        let ss = st.steps.get(&s.id);
        let failed =
            ss.is_some_and(|x| matches!(x.status, StepStatus::Failed | StepStatus::Crashed));
        if failed && !absorbed(&s.id) {
            let why = ss.and_then(|x| x.reason.clone()).unwrap_or_else(|| "it failed".into());
            return Action::Finish {
                status: RunStatus::Failed,
                error: Some(format!(
                    "step {} {}: {why}",
                    s.id,
                    ss.map_or("failed", |x| x.status.label())
                )),
            };
        }
    }
    Action::Finish { status: RunStatus::Succeeded, error: None }
}

/// Outcomes that retry when `retry.on` is empty.
pub const RETRY_DEFAULT: [&str; 4] = ["failed", "timeout", "stall", "crashed"];

/// Should an attempt that ended with `outcome` be retried? `retry.max` counts retries, so a
/// step runs at most `max + 1` times. Cancellations and rejections never retry.
pub fn should_retry(s: &Step, ss: &StepState, outcome: &str) -> bool {
    let Some(r) = &s.retry else { return false };
    if ["cancelled", "rejected", "succeeded"].contains(&outcome) {
        return false;
    }
    let on =
        r.on.iter().any(|o| o == outcome) || (r.on.is_empty() && RETRY_DEFAULT.contains(&outcome));
    on && !r.not_on.iter().any(|o| o == outcome) && (ss.attempts.len() as u32) <= r.max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RunEvent;
    use std::collections::BTreeMap;

    fn recipe(src: &str) -> Recipe {
        Recipe::parse(src).unwrap()
    }

    fn state(r: &Recipe) -> RunState {
        let mut s = RunState::new("r1");
        s.apply(&RunEvent::Created {
            kind: crate::state::RunKind::Recipe,
            name: r.name.clone(),
            recipe_sha: String::new(),
            vars: BTreeMap::new(),
            project: None,
            base: None,
            lead: None,
            forked_from: None,
            steps: r.steps.iter().map(|s| s.id.clone()).collect(),
            at: 0,
        });
        s
    }

    fn end(s: &mut RunState, id: &str, status: StepStatus) {
        s.apply(&RunEvent::AttemptStarted { step: id.into(), n: 1, at: 1 });
        s.apply(&RunEvent::StepEnded {
            step: id.into(),
            status,
            reason: None,
            decided_by: None,
            at: 2,
        });
    }

    fn starts(a: &[Action]) -> Vec<&str> {
        a.iter()
            .filter_map(|x| match x {
                Action::Start { step } => Some(step.as_str()),
                _ => None,
            })
            .collect()
    }

    const DIAMOND: &str = r#"
name = "d"
[[step]]
id = "a"
run = "true"
[[step]]
id = "b"
needs = ["a"]
run = "true"
[[step]]
id = "c"
needs = ["a"]
if = "steps.a.outputs.go == 'yes'"
run = "true"
[[step]]
id = "d"
needs = ["b", "c"]
join = "all_settled"
run = "true"
"#;

    #[test]
    fn a_diamond_schedules_in_order_and_skips_by_condition() {
        let r = recipe(DIAMOND);
        let mut s = state(&r);
        assert_eq!(starts(&plan(&r, &s, "")), vec!["a"]);
        s.apply(&RunEvent::AttemptStarted { step: "a".into(), n: 1, at: 1 });
        assert!(plan(&r, &s, "").is_empty(), "nothing while a runs");
        s.apply(&RunEvent::StepEnded {
            step: "a".into(),
            status: StepStatus::Succeeded,
            reason: None,
            decided_by: None,
            at: 2,
        });
        let p = plan(&r, &s, "");
        assert_eq!(starts(&p), vec!["b"]);
        assert!(p.iter().any(|x| matches!(x, Action::Skip { step, reason } if step == "c" && reason.contains("condition"))));
        s.apply(&RunEvent::StepEnded {
            step: "c".into(),
            status: StepStatus::Skipped,
            reason: None,
            decided_by: None,
            at: 3,
        });
        end(&mut s, "b", StepStatus::Succeeded);
        assert_eq!(starts(&plan(&r, &s, "")), vec!["d"], "all_settled runs after a skip");
        end(&mut s, "d", StepStatus::Succeeded);
        assert_eq!(
            plan(&r, &s, ""),
            vec![Action::Finish { status: RunStatus::Succeeded, error: None }]
        );
    }

    #[test]
    fn a_failure_skips_dependents_and_fails_the_run() {
        let r = recipe(DIAMOND);
        let mut s = state(&r);
        end(&mut s, "a", StepStatus::Failed);
        let p = plan(&r, &s, "");
        assert!(p.iter().all(|x| matches!(x, Action::Skip { .. })), "{p:?}");
        for x in p {
            if let Action::Skip { step, .. } = x {
                s.apply(&RunEvent::StepEnded {
                    step,
                    status: StepStatus::Skipped,
                    reason: None,
                    decided_by: None,
                    at: 3,
                });
            }
        }
        // d (all_settled) still runs once b and c are settled.
        assert_eq!(starts(&plan(&r, &s, "")), vec!["d"]);
        end(&mut s, "d", StepStatus::Succeeded);
        assert!(
            matches!(&plan(&r, &s, "")[..], [Action::Finish { status: RunStatus::Failed, error: Some(e) }] if e.contains("step a"))
        );
    }

    #[test]
    fn first_success_cancels_the_rest_and_absorbs_failures() {
        let r = recipe(
            r#"
name = "best"
[[step]]
id = "x"
run = "true"
[[step]]
id = "y"
run = "true"
[[step]]
id = "z"
run = "true"
[[step]]
id = "pick"
needs = ["x", "y", "z"]
join = "first_success"
cancel_siblings = "interrupt"
run = "true"
"#,
        );
        let mut s = state(&r);
        assert_eq!(starts(&plan(&r, &s, "")), vec!["x", "y", "z"]);
        for id in ["x", "y", "z"] {
            s.apply(&RunEvent::AttemptStarted { step: id.into(), n: 1, at: 1 });
        }
        s.apply(&RunEvent::StepEnded {
            step: "x".into(),
            status: StepStatus::Failed,
            reason: None,
            decided_by: None,
            at: 2,
        });
        assert!(plan(&r, &s, "").is_empty());
        s.apply(&RunEvent::StepEnded {
            step: "y".into(),
            status: StepStatus::Succeeded,
            reason: None,
            decided_by: None,
            at: 3,
        });
        let p = plan(&r, &s, "");
        assert!(p.contains(&Action::Start { step: "pick".into() }));
        assert!(
            p.contains(&Action::Cancel { step: "z".into(), reason: "y succeeded first".into() })
        );
        s.apply(&RunEvent::StepEnded {
            step: "z".into(),
            status: StepStatus::Cancelled,
            reason: None,
            decided_by: None,
            at: 4,
        });
        end(&mut s, "pick", StepStatus::Succeeded);
        assert_eq!(
            plan(&r, &s, ""),
            vec![Action::Finish { status: RunStatus::Succeeded, error: None }],
            "x's failure is absorbed"
        );
        // All of them failing fails the join.
        let mut s = state(&r);
        for id in ["x", "y", "z"] {
            end(&mut s, id, StepStatus::Failed);
        }
        assert!(matches!(&plan(&r, &s, "")[..], [Action::Fail { step, .. }] if step == "pick"));
    }

    #[test]
    fn retries_follow_the_policy() {
        let r = recipe(
            "name = \"r\"\n[[step]]\nid = \"a\"\nrun = \"true\"\nretry = { max = 1, not_on = [\"timeout\"] }\n",
        );
        let step = r.step("a").unwrap();
        let mut ss = StepState::default();
        ss.attempts.push(Default::default());
        assert!(should_retry(step, &ss, "failed"));
        assert!(!should_retry(step, &ss, "timeout"));
        assert!(!should_retry(step, &ss, "cancelled"));
        ss.attempts.push(Default::default());
        assert!(!should_retry(step, &ss, "failed"), "max = 1 means two attempts");
    }
}
