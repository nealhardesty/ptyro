//! `ptyro chat` (SPEC §22.12): plain conversations with your agents, from any shell. A chat is
//! kept (in `chat.db`) and shows in the TUI's and the web's CHATS section too; `ptyro chat ask`
//! is the one-shot that keeps nothing (`git diff | ptyro chat ask "review this"`).

use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

use clap::{Args, Subcommand};
use clap_complete::ArgValueCandidates;
use ptyro_core::chat::{ChatBody, ChatItem};
use ptyro_core::chats::{ChatInfo, ChatNode};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{ClientMsg, Reply, Request, ServerMsg};

use crate::cli::{Ctx, ago, unexpected};
use crate::complete;

/// `chat new`'s, and plain `chat`'s: `ptyro chat --folder ~/notes "what's left?"`.
#[derive(Args)]
pub struct NewArgs {
    /// The first message (`-`, or nothing on a pipe: read standard input). One that starts with
    /// a subcommand's name (`show me how…`, `find`, `set`, `open`…) needs `chat new` or quotes
    prompt: Vec<String>,
    /// `agent` or `agent@profile` (default `[chat] agent`, else the first installed)
    #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
    agent: Option<String>,
    /// The workspace it belongs to (default main)
    #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
    workspace: Option<String>,
    #[arg(long)]
    title: Option<String>,
    /// Keep nothing: not saved, not searchable, gone when its agent stops
    #[arg(long)]
    incognito: bool,
    /// Keep it, though `[chat] incognito` is on
    #[arg(long, conflicts_with = "incognito")]
    keep: bool,
    /// A folder it may read (repeatable)
    #[arg(long = "folder", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    folders: Vec<String>,
    /// Don't wait for the answer
    #[arg(long)]
    no_wait: bool,
    /// Seconds to wait for the answer
    #[arg(long, default_value_t = 600)]
    timeout: u64,
    /// The chat and its answer as JSON, once it is in
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
pub enum ChatCmd {
    /// A new chat (the default): with words, its first message, answered here
    New(NewArgs),
    /// One question, one answer on standard output; nothing is kept unless --keep. Standard
    /// input is context: `git diff | ptyro chat ask "review this"`
    Ask {
        #[arg(required = true)]
        question: Vec<String>,
        /// Keep it as a chat
        #[arg(long)]
        keep: bool,
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// Seconds to wait for the answer
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// The answer as JSON (as `chat new --json`)
        #[arg(long)]
        json: bool,
        /// Don't read standard input as context (in a loop, or under a program that keeps it
        /// open)
        #[arg(long)]
        no_stdin: bool,
    },
    /// Chats, pinned first, then the newest
    #[command(alias = "list")]
    Ls {
        /// One workspace's
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        /// Only pinned ones
        #[arg(long)]
        pinned: bool,
        /// Archived ones instead
        #[arg(long)]
        archived: bool,
        /// Only this machine's (`here` for this one's), when others are linked
        #[arg(long, add = ArgValueCandidates::new(complete::machines))]
        machine: Option<String>,
        /// At most N
        #[arg(short, long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    /// Print a chat
    Show {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// Each message's id (for `edit`)
        #[arg(long)]
        ids: bool,
        /// text (default), md or json
        #[arg(long, default_value = "text")]
        format: String,
        /// (Words after the chat: `ptyro chat show me how…` meant a new chat)
        #[arg(hide = true, trailing_var_arg = true)]
        rest: Vec<String>,
    },
    /// Ask again: a new answer replaces the last one, printed as it arrives. Only the last
    /// answer can be asked again (NODE, when given, is the last message or its answer)
    Retry {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        node: Option<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Say your last message differently (NODE: its id, see `show --ids`): it and its answer
    /// are replaced, answered afresh. Only your last message can be edited
    Edit {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        node: String,
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Search every chat's messages and titles, on every linked machine too: words (all must
    /// match), "a phrase", prefix*, -word, agent:codex, since:7d, workspace:work, by:meaning
    Search {
        /// (A word may start with `-`: it is left out. After `--`, every word is a word.)
        #[arg(required = true, allow_hyphen_values = true)]
        query: Vec<String>,
        /// Only in this chat
        #[arg(long = "in", value_name = "CHAT", add = ArgValueCandidates::new(complete::chats))]
        chat: Option<String>,
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long)]
        pinned: bool,
        /// Archived chats too
        #[arg(long)]
        archived: bool,
        /// This machine only
        #[arg(long)]
        here: bool,
        /// Only messages about it, by meaning (≈), not by their words (`[chat] meaning`)
        #[arg(long, conflicts_with = "words")]
        meaning: bool,
        /// Only messages with the words, even when meaning search is on
        #[arg(long)]
        words: bool,
        /// At most N
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Ask your chats a question: an agent searches them and answers, citing c:7f3k/node
    Find {
        #[arg(required = true)]
        question: Vec<String>,
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// The answer as JSON (as `chat new --json`)
        #[arg(long)]
        json: bool,
    },
    /// Open a chat in the TUI's chat view
    Open {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// (Words after the chat: `ptyro chat open a file…` meant a new chat)
        #[arg(hide = true, trailing_var_arg = true)]
        rest: Vec<String>,
    },
    /// Send a message; the answer is printed as it arrives (unless --no-wait)
    Send {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// The answer as JSON (as `chat new --json`)
        #[arg(long)]
        json: bool,
    },
    /// Answer the chat's permission request: allow, always or deny
    Answer {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        decision: String,
        /// Only that request (its node, `show --ids`): refused if it was answered already
        #[arg(long)]
        node: Option<String>,
    },
    /// Stop the answer being written
    Stop {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
    },
    /// Rename a chat: a chat's name is its title, what people read (it is found by its handle,
    /// `c:7f3k`); `--title ""` clears it, `--auto` asks for a new automatic one
    Rename {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// The new title (several words are joined)
        #[arg(conflicts_with_all = ["auto", "title"], required_unless_present_any = ["auto", "title"])]
        name: Vec<String>,
        /// The new title, as the words do; `--title ""` clears it
        #[arg(long, conflicts_with = "auto")]
        title: Option<String>,
        /// A new automatic title, made from the conversation (`[chat] title`)
        #[arg(long)]
        auto: bool,
        #[arg(long)]
        json: bool,
    },
    /// Keep chats at the top
    Pin {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Unpin {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Put chats away (`chat ls --archived` lists them)
    Archive {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Unarchive {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change the agent or the folders it reads; the next message carries the conversation over
    Set {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// The folders it may read, all of them (repeatable)
        #[arg(long = "folder", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        folders: Vec<String>,
        /// It reads no folder
        #[arg(long, conflicts_with = "folders")]
        no_folders: bool,
        /// Switch to an agent under another profile than the chat's without asking first
        #[arg(short, long)]
        yes: bool,
        #[arg(long)]
        json: bool,
        /// (Words after the chat: `ptyro chat set up a…` meant a new chat)
        #[arg(hide = true, trailing_var_arg = true)]
        rest: Vec<String>,
    },
    /// Make it a task: an agent session in a new worktree of a project, starting from the chat's
    /// conversation and its last answer
    Task {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// The project to work in (default: the only one of the chat's workspace)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// `agent` or `agent@profile` (default: the chat's agent)
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// The worktree's name (default: from the chat's title)
        #[arg(short, long)]
        name: Option<String>,
        /// Go ahead when the task's profile isn't the chat's (or a profile guard asks)
        #[arg(short, long)]
        yes: bool,
        /// Don't attach to its agent; print the worktree instead (in a terminal it attaches)
        #[arg(short, long)]
        detach: bool,
        /// Print the worktree and its session as JSON (doesn't attach)
        #[arg(long)]
        json: bool,
    },
    /// Delete chats for good: each by its handle, id or @last (never a title). Asks first
    /// unless --yes
    #[command(alias = "delete")]
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
    },
    /// Write a chat out as Markdown or JSON
    Export {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// md (default) or json
        #[arg(long, default_value = "md")]
        format: String,
        /// To this file instead of standard output
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
}

pub async fn run(ctx: &Ctx, cmd: Option<ChatCmd>, plain: NewArgs) -> Result<i32, Error> {
    if cmd.is_none() {
        removed(&plain.prompt)?;
    }
    match cmd.unwrap_or(ChatCmd::New(plain)) {
        ChatCmd::New(NewArgs {
            prompt,
            agent,
            workspace,
            title,
            incognito,
            keep,
            folders,
            no_wait,
            timeout,
            json,
        }) => {
            let text = words_or_stdin(prompt)?;
            // Neither: as `[chat] incognito` says.
            let incognito = (incognito || keep).then_some(incognito);
            let folders = folders.iter().map(|f| full_path(f)).collect();
            let wait = Duration::from_secs(timeout);
            new(ctx, text, agent, workspace, title, incognito, folders, no_wait, wait, json).await
        }
        ChatCmd::Retry { chat, node, no_wait, timeout, json } => {
            let c = ctx.connect(true).await?;
            let known = known_nodes(&c, &chat).await?;
            let info = chat_of(c.request(Request::ChatRetry { chat, node }).await?)?;
            // The answer is a new one to a message already there: follow from its end.
            let wait = Duration::from_secs(timeout);
            answer(ctx, c, &info, &known, wait, true, no_wait, json).await
        }
        ChatCmd::Edit { chat, node, text, no_wait, timeout, json } => {
            let c = ctx.connect(true).await?;
            let known = known_nodes(&c, &chat).await?;
            let info =
                chat_of(c.request(Request::ChatEdit { chat, node, text: text.join(" ") }).await?)?;
            answer(ctx, c, &info, &known, Duration::from_secs(timeout), false, no_wait, json).await
        }
        ChatCmd::Search {
            query,
            chat,
            workspace,
            agent,
            pinned,
            archived,
            here,
            meaning,
            words,
            limit,
            json,
        } => {
            // (Words may start with `-`, so the flags typed after them arrive as words.)
            let mut a = SearchArgs {
                chat,
                workspace,
                agent,
                pinned,
                archived,
                here,
                by: (meaning.then_some("meaning")).or(words.then_some("words")),
                limit,
                json,
            };
            let mut words = a.take_flags(&query)?.join(" ");
            if let Some(by) = a.by {
                words.push_str(&format!(" by:{by}"));
            }
            search(
                ctx,
                words,
                a.chat,
                a.workspace,
                a.agent,
                a.pinned,
                a.archived,
                !a.here,
                a.limit,
                a.json,
            )
            .await
        }
        ChatCmd::Find { question, agent, timeout, json } => {
            let c = ctx.connect(true).await?;
            let req = Request::ChatFind { question: question.join(" "), agent, workspace: None };
            let info = chat_of(c.request(req).await?)?;
            let none = HashSet::new();
            let out = if json { Out::Quiet } else { Out::Final };
            let answered = tokio::select! {
                r = follow(c, &info, &none, Duration::from_secs(timeout), out, false) => r,
                _ = tokio::signal::ctrl_c() => Err(Error::new(ErrorCode::Invalid, "interrupted")),
            };
            if json && let Ok(a) = &answered {
                print_answer(ctx, &info, a).await;
            }
            // (It is incognito: it goes with its agent; this makes sure.)
            if let Ok(c) = ctx.connect(false).await {
                let _ =
                    c.request(Request::ChatDelete { chat: info.id.clone(), confirmed: true }).await;
            }
            answered.map(|a| if a.ok { 0 } else { 1 })
        }
        ChatCmd::Ask { question, keep, agent, timeout, json, no_stdin } => {
            let context = if no_stdin || std::io::stdin().is_terminal() {
                String::new()
            } else {
                read_stdin()?
            };
            let question = with_context(question.join(" "), &context);
            ask(ctx, question, keep, agent, timeout, json).await
        }
        ChatCmd::Ls { workspace, pinned, archived, machine, limit, json } => {
            ls(ctx, workspace, pinned, archived, machine, limit, json).await
        }
        ChatCmd::Show { chat, ids, format, rest } => {
            meant_new("show", &chat, &rest)?;
            show(ctx, chat, ids, &format).await
        }
        ChatCmd::Open { chat, rest } => {
            meant_new("open", &chat, &rest)?;
            open(ctx, &chat).await
        }
        ChatCmd::Send { chat, text, no_wait, timeout, json } => {
            let c = ctx.connect(true).await?;
            // What was there before: the answer is what follows this message, not an earlier one.
            let known = known_nodes(&c, &chat).await?;
            let info = chat_of(c.request(Request::ChatSend { chat, text: text.join(" ") }).await?)?;
            answer(ctx, c, &info, &known, Duration::from_secs(timeout), false, no_wait, json).await
        }
        ChatCmd::Answer { chat, decision, node } => {
            done(ctx, Request::ChatAnswer { chat, decision, node }).await
        }
        ChatCmd::Stop { chat } => done(ctx, Request::ChatStop { chat }).await,
        ChatCmd::Rename { chat, name, title, auto, json } => {
            let title = title.unwrap_or_else(|| name.join(" "));
            update_as(ctx, &chat, false, json, |r| {
                if let Request::ChatUpdate { title: t, auto_title, .. } = r {
                    *t = Some(title);
                    *auto_title = auto;
                }
            })
            .await
        }
        ChatCmd::Pin { chats, json } => {
            each(ctx, chats, "pinned", json, |r| set_pinned(r, true)).await
        }
        ChatCmd::Unpin { chats, json } => {
            each(ctx, chats, "unpinned", json, |r| set_pinned(r, false)).await
        }
        ChatCmd::Archive { chats, json } => {
            each(ctx, chats, "archived", json, |r| set_archived(r, true)).await
        }
        ChatCmd::Unarchive { chats, json } => {
            each(ctx, chats, "back from the archive", json, |r| set_archived(r, false)).await
        }
        ChatCmd::Set { chat, agent, folders, no_folders, yes, json, rest } => {
            meant_new("set", &chat, &rest)?;
            if agent.is_none() && folders.is_empty() && !no_folders {
                return Err(Error::invalid(
                    "what should change? --agent A, --folder DIR or --no-folders",
                ));
            }
            let folders = (no_folders || !folders.is_empty())
                .then(|| folders.iter().map(|f| full_path(f)).collect());
            update_as(ctx, &chat, yes, json, |r| {
                if let Request::ChatUpdate { agent: a, folders: f, .. } = r {
                    *a = agent;
                    *f = folders;
                }
            })
            .await
        }
        ChatCmd::Task { chat, project, agent, name, yes, detach, json } => {
            task(ctx, chat, project, agent, name, yes, detach || json, json).await
        }
        ChatCmd::Rm { chats, yes } => {
            let c = ctx.connect(true).await?;
            for chat in chats {
                // An exact address only (never a title), asked first (SPEC §12.1). The "yes"
                // deletes the chat that was asked about, by its id: `@last` may name a newer
                // chat by the time the person answers. So the address is checked (the daemon
                // refuses a title), then resolved to an id once, and the question is about that id.
                let mut id = chat.clone();
                if !yes {
                    let question = |chat: String| async {
                        let ask = Request::ChatDelete { chat, confirmed: false };
                        match c.request(ask).await {
                            Ok(_) => Err(Error::internal("deleted without asking")),
                            Err(e) if e.code == ErrorCode::NeedsConfirmation => Ok(e.message),
                            Err(e) => Err(e),
                        }
                    };
                    question(chat.clone()).await?;
                    let read = Request::ChatRead { chat: chat.clone(), from: None, limit: Some(0) };
                    let Reply::ChatTranscript { info, .. } = c.request(read).await? else {
                        return Err(Error::internal("unexpected reply"));
                    };
                    id = info.id;
                    crate::cli::ask_first(&question(id.clone()).await?, false)?;
                }
                c.request(Request::ChatDelete { chat: id, confirmed: true }).await?;
                eprintln!("deleted {chat}");
            }
            Ok(0)
        }
        ChatCmd::Export { chat, format, output } => {
            let c = ctx.connect(true).await?;
            let text = match c.request(Request::ChatExport { chat, format }).await? {
                Reply::Text { text } => text,
                r => return Err(unexpected(r)),
            };
            match output {
                Some(p) => std::fs::write(&p, text)
                    .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?,
                None => print!("{text}"),
            }
            Ok(0)
        }
    }
}

/// A command chats had until 2026-11-17, typed as plain `chat`'s words (`ptyro chat fork c:7f3k`):
/// said so, rather than asked of an agent. `chat new "fork …"` still asks it.
fn removed(words: &[String]) -> Result<(), Error> {
    let Some(first) = words.first() else { return Ok(()) };
    let why = match first.as_str() {
        "branch" | "fork" | "keep" => {
            "chats don't branch any more: retry and edit replace the last turn"
        }
        "compare" | "pick" => "a chat has one agent: `chat set --agent` switches it",
        "import" => "chats imported before stay, as ordinary chats",
        "presets" => "every chat searches the web and reads the folders you give it",
        _ => return Ok(()),
    };
    Err(Error::invalid(format!(
        "`chat {first}` was removed on 2026-11-17 ({why}); to ask it of an agent, `ptyro chat new \"{}\"`",
        words.join(" ")
    )))
}

fn chat_of(r: Reply) -> Result<ChatInfo, Error> {
    match r {
        Reply::Chat { info } => Ok(info),
        r => Err(unexpected(r)),
    }
}

/// The words, or standard input when they are `-` (or none, on a pipe).
fn words_or_stdin(words: Vec<String>) -> Result<String, Error> {
    if words.as_slice() == ["-"] || (words.is_empty() && !std::io::stdin().is_terminal()) {
        return read_stdin();
    }
    Ok(words.join(" "))
}

/// Standard input, all of it; bytes that aren't UTF-8 (a Latin-1 file in a diff) become `�`
/// rather than losing the whole of it.
fn read_stdin() -> Result<String, Error> {
    let mut b = Vec::new();
    std::io::stdin()
        .read_to_end(&mut b)
        .map_err(|e| Error::invalid(format!("standard input: {e}")))?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// A question with what was piped in after it, fenced by more backticks than it holds in a row
/// (so a diff of Markdown keeps its shape).
fn with_context(question: String, context: &str) -> String {
    if context.trim().is_empty() {
        return question;
    }
    let (mut run, mut most) = (0, 0);
    for c in context.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        most = most.max(run);
    }
    let fence = "`".repeat(most.max(2) + 1);
    format!("{question}\n\n{fence}\n{}\n{fence}", context.trim_end())
}

/// A folder as typed, made full against this directory (`.`, `notes`); `~` and full paths are
/// left for the chat's machine to read.
fn full_path(f: &str) -> String {
    if ptyro_core::os::looks_like_path(f) && !f.starts_with('.') {
        return f.to_owned();
    }
    let p = std::env::current_dir().map(|d| d.join(f)).unwrap_or_else(|_| f.into());
    ptyro_core::os::canonicalize(&p).unwrap_or(p).to_string_lossy().into_owned()
}

/// What `with_confirmation` sets when the person agrees (SPEC §22.7: another profile).
fn agree(r: &mut Request) {
    if let Request::ChatTask { confirmed, .. }
    | Request::ChatUpdate { confirmed, .. }
    | Request::ChatDelete { confirmed, .. } = r
    {
        *confirmed = true;
    }
}

/// `with_confirmation`, saying only the flag chat commands take.
async fn confirmed(c: &ptyro_client::Client, req: Request, yes: bool) -> Result<Reply, Error> {
    crate::cli::with_confirmation_as(c, req, yes, agree, "pass --yes").await
}

/// `chat task`: the worktree and the session that carries the chat out.
#[allow(clippy::too_many_arguments)]
async fn task(
    ctx: &Ctx,
    chat: String,
    project: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    yes: bool,
    detach: bool,
    json: bool,
) -> Result<i32, Error> {
    let attaching = crate::cli::attaches(detach);
    let c = ctx.connect(true).await?;
    let project = crate::cli::addr_here_opt(project)?;
    let req = Request::ChatTask { chat, project, agent, name, confirmed: false };
    let r = confirmed(&c, req, yes).await?;
    let Reply::WorktreeCreated { info, agent, agent_pending, notes, .. } = r else {
        return Err(unexpected(r));
    };
    if json {
        let v = serde_json::json!({ "worktree": info, "session": agent, "pending": agent_pending });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(0);
    }
    for n in &notes {
        eprintln!("  {n}");
    }
    if attaching && (agent.is_some() || agent_pending) {
        let follow = agent_pending.then(|| info.id.clone());
        let opts = ptyro_tui::Options { follow_worktree: follow, ..Default::default() };
        return crate::cli::attach_with(ctx, c, agent, opts).await;
    }
    match agent {
        Some(s) => println!(
            "{}: {} works on it (`ptyro attach {}`)",
            info.qualified(),
            s.agent.as_deref().unwrap_or("the agent"),
            s.name
        ),
        None if agent_pending => {
            println!("{}: the agent starts once its setup has run", info.qualified())
        }
        None => println!("{}", info.qualified()),
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
async fn new(
    ctx: &Ctx,
    text: String,
    agent: Option<String>,
    workspace: Option<String>,
    title: Option<String>,
    incognito: Option<bool>,
    folders: Vec<String>,
    no_wait: bool,
    wait: Duration,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let prompt = Some(text.clone()).filter(|t| !t.trim().is_empty());
    let has_prompt = prompt.is_some();
    let req = Request::ChatNew { workspace, agent, title, prompt, incognito, folders };
    let info = chat_of(c.request(req).await?)?;
    // No question, or not waited for: the chat, with no answer yet.
    if json && (!has_prompt || no_wait) {
        print_answer(ctx, &info, &Answered::default()).await;
        return Ok(0);
    }
    if !has_prompt {
        // No question yet: the chat view is where one is typed.
        if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
            return open(ctx, &info.addr()).await;
        }
        println!("{}", info.addr());
        return Ok(0);
    }
    eprintln!("{} · {}", info.addr(), info.agent);
    answer(ctx, c, &info, &HashSet::new(), wait, false, no_wait, json).await
}

/// `show`, `open` and `set` take one chat: more words were a question (`ptyro chat show me how
/// to…`), which `chat new` (or quotes) asks.
fn meant_new(verb: &str, chat: &str, rest: &[String]) -> Result<(), Error> {
    if rest.is_empty() {
        return Ok(());
    }
    let words =
        std::iter::once(verb).chain(std::iter::once(chat)).chain(rest.iter().map(String::as_str));
    let q = words.collect::<Vec<_>>().join(" ");
    Err(Error::invalid(format!(
        "`chat {verb}` takes one chat, not {}; did you mean `ptyro chat new \"{q}\"`?",
        rest.len() + 1
    )))
}

async fn ask(
    ctx: &Ctx,
    question: String,
    keep: bool,
    agent: Option<String>,
    timeout: u64,
    json: bool,
) -> Result<i32, Error> {
    let text = question;
    let mut c = ctx.connect(true).await?;
    // Incognito unless kept (SPEC §22.9): nothing of the question or its answer is written.
    let info = chat_of(
        c.request(Request::ChatNew {
            workspace: None,
            agent,
            title: None,
            prompt: None,
            incognito: Some(!keep),
            folders: Vec::new(),
        })
        .await?,
    )?;
    let addr = info.addr();
    // However it ends (an answer, an error, a timeout, Ctrl-C), nothing is kept unless --keep.
    let none = HashSet::new();
    let pushes = c.pushes();
    let wait = Duration::from_secs(timeout);
    let out = if json { Out::Quiet } else { Out::Final };
    let answered = tokio::select! {
        r = follow_sending(pushes, c, &info, &none, wait, out, false, Some(text)) => r,
        _ = tokio::signal::ctrl_c() => Err(Error::new(ErrorCode::Invalid, "interrupted")),
    };
    if json && let Ok(a) = &answered {
        print_answer(ctx, &info, a).await;
    }
    if keep {
        eprintln!("kept as {addr}");
    } else if let Ok(c) = ctx.connect(false).await {
        let _ = c.request(Request::ChatDelete { chat: info.id.clone(), confirmed: true }).await;
    }
    answered.map(|a| if a.ok { 0 } else { 1 })
}

/// How a followed answer is printed: as it arrives, once when it is done (the final answer only),
/// or not at all (`--json` prints it after).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Out {
    Stream,
    Final,
    Quiet,
}

impl Out {
    fn of(json: bool) -> Self {
        if json { Self::Quiet } else { Self::Stream }
    }
}

/// An answer as it ended: whether well, the message it answers, its first node and its text.
#[derive(Default)]
struct Answered {
    ok: bool,
    message: Option<String>,
    node: Option<String>,
    text: String,
    stop: String,
}

/// `--json` for a verb that asks: the chat (as it is now), the message asked and its answer (by
/// the chat's agent; none when it wasn't waited for).
async fn print_answer(ctx: &Ctx, info: &ChatInfo, a: &Answered) {
    let now = match ctx.connect(false).await {
        Ok(c) => match c
            .request(Request::ChatRead { chat: info.id.clone(), from: None, limit: None })
            .await
        {
            Ok(Reply::ChatTranscript { info, .. }) => info,
            _ => info.clone(),
        },
        Err(_) => info.clone(),
    };
    let answers: Vec<serde_json::Value> = a
        .node
        .iter()
        .map(|n| serde_json::json!({ "agent": now.agent, "node": n, "text": a.text }))
        .collect();
    let v = serde_json::json!({
        "chat": now,
        "message": a.message,
        "answers": answers,
        "stop": (!a.stop.is_empty()).then_some(&a.stop),
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// `--json` for a verb that changes a chat: `{"chat": …}`, the shape the asking verbs start with.
fn print_chat(info: &ChatInfo) {
    let v = serde_json::json!({ "chat": info });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// Follow the answer to what was just asked (`retry`: to the message already there), printed as
/// it arrives or, with `json`, as `print_answer` once it is in; `no_wait`: not followed (the
/// JSON then has no answer). The exit code: 0 when it ended well.
#[allow(clippy::too_many_arguments)]
async fn answer(
    ctx: &Ctx,
    c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    retry: bool,
    no_wait: bool,
    json: bool,
) -> Result<i32, Error> {
    if no_wait {
        if json {
            print_answer(ctx, info, &Answered { ok: true, ..Answered::default() }).await;
        }
        return Ok(0);
    }
    let a = follow(c, info, known, timeout, Out::of(json), retry).await?;
    if json {
        print_answer(ctx, info, &a).await;
    }
    Ok(if a.ok { 0 } else { 1 })
}

/// Follow a chat until the answer to the message just sent is complete, printing it as `out`
/// says. The message is the first one that isn't in `known` (what the chat had before); `retry`:
/// the last one shown, and the answer is what follows it that wasn't there before. Returns how it
/// ended; an agent that failed to start (an error notice) ends it too.
async fn follow(
    mut c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    out: Out,
    retry: bool,
) -> Result<Answered, Error> {
    follow_sending(c.pushes(), c, info, known, timeout, out, retry, None).await
}

/// Every node a chat shows now (so what comes next is told apart).
async fn known_nodes(c: &ptyro_client::Client, chat: &str) -> Result<HashSet<String>, Error> {
    match c.request(Request::ChatRead { chat: chat.to_owned(), from: None, limit: None }).await? {
        Reply::ChatTranscript { nodes, .. } => Ok(nodes.into_iter().map(|n| n.id).collect()),
        r => Err(unexpected(r)),
    }
}

/// `follow`, sending `send` once subscribed: whatever its agent does, even dying at once, is
/// seen (an incognito chat goes with its agent).
#[allow(clippy::too_many_arguments)]
async fn follow_sending(
    mut pushes: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    out: Out,
    retry: bool,
    send: Option<String>,
) -> Result<Answered, Error> {
    let stream = out == Out::Stream;
    c.send(ClientMsg::ChatSubscribe { chat: info.id.clone() })?;
    // Sent once the chat as it is has arrived, so the answer is only ever news.
    let mut send = send;
    let deadline = tokio::time::Instant::now() + timeout;
    // Items before the last user message are history; what follows is the answer.
    let mut after: Option<String> = None;
    let mut items: BTreeMap<String, ChatItem> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut printed: BTreeMap<String, usize> = BTreeMap::new();
    let mut asked: Vec<String> = Vec::new();
    let stdout = std::io::stdout();
    loop {
        let msg = match tokio::time::timeout_at(deadline, pushes.recv()).await {
            Ok(Some(m)) => m,
            Ok(None) => return Err(Error::internal("the daemon went away")),
            Err(_) => {
                return Err(Error::new(
                    ErrorCode::Timeout,
                    format!("no answer yet; `ptyro chat show {}` shows it later", info.addr()),
                ));
            }
        };
        let ServerMsg::ChatLive { chat, reset, items: got, .. } = msg else { continue };
        if chat != info.id {
            continue;
        }
        if let Some(text) = send.take() {
            c.request(Request::ChatSend { chat: info.id.clone(), text }).await?;
        }
        if reset {
            items.clear();
            order.clear();
        }
        // It can't be followed (it is gone: an incognito chat whose agent ended): the reason.
        if let [ChatItem { id, body: ChatBody::Notice { text, .. }, .. }] = got.as_slice()
            && reset
            && id == "error"
        {
            return Err(Error::new(ErrorCode::NotFound, text.clone()));
        }
        for it in got {
            if !items.contains_key(&it.id) {
                order.push(it.id.clone());
            }
            items.insert(it.id.clone(), it);
        }
        if after.is_none() {
            after = if retry {
                order
                    .iter()
                    .rev()
                    .find(|id| matches!(items[*id].body, ChatBody::User { .. }))
                    .cloned()
            } else {
                order
                    .iter()
                    .find(|id| {
                        !known.contains(*id) && matches!(items[*id].body, ChatBody::User { .. })
                    })
                    .cloned()
            };
        }
        let Some(start) = &after else { continue };
        // What came before is history (a retry's old answer was there already).
        let tail =
            order.iter().skip_while(|id| *id != start).skip(1).filter(|id| !known.contains(*id));
        let mut lock = stdout.lock();
        for id in tail {
            let it = &items[id];
            match &it.body {
                ChatBody::Assistant { text, .. } if stream => {
                    let done = printed.get(id).copied().unwrap_or(0);
                    if text.len() > done && text.is_char_boundary(done) {
                        let _ = write!(lock, "{}", &text[done..]);
                        printed.insert(id.clone(), text.len());
                    }
                }
                ChatBody::Tool { name, title, .. } if !printed.contains_key(id) => {
                    eprintln!("▸ {name} {title}");
                    printed.insert(id.clone(), 0);
                }
                ChatBody::Permission { tool, title, decided: None, .. } if !asked.contains(id) => {
                    asked.push(id.clone());
                    eprintln!(
                        "⚠ {} wants permission: {tool} {title}\n  answer: ptyro chat answer {} allow|always|deny",
                        info.agent,
                        info.addr()
                    );
                }
                ChatBody::Notice { text, level } if !printed.contains_key(id) => {
                    eprintln!("! {text}");
                    printed.insert(id.clone(), 0);
                    // The agent didn't start, or died: no answer is coming.
                    if *level == ptyro_core::chat::NoticeLevel::Error {
                        let _ = c.send(ClientMsg::ChatUnsubscribe { chat: info.id.clone() });
                        let message = Some(start.clone());
                        return Ok(Answered {
                            message,
                            stop: "error".into(),
                            ..Answered::default()
                        });
                    }
                }
                ChatBody::TurnEnd { stop, .. } => {
                    let (nodes, answer): (Vec<&String>, Vec<&str>) = order
                        .iter()
                        .skip_while(|i| *i != start)
                        .filter(|i| !known.contains(*i))
                        .filter_map(|i| match &items[i].body {
                            ChatBody::Assistant { text, .. } => Some((i, text.as_str())),
                            _ => None,
                        })
                        .unzip();
                    let text = answer.join("\n\n");
                    if out != Out::Quiet {
                        if !stream {
                            let _ = write!(lock, "{text}");
                        }
                        let _ = writeln!(lock);
                        let _ = lock.flush();
                    }
                    let ok = matches!(stop.as_str(), "end_turn" | "completed" | "success" | "");
                    if !ok {
                        eprintln!("({stop})");
                    }
                    let _ = c.send(ClientMsg::ChatUnsubscribe { chat: info.id.clone() });
                    return Ok(Answered {
                        ok,
                        message: Some(start.clone()),
                        node: nodes.first().map(|n| (*n).clone()),
                        text,
                        stop: stop.clone(),
                    });
                }
                _ => {}
            }
        }
        let _ = lock.flush();
    }
}

#[allow(clippy::too_many_arguments)]
async fn ls(
    ctx: &Ctx,
    workspace: Option<String>,
    pinned: bool,
    archived: bool,
    machine: Option<String>,
    limit: Option<u32>,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let all = workspace.is_none();
    // (With a machine, the limit applies after its chats are picked.)
    let asked = if machine.is_some() { None } else { limit };
    let req = Request::ChatList { workspace, all, pinned, archived, limit: asked };
    let mut chats = match c.request(req).await? {
        Reply::Chats { chats, .. } => chats,
        r => return Err(unexpected(r)),
    };
    if let Some(m) = &machine {
        chats.retain(|c| match c.machine.as_deref() {
            Some(x) => x == m,
            None => m == "here",
        });
    }
    if let (Some(n), true) = (limit, machine.is_some()) {
        chats.truncate(n as usize);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&chats).unwrap_or_default());
        return Ok(0);
    }
    if chats.is_empty() {
        eprintln!("no chats yet: `ptyro chat \"your question\"` starts one");
        return Ok(0);
    }
    // Another machine's that is down: listed as it was (SPEC §22.11).
    let down: Vec<String> = if chats.iter().any(|c| c.machine.is_some()) {
        match c.request(Request::MachineList).await {
            Ok(Reply::Machines { machines }) => machines
                .into_iter()
                .filter(|m| m.state != ptyro_core::model::MachineState::Online)
                .map(|m| m.name)
                .collect(),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    for ch in &chats {
        let mut marks = String::new();
        if ch.machine.as_ref().is_some_and(|m| down.contains(m)) {
            marks.push_str(" (offline)");
        }
        if ch.unread > 0 {
            marks.push_str(" ●");
        }
        if let Some(a) = &ch.attention {
            marks.push_str(&format!(" ⚠ {}", a.text));
        }
        println!(
            "{:<12} {} {:<40} {:<10} {:<10} {:>5}{marks}",
            ch.addr(),
            ch.glyph(),
            crate::cli::truncate(ch.label(), 40),
            ch.agent,
            ch.workspace,
            ago(ch.last_ms),
        );
    }
    Ok(0)
}

async fn show(ctx: &Ctx, chat: String, ids: bool, format: &str) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let (info, nodes) = match c.request(Request::ChatRead { chat, from: None, limit: None }).await?
    {
        Reply::ChatTranscript { info, nodes } => (info, nodes),
        r => return Err(unexpected(r)),
    };
    match format {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "chat": info, "nodes": nodes }))
                .unwrap_or_default()
        ),
        "md" => {
            let text = match c
                .request(Request::ChatExport { chat: info.id.clone(), format: "md".into() })
                .await?
            {
                Reply::Text { text } => text,
                r => return Err(unexpected(r)),
            };
            print!("{text}");
        }
        "text" => print_text(&info, &nodes, ids),
        f => return Err(Error::invalid(format!("{f:?}: text, md or json"))),
    }
    Ok(0)
}

fn print_text(info: &ChatInfo, nodes: &[ChatNode], ids: bool) {
    println!("{} · {} · {}@{}", info.addr(), info.label(), info.agent, info.profile);
    for n in nodes {
        let id = if ids { format!("[{}] ", short_id(&n.id)) } else { String::new() };
        match &n.body {
            ChatBody::User { text } => println!("\n{id}› {}", text.trim()),
            ChatBody::Assistant { text, .. } => println!("\n{id}{}", text.trim()),
            ChatBody::Tool { name, title, status, .. } => {
                println!("  ▸ {name} {title} {}", status.glyph());
            }
            ChatBody::Permission { tool, title, decided, .. } => println!(
                "  ⚠ wants permission: {tool} {title}{}",
                decided.as_ref().map(|d| format!(" → {d}")).unwrap_or_default()
            ),
            ChatBody::Notice { text, .. } => println!("  ! {text}"),
            _ => {}
        }
    }
}

/// A node's id as people use it: its last 8 characters (`chat edit … NODE` takes them).
fn short_id(id: &str) -> String {
    id[id.len().saturating_sub(8)..].to_owned()
}

/// `chat search`'s flags. Its words may start with `-` (a word left out), so clap hands every
/// argument after the first word over as a word: the flags among them are taken back here.
struct SearchArgs {
    chat: Option<String>,
    workspace: Option<String>,
    agent: Option<String>,
    pinned: bool,
    archived: bool,
    here: bool,
    /// `--meaning` or `--words` (SPEC §22.10), sent as `by:` in the query.
    by: Option<&'static str>,
    limit: u32,
    json: bool,
}

impl SearchArgs {
    /// The query's words, with the flags among them applied (`--` ends the flags).
    fn take_flags<'a>(&mut self, query: &'a [String]) -> Result<Vec<&'a str>, Error> {
        let mut words = Vec::new();
        let mut it = query.iter().map(String::as_str);
        while let Some(w) = it.next() {
            let (name, inline) = match w.split_once('=') {
                Some((n, v)) if n.starts_with('-') => (n, Some(v)),
                _ => (w, None),
            };
            let value = |it: &mut dyn Iterator<Item = &'a str>| {
                inline
                    .map(str::to_owned)
                    .or_else(|| it.next().map(str::to_owned))
                    .ok_or_else(|| Error::new(ErrorCode::Invalid, format!("{name} needs a value")))
            };
            match name {
                "--" => words.extend(it.by_ref()),
                "--json" => self.json = true,
                "--here" => self.here = true,
                "--pinned" => self.pinned = true,
                "--archived" => self.archived = true,
                "--meaning" | "--words" => {
                    let by = if name == "--meaning" { "meaning" } else { "words" };
                    if self.by.is_some_and(|b| b != by) {
                        return Err(Error::invalid(
                            "--meaning and --words can't go together: one or the other",
                        ));
                    }
                    self.by = Some(by);
                }
                "--in" => self.chat = Some(value(&mut it)?),
                "-w" | "--workspace" => self.workspace = Some(value(&mut it)?),
                "-a" | "--agent" => self.agent = Some(value(&mut it)?),
                "-l" | "--limit" => {
                    let v = value(&mut it)?;
                    self.limit = v.parse().map_err(|_| {
                        Error::new(ErrorCode::Invalid, format!("{name}: {v:?} is not a number"))
                    })?;
                }
                _ => words.push(w),
            }
        }
        Ok(words)
    }
}

#[allow(clippy::too_many_arguments)]
async fn search(
    ctx: &Ctx,
    query: String,
    chat: Option<String>,
    workspace: Option<String>,
    agent: Option<String>,
    pinned: bool,
    archived: bool,
    machines: bool,
    limit: u32,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let req = Request::ChatSearch {
        query,
        chat,
        workspace,
        agent,
        pinned,
        archived,
        limit: Some(limit),
        machines,
        profile: None,
    };
    let hits = match c.request(req).await {
        Ok(Reply::ChatHits { hits }) => hits,
        Ok(r) => return Err(unexpected(r)),
        // Meaning search is off: offer to switch it on (SPEC §22.10).
        Err(e) if e.code == ErrorCode::NeedsConfirmation => return meaning_on(ctx, e),
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&hits).unwrap_or_default());
        return Ok(0);
    }
    // Nothing found is an answer, not a failure (as `ls` with nothing to list).
    if hits.is_empty() {
        eprintln!("nothing matches");
        return Ok(0);
    }
    // Matches bold on a terminal; between * elsewhere.
    let (on, off) =
        if std::io::stdout().is_terminal() { ("\x1b[1m", "\x1b[0m") } else { ("*", "*") };
    for h in &hits {
        let cite = match &h.node {
            Some(n) => format!("{}/{}", h.chat.addr(), short_id(n)),
            None => h.chat.addr(),
        };
        println!("{cite}  {} · {} · {}", h.chat.label(), h.chat.agent, ago(h.at_ms));
        let snippet = h
            .snippet
            .replace('\n', " ")
            .replace(ptyro_core::chats::MARK_ON, on)
            .replace(ptyro_core::chats::MARK_OFF, off);
        // ≈: found by meaning, none of the words is in it.
        let how = if h.how == "meaning" { "≈ " } else { "" };
        println!("    {how}{snippet}");
    }
    Ok(0)
}

/// `search --meaning` with meaning search off: on a terminal, ask to switch it on (in your
/// config, as the settings panel would); the daemon then downloads its model.
fn meaning_on(ctx: &Ctx, e: Error) -> Result<i32, Error> {
    if ctx.remote.is_some() || !crate::cli::confirm(&format!("{}. Switch it on?", e.message)) {
        return Err(Error::new(
            ErrorCode::Invalid,
            format!("{} (`ptyro config set chat.meaning true`)", e.message),
        ));
    }
    let layer = ptyro_core::settings::Layer::User(ptyro_core::config::Config::path(&ctx.paths));
    let op = ptyro_core::settings::Op::Set { key: "chat.meaning".into(), value: "true".into() };
    ptyro_core::settings::apply(&layer, &op, &crate::config::lookups(), &crate::config::semantic)
        .map_err(|e| Error::new(ErrorCode::Invalid, e))?;
    eprintln!(
        "meaning search is on: its model is downloading, then your chats are embedded \
         (`ptyro doctor` shows how far). Search by words meanwhile."
    );
    Ok(1)
}

/// The chat view: the TUI, opened on this chat.
async fn open(ctx: &Ctx, chat: &str) -> Result<i32, Error> {
    crate::cli::check_attachable()?;
    let c = ctx.connect(true).await?;
    let (info, _) = match c
        .request(Request::ChatRead { chat: chat.to_owned(), from: None, limit: None })
        .await?
    {
        Reply::ChatTranscript { info, nodes, .. } => (info, nodes),
        r => return Err(unexpected(r)),
    };
    let opts = ptyro_tui::Options { chat: Some(info.id), ..ptyro_tui::Options::default() };
    crate::cli::attach_with(ctx, c, None, opts).await
}

async fn done(ctx: &Ctx, req: Request) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    c.request(req).await?;
    Ok(0)
}

fn blank(chat: &str) -> Request {
    Request::ChatUpdate {
        chat: chat.to_owned(),
        title: None,
        pinned: None,
        archived: None,
        agent: None,
        folders: None,
        confirmed: false,
        auto_title: false,
    }
}

fn set_pinned(r: &mut Request, v: bool) {
    if let Request::ChatUpdate { pinned, .. } = r {
        *pinned = Some(v);
    }
}

fn set_archived(r: &mut Request, v: bool) {
    if let Request::ChatUpdate { archived, .. } = r {
        *archived = Some(v);
    }
}

/// A change to a chat, asking first where the daemon says to (`yes`: agreed already).
async fn update_as(
    ctx: &Ctx,
    chat: &str,
    yes: bool,
    json: bool,
    f: impl FnOnce(&mut Request),
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mut req = blank(chat);
    f(&mut req);
    let info = chat_of(confirmed(&c, req, yes).await?)?;
    if json {
        print_chat(&info);
    } else {
        eprintln!("{} · {}", info.addr(), info.label());
    }
    Ok(0)
}

/// The same change to each chat; `json`: `{"chats": […]}`, each as it is now.
async fn each(
    ctx: &Ctx,
    chats: Vec<String>,
    what: &str,
    json: bool,
    f: impl Fn(&mut Request),
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mut done = Vec::new();
    for chat in chats {
        let mut req = blank(&chat);
        f(&mut req);
        let info = chat_of(c.request(req).await?)?;
        if !json {
            eprintln!("{what}: {} ({})", info.addr(), info.label());
        }
        done.push(info);
    }
    if json {
        let v = serde_json::json!({ "chats": done });
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::SearchArgs;

    #[test]
    fn piped_context_is_fenced_past_its_own_backticks() {
        let q = super::with_context("review".into(), "a\n```rust\nx\n```\n");
        assert_eq!(q, "review\n\n````\na\n```rust\nx\n```\n````");
        assert_eq!(super::with_context("q".into(), "  \n"), "q");
        assert_eq!(super::with_context("q".into(), "plain"), "q\n\n```\nplain\n```");
    }

    fn args() -> SearchArgs {
        SearchArgs {
            chat: None,
            workspace: None,
            agent: None,
            pinned: false,
            archived: false,
            here: false,
            by: None,
            limit: 20,
            json: false,
        }
    }

    #[test]
    fn search_flags_after_the_words_are_flags() {
        let q: Vec<String> = "redis -budget -w work --in=c:7f3k -l 3 -a codex --json -- -n"
            .split(' ')
            .map(str::to_owned)
            .collect();
        let mut a = args();
        assert_eq!(a.take_flags(&q).unwrap(), ["redis", "-budget", "-n"]);
        assert_eq!(a.workspace.as_deref(), Some("work"));
        assert_eq!(a.chat.as_deref(), Some("c:7f3k"));
        assert_eq!(a.agent.as_deref(), Some("codex"));
        assert_eq!((a.limit, a.json), (3, true));
        let mut m = args();
        assert_eq!(m.take_flags(&["cats".into(), "--meaning".into()]).unwrap(), ["cats"]);
        assert_eq!(m.by, Some("meaning"));
        let mut both = args();
        assert!(both.take_flags(&["cats".into(), "--meaning".into(), "--words".into()]).is_err());
        let mut clap_said = args();
        clap_said.by = Some("words");
        assert!(clap_said.take_flags(&["cats".into(), "--meaning".into()]).is_err());
        assert!(args().take_flags(&["x".into(), "--in".into()]).is_err());
        assert!(args().take_flags(&["x".into(), "-l".into(), "lots".into()]).is_err());
    }
}
