//! Tiny models in the real one's file layout, for tests and the spike: every code path runs
//! offline, and nothing is downloaded while ptyro is developed.

use std::path::Path;

use crate::safetensors::{self, Matrix};

/// A model in `dir` whose tokens are `[UNK]` (id 0, a zero row) and then `tokens`, each with its
/// row. Every row has the first one's length.
pub fn write(dir: &Path, tokens: &[(&str, &[f32])]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let cols = tokens.first().map_or(1, |t| t.1.len());
    let mut data = vec![0f32; cols];
    let mut vocab = serde_json::Map::new();
    vocab.insert("[UNK]".into(), 0.into());
    let mut txt = String::from("[UNK]\n");
    for (i, (t, row)) in tokens.iter().enumerate() {
        let mut row = row.to_vec();
        row.resize(cols, 0.0);
        data.extend(row);
        vocab.insert((*t).to_owned(), (i + 1).into());
        txt.push_str(t);
        txt.push('\n');
    }
    let m = Matrix { rows: tokens.len() + 1, cols, data };
    std::fs::write(dir.join("model.safetensors"), safetensors::write_f32("embeddings", &m))?;
    std::fs::write(dir.join("vocab.txt"), txt)?;
    let tok = serde_json::json!({
        "version": "1.0",
        "normalizer": { "type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true,
                        "strip_accents": null, "lowercase": true },
        "pre_tokenizer": { "type": "BertPreTokenizer" },
        "model": { "type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##",
                   "max_input_chars_per_word": 100, "vocab": vocab }
    });
    std::fs::write(dir.join("tokenizer.json"), tok.to_string())?;
    std::fs::write(
        dir.join("config.json"),
        format!(r#"{{"model_type": "model2vec", "hidden_dim": {cols}, "normalize": true}}"#),
    )?;
    Ok(())
}

/// A model of `words` tokens `w0`, `w1`… with pseudo-random rows of `dim` (the spike's).
pub fn random(dir: &Path, words: usize, dim: usize, seed: u64) -> std::io::Result<()> {
    let mut rng = Rng(seed | 1);
    let names: Vec<String> = (0..words).map(|i| format!("w{i}")).collect();
    let rows: Vec<Vec<f32>> = (0..words).map(|_| (0..dim).map(|_| rng.f32()).collect()).collect();
    let tokens: Vec<(&str, &[f32])> =
        names.iter().zip(&rows).map(|(n, r)| (n.as_str(), r.as_slice())).collect();
    write(dir, &tokens)
}

/// xorshift64*: enough randomness for a synthetic model, and the same every run.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// In [-1, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}
