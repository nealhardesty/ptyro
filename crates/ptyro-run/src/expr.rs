//! The condition language for `if = "…"`: deliberately tiny and not Turing-complete.
//!
//! ```text
//! expr  := or
//! or    := and ("||" and)*
//! and   := not ("&&" not)*
//! not   := "!" not | cmp
//! cmp   := atom (("==" | "!=" | "<" | "<=" | ">" | ">=" | "contains") atom)?
//! atom  := "(" expr ")" | 'str' | "str" | number | true | false | null | path
//! path  := ident ("." ident)*        e.g. steps.review.outputs.decision
//! ```
//!
//! Values are JSON. A missing path is `null`; `null`, `false`, `""`, `0` and `[]` are false.
//! Numbers compare as numbers, and a string that parses as a number compares as one too (exit
//! codes arrive as either).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Path(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp(Box<Expr>, Op, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    Ident(String),
    Op(Op),
    And,
    Or,
    Not,
    Open,
    Close,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        match ch {
            ' ' | '\t' | '\n' => i += 1,
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            '&' if next == Some('&') => {
                out.push(Tok::And);
                i += 2;
            }
            '|' if next == Some('|') => {
                out.push(Tok::Or);
                i += 2;
            }
            '=' if next == Some('=') => {
                out.push(Tok::Op(Op::Eq));
                i += 2;
            }
            '!' if next == Some('=') => {
                out.push(Tok::Op(Op::Ne));
                i += 2;
            }
            '!' => {
                out.push(Tok::Not);
                i += 1;
            }
            '<' | '>' => {
                let eq = next == Some('=');
                out.push(Tok::Op(match (ch, eq) {
                    ('<', true) => Op::Le,
                    ('<', false) => Op::Lt,
                    (_, true) => Op::Ge,
                    _ => Op::Gt,
                }));
                i += if eq { 2 } else { 1 };
            }
            '\'' | '"' => {
                let mut j = i + 1;
                let mut text = String::new();
                while j < c.len() && c[j] != ch {
                    if c[j] == '\\' && j + 1 < c.len() {
                        j += 1;
                    }
                    text.push(c[j]);
                    j += 1;
                }
                if j >= c.len() {
                    return Err(format!("unterminated string at column {}", i + 1));
                }
                out.push(Tok::Str(text));
                i = j + 1;
            }
            d if d.is_ascii_digit() || (d == '-' && next.is_some_and(|n| n.is_ascii_digit())) => {
                let mut j = i + 1;
                while j < c.len() && (c[j].is_ascii_digit() || c[j] == '.') {
                    j += 1;
                }
                let t: String = c[i..j].iter().collect();
                out.push(Tok::Num(t.parse().map_err(|_| format!("bad number {t:?}"))?));
                i = j;
            }
            a if a.is_ascii_alphabetic() || a == '_' => {
                let mut j = i + 1;
                while j < c.len() && (c[j].is_ascii_alphanumeric() || "_-.".contains(c[j])) {
                    j += 1;
                }
                let t: String = c[i..j].iter().collect();
                out.push(if t == "contains" { Tok::Op(Op::Contains) } else { Tok::Ident(t) });
                i = j;
            }
            other => return Err(format!("unexpected {other:?} at column {}", i + 1)),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.at += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.peek() == Some(&Tok::And) {
            self.at += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Tok::Not) {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.atom()?;
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            self.at += 1;
            let r = self.atom()?;
            return Ok(Expr::Cmp(Box::new(l), op, Box::new(r)));
        }
        Ok(l)
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Open) => {
                let e = self.or()?;
                match self.next() {
                    Some(Tok::Close) => Ok(e),
                    _ => Err("missing )".into()),
                }
            }
            Some(Tok::Str(s)) => Ok(Expr::Lit(Value::String(s))),
            Some(Tok::Num(n)) => Ok(Expr::Lit(serde_json::json!(n))),
            Some(Tok::Ident(i)) => Ok(match i.as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                _ => {
                    if i.ends_with('.') || i.contains("..") {
                        return Err(format!("bad path {i:?}"));
                    }
                    Expr::Path(i)
                }
            }),
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("the condition ends too early".into()),
        }
    }
}

pub fn parse(s: &str) -> Result<Expr, String> {
    let toks = lex(s)?;
    if toks.is_empty() {
        return Err("empty condition".into());
    }
    let mut p = Parser { toks, at: 0 };
    let e = p.or()?;
    if p.at < p.toks.len() {
        return Err(format!("unexpected {:?} after the condition", p.toks[p.at]));
    }
    Ok(e)
}

/// Look `path` (`a.b.c`) up in `ctx`; missing → `null`.
pub fn lookup(ctx: &Value, path: &str) -> Value {
    let mut v = ctx;
    for part in path.split('.') {
        v = match v {
            Value::Object(m) => match m.get(part) {
                Some(x) => x,
                None => return Value::Null,
            },
            Value::Array(a) => match part.parse::<usize>().ok().and_then(|i| a.get(i)) {
                Some(x) => x,
                None => return Value::Null,
            },
            _ => return Value::Null,
        };
    }
    v.clone()
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

fn eq(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) if !matches!((a, b), (Value::String(_), Value::String(_))) => x == y,
        _ => a == b,
    }
}

impl Expr {
    pub fn eval(&self, ctx: &Value) -> Value {
        match self {
            Self::Lit(v) => v.clone(),
            Self::Path(p) => lookup(ctx, p),
            Self::Not(e) => Value::Bool(!truthy(&e.eval(ctx))),
            Self::And(a, b) => Value::Bool(truthy(&a.eval(ctx)) && truthy(&b.eval(ctx))),
            Self::Or(a, b) => Value::Bool(truthy(&a.eval(ctx)) || truthy(&b.eval(ctx))),
            Self::Cmp(a, op, b) => {
                let (x, y) = (a.eval(ctx), b.eval(ctx));
                Value::Bool(match op {
                    Op::Eq => eq(&x, &y),
                    Op::Ne => !eq(&x, &y),
                    Op::Contains => match (&x, &y) {
                        (Value::String(s), Value::String(t)) => s.contains(t.as_str()),
                        (Value::Array(a), v) => a.iter().any(|e| eq(e, v)),
                        _ => false,
                    },
                    cmp => match (num(&x), num(&y)) {
                        (Some(p), Some(q)) => match cmp {
                            Op::Lt => p < q,
                            Op::Le => p <= q,
                            Op::Gt => p > q,
                            _ => p >= q,
                        },
                        _ => false,
                    },
                })
            }
        }
    }

    pub fn holds(&self, ctx: &Value) -> bool {
        truthy(&self.eval(ctx))
    }

    /// Every path it reads (validation checks them against the recipe).
    pub fn paths(&self) -> Vec<String> {
        match self {
            Self::Lit(_) => vec![],
            Self::Path(p) => vec![p.clone()],
            Self::Not(e) => e.paths(),
            Self::And(a, b) | Self::Or(a, b) | Self::Cmp(a, _, b) => {
                let mut v = a.paths();
                v.extend(b.paths());
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn holds(e: &str, ctx: &Value) -> bool {
        parse(e).unwrap().holds(ctx)
    }

    #[test]
    fn conditions_evaluate() {
        let ctx = json!({
            "steps": { "review": { "status": "succeeded", "exit_code": 0, "outputs": { "decision": "changes", "issues": ["a", "b"] } },
                       "test": { "exit_code": "1" } },
            "gates": { "g": { "choice": "pr" } },
        });
        assert!(holds("steps.review.outputs.decision == 'changes'", &ctx));
        assert!(holds("steps.review.outputs.decision != \"approve\"", &ctx));
        assert!(holds("steps.test.exit_code != 0 && gates.g.choice == 'pr'", &ctx));
        assert!(holds("steps.test.exit_code == 1", &ctx), "string exit codes compare as numbers");
        assert!(holds("!(steps.missing.status == 'succeeded') || false", &ctx));
        assert!(holds("steps.review.outputs.issues contains 'b'", &ctx));
        assert!(holds("steps.review.outputs.issues", &ctx));
        assert!(!holds("steps.nope.outputs.x", &ctx));
        assert!(holds("steps.review.exit_code < 2 && steps.review.exit_code >= 0", &ctx));
        assert!(holds("'10' != '10.0'", &ctx), "two strings compare as text");
    }

    #[test]
    fn bad_conditions_are_explained() {
        assert!(parse("a ==").unwrap_err().contains("ends too early"));
        assert!(parse("(a").unwrap_err().contains("missing )"));
        assert!(parse("'open").unwrap_err().contains("unterminated"));
        assert!(parse("a b").unwrap_err().contains("after the condition"));
        assert!(parse("a = b").unwrap_err().contains("unexpected"));
        assert_eq!(parse("a.b == c").unwrap().paths(), vec!["a.b", "c"]);
    }
}
