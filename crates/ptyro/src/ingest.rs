//! `ptyro ingest`, `ptyro kb ls`, `kb sync` and `kb rm` (SPEC §25.10): the folders, files and
//! projects the knowledge base reads (its roots), and what the agents kept before ptyro did
//! (`ingest --history`).

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use clap_complete::ArgValueCandidates;
use ptyro_core::kb::search::pieces;
use ptyro_core::kb::{KbRootInfo, RootOptions, RootSpec, RootState};
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};
use serde_json::json;

use crate::cli::{Ctx, ago, unexpected};
use crate::complete;
use crate::kb::{ask, printable, printable_lines};

/// How often a command that waits for a read asks how far it is.
const POLL: Duration = Duration::from_millis(250);

/// `ptyro ingest`'s arguments.
#[derive(Args, Debug)]
pub struct IngestArgs {
    /// The folders or files to read and keep reading
    #[arg(value_name = "PATH", required_unless_present = "history")]
    paths: Vec<PathBuf>,
    /// What to call it in `ptyro kb ls` (default: the folder's name)
    #[arg(short, long, conflicts_with = "history")]
    name: Option<String>,
    /// The project it belongs to (default: none)
    #[arg(short, long, conflicts_with = "history", add = ArgValueCandidates::new(complete::projects))]
    project: Option<String>,
    /// Only this profile's agents may see it (default: every profile's). With `--history`:
    /// only this profile's folders
    #[arg(long, add = ArgValueCandidates::new(complete::profiles))]
    profile: Option<String>,
    /// Only files that match (a glob like `docs/**` or `*.md`; several: one flag each)
    #[arg(long, value_name = "GLOB", conflicts_with = "history")]
    include: Vec<String>,
    /// Not the files that match
    #[arg(long, value_name = "GLOB", conflicts_with = "history")]
    exclude: Vec<String>,
    /// Read source code itself too, found by its words (it makes the knowledge base large)
    #[arg(long, conflicts_with = "history")]
    code: bool,
    /// Don't read source files' comments
    #[arg(long, conflicts_with = "history")]
    no_comments: bool,
    /// Don't read the repository's commit messages
    #[arg(long, conflicts_with = "history")]
    no_commits: bool,
    /// Read it now and never again
    #[arg(long, conflicts_with = "history")]
    once: bool,
    /// Instead: read what your agents kept before ptyro did, every conversation in each
    /// profile's Claude Code and Codex folders that isn't kept yet. It says how many it
    /// found before it reads any
    #[arg(long, conflicts_with = "paths")]
    history: bool,
    /// With `--history`: only this agent's (claude or codex)
    #[arg(short, long, value_parser = ["claude", "codex"])]
    agent: Option<String>,
    /// With `--history`: only conversations written to since then: 30d, 12h, 2w
    #[arg(long, value_name = "AGE")]
    since: Option<String>,
    /// With `--history`: count, and read nothing
    #[arg(long)]
    dry_run: bool,
    /// On a linked machine instead of this computer: its folders, read into its knowledge
    /// base (the paths are that machine's)
    #[arg(long, value_name = "MACHINE", add = ArgValueCandidates::new(complete::machines))]
    machine: Option<String>,
    #[arg(long)]
    json: bool,
}

async fn roots(
    ctx: &Ctx,
    c: &ptyro_client::Client,
    machine: &Option<String>,
) -> Result<Vec<KbRootInfo>, Error> {
    match ask(ctx, c, Request::KbRoots { machine: machine.clone() }).await? {
        Reply::KbRoots { roots } => Ok(roots),
        r => Err(unexpected(r)),
    }
}

/// A line that is written over while something is read, on a terminal only.
fn progress(text: &str) {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K{}", printable(text));
    }
}

/// A file that was not kept (`uri`), as the path it has under the root at `root`: `None`
/// when it is not under it (by whole names: `/a/docs2/x` is not under `/a/doc`). The root
/// itself, a file that was given, is its own whole path.
fn under(root: &str, uri: &str) -> Option<String> {
    let path = std::path::Path::new(uri.strip_prefix("file://")?);
    let rel = path.strip_prefix(root).ok()?;
    Some(if rel.as_os_str().is_empty() { uri.to_owned() } else { rel.display().to_string() })
}

fn progress_end() {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Wait until none of these roots (every root, when none is named) is being read or removed;
/// the roots as they then are.
async fn settled(
    ctx: &Ctx,
    c: &ptyro_client::Client,
    ids: &[String],
    machine: &Option<String>,
) -> Result<Vec<KbRootInfo>, Error> {
    loop {
        let all = roots(ctx, c, machine).await?;
        let busy: Vec<&KbRootInfo> = all
            .iter()
            .filter(|r| ids.is_empty() || ids.contains(&r.root.id))
            .filter(|r| r.progress.is_some() || r.removing)
            .collect();
        match busy.first() {
            None => {
                progress_end();
                return Ok(all);
            }
            Some(r) => match r.progress {
                Some((done, of)) if of > 0 => {
                    progress(&format!("reading {}: {done} of {of}", r.root.name))
                }
                _ if r.removing => progress(&format!("removing {}…", r.root.name)),
                _ => progress(&format!("reading {}…", r.root.name)),
            },
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A root in a line: what it holds and how it stands.
fn holds(r: &KbRootInfo) -> String {
    let mut parts = vec![plural(r.files, "file", "files")];
    if r.commits > 0 {
        parts.push(plural(r.commits, "commit", "commits"));
    }
    parts.push(pieces(r.chunks));
    parts.join(", ")
}

impl IngestArgs {
    /// The history's own flags go with `--history` only.
    fn check(&self) -> Result<(), Error> {
        if !self.history && (self.agent.is_some() || self.since.is_some() || self.dry_run) {
            return Err(Error::invalid(
                "--agent, --since and --dry-run go with --history (`ptyro ingest --history --dry-run`)",
            ));
        }
        Ok(())
    }
}

pub async fn run(ctx: &Ctx, a: IngestArgs) -> Result<i32, Error> {
    a.check()?;
    if a.history {
        return history(ctx, a).await;
    }
    let c = ctx.connect(true).await?;
    let options = RootOptions {
        include: a.include,
        exclude: a.exclude,
        comments: a.no_comments.then_some(false),
        code: a.code.then_some(true),
        commits: a.no_commits.then_some(false),
        once: a.once,
    };
    if a.name.is_some() && a.paths.len() > 1 {
        return Err(Error::invalid("--name names one root: give one path with it"));
    }
    let mut out = Vec::new();
    let mut code = 0;
    for path in a.paths {
        // A path as the daemon knows it: whole. (A remote daemon's paths are its own, and so
        // are a linked machine's.)
        let here = ctx.remote.is_none() && a.machine.is_none();
        let path = if path.is_relative() && !path.starts_with("~") && here {
            std::env::current_dir().map_err(|e| Error::internal(e.to_string()))?.join(path)
        } else {
            path
        };
        let spec = RootSpec {
            path: path.to_string_lossy().into_owned(),
            name: a.name.clone(),
            project: a.project.clone(),
            profile: a.profile.clone(),
            options: options.clone(),
        };
        let req = Request::KbIngest { spec, machine: a.machine.clone() };
        let (root, plan) = match ask(ctx, &c, req).await? {
            Reply::KbIngested { root, plan } => (root, plan),
            r => return Err(unexpected(r)),
        };
        if !a.json {
            if plan.existing {
                eprintln!(
                    "{} is read already, as {} ({}): reading it again",
                    path.display(),
                    printable(&root.root.name),
                    printable(&root.root.path)
                );
            }
            let line = format!("{} ({}): {}", root.root.name, root.root.path, plan.line());
            eprintln!("{}", printable(&line));
        }
        let all = settled(ctx, &c, std::slice::from_ref(&root.root.id), &a.machine).await?;
        let Some(now) = all.into_iter().find(|r| r.root.id == root.root.id) else {
            let name = printable(&root.root.name);
            return Err(Error::new(ErrorCode::NotFound, format!("{name} is gone")));
        };
        let name = printable(&now.root.name);
        if a.json {
            out.push(json!({ "root": now, "plan": plan }));
            continue;
        }
        match (&now.root.state, &now.root.error) {
            (RootState::Error | RootState::Missing, e) => {
                let why = e.as_deref().map(printable);
                eprintln!(
                    "{name} could not be read: {}",
                    why.as_deref().unwrap_or("`ptyro kb` says more")
                );
                code = 1;
            }
            (_, note) => {
                println!("read {name}: {}", holds(&now));
                if let Some(note) = note {
                    eprintln!("  {}", printable(note));
                }
            }
        }
        // What was not kept of it, and why (this machine's: the status is).
        if a.machine.is_none()
            && let Ok(Reply::KbStatus { status }) = ask(ctx, &c, Request::KbStatus).await
        {
            let skipped: Vec<(String, &str)> = status
                .skipped
                .iter()
                .filter_map(|s| Some((under(&now.root.path, &s.uri)?, s.why.as_str())))
                .collect();
            for (rel, why) in skipped.iter().rev().take(10) {
                eprintln!("  not kept: {}", printable(&format!("{rel}: {why}")));
            }
            if skipped.len() > 10 {
                eprintln!("  and {} more: `ptyro kb` lists them", skipped.len() - 10);
            }
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    Ok(code)
}

async fn history(ctx: &Ctx, a: IngestArgs) -> Result<i32, Error> {
    let since_ms = match a.since.as_deref() {
        Some(age) => {
            let d = ptyro_core::config::parse_duration(age)
                .map_err(|e| Error::invalid(format!("--since {age}: {e}")))?;
            Some(ptyro_core::now_ms().saturating_sub(d.as_millis() as u64))
        }
        None => None,
    };
    let c = ctx.connect(true).await?;
    let req = |dry_run| Request::KbHistory {
        agent: a.agent.clone(),
        profile: a.profile.clone(),
        since_ms,
        dry_run,
        machine: a.machine.clone(),
    };
    // Counted first, and said, before anything is read.
    let plan = match ask(ctx, &c, req(true)).await? {
        Reply::KbHistory { plan } => plan,
        r => return Err(unexpected(r)),
    };
    if a.dry_run || plan.new == 0 {
        if a.json {
            println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
        } else {
            print!("{}", printable_lines(&plan.text()));
        }
        return Ok(0);
    }
    if !a.json {
        eprint!("{}", printable_lines(&plan.text()));
    }
    let plan = match ask(ctx, &c, req(false)).await? {
        Reply::KbHistory { plan } => plan,
        r => return Err(unexpected(r)),
    };
    // Read behind everything else: wait until nothing is waiting. (A linked machine reads
    // at its own pace; its own `ptyro kb` says how far it is.)
    while a.machine.is_none() {
        let queued = match ask(ctx, &c, Request::KbStatus).await? {
            Reply::KbStatus { status } => status.queued,
            r => return Err(unexpected(r)),
        };
        if queued == 0 {
            progress_end();
            break;
        }
        progress(&format!("reading: {queued} waiting"));
        tokio::time::sleep(POLL).await;
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
    } else {
        println!(
            "read {}: `ptyro kb recent` lists them, `ptyro recall` searches them",
            plural(plan.new, "conversation", "conversations")
        );
    }
    Ok(0)
}

/// `ptyro kb ls`.
pub async fn list(ctx: &Ctx, machine: Option<String>, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let all = roots(ctx, &c, &machine).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
        return Ok(0);
    }
    if all.is_empty() {
        eprintln!(
            "nothing is read yet: `ptyro ingest PATH` reads a folder, and a project is read when ptyro knows it"
        );
        return Ok(0);
    }
    let wide = all.iter().map(|r| r.root.name.chars().count()).max().unwrap_or(4).max(4);
    println!("{:wide$}  KIND     STATE     HOLDS", "NAME");
    for r in &all {
        let state = match (r.removing, r.progress) {
            (true, _) => "removing".to_owned(),
            (_, Some((done, of))) if of > 0 => format!("{done}/{of}"),
            (_, Some(_)) => "waiting".to_owned(),
            _ => r.root.state.to_string(),
        };
        let mut about = vec![holds(r)];
        about.extend(r.root.synced_ms.map(|t| format!("read {} ago", ago(t))));
        about.extend(r.root.profile.as_ref().map(|p| format!("profile {p}")));
        about.push(r.root.path.clone());
        println!(
            "{:wide$}  {:7}  {:8}  {}",
            printable(&r.root.name),
            r.root.kind.to_string(),
            state,
            printable(&about.join(" · "))
        );
        if let Some(e) = &r.root.error {
            println!("{:wide$}  {}", "", printable(e));
        }
    }
    Ok(0)
}

/// `ptyro kb sync [ROOT…]`.
pub async fn sync(
    ctx: &Ctx,
    named: Vec<String>,
    machine: Option<String>,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let all_of_it = named.is_empty();
    let req = Request::KbSync { roots: named.clone(), machine: machine.clone() };
    let (notes, changed, removed, n) = match ask(ctx, &c, req).await? {
        Reply::KbSynced { notes, changed, removed, roots } => (notes, changed, removed, roots),
        r => return Err(unexpected(r)),
    };
    let ids: Vec<String> = if all_of_it {
        Vec::new()
    } else {
        let all = roots(ctx, &c, &machine).await?;
        all.into_iter()
            .filter(|r| {
                named.iter().any(|n| n == &r.root.name || n == &r.root.id || n == &r.root.path)
            })
            .map(|r| r.root.id)
            .collect()
    };
    let after = if n > 0 {
        settled(ctx, &c, &ids, &machine).await?
    } else {
        roots(ctx, &c, &machine).await?
    };
    if json {
        println!(
            "{}",
            json!({ "notes": notes, "changed": changed, "removed": removed, "roots": after })
        );
        return Ok(0);
    }
    if all_of_it {
        println!(
            "{notes} note{} in the folder: {changed} read again, {removed} gone",
            if notes == 1 { "" } else { "s" }
        );
    }
    for r in after.iter().filter(|r| ids.is_empty() || ids.contains(&r.root.id)) {
        let name = printable(&r.root.name);
        match (&r.root.state, &r.root.error) {
            (RootState::Missing | RootState::Error, e) => {
                let why = e.as_deref().map(printable);
                println!("{name}: {}", why.as_deref().unwrap_or("it could not be read"))
            }
            _ => println!("{name}: {}", holds(r)),
        }
    }
    Ok(0)
}

/// `ptyro kb rm ROOT`.
pub async fn remove(
    ctx: &Ctx,
    root: String,
    machine: Option<String>,
    yes: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let req =
        |confirmed| Request::KbRemove { root: root.clone(), confirmed, machine: machine.clone() };
    if !yes {
        match ask(ctx, &c, req(false)).await {
            Ok(_) => return Err(Error::internal("removed without asking")),
            Err(e) if e.code == ErrorCode::NeedsConfirmation => {
                crate::cli::ask_first(&printable_lines(&e.message), false)?;
            }
            Err(e) => return Err(e),
        }
    }
    let (name, items, chunks) = match ask(ctx, &c, req(true)).await? {
        Reply::KbRemoved { name, items, chunks } => (name, items, chunks),
        r => return Err(unexpected(r)),
    };
    // It goes in batches, off the request: wait until it is gone.
    loop {
        let all = roots(ctx, &c, &machine).await?;
        if !all.iter().any(|r| r.root.name == name) {
            progress_end();
            break;
        }
        progress(&format!("removing {name}…"));
        tokio::time::sleep(POLL).await;
    }
    if json {
        println!("{}", json!({ "name": name, "items": items, "chunks": chunks }));
    } else {
        let name = printable(&name);
        eprintln!(
            "{name} is no longer read: dropped {} ({}); your files aren't touched",
            plural(items, "item", "items"),
            pieces(chunks)
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct Ingest {
        #[command(flatten)]
        a: IngestArgs,
    }

    fn args(words: &[&str]) -> Result<IngestArgs, clap::Error> {
        Ingest::try_parse_from(std::iter::once("ingest").chain(words.iter().copied())).map(|i| i.a)
    }

    #[test]
    fn the_arguments() {
        let a = args(&[
            "docs",
            "~/notes",
            "--include",
            "*.md",
            "--include",
            "guides/**",
            "--exclude",
            "drafts",
            "--code",
            "--no-commits",
            "--once",
            "-p",
            "feeder",
            "--profile",
            "work",
        ])
        .unwrap();
        assert_eq!(a.paths.len(), 2);
        assert_eq!((a.include.len(), a.exclude.len()), (2, 1));
        assert!(a.code && a.no_commits && a.once && !a.no_comments && !a.history);
        assert_eq!((a.project.as_deref(), a.profile.as_deref()), (Some("feeder"), Some("work")));
        let h = args(&["--history", "--agent", "codex", "--since", "30d", "--dry-run"]).unwrap();
        assert!(h.history && h.dry_run && h.paths.is_empty());
        assert_eq!((h.agent.as_deref(), h.since.as_deref()), (Some("codex"), Some("30d")));
        // A path or the history, never both or neither; the history's own flags need it.
        assert!(args(&[]).is_err());
        assert!(args(&["docs", "--history"]).is_err());
        assert!(args(&["docs", "--dry-run"]).unwrap().check().is_err());
        assert!(args(&["docs", "-a", "codex"]).unwrap().check().is_err());
        assert!(h.check().is_ok() && a.check().is_ok());
        assert!(args(&["--history", "--code"]).is_err());
        assert!(args(&["--history", "--agent", "opencode"]).is_err());
    }

    /// Review B7: a file that was not kept is a root's by whole names, not by how its path
    /// begins.
    #[test]
    fn a_skipped_file_is_under_its_own_root_only() {
        assert_eq!(under("/a/doc", "file:///a/doc/x/y.md").as_deref(), Some("x/y.md"));
        assert_eq!(under("/a/doc", "file:///a/docs2/y.md"), None);
        assert_eq!(under("/a/doc.md", "file:///a/doc.md").as_deref(), Some("file:///a/doc.md"));
        assert_eq!(under("/a/doc", "session://claude/1"), None);
    }
}
