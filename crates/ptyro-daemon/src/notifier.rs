//! Notifications (SPEC §9.5, §9.6, M4.6, M4.22): every attention goes to the inbox; then, unless
//! muted, quiet, coalesced or kept to a badge by its workspace, to the TUI (OSC 9/777 and the
//! bell), the desktop, and external sinks (ntfy, Pushover, Slack, Discord, Telegram, a command).
//!
//! Approve/deny links in sinks point at a small callback listener (off unless configured). Links
//! are signed (HMAC-SHA256 with a daemon key), single-use and expiring. A GET only shows a page
//! with a button; only the POST acts, so link previews and unfurlers can't approve anything.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use ptyro_agents::Decision;
use ptyro_core::config::{Secret, SinkDef, in_quiet_hours, parse_duration, parse_quiet_hours};
use ptyro_core::model::DEFAULT_PROFILE;
use ptyro_core::status::{AgentStatus, AttentionKind, InboxEntry};
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{DomainEvent, Reply};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::daemon::Daemon;
use crate::session::Session;

/// One notification, as channels see it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Note {
    pub session: String,
    pub session_name: String,
    pub kind: AttentionKind,
    pub title: String,
    pub body: String,
    pub profile: String,
    pub workspace: Option<String>,
    pub project: Option<String>,
    /// Approve / deny links (when the callback listener is on and the prompt can be answered).
    pub approve_url: Option<String>,
    pub deny_url: Option<String>,
}

impl Daemon {
    /// The inbox entry, then every channel that applies.
    pub(crate) async fn notify(
        self: &Arc<Self>,
        s: &Arc<Session>,
        kind: AttentionKind,
        body: String,
    ) {
        let info = self.info(s);
        let config = self.config();
        let project = info.project.as_deref().and_then(|p| self.project_by_id(p)).map(|p| p.slug);
        let label = self.profile_label_for(&info.profile);
        let mut title = String::new();
        if let Some(l) = &label {
            title.push_str(&format!("[{l}] "));
        }
        title.push_str(&info.name);
        if let Some(p) = &project {
            title.push_str(&format!(" ({p})"));
        }
        let mut entry = InboxEntry {
            id: 0,
            session: info.id.full(),
            session_name: info.name.clone(),
            at_ms: now_ms(),
            kind,
            title: title.clone(),
            body: body.clone(),
            read_ms: None,
            workspace: info.workspace.clone(),
            profile: Some(info.profile.clone()),
        };
        entry.id = match &self.store {
            Some(st) => st.inbox_add(entry.clone()).await.unwrap_or(0),
            None => {
                let mut mem = self.inbox_mem.lock().expect("inbox");
                let id = mem.len() as i64 + 1;
                mem.push(InboxEntry { id, ..entry.clone() });
                id
            }
        };
        let _ = self.events.send(DomainEvent::InboxAdded { entry });
        // Beyond the inbox: kind, mute, workspace mode, quiet hours, coalescing.
        let n = &config.notifications;
        let breakthrough = config.attention.breakthrough.iter().any(|k| k == kind.label());
        if !n.on.iter().any(|k| k == kind.label()) {
            return;
        }
        if self.muted(s) {
            return;
        }
        let mode = info
            .workspace
            .as_deref()
            .and_then(|w| self.find_workspace(w).ok())
            .map(|w| w.notify)
            .unwrap_or_else(|| "all".into());
        if mode == "mute" || (mode == "badge" && !breakthrough) {
            return;
        }
        if let Some(q) = n.quiet_hours.as_deref().and_then(|q| parse_quiet_hours(q).ok())
            && let Some((h, m)) = ptyro_pty::local_time_hm()
            && in_quiet_hours(q, u16::from(h) * 60 + u16::from(m))
            && !breakthrough
        {
            return;
        }
        let coalesce = parse_duration(&n.coalesce).map(|d| d.as_millis() as u64).unwrap_or(10_000);
        {
            let mut last = self.last_notified.lock().expect("last_notified");
            let now = now_ms();
            // Blocked always gets through: it's the one that costs time.
            if kind != AttentionKind::Blocked
                && last.get(&s.id).is_some_and(|t| now.saturating_sub(*t) < coalesce)
            {
                return;
            }
            last.insert(s.id, now);
        }
        let answerable = kind == AttentionKind::Blocked
            && s.engine.lock().expect("engine").record.as_ref().is_some_and(|r| {
                r.status == AgentStatus::Blocked && r.kind.as_deref() == Some("permission")
            })
            && s.adapter().is_some_and(|a| ptyro_agents::adapter(&a).caps().answer);
        let (approve_url, deny_url) = if answerable {
            self.action_links(&info.id.full(), info.status.as_ref().map(|r| r.since_ms))
        } else {
            (None, None)
        };
        let note = Note {
            session: info.id.full(),
            session_name: info.name,
            kind,
            title,
            body,
            profile: info.profile,
            workspace: info.workspace,
            project,
            approve_url,
            deny_url,
        };
        self.deliver(&note, false).await;
    }

    fn profile_label_for(&self, profile: &str) -> Option<String> {
        let config = self.config();
        if profile == DEFAULT_PROFILE
            && !config.profiles.contains_key(DEFAULT_PROFILE)
            && config.profiles.is_empty()
        {
            return None;
        }
        Some(
            config
                .profiles
                .get(profile)
                .and_then(|p| p.label.clone())
                .unwrap_or_else(|| profile.to_owned()),
        )
    }

    /// Send to every configured channel; `results` describe what happened (for `notify test`).
    pub(crate) async fn deliver(self: &Arc<Self>, note: &Note, report: bool) -> Vec<String> {
        let config = self.config();
        let ch = &config.notifications.channels;
        let mut results = vec!["inbox: added".to_owned()];
        if ch.iter().any(|c| c == "osc" || c == "bell")
            && let Ok(session) = note.session.parse()
        {
            let _ = self.events.send(DomainEvent::Notify {
                session,
                kind: note.kind,
                title: note.title.clone(),
                body: note.body.clone(),
                bell: ch.iter().any(|c| c == "bell"),
            });
            results.push(format!(
                "terminal: sent to {} client(s)",
                self.clients.load(std::sync::atomic::Ordering::Relaxed)
            ));
        }
        if ch.iter().any(|c| c == "desktop") {
            let r = desktop(note, config.notifications.desktop_command.as_deref());
            results.push(format!("desktop: {r}"));
        }
        // Paired devices that turned notifications on (M9.4). In the background: a slow push
        // service must not hold up the rest.
        if config.remote.push {
            let payload = crate::push::Payload {
                title: note.title.clone(),
                body: note.body.clone(),
                session: Some(note.session.clone()),
                kind: note.kind.label().to_owned(),
                tag: format!("ptyro-{}", note.session),
                since: self
                    .resolve(&note.session)
                    .ok()
                    .and_then(|s| s.engine.lock().expect("engine").record.clone())
                    .filter(|r| r.status == ptyro_core::status::AgentStatus::Blocked)
                    .map(|r| r.since_ms),
            };
            if report {
                results.extend(self.push_all(None, payload).await);
            } else {
                let me = Arc::clone(self);
                tokio::spawn(async move {
                    for r in me.push_all(None, payload).await {
                        tracing::debug!("{r}");
                    }
                });
            }
        }
        if ch.iter().any(|c| c == "sinks") || !config.notifications.sinks.is_empty() {
            let sinks: Vec<SinkDef> = config
                .notifications
                .sinks
                .iter()
                .filter(|sink| {
                    let on = sink.on();
                    report
                        || if on.is_empty() {
                            matches!(note.kind, AttentionKind::Blocked | AttentionKind::Errored)
                        } else {
                            on.iter().any(|k| k == note.kind.label())
                        }
                })
                .cloned()
                .collect();
            let (me, note2) = (Arc::clone(self), note.clone());
            // Slow or hung sinks (a curl, a secret command waiting on a pinentry) must never
            // hold up status, attention or the hooks agents wait on: off the status path,
            // unless someone is waiting for the report (`notify test`).
            let job = async move {
                let mut out = Vec::new();
                for sink in sinks {
                    let kind = sink.kind();
                    let (me2, n) = (Arc::clone(&me), note2.clone());
                    let r = tokio::task::spawn_blocking(move || me2.send_sink(&sink, &n))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                    out.push(format!("{kind}: {}", r.unwrap_or_else(|e| format!("failed: {e}"))));
                }
                out
            };
            if report {
                results.extend(job.await);
            } else {
                tokio::spawn(async move {
                    for r in job.await {
                        tracing::debug!("sink {r}");
                    }
                });
            }
        }
        results
    }

    pub(crate) async fn notify_test(self: &Arc<Self>) -> Result<Reply, Error> {
        let note = Note {
            session: String::new(),
            session_name: "ptyro".into(),
            kind: AttentionKind::Notification,
            title: "ptyro test".into(),
            body: "If you can read this, notifications reach you.".into(),
            profile: DEFAULT_PROFILE.into(),
            workspace: None,
            project: None,
            approve_url: None,
            deny_url: None,
        };
        Ok(Reply::Notified { results: self.deliver(&note, true).await })
    }

    fn sink_secret(&self, s: &Secret) -> Result<String, String> {
        match s {
            Secret::Literal(v) => Ok(v.clone()),
            Secret::Command { command } => {
                let (prog, args) = command.split_first().ok_or("empty secret command")?;
                let mut c = std::process::Command::new(prog);
                c.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                let (ok, out) =
                    run_bounded(c, None, SINK_TIMEOUT).map_err(|e| format!("{prog}: {e}"))?;
                if !ok {
                    return Err(format!("{prog} failed"));
                }
                Ok(String::from_utf8_lossy(&out).trim_end().to_owned())
            }
        }
    }

    /// One external sink (blocking). Secrets go to curl through its stdin config, never argv.
    fn send_sink(&self, sink: &SinkDef, note: &Note) -> Result<String, String> {
        let text = format!("{}\n{}", note.title, note.body);
        let links = match (&note.approve_url, &note.deny_url) {
            (Some(a), Some(d)) => Some((a.clone(), d.clone())),
            _ => None,
        };
        match sink {
            SinkDef::Command { command, .. } => {
                let (prog, args) = command.split_first().ok_or("empty command")?;
                let mut c = std::process::Command::new(prog);
                c.args(args).stderr(std::process::Stdio::null());
                let input = serde_json::to_string(note).unwrap_or_default().into_bytes();
                run_bounded(c, Some(input), SINK_TIMEOUT).map_err(|e| format!("{prog}: {e}"))?;
                Ok("ran".into())
            }
            SinkDef::Ntfy { url, token, .. } => {
                let mut cfg = vec![
                    format!("url = {}", quote(url)),
                    format!("header = {}", quote(&format!("Title: {}", ascii(&note.title)))),
                ];
                cfg.push(format!("header = {}", quote(&format!("Tags: {}", note.kind.label()))));
                if note.kind == AttentionKind::Blocked {
                    cfg.push(format!("header = {}", quote("Priority: high")));
                }
                if let Some(t) = token {
                    cfg.push(format!(
                        "header = {}",
                        quote(&format!("Authorization: Bearer {}", self.sink_secret(t)?))
                    ));
                }
                if let Some((a, d)) = &links {
                    // ntfy's `http` actions are POSTed by the phone app itself.
                    cfg.push(format!("header = {}", quote(&format!("Actions: http, Approve, {a}, method=POST, clear=true; http, Deny, {d}, method=POST, clear=true"))));
                }
                cfg.push(format!("data = {}", quote(&note.body)));
                curl(&cfg)
            }
            SinkDef::Pushover { token, user, .. } => {
                let mut cfg = vec![
                    format!("url = {}", quote("https://api.pushover.net/1/messages.json")),
                    format!(
                        "form-string = {}",
                        quote(&format!("token={}", self.sink_secret(token)?))
                    ),
                    format!(
                        "form-string = {}",
                        quote(&format!("user={}", self.sink_secret(user)?))
                    ),
                    format!("form-string = {}", quote(&format!("title={}", note.title))),
                    format!("form-string = {}", quote(&format!("message={}", note.body))),
                ];
                if let Some((a, _)) = &links {
                    cfg.push(format!("form-string = {}", quote(&format!("url={a}"))));
                    cfg.push(format!("form-string = {}", quote("url_title=Review & approve")));
                }
                curl(&cfg)
            }
            SinkDef::Slack { webhook, .. } => {
                let mut msg = serde_json::json!({ "text": text });
                if let Some((a, d)) = &links {
                    msg["blocks"] = serde_json::json!([
                        { "type": "section", "text": { "type": "mrkdwn", "text": format!("*{}*\n{}", note.title, note.body) } },
                        { "type": "actions", "elements": [
                            { "type": "button", "text": { "type": "plain_text", "text": "Approve" }, "url": a, "style": "primary" },
                            { "type": "button", "text": { "type": "plain_text", "text": "Deny" }, "url": d, "style": "danger" }
                        ] }
                    ]);
                }
                json_post(&self.sink_secret(webhook)?, &msg)
            }
            SinkDef::Discord { webhook, .. } => {
                let mut content = format!("**{}**\n{}", note.title, note.body);
                if let Some((a, d)) = &links {
                    content.push_str(&format!("\n[Approve](<{a}>) · [Deny](<{d}>)"));
                }
                json_post(&self.sink_secret(webhook)?, &serde_json::json!({ "content": content }))
            }
            SinkDef::Telegram { token, chat_id, .. } => {
                let mut msg = serde_json::json!({ "chat_id": chat_id, "text": text });
                if let Some((a, d)) = &links {
                    msg["reply_markup"] = serde_json::json!({ "inline_keyboard": [[{ "text": "Approve", "url": a }, { "text": "Deny", "url": d }]] });
                }
                json_post(
                    &format!(
                        "https://api.telegram.org/bot{}/sendMessage",
                        self.sink_secret(token)?
                    ),
                    &msg,
                )
            }
        }
    }

    // ── mutes ──────────────────────────────────────────────────────────────

    fn muted(&self, s: &Session) -> bool {
        let (project, workspace) = {
            let m = s.meta.lock().expect("meta");
            (m.place.project.clone(), m.place.workspace.clone())
        };
        let now = now_ms();
        let mutes = self.mutes.lock().expect("mutes");
        let hit = |t: &str| mutes.get(t).is_some_and(|until| until.is_none_or(|u| u > now));
        hit(&s.id.full())
            || project.is_some_and(|p| hit(&format!("project:{p}")))
            || workspace.is_some_and(|w| hit(&format!("workspace:{w}")))
    }

    pub(crate) async fn mute(
        self: &Arc<Self>,
        target: &str,
        minutes: Option<u32>,
        unmute: bool,
    ) -> Result<Reply, Error> {
        let key = if let Some(p) = target.strip_prefix("project:") {
            format!("project:{}", self.find_project(p)?.id)
        } else if let Some(w) = target.strip_prefix("workspace:") {
            format!("workspace:{}", self.find_workspace(w)?.name)
        } else {
            self.resolve(target)?.id.full()
        };
        let until = minutes.map(|m| now_ms() + u64::from(m) * 60_000);
        let snapshot = {
            let mut mutes = self.mutes.lock().expect("mutes");
            if unmute {
                mutes.remove(&key);
            } else {
                mutes.insert(key, until);
            }
            serde_json::to_value(&*mutes).unwrap_or_default()
        };
        if let Some(st) = &self.store {
            let _ = st.kv_set("mutes".into(), snapshot).await;
        }
        Ok(Reply::Done)
    }

    // ── signed action links and the callback listener ──────────────────────

    fn callback_key(&self) -> Vec<u8> {
        let path = self.paths.data.join("callback.key");
        if let Ok(k) = std::fs::read(&path)
            && k.len() >= 32
        {
            return k;
        }
        let k = ptyro_agents::adapter::random_hex().into_bytes();
        // Created private: never readable by others, even for a moment.
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            let _ = f.write_all(&k);
        }
        let _ =
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
        k
    }

    /// Signed approve/deny links for the prompt pending now (`since`: its blocked status's
    /// `since_ms`, so a link never approves a later prompt).
    fn action_links(&self, session: &str, since: Option<u64>) -> (Option<String>, Option<String>) {
        let config = self.config();
        let cb = &config.notifications.callback;
        let Some(listen) = &cb.listen else { return (None, None) };
        let base = cb.public_url.clone().unwrap_or_else(|| format!("http://{listen}"));
        let ttl = parse_duration(&cb.link_ttl).map(|d| d.as_millis() as u64).unwrap_or(900_000);
        let key = self.callback_key();
        let mk = |d: &str| {
            let claims = serde_json::json!({ "s": session, "d": d, "e": now_ms() + ttl, "n": ptyro_agents::adapter::random_hex(), "p": since });
            let token = sign(&key, &claims.to_string());
            format!("{}/a/{token}", base.trim_end_matches('/'))
        };
        (Some(mk("allow")), Some(mk("deny")))
    }

    /// Check a token: signature, expiry, single use. Returns (session, decision, the prompt).
    fn redeem(
        &self,
        token: &str,
        consume: bool,
    ) -> Result<(String, Decision, Option<u64>), &'static str> {
        let claims = verify(&self.callback_key(), token).ok_or("this link is not valid")?;
        let v: serde_json::Value =
            serde_json::from_str(&claims).map_err(|_| "this link is not valid")?;
        if v["e"].as_u64().is_none_or(|e| e < now_ms()) {
            return Err("this link has expired");
        }
        let nonce = v["n"].as_str().unwrap_or_default().to_owned();
        let mut used = self.used_links.lock().expect("used_links");
        if used.contains_key(&nonce) {
            return Err("this link was already used");
        }
        if consume {
            used.retain(|_, exp| *exp > now_ms());
            used.insert(nonce, v["e"].as_u64().unwrap_or(0));
        }
        let d = v["d"].as_str().and_then(Decision::parse).ok_or("this link is not valid")?;
        Ok((v["s"].as_str().unwrap_or_default().to_owned(), d, v["p"].as_u64()))
    }

    /// The approve/deny listener (M4.22), if `[notifications.callback] listen` is set. Plain
    /// HTTP, so only on loopback or Tailscale unless `insecure` (as `ptyro serve`).
    pub fn start_callback(self: &Arc<Self>) {
        let cb = self.config().notifications.callback.clone();
        let Some(listen) = cb.listen else { return };
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let l = match tokio::net::TcpListener::bind(&listen).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("callback listener {listen}: {e}");
                    return;
                }
            };
            let ip = l.local_addr().map(|a| a.ip());
            if !cb.insecure && !ip.is_ok_and(ptyro_core::config::safe_address) {
                tracing::warn!(
                    "callback listener {listen}: not loopback or a Tailscale address, so links \
                     would cross the network in clear; set [notifications.callback] \
                     insecure = true if you mean it"
                );
                return;
            }
            tracing::info!("approve/deny links answered on {listen}");
            loop {
                let Ok((mut sock, _)) = l.accept().await else { continue };
                let me = Arc::clone(&me);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (code, body) = me.callback(&req).await;
                    let resp = format!(
                        "HTTP/1.1 {code}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
    }

    async fn callback(self: &Arc<Self>, req: &str) -> (&'static str, String) {
        let line = req.lines().next().unwrap_or("");
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let Some(token) = path.strip_prefix("/a/") else {
            return ("404 Not Found", page("Not found", "Nothing here."));
        };
        match method {
            "GET" => match self.redeem(token, false).map(|(s, d, _)| (s, d)) {
                Ok((session, d)) => {
                    let name =
                        session.parse().ok().and_then(|id| self.session(id)).map(|s| self.info(&s));
                    let what = name
                        .as_ref()
                        .and_then(|i| i.status.as_ref())
                        .and_then(|r| r.detail.clone())
                        .unwrap_or_default();
                    let who = name.map(|i| i.name).unwrap_or_else(|| "the session".into());
                    let verb = if d == Decision::Deny { "Deny" } else { "Approve" };
                    let body = format!(
                        "<p><b>{}</b> wants permission:</p><pre>{}</pre><form method=post><button style=\"font-size:1.4em;padding:.5em 2em\">{verb}</button></form>",
                        html(&who),
                        html(&what)
                    );
                    ("200 OK", page(&format!("{verb}?"), &body))
                }
                Err(e) => ("410 Gone", page("Link not usable", e)),
            },
            "POST" => match self.redeem(token, true) {
                Ok((session, d, since)) => match self.answer(&session, d, since).await {
                    Ok(_) => (
                        "200 OK",
                        page("Done", if d == Decision::Deny { "Denied." } else { "Approved." }),
                    ),
                    Err(e) => ("409 Conflict", page("Not applied", &html(&e.message))),
                },
                Err(e) => ("410 Gone", page("Link not usable", e)),
            },
            _ => ("405 Method Not Allowed", page("No", "GET or POST only.")),
        }
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><meta name=viewport content=\"width=device-width\"><title>{t}</title><body style=\"font-family:system-ui;max-width:40em;margin:2em auto;padding:0 1em\"><h1>{t}</h1>{body}</body>",
        t = html(title)
    )
}

fn html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A curl config-file string.
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

/// Header values must be ASCII-ish for some servers.
fn ascii(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii() && !c.is_control() { c } else { '?' }).collect()
}

pub(crate) fn curl(cfg: &[String]) -> Result<String, String> {
    let mut child = std::process::Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "-m",
            "10",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "--config",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(cfg.join("\n").as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let code = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        Ok(format!("sent (HTTP {code})"))
    } else {
        Err(format!(
            "HTTP {code} {}",
            String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim()
        ))
    }
}

fn json_post(url: &str, v: &serde_json::Value) -> Result<String, String> {
    curl(&[
        format!("url = {}", quote(url)),
        format!("header = {}", quote("Content-Type: application/json")),
        format!("data = {}", quote(&v.to_string())),
    ])
}

fn desktop(note: &Note, custom: Option<&[String]>) -> String {
    let argv: Vec<String> = match custom {
        Some(c) => c
            .iter()
            .map(|a| a.replace("{title}", &note.title).replace("{body}", &note.body))
            .collect(),
        None if cfg!(target_os = "macos") => vec![
            "osascript".into(),
            "-e".into(),
            format!(
                "display notification {} with title {}",
                applescript(&note.body),
                applescript(&note.title)
            ),
        ],
        None => vec![
            "notify-send".into(),
            "--app-name=ptyro".into(),
            note.title.clone(),
            note.body.clone(),
        ],
    };
    let Some((prog, args)) = argv.split_first() else { return "no command".into() };
    match std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reaped, not left a zombie per notification.
            std::thread::spawn(move || child.wait());
            format!("{prog} started")
        }
        Err(e) => format!("{prog}: {e}"),
    }
}

/// How long a sink command or a secret command may take.
const SINK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run a command with optional stdin, collecting stdout, killed after `limit`. Pipes are
/// drained on threads, so a chatty command can't deadlock. `(exited 0, stdout)`.
pub(crate) fn run_bounded(
    mut cmd: std::process::Command,
    input: Option<Vec<u8>>,
    limit: std::time::Duration,
) -> Result<(bool, Vec<u8>), String> {
    use std::io::Read;
    cmd.stdout(std::process::Stdio::piped());
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("took longer than {}s", limit.as_secs()));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    Ok((status.success(), reader.join().unwrap_or_default()))
}

fn applescript(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ── HMAC-SHA256 tokens ─────────────────────────────────────────────────────

fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new().chain_update(&ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(&opad).chain_update(inner).finalize().into()
}

fn b64url(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            s.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    s
}

fn unb64url(s: &str) -> Option<Vec<u8>> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut out = Vec::new();
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `<payload>.<mac>`, both base64url.
pub fn sign(key: &[u8], claims: &str) -> String {
    format!("{}.{}", b64url(claims.as_bytes()), b64url(&hmac(key, claims.as_bytes())))
}

pub fn verify(key: &[u8], token: &str) -> Option<String> {
    let (p, m) = token.split_once('.')?;
    let claims = unb64url(p)?;
    let mac = unb64url(m)?;
    let want = hmac(key, &claims);
    // Constant-time compare.
    (mac.len() == 32 && mac.iter().zip(want.iter()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0)
        .then(|| String::from_utf8(claims).ok())
        .flatten()
}

/// Kept for mutes loaded from the store.
pub type Mutes = HashMap<String, Option<u64>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 test case 2.
        let mac = hmac(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn tokens_verify_and_tamper_fails() {
        let t = sign(b"k", r#"{"s":"x","d":"allow"}"#);
        assert_eq!(verify(b"k", &t).as_deref(), Some(r#"{"s":"x","d":"allow"}"#));
        assert!(verify(b"other", &t).is_none());
        let (p, m) = t.split_once('.').unwrap();
        let forged = format!("{}.{m}", b64url(br#"{"s":"x","d":"always"}"#));
        assert!(verify(b"k", &forged).is_none());
        assert!(verify(b"k", p).is_none());
    }

    #[test]
    fn curl_quoting() {
        assert_eq!(quote("a \"b\"\nc"), "\"a \\\"b\\\"\\nc\"");
        assert_eq!(html("<b>&"), "&lt;b&gt;&amp;");
    }
}
