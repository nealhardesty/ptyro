//! `ptyro chat` (SPEC §22.12): plain conversations with your agents, from any shell. A chat is
//! kept (in `chat.db`) and shows in the TUI's and the web's CHATS section too; `ptyro chat ask`
//! is the one-shot that keeps nothing (`git diff | ptyro chat ask "review this"`).

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_core::chat::{ChatBody, ChatItem};
use ptyro_core::chats::{ChatInfo, ChatNode};
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
        /// text (default), md or json
        #[arg(long, default_value = "text")]
        format: String,
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
        no_wait: false,
        json: false,
    });
    match cmd {
        ChatCmd::New { prompt, agent, preset, workspace, title, no_wait, json } => {
            let text = words_or_stdin(prompt)?;
            new(ctx, text, agent, preset, workspace, title, no_wait, json).await
        }
        ChatCmd::Ask { question, keep, agent, preset, timeout } => {
            ask(ctx, question.join(" "), keep, agent, preset, timeout).await
        }
        ChatCmd::Ls { workspace, pinned, archived, limit, json } => {
            ls(ctx, workspace, pinned, archived, limit, json).await
        }
        ChatCmd::Show { chat, all, format } => show(ctx, chat, all, &format).await,
        ChatCmd::Open { chat } => open(ctx, &chat).await,
        ChatCmd::Send { chat, text, no_wait, timeout } => {
            let c = ctx.connect(true).await?;
            let info = chat_of(c.request(Request::ChatSend { chat, text: text.join(" ") }).await?)?;
            if no_wait {
                return Ok(0);
            }
            wait(c, &info, Duration::from_secs(timeout), true).await.map(|_| 0)
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
    no_wait: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let prompt = Some(text.clone()).filter(|t| !t.trim().is_empty());
    let has_prompt = prompt.is_some();
    let info =
        chat_of(c.request(Request::ChatNew { workspace, agent, preset, title, prompt }).await?)?;
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
    wait(c, &info, Duration::from_secs(600), true).await.map(|_| 0)
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
    let c = ctx.connect(true).await?;
    let info = chat_of(
        c.request(Request::ChatNew {
            workspace: None,
            agent,
            preset,
            title: None,
            prompt: Some(text),
        })
        .await?,
    )?;
    let addr = info.addr();
    let answered = wait(c, &info, Duration::from_secs(timeout), false).await;
    let c = ctx.connect(false).await?;
    if keep {
        eprintln!("kept as {addr}");
    } else {
        let _ = c.request(Request::ChatDelete { chat: info.id.clone() }).await;
    }
    answered.map(|ok| if ok { 0 } else { 1 })
}

/// Follow a chat until the answer to its last message is complete, printing it (`stream`: as
/// it arrives; else once, the final answer only). Returns whether the turn ended well.
async fn wait(
    mut c: ptyro_client::Client,
    info: &ChatInfo,
    timeout: Duration,
    stream: bool,
) -> Result<bool, Error> {
    let mut pushes = c.pushes();
    c.send(ClientMsg::ChatSubscribe { chat: info.id.clone() })?;
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
        for it in got {
            if !items.contains_key(&it.id) {
                order.push(it.id.clone());
            }
            items.insert(it.id.clone(), it);
        }
        if after.is_none() {
            after = order
                .iter()
                .rev()
                .find(|id| matches!(items[*id].body, ChatBody::User { .. }))
                .cloned();
        }
        let Some(start) = &after else { continue };
        let tail = order.iter().skip_while(|id| *id != start).skip(1);
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
                ChatBody::Notice { text, .. } if !printed.contains_key(id) => {
                    eprintln!("! {text}");
                    printed.insert(id.clone(), 0);
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
    limit: Option<u32>,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let all = workspace.is_none();
    let chats =
        match c.request(Request::ChatList { workspace, all, pinned, archived, limit }).await? {
            Reply::Chats { chats } => chats,
            r => return Err(unexpected(r)),
        };
    if json {
        println!("{}", serde_json::to_string_pretty(&chats).unwrap_or_default());
        return Ok(0);
    }
    if chats.is_empty() {
        eprintln!("no chats yet: `ptyro chat \"your question\"` starts one");
        return Ok(0);
    }
    for ch in &chats {
        let mut marks = String::new();
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

async fn show(ctx: &Ctx, chat: String, all: bool, format: &str) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let (info, nodes) = match c.request(Request::ChatRead { chat, all }).await? {
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
        "text" => print_text(&info, &nodes),
        f => return Err(Error::invalid(format!("{f:?}: text, md or json"))),
    }
    Ok(0)
}

fn print_text(info: &ChatInfo, nodes: &[ChatNode]) {
    println!(
        "{} · {} · {}@{} · {}",
        info.addr(),
        info.label(),
        info.agent,
        info.profile,
        info.tools
    );
    for n in nodes {
        match &n.body {
            ChatBody::User { text } => println!("\n› {}", text.trim()),
            ChatBody::Assistant { text, .. } => println!("\n{}", text.trim()),
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

/// The chat view: the TUI, opened on this chat.
async fn open(ctx: &Ctx, chat: &str) -> Result<i32, Error> {
    crate::cli::check_attachable()?;
    let c = ctx.connect(true).await?;
    let (info, _) = match c.request(Request::ChatRead { chat: chat.to_owned(), all: false }).await?
    {
        Reply::ChatTranscript { info, nodes } => (info, nodes),
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
