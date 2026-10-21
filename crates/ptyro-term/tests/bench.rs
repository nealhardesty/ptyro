//! Parse-throughput micro benchmarks. Ignored by default: `make bench` runs them in release.

use std::time::Instant;

use ptyro_core::TermSize;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker};

fn run(name: &str, data: &[u8], size: TermSize, chunk: usize) {
    let mut emu = AlacrittyEmulator::new(size, 10_000);
    let mut tracker = FrameTracker::new(&mut emu);
    let mut out = FeedOutput::default();
    let start = Instant::now();
    for part in data.chunks(chunk) {
        emu.feed(part, &mut out);
        tracker.update(&mut emu);
        out.replies.clear();
        out.events.clear();
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{name:<28} {:>6.1} MB {:>7.0} MB/s",
        data.len() as f64 / 1e6,
        data.len() as f64 / 1e6 / secs
    );
}

#[test]
#[ignore = "benchmark; run with --ignored in release"]
fn emulator_parse_throughput() {
    let long: Vec<u8> =
        (0..200_000).flat_map(|_| [b'x'; 180].into_iter().chain(*b"\r\n")).collect();
    let short: Vec<u8> = (0..3_000_000u32).flat_map(|i| format!("{i}\r\n").into_bytes()).collect();
    let colored: Vec<u8> = (0..300_000)
        .flat_map(|i| format!("\x1b[3{}mword{i}\x1b[0m some text here\r\n", i % 8).into_bytes())
        .collect();
    for chunk in [4096, 65536] {
        run(&format!("long lines, {chunk} B reads"), &long, TermSize::new(200, 50), chunk);
        run(&format!("short lines, {chunk} B reads"), &short, TermSize::new(200, 50), chunk);
        run(&format!("SGR-coloured, {chunk} B reads"), &colored, TermSize::new(200, 50), chunk);
    }
}

#[test]
#[ignore = "benchmark; run with --ignored in release"]
fn sniffer_throughput() {
    use ptyro_term::sniff::Sniffer;
    let long: Vec<u8> =
        (0..200_000).flat_map(|_| [b'x'; 180].into_iter().chain(*b"\r\n")).collect();
    let mut s = Sniffer::default();
    let mut seqs = Vec::new();
    let start = Instant::now();
    for part in long.chunks(4096) {
        s.scan(part, &mut seqs);
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{:<28} {:>6.1} MB {:>7.0} MB/s",
        "sniffer only",
        long.len() as f64 / 1e6,
        long.len() as f64 / 1e6 / secs
    );
}
