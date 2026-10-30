//! Meaning search for chats (SPEC §22.10 tier 3): a static embedding of each message, compared
//! with the query's by cosine, merged with the keyword hits by reciprocal-rank fusion.
//!
//! The model is model2vec's `potion-base-8M` (MIT), read in pure Rust: no ONNX, no `tokenizers`
//! crate. model2vec's `StaticModel.encode` is a lookup, not a network: tokenize with the model's
//! WordPiece tokenizer and no special tokens (`add_special_tokens=False`, so no `[CLS]`/`[SEP]`),
//! drop `[UNK]`, keep the first 512 tokens, average their rows of the embedding matrix, and
//! normalize. [`Model::embed`] does exactly that; the vectors are stored as int8 ([`quantize`]).

use std::collections::HashMap;
use std::hash::Hash;
use std::path::Path;

pub mod safetensors;
pub mod synthetic;
pub mod wordpiece;

use safetensors::Matrix;
use wordpiece::WordPiece;

/// The model's folder name under `<data>/models/`.
pub const NAME: &str = "potion-base-8M";
/// Its Hugging Face repository and the commit it is pinned to: the files never change under us.
pub const REPO: &str = "minishlab/potion-base-8M";
pub const COMMIT: &str = "bf8b056651a2c21b8d2565580b8569da283cab23";
/// What a stored vector was made with (`node_vec.model`): the model, its commit, the encoding.
/// Vectors of any other model are made again.
pub const MODEL_ID: &str = "potion-base-8M@bf8b056/i8";

/// One file of the model, and how it is checked once downloaded.
#[derive(Debug, Clone, Copy)]
pub struct File {
    pub name: &'static str,
    /// Its exact size (from the Hugging Face API, 2026-10-30).
    pub bytes: u64,
    /// Its SHA-256, where Hugging Face publishes one (the LFS file). The small files are checked
    /// by their size and by loading them: a file that parses as the tokenizer, with a token for
    /// every row of the matrix, is the tokenizer.
    pub sha256: Option<&'static str>,
}

/// The files the model needs (31 MB). `vocab.txt` is the same vocabulary as `tokenizer.json`'s
/// (a fallback should its model not be WordPiece); `README.md` and the rest aren't needed.
pub const FILES: [File; 4] = [
    File {
        name: "model.safetensors",
        bytes: 30_236_760,
        sha256: Some("f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2"),
    },
    File { name: "tokenizer.json", bytes: 683_666, sha256: None },
    File { name: "vocab.txt", bytes: 219_690, sha256: None },
    // `{"model_type": "model2vec", …, "hidden_dim": 256, "normalize": true}`; its size isn't
    // published, so 0 means "any" and loading it checks it.
    File { name: "config.json", bytes: 0, sha256: None },
];

/// All of [`FILES`], for the download's progress.
pub fn total_bytes() -> u64 {
    FILES.iter().map(|f| f.bytes).sum()
}

/// Where a file is fetched from: `base` is `https://huggingface.co` (or a test's `file://`).
pub fn url(base: &str, file: &str) -> String {
    format!("{}/{REPO}/resolve/{COMMIT}/{file}", base.trim_end_matches('/'))
}

/// model2vec's default `max_length`.
const MAX_TOKENS: usize = 512;
/// Text past this is never reached by 512 tokens, so it isn't tokenized.
const MAX_CHARS: usize = 8 * 1024;

/// A loaded model.
#[derive(Debug)]
pub struct Model {
    tok: WordPiece,
    emb: Matrix,
}

impl Model {
    /// The model in `dir` (the files of [`FILES`]), checked: a model2vec config, a WordPiece
    /// vocabulary, a matrix with a row for every token.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let read = |f: &str| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"));
        if let Ok(c) = read("config.json") {
            let c: serde_json::Value =
                serde_json::from_slice(&c).map_err(|e| format!("config.json: {e}"))?;
            if c["model_type"].as_str().is_some_and(|t| t != "model2vec") {
                return Err(format!("config.json: a {} model, not model2vec", c["model_type"]));
            }
        }
        let tok = read("tokenizer.json")
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| WordPiece::from_tokenizer_json(&v));
        let tok = match tok {
            Some(t) => t,
            None => WordPiece::from_vocab_txt(&String::from_utf8_lossy(&read("vocab.txt")?)),
        };
        let emb = safetensors::read(&read("model.safetensors")?, "embeddings")
            .map_err(|e| format!("model.safetensors: {e}"))?;
        match tok.max_id() {
            Some(m) if (m as usize) < emb.rows => {}
            _ => {
                return Err(format!(
                    "the tokenizer's {} tokens don't fit the model's {} rows",
                    tok.len(),
                    emb.rows
                ));
            }
        }
        if emb.cols == 0 {
            return Err("model.safetensors: a matrix with no columns".into());
        }
        Ok(Self { tok, emb })
    }

    /// The length of a vector.
    pub fn dim(&self) -> usize {
        self.emb.cols
    }

    /// `text`'s vector: the mean of its tokens' rows, unit length (all zeros for text with no
    /// known token).
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let text = match text.char_indices().nth(MAX_CHARS) {
            Some((i, _)) => &text[..i],
            None => text,
        };
        let unk = self.tok.unk();
        let mut sum = vec![0f32; self.dim()];
        let mut n = 0;
        for id in self.tok.encode(text).into_iter().filter(|id| Some(*id) != unk).take(MAX_TOKENS) {
            if let Some(row) = self.emb.row(id as usize) {
                for (s, v) in sum.iter_mut().zip(row) {
                    *s += v;
                }
                n += 1;
            }
        }
        // (The mean's scale goes in the normalization.)
        if n > 0 {
            normalize(&mut sum);
        }
        sum
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// A unit vector as stored (SPEC §22.10): its scale (f32, little endian) then one signed byte a
/// dimension, `x ≈ scale × byte`. 260 bytes for 256 dimensions, a quarter of f32's, and the
/// ranking it gives is the f32 one but for near ties.
pub fn quantize(v: &[f32]) -> Vec<u8> {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    let scale = if max > 0.0 { max / 127.0 } else { 0.0 };
    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&scale.to_le_bytes());
    out.extend(v.iter().map(|x| if scale > 0.0 { (x / scale).round() as i8 as u8 } else { 0 }));
    out
}

/// The cosine of a stored vector and a unit query vector; `None` when they don't match.
pub fn score(blob: &[u8], q: &[f32]) -> Option<f32> {
    let (scale, bytes) = blob.split_first_chunk::<4>()?;
    if bytes.len() != q.len() {
        return None;
    }
    let dot: f32 = bytes.iter().zip(q).map(|(b, x)| f32::from(*b as i8) * x).sum();
    Some(dot * f32::from_le_bytes(*scale))
}

/// Below this cosine a message is not "about" the query. Measured on the synthetic model only:
/// unverified for `potion-base-8M` until it is tried on real chats.
pub const MIN_SCORE: f32 = 0.3;

/// The best `k` of `items` by score, best first (a brute-force scan's top list).
pub fn top_k<T>(items: impl IntoIterator<Item = (f32, T)>, k: usize) -> Vec<(f32, T)> {
    let mut best: Vec<(f32, T)> = Vec::with_capacity(k + 1);
    for (s, t) in items {
        if best.len() == k && best.last().is_some_and(|(w, _)| *w >= s) {
            continue;
        }
        let at = best.partition_point(|(b, _)| *b >= s);
        best.insert(at, (s, t));
        best.truncate(k);
    }
    best
}

/// How a hit was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    Words,
    Meaning,
    Both,
}

impl Found {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Words => "words",
            Self::Meaning => "meaning",
            Self::Both => "both",
        }
    }
}

/// Reciprocal-rank fusion's constant (Cormack et al.: 60 works across collections).
pub const RRF_K: f64 = 60.0;

/// Two ranked lists (best first) as one: each item scores Σ 1/(k + rank) over the lists it is
/// in. Ties keep the keyword list's order. Returns the items with their fused score.
pub fn fuse<K: Eq + Hash + Clone>(words: &[K], meaning: &[K]) -> Vec<(K, Found, f64)> {
    let mut by: HashMap<K, (f64, Found, usize)> = HashMap::new();
    for (i, k) in words.iter().enumerate() {
        by.entry(k.clone()).or_insert((1.0 / (RRF_K + i as f64 + 1.0), Found::Words, i));
    }
    for (i, k) in meaning.iter().enumerate() {
        let s = 1.0 / (RRF_K + i as f64 + 1.0);
        by.entry(k.clone())
            .and_modify(|e| {
                e.0 += s;
                e.1 = Found::Both;
            })
            .or_insert((s, Found::Meaning, words.len() + i));
    }
    let mut out: Vec<(K, f64, Found, usize)> =
        by.into_iter().map(|(k, (s, f, at))| (k, s, f, at)).collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.3.cmp(&b.3)));
    out.into_iter().map(|(k, s, f, _)| (k, f, s)).collect()
}

/// The start of a message, for a hit found by meaning (no word of it matched to show around).
pub fn opening(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let cut: String = flat.chars().take(max_chars).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > max_chars / 2 => &cut[..i],
        _ => &cut,
    };
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> (tempfile::TempDir, Model) {
        let dir = tempfile::tempdir().unwrap();
        synthetic::write(
            dir.path(),
            &[
                ("cat", &[1.0, 0.0, 0.0]),
                ("kitten", &[0.9, 0.1, 0.0]),
                ("dog", &[0.0, 1.0, 0.0]),
                ("##s", &[0.0, 0.0, 1.0]),
                ("[CLS]", &[0.0, 0.0, 5.0]),
            ],
        )
        .unwrap();
        let m = Model::load(dir.path()).unwrap();
        (dir, m)
    }

    #[test]
    fn embeds_as_model2vec_does() {
        let (_d, m) = model();
        assert_eq!(m.dim(), 3);
        // One known token: its row, normalized.
        let v = m.embed("Cat");
        assert!((v[0] - 1.0).abs() < 1e-6 && v[1] == 0.0);
        // The mean of the tokens' rows; unknown words (and [UNK]) leave no trace, and no special
        // token is added (the [CLS] row would pull it to the third axis).
        let a = m.embed("cat dog zebra");
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!((a[0] - s).abs() < 1e-6 && (a[1] - s).abs() < 1e-6 && a[2] == 0.0, "{a:?}");
        // A continuation is a token of its own: "cats" = cat + ##s.
        let c = m.embed("cats");
        assert!((c[0] - s).abs() < 1e-6 && (c[2] - s).abs() < 1e-6, "{c:?}");
        // Nothing known: zeros.
        assert_eq!(m.embed("zebra!"), [0.0, 0.0, 0.0]);
        // Near in meaning, near in cosine.
        let q = m.embed("kitten");
        let cat = score(&quantize(&m.embed("my cat")), &q).unwrap();
        let dog = score(&quantize(&m.embed("my dog")), &q).unwrap();
        assert!(cat > 0.9 && dog < 0.2, "{cat} {dog}");
    }

    #[test]
    fn a_bad_model_says_what_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        synthetic::write(dir.path(), &[("a", &[1.0])]).unwrap();
        // A vocabulary bigger than the matrix.
        std::fs::write(dir.path().join("tokenizer.json"), "{}").unwrap();
        std::fs::write(dir.path().join("vocab.txt"), "[UNK]\na\nb\nc").unwrap();
        assert!(Model::load(dir.path()).unwrap_err().contains("don't fit"));
        std::fs::write(dir.path().join("vocab.txt"), "[UNK]\na").unwrap();
        // vocab.txt is the fallback when tokenizer.json isn't WordPiece.
        assert!(Model::load(dir.path()).is_ok());
        std::fs::write(dir.path().join("config.json"), r#"{"model_type": "bert"}"#).unwrap();
        assert!(Model::load(dir.path()).unwrap_err().contains("not model2vec"));
        std::fs::remove_file(dir.path().join("model.safetensors")).unwrap();
        std::fs::remove_file(dir.path().join("config.json")).unwrap();
        assert!(Model::load(dir.path()).unwrap_err().contains("model.safetensors"));
    }

    #[test]
    fn quantized_vectors_keep_the_cosine() {
        let mut v: Vec<f32> = (0..256).map(|i| ((i * 37 % 101) as f32 - 50.0) / 50.0).collect();
        normalize(&mut v);
        let b = quantize(&v);
        assert_eq!(b.len(), 260);
        assert!((score(&b, &v).unwrap() - 1.0).abs() < 0.01);
        assert_eq!(score(&b, &v[..10]), None);
        assert_eq!(score(&quantize(&[0.0; 4]), &[1.0; 4]), Some(0.0));
    }

    #[test]
    fn top_k_keeps_the_best() {
        let t = top_k([(0.1, 'a'), (0.9, 'b'), (0.5, 'c'), (0.7, 'd'), (0.2, 'e')], 3);
        assert_eq!(t.iter().map(|x| x.1).collect::<String>(), "bdc");
        assert!(top_k([(1.0, 'a')], 0).is_empty());
    }

    #[test]
    fn fusion_rewards_agreement() {
        let f = fuse(&["a", "b", "c"], &["c", "d"]);
        let order: Vec<&str> = f.iter().map(|x| x.0).collect();
        // c is in both lists, so first; then by rank, the keyword list's first on a tie.
        assert_eq!(order, ["c", "a", "b", "d"]);
        assert_eq!(f[0].1, Found::Both);
        assert_eq!(f[3].1, Found::Meaning);
        assert_eq!(f[1].1.as_str(), "words");
        assert!(fuse::<&str>(&[], &[]).is_empty());
    }

    #[test]
    fn openings_cut_at_a_word() {
        assert_eq!(opening("  short\n text ", 40), "short text");
        assert_eq!(opening("the quick brown fox jumps", 12), "the quick…");
        assert_eq!(opening("abcdefghijklmnop", 5), "abcde…");
    }

    #[test]
    fn the_pinned_files() {
        assert_eq!(total_bytes(), 31_140_116);
        assert_eq!(
            url("https://huggingface.co/", "vocab.txt"),
            "https://huggingface.co/minishlab/potion-base-8M/resolve/bf8b056651a2c21b8d2565580b8569da283cab23/vocab.txt"
        );
    }
}
