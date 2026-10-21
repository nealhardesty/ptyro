//! Painting a session's screen into the TUI's buffer (M8.3): a full 200×60 colourful screen.

use criterion::{Criterion, criterion_group, criterion_main};
use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn paint(c: &mut Criterion) {
    let size = TermSize::new(200, 60);
    let mut e = AlacrittyEmulator::new(size, 100);
    let mut out = FeedOutput::default();
    for y in 0..60u16 {
        let line: String = (0..20)
            .map(|i| format!("\x1b[3{}m{:<9}\x1b[0m ", (i + y) % 8, format!("w{y}x{i}")))
            .collect();
        e.feed(format!("{line}\r\n").as_bytes(), &mut out);
    }
    let mut t = FrameTracker::new(&mut e);
    let delta = t.build(&e, None, &mut StyleTable::default());
    let mut cache = ScreenCache::new(size);
    cache.apply(true, t.seq(), &delta);
    let area = Rect::new(0, 0, 200, 60);
    let mut buf = Buffer::empty(area);
    c.bench_function("paint/screen-200x60", |b| {
        b.iter(|| {
            ptyro_tui::paint::screen(&cache, &mut buf, area, ptyro_tui::theme::Depth::True, false)
        })
    });
}

criterion_group!(benches, paint);
criterion_main!(benches);
