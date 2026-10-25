//! Workspaces, projects and profiles (SPEC §2.1, §10.1, §10.10): addressing, persistence and the
//! requests that manage them. Worktree operations are in `worktrees.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ptyro_agents::profile;
use ptyro_core::model::{
    DEFAULT_PROFILE, MAIN_WORKSPACE, ProjectInfo, ROOT_WORKTREE, WorkspaceInfo, WorktreeInfo,
    WorktreeState, new_id, slugify,
};
use ptyro_core::project::ProjectConfig;
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_git::Git;
use ptyro_protocol::{DomainEvent, Reply, Request};
use ptyro_store::TrustRow;

use crate::daemon::Daemon;

pub const SCRIPTS_SCOPE: &str = "scripts";

fn blocking_err(e: tokio::task::JoinError) -> Error {
    Error::internal(e.to_string())
}

pub(crate) fn git_err(e: ptyro_git::GitError) -> Error {
    match e {
        ptyro_git::GitError::Invalid(m) => Error::invalid(m),
        ptyro_git::GitError::Missing => {
            Error::new(ErrorCode::Unsupported, "git is not installed or not on PATH")
        }
        other => Error::new(ErrorCode::Conflict, other.to_string()),
    }
}

impl Daemon {
    // ── lookups ─────────────────────────────────────────────────────────────

    pub fn project_by_id(&self, id: &str) -> Option<ProjectInfo> {
        self.registry.lock().expect("registry").projects.iter().find(|p| p.id == id).cloned()
    }

    pub fn worktree_by_id(&self, id: &str) -> Option<WorktreeInfo> {
        self.registry.lock().expect("registry").worktrees.iter().find(|w| w.id == id).cloned()
    }

    pub fn root_worktree(&self, project: &str) -> Option<WorktreeInfo> {
        let reg = self.registry.lock().expect("registry");
        reg.worktrees.iter().find(|w| w.project == project && w.is_root).cloned()
    }

    /// A project by slug, id (or a ≥4-character id suffix), or a path inside one of its checkouts.
    pub fn find_project(&self, addr: &str) -> Result<ProjectInfo, Error> {
        let reg = self.registry.lock().expect("registry");
        let addr = addr.trim().strip_prefix("p:").unwrap_or(addr.trim());
        if let Some(p) = reg.projects.iter().find(|p| p.slug == addr || p.id == addr) {
            return Ok(p.clone());
        }
        if addr.starts_with(['/', '.', '~']) {
            let path = ptyro_core::config::expand_home(addr);
            let path = path.canonicalize().unwrap_or(path);
            let wt = innermost(reg.worktrees.iter(), &path);
            if let Some(p) = wt.and_then(|w| reg.projects.iter().find(|p| p.id == w.project)) {
                return Ok(p.clone());
            }
            return Err(Error::missing("project", addr));
        }
        let lower = addr.to_ascii_lowercase();
        let by_suffix: Vec<_> =
            reg.projects.iter().filter(|p| lower.len() >= 4 && p.id.ends_with(&lower)).collect();
        match by_suffix.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Error::missing("project", addr)),
            _ => {
                Err(Error::new(ErrorCode::Ambiguous, format!("{addr:?} matches several projects")))
            }
        }
    }

    /// A worktree by `project/name` (`myrepo/@root`), unique name, id suffix, or path.
    pub fn find_worktree(&self, addr: &str) -> Result<WorktreeInfo, Error> {
        let addr = addr.trim().strip_prefix("w:").unwrap_or(addr.trim());
        if addr.starts_with(['/', '.', '~']) {
            let path = ptyro_core::config::expand_home(addr);
            let path = path.canonicalize().unwrap_or(path);
            let reg = self.registry.lock().expect("registry");
            return innermost(reg.worktrees.iter(), &path)
                .cloned()
                .ok_or_else(|| Error::missing("worktree", addr));
        }
        if let Some((project, name)) = addr.split_once('/') {
            let p = self.find_project(project)?;
            let reg = self.registry.lock().expect("registry");
            return reg
                .worktrees
                .iter()
                .find(|w| w.project == p.id && w.name == name)
                .cloned()
                .ok_or_else(|| Error::missing("worktree", addr));
        }
        let reg = self.registry.lock().expect("registry");
        let named: Vec<_> = reg.worktrees.iter().filter(|w| w.name == addr).collect();
        match named.as_slice() {
            [one] => return Ok((*one).clone()),
            [] => {}
            many => {
                let q: Vec<String> = many.iter().map(|w| w.qualified()).collect();
                return Err(Error::new(
                    ErrorCode::Ambiguous,
                    format!("{addr:?} is ambiguous: {}", q.join(", ")),
                ));
            }
        }
        let lower = addr.to_ascii_lowercase();
        let by_suffix: Vec<_> =
            reg.worktrees.iter().filter(|w| lower.len() >= 4 && w.id.ends_with(&lower)).collect();
        match by_suffix.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Error::missing("worktree", addr)),
            _ => {
                Err(Error::new(ErrorCode::Ambiguous, format!("{addr:?} matches several worktrees")))
            }
        }
    }

    pub fn find_workspace(&self, name: &str) -> Result<WorkspaceInfo, Error> {
        let reg = self.registry.lock().expect("registry");
        reg.workspaces
            .iter()
            .find(|w| w.name == name)
            .cloned()
            .ok_or_else(|| Error::missing("workspace", name))
    }

    // ── persistence + events ────────────────────────────────────────────────

    pub(crate) fn put_worktree(&self, w: WorktreeInfo) {
        {
            let mut reg = self.registry.lock().expect("registry");
            match reg.worktrees.iter_mut().find(|o| o.id == w.id) {
                Some(o) => *o = w.clone(),
                None => reg.worktrees.push(w.clone()),
            }
        }
        if let Some(store) = &self.store {
            store.save_worktree(w.clone());
        }
        let _ = self.events.send(DomainEvent::WorktreeUpdated { info: w });
    }

    /// Update the in-memory copy only (live status, lock flag): not stored.
    pub(crate) fn update_worktree(
        &self,
        id: &str,
        f: impl FnOnce(&mut WorktreeInfo),
    ) -> Option<WorktreeInfo> {
        let w = {
            let mut reg = self.registry.lock().expect("registry");
            let w = reg.worktrees.iter_mut().find(|o| o.id == id)?;
            f(w);
            w.clone()
        };
        let _ = self.events.send(DomainEvent::WorktreeUpdated { info: w.clone() });
        Some(w)
    }

    pub(crate) fn drop_worktree(&self, mut w: WorktreeInfo) {
        self.registry.lock().expect("registry").worktrees.retain(|o| o.id != w.id);
        w.state = WorktreeState::Removed;
        w.removed_ms = Some(now_ms());
        w.ports = None;
        w.status = None;
        if let Some(store) = &self.store {
            store.save_worktree(w.clone());
        }
        let _ = self.events.send(DomainEvent::WorktreeRemoved { id: w.id });
    }

    pub(crate) fn touch_worktree(&self, id: &str) {
        let Some(mut w) = self.worktree_by_id(id) else { return };
        w.last_used_ms = Some(now_ms());
        let project = w.project.clone();
        self.put_worktree(w);
        let p = {
            let mut reg = self.registry.lock().expect("registry");
            reg.projects.iter_mut().find(|p| p.id == project).map(|p| {
                p.last_used_ms = Some(now_ms());
                p.clone()
            })
        };
        if let (Some(p), Some(store)) = (p, &self.store) {
            store.save_project(p);
        }
    }

    pub(crate) fn put_project(&self, p: ProjectInfo) {
        {
            let mut reg = self.registry.lock().expect("registry");
            match reg.projects.iter_mut().find(|o| o.id == p.id) {
                Some(o) => *o = p.clone(),
                None => reg.projects.push(p.clone()),
            }
        }
        if let Some(store) = &self.store {
            store.save_project(p.clone());
        }
        let _ = self.events.send(DomainEvent::ProjectUpdated { info: p });
    }

    pub(crate) fn put_workspace(&self, w: WorkspaceInfo) {
        {
            let mut reg = self.registry.lock().expect("registry");
            match reg.workspaces.iter_mut().find(|o| o.id == w.id) {
                Some(o) => *o = w.clone(),
                None => reg.workspaces.push(w.clone()),
            }
        }
        if let Some(store) = &self.store {
            store.save_workspace(w.clone());
        }
        let _ = self.events.send(DomainEvent::WorkspaceUpdated { info: w });
    }

    /// `main` always exists (SPEC §2.1).
    pub fn ensure_main_workspace(&self) {
        let exists = self
            .registry
            .lock()
            .expect("registry")
            .workspaces
            .iter()
            .any(|w| w.name == MAIN_WORKSPACE);
        if !exists {
            self.put_workspace(WorkspaceInfo {
                id: new_id(),
                name: MAIN_WORKSPACE.into(),
                profile: DEFAULT_PROFILE.into(),
                notify: "all".into(),
                projects: Vec::new(),
                created_ms: now_ms(),
            });
        }
    }

    // ── trust (SPEC §10.5) ─────────────────────────────────────────────────

    pub fn is_trusted(&self, project: &str, fingerprint: &str) -> bool {
        self.is_trusted_as(project, SCRIPTS_SCOPE, fingerprint)
    }

    /// Trust for one scope: `scripts`, or `recipe:<name>` for a repo's recipes (M7.4).
    pub fn is_trusted_as(&self, project: &str, scope: &str, fingerprint: &str) -> bool {
        let reg = self.registry.lock().expect("registry");
        reg.trust
            .iter()
            .any(|t| t.project == project && t.scope == scope && t.fingerprint == fingerprint)
    }

    pub(crate) fn trust(&self, project: &str, fingerprint: &str) {
        self.trust_as(project, SCRIPTS_SCOPE, fingerprint);
    }

    pub(crate) fn trust_as(&self, project: &str, scope: &str, fingerprint: &str) {
        let row = TrustRow {
            project: project.into(),
            scope: scope.into(),
            fingerprint: fingerprint.into(),
            approved_ms: now_ms(),
        };
        {
            let mut reg = self.registry.lock().expect("registry");
            reg.trust.retain(|t| !(t.project == project && t.scope == scope));
            reg.trust.push(row.clone());
        }
        if let Some(store) = &self.store {
            store.save_trust(row);
        }
        tracing::info!(project, scope, "trusted");
    }

    /// The scripts as they'd run in `wt`, with trust state.
    pub fn scripts_info(
        &self,
        p: &ProjectInfo,
        wt: &WorktreeInfo,
    ) -> Result<ptyro_core::model::ScriptsInfo, Error> {
        let cfg = ProjectConfig::load(&p.root, &wt.path).map_err(Error::invalid)?;
        let fp = cfg.trust_fingerprint();
        let mut names: Vec<String> = Vec::new();
        if let Some((n, _)) = cfg.scripts.run_script(None) {
            names.push(format!("run:{n}"));
        }
        for n in cfg.scripts.run.keys() {
            if !names.contains(&format!("run:{n}")) {
                names.push(format!("run:{n}"));
            }
        }
        for (n, s) in [
            ("setup", &cfg.scripts.setup),
            ("archive", &cfg.scripts.archive),
            ("after_turn", &cfg.scripts.after_turn),
        ] {
            if s.is_some() {
                names.push(n.into());
            }
        }
        Ok(ptyro_core::model::ScriptsInfo {
            project: p.slug.clone(),
            text: cfg.scripts.describe_from(&cfg.origin),
            trusted: cfg.scripts.is_empty() || self.is_trusted(&p.id, &fp),
            fingerprint: fp,
            names,
        })
    }

    // ── projects (SPEC §10.1) ──────────────────────────────────────────────

    pub async fn project_add(
        self: &Arc<Self>,
        path: PathBuf,
        workspace: Option<String>,
    ) -> Result<Reply, Error> {
        let workspace = match workspace {
            Some(w) => w,
            None => self.last_workspace().await,
        };
        let ws = self.find_workspace(&workspace)?;
        let facts = tokio::task::spawn_blocking(move || -> Result<_, Error> {
            let git = Git::new();
            let root = git.main_root(&path).map_err(git_err)?;
            let root = root.canonicalize().unwrap_or(root);
            let branch = git.default_branch(&root);
            let base = git.default_base(&root);
            let current = git.current_branch(&root);
            let remote = git.main_remote(&root).map(|r| r.url);
            Ok((root, branch, base, current, remote))
        })
        .await
        .map_err(blocking_err)??;
        let (root, default_branch, base, current, remote_url) = facts;
        // One add at a time: two adds of the same repository (discovery and the CLI) would
        // otherwise both find it new and register it twice.
        let _one = self.project_adding.lock().await;
        let mut notes = Vec::new();
        let existing = self
            .registry
            .lock()
            .expect("registry")
            .projects
            .iter()
            .find(|p| p.root == root)
            .cloned();
        let project = match existing {
            Some(p) => {
                notes.push(format!("{} is already registered", p.slug));
                p
            }
            None => {
                let cfg = ProjectConfig::load(&root, &root).unwrap_or_default();
                let base_slug = slugify(&cfg.project.name.clone().unwrap_or_else(|| {
                    root.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
                }));
                let slug = {
                    let reg = self.registry.lock().expect("registry");
                    let taken = |s: &str| reg.projects.iter().any(|p| p.slug == s);
                    if taken(&base_slug) {
                        (2..)
                            .map(|k| format!("{base_slug}-{k}"))
                            .find(|s| !taken(s))
                            .expect("infinite")
                    } else {
                        base_slug
                    }
                };
                let config = self.config();
                let remotes: Vec<String> = remote_url.iter().cloned().collect();
                let claim = profile::claimants(&config, &remotes, &root);
                let pin = match claim.as_slice() {
                    [one] => {
                        notes.push(format!(
                            "pinned to profile {one} (its match rules fit this repository)"
                        ));
                        Some(one.clone())
                    }
                    [] => None,
                    many => {
                        notes.push(format!(
                            "matches profiles {}; pin one with `ptyro project pin {slug} <profile>`",
                            many.join(", ")
                        ));
                        None
                    }
                };
                let p = ProjectInfo {
                    id: new_id(),
                    slug,
                    root: root.clone(),
                    default_branch,
                    remote_url,
                    pin,
                    created_ms: now_ms(),
                    last_used_ms: None,
                };
                self.put_project(p.clone());
                self.put_worktree(WorktreeInfo {
                    id: new_id(),
                    project: p.id.clone(),
                    project_slug: p.slug.clone(),
                    name: ROOT_WORKTREE.into(),
                    path: root,
                    branch: current,
                    base,
                    is_root: true,
                    profile: None,
                    state: WorktreeState::Ready,
                    ports: None,
                    created_ms: now_ms(),
                    removed_ms: None,
                    last_used_ms: None,
                    status: None,
                    locked: false,
                    pr: None,
                    title: None,
                    integrated: None,
                });
                if let Some(rw) = self.root_worktree(&p.id) {
                    self.watch_worktree_bg(&rw);
                    let _ = self.status_tx.send(rw.id);
                }
                p
            }
        };
        if !ws.projects.contains(&project.id) {
            let mut ws = ws;
            ws.projects.push(project.id.clone());
            self.put_workspace(ws);
        }
        Ok(Reply::Project { info: project, notes })
    }

    fn project_remove(&self, addr: &str, release: bool) -> Result<Reply, Error> {
        let p = self.find_project(addr)?;
        let worktrees: Vec<WorktreeInfo> = {
            let reg = self.registry.lock().expect("registry");
            reg.worktrees.iter().filter(|w| w.project == p.id).cloned().collect()
        };
        let in_it: Vec<Arc<crate::session::Session>> = worktrees
            .iter()
            .flat_map(|w| self.sessions_in(&w.id))
            .filter(|s| s.is_live())
            .collect();
        let live: Vec<String> = in_it
            .iter()
            .map(|s| s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name.clone())
            .collect();
        if release {
            // They keep running, outside any project.
            for s in &in_it {
                self.place_session(s, None);
            }
        } else if !live.is_empty() {
            return Err(Error::conflict(format!(
                "{} has running sessions ({}); stop them first",
                p.slug,
                live.join(", ")
            )));
        }
        {
            let mut reg = self.registry.lock().expect("registry");
            reg.projects.retain(|o| o.id != p.id);
            reg.worktrees.retain(|w| w.project != p.id);
            reg.trust.retain(|t| t.project != p.id);
            for w in &mut reg.workspaces {
                w.projects.retain(|id| *id != p.id);
            }
        }
        self.unwatch_project(&p.id);
        if let Some(store) = &self.store {
            store.delete_project(p.id.clone());
        }
        let _ = self.events.send(DomainEvent::ProjectRemoved { id: p.id.clone() });
        self.decline(&p.root);
        let kept: Vec<String> =
            worktrees.iter().filter(|w| !w.is_root).map(|w| w.path.display().to_string()).collect();
        let mut notes = vec![format!("forgot {}", p.slug)];
        if !kept.is_empty() {
            notes.push(format!("its worktrees are still on disk: {}", kept.join(", ")));
        }
        Ok(Reply::Removed { notes })
    }

    fn project_pin(&self, addr: &str, pin: Option<String>) -> Result<Reply, Error> {
        let mut p = self.find_project(addr)?;
        if let Some(name) = &pin
            && !profile::exists(&self.config(), name)
        {
            return Err(Error::invalid(format!("no profile named {name:?}")));
        }
        p.pin = pin;
        self.put_project(p.clone());
        Ok(Reply::Project { info: p, notes: Vec::new() })
    }

    // ── discovery (SPEC §10.1) ─────────────────────────────────────────────

    async fn discover(self: &Arc<Self>, refresh: bool) -> Result<Reply, Error> {
        const KEY: &str = "discover:cache";
        const FRESH_MS: u64 = 3_600_000;
        if !refresh
            && let Some(store) = &self.store
            && let Ok(Some(v)) = store.kv_get(KEY.into()).await
            && v["at"].as_u64().is_some_and(|at| now_ms().saturating_sub(at) < FRESH_MS)
            && let Ok(paths) = serde_json::from_value::<Vec<PathBuf>>(v["paths"].clone())
        {
            return Ok(Reply::Paths { paths });
        }
        let config = self.config();
        let roots: Vec<PathBuf> =
            config.discover.roots.iter().map(|r| ptyro_core::config::expand_home(r)).collect();
        let depth = config.discover.depth;
        let paths = tokio::task::spawn_blocking(move || scan_repos(&roots, depth))
            .await
            .map_err(blocking_err)?;
        if let Some(store) = &self.store {
            let _ = store
                .kv_set(KEY.into(), serde_json::json!({ "at": now_ms(), "paths": paths }))
                .await;
        }
        Ok(Reply::Paths { paths })
    }

    // ── profiles (SPEC §10.10) ─────────────────────────────────────────────

    fn profile_list(&self) -> Reply {
        let config = self.config();
        let root = self.paths.data.join("profiles");
        let profiles =
            profile::names(&config).iter().map(|n| profile::info(&config, n, &root)).collect();
        Reply::Profiles { profiles }
    }

    fn profile_env(&self, name: &str) -> Result<Reply, Error> {
        let config = self.config();
        let root = self.paths.data.join("profiles");
        // Secrets are never resolved for display.
        let plan = profile::plan(&config, name, &root, &mut |_, _| Ok(String::new()))
            .map_err(Error::invalid)?;
        let mut vars: Vec<(String, String)> = plan.shown;
        for r in &plan.remove {
            vars.push((r.clone(), "<removed>".into()));
        }
        for p in &plan.remove_prefixes {
            vars.push((format!("{p}*"), "<removed>".into()));
        }
        Ok(Reply::Env { vars })
    }

    async fn profile_rename(self: &Arc<Self>, old: &str, new: &str) -> Result<Reply, Error> {
        ptyro_core::names::validate(new).map_err(Error::invalid)?;
        if old == DEFAULT_PROFILE {
            return Err(Error::invalid("the default profile can't be renamed"));
        }
        if let Some(store) = &self.store {
            store
                .rename_profile(old.into(), new.into())
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
        }
        let (workspaces, projects, worktrees) = {
            let mut reg = self.registry.lock().expect("registry");
            for w in &mut reg.workspaces {
                if w.profile == old {
                    w.profile = new.into();
                }
            }
            for p in &mut reg.projects {
                if p.pin.as_deref() == Some(old) {
                    p.pin = Some(new.into());
                }
            }
            for w in &mut reg.worktrees {
                if w.profile.as_deref() == Some(old) {
                    w.profile = Some(new.into());
                }
            }
            (reg.workspaces.clone(), reg.projects.clone(), reg.worktrees.clone())
        };
        for s in self.sessions() {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if m.place.profile == old {
                m.place.profile = new.into();
            }
        }
        for w in workspaces {
            let _ = self.events.send(DomainEvent::WorkspaceUpdated { info: w });
        }
        for p in projects {
            let _ = self.events.send(DomainEvent::ProjectUpdated { info: p });
        }
        for w in worktrees {
            let _ = self.events.send(DomainEvent::WorktreeUpdated { info: w });
        }
        Ok(Reply::Done)
    }

    // ── workspaces ─────────────────────────────────────────────────────────

    fn workspace_new(
        &self,
        name: &str,
        profile: Option<String>,
        notify: Option<String>,
    ) -> Result<Reply, Error> {
        ptyro_core::names::validate(name).map_err(Error::invalid)?;
        if self.find_workspace(name).is_ok() {
            return Err(Error::conflict(format!("workspace {name} exists")));
        }
        let w = WorkspaceInfo {
            id: new_id(),
            name: name.into(),
            profile: self.check_profile(profile)?.unwrap_or_else(|| DEFAULT_PROFILE.into()),
            notify: check_notify(notify)?.unwrap_or_else(|| "all".into()),
            projects: Vec::new(),
            created_ms: now_ms(),
        };
        self.put_workspace(w.clone());
        Ok(Reply::Workspace { info: w })
    }

    fn check_profile(&self, p: Option<String>) -> Result<Option<String>, Error> {
        match p {
            Some(n) if !profile::exists(&self.config(), &n) => {
                Err(Error::invalid(format!("no profile named {n:?}")))
            }
            other => Ok(other),
        }
    }

    pub(crate) async fn handle_registry(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::ProjectAdd { path, workspace } => {
                let reply = self.project_add(path, workspace).await?;
                // Added by hand: it may register on its own again.
                if let Reply::Project { info, .. } = &reply {
                    self.undecline(&info.root);
                }
                Ok(reply)
            }
            Request::ProjectList => Ok(Reply::Projects {
                projects: self.registry.lock().expect("registry").projects.clone(),
            }),
            Request::ProjectGet { addr } => {
                Ok(Reply::Project { info: self.find_project(&addr)?, notes: Vec::new() })
            }
            Request::ProjectRemove { addr, release } => self.project_remove(&addr, release),
            Request::ProjectPin { addr, profile } => self.project_pin(&addr, profile),
            Request::ProjectScripts { addr, worktree } => {
                let p = self.find_project(&addr)?;
                let wt = match worktree {
                    Some(w) => self.find_worktree(&w)?,
                    None => self
                        .root_worktree(&p.id)
                        .ok_or_else(|| Error::missing("worktree", "@root"))?,
                };
                Ok(Reply::Scripts { scripts: self.scripts_info(&p, &wt)? })
            }
            Request::ProjectTrust { addr, fingerprint } => {
                let p = self.find_project(&addr)?;
                self.trust(&p.id, &fingerprint);
                Ok(Reply::Done)
            }
            Request::ProjectUntrust { addr } => {
                let p = self.find_project(&addr)?;
                self.registry.lock().expect("registry").trust.retain(|t| t.project != p.id);
                if let Some(store) = &self.store {
                    store.delete_trust(p.id.clone(), SCRIPTS_SCOPE.into());
                }
                Ok(Reply::Done)
            }
            Request::Discover { refresh } => self.discover(refresh).await,
            Request::WorkspaceList => Ok(Reply::Workspaces {
                workspaces: self.registry.lock().expect("registry").workspaces.clone(),
            }),
            Request::WorkspaceNew { name, profile, notify } => {
                self.workspace_new(&name, profile, notify)
            }
            Request::WorkspaceSet { name, profile, notify } => {
                let mut w = self.find_workspace(&name)?;
                if let Some(p) = self.check_profile(profile)? {
                    w.profile = p;
                }
                if let Some(n) = check_notify(notify)? {
                    w.notify = n;
                }
                self.put_workspace(w.clone());
                Ok(Reply::Workspace { info: w })
            }
            Request::WorkspaceRename { name, new_name } => {
                ptyro_core::names::validate(&new_name).map_err(Error::invalid)?;
                if name == MAIN_WORKSPACE {
                    return Err(Error::invalid("the main workspace can't be renamed"));
                }
                if self.find_workspace(&new_name).is_ok() {
                    return Err(Error::conflict(format!("workspace {new_name} exists")));
                }
                let mut w = self.find_workspace(&name)?;
                w.name = new_name.clone();
                self.put_workspace(w.clone());
                for s in self.sessions() {
                    let moved = {
                        let mut m =
                            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let moved = m.place.workspace.as_deref() == Some(name.as_str());
                        if moved {
                            m.place.workspace = Some(new_name.clone());
                        }
                        moved
                    };
                    // Saved (a dormant session keeps it across restarts) and announced.
                    if moved {
                        s.save(None);
                        let _ =
                            self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
                    }
                }
                Ok(Reply::Workspace { info: w })
            }
            Request::WorkspaceRemove { name } => {
                if name == MAIN_WORKSPACE {
                    return Err(Error::invalid("the main workspace always exists"));
                }
                let w = self.find_workspace(&name)?;
                self.registry.lock().expect("registry").workspaces.retain(|o| o.id != w.id);
                if let Some(store) = &self.store {
                    store.delete_workspace(w.id.clone());
                }
                // Its sessions keep running; they now live in main.
                for s in self.sessions() {
                    let moved = {
                        let mut m =
                            s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        let moved = m.place.workspace.as_deref() == Some(name.as_str());
                        if moved {
                            m.place.workspace = Some(MAIN_WORKSPACE.into());
                        }
                        moved
                    };
                    if moved {
                        s.save(None);
                        let _ =
                            self.events.send(DomainEvent::SessionUpdated { info: self.info(&s) });
                    }
                }
                let _ = self.events.send(DomainEvent::WorkspaceRemoved { id: w.id });
                Ok(Reply::Done)
            }
            Request::WorkspaceMember { name, project, remove } => {
                let mut w = self.find_workspace(&name)?;
                let p = self.find_project(&project)?;
                if remove {
                    w.projects.retain(|id| *id != p.id);
                } else if !w.projects.contains(&p.id) {
                    w.projects.push(p.id);
                }
                self.put_workspace(w.clone());
                Ok(Reply::Workspace { info: w })
            }
            Request::ProfileList => Ok(self.profile_list()),
            Request::ProfileEnv { name } => self.profile_env(&name),
            Request::ProfileRename { old, new } => self.profile_rename(&old, &new).await,
            Request::ProfileResolve { spec } => {
                let explicit = spec.profile.clone().or_else(|| {
                    spec.agent
                        .as_deref()
                        .and_then(|a| ptyro_agents::split_agent(a).1.map(str::to_owned))
                });
                let target = self.target(spec.worktree.as_deref(), &spec.cwd)?;
                let ws = self
                    .home_workspace(spec.workspace.as_deref(), target.as_ref().map(|(_, p)| p))?;
                let r = self.resolve_profile(
                    explicit.as_deref(),
                    target.as_ref(),
                    &ws,
                    spec.override_pin,
                    spec.confirmed,
                )?;
                Ok(Reply::Resolved { profile: r.profile, reason: r.reason, question: r.question })
            }
            other => self.handle_worktrees(other).await,
        }
    }
}

fn check_notify(n: Option<String>) -> Result<Option<String>, Error> {
    match n.as_deref() {
        None | Some("all" | "badge" | "mute") => Ok(n),
        Some(other) => {
            Err(Error::invalid(format!("notify must be all, badge or mute (not {other:?})")))
        }
    }
}

/// Git repositories under `roots`, at most `depth` levels down; never descends into a repo,
/// hidden directories, or dependency trees.
pub fn scan_repos(roots: &[PathBuf], depth: u8) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        if dir.join(".git").exists() {
            out.push(dir.to_path_buf());
            return;
        }
        if depth == 0 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                !n.starts_with('.')
                    && !matches!(n.as_ref(), "node_modules" | "target" | "vendor" | "venv")
            })
            .map(|e| e.path())
            .collect();
        dirs.sort();
        for d in dirs {
            walk(&d, depth - 1, out);
        }
    }
    let mut out = Vec::new();
    for r in roots {
        walk(r, depth, &mut out);
    }
    out.dedup();
    out
}

/// `path` resolved, remembered for half a minute: this runs under the registry lock for every
/// worktree on every sweep, and a checkout on a slow or stale mount must cost its syscall once,
/// not on every call.
fn canonical(path: &Path) -> std::path::PathBuf {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<HashMap<std::path::PathBuf, (std::path::PathBuf, Instant)>>> =
        Mutex::new(None);
    let now = Instant::now();
    {
        let mut guard = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some((c, at)) = map.get(path)
            && now.duration_since(*at) < Duration::from_secs(30)
        {
            return c.clone();
        }
    }
    let c = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut guard = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 1024 {
        map.clear();
    }
    map.insert(path.to_path_buf(), (c.clone(), now));
    c
}

/// The worktree whose checkout holds `path` (already resolved), innermost first. Both sides are
/// compared resolved: on macOS a checkout under /var or /tmp really lives in /private.
pub(crate) fn innermost<'a>(
    worktrees: impl Iterator<Item = &'a WorktreeInfo>,
    path: &Path,
) -> Option<&'a WorktreeInfo> {
    worktrees
        .map(|w| (w, canonical(&w.path)))
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.as_os_str().len())
        .map(|(w, _)| w)
}
