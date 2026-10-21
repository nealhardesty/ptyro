//! Abstract input events (SPEC §5.5). Clients send these; the daemon encodes them into bytes
//! against the child's current terminal modes, which only the daemon knows.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum KeyCode {
    Char {
        c: char,
    },
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F {
        n: u8,
    },
    /// Keypad centre (`5` without Num Lock).
    KeypadBegin,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
}

/// Modifier bit set; plain `u8` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Self = Self(0);
    pub const SHIFT: u8 = 1;
    pub const ALT: u8 = 1 << 1;
    pub const CTRL: u8 = 1 << 2;
    pub const SUPER: u8 = 1 << 3;
    pub const HYPER: u8 = 1 << 4;
    pub const META: u8 = 1 << 5;

    pub fn has(self, m: u8) -> bool {
        self.0 & m != 0
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: KeyCode,
    #[serde(default)]
    pub mods: Mods,
    #[serde(default)]
    pub kind: KeyKind,
    /// The key came from the keypad (kitty protocol distinguishes these).
    #[serde(default)]
    pub keypad: bool,
}

impl KeyEvent {
    pub fn new(code: KeyCode, mods: Mods) -> Self {
        Self { code, mods, kind: KeyKind::Press, keypad: false }
    }
    pub fn char(c: char) -> Self {
        Self::new(KeyCode::Char { c }, Mods::NONE)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum MouseKind {
    Down { button: MouseButton },
    Up { button: MouseButton },
    Drag { button: MouseButton },
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// Cell coordinates within the session's screen, zero-based.
    pub x: u16,
    pub y: u16,
    #[serde(default)]
    pub mods: Mods,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum InputEvent {
    Key {
        key: KeyEvent,
    },
    Mouse {
        mouse: MouseEvent,
    },
    Paste {
        text: String,
    },
    Focus {
        gained: bool,
    },
    /// Escape hatch (`ptyro send --raw`, raw attach): bytes written as-is.
    Raw {
        bytes: Vec<u8>,
    },
}
