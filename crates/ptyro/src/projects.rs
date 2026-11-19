//! `ptyro project`, `ws`, `wt` and `gc` (SPEC §12.2, M3).

use std::path::PathBuf;

use clap::Subcommand;
use clap_complete::ArgValueCandidates;
use ptyro_client::Client;
use ptyro_core::error::Again;
use ptyro_core::model::{ProjectInfo, RemoveOpts, WorktreeInfo, WorktreeSpec, WorktreeState};
use ptyro_core::session::SpawnSpec;
use ptyro_core::{Error, ErrorCode, removal};
use ptyro_protocol::{Reply, Request};

use crate::cli::{
    Ctx, NewArgs, addr_here, addr_here_opt, ago, ask_first, check_attachable, client_env, confirm,
    current_dir, fixed_by, no_daemon, session_of, truncate, unexpected, with_confirmation,
};

#[derive(Subcommand)]
pub enum ProjectCmd {
    /// Register the git repository containing PATH (default: the current directory)
    Add {
        path: Option<PathBuf>,
        /// Add it to this workspace (default: the one the TUI showed last)
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::workspaces))]
        workspace: Option<String>,
        /// It is the repository a registered project lost (its directory gone): move that
        /// project here instead, as `project move` does (asked when it's seen)
        #[arg(long, conflicts_with = "separate")]
        moved: bool,
        /// Register it as a project of its own even when a project whose directory is gone
        /// seems to be it, moved; that project isn't offered this place again
        #[arg(long)]
        separate: bool,
    },
    /// List projects
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Forget a project: its trust, pin and place in workspaces go; its worktrees stay on disk.
    /// Asks first unless --yes
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
        /// Its running sessions keep going, outside any project (else they block it)
        #[arg(long)]
        release: bool,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
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
    /// Rename a project: its slug, the address its worktrees and sessions are found by
    /// (`NEW/name`). Its worktrees' directories stay where they are
    Rename {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
        new: String,
    },
    /// Point a project at where its repository is now, after you moved it (`mv`, another
    /// disk): it keeps its trust, pin, workspaces, sessions and worktrees, and its worktrees are
    /// linked to the new place. Refused for another repository, or for a copy (the old one still
    /// there). A project registered at PATH already is folded into it
    #[command(visible_alias = "relocate")]
    Move {
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
        /// Where the repository is now (its main checkout)
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: PathBuf,
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
    /// Remove a workspace: its sessions keep running and move to main, with its chats and the
    /// projects no other workspace has. Asks first unless --yes
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
    },
    /// Rename a workspace
    Rename { name: String, new_name: String },
    /// Add a project to a workspace (it can be in several)
    #[command(alias = "add")]
    AddProject {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
    },
    /// Take a project out of a workspace (it stays registered)
    RemoveProject {
        #[arg(add = ArgValueCandidates::new(crate::complete::workspaces))]
        name: String,
        #[arg(add = ArgValueCandidates::new(crate::complete::projects))]
        project: String,
    },
}

#[derive(Subcommand)]
// Parsed once per command line: boxing `New` would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum WtCmd {
    /// List worktrees
    Ls {
        /// Only this project's
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Create a worktree on a new branch; with --agent, start it there and attach (unless -d)
    New {
        /// Its name (default: from the prompt or title, else a friendly random one)
        #[arg(short, long)]
        name: Option<String>,
        /// (The name, the old way: `wt new NAME`)
        #[arg(hide = true, conflicts_with = "name")]
        name_arg: Option<String>,
        /// Project (default: the one containing the current directory)
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::projects))]
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
        /// Don't run the setup script
        #[arg(long)]
        no_setup: bool,
        /// Don't fetch the base first
        #[arg(long)]
        no_fetch: bool,
        /// Start an agent in it (after setup), and attach to it unless -d
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::agents))]
        agent: Option<String>,
        #[arg(long, add = ArgValueCandidates::new(crate::complete::profiles))]
        profile: Option<String>,
        #[arg(long)]
        override_pin: bool,
        #[arg(short, long)]
        yes: bool,
        /// Don't attach to the agent; print the worktree
        #[arg(short, long)]
        detach: bool,
        /// Attach even without an agent (to watch setup)
        #[arg(long, hide = true, conflicts_with = "detach")]
        attach: bool,
        /// Print the worktree as JSON (doesn't attach)
        #[arg(long)]
        json: bool,
    },
    /// Remove a worktree, refusing if it would lose work (SPEC §10.8): its directory is deleted,
    /// and its branch too if it is merged into its base or the same as its pushed copy. Asks first
    /// unless --yes
    Rm {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// Discard uncommitted, untracked, unpushed or stashed work (asks again, saying what)
        #[arg(long)]
        force: bool,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
        /// Stop sessions running in it first
        #[arg(long)]
        stop_sessions: bool,
        /// Keep its branch, even when it is merged
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
    /// Run a lifecycle script: the default run script, setup, or a run script by name
    Run {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        script: Option<String>,
        #[arg(long)]
        attach: bool,
    },
    /// Open the worktree in an editor (\[editors\]; terminal editors open as a ptyro session),
    /// the file manager, a terminal app, or a new shell
    Open {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: Option<String>,
        /// An installed editor (default: `editors.default`), `finder` (`files` on Linux) for the
        /// file manager, a terminal app (`terminal`, `iterm`, `ghostty`, `gnome-terminal`…), or
        /// `shell` for a new shell in it
        #[arg(short, long, add = ArgValueCandidates::new(crate::complete::editors))]
        editor: Option<String>,
    },
    /// Rename a worktree, or retitle it: its name is its address (`project/NEW`), its title is
    /// what people read (`--title`). Its branch stays and so does its directory, so what runs in
    /// it carries on. The main checkout is always `@root`
    Rename {
        #[arg(add = ArgValueCandidates::new(crate::complete::worktrees))]
        worktree: String,
        /// The new name
        #[arg(required_unless_present = "title")]
        new: Option<String>,
        /// The new title, for people to read; `--title ""` clears it
        #[arg(long)]
        title: Option<String>,
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

/// `  (gone: it seems to have moved to …)` after a project whose main checkout is missing, with
/// the command that moves it (SPEC §10.1).
fn gone(p: &ProjectInfo, worktrees: &[WorktreeInfo]) -> String {
    let missing = worktrees
        .iter()
        .any(|w| w.project == p.id && w.is_root && w.state == WorktreeState::Missing);
    match (&p.moved_to, missing) {
        (Some(to), _) => format!(
            "  (gone: it seems to have moved to {}; `ptyro project move {} {}`)",
            to.display(),
            p.slug,
            to.display()
        ),
        (None, true) => format!("  (gone: moved it? `ptyro project move {} NEWPATH`)", p.slug),
        (None, false) => String::new(),
    }
}

fn notes(n: &[String]) {
    for l in n {
        eprintln!("  {l}");
    }
}

/// A project address, defaulting to the one containing the current directory.
fn project_addr(p: Option<String>) -> Result<String, Error> {
    match p {
        Some(p) => addr_here(p),
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
        // What is left to do (UX2.2): the line counts only while there is something to commit or
        // push, then `↑2` / `not pushed ↑2` / `● 3`, or a quiet `✓ pushed`.
        let l = s.left();
        if l.open() && s.insertions + s.deletions > 0 {
            parts.push(format!("+{} -{}", s.insertions, s.deletions));
        }
        if !l.quiet() {
            parts.push(l.words());
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
        ProjectCmd::Add { path, workspace, moved, separate } => {
            let path = match path {
                Some(p) => ptyro_core::os::canonicalize(&p)
                    .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?,
                None => current_dir()?,
            };
            let req = Request::ProjectAdd { path: path.clone(), workspace, moved, separate };
            let reply = match c.request(req.clone()).await {
                // A project whose directory is gone is this repository, moved (SPEC §10.1): the
                // refusal's way through, asked here as the TUI and the web ask it.
                Err(e) if fixed_by(&e, &Again::Moved) => {
                    let label = e.fix.as_ref().map_or("", |f| f.label.as_str());
                    if !confirm(&format!("{} {label}?", e.message)) {
                        eprintln!("{}", e.message);
                        eprintln!(
                            "  to move it: `ptyro project add {p} --moved`\n  to keep them apart: `ptyro project add {p} --separate`",
                            p = path.display()
                        );
                        return Ok(ErrorCode::Conflict.exit_code());
                    }
                    let again = req.with_fix(&Again::Moved);
                    c.request(again.ok_or_else(|| Error::internal("no way through"))?).await?
                }
                other => other?,
            };
            let Reply::Project { info, notes: n } = reply else {
                return Err(Error::internal("unexpected reply"));
            };
            println!("{}", info.slug);
            eprintln!("project {} at {}", info.slug, info.root.display());
            notes(&n);
            Ok(0)
        }
        ProjectCmd::Move { project, path } => {
            // Absolute, as the daemon (elsewhere) needs it, and resolved as it names folders
            // (`~/external` → `/Volumes/…`); one that isn't here may be on the project's
            // machine (MM1.B): the daemon there says whether it exists.
            let path = if path.is_relative() { current_dir()?.join(path) } else { path };
            let path = ptyro_core::os::canonicalize(&path).unwrap_or(path);
            let Reply::Project { info, notes: n } =
                c.request(Request::ProjectMove { addr: addr_here(project)?, path }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            println!("{}", info.slug);
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
                    "{:<20} {:<10} {:<12} {:>3}  {}{}",
                    truncate(&p.slug, 20),
                    p.pin.as_deref().unwrap_or("-"),
                    truncate(p.default_branch.as_deref().unwrap_or("-"), 12),
                    n,
                    p.root.display(),
                    gone(p, &worktrees)
                );
            }
            Ok(0)
        }
        ProjectCmd::Rm { project, release, yes } => {
            // Found first, then asked about by its name: a typo asks nothing.
            let Reply::Project { info, .. } =
                c.request(Request::ProjectGet { addr: addr_here(project)? }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            let mut q = format!("Forget project {}? {}", info.slug, removal::PROJECT);
            if release {
                q = format!("{q} {}", removal::PROJECT_RELEASE);
            }
            ask_first(&q, yes)?;
            let req = Request::ProjectRemove { addr: info.id, release };
            let reply = match c.request(req.clone()).await {
                // Its sessions are running: the refusal's way through (SPEC §6.6), the TUI's
                // and the web's offer, asked here too.
                Err(e) if fixed_by(&e, &Again::Release) => {
                    let label = e.fix.as_ref().map_or("", |f| f.label.as_str());
                    if yes || !confirm(&format!("{} {label}?", e.message)) {
                        return Err(e);
                    }
                    let again = req.with_fix(&Again::Release);
                    c.request(again.ok_or_else(|| Error::internal("no way through"))?).await?
                }
                other => other?,
            };
            let Reply::Removed { notes: n } = reply else {
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
            println!("{}  {}{}", info.slug, info.root.display(), gone(&info, &worktrees));
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
        ProjectCmd::Rename { project, new } => {
            let Reply::Project { info, .. } =
                c.request(Request::ProjectRename { addr: addr_here(project)?, name: new }).await?
            else {
                return Err(Error::internal("unexpected reply"));
            };
            eprintln!("renamed to {}", info.slug);
            Ok(0)
        }
        ProjectCmd::Pin { project, profile } => {
            c.request(Request::ProjectPin {
                addr: addr_here(project.clone())?,
                profile: Some(profile.clone()),
            })
            .await?;
            eprintln!(
                "{project}: sessions now run under profile {profile} unless chosen otherwise"
            );
            Ok(0)
        }
        ProjectCmd::Unpin { project } => {
            c.request(Request::ProjectPin { addr: addr_here(project)?, profile: None }).await?;
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
            // No --profile: a profile with the workspace's name is the one meant (`ws new work`).
            let named = match &profile {
                Some(_) => None,
                None => match c.request(Request::ProfileList).await? {
                    Reply::Profiles { profiles } => profiles
                        .iter()
                        .any(|p| p.name == name && p.machine.is_none())
                        .then(|| name.clone()),
                    _ => None,
                },
            };
            let profile = profile.or_else(|| named.clone());
            let shown = profile.clone().unwrap_or_else(|| "default".into());
            c.request(Request::WorkspaceNew { name: name.clone(), profile, notify }).await?;
            let why = if named.is_some() { ", named like it" } else { "" };
            eprintln!(
                "workspace {name} created with profile {shown}{why} (`ptyro ws set {name} --profile P` changes it); add projects with `ptyro ws add-project {name} <project>`"
            );
            Ok(0)
        }
        WsCmd::Set { name, profile, notify } => {
            c.request(Request::WorkspaceSet { name, profile, notify }).await?;
            Ok(0)
        }
        WsCmd::Rm { name, yes } => {
            // Found first (main is the daemon's to refuse), then asked about, then removed by
            // the name that was asked about.
            let Reply::Workspaces { workspaces } = c.request(Request::WorkspaceList).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            let w =
                ptyro_core::lookup::pick("workspace", &name, &workspaces, |w| &w.name, |w| &w.id)?;
            let name = w.name.clone();
            if name != ptyro_core::model::MAIN_WORKSPACE {
                ask_first(&format!("Remove workspace {name}? {}", removal::WORKSPACE), yes)?;
            }
            c.request(Request::WorkspaceRemove { name }).await?;
            Ok(0)
        }
        WsCmd::Rename { name, new_name } => {
            c.request(Request::WorkspaceRename { name, new_name }).await?;
            Ok(0)
        }
        WsCmd::AddProject { name, project } => {
            let project = addr_here(project)?;
            c.request(Request::WorkspaceMember { name, project, remove: false }).await?;
            Ok(0)
        }
        WsCmd::RemoveProject { name, project } => {
            let project = addr_here(project)?;
            c.request(Request::WorkspaceMember { name, project, remove: true }).await?;
            Ok(0)
        }
    }
}

/// Create a worktree, asking about guards on a TTY.
async fn create(c: &Client, spec: WorktreeSpec, yes: bool) -> Result<Reply, Error> {
    with_confirmation(c, Request::WorktreeNew { spec }, yes, |r| {
        if let Request::WorktreeNew { spec } = r {
            spec.override_pin = true;
            spec.confirmed = true;
        }
    })
    .await
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
    // Decided once: `new` below attaches (or not) the same way.
    a.detach = !crate::cli::attaches(a.detach || a.json);
    let c = client(ctx).await?;
    // The project: --project, else the one containing -C, else the current directory's.
    let project = match (project, &a.cwd) {
        (Some(p), _) => addr_here(p)?,
        (None, Some(dir)) => ptyro_core::os::canonicalize(dir)
            .map_err(|e| Error::invalid(format!("{}: {e}", dir.display())))?
            .to_string_lossy()
            .into_owned(),
        (None, None) => project_addr(None)?,
    };
    // The worktree takes the profile asked for (`--profile`, or `claude@work`'s), so the session
    // started in it below agrees with its pin.
    let agent_profile = a.agent.as_deref().and_then(|x| ptyro_agents::split_agent(x).1);
    // No agent here: the session is started below like any `ptyro new`, so its name, command
    // and output are the usual ones.
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
                scrollback: None,
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
            let worktrees = worktrees_of(
                c.request(Request::WorktreeList { project: addr_here_opt(project)? }).await?,
            )?;
            if json {
                print_json(&worktrees);
                return Ok(0);
            }
            let Reply::Sessions { sessions } = c.request(Request::List).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            println!("{:<28} {:<26} {:<9} {:>3}  STATUS", "WORKTREE", "BRANCH", "PROFILE", "S");
            for w in &worktrees {
                let n = sessions
                    .iter()
                    .filter(|s| s.worktree.as_deref() == Some(w.id.as_str()) && s.state.is_live())
                    .count();
                println!(
                    "{:<28} {:<26} {:<9} {:>3}  {}",
                    truncate(&w.qualified(), 28),
                    truncate(w.branch.as_deref().unwrap_or("(detached)"), 26),
                    w.profile.as_deref().unwrap_or("-"),
                    n,
                    status_line(w)
                );
            }
            Ok(0)
        }
        WtCmd::New {
            name,
            name_arg,
            project,
            base,
            branch,
            existing,
            pr,
            issue,
            prompt,
            title,
            no_setup,
            no_fetch,
            agent,
            profile,
            override_pin,
            yes,
            detach,
            attach,
            json,
        } => {
            // It attaches to the agent it starts, as `ptyro new` does (`--attach`: even without).
            let attach = attach || (agent.is_some() && crate::cli::attaches(detach || json));
            if attach {
                check_attachable()?;
            }
            let spec = WorktreeSpec {
                project: project_addr(project)?,
                name: name.or(name_arg),
                branch,
                existing,
                pr,
                issue,
                prompt,
                title,
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
            // Found first, then asked about by its name and branch: a typo asks nothing.
            let info =
                worktree_of(c.request(Request::WorktreeGet { addr: addr_here(worktree)? }).await?)?;
            let what = if keep_branch { removal::WORKTREE_KEEP_BRANCH } else { removal::WORKTREE };
            let name = format!("{} ({})", info.qualified(), info.branch.as_deref().unwrap_or("-"));
            ask_first(&format!("Remove worktree {name}? {what} {}", removal::SAFE), yes)?;
            let worktree = info.id;
            let opts = RemoveOpts { force: false, stop_sessions, keep_branch };
            let req = Request::WorktreeRemove { addr: worktree.clone(), opts };
            let reply = match c.request(req).await {
                Err(e) if force && fixed_by(&e, &Again::Force) => {
                    eprintln!("{}", e.message);
                    let q = format!("Discard that work and remove it? {}", removal::FORCED);
                    ask_first(&q, yes)?;
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
                let words = s.left().words();
                println!(
                    "  to do     {}",
                    if words.is_empty() { "nothing yet".to_owned() } else { words }
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
        WtCmd::Run { worktree, script, attach } => {
            if attach {
                check_attachable()?;
            }
            let size = attach.then(|| ptyro_tui::pane_size(&ctx.paths)).flatten();
            let info = session_of(
                c.request(Request::WorktreeRun { addr: addr_here(worktree)?, script, size })
                    .await?,
            )?;
            if attach {
                return crate::cli::attach(ctx, c, Some(info), false, false).await;
            }
            println!("{}", info.handle);
            eprintln!("started {} — `ptyro attach {}` to watch", info.name, info.name);
            Ok(0)
        }
        WtCmd::Open { worktree, editor } => open(ctx, c, wt_addr(worktree)?, editor).await,
        WtCmd::Rename { worktree, new, title } => {
            let mut addr = addr_here(worktree)?;
            if let Some(name) = new {
                let info = worktree_of(c.request(Request::WorktreeRename { addr, name }).await?)?;
                eprintln!("renamed to {}", info.qualified());
                addr = info.id;
            }
            if let Some(title) = title {
                let req = Request::Retitle { addr, title: Some(title), worktree: true };
                let info = worktree_of(c.request(req).await?)?;
                eprintln!(
                    "{}: {}",
                    info.qualified(),
                    info.title.unwrap_or_else(|| "title cleared".into())
                );
            }
            Ok(0)
        }
    }
}

/// `wt open` (SPEC §13.2): terminal editors open inside ptyro as a session in the worktree.
async fn open(ctx: &Ctx, c: Client, addr: String, editor: Option<String>) -> Result<i32, Error> {
    // On another computer (`--remote`, Windows' WSL daemon) the worktree's path is the daemon's:
    // the daemon opens it, with its own editors (WIN3), except a shell, which is a session.
    if ctx.remote.is_some() && editor.as_deref() != Some("shell") {
        return match c.request(Request::WorktreeOpen { addr, editor: editor.clone() }).await? {
            Reply::Session { info } => {
                if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                    return crate::cli::attach(ctx, c, Some(info), false, false).await;
                }
                println!("{}", info.handle);
                Ok(0)
            }
            Reply::Notes { notes } => {
                for n in notes {
                    eprintln!("{n}");
                }
                Ok(0)
            }
            r => Err(unexpected(r)),
        };
    }
    let info = worktree_of(c.request(Request::WorktreeGet { addr }).await?)?;
    let config = ptyro_core::config::Config::load(&ptyro_core::config::Config::path(&ctx.paths))
        .unwrap_or_default();
    let name = editor.unwrap_or_else(|| config.default_editor());
    // `shell`: a new shell in it, the TUI's `!` (unless an editor of yours has that name).
    let (argv, inside) = if name == "shell" && !config.editors.contains_key("shell") {
        (Vec::new(), true)
    } else {
        let def = config
            .editor(&name)
            .ok_or_else(|| Error::new(ErrorCode::NotFound, config.no_editor(&name)))?;
        (def.argv(&info.path), def.inside)
    };
    if inside {
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

/// What to do with one stale worktree, asked.
enum GcAnswer {
    Remove,
    /// Keep it from clean-up (`GcKeep`): not offered again until something changes.
    Keep,
    /// Leave it for now: offered again next time.
    Skip,
}

/// What to do with one stale worktree: `r` removes it, `k` keeps it from clean-up, anything else
/// leaves it for now; left for now without a terminal.
fn ask_remove(what: &str) -> GcAnswer {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return GcAnswer::Skip;
    }
    eprint!("{what}: r remove · k keep it (not offered again until it changes) · ⏎ not now ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    match line.trim() {
        "r" | "R" => GcAnswer::Remove,
        "k" | "K" => GcAnswer::Keep,
        _ => GcAnswer::Skip,
    }
}

/// Keep one worktree from clean-up, saying what that means.
async fn gc_keep(c: &ptyro_client::Client, worktree: String) -> Result<(), Error> {
    match c.request(Request::GcKeep { worktree }).await? {
        Reply::Notes { notes } => notes.iter().for_each(|n| eprintln!("{n}")),
        other => return Err(unexpected(other)),
    }
    Ok(())
}

pub async fn gc(
    ctx: &Ctx,
    dry_run: bool,
    json: bool,
    only_scratch: bool,
    keep: Vec<String>,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    // `--keep-wt`: keep those, and nothing else.
    if !keep.is_empty() {
        for w in keep {
            gc_keep(&c, w).await?;
        }
        return Ok(0);
    }
    let Reply::Gc { entries, scratch } = c.request(Request::Gc).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    // `--scratch`: only scratch shells' directories left behind (`--json` too).
    let entries = if only_scratch { Vec::new() } else { entries };
    if json {
        // The stale worktrees, as always (scripts read that shape), or with --scratch the
        // scratch leftovers.
        if only_scratch {
            print_json(&scratch);
        } else {
            print_json(&entries);
        }
        return Ok(0);
    }
    if entries.is_empty() && scratch.is_empty() {
        eprintln!("nothing stale");
        return Ok(0);
    }
    let scratch_list = scratch.clone();
    for e in &entries {
        let w = &e.worktree;
        println!(
            "{:<28} {:<8} {:>11}  {}",
            truncate(&w.qualified(), 28),
            ago(w.last_used_ms.unwrap_or(w.created_ms)),
            e.size_label().unwrap_or_else(|| "-".into()),
            e.reasons.join("; ")
        );
    }
    let total: u64 = entries.iter().filter_map(|e| e.bytes).sum();
    if total > 0 {
        eprintln!("{} on disk in all", ptyro_core::model::human_bytes(total));
    }
    for l in &scratch {
        // Its own name (the shell's id) tells the rows apart; the whole path is long and alike.
        let name = l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!(
            "{:<28} {:<8} {:>11}  a scratch shell's directory, left behind with {} files in it",
            truncate(&format!("scratch/{name}"), 28),
            ago(l.modified_ms),
            ptyro_core::model::human_bytes(l.bytes),
            l.files
        );
    }
    if dry_run {
        return Ok(0);
    }
    for e in entries {
        let w = e.worktree;
        // Removing refuses when work would be lost.
        match ask_remove(&w.qualified()) {
            GcAnswer::Remove => {}
            GcAnswer::Keep => {
                if let Err(e) = gc_keep(&c, w.id.clone()).await {
                    eprintln!("  {}", e.message);
                }
                continue;
            }
            GcAnswer::Skip => continue,
        }
        let req = Request::WorktreeRemove { addr: w.id.clone(), opts: RemoveOpts::default() };
        match c.request(req).await {
            Ok(Reply::Removed { notes: n }) => n.iter().for_each(|l| eprintln!("  {l}")),
            Ok(other) => return Err(unexpected(other)),
            // Refusals (unsaved work) are reported, never forced from gc: the way through is
            // said, for the person to choose.
            Err(e) if fixed_by(&e, &Again::Force) => eprintln!(
                "  {}\n  (`ptyro wt rm {} --force` discards it)",
                e.message.replace('\n', "\n  "),
                w.qualified()
            ),
            Err(e) => eprintln!("  {}", e.message),
        }
    }
    clean_scratch(&c, &scratch_list).await;
    Ok(0)
}

/// Scratch shells' directories left behind (SPEC §23.8): each said, and deleted after a yes.
async fn clean_scratch(c: &ptyro_client::Client, list: &[ptyro_core::model::ScratchLeftover]) {
    use std::io::{BufRead, IsTerminal, Write};
    if list.is_empty() || !std::io::stdin().is_terminal() {
        return;
    }
    for l in list {
        eprint!(
            "Delete the scratch directory {} ({} files)? {} d delete · ⏎ keep ",
            l.path.display(),
            l.files,
            removal::SCRATCH
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        if !matches!(line.trim(), "d" | "D") {
            continue;
        }
        match c.request(Request::ShellScratchRemove { path: l.path.clone() }).await {
            Ok(Reply::Removed { notes }) => notes.iter().for_each(|n| eprintln!("  {n}")),
            Ok(_) => {}
            Err(e) => eprintln!("  {}", e.message),
        }
    }
}
