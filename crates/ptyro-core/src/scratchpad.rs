//! The scratchpad (NOTE1, SPEC §13.6): persistent notes files for ideas and bugs, plain
//! Markdown: one for everywhere (`$PTYRO_SCRATCHPAD`) and one per project
//! (`$PTYRO_PROJECT_SCRATCHPAD`, NOTE1.7). This is its file logic, with no daemon in it: the
//! file and its history, the `- [ ]` items, and the three-way merge that lets the web, the CLI
//! and an editor all write it without anyone's words being lost.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What a new global pad starts with.
pub const HEADER: &str = "\
# Scratchpad — everywhere

Ideas, bugs, things to try. Plain Markdown: `- [ ]` lines are items you can start as tasks
(`ptyro scratchpad start`, or ⏎ on one in the picker). Add from anywhere:
`ptyro scratchpad add \"…\"`; this file is `$PTYRO_SCRATCHPAD`. Each project has a pad of its
own too (`ptyro scratchpad --project`); inside a project that one is the default.

";

/// What a new project pad starts with.
pub fn project_header(name: &str) -> String {
    format!(
        "\
# Scratchpad — {name} (this project)

Ideas, bugs, things to try for {name}. Plain Markdown: `- [ ]` lines are items you can start as
tasks (`ptyro scratchpad start`, or ⏎ on one in the picker). Every worktree of the project shares
this file (`$PTYRO_PROJECT_SCRATCHPAD`); git ignores it (`.git/info/exclude`), so it stays yours.
`ptyro scratchpad add \"…\"` inside the project adds here; `--global` adds to the pad for everywhere.

"
    )
}

/// The project pad of the checkout at `root`: `<root>/.ptyro/scratchpad.md`.
pub fn project_pad_path(root: &Path) -> PathBuf {
    root.join(".ptyro").join("scratchpad.md")
}

/// Its saved copies, beside it.
pub fn project_history_dir(root: &Path) -> PathBuf {
    root.join(".ptyro").join("scratchpad.history")
}

/// The line for the repository's `.git/info/exclude`: the pad, its saved copies and the temp
/// files of a write, and nothing else in `.ptyro/` (recipes and change sets there are committed).
pub const EXCLUDE_LINE: &str = "/.ptyro/scratchpad.*";

/// Which pad: the one for everywhere, or a project's. Requests default to `Global`, so a client
/// that predates project pads works as it did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadSel {
    #[default]
    Global,
    /// A project, by id, slug or a path inside it (replies and events carry its id).
    Project(String),
    /// The pad of the project this directory is in, else the global one (requests only).
    Near(PathBuf),
}

impl PadSel {
    pub fn is_global(&self) -> bool {
        *self == Self::Global
    }
}

/// A revision: the first 12 hex digits of the text's SHA-256.
pub fn rev(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// One `- [ ]` / `- [x]` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// 1-based line number in the file.
    pub line: u32,
    pub done: bool,
    /// The words, without the checkbox, the stamp or the tags.
    pub text: String,
    /// The heading it is under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// When it was added (`2026-10-24 14:02`), from its stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Where it was added (a worktree or directory), from its stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whence: Option<String>,
    /// The agent that added it (`claude-rook`), when not a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The worktree started from it (`[wt:name]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// The pull request that finished it (`(#42)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
}

/// An entry to add (`ptyro scratchpad add`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub text: String,
    /// Under this heading (made at the end when missing).
    #[serde(default)]
    pub section: Option<String>,
    /// A plain bullet instead of a `- [ ]` item.
    #[serde(default)]
    pub note: bool,
    /// `2026-10-24 14:02`; none for no stamp at all.
    #[serde(default)]
    pub at: Option<String>,
    /// Where from: a worktree address or a directory name.
    #[serde(default)]
    pub whence: Option<String>,
    /// An agent's name.
    #[serde(default)]
    pub by: Option<String>,
}

/// Text for inside a stamp: one line, with neither the backtick nor the ` · ` that
/// [`unstamp`] reads the stamp back by.
fn stamp_text(s: &str) -> String {
    s.replace(['\n', '\r'], " ").replace('`', "'").replace(" · ", " - ")
}

impl Entry {
    /// The line (and indented continuation lines) this entry is.
    pub fn render(&self) -> String {
        let mut head = String::new();
        if let Some(at) = &self.at {
            head.push_str(at);
            head.push_str(" · ");
            if let Some(w) = &self.whence {
                head.push_str(&format!("`{}` · ", stamp_text(w)));
            }
            if let Some(b) = &self.by {
                head.push_str(&format!("by `{}` · ", stamp_text(b)));
            }
        }
        let mut lines = self.text.trim_end().lines();
        let first = lines.next().unwrap_or("");
        let mut out = format!("- {}{head}{first}", if self.note { "" } else { "[ ] " });
        for l in lines {
            out.push_str("\n  ");
            out.push_str(l);
        }
        out.push('\n');
        out
    }
}

/// A saved copy of the pad.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rev: String,
    pub at_ms: u64,
    pub bytes: u64,
    pub lines: u32,
}

/// The result of writing over a text that changed meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub text: String,
    /// Regions where both sides changed the same lines and both are kept between markers.
    pub conflicts: u32,
}

/// Where the pad and its history live.
#[derive(Debug, Clone)]
pub struct Pad {
    pub path: PathBuf,
    history: PathBuf,
    keep_days: u64,
    /// What a new file starts with.
    header: String,
}

/// A change seen from outside (an editor that saves as you type) isn't saved again within this
/// long of the last copy.
const LAZY_MS: u64 = 15_000;
/// Snapshots never thinned or pruned by age: the newest few.
const KEEP_NEWEST: usize = 20;
/// Most snapshots kept, whatever their age.
const KEEP_MAX: usize = 400;
const HOUR_MS: u64 = 3_600_000;

impl Pad {
    pub fn new(path: PathBuf, history: PathBuf, keep_days: u64) -> Self {
        Self { path, history, keep_days, header: HEADER.to_owned() }
    }

    /// The pad of the project `name` whose main checkout is `root`.
    pub fn project(root: &Path, name: &str, keep_days: u64) -> Self {
        let mut p = Self::new(project_pad_path(root), project_history_dir(root), keep_days);
        p.header = project_header(name);
        p
    }

    /// The file itself, if it is a symlink into a synced folder: writes go through it.
    fn real(&self) -> PathBuf {
        std::fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone())
    }

    /// Make the file (with its header) when it doesn't exist yet.
    pub fn ensure(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&self.path) {
            Ok(mut f) => f.write_all(self.header.as_bytes()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The text (made first when there is none).
    pub fn read(&self) -> std::io::Result<String> {
        self.ensure()?;
        let bytes = std::fs::read(&self.path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Replace the file (atomically), saving what it held first. A file that isn't UTF-8 text
    /// is left alone: the lossy copy we'd save and write back would be the only one left.
    pub fn write(&self, text: &str) -> std::io::Result<()> {
        self.ensure()?;
        let real = self.real();
        let bytes = std::fs::read(&real)?;
        let Ok(old) = String::from_utf8(bytes) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the scratchpad isn't valid UTF-8 text; fix it in your editor first (it is left as it is)",
            ));
        };
        if old != text {
            let _ = self.snapshot(&old);
        }
        // A temp file of its own beside it, with the file's own permissions (a private pad stays
        // private), so two writers never share one.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let tmp = real.with_extension(format!(
            "md.{}.{}.tmp",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let put = || -> std::io::Result<()> {
            std::fs::write(&tmp, text)?;
            if let Ok(m) = std::fs::metadata(&real) {
                std::fs::set_permissions(&tmp, m.permissions())?;
            }
            std::fs::rename(&tmp, &real)
        };
        let r = put();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    /// Add to the end with a single append, so an editor with the file open sees only the
    /// added bytes.
    pub fn append_raw(&self, text: &str) -> std::io::Result<()> {
        let cur = self.read()?;
        // (Only text we can read back whole is worth a copy.)
        if std::fs::read(&self.path).is_ok_and(|b| std::str::from_utf8(&b).is_ok()) {
            let _ = self.snapshot(&cur);
        }
        let mut f = std::fs::OpenOptions::new().append(true).open(self.real())?;
        let mut add = String::new();
        if !cur.is_empty() && !cur.ends_with('\n') {
            add.push('\n');
        }
        add.push_str(text);
        f.write_all(add.as_bytes())
    }

    /// Add an entry: at the end, or under its heading.
    pub fn add(&self, e: &Entry) -> std::io::Result<String> {
        let line = e.render();
        match &e.section {
            None => self.append_raw(&line)?,
            Some(sec) => {
                let cur = self.read()?;
                self.write(&insert_under(&cur, sec, &line))?;
            }
        }
        self.read()
    }

    /// Save `text` as a snapshot, unless the newest one is the same.
    pub fn snapshot(&self, text: &str) -> std::io::Result<()> {
        self.save(text, false)
    }

    /// The same for a change seen from outside: skipped when the newest copy is very recent, so
    /// an editor that saves every keystroke doesn't fill the history.
    pub fn snapshot_lazy(&self, text: &str) -> std::io::Result<()> {
        self.save(text, true)
    }

    fn save(&self, text: &str, lazy: bool) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.history)?;
        let r = rev(text);
        let now = crate::now_ms();
        if let Some((at, last, _)) = self.list_files().last()
            && (*last == r || (lazy && now.saturating_sub(*at) < LAZY_MS))
        {
            return Ok(());
        }
        std::fs::write(self.history.join(format!("{now}-{r}.md")), text)?;
        self.prune();
        Ok(())
    }

    /// `(at_ms, rev, path)`, oldest first.
    fn list_files(&self) -> Vec<(u64, String, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.history) else { return Vec::new() };
        let mut v: Vec<(u64, String, PathBuf)> = rd
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".md")?;
                let (at, r) = stem.split_once('-')?;
                Some((at.parse().ok()?, r.to_owned(), e.path()))
            })
            .collect();
        v.sort();
        v
    }

    /// Old copies thin out: every one for the first hour, then one per ten minutes for a day,
    /// then one per hour, until `keep` days; the newest few and a total cap always hold.
    fn prune(&self) {
        let files = self.list_files();
        let now = crate::now_ms();
        let cutoff = now.saturating_sub(self.keep_days.saturating_mul(86_400_000));
        let n = files.len();
        let mut seen = std::collections::HashSet::new();
        for (i, (at, _, p)) in files.iter().enumerate().rev() {
            let from_newest = n - 1 - i;
            let age = now.saturating_sub(*at);
            let bucket = if age < HOUR_MS {
                *at
            } else if age < 24 * HOUR_MS {
                1 << 62 | (*at / (HOUR_MS / 6))
            } else {
                1 << 63 | (*at / HOUR_MS)
            };
            let dup = !seen.insert(bucket);
            let old = *at < cutoff;
            if from_newest >= KEEP_NEWEST && (dup || old) || from_newest >= KEEP_MAX {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// Saved copies, newest first.
    pub fn history(&self) -> Vec<Snapshot> {
        let mut v: Vec<Snapshot> = self
            .list_files()
            .into_iter()
            .filter_map(|(at_ms, rev, p)| {
                let text = std::fs::read_to_string(p).ok()?;
                Some(Snapshot {
                    rev,
                    at_ms,
                    bytes: text.len() as u64,
                    lines: u32::try_from(text.lines().count()).unwrap_or(u32::MAX),
                })
            })
            .collect();
        v.reverse();
        v
    }

    /// A saved copy's text, by revision (any unique prefix).
    pub fn snapshot_text(&self, rev_prefix: &str) -> Option<String> {
        if rev_prefix.is_empty() {
            return None;
        }
        let hits: Vec<_> = self
            .list_files()
            .into_iter()
            .rev()
            .filter(|(_, r, _)| r.starts_with(rev_prefix))
            .collect();
        // An exact revision, or a prefix that picks one copy: never "the newest that starts so".
        let (_, _, p) = match hits.as_slice() {
            [one] => one,
            many => many.iter().find(|(_, r, _)| r == rev_prefix)?,
        };
        std::fs::read_to_string(p).ok()
    }

    /// Put a saved copy back (what's there now is saved first). Returns the new text.
    pub fn restore(&self, rev_prefix: &str) -> Result<String, String> {
        let text = self.snapshot_text(rev_prefix).ok_or_else(|| {
            format!(
                "no saved copy {rev_prefix:?}, or more than one starts so (`ptyro scratchpad log`)"
            )
        })?;
        self.write(&text).map_err(|e| e.to_string())?;
        Ok(text)
    }

    /// Write `text`, which was edited from `base_rev`'s text, over a file that may have moved
    /// on: unchanged → written as is; changed → three-way merged with `base` (the text of
    /// `base_rev`, when the caller still has it), else both texts are kept between markers.
    pub fn put(&self, base_rev: &str, base: Option<&str>, text: &str) -> std::io::Result<Merged> {
        let cur = self.read()?;
        if rev(&cur) == base_rev || cur == text {
            if cur != text {
                self.write(text)?;
            }
            return Ok(Merged { text: text.to_owned(), conflicts: 0 });
        }
        let merged = match base {
            Some(b) => merge3(b, &cur, text),
            None => both_kept(&cur, text),
        };
        self.write(&merged.text)?;
        Ok(merged)
    }

    /// Apply `f` to the text and write the result when it changed.
    pub fn modify(&self, f: impl FnOnce(&str) -> Result<String, String>) -> Result<String, String> {
        let cur = self.read().map_err(|e| e.to_string())?;
        let new = f(&cur)?;
        if new != cur {
            self.write(&new).map_err(|e| e.to_string())?;
        }
        Ok(new)
    }
}

/// The most text a scratchpad write (`ScratchPut`) may carry; the daemon refuses more.
pub const MAX_PAD_BYTES: usize = 4 << 20;

const MARK_OURS: &str = "<<<<<<< on disk";
const MARK_MID: &str = "=======";
const MARK_THEIRS: &str = ">>>>>>> your edit";

fn both_kept(ours: &str, theirs: &str) -> Merged {
    let nl = |s: &str| if s.ends_with('\n') || s.is_empty() { String::new() } else { "\n".into() };
    Merged {
        text: format!(
            "{MARK_OURS}\n{ours}{}{MARK_MID}\n{theirs}{}{MARK_THEIRS}\n",
            nl(ours),
            nl(theirs)
        ),
        conflicts: 1,
    }
}

/// A change to the base: lines `start..end` become `with`.
#[derive(Debug, Clone)]
struct Change {
    start: usize,
    end: usize,
    with: Vec<String>,
}

fn changes(base: &[String], new: &[String]) -> Vec<Change> {
    use similar::DiffOp;
    let mut out = Vec::new();
    // A pad of unrelated lines can make a diff slow: after a couple of seconds the answer is
    // coarser (still correct), not later.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    for op in
        similar::capture_diff_slices_deadline(similar::Algorithm::Myers, base, new, Some(deadline))
    {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                out.push(Change { start: old_index, end: old_index + old_len, with: vec![] });
            }
            DiffOp::Insert { old_index, new_index, new_len } => out.push(Change {
                start: old_index,
                end: old_index,
                with: new[new_index..new_index + new_len].to_vec(),
            }),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => out.push(Change {
                start: old_index,
                end: old_index + old_len,
                with: new[new_index..new_index + new_len].to_vec(),
            }),
        }
    }
    out
}

fn lines_of(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(str::to_owned).collect()
}

/// `base[s..e]` with the `changes` that fall in it applied.
fn apply(base: &[String], s: usize, e: usize, changes: &[&Change]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = s;
    for c in changes {
        out.extend_from_slice(&base[pos..c.start]);
        out.extend(c.with.iter().cloned());
        pos = c.end;
    }
    out.extend_from_slice(&base[pos..e]);
    out
}

/// Three-way merge by lines: `ours` (what's on disk) and `theirs` (the edit being put) both
/// descend from `base`. Edits to different lines combine; two appends at the same place both
/// stay (ours first); the same lines changed differently are both kept between markers.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Merged {
    let (b, o, t) = (lines_of(base), lines_of(ours), lines_of(theirs));
    let (co, ct) = (changes(&b, &o), changes(&b, &t));
    let (mut io, mut it) = (0, 0);
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0;
    let mut conflicts = 0;
    while io < co.len() || it < ct.len() {
        let first = match (co.get(io), ct.get(it)) {
            (Some(a), Some(c)) => a.start.min(c.start),
            (Some(a), None) => a.start,
            (None, Some(c)) => c.start,
            (None, None) => break,
        };
        let (s, mut e) = (first, first);
        let (mut mo, mut mt): (Vec<&Change>, Vec<&Change>) = (vec![], vec![]);
        let mut grew = true;
        while grew {
            grew = false;
            if let Some(c) = co.get(io + mo.len())
                && (c.start < e || c.start == s)
            {
                e = e.max(c.end);
                mo.push(c);
                grew = true;
            }
            if let Some(c) = ct.get(it + mt.len())
                && (c.start < e || c.start == s)
            {
                e = e.max(c.end);
                mt.push(c);
                grew = true;
            }
        }
        io += mo.len();
        it += mt.len();
        out.extend_from_slice(&b[pos..s]);
        let (ro, rt) = (apply(&b, s, e, &mo), apply(&b, s, e, &mt));
        let inserts = |m: &[&Change]| m.iter().all(|c| c.start == c.end);
        if mt.is_empty() || ro == rt {
            out.extend(ro);
        } else if mo.is_empty() {
            out.extend(rt);
        } else if inserts(&mo) && inserts(&mt) && s == e {
            out.extend(ro);
            out.extend(rt);
        } else {
            conflicts += 1;
            let nl = |v: &Vec<String>| v.last().is_some_and(|l| !l.ends_with('\n'));
            out.push(format!("{MARK_OURS}\n"));
            let fix = nl(&ro);
            out.extend(ro);
            if fix {
                out.push("\n".into());
            }
            out.push(format!("{MARK_MID}\n"));
            let fix = nl(&rt);
            out.extend(rt);
            if fix {
                out.push("\n".into());
            }
            out.push(format!("{MARK_THEIRS}\n"));
        }
        pos = e;
    }
    out.extend_from_slice(&b[pos..]);
    Merged { text: out.concat(), conflicts }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes).then_some(())?;
    let rest = line[hashes..].strip_prefix(' ')?;
    Some((hashes, rest.trim()))
}

/// `entry` at the end of `section` (a heading's text, any level, any case), after its last
/// non-blank line; the section is made at the end of the file when there is none.
pub fn insert_under(text: &str, section: &str, entry: &str) -> String {
    // A heading is one line, whatever the caller sent.
    let section = &section.replace(['\n', '\r'], " ");
    let lines = lines_of(text);
    let at = lines.iter().position(|l| {
        heading(l.trim_end()).is_some_and(|(_, h)| h.eq_ignore_ascii_case(section.trim()))
    });
    let Some(h) = at else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        return format!("{out}## {}\n\n{entry}", section.trim());
    };
    let level = heading(lines[h].trim_end()).map_or(2, |(l, _)| l);
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(h + 1) {
        if heading(l.trim_end()).is_some_and(|(lv, _)| lv <= level) {
            end = i;
            break;
        }
    }
    // After the last line with words in it.
    let mut ins = end;
    while ins > h + 1 && lines[ins - 1].trim().is_empty() {
        ins -= 1;
    }
    let mut out = lines[..ins].concat();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // A fresh section starts after a blank line.
    if ins == h + 1 {
        out.push('\n');
    }
    out.push_str(entry);
    if ins < lines.len() && lines[ins..].iter().any(|l| !l.trim().is_empty()) {
        if lines[ins].trim().is_empty() {
            out.push_str(&lines[ins..].concat());
        } else {
            out.push('\n');
            out.push_str(&lines[ins..].concat());
        }
    } else {
        out.push_str(&lines[ins..].concat());
    }
    out
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && b[13] == b':'
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15].iter().all(|i| b[*i].is_ascii_digit())
}

/// A line's checkbox: `(done, the rest)`.
fn checkbox(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start();
    let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
    let (done, rest) = match t.get(..3)? {
        "[ ]" => (false, &t[3..]),
        "[x]" | "[X]" => (true, &t[3..]),
        _ => return None,
    };
    Some((done, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// Split a stamp off the front of an item: `(at, whence, by, text)`.
fn unstamp(rest: &str) -> (Option<String>, Option<String>, Option<String>, String) {
    let mut parts = rest.split(" · ");
    let Some(first) = parts.next() else { return (None, None, None, rest.to_owned()) };
    if !is_date(first) {
        return (None, None, None, rest.to_owned());
    }
    let (mut whence, mut by) = (None, None);
    let mut remaining: Vec<&str> = parts.collect();
    while let Some(p) = remaining.first() {
        if let Some(w) = p.strip_prefix("by `").and_then(|x| x.strip_suffix('`')) {
            by = Some(w.to_owned());
        } else if let Some(w) = p.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
            whence = Some(w.to_owned());
        } else {
            break;
        }
        remaining.remove(0);
    }
    (Some(first.to_owned()), whence, by, remaining.join(" · "))
}

/// Split `[wt:name]` and `(#42)` tags off the end of an item's words.
fn untag(text: &str) -> (String, Option<String>, Option<u64>) {
    let (mut t, mut wt, mut pr) = (text.trim_end().to_owned(), None, None);
    loop {
        if let Some(i) = t.rfind(" [wt:")
            && t.ends_with(']')
        {
            wt = Some(t[i + 5..t.len() - 1].to_owned());
            t.truncate(i);
        } else if let Some(i) = t.rfind(" (#")
            && t.ends_with(')')
            && let Ok(n) = t[i + 3..t.len() - 1].parse::<u64>()
        {
            pr = Some(n);
            t.truncate(i);
        } else {
            return (t, wt, pr);
        }
    }
}

/// Every checkbox item in the text.
pub fn items(text: &str) -> Vec<Item> {
    let mut section = None;
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if let Some((_, h)) = heading(l) {
            // The pad's own title (`# Scratchpad — ptyro (this project)`) isn't a section.
            if i > 0 || !h.starts_with("Scratchpad") {
                section = Some(h.to_owned());
            }
            continue;
        }
        let Some((done, rest)) = checkbox(l) else { continue };
        let (at, whence, by, body) = unstamp(rest);
        let (text, worktree, pr) = untag(&body);
        out.push(Item {
            line: u32::try_from(i + 1).unwrap_or(u32::MAX),
            done,
            text,
            section: section.clone(),
            at,
            whence,
            by,
            worktree,
            pr,
        });
    }
    out
}

/// Which item a selector means: a line number, or words that appear in exactly one open item.
pub fn select(text: &str, sel: &str) -> Result<Item, String> {
    let all = items(text);
    let sel = sel.trim();
    if let Ok(n) = sel.trim_start_matches(['L', 'l', '#']).parse::<u32>() {
        return all
            .into_iter()
            .find(|i| i.line == n)
            .ok_or_else(|| format!("line {n} is not a checkbox item (`ptyro scratchpad todo`)"));
    }
    let want = sel.to_lowercase();
    let hits: Vec<&Item> =
        all.iter().filter(|i| !i.done && i.text.to_lowercase().contains(&want)).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no open item says {sel:?} (`ptyro scratchpad todo`)")),
        many => Err(format!(
            "{} open items say {sel:?}: {}",
            many.len(),
            many.iter().map(|i| format!("line {}", i.line)).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Tick the item on `line` (`- [x]`), adding `(#pr)` when given.
pub fn tick(text: &str, line: u32, pr: Option<u64>) -> Result<String, String> {
    edit_line(text, line, |l| {
        let (done, _) = checkbox(l).ok_or("not a checkbox item")?;
        if done {
            return Ok(l.to_owned());
        }
        let at = l.find("[ ]").ok_or("not a checkbox item")?;
        let mut s = format!("{}[x]{}", &l[..at], &l[at + 3..]);
        if let Some(n) = pr {
            s.push_str(&format!(" (#{n})"));
        }
        Ok(s)
    })
}

/// Untick it again.
pub fn untick(text: &str, line: u32) -> Result<String, String> {
    edit_line(text, line, |l| {
        // The checkbox is the first `[` of a checkbox line; a `[x]` in the text isn't it.
        let (done, _) = checkbox(l).ok_or("not a ticked item")?;
        let at = l.find('[').filter(|_| done).ok_or("not a ticked item")?;
        Ok(format!("{}[ ]{}", &l[..at], &l[at + 3..]))
    })
}

/// Mark the item as being worked in worktree `name` (`[wt:name]` on its first line).
pub fn tag_worktree(text: &str, line: u32, name: &str) -> Result<String, String> {
    edit_line(text, line, |l| {
        if l.contains(" [wt:") {
            return Ok(l.to_owned());
        }
        Ok(format!("{} [wt:{name}]", l.trim_end()))
    })
}

fn edit_line(
    text: &str,
    line: u32,
    f: impl FnOnce(&str) -> Result<String, &'static str>,
) -> Result<String, String> {
    let mut lines = lines_of(text);
    let i = (line as usize).checked_sub(1).filter(|i| *i < lines.len()).ok_or("no such line")?;
    let nl = lines[i].ends_with('\n');
    let new = f(lines[i].trim_end_matches('\n'))?;
    lines[i] = if nl { format!("{new}\n") } else { new };
    Ok(lines.concat())
}

/// The words of an item and the lines indented under it: what a task is told to do.
pub fn body(text: &str, item: &Item) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = item.text.clone();
    for l in lines.iter().skip(item.line as usize) {
        if l.starts_with("  ") && !l.trim().is_empty() && checkbox(l).is_none() {
            out.push('\n');
            out.push_str(l.trim());
        } else {
            break;
        }
    }
    out
}

/// The terminal editor the pad opens in a pane, or the editor to run in the foreground: the
/// words of a command line (`$EDITOR="code -w"`), else the first of these found on `PATH`.
pub fn fallback_editor() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    ["nvim", "vim", "nano", "vi"]
        .into_iter()
        .find(|e| std::env::split_paths(&path).any(|d| is_exec(&d.join(e))))
        .map(str::to_owned)
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The words to start an editor with, for editors that run in a terminal: `-c 'set autoread'`
/// for vim and nvim, so an append from elsewhere shows without a prompt.
pub fn editor_argv(command: &[String], path: &Path) -> Vec<String> {
    let mut argv: Vec<String> =
        command.iter().map(|a| a.replace("{path}", &path.to_string_lossy())).collect();
    let prog =
        argv.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()).unwrap_or_default();
    let has_path = command.iter().any(|a| a.contains("{path}"));
    if matches!(prog.as_str(), "nvim" | "vim" | "vi") {
        argv.insert(1, "set autoread".into());
        argv.insert(1, "-c".into());
        // Re-read as soon as the window gets focus or the cursor rests.
        argv.insert(1, "autocmd FocusGained,BufEnter,CursorHold * silent! checktime".into());
        argv.insert(1, "-c".into());
    }
    if !has_path {
        argv.push(path.to_string_lossy().into_owned());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad() -> (tempfile::TempDir, Pad) {
        let d = tempfile::tempdir().unwrap();
        let p = Pad::new(d.path().join("scratchpad.md"), d.path().join("hist"), 30);
        (d, p)
    }

    #[test]
    fn a_new_pad_has_its_header_and_survives_reading() {
        let (_d, p) = pad();
        let t = p.read().unwrap();
        assert!(t.starts_with("# Scratchpad"));
        assert_eq!(p.read().unwrap(), t, "reading doesn't change it");
    }

    #[test]
    fn entries_are_stamped_and_read_back() {
        let e = Entry {
            text: "login times out on safari\nrepro: wait 30s".into(),
            at: Some("2026-10-24 14:02".into()),
            whence: Some("ptyro/bugs".into()),
            by: Some("claude-rook".into()),
            ..Entry::default()
        };
        let line = e.render();
        assert_eq!(
            line,
            "- [ ] 2026-10-24 14:02 · `ptyro/bugs` · by `claude-rook` · login times out on safari\n  repro: wait 30s\n"
        );
        let it = &items(&line)[0];
        assert_eq!(it.text, "login times out on safari");
        assert_eq!(it.at.as_deref(), Some("2026-10-24 14:02"));
        assert_eq!(it.whence.as_deref(), Some("ptyro/bugs"));
        assert_eq!(it.by.as_deref(), Some("claude-rook"));
        assert_eq!(body(&line, it), "login times out on safari\nrepro: wait 30s");
        // Unstamped, and words with a dot in them are only words.
        let plain = items("- [x] fix a · b thing\n");
        assert!(plain[0].done);
        assert_eq!(plain[0].text, "fix a · b thing");
    }

    #[test]
    fn items_know_their_section_and_their_tags() {
        let t = "# Pad\n## Bugs\n- [ ] one [wt:fix-one]\n- [x] two (#42)\n## Ideas\n* [ ] three\n- not an item\n";
        let v = items(t);
        assert_eq!(v.len(), 3);
        assert_eq!(
            (v[0].section.as_deref(), v[0].worktree.as_deref()),
            (Some("Bugs"), Some("fix-one"))
        );
        assert_eq!((v[1].done, v[1].pr, v[1].line), (true, Some(42), 4));
        assert_eq!(v[2].section.as_deref(), Some("Ideas"));
    }

    #[test]
    fn selecting_by_line_or_by_words() {
        let t = "- [ ] fix login\n- [ ] fix logout\n- [x] fix cache\n";
        assert_eq!(select(t, "2").unwrap().text, "fix logout");
        assert_eq!(select(t, "logout").unwrap().line, 2);
        assert!(select(t, "fix").unwrap_err().contains("2 open items"));
        assert!(select(t, "cache").is_err(), "done items aren't matched by words");
        assert!(select(t, "nothing").is_err());
    }

    #[test]
    fn ticking_tagging_and_unticking_touch_one_line() {
        let t = "a\n- [ ] one\n- [ ] two\n";
        let t2 = tick(t, 2, Some(7)).unwrap();
        assert_eq!(t2, "a\n- [x] one (#7)\n- [ ] two\n");
        assert_eq!(untick(&t2, 2).unwrap(), "a\n- [ ] one (#7)\n- [ ] two\n");
        assert_eq!(tag_worktree(t, 3, "w1").unwrap(), "a\n- [ ] one\n- [ ] two [wt:w1]\n");
        assert!(tick(t, 1, None).is_err());
        assert!(tick(t, 9, None).is_err());
    }

    #[test]
    fn adding_under_a_heading() {
        let t = "# Pad\n\n## Bugs\n\n- [ ] a\n\n## Ideas\n\n- [ ] b\n";
        let out = insert_under(t, "bugs", "- [ ] new\n");
        assert_eq!(out, "# Pad\n\n## Bugs\n\n- [ ] a\n- [ ] new\n\n## Ideas\n\n- [ ] b\n");
        let out = insert_under(t, "Later", "- [ ] z\n");
        assert!(out.ends_with("- [ ] b\n\n## Later\n\n- [ ] z\n"), "{out}");
        let empty = insert_under("## Bugs\n\n## Ideas\n", "Bugs", "- [ ] x\n");
        assert_eq!(empty, "## Bugs\n\n- [ ] x\n\n## Ideas\n");
    }

    #[test]
    fn appends_keep_what_is_there_and_are_snapshotted() {
        let (_d, p) = pad();
        p.append_raw("- [ ] a\n").unwrap();
        p.add(&Entry { text: "b".into(), ..Entry::default() }).unwrap();
        let t = p.read().unwrap();
        assert!(t.ends_with("- [ ] a\n- [ ] b\n"), "{t}");
        assert!(!p.history().is_empty());
    }

    #[test]
    fn history_keeps_every_state_you_left_and_restores_one() {
        let (_d, p) = pad();
        p.write("one\n").unwrap();
        p.write("two\n").unwrap();
        p.write("three\n").unwrap();
        let revs: Vec<String> = p.history().into_iter().map(|s| s.rev).collect();
        for t in ["one\n", "two\n"] {
            assert!(revs.contains(&rev(t)), "{t:?} is kept: {revs:?}");
        }
        assert!(p.restore(&rev("one\n")[..6]).is_ok(), "a prefix is enough");
        assert_eq!(p.read().unwrap(), "one\n");
        assert!(p.snapshot_text(&rev("three\n")).is_some(), "restoring saved what was there");
        assert!(p.restore("zzzz").is_err());
        assert!(p.restore("").is_err());
    }

    #[test]
    fn a_change_seen_from_outside_is_not_saved_again_right_away() {
        let (_d, p) = pad();
        p.snapshot("a\n").unwrap();
        p.snapshot_lazy("b\n").unwrap();
        p.snapshot_lazy("c\n").unwrap();
        assert_eq!(p.history().len(), 1, "the editor's keystrokes aren't versions");
        p.snapshot("d\n").unwrap();
        assert_eq!(p.history().len(), 2);
    }

    #[test]
    fn old_copies_thin_out_but_the_newest_stay() {
        let (d, p) = pad();
        std::fs::create_dir_all(&p.history).unwrap();
        let now = crate::now_ms();
        // 60 copies, one a minute, a day and a half ago: they thin to one an hour.
        for i in 0..60u64 {
            let at = now - 36 * HOUR_MS + i * 60_000;
            std::fs::write(p.history.join(format!("{at}-{i:012x}.md")), format!("v{i}\n")).unwrap();
        }
        p.snapshot("fresh\n").unwrap();
        let n = p.history().len();
        assert!((2..=KEEP_NEWEST + 2).contains(&n), "{n}");
        drop(d);
    }

    #[test]
    fn a_stale_put_merges_edits_to_different_lines() {
        let base = "a\nb\nc\nd\n";
        let m = merge3(base, "a\nB\nc\nd\n", "a\nb\nc\nD\n");
        assert_eq!(m.text, "a\nB\nc\nD\n");
        assert_eq!(m.conflicts, 0);
    }

    #[test]
    fn two_appends_both_stay() {
        let m = merge3("a\n", "a\nfrom disk\n", "a\nfrom web\n");
        assert_eq!(m.text, "a\nfrom disk\nfrom web\n");
        assert_eq!(m.conflicts, 0);
    }

    #[test]
    fn the_same_line_changed_two_ways_is_kept_both_ways() {
        let m = merge3("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n");
        assert_eq!(m.conflicts, 1);
        assert_eq!(m.text, "a\n<<<<<<< on disk\nX\n=======\nY\n>>>>>>> your edit\nc\n");
    }

    #[test]
    fn the_same_change_twice_is_one_change() {
        let m = merge3("a\nb\n", "a\nB\n", "a\nB\n");
        assert_eq!((m.text.as_str(), m.conflicts), ("a\nB\n", 0));
        // A deletion and an edit elsewhere.
        let m = merge3("a\nb\nc\n", "b\nc\n", "a\nb\nC\n");
        assert_eq!((m.text.as_str(), m.conflicts), ("b\nC\n", 0));
    }

    #[test]
    fn put_merges_over_a_file_that_moved_on() {
        let (_d, p) = pad();
        p.write("a\nb\n").unwrap();
        let base = p.read().unwrap();
        p.write("a\nb\nfrom cli\n").unwrap();
        let m = p.put(&rev(&base), Some(&base), "A\nb\n").unwrap();
        assert_eq!((m.text.as_str(), m.conflicts), ("A\nb\nfrom cli\n", 0));
        assert_eq!(p.read().unwrap(), "A\nb\nfrom cli\n");
        // No base to merge with: both are kept.
        let m = p.put("deadbeef0000", None, "other\n").unwrap();
        assert_eq!(m.conflicts, 1);
        assert!(m.text.contains("A\nb\nfrom cli\n") && m.text.contains("other\n"));
    }

    #[test]
    fn put_with_the_current_revision_just_writes() {
        let (_d, p) = pad();
        let cur = p.read().unwrap();
        let m = p.put(&rev(&cur), None, "new text\n").unwrap();
        assert_eq!((m.text.as_str(), m.conflicts), ("new text\n", 0));
    }

    #[test]
    fn a_symlinked_pad_is_written_through() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("synced.md");
        std::fs::write(&real, "x\n").unwrap();
        let link = d.path().join("scratchpad.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let p = Pad::new(link.clone(), d.path().join("h"), 30);
        p.write("y\n").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "y\n");
    }

    #[test]
    fn terminal_vim_re_reads_changes() {
        let a = editor_argv(&["nvim".into()], Path::new("/p.md"));
        assert_eq!(a[0], "nvim");
        assert!(a.contains(&"set autoread".to_owned()));
        assert_eq!(a.last().unwrap(), "/p.md");
        let b = editor_argv(&["hx".into(), "{path}".into()], Path::new("/p.md"));
        assert_eq!(b, ["hx", "/p.md"]);
    }
}

#[cfg(test)]
mod hardening {
    use super::*;

    #[test]
    fn a_stamp_and_a_section_stay_on_their_own_line() {
        let e = Entry {
            text: "do it".into(),
            at: Some("2026-10-24 10:00".into()),
            whence: Some("a`b · c\nd".into()),
            by: Some("me".into()),
            ..Entry::default()
        };
        let line = e.render();
        assert_eq!(line.matches('\n').count(), 1, "{line:?}");
        assert!(line.contains("`a'b - c d` · by `me` · do it"), "{line:?}");
        let out = insert_under("# Pad\n", "Bugs\n- [ ] injected", "- [ ] x\n");
        assert!(out.contains("## Bugs - [ ] injected\n"), "{out:?}");
        assert!(!out.contains("\n- [ ] injected"), "{out:?}");
    }

    fn pad(d: &std::path::Path) -> Pad {
        Pad::new(d.join("scratchpad.md"), d.join("hist"), 30)
    }

    #[test]
    fn unticking_changes_the_checkbox_not_a_look_alike_in_the_text() {
        let t = "- [X] see [x] docs\n";
        assert_eq!(untick(t, 1).unwrap(), "- [ ] see [x] docs\n");
        assert!(untick("- [ ] open\n", 1).is_err());
        assert!(untick("plain [x] line\n", 1).is_err());
    }

    #[test]
    fn a_pad_that_is_not_utf8_is_never_rewritten_lossily() {
        let d = tempfile::tempdir().unwrap();
        let p = pad(d.path());
        p.ensure().unwrap();
        let bytes = b"# Pad\n- [ ] caf\xe9\n".to_vec();
        std::fs::write(&p.path, &bytes).unwrap();
        assert!(p.write("# new\n").is_err());
        assert_eq!(std::fs::read(&p.path).unwrap(), bytes, "untouched");
        // Adding a line appends bytes and keeps the rest exactly.
        p.append_raw("- [ ] more\n").unwrap();
        assert!(std::fs::read(&p.path).unwrap().starts_with(&bytes));
        assert!(p.list_files().is_empty(), "no lossy snapshot was saved");
    }

    #[test]
    fn writing_keeps_a_private_pads_permissions_and_leaves_no_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = pad(d.path());
        p.ensure().unwrap();
        std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        p.write("# one\n").unwrap();
        p.write("# two\n").unwrap();
        assert_eq!(std::fs::metadata(&p.path).unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn a_saved_copy_is_found_by_its_revision_or_a_prefix_that_picks_one() {
        let d = tempfile::tempdir().unwrap();
        let p = pad(d.path());
        std::fs::create_dir_all(&p.history).unwrap();
        for (at, rev) in [(1_000u64, "aaaa1111"), (2_000, "aaaa2222"), (3_000, "bbbb3333")] {
            std::fs::write(p.history.join(format!("{at}-{rev}.md")), rev).unwrap();
        }
        assert_eq!(p.snapshot_text("bbbb").as_deref(), Some("bbbb3333"));
        assert_eq!(p.snapshot_text("aaaa2").as_deref(), Some("aaaa2222"));
        assert_eq!(p.snapshot_text("aaaa"), None, "two copies start so: not the newest of them");
        assert_eq!(p.snapshot_text("aaaa1111").as_deref(), Some("aaaa1111"));
        assert_eq!(p.snapshot_text("zz"), None);
        assert!(p.restore("aaaa").unwrap_err().contains("more than one"));
    }

    #[test]
    fn a_huge_keep_does_not_overflow_the_cutoff() {
        let d = tempfile::tempdir().unwrap();
        let p = Pad::new(d.path().join("p.md"), d.path().join("h"), u64::MAX);
        p.write("# a\n").unwrap();
        p.write("# b\n").unwrap();
        assert_eq!(p.list_files().len(), 2, "both copies kept: nothing pruned as 'too old'");
    }
}

#[cfg(test)]
mod project_pads {
    use super::*;

    #[test]
    fn a_project_pad_lives_in_the_main_checkout_with_its_history_beside_it() {
        let root = Path::new("/src/ptyro");
        assert_eq!(project_pad_path(root), Path::new("/src/ptyro/.ptyro/scratchpad.md"));
        assert_eq!(project_history_dir(root), Path::new("/src/ptyro/.ptyro/scratchpad.history"));
        // What git is told to ignore is the pad, its copies and the temp files of a write,
        // not the rest of `.ptyro/` (recipes and change sets are committed there).
        assert_eq!(EXCLUDE_LINE, "/.ptyro/scratchpad.*");
    }

    #[test]
    fn a_new_project_pad_says_which_pad_it_is_and_that_git_ignores_it() {
        let d = tempfile::tempdir().unwrap();
        let p = Pad::project(d.path(), "ptyro", 30);
        let text = p.read().unwrap();
        assert!(text.starts_with("# Scratchpad — ptyro (this project)"), "{text}");
        assert!(text.contains("git ignores it") && text.contains("$PTYRO_PROJECT_SCRATCHPAD"));
        assert!(d.path().join(".ptyro/scratchpad.md").exists(), "the directory is made");
        // History is beside it, with the same keep rules.
        p.write("# changed\n").unwrap();
        assert_eq!(p.history().len(), 1);
        assert!(d.path().join(".ptyro/scratchpad.history").is_dir());
        // The global pad's header names it too, and is told apart.
        assert!(HEADER.starts_with("# Scratchpad — everywhere"));
        assert!(items(&text).is_empty(), "a header holds no items");
    }

    #[test]
    fn an_existing_dot_ptyro_directory_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ptyro/recipes")).unwrap();
        std::fs::write(d.path().join(".ptyro/recipes/r.toml"), "x").unwrap();
        Pad::project(d.path(), "p", 30).ensure().unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join(".ptyro/recipes/r.toml")).unwrap(), "x");
    }

    #[test]
    fn the_selector_is_global_unless_it_says_otherwise() {
        assert_eq!(PadSel::default(), PadSel::Global);
        for (sel, json) in [
            (PadSel::Global, r#""global""#),
            (PadSel::Project("p1".into()), r#"{"project":"p1"}"#),
            (PadSel::Near("/a/b".into()), r#"{"near":"/a/b"}"#),
        ] {
            assert_eq!(serde_json::to_string(&sel).unwrap(), json);
            assert_eq!(serde_json::from_str::<PadSel>(json).unwrap(), sel);
        }
    }
}
