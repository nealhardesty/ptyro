//! Built-in recipes (M7.8). They show the primitives off and are what most people run; a
//! file with the same name in `~/.config/ptyro/recipes/` (or a trusted repo's
//! `.ptyro/recipes/`) replaces one.

pub const IMPLEMENT_AND_REVIEW: &str = r#"name = "implement-and-review"
description = "One agent implements, an agent from another vendor reviews, the first fixes what's right; you decide."
vars = { task = { required = true, description = "what to build" }, writer = { default = "claude" }, reviewer = { default = "codex" }, test = { default = "scripts.run.test", description = "the check that must pass" } }

[budget]
wall = "3h"
sessions = 4

[[step]]
id = "impl"
spawn = { agent = "{{vars.writer}}", worktree = "new", name = "impl" }
prompt = "Implement: {{vars.task}}\nCommit when the tests pass."
done_when = { run = "{{vars.test}}", max_bounces = 3 }
timeout = { total = "45m", stall = "15m" }
guard = { deny_cmds = ["git push"] }

[[step]]
id = "review"
needs = ["impl"]
spawn = { agent = "{{vars.reviewer}}", worktree = "impl", name = "review" }
prompt = "Review the changes on this branch against its base ({{steps.impl.outputs.base}}) for this task: {{vars.task}}\nCorrectness and requirements only; don't edit files."
output = { schema = "builtin:verdict" }
timeout = "30m"

[[step]]
id = "fix"
needs = ["review"]
if = "steps.review.outputs.output.decision == 'changes'"
send = { to = "impl", text = "A reviewer found:\n{{steps.review.outputs.output.issues}}\nTriage them: fix what is right, push back on what isn't, and commit." }
done_when = { run = "{{vars.test}}", max_bounces = 2 }
timeout = "30m"

[[step]]
id = "gate"
needs = ["review", "fix"]
join = "all_settled"
gate = { ask = "Ship impl ({{steps.impl.outputs.diffstat}})?", options = ["pr", "integrate", "discard"], timeout = "8h", on_timeout = "escalate" }

[[step]]
id = "pr"
needs = ["gate"]
if = "gates.gate.choice == 'pr'"
apply = { from = "impl", actions = ["commit", "push", "pr"] }

[[step]]
id = "integrate"
needs = ["gate"]
if = "gates.gate.choice == 'integrate'"
apply = { from = "impl", actions = ["commit", "integrate"] }
"#;

pub const BEST_OF_N: &str = r#"name = "best-of-n"
description = "Three agents attempt the same task in their own worktrees; ptyro ranks them by evidence and you pick."
vars = { task = { required = true }, a = { default = "claude" }, b = { default = "codex" }, c = { default = "gemini" }, test = { default = "scripts.run.test" } }

[budget]
wall = "2h"
sessions = 3

[[step]]
id = "a"
spawn = { agent = "{{vars.a}}", worktree = "new" }
prompt = "{{vars.task}}\nCommit when you're done."
timeout = { total = "45m", stall = "15m" }

[[step]]
id = "b"
spawn = { agent = "{{vars.b}}", worktree = "new" }
prompt = "{{vars.task}}\nCommit when you're done."
timeout = { total = "45m", stall = "15m" }

[[step]]
id = "c"
spawn = { agent = "{{vars.c}}", worktree = "new" }
prompt = "{{vars.task}}\nCommit when you're done."
timeout = { total = "45m", stall = "15m" }

[[step]]
id = "rank"
needs = ["a", "b", "c"]
join = "all_settled"
verify = { candidates = ["a", "b", "c"], tests = "{{vars.test}}" }

[[step]]
id = "pick"
needs = ["rank"]
gate = { ask = "Best: {{steps.rank.outputs.best}}. Pick with `ptyro compare {{run.id}}` (or Ctrl-Space R), then say done.", options = ["done", "discard-all"] }
"#;

pub const TEST_FIXER: &str = r#"name = "test-fixer"
description = "Run the tests; if they fail, an agent fixes the code (not the tests) until they pass; you decide whether to ship."
vars = { test = { default = "scripts.run.test", description = "the failing check" }, agent = { default = "claude" } }

[budget]
wall = "2h"
sessions = 2

[[step]]
id = "check"
run = "{{vars.test}}"
timeout = "20m"

[[step]]
id = "fix"
needs = ["check"]
join = "any"
if = "steps.check.exit_code != 0"
spawn = { agent = "{{vars.agent}}", worktree = "new", name = "fixer" }
prompt = "This check fails: `{{vars.test}}`\nIts output ends with:\n{{steps.check.outputs.tail}}\nFix the code so it passes. Don't weaken or delete tests. Commit when it passes."
done_when = { run = "{{vars.test}}", max_bounces = 4 }
timeout = { total = "60m", stall = "15m" }
guard = { deny_cmds = ["git push"] }

[[step]]
id = "gate"
needs = ["fix"]
gate = { ask = "Tests pass in the fixer's worktree. Ship it?", options = ["pr", "discard"] }

[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'pr'"
apply = { from = "fix", actions = ["commit", "push", "pr"] }
"#;

pub const DOC_WRITER: &str = r#"name = "doc-writer"
description = "An agent writes or updates documentation; a second agent checks it against the code."
vars = { topic = { required = true, description = "what to document" }, writer = { default = "claude" }, checker = { default = "codex" } }

[budget]
sessions = 2

[[step]]
id = "write"
spawn = { agent = "{{vars.writer}}", worktree = "new", name = "docs" }
prompt = "Write or update the documentation for: {{vars.topic}}\nRead the code first; document what it does today, with examples that work. Commit when done."
timeout = "40m"

[[step]]
id = "check"
needs = ["write"]
spawn = { agent = "{{vars.checker}}", worktree = "write", name = "doc-check" }
prompt = "Check the documentation changes on this branch against the code: every claim, flag and example must be true. Don't edit files."
output = { schema = "builtin:verdict" }
timeout = "20m"

[[step]]
id = "fix"
needs = ["check"]
if = "steps.check.outputs.output.decision == 'changes'"
send = { to = "write", text = "A checker found these problems in the docs:\n{{steps.check.outputs.output.issues}}\nFix the real ones and commit." }

[[step]]
id = "gate"
needs = ["check", "fix"]
join = "all_settled"
gate = { ask = "Open a PR for the docs?", options = ["pr", "discard"] }

[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'pr'"
apply = { from = "write", actions = ["commit", "push", "pr"] }
"#;

pub const OVERNIGHT: &str = r#"name = "overnight"
description = "Loop a fresh agent session on a task until a check passes (or progress stops); review in the morning."
vars = { task = { required = true }, check = { default = "scripts.run.test" }, agent = { default = "claude" } }

[budget]
wall = "8h"
sessions = 12

[[step]]
id = "work"
spawn = { agent = "{{vars.agent}}", worktree = "new", name = "overnight" }
prompt = "{{vars.task}}\nThis is iteration {{loop.iteration}}; earlier iterations' work is already in this worktree. The check `{{vars.check}}` last said:\n{{loop.last_check}}\nMake progress, commit, and stop."
loop = { until = "{{vars.check}}", max = 10, no_progress = 2 }
timeout = { total = "40m", stall = "15m" }
guard = { deny_cmds = ["git push"] }

[[step]]
id = "morning"
needs = ["work"]
join = "any"
gate = { ask = "Overnight: {{steps.work.outputs.iterations}} iterations, check {{steps.work.status}}. Ship it?", options = ["pr", "discard"] }

[[step]]
id = "ship"
needs = ["morning"]
if = "gates.morning.choice == 'pr'"
apply = { from = "work", actions = ["commit", "push", "pr"] }
"#;

/// `(name, source)` of every built-in.
pub const ALL: [(&str, &str); 5] = [
    ("implement-and-review", IMPLEMENT_AND_REVIEW),
    ("best-of-n", BEST_OF_N),
    ("test-fixer", TEST_FIXER),
    ("doc-writer", DOC_WRITER),
    ("overnight", OVERNIGHT),
];

pub fn get(name: &str) -> Option<&'static str> {
    ALL.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_builtin_is_valid() {
        for (name, src) in super::ALL {
            let r = crate::recipe::Recipe::parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(r.name, name);
        }
    }
}
