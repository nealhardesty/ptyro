//! KB0.4 (SPEC §25.6.3): the comment table, tried on real code. Give it folders; it reads every
//! source file under them (nothing is written), runs `ptyro_core::kb::comments::judge` and
//! prints what was kept and what each rule dropped, with a sample of each to judge by eye. Run
//! with `make spike-kb-comments ARGS="<folder>…"` (`SAMPLE=20` for more of each).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ptyro_core::kb::comments::{self, Block, Verdict};

/// §25.6.3: files over `kb.max_file` are not read, and a NUL in the first 8 KiB means binary.
const MAX_FILE: u64 = 1 << 20;
const SKIP: [&str; 6] = ["node_modules", "target", "dist", "build", "vendor", ".venv"];

#[derive(Default)]
struct Tally {
    by_ext: BTreeMap<String, (usize, usize)>,
    unknown: BTreeMap<String, usize>,
    blocks: BTreeMap<&'static str, usize>,
    chars: BTreeMap<&'static str, usize>,
    samples: BTreeMap<&'static str, Vec<(PathBuf, Block)>>,
    lines: usize,
    comment_lines: usize,
    trailing: usize,
    bytes: usize,
    seen: u64,
}

fn main() {
    let roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        eprintln!("usage: kb_comments <folder>…");
        std::process::exit(2);
    }
    let sample = std::env::var("SAMPLE").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    for root in roots {
        let mut tally = Tally::default();
        let t = Instant::now();
        walk(&root, &mut tally, sample);
        report(&root, &tally, t.elapsed().as_secs_f64());
    }
}

fn walk(dir: &Path, tally: &mut Tally, sample: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                walk(&e.path(), tally, sample);
            }
        } else if kind.is_file() && e.metadata().is_ok_and(|m| m.len() <= MAX_FILE) {
            file(&e.path(), &name, tally, sample);
        }
    }
}

fn file(path: &Path, name: &str, tally: &mut Tally, sample: usize) {
    let Ok(bytes) = std::fs::read(path) else { return };
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => name.to_ascii_lowercase(),
    };
    let Some(marks) = comments::marks(name, &text) else {
        *tally.unknown.entry(ext).or_default() += 1;
        return;
    };
    let judged = comments::judge(name, &text);
    tally.bytes += bytes.len();
    let entry = tally.by_ext.entry(ext).or_default();
    entry.0 += 1;
    entry.1 += judged.iter().filter(|(_, v)| *v == Verdict::Keep).count();

    // What whole-line-only leaves out: a comment after code on its line. Counted roughly (a
    // mark with a space each side, after something else), outside the blocks that were read.
    let mut in_block = vec![false; text.lines().count() + 2];
    for (b, _) in &judged {
        in_block[b.start..=b.end].fill(true);
        tally.comment_lines += b.end - b.start + 1;
    }
    for (i, line) in text.lines().enumerate() {
        tally.lines += 1;
        // (A line that long is not written by hand.)
        if in_block[i + 1] || line.len() > 1000 {
            continue;
        }
        let trails = marks.line.iter().any(|m| {
            line.match_indices(m).any(|(at, _)| {
                let (before, after) = (&line[..at], &line[at + m.len()..]);
                !before.trim().is_empty()
                    && before.ends_with([' ', '\t'])
                    && (after.is_empty() || after.starts_with([' ', '/', '!']))
            })
        });
        tally.trailing += usize::from(trails);
    }

    for (block, verdict) in judged {
        let key = match verdict {
            Verdict::Keep => "kept",
            Verdict::Directive => "dropped: a tool's directive",
            Verdict::Licence => "dropped: a licence header",
            Verdict::Code => "dropped: code switched off",
            Verdict::Short => "dropped: too short",
        };
        *tally.blocks.entry(key).or_default() += 1;
        *tally.chars.entry(key).or_default() += block.text.len();
        // A fair sample without holding every block: each new one replaces a kept one with
        // the odds that keep all equally likely.
        let n = tally.blocks[key];
        let kept = tally.samples.entry(key).or_default();
        tally.seen = tally.seen.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(n as u64 | 1);
        let at = (tally.seen >> 33) as usize % n;
        if kept.len() < sample {
            kept.push((path.to_path_buf(), block));
        } else if at < sample {
            kept[at] = (path.to_path_buf(), block);
        }
    }
}

fn report(root: &Path, t: &Tally, secs: f64) {
    let files: usize = t.by_ext.values().map(|v| v.0).sum();
    println!("\n══ {} ══", root.display());
    println!(
        "{files} source files, {} lines, {:.1} MB, read in {:.2} s ({:.0} MB/s)",
        t.lines,
        t.bytes as f64 / 1e6,
        secs,
        t.bytes as f64 / 1e6 / secs.max(1e-9)
    );
    let mut by: Vec<_> = t.by_ext.iter().collect();
    by.sort_by_key(|(_, v)| std::cmp::Reverse(v.0));
    let exts: Vec<String> =
        by.iter().take(14).map(|(e, (n, kept))| format!(".{e} {n} ({kept} kept)")).collect();
    println!("by extension: {}", exts.join(", "));
    let mut unknown: Vec<_> = t.unknown.iter().collect();
    unknown.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let exts: Vec<String> = unknown.iter().take(14).map(|(e, n)| format!(".{e} {n}")).collect();
    println!("not in the table: {}", exts.join(", "));
    let all: usize = t.blocks.values().sum();
    println!(
        "{all} blocks on {} whole comment lines ({:.0}% of the lines); about {} trailing comments were not read ({:.0}% of all comment lines)",
        t.comment_lines,
        100.0 * t.comment_lines as f64 / t.lines.max(1) as f64,
        t.trailing,
        100.0 * t.trailing as f64 / (t.trailing + t.comment_lines).max(1) as f64
    );
    for (key, n) in &t.blocks {
        println!(
            "  {key:<30} {n:>7}  {:>5.1}%  {:>7.1} KB of text",
            100.0 * *n as f64 / all.max(1) as f64,
            t.chars[key] as f64 / 1e3
        );
    }
    for (key, samples) in &t.samples {
        println!("── {key} ──");
        for (path, b) in samples {
            let rel = path.strip_prefix(root).unwrap_or(path);
            println!("  {}:{}", rel.display(), b.anchor());
            for line in b.text.lines().take(4) {
                println!("    │ {}", line.chars().take(110).collect::<String>());
            }
            if b.text.lines().count() > 4 {
                println!("    │ … {} more lines", b.text.lines().count() - 4);
            }
            println!("    └ {}", b.code.chars().take(90).collect::<String>());
        }
    }
}
