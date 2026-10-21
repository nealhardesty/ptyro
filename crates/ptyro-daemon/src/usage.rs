//! Usage and quota meters per profile (M4.17, R-USE-01).
//!
//! - **Claude**: `ptyro statusline-relay` is Claude's `statusLine` command. It forwards the
//!   statusline JSON here (`rate_limits.five_hour` / `.seven_day`: `used_percentage`,
//!   `resets_at` in epoch seconds; `cost.total_cost_usd`; `context_window.used_percentage`) and
//!   prints the user's own statusline.
//! - **Codex** (opt-in, `[agents.codex] usage = true`): a side `codex app-server` per Codex home
//!   answers `account/rateLimits/read` and pushes `account/rateLimits/updated`
//!   (`primary`/`secondary`: `usedPercent`, `resetsAt`, `windowDurationMins`).
//! - A `StopFailure` with `rate_limit` marks the profile rate-limited until the window resets.

use std::sync::Arc;
use std::time::Duration;

use ptyro_core::status::{Usage, UsageWindow};
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{DomainEvent, Reply};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::daemon::Daemon;
use crate::session::Session;

/// Claude statusline JSON → usage.
pub fn claude_usage(profile: &str, v: &Value) -> Usage {
    let mut windows = Vec::new();
    for (key, name) in [("five_hour", "5h"), ("seven_day", "7d")] {
        let w = &v["rate_limits"][key];
        if let Some(p) = w["used_percentage"].as_f64() {
            windows.push(UsageWindow {
                name: name.into(),
                used_percent: p.clamp(0.0, 100.0).round() as u8,
                resets_at_ms: w["resets_at"].as_u64().map(|s| s * 1000),
            });
        }
    }
    let limited_until_ms =
        windows.iter().filter(|w| w.used_percent >= 100).filter_map(|w| w.resets_at_ms).max();
    Usage {
        profile: profile.into(),
        agent: "claude".into(),
        windows,
        cost_millis: v["cost"]["total_cost_usd"]
            .as_f64()
            .map(|c| (c.max(0.0) * 1000.0).round() as u64),
        context_percent: v["context_window"]["used_percentage"]
            .as_f64()
            .map(|p| p.clamp(0.0, 100.0).round() as u8),
        limited_until_ms,
        at_ms: now_ms(),
    }
}

/// Codex `RateLimitSnapshot` → usage windows.
pub fn codex_windows(snap: &Value) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    for key in ["primary", "secondary"] {
        let w = &snap[key];
        let Some(p) = w["usedPercent"].as_u64() else { continue };
        let name = match w["windowDurationMins"].as_u64() {
            Some(m) if m % 1440 == 0 => format!("{}d", m / 1440),
            Some(m) if m % 60 == 0 => format!("{}h", m / 60),
            Some(m) => format!("{m}m"),
            None => key.to_owned(),
        };
        out.push(UsageWindow {
            name,
            used_percent: p.min(100) as u8,
            resets_at_ms: w["resetsAt"].as_u64().map(|s| s * 1000),
        });
    }
    out
}

impl Daemon {
    pub(crate) fn put_usage(&self, mut u: Usage) {
        let key = (u.profile.clone(), u.agent.clone());
        let changed = {
            let mut map = self.usage.lock().expect("usage");
            // Statusline JSON doesn't carry the rate-limit flag: keep it until it resets.
            if let Some(old) = map.get(&key)
                && u.limited_until_ms.is_none()
                && old.limited_until_ms.is_some_and(|t| t > now_ms())
            {
                u.limited_until_ms = old.limited_until_ms;
            }
            let same = map.get(&key).is_some_and(|o| {
                o.windows == u.windows
                    && o.limited_until_ms == u.limited_until_ms
                    && o.context_percent == u.context_percent
            });
            map.insert(key, u.clone());
            !same
        };
        if changed {
            let _ = self.events.send(DomainEvent::UsageUpdated { usage: u });
        }
    }

    pub(crate) fn statusline(&self, session: &str, payload: &Value) -> Result<Reply, Error> {
        let id = session.parse().map_err(|_| Error::invalid("bad session id"))?;
        let s = self.session(id).ok_or_else(|| Error::not_found(session))?;
        let profile = s.meta.lock().expect("meta").place.profile.clone();
        self.put_usage(claude_usage(&profile, payload));
        Ok(Reply::Done)
    }

    /// A rate-limit failure: the account is limited until its fullest window resets.
    pub(crate) fn rate_limited(&self, s: &Session) {
        let (profile, adapter) = {
            let m = s.meta.lock().expect("meta");
            (m.place.profile.clone(), m.adapter.clone().unwrap_or_else(|| "agent".into()))
        };
        let mut u = self
            .usage
            .lock()
            .expect("usage")
            .get(&(profile.clone(), adapter.clone()))
            .cloned()
            .unwrap_or(Usage {
                profile,
                agent: adapter,
                windows: Vec::new(),
                cost_millis: None,
                context_percent: None,
                limited_until_ms: None,
                at_ms: now_ms(),
            });
        let reset =
            u.windows.iter().filter_map(|w| w.resets_at_ms).max().unwrap_or(now_ms() + 3_600_000);
        u.limited_until_ms = Some(reset);
        u.at_ms = now_ms();
        self.put_usage(u);
    }

    pub(crate) fn usage_list(&self) -> Reply {
        let mut usage: Vec<Usage> = self.usage.lock().expect("usage").values().cloned().collect();
        usage.sort_by(|a, b| (&a.profile, &a.agent).cmp(&(&b.profile, &b.agent)));
        Reply::Usage { usage }
    }

    /// Codex rate limits for a session's profile, through a side app-server (opt-in). One per
    /// profile; it lives while that profile has Codex sessions.
    pub(crate) fn start_codex_usage(self: &Arc<Self>, s: &Arc<Session>) {
        let profile = s.meta.lock().expect("meta").place.profile.clone();
        let agent =
            s.meta.lock().expect("meta").place.agent.clone().unwrap_or_else(|| "codex".into());
        if ptyro_agents::def(&self.config(), &agent).usage != Some(true) {
            return;
        }
        if !self.codex_usage.lock().expect("codex_usage").insert(profile.clone()) {
            return;
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = me.codex_usage_loop(&profile, &agent).await {
                tracing::info!(profile, "codex usage meter stopped: {e}");
            }
            me.codex_usage.lock().expect("codex_usage").remove(&profile);
        });
    }

    async fn codex_usage_loop(self: &Arc<Self>, profile: &str, agent: &str) -> Result<(), String> {
        let plan = self.profile_plan(profile).await.map_err(|e| e.message)?;
        let env = plan.apply(self.base_env.iter().cloned());
        let argv =
            ptyro_agents::command(&self.config(), agent).unwrap_or_else(|| vec!["codex".into()]);
        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg("app-server")
            .env_clear()
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
        let send = |v: Value| format!("{v}\n");
        stdin
            .write_all(send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "ptyro", "version": env!("CARGO_PKG_VERSION")}}})).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin
            .write_all(
                send(serde_json::json!({"jsonrpc": "2.0", "method": "initialized"})).as_bytes(),
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut next_id = 2u64;
        let mut poll = tokio::time::interval(Duration::from_secs(300));
        loop {
            // Stop with the profile's last Codex session.
            let any = self.sessions().iter().any(|s| {
                s.is_live()
                    && s.adapter().as_deref() == Some("codex")
                    && s.meta.lock().expect("meta").place.profile == profile
            });
            if !any {
                return Ok(());
            }
            tokio::select! {
                _ = poll.tick() => {
                    let req = serde_json::json!({"jsonrpc": "2.0", "id": next_id, "method": "account/rateLimits/read", "params": {}});
                    next_id += 1;
                    stdin.write_all(send(req).as_bytes()).await.map_err(|e| e.to_string())?;
                }
                line = lines.next_line() => {
                    let Some(line) = line.map_err(|e| e.to_string())? else { return Err("app-server exited".into()) };
                    let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                    let snap = if v["method"] == "account/rateLimits/updated" {
                        v["params"]["rateLimits"].clone()
                    } else if v.get("result").is_some_and(|r| r.get("rateLimits").is_some()) {
                        v["result"]["rateLimits"].clone()
                    } else {
                        if let Some(e) = v.get("error") {
                            tracing::debug!(profile, "codex rate limits: {e}");
                        }
                        continue;
                    };
                    let windows = codex_windows(&snap);
                    let limited = snap["rateLimitReachedType"].as_str().is_some().then(|| windows.iter().filter_map(|w| w.resets_at_ms).max()).flatten();
                    self.put_usage(Usage { profile: profile.into(), agent: "codex".into(), windows, cost_millis: None, context_percent: None, limited_until_ms: limited, at_ms: now_ms() });
                }
                () = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_statusline_and_codex_snapshot() {
        let v = serde_json::json!({
            "rate_limits": { "five_hour": { "used_percentage": 42.4, "resets_at": 1_800_000_000u64 }, "seven_day": { "used_percentage": 100.0, "resets_at": 1_800_100_000u64 } },
            "cost": { "total_cost_usd": 1.25 },
            "context_window": { "used_percentage": 61.0 }
        });
        let u = claude_usage("work", &v);
        assert_eq!(u.windows.len(), 2);
        assert_eq!((u.windows[0].name.as_str(), u.windows[0].used_percent), ("5h", 42));
        assert_eq!(u.limited_until_ms, Some(1_800_100_000_000), "the full window decides");
        assert_eq!((u.cost_millis, u.context_percent), (Some(1250), Some(61)));
        let snap = serde_json::json!({ "primary": { "usedPercent": 12, "windowDurationMins": 300, "resetsAt": 10 }, "secondary": { "usedPercent": 3, "windowDurationMins": 10080 } });
        let w = codex_windows(&snap);
        assert_eq!(
            (w[0].name.as_str(), w[0].used_percent, w[0].resets_at_ms),
            ("5h", 12, Some(10_000))
        );
        assert_eq!(w[1].name, "7d");
    }
}
