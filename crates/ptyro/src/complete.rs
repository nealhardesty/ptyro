//! Shell completions (M5.4). `ptyro completions <shell>` prints a small registration script; the
//! shell then asks `ptyro` itself for every completion, so names of live sessions, worktrees,
//! projects, workspaces and profiles complete as they are right now. Without a daemon only the
//! config-based ones (agents, profiles) complete; nothing ever starts a daemon.

use std::time::Duration;

use clap_complete::CompletionCandidate;
use clap_complete::env::{EnvCompleter, Shells};
use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_protocol::{Outcome, Reply, Request, ServerMsg};

/// The variable the registration script sets when it calls back (not `COMPLETE`: too generic).
pub const VAR: &str = "PTYRO_COMPLETE";

/// Answer the shell's question and exit, if this run is one.
pub fn handle(factory: fn() -> clap::Command) {
    clap_complete::CompleteEnv::with_factory(factory).var(VAR).complete();
}

/// The registration script for `shell`.
pub fn registration(shell: &str) -> Result<String, String> {
    let shells = Shells::builtins();
    let c: &dyn EnvCompleter = shells
        .completer(shell)
        .ok_or_else(|| format!("unknown shell {shell:?} (bash, zsh, fish, elvish, powershell)"))?;
    let exe = std::env::current_exe()
        .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned());
    let mut buf = Vec::new();
    c.write_registration(VAR, "ptyro", "ptyro", &exe, &mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn ask(req: Request) -> Option<Reply> {
    let paths = Paths::from_env().ok()?;
    match crate::relay::request_at(&paths.socket, req, Duration::from_millis(400))? {
        ServerMsg::Response { outcome: Outcome::Ok { reply }, .. } => Some(reply),
        _ => None,
    }
}

fn candidate(value: String, help: Option<String>) -> CompletionCandidate {
    CompletionCandidate::new(value).help(help.map(Into::into))
}

pub fn sessions() -> Vec<CompletionCandidate> {
    let Some(Reply::Sessions { sessions }) = ask(Request::List) else { return Vec::new() };
    let mut out: Vec<CompletionCandidate> = sessions
        .into_iter()
        .map(|s| {
            let what =
                s.status.as_ref().map_or_else(|| s.state.label(), |r| r.status.label().to_owned());
            candidate(
                s.name,
                Some(format!(
                    "{} · {what}",
                    s.agent.unwrap_or_else(|| format!("{:?}", s.kind).to_lowercase())
                )),
            )
        })
        .collect();
    for sel in
        ["@all", "@live", "@exited", "@working", "@blocked", "@idle", "@done", "@focused", "@last"]
    {
        out.push(candidate(sel.into(), None));
    }
    out
}

pub fn worktrees() -> Vec<CompletionCandidate> {
    let Some(Reply::Worktrees { worktrees }) = ask(Request::WorktreeList { project: None }) else {
        return Vec::new();
    };
    worktrees.into_iter().map(|w| candidate(w.qualified(), w.branch.clone())).collect()
}

pub fn projects() -> Vec<CompletionCandidate> {
    let Some(Reply::Projects { projects }) = ask(Request::ProjectList) else { return Vec::new() };
    projects.into_iter().map(|p| candidate(p.slug, Some(p.root.display().to_string()))).collect()
}

pub fn workspaces() -> Vec<CompletionCandidate> {
    let Some(Reply::Workspaces { workspaces }) = ask(Request::WorkspaceList) else {
        return Vec::new();
    };
    workspaces.into_iter().map(|w| candidate(w.name, Some(w.profile))).collect()
}

fn config() -> Config {
    Paths::from_env().ok().and_then(|p| Config::load(&Config::path(&p)).ok()).unwrap_or_default()
}

pub fn profiles() -> Vec<CompletionCandidate> {
    ptyro_agents::profile::names(&config()).into_iter().map(|p| candidate(p, None)).collect()
}

/// Agents, and `agent@profile` for each profile.
pub fn agents() -> Vec<CompletionCandidate> {
    let c = config();
    let profiles = ptyro_agents::profile::names(&c);
    let mut out = Vec::new();
    for a in ptyro_agents::names(&c) {
        for p in &profiles {
            out.push(candidate(format!("{a}@{p}"), None));
        }
        out.insert(0, candidate(a, None));
    }
    out
}
