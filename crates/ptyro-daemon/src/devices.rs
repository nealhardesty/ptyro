//! Paired devices (SPEC §16.3, M9.2): long random tokens, stored only as SHA-256 hashes, each
//! with a scope the daemon enforces on every request and keystroke the device sends. Pairing
//! and revoking happen only from this machine, or from a device with `full` scope. Every remote action that touches a session is
//! written to that session's event log.

use ptyro_core::secret::{ct_eq, token};
use ptyro_core::sync::MutexExt;
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{DeviceInfo, PushSub, Reply, Request, Scope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::daemon::Daemon;
use crate::mail::Caller;

const KV_KEY: &str = "devices";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    /// SHA-256 of the token, hex.
    pub token_sha256: String,
    pub created_ms: u64,
    #[serde(default)]
    pub last_seen_ms: Option<u64>,
    #[serde(default)]
    pub last_via: Option<String>,
    #[serde(default)]
    pub push: Vec<PushSub>,
    /// Not claimed yet: the code's hash and when it stops working. Such a record has no token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingCode>,
    /// Another machine's daemon (MM1.B): its connections are links.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub machine: bool,
}

/// What `ptyro pair` prints instead of the token (security review M2): the printed text lands
/// in scrollback and recordings, so it must be worthless once used or after [`PAIRING_TTL_MS`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCode {
    pub code_sha256: String,
    pub expires_ms: u64,
}

/// How long a pairing code can be claimed.
pub const PAIRING_TTL_MS: u64 = 10 * 60 * 1000;

impl DeviceRecord {
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            scope: self.scope,
            created_ms: self.created_ms,
            last_seen_ms: self.last_seen_ms,
            last_via: self.last_via.clone(),
            push: self.push.len() as u32,
            pending: self.pairing.is_some(),
            machine: self.machine,
        }
    }
}

/// What an authenticated device may do, carried with each of its requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    pub device: u64,
    pub scope: Scope,
    /// A machine's link (MM1.B).
    pub machine: bool,
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Push subscriptions kept for one device.
const MAX_PUSH_PER_DEVICE: usize = 10;

/// Device names are one device each, whatever their case.
fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn id_num(id: &str) -> u64 {
    u64::from_str_radix(id, 16).unwrap_or(0)
}

/// "Always allow": a standing permission, not an answer to one prompt.
fn is_always(decision: &str) -> bool {
    ptyro_agents::Decision::parse(decision) == Some(ptyro_agents::Decision::Always)
}

/// A gate that lets code ship (a change set's push or its merge train) is answered with control
/// or better; the other gates are approve's.
fn ships_code(step: &str) -> bool {
    step.starts_with("push-") || step.starts_with("land-")
}

/// The scope a request needs from a device. The match is exhaustive, with no catch-all: a new
/// request is a compile error until someone decides its scope here (`tests/scopes.rs` then
/// checks the decision against a table and against a daemon). What reaches the machine itself,
/// or changes what ptyro is, is `full`.
pub fn needed(req: &Request) -> Scope {
    use Request::*;
    match req {
        List
        | Get { .. }
        | ReadScreen { .. }
        | ReadScrollback { .. }
        | Status
        | Resolve { .. }
        | StatusExplain { .. }
        | Inbox { .. }
        | Events { .. }
        | Usage
        | History { .. }
        | ProjectList
        | ProjectGet { .. }
        | WorkspaceList
        | WorktreeList { .. }
        | WorktreeGet { .. }
        | WorktreeDiff { .. }
        | WorktreeLog { .. }
        | RunList { .. }
        | RunGet { .. }
        | RunEvents { .. }
        | GateList { .. }
        | MsgList { .. }
        | LeaseList
        | Radar { .. }
        | CheckpointList { .. }
        | CheckpointDiff { .. }
        | WorktreeArchives { .. }
        | Branches { .. }
        | ProfileList
        | Agents
        | Editors
        | ProjectConfig { .. }
        | Replay { .. }
        | Export { .. }
        | Web { on: None, .. }
        | ReviewList { .. }
        | ReviewQueue
        | PrGet { .. }
        | RecipeList { .. }
        | RecipeShow { .. }
        | LoadView { .. }
        | PushKey
        | PushSubscribe { .. }
        | PushUnsubscribe { .. }
        | PushTest
        | ChatGet { .. }
        | ScratchGet
        | LastLaunch { .. }
        | MachineList
        | ScratchTodo { .. }
        | ScratchHistory
        | ScratchPane { create: false, .. } => Scope::Read,
        Answer { decision, .. } if is_always(decision) => Scope::Control,
        Answer { .. }
        | GateResolve { .. }
        | ClearAttention { .. }
        | InboxMark { .. }
        | Mute { .. } => Scope::Approve,
        SendText { .. }
        | SendBytes { .. }
        | Resume { .. }
        | Hibernate { .. }
        | Kill { .. }
        | Remove { .. }
        | Restart { .. }
        | Rename { .. }
        | MsgPost { .. }
        | ReviewAdd { .. }
        | ReviewSend { .. }
        | Iterate { .. }
        | RunCancel { .. }
        | RunRetry { .. }
        // Gathering the evidence runs the recipe's checks in the candidates' worktrees and may
        // start a judge agent: not a read.
        | Compare { .. }
        | Convert { .. }
        | Retitle { .. }
        | Summarize { .. }
        | WorktreeOpen { .. }
        | ScratchPut { .. }
        | ScratchAdd { .. }
        | ScratchMark { .. }
        | ScratchRestore { .. }
        | Adopt { .. } => Scope::Control,
        // Finding one reads; starting one is a shell: full.
        QuickShell { create: false, .. } => Scope::Read,
        // A configured agent (its command comes from the user's config) is control; any other
        // command line is full.
        // A verify check, a lead or `ptyro mcp` is a command line of its own: full.
        Spawn { spec }
            if spec.agent.is_some()
                && spec.argv.is_empty()
                && spec.verify.is_none()
                && spec.lead.is_none()
                && !spec.mcp =>
        {
            Scope::Control
        }
        // The same requests when they start something: a shell, a command line of its own, a
        // new pad pane, switching the web app's server on or off.
        Spawn { .. } | QuickShell { .. } | ScratchPane { .. } | Web { .. } => Scope::Full,
        // Everything else changes the machine, the config, the daemon or who may talk to it.
        MachineAdd { .. } | MachineRemove { .. } | MachineHide { .. } | MachineRetry { .. } => Scope::Full,
        Shutdown { .. } | Upgrade { .. } | DevicePair { .. } | DeviceClaim { .. } | DeviceList | DeviceRevoke { .. } | SaveView { .. } | ReloadConfig | ProjectAdd { .. } | ProjectRemove { .. } | ProjectPin { .. } | ProjectScripts { .. } | ProjectTrust { .. } | ProjectUntrust { .. } | Discover { .. } | WorkspaceNew { .. } | WorkspaceSet { .. } | WorkspaceRename { .. } | WorkspaceRemove { .. } | WorkspaceMember { .. } | WorktreeNew { .. } | WorktreeRemove { .. } | WorktreeRefresh { .. } | WorktreeRun { .. } | WorktreeLock { .. } | WorktreeRenameBranch { .. } | Gc | ProjectSuggest { .. } | WorktreeArchive { .. } | WorktreeRestore { .. } | WorktreeFinish { .. } | ProfileEnv { .. } | ProfileRename { .. } | ProfileResolve { .. } | Hook { .. } | StatusLine { .. } | Report { .. } | NotifyTest | Doctor | SelfTest { .. } | WorktreeCommit { .. } | WorktreeIntegrate { .. } | WorktreePair { .. } | PrCreate { .. } | PrFix { .. } | PrAutomerge { .. } | PrWatch { .. } | PrReply { .. } | ReviewRemove { .. } | ReviewDone { .. } | RunFork { .. } | RecipeRun { .. } | Fanout { .. } | Pick { .. } | Discard { .. } | MsgCheck { .. } | MsgAck { .. } | LeaseClaim { .. } | LeaseRelease { .. } | CheckpointRestore { .. } | SetVerify { .. } | LeadStart { .. } | Lead { .. } | Cs { .. } => Scope::Full,
    }
}

/// The session a request acts on, for the audit trail.
fn target(req: &Request) -> Option<&str> {
    use Request::*;
    match req {
        SendText { addr, .. }
        | SendBytes { addr, .. }
        | Answer { addr, .. }
        | ClearAttention { addr, .. }
        | Resume { addr }
        | Hibernate { addr }
        | Kill { addr, .. }
        | Restart { addr, .. }
        | Rename { addr, .. } => Some(addr),
        _ => None,
    }
}

impl Daemon {
    /// Devices from the store (or nothing, without one).
    pub(crate) async fn load_devices(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get(KV_KEY.into()).await
            && let Ok(list) = serde_json::from_value::<Vec<DeviceRecord>>(v)
        {
            *self.devices.locked() = list;
        }
    }

    pub(crate) fn save_devices(&self) {
        let v = serde_json::to_value(&*self.devices.locked()).unwrap_or_default();
        self.save_kv(KV_KEY, v);
    }

    /// The device a token belongs to. Every record is compared, in constant time, whatever
    /// matches first.
    pub(crate) fn authenticate(&self, token: Option<&str>, via: Option<&str>) -> Option<Grant> {
        let h = hash(token?);
        let mut devices = self.devices.locked();
        let mut found = None;
        for (i, d) in devices.iter().enumerate() {
            if d.pairing.is_none() && ct_eq(d.token_sha256.as_bytes(), h.as_bytes()) {
                found = Some(i);
            }
        }
        let d = &mut devices[found?];
        d.last_seen_ms = Some(now_ms());
        d.last_via = via.map(str::to_owned);
        Some(Grant { device: id_num(&d.id), scope: d.scope, machine: d.machine })
    }

    /// Forget pairings whose code expired unclaimed.
    fn prune_pairings(&self) {
        let now = now_ms();
        let pruned = {
            let mut d = self.devices.locked();
            let before = d.len();
            d.retain(|x| x.pairing.as_ref().is_none_or(|p| p.expires_ms > now));
            before != d.len()
        };
        if pruned {
            self.save_devices();
        }
    }

    /// Still paired (revocation closes live connections).
    pub(crate) fn device_valid(&self, device: u64) -> bool {
        self.devices.locked().iter().any(|d| id_num(&d.id) == device)
    }

    pub(crate) fn device_name(&self, device: u64) -> String {
        self.devices
            .locked()
            .iter()
            .find(|d| id_num(&d.id) == device)
            .map_or_else(|| format!("device {device:012x}"), |d| d.name.clone())
    }

    /// Hold a device to its scope; record what it did to sessions.
    pub(crate) fn guard_device(&self, req: &Request, caller: Caller) -> Result<(), Error> {
        let Some(g) = caller.grant else { return Ok(()) };
        // A code is claimed by `ptyro serve` for a device that isn't one yet. Pairing and
        // revoking are the machine's own, and a device with `full` scope's, which can run any
        // command as you already (the web app's Devices page).
        if matches!(req, Request::DeviceClaim { .. })
            || (matches!(req, Request::DevicePair { .. } | Request::DeviceRevoke { .. })
                && g.scope < Scope::Full)
        {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "devices are paired and revoked on the machine that runs ptyro, or from a device paired with `--scope full`",
            ));
        }
        let need = needed(req);
        if g.scope < need {
            tracing::warn!(device = %format!("{:012x}", g.device), "refused: needs {}", need.label());
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                format!(
                    "this device is paired with scope {}; that needs {} (`ptyro pair --scope {}`)",
                    g.scope.label(),
                    need.label(),
                    need.label()
                ),
            ));
        }
        if let Request::GateResolve { id, .. } = req
            && g.scope < Scope::Control
            && self.find_gate(id.trim()).is_some_and(|(_, step, _)| ships_code(&step))
        {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "this gate lets code ship: answering it needs control (`ptyro pair --scope control`)",
            ));
        }
        if need >= Scope::Approve
            && let Some(s) = target(req).and_then(|a| self.resolve(a).ok())
        {
            self.audit(&s, g, req_label(req));
        }
        Ok(())
    }

    /// A remote action on a session, in its event log (and the daemon log).
    pub(crate) fn audit(&self, s: &crate::session::Session, g: Grant, what: &str) {
        let device = self.device_name(g.device);
        tracing::info!(session = %s.id, %device, "remote {what}");
        s.record_event("remote", serde_json::json!({ "device": device, "action": what }));
    }

    pub(crate) async fn handle_devices(
        &self,
        req: Request,
        caller: Caller,
    ) -> Result<Reply, Error> {
        match req {
            Request::DevicePair { name, scope, machine } => {
                let name = name.trim();
                if name.is_empty() || name.len() > 64 {
                    return Err(Error::invalid("a device name is 1–64 characters"));
                }
                let code = token("ptp_").map_err(|e| Error::internal(format!("random: {e}")))?;
                let id = ptyro_core::secret::random_bytes(6)
                    .map_err(|e| Error::internal(format!("random: {e}")))?
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                // Tests shorten it (never lengthen it) with PTYRO_TEST_PAIRING_TTL_MS.
                let ttl = std::env::var("PTYRO_TEST_PAIRING_TTL_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .map_or(PAIRING_TTL_MS, |t: u64| t.min(PAIRING_TTL_MS));
                let expires_ms = now_ms() + ttl;
                let rec = DeviceRecord {
                    id,
                    name: name.to_owned(),
                    scope,
                    token_sha256: String::new(),
                    created_ms: now_ms(),
                    last_seen_ms: None,
                    last_via: None,
                    push: Vec::new(),
                    pairing: Some(PairingCode { code_sha256: hash(&code), expires_ms }),
                    machine,
                };
                let device = rec.info();
                self.prune_pairings();
                let replaces = {
                    let mut devices = self.devices.locked();
                    // A name is one device: an earlier unused link for it is superseded now,
                    // and a paired one is replaced when this link is claimed (not before, so
                    // an abandoned pairing doesn't cut the phone off).
                    devices.retain(|d| d.pairing.is_none() || !same_name(&d.name, &rec.name));
                    let replaces = devices
                        .iter()
                        .find(|d| d.pairing.is_none() && same_name(&d.name, &rec.name))
                        .map(DeviceRecord::info);
                    devices.push(rec);
                    replaces
                };
                self.save_devices();
                tracing::info!(device = %device.id, name = %device.name, scope = device.scope.label(), replaces = ?replaces.as_ref().map(|d| d.id.clone()), "pairing code issued");
                Ok(Reply::Pairing { device, code, expires_ms, replaces })
            }
            Request::DeviceClaim { code, via } => {
                self.prune_pairings();
                let h = hash(&code);
                let token = token("ptd_").map_err(|e| Error::internal(format!("random: {e}")))?;
                let device = {
                    let mut devices = self.devices.locked();
                    let mut found = None;
                    for (i, d) in devices.iter().enumerate() {
                        if d.pairing
                            .as_ref()
                            .is_some_and(|p| ct_eq(p.code_sha256.as_bytes(), h.as_bytes()))
                        {
                            found = Some(i);
                        }
                    }
                    let Some(i) = found else {
                        return Err(Error::new(
                            ErrorCode::PermissionDenied,
                            "this pairing code is unknown, already used or expired: run `ptyro pair` again",
                        ));
                    };
                    let d = &mut devices[i];
                    // Used up: the printed code is worth nothing from here on.
                    d.pairing = None;
                    d.token_sha256 = hash(&token);
                    d.last_via = via;
                    let device = d.info();
                    // The new credentials replace the old device of the same name: its token
                    // stops working and its connections close (they were checked against it).
                    let before = devices.len();
                    let mut replaced = None;
                    devices.retain(|x| {
                        let old = x.id != device.id
                            && x.pairing.is_none()
                            && same_name(&x.name, &device.name);
                        if old && replaced.is_none() {
                            replaced = Some(x.info());
                        }
                        !old
                    });
                    if before != devices.len() {
                        self.devices_epoch.send_modify(|e| *e += 1);
                    }
                    (device, replaced)
                };
                let (device, replaced) = device;
                self.save_devices();
                tracing::info!(device = %device.id, name = %device.name, via = ?device.last_via, replaced = ?replaced.as_ref().map(|d| d.id.clone()), "paired");
                Ok(Reply::Paired { device, token, replaced })
            }
            Request::DeviceList => {
                self.prune_pairings();
                Ok(Reply::Devices {
                    devices: self.devices.locked().iter().map(DeviceRecord::info).collect(),
                })
            }
            Request::DeviceRevoke { id } => {
                let removed = {
                    let mut d = self.devices.locked();
                    // Names needn't be unique: by name, only when it names one device.
                    let named = d.iter().filter(|x| x.name == id).count();
                    if named > 1 && !d.iter().any(|x| x.id == id) {
                        return Err(Error::new(
                            ErrorCode::Ambiguous,
                            format!(
                                "{named} devices are called {id:?}: revoke by id (ptyro devices)"
                            ),
                        ));
                    }
                    let before = d.len();
                    d.retain(|x| x.id != id && x.name != id);
                    before != d.len()
                };
                if !removed {
                    return Err(Error::not_found(&id));
                }
                self.save_devices();
                self.devices_epoch.send_modify(|e| *e += 1);
                tracing::info!(device = %id, "revoked");
                Ok(Reply::Done)
            }
            Request::PushKey => Ok(Reply::PushKey { key: self.vapid()?.public_b64() }),
            Request::PushSubscribe { sub } => {
                let g = caller.grant.ok_or_else(|| {
                    Error::invalid("push subscriptions belong to a paired device")
                })?;
                url_ok(&sub.endpoint, &self.config().remote.push_hosts)?;
                // base64url keys of the sizes browsers make (P-256 point 87 chars, auth 22).
                let b64 = |s: &str, max: usize| {
                    !s.is_empty()
                        && s.len() <= max
                        && s.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'='
                        })
                };
                if sub.endpoint.len() > 1024 || !b64(&sub.p256dh, 128) || !b64(&sub.auth, 64) {
                    return Err(Error::invalid("that isn't a push subscription a browser makes"));
                }
                {
                    let mut devices = self.devices.locked();
                    // One endpoint belongs to one device.
                    for d in devices.iter_mut() {
                        d.push.retain(|p| p.endpoint != sub.endpoint);
                    }
                    if let Some(d) = devices.iter_mut().find(|d| id_num(&d.id) == g.device) {
                        d.push.push(sub);
                        // A browser has a handful; the oldest go first, so a device can't make
                        // every notification a thousand requests.
                        let extra = d.push.len().saturating_sub(MAX_PUSH_PER_DEVICE);
                        d.push.drain(..extra);
                    }
                }
                self.save_devices();
                Ok(Reply::Done)
            }
            Request::PushUnsubscribe { endpoint } => {
                // A device removes its own subscriptions only; a local client, any.
                for d in self.devices.locked().iter_mut() {
                    if caller.grant.is_some_and(|g| id_num(&d.id) != g.device) {
                        continue;
                    }
                    d.push.retain(|p| p.endpoint != endpoint);
                }
                self.save_devices();
                Ok(Reply::Done)
            }
            Request::PushTest => {
                let results = self
                    .push_all(
                        caller.grant.map(|g| g.device),
                        crate::push::Payload {
                            title: "ptyro".into(),
                            body: "Push notifications work.".into(),
                            session: None,
                            kind: "test".into(),
                            tag: "ptyro-test".into(),
                            since: None,
                        },
                    )
                    .await;
                if results.is_empty() {
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        "no device has push notifications turned on (open the web app → Notifications)",
                    ));
                }
                Ok(Reply::Notes { notes: results })
            }
            _ => Err(Error::invalid("not a device request")),
        }
    }
}

/// Push services are HTTPS; anything else is refused before it reaches curl.
fn url_ok(endpoint: &str, extra: &[String]) -> Result<(), Error> {
    // Tests deliver to a plain-HTTP listener on loopback.
    let test = std::env::var_os("PTYRO_TEST_PUSH_HTTP").is_some()
        && endpoint.starts_with("http://127.0.0.1:");
    // Only the browsers' push services: a device can't make the daemon post to any host.
    let host = push_host(endpoint).unwrap_or_default();
    let known = !host.is_empty()
        && [
            "fcm.googleapis.com",
            "android.googleapis.com",
            "updates.push.services.mozilla.com",
            "push.apple.com",
            "notify.windows.com",
        ]
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|k| {
            let k = k.to_ascii_lowercase();
            host == k || host.ends_with(&format!(".{k}"))
        });
    if (known || test) && endpoint.len() < 2048 && !endpoint.contains(['\n', '"', ' ']) {
        Ok(())
    } else {
        Err(Error::invalid("a push endpoint is a browser push service's https URL"))
    }
}

/// The lowercased host of `https://host[:port][/path][?query]`, or `None` for anything a URL
/// parser (curl's) might read as another host: userinfo (`@`), a fragment (`#`), backslashes,
/// controls, a host that isn't plain dot-separated DNS labels (no `%`, brackets, empty labels),
/// a port that isn't digits. Strict on purpose: push services hand out plain URLs.
fn push_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.strip_prefix("https://")?;
    if endpoint.contains(['#', '\\', '@']) || endpoint.chars().any(char::is_control) {
        return None;
    }
    let authority = rest.split(['/', '?']).next()?;
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let label =
        |l: &str| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    (host.split('.').all(label) && port_ok).then(|| host.to_ascii_lowercase())
}

fn req_label(req: &Request) -> &'static str {
    match req {
        Request::SendText { .. } | Request::SendBytes { .. } => "typed",
        Request::Answer { .. } => "answered",
        Request::ClearAttention { .. } => "cleared attention",
        Request::Resume { .. } => "resumed",
        Request::Hibernate { .. } => "hibernated",
        Request::Kill { .. } => "stopped",
        Request::Restart { .. } => "restarted",
        Request::Rename { .. } => "renamed",
        _ => "acted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::session::SpawnSpec;

    #[test]
    fn scopes_are_ordered_and_requests_classified() {
        assert!(
            Scope::Read < Scope::Approve
                && Scope::Approve < Scope::Control
                && Scope::Control < Scope::Full
        );
        assert_eq!(needed(&Request::List), Scope::Read);
        assert_eq!(
            needed(&Request::Answer { addr: "a".into(), decision: "allow".into(), expect: None }),
            Scope::Approve
        );
        assert_eq!(
            needed(&Request::SendText {
                addr: "a".into(),
                text: "x".into(),
                enter: true,
                wait_ready_ms: None
            }),
            Scope::Control
        );
        let any = SpawnSpec { argv: vec!["sh".into()], ..SpawnSpec::default() };
        assert_eq!(needed(&Request::Spawn { spec: any }), Scope::Full);
        let verify = SpawnSpec {
            agent: Some("claude".into()),
            verify: Some("curl x | sh".into()),
            ..SpawnSpec::default()
        };
        assert_eq!(needed(&Request::Spawn { spec: verify }), Scope::Full);
        assert!(url_ok("https://fcm.googleapis.com/fcm/send/abc", &[]).is_ok());
        assert!(url_ok("https://web.push.apple.com/QGx", &[]).is_ok());
        assert!(url_ok("https://internal.corp/admin", &[]).is_err());
        assert!(url_ok("https://evilpush.apple.com.example/x", &[]).is_err());
        assert!(url_ok("https://ntfy.example.com/up/x", &["ntfy.example.com".into()]).is_ok());
        assert!(url_ok("https://web.push.apple.com:443/QGx?a=b", &[]).is_ok());
        // Anything curl could send to a host other than the one the allow-list saw.
        for bad in [
            "https://evil.example#.push.apple.com/x",
            "https://evil.example#@web.push.apple.com/x",
            "https://web.push.apple.com@evil.example/x",
            "https://user:pw@web.push.apple.com/x",
            "https://evil.example\\.push.apple.com/x",
            "https://evil.example\\@web.push.apple.com/x",
            "https://web.push.apple.com:evil.example/x",
            "https://web.push.apple.com:/x",
            "https://%65vil.push.apple.com/x",
            "https://[::1].push.apple.com/x",
            "https://web..push.apple.com/x",
            "https://web.push.apple.com./x",
            "http://web.push.apple.com/x",
            "https://web.push.apple.com/x\t",
            "https://push.apple.com.evil.example/x",
        ] {
            assert!(url_ok(bad, &[]).is_err(), "{bad}");
        }
        // Gathering a compare's evidence runs the project's checks: control's, not a read.
        assert_eq!(needed(&Request::Compare { run: "r".into(), refresh: false }), Scope::Control);
        // "Always" is a standing permission; one allow or deny is approve's.
        let ans = |d: &str| Request::Answer { addr: "a".into(), decision: d.into(), expect: None };
        assert_eq!(needed(&ans("allow")), Scope::Approve);
        assert_eq!(needed(&ans("always")), Scope::Control);
        assert_eq!(needed(&ans("a")), Scope::Control);
        assert!(
            ships_code("push-1700000000000") && ships_code("land-api") && !ships_code("review")
        );
        assert_eq!(needed(&Request::Upgrade { binary: None }), Scope::Full);
        assert_eq!(needed(&Request::DeviceList), Scope::Full);
        // Stopping what you started is control's, as the scope's description says; starting any
        // command, making worktrees and shipping aren't.
        assert_eq!(needed(&Request::Kill { addr: "x".into(), signal: None }), Scope::Control);
        assert_eq!(needed(&Request::Remove { addr: "x".into() }), Scope::Control);
        assert_eq!(needed(&Request::Agents), Scope::Read);
        assert_eq!(needed(&Request::Replay { addr: "x".into() }), Scope::Read);
        assert_eq!(needed(&Request::Editors), Scope::Read);
        assert_eq!(needed(&Request::Web { on: None, via: None }), Scope::Read);
        assert_eq!(needed(&Request::Web { on: Some(true), via: None }), Scope::Full);
        assert_eq!(
            needed(&Request::WorktreeOpen { addr: "x".into(), editor: None }),
            Scope::Control
        );
    }
}
