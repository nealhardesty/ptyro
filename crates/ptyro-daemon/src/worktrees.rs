//! Worktrees (SPEC §10.2–10.9): creation with includes, ports and the setup script; removal with
//! the safety matrix; lifecycle scripts; locks while sessions run; repository status;
//! reconciliation with `git worktree list`; gc.

use ptyro_core::sync::MutexExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ptyro_core::config::{StartAgents, WorktreeLocation};
use ptyro_core::model::{
    GcEntry, PortBlock, ProjectInfo, RemoveOpts, WorktreeInfo, WorktreeSpec, WorktreeState, new_id,
    render_branch, slugify,
};
use ptyro_core::project::{ProjectConfig, RunMode, ShareMode};
use ptyro_core::session::{SessionKind, SpawnSpec};
use ptyro_core::{Error, ErrorCode, SessionId, TermSize, names, now_ms};
use ptyro_git::{BranchFate, Git, Source};
use ptyro_protocol::{DomainEvent, Reply, Request};

use crate::daemon::{Daemon, user_shell};
use crate::launch::Launch;
use crate::registry::git_err;
use crate::session::{Place, Session};

const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Run blocking work (git, file copies) off the async workers.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::internal(e.to_string()))
}

/// Refuse a path whose ptyro-owned part contains a symlink (SPEC §10.2).
fn refuse_symlinks(ours: &Path, path: &Path) -> Result<(), Error> {
    let Ok(rest) = path.strip_prefix(ours) else { return Ok(()) };
    let mut p = ours.to_path_buf();
    let check = |p: &Path| -> Result<(), Error> {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::invalid(format!(
                "{} is a symlink; ptyro refuses symlinked worktree roots",
                p.display()
            )));
        }
        Ok(())
    };
    check(&p)?;
    for c in rest.components() {
        p.push(c);
        check(&p)?;
    }
    Ok(())
}

/// Where a worktree named `name` goes, and the directory ptyro owns above it.
fn worktree_path(
    location: WorktreeLocation,
    data: &Path,
    p: &ProjectInfo,
    name: &str,
) -> (PathBuf, PathBuf) {
    match location {
        WorktreeLocation::Data => {
            let ours = data.join("worktrees");
            (ours.join(&p.slug).join(name), ours)
        }
        WorktreeLocation::Sibling => {
            let leaf = p
                .root
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.slug.clone());
            let ours = p.root.parent().unwrap_or(&p.root).join(format!("{leaf}.worktrees"));
            (ours.join(name), ours)
        }
        WorktreeLocation::InRepo => {
            let ours = p.root.join(".ptyro");
            (ours.join("worktrees").join(name), ours)
        }
    }
}

/// Copy `.worktreeinclude` matches and `worktree.copy` files, share heavy dirs (SPEC §10.4).
fn bring_files(git: &Git, root: &Path, dst: &Path, cfg: &ProjectConfig) -> Vec<String> {
    let mut notes = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join(".worktreeinclude")) {
        let pats: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_owned)
            .collect();
        match git.untracked_matching(root, &pats, true) {
            Ok(f) => files.extend(f),
            Err(e) => notes.push(format!(".worktreeinclude: {e}")),
        }
    }
    match git.untracked_matching(root, &cfg.worktree.copy, false) {
        Ok(f) => files.extend(f.into_iter().filter(|f| !files.contains(f)).collect::<Vec<_>>()),
        Err(e) => notes.push(format!("worktree.copy: {e}")),
    }
    let shared: Vec<&String> = cfg.worktree.share.iter().collect();
    let mut copied = 0;
    for f in &files {
        // Shared directories are handled below as a whole.
        if shared.iter().any(|s| f.starts_with(s.trim_end_matches('/'))) {
            continue;
        }
        let (from, to) = (root.join(f), dst.join(f));
        // Never through a link: a tracked `config -> /elsewhere` in the new worktree (a PR
        // from a fork, say) would carry your secrets out of it.
        let linked = f.ancestors().any(|a| {
            !a.as_os_str().is_empty()
                && std::fs::symlink_metadata(dst.join(a)).is_ok_and(|m| m.file_type().is_symlink())
        });
        if linked || std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::copy(&from, &to) {
            Ok(_) => copied += 1,
            Err(e) => notes.push(format!("copy {}: {e}", f.display())),
        }
    }
    if copied > 0 {
        notes.push(format!("copied {copied} file(s) git doesn't carry"));
    }
    for s in shared {
        let rel = s.trim_end_matches('/');
        let (from, to) = (root.join(rel), dst.join(rel));
        if !from.exists() || to.exists() || rel.contains("..") || rel.starts_with('/') {
            continue;
        }
        // Never through a link in the new worktree either: a tracked `a -> ~/.ssh` with
        // `share = ["a/x"]` would put the link or the copy inside its target.
        let rel_path = Path::new(rel);
        let linked = rel_path.components().any(|c| !matches!(c, std::path::Component::Normal(_)))
            || rel_path.ancestors().any(|a| {
                !a.as_os_str().is_empty()
                    && std::fs::symlink_metadata(dst.join(a))
                        .is_ok_and(|m| m.file_type().is_symlink())
            });
        if linked {
            notes.push(format!("{rel}: not shared, a link in the worktree leads out of it"));
            continue;
        }
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let symlink = || std::os::unix::fs::symlink(&from, &to);
        let how = match cfg.worktree.share_mode {
            ShareMode::Symlink => symlink().map(|()| "linked"),
            ShareMode::Clone => match ptyro_git::clone_tree(&from, &to, false) {
                Ok(_) => Ok("cloned (copy-on-write)"),
                Err(_) => symlink().map(|()| "linked (no copy-on-write here)"),
            },
            ShareMode::Copy => ptyro_git::clone_tree(&from, &to, true)
                .map(|cow| if cow { "cloned (copy-on-write)" } else { "copied" }),
        };
        match how {
            Ok(h) => notes.push(format!("{rel}: {h}")),
            Err(e) => notes.push(format!("{rel}: {e}")),
        }
        // `node_modules/` ignores directories only; a symlink to one is a file to git, and would
        // count as untracked work. Ignore the entry itself (it was ignored in the main checkout).
        if git.run(root, &["check-ignore", "-q", &format!("{rel}/")]).is_ok() {
            let _ = git.exclude(root, &format!("/{rel}"));
        }
    }
    notes
}

fn port_free(p: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
}

impl Daemon {
    /// A free block of `ports.count` ports (SPEC §10.7): not given to another worktree, and
    /// every port bindable right now.
    fn allocate_ports(&self) -> Option<PortBlock> {
        let config = self.config();
        let (lo, hi) = config.ports.bounds().ok()?;
        let count = config.ports.count;
        let taken: Vec<PortBlock> =
            self.registry.locked().worktrees.iter().filter_map(|w| w.ports).collect();
        let mut base = u32::from(lo);
        while base + u32::from(count) - 1 <= u32::from(hi) {
            let b = base as u16;
            let block = PortBlock { base: b, count };
            let overlaps = taken.iter().any(|t| {
                u32::from(t.base) < base + u32::from(count)
                    && base < u32::from(t.base) + u32::from(t.count)
            });
            if !overlaps && (0..count).all(|i| port_free(b + i)) {
                return Some(block);
            }
            base += u32::from(count);
        }
        None
    }

    /// The branch for a new worktree from the template (SPEC §10.3).
    fn branch_for(&self, cfg: &ProjectConfig, name: &str) -> String {
        let template = cfg
            .worktree
            .branch_template
            .clone()
            .unwrap_or_else(|| self.config().worktrees.branch_template);
        let mut vars = BTreeMap::new();
        vars.insert("name", name.to_owned());
        vars.insert("slug", slugify(name));
        vars.insert("user", std::env::var("USER").unwrap_or_default());
        vars.insert("date", today());
        render_branch(&template, &vars)
    }

    /// SPEC §10.3: name → profile → branch and base → `git worktree add` → marker → includes →
    /// ports → setup script → agent.
    pub async fn create_worktree(self: &Arc<Self>, mut spec: WorktreeSpec) -> Result<Reply, Error> {
        // A path in a repository that isn't a project yet registers it (UX1.5), so
        // `ptyro "fix it"` or `new --new-wt` works in a fresh clone.
        let p = match self.find_project(&spec.project) {
            Ok(p) => p,
            Err(e) if Path::new(&spec.project).is_absolute() => self
                .auto_register(Path::new(&spec.project), spec.workspace.as_deref())
                .await
                .ok_or(e)?,
            Err(e) => return Err(e),
        };
        let mut issue_note = None;
        if let Some(issue) = spec.issue.clone() {
            issue_note = Some(self.from_issue(&p, &issue, &mut spec).await?);
        }
        let root_wt = self
            .root_worktree(&p.id)
            .ok_or_else(|| Error::missing("worktree", format!("{}/@root", p.slug)))?;
        let config = self.config();
        let root_cfg = ProjectConfig::load(&p.root, &p.root).map_err(Error::invalid)?;
        let taken = |n: &str| {
            let reg = self.registry.locked();
            reg.worktrees.iter().any(|w| w.project == p.id && w.name == n)
        };
        let name = match spec.name.clone() {
            Some(n) => {
                names::validate(&n).map_err(Error::invalid)?;
                if taken(&n) {
                    return Err(Error::conflict(format!("{}/{n} exists", p.slug)));
                }
                n
            }
            // From what it's for (UX1.24): `--title` or the prompt (`fix-flaky-test`), made unique;
            // else a friendly pair.
            None => {
                match spec.title.as_deref().or(spec.prompt.as_deref()).and_then(names::from_text) {
                    Some(n) if !taken(&n) => n,
                    Some(n) => {
                        (2..).map(|k| format!("{n}-{k}")).find(|c| !taken(c)).expect("infinite")
                    }
                    None => {
                        names::friendly(SessionId::new().seed(), |n| taken(n) || self.name_taken(n))
                    }
                }
            }
        };
        let explicit = spec.profile.clone().or_else(|| {
            spec.agent.as_deref().and_then(|a| ptyro_agents::split_agent(a).1.map(str::to_owned))
        });
        let workspace = self.home_workspace(spec.workspace.as_deref(), Some(&p))?;
        let target = (root_wt.clone(), p.clone());
        let res = self.resolve_profile(
            explicit.as_deref(),
            Some(&target),
            &workspace,
            spec.override_pin,
            spec.confirmed,
        )?;
        if let Some(q) = res.question {
            return Err(Error::new(ErrorCode::NeedsConfirmation, q));
        }
        let mut notes: Vec<String> = res.warning.into_iter().chain(issue_note).collect();
        notes.push(format!("profile {} ({})", res.profile, res.reason.label()));
        // `take_changes` (UX1.10): the main checkout's uncommitted work, snapshot first; the
        // worktree starts where the main checkout is, so the changes apply as they were.
        let taken = if spec.take_changes {
            let root = p.root.clone();
            Some(
                blocking(move || -> Result<ptyro_git::orch::Snapshot, Error> {
                    let git = Git::new();
                    if git.status(&root, None).map_err(git_err)?.dirty() == 0 {
                        return Err(Error::invalid(
                            "nothing to take: the main checkout has no uncommitted changes",
                        ));
                    }
                    git.checkpoint(&root, "ptyro: changes taken into a new worktree")
                        .map_err(git_err)
                })
                .await??,
            )
        } else {
            None
        };
        let (mut source, branch) = match (spec.pr, spec.existing, spec.branch.clone()) {
            (Some(n), _, b) => {
                let branch = b.unwrap_or_else(|| format!("pr/{n}"));
                let remote = Git::new()
                    .main_remote(&p.root)
                    .map(|r| r.name)
                    .unwrap_or_else(|| "origin".into());
                (Source::PullRequest { number: n, branch: branch.clone(), remote }, branch)
            }
            (None, true, Some(b)) => (Source::Existing { branch: b.clone() }, b),
            (None, true, None) => return Err(Error::invalid("--existing needs --branch")),
            (None, false, b) => {
                let branch = b.unwrap_or_else(|| self.branch_for(&root_cfg, &name));
                let base = spec
                    .base
                    .clone()
                    .or_else(|| root_cfg.project.base.clone())
                    .or_else(|| root_wt.base.clone())
                    .or_else(|| p.default_branch.clone())
                    .unwrap_or_else(|| "HEAD".into());
                (Source::NewBranch { branch: branch.clone(), base }, branch)
            }
        };
        if let Some(head) = taken.as_ref().and_then(|t| t.head.clone())
            && let Source::NewBranch { base, .. } = &mut source
        {
            *base = head;
        }
        // A branch that came from elsewhere (a PR from a fork, a branch someone else made)
        // carries its own `.ptyro.toml`. What may be copied out of your checkout, and what may be
        // shared, is your checkout's say, not that file's: its scripts are trust-gated, this isn't.
        let foreign = matches!(source, Source::PullRequest { .. } | Source::Existing { .. });
        let own_files = (
            root_cfg.worktree.copy.clone(),
            root_cfg.worktree.share.clone(),
            root_cfg.worktree.share_mode,
        );
        let base = match &source {
            Source::NewBranch { base, .. } => Some(base.clone()),
            _ => root_wt.base.clone(),
        };
        let (path, ours) = match &spec.path {
            Some(at) => (at.clone(), at.parent().map(Path::to_path_buf).unwrap_or_default()),
            None => worktree_path(config.worktrees.location, &self.paths.data, &p, &name),
        };
        refuse_symlinks(&ours, &path)?;
        if path.exists() {
            return Err(Error::conflict(format!("{} already exists", path.display())));
        }
        // git runs under the worktree's profile, so a fetch uses that profile's SSH key. Before
        // the worktree is registered: a failure here leaves nothing half-made.
        let plan = self.profile_plan(&res.profile).await?;
        let mut wt = WorktreeInfo {
            machine: None,
            id: new_id(),
            project: p.id.clone(),
            project_slug: p.slug.clone(),
            name: name.clone(),
            path: path.clone(),
            branch: Some(branch),
            base: base.clone(),
            is_root: false,
            profile: Some(res.profile.clone()),
            state: WorktreeState::Creating,
            ports: None,
            created_ms: now_ms(),
            removed_ms: None,
            last_used_ms: None,
            status: None,
            locked: false,
            pr: None,
            title: spec
                .title
                .clone()
                .or_else(|| spec.prompt.clone())
                .map(|t| ptyro_core::session::brief(&t, 120)),
            integrated: None,
        };
        self.put_worktree(wt.clone());
        let progress = |step: &str| {
            let _ = self.events.send(DomainEvent::WorktreeProgress {
                worktree: format!("{}/{name}", p.slug),
                step: step.into(),
            });
        };
        let env = plan.apply(self.base_env.iter().cloned());
        let fetch = spec.fetch.unwrap_or(config.worktrees.fetch);
        let (root, location) = (p.root.clone(), config.worktrees.location);
        progress("checking out");
        let git_steps = {
            let (path, source, wt_id, p_id) =
                (path.clone(), source.clone(), wt.id.clone(), p.id.clone());
            let name = name.clone();
            blocking(move || -> Result<Vec<String>, Error> {
                let git = Git::with_env(env);
                let mut notes = Vec::new();
                if fetch
                    && let Source::NewBranch { base, .. } = &source
                    && let Some((remote, branch)) = base.split_once('/')
                    && git.remotes(&root).iter().any(|r| r.name == remote)
                    && let Err(e) = git.fetch(&root, remote, branch)
                {
                    notes.push(format!("fetch failed, using the local {base} ({e})"));
                }
                if location == WorktreeLocation::InRepo {
                    git.exclude(&root, "/.ptyro/").map_err(git_err)?;
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| Error::internal(format!("{}: {e}", parent.display())))?;
                }
                git.worktree_add(&root, &path, &source).map_err(git_err)?;
                if let Ok(dir) = git.git_dir(&path) {
                    let marker = serde_json::json!({ "id": wt_id, "project": p_id, "name": name, "created_at": now_ms() });
                    let _ = std::fs::write(dir.join("ptyro.json"), marker.to_string());
                }
                Ok(notes)
            })
            .await?
        };
        match git_steps {
            Ok(n) => notes.extend(n),
            Err(e) => {
                self.drop_worktree(wt);
                return Err(e);
            }
        }
        if let Some(t) = taken {
            // A copy first; the main checkout gives its changes up only once they're here.
            let (root, path, commit, tree, refname) = (
                p.root.clone(),
                path.clone(),
                t.commit,
                t.tree,
                format!("refs/ptyro/taken/{name}"),
            );
            let shown = refname.clone();
            let moved = blocking(move || -> Result<(), ptyro_git::GitError> {
                let git = Git::new();
                git.update_ref(&root, &refname, &commit)?;
                git.restore(&path, &commit)?;
                // Fetching and checking out took a while: whatever was written in the main
                // checkout since the snapshot isn't in the worktree, so don't wipe it.
                let now = git.checkpoint(&root, "ptyro: re-checking before taking changes")?;
                if now.tree != tree {
                    return Err(ptyro_git::GitError::Failed {
                        args: "take changes".into(),
                        message: "the main checkout changed while the worktree was being made"
                            .into(),
                    });
                }
                git.run(&root, &["reset", "-q", "--hard"])?;
                git.run(&root, &["clean", "-q", "-fd"])?;
                Ok(())
            })
            .await?;
            notes.push(match moved {
                Ok(()) => format!("took the main checkout's uncommitted changes (a copy stays at {shown})"),
                Err(e) => format!(
                    "couldn't move the main checkout's changes ({e}); they're still there, and a copy is at {shown}"
                ),
            });
        }
        let mut cfg = ProjectConfig::load(&p.root, &path).unwrap_or(root_cfg);
        if foreign {
            (cfg.worktree.copy, cfg.worktree.share, cfg.worktree.share_mode) = own_files;
        }
        progress("copying files git doesn't carry");
        let file_notes = {
            let (root, path, cfg) = (p.root.clone(), path.clone(), cfg.clone());
            blocking(move || bring_files(&Git::new(), &root, &path, &cfg)).await?
        };
        notes.extend(file_notes);
        wt.state = WorktreeState::Ready;
        if let Ok(s) = Git::new().status(&path, base.as_deref()) {
            wt.status = Some(s);
        }
        {
            // Two worktrees made at once must not be given the same block: choosing one and
            // recording it are one step.
            static PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _one = PORTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            wt.ports = self.allocate_ports();
            match wt.ports {
                Some(b) => notes.push(format!("ports {}-{}", b.base, b.base + b.count - 1)),
                None => notes.push("no free port block (see [ports] in config)".into()),
            }
            self.put_worktree(wt.clone());
        }
        self.watch_worktree_bg(&wt);
        // Setup: only trusted scripts run (SPEC §10.5).
        let scripts = self.scripts_info(&p, &wt)?;
        if let Some(fp) = &spec.trust
            && *fp == scripts.fingerprint
        {
            self.trust(&p.id, fp);
        }
        let trusted = cfg.scripts.is_empty() || self.is_trusted(&p.id, &scripts.fingerprint);
        let mut untrusted = None;
        let mut setup = None;
        if cfg.scripts.setup.is_some() && spec.setup {
            if trusted {
                progress("running setup");
                setup =
                    Some(self.run_script(&wt, Some("setup"), spec.size, spec.env.clone()).await?);
            } else {
                notes.push(format!(
                    "setup not run: scripts untrusted (`ptyro project trust {}`)",
                    p.slug
                ));
                untrusted = Some(self.scripts_info(&p, &wt)?);
            }
        }
        let start = cfg.worktree.start_agents.unwrap_or(config.worktrees.start_agents);
        let mut agent_info = None;
        let mut agent_pending = false;
        if let Some(agent) = spec.agent.clone() {
            let (agent, _) = ptyro_agents::split_agent(&agent);
            let spawn = SpawnSpec {
                agent: Some(agent.to_owned()),
                cwd: path.clone(),
                worktree: Some(wt.id.clone()),
                env: spec.env.clone(),
                size: spec.size,
                workspace: Some(workspace.clone()),
                confirmed: true,
                prompt: spec.prompt.clone(),
                verify: spec.verify.clone(),
                lead: spec.lead.clone(),
                structured: spec.structured,
                ..SpawnSpec::default()
            };
            match (&setup, start) {
                _ if self.review_gate().is_some() => {
                    let n = self.review_gate().unwrap_or(0);
                    let pos = self.queue_spawn(spawn);
                    agent_pending = true;
                    notes.push(format!(
                        "{n} worktrees wait for review (review.max_unreviewed): the agent starts when you've reviewed one (queued #{pos})"
                    ));
                }
                (Some(s), StartAgents::AfterSetup) => {
                    agent_pending = true;
                    let (me, s) = (Arc::clone(self), Arc::clone(s));
                    tokio::spawn(async move {
                        s.wait_exit().await;
                        if let Err(e) = me.spawn(spawn).await {
                            tracing::warn!("agent after setup: {}", e.message);
                        }
                    });
                }
                _ => {
                    let s = self.spawn(spawn).await?;
                    agent_info = Some(self.info(&s));
                }
            }
        }
        progress("ready");
        // Started from a scratchpad item (NOTE1.3): the item says where it is being worked.
        if let Some(line) = spec.pad_item {
            self.pad_started(line, &wt.qualified());
        }
        Ok(Reply::WorktreeCreated {
            info: wt,
            setup: setup.map(|s| self.info(&s)),
            agent: agent_info,
            agent_pending,
            untrusted,
            notes,
            issue_prompt: spec
                .issue
                .as_ref()
                .filter(|_| spec.agent.is_none())
                .and(spec.prompt.clone()),
        })
    }

    /// Run a lifecycle script in a `script` session (SPEC §10.5). `which`: `setup`, `archive`,
    /// `after_turn`, a run script name (`dev`, `run:dev`), or `None` for the default run script.
    pub async fn run_script(
        self: &Arc<Self>,
        wt: &WorktreeInfo,
        which: Option<&str>,
        size: Option<TermSize>,
        client_env: BTreeMap<String, String>,
    ) -> Result<Arc<Session>, Error> {
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let cfg = ProjectConfig::load(&p.root, &wt.path).map_err(Error::invalid)?;
        let (role, command) = match which {
            None => {
                let (n, r) = cfg.scripts.run_script(None).ok_or_else(|| {
                    Error::missing(
                        "run script",
                        "default (mark one with `default = true` in .ptyro.toml)",
                    )
                })?;
                (format!("run:{n}"), r.command.clone())
            }
            Some(w) => {
                let role = match w {
                    "setup" | "archive" | "after_turn" => w.to_owned(),
                    other => format!("run:{}", other.strip_prefix("run:").unwrap_or(other)),
                };
                let cmd =
                    cfg.scripts.command(&role).ok_or_else(|| Error::missing("script", &role))?;
                (role, cmd)
            }
        };
        let fp = cfg.trust_fingerprint();
        if !self.is_trusted(&p.id, &fp) {
            return Err(Error::new(
                ErrorCode::UntrustedProject,
                format!(
                    "{}'s scripts are not trusted; review them with `ptyro project trust {}`",
                    p.slug, p.slug
                ),
            ));
        }
        if role.starts_with("run:") && cfg.scripts.run_mode == RunMode::Exclusive {
            let others: Vec<Arc<Session>> = self
                .sessions()
                .into_iter()
                .filter(|s| {
                    let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    m.state.is_live()
                        && m.place.project.as_deref() == Some(p.id.as_str())
                        && m.place.worktree.as_deref() != Some(wt.id.as_str())
                        && m.place.script.as_deref().is_some_and(|r| r.starts_with("run:"))
                })
                .collect();
            futures::future::join_all(others.iter().map(|s| s.kill(None))).await;
        }
        let target = (wt.clone(), p.clone());
        let workspace = self.home_workspace(None, Some(&p))?;
        // Scripts can't ask questions; a refusing guard still refuses.
        let res = self.resolve_profile(None, Some(&target), &workspace, false, true)?;
        let label = role.strip_prefix("run:").unwrap_or(&role);
        let name = self.free_name(&format!("{}.{label}", wt.name.trim_start_matches('@')));
        self.launch(Launch {
            id: SessionId::new(),
            argv: vec![user_shell(), "-c".into(), command],
            kind: SessionKind::Script,
            name,
            cwd: wt.path.clone(),
            size: size.unwrap_or(TermSize::DEFAULT),
            client_env,
            place: Place {
                profile: res.profile,
                worktree: Some(wt.id.clone()),
                project: Some(p.id.clone()),
                workspace: Some(workspace),
                agent: None,
                script: Some(role),
                quick: false,
                pad: false,
                chat: None,
            },
            replace: false,
            agent: None,
        })
        .await
    }

    /// SPEC §10.8, safety-critical: refuse while sessions run or work would be lost, unless told
    /// otherwise; archive script; remove; keep the branch unless nothing is lost.
    pub async fn remove_worktree(
        self: &Arc<Self>,
        addr: &str,
        opts: RemoveOpts,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        if wt.is_root {
            return Err(Error::invalid(format!(
                "{} is the project's main checkout; use `ptyro project rm {}` to forget the project",
                wt.qualified(),
                wt.project_slug
            )));
        }
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let sessions = self.sessions_in(&wt.id);
        let live: Vec<&Arc<Session>> = sessions.iter().filter(|s| s.is_live()).collect();
        // A quick terminal at its prompt goes with its worktree (UX1.25).
        let busy: Vec<&&Arc<Session>> = live.iter().filter(|s| s.busy()).collect();
        if !busy.is_empty() && !opts.stop_sessions {
            let names: Vec<String> = busy
                .iter()
                .map(|s| {
                    s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone()
                })
                .collect();
            return Err(Error::conflict(format!(
                "refusing to remove {}: {} session(s) running in it ({}); stop them or use --stop-sessions",
                wt.qualified(),
                names.len(),
                names.join(", ")
            )));
        }
        let mut notes = Vec::new();
        let exists = wt.path.exists();
        if exists {
            let (path, branch, base) = (wt.path.clone(), wt.branch.clone(), wt.base.clone());
            let safety =
                blocking(move || Git::new().safety(&path, branch.as_deref(), base.as_deref()))
                    .await?
                    .map_err(git_err)?;
            if !safety.is_clean() && !opts.force {
                return Err(Error::new(
                    ErrorCode::DirtyWorktree,
                    format!(
                        "refusing to remove {} — it has work that would be lost:\n  {}\nCommit or push it, or use --force to discard it.",
                        wt.qualified(),
                        safety.report()
                    ),
                ));
            }
            if !safety.is_clean() {
                notes.push(format!("discarded: {}", safety.report().replace("\n  ", "; ")));
            }
            // Not a reason to refuse (every build leaves some), but the person should hear it.
            if let Some(n) = safety.ignored_note() {
                notes.push(n);
            }
        }
        futures::future::join_all(live.iter().map(|s| s.kill(None))).await;
        let _ = self.events.send(DomainEvent::WorktreeProgress {
            worktree: wt.qualified(),
            step: "removing".into(),
        });
        if exists {
            let cfg = ProjectConfig::load(&p.root, &wt.path).unwrap_or_default();
            if cfg.scripts.archive.is_some() && self.is_trusted(&p.id, &cfg.trust_fingerprint()) {
                match self.run_script(&wt, Some("archive"), None, BTreeMap::new()).await {
                    Ok(s) => {
                        if tokio::time::timeout(ARCHIVE_TIMEOUT, s.wait_exit()).await.is_err() {
                            let _ = s.kill(None).await;
                            notes.push("archive script timed out and was stopped".into());
                        } else {
                            notes.push(format!(
                                "archive script {}",
                                s.meta
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .state
                                    .label()
                            ));
                        }
                        self.forget_session(s.id);
                    }
                    Err(e) => notes.push(format!("archive script not run: {}", e.message)),
                }
            }
        }
        let (root, path, branch, base, force, keep) = (
            p.root.clone(),
            wt.path.clone(),
            wt.branch.clone(),
            wt.base.clone(),
            opts.force,
            opts.keep_branch,
        );
        let git_notes = blocking(move || -> Result<Vec<String>, Error> {
            let git = Git::new();
            let mut notes = Vec::new();
            // Ptyro's own lock goes; a lock the user set (`git worktree lock`) stays, and with
            // it a refusal to remove unless they forced it.
            let _ = git.unlock_ours(&root, &path);
            if path.exists() {
                git.worktree_remove(&root, &path, force).map_err(git_err)?;
            } else {
                let _ = git.run(&root, &["worktree", "prune"]);
            }
            if let Some(b) = branch {
                match (keep, git.branch_fate(&root, &b, base.as_deref())) {
                    (true, _) => notes.push(format!("kept branch {b} (--keep-branch)")),
                    (false, BranchFate::Delete(why)) => match git.delete_branch(&root, &b) {
                        Ok(()) => notes.push(format!("deleted branch {b} ({why})")),
                        Err(e) => notes.push(format!("kept branch {b}: {e}")),
                    },
                    (false, BranchFate::Keep(why)) => notes.push(format!("kept branch {b}: {why}")),
                }
            }
            Ok(notes)
        })
        .await??;
        notes.extend(git_notes);
        if let Some(b) = wt.ports {
            notes.push(format!("released ports {}-{}", b.base, b.base + b.count - 1));
        }
        self.unwatch(&wt.id);
        for s in &sessions {
            self.forget_session(s.id);
        }
        self.drop_worktree(wt.clone());
        self.pad_finished(&wt.qualified(), wt.pr.as_ref().map(|p| u64::from(p.number)));
        notes.insert(0, format!("removed {}", wt.qualified()));
        Ok(Reply::Removed { notes })
    }

    /// Recompute repository status (and notice a vanished or returned directory).
    pub async fn refresh_status(self: &Arc<Self>, id: &str) -> Option<WorktreeInfo> {
        let wt = self.worktree_by_id(id)?;
        if wt.state == WorktreeState::Creating {
            return Some(wt);
        }
        let exists = wt.path.is_dir();
        match (exists, wt.state) {
            (false, WorktreeState::Ready) => {
                let mut w = wt;
                w.state = WorktreeState::Missing;
                w.status = None;
                self.put_worktree(w.clone());
                return Some(w);
            }
            (false, _) => return Some(wt),
            (true, WorktreeState::Missing) => {
                let mut w = wt.clone();
                w.state = WorktreeState::Ready;
                self.put_worktree(w);
            }
            _ => {}
        }
        let (path, base) = (wt.path.clone(), wt.base.clone());
        let status =
            blocking(move || Git::new().status(&path, base.as_deref())).await.ok()?.ok()?;
        let branch = status.branch.clone();
        let changed = wt.status.as_ref().is_none_or(|s| !same_status(s, &status))
            || (branch.is_some() && branch != wt.branch);
        if !changed {
            return Some(wt);
        }
        if branch.is_some() && branch != wt.branch {
            // Checked out another branch by hand: follow it.
            let mut w = wt.clone();
            w.branch = branch;
            w.status = Some(status);
            self.put_worktree(w.clone());
            return Some(w);
        }
        self.update_worktree(id, |w| w.status = Some(status))
    }

    /// `git worktree lock` worktrees with live sessions, unlock the rest (SPEC §10.3 step 3).
    pub async fn sync_locks(self: &Arc<Self>) {
        let worktrees: Vec<WorktreeInfo> = {
            let reg = self.registry.locked();
            reg.worktrees
                .iter()
                .filter(|w| !w.is_root && w.state == WorktreeState::Ready)
                .cloned()
                .collect()
        };
        for wt in worktrees {
            let live = self.sessions_in(&wt.id).iter().any(|s| s.is_live());
            if live == wt.locked {
                continue;
            }
            let Some(p) = self.project_by_id(&wt.project) else { continue };
            let (root, path, id) = (p.root.clone(), wt.path.clone(), wt.id.clone());
            let ok = blocking(move || {
                let git = Git::new();
                if live {
                    git.lock(&root, &path, &format!("ptyro:{id}"))
                } else {
                    git.unlock(&root, &path)
                }
            })
            .await
            .is_ok_and(|r| r.is_ok());
            if ok {
                self.update_worktree(&wt.id, |w| w.locked = live);
            }
        }
    }

    /// SPEC §7.5: missing directories are marked (never pruned); unknown ptyro-marked worktrees
    /// are adopted again; every project has its `@root`.
    pub async fn reconcile(self: &Arc<Self>) {
        let projects = self.registry.locked().projects.clone();
        for p in projects {
            let root = p.root.clone();
            let listed = blocking(move || {
                let git = Git::new();
                git.worktree_list(&root).map(|list| {
                    list.into_iter()
                        .map(|e| {
                            let marker = git
                                .git_dir(&e.path)
                                .ok()
                                .and_then(|d| std::fs::read_to_string(d.join("ptyro.json")).ok())
                                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
                            (e, marker)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .await;
            let Ok(Ok(listed)) = listed else {
                tracing::warn!(project = %p.slug, "cannot list worktrees (repository moved?)");
                continue;
            };
            let known: Vec<WorktreeInfo> = {
                let reg = self.registry.locked();
                reg.worktrees.iter().filter(|w| w.project == p.id).cloned().collect()
            };
            for w in &known {
                let exists = w.path.is_dir();
                if !exists && w.state == WorktreeState::Ready {
                    let mut m = w.clone();
                    m.state = WorktreeState::Missing;
                    tracing::warn!(worktree = %w.qualified(), "directory missing");
                    self.put_worktree(m);
                } else if exists && w.state == WorktreeState::Missing {
                    let mut m = w.clone();
                    m.state = WorktreeState::Ready;
                    self.put_worktree(m);
                } else if w.state == WorktreeState::Creating {
                    let mut m = w.clone();
                    m.state = if exists { WorktreeState::Ready } else { WorktreeState::Failed };
                    self.put_worktree(m);
                }
            }
            for (e, marker) in listed {
                let Some(m) = marker else { continue };
                let (Some(id), Some(project), Some(name)) =
                    (m["id"].as_str(), m["project"].as_str(), m["name"].as_str())
                else {
                    continue;
                };
                if project != p.id || known.iter().any(|w| w.id == id || w.path == e.path) {
                    continue;
                }
                tracing::info!(worktree = %format!("{}/{name}", p.slug), "re-adopted");
                self.put_worktree(WorktreeInfo {
                    machine: None,
                    id: id.into(),
                    project: p.id.clone(),
                    project_slug: p.slug.clone(),
                    name: name.into(),
                    path: e.path.clone(),
                    branch: e.branch.clone(),
                    base: self.root_worktree(&p.id).and_then(|r| r.base),
                    is_root: false,
                    profile: None,
                    state: WorktreeState::Ready,
                    ports: self.allocate_ports(),
                    created_ms: m["created_at"].as_u64().unwrap_or_else(now_ms),
                    removed_ms: None,
                    last_used_ms: None,
                    status: None,
                    locked: false,
                    pr: None,
                    title: None,
                    integrated: None,
                });
            }
        }
        // ptyro's locks don't survive a daemon that's gone: start from what git says.
        let ids: Vec<String> =
            self.registry.locked().worktrees.iter().map(|w| w.id.clone()).collect();
        for id in &ids {
            let _ = self.status_tx.send(id.clone());
        }
    }

    /// Stale worktrees (SPEC §10.8): gone from disk, branch merged (a squash-merged PR too), or
    /// unused for a while; a locked one is being kept, so only its missing directory counts.
    /// Each comes with what it takes on disk, the biggest first.
    pub(crate) async fn gc(self: &Arc<Self>) -> Result<Reply, Error> {
        let stale_days = self.config().worktrees.stale_days;
        let worktrees: Vec<WorktreeInfo> = {
            let reg = self.registry.locked();
            reg.worktrees.iter().filter(|w| !w.is_root).cloned().collect()
        };
        let mut entries = Vec::new();
        for w in worktrees {
            let mut reasons = Vec::new();
            if !w.path.exists() {
                reasons.push("its directory is gone".to_owned());
            } else if w.locked {
                continue;
            }
            if let (Some(b), Some(p)) = (&w.branch, self.project_by_id(&w.project))
                && w.path.exists()
            {
                let (root, b, base) = (p.root.clone(), b.clone(), w.base.clone());
                let merged = blocking(move || {
                    let git = Git::new();
                    let tip = git.rev(&root, &format!("refs/heads/{b}"))?;
                    let base = base.filter(|x| git.ref_exists(&root, x))?;
                    // Merged: it has commits of its own (it moved from where its reflog says it
                    // was made), and they are in the base. (Before, this asked for commits that
                    // weren't in the base *and* were: never both, so it never said merged.)
                    let made = git
                        .run(&root, &["reflog", "show", "--format=%H", &format!("refs/heads/{b}")])
                        .ok()?
                        .lines()
                        .last()
                        .map(str::to_owned)?;
                    (made != tip && git.is_ancestor(&root, &tip, &base)).then_some(base)
                })
                .await?;
                if let Some(base) = merged {
                    reasons.push(format!("branch merged into {base}"));
                }
            }
            // A squash or rebase merge leaves the branch's commits out of the base: the PR says.
            if w.path.exists() && reasons.is_empty() && w.shipped() {
                reasons.push(
                    if w.pr.as_ref().is_some_and(|p| p.state == "MERGED") {
                        "its PR merged"
                    } else {
                        "integrated into the main branch"
                    }
                    .to_owned(),
                );
            }
            let has_sessions = !self.sessions_in(&w.id).is_empty();
            let last = w.last_used_ms.unwrap_or(w.created_ms);
            let days = now_ms().saturating_sub(last) / 86_400_000;
            if !has_sessions && days >= u64::from(stale_days) {
                reasons.push(format!("no sessions for {days} days"));
            }
            if !reasons.is_empty() {
                entries.push(GcEntry { worktree: w, reasons, bytes: None, bytes_partial: false });
            }
        }
        self.measure(&mut entries).await;
        entries.sort_by_key(|e| std::cmp::Reverse(e.bytes.unwrap_or(0)));
        Ok(Reply::Gc { entries })
    }

    /// What each entry's directory takes on disk: measured at most every half hour (the doctor
    /// asks every ten minutes), side by side, each giving up after a few seconds.
    async fn measure(&self, entries: &mut [GcEntry]) {
        const FRESH: Duration = Duration::from_secs(1800);
        const BUDGET: Duration = Duration::from_secs(3);
        let todo: Vec<(String, PathBuf)> = {
            let sizes = self.gc_sizes.locked();
            entries
                .iter()
                .filter(|e| e.worktree.path.exists())
                .filter(|e| sizes.get(&e.worktree.id).is_none_or(|(_, _, at)| at.elapsed() > FRESH))
                .map(|e| (e.worktree.id.clone(), e.worktree.path.clone()))
                .collect()
        };
        let measured = futures::future::join_all(todo.into_iter().map(|(id, path)| {
            tokio::task::spawn_blocking(move || (id, disk_usage(&path, BUDGET)))
        }))
        .await;
        let mut sizes = self.gc_sizes.locked();
        let now = std::time::Instant::now();
        for (id, (bytes, partial)) in measured.into_iter().flatten() {
            sizes.insert(id, (bytes, partial, now));
        }
        sizes.retain(|id, _| entries.iter().any(|e| &e.worktree.id == id));
        for e in entries.iter_mut() {
            if let Some((bytes, partial, _)) = sizes.get(&e.worktree.id) {
                (e.bytes, e.bytes_partial) = (Some(*bytes), *partial);
            }
        }
    }

    /// `wt rename-branch` (SPEC §10.3): never once pushed. Without a name, one is derived from the
    /// first commit's subject.
    /// Rename to the template's branch for `name` (auto-rename from the first prompt).
    pub(crate) async fn rename_branch_to(
        self: &Arc<Self>,
        wt_id: &str,
        name: &str,
    ) -> Result<Reply, Error> {
        let wt = self.worktree_by_id(wt_id).ok_or_else(|| Error::missing("worktree", wt_id))?;
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let cfg = ProjectConfig::load(&p.root, &wt.path).unwrap_or_default();
        let branch = self.branch_for(&cfg, name);
        self.rename_branch(&wt.id, Some(branch)).await
    }

    async fn rename_branch(
        self: &Arc<Self>,
        addr: &str,
        new: Option<String>,
    ) -> Result<Reply, Error> {
        let mut wt = self.find_worktree(addr)?;
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let old = wt
            .branch
            .clone()
            .ok_or_else(|| Error::invalid("the worktree has no branch (detached)"))?;
        let cfg = ProjectConfig::load(&p.root, &wt.path).unwrap_or_default();
        // The first prompt someone gave an agent here names the work best (M3.16).
        let prompt = self.sessions_in(&wt.id).iter().find_map(|s| {
            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).first_prompt.clone()
        });
        let new = match new {
            Some(n) => n,
            None if prompt.is_some() => {
                let slug: String = slugify(prompt.as_deref().unwrap_or_default())
                    .split('-')
                    .take(6)
                    .collect::<Vec<_>>()
                    .join("-");
                self.branch_for(&cfg, &slug)
            }
            None => {
                let (path, base) =
                    (wt.path.clone(), wt.base.clone().unwrap_or_else(|| "HEAD".into()));
                let first = blocking(move || Git::new().subjects_since(&path, &base))
                    .await?
                    .into_iter()
                    .next();
                let first = first.ok_or_else(|| {
                    Error::invalid("no commits yet to name the branch after; give a name")
                })?;
                let slug: String = slugify(&first).split('-').take(6).collect::<Vec<_>>().join("-");
                self.branch_for(&cfg, &slug)
            }
        };
        let (root, o, n) = (p.root.clone(), old.clone(), new.clone());
        blocking(move || {
            let git = Git::new();
            if git.is_published(&root, &o) {
                return Err(Error::conflict(format!(
                    "{o} has been pushed; renaming it would orphan the remote branch"
                )));
            }
            git.rename_branch(&root, &o, &n).map_err(git_err)
        })
        .await??;
        wt.branch = Some(new);
        self.put_worktree(wt.clone());
        Ok(Reply::Worktree { info: wt })
    }

    pub(crate) async fn handle_worktrees(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::WorktreeList { project } => {
                let pid = project.map(|p| self.find_project(&p)).transpose()?.map(|p| p.id);
                let reg = self.registry.locked();
                let worktrees = reg
                    .worktrees
                    .iter()
                    .filter(|w| pid.as_ref().is_none_or(|p| w.project == *p))
                    .cloned()
                    .collect();
                Ok(Reply::Worktrees { worktrees })
            }
            Request::WorktreeGet { addr } => {
                Ok(Reply::Worktree { info: self.find_worktree(&addr)? })
            }
            Request::WorktreeNew { spec } => {
                let (agent, profile, asked) =
                    (spec.agent.clone(), spec.profile.clone(), spec.workspace.clone());
                let reply = self.create_worktree(spec).await?;
                // Its agent counts for quick-new (UX3.2), as `Spawn` does.
                if let (Some(agent), Reply::WorktreeCreated { info, .. }) = (agent, &reply)
                    && let Ok(p) = self.find_project(&info.project)
                    && let Ok(ws) = self.home_workspace(asked.as_deref(), Some(&p))
                {
                    let spec = SpawnSpec { agent: Some(agent), profile, ..SpawnSpec::default() };
                    self.remember_launch(&ws, &spec).await;
                }
                Ok(reply)
            }
            Request::WorktreeRemove { addr, opts } => self.remove_worktree(&addr, opts).await,
            Request::WorktreeArchive { addr, stop_sessions } => {
                self.archive_worktree(&addr, stop_sessions).await
            }
            Request::WorktreeRestore { addr } => self.restore_worktree(&addr).await,
            Request::WorktreeArchiveDelete { addr } => self.delete_archive(&addr).await,
            Request::WorktreeArchives { project } => self.list_archives(project.as_deref()).await,
            Request::ProjectSuggest { addr, apply } => self.project_suggest(&addr, apply).await,
            Request::Branches { project } => {
                let p = self.find_project(&project)?;
                let used: Vec<String> = {
                    let reg = self.registry.locked();
                    reg.worktrees
                        .iter()
                        .filter(|w| w.project == p.id)
                        .filter_map(|w| w.branch.clone())
                        .collect()
                };
                let root = p.root.clone();
                let names = blocking(move || {
                    let git = Git::new();
                    let refs = |pattern: &str| -> Vec<String> {
                        git.run(
                            &root,
                            &[
                                "for-each-ref",
                                "--sort=-committerdate",
                                "--format=%(refname:short)",
                                pattern,
                            ],
                        )
                        .map(|o| o.lines().map(str::to_owned).collect())
                        .unwrap_or_default()
                    };
                    let local = refs("refs/heads/");
                    let remote = git.main_remote(&root).map(|r| r.name);
                    let mut names: Vec<String> =
                        local.iter().filter(|b| !used.contains(b)).cloned().collect();
                    if let Some(r) = remote {
                        for b in refs(&format!("refs/remotes/{r}/")) {
                            let short = b.strip_prefix(&format!("{r}/")).unwrap_or(&b).to_owned();
                            if short != "HEAD" && !local.contains(&short) {
                                names.push(short);
                            }
                        }
                    }
                    names
                })
                .await?;
                Ok(Reply::Branches { names })
            }
            Request::WorktreeFinish { addr, integrate } => {
                self.finish_worktree(&addr, integrate).await
            }
            Request::WorktreeRefresh { addr } => {
                let wt = self.find_worktree(&addr)?;
                let info = self.refresh_status(&wt.id).await.unwrap_or(wt);
                Ok(Reply::Worktree { info })
            }
            Request::WorktreeRun { addr, script, size } => {
                let wt = self.find_worktree(&addr)?;
                let s = self.run_script(&wt, script.as_deref(), size, BTreeMap::new()).await?;
                Ok(Reply::Session { info: self.info(&s) })
            }
            Request::WorktreeLock { addr, lock } => {
                let wt = self.find_worktree(&addr)?;
                let p = self
                    .project_by_id(&wt.project)
                    .ok_or_else(|| Error::missing("project", &wt.project))?;
                let (root, path, id) = (p.root, wt.path.clone(), wt.id.clone());
                blocking(move || {
                    let git = Git::new();
                    if lock {
                        git.lock(&root, &path, &format!("ptyro:{id}"))
                    } else {
                        git.unlock(&root, &path)
                    }
                })
                .await?
                .map_err(git_err)?;
                let info = self.update_worktree(&wt.id, |w| w.locked = lock).unwrap_or(wt);
                Ok(Reply::Worktree { info })
            }
            Request::WorktreeRenameBranch { addr, branch } => {
                self.rename_branch(&addr, branch).await
            }
            Request::Gc => self.gc().await,
            other => Err(Error::invalid(format!("unsupported request {other:?}"))),
        }
    }
}

/// Status equality ignoring the timestamp.
fn same_status(a: &ptyro_core::model::RepoStatus, b: &ptyro_core::model::RepoStatus) -> bool {
    let strip =
        |s: &ptyro_core::model::RepoStatus| ptyro_core::model::RepoStatus { at_ms: 0, ..s.clone() };
    strip(a) == strip(b)
}

/// Today's date as `YYYY-MM-DD` (UTC), for `{date}` in branch templates.
fn today() -> String {
    let days = (now_ms() / 86_400_000) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

/// The disk `path` takes (allocated blocks, so sparse files count what they use), without
/// following links, and whether it stopped at `budget` with more left to count.
pub(crate) fn disk_usage(path: &Path, budget: Duration) -> (u64, bool) {
    use std::os::unix::fs::MetadataExt;
    let deadline = std::time::Instant::now() + budget;
    let (mut total, mut stack) = (0u64, vec![path.to_path_buf()]);
    while let Some(dir) = stack.pop() {
        if std::time::Instant::now() > deadline {
            return (total, true);
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            total += m.blocks() * 512;
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    (total, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlinked_roots_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("worktrees");
        std::fs::create_dir_all(dir.path().join("real")).unwrap();
        std::fs::create_dir_all(&ours).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), ours.join("proj")).unwrap();
        assert!(refuse_symlinks(&ours, &ours.join("proj/name")).is_err());
        assert!(refuse_symlinks(&ours, &ours.join("other/name")).is_ok());
    }

    #[test]
    fn dates() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }

    #[test]
    fn layouts() {
        let p = ProjectInfo {
            machine: None,
            id: "x".into(),
            slug: "app".into(),
            root: "/src/app".into(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        };
        let data = Path::new("/data");
        assert_eq!(
            worktree_path(WorktreeLocation::Data, data, &p, "n").0,
            PathBuf::from("/data/worktrees/app/n")
        );
        assert_eq!(
            worktree_path(WorktreeLocation::Sibling, data, &p, "n").0,
            PathBuf::from("/src/app.worktrees/n")
        );
        assert_eq!(
            worktree_path(WorktreeLocation::InRepo, data, &p, "n").0,
            PathBuf::from("/src/app/.ptyro/worktrees/n")
        );
    }

    fn repo(dir: &Path) -> Git {
        let g = Git::new();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@e"],
            vec!["config", "user.name", "t"],
        ] {
            g.run(dir, &args).unwrap();
        }
        g
    }

    /// A pull request's own checkout can hold a tracked link `a -> ~/.ssh`; with `share =
    /// ["a/x"]` the share (a link, or a copy of the main checkout's `a/x`) must not land in it.
    #[test]
    fn sharing_never_writes_through_a_link_in_the_new_worktree() {
        let d = tempfile::tempdir().unwrap();
        let (root, dst, outside) =
            (d.path().join("root"), d.path().join("wt"), d.path().join("outside"));
        for p in [&root, &dst, &outside] {
            std::fs::create_dir_all(p).unwrap();
        }
        let git = repo(&root);
        std::fs::create_dir_all(root.join("a/x")).unwrap();
        std::fs::write(root.join("a/x/big"), "heavy").unwrap();
        std::os::unix::fs::symlink(&outside, dst.join("a")).unwrap();
        let mut cfg = ProjectConfig::default();
        cfg.worktree.share = vec!["a/x".into()];
        cfg.worktree.share_mode = ShareMode::Copy;
        let notes = bring_files(&git, &root, &dst, &cfg);
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing in the link's target"
        );
        assert!(notes.iter().any(|n| n.contains("leads out of it")), "{notes:?}");
        // The same share into a plain worktree works.
        let plain = d.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        cfg.worktree.share_mode = ShareMode::Symlink;
        bring_files(&git, &root, &plain, &cfg);
        assert!(plain.join("a/x").exists());
    }
}
