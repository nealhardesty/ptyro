//! Settings an older ptyro read and this one doesn't (SPEC §13.8): what a feature that was
//! removed leaves behind in a config file. `Config` and `ProjectConfig` refuse keys they don't
//! know, so a typo is caught; a key in this table is accepted instead, ignored, and reported
//! (`ptyro config check`, `ptyro doctor`, the TUI's and the web's health views) with its fix:
//! `ptyro config unset KEY` takes the line out.
//!
//! A batch that removes a feature adds its keys here; nothing is ever taken out of the table.

use serde::Serialize;

/// One retired key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Retired {
    /// Its dotted path (`summaries.agent`), or a table's (`themes`), which retires everything
    /// under it.
    pub key: &'static str,
    /// The day it stopped being read (`2026-11-17`).
    pub removed: &'static str,
    /// Why, in one line.
    pub why: &'static str,
}

/// Every retired key, oldest first. Change sets, the first feature removed (2026-11-17), had
/// no settings of their own.
pub const RETIRED: &[Retired] = &[
    Retired {
        key: "orchestration.lease_ttl",
        removed: "2026-11-17",
        why: "file leases were removed with the rest of orchestration",
    },
    Retired {
        key: "orchestration.radar",
        removed: "2026-11-17",
        why: "the collision radar was removed with the rest of orchestration",
    },
    Retired {
        key: "orchestration.message_ttl",
        removed: "2026-11-17",
        why: "mail between agents was removed with the rest of orchestration",
    },
    Retired {
        key: "github",
        removed: "2026-11-17",
        why: "pull requests, checks and their watcher were removed; `gh` is used only to start work from an issue",
    },
    Retired {
        key: "review",
        removed: "2026-11-17",
        why: "the review queue and its hold on new sessions were removed",
    },
    Retired {
        key: "worktrees.archive_on_merge",
        removed: "2026-11-17",
        why: "it archived a worktree when its pull request merged, and pull requests were removed",
    },
    Retired {
        key: "verify",
        removed: "2026-11-17",
        why: "verified-done (a check an agent's turn had to pass) was removed",
    },
    Retired {
        key: "scratchpad",
        removed: "2026-11-17",
        why: "the scratchpad was removed; your notes file is yours and stays where it is",
    },
    Retired {
        key: "worktrees.archive_purge_days",
        removed: "2026-11-17",
        why: "archived worktrees were removed; `ptyro wt rm` removes one, `ptyro gc` lists the stale ones",
    },
    Retired {
        key: "worktrees.auto_rename_branch",
        removed: "2026-11-17",
        why: "renaming a worktree's branch was removed; `git branch -m` renames one",
    },
    Retired {
        key: "ports",
        removed: "2026-11-17",
        why: "port blocks for worktrees were removed; scripts no longer get PTYRO_PORT, so pick ports in your own scripts",
    },
    Retired {
        key: "ui.show_ports",
        removed: "2026-11-17",
        why: "listening-port detection was removed",
    },
    Retired {
        key: "agents.*.after_turn",
        removed: "2026-11-17",
        why: "after_turn scripts were removed; run a linter from your agent's own hooks",
    },
    Retired {
        key: "scripts.after_turn",
        removed: "2026-11-17",
        why: "after_turn scripts were removed; run a linter from your agent's own hooks",
    },
    Retired {
        key: "project.default_agent",
        removed: "2026-11-17",
        why: "only quick new (`new --again`) read it, and quick new was removed; `[agents] default` picks the agent new sessions start",
    },
    Retired {
        key: "scripts.archive",
        removed: "2026-11-17",
        why: "archived worktrees, and the script that ran when one was archived or removed, were removed",
    },
    Retired {
        key: "summaries",
        removed: "2026-11-17",
        why: "session summaries were removed; a chat's titler is `chat.title_command`",
    },
];

/// A key a test retires, in this crate's tests and, in a debug build, when
/// `PTYRO_TEST_RETIRED` names it (the CLI's tests).
const TEST_ONLY: Retired =
    Retired { key: "retired_for_tests", removed: "2026-11-17", why: "a test's own key" };

/// The table, with the test's key where a test asks for it.
fn table() -> Vec<Retired> {
    let mut v = RETIRED.to_vec();
    if cfg!(test) || crate::test_knob("PTYRO_TEST_RETIRED").is_some() {
        v.push(TEST_ONLY);
    }
    v
}

/// A retired key a file sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Found {
    /// The path as the file has it: the entry's own, or a table under a retired table's.
    pub key: String,
    pub removed: &'static str,
    pub why: &'static str,
}

impl Found {
    /// What to tell the person, ending with the fix (`unset` is the command that removes it,
    /// with its `-p` when the key is in a repository's file).
    pub fn note(&self, unset: &str) -> String {
        format!(
            "{} is no longer used (removed {}: {}): remove it, `{unset}`",
            self.key, self.removed, self.why
        )
    }
}

/// The entry that retires `key` (the key itself, or a table above it). A `*` in an entry's
/// path stands for any one name (`agents.*.after_turn`).
pub fn find(key: &str) -> Option<Retired> {
    let segs: Vec<&str> = key.split('.').collect();
    table().into_iter().find(|r| {
        let pat: Vec<&str> = r.key.split('.').collect();
        segs.len() >= pat.len() && pat.iter().zip(&segs).all(|(p, s)| *p == "*" || p == s)
    })
}

/// The retired keys `doc` sets, each once (a retired table is reported as the table), with
/// each `*` named as the file has it.
fn scan(doc: &toml_edit::DocumentMut) -> Vec<Found> {
    fn walk(item: &toml_edit::Item, pat: &[&str], at: &mut Vec<String>, out: &mut Vec<String>) {
        let Some((seg, rest)) = pat.split_first() else {
            out.push(at.join("."));
            return;
        };
        let Some(t) = item.as_table_like() else { return };
        for (name, next) in t.iter() {
            if *seg == "*" || *seg == name {
                at.push(name.to_owned());
                walk(next, rest, at, out);
                at.pop();
            }
        }
    }
    let mut out = Vec::new();
    for r in table() {
        let mut keys = Vec::new();
        walk(doc.as_item(), &r.key.split('.').collect::<Vec<_>>(), &mut Vec::new(), &mut keys);
        out.extend(keys.into_iter().map(|key| Found { key, removed: r.removed, why: r.why }));
    }
    out
}

/// The retired keys in a file's text. Text that isn't TOML has none (its error is the parser's
/// to report).
pub fn found(text: &str) -> Vec<Found> {
    text.parse::<toml_edit::DocumentMut>().map(|d| scan(&d)).unwrap_or_default()
}

/// `text` without its retired keys (comments and everything else kept), and what was taken out;
/// `None` when it has none, so the parser reads the file itself and its errors point at its
/// lines.
pub fn strip(text: &str) -> Option<(String, Vec<Found>)> {
    let mut doc: toml_edit::DocumentMut = text.parse().ok()?;
    let found = scan(&doc);
    if found.is_empty() {
        return None;
    }
    for f in &found {
        remove(&mut doc, &f.key);
    }
    Some((doc.to_string(), found))
}

/// The same for a table already parsed (a repository's files, which are merged as tables).
pub fn strip_table(t: &mut toml::Table) -> Vec<Found> {
    /// Take what `segs` names out of `t`, pushing each path taken (`at` is where `t` is).
    fn take(t: &mut toml::Table, segs: &[&str], at: &str, out: &mut Vec<String>) {
        let path =
            |name: &str| if at.is_empty() { name.to_owned() } else { format!("{at}.{name}") };
        match segs {
            [] => {}
            [last] => {
                let names: Vec<String> =
                    t.keys().filter(|k| *last == "*" || last == k).cloned().collect();
                for n in names {
                    t.remove(&n);
                    out.push(path(&n));
                }
            }
            [first, rest @ ..] => {
                for (n, v) in t.iter_mut().filter(|(k, _)| *first == "*" || first == k) {
                    if let Some(sub) = v.as_table_mut() {
                        take(sub, rest, &path(n), out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for r in table() {
        let mut keys = Vec::new();
        take(t, &r.key.split('.').collect::<Vec<_>>(), "", &mut keys);
        out.extend(keys.into_iter().map(|key| Found { key, removed: r.removed, why: r.why }));
    }
    out
}

/// Take `key` out of `doc`; `true` if it was there. Empty tables it leaves stay (a header with
/// nothing under it is harmless, and may carry the person's comments).
pub fn remove(doc: &mut toml_edit::DocumentMut, key: &str) -> bool {
    let segs: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = segs.split_last() else { return false };
    let mut at: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in parents {
        match at.get_mut(p).and_then(toml_edit::Item::as_table_like_mut) {
            Some(next) => at = next,
            None => return false,
        }
    }
    at.remove(last).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retired_key_is_found_and_taken_out_with_the_rest_kept() {
        let text = "# mine\n[ui]\ntheme = \"ptyro-light\"\n\n[retired_for_tests]\nold = 1\n";
        assert_eq!(
            found(text).iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            ["retired_for_tests"]
        );
        let (out, gone) = strip(text).expect("it has one");
        assert_eq!(gone.len(), 1);
        assert!(out.contains("# mine") && out.contains("theme = \"ptyro-light\""), "{out}");
        assert!(!out.contains("retired_for_tests") && !out.contains("old"), "{out}");
        assert!(strip("[ui]\ntheme = \"ptyro-light\"\n").is_none(), "nothing to take out");
        assert!(found("not = [toml").is_empty());
        assert!(find("retired_for_tests.old").is_some() && find("retired_for_testsx").is_none());
        let note = gone[0].note("ptyro config unset retired_for_tests");
        assert!(
            note.contains("no longer used")
                && note.ends_with("`ptyro config unset retired_for_tests`"),
            "{note}"
        );
    }

    /// An old config that still sets a removed feature's keys loads, as does a repository's;
    /// a key that was never ptyro's is still an error, with its line.
    #[test]
    fn an_old_config_loads_and_a_typo_still_fails() {
        let old = "[retired_for_tests]\nold = 1\n\n[ui]\ntheme = \"ptyro-light\"\n";
        let c = crate::config::Config::parse(old).expect("a retired key is ignored");
        assert_eq!(c.ui.theme, "ptyro-light");
        assert!(crate::project::ProjectConfig::parse("retired_for_tests = true\n").is_ok());
        let e = crate::config::Config::parse("[ui]\nthemme = \"x\"\n").unwrap_err();
        assert!(e.contains("unknown field `themme`") && e.contains("line 2"), "{e}");
        let e = crate::config::Config::parse("[retired_for_tests]\nold = 1\n[ui]\nthemme = 1\n")
            .unwrap_err();
        assert!(e.contains("themme"), "a typo beside a retired key is still caught: {e}");
    }

    /// The orchestration settings (2026-11-17) load and are reported; the checkpoint switch
    /// beside them is still read.
    #[test]
    fn the_orchestration_keys_are_retired_and_checkpoints_stay() {
        let old = "[orchestration]\ncheckpoints = false\nlease_ttl = \"30m\"\nradar = true\nmessage_ttl = \"24h\"\n";
        let c = crate::config::Config::parse(old).expect("an old config loads");
        assert!(!c.checkpoints());
        let keys: Vec<String> = found(old).into_iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            ["orchestration.lease_ttl", "orchestration.radar", "orchestration.message_ttl"]
        );
        let (out, _) = strip(old).unwrap();
        assert!(out.contains("checkpoints = false") && !out.contains("radar"), "{out}");
        let c = crate::config::Config::parse(
            "[notifications]\non = [\"blocked\", \"gate\", \"run\"]\n",
        )
        .expect("the notification kinds runs had are ignored, not an error");
        assert!(c.notifications.on.contains(&"blocked".to_owned()));
    }

    /// The settings of pull requests, review and verified-done (2026-11-17) load and are
    /// reported, an empty `[github]` header included; a repository's `[verify]` too.
    #[test]
    fn the_ship_and_review_keys_are_retired() {
        let old = "[github]\npoll = \"60s\"\ndraft = true\nmerge_method = \"squash\"\nwatch = true\nescalate_after = \"2h\"\nlog_lines = 80\n\n[review]\nmax_unreviewed = 2\ndigest = \"30m\"\n\n[worktrees]\narchive_on_merge = true\nfetch = false\n";
        let c = crate::config::Config::parse(old).expect("an old config loads");
        assert!(!c.worktrees.fetch, "the rest of [worktrees] is still read");
        let keys: Vec<String> = found(old).into_iter().map(|f| f.key).collect();
        assert_eq!(keys, ["github", "review", "worktrees.archive_on_merge"]);
        assert!(find("github.merge_method").is_some() && find("review.digest").is_some());
        let (out, _) = strip(old).unwrap();
        assert!(!out.contains("github") && out.contains("fetch = false"), "{out}");
        crate::config::Config::parse("[github]\n").expect("an empty header left behind loads");
        let repo = "[verify]\ncheck = \"test\"\nmax_bounces = 3\nauto = true\n";
        assert!(!crate::project::ProjectConfig::parse(repo).unwrap().contains_key("verify"));
        assert_eq!(found(repo)[0].key, "verify");
    }

    /// The scratchpad's settings (2026-11-17) load and are reported; a notification list that
    /// still names the inbox's channel loads too, and an editor of `[editors]` is still read.
    #[test]
    fn the_scratchpad_keys_are_retired_and_the_inbox_channel_is_ignored() {
        let old = "[scratchpad]\npath = \"~/Sync/pad.md\"\nmode = \"popup\"\neditor = \"hx\"\nkeep = 30\ntick_on_finish = true\n\n[editors]\ndefault = \"zed\"\n\n[notifications]\nchannels = [\"inbox\", \"osc\"]\n";
        let c = crate::config::Config::parse(old).expect("an old config loads");
        assert!(c.editors.contains_key("default"), "[editors] is still read");
        assert!(c.notifications.channels.contains(&"osc".to_owned()));
        assert_eq!(found(old).into_iter().map(|f| f.key).collect::<Vec<_>>(), ["scratchpad"]);
        assert!(find("scratchpad.tick_on_finish").is_some());
        let (out, _) = strip(old).unwrap();
        assert!(!out.contains("pad.md") && out.contains("default = \"zed\""), "{out}");
        assert!(!crate::config::Config::default().notifications.channels.contains(&"inbox".into()));
    }

    /// The worktree extras' settings (2026-11-17) load and are reported: archives, ports, branch
    /// names from the first prompt, `after_turn` and `archive` scripts, a project's default
    /// agent. Each agent's `after_turn` is found under its own name, and its other keys stay.
    #[test]
    fn the_worktree_extras_keys_are_retired() {
        let old = "[worktrees]\narchive_purge_days = 30\nauto_rename_branch = true\nstale_days = 7\n\n[ports]\nrange = \"20000-29999\"\ncount = 10\n\n[ui]\nshow_ports = false\nclock = false\n\n[agents.claude]\nafter_turn = false\n\n[agents.codex]\nafter_turn = true\nusage = true\n";
        let c = crate::config::Config::parse(old).expect("an old config loads");
        assert_eq!(c.worktrees.stale_days, 7, "the rest of [worktrees] is still read");
        assert!(!c.ui.clock);
        let keys: Vec<String> = found(old).into_iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            [
                "worktrees.archive_purge_days",
                "worktrees.auto_rename_branch",
                "ports",
                "ui.show_ports",
                "agents.claude.after_turn",
                "agents.codex.after_turn"
            ]
        );
        assert!(
            find("agents.claude.after_turn").is_some() && find("agents.claude.usage").is_none()
        );
        assert!(find("ports.range").is_some());
        let (out, _) = strip(old).unwrap();
        assert!(!out.contains("after_turn") && out.contains("usage = true"), "{out}");
        let repo = "[project]\ndefault_agent = \"codex\"\nbase = \"origin/main\"\n\n[scripts]\nsetup = \"make\"\narchive = \"down\"\nafter_turn = \"lint\"\n";
        let keys: Vec<String> = found(repo).into_iter().map(|f| f.key).collect();
        assert_eq!(keys, ["scripts.after_turn", "project.default_agent", "scripts.archive"]);
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(crate::project::FILE), repo).unwrap();
        let pc = crate::project::ProjectConfig::load(d.path(), d.path())
            .expect("an old repository file loads");
        assert_eq!(pc.scripts.setup.as_deref(), Some("make"));
        assert_eq!(pc.project.base.as_deref(), Some("origin/main"));
    }

    /// Summaries (2026-11-17) load and are reported; a chat's titler is its own setting now,
    /// with the command the summaries had as its default.
    #[test]
    fn the_summaries_are_retired_and_the_chat_titler_stands_alone() {
        let old = "[summaries]\nenabled = true\ndaily_cap = 50\ncommand = [\"my-llm\"]\n\n[chat]\ntitle = \"auto\"\n";
        let c = crate::config::Config::parse(old).expect("an old config loads");
        assert_eq!(c.chat.title, "auto");
        assert_eq!(found(old).into_iter().map(|f| f.key).collect::<Vec<_>>(), ["summaries"]);
        assert!(find("summaries.command").is_some());
        assert_eq!(crate::config::Config::default().chat.title_command[..2], ["claude", "-p"]);
    }

    #[test]
    fn a_parsed_table_loses_its_retired_keys_too() {
        let mut t: toml::Table = toml::from_str("retired_for_tests = 1\nkeep = 2\n").unwrap();
        assert_eq!(strip_table(&mut t).len(), 1);
        assert_eq!(t.keys().collect::<Vec<_>>(), ["keep"]);
    }
}
