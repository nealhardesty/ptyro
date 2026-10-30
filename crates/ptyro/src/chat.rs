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
        /// What it may use: chat (web search, the default), read, none, or your own
        #[arg(long)]
        preset: Option<String>,
        /// The workspace it belongs to (default main)
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Keep nothing: not saved, not searchable, gone when its agent stops
        #[arg(long)]
        incognito: bool,
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
    /// match), "a phrase", prefix*, -word, agent:codex, since:7d, workspace:work
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
    /// Change the agent or the tool preset; the next message carries the conversation over
    Set {
        #[arg(add = ArgValueCandidates::new(complete::chats))]
        chat: String,
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Delete chats (gone at once from every list)
    #[command(alias = "delete")]
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(complete::chats))]
        chats: Vec<String>,
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
        no_wait: false,
        json: false,
    });
    match cmd {
        ChatCmd::New { prompt, agent, preset, workspace, title, incognito, no_wait, json } => {
            let text = words_or_stdin(prompt)?;
            new(ctx, text, agent, preset, workspace, title, incognito, no_wait, json).await
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
        ChatCmd::Search { query, chat, workspace, agent, pinned, archived, here, limit, json } => {
            // (Words may start with `-`, so the flags typed after them arrive as words.)
            let mut a = SearchArgs { chat, workspace, agent, pinned, archived, here, limit, json };
            let words = a.take_flags(&query)?;
            search(
                ctx,
                words.join(" "),
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
        ChatCmd::Ls { workspace, pinned, archived, machine, limit, json } => {
            ls(ctx, workspace, pinned, archived, machine, limit, json).await
        }
        ChatCmd::Show { chat, all, ids, branches, format } => {
            show(ctx, chat, all, ids, branches, &format).await
        }
        ChatCmd::Open { chat } => open(ctx, &chat).await,
        ChatCmd::Send { chat, text, no_wait, timeout } => {
            let c = ctx.connect(true).await?;
            // What was there before: the answer is what follows this message, not an earlier one.
            let known: HashSet<String> = match c
                .request(Request::ChatRead { chat: chat.clone(), all: true })
                .await?
            {
                Reply::ChatTranscript { nodes, .. } => nodes.into_iter().map(|n| n.id).collect(),
                r => return Err(unexpected(r)),
            };
            let info = chat_of(c.request(Request::ChatSend { chat, text: text.join(" ") }).await?)?;
            if no_wait {
                return Ok(0);
            }
            let ok = wait(c, &info, &known, Duration::from_secs(timeout), true).await?;
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
        ChatCmd::Set { chat, agent, preset } => {
            if agent.is_none() && preset.is_none() {
                return Err(Error::invalid("what should change? --agent A, --preset P"));
            }
            update(ctx, &chat, |r| {
                if let Request::ChatUpdate { agent: a, preset: p, .. } = r {
                    *a = agent;
                    *p = preset;
                }
            })
            .await
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

#[allow(clippy::too_many_arguments)]
async fn new(
    ctx: &Ctx,
    text: String,
    agent: Option<String>,
    preset: Option<String>,
    workspace: Option<String>,
    title: Option<String>,
    incognito: bool,
    no_wait: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let prompt = Some(text.clone()).filter(|t| !t.trim().is_empty());
    let has_prompt = prompt.is_some();
    let req = Request::ChatNew { workspace, agent, preset, title, prompt, incognito };
    let info = chat_of(c.request(req).await?)?;
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
    eprintln!("{} · {} · {}", info.addr(), info.agent, info.tools);
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
    if let Some(text) = send {
        c.request(Request::ChatSend { chat: info.id.clone(), text }).await?;
    }
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
    // (With a machine, the limit applies after it is picked.)
    let asked = if machine.is_some() { None } else { limit };
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
        if let Some(n) = limit {
            chats.truncate(n as usize);
        }
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
    for n in nodes {
        if let Some(s) = siblings.iter().find(|s| s.node == n.id) {
            let others: Vec<String> =
                s.ids.iter().filter(|i| **i != n.id).map(|i| short_id(i)).collect();
            println!("\n  ‹ {}/{} › others: {}", s.at, s.of, others.join(", "));
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
    let hits = match c.request(req).await? {
        Reply::ChatHits { hits } => hits,
        r => return Err(unexpected(r)),
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
        println!("    {snippet}");
    }
    Ok(0)
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
        assert!(args().take_flags(&["x".into(), "--chat".into()]).is_err());
        assert!(args().take_flags(&["x".into(), "-n".into(), "lots".into()]).is_err());
    }
}
