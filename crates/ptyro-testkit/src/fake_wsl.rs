//! `ptyro fake-wsl`: enough of `wsl.exe` for setup's WSL side to be tested on any machine
//! (BOOT8), with no WSL. A test points `PTYRO_WSL` at a stand-in that runs this
//! ([`crate::program::program`]), with `PTYRO_FAKE_WSL` naming a directory:
//!
//! | file | role |
//! |---|---|
//! | `distros` | the distros, one a line, the default first (`wsl -l -q`); absent or empty: none |
//! | `<distro>/ptyro/` | ptyro is installed in that distro: its `PTYRO_HOME` (absent: it isn't) |
//! | `<distro>/fake/` | that distro's fake machine, setup's `PTYRO_TEST_SETUP` there |
//! | `<distro>/home/` | its `$HOME` |
//! | `<distro>/old` | its ptyro is one from before setup: only `--version` works |
//! | `wsl.log` | each call's arguments, one call a line (written) |
//!
//! `wsl [-d D] --exec sh -c SCRIPT BIN ARGS…` runs this same ptyro as the distro's: the words
//! after `exec "$0"` in SCRIPT (`"$@"` meaning ARGS), with that distro's environment.

use std::io::Write;
use std::path::PathBuf;

pub fn main(args: &[String]) -> i32 {
    let Some(dir) = std::env::var_os("PTYRO_FAKE_WSL").map(PathBuf::from) else {
        eprintln!("fake-wsl: PTYRO_FAKE_WSL is not set");
        return 2;
    };
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("wsl.log"))
    {
        let _ = writeln!(f, "{}", args.join(" "));
    }
    let distros: Vec<String> = std::fs::read_to_string(dir.join("distros"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if args.iter().any(|a| a == "-l") {
        if distros.is_empty() {
            println!("Windows Subsystem for Linux has no installed distributions.");
            return 1;
        }
        for d in &distros {
            print!("{d}\r\n");
        }
        return 0;
    }
    let mut rest = args;
    let mut distro = distros.first().cloned();
    if rest.first().map(String::as_str) == Some("-d") {
        distro = rest.get(1).cloned();
        rest = rest.get(2..).unwrap_or_default();
    }
    let Some(distro) = distro.filter(|d| distros.contains(d)) else {
        println!("There is no distribution with the supplied name.");
        return 1;
    };
    // --exec sh -c SCRIPT BIN ARGS…
    let [exec, sh, c, script, _bin, after @ ..] = rest else {
        eprintln!("fake-wsl: expected --exec sh -c SCRIPT BIN ARGS…, got {rest:?}");
        return 2;
    };
    if exec != "--exec" || sh != "sh" || c != "-c" {
        eprintln!("fake-wsl: expected --exec sh -c, got {rest:?}");
        return 2;
    }
    let words = script.split("exec \"$0\"").nth(1).unwrap_or("").trim();
    let argv: Vec<String> = if words == "\"$@\"" {
        after.to_vec()
    } else {
        words.split_whitespace().map(str::to_owned).collect()
    };
    let d = dir.join(&distro);
    if !d.join("ptyro").is_dir() {
        eprintln!("sh: 1: exec: ptyro: not found");
        return 127;
    }
    if d.join("old").exists() {
        if argv.first().map(String::as_str) == Some("--version") {
            println!("ptyro 0.0.1");
            return 0;
        }
        eprintln!("error: unrecognized subcommand '{}'", argv.first().cloned().unwrap_or_default());
        return 2;
    }
    let Ok(me) = std::env::current_exe() else { return 2 };
    let mut cmd = std::process::Command::new(me);
    cmd.args(&argv)
        .env("PTYRO_TEST_SETUP", d.join("fake"))
        .env("PTYRO_HOME", d.join("ptyro"))
        .env("HOME", d.join("home"))
        .env("USERPROFILE", d.join("home"))
        .env("SHELL", "/bin/bash");
    for v in ["PTYRO_SOCKET", "PTYRO_SESSION_ID", "PTYRO_CONFIG", "PTYRO_REMOTE", "PTYRO_WSL"] {
        cmd.env_remove(v);
    }
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fake-wsl: {e}");
            127
        }
    }
}
