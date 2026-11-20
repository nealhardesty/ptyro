//! What ptyro's transcript reader (SPEC §25.6.1) makes of a transcript: `transcript <claude|codex>`
//! reads the file on stdin and prints each part as a JSON line `[kind, text]`. `make verify-kb`
//! runs it over the transcripts the real agents just wrote.

use std::io::Read;

fn main() {
    let agent = std::env::args().nth(1).unwrap_or_default();
    let mut bytes = Vec::new();
    if std::io::stdin().read_to_end(&mut bytes).is_err() {
        std::process::exit(2);
    }
    let Some(t) = ptyro_agents::adapter(&agent).read_transcript(&bytes, 0) else {
        eprintln!("usage: transcript <claude|codex> < file.jsonl");
        std::process::exit(2);
    };
    for p in t.parts {
        println!("{}", serde_json::json!([p.kind.as_str(), p.text]));
    }
}
