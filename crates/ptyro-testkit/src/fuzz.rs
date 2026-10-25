//! Fuzz targets (M8.4), shared by `cargo fuzz` (the `fuzz/` crate, nightly) and the stable
//! mutation runner `ptyro-fuzz` (`make fuzz-smoke`, CI). Each takes arbitrary bytes and must
//! never panic; some also check an invariant.

use ptyro_core::TermSize;
use ptyro_core::input::{InputEvent, KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use ptyro_term::modes::{MouseProtocol, TermModes};
use ptyro_term::{AlacrittyEmulator, Emulator, FeedOutput};

/// A fuzz target: must not panic on any input.
pub type Target = fn(&[u8]);

pub const TARGETS: &[(&str, Target)] = &[
    ("sniff", sniff),
    ("emulator", emulator),
    ("protocol", protocol),
    ("input", input),
    ("recipe", recipe),
    ("record", record),
    ("settings", settings),
    ("imports", imports),
    ("hostjson", hostjson),
    ("handoff", handoff),
    ("parsers", parsers),
];

pub fn target(name: &str) -> Option<fn(&[u8])> {
    TARGETS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// The side-channel scanner: no panics, and the same sequences however the input is split.
pub fn sniff(data: &[u8]) {
    let mut whole = ptyro_term::sniff::Sniffer::default();
    let mut a = Vec::new();
    whole.scan(data, &mut a);
    let split = data.first().map_or(1, |b| usize::from(*b % 17) + 1);
    let mut parts = ptyro_term::sniff::Sniffer::default();
    let mut b = Vec::new();
    for chunk in data.chunks(split) {
        parts.scan(chunk, &mut b);
    }
    assert_eq!(a, b, "the scanner depends on how reads are split");
}

/// The emulator on arbitrary output, with resizes and an image round-trip in between.
pub fn emulator(data: &[u8]) {
    let cols = 2 + u16::from(data.first().copied().unwrap_or(10) % 120);
    let rows = 1 + u16::from(data.get(1).copied().unwrap_or(5) % 50);
    let mut e = AlacrittyEmulator::new(TermSize::new(cols, rows), 200);
    let mut out = FeedOutput::default();
    let mut row = Vec::new();
    for (i, chunk) in data.chunks(97).enumerate() {
        e.feed(chunk, &mut out);
        if i % 5 == 4 {
            let s = TermSize::new(
                2 + u16::from(chunk[0] % 90),
                1 + u16::from(chunk[chunk.len() - 1] % 40),
            );
            e.resize(s);
        }
        if i % 7 == 6 {
            e = AlacrittyEmulator::from_image(e.image());
        }
        for y in -(e.scrollback_len() as i32).min(3)..i32::from(e.size().rows) {
            e.line(y, &mut row);
        }
        let _ = e.cursor();
        let _ = e.take_damage();
    }
}

/// The protocol decoder: arbitrary frames, compressed or not, never panic or over-allocate.
pub fn protocol(data: &[u8]) {
    let _ = ptyro_protocol::decode::<ptyro_protocol::ClientMsg>(data);
    let _ = ptyro_protocol::decode::<ptyro_protocol::ServerMsg>(data);
    let mut z = ptyro_protocol::ZSTD_MAGIC.to_vec();
    z.extend_from_slice(data);
    let _ = ptyro_protocol::decode::<ptyro_protocol::ServerMsg>(&z);
}

fn modes(b: u8, c: u8) -> TermModes {
    TermModes {
        app_cursor: b & 1 != 0,
        app_keypad: b & 2 != 0,
        bracketed_paste: b & 4 != 0,
        focus_events: b & 8 != 0,
        alt_screen: b & 16 != 0,
        show_cursor: b & 32 != 0,
        mouse: match c % 4 {
            0 => MouseProtocol::Off,
            1 => MouseProtocol::Click,
            2 => MouseProtocol::Drag,
            _ => MouseProtocol::Motion,
        },
        sgr_mouse: b & 64 != 0,
        kitty: c >> 3,
    }
}

/// The input encoder: every key, mouse event and paste, in every mode.
pub fn input(data: &[u8]) {
    for w in data.chunks(4) {
        let [k, m, a, b] =
            [w[0], *w.get(1).unwrap_or(&0), *w.get(2).unwrap_or(&0), *w.get(3).unwrap_or(&0)];
        let modes = modes(a, b);
        let code = match k % 24 {
            0 => KeyCode::Char {
                c: char::from_u32(u32::from(m) | (u32::from(a) << 8)).unwrap_or('x'),
            },
            1 => KeyCode::Enter,
            2 => KeyCode::Tab,
            3 => KeyCode::BackTab,
            4 => KeyCode::Backspace,
            5 => KeyCode::Esc,
            6 => KeyCode::Up,
            7 => KeyCode::Down,
            8 => KeyCode::Left,
            9 => KeyCode::Right,
            10 => KeyCode::Home,
            11 => KeyCode::End,
            12 => KeyCode::PageUp,
            13 => KeyCode::PageDown,
            14 => KeyCode::Insert,
            15 => KeyCode::Delete,
            16 => KeyCode::F { n: m % 30 },
            17 => KeyCode::KeypadBegin,
            18 => KeyCode::CapsLock,
            19 => KeyCode::Menu,
            20 => KeyCode::Pause,
            21 => KeyCode::PrintScreen,
            22 => KeyCode::NumLock,
            _ => KeyCode::ScrollLock,
        };
        let key = KeyEvent::new(code, Mods(m));
        let _ = ptyro_term::input::encode(&InputEvent::Key { key }, &modes);
        let button =
            [MouseButton::Left, MouseButton::Middle, MouseButton::Right][usize::from(m % 3)];
        let kind = match k % 8 {
            0 => MouseKind::Down { button },
            1 => MouseKind::Up { button },
            2 => MouseKind::Drag { button },
            3 => MouseKind::Moved,
            4 => MouseKind::ScrollUp,
            5 => MouseKind::ScrollDown,
            6 => MouseKind::ScrollLeft,
            _ => MouseKind::ScrollRight,
        };
        let mouse = MouseEvent { kind, x: u16::from(a) * 40, y: u16::from(b) * 40, mods: Mods(m) };
        let _ = ptyro_term::input::encode(&InputEvent::Mouse { mouse }, &modes);
    }
    let text = String::from_utf8_lossy(data);
    let pasted = ptyro_term::input::encode_paste(&text, &modes(4, 0));
    // A paste can never end the bracket early (SPEC §5.5).
    let inner = &pasted[6..pasted.len().saturating_sub(6)];
    assert!(!inner.windows(6).any(|w| w == b"\x1b[201~"), "paste injection");
}

/// Recipes, templates and conditions (the run engine's untrusted input).
pub fn recipe(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    if let Ok(r) = ptyro_run::Recipe::parse(&text) {
        let _ = r.graph("mermaid");
        let _ = r.bind(&std::collections::BTreeMap::new());
    }
    if let Ok(e) = ptyro_run::expr::parse(&text) {
        let _ = e.eval(&serde_json::json!({ "steps": { "a": { "status": "succeeded" } } }));
    }
    let _ = ptyro_run::template::references(&text);
    let _ = ptyro_run::template::render(&text, &serde_json::json!({}));
}

/// Recording files (read back after crashes, so maybe torn).
pub fn record(data: &[u8]) {
    let dir = std::env::temp_dir().join(format!("ptyro-fuzz-rec-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut file = ptyro_term::record::MAGIC.to_vec();
    file.extend_from_slice(data);
    if std::fs::write(dir.join("pty.log"), &file).is_ok()
        && let Ok(recs) = ptyro_term::record::read_dir(&dir)
    {
        let _ = ptyro_term::record::asciicast(&recs, "fuzz");
        let mut e = AlacrittyEmulator::new(TermSize::new(40, 10), 100);
        // Cap the replayed size: a fuzzed resize record may ask for anything.
        let recs: Vec<_> = recs
            .into_iter()
            .map(|mut r| {
                if let ptyro_term::record::Record::Resize(s)
                | ptyro_term::record::Record::Start(s) = &mut r.record
                {
                    *s = TermSize::new(s.cols.clamp(2, 300), s.rows.clamp(1, 200));
                }
                r
            })
            .collect();
        ptyro_term::record::replay(&recs, &mut e);
    }
}

/// The settings editor on arbitrary files, keys and typed input: no panics, and whatever it
/// writes is a file that still checks (it never saves half of an edit).
pub fn settings(data: &[u8]) {
    use ptyro_core::settings::{self, Lookups, Scope};
    let text = String::from_utf8_lossy(data);
    let mut parts = text.splitn(5, '\n');
    let head = data.first().copied().unwrap_or(0);
    let scope = if head & 1 == 0 { Scope::User } else { Scope::Project };
    let (file, key, value, extra) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    // Half the time edit a real setting's key, so the typed parsers are reached.
    let real;
    let key = if head & 2 == 0 && !settings::SETTINGS.is_empty() {
        real = settings::SETTINGS[usize::from(head >> 2) % settings::SETTINGS.len()]
            .key
            .replace('*', if key.is_empty() { "x" } else { key });
        real.as_str()
    } else {
        key
    };
    let lk =
        Lookups { themes: vec!["dark".into()], actions: vec![("a".into(), "an action".into())] };
    let ok = |_: &str| Ok(());
    let file = if head & 4 == 0 { file.to_owned() } else { settings::starter() };
    let verify = |r: Result<String, String>| {
        if let Ok(new) = r {
            assert!(settings::check(scope, &new).is_ok(), "an edit saved an invalid file");
        }
    };
    verify(settings::set(scope, &file, key, value, &lk, &ok));
    verify(settings::unset(scope, &file, key, &ok));
    verify(settings::add(
        scope,
        &file,
        key,
        value,
        &[(extra, value), ("command", extra)],
        &lk,
        &ok,
    ));
    verify(settings::remove(scope, &file, key, value, &ok));
    verify(settings::rename(scope, &file, key, value, extra, &ok));
    let _ = settings::check(scope, &file);
    if let Some(st) = settings::find(key) {
        let _ = settings::parse_input(st, value, &lk);
    }
    let _ = settings::split(key);
}

/// The importers of other tools' files (a checkout's own, so untrusted): no panics.
pub fn imports(data: &[u8]) {
    use ptyro_core::imports;
    let text = String::from_utf8_lossy(data);
    let _ = imports::conductor(&text);
    let _ = imports::conductor_json(&text);
    let _ = imports::superset(std::path::Path::new("/tmp/checkout"), &text);
    let _ = imports::codex(&text);
    let _ = imports::claude_launch(&text);
    let _ = imports::cursor(&text);
    let _ = imports::copilot(&text);
    let _ = imports::strip_json_comments(&text);
    let w = imports::sh_word(&text);
    assert!(!w.contains('\0') || text.contains('\0'), "a NUL from nowhere");
}

/// The host API's pure layers: requests and replies as JSON text (a browser sends these), and
/// the setting-edit commands the settings panel sends. Never panic; whatever decodes re-encodes.
pub fn hostjson(data: &[u8]) {
    use ptyro_protocol::{ClientMsg, Reply, Request, ServerMsg};
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) {
        if let Ok(r) = serde_json::from_value::<Request>(v.clone()) {
            let again = serde_json::to_value(&r).expect("a request encodes");
            assert!(serde_json::from_value::<Request>(again).is_ok(), "re-decode");
        }
        let _ = serde_json::from_value::<Reply>(v.clone());
        let _ = serde_json::from_value::<ClientMsg>(v.clone());
        let _ = serde_json::from_value::<ServerMsg>(v.clone());
        // The settings panel's op: {"op": "...", "key": "...", "value": "..."} against a file.
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
        let _ = ptyro_core::settings::set(
            ptyro_core::settings::Scope::User,
            "",
            s("key"),
            s("value"),
            &ptyro_core::settings::Lookups::default(),
            &|_: &str| Ok(()),
        );
        let _ = ptyro_core::names::validate(s("name"));
    }
}

/// The handoff image a new daemon reads from the old one: arbitrary bytes as the whole image,
/// and as one session's compressed emulator (valid ones are mutated from a real screen).
pub fn handoff(data: &[u8]) {
    let _ = ptyro_daemon::decode_handoff_image(data);
    let _ = ptyro_daemon::decode_handoff_emulator(data);
}

/// Configuration values and files, and the small text formats around them.
pub fn parsers(data: &[u8]) {
    use ptyro_core::{config, names, project::ProjectConfig, scratchpad};
    let text = String::from_utf8_lossy(data);
    let _ = config::Config::parse(&text);
    let _ = ProjectConfig::parse(&text);
    let _ = config::parse_duration(&text);
    let _ = config::parse_bytes(&text);
    let _ = config::parse_quiet_hours(&text);
    let _ = config::parse_color(&text);
    let _ = config::parse_key(&text);
    let _ = config::expand_home(&text);
    let _ = config::dangerous_arg(&[text.to_string()]);
    let _ = names::validate(&text);
    let _ = names::problem(&text);
    let _ = names::tidy(&text);
    let _ = names::from_text(&text);
    let (a, rest) = text.split_once('\u{1}').unwrap_or((&text, ""));
    let (b, c) = rest.split_once('\u{1}').unwrap_or((rest, ""));
    let merged = scratchpad::merge3(a, b, c);
    let _ = scratchpad::rev(&merged.text);
    let _ = scratchpad::insert_under(a, b, c);
    let line = u32::from(data.first().copied().unwrap_or(1) % 12);
    for item in scratchpad::items(a) {
        let _ = scratchpad::body(a, &item);
    }
    let _ = scratchpad::select(a, b);
    let _ = scratchpad::tick(a, line, Some(1));
    let _ = scratchpad::untick(a, line);
    let _ = scratchpad::tag_worktree(a, line, b);
}

/// Seeds for the mutation runner: real escape sequences, protocol frames, recipes.
pub fn seeds(target: &str) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = vec![Vec::new(), b"hello\r\n".to_vec()];
    match target {
        "sniff" | "emulator" => {
            for s in [
                "\x1b]7501;state=working:msg=aGk\x1b\\",
                "\x1b]7501;?\x07\x1b[c",
                "\x1b]0;title\x07\x1b[?1049h\x1b[2J\x1b[H",
                "\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\x1b]133;D;0\x07",
                "\x1b]9;4;1;50\x07\x1b]777;notify;a;b\x07",
                "\x1bP+q544e\x1b\\\x1b[>q\x1b[16t",
                "\x1b[38;2;1;2;3m\x1b[48;5;200mX\x1b[0m\x1b[?2026h\x1b[?2026l",
                "\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[<u",
                "漢字\u{0301}e\u{0301}\x1b]8;;http://x\x07link\x1b]8;;\x07",
                "\x1b]52;c;aGVsbG8=\x07\x1bc",
            ] {
                v.push(s.as_bytes().to_vec());
            }
        }
        "protocol" => {
            use ptyro_protocol::{ClientMsg, Encoding, Request, encode};
            for m in [
                ClientMsg::Ping,
                ClientMsg::Request { id: 1, req: Request::List },
                ClientMsg::Request {
                    id: 2,
                    req: Request::RunList { active: true, limit: Some(5) },
                },
            ] {
                v.push(encode(&m, Encoding::MsgPack).map(|b| b.to_vec()).unwrap_or_default());
                v.push(encode(&m, Encoding::Json).map(|b| b.to_vec()).unwrap_or_default());
            }
        }
        "recipe" => {
            for (_, src) in ptyro_run::builtin::ALL {
                v.push(src.as_bytes().to_vec());
            }
            v.push(b"steps.a.status == 'succeeded' && !(x.y < 3) || 'z' contains 'q'".to_vec());
        }
        "record" => {
            let mut r = vec![
                3u8, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 80, 0, 24, 0, 0, 0, 0, 0, 0, 0, 1,
            ];
            r.extend_from_slice(&[0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 9]);
            r.extend_from_slice(b"hello");
            v.push(r);
        }
        "settings" => {
            for s in [
                "\n\n\n\n",
                "[ui]\ntheme = \"dark\"\n\nui.theme\ndark\n",
                "# c\n[agents.x]\ncommand = \"x\"\n\nagents\nx\ncommand\ny",
                "[[hooks]]\n\n\u{0}\u{3}\n1h\n",
            ] {
                v.push(s.as_bytes().to_vec());
            }
        }
        "imports" => {
            for s in [
                "[scripts]\nsetup = \"npm i\"\n",
                "{\"scripts\": {\"setup\": [\"npm i\"]}}",
                "{\"setup\": [\"a\"], // c\n\"run\": \"b\"}",
                "version = 1\n[[actions]]\nname = \"x\"\ncommand = \"y\"\n",
                "{\"configurations\": [{\"name\": \"a\", \"runtimeExecutable\": \"x\", \"runtimeArgs\": [\"1\"]}]}",
                "{\"install\": \"npm i\", \"start\": \"npm run dev\"}",
                "jobs:\n  copilot-setup-steps:\n    steps:\n      - run: make\n",
            ] {
                v.push(s.as_bytes().to_vec());
            }
        }
        "hostjson" => {
            for s in [
                r#"{"op":"list"}"#,
                r#"{"key":"ui.theme","value":"dark","name":"a"}"#,
                r#"{"type":"Request","id":1,"req":"List"}"#,
                r#"{"Request":{"id":2,"req":{"RunList":{"active":true,"limit":5}}}}"#,
            ] {
                v.push(s.as_bytes().to_vec());
            }
        }
        "handoff" => {
            let mut e = AlacrittyEmulator::new(TermSize::new(40, 10), 100);
            e.feed(b"hello\r\n\x1b[31mred\x1b[0m", &mut FeedOutput::default());
            if let Ok(raw) = rmp_serde::to_vec(&e.image())
                && let Ok(z) = zstd::bulk::compress(&raw, 1)
            {
                v.push(z);
                v.push(raw);
            }
        }
        "parsers" => {
            for s in [
                "[daemon]\nlog = \"info\"\n",
                "10s",
                "5 GiB",
                "22:00-07:00",
                "#ff8800",
                "C-q c",
                "- [ ] a\n- [x] b\n\u{1}- [ ] a\n\u{1}- [ ] c\n",
            ] {
                v.push(s.as_bytes().to_vec());
            }
        }
        _ => v.push(vec![0, 1, 2, 3, 255, 254, 27, 91]),
    }
    v
}

/// A tiny, fast, seeded RNG for the mutation runner (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }
}

/// Mutate `input` (bit flips, byte sets, inserts of interesting bytes, splices, truncation).
pub fn mutate(rng: &mut Rng, input: &[u8], corpus: &[Vec<u8>]) -> Vec<u8> {
    const INTERESTING: &[&[u8]] = &[
        b"\x1b",
        b"\x1b[",
        b"\x1b]",
        b"\x07",
        b"\x1b\\",
        b";",
        b"\xff",
        b"\x00",
        b"9999999999",
        b"\x1bP",
        b"\x9b",
        b"\xc3",
        b"{",
        b"}",
        b"\"",
    ];
    let mut v = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(6) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = rng.next_u64() as u8;
            }
            2 => {
                let i = rng.below(v.len() + 1);
                let ins = INTERESTING[rng.below(INTERESTING.len())];
                v.splice(i..i, ins.iter().copied());
            }
            3 if !corpus.is_empty() => {
                let other = &corpus[rng.below(corpus.len())];
                if !other.is_empty() {
                    let a = rng.below(other.len());
                    let b = a + rng.below(other.len() - a + 1);
                    let i = rng.below(v.len() + 1);
                    v.splice(i..i, other[a..b].iter().copied());
                }
            }
            4 if !v.is_empty() => v.truncate(rng.below(v.len())),
            _ => {
                let n = rng.below(16);
                for _ in 0..n {
                    v.push(rng.next_u64() as u8);
                }
            }
        }
    }
    v.truncate(64 * 1024);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every target takes every one of its seeds, and its seeds cut short, without panicking.
    #[test]
    fn every_target_survives_its_seeds() {
        for (name, f) in TARGETS {
            for seed in seeds(name) {
                f(&seed);
                f(&seed[..seed.len() / 2]);
            }
        }
    }
}
