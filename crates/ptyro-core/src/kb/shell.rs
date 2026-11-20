//! A shell's commands as a source (SPEC §25.6.5): one item for each shell and day
//! (`shell://<session id>/<YYYY-MM-DD>`), a `command` chunk for each command line (`$ make
//! test`, under the folder it ran in, how it ended and how often), and, only where the user
//! asked for it, an `output` chunk beside it with what the command printed.
//!
//! **Never kept** ([`left_out`]): a command typed with a space before it (the habit that
//! keeps one out of a shell's history), one whose first word is on `kb.shell_skip`, and one
//! that names a path on the never list (`cat .env`, `ssh-add ~/.ssh/id_ed25519`): neither
//! the line nor what it printed.
//!
//! **Secrets in a command line** ([`hide`]): the redactor every source goes through (§25.7)
//! knows tokens, `Bearer …`, `password=…` and a URL's password. A command line has shapes of
//! its own that it doesn't: `export FOO_KEY=…`, `--password hunter2`, `mysql -phunter2`,
//! `curl -u me:hunter2`. Those values are taken out here, before the redactor. It is a net,
//! not a proof: a secret in no known shape gets through, which is why a space before a
//! command keeps it out altogether.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::ChunkKind;
use super::chunk::Part;
use super::ingest::Never;

/// The most of a command line that is kept, in bytes.
pub const TEXT: usize = 4 << 10;

/// A command that ended in a shell, as its session saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// The command line as the shell sent it: a space before it is still there.
    pub text: String,
    /// Where the shell was.
    pub cwd: String,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub exit: Option<i32>,
    /// What it printed, where that is collected (`ptyro_term::capture`).
    pub output: Option<String>,
}

/// The item a shell's commands of one day are kept in.
pub fn uri(session: &str, day: &str) -> String {
    format!("shell://{session}/{day}")
}

/// `2026-11-20`: the day of a moment. (Give it local time to get the local day.)
pub fn day(ms: u64) -> String {
    super::card::minute(ms)[..10].to_owned()
}

/// Whether nothing is kept of this command line: it was typed with a space before it, its
/// first word is one of `skip`, it names a path that is never read, or it ran in a folder
/// that is never read (`cwd`: there `cat config` names one too, so each word is also asked
/// about as a path from that folder).
pub fn left_out(text: &str, cwd: &str, skip: &[String], never: &Never) -> bool {
    if text.starts_with(char::is_whitespace) || text.trim().is_empty() {
        return true;
    }
    let first = text.split_whitespace().next().unwrap_or_default();
    let first = first.rsplit('/').next().unwrap_or(first);
    if skip.iter().any(|s| s == first) {
        return true;
    }
    let cwd = Path::new(cwd);
    if never.is_never(cwd) {
        return true;
    }
    text.split(|c: char| c.is_whitespace() || "'\"`=:,;|&<>(){}".contains(c))
        .filter(|w| !w.is_empty())
        .any(|w| never.is_never(Path::new(w)) || never.is_never(&cwd.join(w)))
}

/// The words in a variable's or a flag's name that say its value is a secret.
const SECRET: [&str; 12] = [
    "password",
    "passwd",
    "passphrase",
    "pass",
    "pw",
    "secret",
    "token",
    "key",
    "apikey",
    "auth",
    "credentials",
    "credential",
];

/// Whether a name (`FOO_API_KEY`, `--auth-token`, `PGPASSWORD`) says its value is a secret:
/// one of its parts ends in one of [`SECRET`].
fn secret_name(name: &str) -> bool {
    name.split(['_', '-', '.'])
        .filter(|p| !p.is_empty())
        .any(|p| SECRET.iter().any(|s| p.to_ascii_lowercase().ends_with(s)))
}

/// Something that stands for a value, not a value: `$TOKEN`, `$(pass show x)`, `<token>`.
fn stands_for(value: &str) -> bool {
    let v = value.trim_matches(['"', '\'']);
    v.is_empty() || v.starts_with(['$', '<', '{', '%'])
}

/// Programs whose `-p` takes a password: written against it only (`-phunter2`; mysql's
/// `-p name` is a database), or as the next word too.
const P_AGAINST: [&str; 5] = ["mysql", "mysqldump", "mysqladmin", "mariadb", "mariadb-dump"];
const P_NEXT: [&str; 7] = ["sshpass", "docker", "podman", "helm", "mongosh", "mongo", "smbclient"];

/// A command line with the secrets its own shapes show taken out (`<redacted>` in their
/// place): the value of a variable or a flag whose name says it is one, a password given to
/// `-p`, and the password after `-u user:`. What stands for a value (`$TOKEN`) stays.
pub fn hide(text: &str) -> String {
    const MARK: &str = "<redacted>";
    // The words, by where they are: split at blanks outside quotes.
    let mut words: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut quote) = (None, None);
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => {
                quote = Some(c);
                start.get_or_insert(i);
            }
            (None, c) if c.is_whitespace() => {
                if let Some(s) = start.take() {
                    words.push((s, i));
                }
            }
            (None, _) => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        words.push((s, text.len()));
    }
    // What goes: byte ranges, in order.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut program: Option<&str> = None;
    // The next word is a secret (`after`: only what follows this character in it).
    let mut next: Option<Option<char>> = None;
    for (from, to) in words {
        let w = &text[from..to];
        if let Some(after) = next.take()
            && (!w.starts_with('-') || after.is_some())
        {
            let at = match after {
                Some(c) => w.find(c).map(|i| from + i + 1),
                None => Some(from),
            };
            if let Some(at) = at.filter(|at| !stands_for(&text[*at..to])) {
                cuts.push((at, to));
            }
            continue;
        }
        if matches!(w, "|" | "||" | "&&" | ";" | "&") {
            program = None;
            continue;
        }
        let name_end = w.find('=');
        let name = name_end.map(|e| &w[..e]);
        let is_var = name.is_some_and(|n| {
            !n.is_empty()
                && !n.starts_with(|c: char| c.is_ascii_digit())
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        if w.starts_with('-') {
            let flag = name.unwrap_or(w);
            let prog = program.unwrap_or_default();
            if secret_name(flag.trim_start_matches('-')) && flag.len() > 2 {
                match name_end {
                    Some(e) if !stands_for(&w[e + 1..]) => cuts.push((from + e + 1, to)),
                    Some(_) => {}
                    None => next = Some(None),
                }
            } else if w.starts_with("-p")
                && !w.starts_with("-p=")
                && w.len() > 2
                && (P_AGAINST.contains(&prog) || P_NEXT.contains(&prog))
            {
                cuts.push((from + 2, to));
            } else if (w == "-p" && P_NEXT.contains(&prog)) || (w == "-a" && prog == "redis-cli") {
                next = Some(None);
            } else if matches!(w, "-u" | "--user") && matches!(prog, "curl" | "wget") {
                next = Some(Some(':'));
            } else if let Some(pw) = w
                .strip_prefix("--user=")
                .filter(|_| matches!(prog, "curl" | "wget"))
                .and_then(|v| v.find(':'))
            {
                cuts.push((from + "--user=".len() + pw + 1, to));
            }
        } else if is_var {
            let e = name_end.unwrap_or(0);
            if name.is_some_and(secret_name) && !stands_for(&w[e + 1..]) {
                cuts.push((from + e + 1, to));
            }
        } else if program.is_none() && !matches!(w, "sudo" | "export" | "env" | "time" | "exec") {
            program = Some(w.rsplit('/').next().unwrap_or(w));
        }
        if w.ends_with(';') {
            program = None;
        }
    }
    if cuts.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (from, to) in cuts {
        out.push_str(&text[at..from]);
        out.push_str(MARK);
        at = to;
    }
    out.push_str(&text[at..]);
    out
}

/// What finds a command's chunk again: the same line in the same folder is one chunk, which
/// counts. Made of the line *as it is stored* (redacted), so that it says nothing of a secret.
pub fn anchor(cwd: &str, redacted: &str) -> String {
    let mut h = Sha256::new();
    h.update(cwd.as_bytes());
    h.update([0xff]);
    h.update(redacted.as_bytes());
    format!("{:x}", h.finalize())[..16].to_owned()
}

/// How often a stored command chunk says it ran.
pub fn count(meta: Option<&serde_json::Value>) -> u64 {
    meta.and_then(|m| m["count"].as_u64()).unwrap_or(1)
}

/// `0.4s`, `12s`, `3m 05s`, `2h 10m`.
fn took(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=9 => format!("{}.{}s", s, ms % 1000 / 100),
        10..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// The command's chunks: the line, and what it printed if that was collected. `anchor`:
/// [`anchor`] of the redacted line. `count`: how often it has run with this one.
pub fn parts(ran: &Ran, anchor: &str, count: u64) -> Vec<Part> {
    let text = clip(ran.text.trim(), TEXT);
    let ms = ran.ended_ms.saturating_sub(ran.started_ms);
    let mut about = vec![format!("in {}", ran.cwd)];
    if let Some(code) = ran.exit.filter(|c| *c != 0) {
        about.push(format!("exit {code}"));
    }
    if ms >= 1000 {
        about.push(format!("took {}", took(ms)));
    }
    if count > 1 {
        about.push(format!("{count} times"));
    }
    let mut command = Part::new(ChunkKind::Command, format!("$ {text}"), ran.ended_ms)
        .anchor(anchor)
        .heading(about.join(" · "));
    command.meta = Some(serde_json::json!({
        "cwd": ran.cwd,
        "exit": ran.exit,
        "ms": ms,
        "count": count,
    }));
    let mut out = vec![command];
    if let Some(printed) = ran.output.as_deref().filter(|o| !o.trim().is_empty()) {
        out.push(
            Part::new(ChunkKind::Output, printed, ran.ended_ms)
                .anchor(anchor)
                .heading(format!("$ {}", crate::session::brief(text, 120))),
        );
    }
    out
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip() -> Vec<String> {
        crate::config::Kb::default().shell_skip
    }

    #[test]
    fn what_is_never_kept_of_a_shell() {
        let never = Never::new(&["*.sqlite".to_owned()]);
        let out = |text: &str| left_out(text, "/work/app", &skip(), &never);
        // Typed with a space before it; nothing; the trivial ones, by their first word.
        for text in
            [" export TOKEN=abc", "\tsecret", "", "  ", "ls", "ls -la src", "cd ..", "/bin/ls"]
        {
            assert!(out(text), "{text:?}");
        }
        // A key or credential file named anywhere in it, and `kb.exclude`'s own.
        for text in [
            "cat .env",
            "cat ./config/.env.production",
            "ssh-add ~/.ssh/id_ed25519",
            "openssl x509 -in server.pem -text",
            "scp host:.netrc .",
            "source <(cat credentials.json)",
            "aws --profile=x s3 cp ~/.aws/config s3://bucket",
            "FOO=$(cat secrets.yaml) make",
            "sqlite3 app.sqlite .dump",
        ] {
            assert!(out(text), "{text:?}");
        }
        for text in [
            "make test",
            "lsof -i :8080",
            "git commit -m 'fix the env loader'",
            "git clone https://github.com/a/b.git",
            "aws s3 ls",
            "ffmpeg -i in.mov -vf scale=1280:-2 out.mp4",
            "echo x >> .gitignore",
        ] {
            assert!(!out(text), "{text:?}");
        }
        assert!(!left_out("ls", "/work/app", &[], &never), "an empty list skips nothing");
    }

    /// The never list is asked of where a command ran too: in `~/.ssh`, `cat config` names a
    /// key file as surely as `cat ~/.ssh/config` does elsewhere.
    #[test]
    fn nothing_is_kept_of_a_command_run_in_a_folder_that_is_never_read() {
        let never = Never::new(&["config/secrets/*.yml".to_owned(), "vault".to_owned()]);
        for (cwd, text) in [
            ("/home/me/.ssh", "cat config"),
            ("/home/me/.aws", "grep key credentials"),
            ("/home/me/.gnupg/private-keys-v1.d", "make"),
            ("/srv/vault/prod", "head -3 users.txt"),
            // A pattern with a folder in it: the word, from that folder.
            ("/srv/app/config/secrets", "cat prod.yml"),
            ("/srv/app/config", "cat secrets/prod.yml"),
        ] {
            assert!(left_out(text, cwd, &[], &never), "{cwd}: {text}");
        }
        for (cwd, text) in [
            ("/home/me/src/sshd", "cat config"),
            ("/srv/app/config", "cat app.yml"),
            ("/srv/app/config/secrets", "make"),
            ("", "make test"),
        ] {
            assert!(!left_out(text, cwd, &[], &never), "{cwd}: {text}");
        }
    }

    #[test]
    fn a_command_lines_own_secrets_are_taken_out() {
        for (line, hidden) in [
            ("export FOO_KEY=abc123", "export FOO_KEY=<redacted>"),
            (
                "export OPENAI_API_KEY=\"sk live\" DEBUG=1",
                "export OPENAI_API_KEY=<redacted> DEBUG=1",
            ),
            (
                "PGPASSWORD=hunter2 psql -h db -p 5432 shop",
                "PGPASSWORD=<redacted> psql -h db -p 5432 shop",
            ),
            ("mysql -u root -phunter2 shop", "mysql -u root -p<redacted> shop"),
            (
                "mysqldump -uroot -p'my pass' shop > dump.sql",
                "mysqldump -uroot -p<redacted> shop > dump.sql",
            ),
            ("sshpass -p hunter2 ssh host", "sshpass -p <redacted> ssh host"),
            (
                "echo x | docker login -u me -p hunter2 ghcr.io",
                "echo x | docker login -u me -p <redacted> ghcr.io",
            ),
            ("redis-cli -a hunter2 ping", "redis-cli -a <redacted> ping"),
            ("gh auth login --with-token hunter2", "gh auth login --with-token <redacted>"),
            ("tool --password hunter2 --verbose", "tool --password <redacted> --verbose"),
            ("tool --api-key=hunter2 run", "tool --api-key=<redacted> run"),
            ("curl -u me:hunter2 https://example.com", "curl -u me:<redacted> https://example.com"),
            (
                "sudo curl --user=me:hunter2 https://example.com",
                "sudo curl --user=me:<redacted> https://example.com",
            ),
        ] {
            assert_eq!(hide(line), hidden, "{line}");
            assert_eq!(hide(hidden), hidden, "twice changes nothing: {line}");
        }
        // What stands for a value, and what only looks like one of these, stays.
        for line in [
            "export GITHUB_TOKEN=$(gh auth token)",
            "tool --password \"$PASS\"",
            "mysql -u root -p shop",
            "psql -p 5433 shop",
            "git log --author=neal -p",
            "ssh -p 2222 host",
            "curl -u me https://example.com",
            "make KEYBOARD=us test",
            "ffmpeg -i in.mov -vf scale=1280:-2 out.mp4",
            "tool --key",
        ] {
            assert_eq!(hide(line), line);
        }
    }

    #[test]
    fn a_command_is_a_line_under_where_and_how_it_ran() {
        let mut ran = Ran {
            text: "make  test".into(),
            cwd: "/home/a/proj".into(),
            started_ms: 1_000,
            ended_ms: 13_400,
            exit: Some(2),
            output: None,
        };
        let a = anchor(&ran.cwd, "make  test");
        let p = parts(&ran, &a, 3);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].kind, p[0].text.as_str()), (ChunkKind::Command, "$ make  test"));
        assert_eq!(p[0].heading.as_deref(), Some("in /home/a/proj · exit 2 · took 12s · 3 times"));
        assert_eq!(p[0].anchor.as_deref(), Some(a.as_str()));
        let meta = p[0].meta.as_ref().unwrap();
        assert_eq!(count(Some(meta)), 3);
        assert_eq!((meta["exit"].as_i64(), meta["ms"].as_u64()), (Some(2), Some(12_400)));
        assert_eq!(p[0].at_ms, 13_400);

        // Quick, clean and once: only where.
        ran.exit = Some(0);
        ran.ended_ms = 1_200;
        ran.output = Some("ok\n".into());
        let p = parts(&ran, &a, 1);
        assert_eq!(p[0].heading.as_deref(), Some("in /home/a/proj"));
        // What it printed stands beside it, under the same anchor.
        assert_eq!((p[1].kind, p[1].text.as_str()), (ChunkKind::Output, "ok\n"));
        assert_eq!(p[1].anchor, p[0].anchor);
        assert_eq!(p[1].heading.as_deref(), Some("$ make test"));

        // The same line elsewhere is another chunk; a long line is cut.
        assert_ne!(anchor("/elsewhere", "make  test"), a);
        ran.text = "é".repeat(TEXT);
        assert!(parts(&ran, &a, 1)[0].text.len() <= TEXT + 2);
        assert_eq!(count(None), 1);
    }

    #[test]
    fn a_day_and_its_uri() {
        assert_eq!(day(1_795_132_800_000), "2026-11-20");
        assert_eq!(uri("0192abcd", "2026-11-20"), "shell://0192abcd/2026-11-20");
        assert_eq!(took(400), "0.4s");
        assert_eq!(took(185_000), "3m 05s");
        assert_eq!(took(7_800_000), "2h 10m");
    }
}
