//! `ptyro project`, `ws`, `wt` and `gc` (SPEC §12.2, M3).

use std::path::PathBuf;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_client::Client;
use ptyro_core::model::{ProjectInfo, RemoveOpts, WorktreeInfo, WorktreeSpec, WorktreeState};
use ptyro_core::session::SpawnSpec;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};

use crate::cli::{
    Ctx, NewArgs, age, check_attachable, client_env, confirm, current_dir, no_daemon, session_of,
    truncate, unexpected, with_confirmation,
};

#[derive(Subcommand)]
pub enum ProjectCmd {
    /// Register the git repository containing PATH (default: the current directory)
    Add {
        path: Option<PathBuf>,
        /// Add it to this workspace (default: main)
        #[arg(long)]
        workspace: Option<String>,
    },
    /// List projects
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Forget a project (its worktrees stay on disk)
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
    },
    /// Show a project with its worktrees and scripts
    Show {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Review the project's lifecycle scripts and trust them (SPEC §10.5)
    Trust {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        /// Trust without asking (scripts can see this repository's code anyway)
        #[arg(short, long)]
        yes: bool,
    },
    /// Stop trusting the project's scripts
    Untrust {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
    },
    /// Print the project's effective config (.ptyro.toml + .ptyro.local.toml)
    Config {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        /// Show where each value came from (.ptyro.toml, .ptyro.local.toml, or another tool's file)
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        json: bool,
    },
    /// Pin the project to a profile: its sessions run under it
    Pin {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
        #[arg(add = ArgValueCandidates::new(crate::complete::profiles))]
        profile: String,
    },
    /// Remove the pin
    Unpin {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
    },
    /// List git repositories under discover.roots
    Discover {
        /// Rescan instead of using the cache
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// What ptyro found the project needs (a setup script from its lockfile, .env files to
    /// copy into worktrees); --apply writes them to .ptyro.local.toml, --decline says no
    Suggest {
        project: Option<String>,
        #[arg(long, conflicts_with = "decline")]
        apply: bool,
        #[arg(long)]
        decline: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum WsCmd {
    /// List workspaces
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Create a workspace
    New {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        /// all | badge | mute
        #[arg(long)]
        notify: Option<String>,
    },
    /// Change a workspace's profile or notifications
    Set {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        notify: Option<String>,
    },
    /// Remove a workspace (its sessions move to main)
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
    },
    /// Rename a workspace
    Rename { name: String, new_name: String },
    /// Add a project to a workspace
    Add { name: String, project: String },
    /// Take a project out of a workspace
    Remove { name: String, project: String },
}

#[derive(Subcommand)]
pub enum WtCmd {
    /// Finish a worktree: commit what's left (its agent asked to; else its title is the message),
    /// then push and open a PR (default) or --integrate into the main branch
    Finish {
        worktree: Option<String>,
        /// Rebase onto the main branch and fast-forward it, instead of a PR
        #[arg(long)]
        integrate: bool,
    },
    /// Archive a worktree: a snapshot of its files and its branch are kept, the worktree goes;
    /// `wt restore` brings it back as it was. Never loses work, so it doesn't refuse like `rm`
    Archive {
        worktree: Option<String>,
        /// Stop the sessions running in it first
        #[arg(long)]
        stop_sessions: bool,
    },
    /// Bring an archived worktree back as it was (`name` or `project/name`)
    Restore { worktree: String },
    /// Archived worktrees, newest first
    Archives {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change a worktree's title, what people read; no title clears it (its name stays)
    Retitle {
        worktree: String,
        /// The new title (several words are joined)
        title: Vec<String>,
    },
    /// List worktrees
    Ls {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create a worktree on a new branch (default name: a friendly random one)
    New {
        name: Option<String>,
        /// Project (default: the one containing the current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Start from this ref (default: the project's base, e.g. origin/main)
        #[arg(long)]
        base: Option<String>,
        /// Branch name (default: worktrees.branch_template)
        #[arg(long)]
        branch: Option<String>,
        /// Check out the existing --branch instead of creating it
        #[arg(long, requires = "branch")]
        existing: bool,
        /// Check out GitHub pull request N (a number or its URL)
        #[arg(long, value_parser = crate::parse_number)]
        pr: Option<u32>,
        /// Start from a GitHub issue (number or URL): the branch and the agent's first prompt
        /// come from it
        #[arg(long, conflicts_with = "pr")]
        issue: Option<String>,
        /// The agent's first prompt (with --agent)
        #[arg(long, requires = "agent")]
        prompt: Option<String>,
        /// What it's for, for people to read (default: the prompt; its name comes from it too)
        #[arg(long)]
        title: Option<String>,
        /// Move the main checkout's uncommitted changes into it (a copy stays at
        /// refs/ptyro/taken/<name>)
        #[arg(long, conflicts_with_all = ["pr", "issue", "existing"])]
        take_changes: bool,
        /// Don't run the setup script
        #[arg(long)]
        no_setup: bool,
        /// Don't fetch the base first
        #[arg(long)]
        no_fetch: bool,
        /// Start an agent in it (after setup)
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        override_pin: bool,
        #[arg(short, long)]
        yes: bool,
        /// Attach to it afterwards
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove a worktree, refusing if it would lose work (SPEC §10.8)
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// Discard uncommitted, untracked, unpushed or stashed work (asks first)
        #[arg(long)]
        force: bool,
        /// With --force: don't ask
        #[arg(short, long)]
        yes: bool,
        /// Stop sessions running in it first
        #[arg(long)]
        stop_sessions: bool,
        #[arg(long)]
        keep_branch: bool,
    },
    /// Show a worktree's repository status
    Status {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the worktree's changes against its base
    Diff {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        /// base (default: everything since the base), upstream (unpushed), last-turn (what the
        /// agent's last turn did), head (uncommitted only)
        #[arg(long, default_value = "base", value_parser = ["base", "upstream", "last-turn", "head"])]
        against: String,
        /// Files and line counts only
        #[arg(long)]
        stat: bool,
        #[arg(long)]
        json: bool,
    },
    /// Commits since the base
    Log {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Commit as the worktree's profile: `-m MSG` with --all or files, or --ask-agent
    Commit {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(short, long, required_unless_present = "ask_agent")]
        message: Option<String>,
        /// Stage everything first (new files too)
        #[arg(short, long)]
        all: bool,
        /// Stage these files first
        #[arg(last = true)]
        paths: Vec<String>,
        /// Ask the worktree's agent to commit its work instead (once its turn is over)
        #[arg(long, conflicts_with_all = ["message", "all"])]
        ask_agent: bool,
    },
    /// Rebase onto the main branch and fast-forward it (or --cherry-pick the commits there)
    Integrate {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// The target branch (default: the project's default branch)
        #[arg(long)]
        into: Option<String>,
        #[arg(long)]
        cherry_pick: bool,
        /// Push the target branch afterwards
        #[arg(long)]
        push: bool,
    },
    /// Pair mode: the project's main checkout shows this worktree (your IDE and dev server see
    /// the agent's work); `unpair` brings your own work back
    Pair {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// Re-sync the main checkout (edits you made there go to the worktree first)
        #[arg(long)]
        refresh: bool,
    },
    /// End pair mode: your edits in the main checkout go to the worktree, and the main checkout
    /// gets its own branch and uncommitted work back
    Unpair {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
    },
    /// Run a lifecycle script: the default run script, or setup, archive, after_turn, a run name
    Run {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        script: Option<String>,
        #[arg(long)]
        attach: bool,
    },
    /// Open the worktree in an editor ([editors]; terminal editors open as a ptyro session)
    Open {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        #[arg(short, long)]
        editor: Option<String>,
    },
    /// Lock the worktree against `git worktree prune/remove`
    Lock {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
    },
    Unlock {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
    },
    /// Rename its branch (refused once pushed); without NEW, named after the first commit
    RenameBranch {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        new: Option<String>,
    },
}

async fn client(ctx: &Ctx) -> Result<Client, Error> {
    ctx.connect(true).await
}

fn projects_of(r: Reply) -> Result<Vec<ProjectInfo>, Error> {
    match r {
        Reply::Projects { projects } => Ok(projects),
        other => Err(unexpected(other)),
    }
}

fn worktrees_of(r: Reply) -> Result<Vec<WorktreeInfo>, Error> {
    match r {
        Reply::Worktrees { worktrees } => Ok(worktrees),
        other => Err(unexpected(other)),
    }
}

fn worktree_of(r: Reply) -> Result<WorktreeInfo, Error> {
    match r {
        Reply::Worktree { info } => Ok(info),
        other => Err(unexpected(other)),
    }
}

fn print_json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn notes(n: &[String]) {
    for l in n {
        eprintln!("  {l}");
    }
}

/// A project address, defaulting to the one containing the current directory.
fn project_addr(p: Option<String>) -> Result<String, Error> {
    match p {
        Some(p) => Ok(p),
        None => Ok(current_dir()?.to_string_lossy().into_owned()),
    }
}

/// A worktree address, defaulting to the one containing the current directory.
fn wt_addr(w: Option<String>) -> Result<String, Error> {
    project_addr(w)
}

fn status_line(w: &WorktreeInfo) -> String {
    let mut parts = Vec::new();
    if w.state != WorktreeState::Ready {
        parts.push(w.state.label().to_owned());
    }
    if let Some(s) = &w.status {
        if s.insertions + s.deletions > 0 {
            parts.push(format!("+{} -{}", s.insertions, s.deletions));
        }
        if s.dirty() > 0 {
            parts.push(format!("{} dirty", s.dirty()));
        }
        if s.base_ahead > 0 {
            parts.push(format!("{} ahead", s.base_ahead));
        }
        if s.base_behind > 0 {
            parts.push(format!("{} behind", s.base_behind));
        }
    }
    if w.locked {
        parts.push("locked".into());
    }
    if parts.is_empty() { "clean".into() } else { parts.join(", ") }
}

pub async fn project(ctx: &Ctx, cmd: ProjectCmd) -> Result<i32, Error> {
    let c = client(ctx).await?;
    match cmd {
        ProjectCmd::Add { path, workspace } => {
            let path = match path {
                Some(p) => {
                    p.canonicalize().map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?
                }
                None => current_dir()?,
            };
            let Reply::Project { info, notes: n } =
                c.request(Request::ProjectAdd { path, workspace }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            println!("{}", info.slug);
            eprintln!("project {} at {}", info.slug, info.root.display());
            notes(&n);
            Ok(0)
        }
        ProjectCmd::Ls { json } => {
            let projects = projects_of(c.request(Request::ProjectList).await?)?;
            let worktrees =
                worktrees_of(c.request(Request::WorktreeList { project: None }).await?)?;
            if json {
                print_json(&projects);
                return Ok(0);
            }
            if projects.is_empty() {
                eprintln!("no projects — register one with `ptyro project add [path]`");
                return Ok(0);
            }
            println!("{:<20} {:<10} {:<12} {:>3}  ROOT", "PROJECT", "PIN", "BRANCH", "WT");
            for p in &projects {
                let n = worktrees.iter().filter(|w| w.project == p.id && !w.is_root).count();
                println!(
                    "{:<20} {:<10} {:<12} {:>3}  {}",
                    truncate(&p.slug, 20),
                    p.pin.as_deref().unwrap_or("-"),
                    truncate(p.default_branch.as_deref().unwrap_or("-"), 12),
                    n,
                    p.root.display()
                );
            }
            Ok(0)
        }
        ProjectCmd::Rm { project } => {
            let Reply::Removed { notes: n } =
                c.request(Request::ProjectRemove { addr: project, release: false }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        ProjectCmd::Show { project, json } => {
            let addr = project_addr(project)?;
            let Reply::Project { info, .. } =
                c.request(Request::ProjectGet { addr: addr.clone() }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            let worktrees = worktrees_of(
                c.request(Request::WorktreeList { project: Some(info.id.clone()) }).await?,
            )?;
            let Reply::Scripts { scripts } =
                c.request(Request::ProjectScripts { addr, worktree: None }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                print_json(
                    &serde_json::json!({ "project": info, "worktrees": worktrees, "scripts": scripts }),
                );
                return Ok(0);
            }
            println!("{}  {}", info.slug, info.root.display());
            println!("  default branch  {}", info.default_branch.as_deref().unwrap_or("-"));
            println!("  remote          {}", info.remote_url.as_deref().unwrap_or("-"));
            println!("  pinned to       {}", info.pin.as_deref().unwrap_or("- (not pinned)"));
            println!("  worktrees");
            for w in &worktrees {
                println!(
                    "    {:<18} {:<26} {}",
                    w.name,
                    truncate(w.branch.as_deref().unwrap_or("(detached)"), 26),
                    status_line(w)
                );
            }
            if scripts.text.is_empty() {
                println!("  scripts         none (.ptyro.toml [scripts])");
            } else {
                println!(
                    "  scripts         {} ({})",
                    scripts.names.join(", "),
                    if scripts.trusted { "trusted" } else { "NOT trusted" }
                );
            }
            Ok(0)
        }
        ProjectCmd::Trust { project, yes } => {
            let addr = project_addr(project)?;
            let Reply::Scripts { scripts } =
                c.request(Request::ProjectScripts { addr: addr.clone(), worktree: None }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if scripts.text.is_empty() {
                eprintln!("{} has no scripts", scripts.project);
                return Ok(0);
            }
            if scripts.trusted {
                eprintln!("{}'s scripts are already trusted", scripts.project);
                return Ok(0);
            }
            eprintln!("{} wants to run these scripts (from .ptyro.toml):\n", scripts.project);
            eprintln!("{}", scripts.text);
            if !yes && !confirm("Trust them? They run as you, with this repository's code") {
                return Err(Error::new(ErrorCode::UntrustedProject, "not trusted"));
            }
            c.request(Request::ProjectTrust { addr, fingerprint: scripts.fingerprint }).await?;
            eprintln!("trusted; any change to them asks again");
            Ok(0)
        }
        ProjectCmd::Untrust { project } => {
            c.request(Request::ProjectUntrust { addr: project_addr(project)? }).await?;
            Ok(0)
        }
        ProjectCmd::Config { project, explain, json } => {
            let Reply::Project { info, .. } =
                c.request(Request::ProjectGet { addr: project_addr(project)? }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            let cfg = ptyro_core::project::ProjectConfig::load(&info.root, &info.root)
                .map_err(Error::invalid)?;
            if json {
                print_json(&serde_json::json!({
                    "config": cfg,
                    "origin": cfg.origin,
                    "imported": cfg.imported,
                }));
                return Ok(0);
            }
            print!("{}", toml::to_string_pretty(&cfg).map_err(|e| Error::internal(e.to_string()))?);
            if explain {
                println!();
                println!("# where each value comes from");
                for (k, from) in &cfg.origin {
                    println!("#   {k:<28} {from}");
                }
                for i in &cfg.imported {
                    for n in &i.notes {
                        println!("#   {}: {n}", i.file);
                    }
                }
                if cfg.origin.is_empty() {
                    println!("#   (nothing set: everything is a default)");
                }
            }
            Ok(0)
        }
        ProjectCmd::Pin { project, profile } => {
            c.request(Request::ProjectPin {
                addr: project.clone(),
                profile: Some(profile.clone()),
            })
            .await?;
            eprintln!(
                "{project}: sessions now run under profile {profile} unless chosen otherwise"
            );
            Ok(0)
        }
        ProjectCmd::Unpin { project } => {
            c.request(Request::ProjectPin { addr: project, profile: None }).await?;
            Ok(0)
        }
        ProjectCmd::Suggest { project, apply, decline, json } => {
            let apply = (apply || decline).then_some(apply);
            let Reply::Suggestions { suggestions: s } =
                c.request(Request::ProjectSuggest { addr: project_addr(project)?, apply }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                print_json(&s);
                return Ok(0);
            }
            if s.is_empty() {
                eprintln!("{}: nothing to suggest", s.project);
            }
            if let Some(cmd) = &s.setup {
                println!("setup: {cmd}   (from {})", s.from.as_deref().unwrap_or("?"));
            }
            if !s.copy.is_empty() {
                println!("copy into each worktree: {}", s.copy.join(", "));
            }
            match apply {
                Some(true) => eprintln!("written to .ptyro.local.toml"),
                Some(false) => eprintln!("not offered again"),
                None if !s.is_empty() => {
                    eprintln!("`ptyro project suggest --apply` writes them to .ptyro.local.toml")
                }
                None => {}
            }
            Ok(0)
        }
        ProjectCmd::Discover { refresh, json } => {
            let Reply::Paths { paths } = c.request(Request::Discover { refresh }).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            let known = projects_of(c.request(Request::ProjectList).await?)?;
            if json {
                print_json(&paths);
                return Ok(0);
            }
            for p in &paths {
                let mark = if known.iter().any(|k| k.root == *p) { "  (registered)" } else { "" };
                println!("{}{mark}", p.display());
            }
            if paths.is_empty() {
                eprintln!("no repositories under discover.roots");
            }
            Ok(0)
        }
    }
}

pub async fn ws(ctx: &Ctx, cmd: WsCmd) -> Result<i32, Error> {
    let c = client(ctx).await?;
    match cmd {
        WsCmd::Ls { json } => {
            let Reply::Workspaces { workspaces } = c.request(Request::WorkspaceList).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                print_json(&workspaces);
                return Ok(0);
            }
            let projects = projects_of(c.request(Request::ProjectList).await?)?;
            println!("{:<16} {:<12} {:<7} PROJECTS", "WORKSPACE", "PROFILE", "NOTIFY");
            for w in &workspaces {
                let names: Vec<&str> = w
                    .projects
                    .iter()
                    .filter_map(|id| projects.iter().find(|p| p.id == *id).map(|p| p.slug.as_str()))
                    .collect();
                println!("{:<16} {:<12} {:<7} {}", w.name, w.profile, w.notify, names.join(", "));
            }
            Ok(0)
        }
        WsCmd::New { name, profile, notify } => {
            c.request(Request::WorkspaceNew { name: name.clone(), profile, notify }).await?;
            eprintln!(
                "workspace {name} created; add projects with `ptyro ws add {name} <project>`"
            );
            Ok(0)
        }
        WsCmd::Set { name, profile, notify } => {
            c.request(Request::WorkspaceSet { name, profile, notify }).await?;
            Ok(0)
        }
        WsCmd::Rm { name } => {
            c.request(Request::WorkspaceRemove { name }).await?;
            Ok(0)
        }
        WsCmd::Rename { name, new_name } => {
            c.request(Request::WorkspaceRename { name, new_name }).await?;
            Ok(0)
        }
        WsCmd::Add { name, project } => {
            c.request(Request::WorkspaceMember { name, project, remove: false }).await?;
            Ok(0)
        }
        WsCmd::Remove { name, project } => {
            c.request(Request::WorkspaceMember { name, project, remove: true }).await?;
            Ok(0)
        }
    }
}

/// Create a worktree, asking about guards on a TTY.
async fn create(c: &Client, spec: WorktreeSpec, yes: bool) -> Result<Reply, Error> {
    offer_suggestions(c, &spec.project).await;
    with_confirmation(c, Request::WorktreeNew { spec }, yes, |r| {
        if let Request::WorktreeNew { spec } = r {
            spec.override_pin = true;
            spec.confirmed = true;
        }
    })
    .await
}

/// A project's first worktree, on a TTY (UX1.11): offer what ptyro found it needs, once.
async fn offer_suggestions(c: &Client, project: &str) {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return;
    }
    let Ok(Reply::Suggestions { suggestions: s }) =
        c.request(Request::ProjectSuggest { addr: project.into(), apply: None }).await
    else {
        return;
    };
    if s.offered || s.is_empty() {
        return;
    }
    eprintln!("{}'s first worktree. ptyro found:", s.project);
    if let Some(cmd) = &s.setup {
        eprintln!("  {} → setup = \"{cmd}\"", s.from.as_deref().unwrap_or("?"));
    }
    if !s.copy.is_empty() {
        eprintln!("  {} → copied into each worktree (git ignores them)", s.copy.join(", "));
    }
    let take = confirm("Write these to .ptyro.local.toml (yours, not committed)?");
    let _ = c.request(Request::ProjectSuggest { addr: project.into(), apply: Some(take) }).await;
}

/// On a TTY, offer to trust untrusted scripts and run setup now.
async fn offer_trust(
    c: &Client,
    reply: &Reply,
) -> Result<Option<ptyro_core::session::SessionInfo>, Error> {
    let Reply::WorktreeCreated { info, untrusted: Some(s), .. } = reply else { return Ok(None) };
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(None);
    }
    eprintln!("\n{} has scripts that haven't been trusted:\n\n{}", s.project, s.text);
    if !confirm("Trust them and run setup now?") {
        return Ok(None);
    }
    c.request(Request::ProjectTrust {
        addr: info.project.clone(),
        fingerprint: s.fingerprint.clone(),
    })
    .await?;
    let run =
        Request::WorktreeRun { addr: info.id.clone(), script: Some("setup".into()), size: None };
    Ok(Some(session_of(c.request(run).await?)?))
}

pub async fn new_in_new_worktree(
    ctx: &Ctx,
    mut a: NewArgs,
    name: String,
    project: Option<String>,
) -> Result<i32, Error> {
    if !a.detach {
        check_attachable()?;
    }
    let c = client(ctx).await?;
    // The project: --project, else the one containing -C, else the current directory's.
    let project = match (project, &a.cwd) {
        (Some(p), _) => p,
        (None, Some(dir)) => dir
            .canonicalize()
            .map_err(|e| Error::invalid(format!("{}: {e}", dir.display())))?
            .to_string_lossy()
            .into_owned(),
        (None, None) => project_addr(None)?,
    };
    // The worktree takes the profile asked for (`--profile`, or `claude@work`'s), so the session
    // started in it below agrees with its pin.
    let agent_profile = a.agent.as_deref().and_then(|x| ptyro_agents::split_agent(x).1);
    // No agent here: the session is started below like any `ptyro new`, so its name, command,
    // --unqueued and output are the usual ones.
    let spec = WorktreeSpec {
        project,
        name: (!name.is_empty()).then_some(name),
        pr: a.pr,
        issue: a.issue.clone(),
        profile: a.profile.clone().or_else(|| agent_profile.map(str::to_owned)),
        override_pin: a.override_pin,
        confirmed: a.yes,
        size: if a.detach { a.size } else { ptyro_tui::pane_size(&ctx.paths) },
        env: client_env(),
        setup: true,
        // Named and titled after what it's for (UX1.24); the session below gets the prompt.
        title: a.title.clone().or_else(|| a.prompt.clone().filter(|p| !p.starts_with('@'))),
        take_changes: a.take_changes,
        ..WorktreeSpec::default()
    };
    let reply = create(&c, spec, a.yes).await?;
    let setup_now = offer_trust(&c, &reply).await?;
    let Reply::WorktreeCreated { info, setup, notes: n, issue_prompt, .. } = reply else {
        return Err(Error::internal("unexpected reply"));
    };
    eprintln!(
        "worktree {} on {} at {}",
        info.qualified(),
        info.branch.as_deref().unwrap_or("-"),
        info.path.display()
    );
    notes(&n);
    if a.agent.is_some() {
        // The daemon read the issue (with gh, under the worktree's profile).
        if a.prompt.is_none() {
            a.prompt = issue_prompt;
        }
        if let Some(s) = setup.or(setup_now)
            && start_after_setup(ctx, &c, &info).await
        {
            eprintln!("waiting for setup ({}) before starting the agent", s.name);
            let o = crate::script::WaitOpts {
                conditions: vec!["exit".into()],
                pattern: None,
                scrollback: false,
                timeout: None,
                any: false,
                stall: None,
            };
            let mut c = c;
            crate::script::wait_for_lasting(ctx, &mut c, &[s.id], &o).await?;
        }
    }
    let a = NewArgs { wt: Some(info.id.clone()), cwd: None, ..a };
    crate::cli::new(ctx, a).await
}

/// `worktrees.start_agents` for this worktree (the project's `.ptyro.toml` first): agents wait
/// for setup unless it says `immediately`.
async fn start_after_setup(ctx: &Ctx, c: &Client, wt: &WorktreeInfo) -> bool {
    use ptyro_core::config::{Config, StartAgents};
    let global = Config::load(&Config::path(&ctx.paths)).unwrap_or_default().worktrees.start_agents;
    let project = match c.request(Request::ProjectGet { addr: wt.project.clone() }).await {
        Ok(Reply::Project { info, .. }) => {
            ptyro_core::project::ProjectConfig::load(&info.root, &wt.path).ok()
        }
        _ => None,
    };
    project.and_then(|p| p.worktree.start_agents).unwrap_or(global) == StartAgents::AfterSetup
}

pub async fn wt(ctx: &Ctx, cmd: WtCmd) -> Result<i32, Error> {
    let c = client(ctx).await?;
    match cmd {
        WtCmd::Ls { project, json } => {
            let worktrees = worktrees_of(c.request(Request::WorktreeList { project }).await?)?;
            if json {
                print_json(&worktrees);
                return Ok(0);
            }
            let Reply::Sessions { sessions } = c.request(Request::List).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            println!(
                "{:<28} {:<26} {:<9} {:<11} {:>3}  STATUS",
                "WORKTREE", "BRANCH", "PROFILE", "PORTS", "S"
            );
            for w in &worktrees {
                let n = sessions
                    .iter()
                    .filter(|s| s.worktree.as_deref() == Some(w.id.as_str()) && s.state.is_live())
                    .count();
                let ports =
                    w.ports.map_or("-".into(), |p| format!("{}-{}", p.base, p.base + p.count - 1));
                println!(
                    "{:<28} {:<26} {:<9} {:<11} {:>3}  {}",
                    truncate(&w.qualified(), 28),
                    truncate(w.branch.as_deref().unwrap_or("(detached)"), 26),
                    w.profile.as_deref().unwrap_or("-"),
                    ports,
                    n,
                    status_line(w)
                );
            }
            Ok(0)
        }
        WtCmd::Finish { worktree, integrate } => {
            let addr = wt_addr(worktree)?;
            eprintln!(
                "finishing {addr}: committing what's left, then {}…",
                if integrate { "integrating" } else { "a PR" }
            );
            let Reply::Notes { notes: n } =
                c.request(Request::WorktreeFinish { addr, integrate }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        WtCmd::Archive { worktree, stop_sessions } => {
            let addr = wt_addr(worktree)?;
            let Reply::Notes { notes: n } =
                c.request(Request::WorktreeArchive { addr, stop_sessions }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        WtCmd::Restore { worktree } => {
            let Reply::Worktree { info } =
                c.request(Request::WorktreeRestore { addr: worktree }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            eprintln!(
                "restored {} on {} at {}",
                info.qualified(),
                info.branch.as_deref().unwrap_or("-"),
                info.path.display()
            );
            Ok(0)
        }
        WtCmd::Archives { project, json } => {
            let Reply::Archives { entries } =
                c.request(Request::WorktreeArchives { project }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            if json {
                print_json(&entries);
                return Ok(0);
            }
            if entries.is_empty() {
                eprintln!("nothing archived");
            }
            for e in &entries {
                println!(
                    "{:<28} {:<8} {:<24} {}",
                    truncate(&format!("{}/{}", e.project_slug, e.name), 28),
                    age(e.archived_ms),
                    truncate(&e.branch, 24),
                    e.title.as_deref().unwrap_or("")
                );
            }
            Ok(0)
        }
        WtCmd::Retitle { worktree, title } => {
            let title = (!title.is_empty()).then(|| title.join(" "));
            let Reply::Worktree { info } =
                c.request(Request::Retitle { addr: worktree, title, worktree: true }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            eprintln!(
                "{}: {}",
                info.qualified(),
                info.title.unwrap_or_else(|| "title cleared".into())
            );
            Ok(0)
        }
        WtCmd::New {
            name,
            project,
            base,
            branch,
            existing,
            pr,
            issue,
            prompt,
            title,
            take_changes,
            no_setup,
            no_fetch,
            agent,
            profile,
            override_pin,
            yes,
            attach,
            json,
        } => {
            if attach {
                check_attachable()?;
            }
            let spec = WorktreeSpec {
                project: project_addr(project)?,
                name,
                branch,
                existing,
                pr,
                issue,
                prompt,
                title,
                take_changes,
                base,
                fetch: no_fetch.then_some(false),
                setup: !no_setup,
                profile,
                override_pin,
                confirmed: yes,
                agent,
                size: if attach { ptyro_tui::pane_size(&ctx.paths) } else { None },
                env: client_env(),
                ..WorktreeSpec::default()
            };
            let reply = create(&c, spec, yes).await?;
            let setup_now = if json { None } else { offer_trust(&c, &reply).await? };
            let Reply::WorktreeCreated { info, setup, agent, agent_pending, notes: n, .. } =
                reply.clone()
            else {
                return Err(unexpected(reply));
            };
            if json {
                print_json(&info);
            } else {
                println!("{}", info.qualified());
                eprintln!(
                    "on {} at {}",
                    info.branch.as_deref().unwrap_or("-"),
                    info.path.display()
                );
                notes(&n);
                if agent_pending {
                    eprintln!("  the agent starts when setup finishes");
                }
            }
            if attach {
                let show = agent.or(setup).or(setup_now);
                let follow = agent_pending.then(|| info.id.clone());
                return crate::cli::attach_with(
                    ctx,
                    c,
                    show,
                    ptyro_tui::Options { follow_worktree: follow, ..Default::default() },
                )
                .await;
            }
            Ok(0)
        }
        WtCmd::Rm { worktree, force, yes, stop_sessions, keep_branch } => {
            let opts = RemoveOpts { force: false, stop_sessions, keep_branch };
            let req = Request::WorktreeRemove { addr: worktree.clone(), opts };
            let reply = match c.request(req).await {
                Err(e) if e.code == ErrorCode::DirtyWorktree && force => {
                    eprintln!("{}", e.message);
                    if !yes && !confirm("Discard all of that and remove it anyway?") {
                        return Err(Error::new(ErrorCode::DirtyWorktree, "kept"));
                    }
                    let opts = RemoveOpts { force: true, stop_sessions, keep_branch };
                    c.request(Request::WorktreeRemove { addr: worktree, opts }).await?
                }
                other => other?,
            };
            let Reply::Removed { notes: n } = reply else { return Err(unexpected(reply)) };
            for l in n {
                eprintln!("{l}");
            }
            Ok(0)
        }
        WtCmd::Status { worktree, json } => {
            let info = worktree_of(
                c.request(Request::WorktreeRefresh { addr: wt_addr(worktree)? }).await?,
            )?;
            if json {
                print_json(&info);
                return Ok(0);
            }
            println!("{}  {}", info.qualified(), info.path.display());
            println!(
                "  branch    {}  (base {})",
                info.branch.as_deref().unwrap_or("(detached)"),
                info.base.as_deref().unwrap_or("-")
            );
            println!(
                "  profile   {}",
                info.profile.as_deref().unwrap_or("- (resolved per session)")
            );
            if let Some(p) = info.ports {
                println!("  ports     {}-{} (PTYRO_PORT={})", p.base, p.base + p.count - 1, p.base);
            }
            if let Some(s) = &info.status {
                println!(
                    "  vs base   {} ahead, {} behind, +{} -{}",
                    s.base_ahead, s.base_behind, s.insertions, s.deletions
                );
                if let Some(u) = &s.upstream {
                    println!("  upstream  {u}: {} ahead, {} behind", s.ahead, s.behind);
                }
                println!(
                    "  changes   {} staged, {} modified, {} untracked, {} conflicted",
                    s.staged, s.modified, s.untracked, s.conflicts
                );
                if let (Some(h), Some(subj)) = (&s.head, &s.subject) {
                    println!("  last      {h} {subj}");
                }
            }
            println!(
                "  state     {}{}",
                info.state.label(),
                if info.locked { ", locked (sessions running)" } else { "" }
            );
            Ok(0)
        }
        WtCmd::Diff { worktree, against, stat, json } => {
            crate::ship::diff(ctx, &c, wt_addr(worktree)?, &against, stat, json).await
        }
        WtCmd::Log { worktree, json } => crate::ship::log(&c, wt_addr(worktree)?, json).await,
        WtCmd::Commit { worktree, message, all, paths, ask_agent } => {
            crate::ship::commit(&c, wt_addr(worktree)?, message, all, paths, ask_agent).await
        }
        WtCmd::Integrate { worktree, into, cherry_pick, push } => {
            let Reply::Notes { notes: n } = c
                .request(Request::WorktreeIntegrate { addr: worktree, into, cherry_pick, push })
                .await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        WtCmd::Pair { worktree, refresh } => {
            let Reply::Notes { notes: n } =
                c.request(Request::WorktreePair { addr: worktree, off: false, refresh }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        WtCmd::Unpair { worktree } => {
            let Reply::Notes { notes: n } = c
                .request(Request::WorktreePair { addr: worktree, off: true, refresh: false })
                .await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            notes(&n);
            Ok(0)
        }
        WtCmd::Run { worktree, script, attach } => {
            if attach {
                check_attachable()?;
            }
            let size = attach.then(|| ptyro_tui::pane_size(&ctx.paths)).flatten();
            let info = session_of(
                c.request(Request::WorktreeRun { addr: worktree, script, size }).await?,
            )?;
            if attach {
                return crate::cli::attach(ctx, c, Some(info), false, false).await;
            }
            println!("{}", info.handle);
            eprintln!("started {} — `ptyro attach {}` to watch", info.name, info.name);
            Ok(0)
        }
        WtCmd::Open { worktree, editor } => open(ctx, c, wt_addr(worktree)?, editor).await,
        WtCmd::Lock { worktree } => {
            c.request(Request::WorktreeLock { addr: worktree, lock: true }).await?;
            Ok(0)
        }
        WtCmd::Unlock { worktree } => {
            c.request(Request::WorktreeLock { addr: worktree, lock: false }).await?;
            Ok(0)
        }
        WtCmd::RenameBranch { worktree, new } => {
            let info = worktree_of(
                c.request(Request::WorktreeRenameBranch { addr: worktree, branch: new }).await?,
            )?;
            eprintln!("branch is now {}", info.branch.as_deref().unwrap_or("-"));
            Ok(0)
        }
    }
}

/// `wt open` (SPEC §13.2): terminal editors open inside ptyro as a session in the worktree.
async fn open(ctx: &Ctx, c: Client, addr: String, editor: Option<String>) -> Result<i32, Error> {
    let info = worktree_of(c.request(Request::WorktreeGet { addr }).await?)?;
    let config = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&ctx.paths))
        .unwrap_or_default();
    let name = editor.unwrap_or_else(|| config.default_editor());
    let def = config.editor(&name).ok_or_else(|| {
        Error::new(
            ErrorCode::NotFound,
            format!("no editor {name:?} (known: {})", config.editor_names().join(", ")),
        )
    })?;
    let path = info.path.to_string_lossy().into_owned();
    let mut argv: Vec<String> = def.command.iter().map(|a| a.replace("{path}", &path)).collect();
    if !def.command.iter().any(|a| a.contains("{path}")) {
        argv.push(path.clone());
    }
    if def.inside {
        let attaching = std::io::IsTerminal::is_terminal(&std::io::stdout());
        let spec = SpawnSpec {
            argv,
            cwd: info.path.clone(),
            worktree: Some(info.id.clone()),
            name: None,
            env: client_env(),
            size: if attaching { ptyro_tui::pane_size(&ctx.paths) } else { None },
            confirmed: true,
            ..SpawnSpec::default()
        };
        let s = session_of(c.request(Request::Spawn { spec }).await?)?;
        if attaching {
            return crate::cli::attach(ctx, c, Some(s), false, false).await;
        }
        println!("{}", s.handle);
        return Ok(0);
    }
    let (prog, args) = argv.split_first().ok_or_else(|| Error::invalid("empty editor command"))?;
    std::process::Command::new(prog)
        .args(args)
        .current_dir(&info.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| Error::new(ErrorCode::NotFound, format!("{prog}: {e}")))?;
    eprintln!("opened {} in {name}", info.qualified());
    Ok(0)
}

/// `a` archive (keeps everything), `r` remove, else skip; `None` without a terminal.
fn ask_archive_or_remove(what: &str) -> Option<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprint!("{what}: a archive (keeps its work) · r remove · ⏎ skip ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    match line.trim() {
        "a" | "A" => Some(true),
        "r" | "R" => Some(false),
        _ => None,
    }
}

pub async fn gc(ctx: &Ctx, dry_run: bool, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::Gc { entries } = c.request(Request::Gc).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        print_json(&entries);
        return Ok(0);
    }
    if entries.is_empty() {
        eprintln!("nothing stale");
        return Ok(0);
    }
    for e in &entries {
        let w = &e.worktree;
        println!(
            "{:<28} {:<8} {}",
            truncate(&w.qualified(), 28),
            age(w.last_used_ms.unwrap_or(w.created_ms)),
            e.reasons.join("; ")
        );
    }
    if dry_run {
        return Ok(0);
    }
    for e in entries {
        let w = e.worktree;
        // Archiving keeps everything (UX1.12); removing refuses when work would be lost.
        let req = match ask_archive_or_remove(&w.qualified()) {
            Some(true) => Request::WorktreeArchive { addr: w.id.clone(), stop_sessions: false },
            Some(false) => {
                Request::WorktreeRemove { addr: w.id.clone(), opts: RemoveOpts::default() }
            }
            None => continue,
        };
        if let Request::WorktreeArchive { .. } = req {
            match c.request(req).await {
                Ok(Reply::Notes { notes: n }) => n.iter().for_each(|l| eprintln!("  {l}")),
                Ok(other) => return Err(unexpected(other)),
                Err(e) => eprintln!("  {}", e.message),
            }
            continue;
        }
        match c.request(req).await {
            Ok(Reply::Removed { notes: n }) => n.iter().for_each(|l| eprintln!("  {l}")),
            Ok(other) => return Err(unexpected(other)),
            // Refusals (unsaved work) are reported, never forced from gc.
            Err(e) => eprintln!("  {}", e.message),
        }
    }
    Ok(0)
}
