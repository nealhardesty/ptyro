//! Key bindings as the panel lists them: every default and every override in one list, each with
//! what it runs in plain words and where it stands, and the one change that edits it (SPEC §13).
//!
//! The defaults belong to the TUI's key map, which core can't see; the caller passes them in
//! [`Lookups::keys`] named as `[keys]` names them (`d`, `nav:j`, `global:M-1`).

use serde::Serialize;
use serde_json::Value;

use super::{Extra, Lookups, Scope};
use crate::config::{key_label, normalize_key, parse_key};

/// Where a binding stands against the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    /// What ptyro ships (or a line in the file that says the same).
    Default,
    /// A key of your own: there is no default for it.
    Yours,
    /// A default key that you made run something else.
    Changed,
    /// Switched off (`"none"`); a default one comes back by resetting it.
    Unbound,
}

impl Standing {
    pub fn says(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Yours => "yours",
            Self::Changed => "changed",
            Self::Unbound => "unbound",
        }
    }
}

/// One key and what it runs.
#[derive(Debug, Clone, Serialize)]
pub struct Binding {
    /// What `[keys]` calls it: `d`, `nav:j`, `global:M-1`. The name every change uses.
    pub name: String,
    /// `prefix`, `nav` or `global`.
    pub table: &'static str,
    /// When it works, in words.
    pub when: &'static str,
    /// The key as shown (`C-d`), without the table.
    pub key: String,
    /// What it runs now, with its argument (`session.focus 2`); `none` when unbound.
    pub action: String,
    /// What it runs out of the box, if it is a default key.
    pub default: Option<String>,
    pub standing: Standing,
    /// Set in the file (a reset removes the line).
    pub mine: bool,
    /// What the action does, in plain words (the default's, while unbound).
    pub help: String,
    /// A group shown as one row (the digits that show session 1 to 9): not edited here.
    pub fixed: bool,
}

/// `(table, spec)` of a `[keys]` name.
fn table_of(name: &str) -> (&'static str, &str) {
    if let Some(s) = name.strip_prefix("global:") {
        ("global", s)
    } else if let Some(s) = name.strip_prefix("nav:") {
        ("nav", s)
    } else {
        ("prefix", name)
    }
}

/// When a table's keys work.
pub fn when(table: &str) -> &'static str {
    match table {
        "nav" => "in the sidebar",
        "global" => "anywhere",
        _ => "after the prefix",
    }
}

/// The key as the key map compares it: `Q`, `S-q` and `shift-q` are one key.
pub fn slot(name: &str) -> (&'static str, String) {
    let (table, spec) = table_of(name);
    let label =
        parse_key(spec).map_or_else(|_| spec.trim().to_owned(), |k| key_label(&normalize_key(k)));
    (table, label)
}

/// The `[keys]` name for `key` in `table`.
pub fn name_in(table: &str, key: &str) -> String {
    match table {
        "nav" => format!("nav:{key}"),
        "global" => format!("global:{key}"),
        _ => key.to_owned(),
    }
}

/// The action's own name, without its argument.
fn word(action: &str) -> &str {
    action.split_whitespace().next().unwrap_or_default()
}

/// What an action does, in plain words.
pub fn describe(lk: &Lookups, action: &str) -> String {
    match word(action) {
        "none" => "Nothing: the key is unbound.".to_owned(),
        w => lk
            .actions
            .iter()
            .find(|(id, _)| id == w)
            .map_or_else(|| format!("{w}: not an action ptyro knows"), |(_, d)| d.clone()),
    }
}

/// Every binding: the defaults, then what the file does to them, in the order the help lists
/// them (by table, then by action). `root` is the effective config as JSON.
pub fn bindings(root: &Value, lk: &Lookups) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    let mut put = |name: &str, value: &str, from_file: bool| {
        let (table, key) = slot(name);
        let value = value.trim().to_owned();
        match out.iter_mut().find(|b| b.table == table && b.key == key) {
            Some(b) => {
                b.name = name.to_owned();
                b.mine = from_file;
                b.standing = match (&b.default, value.as_str()) {
                    (Some(d), v) if *d == v => Standing::Default,
                    (Some(_), "none") => Standing::Unbound,
                    (Some(_), _) => Standing::Changed,
                    (None, "none") => Standing::Unbound,
                    (None, _) => Standing::Yours,
                };
                b.action = value;
            }
            None => out.push(Binding {
                name: name.to_owned(),
                table,
                when: when(table),
                key,
                default: (!from_file).then(|| value.clone()),
                standing: match (from_file, value.as_str()) {
                    (false, _) => Standing::Default,
                    (true, "none") => Standing::Unbound,
                    (true, _) => Standing::Yours,
                },
                action: value,
                mine: from_file,
                help: String::new(),
                fixed: false,
            }),
        }
    };
    for (name, value) in &lk.keys {
        put(name, value, false);
    }
    for (name, value) in super::map_members(root, "keys") {
        put(&name, &value, true);
    }
    for b in &mut out {
        let shown =
            if b.action == "none" { b.default.as_deref().unwrap_or("none") } else { &b.action };
        b.help = describe(lk, shown);
    }
    fold_digits(&mut out);
    let order = |b: &Binding| {
        let a = if b.action == "none" { b.default.as_deref().unwrap_or("") } else { &b.action };
        lk.actions.iter().position(|(id, _)| id == word(a)).unwrap_or(usize::MAX)
    };
    let tables = ["prefix", "nav", "global"];
    out.sort_by_cached_key(|b| {
        (tables.iter().position(|t| *t == b.table), order(b), b.fixed, b.key.clone())
    });
    out
}

/// The nine digit keys that show session 1 to 9 are one row while none of them is touched.
fn fold_digits(out: &mut Vec<Binding>) {
    for table in ["prefix", "nav"] {
        let digit = |b: &Binding| {
            b.table == table
                && b.default.as_deref().is_some_and(|d| d.starts_with("session.focus "))
                && b.key.len() == 1
        };
        let all: Vec<&Binding> = out.iter().filter(|b| digit(b)).collect();
        if all.len() != 9 || all.iter().any(|b| b.mine || b.standing != Standing::Default) {
            continue;
        }
        let first = all[0].clone();
        out.retain(|b| !digit(b));
        out.push(Binding {
            name: name_in(table, "1"),
            key: "1…9".to_owned(),
            action: "session.focus N".to_owned(),
            default: Some("session.focus N".to_owned()),
            fixed: true,
            ..first
        });
    }
}

/// The binding already on `name`'s key that runs something, if any: what a new binding would
/// take the key from.
pub fn owner<'a>(all: &'a [Binding], name: &str) -> Option<&'a Binding> {
    let (table, key) = slot(name);
    all.iter().find(|b| !b.fixed && b.table == table && b.key == key && b.action != "none").or_else(
        || {
            // A folded `1…9` owns its digits.
            all.iter().find(|b| {
                b.fixed
                    && b.table == table
                    && key.len() == 1
                    && key.chars().all(|c| ('1'..='9').contains(&c))
            })
        },
    )
}

/// `text` with each binding changed: `Some(action)` makes the key run it (`"none"` unbinds), `None`
/// takes the key's line out so the default is back. A line that says what the default says is
/// not written, and `"none"` on a key with no default is just removed, so the file only holds
/// what differs.
pub fn bind(
    scope: Scope,
    text: &str,
    changes: &[(String, Option<String>)],
    lk: &Lookups,
    extra: Extra<'_>,
) -> Result<String, String> {
    let mut text = text.to_owned();
    for (name, value) in changes {
        let at = slot(name);
        let default = lk.keys.iter().find(|(n, _)| slot(n) == at).map(|(_, v)| v.trim());
        // A key already in the file under another spelling (`S-q` for `Q`) is that line.
        let own = super::members(&text, "keys").into_iter().find(|m| slot(m) == at);
        let name = own.clone().unwrap_or_else(|| name.clone());
        let want = value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let want = match (want, default) {
            (Some(v), Some(d)) if v == d => None,
            (Some("none"), None) => None,
            (w, _) => w,
        };
        let key = format!("keys.{name}");
        text = match (want, own) {
            (Some(v), _) => super::set(scope, &text, &key, v, lk, extra)?,
            (None, Some(_)) => super::unset(scope, &text, &key, extra)?,
            (None, None) => text,
        };
    }
    Ok(text)
}
