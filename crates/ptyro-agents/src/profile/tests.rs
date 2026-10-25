//! Table-driven tests for profile resolution and environment construction (M3.17).

use super::*;

const CONFIG: &str = r#"
[profiles.work]
label = "work"
color = "blue"
claude = "/acct/claude-work"
codex = { dir = "/acct/codex-work", api_key = { command = ["op", "read", "codex"] } }
git = { name = "Work Me", email = "me@acme.example", ssh_key = "/keys/id work" }
gh = { token = "ghp_literal" }
env = { AWS_PROFILE = "acme" }
secrets = { NPM_TOKEN = { command = ["pass", "npm"] } }
match = { remotes = ["github.com:acme/*"], paths = ["/work/**"] }
scrub_extra = ["MY_CORP_TOKEN"]

[profiles.personal]
claude = "/acct/claude-me"
match = { remotes = ["github.com/me/*"] }
"#;

fn config() -> Config {
    Config::parse(CONFIG).unwrap()
}

fn secrets(label: &str, s: &Secret) -> Result<String, String> {
    match s {
        Secret::Literal(v) => Ok(v.clone()),
        Secret::Command { command } => Ok(format!("resolved:{}:{label}", command.join(" "))),
    }
}

fn env_of(c: &Config, profile: &str, base: &[(&str, &str)]) -> BTreeMap<String, String> {
    let p = plan(c, profile, Path::new("/data/profiles"), &mut secrets).unwrap();
    p.apply(base.iter().map(|(k, v)| ((*k).into(), (*v).into())))
        .into_iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect()
}

const LEAKY_BASE: &[(&str, &str)] = &[
    ("HOME", "/home/u"),
    ("CLAUDE_CONFIG_DIR", "/leaked/claude"),
    ("ANTHROPIC_API_KEY", "sk-leak"),
    ("GIT_AUTHOR_NAME", "Leaky"),
    ("GIT_COMMITTER_DATE", "yesterday"),
    ("MY_CORP_TOKEN", "corp"),
    ("SSH_AUTH_SOCK", "/agent.sock"),
];

#[test]
fn resolution_order() {
    let cases: &[(Inputs<'_>, &str, ProfileReason)] = &[
        (Inputs::default(), "default", ProfileReason::Default),
        (Inputs { workspace: Some("w"), ..Inputs::default() }, "w", ProfileReason::Workspace),
        (
            Inputs { pin: Some("p"), workspace: Some("w"), ..Inputs::default() },
            "p",
            ProfileReason::Pinned,
        ),
        (
            Inputs {
                worktree: Some("t"),
                pin: Some("p"),
                workspace: Some("w"),
                ..Inputs::default()
            },
            "t",
            ProfileReason::Worktree,
        ),
        (
            Inputs {
                explicit: Some("e"),
                worktree: Some("t"),
                pin: Some("p"),
                workspace: Some("w"),
            },
            "e",
            ProfileReason::Explicit,
        ),
    ];
    for (i, want, reason) in cases {
        assert_eq!(resolve(*i), ((*want).to_owned(), *reason), "{i:?}");
    }
    assert_eq!(
        contradicts(Inputs { explicit: Some("personal"), pin: Some("work"), ..Inputs::default() }),
        Some(("work".into(), ProfileReason::Pinned))
    );
    assert_eq!(
        contradicts(Inputs { explicit: Some("work"), pin: Some("work"), ..Inputs::default() }),
        None
    );
    assert_eq!(contradicts(Inputs { pin: Some("work"), ..Inputs::default() }), None);
}

#[test]
fn every_profile_scrubs_managed_vars() {
    let c = config();
    for profile in ["default", "work", "personal"] {
        let env = env_of(&c, profile, LEAKY_BASE);
        assert_ne!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/leaked/claude"),
            "{profile}"
        );
        assert_ne!(env.get("GIT_AUTHOR_NAME").map(String::as_str), Some("Leaky"), "{profile}");
        assert!(!env.contains_key("GIT_COMMITTER_DATE"), "{profile}");
        assert_eq!(env.get("PTYRO_PROFILE").map(String::as_str), Some(profile));
        assert_eq!(env.get("SSH_AUTH_SOCK").map(String::as_str), Some("/agent.sock"), "kept");
    }
}

#[test]
fn isolation_rules() {
    // No profiles at all: default inherits credentials.
    let none = Config::default();
    let env = env_of(&none, "default", LEAKY_BASE);
    assert_eq!(env.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-leak"));
    assert!(!env.contains_key("CLAUDE_CONFIG_DIR"), "managed vars go even then");
    // Once any profile exists, default isolates too.
    let c = config();
    assert!(!env_of(&c, "default", LEAKY_BASE).contains_key("ANTHROPIC_API_KEY"));
    let work = env_of(&c, "work", LEAKY_BASE);
    assert!(!work.contains_key("ANTHROPIC_API_KEY"));
    assert!(!work.contains_key("MY_CORP_TOKEN"), "scrub_extra");
    // Opting out.
    let c = Config::parse(&format!("{CONFIG}\n[profiles.default]\nisolate = false\n")).unwrap();
    assert_eq!(
        env_of(&c, "default", LEAKY_BASE).get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-leak")
    );
}

#[test]
fn work_profile_environment() {
    let c = config();
    let env = env_of(&c, "work", LEAKY_BASE);
    let get = |k: &str| env.get(k).map(String::as_str);
    assert_eq!(get("CLAUDE_CONFIG_DIR"), Some("/acct/claude-work"));
    assert_eq!(get("CODEX_HOME"), Some("/acct/codex-work"));
    assert_eq!(get("OPENAI_API_KEY"), Some("resolved:op read codex:profiles.work.codex.api_key"));
    assert_eq!(get("CODEX_API_KEY"), get("OPENAI_API_KEY"));
    // Omitted agents get a managed dir, never the agent's default one.
    assert_eq!(get("GEMINI_CLI_HOME"), Some("/data/profiles/work/gemini"));
    assert_eq!(get("GH_CONFIG_DIR"), Some("/data/profiles/work/gh"));
    assert_eq!(get("GH_TOKEN"), Some("ghp_literal"));
    assert_eq!(get("GIT_AUTHOR_NAME"), Some("Work Me"));
    assert_eq!(get("GIT_COMMITTER_EMAIL"), Some("me@acme.example"));
    assert_eq!(get("GIT_SSH_COMMAND"), Some("ssh -i '/keys/id work' -o IdentitiesOnly=yes"));
    assert_eq!(get("AWS_PROFILE"), Some("acme"), "the profile's own env wins over the scrub");
    assert_eq!(get("NPM_TOKEN"), Some("resolved:pass npm:profiles.work.secrets.NPM_TOKEN"));
}

#[test]
fn secrets_never_appear_in_the_shown_env() {
    let c = config();
    let p = plan(&c, "work", Path::new("/data/profiles"), &mut secrets).unwrap();
    let shown = p.shown_map();
    for k in ["OPENAI_API_KEY", "GH_TOKEN", "NPM_TOKEN"] {
        assert_eq!(shown[k], format!("<secret:{k}>"));
    }
    assert_eq!(shown["CLAUDE_CONFIG_DIR"], "/acct/claude-work");
    assert!(p.create_dirs.contains(&PathBuf::from("/data/profiles/work/gemini")));
    let failing = plan(&c, "work", Path::new("/d"), &mut |label: &str, _: &Secret| {
        Err(format!("{label}: op not signed in"))
    });
    assert!(failing.unwrap_err().contains("profiles.work.codex.api_key"));
    assert!(plan(&c, "nope", Path::new("/d"), &mut secrets).unwrap_err().contains("no profile"));
}

#[test]
fn default_profile_keeps_agent_dirs() {
    let c = config();
    let env = env_of(&c, "default", &[("HOME", "/h")]);
    assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
    assert!(!env.contains_key("CODEX_HOME"));
}

#[test]
fn match_rules_and_claimants() {
    let c = config();
    let p = Path::new("/elsewhere/repo");
    assert_eq!(claimants(&c, &["git@github.com:acme/api.git".into()], p), ["work"]);
    assert_eq!(
        claimants(&c, &["https://github.com/acme/api".into()], p),
        ["work"],
        "either spelling"
    );
    assert_eq!(claimants(&c, &["git@github.com:me/dots.git".into()], p), ["personal"]);
    assert!(claimants(&c, &["git@github.com:other/x.git".into()], p).is_empty());
    assert_eq!(claimants(&c, &[], Path::new("/work/deep/repo")), ["work"]);
}

#[test]
fn env_files() {
    let v = parse_env_file("# c\nexport A=1\nB=\"two words\"\nC='x'\nnoequals\n");
    assert_eq!(
        v,
        [("A".into(), "1".into()), ("B".into(), "two words".into()), ("C".into(), "x".into())]
    );
}

#[test]
fn shares_link_but_never_overwrite_or_share_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let default_claude = home.join(".claude");
    std::fs::create_dir_all(default_claude.join("skills")).unwrap();
    std::fs::write(default_claude.join("CLAUDE.md"), "shared").unwrap();
    std::fs::write(default_claude.join("settings.json"), "default").unwrap();
    let work = home.join("claude-work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("settings.json"), "mine").unwrap();
    let c = Config::parse(&format!(
        "[profiles.work]\nclaude = \"{}\"\nshare = {{ claude = [\"CLAUDE.md\", \"skills\", \"settings.json\", \"missing\"] }}\n",
        work.display()
    ))
    .unwrap();
    let defaults = |a: &str| (a == "claude").then(|| default_claude.clone());
    let out = sync_shares_from(&c, "work", &home.join("managed"), defaults);
    assert!(out.contains(&ShareOutcome::Linked(work.join("CLAUDE.md"))), "{out:?}");
    assert!(out.contains(&ShareOutcome::Linked(work.join("skills"))));
    assert!(out.contains(&ShareOutcome::Exists(work.join("settings.json"))));
    assert!(out.contains(&ShareOutcome::MissingSource(default_claude.join("missing"))));
    assert_eq!(
        std::fs::read_to_string(work.join("settings.json")).unwrap(),
        "mine",
        "never overwritten"
    );
    assert_eq!(std::fs::read_to_string(work.join("CLAUDE.md")).unwrap(), "shared");
    // Idempotent.
    let again = sync_shares_from(&c, "work", &home.join("managed"), defaults);
    assert!(again.contains(&ShareOutcome::AlreadyLinked(work.join("CLAUDE.md"))));
}

#[test]
fn isolation_scrubs_credentials_by_shape_but_not_what_the_profile_sets() {
    let base = [
        ("HOME", "/home/u"),
        ("PATH", "/usr/bin"),
        ("OPENAI_BASE_URL", "https://corp.example/v1"),
        ("OPENAI_ORG_ID", "org-1"),
        ("AZURE_OPENAI_ENDPOINT", "https://x"),
        ("OPENROUTER_API_KEY", "sk-or"),
        ("XAI_API_KEY", "xai"),
        ("MISTRAL_API_KEY", "m"),
        ("GH_HOST", "ghe.example"),
        ("GH_ENTERPRISE_TOKEN", "t"),
        ("GITHUB_ENTERPRISE_TOKEN", "t"),
        ("GIT_ASKPASS", "/bin/pw"),
        ("SSH_ASKPASS", "/bin/pw"),
        ("GIT_SSH", "/bin/ssh2"),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "credential.helper"),
        ("GIT_CONFIG_VALUE_0", "store"),
        ("GIT_CONFIG_PARAMETERS", "'a=b'"),
        ("AWS_SHARED_CREDENTIALS_FILE", "/home/u/.aws/c"),
        ("DATABASE_PASSWORD", "pw"),
        ("SOME_CORP_TOKEN", "t"),
        ("TERM", "xterm"),
        ("EDITOR", "vi"),
    ];
    let c = config();
    let work = env_of(&c, "work", &base);
    for gone in
        base.iter().map(|(k, _)| *k).filter(|k| !["HOME", "PATH", "TERM", "EDITOR"].contains(k))
    {
        assert!(!work.contains_key(gone), "{gone} leaked into an isolated profile");
    }
    for kept in ["HOME", "PATH", "TERM", "EDITOR"] {
        assert!(work.contains_key(kept), "{kept} is not a credential");
    }
    // What the profile itself sets comes after the scrub.
    assert_eq!(work.get("AWS_PROFILE").map(String::as_str), Some("acme"));
    assert_eq!(
        work.get("NPM_TOKEN").map(String::as_str),
        Some("resolved:pass npm:profiles.work.secrets.NPM_TOKEN")
    );
    assert_eq!(work.get("GH_TOKEN").map(String::as_str), Some("ghp_literal"));
    // A profile that opts out of isolation keeps them.
    let open = Config::parse(&format!("{CONFIG}\n[profiles.default]\nisolate = false\n")).unwrap();
    assert!(env_of(&open, "default", &base).contains_key("OPENROUTER_API_KEY"));
}

#[test]
fn credential_files_are_refused_however_they_are_spelled() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let default_claude = home.join(".claude");
    std::fs::create_dir_all(&default_claude).unwrap();
    std::fs::write(default_claude.join(".credentials.json"), "secret").unwrap();
    let work = home.join("claude-work");
    std::fs::create_dir_all(&work).unwrap();
    // (The config parser refuses most of these; the planner must hold on its own.)
    let mut c =
        Config::parse(&format!("[profiles.work]\nclaude = \"{}\"\n", work.display())).unwrap();
    c.profiles.get_mut("work").unwrap().share.insert(
        "claude".into(),
        [
            ".credentials.json/",
            "./.credentials.json",
            ".Credentials.JSON",
            "x/../.credentials.json",
            "../.claude/.credentials.json",
            "/etc/passwd",
            ".",
            "",
        ]
        .map(String::from)
        .to_vec(),
    );
    let defaults = |a: &str| (a == "claude").then(|| default_claude.clone());
    let out = sync_shares_from(&c, "work", &home.join("managed"), defaults);
    assert_eq!(out.len(), 8, "{out:?}");
    assert!(out.iter().all(|o| matches!(o, ShareOutcome::Refused(_))), "{out:?}");
    assert!(!work.join(".credentials.json").exists());
    assert_eq!(share_path("a//b/./c").unwrap(), PathBuf::from("a/b/c"));
}

#[test]
fn env_files_know_quotes_comments_and_bad_names() {
    let v = parse_env_file(
        "A=1 # one\nB=\"x # not a comment\" # but this is\nC='it''s'\nD=\"line\\nbreak \\\"q\\\" \\$HOME\"\n1BAD=x\nBAD KEY=x\nE=http://h/#frag\n=nokey\nexport F=\n",
    );
    let m: BTreeMap<_, _> = v.into_iter().collect();
    assert_eq!(m["A"], "1");
    assert_eq!(m["B"], "x # not a comment");
    assert_eq!(m["C"], "it");
    assert_eq!(m["D"], "line\nbreak \"q\" $HOME");
    assert_eq!(m["E"], "http://h/#frag");
    assert_eq!(m["F"], "");
    assert_eq!(m.len(), 6, "{m:?}");
}
