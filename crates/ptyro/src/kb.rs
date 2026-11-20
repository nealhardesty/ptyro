//! `ptyro recall` and `ptyro kb` (SPEC §25.10): the knowledge base from the command line. Search
//! it, open what a search found, see the history, write and list notes, forget, and see how it
//! stands.

use std::io::IsTerminal;

use clap::{Args, Subcommand};
use clap_complete::ArgValueCandidates;
use ptyro_core::chats::{MARK_OFF, MARK_ON};
use ptyro_core::kb::card::minute;
use ptyro_core::kb::search::{HitKind, KbHit, KbListed, KbStatusInfo, pieces};
use ptyro_core::kb::{ItemKind, KbChunk, Writer};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};
use serde_json::json;

use crate::cli::{Ctx, ago, unexpected};
use crate::complete;

/// `ptyro recall`'s arguments.
#[derive(Args, Debug)]
pub struct RecallArgs {
    /// What to look for: words (all must match), "a phrase", prefix*, -word, and the filters
    /// kind:session, project:NAME, since:7d, before:30d, agent:codex, workspace:work,
    /// file:PATH, wrote:you|agent, by:meaning. (A word may start with `-`: it is left out.
    /// After `--`, every word is a word.)
    #[arg(required = true, allow_hyphen_values = true)]
    query: Vec<String>,
    /// Only this project's
    #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
    project: Option<String>,
    /// Only these kinds: session, chat, note, doc, comment, code, path, commit, command
    /// (several with commas)
    #[arg(short, long, value_name = "KIND,…")]
    kind: Option<String>,
    /// Only what is newer than this: 7d, 12h, 2w
    #[arg(long, value_name = "AGE")]
    since: Option<String>,
    /// Only what is older than this
    #[arg(long, value_name = "AGE")]
    before: Option<String>,
    /// Only what is about it, by meaning (≈), not by its words (Knowledge → meaning)
    #[arg(long, conflicts_with = "words")]
    meaning: bool,
    /// Only what has the words, even when meaning search is on
    #[arg(long)]
    words: bool,
    /// Only this machine's: a linked machine's name (the query's `machine:` says the same)
    #[arg(long, value_name = "MACHINE", conflicts_with = "here", add = ArgValueCandidates::new(complete::machines))]
    machine: Option<String>,
    /// This computer only: linked machines aren't asked
    #[arg(long)]
    here: bool,
    /// Only what you wrote (`you`), or what agents did (`agent`); the query's `wrote:` says
    /// the same
    #[arg(long, value_name = "WHO", value_parser = ["you", "agent"])]
    wrote: Option<String>,
    /// At most N (100 at most)
    #[arg(short, long, default_value_t = 20)]
    limit: u32,
    /// The hits as JSON: the list of `KbHit`
    #[arg(long)]
    json: bool,
}

impl RecallArgs {
    /// The query's words, with the flags typed among them applied: its words may start with
    /// `-`, so clap hands everything after the first word over as a word (as `chat search`).
    fn take_flags(&mut self) -> Result<Vec<String>, Error> {
        let mut words = Vec::new();
        let query = std::mem::take(&mut self.query);
        let mut it = query.into_iter();
        while let Some(w) = it.next() {
            let (name, inline) = match w.split_once('=') {
                Some((n, v)) if n.starts_with('-') => (n.to_owned(), Some(v.to_owned())),
                _ => (w.clone(), None),
            };
            let mut value = || {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| Error::new(ErrorCode::Invalid, format!("{name} needs a value")))
            };
            match name.as_str() {
                "--" => words.extend(it.by_ref()),
                "--json" => self.json = true,
                "--meaning" => self.meaning = true,
                "--words" => self.words = true,
                "-p" | "--project" => self.project = Some(value()?),
                "-k" | "--kind" => self.kind = Some(value()?),
                "--since" => self.since = Some(value()?),
                "--before" => self.before = Some(value()?),
                "--machine" => self.machine = Some(value()?),
                "--here" => self.here = true,
                "--wrote" => self.wrote = Some(value()?),
                "-l" | "--limit" => {
                    let v = value()?;
                    self.limit = v.parse().map_err(|_| {
                        Error::new(ErrorCode::Invalid, format!("{name}: {v:?} is not a number"))
                    })?;
                }
                _ => words.push(w),
            }
        }
        if self.meaning && self.words {
            return Err(Error::invalid(
                "--meaning and --words can't go together: one or the other",
            ));
        }
        if self.here && self.machine.is_some() {
            return Err(Error::invalid("--here and --machine can't go together: one or the other"));
        }
        Ok(words)
    }
}

#[derive(Subcommand, Debug)]
pub enum KbCmd {
    /// Whether it is on, what it holds (items by kind, its size, what can be found by
    /// meaning), how long sessions are kept, and what was not kept and why
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Open what a search found: a piece with its neighbours (`k:7f3k/12`), or a whole item
    /// (`k:7f3k`); a linked machine's by its name in front (`laptop2:k:7f3k`)
    Show {
        /// An address from `ptyro recall` or `ptyro kb recent`
        #[arg(value_name = "REF")]
        addr: String,
        /// How many pieces to show each side of the one named (default 2)
        #[arg(long, value_name = "N")]
        context: Option<u32>,
        /// Open its source instead: a session in the TUI (resumed if it can be), a chat in
        /// its view, a note or a file in your editor (a file at the line of the piece), a
        /// commit as `git show`
        #[arg(long, conflicts_with_all = ["context", "json"])]
        open: bool,
        #[arg(long)]
        json: bool,
    },
    /// The history: what was kept, newest first, by day
    Recent {
        /// Only these kinds: session, file, commit, note, shell (several with commas)
        #[arg(short, long, value_name = "KIND,…")]
        kind: Option<String>,
        /// Only this project's
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// A linked machine's history instead of this computer's
        #[arg(long, value_name = "MACHINE", add = ArgValueCandidates::new(complete::machines))]
        machine: Option<String>,
        /// Only what you wrote (`you`), or what agents did (`agent`)
        #[arg(long, value_name = "WHO", value_parser = ["you", "agent"])]
        wrote: Option<String>,
        /// At most N (100 at most)
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Write a note: a decision, a fact, a how-to, as a Markdown file in ptyro's notes folder
    /// that every search and every agent's `recall` finds first. With no text (or `--edit`)
    /// it opens your editor; `-` reads the text from stdin
    Note {
        /// The note. Its first heading (`# …`), else its first line, is its title
        #[arg(value_name = "TEXT")]
        text: Vec<String>,
        /// Its title, put above the text as its heading
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// The project it is about (default: none, a global note)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Only this profile's agents may see it (default: every profile's)
        #[arg(long, add = ArgValueCandidates::new(complete::profiles))]
        profile: Option<String>,
        /// A tag, found by search (several: `--tag db --tag decision`)
        #[arg(long, value_name = "TAG")]
        tag: Vec<String>,
        /// Write it in your editor ($VISUAL, $EDITOR), starting from the text given
        #[arg(long)]
        edit: bool,
        #[arg(long)]
        json: bool,
    },
    /// The notes, by project: each with its address, who wrote it and when. `kb show REF
    /// --open` edits one in your editor; `kb forget REF` deletes one
    Notes {
        /// Only this project's (`_global`: the ones about no project)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Only what you wrote (`you`), or what agents did (`agent`)
        #[arg(long, value_name = "WHO", value_parser = ["you", "agent"])]
        wrote: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// What is read into the knowledge base: every project and every folder or file given to
    /// `ptyro ingest`, with what each holds, when it was last read, and what went wrong
    Ls {
        /// A linked machine's instead of this computer's
        #[arg(long, value_name = "MACHINE", add = ArgValueCandidates::new(complete::machines))]
        machine: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Look again now, for what changed: at the notes folder and everything `kb ls` lists,
    /// or only at what you name (ptyro also looks by itself: at the notes as they change, at
    /// a project when an agent's turn ends in it, at everything every `kb.sync_every`)
    Sync {
        /// Only these (names from `ptyro kb ls`, or paths)
        #[arg(value_name = "ROOT")]
        roots: Vec<String>,
        /// A linked machine's instead of this computer's
        #[arg(long, value_name = "MACHINE", add = ArgValueCandidates::new(complete::machines))]
        machine: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Stop reading a folder or file given to `ptyro ingest`, and drop what was kept of it
    /// (your files aren't touched; ingest it again and it is back). Asks first
    Rm {
        /// Its name from `ptyro kb ls`, or its path
        #[arg(value_name = "ROOT")]
        root: String,
        /// A linked machine's instead of this computer's
        #[arg(long, value_name = "MACHINE", add = ArgValueCandidates::new(complete::machines))]
        machine: Option<String>,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete items and their text for good: nothing of them is kept again. Asks first
    Forget {
        /// Addresses from `ptyro recall` or `ptyro kb recent` (`k:7f3k`)
        #[arg(value_name = "REF", required_unless_present_any = ["query", "session"])]
        addrs: Vec<String>,
        /// Everything this search finds (it lists them, then asks)
        #[arg(short, long, value_name = "QUERY")]
        query: Option<String>,
        /// Everything kept of this session, which is private from then on (nothing more of it
        /// is kept; that can't be switched off again)
        #[arg(long, value_name = "SESSION", conflicts_with_all = ["addrs", "query"], add = ArgValueCandidates::new(crate::complete::sessions))]
        session: Option<String>,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

/// A comma list of kinds, each one of `T`'s words.
fn kinds<T: Copy>(
    list: Option<&str>,
    parse: fn(&str) -> Option<T>,
    all: &[T],
    word: fn(T) -> &'static str,
) -> Result<Vec<T>, Error> {
    let mut out = Vec::new();
    for k in list.into_iter().flat_map(|l| l.split(',')).map(str::trim).filter(|k| !k.is_empty()) {
        out.push(parse(k).ok_or_else(|| {
            let known: Vec<&str> = all.iter().map(|k| word(*k)).collect();
            Error::invalid(format!("no kind is called {k:?}: {}", known.join(", ")))
        })?);
    }
    Ok(out)
}

/// `--wrote`'s word as the writer it names.
fn wrote(who: Option<&str>) -> Option<Writer> {
    who.map(|w| if w == "you" { Writer::User } else { Writer::Agent })
}

/// The knowledge base is off: on a terminal, offer to switch it on (in your config, as the
/// settings panel would); else say how.
fn off(ctx: &Ctx, e: Error) -> Error {
    let how = "`ptyro config set kb.enabled true`";
    if ctx.remote.is_some() || !crate::cli::confirm("The knowledge base is off. Switch it on?") {
        return Error::new(e.code, format!("the knowledge base is off: {how} switches it on"));
    }
    match crate::config::set_user(&ctx.paths, "kb.enabled", "true") {
        Ok(()) => Error::new(
            ErrorCode::Unsupported,
            "the knowledge base is on now: agent sessions are kept from here on. Run that again.",
        ),
        Err(why) => Error::new(ErrorCode::Invalid, why),
    }
}

/// Send a request of the knowledge base's; when it is off, say so and offer to switch it on.
pub(crate) async fn ask(ctx: &Ctx, c: &ptyro_client::Client, req: Request) -> Result<Reply, Error> {
    match c.request(req).await {
        Err(e)
            if e.code == ErrorCode::Unsupported && e.message.contains("knowledge base is off") =>
        {
            Err(off(ctx, e))
        }
        // A daemon from before the knowledge base's commands.
        Err(e)
            if e.code == ErrorCode::Invalid && e.message.contains("couldn't read this request") =>
        {
            Err(Error::new(
                ErrorCode::Unsupported,
                "the running daemon is older than this ptyro and has no knowledge base commands: `ptyro daemon upgrade` brings it up to date (sessions keep running)",
            ))
        }
        other => other,
    }
}

/// Matches bold on a terminal; between `*` elsewhere.
fn marked(snippet: &str) -> String {
    let (on, off) =
        if std::io::stdout().is_terminal() { ("\x1b[1m", "\x1b[0m") } else { ("*", "*") };
    snippet.replace('\n', " ").replace(MARK_ON, on).replace(MARK_OFF, off)
}

/// A hit's address as people type it: a chat's message by the last 8 characters of its id.
fn cite(h: &KbHit) -> String {
    match (&h.node, h.addr.split_once('/')) {
        (Some(n), Some((chat, _))) => format!("{chat}/{}", &n[n.len().saturating_sub(8)..]),
        _ => h.addr.clone(),
    }
}

/// What a hit is and where it is from, on one line: `session · retry policy · feeder · claude · 3d`.
fn about(kind: &str, title: Option<&str>, place: &[Option<&str>], at_ms: Option<u64>) -> String {
    let mut parts = vec![kind.to_owned()];
    parts.extend(title.map(|t| t.replace('\n', " ")));
    parts.extend(place.iter().flatten().map(|p| (*p).to_owned()));
    parts.extend(at_ms.map(ago));
    parts.join(" · ")
}

pub async fn recall(ctx: &Ctx, mut a: RecallArgs) -> Result<i32, Error> {
    let mut query = a.take_flags()?.join(" ");
    if query.trim().is_empty() {
        return Err(Error::invalid("say what to look for (words, \"a phrase\", prefix*)"));
    }
    for (word, value) in [("since", &a.since), ("before", &a.before), ("wrote", &a.wrote)] {
        if let Some(v) = value {
            query.push_str(&format!(" {word}:{v}"));
        }
    }
    if let Some(by) = a.meaning.then_some("meaning").or(a.words.then_some("words")) {
        query.push_str(&format!(" by:{by}"));
    }
    let c = ctx.connect(true).await?;
    let req = Request::KbSearch {
        query,
        project: a.project,
        kinds: kinds(a.kind.as_deref(), HitKind::parse, HitKind::ALL, HitKind::as_str)?,
        since_ms: None,
        before_ms: None,
        limit: Some(a.limit),
        // Where you are: what is of this project ranks higher.
        here: std::env::current_dir().ok().map(|d| d.display().to_string()),
        // Every linked machine that is up searches its own too (SPEC §25.11).
        machines: !a.here,
        machine: a.machine,
        profile: None,
    };
    let (hits, words_only, left_out) = match ask(ctx, &c, req).await {
        Ok(Reply::KbHits { hits, words_only, left_out }) => (hits, words_only, left_out),
        Ok(r) => return Err(unexpected(r)),
        // Meaning search is off: offer to switch it on (SPEC §25.10), as `chat search` does.
        Err(e) if e.code == ErrorCode::NeedsConfirmation => return meaning_on(ctx, e),
        Err(e) => return Err(e),
    };
    // A machine that didn't answer is said, and the search is still an answer.
    for m in &left_out {
        eprintln!("(not searched: {m})");
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&hits).unwrap_or_default());
        return Ok(0);
    }
    // Nothing found is an answer, not a failure (as `ls` with nothing to list).
    if hits.is_empty() {
        eprintln!("nothing matches");
        return Ok(0);
    }
    let lines = std::io::stdout().is_terminal();
    for h in &hits {
        let place = [h.machine.as_deref(), h.project_name.as_deref(), h.agent.as_deref()];
        let what = about(h.kind.as_str(), h.title.as_deref(), &place, Some(h.at_ms));
        // ≈: found by meaning, none of the words is in it.
        let how = if h.how == "meaning" { "≈ " } else { "" };
        if lines {
            println!("{}  {what}", cite(h));
            println!("    {how}{}", marked(&h.snippet));
        } else {
            // One hit a line, for a pipe.
            println!("{}  {what}  {how}{}", cite(h), marked(&h.snippet));
        }
    }
    if a.meaning || a.words {
        return Ok(0);
    }
    if let Some(why) = words_only.filter(|_| lines) {
        eprintln!("({why})");
    }
    Ok(0)
}

/// `--meaning` with meaning search off: on a terminal, ask to switch it on.
fn meaning_on(ctx: &Ctx, e: Error) -> Result<i32, Error> {
    if ctx.remote.is_some() || !crate::cli::confirm(&format!("{}. Switch it on?", e.message)) {
        return Err(Error::new(
            ErrorCode::Invalid,
            format!("{} (`ptyro config set kb.meaning true`)", e.message),
        ));
    }
    crate::config::set_user(&ctx.paths, "kb.meaning", "true")
        .map_err(|e| Error::new(ErrorCode::Invalid, e))?;
    eprintln!(
        "meaning search is on: its model is downloading, then what is kept is embedded \
         (`ptyro kb` shows how far). Search by words meanwhile."
    );
    Ok(1)
}

pub async fn run(ctx: &Ctx, cmd: Option<KbCmd>, json: bool) -> Result<i32, Error> {
    match cmd.unwrap_or(KbCmd::Status { json }) {
        KbCmd::Status { json } => status(ctx, json).await,
        KbCmd::Show { addr, open: true, .. } => open(ctx, &addr).await,
        KbCmd::Show { addr, context, json, .. } => show(ctx, addr, context, json).await,
        KbCmd::Note { text, title, project, profile, tag, edit, json } => {
            note(ctx, text, title, project, profile, tag, edit, json).await
        }
        KbCmd::Notes { project, wrote: who, json } => notes(ctx, project, who, json).await,
        KbCmd::Ls { machine, json } => crate::ingest::list(ctx, machine, json).await,
        KbCmd::Sync { roots, machine, json } => {
            crate::ingest::sync(ctx, roots, machine, json).await
        }
        KbCmd::Rm { root, machine, yes, json } => {
            crate::ingest::remove(ctx, root, machine, yes, json).await
        }
        KbCmd::Recent { kind, project, machine, wrote: who, limit, json } => {
            let kinds = kinds(kind.as_deref(), ItemKind::parse, ItemKind::ALL, ItemKind::as_str)?;
            let c = ctx.connect(true).await?;
            let req = Request::KbRecent {
                kinds,
                project,
                session: None,
                worktree: None,
                since_ms: None,
                before_ms: None,
                limit: Some(limit),
                machine,
                writer: wrote(who.as_deref()),
            };
            let items = match ask(ctx, &c, req).await? {
                Reply::KbItems { items } => items,
                r => return Err(unexpected(r)),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
                return Ok(0);
            }
            if items.is_empty() {
                eprintln!("nothing is kept yet");
                return Ok(0);
            }
            let mut day = String::new();
            for i in &items {
                let when = minute(at(i));
                let (date, time) = when.split_once(' ').unwrap_or((&when, ""));
                if date != day {
                    println!("{date}");
                    day = date.to_owned();
                }
                println!("  {time}  {}  {}", i.addr, listed(i, false));
            }
            Ok(0)
        }
        KbCmd::Forget { session: Some(session), yes, json, .. } => {
            forget_session(ctx, session, yes, json).await
        }
        KbCmd::Forget { addrs, query, yes, json, session: None } => {
            forget(ctx, addrs, query, yes, json).await
        }
    }
}

/// When an item happened: its end, else its start, else when it was kept.
fn at(i: &KbListed) -> u64 {
    i.item.ended_ms.or(i.item.started_ms).or(i.item.ingested_ms).unwrap_or(0)
}

/// An item on one line: `session · retry policy · feeder · claude · 12 pieces`.
fn listed(i: &KbListed, with_age: bool) -> String {
    let place = [i.project_name.as_deref(), i.item.agent.as_deref()];
    let title = i.item.title.as_deref().or(i.item.rel.as_deref());
    let line = about(i.item.kind.as_str(), title, &place, with_age.then(|| at(i)));
    format!("{line} · {}", pieces(u64::from(i.item.chunks)))
}

async fn status(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let s = match ask(ctx, &c, Request::KbStatus).await? {
        Reply::KbStatus { status } => status,
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
        return Ok(0);
    }
    print!("{}", status_text(&s));
    Ok(0)
}

/// `ptyro kb`, in words.
fn status_text(s: &KbStatusInfo) -> String {
    s.lines().join("\n") + "\n"
}

async fn read(
    ctx: &Ctx,
    c: &ptyro_client::Client,
    addr: String,
    context: Option<u32>,
) -> Result<(KbListed, Vec<KbChunk>, Option<u32>, bool), Error> {
    let req = Request::KbRead { addr, before: context, after: context, profile: None };
    match ask(ctx, c, req).await? {
        Reply::KbDoc { item, chunks, at, stale } => Ok((item, chunks, at, stale)),
        r => Err(unexpected(r)),
    }
}

async fn show(ctx: &Ctx, addr: String, context: Option<u32>, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    // (`--context` is for a piece; an item is shown whole.)
    let context = context.filter(|_| addr.contains('/'));
    let (item, chunks, at, stale) = read(ctx, &c, addr, context).await?;
    if json {
        let doc = json!({ "item": item, "chunks": chunks, "at": at, "stale": stale });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return Ok(0);
    }
    println!("{}  {}", item.addr, listed(&item, true));
    println!("    {} · {}", item.item.uri, minute(self::at(&item)));
    if stale {
        println!("    (it was read again since: that piece is gone, so here it is from its start)");
    }
    for ch in &chunks {
        let mark = if at == Some(ch.idx) { "▶" } else { " " };
        let place = ch.heading.as_deref().map(|h| format!(" · {h}")).unwrap_or_default();
        println!("\n{mark} {}/{}  {}{place}", item.addr, ch.idx, ch.kind);
        for line in ch.text.lines() {
            println!("    {line}");
        }
    }
    Ok(0)
}

/// `kb show --open`: the source itself. A session opens in the TUI (resumed when it is
/// dormant and can be); a chat in its view; a note and a file in your editor; a commit as
/// `git show`.
async fn open(ctx: &Ctx, addr: &str) -> Result<i32, Error> {
    // (A linked machine's chat is `laptop2:c:7f3k`.)
    if addr.starts_with("c:") || addr.contains(":c:") {
        let chat = addr.split('/').next().unwrap_or(addr).to_owned();
        return crate::chat::open(ctx, &chat).await;
    }
    let c = ctx.connect(true).await?;
    let (item, chunks, at, _) = read(ctx, &c, addr.to_owned(), Some(0)).await?;
    let gone = || {
        let conversation = item.item.uri.rsplit('/').next().unwrap_or_default();
        Error::new(
            ErrorCode::NotFound,
            format!(
                "the session it happened in is no longer in ptyro; what was kept of it is `ptyro kb show {}`, and its agent may still carry it on by its own id ({conversation})",
                item.addr
            ),
        )
    };
    match (item.item.kind, &item.item.session) {
        (ItemKind::Session, Some(id)) => {
            let info = match c.request(Request::Get { addr: id.clone() }).await {
                Ok(Reply::Session { info }) => info,
                Ok(r) => return Err(unexpected(r)),
                Err(e) if e.code == ErrorCode::NotFound => return Err(gone()),
                Err(e) => return Err(e),
            };
            crate::cli::attach(ctx, c, Some(info), false, false).await
        }
        (ItemKind::Session, None) => Err(gone()),
        (ItemKind::Note, _) => edit_note(ctx, &c, &item.addr).await,
        (ItemKind::File | ItemKind::Commit, _) if ctx.remote.is_some() || far(&item.addr) => {
            Err(Error::invalid(format!(
                "that {} is on another computer: `ptyro kb show {}` shows what is kept of it",
                item.item.kind, item.addr
            )))
        }
        (ItemKind::File, _) => {
            let piece = at.and_then(|at| chunks.iter().find(|c| c.idx == at));
            open_file(&item, piece)
        }
        (ItemKind::Commit, _) => open_commit(ctx, &c, &item).await,
        (kind, _) => Err(Error::invalid(format!(
            "a {kind} has nothing to open from here yet: `ptyro kb show {}` shows what is kept of it",
            item.addr
        ))),
    }
}

/// An address under a linked machine's name (`laptop2:k:7f3k`).
fn far(addr: &str) -> bool {
    !addr.starts_with("k:")
}

/// The line of a file a piece stands at: a comment's or a window of code's own (`L120-L161`),
/// else the line of the heading a document's section is under.
fn line_of(path: &std::path::Path, piece: &KbChunk) -> Option<u32> {
    if let Some(n) = piece.anchor.as_deref().and_then(|a| a.strip_prefix('L')) {
        return n.split('-').next()?.parse().ok();
    }
    let heading = piece.heading.as_deref()?.rsplit(" › ").next()?.trim();
    let text = std::fs::read_to_string(path).ok()?;
    let at = text.lines().position(|l| {
        l.starts_with('#') && l.trim_start_matches('#').trim().eq_ignore_ascii_case(heading)
    })?;
    Some(at as u32 + 1)
}

/// `kb show REF --open` of a file: the file itself in your editor, at the piece's line.
fn open_file(item: &KbListed, piece: Option<&KbChunk>) -> Result<i32, Error> {
    let path = std::path::PathBuf::from(item.item.uri.strip_prefix("file://").unwrap_or_default());
    if !path.is_file() {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "{} is no longer there: `ptyro kb show {}` shows what was kept of it",
                path.display(),
                item.addr
            ),
        ));
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let line = piece.and_then(|p| line_of(&path, p));
    let argv = ptyro_core::shell::editor_command_at(&editor, &path, line);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::internal(format!("{editor}: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

/// `kb show REF --open` of a commit: `git show` of it, in its repository (git's own pager).
async fn open_commit(ctx: &Ctx, c: &ptyro_client::Client, item: &KbListed) -> Result<i32, Error> {
    // `commit://<root id>/<sha>`: the root says where the repository is.
    let rest = item.item.uri.strip_prefix("commit://").unwrap_or_default();
    let (root, sha) = rest.split_once('/').unwrap_or((rest, ""));
    let roots = match ask(ctx, c, Request::KbRoots { machine: None }).await? {
        Reply::KbRoots { roots } => roots,
        r => return Err(unexpected(r)),
    };
    let Some(dir) = roots.iter().find(|r| r.root.id == root).map(|r| r.root.path.clone()) else {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "its repository is no longer read: `ptyro kb show {}` shows what was kept of it",
                item.addr
            ),
        ));
    };
    let status = std::process::Command::new("git")
        .args(["-C", &dir, "show", sha])
        .status()
        .map_err(|e| Error::internal(format!("git: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

async fn forget(
    ctx: &Ctx,
    addrs: Vec<String>,
    query: Option<String>,
    yes: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mut ids: Vec<String> = Vec::new();
    let mut more = false;
    if yes {
        ids = addrs;
    } else {
        // Asked first, about exactly what will go: each address and the query's hits are
        // resolved to items once, and the "yes" forgets those items, by their ids.
        for addr in addrs {
            if addr.starts_with("c:") || addr.contains(":c:") {
                return Err(Error::invalid(format!(
                    "{addr} is a chat: `ptyro chat rm` deletes one"
                )));
            }
            let (item, ..) = read(ctx, &c, addr, Some(0)).await?;
            // An item here by its id; a linked machine's by its address there.
            let id = if item.addr.starts_with("k:") { item.item.id } else { item.addr };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if let Some(q) = &query {
            let req = Request::KbSearch {
                query: q.clone(),
                project: None,
                kinds: Vec::new(),
                since_ms: None,
                before_ms: None,
                limit: Some(ptyro_core::kb::search::MAX_LIMIT),
                here: None,
                machines: false,
                machine: None,
                profile: None,
            };
            let hits = match ask(ctx, &c, req).await? {
                Reply::KbHits { hits, .. } => hits,
                r => return Err(unexpected(r)),
            };
            more = hits.len() >= ptyro_core::kb::search::MAX_LIMIT as usize;
            for h in hits.into_iter().filter(|h| h.kind != HitKind::Chat) {
                if !ids.contains(&h.item) {
                    ids.push(h.item);
                }
            }
            if ids.is_empty() {
                eprintln!("nothing in the knowledge base matches");
                return Ok(0);
            }
        }
        let question =
            Request::KbForget { addrs: ids.clone(), query: None, confirmed: false, session: None };
        match ask(ctx, &c, question).await {
            Ok(_) => return Err(Error::internal("forgotten without asking")),
            Err(e) if e.code == ErrorCode::NeedsConfirmation => {
                crate::cli::ask_first(&e.message, false)?;
            }
            Err(e) => return Err(e),
        }
    }
    let query = query.filter(|_| yes);
    let (items, chunks) =
        match ask(ctx, &c, Request::KbForget { addrs: ids, query, confirmed: true, session: None })
            .await?
        {
            Reply::KbForgotten { items, chunks } => (items, chunks),
            r => return Err(unexpected(r)),
        };
    if json {
        println!("{}", json!({ "items": items, "chunks": chunks }));
    } else {
        eprintln!(
            "forgot {items} item{} ({}): deleted for good, and never kept again",
            if items == 1 { "" } else { "s" },
            pieces(u64::from(chunks))
        );
        if more {
            eprintln!("the search may find more: run it again");
        }
    }
    Ok(0)
}

/// Who wrote an item, as a list says it: `you`, or the agent.
fn by(i: &KbListed) -> String {
    match (i.item.writer, i.item.agent.as_deref()) {
        (Writer::User, _) => "you".into(),
        (_, Some(agent)) => agent.to_owned(),
        (w, None) => w.to_string(),
    }
}

/// `text` in your editor, as a file called `name`: what it holds when the editor is done, or
/// `None` when that is nothing.
fn edited(name: &str, text: &str) -> Result<Option<String>, Error> {
    let dir =
        tempfile::tempdir().map_err(|e| Error::internal(format!("a temporary folder: {e}")))?;
    let path = dir.path().join(name);
    std::fs::write(&path, text).map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let argv = ptyro_core::shell::editor_command(&editor, &path);
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| Error::internal(format!("{editor}: {e}")))?;
    if !status.success() {
        return Err(Error::internal(format!("{editor} exited with {status}")));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    Ok(Some(text).filter(|t| !t.trim().is_empty()))
}

/// What a note that was written is called, on one line.
fn noted(item: &KbListed, path: &str) -> String {
    format!("{}  {}  {path}", item.addr, item.item.title.as_deref().unwrap_or("(no title)"))
}

/// `kb note`: write one. The daemon says whose it is (yours; inside an agent's session, that
/// agent's, as its `remember` tool would write it).
#[allow(clippy::too_many_arguments)]
async fn note(
    ctx: &Ctx,
    text: Vec<String>,
    title: Option<String>,
    project: Option<String>,
    profile: Option<String>,
    tags: Vec<String>,
    edit: bool,
    json: bool,
) -> Result<i32, Error> {
    let mut text = text.join(" ");
    if text == "-" {
        text = std::io::read_to_string(std::io::stdin())
            .map_err(|e| Error::internal(format!("stdin: {e}")))?;
    }
    if edit || text.trim().is_empty() {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(Error::invalid(
                "say what to note: `ptyro kb note \"we chose X because Y\"`, or `-` to read it from stdin (with a terminal, no text opens your editor)",
            ));
        }
        // The title is the note's first heading there: one thing to edit, not two.
        let start = ptyro_core::kb::note::compose(&text, title.as_deref());
        let start = if start.is_empty() { "# ".to_owned() } else { start };
        text = match edited("note.md", &format!("{start}\n"))? {
            Some(t) if t.trim() != "#" => t,
            _ => {
                eprintln!("nothing written: no note");
                return Ok(1);
            }
        };
    }
    let title = title.filter(|_| !edit);
    let c = ctx.connect(true).await?;
    let req = Request::KbNote { text, title, project, profile, tags, replaces: None, addr: None };
    let (item, path) = match ask(ctx, &c, req).await? {
        Reply::KbNoted { item, path } => (item, path),
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", json!({ "item": item, "path": path }));
    } else {
        println!("{}", noted(&item, &path));
    }
    Ok(0)
}

/// `kb show REF --open` of a note: its file in your editor, written back when it changed.
async fn edit_note(ctx: &Ctx, c: &ptyro_client::Client, addr: &str) -> Result<i32, Error> {
    let (item, text) = match ask(ctx, c, Request::KbNoteRead { addr: addr.to_owned() }).await? {
        Reply::KbNoteText { item, text, .. } => (item, text),
        r => return Err(unexpected(r)),
    };
    let name = item.item.uri.rsplit('/').next().unwrap_or("note");
    let Some(new) = edited(&format!("{name}.md"), &text)? else {
        eprintln!(
            "left as it was (an empty note is not written: `ptyro kb forget {}` deletes it)",
            item.addr
        );
        return Ok(1);
    };
    if new == text {
        eprintln!("unchanged");
        return Ok(0);
    }
    let req = Request::KbNote {
        addr: Some(item.item.id.clone()),
        text: new,
        title: None,
        project: None,
        profile: None,
        tags: Vec::new(),
        replaces: None,
    };
    match ask(ctx, c, req).await? {
        Reply::KbNoted { item, path } => println!("{}", noted(&item, &path)),
        r => return Err(unexpected(r)),
    }
    Ok(0)
}

/// `kb notes`: by project, the ones about none first.
async fn notes(
    ctx: &Ctx,
    project: Option<String>,
    who: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let req = Request::KbNotes { project, writer: wrote(who.as_deref()) };
    let mut items = match ask(ctx, &c, req).await? {
        Reply::KbItems { items } => items,
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
        return Ok(0);
    }
    if items.is_empty() {
        eprintln!("no notes yet: `ptyro kb note \"we chose X because Y\"` writes one");
        return Ok(0);
    }
    // (Stable: within a project they stay newest first.)
    items.sort_by(|a, b| a.project_name.cmp(&b.project_name));
    let mut last: Option<Option<&str>> = None;
    for i in &items {
        let project = i.project_name.as_deref();
        if last != Some(project) {
            println!("{}", project.unwrap_or("global"));
            last = Some(project);
        }
        let title = i.item.title.as_deref().unwrap_or(i.item.uri.as_str()).replace('\n', " ");
        println!("  {}  {title} · {} · {}", i.addr, by(i), ago(at(i)));
    }
    Ok(0)
}

/// `kb forget --session`: what was kept of a session goes, and it is private from then on.
async fn forget_session(ctx: &Ctx, session: String, yes: bool, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let req = |confirmed| Request::KbForget {
        addrs: Vec::new(),
        query: None,
        confirmed,
        session: Some(session.clone()),
    };
    if !yes {
        match ask(ctx, &c, req(false)).await {
            Ok(_) => return Err(Error::internal("forgotten without asking")),
            Err(e) if e.code == ErrorCode::NeedsConfirmation => {
                crate::cli::ask_first(&e.message, false)?;
            }
            Err(e) => return Err(e),
        }
    }
    let (items, chunks) = match ask(ctx, &c, req(true)).await? {
        Reply::KbForgotten { items, chunks } => (items, chunks),
        r => return Err(unexpected(r)),
    };
    if json {
        println!("{}", json!({ "items": items, "chunks": chunks, "private": true }));
    } else {
        eprintln!(
            "{session} is private now: forgot {items} item{} ({}), and nothing more of it is kept",
            if items == 1 { "" } else { "s" },
            pieces(u64::from(chunks))
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ptyro_core::kb::search::{KbCount, KbSkipped};

    use super::*;

    #[derive(Parser)]
    struct Recall {
        #[command(flatten)]
        a: RecallArgs,
    }

    fn args(words: &[&str]) -> RecallArgs {
        Recall::try_parse_from(std::iter::once("recall").chain(words.iter().copied())).unwrap().a
    }

    #[test]
    fn recalls_flags_are_taken_wherever_they_are_typed() {
        // Before the words, clap takes them.
        let mut a = args(&["-l", "5", "--kind", "session,chat", "retry", "policy"]);
        assert_eq!(a.take_flags().unwrap(), ["retry", "policy"]);
        assert_eq!((a.limit, a.kind.as_deref()), (5, Some("session,chat")));
        // After the first word they arrive as words, and are taken back; `-word` stays.
        let mut a = args(&[
            "retry",
            "-flaky",
            "--json",
            "-p",
            "ptyro",
            "--since=7d",
            "--before",
            "1d",
            "-l",
            "3",
            "--words",
            "-k",
            "doc",
            "--",
            "--json",
        ]);
        assert_eq!(a.take_flags().unwrap(), ["retry", "-flaky", "--json"]);
        assert!(a.json && a.words && !a.meaning);
        assert_eq!(
            (a.limit, a.project.as_deref(), a.kind.as_deref()),
            (3, Some("ptyro"), Some("doc"))
        );
        assert_eq!((a.since.as_deref(), a.before.as_deref()), (Some("7d"), Some("1d")));
        let mut a = args(&["x", "--meaning", "--words"]);
        assert!(a.take_flags().unwrap_err().message.contains("can't go together"));
        let mut a = args(&["x", "-l"]);
        assert!(a.take_flags().unwrap_err().message.contains("needs a value"));
        assert!(Recall::try_parse_from(["recall"]).is_err(), "it needs something to look for");
    }

    #[test]
    fn kinds_are_their_words_or_an_error_that_names_them() {
        let k = kinds(Some("session, doc"), HitKind::parse, HitKind::ALL, HitKind::as_str).unwrap();
        assert_eq!(k, [HitKind::Session, HitKind::Doc]);
        assert!(kinds(None, HitKind::parse, HitKind::ALL, HitKind::as_str).unwrap().is_empty());
        let e =
            kinds(Some("sesion"), ItemKind::parse, ItemKind::ALL, ItemKind::as_str).unwrap_err();
        assert_eq!(e.message, "no kind is called \"sesion\": session, file, commit, note, shell");
    }

    #[test]
    fn the_status_in_words() {
        let off = KbStatusInfo { path: "/d/kb.db".into(), bytes: 2048, ..KbStatusInfo::default() };
        let text = status_text(&off);
        assert!(text.starts_with("knowledge base: off."), "{text}");
        assert!(
            text.contains("stays in /d/kb.db (")
                && text.contains("ptyro config set kb.enabled true"),
            "{text}"
        );
        let on = KbStatusInfo {
            enabled: true,
            sessions: false,
            retain_days: 30,
            kinds: vec![
                KbCount { kind: ItemKind::Session, items: 2, chunks: 9 },
                KbCount { kind: ItemKind::Note, items: 1, chunks: 1 },
            ],
            vectors: 4,
            forgotten: 1,
            queued: 2,
            skipped: vec![KbSkipped {
                uri: "file:///x/.env".into(),
                why: "never read".into(),
                at_ms: 0,
            }],
            ..off.clone()
        };
        let text = status_text(&on);
        for line in [
            "knowledge base: on · /d/kb.db (",
            "  2 sessions · 9 pieces\n",
            "  1 note · 1 piece\n",
            "  4 of 10 pieces can be found by meaning\n",
            "  agent sessions are not kept (`kb.sessions` is off)\n",
            "  sessions older than 30 days are deleted\n",
            "  1 forgotten: never kept again\n",
            "  2 waiting to be read\n",
            "  not kept (1):\n    file:///x/.env: never read (",
        ] {
            assert!(text.contains(line), "{line:?} in:\n{text}");
        }
        let broken = KbStatusInfo { error: Some("kb.db is damaged: move it aside".into()), ..on };
        assert_eq!(
            status_text(&broken),
            "knowledge base: on, but kb.db is damaged: move it aside\n"
        );
    }
}
