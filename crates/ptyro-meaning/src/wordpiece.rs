//! BERT uncased WordPiece, as the `tokenizers` library runs `bge-base-en-v1.5`'s `tokenizer.json`
//! (the tokenizer of `potion-base-8M`): the BertNormalizer (clean the text, space out CJK, strip
//! accents, lowercase), the BertPreTokenizer (split on whitespace and on every punctuation mark)
//! and WordPiece (longest match first, `##` continuations, a word of more than 100 characters or
//! with an unknown piece is `[UNK]`). No special tokens are added: model2vec encodes with
//! `add_special_tokens=False`.
//!
//! Two parts of Unicode are approximated, because the tables they need are a crate each:
//! accents are stripped for Latin letters (U+00C0–U+024F, and every combining mark U+0300–U+036F)
//! rather than by full NFD, and "punctuation" is ASCII's plus the common Unicode punctuation
//! blocks rather than every `P*` character. Text in those scripts embeds a little differently
//! from the reference; English, and most European text, the same.

use std::collections::HashMap;

/// A WordPiece vocabulary and its rules.
#[derive(Debug, Clone)]
pub struct WordPiece {
    vocab: HashMap<String, u32>,
    unk: Option<u32>,
    prefix: String,
    max_chars: usize,
    lowercase: bool,
    strip_accents: bool,
}

impl WordPiece {
    /// From a `vocab.txt` (one token a line, the line number is its id).
    pub fn from_vocab_txt(text: &str) -> Self {
        let vocab = text
            .lines()
            .enumerate()
            .map(|(i, t)| (t.trim_end_matches('\r').to_owned(), i as u32))
            .collect();
        Self::new(vocab, "[UNK]", "##", 100, true, true)
    }

    /// From a `tokenizer.json` whose model is WordPiece; `None` when it is another kind.
    pub fn from_tokenizer_json(v: &serde_json::Value) -> Option<Self> {
        let model = &v["model"];
        if model["type"].as_str() != Some("WordPiece") {
            return None;
        }
        let vocab: HashMap<String, u32> = model["vocab"]
            .as_object()?
            .iter()
            .filter_map(|(k, id)| Some((k.clone(), u32::try_from(id.as_u64()?).ok()?)))
            .collect();
        let norm = &v["normalizer"];
        let lowercase = norm["lowercase"].as_bool().unwrap_or(true);
        // `strip_accents: null` follows `lowercase`, as in BERT.
        let strip_accents = norm["strip_accents"].as_bool().unwrap_or(lowercase);
        Some(Self::new(
            vocab,
            model["unk_token"].as_str().unwrap_or("[UNK]"),
            model["continuing_subword_prefix"].as_str().unwrap_or("##"),
            model["max_input_chars_per_word"].as_u64().unwrap_or(100) as usize,
            lowercase,
            strip_accents,
        ))
    }

    fn new(
        vocab: HashMap<String, u32>,
        unk: &str,
        prefix: &str,
        max_chars: usize,
        lowercase: bool,
        strip_accents: bool,
    ) -> Self {
        let unk = vocab.get(unk).copied();
        Self { vocab, unk, prefix: prefix.to_owned(), max_chars, lowercase, strip_accents }
    }

    pub fn len(&self) -> usize {
        self.vocab.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vocab.is_empty()
    }

    /// The largest id, so the embedding has a row for every token.
    pub fn max_id(&self) -> Option<u32> {
        self.vocab.values().copied().max()
    }

    pub fn unk(&self) -> Option<u32> {
        self.unk
    }

    /// `text`'s token ids, `[UNK]` included where a word has no pieces.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let norm = self.normalize(text);
        let mut ids = Vec::new();
        for word in pre_tokenize(&norm) {
            self.word(word, &mut ids);
        }
        ids
    }

    fn normalize(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            // clean_text: no NUL, replacement or control characters; all whitespace a space.
            if c == '\0' || c == '\u{fffd}' || (is_control(c) && !c.is_whitespace()) {
                continue;
            }
            if c.is_whitespace() {
                out.push(' ');
                continue;
            }
            // handle_chinese_chars: each CJK ideograph is a word of its own.
            if is_cjk(c) {
                out.push(' ');
                out.push(c);
                out.push(' ');
                continue;
            }
            let c = if self.strip_accents {
                if is_combining(c) {
                    continue;
                }
                strip_accent(c)
            } else {
                c
            };
            if self.lowercase {
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
        }
        out
    }

    /// One word's pieces, longest first; a word with no way through is one `[UNK]`.
    fn word(&self, word: &str, ids: &mut Vec<u32>) {
        let chars: Vec<(usize, char)> = word.char_indices().collect();
        if chars.len() > self.max_chars {
            ids.extend(self.unk);
            return;
        }
        let at = |i: usize| chars.get(i).map_or(word.len(), |c| c.0);
        let mut pieces = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut found = None;
            let mut key = String::new();
            while start < end {
                key.clear();
                if start > 0 {
                    key.push_str(&self.prefix);
                }
                key.push_str(&word[at(start)..at(end)]);
                if let Some(&id) = self.vocab.get(&key) {
                    found = Some(id);
                    break;
                }
                end -= 1;
            }
            match found {
                Some(id) => pieces.push(id),
                None => {
                    ids.extend(self.unk);
                    return;
                }
            }
            start = end;
        }
        ids.extend(pieces);
    }
}

/// BertPreTokenizer: whitespace separates words; every punctuation mark is a word of its own.
fn pre_tokenize(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for chunk in text.split(char::is_whitespace).filter(|s| !s.is_empty()) {
        let mut from = 0;
        for (i, c) in chunk.char_indices() {
            if is_punct(c) {
                if from < i {
                    out.push(&chunk[from..i]);
                }
                out.push(&chunk[i..i + c.len_utf8()]);
                from = i + c.len_utf8();
            }
        }
        if from < chunk.len() {
            out.push(&chunk[from..]);
        }
    }
    out
}

fn is_control(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
}

fn is_combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036f}')
}

/// BERT's punctuation: ASCII's (`$`, `+`, `<`… count too) and the Unicode punctuation blocks.
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(c,
            '\u{a1}' | '\u{a7}' | '\u{ab}' | '\u{b6}' | '\u{b7}' | '\u{bb}' | '\u{bf}'
            | '\u{37e}' | '\u{387}'
            | '\u{55a}'..='\u{55f}' | '\u{589}' | '\u{58a}'
            | '\u{5be}' | '\u{5c0}' | '\u{5c3}' | '\u{5c6}' | '\u{5f3}' | '\u{5f4}'
            | '\u{609}' | '\u{60a}' | '\u{60c}' | '\u{60d}' | '\u{61b}' | '\u{61e}' | '\u{61f}'
            | '\u{66a}'..='\u{66d}' | '\u{6d4}'
            | '\u{964}' | '\u{965}' | '\u{970}' | '\u{e4f}' | '\u{e5a}' | '\u{e5b}'
            | '\u{2010}'..='\u{2027}' | '\u{2030}'..='\u{2043}' | '\u{2045}'..='\u{2051}'
            | '\u{2053}'..='\u{205e}' | '\u{207d}' | '\u{207e}' | '\u{208d}' | '\u{208e}'
            | '\u{2308}'..='\u{230b}' | '\u{2329}' | '\u{232a}' | '\u{2768}'..='\u{2775}'
            | '\u{27c5}' | '\u{27c6}' | '\u{27e6}'..='\u{27ef}' | '\u{2983}'..='\u{2998}'
            | '\u{29d8}'..='\u{29db}' | '\u{29fc}' | '\u{29fd}' | '\u{2cf9}'..='\u{2cfc}'
            | '\u{2cfe}' | '\u{2cff}' | '\u{2e00}'..='\u{2e2e}' | '\u{2e30}'..='\u{2e4f}'
            | '\u{3001}'..='\u{3003}' | '\u{3008}'..='\u{3011}' | '\u{3014}'..='\u{301f}'
            | '\u{3030}' | '\u{303d}' | '\u{30a0}' | '\u{30fb}'
            | '\u{fe10}'..='\u{fe19}' | '\u{fe30}'..='\u{fe52}' | '\u{fe54}'..='\u{fe61}'
            | '\u{fe63}' | '\u{fe68}' | '\u{fe6a}' | '\u{fe6b}'
            | '\u{ff01}'..='\u{ff03}' | '\u{ff05}'..='\u{ff0a}' | '\u{ff0c}'..='\u{ff0f}'
            | '\u{ff1a}' | '\u{ff1b}' | '\u{ff1f}' | '\u{ff20}' | '\u{ff3b}'..='\u{ff3d}'
            | '\u{ff3f}' | '\u{ff5b}' | '\u{ff5d}' | '\u{ff5f}'..='\u{ff65}')
}

/// BERT's "Chinese character" ranges (the CJK ideograph blocks).
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{20000}'..='\u{2a6df}'
        | '\u{2a700}'..='\u{2b73f}' | '\u{2b740}'..='\u{2b81f}' | '\u{2b820}'..='\u{2ceaf}'
        | '\u{f900}'..='\u{faff}' | '\u{2f800}'..='\u{2fa1f}')
}

/// A precomposed Latin letter's base letter (what NFD and dropping the marks leaves; the tables
/// were made with Python's `unicodedata`).
fn strip_accent(c: char) -> char {
    // U+00C0..U+017F, one base letter each ('·' where the letter has no decomposition).
    const LATIN: &str = "AAAAAA·CEEEEIIII·NOOOOO··UUUUY··aaaaaa·ceeeeiiii·nooooo··uuuuy·yAaAaAaCcCcCcCcDd··EeEeEeEeEeGgGg\
                         GgGgHh··IiIiIiIiI···JjKk·LlLlLl····NnNnNn···OoOoOo··RrRrRrSsSsSsSsTtTt··UuUuUuUuUuUuWwYyYZzZzZz·";
    let n = c as u32;
    if (0xc0..=0x17f).contains(&n) {
        return match LATIN.chars().nth((n - 0xc0) as usize) {
            Some(b) if b != '·' => b,
            _ => c,
        };
    }
    match c {
        'Ǎ' | 'Ǟ' | 'Ǡ' | 'Ǻ' | 'Ȁ' | 'Ȃ' | 'Ȧ' => 'A',
        'Ȅ' | 'Ȇ' | 'Ȩ' => 'E',
        'Ǧ' | 'Ǵ' => 'G',
        'Ȟ' => 'H',
        'Ǐ' | 'Ȉ' | 'Ȋ' => 'I',
        'Ǩ' => 'K',
        'Ǹ' => 'N',
        'Ơ' | 'Ǒ' | 'Ǫ' | 'Ǭ' | 'Ȍ' | 'Ȏ' | 'Ȫ' | 'Ȭ' | 'Ȯ' | 'Ȱ' => 'O',
        'Ȑ' | 'Ȓ' => 'R',
        'Ș' => 'S',
        'Ț' => 'T',
        'Ư' | 'Ǔ' | 'Ǖ' | 'Ǘ' | 'Ǚ' | 'Ǜ' | 'Ȕ' | 'Ȗ' => 'U',
        'Ȳ' => 'Y',
        'ǎ' | 'ǟ' | 'ǡ' | 'ǻ' | 'ȁ' | 'ȃ' | 'ȧ' => 'a',
        'ȅ' | 'ȇ' | 'ȩ' => 'e',
        'ǧ' | 'ǵ' => 'g',
        'ȟ' => 'h',
        'ǐ' | 'ȉ' | 'ȋ' => 'i',
        'ǰ' => 'j',
        'ǩ' => 'k',
        'ǹ' => 'n',
        'ơ' | 'ǒ' | 'ǫ' | 'ǭ' | 'ȍ' | 'ȏ' | 'ȫ' | 'ȭ' | 'ȯ' | 'ȱ' => 'o',
        'ȑ' | 'ȓ' => 'r',
        'ș' => 's',
        'ț' => 't',
        'ư' | 'ǔ' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' | 'ȕ' | 'ȗ' => 'u',
        'ȳ' => 'y',
        'Ǣ' | 'Ǽ' => 'Æ',
        'Ǿ' => 'Ø',
        'ǣ' | 'ǽ' => 'æ',
        'ǿ' => 'ø',
        'Ǯ' => 'Ʒ',
        'ǯ' => 'ʒ',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(tokens: &[&str]) -> WordPiece {
        WordPiece::from_vocab_txt(&tokens.join("\n"))
    }

    #[test]
    fn the_latin_table_lines_up() {
        // Spot checks across the table: each letter's position must match its code point.
        for (c, b) in [
            ('À', 'A'),
            ('Ç', 'C'),
            ('Ñ', 'N'),
            ('Ø', 'Ø'),
            ('Ý', 'Y'),
            ('à', 'a'),
            ('é', 'e'),
            ('ÿ', 'y'),
            ('Ā', 'A'),
            ('ď', 'd'),
            ('Đ', 'Đ'),
            ('ę', 'e'),
            ('ğ', 'g'),
            ('Ħ', 'Ħ'),
            ('İ', 'I'),
            ('ı', 'ı'),
            ('Ĵ', 'J'),
            ('ķ', 'k'),
            ('ĸ', 'ĸ'),
            ('ľ', 'l'),
            ('Ł', 'Ł'),
            ('ń', 'n'),
            ('ŉ', 'ŉ'),
            ('ő', 'o'),
            ('Œ', 'Œ'),
            ('ř', 'r'),
            ('š', 's'),
            ('ţ', 't'),
            ('Ŧ', 'Ŧ'),
            ('ů', 'u'),
            ('ŵ', 'w'),
            ('Ÿ', 'Y'),
            ('ž', 'z'),
            ('ſ', 'ſ'),
        ] {
            assert_eq!(strip_accent(c), b, "{c}");
        }
    }

    #[test]
    fn words_split_on_spaces_and_punctuation() {
        assert_eq!(
            pre_tokenize("hello,  world! it's"),
            ["hello", ",", "world", "!", "it", "'", "s"]
        );
        assert_eq!(pre_tokenize("a—b"), ["a", "—", "b"]);
    }

    #[test]
    fn normalizes_like_bert_uncased() {
        let w = wp(&["[UNK]"]);
        assert_eq!(w.normalize("Café\tNAÏVE\u{0}\u{7}"), "cafe naive");
        assert_eq!(w.normalize("e\u{301}x"), "ex");
        assert_eq!(w.normalize("日本"), " 日  本 ");
    }

    #[test]
    fn longest_match_first_with_continuations() {
        let w = wp(&["[UNK]", "un", "##aff", "##able", "una", "##ff", "play", "##ing", ","]);
        // "una" is longer than "un": longest first, even where the shorter would read better.
        assert_eq!(w.encode("Unaffable"), [4, 5, 3]);
        assert_eq!(w.encode("playing, playing"), [6, 7, 8, 6, 7]);
        // A word with a piece missing is one [UNK], not a partial match.
        assert_eq!(w.encode("playx"), [0]);
        // Too long: [UNK] without trying.
        assert_eq!(w.encode(&"a".repeat(101)), [0]);
    }

    #[test]
    fn reads_a_tokenizer_json() {
        let j = serde_json::json!({
            "normalizer": { "type": "BertNormalizer", "lowercase": true, "strip_accents": null },
            "model": { "type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##",
                       "max_input_chars_per_word": 100, "vocab": { "[UNK]": 0, "cafe": 1 } }
        });
        let w = WordPiece::from_tokenizer_json(&j).unwrap();
        assert_eq!(w.encode("Café"), [1]);
        assert_eq!((w.len(), w.max_id(), w.unk()), (2, Some(1), Some(0)));
        let bpe = serde_json::json!({ "model": { "type": "BPE" } });
        assert!(WordPiece::from_tokenizer_json(&bpe).is_none());
    }
}
