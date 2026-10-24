//! `ptyro profile …` (SPEC §10.10, M3.19). Profiles live in the user config file, so `new`, `rm`,
//! `rename` and `detect --apply` edit it in place (comments and layout kept, via `toml_edit`),
//! validate the result, and only then save it; the daemon reloads it by itself.

use std::path::PathBuf;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_agents::detect::{Suggestion, detect};
use ptyro_agents::profile::{self, ShareOutcome};
use ptyro_core::Error;
use ptyro_core::config::Config;
use ptyro_core::model::{DEFAULT_PROFILE, ProfileInfo};
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{Reply, Request};

use crate::cli::{Ctx, client_env, confirm, session_of, unexpected};

#[derive(Subcommand)]
pub enum ProfileCmd {
    /// List profiles
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Show a profile: its dirs, identity and the variables sessions get
    Show {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a profile in your config, adopting existing dirs (`--adopt claude=~/.claude-work`)
    New {
        name: String,
        /// AGENT=DIR for claude, codex, gemini, opencode or gh (repeatable)
        #[arg(long, value_parser = parse_adopt)]
        adopt: Vec<(String, String)>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        git_name: Option<String>,
        #[arg(long)]
        git_email: Option<String>,
        #[arg(long)]
        ssh_key: Option<String>,
        /// Remote globs that pin matching projects to it (`github.com:acme/*`, repeatable)
        #[arg(long = "match")]
        matches: Vec<String>,
    },
    /// Delete a profile from your config (refused while anything uses it)
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// Rename a profile everywhere: config, workspaces, pins, worktrees, sessions
    Rename { old: String, new: String },
    /// The environment its sessions get (secrets shown as <secret:NAME>)
    Env {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Log in: run the tool's own login flow under the profile (claude, codex, gemini, opencode, gh)
    Login {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: String,
        target: String,
    },
    /// Create the `share` symlinks (CLAUDE.md, skills, …) from the agents' default dirs
    Sync {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: String,
    },
    /// Find existing multi-account setups (aliases, exports, ~/.claude-* dirs, git includeIf)
    Detect {
        /// Add the profiles found to your config
        #[arg(long)]
        apply: bool,
    },
    /// Check each profile: dirs, logins, git identity, and pins that contradict match rules
    Doctor {
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn parse_adopt(s: &str) -> Result<(String, String), String> {
    let (a, d) = s.split_once('=').ok_or("expected AGENT=DIR, e.g. claude=~/.claude-work")?;
    if ptyro_agents::dir_var(a).is_none() {
        return Err(format!("{a:?} is not one of claude, codex, gemini, opencode, gh"));
    }
    Ok((a.to_owned(), d.to_owned()))
}

fn config_path(ctx: &Ctx) -> PathBuf {
    Config::path(&ctx.paths)
}

fn load(ctx: &Ctx) -> Result<Config, Error> {
    Config::load(&config_path(ctx)).map_err(|e| Error::invalid(e.to_string()))
}

/// Edit the config file, validate, then save atomically.
fn edit(
    ctx: &Ctx,
    f: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), Error>,
) -> Result<(), Error> {
    let path = config_path(ctx);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::internal(format!("{}: {e}", path.display()))),
    };
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    f(&mut doc)?;
    let new = doc.to_string();
    Config::parse(&new)
        .map_err(|e| Error::invalid(format!("the edited config would be invalid: {e}")))?;
    crate::config::write_private(&path, &new)
}

/// Have a running daemon pick up the edit now rather than on its file watch.
async fn reload(ctx: &Ctx) {
    if let Ok(c) = ctx.connect(false).await {
        let _ = c.request(Request::ReloadConfig).await;
    }
}

fn profiles_table(doc: &mut toml_edit::DocumentMut) -> Result<&mut toml_edit::Table, Error> {
    let item = doc.entry("profiles").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    item.as_table_mut().ok_or_else(|| Error::invalid("[profiles] in your config is not a table"))
}

fn inline(pairs: &[(&str, String)]) -> toml_edit::Item {
    let mut t = toml_edit::InlineTable::new();
    for (k, v) in pairs {
        t.insert(*k, v.as_str().into());
    }
    toml_edit::value(t)
}

async fn profiles(ctx: &Ctx) -> Result<Vec<ProfileInfo>, Error> {
    let c = ctx.connect(true).await?;
    match c.request(Request::ProfileList).await? {
        Reply::Profiles { profiles } => Ok(profiles),
        other => Err(unexpected(other)),
    }
}

pub(crate) fn credential_file(agent: &str) -> &'static str {
    match agent {
        "claude" => ".credentials.json",
        "codex" => "auth.json",
        "gemini" => "oauth_creds.json",
        "opencode" => "auth.json",
        "gh" => "hosts.yml",
        _ => "",
    }
}

pub async fn run(ctx: &Ctx, cmd: ProfileCmd) -> Result<i32, Error> {
    match cmd {
        ProfileCmd::Ls { json } => {
            let all = profiles(ctx).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
                return Ok(0);
            }
            println!(
                "{:<12} {:<10} {:<8} {:<8} {:<24} CLAUDE",
                "PROFILE", "LABEL", "COLOR", "ISOLATE", "GIT"
            );
            for p in &all {
                println!(
                    "{:<12} {:<10} {:<8} {:<8} {:<24} {}",
                    p.name,
                    p.label,
                    p.color.as_deref().unwrap_or("-"),
                    if p.isolate { "yes" } else { "no" },
                    p.git_email.as_deref().unwrap_or("-"),
                    p.dirs.get("claude").map_or("-".into(), |d| d.display().to_string())
                );
            }
            Ok(0)
        }
        ProfileCmd::Show { name, json } => {
            let all = profiles(ctx).await?;
            let p = all
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| Error::missing("profile", &name))?;
            if json {
                println!("{}", serde_json::to_string_pretty(p).unwrap_or_default());
                return Ok(0);
            }
            println!(
                "{} — label {:?}{}",
                p.name,
                p.label,
                p.color.as_deref().map(|c| format!(", colour {c}")).unwrap_or_default()
            );
            println!(
                "  isolate   {}",
                if p.isolate {
                    "yes: inherited credentials are removed"
                } else {
                    "no: inherits the daemon's credentials"
                }
            );
            for (agent, dir) in &p.dirs {
                println!("  {agent:<9} {}", dir.display());
            }
            if let Some(e) = &p.git_email {
                println!("  git       {e}");
            }
            if !p.matches.is_empty() {
                println!("  match     remotes {:?} paths {:?}", p.matches.remotes, p.matches.paths);
            }
            println!("  vars      {}", p.vars.join(" "));
            Ok(0)
        }
        ProfileCmd::New { name, adopt, label, color, git_name, git_email, ssh_key, matches } => {
            let new =
                NewProfile { name, adopt, label, color, git_name, git_email, ssh_key, matches };
            for line in create(ctx, new).await? {
                eprintln!("{line}");
            }
            Ok(0)
        }
        ProfileCmd::Rm { name, force } => {
            for line in remove(ctx, &name, force).await? {
                eprintln!("{line}");
            }
            Ok(0)
        }
        ProfileCmd::Rename { old, new } => {
            for line in rename(ctx, &old, &new).await? {
                eprintln!("{line}");
            }
            Ok(0)
        }
        ProfileCmd::Env { name, json } => {
            let c = ctx.connect(true).await?;
            let Reply::Env { vars } = c.request(Request::ProfileEnv { name }).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                let map: std::collections::BTreeMap<_, _> = vars.into_iter().collect();
                println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
                return Ok(0);
            }
            for (k, v) in vars {
                println!("{k}={v}");
            }
            Ok(0)
        }
        ProfileCmd::Login { name, target } => {
            let attaching = std::io::IsTerminal::is_terminal(&std::io::stdout());
            let (c, info) = login(ctx, &name, &target, attaching).await?;
            eprintln!(
                "logging in to {target} as profile {name} (ptyro never sees the credentials)"
            );
            if attaching {
                return crate::cli::attach(ctx, c, Some(info), false, false).await;
            }
            println!("{}", info.handle);
            Ok(0)
        }
        ProfileCmd::Sync { name } => {
            for line in sync(ctx, &name)? {
                eprintln!("{line}");
            }
            Ok(0)
        }
        ProfileCmd::Detect { apply } => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let found = detect(&home);
            if found.is_empty() {
                eprintln!(
                    "no multi-account setup found (aliases, exports, ~/.claude-* dirs, git includeIf)"
                );
                return Ok(0);
            }
            let config = load(ctx)?;
            for s in &found {
                let known = config.profiles.contains_key(&s.profile);
                println!(
                    "# from {}{}",
                    s.sources.join(", "),
                    if known { " — already defined" } else { "" }
                );
                println!("{}", s.to_toml());
            }
            if apply {
                let new =
                    found.iter().filter(|s| !config.profiles.contains_key(&s.profile)).count();
                if new == 0 {
                    eprintln!("nothing new to add");
                    return Ok(0);
                }
                if !confirm(&format!("Add {new} profile(s) to {}?", config_path(ctx).display())) {
                    return Ok(0);
                }
                for line in detect_apply(ctx, None).await? {
                    eprintln!("{line}");
                }
            }
            Ok(0)
        }
        ProfileCmd::Doctor { name, json } => crate::doctor::profile_doctor(ctx, name, json).await,
    }
}

/// What still refers to a profile.
async fn usage(ctx: &Ctx, name: &str) -> Result<Vec<String>, Error> {
    let c = ctx.connect(true).await?;
    let mut used = Vec::new();
    if let Reply::Workspaces { workspaces } = c.request(Request::WorkspaceList).await? {
        used.extend(
            workspaces
                .into_iter()
                .filter(|w| w.profile == name)
                .map(|w| format!("workspace {}", w.name)),
        );
    }
    if let Reply::Projects { projects } = c.request(Request::ProjectList).await? {
        used.extend(
            projects
                .into_iter()
                .filter(|p| p.pin.as_deref() == Some(name))
                .map(|p| format!("project {} (pin)", p.slug)),
        );
    }
    if let Reply::Worktrees { worktrees } =
        c.request(Request::WorktreeList { project: None }).await?
    {
        used.extend(
            worktrees
                .into_iter()
                .filter(|w| w.profile.as_deref() == Some(name))
                .map(|w| format!("worktree {}", w.qualified())),
        );
    }
    if let Reply::Sessions { sessions } = c.request(Request::List).await? {
        used.extend(
            sessions
                .into_iter()
                .filter(|s| s.profile == name && s.state.is_live())
                .map(|s| format!("session {}", s.name)),
        );
    }
    Ok(used)
}

// ── what the CLI and the web app both do ────────────────────────────────────────────────────

/// A new profile (`profile new`).
pub(crate) struct NewProfile {
    pub name: String,
    pub adopt: Vec<(String, String)>,
    pub label: Option<String>,
    pub color: Option<String>,
    pub git_name: Option<String>,
    pub git_email: Option<String>,
    pub ssh_key: Option<String>,
    pub matches: Vec<String>,
}

/// Add a profile to the config; the lines say what was made.
pub(crate) async fn create(ctx: &Ctx, new: NewProfile) -> Result<Vec<String>, Error> {
    let NewProfile { name, adopt, label, color, git_name, git_email, ssh_key, matches } = new;
    ptyro_core::names::validate(&name).map_err(Error::invalid)?;
    for (a, _) in &adopt {
        if ptyro_agents::dir_var(a).is_none() {
            return Err(Error::invalid(format!(
                "{a:?} is not one of claude, codex, gemini, opencode, gh"
            )));
        }
    }
    edit(ctx, |doc| {
        let t = profiles_table(doc)?;
        if t.contains_key(&name) {
            return Err(Error::conflict(format!("profile {name} exists")));
        }
        let mut p = toml_edit::Table::new();
        if let Some(l) = label {
            p["label"] = toml_edit::value(l);
        }
        if let Some(c) = color {
            p["color"] = toml_edit::value(c);
        }
        for (agent, dir) in &adopt {
            p[agent.as_str()] = toml_edit::value(dir.as_str());
        }
        let git: Vec<(&str, String)> =
            [("name", git_name), ("email", git_email), ("ssh_key", ssh_key)]
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect();
        if !git.is_empty() {
            p["git"] = inline(&git);
        }
        if !matches.is_empty() {
            let mut arr = toml_edit::Array::new();
            for m in &matches {
                arr.push(m.as_str());
            }
            let mut m = toml_edit::InlineTable::new();
            m.insert("remotes", toml_edit::Value::Array(arr));
            p["match"] = toml_edit::value(m);
        }
        t.insert(&name, toml_edit::Item::Table(p));
        Ok(())
    })?;
    reload(ctx).await;
    let config = load(ctx)?;
    let info = profile::info(&config, &name, &ctx.paths.data.join("profiles"));
    let mut out = vec![format!("profile {name} added to {}", config_path(ctx).display())];
    for (agent, dir) in &info.dirs {
        let adopted = adopt.iter().any(|(a, _)| a == agent);
        out.push(format!(
            "  {agent:<9} {}{}",
            dir.display(),
            if adopted { " (adopted)" } else { " (managed; `ptyro profile login` fills it)" }
        ));
    }
    Ok(out)
}

/// Delete a profile from the config (refused while anything uses it, unless `force`).
pub(crate) async fn remove(ctx: &Ctx, name: &str, force: bool) -> Result<Vec<String>, Error> {
    if name == DEFAULT_PROFILE {
        return Err(Error::invalid("the default profile always exists"));
    }
    let used = usage(ctx, name).await?;
    if !used.is_empty() && !force {
        return Err(Error::conflict(format!(
            "profile {name} is in use: {}; change those first or use --force",
            used.join(", ")
        )));
    }
    edit(ctx, |doc| {
        let t = profiles_table(doc)?;
        t.remove(name).ok_or_else(|| Error::missing("profile", name))?;
        Ok(())
    })?;
    reload(ctx).await;
    Ok(vec![format!("profile {name} removed from your config (its dirs are untouched)")])
}

/// Rename a profile everywhere: config, workspaces, pins, worktrees, sessions.
pub(crate) async fn rename(ctx: &Ctx, old: &str, new: &str) -> Result<Vec<String>, Error> {
    ptyro_core::names::validate(new).map_err(Error::invalid)?;
    if old == DEFAULT_PROFILE {
        return Err(Error::invalid("the default profile can't be renamed"));
    }
    edit(ctx, |doc| {
        let t = profiles_table(doc)?;
        if t.contains_key(new) {
            return Err(Error::conflict(format!("profile {new} exists")));
        }
        let item = t.remove(old).ok_or_else(|| Error::missing("profile", old))?;
        t.insert(new, item);
        Ok(())
    })?;
    reload(ctx).await;
    let c = ctx.connect(true).await?;
    c.request(Request::ProfileRename { old: old.to_owned(), new: new.to_owned() }).await?;
    Ok(vec![format!(
        "renamed {old} → {new} in your config, workspaces, pins, worktrees and sessions"
    )])
}

/// Log in to `target` under the profile: the tool's own flow, in a session. `size`: the
/// attaching terminal's.
pub(crate) async fn login(
    ctx: &Ctx,
    name: &str,
    target: &str,
    attaching: bool,
) -> Result<(ptyro_client::Client, ptyro_core::session::SessionInfo), Error> {
    let argv = ptyro_agents::login_argv(target).ok_or_else(|| {
        Error::invalid(format!(
            "can't log in to {target:?}: use claude, codex, gemini, opencode or gh"
        ))
    })?;
    let config = load(ctx)?;
    if !profile::exists(&config, name) {
        return Err(Error::missing("profile", name));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
    let c = ctx.connect(true).await?;
    let spec = SpawnSpec {
        argv,
        cwd: home,
        profile: Some(name.to_owned()),
        env: client_env(),
        size: if attaching { ptyro_tui::pane_size(&ctx.paths) } else { None },
        confirmed: true,
        ..SpawnSpec::default()
    };
    let info = session_of(c.request(Request::Spawn { spec }).await?)?;
    Ok((c, info))
}

/// Create the profile's `share` symlinks; one line each.
pub(crate) fn sync(ctx: &Ctx, name: &str) -> Result<Vec<String>, Error> {
    let config = load(ctx)?;
    if !config.profiles.contains_key(name) {
        return Err(Error::missing("profile", name));
    }
    let out = profile::sync_shares(&config, name, &ctx.paths.data.join("profiles"));
    if out.is_empty() {
        return Ok(vec![format!(
            "profile {name} shares nothing (add share = {{ claude = [\"CLAUDE.md\"] }})"
        )]);
    }
    Ok(out
        .into_iter()
        .map(|o| match o {
            ShareOutcome::Linked(p) => format!("linked   {}", p.display()),
            ShareOutcome::AlreadyLinked(p) => format!("ok       {}", p.display()),
            ShareOutcome::Exists(p) => {
                format!("kept     {} (exists; never overwritten)", p.display())
            }
            ShareOutcome::MissingSource(p) => format!("missing  {}", p.display()),
            ShareOutcome::Refused(why) => format!("refused  {why}"),
        })
        .collect())
}

/// Multi-account setups found on this machine, each with whether the config has it already.
pub(crate) fn detect_list(ctx: &Ctx) -> Result<Vec<(Suggestion, bool)>, Error> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let config = load(ctx)?;
    Ok(detect(&home)
        .into_iter()
        .map(|s| {
            let known = config.profiles.contains_key(&s.profile);
            (s, known)
        })
        .collect())
}

/// Add what `detect` found to the config: the named profiles, or every new one.
pub(crate) async fn detect_apply(ctx: &Ctx, only: Option<&[String]>) -> Result<Vec<String>, Error> {
    let found = detect_list(ctx)?;
    let new: Vec<&Suggestion> = found
        .iter()
        .filter(|(s, known)| !known && only.is_none_or(|o| o.contains(&s.profile)))
        .map(|(s, _)| s)
        .collect();
    if new.is_empty() {
        return Ok(vec!["nothing new to add".into()]);
    }
    edit(ctx, |doc| {
        for s in &new {
            let parsed: toml_edit::DocumentMut =
                s.to_toml().parse().map_err(|e| Error::internal(format!("{e}")))?;
            let table = parsed["profiles"][s.profile.as_str()].clone();
            profiles_table(doc)?.insert(&s.profile, table);
        }
        Ok(())
    })?;
    reload(ctx).await;
    let mut out = vec![format!("added {} profile(s)", new.len())];
    out.push("ptyro never edits your rc files, so the aliases keep working outside ptyro".into());
    Ok(out)
}
