//! Checkpoints and verified-done from the shell (SPEC §15.3, M7.9): `ptyro checkpoint` lists,
//! diffs and restores the per-turn checkpoints of a session's worktree; `ptyro verify` sets the
//! check an agent's turns must pass.

use std::io::IsTerminal;

use clap::Subcommand;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};

use crate::cli::{Ctx, ago, current_dir, no_daemon, unexpected};

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

fn json_out<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn me() -> Option<String> {
    std::env::var("PTYRO_SESSION_ID").ok()
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
                    ago(k.at_ms),
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
