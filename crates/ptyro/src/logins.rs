//! `ptyro login` and setup's logins (ROADMAP BOOT4, SPEC §24.4): one account per agent and the
//! GitHub CLI, each through the tool's own flow in this terminal (its browser page, or a link and
//! a code), confirmed by the tool's own status command. ptyro never reads, parses or stores a
//! credential: "logged in" is the status command's exit code, its output thrown away. More
//! accounts are profiles (`ptyro profile login`), which setup's last line says.

use std::sync::atomic::{AtomicBool, Ordering};

use ptyro_agents::catalog;
use ptyro_agents::setup::login::{self, LoginRow, LoginState};
use ptyro_agents::setup::{self, Host, Where};
use ptyro_client::wsl_setup;
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

pub struct LoginArgs {
    pub names: Vec<String>,
    pub check: bool,
    pub json: bool,
    pub place: Option<String>,
    pub tty: bool,
}

pub async fn login(ctx: &Ctx, a: LoginArgs) -> Result<i32, Error> {
    let mut ids = Vec::new();
    for n in a.names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if item.login.is_none() {
            return Err(Error::invalid(format!(
                "{} has no login: ptyro logs in the agents and the GitHub CLI ({})",
                item.name,
                programs().join(", ")
            )));
        }
        if !ids.contains(&item.id) {
            ids.push(item.id.clone());
        }
    }
    let check = a.check || a.json;
    if !check && !crate::setup::interactive(a.tty) {
        return Err(Error::invalid(
            "a login shows its sign-in (a browser page, or a link and a code) in the terminal: run `ptyro login` in one. `ptyro login --check` says where each stands",
        ));
    }
    let host = setup::host(&ctx.paths);
    let mut ok = true;
    let mut rows: Vec<LoginRow> = Vec::new();
    let sides = sides(&ctx.paths, &*host, a.place.as_deref()).await?;
    let two = sides.len() > 1;
    for distro in sides {
        if two && !a.json {
            eprintln!(
                "\n── {} ──",
                distro.as_deref().map_or("Windows".into(), |d| format!("WSL ({d})"))
            );
        }
        match distro {
            None if a.json => rows.extend(login::rows(&*host, &ids)),
            None if check => ok &= show(&login::rows(&*host, &ids), !ids.is_empty()),
            None => ok &= walk(&*host, &ids, !ids.is_empty()),
            Some(d) if a.json => {
                rows.extend(
                    wsl_setup::logins(&d, &ids)
                        .map_err(|e| Error::new(ErrorCode::Unsupported, e))?,
                );
            }
            Some(d) => {
                let mut args = vec!["login".to_owned()];
                if check {
                    args.push("--check".into());
                }
                args.extend(ids.iter().cloned());
                ok &= wsl_setup::interact(&d, &args)
                    .map_err(|e| Error::new(ErrorCode::Unsupported, e))?
                    == 0;
            }
        }
    }
    if a.json {
        ok = rows.iter().all(|r| fine(r, !ids.is_empty()));
        let text =
            serde_json::to_string_pretty(&rows).map_err(|e| Error::internal(e.to_string()))?;
        println!("{text}");
    }
    Ok(if ok { 0 } else { 1 })
}

/// The commands `ptyro login` knows (`claude`, `agy`, `gh`…).
fn programs() -> Vec<String> {
    catalog::items().iter().filter(|i| i.login.is_some()).map(|i| i.program.clone()).collect()
}

/// The sides to log in on: `None` this computer, `Some` a WSL distro. On Windows by `--where`
/// (default: the daemon's side, as setup's); elsewhere this computer.
async fn sides(
    paths: &Paths,
    host: &dyn Host,
    given: Option<&str>,
) -> Result<Vec<Option<String>>, Error> {
    let platform = host.platform();
    let place = crate::setup::place_of(given, platform)?;
    if !platform.is_windows() {
        return Ok(vec![None]);
    }
    let windows = None;
    if matches!(place, Some(Where::Windows | Where::Local)) {
        return Ok(vec![windows]);
    }
    let remote = crate::winclient::resolve(None, paths, false).await;
    let prefer = remote.as_deref().and_then(ptyro_client::remote::wsl_distro).map(str::to_owned);
    let status = wsl_setup::probe(prefer.as_deref()).await;
    let wsl = status.usable().map(|d| Some(d.to_owned()));
    let place = place
        .unwrap_or_else(|| crate::setup::default_place(prefer.as_deref(), Some(&status), false));
    Ok(match (place, wsl) {
        (Where::Wsl, Some(w)) => vec![w],
        (Where::Wsl, None) => {
            return Err(Error::new(
                ErrorCode::Unsupported,
                format!("--where wsl: {}", status.words()),
            ));
        }
        (Where::Both, Some(w)) => vec![windows, w],
        (Where::Both, None) => {
            eprintln!("{}", status.words());
            vec![windows]
        }
        _ => vec![windows],
    })
}

/// A row is as it should be: logged in, or can't be checked; not installed only when it wasn't
/// asked for by name.
fn fine(r: &LoginRow, named: bool) -> bool {
    match r.state {
        LoginState::LoggedIn | LoginState::Unconfirmed => true,
        LoginState::Missing => !named,
        LoginState::LoggedOut => false,
    }
}

/// The name `ptyro install` and `ptyro login` take for it (`agy`, `gh`).
fn program(id: &str) -> String {
    catalog::item(id).map_or_else(|| id.to_owned(), |i| i.program.clone())
}

/// Nothing with a login is installed (and nothing was named): say how to get them.
fn none_here(rows: &[LoginRow], named: bool) -> bool {
    let none = !named && rows.iter().all(|r| r.state == LoginState::Missing);
    if none {
        eprintln!("No agent or GitHub CLI is installed yet: `ptyro setup` installs them.");
    }
    none
}

/// `--check`: a line each (what isn't installed only when named). True when each is fine.
fn show(rows: &[LoginRow], named: bool) -> bool {
    none_here(rows, named);
    for r in rows {
        match r.state {
            LoginState::LoggedIn => eprintln!("✓ {}: logged in", r.name),
            LoginState::LoggedOut => {
                eprintln!(
                    "✗ {}: not logged in: `ptyro login {}` (it runs `{}`)",
                    r.name,
                    program(&r.id),
                    r.command
                )
            }
            LoginState::Unconfirmed => eprintln!(
                "? {}: can't be checked (its maker names no status command); it signs in when it starts: `{}`",
                r.name, r.command
            ),
            LoginState::Missing if named => {
                eprintln!("– {}: not installed: `ptyro install {}`", r.name, program(&r.id))
            }
            LoginState::Missing => {}
        }
    }
    rows.iter().all(|r| fine(r, named))
}

/// Log in each of `ids` (every one with a login when empty) that isn't yet, one after another,
/// in this terminal. `named`: they were asked for (by name, or setup just installed them), so
/// one that can't be checked is run too, and one not installed says so. True when each ends
/// logged in (or can't be checked).
pub(crate) fn walk(host: &dyn Host, ids: &[String], named: bool) -> bool {
    let mut ok = true;
    let rows = login::rows(host, ids);
    none_here(&rows, named);
    for r in rows {
        let run = match r.state {
            LoginState::LoggedIn => {
                eprintln!("✓ {}: logged in", r.name);
                false
            }
            LoginState::Missing => {
                if named {
                    eprintln!("– {}: not installed: `ptyro install {}`", r.name, program(&r.id));
                    ok = false;
                }
                false
            }
            LoginState::LoggedOut => true,
            LoginState::Unconfirmed if named => true,
            LoginState::Unconfirmed => {
                eprintln!(
                    "? {}: can't be checked; it signs in when it starts (`ptyro login {}` starts it now)",
                    r.name,
                    program(&r.id)
                );
                false
            }
        };
        if run {
            ok &= log_in(host, &r);
        }
    }
    ok
}

/// One login, in this terminal, and how it ended.
fn log_in(host: &dyn Host, r: &LoginRow) -> bool {
    let Some(item) = catalog::item(&r.id) else { return false };
    eprintln!("\n▸ Logging in to {}: `{}`", r.name, r.command);
    eprintln!(
        "  Its own sign-in: a page in your browser, or a link and a code to enter there. ptyro never sees it. Ctrl+C skips it."
    );
    if r.state == LoginState::Unconfirmed {
        eprintln!("  This starts {}: sign in, then quit it to come back here.", r.name);
    }
    let ended = interrupts_skip(|| login::log_in(host, item));
    let again =
        format!("`ptyro login {}` tries again, or run `{}` yourself", program(&r.id), r.command);
    match ended {
        Ok(LoginState::LoggedIn) => {
            eprintln!("✓ {}: logged in", r.name);
            true
        }
        Ok(LoginState::Unconfirmed) => {
            eprintln!(
                "? {}: done. Its maker names no way to check a login, so if it isn't signed in it asks when it starts",
                r.name
            );
            true
        }
        Ok(_) => {
            let how = r.check.as_deref().map(|c| format!(" (`{c}` says so)")).unwrap_or_default();
            eprintln!("✗ {}: still not logged in{how}", r.name);
            eprintln!("  fix: {again}");
            false
        }
        Err(why) => {
            eprintln!("✗ {}: not logged in: {why}", r.name);
            eprintln!("  fix: {again}");
            false
        }
    }
}

/// A login running: Ctrl+C is for it (an agent quits with it, a login gives up), not for ptyro.
static IN_LOGIN: AtomicBool = AtomicBool::new(false);

/// Run `f` (a login in this terminal) with Ctrl+C going to it alone. Once a handler is in place
/// it stays (the platform keeps it), so outside a login Ctrl+C still ends ptyro, as before.
fn interrupts_skip<T>(f: impl FnOnce() -> T) -> T {
    static GUARD: std::sync::Once = std::sync::Once::new();
    GUARD.call_once(|| {
        let (ready, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                let _ = ready.send(());
                return;
            };
            rt.block_on(async move {
                #[cfg(unix)]
                let sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
                #[cfg(windows)]
                let sig = tokio::signal::windows::ctrl_c();
                let _ = ready.send(());
                let Ok(mut sig) = sig else { return };
                while sig.recv().await.is_some() {
                    if !IN_LOGIN.load(Ordering::SeqCst) {
                        eprintln!();
                        std::process::exit(130);
                    }
                }
            });
        });
        let _ = wait.recv();
    });
    IN_LOGIN.store(true, Ordering::SeqCst);
    let out = f();
    IN_LOGIN.store(false, Ordering::SeqCst);
    out
}

/// After setup's run on a side (`distro`: WSL's): walk through the logins of what it installed (or was picked and
/// is here), when a person is at the terminal (`person`); otherwise say where each stands and
/// how to log in later. True when there were logins to speak of.
pub(crate) fn after_setup(
    host: &dyn Host,
    ids: &[String],
    distro: Option<&str>,
    person: bool,
) -> bool {
    if ids.is_empty() {
        return false;
    }
    eprintln!("\nLogins: one account each, through each tool's own sign-in.");
    match distro {
        None if person => {
            walk(host, ids, true);
        }
        None => {
            show(&login::rows(host, ids), true);
            eprintln!("`ptyro login` logs them in, in a terminal.");
        }
        Some(d) => {
            let mut args = vec!["login".to_owned()];
            if !person {
                args.push("--check".into());
            }
            args.extend(ids.iter().cloned());
            if let Err(e) = wsl_setup::interact(d, &args) {
                eprintln!("✗ couldn't reach ptyro in {d} (WSL): {e}");
                eprintln!("  fix: `wsl -d {d} -- ptyro login {}`", ids.join(" "));
            } else if !person {
                eprintln!("`ptyro login --where wsl` logs them in, in a terminal.");
            }
        }
    }
    true
}

/// A session on the daemon running `ptyro login <id>` (the web's "Log in"): the tool's own
/// sign-in in a terminal the web shows, confirmed there as in the CLI.
pub(crate) async fn spawn(ctx: &Ctx, id: &str) -> Result<ptyro_core::session::SessionInfo, Error> {
    use ptyro_core::session::SpawnSpec;
    use ptyro_protocol::Request;
    let item = setup::resolve(id).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
    if item.login.is_none() {
        return Err(Error::invalid(format!("{} has no login", item.name)));
    }
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    // Windows sets only USERPROFILE.
    let home = ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|v| std::env::var_os(v).filter(|h| !h.is_empty()))
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let c = ctx.connect(true).await?;
    let spec = SpawnSpec {
        argv: vec![exe.to_string_lossy().into_owned(), "login".into(), item.program.clone()],
        cwd: home,
        // The default profile: the agents' own folders, where setup's one account each goes.
        profile: Some(ptyro_core::model::DEFAULT_PROFILE.to_owned()),
        env: crate::cli::client_env(),
        confirmed: true,
        ..SpawnSpec::default()
    };
    crate::cli::session_of(c.request(Request::Spawn { spec }).await?)
}
