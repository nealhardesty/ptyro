//! `spike-conpty`: WIN0.2 (what ConPTY does to what ptyro needs) and WIN0.3 (can a second
//! process take a pseudoconsole over). See ROADMAP WIN0 and `make spike-conpty`.
//!
//!   spike-conpty run <out-dir> [conpty.dll]   on Windows: every case, every variant, takeovers
//!   spike-conpty report <out-dir>             anywhere: the findings as Markdown
//!
//! `child`, `child-echo`, `takeover-old` and `takeover-new` are its own helpers.

mod cases;
mod report;
#[cfg(windows)]
mod win;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/ptyro-term/tests/fixtures");
    match args.first().map(String::as_str) {
        Some("report") if args.len() >= 2 => {
            let fx = args.get(2).map_or(fixtures, std::path::PathBuf::from);
            match report::report(std::path::Path::new(&args[1]), &fx) {
                Ok(md) => print!("{md}"),
                Err(e) => {
                    eprintln!("report: {e}");
                    std::process::exit(1);
                }
            }
        }
        #[cfg(windows)]
        Some("run") if args.len() >= 2 => {
            // Beside the exe when copied to Windows; else ptyro-term's own.
            let local = std::env::current_exe().unwrap().with_file_name("fixtures");
            let fx = if local.is_dir() { local } else { fixtures };
            let dll = args.get(2).filter(|d| *d != "-").map(std::path::PathBuf::from);
            if let Err(e) = win::run(std::path::Path::new(&args[1]), dll.as_deref(), &fx) {
                eprintln!("run: {e}");
                std::process::exit(1);
            }
        }
        #[cfg(windows)]
        Some("child") => win::child(&args[1..]),
        #[cfg(windows)]
        Some("child-echo") => win::child_echo(&args[1..]),
        #[cfg(windows)]
        Some("child-records") => win::child_records(&args[1..]),
        #[cfg(windows)]
        Some("takeover-old") => win::takeover_old(&args[1..]),
        #[cfg(windows)]
        Some("takeover-new") => win::takeover_new(&args[1..]),
        _ => {
            eprintln!(
                "usage: spike-conpty run <out-dir> [conpty.dll] (Windows) | report <out-dir> [fixtures]"
            );
            std::process::exit(2);
        }
    }
}
