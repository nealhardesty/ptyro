//! `ptyro shells` (SPEC §23.7, ROADMAP SH.3): a workspace's SHELLS from any shell. Numbered
//! terminals that belong to the workspace, not a worktree: list them, start one (any command),
//! attach to one by number, renumber, move between SHELLS and worktrees, and watch one. Every
//! session command also takes `%N` (`ptyro send %3 'make test'`).

use clap::{Args, Subcommand};
use clap_complete::ArgValueCandidates;
use ptyro_core::Error;
use ptyro_core::session::{SessionInfo, SpawnSpec, Watch};
use ptyro_protocol::{Reply, Request};
use serde::Serialize;

use crate::cli::{
    Ctx, attach_with, check_attachable, client_env, current_dir, no_daemon, session_of, unexpected,
};
use crate::complete;

/// `shells ls`'s, and plain `shells`'s.
#[derive(Args)]
pub struct LsArgs {
    /// The workspace (default: the one the session you're in belongs to, else the one the TUI
    /// showed last)
    #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
    workspace: Option<String>,
    /// Every workspace's
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
pub enum ShellsCmd {
    /// The workspace's shells, by number (the default)
    Ls(LsArgs),
    /// Start a shell in SHELLS (`-- htop`: any command), in the directory you're in
    New {
        /// The command to run instead of your shell
        #[arg(last = true)]
        command: Vec<String>,
        /// Where it starts (default: here)
        #[arg(short = 'C', long, value_hint = clap::ValueHint::DirPath)]
        dir: Option<std::path::PathBuf>,
        /// The number to give it (refused when taken; default: the lowest free)
        #[arg(long)]
        slot: Option<u32>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// In a new, empty directory of its own, which goes with it
        #[arg(long, conflicts_with = "dir")]
        scratch: bool,
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        /// Attach to it once it starts
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Attach with shell N shown (as `ptyro attach %N`); one lost in a restart starts again
    Go {
        number: u32,
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
    },
    /// Give shell N the number M; a shell already there swaps with it
    Move {
        number: u32,
        to: u32,
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
    },
    /// Move a worktree's session into its workspace's SHELLS: it outlives the worktree
    Loose {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
    },
    /// Move a shell from SHELLS into a worktree (default: the one its directory is in)
    Adopt {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: Option<String>,
    },
    /// Tell you when a shell's command ends, it prints, goes quiet, or shows a line that matches
    Watch {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// When its running command ends, however short (once)
        #[arg(long, group = "what")]
        done: bool,
        /// When new output arrives while nobody looks
        #[arg(long, group = "what")]
        output: bool,
        /// When it has printed nothing for this long (`30s`, `5m`)
        #[arg(long, group = "what", value_name = "TIME")]
        silence: Option<String>,
        /// When a line on its screen matches this pattern (once)
        #[arg(long = "match", group = "what", value_name = "PATTERN")]
        pattern: Option<String>,
        /// Stop watching it
        #[arg(long, group = "what")]
        off: bool,
    },
}

/// One line of `shells ls --json`.
#[derive(Serialize)]
struct Row<'a> {
    slot: u32,
    id: String,
    name: &'a str,
    label: String,
    kind: ptyro_core::session::SessionKind,
    state: String,
    workspace: Option<&'a str>,
    cwd: &'a std::path::Path,
    live_cwd: Option<&'a std::path::Path>,
    command: Option<&'a str>,
    last_exit: Option<i32>,
    watch: Option<String>,
    attention: Option<&'a str>,
    lost: bool,
    scratch: bool,
}

pub async fn run(ctx: &Ctx, cmd: Option<ShellsCmd>, plain: LsArgs) -> Result<i32, Error> {
    match cmd.unwrap_or(ShellsCmd::Ls(plain)) {
        ShellsCmd::Ls(a) => ls(ctx, a).await,
        ShellsCmd::New { command, dir, slot, name, title, scratch, workspace, attach, json } => {
            if attach {
                check_attachable()?;
            }
            let c = ctx.connect(true).await?;
            let workspace = match workspace {
                Some(w) => Some(w),
                None => own_workspace(&c).await,
            };
            let cwd = match dir {
                Some(d) => std::fs::canonicalize(&d)
                    .map_err(|e| Error::invalid(format!("{}: {e}", d.display())))?,
                None => current_dir()?,
            };
            let spec = SpawnSpec {
                argv: command,
                cwd,
                name,
                title,
                env: client_env(),
                workspace,
                loose: true,
                slot,
                scratch,
                ..SpawnSpec::default()
            };
            let info = session_of(c.request(Request::Spawn { spec }).await?)?;
            if attach {
                return attach_with(ctx, c, Some(info), ptyro_tui::Options::default()).await;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
            } else {
                println!("{} {}", info.slot.unwrap_or_default(), info.name);
            }
            Ok(0)
        }
        ShellsCmd::Go { number, workspace } => {
            let c = ctx.connect(true).await?;
            let addr = slot_addr(&c, workspace, number).await;
            let info = session_of(c.request(Request::Get { addr: addr.clone() }).await?)?;
            check_attachable()?;
            if info.lost {
                c.request(Request::Restart { addr }).await?;
            }
            attach_with(ctx, c, Some(info), ptyro_tui::Options::default()).await
        }
        ShellsCmd::Move { number, to, workspace } => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let addr = slot_addr(&c, workspace, number).await;
            let info = session_of(c.request(Request::ShellMove { addr, slot: to }).await?)?;
            println!("{} is shell {to} now", info.name);
            Ok(0)
        }
        ShellsCmd::Loose { session } => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let req = Request::ShellPlace { addr: session, loose: true, worktree: None };
            let info = session_of(c.request(req).await?)?;
            println!("{} is shell {} now", info.name, info.slot.unwrap_or_default());
            Ok(0)
        }
        ShellsCmd::Adopt { session, worktree } => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let req = Request::ShellPlace { addr: session, loose: false, worktree };
            let info = session_of(c.request(req).await?)?;
            println!("{} joined its worktree", info.name);
            Ok(0)
        }
        ShellsCmd::Watch { session, done, output, silence, pattern, off } => {
            let watch = if off {
                None
            } else if let Some(t) = silence {
                Some(Watch::parse(&format!("silence {t}")).map_err(Error::invalid)?)
            } else if let Some(re) = pattern {
                Some(Watch::Match { re })
            } else if output {
                Some(Watch::Output)
            } else {
                // `done` is what a bare `watch` means.
                let _ = done;
                Some(Watch::Done)
            };
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let label = watch.as_ref().map(Watch::label);
            c.request(Request::ShellWatch { addr: session, watch }).await?;
            match label {
                Some(l) => println!("watching: {l}"),
                None => println!("not watched"),
            }
            Ok(0)
        }
    }
}

async fn ls(ctx: &Ctx, a: LsArgs) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let ws = match (&a.workspace, a.all) {
        (_, true) => None,
        (Some(w), _) => Some(w.clone()),
        (None, false) => Some(context_workspace(&c).await),
    };
    let sessions = match c.request(Request::List).await? {
        Reply::Sessions { sessions } => sessions,
        r => return Err(unexpected(r)),
    };
    let mut shells: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|s| s.loose && s.slot.is_some())
        .filter(|s| ws.as_deref().is_none_or(|w| s.workspace.as_deref() == Some(w)))
        .collect();
    shells.sort_by_key(|s| (s.workspace.clone(), s.slot));
    if a.json {
        let rows: Vec<Row<'_>> = shells
            .iter()
            .map(|s| Row {
                slot: s.slot.unwrap_or_default(),
                id: s.id.full(),
                name: &s.name,
                label: label(s),
                kind: s.kind,
                state: s.state.label(),
                workspace: s.workspace.as_deref(),
                cwd: &s.cwd,
                live_cwd: s.live_cwd.as_deref(),
                command: s.command.as_ref().and_then(|c| c.text.as_deref()),
                last_exit: s.last_command_exit,
                watch: s.watch.as_ref().map(Watch::label),
                attention: s.attention.as_ref().map(|a| a.text.as_str()),
                lost: s.lost,
                scratch: s.scratch,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
        return Ok(0);
    }
    if shells.is_empty() {
        let here = ws.as_deref().unwrap_or("any workspace");
        println!("no shells in {here} yet: `ptyro shells new` starts one (Alt-1 in the TUI)");
        return Ok(0);
    }
    for s in shells {
        let mut line = format!("{:>2}  {}", s.slot.unwrap_or_default(), label(s));
        if a.all {
            line = format!("{:<10} {line}", s.workspace.as_deref().unwrap_or("main"));
        }
        let state = if s.lost {
            "↻ lost in a restart (`ptyro shells go` starts it again)".to_owned()
        } else if let Some(at) = &s.attention {
            format!("◉ {}", at.text)
        } else if let Some(c) = s.command.as_ref() {
            match (c.ended_ms, c.exit) {
                (None, _) => format!("● {}", c.text.as_deref().unwrap_or("running")),
                (Some(_), Some(0) | None) => format!("✓ {}", c.text.as_deref().unwrap_or("")),
                (Some(_), Some(e)) => format!("✗{e} {}", c.text.as_deref().unwrap_or("")),
            }
        } else if !s.state.is_live() {
            s.state.label()
        } else {
            String::new()
        };
        if !state.is_empty() {
            line.push_str("  ");
            line.push_str(state.trim_end());
        }
        if let Some(w) = &s.watch {
            line.push_str(&format!("  ◎ {}", w.label()));
        }
        println!("{line}");
    }
    Ok(0)
}

/// A shell's label (SPEC §23.4): `ptyro_core::session::shell_label`.
fn label(s: &SessionInfo) -> String {
    let home = ptyro_core::config::expand_home("~");
    ptyro_core::session::shell_label(s, Some(&home))
}

/// The workspace of the session this command runs in, when it runs in one.
async fn own_workspace(c: &ptyro_client::Client) -> Option<String> {
    let id = std::env::var("PTYRO_SESSION_ID").ok()?;
    match c.request(Request::Get { addr: id }).await {
        Ok(Reply::Session { info }) => info.workspace,
        _ => None,
    }
}

/// What a bare `%N` means here (SPEC §23.5): the workspace of the session this runs in, else
/// the one the TUI showed last, else `main`.
async fn context_workspace(c: &ptyro_client::Client) -> String {
    if let Some(w) = own_workspace(c).await {
        return w;
    }
    match c.request(Request::LoadView { name: "_tui".into() }).await {
        Ok(Reply::View { view: Some(v) }) => {
            v["workspace"].as_str().map_or_else(|| "main".into(), str::to_owned)
        }
        _ => "main".into(),
    }
}

/// `ws%N`, for the asked workspace, else the context's.
async fn slot_addr(c: &ptyro_client::Client, workspace: Option<String>, n: u32) -> String {
    let ws = match workspace {
        Some(w) => w,
        None => context_workspace(c).await,
    };
    format!("{ws}%{n}")
}
