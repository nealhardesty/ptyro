//! `ptyro setup` and `ptyro install` (ROADMAP BOOT1, SPEC §24.2): the setup checklist in a
//! terminal, and installing by name. Both use the one plan engine (`ptyro_agents::setup`) on
//! this computer, with no daemon (a fresh machine has none yet). Admin steps ask once, in this
//! terminal (sudo's own prompt; Windows' UAC from the installer); `--yes` asks nothing and
//! reports what would have needed a prompt, with its command.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};

use ptyro_agents::catalog::{self, Kind, Platform};
use ptyro_agents::setup::{
    self, Admin, Entry, Event, Plan, Report, Side, Sided, State, Survey, Where, WslStatus,
};
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
}

/// A person can answer: a terminal (or a test's fake machine, which pipes its answers; or
/// `--tty`, said by the TUI).
fn interactive(tty: bool) -> bool {
    tty || (std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
        || ptyro_core::test_knob("PTYRO_TEST_SETUP").is_some()
}

/// What each side has ticked (BOOT8): `here` is this computer (Windows itself, on Windows),
/// `wsl` the WSL distro's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Picks {
    here: Vec<String>,
    wsl: Vec<String>,
}

impl Picks {
    fn of(&mut self, side: Side) -> &mut Vec<String> {
        if side == Side::Wsl { &mut self.wsl } else { &mut self.here }
    }
}

/// What setup found: this computer, and on Windows with a usable WSL, the distro.
struct Sides {
    here: Survey,
    /// This computer's side: `Windows` on Windows, else `Local`.
    side: Side,
    /// WSL's survey, when WSL is usable (and wanted).
    wsl: Option<Survey>,
    /// WSL as found (Windows), or that this is WSL; `None` elsewhere.
    status: Option<WslStatus>,
}

impl Sides {
    fn distro(&self) -> Option<&str> {
        self.status.as_ref().and_then(WslStatus::usable).filter(|_| self.wsl.is_some())
    }

    /// The sides `place` means here, those that can be had.
    fn sides(&self, place: Where) -> Vec<Side> {
        place
            .sides(self.side)
            .into_iter()
            .filter(|s| *s != Side::Wsl || self.wsl.is_some())
            .collect()
    }

    fn plan(&self, picks: &Picks) -> Plan {
        let mine = setup::plan(&self.here, &picks.here);
        let wsl = self.wsl.as_ref().map(|s| setup::plan(s, &picks.wsl));
        Plan::merge(mine, wsl, self.status.clone())
    }

    /// The default ticks not installed yet, on `place`'s sides.
    fn defaults(&self, place: Where) -> Picks {
        let mut p = Picks::default();
        for side in self.sides(place) {
            let survey = if side == Side::Wsl { self.wsl.as_ref() } else { Some(&self.here) };
            if let Some(s) = survey {
                *p.of(side) = setup::defaults(s);
            }
        }
        p
    }

    /// `ids` ticked on `place`'s sides.
    fn picks(&self, place: Where, ids: &[String]) -> Picks {
        let mut p = Picks::default();
        for side in self.sides(place) {
            *p.of(side) = ids.to_vec();
        }
        p
    }
}

/// `--where`, checked against this machine: on Windows any of them; elsewhere only `local`.
fn place_of(given: Option<&str>, platform: Platform) -> Result<Option<Where>, Error> {
    let Some(w) = given else { return Ok(None) };
    let w: Where = w.parse().map_err(Error::invalid)?;
    if platform.is_windows() || w == Where::Local {
        return Ok(Some(w));
    }
    Err(Error::invalid(if platform == Platform::Wsl {
        format!(
            "--where {} is for Windows' ptyro: this is WSL, and setup here installs into it. For Windows itself, run `ptyro setup --where windows` in a Windows terminal",
            w.name()
        )
    } else {
        format!(
            "--where {} is for Windows, where an agent can go into WSL, onto Windows, or both; on {} everything installs on this computer: leave --where out",
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
    let here = setup::survey(&*host, &BTreeMap::new());
    let platform = here.platform;
    let place = place_of(given, platform)?;
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
        // The daemon's distro, and only it: another distro with ptyro isn't where the
        // sessions run, so the default stays Windows and the status line says why.
        None => match (prefer.as_deref(), sides.status.as_ref().and_then(WslStatus::usable)) {
            (Some(p), Some(d)) if p.is_empty() || p.eq_ignore_ascii_case(d) => Where::Wsl,
            (Some(p), _) => {
                if !quiet {
                    eprintln!("{}", daemon_elsewhere(p, sides.status.as_ref()));
                }
                Where::Windows
            }
            (None, _) => Where::Windows,
        },
    };
    Ok((host, sides, place))
}

/// Why the default isn't the daemon's distro `prefer` (`client.default_remote`).
fn daemon_elsewhere(prefer: &str, status: Option<&WslStatus>) -> String {
    match status {
        Some(WslStatus::Usable { distro, .. }) => format!(
            "WSL: your daemon is in {prefer}, which setup can't reach; {distro} has ptyro, so `--where wsl` installs there. By default this sets up Windows only."
        ),
        Some(s) => s.words(),
        None => String::new(),
    }
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
        let survey = setup::survey(&*host, &BTreeMap::new());
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
    go(ctx, &*host, &sides, &plan, admin, a.json, false)
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
    let (host, sides, place) = look(&ctx.paths, a.place.as_deref(), quiet).await?;
    let plan = sides.plan(&sides.picks(place, &ids));
    if a.dry_run {
        return show_plan(&plan, a.json);
    }
    let two = plan.sides().len() > 1;
    if !a.yes && asked.is_none() && !plan.steps().is_empty() {
        eprintln!();
        for e in plan.steps() {
            eprintln!("  {}", step_line(e, two));
        }
        if !confirm("Install? [Y/n] ") {
            eprintln!("Nothing installed.");
            return Ok(0);
        }
    }
    let admin = asked.unwrap_or(if a.yes { Admin::Silent } else { Admin::Ask });
    go(ctx, &*host, &sides, &plan, admin, a.json, quiet)
}

/// Run the plan with live output, side by side (this computer first, then WSL's), then what's
/// next. `quiet`: run for Windows' ptyro, which says the rest itself.
fn go(
    ctx: &Ctx,
    host: &dyn setup::Host,
    sides: &Sides,
    plan: &Plan,
    admin: Admin,
    json: bool,
    quiet: bool,
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
        }
        return Ok(0);
    }
    let names: BTreeMap<String, String> =
        plan.entries.iter().map(|e| (e.id.clone(), e.name.clone())).collect();
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
    let windows = plan.platform.is_windows();
    let mut total = Report::default();
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
    }
    if tools {
        offer_path(&ctx.paths, admin != Admin::Ask);
    }
    Ok(if total.ok() { 0 } else { 1 })
}

/// `Windows`, `WSL (Ubuntu)`.
fn side_name(side: Side, sides: &Sides) -> String {
    match (side, sides.distro()) {
        (Side::Wsl, Some(d)) => format!("WSL ({d})"),
        (s, _) => s.words().to_owned(),
    }
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
        Event::Failed { id, error, fix } => {
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
        Event::Logins { ids } => {
            eprintln!("\nTo log in (each also asks the first time it starts):");
            for id in ids {
                if let Some(l) = catalog::item(id).and_then(|i| i.login.as_ref()) {
                    eprintln!("  {}: `{}`", name(id), l.start.join(" "));
                }
            }
        }
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
    let what = if a.update { "update" } else { "install" };
    format!("{}{at}: {what} through {}{admin}", e.name, a.via.words())
}

fn here_already(e: &Entry) -> bool {
    matches!(e.state, State::Installed { .. } | State::Other { .. })
}

/// The items a person can tick, in the checklist's order: what isn't here (or isn't new
/// enough) on some side.
fn rows(plan: &Plan) -> Vec<&Entry> {
    let sides = plan.sides();
    let Some(first) = sides.first() else { return Vec::new() };
    plan.entries
        .iter()
        .filter(|e| e.side == *first)
        .filter(|e| {
            sides.iter().any(|s| plan.entry_on(&e.id, *s).is_some_and(|x| !here_already(x)))
        })
        .collect()
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
    if let Some(b) = &e.blocked {
        notes.push(format!("can't: {b}"));
    }
    notes
}

fn mark(e: &Entry) -> &'static str {
    if e.picked {
        "[x]"
    } else if e.ticked {
        "[+]"
    } else {
        "[ ]"
    }
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
            if let Some(a) = e.action.as_ref().filter(|_| e.ticked) {
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

/// One side's cell: `[x] WinGet`, `[+] apt (admin)`, `✓ 2.43.0`, `[ ]`.
fn cell(e: Option<&Entry>) -> String {
    let Some(e) = e else { return String::new() };
    if here_already(e) {
        return format!("✓ {}", e.state.version().unwrap_or("installed"));
    }
    match e.action.as_ref().filter(|_| e.ticked) {
        Some(a) => {
            format!("{} {}{}", mark(e), a.via.words(), if a.admin { " (admin)" } else { "" })
        }
        None if e.ticked && e.blocked.is_some() => format!("{} can't", mark(e)),
        None => mark(e).to_owned(),
    }
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
            let here: Vec<&Entry> =
                targets.iter().filter_map(|s| plan.entry_on(&e.id, *s)).collect();
            if here.iter().any(|x| x.picked) {
                for x in &here {
                    picks.of(x.side).retain(|p| *p != e.id);
                }
                continue;
            }
            for x in here {
                if x.ticked {
                    let _ = writeln!(
                        out,
                        "  {} is needed by something ticked{}: untick that first",
                        e.name,
                        on(x.side)
                    );
                } else if here_already(x) {
                    let _ = writeln!(out, "  {} is here already{}", e.name, on(x.side));
                } else {
                    picks.of(x.side).push(e.id.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_agents::catalog::Platform;

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
