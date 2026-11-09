//! CLI command implementations (SPEC §12). Errors print as `ptyro: …` and exit with the codes of
//! SPEC §12.4.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::time::Duration;

use ptyro_client::{Client, ConnectOptions};
use ptyro_core::paths::Paths;
use ptyro_core::session::{SessionInfo, SpawnSpec};
use ptyro_core::{Error, ErrorCode, TermSize, now_ms};
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

pub fn daemon_start(foreground: bool, spawned: bool, serve: bool) -> i32 {
    if serve {
        // Written before it starts, so the daemon reads it as its first config.
        let paths = match Paths::from_env() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ptyro: {e}");
                return 1;
            }
        };
        match ptyro_core::config::set_serve(&ptyro_core::config::Config::path(&paths), true, None) {
            Ok(notes) => notes.iter().for_each(|n| eprintln!("web app: {n}")),
            Err(e) => {
                eprintln!("ptyro: {}", e.message);
                return e.code.exit_code();
            }
        }
    }
    // Leave the client's session and process group so its terminal closing can't hang us up.
    // Done before any runtime exists: we never fork an initialized tokio process. (On Windows the
    // client starts it detached instead.)
    #[cfg(unix)]
    if spawned {
        let _ = rustix::process::setsid();
    }
    #[cfg(not(unix))]
    let _ = spawned;
    // Windows: ptyro is an app Windows knows (its toasts' name and icon, its links) (WIN10).
    #[cfg(windows)]
    if let Ok(p) = Paths::from_env() {
        crate::winapp::ensure(&p.data);
    }
    match ptyro_daemon::run(ptyro_daemon::Options { foreground }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ptyro: {e:#}");
            if e.to_string().contains("already running") {
                if serve {
                    // The daemon that is running reads the config it was just given.
                    eprintln!(
                        "ptyro: the daemon was already running; the web app starts in it now (`ptyro web`)"
                    );
                    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
                    if let (Ok(rt), Ok(paths)) = (rt, Paths::from_env()) {
                        let ctx = Ctx::local(paths);
                        let _ = rt.block_on(crate::webapp::ask(&ctx, Some(true), None));
                    }
                }
                ErrorCode::Conflict.exit_code()
            } else {
                1
            }
        }
    }
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
    /// local`): on Windows, what lets ptyro start one (WIN5).
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
    // Where the daemon is (WIN3). Commands that never talk to one don't go looking for it.
    let discover = !matches!(
        cmd,
        Some(
            Cmd::Config { .. }
                | Cmd::Completions { .. }
                | Cmd::ShellInit { .. }
                | Cmd::Init { .. }
                | Cmd::Setup { .. }
                | Cmd::Install { .. }
                | Cmd::Update { .. }
                | Cmd::Login { .. }
                | Cmd::Uninstall { .. }
                | Cmd::Backup { .. }
                | Cmd::Daemon { .. }
                | Cmd::Proxy
        )
    );
    // Windows: so the toasts of a daemon in WSL say "ptyro" and open their session (WIN10). Not
    // on the way out.
    if !matches!(cmd, Some(Cmd::Uninstall { .. })) {
        crate::winapp::ensure(&paths.data);
    }
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
                let there = match r.strip_prefix("wsl://") {
                    Some(d) if !d.is_empty() => format!("`wsl -d {d} -- ptyro update`"),
                    Some(_) => "`wsl ptyro update`".to_owned(),
                    None => format!("`ptyro update` on {r}"),
                };
                return Err(Error::invalid(format!(
                    "update is about this computer's agents, not {r}'s: run {there}"
                )));
            }
            let what = if matches!(cmd, Some(Cmd::Login { .. })) { "login" } else { "setup" };
            if r.starts_with("wsl://") {
                return Err(Error::invalid(format!(
                    "{what} reaches WSL itself: `ptyro {what} --where wsl` (or both), not --remote {r}"
                )));
            }
            return Err(Error::invalid(format!(
                "{what} is about this computer, not {r}: run `ptyro {what}` there"
            )));
        }
        None
    } else {
        crate::winclient::resolve(remote, &paths, discover).await
    };
    // `local` is this computer's own daemon, not a remote one.
    let local = ptyro_client::is_local(remote.as_deref());
    let remote = remote.filter(|_| !local);
    DAEMON_ELSEWHERE.store(remote.is_some(), std::sync::atomic::Ordering::Relaxed);
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
            take_changes,
            pr,
            issue,
            unqueued,
            verify,
            lead,
            max_workers,
            lead_agents,
            structured,
            argv,
        }) => {
            let lead = (lead || max_workers.is_some() || !lead_agents.is_empty()).then(|| {
                let mut p = serde_json::json!({});
                if let Some(n) = max_workers {
                    p["max_sessions"] = n.into();
                }
                if !lead_agents.is_empty() {
                    p["agents"] = lead_agents.clone().into();
                }
                p
            });
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
                take_changes,
                pr,
                issue,
                unqueued,
                verify,
                lead,
                structured,
                welcome: false,
                pad_item: None,
            };
            let new_wt = new_wt.or_else(|| {
                (a.pr.is_some() || a.issue.is_some() || a.take_changes).then(String::new)
            });
            match new_wt {
                Some(wt_name) => {
                    crate::projects::new_in_new_worktree(&ctx, a, wt_name, project).await
                }
                None => new(&ctx, a).await,
            }
        }
        Some(Cmd::Attach { session, workspace, raw, view_only, tab, pane }) => {
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
            let tui = ptyro_tui::Options { raw, view_only, workspace, ..Default::default() };
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
        Some(Cmd::Init { yes, dry_run }) => crate::init::run(&ctx, yes, dry_run).await,
        Some(Cmd::Setup { yes, agents, tools, dry_run, json, place, survey, move_, tty }) => {
            let a = crate::setup::SetupArgs {
                yes,
                agents,
                tools,
                dry_run,
                json,
                place,
                survey,
                tty,
                move_,
            };
            crate::setup::setup(&ctx, a).await
        }
        Some(Cmd::Install {
            names,
            yes,
            dry_run,
            json,
            place,
            admin,
            tty,
            confirmed,
            move_,
            moves,
        }) => {
            let a = crate::setup::InstallArgs {
                names,
                yes,
                dry_run,
                json,
                place,
                admin,
                tty,
                confirmed,
                move_,
                moves,
            };
            crate::setup::install(&ctx, a).await
        }
        Some(Cmd::Update { names, check, yes, json, tty }) => {
            let a = crate::setup::UpdateArgs { names, check, yes, json, tty };
            crate::setup::update(&ctx, a).await
        }
        Some(Cmd::Login { names, check, json, place, tty, installed }) => {
            let a = crate::logins::LoginArgs { names, check, json, place, tty, installed };
            crate::logins::login(&ctx, a).await
        }
        Some(Cmd::About { json }) => crate::about::run(&ctx, json).await,
        Some(Cmd::Doctor { quick, json }) => crate::doctor::run_cmd(&ctx, quick, json).await,
        Some(Cmd::Tour { here: false }) => run_tour(&ctx).await,
        Some(Cmd::Tour { here: true }) => tour_here(&ctx).await,
        // Kill is immediate and keeps the session listed, as in the TUI and the web (SPEC §7.3).
        Some(Cmd::Kill { sessions, signal, keep: _ }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            for info in targets(&client, &sessions).await? {
                if !info.state.is_live() {
                    eprintln!("{} isn't running", info.name);
                    continue;
                }
                let signal = signal.unwrap_or(9);
                client
                    .request(Request::Kill { addr: info.id.full(), signal: Some(signal) })
                    .await?;
                match signal {
                    9 => eprintln!("killed {}", info.name),
                    n => eprintln!("sent signal {n} to {}", info.name),
                }
            }
            Ok(0)
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
        Some(Cmd::Rm { sessions, stop }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            for info in targets(&client, &sessions).await? {
                client.request(Request::Remove { addr: info.id.full(), stop }).await?;
                if stop && info.state.is_live() {
                    eprintln!("stopped and removed {}", info.name);
                }
            }
            Ok(0)
        }
        Some(Cmd::Rename { session, name }) => {
            let client = ctx.connect(false).await.map_err(no_daemon)?;
            let info = session_of(client.request(Request::Rename { addr: session, name }).await?)?;
            eprintln!("renamed to {}", info.name);
            Ok(0)
        }
        Some(Cmd::Read { session, chat: true, json, .. }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            match c.request(Request::ChatGet { addr: session }).await? {
                Reply::Transcript { items } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
                    } else {
                        for i in items {
                            println!("{}", serde_json::to_string(&i).unwrap_or_default());
                        }
                    }
                    Ok(0)
                }
                r => Err(unexpected(r)),
            }
        }
        Some(Cmd::Read { session, screen: _, scrollback, ansi, plain: _, json, chat: false }) => {
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
            backup,
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
                backup,
                purge_refs,
            };
            crate::uninstall::run(&ctx, opts, json).await
        }
        Some(Cmd::Backup { output, with_recordings, with_ca, no_chats, json }) => {
            let opts = crate::backup::Opts { output, with_recordings, with_ca, no_chats };
            crate::backup::run(&ctx, opts, json).await
        }
        Some(Cmd::Project { cmd }) => crate::projects::project(&ctx, cmd).await,
        Some(Cmd::Pr { cmd }) => crate::ship::pr(&ctx, cmd).await,
        Some(Cmd::WatchPr { worktree, off }) => {
            crate::ship::pr(&ctx, crate::ship::PrCmd::Watch { worktree, off }).await
        }
        Some(Cmd::Review { cmd }) => crate::ship::review(&ctx, cmd).await,
        Some(Cmd::Run { cmd }) => crate::orch::run(&ctx, cmd).await,
        Some(Cmd::Recipe { cmd }) => crate::orch::recipe(&ctx, cmd).await,
        Some(Cmd::Fanout {
            agents,
            n,
            prompt,
            prompt_file,
            project,
            base,
            name,
            test,
            verify,
            judge,
            timeout,
            structured,
            wait,
            json,
        }) => {
            let spec = ptyro_run::wire::FanoutSpec {
                agents,
                prompt: String::new(),
                project,
                base,
                name,
                test,
                verify,
                judge,
                timeout,
                structured,
            };
            crate::orch::fanout(&ctx, spec, n, prompt, prompt_file, wait, json).await
        }
        Some(Cmd::Compare { run, refresh, json }) => {
            crate::orch::compare(&ctx, run, refresh, json).await
        }
        Some(Cmd::Pick { run, candidate, cherry_pick, pr, apply, into, paths, hunks }) => {
            crate::orch::pick(&ctx, run, candidate, cherry_pick, pr, apply, into, paths, hunks)
                .await
        }
        Some(Cmd::Discard { run, candidate, force }) => {
            crate::orch::discard(&ctx, run, candidate, force).await
        }
        Some(Cmd::Iterate { run, args }) => crate::orch::iterate(&ctx, run, args).await,
        Some(Cmd::Gate { cmd }) => crate::orch::gate(&ctx, cmd).await,
        Some(Cmd::Cs { cmd }) => crate::cs::cs(&ctx, cmd).await,
        Some(Cmd::Msg { cmd }) => crate::orch::msg(&ctx, cmd).await,
        Some(Cmd::Lease { cmd }) => crate::orch::lease(&ctx, cmd).await,
        Some(Cmd::Radar { project, json }) => crate::orch::radar(&ctx, project, json).await,
        Some(Cmd::Checkpoint { cmd }) => crate::orch::checkpoint(&ctx, cmd).await,
        Some(Cmd::Verify { session, check, off, max_bounces }) => {
            crate::orch::verify(&ctx, session, check, off, max_bounces).await
        }
        Some(Cmd::Mcp { chat }) => crate::mcp::serve(&ctx, chat).await,
        Some(Cmd::Proxy) => crate::remote::proxy(&ctx).await,
        Some(Cmd::Convert { session, to, force }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let structured = to == "structured";
            match c.request(Request::Convert { addr: session, structured, force }).await? {
                Reply::Session { info } => {
                    eprintln!(
                        "{} now runs {} (same conversation) — attach with `ptyro attach {}`",
                        info.name,
                        if structured { "in structured mode" } else { "in its own terminal UI" },
                        info.name
                    );
                    Ok(0)
                }
                r => Err(unexpected(r)),
            }
        }
        Some(Cmd::Retitle { target, title }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            let title = (!title.is_empty()).then(|| title.join(" "));
            let cleared = title.is_none();
            match c.request(Request::Retitle { addr: target, title, worktree: false }).await? {
                Reply::Session { info } if cleared => eprintln!("{}: title cleared", info.name),
                Reply::Session { info } => {
                    eprintln!("{}: {}", info.name, info.title.unwrap_or_default())
                }
                Reply::Worktree { info } => eprintln!(
                    "{}: {}",
                    info.qualified(),
                    info.title.unwrap_or_else(|| "title cleared".into())
                ),
                r => return Err(unexpected(r)),
            }
            Ok(0)
        }
        Some(Cmd::Summarize { session, json }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            match c.request(Request::Summarize { addr: session }).await? {
                Reply::Session { info } => {
                    let summary = info.summary.unwrap_or_else(|| ptyro_core::session::Summary {
                        text: String::new(),
                        turn: info.turns,
                        at_ms: 0,
                    });
                    if json {
                        println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
                    } else {
                        println!("{}: {}", info.name, summary.text);
                    }
                    Ok(0)
                }
                r => Err(unexpected(r)),
            }
        }
        Some(Cmd::Adopt { session }) => {
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            match c.request(Request::Adopt { addr: session }).await? {
                Reply::Session { info } => {
                    eprintln!(
                        "{} runs Claude as a managed session now, same conversation — attach with `ptyro attach {}`",
                        info.name, info.name
                    );
                    Ok(0)
                }
                r => Err(unexpected(r)),
            }
        }
        Some(Cmd::Scratchpad { cmd, pane, json }) => crate::pad::run(&ctx, cmd, pane, json).await,
        Some(Cmd::Chat { cmd, new }) => crate::chat::run(&ctx, cmd, new).await,
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
        Some(Cmd::Pair { name, scope, url, json, open }) => {
            // "phone" is the default name; a browser on this computer is a "browser".
            let name = if open && name == "phone" { "browser".to_owned() } else { name };
            crate::remote::pair(&ctx, name, scope.as_deref(), url, json, open).await
        }
        Some(Cmd::Devices { cmd }) => crate::remote::devices(&ctx, cmd).await,
        Some(Cmd::Machine { cmd }) => machine(&ctx, cmd).await,
        Some(Cmd::MachineBridge { url, fingerprint }) => {
            crate::machines::bridge(url, fingerprint).await
        }
        Some(Cmd::Replay { session, speed, max_idle, from }) => {
            crate::record::replay(&ctx, session, speed, max_idle, from).await
        }
        Some(Cmd::Export { session, format, output }) => {
            crate::record::export(&ctx, session, format, output).await
        }
        Some(Cmd::Ws { cmd }) => crate::projects::ws(&ctx, cmd).await,
        Some(Cmd::Wt { cmd }) => crate::projects::wt(&ctx, cmd).await,
        Some(Cmd::Profile { cmd }) => crate::profiles::run(&ctx, cmd).await,
        Some(Cmd::Gc { dry_run, json, scratch }) => {
            crate::projects::gc(&ctx, dry_run, json, scratch).await
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
        Some(Cmd::Inbox { workspace, unread, read, json }) => {
            crate::agents::inbox(&ctx, workspace, unread, read, json).await
        }
        Some(Cmd::Mute { target, duration, off }) => {
            crate::agents::mute(&ctx, target, duration, off).await
        }
        Some(Cmd::Usage { json }) => crate::agents::usage(&ctx, json).await,
        Some(Cmd::Notify { cmd }) => crate::agents::notify(&ctx, cmd).await,
        Some(Cmd::Integrations { cmd }) => crate::agents::integrations(&ctx, cmd).await,
        Some(Cmd::Report { state, kind, msg, progress, session }) => {
            let addr = match session.or_else(|| std::env::var("PTYRO_SESSION_ID").ok()) {
                Some(a) => a,
                None => return Err(Error::invalid("not inside a ptyro session; pass --session")),
            };
            let c = ctx.connect(false).await.map_err(no_daemon)?;
            c.request(Request::Report { addr, state, kind, msg, progress }).await?;
            Ok(0)
        }
        Some(Cmd::Hook { .. } | Cmd::StatuslineRelay | Cmd::OpenUrl { .. }) => {
            unreachable!("handled in main")
        }
        Some(Cmd::ShellInit { shell }) => {
            print!("{}", shell_init(shell, &ctx.paths));
            Ok(0)
        }
        Some(Cmd::Daemon { cmd }) => daemon(&ctx, cmd).await,
        Some(Cmd::FakeAgent { .. }) => unreachable!("handled in main"),
        #[cfg(debug_assertions)]
        Some(Cmd::FakeGh { .. } | Cmd::FakeInstaller { .. } | Cmd::FakeWsl { .. }) => {
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
        } else if status.pid == before.pid {
            eprintln!(
                "upgraded in {ms} ms: daemon {} (pid {}, unchanged), {} session(s)",
                status.version, status.pid, status.sessions
            );
        } else {
            // Windows: the new daemon is another process; every session's program is the same.
            eprintln!(
                "upgraded in {ms} ms: daemon {} (pid {}, was {}), {} session(s) handed over",
                status.version, status.pid, before.pid, status.sessions
            );
        }
        return Ok(if status.rolled_back { 1 } else { 0 });
    }
}

/// `ptyro tour` (UX1.22): the daily loop with a pretend agent, in a ptyro of its own.
pub async fn run_tour(ctx: &Ctx) -> Result<i32, Error> {
    crate::tour::run(ctx).await
}

/// `ptyro tour --here`: the TUI with the tour's coach, on this ptyro (the tour's own, which
/// `run_tour` sets up). A shell to start from, if nothing runs yet.
async fn tour_here(ctx: &Ctx) -> Result<i32, Error> {
    check_attachable()?;
    let client = ctx.connect(true).await?;
    let info = match most_recent(&client).await? {
        Some(i) => i,
        None => {
            let spec = SpawnSpec {
                cwd: current_dir()?,
                env: client_env(),
                size: ptyro_tui::pane_size(&ctx.paths),
                ..SpawnSpec::default()
            };
            session_of(client.request(Request::Spawn { spec }).await?)?
        }
    };
    let tui = ptyro_tui::Options { tour: true, ..Default::default() };
    attach_with(ctx, client, Some(info), tui).await
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
        Again::Trust { fingerprint } => format!("--trust {fingerprint}"),
    };
    format!("{} (again with `{flag}`: {})", e.message, f.label.to_lowercase())
}

/// The fingerprint a refusal asks you to trust, if it asks.
pub fn trust_of(e: &Error) -> Option<String> {
    match &e.fix.as_ref()?.again {
        ptyro_core::error::Again::Trust { fingerprint } => Some(fingerprint.clone()),
        _ => None,
    }
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

/// `ptyro scratchpad start`: the item's words as a task, like `ptyro "…"`, and the new worktree
/// is noted on the item (NOTE1.3).
pub async fn start_pad_task(
    ctx: &Ctx,
    prompt: &str,
    agent: Option<String>,
    line: u32,
) -> Result<i32, Error> {
    let cwd = current_dir()?;
    let config = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&ctx.paths))
        .unwrap_or_default();
    let a = NewArgs {
        agent: Some(agent.unwrap_or_else(|| ptyro_agents::default_agent(&config))),
        prompt: Some(prompt.to_owned()),
        cwd: Some(cwd.clone()),
        pad_item: Some(line),
        ..NewArgs::default()
    };
    if in_git_repo(&cwd) {
        crate::projects::new_in_new_worktree(ctx, a, String::new(), None).await
    } else {
        new(ctx, a).await
    }
}

fn in_git_repo(dir: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-dir"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn default_action(ctx: &Ctx) -> Result<i32, Error> {
    let client = ctx.connect(true).await?;
    match most_recent(&client).await? {
        // The TUI restores the last workspace and its view (newest live session if none).
        Some(_) => attach(ctx, client, None, false, false).await,
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
    /// Another of what the workspace last started (UX3.2).
    pub again: bool,
    pub wt: Option<String>,
    pub override_pin: bool,
    pub yes: bool,
    pub prompt: Option<String>,
    /// Its title (UX1.24).
    pub title: Option<String>,
    /// A new worktree taking the main checkout's changes (UX1.10).
    pub take_changes: bool,
    /// `ptyro` with nothing running: the TUI opens on the first run or home (UX1.19).
    pub welcome: bool,
    pub pr: Option<u32>,
    pub issue: Option<String>,
    pub unqueued: bool,
    /// Verified-done check (M7.9).
    pub verify: Option<String>,
    /// Lead policy (M7.6).
    pub lead: Option<serde_json::Value>,
    /// Structured mode (M10).
    pub structured: bool,
    /// Started from the scratchpad item on this line (NOTE1.3).
    pub pad_item: Option<u32>,
}

/// The variables a session should see from this client (SSH agent, display).
pub fn client_env() -> BTreeMap<String, String> {
    ptyro_core::os::CLIENT_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
        .collect()
}

/// Whether the daemon is another computer's (a WSL distro counts): set once, by `dispatch`.
static DAEMON_ELSEWHERE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The current directory, as the daemon sees it: here, canonical; on Windows, where the daemon
/// is WSL's (WIN3), the same folder as the distro names it (`C:\src` is `/mnt/c/src`). A native
/// Windows daemon (WIN5) takes the Windows path as it is.
pub fn current_dir() -> Result<std::path::PathBuf, Error> {
    let cwd =
        std::env::current_dir().map_err(|e| Error::invalid(format!("current directory: {e}")))?;
    let cwd = cwd.canonicalize().map_err(|e| Error::invalid(format!("{}: {e}", cwd.display())))?;
    if cfg!(windows) && !DAEMON_ELSEWHERE.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(plain_windows_path(&cwd));
    }
    if cfg!(windows) {
        let shown = cwd.to_string_lossy();
        return ptyro_core::wsl::path_in_distro(&shown, "/mnt/").map(Into::into).ok_or_else(|| {
            Error::invalid(format!(
                "{shown} isn't a folder WSL can reach: work from a drive (C:\\…) or the distro \
                 (\\\\wsl.localhost\\…)"
            ))
        });
    }
    Ok(cwd)
}

/// `canonicalize`'s verbatim form as programs write it: `\\?\C:\src` is `C:\src`, and
/// `\\?\UNC\server\share` is `\\server\share`.
fn plain_windows_path(p: &std::path::Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}").into();
    }
    s.strip_prefix(r"\\?\").map_or_else(|| p.to_path_buf(), Into::into)
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
            } else if std::io::stdin().is_terminal() {
                Err(Error::new(ErrorCode::NeedsConfirmation, "cancelled"))
            } else {
                Err(Error::new(e.code, format!("{} (no terminal to ask: {flags})", e.message)))
            }
        }
        other => other,
    }
}

pub async fn new(ctx: &Ctx, a: NewArgs) -> Result<i32, Error> {
    let attaching = !a.detach;
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
        worktree: a.wt,
        profile: a.profile,
        override_pin: a.override_pin,
        confirmed: a.yes,
        name: a.name,
        env: client_env(),
        size,
        prompt: a.prompt,
        title: a.title,
        unqueued: a.unqueued,
        verify: a.verify,
        lead: a.lead,
        structured: a.structured,
        again: a.again,
        ..SpawnSpec::default()
    };
    let reply = with_confirmation(&client, Request::Spawn { spec }, false, |r| {
        if let Request::Spawn { spec } = r {
            spec.override_pin = true;
            spec.confirmed = true;
        }
    })
    .await?;
    if let Reply::Queued { position, unreviewed } = reply {
        eprintln!(
            "queued (#{position}): {unreviewed} worktrees wait for review (review.max_unreviewed); it starts when you've reviewed one (`ptyro review queue`), or pass --unqueued"
        );
        return Ok(0);
    }
    let info = session_of(reply)?;
    if attaching {
        let tui = ptyro_tui::Options { welcome: a.welcome, ..Default::default() };
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
    scrollback: Option<u32>,
) -> Result<i32, Error> {
    let attaching = !json && !read;
    if attaching {
        check_attachable()?;
    }
    let client =
        if read { ctx.connect(false).await.map_err(no_daemon)? } else { ctx.connect(true).await? };
    let req = Request::QuickShell {
        worktree,
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
    let mut end = ptyro_tui::run(opts.clone(), &paths, tui.clone()).await;
    // Asked for the tour: it runs in a ptyro of its own, then this TUI comes back.
    while matches!(end, Ok(AttachEnd::Tour)) {
        run_tour(ctx).await?;
        end = ptyro_tui::run(
            opts.clone(),
            &paths,
            ptyro_tui::Options { welcome: false, ..tui.clone() },
        )
        .await;
    }
    match end {
        Ok(AttachEnd::Tour) => unreachable!("handled above"),
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
            return Err(Error::new(ErrorCode::NotFound, format!("no session matches {addr:?}")));
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
            age(e.created_ms),
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
    /// PowerShell (pwsh 7+ and Windows PowerShell 5.1)
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

async fn list(ctx: &Ctx, json: bool, filter: Filter) -> Result<i32, Error> {
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
            let mut all = sessions(&client).await?;
            all.retain(|s| filter.keeps(s, &projects));
            (all, worktrees)
        }
        // No local daemon simply means no sessions (`local` too: this computer's own, as a
        // Windows client names it); an unreachable remote is an error.
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
        let ports = if s.ports.is_empty() {
            String::new()
        } else {
            format!(" :{}", s.ports.iter().map(u16::to_string).collect::<Vec<_>>().join(" :"))
        };
        println!(
            "{:<8} {:<18} {:<11} {:<20} {}{:<24} {}{:>3}  {:<5} {}{ports}",
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
            age(s.created_ms),
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

pub fn age(ms: u64) -> String {
    let secs = now_ms().saturating_sub(ms) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

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
                println!("  uptime    {}", age(status.started_ms));
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
                        status.upgraded_ms.map(age).unwrap_or_default(),
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
        M::Rm { name } => {
            let said = format!("{name} is no longer mirrored here (revoke its token there too)");
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
        M::Pair { url, json } => crate::machines::pair(ctx, url, json).await,
    }
}

/// The handoff images this binary's daemon can take over from.
const HANDOFF_VERSIONS: &[u32] = ptyro_daemon::HANDOFF_VERSIONS;

#[cfg(test)]
mod path_tests {
    use super::plain_windows_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_verbatim_windows_path_is_written_the_usual_way() {
        assert_eq!(plain_windows_path(Path::new(r"\\?\C:\src")), PathBuf::from(r"C:\src"));
        assert_eq!(
            plain_windows_path(Path::new(r"\\?\UNC\nas\share\x")),
            PathBuf::from(r"\\nas\share\x")
        );
        assert_eq!(plain_windows_path(Path::new("/home/me")), PathBuf::from("/home/me"));
    }
}
