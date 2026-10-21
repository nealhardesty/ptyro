//! Encode abstract input for the child, against the child's current modes (SPEC §5.5).

use ptyro_core::input::{
    InputEvent, KeyCode, KeyEvent, KeyKind, Mods, MouseButton, MouseEvent, MouseKind,
};
use terminput::{Encoding, KittyFlags};

use crate::modes::{MouseProtocol, TermModes, kitty};

pub fn encode(event: &InputEvent, modes: &TermModes) -> Vec<u8> {
    match event {
        InputEvent::Key { key } => encode_key(key, modes),
        InputEvent::Mouse { mouse } => encode_mouse(mouse, modes),
        InputEvent::Paste { text } => encode_paste(text, modes),
        InputEvent::Focus { gained } => match (modes.focus_events, gained) {
            (false, _) => vec![],
            (true, true) => b"\x1b[I".to_vec(),
            (true, false) => b"\x1b[O".to_vec(),
        },
        InputEvent::Raw { bytes } => bytes.clone(),
    }
}

/// terminput's `KittyFlags` are numbered one bit higher than the kitty spec, so map by name.
fn kitty_flags(k: u8) -> KittyFlags {
    let mut f = KittyFlags::empty();
    for (bit, flag) in [
        (kitty::DISAMBIGUATE, KittyFlags::DISAMBIGUATE_ESCAPE_CODES),
        (kitty::REPORT_EVENT_TYPES, KittyFlags::REPORT_EVENT_TYPES),
        (kitty::REPORT_ALTERNATE_KEYS, KittyFlags::REPORT_ALTERNATE_KEYS),
        (kitty::REPORT_ALL_KEYS, KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
    ] {
        if k & bit != 0 {
            f |= flag;
        }
    }
    f
}

fn to_terminput_mods(m: Mods) -> terminput::KeyModifiers {
    use terminput::KeyModifiers as K;
    let mut out = K::NONE;
    for (bit, k) in [
        (Mods::SHIFT, K::SHIFT),
        (Mods::ALT, K::ALT),
        (Mods::CTRL, K::CTRL),
        (Mods::SUPER, K::SUPER),
        (Mods::HYPER, K::HYPER),
        (Mods::META, K::META),
    ] {
        if m.has(bit) {
            out |= k;
        }
    }
    out
}

fn to_terminput_code(code: KeyCode) -> terminput::KeyCode {
    use terminput::KeyCode as T;
    match code {
        KeyCode::Char { c } => T::Char(c),
        KeyCode::Enter => T::Enter,
        KeyCode::Tab | KeyCode::BackTab => T::Tab,
        KeyCode::Backspace => T::Backspace,
        KeyCode::Esc => T::Esc,
        KeyCode::Up => T::Up,
        KeyCode::Down => T::Down,
        KeyCode::Left => T::Left,
        KeyCode::Right => T::Right,
        KeyCode::Home => T::Home,
        KeyCode::End => T::End,
        KeyCode::PageUp => T::PageUp,
        KeyCode::PageDown => T::PageDown,
        KeyCode::Insert => T::Insert,
        KeyCode::Delete => T::Delete,
        KeyCode::F { n } => T::F(n),
        KeyCode::KeypadBegin => T::KeypadBegin,
        KeyCode::CapsLock => T::CapsLock,
        KeyCode::ScrollLock => T::ScrollLock,
        KeyCode::NumLock => T::NumLock,
        KeyCode::PrintScreen => T::PrintScreen,
        KeyCode::Pause => T::Pause,
        KeyCode::Menu => T::Menu,
    }
}

pub fn encode_key(key: &KeyEvent, modes: &TermModes) -> Vec<u8> {
    let wants_events = modes.kitty & kitty::REPORT_EVENT_TYPES != 0;
    let kind = match key.kind {
        KeyKind::Release if !wants_events => return vec![],
        KeyKind::Repeat if !wants_events => terminput::KeyEventKind::Press,
        KeyKind::Press => terminput::KeyEventKind::Press,
        KeyKind::Repeat => terminput::KeyEventKind::Repeat,
        KeyKind::Release => terminput::KeyEventKind::Release,
    };
    let mut mods = key.mods;
    if key.code == KeyCode::BackTab {
        mods.0 |= Mods::SHIFT;
    }
    let encoding = match modes.kitty {
        0 => Encoding::Xterm,
        k => Encoding::Kitty(kitty_flags(k)),
    };
    // Legacy Ctrl encodings exist only for ASCII; terminput truncates the char to a byte and can
    // overflow. Send the character itself, as xterm does. (Kitty without these flags is legacy.)
    let legacy = modes.kitty & (kitty::DISAMBIGUATE | kitty::REPORT_ALL_KEYS) == 0;
    if legacy && matches!(key.code, KeyCode::Char { c } if !c.is_ascii()) {
        mods.0 &= !Mods::CTRL;
    }
    let mut state = terminput::KeyEventState::NONE;
    if key.keypad {
        state |= terminput::KeyEventState::KEYPAD;
    }
    let ev = terminput::KeyEvent::new(to_terminput_code(key.code))
        .modifiers(to_terminput_mods(mods))
        .kind(kind)
        .state(state);
    let mut buf = [0u8; 64];
    let Ok(n) = terminput::Event::Key(ev).encode(&mut buf, encoding) else { return vec![] };
    let mut out = buf[..n].to_vec();
    // terminput's xterm encoder ignores DECCKM: in application-cursor mode, unmodified
    // arrows/Home/End are SS3 (`ESC O A`), not CSI (`ESC [ A`). vim, less and htop rely on it.
    let cursor_key = matches!(
        key.code,
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
    );
    if encoding == Encoding::Xterm
        && modes.app_cursor
        && cursor_key
        && mods.is_empty()
        && out.len() == 3
        && out[1] == b'['
    {
        out[1] = b'O';
    }
    out
}

pub fn encode_paste(text: &str, modes: &TermModes) -> Vec<u8> {
    if modes.bracketed_paste {
        // Paste-injection defence (SPEC §17): no escape in the payload at all, so nothing in it
        // can close the bracket ("\x1b[20\x1b[201~1~" would survive a one-pass removal).
        let clean: String = text.chars().filter(|c| !matches!(c, '\x1b' | '\u{9b}')).collect();
        [b"\x1b[200~".as_slice(), clean.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

pub fn encode_mouse(ev: &MouseEvent, modes: &TermModes) -> Vec<u8> {
    let wanted = !matches!(
        (modes.mouse, ev.kind),
        (MouseProtocol::Off, _)
            | (MouseProtocol::Click, MouseKind::Drag { .. } | MouseKind::Moved)
            | (MouseProtocol::Drag, MouseKind::Moved)
    );
    if !wanted {
        return vec![];
    }
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match ev.kind {
        MouseKind::Down { button: b } => (button(b), false),
        MouseKind::Up { button: b } => (button(b), true),
        MouseKind::Drag { button: b } => (button(b) + 32, false),
        MouseKind::Moved => (35, false),
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
        MouseKind::ScrollLeft => (66, false),
        MouseKind::ScrollRight => (67, false),
    };
    if ev.mods.has(Mods::SHIFT) {
        code += 4;
    }
    if ev.mods.has(Mods::ALT) || ev.mods.has(Mods::META) {
        code += 8;
    }
    if ev.mods.has(Mods::CTRL) {
        code += 16;
    }
    let (x, y) = (ev.x + 1, ev.y + 1);
    if modes.sgr_mouse {
        let end = if release { 'm' } else { 'M' };
        return format!("\x1b[<{code};{x};{y}{end}").into_bytes();
    }
    // Legacy X10/normal encoding: release has no button; coordinates are limited to 223.
    if release {
        code = 3 + (code & !3);
    }
    if x > 223 || y > 223 {
        return vec![];
    }
    vec![0x1b, b'[', b'M', (32 + code) as u8, (32 + x) as u8, (32 + y) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: u8) -> InputEvent {
        InputEvent::Key { key: KeyEvent::new(code, Mods(mods)) }
    }

    #[test]
    fn decckm_arrows() {
        let up = key(KeyCode::Up, 0);
        assert_eq!(encode(&up, &TermModes::default()), b"\x1b[A");
        let app = TermModes { app_cursor: true, ..Default::default() };
        assert_eq!(encode(&up, &app), b"\x1bOA");
        assert_eq!(encode(&key(KeyCode::Up, Mods::SHIFT), &app), b"\x1b[1;2A");
    }

    #[test]
    fn shift_enter_needs_kitty() {
        let se = key(KeyCode::Enter, Mods::SHIFT);
        assert_eq!(encode(&se, &TermModes::default()), b"\r");
        let k = TermModes { kitty: kitty::DISAMBIGUATE, ..Default::default() };
        assert_eq!(encode(&se, &k), b"\x1b[13;2u");
    }

    #[test]
    fn releases_only_when_requested() {
        let rel =
            InputEvent::Key { key: KeyEvent { kind: KeyKind::Release, ..KeyEvent::char('a') } };
        assert!(encode(&rel, &TermModes::default()).is_empty());
        let k = TermModes {
            kitty: kitty::DISAMBIGUATE | kitty::REPORT_EVENT_TYPES,
            ..Default::default()
        };
        assert!(!encode(&rel, &k).is_empty());
    }

    #[test]
    fn ctrl_c_and_backtab() {
        assert_eq!(encode(&key(KeyCode::Char { c: 'c' }, Mods::CTRL), &TermModes::default()), [3]);
        assert_eq!(encode(&key(KeyCode::BackTab, 0), &TermModes::default()), b"\x1b[Z");
    }

    #[test]
    fn ctrl_with_non_ascii_sends_the_char() {
        // Found by the overflow-checked fuzz profile: terminput adds 1 to `c as u8` (0xff here).
        for c in ['\u{c9ff}', '\u{ff}', 'é'] {
            let mods = Mods::CTRL | Mods::ALT | Mods::SHIFT;
            let out = encode(&key(KeyCode::Char { c }, mods), &TermModes::default());
            assert!(String::from_utf8_lossy(&out).ends_with(c), "{c:?}: {out:?}");
            let events_only = TermModes { kitty: kitty::REPORT_EVENT_TYPES, ..Default::default() };
            encode(&key(KeyCode::Char { c }, mods), &events_only);
        }
    }

    #[test]
    fn paste_and_focus() {
        let bp = TermModes { bracketed_paste: true, focus_events: true, ..Default::default() };
        let p = InputEvent::Paste { text: "a\x1b[201~b".into() };
        assert_eq!(encode(&p, &bp), b"\x1b[200~a[201~b\x1b[201~");
        // Nested and C1 forms can't close the bracket either: no ESC or CSI survives.
        let p = InputEvent::Paste { text: "x\x1b[20\x1b[201~1~\u{9b}201~y".into() };
        let out = encode(&p, &bp);
        let body = &out[6..out.len() - 6];
        assert!(!body.contains(&0x1b) && !String::from_utf8_lossy(body).contains('\u{9b}'));
        assert_eq!(
            encode(&InputEvent::Paste { text: "a\nb".into() }, &TermModes::default()),
            b"a\rb"
        );
        assert_eq!(encode(&InputEvent::Focus { gained: false }, &bp), b"\x1b[O");
        assert!(encode(&InputEvent::Focus { gained: true }, &TermModes::default()).is_empty());
    }

    #[test]
    fn mouse_encodings() {
        let down = MouseEvent {
            kind: MouseKind::Down { button: MouseButton::Left },
            x: 4,
            y: 2,
            mods: Mods::NONE,
        };
        let up = MouseEvent { kind: MouseKind::Up { button: MouseButton::Left }, ..down };
        let moved = MouseEvent { kind: MouseKind::Moved, ..down };
        let sgr = TermModes { mouse: MouseProtocol::Click, sgr_mouse: true, ..Default::default() };
        assert_eq!(encode_mouse(&down, &sgr), b"\x1b[<0;5;3M");
        assert_eq!(encode_mouse(&up, &sgr), b"\x1b[<0;5;3m");
        assert!(encode_mouse(&moved, &sgr).is_empty());
        let x10 = TermModes { mouse: MouseProtocol::Motion, ..Default::default() };
        assert_eq!(encode_mouse(&down, &x10), [0x1b, b'[', b'M', 32, 37, 35]);
        assert_eq!(encode_mouse(&up, &x10), [0x1b, b'[', b'M', 35, 37, 35]);
        assert!(encode_mouse(&down, &TermModes::default()).is_empty());
    }
}
