//! `ptyro-fuzz`: a stable-Rust mutation fuzzer over the targets in `ptyro_testkit::fuzz`
//! (M8.4). `make fuzz-smoke` runs every target for a few seconds in CI; `cargo fuzz` (nightly,
//! `make fuzz TARGET=…`) runs the same targets coverage-guided. A crash is saved under
//! `target/fuzz-crashes/` and replayed with `ptyro-fuzz --target T --replay FILE`.

use std::time::{Duration, Instant};

use ptyro_testkit::fuzz::{self, Rng};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let seconds: f64 = get("--seconds").and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let seed: u64 = get("--seed").and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
    });
    let only = get("--target");
    if let (Some(t), Some(file)) = (&only, get("--replay")) {
        let f = fuzz::target(t).unwrap_or_else(|| panic!("no target {t}"));
        let data = std::fs::read(&file).expect("read the crash file");
        f(&data);
        println!("{t}: {file} runs clean");
        return;
    }
    let out = std::path::Path::new("target/fuzz-crashes");
    let mut failed = false;
    for (name, f) in fuzz::TARGETS {
        if only.as_deref().is_some_and(|o| o != *name) {
            continue;
        }
        let mut rng = Rng::new(seed ^ name.len() as u64);
        let mut corpus = fuzz::seeds(name);
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        let mut runs = 0u64;
        std::panic::set_hook(Box::new(|_| {}));
        while Instant::now() < deadline {
            let base = corpus[rng.below(corpus.len())].clone();
            let input = fuzz::mutate(&mut rng, &base, &corpus);
            let r = std::panic::catch_unwind(|| f(&input));
            runs += 1;
            if let Err(e) = r {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_default();
                let _ = std::fs::create_dir_all(out);
                let path = out.join(format!("{name}-{seed}-{runs}.bin"));
                let _ = std::fs::write(&path, &input);
                eprintln!(
                    "{name}: CRASH after {runs} runs: {msg}\n  saved {} ({} bytes); replay: ptyro-fuzz --target {name} --replay {}",
                    path.display(),
                    input.len(),
                    path.display()
                );
                failed = true;
                break;
            }
            if runs.is_multiple_of(64) && corpus.len() < 4096 {
                corpus.push(input);
            }
        }
        let _ = std::panic::take_hook();
        if !failed {
            println!("{name:9} {runs:>8} runs in {seconds}s, seed {seed}: ok");
        }
    }
    if failed {
        std::process::exit(1);
    }
}
