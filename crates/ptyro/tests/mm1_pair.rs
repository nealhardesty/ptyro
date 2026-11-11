//! MM1.B pairing (SPEC §16.4b): several machines pair with one, each with a code of its own,
//! over its real `ptyro serve`, and every link stays up. A code is spent only by the claim that
//! keeps it: whatever would refuse the machine is asked before it is claimed.

use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct Killed(Child);
impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn named(name: &str) -> TempDaemon {
    TempDaemon::start_with(BIN, &[], Some(&format!("[daemon]\nmachine_name = \"{name}\"\n")))
        .unwrap()
}

/// (succeeded, stdout + stderr)
fn run(d: &TempDaemon, args: &[&str]) -> (bool, String) {
    let out = d.cmd().args(args).output().unwrap();
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

fn json(d: &TempDaemon, args: &[&str]) -> Value {
    let out = d.cmd().args(args).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{args:?}: {err}");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {err}"))
}

fn code(host: &TempDaemon) -> String {
    let out = host.cmd().args(["machine", "pair", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<Value>(&out.stdout).unwrap()["code"].as_str().unwrap().to_owned()
}

fn state(d: &TempDaemon, machine: &str) -> String {
    let v = json(d, &["machine", "ls", "--json"]);
    v.as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"] == machine)
        .map(|m| m["state"].as_str().unwrap_or_default().to_owned())
        .unwrap_or_default()
}

fn until_online(d: &TempDaemon, machine: &str) {
    let deadline = Instant::now() + WAIT;
    while state(d, machine) != "online" {
        assert!(Instant::now() < deadline, "{machine} never came online: {}", state(d, machine));
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The host's devices: (name, pending, machine).
fn devices(host: &TempDaemon) -> Vec<(String, bool, bool)> {
    let v = json(host, &["devices", "ls", "--json"]);
    v.as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            (
                d["name"].as_str().unwrap_or_default().to_owned(),
                d["pending"].as_bool().unwrap_or(false),
                d["machine"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

fn waiting_codes(host: &TempDaemon) -> usize {
    devices(host).iter().filter(|(_, pending, machine)| *pending && *machine).count()
}

#[test]
fn two_machines_pair_with_one_and_both_stay_linked() {
    let turkey = named("turkey");
    let port = free_port();
    let _serve = Killed(
        turkey
            .cmd()
            .args(["serve", "--loopback-http", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (bear, walrus) = (named("bear"), named("walrus"));

    // Two codes made before either is used: the second doesn't cancel the first.
    let (for_bear, for_walrus) = (code(&turkey), code(&turkey));
    assert_eq!(waiting_codes(&turkey), 2);

    // Its own code, run on the machine that made it, is refused and not spent.
    let (ok, text) = run(&turkey, &["machine", "add", &for_bear]);
    assert!(!ok && text.contains("this machine's own"), "{text}");
    assert_eq!(waiting_codes(&turkey), 2, "a refused add spends nothing");

    let (ok, text) = run(&bear, &["machine", "add", &for_bear]);
    assert!(ok, "{text}");
    until_online(&bear, "turkey");
    let (ok, text) = run(&walrus, &["machine", "add", &for_walrus]);
    assert!(ok, "{text}");
    until_online(&walrus, "turkey");

    // Each is a device of its own, named after itself; the second replaced nothing.
    let paired: Vec<String> =
        devices(&turkey).into_iter().filter(|d| d.2 && !d.1).map(|d| d.0).collect();
    assert_eq!(paired.len(), 2, "{paired:?}");
    assert!(paired.contains(&"bear".into()) && paired.contains(&"walrus".into()), "{paired:?}");
    let (_, listed) = run(&turkey, &["devices", "ls"]);
    assert!(listed.contains("[machine link]"), "{listed}");
    // bear's link outlives walrus's pairing (it used to be cut off: one `machine` device).
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(state(&bear, "turkey"), "online");

    // Adding it again is refused before the code is claimed: the code still works after.
    let spare = code(&turkey);
    let (ok, text) = run(&bear, &["machine", "add", &spare]);
    assert!(!ok && text.contains("already added"), "{text}");
    let (ok, text) = run(&bear, &["machine", "add", &spare, "--name", "other"]);
    assert!(!ok && text.contains("already added as turkey"), "{text}");
    assert_eq!(waiting_codes(&turkey), 1, "the spare wasn't spent");

    // A spent code says what happened and what to do.
    let (ok, text) = run(&bear, &["machine", "rm", "turkey", "--yes"]);
    assert!(ok, "{text}");
    let (ok, text) = run(&bear, &["machine", "add", &for_bear]);
    assert!(!ok, "{text}");
    assert!(text.contains("already used") && text.contains("machine pair` on turkey"), "{text}");

    // Paired again, it is a device of its own beside the old one, which stays until revoked.
    let (ok, text) = run(&bear, &["machine", "add", &spare]);
    assert!(ok, "{text}");
    until_online(&bear, "turkey");
    let names: Vec<String> = devices(&turkey).into_iter().filter(|d| d.2).map(|d| d.0).collect();
    assert!(names.contains(&"bear-2".into()) && names.contains(&"walrus".into()), "{names:?}");
    assert_eq!(state(&walrus, "turkey"), "online");

    // A person's device can't take a machine link's name (revoking by name stays one device).
    let (ok, text) = run(&turkey, &["pair", "walrus", "--json"]);
    assert!(!ok && text.contains("another machine's link"), "{text}");
    // Only so many machine codes wait at once: a newer one retires the oldest.
    for _ in 0..10 {
        code(&turkey);
    }
    assert_eq!(waiting_codes(&turkey), 8);
    assert_eq!(state(&bear, "turkey"), "online");

    // Revoked there, the link here says so and how to pair again, not "is it awake?".
    let (ok, text) = run(&turkey, &["devices", "revoke", "walrus", "--yes"]);
    assert!(ok, "{text}");
    let deadline = Instant::now() + WAIT;
    loop {
        let v = json(&walrus, &["machine", "ls", "--json"]);
        let fix = v[0]["fix"].as_str().unwrap_or_default().to_owned();
        if fix.contains("no longer accepts this machine's pairing") {
            break;
        }
        assert!(Instant::now() < deadline, "no fix for a revoked link: {v}");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(state(&bear, "turkey"), "online", "only walrus was revoked");
}

#[test]
fn machines_that_share_a_name_still_pair_under_another() {
    let host = named("ubuntu");
    let port = free_port();
    let _serve = Killed(
        host.cmd()
            .args(["serve", "--loopback-http", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let twin = named("ubuntu");
    let c = code(&host);
    // Same name, but not its own code: it says to name it, and the code isn't spent.
    let (ok, text) = run(&twin, &["machine", "add", &c]);
    assert!(!ok && text.contains("--name"), "{text}");
    assert_eq!(waiting_codes(&host), 1);
    let (ok, text) = run(&twin, &["machine", "add", &c, "--name", "ubuntu-2"]);
    assert!(ok, "{text}");
    until_online(&twin, "ubuntu-2");
}
