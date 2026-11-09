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
    /// Hidden `--installed IDS`: the names are setup's list of what to log in (not asked for by
    /// name), these just installed (Windows' ptyro running WSL's walk).
    pub installed: Option<String>,
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
    // Setup's list (from Windows' ptyro): not named, and these just installed.
    let fresh: Vec<String> = a
        .installed
        .as_deref()
        .map(|i| i.split(',').filter(|x| !x.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let named = !ids.is_empty() && a.installed.is_none();
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
            None if check => ok &= show(&login::rows(&*host, &ids), named, &fresh),
            None => ok &= walk(&*host, &ids, named, &fresh),
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
                if a.tty {
                    args.push("--tty".into());
                }
                if let Some(i) = &a.installed {
                    args.push(format!("--installed={i}"));
                }
                args.extend(ids.iter().cloned());
                // Ctrl+C there skips that login (WSL's ptyro handles it); it never ends this one.
                ok &= interrupts_skip(|| wsl_setup::interact(&d, &args))
                    .map_err(|e| Error::new(ErrorCode::Unsupported, e))?
                    == 0;
            }
        }
    }
    if a.json {
        ok = rows.iter().all(|r| fine(r, named));
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
    let place = crate::setup::place_of(given, platform, "login")?;
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

/// Nothing with a login is installed (and nothing was named or just installed): say how to
/// get them.
fn none_here(rows: &[LoginRow], named: bool, fresh: &[String]) {
    if !named && fresh.is_empty() && rows.iter().all(|r| r.state == LoginState::Missing) {
        eprintln!("No agent or GitHub CLI is installed yet: `ptyro setup` installs them.");
    }
}

/// Installed just now, but not found from here yet (an installer that changed `PATH` for new
/// terminals): say so, rather than offering to install it again.
fn not_found_yet(r: &LoginRow) {
    eprintln!(
        "– {}: installed, but this terminal can't find it yet: open a new terminal, then `ptyro login {}`",
        r.name,
        program(&r.id)
    );
}

/// `--check`: a line each (what isn't installed only when named). True when each is fine.
/// `fresh`: just installed by setup.
fn show(rows: &[LoginRow], named: bool, fresh: &[String]) -> bool {
    none_here(rows, named, fresh);
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
            LoginState::Missing if fresh.contains(&r.id) => not_found_yet(r),
            LoginState::Missing if named => {
                eprintln!("– {}: not installed: `ptyro install {}`", r.name, program(&r.id))
            }
            LoginState::Missing => {}
        }
    }
    rows.iter().all(|r| fine(r, named))
}

/// Log in each of `ids` (every one with a login when empty) that isn't yet, one after another,
/// in this terminal. `named`: asked for by name, so one that can't be checked is run too, and
/// one not installed says so. `fresh`: just installed by setup: one that can't be checked runs
/// too (it can't have signed in yet), and one not found yet needs a new terminal. One that can't
/// be checked and was here already isn't started again on every setup. True when each ends
/// logged in (or can't be checked, or was skipped).
pub(crate) fn walk(host: &dyn Host, ids: &[String], named: bool, fresh: &[String]) -> bool {
    let mut ok = true;
    let rows = login::rows(host, ids);
    none_here(&rows, named, fresh);
    for r in rows {
        let run = match r.state {
            LoginState::LoggedIn => {
                eprintln!("✓ {}: logged in", r.name);
                false
            }
            LoginState::Missing => {
                if fresh.contains(&r.id) {
                    not_found_yet(&r);
                } else if named {
                    eprintln!("– {}: not installed: `ptyro install {}`", r.name, program(&r.id));
                    ok = false;
                }
                false
            }
            LoginState::LoggedOut => true,
            LoginState::Unconfirmed if named || fresh.contains(&r.id) => true,
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
        Ok(login::Outcome { state: LoginState::LoggedIn, .. }) => {
            eprintln!("✓ {}: logged in", r.name);
            true
        }
        Ok(login::Outcome { state: LoginState::Unconfirmed, ended: None }) => {
            eprintln!(
                "? {}: done. Its maker names no way to check a login, so if it isn't signed in it asks when it starts",
                r.name
            );
            true
        }
        // Quit with Ctrl+C (or however it quits): nothing says whether it signed in first.
        Ok(login::Outcome { state: LoginState::Unconfirmed, ended: Some(how) }) => {
            eprintln!(
                "– {}: skipped ({how}). If it isn't signed in, it asks when it starts; `ptyro login {}` starts it again",
                r.name,
                program(&r.id)
            );
            true
        }
        Ok(login::Outcome { ended: Some(why), .. }) => {
            eprintln!("✗ {}: not logged in: {why}", r.name);
            eprintln!("  fix: {again}");
            false
        }
        Ok(login::Outcome { ended: None, .. }) => {
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

/// After setup's run on a side (`distro`: WSL's): walk through the logins of what it installed
/// (`fresh`) or was picked and is here (`ids` holds both), when a person is at the terminal
/// (`person`); otherwise say where each stands and how to log in later. True when there were
/// logins to speak of.
pub(crate) fn after_setup(
    host: &dyn Host,
    ids: &[String],
    fresh: &[String],
    distro: Option<&str>,
    person: bool,
) -> bool {
    if ids.is_empty() {
        return false;
    }
    eprintln!("\nLogins: one account each, through each tool's own sign-in.");
    match distro {
        None if person => {
            walk(host, ids, false, fresh);
        }
        None => {
            show(&login::rows(host, ids), false, fresh);
            eprintln!("`ptyro login` logs them in, in a terminal.");
        }
        Some(d) => {
            let mut args = vec!["login".to_owned(), format!("--installed={}", fresh.join(","))];
            if person {
                // Windows' ptyro has a person here; WSL's sees its terminal through wsl.exe.
                args.push("--tty".into());
            } else {
                args.push("--check".into());
            }
            args.extend(ids.iter().cloned());
            // Ctrl+C there skips that login (WSL's ptyro handles it); it never ends this one.
            if let Err(e) = interrupts_skip(|| wsl_setup::interact(d, &args)) {
                eprintln!("✗ couldn't reach ptyro in {d} (WSL): {e}");
                eprintln!("  fix: `wsl -d {d} -- ptyro login {}`", ids.join(" "));
            } else if !person {
                eprintln!("`ptyro login --where wsl` logs them in, in a terminal.");
            }
        }
    }
    true
}

/// A session on the daemon running `ptyro login --where <place> <id>` (the web's "Log in"): the
/// tool's own sign-in in a terminal the web shows, confirmed there as in the CLI. `place` is
/// `local` (the daemon's computer) or `wsl` (a Windows daemon's WSL distro: WSL's own ptyro runs
/// it through `wsl.exe`, in the same terminal).
pub(crate) async fn spawn(
    ctx: &Ctx,
    id: &str,
    place: &str,
) -> Result<ptyro_core::session::SessionInfo, Error> {
    use ptyro_core::session::SpawnSpec;
    use ptyro_protocol::Request;
    let item = setup::resolve(id).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
    if item.login.is_none() {
        return Err(Error::invalid(format!("{} has no login", item.name)));
    }
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    // The installed binary's own name, as the self-update's session uses (`own::spawn_update`):
    // after `ptyro update ptyro`, `current_exe` can be `…/ptyro (deleted)` or `ptyro.exe.old`.
    let exe = ptyro_agents::own::installed_name(&exe);
    // Windows sets only USERPROFILE.
    let home = ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|v| std::env::var_os(v).filter(|h| !h.is_empty()))
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let c = ctx.connect(true).await?;
    let spec = SpawnSpec {
        // The side the page listed it on: the daemon machine's, or its WSL distro's.
        argv: vec![
            exe.to_string_lossy().into_owned(),
            "login".into(),
            "--where".into(),
            place.into(),
            item.program.clone(),
        ],
        cwd: home,
        // The default profile: the agents' own folders, where setup's one account each goes.
        profile: Some(ptyro_core::model::DEFAULT_PROFILE.to_owned()),
        env: crate::cli::client_env(),
        confirmed: true,
        // What a sign-in prints or echoes (a code the person pastes) never goes to disk.
        unrecorded: true,
        ..SpawnSpec::default()
    };
    crate::cli::session_of(c.request(Request::Spawn { spec }).await?)
}
