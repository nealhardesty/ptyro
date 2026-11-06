//! WIN6 end to end, on Windows itself: a project's scripts each in the shell they're written for
//! (`[scripts] shell`, `auto` by default), on a native daemon. CI's `windows` job runs these
//! (`make test-windows`); its runner has Git for Windows, so Git Bash.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use ptyro_protocol::{Reply, Request};
use ptyro_testkit::TempDaemon;

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

struct World {
    home: tempfile::TempDir,
    d: TempDaemon,
}

impl World {
    fn new() -> Self {
        let home = tempfile::Builder::new().prefix("ptyro-win6-").tempdir().unwrap();
        let h = ptyro_core::os::canonicalize(home.path()).unwrap();
        let h = PathBuf::from(h.to_string_lossy().trim_start_matches(r"\\?\"));
        std::fs::write(
            h.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n[core]\n\tautocrlf = false\n",
        )
        .unwrap();
        let hs = h.to_string_lossy().into_owned();
        let env =
            [("USERPROFILE", hs.as_str()), ("HOME", hs.as_str()), ("GIT_CONFIG_NOSYSTEM", "1")];
        let d = TempDaemon::start_with(BIN, &env, Some("[worktrees]\nfetch = false\n")).unwrap();
        Self { home, d }
    }

    fn home(&self) -> PathBuf {
        let h = ptyro_core::os::canonicalize(self.home.path()).unwrap();
        PathBuf::from(h.to_string_lossy().trim_start_matches(r"\\?\"))
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = self.home().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.git(&root, &["init", "-q"]);
        for (f, text) in files {
            std::fs::write(root.join(f), text).unwrap();
        }
        self.git(&root, &["add", "-A"]);
        self.git(&root, &["commit", "-q", "-m", "init"]);
        root
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self
            .d
            .cmd()
            .args(args)
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .current_dir(self.home())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "ptyro {args:?}: {text}");
        text
    }
}

/// Run `project`'s `script` in its main checkout; its argv and its screen once `want` shows.
async fn run(
    w: &World,
    c: &ptyro_client::Client,
    project: &str,
    script: &str,
    want: &str,
) -> (Vec<String>, String) {
    let reply = c
        .request(Request::WorktreeRun {
            addr: format!("{project}/@root"),
            script: Some(script.into()),
            size: None,
        })
        .await
        .unwrap_or_else(|e| panic!("{project} {script}: {}", e.message));
    let Reply::Session { info } = reply else { panic!("{reply:?}") };
    let screen = w.d.wait_for_text(c, &info.name, want, WAIT).await.unwrap();
    (info.argv, screen)
}

#[tokio::test]
async fn each_script_runs_in_its_own_shell() {
    let w = World::new();
    let toml = "[scripts.run.ps]\ncommand = './hello.ps1 there'\n\
                [scripts.run.sh]\ncommand = './hello.sh there'\n\
                [scripts.run.inline]\ncommand = 'Write-Output (\"inline-\" + (1 + 1))'\n";
    let root = w.repo(
        "auto",
        &[
            (".ptyro.toml", toml),
            ("hello.ps1", "param($who)\nWrite-Output \"ps-ran-$who\"\nStart-Sleep 30\n"),
            ("hello.sh", "#!/bin/sh\necho \"sh-ran-$1\"\nsleep 30\n"),
        ],
    );
    w.ok(&["project", "add", &root.to_string_lossy()]);
    w.ok(&["project", "trust", "auto", "--yes"]);
    let c = w.d.client().await.unwrap();
    // A .ps1 file: PowerShell, whatever the execution policy (Windows PowerShell refuses by
    // default).
    let (argv, _) = run(&w, &c, "auto", "ps", "ps-ran-there").await;
    assert!(argv.iter().any(|a| a == "Bypass"), "{argv:?}");
    // A .sh file: Git Bash.
    let (argv, _) = run(&w, &c, "auto", "sh", "sh-ran-there").await;
    assert!(argv[0].to_lowercase().ends_with(r"\git\bin\bash.exe"), "{argv:?}");
    // Anything else: the default shell, PowerShell.
    run(&w, &c, "auto", "inline", "inline-2").await;
}

#[tokio::test]
async fn cmd_reads_its_command_line_as_written() {
    let w = World::new();
    let toml = "[scripts]\nshell = 'cmd'\n[scripts.run.q]\ncommand = 'echo \"a  b\" & echo done-%OS%& ping -n 30 127.0.0.1 >nul'\n";
    let root = w.repo("cmdp", &[(".ptyro.toml", toml)]);
    w.ok(&["project", "add", &root.to_string_lossy()]);
    w.ok(&["project", "trust", "cmdp", "--yes"]);
    let c = w.d.client().await.unwrap();
    // The quotes and the `&` reached cmd as written: Rust's `\"` quoting would have broken both.
    let (argv, screen) = run(&w, &c, "cmdp", "q", "done-Windows_NT").await;
    assert!(argv[0].to_lowercase().ends_with(r"\system32\cmd.exe"), "{argv:?}");
    assert!(screen.contains("\"a  b\""), "{screen}");
}
