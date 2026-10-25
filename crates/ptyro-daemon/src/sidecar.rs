//! Side channels the daemon holds next to a session (SPEC §8.6): OpenCode's embedded server.
//! ptyro gave it a port and a password at launch; here it follows `GET /event` (server-sent
//! events), turns events into signals, and answers permission prompts through the API.
//!
//! The server is on 127.0.0.1, so a minimal HTTP/1.1 client over a plain TCP stream is enough.

use ptyro_core::sync::MutexExt;
use std::sync::Arc;
use std::time::Duration;

use ptyro_agents::{Decision, Sidecar};
use ptyro_core::status::{Confidence, Signal, SignalSource, Sourced};
use ptyro_core::{Error, ErrorCode};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::daemon::Daemon;
use crate::session::Session;

const USER: &str = "opencode";

pub(crate) fn basic(password: &str) -> String {
    let raw = format!("{USER}:{password}");
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = raw.as_bytes();
    let mut s = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            s.push(if i <= chunk.len() {
                T[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    s
}

/// Pending OpenCode permission ids per session: (opencode session id, permission id).
pub type Pending = std::collections::HashMap<ptyro_core::SessionId, (String, String)>;

impl Daemon {
    pub(crate) fn start_sidecar(self: &Arc<Self>, s: &Arc<Session>, sc: Sidecar) {
        let Sidecar::OpenCode { port, password } = sc;
        self.sidecars.locked().insert(s.id, (port, password.clone()));
        let (me, s) = (Arc::clone(self), Arc::clone(s));
        tokio::spawn(async move {
            // The server comes up a moment after the TUI; reconnect while the session lives.
            // A restart reuses the session id with a new server: this loop belongs to the old one
            // once the entry says otherwise, and must not tidy away the new one's.
            let mine = |me: &Daemon| {
                me.sidecars.locked().get(&s.id).is_some_and(|(p, pw)| *p == port && *pw == password)
            };
            while s.is_live() && mine(&me) {
                if let Err(e) = me.follow_events(&s, port, &password).await {
                    tracing::debug!(session = %s.id, "opencode events: {e}");
                }
                if mine(&me) {
                    me.sidecar_live.locked().remove(&s.id);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if mine(&me) {
                me.sidecars.locked().remove(&s.id);
            }
        });
    }

    async fn follow_events(
        self: &Arc<Self>,
        s: &Arc<Session>,
        port: u16,
        password: &str,
    ) -> std::io::Result<()> {
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await?;
        let req = format!(
            "GET /event HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\n\r\n",
            basic(password)
        );
        sock.write_all(req.as_bytes()).await?;
        let mut r = BufReader::new(sock);
        let mut status = String::new();
        r.read_line(&mut status).await?;
        if !status.contains(" 200") {
            return Err(std::io::Error::other(format!("server said {}", status.trim())));
        }
        self.sidecar_live.locked().insert(s.id);
        // Headers, then events; chunked framing lines are skipped since they never start with
        // `data:`.
        let mut data = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let l = line.trim_end_matches(['\r', '\n']);
            if let Some(d) = l.strip_prefix("data:") {
                data.push_str(d.trim_start());
            } else if l.is_empty() && !data.is_empty() {
                if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&data) {
                    self.opencode_event(s, &ev).await;
                }
                data.clear();
            }
        }
    }

    async fn opencode_event(self: &Arc<Self>, s: &Arc<Session>, ev: &serde_json::Value) {
        let props = &ev["properties"];
        match ev["type"].as_str() {
            Some("permission.asked" | "permission.updated") => {
                if let (Some(sid), Some(pid)) = (props["sessionID"].as_str(), props["id"].as_str())
                {
                    self.oc_pending.locked().insert(s.id, (sid.to_owned(), pid.to_owned()));
                }
            }
            Some("permission.replied") => {
                self.oc_pending.locked().remove(&s.id);
            }
            _ => {}
        }
        let signals: Vec<Sourced> = ptyro_agents::others::opencode_event(ev)
            .into_iter()
            .map(|signal: Signal| Sourced {
                signal,
                source: SignalSource::Sidecar,
                confidence: Confidence::High,
            })
            .collect();
        if !signals.is_empty() {
            let _ = self.feed_tx.send(crate::session::Feed::Signals(s.id, signals));
        }
    }

    /// `POST /session/:id/permissions/:permissionID` with `once`, `always` or `reject`.
    pub(crate) async fn opencode_answer(
        self: &Arc<Self>,
        s: &Arc<Session>,
        d: Decision,
    ) -> Result<(), Error> {
        let (port, password) = self.sidecars.locked().get(&s.id).cloned().ok_or_else(|| {
            Error::new(ErrorCode::Unsupported, "OpenCode's server isn't connected")
        })?;
        let (sid, pid) = self
            .oc_pending
            .locked()
            .get(&s.id)
            .cloned()
            .ok_or_else(|| Error::conflict("no OpenCode permission is pending"))?;
        let response = match d {
            Decision::Allow => "once",
            Decision::Always => "always",
            Decision::Deny => "reject",
        };
        let body = serde_json::json!({ "response": response }).to_string();
        let req = format!(
            "POST /session/{sid}/permissions/{pid} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Basic {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            basic(&password),
            body.len()
        );
        let mut sock = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        sock.write_all(req.as_bytes()).await.map_err(|e| Error::internal(e.to_string()))?;
        let mut resp = vec![0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut resp))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        let head = String::from_utf8_lossy(&resp[..n]);
        if head.starts_with("HTTP/1.1 2") || head.starts_with("HTTP/1.0 2") {
            Ok(())
        } else {
            Err(Error::internal(format!(
                "OpenCode said {}",
                head.lines().next().unwrap_or("nothing")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn basic_auth() {
        assert_eq!(super::basic("pw"), "b3BlbmNvZGU6cHc=");
    }
}
