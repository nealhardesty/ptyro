//! `ptyro setup` and `ptyro install` (ROADMAP BOOT1, SPEC §24.2): the setup checklist in a
//! terminal, and installing by name. Both use the one plan engine (`ptyro_agents::setup`) on
//! this computer, with no daemon (a fresh machine has none yet). Admin steps ask once, in this
//! terminal (sudo's own prompt; Windows' UAC from the installer); `--yes` asks nothing and
//! reports what would have needed a prompt, with its command.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};

use ptyro_agents::catalog::{self, Kind, Platform};
use ptyro_agents::own;
use ptyro_agents::setup::checklist::{self, Picks, Sides, cell, here_already, mark, rows};
use ptyro_agents::setup::{
    self, Admin, Entry, Event, Plan, Report, Side, Sided, State, Where, WslStatus,
};
use ptyro_agents::updates::{self, Outdated};
use ptyro_client::wsl_setup;
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

pub struct SetupArgs {
    pub yes: bool,
    pub agents: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub dry_run: bool,
    pub json: bool,
    /// `--where` (BOOT8): on Windows `wsl`, `windows` or `both`.
    pub place: Option<String>,
    /// Hidden: print what is installed as JSON (Windows' ptyro asks a WSL distro's this way).
    pub survey: bool,
    /// Hidden: a person answers on stdin, terminal or not (the TUI's split in WSL running
    /// Windows' ptyro.exe through interop, whose console is a pipe).
    pub tty: bool,
}

pub struct InstallArgs {
    pub names: Vec<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
    pub place: Option<String>,
    /// Hidden `--admin ask|silent|never`: Windows' ptyro running this in a WSL distro, the person having
    /// said yes there already. `ask`: sudo may prompt in the terminal; `never`: for the web.
    pub admin: Option<String>,
    pub tty: bool,
    /// Hidden `--confirmed`: the TUI's checklist asked already (BOOT2), so no `Install?`; the
    /// rest (sudo's prompt, the logins, the profiles line) is as when typed.
    pub confirmed: bool,
}

/// A person can answer: a terminal (or a test's fake machine, which pipes its answers; or
/// `--tty`, said by the TUI).
pub(crate) fn interactive(tty: bool) -> bool {
    tty || (std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
        || ptyro_core::test_knob("PTYRO_TEST_SETUP").is_some()
}

/// `--where`, checked against this machine: on Windows any of them; elsewhere only `local`.
/// `cmd` is the command it's for (`setup`, `login`), for the words of a refusal.
pub(crate) fn place_of(
    given: Option<&str>,
    platform: Platform,
    cmd: &str,
) -> Result<Option<Where>, Error> {
    let Some(w) = given else { return Ok(None) };
    let w: Where = w.parse().map_err(Error::invalid)?;
    if platform.is_windows() || w == Where::Local {
        return Ok(Some(w));
    }
    let (here, sides) = if cmd == "login" {
        (
            "`ptyro login` here logs in to it",
            "an agent can be logged in on WSL's side, Windows', or both",
        )
    } else {
        ("setup here installs into it", "an agent can go into WSL, onto Windows, or both")
    };
    Err(Error::invalid(if platform == Platform::Wsl {
        format!(
            "--where {} is for Windows' ptyro: this is WSL, and {here}. For Windows itself, run `ptyro {cmd} --where windows` in a Windows terminal",
            w.name()
        )
    } else {
        format!(
            "--where {} is for Windows, where {sides}; on {} everything is on this computer: leave --where out",
            w.name(),
            platform.name()
        )
    }))
}

/// Look at this computer, and on Windows at WSL (unless only Windows is wanted). The default
/// place is the side the user's daemon runs on: WSL's when `client.default_remote` is a
/// `wsl://` one and WSL is usable, else this computer.
async fn look(
    paths: &Paths,
    given: Option<&str>,
    quiet: bool,
) -> Result<(Box<dyn setup::Host + Send>, Sides, Where), Error> {
    if !quiet {
        eprintln!("Looking at what's installed…");
    }
    let host = setup::host(paths);
    // The remembered newest versions (BOOT5): an outdated agent shows so, and can be ticked;
    // one installed another way is offered the move unless `updates.keep_installer` keeps it
    // (BOOT7).
    let here = updates::survey(&*host, &paths.state, &updates_config(paths));
    let platform = here.platform;
    let place = place_of(given, platform, "setup")?;
    if !platform.is_windows() {
        let status = (platform == Platform::Wsl)
            .then(|| WslStatus::Inside { distro: std::env::var("WSL_DISTRO_NAME").ok() });
        let sides = Sides { here, side: Side::Local, wsl: None, status };
        return Ok((host, sides, Where::Local));
    }
    let remote = crate::winclient::resolve(None, paths, false).await;
    let prefer = remote.as_deref().and_then(ptyro_client::remote::wsl_distro).map(str::to_owned);
    let mut sides = Sides { here, side: Side::Windows, wsl: None, status: None };
    if !matches!(place, Some(Where::Windows | Where::Local)) {
        if !quiet {
            eprintln!("Looking at WSL…");
        }
        let mut status = wsl_setup::probe(prefer.as_deref()).await;
        if let Some(d) = status.usable().map(str::to_owned) {
            match wsl_setup::survey(&d).await {
                Ok(s) => sides.wsl = Some(s),
                Err(why) => status = why,
            }
        }
        sides.status = Some(status);
    }
    let place = match place {
        Some(Where::Wsl) if sides.wsl.is_none() => {
            let why = match &sides.status {
                Some(WslStatus::Unusable { reason, fix }) => format!("{reason}: {fix}"),
                _ => "WSL isn't usable here".to_owned(),
            };
            return Err(Error::new(ErrorCode::Unsupported, format!("--where wsl: {why}")));
        }
        Some(Where::Both) if sides.wsl.is_none() => {
            if let Some(s) = &sides.status {
                eprintln!("{}", s.words());
            }
            Where::Windows
        }
        Some(p) => p,
        None => default_place(prefer.as_deref(), sides.status.as_ref(), quiet),
    };
    Ok((host, sides, place))
}

/// The default side on Windows ([`checklist::default_place`]), saying why it isn't the
/// daemon's distro when it isn't (unless `quiet`).
pub(crate) fn default_place(
    prefer: Option<&str>,
    status: Option<&WslStatus>,
    quiet: bool,
) -> Where {
    let (place, why) = checklist::default_place(prefer, status);
    if let Some(why) = why.filter(|w| !quiet && !w.is_empty()) {
        eprintln!("{why}");
    }
    place
}

/// The ids `names` mean, each of `kind`; `none` is none.
fn ids_of(names: &[String], kind: Kind, flag: &str) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for n in names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        if n == "none" {
            continue;
        }
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if item.kind != kind {
            let other = if kind == Kind::Agent { "--tools" } else { "--agents" };
            return Err(Error::invalid(format!(
                "{} isn't for {flag}: put it in {other}",
                item.name
            )));
        }
        if !out.contains(&item.id) {
            out.push(item.id.clone());
        }
    }
    Ok(out)
}

pub async fn setup(ctx: &Ctx, a: SetupArgs) -> Result<i32, Error> {
    if a.survey {
        let host = setup::host(&ctx.paths);
        let survey = updates::survey(&*host, &ctx.paths.state, &updates_config(&ctx.paths));
        let text = serde_json::to_string(&survey).map_err(|e| Error::internal(e.to_string()))?;
        println!("{text}");
        return Ok(0);
    }
    let named = a.agents.is_some() || a.tools.is_some();
    let mut ids = ids_of(a.agents.as_deref().unwrap_or_default(), Kind::Agent, "--agents")?;
    ids.extend(ids_of(a.tools.as_deref().unwrap_or_default(), Kind::Tool, "--tools")?);
    if !a.yes && !a.dry_run && !interactive(a.tty) {
        return Err(Error::invalid(
            "`ptyro setup` is a checklist: run it in a terminal, or pass --yes (with --agents and --tools, or the defaults: Claude Code, Codex, git, GitHub CLI)",
        ));
    }
    // Already running (the web's, another terminal's, the TUI's): say so before the checklist,
    // and hold the machine's lock from here, so nothing starts while this one is asked.
    let lock = if a.dry_run { None } else { Some(take_lock(ctx, "`ptyro setup`")?) };
    let (host, sides, place) = look(&ctx.paths, a.place.as_deref(), false).await?;
    let mut picks = if named { sides.picks(place, &ids) } else { sides.defaults(place) };
    if a.dry_run {
        return show_plan(&sides.plan(&picks), a.json);
    }
    if !a.yes {
        let stdin = std::io::stdin();
        match checklist(&sides, place, picks, &mut stdin.lock(), &mut std::io::stderr()) {
            Some(p) => picks = p,
            None => {
                eprintln!("Nothing installed.");
                return Ok(0);
            }
        }
    }
    let plan = sides.plan(&picks);
    let admin = if a.yes { Admin::Silent } else { Admin::Ask };
    go(ctx, &*host, &sides, &plan, admin, a.json, false, lock, "`ptyro setup`")
}

pub async fn install(ctx: &Ctx, a: InstallArgs) -> Result<i32, Error> {
    let mut ids = Vec::new();
    for n in a.names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if !ids.contains(&item.id) {
            ids.push(item.id.clone());
        }
    }
    if ids.is_empty() {
        return Err(Error::invalid("name what to install: `ptyro install agy`"));
    }
    // Asked already, by Windows' ptyro (BOOT8): run, and ask only for administrator rights.
    let asked = match a.admin.as_deref() {
        None => None,
        Some("ask") => Some(Admin::Ask),
        Some("silent") => Some(Admin::Silent),
        Some("never") => Some(Admin::Never),
        Some(other) => {
            return Err(Error::invalid(format!(
                "--admin takes ask, silent or never, not {other:?}"
            )));
        }
    };
    if !a.yes && !a.dry_run && asked.is_none() && !interactive(a.tty) {
        return Err(Error::invalid(
            "`ptyro install` asks before it installs: run it in a terminal, or pass --yes",
        ));
    }
    let quiet = asked.is_some();
    let who = if quiet {
        "`ptyro install`, for Windows' ptyro"
    } else if a.confirmed {
        "the TUI's setup checklist"
    } else {
        "`ptyro install`"
    };
    // Already running: say so before asking anything, and hold the lock from here.
    let lock = if a.dry_run { None } else { Some(take_lock(ctx, who)?) };
    let (host, mut sides, place) = look(&ctx.paths, a.place.as_deref(), quiet).await?;
    let mut plan = sides.plan(&sides.picks(place, &ids));
    if a.dry_run {
        return show_plan(&plan, a.json);
    }
    let two = plan.sides().len() > 1;
    if !a.yes && asked.is_none() && !a.confirmed && !plan.steps().is_empty() {
        eprintln!();
        for e in plan.steps() {
            eprintln!("  {}", step_line(e, two));
        }
        // A move to the recommended installer (BOOT7) can be declined for good: `k` keeps
        // those the way they're installed (this computer's own; WSL's keeps its own).
        let moving: Vec<&Entry> = plan
            .steps()
            .into_iter()
            .filter(|e| e.side != Side::Wsl && e.action.as_ref().is_some_and(|a| a.moving))
            .collect();
        let q = if moving.is_empty() {
            "Install? [Y/n] ".to_owned()
        } else {
            let names: Vec<&str> = moving.iter().map(|e| e.name.as_str()).collect();
            let how: Vec<&str> =
                moving.iter().filter_map(|e| Some(e.moves.as_ref()?.from.words())).collect();
            format!(
                "Install? [Y/n, or k to keep {} with {} and not be asked again] ",
                names.join(" and "),
                how.first().copied().unwrap_or("its installer")
            )
        };
        match answer(&q) {
            Answer::Yes => {}
            Answer::No => {
                eprintln!("Nothing installed.");
                return Ok(0);
            }
            Answer::Keep if !moving.is_empty() => {
                let kept: Vec<String> = moving.iter().map(|e| e.id.clone()).collect();
                let names: Vec<String> = moving.iter().map(|e| e.name.clone()).collect();
                match keep_installer(&ctx.paths, &kept) {
                    Ok(()) => eprintln!(
                        "Kept {} the way {} installed: {} updates that way, and the move isn't offered again (updates.keep_installer).",
                        names.join(" and "),
                        if kept.len() == 1 { "it's" } else { "they're" },
                        if kept.len() == 1 { "it" } else { "each" }
                    ),
                    Err(e) => eprintln!("Couldn't save updates.keep_installer ({e}): kept for now"),
                }
                // Not run this time either: kept is a no to the move, not a yes to an update.
                let rest: Vec<String> = ids.iter().filter(|i| !kept.contains(i)).cloned().collect();
                sides.here.keep.extend(kept);
                let mut picks = sides.picks(place, &rest);
                picks.wsl = sides.picks(place, &ids).wsl;
                plan = sides.plan(&picks);
                if plan.steps().is_empty() {
                    eprintln!("Nothing else to install.");
                    return Ok(0);
                }
            }
            Answer::Keep => {
                eprintln!("Nothing installed.");
                return Ok(0);
            }
        }
    }
    let admin = asked.unwrap_or(if a.yes { Admin::Silent } else { Admin::Ask });
    go(ctx, &*host, &sides, &plan, admin, a.json, quiet, lock, who)
}

/// `ids` (agents installed another way) kept that way from now on (BOOT7): added to this
/// computer's `updates.keep_installer`, so the move isn't offered again and they update the way
/// they're installed.
pub(crate) fn keep_installer(paths: &Paths, ids: &[String]) -> Result<(), String> {
    let mut list = updates_config(paths).keep_installer;
    for id in ids {
        if !updates::kept(&ptyro_core::config::Updates {
            keep_installer: list.clone(),
            ..Default::default()
        })
        .contains(id)
        {
            list.push(id.clone());
        }
    }
    let value =
        format!("[{}]", list.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(", "));
    crate::config::set_user(paths, "updates.keep_installer", &value)
}

fn updates_config(paths: &Paths) -> ptyro_core::config::Updates {
    ptyro_core::config::Config::load(&ptyro_core::config::Config::path(paths))
        .map(|c| c.updates)
        .unwrap_or_default()
}

pub struct UpdateArgs {
    pub names: Vec<String>,
    pub check: bool,
    pub yes: bool,
    pub json: bool,
    pub tty: bool,
}

/// `ptyro update [NAME…] [--check]` (BOOT5, SPEC §24.7): look for newer versions afresh, then
/// update the agents that have one (those named, or every one `updates.ignore` doesn't skip),
/// each through the plan engine with the method it was installed by, under the machine's setup
/// lock, after a yes. Sessions running an updated agent keep the old version until they restart,
/// and are named; on Windows, an agent that can't be replaced while it runs is offered a retry.
/// ptyro itself is one more row (BOOT6, SPEC §24.8): its release downloaded, checked and swapped
/// in last, then the daemon upgraded live, and on Windows with WSL the other half brought to the
/// same version.
pub async fn update(ctx: &Ctx, a: UpdateArgs) -> Result<i32, Error> {
    let cfg = updates_config(&ctx.paths);
    let mut ids: Vec<String> = Vec::new();
    for n in a.names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        if n.eq_ignore_ascii_case(own::ID) {
            if !ids.iter().any(|i| i == own::ID) {
                ids.push(own::ID.to_owned());
            }
            continue;
        }
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if item.kind != Kind::Agent {
            return Err(Error::invalid(format!(
                "ptyro updates the agents; {} updates the way you installed it (its package manager), as before",
                item.name
            )));
        }
        if !ids.contains(&item.id) {
            ids.push(item.id.clone());
        }
    }
    let state = ctx.paths.state.clone();
    if a.check {
        if !a.json {
            eprintln!("Looking for newer versions…");
        }
        let host = setup::host(&ctx.paths);
        // Beside any install: one running now leaves what was remembered (said so), and one that
        // starts while this looks keeps what it finds (`updates::look_beside`).
        let busy = setup::lock::Lock::held_by(&state);
        let (v, survey) = updates::look_beside(&*host, &state, updates::now(), true);
        if let Some(by) = busy.filter(|_| !a.json) {
            eprintln!(
                "Setup is running (from {by}): this is what was found before; look again once it's done."
            );
        }
        if a.json {
            let out: Vec<&Outdated> =
                v.outdated.iter().filter(|o| ids.is_empty() || ids.contains(&o.id)).collect();
            let doc = serde_json::json!({
                "outdated": out,
                "ignored": out.iter().filter(|o| updates::ignored(&cfg.ignore, o)).map(|o| &o.id).collect::<Vec<_>>(),
                "failed": v.failed,
                "checked": v.checked,
            });
            println!("{doc}");
            return Ok(0);
        }
        say_where_each_stands(&v, &survey, &cfg, &ids);
        return Ok(0);
    }
    if !a.yes && !interactive(a.tty) {
        return Err(Error::invalid(
            "`ptyro update` asks before it updates: run it in a terminal, or pass --yes",
        ));
    }
    // One install or update at a time on this machine, whoever starts it: said at once when one
    // runs, before anything is looked up.
    if let Some(by) = setup::lock::Lock::held_by(&state) {
        return Err(Error::conflict(setup::lock::busy(&by)));
    }
    if !a.json {
        eprintln!("Looking for newer versions…");
    }
    let host = setup::host(&ctx.paths);
    // Looked up before the lock is taken: a slow network never holds it.
    let looked = updates::lookup(&*host);
    // ptyro itself (BOOT6): whether it will be updated (or was named), so the other half of a
    // Windows install with WSL is looked at now, before the lock (a cold distro takes seconds).
    let install = own::Install::here();
    let ptyro_named = ids.iter().any(|i| i == own::ID);
    // As `updates::remember` will see it: this look's, else the one remembered (the API may be
    // out of requests while the download still works), so the halves are decided on the same
    // view the update list is.
    let ptyro_latest = looked
        .0
        .get(own::ID)
        .cloned()
        .or_else(|| updates::load(&state).latest.get(own::ID).cloned());
    let ptyro_wanted = match own::finding(&install, ptyro_latest.as_deref()) {
        Some(o) if ids.is_empty() => !updates::ignored(&cfg.ignore, &o) && o.managed.is_none(),
        Some(_) => ptyro_named,
        None => ptyro_named && ptyro_latest.is_some(),
    };
    let half =
        if ptyro_wanted { crate::own::other_half(&ctx.paths, host.platform()).await } else { None };
    let lock = take_lock(ctx, "`ptyro update`")?;
    let since = updates::generation(&state);
    let (v, survey) = updates::remember(&*host, &state, updates::now(), since, Some(looked), true);
    // A ptyro a package manager owns is said, not updated, unless named (it then says how).
    let todo: Vec<Outdated> = v
        .outdated
        .iter()
        .filter(|o| {
            if ids.is_empty() {
                !updates::ignored(&cfg.ignore, o) && o.managed.is_none()
            } else {
                ids.contains(&o.id)
            }
        })
        .cloned()
        .collect();
    let ptyro_todo = todo.iter().find(|o| o.id == own::ID).cloned();
    if !a.json {
        for id in ids.iter().filter(|id| !todo.iter().any(|o| &o.id == *id)) {
            eprintln!("{}", standing(id, &v, &survey));
        }
    }
    if todo.is_empty() {
        if ids.is_empty() && !a.json {
            say_where_each_stands(&v, &survey, &cfg, &[]);
        }
        drop(lock);
        if let (Some(h), Some(now)) = (&half, own::version_of(&install.path)) {
            return Ok(if crate::own::keep_in_step(h, &now, a.json).await { 0 } else { 1 });
        }
        return Ok(0);
    }
    let todo_ids: Vec<String> = todo.iter().map(|o| o.id.clone()).collect();
    let running = sessions_running(ctx, &todo_ids).await;
    if !a.yes {
        eprintln!();
        for o in todo.iter().filter(|o| o.id == own::ID) {
            for l in crate::own::question(o, &install) {
                eprintln!("{l}");
            }
            if let Some(h) = half.as_ref().filter(|h| h.version.as_deref() != Some(&o.latest)) {
                eprintln!(
                    "    and {} {} too, so both halves agree",
                    h.label,
                    h.version
                        .as_deref()
                        .map_or("(version unknown)".into(), |v| format!("{v} → {}", o.latest))
                );
            }
        }
        for o in todo.iter().filter(|o| o.id != own::ID) {
            let how = o.command.as_deref().map(|c| format!(": `{c}`")).unwrap_or_default();
            let admin = if o.admin { " (as administrator)" } else { "" };
            eprintln!("  {} {} → {}{how}{admin}", o.name, o.version, o.latest);
            if let Some(names) = running.get(&o.id) {
                eprintln!(
                    "    {} {} {} until restarted: {}",
                    still_run(names.len()),
                    o.name,
                    o.version,
                    names.join(", ")
                );
            }
        }
        if !confirm("Update? [Y/n] ") {
            eprintln!("Nothing updated.");
            return Ok(0);
        }
    }
    let admin = if a.yes { Admin::Silent } else { Admin::Ask };
    let windows = survey.platform.is_windows();
    let names: BTreeMap<String, String> =
        todo.iter().map(|o| (o.id.clone(), o.name.clone())).collect();
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
    // An update never moves (BOOT7): each agent updates the way it's installed here; the move is
    // `ptyro install`'s, offered beside the finding.
    let never_move = |s: &setup::Survey| {
        let mut s = s.clone();
        s.keep.extend(
            catalog::items().iter().filter(|i| i.kind == Kind::Agent).map(|i| i.id.clone()),
        );
        s
    };
    let mut plan = setup::plan(&never_move(&survey), &todo_ids);
    let mut done: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let agents = todo.iter().any(|o| o.id != own::ID);
    loop {
        if !agents {
            break;
        }
        let mut busy: Vec<String> = Vec::new();
        let mut on = |ev: Event| {
            if let Event::Failed { id, in_use: true, .. } = &ev {
                busy.push(id.clone());
            }
            if a.json {
                if let Ok(line) = serde_json::to_string(&ev) {
                    println!("{line}");
                }
                return;
            }
            match &ev {
                Event::Done { id, version } => eprintln!(
                    "✓ {} {}",
                    name(id),
                    version
                        .as_deref()
                        .unwrap_or("(updated; a new terminal may be needed to find it)")
                ),
                Event::Finished { .. } | Event::Logins { .. } => {}
                _ => print_event(&ev, &name, windows),
            }
        };
        let report = setup::run(&plan, &*host, admin, &mut on);
        done.extend(report.done.iter().cloned());
        failed.retain(|f| !report.done.contains(f));
        for f in report.failed.iter().chain(&report.skipped).chain(&report.blocked) {
            if !failed.contains(f) {
                failed.push(f.clone());
            }
        }
        // Windows won't replace a program while it runs: say which sessions run it, and
        // offer to try again once they're closed (a person at the terminal only).
        if !(windows && admin == Admin::Ask && !a.json && !busy.is_empty()) {
            break;
        }
        let now_running = sessions_running(ctx, &busy).await;
        for id in &busy {
            let who = now_running
                .get(id)
                .map(|n| format!(" ({}: {})", sessions(n.len()), n.join(", ")))
                .unwrap_or_default();
            eprintln!(
                "\n{} is still running{who}, and Windows won't replace a program while it runs.",
                name(id)
            );
        }
        eprintln!("End those sessions (or quit it wherever else it runs), then try again here.");
        if !confirm("Try again now? [Y/n] ") {
            break;
        }
        // Looked at again: one that updated itself meanwhile (Claude Code and Antigravity do)
        // is done, not a failure.
        let fresh = setup::survey(&*host, &v.latest);
        let mut still: Vec<String> = Vec::new();
        for id in busy {
            let old = matches!(
                fresh.states.get(&id),
                Some(State::Outdated { .. } | State::Other { latest: Some(_), .. })
            );
            if old {
                still.push(id);
            } else {
                eprintln!("✓ {} is up to date now", name(&id));
                failed.retain(|f| f != &id);
                done.push(id);
            }
        }
        if still.is_empty() {
            break;
        }
        plan = setup::plan(&never_move(&fresh), &still);
    }
    // ptyro itself last (BOOT6), still under the lock: its release, checked and swapped in.
    let mut ptyro_new: Option<String> = None;
    if let Some(o) = &ptyro_todo {
        let on = |ev: Event| {
            if a.json {
                if let Ok(line) = serde_json::to_string(&ev) {
                    println!("{line}");
                }
            } else {
                print_event(&ev, &name, false);
            }
        };
        // Off the runtime's thread, its progress shown as it comes.
        let (install, latest) = (install.clone(), o.latest.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let job = tokio::task::spawn_blocking(move || {
            own::update(&install, &latest, admin, &mut |ev| {
                let _ = tx.send(ev);
            })
        });
        while let Some(ev) = rx.recv().await {
            on(ev);
        }
        let r = job.await.map_err(|e| Error::internal(e.to_string()))?;
        match r {
            Ok(v) => {
                done.push(own::ID.to_owned());
                ptyro_new = Some(v);
            }
            Err(_) => failed.push(own::ID.to_owned()),
        }
    }
    let after = updates::refresh(&*host, &state, updates::now());
    drop(lock);
    // Then the daemon onto it, live, and the other half to the same version (also when ptyro
    // was named and is the newest already).
    if let Some(v) = &ptyro_new
        && !crate::own::upgrade_daemon(ctx, install.after(), v).await
    {
        failed.push("daemon".into());
    }
    let ptyro_now = match (&ptyro_new, &ptyro_todo) {
        (Some(v), _) => Some(v.clone()),
        (None, None) if ptyro_named => own::version_of(&install.path),
        _ => None,
    };
    if let (Some(h), Some(v)) = (&half, &ptyro_now)
        && !crate::own::keep_in_step(h, v, a.json).await
    {
        failed.push("the other half".into());
    }
    if !a.json {
        for id in &done {
            if let (Some(o), Some(n)) = (todo.iter().find(|o| &o.id == id), running.get(id)) {
                eprintln!(
                    "{} {} {} until restarted: {}",
                    still_run(n.len()),
                    o.name,
                    o.version,
                    n.join(", ")
                );
            }
        }
        let mut parts = vec![format!("{} updated", done.len())];
        if !failed.is_empty() {
            parts.push(format!("{} not updated", failed.len()));
        }
        let tail = if failed.is_empty() { "." } else { ": each says its fix above." };
        eprintln!("\n{}{tail}", parts.join(", "));
        let left: Vec<Outdated> = updates::offered(&after, &cfg)
            .into_iter()
            .filter(|o| !todo.iter().any(|t| t.id == o.id))
            .collect();
        if !left.is_empty() {
            eprintln!(
                "Also out of date: {}",
                left.iter().map(|o| o.line()).collect::<Vec<_>>().join("; ")
            );
        }
    }
    Ok(if failed.is_empty() { 0 } else { 1 })
}

/// `1 session`, `2 sessions`.
fn sessions(n: usize) -> String {
    if n == 1 { "1 session".into() } else { format!("{n} sessions") }
}

/// `1 session still runs`, `2 sessions still run`.
fn still_run(n: usize) -> String {
    if n == 1 { "1 session still runs".into() } else { format!("{n} sessions still run") }
}

/// A named agent with nothing to update: the newest already, not installed, or not known.
fn standing(id: &str, v: &updates::Versions, survey: &setup::Survey) -> String {
    if id == own::ID {
        return own_standing(v);
    }
    let name = catalog::item(id).map_or(id, |i| i.name.as_str());
    let program = catalog::item(id).map_or(id, |i| i.program.as_str());
    match (survey.states.get(id), v.latest.get(id)) {
        (None | Some(State::Missing), _) => {
            format!("{name} isn't installed: `ptyro install {program}` installs it")
        }
        (Some(s), None) => format!(
            "? {name} {}: its newest version couldn't be looked up ({})",
            s.version().unwrap_or("(version unknown)"),
            v.failed.get(id).map_or("no source", String::as_str)
        ),
        (Some(s), Some(l)) => {
            format!("✓ {name} {} is the newest ({l})", s.version().unwrap_or("?"))
        }
    }
}

/// Where ptyro itself stands (BOOT6): the newest, a build, or not looked up.
fn own_standing(v: &updates::Versions) -> String {
    let install = own::Install::here();
    let have = own::version_of(&install.path).unwrap_or_else(|| "(version unknown)".into());
    if install.how == own::How::Build {
        return format!(
            "· ptyro {have} is a build ({}): it updates by building again",
            install.path.display()
        );
    }
    match (v.outdated.iter().find(|o| o.id == own::ID), v.latest.get(own::ID)) {
        (Some(o), _) => format!("! {}", o.finding()),
        (None, Some(l)) => format!("✓ ptyro {have} is the newest ({l})"),
        (None, None) => format!(
            "? ptyro {have}: its newest version couldn't be looked up ({})",
            v.failed.get(own::ID).map_or("no source", String::as_str)
        ),
    }
}

/// `--check`, or nothing to update: where each installed agent stands.
fn say_where_each_stands(
    v: &updates::Versions,
    survey: &setup::Survey,
    cfg: &ptyro_core::config::Updates,
    ids: &[String],
) {
    let installed: Vec<&catalog::Item> = catalog::items()
        .iter()
        .filter(|i| i.kind == Kind::Agent && (ids.is_empty() || ids.contains(&i.id)))
        .filter(|i| survey.states.get(&i.id).is_some_and(State::present))
        .collect();
    let only_ptyro = !ids.is_empty() && ids.iter().all(|i| i == own::ID);
    if installed.is_empty() && !only_ptyro {
        eprintln!("No agent is installed: `ptyro setup` installs them.");
    }
    let mut any = false;
    for i in installed {
        match v.outdated.iter().find(|o| o.id == i.id) {
            Some(o) if updates::ignored(&cfg.ignore, o) => {
                eprintln!("· {}: skipped (updates.ignore)", o.line())
            }
            Some(o) => {
                any = true;
                eprintln!("! {}", o.finding());
            }
            None => eprintln!("{}", standing(&i.id, v, survey)),
        }
    }
    if ids.is_empty() || ids.iter().any(|i| i == own::ID) {
        let install = own::Install::here();
        if install.how != own::How::Build {
            match v.outdated.iter().find(|o| o.id == own::ID) {
                Some(o) if updates::ignored(&cfg.ignore, o) => {
                    eprintln!("· {}: skipped (updates.ignore)", o.line())
                }
                Some(o) => {
                    any = o.managed.is_none() || any;
                    eprintln!("! {}", o.finding());
                }
                None => eprintln!("{}", own_standing(v)),
            }
        }
    }
    if !v.failed.is_empty() && v.failed.len() == updates::looked_up().len() {
        eprintln!("Couldn't look for newer versions (offline?): ptyro looks again tomorrow.");
    }
    if any {
        eprintln!("`ptyro update` updates them all.");
    }
}

/// The live sessions on this computer's daemon running each agent (by id): their names. None
/// when no daemon runs.
async fn sessions_running(ctx: &Ctx, ids: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(c) = ctx.connect(false).await else { return out };
    let Ok(ptyro_protocol::Reply::Sessions { sessions }) =
        c.request(ptyro_protocol::Request::List).await
    else {
        return out;
    };
    for s in sessions.iter().filter(|s| s.machine.is_none() && s.state.is_live()) {
        let prog = s
            .argv
            .first()
            .map(|p| {
                let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
                base.strip_suffix(".exe").unwrap_or(base).to_owned()
            })
            .unwrap_or_default();
        for id in ids {
            let Some(item) = catalog::item(id) else { continue };
            if s.agent.as_deref() == Some(id.as_str()) || prog == item.program {
                out.entry(id.clone()).or_default().push(s.name.clone());
            }
        }
    }
    out
}

/// Run the plan with live output, side by side (this computer first, then WSL's), then what's
/// next. `quiet`: run for Windows' ptyro, which says the rest itself. `lock`: the machine's
/// setup lock, held since before the questions; `None`, it is taken here for `who`.
#[allow(clippy::too_many_arguments)]
fn go(
    ctx: &Ctx,
    host: &dyn setup::Host,
    sides: &Sides,
    plan: &Plan,
    admin: Admin,
    json: bool,
    quiet: bool,
    lock: Option<setup::lock::Lock>,
    who: &str,
) -> Result<i32, Error> {
    let two = plan.sides().len() > 1;
    let tag =
        |e: &Entry| if two { format!(" ({})", side_name(e.side, sides)) } else { String::new() };
    if !quiet {
        for e in
            plan.entries.iter().filter(|e| e.picked && e.action.is_none() && e.blocked.is_none())
        {
            eprintln!("✓ {}{} is here already ({})", e.name, tag(e), e.state.words());
        }
    }
    let work = |side: Side| {
        plan.side(side).entries.iter().any(|e| e.will_run() || (e.ticked && e.blocked.is_some()))
    };
    let busy: Vec<Side> = plan.sides().into_iter().filter(|s| work(*s)).collect();
    if busy.is_empty() {
        if !quiet {
            eprintln!("Nothing to install.");
            if !json {
                logins(host, sides, plan, &[], admin);
            }
        }
        return Ok(0);
    }
    // One install at a time on this machine, whoever starts it (the web's run takes it too).
    let lock = match lock {
        Some(l) => l,
        None => take_lock(ctx, who)?,
    };
    let names: BTreeMap<String, String> =
        plan.entries.iter().map(|e| (e.id.clone(), e.name.clone())).collect();
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
    let windows = plan.platform.is_windows();
    let mut total = Report::default();
    let mut reports: Vec<(Side, Report)> = Vec::new();
    let mut tools = false;
    for side in &busy {
        let side = *side;
        if busy.len() > 1 && !json {
            eprintln!(
                "
── {} ──",
                side_name(side, sides)
            );
        }
        let mut on = |ev: Event| {
            if json {
                let line = if windows {
                    serde_json::to_string(&Sided { side, event: ev })
                } else {
                    serde_json::to_string(&ev)
                };
                if let Ok(line) = line {
                    println!("{line}");
                }
                return;
            }
            print_event(&ev, &name, windows && side != Side::Wsl);
        };
        let report = match (side, sides.distro()) {
            (Side::Wsl, Some(distro)) => {
                let how = match admin {
                    Admin::Ask => wsl_setup::Admin::Ask,
                    Admin::Silent => wsl_setup::Admin::Silent,
                    Admin::Never => wsl_setup::Admin::Never,
                };
                wsl_setup::run(distro, plan, how, &mut on)
            }
            _ => {
                let r = setup::run(&plan.side(side), host, admin, &mut on);
                tools |= r.tools;
                r
            }
        };
        total.merge(&report);
        reports.push((side, report));
    }
    if !total.done.is_empty() || !total.failed.is_empty() {
        // What is out of date changed (BOOT5; a move that failed half way changed it too,
        // BOOT7): the badge and the start-up note follow. Under the lock still, so a look beside
        // it can't write over this.
        updates::refresh(host, &ctx.paths.state, updates::now());
    }
    drop(lock);
    if tools {
        offer_path(&ctx.paths, admin != Admin::Ask);
    }
    if !quiet && !json {
        logins(host, sides, plan, &reports, admin);
    }
    Ok(if total.ok() { 0 } else { 1 })
}

/// The machine's setup lock (`setup::lock`): one install at a time, whoever starts it.
fn take_lock(ctx: &Ctx, who: &str) -> Result<setup::lock::Lock, Error> {
    setup::lock::Lock::take(&ctx.paths.state, who).map_err(Error::conflict)
}

/// After the installs (BOOT4): each side's logins, walked through in this terminal when a person
/// is here (else where each stands), then how to add more accounts as profiles. The exit code
/// stays the installs'.
fn logins(
    host: &dyn setup::Host,
    sides: &Sides,
    plan: &Plan,
    reports: &[(Side, Report)],
    admin: Admin,
) {
    let none = Report::default();
    let mut any = false;
    for side in plan.sides() {
        let report = reports.iter().find(|(s, _)| *s == side).map_or(&none, |(_, r)| r);
        let ids = setup::login::after(&plan.side(side), report);
        let distro = if side == Side::Wsl { sides.distro() } else { None };
        if side == Side::Wsl && distro.is_none() {
            continue;
        }
        if plan.sides().len() > 1 && !ids.is_empty() {
            eprintln!("\n── {} ──", side_name(side, sides));
        }
        any |= crate::logins::after_setup(host, &ids, &report.done, distro, admin == Admin::Ask);
    }
    if any {
        eprintln!("\n{}", setup::login::PROFILES_HINT);
    }
}

/// `Windows`, `WSL (Ubuntu)`.
fn side_name(side: Side, sides: &Sides) -> String {
    sides.name(side)
}

fn print_event(ev: &Event, name: &dyn Fn(&str) -> String, windows: bool) {
    let list = |ids: &[String]| ids.iter().map(|i| name(i)).collect::<Vec<_>>().join(", ");
    match ev {
        Event::Elevating { ids } if windows => eprintln!(
            "\n{} need administrator rights: Windows asks (a UAC prompt) as each installer starts.",
            list(ids)
        ),
        Event::Elevating { ids } => eprintln!(
            "\n{} need administrator rights: sudo asks for your password once, here.",
            list(ids)
        ),
        Event::Started { name, command, admin, .. } => {
            let how = if *admin { " (as administrator)" } else { "" };
            eprintln!("\n▸ {name}{how}");
            for l in command.lines() {
                eprintln!("  $ {}", l.trim());
            }
        }
        Event::Line { text, .. } => eprintln!("  │ {text}"),
        Event::Done { id, version: Some(v) } => eprintln!("✓ {} {v}", name(id)),
        Event::Done { id, version: None } => {
            eprintln!("✓ {} (installed; a new terminal may be needed to find it)", name(id))
        }
        Event::Failed { id, error, fix, .. } => {
            eprintln!("✗ {}: {error}", name(id));
            eprintln!("  fix: {fix}");
        }
        Event::Blocked { id, reason, fix } => {
            eprintln!("✗ {}: can't be installed here: {reason}", name(id));
            eprintln!("  fix: {fix}");
        }
        Event::Skipped { id, reason, fix } => {
            eprintln!("– {}: not installed, {reason}", name(id));
            eprintln!("  fix: {fix}");
        }
        // The logins are walked through after every side's run (`logins`).
        Event::Logins { .. } => {}
        Event::Finished { report } => eprintln!("\n{}", summary(report)),
    }
}

fn summary(r: &Report) -> String {
    let mut parts = vec![format!("{} installed", r.done.len())];
    if !r.failed.is_empty() {
        parts.push(format!("{} failed", r.failed.len()));
    }
    if !r.skipped.is_empty() {
        parts.push(format!("{} not run", r.skipped.len()));
    }
    if !r.blocked.is_empty() {
        parts.push(format!("{} can't be installed here", r.blocked.len()));
    }
    let tail = if r.ok() { "." } else { ": each says its fix above." };
    format!("{}{tail}", parts.join(", "))
}

enum Answer {
    Yes,
    No,
    /// `k`: keep it the way it's installed (BOOT7's move declined for good).
    Keep,
}

/// A `[Y/n/k]` question; nothing typed is yes, end of input is no.
fn answer(q: &str) -> Answer {
    eprint!("{q}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => Answer::No,
        Ok(_) => match s.trim() {
            "" | "y" | "Y" | "yes" => Answer::Yes,
            "k" | "K" | "keep" => Answer::Keep,
            _ => Answer::No,
        },
    }
}

/// A `[Y/n]` question; nothing typed is yes, end of input is no.
fn confirm(q: &str) -> bool {
    eprint!("{q}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(s.trim(), "" | "y" | "Y" | "yes"),
    }
}

/// New terminals need ptyro's tools folders on `PATH`: through `ptyro shell-init`'s line in the
/// shell's rc file (it adds them first), or on Windows the user's `PATH`. Asked; `--yes` only
/// says how, as `ptyro init --yes` leaves rc files alone.
fn offer_path(paths: &Paths, yes: bool) {
    let dirs = setup::path_dirs(paths);
    let shown: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    eprintln!(
        "\nSome of this went into ptyro's tools folder ({}). ptyro's own sessions find it.",
        paths.tools().display()
    );
    if cfg!(windows) {
        if yes || !confirm("Add its folders to your PATH, for new terminals? [Y/n] ") {
            eprintln!("For other terminals, add these to your PATH: {}", shown.join("; "));
            return;
        }
        match add_user_path(&dirs) {
            Ok(()) => eprintln!("Added to your PATH: new terminals find them."),
            Err(e) => {
                eprintln!("Couldn't change your PATH ({e}): add {} yourself", shown.join("; "))
            }
        }
        return;
    }
    let shell = std::env::var("SHELL").unwrap_or_default();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let (Some(kind), Some(home)) = (ptyro_core::shell::ShellKind::of_program(&shell), home) else {
        eprintln!("For other terminals, add these to your PATH: {}", shown.join(":"));
        return;
    };
    let (rc, line) = kind.rc_line(&home);
    let text = std::fs::read_to_string(&rc).unwrap_or_default();
    if text.contains("ptyro shell-init") {
        eprintln!("{} loads ptyro's line, which adds them: open a new terminal.", rc.display());
        return;
    }
    let q = format!("Add `{line}` to {}? New terminals then find them. [Y/n] ", rc.display());
    if yes || !confirm(&q) {
        eprintln!("For other terminals, add this line to {}: {line}", rc.display());
        return;
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    let add =
        format!("{sep}\n# ptyro shell integration, and the tools ptyro setup installed\n{line}\n");
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc)
        .and_then(|mut f| f.write_all(add.as_bytes()));
    match r {
        Ok(()) => eprintln!("Added to {}: open a new terminal.", rc.display()),
        Err(e) => eprintln!("Couldn't write {} ({e}): add `{line}` yourself", rc.display()),
    }
}

/// Windows: `dirs` at the front of the user's `PATH` (HKCU\Environment), each once. Read and
/// written as stored: `%VAR%` entries stay unexpanded and REG_EXPAND_SZ stays so (PowerShell's
/// `[Environment]::SetEnvironmentVariable` would expand them for good and write REG_SZ).
#[cfg(windows)]
fn add_user_path(dirs: &[std::path::PathBuf]) -> Result<(), String> {
    use ptyro_pty::winreg::{Raw, announce_environment, get_raw, set_raw};
    let cur =
        get_raw(false, "Environment", "Path").unwrap_or(Raw { text: String::new(), expand: true });
    let mine: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    let next = setup::prepend_entries(&cur.text, &mine);
    if next != cur.text {
        // A Path that held only plain folders may stay REG_SZ; one with `%VAR%` must expand.
        set_raw("Environment", "Path", &next, cur.expand || next.contains('%'))
            .map_err(|e| e.to_string())?;
        announce_environment();
    }
    Ok(())
}

#[cfg(not(windows))]
fn add_user_path(_dirs: &[std::path::PathBuf]) -> Result<(), String> {
    Err("only on Windows".into())
}

/// `--dry-run`: the checklist and what would run.
fn show_plan(plan: &Plan, json: bool) -> Result<i32, Error> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(plan).map_err(|e| Error::internal(e.to_string()))?
        );
        return Ok(0);
    }
    let mut out = std::io::stdout();
    let _ = render(plan, &mut out);
    let steps = plan.steps();
    let two = plan.sides().len() > 1;
    if steps.is_empty() {
        let _ = writeln!(out, "\nNothing to install.");
    } else {
        let _ = writeln!(out, "\nWhat would run, in order:");
        for (e, a) in steps.iter().filter_map(|e| Some((e, e.action.as_ref()?))) {
            let admin = if a.admin { " (as administrator)" } else { "" };
            let at = if two { format!(" ({})", e.side.words()) } else { String::new() };
            let _ = writeln!(out, "  {}{at}{admin}:", e.name);
            for l in a.command.lines() {
                let _ = writeln!(out, "    $ {}", l.trim());
            }
        }
    }
    Ok(0)
}

/// `GitHub CLI: through apt (as administrator)`; with two sides, which (`… (WSL)`).
fn step_line(e: &Entry, two: bool) -> String {
    let at = if two { format!(" ({})", e.side.words()) } else { String::new() };
    let Some(a) = &e.action else { return format!("{}{at}", e.name) };
    let admin = if a.admin { " (as administrator)" } else { "" };
    if let Some(m) = e.moves.as_ref().filter(|_| a.moving) {
        let from = e.state.path().map(|p| format!(" ({p})")).unwrap_or_default();
        return format!(
            "{}{at}: {}{from}{admin}: install it through {}, check it runs, then `{}`; {}",
            e.name,
            m.words(),
            m.to.words(),
            m.uninstall,
            m.keeps_words()
        );
    }
    let what = if a.update { "update" } else { "install" };
    format!("{}{at}: {what} through {}{admin}", e.name, a.via.words())
}

/// The checklist: agents, tools (numbered), and what's here already; with two sides, a column
/// each. Then what WSL is, when it matters.
fn render(plan: &Plan, out: &mut dyn Write) -> std::io::Result<()> {
    if plan.sides().len() > 1 {
        render_sides(plan, out)?;
    } else {
        render_one(plan, out)?;
    }
    match &plan.wsl {
        Some(w @ (WslStatus::Unusable { .. } | WslStatus::Inside { .. })) => {
            writeln!(out, "\n{}", w.words())
        }
        _ => Ok(()),
    }
}

/// What a checklist line says after the name, for one side's entry.
fn notes_of(plan: &Plan, e: &Entry, label: bool) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(l) = e.label.as_ref().filter(|_| label) {
        notes.push(l.clone());
    }
    if !e.needed_by.is_empty() {
        let by: Vec<&str> = e
            .needed_by
            .iter()
            .map(|id| plan.entry(id).map_or(id.as_str(), |x| x.name.as_str()))
            .collect();
        notes.push(format!("needed by {}", by.join(", ")));
    }
    if matches!(e.state, State::TooOld { .. } | State::Outdated { .. }) {
        notes.push(e.state.words());
    }
    // Installed another way (BOOT7): the move, offered or ticked.
    if let Some(m) = &e.moves {
        notes.push(e.state.words());
        notes.push(if e.ticked {
            format!("{}, then removes {}'s copy", m.words(), m.from.words())
        } else {
            "move to the recommended installer?".to_owned()
        });
    }
    if let Some(b) = &e.blocked {
        notes.push(format!("can't: {b}"));
    }
    notes
}

fn render_one(plan: &Plan, out: &mut dyn Write) -> std::io::Result<()> {
    let pick = rows(plan);
    for (title, kind) in [("Agents", Kind::Agent), ("Tools", Kind::Tool)] {
        let rows: Vec<(usize, &&Entry)> =
            pick.iter().enumerate().filter(|(_, e)| e.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        writeln!(out, "\n{title}")?;
        for (i, e) in rows {
            let mut notes = notes_of(plan, e, true);
            if let Some(a) = e.action.as_ref().filter(|a| e.ticked && !a.moving) {
                let admin = if a.admin { ", as administrator" } else { "" };
                // Where "through" goes: before a reason it can't, as BOOT1 wrote it.
                let at = notes.iter().position(|n| n.starts_with("can't")).unwrap_or(notes.len());
                notes.insert(at, format!("through {}{admin}", a.via.words()));
            }
            let notes =
                if notes.is_empty() { String::new() } else { format!("  ({})", notes.join("; ")) };
            writeln!(out, "  {:>2} {} {}{notes}", i + 1, mark(e), e.name)?;
        }
    }
    let here: Vec<&Entry> = plan.entries.iter().filter(|e| here_already(e)).collect();
    if !here.is_empty() {
        writeln!(out, "\nAlready here")?;
        for e in here {
            let at = e.state.path().map(|p| format!(" ({p})")).unwrap_or_default();
            writeln!(out, "     ✓ {}: {}{at}", e.name, e.state.words())?;
        }
    }
    Ok(())
}

const NAME: usize = 13;
const CELL: usize = 28;

fn render_sides(plan: &Plan, out: &mut dyn Write) -> std::io::Result<()> {
    let sides = plan.sides();
    let heads: Vec<String> = sides
        .iter()
        .map(|s| match (s, &plan.wsl) {
            (Side::Wsl, Some(WslStatus::Usable { distro, .. })) => format!("WSL ({distro})"),
            (s, _) => s.words().to_owned(),
        })
        .collect();
    let pick = rows(plan);
    for (title, kind) in [("Agents", Kind::Agent), ("Tools", Kind::Tool)] {
        let rows: Vec<(usize, &&Entry)> =
            pick.iter().enumerate().filter(|(_, e)| e.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        let head: String = heads.iter().map(|h| format!("{h:<CELL$}")).collect();
        writeln!(out, "\n{:<w$}{}", title, head.trim_end(), w = NAME + 6)?;
        for (i, e) in rows {
            let cells: String = sides
                .iter()
                .map(|s| format!("{:<CELL$}", cell(plan.entry_on(&e.id, *s))))
                .collect();
            let label = e.label.as_ref().map(|l| format!("  ({l})")).unwrap_or_default();
            let line = format!("  {:>2} {:<NAME$} {}", i + 1, e.name, cells);
            writeln!(out, "{}{label}", line.trim_end())?;
            for (s, h) in sides.iter().zip(&heads) {
                let Some(x) = plan.entry_on(&e.id, *s) else { continue };
                let notes = notes_of(plan, x, false);
                if !notes.is_empty() {
                    writeln!(out, "       {h}: {}", notes.join("; "))?;
                }
            }
        }
    }
    let here: Vec<&Entry> = plan
        .entries
        .iter()
        .filter(|e| e.side == sides[0])
        .filter(|e| sides.iter().all(|s| plan.entry_on(&e.id, *s).is_none_or(here_already)))
        .collect();
    if !here.is_empty() {
        writeln!(out, "\nAlready here")?;
        for e in here {
            let each: Vec<String> = sides
                .iter()
                .zip(&heads)
                .filter_map(|(s, h)| {
                    plan.entry_on(&e.id, *s).map(|x| format!("{h} {}", x.state.words()))
                })
                .collect();
            writeln!(out, "     ✓ {}: {}", e.name, each.join("; "))?;
        }
    }
    Ok(())
}

/// The checklist in a terminal: tick and untick by number (or name) until Enter. With WSL's
/// side too, a number ticks on `place`'s sides, and `3:wsl`, `3:windows` or `3:both` puts item 3
/// there. `None`: quit.
fn checklist(
    sides: &Sides,
    place: Where,
    mut picks: Picks,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Option<Picks> {
    let two = sides.wsl.is_some();
    let also = sides.distro().map(|d| format!(" and WSL ({d})")).unwrap_or_default();
    let _ = writeln!(
        out,
        "\nSet up this computer ({}){also}: tick what to install. [x] ticked, [+] needed by something ticked.",
        sides.here.platform.name()
    );
    if two {
        let _ = writeln!(
            out,
            "A number ticks it on {}; 3:wsl, 3:windows or 3:both puts item 3 there.",
            match place {
                Where::Both => "both".to_owned(),
                p => side_name(sides.sides(p)[0], sides),
            }
        );
    }
    let offered: Vec<Side> = sides.sides(Where::Both);
    loop {
        let plan = sides.plan(&picks);
        let _ = render(&plan, out);
        let hint = if two { " (e.g. 3 5, or 3:both)" } else { " (e.g. 3 5)" };
        let _ = write!(out, "\nNumbers to tick or untick{hint}, Enter to install, q to quit: ");
        let _ = out.flush();
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            let _ = writeln!(out);
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            return Some(picks);
        }
        if matches!(line, "q" | "quit" | "exit") {
            return None;
        }
        let pick = rows(&plan);
        for word in line.split([',', ' ']).filter(|w| !w.is_empty()) {
            let (what, at) = match word.split_once(':') {
                Some((w, s)) => (w, Some(s)),
                None => (word, None),
            };
            let at = match at.map(str::parse::<Where>) {
                None => None,
                Some(Ok(w)) if two => Some(w),
                Some(Ok(_)) => {
                    let _ = writeln!(out, "  {word:?}: everything here installs on this computer");
                    continue;
                }
                Some(Err(e)) => {
                    let _ = writeln!(out, "  {word:?}: {e}");
                    continue;
                }
            };
            let entry = match what.parse::<usize>() {
                Ok(n) => pick.get(n.wrapping_sub(1)).copied(),
                Err(_) => setup::resolve(what).ok().and_then(|i| plan.entry(&i.id)),
            };
            let Some(e) = entry else {
                let _ = writeln!(out, "  {word:?}: not on the list");
                continue;
            };
            let targets = sides.sides(at.unwrap_or(place));
            let on =
                |s: Side| if two { format!(" on {}", side_name(s, sides)) } else { String::new() };
            if at.is_some() {
                // A place named: ticked there, and nowhere else.
                for s in &offered {
                    let Some(x) = plan.entry_on(&e.id, *s) else { continue };
                    let list = picks.of(*s);
                    if !targets.contains(s) {
                        list.retain(|p| *p != e.id);
                    } else if here_already(x) {
                        let _ = writeln!(out, "  {} is here already{}", e.name, on(*s));
                    } else if !list.contains(&e.id) {
                        list.push(e.id.clone());
                    }
                }
                continue;
            }
            for line in checklist::toggle(sides, &plan, &mut picks, &e.id, place) {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_agents::catalog::{self, Platform};
    use ptyro_agents::setup::Survey;

    fn survey(installed: &[(&str, &str)]) -> Survey {
        let mut states = BTreeMap::new();
        for i in catalog::items() {
            let st = match installed.iter().find(|(id, _)| *id == i.id) {
                Some((_, v)) => State::Installed { version: Some((*v).into()), path: None },
                None => State::Missing,
            };
            states.insert(i.id.clone(), st);
        }
        Survey {
            platform: Platform::Macos,
            states,
            programs: ["brew".to_owned()].into(),
            candidates: BTreeMap::new(),
            keep: Default::default(),
        }
    }

    fn drive(s: &Survey, picks: &[&str], answers: &str) -> (Option<Vec<String>>, String) {
        let mut out = Vec::new();
        let sides = Sides { here: s.clone(), side: Side::Local, wsl: None, status: None };
        let picks = Picks { here: picks.iter().map(|p| (*p).to_owned()).collect(), wsl: vec![] };
        let r = checklist(&sides, Where::Local, picks, &mut answers.as_bytes(), &mut out);
        (r.map(|p| p.here), String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_checklist_ticks_by_number_and_name_and_enter_installs() {
        let s = survey(&[("git", "2.43.0")]);
        // Agents 1-5, then the tools not here: gh 6, node 7.
        let (r, out) = drive(&s, &["claude", "codex", "gh"], "2\n5 agy\n\n");
        assert_eq!(r.unwrap(), ["claude", "gh", "gemini", "antigravity"]);
        assert!(out.contains("   1 [x] Claude Code"), "{out}");
        assert!(out.contains("Already here\n     ✓ git: installed 2.43.0"), "{out}");
        // After ticking Gemini CLI, Node.js is ticked for it.
        assert!(
            out.contains("   7 [+] Node.js  (needed by Gemini CLI; through Homebrew)"),
            "{out}"
        );
    }

    #[test]
    fn uninstall_takes_the_rc_line_setup_adds_back_out() {
        let before = "export A=1\n";
        let added = format!(
            "{before}\n# ptyro shell integration, and the tools ptyro setup installed\neval \"$(ptyro shell-init bash)\"\n"
        );
        let (kept, gone) = crate::uninstall::strip_rc(&added);
        assert_eq!(kept, before);
        assert_eq!(gone.len(), 3, "{gone:?}");
    }

    #[test]
    fn a_needed_item_stays_and_quitting_installs_nothing() {
        let s = survey(&[]);
        let (r, out) = drive(&s, &["gemini"], "8\n9\nq\n");
        assert!(r.is_none());
        assert!(out.contains("Node.js is needed by something ticked"), "{out}");
        assert!(out.contains("\"9\": not on the list"), "{out}");
        let (r, _) = drive(&s, &[], "");
        assert!(r.is_none(), "end of input is quit");
    }
}
