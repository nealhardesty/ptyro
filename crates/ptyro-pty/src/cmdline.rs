//! Windows command lines (WIN5). A Windows program gets one string and parses it into argv
//! itself, by the MSVC rules; a batch file is run by cmd.exe, which has rules of its own. Pure
//! string work, so Unix runs its tests too.

use std::io;
use std::path::{Path, PathBuf};

/// The program to start for `prog`, found the way a shell would: a path is used as it is (with a
/// `PATHEXT` extension added when it has none); a bare name is looked for in each `PATH`
/// directory with each `PATHEXT` extension. Only names with such an extension count: npm puts an
/// extensionless shell script beside `claude.cmd`, which Windows can't start.
pub fn resolve(prog: &str, path: Option<&str>, pathext: Option<&str>) -> io::Result<PathBuf> {
    let pathext = pathext.filter(|p| !p.is_empty()).unwrap_or(".COM;.EXE;.BAT;.CMD");
    let names = ptyro_core::os::windows_program_names(prog, pathext);
    let runnable = |p: &Path| ptyro_core::os::windows_runnable(p, pathext) && p.is_file();
    if prog.contains(['\\', '/']) || Path::new(prog).is_absolute() {
        if let Some(p) = names.iter().map(PathBuf::from).find(|p| runnable(p)) {
            return Ok(p);
        }
    } else {
        for dir in std::env::split_paths(path.unwrap_or_default()) {
            if let Some(p) = names.iter().map(|n| dir.join(n)).find(|p| runnable(p)) {
                return Ok(p);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{prog}: not found (PATH, PATHEXT)")))
}

/// Whether Windows runs `program` through cmd.exe: a `.bat` or `.cmd` file.
pub fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"))
}

/// The command line that starts `program` with `args`. A batch file goes through `cmd` (the
/// full path to System32's cmd.exe), with its arguments quoted for cmd.exe; an argument cmd.exe
/// can't be given safely (a line break, a NUL) is refused rather than mangled.
pub fn command_line(program: &Path, args: &[String], cmd: &Path) -> io::Result<String> {
    let mut line = String::new();
    if is_batch(program) {
        // `/d`: no AutoRun commands; `/e:ON /v:OFF`: extensions on, `!` literal; `/c "…"`: cmd
        // takes everything between the outer quotes as the command.
        quote_program(cmd, &mut line);
        line.push_str(" /e:ON /v:OFF /d /c \"");
        quote_program(program, &mut line);
        for a in args {
            line.push(' ');
            quote_batch_arg(a, &mut line)?;
        }
        line.push('"');
    } else if let Some(tail) = cmd_tail(program, args) {
        quote_program(program, &mut line);
        line.push(' ');
        line.push_str(&tail);
    } else {
        quote_program(program, &mut line);
        for a in args {
            if a.contains('\0') {
                return Err(nul(a));
            }
            line.push(' ');
            quote_arg(a, &mut line);
        }
    }
    Ok(line)
}

/// cmd.exe reads the text after `/c` (or `/k`) itself, not by the MSVC rules (a `\"` is two
/// characters to it): its switches go as they are, and that text raw, in quotes when `/s` asks
/// cmd to strip them. `None` for any other program, or a cmd.exe with no command.
pub fn cmd_tail(program: &Path, args: &[String]) -> Option<String> {
    // By either separator: a Windows path, read where `\` may not be one (tests on Unix).
    let full = program.to_string_lossy();
    let name = full.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
    if name != "cmd.exe" && name != "cmd" {
        return None;
    }
    let at =
        args.iter().position(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("/k"))?;
    let switches = args[..=at].join(" ");
    let text = args[at + 1..].join(" ");
    let quoted = args[..at].iter().any(|a| a.eq_ignore_ascii_case("/s"));
    Some(if quoted { format!("{switches} \"{text}\"") } else { format!("{switches} {text}") })
}

fn nul(a: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("an argument holds a NUL: {a:?}"))
}

/// The program, in quotes (a path never holds a `"`).
fn quote_program(p: &Path, out: &mut String) {
    out.push('"');
    out.push_str(&p.to_string_lossy());
    out.push('"');
}

/// One argument for a program that splits its command line by the MSVC rules: in quotes when
/// it has spaces, tabs or quotes (or is empty); a quote is `\"`; backslashes are doubled only
/// before a quote.
pub fn quote_arg(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1)),
            _ => out.extend(std::iter::repeat_n('\\', backslashes)),
        }
        out.push(c);
        backslashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// One argument for a batch file, as cmd.exe reads it: in quotes when it has anything cmd treats
/// specially; a quote is `""`; `%` is `%%cd:~,%`, which expands to nothing and a `%`, so no
/// `%VAR%` in an argument expands. Line breaks and NULs can't be passed: refused.
fn quote_batch_arg(arg: &str, out: &mut String) -> io::Result<()> {
    if arg.contains(['\r', '\n', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a batch file can't be given an argument with a line break: {arg:?}"),
        ));
    }
    const SPECIAL: &[char] =
        &[' ', '\t', '&', '|', '<', '>', '^', ',', ';', '=', '(', ')', '%', '!', '"', '\'', '`'];
    if !arg.is_empty() && !arg.contains(SPECIAL) {
        out.push_str(arg);
        return Ok(());
    }
    out.push('"');
    for c in arg.chars() {
        match c {
            '"' => out.push_str("\"\""),
            '%' => out.push_str("%%cd:~,%"),
            _ => out.push(c),
        }
    }
    // A trailing backslash would escape the closing quote for a program the batch file starts.
    let trailing = arg.chars().rev().take_while(|c| *c == '\\').count();
    out.extend(std::iter::repeat_n('\\', trailing));
    out.push('"');
    Ok(())
}

/// A command line split into its arguments by the MSVC rules (`CommandLineToArgvW`'s, which
/// [`quote_arg`] writes for): the program's name ends at the first space or tab unless quoted,
/// then backslashes are literal except before a quote, `2n` of them and a quote are `n` and an
/// opening or closing quote, `2n+1` and a quote are `n` and a literal `"`.
pub fn split(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    // The program: no backslash rules, a quote just toggles.
    let mut prog = String::new();
    let mut quoted = false;
    while let Some(&c) = chars.peek() {
        if !quoted && (c == ' ' || c == '\t') {
            break;
        }
        chars.next();
        if c == '"' {
            quoted = !quoted;
        } else {
            prog.push(c);
        }
    }
    args.push(prog);
    loop {
        while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
            chars.next();
        }
        if chars.peek().is_none() {
            return args;
        }
        let mut arg = String::new();
        let mut quoted = false;
        loop {
            match chars.peek().copied() {
                None => break,
                Some(' ' | '\t') if !quoted => break,
                Some('\\') => {
                    let mut n = 0;
                    while chars.peek() == Some(&'\\') {
                        chars.next();
                        n += 1;
                    }
                    if chars.peek() == Some(&'"') {
                        arg.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            chars.next();
                            arg.push('"');
                        }
                    } else {
                        arg.extend(std::iter::repeat_n('\\', n));
                    }
                }
                Some('"') => {
                    chars.next();
                    // `""` inside quotes is a literal quote (the CRT's rule).
                    if quoted && chars.peek() == Some(&'"') {
                        chars.next();
                        arg.push('"');
                    } else {
                        quoted = !quoted;
                    }
                }
                Some(c) => {
                    chars.next();
                    arg.push(c);
                }
            }
        }
        args.push(arg);
    }
}

/// The script an interpreter's command line runs, as a name: its file's stem, or for a
/// package's generic `cli.js`/`index.js`, the folder it's in (`node …\codex\bin\codex.js` is
/// `codex`; `node …\claude-code\cli.js` is `claude-code`).
pub fn script_name(line: &str) -> Option<String> {
    let args = split(line);
    let script = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    let mut parts = script.rsplit(['\\', '/']);
    let file = parts.next()?;
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    if ["cli", "index", "main", "bin", "run"].contains(&stem.to_ascii_lowercase().as_str()) {
        return parts.next().filter(|d| !d.is_empty()).map(str::to_owned);
    }
    (!stem.is_empty()).then(|| stem.to_owned())
}

/// The environment block `CreateProcessW` takes: `K=V` pairs in UTF-16, each NUL-ended, sorted
/// by name without regard to case (as Windows keeps it), and a final NUL.
pub fn env_block(env: &[(std::ffi::OsString, std::ffi::OsString)]) -> Vec<u16> {
    let mut pairs: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| !k.is_empty() && !k.contains('='))
        .collect();
    pairs.sort_by_key(|(k, _)| k.to_uppercase());
    pairs.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in pairs {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// A variable from a child's environment, by name without regard to case (as Windows reads it).
pub fn env_var<'a>(
    env: &'a [(std::ffi::OsString, std::ffi::OsString)],
    name: &str,
) -> Option<&'a str> {
    env.iter()
        .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(a: &str) -> String {
        let mut s = String::new();
        quote_arg(a, &mut s);
        s
    }

    #[test]
    fn arguments_are_quoted_the_msvc_way() {
        assert_eq!(q("plain"), "plain");
        assert_eq!(q(""), "\"\"");
        assert_eq!(q("a b"), "\"a b\"");
        assert_eq!(q(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(q(r"C:\dir\"), r"C:\dir\");
        assert_eq!(q(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(q(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(q(r"a\\b c"), r#""a\\b c""#);
    }

    #[test]
    fn a_command_line_splits_back_into_what_was_quoted() {
        let argv =
            ["plain", "", "a b", r#"say "hi""#, r"C:\my dir\", r#"a\"b"#, r"a\\b c", "tab\there"];
        let mut line = String::from(r#""C:\Program Files\node.exe""#);
        for a in argv {
            line.push(' ');
            quote_arg(a, &mut line);
        }
        let back = split(&line);
        assert_eq!(back[0], r"C:\Program Files\node.exe");
        assert_eq!(&back[1..], argv);
        assert_eq!(split(r"node.exe  C:\x\cli.js --flag"), ["node.exe", r"C:\x\cli.js", "--flag"]);
    }

    #[test]
    fn an_interpreters_program_is_the_script_it_runs() {
        assert_eq!(
            script_name(
                r#""C:\nodejs\node.exe" C:\npm\node_modules\@openai\codex\bin\codex.js exec"#
            )
            .as_deref(),
            Some("codex")
        );
        assert_eq!(
            script_name(r"node --no-warnings C:\npm\node_modules\@anthropic-ai\claude-code\cli.js")
                .as_deref(),
            Some("claude-code")
        );
        assert_eq!(script_name("python -m http.server").as_deref(), Some("http"));
        assert_eq!(script_name("node").as_deref(), None);
    }

    #[test]
    fn a_program_and_its_arguments_make_one_line() {
        let cmd = Path::new(r"C:\Windows\System32\cmd.exe");
        let l =
            command_line(Path::new(r"C:\Program Files\x\x.exe"), &["-a".into(), "b c".into()], cmd);
        assert_eq!(l.unwrap(), r#""C:\Program Files\x\x.exe" -a "b c""#);
        assert!(command_line(Path::new("x.exe"), &["a\0b".into()], cmd).is_err());
    }

    #[test]
    fn cmd_gets_its_command_text_as_written() {
        let cmd = Path::new(r"C:\Windows\System32\cmd.exe");
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // What `command_argv` makes for cmd: /s strips the quotes around the whole text.
        let l = command_line(cmd, &args(&["/d", "/s", "/c", r#"echo "a b" & exit /b 3"#]), cmd);
        assert_eq!(
            l.unwrap(),
            r#""C:\Windows\System32\cmd.exe" /d /s /c "echo "a b" & exit /b 3""#
        );
        // Without /s: the words as they are.
        assert_eq!(cmd_tail(cmd, &args(&["/c", "dir", "/b"])).unwrap(), "/c dir /b");
        assert_eq!(cmd_tail(Path::new("pwsh.exe"), &args(&["/c", "x"])), None);
        assert_eq!(cmd_tail(cmd, &args(&["/d"])), None);
    }

    #[test]
    fn a_batch_file_runs_through_cmd_with_nothing_expanded() {
        let cmd = Path::new(r"C:\Windows\System32\cmd.exe");
        let script = Path::new(r"C:\npm\claude.CMD");
        let l = command_line(script, &["--print".into(), "50% & done".into(), "x\"y".into()], cmd);
        assert_eq!(
            l.unwrap(),
            r#""C:\Windows\System32\cmd.exe" /e:ON /v:OFF /d /c ""C:\npm\claude.CMD" --print "50%%cd:~,% & done" "x""y"""#
        );
        assert!(command_line(script, &["two\nlines".into()], cmd).is_err());
        let trailing = command_line(script, &[r"C:\a dir\".into()], cmd).unwrap();
        assert!(trailing.ends_with(r#""C:\a dir\\"""#), "{trailing}");
        assert!(is_batch(Path::new("x.bat")) && !is_batch(Path::new("x.exe")));
    }

    #[test]
    fn the_environment_block_is_sorted_and_ends_twice() {
        let env = [("b", "2"), ("A", "1"), ("a", "dup"), ("=C:", "x")]
            .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)));
        let block = env_block(&env);
        let text = String::from_utf16(&block).unwrap();
        assert_eq!(text, "A=1\0b=2\0\0");
        assert_eq!(env_block(&[]), [0, 0]);
        assert_eq!(env_var(&env, "B"), Some("2"));
    }

    #[test]
    fn a_program_is_found_with_its_extension_only() {
        let dir = tempfile::tempdir().unwrap();
        // npm's layout: an extensionless shell script beside the .cmd that Windows can run.
        std::fs::write(dir.path().join("claude"), "#!/bin/sh").unwrap();
        std::fs::write(dir.path().join("claude.cmd"), "@echo off").unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let found = resolve("claude", Some(&path), Some(".COM;.EXE;.BAT;.CMD")).unwrap();
        assert_eq!(found, dir.path().join("claude.cmd"));
        assert!(resolve("nothing", Some(&path), None).is_err());
        let full = dir.path().join("claude");
        assert_eq!(
            resolve(&full.to_string_lossy(), None, None).unwrap(),
            dir.path().join("claude.cmd")
        );
    }
}
