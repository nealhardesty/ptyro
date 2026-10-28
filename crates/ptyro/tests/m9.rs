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
            // Plain HTTP: these tests speak the protocol; `serve_speaks_https_by_default` the TLS.
            .args(["serve", "--loopback-http", "--port", &port.to_string()])
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
            "POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    // Control can't run arbitrary commands, and only a full device pairs devices.
    let o = tab.request(json!({ "t": "spawn", "spec": { "argv": ["sh"], "cwd": "/" } })).await;
    assert!(
        o["t"] == "err" && o["error"]["message"].as_str().is_some_and(|m| m.contains("needs full")),
        "{o}"
    );
    let o = tab.request(json!({ "t": "device_pair", "name": "x", "scope": "full" })).await;
    assert!(
        o["t"] == "err" && o["error"]["message"].as_str().unwrap().contains("paired and revoked"),
        "{o}"
    );
    let mut lap = Phone::signed_in(serve.port, &full).await;
    let made = lap.ok(json!({ "t": "device_pair", "name": "slate", "scope": "approve" })).await;
    assert!(made["code"].as_str().unwrap().starts_with("ptp_"), "{made}");
    // Claiming is `ptyro serve`'s, for a device that isn't one yet: never a device's.
    let o = lap.request(json!({ "t": "device_claim", "code": "ptp_x", "via": null })).await;
    assert_eq!(o["t"], "err", "{o}");

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
/// can't sign in; the default scope is `full`, with a warning.
#[tokio::test]
async fn pairing_codes_are_single_use_and_expire() {
    let w = World::new("", &[("PTYRO_TEST_PAIRING_TTL_MS", "3000")]);
    let serve = w.serve();
    let v = w.json(&["pair", "phone", "--json"]);
    assert_eq!(v["device"]["scope"], "full");
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

/// A link opens where `[remote] access` and `port` say, over HTTPS; a `--url` is used as written.
#[test]
fn pair_links_follow_access_and_port() {
    let w = World::new("", &[("PTYRO_TEST_PAIRING_TTL_MS", "1500")]);
    let link = |w: &World, args: &[&str]| -> (String, Value) {
        let mut a = vec!["pair", "p", "--scope", "read", "--json"];
        a.extend_from_slice(args);
        let v = w.json(&a);
        (v["url"].as_str().unwrap().to_owned(), v)
    };
    // Out of the box: this computer, the default port, HTTPS; and it says what to do for a phone.
    let (u, v) = link(&w, &[]);
    assert!(u.starts_with("https://127.0.0.1:7091/#pair="), "{u}");
    assert!(v["notes"].to_string().contains("remote.access"), "{v}");
    let (u, _) = link(&w, &["--url", "https://phone-host.example:8443/"]);
    assert!(u.starts_with("https://phone-host.example:8443/#pair="), "{u}");
    // A port of your own is in the link.
    let p = World::new("[remote]\nport = 8123\n", &[("PTYRO_TEST_PAIRING_TTL_MS", "1500")]);
    let (u, _) = link(&p, &[]);
    assert!(u.starts_with("https://127.0.0.1:8123/#pair="), "{u}");
    // The text form prints the same link (QR and all).
    let out = w.cli(&["pair", "q", "--scope", "read", "--url", "https://box.example:7091"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("https://box.example:7091/#pair=ptp_"), "{text}");
}

/// `full` is the default scope, with a plain warning; naming a scope is the owner's choice and
/// is not nagged.
#[test]
fn the_default_scope_is_full_with_a_warning_and_a_named_one_has_none() {
    let w = World::new("", &[("PTYRO_TEST_PAIRING_TTL_MS", "1500")]);
    let v = w.json(&["pair", "a", "--json"]);
    assert_eq!(v["device"]["scope"], "full");
    let warn = v["warning"].as_str().unwrap();
    assert!(warn.contains("FULL") && warn.contains("--scope approve"), "{v}");
    let v = w.json(&["pair", "b", "--scope", "approve", "--json"]);
    assert_eq!(v["device"]["scope"], "approve");
    assert!(v["warning"].is_null(), "{v}");
    let v = w.json(&["pair", "c", "--scope", "full", "--json"]);
    assert!(v["warning"].is_null(), "an explicit full is not nagged: {v}");
    let out = w.cli(&["pair", "d"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("WARNING: This device gets FULL"));
    let out = w.cli(&["pair", "e", "--scope", "read"]);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("WARNING"));
}

/// Pairing a name again replaces that device and takes the scope asked for now, up or down: the
/// old token and its live connections stop at once, and nothing changes before the new link is
/// opened.
#[tokio::test]
async fn pairing_a_name_again_replaces_the_device_and_takes_the_new_scope() {
    let w = World::new("", &[("PTYRO_TEST_PAIRING_TTL_MS", "2500")]);
    let serve = w.serve();
    let (id1, old) = w.pair(&serve, "phone", "approve");
    let mut live = Phone::signed_in(serve.port, &old).await;
    // Upgrade. Until the new link is opened the old device is untouched.
    let v = w.json(&["pair", "phone", "--scope", "full", "--json"]);
    assert_eq!(v["replaces"]["id"], id1.as_str(), "{v}");
    assert_eq!(v["replaces"]["scope"], "approve");
    live.ok(json!({ "t": "list" })).await;
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    let (status, body) = claim(serve.port, &code);
    assert_eq!(status, 200, "{body}");
    let r: Value = serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(r["replaced"]["id"], id1.as_str(), "{r}");
    let bye = live.until("goodbye", |m| m["t"] == "goodbye").await;
    assert!(bye["reason"].as_str().unwrap().contains("revoked"), "{bye}");
    let (p, _) =
        Phone::hello(serve.port, json!({ "kind": "web", "version": "x", "pid": 0, "token": old }))
            .await;
    assert!(p.is_none(), "the old token is refused");
    let devices = w.json(&["devices", "ls", "--json"]);
    assert_eq!(devices.as_array().unwrap().len(), 1, "{devices}");
    assert_eq!(devices[0]["scope"], "full");
    // The new token works, with the new scope.
    let new = r["token"].as_str().unwrap().to_owned();
    let mut p = Phone::signed_in(serve.port, &new).await;
    let o = p.request(json!({ "t": "spawn", "spec": { "argv": ["sh"], "cwd": "/" } })).await;
    assert_ne!(o["t"], "err", "{o}");
    // Downgrade: full to read.
    let v = w.json(&["pair", "Phone", "--scope", "read", "--json"]);
    assert_eq!(v["replaces"]["scope"], "full", "{v}");
    let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
    let (status, body) = claim(serve.port, &code);
    assert_eq!(status, 200, "{body}");
    let devices = w.json(&["devices", "ls", "--json"]);
    assert_eq!(devices.as_array().unwrap().len(), 1, "{devices}");
    assert_eq!(devices[0]["scope"], "read");
    let (p, _) =
        Phone::hello(serve.port, json!({ "kind": "web", "version": "x", "pid": 0, "token": new }))
            .await;
    assert!(p.is_none(), "the replaced token is refused");
    // An unused link is superseded by the next one for the same name.
    let (_, first) = w.pair_code("tablet", "read");
    let (_, second) = w.pair_code("tablet", "control");
    assert_eq!(claim(serve.port, &first).0, 401, "the earlier link is dead");
    assert_eq!(claim(serve.port, &second).0, 200);
    // The text form says what happens.
    let _ = w.json(&["pair", "tablet", "--scope", "approve", "--json"]);
    let out = w.cli(&["pair", "tablet", "--scope", "approve"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Replacing the existing device"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The daemon names its machine (`[daemon] machine_name`, else the host name), in its status
/// and in the pairing output; the web app and the TUI show it.
#[test]
fn the_daemon_reports_its_machine() {
    let w = World::new(
        "[daemon]\nmachine_name = \"studio\"\n",
        &[("PTYRO_TEST_PAIRING_TTL_MS", "1500")],
    );
    assert_eq!(w.json(&["daemon", "status", "--json"])["machine"], "studio");
    assert!(w.ok(&["daemon", "status"]).contains("machine   studio"));
    assert_eq!(w.json(&["pair", "m", "--scope", "read", "--json"])["machine"], "studio");
    let out = w.cli(&["pair", "m2", "--scope", "read"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Pairing \"m2\" with studio."));
    assert!(w.ok(&["config", "get", "daemon.machine_name"]).contains("studio"));
    // Unset: the host name, short and plain.
    let auto = World::new("", &[]);
    let m = auto.json(&["daemon", "status", "--json"])["machine"].as_str().unwrap().to_owned();
    assert!(!m.is_empty() && m.chars().count() <= 24 && !m.contains('.'), "{m}");
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
    use p256::elliptic_curve::sec1::ToSec1Point;
    let salt = &body[..16];
    let idlen = body[20] as usize;
    let as_public = &body[21..21 + idlen];
    let ct = &body[21 + idlen..];
    let ua_public = ua_secret.public_key().to_sec1_point(false);
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
    use p256::elliptic_curve::sec1::ToSec1Point;
    let p256dh = b64url(ua.public_key().to_sec1_point(false).as_bytes());
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
    let w = World::new(
        &format!("[remote]\nserve = true\naccess = \"localhost\"\nport = {port}\n"),
        &[("PTYRO_LOOPBACK_HTTP", "1")],
    );
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
    // Plain HTTP is for this computer only: never with a wider `access`.
    let fresh = World::new("", &[]);
    let out = fresh.cli(&["serve", "--loopback-http", "--access", "open"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("plain HTTP is only for this computer"), "{err}");
    // Half of your own certificate pair says which half is missing, not a silent fallback.
    let half = World::new("[remote]\ntls_cert = \"/nowhere/cert.pem\"\n", &[]);
    let out = half.cli(&["serve", "--port", "0"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("tls_key isn't") && err.contains("remote.tls_key PATH"), "{err}");
}

/// WEB1.9: the web app's server is a setting. `ptyro web on` has the daemon run it (the config
/// file says so, nothing else in it moves), `daemon status` and the doctor say where it is,
/// `web off` takes the port back, and a daemon restart brings it up again by itself.
#[test]
fn the_web_app_is_a_setting_the_daemon_keeps() {
    // Held until the instant before it is wanted: other tests pick "free" ports too, and this
    // one is not bound until `web on`, minutes into a busy run.
    let hold = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = hold.local_addr().unwrap().port();
    let mut w = World::new(
        &format!("[remote]\naccess = \"localhost\"\nport = {port}\n"),
        &[("PTYRO_LOOPBACK_HTTP", "1")],
    );
    let config_path = std::path::PathBuf::from(w.ok(&["config", "path"]).trim());
    let status = |w: &World| -> serde_json::Value {
        let out = w.ok(&["daemon", "status", "--json"]);
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["web"].clone()
    };
    let closed = |port: u16| {
        let deadline = Instant::now() + WAIT;
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(Instant::now() < deadline, "the port stayed open");
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Off by default, and it says how to turn it on.
    let web = status(&w);
    assert_eq!(web["wanted"], false);
    assert_eq!(web["up"], false);
    let said = w.ok(&["web"]);
    assert!(said.contains("off") && said.contains("ptyro web on"), "{said}");

    // A way to be reached that isn't one of ours is refused, and changes nothing.
    let bad = w.cli(&["web", "on", "--access", "carrier-pigeon"]);
    assert!(!bad.status.success());

    // On: up, with its address, before the command returns.
    drop(hold);
    let said = w.ok(&["web", "on"]);
    assert!(said.contains("up at http://127.0.0.1:"), "{said}");
    assert!(said.contains("ptyro pair"), "{said}");
    wait_http(port);
    assert_eq!(
        http(port, "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").0,
        200
    );
    let web = status(&w);
    assert_eq!((web["wanted"].clone(), web["up"].clone()), (true.into(), true.into()), "{web}");
    assert!(web["url"].as_str().unwrap().contains(&port.to_string()), "{web}");
    assert!(web["pid"].as_u64().is_some());

    // The file keeps the rest of itself.
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config.contains("serve = true") && config.contains(&format!("port = {port}")),
        "{config}"
    );

    // A `[remote]` setting changed from the settings commands starts the server again with it.
    let before = status(&w)["pid"].as_u64().unwrap();
    w.ok(&["config", "set", "remote.approval_wait", "5m"]);
    let deadline = Instant::now() + WAIT;
    loop {
        let web = status(&w);
        if web["up"] == true && web["pid"].as_u64().is_some_and(|p| p != before) {
            break;
        }
        assert!(Instant::now() < deadline, "the web app wasn't started again: {web}");
        std::thread::sleep(Duration::from_millis(100));
    }
    wait_http(port);
    // The daemon's log level is a setting it follows while it runs.
    w.ok(&["config", "set", "daemon.log_level", "debug"]);
    assert!(!w.cli(&["config", "set", "daemon.log_level", "loud"]).status.success());
    w.ok(&["config", "unset", "daemon.log_level", "remote.approval_wait"]);

    // The doctor and `daemon status` show it.
    let doctor: serde_json::Value =
        serde_json::from_str(&w.ok(&["doctor", "--quick", "--json"])).unwrap();
    let check = doctor.as_array().unwrap().iter().find(|c| c["name"] == "web app");
    let check = check.unwrap_or_else(|| panic!("no web app check in {doctor}"));
    assert_eq!(check["level"], "ok", "{check}");
    assert!(w.ok(&["daemon", "status"]).contains("web app   up at"));

    // A paired phone still works through a daemon restart: the server comes back with it.
    w.d.restart().unwrap();
    wait_http(port);
    let deadline = Instant::now() + WAIT;
    while status(&w)["up"] != true {
        assert!(
            Instant::now() < deadline,
            "the web app didn't come back with the daemon: {}\n{}",
            status(&w),
            String::from_utf8_lossy(
                &std::process::Command::new("ps")
                    .args(["-ax", "-o", "pid,ppid,etime,command"])
                    .output()
                    .unwrap()
                    .stdout
            )
            .lines()
            .filter(|l| l.contains("serve --managed") || l.contains("daemon start"))
            .collect::<Vec<_>>()
            .join("\n")
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // A daemon that dies without a word (kill -9) leaves its server behind with the socket file:
    // the server notices it is orphaned and leaves, so the next daemon's can have the port.
    w.d.crash();
    w.d.restart().unwrap();
    let deadline = Instant::now() + WAIT;
    while status(&w)["up"] != true {
        assert!(
            Instant::now() < deadline,
            "the web app didn't come back after a crash: {}",
            status(&w)
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Off: the port is free, devices and all stay as they were, and it stays off.
    let said = w.ok(&["web", "off"]);
    assert!(said.contains("off"), "{said}");
    closed(port);
    assert_eq!(status(&w)["wanted"], false);
    assert!(std::fs::read_to_string(&config_path).unwrap().contains("serve = false"));

    // A `ptyro serve` started by hand is noticed, and the daemon doesn't fight it for the port.
    let by_hand =
        w.d.cmd()
            .args(["serve", "--loopback-http", "--port", &port.to_string()])
            .env("HOME", w.home())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
    wait_http(port);
    let deadline = Instant::now() + WAIT;
    let web = loop {
        let web = status(&w);
        if web["by_hand"].is_number() || Instant::now() > deadline {
            break web;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(web["by_hand"].as_u64(), Some(u64::from(by_hand.id())), "{web}");
    assert!(w.ok(&["web"]).contains("started by hand"));
    let mut by_hand = by_hand;
    let _ = by_hand.kill();
    let _ = by_hand.wait();
}

/// The web app's TypeScript screen model (web/dist/screen.js), fed live frames through
/// `ptyro serve`, shows exactly what the daemon's own screen does: colours, wide characters,
/// combining marks and wrapped lines included. Skipped without Node.
#[test]
fn the_web_screen_model_matches_the_daemon() {
    if !node_available() {
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

/// NOTE1.5: the web app's scratchpad logic (web/test/pad.mjs): the Markdown reader, the offline
/// queue, and, through `ptyro serve`, add / merging put / tick / history for a device with
/// control, while a read-only device can read and is refused every write. Skipped without Node.
#[test]
fn the_web_scratchpad_client_logic_works() {
    if !node_available() {
        return;
    }
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, control) = w.pair(&serve, "phone", "control");
    let (_, read) = w.pair(&serve, "viewer", "read");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test/pad.mjs");
    let out = std::process::Command::new("node")
        .arg(script)
        .arg(format!("ws://127.0.0.1:{}/ws", serve.port))
        .arg(&control)
        .arg(&read)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// WEB1.7: `ptyro serve` speaks HTTPS by default, with a certificate from a CA it made, which a
/// phone trusts once (`/ca.crt`): valid for `localhost` and `127.0.0.1`; plain HTTP gets nothing;
/// the web app's WebSocket works over it; `ptyro pair` points at the `https://` address.
#[tokio::test]
async fn serve_speaks_https_by_default() {
    use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
    let w = World::new("", &[]);
    let port = free_port();
    let mut child =
        w.d.cmd()
            .args(["serve", "--access", "localhost", "--port", &port.to_string()])
            .env("HOME", w.home())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
    wait_http(port);
    let ca_path = w.d.paths.data.join("serve/tls/ca.pem");
    let ca_pem = std::fs::read_to_string(&ca_path).expect("the CA, made on first use");
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let client = std::sync::Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let tls = |name: ServerName<'static>| {
        let client = std::sync::Arc::clone(&client);
        async move {
            let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            tokio_rustls::TlsConnector::from(client).connect(name, tcp).await
        }
    };
    let get = |name: ServerName<'static>, path: &'static str| {
        let tls = &tls;
        async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = tls(name).await.expect("a certificate the CA vouches for");
            let req =
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ = s.read_to_end(&mut out).await;
            String::from_utf8_lossy(&out).into_owned()
        }
    };
    // Trusted for its names and its address.
    for name in [
        ServerName::try_from("localhost").unwrap(),
        ServerName::from(std::net::IpAddr::from([127, 0, 0, 1])),
    ] {
        let r = get(name, "/healthz").await;
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    }
    // The CA, for a phone to trust once.
    let r = get(ServerName::try_from("localhost").unwrap(), "/ca.crt").await;
    assert!(r.contains("application/x-x509-ca-cert") && r.contains(ca_pem.trim()), "{r}");
    // Not a name it holds: refused by the client, as a phone's browser would.
    assert!(tls(ServerName::try_from("example.com").unwrap()).await.is_err());
    // Plain HTTP to it: no answer a browser would show.
    let (code, _) =
        http(port, "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    assert_ne!(code, 200);
    // The web app's WebSocket, over TLS (wss): hello without a token is answered (and refused).
    let s = tls(ServerName::try_from("localhost").unwrap()).await.unwrap();
    let (mut ws, _) =
        tokio_tungstenite::client_async(format!("wss://localhost:{port}/ws"), s).await.unwrap();
    let hello = json!({ "t": "hello", "proto": { "major": 1, "minor": 8 }, "client": { "kind": "web", "version": "web", "pid": 0 }, "features": [] });
    ws.send(Message::text(hello.to_string())).await.unwrap();
    let first = loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => break serde_json::from_str::<Value>(&t).unwrap(),
            _ => continue,
        }
    };
    assert_eq!(first["t"], "reject", "{first}");
    // `ptyro pair` sends phones to this serve's https address, and says how to trust it.
    let paired: Value = serde_json::from_str(&w.ok(&["pair", "--json", "phone"])).unwrap();
    assert!(
        paired["url"].as_str().unwrap().starts_with(&format!("https://127.0.0.1:{port}/")),
        "{paired}"
    );
    assert_eq!(paired["ca"]["url"], format!("https://127.0.0.1:{port}/ca.crt"));
    let _ = child.kill();
    let _ = child.wait();
}

/// WEB1.7: `--cert`/`--key` serve your own certificate (phones that trust its issuer need
/// nothing installed), and there is no ptyro CA to hand out.
#[tokio::test]
async fn serve_uses_your_own_certificate() {
    use rustls::pki_types::{CertificateDer, ServerName};
    let w = World::new("", &[]);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let (cert_path, key_path) = (w.home().join("mine.pem"), w.home().join("mine-key.pem"));
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    let port = free_port();
    let mut child =
        w.d.cmd()
            .args(["serve", "--access", "localhost", "--port", &port.to_string(), "--cert"])
            .arg(&cert_path)
            .arg("--key")
            .arg(&key_path)
            .env("HOME", w.home())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
    wait_http(port);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(cert.der().to_vec())).unwrap();
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let get = |path: &'static str| {
        let client = std::sync::Arc::new(client.clone());
        async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let mut s = tokio_rustls::TlsConnector::from(client)
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await
                .expect("your certificate");
            let req =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ = s.read_to_end(&mut out).await;
            String::from_utf8_lossy(&out).into_owned()
        }
    };
    assert!(get("/healthz").await.starts_with("HTTP/1.1 200"));
    assert!(get("/ca.crt").await.starts_with("HTTP/1.1 404"), "no CA of ptyro's to trust");
    assert!(!w.d.paths.data.join("serve/tls/ca.pem").exists(), "none made");
    let _ = child.kill();
    let _ = child.wait();
}

/// Node is there. Without it the web app's logic tests are skipped, unless `PTYRO_REQUIRE_NODE` is
/// set (CI sets it): then a missing Node fails the test instead of letting it pass unrun.
fn node_available() -> bool {
    let there = std::process::Command::new("node").arg("--version").output().is_ok();
    if !there {
        assert!(
            std::env::var_os("PTYRO_REQUIRE_NODE").is_none(),
            "PTYRO_REQUIRE_NODE is set but node isn't installed: the web app's tests would be skipped"
        );
        eprintln!("node not found: skipping a web logic test");
    }
    there
}

/// `POST /api/host` with a bearer token → (status, the JSON body).
fn host(port: u16, token: Option<&str>, body: &Value) -> (u16, Value) {
    let body = body.to_string();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let (code, text) = http(
        port,
        &format!(
            "POST /api/host HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    );
    let json = text
        .split("\r\n\r\n")
        .nth(1)
        .and_then(|b| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    (code, json)
}

/// WEB1.8: a device with `control` scope may take control of a session and size it to its own
/// screen; one that only watches never resizes it under the person at the terminal.
#[tokio::test]
async fn a_control_device_may_size_a_session_and_a_watching_one_may_not() {
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, ctl) = w.pair(&serve, "typist", "control");
    let (_, ro) = w.pair(&serve, "watcher", "approve");
    w.ok(&["new", "-d", "-n", "sz", "--size", "80x24", "--", "sh", "-c", "sleep 120"]);
    let id = w.json(&["status", "sz", "--json"])["session"]["id"].as_str().unwrap().to_owned();
    let size = || {
        let v = w.json(&["status", "sz", "--json"]);
        (
            v["session"]["size"]["cols"].as_u64().unwrap(),
            v["session"]["size"]["rows"].as_u64().unwrap(),
        )
    };
    let wait = |want: (u64, u64)| {
        let deadline = Instant::now() + WAIT;
        while size() != want {
            assert!(Instant::now() < deadline, "the session stayed {:?}, wanted {want:?}", size());
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    let mut watcher = Phone::signed_in(serve.port, &ro).await;
    watcher.send(json!({ "t": "subscribe", "session": id, "size": { "cols": 120, "rows": 40 }, "control": true })).await;
    watcher
        .send(json!({ "t": "resize", "session": id, "size": { "cols": 120, "rows": 40 } }))
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(size(), (80, 24), "a watching device resized the session");

    let mut typist = Phone::signed_in(serve.port, &ctl).await;
    typist.send(json!({ "t": "subscribe", "session": id, "size": { "cols": 100, "rows": 30 }, "control": true })).await;
    wait((100, 30));
    typist.send(json!({ "t": "resize", "session": id, "size": { "cols": 90, "rows": 20 } })).await;
    wait((90, 20));
}

/// WEB1.8: the web terminal's keyboard and selection (web/dist/keys.js, grid.js) against a real
/// session. The keys a browser makes are pressed into a session that reads its own raw input,
/// and what it received is what the TypeScript said it would send; a selection over the
/// scrollback copies what `ptyro read --scrollback` prints. Skipped without Node.
#[test]
fn the_web_terminal_types_into_a_real_session() {
    if !node_available() {
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test/term.mjs");
    let node = |args: &[&str]| {
        let out = std::process::Command::new("node").arg(&script).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(node(&["units"]), "ok");
    let plan: Value = serde_json::from_str(&node(&["plan"])).unwrap();
    let (count, hex) = (plan["count"].as_u64().unwrap(), plan["hex"].as_str().unwrap().to_owned());

    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, token) = w.pair(&serve, "typist", "control");
    let url = format!("ws://127.0.0.1:{}/ws", serve.port);

    // A program in raw mode with bracketed paste on, printing the bytes it was sent.
    let cmd = format!(
        "printf '\\033[?2004h'; stty raw -echo opost onlcr; echo READY; dd bs=1 count={count} 2>/dev/null | od -An -tx1 -v; sleep 60"
    );
    w.ok(&["new", "-d", "-n", "keys", "--size", "80x24", "--", "sh", "-c", &cmd]);
    w.wait_screen("keys", "READY");
    node(&["type", &url, &token, "keys"]);
    let deadline = Instant::now() + WAIT;
    let got = loop {
        let screen = w.ok(&["read", "keys"]);
        let bytes: Vec<String> = screen
            .split_whitespace()
            .filter(|t| t.len() == 2 && t.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_owned)
            .collect();
        if bytes.len() as u64 >= count || Instant::now() > deadline {
            break bytes.join(" ");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        got,
        hex,
        "the session received other bytes than the keys the page sends:\n{}",
        w.ok(&["read", "keys"])
    );

    // A read-only device may not type, whatever it sends.
    let (_, ro) = w.pair(&serve, "watcher", "read");
    w.ok(&[
        "new",
        "-d",
        "-n",
        "quiet",
        "--",
        "sh",
        "-c",
        "stty raw -echo opost onlcr; echo READY; cat -v; sleep 60",
    ]);
    w.wait_screen("quiet", "READY");
    node(&["type", &url, &ro, "quiet"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!w.ok(&["read", "quiet"]).contains("^["), "a read-only device typed");

    // A selection over the scrollback copies what `read --scrollback` prints.
    w.ok(&[
        "new",
        "-d",
        "-n",
        "long",
        "--size",
        "40x10",
        "--",
        "sh",
        "-c",
        "seq 1 100; echo THE-END; sleep 60",
    ]);
    w.wait_screen("long", "THE-END");
    let doc: Value = serde_json::from_str(&node(&["scroll", &url, &token, "long"])).unwrap();
    assert!(doc["history"].as_u64().unwrap() > 90, "{doc}");
    let norm = |t: &str| {
        let mut lines: Vec<String> = t.lines().map(|l| l.trim_end().to_owned()).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    };
    let copied = norm(doc["text"].as_str().unwrap());
    let read = norm(&w.ok(&["read", "long", "--scrollback"]));
    assert_eq!(copied, read, "what a select-all copies is what `read --scrollback` prints");
    assert!(copied.starts_with("1\n2\n3") && copied.ends_with("100\nTHE-END"), "{copied}");
}

/// WEB1.9: a device may ask how the web app's server is; turning it on or off is `full`.
#[tokio::test]
async fn only_a_full_device_changes_the_web_apps_server() {
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, ctl) = w.pair(&serve, "tablet", "control");
    let (_, full) = w.pair(&serve, "laptop", "full");
    let mut tab = Phone::signed_in(serve.port, &ctl).await;
    let mut lap = Phone::signed_in(serve.port, &full).await;

    // The tablet may look.
    let r = tab.ok(json!({ "t": "web" })).await;
    assert_eq!(r["web"]["wanted"], false, "{r}");

    // It may not change it, and the setting didn't move.
    let r = tab.request(json!({ "t": "web", "on": true })).await;
    assert_eq!(r["t"], "err", "{r}");
    assert!(w.ok(&["web"]).contains("off"));

    // The laptop may: off is off, with nothing to say.
    let r = lap.ok(json!({ "t": "web", "on": false })).await;
    assert_eq!(r["web"]["wanted"], false, "{r}");
}

/// ROADMAP WEB1: what the web app asks of the daemon and of the machine.
#[tokio::test]
async fn the_web_app_asks_the_daemon_and_the_machine() {
    let w = World::new("", &[]);
    let serve = w.serve();
    let (_, ctl) = w.pair(&serve, "tablet", "control");
    let (_, full) = w.pair(&serve, "laptop", "full");
    let welcome = |p: Value| p["scope"].clone();

    // The welcome says what the device may do.
    let (_, first) = Phone::hello(
        serve.port,
        json!({ "kind": "web", "version": "web", "pid": 0, "token": ctl }),
    )
    .await;
    assert_eq!(welcome(first), "control");
    let mut lap = Phone::signed_in(serve.port, &full).await;
    let mut tab = Phone::signed_in(serve.port, &ctl).await;

    // The agents it can start and the editors it can open a worktree in.
    let a = tab.ok(json!({ "t": "agents" })).await;
    assert_eq!(a["default"], "claude", "{a}");
    let claude = a["agents"].as_array().unwrap().iter().find(|x| x["name"] == "claude").unwrap();
    assert_eq!(claude["installed"], true, "{a}");
    let e = tab.ok(json!({ "t": "editors" })).await;
    assert!(e["names"].as_array().unwrap().iter().any(|n| n == "nvim"), "{e}");

    // A session's recording: exported as text and asciicast, replayed as screen frames. A
    // control device stops what it started.
    w.ok(&["new", "-d", "-n", "rec", "--", "sh", "-c", "echo recorded-line; sleep 30"]);
    w.wait_screen("rec", "recorded-line");
    let t = tab.ok(json!({ "t": "export", "addr": "rec", "format": "text" })).await;
    assert!(t["text"].as_str().unwrap().contains("recorded-line"), "{t}");
    let c = tab.ok(json!({ "t": "export", "addr": "rec", "format": "asciicast" })).await;
    assert!(c["text"].as_str().unwrap().starts_with("{\"version\":2"), "{c}");
    let o = tab.request(json!({ "t": "export", "addr": "rec", "format": "mp4" })).await;
    assert_eq!(o["t"], "err", "{o}");
    let r = tab.ok(json!({ "t": "replay", "addr": "rec" })).await;
    let steps = r["replay"]["steps"].as_array().unwrap();
    assert!(!steps.is_empty() && steps[0]["keyframe"] == true, "{r}");
    assert!(steps[1..].iter().all(|s| s["keyframe"] == false));
    let shown = serde_json::to_string(&r).unwrap();
    assert!(
        shown.contains("recorded-line") || shown.contains("recorded"),
        "the replay carries the output"
    );
    tab.ok(json!({ "t": "kill", "addr": "rec", "signal": null })).await;
    tab.ok(json!({ "t": "remove", "addr": "rec" })).await;

    // A project's configuration, with where each value comes from.
    let repo = w.home().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    w.ok(&["project", "add", repo.to_str().unwrap()]);
    let cfg = lap.ok(json!({ "t": "project_config", "addr": "repo" })).await;
    assert!(cfg["text"].as_str().unwrap().contains("where each value comes from"), "{cfg}");

    // The machine: a full device reads, checks and saves the config; others are refused.
    let (code, v) = host(serve.port, Some(&full), &json!({ "op": "config.get" }));
    assert_eq!((code, &v["t"]), (200, &json!("ok")), "{v}");
    assert!(v["reply"]["path"].as_str().unwrap().ends_with("config.toml"), "{v}");
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "config.check", "text": "[ui\n" }));
    assert_eq!(v["t"], "err", "a broken config is refused: {v}");
    let (_, v) = host(
        serve.port,
        Some(&full),
        &json!({ "op": "config.check", "text": "[ui]\nsidebar_width = 31\n" }),
    );
    assert_eq!(v["t"], "ok", "{v}");
    let text = format!(
        "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n[ui]\nsidebar_width = 33\n"
    );
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "config.save", "text": text }));
    assert_eq!(v["t"], "ok", "{v}");
    assert!(w.ok(&["config", "show"]).contains("sidebar_width = 33"));
    let (code, v) = host(serve.port, Some(&ctl), &json!({ "op": "config.get" }));
    assert_eq!(code, 200);
    assert_eq!(v["error"]["code"], "permission_denied", "{v}");
    assert_eq!(host(serve.port, None, &json!({ "op": "config.get" })).0, 401);
    assert_eq!(host(serve.port, Some("ptd_nope"), &json!({ "op": "config.get" })).0, 401);
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "rm -rf" }));
    assert_eq!(v["t"], "err", "{v}");

    // Profiles through the same functions as the CLI.
    let (_, v) = host(
        serve.port,
        Some(&full),
        &json!({ "op": "profile.new", "name": "work", "label": "Work", "git_email": "me@work.example" }),
    );
    assert_eq!(v["t"], "ok", "{v}");
    assert!(w.ok(&["profile", "ls"]).contains("work"));
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "profile.new", "name": "work" }));
    assert_eq!(v["error"]["code"], "conflict", "{v}");
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "profile.rm", "name": "work" }));
    assert_eq!(v["t"], "ok", "{v}");
    assert!(!w.ok(&["profile", "ls"]).contains("work"));

    // The daemon's own log and the doctor's checks.
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "daemon.logs", "lines": 5 }));
    assert_eq!(v["t"], "ok", "{v}");
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "doctor", "quick": true }));
    let checks = v["reply"]["checks"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert!(
        checks
            .iter()
            .any(|c| c["section"] == "daemon" && c["name"] == "running" && c["level"] == "ok"),
        "{v}"
    );
    // A tour is for its own ptyro only: this one refuses to end itself.
    let (_, v) = host(serve.port, Some(&full), &json!({ "op": "tour.end" }));
    assert_eq!(v["error"]["code"], "invalid", "{v}");
}

/// CFG1.3: the web settings panel's logic (`web/test/config.mjs`), and through `ptyro serve` the
/// calls it makes: the schema, the values with where they come from, a checked change that keeps
/// the file's comments, undo, and a device without full scope being refused. Skipped without Node.
#[test]
fn the_web_settings_panel_logic_and_calls_work() {
    if !node_available() {
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test/config.mjs");
    let run = |args: &[&str]| {
        let out = std::process::Command::new("node").arg(&script).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["units"]);
    let w = World::new("# mine\n[ui]\nmouse = false # kept\n", &[]);
    let serve = w.serve();
    let (_, full) = w.pair(&serve, "laptop", "full");
    let (_, ctl) = w.pair(&serve, "phone", "control");
    run(&["host", &format!("http://127.0.0.1:{}", serve.port), &full]);
    // The comments survived everything the script did.
    let file = std::fs::read_to_string(w.d.paths.config.join("config.toml")).unwrap();
    assert!(file.contains("# mine\n[ui]\nmouse = false # kept\n"), "{file}");
    // Changing settings is for full scope.
    for op in ["settings.schema", "settings.view", "settings.apply", "settings.restore"] {
        let (code, v) = host(serve.port, Some(&ctl), &json!({ "op": op, "scope": "user" }));
        assert_eq!(v["error"]["code"], "permission_denied", "{op}: {code} {v}");
    }
}

/// The web app's own logic, with no daemon: its helpers and request timeouts (`kit.mjs`), and that
/// the service worker's file list, the server's asset list and the built modules agree
/// (`assets.mjs`). Skipped without Node.
#[test]
fn the_web_apps_helpers_and_asset_lists_agree() {
    if !node_available() {
        return;
    }
    for name in ["kit.mjs", "assets.mjs"] {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test").join(name);
        let out = std::process::Command::new("node").arg(script).output().unwrap();
        assert!(
            out.status.success(),
            "{name}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The web app's QR encoder (pairing another device from the web): structure a scanner relies
/// on, in `web/test/qr.mjs`. Skipped without Node.
#[test]
fn the_web_apps_qr_codes_are_well_formed() {
    if !node_available() {
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/test/qr.mjs");
    let out = std::process::Command::new("node").arg(script).output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
