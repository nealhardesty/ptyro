//! Facts about this build for `ptyro about` (UX2.1): the commit it was built from, that
//! commit's date, and the target triple. Without git (a source tarball) the commit is left
//! out, and nothing here fails the build.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    // A build can name its commit itself (a packager without .git).
    let commit = std::env::var("PTYRO_BUILD_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| git(&["rev-parse", "--short=9", "HEAD"]))
        .unwrap_or_default();
    let date = std::env::var("PTYRO_BUILD_DATE")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| git(&["log", "-1", "--format=%cs"]))
        .unwrap_or_default();
    println!("cargo:rustc-env=PTYRO_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=PTYRO_BUILD_DATE={date}");
    println!("cargo:rustc-env=PTYRO_BUILD_TARGET={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rerun-if-env-changed=PTYRO_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=PTYRO_BUILD_DATE");
    // New commits (a branch moving, a checkout) rebuild this; linked worktrees keep refs in
    // the common directory, which `--git-path` resolves.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &r])
    {
        println!("cargo:rerun-if-changed={path}");
    }
}
