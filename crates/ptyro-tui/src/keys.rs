//! crossterm events → ptyro's abstract input (SPEC §5.5). The daemon does the byte encoding.

use crossterm::event::{
    KeyCode as CK, KeyEvent as CKey, KeyEventKind, KeyEventState, KeyModifiers, MouseButton as CB,
    MouseEvent as CMouse, MouseEventKind as CM,
};
use ptyro_core::input::{KeyCode, KeyEvent, KeyKind, Mods, MouseButton, MouseEvent, MouseKind};

pub fn key(k: &CKey) -> Option<KeyEvent> {
    let code = match k.code {
        CK::Char(c) => KeyCode::Char { c },
        CK::Enter => KeyCode::Enter,
        CK::Tab => KeyCode::Tab,
        CK::BackTab => KeyCode::BackTab,
        CK::Backspace => KeyCode::Backspace,
        CK::Esc => KeyCode::Esc,
        CK::Up => KeyCode::Up,
        CK::Down => KeyCode::Down,
        CK::Left => KeyCode::Left,
        CK::Right => KeyCode::Right,
        CK::Home => KeyCode::Home,
        CK::End => KeyCode::End,
        CK::PageUp => KeyCode::PageUp,
        CK::PageDown => KeyCode::PageDown,
        CK::Insert => KeyCode::Insert,
        CK::Delete => KeyCode::Delete,
        CK::F(n) => KeyCode::F { n },
        CK::KeypadBegin => KeyCode::KeypadBegin,
        CK::CapsLock => KeyCode::CapsLock,
        CK::ScrollLock => KeyCode::ScrollLock,
        CK::NumLock => KeyCode::NumLock,
        CK::PrintScreen => KeyCode::PrintScreen,
        CK::Pause => KeyCode::Pause,
        CK::Menu => KeyCode::Menu,
        CK::Null => KeyCode::Char { c: ' ' },
        _ => return None,
    };
    let mut mods = mods(k.modifiers);
    // crossterm reports NUL as Ctrl-Space's `Null`; keep it Ctrl-Space.
    if k.code == CK::Null {
        mods.0 |= Mods::CTRL;
    }
    let kind = match k.kind {
        KeyEventKind::Press => KeyKind::Press,
        KeyEventKind::Repeat => KeyKind::Repeat,
        KeyEventKind::Release => KeyKind::Release,
    };
    Some(KeyEvent { code, mods, kind, keypad: k.state.contains(KeyEventState::KEYPAD) })
}

pub fn mods(m: KeyModifiers) -> Mods {
    let mut out = 0;
    for (cm, bit) in [
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::ALT, Mods::ALT),
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::SUPER, Mods::SUPER),
        (KeyModifiers::HYPER, Mods::HYPER),
        (KeyModifiers::META, Mods::META),
    ] {
        if m.contains(cm) {
            out |= bit;
        }
    }
    Mods(out)
}

pub fn mouse(m: &CMouse) -> MouseEvent {
    let button = |b: CB| match b {
        CB::Left => MouseButton::Left,
        CB::Middle => MouseButton::Middle,
        CB::Right => MouseButton::Right,
    };
    let kind = match m.kind {
        CM::Down(b) => MouseKind::Down { button: button(b) },
        CM::Up(b) => MouseKind::Up { button: button(b) },
        CM::Drag(b) => MouseKind::Drag { button: button(b) },
        CM::Moved => MouseKind::Moved,
        CM::ScrollUp => MouseKind::ScrollUp,
        CM::ScrollDown => MouseKind::ScrollDown,
        CM::ScrollLeft => MouseKind::ScrollLeft,
        CM::ScrollRight => MouseKind::ScrollRight,
    };
    MouseEvent { kind, x: m.column, y: m.row, mods: mods(m.modifiers) }
}
