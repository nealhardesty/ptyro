//! `ptyro fake-gh`: enough of the GitHub CLI for starting work from an issue or a pull request
//! to be tested without GitHub. A test puts a `gh` wrapper on the daemon's PATH that runs this,
//! with `PTYRO_FAKE_GH` naming a state directory:
//!
//! | file | role |
//! |---|---|
//! | `calls.log` | every invocation, one per line (written) |
//! | `pr.json` | the PR (`gh pr view`) |
//! | `issue.json` | `gh issue view` |

use std::io::Write;
use std::path::PathBuf;

fn read(dir: &std::path::Path, f: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(f)).ok()
}

pub fn main(args: &[String]) -> i32 {
    let Some(dir) = std::env::var_os("PTYRO_FAKE_GH").map(PathBuf::from) else {
        eprintln!("fake-gh: PTYRO_FAKE_GH is not set");
        return 2;
    };
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("calls.log"))
    {
        let _ = writeln!(f, "{}", args.join(" "));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (file, missing) = match a.as_slice() {
        ["pr", "view", ..] => ("pr.json", "no pull requests found"),
        ["issue", "view", ..] => ("issue.json", "could not find issue"),
        _ => {
            eprintln!("fake-gh: unsupported: {}", args.join(" "));
            return 2;
        }
    };
    match read(&dir, file) {
        Some(t) => {
            println!("{t}");
            0
        }
        None => {
            eprintln!("{missing}");
            1
        }
    }
}
