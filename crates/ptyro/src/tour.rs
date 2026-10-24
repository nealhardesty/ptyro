//! `ptyro tour` (UX1.22): ptyro's daily loop, done for real. A ptyro of its own (daemon, store
//! and worktrees under a throwaway directory) with a demo repository and a pretend agent (the
//! built-in fake, `PTYRO_FAKE_TOUR`), the TUI with its coach on top (`ptyro tour --here`), and
//! everything deleted afterwards. Your own sessions, projects and config are never touched; the
//! tour borrows only your `[ui]` and `[keys]`, so its keys are yours.

use std::path::Path;
use std::process::Command;

use ptyro_core::Error;
use ptyro_core::config::Config;

use crate::cli::Ctx;

/// The demo repository's one file, with the typo the pretend agent fixes.
const GREETING: &str = "Helo, world!\n";

/// Variables of the outer ptyro the tour must not see (they would point it at your daemon).
const OUTER: &[&str] = &["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_PROFILE"];

pub async fn run(ctx: &Ctx) -> Result<i32, Error> {
    crate::cli::check_attachable()?;
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    sweep(&exe);
    let dir = std::env::temp_dir().join(format!(
        "ptyro-tour-{}-{}",
        std::process::id(),
        ptyro_core::now_ms()
    ));
    let home = dir.join("ptyro");
    let demo = dir.join("demo");
    let mine = std::fs::read_to_string(Config::path(&ctx.paths)).unwrap_or_default();
    let result = prepare(&exe, &home, &demo, &mine).and_then(|()| {
        eprintln!("[the tour: a ptyro of its own, a demo repository and a pretend agent]");
        // The TUI with the coach, as a child: it gets this terminal, the tour's ptyro and the
        // demo repository, and none of yours.
        let status = env(&mut Command::new(&exe), &home)
            .args(["tour", "--here"])
            .current_dir(&demo)
            .status()
            .map_err(|e| Error::internal(format!("start the tour: {e}")))?;
        Ok(status.code().unwrap_or(1))
    });
    let _ = env(&mut Command::new(&exe), &home)
        .args(["daemon", "stop", "--force"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("[the tour is over: its ptyro, repository and agent are gone]");
    result
}

/// The web tour (WEB1.6): the same throwaway ptyro (its own daemon, store and worktrees, a demo
/// repository, the pretend agent), served by a `ptyro serve` of its own on a free loopback port,
/// paired with a device for the tour. The web app opens it in a new tab and its coach walks the
/// steps; `web_end` there stops all of it and deletes it. Returns `{url, dir}`.
pub async fn web_start(ctx: &Ctx) -> Result<serde_json::Value, Error> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    sweep(&exe);
    let dir = std::env::temp_dir().join(format!(
        "ptyro-tour-{}-{}",
        std::process::id(),
        ptyro_core::now_ms()
    ));
    let home = dir.join("ptyro");
    let demo = dir.join("demo");
    let mine = std::fs::read_to_string(Config::path(&ctx.paths)).unwrap_or_default();
    prepare(&exe, &home, &demo, &mine)?;
    let fail = |what: &str, dir: &Path, exe: &Path, home: &Path, e: String| {
        let _ = env(&mut Command::new(exe), home).args(["daemon", "stop", "--force"]).output();
        let _ = std::fs::remove_dir_all(dir);
        Error::internal(format!("{what}: {e}"))
    };
    // The tour's daemon starts with its first project.
    let added = env(&mut Command::new(&exe), &home).args(["project", "add"]).arg(&demo).output();
    if !added.as_ref().is_ok_and(|o| o.status.success()) {
        return Err(fail("the tour's project", &dir, &exe, &home, format!("{added:?}")));
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| Error::internal(format!("a free port: {e}")))?
        .port();
    let log = std::fs::File::create(dir.join("serve.log"))
        .map_err(|e| Error::internal(format!("the tour's log: {e}")))?;
    env(&mut Command::new(&exe), &home)
        .args(["serve", "--insecure", "--listen", &format!("127.0.0.1:{port}")])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| Error::internal(e.to_string()))?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|e| fail("the tour's server", &dir, &exe, &home, e.to_string()))?;
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let out = env(&mut Command::new(&exe), &home)
        .args([
            "pair",
            "--scope",
            "full",
            "--json",
            "--url",
            &format!("http://127.0.0.1:{port}"),
            "tour",
        ])
        .output()
        .map_err(|e| fail("pairing the tour", &dir, &exe, &home, e.to_string()))?;
    let url = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|v| v["url"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            fail(
                "pairing the tour",
                &dir,
                &exe,
                &home,
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        })?;
    Ok(serde_json::json!({ "url": format!("{url}&tour=1"), "dir": dir }))
}

/// End the web tour this server belongs to: its daemon stops, its directory goes, and this
/// server with them. Refused for anything that isn't a tour's own ptyro.
pub async fn web_end() -> Result<serde_json::Value, Error> {
    let home = std::env::var_os("PTYRO_HOME").map(std::path::PathBuf::from);
    let dir = home
        .as_deref()
        .and_then(Path::parent)
        .filter(|d| {
            d.parent() == Some(std::env::temp_dir().as_path())
                && d.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ptyro-tour-"))
        })
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::invalid("this isn't a tour"))?;
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    let home = dir.join("ptyro");
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let _ = env(&mut Command::new(&exe), &home)
            .args(["daemon", "stop", "--force"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        std::process::exit(0);
    });
    Ok(serde_json::json!({ "ending": true }))
}

/// Earlier tours that never got to clean up (their terminal closed, they were killed): stop
/// their daemon and delete them. A tour is `ptyro-tour-<pid>-<ms>`, stale once `<pid>` is gone.
fn sweep(exe: &Path) {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix("ptyro-tour-")
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<i32>().ok())
        else {
            continue;
        };
        let alive = rustix::process::Pid::from_raw(pid)
            .is_some_and(|p| rustix::process::test_kill_process(p).is_ok());
        if alive {
            continue;
        }
        let _ = env(&mut Command::new(exe), &e.path().join("ptyro"))
            .args(["daemon", "stop", "--force"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(e.path());
    }
}

/// `cmd` under the tour's ptyro: its home, nothing of the outer one, commits that work even
/// with no git identity set up.
fn env<'a>(cmd: &'a mut Command, home: &Path) -> &'a mut Command {
    for k in OUTER {
        cmd.env_remove(k);
    }
    cmd.env("PTYRO_HOME", home)
        .env("PTYRO_ALLOW_NESTED", "1")
        .env("GIT_AUTHOR_NAME", "ptyro tour")
        .env("GIT_AUTHOR_EMAIL", "tour@ptyro.invalid")
        .env("GIT_COMMITTER_NAME", "ptyro tour")
        .env("GIT_COMMITTER_EMAIL", "tour@ptyro.invalid")
}

/// The tour's config (your `[ui]` and `[keys]`, its pretend agent) and its demo repository.
fn prepare(exe: &Path, home: &Path, demo: &Path, mine: &str) -> Result<(), Error> {
    let io = |what: &str, e: std::io::Error| Error::internal(format!("{what}: {e}"));
    std::fs::create_dir_all(home.join("config")).map_err(|e| io("the tour's directory", e))?;
    std::fs::write(home.join("config").join("config.toml"), config(exe, mine))
        .map_err(|e| io("the tour's config", e))?;
    std::fs::create_dir_all(demo).map_err(|e| io("the demo repository", e))?;
    std::fs::write(demo.join("greeting.txt"), GREETING).map_err(|e| io("greeting.txt", e))?;
    std::fs::write(
        demo.join("README.md"),
        "# demo\n\nThe tour's repository: ptyro made it, and deletes it when the tour ends.\n",
    )
    .map_err(|e| io("README.md", e))?;
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &[
            "-c",
            "user.name=ptyro tour",
            "-c",
            "user.email=tour@ptyro.invalid",
            "commit",
            "-qm",
            "A greeting",
        ],
    ] {
        let ok = Command::new("git")
            .args(args)
            .current_dir(demo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err(Error::internal(format!(
                "git {} in the demo repository failed",
                args.join(" ")
            )));
        }
    }
    Ok(())
}

/// The tour's `config.toml`: the keys you know, its pretend Claude, nothing that reaches out.
fn config(exe: &Path, mine: &str) -> String {
    let mut out = toml::Table::new();
    if let Ok(t) = mine.parse::<toml::Table>() {
        for k in ["ui", "keys"] {
            if let Some(v) = t.get(k) {
                out.insert(k.into(), v.clone());
            }
        }
    }
    let exe = exe.to_string_lossy().into_owned();
    let tour: toml::Table = format!(
        r#"
[agents.claude]
command = [{exe}, "fake-agent", "--as", "claude"]
default = true

[agents.claude.env]
PTYRO_FAKE_TOUR = "1"
PTYRO_FAKE_PASTE = "1"

[worktrees]
fetch = false

[discover]
roots = []
"#,
        exe = toml::Value::String(exe)
    )
    .parse()
    .expect("the tour's config");
    out.extend(tour);
    out.to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_tour_left_behind_is_swept_and_a_running_one_is_not() {
        let tmp = std::env::temp_dir();
        let stamp = ptyro_core::now_ms();
        // No process has this pid (above every system's pid limit); this test's own is alive.
        let gone = tmp.join(format!("ptyro-tour-9999999-{stamp}"));
        let alive = tmp.join(format!("ptyro-tour-{}-{stamp}", std::process::id()));
        for d in [&gone, &alive] {
            std::fs::create_dir_all(d.join("ptyro")).unwrap();
        }
        sweep(Path::new("/usr/bin/true"));
        assert!(!gone.exists(), "left behind: swept");
        assert!(alive.exists(), "still running: kept");
        std::fs::remove_dir_all(&alive).unwrap();
    }

    #[test]
    fn the_tours_config_keeps_your_keys_and_brings_its_own_agent() {
        let mine = "[ui]\nprefix = \"C-]\"\n[keys]\n\"y\" = \"pane.zoom\"\n[agents.claude]\ncommand = \"claude\"\n";
        let c: toml::Table = config(&PathBuf::from("/bin/ptyro"), mine).parse().unwrap();
        assert_eq!(c["ui"]["prefix"].as_str(), Some("C-]"));
        assert_eq!(c["keys"]["y"].as_str(), Some("pane.zoom"));
        let agent = &c["agents"]["claude"];
        assert_eq!(
            agent["command"][0].as_str(),
            Some("/bin/ptyro"),
            "the pretend agent, not yours"
        );
        assert_eq!(agent["env"]["PTYRO_FAKE_TOUR"].as_str(), Some("1"));
        assert!(Config::parse(&config(&PathBuf::from("/bin/ptyro"), "not toml")).is_ok());
    }
}
