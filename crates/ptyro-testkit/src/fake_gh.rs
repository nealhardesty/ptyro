//! `ptyro fake-gh`: enough of the GitHub CLI for ptyro's PR features to be tested without
//! GitHub (M6). A test puts a `gh` wrapper on the daemon's PATH that runs this, with
//! `PTYRO_FAKE_GH` naming a state directory:
//!
//! | file | role |
//! |---|---|
//! | `calls.log` | every invocation, one per line (written) |
//! | `pr.json` | the PR (`gh pr view`); written by `gh pr create` |
//! | `checks.json` | `gh pr checks` (exits 8 while any is pending) |
//! | `log.txt` | `gh run view --log-failed` |
//! | `review_comments.json` | `gh api …/pulls/N/comments` |
//! | `conversation.json` | `gh pr view N --json comments` |
//! | `issue.json` | `gh issue view` |
//! | `body.txt`, `replies.txt` | what was posted (written; `pr edit --body` rewrites it) |
//!
//! With `PTYRO_FAKE_GH_PER_REPO=1` each repository (the basename of the directory gh runs in)
//! gets its own state directory and PR number, for change sets' one-PR-per-member (M11).

use std::io::Write;
use std::path::PathBuf;

use serde_json::{Value, json};

fn read(dir: &std::path::Path, f: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(f)).ok()
}

fn arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

pub fn main(args: &[String]) -> i32 {
    let Some(mut dir) = std::env::var_os("PTYRO_FAKE_GH").map(PathBuf::from) else {
        eprintln!("fake-gh: PTYRO_FAKE_GH is not set");
        return 2;
    };
    let mut number = 7;
    if std::env::var_os("PTYRO_FAKE_GH_PER_REPO").is_some() {
        let repo = std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        number = 10 + repo.bytes().map(u32::from).sum::<u32>() % 90;
        dir = dir.join(repo);
        let _ = std::fs::create_dir_all(&dir);
    }
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("calls.log"))
    {
        let _ = writeln!(f, "{}", args.join(" "));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["pr", "view", _, "--json", fields] if fields.contains("comments") => {
            let c: Value = read(&dir, "conversation.json")
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(json!([]));
            println!("{}", json!({ "comments": c }));
            0
        }
        ["pr", "view", _, "--json", "body"] => {
            println!("{}", json!({ "body": read(&dir, "body.txt").unwrap_or_default() }));
            0
        }
        ["pr", "edit", _, "--body", body] => {
            let _ = std::fs::write(dir.join("body.txt"), body);
            0
        }
        ["pr", "view", ..] => match read(&dir, "pr.json") {
            Some(t) => {
                println!("{t}");
                0
            }
            None => {
                eprintln!("no pull requests found for branch \"{}\"", a.get(2).unwrap_or(&""));
                1
            }
        },
        ["pr", "checks", ..] => {
            let t = read(&dir, "checks.json").unwrap_or_else(|| "[]".into());
            println!("{t}");
            if t.contains("\"pending\"") {
                8
            } else if t.contains("\"fail\"") {
                1
            } else {
                0
            }
        }
        ["pr", "create", ..] => {
            let title = arg(args, "--title").unwrap_or("");
            let body = arg(args, "--body").unwrap_or("");
            let draft = args.iter().any(|x| x == "--draft");
            let _ = std::fs::write(dir.join("body.txt"), body);
            let url = format!("https://github.com/acme/app/pull/{number}");
            let pr = json!({
                "number": number, "url": url, "title": title,
                "state": "OPEN", "isDraft": draft, "reviewDecision": "", "autoMergeRequest": null,
            });
            let _ = std::fs::write(dir.join("pr.json"), pr.to_string());
            println!("{url}");
            0
        }
        ["pr", "merge", _, rest @ ..] => {
            let mut pr: Value = read(&dir, "pr.json")
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(json!({}));
            if rest.contains(&"--disable-auto") {
                pr["autoMergeRequest"] = Value::Null;
            } else if rest.contains(&"--auto") {
                pr["autoMergeRequest"] = json!({ "mergeMethod": rest.last() });
            } else {
                pr["state"] = json!("MERGED");
            }
            let _ = std::fs::write(dir.join("pr.json"), pr.to_string());
            0
        }
        ["pr", "comment", _, "--body", body] => {
            append(&dir, &format!("comment: {body}"));
            0
        }
        ["run", "view", _, "--log-failed"] => {
            print!("{}", read(&dir, "log.txt").unwrap_or_default());
            0
        }
        ["api", "--method", "POST", path, "-f", body] => {
            append(&dir, &format!("reply {path}: {}", body.trim_start_matches("body=")));
            println!("{{}}");
            0
        }
        ["api", path] if path.contains("/comments") => {
            println!("{}", read(&dir, "review_comments.json").unwrap_or_else(|| "[]".into()));
            0
        }
        ["issue", "view", ..] => match read(&dir, "issue.json") {
            Some(t) => {
                println!("{t}");
                0
            }
            None => {
                eprintln!("could not find issue");
                1
            }
        },
        _ => {
            eprintln!("fake-gh: unsupported: {}", args.join(" "));
            2
        }
    }
}

fn append(dir: &std::path::Path, line: &str) {
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("replies.txt"))
    {
        let _ = writeln!(f, "{line}");
    }
}
