//! `ptyro scratchpad` (NOTE1.0): the one notes file, from the command line. It edits in your
//! editor, here; prints and adds to it; lists its `- [ ]` items and ticks or starts them; and
//! brings back old copies. The file is plain Markdown and `$PTYRO_SCRATCHPAD` points at it, so
//! none of this is needed to read it.

use std::io::{IsTerminal, Read};

use clap::Subcommand;
use ptyro_core::Error;
use ptyro_core::config::Config;
use ptyro_core::scratchpad as pad;
use ptyro_protocol::{PadMark, Reply, Request};

use crate::cli::{Ctx, age, current_dir, unexpected};

#[derive(Subcommand)]
pub enum PadCmd {
    /// Open it in your editor, here (the default)
    Edit,
    /// Print where the file is
    Path,
    /// Print it
    Cat {
        /// Only the last N lines
        #[arg(long)]
        tail: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Add a line, stamped with the time and where you are: `ptyro scratchpad add "login
    /// times out on safari"`, or from a pipe: `git diff --stat | ptyro scratchpad add -`
    Add {
        /// The words (`-`, or nothing on a pipe: read standard input)
        text: Vec<String>,
        /// Under this heading (made if it isn't there)
        #[arg(long)]
        section: Option<String>,
        /// A plain bullet, not a `- [ ]` item
        #[arg(long)]
        note: bool,
        /// No time or place on it
        #[arg(long)]
        no_stamp: bool,
        #[arg(long)]
        json: bool,
    },
    /// The open `- [ ]` items, with their line numbers
    Todo {
        /// Ticked ones too
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Tick an item: its line number, or words from one open item
    Done {
        item: String,
        /// Untick it instead
        #[arg(long)]
        undo: bool,
    },
    /// Start an item as a task: a new worktree and your default agent, told what it says
    Start {
        /// Its line number, or words from one open item
        item: String,
        /// The agent (default: yours)
        #[arg(long)]
        agent: Option<String>,
        /// Just print what it would be told
        #[arg(long)]
        dry_run: bool,
    },
    /// Saved copies of the file, newest first
    Log {
        #[arg(long)]
        json: bool,
    },
    /// Put a saved copy back (what's there now is saved first)
    Restore { rev: String },
}

pub async fn run(ctx: &Ctx, cmd: Option<PadCmd>, pane: bool, json: bool) -> Result<i32, Error> {
    if pane {
        if cmd.is_some() {
            return Err(Error::invalid(
                "--pane opens the editor as a session; it takes no subcommand",
            ));
        }
        return open_pane(ctx, json).await;
    }
    match cmd.unwrap_or(PadCmd::Edit) {
        PadCmd::Edit => edit(ctx),
        PadCmd::Path => {
            println!("{}", config(ctx).scratchpad_path(&ctx.paths.data).display());
            Ok(0)
        }
        PadCmd::Cat { tail, json } => cat(ctx, tail, json).await,
        PadCmd::Add { text, section, note, no_stamp, json } => {
            add(ctx, text, section, note, no_stamp, json).await
        }
        PadCmd::Todo { all, json } => todo(ctx, all, json).await,
        PadCmd::Done { item, undo } => done(ctx, &item, undo).await,
        PadCmd::Start { item, agent, dry_run } => start(ctx, &item, agent, dry_run).await,
        PadCmd::Log { json } => log(ctx, json).await,
        PadCmd::Restore { rev } => restore(ctx, &rev).await,
    }
}

fn config(ctx: &Ctx) -> Config {
    Config::load(&Config::path(&ctx.paths)).unwrap_or_default()
}

/// The pad's file and its history, without the daemon.
fn local_pad(ctx: &Ctx, config: &Config) -> pad::Pad {
    pad::Pad::new(
        config.scratchpad_path(&ctx.paths.data),
        ctx.paths.data.join("scratchpad.history"),
        config.scratchpad.keep,
    )
}

/// Open the file in the editor, in this terminal, and wait for it.
fn edit(ctx: &Ctx) -> Result<i32, Error> {
    let config = config(ctx);
    let file = local_pad(ctx, &config);
    file.ensure().map_err(|e| Error::internal(format!("{}: {e}", file.path.display())))?;
    let command = config.scratchpad_editor(false).ok_or_else(|| {
        Error::invalid(
            "no editor found: set $EDITOR, or [scratchpad] editor in `ptyro config edit`",
        )
    })?;
    let argv = pad::editor_argv(&command, &file.path);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::invalid(format!("{}: {e}", argv[0])))?;
    Ok(status.code().unwrap_or(1))
}

async fn open_pane(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    if !json {
        crate::cli::check_attachable()?;
    }
    let client = ctx.connect(true).await?;
    let req = Request::ScratchPane {
        create: true,
        env: crate::cli::client_env(),
        size: None,
        workspace: None,
    };
    let info = crate::cli::session_of(client.request(req).await?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
        return Ok(0);
    }
    crate::cli::attach_with(ctx, client, Some(info), ptyro_tui::Options::default()).await
}

async fn doc(ctx: &Ctx) -> Result<ptyro_protocol::PadDoc, Error> {
    let c = ctx.connect(true).await?;
    match c.request(Request::ScratchGet).await? {
        Reply::Scratch { doc } => Ok(doc),
        r => Err(unexpected(r)),
    }
}

async fn cat(ctx: &Ctx, tail: Option<usize>, json: bool) -> Result<i32, Error> {
    // Plain text needs no daemon; JSON carries the revision, which is the daemon's.
    if json {
        let d = doc(ctx).await?;
        println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
        return Ok(0);
    }
    let config = config(ctx);
    let text =
        local_pad(ctx, &config).read().map_err(|e| Error::internal(format!("scratchpad: {e}")))?;
    match tail {
        Some(n) => {
            let lines: Vec<&str> = text.lines().collect();
            for l in &lines[lines.len().saturating_sub(n)..] {
                println!("{l}");
            }
        }
        None => print!("{text}"),
    }
    Ok(0)
}

async fn add(
    ctx: &Ctx,
    words: Vec<String>,
    section: Option<String>,
    note: bool,
    no_stamp: bool,
    json: bool,
) -> Result<i32, Error> {
    let from_pipe =
        words.as_slice() == ["-"] || (words.is_empty() && !std::io::stdin().is_terminal());
    let text = if from_pipe {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Error::invalid(format!("standard input: {e}")))?;
        s
    } else {
        words.join(" ")
    };
    if text.trim().is_empty() {
        return Err(Error::invalid(
            "nothing to add: ptyro scratchpad add \"login times out on safari\" (or pipe text in with `-`)",
        ));
    }
    let c = ctx.connect(true).await?;
    let req = Request::ScratchAdd {
        text,
        section,
        note,
        stamp: !no_stamp,
        session: std::env::var("PTYRO_SESSION_ID").ok(),
        cwd: current_dir().ok(),
    };
    let Reply::Scratch { doc } = c.request(req).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        eprintln!("added to {} ({} open)", doc.path.display(), doc.open);
    }
    Ok(0)
}

fn show(i: &pad::Item) -> String {
    let mut out = format!("{:>5}  {}{}", i.line, if i.done { "✓ " } else { "" }, i.text);
    let mut tags = Vec::new();
    if let Some(s) = &i.section {
        tags.push(s.clone());
    }
    if let Some(w) = &i.worktree {
        tags.push(format!("→ {w}"));
    }
    if let Some(n) = i.pr {
        tags.push(format!("#{n}"));
    }
    if let Some(b) = &i.by {
        tags.push(format!("by {b}"));
    }
    if let Some(a) = &i.at {
        tags.push(a.clone());
    }
    if !tags.is_empty() {
        out.push_str(&format!("   · {}", tags.join(" · ")));
    }
    out
}

async fn todo(ctx: &Ctx, all: bool, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::ScratchItems { rev, items } = c.request(Request::ScratchTodo { all }).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::json!({ "rev": rev, "items": items }));
        return Ok(0);
    }
    if items.is_empty() {
        eprintln!("nothing open (`ptyro scratchpad add \"…\"`)");
    }
    for i in &items {
        println!("{}", show(i));
    }
    Ok(0)
}

async fn done(ctx: &Ctx, item: &str, undo: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mark = if undo { PadMark::Open } else { PadMark::Done { pr: None } };
    let Reply::ScratchItems { items, .. } =
        c.request(Request::ScratchMark { sel: item.to_owned(), mark }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    for i in &items {
        println!("{}", show(i));
    }
    Ok(0)
}

async fn start(ctx: &Ctx, item: &str, agent: Option<String>, dry_run: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::Scratch { doc } = c.request(Request::ScratchGet).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    let it = pad::select(&doc.text, item).map_err(Error::invalid)?;
    if it.done {
        return Err(Error::invalid(format!(
            "line {} is ticked already (`ptyro scratchpad done --undo {}`)",
            it.line, it.line
        )));
    }
    let prompt = pad::body(&doc.text, &it);
    if dry_run {
        println!("{prompt}");
        return Ok(0);
    }
    drop(c);
    crate::cli::start_pad_task(ctx, &prompt, agent, it.line).await
}

async fn log(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::ScratchLog { snapshots } = c.request(Request::ScratchHistory).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshots).unwrap_or_default());
        return Ok(0);
    }
    let now = ptyro_core::now_ms();
    for s in &snapshots {
        println!("{}  {:>8} ago  {} lines", s.rev, age(now.saturating_sub(s.at_ms)), s.lines);
    }
    if snapshots.is_empty() {
        eprintln!("no saved copies yet");
    }
    Ok(0)
}

async fn restore(ctx: &Ctx, rev: &str) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::Scratch { doc } = c.request(Request::ScratchRestore { rev: rev.to_owned() }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    eprintln!("restored {} ({} open)", doc.rev, doc.open);
    Ok(0)
}
