//! Handoff images (M8.1): an emulator rebuilt from its image looks and behaves the same, even
//! for output that arrives afterwards (leaving the alternate screen, keyboard-protocol pops).

use ptyro_core::TermSize;
use ptyro_term::emulator::{CellData, EmuImage};
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput};

fn feed(e: &mut AlacrittyEmulator, s: &str) -> FeedOutput {
    let mut out = FeedOutput::default();
    e.feed(s.as_bytes(), &mut out);
    out
}

/// Every line, history included, with styles.
fn dump(e: &AlacrittyEmulator) -> Vec<(Vec<CellData>, bool)> {
    let mut v = Vec::new();
    let mut row = Vec::new();
    for l in -(e.scrollback_len() as i32)..i32::from(e.size().rows) {
        e.line(l, &mut row);
        v.push((row.clone(), e.line_wrapped(l)));
    }
    v
}

fn round_trip(e: &mut AlacrittyEmulator) -> AlacrittyEmulator {
    let img = e.image();
    let bytes = rmp_serde::to_vec(&img).unwrap();
    let back: EmuImage = rmp_serde::from_slice(&bytes).unwrap();
    AlacrittyEmulator::from_image(back)
}

fn text(e: &AlacrittyEmulator) -> Vec<String> {
    dump(e)
        .iter()
        .map(|(r, w)| {
            format!(
                "{}{}",
                r.iter().map(|c| c.c).collect::<String>().trim_end(),
                if *w { "⏎" } else { "" }
            )
        })
        .collect()
}

fn same(a: &AlacrittyEmulator, b: &AlacrittyEmulator) {
    assert_eq!(a.size(), b.size());
    assert_eq!(text(a), text(b));
    assert_eq!(a.scrollback_len(), b.scrollback_len());
    assert!(dump(a) == dump(b), "styles differ");
    assert_eq!(a.cursor(), b.cursor());
    assert_eq!(a.modes(), b.modes());
    assert_eq!(a.title(), b.title());
}

#[test]
fn a_shell_screen_with_history_survives() {
    let mut e = AlacrittyEmulator::new(TermSize::new(40, 6), 1000);
    for i in 0..30 {
        feed(&mut e, &format!("\x1b[3{}mline {i}\x1b[0m 漢字\r\n", i % 8));
    }
    feed(&mut e, "\x1b]0;my title\x07\x1b[?2004h\x1b[?1h\x1b[5 q$ partial");
    let mut r = round_trip(&mut e);
    same(&e, &r);
    // What comes next lands in the same place.
    for x in [&mut e, &mut r] {
        feed(x, " typed\r\nnext line\x1b[2;3Hzz");
    }
    same(&e, &r);
}

#[test]
fn the_alternate_screen_and_keyboard_protocol_survive() {
    let mut e = AlacrittyEmulator::new(TermSize::new(30, 8), 500);
    for i in 0..20 {
        feed(&mut e, &format!("shell {i}\r\n"));
    }
    // A full-screen program: alt screen, kitty flags pushed, mouse, cursor somewhere.
    feed(
        &mut e,
        "\x1b[?1049h\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[2J\x1b[H┌──┐\r\n│ui│\r\n└──┘\x1b[5;10H",
    );
    let mut r = round_trip(&mut e);
    same(&e, &r);
    // The program exits: pop the keyboard flags, leave the alt screen; the shell is back.
    for x in [&mut e, &mut r] {
        feed(x, "\x1b[<u\x1b[?1000l\x1b[?1006l\x1b[?1049l$ ");
    }
    same(&e, &r);
    let mut row = Vec::new();
    r.line(0, &mut row);
    let text: String = row.iter().map(|c| c.c).collect();
    assert!(text.starts_with("shell 13"), "{text:?}");
}

#[test]
fn query_replies_are_unchanged_after_restore() {
    let mut e = AlacrittyEmulator::new(TermSize::new(20, 4), 10);
    feed(&mut e, "hello");
    let mut r = round_trip(&mut e);
    let a = feed(&mut e, "\x1b[6n\x1b[c");
    let b = feed(&mut r, "\x1b[6n\x1b[c");
    assert_eq!(a.replies, b.replies, "cursor position and DA answer the same");
}
