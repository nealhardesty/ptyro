use ptyro_store::Registry;

use super::*;

const DAY: &str = "2026-11-20";

fn set_config(d: &Daemon, toml: &str) {
    *d.config.write().unwrap() = ptyro_core::config::Config::parse(toml).unwrap();
    d.kb_output_switch();
}

fn job(text: &str, output: Option<&str>) -> ShellJob {
    ShellJob {
        uri: shell::uri("s1", DAY),
        title: format!("zsh · {DAY}"),
        stamp: Stamp::new(Writer::User, None),
        facts: Facts { shell: true, ..Facts::default() },
        ran: Ran {
            text: text.into(),
            cwd: "/work".into(),
            started_ms: ptyro_core::now_ms() - 2_000,
            ended_ms: ptyro_core::now_ms(),
            exit: Some(0),
            output: output.map(Into::into),
        },
    }
}

async fn kept(d: &Daemon) -> Vec<(ChunkKind, String, u64)> {
    let db = d.kb_db().unwrap();
    let Some(item) = db.item_by_uri(shell::uri("s1", DAY)).await.unwrap() else {
        return Vec::new();
    };
    assert_eq!((item.kind, item.title.as_deref()), (ItemKind::Shell, Some("zsh · 2026-11-20")));
    db.chunks(item.id, 0, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.kind, c.text, shell::count(c.meta.as_ref())))
        .collect()
}

#[tokio::test]
async fn a_command_is_one_chunk_that_counts_and_its_secrets_are_redacted() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    d.start_kb();
    assert!(!d.io.kb_output.load(Ordering::Relaxed), "output is not collected unless asked for");

    d.kb_shell(job("make test", None)).await.unwrap();
    d.kb_shell(job("make test", None)).await.unwrap();
    // The same request with another token is the same line once it is stored.
    for token in ["abcDEF123456ghiJKL7890", "zyxWVU987654tsrQPO3210"] {
        let line = format!("curl -H \"Authorization: Bearer {token}\" https://api.example.com/v1");
        d.kb_shell(job(&line, None)).await.unwrap();
    }
    d.kb_shell(job("export FOO_KEY=zqkeyvalue GITHUB_TOKEN=ghp_abcdefghij0123456789", None))
        .await
        .unwrap();
    d.kb_shell(job("mysql -u root -phunter2secret9 shop", None)).await.unwrap();
    let all = kept(&d).await;
    assert_eq!(all.len(), 4, "{all:?}");
    assert_eq!(all[3].1, "$ mysql -u root -p<redacted> shop");
    assert_eq!(all[0], (ChunkKind::Command, "$ make test".into(), 2));
    assert_eq!(all[1].2, 2, "{all:?}");
    let text: String = all.iter().map(|c| c.1.as_str()).collect::<Vec<_>>().join("\n");
    assert!(text.contains("Authorization: Bearer <redacted>"), "{text}");
    assert!(text.contains("https://api.example.com/v1"), "{text}");
    for secret in ["abcDEF123456", "zyxWVU987654", "ghp_abcdefghij", "hunter2secret9", "zqkeyvalue"]
    {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    // The setting says commands: what one printed is not kept, even if it was handed over.
    d.kb_shell(job("cargo build", Some("Compiling ptyro"))).await.unwrap();
    assert!(kept(&d).await.iter().all(|c| c.0 == ChunkKind::Command));
}

#[tokio::test]
async fn output_is_kept_only_when_asked_for_stands_beside_its_command_and_ages_out() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    set_config(&d, "[kb]\nshell = \"output\"\nshell_output_days = 30\n");
    d.start_kb();
    assert!(d.io.kb_output.load(Ordering::Relaxed));

    d.kb_shell(job("cargo build", Some("Compiling ptyro\npassword=hunter2secret9"))).await.unwrap();
    d.kb_shell(job("make", Some("first run"))).await.unwrap();
    d.kb_shell(job("make", Some("second run"))).await.unwrap();
    let all = kept(&d).await;
    let shown: Vec<_> = all.iter().map(|c| (c.0, c.1.as_str())).collect();
    assert_eq!(
        shown,
        [
            (ChunkKind::Command, "$ cargo build"),
            (ChunkKind::Output, "Compiling ptyro\npassword=<redacted>"),
            (ChunkKind::Command, "$ make"),
            // (The last run's: a hundred `make` are one row.)
            (ChunkKind::Output, "second run"),
        ]
    );
    // A run that printed nothing leaves no output behind.
    d.kb_shell(job("make", None)).await.unwrap();
    assert_eq!(kept(&d).await.len(), 3);

    // Nothing is old yet; with a day's retention and output a month old, it goes and the
    // command stays.
    let db = d.kb_db().unwrap();
    assert_eq!(d.kb_shell_output_pass(&db).await, 0);
    let mut old = job("old build", Some("printed long ago"));
    old.ran.started_ms -= 40 * 86_400_000;
    old.ran.ended_ms -= 40 * 86_400_000;
    d.kb_shell(old).await.unwrap();
    assert!(kept(&d).await.iter().any(|c| c.1 == "printed long ago"));
    assert_eq!(d.kb_shell_output_pass(&db).await, 1);
    let all = kept(&d).await;
    assert!(all.iter().any(|c| c.1 == "$ old build"), "{all:?}");
    assert!(!all.iter().any(|c| c.1 == "printed long ago"), "{all:?}");
    set_config(&d, "[kb]\nshell = \"output\"\nshell_output_days = 0\n");
    assert_eq!(d.kb_shell_output_pass(&db).await, 0, "0 keeps it as long as its command");

    // Switched off: the emulators stop collecting, and the gate refuses what still arrives.
    set_config(&d, "[kb]\nshell = \"off\"\n");
    assert!(!d.io.kb_output.load(Ordering::Relaxed));
    let n = kept(&d).await.len();
    match d.kb_shell(job("never kept", None)).await {
        Err(Refused::Skip(ptyro_core::kb::ingest::Skip::ShellsOff)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(kept(&d).await.len(), n);
}

#[tokio::test]
async fn a_forgotten_day_keeps_nothing_more() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    d.start_kb();
    d.kb_shell(job("terraform apply", None)).await.unwrap();
    let db = d.kb_db().unwrap();
    let item = db.item_by_uri(shell::uri("s1", DAY)).await.unwrap().unwrap();
    db.forget(item.id.clone()).await.unwrap();
    match d.kb_shell(job("terraform destroy", None)).await {
        Err(Refused::Skip(ptyro_core::kb::ingest::Skip::Forgotten)) => {}
        other => panic!("{other:?}"),
    }
    assert!(db.chunks(item.id, 0, 0).await.unwrap().is_empty());
}
