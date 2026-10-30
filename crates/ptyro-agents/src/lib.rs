//! Agent adapters: claude, codex, gemini, opencode, shell, generic (SPEC §8). The only crate with
//! agent-specific knowledge.
//!
//! M3 has what workspaces and profiles need: how to start an agent, which variables point each
//! agent at an account, profile resolution and the environment a session gets (SPEC §10.10), and
//! detection of existing multi-account setups. Adapters proper (hooks, status, resume) are M4.

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod detect;
pub mod glob;
pub mod locate;
pub mod others;
pub mod profile;

use std::path::PathBuf;

use ptyro_core::config::{AgentDef, Config, expand_home};

pub use adapter::{Adapter, Caps, Decision, LaunchCtx, LaunchPlan, Sidecar, StopStep};

/// Agents ptyro knows without configuration: name → the CLI on `PATH`.
pub const BUILTIN: &[(&str, &str)] = &[
    ("claude", "claude"),
    ("codex", "codex"),
    ("gemini", "gemini"),
    ("opencode", "opencode"),
    ("antigravity", "agy"),
];

/// Adapter ids.
pub const ADAPTERS: &[&str] = &["claude", "codex", "gemini", "opencode", "antigravity", "generic"];

/// `claude@work` → (`claude`, `Some("work")`).
pub fn split_agent(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((a, p)) if !p.is_empty() => (a, Some(p)),
        Some((a, _)) => (a, None),
        None => (spec, None),
    }
}

/// The agent's definition (an empty one for built-ins without config).
pub fn def(config: &Config, agent: &str) -> AgentDef {
    config.agents.get(agent).cloned().unwrap_or_default()
}

/// How to start `agent`: `[agents.<name>] command` plus `args`, else the built-in CLI.
pub fn command(config: &Config, agent: &str) -> Option<Vec<String>> {
    let d = config.agents.get(agent);
    let mut argv = match d.and_then(|d| d.command.as_ref()) {
        Some(c) => c.argv(),
        None => vec![BUILTIN.iter().find(|(n, _)| *n == agent).map(|(_, c)| (*c).to_owned())?],
    };
    argv.extend(d.map(|d| d.args.clone()).unwrap_or_default());
    (!argv.is_empty()).then_some(argv)
}

/// [`command`], its program found where it really is (`locate`): on `path`, else where npm,
/// nvm or `npx` put it. `None` when the agent is unknown or its program is nowhere.
pub fn resolve(config: &Config, agent: &str, path: &str) -> Option<(Vec<String>, locate::Found)> {
    let argv = command(config, agent)?;
    let found = locate::find(&argv[0], path)?;
    let mut v = found.argv.clone();
    v.extend(argv.into_iter().skip(1));
    Some((v, found))
}

/// Which adapter drives `agent`: configured, else its name if that's an adapter, else generic.
pub fn adapter_name(config: &Config, agent: &str) -> String {
    if let Some(a) = config.agents.get(agent).and_then(|d| d.adapter.clone()) {
        return a;
    }
    if ADAPTERS.contains(&agent) { agent.to_owned() } else { "generic".into() }
}

pub fn adapter(id: &str) -> Box<dyn Adapter> {
    match id {
        "claude" => Box::new(claude::Claude),
        "codex" => Box::new(codex::Codex),
        "gemini" => Box::new(others::Gemini),
        "opencode" => Box::new(others::OpenCode),
        "antigravity" | "agy" => Box::new(others::Antigravity),
        _ => Box::new(others::Generic),
    }
}

/// The adapter of the agent `program` (a process name, `claude`) runs, when it is one: a
/// configured agent's command, else a built-in's (UX1.4: an agent started by hand in a shell).
pub fn adapter_for_program(config: &Config, program: &str) -> Option<String> {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    let configured = names(config).into_iter().find(|a| {
        command(config, a).and_then(|c| c.first().map(|p| base(p))).as_deref() == Some(program)
    });
    let builtin = || BUILTIN.iter().find(|(_, p)| *p == program).map(|(n, _)| (*n).to_owned());
    configured
        .map(|a| adapter_name(config, &a))
        .filter(|a| a != "generic")
        .or_else(|| builtin().map(|n| adapter_name(config, &n)))
        .filter(|a| a != "generic")
}

/// Every agent name: built-ins, then configured ones.
pub fn names(config: &Config) -> Vec<String> {
    let mut v: Vec<String> = BUILTIN.iter().map(|(n, _)| (*n).to_owned()).collect();
    for k in config.agents.keys() {
        if !v.contains(k) {
            v.push(k.clone());
        }
    }
    v
}

/// The configured default agent: `[agents.<name>] default = true`, else `claude`.
pub fn default_agent(config: &Config) -> String {
    config
        .agents
        .iter()
        .find(|(_, d)| d.default)
        .map_or_else(|| "claude".into(), |(k, _)| k.clone())
}

/// The variable that points `agent` (or `gh`) at a config directory.
pub fn dir_var(agent: &str) -> Option<&'static str> {
    Some(match agent {
        "claude" => "CLAUDE_CONFIG_DIR",
        "codex" => "CODEX_HOME",
        "gemini" => "GEMINI_CLI_HOME",
        "opencode" => "OPENCODE_CONFIG_DIR",
        "gh" => "GH_CONFIG_DIR",
        _ => return None,
    })
}

/// The agent (by adapter) whose account dir a session's profile sets: `codex` → `CODEX_HOME`.
pub fn home_var(adapter: &str) -> Option<&'static str> {
    match adapter {
        "antigravity" => dir_var("gemini"),
        a => dir_var(a),
    }
}

/// Where the agent keeps its config when nothing redirects it.
pub fn default_dir(agent: &str) -> Option<PathBuf> {
    Some(match agent {
        "claude" => expand_home("~/.claude"),
        "codex" => expand_home("~/.codex"),
        "gemini" => expand_home("~/.gemini"),
        "opencode" => expand_home("~/.config/opencode"),
        "gh" => expand_home("~/.config/gh"),
        _ => return None,
    })
}

/// The tool's own login flow (SPEC §10.10 `profile login`); ptyro never sees the credentials.
pub fn login_argv(target: &str) -> Option<Vec<String>> {
    let v: &[&str] = match target {
        // A fresh config dir starts Claude's login on first run; `/login` switches accounts.
        "claude" => &["claude"],
        "codex" => &["codex", "login"],
        "gemini" => &["gemini"],
        "opencode" => &["opencode", "auth", "login"],
        "antigravity" => &["agy"],
        "gh" => &["gh", "auth", "login"],
        _ => return None,
    };
    Some(v.iter().map(|s| (*s).to_owned()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn programs_name_their_agents() {
        let config = Config::default();
        assert_eq!(super::adapter_for_program(&config, "claude").as_deref(), Some("claude"));
        assert_eq!(super::adapter_for_program(&config, "agy").as_deref(), Some("antigravity"));
        assert_eq!(super::adapter_for_program(&config, "zsh"), None);
        let mut config = Config::default();
        config.agents.insert(
            "mine".into(),
            toml::from_str("command = [\"/opt/bin/my-agent\"]\nadapter = \"codex\"").unwrap(),
        );
        assert_eq!(super::adapter_for_program(&config, "my-agent").as_deref(), Some("codex"));
    }

    use super::*;

    #[test]
    fn agent_commands() {
        let c = Config::parse(
            "[agents.claude]\ncommand = [\"/opt/claude\", \"--x\"]\nargs = [\"--y\"]\n[agents.aider]\ncommand = \"aider\"\ndefault = true\n[agents.cw]\nadapter = \"claude\"\n",
        )
        .unwrap();
        assert_eq!(command(&c, "claude").unwrap(), ["/opt/claude", "--x", "--y"]);
        assert_eq!(command(&c, "codex").unwrap(), ["codex"]);
        assert_eq!(command(&c, "antigravity").unwrap(), ["agy"]);
        assert_eq!(command(&c, "aider").unwrap(), ["aider"]);
        assert!(command(&c, "nope").is_none());
        assert_eq!(default_agent(&c), "aider");
        assert_eq!(default_agent(&Config::default()), "claude");
        assert_eq!(split_agent("claude@work"), ("claude", Some("work")));
        assert_eq!(split_agent("claude"), ("claude", None));
        assert!(names(&c).contains(&"aider".to_owned()));
        assert_eq!(adapter_name(&c, "aider"), "generic");
        assert_eq!(adapter_name(&c, "cw"), "claude");
        assert_eq!(adapter(&adapter_name(&c, "codex")).id(), "codex");
    }
}
