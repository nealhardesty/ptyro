//! Launching sessions (SPEC §5.1, §10.6, §10.10): which profile, the environment it gets, and the
//! one path every spawn takes (CLI, TUI, restart, scripts), so they can't drift apart.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ptyro_agents::profile::{self, Inputs, Plan};
use ptyro_core::config::{Guard, Secret, parse_duration};
use ptyro_core::model::{DEFAULT_PROFILE, ProfileReason, ProjectInfo, WorktreeInfo, WorktreeState};
use ptyro_core::session::SessionKind;
use ptyro_core::{Error, ErrorCode, SessionId, TermSize};
use ptyro_protocol::DomainEvent;

use crate::daemon::Daemon;
use crate::session::{Place, Session, Spawned};

/// An agent to start through its adapter (SPEC §8.1).
#[derive(Debug, Clone)]
pub struct AgentStart {
    /// The configured agent name.
    pub name: String,
    /// Resume this conversation instead of starting one.
    pub resume: Option<String>,
    /// Type this once the agent is ready.
    pub prompt: Option<String>,
    /// Run this instead of the configured command (`ptyro doctor`'s self-test).
    pub command: Option<Vec<String>>,
    /// Give it the `ptyro mcp` server (a lead agent, M7.6).
    pub mcp: bool,
    /// Drive it over its structured protocol (M10).
    pub structured: bool,
}

/// A structured launch (M10): the protocol, the driver's options, OpenCode's port and password.
type StructuredStart = (String, ptyro_drive::Opts, Option<(u16, String)>);

/// A port nobody listens on right now.
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

/// Everything needed to start (or restart) one session.
pub struct Launch {
    pub id: SessionId,
    pub argv: Vec<String>,
    pub kind: SessionKind,
    pub name: String,
    pub cwd: PathBuf,
    pub size: TermSize,
    pub client_env: BTreeMap<String, String>,
    pub place: Place,
    /// Restart: replace the session with the same id instead of adding one.
    pub replace: bool,
    /// For agents: `argv` holds extra arguments, the adapter builds the rest.
    pub agent: Option<AgentStart>,
}

/// The outcome of SPEC §10.10's resolution order plus guards.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub profile: String,
    pub reason: ProfileReason,
    /// Ask the user this before spawning (guard = confirm, or a pin override).
    pub question: Option<String>,
    pub warning: Option<String>,
}

const SECRET_TIMEOUT: Duration = Duration::from_secs(20);

/// Variables a run gives its sessions (the daemon sets them; SPEC §15.3).
const RUN_VARS: [&str; 5] =
    ["PTYRO_RUN_", "PTYRO_STEP_", "PTYRO_LEAD", "PTYRO_ATTEMPT", "PTYRO_CS"];

impl Daemon {
    /// The worktree a session runs in: the one addressed, else the registered worktree that
    /// contains `cwd` (so `ptyro new` inside a repo is placed in it).
    pub(crate) fn target(
        &self,
        addr: Option<&str>,
        cwd: &Path,
    ) -> Result<Option<(WorktreeInfo, ProjectInfo)>, Error> {
        if let Some(a) = addr {
            let wt = self.find_worktree(a)?;
            if wt.state != WorktreeState::Ready {
                return Err(Error::conflict(format!(
                    "worktree {} is {}",
                    wt.qualified(),
                    wt.state.label()
                )));
            }
            let p = self
                .project_by_id(&wt.project)
                .ok_or_else(|| Error::missing("project", &wt.project))?;
            return Ok(Some((wt, p)));
        }
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let reg = self.registry.lock().expect("registry");
        let best = reg
            .worktrees
            .iter()
            .filter(|w| w.state == WorktreeState::Ready && cwd.starts_with(&w.path))
            .max_by_key(|w| w.path.as_os_str().len());
        Ok(best.and_then(|w| {
            reg.projects.iter().find(|p| p.id == w.project).map(|p| (w.clone(), p.clone()))
        }))
    }

    /// SPEC §10.10: explicit → worktree → pin → workspace → default, then the pin-override and
    /// guard checks.
    pub(crate) fn resolve_profile(
        &self,
        explicit: Option<&str>,
        target: Option<&(WorktreeInfo, ProjectInfo)>,
        workspace: &str,
        override_pin: bool,
        confirmed: bool,
    ) -> Result<Resolution, Error> {
        let config = self.config();
        let ws_profile = {
            let reg = self.registry.lock().expect("registry");
            reg.workspaces.iter().find(|w| w.name == workspace).map(|w| w.profile.clone())
        };
        let inputs = Inputs {
            explicit,
            worktree: target.and_then(|(w, _)| w.profile.as_deref()),
            pin: target.and_then(|(_, p)| p.pin.as_deref()),
            workspace: ws_profile.as_deref().filter(|p| *p != DEFAULT_PROFILE),
        };
        let (name, reason) = profile::resolve(inputs);
        if !profile::exists(&config, &name) {
            return Err(Error::invalid(format!(
                "profile {name:?} is not defined (profiles: {}); add [profiles.{name}] to your config",
                profile::names(&config).join(", ")
            )));
        }
        let mut question = None;
        let mut warning = None;
        if let Some((other, why)) = profile::contradicts(inputs)
            && !override_pin
        {
            let what = target.map_or_else(|| "this repository".to_owned(), |(w, _)| w.qualified());
            let why = match why {
                ProfileReason::Worktree => "was created under",
                _ => "is pinned to",
            };
            question = Some(format!("{what} {why} profile {other}; run under {name} instead?"));
        }
        if let Some((wt, p)) = target
            && !matches!(reason, ProfileReason::Worktree | ProfileReason::Pinned)
        {
            let remotes: Vec<String> = p.remote_url.iter().cloned().collect();
            let claim = profile::claimants(&config, &remotes, &wt.path);
            if !claim.is_empty() && !claim.contains(&name) {
                let owner = &claim[0];
                let guard = config.profiles.get(owner).map(|d| d.guard).unwrap_or_default();
                let text = format!(
                    "{} matches profile {owner}'s rules, but would run under {name}",
                    wt.qualified()
                );
                match guard {
                    Guard::Refuse => {
                        return Err(Error::new(
                            ErrorCode::PermissionDenied,
                            format!(
                                "{text}; refusing (profiles.{owner}.guard = \"refuse\"). Pin the project or pick --profile {owner}"
                            ),
                        ));
                    }
                    Guard::Confirm if !confirmed && !override_pin => {
                        question.get_or_insert(format!("{text}. Continue?"));
                    }
                    Guard::Confirm => {}
                    Guard::Warn => warning = Some(text),
                }
            }
        }
        Ok(Resolution { profile: name, reason, question, warning })
    }

    /// The profile's environment plan with secrets resolved (blocking work off the runtime).
    pub(crate) async fn profile_plan(self: &Arc<Self>, name: &str) -> Result<Plan, Error> {
        let me = Arc::clone(self);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let config = me.config();
            let ttl = config
                .profiles
                .get(&name)
                .and_then(|d| d.secret_ttl.as_deref())
                .and_then(|t| parse_duration(t).ok())
                .unwrap_or(Duration::ZERO);
            let root = me.paths.data.join("profiles");
            profile::plan(&config, &name, &root, &mut |label, s| me.secret(label, s, ttl))
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))?
        .map_err(|e| Error::new(ErrorCode::Invalid, e))
    }

    /// Resolve one secret. Values live only in memory (for `ttl`), never in logs or errors.
    fn secret(&self, label: &str, s: &Secret, ttl: Duration) -> Result<String, String> {
        let argv = match s {
            Secret::Literal(v) => return Ok(v.clone()),
            Secret::Command { command } => command,
        };
        if !ttl.is_zero()
            && let Some((v, at)) = self.secrets.lock().expect("secrets").get(label)
            && at.elapsed() < ttl
        {
            return Ok(v.clone());
        }
        let (prog, args) = argv.split_first().ok_or_else(|| format!("{label}: empty command"))?;
        let mut child = std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("{label}: cannot run {prog}: {e}"))?;
        // Both pipes drained while it runs: a chatty command can't fill one and hang.
        let drain = |p: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = p {
                    let _ = p.read_to_end(&mut buf);
                }
                buf
            })
        };
        let stdout =
            drain(child.stdout.take().map(|x| Box::new(x) as Box<dyn std::io::Read + Send>));
        let stderr =
            drain(child.stderr.take().map(|x| Box::new(x) as Box<dyn std::io::Read + Send>));
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > SECRET_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{label}: {prog} took longer than {}s",
                        SECRET_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(format!("{label}: {e}")),
            }
        };
        let out = std::process::Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        };
        if !status.success() {
            // stderr explains failures (`op` not signed in); stdout might be a secret: never shown.
            let why: String = String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
            return Err(format!("{label}: {prog} failed ({status}) {why}").trim_end().to_owned());
        }
        let value = String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_owned();
        if !ttl.is_zero() {
            self.secrets
                .lock()
                .expect("secrets")
                .insert(label.to_owned(), (value.clone(), Instant::now()));
        }
        Ok(value)
    }

    /// SPEC §10.6: what scripts and sessions in a worktree know about it.
    pub(crate) fn worktree_env(&self, wt: &WorktreeInfo) -> Vec<(String, String)> {
        let Some(p) = self.project_by_id(&wt.project) else { return Vec::new() };
        let mut v = vec![
            ("PTYRO_PROJECT".to_owned(), p.slug.clone()),
            ("PTYRO_ROOT_PATH".to_owned(), p.root.to_string_lossy().into_owned()),
            ("PTYRO_WORKTREE_PATH".to_owned(), wt.path.to_string_lossy().into_owned()),
            ("PTYRO_WORKTREE_NAME".to_owned(), wt.name.clone()),
        ];
        if let Some(b) = &wt.branch {
            v.push(("PTYRO_BRANCH".into(), b.clone()));
        }
        if let Some(ports) = wt.ports {
            v.push(("PTYRO_PORT".into(), ports.base.to_string()));
            v.push(("PTYRO_PORT_COUNT".into(), ports.count.to_string()));
        }
        // Scripts imported from another tool expect its variables (M5.10).
        let tools: Vec<&str> = ptyro_core::project::ProjectConfig::load(&p.root, &wt.path)
            .map(|c| c.imported.iter().map(|i| i.tool).collect())
            .unwrap_or_default();
        let (root, path) =
            (p.root.to_string_lossy().into_owned(), wt.path.to_string_lossy().into_owned());
        if self.config().compat.conductor_env || tools.contains(&"conductor") {
            v.push(("CONDUCTOR_ROOT_PATH".into(), root.clone()));
            v.push(("CONDUCTOR_WORKSPACE_PATH".into(), path.clone()));
            v.push(("CONDUCTOR_WORKSPACE_NAME".into(), wt.name.clone()));
            v.push(("CONDUCTOR_IS_LOCAL".into(), "1".into()));
            if let Some(b) = &p.default_branch {
                v.push(("CONDUCTOR_DEFAULT_BRANCH".into(), b.clone()));
            }
            if let Some(ports) = wt.ports {
                v.push(("CONDUCTOR_PORT".into(), ports.base.to_string()));
            }
        }
        if tools.contains(&"superset") {
            v.push(("SUPERSET_ROOT_PATH".into(), root.clone()));
            v.push(("SUPERSET_WORKSPACE_PATH".into(), path.clone()));
            v.push(("SUPERSET_WORKSPACE_NAME".into(), wt.name.clone()));
        }
        if tools.contains(&"codex") {
            v.push(("CODEX_SOURCE_TREE_PATH".into(), root));
            v.push(("CODEX_WORKTREE_PATH".into(), path));
        }
        v
    }

    /// The complete child environment (SPEC §10.10 steps 1–4).
    fn session_env(
        &self,
        id: SessionId,
        plan: &Plan,
        client_env: &BTreeMap<String, String>,
        wt_env: &[(String, String)],
        agent_env: &[(String, String)],
    ) -> Vec<(OsString, OsString)> {
        let socket = self.paths.socket.as_os_str().to_owned();
        let id_str = OsString::from(id.full());
        let mut extra: Vec<(&str, &OsStr)> = Vec::new();
        for (k, v) in client_env {
            if let Some(allowed) = ptyro_pty::CLIENT_ENV.iter().find(|a| **a == k) {
                extra.push((allowed, OsStr::new(v)));
            } else if RUN_VARS.iter().any(|p| k.starts_with(p)) {
                // A run's step tells its agent where it is (`$PTYRO_STEP_OUT`, M7).
                extra.push((k, OsStr::new(v)));
            }
        }
        // An agent definition's own env still wins (SPEC §10.10 step 5).
        for (k, v) in plan.set.iter().chain(wt_env).chain(agent_env) {
            extra.push((k, OsStr::new(v)));
        }
        extra.push(("PTYRO_SESSION_ID", id_str.as_os_str()));
        extra.push(("PTYRO_SOCKET", socket.as_os_str()));
        ptyro_pty::child_env(plan.strip(self.base_env.iter().cloned()), extra)
    }

    /// The one way sessions start.
    pub(crate) async fn launch(self: &Arc<Self>, l: Launch) -> Result<Arc<Session>, Error> {
        // Held until the session is in the list: no second relaunch of the same session, no
        // second session with the same name, nothing that a handoff in progress would miss.
        let _claim = Claim::take(self, &l)?;
        let plan = self.profile_plan(&l.place.profile).await?;
        for dir in &plan.create_dirs {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
        }
        let wt = l.place.worktree.as_deref().and_then(|id| self.worktree_by_id(id));
        let wt_env = wt.as_ref().map(|w| self.worktree_env(w)).unwrap_or_default();
        let config = self.config();
        let mut argv = l.argv.clone();
        let mut agent_env: Vec<(String, String)> = Vec::new();
        let mut adapter_id = None;
        let mut conversation = None;
        let mut sidecar = None;
        let mut dangerous = false;
        let mut structured: Option<StructuredStart> = None;
        if let Some(a) = &l.agent {
            let def = ptyro_agents::def(&config, &a.name);
            let id = ptyro_agents::adapter_name(&config, &a.name);
            let adapter = ptyro_agents::adapter(&id);
            let mut base =
                a.command.clone().or_else(|| ptyro_agents::command(&config, &a.name)).ok_or_else(
                    || {
                        Error::new(
                            ErrorCode::NotFound,
                            format!(
                                "no agent named {:?} (known: {})",
                                a.name,
                                ptyro_agents::names(&config).join(", ")
                            ),
                        )
                    },
                )?;
            base.extend(l.argv.iter().cloned());
            dangerous = base
                .iter()
                .any(|x| ptyro_core::config::DANGEROUS_ARGS.iter().any(|d| x.starts_with(d)));
            if dangerous && !def.dangerous {
                return Err(Error::new(
                    ErrorCode::PermissionDenied,
                    format!(
                        "these arguments skip the agent's permission prompts; set `dangerous = true` in [agents.{}] to allow that",
                        a.name
                    ),
                ));
            }
            let home = ptyro_agents::home_var(&id)
                .and_then(|v| plan.set.iter().find(|(k, _)| k == v).map(|(_, p)| PathBuf::from(p)))
                .or_else(|| ptyro_agents::default_dir(&id));
            let ptyro = std::env::current_exe()
                .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned());
            let session = l.id.full();
            let project_root =
                wt.as_ref().and_then(|w| self.project_by_id(&w.project)).map(|p| p.root);
            let integrations = self.paths.data.join("integrations");
            // A lead (new, or one resumed or restarted) gets ptyro's MCP server (SPEC §15.4).
            let lead = a.mcp || self.run_of_session(&session).is_some_and(|(_, _, lead)| lead);
            let mcp = lead.then(|| {
                serde_json::json!({ "mcpServers": { "ptyro": {
                    "type": "stdio",
                    "command": ptyro,
                    "args": ["mcp"],
                    "env": { "PTYRO_SESSION_ID": session, "PTYRO_SOCKET": self.paths.socket.to_string_lossy() },
                } } })
                .to_string()
            });
            if a.structured {
                // M10: the agent's structured protocol instead of its terminal UI.
                let protocol = def
                    .protocol
                    .clone()
                    .or_else(|| ptyro_drive::protocol_for(&id).map(str::to_owned))
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::Unsupported,
                            format!(
                                "{} has no structured mode (an ACP agent: `protocol = \"acp\"` in [agents.{}])",
                                a.name, a.name
                            ),
                        )
                    })?;
                let session_id = (protocol == "stream-json" && a.resume.is_none())
                    .then(ptyro_agents::adapter::uuid_v4);
                let opts = ptyro_drive::Opts {
                    cwd: l.cwd.to_string_lossy().into_owned(),
                    resume: a.resume.clone(),
                    session_id: session_id.clone(),
                };
                let http = (protocol == "opencode")
                    .then(free_port)
                    .flatten()
                    .map(|p| (p, ptyro_agents::adapter::random_hex()));
                let mut base = base;
                if let (Some(m), "stream-json") = (&mcp, protocol.as_str()) {
                    base.extend(["--mcp-config".to_owned(), m.clone()]);
                }
                argv = ptyro_drive::launch_args(
                    &protocol,
                    &id,
                    &base,
                    &opts,
                    http.as_ref().map(|h| h.0),
                );
                agent_env.extend(def.env.iter().map(|(k, v)| (k.clone(), v.clone())));
                if let Some((_, pw)) = &http {
                    agent_env.push(("OPENCODE_SERVER_PASSWORD".into(), pw.clone()));
                }
                conversation = a.resume.clone().or(session_id);
                adapter_id = Some(id);
                structured = Some((protocol, opts, http));
            } else {
                let ctx = ptyro_agents::LaunchCtx {
                    agent: &a.name,
                    def: &def,
                    base_argv: base,
                    session: &session,
                    name: &l.name,
                    cwd: &l.cwd,
                    integrations: &integrations,
                    ptyro: &ptyro,
                    port: if id == "opencode" { free_port() } else { None },
                    agent_home: home,
                    project_root: project_root.as_deref(),
                    mcp: mcp.as_deref(),
                };
                let lp = match &a.resume {
                    Some(c) => adapter.resume(&ctx, c),
                    None => adapter.launch(&ctx),
                }
                .map_err(Error::invalid)?;
                for (path, content) in &lp.files {
                    if std::fs::read_to_string(path).ok().as_deref() != Some(content.as_str()) {
                        if let Some(dir) = path.parent() {
                            let _ =
                                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
                        }
                        std::fs::write(path, content)
                            .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
                    }
                }
                argv = lp.argv;
                agent_env = lp.env;
                agent_env.extend(def.env.iter().map(|(k, v)| (k.clone(), v.clone())));
                conversation = lp.conversation;
                sidecar = lp.sidecar;
                adapter_id = Some(id);
            }
        }
        let env = self.session_env(l.id, &plan, &l.client_env, &wt_env, &agent_env);
        let mut shown = plan.shown_map();
        shown.extend(wt_env.iter().cloned());
        // Agent variables can be secrets (OpenCode's server password): names only.
        shown.extend(agent_env.iter().map(|(k, _)| (k.clone(), "<set>".to_owned())));
        let mut launch = serde_json::json!({ "argv": argv, "cwd": l.cwd, "profile": plan.profile, "env": shown });
        if let Some((p, _, _)) = &structured {
            launch["structured"] = p.clone().into();
        }
        let scrollback = config.daemon.scrollback_lines;
        let stale_after_ms = parse_duration(&config.attention.stale_after)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(300_000);
        let spawned = Spawned {
            argv,
            kind: l.kind,
            name: l.name,
            cwd: l.cwd,
            size: l.size,
            env,
            client_env: l.client_env,
            scrollback,
            place: l.place,
            launch,
            adapter: adapter_id,
            conversation,
            dangerous,
            stale_after_ms,
            record: self.record_spec(l.id, l.agent.as_ref().map(|a| a.name.as_str())),
        };
        // A relaunch in place keeps the session's part in a run.
        let carried = self.session(l.id).map(|old| {
            let m = old.meta.lock().expect("meta");
            (m.run.clone(), m.verify.clone(), m.guard.clone())
        });
        let spawned_session = match structured {
            Some((protocol, opts, http)) => {
                let driver = ptyro_drive::new(&protocol, opts)
                    .ok_or_else(|| Error::invalid(format!("unknown protocol {protocol:?}")))?;
                let st = crate::structured::Structured::new(&protocol, http);
                Session::spawn_structured(
                    l.id,
                    spawned,
                    st,
                    crate::structured::Start { driver },
                    self.hub(),
                )
                .map(|(s, r)| (s, Some(r)))
            }
            None => Session::spawn(l.id, spawned, self.hub()).map(|s| (s, None)),
        };
        let (session, driver) = spawned_session.map_err(|e| {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::NotFound
            } else {
                ErrorCode::Invalid
            };
            Error::new(code, format!("cannot start session: {e}"))
        })?;
        if let Some((run, verify, guard)) = carried.filter(|_| l.replace) {
            let mut m = session.meta.lock().expect("meta");
            m.run = run;
            m.verify = verify;
            m.guard = guard;
        }
        if l.replace {
            if !self.replace_session(&session) {
                // Removed while it relaunched: don't leave a process nobody can see.
                self.stop_session(&session).await;
                return Err(Error::conflict("the session was removed while it restarted"));
            }
        } else {
            self.push_session(Arc::clone(&session));
            tracing::info!(session = %l.id, "spawned");
            let _ = self.events.send(DomainEvent::SessionCreated { info: self.info(&session) });
        }
        // Only now: the driver's first status must find the session registered.
        if let Some(r) = driver {
            crate::structured::run(Arc::clone(&session), r);
        }
        if let Some(w) = wt {
            self.touch_worktree(&w.id);
            self.sync_locks().await;
        }
        if let Some(sc) = sidecar {
            self.start_sidecar(&session, sc);
        }
        if session.adapter().as_deref() == Some("codex") {
            self.start_codex_usage(&session);
        }
        if let Some(prompt) =
            l.agent.as_ref().and_then(|a| a.prompt.clone()).filter(|p| !p.trim().is_empty())
        {
            self.deliver_prompt(Arc::clone(&session), prompt);
        }
        Ok(session)
    }
}

/// A launch in flight: its session id (a relaunch) or its name (a new session) is taken until
/// it is in the session list.
struct Claim<'a> {
    daemon: &'a Daemon,
    id: Option<ptyro_core::SessionId>,
    name: Option<String>,
}

impl<'a> Claim<'a> {
    fn take(daemon: &'a Daemon, l: &Launch) -> Result<Self, Error> {
        if daemon.upgrading.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::conflict("the daemon is upgrading; try again in a moment"));
        }
        let mut c = daemon.launching.lock().expect("launching");
        if l.replace {
            if !c.0.insert(l.id) {
                return Err(Error::conflict("that session is already being restarted"));
            }
            return Ok(Self { daemon, id: Some(l.id), name: None });
        }
        if !c.1.insert(l.name.clone()) {
            return Err(Error::conflict(format!("a session named {:?} is being started", l.name)));
        }
        Ok(Self { daemon, id: None, name: Some(l.name.clone()) })
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut c = self.daemon.launching.lock().expect("launching");
        if let Some(id) = self.id {
            c.0.remove(&id);
        }
        if let Some(n) = &self.name {
            c.1.remove(n);
        }
    }
}
