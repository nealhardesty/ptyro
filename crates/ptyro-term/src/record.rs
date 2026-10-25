//! The recorder's file format (SPEC §7.6, M8.2): a session's raw PTY output with timing, so it
//! can be replayed, exported (asciicast v2, text) and used as scrollback deeper than memory.
//!
//! ```text
//! file    = "PTYROREC" 0x01 record*
//! record  = kind:u8 len:u32be at_us:u64be payload[len]
//! kind 0  output   the bytes the program wrote
//! kind 1  resize   cols:u16be rows:u16be
//! kind 2  keyframe ANSI that redraws the screen (written at the start of a segment, so a ring
//!                  that lost its beginning still replays to the right picture)
//! kind 3  start    cols:u16be rows:u16be unix_ms:u64be; `at_us` of later records count from it
//! ```
//!
//! The ring: `pty.log` grows to half the budget, then becomes `pty.log.1` (replacing the older
//! one) and a new `pty.log` starts with a `start` and a `keyframe`. A torn last record (a
//! crash mid-write) is ignored, and cut off when the recording is continued.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use ptyro_core::TermSize;

pub const MAGIC: &[u8; 9] = b"PTYROREC\x01";
const HEADER: usize = 1 + 4 + 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Output(Vec<u8>),
    Resize(TermSize),
    Keyframe(Vec<u8>),
    Start(TermSize),
}

/// A record with its absolute time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed {
    pub at_ms: u64,
    /// Microseconds since the recording's first `start` (for playback timing).
    pub t_us: u64,
    pub record: Record,
}

/// Appends a session's output to `<dir>/pty.log`, ring-rotated at `max_bytes`.
pub struct Recorder {
    dir: PathBuf,
    file: File,
    len: u64,
    max_bytes: u64,
    started: Instant,
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).read(true).append(true).mode(0o600).open(path)
}

/// A recording that begins with another magic than ours is another version's (a rollback, or a
/// newer build): leave it be, under another name, rather than truncate it as torn.
fn set_aside_unknown(path: &Path) {
    let Ok(f) = File::open(path) else { return };
    let mut magic = [0u8; MAGIC.len()];
    if f.metadata().is_ok_and(|m| m.len() >= magic.len() as u64)
        && f.read_exact_at(&mut magic, 0).is_ok()
        && &magic != MAGIC
    {
        let _ = std::fs::rename(
            path,
            path.with_extension(format!("log.unrecognised-{}", now_unix_ms())),
        );
    }
}

/// Bytes at the start of `file` that are the magic and whole records; 0 when the magic itself
/// is torn or wrong.
fn valid_len(file: &File) -> io::Result<u64> {
    let total = file.metadata()?.len();
    let mut magic = [0u8; MAGIC.len()];
    if total < magic.len() as u64 {
        return Ok(0);
    }
    file.read_exact_at(&mut magic, 0)?;
    if &magic != MAGIC {
        return Ok(0);
    }
    let mut pos = magic.len() as u64;
    let mut header = [0u8; HEADER];
    while pos + HEADER as u64 <= total {
        file.read_exact_at(&mut header, pos)?;
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let end = pos + HEADER as u64 + u64::from(len);
        if end > total {
            break;
        }
        pos = end;
    }
    Ok(pos)
}

impl Recorder {
    /// Open (or continue) the recording in `dir` for a session of `size`.
    pub fn open(dir: &Path, size: TermSize, max_bytes: u64) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let path = dir.join("pty.log");
        set_aside_unknown(&path);
        let mut file = open_append(&path)?;
        // Continue after the last whole record: appending after a torn one (a crash mid-write)
        // would make everything after it misparse.
        let mut len = valid_len(&file)?;
        if len != file.metadata()?.len() {
            file.set_len(len)?;
        }
        if len == 0 {
            file.write_all(MAGIC)?;
            len = MAGIC.len() as u64;
        }
        let mut r = Self {
            dir: dir.to_path_buf(),
            file,
            len,
            max_bytes: max_bytes.max(64 * 1024),
            started: Instant::now(),
        };
        r.start(size)?;
        Ok(r)
    }

    fn write(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        let at = self.started.elapsed().as_micros() as u64;
        let mut buf = Vec::with_capacity(HEADER + payload.len());
        buf.push(kind);
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(&at.to_be_bytes());
        buf.extend_from_slice(payload);
        self.file.write_all(&buf)?;
        self.len += buf.len() as u64;
        Ok(())
    }

    fn start(&mut self, size: TermSize) -> io::Result<()> {
        self.started = Instant::now();
        let mut p = Vec::with_capacity(12);
        p.extend_from_slice(&size.cols.to_be_bytes());
        p.extend_from_slice(&size.rows.to_be_bytes());
        p.extend_from_slice(&now_unix_ms().to_be_bytes());
        self.write(3, &p)
    }

    /// The program's output. `keyframe` (the screen as ANSI, and its size) is asked for only
    /// when the ring rotates.
    pub fn output(
        &mut self,
        bytes: &[u8],
        keyframe: impl FnOnce() -> (Vec<u8>, TermSize),
    ) -> io::Result<()> {
        if self.len + bytes.len() as u64 > self.max_bytes / 2 {
            let (kf, size) = keyframe();
            self.rotate(&kf, size)?;
        }
        self.write(0, bytes)
    }

    pub fn resize(&mut self, size: TermSize) -> io::Result<()> {
        let mut p = Vec::with_capacity(4);
        p.extend_from_slice(&size.cols.to_be_bytes());
        p.extend_from_slice(&size.rows.to_be_bytes());
        self.write(1, &p)
    }

    fn rotate(&mut self, keyframe: &[u8], size: TermSize) -> io::Result<()> {
        let cur = self.dir.join("pty.log");
        std::fs::rename(&cur, self.dir.join("pty.log.1"))?;
        self.file = open_append(&cur)?;
        self.file.write_all(MAGIC)?;
        self.len = MAGIC.len() as u64;
        self.start(size)?;
        self.write(2, keyframe)
    }

    /// Bytes on disk (both segments).
    pub fn bytes(&self) -> u64 {
        self.len + std::fs::metadata(self.dir.join("pty.log.1")).map_or(0, |m| m.len())
    }
}

fn parse(bytes: &[u8], out: &mut Vec<Timed>, base: &mut (u64, u64)) {
    let Some(mut rest) = bytes.strip_prefix(MAGIC.as_slice()) else { return };
    while rest.len() >= HEADER {
        let kind = rest[0];
        let len = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        let at = u64::from_be_bytes(rest[5..13].try_into().expect("8 bytes"));
        if rest.len() < HEADER + len {
            break; // torn
        }
        let p = &rest[HEADER..HEADER + len];
        let size = |p: &[u8]| {
            (p.len() >= 4).then(|| {
                TermSize::new(u16::from_be_bytes([p[0], p[1]]), u16::from_be_bytes([p[2], p[3]]))
            })
        };
        let record = match kind {
            0 => Some(Record::Output(p.to_vec())),
            1 => size(p).map(Record::Resize),
            2 => Some(Record::Keyframe(p.to_vec())),
            3 => {
                if p.len() >= 12 {
                    let unix = u64::from_be_bytes(p[4..12].try_into().expect("8 bytes"));
                    // The first start is t = 0; later ones (a ring segment, a restart, an
                    // upgrade) continue from their wall-clock time.
                    if base.0 == 0 {
                        base.0 = unix;
                    }
                    // Starts carry milliseconds, records microseconds: a segment begun within
                    // the same millisecond must not land before what came before it.
                    let last = out.last().map_or(0, |t: &Timed| t.t_us);
                    base.1 = unix.saturating_sub(base.0).saturating_mul(1000).max(last);
                }
                size(p).map(Record::Start)
            }
            _ => None,
        };
        if let Some(record) = record {
            // Times come from the file: a corrupt one must not overflow.
            let t_us = base.1.saturating_add(if kind == 3 { 0 } else { at });
            out.push(Timed { at_ms: base.0.saturating_add(t_us / 1000), t_us, record });
        }
        rest = &rest[HEADER + len..];
    }
}

/// Everything recorded in `dir`, oldest first (both ring segments).
pub fn read_dir(dir: &Path) -> io::Result<Vec<Timed>> {
    let mut out = Vec::new();
    let mut base = (0, 0);
    let mut found = false;
    for name in ["pty.log.1", "pty.log"] {
        let mut bytes = Vec::new();
        match File::open(dir.join(name)) {
            Ok(mut f) => {
                f.read_to_end(&mut bytes)?;
                found = true;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        parse(&bytes, &mut out, &mut base);
    }
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no recording in {}", dir.display()),
        ));
    }
    Ok(out)
}

/// The size a recording starts at.
pub fn first_size(records: &[Timed]) -> Option<TermSize> {
    records.iter().find_map(|r| match r.record {
        Record::Start(s) | Record::Resize(s) => Some(s),
        _ => None,
    })
}

/// Feed a recording into an emulator (its replies are dropped: nobody asked).
pub fn replay(records: &[Timed], emu: &mut dyn crate::Emulator) {
    let mut sink = crate::FeedOutput::default();
    for r in records {
        match &r.record {
            Record::Output(b) | Record::Keyframe(b) => emu.feed(b, &mut sink),
            Record::Resize(s) | Record::Start(s) => {
                if emu.size() != *s {
                    emu.resize(*s);
                }
            }
        }
        sink.replies.clear();
        sink.events.clear();
    }
}

/// asciicast v2 (asciinema): a header line, then `[seconds, "o"|"r", data]` lines. Output split
/// inside a UTF-8 character is carried to the next event.
pub fn asciicast(records: &[Timed], title: &str) -> String {
    let size = first_size(records).unwrap_or(TermSize::DEFAULT);
    let start = records.first().map_or(0, |r| r.at_ms);
    let mut out = format!(
        "{{\"version\":2,\"width\":{},\"height\":{},\"timestamp\":{},\"title\":{},\"env\":{{\"TERM\":\"xterm-256color\"}}}}\n",
        size.cols,
        size.rows,
        start / 1000,
        json_str(title)
    );
    let mut carry: Vec<u8> = Vec::new();
    let mut first = true;
    for r in records {
        let t = r.t_us as f64 / 1e6;
        match &r.record {
            Record::Output(b) | Record::Keyframe(b) => {
                carry.extend_from_slice(b);
                let valid = match std::str::from_utf8(&carry) {
                    Ok(_) => carry.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => carry.len(),
                };
                let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                carry.drain(..valid);
                if !text.is_empty() {
                    out.push_str(&format!("[{t:.6},\"o\",{}]\n", json_str(&text)));
                }
            }
            Record::Resize(s) | Record::Start(s) => {
                if !first || Some(*s) != Some(size) {
                    out.push_str(&format!("[{t:.6},\"r\",\"{}x{}\"]\n", s.cols, s.rows));
                }
            }
        }
        first = false;
    }
    out
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Just the output bytes, as a `.ptylog` fixture (docs/fixtures.md).
pub fn raw_output(records: &[Timed]) -> Vec<u8> {
    let mut v = Vec::new();
    for r in records {
        if let Record::Output(b) = &r.record {
            v.extend_from_slice(b);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AlacrittyEmulator, Emulator};

    #[test]
    fn record_rotate_and_replay() {
        let dir = tempfile_dir();
        let size = TermSize::new(20, 4);
        let mut rec = Recorder::open(&dir, size, 64 * 1024).unwrap();
        let mut emu = AlacrittyEmulator::new(size, 100);
        let mut sink = crate::FeedOutput::default();
        for i in 0..4000 {
            let line = format!("line {i}\r\n");
            emu.feed(line.as_bytes(), &mut sink);
            rec.output(line.as_bytes(), || {
                let mut kf = b"\x1b[H\x1b[2J".to_vec();
                kf.extend_from_slice(b"(keyframe)\r\n");
                (kf, size)
            })
            .unwrap();
        }
        rec.resize(TermSize::new(30, 5)).unwrap();
        rec.output(b"after resize", || unreachable!()).unwrap();
        assert!(dir.join("pty.log.1").exists(), "rotated");
        assert!(rec.bytes() <= 64 * 1024 + 2048, "{}", rec.bytes());
        let recs = read_dir(&dir).unwrap();
        assert!(recs.iter().any(|r| matches!(r.record, Record::Keyframe(_))));
        let mut back = AlacrittyEmulator::new(size, 100);
        replay(&recs, &mut back);
        assert_eq!(back.size(), TermSize::new(30, 5));
        let mut row = Vec::new();
        back.line(4, &mut row);
        assert!(row.iter().map(|c| c.c).collect::<String>().starts_with("after resize"));
        let cast = asciicast(&recs, "t");
        assert!(cast.starts_with("{\"version\":2"));
        assert!(cast.contains("\"r\",\"30x5\""));
        // Times never go backwards.
        let ts: Vec<u64> = recs.iter().map(|r| r.t_us).collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{:?}", &ts[..5]);
        // A torn tail is ignored.
        let p = dir.join("pty.log");
        let mut b = std::fs::read(&p).unwrap();
        b.extend_from_slice(&[0, 0, 0, 9]);
        std::fs::write(&p, b).unwrap();
        assert_eq!(read_dir(&dir).unwrap().len(), recs.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopening_cuts_a_torn_tail_and_rewrites_a_torn_header() {
        let dir = tempfile_dir();
        let size = TermSize::new(20, 4);
        let mut rec = Recorder::open(&dir, size, 64 * 1024).unwrap();
        rec.output(b"one", || unreachable!()).unwrap();
        drop(rec);
        let p = dir.join("pty.log");
        let mut b = std::fs::read(&p).unwrap();
        b.extend_from_slice(&[0, 0, 0, 0, 50, 0, 0]); // a crash mid-header
        std::fs::write(&p, b).unwrap();
        let mut rec = Recorder::open(&dir, size, 64 * 1024).unwrap();
        rec.output(b"two", || unreachable!()).unwrap();
        let out: Vec<Vec<u8>> = read_dir(&dir)
            .unwrap()
            .into_iter()
            .filter_map(|r| match r.record {
                Record::Output(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(out, [b"one".to_vec(), b"two".to_vec()]);
        drop(rec);
        // Only part of the magic made it to disk.
        std::fs::write(&p, &MAGIC[..4]).unwrap();
        let mut rec = Recorder::open(&dir, size, 64 * 1024).unwrap();
        rec.output(b"three", || unreachable!()).unwrap();
        let recs = read_dir(&dir).unwrap();
        assert!(matches!(&recs.last().unwrap().record, Record::Output(b) if b == b"three"));
        assert_eq!(rec.len, std::fs::metadata(&p).unwrap().len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_in_another_format_is_set_aside_not_truncated() {
        let dir = tempfile_dir();
        let p = dir.join("pty.log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, b"PTYLOG99-from-another-version-with-real-content").unwrap();
        let mut rec = Recorder::open(&dir, TermSize::new(20, 4), 64 * 1024).unwrap();
        rec.output(b"fresh", || unreachable!()).unwrap();
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("unrecognised"))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read(kept[0].path()).unwrap(),
            b"PTYLOG99-from-another-version-with-real-content"
        );
        assert!(
            matches!(&read_dir(&dir).unwrap().last().unwrap().record, Record::Output(b) if b == b"fresh")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_values_neither_overflow_nor_resize_wildly() {
        let rec = |kind: u8, at: u64, p: &[u8]| {
            let mut v = vec![kind];
            v.extend_from_slice(&(p.len() as u32).to_be_bytes());
            v.extend_from_slice(&at.to_be_bytes());
            v.extend_from_slice(p);
            v
        };
        let start = |cols: u16, rows: u16, unix: u64| {
            [cols.to_be_bytes(), rows.to_be_bytes()]
                .concat()
                .into_iter()
                .chain(unix.to_be_bytes())
                .collect::<Vec<u8>>()
        };
        let bytes = [
            MAGIC.to_vec(),
            rec(3, 0, &start(0, 0, 1)),
            rec(3, 0, &start(65535, 65535, u64::MAX)),
            rec(0, u64::MAX, b"x"),
            rec(1, u64::MAX, &[0, 0, 0, 0]),
        ]
        .concat();
        let mut out = Vec::new();
        parse(&bytes, &mut out, &mut (0, 0));
        assert_eq!(out.len(), 4);
        assert_eq!(out[1].record, Record::Start(TermSize::MAX));
        assert_eq!(out[3].record, Record::Resize(TermSize::MIN));
        let mut emu = AlacrittyEmulator::new(TermSize::new(10, 2), 10);
        replay(&out, &mut emu);
        assert_eq!(emu.size(), TermSize::MIN);
    }

    #[test]
    fn asciicast_keeps_split_utf8_together() {
        let recs = vec![
            Timed { at_ms: 0, t_us: 0, record: Record::Start(TermSize::new(10, 2)) },
            Timed { at_ms: 0, t_us: 10, record: Record::Output("漢".as_bytes()[..2].to_vec()) },
            Timed { at_ms: 0, t_us: 20, record: Record::Output("漢".as_bytes()[2..].to_vec()) },
        ];
        let c = asciicast(&recs, "x");
        assert!(c.contains("\"o\",\"漢\""), "{c}");
        assert!(!c.contains('\u{fffd}'));
    }

    fn tempfile_dir() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "ptyro-rec-{}-{}-{n}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
