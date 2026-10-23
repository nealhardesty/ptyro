//! `make tui-parity` (ROADMAP UX1.0, SPEC §1.3): the TUI is first class, so every CLI command
//! has a TUI path, or a waiver with a reason, in `docs/tui-parity.md`.
//!
//! Each row of that file's table names a command and says how the TUI does it:
//! - `` `action` … ``: the first backticked word is a TUI action (`crate::keymap::ACTIONS`),
//!   so the palette and `?` list it; the rest says where (a key in a view, a tab);
//! - `shown: …`: always on screen (the sidebar, the status bar), nothing to run;
//! - `waived: …`: no TUI path on purpose, and why;
//! - `gap: …`: missing. UX1 closed every one; a gap fails the check (`PTYRO_PARITY_STRICT=0`
//!   only lists them, while one is being worked on).
//!
//! The check fails on a command with no row, a row for a command that doesn't exist, a cell in
//! none of these forms, and an action that doesn't exist.

use std::collections::BTreeSet;

const DOC: &str = include_str!("../../../docs/tui-parity.md");

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

/// `(command, cell)` for each table row: `| `wt new` | `worktree.new` … |`.
fn rows() -> Vec<(String, String)> {
    DOC.lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.trim().strip_prefix('|')?.split('|').map(str::trim).collect();
            let cmd = cells.first()?.strip_prefix('`')?.strip_suffix('`')?;
            Some((cmd.to_owned(), cells.get(1).copied().unwrap_or("").to_owned()))
        })
        .collect()
}

#[derive(Debug, PartialEq)]
enum Path {
    Action,
    Shown,
    Waived,
    Gap,
}

fn classify(cell: &str) -> Result<Path, String> {
    let words = |p: &str| cell.strip_prefix(p).is_some_and(|r| r.trim().len() > 3);
    if words("waived:") {
        return Ok(Path::Waived);
    }
    if words("shown:") {
        return Ok(Path::Shown);
    }
    if let Some(r) = cell.strip_prefix("gap:") {
        return if r.trim().starts_with("UX1.") {
            Ok(Path::Gap)
        } else {
            Err("a gap names the roadmap task that closes it (`gap: UX1.21 …`)".into())
        };
    }
    let action = cell.strip_prefix('`').and_then(|r| r.split('`').next()).unwrap_or("");
    if ptyro_tui::keymap::spec(action).is_some() {
        Ok(Path::Action)
    } else if action.is_empty() {
        Err("starts with neither a `action`, shown:, waived: nor gap:".into())
    } else {
        Err(format!("{action:?} is not a TUI action (crates/ptyro-tui/src/keymap.rs ACTIONS)"))
    }
}

#[test]
fn every_cli_command_has_a_tui_path_or_a_waiver() {
    let commands = commands();
    let rows = rows();
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    let mut gaps = Vec::new();
    for (cmd, cell) in &rows {
        if !seen.insert(cmd.clone()) {
            problems.push(format!("`{cmd}`: listed twice"));
        }
        if !commands.contains(cmd) {
            problems.push(format!("`{cmd}`: no such command (renamed or removed?)"));
        }
        match classify(cell) {
            Ok(Path::Gap) => gaps.push(format!("`{cmd}`: {cell}")),
            Ok(_) => {}
            Err(e) => problems.push(format!("`{cmd}`: {e}")),
        }
    }
    for cmd in commands.iter().filter(|c| !seen.contains(*c)) {
        problems.push(format!(
            "`{cmd}`: not in docs/tui-parity.md. Give it a TUI path (an action in the palette), \
             or a row saying `waived: why` (SPEC §1.3)"
        ));
    }
    let strict = std::env::var_os("PTYRO_PARITY_STRICT").is_none_or(|v| v != "0");
    if strict {
        problems.extend(gaps.iter().map(|g| format!("gap {g}")));
    } else if !gaps.is_empty() {
        eprintln!("{} TUI gap(s) left:\n  {}", gaps.len(), gaps.join("\n  "));
    }
    assert!(problems.is_empty(), "TUI parity (docs/tui-parity.md):\n  {}", problems.join("\n  "));
}

#[test]
fn cells_are_classified() {
    assert_eq!(classify("`worktree.new`, then type a name"), Ok(Path::Action));
    assert_eq!(classify("shown: the sidebar's diffstat"), Ok(Path::Shown));
    assert_eq!(classify("waived: run by programs inside a session"), Ok(Path::Waived));
    assert_eq!(classify("gap: UX1.21 (profiles)"), Ok(Path::Gap));
    assert!(classify("gap: later").is_err());
    assert!(classify("waived:").is_err(), "a waiver says why");
    assert!(classify("`no.such.action`").unwrap_err().contains("not a TUI action"));
    assert!(classify("the inspector").is_err());
}
