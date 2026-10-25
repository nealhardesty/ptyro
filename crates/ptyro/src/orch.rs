//! Orchestration from the shell (SPEC §15, M7): runs, recipes, fan-out and compare, gates, the
//! mailbox, leases, the collision radar, checkpoints and verified-done. The TUI's runs view
//! (`O`) sends the same requests.
//!
//! ```sh
//! ptyro fanout --agents claude,codex,gemini --prompt-file task.md --test --wait
//! ptyro compare @last && ptyro pick @last claude
//! ptyro recipe run implement-and-review --var task="add --json to ls"
//! ptyro gate resolve g-7f3a approve
//! ```

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_client::Client;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};
use ptyro_run::wire::{CompareDoc, FanoutSpec, PickMode, RunInfo};
use ptyro_run::{RunStatus, StepStatus};

use crate::cli::{Ctx, age, current_dir, no_daemon, unexpected};

#[derive(Subcommand)]
pub enum RunCmd {
    /// Runs, newest first
    Ls {
        /// Only running or waiting ones
        #[arg(long)]
        active: bool,
        /// Show forks under the run they came from
        #[arg(long)]
        tree: bool,
        #[arg(long)]
        json: bool,
    },
    /// A run's steps, attempts, gates and outputs
    Show {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// The run's event log, oldest first
    Events {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Wait until the run ends (or a gate opens, with --gate)
    Wait {
        run: String,
        #[arg(long)]
        timeout: Option<String>,
        /// Also return when a gate waits for an answer
        #[arg(long)]
        gate: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop the run: running steps are cancelled, their agents stopped (worktrees stay)
    Cancel { run: String },
    /// Run a step again (a failed, held or finished one), and what it skipped
    Retry { run: String, step: String },
    /// A new run that keeps every step before STEP and runs from there
    Fork {
        run: String,
        #[arg(long)]
        from: String,
        /// Change a variable: `--set task="…"`
        #[arg(long, value_parser = parse_kv)]
        set: Vec<(String, String)>,
        #[arg(long)]
        json: bool,
    },
    /// Make a running agent session a lead (it can then use `ptyro mcp` and `ptyro msg`)
    Lead {
        #[arg(add = ArgValueCandidates::new(crate::complete::sessions))]
        session: String,
        #[arg(long)]
        max_workers: Option<u32>,
        #[arg(long = "agent")]
        agents: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum RecipeCmd {
    /// Built-in, your own (~/.config/ptyro/recipes) and the project's (.ptyro/recipes)
    Ls {
        #[arg(long, add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// A recipe's source
    Show {
        name: String,
        #[arg(long, add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
    },
    /// Check a recipe file (every mistake at once, with where)
    Validate { file: PathBuf },
    /// The step graph, as Mermaid (default) or Graphviz dot
    Graph {
        /// A recipe name or file
        recipe: String,
        #[arg(long, default_value = "mermaid")]
        format: String,
        #[arg(long, add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
    },
    /// Start a recipe (a name or a .toml file)
    Run {
        recipe: String,
        /// `--var task="…"` (repeatable)
        #[arg(long = "var", value_parser = parse_kv)]
        vars: Vec<(String, String)>,
        #[arg(long, add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        /// Base for new worktrees (default: the project's)
        #[arg(long)]
        base: Option<String>,
        /// Trust this repository recipe (the fingerprint `ptyro recipe run` printed)
        #[arg(long)]
        trust: Option<String>,
        /// Wait for it to end (exit 0 if it succeeded)
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum GateCmd {
    /// Gates waiting for an answer (--all: answered ones too)
    Ls {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Answer a gate: `ptyro gate resolve g-7f3a approve`
    Resolve {
        id: String,
        choice: String,
        #[arg(long)]
        note: Option<String>,
        /// An idempotency key: resending with it never answers twice
        #[arg(long)]
        key: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum MsgCmd {
    /// Send a message: TO is a session, @lead, @user, @all, @idle, `@<agent>` or `@run:<id>`
    Send {
        to: String,
        text: String,
        /// dispatch, status, question, escalation, note…
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        reply_to: Option<String>,
        /// Deliver mid-turn where the agent allows it
        #[arg(long)]
        interrupt: bool,
        /// Sending again with the same key doesn't send twice
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Messages for you (or --me SESSION), after --since; --wait for the first one
    Check {
        #[arg(long)]
        me: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        wait: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The ledger: every message (of a run) with how far it got
    Ls {
        #[arg(long)]
        run: Option<String>,
        #[arg(short = 'n', long, default_value_t = 100)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Mark messages handled
    Ack {
        #[arg(long)]
        me: Option<String>,
        ids: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum LeaseCmd {
    /// Claim files (paths or globs) for a session: others' edits to them are denied
    Claim {
        #[arg(required = true)]
        patterns: Vec<String>,
        /// Only you may change them (default: shared, which only blocks exclusive claims)
        #[arg(long)]
        exclusive: bool,
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        /// The holder (default: the session you're in)
        #[arg(long = "as")]
        holder: Option<String>,
    },
    /// Release your leases (all, or these patterns)
    Release {
        patterns: Vec<String>,
        #[arg(long = "as")]
        holder: Option<String>,
    },
    Ls {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum CheckpointCmd {
    /// A session's checkpoints (or every session's in a worktree)
    Ls {
        /// A session or worktree (default: the one you're in)
        addr: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// What changed from checkpoint N (to M, or to now)
    Diff {
        session: String,
        n: u32,
        to: Option<u32>,
        #[arg(long)]
        stat: bool,
    },
    /// Put the worktree back to checkpoint N (default: the latest); the present is checkpointed first
    Restore {
        addr: String,
        n: Option<u32>,
        /// The checkpoint that closed this agent turn
        #[arg(long, conflicts_with = "n")]
        turn: Option<u32>,
        #[arg(short, long)]
        yes: bool,
    },
}

pub fn parse_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s.split_once('=').ok_or("expected KEY=VALUE")?;
    Ok((k.trim().to_owned(), v.to_owned()))
}

fn json_out<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn duration(s: &str) -> Result<Duration, Error> {
    crate::script::duration(s)
}

/// The project for a new run: the one asked for, else the repository you're in.
fn project_or_here(p: Option<String>) -> Result<Option<String>, Error> {
    Ok(match p {
        Some(p) => Some(p),
        None => Some(current_dir()?.to_string_lossy().into_owned()),
    })
}

fn me() -> Option<String> {
    std::env::var("PTYRO_SESSION_ID").ok()
}

fn run_of(r: Reply) -> Result<RunInfo, Error> {
    match r {
        Reply::Run { run } => Ok(run),
        other => Err(unexpected(other)),
    }
}

fn icon(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "·",
        StepStatus::Running => "●",
        StepStatus::Waiting => "⏸",
        StepStatus::Succeeded => "✓",
        StepStatus::Failed => "✗",
        StepStatus::Crashed => "↯",
        StepStatus::Skipped => "–",
        StepStatus::Cancelled => "⊘",
    }
}

fn print_run(r: &RunInfo) {
    let s = &r.state;
    println!(
        "run {}  {} {}  {}  started {} ago",
        s.id,
        s.kind.label(),
        s.name,
        s.status.label(),
        age(s.created_ms)
    );
    for (k, v) in &s.vars {
        println!(
            "  var {k} = {}",
            v.lines().next().unwrap_or("").chars().take(80).collect::<String>()
        );
    }
    if let Some((from, step)) = &s.forked_from {
        println!("  forked from {from} at {step}");
    }
    if let Some(e) = &s.error {
        println!("  error: {e}");
    }
    println!("  dir {}", r.dir);
    for id in &r.order {
        let Some(ss) = s.steps.get(id) else { continue };
        let a = ss.attempt();
        let mut line = format!("  {} {:<14} {:<10}", icon(ss.status), id, ss.status.label());
        if ss.attempts.len() > 1 {
            line.push_str(&format!(" try {}", ss.attempts.len()));
        }
        if let Some(b) = a.map(|a| a.bounces).filter(|b| *b > 0) {
            line.push_str(&format!(" bounced {b}×"));
        }
        if let Some(n) = ss.outputs.get("session_name").and_then(|v| v.as_str()) {
            line.push_str(&format!("  {n}"));
        }
        if let Some(n) = ss.outputs.get("worktree_name").and_then(|v| v.as_str()) {
            line.push_str(&format!("  in {n}"));
        }
        if let Some(d) = ss.outputs.get("diffstat").and_then(|v| v.as_str()) {
            line.push_str(&format!("  {d}"));
        }
        if let Some(c) = ss.outputs.get("exit_code") {
            line.push_str(&format!("  exit {c}"));
        }
        if let Some(b) = ss.outputs.get("best").and_then(|v| v.as_str()) {
            line.push_str(&format!("  best: {b}"));
        }
        if let Some(d) = &ss.decided_by {
            line.push_str(&format!("  ({d})"));
        }
        println!("{line}");
        if let Some(g) = &ss.gate {
            match &g.choice {
                None => println!(
                    "      ⏸ gate {}: {}\n        ptyro gate resolve {} {}",
                    g.id,
                    g.ask,
                    g.id,
                    g.options.join("|")
                ),
                Some(c) => println!(
                    "      gate {} answered {c:?} by {}",
                    g.id,
                    g.by.clone().unwrap_or_default()
                ),
            }
        }
        if ss.status != StepStatus::Succeeded
            && let Some(r) = &ss.reason
        {
            for l in r.lines().take(6) {
                println!("      {l}");
            }
        }
    }
    if !s.picked.is_empty() {
        println!(
            "  picked: {}",
            s.picked.iter().map(|(p, m)| format!("{p} ({m})")).collect::<Vec<_>>().join(", ")
        );
    }
}

/// "12 calls (Bash 7, Edit 5), 1 failed, 3 files · $0.0412" (structured candidates, M10).
pub fn tools_line(t: &ptyro_run::wire::ToolUse) -> String {
    let by: Vec<String> = t.by_tool.iter().map(|(k, v)| format!("{k} {v}")).collect();
    let mut s =
        format!("{} call{} ({})", t.calls, if t.calls == 1 { "" } else { "s" }, by.join(", "));
    if t.failed > 0 {
        s.push_str(&format!(", {} failed", t.failed));
    }
    if !t.files.is_empty() {
        s.push_str(&format!(
            ", {} file{}",
            t.files.len(),
            if t.files.len() == 1 { "" } else { "s" }
        ));
    }
    if let Some(c) = t.cost_micro_usd {
        s.push_str(&format!(" · ${:.4}", c as f64 / 1e6));
    }
    s
}

pub fn print_compare(d: &CompareDoc) {
    if let Some(b) = d.base_tests {
        println!(
            "base {}: tests {}",
            d.base.clone().unwrap_or_default(),
            if b == 0 { "pass".to_owned() } else { format!("fail (exit {b})") }
        );
    }
    for w in &d.warnings {
        println!("⚠ {w}");
    }
    for c in &d.candidates {
        let mark = if c.picked {
            " ★ picked"
        } else if c.discarded {
            " (discarded)"
        } else {
            ""
        };
        println!(
            "#{} {:<12} {:<9} {:<10} {}{mark}",
            c.rank,
            c.step,
            c.status,
            c.agent.clone().unwrap_or_default(),
            c.worktree_name.clone().unwrap_or_default()
        );
        println!("   {}", c.reasons.join(" · "));
        if let Some(m) = &c.last_message {
            println!("   “{}”", m.chars().take(140).collect::<String>());
        }
        if let Some(n) = &c.judge_note {
            println!("   judge: {n}");
        }
        if let Some(t) = &c.tools {
            println!("   tools: {}", tools_line(t));
        }
    }
    if let Some(t) = &d.top_difference {
        println!("\nbetween #1 and #2:\n{}", t.trim_end());
    }
    if let Some(best) = d.candidates.first() {
        println!(
            "\nnext: ptyro pick {} {}  ·  ptyro iterate {} {} \"…\"  ·  ptyro discard {}",
            d.run, best.step, d.run, best.step, d.run
        );
    }
}

/// Wait for a run to end (or open a gate). Exit code: 0 succeeded, 1 failed, 5 cancelled, 7 timeout.
async fn wait_run(
    ctx: &Ctx,
    c: &mut Client,
    run: &str,
    timeout: Option<Duration>,
    gate: bool,
) -> Result<(RunInfo, i32), Error> {
    let start = Instant::now();
    loop {
        let r = match c.request(Request::RunGet { id: run.into() }).await {
            // A live upgrade (or a restart): runs survive both, so follow the new daemon.
            Err(e) if e.code == ErrorCode::DaemonUnavailable => {
                *c = crate::script::reconnect(ctx).await?;
                continue;
            }
            other => run_of(other?)?,
        };
        let code = match r.state.status {
            RunStatus::Succeeded => Some(0),
            RunStatus::Failed => Some(1),
            RunStatus::Cancelled => Some(ErrorCode::Conflict.exit_code()),
            RunStatus::Waiting if gate => Some(0),
            _ => None,
        };
        if let Some(code) = code {
            return Ok((r, code));
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            return Ok((r, ErrorCode::Timeout.exit_code()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn run(ctx: &Ctx, cmd: RunCmd) -> Result<i32, Error> {
    let mut c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        RunCmd::Ls { active, tree, json } => {
            let Reply::Runs { runs } =
                c.request(Request::RunList { active, limit: Some(200) }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&runs);
                return Ok(0);
            }
            if runs.is_empty() {
                eprintln!("no runs yet (ptyro fanout, ptyro recipe run)");
            }
            let line = |r: &RunInfo, indent: &str| {
                let s = &r.state;
                let done = s.steps.values().filter(|x| x.status.is_over()).count();
                let gates = s.open_gates().len();
                println!(
                    "{indent}{:<9} {:<8} {:<22} {:<10} {done}/{} steps  {} ago{}",
                    s.id,
                    s.kind.label(),
                    s.name,
                    s.status.label(),
                    s.steps.len(),
                    age(s.created_ms),
                    if gates > 0 { format!("  ⏸ {gates} gate(s)") } else { String::new() }
                );
            };
            if tree {
                let children: Vec<&RunInfo> =
                    runs.iter().filter(|r| r.state.forked_from.is_some()).collect();
                for r in runs.iter().filter(|r| r.state.forked_from.is_none()) {
                    line(r, "");
                    for ch in children
                        .iter()
                        .filter(|x| x.state.forked_from.as_ref().is_some_and(|f| f.0 == r.state.id))
                    {
                        line(ch, "  └ ");
                    }
                }
            } else {
                for r in &runs {
                    line(r, "");
                }
            }
            Ok(0)
        }
        RunCmd::Show { run, json } => {
            let r = run_of(c.request(Request::RunGet { id: run }).await?)?;
            if json {
                json_out(&r);
            } else {
                print_run(&r);
            }
            Ok(0)
        }
        RunCmd::Events { run, json } => {
            let Reply::RunEvents { events } = c.request(Request::RunEvents { id: run }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            for (seq, at, e) in events {
                if json {
                    println!("{}", serde_json::json!({ "seq": seq, "at_ms": at, "event": e }));
                } else {
                    let kind = e["e"].as_str().unwrap_or("?");
                    let step = e["step"].as_str().map(|s| format!(" {s}")).unwrap_or_default();
                    let mut rest = e.clone();
                    if let Some(o) = rest.as_object_mut() {
                        o.remove("e");
                        o.remove("step");
                        o.remove("at");
                    }
                    println!("{seq:>4} {:>8} ago  {kind}{step} {}", age(at), rest);
                }
            }
            Ok(0)
        }
        RunCmd::Wait { run, timeout, gate, json } => {
            let t = timeout.as_deref().map(duration).transpose()?;
            let id = run_of(c.request(Request::RunGet { id: run }).await?)?.state.id;
            let (r, code) = wait_run(ctx, &mut c, &id, t, gate).await?;
            if json {
                json_out(&r);
            } else {
                println!("{} {}", r.state.id, r.state.status.label());
            }
            Ok(code)
        }
        RunCmd::Cancel { run } => {
            let r = run_of(c.request(Request::RunCancel { id: run }).await?)?;
            eprintln!("cancelled {}", r.state.id);
            Ok(0)
        }
        RunCmd::Retry { run, step } => {
            let r = run_of(c.request(Request::RunRetry { id: run, step: step.clone() }).await?)?;
            eprintln!("{}: {step} runs again", r.state.id);
            Ok(0)
        }
        RunCmd::Fork { run, from, set, json } => {
            let r = run_of(
                c.request(Request::RunFork { id: run, from, set: set.into_iter().collect() })
                    .await?,
            )?;
            if json {
                json_out(&r);
            } else {
                println!("{}", r.state.id);
                eprintln!(
                    "forked: steps before the fork point kept; ptyro run show {}",
                    r.state.id
                );
            }
            Ok(0)
        }
        RunCmd::Lead { session, max_workers, agents } => {
            let mut p = ptyro_run::Policy::default();
            if let Some(n) = max_workers {
                p.max_sessions = n;
            }
            p.agents = agents;
            let r =
                run_of(c.request(Request::LeadStart { addr: session, policy: Some(p) }).await?)?;
            println!("{}", r.state.id);
            eprintln!(
                "lead run {}: its agent can coordinate workers with `ptyro msg` (and `ptyro mcp`, if its agent was started with --lead)",
                r.state.id
            );
            Ok(0)
        }
    }
}

pub async fn recipe(ctx: &Ctx, cmd: RecipeCmd) -> Result<i32, Error> {
    if let RecipeCmd::Validate { file } = &cmd {
        let src = std::fs::read_to_string(file)
            .map_err(|e| Error::invalid(format!("{}: {e}", file.display())))?;
        return match ptyro_run::Recipe::parse(&src) {
            Ok(r) => {
                println!("{}: ok ({} steps)", r.name, r.steps.len());
                Ok(0)
            }
            Err(e) => {
                for l in &e.0 {
                    eprintln!("{}: {l}", file.display());
                }
                Ok(ErrorCode::Invalid.exit_code())
            }
        };
    }
    let mut c = ctx.connect(true).await.map_err(no_daemon)?;
    match cmd {
        RecipeCmd::Validate { .. } => unreachable!(),
        RecipeCmd::Ls { project, json } => {
            let project = project_or_here(project)?;
            let Reply::Recipes { recipes } = c.request(Request::RecipeList { project }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&recipes);
                return Ok(0);
            }
            for r in recipes {
                let flag = match (&r.error, &r.untrusted) {
                    (Some(_), _) => " (invalid)",
                    (_, Some(_)) => " (untrusted)",
                    _ => "",
                };
                println!(
                    "{:<22} {:<12} {}{flag}",
                    r.name,
                    r.source,
                    r.description.clone().unwrap_or_default()
                );
                if !r.vars.is_empty() {
                    println!("{:<22} vars: {}", "", r.vars.join(", "));
                }
                if let Some(e) = r.error {
                    println!("{:<22} {}", "", e.lines().next().unwrap_or(""));
                }
            }
            Ok(0)
        }
        RecipeCmd::Show { name, project } => {
            let project = project_or_here(project)?;
            let Reply::Recipe { info, src } =
                c.request(Request::RecipeShow { name, project }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            eprintln!(
                "# {} ({}{})",
                info.name,
                info.source,
                info.path.map(|p| format!(", {p}")).unwrap_or_default()
            );
            print!("{src}");
            if let Some(fp) = info.untrusted {
                eprintln!("# untrusted: run it with --trust {fp} once you've read it");
            }
            Ok(0)
        }
        RecipeCmd::Graph { recipe, format, project } => {
            let src = match std::fs::read_to_string(&recipe) {
                Ok(s) => s,
                Err(_) => {
                    let Reply::Recipe { src, .. } = c
                        .request(Request::RecipeShow {
                            name: recipe,
                            project: project_or_here(project)?,
                        })
                        .await?
                    else {
                        return Err(Error::internal("unexpected reply"));
                    };
                    src
                }
            };
            let r = ptyro_run::Recipe::parse(&src).map_err(|e| Error::invalid(e.to_string()))?;
            print!("{}", r.graph(&format).map_err(Error::invalid)?);
            Ok(0)
        }
        RecipeCmd::Run { recipe, vars, project, base, trust, wait, json } => {
            let (name, src) = match std::fs::read_to_string(&recipe) {
                Ok(s) if recipe.ends_with(".toml") || recipe.contains('/') => (None, Some(s)),
                _ => (Some(recipe), None),
            };
            let req = Request::RecipeRun {
                name,
                src,
                vars: vars.into_iter().collect::<BTreeMap<_, _>>(),
                project: project_or_here(project)?,
                base,
                trust,
            };
            let r = match c.request(req.clone()).await {
                Err(e) if e.code == ErrorCode::UntrustedProject => {
                    let fp = e.message.rsplit(' ').next().unwrap_or("").to_owned();
                    if std::io::stdin().is_terminal() && !fp.is_empty() {
                        eprintln!("{}", e.message);
                        eprint!("Trust it and run? [y/N] ");
                        let mut a = String::new();
                        let _ = std::io::stdin().read_line(&mut a);
                        if !a.trim().eq_ignore_ascii_case("y") {
                            return Ok(ErrorCode::UntrustedProject.exit_code());
                        }
                        let Request::RecipeRun { name, src, vars, project, base, .. } = req else {
                            unreachable!()
                        };
                        run_of(
                            c.request(Request::RecipeRun {
                                name,
                                src,
                                vars,
                                project,
                                base,
                                trust: Some(fp),
                            })
                            .await?,
                        )?
                    } else {
                        return Err(e);
                    }
                }
                other => run_of(other?)?,
            };
            if !wait {
                if json {
                    json_out(&r);
                } else {
                    println!("{}", r.state.id);
                    eprintln!(
                        "started {} ({}): ptyro run show {} · Ctrl-Q O in the TUI",
                        r.state.name, r.state.id, r.state.id
                    );
                }
                return Ok(0);
            }
            let (r, code) = wait_run(ctx, &mut c, &r.state.id, None, false).await?;
            if json {
                json_out(&r);
            } else {
                print_run(&r);
            }
            Ok(code)
        }
    }
}

pub async fn fanout(
    ctx: &Ctx,
    mut spec: FanoutSpec,
    n: Option<u32>,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    wait: bool,
    json: bool,
) -> Result<i32, Error> {
    spec.prompt = match (prompt, prompt_file) {
        (_, Some(f)) => std::fs::read_to_string(&f)
            .map_err(|e| Error::invalid(format!("{}: {e}", f.display())))?,
        (Some(p), None) => p,
        (None, None) => {
            return Err(Error::invalid("what should they do? --prompt TEXT or --prompt-file FILE"));
        }
    };
    let mut list: Vec<String> = spec
        .agents
        .iter()
        .flat_map(|a| a.split(','))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(n) = n {
        // `-n 3 --agents claude`: three of the same; `-n 4 --agents claude,codex`: alternate.
        let pool = if list.is_empty() { vec!["claude".to_owned()] } else { list.clone() };
        list = (0..n as usize).map(|i| pool[i % pool.len()].clone()).collect();
    }
    spec.agents = list;
    spec.project = project_or_here(spec.project)?;
    let mut c = ctx.connect(true).await.map_err(no_daemon)?;
    let r = run_of(c.request(Request::Fanout { spec }).await?)?;
    if !wait {
        if json {
            json_out(&r);
        } else {
            println!("{}", r.state.id);
            eprintln!(
                "fan-out {} with {} candidates: ptyro compare {} (when they're done) · Ctrl-Q O",
                r.state.id,
                r.order.len().saturating_sub(1),
                r.state.id
            );
        }
        return Ok(0);
    }
    let (r, code) = wait_run(ctx, &mut c, &r.state.id, None, false).await?;
    let Reply::Compare { doc } =
        c.request(Request::Compare { run: r.state.id.clone(), refresh: false }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        json_out(&doc);
    } else {
        print_compare(&doc);
    }
    Ok(code)
}

pub async fn compare(ctx: &Ctx, run: String, refresh: bool, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::Compare { doc } = c.request(Request::Compare { run, refresh }).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        json_out(&doc);
    } else {
        print_compare(&doc);
    }
    Ok(0)
}

fn notes(r: Reply) -> Result<i32, Error> {
    match r {
        Reply::Notes { notes } => {
            for n in notes {
                eprintln!("{n}");
            }
            Ok(0)
        }
        other => Err(unexpected(other)),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn pick(
    ctx: &Ctx,
    run: String,
    candidate: String,
    cherry_pick: bool,
    pr: bool,
    apply: bool,
    into: Option<String>,
    paths: Vec<String>,
    hunks: Vec<String>,
) -> Result<i32, Error> {
    let mode = if pr {
        PickMode::Pr
    } else if cherry_pick {
        PickMode::CherryPick
    } else if apply || !paths.is_empty() || !hunks.is_empty() {
        PickMode::Apply
    } else {
        PickMode::Merge
    };
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    notes(c.request(Request::Pick { run, candidate, mode, into, paths, hunks }).await?)
}

pub async fn discard(
    ctx: &Ctx,
    run: String,
    candidate: Option<String>,
    force: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    notes(c.request(Request::Discard { run, candidate, force }).await?)
}

pub async fn iterate(ctx: &Ctx, run: String, mut args: Vec<String>) -> Result<i32, Error> {
    let text = args.pop().ok_or_else(|| Error::invalid("what should they do next?"))?;
    let candidate = args.pop();
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    notes(c.request(Request::Iterate { run, candidate, text }).await?)
}

pub async fn gate(ctx: &Ctx, cmd: GateCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        GateCmd::Ls { all, json } => {
            let Reply::Gates { gates } = c.request(Request::GateList { all }).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&gates);
                return Ok(0);
            }
            if gates.is_empty() {
                eprintln!("no gates waiting");
            }
            for g in gates {
                let state = g.gate.choice.as_ref().map_or_else(
                    || format!("waiting {}", age(g.gate.opened_ms)),
                    |c| format!("answered {c:?}"),
                );
                println!(
                    "{:<7} {:<9} {:<10} {}  [{}]  {state}",
                    g.gate.id,
                    g.run,
                    g.step,
                    g.gate.ask,
                    g.gate.options.join("/")
                );
            }
            Ok(0)
        }
        GateCmd::Resolve { id, choice, note, key } => {
            let Reply::Gates { gates } = c
                .request(Request::GateResolve { id, choice, note, key, by: Some("cli".into()) })
                .await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            for g in gates {
                eprintln!(
                    "gate {} ({} / {}): {}",
                    g.gate.id,
                    g.run,
                    g.step,
                    g.gate.choice.unwrap_or_default()
                );
            }
            Ok(0)
        }
    }
}

pub async fn msg(ctx: &Ctx, cmd: MsgCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        MsgCmd::Send { to, text, kind, run, reply_to, interrupt, key, json } => {
            let req = Request::MsgPost {
                from: me(),
                to,
                kind: Some(kind),
                body: text,
                run,
                in_reply_to: reply_to,
                priority: interrupt.then(|| "interrupt".into()),
                ttl_ms: None,
                key,
            };
            let Reply::Posted { ids, to } = c.request(req).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&serde_json::json!({ "ids": ids, "to": to }));
            } else if to.is_empty() {
                eprintln!("not sent (a repeat, or the recipients' queues are full)");
            } else {
                eprintln!("queued for {} (delivered once each is idle)", to.join(", "));
            }
            Ok(0)
        }
        MsgCmd::Check { me: who, since, wait, json } => {
            let who = who.or_else(me).unwrap_or_else(|| "@user".into());
            let wait_ms = wait.as_deref().map(duration).transpose()?.map(|d| d.as_millis() as u64);
            let Reply::Messages { messages } =
                c.request(Request::MsgCheck { me: who, since, wait_ms }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&messages);
            } else {
                for m in &messages {
                    println!(
                        "{} {} → {} [{}] {}\n  {}",
                        m.id,
                        m.from,
                        m.to,
                        m.kind,
                        age(m.at_ms),
                        m.body.replace('\n', "\n  ")
                    );
                }
            }
            Ok(if messages.is_empty() && wait_ms.is_some() {
                ErrorCode::Timeout.exit_code()
            } else {
                0
            })
        }
        MsgCmd::Ls { run, limit, json } => {
            let Reply::Messages { messages } =
                c.request(Request::MsgList { run, limit: Some(limit) }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&messages);
            } else {
                for m in &messages {
                    println!(
                        "{:>8} ago  {:<10} {} → {} [{}] {}",
                        age(m.at_ms),
                        m.state,
                        m.from,
                        m.to,
                        m.kind,
                        m.body.lines().next().unwrap_or("").chars().take(80).collect::<String>()
                    );
                }
            }
            Ok(0)
        }
        MsgCmd::Ack { me: who, ids } => {
            let who = who.or_else(me).unwrap_or_else(|| "@user".into());
            c.request(Request::MsgAck { me: who, ids }).await?;
            Ok(0)
        }
    }
}

pub async fn lease(ctx: &Ctx, cmd: LeaseCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let holder = |h: Option<String>| {
        h.or_else(me).ok_or_else(|| {
            Error::invalid("whose lease? run it inside a session, or pass --as SESSION")
        })
    };
    let (leases, refused) = match cmd {
        LeaseCmd::Claim { patterns, exclusive, ttl, reason, holder: h } => {
            let ttl_ms = ttl.as_deref().map(duration).transpose()?.map(|d| d.as_millis() as u64);
            match c
                .request(Request::LeaseClaim {
                    holder: holder(h)?,
                    patterns,
                    exclusive,
                    ttl_ms,
                    reason,
                })
                .await?
            {
                Reply::Leases { leases, refused } => (leases, refused),
                other => return Err(unexpected(other)),
            }
        }
        LeaseCmd::Release { patterns, holder: h } => {
            match c.request(Request::LeaseRelease { holder: holder(h)?, patterns }).await? {
                Reply::Leases { leases, refused } => (leases, refused),
                other => return Err(unexpected(other)),
            }
        }
        LeaseCmd::Ls { json } => {
            let Reply::Leases { leases, .. } = c.request(Request::LeaseList).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&leases);
                return Ok(0);
            }
            (leases, vec![])
        }
    };
    for l in &leases {
        let left = l.expires_ms.saturating_sub(ptyro_core::now_ms()) / 60_000;
        println!(
            "{:<9} {:<16} {:<30} {:<9} {left}m left{}",
            l.id,
            l.holder_name,
            l.pattern,
            if l.exclusive { "exclusive" } else { "shared" },
            l.reason.as_deref().map(|r| format!("  {r}")).unwrap_or_default()
        );
    }
    for r in &refused {
        eprintln!("refused: {r}");
    }
    Ok(if refused.is_empty() { 0 } else { ErrorCode::Conflict.exit_code() })
}

pub async fn radar(ctx: &Ctx, project: Option<String>, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::Radar { radar } =
        c.request(Request::Radar { project: project_or_here(project)? }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        json_out(&radar);
        return Ok(0);
    }
    if radar.overlaps.is_empty() {
        println!(
            "{}: no overlapping changes between {} live worktree(s)",
            radar.project,
            radar.worktrees.len()
        );
    }
    for o in &radar.overlaps {
        println!("{} ⟷ {}: both changed {}", o.a, o.b, o.files.join(", "));
        if !o.conflicts.is_empty() {
            println!("   merging them would conflict in {}", o.conflicts.join(", "));
        }
    }
    Ok(if radar.overlaps.iter().any(|o| !o.conflicts.is_empty()) { 1 } else { 0 })
}

pub async fn checkpoint(ctx: &Ctx, cmd: CheckpointCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        CheckpointCmd::Ls { addr, json } => {
            let addr = match addr.or_else(me) {
                Some(a) => a,
                None => current_dir()?.to_string_lossy().into_owned(),
            };
            let Reply::Checkpoints { checkpoints } =
                c.request(Request::CheckpointList { addr }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                json_out(&checkpoints);
                return Ok(0);
            }
            if checkpoints.is_empty() {
                eprintln!(
                    "no checkpoints yet (one is taken after every agent turn that changed something)"
                );
            }
            for k in &checkpoints {
                println!(
                    "{:>3}  turn {:<3} {:>8} ago  {} file(s)  {}  {}",
                    k.n,
                    k.turn,
                    age(k.at_ms),
                    k.files,
                    &k.sha[..k.sha.len().min(10)],
                    k.refname
                );
            }
            Ok(0)
        }
        CheckpointCmd::Diff { session, n, to, stat } => {
            let Reply::Diff { doc } =
                c.request(Request::CheckpointDiff { addr: session, n, to }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if stat {
                for f in &doc.files {
                    println!(
                        "{:>5} {:>5}  {} ({})",
                        format!("+{}", f.added),
                        format!("-{}", f.removed),
                        f.path,
                        f.status
                    );
                }
            } else {
                print!("{}", crate::ship::render_diff(&doc, std::io::stdout().is_terminal()));
            }
            Ok(0)
        }
        CheckpointCmd::Restore { addr, n, turn, yes } => {
            if !yes && std::io::stdin().is_terminal() {
                eprint!(
                    "Restore {addr} to {}? The worktree's files change (the present is checkpointed first). [y/N] ",
                    n.map(|n| format!("checkpoint {n}"))
                        .or(turn.map(|t| format!("turn {t}")))
                        .unwrap_or_else(|| "the latest checkpoint".into())
                );
                let mut a = String::new();
                let _ = std::io::stdin().read_line(&mut a);
                if !a.trim().eq_ignore_ascii_case("y") {
                    return Ok(ErrorCode::Conflict.exit_code());
                }
            }
            notes(c.request(Request::CheckpointRestore { addr, n, turn }).await?)
        }
    }
}

pub async fn verify(
    ctx: &Ctx,
    session: String,
    check: Option<String>,
    off: bool,
    max_bounces: Option<u32>,
) -> Result<i32, Error> {
    if check.is_none() && !off {
        return Err(Error::invalid(
            "give the check (`scripts.run.test`, a run script, a command), or --off",
        ));
    }
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let info = crate::cli::session_of(
        c.request(Request::SetVerify { addr: session, check: check.filter(|_| !off), max_bounces })
            .await?,
    )?;
    match info.verify {
        Some(v) => eprintln!(
            "{}: its turns are done when `{}` passes (bounced back up to {}×)",
            info.name, v.check, v.max_bounces
        ),
        None => eprintln!("{}: verified-done off", info.name),
    }
    Ok(0)
}
