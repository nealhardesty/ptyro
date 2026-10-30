//! Many machines (MM1.B, SPEC §16.4b): this daemon links to other machines' daemons and mirrors
//! their workspaces beside its own, so every client of it (the TUI, the CLI, `ptyro serve` and
//! so the web app) sees one daemon.
//!
//! - **A link** per machine is a client of the other daemon (`ClientKind::Machine`, over
//!   `ssh host ptyro proxy` or `ptyro machine-bridge` to its `ptyro serve`). It must run the
//!   same version of ptyro; it is retried with a backoff and comes back by itself.
//! - **The mirror** keeps what the link heard, with the machine in every name
//!   (`laptop2:work`, `laptop2:fix-flaky-test`) and in `machine`. It is kept while the machine is
//!   offline: its workspaces stay listed as they were.
//! - **Routing:** a request that names something of a mirror (`laptop2:…`, or its id) is sent
//!   over the link with the machine taken off its names, and its reply comes back with it put
//!   on. Lists are this daemon's plus every mirror's. Screens and typing go over a connection of
//!   the client's own (`conn.rs`).
//! - **No echoes:** a link is answered with this machine's own things only, and its requests are
//!   never routed on.
//! - **Chats** (CHAT1.9, SPEC §22.11): a machine's chats are mirrored as `ChatInfo` (never a
//!   transcript), listed beside this machine's, routed by `chat` like any address, and followed
//!   live over the client's own upstream (`conn.rs`); the engine runs on the owning machine.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use ptyro_client::{Client, ConnectOptions};
use ptyro_core::chats::ChatInfo;
use ptyro_core::model::{
    MachineInfo, MachineState, MachineTransport, ProfileInfo, ProjectInfo, WorkspaceInfo,
    WorktreeInfo, split_machine,
};
use ptyro_core::session::SessionInfo;
use ptyro_core::status::{InboxEntry, Usage};
use ptyro_core::sync::MutexExt;
use ptyro_core::{ClientId, Error, ErrorCode, SessionId, now_ms};
use ptyro_protocol::{ClientKind, ClientMsg, DomainEvent, Reply, Request, ServerMsg};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::daemon::Daemon;

const KV_KEY: &str = "machines";
/// How long adding a machine waits for it to answer.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest wait between tries of a link that is down.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Inbox entries kept per mirror.
const INBOX_KEEP: usize = 300;

/// A machine as kept in the store (its token, if any, is a file of its own).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MachineRecord {
    pub name: String,
    pub transport: MachineTransport,
    pub added_ms: u64,
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// What a link last heard from its machine, with the machine in every name.
#[derive(Debug, Default, Clone)]
pub(crate) struct Mirror {
    pub workspaces: Vec<WorkspaceInfo>,
    pub projects: Vec<ProjectInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub sessions: Vec<SessionInfo>,
    pub profiles: Vec<ProfileInfo>,
    pub usage: Vec<Usage>,
    /// Newest first.
    pub inbox: Vec<InboxEntry>,
    /// Its chats that aren't archived (SPEC §22.11): listed here while it is offline too.
    pub chats: Vec<ChatInfo>,
    /// Ids of its archived chats seen so far (not mirrored, but still routed there).
    pub archived: HashSet<String>,
}

pub(crate) struct Link {
    pub record: MachineRecord,
    pub info: MachineInfo,
    pub mirror: Mirror,
    pub client: Option<Arc<Client>>,
    pub task: Option<JoinHandle<()>>,
    pub wake: Arc<Notify>,
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

// ── names: putting the machine on, taking it off ─────────────────────────────

/// `laptop2` + `work` → `laptop2:work`.
pub(crate) fn qualify(machine: &str, name: &str) -> String {
    format!("{machine}:{name}")
}

/// Another machine's client id as this daemon's clients see it: the client of ours that holds
/// that connection (its upstream, `conn.rs`), else one that never matches a client here.
pub(crate) type MapClient<'a> = &'a dyn Fn(u64) -> u64;

/// A client id that is no client of this daemon (someone at the other machine).
pub(crate) fn foreign(id: u64) -> u64 {
    id | (1 << 63)
}

fn has(o: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().all(|k| o.contains_key(*k))
}

fn put_on(o: &mut serde_json::Map<String, Value>, key: &str, machine: &str) {
    if let Some(Value::String(s)) = o.get_mut(key)
        && !s.is_empty()
        && !s.starts_with(&format!("{machine}:"))
    {
        *s = qualify(machine, s);
    }
}

/// Put `machine` on everything of its in a reply or an event (as JSON): sessions, workspaces,
/// projects, worktrees, profiles, inbox entries and usage get `machine` and their names the
/// prefix; client ids (a session's controller) are mapped.
pub(crate) fn qualify_value(v: &mut Value, machine: &str, map: MapClient<'_>) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(|x| qualify_value(x, machine, map)),
        Value::Object(o) => {
            // Something already of a machine (it can't be: links aren't sent mirrors) is left.
            let mine = o.get("machine").is_none_or(Value::is_null);
            if mine {
                let mut tag = true;
                if has(o, &["handle", "kind", "argv", "state", "cwd"]) {
                    for k in ["name", "workspace", "profile"] {
                        put_on(o, k, machine);
                    }
                    if let Some(c) = o.get("controller").and_then(Value::as_u64) {
                        o.insert("controller".into(), Value::from(map(c)));
                    }
                } else if has(o, &["projects", "notify", "profile", "name"]) {
                    put_on(o, "name", machine);
                    put_on(o, "profile", machine);
                } else if has(o, &["slug", "root"]) {
                    put_on(o, "slug", machine);
                    put_on(o, "pin", machine);
                } else if has(o, &["project_slug", "is_root"]) {
                    put_on(o, "project_slug", machine);
                    put_on(o, "profile", machine);
                } else if has(o, &["label", "defined", "isolate"]) {
                    put_on(o, "name", machine);
                } else if has(o, &["session_name", "at_ms", "title", "body"]) {
                    put_on(o, "workspace", machine);
                    put_on(o, "profile", machine);
                    put_on(o, "session_name", machine);
                } else if has(o, &["windows", "agent", "profile"]) {
                    put_on(o, "profile", machine);
                } else if has(o, &["title_src", "tools", "nodes", "handle"]) {
                    // A chat (SPEC §22.11): its ids stay as they are (unique everywhere).
                    put_on(o, "workspace", machine);
                    put_on(o, "profile", machine);
                } else {
                    tag = false;
                }
                if tag {
                    o.insert("machine".into(), Value::from(machine));
                }
            }
            // A controller change names a client.
            if o.get("t").and_then(Value::as_str) == Some("controller_changed")
                && let Some(c) = o.get("controller").and_then(Value::as_u64)
            {
                o.insert("controller".into(), Value::from(map(c)));
            }
            for (_, x) in o.iter_mut() {
                qualify_value(x, machine, map);
            }
        }
        _ => {}
    }
}

fn qualified<T: Serialize + for<'de> Deserialize<'de>>(
    t: T,
    machine: &str,
    map: MapClient<'_>,
) -> T {
    let Ok(mut v) = serde_json::to_value(&t) else { return t };
    qualify_value(&mut v, machine, map);
    serde_json::from_value(v).unwrap_or(t)
}

/// The fields of a request that address something (and of its `spec`).
const ADDRESS_KEYS: &[&str] =
    &["addr", "name", "workspace", "project", "worktree", "session", "target", "chat"];

/// Every address in a request, as (where, the text).
fn addresses(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Value::Object(o) = v {
        for (k, x) in o {
            match x {
                Value::String(s) if ADDRESS_KEYS.contains(&k.as_str()) => out.push(s.clone()),
                Value::Object(_) if k == "spec" => out.extend(addresses(x)),
                _ => {}
            }
        }
    }
    out
}

/// Take `machine:` off a request's addresses, for its own machine.
pub(crate) fn strip_value(v: &mut Value, machine: &str) {
    let prefix = format!("{machine}:");
    if let Value::Object(o) = v {
        for (k, x) in o.iter_mut() {
            match x {
                Value::String(s) if ADDRESS_KEYS.contains(&k.as_str()) => {
                    if let Some(rest) = s.strip_prefix(&prefix) {
                        *s = rest.to_owned();
                    }
                }
                Value::Object(_) if k == "spec" => strip_value(x, machine),
                _ => {}
            }
        }
    }
}

/// Requests about this machine itself: never routed, whatever they name.
fn here_only(req: &Request) -> bool {
    use Request::*;
    matches!(
        req,
        List | Status
            | Shutdown { .. }
            | Upgrade { .. }
            | DevicePair { .. }
            | DeviceClaim { .. }
            | DeviceList
            | DeviceRevoke { .. }
            | PushKey
            | PushSubscribe { .. }
            | PushUnsubscribe { .. }
            | PushTest
            | ScratchGet
            | ScratchPut { .. }
            | ScratchAdd { .. }
            | ScratchTodo { .. }
            | ScratchMark { .. }
            | ScratchHistory
            | ScratchRestore { .. }
            | ScratchPane { .. }
            | SaveView { .. }
            | LoadView { .. }
            | ReloadConfig
            | History { .. }
            | ProjectList
            | Discover { .. }
            | WorkspaceList
            | Gc
            | ProfileList
            | ProfileEnv { .. }
            | ProfileRename { .. }
            | Hook { .. }
            | StatusLine { .. }
            | Report { .. }
            | Inbox { .. }
            | Mute { .. }
            | Usage
            | Agents
            | Editors
            | Web { .. }
            | NotifyTest
            | Doctor
            | SelfTest { .. }
            | ReviewQueue
            | RunList { .. }
            | RunGet { .. }
            | RunEvents { .. }
            | RunCancel { .. }
            | RunRetry { .. }
            | RunFork { .. }
            | RecipeList { .. }
            | RecipeShow { .. }
            | RecipeRun { .. }
            | Fanout { .. }
            | GateList { .. }
            | GateResolve { .. }
            | Compare { .. }
            | Pick { .. }
            | Discard { .. }
            | Iterate { .. }
            | MsgPost { .. }
            | MsgCheck { .. }
            | MsgAck { .. }
            | MsgList { .. }
            | LeaseClaim { .. }
            | LeaseRelease { .. }
            | LeaseList
            | Radar { .. }
            | Lead { .. }
            | Cs { .. }
            | MachineList
            | MachineAdd { .. }
            | MachineRemove { .. }
            | MachineHide { .. }
            | MachineRetry { .. }
    )
}

/// What to say when the two run different versions, and where to act: the older one is updated.
pub(crate) fn version_fix(machine: &str, theirs: &str, ours: &str) -> (String, String) {
    let error = format!("{machine} runs ptyro {theirs} and this machine {ours}: they must match");
    let older_there = newer(ours, theirs);
    let fix = if older_there {
        format!("update ptyro on {machine} to {ours}, then run `ptyro daemon upgrade` there")
    } else {
        format!("update ptyro here to {theirs}, then run `ptyro daemon upgrade`")
    };
    (error, fix)
}

/// `a` is a newer version than `b` (numerically, part by part).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
    };
    parts(a) > parts(b)
}

fn kind_name(t: &MachineTransport) -> &'static str {
    match t {
        MachineTransport::Ssh { .. } => "ssh",
        MachineTransport::Token { .. } => "token",
    }
}

impl Daemon {
    // ── records ──────────────────────────────────────────────────────────────

    fn token_path(&self, name: &str) -> std::path::PathBuf {
        self.paths.data.join("machines").join(format!("{name}.token"))
    }

    fn save_token(&self, name: &str, token: &str) -> Result<(), Error> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.token_path(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::internal(e.to_string()))?;
            let _ =
                std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
        f.write_all(token.as_bytes()).map_err(|e| Error::internal(e.to_string()))
    }

    fn token_of(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.token_path(name)).ok().map(|t| t.trim().to_owned())
    }

    async fn save_machines(&self) {
        let records: Vec<MachineRecord> =
            self.links.locked().values().map(|l| l.record.clone()).collect();
        if let Some(st) = &self.store {
            let _ =
                st.kv_set(KV_KEY.into(), serde_json::to_value(records).unwrap_or_default()).await;
        }
    }

    /// At start: every machine's link.
    pub(crate) async fn start_machines(self: &Arc<Self>) {
        let Some(store) = &self.store else { return };
        let Ok(Some(v)) = store.kv_get(KV_KEY.into()).await else { return };
        let Ok(records) = serde_json::from_value::<Vec<MachineRecord>>(v) else { return };
        for r in records {
            self.open_link(r);
        }
    }

    fn open_link(self: &Arc<Self>, record: MachineRecord) {
        let name = record.name.clone();
        let info = MachineInfo {
            name: name.clone(),
            transport: record.transport.clone(),
            state: MachineState::Connecting,
            since_ms: Some(now_ms()),
            version: None,
            calls_itself: None,
            workspaces: Vec::new(),
            hidden: record.hidden.clone(),
            error: None,
            fix: None,
            added_ms: record.added_ms,
        };
        let wake = Arc::new(Notify::new());
        let me = Arc::clone(self);
        let task_name = name.clone();
        let task = tokio::spawn(async move { me.run_link(task_name).await });
        self.links.locked().insert(
            name,
            Link { record, info, mirror: Mirror::default(), client: None, task: Some(task), wake },
        );
    }

    pub(crate) fn machine_infos(&self) -> Vec<MachineInfo> {
        self.links.locked().values().map(|l| l.info.clone()).collect()
    }

    fn set_link_state(
        &self,
        name: &str,
        state: MachineState,
        error: Option<String>,
        fix: Option<String>,
    ) {
        let info = {
            let mut links = self.links.locked();
            let Some(l) = links.get_mut(name) else { return };
            if l.info.state != state {
                l.info.since_ms = Some(now_ms());
            }
            l.info.state = state;
            l.info.error = error;
            l.info.fix = fix;
            l.info.clone()
        };
        let _ = self.mirrored.send(DomainEvent::MachineUpdated { info });
    }

    // ── connecting ───────────────────────────────────────────────────────────

    /// A fresh connection to `transport`'s daemon (a link, or a client's own upstream).
    pub(crate) async fn dial(
        &self,
        name: &str,
        transport: &MachineTransport,
    ) -> Result<Client, Error> {
        let mut opts = ConnectOptions::new(self.paths.clone(), ClientKind::Machine);
        opts.autospawn = false;
        opts.compress = true;
        opts.remote = None;
        match transport {
            MachineTransport::Ssh { target } => opts.remote = Some(target.clone()),
            MachineTransport::Token { url, fingerprint } => {
                let exe = std::env::var("PTYRO_BIN")
                    .ok()
                    .filter(|b| !b.is_empty())
                    .or_else(|| {
                        std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| "ptyro".into());
                let mut argv = vec![exe, "machine-bridge".into(), "--url".into(), url.clone()];
                if let Some(f) = fingerprint {
                    argv.extend(["--fingerprint".into(), f.clone()]);
                }
                opts.command = Some((argv, url.clone()));
                opts.token = self.token_of(name);
                if opts.token.is_none() {
                    return Err(Error::new(
                        ErrorCode::PermissionDenied,
                        format!(
                            "no token for {name}: add it again with a fresh code from `ptyro machine pair` on it"
                        ),
                    ));
                }
            }
        }
        match tokio::time::timeout(CHECK_TIMEOUT, Client::connect(&opts)).await {
            Ok(Ok(c)) => Ok(c),
            Ok(Err(e)) => Err(Error::new(ErrorCode::DaemonUnavailable, e.to_string())),
            Err(_) => Err(Error::new(
                ErrorCode::DaemonUnavailable,
                format!("{} didn't answer within {}s", transport.label(), CHECK_TIMEOUT.as_secs()),
            )),
        }
    }

    /// One machine's link, for as long as it is kept: connect, check the version, mirror,
    /// follow; when it drops, keep the mirror (offline) and try again.
    async fn run_link(self: Arc<Self>, name: String) {
        let ours = env!("CARGO_PKG_VERSION");
        let mut backoff = Duration::from_secs(1);
        loop {
            let Some((transport, wake)) = self
                .links
                .locked()
                .get(&name)
                .map(|l| (l.record.transport.clone(), Arc::clone(&l.wake)))
            else {
                return;
            };
            let online = self
                .links
                .locked()
                .get(&name)
                .is_some_and(|l| l.info.state == MachineState::Online);
            if !online {
                self.set_link_state(&name, MachineState::Connecting, None, None);
            }
            match self.dial(&name, &transport).await {
                Err(e) => {
                    let fix = match kind_name(&transport) {
                        "ssh" => format!(
                            "check that `ssh {} ptyro --version` works here without a password (keys or ssh-agent)",
                            ptyro_client::remote::parse(&transport.label()).0
                        ),
                        _ => format!(
                            "check that {name} is awake and its web app's server is on (`ptyro serve`)"
                        ),
                    };
                    self.set_link_state(&name, MachineState::Offline, Some(e.message), Some(fix));
                }
                Ok(mut client) => {
                    {
                        let mut links = self.links.locked();
                        if let Some(l) = links.get_mut(&name) {
                            l.info.version = Some(client.daemon_version.clone());
                            l.info.calls_itself = client.machine.clone();
                        }
                    }
                    if client.daemon_version != ours {
                        let (error, fix) = version_fix(&name, &client.daemon_version, ours);
                        self.set_link_state(&name, MachineState::Mismatch, Some(error), Some(fix));
                        drop(client);
                        // Until someone updates one of them: not worth trying often.
                        let _ = tokio::time::timeout(MAX_BACKOFF * 5, wake.notified()).await;
                        continue;
                    }
                    let pushes = client.pushes();
                    let client = Arc::new(client);
                    if client.send(ClientMsg::WatchEvents).is_ok()
                        && self.refresh_mirror(&name, &client).await.is_ok()
                    {
                        if let Some(l) = self.links.locked().get_mut(&name) {
                            l.client = Some(Arc::clone(&client));
                        }
                        self.set_link_state(&name, MachineState::Online, None, None);
                        backoff = Duration::from_secs(1);
                        self.follow(&name, &client, pushes).await;
                        if let Some(l) = self.links.locked().get_mut(&name) {
                            l.client = None;
                        }
                        self.set_link_state(
                            &name,
                            MachineState::Offline,
                            Some(format!("{name} went away")),
                            Some("it reconnects by itself when it is back".into()),
                        );
                    }
                }
            }
            let _ = tokio::time::timeout(backoff, wake.notified()).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Read everything the machine has, and tell this daemon's clients what changed.
    async fn refresh_mirror(&self, name: &str, client: &Client) -> Result<(), Error> {
        let map = self.client_map(name);
        let q = |r: Reply| qualified(r, name, &map);
        let mut m = Mirror::default();
        if let Reply::Workspaces { workspaces } = q(client.request(Request::WorkspaceList).await?) {
            m.workspaces = workspaces;
        }
        if let Reply::Projects { projects } = q(client.request(Request::ProjectList).await?) {
            m.projects = projects;
        }
        if let Reply::Worktrees { worktrees } =
            q(client.request(Request::WorktreeList { project: None }).await?)
        {
            m.worktrees = worktrees;
        }
        if let Reply::Sessions { sessions } = q(client.request(Request::List).await?) {
            m.sessions = sessions;
        }
        if let Ok(Reply::Profiles { profiles }) = client.request(Request::ProfileList).await.map(q)
        {
            m.profiles = profiles;
        }
        if let Ok(Reply::Usage { usage }) = client.request(Request::Usage).await.map(q) {
            m.usage = usage;
        }
        if let Ok(Reply::Inbox { entries }) = client
            .request(Request::Inbox {
                workspace: None,
                unread_only: false,
                limit: Some(INBOX_KEEP as u32),
            })
            .await
            .map(q)
        {
            m.inbox = entries;
        }
        // Chats off there, or an older answer: none, not a broken link.
        let chats = Request::ChatList {
            workspace: None,
            all: true,
            pinned: false,
            archived: false,
            limit: None,
        };
        if let Ok(Reply::Chats { chats }) = client.request(chats).await.map(q) {
            m.chats = chats;
        }
        let old = {
            let mut links = self.links.locked();
            let Some(l) = links.get_mut(name) else { return Ok(()) };
            l.info.workspaces = m
                .workspaces
                .iter()
                .filter_map(|w| split_machine(&w.name).map(|(_, n)| n.to_owned()))
                .collect();
            std::mem::replace(&mut l.mirror, m.clone())
        };
        // What went, then what is: a client that knew the old mirror ends up with the new one.
        let ids = |v: &[SessionInfo]| v.iter().map(|s| s.id).collect::<HashSet<_>>();
        let now = ids(&m.sessions);
        for s in old.sessions.iter().filter(|s| !now.contains(&s.id)) {
            let _ = self.mirrored.send(DomainEvent::SessionRemoved { id: s.id });
        }
        let gone = |a: &[String], b: &[String]| {
            a.iter().filter(|x| !b.contains(x)).cloned().collect::<Vec<_>>()
        };
        let wt_ids = |v: &[WorktreeInfo]| v.iter().map(|w| w.id.clone()).collect::<Vec<_>>();
        for id in gone(&wt_ids(&old.worktrees), &wt_ids(&m.worktrees)) {
            let _ = self.mirrored.send(DomainEvent::WorktreeRemoved { id });
        }
        let p_ids = |v: &[ProjectInfo]| v.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
        for id in gone(&p_ids(&old.projects), &p_ids(&m.projects)) {
            let _ = self.mirrored.send(DomainEvent::ProjectRemoved { id });
        }
        let w_ids = |v: &[WorkspaceInfo]| v.iter().map(|w| w.id.clone()).collect::<Vec<_>>();
        for id in gone(&w_ids(&old.workspaces), &w_ids(&m.workspaces)) {
            let _ = self.mirrored.send(DomainEvent::WorkspaceRemoved { id });
        }
        let c_ids = |v: &[ChatInfo]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
        for id in gone(&c_ids(&old.chats), &c_ids(&m.chats)) {
            let _ = self.mirrored.send(DomainEvent::ChatRemoved { id, machine: Some(name.into()) });
        }
        let hidden = self.hidden_of(name);
        let view = filter_hidden(m, &hidden);
        for info in view.workspaces {
            let _ = self.mirrored.send(DomainEvent::WorkspaceUpdated { info });
        }
        for info in view.projects {
            let _ = self.mirrored.send(DomainEvent::ProjectUpdated { info });
        }
        for info in view.worktrees {
            let _ = self.mirrored.send(DomainEvent::WorktreeUpdated { info });
        }
        for info in view.sessions {
            let _ = self.mirrored.send(DomainEvent::SessionUpdated { info });
        }
        for info in view.chats {
            let _ = self.mirrored.send(DomainEvent::ChatUpdated { info });
        }
        Ok(())
    }

    /// Follow the machine's events until the link drops.
    async fn follow(
        self: &Arc<Self>,
        name: &str,
        client: &Client,
        mut pushes: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    ) {
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        loop {
            tokio::select! {
                m = pushes.recv() => match m {
                    Some(ServerMsg::Event { event }) => {
                        if matches!(event, DomainEvent::EventsLost { .. }) {
                            if self.refresh_mirror(name, client).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        let map = self.client_map(name);
                        let event = qualified(event, name, &map);
                        self.mirror_event(name, event).await;
                    }
                    Some(ServerMsg::Goodbye { .. }) | None => return,
                    Some(_) => {}
                },
                _ = ping.tick() => {
                    // A machine that went to sleep never closes its end: a request tells.
                    let alive = tokio::time::timeout(Duration::from_secs(15), client.request(Request::Status)).await;
                    if !matches!(alive, Ok(Ok(_))) {
                        return;
                    }
                }
            }
        }
    }

    /// Apply one of the machine's (qualified) events to its mirror, and pass it on.
    async fn mirror_event(self: &Arc<Self>, name: &str, event: DomainEvent) {
        let hidden = self.hidden_of(name);
        let mut relay = None;
        let pass = {
            let mut links = self.links.locked();
            let Some(l) = links.get_mut(name) else { return };
            let m = &mut l.mirror;
            match &event {
                DomainEvent::SessionCreated { info }
                | DomainEvent::SessionUpdated { info }
                | DomainEvent::SessionExited { info }
                | DomainEvent::SessionRestarted { info } => {
                    upsert(&mut m.sessions, info.clone(), |s| s.id);
                    true
                }
                DomainEvent::SessionRemoved { id } => {
                    m.sessions.retain(|s| s.id != *id);
                    true
                }
                DomainEvent::WorkspaceUpdated { info } => {
                    upsert(&mut m.workspaces, info.clone(), |w| w.id.clone());
                    l.info.workspaces = m
                        .workspaces
                        .iter()
                        .filter_map(|w| split_machine(&w.name).map(|(_, n)| n.to_owned()))
                        .collect();
                    !hidden.iter().any(|h| qualify(name, h) == info.name)
                }
                DomainEvent::WorkspaceRemoved { id } => {
                    m.workspaces.retain(|w| w.id != *id);
                    true
                }
                DomainEvent::ProjectUpdated { info }
                | DomainEvent::ProjectAutoAdded { info, .. } => {
                    upsert(&mut m.projects, info.clone(), |p| p.id.clone());
                    true
                }
                DomainEvent::ProjectRemoved { id } => {
                    m.projects.retain(|p| p.id != *id);
                    true
                }
                DomainEvent::WorktreeUpdated { info } => {
                    upsert(&mut m.worktrees, info.clone(), |w| w.id.clone());
                    true
                }
                DomainEvent::WorktreeRemoved { id } => {
                    m.worktrees.retain(|w| w.id != *id);
                    true
                }
                DomainEvent::UsageUpdated { usage } => {
                    let key = (usage.profile.clone(), usage.agent.clone());
                    m.usage.retain(|u| (u.profile.clone(), u.agent.clone()) != key);
                    m.usage.push(usage.clone());
                    true
                }
                DomainEvent::InboxAdded { entry } => {
                    m.inbox.insert(0, entry.clone());
                    m.inbox.truncate(INBOX_KEEP);
                    relay = Some(entry.clone());
                    true
                }
                DomainEvent::ChatUpdated { info } => {
                    if info.archived {
                        m.chats.retain(|c| c.id != info.id);
                        m.archived.insert(info.id.clone());
                    } else {
                        m.archived.remove(&info.id);
                        upsert(&mut m.chats, info.clone(), |c| c.id.clone());
                    }
                    let in_hidden = split_machine(&info.workspace)
                        .is_some_and(|(_, w)| hidden.iter().any(|h| h == w));
                    !in_hidden
                }
                DomainEvent::ChatRemoved { id, .. } => {
                    m.chats.retain(|c| c.id != *id);
                    true
                }
                DomainEvent::WorktreeProgress { .. }
                | DomainEvent::ControllerChanged { .. }
                | DomainEvent::Bell { .. }
                | DomainEvent::SessionResumed { .. } => true,
                // The machine's own business: its config, runs, scratchpad, its terminal
                // notifications (ours come from its inbox entries, below), its own links.
                _ => false,
            }
        };
        let event = match event {
            // Another machine's repository registering itself isn't news to undo here.
            DomainEvent::ProjectAutoAdded { info, .. } => DomainEvent::ProjectUpdated { info },
            // An archived chat leaves the lists here, as there.
            DomainEvent::ChatUpdated { info } if info.archived => {
                DomainEvent::ChatRemoved { id: info.id, machine: Some(name.to_owned()) }
            }
            DomainEvent::ChatRemoved { id, .. } => {
                DomainEvent::ChatRemoved { id, machine: Some(name.to_owned()) }
            }
            e => e,
        };
        if pass {
            let _ = self.mirrored.send(event);
        }
        if let Some(entry) = relay {
            self.relay_alert(&entry).await;
        }
    }

    /// Its alerts reach you through this daemon (SPEC §16.4b): an inbox entry the machine made is
    /// delivered by this daemon's channels, under this machine's notification settings.
    async fn relay_alert(self: &Arc<Self>, entry: &InboxEntry) {
        let config = self.config();
        let n = &config.notifications;
        let kind = entry.kind;
        let breakthrough = config.attention.breakthrough.iter().any(|k| k == kind.label());
        if !n.on.iter().any(|k| k == kind.label()) {
            return;
        }
        if let Some(q) =
            n.quiet_hours.as_deref().and_then(|q| ptyro_core::config::parse_quiet_hours(q).ok())
            && let Some((h, m)) = ptyro_pty::local_time_hm()
            && ptyro_core::config::in_quiet_hours(q, u16::from(h) * 60 + u16::from(m))
            && !breakthrough
        {
            return;
        }
        let note = crate::notifier::Note {
            session: entry.session.clone(),
            session_name: entry.session_name.clone(),
            kind,
            title: entry.title.clone(),
            body: entry.body.clone(),
            profile: entry.profile.clone().unwrap_or_default(),
            workspace: entry.workspace.clone(),
            project: None,
            approve_url: None,
            deny_url: None,
        };
        self.deliver(&note, false).await;
    }

    /// A machine's client ids, as ours: the clients whose upstream holds that connection.
    fn client_map(&self, name: &str) -> impl Fn(u64) -> u64 + use<> {
        let ups: HashMap<u64, u64> = self
            .upstream_ids
            .locked()
            .iter()
            .filter(|((m, _), _)| m == name)
            .map(|((_, theirs), ours)| (*theirs, ours.0))
            .collect();
        move |id| ups.get(&id).copied().unwrap_or_else(|| foreign(id))
    }

    fn hidden_of(&self, name: &str) -> Vec<String> {
        self.links.locked().get(name).map(|l| l.record.hidden.clone()).unwrap_or_default()
    }

    // ── routing ──────────────────────────────────────────────────────────────

    /// The machine a request is for, when it is one of the mirrored ones: by `laptop2:` on one
    /// of its addresses, or by the id of something of a mirror. Two machines at once: refused.
    pub(crate) fn machine_for(&self, req: &Request) -> Result<Option<String>, Error> {
        if here_only(req) {
            return Ok(None);
        }
        let links = self.links.locked();
        if links.is_empty() {
            return Ok(None);
        }
        let v = serde_json::to_value(req).unwrap_or_default();
        let mut found: Option<String> = None;
        let addrs = addresses(&v);
        // A worktree or project of this machine's, next to a workspace of another's: neither
        // machine can serve it. Say so, instead of the other machine's "no worktree matches".
        if let Some(m) = addrs.iter().find_map(|a| split_machine(a).map(|(m, _)| m.to_owned()))
            && links.contains_key(&m)
        {
            let reg = self.registry.locked();
            if let Some(a) = addrs.iter().find(|a| {
                reg.worktrees.iter().any(|w| w.id == **a)
                    || reg.projects.iter().any(|p| p.id == **a)
            }) {
                return Err(Error::invalid(format!(
                    "{a} is on this machine, but the workspace is on {m}: use a workspace here"
                )));
            }
        }
        for a in addrs {
            let by_prefix =
                split_machine(&a).map(|(m, _)| m.to_owned()).filter(|m| links.contains_key(m));
            let by_id = || {
                links.iter().find_map(|(m, l)| {
                    let mi = &l.mirror;
                    let session = a
                        .parse::<SessionId>()
                        .ok()
                        .is_some_and(|id| mi.sessions.iter().any(|s| s.id == id));
                    let hit = session
                        || mi.chats.iter().any(|c| c.id == a)
                        || mi.archived.contains(&a)
                        || mi.workspaces.iter().any(|w| w.id == a)
                        || mi.projects.iter().any(|p| p.id == a)
                        || mi.worktrees.iter().any(|w| w.id == a);
                    hit.then(|| m.clone())
                })
            };
            if let Some(m) = by_prefix.or_else(by_id) {
                match &found {
                    Some(f) if *f != m => {
                        return Err(Error::invalid(format!(
                            "this names things on two machines ({f} and {m}): one at a time"
                        )));
                    }
                    _ => found = Some(m),
                }
            }
        }
        Ok(found)
    }

    /// A name this machine doesn't have, that exactly one mirror does (`fix-flaky-test` for
    /// `laptop2:fix-flaky-test`). Several: refused, naming them.
    pub(crate) fn machine_guess(&self, req: &Request) -> Result<Option<String>, Error> {
        if here_only(req) {
            return Ok(None);
        }
        let v = serde_json::to_value(req).unwrap_or_default();
        let Some(a) = addresses(&v).into_iter().next().filter(|a| split_machine(a).is_none())
        else {
            return Ok(None);
        };
        let links = self.links.locked();
        let hits: Vec<String> = links
            .iter()
            .filter(|(m, l)| {
                let q = qualify(m, &a);
                let mi = &l.mirror;
                let chat = a.strip_prefix("c:").unwrap_or(&a);
                mi.chats.iter().any(|c| c.handle == chat || c.id == a)
                    || mi.sessions.iter().any(|s| s.name == q || s.handle == a)
                    || mi.worktrees.iter().any(|w| w.qualified() == q)
                    || mi.workspaces.iter().any(|w| w.name == q)
                    || mi.projects.iter().any(|p| p.slug == q)
            })
            .map(|(m, _)| m.clone())
            .collect();
        match hits.as_slice() {
            [] => Ok(None),
            [m] => Ok(Some(m.clone())),
            many => Err(Error::invalid(format!(
                "{a} is on several machines: say which ({})",
                many.iter().map(|m| qualify(m, &a)).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    /// Send a request to its machine, its names taken off; its reply with them put back.
    pub(crate) async fn forward(&self, name: &str, req: Request) -> Result<Reply, Error> {
        let client = {
            let links = self.links.locked();
            let l = links.get(name).ok_or_else(|| Error::missing("machine", name))?;
            match (&l.client, l.info.state) {
                (Some(c), MachineState::Online) => Arc::clone(c),
                (_, state) => {
                    let since =
                        l.info.since_ms.map(|t| format!(" since {}", clock(t))).unwrap_or_default();
                    let mut msg = format!("{name} is {}{since}", state.label());
                    if let Some(f) = &l.info.fix {
                        msg.push_str(&format!(": {f}"));
                    }
                    return Err(Error::new(ErrorCode::DaemonUnavailable, msg));
                }
            }
        };
        let mut v = serde_json::to_value(&req).map_err(|e| Error::internal(e.to_string()))?;
        strip_value(&mut v, name);
        let req: Request = serde_json::from_value(v).map_err(|e| Error::internal(e.to_string()))?;
        let reply = client.request(req).await?;
        let map = self.client_map(name);
        Ok(qualified(reply, name, &map))
    }

    /// A list this daemon answers, with every mirror's (shown) part added.
    pub(crate) fn merge_mirrors(&self, req: &Request, reply: Reply) -> Reply {
        let views: Vec<Mirror> = {
            let links = self.links.locked();
            if links.is_empty() {
                return reply;
            }
            links.values().map(|l| filter_hidden(l.mirror.clone(), &l.record.hidden)).collect()
        };
        match reply {
            Reply::Sessions { mut sessions } if matches!(req, Request::List) => {
                sessions.extend(views.iter().flat_map(|m| m.sessions.clone()));
                Reply::Sessions { sessions }
            }
            Reply::Workspaces { mut workspaces } => {
                workspaces.extend(views.iter().flat_map(|m| m.workspaces.clone()));
                Reply::Workspaces { workspaces }
            }
            Reply::Projects { mut projects } => {
                projects.extend(views.iter().flat_map(|m| m.projects.clone()));
                Reply::Projects { projects }
            }
            Reply::Worktrees { mut worktrees }
                if matches!(req, Request::WorktreeList { project: None }) =>
            {
                worktrees.extend(views.iter().flat_map(|m| m.worktrees.clone()));
                Reply::Worktrees { worktrees }
            }
            Reply::Profiles { mut profiles } => {
                profiles.extend(views.iter().flat_map(|m| m.profiles.clone()));
                Reply::Profiles { profiles }
            }
            // Chats (SPEC §22.11): every machine's, interleaved as one list is.
            Reply::Chats { mut chats } => {
                if let Request::ChatList {
                    workspace: None, pinned, archived: false, limit, ..
                } = req
                {
                    chats.extend(
                        views.iter().flat_map(|m| m.chats.clone()).filter(|c| !pinned || c.pinned),
                    );
                    chats.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_ms.cmp(&a.last_ms)));
                    if let Some(n) = limit.filter(|n| *n > 0) {
                        chats.truncate(n as usize);
                    }
                }
                Reply::Chats { chats }
            }
            Reply::Usage { mut usage } => {
                usage.extend(views.iter().flat_map(|m| m.usage.clone()));
                Reply::Usage { usage }
            }
            Reply::Inbox { mut entries } => {
                if let Request::Inbox { workspace, unread_only, limit } = req {
                    entries.extend(views.iter().flat_map(|m| m.inbox.clone()).filter(|e| {
                        (!unread_only || e.read_ms.is_none())
                            && workspace.as_ref().is_none_or(|w| e.workspace.as_ref() == Some(w))
                    }));
                    entries.sort_by_key(|e| std::cmp::Reverse(e.at_ms));
                    if let Some(n) = limit {
                        entries.truncate(*n as usize);
                    }
                }
                Reply::Inbox { entries }
            }
            other => other,
        }
    }

    /// The machine a mirrored session lives on.
    pub(crate) fn machine_of_session(
        &self,
        id: SessionId,
    ) -> Option<(String, MachineTransport, bool)> {
        self.links.locked().iter().find_map(|(m, l)| {
            l.mirror.sessions.iter().any(|s| s.id == id).then(|| {
                (m.clone(), l.record.transport.clone(), l.info.state == MachineState::Online)
            })
        })
    }

    /// The machine a chat lives on, when it is a mirrored one: by `laptop2:` on its address, or
    /// by its id → (machine, the chat's address there, its transport, online).
    pub(crate) fn machine_of_chat(
        &self,
        chat: &str,
    ) -> Option<(String, String, MachineTransport, bool)> {
        let links = self.links.locked();
        let found = split_machine(chat)
            .filter(|(m, _)| links.contains_key(*m))
            .map(|(m, rest)| (m.to_owned(), rest.to_owned()))
            .or_else(|| {
                links.iter().find_map(|(m, l)| {
                    let mine = l.mirror.chats.iter().any(|c| c.id == chat)
                        || l.mirror.archived.contains(chat);
                    mine.then(|| (m.clone(), chat.to_owned()))
                })
            })?;
        let l = links.get(&found.0)?;
        Some((found.0, found.1, l.record.transport.clone(), l.info.state == MachineState::Online))
    }

    /// ` since 14:02`, when the machine's link went down (empty if it never did).
    pub(crate) fn offline_since(&self, name: &str) -> String {
        self.links
            .locked()
            .get(name)
            .and_then(|l| l.info.since_ms)
            .map(|t| format!(" since {}", clock(t)))
            .unwrap_or_default()
    }

    /// `ChatList` of archived chats, with every online machine's (they are not mirrored).
    pub(crate) async fn archived_chats_too(&self, req: &Request, reply: Reply) -> Reply {
        let (Request::ChatList { workspace: None, archived: true, .. }, Reply::Chats { mut chats }) =
            (req, reply.clone())
        else {
            return reply;
        };
        let online: Vec<String> = self
            .links
            .locked()
            .iter()
            .filter(|(_, l)| l.info.state == MachineState::Online)
            .map(|(m, _)| m.clone())
            .collect();
        for m in online {
            if let Ok(Reply::Chats { chats: theirs }) = self.forward(&m, req.clone()).await {
                let hidden = self.hidden_of(&m);
                // Remembered, so acting on one (unarchive, open, delete) is routed there.
                if let Some(l) = self.links.locked().get_mut(&m) {
                    l.mirror.archived.extend(theirs.iter().map(|c| c.id.clone()));
                }
                chats.extend(theirs.into_iter().filter(|c| {
                    !split_machine(&c.workspace).is_some_and(|(_, w)| hidden.iter().any(|h| h == w))
                }));
            }
        }
        // One list, ordered and cut as a single machine's is.
        chats.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_ms.cmp(&a.last_ms)));
        if let Request::ChatList { limit: Some(n), .. } = req
            && *n > 0
        {
            chats.truncate(*n as usize);
        }
        Reply::Chats { chats }
    }

    // ── requests ─────────────────────────────────────────────────────────────

    pub(crate) async fn handle_machines(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::MachineList => Ok(Reply::Machines { machines: self.machine_infos() }),
            Request::MachineAdd { name, transport, token, force } => {
                self.add_machine(name, transport, token, force).await
            }
            Request::MachineRemove { name } => {
                let gone = self.links.locked().remove(&name);
                let Some(link) = gone else { return Err(Error::missing("machine", &name)) };
                let _ = std::fs::remove_file(self.token_path(&name));
                self.save_machines().await;
                // Its things leave every list.
                for s in &link.mirror.sessions {
                    let _ = self.mirrored.send(DomainEvent::SessionRemoved { id: s.id });
                }
                for w in &link.mirror.worktrees {
                    let _ = self.mirrored.send(DomainEvent::WorktreeRemoved { id: w.id.clone() });
                }
                for p in &link.mirror.projects {
                    let _ = self.mirrored.send(DomainEvent::ProjectRemoved { id: p.id.clone() });
                }
                for w in &link.mirror.workspaces {
                    let _ = self.mirrored.send(DomainEvent::WorkspaceRemoved { id: w.id.clone() });
                }
                for c in &link.mirror.chats {
                    let _ = self.mirrored.send(DomainEvent::ChatRemoved {
                        id: c.id.clone(),
                        machine: Some(name.clone()),
                    });
                }
                let _ = self.mirrored.send(DomainEvent::MachineRemoved { name });
                Ok(Reply::Done)
            }
            Request::MachineHide { name, workspace, hidden } => {
                let ws = split_machine(&workspace)
                    .filter(|(m, _)| *m == name)
                    .map_or(workspace.clone(), |(_, w)| w.to_owned());
                let (info, mirror) = {
                    let mut links = self.links.locked();
                    let l = links.get_mut(&name).ok_or_else(|| Error::missing("machine", &name))?;
                    if !l.info.workspaces.contains(&ws) && !l.record.hidden.contains(&ws) {
                        return Err(Error::missing("workspace", qualify(&name, &ws)));
                    }
                    l.record.hidden.retain(|h| *h != ws);
                    if hidden {
                        l.record.hidden.push(ws.clone());
                    }
                    l.info.hidden = l.record.hidden.clone();
                    (l.info.clone(), l.mirror.clone())
                };
                self.save_machines().await;
                // Its chats go and come back with it (SPEC §22.11).
                for c in mirror.chats.iter().filter(|c| c.workspace == qualify(&name, &ws)) {
                    let _ = self.mirrored.send(if hidden {
                        DomainEvent::ChatRemoved { id: c.id.clone(), machine: Some(name.clone()) }
                    } else {
                        DomainEvent::ChatUpdated { info: c.clone() }
                    });
                }
                if let Some(w) = mirror.workspaces.iter().find(|w| w.name == qualify(&name, &ws)) {
                    let _ = self.mirrored.send(if hidden {
                        DomainEvent::WorkspaceRemoved { id: w.id.clone() }
                    } else {
                        DomainEvent::WorkspaceUpdated { info: w.clone() }
                    });
                }
                let _ = self.mirrored.send(DomainEvent::MachineUpdated { info: info.clone() });
                Ok(Reply::Machine { info })
            }
            Request::MachineRetry { name } => {
                let info = {
                    let links = self.links.locked();
                    let l = links.get(&name).ok_or_else(|| Error::missing("machine", &name))?;
                    l.wake.notify_one();
                    l.info.clone()
                };
                Ok(Reply::Machine { info })
            }
            _ => Err(Error::internal("not a machine request")),
        }
    }

    async fn add_machine(
        self: &Arc<Self>,
        name: Option<String>,
        transport: MachineTransport,
        token: Option<String>,
        force: bool,
    ) -> Result<Reply, Error> {
        if let MachineTransport::Ssh { target } = &transport
            && (target.trim().is_empty() || ptyro_client::remote::parse(target).0.starts_with('-'))
        {
            return Err(Error::invalid(format!("bad ssh target {target:?}")));
        }
        if matches!(transport, MachineTransport::Token { .. }) && token.is_none() {
            return Err(Error::invalid(
                "a token is needed: claim the other machine's `ptyro machine pair` code (`ptyro machine add CODE` does it)",
            ));
        }
        // The token goes where `dial` reads it, under a temporary name until the machine says
        // what it is called.
        let probe = format!(".adding-{}", ptyro_agents::adapter::random_hex());
        if let Some(t) = &token {
            self.save_token(&probe, t)?;
        }
        // The check's connection closes here; the link makes its own.
        let checked = self
            .dial(&probe, &transport)
            .await
            .map(|c| (c.machine.clone(), c.daemon_version.clone()));
        let (calls_itself, version) = match &checked {
            Ok((m, v)) => (m.clone(), Some(v.clone())),
            Err(_) => (None, None),
        };
        if let Err(e) = &checked
            && !force
        {
            let _ = std::fs::remove_file(self.token_path(&probe));
            return Err(Error::new(
                e.code,
                format!("{} didn't answer, so it wasn't added: {}", transport.label(), e.message),
            ));
        }
        let name =
            match name.or_else(|| calls_itself.as_deref().and_then(ptyro_core::names::as_name)) {
                Some(n) => n,
                None => {
                    let _ = std::fs::remove_file(self.token_path(&probe));
                    return Err(Error::invalid(
                        "give it a name (--name laptop2): it didn't say its own",
                    ));
                }
            };
        let cleanup = || {
            let _ = std::fs::remove_file(self.token_path(&probe));
        };
        if let Err(e) = ptyro_core::names::validate(&name) {
            cleanup();
            return Err(Error::invalid(e));
        }
        // `c:7f3k` is a chat's address (SPEC §22.3): a machine called `c` would take every one.
        if name == "c" {
            cleanup();
            return Err(Error::invalid(
                "`c` can't name a machine: `c:…` is how chats are addressed. Pick another with --name",
            ));
        }
        if ptyro_core::names::as_name(&self.machine()).as_deref() == Some(name.as_str()) {
            cleanup();
            return Err(Error::invalid(format!(
                "{name} is this machine's own name: a machine can't mirror itself"
            )));
        }
        if self.links.locked().contains_key(&name) {
            cleanup();
            return Err(Error::conflict(format!(
                "{name} is already added (`ptyro machine rm {name}` first to add it again)"
            )));
        }
        if token.is_some() {
            std::fs::rename(self.token_path(&probe), self.token_path(&name))
                .map_err(|e| Error::internal(format!("token: {e}")))?;
        }
        let record =
            MachineRecord { name: name.clone(), transport, added_ms: now_ms(), hidden: Vec::new() };
        self.open_link(record);
        if let Some(l) = self.links.locked().get_mut(&name) {
            l.info.version = version.clone();
            l.info.calls_itself = calls_itself;
        }
        // The check above already met it: say now that it can't link, with the fix, instead of
        // a "connecting" that the link then turns into this a moment later.
        if let Some(theirs) = version.filter(|v| v != env!("CARGO_PKG_VERSION")) {
            let (error, fix) = version_fix(&name, &theirs, env!("CARGO_PKG_VERSION"));
            self.set_link_state(&name, MachineState::Mismatch, Some(error), Some(fix));
        }
        self.save_machines().await;
        let info = self
            .links
            .locked()
            .get(&name)
            .map(|l| l.info.clone())
            .ok_or_else(|| Error::internal("the machine went away"))?;
        let _ = self.mirrored.send(DomainEvent::MachineUpdated { info: info.clone() });
        Ok(Reply::Machine { info })
    }
}

/// A mirror as shown: without the workspaces hidden here, and what lives only in them.
pub(crate) fn filter_hidden(mut m: Mirror, hidden: &[String]) -> Mirror {
    if hidden.is_empty() {
        return m;
    }
    let is_hidden =
        |name: &str| split_machine(name).is_some_and(|(_, w)| hidden.iter().any(|h| h == w));
    let (gone, kept): (Vec<WorkspaceInfo>, Vec<WorkspaceInfo>) =
        std::mem::take(&mut m.workspaces).into_iter().partition(|w| is_hidden(&w.name));
    let shown: HashSet<&String> = kept.iter().flat_map(|w| &w.projects).collect();
    let only_hidden: HashSet<String> = gone
        .iter()
        .flat_map(|w| w.projects.iter())
        .filter(|p| !shown.contains(p))
        .cloned()
        .collect();
    m.projects.retain(|p| !only_hidden.contains(&p.id));
    m.worktrees.retain(|w| !only_hidden.contains(&w.project));
    m.sessions.retain(|s| {
        !s.project.as_ref().is_some_and(|p| only_hidden.contains(p))
            && !(s.project.is_none() && s.workspace.as_deref().is_some_and(is_hidden))
    });
    m.inbox.retain(|e| !e.workspace.as_deref().is_some_and(is_hidden));
    m.chats.retain(|c| !is_hidden(&c.workspace));
    m.workspaces = kept;
    m
}

fn upsert<T, K: PartialEq>(v: &mut Vec<T>, x: T, key: impl Fn(&T) -> K) {
    let k = key(&x);
    match v.iter_mut().find(|y| key(y) == k) {
        Some(y) => *y = x,
        None => v.push(x),
    }
}

/// `14:02`, local time.
fn clock(ms: u64) -> String {
    let secs = i64::try_from(ms / 1000).unwrap_or(0);
    let local = ptyro_pty::local_time_hm_at(secs);
    local.map_or_else(|| "a while".into(), |(h, m)| format!("{h:02}:{m:02}"))
}

/// Mirrored machines' things, kept for the client ids of this daemon's clients.
pub(crate) type UpstreamIds = std::sync::Mutex<HashMap<(String, u64), ClientId>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, ws: &str) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": SessionId::new(), "handle": "abc", "name": name, "kind": "shell",
            "argv": [], "cwd": "/", "state": {"t": "running"},
            "size": {"cols": 80, "rows": 24}, "created_ms": 0, "profile": "work",
            "workspace": ws, "controller": 7,
        }))
        .unwrap()
    }

    #[test]
    fn everything_of_a_machine_carries_its_name() {
        let s = session("fix-flaky-test", "main");
        let map = |id: u64| if id == 7 { 70 } else { foreign(id) };
        let q = qualified(s, "laptop2", &map);
        assert_eq!(q.name, "laptop2:fix-flaky-test");
        assert_eq!(q.workspace.as_deref(), Some("laptop2:main"));
        assert_eq!(q.profile, "laptop2:work");
        assert_eq!(q.machine.as_deref(), Some("laptop2"));
        assert_eq!(q.controller.map(|c| c.0), Some(70), "its upstream is our client");
        let ws: WorkspaceInfo = serde_json::from_value(serde_json::json!({
            "id": "w1", "name": "work", "profile": "work", "notify": "all", "projects": ["p"], "created_ms": 0,
        }))
        .unwrap();
        let reply = qualified(Reply::Workspaces { workspaces: vec![ws] }, "laptop2", &|i| i);
        let Reply::Workspaces { workspaces } = reply else { panic!() };
        assert_eq!(
            (workspaces[0].name.as_str(), workspaces[0].profile.as_str()),
            ("laptop2:work", "laptop2:work")
        );
        // Twice is once.
        let again =
            qualified(Reply::Workspaces { workspaces: workspaces.clone() }, "laptop2", &|i| i);
        assert_eq!(again, Reply::Workspaces { workspaces });
    }

    #[test]
    fn a_controller_change_names_our_client_or_none_of_ours() {
        let id = SessionId::new();
        let e = DomainEvent::ControllerChanged { session: id, controller: Some(ClientId(3)) };
        let map = |c: u64| if c == 3 { 30 } else { foreign(c) };
        let DomainEvent::ControllerChanged { controller, .. } = qualified(e, "laptop2", &map)
        else {
            panic!()
        };
        assert_eq!(controller, Some(ClientId(30)));
        assert_eq!(foreign(3) & (1 << 63), 1 << 63);
    }

    #[test]
    fn requests_lose_the_machine_on_their_addresses_only() {
        let req = Request::SendText {
            addr: "laptop2:fix-flaky-test".into(),
            text: "laptop2: keep this".into(),
            enter: true,
            wait_ready_ms: None,
        };
        let mut v = serde_json::to_value(&req).unwrap();
        strip_value(&mut v, "laptop2");
        let Request::SendText { addr, text, .. } = serde_json::from_value(v).unwrap() else {
            panic!()
        };
        assert_eq!((addr.as_str(), text.as_str()), ("fix-flaky-test", "laptop2: keep this"));
        let spawn = Request::Spawn {
            spec: ptyro_core::session::SpawnSpec {
                worktree: Some("laptop2:myrepo/feat".into()),
                workspace: Some("laptop2:work".into()),
                ..Default::default()
            },
        };
        let mut v = serde_json::to_value(&spawn).unwrap();
        let found: HashSet<String> = addresses(&v).into_iter().collect();
        assert_eq!(
            found,
            HashSet::from(["laptop2:myrepo/feat".to_owned(), "laptop2:work".to_owned()])
        );
        strip_value(&mut v, "laptop2");
        let Request::Spawn { spec } = serde_json::from_value(v).unwrap() else { panic!() };
        assert_eq!(spec.worktree.as_deref(), Some("myrepo/feat"));
        assert_eq!(spec.workspace.as_deref(), Some("work"));
        assert!(here_only(&Request::DevicePair {
            name: "laptop2:x".into(),
            scope: Default::default(),
            machine: false
        }));
    }

    #[test]
    fn the_older_side_is_the_one_to_update() {
        let (e, fix) = version_fix("laptop2", "0.0.11", "0.0.12");
        assert!(e.contains("laptop2 runs ptyro 0.0.11 and this machine 0.0.12"), "{e}");
        assert!(fix.contains("on laptop2") && fix.contains("0.0.12"), "{fix}");
        let (_, fix) = version_fix("laptop2", "0.1.0", "0.0.12");
        assert!(fix.contains("update ptyro here to 0.1.0"), "{fix}");
        assert!(newer("0.0.12", "0.0.9"));
    }

    #[test]
    fn a_hidden_workspace_takes_what_only_it_has() {
        let ws = |id: &str, name: &str, projects: &[&str]| -> WorkspaceInfo {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": name, "profile": "p", "notify": "all", "projects": projects, "created_ms": 0,
            }))
            .unwrap()
        };
        let pr = |id: &str| -> ProjectInfo {
            serde_json::from_value(
                serde_json::json!({"id": id, "slug": id, "root": "/r", "created_ms": 0}),
            )
            .unwrap()
        };
        let mut a = session("a", "laptop2:main");
        a.project = Some("p1".into());
        let mut b = session("b", "laptop2:old");
        b.project = Some("p2".into());
        let m = Mirror {
            workspaces: vec![
                ws("w1", "laptop2:main", &["p1"]),
                ws("w2", "laptop2:old", &["p1", "p2"]),
            ],
            projects: vec![pr("p1"), pr("p2")],
            sessions: vec![a.clone(), b],
            ..Default::default()
        };
        let shown = filter_hidden(m, &["old".to_owned()]);
        assert_eq!(shown.workspaces.len(), 1);
        assert_eq!(shown.projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p1"]);
        assert_eq!(shown.sessions.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["a"]);
    }
}
