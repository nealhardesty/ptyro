//! `ptyro chat` (SPEC §22.12): plain conversations with your agents, from any shell. A chat is
//! kept (in `chat.db`) and shows in the TUI's and the web's CHATS section too; `ptyro chat ask`
//! is the one-shot that keeps nothing (`git diff | ptyro chat ask "review this"`).

use std::collections::{BTreeMap, HashSet};
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_core::chat::{ChatBody, ChatItem};
use ptyro_core::chats::{ChatInfo, ChatNode, ChatSiblings};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{ClientMsg, Reply, Request, ServerMsg};

use crate::cli::{Ctx, age, unexpected};
use crate::complete;

#[derive(Subcommand)]
pub enum ChatCmd {
    /// A new chat (the default): with words, its first message, answered here
    New {
        /// The first message (`-`, or nothing on a pipe: read standard input)
        prompt: Vec<String>,
        /// `agent` or `agent@profile` (default `[chat] agent`, else the first installed)
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// What it may use: chat (web search, the default), read, none, or your own (`chat presets`)
        #[arg(long, add = ArgValueCandidates::new(complete::chat_presets))]
        preset: Option<String>,
        /// The workspace it belongs to (default main)
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Keep nothing: not saved, not searchable, gone when its agent stops
        #[arg(long)]
        incognito: bool,
        /// A folder it may read (repeatable); with no --preset the chat then reads (`read`)
        #[arg(long = "folder", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        folders: Vec<String>,
        /// Ask several agents the first message, each answer a branch (`claude,codex`; as
        /// `chat compare`)
        #[arg(long, value_delimiter = ',', add = ArgValueCandidates::new(complete::agents))]
        compare: Vec<String>,
        /// Ask a --compare agent under another profile than the chat's without asking first
        #[arg(short, long)]
        yes: bool,
        /// Don't wait for the answer
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// One question, one answer on standard output; nothing is kept unless --keep. Standard
    /// input is context: `git diff | ptyro chat ask "review this"`
    Ask {
        #[arg(required = true)]
        question: Vec<String>,
        /// Keep it as a chat
        #[arg(long)]
        keep: bool,
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long)]
        preset: Option<String>,
        /// Seconds to wait for the answer
        #[arg(long, default_value_t = 600)]
        timeout: u64,
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
        #[arg(short = 'n', long)]
        limit: Option<u32>,
        /// Only chats imported from the agents' own histories (`chat import`)
        #[arg(long)]
        imported: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print a chat: its shown branch
    Show {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// Every branch's messages, not just the shown one
        #[arg(long)]
        all: bool,
        /// Each message's id (for `edit`, `retry`, `fork --at`, `branch --to`)
        #[arg(long)]
        ids: bool,
        /// Where the shown branch has others (`‹ 2/3 ›` and their ids)
        #[arg(long)]
        branches: bool,
        /// text (default), md or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Ask again: a new answer beside the old one (which stays as a branch), printed as it
    /// arrives. NODE: the message (or a node of the answer) to retry; default the last
    Retry {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        node: Option<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Say a message differently (NODE: its id, see `show --ids`): answered afresh; the old one
    /// and its answer stay as a branch
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
    },
    /// Show another branch of a chat: the one through NODE
    Branch {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        #[arg(long)]
        to: String,
    },
    /// A new chat from this one (from the end, or `--at` a message), with its own agent; with
    /// words, its first message. `--side`: a side chat that reads this one and never changes it,
    /// gone after its answer unless `--keep`
    Fork {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// The message to fork after (default: the end)
        #[arg(long)]
        at: Option<String>,
        /// Continue on another agent (`agent@profile`)
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long)]
        side: bool,
        /// Keep the side chat as a chat of its own
        #[arg(long, requires = "side")]
        keep: bool,
        /// A first message
        prompt: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Keep a side chat as a chat of its own
    Keep {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
    },
    /// Search every chat's messages and titles, on every linked machine too: words (all must
    /// match), "a phrase", prefix*, -word, agent:codex, since:7d, workspace:work, by:meaning
    Search {
        /// (A word may start with `-`: it is left out. After `--`, every word is a word.)
        #[arg(required = true, allow_hyphen_values = true)]
        query: Vec<String>,
        /// Only this chat
        #[arg(long, add = ArgValueCandidates::new(complete::chats))]
        chat: Option<String>,
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
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
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Ask your chats a question: an agent searches them and answers, citing c:7f3k/node
    Find {
        #[arg(required = true)]
        question: Vec<String>,
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Open a chat in the TUI's chat view
    Open {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
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
    },
    /// Answer the chat's permission request: allow, always or deny
    Answer {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        decision: String,
    },
    /// Stop the answer being written
    Stop {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
    },
    /// Give a chat a title (none: a new automatic one)
    Retitle {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        title: Vec<String>,
    },
    /// Keep chats at the top
    Pin {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
    },
    Unpin {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
    },
    /// Put chats away (`chat ls --archived` lists them)
    Archive {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
    },
    Unarchive {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
    },
    /// Change the agent, the tool preset or the folders it reads; the next message carries the
    /// conversation over
    Set {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long, add = ArgValueCandidates::new(complete::chat_presets))]
        preset: Option<String>,
        /// The folders it may read, all of them (repeatable)
        #[arg(long = "folder", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        folders: Vec<String>,
        /// It reads no folder
        #[arg(long, conflicts_with = "folders")]
        no_folders: bool,
    },
    /// The tool presets, each with what it lets a chat use
    Presets {
        #[arg(long)]
        json: bool,
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
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// The worktree's name (default: from the chat's title)
        #[arg(long)]
        name: Option<String>,
        /// Go ahead when the task's profile isn't the chat's (or a profile guard asks)
        #[arg(short, long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete chats (gone at once from every list)
    #[command(alias = "delete")]
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
    },
    /// Ask several agents the same question: each answer is a branch of the chat, labelled with
    /// its agent (`‹ 2/3 codex ›`); with no TEXT the last message is asked again of the agents
    /// that haven't answered it. Each answer is printed under its agent once all are in
    Compare {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        /// The question (none: the chat's last message again)
        text: Vec<String>,
        /// Which agents, `agent` or `agent@profile`, comma-separated (default: every installed
        /// one that can chat)
        #[arg(long, value_delimiter = ',', add = ArgValueCandidates::new(complete::agents))]
        agents: Vec<String>,
        /// Ask an agent under another profile than the chat's without asking first
        #[arg(short, long)]
        yes: bool,
        /// Don't wait for the answers
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        json: bool,
    },
    /// Keep an answer: show its branch and switch the chat to the agent that wrote it, so the
    /// next message goes there. NODE: a node of that branch (default: the one shown)
    Pick {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        node: Option<String>,
    },
    /// Import your agents' own histories (Claude Code, Codex and Gemini sessions) as read-only
    /// chats: new sessions and what old ones gained since the last import. Searchable at once;
    /// a message to one continues it as a fork. `[chat] import = true` keeps doing it
    Import {
        /// claude, codex, gemini, or all (the default)
        #[arg(long, default_value = "all")]
        agent: String,
        /// One profile's histories (default: every profile's)
        #[arg(long, add = ArgValueCandidates::new(complete::profiles))]
        profile: Option<String>,
        /// Only sessions touched in this long (`30d`, `12h`)
        #[arg(long)]
        since: Option<String>,
        /// Count what would be imported; write nothing
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
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

pub async fn run(ctx: &Ctx, cmd: Option<ChatCmd>, words: Vec<String>) -> Result<i32, Error> {
    let cmd = cmd.unwrap_or(ChatCmd::New {
        prompt: words,
        agent: None,
        preset: None,
        workspace: None,
        title: None,
        incognito: false,
        folders: Vec::new(),
        compare: Vec::new(),
        yes: false,
        no_wait: false,
        json: false,
    });
    match cmd {
        ChatCmd::New {
            prompt,
            agent,
            preset,
            workspace,
            title,
            incognito,
            folders,
            compare,
            yes,
            no_wait,
            json,
        } => {
            let text = words_or_stdin(prompt)?;
            let folders = folders.iter().map(|f| full_path(f)).collect();
            new(
                ctx, text, agent, preset, workspace, title, incognito, folders, compare, yes,
                no_wait, json,
            )
            .await
        }
        ChatCmd::Retry { chat, node, no_wait, timeout } => {
            let c = ctx.connect(true).await?;
            let known = known_nodes(&c, &chat).await?;
            let info = chat_of(c.request(Request::ChatRetry { chat, node }).await?)?;
            if no_wait {
                return Ok(0);
            }
            // The answer is a new one to a message already there: follow from its end.
            let ok = wait_answer(c, &info, &known, Duration::from_secs(timeout), true).await?;
            Ok(if ok { 0 } else { 1 })
        }
        ChatCmd::Edit { chat, node, text, no_wait, timeout } => {
            let c = ctx.connect(true).await?;
            let known = known_nodes(&c, &chat).await?;
            let info =
                chat_of(c.request(Request::ChatEdit { chat, node, text: text.join(" ") }).await?)?;
            if no_wait {
                return Ok(0);
            }
            let ok = wait(c, &info, &known, Duration::from_secs(timeout), true).await?;
            Ok(if ok { 0 } else { 1 })
        }
        ChatCmd::Branch { chat, to } => {
            let c = ctx.connect(true).await?;
            let info = chat_of(c.request(Request::ChatBranch { chat, to }).await?)?;
            eprintln!("{}: showing the branch through it", info.addr());
            Ok(0)
        }
        ChatCmd::Fork { chat, at, agent, side, keep, prompt, json } => {
            fork(ctx, chat, at, agent, side, keep, prompt.join(" "), json).await
        }
        ChatCmd::Keep { chat } => {
            update(ctx, &chat, |r| {
                if let Request::ChatUpdate { keep, .. } = r {
                    *keep = Some(true);
                }
            })
            .await
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
        ChatCmd::Find { question, agent, timeout } => {
            let c = ctx.connect(true).await?;
            let req = Request::ChatFind { question: question.join(" "), agent, workspace: None };
            let info = chat_of(c.request(req).await?)?;
            let none = HashSet::new();
            let answered = tokio::select! {
                r = wait(c, &info, &none, Duration::from_secs(timeout), false) => r,
                _ = tokio::signal::ctrl_c() => Err(Error::new(ErrorCode::Invalid, "interrupted")),
            };
            // (It is incognito: it goes with its agent; this makes sure.)
            if let Ok(c) = ctx.connect(false).await {
                let _ = c.request(Request::ChatDelete { chat: info.id.clone() }).await;
            }
            answered.map(|ok| if ok { 0 } else { 1 })
        }
        ChatCmd::Ask { question, keep, agent, preset, timeout } => {
            ask(ctx, question.join(" "), keep, agent, preset, timeout).await
        }
        ChatCmd::Ls { workspace, pinned, archived, machine, limit, imported, json } => {
            ls(ctx, workspace, pinned, archived, machine, limit, imported, json).await
        }
        ChatCmd::Compare { chat, text, agents, yes, no_wait, timeout, json } => {
            let text = Some(text.join(" ")).filter(|t| !t.trim().is_empty());
            compare(ctx, chat, agents, text, yes, no_wait, Duration::from_secs(timeout), json).await
        }
        ChatCmd::Pick { chat, node } => {
            let c = ctx.connect(true).await?;
            let info = chat_of(c.request(Request::ChatPick { chat, node }).await?)?;
            eprintln!("kept: {} goes on with {}@{}", info.addr(), info.agent, info.profile);
            Ok(0)
        }
        ChatCmd::Import { agent, profile, since, dry_run, json } => {
            import(ctx, agent, profile, since, dry_run, json).await
        }
        ChatCmd::Show { chat, all, ids, branches, format } => {
            show(ctx, chat, all, ids, branches, &format).await
        }
        ChatCmd::Open { chat } => open(ctx, &chat).await,
        ChatCmd::Send { chat, text, no_wait, timeout } => {
            let c = ctx.connect(true).await?;
            // What was there before: the answer is what follows this message, not an earlier one.
            let (asked, known): (String, HashSet<String>) =
                match c.request(Request::ChatRead { chat: chat.clone(), all: true }).await? {
                    Reply::ChatTranscript { info, nodes, .. } => {
                        (info.id, nodes.into_iter().map(|n| n.id).collect())
                    }
                    r => return Err(unexpected(r)),
                };
            let info = chat_of(c.request(Request::ChatSend { chat, text: text.join(" ") }).await?)?;
            // An imported chat carries on as a fork: the answer is the fork's last message's.
            let forked = info.id != asked;
            if forked {
                eprintln!("continued as a fork: {} ({})", info.addr(), info.label());
            }
            if no_wait {
                return Ok(0);
            }
            let ok = if forked {
                wait_answer(c, &info, &HashSet::new(), Duration::from_secs(timeout), true).await?
            } else {
                wait(c, &info, &known, Duration::from_secs(timeout), true).await?
            };
            Ok(if ok { 0 } else { 1 })
        }
        ChatCmd::Answer { chat, decision } => {
            done(ctx, Request::ChatAnswer { chat, decision }).await
        }
        ChatCmd::Stop { chat } => done(ctx, Request::ChatStop { chat }).await,
        ChatCmd::Retitle { chat, title } => {
            update(ctx, &chat, |r| {
                if let Request::ChatUpdate { title: t, .. } = r {
                    *t = Some(title.join(" "));
                }
            })
            .await
        }
        ChatCmd::Pin { chats } => each(ctx, chats, "pinned", |r| set_pinned(r, true)).await,
        ChatCmd::Unpin { chats } => each(ctx, chats, "unpinned", |r| set_pinned(r, false)).await,
        ChatCmd::Archive { chats } => each(ctx, chats, "archived", |r| set_archived(r, true)).await,
        ChatCmd::Unarchive { chats } => {
            each(ctx, chats, "back from the archive", |r| set_archived(r, false)).await
        }
        ChatCmd::Set { chat, agent, preset, folders, no_folders } => {
            if agent.is_none() && preset.is_none() && folders.is_empty() && !no_folders {
                return Err(Error::invalid(
                    "what should change? --agent A, --preset P, --folder DIR or --no-folders",
                ));
            }
            let folders = (no_folders || !folders.is_empty())
                .then(|| folders.iter().map(|f| full_path(f)).collect());
            update(ctx, &chat, |r| {
                if let Request::ChatUpdate { agent: a, preset: p, folders: f, .. } = r {
                    *a = agent;
                    *p = preset;
                    *f = folders;
                }
            })
            .await
        }
        ChatCmd::Presets { json } => {
            let c = ctx.connect(true).await?;
            let presets = match c.request(Request::ChatPresets).await? {
                Reply::ChatPresets { presets } => presets,
                r => return Err(unexpected(r)),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&presets).unwrap_or_default());
                return Ok(0);
            }
            let w = presets.iter().map(|p| p.name.len()).max().unwrap_or(4).max(4);
            for p in &presets {
                let mine = if p.builtin { "" } else { "  (yours)" };
                println!("{:<w$}  {}{mine}", p.name, p.about);
            }
            Ok(0)
        }
        ChatCmd::Task { chat, project, agent, name, yes, json } => {
            task(ctx, chat, project, agent, name, yes, json).await
        }
        ChatCmd::Rm { chats } => {
            let c = ctx.connect(true).await?;
            for chat in chats {
                c.request(Request::ChatDelete { chat: chat.clone() }).await?;
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

fn chat_of(r: Reply) -> Result<ChatInfo, Error> {
    match r {
        Reply::Chat { info } => Ok(info),
        r => Err(unexpected(r)),
    }
}

/// The words, or standard input when they are `-` (or none, on a pipe).
fn words_or_stdin(words: Vec<String>) -> Result<String, Error> {
    if words.as_slice() == ["-"] || (words.is_empty() && !std::io::stdin().is_terminal()) {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Error::invalid(format!("standard input: {e}")))?;
        return Ok(s);
    }
    Ok(words.join(" "))
}

/// A folder as typed, made full against this directory (`.`, `notes`); `~` and full paths are
/// left for the chat's machine to read.
fn full_path(f: &str) -> String {
    if f.starts_with('/') || f.starts_with('~') {
        return f.to_owned();
    }
    let p = std::env::current_dir().map(|d| d.join(f)).unwrap_or_else(|_| f.into());
    p.canonicalize().unwrap_or(p).to_string_lossy().into_owned()
}

/// What `with_confirmation` sets when the person agrees (SPEC §22.7: another profile).
fn agree(r: &mut Request) {
    if let Request::ChatTask { confirmed, .. }
    | Request::ChatCompare { confirmed, .. }
    | Request::ChatNew { confirmed, .. } = r
    {
        *confirmed = true;
    }
}

/// `with_confirmation`, saying only the flag chat commands take.
async fn confirmed(c: &ptyro_client::Client, req: Request, yes: bool) -> Result<Reply, Error> {
    crate::cli::with_confirmation_as(c, req, yes, agree, "pass --yes").await
}

/// Why a new chat's compare didn't start: the notice it left, with the chat it made.
async fn compare_refusal(c: &ptyro_client::Client, info: &ChatInfo) -> String {
    let why = match c.request(Request::ChatRead { chat: info.id.clone(), all: true }).await {
        Ok(Reply::ChatTranscript { nodes, .. }) => {
            nodes.into_iter().rev().find_map(|n| match n.body {
                ChatBody::Notice { text, .. } => Some(text),
                _ => None,
            })
        }
        _ => None,
    };
    format!(
        "{}: the agents weren't asked ({}); `ptyro chat compare {} --agents A,B \"…\"` asks again, `ptyro chat rm {}` removes it",
        info.addr(),
        why.as_deref().unwrap_or("the compare didn't start"),
        info.addr(),
        info.addr()
    )
}

/// `chat task`: the worktree and the session that carries the chat out.
async fn task(
    ctx: &Ctx,
    chat: String,
    project: Option<String>,
    agent: Option<String>,
    name: Option<String>,
    yes: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
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
    preset: Option<String>,
    workspace: Option<String>,
    title: Option<String>,
    incognito: bool,
    folders: Vec<String>,
    compare: Vec<String>,
    yes: bool,
    no_wait: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let prompt = Some(text.clone()).filter(|t| !t.trim().is_empty());
    let has_prompt = prompt.is_some();
    if !compare.is_empty() && !has_prompt {
        return Err(Error::invalid("--compare asks a question of each agent: say it"));
    }
    let req = Request::ChatNew {
        workspace,
        agent,
        preset,
        title,
        prompt,
        incognito,
        folders,
        compare: compare.clone(),
        confirmed: false,
    };
    let info = chat_of(confirmed(&c, req, yes).await?)?;
    if !compare.is_empty() {
        if info.comparing.is_none() {
            // It didn't start: no first message was written, so there is no answer to wait for.
            return Err(Error::invalid(compare_refusal(&c, &info).await));
        }
        eprintln!("{} · {}", info.addr(), info.tools_badge());
        return compare_wait(c, info, no_wait, Duration::from_secs(600), json).await;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
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
    eprintln!("{} · {} · {}", info.addr(), info.agent, info.tools_badge());
    if no_wait {
        return Ok(0);
    }
    let ok = wait(c, &info, &HashSet::new(), Duration::from_secs(600), true).await?;
    Ok(if ok { 0 } else { 1 })
}

async fn ask(
    ctx: &Ctx,
    question: String,
    keep: bool,
    agent: Option<String>,
    preset: Option<String>,
    timeout: u64,
) -> Result<i32, Error> {
    let mut text = question;
    if !std::io::stdin().is_terminal() {
        let mut ctxt = String::new();
        let _ = std::io::stdin().read_to_string(&mut ctxt);
        if !ctxt.trim().is_empty() {
            text = format!("{text}\n\n```\n{}\n```", ctxt.trim_end());
        }
    }
    let mut c = ctx.connect(true).await?;
    // Incognito unless kept (SPEC §22.9): nothing of the question or its answer is written.
    let info = chat_of(
        c.request(Request::ChatNew {
            workspace: None,
            agent,
            preset,
            title: None,
            prompt: None,
            incognito: !keep,
            folders: Vec::new(),
            compare: Vec::new(),
            confirmed: false,
        })
        .await?,
    )?;
    let addr = info.addr();
    // However it ends (an answer, an error, a timeout, Ctrl-C), nothing is kept unless --keep.
    let none = HashSet::new();
    let pushes = c.pushes();
    let wait = Duration::from_secs(timeout);
    let answered = tokio::select! {
        r = follow_sending(pushes, c, &info, &none, wait, false, false, Some(text)) => r,
        _ = tokio::signal::ctrl_c() => Err(Error::new(ErrorCode::Invalid, "interrupted")),
    };
    if keep {
        eprintln!("kept as {addr}");
    } else if let Ok(c) = ctx.connect(false).await {
        let _ = c.request(Request::ChatDelete { chat: info.id.clone() }).await;
    }
    answered.map(|ok| if ok { 0 } else { 1 })
}

/// Follow a chat until the answer to the message just sent is complete, printing it (`stream`:
/// as it arrives; else once, the final answer only). The message is the first one that isn't in
/// `known` (what the chat had before). Returns whether the turn ended well; an agent that failed
/// to start (an error notice) ends it too.
async fn wait(
    c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    stream: bool,
) -> Result<bool, Error> {
    follow(c, info, known, timeout, stream, false).await
}

/// The same for a retry: the message is already there (the last one shown); the answer is what
/// follows it that wasn't there before.
async fn wait_answer(
    c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    stream: bool,
) -> Result<bool, Error> {
    follow(c, info, known, timeout, stream, true).await
}

/// Every node a chat has now (so what comes next is told apart).
async fn known_nodes(c: &ptyro_client::Client, chat: &str) -> Result<HashSet<String>, Error> {
    match c.request(Request::ChatRead { chat: chat.to_owned(), all: true }).await? {
        Reply::ChatTranscript { nodes, .. } => Ok(nodes.into_iter().map(|n| n.id).collect()),
        r => Err(unexpected(r)),
    }
}

async fn follow(
    mut c: ptyro_client::Client,
    info: &ChatInfo,
    known: &HashSet<String>,
    timeout: Duration,
    stream: bool,
    retry: bool,
) -> Result<bool, Error> {
    follow_sending(c.pushes(), c, info, known, timeout, stream, retry, None).await
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
    stream: bool,
    retry: bool,
    send: Option<String>,
) -> Result<bool, Error> {
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
    let out = std::io::stdout();
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
        let mut lock = out.lock();
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
                        return Ok(false);
                    }
                }
                ChatBody::TurnEnd { stop, .. } => {
                    if !stream {
                        let answer: Vec<&str> = order
                            .iter()
                            .skip_while(|i| *i != start)
                            .filter_map(|i| match &items[i].body {
                                ChatBody::Assistant { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect();
                        let _ = write!(lock, "{}", answer.join("\n\n"));
                    }
                    let _ = writeln!(lock);
                    let _ = lock.flush();
                    let ok = matches!(stop.as_str(), "end_turn" | "completed" | "success" | "");
                    if !ok {
                        eprintln!("({stop})");
                    }
                    let _ = c.send(ClientMsg::ChatUnsubscribe { chat: info.id.clone() });
                    return Ok(ok);
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
    imported: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let all = workspace.is_none();
    // (With a machine, or imported ones only, the limit applies after they are picked.)
    let asked = if machine.is_some() || imported { None } else { limit };
    let req = Request::ChatList { workspace, all, pinned, archived, limit: asked };
    let mut chats = match c.request(req).await? {
        Reply::Chats { chats } => chats,
        r => return Err(unexpected(r)),
    };
    if let Some(m) = &machine {
        chats.retain(|c| match c.machine.as_deref() {
            Some(x) => x == m,
            None => m == "here",
        });
    }
    if imported {
        chats.retain(|c| c.source.is_some());
    }
    if let (Some(n), true) = (limit, machine.is_some() || imported) {
        chats.truncate(n as usize);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&chats).unwrap_or_default());
        return Ok(0);
    }
    if chats.is_empty() && imported {
        eprintln!("no imported chats: `ptyro chat import` imports your agents' own histories");
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
            age(ch.last_ms),
        );
    }
    Ok(0)
}

async fn show(
    ctx: &Ctx,
    chat: String,
    all: bool,
    ids: bool,
    branches: bool,
    format: &str,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let (info, nodes, siblings) = match c.request(Request::ChatRead { chat, all }).await? {
        Reply::ChatTranscript { info, nodes, siblings } => (info, nodes, siblings),
        r => return Err(unexpected(r)),
    };
    match format {
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "chat": info, "nodes": nodes, "siblings": siblings })
            )
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
        "text" => print_text(&info, &nodes, ids, if branches { &siblings } else { &[] }),
        f => return Err(Error::invalid(format!("{f:?}: text, md or json"))),
    }
    Ok(0)
}

fn print_text(info: &ChatInfo, nodes: &[ChatNode], ids: bool, siblings: &[ChatSiblings]) {
    println!(
        "{} · {} · {}@{} · {}",
        info.addr(),
        info.label(),
        info.agent,
        info.profile,
        info.tools
    );
    if let Some(from) = info.imported_from() {
        println!("⇣ {from} (read-only: a message continues it as a fork)");
    }
    for n in nodes {
        if let Some(s) = siblings.iter().find(|s| s.node == n.id) {
            // Each with the agent that wrote it, where they differ (a compare).
            let named = |i: usize| match (s.label(), s.agents.get(i)) {
                (Some(_), Some(a)) if !a.is_empty() => format!("{} ({a})", short_id(&s.ids[i])),
                _ => short_id(&s.ids[i]),
            };
            let others: Vec<String> =
                (0..s.ids.len()).filter(|i| s.ids[*i] != n.id).map(named).collect();
            let who = s.label().map(|a| format!(" {a}")).unwrap_or_default();
            println!("\n  ‹ {}/{}{who} › others: {}", s.at, s.of, others.join(", "));
        }
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

/// `chat compare`: ask, wait for every agent (the progress on standard error), then each
/// answer under its agent.
#[allow(clippy::too_many_arguments)]
async fn compare(
    ctx: &Ctx,
    chat: String,
    agents: Vec<String>,
    text: Option<String>,
    yes: bool,
    no_wait: bool,
    timeout: Duration,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let req = Request::ChatCompare { chat, agents, text, confirmed: false };
    let info = chat_of(confirmed(&c, req, yes).await?)?;
    compare_wait(c, info, no_wait, timeout, json).await
}

/// Wait for a compare under way in `info` (the progress on standard error), then print each
/// answer under its agent.
async fn compare_wait(
    c: ptyro_client::Client,
    info: ChatInfo,
    no_wait: bool,
    timeout: Duration,
    json: bool,
) -> Result<i32, Error> {
    let Some(mut last) = info.comparing.clone() else {
        return Err(Error::internal("the daemon started no compare"));
    };
    if no_wait {
        eprintln!("{} (`ptyro chat show {} --branches` shows them)", last.line(), info.addr());
        return Ok(0);
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut said = String::new();
    loop {
        let line = last.line();
        if line != said && !json {
            eprintln!("{line}");
            said = line;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::new(
                ErrorCode::Timeout,
                format!(
                    "still comparing; `ptyro chat show {} --branches` shows it later",
                    info.addr()
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        match c.request(Request::ChatRead { chat: info.id.clone(), all: false }).await? {
            Reply::ChatTranscript { info: now, .. } => match now.comparing {
                Some(x) => last = x,
                // Over: how each ended is in the notice it left (read below).
                None => break,
            },
            r => return Err(unexpected(r)),
        }
    }
    let (now, nodes, siblings) =
        match c.request(Request::ChatRead { chat: info.id.clone(), all: true }).await? {
            Reply::ChatTranscript { info, nodes, siblings } => (info, nodes, siblings),
            r => return Err(unexpected(r)),
        };
    // The outcome, as the notice under the shown answer says it: `compared: claude ✓ · codex ✗
    // why`, for the agents last seen still asking.
    if let Some(ChatBody::Notice { text, .. }) = nodes
        .iter()
        .rev()
        .map(|n| &n.body)
        .find(|b| matches!(b, ChatBody::Notice { text, .. } if text.starts_with("compared:")))
    {
        said = text.clone();
        let line = text.trim_start_matches("compared:").split(" — ").next().unwrap_or_default();
        for part in line.split(" · ").map(str::trim) {
            let Some((agent, rest)) = part.split_once(' ') else { continue };
            let Some(a) = last.agents.iter_mut().find(|a| a.agent == agent) else { continue };
            let (mark, note) = rest.split_once(' ').unwrap_or((rest, ""));
            a.state = match mark {
                "✓" => "done",
                "✗" => "failed",
                "⊘" => "stopped",
                _ => continue,
            }
            .into();
            a.note = note.to_owned();
        }
    }
    // Each answer: a child of the message, followed down its newest children.
    let kids: Vec<&ChatNode> =
        nodes.iter().filter(|n| n.parent.as_deref() == Some(last.node.as_str())).collect();
    let label_of = |id: &str| {
        siblings.iter().find(|s| s.ids.iter().any(|i| i == id)).and_then(|s| {
            let i = s.ids.iter().position(|x| x == id)?;
            s.agents.get(i).cloned().filter(|a| !a.is_empty())
        })
    };
    let done: Vec<&str> =
        last.agents.iter().filter(|a| a.state == "done").map(|a| a.agent.as_str()).collect();
    let mut answers = Vec::new();
    for (k, kid) in kids.iter().enumerate() {
        let mut text = Vec::new();
        let mut at = Some(*kid);
        while let Some(n) = at {
            if let ChatBody::Assistant { text: t, .. } = &n.body {
                text.push(t.trim().to_owned());
            }
            at = nodes.iter().rev().find(|x| x.parent.as_deref() == Some(n.id.as_str()));
        }
        let agent = label_of(&kid.id)
            .or_else(|| (kids.len() == 1).then(|| done.first().map(|a| (*a).to_owned())).flatten())
            .unwrap_or_else(|| format!("answer {}", k + 1));
        answers
            .push(serde_json::json!({ "agent": agent, "node": kid.id, "text": text.join("\n\n") }));
    }
    // Every agent asked this time failed (or was stopped): an older answer is no success.
    let failed = !last.agents.is_empty()
        && last.agents.iter().all(|a| matches!(a.state.as_str(), "failed" | "stopped"));
    let code = if answers.is_empty() || failed { 1 } else { 0 };
    if json {
        let out = serde_json::json!({
            "chat": now,
            "message": last.node,
            "answers": answers,
            "agents": last.agents,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return Ok(code);
    }
    for a in &answers {
        println!("── {} ──", a["agent"].as_str().unwrap_or_default());
        println!("{}\n", a["text"].as_str().unwrap_or_default());
    }
    eprintln!("{said}");
    eprintln!(
        "`ptyro chat pick {} NODE` keeps one (`chat show {} --branches --ids` lists them)",
        now.addr(),
        now.addr()
    );
    Ok(code)
}

/// `chat import`: what was imported (or, dry, would be), per agent and profile.
async fn import(
    ctx: &Ctx,
    agent: String,
    profile: Option<String>,
    since: Option<String>,
    dry_run: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    if !json {
        eprintln!("{} your agents' histories…", if dry_run { "Counting" } else { "Importing" });
    }
    let req = Request::ChatImport { agent: Some(agent), profile, since, dry_run };
    let report = match c.request(req).await? {
        Reply::ChatImported { report } => report,
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        return Ok(0);
    }
    for s in &report.sources {
        println!(
            "{:<7} {:<10} {:>4} new {:>4} grown {:>5} unchanged {:>4} skipped {:>7} messages  {}",
            s.agent, s.profile, s.new, s.grown, s.unchanged, s.skipped, s.messages, s.dir
        );
    }
    if report.sources.is_empty() {
        println!(
            "no agent history found (Claude Code, Codex and Gemini keep theirs in their config folders)"
        );
    }
    for n in &report.notes {
        eprintln!("· {n}");
    }
    eprintln!("{}", report.summary());
    if !dry_run && report.sources.iter().any(|s| s.new + s.grown > 0) {
        eprintln!("`ptyro chat ls --imported` lists them; `ptyro chat search` finds what they say");
    }
    Ok(0)
}

/// A node's id as people use it: its last 8 characters (`chat edit … NODE` takes them).
fn short_id(id: &str) -> String {
    id[id.len().saturating_sub(8)..].to_owned()
}

#[allow(clippy::too_many_arguments)]
async fn fork(
    ctx: &Ctx,
    chat: String,
    at: Option<String>,
    agent: Option<String>,
    side: bool,
    keep: bool,
    prompt: String,
    json: bool,
) -> Result<i32, Error> {
    if json && side && !keep {
        return Err(Error::new(
            ErrorCode::Invalid,
            "a side chat goes after its answer, so it has no JSON to give: add --keep",
        ));
    }
    let c = ctx.connect(true).await?;
    let info = chat_of(c.request(Request::ChatFork { chat, node: at, agent, side }).await?)?;
    if keep {
        c.request(Request::ChatUpdate {
            chat: info.id.clone(),
            title: None,
            pinned: None,
            archived: None,
            agent: None,
            preset: None,
            keep: Some(true),
            folders: None,
        })
        .await?;
    }
    if json {
        // The chat at once; its answer arrives there (`chat show`).
        if !prompt.trim().is_empty() {
            c.request(Request::ChatSend { chat: info.id.clone(), text: prompt }).await?;
        }
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
        return Ok(0);
    }
    eprintln!("{} · {} · {}", info.addr(), info.label(), info.agent);
    if prompt.trim().is_empty() {
        if side && !keep {
            eprintln!(
                "(a side chat with nothing to ask: `ptyro chat rm {}` when done)",
                info.addr()
            );
        }
        return Ok(0);
    }
    let known = known_nodes(&c, &info.id).await?;
    c.request(Request::ChatSend { chat: info.id.clone(), text: prompt }).await?;
    let ok = tokio::select! {
        r = wait(c, &info, &known, Duration::from_secs(600), true) => r,
        _ = tokio::signal::ctrl_c() => Err(Error::new(ErrorCode::Invalid, "interrupted")),
    };
    // A side chat goes after its answer, or a Ctrl-C (SPEC §22.7), unless kept.
    if side
        && !keep
        && let Ok(c) = ctx.connect(false).await
    {
        let _ = c.request(Request::ChatDelete { chat: info.id.clone() }).await;
    }
    ok.map(|ok| if ok { 0 } else { 1 })
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
                "--chat" => self.chat = Some(value(&mut it)?),
                "-w" | "--workspace" => self.workspace = Some(value(&mut it)?),
                "--agent" => self.agent = Some(value(&mut it)?),
                "-n" | "--limit" => {
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
    if hits.is_empty() {
        eprintln!("nothing matches");
        return Ok(1);
    }
    // Matches bold on a terminal; between * elsewhere.
    let (on, off) =
        if std::io::stdout().is_terminal() { ("\x1b[1m", "\x1b[0m") } else { ("*", "*") };
    for h in &hits {
        let cite = match &h.node {
            Some(n) => format!("{}/{}", h.chat.addr(), short_id(n)),
            None => h.chat.addr(),
        };
        println!("{cite}  {} · {} · {}", h.chat.label(), h.chat.agent, age(h.at_ms));
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
    let (info, _) = match c.request(Request::ChatRead { chat: chat.to_owned(), all: false }).await?
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
        preset: None,
        keep: None,
        folders: None,
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

async fn update(ctx: &Ctx, chat: &str, f: impl FnOnce(&mut Request)) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mut req = blank(chat);
    f(&mut req);
    let info = chat_of(c.request(req).await?)?;
    eprintln!("{} · {}", info.addr(), info.label());
    Ok(0)
}

async fn each(
    ctx: &Ctx,
    chats: Vec<String>,
    what: &str,
    f: impl Fn(&mut Request),
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    for chat in chats {
        let mut req = blank(&chat);
        f(&mut req);
        let info = chat_of(c.request(req).await?)?;
        eprintln!("{what}: {} ({})", info.addr(), info.label());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::SearchArgs;

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
        let q: Vec<String> = "redis -budget -w work --chat=c:7f3k -n 3 --agent codex --json -- -n"
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
        assert!(args().take_flags(&["x".into(), "--chat".into()]).is_err());
        assert!(args().take_flags(&["x".into(), "-n".into(), "lots".into()]).is_err());
    }
}
