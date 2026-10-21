//! Property tests: deltas always add up to the keyframe, and nothing panics on arbitrary input.

use proptest::prelude::*;
use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_term::sniff::Sniffer;
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput, FrameTracker, StyleTable};

/// Pieces of realistic terminal traffic, so random sequences exercise the interesting paths.
fn chunk() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        "[a-z ]{1,12}".prop_map(String::into_bytes),
        Just(b"\r\n".to_vec()),
        Just("漢字".as_bytes().to_vec()),
        Just("e\u{301}".as_bytes().to_vec()),
        Just("👍".as_bytes().to_vec()),
        (1u8..30, 1u8..90).prop_map(|(r, c)| format!("\x1b[{r};{c}H").into_bytes()),
        (0u8..108).prop_map(|n| format!("\x1b[{n}m").into_bytes()),
        (0u8..255).prop_map(|n| format!("\x1b[38;5;{n}m").into_bytes()),
        Just(b"\x1b[38;2;10;200;30m".to_vec()),
        Just(b"\x1b[2J".to_vec()),
        Just(b"\x1b[K".to_vec()),
        Just(b"\x1b[3L".to_vec()),
        Just(b"\x1b[2M".to_vec()),
        Just(b"\x1b[5;10r".to_vec()),
        Just(b"\x1b[r".to_vec()),
        Just(b"\x1bM".to_vec()),
        Just(b"\x1b[?1049h".to_vec()),
        Just(b"\x1b[?1049l".to_vec()),
        Just(b"\x1b[?25l".to_vec()),
        Just(b"\x1b[?25h".to_vec()),
        Just(b"\x1b[?2004h".to_vec()),
        Just(b"\x1b[?1000h\x1b[?1006h".to_vec()),
        Just(b"\x1b]0;title\x07".to_vec()),
        Just(b"\x1b[?2026h".to_vec()),
        Just(b"\x1b[?2026l".to_vec()),
        Just(b"\t".to_vec()),
        Just(b"\x08".to_vec()),
        prop::collection::vec(any::<u8>(), 0..16),
    ]
}

#[derive(Debug, Clone)]
enum Step {
    Feed(Vec<u8>),
    Resize(u16, u16),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => prop::collection::vec(chunk(), 1..6).prop_map(|c| Step::Feed(c.concat())),
        1 => (4u16..90, 2u16..30).prop_map(|(c, r)| Step::Resize(c, r)),
    ]
}

fn keyframe(emu: &AlacrittyEmulator, t: &mut FrameTracker) -> ScreenCache {
    let mut c = ScreenCache::new(emu.size());
    c.apply(true, t.seq(), &t.build(emu, None, &mut StyleTable::default()));
    c
}

/// What a viewer sees: text plus resolved styles per cell, cursor, modes, title, size.
fn view(c: &ScreenCache) -> Vec<String> {
    let mut out: Vec<String> = c
        .rows
        .iter()
        .map(|r| r.iter().map(|cell| format!("{}{:?}", cell.text, c.style(cell.style))).collect())
        .collect();
    out.push(format!("{:?} {:?} {:?} {}", c.cursor, c.modes, c.title, c.size));
    out
}

proptest! {
    // 256 cases by default; `PROPTEST_CASES=5000 cargo test` for a deeper run.

    #[test]
    fn incremental_deltas_equal_keyframe(steps in prop::collection::vec(step(), 1..25)) {
        let mut emu = AlacrittyEmulator::new(TermSize::new(40, 12), 50);
        let mut tracker = FrameTracker::new(&mut emu);
        let mut client = keyframe(&emu, &mut tracker);
        let mut styles = StyleTable::default();
        // Initial keyframe for the incremental client, sharing its style table from now on.
        client.apply(true, tracker.seq(), &tracker.build(&emu, None, &mut styles));
        for s in steps {
            match s {
                Step::Feed(bytes) => {
                    let mut out = FeedOutput::default();
                    emu.feed(&bytes, &mut out);
                    if emu.sync_deadline().is_some() {
                        emu.expire_sync(&mut out);
                    }
                }
                Step::Resize(c, r) => emu.resize(TermSize::new(c, r)),
            }
            let since = client.seq;
            if tracker.update(&mut emu) {
                let d = tracker.build(&emu, Some(since), &mut styles);
                client.apply(false, tracker.seq(), &d);
            }
            prop_assert_eq!(view(&client), view(&keyframe(&emu, &mut tracker)));
        }
    }

    #[test]
    fn sniffer_and_emulator_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut s = Sniffer::default();
        let mut seqs = Vec::new();
        s.scan(&data, &mut seqs);
        let mut emu = AlacrittyEmulator::new(TermSize::new(20, 5), 10);
        let mut out = FeedOutput::default();
        for part in data.chunks(97) {
            emu.feed(part, &mut out);
        }
    }
}

/// Regression found by `incremental_deltas_equal_keyframe` (PROPTEST_CASES=4000).
#[test]
fn regression_wide_chars_scroll_region_after_resize() {
    let steps = [
        Step::Feed("漢字".as_bytes().to_vec()),
        Step::Resize(17, 2),
        Step::Feed(b"aa aaa aaaaa\x1b[38;5;0m".to_vec()),
        Step::Feed("👍\x1b[5;10r".as_bytes().to_vec()),
        Step::Feed(b"\r\n".to_vec()),
        Step::Feed("漢字".as_bytes().to_vec()),
    ];
    run_steps(&steps);
}

fn run_steps(steps: &[Step]) {
    let mut emu = AlacrittyEmulator::new(TermSize::new(40, 12), 50);
    let mut tracker = FrameTracker::new(&mut emu);
    let mut styles = StyleTable::default();
    let mut client = ScreenCache::new(emu.size());
    client.apply(true, tracker.seq(), &tracker.build(&emu, None, &mut styles));
    for s in steps {
        match s {
            Step::Feed(bytes) => {
                let mut out = FeedOutput::default();
                emu.feed(bytes, &mut out);
            }
            Step::Resize(c, r) => emu.resize(TermSize::new(*c, *r)),
        }
        let since = client.seq;
        if tracker.update(&mut emu) {
            let d = tracker.build(&emu, Some(since), &mut styles);
            client.apply(false, tracker.seq(), &d);
        }
        let key = keyframe(&emu, &mut tracker);
        assert_eq!(client.plain_text(), key.plain_text(), "after {s:?}");
        assert_eq!(view(&client), view(&key), "after {s:?}");
    }
}
