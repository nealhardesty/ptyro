//! `ptyro setup` and `ptyro install` (ROADMAP BOOT1, SPEC §24.2): the setup checklist in a
//! terminal, and installing by name. Both use the one plan engine (`ptyro_agents::setup`) on
//! this computer, with no daemon (a fresh machine has none yet). Admin steps ask once, in this
//! terminal (sudo's own prompt); `--yes` asks nothing and reports what would have needed a
//! prompt, with its command.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};

use ptyro_agents::catalog::{self, Kind};
use ptyro_agents::own;
use ptyro_agents::setup::checklist::{self, here_already, mark, rows};
use ptyro_agents::setup::{self, Admin, Entry, Event, Plan, Report, State, Survey};
use ptyro_agents::updates::{self, Outdated};
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

mod rest;

pub struct SetupArgs {
    pub yes: bool,
    pub agents: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub dry_run: bool,
    pub json: bool,
    /// The removed `--where` (hidden): refused with what to do instead.
    pub place: Option<String>,
    /// `--move` (BOOT7): with `--yes`, move the ticked agents installed another way too.
    pub move_: bool,
}

pub struct InstallArgs {
    pub names: Vec<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
    /// The removed `--where` (hidden): refused with what to do instead.
    pub place: Option<String>,
    /// Hidden `--confirmed`: the TUI's checklist asked already (BOOT2), so no `Install?`; the
    /// rest (sudo's prompt, the logins, the profiles line) is as when typed.
    pub confirmed: bool,
    /// `--move` (BOOT7): with `--yes`, move the named agents installed another way too.
    pub move_: bool,
}

/// A person can answer: a terminal (or a test's fake machine, which pipes its answers).
pub(crate) fn interactive() -> bool {
    (std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
        || ptyro_core::test_knob("PTYRO_TEST_SETUP").is_some()
}

/// `--where` chose a side for native Windows' ptyro, which is gone (2026-11-18): refused, with
/// what to do instead. `None`: nothing given, nothing to say.
pub(crate) fn where_removed(given: Option<&str>) -> Result<(), Error> {
    match given {
        None => Ok(()),
        Some(_) => Err(Error::invalid(
            "`--where` chose Windows' or WSL's side for the native Windows ptyro, which was removed on 2026-11-18: setup installs on this computer (inside WSL, into WSL). Leave --where out",
        )),
    }
}

/// Look at this computer.
fn look(paths: &Paths) -> (Box<dyn setup::Host + Send>, Survey) {
    eprintln!("Looking at what's installed…");
    let host = setup::host(paths);
    // The remembered newest versions (BOOT5): an outdated agent shows so, and can be ticked;
    // one installed another way is offered the move unless `updates.keep_installer` keeps it
    // (BOOT7).
    let here = updates::survey(&*host, &paths.state, &updates_config(paths));
    (host, here)
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
    where_removed(a.place.as_deref())?;
    let named = a.agents.is_some() || a.tools.is_some();
    let mut ids = ids_of(a.agents.as_deref().unwrap_or_default(), Kind::Agent, "--agents")?;
    ids.extend(ids_of(a.tools.as_deref().unwrap_or_default(), Kind::Tool, "--tools")?);
    if !a.yes && !a.dry_run && !interactive() {
        return Err(Error::invalid(
            "`ptyro setup` is a checklist: run it in a terminal, or pass --yes (with --agents and --tools, or the defaults: Claude Code, Codex, git, GitHub CLI)",
        ));
    }
    // Already running (the web's, another terminal's, the TUI's): say so before the checklist,
    // and hold the machine's lock from here, so nothing starts while this one is asked.
    let lock = if a.dry_run { None } else { Some(take_lock(ctx, "`ptyro setup`")?) };
    let (host, mut survey) = look(&ctx.paths);
    let mut picks = if named { ids } else { setup::defaults(&survey) };
    // Unattended, nothing moves unless `--move` (BOOT7: a move is offered, never done unasked);
    // each one passed over is said.
    if a.yes && !a.move_ {
        say_unmoved(&setup::plan(&survey, &picks), &[]);
        checklist::keep_all(&mut survey, &[]);
    }
    if a.dry_run {
        return show_plan(&setup::plan(&survey, &picks), a.json);
    }
    // A person at a terminal goes on to the rest after the checklist (what `ptyro init` asked):
    // accounts, preferences, repositories. A script's run, or one for named items, stops here.
    let rest = !a.yes && !named && !a.json && rest::wanted();
    if !a.yes {
        // Let go of stdin before the rest of setup reads it (its lock isn't reentrant).
        let picked =
            checklist(&survey, picks, &mut std::io::stdin().lock(), &mut std::io::stderr());
        match picked {
            Some(p) => picks = p,
            None => {
                eprintln!("Nothing installed.");
                drop(lock);
                return if rest { rest::run(ctx).await } else { Ok(0) };
            }
        }
    }
    let plan = setup::plan(&survey, &picks);
    let admin = if a.yes { Admin::Silent } else { Admin::Ask };
    let code = go(ctx, &*host, &plan, admin, a.json, lock, "`ptyro setup`")?;
    if rest {
        rest::run(ctx).await?;
    }
    Ok(code)
}

pub async fn install(ctx: &Ctx, a: InstallArgs) -> Result<i32, Error> {
    where_removed(a.place.as_deref())?;
    let mut ids = Vec::new();
    for n in a.names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if !ids.contains(&item.id) {
            ids.push(item.id.clone());
        }
    }
    if ids.is_empty() {
        return Err(Error::invalid("name what to install: `ptyro install codex`"));
    }
    if !a.yes && !a.dry_run && !interactive() {
        return Err(Error::invalid(
            "`ptyro install` asks before it installs: run it in a terminal, or pass --yes",
        ));
    }
    let who = if a.confirmed { "the TUI's setup checklist" } else { "`ptyro install`" };
    // Already running: say so before asking anything, and hold the lock from here.
    let lock = if a.dry_run { None } else { Some(take_lock(ctx, who)?) };
    let (host, mut survey) = look(&ctx.paths);
    // A move is offered, never done unasked (BOOT7, the design): the question asks it, the TUI's
    // checklist ticked it, or `--move` says so; unattended (`--yes`) the agent stays as it is.
    if !(a.move_ || a.confirmed || !a.yes) {
        say_unmoved(&setup::plan(&survey, &ids), &[]);
        checklist::keep_all(&mut survey, &[]);
    }
    let mut plan = setup::plan(&survey, &ids);
    if a.dry_run {
        return show_plan(&plan, a.json);
    }
    if !a.yes && !a.confirmed && !plan.steps().is_empty() {
        eprintln!();
        for e in plan.steps() {
            eprintln!("  {}", step_line(e));
        }
        // A move to the recommended installer (BOOT7) can be declined for good: `k` keeps
        // those the way they're installed.
        let moving: Vec<&Entry> = plan
            .steps()
            .into_iter()
            .filter(|e| e.action.as_ref().is_some_and(|a| a.moving))
            .collect();
        let q = if moving.is_empty() {
            "Install? [Y/n] ".to_owned()
        } else {
            let each: Vec<String> = moving
                .iter()
                .map(|e| {
                    let how = e.moves.as_ref().map_or("its installer", |m| m.from.words());
                    format!("{} with {how}", e.name)
                })
                .collect();
            format!("Install? [Y/n, or k to keep {} and not be asked again] ", each.join(" and "))
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
                survey.keep.extend(kept);
                plan = setup::plan(&survey, &rest);
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
    let admin = if a.yes { Admin::Silent } else { Admin::Ask };
    go(ctx, &*host, &plan, admin, a.json, lock, who)
}

/// Unattended (`--yes` without `--move`), each picked agent installed another way that is left
/// as it is, and how to move it (BOOT7).
fn say_unmoved(plan: &Plan, allowed: &[String]) {
    for e in
        plan.entries.iter().filter(|e| e.picked && e.moves.is_some() && !allowed.contains(&e.id))
    {
        let m = e.moves.as_ref().map_or("another way", |m| m.from.words());
        eprintln!(
            "{} is installed with {m}: moving it to the recommended installer needs a yes (`ptyro install {}` in a terminal, or --move); left as it is",
            e.name,
            catalog::item(&e.id).map_or(e.id.as_str(), |i| i.program.as_str())
        );
    }
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
    /// This machine only, not the machines linked to it (SPEC §24.8).
    pub here: bool,
}

/// `ptyro update [NAME…] [--check]` (BOOT5, SPEC §24.7): look for newer versions afresh, then
/// update the agents that have one (those named, or every one `updates.ignore` doesn't skip),
/// each through the plan engine with the method it was installed by, under the machine's setup
/// lock, after a yes. Sessions running an updated agent keep the old version until they restart,
/// and are named. ptyro itself is one more row (BOOT6, SPEC §24.8): its release downloaded,
/// checked and swapped in last, then the daemon upgraded live.
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
    if !a.yes && !interactive() {
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
    // ptyro itself (BOOT6).
    let install = own::Install::here();
    let ptyro_named = ids.iter().any(|i| i == own::ID);
    // As `updates::remember` will see it: this look's, else the one remembered (the API may be
    // out of requests while the download still works).
    let ptyro_latest = looked
        .0
        .get(own::ID)
        .cloned()
        .or_else(|| updates::load(&state).latest.get(own::ID).cloned());
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
    // The machines linked here on an older ptyro than this one will run (SPEC §24.8): offered
    // too, each updating itself. Not with `--here` (a machine asked by another), nor when only
    // agents were named.
    // Each of them installs the newest release (`SelfUpdate`), so they are offered only when
    // this machine ends up on it too: otherwise they would pass it, and their links stay down
    // the other way.
    let target = ptyro_todo.as_ref().map_or_else(
        || own::version_of(&install.path).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
        |o| o.latest.clone(),
    );
    let linked = if a.here || !(ids.is_empty() || ptyro_named) {
        Vec::new()
    } else {
        let behind = crate::linked::behind(ctx, &target).await;
        if !behind.is_empty() && ptyro_latest.as_deref() != Some(target.as_str()) {
            eprintln!(
                "note: machines linked here run an older ptyro ({}), but this machine won't be on the newest release, so they aren't offered (they'd pass it): `ptyro machine update` asks them anyway",
                behind
                    .iter()
                    .map(|b| format!("{} {}", b.name, b.version))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Vec::new()
        } else {
            behind
        }
    };
    let linked_names: Vec<String> = linked.iter().map(|b| b.name.clone()).collect();
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
        let mut ok = true;
        // This machine is current; machines linked to it may not be.
        if !linked.is_empty() {
            eprintln!("\nThis machine runs ptyro {target}; machines linked here are behind:");
            eprintln!("{}", crate::linked::question_line(&linked, &target));
            if a.yes || confirm("Update them? [Y/n] ") {
                ok &= crate::linked::update_them(ctx, &linked_names, a.json).await;
            } else {
                eprintln!("They weren't asked: `ptyro machine update` asks them later.");
            }
        }
        return Ok(if ok { 0 } else { 1 });
    }
    let todo_ids: Vec<String> = todo.iter().map(|o| o.id.clone()).collect();
    let running = sessions_running(ctx, &todo_ids).await;
    if !a.yes {
        eprintln!();
        for o in todo.iter().filter(|o| o.id == own::ID) {
            for l in crate::own::question(o, &install) {
                eprintln!("{l}");
            }
        }
        if !linked.is_empty() {
            eprintln!("{}", crate::linked::question_line(&linked, &target));
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
    let mut done: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    if todo.iter().any(|o| o.id != own::ID) {
        let plan = setup::plan(&never_move(&survey), &todo_ids);
        let mut on = |ev: Event| {
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
                _ => print_event(&ev, &name),
            }
        };
        let report = setup::run(&plan, &*host, admin, &mut on);
        done.extend(report.done.iter().cloned());
        failed.extend(report.failed.iter().chain(&report.skipped).chain(&report.blocked).cloned());
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
                print_event(&ev, &name);
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
    // Then the daemon onto it, live.
    if let Some(v) = &ptyro_new
        && !crate::own::upgrade_daemon(ctx, install.after(), v).await
    {
        failed.push("daemon".into());
    }
    // Then the machines linked here, once this one is on the release they will install (asked
    // over connections of their own, so their links being down for the version doesn't matter).
    if !linked.is_empty() {
        if ptyro_todo.is_some() && ptyro_new.is_none() {
            eprintln!(
                "The machines linked here weren't asked, since this machine's ptyro wasn't updated: `ptyro machine update` asks them once it is."
            );
        } else if !crate::linked::update_them(ctx, &linked_names, a.json).await {
            failed.push("a linked machine".into());
        }
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
        eprintln!("{}", ptyro_agents::locate::no_agent());
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
            .map(|p| p.rsplit('/').next().unwrap_or(p).to_owned())
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

/// Run the plan with live output, then what's next. `lock`: the machine's setup lock, held since
/// before the questions; `None`, it is taken here for `who`.
fn go(
    ctx: &Ctx,
    host: &dyn setup::Host,
    plan: &Plan,
    admin: Admin,
    json: bool,
    lock: Option<setup::lock::Lock>,
    who: &str,
) -> Result<i32, Error> {
    for e in plan.entries.iter().filter(|e| e.picked && e.action.is_none() && e.blocked.is_none()) {
        eprintln!("✓ {} is here already ({})", e.name, e.state.words());
    }
    if !plan.entries.iter().any(|e| e.will_run() || (e.ticked && e.blocked.is_some())) {
        eprintln!("Nothing to install.");
        if !json {
            logins(host, plan, &Report::default(), admin);
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
    let mut on = |ev: Event| {
        if json {
            if let Ok(line) = serde_json::to_string(&ev) {
                println!("{line}");
            }
            return;
        }
        print_event(&ev, &name);
    };
    let total = setup::run(plan, host, admin, &mut on);
    if !total.done.is_empty() || !total.failed.is_empty() {
        // What is out of date changed (BOOT5; a move that failed half way changed it too,
        // BOOT7): the badge and the start-up note follow. Under the lock still, so a look beside
        // it can't write over this.
        updates::refresh(host, &ctx.paths.state, updates::now());
    }
    drop(lock);
    if total.tools {
        offer_path(&ctx.paths, admin != Admin::Ask);
    }
    if !json {
        logins(host, plan, &total, admin);
    }
    Ok(if total.ok() { 0 } else { 1 })
}

/// The machine's setup lock (`setup::lock`): one install at a time, whoever starts it.
fn take_lock(ctx: &Ctx, who: &str) -> Result<setup::lock::Lock, Error> {
    setup::lock::Lock::take(&ctx.paths.state, who).map_err(Error::conflict)
}

/// After the installs (BOOT4): the logins, walked through in this terminal when a person is here
/// (else where each stands), then how to add more accounts as profiles. The exit code stays the
/// installs'.
fn logins(host: &dyn setup::Host, plan: &Plan, report: &Report, admin: Admin) {
    let ids = setup::login::after(plan, report);
    if crate::logins::after_setup(host, &ids, &report.done, admin == Admin::Ask) {
        eprintln!("\n{}", setup::login::PROFILES_HINT);
    }
}

fn print_event(ev: &Event, name: &dyn Fn(&str) -> String) {
    let list = |ids: &[String]| ids.iter().map(|i| name(i)).collect::<Vec<_>>().join(", ");
    match ev {
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
        // The logins are walked through after the run (`logins`).
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
pub(crate) fn confirm(q: &str) -> bool {
    eprint!("{q}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(s.trim(), "" | "y" | "Y" | "yes"),
    }
}

/// New terminals need ptyro's tools folders on `PATH`: through `ptyro shell-init`'s line in the
/// shell's rc file (it adds them first). Asked; `--yes` only says how, as it leaves rc files
/// alone.
fn offer_path(paths: &Paths, yes: bool) {
    let dirs = setup::path_dirs(paths);
    let shown: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    eprintln!(
        "\nSome of this went into ptyro's tools folder ({}). ptyro's own sessions find it.",
        paths.tools().display()
    );
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
    if steps.is_empty() {
        let _ = writeln!(out, "\nNothing to install.");
    } else {
        let _ = writeln!(out, "\nWhat would run, in order:");
        for (e, a) in steps.iter().filter_map(|e| Some((e, e.action.as_ref()?))) {
            let admin = if a.admin { " (as administrator)" } else { "" };
            let _ = writeln!(out, "  {}{admin}:", e.name);
            for l in a.command.lines() {
                let _ = writeln!(out, "    $ {}", l.trim());
            }
        }
    }
    Ok(0)
}

/// `GitHub CLI: through apt (as administrator)`.
fn step_line(e: &Entry) -> String {
    let Some(a) = &e.action else { return e.name.clone() };
    let admin = if a.admin { " (as administrator)" } else { "" };
    if let Some(m) = e.moves.as_ref().filter(|_| a.moving) {
        let from = e.state.path().map(|p| format!(" ({p})")).unwrap_or_default();
        return format!(
            "{}: {}{from}{admin}: install it through {}, check it runs, then `{}`; {}",
            e.name,
            m.words(),
            m.to.words(),
            m.uninstall,
            m.keeps_words()
        );
    }
    let what = if a.update { "update" } else { "install" };
    format!("{}: {what} through {}{admin}", e.name, a.via.words())
}

/// What a checklist line says after the name.
fn notes_of(plan: &Plan, e: &Entry) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(l) = &e.label {
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

/// The checklist: agents, tools (numbered), and what's here already.
fn render(plan: &Plan, out: &mut dyn Write) -> std::io::Result<()> {
    let pick = rows(plan);
    for (title, kind) in [("Agents", Kind::Agent), ("Tools", Kind::Tool)] {
        let rows: Vec<(usize, &&Entry)> =
            pick.iter().enumerate().filter(|(_, e)| e.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        writeln!(out, "\n{title}")?;
        for (i, e) in rows {
            let mut notes = notes_of(plan, e);
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

/// The checklist in a terminal: tick and untick by number (or name) until Enter. `None`: quit.
fn checklist(
    survey: &Survey,
    mut picks: Vec<String>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Option<Vec<String>> {
    let _ = writeln!(
        out,
        "\nSet up this computer ({}): tick what to install. [x] ticked, [+] needed by something ticked.",
        survey.platform.name()
    );
    loop {
        let plan = setup::plan(survey, &picks);
        let _ = render(&plan, out);
        let _ =
            write!(out, "\nNumbers to tick or untick (e.g. 3 5), Enter to install, q to quit: ");
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
            let entry = match word.parse::<usize>() {
                Ok(n) => pick.get(n.wrapping_sub(1)).copied(),
                Err(_) => setup::resolve(word).ok().and_then(|i| plan.entry(&i.id)),
            };
            let Some(e) = entry else {
                let _ = writeln!(out, "  {word:?}: not on the list");
                continue;
            };
            for line in checklist::toggle(&plan, &mut picks, &e.id) {
                let _ = writeln!(out, "  {line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_agents::catalog::{self, Platform};

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
        let picks = picks.iter().map(|p| (*p).to_owned()).collect();
        let r = checklist(s, picks, &mut answers.as_bytes(), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_checklist_ticks_by_number_and_name_and_enter_installs() {
        let s = survey(&[("git", "2.43.0")]);
        // Agents 1-3, then the tools not here: gh 4, node 5.
        let (r, out) = drive(&s, &["claude", "codex", "gh"], "2\nopencode\n\n");
        assert_eq!(r.unwrap(), ["claude", "gh", "opencode"]);
        assert!(out.contains("   1 [x] Claude Code"), "{out}");
        assert!(out.contains("Already here\n     ✓ git: installed 2.43.0"), "{out}");
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
    fn an_unknown_answer_is_said_and_quitting_installs_nothing() {
        let s = survey(&[]);
        let (r, out) = drive(&s, &["claude"], "9\nq\n");
        assert!(r.is_none());
        assert!(out.contains("\"9\": not on the list"), "{out}");
        let (r, _) = drive(&s, &[], "");
        assert!(r.is_none(), "end of input is quit");
    }

    /// The old `--where` (native Windows' sides, removed 2026-11-18) is refused with what to do.
    #[test]
    fn the_removed_where_says_to_leave_it_out() {
        assert!(where_removed(None).is_ok());
        let e = where_removed(Some("wsl")).unwrap_err();
        assert!(e.message.contains("removed on 2026-11-18"), "{}", e.message);
        assert!(e.message.contains("Leave --where out"), "{}", e.message);
    }
}
