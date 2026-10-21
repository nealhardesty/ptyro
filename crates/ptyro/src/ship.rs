//! Review and ship from the shell (M6): `wt diff/log/commit`, `pr …` and `review …`. The TUI's
//! inspector (`D`) sends the same requests.

use std::io::IsTerminal;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_client::Client;
use ptyro_core::model::{DiffAgainst, DiffDoc, LineKind, PrInfo, ReviewComment};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};

use crate::cli::{Ctx, age, current_dir, no_daemon, session_of, unexpected};

#[derive(Subcommand)]
pub enum PrCmd {
    /// Push the branch and open a pull request (a draft unless --ready or github.draft = false)
    Create {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long, conflicts_with = "ready")]
        draft: bool,
        #[arg(long)]
        ready: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        /// Target branch (default: the base's branch)
        #[arg(long)]
        base: Option<String>,
    },
    /// The worktree's PR: state, review and checks
    Status {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        /// Ask GitHub now instead of the last poll
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// The PR's checks, one per line
    Checks {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Send the failing checks' logs to the worktree's agent (once its turn is over)
    Fix {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
    },
    /// Merge when the checks pass (`gh pr merge --auto`)
    Automerge {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        /// squash, merge or rebase (default github.merge_method)
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        off: bool,
    },
    /// Feed the PR's new CI failures and review comments to the agent (on by default for PRs
    /// ptyro opened)
    Watch {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        off: bool,
    },
    /// Reply on a review thread (`--comment ID`) or on the PR, marked "via ptyro"
    Reply {
        /// Default: the worktree you're in
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        comment: Option<u64>,
        text: String,
    },
    /// Find the worktree of a PR (number or URL) and attach to its agent
    Open { pr: String },
}

#[derive(Subcommand)]
pub enum ReviewCmd {
    /// Comment on a line of the worktree's diff: `ptyro review add W src/x.rs:42 "rename this"`
    Add {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// FILE:LINE (the line in the new file)
        at: String,
        text: String,
    },
    /// The comments waiting to be sent
    Ls {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Remove a comment, or --all
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        #[arg(required_unless_present = "all")]
        id: Option<u32>,
        #[arg(long)]
        all: bool,
    },
    /// Send the comments to the worktree's agent as one prompt (once its turn is over)
    Send {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        /// To this session instead of the worktree's newest agent
        #[arg(long)]
        to: Option<String>,
        /// A line before the comments ("Mostly fine, but:")
        #[arg(long)]
        note: Option<String>,
    },
    /// Worktrees ready for review, best first
    Queue {
        #[arg(long)]
        json: bool,
    },
    /// Mark the worktree reviewed (it leaves the queue until its agent's next turn)
    Done {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
    },
}

fn here(w: Option<String>) -> Result<String, Error> {
    match w {
        Some(w) => Ok(w),
        None => Ok(current_dir()?.to_string_lossy().into_owned()),
    }
}

fn json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// A diff as `git diff` would print it, coloured on a terminal.
pub fn render_diff(doc: &DiffDoc, color: bool) -> String {
    let (g, r, c, b, z) = if color {
        ("\x1b[32m", "\x1b[31m", "\x1b[36m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let mut out = String::new();
    for f in &doc.files {
        let from = f.old_path.as_deref().unwrap_or(&f.path);
        out.push_str(&format!("{b}diff --ptyro a/{from} b/{} ({}){z}\n", f.path, f.status));
        if f.binary {
            out.push_str("Binary file\n");
            continue;
        }
        for h in &f.hunks {
            out.push_str(&format!("{c}{}{z}\n", h.header));
            for l in &h.lines {
                let (mark, col) = match l.kind {
                    LineKind::Added => ('+', g),
                    LineKind::Removed => ('-', r),
                    LineKind::Context => (' ', ""),
                };
                out.push_str(&format!(
                    "{col}{mark}{}{}\n",
                    l.text,
                    if col.is_empty() { "" } else { z }
                ));
            }
        }
    }
    if doc.truncated {
        out.push_str("… (too big: the rest is left out)\n");
    }
    out
}

pub async fn diff(
    _ctx: &Ctx,
    c: &Client,
    addr: String,
    against: &str,
    stat: bool,
    as_json: bool,
) -> Result<i32, Error> {
    let against = DiffAgainst::parse(against).unwrap_or_default();
    let Reply::Diff { doc } = c.request(Request::WorktreeDiff { addr, against }).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if as_json {
        json(&doc);
    } else if stat {
        let (mut a, mut d) = (0, 0);
        for f in &doc.files {
            println!(
                "{:>5} {:>5}  {} ({})",
                format!("+{}", f.added),
                format!("-{}", f.removed),
                f.path,
                f.status
            );
            a += f.added;
            d += f.removed;
        }
        println!("{} file(s), +{a} −{d} against {}", doc.files.len(), against.label());
    } else {
        print!("{}", render_diff(&doc, std::io::stdout().is_terminal()));
    }
    Ok(0)
}

pub async fn log(c: &Client, addr: String, as_json: bool) -> Result<i32, Error> {
    let Reply::Commits { commits } = c.request(Request::WorktreeLog { addr }).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if as_json {
        json(&commits);
        return Ok(0);
    }
    if commits.is_empty() {
        eprintln!("no commits beyond the base");
    }
    for k in &commits {
        println!(
            "{} {:<60} +{} −{} {} ago, {}",
            &k.sha[..10.min(k.sha.len())],
            k.subject,
            k.added,
            k.removed,
            age(k.at_ms),
            k.author
        );
    }
    Ok(0)
}

pub async fn commit(
    c: &Client,
    addr: String,
    message: Option<String>,
    all: bool,
    paths: Vec<String>,
    ask_agent: bool,
) -> Result<i32, Error> {
    if ask_agent {
        let Reply::Worktree { info } =
            c.request(Request::WorktreeGet { addr: addr.clone() }).await?
        else {
            return Err(Error::internal("unexpected reply"));
        };
        let Reply::Sessions { sessions } = c.request(Request::List).await? else {
            return Err(Error::internal("unexpected reply"));
        };
        let agent = sessions
            .into_iter()
            .filter(|s| {
                s.worktree.as_deref() == Some(info.id.as_str())
                    && s.agent.is_some()
                    && s.state.is_live()
            })
            .max_by_key(|s| s.created_ms)
            .ok_or_else(|| Error::missing("agent session", format!("in {}", info.qualified())))?;
        c.request(Request::SendText {
            addr: agent.id.full(),
            text: "[via ptyro] Please commit your work now: stage what belongs together, and write a clear commit message (subject line under 72 characters, then why).".into(),
            enter: true,
            wait_ready_ms: Some(300_000),
        })
        .await?;
        eprintln!("asked {} to commit", agent.name);
        return Ok(0);
    }
    let message = message.unwrap_or_default();
    let Reply::Committed { sha } =
        c.request(Request::WorktreeCommit { addr, message, paths, all }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    println!("{}", &sha[..12.min(sha.len())]);
    Ok(0)
}

fn check_line(pr: &PrInfo) -> String {
    let (p, f, w) = pr.tally();
    let mut parts = Vec::new();
    if f > 0 {
        parts.push(format!("✗ {f} failing"));
    }
    if w > 0 {
        parts.push(format!("◌ {w} running"));
    }
    if p > 0 {
        parts.push(format!("✓ {p} passed"));
    }
    if parts.is_empty() { "no checks".into() } else { parts.join(" · ") }
}

pub async fn pr(ctx: &Ctx, cmd: PrCmd) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let get = |r: Reply| match r {
        Reply::Pr { info } => Ok(info),
        other => Err(unexpected(other)),
    };
    match cmd {
        PrCmd::Create { worktree, draft, ready, title, body, base } => {
            let draft = if ready {
                false
            } else {
                draft
                    || ptyro_core::config::Config::load(&ptyro_core::config::Config::path(
                        &ctx.paths,
                    ))
                    .unwrap_or_default()
                    .github
                    .draft
            };
            let info = get(c
                .request(Request::PrCreate { addr: here(worktree)?, draft, title, body, base })
                .await?)?;
            if let Some(p) = info {
                println!("{}", p.url);
                eprintln!(
                    "PR #{}{}; ptyro watches its checks and review comments",
                    p.number,
                    if p.draft { " (draft)" } else { "" }
                );
            }
            Ok(0)
        }
        PrCmd::Status { worktree, refresh, json: j } => {
            let info = get(c.request(Request::PrGet { addr: here(worktree)?, refresh }).await?)?;
            if j {
                json(&info);
                return Ok(0);
            }
            match info {
                Some(p) => {
                    println!(
                        "#{} {} [{}{}]",
                        p.number,
                        p.title,
                        p.state.to_lowercase(),
                        if p.draft { ", draft" } else { "" }
                    );
                    println!("  {}", p.url);
                    println!("  checks: {}", check_line(&p));
                    if let Some(r) = &p.review {
                        println!("  review: {}", r.to_lowercase().replace('_', " "));
                    }
                    if p.automerge {
                        println!("  merges automatically when the checks pass");
                    }
                    Ok(0)
                }
                None => Err(Error::new(
                    ErrorCode::NotFound,
                    "no pull request for this worktree's branch (`ptyro pr create`)",
                )),
            }
        }
        PrCmd::Checks { worktree, json: j } => {
            let info = get(c
                .request(Request::PrGet { addr: here(worktree)?, refresh: true })
                .await?)?
            .ok_or_else(|| {
                Error::new(ErrorCode::NotFound, "no pull request for this worktree's branch")
            })?;
            if j {
                json(&info.checks);
            } else {
                for k in &info.checks {
                    let mark = match k.bucket.as_str() {
                        "pass" => "✓",
                        "fail" | "cancel" => "✗",
                        "pending" => "◌",
                        _ => "·",
                    };
                    println!("{mark} {:<40} {}", k.name, k.link.as_deref().unwrap_or(""));
                }
            }
            let (_, failed, pending) = info.tally();
            Ok(if failed > 0 {
                1
            } else if pending > 0 {
                ErrorCode::Timeout.exit_code()
            } else {
                0
            })
        }
        PrCmd::Fix { worktree } => {
            notes(c.request(Request::PrFix { addr: here(worktree)? }).await?)
        }
        PrCmd::Automerge { worktree, method, off } => {
            let info = get(c
                .request(Request::PrAutomerge { addr: here(worktree)?, method, off })
                .await?)?;
            if let Some(p) = info {
                eprintln!(
                    "PR #{}: {}",
                    p.number,
                    if off { "auto-merge off" } else { "merges when the checks pass" }
                );
            }
            Ok(0)
        }
        PrCmd::Watch { worktree, off } => {
            c.request(Request::PrWatch { addr: here(worktree)?, off }).await?;
            eprintln!(
                "{}",
                if off {
                    "not watching"
                } else {
                    "watching: new CI failures and review comments go to the agent once its turn is over"
                }
            );
            Ok(0)
        }
        PrCmd::Reply { worktree, comment, text } => {
            c.request(Request::PrReply { addr: here(worktree)?, comment, text }).await?;
            Ok(0)
        }
        PrCmd::Open { pr } => {
            let n = crate::parse_number(&pr).map_err(Error::invalid)?;
            let Reply::Worktrees { worktrees } =
                c.request(Request::WorktreeList { project: None }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            let wt = worktrees
                .into_iter()
                .find(|w| {
                    w.pr.as_ref().is_some_and(|p| {
                        p.number == n || p.url.trim_end_matches('/') == pr.trim_end_matches('/')
                    })
                })
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::NotFound,
                        format!("no worktree has PR #{n} (`ptyro wt new --pr {n}` checks it out)"),
                    )
                })?;
            let Reply::Sessions { sessions } = c.request(Request::List).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            let s = sessions
                .into_iter()
                .filter(|s| s.worktree.as_deref() == Some(wt.id.as_str()))
                .max_by_key(|s| (s.agent.is_some(), s.created_ms));
            match s {
                Some(s) if std::io::stdin().is_terminal() => {
                    let info = session_of(c.request(Request::Get { addr: s.id.full() }).await?)?;
                    crate::cli::attach(ctx, c, Some(info), false, false).await
                }
                Some(s) => {
                    println!("{}", s.name);
                    Ok(0)
                }
                None => {
                    println!("{}", wt.qualified());
                    eprintln!(
                        "no session in {} (start one with `ptyro new --wt {}`)",
                        wt.qualified(),
                        wt.qualified()
                    );
                    Ok(0)
                }
            }
        }
    }
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

fn comments_of(r: Reply) -> Result<Vec<ReviewComment>, Error> {
    match r {
        Reply::Comments { comments } => Ok(comments),
        other => Err(unexpected(other)),
    }
}

pub async fn review(ctx: &Ctx, cmd: ReviewCmd) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    match cmd {
        ReviewCmd::Add { worktree, at, text } => {
            let (path, line) = at
                .rsplit_once(':')
                .and_then(|(p, l)| Some((p.to_owned(), l.parse::<u32>().ok()?)))
                .ok_or_else(|| Error::invalid(format!("{at:?}: expected FILE:LINE")))?;
            // The line's text, so the agent sees what was meant.
            let code = match c
                .request(Request::WorktreeDiff {
                    addr: worktree.clone(),
                    against: DiffAgainst::Base,
                })
                .await
            {
                Ok(Reply::Diff { doc }) => doc
                    .files
                    .iter()
                    .filter(|f| f.path == path)
                    .flat_map(|f| f.hunks.iter().flat_map(|h| h.lines.iter()))
                    .find(|l| l.new == Some(line))
                    .map(|l| l.text.clone()),
                _ => None,
            };
            let comments = comments_of(
                c.request(Request::ReviewAdd { addr: worktree, path, line, code, text }).await?,
            )?;
            eprintln!(
                "{} comment(s) waiting; `ptyro review send` sends them to the agent",
                comments.len()
            );
            Ok(0)
        }
        ReviewCmd::Ls { worktree, json: j } => {
            let comments =
                comments_of(c.request(Request::ReviewList { addr: here(worktree)? }).await?)?;
            if j {
                json(&comments);
            } else {
                for k in comments {
                    println!("{:>3}. {}:{}  {}", k.id, k.path, k.line, k.text);
                }
            }
            Ok(0)
        }
        ReviewCmd::Rm { worktree, id, all } => {
            let left = comments_of(
                c.request(Request::ReviewRemove {
                    addr: worktree,
                    id: if all { None } else { id },
                })
                .await?,
            )?;
            eprintln!("{} left", left.len());
            Ok(0)
        }
        ReviewCmd::Send { worktree, to, note } => {
            let info = session_of(
                c.request(Request::ReviewSend { addr: here(worktree)?, session: to, note }).await?,
            )?;
            eprintln!("sent to {} (typed once its turn is over)", info.name);
            Ok(0)
        }
        ReviewCmd::Queue { json: j } => {
            let Reply::ReviewQueue { items } = c.request(Request::ReviewQueue).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if j {
                json(&items);
                return Ok(0);
            }
            if items.is_empty() {
                eprintln!("nothing waits for review");
            }
            for (i, it) in items.iter().enumerate() {
                println!(
                    "{:>2}. {:<28} {:>4}  {}",
                    i + 1,
                    it.worktree.qualified(),
                    it.score,
                    it.reasons.join(", ")
                );
                if let Some(m) = &it.last_message {
                    println!("      “{m}”");
                }
            }
            Ok(0)
        }
        ReviewCmd::Done { worktree } => {
            c.request(Request::ReviewDone { addr: worktree }).await?;
            Ok(0)
        }
    }
}
