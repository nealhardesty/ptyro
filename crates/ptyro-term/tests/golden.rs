//! Golden tests (ROADMAP M1.8): replay recorded program output (`tests/fixtures/<name>.<COLS>x<ROWS>.ptylog`)
//! and snapshot the resulting screen. Record new fixtures with `make fixture` (docs/fixtures.md).
//!
//! Each fixture is also replayed in small odd-sized chunks: the screen must not depend on where
//! the PTY happened to split reads.

use std::path::Path;

use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};

fn replay(bytes: &[u8], size: TermSize, chunk: Option<usize>) -> (ScreenCache, String) {
    let mut emu = AlacrittyEmulator::new(size, 1000);
    let mut out = FeedOutput::default();
    match chunk {
        None => emu.feed(bytes, &mut out),
        Some(n) => {
            // Vary chunk sizes deterministically: n, n+1, n+2, … wrapping.
            let mut i = 0;
            let mut k = 0;
            while i < bytes.len() {
                let len = (n + k % 7).min(bytes.len() - i);
                emu.feed(&bytes[i..i + len], &mut out);
                i += len;
                k += 1;
            }
        }
    }
    if emu.sync_deadline().is_some() {
        emu.expire_sync(&mut out);
    }
    let mut tracker = FrameTracker::new(&mut emu);
    let mut cache = ScreenCache::new(size);
    cache.apply(true, tracker.seq(), &tracker.build(&emu, None, &mut StyleTable::default()));
    let modes = format!("{:?}", emu.modes());
    (cache, modes)
}

fn snapshot_text(cache: &ScreenCache, modes: &str) -> String {
    format!(
        "{}──── cursor {:?}\n──── modes {modes}\n──── title {:?}\n──── ansi\n{}",
        cache.plain_text(),
        cache.cursor,
        cache.title,
        cache.ansi_text().replace('\x1b', "␛")
    )
}

#[test]
fn fixtures_render_as_recorded() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut names: Vec<_> =
        std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).map(|e| e.path()).collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures in {}", dir.display());
    for path in names {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(stem) = file.strip_suffix(".ptylog") else { continue };
        let (name, size) = stem.rsplit_once('.').expect("name.COLSxROWS.ptylog");
        let (c, r) = size.split_once('x').expect("COLSxROWS");
        let size = TermSize::new(c.parse().unwrap(), r.parse().unwrap());
        let bytes = std::fs::read(&path).unwrap();

        let (whole, modes) = replay(&bytes, size, None);
        let (chunked, chunked_modes) = replay(&bytes, size, Some(1));
        assert_eq!(whole.plain_text(), chunked.plain_text(), "{name}: chunking changed the screen");
        assert_eq!(whole.ansi_text(), chunked.ansi_text(), "{name}: chunking changed the styles");
        assert_eq!(modes, chunked_modes, "{name}: chunking changed the modes");

        insta::assert_snapshot!(name, snapshot_text(&whole, &modes));
    }
}
