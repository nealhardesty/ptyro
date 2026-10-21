//! `ptyro cs`: change sets from the shell (SPEC §15.5, M11) — one change across several
//! repositories, with a lead, a contract, a ledger, set checkpoints and a ship train.
//!
//! ```sh
//! ptyro cs new add-user-tier --projects api,sdk,web --prompt "users get a tier: free or pro"
//! ptyro cs contract add-user-tier approve
//! ptyro cs status add-user-tier
//! ptyro cs land add-user-tier
//! ```
//!
//! Inside a set's agents, `$PTYRO_CS` names the set: `ptyro cs ready "$PTYRO_CS"`.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_client::Client;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};
use ptyro_run::changeset::Phase;
use ptyro_run::wire::{ChangeSetInfo, ContractAction, CsNewSpec, CsOp, LedgerEntry, TemplateInfo};
use serde_json::Value;

use crate::cli::{Ctx, age, client_env, confirm, no_daemon, unexpected};

// Parsed once per process: the size of `New` doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum CsCmd {
    /// Start a change set: a worktree per member side by side, a lead that drafts the contract
    New {
        name: String,
        /// Member projects, comma-separated (with --template: extra members)
        #[arg(long, value_delimiter = ',', add = ArgValueCandidates::new(crate::complete::projects))]
        projects: Vec<String>,
        /// A template (~/.config/ptyro/changesets/*.toml, or a trusted hub repo's)
        #[arg(long)]
        template: Option<String>,
        /// The lead's agent (default: your default agent)
        #[arg(long)]
        lead: Option<String>,
        /// lead (one agent per member, the default), solo (one agent for all), manual (none)
        #[arg(long)]
        layout: Option<String>,
        /// The goal, for the lead and every member
        #[arg(short, long, conflicts_with_all = ["prompt_file", "from_issue"])]
        prompt: Option<String>,
        #[arg(long)]
        prompt_file: Option<std::path::PathBuf>,
        /// The goal from a GitHub issue (a number in the first member's repo, or a URL)
        #[arg(long)]
        from_issue: Option<String>,
        /// `sdk:api`, `web:api+sdk` (repeatable); default: from the template, else from manifests
        #[arg(long = "depends", value_name = "MEMBER:DEPS")]
        depends: Vec<String>,
        /// A member's agent: `api=codex` (repeatable)
        #[arg(long = "agent", value_name = "MEMBER=AGENT")]
        agents: Vec<String>,
        /// A member's base: `web=origin/develop` (repeatable)
        #[arg(long = "base", value_name = "MEMBER=REF")]
        bases: Vec<String>,
        /// Where you approve: contract,push,land (default: all three)
        #[arg(long, value_delimiter = ',', conflicts_with = "no_gates")]
        gates: Option<Vec<String>>,
        /// No approvals at all
        #[arg(long)]
        no_gates: bool,
        /// An integration check, run in the set's root (repeatable)
        #[arg(long)]
        integration: Vec<String>,
        /// Build against siblings: turn on the overlays the manifests suggest
        #[arg(long)]
        overlay: bool,
        #[arg(long)]
        workspace: Option<String>,
        /// The fingerprint of a hub template you reviewed
        #[arg(long)]
        trust: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change sets, newest first
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// A set: members, their agents and state, the contract, gates, and what's wrong
    #[command(alias = "status")]
    Show {
        set: String,
        #[arg(long)]
        json: bool,
    },
    /// The ledger: who decided what, when, and why (set events and messages)
    Log {
        set: String,
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        json: bool,
    },
    /// Message a member, the lead (@lead) or everyone (@all); delivered between turns
    Msg {
        set: String,
        to: String,
        text: String,
        /// assign, question, answer, contract_change, handoff, …
        #[arg(long)]
        kind: Option<String>,
        /// Deliver now: stop its turn first
        #[arg(long)]
        interrupt: bool,
    },
    /// Say a member's part works against the contract (wakes members blocked on it)
    Ready {
        set: String,
        note: Option<String>,
        /// For a member other than your own (the human or the lead)
        #[arg(long)]
        member: Option<String>,
    },
    /// Say a member needs a sibling first (it is woken when that one is ready)
    Blocked {
        set: String,
        on: String,
        note: Option<String>,
        #[arg(long)]
        member: Option<String>,
    },
    /// The lead (or you) gives a member a task
    Assign { set: String, member: String, task: String },
    /// Ask the human a question (the lead), answered from the inbox, TUI or phone
    Ask {
        set: String,
        question: String,
        #[arg(long = "option")]
        options: Vec<String>,
    },
    /// The contract: show, edit, diff, history, submit, revise, approve
    Contract {
        set: String,
        #[command(subcommand)]
        cmd: Option<ContractCmd>,
    },
    /// A member's changes against its base
    Diff { set: String, member: String },
    /// Run the integration checks and every member's contract check
    Check {
        set: String,
        #[arg(long)]
        json: bool,
    },
    /// Snapshot every member at once (HEAD plus uncommitted work)
    Checkpoint { set: String, label: Option<String> },
    /// The set's checkpoints
    Checkpoints {
        set: String,
        #[arg(long)]
        json: bool,
    },
    /// Return every member to a set checkpoint
    Restore {
        set: String,
        n: u32,
        /// Even with work since the last checkpoint (it is checkpointed first)
        #[arg(long)]
        force: bool,
    },
    /// Build against siblings' in-progress code: auto (from manifests), go, rust, node, python,
    /// compose, or off
    Overlay {
        set: String,
        #[arg(default_value = "auto")]
        mode: String,
        #[arg(long)]
        member: Option<String>,
    },
    /// Commit every member with changes: one message, Change-Set trailers
    Commit {
        set: String,
        #[arg(short, long)]
        message: String,
    },
    /// Push every member in dependency order (resumes after a failure)
    Push {
        set: String,
        #[arg(short, long)]
        yes: bool,
    },
    /// One PR per member, each with a live table of its siblings
    Pr {
        set: String,
        #[arg(long)]
        draft: bool,
    },
    /// The merge train: each member in order, after its checks, through a gate unless --yes
    Land {
        set: String,
        #[arg(short, long)]
        yes: bool,
        /// Start it and return (follow with `ptyro cs log --follow`)
        #[arg(long)]
        no_wait: bool,
    },
    /// Add a project to the set
    Add {
        set: String,
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
    },
    /// Drop a member (its worktree is removed safely)
    Drop {
        set: String,
        member: String,
        #[arg(long)]
        force: bool,
    },
    /// Remove the set: every member's worktree, safely (one with unshipped work blocks all)
    Rm {
        set: String,
        /// Keep this member's worktree (repeatable)
        #[arg(long)]
        keep: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    /// Change-set templates: yours and trusted hub repositories'
    Templates {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ContractCmd {
    /// A revision (default: the latest; before r1, the draft)
    Show {
        #[arg(long)]
        rev: Option<u32>,
    },
    /// Edit CONTRACT.md in $EDITOR; saving records a revision
    Edit,
    /// Revision A → B (default: the last two), or the latest against FILE
    Diff {
        file: Option<String>,
        #[arg(long)]
        a: Option<u32>,
        #[arg(long)]
        b: Option<u32>,
    },
    History,
    /// Record CONTRACT.md as a revision now (the first goes to your approval)
    Submit {
        #[arg(short, long)]
        message: Option<String>,
    },
    /// A new revision from a file (or a unified diff with --diff)
    Revise {
        file: std::path::PathBuf,
        #[arg(long)]
        diff: bool,
        /// The members it is for (default: from the changed sections' affects: lines)
        #[arg(long, value_delimiter = ',')]
        affects: Vec<String>,
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Approve the revision waiting for you (members start)
    Approve {
        #[arg(long)]
        note: Option<String>,
    },
    /// Send it back to the lead with a note
    Reject {
        note: String,
    },
}

fn kv(items: &[String], sep: char, what: &str) -> Result<BTreeMap<String, String>, Error> {
    items
        .iter()
        .map(|s| {
            s.split_once(sep).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).ok_or_else(
                || Error::invalid(format!("{what}: expected MEMBER{sep}VALUE, got {s:?}")),
            )
        })
        .collect()
}

async fn op(c: &Client, op: CsOp) -> Result<Value, Error> {
    match c.request(Request::Cs { op }).await? {
        Reply::Cs { value } => Ok(value),
        r => Err(unexpected(r)),
    }
}

fn info_of(v: Value) -> Result<ChangeSetInfo, Error> {
    serde_json::from_value(v).map_err(|e| Error::internal(format!("unexpected reply: {e}")))
}

fn json_out(v: &impl serde::Serialize) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn print_notes(v: &Value) {
    if let Some(n) = v["notes"].as_array() {
        for x in n.iter().filter_map(Value::as_str) {
            eprintln!("{x}");
        }
    }
}

fn mark(word: &str) -> &'static str {
    match word {
        "ready" | "landed" => "✓",
        "working" => "●",
        "failed" | "conflict" | "missing" => "✗",
        w if w.starts_with("blocked") => "◐",
        "stale" => "!",
        _ => "·",
    }
}

/// `◆ add-user-tier  working · 2/3 ready · contract r2 (approved r1)`.
pub fn headline(i: &ChangeSetInfo) -> String {
    let c = &i.set;
    let contract = match (c.contract_head(), c.approved()) {
        (None, _) => "no contract yet".to_owned(),
        (Some(h), Some(a)) if a.n == h.n => format!("contract r{}", h.n),
        (Some(h), Some(a)) => format!("contract r{} (approved r{})", h.n, a.n),
        (Some(h), None) => format!("contract r{} (not approved)", h.n),
    };
    format!(
        "◆ {}  {} · {}/{} ready · {contract}{}",
        c.name,
        i.report.state,
        i.report.ready.0,
        i.report.ready.1,
        i.job.as_ref().map(|j| format!(" · {j} running")).unwrap_or_default()
    )
}

fn print_info(i: &ChangeSetInfo) {
    let c = &i.set;
    println!("{}", headline(i));
    println!(
        "  {} layout · profile {} · order {}",
        c.layout.label(),
        c.profile,
        c.order().map(|o| o.join(" → ")).unwrap_or_else(|cy| format!("cycle {}", cy.join("→")))
    );
    if let Some(g) = &c.goal {
        println!("  goal: {}", g.lines().next().unwrap_or(""));
    }
    if let Some(l) = &i.lead {
        println!("  lead   {l}");
    }
    for m in &c.members {
        let word = i.report.members.get(&m.name).cloned().unwrap_or_default();
        let live = i.live.get(&m.name).cloned().unwrap_or_default();
        let mut bits = vec![];
        if live.ahead > 0 {
            bits.push(format!("+{} commit(s)", live.ahead));
        }
        if live.dirty {
            bits.push("uncommitted".into());
        }
        if let Some(p) = m.pr {
            bits.push(format!("PR #{p}"));
        }
        if let Some(o) = &m.overlay {
            bits.push(format!("overlay {o}"));
        }
        if let Some(l) = &m.landed {
            bits.push(l.clone());
        }
        println!(
            "  {} {:<10} {:<16} {:<22} {}{}",
            mark(&word),
            m.name,
            word,
            m.branch,
            i.sessions.get(&m.name).map(|s| format!("[{s}] ")).unwrap_or_default(),
            bits.join(", ")
        );
    }
    for g in &i.gates {
        println!(
            "  ? gate {} ({}): {}  [{}]",
            g.gate.id,
            g.step,
            g.gate.ask,
            g.gate.options.join("/")
        );
    }
    for x in &i.report.issues {
        println!("  ✗ {}: {}\n    → {}", x.state, x.detail, x.action);
    }
    println!("  root {}", c.root);
}

fn print_ledger(entries: &[LedgerEntry]) {
    for e in entries {
        match e.t.as_str() {
            "message" => println!(
                "{:>6} ago  {} → {} [{}] {}",
                age(e.at_ms),
                e.from.clone().unwrap_or_default(),
                e.to.clone().unwrap_or_default(),
                e.kind,
                e.text.lines().next().unwrap_or("").chars().take(120).collect::<String>()
            ),
            _ => println!("{:>6} ago  · {}", age(e.at_ms), e.text),
        }
    }
}

async fn answer_gates(
    c: &Client,
    i: &ChangeSetInfo,
    prefix: &str,
    answered: &mut Vec<String>,
) -> Result<(), Error> {
    for g in i.gates.iter().filter(|g| g.step.starts_with(prefix)) {
        if answered.contains(&g.gate.id) {
            continue;
        }
        answered.push(g.gate.id.clone());
        let yes = g.gate.options.first().cloned().unwrap_or_else(|| "approve".into());
        let no = g.gate.options.last().cloned().unwrap_or_else(|| "stop".into());
        let choice = if confirm(&g.gate.ask) { yes } else { no };
        c.request(Request::GateResolve {
            id: g.gate.id.clone(),
            choice,
            note: None,
            key: None,
            by: Some("cli".into()),
        })
        .await?;
    }
    Ok(())
}

pub async fn cs(ctx: &Ctx, cmd: CsCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        CsCmd::New {
            name,
            projects,
            template,
            lead,
            layout,
            prompt,
            prompt_file,
            from_issue,
            depends,
            agents,
            bases,
            gates,
            no_gates,
            integration,
            overlay,
            workspace,
            trust,
            json,
        } => {
            let goal = match (prompt, prompt_file) {
                (Some(p), _) => Some(p),
                (None, Some(f)) => Some(
                    std::fs::read_to_string(&f)
                        .map_err(|e| Error::invalid(format!("{}: {e}", f.display())))?,
                ),
                _ => None,
            };
            let depends = kv(&depends, ':', "--depends")?
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        v.split(['+', ','])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    )
                })
                .collect();
            let spec = CsNewSpec {
                name,
                projects,
                template,
                lead,
                layout,
                goal,
                from_issue,
                depends,
                agents: kv(&agents, '=', "--agent")?,
                bases: kv(&bases, '=', "--base")?,
                gates: if no_gates { Some(vec![]) } else { gates },
                integration,
                workspace,
                overlay,
                env: client_env(),
                trust,
            };
            let mut v = op(&c, CsOp::New { spec: Box::new(spec.clone()) }).await;
            // A hub template you haven't trusted yet: show it, ask, and go on.
            if let Err(e) = &v
                && e.code == ErrorCode::UntrustedProject
                && std::io::stdin().is_terminal()
                && let Some(fp) = e.message.rsplit("--trust ").next().map(str::to_owned)
            {
                eprintln!("{}", e.message);
                if confirm("Trust this template and go on?") {
                    v = op(&c, CsOp::New { spec: Box::new(CsNewSpec { trust: Some(fp), ..spec }) })
                        .await;
                }
            }
            let i = info_of(v?)?;
            if json {
                json_out(&i);
                return Ok(0);
            }
            print_info(&i);
            match i.set.layout {
                ptyro_run::changeset::Layout::Lead => eprintln!(
                    "\nThe lead is drafting the contract.{} Follow along: ptyro cs log {} --follow",
                    if i.set.gated("contract") {
                        " You approve it with `ptyro cs contract <set> approve` (or from the inbox)."
                    } else {
                        ""
                    },
                    i.set.name
                ),
                _ => eprintln!("\nptyro cs status {} shows where it stands", i.set.name),
            }
            Ok(0)
        }
        CsCmd::Ls { json } => {
            let v: Vec<ChangeSetInfo> = serde_json::from_value(op(&c, CsOp::List).await?)
                .map_err(|e| Error::internal(e.to_string()))?;
            if json {
                json_out(&v);
                return Ok(0);
            }
            if v.is_empty() {
                eprintln!("no change sets — start one with `ptyro cs new NAME --projects a,b`");
            }
            for i in &v {
                println!(
                    "{}  ({}, {} ago)",
                    headline(i),
                    i.status,
                    age(i.set.contract.first().map_or(0, |r| r.at_ms).max(1))
                );
            }
            Ok(0)
        }
        CsCmd::Show { set, json } => {
            let i = info_of(op(&c, CsOp::Show { set }).await?)?;
            if json {
                json_out(&i);
            } else {
                print_info(&i);
            }
            Ok(0)
        }
        CsCmd::Log { set, follow, json } => {
            let mut since: Option<u64> = None;
            loop {
                let entries: Vec<LedgerEntry> =
                    serde_json::from_value(op(&c, CsOp::Log { set: set.clone(), since }).await?)
                        .map_err(|e| Error::internal(e.to_string()))?;
                if json {
                    for e in &entries {
                        println!("{}", serde_json::to_string(e).unwrap_or_default());
                    }
                } else {
                    print_ledger(&entries);
                }
                if !follow {
                    return Ok(0);
                }
                if let Some(s) = entries.iter().map(|e| e.seq).max().filter(|s| *s > 0) {
                    since = Some(s);
                } else if since.is_none() {
                    since = Some(0);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        CsCmd::Msg { set, to, text, kind, interrupt } => {
            let v = op(&c, CsOp::Msg { set, to, text, kind, interrupt }).await?;
            let to: Vec<String> = v["to"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            if to.is_empty() {
                eprintln!("not sent (a repeat, or the queue is full)");
            } else if interrupt {
                eprintln!(
                    "sent to {} (now: a working agent's turn is stopped first)",
                    to.join(", ")
                );
            } else {
                eprintln!("queued for {} (delivered between turns)", to.join(", "));
            }
            Ok(0)
        }
        CsCmd::Ready { set, note, member } => {
            let v = op(&c, CsOp::Ready { set, member, note }).await?;
            let woke: Vec<&str> = v["woke"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            eprintln!(
                "{} is ready (contract r{}){}",
                v["member"].as_str().unwrap_or("?"),
                v["rev"],
                if woke.is_empty() { String::new() } else { format!("; woke {}", woke.join(", ")) }
            );
            Ok(0)
        }
        CsCmd::Blocked { set, on, note, member } => {
            let v = op(&c, CsOp::Blocked { set, on, member, note }).await?;
            if v["woken"] == Value::Bool(true) {
                eprintln!("{} is ready already: carry on", v["on"].as_str().unwrap_or("?"));
            } else {
                eprintln!(
                    "noted: you're woken when {} is ready (the lead knows)",
                    v["on"].as_str().unwrap_or("?")
                );
            }
            Ok(0)
        }
        CsCmd::Assign { set, member, task } => {
            op(&c, CsOp::Assign { set, member: member.clone(), task }).await?;
            eprintln!("assigned to {member} (delivered between its turns)");
            Ok(0)
        }
        CsCmd::Ask { set, question, options } => {
            let v = op(&c, CsOp::Gate { set, question, options }).await?;
            eprintln!(
                "asked the human (gate {}); the answer comes back as a message",
                v["gate"].as_str().unwrap_or("?")
            );
            Ok(0)
        }
        CsCmd::Contract { set, cmd } => {
            contract(&c, set, cmd.unwrap_or(ContractCmd::Show { rev: None })).await
        }
        CsCmd::Diff { set, member } => {
            let v = op(&c, CsOp::Diff { set, member }).await?;
            print!("{}", v["diff"].as_str().unwrap_or_default());
            Ok(0)
        }
        CsCmd::Check { set, json } => {
            eprintln!("running checks…");
            let v = op(&c, CsOp::Check { set }).await?;
            if json {
                json_out(&v);
            } else {
                let show = |r: &Value, label: &str| {
                    let ok = r["ok"] == Value::Bool(true);
                    println!(
                        "{} {label}: {} ({} ms)",
                        if ok { "✓" } else { "✗" },
                        r["command"].as_str().unwrap_or(""),
                        r["duration_ms"]
                    );
                    if !ok {
                        for l in r["tail"]
                            .as_str()
                            .unwrap_or("")
                            .lines()
                            .rev()
                            .take(12)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                        {
                            println!("    {l}");
                        }
                    }
                };
                for r in v["integration"].as_array().into_iter().flatten() {
                    show(r, "integration");
                }
                for (m, r) in v["members"].as_object().into_iter().flatten() {
                    show(r, m);
                }
                if v["integration"].as_array().is_none_or(Vec::is_empty)
                    && v["members"].as_object().is_none_or(|m| m.is_empty())
                {
                    eprintln!(
                        "no checks defined (a template's integration = […] and contract_checks, or `cs new --integration`)"
                    );
                }
            }
            Ok(if v["ok"] == Value::Bool(true) { 0 } else { 1 })
        }
        CsCmd::Checkpoint { set, label } => {
            let v = op(&c, CsOp::Checkpoint { set, label }).await?;
            eprintln!(
                "set checkpoint #{} of {} member(s)",
                v["n"],
                v["members"].as_object().map_or(0, |m| m.len())
            );
            Ok(0)
        }
        CsCmd::Checkpoints { set, json } => {
            let i = info_of(op(&c, CsOp::Show { set }).await?)?;
            if json {
                json_out(&i.set.checkpoints);
                return Ok(0);
            }
            for k in &i.set.checkpoints {
                println!(
                    "#{:<3} {:>6} ago  {:<9} {}",
                    k.n,
                    age(k.at_ms),
                    k.reason,
                    k.label.clone().unwrap_or_default()
                );
            }
            Ok(0)
        }
        CsCmd::Restore { set, n, force } => {
            print_notes(&op(&c, CsOp::Restore { set, n, force }).await?);
            Ok(0)
        }
        CsCmd::Overlay { set, mode, member } => {
            let v = op(&c, CsOp::Overlay { set, member, mode }).await?;
            print_notes(&v);
            if v["notes"].as_array().is_none_or(Vec::is_empty) {
                eprintln!("nothing to wire (no member consumes a sibling in that ecosystem)");
            }
            Ok(0)
        }
        CsCmd::Commit { set, message } => {
            let v = op(&c, CsOp::Commit { set, message }).await?;
            for x in v["committed"].as_array().into_iter().flatten() {
                let sha = x["sha"].as_str().unwrap_or("");
                eprintln!("{} {}", x["member"].as_str().unwrap_or(""), sha.get(..7).unwrap_or(sha));
            }
            Ok(0)
        }
        CsCmd::Push { set, yes } => {
            let i = info_of(op(&c, CsOp::Show { set: set.clone() }).await?)?;
            let mut yes = yes;
            if !yes && i.set.gated("push") && std::io::stdin().is_terminal() {
                let order = i.set.order().unwrap_or_default().join(" → ");
                if !confirm(&format!("Push {} ({order})?", i.set.name)) {
                    return Ok(1);
                }
                yes = true;
            }
            let v = op(&c, CsOp::Push { set, yes }).await?;
            if let Some(g) = v["gate"].as_str() {
                eprintln!(
                    "waiting for your approval: gate {g} (ptyro gate resolve {g} push, the inbox, or your phone)"
                );
                return Ok(0);
            }
            let list = |k: &str| {
                v[k].as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            };
            eprintln!(
                "pushed: {}; up to date: {}",
                list("pushed").if_empty("none"),
                list("up_to_date").if_empty("none")
            );
            Ok(0)
        }
        CsCmd::Pr { set, draft } => {
            let v = op(&c, CsOp::Pr { set, draft }).await?;
            for x in v["opened"].as_array().into_iter().flatten() {
                println!(
                    "{}  #{}  {}",
                    x["member"].as_str().unwrap_or(""),
                    x["number"],
                    x["url"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        CsCmd::Land { set, yes, no_wait } => land(&c, set, yes, no_wait).await,
        CsCmd::Add { set, project, agent, depends_on } => {
            let v = op(&c, CsOp::Add { set, project, agent, depends_on }).await?;
            eprintln!(
                "{} joined ({})",
                v["member"].as_str().unwrap_or(""),
                v["worktree"].as_str().unwrap_or("")
            );
            Ok(0)
        }
        CsCmd::Drop { set, member, force } => {
            op(&c, CsOp::Drop { set, member: member.clone(), force }).await?;
            eprintln!("dropped {member}");
            Ok(0)
        }
        CsCmd::Rm { set, keep, force } => {
            let v = op(&c, CsOp::Rm { set, keep, force }).await?;
            eprintln!(
                "removed {}",
                v["removed"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            );
            Ok(0)
        }
        CsCmd::Templates { json } => {
            let v: Vec<TemplateInfo> = serde_json::from_value(op(&c, CsOp::Templates).await?)
                .map_err(|e| Error::internal(e.to_string()))?;
            if json {
                json_out(&v);
                return Ok(0);
            }
            if v.is_empty() {
                eprintln!(
                    "no templates (~/.config/ptyro/changesets/*.toml, or a repo's .ptyro/changesets/)"
                );
            }
            for t in &v {
                println!(
                    "{:<20} {:<14} {}{}{}",
                    t.name,
                    t.source,
                    t.members.join(", "),
                    t.description.as_ref().map(|d| format!("  — {d}")).unwrap_or_default(),
                    t.error
                        .as_ref()
                        .map(|e| format!("  ✗ {e}"))
                        .or(t.untrusted.as_ref().map(|_| "  (untrusted)".to_owned()))
                        .unwrap_or_default()
                );
            }
            Ok(0)
        }
    }
}

trait IfEmpty {
    fn if_empty(self, v: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, v: &str) -> String {
        if self.is_empty() { v.to_owned() } else { self }
    }
}

async fn contract(c: &Client, set: String, cmd: ContractCmd) -> Result<i32, Error> {
    let action = match cmd {
        ContractCmd::Show { rev } => {
            let v = op(c, CsOp::Contract { set, action: ContractAction::Show { rev } }).await?;
            if v["draft"] == Value::Bool(true) {
                eprintln!("(a draft: no revision recorded yet)");
            } else {
                eprintln!(
                    "r{} by {}{}",
                    v["n"],
                    v["by"].as_str().unwrap_or("?"),
                    v["approved"]
                        .as_str()
                        .map(|a| format!(", approved by {a}"))
                        .unwrap_or_default()
                );
            }
            print!("{}", v["text"].as_str().unwrap_or_default());
            return Ok(0);
        }
        ContractCmd::Edit => {
            let i = info_of(op(c, CsOp::Show { set: set.clone() }).await?)?;
            let path = std::path::Path::new(&i.set.root).join("CONTRACT.md");
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into());
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\""))
                .arg("sh")
                .arg(&path)
                .status()
                .map_err(|e| Error::internal(format!("{editor}: {e}")))?;
            if !status.success() {
                return Err(Error::internal(format!("{editor} exited with {status}")));
            }
            let text =
                std::fs::read_to_string(&path).map_err(|e| Error::internal(e.to_string()))?;
            ContractAction::Revise {
                text: Some(text),
                diff: None,
                affects: vec![],
                summary: Some("edited by the human".into()),
            }
        }
        ContractCmd::Diff { file, a, b } => {
            let v =
                op(c, CsOp::Contract { set, action: ContractAction::Diff { a, b, file } }).await?;
            if v["same"] == Value::Bool(true) {
                eprintln!("no drift: the file matches contract r{}", v["a"]);
                return Ok(0);
            }
            print!("{}", v["diff"].as_str().unwrap_or_default());
            println!();
            return Ok(if v["b"].is_string() { 1 } else { 0 });
        }
        ContractCmd::History => {
            let v = op(c, CsOp::Contract { set, action: ContractAction::History }).await?;
            for r in v.as_array().into_iter().flatten() {
                println!(
                    "r{:<3} {:>6} ago  by {:<8} {}{}{}",
                    r["n"],
                    age(r["at_ms"].as_u64().unwrap_or(0)),
                    r["by"].as_str().unwrap_or(""),
                    r["approved"].as_str().map(|a| format!("approved ({a}) ")).unwrap_or_default(),
                    r["affects"]
                        .as_array()
                        .filter(|a| !a.is_empty())
                        .map(|a| format!(
                            "affects {} ",
                            a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")
                        ))
                        .unwrap_or_default(),
                    r["summary"].as_str().unwrap_or("")
                );
            }
            return Ok(0);
        }
        ContractCmd::Submit { message } => ContractAction::Submit { summary: message },
        ContractCmd::Revise { file, diff, affects, message } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|e| Error::invalid(format!("{}: {e}", file.display())))?;
            if diff {
                ContractAction::Revise { text: None, diff: Some(text), affects, summary: message }
            } else {
                ContractAction::Revise { text: Some(text), diff: None, affects, summary: message }
            }
        }
        ContractCmd::Approve { note } => return resolve_contract(c, &set, "approve", note).await,
        ContractCmd::Reject { note } => {
            return resolve_contract(c, &set, "revise", Some(note)).await;
        }
    };
    let v = op(c, CsOp::Contract { set, action }).await?;
    eprintln!(
        "contract r{}{}{}",
        v["rev"],
        v["affects"]
            .as_array()
            .filter(|a| !a.is_empty())
            .map(|a| format!(
                " (affects {})",
                a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
            ))
            .unwrap_or_default(),
        if v["gate"] == Value::Bool(true) {
            " — waiting for approval: ptyro cs contract <set> approve"
        } else {
            ""
        }
    );
    Ok(0)
}

async fn resolve_contract(
    c: &Client,
    set: &str,
    choice: &str,
    note: Option<String>,
) -> Result<i32, Error> {
    let i = info_of(op(c, CsOp::Show { set: set.to_owned() }).await?)?;
    let g = i.gates.iter().find(|g| g.step.starts_with("contract-r")).ok_or_else(|| {
        Error::conflict(format!(
            "no contract revision of {set} waits for approval (ptyro cs contract {set} history)"
        ))
    })?;
    c.request(Request::GateResolve {
        id: g.gate.id.clone(),
        choice: choice.into(),
        note,
        key: None,
        by: Some("cli".into()),
    })
    .await?;
    eprintln!(
        "{} {}",
        g.step.replace("contract-", "contract "),
        if choice == "approve" {
            "approved: the member agents are starting"
        } else {
            "sent back to the lead"
        }
    );
    Ok(0)
}

/// Start the train and follow it, answering its gates here when there is a terminal.
async fn land(c: &Client, set: String, yes: bool, no_wait: bool) -> Result<i32, Error> {
    let v = op(c, CsOp::Land { set: set.clone(), yes }).await?;
    let todo: Vec<&str> = v["landing"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    eprintln!("landing {} in order", todo.join(" → "));
    if no_wait {
        return Ok(0);
    }
    let tty = std::io::stdin().is_terminal();
    let mut answered = Vec::new();
    let mut since: Option<u64> = None;
    let mut told = Vec::new();
    loop {
        let entries: Vec<LedgerEntry> =
            serde_json::from_value(op(c, CsOp::Log { set: set.clone(), since }).await?)
                .map_err(|e| Error::internal(e.to_string()))?;
        if since.is_some() {
            print_ledger(&entries.iter().filter(|e| e.t == "event").cloned().collect::<Vec<_>>());
        }
        since = entries.iter().map(|e| e.seq).max().or(since).or(Some(0));
        let i = info_of(op(c, CsOp::Show { set: set.clone() }).await?)?;
        if i.set.phase == Phase::Landed {
            eprintln!("{} landed", i.set.name);
            return Ok(0);
        }
        if let Some((m, why)) =
            i.set.train.as_ref().and_then(|t| t.stopped.clone()).filter(|_| i.job.is_none())
        {
            eprintln!("the merge train stopped at {m}: {why}");
            for x in i.report.issues.iter().filter(|x| x.state == "land_stopped") {
                eprintln!("{}\n→ {}", x.detail, x.action);
            }
            return Ok(1);
        }
        if tty {
            answer_gates(c, &i, "land-", &mut answered).await?;
        } else {
            for g in i.gates.iter().filter(|g| g.step.starts_with("land-")) {
                if !told.contains(&g.gate.id) {
                    told.push(g.gate.id.clone());
                    eprintln!(
                        "waiting: gate {} — {} (ptyro gate resolve {} merge)",
                        g.gate.id, g.gate.ask, g.gate.id
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
