//! WIN10 on Windows itself: `ptyro daemon install-service` registers a per-user logon task with
//! Windows' Task Scheduler that runs the windowless `ptyrow.exe daemon start --spawned`, and
//! `uninstall-service` removes it; a `ptyro:` link that isn't one ptyro writes opens nothing.
//! The task here is a test's own (`PTYRO_TEST_SERVICE_TASK`), never started, so the computer's
//! `\ptyro\daemon` is left alone. CI's `windows` job runs these (`make test-windows`).
#![cfg(windows)]

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");

fn ptyro(home: &std::path::Path, task: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("PTYRO_HOME", home)
        .env("PTYRO_TEST_SERVICE_TASK", task)
        .env("PTYRO_NO_AUTOSPAWN", "1")
        .output()
        .unwrap()
}

fn said(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// What Task Scheduler has for `task`, as its XML; `None` when there's no such task.
fn query(task: &str) -> Option<String> {
    let o = Command::new("schtasks.exe").args(["/Query", "/TN", task, "/XML"]).output().unwrap();
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn the_logon_task_is_registered_windowless_and_removed() {
    let home = tempfile::tempdir().unwrap();
    let task = format!("\\ptyro-test\\daemon-{}", std::process::id());
    let _gone = Remove(task.clone());
    // The windowless launcher is built beside ptyro.exe (`cargo build -p ptyro --bins`).
    let ptyrow = std::path::Path::new(BIN).with_file_name("ptyrow.exe");
    assert!(ptyrow.is_file(), "{} wasn't built", ptyrow.display());

    // --dry-run prints the task and registers nothing.
    let o = ptyro(home.path(), &task, &["daemon", "install-service", "--dry-run"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("<LogonTrigger>"), "{}", said(&o));
    assert_eq!(query(&task), None, "a dry run registered it");

    let o = ptyro(home.path(), &task, &["daemon", "install-service"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("starts when you log on"), "{}", said(&o));
    let xml = query(&task).expect("the task is registered");
    let user = std::env::var("USERNAME").unwrap();
    assert!(xml.contains("<LogonTrigger>"), "{xml}");
    assert!(xml.contains(&user), "this user's logon: {xml}");
    assert!(xml.contains("ptyrow.exe</Command>"), "{xml}");
    assert!(xml.contains("<Arguments>daemon start --spawned</Arguments>"), "{xml}");
    // LeastPrivilege is Task Scheduler's default, which it leaves out when it exports a task.
    assert!(!xml.contains("HighestAvailable"), "never elevated: {xml}");

    // Installing again replaces it; removing it twice is fine.
    let o = ptyro(home.path(), &task, &["daemon", "install-service"]);
    assert!(o.status.success(), "{}", said(&o));
    let o = ptyro(home.path(), &task, &["daemon", "uninstall-service"]);
    assert!(o.status.success(), "{}", said(&o));
    assert_eq!(query(&task), None, "the task is gone");
    let o = ptyro(home.path(), &task, &["daemon", "uninstall-service"]);
    assert!(o.status.success(), "{}", said(&o));
}

/// The test's task goes, whatever happened.
struct Remove(String);

impl Drop for Remove {
    fn drop(&mut self) {
        let _ = Command::new("schtasks.exe").args(["/Delete", "/TN", &self.0, "/F"]).output();
    }
}

/// A browser can hand Windows any `ptyro:` link: only `ptyro://attach/<session>` with a remote
/// ptyro writes is followed; anything else is refused before anything starts.
#[test]
fn a_link_ptyro_does_not_write_opens_nothing() {
    let home = tempfile::tempdir().unwrap();
    for bad in ["ptyro://run/calc", "ptyro://attach/--help", "ptyro://attach/c1?remote=ssh://x"] {
        let o = ptyro(home.path(), "\\ptyro-test\\unused", &["open-url", bad]);
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", said(&o));
        assert!(said(&o).contains("not a link ptyro opens"), "{bad}: {}", said(&o));
    }
}
