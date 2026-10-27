//! The scripting commands (SPEC §12.2, M5.2): `send`, `broadcast`, `read`, `wait`, `events`
//! and `stop`. Everything a script needs to drive agents without a TUI:
//!
//! ```sh
//! ptyro send fixer "fix the flaky test" --wait-ready --wait   # → "fixer idle"
//! ptyro wait @working --for idle --timeout 30m
//! ptyro read fixer --json | jq .screen
//! ```
//!
//! `idle` and `done` mean "the turn ptyro last typed has ended": a status older than the last
//! prompt ptyro typed (or one still waiting to be typed) doesn't count, so `new --prompt` followed
//! by `wait --for idle` never returns on the idle *before* the prompt.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ptyro_client::Client;
use ptyro_core::session::{ProcessState, SessionInfo};
use ptyro_core::status::AgentStatus;
use ptyro_core::{Error, ErrorCode, SessionId, now_ms};
use ptyro_protocol::{ClientMsg, DomainEvent, Reply, Request, ScreenFormat, ServerMsg};
use serde::Serialize;

use crate::cli::{Ctx, no_daemon, targets, unexpected};

pub const CONDITIONS: [&str; 7] =
    ["working", "blocked", "idle", "done", "errored", "exit", "any-attention"];

pub fn duration(s: &str) -> Result<Duration, Error> {
    ptyro_core::config::parse_duration(s).map_err(|e| Error::invalid(format!("{s:?}: {e}")))
}

fn status_of(s: &SessionInfo) -> Option<AgentStatus> {
    s.status.as_ref().map(|r| r.status)
}

pub use ptyro_core::session::reached;

/// How a session stands, in one word: the agent status, else the process state.
fn word(s: &SessionInfo) -> String {
    match &s.state {
        ProcessState::Exited { .. } | ProcessState::Failed { .. } => "exited".into(),
        ProcessState::Dormant => "dormant".into(),
        _ => status_of(s).map_or_else(|| s.state.label(), |st| st.label().to_owned()),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub session: String,
    pub name: String,
    /// The condition was met.
    pub reached: bool,
    /// `idle`, `blocked`, `stalled`, `exited`, `timeout`, …
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub turns: u32,
    /// The regex match, for `wait --match`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
    pub waited_ms: u64,
}

impl Outcome {
    fn of(s: &SessionInfo, reached: bool, state: String, started: Instant) -> Self {
        Self {
            session: s.id.full(),
            name: s.name.clone(),
            reached,
            state,
            detail: s.status.as_ref().and_then(|r| r.detail.clone()),
            turns: s.turns,
            matched: None,
            waited_ms: started.elapsed().as_millis() as u64,
        }
    }
}

pub struct WaitOpts {
    /// Any of these conditions ends the wait for a session.
    pub conditions: Vec<String>,
    pub pattern: Option<regex::Regex>,
    pub scrollback: bool,
    pub timeout: Option<Duration>,
    pub any: bool,
    pub stall: Option<Duration>,
}

async fn get(c: &Client, id: SessionId) -> Result<SessionInfo, Error> {
    match c.request(Request::Get { addr: id.full() }).await? {
        Reply::Session { info } => Ok(info),
        other => Err(unexpected(other)),
    }
}

async fn screen_text(c: &Client, id: SessionId, scrollback: bool) -> Result<String, Error> {
    if scrollback {
        match c.request(Request::ReadScrollback { addr: id.full(), lines: None }).await? {
            Reply::Scrollback { doc } => {
                Ok(ptyro_core::cache::plain_text(&ptyro_core::cache::doc_rows(&doc)))
            }
            other => Err(unexpected(other)),
        }
    } else {
        match c
            .request(Request::ReadScreen { addr: id.full(), format: ScreenFormat::Plain })
            .await?
        {
            Reply::Screen { text } => Ok(text),
            other => Err(unexpected(other)),
        }
    }
}

/// Wait for `ids` per `o`. Events wake it at once; a 250 ms tick covers screen matches and
/// stalls (neither raises an event).
pub async fn wait_for(
    c: &mut Client,
    ids: &[SessionId],
    o: &WaitOpts,
) -> Result<Vec<Outcome>, Error> {
    let started = Instant::now();
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents)?;
    let mut done: HashMap<SessionId, Outcome> = HashMap::new();
    loop {
        for id in ids {
            if done.contains_key(id) {
                continue;
            }
            let s = match get(c, *id).await {
                Ok(s) => s,
                Err(e) if e.code == ErrorCode::NotFound => {
                    let out = Outcome {
                        session: id.full(),
                        name: id.full(),
                        reached: o.conditions.iter().any(|c| c == "exit"),
                        state: "removed".into(),
                        detail: None,
                        turns: 0,
                        matched: None,
                        waited_ms: started.elapsed().as_millis() as u64,
                    };
                    done.insert(*id, out);
                    continue;
                }
                Err(e) => return Err(e),
            };
            if let Some(re) = &o.pattern {
                let text = screen_text(c, *id, o.scrollback).await?;
                if let Some(m) = re.find(&text) {
                    let mut out = Outcome::of(&s, true, "matched".into(), started);
                    out.matched = Some(m.as_str().to_owned());
                    done.insert(*id, out);
                    continue;
                }
            } else if let Some(cond) = o.conditions.iter().find(|cnd| reached(&s, cnd)) {
                let state = if cond == "exit" { "exited".to_owned() } else { word(&s) };
                done.insert(*id, Outcome::of(&s, true, state, started));
                continue;
            }
            if !s.state.is_live() && s.state != ProcessState::Starting {
                // It can't get there any more.
                done.insert(*id, Outcome::of(&s, false, word(&s), started));
                continue;
            }
            if let Some(stall) = o.stall
                && status_of(&s) == Some(AgentStatus::Working)
                && now_ms().saturating_sub(s.last_output_ms.unwrap_or(s.created_ms))
                    >= stall.as_millis() as u64
            {
                done.insert(*id, Outcome::of(&s, false, "stalled".into(), started));
            }
        }
        let finished = if o.any {
            done.values().any(|d| d.reached) || done.len() == ids.len()
        } else {
            done.len() == ids.len()
        };
        if finished {
            break;
        }
        if let Some(t) = o.timeout
            && started.elapsed() >= t
        {
            for id in ids {
                if !done.contains_key(id) {
                    let s = get(c, *id).await?;
                    done.insert(*id, Outcome::of(&s, false, "timeout".into(), started));
                }
            }
            break;
        }
        tokio::select! {
            m = pushes.recv() => {
                match m {
                    Some(ServerMsg::Goodbye { reason }) if reason.contains("upgrading") => {
                        return Err(Error::new(ErrorCode::DaemonUnavailable, UPGRADING));
                    }
                    None | Some(ServerMsg::Goodbye { .. }) => {
                        return Err(Error::new(ErrorCode::DaemonUnavailable, "lost the daemon"));
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
    Ok(ids.iter().filter_map(|id| done.remove(id)).collect())
}

/// What a client is told while the daemon re-execs itself (M8.1): reconnect and carry on.
pub const UPGRADING: &str = "the daemon is upgrading";

/// [`wait_for`], carried across live upgrades: reconnect and keep waiting, with what is left of
/// the timeout.
pub async fn wait_for_lasting(
    ctx: &Ctx,
    c: &mut Client,
    ids: &[SessionId],
    o: &WaitOpts,
) -> Result<Vec<Outcome>, Error> {
    let started = Instant::now();
    loop {
        let left = o.timeout.map(|t| t.saturating_sub(started.elapsed()));
        let opts = WaitOpts {
            timeout: left,
            pattern: o.pattern.clone(),
            conditions: o.conditions.clone(),
            ..*o
        };
        match wait_for(c, ids, &opts).await {
            Err(e) if e.message == UPGRADING => *c = reconnect(ctx).await?,
            other => return other,
        }
    }
}

/// Connect again after an upgrade (the new image answers within a second or two).
pub async fn reconnect(ctx: &Ctx) -> Result<Client, Error> {
    let start = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        match ctx.connect(false).await {
            Ok(c) => return Ok(c),
            Err(e) if start.elapsed() > Duration::from_secs(30) => return Err(e),
            Err(_) => {}
        }
    }
}

/// Print outcomes and pick the exit code: 0 when the wait succeeded, 7 on a timeout or stall,
/// 5 when a session exited (or vanished) before getting there.
fn report(outcomes: &[Outcome], any: bool, json: bool) -> i32 {
    if json {
        println!("{}", serde_json::to_string_pretty(outcomes).unwrap_or_default());
    } else {
        for o in outcomes {
            let extra = o.matched.as_deref().map(|m| format!(" {m:?}")).unwrap_or_default();
            println!("{} {}{extra}", o.name, o.state);
        }
    }
    let ok =
        if any { outcomes.iter().any(|o| o.reached) } else { outcomes.iter().all(|o| o.reached) };
    if ok {
        0
    } else if outcomes
        .iter()
        .any(|o| !o.reached && matches!(o.state.as_str(), "timeout" | "stalled"))
    {
        ErrorCode::Timeout.exit_code()
    } else {
        ErrorCode::Conflict.exit_code()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn wait(
    ctx: &Ctx,
    sessions: Vec<String>,
    condition: String,
    pattern: Option<String>,
    scrollback: bool,
    timeout: Option<String>,
    any: bool,
    stall: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    let pattern = pattern
        .map(|p| regex::Regex::new(&p).map_err(|e| Error::invalid(format!("--match: {e}"))))
        .transpose()?;
    let o = WaitOpts {
        conditions: vec![condition],
        pattern,
        scrollback,
        timeout: timeout.as_deref().map(duration).transpose()?,
        any,
        stall: stall.as_deref().map(duration).transpose()?,
    };
    let mut c = ctx.connect(false).await.map_err(no_daemon)?;
    let ids: Vec<SessionId> = targets(&c, &sessions).await?.iter().map(|s| s.id).collect();
    let outcomes = wait_for_lasting(ctx, &mut c, &ids, &o).await?;
    Ok(report(&outcomes, any, json))
}

/// `SESSION… TEXT` → (sessions, text).
fn split(mut args: Vec<String>) -> (Vec<String>, String) {
    let text = args.pop().unwrap_or_default();
    (args, text)
}

/// `--raw` text → the bytes it stands for.
fn raw_bytes(text: &str) -> Result<Vec<u8>, Error> {
    ptyro_core::input::unescape(text).map_err(Error::invalid)
}

#[allow(clippy::too_many_arguments)]
pub async fn send(
    ctx: &Ctx,
    args: Vec<String>,
    no_enter: bool,
    raw: bool,
    wait_ready: Option<String>,
    wait: bool,
    timeout: Option<String>,
    stall: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    if args.len() < 2 {
        return Err(Error::invalid("usage: ptyro send SESSION… TEXT"));
    }
    if wait && (raw || no_enter) {
        // Only a prompt submitted with Enter starts a turn that --wait can see end.
        return Err(Error::invalid(
            "--wait waits for the turn a prompt starts; it can't follow --raw or --no-enter",
        ));
    }
    if raw && wait_ready.is_some() {
        return Err(Error::invalid("--raw sends keys at once; it can't --wait-ready"));
    }
    let (sessions, text) = split(args);
    let ready = wait_ready.as_deref().map(duration).transpose()?;
    let keys = raw.then(|| raw_bytes(&text)).transpose()?;
    let mut c = ctx.connect(false).await.map_err(no_daemon)?;
    let infos = targets(&c, &sessions).await?;
    if let Some(bytes) = keys {
        for info in &infos {
            c.request(Request::SendBytes { addr: info.id.full(), bytes: bytes.clone() }).await?;
        }
        return Ok(0);
    }
    let enter = !no_enter;
    let started = now_ms();
    let send_text = |id: SessionId| {
        let left = ready.map(|d| (d.as_millis() as u64).saturating_sub(now_ms() - started));
        Request::SendText { addr: id.full(), text: text.clone(), enter, wait_ready_ms: left }
    };
    let results =
        futures::future::join_all(infos.iter().map(|info| c.request(send_text(info.id)))).await;
    let mut lost = Vec::new();
    for (info, r) in infos.iter().zip(results) {
        match r {
            Err(e) if e.code == ErrorCode::DaemonUnavailable && enter => lost.push(info.id),
            r => {
                r?;
            }
        }
    }
    if !lost.is_empty() {
        // The daemon went away mid-request (a live upgrade, while it waited for the agent to be
        // ready). A prompt typed since we started stamped `last_prompt_ms`; only the rest are
        // sent again, so nothing is typed twice.
        c = reconnect(ctx).await?;
        for id in lost {
            if get(&c, id).await?.last_prompt_ms.is_some_and(|t| t >= started) {
                continue;
            }
            c.request(send_text(id)).await?;
        }
    }
    if !wait {
        return Ok(0);
    }
    // The turn this prompt starts is over when the agent is idle again, needs you, errs or
    // exits; `report` tells which.
    let o = WaitOpts {
        conditions: ["idle", "blocked", "errored", "exit"].map(String::from).into(),
        pattern: None,
        scrollback: false,
        timeout: timeout.as_deref().map(duration).transpose()?,
        any: false,
        stall: stall.as_deref().map(duration).transpose()?,
    };
    let ids: Vec<SessionId> = infos.iter().map(|s| s.id).collect();
    let outcomes = wait_for_lasting(ctx, &mut c, &ids, &o).await?;
    // Ending on "blocked" or "errored" is a fine answer to "how did the turn end?".
    Ok(report(&outcomes, false, json))
}

pub async fn broadcast(
    ctx: &Ctx,
    args: Vec<String>,
    no_enter: bool,
    timeout: String,
) -> Result<i32, Error> {
    if args.len() < 2 {
        return Err(Error::invalid("usage: ptyro broadcast SESSION… TEXT"));
    }
    let (sessions, text) = split(args);
    let t = duration(&timeout)?;
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let infos = targets(&c, &sessions).await?;
    let sends = infos.iter().map(|info| {
        c.request(Request::SendText {
            addr: info.id.full(),
            text: text.clone(),
            enter: !no_enter,
            wait_ready_ms: Some(t.as_millis() as u64),
        })
    });
    let mut code = 0;
    for (info, r) in infos.iter().zip(futures::future::join_all(sends).await) {
        match r {
            Ok(_) => eprintln!("sent to {}", info.name),
            Err(e) => {
                eprintln!("ptyro: {}: {}", info.name, e.message);
                if code == 0 {
                    code = e.code.exit_code();
                }
            }
        }
    }
    Ok(code)
}

pub async fn stop(ctx: &Ctx, sessions: Vec<String>) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    for info in targets(&c, &sessions).await? {
        c.request(Request::Kill { addr: info.id.full(), signal: None }).await?;
        eprintln!("stopped {}", info.name);
    }
    Ok(0)
}

#[derive(Serialize)]
struct ReadOut<'a> {
    session: String,
    name: &'a str,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'a ptyro_core::status::StatusRecord>,
    turns: u32,
    /// `screen` or `scrollback`.
    view: &'static str,
    text: String,
}

pub async fn read(
    ctx: &Ctx,
    session: String,
    scrollback: Option<u32>,
    ansi: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let text = if let Some(lines) = scrollback {
        let Reply::Scrollback { doc } = c
            .request(Request::ReadScrollback { addr: session.clone(), lines: Some(lines) })
            .await?
        else {
            return Err(Error::internal("unexpected reply"));
        };
        let rows = ptyro_core::cache::doc_rows(&doc);
        if ansi {
            ptyro_core::cache::ansi_text(&rows, &ptyro_core::cache::doc_styles(&doc))
        } else {
            ptyro_core::cache::plain_text(&rows)
        }
    } else {
        let format = if ansi { ScreenFormat::Ansi } else { ScreenFormat::Plain };
        match c.request(Request::ReadScreen { addr: session.clone(), format }).await? {
            Reply::Screen { text } => text,
            other => return Err(unexpected(other)),
        }
    };
    if json {
        let info = match c.request(Request::Get { addr: session }).await? {
            Reply::Session { info } => info,
            other => return Err(unexpected(other)),
        };
        let out = ReadOut {
            session: info.id.full(),
            name: &info.name,
            state: word(&info),
            status: info.status.as_ref(),
            turns: info.turns,
            view: if scrollback.is_some() { "scrollback" } else { "screen" },
            text,
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        print!("{text}");
    }
    Ok(0)
}

fn clock(ms: u64) -> String {
    let secs = (ms / 1000) % 86_400;
    // UTC: scripts compare these; --json has the exact `at_ms`.
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

pub async fn events(
    ctx: &Ctx,
    follow: bool,
    session: Option<String>,
    limit: u32,
    json: bool,
) -> Result<i32, Error> {
    let mut c = ctx.connect(false).await.map_err(no_daemon)?;
    // A selector may match several sessions: their events, merged.
    let only: Option<Vec<SessionId>> = match &session {
        Some(a) => Some(targets(&c, std::slice::from_ref(a)).await?.iter().map(|s| s.id).collect()),
        None => None,
    };
    if !follow {
        let mut events = Vec::new();
        let filters: Vec<Option<String>> = match &only {
            Some(ids) => ids.iter().map(|i| Some(i.full())).collect(),
            None => vec![None],
        };
        for session in filters {
            let Reply::Events { events: mut more } =
                c.request(Request::Events { session, limit: Some(limit) }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            events.append(&mut more);
        }
        events.sort_by_key(|e| e.at_ms);
        let events = &events[events.len().saturating_sub(limit as usize)..];
        let names: HashMap<String, String> = match c.request(Request::List).await? {
            Reply::Sessions { sessions } => {
                sessions.into_iter().map(|s| (s.id.full(), s.name)).collect()
            }
            _ => HashMap::new(),
        };
        for e in events {
            if json {
                println!("{}", serde_json::to_string(e).unwrap_or_default());
            } else {
                let who = names.get(&e.session).cloned().unwrap_or_else(|| e.session.clone());
                let detail = match &e.detail {
                    serde_json::Value::Null => String::new(),
                    d => format!(" {d}"),
                };
                println!("{} {:<18} {:<8}{detail}", clock(e.at_ms), who, e.kind);
            }
        }
        return Ok(0);
    }
    // Live: every domain event as one JSON line (`--json` is implied).
    let _ = json;
    let mut pushes = c.pushes();
    c.send(ClientMsg::WatchEvents)?;
    use std::io::Write as _;
    loop {
        let Some(m) = pushes.recv().await else { break };
        match m {
            // A live upgrade (M8.1): follow the new image.
            ServerMsg::Goodbye { reason } if reason.contains("upgrading") => {
                c = reconnect(ctx).await?;
                pushes = c.pushes();
                c.send(ClientMsg::WatchEvents)?;
                continue;
            }
            ServerMsg::Event { event } => {
                if let Some(ids) = &only
                    && event_session(&event).is_some_and(|s| !ids.contains(&s))
                {
                    continue;
                }
                let line = serde_json::json!({ "at_ms": now_ms(), "event": event });
                let mut out = std::io::stdout().lock();
                if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                    return Ok(0); // the reader went away (`| head`)
                }
            }
            ServerMsg::Goodbye { .. } => break,
            _ => {}
        }
    }
    Err(Error::new(ErrorCode::DaemonUnavailable, "the daemon went away"))
}

fn event_session(e: &DomainEvent) -> Option<SessionId> {
    match e {
        DomainEvent::SessionCreated { info }
        | DomainEvent::SessionUpdated { info }
        | DomainEvent::SessionExited { info }
        | DomainEvent::SessionRestarted { info } => Some(info.id),
        DomainEvent::SessionRemoved { id } => Some(*id),
        DomainEvent::ControllerChanged { session, .. }
        | DomainEvent::Bell { session }
        | DomainEvent::SessionResumed { session }
        | DomainEvent::Clipboard { session, .. }
        | DomainEvent::Notification { session, .. }
        | DomainEvent::Notify { session, .. } => Some(*session),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::status::{Confidence, SignalSource, StatusRecord};

    fn info(status: AgentStatus, since: u64, last_prompt: Option<u64>) -> SessionInfo {
        let mut s = SessionInfo {
            machine: None,
            id: SessionId::new(),
            handle: String::new(),
            name: "x".into(),
            kind: ptyro_core::session::SessionKind::Agent,
            argv: vec![],
            cwd: "/".into(),
            state: ProcessState::Running,
            size: ptyro_core::TermSize::new(80, 24),
            pid: None,
            created_ms: 0,
            ended_ms: None,
            viewers: 0,
            controller: None,
            title: None,
            foreground: None,
            live_cwd: None,
            program_status: None,
            progress: None,
            last_command_exit: None,
            profile: "default".into(),
            worktree: None,
            project: None,
            workspace: None,
            agent: None,
            script: None,
            ports: vec![],
            status: None,
            attention: None,
            activity: None,
            conversation: None,
            children: vec![],
            dangerous: false,
            prompt_pending: false,
            last_prompt_ms: None,
            turns: 0,
            last_output_ms: None,
            verify: None,
            run: None,
            held: false,
            structured: None,
            task: None,
            last_reply: None,
            command: None,
            inner_agent: None,
            summary: None,
            window_title: None,
            quick: false,
            pad: false,
        };
        let mut r = StatusRecord::new(status, Confidence::High, SignalSource::Hook);
        r.since_ms = since;
        s.status = Some(r);
        s.last_prompt_ms = last_prompt;
        s
    }

    #[test]
    fn idle_means_the_turn_after_the_last_prompt() {
        assert!(reached(&info(AgentStatus::Idle, 10, None), "idle"));
        // The idle before the prompt doesn't count…
        assert!(!reached(&info(AgentStatus::Idle, 10, Some(20)), "idle"));
        // …the one after it does.
        assert!(reached(&info(AgentStatus::Idle, 30, Some(20)), "idle"));
        let mut pending = info(AgentStatus::Idle, 30, None);
        pending.prompt_pending = true;
        assert!(!reached(&pending, "idle"), "a prompt still waits to be typed");
        // Blocked counts whenever it is.
        assert!(reached(&info(AgentStatus::Blocked, 10, Some(20)), "blocked"));
        assert!(reached(&info(AgentStatus::Blocked, 10, Some(20)), "any-attention"));
        assert!(!reached(&info(AgentStatus::Working, 30, None), "idle"));
        assert!(reached(&info(AgentStatus::Done, 30, None), "idle"), "done ends a turn too");
    }
}
