//! `ptyro init` (SPEC §12.3, M5.5): first-run setup in a few questions.
//!
//! 1. What's here: the terminal (and tmux advice), the agent CLIs and their versions.
//! 2. Existing multi-account setups (aliases, functions, exports, `~/.claude-*` dirs, git
//!    `includeIf`), offered as profiles, matched to repositories by their paths.
//! 3. Prefix key, worktree location, desktop notifications.
//! 4. The config file: only keys you don't have yet are added (comments and layout kept), shown
//!    before anything is written.
//! 5. Repositories under the discover roots, to register as projects.
//! 6. Completions, the login service and the tour: offered, never done without a yes.
//!
//! `--yes` accepts every suggestion and skips the questions that change things outside ptyro
//! (your rc file, the service); `--dry-run` shows the config it would write and stops.

use std::io::{BufRead, IsTerminal, Write};

use ptyro_agents::detect::detect;
use ptyro_core::Error;
use ptyro_core::config::{Config, expand_home};
use ptyro_protocol::{Reply, Request};

use crate::cli::Ctx;

struct Asker {
    yes: bool,
}

impl Asker {
    fn line(&self, q: &str, default: &str) -> String {
        if self.yes {
            return default.to_owned();
        }
        eprint!("{q} [{default}] ");
        let _ = std::io::stderr().flush();
        let mut s = String::new();
        let _ = std::io::stdin().lock().read_line(&mut s);
        let s = s.trim();
        if s.is_empty() { default.to_owned() } else { s.to_owned() }
    }

    fn yes_no(&self, q: &str, default: bool) -> bool {
        let a = self.line(q, if default { "Y/n" } else { "y/N" });
        match a.as_str() {
            "Y/n" => true,
            "y/N" => false,
            a => matches!(a, "y" | "Y" | "yes"),
        }
    }

    /// Questions that would change things outside ptyro: never answered by `--yes`.
    fn outside(&self, q: &str) -> bool {
        !self.yes && self.yes_no(q, false)
    }
}

fn section(title: &str) {
    eprintln!();
    eprintln!("── {title} ──");
}

/// `1,3-5`, `all`, `none` → indices.
pub fn pick(answer: &str, n: usize) -> Vec<usize> {
    match answer.trim() {
        "all" | "a" => (0..n).collect(),
        "" | "none" | "n" => Vec::new(),
        s => {
            let mut out = Vec::new();
            for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
                let range = part.split_once('-');
                let (a, b) = match range {
                    Some((a, b)) => (a.parse::<usize>().ok(), b.parse::<usize>().ok()),
                    None => (part.parse().ok(), part.parse().ok()),
                };
                if let (Some(a), Some(b)) = (a, b) {
                    for i in a.min(b)..=a.max(b) {
                        if (1..=n).contains(&i) && !out.contains(&(i - 1)) {
                            out.push(i - 1);
                        }
                    }
                }
            }
            out
        }
    }
}

/// Add `key = value` under `table` unless it's already set.
fn set_default(
    doc: &mut toml_edit::DocumentMut,
    table: &str,
    key: &str,
    value: toml_edit::Item,
) -> bool {
    let t = doc.entry(table).or_insert(toml_edit::table());
    let Some(t) = t.as_table_mut() else { return false };
    if t.contains_key(key) {
        return false;
    }
    t.insert(key, value);
    true
}

pub async fn run(ctx: &Ctx, yes: bool, dry_run: bool) -> Result<i32, Error> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !interactive && !yes && !dry_run {
        return Err(Error::invalid(
            "`ptyro init` asks questions: run it in a terminal, or pass --yes",
        ));
    }
    let ask = Asker { yes: yes || dry_run };
    let path = Config::path(&ctx.paths);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut =
        existing.parse().map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    let current = Config::parse(&existing).unwrap_or_default();
    eprintln!("ptyro init: a few questions, then your config. Nothing changes until you say so.");

    section("your terminal");
    let mut r = crate::doctor::Report::default();
    crate::doctor::terminal(&mut r).await;
    crate::doctor::print_checks(&r);

    section("agents");
    let mut r = crate::doctor::Report::default();
    crate::doctor::agents(&current, &mut r).await;
    if r.0.is_empty() {
        eprintln!(
            "  none found on PATH (claude, codex, gemini, opencode, agy); install one, then `ptyro init` again"
        );
    }
    crate::doctor::print_checks(&r);

    section("accounts");
    let home = expand_home("~");
    let suggestions: Vec<_> =
        detect(&home).into_iter().filter(|s| !current.profiles.contains_key(&s.profile)).collect();
    let mut profiles_toml = String::new();
    if suggestions.is_empty() {
        eprintln!(
            "  no separate accounts found: everything runs as `default` (add profiles later with `ptyro profile new`)"
        );
    }
    for s in &suggestions {
        eprintln!(
            "  profile {:<10} {}",
            s.profile,
            s.dirs.iter().map(|(a, d)| format!("{a}={d}")).collect::<Vec<_>>().join(" ")
        );
        for src in &s.sources {
            eprintln!("    from {src}");
        }
        if ask.yes_no(&format!("  add profile {}?", s.profile), true) {
            profiles_toml.push_str(&s.to_toml());
            profiles_toml.push('\n');
        }
    }

    section("preferences");
    let prefix = ask.line(
        "  prefix keys, comma-separated (C-q, C-], C-Space)",
        &current.ui.prefix.keys().join(", "),
    );
    let prefix: Vec<String> =
        prefix.split(',').map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()).collect();
    if cfg!(target_os = "macos") && prefix == ["C-Space"] {
        eprintln!(
            "  note: macOS uses Ctrl-Space for input sources; free it in Keyboard settings, or pick another"
        );
    }
    let location = ask.line("  where worktrees go (data, sibling, in-repo)", "data");
    let desktop = ask.yes_no("  desktop notifications when an agent needs you?", false);
    let phone = !current.remote.serve
        && ask.outside(
            "  run the web app for your phone? (the daemon serves it on a port and keeps it up; \
             you pair a phone next)",
        );

    // ── the config ──
    let mut changes: Vec<String> = Vec::new();
    if prefix != Config::default().ui.prefix.keys() {
        let value = match prefix.as_slice() {
            [one] => toml_edit::value(one.as_str()),
            many => toml_edit::value(many.iter().map(String::as_str).collect::<toml_edit::Array>()),
        };
        if set_default(&mut doc, "ui", "prefix", value) {
            changes.push(format!("ui.prefix = {prefix:?}"));
        }
    }
    if !matches!(location.as_str(), "data" | "sibling" | "in-repo") {
        return Err(Error::invalid(format!("unknown worktree location {location:?}")));
    }
    if location != "data"
        && set_default(&mut doc, "worktrees", "location", toml_edit::value(location.clone()))
    {
        changes.push(format!("worktrees.location = {location:?}"));
    }
    if desktop {
        let mut arr = toml_edit::Array::new();
        for c in ["inbox", "osc", "bell", "desktop"] {
            arr.push(c);
        }
        if set_default(&mut doc, "notifications", "channels", toml_edit::value(arr)) {
            changes.push("notifications.channels += desktop".into());
        }
    }
    let roots: Vec<String> = Config::default()
        .discover
        .roots
        .into_iter()
        .chain(["~/work", "~/git"].map(String::from))
        .filter(|r| expand_home(r).is_dir())
        .collect();
    if current.discover.roots == Config::default().discover.roots && !roots.is_empty() {
        let mut arr = toml_edit::Array::new();
        for r in &roots {
            arr.push(r.as_str());
        }
        if set_default(&mut doc, "discover", "roots", toml_edit::value(arr)) {
            changes.push(format!("discover.roots = {roots:?}"));
        }
    }
    if !profiles_toml.is_empty() {
        let extra: toml_edit::DocumentMut =
            profiles_toml.parse().map_err(|e| Error::internal(format!("profiles: {e}")))?;
        // `[profiles.work]` tables, added next to any you have.
        if let Some(src) = extra.get("profiles").and_then(toml_edit::Item::as_table) {
            let profiles = doc.entry("profiles").or_insert(toml_edit::table());
            if let Some(dst) = profiles.as_table_mut() {
                dst.set_implicit(true);
                for (name, t) in src.iter() {
                    if !dst.contains_key(name) {
                        dst.insert(name, t.clone());
                        changes.push(format!("profiles.{name}"));
                    }
                }
            }
        }
    }
    let new_text = doc.to_string();
    Config::parse(&new_text)
        .map_err(|e| Error::internal(format!("the config init built is invalid: {e}")))?;
    section(&format!("config: {}", path.display()));
    if changes.is_empty() {
        eprintln!("  nothing to add: your config already has all of it");
    } else {
        for c in &changes {
            eprintln!("  + {c}");
        }
    }
    if dry_run {
        print!("{new_text}");
        return Ok(0);
    }
    if !changes.is_empty() && ask.yes_no("  write it?", true) {
        crate::config::write_private(&path, &new_text)?;
        eprintln!("  wrote {}", path.display());
    }

    // ── the web app ──
    if phone {
        section("web app");
        for n in ptyro_core::config::set_serve(&path, true, None)? {
            eprintln!("  {n}");
        }
        eprintln!("  [remote] serve = true: the daemon runs it from now on");
    }

    // ── projects ──
    section("projects");
    let c = ctx.connect(true).await?;
    let _ = c.request(Request::ReloadConfig).await;
    let known: Vec<std::path::PathBuf> = match c.request(Request::ProjectList).await? {
        Reply::Projects { projects } => projects.into_iter().map(|p| p.root).collect(),
        _ => Vec::new(),
    };
    let repos: Vec<std::path::PathBuf> =
        match c.request(Request::Discover { refresh: true }).await? {
            Reply::Paths { paths } => paths.into_iter().filter(|p| !known.contains(p)).collect(),
            _ => Vec::new(),
        };
    if repos.is_empty() {
        eprintln!(
            "  no new repositories under your discover roots (register one with `ptyro project add`)"
        );
    } else {
        for (i, p) in repos.iter().enumerate().take(40) {
            eprintln!("  {:>2}. {}", i + 1, p.display());
        }
        if repos.len() > 40 {
            eprintln!("  … and {} more (`ptyro project discover`)", repos.len() - 40);
        }
        // `--yes` never registers a pile of repositories: that's a choice to make by hand.
        let answer = if yes {
            "none".to_owned()
        } else {
            ask.line("  register which? (1,3-5, all, none)", "none")
        };
        for i in pick(&answer, repos.len().min(40)) {
            match c.request(Request::ProjectAdd { path: repos[i].clone(), workspace: None }).await {
                Ok(Reply::Project { info, notes }) => {
                    eprintln!("  + {} ({})", info.slug, info.pin.as_deref().unwrap_or("default"));
                    for n in notes {
                        eprintln!("      {n}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("  ✗ {}: {}", repos[i].display(), e.message),
            }
        }
    }

    // ── extras ──
    section("extras");
    let shell = std::env::var("SHELL").unwrap_or_default();
    let (rc, line) = if shell.ends_with("zsh") {
        ("~/.zshrc", "source <(ptyro completions zsh)")
    } else if shell.ends_with("fish") {
        ("~/.config/fish/config.fish", "ptyro completions fish | source")
    } else {
        ("~/.bashrc", "source <(ptyro completions bash)")
    };
    if std::fs::read_to_string(expand_home(rc)).is_ok_and(|t| t.contains("ptyro completions")) {
        eprintln!("  ✓ completions already in {rc}");
    } else if ask.outside(&format!("  add completions to {rc} ({line})?")) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(expand_home(rc))
            .map_err(|e| Error::internal(format!("{rc}: {e}")))?;
        writeln!(f, "\n# ptyro completions (live session and worktree names)\n{line}")
            .map_err(|e| Error::internal(e.to_string()))?;
        eprintln!("  added; open a new shell to use them");
    } else {
        eprintln!("  completions: add `{line}` to {rc}");
    }
    // The web app is up once the daemon has picked the setting up.
    let mut web_url = None;
    if phone || current.remote.serve {
        for _ in 0..50 {
            if let Ok((w, _)) = crate::webapp::ask(ctx, None, None).await
                && w.up
            {
                web_url = w.url;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        match &web_url {
            Some(u) => eprintln!("  ✓ web app up at {u}"),
            None => eprintln!("  web app not up yet: `ptyro web` says why"),
        }
    }
    let at_login = if phone || current.remote.serve {
        "  start the daemon at login, so the web app is up after a reboot (a systemd/launchd service)?"
    } else {
        "  start the daemon at login (a systemd/launchd service)?"
    };
    if !crate::service::installed() && ask.outside(at_login) {
        crate::service::install(ctx, false)?;
    }
    eprintln!();
    eprintln!(
        "done. `ptyro` opens the TUI; `ptyro doctor` checks everything; `ptyro tour` shows the keys."
    );
    if phone
        && web_url.is_some()
        && interactive
        && ask.yes_no("show the QR code for your phone now?", true)
    {
        crate::remote::pair(ctx, "phone".into(), "control", None, false).await?;
    }
    if interactive
        && !yes
        && ask.yes_no("take the tour now? (a task, start to finish, with a pretend agent)", true)
    {
        drop(c);
        return crate::cli::run_tour(ctx).await;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks() {
        assert_eq!(super::pick("1,3-4", 5), [0, 2, 3]);
        assert_eq!(super::pick("all", 3), [0, 1, 2]);
        assert!(super::pick("none", 3).is_empty());
        assert_eq!(super::pick("9, 2", 3), [1]);
    }
}
