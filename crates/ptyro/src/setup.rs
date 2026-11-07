//! `ptyro setup` and `ptyro install` (ROADMAP BOOT1, SPEC §24.2): the setup checklist in a
//! terminal, and installing by name. Both use the one plan engine (`ptyro_agents::setup`) on
//! this computer, with no daemon (a fresh machine has none yet). Admin steps ask once, in this
//! terminal (sudo's own prompt; Windows' UAC from the installer); `--yes` asks nothing and
//! reports what would have needed a prompt, with its command.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};

use ptyro_agents::catalog::{self, Kind};
use ptyro_agents::setup::{self, Admin, Entry, Event, Plan, Report, State, Survey};
use ptyro_core::paths::Paths;
use ptyro_core::{Error, ErrorCode};

use crate::cli::Ctx;

pub struct SetupArgs {
    pub yes: bool,
    pub agents: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub dry_run: bool,
    pub json: bool,
}

/// A person can answer: a terminal (or a test's fake machine, which pipes its answers).
fn interactive() -> bool {
    (std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
        || ptyro_core::test_knob("PTYRO_TEST_SETUP").is_some()
}

fn look(paths: &Paths) -> (Box<dyn setup::Host + Send>, Survey) {
    eprintln!("Looking at what's installed…");
    let host = setup::host(paths);
    let survey = setup::survey(&*host, &BTreeMap::new());
    (host, survey)
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
    let named = a.agents.is_some() || a.tools.is_some();
    let mut picks = ids_of(a.agents.as_deref().unwrap_or_default(), Kind::Agent, "--agents")?;
    picks.extend(ids_of(a.tools.as_deref().unwrap_or_default(), Kind::Tool, "--tools")?);
    if !a.yes && !a.dry_run && !interactive() {
        return Err(Error::invalid(
            "`ptyro setup` is a checklist: run it in a terminal, or pass --yes (with --agents and --tools, or the defaults: Claude Code, Codex, git, GitHub CLI)",
        ));
    }
    let (host, survey) = look(&ctx.paths);
    if !named {
        picks = setup::defaults(&survey);
    }
    if a.dry_run {
        let plan = setup::plan(&survey, &picks);
        return show_plan(&plan, a.json);
    }
    if !a.yes {
        let stdin = std::io::stdin();
        match checklist(&survey, picks, &mut stdin.lock(), &mut std::io::stderr()) {
            Some(p) => picks = p,
            None => {
                eprintln!("Nothing installed.");
                return Ok(0);
            }
        }
    }
    let plan = setup::plan(&survey, &picks);
    go(ctx, &*host, &plan, a.yes, a.json)
}

pub async fn install(
    ctx: &Ctx,
    names: Vec<String>,
    yes: bool,
    dry_run: bool,
    json: bool,
) -> Result<i32, Error> {
    let mut picks = Vec::new();
    for n in names.iter().flat_map(|n| n.split([',', ' '])).filter(|n| !n.is_empty()) {
        let item = setup::resolve(n).map_err(|e| Error::new(ErrorCode::NotFound, e))?;
        if !picks.contains(&item.id) {
            picks.push(item.id.clone());
        }
    }
    if picks.is_empty() {
        return Err(Error::invalid("name what to install: `ptyro install agy`"));
    }
    if !yes && !dry_run && !interactive() {
        return Err(Error::invalid(
            "`ptyro install` asks before it installs: run it in a terminal, or pass --yes",
        ));
    }
    let (host, survey) = look(&ctx.paths);
    let plan = setup::plan(&survey, &picks);
    if dry_run {
        return show_plan(&plan, json);
    }
    if !yes && !plan.steps().is_empty() {
        eprintln!();
        for e in plan.steps() {
            eprintln!("  {}", step_line(e));
        }
        if !confirm("Install? [Y/n] ") {
            eprintln!("Nothing installed.");
            return Ok(0);
        }
    }
    go(ctx, &*host, &plan, yes, json)
}

/// Run the plan with live output, then what's next.
fn go(ctx: &Ctx, host: &dyn setup::Host, plan: &Plan, yes: bool, json: bool) -> Result<i32, Error> {
    for e in plan.entries.iter().filter(|e| e.ticked && e.action.is_none()) {
        match &e.blocked {
            Some(why) => eprintln!("✗ {}: {why}", e.name),
            None if e.picked => eprintln!("✓ {} is here already ({})", e.name, e.state.words()),
            None => {}
        }
    }
    if plan.steps().is_empty() {
        let blocked = plan.entries.iter().any(|e| e.blocked.is_some());
        if !blocked {
            eprintln!("Nothing to install.");
        }
        return Ok(if blocked { 1 } else { 0 });
    }
    let names: BTreeMap<String, String> =
        plan.entries.iter().map(|e| (e.id.clone(), e.name.clone())).collect();
    let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| id.to_owned());
    let admin = if yes { Admin::Silent } else { Admin::Ask };
    let windows = plan.platform.is_windows();
    let report = setup::run(plan, host, admin, &mut |ev| {
        if json {
            if let Ok(line) = serde_json::to_string(&ev) {
                println!("{line}");
            }
            return;
        }
        print_event(&ev, &name, windows);
    });
    if report.tools {
        offer_path(&ctx.paths, yes);
    }
    Ok(if report.ok() { 0 } else { 1 })
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

/// Windows: `dirs` at the front of the user's `PATH` (HKCU\Environment), each once.
#[cfg(windows)]
fn add_user_path(dirs: &[std::path::PathBuf]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let list: Vec<String> =
        dirs.iter().map(|d| format!("'{}'", d.display().to_string().replace('\'', "''"))).collect();
    let script = format!(
        "$add = @({}); $cur = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         $parts = @($cur -split ';' | Where-Object {{ $_ -and ($add -notcontains $_) }}); \
         [Environment]::SetEnvironmentVariable('Path', (($add + $parts) -join ';'), 'User')",
        list.join(", ")
    );
    let ok = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if ok { Ok(()) } else { Err("PowerShell couldn't set it".into()) }
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
    let what = if a.update { "update" } else { "install" };
    format!("{}: {what} through {}{admin}", e.name, a.via.words())
}

/// The entries a person can tick: what isn't here, or isn't new enough.
fn pickable(plan: &Plan) -> Vec<&Entry> {
    plan.entries
        .iter()
        .filter(|e| !matches!(e.state, State::Installed { .. } | State::Other { .. }))
        .collect()
}

/// The checklist: agents, tools (numbered), and what's here already.
fn render(plan: &Plan, out: &mut dyn Write) -> std::io::Result<()> {
    let pick = pickable(plan);
    for (title, kind) in [("Agents", Kind::Agent), ("Tools", Kind::Tool)] {
        let rows: Vec<(usize, &&Entry)> =
            pick.iter().enumerate().filter(|(_, e)| e.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        writeln!(out, "\n{title}")?;
        for (i, e) in rows {
            let mark = if e.picked {
                "[x]"
            } else if e.ticked {
                "[+]"
            } else {
                "[ ]"
            };
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
            if let Some(a) = e.action.as_ref().filter(|_| e.ticked) {
                let admin = if a.admin { ", as administrator" } else { "" };
                notes.push(format!("through {}{admin}", a.via.words()));
            }
            if let Some(b) = &e.blocked {
                notes.push(format!("can't: {b}"));
            }
            let notes =
                if notes.is_empty() { String::new() } else { format!("  ({})", notes.join("; ")) };
            writeln!(out, "  {:>2} {mark} {}{notes}", i + 1, e.name)?;
        }
    }
    let here: Vec<&Entry> = plan
        .entries
        .iter()
        .filter(|e| matches!(e.state, State::Installed { .. } | State::Other { .. }))
        .collect();
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
        let pick = pickable(&plan);
        for word in line.split([',', ' ']).filter(|w| !w.is_empty()) {
            let entry = match word.parse::<usize>() {
                Ok(n) => pick.get(n.wrapping_sub(1)).copied(),
                Err(_) => setup::resolve(word).ok().and_then(|i| plan.entry(&i.id)),
            };
            let Some(e) = entry else {
                let _ = writeln!(out, "  {word:?}: not on the list");
                continue;
            };
            if e.picked {
                picks.retain(|p| *p != e.id);
            } else if e.ticked {
                let _ =
                    writeln!(out, "  {} is needed by something ticked: untick that first", e.name);
            } else if matches!(e.state, State::Installed { .. } | State::Other { .. }) {
                let _ = writeln!(out, "  {} is here already", e.name);
            } else {
                picks.push(e.id.clone());
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
        let picks = picks.iter().map(|p| (*p).to_owned()).collect();
        let r = checklist(s, picks, &mut answers.as_bytes(), &mut out);
        (r, String::from_utf8(out).unwrap())
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
