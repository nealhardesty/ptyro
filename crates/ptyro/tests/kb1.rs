//! KB1 end to end (SPEC §25): the knowledge base's file as a daemon keeps it. KB1.1: `kb.db` is
//! made when the daemon starts, private; `ptyro doctor`, `ptyro about` and the uninstall plan
//! name it; it is closed before a live upgrade's exec and opened again after; switched off, it
//! is not made, and a file that can't be opened is a reason with its fix while the daemon runs.

use std::path::PathBuf;
use std::process::Output;

use ptyro_core::kb::ingest::{self, Body, Gate, Never, Source, Stamp};
use ptyro_core::kb::redact::Redactor;
use ptyro_core::kb::{ItemKind, Writer};
use ptyro_store::kb::KbDb;
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new(config: &str) -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-kb1-").tempdir().unwrap();
        let hs = ptyro_core::os::canonicalize(home.path()).unwrap().to_string_lossy().into_owned();
        let env = [("HOME", hs.as_str()), ("SHELL", "/bin/sh")];
        let d = TempDaemon::start_with(BIN, &env, Some(config)).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        ptyro_core::os::canonicalize(self.home.path()).unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.d.cmd().args(args).env("HOME", self.home()).current_dir(self.home()).output().unwrap()
    }

    fn out(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    }

    fn kb_db(&self) -> PathBuf {
        self.d.home.path().join("data/kb.db")
    }

    /// `ptyro doctor`'s line about the knowledge base.
    fn doctor_line(&self) -> String {
        let all = self.out(&["doctor", "--quick"]);
        all.lines()
            .find(|l| l.contains("knowledge base"))
            .unwrap_or_else(|| panic!("no knowledge base line in:\n{all}"))
            .to_owned()
    }
}

/// A document written into `kb.db` from outside the daemon (nothing in KB1.1 to KB1.3 gives a
/// client a way in yet), through the same plan and writer the daemon uses.
async fn write_doc(path: &std::path::Path, uri: &str, text: &str) {
    let db = KbDb::open(path, None).unwrap();
    let never = Never::default();
    let passed = ingest::gate(&Gate::open(&never)).unwrap();
    let source = Source::new(
        ItemKind::File,
        uri,
        Body::Markdown { name: "a.md".into(), text: text.into(), at_ms: 1 },
    );
    let plan =
        ingest::plan(passed, source, Stamp::new(Writer::File, None), &Redactor::default()).unwrap();
    db.write(plan).await.unwrap();
    db.checkpoint().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_keeps_kb_db_and_doctor_about_and_uninstall_name_it() {
    let w = World::new("");
    let db = w.kb_db();
    // (The socket takes a connection before the daemon has finished starting; an answer
    // means it has.)
    let pid = w.out(&["daemon", "status", "--json"]);
    assert!(db.exists(), "made when the daemon starts, before anything asks for it");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&db).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let line = w.doctor_line();
    assert!(
        line.contains("knowledge base: nothing yet in 0 piece(s), nothing forgotten"),
        "{line}"
    );
    assert!(line.contains("kb.db (schema 1)"), "{line}");

    let about = w.out(&["about"]);
    assert!(about.lines().any(|l| l.starts_with("knowledge") && l.ends_with("kb.db")), "{about}");
    let json: serde_json::Value = serde_json::from_str(&w.out(&["about", "--json"])).unwrap();
    let place =
        json["places"].as_array().unwrap().iter().find(|p| p["name"] == "knowledge").unwrap();
    assert_eq!(place["exists"], true);

    let plan = w.out(&["uninstall", "--dry-run"]);
    assert!(
        plan.contains("knowledge     what the knowledge base kept of your work, kb.db ("),
        "{plan}"
    );
    assert!(db.exists(), "a dry run removes nothing");

    // Across a live upgrade: the same file, opened again by the new image.
    write_doc(&db, "file:///repo/a.md", "# A\n\nkept across the upgrade").await;
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: 1 file in 1 piece(s)"), "{line}");
    let up = w.cli(&["daemon", "upgrade"]);
    assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));
    let after = w.out(&["daemon", "status", "--json"]);
    let field = |s: &str, k: &str| serde_json::from_str::<serde_json::Value>(s).unwrap()[k].clone();
    assert_eq!(field(&pid, "pid"), field(&after, "pid"), "an upgrade in place");
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: 1 file in 1 piece(s)"), "{line}");
    // And the new image's writer has it: another write from outside is seen at once.
    write_doc(&db, "file:///repo/b.md", "# B\n\nwritten after").await;
    assert!(w.doctor_line().contains("knowledge base: 2 files in 2 piece(s)"));
}

#[test]
fn switched_off_it_is_not_made_and_a_file_that_cannot_be_opened_says_why() {
    let mut w = World::new("[kb]\nenabled = false\n");
    let line = w.doctor_line();
    assert!(line.contains("knowledge base: off ([kb] enabled = false)"), "{line}");
    assert!(!w.kb_db().exists(), "nothing is made while it is off");
    let plan = w.out(&["uninstall", "--dry-run"]);
    assert!(!plan.contains("kb.db"), "{plan}");

    // Switched on over a file that is no database: the daemon runs, sessions start as ever,
    // and the reason comes with what to do. The file is left as it was.
    std::fs::write(w.kb_db(), "not a database at all, just some text").unwrap();
    w.d.write_config("").unwrap();
    w.d.restart().unwrap();
    let o = w.cli(&["new", "-d", "-n", "plain", "--", "sh", "-c", "sleep 30"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let line = w.doctor_line();
    assert!(line.contains("the knowledge base is unavailable"), "{line}");
    assert!(line.contains("is damaged: move it aside"), "{line}");
    assert_eq!(
        std::fs::read_to_string(w.kb_db()).unwrap(),
        "not a database at all, just some text"
    );
}
