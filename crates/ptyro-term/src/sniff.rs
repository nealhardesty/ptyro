//! Side-channel scanner (SPEC §5.6, §5.7): sees every byte the child writes *before* the emulator
//! does, so ptyro can answer queries alacritty_terminal doesn't (OSC 7501, XTVERSION, XTGETTCAP,
//! `CSI 16 t`) and observe sequences alacritty swallows (OSC 7501 reports, OSC 9/777/99).
//!
//! Stateful across reads: sequences may be split at any byte. Bodies are capped at [`MAX_SEQ`];
//! an oversized sequence is dropped whole, never truncated.

pub const MAX_SEQ: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seq {
    /// OSC body without the `ESC ]` introducer and terminator.
    Osc(Vec<u8>),
    /// CSI: parameter + intermediate bytes, and the final byte.
    Csi { params: Vec<u8>, final_byte: u8 },
    /// DCS body without `ESC P` and the terminator.
    Dcs(Vec<u8>),
    /// RIS (`ESC c`): a full reset, which also removes every OSC 7501 record.
    Ris,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    #[default]
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Dcs,
    DcsEsc,
    /// SOS/PM/APC: skipped until ST, so an `ESC \` inside isn't misread.
    Str,
    StrEsc,
}

#[derive(Default)]
pub struct Sniffer {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl Sniffer {
    pub fn scan(&mut self, bytes: &[u8], out: &mut Vec<Seq>) {
        self.scan_with_ends(bytes, out, None);
    }

    /// Like [`scan`](Self::scan), also recording, for each sequence, the offset in `bytes` just
    /// past its terminator (so a caller can split the batch exactly there).
    pub fn scan_with_ends(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<Seq>,
        mut ends: Option<&mut Vec<usize>>,
    ) {
        let mut i = 0;
        while i < bytes.len() {
            // Fast path: plain text can't start a sequence, so skip straight to the next ESC.
            if self.state == State::Ground {
                match bytes[i..].iter().position(|&b| b == 0x1b) {
                    Some(n) => i += n,
                    None => return,
                }
            }
            let before = out.len();
            self.step(bytes[i], out);
            i += 1;
            if out.len() > before
                && let Some(ends) = ends.as_deref_mut()
            {
                ends.push(i);
            }
        }
    }

    fn step(&mut self, b: u8, out: &mut Vec<Seq>) {
        use State::*;
        self.state = match (self.state, b) {
            (Ground, 0x1b) => Esc,
            (Ground, _) => Ground,
            (Esc, b'[') => self.start(Csi),
            (Esc, b']') => self.start(Osc),
            (Esc, b'P') => self.start(Dcs),
            (Esc, b'X' | b'^' | b'_') => Str,
            (Esc, b'c') => {
                out.push(Seq::Ris);
                Ground
            }
            (Esc, 0x1b) => Esc,
            (Esc, _) => Ground,
            (Csi, 0x40..=0x7e) => {
                if !self.overflow {
                    out.push(Seq::Csi { params: std::mem::take(&mut self.buf), final_byte: b });
                }
                Ground
            }
            (Csi, 0x1b) => Esc,
            (Csi, 0x18 | 0x1a) => Ground, // CAN / SUB abort
            (Csi, _) => self.push(b, Csi),
            (Osc, 0x07) => self.finish_osc(out),
            (Osc, 0x1b) => OscEsc,
            (Osc, 0x18 | 0x1a) => Ground,
            (Osc, _) => self.push(b, Osc),
            (OscEsc, b'\\') => self.finish_osc(out),
            (OscEsc, b'[') => self.start(Csi),
            (OscEsc, b']') => self.start(Osc),
            (OscEsc, _) => Ground,
            (Dcs, 0x1b) => DcsEsc,
            (Dcs, 0x18 | 0x1a) => Ground,
            (Dcs, _) => self.push(b, Dcs),
            (DcsEsc, b'\\') => {
                if !self.overflow {
                    out.push(Seq::Dcs(std::mem::take(&mut self.buf)));
                }
                Ground
            }
            (DcsEsc, _) => Ground,
            (Str, 0x1b) => StrEsc,
            (Str, 0x07) => Ground,
            (Str, _) => Str,
            (StrEsc, b'\\') => Ground,
            (StrEsc, _) => Str,
        };
    }

    pub(crate) fn state(&self) -> State {
        self.state
    }

    /// Offset of the first `\n` in `bytes` that is outside any escape sequence, scanning from
    /// `state`.
    pub(crate) fn ground_newline(state: State, bytes: &[u8]) -> Option<usize> {
        let mut s = Sniffer { state, ..Sniffer::default() };
        let mut sink = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' && s.state == State::Ground {
                return Some(i);
            }
            s.step(b, &mut sink);
            sink.clear();
        }
        None
    }

    fn start(&mut self, s: State) -> State {
        self.buf.clear();
        self.overflow = false;
        s
    }

    fn push(&mut self, b: u8, s: State) -> State {
        if self.buf.len() < MAX_SEQ {
            self.buf.push(b);
        } else {
            self.overflow = true;
        }
        s
    }

    fn finish_osc(&mut self, out: &mut Vec<Seq>) -> State {
        if !self.overflow {
            out.push(Seq::Osc(std::mem::take(&mut self.buf)));
        }
        State::Ground
    }
}

/// OSC 7501 Program Status Protocol message (spec rev 0.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Psp {
    Probe,
    Report(PspReport),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PspReport {
    pub state: String,
    pub id: Option<String>,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

const STATES: &[&str] = &["idle", "working", "done", "blocked", "error", "clear"];
const KINDS: &[&str] = &["permission", "question", "auth"];

fn token_ok(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

/// `segment ("/" segment)*`, segments `[A-Za-z0-9_.+-]{1,32}`, ≤ 8 deep, ≤ 128 bytes.
fn id_ok(id: &str) -> bool {
    id.len() <= 128 && id.split('/').count() <= 8 && id.split('/').all(|seg| token_ok(seg, 32))
}

/// Parse an OSC body (spec rev 0.4). Returns `None` for non-7501 OSCs and for every report the
/// spec says to discard whole: over a limit, bad base64, control characters in the decoded
/// text, an invalid id, or a missing or unknown `state`. Nothing from a discarded report applies.
pub fn parse_psp(osc: &[u8]) -> Option<Psp> {
    let body = osc.strip_prefix(b"7501;")?;
    if body.trim_ascii() == b"?" {
        return Some(Psp::Probe);
    }
    if osc.len() > 4096 {
        return None;
    }
    let body = std::str::from_utf8(body).ok()?;
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for pair in body.split(':') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let key_ok = (1..=16).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_lowercase());
        let value_ok = v.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b));
        if !key_ok || !value_ok {
            continue; // a malformed pair is skipped; the rest still counts
        }
        pairs.retain(|(pk, _)| *pk != k); // repeated key: the last wins
        pairs.push((k, v));
    }
    let get = |k: &str| pairs.iter().find(|(pk, _)| *pk == k).map(|(_, v)| *v);
    let state = get("state")?;
    if !STATES.contains(&state) {
        return None;
    }
    let mut r = PspReport { state: state.to_owned(), ..PspReport::default() };
    if let Some(id) = get("id") {
        if !id_ok(id) {
            return None; // never falls back to the root record
        }
        r.id = Some(id.to_owned());
    }
    if state == "blocked" {
        r.kind = get("kind").filter(|k| KINDS.contains(k)).map(str::to_owned);
    }
    if state == "working" || state == "blocked" {
        r.progress = get("progress").and_then(|p| p.parse().ok()).filter(|p| *p <= 100);
    }
    r.app = get("app").filter(|a| token_ok(a, 32)).map(str::to_owned);
    for (key, enc_max, dec_max) in [("title", 256, 192), ("msg", 2732, 2048)] {
        let Some(v) = get(key) else { continue };
        if v.len() > enc_max {
            return None;
        }
        let text = b64(v)?;
        if text.len() > dec_max || text.chars().any(|c| c.is_control()) {
            return None;
        }
        if key == "title" {
            r.title = Some(text);
        } else {
            r.msg = Some(text);
        }
    }
    Some(Psp::Report(r))
}

/// Our probe reply (SPEC §5.6). The grammar has no spaces around `:`.
pub const PSP_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// Minimal standard-alphabet base64 decoder (padding optional), to keep the spike dependency-light.
fn b64(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().take_while(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<Seq> {
        let mut s = Sniffer::default();
        let mut out = vec![];
        for c in chunks {
            s.scan(c, &mut out);
        }
        out
    }

    #[test]
    fn probe_split_across_reads_then_da1() {
        let out = scan(&[b"hi\x1b]75", b"01;?\x1b", b"\\\x1b[c"]);
        assert_eq!(out[0], Seq::Osc(b"7501;?".to_vec()));
        assert_eq!(out[1], Seq::Csi { params: vec![], final_byte: b'c' });
        assert_eq!(parse_psp(b"7501;?"), Some(Psp::Probe));
    }

    #[test]
    fn report_with_base64() {
        let Some(Psp::Report(r)) =
            parse_psp(b"7501;state=blocked:kind=permission:msg=QmFzaChscyk:bogus")
        else {
            panic!()
        };
        assert_eq!((r.state.as_str(), r.kind.as_deref()), ("blocked", Some("permission")));
        assert_eq!(r.msg.as_deref(), Some("Bash(ls)"));
        assert_eq!(parse_psp(b"7501;state=sleepy"), None);
        assert_eq!(parse_psp(b"0;title"), None);
    }

    fn report(b: &[u8]) -> Option<PspReport> {
        match parse_psp(b)? {
            Psp::Report(r) => Some(r),
            Psp::Probe => None,
        }
    }

    #[test]
    fn conformance_rev_0_4() {
        // Whitespace, repeated keys (last wins), malformed pairs skipped.
        let r =
            report(b"7501; state = working :state=blocked:kind=question:junk:Bad=1:progress=40")
                .unwrap();
        assert_eq!(
            (r.state.as_str(), r.kind.as_deref(), r.progress),
            ("blocked", Some("question"), Some(40))
        );
        // kind only with blocked, progress only with working/blocked, unknown kind absent.
        let r = report(b"7501;state=idle:kind=permission:progress=10").unwrap();
        assert_eq!((r.kind, r.progress), (None, None));
        assert_eq!(report(b"7501;state=blocked:kind=coffee").unwrap().kind, None);
        // Control characters in decoded text discard the whole report (here "a\nb").
        assert_eq!(report(b"7501;state=working:msg=YQpi"), None);
        // Bad base64 discards it too ("-" is a legal value byte but not base64); a value with
        // illegal bytes is a malformed pair, skipped.
        assert_eq!(report(b"7501;state=working:msg=a-b"), None);
        assert!(report(b"7501;state=working:msg=@@@").unwrap().msg.is_none());
        // Ids: valid ones kept, invalid ones discard the report (never fall back to root).
        assert_eq!(
            report(b"7501;state=working:id=build/test.1").unwrap().id.as_deref(),
            Some("build/test.1")
        );
        assert_eq!(report(b"7501;state=working:id=a//b"), None);
        assert_eq!(report(b"7501;state=working:id=a/b/c/d/e/f/g/h/i"), None);
        // An invalid app is absent, not fatal.
        assert_eq!(
            report(b"7501;state=idle:app=way-too-long-application-name-xxxxxx").unwrap().app,
            None
        );
        // Missing state.
        assert_eq!(report(b"7501;kind=permission"), None);
        assert_eq!(scan(&[b"\x1bc"]), vec![Seq::Ris]);
    }

    #[test]
    fn dcs_is_captured_apc_skipped_and_bel_terminates_osc() {
        let out = scan(&[b"\x1bP+q544e\x1b\\\x1b]0;t\x07\x1b[>q\x1b_apc\x1b\\"]);
        assert_eq!(
            out,
            vec![
                Seq::Dcs(b"+q544e".to_vec()),
                Seq::Osc(b"0;t".to_vec()),
                Seq::Csi { params: b">".to_vec(), final_byte: b'q' }
            ]
        );
    }

    #[test]
    fn oversized_sequences_are_dropped_whole() {
        let mut big = b"\x1b]9;".to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_SEQ + 10));
        big.extend(b"\x07\x1b]0;ok\x07");
        assert_eq!(scan(&[&big]), vec![Seq::Osc(b"0;ok".to_vec())]);
    }
}
