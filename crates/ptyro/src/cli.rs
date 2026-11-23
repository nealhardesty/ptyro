//! CLI command implementations (SPEC §12). Errors print as `ptyro: …` and exit with the codes of
//! SPEC §12.4.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

use ptyro_client::{Client, ConnectOptions};
use ptyro_core::paths::Paths;
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_core::{Error, ErrorCode, TermSize};
use ptyro_protocol::{ClientKind, Encoding, Reply, Request};
use ptyro_tui::AttachEnd;

use crate::{Cmd, DaemonCmd};

pub fn parse_size(s: &str) -> Result<TermSize, String> {
    let (c, r) = s.split_once('x').ok_or("expected COLSxROWS, e.g. 120x40")?;
    let cols: u16 = c.parse().map_err(|_| format!("bad columns {c:?}"))?;
    let rows: u16 = r.parse().map_err(|_| format!("bad rows {r:?}"))?;
    if cols < 2 || rows < 1 {
        return Err("size too small".into());
    }
    Ok(TermSize::new(cols, rows))
}

pub fn daemon_start(foreground: bool, spawned: bool) -> i32 {
    // Typed at a prompt: start it in the background, as any client would, and say so. Only
    // `--foreground` (service managers, debugging) keeps it in this terminal.
    if !foreground && !spawned {
        return start_detached();
    }
    // Leave the client's session and process group so its terminal closing can't hang us up.
    // Done before any runtime exists: we never fork an initialized tokio process.
    if spawned {
        let _ = rustix::process::setsid();
    }
    match ptyro_daemon::run(ptyro_daemon::Options { foreground }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ptyro: {e:#}");
            if e.to_string().contains("already running") {
                ErrorCode::Conflict.exit_code()
            } else {
                1
            }
        }
    }
}

/// `ptyro daemon start`: the daemon detached (`--spawned`, as a client starts it), this command
/// returning once it accepts connections.
fn start_detached() -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("ptyro: {e}");
            return 1;
        }
    };
    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ptyro: {e}");
            return 1;
        }
    };
    let ctx = Ctx::local(paths);
    rt.block_on(async {
        let status = |client: Client| async move {
            match client.request(Request::Status).await {
                Ok(Reply::Status { status }) => Ok(status.pid),
                Ok(_) => Err(Error::internal("unexpected reply")),
                Err(e) => Err(e),
            }
        };
        if let Ok(client) = ctx.connect(false).await {
            match status(client).await {
                Ok(pid) => eprintln!("ptyro: daemon already running (pid {pid})"),
                Err(_) => eprintln!("ptyro: daemon already running"),
            }
            return ErrorCode::Conflict.exit_code();
        }
        // Asked for by name: started even under `PTYRO_NO_AUTOSPAWN`.
        let mut opts = ctx.options(ClientKind::Cli, true);
        opts.autospawn = true;
        let started = match Client::connect(&opts).await {
            Ok(client) => status(client).await,
            Err(e) => Err(e.into()),
        };
        match started {
            Ok(pid) => {
                eprintln!("daemon started (pid {pid}); logs in {}", ctx.paths.log_dir().display());
                0
            }
            Err(e) => {
                eprintln!("ptyro: {}", said(&e));
                e.code.exit_code()
            }
        }
    })
}

pub async fn run(
    cmd: Option<Cmd>,
    task: Vec<String>,
    json_wire: bool,
    remote: Option<String>,
) -> i32 {
    match dispatch(cmd, task, json_wire, remote).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ptyro: {}", said(&e));
            e.code.exit_code()
        }
    }
}

pub struct Ctx {
    pub paths: Paths,
    encoding: Encoding,
    /// `--remote`: the daemon is on another machine (over ssh).
    pub remote: Option<String>,
    /// This computer's own daemon was asked for (`--remote local`, `client.default_remote =
    /// local`).
    pub local: bool,
}

impl Ctx {
    /// For this machine's own daemon (what `ptyro serve` does host operations with).
    pub fn local(paths: Paths) -> Self {
        Self { paths, encoding: Encoding::MsgPack, remote: None, local: true }
    }

    fn options(&self, kind: ClientKind, autospawn: bool) -> ConnectOptions {
        let mut o = ConnectOptions::new(self.paths.clone(), kind);
        // `PTYRO_NO_AUTOSPAWN=1` (tests, scripts) wins over every command that would start one
        // (`doctor`, `new`…), so nothing leaves a daemon running its caller didn't ask for.
        o.autospawn = autospawn && o.autospawn;
        o.encoding = self.encoding;
        if self.remote.is_some() {
            o.remote.clone_from(&self.remote);
        } else if self.local {
            o.remote = Some(ptyro_client::LOCAL.into());
        }
        o
    }

    pub async fn connect(&self, autospawn: bool) -> Result<Client, Error> {
        Ok(Client::connect(&self.options(ClientKind::Cli, autospawn)).await?)
    }
}

async fn dispatch(
    cmd: Option<Cmd>,
    task: Vec<String>,
    json_wire: bool,
    remote: Option<String>,
) -> Result<i32, Error> {
    let paths = Paths::from_env().map_err(|e| Error::internal(e.to_string()))?;
    let encoding = if json_wire { Encoding::Json } else { Encoding::MsgPack };
    // Uninstall is about this computer: a saved or given remote never sends it to another
    // machine's daemon, which it would stop with every session.
    let remote = if matches!(cmd, Some(Cmd::Uninstall { .. })) {
        if let Some(r) = remote {
            return Err(Error::invalid(format!(
                "uninstall removes ptyro from this computer, not from {r}: run `ptyro uninstall` there"
            )));
        }
        None
    } else if matches!(
        cmd,
        Some(Cmd::Setup { .. } | Cmd::Install { .. } | Cmd::Login { .. } | Cmd::Update { .. })
    ) {
        // Setup installs on this computer, with no daemon (BOOT1); logins too (BOOT4), and
        // updates (BOOT5).
        if let Some(r) = remote.filter(|r| !ptyro_client::is_local(Some(r))) {
            if matches!(cmd, Some(Cmd::Update { .. })) {
                return Err(Error::invalid(format!(
                    "update is about this computer's agents, not {r}'s: run `ptyro update` on {r}"
                )));
            }
            let what = if matches!(cmd, Some(Cmd::Login { .. })) { "login" } else { "setup" };
            return Err(Error::invalid(format!(
                "{what} is about this computer, not {r}: run `ptyro {what}` there"
            )));
        }
        None
    } else {
        resolve_remote(remote, &paths)
    };
    // `local` is this computer's own daemon, not a remote one.
    let local = ptyro_client::is_local(remote.as_deref());
    let remote = remote.filter(|_| !local);
    let ctx = Ctx { paths, encoding, remote, local };
    match cmd {
        None if !task.is_empty() => start_task(&ctx, &task.join(" ")).await,
        None => default_action(&ctx).await,
        Some(Cmd::New {
            detach,
            name,
            cwd,
            size,
            json,
            agent,
            profile,
            again,
            wt,
            new_wt,
            project,
            override_pin,
            yes,
            prompt,
            prompt_file,
            title,
            pr,
            issue,
            private,
            no_private,
            argv,
        }) => {
            let prompt = match prompt_file {
                Some(f) => Some(
                    std::fs::read_to_string(&f)
                        .map_err(|e| Error::invalid(format!("{}: {e}", f.display())))?,
                ),
                None => prompt,
            };
            let a = NewArgs {
                detach,
                name,
                cwd,
                size,
                json,
                argv,
                agent,
                profile,
                again,
                wt,
                override_pin,
                yes,
                prompt,
                title,
                pr,
                issue,
                private: private_flag(private, no_private),
                welcome: false,
            };
            let new_wt = new_wt.or_else(|| (a.pr.is_some() || a.issue.is_some()).then(String::new));
            match new_wt {
                Some(wt_name) => {
                    crate::projects::new_in_new_worktree(&ctx, a, wt_name, project).await
                }
                // `-p` once meant `--prompt`: never drop it unheard.
                None if project.is_some() => Err(Error::invalid(
                    "--project picks the project a new worktree goes in: add --new-wt, or give the agent a prompt with --prompt",
                )),
                None => new(&ctx, a).await,
            }
        }
        Some(Cmd::Attach { session, workspace, raw, view_only, restarted, tab, pane }) => {
            let client = ctx.connect(true).await?;
            if tab || pane {
                let info = match &session {
                    Some(addr) => get(&client, addr).await?,
                    None => most_recent(&client).await?.ok_or_else(|| {
                        Error::new(
                            ErrorCode::NotFound,
                            "no sessions yet; start one with `ptyro new`",
                        )
                    })?,
                };
                let place =
                    if pane { ptyro_tui::tab::Place::Pane } else { ptyro_tui::tab::Place::Tab };
                let said = ptyro_tui::tab::open(place, &info.id.full(), ctx.remote.as_deref())
                    .map_err(|e| Error::new(ErrorCode::Unsupported, e))?;
                eprintln!("{} {said}", info.name);
                return Ok(0);
            }
            let info = match (session, &workspace) {
                (Some(addr), _) => Some(get(&client, &addr).await?),
                // The workspace's own view decides what to show.
                (None, Some(_)) => None,
                (None, None) => Some(most_recent(&client).await?.ok_or_else(|| {
                    Error::new(ErrorCode::NotFound, "no sessions yet; start one with `ptyro new`")
                })?),
            };
            let restarted = restarted
                .and_then(|r| r.split_once(':').map(|(a, b)| (a.to_owned(), b.to_owned())));
            let tui =
                ptyro_tui::Options { raw, view_only, workspace, restarted, ..Default::default() };
            attach_with(&ctx, client, info, tui).await
        }
        Some(Cmd::List { json, all, status, project, workspace, chats }) => {
            if all {
                history(&ctx, json).await
            } else {
                list(&ctx, json, Filter { status, project, workspace, chats }).await
            }
        }
        Some(Cmd::Stop { sessions }) => crate::script::stop(&ctx, sessions).await,
        Some(Cmd::Send { args, no_enter, raw, wait_ready, wait, timeout, stall, json }) => {
            crate::script::send(&ctx, args, no_enter, raw, wait_ready, wait, timeout, stall, json)
                .await
        }
        Some(Cmd::Broadcast { args, no_enter, timeout }) => {
            crate::script::broadcast(&ctx, args, no_enter, timeout).await
        }
        Some(Cmd::Wait {
            sessions,
            condition,
            pattern,
            scrollback,
            timeout,
            any,
            all: _,
            stall,
            json,
        }) => {
            crate::script::wait(
                &ctx, sessions, condition, pattern, scrollback, timeout, any, stall, json,
            )
            .await
        }
        Some(Cmd::Events { follow, session, limit, json }) => {
            crate::script::events(&ctx, follow, session, limit, json).await
        }
        Some(Cmd::Completions { shell }) => {
            print!("{}", crate::complete::registration(&shell).map_err(Error::invalid)?);
            Ok(0)
        }
        Some(Cmd::Setup { yes, agents, tools, dry_run, json, place, move_ }) => {
            let a = crate::setup::SetupArgs { yes, agents, tools, dry_run, json, place, move_ };
            crate::setup::setup(&ctx, a).await
        }
        Some(Cmd::Install { names, yes, dry_run, json, place, confirmed, move_ }) => {
            let a =
                crate::setup::InstallArgs { names, yes, dry_run, json, place, confirmed, move_ };
            crate::setup::install(&ctx, a).await
        }
        Some(Cmd::Update { names, check, yes, json, here }) => {
            let a = crate::setup::UpdateArgs { names, check, yes, json, here };
            crate::setup::update(&ctx, a).await
        }
        Some(Cmd::Login { names, check, json, place }) => {
            let a = crate::logins::LoginArgs { names, check, json, place };
            crate::logins::login(&ctx, a).await
        }
        Some(Cmd::About { json }) => crate::about::run(&ctx, json).await,
        Some(Cmd::Doctor { quick, json, fix }) => {
            crate::doctor::run_cmd(&ctx, quick, json, fix).await
        }
        // Kill is immediate and keeps the session listed, as in the TUI and the web (SPEC §7.3).
        Some(Cmd::Kill { sessions, signal, keep: _ }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            let mut code = 0;
            for info in targets(&client, &sessions).await? {
                if !info.state.is_live() {
                    eprintln!("{} isn't running", info.name);
                    continue;
                }
                let signal = signal.unwrap_or(9);
                let req = Request::Kill { addr: info.id.full(), signal: Some(signal) };
                match client.request(req).await {
                    Err(e) => failed(&mut code, &info.name, &e),
                    Ok(_) if signal == 9 => eprintln!("killed {}", info.name),
                    Ok(_) => eprintln!("sent signal {signal} to {}", info.name),
                }
            }
            Ok(code)
        }
        Some(Cmd::Restart { sessions }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            for info in targets(&client, &sessions).await? {
                let info =
                    session_of(client.request(Request::Restart { addr: info.id.full() }).await?)?;
                eprintln!("restarted {}", info.name);
            }
            Ok(0)
        }
        Some(Cmd::Rm { sessions, stop, yes }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            let found = targets(&client, &sessions).await?;
            // A dormant agent could still be resumed: forgetting it asks first, once for all
            // of them (owner, 2026-11-09).
            let dormant: Vec<&str> = found
                .iter()
                .filter(|s| s.state == ptyro_core::session::ProcessState::Dormant)
                .map(|s| s.name.as_str())
                .collect();
            if !dormant.is_empty() {
                let s = if dormant.len() > 1 { "s" } else { "" };
                let q = format!(
                    "Remove the dormant session{s} {}? {}",
                    dormant.join(", "),
                    ptyro_core::removal::DORMANT
                );
                ask_first(&q, yes)?;
            }
            // Each is tried: one that is refused (still running) is said, and the rest go on.
            let mut code = 0;
            for info in found {
                match client.request(Request::Remove { addr: info.id.full(), stop }).await {
                    Err(e) => failed(&mut code, &info.name, &e),
                    Ok(_) if stop && info.state.is_live() => {
                        eprintln!("stopped and removed {}", info.name);
                    }
                    Ok(_) => {}
                }
            }
            Ok(code)
        }
        Some(Cmd::Rename { session, name, title }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            let mut addr = session;
            if let Some(name) = name {
                let info = session_of(client.request(Request::Rename { addr, name }).await?)?;
                eprintln!("renamed to {}", info.name);
                addr = info.id.full();
            }
            if let Some(title) = title {
                let req = Request::Retitle { addr, title: Some(title), worktree: false };
                let info = session_of(client.request(req).await?)?;
                match info.title {
                    Some(t) => eprintln!("{}: {t}", info.name),
                    None => eprintln!("{}: title cleared", info.name),
                }
            }
            Ok(0)
        }
        Some(Cmd::Read { session, screen: _, scrollback, ansi, plain: _, json }) => {
            crate::script::read(&ctx, session, scrollback, ansi, json).await
        }
        Some(Cmd::Config { cmd }) => crate::config::run(&ctx, cmd).await,
        Some(Cmd::Uninstall {
            dry_run,
            yes,
            delete_work,
            keep_config,
            keep_data,
            keep_binary,
            delete_notes,
            purge_refs,
            json,
        }) => {
            let opts = crate::uninstall::Opts {
                dry_run,
                yes,
                delete_work,
                keep_config,
                keep_data,
                keep_binary,
                purge_refs,
                delete_notes,
            };
            crate::uninstall::run(&ctx, opts, json).await
        }
        Some(Cmd::Project { cmd }) => crate::projects::project(&ctx, cmd).await,
        Some(Cmd::Checkpoint { cmd }) => crate::checkpoint::checkpoint(&ctx, cmd).await,
        Some(Cmd::Mcp { chat }) => crate::mcp::serve(&ctx, chat).await,
        Some(Cmd::Proxy) => crate::remote::proxy(&ctx).await,
        Some(Cmd::Chat { cmd, new }) => crate::chat::run(&ctx, cmd, new).await,
        Some(Cmd::Recall { args }) => crate::kb::recall(&ctx, args).await,
        Some(Cmd::Ingest { args }) => crate::ingest::run(&ctx, args).await,
        Some(Cmd::Kb { cmd, json }) => crate::kb::run(&ctx, cmd, json).await,
        Some(Cmd::Shells { cmd, ls }) => crate::shells::run(&ctx, cmd, ls).await,
        Some(Cmd::Shell { worktree, json, read, scrollback }) => {
            quick_shell(&ctx, worktree, json, read, scrollback).await
        }
        Some(Cmd::Serve { access, port, cert, key, loopback_http, managed }) => {
            let own = cert.zip(key);
            // (`PTYRO_LOOPBACK_HTTP` is how the daemon's own server is made plain, for the tests.)
            let plain = loopback_http || std::env::var_os("PTYRO_LOOPBACK_HTTP").is_some();
            crate::serve::run(&ctx, access, port, own, plain, managed).await
        }
        Some(Cmd::Web { action, access, pair, json }) => {
            crate::webapp::run(&ctx, &action, access, json, pair).await
        }
        Some(Cmd::Pair { name, name_arg, scope, url, json, open }) => {
            // "phone" is the default name; a browser on this computer is a "browser".
            let name = name
                .or(name_arg)
                .unwrap_or_else(|| if open { "browser" } else { "phone" }.to_owned());
            crate::remote::pair(&ctx, name, scope.as_deref(), url, json, open).await
        }
        Some(Cmd::Devices { cmd }) => crate::remote::devices(&ctx, cmd).await,
        Some(Cmd::Machine { cmd }) => machine(&ctx, cmd).await,
        Some(Cmd::MachineBridge { url, fingerprint }) => {
            crate::machines::bridge(url, fingerprint).await
        }
        Some(Cmd::Ws { cmd }) => crate::projects::ws(&ctx, cmd).await,
        Some(Cmd::Wt { cmd }) => crate::projects::wt(&ctx, cmd).await,
        Some(Cmd::Profile { cmd }) => crate::profiles::run(&ctx, cmd).await,
        Some(Cmd::Gc { dry_run, json, scratch, keep_wt }) => {
            crate::projects::gc(&ctx, dry_run, json, scratch, keep_wt).await
        }
        Some(Cmd::Status { session, explain, json }) => {
            crate::agents::status(&ctx, session, explain, json).await
        }
        Some(Cmd::Resume { sessions }) => {
            crate::agents::each(&ctx, &sessions, "resumed", |addr| Request::Resume { addr }).await
        }
        Some(Cmd::Hibernate { sessions }) => {
            crate::agents::each(&ctx, &sessions, "hibernated", |addr| Request::Hibernate { addr })
                .await
        }
        Some(Cmd::Answer { session, decision }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            c.request(Request::Answer { addr: session, decision, expect: None }).await?;
            Ok(0)
        }
        Some(Cmd::Mute { target, duration, off }) => {
            crate::agents::mute(&ctx, target, duration, off).await
        }
        Some(Cmd::Usage { json }) => crate::agents::usage(&ctx, json).await,
        Some(Cmd::Home { project, json, put_away, restore }) => {
            crate::home::run(&ctx, crate::home::Args { project, json, put_away, restore }).await
        }
        Some(Cmd::Notify { cmd }) => crate::agents::notify(&ctx, cmd).await,
        Some(Cmd::Hook { .. } | Cmd::StatuslineRelay) => {
            unreachable!("handled in main")
        }
        Some(Cmd::ShellInit { shell }) => {
            print!("{}", shell_init(shell, &ctx.paths));
            Ok(0)
        }
        Some(Cmd::Daemon { cmd }) => daemon(&ctx, cmd).await,
        Some(Cmd::FakeAgent { .. }) => unreachable!("handled in main"),
        #[cfg(debug_assertions)]
        Some(Cmd::FakeGh { .. } | Cmd::FakeInstaller { .. }) => {
            unreachable!("handled in main")
        }
    }
}

/// `ptyro daemon upgrade`: hand off, then wait for the new image to answer.
pub(crate) async fn upgrade(
    ctx: &Ctx,
    binary: Option<std::path::PathBuf>,
    json: bool,
) -> Result<i32, Error> {
    let client = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::Status { status: before } = client.request(Request::Status).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    let Reply::Upgrading { from, to, binary, sessions } =
        client.request(Request::Upgrade { binary }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if !json {
        eprintln!(
            "upgrading the daemon {from} → {to} ({}); {sessions} session(s) keep running…",
            binary.display()
        );
    }
    drop(client);
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if started.elapsed() > Duration::from_secs(30) {
            return Err(Error::new(
                ErrorCode::Timeout,
                "the new daemon didn't answer within 30 s: see `ptyro daemon logs`",
            ));
        }
        let Ok(c) = ctx.connect(false).await else { continue };
        let Ok(Reply::Status { status }) = c.request(Request::Status).await else { continue };
        if status.generation <= before.generation {
            continue;
        }
        let ms = started.elapsed().as_millis();
        if json {
            println!(
                "{}",
                serde_json::json!({ "from": from, "to": status.version, "pid": status.pid, "sessions": status.sessions, "rolled_back": status.rolled_back, "ms": ms })
            );
        } else if status.rolled_back {
            eprintln!(
                "the upgrade was called off, so the daemon {} carries on with every session; see `ptyro daemon logs`, or `ptyro daemon restart --hibernate` to restart it instead (agents resume their conversations; shells and commands start again)",
                status.version
            );
        } else {
            eprintln!(
                "upgraded in {ms} ms: daemon {} (pid {}, unchanged), {} session(s)",
                status.version, status.pid, status.sessions
            );
        }
        return Ok(if status.rolled_back { 1 } else { 0 });
    }
}

/// What the CLI prints for an error: the daemon's words, then a refusal's way through the
/// CLI's way, the flag that says it (SPEC §6.6).
pub fn said(e: &Error) -> String {
    use ptyro_core::error::Again;
    let Some(f) = &e.fix else { return e.message.clone() };
    let flag = match &f.again {
        Again::Force => "--force".to_owned(),
        Again::StopSessions => "--stop-sessions".to_owned(),
        Again::Stop => "--stop".to_owned(),
        Again::Release => "--release".to_owned(),
        Again::Moved => "--moved".to_owned(),
    };
    format!("{} (again with `{flag}`: {})", e.message, f.label.to_lowercase())
}

/// The refusal's way through is `again` (SPEC §6.6).
pub fn fixed_by(e: &Error, again: &ptyro_core::error::Again) -> bool {
    e.fix.as_ref().is_some_and(|f| f.again == *again)
}

pub fn no_daemon(e: Error) -> Error {
    if e.code == ErrorCode::DaemonUnavailable {
        Error::new(
            ErrorCode::DaemonUnavailable,
            "the daemon is not running (so there are no sessions)",
        )
    } else {
        e
    }
}

pub fn unexpected(r: Reply) -> Error {
    Error::internal(format!("unexpected reply: {r:?}"))
}

pub fn session_of(r: Reply) -> Result<SessionInfo, Error> {
    match r {
        Reply::Session { info } => Ok(info),
        other => Err(unexpected(other)),
    }
}

async fn get(client: &Client, addr: &str) -> Result<SessionInfo, Error> {
    session_of(client.request(Request::Get { addr: addr.into() }).await?)
}

async fn sessions(client: &Client) -> Result<Vec<SessionInfo>, Error> {
    match client.request(Request::List).await? {
        Reply::Sessions { sessions } => Ok(sessions),
        other => Err(unexpected(other)),
    }
}

/// The newest live session, else the newest one at all.
async fn most_recent(client: &Client) -> Result<Option<SessionInfo>, Error> {
    let all = sessions(client).await?;
    let live = all.iter().rev().find(|s| s.state.is_live()).cloned();
    Ok(live.or_else(|| all.last().cloned()))
}

/// `ptyro "fix the flaky test"` (UX1.9): the default agent with that as its first prompt, in a
/// new worktree named after it when you're in a git repository (registered on the way), else
/// right here; then attach. One word alone is more likely a mistyped command than a task.
async fn start_task(ctx: &Ctx, task: &str) -> Result<i32, Error> {
    if !task.trim().contains(char::is_whitespace) {
        return Err(Error::invalid(format!(
            "unknown command {task:?} (`ptyro --help` lists them; to start a task, give it in words: ptyro \"fix the flaky test\")"
        )));
    }
    let cwd = current_dir()?;
    let config = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&ctx.paths))
        .unwrap_or_default();
    let a = NewArgs {
        agent: Some(ptyro_agents::default_agent(&config)),
        prompt: Some(task.trim().to_owned()),
        cwd: Some(cwd.clone()),
        ..NewArgs::default()
    };
    let in_repo = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["rev-parse", "--git-dir"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if in_repo {
        crate::projects::new_in_new_worktree(ctx, a, String::new(), None).await
    } else {
        new(ctx, a).await
    }
}

async fn default_action(ctx: &Ctx) -> Result<i32, Error> {
    let client = ctx.connect(true).await?;
    match most_recent(&client).await? {
        // The TUI restores the last workspace and its view (newest live session if none).
        Some(_) => {
            attach_with(ctx, client, None, ptyro_tui::Options { home: true, ..Default::default() })
                .await
        }
        None => {
            drop(client);
            new(ctx, NewArgs { welcome: true, ..NewArgs::default() }).await
        }
    }
}

#[derive(Default)]
pub struct NewArgs {
    pub detach: bool,
    pub name: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
    pub size: Option<TermSize>,
    pub json: bool,
    pub argv: Vec<String>,
    pub agent: Option<String>,
    pub profile: Option<String>,
    /// Quick new (UX3.2, UX3.7): another of the agent the worktree last started.
    pub again: bool,
    pub wt: Option<String>,
    pub override_pin: bool,
    pub yes: bool,
    pub prompt: Option<String>,
    /// Its title (UX1.24).
    pub title: Option<String>,
    /// `ptyro` with nothing running: the TUI opens on the first run or home (UX1.19).
    pub welcome: bool,
    pub pr: Option<u32>,
    pub issue: Option<String>,
    /// `--private` / `--no-private` (SPEC §25.7a); `None`: as `kb.private` says.
    pub private: Option<bool>,
}

/// `--private` and `--no-private` as the daemon takes them: neither is "as `[kb] private` says".
pub fn private_flag(private: bool, no_private: bool) -> Option<bool> {
    (private || no_private).then_some(private)
}

/// The variables a session should see from this client (SSH agent, display).
pub fn client_env() -> BTreeMap<String, String> {
    ptyro_core::os::CLIENT_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect()
}

/// The daemon to use: `--remote` (`given`), else `PTYRO_REMOTE`, else `[client] default_remote`.
pub(crate) fn resolve_remote(given: Option<String>, paths: &Paths) -> Option<String> {
    if given.is_some() {
        return given;
    }
    if let Ok(r) = std::env::var("PTYRO_REMOTE")
        && !r.trim().is_empty()
    {
        return Some(r);
    }
    let config = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(paths))
        .unwrap_or_default();
    config.client.default_remote.filter(|r| !r.trim().is_empty())
}

/// The current directory, as the daemon sees it: here, canonical.
pub fn current_dir() -> Result<std::path::PathBuf, Error> {
    let cwd =
        std::env::current_dir().map_err(|e| Error::invalid(format!("current directory: {e}")))?;
    daemon_path(&cwd)
}

/// An address the daemon reads (SPEC §2.3), with a relative path (`.`, `../app`) made this
/// client's: the daemon would read it against its own directory. Names, ids, `~` and absolute
/// paths go as they are. Every `--project` and worktree argument goes through it.
pub fn addr_here(addr: String) -> Result<String, Error> {
    if !addr.starts_with('.') {
        return Ok(addr);
    }
    let cwd =
        std::env::current_dir().map_err(|e| Error::invalid(format!("current directory: {e}")))?;
    Ok(daemon_path(&cwd.join(&addr))?.to_string_lossy().into_owned())
}

/// `addr_here` for an optional address.
pub fn addr_here_opt(addr: Option<String>) -> Result<Option<String>, Error> {
    addr.map(addr_here).transpose()
}

/// A folder on this computer as the daemon names it: canonical.
fn daemon_path(p: &std::path::Path) -> Result<std::path::PathBuf, Error> {
    p.canonicalize().map_err(|e| Error::invalid(format!("{}: {e}", p.display())))
}

/// Ask a yes/no question on the terminal; `false` without one.
pub fn confirm(question: &str) -> bool {
    use std::io::{BufRead, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// A destructive command's question (SPEC §12.1, owner 2026-11-09): asked on a terminal unless
/// `yes`; with no terminal to ask, refused with an error that offers `--yes`.
pub fn ask_first(question: &str, yes: bool) -> Result<(), Error> {
    if yes || confirm(question) {
        return Ok(());
    }
    Err(not_agreed(question, "pass --yes"))
}

/// One of several sessions refused (`rm @all`, `stop`, `kill`): said, and the first failure's
/// exit code kept, while the rest go on.
pub(crate) fn failed(code: &mut i32, name: &str, e: &Error) {
    // Its way through in the CLI's words (`said`); named once, by the message when it does.
    match said(e) {
        s if e.message.contains(name) => eprintln!("ptyro: {s}"),
        s => eprintln!("ptyro: {name}: {s}"),
    }
    if *code == 0 {
        *code = e.code.exit_code();
    }
}

/// Why a question wasn't agreed to: a "no" on the terminal, or no terminal to ask (`flags`
/// says what to pass instead).
fn not_agreed(question: &str, flags: &str) -> Error {
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Error::new(ErrorCode::NeedsConfirmation, "cancelled: nothing was changed")
    } else {
        Error::new(
            ErrorCode::NeedsConfirmation,
            format!("{question} (no terminal to ask: {flags})"),
        )
    }
}

/// Send a request that may need the user's confirmation (SPEC §10.10 guards): ask on a TTY and
/// resend with `agree` applied; without a TTY, explain the flags.
pub async fn with_confirmation<F>(
    client: &Client,
    req: Request,
    yes: bool,
    agree: F,
) -> Result<Reply, Error>
where
    F: Fn(&mut Request),
{
    with_confirmation_as(client, req, yes, agree, "pass --yes, or --override-pin").await
}

/// `with_confirmation`, where no terminal can ask: `flags` says what to pass instead.
pub async fn with_confirmation_as<F>(
    client: &Client,
    mut req: Request,
    yes: bool,
    agree: F,
    flags: &str,
) -> Result<Reply, Error>
where
    F: Fn(&mut Request),
{
    if yes {
        agree(&mut req);
    }
    match client.request(req.clone()).await {
        Err(e) if e.code == ErrorCode::NeedsConfirmation => {
            if confirm(&e.message) {
                agree(&mut req);
                client.request(req).await
            } else {
                Err(not_agreed(&e.message, flags))
            }
        }
        other => other,
    }
}

pub async fn new(ctx: &Ctx, a: NewArgs) -> Result<i32, Error> {
    // `ptyro` alone opens the TUI, so it says why it can't instead of starting a shell.
    let attaching = a.welcome || attaches(a.detach || a.json);
    if attaching {
        check_attachable()?;
    }
    let cwd = match a.cwd {
        Some(p) => ptyro_core::os::canonicalize(&p)
            .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?,
        None => current_dir()?,
    };
    let size = if attaching { ptyro_tui::pane_size(&ctx.paths) } else { a.size };
    let client = ctx.connect(true).await?;
    let spec = SpawnSpec {
        argv: a.argv,
        agent: a.agent,
        cwd,
        worktree: addr_here_opt(a.wt)?,
        profile: a.profile,
        override_pin: a.override_pin,
        confirmed: a.yes,
        name: a.name,
        env: client_env(),
        size,
        prompt: a.prompt,
        title: a.title,
        again: a.again,
        private: a.private,
        ..SpawnSpec::default()
    };
    let reply = with_confirmation(&client, Request::Spawn { spec }, false, |r| {
        if let Request::Spawn { spec } = r {
            spec.override_pin = true;
            spec.confirmed = true;
        }
    })
    .await?;
    let info = session_of(reply)?;
    if attaching {
        // (A bare `ptyro` that had to start the first session: home, as any bare `ptyro`.)
        let tui = ptyro_tui::Options { welcome: a.welcome, home: a.welcome, ..Default::default() };
        return attach_with(ctx, client, Some(info), tui).await;
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
    } else {
        println!("{}", info.handle);
        eprintln!(
            "started {} ({}, profile {}) — attach with `ptyro attach {}`",
            info.name, info.handle, info.profile, info.name
        );
    }
    Ok(0)
}

/// Whether a command that starts an interactive session attaches to it (owner, 2026-11-09):
/// yes in a terminal of its own, unless `-d`. Without a terminal (a script), or inside a ptyro
/// session (whose TUI shows the new one), it starts detached and says so.
pub fn attaches(detach: bool) -> bool {
    !detach
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && (std::env::var_os("PTYRO_SESSION_ID").is_none()
            || std::env::var_os("PTYRO_ALLOW_NESTED").is_some())
}

pub fn check_attachable() -> Result<(), Error> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Error::invalid(
            "attaching needs a terminal; use `ptyro new -d` to start detached",
        ));
    }
    if std::env::var_os("PTYRO_SESSION_ID").is_some()
        && std::env::var_os("PTYRO_ALLOW_NESTED").is_none()
    {
        return Err(Error::new(
            ErrorCode::Conflict,
            "already inside a ptyro session; nesting the TUI is confusing (set PTYRO_ALLOW_NESTED=1 to do it anyway)",
        ));
    }
    Ok(())
}

pub async fn attach(
    ctx: &Ctx,
    client: Client,
    info: Option<SessionInfo>,
    raw: bool,
    view_only: bool,
) -> Result<i32, Error> {
    attach_with(ctx, client, info, ptyro_tui::Options { raw, view_only, ..Default::default() })
        .await
}

/// `ptyro shell` (UX1.25): the quick terminal here, attached in the TUI's drawer, as JSON, or
/// its screen.
async fn quick_shell(
    ctx: &Ctx,
    worktree: Option<String>,
    json: bool,
    read: bool,
    scrollback: Option<Option<u32>>,
) -> Result<i32, Error> {
    let attaching = !json && !read;
    if attaching {
        check_attachable()?;
    }
    let client =
        if read { ctx.connect(false).await.map_err(no_daemon)? } else { ctx.connect(true).await? };
    let req = Request::QuickShell {
        worktree: addr_here_opt(worktree)?,
        cwd: current_dir()?,
        create: !read,
        env: client_env(),
        size: None,
        workspace: None,
    };
    let info = session_of(client.request(req).await?)?;
    if read {
        drop(client);
        return crate::script::read(ctx, info.id.full(), scrollback, false, false).await;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default());
        return Ok(0);
    }
    attach_with(ctx, client, Some(info), ptyro_tui::Options::default()).await
}

pub async fn attach_with(
    ctx: &Ctx,
    client: Client,
    info: Option<SessionInfo>,
    mut tui: ptyro_tui::Options,
) -> Result<i32, Error> {
    check_attachable()?;
    // The TUI connects on its own, announcing the terminal's colours in its Hello.
    drop(client);
    let opts = ctx.options(ClientKind::Tui, false);
    let paths = ctx.paths.clone();
    tui.workspace = tui.workspace.or_else(|| info.as_ref().and_then(|i| i.workspace.clone()));
    tui.focus = info.map(|i| i.id);
    let end = ptyro_tui::run(opts.clone(), &paths, tui.clone()).await;
    match end {
        Ok(AttachEnd::Restart { from, to, workspace, session }) => {
            restart_tui(&restart_args(ctx, &tui, &from, &to, &workspace, session.as_deref()), &to)
        }
        Ok(AttachEnd::Detached) => {
            eprintln!("[detached]");
            Ok(0)
        }
        Ok(AttachEnd::Exited(state)) => {
            eprintln!("[session {}]", state.label());
            Ok(0)
        }
        Ok(AttachEnd::Gone) => {
            eprintln!("[no sessions left]");
            Ok(0)
        }
        Ok(AttachEnd::Stopped) => {
            eprintln!(
                "[ptyro stopped: agents that can resume come back dormant; `ptyro` starts it again]"
            );
            Ok(0)
        }
        Ok(AttachEnd::Lost(reason)) => {
            Err(Error::new(ErrorCode::DaemonUnavailable, format!("lost the daemon: {reason}")))
        }
        Err(e) => Err(Error::internal(format!("terminal: {e:#}"))),
    }
}

/// What the new TUI is started with: attached to the workspace this one showed (its view comes
/// back from the daemon), never the command that started this one (`ptyro new …` would start
/// another session); the same `--remote`, raw mode's one session, view-only.
fn restart_args(
    ctx: &Ctx,
    tui: &ptyro_tui::Options,
    from: &str,
    to: &str,
    workspace: &str,
    session: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(r) = &ctx.remote {
        args.extend(["--remote".to_owned(), r.clone()]);
    }
    args.push("attach".into());
    match session {
        Some(s) => args.extend([s.to_owned(), "--raw".into()]),
        None if !workspace.is_empty() => args.extend(["--workspace".into(), workspace.to_owned()]),
        None => {}
    }
    if tui.view_only {
        args.push("--view-only".into());
    }
    args.extend(["--restarted".into(), format!("{from}:{to}")]);
    args
}

/// An update replaced this binary and upgraded the daemon: the new TUI takes this one's place,
/// in this terminal (SPEC §24.8a): it replaces this process.
fn restart_tui(args: &[String], to: &str) -> Result<i32, Error> {
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    let exe = ptyro_agents::own::installed_name(&exe);
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(args);
    let failed = |e: std::io::Error| {
        Error::internal(format!(
            "ptyro {to} is installed, but it didn't start in this TUI's place ({e}): run `ptyro` again"
        ))
    };
    use std::os::unix::process::CommandExt;
    Err(failed(cmd.exec()))
}

/// Expand selectors (each may be a name, handle or `@…` selector) into sessions.
pub async fn targets(client: &Client, addrs: &[String]) -> Result<Vec<SessionInfo>, Error> {
    let mut out: Vec<SessionInfo> = Vec::new();
    for addr in addrs {
        let Reply::Sessions { sessions } =
            client.request(Request::Resolve { addr: addr.clone() }).await?
        else {
            return Err(Error::internal("unexpected reply"));
        };
        if sessions.is_empty() {
            return Err(ptyro_core::lookup::not_found("session", addr, []));
        }
        for s in sessions {
            if !out.iter().any(|o| o.id == s.id) {
                out.push(s);
            }
        }
    }
    Ok(out)
}

async fn history(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let client = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::History { entries } = client.request(Request::History { limit: Some(200) }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
        return Ok(0);
    }
    println!("{:<20} {:<16} {:<26} {:<6} {:<6} CWD", "NAME", "STATE", "COMMAND", "AGE", "RAN");
    for e in &entries {
        let ran =
            e.ended_ms.map_or_else(|| "-".into(), |end| duration(end.saturating_sub(e.created_ms)));
        let mark = if e.current { "" } else { " (gone)" };
        println!(
            "{:<20} {:<16} {:<26} {:<6} {:<6} {}",
            truncate(&e.name, 20),
            truncate(&format!("{}{mark}", e.state.label()), 16),
            truncate(&e.argv.join(" "), 26),
            ago(e.created_ms),
            ran,
            e.cwd.display()
        );
    }
    Ok(0)
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    /// PowerShell (pwsh 7+)
    #[value(alias = "powershell")]
    Pwsh,
}

impl Shell {
    pub fn kind(self) -> ptyro_core::shell::ShellKind {
        use ptyro_core::shell::ShellKind;
        match self {
            Self::Bash => ShellKind::Bash,
            Self::Zsh => ShellKind::Zsh,
            Self::Fish => ShellKind::Fish,
            Self::Pwsh => ShellKind::Pwsh,
        }
    }
}

/// Shell integration (SPEC §5.7, `ptyro_core::shell`): OSC 133 prompt marks (copy-mode prompt
/// jumps, last exit status) and the directory. Each snippet does nothing outside ptyro (`$PTYRO`
/// unset).
/// The snippet, after the lines that put the tools `ptyro setup` installed on `PATH` (only the
/// folders that exist: nothing for a machine that never needed them).
pub fn shell_init(shell: Shell, paths: &Paths) -> String {
    let dirs: Vec<_> =
        ptyro_agents::setup::path_dirs(paths).into_iter().filter(|d| d.is_dir()).collect();
    let kind = shell.kind();
    format!("{}{}", ptyro_core::shell::path_lines(kind, &dirs), kind.snippet())
}

/// `ls --status/--project/--workspace`.
pub struct Filter {
    pub status: Option<ptyro_core::selector::State>,
    pub project: Option<String>,
    pub workspace: Option<String>,
    /// Chats' engines too (SPEC §22.2).
    pub chats: bool,
}

impl Filter {
    fn keeps(&self, s: &SessionInfo, projects: &[ptyro_core::model::ProjectInfo]) -> bool {
        let status_ok = self.status.is_none_or(|want| {
            want.matches(&s.state, s.status.as_ref().map(|r| r.status), || s.is_active())
        });
        let project_ok = self.project.as_deref().is_none_or(|p| {
            s.project
                .as_deref()
                .is_some_and(|id| id == p || projects.iter().any(|x| x.id == id && x.slug == p))
        });
        let ws_ok = self.workspace.as_deref().is_none_or(|w| s.workspace.as_deref() == Some(w));
        status_ok && project_ok && ws_ok && (self.chats || s.chat.is_none())
    }
}

async fn list(ctx: &Ctx, json: bool, mut filter: Filter) -> Result<i32, Error> {
    let (all, worktrees) = match ctx.connect(false).await {
        Ok(client) => {
            let worktrees = match client.request(Request::WorktreeList { project: None }).await? {
                Reply::Worktrees { worktrees } => worktrees,
                _ => Vec::new(),
            };
            let projects = match client.request(Request::ProjectList).await? {
                Reply::Projects { projects } => projects,
                _ => Vec::new(),
            };
            // `-p .` (a path): the project it is in, by id.
            if let Some(p) = filter.project.clone().filter(|p| ptyro_core::os::looks_like_path(p))
                && let Reply::Project { info, .. } =
                    client.request(Request::ProjectGet { addr: addr_here(p)? }).await?
            {
                filter.project = Some(info.id);
            }
            let mut all = sessions(&client).await?;
            all.retain(|s| filter.keeps(s, &projects));
            (all, worktrees)
        }
        // No local daemon simply means no sessions (`local` too: this computer's own); an
        // unreachable remote is an error.
        Err(e)
            if e.code == ErrorCode::DaemonUnavailable && {
                let remote = ctx.options(ClientKind::Cli, false).remote;
                remote.is_none() || ptyro_client::is_local(remote.as_deref())
            } =>
        {
            (Vec::new(), Vec::new())
        }
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
        return Ok(0);
    }
    if all.is_empty() {
        eprintln!("no sessions — start one with `ptyro new`");
        return Ok(0);
    }
    let profiles = all.iter().any(|s| s.profile != ptyro_core::model::DEFAULT_PROFILE);
    // A MODEL column once any agent has said which model it runs.
    let models = all.iter().any(|s| s.model.is_some());
    let home = std::env::var("HOME").unwrap_or_default();
    println!(
        "{:<8} {:<18} {:<11} {:<20} {}{:<24} {}{:>3}  {:<5} STATUS",
        "HANDLE",
        "NAME",
        "STATE",
        "COMMAND",
        if models { format!("{:<12} ", "MODEL") } else { String::new() },
        "WHERE",
        if profiles { format!("{:<10} ", "PROFILE") } else { String::new() },
        "V",
        "AGE"
    );
    for s in &all {
        let cmd = match &s.script {
            Some(role) => format!("script {}", role.trim_start_matches("run:")),
            None => s.foreground.clone().unwrap_or_else(|| s.argv.join(" ")),
        };
        // What it's doing: an agent's status as `ptyro status` says it ("blocked: trust this
        // folder?"), else what the program reports of itself. An agent's used to be missing here.
        let status = match &s.status {
            Some(_) if s.state.is_live() => crate::agents::status_text(s),
            _ => s.program_status.as_ref().map_or(String::new(), |p| {
                let kind = p.kind.as_deref().map(|k| format!("/{k}")).unwrap_or_default();
                format!("{}{kind}", p.state)
            }),
        };
        // A private session says so, with incognito's mark (SPEC §25.7a).
        let status = if s.private { format!("◌ private {status}") } else { status };
        let status = status.trim_end();
        let place = s
            .worktree
            .as_deref()
            .and_then(|w| worktrees.iter().find(|x| x.id == w))
            .map(ptyro_core::model::WorktreeInfo::qualified)
            .unwrap_or_else(|| {
                let cwd = s.cwd.display().to_string();
                match cwd.strip_prefix(&home) {
                    Some(rest) if !home.is_empty() => format!("~{rest}"),
                    _ => cwd,
                }
            });
        println!(
            "{:<8} {:<18} {:<11} {:<20} {}{:<24} {}{:>3}  {:<5} {}",
            s.handle,
            truncate(&s.name, 18),
            s.state.label(),
            truncate(&cmd, 20),
            if models {
                let m = s.model.as_deref().map(ptyro_core::session::model_label);
                format!("{:<12} ", truncate(m.as_deref().unwrap_or("-"), 12))
            } else {
                String::new()
            },
            truncate(&place, 24),
            if profiles { format!("{:<10} ", truncate(&s.profile, 10)) } else { String::new() },
            s.viewers,
            ago(s.created_ms),
            status
        );
    }
    Ok(0)
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

pub use ptyro_core::ago;

/// Stop the daemon; `hibernate` puts resumable agents to sleep first (SPEC §7.4).
pub(crate) async fn stop_daemon(ctx: &Ctx, force: bool, hibernate: bool) -> Result<i32, Error> {
    let client = match ctx.connect(false).await {
        Ok(c) => c,
        Err(e) if e.code == ErrorCode::DaemonUnavailable => {
            eprintln!("daemon is not running");
            return Ok(0);
        }
        Err(e) => return Err(e),
    };
    if hibernate {
        for s in sessions(&client).await? {
            let resumable = s.state.is_live() && s.conversation.is_some();
            if resumable && client.request(Request::Hibernate { addr: s.id.full() }).await.is_ok() {
                eprintln!("hibernated {}", s.name);
            }
        }
    }
    client.request(Request::Shutdown { force }).await?;
    drop(client);
    // The socket goes first, then the daemon stops its sessions (agents hibernate, which
    // takes a few seconds) and exits: wait for that, so a new daemon can start right away.
    let start = std::time::Instant::now();
    // It holds daemon.lock until it exits (zombies included, unlike a pid check).
    let lock = ctx.paths.lock_file();
    let alive = || std::fs::File::open(&lock).is_ok_and(|f| f.try_lock_shared().is_err());
    loop {
        let up = if ctx.remote.is_some() {
            // Its files are on the other machine: it's gone when it no longer answers.
            tokio::time::sleep(Duration::from_millis(500)).await;
            !matches!(ctx.connect(false).await, Err(e) if e.code == ErrorCode::DaemonUnavailable)
        } else {
            ctx.paths.socket.exists() || alive()
        };
        if !up {
            break;
        }
        if start.elapsed() >= Duration::from_secs(30) {
            return Err(Error::new(
                ErrorCode::Timeout,
                "the daemon is still running 30s after it was asked to stop (still stopping sessions?)",
            ));
        }
        if ctx.remote.is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    eprintln!("daemon stopped");
    Ok(0)
}

async fn daemon(ctx: &Ctx, cmd: DaemonCmd) -> Result<i32, Error> {
    match cmd {
        DaemonCmd::Start { .. } => unreachable!("handled in main"),
        DaemonCmd::Stop { force, hibernate } => stop_daemon(ctx, force, hibernate).await,
        DaemonCmd::Restart { force, hibernate } => {
            stop_daemon(ctx, force, hibernate).await?;
            let client = ctx.connect(true).await?;
            let Reply::Status { status } = client.request(Request::Status).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            eprintln!("daemon started (pid {})", status.pid);
            Ok(0)
        }
        DaemonCmd::InstallService { dry_run } => crate::service::install(ctx, dry_run),
        DaemonCmd::UninstallService { dry_run } => crate::service::uninstall(ctx, dry_run),
        DaemonCmd::Status { json } => {
            let client = ctx.connect(false).await.map_err(|e| {
                if e.code == ErrorCode::DaemonUnavailable {
                    Error::new(e.code, "daemon is not running")
                } else {
                    e
                }
            })?;
            let Reply::Status { status } = client.request(Request::Status).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status).unwrap_or_default());
            } else {
                println!(
                    "ptyro daemon {} (protocol {}.{})",
                    status.version, status.proto.major, status.proto.minor
                );
                if let Some(m) = &status.machine {
                    println!("  machine   {m}");
                }
                println!("  pid       {}", status.pid);
                println!("  uptime    {}", ago(status.started_ms));
                if let Some(l) = &status.load {
                    println!("  load      {} ({} CPUs)", l.text(), l.cpus);
                }
                println!("  sessions  {} ({} running)", status.sessions, status.live_sessions);
                println!("  clients   {}", status.clients);
                println!("  socket    {}", status.socket.display());
                println!("  logs      {}", status.log_dir.display());
                if let Some(h) = &status.wsl_hold {
                    println!("  holds WSL {}", crate::about::hold_text(h));
                }
                if let Some(w) = &status.web {
                    let l = crate::webapp::lines(w);
                    println!("  web app   {}", l.join("\n            "));
                    if !w.wanted && w.by_hand.is_none() {
                        println!(
                            "            `ptyro web on` has the daemon run it, for your phone"
                        );
                    }
                }
                if status.generation > 0 {
                    println!(
                        "  upgrades  {} (last {} ago{})",
                        status.generation,
                        status.upgraded_ms.map(ago).unwrap_or_default(),
                        if status.rolled_back { ", rolled back" } else { "" }
                    );
                }
                if let Some(m) = &status.metrics {
                    println!(
                        "  memory    {} MiB RSS, {} fds, {} tasks",
                        m.rss_kib / 1024,
                        m.open_fds,
                        m.tasks_alive
                    );
                    println!(
                        "  loop lag  {} ms max, {} ms p99 (last minute)",
                        m.loop_lag_ms_max, m.loop_lag_ms_p99
                    );
                    println!(
                        "  io        {} PTY readers, {} MiB read, {} frames sent",
                        m.pty_readers,
                        m.bytes_read / (1024 * 1024),
                        m.frames_sent
                    );
                    println!(
                        "  terminal  ~{} MiB of screens and scrollback",
                        m.scrollback_bytes / (1024 * 1024)
                    );
                    if m.crashes > 0 {
                        println!(
                            "  crashes   {} (reports in {}/crashes)",
                            m.crashes,
                            ctx.paths.state.display()
                        );
                    }
                }
            }
            Ok(0)
        }
        DaemonCmd::HandoffVersion => {
            println!(
                "{}",
                HANDOFF_VERSIONS.iter().map(u32::to_string).collect::<Vec<_>>().join(" ")
            );
            Ok(0)
        }
        DaemonCmd::Upgrade { binary, json } => upgrade(ctx, binary, json).await,
        DaemonCmd::Logs { lines, follow } => {
            let dir = ctx.paths.log_dir();
            let newest = std::fs::read_dir(&dir)
                .map_err(|e| Error::new(ErrorCode::NotFound, format!("{}: {e}", dir.display())))?
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("daemon"))
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
            let Some(entry) = newest else {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!("no daemon logs in {}", dir.display()),
                ));
            };
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            eprintln!("==> {} <==", entry.path().display());
            for l in &all[all.len().saturating_sub(lines)..] {
                println!("{l}");
            }
            if follow {
                use std::io::{Read, Seek, Write};
                let mut f = std::fs::File::open(entry.path())
                    .map_err(|e| Error::internal(e.to_string()))?;
                let mut pos = text.len() as u64;
                loop {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let len = f.metadata().map(|m| m.len()).unwrap_or(pos);
                    if len < pos {
                        pos = 0; // rotated or truncated
                    }
                    if len > pos {
                        let _ = f.seek(std::io::SeekFrom::Start(pos));
                        let mut buf = Vec::new();
                        let _ = Read::by_ref(&mut f).take(len - pos).read_to_end(&mut buf);
                        pos = len;
                        let mut out = std::io::stdout().lock();
                        if out.write_all(&buf).and_then(|_| out.flush()).is_err() {
                            return Ok(0);
                        }
                    }
                }
            }
            Ok(0)
        }
    }
}

async fn machine(ctx: &Ctx, cmd: Option<crate::MachineCmd>) -> Result<i32, Error> {
    use crate::MachineCmd as M;
    match cmd.unwrap_or(M::Ls { json: false }) {
        M::Ls { json } => crate::machines::ls(ctx, json).await,
        M::Add { target, name, force, json } => {
            crate::machines::add(ctx, target, name, force, json).await
        }
        M::Rm { name, yes } => {
            // Found first, then asked about: a typo asks nothing.
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let Reply::Machines { machines } = c.request(Request::MachineList).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            // By name, any case, as the daemon finds it.
            let Some(m) = machines.iter().find(|m| ptyro_core::lookup::same(&m.name, &name)) else {
                let known: Vec<String> = machines.iter().map(|m| m.name.clone()).collect();
                return Err(ptyro_core::lookup::not_found_among("machine", &name, &known));
            };
            let name = m.name.clone();
            ask_first(&format!("Stop mirroring {name}? {}", ptyro_core::removal::MACHINE), yes)?;
            let said = match &m.transport {
                // Its device there (named after this machine) stays until it is revoked there.
                ptyro_core::model::MachineTransport::Token { .. } => {
                    let me = match c.request(Request::Status).await {
                        Ok(Reply::Status { status }) => {
                            status.machine.as_deref().and_then(ptyro_core::names::derive)
                        }
                        _ => None,
                    };
                    format!(
                        "{name} is no longer mirrored here. Its pairing with this machine stays on {name} until you revoke it there: `ptyro devices revoke {}` on {name} (Devices in its web app)",
                        me.unwrap_or_else(|| "NAME".into())
                    )
                }
                ptyro_core::model::MachineTransport::Ssh { .. } => {
                    format!("{name} is no longer mirrored here")
                }
            };
            crate::machines::simple(ctx, Request::MachineRemove { name }, said).await
        }
        M::Hide { name, workspace } => {
            let req = Request::MachineHide { name, workspace, hidden: true };
            crate::machines::simple(ctx, req, String::new()).await
        }
        M::Unhide { name, workspace } => {
            let req = Request::MachineHide { name, workspace, hidden: false };
            crate::machines::simple(ctx, req, String::new()).await
        }
        M::Retry { name } => {
            crate::machines::simple(ctx, Request::MachineRetry { name }, String::new()).await
        }
        M::Update { names, all, yes, json } => {
            crate::linked::machine_update(ctx, names, all, yes, json).await
        }
        M::Pair { url, json } => crate::machines::pair(ctx, url, json).await,
    }
}

/// The handoff images this binary's daemon can take over from.
const HANDOFF_VERSIONS: &[u32] = ptyro_daemon::HANDOFF_VERSIONS;

#[cfg(test)]
mod path_tests {
    #[test]
    fn a_relative_path_is_this_clients_and_a_name_stays_a_name() {
        use super::{addr_here, current_dir};
        let here = current_dir().unwrap().to_string_lossy().into_owned();
        assert_eq!(addr_here(".".into()).unwrap(), here);
        let up = current_dir().unwrap().parent().unwrap().to_string_lossy().into_owned();
        assert_eq!(addr_here("..".into()).unwrap(), up);
        for name in ["myrepo", "myrepo/brisk-rook", "/abs/path", "~/src/app", "c:7f3k"] {
            assert_eq!(addr_here(name.into()).unwrap(), name, "a name, id or absolute path");
        }
        assert!(addr_here("./no-such-folder-here".into()).is_err(), "a missing path says so");
    }
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    /// A restart attaches to the workspace shown, never what first started the TUI.
    #[test]
    fn a_restart_attaches_to_the_same_place_never_reruns_the_command() {
        let paths = ptyro_core::paths::Paths::resolve(|v| {
            (v == "PTYRO_HOME").then(|| std::env::temp_dir().join("ptyro-restart-args"))
        })
        .unwrap();
        let mut ctx = Ctx::local(paths);
        let tui = ptyro_tui::Options::default();
        assert_eq!(
            restart_args(&ctx, &tui, "0.0.68", "0.0.69", "work", None),
            ["attach", "--workspace", "work", "--restarted", "0.0.68:0.0.69"]
        );
        ctx.remote = Some("box".into());
        let raw = ptyro_tui::Options { raw: true, view_only: true, ..Default::default() };
        assert_eq!(
            restart_args(&ctx, &raw, "0.0.68", "0.0.69", "work", Some("01abc")),
            [
                "--remote",
                "box",
                "attach",
                "01abc",
                "--raw",
                "--view-only",
                "--restarted",
                "0.0.68:0.0.69"
            ]
        );
    }
}
