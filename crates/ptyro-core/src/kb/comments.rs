//! Source comments, by simple rules (SPEC §25.6.3): a table from a file's name to its language's
//! comment marks, and one pass over the lines. No parser per language.
//!
//! Only **whole-line comments and blocks that start a line** are read. A trailing comment is
//! not, and so neither is a `//` or a `#` inside a string on a line of code: nothing here parses
//! strings, except Python's triple quotes, which is where its docstrings are. A run of comment
//! lines is one [`Block`], kept with the line of code it stands on. [`judge`] says of each block
//! whether it is prose worth finding or one of the things that only look like it: a licence
//! header, a tool's directive, code that was switched off, a few stray characters.

/// One run of comment lines, its marks stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Its first and last line, counted from 1.
    pub start: usize,
    pub end: usize,
    /// The comment's own text: no marks, no decoration, no blank line at either end.
    pub text: String,
    /// The line of code it is about, at most [`CODE_CHARS`] characters: the first line of code
    /// after it, or for a Python docstring the `def` or `class` line above it. Empty when there
    /// is none (the end of the file, or another comment after a blank line).
    pub code: String,
}

impl Block {
    /// Its lines as an anchor: `L120-L161`.
    pub fn anchor(&self) -> String {
        format!("L{}-L{}", self.start, self.end)
    }
}

/// What [`judge`] makes of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Prose a person might look for.
    Keep,
    /// Every line is an instruction to a tool (`eslint-disable`, `noqa`, `go:build`, `-*- … -*-`).
    Directive,
    /// It stands before the file's first line of code and names a copyright or a licence.
    Licence,
    /// Code that was switched off: more than half of its lines read as code.
    Code,
    /// Under [`MIN_PROSE`] letters in words, or under three words (words in capitals only
    /// are not counted: they are labels).
    Short,
}

/// The letters (in words) a block needs to be kept.
pub const MIN_PROSE: usize = 12;
/// How much of the following line of code a block keeps.
pub const CODE_CHARS: usize = 200;

/// A language family's comment marks.
#[derive(Debug, PartialEq, Eq)]
pub struct Marks {
    /// What starts a comment that runs to the end of its line.
    pub line: &'static [&'static str],
    /// What opens and closes a block comment.
    pub block: &'static [(&'static str, &'static str)],
    /// Whether a block inside a block must be closed too.
    pub nested: bool,
    /// Python: a triple-quoted string that opens a module, a class or a function is its comment.
    pub docstrings: bool,
}

const fn marks_of(
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    nested: bool,
) -> Marks {
    Marks { line, block, nested, docstrings: false }
}

static C: Marks = marks_of(&["//"], &[("/*", "*/")], false);
static C_NESTED: Marks = marks_of(&["//"], &[("/*", "*/")], true);
static CSS: Marks = marks_of(&[], &[("/*", "*/")], false);
static HASH: Marks = marks_of(&["#"], &[], false);
static PYTHON: Marks = Marks { line: &["#"], block: &[], nested: false, docstrings: true };
static RUBY: Marks = marks_of(&["#"], &[("=begin", "=end")], false);
static PHP: Marks = marks_of(&["//", "#"], &[("/*", "*/")], false);
static HCL: Marks = marks_of(&["#", "//"], &[("/*", "*/")], false);
static NIX: Marks = marks_of(&["#"], &[("/*", "*/")], false);
static POWERSHELL: Marks = marks_of(&["#"], &[("<#", "#>")], false);
static JULIA: Marks = marks_of(&["#"], &[("#=", "=#")], true);
static SQL: Marks = marks_of(&["--"], &[("/*", "*/")], false);
static LUA: Marks = marks_of(&["--"], &[("--[[", "]]")], false);
static HASKELL: Marks = marks_of(&["--"], &[("{-", "-}")], true);
static DASH: Marks = marks_of(&["--"], &[], false);
static HTML: Marks = marks_of(&[], &[("<!--", "-->")], false);
static VUE: Marks = marks_of(&["//"], &[("/*", "*/"), ("<!--", "-->")], false);
static LISP: Marks = marks_of(&[";"], &[], false);
static INI: Marks = marks_of(&[";", "#"], &[], false);
static PERCENT: Marks = marks_of(&["%"], &[], false);
static ML: Marks = marks_of(&[], &[("(*", "*)")], true);
static FSHARP: Marks = marks_of(&["//"], &[("(*", "*)")], true);

/// The table: file extensions (lower case) to marks. Markdown and the other documents are not
/// here: they are text, not source (§25.6.3's Documents tier).
static BY_EXTENSION: &[(&[&str], &Marks)] = &[
    (
        &[
            "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "m", "mm", "go", "java", "js", "jsx",
            "mjs", "cjs", "ts", "tsx", "mts", "cts", "cs", "zig", "groovy", "gradle", "proto",
            "scss", "sass", "less", "jsonc", "json5", "glsl", "v", "sv", "cu", "ino",
        ],
        &C,
    ),
    (&["rs", "swift", "kt", "kts", "scala", "sc", "dart", "d"], &C_NESTED),
    (&["css"], &CSS),
    (
        &[
            "sh",
            "bash",
            "zsh",
            "fish",
            "ksh",
            "yaml",
            "yml",
            "toml",
            "mk",
            "mak",
            "cmake",
            "ex",
            "exs",
            "r",
            "pl",
            "pm",
            "tcl",
            "awk",
            "nim",
            "cr",
            "graphql",
            "gql",
            "properties",
            "conf",
            "dockerfile",
        ],
        &HASH,
    ),
    (&["py", "pyi", "pyw"], &PYTHON),
    (&["rb", "rake", "gemspec"], &RUBY),
    (&["php"], &PHP),
    (&["tf", "tfvars", "hcl"], &HCL),
    (&["nix"], &NIX),
    (&["ps1", "psm1", "psd1"], &POWERSHELL),
    (&["jl"], &JULIA),
    (&["sql"], &SQL),
    (&["lua"], &LUA),
    (&["hs", "elm", "purs"], &HASKELL),
    (&["ada", "adb", "ads", "vhd", "vhdl"], &DASH),
    (&["html", "htm", "xhtml", "xml", "xsd", "xsl"], &HTML),
    (&["vue", "svelte", "astro"], &VUE),
    (&["lisp", "cl", "el", "clj", "cljs", "cljc", "edn", "scm", "ss", "rkt"], &LISP),
    (&["ini", "cfg"], &INI),
    (&["tex", "sty", "cls", "erl", "hrl"], &PERCENT),
    (&["ml", "mli"], &ML),
    (&["fs", "fsi", "fsx"], &FSHARP),
];

/// Files known by their whole name (lower case).
static BY_NAME: &[(&[&str], &Marks)] = &[
    (
        &[
            "makefile",
            "gnumakefile",
            "dockerfile",
            "containerfile",
            "justfile",
            "cmakelists.txt",
            "procfile",
        ],
        &HASH,
    ),
    (&["rakefile", "gemfile", "vagrantfile", "brewfile", "podfile"], &RUBY),
];

/// The marks of the file called `name` (a path will do), or `None` when it isn't source this
/// table knows. A file with no extension is known by its `#!` line, so `text` is its contents
/// (the first line is enough).
pub fn marks(name: &str, text: &str) -> Option<&'static Marks> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    let find = |table: &'static [(&[&str], &Marks)], key: &str| {
        table.iter().find(|(keys, _)| keys.contains(&key)).map(|(_, m)| *m)
    };
    if let Some(m) = find(BY_NAME, &base) {
        return Some(m);
    }
    if base.starts_with("dockerfile.") {
        return Some(&HASH);
    }
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => find(BY_EXTENSION, ext),
        // No extension (or a dot file): the interpreter line says what it is.
        _ => {
            let first = text.lines().next().unwrap_or_default();
            let program = first.strip_prefix("#!")?.rsplit(['/', ' ']).next().unwrap_or_default();
            let is = |names: &[&str]| names.iter().any(|n| program.starts_with(n));
            if is(&["python", "pypy"]) {
                Some(&PYTHON)
            } else if is(&["ruby"]) {
                Some(&RUBY)
            } else if is(&["node", "deno", "bun"]) {
                Some(&C)
            } else if is(&["sh", "bash", "zsh", "dash", "ksh", "fish", "perl", "awk", "make"]) {
                Some(&HASH)
            } else {
                None
            }
        }
    }
}

/// The comments of `text` worth keeping: [`judge`]'s blocks whose verdict is [`Verdict::Keep`].
pub fn extract(name: &str, text: &str) -> Vec<Block> {
    judge(name, text).into_iter().filter(|(_, v)| *v == Verdict::Keep).map(|(b, _)| b).collect()
}

/// Every comment block of `text`, in order, each with what the rules make of it. Empty for a
/// file whose name [`marks`] doesn't know. One pass; the time is linear in the text.
pub fn judge(name: &str, text: &str) -> Vec<(Block, Verdict)> {
    let Some(marks) = marks(name, text) else { return Vec::new() };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut scan = Scan {
        marks,
        nested: marks.nested,
        out: Vec::new(),
        run: None,
        seen_code: false,
        dead: [false; 4],
        above: "",
        opens_body: true,
    };
    let mut i = 0;
    while i < lines.len() {
        i = scan.line(&lines, i);
    }
    scan.close("");
    scan.out
}

/// A run of comment lines being gathered.
struct Run {
    start: usize,
    end: usize,
    lines: Vec<String>,
    /// Every part of it is a documentation comment (`///`, `//!`, `/** */`).
    doc: bool,
    /// It started before the file's first line of code.
    heads_file: bool,
    /// A blank line came after it: nothing more joins it, and only code can be its line.
    ended: bool,
}

struct Scan<'a> {
    marks: &'static Marks,
    /// Whether blocks still nest: off once one didn't close, so a broken file stays linear.
    nested: bool,
    out: Vec<(Block, Verdict)>,
    run: Option<Run>,
    seen_code: bool,
    /// A closer (by its place in `marks.block`, then Python's `"""` and `'''`) that was looked
    /// for to the end of the file and not found: nothing later can close either.
    dead: [bool; 4],
    /// Python: the last line of code, and whether a string right after it is a docstring (it
    /// ended in `:`, or nothing came before).
    above: &'a str,
    opens_body: bool,
}

impl<'a> Scan<'a> {
    /// Reads the line at `i` (and what it opens) and returns where to go on.
    fn line(&mut self, lines: &[&'a str], i: usize) -> usize {
        let t = lines[i].trim();
        // The interpreter line is neither a comment nor the file's first line of code.
        if i == 0 && t.starts_with("#!") {
            return 1;
        }
        if t.is_empty() {
            if let Some(run) = &mut self.run {
                run.ended = true;
            }
            return i + 1;
        }
        let marks = self.marks;
        for (b, (open, close)) in marks.block.iter().enumerate() {
            let Some(rest) = t.strip_prefix(open) else { continue };
            // MySQL runs `/*!40101 SET … */`: it is a statement, not a comment.
            let runs =
                rest.strip_prefix('!').is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
            if self.dead[b] || runs {
                break;
            }
            let Some((end, last, after)) = self.closer(lines, i, rest, open, close) else {
                // A nested block left open says nothing of plain ones further on: they are
                // looked for once more, without nesting, and then no more.
                self.dead[b] = !self.nested;
                self.nested = false;
                break;
            };
            let mut body: Vec<&str> = Vec::with_capacity(end - i + 1);
            if end == i {
                body.push(last);
            } else {
                body.push(rest);
                body.extend(&lines[i + 1..end]);
                body.push(last);
            }
            let (cleaned, doc) = clean_block(&body);
            self.add(i, end, cleaned, doc);
            if !after.trim().is_empty() {
                self.close(after);
            }
            return end + 1;
        }
        for mark in marks.line {
            let Some(rest) = t.strip_prefix(mark) else { continue };
            // `#!/bin/sh` and `#[attribute]` are not comments in any language that has them.
            if *mark == "#" && (rest.starts_with('!') || rest.starts_with('[')) {
                break;
            }
            let (cleaned, doc) = clean_line(rest, mark);
            self.add(i, i, vec![cleaned], doc);
            return i + 1;
        }
        if marks.docstrings {
            return self.python(lines, i, t);
        }
        self.close(t);
        i + 1
    }

    /// Where the block that opened on line `i` closes: the line, that line's text before the
    /// closer (after the opener, on line `i`), and what follows the closer.
    fn closer(
        &self,
        lines: &[&'a str],
        i: usize,
        rest: &'a str,
        open: &str,
        close: &str,
    ) -> Option<(usize, &'a str, &'a str)> {
        let mut depth = 1usize;
        for (j, line) in std::iter::once(rest).chain(lines[i + 1..].iter().copied()).enumerate() {
            if !self.nested {
                if let Some(at) = line.find(close) {
                    return Some((i + j, &line[..at], &line[at + close.len()..]));
                }
                continue;
            }
            // Openers and closers in the order they come; each `find` starts where the last
            // one of its kind ended, so a long line is read once.
            let from = |what: &str, at: usize| line[at..].find(what).map(|p| p + at);
            let (mut o, mut c) = (from(open, 0), from(close, 0));
            while let Some(shut) = c {
                match o {
                    Some(opened) if opened < shut => {
                        depth += 1;
                        let next = opened + open.len();
                        o = from(open, next);
                        if shut < next {
                            c = from(close, next);
                        }
                    }
                    _ => {
                        depth -= 1;
                        let next = shut + close.len();
                        if depth == 0 {
                            return Some((i + j, &line[..shut], &line[next..]));
                        }
                        c = from(close, next);
                        if o.is_some_and(|opened| opened < next) {
                            o = from(open, next);
                        }
                    }
                }
            }
            if let Some(opened) = o {
                depth += line[opened..].matches(open).count();
            }
        }
        None
    }

    /// A line of Python that is not a `#` comment: a docstring, or code whose triple-quoted
    /// strings are stepped over (their lines are neither comments nor docstrings).
    fn python(&mut self, lines: &[&'a str], i: usize, t: &'a str) -> usize {
        let prefix = t.len() - t.trim_start_matches(['r', 'R', 'u', 'U', 'b', 'B', 'f', 'F']).len();
        let quoted = &t[prefix..];
        let delim = ["\"\"\"", "'''"].into_iter().find(|d| quoted.starts_with(d));
        if let (Some(delim), true) = (delim, prefix <= 2)
            && let Some((end, body)) = self.string(lines, i, &quoted[3..], delim)
        {
            let plain = !t[..prefix].contains(['b', 'B', 'f', 'F']);
            if plain && self.opens_body {
                self.close("");
                let (cleaned, _) = clean_block(&body);
                self.add(i, end, cleaned, true);
                let above = self.above;
                self.close(above);
            } else {
                self.close(t);
            }
            self.opens_body = false;
            return end + 1;
        }
        self.close(t);
        self.above = t;
        self.opens_body = t.ends_with(':');
        // A string that opens on this line and doesn't close on it: skip to where it does.
        let (bytes, mut at) = (t.as_bytes(), 0);
        let mut open: Option<(&str, usize)> = None;
        while at + 3 <= bytes.len() {
            let delim = match &bytes[at..at + 3] {
                b"\"\"\"" => "\"\"\"",
                b"'''" => "'''",
                _ => {
                    at += 1;
                    continue;
                }
            };
            at += 3;
            match open {
                None => open = Some((delim, at)),
                Some((opened, _)) if opened == delim => open = None,
                Some(_) => {}
            }
        }
        if let Some((delim, from)) = open {
            return self.string(lines, i, &t[from..], delim).map_or(i + 1, |(end, _)| end + 1);
        }
        i + 1
    }

    /// A triple-quoted string whose text after the opening quotes on line `i` is `rest`: the
    /// line it closes on and its lines. `None` when it never closes.
    fn string(
        &mut self,
        lines: &[&'a str],
        i: usize,
        rest: &'a str,
        delim: &str,
    ) -> Option<(usize, Vec<&'a str>)> {
        if let Some(at) = rest.find(delim) {
            return Some((i, vec![&rest[..at]]));
        }
        let dead = 2 + usize::from(delim == "'''");
        if self.dead[dead] {
            return None;
        }
        let Some((n, at)) =
            lines[i + 1..].iter().enumerate().find_map(|(n, l)| l.find(delim).map(|at| (n, at)))
        else {
            self.dead[dead] = true;
            return None;
        };
        let end = i + 1 + n;
        let mut body = vec![rest];
        body.extend(&lines[i + 1..end]);
        body.push(&lines[end][..at]);
        Some((end, body))
    }

    /// Lines `start..=end` (from 0) are a comment: they join the run above them, or start one.
    fn add(&mut self, start: usize, end: usize, lines: Vec<String>, doc: bool) {
        if self.run.as_ref().is_some_and(|r| r.ended) {
            self.close("");
        }
        match &mut self.run {
            Some(run) => {
                run.end = end;
                run.doc &= doc;
                run.lines.extend(lines);
            }
            None => {
                self.run =
                    Some(Run { start, end, lines, doc, heads_file: !self.seen_code, ended: false });
            }
        }
    }

    /// The run, if there is one, is finished, and `code` is the line it stands on. A line of
    /// code was read (unless `code` is empty).
    fn close(&mut self, code: &str) {
        if !code.is_empty() {
            self.seen_code = true;
        }
        let Some(run) = self.run.take() else { return };
        let lines: Vec<&str> = run.lines.iter().map(|l| l.trim_end()).collect();
        let Some(first) = lines.iter().position(|l| !l.is_empty()) else { return };
        let last = lines.iter().rposition(|l| !l.is_empty()).unwrap_or(first);
        let lines = &lines[first..=last];
        let verdict = verdict(lines, run.doc, run.heads_file);
        let code: String = code.trim().chars().take(CODE_CHARS).collect();
        let block = Block { start: run.start + 1, end: run.end + 1, text: lines.join("\n"), code };
        self.out.push((block, verdict));
    }
}

/// A line comment's text after its mark: the mark's repeats (`///`, `##`, `;;;`), a doc mark
/// and one space go. Returns the text and whether it was a documentation comment.
fn clean_line(rest: &str, mark: &str) -> (String, bool) {
    let last = mark.chars().next_back().unwrap_or(' ');
    let more = rest.trim_start_matches(last);
    let mut doc = mark == "//" && rest.len() - more.len() == 1;
    let mut text = more;
    if mark == "//"
        && let Some(inner) = text.strip_prefix('!')
    {
        (doc, text) = (true, inner);
    }
    let text = text.strip_prefix([' ', '\t']).unwrap_or(text);
    (undecorated(text).to_owned(), doc)
}

/// A block comment's lines without its frame: the opener's own `*` or `!` (which makes it a
/// documentation comment), the `*` that leads each line when every line has one, else the
/// indentation they share.
fn clean_block(body: &[&str]) -> (Vec<String>, bool) {
    let first = body.first().copied().unwrap_or_default();
    let opened = first.trim_start_matches(['*', '!']);
    let doc = opened.len() < first.len();
    let rest = body.get(1..).unwrap_or_default();
    let starred = rest.iter().all(|l| {
        let t = l.trim_start();
        t.is_empty() || t.starts_with('*')
    });
    let indent = rest
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = vec![undecorated(opened.trim()).to_owned()];
    for l in rest {
        let text = if starred {
            let t = l.trim_start().trim_start_matches('*');
            t.strip_prefix(' ').unwrap_or(t)
        } else {
            l.get(indent..).unwrap_or_default()
        };
        out.push(undecorated(text).to_owned());
    }
    (out, doc)
}

/// A line that is only a rule (`-----`, `=====`, `* * *`) is nothing.
fn undecorated(line: &str) -> &str {
    if line.chars().any(char::is_alphanumeric) { line } else { "" }
}

fn verdict(lines: &[&str], doc: bool, heads_file: bool) -> Verdict {
    let said: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if said.iter().all(|l| is_directive(l)) {
        return Verdict::Directive;
    }
    if heads_file && said.iter().any(|l| names_licence(l)) {
        return Verdict::Licence;
    }
    // Nobody switches code off with a documentation comment; its lines of code are examples.
    // So are the lines of any comment that stand further in than its text, and what is
    // between ``` fences: only the lines at the margin are judged.
    if !doc {
        let indent = |l: &str| l.len() - l.trim_start().len();
        let margin = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| indent(l)).min();
        let mut fenced = false;
        let (mut judged, mut code) = (0, 0);
        for l in lines.iter().filter(|l| !l.trim().is_empty()) {
            if l.trim_start().starts_with("```") {
                fenced = !fenced;
            } else if !fenced && Some(indent(l)) == margin {
                judged += 1;
                code += usize::from(looks_like_code(l.trim()));
            }
        }
        if code * 2 > judged {
            return Verdict::Code;
        }
    }
    // Words: letters only, and not shouted (`END OF QUERIES`, `MANA THISTLE`). A
    // script that writes no spaces between its words counts one for every two characters.
    let (mut count, mut letters) = (0, 0);
    for w in said.iter().flat_map(|l| l.split(|c: char| !c.is_alphabetic())) {
        let n = w.chars().count();
        if n == 0 || (n > 1 && w.chars().all(char::is_uppercase)) {
            continue;
        }
        count += if w.is_ascii() { 1 } else { n.div_ceil(2) };
        letters += n;
    }
    if count < 3 || letters < MIN_PROSE {
        return Verdict::Short;
    }
    Verdict::Keep
}

/// What a tool reads, not a person: each is the start of a comment's line (lower case). The
/// ones that are also English words carry what follows them, so that prose isn't caught
/// ("Typed: what a name can't hold").
const DIRECTIVES: &[&str] = &[
    "eslint-",
    "eslint ",
    "prettier-ignore",
    "biome-ignore",
    "deno-lint-ignore",
    "@ts-",
    "tslint:",
    "istanbul ignore",
    "c8 ignore",
    "v8 ignore",
    "@flow",
    "@jsx",
    "@jest-environment",
    "@vitest-environment",
    "@generated",
    "<reference ",
    "#region",
    "#endregion",
    "sourcemappingurl=",
    "noqa",
    "type: ignore",
    "pylint:",
    "mypy:",
    "flake8:",
    "isort:",
    "fmt: off",
    "fmt: on",
    "fmt: skip",
    "yapf:",
    "nosec",
    "pragma:",
    "pragma ",
    "noinspection",
    "shellcheck ",
    "nolint",
    "+build",
    "lint:ignore",
    "lint:file-ignore",
    "-*-",
    "vim:",
    "vi: set",
    "frozen_string_literal:",
    "coding: utf",
    "coding=utf",
    "encoding: utf",
    "rubocop:",
    "swiftlint:",
    "ktlint-",
    "cspell:",
    "spell-checker:",
    "codespell:",
    "syntax=",
    "yaml-language-server:",
    "clang-format off",
    "clang-format on",
    "typed: true",
    "typed: false",
    "typed: strict",
    "typed: strong",
    "typed: ignore",
];

fn is_directive(line: &str) -> bool {
    let head: String = line.chars().take(24).flat_map(char::to_lowercase).collect();
    // Go's own (`//go:build`, `//go:fix inline`): no space after the colon, as no prose has.
    let go =
        head.strip_prefix("go:").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_lowercase()));
    go || DIRECTIVES.iter().any(|d| head.starts_with(d))
}

/// What a licence header says. "Licence" alone is not enough: a file's opening comment may
/// well be about licences.
const LICENCE: &[&str] = &[
    "copyright",
    "©",
    "spdx-license",
    "licensed under",
    "licensed to",
    "all rights reserved",
    "permission is hereby granted",
    "redistribution and use",
    "governed by a",
    "public license",
    "mit license",
    "apache license",
    "bsd license",
    "license, version",
    "without warranty",
    "without warranties",
];

fn names_licence(line: &str) -> bool {
    let l = line.to_lowercase();
    LICENCE.iter().any(|w| l.contains(w))
}

/// Whether a comment's line reads as code. `{` and `}` end no sentence. A line that ends as
/// a statement does (`;`) must also hold a call or an operator and few plain words; one that
/// ends in a bracket must *start* as code does (`name(`, `a.b`, `return`), because prose ends
/// in a call all the time ("Handle len(slice)"). A shell command has none of these endings:
/// it is known by its first word and a flag, a path or a variable after it. What stands
/// between backticks is prose quoting code, and is not looked at.
fn looks_like_code(line: &str) -> bool {
    let l = line.trim_end_matches(',').trim_end();
    if l.ends_with(['{', '}']) {
        return true;
    }
    let plain: String = l.split('`').step_by(2).collect();
    let plain = plain.trim();
    let first = plain.split(' ').next().unwrap_or_default();
    let named = |name: &str| {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_.[]\"'".contains(c))
    };
    if first == plain {
        // One word: `call();`, `list[0]`, `NAME=value`.
        return plain.ends_with([';', ')', ']'])
            || plain.split_once('=').is_some_and(|(name, value)| named(name) && !value.is_empty());
    }
    let call = |s: &str| {
        s.match_indices('(')
            .any(|(at, _)| s[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
    };
    let sign = call(plain)
        || [" = ", "==", "!=", "::", "->", "=>", "+=", "&&", "||", ":="]
            .iter()
            .any(|s| plain.contains(s));
    // Words as prose has them: letters only, and not shouted (SQL's keywords are).
    let words = plain
        .split(' ')
        .map(|w| w.trim_matches(|c: char| ",.;:!?\"'".contains(c)))
        .filter(|w| {
            w.chars().count() >= 2
                && w.chars().all(char::is_alphabetic)
                && w.chars().any(char::is_lowercase)
        })
        .count();
    // `name = value`, with nothing but a name (and a `let` or a type) before the sign.
    let assigned = [" = ", " += ", " := "].iter().any(|op| {
        plain.split_once(op).is_some_and(|(name, value)| {
            !value.is_empty() && name.split(' ').count() <= 2 && name.split(' ').all(named)
        })
    });
    if plain.ends_with(';') {
        return sign && words < 4;
    }
    let starts = call(first)
        || first.contains(['[', '='])
        || first.contains("->")
        || first.contains("::")
        || first.trim_end_matches('.').contains('.')
        || STATEMENTS.contains(&first);
    if plain.ends_with([')', ']']) {
        return assigned || (sign && starts);
    }
    // A call that goes on over the next lines: `log(FILTER, "…",`.
    if (line.trim_end().ends_with(',') || plain.ends_with('(')) && call(first) {
        return true;
    }
    if COMMANDS.contains(&first) && words < 6 && !plain.ends_with(['.', ':', '?', '!']) {
        return plain.split(' ').skip(1).any(|w| {
            w.starts_with(['-', '$', '~', '/', '|', '>', '.', '"', '\''])
                || w.contains(['/', '='])
                || w.chars().any(|c| c.is_ascii_digit())
        });
    }
    assigned && words < 4
}

/// First words of a statement that are seldom the first word of a sentence written in lower
/// case.
const STATEMENTS: &[&str] = &[
    "return", "if", "elif", "while", "await", "yield", "raise", "throw", "assert", "del", "print",
    "import", "let", "var", "const", "def", "fn", "func",
];

/// First words of a shell command. Several are English ("make sure…", "set the…"), so one
/// counts only with a flag, a path, a variable or a number after it.
const COMMANDS: &[&str] = &[
    "sudo",
    "cd",
    "mkdir",
    "rm",
    "cp",
    "mv",
    "ln",
    "chmod",
    "chown",
    "echo",
    "export",
    "source",
    "set",
    "unset",
    "alias",
    "apt",
    "apt-get",
    "dnf",
    "yum",
    "pacman",
    "brew",
    "pip",
    "pip3",
    "npm",
    "npx",
    "yarn",
    "pnpm",
    "cargo",
    "go",
    "make",
    "git",
    "docker",
    "kubectl",
    "curl",
    "wget",
    "tar",
    "cat",
    "ls",
    "grep",
    "sed",
    "awk",
    "systemctl",
    "python",
    "python3",
    "node",
    "bash",
    "sh",
    "exec",
    "exit",
    "bind-key",
    "bind",
    "RUN",
    "ENV",
    "COPY",
];

#[cfg(test)]
mod tests;
