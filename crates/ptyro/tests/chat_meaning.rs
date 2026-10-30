//! CHAT5's meaning search end to end (SPEC §22.10 tier 3), through the CLI against a real daemon
//! whose agent is the fake (it echoes what it is told) and whose model is a tiny synthetic one in
//! the real one's layout: nothing is downloaded (`PTYRO_MEANING_URL` points nowhere).

use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

fn config(meaning: bool) -> String {
    format!(
        "[chat]\nmeaning = {meaning}\ntitle_command = [\"/bin/sh\", \"-c\", \"cat >/dev/null; echo Titled\"]\n\
         [agents.claude]\ncommand = [\"{BIN}\", \"fake-agent\", \"--as\", \"claude\"]\n"
    )
}

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn cli(&self, args: &[&str]) -> Output {
        let h = self.home.path();
        self.d.cmd().args(args).env("HOME", h).current_dir(h).output().unwrap()
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

    fn hits(&self, args: &[&str]) -> Vec<Value> {
        let mut a = vec!["chat", "search"];
        a.extend(args);
        a.push("--json");
        let out = self.ok(&a);
        serde_json::from_str::<Value>(&out).unwrap().as_array().cloned().unwrap_or_default()
    }

    fn until(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !f() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn titles(hits: &[Value]) -> Vec<String> {
    hits.iter().map(|h| h["chat"]["title"].as_str().unwrap_or_default().to_owned()).collect()
}

/// Cats near kittens, compilers near slowness, nothing near both.
fn model(dir: &Path) {
    ptyro_meaning::synthetic::write(
        dir,
        &[
            ("cat", &[1.0, 0.0, 0.0]),
            ("kitten", &[0.95, 0.2, 0.0]),
            ("compiler", &[0.0, 1.0, 0.0]),
            ("slow", &[0.0, 0.8, 0.4]),
        ],
    )
    .unwrap();
}

#[test]
fn meaning_search_finds_what_words_dont() {
    let home = tempfile::Builder::new().prefix("ptyro-meaning-").tempdir().unwrap();
    let hs = home.path().canonicalize().unwrap().to_string_lossy().into_owned();
    let env = [
        ("HOME", hs.as_str()),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("SHELL", "/bin/sh"),
        ("PTYRO_MEANING_URL", "file:///nonexistent/ptyro-meaning-test"),
    ];
    let d = TempDaemon::start_with(BIN, &env, Some(&config(false))).unwrap();
    let w = World { home, d };
    w.ok(&["chat", "new", "--title", "Pets", "my kitten sleeps on the sofa"]);
    w.ok(&["chat", "new", "--title", "Builds", "the compiler is slow today"]);

    // Off: words only, and asking for meaning says how to switch it on (no terminal to ask).
    assert!(w.hits(&["cat"]).is_empty());
    let out = w.cli(&["chat", "search", "cat", "--meaning"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("meaning search is off") && err.contains("chat.meaning true"), "{err}");

    // On, with its model in place: every message is embedded in the background.
    model(&w.d.paths.data.join("models").join(ptyro_meaning::NAME));
    w.d.write_config(&config(true)).unwrap();
    w.until("embedded", || {
        let out = w.cli(&["chat", "search", "cat", "--meaning", "--json"]);
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("kitten")
    });
    let cat = w.hits(&["cat", "--meaning"]);
    assert!(titles(&cat).iter().all(|t| t == "Pets"), "{cat:?}");
    assert!(cat.iter().all(|h| h["how"] == "meaning"), "{cat:?}");
    let user = cat.iter().find(|h| h["kind"] == "user").expect("the message itself");
    assert_eq!(user["snippet"], "my kitten sleeps on the sofa");
    // Both ways: the keyword hits first, marked as found both ways; the CLI marks meaning's ≈.
    let both = w.hits(&["kitten"]);
    assert_eq!(both[0]["how"], "both", "{both:?}");
    assert!(w.ok(&["chat", "search", "cat"]).contains("≈ my kitten"));
    // Words only: no cat in any message.
    assert!(!w.cli(&["chat", "search", "cat", "--words"]).status.success());
    // Every filter still holds: a word left out, and a search scoped to another chat.
    assert!(w.hits(&["cat", "-sofa", "--meaning"]).is_empty());
    assert!(w.hits(&["slow", "--meaning"]).iter().all(|h| h["chat"]["title"] == "Builds"));

    // A new message is embedded once written; an incognito chat never is.
    let builds = w
        .hits(&["compiler"])
        .first()
        .map(|h| format!("c:{}", h["chat"]["handle"].as_str().unwrap()))
        .unwrap();
    w.ok(&["chat", "send", &builds, "a kitten walked over the keyboard"]);
    w.until("the new message embedded", || {
        titles(&w.hits(&["cat", "--meaning"])).iter().any(|t| t == "Builds")
    });
    w.ok(&["chat", "new", "--incognito", "a cat secret"]);
    assert!(
        w.hits(&["cat", "--meaning"]).iter().all(|h| h["chat"]["incognito"] != true),
        "incognito chats are never embedded"
    );

    // Doctor says it is ready.
    w.until("doctor says ready", || {
        String::from_utf8_lossy(&w.cli(&["doctor", "--quick"]).stdout)
            .contains("meaning search: ready")
    });
}
