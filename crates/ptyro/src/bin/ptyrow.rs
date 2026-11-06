//! `ptyrow.exe`: run the `ptyro.exe` beside it with the same arguments, without a window (WIN10).
//!
//! `ptyro.exe` is a console program, so whatever Windows starts it from that has no terminal of
//! its own opens one: the logon task that starts the daemon would keep a console window (or a
//! Windows Terminal) open for as long as it runs, and a toast's click would flash one before the
//! session's tab appears. This is a GUI program (no console is ever made for it), as `pythonw.exe`
//! and `javaw.exe` are; it starts `ptyro.exe` with a console that has no window, and goes. The
//! logon task runs `ptyrow.exe daemon start --spawned`; `ptyro:` links open through
//! `ptyrow.exe open-url "%1"`. Only the standard library: it is a trampoline, nothing more.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use std::os::windows::process::CommandExt;
    // A console with no window (its children share it, so they get none either), in a process
    // group of its own; inheriting nothing of ours.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let Some(ptyro) = std::env::current_exe().ok().map(|p| p.with_file_name("ptyro.exe")) else {
        std::process::exit(2);
    };
    let status = std::process::Command::new(&ptyro)
        .args(std::env::args_os().skip(1))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn();
    // Nobody reads our exit code but the task scheduler: say whether ptyro.exe started.
    std::process::exit(if status.is_ok() { 0 } else { 2 });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ptyrow is for Windows: it starts ptyro.exe without a window. Run ptyro here.");
    std::process::exit(2);
}
