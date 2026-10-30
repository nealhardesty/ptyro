//! `make parity` (ROADMAP UX1.0, WEB1.0; SPEC §1.3): the CLI, the TUI and the web app are all
//! first class, so every CLI command has a TUI path and a web path, or a waiver with a reason, in
//! `docs/parity.md`.
//!
//! Each row of that file's tables names a command, then how the TUI does it, then the web app:
//! - `` `action` … ``: the first backticked word is an action: a TUI action
//!   (`ptyro_tui::keymap::ACTIONS`, so the palette and `?` list it) or a web one (`ACTIONS` in
//!   `web/src/*.ts`, each a control's tooltip); the rest says where;
//! - `shown: …`: always on screen, nothing to run;
//! - `waived: …`: no path on purpose, and why;
//! - `gap: TASK …`: missing, and the ROADMAP task that brings it.
//!
//! The check fails on a command with no row, a row for a command that doesn't exist, a cell in
//! none of these forms, and an action that doesn't exist. A TUI gap fails it too, since the CLI
//! and the TUI are always in parity (`PTYRO_PARITY_STRICT=0` only lists them, while one is being
//! worked on). A web gap fails too (ROADMAP WEB1 is done); `PTYRO_PARITY_WEB_STRICT=0` only lists
//! them, while one is being worked on.

use std::collections::{BTreeMap, BTreeSet};

const DOC: &str = include_str!("../../../docs/parity.md");
const COMMANDS_DOC: &str = include_str!("../../../docs/commands.md");
const README: &str = include_str!("../../../README.md");
const MAKEFILE: &str = include_str!("../../../Makefile");
/// The web app's sources that register actions: the core and one module per kind of work.
const WEB_SOURCES: [(&str, &str); 10] = [
    ("app.ts", include_str!("../../../web/src/app.ts")),
    ("sessions.ts", include_str!("../../../web/src/sessions.ts")),
    ("projects.ts", include_str!("../../../web/src/projects.ts")),
    ("review.ts", include_str!("../../../web/src/review.ts")),
    ("runs.ts", include_str!("../../../web/src/runs.ts")),
    ("settings.ts", include_str!("../../../web/src/settings.ts")),
    ("term.ts", include_str!("../../../web/src/term.ts")),
    ("panel.ts", include_str!("../../../web/src/panel.ts")),
    ("machines.ts", include_str!("../../../web/src/machines.ts")),
    ("chats.ts", include_str!("../../../web/src/chats.ts")),
];

/// Every runnable command, `wt new` style, without hidden ones (internal helpers).
fn commands() -> BTreeSet<String> {
    fn walk(c: &clap::Command, path: &str, out: &mut BTreeSet<String>) {
        for s in c.get_subcommands().filter(|s| s.get_name() != "help" && !s.is_hide_set()) {
            let p = if path.is_empty() {
                s.get_name().to_owned()
            } else {
                format!("{path} {}", s.get_name())
            };
            if s.get_subcommands().next().is_none() {
                out.insert(p);
            } else {
                walk(s, &p, out);
            }
        }
    }
    // The whole tree is built on the stack: more than a test thread's 2 MiB in a debug build.
    std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            let mut out = BTreeSet::new();
            walk(&<crate::Cli as clap::CommandFactory>::command(), "", &mut out);
            out
        })
        .unwrap()
        .join()
        .unwrap()
}

struct Row {
    command: String,
    tui: String,
    /// `None`: the row has no web cell.
    web: Option<String>,
}

/// Each table row: `| `wt new` | `worktree.new` … | gap: WEB1.3 … |`.
fn rows() -> Vec<Row> {
    DOC.lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.trim().strip_prefix('|')?.split('|').map(str::trim).collect();
            let command = cells.first()?.strip_prefix('`')?.strip_suffix('`')?;
            Some(Row {
                command: command.to_owned(),
                tui: cells.get(1).copied().unwrap_or("").to_owned(),
                web: cells.get(2).copied().filter(|c| !c.is_empty()).map(str::to_owned),
            })
        })
        .collect()
}

/// The web app's actions: the keys of each source's `const ACTIONS = { … } as const;`.
fn web_actions() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (file, src) in WEB_SOURCES {
        let start = src
            .find("const ACTIONS = {")
            .unwrap_or_else(|| panic!("web/src/{file} has `const ACTIONS`"));
        let body = &src[start..];
        let body = &body[..body
            .find("} as const;")
            .unwrap_or_else(|| panic!("ACTIONS in web/src/{file} ends with `}} as const;`"))];
        out.extend(body.lines().filter_map(|l| {
            l.trim().strip_prefix('"')?.split_once("\":").map(|(k, _)| k.to_owned())
        }));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Client {
    Tui,
    Web,
}

#[derive(Debug, PartialEq)]
enum Path {
    Action,
    Shown,
    Waived,
    /// The ROADMAP task that brings it (`WEB1.2`).
    Gap(String),
}

/// A ROADMAP task id: `UX1.21`, `WEB1.3`, `M9.4`.
fn task_id(s: &str) -> Option<&str> {
    let id = s.split(|c: char| c.is_whitespace() || c == '(' || c == ',').next()?;
    let (m, n) = id.split_once('.')?;
    let letters = m.trim_end_matches(|c: char| c.is_ascii_digit());
    (!letters.is_empty()
        && letters.chars().all(|c| c.is_ascii_uppercase())
        && letters.len() < m.len()
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit()))
    .then_some(id)
}

fn classify(cell: &str, client: Client, web: &BTreeSet<String>) -> Result<Path, String> {
    let words = |p: &str| cell.strip_prefix(p).is_some_and(|r| r.trim().len() > 3);
    if words("waived:") {
        return Ok(Path::Waived);
    }
    if words("shown:") {
        return Ok(Path::Shown);
    }
    if let Some(r) = cell.strip_prefix("gap:") {
        return task_id(r.trim()).map(|t| Path::Gap(t.to_owned())).ok_or_else(|| {
            "a gap names the roadmap task that closes it (`gap: WEB1.2 …`)".to_owned()
        });
    }
    let action = cell.strip_prefix('`').and_then(|r| r.split('`').next()).unwrap_or("");
    let known = match client {
        Client::Tui => ptyro_tui::keymap::spec(action).is_some(),
        Client::Web => web.contains(action),
    };
    match (known, action.is_empty(), client) {
        (true, _, _) => Ok(Path::Action),
        (false, true, _) => Err("starts with neither a `action`, shown:, waived: nor gap:".into()),
        (false, false, Client::Tui) => {
            Err(format!("{action:?} is not a TUI action (crates/ptyro-tui/src/keymap.rs ACTIONS)"))
        }
        (false, false, Client::Web) => {
            Err(format!("{action:?} is not a web action (ACTIONS in web/src/*.ts)"))
        }
    }
}

#[test]
fn every_cli_command_has_a_tui_path_and_a_web_path_or_a_waiver() {
    let commands = commands();
    let web = web_actions();
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let (mut tui_gaps, mut web_gaps) = (Vec::new(), BTreeMap::<String, Vec<String>>::new());
    for row in rows() {
        let cmd = &row.command;
        if !seen.insert(cmd.clone()) {
            problems.push(format!("`{cmd}`: listed twice"));
        }
        if !commands.contains(cmd) {
            problems.push(format!("`{cmd}`: no such command (renamed or removed?)"));
        }
        match classify(&row.tui, Client::Tui, &web) {
            Ok(Path::Gap(_)) => tui_gaps.push(format!("`{cmd}`: {}", row.tui)),
            Ok(_) => {}
            Err(e) => problems.push(format!("`{cmd}` (TUI): {e}")),
        }
        match row.web.as_deref().map(|c| classify(c, Client::Web, &web)) {
            None => problems.push(format!("`{cmd}`: no web cell (its row has two columns)")),
            Some(Ok(Path::Gap(t))) => web_gaps.entry(t).or_default().push(cmd.clone()),
            Some(Ok(_)) => {}
            Some(Err(e)) => problems.push(format!("`{cmd}` (web): {e}")),
        }
    }
    for cmd in commands.iter().filter(|c| !seen.contains(*c)) {
        problems.push(format!(
            "`{cmd}`: not in docs/parity.md. Give it a TUI path (an action in the palette) and a \
             web one, or say `waived: why` (SPEC §1.3)"
        ));
    }
    let strict = std::env::var_os("PTYRO_PARITY_STRICT").is_none_or(|v| v != "0");
    if strict {
        problems.extend(tui_gaps.iter().map(|g| format!("TUI gap {g}")));
    } else if !tui_gaps.is_empty() {
        eprintln!("{} TUI gap(s) left:\n  {}", tui_gaps.len(), tui_gaps.join("\n  "));
    }
    let web_strict = std::env::var_os("PTYRO_PARITY_WEB_STRICT").is_none_or(|v| v != "0");
    let total: usize = web_gaps.values().map(Vec::len).sum();
    if total > 0 {
        let by_task: Vec<String> =
            web_gaps.iter().map(|(t, cmds)| format!("{t}: {}", cmds.len())).collect();
        eprintln!("{total} web gap(s) left, by ROADMAP task: {}", by_task.join(", "));
        if web_strict {
            for (t, cmds) in &web_gaps {
                problems.push(format!("web gap {t}: {}", cmds.join(", ")));
            }
        }
    }
    assert!(problems.is_empty(), "parity (docs/parity.md):\n  {}", problems.join("\n  "));
}

#[test]
fn the_web_apps_actions_are_read_from_its_source() {
    let web = web_actions();
    for a in ["sessions", "answer", "session.reply", "inbox"] {
        assert!(web.contains(a), "{a} in {web:?}");
    }
}

#[test]
fn cells_are_classified() {
    let web: BTreeSet<String> = ["answer".to_owned()].into();
    let tui = |c: &str| classify(c, Client::Tui, &web);
    assert_eq!(tui("`worktree.new`, then type a name"), Ok(Path::Action));
    assert_eq!(tui("shown: the sidebar's diffstat"), Ok(Path::Shown));
    assert_eq!(tui("waived: run by programs inside a session"), Ok(Path::Waived));
    assert_eq!(tui("gap: UX1.21 (profiles)"), Ok(Path::Gap("UX1.21".into())));
    assert!(tui("gap: later").is_err());
    assert!(tui("waived:").is_err(), "a waiver says why");
    assert!(tui("`no.such.action`").unwrap_err().contains("not a TUI action"));
    assert!(tui("the inspector").is_err());
    let w = |c: &str| classify(c, Client::Web, &web);
    assert_eq!(w("`answer`: Allow on its card"), Ok(Path::Action));
    assert_eq!(w("gap: WEB1.2 (sessions)"), Ok(Path::Gap("WEB1.2".into())));
    assert!(w("`worktree.new`").unwrap_err().contains("not a web action"));
    assert_eq!(task_id("M9.4 x"), Some("M9.4"));
    assert_eq!(task_id("WEB1 x"), None);
}

/// `docs/commands.md` is where people look a command up: every command is on a line of it that
/// names its group (`ptyro config …`) and it (`set`), under its own name or its group's alias
/// (`wt` for `worktree`). Several commands may share a line (`check|show|edit`).
#[test]
fn every_command_is_in_the_commands_page() {
    let missing = std::thread::Builder::new()
        .stack_size(32 << 20)
        .spawn(|| {
            fn words(s: &str) -> Vec<&str> {
                s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .filter(|w| !w.is_empty())
                    .collect()
            }
            fn walk(c: &clap::Command, path: &[String], groups: &[String], out: &mut Vec<String>) {
                for s in c.get_subcommands().filter(|s| s.get_name() != "help" && !s.is_hide_set())
                {
                    let mut here = path.to_vec();
                    here.push(s.get_name().to_owned());
                    // What the first word may be: the name or an alias.
                    let first: Vec<String> = if path.is_empty() {
                        std::iter::once(s.get_name().to_owned())
                            .chain(s.get_all_aliases().map(str::to_owned))
                            .collect()
                    } else {
                        groups.to_vec()
                    };
                    if s.get_subcommands().next().is_some() {
                        walk(s, &here, &first, out);
                        continue;
                    }
                    let found = if path.is_empty() {
                        first.iter().any(|f| {
                            // `ptyro rename …`, or one of several on a line: `… / `rename …`.
                            COMMANDS_DOC.contains(&format!("ptyro {f}"))
                                || COMMANDS_DOC.contains(&format!("`{f} "))
                                || COMMANDS_DOC.contains(&format!("`{f}`"))
                        })
                    } else {
                        // The line names the group, then (further along) the command.
                        let middle = here[1..here.len() - 1].join(" ");
                        let leaf = s.get_name();
                        first.iter().any(|f| {
                            let head =
                                if middle.is_empty() { f.clone() } else { format!("{f} {middle}") };
                            COMMANDS_DOC.lines().any(|l| {
                                l.find(&format!("ptyro {head}")).is_some_and(|i| {
                                    words(&l[i + 6 + head.len()..]).contains(&leaf)
                                })
                            })
                        })
                    };
                    if !found {
                        out.push(here.join(" "));
                    }
                }
            }
            let mut out = Vec::new();
            walk(&<crate::Cli as clap::CommandFactory>::command(), &[], &[], &mut out);
            out
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        missing.is_empty(),
        "docs/commands.md has no line for {missing:?}: add `ptyro <group> … <command>` where it belongs"
    );
}

/// The README's Development table is where `make` targets are explained: every target with a
/// `##` description in the Makefile has a `make <target>` there.
#[test]
fn every_make_target_is_in_the_readme() {
    let missing: Vec<&str> = MAKEFILE
        .lines()
        .filter_map(|l| {
            let (target, rest) = l.split_once(':')?;
            (rest.contains("## ")
                && !target.is_empty()
                && target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .then_some(target)
        })
        .filter(|t| !README.contains(&format!("make {t}")) && !README.contains(&format!("`{t}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "README.md never mentions the `make` targets {missing:?}: add them to the Development table"
    );
}
