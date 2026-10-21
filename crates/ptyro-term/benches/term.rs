//! Criterion benches (M8.3, SPEC §18.3): emulator parse throughput, frame building (keyframe
//! and delta), scrollback documents, and the handoff image. `make criterion`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ptyro_core::TermSize;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};

fn data() -> Vec<(&'static str, Vec<u8>)> {
    let plain: Vec<u8> =
        (0..20_000).flat_map(|_| [b'x'; 180].into_iter().chain(*b"\r\n")).collect();
    let short: Vec<u8> = (0..300_000u32).flat_map(|i| format!("{i}\r\n").into_bytes()).collect();
    let sgr: Vec<u8> = (0..40_000)
        .flat_map(|i| format!("\x1b[3{}mword{i}\x1b[0m some text here\r\n", i % 8).into_bytes())
        .collect();
    let cjk: Vec<u8> = (0..20_000)
        .flat_map(|_| "漢字かなカナ한글 emoji 🙂 mixed text\r\n".as_bytes().to_vec())
        .collect();
    vec![("plain-180col", plain), ("short-lines", short), ("sgr", sgr), ("cjk", cjk)]
}

fn parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("parse");
    for (name, bytes) in data() {
        g.throughput(Throughput::Bytes(bytes.len() as u64));
        g.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, bytes| {
            b.iter(|| {
                let mut e = AlacrittyEmulator::new(TermSize::new(200, 50), 10_000);
                let mut t = FrameTracker::new(&mut e);
                let mut out = FeedOutput::default();
                for part in bytes.chunks(65_536) {
                    e.feed(part, &mut out);
                    t.update(&mut e);
                    out.replies.clear();
                }
            });
        });
    }
    g.finish();
}

/// A full, colourful 200×60 screen.
fn busy(size: TermSize) -> AlacrittyEmulator {
    let mut e = AlacrittyEmulator::new(size, 10_000);
    let mut out = FeedOutput::default();
    for y in 0..size.rows {
        let line: String = (0..size.cols / 10)
            .map(|i| format!("\x1b[3{}m{:<9}\x1b[0m ", (i + y) % 8, format!("w{y}x{i}")))
            .collect();
        e.feed(format!("{line}\r\n").as_bytes(), &mut out);
    }
    e
}

fn frames(c: &mut Criterion) {
    let size = TermSize::new(200, 60);
    let mut e = busy(size);
    let mut t = FrameTracker::new(&mut e);
    let mut styles = StyleTable::default();
    let kf = t.build(&e, None, &mut styles);
    let bytes = rmp_serde::to_vec_named(&kf).unwrap();
    eprintln!("keyframe 200x60: {} bytes on the wire (budget 64 KiB)", bytes.len());
    c.bench_function("frame/keyframe-200x60", |b| {
        b.iter(|| t.build(&e, None, &mut StyleTable::default()))
    });
    let mut out = FeedOutput::default();
    c.bench_function("frame/delta-one-row", |b| {
        b.iter(|| {
            e.feed(b"\x1b[10;5Hchanged", &mut out);
            t.update(&mut e);
            let since = t.seq().saturating_sub(1);
            t.build(&e, Some(since), &mut styles)
        })
    });
}

fn scrollback(c: &mut Criterion) {
    let mut e = AlacrittyEmulator::new(TermSize::new(120, 40), 10_000);
    let mut out = FeedOutput::default();
    for i in 0..12_000 {
        e.feed(
            format!("\x1b[32mline\x1b[0m {i} of a scrollback document\r\n").as_bytes(),
            &mut out,
        );
    }
    c.bench_function("scrollback/doc-10k", |b| {
        b.iter(|| ptyro_term::frame::scrollback_doc(&e, 10_000))
    });
    c.bench_function("handoff/image-10k", |b| {
        b.iter(|| rmp_serde::to_vec(&e.image()).unwrap().len())
    });
    let img = rmp_serde::to_vec(&e.image()).unwrap();
    eprintln!("handoff image, 10k lines × 120 cols: {} KiB", img.len() / 1024);
    c.bench_function("handoff/restore-10k", |b| {
        b.iter(|| AlacrittyEmulator::from_image(rmp_serde::from_slice(&img).unwrap()))
    });
}

criterion_group!(benches, parse, frames, scrollback);
criterion_main!(benches);
