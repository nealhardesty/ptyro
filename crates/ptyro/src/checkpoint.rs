//! Checkpoints from the shell (SPEC §15.3): `ptyro checkpoint` lists, diffs and restores the
//! per-turn checkpoints of a session's worktree.

use std::io::IsTerminal;

use clap::Subcommand;
use ptyro_core::model::{DiffDoc, LineKind};
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

/// A diff as `git diff` would print it, coloured on a terminal.
fn render_diff(doc: &DiffDoc, color: bool) -> String {
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
                print!("{}", render_diff(&doc, std::io::stdout().is_terminal()));
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
