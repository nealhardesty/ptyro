//! The findings, from what `run` recorded: judged with ptyro's own sniffer and emulator, so
//! "survives ConPTY" means "ptyro would still see it".

use std::fmt::Write as _;
use std::path::Path;

use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_term::sniff::{Seq, Sniffer};
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};
use serde_json::Value;

use crate::cases::{self, KEYS, QUERIES, RECORD_KEYS, SEQS};

/// Bytes as someone reads them: `\e`, `\r`, `\n`, `\xHH` for the rest of the controls.
pub fn shown(b: &[u8]) -> String {
    let mut s = String::new();
    for ch in String::from_utf8_lossy(b).chars() {
        match ch {
            '\x1b' => s.push_str("\\e"),
            '\r' => s.push_str("\\r"),
            '\n' => s.push_str("\\n"),
            '\x07' => s.push_str("\\a"),
            '|' => s.push_str("\\|"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(s, "\\x{:02x}", c as u32);
            }
            c => s.push(c),
        }
    }
    s
}

fn short(b: &[u8], n: usize) -> String {
    let s = shown(b);
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Every sequence in `bytes`, with the offset just past it.
fn sniff(bytes: &[u8]) -> Vec<(Seq, usize)> {
    let (mut s, mut out, mut ends) = (Sniffer::default(), Vec::new(), Vec::new());
    s.scan_with_ends(bytes, &mut out, Some(&mut ends));
    out.into_iter().zip(ends).collect()
}

fn seq_text(s: &Seq) -> String {
    match s {
        Seq::Osc(b) => format!("OSC {}", shown(b)),
        Seq::Csi { params, final_byte } => format!("CSI {}{}", shown(params), *final_byte as char),
        Seq::Dcs(b) => format!("DCS {}", shown(b)),
        Seq::Ris => "RIS".into(),
    }
}

/// Text without escape sequences (as the screen would show it, roughly).
fn plain(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // Skip to the end of the sequence.
            let mut s = Sniffer::default();
            let mut v = Vec::new();
            let mut j = i;
            while j < bytes.len() {
                s.scan(&bytes[j..=j], &mut v);
                j += 1;
                if !v.is_empty() {
                    break;
                }
                // A two-byte escape (ESC x) that the sniffer doesn't report.
                if j - i == 2 && !matches!(bytes[i + 1], b'[' | b']' | b'P' | b'_' | b'^' | b'X') {
                    break;
                }
            }
            i = j;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn replay(
    chunks: &[&[u8]],
    size: TermSize,
    resizes: &[(usize, TermSize)],
) -> (ScreenCache, String, usize) {
    let mut emu = AlacrittyEmulator::new(size, 1000);
    let mut out = FeedOutput::default();
    let all: Vec<u8> = chunks.concat();
    let mut at = 0;
    for (off, sz) in resizes {
        let off = (*off).min(all.len());
        if off > at {
            emu.feed(&all[at..off], &mut out);
            at = off;
        }
        emu.resize(*sz);
    }
    emu.feed(&all[at..], &mut out);
    if emu.sync_deadline().is_some() {
        emu.expire_sync(&mut out);
    }
    let size = emu.size();
    let mut tracker = FrameTracker::new(&mut emu);
    let mut cache = ScreenCache::new(size);
    cache.apply(true, tracker.seq(), &tracker.build(&emu, None, &mut StyleTable::default()));
    let modes = format!("{:?}", emu.modes());
    let history = emu.scrollback_len();
    (cache, modes, history)
}

/// What conhost writes when its pseudoconsole closes (`?9001l`, `?1004l`, in either order): not
/// the program's, so left out when comparing screens and modes.
fn untorn(mut b: Vec<u8>) -> Vec<u8> {
    for tail in
        [&b"\x1b[?9001l\x1b[?1004l"[..], b"\x1b[?1004l\x1b[?9001l", b"\x1b[?9001l", b"\x1b[?1004l"]
    {
        if b.ends_with(tail) {
            b.truncate(b.len() - tail.len());
        }
    }
    b
}

/// Cells as they look: the character, and its style, except a blank's foreground (invisible).
fn visible(c: &ScreenCache) -> Vec<Vec<(String, String)>> {
    c.rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|cell| {
                    let mut st = c.style(cell.style);
                    if cell.text.trim().is_empty()
                        && st.flags.0 & ptyro_core::screen::StyleFlags::ANY_UNDERLINE == 0
                    {
                        st.fg = Default::default();
                    }
                    (cell.text.clone(), format!("{st:?}"))
                })
                .collect()
        })
        .collect()
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap_or_default()
}

fn variants(out: &Path) -> Vec<String> {
    let order = ["inbox", "inbox-passthrough", "nuget", "nuget-passthrough"];
    order
        .iter()
        .filter(|v| out.join(v).join("meta.json").exists())
        .map(|v| (*v).to_owned())
        .collect()
}

pub fn report(out: &Path, fixtures: &Path) -> Result<String, String> {
    let vs = variants(out);
    if vs.is_empty() {
        return Err(format!("no results in {} (run it on Windows first)", out.display()));
    }
    let metas: Vec<Value> = vs
        .iter()
        .map(|v| serde_json::from_slice(&read(&out.join(v), "meta.json")).unwrap_or(Value::Null))
        .collect();
    let mut md = String::new();
    let _ = writeln!(md, "# ConPTY spike report (WIN0.2, WIN0.3)\n");
    if let Ok(env) = std::fs::read_to_string(out.join("environment.txt")) {
        let _ = writeln!(md, "```\n{}\n```\n", env.trim());
    }
    let _ = writeln!(
        md,
        "Variants: {}.\n",
        vs.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ")
    );
    let _ = writeln!(md, "Cases:\n");
    let mut last = "";
    for c in cases::all(&cases::fixtures(fixtures)) {
        if c.what != last {
            let _ = writeln!(
                md,
                "- `{}`{}: {}",
                c.name,
                if c.name.starts_with("query-") || c.name.starts_with("fixture-") {
                    "…"
                } else {
                    ""
                },
                c.what
            );
            last = c.what;
        }
    }
    md.push('\n');

    // What conhost writes before the child says anything.
    let _ = writeln!(
        md,
        "## What conhost writes first\n\nThe start of the `ctrl-c-raw` case's output (the child writes only `ready`).\n"
    );
    let _ = writeln!(md, "| Variant | First bytes |\n|---|---|");
    for v in &vs {
        let b = read(&out.join(v), "ctrl-c-raw.out");
        let end = find(&b, b"ready", 0).unwrap_or(b.len().min(200));
        let _ = writeln!(md, "| {v} | `{}` |", short(&b[..end], 240));
    }

    // Sequences.
    let _ = writeln!(md, "\n## Sequences a program writes (case `seqs`)\n");
    let _ = writeln!(
        md,
        "*kept*: the same sequence, between its markers. *moved*: the same, elsewhere. *changed*: something else came. *dropped*: nothing.\n"
    );
    let mut head = "| Sequence | ptyro uses it for |".to_owned();
    let mut rule = "|---|---|".to_owned();
    for v in &vs {
        let _ = write!(head, " {v} |");
        rule.push_str("---|");
    }
    let _ = writeln!(md, "{head}\n{rule}");
    let outs: Vec<Vec<u8>> = vs.iter().map(|v| read(&out.join(v), "seqs.out")).collect();
    let sniffed: Vec<Vec<(Seq, usize)>> = outs.iter().map(|o| sniff(o)).collect();
    for it in SEQS {
        let want: Vec<Seq> = sniff(it.bytes).into_iter().map(|(s, _)| s).collect();
        let (open, close) = cases::markers(it.label);
        let mut row = format!("| `{}` | {} |", it.label, it.why);
        for (o, seqs) in outs.iter().zip(&sniffed) {
            let verdict = match find(o, open.as_bytes(), 0) {
                None => "marker missing".to_owned(),
                Some(a) => {
                    let start = a + open.len();
                    let end = find(o, close.as_bytes(), start).unwrap_or(o.len());
                    let inside: Vec<&Seq> = seqs
                        .iter()
                        .filter(|(_, e)| *e > start && *e <= end)
                        .map(|(s, _)| s)
                        .collect();
                    let text_in = plain(&o[start..end]);
                    let text_want = plain(it.bytes);
                    if want.is_empty() {
                        // Text only (wide characters).
                        if text_in == text_want { "kept".into() } else { format!("text `{}`", shown(text_in.as_bytes())) }
                    } else if want.iter().all(|w| inside.contains(&w)) {
                        let extra: Vec<String> = inside.iter().filter(|s| !want.contains(s)).map(|s| seq_text(s)).collect();
                        if extra.is_empty() { "kept".into() } else { format!("kept, plus `{}`", extra.join(" ")) }
                    } else if want.iter().all(|w| seqs.iter().any(|(s, _)| s == w)) {
                        let at = seqs.iter().find(|(s, _)| *s == want[0]).map_or(0, |(_, e)| *e);
                        if at <= a { "moved ahead of its text".into() } else { "moved after its text".into() }
                    } else if let [Seq::Osc(b)] = want.as_slice()
                        && let Some(rest) = b.strip_prefix(b"2;")
                        && seqs.iter().any(|(s, _)| matches!(s, Seq::Osc(o) if o.strip_prefix(b"0;") == Some(rest)))
                    {
                        "became OSC 0, later".into()
                    } else if !inside.is_empty() {
                        format!("changed: `{}`", inside.iter().map(|s| seq_text(s)).collect::<Vec<_>>().join(" "))
                    } else {
                        "dropped".into()
                    }
                }
            };
            let _ = write!(row, " {verdict} |");
        }
        let _ = writeln!(md, "{row}");
    }

    // Queries.
    let _ = writeln!(md, "\n## Queries (cases `query-*`)\n");
    let _ = writeln!(
        md,
        "*passed on*: the query came out of the pseudoconsole, for the terminal (ptyro) to answer. *answered*: conhost answered the child itself (what the child read).\n"
    );
    let _ = writeln!(md, "{head}\n{rule}");
    for q in QUERIES {
        let name = format!("query-{}", q.label);
        let mut row = format!("| `{}` | {} |", q.label, q.why);
        for v in &vs {
            let d = out.join(v);
            let o = read(&d, &format!("{name}.out"));
            let want: Vec<Seq> = sniff(q.bytes).into_iter().map(|(s, _)| s).collect();
            let passed = want.iter().all(|w| sniff(&o).iter().any(|(s, _)| s == w));
            let log = String::from_utf8_lossy(&read(&d, &format!("{name}.child.log"))).into_owned();
            let answers: Vec<u8> = log
                .lines()
                .filter_map(|l| l.split_once(" in ").map(|(_, h)| h))
                .flat_map(|h| h.split(' ').filter_map(|x| u8::from_str_radix(x, 16).ok()))
                .collect();
            let verdict = match (passed, answers.is_empty()) {
                (true, true) => "passed on".to_owned(),
                (true, false) => format!("passed on, and answered `{}`", short(&answers, 60)),
                (false, false) => format!("answered `{}`", short(&answers, 60)),
                (false, true) => "dropped".to_owned(),
            };
            let _ = write!(row, " {verdict} |");
        }
        let _ = writeln!(md, "{row}");
    }

    // Keys.
    let _ = writeln!(md, "\n## Keys typed into the pseudoconsole (case `keys`)\n");
    let _ = writeln!(
        md,
        "What the child read for each, in VT input mode, after turning on bracketed paste, SGR mouse, focus and kitty keys. The host types one every 200 ms.\n"
    );
    let mut khead = "| Typed | |".to_owned();
    for v in &vs {
        let _ = write!(khead, " {v} |");
    }
    let _ = writeln!(md, "{khead}\n{rule}");
    let groups: Vec<Vec<Vec<u8>>> = vs
        .iter()
        .zip(&metas)
        .map(|(v, m)| {
            by_input(&read(&out.join(v), "keys.child.log"), &m["keys"], KEYS.len(), "in ")
                .into_iter()
                .map(|ls| {
                    ls.iter()
                        .flat_map(|l| l.split(' ').filter_map(|x| u8::from_str_radix(x, 16).ok()))
                        .collect()
                })
                .collect()
        })
        .collect();
    for (i, k) in KEYS.iter().enumerate() {
        let mut row = format!("| `{}` | {} |", shown(k.bytes), k.why);
        for g in &groups {
            let got = g.get(i).map_or("—".to_owned(), |b| {
                if b.as_slice() == k.bytes {
                    "same".to_owned()
                } else if b.is_empty() {
                    "nothing".to_owned()
                } else {
                    format!("`{}`", short(b, 50))
                }
            });
            let _ = write!(row, " {got} |");
        }
        let _ = writeln!(md, "{row}");
    }

    // Latency and throughput.
    let _ = writeln!(md, "\n## Echo latency and throughput (cases `latency`, `flood`)\n");
    let _ = writeln!(
        md,
        "*Latency*: from the host writing a letter to that letter coming back out (the child echoes at once), over 20 letters. *Throughput*: 2 MiB of coloured lines from the child, from its start to `FLOOD-END` arriving, and how many bytes conhost wrote for them.\n"
    );
    let _ = writeln!(
        md,
        "| Variant | Echo median | Echo worst | Flood time | Flood rate | Bytes out for 2 MiB in |\n|---|---|---|---|---|---|"
    );
    for (v, m) in vs.iter().zip(&metas) {
        let o = read(&out.join(v), "latency.out");
        let mm = &m["latency"];
        let chunks: Vec<(u64, usize)> = mm["chunks"]
            .as_array()
            .map(|a| {
                a.iter().filter_map(|c| Some((c[0].as_u64()?, c[1].as_u64()? as usize))).collect()
            })
            .unwrap_or_default();
        let typed: Vec<(usize, u64)> = mm["timeline"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["what"] == "input")
                    .filter_map(|e| Some((e["i"].as_u64()? as usize, e["at"].as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let ready = find(&o, b"ready", 0).map_or(0, |p| p + 5);
        let mut lat = Vec::new();
        let mut from = ready;
        for (i, at) in &typed {
            let ch = b'a' + *i as u8;
            let Some(p) = o[from..].iter().position(|b| *b == ch).map(|p| p + from) else {
                continue;
            };
            from = p + 1;
            if let Some((t, _)) = chunks.iter().find(|(_, off)| *off > p) {
                lat.push(t.saturating_sub(*at));
            }
        }
        lat.sort_unstable();
        let med = lat.get(lat.len() / 2).map_or("—".to_owned(), |x| format!("{x} ms"));
        let worst = lat.last().map_or("—".to_owned(), |x| format!("{x} ms"));
        let f = read(&out.join(v), "flood.out");
        let fm = &m["flood"];
        let spawned = fm["timeline"][0]["at"].as_u64().unwrap_or(0);
        let end = find(&f, b"FLOOD-END", 0);
        let fchunks: Vec<(u64, usize)> = fm["chunks"]
            .as_array()
            .map(|a| {
                a.iter().filter_map(|c| Some((c[0].as_u64()?, c[1].as_u64()? as usize))).collect()
            })
            .unwrap_or_default();
        let (ftime, rate) = match end.and_then(|e| fchunks.iter().find(|(_, off)| *off > e)) {
            Some((t, _)) => {
                let ms = t.saturating_sub(spawned).max(1);
                (format!("{ms} ms"), format!("{:.1} MiB/s", 2.0 * 1000.0 / ms as f64))
            }
            None => ("didn't arrive".to_owned(), "—".to_owned()),
        };
        let _ = writeln!(md, "| {v} | {med} | {worst} | {ftime} | {rate} | {} |", f.len());
    }

    // Records.
    let _ = writeln!(md, "\n## Keys read as console records (case `records`)\n");
    let _ = writeln!(
        md,
        "A child with VT input off reads `INPUT_RECORD`s, as PowerShell's PSReadLine and cmd do. What each typed sequence became (key-down records: virtual key, character, modifier state; `0x10` is Shift, `0x8` left Ctrl, `0x2` left Alt).\n"
    );
    let _ = writeln!(md, "{khead}\n{rule}");
    let rgroups: Vec<Vec<String>> = vs
        .iter()
        .zip(&metas)
        .map(|(v, m)| {
            by_input(
                &read(&out.join(v), "records.child.log"),
                &m["records"],
                RECORD_KEYS.len(),
                "key ",
            )
            .into_iter()
            .map(|ls| {
                let downs: Vec<String> = ls
                    .iter()
                    .filter_map(|l| l.strip_prefix("down "))
                    .map(|d| {
                        let f: Vec<&str> = d.split(' ').collect();
                        let get = |k: &str| f.iter().find_map(|x| x.strip_prefix(k)).unwrap_or("?");
                        format!("vk{} ch{} {}", get("vk="), get("ch="), get("state="))
                    })
                    .collect();
                if ls.is_empty() {
                    "nothing".to_owned()
                } else if downs.is_empty() {
                    "only key-ups".to_owned()
                } else {
                    downs.join(", ")
                }
            })
            .collect()
        })
        .collect();
    for (i, k) in RECORD_KEYS.iter().enumerate() {
        let mut row = format!("| `{}` | {} |", shown(k.bytes), k.why);
        for g in &rgroups {
            let _ = write!(row, " {} |", g.get(i).map_or("—".to_owned(), |s| format!("`{s}`")));
        }
        let _ = writeln!(md, "{row}");
    }

    // Ctrl+C and close.
    let _ = writeln!(md, "\n## Ctrl+C and closing\n");
    let _ = writeln!(md, "| Case | {} |\n|---|{}", vs.join(" | "), "---|".repeat(vs.len()));
    for case in ["ctrl-c", "ctrl-c-raw", "close"] {
        let mut row = format!("| `{case}` |");
        for (v, m) in vs.iter().zip(&metas) {
            let log = String::from_utf8_lossy(&read(&out.join(v), &format!("{case}.child.log")))
                .into_owned();
            let events: Vec<&str> = log
                .lines()
                .filter_map(|l| l.split_once(' ').map(|x| x.1))
                .filter(|l| l.starts_with("ctrl ") || l.starts_with("in "))
                .collect();
            let mm = &m[case];
            let _ = write!(
                row,
                " {}; exit {}; close {} ms |",
                if events.is_empty() { "nothing reached it".to_owned() } else { events.join(", ") },
                mm["exit_code"],
                mm["close_ms"]
            );
        }
        let _ = writeln!(md, "{row}");
    }
    let _ = writeln!(
        md,
        "\n(`ctrl 0` is CTRL_C_EVENT, `ctrl 2` CTRL_CLOSE_EVENT; `in 03` is the byte.)"
    );

    // Resize.
    let _ = writeln!(md, "\n## Resizing (cases `resize`, `alt-resize`)\n");
    for case in ["resize", "alt-resize"] {
        let _ = writeln!(md, "**{case}**\n");
        let _ = writeln!(
            md,
            "| Variant | Bytes after each resize | Repaints | History lines (ptyro's emulator, at the end) |\n|---|---|---|---|"
        );
        for (v, m) in vs.iter().zip(&metas) {
            let o = read(&out.join(v), &format!("{case}.out"));
            let mm = &m[case];
            let chunks: Vec<(u64, usize)> = mm["chunks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|c| Some((c[0].as_u64()?, c[1].as_u64()? as usize)))
                        .collect()
                })
                .unwrap_or_default();
            let resizes: Vec<(u64, u16, u16)> = mm["timeline"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|e| e["what"] == "resize")
                        .filter_map(|e| {
                            Some((
                                e["at"].as_u64()?,
                                e["cols"].as_u64()? as u16,
                                e["rows"].as_u64()? as u16,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            // The byte offset at each resize: everything read before it.
            let offs: Vec<usize> = resizes
                .iter()
                .map(|(t, _, _)| {
                    chunks.iter().filter(|(ct, _)| ct <= t).map(|(_, off)| *off).max().unwrap_or(0)
                })
                .collect();
            let mut sizes = Vec::new();
            let mut repaints = Vec::new();
            for (i, off) in offs.iter().enumerate() {
                let end = offs.get(i + 1).copied().unwrap_or(o.len());
                let seg = &o[(*off).min(end)..end];
                sizes.push(seg.len().to_string());
                let clears =
                    find(seg, b"\x1b[2J", 0).is_some() || find(seg, b"\x1b[H", 0).is_some();
                repaints.push(if clears {
                    "yes"
                } else if seg.is_empty() {
                    "—"
                } else {
                    "partly"
                });
            }
            let size = TermSize::new(
                mm["cols"].as_u64().unwrap_or(80) as u16,
                mm["rows"].as_u64().unwrap_or(24) as u16,
            );
            let rz: Vec<(usize, TermSize)> = offs
                .iter()
                .zip(&resizes)
                .map(|(o, (_, c, r))| (*o, TermSize::new(*c, *r)))
                .collect();
            let (_, _, history) = replay(&[&o], size, &rz);
            let _ = writeln!(
                md,
                "| {v} | {} | {} | {history} |",
                sizes.join(", "),
                repaints.join(", ")
            );
        }
        // The same bytes straight into ptyro's emulator, with the same resizes: the baseline.
        let case_bytes = cases::all(&cases::fixtures(fixtures))
            .into_iter()
            .find(|c| c.name == case)
            .map(|c| c.output)
            .unwrap_or_default();
        let rz: Vec<(usize, TermSize)> = cases::all(&[])
            .into_iter()
            .find(|c| c.name == case)
            .map(|c| {
                c.resizes.iter().map(|(_, c, r)| (usize::MAX, TermSize::new(*c, *r))).collect()
            })
            .unwrap_or_default();
        let base = cases::all(&[])
            .into_iter()
            .find(|c| c.name == case)
            .map(|c| TermSize::new(c.cols, c.rows))
            .unwrap_or(TermSize::new(80, 24));
        let (_, _, h) = replay(&[&case_bytes], base, &rz);
        let _ = writeln!(
            md,
            "\nWithout ConPTY (the program's own bytes, the same resizes): {h} history lines.\n"
        );
    }

    // Fixtures.
    let _ = writeln!(md, "## Recorded programs (cases `fixture-*`)\n");
    let _ = writeln!(
        md,
        "Each recording replayed through the pseudoconsole, then both into ptyro's emulator: rows whose text differs, and whether the colours and attributes match. Focus reporting is left out of the modes: conhost turns it on for itself when it starts (`?1004h`) and off when it closes.\n"
    );
    let _ = writeln!(md, "{}\n{rule}", khead.replacen("| Typed | |", "| Recording | Size |", 1));
    for (name, cols, rows, bytes) in cases::fixtures(fixtures) {
        let size = TermSize::new(cols, rows);
        let (want, wmodes, _) = replay(&[&bytes], size, &[]);
        let mut row = format!("| `{name}` | {cols}×{rows} |");
        for v in &vs {
            let o = untorn(read(&out.join(v), &format!("fixture-{name}.out")));
            let (got, gmodes, _) = replay(&[&o], size, &[]);
            let (wt, gt) = (want.plain_text(), got.plain_text());
            let diff =
                wt.lines().zip(gt.lines()).filter(|(a, b)| a.trim_end() != b.trim_end()).count()
                    + wt.lines().count().abs_diff(gt.lines().count());
            let styles = if want.ansi_text() == got.ansi_text() {
                "same styles"
            } else if visible(&want) == visible(&got) {
                "same look (styles differ only on blanks)"
            } else {
                "styles differ"
            };
            let modes = if mode_fields(&wmodes)
                .iter()
                .zip(mode_fields(&gmodes).iter())
                .all(|(a, b)| a == b || a.starts_with("focus_events"))
            {
                ""
            } else {
                ", modes differ"
            };
            let _ = write!(row, " {diff} rows differ, {styles}{modes} |");
        }
        let _ = writeln!(md, "{row}");
    }
    for (name, cols, rows, bytes) in cases::fixtures(fixtures) {
        let size = TermSize::new(cols, rows);
        let (want, wmodes, _) = replay(&[&bytes], size, &[]);
        for v in &vs {
            let o = untorn(read(&out.join(v), &format!("fixture-{name}.out")));
            let (got, gmodes, _) = replay(&[&o], size, &[]);
            if wmodes != gmodes {
                let diff: Vec<String> = mode_fields(&wmodes)
                    .into_iter()
                    .zip(mode_fields(&gmodes))
                    .filter(|(a, b)| a != b && !a.starts_with("focus_events"))
                    .map(|(a, b)| format!("{a} → {}", b.split(": ").nth(1).unwrap_or(&b)))
                    .collect();
                if !diff.is_empty() {
                    let _ = writeln!(md, "\n`{name}` under {v}: modes {}", diff.join(", "));
                }
            }
            if visible(&want) != visible(&got) {
                let (wa, ga) = (want.ansi_text(), got.ansi_text());
                if let Some((a, b)) = wa.lines().zip(ga.lines()).find(|(a, b)| a != b) {
                    let _ = writeln!(
                        md,
                        "\n`{name}` under {v}, the first row whose styles differ:\n\n    want `{}`\n    got  `{}`",
                        short(a.as_bytes(), 300),
                        short(b.as_bytes(), 300)
                    );
                }
            }
            let firsts: Vec<String> = want
                .plain_text()
                .lines()
                .zip(got.plain_text().lines())
                .enumerate()
                .filter(|(_, (a, b))| a.trim_end() != b.trim_end())
                .take(3)
                .map(|(i, (a, b))| {
                    format!(
                        "    row {i}: want `{}`\n    row {i}: got  `{}`",
                        a.trim_end(),
                        b.trim_end()
                    )
                })
                .collect();
            if !firsts.is_empty() {
                let _ = writeln!(
                    md,
                    "\n`{name}` under {v}, the first rows that differ:\n\n{}",
                    firsts.join("\n")
                );
            }
        }
    }

    // Takeover.
    let _ = writeln!(md, "\n## WIN0.3: a second process takes the pseudoconsole over\n");
    let _ = writeln!(
        md,
        "The old process opens a pseudoconsole with an echoing child, then exits without closing it. *none*: it hands nothing on. *inherit*: it starts the new process with the six handles (our pipe ends, the HPCON's signal, reference and conhost handles, the child). *dup*: it copies them into a new process that is already running. The new one builds an HPCON from the three and drives it.\n"
    );
    let tdir = out.join("takeover");
    let mut rows = Vec::new();
    for v in ["inbox", "nuget"] {
        for mode in ["none", "inherit", "dup"] {
            let p = tdir.join(v).join(mode).join("result.json");
            let Ok(t) = std::fs::read_to_string(&p) else { continue };
            let r: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            let old = &r["old"];
            let line = if mode == "none" {
                format!(
                    "| {v} | {mode} | conhost `{}` | child alive 2 s later: {}; conhost alive: {} | |",
                    old["conhost_image"].as_str().unwrap_or("?").rsplit('\\').next().unwrap_or(""),
                    r["child_alive_2s_later"],
                    r["conhost_alive_2s_later"]
                )
            } else {
                let n = &r["new"];
                if let Some(e) = n["error"].as_str().or(old["error"].as_str()) {
                    format!("| {v} | {mode} | | error: {e} | |")
                } else {
                    let before: Vec<u64> = old["beats_seen"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_u64).collect())
                        .unwrap_or_default();
                    let after: Vec<u64> = n["beats_seen"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_u64).collect())
                        .unwrap_or_default();
                    let gap = match (before.last(), after.first()) {
                        (Some(b), Some(a)) if *a == b + 1 => "none lost".to_owned(),
                        (Some(b), Some(a)) => format!("old saw ..{b}, new from {a}"),
                        _ => "?".to_owned(),
                    };
                    format!(
                        "| {v} | {mode} | old gone: {}; child alive: {} | heartbeats {} ({gap}), typed→echoed {}, resize hr {} → {}, close {} ms → child exited {} (code {}), output ended {} | {} |",
                        n["old_gone"],
                        n["child_alive_after_old"],
                        n["beats_after_takeover"],
                        n["echoed"],
                        n["resize_hr"],
                        n["resized"],
                        n["close_ms"],
                        n["child_exited_after_close"],
                        n["child_exit_code"],
                        n["output_ended"],
                        if n["echoed"] == true
                            && n["resized"] == true
                            && n["child_exited_after_close"] == true
                        {
                            "**works**"
                        } else {
                            "**fails**"
                        }
                    )
                }
            };
            rows.push(line);
        }
    }
    if rows.is_empty() {
        let _ = writeln!(md, "(no takeover results)");
    } else {
        let _ = writeln!(md, "| ConPTY | Mode | Before | After | |\n|---|---|---|---|---|");
        for r in rows {
            let _ = writeln!(md, "{r}");
        }
    }
    Ok(md)
}

/// The child's log lines that start with `prefix` (the prefix taken off), sorted to the input
/// the host had last typed when they arrived. Both log the wall clock (ms since the epoch); a
/// line belongs to the latest input typed up to 40 ms after it (the child may log a moment
/// before the host notes its write).
fn by_input(log: &[u8], meta: &Value, n: usize, prefix: &str) -> Vec<Vec<String>> {
    let tl = meta["timeline"].as_array().cloned().unwrap_or_default();
    let typed: Vec<(usize, u64)> = tl
        .iter()
        .filter(|e| e["what"] == "input")
        .filter_map(|e| Some((e["i"].as_u64()? as usize, e["epoch"].as_u64()?)))
        .collect();
    let mut out = vec![Vec::new(); n];
    for l in String::from_utf8_lossy(log).lines() {
        let Some((t, rest)) = l.split_once(' ') else { continue };
        let (Ok(t), Some(body)) = (t.parse::<u64>(), rest.strip_prefix(prefix)) else { continue };
        let host = t;
        if let Some((i, _)) = typed.iter().rev().find(|(_, at)| *at <= host + 40)
            && *i < n
        {
            out[*i].push(body.to_owned());
        }
    }
    out
}

/// `TermModes { a: x, b: y }` as `a: x` fields.
fn mode_fields(m: &str) -> Vec<String> {
    let inner = m.split_once('{').map_or(m, |x| x.1).trim_end_matches('}').trim();
    inner.split(", ").map(|f| f.trim().to_owned()).collect()
}
