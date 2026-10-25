//! `ptyro serve` against a page or a client that misbehaves: cross-site requests, a body that
//! never arrives, no daemon to talk to. Plain HTTP on loopback, a daemon of its own.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct Serve {
    child: Child,
    port: u16,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(d: &TempDaemon, env: &[(&str, &str)]) -> Serve {
    // Held until the instant before `serve` wants it.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut cmd = d.cmd();
    cmd.args(["serve", "--insecure", "--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let s = Serve { child: cmd.spawn().unwrap(), port };
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "ptyro serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    s
}

/// A raw request → (status, the whole response as text).
fn http(port: u16, req: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    (text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0), text)
}

fn pair(port: u16, origin: Option<&str>) -> (u16, String) {
    let body = r#"{"code":"ptp_junk"}"#;
    let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
    http(
        port,
        &format!(
            "POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

/// A page on another site can't use the server (a WebSocket and a simple POST need no
/// pre-flight), and what it sends doesn't count against the visitor's own logins.
#[test]
fn a_page_from_another_site_is_refused_and_locks_nobody_out() {
    let d = TempDaemon::start(BIN).unwrap();
    let s = serve(&d, &[]);
    let port = s.port;
    for _ in 0..15 {
        let (code, _) = pair(port, Some("https://evil.example"));
        assert_eq!(code, 403);
    }
    let (code, text) = http(
        port,
        &format!(
            "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.example\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ),
    );
    assert_eq!(code, 403, "{text}");
    // Not a browser (no Origin), and the server's own page: both reach the pairing check, which
    // says the code is wrong, not that there were too many tries.
    assert_eq!(pair(port, None).0, 401);
    assert_eq!(pair(port, Some(&format!("http://127.0.0.1:{port}"))).0, 401);
    // Answers say nothing may keep them.
    let (_, text) = pair(port, None);
    assert!(text.to_ascii_lowercase().contains("cache-control: no-store"), "{text}");
}

/// A request that promises a body and never sends it gives up its place.
#[test]
fn a_body_that_never_arrives_is_given_up_on() {
    let d = TempDaemon::start(BIN).unwrap();
    let s = serve(&d, &[("PTYRO_TEST_BODY_TIMEOUT_MS", "400")]);
    let started = Instant::now();
    let (code, text) = http(
        s.port,
        &format!(
            "POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{\"code\":",
            s.port
        ),
    );
    assert_eq!(code, 408, "{text}");
    assert!(started.elapsed() < Duration::from_secs(8));
}

/// With no daemon, a request from the network doesn't start one, and what it is told doesn't
/// name this machine's files.
#[test]
fn nobody_on_the_network_can_start_the_daemon() {
    let mut d = TempDaemon::start(BIN).unwrap();
    // (`cmd()` says never to start a daemon: take that away, so the server's own choice shows.)
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut cmd = d.cmd();
    cmd.env_remove("PTYRO_NO_AUTOSPAWN")
        .args(["serve", "--insecure", "--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let s = Serve { child: cmd.spawn().unwrap(), port };
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    }
    d.stop();
    // (A stopped daemon may leave its socket file behind, as a crash does: take it away, so
    // that what shows next is whether anything makes a new one.)
    let _ = std::fs::remove_file(&d.paths.socket);
    let (code, text) = pair(port, None);
    assert_eq!(code, 502, "{text}");
    assert!(text.contains("the ptyro daemon isn't reachable"), "{text}");
    assert!(!text.contains(&d.paths.socket.display().to_string()), "{text}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!d.paths.socket.exists(), "a request from the network started a daemon");
    drop(s);
}

/// A device with full scope → its token.
fn full_token(d: &TempDaemon, port: u16) -> String {
    let out = d.cmd().args(["pair", "laptop", "--scope", "full", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    let body = serde_json::json!({ "code": code }).to_string();
    let (status, text) = http(
        port,
        &format!(
            "POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value =
        serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    v["token"].as_str().unwrap().to_owned()
}

fn host(port: u16, token: &str, body: &serde_json::Value) -> serde_json::Value {
    let body = body.to_string();
    let (_, text) = http(
        port,
        &format!(
            "POST /api/host HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap_or("null")).unwrap_or_default()
}

/// The config editor saves what it opened on: if the file is something else by then (the
/// settings page or a command changed it), saving says so instead of undoing that change.
#[test]
fn saving_the_config_refuses_to_undo_a_change_made_since() {
    let d = TempDaemon::start_with(BIN, &[], Some("[ui]\nmouse = false\n")).unwrap();
    let s = serve(&d, &[]);
    let token = full_token(&d, s.port);
    let got = host(s.port, &token, &serde_json::json!({ "op": "config.get" }));
    let opened = got["reply"]["text"].as_str().unwrap().to_owned();
    assert!(opened.contains("mouse = false"), "{got}");
    // Someone else changes the file.
    let out = d.cmd().args(["config", "set", "ui.clock", "off"]).output().unwrap();
    assert!(out.status.success());
    let stale = format!("{opened}\n# my edit\n");
    let r = host(
        s.port,
        &token,
        &serde_json::json!({ "op": "config.save", "text": stale, "base": opened }),
    );
    assert_eq!(r["t"], "err", "{r}");
    assert_eq!(r["error"]["code"], "conflict", "{r}");
    let file = std::fs::read_to_string(d.paths.config.join("config.toml")).unwrap();
    assert!(file.contains("clock = false") && !file.contains("# my edit"), "{file}");
    // Opened again, it saves.
    let fresh = host(s.port, &token, &serde_json::json!({ "op": "config.get" }));
    let now = fresh["reply"]["text"].as_str().unwrap().to_owned();
    let r = host(
        s.port,
        &token,
        &serde_json::json!({ "op": "config.save", "text": format!("{now}# my edit\n"), "base": now }),
    );
    assert_eq!(r["t"], "ok", "{r}");
    assert!(
        std::fs::read_to_string(d.paths.config.join("config.toml")).unwrap().contains("# my edit")
    );
    // No base (an older client): it saves as it always did.
    let r = host(
        s.port,
        &token,
        &serde_json::json!({ "op": "config.save", "text": "[ui]\nmouse = true\n" }),
    );
    assert_eq!(r["t"], "ok", "{r}");
    // The daemon's log tail comes back in lines, from the end.
    let r = host(s.port, &token, &serde_json::json!({ "op": "daemon.logs", "lines": 5 }));
    assert_eq!(r["t"], "ok", "{r}");
    assert!(r["reply"]["text"].as_str().unwrap().lines().count() <= 5, "{r}");
}
