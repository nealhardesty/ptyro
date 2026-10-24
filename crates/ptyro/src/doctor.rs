//! `ptyro doctor` and `ptyro profile doctor` (M5.6): everything that makes ptyro misbehave
//! without saying so, checked in one place. Nothing here changes anything, and secrets are never
//! read: logins are asked of each tool's own status command (`claude auth status`,
//! `codex login status`, `gh auth status`) under the profile's config dir.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ptyro_agents::profile;
use ptyro_core::config::{Config, expand_home};
use ptyro_core::model::DEFAULT_PROFILE;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{PROTO, Reply, Request};
use serde::Serialize;

use crate::cli::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    /// Worth knowing, nothing wrong.
    Note,
    /// Probably wrong, or will be.
    Warn,
    /// Broken.
    Problem,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub section: String,
    pub name: String,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
pub(crate) struct Report(pub Vec<Check>);

impl Report {
    fn add(&mut self, section: &str, name: &str, level: Level, text: impl Into<String>) {
        self.0.push(Check { section: section.into(), name: name.into(), level, text: text.into() });
    }
}

/// Agent versions ptyro was verified against (SPEC §8, §20).
const TESTED: &[(&str, &str)] =
    &[("claude", "2.1.304"), ("codex", "0.143.0"), ("gemini", "0.64.0")];

/// Run a short command; `None` if it can't start or takes longer than `t`.
async fn run(argv: &[String], env: &[(String, PathBuf)], t: Duration) -> Option<(bool, String)> {
    let mut cmd = tokio::process::Command::new(argv.first()?);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(t, cmd.output()).await.ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text.trim().to_owned()))
}

pub fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        return Some(PathBuf::from(prog)).filter(|p| p.is_file());
    }
    which_in(prog, &std::env::var("PATH").unwrap_or_default())
}

fn which_in(prog: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path).map(|d| d.join(prog)).find(|p| p.is_file())
}

fn first_version(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == 'v')
        .find(|w| w.split('.').count() >= 2 && w.split('.').all(|p| p.parse::<u32>().is_ok()))
        .map(str::to_owned)
}

pub async fn run_cmd(ctx: &Ctx, quick: bool, json: bool) -> Result<i32, Error> {
    let config_path = Config::path(&ctx.paths);
    let mut r = Report::default();
    let config = match Config::load(&config_path) {
        Ok(c) => {
            r.add("ptyro", "config", Level::Ok, format!("{} is valid", config_path.display()));
            if let Some(mode) = crate::config::too_open(&config_path) {
                r.add(
                    "ptyro",
                    "config mode",
                    Level::Warn,
                    format!(
                        "{} is {mode:03o}: other users can read it, and it can hold secrets \
                         (`chmod 600 {}`)",
                        config_path.display(),
                        config_path.display()
                    ),
                );
            }
            c
        }
        Err(e) => {
            r.add("ptyro", "config", Level::Problem, format!("{}: {e}", config_path.display()));
            Config::default()
        }
    };
    r.add(
        "ptyro",
        "version",
        Level::Ok,
        format!(
            "ptyro {} at {}",
            env!("CARGO_PKG_VERSION"),
            std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
        ),
    );
    daemon(ctx, &config, quick, &mut r).await;
    remote(ctx, &config, &mut r);
    scratchpad(ctx, &config, &mut r);
    terminal(&mut r).await;
    git(&mut r).await;
    agents(&config, &mut r).await;
    profiles(ctx, &config, None, &mut r).await;
    print(&r, json);
    Ok(exit_code(&r))
}

/// The scratchpad (NOTE1): the file can be written, and there is a terminal editor for its pane.
fn scratchpad(ctx: &Ctx, config: &Config, r: &mut Report) {
    let path = config.scratchpad_path(&ctx.paths.data);
    let pad = ptyro_core::scratchpad::Pad::new(
        path.clone(),
        ctx.paths.data.join("scratchpad.history"),
        config.scratchpad.keep,
    );
    match pad.ensure() {
        Ok(()) if std::fs::OpenOptions::new().append(true).open(&path).is_ok() => {
            r.add(
                "scratchpad",
                "file",
                Level::Ok,
                format!("{} (also $PTYRO_SCRATCHPAD)", path.display()),
            );
        }
        Ok(()) => r.add(
            "scratchpad",
            "file",
            Level::Problem,
            format!(
                "{} isn't writable (`chmod u+w {}`, or set [scratchpad] path)",
                path.display(),
                path.display()
            ),
        ),
        Err(e) => r.add(
            "scratchpad",
            "file",
            Level::Problem,
            format!("{}: {e} (set [scratchpad] path in `ptyro config edit`)", path.display()),
        ),
    }
    match config.scratchpad_editor(true) {
        Some(cmd) if which(&cmd[0]).is_some() || cmd[0].contains('/') => {
            r.add("scratchpad", "editor", Level::Ok, cmd.join(" "));
        }
        Some(cmd) => r.add(
            "scratchpad",
            "editor",
            Level::Warn,
            format!("{} not found: the pane can't start it (install it, or set [scratchpad] editor)", cmd[0]),
        ),
        None => r.add(
            "scratchpad",
            "editor",
            Level::Warn,
            "no terminal editor found (install nvim, vim or nano, or set [scratchpad] editor): `C-q ;` can't open the pad",
        ),
    }
}

/// `ptyro serve` (WEB1.7): plain HTTP where tokens would cross a network, and its certificate.
fn remote(ctx: &Ctx, config: &Config, r: &mut Report) {
    let listen = config.remote.listen.parse::<std::net::SocketAddr>().ok();
    let off_loopback = config.remote.local || listen.is_some_and(|a| !a.ip().is_loopback());
    if config.remote.insecure && off_loopback {
        r.add(
            "remote",
            "plain HTTP",
            Level::Warn,
            "`ptyro serve` speaks plain HTTP off this machine (`[remote] insecure`): device tokens \
             cross the network in clear. Without it ptyro speaks HTTPS (`ptyro config edit`)",
        );
    }
    let Some(ends) = crate::tls::expires(&crate::tls::dir(&ctx.paths.data)) else { return };
    let days = (ends - time::OffsetDateTime::now_utc()).whole_days();
    let (level, text) = if days < 0 {
        (Level::Problem, "ptyro serve's certificate has ended: `ptyro serve` makes a new one when it starts (`ptyro daemon restart` restarts a managed one)".to_owned())
    } else if days < crate::tls::RENEW_DAYS {
        (
            Level::Warn,
            format!(
                "ptyro serve's certificate ends in {days} days and wasn't renewed: `ptyro serve` renews it when it starts (`ptyro daemon restart` restarts a managed one)"
            ),
        )
    } else {
        (Level::Ok, format!("ptyro serve's HTTPS certificate is good for {days} more days"))
    };
    r.add("remote", "certificate", level, text);
}

/// `ptyro profile doctor [NAME]`.
pub async fn profile_doctor(ctx: &Ctx, only: Option<String>, json: bool) -> Result<i32, Error> {
    let config =
        Config::load(&Config::path(&ctx.paths)).map_err(|e| Error::invalid(e.to_string()))?;
    let mut r = Report::default();
    profiles(ctx, &config, only, &mut r).await;
    print(&r, json);
    Ok(exit_code(&r))
}

fn exit_code(r: &Report) -> i32 {
    if r.0.iter().any(|c| c.level == Level::Problem) { 1 } else { 0 }
}

pub(crate) fn print_checks(r: &Report) {
    for c in &r.0 {
        let mark = match c.level {
            Level::Ok => "✓",
            Level::Note => "·",
            Level::Warn => "!",
            Level::Problem => "✗",
        };
        eprintln!("  {mark} {:<12} {}", c.name, c.text);
    }
}

fn print(r: &Report, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(&r.0).unwrap_or_default());
        return;
    }
    let mut section = "";
    for c in &r.0 {
        if c.section != section {
            section = &c.section;
            println!("{section}");
        }
        let mark = match c.level {
            Level::Ok => "✓",
            Level::Note => "·",
            Level::Warn => "!",
            Level::Problem => "✗",
        };
        println!("  {mark} {:<12} {}", c.name, c.text);
    }
    let (w, p) = (
        r.0.iter().filter(|c| c.level == Level::Warn).count(),
        r.0.iter().filter(|c| c.level == Level::Problem).count(),
    );
    println!();
    println!(
        "{}",
        match (w, p) {
            (0, 0) => "all good".to_owned(),
            _ => format!("{p} problem(s), {w} warning(s)"),
        }
    );
}

async fn daemon(ctx: &Ctx, config: &Config, quick: bool, r: &mut Report) {
    const S: &str = "daemon";
    // The socket directory must be private (SPEC §17).
    if let Some(dir) = ctx.paths.socket.parent() {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(dir) {
            Ok(m) if m.mode() & 0o077 != 0 => r.add(
                S,
                "socket dir",
                Level::Problem,
                format!("{} is mode {:o}; it must be 0700", dir.display(), m.mode() & 0o777),
            ),
            Ok(m) if m.uid() != rustix::process::getuid().as_raw() => {
                r.add(S, "socket dir", Level::Problem, format!("{} isn't yours", dir.display()))
            }
            Ok(_) => r.add(S, "socket dir", Level::Ok, format!("{} (private)", dir.display())),
            Err(_) => {}
        }
    }
    let c = match ctx.connect(!quick).await {
        Ok(c) => c,
        Err(e) if e.code == ErrorCode::DaemonUnavailable => {
            r.add(S, "running", Level::Note, "not running (it starts on demand)");
            return;
        }
        Err(e) => {
            r.add(S, "running", Level::Problem, e.message);
            return;
        }
    };
    if let Ok(Reply::Status { status }) = c.request(Request::Status).await {
        let same = status.version == env!("CARGO_PKG_VERSION") && status.proto == PROTO;
        r.add(
            S,
            "running",
            if same { Level::Ok } else { Level::Warn },
            format!(
                "pid {}, version {} (protocol {}.{}){}",
                status.pid,
                status.version,
                status.proto.major,
                status.proto.minor,
                if same {
                    ""
                } else {
                    "; not this ptyro: `ptyro daemon upgrade` (sessions keep running)"
                }
            ),
        );
    }
    let Ok(Reply::Doctor { report }) = c.request(Request::Doctor).await else { return };
    for v in &report.leaked_vars {
        r.add(
            S,
            "environment",
            Level::Note,
            format!("{v} is set in the daemon's environment; sessions don't inherit it (profiles set it)"),
        );
    }
    // A daemon started by a service manager can have a short PATH: agents then "don't exist".
    for agent in ptyro_agents::names(config) {
        let Some(argv) = ptyro_agents::command(config, &agent) else { continue };
        if which(&argv[0]).is_some()
            && !argv[0].contains('/')
            && which_in(&argv[0], &report.path).is_none()
        {
            r.add(
                S,
                "PATH",
                Level::Problem,
                format!("{} is on your PATH but not on the daemon's: sessions can't start it (reinstall the service, or set [agents.{agent}] command to a full path)", argv[0]),
            );
        }
    }
    for w in &report.warnings {
        r.add(S, "noticed", Level::Warn, w.clone());
    }
    if quick {
        return;
    }
    // The hook self-test: the fake agent through each per-launch integration.
    for adapter in ["claude", "codex", "opencode"] {
        match c.request(Request::SelfTest { adapter: adapter.into() }).await {
            Ok(Reply::SelfTest { result }) => r.add(
                "integrations",
                adapter,
                Level::Ok,
                format!("self-test passed: {}", result.seen.join(" → ")),
            ),
            Ok(_) => {}
            Err(e) => r.add("integrations", adapter, Level::Problem, e.message),
        }
    }
}

pub(crate) async fn terminal(r: &mut Report) {
    const S: &str = "terminal";
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let term = var("TERM").unwrap_or_else(|| "(unset)".into());
    let program = var("TERM_PROGRAM").map(|p| format!(" in {p}")).unwrap_or_default();
    r.add(S, "TERM", Level::Ok, format!("{term}{program}"));
    match var("COLORTERM").as_deref() {
        Some("truecolor" | "24bit") => r.add(S, "colour", Level::Ok, "truecolor"),
        _ => r.add(
            S,
            "colour",
            Level::Note,
            "COLORTERM doesn't say truecolor; colours may be reduced",
        ),
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) && var("TMUX").is_none() {
        match crossterm::terminal::supports_keyboard_enhancement() {
            Ok(true) => r.add(S, "keys", Level::Ok, "kitty keyboard protocol (Shift+Enter reaches agents)"),
            Ok(false) => r.add(
                S,
                "keys",
                Level::Note,
                "no kitty keyboard protocol: Shift+Enter can't be told from Enter (Ghostty, kitty, WezTerm, foot have it)",
            ),
            Err(_) => {}
        }
    }
    if var("TMUX").is_some() {
        let opt = |name: &'static str| async move {
            run(
                &["tmux".into(), "show".into(), "-gv".into(), name.into()],
                &[],
                Duration::from_secs(2),
            )
            .await
            .map(|(_, v)| v)
        };
        for (name, want, why) in [
            (
                "allow-passthrough",
                "on",
                "notifications and clipboard escapes can't reach your terminal",
            ),
            ("extended-keys", "on", "Shift+Enter and other modified keys don't reach ptyro"),
            ("set-clipboard", "on", "copies don't reach your system clipboard"),
        ] {
            match opt(name).await.as_deref() {
                Some(v) if v == want || (name == "set-clipboard" && v == "external") => {
                    r.add("tmux", name, Level::Ok, v.to_owned())
                }
                Some(v) => r.add(
                    "tmux",
                    name,
                    Level::Warn,
                    format!("is {v}: {why}; add `set -g {name} {want}` to ~/.tmux.conf"),
                ),
                None => {}
            }
        }
    }
}

async fn git(r: &mut Report) {
    match run(&["git".into(), "--version".into()], &[], Duration::from_secs(3)).await {
        Some((_, out)) => {
            let v = first_version(&out).unwrap_or_default();
            let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
            let old = parts.as_slice() < [2, 40].as_slice();
            r.add(
                "git",
                "version",
                if old { Level::Warn } else { Level::Ok },
                if old {
                    format!("{v}: ptyro is tested with git 2.53; worktree features may be missing")
                } else {
                    v
                },
            );
        }
        None => {
            r.add("git", "version", Level::Problem, "git not found: projects and worktrees need it")
        }
    }
    if which("gh").is_none() {
        r.add("git", "gh", Level::Note, "gh not found: `wt new --pr` and PR features need it");
    }
}

pub(crate) async fn agents(config: &Config, r: &mut Report) {
    const S: &str = "agents";
    for agent in ptyro_agents::names(config) {
        let Some(argv) = ptyro_agents::command(config, &agent) else { continue };
        let configured = config.agents.contains_key(&agent);
        let Some(path) = which(&argv[0]) else {
            if configured {
                r.add(S, &agent, Level::Problem, format!("{} not found", argv[0]));
            }
            continue;
        };
        let adapter = ptyro_agents::adapter_name(config, &agent);
        // `fake-agent` and wrappers may not answer --version: only ask real CLIs.
        let version = if argv.len() == 1 {
            run(&[argv[0].clone(), "--version".into()], &[], Duration::from_secs(5))
                .await
                .and_then(|(_, out)| first_version(&out))
        } else {
            None
        };
        let tested = TESTED.iter().find(|(a, _)| *a == adapter).map(|(_, v)| *v);
        let text = match (&version, tested) {
            (Some(v), Some(t)) if v == t => format!("{v} ({})", path.display()),
            (Some(v), Some(t)) => format!("{v}, verified with {t} ({})", path.display()),
            (Some(v), None) => format!("{v} ({})", path.display()),
            (None, _) => format!("{} (adapter {adapter})", path.display()),
        };
        let level = match (&version, tested) {
            (Some(v), Some(t)) if v.split('.').next() != t.split('.').next() => Level::Warn,
            _ => Level::Ok,
        };
        r.add(S, &agent, level, text);
    }
    // Gemini CLI stopped serving unpaid / Google One accounts on 2026-06-18 (RESEARCH G3).
    let gemini_key = std::env::var_os("GEMINI_API_KEY").is_some()
        || std::env::var_os("GOOGLE_CLOUD_PROJECT").is_some()
        || config
            .profiles
            .values()
            .any(|p| p.gemini.as_ref().and_then(|g| g.def()).is_some_and(|d| d.api_key.is_some()));
    if which("gemini").is_some() && !gemini_key {
        r.add(
            S,
            "gemini",
            Level::Note,
            "Gemini CLI no longer serves consumer Google accounts (since 2026-06-18); use the Antigravity CLI (`agy`) with a personal account",
        );
    }
}

/// Logged in, and as whom, from the tool's own status command.
async fn login(agent: &str, dir: &Path, custom: bool) -> Option<(Level, String)> {
    let var = ptyro_agents::dir_var(agent)?;
    // The default location stays implicit: `CLAUDE_CONFIG_DIR=~/.claude` would move
    // `.claude.json` and look like a different account.
    let env: Vec<(String, PathBuf)> =
        if custom { vec![(var.to_owned(), dir.to_path_buf())] } else { Vec::new() };
    let t = Duration::from_secs(8);
    match agent {
        "claude" => {
            let (_, out) =
                run(&["claude".into(), "auth".into(), "status".into(), "--json".into()], &env, t)
                    .await?;
            let v: serde_json::Value = serde_json::from_str(&out).ok()?;
            if v["loggedIn"].as_bool() == Some(true) {
                let who = v["email"].as_str().unwrap_or("?");
                let org = v["orgName"].as_str().map(|o| format!(", {o}")).unwrap_or_default();
                let plan =
                    v["subscriptionType"].as_str().map(|p| format!(" ({p})")).unwrap_or_default();
                Some((Level::Ok, format!("logged in as {who}{org}{plan}")))
            } else {
                Some((Level::Warn, "not logged in (`ptyro profile login <profile> claude`)".into()))
            }
        }
        "codex" => {
            let (ok, out) =
                run(&["codex".into(), "login".into(), "status".into()], &env, t).await?;
            let line = out.lines().next().unwrap_or("").to_owned();
            Some(if ok {
                (Level::Ok, line)
            } else {
                (Level::Warn, format!("not logged in: {line}"))
            })
        }
        "gh" => {
            let (ok, out) = run(&["gh".into(), "auth".into(), "status".into()], &env, t).await?;
            let who = out
                .lines()
                .find(|l| l.contains("Logged in to"))
                .map(|l| l.trim().trim_start_matches('✓').trim().to_owned());
            Some(if ok {
                (Level::Ok, who.unwrap_or_else(|| "logged in".into()))
            } else {
                (Level::Warn, "not logged in (`ptyro profile login <profile> gh`)".into())
            })
        }
        _ => None,
    }
}

async fn profiles(ctx: &Ctx, config: &Config, only: Option<String>, r: &mut Report) {
    let root = ctx.paths.data.join("profiles");
    for name in profile::names(config) {
        if only.as_ref().is_some_and(|o| *o != name) {
            continue;
        }
        let section = format!("profile {name}");
        let info = profile::info(config, &name, &root);
        for (agent, dir) in &info.dirs {
            let prog = if agent == "gh" { "gh" } else { agent.as_str() };
            if which(prog).is_none() {
                continue;
            }
            let custom = ptyro_agents::default_dir(agent).as_deref() != Some(dir.as_path());
            let shown = dir.display().to_string();
            if !dir.exists() {
                r.add(
                    &section,
                    agent,
                    Level::Note,
                    format!(
                        "{shown}: no dir yet (`ptyro profile login {name} {agent}` creates it)"
                    ),
                );
                continue;
            }
            let (level, state) = match login(agent, dir, custom).await {
                Some(x) => x,
                None => {
                    let cred = dir.join(crate::profiles::credential_file(agent));
                    if cred.exists() {
                        (
                            Level::Ok,
                            format!(
                                "logged in ({} present)",
                                crate::profiles::credential_file(agent)
                            ),
                        )
                    } else {
                        (
                            Level::Note,
                            format!(
                                "not logged in? (no {})",
                                crate::profiles::credential_file(agent)
                            ),
                        )
                    }
                }
            };
            r.add(&section, agent, level, format!("{state} · {shown}"));
        }
        match &info.git_email {
            Some(e) => r.add(&section, "git", Level::Ok, e.clone()),
            None if info.defined && name != DEFAULT_PROFILE => {
                r.add(&section, "git", Level::Note, "inherits your global git identity")
            }
            None => {}
        }
        if let Some(def) = config.profiles.get(&name)
            && let Some(key) = def.git.as_ref().and_then(|g| g.ssh_key.as_deref())
            && !expand_home(key).exists()
        {
            r.add(&section, "ssh key", Level::Problem, format!("{key} does not exist"));
        }
    }
    // Pins that contradict the match rules, and pins to profiles that are gone.
    if let Ok(c) = ctx.connect(false).await
        && let Ok(Reply::Projects { projects }) = c.request(Request::ProjectList).await
    {
        for p in projects {
            let remotes: Vec<String> = p.remote_url.iter().cloned().collect();
            let claim = profile::claimants(config, &remotes, &p.root);
            if let Some(pin) = &p.pin {
                if !claim.is_empty() && !claim.contains(pin) {
                    r.add(
                        "projects",
                        &p.slug,
                        Level::Problem,
                        format!("pinned to {pin} but matches {}", claim.join(", ")),
                    );
                }
                if !profile::exists(config, pin) {
                    r.add(
                        "projects",
                        &p.slug,
                        Level::Problem,
                        format!("pinned to {pin}, which isn't defined"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert_eq!(super::first_version("2.1.304 (Claude Code)").as_deref(), Some("2.1.304"));
        assert_eq!(super::first_version("codex-cli 0.143.0").as_deref(), Some("0.143.0"));
        assert_eq!(super::first_version("git version 2.53.0").as_deref(), Some("2.53.0"));
        assert_eq!(super::first_version("gemini v0.64.0").as_deref(), Some("0.64.0"));
    }
}
