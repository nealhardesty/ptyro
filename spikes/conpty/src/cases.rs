//! What the spike plays through ConPTY, shared by the Windows side (which runs it) and the report
//! (which judges what came out). Everything here is plain data: no Windows.

/// One run: a child in a pseudoconsole of `cols`×`rows` writes `output`, the host types `input`.
#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    /// What it shows, for the report.
    pub what: &'static str,
    pub cols: u16,
    pub rows: u16,
    /// What the child writes to its console, once started.
    pub output: Vec<u8>,
    /// What the host types afterwards: (wait in ms before it, bytes).
    pub input: Vec<(u64, Vec<u8>)>,
    /// The child keeps ENABLE_PROCESSED_INPUT (Ctrl+C is a signal, not a byte).
    pub processed: bool,
    /// How long the child reads its input after writing (ms).
    pub read_ms: u32,
    /// Resizes the host makes after the output: (wait in ms, cols, rows).
    pub resizes: Vec<(u64, u16, u16)>,
    /// The host closes the pseudoconsole this long after the start, with the child still running.
    pub close_after_ms: Option<u64>,
    /// The child reads console input records (as PowerShell's PSReadLine and cmd do), not VT.
    pub records: bool,
    /// The child writes back every byte it reads, at once (for the echo latency).
    pub echo: bool,
}

impl Case {
    fn new(name: &str, what: &'static str, cols: u16, rows: u16, output: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            what,
            cols,
            rows,
            output,
            input: Vec::new(),
            processed: false,
            read_ms: 600,
            resizes: Vec::new(),
            close_after_ms: None,
            records: false,
            echo: false,
        }
    }
}

/// A sequence ptyro writes, reads or answers, by label (SPEC §5.6, §5.7, §9.2).
pub struct Item {
    pub label: &'static str,
    pub bytes: &'static [u8],
    /// What ptyro does with it, for the report.
    pub why: &'static str,
}

/// Sequences a program sends that ptyro reads (or its emulator must see), each between two
/// markers of its own: `{label}` is written before it and `{/}` after.
pub const SEQS: &[Item] = &[
    Item {
        label: "osc7501",
        bytes: b"\x1b]7501;state=working;msg=hi\x1b\\",
        why: "status reports (§9.2)",
    },
    Item {
        label: "osc7501-bel",
        bytes: b"\x1b]7501;state=idle\x07",
        why: "status, BEL-terminated",
    },
    Item { label: "osc133a", bytes: b"\x1b]133;A\x1b\\", why: "prompt start" },
    Item { label: "osc133b", bytes: b"\x1b]133;B\x1b\\", why: "prompt end" },
    Item {
        label: "osc133c",
        bytes: b"\x1b]133;C;cmdline=ls%20-la\x1b\\",
        why: "command start, with its line",
    },
    Item { label: "osc133d", bytes: b"\x1b]133;D;0\x1b\\", why: "command end, exit 0" },
    Item { label: "osc9", bytes: b"\x1b]9;build done\x1b\\", why: "notification (iTerm2)" },
    Item {
        label: "osc777",
        bytes: b"\x1b]777;notify;Title;Body\x1b\\",
        why: "notification (urxvt)",
    },
    Item { label: "osc9;4", bytes: b"\x1b]9;4;1;42\x1b\\", why: "progress (ConEmu)" },
    Item {
        label: "osc9;9",
        bytes: b"\x1b]9;9;C:\\src\\app\x1b\\",
        why: "working directory (ConEmu, WT)",
    },
    Item {
        label: "osc7",
        bytes: b"\x1b]7;file://host/home/me/app\x1b\\",
        why: "working directory",
    },
    Item { label: "osc52", bytes: b"\x1b]52;c;aGVsbG8=\x1b\\", why: "copy to the clipboard" },
    Item {
        label: "osc8",
        bytes: b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        why: "hyperlink",
    },
    Item { label: "osc2", bytes: b"\x1b]2;my title\x1b\\", why: "window title" },
    Item {
        label: "osc21337",
        bytes: b"\x1b]21337;state=busy\x1b\\",
        why: "iTerm2 status (foreign)",
    },
    Item { label: "osc99", bytes: b"\x1b]99;;kitty says hi\x1b\\", why: "notification (kitty)" },
    Item {
        label: "dcs-tmux",
        bytes: b"\x1bPtmux;\x1b\x1b]9;wrapped\x07\x1b\\",
        why: "tmux passthrough wrapper",
    },
    Item { label: "dcs-sixel", bytes: b"\x1bPq#0;2;0;0;0#0~~@@vv@@~~$-\x1b\\", why: "sixel image" },
    Item { label: "apc", bytes: b"\x1b_Gf=24,a=q;AAAA\x1b\\", why: "kitty graphics (APC)" },
    Item {
        label: "sync",
        bytes: b"\x1b[?2026hinside\x1b[?2026l",
        why: "synchronized output (2026)",
    },
    Item { label: "paste-mode", bytes: b"\x1b[?2004h", why: "bracketed paste on" },
    Item { label: "mouse-mode", bytes: b"\x1b[?1000h\x1b[?1006h", why: "mouse reporting, SGR" },
    Item { label: "focus-mode", bytes: b"\x1b[?1004h", why: "focus reporting" },
    Item { label: "kitty-push", bytes: b"\x1b[>1u", why: "kitty keyboard, push flags" },
    Item { label: "cursor-shape", bytes: b"\x1b[5 q", why: "cursor style (DECSCUSR)" },
    Item { label: "truecolor", bytes: b"\x1b[38;2;255;100;0mRGB\x1b[0m", why: "24-bit colour" },
    Item {
        label: "underline-curly",
        bytes: b"\x1b[4:3;58:2::255:0:0mwavy\x1b[0m",
        why: "curly, coloured underline",
    },
    Item { label: "wide", bytes: "漢字🙂e\u{301}".as_bytes(), why: "wide, emoji, combining" },
];

/// Queries a program sends and someone must answer: today ptyro does (§5.6). Behind ConPTY,
/// does conhost answer them itself, pass them on, or drop them?
pub const QUERIES: &[Item] = &[
    Item { label: "da1", bytes: b"\x1b[c", why: "device attributes" },
    Item { label: "da2", bytes: b"\x1b[>c", why: "secondary attributes" },
    Item { label: "xtversion", bytes: b"\x1b[>q", why: "terminal name and version" },
    Item { label: "dsr", bytes: b"\x1b[5n", why: "status report" },
    Item { label: "cpr", bytes: b"\x1b[6n", why: "cursor position" },
    Item { label: "decrqm2026", bytes: b"\x1b[?2026$p", why: "is synchronized output supported" },
    Item { label: "kitty-query", bytes: b"\x1b[?u", why: "kitty keyboard flags" },
    Item { label: "osc10", bytes: b"\x1b]10;?\x1b\\", why: "default foreground" },
    Item { label: "osc11", bytes: b"\x1b]11;?\x1b\\", why: "default background" },
    Item { label: "osc7501q", bytes: b"\x1b]7501;?\x1b\\", why: "the status probe (§9.2)" },
    Item { label: "xtgettcap", bytes: b"\x1bP+q544e\x1b\\", why: "terminfo name" },
    Item { label: "decrqss", bytes: b"\x1bP$qm\x1b\\", why: "current SGR" },
    Item { label: "csi14t", bytes: b"\x1b[14t", why: "text area in pixels" },
    Item { label: "csi16t", bytes: b"\x1b[16t", why: "cell size (Claude Code asks)" },
    Item { label: "csi18t", bytes: b"\x1b[18t", why: "text area in cells" },
];

/// Keys and reports the host types, one at a time, to a child that turned on bracketed paste,
/// SGR mouse, focus reports and the kitty keyboard first.
pub const KEYS: &[Item] = &[
    Item { label: "text", bytes: b"ab", why: "plain text" },
    Item { label: "enter", bytes: b"\r", why: "Enter" },
    Item { label: "up", bytes: b"\x1b[A", why: "cursor key" },
    Item { label: "f1", bytes: b"\x1bOP", why: "F1 (SS3)" },
    Item { label: "f5", bytes: b"\x1b[15~", why: "F5" },
    Item { label: "alt-x", bytes: b"\x1bx", why: "Alt+x" },
    Item { label: "ctrl-a", bytes: b"\x01", why: "Ctrl+A" },
    Item { label: "backspace", bytes: b"\x7f", why: "Backspace" },
    Item { label: "utf8", bytes: "é漢".as_bytes(), why: "non-ASCII text" },
    Item { label: "paste", bytes: b"\x1b[200~pasted\rline\x1b[201~", why: "bracketed paste" },
    Item { label: "mouse-down", bytes: b"\x1b[<0;10;5M", why: "SGR mouse press" },
    Item { label: "mouse-up", bytes: b"\x1b[<0;10;5m", why: "SGR mouse release" },
    Item { label: "focus-in", bytes: b"\x1b[I", why: "focus in" },
    Item { label: "shift-enter", bytes: b"\x1b[13;2u", why: "Shift+Enter (kitty CSI u)" },
    Item { label: "ctrl-i", bytes: b"\x1b[105;5u", why: "Ctrl+I (kitty CSI u)" },
    Item {
        label: "shift-enter-w32",
        bytes: b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_",
        why: "Shift+Enter, win32-input-mode",
    },
    Item {
        label: "a-w32",
        bytes: b"\x1b[65;30;97;1;0;1_\x1b[65;30;97;0;0;1_",
        why: "a, win32-input-mode",
    },
];

/// Keys typed to a child that reads console input records with VT input off: what conhost makes
/// of plain VT, of the kitty keyboard's CSI u, and of win32-input-mode (`CSI Vk;Sc;Uc;Kd;Cs;Rc _`,
/// which conhost asks the terminal for with `?9001h`).
pub const RECORD_KEYS: &[Item] = &[
    Item { label: "enter", bytes: b"\r", why: "Enter, plain VT" },
    Item { label: "shift-enter-csiu", bytes: b"\x1b[13;2u", why: "Shift+Enter, kitty CSI u" },
    Item {
        label: "shift-enter-w32",
        bytes: b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_",
        why: "Shift+Enter, win32-input-mode",
    },
    Item { label: "up", bytes: b"\x1b[A", why: "Up, plain VT" },
    Item { label: "ctrl-up", bytes: b"\x1b[1;5A", why: "Ctrl+Up, plain VT" },
    Item { label: "alt-x", bytes: b"\x1bx", why: "Alt+x, plain VT" },
    Item { label: "ctrl-space", bytes: b"\x00", why: "Ctrl+Space, plain VT (NUL)" },
    Item {
        label: "ctrl-space-w32",
        bytes: b"\x1b[32;57;0;1;8;1_\x1b[32;57;0;0;8;1_",
        why: "Ctrl+Space, win32-input-mode",
    },
];

/// The opening and closing markers around item `label` (plain text, so conhost renders them).
pub fn markers(label: &str) -> (String, String) {
    (format!("{{{label}}}"), "{/}".to_owned())
}

/// The recorded program output in ptyro-term's fixtures, by file name (`name.COLSxROWS.ptylog`).
pub fn fixtures(dir: &std::path::Path) -> Vec<(String, u16, u16, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    let mut paths: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let file = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let Some(stem) = file.strip_suffix(".ptylog") else { continue };
        let Some((name, size)) = stem.rsplit_once('.') else { continue };
        let Some((c, r)) = size.split_once('x') else { continue };
        let (Ok(c), Ok(r)) = (c.parse(), r.parse()) else { continue };
        if let Ok(bytes) = std::fs::read(&p) {
            out.push((name.to_owned(), c, r, bytes));
        }
    }
    out
}

/// Every case, in the order they run. `fixtures`: ptyro-term's recorded screens.
pub fn all(fixtures: &[(String, u16, u16, Vec<u8>)]) -> Vec<Case> {
    let mut cases = Vec::new();

    let mut out = Vec::new();
    for it in SEQS {
        let (open, close) = markers(it.label);
        out.extend_from_slice(open.as_bytes());
        out.extend_from_slice(it.bytes);
        out.extend_from_slice(close.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    cases.push(Case::new("seqs", "each sequence ptyro reads, between text markers", 120, 40, out));

    // One query per run, so an answer can't be mistaken for another's.
    for q in QUERIES {
        let (open, close) = markers(q.label);
        let mut out = open.into_bytes();
        out.extend_from_slice(q.bytes);
        out.extend_from_slice(close.as_bytes());
        out.extend_from_slice(b"\r\n");
        let mut c =
            Case::new(&format!("query-{}", q.label), "a query: who answers it", 80, 24, out);
        c.read_ms = 700;
        cases.push(c);
    }

    // Keys: the child turns the modes on first, the host types each 200 ms apart.
    let mut c = Case::new(
        "keys",
        "keys and reports typed into the pseudoconsole",
        80,
        24,
        b"\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[>1uready\r\n".to_vec(),
    );
    c.input = KEYS.iter().map(|k| (200, k.bytes.to_vec())).collect();
    c.read_ms = 200 * KEYS.len() as u32 + 1500;
    cases.push(c);

    let mut c = Case::new(
        "records",
        "keys read as console records (PSReadLine, cmd)",
        80,
        24,
        b"ready\r\n".to_vec(),
    );
    c.records = true;
    c.input = RECORD_KEYS.iter().map(|k| (250, k.bytes.to_vec())).collect();
    c.read_ms = 250 * RECORD_KEYS.len() as u32 + 1500;
    cases.push(c);

    // Echo latency: one letter every 150 ms, the child writes each back at once.
    let mut c = Case::new(
        "latency",
        "key to echo: how long a typed letter takes to come back",
        80,
        24,
        b"ready\r\n".to_vec(),
    );
    c.echo = true;
    c.input = (b'a'..=b't').map(|ch| (150, vec![ch])).collect();
    c.read_ms = 150 * 20 + 1500;
    cases.push(c);

    // Throughput: 2 MiB of lines as fast as the child can write them, then a marker.
    let mut flood = Vec::with_capacity(2 << 20);
    let mut i = 0u32;
    while flood.len() < (2 << 20) {
        flood.extend_from_slice(format!("{i:07} \x1b[32mgreen\x1b[0m the quick brown fox jumps over the lazy dog 0123456789\r\n").as_bytes());
        i += 1;
    }
    flood.extend_from_slice(b"FLOOD-END\r\n");
    let mut c =
        Case::new("flood", "2 MiB of coloured lines: how fast they come through", 120, 40, flood);
    c.read_ms = 200;
    cases.push(c);

    let mut c =
        Case::new("ctrl-c", "Ctrl+C with processed input: a signal", 80, 24, b"ready\r\n".to_vec());
    c.processed = true;
    c.input = vec![(400, b"\x03".to_vec())];
    c.read_ms = 1500;
    cases.push(c);

    let mut c = Case::new(
        "ctrl-c-raw",
        "Ctrl+C with processed input off: a byte",
        80,
        24,
        b"ready\r\n".to_vec(),
    );
    c.input = vec![(400, b"\x03".to_vec())];
    c.read_ms = 1500;
    cases.push(c);

    let mut c = Case::new(
        "close",
        "the pseudoconsole closed under a running child",
        80,
        24,
        b"ready\r\n".to_vec(),
    );
    c.read_ms = 8000;
    c.close_after_ms = Some(1000);
    cases.push(c);

    // A screenful, then resizes: what conhost writes after each.
    let mut fill = Vec::new();
    for i in 0..60 {
        fill.extend_from_slice(
            format!("line {i:02} the quick brown fox jumps over the lazy dog\r\n").as_bytes(),
        );
    }
    fill.extend_from_slice(b"\x1b[7mbottom\x1b[0m");
    let mut c = Case::new("resize", "60 lines, then narrower, then wider", 80, 24, fill);
    c.resizes = vec![(500, 60, 20), (500, 100, 30), (500, 80, 24)];
    c.read_ms = 2500;
    cases.push(c);

    // The alternate screen, then a resize: does the repaint leave the main screen alone?
    let mut alt = b"main screen text\r\n\x1b[?1049h\x1b[2J\x1b[H".to_vec();
    alt.extend_from_slice(b"full-screen app\x1b[10;10Hmiddle");
    let mut c = Case::new("alt-resize", "the alternate screen, then a resize", 80, 24, alt);
    c.resizes = vec![(500, 70, 20)];
    c.read_ms = 1500;
    cases.push(c);

    for (name, cols, rows, bytes) in fixtures {
        cases.push(Case::new(
            &format!("fixture-{name}"),
            "a recorded program's output",
            *cols,
            *rows,
            bytes.clone(),
        ));
    }
    cases
}
