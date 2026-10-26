//! `ptyro scratchpad` (NOTE1.0, NOTE1.7): the notes files, from the command line. It edits in
//! your editor, here; prints and adds to a pad; lists its `- [ ]` items and ticks or starts them;
//! and brings back old copies. A pad is plain Markdown and `$PTYRO_SCRATCHPAD` (everywhere) and
//! `$PTYRO_PROJECT_SCRATCHPAD` (the project's) point at them, so none of this is needed to read
//! one. Which pad: `--global`, `--project`, else the project you are in, else the global one.

use std::io::{IsTerminal, Read};

use clap::Subcommand;
use ptyro_core::Error;
use ptyro_core::scratchpad::{self as pad, PadSel};
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

/// Which pad the flags ask for.
pub struct Which {
    /// `--global`
    pub everywhere: bool,
    /// `--project` (empty: the one you are in) or `--project=NAME`
    pub project: Option<String>,
}

impl Which {
    fn sel(&self) -> Result<PadSel, Error> {
        Ok(match (&self.project, self.everywhere) {
            (_, true) => PadSel::Global,
            (Some(name), _) if !name.is_empty() => PadSel::Project(name.clone()),
            (Some(_), _) => PadSel::Project(current_dir()?.to_string_lossy().into_owned()),
            // Follow where you are: the project's pad inside one, else the global one.
            _ => PadSel::Near(current_dir()?),
        })
    }
}

/// "the global pad", "the ptyro project pad": what a command says it used.
pub fn label(doc: &ptyro_protocol::PadDoc) -> String {
    match doc.pad {
        PadSel::Project(_) => format!("the {} project pad", doc.name),
        _ => "the global pad (everywhere)".into(),
    }
}

/// An explicit `--project` outside any project says what to do instead.
fn pad_err(sel: &PadSel, e: Error) -> Error {
    match sel {
        PadSel::Project(_) if e.code == ptyro_core::ErrorCode::NotFound => Error::invalid(format!(
            "{e}: not inside a project ptyro knows. Run it from the project's directory or one of \
             its worktrees, name one with `--project=NAME`, or use `--global`"
        )),
        _ => e,
    }
}

pub async fn run(
    ctx: &Ctx,
    cmd: Option<PadCmd>,
    which: Which,
    pane: bool,
    json: bool,
) -> Result<i32, Error> {
    let sel = which.sel()?;
    if pane {
        if cmd.is_some() {
            return Err(Error::invalid(
                "--pane opens the editor as a session; it takes no subcommand",
            ));
        }
        return open_pane(ctx, &sel, json).await;
    }
    let r = match cmd.unwrap_or(PadCmd::Edit) {
        PadCmd::Edit => edit(ctx, &sel).await,
        PadCmd::Path => path(ctx, &sel).await,
        PadCmd::Cat { tail, json } => cat(ctx, &sel, tail, json).await,
        PadCmd::Add { text, section, note, no_stamp, json } => {
            add(ctx, &sel, text, section, note, no_stamp, json).await
        }
        PadCmd::Todo { all, json } => todo(ctx, &sel, all, json).await,
        PadCmd::Done { item, undo } => done(ctx, &sel, &item, undo).await,
        PadCmd::Start { item, agent, dry_run } => start(ctx, &sel, &item, agent, dry_run).await,
        PadCmd::Log { json } => log(ctx, &sel, json).await,
        PadCmd::Restore { rev } => restore(ctx, &sel, &rev).await,
    };
    r.map_err(|e| pad_err(&sel, e))
}

fn config(ctx: &Ctx) -> ptyro_core::config::Config {
    ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&ctx.paths))
        .unwrap_or_default()
}

/// Print which pad a command used, for people (not in the output a script reads).
fn say(doc: &ptyro_protocol::PadDoc) {
    if std::io::stdout().is_terminal() {
        eprintln!("({}: {})", label(doc), doc.path.display());
    }
}

async fn path(ctx: &Ctx, sel: &PadSel) -> Result<i32, Error> {
    let d = doc(ctx, sel).await?;
    println!("{}", d.path.display());
    say(&d);
    Ok(0)
}

/// Open the file in the editor, in this terminal, and wait for it.
async fn edit(ctx: &Ctx, sel: &PadSel) -> Result<i32, Error> {
    let d = doc(ctx, sel).await?;
    let command = config(ctx).scratchpad_editor(false).ok_or_else(|| {
        Error::invalid(
            "no editor found: set $EDITOR, or [scratchpad] editor in `ptyro config edit`",
        )
    })?;
    eprintln!("editing {}: {}", label(&d), d.path.display());
    let argv = pad::editor_argv(&command, &d.path);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::invalid(format!("{}: {e}", argv[0])))?;
    Ok(status.code().unwrap_or(1))
}

async fn open_pane(ctx: &Ctx, sel: &PadSel, json: bool) -> Result<i32, Error> {
    if !json {
        crate::cli::check_attachable()?;
    }
    let client = ctx.connect(true).await?;
    let req = Request::ScratchPane {
        pad: sel.clone(),
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

async fn doc(ctx: &Ctx, sel: &PadSel) -> Result<ptyro_protocol::PadDoc, Error> {
    let c = ctx.connect(true).await?;
    match c.request(Request::ScratchGet { pad: sel.clone() }).await? {
        Reply::Scratch { doc } => Ok(doc),
        r => Err(unexpected(r)),
    }
}

async fn cat(ctx: &Ctx, sel: &PadSel, tail: Option<usize>, json: bool) -> Result<i32, Error> {
    let d = doc(ctx, sel).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&d).unwrap_or_default());
        return Ok(0);
    }
    say(&d);
    match tail {
        Some(n) => {
            let lines: Vec<&str> = d.text.lines().collect();
            for l in &lines[lines.len().saturating_sub(n)..] {
                println!("{l}");
            }
        }
        None => print!("{}", d.text),
    }
    Ok(0)
}

async fn add(
    ctx: &Ctx,
    sel: &PadSel,
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
        pad: sel.clone(),
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
        eprintln!("added to {} ({}, {} open)", label(&doc), doc.path.display(), doc.open);
    }
    Ok(0)
}

/// A pad as named before the daemon has resolved it.
fn sel_label(sel: &PadSel) -> &'static str {
    match sel {
        PadSel::Global => "the global pad",
        _ => "this project's pad",
    }
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

async fn todo(ctx: &Ctx, sel: &PadSel, all: bool, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::ScratchItems { rev, items } =
        c.request(Request::ScratchTodo { pad: sel.clone(), all }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::json!({ "rev": rev, "items": items }));
        return Ok(0);
    }
    if items.is_empty() {
        eprintln!("nothing open in {} (`ptyro scratchpad add \"…\"`)", sel_label(sel));
    }
    for i in &items {
        println!("{}", show(i));
    }
    Ok(0)
}

async fn done(ctx: &Ctx, sel: &PadSel, item: &str, undo: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mark = if undo { PadMark::Open } else { PadMark::Done { pr: None } };
    let Reply::ScratchItems { items, .. } =
        c.request(Request::ScratchMark { pad: sel.clone(), sel: item.to_owned(), mark }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    for i in &items {
        println!("{}", show(i));
    }
    Ok(0)
}

async fn start(
    ctx: &Ctx,
    sel: &PadSel,
    item: &str,
    agent: Option<String>,
    dry_run: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::Scratch { doc } = c.request(Request::ScratchGet { pad: sel.clone() }).await? else {
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
    crate::cli::start_pad_task(ctx, &prompt, agent, it.line, doc.pad).await
}

async fn log(ctx: &Ctx, sel: &PadSel, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::ScratchLog { snapshots } =
        c.request(Request::ScratchHistory { pad: sel.clone() }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshots).unwrap_or_default());
        return Ok(0);
    }
    for s in &snapshots {
        println!("{}  {:>5} ago  {} lines", s.rev, age(s.at_ms), s.lines);
    }
    if snapshots.is_empty() {
        eprintln!("no saved copies of {} yet", sel_label(sel));
    }
    Ok(0)
}

async fn restore(ctx: &Ctx, sel: &PadSel, rev: &str) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let Reply::Scratch { doc } =
        c.request(Request::ScratchRestore { pad: sel.clone(), rev: rev.to_owned() }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    eprintln!("restored {} of {} ({} open)", doc.rev, label(&doc), doc.open);
    Ok(0)
}
