//! M9 end-to-end tests: remote. The Done-when (a phone sees an agent blocked on a permission
//! prompt, reads the context, approves, and the agent continues — and nothing unauthenticated
//! reaches the daemon), device scopes and revocation, the CLI and TUI over ssh (`--remote`,
//! with a stand-in ssh), Web Push delivered and decrypted end to end, and the daemon-managed
//! `ptyro serve`. The "phone" is a WebSocket client speaking the web app's JSON protocol.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(extra: &str, env: &[(&str, &str)]) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-m9-").tempdir().unwrap();
        let hs = home.path().canonicalize().unwrap().to_string_lossy().into_owned();
        let mut e = vec![("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        e.extend_from_slice(env);
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n{extra}"
        );
        let d = TempDaemon::start_with(BIN, &e, Some(&config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        self.home.path().canonicalize().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("ptyro {args:?}: {e}\n{out}"))
    }

    /// `ptyro pair` → (device id, the link's one-time code).
    fn pair_code(&self, name: &str, scope: &str) -> (String, String) {
        let v = self.json(&["pair", name, "--scope", scope, "--json"]);
        let url = v["url"].as_str().unwrap();
        let code = url.split("#pair=").nth(1).unwrap().to_owned();
        assert!(code.starts_with("ptp_"), "{url}");
        (v["device"]["id"].as_str().unwrap().to_owned(), code)
    }

    /// `ptyro pair`, then the code traded for the token as the web app does → (id, token).
    fn pair(&self, serve: &Serve, name: &str, scope: &str) -> (String, String) {
        let (id, code) = self.pair_code(name, scope);
        let (status, body) = claim(serve.port, &code);
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["device"]["id"], id.as_str());
        (id, v["token"].as_str().unwrap().to_owned())
    }

    fn serve(&self) -> Serve {
        let port = free_port();
        let child = self
            .d
            .cmd()
            .args(["serve", "--listen", &format!("127.0.0.1:{port}")])
            .env("HOME", self.home())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let s = Serve { child, port };
        wait_http(port);
        s
    }

    fn wait_screen(&self, s: &str, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let t = self.ok(&["read", s]);
            if t.contains(needle) {
                return t;
            }
            assert!(Instant::now() < deadline, "{s} never showed {needle:?}:\n{t}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A raw HTTP/1.1 request; returns (status, headers + body as text).
fn http(port: u16, req: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (code, text)
}

/// `POST /api/pair {"code"}` → (status, response).
fn claim(port: u16, code: &str) -> (u16, String) {
    let body = json!({ "code": code }).to_string();
    http(
        port,
        &format!(
            "POST /api/pair HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn wait_http(port: u16) {
    let deadline = Instant::now() + WAIT;
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

type Ws = WebSocketStream<TcpStream>;

/// The phone: a WebSocket to `ptyro serve`, speaking JSON like the web app.
struct Phone {
    ws: Ws,
    next: u32,
    /// Everything that wasn't a response.
    pushed: Vec<Value>,
}

impl Phone {
    async fn open(port: u16) -> Ws {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (ws, _) = tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/ws"), tcp)
            .await
            .unwrap();
        ws
    }

    /// Hello → the first answer (`welcome` or `reject`).
    async fn hello(port: u16, client: Value) -> (Option<Self>, Value) {
        let mut ws = Self::open(port).await;
        let hello = json!({ "t": "hello", "proto": { "major": 1, "minor": 8 }, "client": client, "features": [] });
        ws.send(Message::text(hello.to_string())).await.unwrap();
        let first = next_json(&mut ws).await.expect("an answer to hello");
        if first["t"] == "welcome" {
            (Some(Self { ws, next: 1, pushed: Vec::new() }), first)
        } else {
            (None, first)
        }
    }

    async fn signed_in(port: u16, token: &str) -> Self {
        let (p, first) =
            Self::hello(port, json!({ "kind": "web", "version": "web", "pid": 0, "token": token }))
                .await;
        p.unwrap_or_else(|| panic!("hello rejected: {first}"))
    }

    async fn send(&mut self, v: Value) {
        self.ws.send(Message::text(v.to_string())).await.unwrap();
    }

    /// A request → its outcome.
    async fn request(&mut self, req: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "t": "request", "id": id, "req": req })).await;
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let m = tokio::time::timeout(left, next_json(&mut self.ws))
                .await
                .expect("timed out")
                .expect("closed");
            if m["t"] == "response" && m["id"] == id {
                return m["outcome"].clone();
            }
            self.pushed.push(m);
        }
    }

    async fn ok(&mut self, req: Value) -> Value {
        let o = self.request(req.clone()).await;
        assert_eq!(o["t"], "ok", "{req} → {o}");
        o["reply"].clone()
    }

    /// Wait for a pushed message matching `f`.
    async fn until(&mut self, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.pushed.iter().position(&f) {
            return self.pushed.remove(i);
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let m = tokio::time::timeout(left, next_json(&mut self.ws))
                .await
                .unwrap_or_else(|_| panic!("never: {what}"))
                .unwrap_or_else(|| panic!("closed waiting for {what}"));
            if f(&m) {
                return m;
            }
        }
    }
}

async fn next_json(ws: &mut Ws) -> Option<Value> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => return serde_json::from_str(t.as_str()).ok(),
            Ok(Message::Binary(b)) => return serde_json::from_slice(&b).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

/// The text of every row in a screen frame.
fn frame_text(m: &Value) -> String {
    m["frame"]["delta"]["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    r["runs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x["text"].as_str().unwrap_or(""))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn session<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {list}"))
}

/// The Done-when: from a "phone", see an agent blocked on a permission prompt, read the
/// context, approve it (an exact decision through Claude's held hook), and watch it continue.
/// And nothing unauthenticated reaches the daemon.
#[tokio::test]
async fn done_when_a_phone_approves_a_blocked_agent() {
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, token) = w.pair(&serve, "phone", "control");
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    w.ok(&["send", "a1", "ask rm -rf build", "--wait-ready"]);

    let mut phone = Phone::signed_in(serve.port, &token).await;
    // It's blocked, the prompt is held for an exact answer, and the context is readable.
    let deadline = Instant::now() + WAIT;
    let s = loop {
        let list = phone.ok(json!({ "t": "list" })).await;
        let s = session(&list, "a1").clone();
        if s["status"]["status"] == "blocked" && s["held"] == true {
            break s;
        }
        assert!(Instant::now() < deadline, "a1 never blocked + held: {s:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(s["status"]["detail"].as_str().unwrap().contains("rm -rf build"), "{s:#}");
    let id = s["id"].as_str().unwrap().to_owned();
    phone.send(json!({ "t": "subscribe", "session": id, "size": null, "control": false })).await;
    let frame =
        phone.until("a keyframe", |m| m["t"] == "screen" && m["frame"]["keyframe"] == true).await;
    assert!(frame_text(&frame).contains("Do you want to proceed?"), "{}", frame_text(&frame));
    // The phone's frames come at the session's own size: it never resizes it.
    assert_eq!(frame["frame"]["delta"]["size"]["cols"], 120);

    phone.ok(json!({ "t": "answer", "addr": id, "decision": "allow" })).await;
    w.wait_screen("a1", "answer: yes");
    let deadline = Instant::now() + WAIT;
    while w.json(&["status", "a1", "--json"])["session"]["status"]["status"] != "idle" {
        assert!(Instant::now() < deadline, "a1 never went on");
        std::thread::sleep(Duration::from_millis(100));
    }
    // The answer is in a1's event log, with the device.
    let events = w.ok(&["events", "-s", "a1", "--json", "-n", "200"]);
    assert!(events.lines().any(|l| l.contains("\"remote\"") && l.contains("phone")), "{events}");

    // Unauthenticated: no token, a wrong token, or claiming to be a local client.
    for client in [
        json!({ "kind": "web", "version": "x", "pid": 0 }),
        json!({ "kind": "web", "version": "x", "pid": 0, "token": "ptd_not-a-token" }),
        json!({ "kind": "tui", "version": "x", "pid": 1 }),
    ] {
        let (p, first) = Phone::hello(serve.port, client.clone()).await;
        assert!(p.is_none() && first["t"] == "reject", "{client} → {first}");
        assert!(first["reason"].as_str().unwrap().contains("unauthorized"), "{first}");
    }
    let (code, _) = http(
        serve.port,
        "POST /api/request HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"t\":\"list\"}",
    );
    assert_eq!(code, 401);
    let (code, text) =
        http(serve.port, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert_eq!(code, 200);
    // A DNS-rebinding page's requests carry its own name.
    let (code, _) =
        http(serve.port, "GET / HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
    assert_eq!(code, 421);
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("content-security-policy: default-src 'none'")
            && lower.contains("x-frame-options: deny"),
        "{text}"
    );
    // A token works for one request too (what notification actions use).
    let body = r#"{"t":"list"}"#;
    let (code, text) = http(
        serve.port,
        &format!(
            "POST /api/request HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(code == 200 && text.contains("\"t\":\"ok\"") && text.contains("\"a1\""), "{text}");
}

/// Scopes hold, revocation cuts a device off at once, pairing stays local, repeated failures
/// lock an address out.
#[tokio::test]
async fn scopes_revocation_and_lockout() {
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, watch) = w.pair(&serve, "watcher", "read");
    let (ctl_id, ctl) = w.pair(&serve, "tablet", "control");
    let (_, full) = w.pair(&serve, "laptop", "full");
    w.ok(&["new", "-d", "-n", "sh1", "--", "sh", "-c", "echo hello; cat"]);
    w.wait_screen("sh1", "hello");

    let mut ro = Phone::signed_in(serve.port, &watch).await;
    ro.ok(json!({ "t": "list" })).await;
    let o = ro.request(json!({ "t": "send_text", "addr": "sh1", "text": "typed", "enter": true, "wait_ready_ms": null })).await;
    assert_eq!(o["t"], "err");
    assert!(o["error"]["message"].as_str().unwrap().contains("scope read"), "{o}");
    let id =
        session(&ro.ok(json!({ "t": "list" })).await, "sh1")["id"].as_str().unwrap().to_owned();
    ro.send(json!({ "t": "input", "session": id, "event": { "t": "paste", "text": "sneaky\n" } }))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!w.ok(&["read", "sh1"]).contains("sneaky"), "read scope must not type");

    let mut tab = Phone::signed_in(serve.port, &ctl).await;
    tab.ok(json!({ "t": "send_text", "addr": "sh1", "text": "from the tablet", "enter": true, "wait_ready_ms": null })).await;
    w.wait_screen("sh1", "from the tablet");
    // Control can't run arbitrary commands, and nobody remote pairs devices.
    let o = tab.request(json!({ "t": "spawn", "spec": { "argv": ["sh"], "cwd": "/" } })).await;
    assert!(
        o["t"] == "err" && o["error"]["message"].as_str().is_some_and(|m| m.contains("needs full")),
        "{o}"
    );
    let mut lap = Phone::signed_in(serve.port, &full).await;
    let o = lap.request(json!({ "t": "device_pair", "name": "x", "scope": "full" })).await;
    assert!(
        o["t"] == "err" && o["error"]["message"].as_str().unwrap().contains("only on the machine"),
        "{o}"
    );

    // Revoke: the live connection gets a goodbye, and the token stops working.
    w.ok(&["devices", "revoke", &ctl_id]);
    let bye = tab.until("goodbye", |m| m["t"] == "goodbye").await;
    assert!(bye["reason"].as_str().unwrap().contains("revoked"), "{bye}");
    let (p, _) =
        Phone::hello(serve.port, json!({ "kind": "web", "version": "x", "pid": 0, "token": ctl }))
            .await;
    assert!(p.is_none());
    let list = w.ok(&["devices"]);
    assert!(
        list.contains("watcher") && list.contains("laptop") && !list.contains("tablet"),
        "{list}"
    );

    // Ten failed logins from one address lock it out (the revoked attempt above was one).
    for _ in 0..9 {
        let _ = Phone::hello(
            serve.port,
            json!({ "kind": "web", "version": "x", "pid": 0, "token": "ptd_bad" }),
        )
        .await;
    }
    let (code, _) = http(
        serve.port,
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(code, 429);
}

/// What `ptyro pair` prints lands in scrollback and recordings, so it is a code, not the
/// token: claimed once, within minutes (here shortened), then worthless. Unclaimed, the device
/// can't sign in; the default scope is `approve`.
#[tokio::test]
async fn pairing_codes_are_single_use_and_expire() {
    let w = World::new("", &[("PTYRO_TEST_PAIRING_TTL_MS", "3000")]);
    let serve = w.serve();
    let v = w.json(&["pair", "phone", "--json"]);
    assert_eq!(v["device"]["scope"], "approve");
    assert_eq!(v["device"]["pending"], true);
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    // The code is no token.
    let (p, first) =
        Phone::hello(serve.port, json!({ "kind": "web", "version": "x", "pid": 0, "token": code }))
            .await;
    assert!(p.is_none() && first["t"] == "reject", "{first}");
    let devices = w.json(&["devices", "ls", "--json"]);
    assert_eq!(devices[0]["pending"], true, "{devices}");
    // Claimed once: the token works, the code doesn't any more.
    let (status, body) = claim(serve.port, &code);
    assert_eq!(status, 200, "{body}");
    let reply: Value = serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let token = reply["token"].as_str().unwrap();
    assert!(token.starts_with("ptd_") && reply["device"]["pending"] == false, "{reply}");
    let mut phone = Phone::signed_in(serve.port, token).await;
    phone.ok(json!({ "t": "list" })).await;
    let (status, _) = claim(serve.port, &code);
    assert_eq!(status, 401, "a used code");
    let via = w.json(&["devices", "ls", "--json"])[0]["last_via"].clone();
    assert!(via.as_str().is_some_and(|v| v.starts_with("ws:127.0.0.1")), "{via}");
    // Unclaimed until it expires: then it is gone, and so is the device it named.
    let (_, late) = w.pair_code("late", "read");
    std::thread::sleep(Duration::from_millis(3500));
    let (status, _) = claim(serve.port, &late);
    assert_eq!(status, 401, "an expired code");
    let devices = w.json(&["devices", "ls", "--json"]);
    assert_eq!(devices.as_array().unwrap().len(), 1, "{devices}");
    // The human-readable form prints the link and waits for the claim; nothing it prints is a
    // token.
    let out = w.cli(&["pair", "printed"]);
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(
        !out.status.success() && text.contains("#pair=ptp_") && text.contains("expired"),
        "{text}"
    );
    assert!(!text.contains("ptd_"), "{text}");
}

/// A stand-in for ssh: checks its arguments, then runs the remote command against the "remote"
/// daemon's home.
fn fake_ssh(dir: &std::path::Path, remote_home: &std::path::Path) -> PathBuf {
    let path = dir.join("fake-ssh");
    let log = dir.join("ssh-args");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nfor last; do :; done\nexport PTYRO_HOME='{}'\nunset PTYRO_SOCKET\nexec sh -c \"$last\"\n",
            log.display(),
            remote_home.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `--remote` (M9.1): the CLI (and the TUI, the same client) works against a daemon on another
/// machine through `ssh host ptyro proxy`; frames are compressed; a failing ssh says why.
#[test]
fn remote_over_ssh() {
    let w = World::new("", &[]);
    w.ok(&["new", "-d", "-n", "far", "--", "sh", "-c", "echo far away; sleep 60"]);
    let local = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(local.path(), w.d.home.path());
    let remote = |args: &[&str]| {
        std::process::Command::new(BIN)
            .args(["--remote", "ssh://me@box.example:2222"])
            .args(args)
            .env("PTYRO_HOME", local.path())
            .env("PTYRO_SSH", &ssh)
            .env("PTYRO_REMOTE_BIN", BIN)
            .env_remove("PTYRO_SOCKET")
            .env_remove("PTYRO_SESSION_ID")
            .output()
            .unwrap()
    };
    let out = remote(&["ls", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(list.as_array().unwrap().iter().any(|s| s["name"] == "far"), "{list}");
    let out = remote(&[
        "new",
        "-d",
        "-n",
        "made-remotely",
        "--",
        "sh",
        "-c",
        "echo hi from there; sleep 60",
    ]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    w.wait_screen("made-remotely", "hi from there");
    let read = remote(&["read", "made-remotely"]);
    assert!(String::from_utf8_lossy(&read.stdout).contains("hi from there"));
    // No local daemon was started.
    assert!(!local.path().join("run").join("ptyro.sock").exists());
    let args = std::fs::read_to_string(local.path().join("ssh-args")).unwrap();
    assert!(
        args.contains("-p 2222")
            && args.contains("me@box.example")
            && args.contains("BatchMode=yes"),
        "{args}"
    );
    assert!(args.contains("proxy"), "{args}");

    // ssh failing: the message carries what ssh said and what to check.
    let bad = local.path().join("bad-ssh");
    std::fs::write(
        &bad,
        "#!/bin/sh\necho 'me@box: Permission denied (publickey).' >&2\nexit 255\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = std::process::Command::new(BIN)
        .args(["--remote", "me@box", "ls"])
        .env("PTYRO_HOME", local.path())
        .env("PTYRO_SSH", &bad)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(8), "{err}");
    assert!(
        err.contains("Permission denied (publickey)") && err.contains("without a password prompt"),
        "{err}"
    );
}

/// Decrypt a Web Push body the way a browser does (RFC 8291), with the subscription's keys.
fn decrypt(body: &[u8], ua_secret: &p256::SecretKey, auth: &[u8]) -> Vec<u8> {
    use aes_gcm::aead::{Aead, KeyInit};
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let salt = &body[..16];
    let idlen = body[20] as usize;
    let as_public = &body[21..21 + idlen];
    let ct = &body[21 + idlen..];
    let ua_public = ua_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(
        ua_secret.to_nonzero_scalar(),
        p256::PublicKey::from_sec1_bytes(as_public).unwrap().as_affine(),
    );
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public.as_bytes());
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(auth), shared.raw_secret_bytes())
        .expand(&info, &mut ikm)
        .unwrap();
    let prk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
    let mut plain = aes_gcm::Aes128Gcm::new_from_slice(&cek)
        .unwrap()
        .decrypt(aes_gcm::Nonce::from_slice(&nonce), ct)
        .unwrap();
    while plain.last() == Some(&0) {
        plain.pop();
    }
    assert_eq!(plain.pop(), Some(2), "last-record delimiter");
    plain
}

fn b64url(d: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for c in d.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=c.len() {
            s.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    s
}

/// One HTTP request received by a stand-in push service; it answers with `status`.
fn receive_push(l: &TcpListener, status: u16) -> (String, Vec<u8>) {
    l.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + WAIT;
    let mut s = loop {
        match l.accept() {
            Ok((s, _)) => break s,
            Err(_) => {
                assert!(Instant::now() < deadline, "no push arrived");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    s.set_nonblocking(false).unwrap();
    s.set_read_timeout(Some(WAIT)).unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head, body_start) = loop {
        let n = s.read(&mut chunk).unwrap();
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break (String::from_utf8_lossy(&buf[..i]).into_owned(), i + 4);
        }
    };
    let len: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap();
    while buf.len() < body_start + len {
        let n = s.read(&mut chunk).unwrap();
        buf.extend_from_slice(&chunk[..n]);
    }
    let _ = s.write_all(
        format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    (head, buf[body_start..body_start + len].to_vec())
}

/// Web Push (M9.4): the device subscribes, a test and a real attention arrive at the push
/// service encrypted for it (and decrypt), signed with VAPID; a gone subscription is dropped.
#[tokio::test]
async fn web_push_end_to_end() {
    let w = World::new("", &[("PTYRO_TEST_PUSH_HTTP", "1")]);
    let serve = w.serve();
    let (_, token) = w.pair(&serve, "phone", "control");
    let mut phone = Phone::signed_in(serve.port, &token).await;
    let key = phone.ok(json!({ "t": "push_key" })).await["key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 87, "uncompressed P-256 point, base64url");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}/push/abc", listener.local_addr().unwrap().port());
    let ua = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let auth = [9u8; 16];
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let p256dh = b64url(ua.public_key().to_encoded_point(false).as_bytes());
    phone.ok(json!({ "t": "push_subscribe", "sub": { "endpoint": endpoint, "p256dh": p256dh, "auth": b64url(&auth) } })).await;

    let rx = std::thread::spawn(move || {
        let first = receive_push(&listener, 201);
        let second = receive_push(&listener, 201);
        let third = receive_push(&listener, 410);
        (first, second, third)
    });
    let notes = phone.ok(json!({ "t": "push_test" })).await;
    assert_eq!(notes["notes"][0], "push phone: sent (HTTP 201)", "{notes}");
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    w.ok(&["send", "a1", "ask touch x", "--wait-ready"]);
    let deadline = Instant::now() + WAIT;
    while w.json(&["status", "a1", "--json"])["session"]["status"]["status"] != "blocked" {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    w.ok(&["answer", "a1", "allow"]);
    // A third push finds the subscription gone (410).
    let notes = phone.ok(json!({ "t": "push_test" })).await;
    assert!(notes["notes"][0].as_str().unwrap().contains("HTTP 410"), "{notes}");
    let got = tokio::task::spawn_blocking(move || rx.join()).await.unwrap();
    let Ok(((h1, b1), (h2, b2), _)) = got else {
        let logs = std::fs::read_dir(&w.d.paths.state)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("daemon"))
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect::<String>();
        panic!("pushes missing; inbox: {}\nlog:\n{logs}", w.ok(&["inbox", "--json"]));
    };
    for h in [&h1, &h2] {
        let l = h.to_ascii_lowercase();
        assert!(l.starts_with("post /push/abc"), "{h}");
        assert!(l.contains("content-encoding: aes128gcm") && l.contains("ttl: 86400"), "{h}");
        assert!(
            l.contains("authorization: vapid t=")
                && l.contains(&format!("k={}", key.to_ascii_lowercase())),
            "{h}"
        );
    }
    let test: Value = serde_json::from_slice(&decrypt(&b1, &ua, &auth)).unwrap();
    assert_eq!(test["kind"], "test");
    let blocked: Value = serde_json::from_slice(&decrypt(&b2, &ua, &auth)).unwrap();
    assert_eq!(blocked["kind"], "blocked", "{blocked}");
    assert!(blocked["title"].as_str().unwrap().contains("a1"), "{blocked}");
    assert!(h2.to_ascii_lowercase().contains("urgency: high"), "{h2}");
    // 410 Gone: the subscription is dropped.
    let deadline = Instant::now() + WAIT;
    loop {
        let d = w.json(&["devices", "ls", "--json"]);
        if d[0]["push"] == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the gone subscription stayed: {d}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `[remote] serve = true`: the daemon runs `ptyro serve`, and a live upgrade brings it back.
#[test]
fn the_daemon_runs_serve_and_keeps_it_through_upgrades() {
    let port = free_port();
    let w = World::new(&format!("[remote]\nserve = true\nlisten = \"127.0.0.1:{port}\"\n"), &[]);
    wait_http(port);
    assert_eq!(
        http(port, "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").0,
        200
    );
    w.ok(&["daemon", "upgrade"]);
    let deadline = Instant::now() + WAIT;
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
            && http(port, "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").0
                == 200
        {
            break;
        }
        assert!(Instant::now() < deadline, "serve didn't come back after the upgrade");
        std::thread::sleep(Duration::from_millis(100));
    }
    // An address that isn't loopback or Tailscale needs --insecure.
    let out = w.cli(&["serve", "--listen", "0.0.0.0:0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("in clear"));
}

/// The web app's TypeScript screen model (web/dist/screen.js), fed live frames through
/// `ptyro serve`, shows exactly what the daemon's own screen does: colours, wide characters,
/// combining marks and wrapped lines included. Skipped without Node.
#[test]
fn the_web_screen_model_matches_the_daemon() {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("node not found: skipping the web client check");
        return;
    }
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, token) = w.pair(&serve, "browser", "read");
    let script = "printf '\\033[1;31mred\\033[0m \\033[38;5;214morange\\033[0m 漢字 e\\314\\201 tab\\there\\n'; \
                  seq 1 30 | tr '\\n' ' '; printf '%0150d\\n' 7; echo END-MARK; sleep 60";
    w.ok(&["new", "-d", "-n", "web1", "--size", "60x20", "--", "sh", "-c", script]);
    w.wait_screen("web1", "END-MARK");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test/frames.mjs");
    let out = std::process::Command::new("node")
        .arg(root)
        .arg(format!("ws://127.0.0.1:{}/ws", serve.port))
        .arg(&token)
        .arg("web1")
        .arg("END-MARK")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let web = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    let daemon = w.ok(&["read", "web1"]).trim_end().to_owned();
    assert_eq!(web, daemon, "the web client and the daemon disagree");
    assert!(web.contains("漢字") && web.contains("e\u{301}"), "{web}");
}
