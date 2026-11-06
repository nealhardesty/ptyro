//! WIN11 on Windows itself: `ptyro serve` speaks HTTPS with its own CA, pairs a device, and keeps
//! its private keys to the user even in a folder others can read; the server a daemon starts
//! leaves when that daemon is gone (Windows has no parent to watch); and two native daemons link
//! as machines (MM1) over ssh. CI's `windows` job runs these (`make test-windows`).
#![cfg(windows)]

use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ptyro_core::model::{MachineState, MachineTransport};
use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(40);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn wait_listening(port: u16) {
    let deadline = Instant::now() + WAIT;
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An HTTPS request to 127.0.0.1:`port` trusting only `ca_pem`, as a phone that installed it.
async fn https(port: u16, ca_pem: &str, req: &str) -> String {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut s = tokio_rustls::TlsConnector::from(Arc::new(client))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("a certificate ptyro's CA vouches for");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    String::from_utf8_lossy(&out).into_owned()
}

/// `icacls`, which must succeed.
fn icacls(args: &[&str]) {
    let o = Command::new("icacls.exe").args(args).output().unwrap();
    assert!(o.status.success(), "icacls {args:?}: {}", String::from_utf8_lossy(&o.stdout));
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_speaks_https_pairs_and_keeps_its_keys_private() {
    let d = TempDaemon::start(BIN).unwrap();
    // A home everyone may read (a folder shared on purpose): the keys still mustn't be.
    let home = d.home.path().display().to_string();
    icacls(&[&home, "/grant", "*S-1-1-0:(OI)(CI)R"]);
    let port = free_port();
    let _serve = Killed(
        d.cmd()
            .args(["serve", "--access", "localhost", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_listening(port);
    let tls = d.paths.data.join("serve").join("tls");
    let ca_pem = std::fs::read_to_string(tls.join("ca.pem")).expect("the CA, made on first use");
    let r = https(
        port,
        &ca_pem,
        "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");

    // The certificate is public, the keys are the user's (and SYSTEM's and Administrators').
    assert!(
        ptyro_pty::acl::readable_by_others(&tls.join("ca.pem")).is_some(),
        "the folder didn't let others read: this test would prove nothing"
    );
    for key in ["ca-key.pem", "server-key.pem"] {
        assert_eq!(ptyro_pty::acl::readable_by_others(&tls.join(key)), None, "{key}");
    }

    // A device pairs: the link is this server's, its code is traded once for a token.
    let out = d.cmd().args(["pair", "phone", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("https://127.0.0.1:{port}/")), "{v}");
    let code = url.split("#pair=").nth(1).unwrap();
    let body = serde_json::json!({ "code": code }).to_string();
    let req = format!(
        "POST /api/pair HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let r = https(port, &ca_pem, &req).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.contains("\"token\""), "{r}");
    let again = https(port, &ca_pem, &req).await;
    assert!(!again.starts_with("HTTP/1.1 200"), "a code works once: {again}");
}

/// The web server a daemon starts (`[remote] serve = true`) leaves when the daemon is killed:
/// it would hold the port for good, and the next daemon's server couldn't listen.
#[test]
fn the_daemons_web_server_leaves_with_it() {
    let port = free_port();
    let config = format!("[remote]\nserve = true\naccess = \"localhost\"\nport = {port}\n");
    let d = TempDaemon::start_with(BIN, &[], Some(&config)).unwrap();
    wait_listening(port);
    let record = d.paths.state.join("serve.json");
    let deadline = Instant::now() + WAIT;
    let serve_pid = loop {
        let pid = std::fs::read_to_string(&record)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["pid"].as_u64());
        if let Some(p) = pid {
            break u32::try_from(p).unwrap();
        }
        assert!(Instant::now() < deadline, "serve never wrote {}", record.display());
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = d.cmd().args(["daemon", "status", "--json"]).output().unwrap();
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    let daemon_pid = status["pid"].as_u64().unwrap().to_string();
    // Killed, not stopped: nothing gets to tell the server.
    let k = Command::new("taskkill.exe").args(["/F", "/PID", &daemon_pid]).output().unwrap();
    assert!(k.status.success(), "{}", String::from_utf8_lossy(&k.stdout));
    assert!(
        ptyro_pty::wait_for_exit(serve_pid, Duration::from_secs(15)),
        "ptyro serve ({serve_pid}) outlived its daemon"
    );
}

/// A script standing in for ssh (a `.cmd`: Windows has no `sh`), bridging to `to`'s daemon
/// whatever it is asked.
fn fake_ssh(dir: &std::path::Path, name: &str, to: &[(String, String)]) -> String {
    let path = dir.join(format!("{name}.cmd"));
    let mut s = String::from("@echo off\r\n");
    for (k, v) in to {
        s.push_str(&format!("set \"{k}={v}\"\r\n"));
    }
    s.push_str(&format!("set \"PTYRO_NO_AUTOSPAWN=1\"\r\n\"{BIN}\" proxy\r\n"));
    std::fs::write(&path, s).unwrap();
    format!("cmd.exe /d /c {}", path.display())
}

/// MM1 between two native daemons: the first adds the second as a machine over ssh, sees it
/// online, and lists its sessions beside its own.
#[tokio::test(flavor = "multi_thread")]
async fn two_native_daemons_link_as_machines() {
    let there =
        TempDaemon::start_with(BIN, &[], Some("[daemon]\nmachine_name = \"winbox2\"\n")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(dir.path(), "ssh-to-winbox2", &there.env());
    let here = TempDaemon::start_with(
        BIN,
        &[("PTYRO_SSH", ssh.as_str())],
        Some("[daemon]\nmachine_name = \"winbox1\"\n"),
    )
    .unwrap();
    let t = there.client().await.unwrap();
    let spec = ptyro_core::session::SpawnSpec {
        argv: vec!["cmd.exe".into(), "/d".into(), "/k".into(), "echo ready".into()],
        cwd: there.home.path().to_owned(),
        name: Some("over-there".into()),
        ..Default::default()
    };
    let Reply::Session { info: theirs } = t.request(Request::Spawn { spec }).await.unwrap() else {
        panic!("spawn")
    };
    let h = here.client().await.unwrap();
    let added = h
        .request(Request::MachineAdd {
            name: None,
            transport: MachineTransport::Ssh { target: "winbox2".into() },
            token: None,
            force: false,
        })
        .await
        .unwrap();
    let Reply::Machine { info } = added else { panic!("{added:?}") };
    assert_eq!(info.name, "winbox2");
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Machines { machines } = h.request(Request::MachineList).await.unwrap() else {
            panic!("machines")
        };
        if machines.iter().any(|m| m.name == "winbox2" && m.state == MachineState::Online) {
            break;
        }
        assert!(Instant::now() < deadline, "winbox2 never came online: {machines:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Sessions { sessions } = h.request(Request::List).await.unwrap() else {
            panic!("list")
        };
        if let Some(s) = sessions.iter().find(|s| s.id == theirs.id) {
            assert_eq!(s.machine.as_deref(), Some("winbox2"));
            assert_eq!(s.name, "winbox2:over-there");
            break;
        }
        assert!(Instant::now() < deadline, "winbox2's session never listed here");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// MM1 over `machine-bridge` (a WebSocket to the other machine's `ptyro serve`, no remote
/// shell), as the roadmap has it for Windows: `machine pair` on one prints a code, `machine add
/// CODE` on the other claims it, and the link comes online.
#[test]
fn a_native_daemon_links_to_another_over_its_web_server() {
    let there =
        TempDaemon::start_with(BIN, &[], Some("[daemon]\nmachine_name = \"winbox4\"\n")).unwrap();
    let port = free_port();
    let _serve = Killed(
        there
            .cmd()
            .args(["serve", "--access", "localhost", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_listening(port);
    let out = there.cmd().args(["machine", "pair", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let code =
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["code"].as_str().unwrap().to_owned();
    let here =
        TempDaemon::start_with(BIN, &[], Some("[daemon]\nmachine_name = \"winbox3\"\n")).unwrap();
    let out = here.cmd().args(["machine", "add", &code]).output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = Instant::now() + WAIT;
    loop {
        let out = here.cmd().args(["machine", "ls", "--json"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let online = serde_json::from_str::<Value>(&text).ok().is_some_and(|v| {
            v.as_array()
                .into_iter()
                .flatten()
                .any(|m| m["name"] == "winbox4" && m["state"] == "online")
        });
        if online {
            break;
        }
        assert!(Instant::now() < deadline, "winbox4 never came online: {text}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
