//! `make test`'s runner, with a flake budget (ROADMAP WIN13): every test binary of
//! `cargo test <args>` runs, as `--no-fail-fast` would. A test that fails is run again, once, on
//! its own. Passing then makes it a flake: named in the output, in `target/flakes.txt` and in
//! the CI job's summary. The run fails when a test fails twice, or when there are more flakes
//! than `FLAKE_BUDGET` (default 2). A flake never passes silently, and a flaky suite can't
//! hide behind retries.
//!
//! Usage: `ptyro-flaky [cargo test arguments]` (`CARGO` names cargo). Doc tests run last,
//! without a retry.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// A test binary cargo built: what it is, and where it runs (its package's folder, as cargo
/// runs it).
struct Binary {
    exe: PathBuf,
    dir: PathBuf,
    package: String,
    name: String,
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// Build the tests and list their binaries (`--no-run`, cargo's JSON messages).
fn build(args: &[String]) -> Result<Vec<Binary>, String> {
    let mut child = Command::new(cargo())
        .arg("test")
        .args(args)
        .args(["--no-run", "--message-format=json-render-diagnostics"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cargo: {e}"))?;
    let mut out = String::new();
    child.stdout.take().map(|mut s| s.read_to_string(&mut out));
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("the tests didn't build (see above)".into());
    }
    let mut bins = Vec::new();
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] != "compiler-artifact" || v["profile"]["test"] != true {
            continue;
        }
        let Some(exe) = v["executable"].as_str() else { continue };
        let manifest = PathBuf::from(v["manifest_path"].as_str().unwrap_or_default());
        bins.push(Binary {
            exe: PathBuf::from(exe),
            dir: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
            package: v["package_id"].as_str().and_then(package_name).unwrap_or_default(),
            name: v["target"]["name"].as_str().unwrap_or_default().to_owned(),
        });
    }
    Ok(bins)
}

/// The package's name from cargo's package id (`path+file:///…/crates/ptyro#0.0.61`, or
/// `path+file:///…#ptyro-core@0.0.61`).
fn package_name(id: &str) -> Option<String> {
    let tail = id.rsplit_once('#')?.1;
    match tail.split_once('@') {
        Some((name, _)) => Some(name.to_owned()),
        None => id.rsplit_once('#')?.0.rsplit('/').next().map(str::to_owned),
    }
}

/// Run `exe` with `args` in `dir`, its output passed through as it comes; whether it passed, and
/// the tests it said failed.
fn run(b: &Binary, args: &[&str]) -> (bool, Vec<String>) {
    let child = Command::new(&b.exe)
        .args(args)
        .current_dir(&b.dir)
        .env("CARGO_MANIFEST_DIR", &b.dir)
        .env("CARGO_PKG_NAME", &b.package)
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ptyro-flaky: {}: {e}", b.exe.display());
            return (false, Vec::new());
        }
    };
    let mut failed = Vec::new();
    if let Some(out) = child.stdout.take() {
        let mut stdout = std::io::stdout().lock();
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = writeln!(stdout, "{line}");
            if let Some(name) =
                line.strip_prefix("test ").and_then(|l| l.strip_suffix(" ... FAILED"))
            {
                failed.push(name.to_owned());
            }
        }
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    (ok, failed)
}

#[derive(Default)]
struct Tally {
    /// (binary, test): failed, then passed alone.
    flaky: Vec<(String, String)>,
    /// (binary, test): failed twice. A test of `*` is the binary itself (it crashed, or failed
    /// without naming a test).
    failed: Vec<(String, String)>,
}

fn label(b: &Binary) -> String {
    format!("{}::{}", b.package, b.name)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let budget: usize =
        std::env::var("FLAKE_BUDGET").ok().and_then(|b| b.parse().ok()).unwrap_or(2);
    let bins = match build(&args) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ptyro-flaky: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut t = Tally::default();
    for b in &bins {
        println!("\n     Running {} ({})", label(b), b.exe.display());
        let (ok, failed) = run(b, &[]);
        if ok {
            continue;
        }
        if failed.is_empty() {
            println!("ptyro-flaky: {} failed without naming a test: running it again", label(b));
            if run(b, &[]).0 {
                t.flaky.push((label(b), "*".into()));
            } else {
                t.failed.push((label(b), "*".into()));
            }
            continue;
        }
        for name in failed {
            println!("ptyro-flaky: {}: running {name} again, alone", label(b));
            if run(b, &["--exact", &name, "--test-threads=1"]).0 {
                t.flaky.push((label(b), name));
            } else {
                t.failed.push((label(b), name));
            }
        }
    }
    // Doc tests: cargo runs them (they're built as they run), once; not when only some targets
    // were asked for.
    let some = args
        .iter()
        .any(|a| matches!(a.as_str(), "--test" | "--tests" | "--lib" | "--bin" | "--bins"));
    let docs_failed = !some
        && !Command::new(cargo())
            .arg("test")
            .args(&args)
            .arg("--doc")
            .status()
            .is_ok_and(|s| s.success());
    report(&t, budget, docs_failed)
}

fn report(t: &Tally, budget: usize, docs_failed: bool) -> ExitCode {
    let mut text = String::new();
    for (b, n) in &t.flaky {
        text.push_str(&format!("flaky   {b} {n}\n"));
    }
    for (b, n) in &t.failed {
        text.push_str(&format!("FAILED  {b} {n}\n"));
    }
    let _ = std::fs::create_dir_all("target");
    let _ = std::fs::write("target/flakes.txt", &text);
    let over = t.flaky.len() > budget;
    let verdict = if !t.failed.is_empty() || docs_failed {
        "failed"
    } else if over {
        "over the flake budget"
    } else {
        "passed"
    };
    println!(
        "\nptyro-flaky: {verdict}: {} failed, {} flaky (budget {budget}){}",
        t.failed.len(),
        t.flaky.len(),
        if docs_failed { ", doc tests failed" } else { "" }
    );
    print!("{text}");
    if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(summary)
    {
        let mut md = format!(
            "### Tests: {verdict}\n\n{} failed, {} flaky (budget {budget}){}\n\n",
            t.failed.len(),
            t.flaky.len(),
            if docs_failed { "; doc tests failed" } else { "" }
        );
        for (b, n) in &t.flaky {
            md.push_str(&format!("- flaky: `{b}` `{n}` (failed, then passed alone)\n"));
        }
        for (b, n) in &t.failed {
            md.push_str(&format!("- **failed**: `{b}` `{n}`\n"));
        }
        let _ = f.write_all(md.as_bytes());
    }
    if verdict == "passed" { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::package_name;

    #[test]
    fn package_ids_name_their_package() {
        assert_eq!(
            package_name("path+file:///home/me/ptyro/crates/ptyro#0.0.61").as_deref(),
            Some("ptyro")
        );
        assert_eq!(
            package_name("path+file:///home/me/ptyro/crates/ptyro-core#ptyro-core@0.0.61")
                .as_deref(),
            Some("ptyro-core")
        );
    }
}
