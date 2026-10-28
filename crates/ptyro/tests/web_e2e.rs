//! The web app in a real browser (SPEC §14): headless Chrome, driven over the DevTools protocol by
//! `web/test/e2e.mjs`, against a real daemon and a real `ptyro serve`. Needs Node and a Chrome
//! (`CHROME`, or the usual names); without them the test says so and passes, unless
//! `PTYRO_REQUIRE_BROWSER=1` (CI sets it where Chrome is installed).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.is_file()))
}

fn chrome() -> Option<PathBuf> {
    if let Some(c) = std::env::var_os("CHROME") {
        return Some(c.into());
    }
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]
        .iter()
        .find_map(|n| on_path(n))
        .or_else(|| {
            let mac = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
            mac.is_file().then_some(mac)
        })
}

fn serve(d: &TempDaemon, home: &Path, port: u16) -> Child {
    let child = d
        .cmd()
        .args(["serve", "--loopback-http", "--port", &port.to_string()])
        .env("HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "ptyro serve never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}

fn stop(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

#[test]
fn the_web_app_works_in_a_real_browser() {
    let required = std::env::var_os("PTYRO_REQUIRE_BROWSER").is_some_and(|v| v == "1");
    let (Some(node), Some(chrome)) = (on_path("node"), chrome()) else {
        assert!(!required, "PTYRO_REQUIRE_BROWSER=1, but Node or Chrome is missing");
        eprintln!("skipped: needs Node and Chrome (set CHROME)");
        return;
    };

    let home = tempfile::Builder::new().prefix("ptyro-e2e-").tempdir().unwrap();
    let hp = home.path().canonicalize().unwrap();
    std::fs::write(hp.join(".gitconfig"), "[user]\n\tname = e2e\n\temail = e2e@example.com\n")
        .unwrap();
    let hs = hp.to_string_lossy().into_owned();
    let config = format!(
        "[attention]\nmin_work = \"1s\"\n[agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n[ui]\n# e2e: keep this comment\n"
    );
    let d =
        TempDaemon::start_with(BIN, &[("HOME", hs.as_str()), ("SHELL", "/bin/sh")], Some(&config))
            .unwrap();

    let repo = hp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in
        [&["init", "-q", "-b", "main"][..], &["commit", "-q", "--allow-empty", "-m", "init"]]
    {
        let st =
            Command::new("git").args(args).current_dir(&repo).env("HOME", &hp).status().unwrap();
        assert!(st.success(), "git {args:?}");
    }
    let cli = |args: &[&str]| {
        let out = d.cmd().args(args).env("HOME", &hp).current_dir(&repo).output().unwrap();
        assert!(out.status.success(), "ptyro {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    cli(&["project", "add", repo.to_str().unwrap()]);

    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut server = Some(serve(&d, &hp, port));
    let pair: serde_json::Value =
        serde_json::from_str(&cli(&["pair", "e2e", "--scope", "full", "--json"])).unwrap();
    let code = pair["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();

    let env: serde_json::Map<String, serde_json::Value> = d
        .env()
        .into_iter()
        .map(|(k, v)| (k, v.into()))
        .chain([("HOME".to_owned(), hs.clone().into())])
        .collect();
    let e2e = serde_json::json!({
        "bin": BIN, "env": env, "repo": repo, "config": d.paths.config.join("config.toml"),
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/test/e2e.mjs");
    let mut js = Command::new(node)
        .arg(script)
        .args([chrome.to_str().unwrap(), &format!("http://127.0.0.1:{port}"), &code])
        .env("E2E", e2e.to_string())
        .env_remove("PTYRO_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut to_js = js.stdin.take().unwrap();
    for line in BufReader::new(js.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        eprintln!("{line}");
        match line.as_str() {
            "@@serve down" => {
                stop(server.take().unwrap());
                writeln!(to_js, "done").unwrap();
            }
            "@@serve up" => {
                server = Some(serve(&d, &hp, port));
                writeln!(to_js, "done").unwrap();
            }
            _ => {}
        }
    }
    let status = js.wait().unwrap();
    if let Some(s) = server {
        stop(s);
    }
    assert!(status.success(), "a browser scenario failed (output above)");
}
