//! Notification sinks that fail (SPEC §13): the server never answers, answers 500, refuses the
//! connection, or its name doesn't resolve. In every case the session still shows as blocked,
//! the failure is logged without the sink's token or its secret URL path, `notify test` reports
//! it without them, and a pile of hung sinks leaves the daemon responsive and its tasks bounded.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(40);
const TOKEN: &str = "tk_s3cr3tXYZ";
const TOPIC: &str = "secret-topic-4f9a";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Hang,
    Http500,
    Refused,
    NoDns,
}

/// A local stand-in for the sink; `hits` counts the connections it accepted.
struct Server {
    url: String,
    hits: Arc<AtomicUsize>,
}

impl Server {
    fn start(mode: Mode) -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        match mode {
            Mode::NoDns => {
                return Self { url: format!("http://no-such-host.invalid/{TOPIC}"), hits };
            }
            // The port was free a moment ago and nobody listens on it now.
            Mode::Refused => drop(l),
            Mode::Hang | Mode::Http500 => {
                let h = Arc::clone(&hits);
                std::thread::spawn(move || {
                    let mut held = Vec::new();
                    for s in l.incoming().flatten() {
                        h.fetch_add(1, Ordering::SeqCst);
                        if mode == Mode::Hang {
                            held.push(s);
                            continue;
                        }
                        std::thread::spawn(move || {
                            let mut s = s;
                            let mut buf = [0u8; 4096];
                            let mut seen = Vec::new();
                            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                                match s.read(&mut buf) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                                }
                            }
                            // The body repeats the secrets, as a careless server might.
                            let body = format!("error for {TOPIC} with {TOKEN}");
                            let _ = write!(
                                s,
                                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                        });
                    }
                });
            }
        }
        Self { url: format!("http://127.0.0.1:{port}/{TOPIC}"), hits }
    }
}

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(sink_url: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-sinks-").tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1"), ("SHELL", "/bin/sh")];
        let config = format!(
            "[attention]\nmin_work = \"1s\"\n[notifications]\nchannels = [\"inbox\", \"sinks\"]\non = [\"blocked\"]\nsinks = [{{ type = \"ntfy\", url = \"{sink_url}\", token = \"{TOKEN}\", on = [\"blocked\"] }}]\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n"
        );
        let d = TempDaemon::start_with(BIN, &env, Some(&config)).unwrap();
        Self { home, d }
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        self.d
            .cmd()
            .args(args)
            .env("HOME", self.home.path())
            .current_dir(self.home.path())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "ptyro {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(self.ok_stdout(args).as_bytes()).unwrap()
    }

    fn ok_stdout(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(out.status.success(), "ptyro {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn status_of(&self, name: &str) -> String {
        self.json(&["status", name, "--json"])["session"]["status"]["status"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn logs(&self) -> String {
        let mut all = String::new();
        for e in std::fs::read_dir(self.d.paths.log_dir()).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with("daemon") {
                all.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
            }
        }
        all
    }

    fn metrics(&self) -> Value {
        self.json(&["daemon", "status", "--json"])["metrics"].clone()
    }

    /// An agent that gets blocked on a permission prompt (the sink's trigger).
    fn block(&self, name: &str) {
        self.ok(&["new", "-d", "--agent", "claude", "-n", name]);
        self.until(&format!("{name} is up"), || self.ok(&["read", name]).contains("fake claude"));
        self.ok(&["send", name, "ask terraform apply"]);
    }

    fn failures_logged(&self) -> usize {
        self.logs().matches("notification sink ntfy: failed").count()
    }
}

fn assert_no_secrets(what: &str, text: &str) {
    for secret in [TOKEN, TOPIC] {
        assert!(!text.contains(secret), "{what} leaks {secret}:\n{text}");
    }
}

/// The common checks for one way a sink can fail.
fn a_failing_sink(mode: Mode) {
    let server = Server::start(mode);
    let w = World::new(&server.url);
    w.block("n1");
    // (a) The notification path being broken doesn't hide the state.
    w.until("n1 shows blocked", || w.status_of("n1") == "blocked");
    // (b) The failure is logged, and the log says nothing secret.
    w.until("the failure is logged", || w.failures_logged() >= 1);
    assert_no_secrets("the daemon log", &w.logs());
    // `notify test` tells the person it failed, and no secret either.
    let out = w.ok(&["notify", "test"]);
    assert!(out.contains("failed"), "{out}");
    assert_no_secrets("notify test", &out);
    if matches!(mode, Mode::Hang | Mode::Http500) {
        assert!(server.hits.load(Ordering::SeqCst) >= 1, "the server was never asked");
    }
    if mode == Mode::Http500 {
        assert!(out.contains("500"), "{out}");
    }
    // The daemon is fine: it still answers, and the inbox still has the notification.
    assert!(
        w.json(&["inbox", "--json"]).as_array().is_some_and(|a| !a.is_empty()),
        "the inbox channel is independent of the sink"
    );
}

#[test]
fn a_sink_that_answers_500_is_logged_without_its_secrets() {
    a_failing_sink(Mode::Http500);
}

#[test]
fn a_sink_that_refuses_the_connection_is_logged_without_its_secrets() {
    a_failing_sink(Mode::Refused);
}

#[test]
fn a_sink_whose_host_does_not_resolve_is_logged_without_its_secrets() {
    a_failing_sink(Mode::NoDns);
}

#[test]
fn a_sink_that_never_answers_times_out_and_is_logged_without_its_secrets() {
    a_failing_sink(Mode::Hang);
}

/// Twenty notifications to a sink that holds every connection open: status stays live while
/// they wait, and once curl's timeout has fired for all of them the daemon's tasks and memory
/// are back to where they were.
#[test]
fn many_hung_sinks_neither_block_status_nor_pile_up() {
    let server = Server::start(Mode::Hang);
    let w = World::new(&server.url);
    let names: Vec<String> = (0..20).map(|i| format!("h{i}")).collect();
    for n in &names {
        w.ok(&["new", "-d", "--agent", "claude", "-n", n]);
    }
    w.until("all agents are up", || {
        names.iter().all(|n| w.ok(&["read", n]).contains("fake claude"))
    });
    let before = w.metrics();
    let (tasks0, rss0) =
        (before["tasks_alive"].as_u64().unwrap(), before["rss_kib"].as_u64().unwrap());
    for n in &names {
        w.ok(&["send", n, "ask terraform apply"]);
    }
    w.until("every agent is blocked", || names.iter().all(|n| w.status_of(n) == "blocked"));
    // While sinks are hung on the wire, the daemon still answers at once.
    let t = Instant::now();
    w.ok(&["ls"]);
    assert!(t.elapsed() < Duration::from_secs(5), "ls took {:?}", t.elapsed());
    // Every one times out (curl gives up after 10 s) and is logged.
    w.until("all 20 failures are logged", || w.failures_logged() >= 20);
    assert!(server.hits.load(Ordering::SeqCst) >= 20);
    assert_no_secrets("the daemon log", &w.logs());
    w.until("tasks are back to where they were", || {
        w.metrics()["tasks_alive"].as_u64().unwrap() <= tasks0 + 5
    });
    let rss1 = w.metrics()["rss_kib"].as_u64().unwrap();
    assert!(rss1 < rss0 + 100 * 1024, "rss grew from {rss0} to {rss1} KiB");
}
