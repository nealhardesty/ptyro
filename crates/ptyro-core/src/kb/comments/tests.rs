use super::*;

/// The kept blocks as (text, the line of code).
fn kept(name: &str, text: &str) -> Vec<(String, String)> {
    extract(name, text).into_iter().map(|b| (b.text, b.code)).collect()
}

fn texts(name: &str, text: &str) -> Vec<String> {
    extract(name, text).into_iter().map(|b| b.text).collect()
}

fn verdicts(name: &str, text: &str) -> Vec<Verdict> {
    judge(name, text).into_iter().map(|(_, v)| v).collect()
}

#[test]
fn c_family_whole_lines_and_blocks() {
    let src = r#"#include <stdio.h>

// Opens the connection and waits for the greeting.
// The timeout is the caller's.
int open_conn(const char *url) {
    const char *base = "http://example.com/a//b"; // a trailing comment is not read
    int n = 1; /* nor is this one */
    /*
     * Retries are counted here, not by the caller:
     * the caller only sees the last error.
     */
    return retry(url, n);
}

/** Closes it, and forgets the greeting. */
void close_conn(void);
"#;
    let got = extract("net.c", src);
    assert_eq!(got.len(), 3, "{got:#?}");
    assert_eq!(
        got[0].text,
        "Opens the connection and waits for the greeting.\nThe timeout is the caller's."
    );
    assert_eq!(got[0].code, "int open_conn(const char *url) {");
    assert_eq!((got[0].start, got[0].end), (3, 4));
    assert_eq!(got[0].anchor(), "L3-L4");
    // The block's frame and the `*` that leads each line are gone.
    assert_eq!(
        got[1].text,
        "Retries are counted here, not by the caller:\nthe caller only sees the last error."
    );
    assert_eq!(got[1].code, "return retry(url, n);");
    assert_eq!((got[1].start, got[1].end), (8, 11));
    assert_eq!(got[2].text, "Closes it, and forgets the greeting.");
    assert_eq!(got[2].code, "void close_conn(void);");
    // Nothing of the string or the trailing comments got in.
    assert!(got.iter().all(|b| !b.text.contains("example.com") && !b.text.contains("trailing")));
}

#[test]
fn rust_attributes_docs_and_nested_blocks() {
    let src = r#"//! The session model: what a session is and how it ends.
//! Nothing here touches a terminal.

#[derive(Debug, Clone)]
#[allow(dead_code)]
/// One running program and its screen.
///
/// ```
/// let s = Session::new();
/// s.close();
/// ```
pub struct Session;

/* An outer note /* with an inner one */ that goes on after it,
   and only ends here. */
fn after() {}
"#;
    let got = kept("session.rs", src);
    assert_eq!(got.len(), 3, "{got:#?}");
    assert_eq!(
        got[0].0,
        "The session model: what a session is and how it ends.\nNothing here touches a terminal."
    );
    // The attributes are code: the module comment stands on the first of them.
    assert_eq!(got[0].1, "#[derive(Debug, Clone)]");
    // A documentation comment keeps its example, and is not "code switched off" for having one.
    assert!(got[1].0.starts_with("One running program and its screen."));
    assert!(got[1].0.contains("let s = Session::new();"));
    assert_eq!(got[1].1, "pub struct Session;");
    // Rust's blocks nest: the first `*/` doesn't end the comment.
    assert_eq!(
        got[2].0,
        "An outer note /* with an inner one */ that goes on after it,\nand only ends here."
    );
    assert_eq!(got[2].1, "fn after() {}");
    assert!(got.iter().all(|(t, _)| !t.contains("derive")));
}

#[test]
fn hash_family_shebang_strings_and_attributes() {
    let src = r##"#!/usr/bin/env bash
# Installs the tools this project needs, once.
set -euo pipefail

echo "# this is a string, not a comment"
url="https://example.com/#anchor"   # trailing: not read
#[not, a, comment]
# Asks before it overwrites anything the user made.
install_all
"##;
    let got = kept("install.sh", src);
    assert_eq!(
        got,
        vec![
            ("Installs the tools this project needs, once.".to_owned(), "set -euo pipefail".into()),
            ("Asks before it overwrites anything the user made.".into(), "install_all".into()),
        ]
    );
    // The same file with no extension is known by its first line.
    assert_eq!(kept("install", src).len(), 2);
    assert!(kept("install.unknown", src).is_empty());
}

#[test]
fn markdown_is_a_document_not_source() {
    let md = "# A heading\n\nSome text.\n\n## Another heading, long enough to be kept\n";
    assert!(judge("README.md", md).is_empty());
    assert!(marks("README.md", md).is_none());
    assert!(marks("notes.txt", "# not a comment either").is_none());
    assert!(marks("data.json", "{}").is_none());
}

#[test]
fn the_table_knows_files_by_extension_name_and_interpreter() {
    assert_eq!(marks("a/b/main.go", "").map(|m| m.line), Some(&["//"][..]));
    assert_eq!(marks("x.PY", "").map(|m| m.docstrings), Some(true));
    assert_eq!(marks("Makefile", "").map(|m| m.line), Some(&["#"][..]));
    assert_eq!(marks("docker/Dockerfile.dev", "").map(|m| m.line), Some(&["#"][..]));
    assert_eq!(marks("CMakeLists.txt", "").map(|m| m.line), Some(&["#"][..]));
    assert_eq!(marks("q.sql", "").map(|m| m.line), Some(&["--"][..]));
    assert_eq!(marks("page.html", "").map(|m| m.block), Some(&[("<!--", "-->")][..]));
    assert_eq!(marks("C:\\src\\lib.rs", "").map(|m| m.nested), Some(true));
    assert_eq!(marks("tool", "#!/usr/bin/env python3\n").map(|m| m.docstrings), Some(true));
    assert_eq!(marks("tool", "#!/usr/bin/env node\n").map(|m| m.line), Some(&["//"][..]));
    assert!(marks("tool", "no interpreter line").is_none());
    assert!(marks(".gitignore", "# comment").is_none());
}

#[test]
fn code_switched_off_is_dropped_and_prose_that_looks_like_it_is_kept() {
    let c = r#"int f(void) {
    // int old = compute(1, 2);
    // if (old > 3) {
    //     report(old);
    // }
    return 0;
}
"#;
    assert_eq!(verdicts("f.c", c), [Verdict::Code]);
    let py = "x = 1\n# y = compute(x)\n# print(y)\nz = 2\n";
    assert_eq!(verdicts("f.py", py), [Verdict::Code]);
    let sh =
        "run\n# sudo apt install --yes wine64 winbind\n# cd /var/tmp\n# VERSION=1.21.0\ndone\n";
    assert_eq!(verdicts("f.sh", sh), [Verdict::Code]);
    let toml = "[a]\n# timeout = 30\nb = 1\n";
    assert_eq!(verdicts("f.toml", toml), [Verdict::Code]);

    // Prose ends in `;` and `)` too, names calls, and quotes code between backticks.
    for prose in [
        "// Generous: registration runs git a few times, which takes seconds on a loaded machine;\n// a wait ends as soon as what it waits for is true.",
        "// (A busy machine can take a moment to refuse connections to a closed socket;\n// until it does, `bind` rightly leaves the file alone.)",
        "// Handle len([]byte(substr))",
        "// MinGW does not support uselocale() and xlocale.h and gettimeofday()",
        "// The same as `Adapter::transcript(&session)`, for a session that ended (see `close()`);",
        "// Since = is handled specially, make sure all existing uses of it still work.",
        "// make sure the flag is set before the first call",
        "// set -e would end the script here, which is why the check is spelled out",
    ] {
        let src = format!("int a;\n{prose}\nint b;\n");
        assert_eq!(verdicts("f.c", &src), [Verdict::Keep], "{prose}");
    }

    // An explanation with an example further in than its text is prose: only the lines at the
    // margin are judged.
    let go = "package a\n\n// Misplaced occurs when a \"...\" is used somewhere other than the\n// final argument to a function call.\n//\n// Example:\n//\n//\tfunc printArgs(args ...int) {}\n//\tfunc f() {\n//\t\tprintArgs(a...)\n//\t}\nconst Misplaced = 1\n";
    assert_eq!(verdicts("a.go", go), [Verdict::Keep]);
}

#[test]
fn a_licence_header_is_dropped_and_only_at_the_top() {
    let go = "// Copyright 2024 The Authors. All rights reserved.\n// Use of this source code is governed by a BSD-style\n// license that can be found in the LICENSE file.\n\n// Package retry runs a function again until it succeeds.\npackage retry\n\n// Copyright notices in a response are passed through untouched.\nfunc f() {}\n";
    assert_eq!(verdicts("retry.go", go), [Verdict::Licence, Verdict::Keep, Verdict::Keep]);
    let c = "/*\n * SPDX-License-Identifier: MIT\n */\nint x;\n";
    assert_eq!(verdicts("x.c", c), [Verdict::Licence]);
    // A file's opening comment may be *about* licences.
    let rs = "//! `ptyro about`: the version, the repository, the licence, the build.\nuse x;\n";
    assert_eq!(verdicts("about.rs", rs), [Verdict::Keep]);
}

#[test]
fn directives_and_stubs_are_dropped() {
    let ts = r#"// eslint-disable-next-line @typescript-eslint/no-explicit-any
const a: any = 1;
// @ts-expect-error the types are wrong upstream
b();
/// <reference lib="webworker" />
c();
// TODO
d();
// ── drawing ─────────────────────────
e();
// Typed: what a name can't hold is replaced, as in every name prompt.
f();
"#;
    assert_eq!(
        verdicts("a.ts", ts),
        [
            Verdict::Directive,
            Verdict::Directive,
            Verdict::Directive,
            Verdict::Short,
            Verdict::Short,
            Verdict::Keep
        ]
    );
    let go = "//go:build linux && amd64\n\n// +build linux\n\n// Go: the language this is written in, as it happens.\npackage a\n";
    assert_eq!(verdicts("a.go", go), [Verdict::Directive, Verdict::Directive, Verdict::Keep]);
    let py = "# -*- coding: utf-8 -*-\nimport os  # noqa\n# type: ignore\n# START OF GENERATED QUERIES\nx = 1\n";
    // A directive and a label in one run are no directive, and still nothing to keep.
    assert_eq!(verdicts("a.py", py), [Verdict::Directive, Verdict::Short]);
    // Rules are not text.
    let sh = "# ------------------------------------------------\n# Section: the things that are installed first of all\n# ------------------------------------------------\nrun\n";
    assert_eq!(texts("a.sh", sh), ["Section: the things that are installed first of all"]);
}

#[test]
fn short_is_measured_in_words() {
    assert_eq!(verdicts("a.rs", "// For people.\nfn a() {}\n"), [Verdict::Short]);
    assert_eq!(verdicts("a.rs", "// Rename a workspace\nfn a() {}\n"), [Verdict::Keep]);
    assert_eq!(verdicts("a.rs", "// MANA THISTLE GROUP THREE\nfn a() {}\n"), [Verdict::Short]);
    // A script without spaces between its words is not one long word.
    assert_eq!(verdicts("a.rs", "// 这个函数检查用户的输入是否有效\nfn a() {}\n"), [Verdict::Keep]);
}

#[test]
fn python_docstrings_and_strings() {
    let src = r##"#!/usr/bin/env python3
"""Reads the transcript of a session.

Only the turns are kept.
"""
import os

SQL = """
# not a comment: this line is inside a string
select 1
"""

def read(path):
    """Return the turns of the transcript at `path`, oldest first."""
    # The file may still be growing: read to the last whole line.
    text = f"""{path} is not a docstring either"""
    return text


class Reader:
    '''Keeps the place where reading stopped.'''

    def close(self):
        r"""
        Forget the place; the next read starts over.
        """
        x = '''a string that
        ends here''' + 'and is followed by code'
        return x
"##;
    let got = kept("read.py", src);
    assert_eq!(
        got,
        vec![
            (
                "Reads the transcript of a session.\n\nOnly the turns are kept.".to_owned(),
                String::new()
            ),
            (
                "Return the turns of the transcript at `path`, oldest first.".into(),
                "def read(path):".into()
            ),
            (
                "The file may still be growing: read to the last whole line.".into(),
                "text = f\"\"\"{path} is not a docstring either\"\"\"".into()
            ),
            ("Keeps the place where reading stopped.".into(), "class Reader:".into()),
            ("Forget the place; the next read starts over.".into(), "def close(self):".into()),
        ],
        "{got:#?}"
    );
    let blocks = extract("read.py", src);
    assert_eq!((blocks[0].start, blocks[0].end), (2, 5));
    assert_eq!((blocks[1].start, blocks[1].end), (14, 14));
}

#[test]
fn other_families() {
    let html = "<html>\n<!-- The navigation is built by the script below,\n     so that it is the same on every page. -->\n<nav></nav>\n<p>a <!-- trailing --> b</p>\n</html>\n";
    assert_eq!(
        kept("index.html", html),
        vec![(
            "The navigation is built by the script below,\nso that it is the same on every page."
                .to_owned(),
            "<nav></nav>".to_owned()
        )]
    );
    let sql = "/*!40101 SET @saved_cs_client = @@character_set_client */;\n-- Every account that has not signed in for a year.\nSELECT id FROM account WHERE seen < now() - 365;\n/* Kept for the audit; remove after the migration. */\nSELECT 1;\n";
    assert_eq!(
        texts("q.sql", sql),
        [
            "Every account that has not signed in for a year.",
            "Kept for the audit; remove after the migration."
        ]
    );
    let lua = "--[[ The table of handlers, by event name.\n     A handler returns true to stop the others. ]]\nlocal handlers = {}\n--- Calls every handler of the event in turn.\nfunction fire() end\n";
    assert_eq!(
        kept("h.lua", lua),
        vec![
            (
                "The table of handlers, by event name.\nA handler returns true to stop the others."
                    .to_owned(),
                "local handlers = {}".to_owned()
            ),
            ("Calls every handler of the event in turn.".into(), "function fire() end".into()),
        ]
    );
    let hs = "{- A queue with {- nested -} notes, which Haskell allows,\n   and that ends only here. -}\nmodule Q where\n-- | The empty queue holds nothing at all.\nempty = []\n";
    assert_eq!(kept("Q.hs", hs).len(), 2);
    assert_eq!(kept("Q.hs", hs)[0].1, "module Q where");
    let ml = "(* Folds the list from the left, as the standard one does. *)\nlet fold = List.fold_left\n";
    assert_eq!(texts("f.ml", ml), ["Folds the list from the left, as the standard one does."]);
    let el = ";;; The mode's keymap, built once when the file loads.\n(defvar map nil)\n";
    assert_eq!(texts("m.el", el), ["The mode's keymap, built once when the file loads."]);
    let tex = "% The title page is set by hand, not by the class.\n\\begin{document}\n";
    assert_eq!(texts("a.tex", tex), ["The title page is set by hand, not by the class."]);
    let css = "/* The sidebar keeps its width when the page scrolls. */\n.side { width: 20em; }\n// not a comment in CSS, whatever it says here\n";
    assert_eq!(texts("a.css", css), ["The sidebar keeps its width when the page scrolls."]);
    let rb = "=begin\nThe parser reads one record a line and stops at the first bad one.\n=end\nclass Parser; end\n";
    assert_eq!(
        texts("p.rb", rb),
        ["The parser reads one record a line and stops at the first bad one."]
    );
}

#[test]
fn runs_blank_lines_and_the_line_of_code() {
    let src = "// The first note stands alone, above a blank line.\n\n// The second note is about the function below it.\n\nfn below() {}\n// The third note has no code after it at all.\n";
    let got = kept("a.rs", src);
    assert_eq!(got.len(), 3);
    // A comment that another comment follows (after a blank line) has no line of its own.
    assert_eq!(got[0].1, "");
    assert_eq!(got[1].1, "fn below() {}");
    assert_eq!(got[2].1, "");
    // Lines one after the other are one block, whatever their marks.
    let src = "/* The header says what the frame holds. */\n// The body follows it at once, with no gap.\nint x;\n";
    assert_eq!(
        texts("a.c", src),
        ["The header says what the frame holds.\nThe body follows it at once, with no gap."]
    );
    // Code after a block's end, on its line, is the line of code.
    let src = "/* The flag that says the cache is warm enough. */ static int warm;\n";
    assert_eq!(kept("a.c", src)[0].1, "static int warm;");
    // The line of code is cut at 200 characters.
    let long = format!(
        "// A long line of code follows this comment here.\nlet x = \"{}\";\n",
        "é".repeat(400)
    );
    assert_eq!(kept("a.rs", &long)[0].1.chars().count(), CODE_CHARS);
}

#[test]
fn an_unterminated_block_does_not_swallow_the_file() {
    let src = "/* this block never ends\nint a;\n// A comment after it is still read as a comment.\nint b;\n/* nor does this one\n";
    assert_eq!(texts("a.c", src), ["A comment after it is still read as a comment."]);
    // Nested: an inner block that closes leaves the outer one open to the end.
    let src = "/* outer /* inner */ never closed\nfn a() {}\n// Still read, though the block above is broken.\nfn b() {}\n/* A later block that does close is read too. */\n";
    assert_eq!(
        texts("a.rs", src),
        [
            "Still read, though the block above is broken.",
            "A later block that does close is read too."
        ]
    );
    let py = "x = \"\"\"never closed\n# A comment in a file whose string never ends.\ny = 1\n";
    assert_eq!(texts("a.py", py), ["A comment in a file whose string never ends."]);
}

#[test]
fn crlf_bom_and_bytes_that_are_not_utf8() {
    let src = "\u{feff}// The first line of a file saved on Windows.\r\n// It has a mark at its start and two bytes at each end.\r\nint x;\r\n\r\n/*\r\n * And a block, with the same line ends.\r\n */\r\nint y;\r\n";
    let got = kept("a.c", src);
    assert_eq!(
        got,
        vec![
            (
                "The first line of a file saved on Windows.\nIt has a mark at its start and two bytes at each end."
                    .to_owned(),
                "int x;".to_owned()
            ),
            ("And a block, with the same line ends.".into(), "int y;".into()),
        ]
    );
    // Latin-1 bytes: read lossily, the comment survives with the bad bytes replaced.
    let bytes = b"// Gr\xfc\xdfe aus M\xfcnchen, in an old encoding.\nint x;\n";
    let text = String::from_utf8_lossy(bytes);
    assert_eq!(
        texts("a.c", &text),
        ["Gr\u{fffd}\u{fffd}e aus M\u{fffd}nchen, in an old encoding."]
    );
}

/// Ten megabytes of each shape that could make a careless scan quadratic. Each is read in a
/// time that only a linear pass can keep (a quadratic one takes minutes).
#[test]
fn ten_megabytes_are_read_in_linear_time() {
    const SIZE: usize = 10 << 20;
    let fill = |unit: &str| unit.repeat(SIZE / unit.len());
    let turn_about = "// What the next line does, said plainly.\nlet x = compute(1);\n";
    let half_open = "/* never /* closed */\nlet x = 1;\n";
    let cases: [(&str, String, usize); 7] = [
        // One enormous run of comment lines: one block.
        ("a.rs", fill("// A line of a comment that goes on and on.\n"), 1),
        // Comments and code, turn about.
        ("a.rs", fill(turn_about), SIZE / turn_about.len()),
        // Blocks that never close (each would be looked for to the end of the file).
        ("a.c", fill("/* never closed\nint x;\n"), 0),
        // (Nested ones: after the first is found open, the rest are read without nesting.)
        ("a.rs", fill(half_open), SIZE / half_open.len() - 1),
        // One line of ten megabytes, full of openers and closers.
        ("a.rs", format!("/* {} */ fn f() {{}}\n", fill("/* a */ ")), 1),
        // Python strings that never close, and that open and close all along one line.
        ("a.py", fill("x = \"\"\"never closed\n"), 0),
        ("a.py", format!("x = {}\n", fill("\"\"\"a\"\"\" + '''b''' + ")), 0),
    ];
    for (name, text, least) in cases {
        let t = std::time::Instant::now();
        let got = judge(name, &text);
        let took = t.elapsed();
        assert_eq!(got.len(), least, "{name}");
        assert!(took.as_secs() < 20, "{name}: {took:?} for {} bytes", text.len());
    }
}
