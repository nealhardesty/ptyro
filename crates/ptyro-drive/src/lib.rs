//! Structured-session drivers (SPEC §7.1, M10): an agent driven over its structured protocol
//! instead of its terminal UI. Each driver is pure protocol logic. It turns what the agent
//! sends (a stdout line, an event, an HTTP reply) into [`Effect`]s, and what a person does
//! (prompt, answer, interrupt) into messages for the agent. The daemon does the I/O, keeps the
//! transcript, and renders it.
//!
//! | Protocol | Agent | Transport |
//! |---|---|---|
//! | `stream-json` | Claude Code `-p --input-format/--output-format stream-json` | stdio, JSON lines |
//! | `app-server` | Codex `app-server` | stdio, JSON-RPC (no `jsonrpc` field) |
//! | `acp` | Gemini `--acp`, any Agent Client Protocol agent | stdio, JSON-RPC 2.0 |
//! | `opencode` | OpenCode `serve` | HTTP + server-sent events |

pub mod acp;
pub mod claude;
pub mod codex;
pub mod opencode;
mod util;

use ptyro_agents::Decision;
use ptyro_core::chat::ChatItem;
use ptyro_core::status::Signal;
use serde_json::Value;

/// Something the daemon does for a driver.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Insert or replace this transcript item (streamed text arrives as growing copies).
    Item(ChatItem),
    /// A status (or other) signal for the session.
    Signal(Signal),
    /// One line to the agent's stdin (the newline is added).
    Send(String),
    /// An HTTP request to the agent's own server (OpenCode); the reply comes back to
    /// [`Driver::http_reply`] with `tag`.
    Http { method: &'static str, path: String, body: Option<Value>, tag: u32 },
}

/// How a driver starts.
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub cwd: String,
    /// Continue this conversation (Claude session id, Codex thread id, ACP session id…).
    pub resume: Option<String>,
    /// A fresh conversation's id, when the protocol lets us choose it (Claude).
    pub session_id: Option<String>,
    /// More fields for the call that starts or resumes the conversation, over the driver's own
    /// (a chat's tool limits, SPEC §22.5): Codex's `thread/start` and `thread/resume`, OpenCode's
    /// `POST /session`, ACP's `session/new` and `session/load` (its `mcpServers`).
    pub params: Option<Value>,
    /// With `resume`: a new conversation from that one's end, leaving it as it was (Claude's
    /// `--fork-session`, CHAT0).
    pub fork: bool,
}

/// `extra`'s fields over `base`'s (objects only).
pub(crate) fn merged(mut base: Value, extra: Option<&Value>) -> Value {
    if let (Some(b), Some(Value::Object(e))) = (base.as_object_mut(), extra) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

pub trait Driver: Send {
    fn protocol(&self) -> &'static str;
    /// What to send once the agent is running (handshakes, a session to start).
    fn start(&mut self) -> Vec<Effect>;
    /// One line from the agent (stdout, or one server-sent event's data).
    fn on_line(&mut self, line: &str) -> Vec<Effect>;
    /// A reply to an [`Effect::Http`].
    fn http_reply(&mut self, _tag: u32, _status: u16, _body: &str) -> Vec<Effect> {
        Vec::new()
    }
    /// A prompt from a person (or a script, a recipe, the mailbox).
    fn prompt(&mut self, text: &str) -> Vec<Effect>;
    /// Answer the open permission request; empty when there is none.
    fn answer(&mut self, d: Decision) -> Vec<Effect>;
    /// Stop the current turn.
    fn interrupt(&mut self) -> Vec<Effect>;
    /// Whether a permission request is open.
    fn pending(&self) -> bool;
    /// Everything needed to carry on in a new daemon image (live upgrade).
    fn save(&self) -> Value;
}

/// The protocol a configured agent speaks in structured mode, by adapter.
pub fn protocol_for(adapter: &str) -> Option<&'static str> {
    Some(match adapter {
        "claude" => "stream-json",
        "codex" => "app-server",
        "gemini" => "acp",
        "opencode" => "opencode",
        _ => return None,
    })
}

/// The protocols a `[agents.x] protocol = …` may name.
pub const PROTOCOLS: &[&str] = &["stream-json", "app-server", "acp", "opencode"];

/// The command line for structured mode: the agent's configured command plus what the protocol
/// needs. `port` is OpenCode's server port.
pub fn launch_args(
    protocol: &str,
    adapter: &str,
    base: &[String],
    opts: &Opts,
    port: Option<u16>,
) -> Vec<String> {
    let mut v = base.to_vec();
    let add = |v: &mut Vec<String>, a: &[&str]| v.extend(a.iter().map(|s| (*s).to_owned()));
    match protocol {
        "stream-json" => {
            add(
                &mut v,
                &[
                    "-p",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-prompt-tool",
                    "stdio",
                ],
            );
            match (&opts.resume, &opts.session_id) {
                (Some(r), _) => {
                    add(&mut v, &["--resume", r]);
                    if opts.fork {
                        add(&mut v, &["--fork-session"]);
                    }
                }
                (None, Some(id)) => add(&mut v, &["--session-id", id]),
                (None, None) => {}
            }
        }
        "app-server" => add(&mut v, &["app-server"]),
        // Gemini needs `--acp`; another ACP agent's command is used as configured.
        "acp" if adapter == "gemini" => add(&mut v, &["--acp"]),
        "opencode" => {
            let p = port.unwrap_or(0).to_string();
            add(&mut v, &["serve", "--port", &p, "--hostname", "127.0.0.1"]);
        }
        _ => {}
    }
    v
}

/// A new driver for `protocol`.
pub fn new(protocol: &str, opts: Opts) -> Option<Box<dyn Driver>> {
    Some(match protocol {
        "stream-json" => Box::new(claude::Claude::new(opts)),
        "app-server" => Box::new(codex::Codex::new(opts)),
        "acp" => Box::new(acp::Acp::new(opts)),
        "opencode" => Box::new(opencode::OpenCode::new(opts)),
        _ => return None,
    })
}

/// A driver carried over a live upgrade.
pub fn restore(protocol: &str, state: Value) -> Option<Box<dyn Driver>> {
    Some(match protocol {
        "stream-json" => Box::new(claude::Claude::restore(state)?),
        "app-server" => Box::new(codex::Codex::restore(state)?),
        "acp" => Box::new(acp::Acp::restore(state)?),
        "opencode" => Box::new(opencode::OpenCode::restore(state)?),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_lines() {
        let base = vec!["claude".to_owned(), "--model".into(), "opus".into()];
        let a = launch_args(
            "stream-json",
            "claude",
            &base,
            &Opts { session_id: Some("u1".into()), ..Opts::default() },
            None,
        );
        assert_eq!(&a[..3], ["claude", "--model", "opus"]);
        assert!(a.windows(2).any(|w| w == ["--permission-prompt-tool", "stdio"]));
        assert!(a.ends_with(&["--session-id".into(), "u1".into()]));
        let r = launch_args(
            "stream-json",
            "claude",
            &base,
            &Opts { resume: Some("c9".into()), session_id: Some("u1".into()), ..Opts::default() },
            None,
        );
        assert!(
            r.ends_with(&["--resume".into(), "c9".into()]) && !r.contains(&"--session-id".into())
        );
        let f = launch_args(
            "stream-json",
            "claude",
            &base,
            &Opts { resume: Some("c9".into()), fork: true, ..Opts::default() },
            None,
        );
        assert!(f.ends_with(&["--resume".into(), "c9".into(), "--fork-session".into()]));
        assert_eq!(
            launch_args("app-server", "codex", &["codex".into()], &Opts::default(), None),
            ["codex", "app-server"]
        );
        assert_eq!(
            launch_args("acp", "gemini", &["/x/gemini".into()], &Opts::default(), None),
            ["/x/gemini", "--acp"]
        );
        assert_eq!(
            launch_args("acp", "generic", &["my-acp-agent".into()], &Opts::default(), None),
            ["my-acp-agent"]
        );
        assert_eq!(
            launch_args("opencode", "opencode", &["opencode".into()], &Opts::default(), Some(4100)),
            ["opencode", "serve", "--port", "4100", "--hostname", "127.0.0.1"]
        );
        assert_eq!(protocol_for("gemini"), Some("acp"));
        assert_eq!(protocol_for("antigravity"), None);
    }
}
