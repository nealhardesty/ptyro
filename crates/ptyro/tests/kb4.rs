//! KB4 end to end (SPEC §25.6.5): what the knowledge base keeps of a shell, under a real
//! daemon. KB4.1: a command line is kept when its command ends and found by a word of it;
//! what it printed is not; a command typed with a space before it, a trivial one and one
//! that names a credential file are not kept; the secrets a command line shows are taken
//! out; `kb.shell = "off"` keeps nothing; a private shell keeps nothing, and is still
//! private when a restart offers it back; `kb forget --session` forgets a shell.
//! KB4.2: with `kb.shell = "output"`, and only then, what a command printed is kept beside
//! it, its head and its tail; never what a command showed that was typed into (echoed or
//! not), and never what a full-screen program drew.
#![cfg(unix)]

use ptyro_core::kb::shell::count;
use ptyro_core::kb::{ChunkKind, ItemKind, KbChunk, Writer};
use ptyro_store::kb::ItemFilter;

mod kbkit;
use kbkit::*;

fn texts(chunks: &[KbChunk], kind: ChunkKind) -> Vec<&str> {
    chunks.iter().filter(|c| c.kind == kind).map(|c| c.text.as_str()).collect()
}

fn has(chunks: &[KbChunk], word: &str) -> bool {
    chunks.iter().any(|c| c.text.contains(word))
}

/// Wait until the daemon runs with `kb.shell` as `word`.
async fn shell_setting(w: &World, word: &str) {
    let o = w.cli(&["config", "set", "kb.shell", word]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    w.status(&format!("kb.shell = {word}"), |s| s["shell"] == word).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shells_commands_are_kept_and_found_and_what_they_print_is_not() {
    let w = World::marked("");
    let c = w.d.client().await.unwrap();
    let sh = w.shell(&c, "sh1", false).await;
    let id = sh.id.full();

    // "That long ffmpeg line": longer than a list shows of a command, kept whole.
    let long = format!(
        "ffmpeg -i input.mov -vf scale=1280:-2 -c:v libx264 -preset slow -crf 18 {} zqffmpegtail.mp4",
        "-metadata comment=padding ".repeat(12)
    );
    assert!(long.len() > 300);
    for line in [
        "printf 'zq%s\\n' outword",
        long.as_str(),
        // Never kept: a space before it, a trivial one, one that names a credential file.
        " echo zqspacedword",
        "ls",
        "cat .env.zqenvword 2>/dev/null",
        // Kept without the secrets they show.
        "curl -H \"Authorization: Bearer zqTok3nAbCdEfGh1234567890\" https://api.example.com/zqcurlword",
        "export FOO_KEY=zqexportedvalue1",
        "mysql -u root -pzqmysqlpass9 shop",
        "make zqrepeat",
        "false",
        "make zqrepeat",
        "true zqsentinel",
    ] {
        w.run(&c, "sh1", line).await;
    }
    let kept = w.shell_kept(&id, "kept the last command", |k| has(k, "zqsentinel")).await;
    let commands = texts(&kept, ChunkKind::Command);
    assert_eq!(
        commands,
        [
            "$ printf 'zq%s\\n' outword",
            &format!("$ {long}"),
            "$ curl -H \"Authorization: Bearer <redacted>\" https://api.example.com/zqcurlword",
            "$ export FOO_KEY=<redacted>",
            "$ mysql -u root -p<redacted> shop",
            "$ make zqrepeat",
            "$ false",
            "$ true zqsentinel",
        ]
    );
    assert_eq!(kept.len(), commands.len(), "commands only: nothing a command printed");
    for never in
        ["zqoutword", "zqspacedword", "zqenvword", "zqTok3n", "zqexportedvalue1", "zqmysqlpass9"]
    {
        assert!(!w.bytes_hold(never), "{never} is in kb.db");
    }
    // Where and how each ran; the same line twice is one row that counts.
    let make = kept.iter().find(|c| c.text == "$ make zqrepeat").unwrap();
    assert_eq!(count(make.meta.as_ref()), 2);
    let home = w.home().display().to_string();
    assert_eq!(make.heading.as_deref(), Some(format!("in {home} · 2 times").as_str()));
    let failed = kept.iter().find(|c| c.text == "$ false").unwrap();
    assert_eq!(failed.heading.as_deref(), Some(format!("in {home} · exit 1").as_str()));

    // One item for the shell and the day: yours, the shell's own session, its profile.
    let f = ItemFilter { kinds: vec![ItemKind::Shell], ..ItemFilter::default() };
    let items = w.db().list(f).await.unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert!(item.uri.starts_with(&format!("shell://{id}/20")), "{}", item.uri);
    assert_eq!((item.writer, item.session.as_deref()), (Writer::User, Some(id.as_str())));
    assert!(item.title.as_deref().is_some_and(|t| t.starts_with("sh1 · 20")), "{:?}", item.title);

    // Found by a word of it, read, listed; and the status says what is kept of shells.
    let found = w.out(&["recall", "zqffmpegtail"]);
    assert!(found.contains("command") && found.contains("zqffmpegtail"), "{found}");
    let found = w.out(&["recall", "--kind", "command", "zqcurlword"]);
    assert!(found.contains("Bearer <redacted>"), "{found}");
    assert!(w.cli(&["recall", "--kind", "session", "zqcurlword"]).stdout.is_empty());
    let recent = w.json(&["kb", "recent", "--kind", "shell", "--json"]);
    let addr = recent[0]["addr"].as_str().unwrap_or_else(|| panic!("{recent:#}")).to_owned();
    let shown = w.out(&["kb", "show", &addr]);
    assert!(shown.contains("$ make zqrepeat") && shown.contains("2 times"), "{shown}");
    let status = w.out(&["kb", "status"]);
    assert!(status.contains("shell commands are kept, not what they print"), "{status}");
    assert!(status.contains("1 shell"), "{status}");

    // Switched off: nothing more is kept, and what was kept stays.
    shell_setting(&w, "off").await;
    w.run(&c, "sh1", "true zqoffword").await;
    shell_setting(&w, "commands").await;
    w.run(&c, "sh1", "true zqonagain").await;
    let kept = w.shell_kept(&id, "kept a command again", |k| has(k, "zqonagain")).await;
    assert!(!has(&kept, "zqoffword") && has(&kept, "zqffmpegtail"));
    assert!(!w.bytes_hold("zqoffword"));

    // `kb forget --session` on a shell: what was kept goes for good, and it is private.
    let o = w.cli(&["kb", "forget", "--session", "sh1", "--yes"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(w.info(&c, "sh1").await.private);
    w.run(&c, "sh1", "true zqafterforget").await;
    // (Another shell's command is kept after it: the queue is in order.)
    let other = w.shell(&c, "sh2", false).await;
    w.run(&c, "sh2", "true zqothershell").await;
    w.shell_kept(&other.id.full(), "kept the other shell's", |k| has(k, "zqothershell")).await;
    assert!(w.shell_chunks(&id).await.is_empty());
    for gone in ["zqffmpegtail", "zqrepeat", "zqafterforget"] {
        assert!(!w.bytes_hold(gone), "{gone} is still in kb.db");
    }
    assert!(w.cli(&["recall", "zqffmpegtail"]).stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_command_printed_is_kept_only_when_asked_for_and_never_what_was_not_printed() {
    let w = World::marked("[kb]\nshell = \"output\"\n");
    let c = w.d.client().await.unwrap();
    let sh = w.shell(&c, "sh1", false).await;
    let id = sh.id.full();
    let status = w.out(&["kb", "status"]);
    assert!(status.contains("shell commands are kept, and what they print"), "{status}");

    w.run(&c, "sh1", "printf 'zq%s\\n' outword").await;
    // More than is kept of one command: its head, its tail, and how much is left out.
    w.run(&c, "sh1", "seq 1 5000").await;
    // A password: typed where nothing is echoed, so never printed. And a command that is
    // typed into at all keeps nothing it showed: here, not even what it printed afterwards.
    let before = w.info(&c, "sh1").await.command.map(|c| c.started_ms);
    w.keys(
        &c,
        "sh1",
        "stty -echo; printf 'zq%s\\n' asking; IFS= read -r pw; stty echo; echo zqgot-${#pw}",
    )
    .await;
    w.d.wait_for_text(&c, "sh1", "zqasking", WAIT).await.unwrap();
    w.keys(&c, "sh1", "zqnoechosecret").await;
    w.ended(&c, "sh1", before).await;
    // The same where what is typed is echoed (a prompt, a REPL).
    let before = w.info(&c, "sh1").await.command.map(|c| c.started_ms);
    w.keys(&c, "sh1", "printf 'zq%s\\n' prompting; IFS= read -r answer; echo zqsaid-$answer").await;
    w.d.wait_for_text(&c, "sh1", "zqprompting", WAIT).await.unwrap();
    w.keys(&c, "sh1", "zqechoedanswer").await;
    w.ended(&c, "sh1", before).await;
    // A full-screen program (the alternate screen): its command is kept, its screen is not.
    w.run(&c, "sh1", "printf '\\033[?1049hzq%s\\033[?1049l' altword").await;
    w.run(&c, "sh1", "true zqsentinel").await;

    let kept = w.shell_kept(&id, "kept the last command", |k| has(k, "zqsentinel")).await;
    assert_eq!(texts(&kept, ChunkKind::Command).len(), 6, "{kept:?}");
    let printed = |command: &str| -> String {
        let anchor = kept.iter().find(|c| c.text.starts_with(command)).unwrap().anchor.clone();
        kept.iter()
            .filter(|c| c.kind == ChunkKind::Output && c.anchor == anchor)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(printed("$ printf 'zq%s\\n' outword"), "zqoutword");
    let seq = printed("$ seq 1 5000");
    assert!(seq.starts_with("1\n2\n3\n") && seq.ends_with("4999\n5000"), "{seq}");
    assert!(seq.contains(" lines left out …"), "{seq}");
    assert!(seq.len() < 9 << 10, "{} bytes of 23893 printed", seq.len());
    assert_eq!(printed("$ stty -echo"), "", "a command that was typed into: {kept:?}");
    assert_eq!(printed("$ printf 'zq%s\\n' prompting"), "", "{kept:?}");
    for typed in ["zqnoechosecret", "zqgot-14", "zqechoedanswer", "zqprompting"] {
        assert!(!w.bytes_hold(typed), "{typed}: what a command typed into showed is in kb.db");
    }
    assert_eq!(printed("$ printf '\\033[?1049h"), "", "an alternate screen is never output");
    assert!(!w.bytes_hold("zqaltword"));
    assert_eq!(printed("$ true zqsentinel"), "");

    // Found by a word it printed.
    let found = w.out(&["recall", "zqoutword"]);
    assert!(found.contains("command") && found.contains("zqoutword"), "{found}");

    // A private shell keeps nothing: no command, no output, no item.
    let private = w.shell(&c, "sp", true).await;
    assert!(private.private);
    w.run(&c, "sp", "printf 'zq%s\\n' privprinted; true zqprivcommand").await;
    // Back to commands only: what the next command prints is not kept.
    shell_setting(&w, "commands").await;
    w.run(&c, "sh1", "printf 'zq%s\\n' laterword").await;
    w.run(&c, "sh1", "true zqsecond").await;
    let kept = w.shell_kept(&id, "kept the second sentinel", |k| has(k, "zqsecond")).await;
    assert!(has(&kept, "zqoutword"), "what was kept stays");
    assert!(w.shell_chunks(&private.id.full()).await.is_empty());
    for never in ["zqlaterword", "zqprivprinted", "zqprivcommand"] {
        assert!(!w.bytes_hold(never), "{never} is in kb.db");
    }
}

/// A private shell of SHELLS that a daemon restart loses is offered back private: started
/// again, its commands are still not kept.
#[tokio::test(flavor = "multi_thread")]
async fn a_private_shell_is_still_private_when_a_restart_offers_it_back() {
    let mut w = World::marked("[shell]\nrestore = \"offer\"\n");
    let o = w.cli(&["shells", "new", "--private", "--detach", "--name", "sp"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let c = w.d.client().await.unwrap();
    let was = w.info(&c, "sp").await;
    assert!(was.private && was.loose);
    // (The list of SHELLS is saved on a tick.)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    drop(c);
    w.d.restart().unwrap();
    let c = w.d.client().await.unwrap();
    let back = w.info(&c, "sp").await;
    assert!(back.lost, "{back:#?}");
    assert!(back.private, "offered back private: {back:#?}");
    let o = w.cli(&["restart", "sp"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    w.d.wait_for_text(&c, "sp", "$", WAIT).await.unwrap();
    assert!(w.info(&c, "sp").await.private);
    w.run(&c, "sp", "true zqprivateagain").await;
    let other = w.shell(&c, "sh2", false).await;
    w.run(&c, "sh2", "true zqothershell").await;
    w.shell_kept(&other.id.full(), "kept the other shell's", |k| has(k, "zqothershell")).await;
    assert!(w.shell_chunks(&back.id.full()).await.is_empty());
    assert!(!w.bytes_hold("zqprivateagain"));
}

/// The real shells, with the integration ptyro gives them and no setup: a command line comes
/// along whole, and one typed with a space before it is kept out, whether the shell's own
/// history takes it (`HISTCONTROL` unset) or not (`ignorespace`). With output asked for,
/// what a command printed is exactly that: nothing of the prompt, nothing of the line typed.
#[tokio::test(flavor = "multi_thread")]
async fn a_real_shells_commands_are_kept_and_a_space_before_one_keeps_it_out() {
    let w = World::new("[kb]\nshell = \"output\"\n");
    let c = w.d.client().await.unwrap();
    let mut checked = 0;
    for shell in ["bash", "zsh"] {
        // (bash before 4.4, macOS's, sees a command start by a DEBUG trap; a newer one, every
        // Linux's, by `PS0`: `PTYRO_REQUIRE_SHELLS` in CI makes sure each machine runs its own.)
        if !shell_here("kb4", shell) {
            continue;
        }
        checked += 1;
        let spec = ptyro_core::session::SpawnSpec {
            argv: vec![shell.to_owned()],
            cwd: w.home(),
            name: Some(shell.into()),
            size: Some(ptyro_core::TermSize::new(120, 30)),
            ..Default::default()
        };
        let info = match c.request(ptyro_protocol::Request::Spawn { spec }).await.expect("spawn") {
            ptyro_protocol::Reply::Session { info } => info,
            other => panic!("{other:?}"),
        };
        let long = format!("echo zqreal{shell} {}", "word ".repeat(60));
        for line in [
            long.trim_end(),
            " echo zqhidden",
            "HISTCONTROL=ignorespace; setopt HIST_IGNORE_SPACE 2>/dev/null",
            " echo zqhiddenagain",
            "echo zqdone",
        ] {
            w.run(&c, shell, line).await;
        }
        let id = info.id.full();
        let kept = w.shell_kept(&id, "kept the last command", |k| has(k, "zqdone")).await;
        assert_eq!(
            texts(&kept, ChunkKind::Command),
            [
                format!("$ {}", long.trim_end()).as_str(),
                "$ HISTCONTROL=ignorespace; setopt HIST_IGNORE_SPACE 2>/dev/null",
                "$ echo zqdone",
            ],
            "{shell}"
        );
        assert!(!w.bytes_hold("zqhidden"), "{shell}");
        assert_eq!(
            texts(&kept, ChunkKind::Output),
            [long.trim_end().strip_prefix("echo ").unwrap(), "zqdone"],
            "{shell}"
        );
    }
    assert!(checked > 0, "neither bash nor zsh is installed: nothing was checked");
}
