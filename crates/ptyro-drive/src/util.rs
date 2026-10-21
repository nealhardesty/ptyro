//! Small helpers the drivers share.

use ptyro_core::chat::{ChatBody, ChatItem, NoticeLevel};
use ptyro_core::status::{AgentStatus, Signal};
use serde_json::Value;

use crate::Effect;

pub fn item(id: impl Into<String>, body: ChatBody) -> Effect {
    Effect::Item(ChatItem::new(id, body))
}

pub fn status(s: AgentStatus) -> Effect {
    Effect::Signal(Signal::status(s))
}

pub fn notice(id: impl Into<String>, level: NoticeLevel, text: impl Into<String>) -> Effect {
    item(id, ChatBody::Notice { level, text: text.into() })
}

/// The text of a content value: a string, `{type:"text",text}`, or an array of those.
pub fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            a.iter().map(text_of).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
        }
        Value::Object(o) => match o.get("text") {
            Some(Value::String(s)) => s.clone(),
            _ => o.get("content").map(text_of).unwrap_or_default(),
        },
        _ => String::new(),
    }
}

/// A unified-looking diff of two texts (whole-file replacement, which is what ACP's
/// `{oldText, newText}` gives).
pub fn diff(path: &str, old: Option<&str>, new: &str) -> String {
    let mut out = format!("--- {path}\n+++ {path}\n");
    let old_lines: Vec<&str> = old.map(|o| o.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();
    // Common prefix and suffix, then the changed middle.
    let pre = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let suf = old_lines[pre..]
        .iter()
        .rev()
        .zip(new_lines[pre..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        pre + 1,
        old_lines.len() - pre - suf,
        pre + 1,
        new_lines.len() - pre - suf
    ));
    for l in &old_lines[pre..old_lines.len() - suf] {
        out.push_str(&format!("-{l}\n"));
    }
    for l in &new_lines[pre..new_lines.len() - suf] {
        out.push_str(&format!("+{l}\n"));
    }
    out
}

/// First line, at most `n` characters.
pub fn one_line(s: &str, n: usize) -> String {
    let l = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if l.chars().count() > n {
        format!("{}…", l.chars().take(n).collect::<String>())
    } else {
        l.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_and_text() {
        let d = diff("a.txt", Some("one\ntwo\nthree\n"), "one\n2\nthree\n");
        assert!(d.contains("@@ -2,1 +2,1 @@\n-two\n+2\n"), "{d}");
        assert!(diff("n.txt", None, "x\n").contains("+x"));
        assert_eq!(text_of(&serde_json::json!([{"type": "text", "text": "a"}, "b"])), "a\nb");
        assert_eq!(one_line("\n  hello world  \nmore", 5), "hello…");
    }
}
