//! The adapter contract (SPEC §8.1): how to launch and resume an agent, how its hooks, titles and
//! screens map onto normalized [`Signal`]s, how to answer its permission prompt, and how to stop
//! it gracefully. Everything here is synchronous and pure except [`random_hex`]/[`uuid_v4`];
//! the daemon does the I/O (writing integration files, running sidecars).

use std::path::{Path, PathBuf};
use std::time::Duration;

use ptyro_core::config::AgentDef;
use ptyro_core::status::Signal;
use serde_json::Value;

/// What an adapter can do (SPEC §8.1 capabilities).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    /// ptyro chooses the conversation id at launch.
    pub preset_conversation: bool,
    /// The id is learned from hooks or events.
    pub learn_conversation: bool,
    pub resume: bool,
    pub hooks: bool,
    pub osc_status: bool,
    pub title_status: bool,
    /// A reliable "blocked on permission" signal.
    pub permission_signal: bool,
    pub turn_end_signal: bool,
    /// The permission prompt can be answered by ptyro (keys or API).
    pub answer: bool,
}

/// Inputs for a launch.
pub struct LaunchCtx<'a> {
    /// The configured agent name (`claude`, `claude-work`, `aider`).
    pub agent: &'a str,
    pub def: &'a AgentDef,
    /// `command` + `args` from config (or the built-in CLI).
    pub base_argv: Vec<String>,
    /// ptyro's session id (hooks report it back via `PTYRO_SESSION_ID`).
    pub session: &'a str,
    /// The session's name (Claude's `-n`).
    pub name: &'a str,
    pub cwd: &'a Path,
    /// `<data>/integrations`.
    pub integrations: &'a Path,
    /// Absolute path of the `ptyro` binary, for hook commands.
    pub ptyro: &'a str,
    /// A free port, for adapters that run a server (OpenCode).
    pub port: Option<u16>,
    /// The agent's config dir under this session's profile (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`).
    pub agent_home: Option<PathBuf>,
    /// The project's main checkout (Codex directory trust).
    pub project_root: Option<&'a Path>,
    /// MCP servers to give the agent (`{"mcpServers": …}` JSON): `ptyro mcp` for a lead.
    pub mcp: Option<&'a str>,
}

/// A side channel the daemon runs next to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sidecar {
    /// OpenCode's embedded server: SSE `/event` with basic auth.
    OpenCode { port: u16, password: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchPlan {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Integration files to write (absolute paths under `integrations`; SPEC §8.1).
    pub files: Vec<(PathBuf, String)>,
    /// The conversation id ptyro pre-assigned.
    pub conversation: Option<String>,
    pub sidecar: Option<Sidecar>,
}

/// One step of a graceful stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopStep {
    /// Only if the agent is working: these keys (interrupt the turn).
    InterruptKeys(Vec<u8>),
    /// Type these keys.
    Keys(Vec<u8>),
    /// Wait this long for the process to exit.
    Wait(Duration),
}

/// The answer to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    /// Allow, and don't ask again for this kind of action.
    Always,
    Deny,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "allow" | "yes" | "y" | "approve" => Self::Allow,
            "always" | "a" => Self::Always,
            "deny" | "no" | "n" | "reject" => Self::Deny,
            _ => return None,
        })
    }
}

pub trait Adapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;
    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan, String>;
    /// Relaunch into an existing conversation.
    fn resume(&self, ctx: &LaunchCtx<'_>, conversation: &str) -> Result<LaunchPlan, String>;
    /// A hook payload (from `ptyro hook <adapter> <event>`) as signals.
    fn on_hook(&self, event: &str, payload: &Value) -> Vec<Signal> {
        let _ = (event, payload);
        Vec::new()
    }
    /// The window title (OSC 0/2) as a medium-confidence status, where the agent encodes one.
    fn on_title(&self, title: &str) -> Option<Signal> {
        let _ = title;
        None
    }
    /// OSC 7501 `done` from this agent means "turn over, waiting for you" (agents) rather than
    /// "task complete" (one-shot tools).
    fn done_means_idle(&self) -> bool {
        true
    }
    /// A status read off the screen, for dialogs that come before any hook (trust, login,
    /// updates). Medium confidence; only consulted while no hook or OSC 7501 has spoken.
    fn on_screen(&self, screen: &str) -> Option<Signal> {
        let _ = screen;
        None
    }
    /// The status the screen shows, for an agent started by hand in a shell session, so with
    /// none of ptyro's hooks (UX1.4): working, blocked on a dialog, else idle. Low confidence;
    /// `None` when this agent's screen says nothing ptyro can read.
    fn screen_status(&self, screen: &str) -> Option<Signal> {
        let _ = screen;
        None
    }
    /// Whether the screen shows the input box (prompt delivery fallback, SPEC §8.2/§8.4).
    fn ready(&self, screen: &str) -> bool {
        let _ = screen;
        false
    }
    /// Keys that answer the permission prompt on screen, where the agent takes keys.
    fn answer(&self, d: Decision) -> Option<Vec<u8>> {
        let _ = d;
        None
    }
    /// Keys that answer whatever question is on `screen` (a folder-trust dialog takes other
    /// keys than a permission prompt); by default, [`Adapter::answer`].
    fn answer_screen(&self, screen: &str, d: Decision) -> Option<Vec<u8>> {
        let _ = screen;
        self.answer(d)
    }
    fn stop(&self) -> Vec<StopStep> {
        vec![StopStep::Wait(Duration::ZERO)]
    }
}

/// 32 random hex characters from the OS's CSPRNG (also an HMAC key, so there is no weak
/// fallback: if the random source fails twice, this panics).
pub fn random_hex() -> String {
    let b = ptyro_core::secret::random_bytes(16)
        .or_else(|_| ptyro_core::secret::random_bytes(16))
        .expect("the OS random source (/dev/urandom) is unavailable");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A random UUIDv4 (Claude and Gemini accept these as `--session-id`).
pub fn uuid_v4() -> String {
    let h = random_hex();
    let mut c: Vec<char> = h.chars().collect();
    c[12] = '4';
    let v = c[16].to_digit(16).unwrap_or(0);
    c[16] = std::char::from_digit((v & 0x3) | 0x8, 16).unwrap_or('8');
    let s: String = c.into_iter().collect();
    format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
}

/// A stable directory for integration files with this content: `<root>/<agent>/<hash>/`.
pub fn integration_dir(root: &Path, agent: &str, content: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(content.as_bytes());
    let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    root.join(agent).join(hex)
}

/// `"Bash: cargo test"`, `"Edit: src/x.rs"`: a tool call in a few words.
pub fn tool_summary(tool: &str, input: &Value) -> String {
    let pick = ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt"];
    let detail = pick
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|d| d.lines().next().unwrap_or("").trim().to_owned())
        .filter(|d| !d.is_empty());
    let mut s = match detail {
        Some(d) => format!("{tool}: {d}"),
        None => tool.to_owned(),
    };
    if s.chars().count() > 120 {
        s = s.chars().take(119).collect::<String>() + "…";
    }
    s
}

pub fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The model a hook payload names: its own `model` (Claude's `SessionStart`, every Codex hook).
pub fn model_in(p: &Value) -> Option<Signal> {
    str_of(p, "model").map(|id| Signal::Model { id: id.to_owned() })
}

/// The model of the last assistant turn in a Claude transcript (`transcript_path`): what ran the
/// latest turn, so a `/model` switch shows. Reads only the file's tail; `None` when it can't.
pub fn transcript_model(path: &str) -> Option<Signal> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    let path = std::path::Path::new(path);
    if !path.is_absolute() || path.extension()? != "jsonl" {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    f.take(TAIL).read_to_end(&mut tail).ok()?;
    String::from_utf8_lossy(&tail).lines().rev().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        let id = v.pointer("/message/model")?.as_str()?;
        // Claude's own placeholder turns (an API error, a local command) name no real model.
        (v["type"] == "assistant" && !id.starts_with('<')).then(|| Signal::Model { id: id.into() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_is_32_hex_chars_and_fresh() {
        let h = random_hex();
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(random_hex(), h);
    }

    #[test]
    fn uuids_and_summaries() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!("89ab".contains(&u[19..20]));
        assert_ne!(uuid_v4(), u);
        let v: Value = serde_json::json!({"command": "cargo test\nmore", "description": "x"});
        assert_eq!(tool_summary("Bash", &v), "Bash: cargo test");
        assert_eq!(tool_summary("Read", &serde_json::json!({})), "Read");
        let a = integration_dir(Path::new("/i"), "claude", "x");
        assert_eq!(a, integration_dir(Path::new("/i"), "claude", "x"));
        assert_ne!(a, integration_dir(Path::new("/i"), "claude", "y"));
    }
}
