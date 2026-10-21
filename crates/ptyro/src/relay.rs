//! The hook relay `ptyro hook <adapter> <event>` and the statusline relay (SPEC §8.3, M4.17).
//!
//! These run inside agents, on every hook, so they must never hurt them: no tokio runtime, a
//! blocking socket with a 500 ms budget overall (a Stop waits longer: it may run a verified-done
//! check), exit 0 whatever happens. Stdout carries only what the daemon answers for the agent (a
//! Stop bounce, a PreToolUse denial). Outside ptyro (`PTYRO_SESSION_ID` unset) they do nothing.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use ptyro_protocol::{
    ClientInfo, ClientKind, ClientMsg, Encoding, PROTO, Request, ServerMsg, decode, encode,
};

const BUDGET: Duration = Duration::from_millis(500);
/// Long enough for a verified-done check (the hook's own timeout in the agent caps it too).
const STOP_BUDGET: Duration = Duration::from_secs(900);
/// A held permission prompt (`[remote] approval_wait`, capped by the hook's own 3600 s).
const APPROVAL_BUDGET: Duration = Duration::from_secs(3600);
const MAX_STDIN: u64 = 4 * 1024 * 1024;

fn frame(msg: &ClientMsg) -> Option<Vec<u8>> {
    let body = encode(msg, Encoding::MsgPack).ok()?;
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    Some(out)
}

fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).ok()?;
    let n = u32::from_be_bytes(len) as usize;
    if n > ptyro_protocol::MAX_FRAME {
        return None;
    }
    let mut body = vec![0u8; n];
    s.read_exact(&mut body).ok()?;
    Some(body)
}

/// Send one request and wait (within the budget) for its answer; `None` on any failure.
pub fn request(req: Request, budget: Duration) -> Option<ServerMsg> {
    request_at(std::path::Path::new(&std::env::var_os("PTYRO_SOCKET")?), req, budget)
}

/// [`request`] to a given socket (shell completions use the client's own).
pub fn request_at(socket: &std::path::Path, req: Request, budget: Duration) -> Option<ServerMsg> {
    let start = Instant::now();
    let mut s = UnixStream::connect(socket).ok()?;
    s.set_write_timeout(Some(budget)).ok()?;
    let hello = ClientMsg::Hello {
        proto: PROTO,
        client: ClientInfo {
            kind: ClientKind::Hook,
            version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            outer: None,
            token: None,
            via: None,
        },
        features: vec![],
    };
    s.write_all(&frame(&hello)?).ok()?;
    s.write_all(&frame(&ClientMsg::Request { id: 1, req })?).ok()?;
    loop {
        let left = budget.checked_sub(start.elapsed())?;
        s.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
        let (msg, _): (ServerMsg, _) = decode(&read_frame(&mut s)?).ok()?;
        match msg {
            ServerMsg::Response { .. } | ServerMsg::Reject { .. } => return Some(msg),
            _ => continue,
        }
    }
}

fn stdin_json() -> serde_json::Value {
    let mut buf = Vec::new();
    let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut buf);
    serde_json::from_slice(&buf).unwrap_or(serde_json::Value::Null)
}

/// `ptyro hook <adapter> <event> [args…]`. Codex's `notify` passes its JSON as the last argument
/// and may chain the user's own notify program (`--chain <prog> <args…>`).
pub fn hook(adapter: &str, event: &str, args: &[String]) -> i32 {
    let (payload, chain) = if adapter == "codex" && event == "notify" {
        let json = args.last().cloned().unwrap_or_default();
        let chain: Vec<String> = match args.iter().position(|a| a == "--chain") {
            Some(i) => args.get(i + 1..args.len().saturating_sub(1)).unwrap_or_default().to_vec(),
            None => Vec::new(),
        };
        (
            serde_json::from_str(&json).unwrap_or(serde_json::Value::Null),
            (!chain.is_empty()).then_some((chain, json)),
        )
    } else {
        (stdin_json(), None)
    };
    // The user's notify runs whatever happens to ptyro.
    if let Some((argv, json)) = chain {
        let _ = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .arg(json)
            .stdin(std::process::Stdio::null())
            .spawn();
    }
    let Ok(session) = std::env::var("PTYRO_SESSION_ID") else { return 0 };
    let mut env = std::collections::BTreeMap::new();
    if let Ok(v) = std::env::var("CLAUDE_CODE_MESSAGING_SOCKET") {
        env.insert("CLAUDE_CODE_MESSAGING_SOCKET".to_owned(), v);
    }
    if let Ok(v) = std::env::var("CLAUDE_CODE_MESSAGING_TOKEN") {
        env.insert("CLAUDE_CODE_MESSAGING_TOKEN".to_owned(), v);
    }
    // A Stop may run the session's verified-done check before answering (M7.9).
    // A Claude permission prompt may be held for a phone's answer (M9.5); the daemon bounds it.
    let budget = match event {
        "Stop" => STOP_BUDGET,
        "PermissionRequest" if adapter == "claude" => APPROVAL_BUDGET,
        _ => BUDGET,
    };
    let req = Request::Hook { session, adapter: adapter.into(), event: event.into(), payload, env };
    // The daemon may answer for the agent: a Stop bounce, a PreToolUse denial.
    if let Some(ServerMsg::Response { outcome, .. }) = request(req, budget)
        && let ptyro_protocol::Outcome::Ok { reply: ptyro_protocol::Reply::HookOutput { stdout } } =
            outcome
    {
        println!("{stdout}");
    }
    0
}

/// Claude's `statusLine` command under ptyro: forward the JSON (usage meters), then print the
/// user's own statusline, so theirs keeps working.
pub fn statusline() -> i32 {
    let mut raw = Vec::new();
    let _ = std::io::stdin().take(MAX_STDIN).read_to_end(&mut raw);
    let payload: serde_json::Value =
        serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    if let Ok(session) = std::env::var("PTYRO_SESSION_ID") {
        let _ = request(
            Request::StatusLine { session, payload: payload.clone() },
            Duration::from_millis(300),
        );
    }
    if let Some(cmd) = user_statusline(&payload) {
        let child = std::process::Command::new("sh")
            .args(["-c", &cmd])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Ok(mut c) = child {
            if let Some(mut stdin) = c.stdin.take() {
                let _ = stdin.write_all(&raw);
            }
            if let Ok(out) = c.wait_with_output() {
                let _ = std::io::stdout().write_all(&out.stdout);
            }
        }
    }
    0
}

/// The user's statusline command: `.claude/settings.local.json`, then `.claude/settings.json` in
/// the project, then the account's `settings.json` (never our own relay).
fn user_statusline(payload: &serde_json::Value) -> Option<String> {
    let mut files = Vec::new();
    if let Some(dir) = payload["workspace"]["project_dir"]
        .as_str()
        .or_else(|| payload["workspace"]["current_dir"].as_str())
        .or_else(|| payload["cwd"].as_str())
    {
        let d = std::path::Path::new(dir).join(".claude");
        files.push(d.join("settings.local.json"));
        files.push(d.join("settings.json"));
    }
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ptyro_core::config::expand_home("~/.claude"));
    files.push(home.join("settings.json"));
    files.iter().find_map(|f| {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
        let cmd = v["statusLine"]["command"].as_str()?.to_owned();
        (!cmd.contains("statusline-relay")).then_some(cmd)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn relay_is_a_no_op_outside_ptyro() {
        // Codex's notify passes JSON as an argument (no stdin read); outside ptyro: exit 0.
        if std::env::var_os("PTYRO_SESSION_ID").is_none() {
            assert_eq!(super::hook("codex", "notify", &["{}".into()]), 0);
        }
    }
}
