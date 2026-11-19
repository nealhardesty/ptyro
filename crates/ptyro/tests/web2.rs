//! WEB2.3, every key exactly once (SPEC §16.4 WEB1.8): a "phone" (a WebSocket to `ptyro serve`
//! speaking the web app's JSON) numbers its keys, loses its connection mid-burst, and sends them
//! again: each lands once. A replay after the session asked something new (the fake agent's
//! permission prompt) is refused and types nothing; so is one too old. A live upgrade keeps what
//! was typed. A client that doesn't ask for numbered keys (an older web app) gets today's
//! behaviour, and a named `send_text` sent twice types once.

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
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-web2-").tempdir().unwrap();
        let hs = ptyro_core::os::canonicalize(home.path()).unwrap().to_string_lossy().into_owned();
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = ['{BIN}', \"fake-agent\", \"--as\", \"claude\"]\n"
        );
        let d = TempDaemon::start_with(
            BIN,
            &[("HOME", hs.as_str()), ("SHELL", "/bin/sh")],
            Some(&config),
        )
        .unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
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

    /// A paired device's token, traded for its one-time code as the web app does.
    fn pair(&self, serve: &Serve, name: &str, scope: &str) -> String {
        let v = self.json(&["pair", name, "--scope", scope, "--json"]);
        let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
        let body = json!({ "code": code }).to_string();
        let req = format!(
            "POST /api/pair HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", serve.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        let v: Value = serde_json::from_str(out.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        v["token"].as_str().unwrap().to_owned()
    }

    fn serve(&self) -> Serve {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = self
            .d
            .cmd()
            .args(["serve", "--loopback-http", "--port", &port.to_string()])
            .env("HOME", self.home())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        while !ptyro_testkit::listening(port) {
            assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve { child, port }
    }

    /// The session's screen, its lines trimmed.
    fn lines(&self, s: &str) -> Vec<String> {
        self.ok(&["read", s]).lines().map(|l| l.trim_end().to_owned()).collect()
    }

    /// Wait until `f` holds for the session's lines.
    fn wait_lines(&self, s: &str, what: &str, f: impl Fn(&[String]) -> bool) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let l = self.lines(s);
            if f(&l) {
                return l;
            }
            assert!(Instant::now() < deadline, "{s} never showed {what}:\n{}", l.join("\n"));
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

type Ws = WebSocketStream<TcpStream>;

/// The phone: a WebSocket to `ptyro serve`, speaking JSON like the web app.
struct Phone {
    ws: Ws,
    next: u32,
    welcome: Value,
    pushed: Vec<Value>,
}

impl Phone {
    /// Signed in; `numbered`: asks for numbered keys, as the web app does since 1.52.
    async fn new(port: u16, token: &str, numbered: bool) -> Self {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/ws"), tcp)
            .await
            .unwrap();
        let features: Vec<&str> = if numbered { vec!["input_seq"] } else { vec![] };
        let hello = json!({ "t": "hello", "proto": { "major": 1, "minor": 8 }, "client": { "kind": "web", "version": "web", "pid": 0, "token": token }, "features": features });
        ws.send(Message::text(hello.to_string())).await.unwrap();
        let welcome = next_json(&mut ws).await.expect("an answer to hello");
        assert_eq!(welcome["t"], "welcome", "{welcome}");
        Self { ws, next: 1, welcome, pushed: Vec::new() }
    }

    async fn send(&mut self, v: Value) {
        self.ws.send(Message::text(v.to_string())).await.unwrap();
    }

    async fn ok(&mut self, req: Value) -> Value {
        let o = self.request(req.clone()).await;
        assert_eq!(o["t"], "ok", "{req} → {o}");
        o["reply"].clone()
    }

    async fn request(&mut self, req: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "t": "request", "id": id, "req": req })).await;
        let m = self.until("its response", |m| m["t"] == "response" && m["id"] == id).await;
        m["outcome"].clone()
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
            self.pushed.push(m);
        }
    }

    /// The `typed` answer that settles `upto` (or more) for `stream`.
    async fn typed(&mut self, stream: &str, upto: u64) -> Value {
        self.until(&format!("typed up to {upto}"), |m| {
            m["t"] == "typed" && m["stream"] == stream && m["upto"].as_u64() >= Some(upto)
        })
        .await
    }

    /// Nothing more arrives for a moment that matches `f`.
    async fn quiet(&mut self, ms: u64, f: impl Fn(&Value) -> bool) {
        assert!(!self.pushed.iter().any(&f), "{:?}", self.pushed);
        let deadline = Instant::now() + Duration::from_millis(ms);
        while let Ok(Some(m)) = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            next_json(&mut self.ws),
        )
        .await
        {
            assert!(!f(&m), "unexpected: {m}");
            self.pushed.push(m);
        }
    }

    /// A session's `state_token` as the list says it now.
    async fn token_of(&mut self, name: &str) -> String {
        let list = self.ok(json!({ "t": "list" })).await;
        session(&list, name)["state_token"].as_str().expect("a state token").to_owned()
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

fn session<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {list}"))
}

fn ch(c: char) -> Value {
    json!({ "t": "key", "key": { "code": { "t": "char", "c": c.to_string() }, "mods": 0 } })
}

fn enter() -> Value {
    json!({ "t": "key", "key": { "code": { "t": "enter" }, "mods": 0 } })
}

/// A message of live keys.
fn live(session: &str, stream: &str, keys: &[(u64, Value)]) -> Value {
    let keys: Vec<Value> = keys.iter().map(|(n, e)| json!({ "n": n, "event": e })).collect();
    json!({ "t": "keys", "session": session, "stream": stream, "keys": keys })
}

/// A replay: each key with the token it was typed against and how long ago.
fn replay(session: &str, stream: &str, keys: &[(u64, Value)], token: &str, age_ms: u64) -> Value {
    let keys: Vec<Value> = keys
        .iter()
        .map(|(n, e)| json!({ "n": n, "event": e, "token": token, "age_ms": age_ms }))
        .collect();
    json!({ "t": "keys", "session": session, "stream": stream, "keys": keys, "replay": true })
}

/// `cat` on a tty: a line echoed as it is typed, printed again at Enter.
fn cat(w: &World, name: &str) -> String {
    let v = w.json(&["new", "-d", "-n", name, "--json", "--", "sh", "-c", "exec cat"]);
    let id = v["id"].as_str().or_else(|| v["session"]["id"].as_str()).expect("an id").to_owned();
    // Ready to read: a line typed through the CLI comes back.
    w.ok(&["send", name, "ready"]);
    w.wait_lines(name, "cat to be reading", |l| l.iter().filter(|x| *x == "ready").count() == 2);
    id
}

fn count(lines: &[String], want: &str) -> usize {
    lines.iter().filter(|l| *l == want).count()
}

#[tokio::test]
async fn every_key_lands_exactly_once_across_a_dropped_socket() {
    let w = World::new();
    let serve = w.serve();
    let token = w.pair(&serve, "phone", "control");
    let id = cat(&w, "k");

    let mut a = Phone::new(serve.port, &token, true).await;
    let features = a.welcome["features"].as_array().unwrap();
    assert!(features.iter().any(|f| f == "input_seq"), "{}", a.welcome);
    let t0 = a.token_of("k").await;
    assert_eq!(t0.len(), 16, "{t0}");
    let text: Vec<char> = "abcdefghij".chars().collect();
    let mut keys: Vec<(u64, Value)> =
        text.iter().enumerate().map(|(i, c)| (i as u64 + 1, ch(*c))).collect();
    keys.push((11, enter()));
    // 1–5, each acknowledged.
    for k in &keys[..5] {
        a.send(live(&id, "s1", std::slice::from_ref(k))).await;
    }
    let ack = a.typed("s1", 5).await;
    assert!(ack["refused"].is_null(), "{ack}");
    // 6–10, and the socket drops before any answer is read: some may have landed, some not.
    for k in &keys[5..10] {
        a.send(live(&id, "s1", std::slice::from_ref(k))).await;
    }
    drop(a);

    // Back: everything not known settled goes again, numbered as before (1–5 too, to show a
    // copy of what was typed is dropped), with Enter typed while away.
    let mut b = Phone::new(serve.port, &token, true).await;
    b.send(replay(&id, "s1", &keys, &t0, 400)).await;
    let ack = b.typed("s1", 11).await;
    assert!(ack["refused"].is_null(), "{ack}");
    let l = w.wait_lines("k", "the line, typed and printed", |l| count(l, "abcdefghij") == 2);
    assert_eq!(count(&l, "abcdefghij"), 2, "{l:?}");
    // The same again, live or replayed: nothing typed.
    b.send(replay(&id, "s1", &keys, &t0, 600)).await;
    b.send(live(&id, "s1", &keys[3..6])).await;
    b.typed("s1", 11).await;
    std::thread::sleep(Duration::from_millis(500));
    let after = w.lines("k");
    assert_eq!(count(&after, "abcdefghij"), 2, "{after:?}");
    assert!(!after.iter().any(|x| x.contains("abcdefghijab") || x == "def"), "{after:?}");

    // Too old: refused, typed never, and told why.
    b.send(replay(&id, "s1", &[(12, ch('Z')), (13, enter())], &t0, 6_000)).await;
    let no = b.typed("s1", 13).await;
    assert_eq!(no["refused"][0]["why"], "stale", "{no}");
    assert_eq!(
        (no["refused"][0]["from"].as_u64(), no["refused"][0]["to"].as_u64()),
        (Some(12), Some(13))
    );
    // A malformed one settles nothing: numbers not rising.
    b.send(live(&id, "s1", &[(20, ch('q')), (19, ch('q'))])).await;
    let bad = b.typed("s1", 13).await;
    assert_eq!(bad["refused"][0]["why"], "invalid", "{bad}");
    // Live keys after all that go as usual.
    b.send(live(&id, "s1", &[(14, ch('o')), (15, ch('k')), (16, enter())])).await;
    b.typed("s1", 16).await;
    let l = w.wait_lines("k", "the next line", |l| count(l, "ok") == 2);
    assert!(!l.iter().any(|x| x.contains('Z') || x.contains('q')), "{l:?}");

    // Another stream (another page) is its own: its 1 is typed.
    b.send(live(&id, "s2", &[(1, ch('x')), (2, enter())])).await;
    b.typed("s2", 2).await;
    w.wait_lines("k", "another page's keys", |l| count(l, "x") == 2);
}

#[tokio::test]
async fn a_replay_after_the_agent_asked_something_new_types_nothing() {
    let w = World::new();
    let serve = w.serve();
    let token = w.pair(&serve, "phone", "control");
    w.ok(&["new", "-d", "-n", "a1", "--agent", "claude"]);
    let mut p = Phone::new(serve.port, &token, true).await;
    // Ready and idle: what the phone saw as it typed `y⏎` offline.
    let deadline = Instant::now() + WAIT;
    let (id, seen) = loop {
        let list = p.ok(json!({ "t": "list" })).await;
        let s = session(&list, "a1").clone();
        if s["status"]["status"] == "idle" {
            break (
                s["id"].as_str().unwrap().to_owned(),
                s["state_token"].as_str().unwrap().to_owned(),
            );
        }
        assert!(Instant::now() < deadline, "a1 never went idle: {s:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // Meanwhile (the phone offline) it asks for permission.
    w.ok(&["send", "a1", "ask rm -rf build", "--wait-ready"]);
    let deadline = Instant::now() + WAIT;
    let now = loop {
        let list = p.ok(json!({ "t": "list" })).await;
        let s = session(&list, "a1").clone();
        if s["status"]["status"] == "blocked" {
            break s["state_token"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "a1 never blocked: {s:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_ne!(now, seen, "the prompt changed the token");
    // The phone is back within a second: its `y⏎` would answer the prompt. Refused.
    p.send(replay(&id, "p1", &[(1, ch('y')), (2, enter())], &seen, 900)).await;
    let no = p.typed("p1", 2).await;
    assert_eq!(no["refused"][0]["why"], "changed", "{no}");
    assert!(no["refused"][0]["reason"].as_str().unwrap().contains("changed"), "{no}");
    // Sent again (its answer lost), with more typed after: refused again, all of it, and said so.
    p.send(replay(&id, "p1", &[(1, ch('y')), (2, enter()), (3, ch('y'))], &now, 300)).await;
    let again = p.typed("p1", 3).await;
    let ranges: Vec<(u64, u64)> = again["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["from"].as_u64().unwrap(), r["to"].as_u64().unwrap()))
        .collect();
    assert_eq!(ranges, vec![(1, 2), (3, 3)], "{again}");
    std::thread::sleep(Duration::from_millis(700));
    let list = p.ok(json!({ "t": "list" })).await;
    assert_eq!(session(&list, "a1")["status"]["status"], "blocked", "the prompt is still waiting");
    let screen = w.ok(&["read", "a1"]);
    assert!(!screen.contains("answer: yes"), "{screen}");
}

/// What a client that doesn't number its keys (an older web app, a TUI) sees is what it saw
/// before: plain input works, no `typed` arrives, no feature is claimed. A device that may only
/// watch is told its numbered keys weren't typed. A named send is typed once however often it
/// comes.
#[tokio::test]
async fn older_clients_watchers_and_named_sends() {
    let w = World::new();
    let serve = w.serve();
    let token = w.pair(&serve, "phone", "control");
    let id = cat(&w, "c");
    let mut old = Phone::new(serve.port, &token, false).await;
    let features = old.welcome["features"].as_array().cloned().unwrap_or_default();
    assert!(!features.iter().any(|f| f == "input_seq"), "{}", old.welcome);
    for e in [ch('h'), ch('i'), enter()] {
        old.send(json!({ "t": "input", "session": id, "event": e })).await;
    }
    w.wait_lines("c", "plain input", |l| count(l, "hi") == 2);
    old.quiet(400, |m| m["t"] == "typed").await;

    // `send_text` with a name, twice (a retry after a dropped connection): typed once.
    for _ in 0..3 {
        old.ok(json!({ "t": "send_text", "addr": id, "text": "once-only", "enter": true, "wait_ready_ms": null, "once": "n-1" })).await;
    }
    std::thread::sleep(Duration::from_millis(500));
    let l = w.wait_lines("c", "the named send", |l| count(l, "once-only") == 2);
    assert_eq!(count(&l, "once-only"), 2, "{l:?}");
    // Another name is another send.
    old.ok(json!({ "t": "send_text", "addr": id, "text": "once-only", "enter": true, "wait_ready_ms": null, "once": "n-2" })).await;
    w.wait_lines("c", "a second send", |l| count(l, "once-only") == 4);

    // A watcher's numbered keys: refused, said, nothing typed.
    let watch = w.pair(&serve, "watcher", "read");
    let mut v = Phone::new(serve.port, &watch, true).await;
    v.send(live(&id, "w1", &[(1, ch('W')), (2, enter())])).await;
    let no = v.typed("w1", 2).await;
    assert_eq!(no["refused"][0]["why"], "scope", "{no}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!w.lines("c").iter().any(|x| x == "W"));
}

/// A live upgrade keeps what was typed: keys typed before it, sent again to the next image (the
/// connection dropped with their answers), are copies; a named send likewise.
#[cfg(unix)]
#[tokio::test]
async fn what_was_typed_crosses_a_live_upgrade() {
    let w = World::new();
    let serve = w.serve();
    let token = w.pair(&serve, "phone", "control");
    let id = cat(&w, "u");
    let mut p = Phone::new(serve.port, &token, true).await;
    let t0 = p.token_of("u").await;
    let keys: Vec<(u64, Value)> = vec![(1, ch('u')), (2, ch('p')), (3, enter())];
    p.send(live(&id, "s1", &keys)).await;
    p.typed("s1", 3).await;
    p.ok(json!({ "t": "send_text", "addr": id, "text": "named", "enter": true, "wait_ready_ms": null, "once": "u-1" })).await;
    w.wait_lines("u", "the keys and the send", |l| count(l, "up") == 2 && count(l, "named") == 2);
    drop(p);

    let up = w.json(&["daemon", "upgrade", "--json"]);
    assert_eq!(up["rolled_back"], false, "{up}");
    // The bridge reconnects to the next image.
    let deadline = Instant::now() + WAIT;
    let mut p = loop {
        let tcp = TcpStream::connect(("127.0.0.1", serve.port)).await;
        if tcp.is_ok() {
            let mut p = Phone::new(serve.port, &token, true).await;
            if p.request(json!({ "t": "status" })).await["t"] == "ok" {
                break p;
            }
        }
        assert!(Instant::now() < deadline, "never back after the upgrade");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let t1 = p.token_of("u").await;
    assert_eq!(t0, t1, "the token is the same across the upgrade (same process, same status)");
    let mut again = keys.clone();
    again.push((4, ch('!')));
    again.push((5, enter()));
    p.send(replay(&id, "s1", &again, &t1, 500)).await;
    let ack = p.typed("s1", 5).await;
    assert!(ack["refused"].is_null(), "{ack}");
    p.ok(json!({ "t": "send_text", "addr": id, "text": "named", "enter": true, "wait_ready_ms": null, "once": "u-1" })).await;
    let l = w.wait_lines("u", "only the new keys", |l| count(l, "!") == 2);
    std::thread::sleep(Duration::from_millis(400));
    let l2 = w.lines("u");
    assert_eq!((count(&l2, "up"), count(&l2, "named")), (2, 2), "{l:?}");
}
