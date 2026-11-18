//! `ptyro login` and setup's logins (ROADMAP BOOT4, SPEC §24.4): one account per agent and the
//! GitHub CLI, each through the tool's own flow in this terminal (its browser page, or a link and
//! a code), confirmed by the tool's own status command. ptyro never reads, parses or stores a
//! credential: "logged in" is the status command's exit code, its output thrown away. More
//! accounts are profiles (`ptyro profile login`), which setup's last line says.

use std::sync::atomic::{AtomicBool, Ordering};

use ptyro_agents::catalog;
use ptyro_agents::setup::login::{self, LoginRow, LoginState};
use ptyro_agents::setup::{self, Host};
use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

pub struct LoginArgs {
    pub names: Vec<String>,
    pub check: bool,
    pub json: bool,
    /// The removed `--where` (hidden): refused with what to do instead.
    pub place: Option<String>,
}

pub async fn login(ctx: &Ctx, a: LoginArgs) -> Result<i32, Error> {
    crate::setup::where_removed(a.place.as_deref())?;
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
    let named = !ids.is_empty();
    let check = a.check || a.json;
    if !check && !crate::setup::interactive() {
        return Err(Error::invalid(
            "a login shows its sign-in (a browser page, or a link and a code) in the terminal: run `ptyro login` in one. `ptyro login --check` says where each stands",
        ));
    }
    let host = setup::host(&ctx.paths);
    let ok = if a.json {
        let rows: Vec<LoginRow> = login::rows(&*host, &ids);
        let ok = rows.iter().all(|r| fine(r, named));
        let text =
            serde_json::to_string_pretty(&rows).map_err(|e| Error::internal(e.to_string()))?;
        println!("{text}");
        ok
    } else if check {
        show(&login::rows(&*host, &ids), named, &[])
    } else {
        walk(&*host, &ids, named, &[])
    };
    Ok(if ok { 0 } else { 1 })
}

/// The commands `ptyro login` knows (`claude`, `opencode`, `gh`…).
fn programs() -> Vec<String> {
    catalog::items().iter().filter(|i| i.login.is_some()).map(|i| i.program.clone()).collect()
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

/// The name `ptyro install` and `ptyro login` take for it (`opencode`, `gh`).
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
                let sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
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

/// After setup's run: walk through the logins of what it installed (`fresh`) or was picked and
/// is here (`ids` holds both), when a person is at the terminal (`person`); otherwise say where
/// each stands and how to log in later. True when there were logins to speak of.
pub(crate) fn after_setup(host: &dyn Host, ids: &[String], fresh: &[String], person: bool) -> bool {
    if ids.is_empty() {
        return false;
    }
    eprintln!("\nLogins: one account each, through each tool's own sign-in.");
    if person {
        walk(host, ids, false, fresh);
    } else {
        show(&login::rows(host, ids), false, fresh);
        eprintln!("`ptyro login` logs them in, in a terminal.");
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
    // The installed binary's own name, as the self-update's session uses (`own::spawn_update`):
    // after `ptyro update ptyro`, `current_exe` can be `…/ptyro (deleted)`.
    let exe = ptyro_agents::own::installed_name(&exe);
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let c = ctx.connect(true).await?;
    let spec = SpawnSpec {
        argv: vec![exe.to_string_lossy().into_owned(), "login".into(), item.program.clone()],
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
