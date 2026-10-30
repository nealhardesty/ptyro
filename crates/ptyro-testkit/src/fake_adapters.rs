//! `ptyro fake-agent --as <adapter>`: the fake agent behaving like Claude Code, Codex, Gemini,
//! OpenCode or Antigravity towards ptyro (M4.15), so CI covers every adapter without real agents.
//!
//! It reads the integration ptyro generated for it, exactly as the real agent would:
//! - **claude**: the `--plugin-dir`'s `hooks/hooks.json`, the `--settings` statusLine command,
//!   `--session-id` / `--resume`;
//! - **codex**: `-c hooks.<Event>=[…]` and `-c notify=[…]`, `resume <id>`;
//! - **gemini** and **antigravity**: an installed extension is simulated by calling
//!   `$PTYRO_BIN hook <adapter> <Event>` directly;
//! - **opencode**: serves `GET /event` (SSE, basic auth with `OPENCODE_SERVER_PASSWORD`) on
//!   `--port`, and the permissions API.
//!
//! Commands, one per line:
//!
//! | command | effect |
//! |---|---|
//! | `work <s> [text]` | a prompt: UserPromptSubmit, PreToolUse(Bash), sleep, PostToolUse, Stop |
//! | `ask <cmd>` | PreToolUse + PermissionRequest, shows the permission dialog; `1`/`2` allow, Esc denies (single keys) |
//! | `fail` | StopFailure rate_limit |
//! | `sub` | a subagent starts and stops |
//! | `team` | two teammates start (same worktree) |
//! | `notify <text>` | Notification agent_needs_input |
//! | `usage <pct>` | runs the statusLine command with rate limits at `pct`% |
//! | `edit <path> <text>` | a turn that writes `text` to `path` (PreToolUse Write: a denial is honoured) |
//! | `sh <command>` | a turn that runs `command` (PreToolUse Bash: a denial is honoured) |
//! | `mcp <tool> <json>` | (claude) calls a tool on the `--mcp-config` server named `ptyro`, prints the result |
//! | `When you are done, write your answer as JSON to the file P …` | writes a verdict to P (`PTYRO_FAKE_VERDICT=changes` for changes) |
//! | `Write JSON to P shaped like …` | (a judge) scores every `*.diff` here into P |
//!
//! With `PTYRO_FAKE_TURNS=1` it treats prose like a real agent: a line `Label: <command>` runs
//! the command, and any other line is a short turn (recipes' prompts and follow-ups).
//! With `PTYRO_FAKE_TOUR=1` (`ptyro tour`) any prose is a task: it asks before running a command,
//! then fixes the typo in `greeting.txt` (or, told no, leaves it).
//! With `PTYRO_FAKE_PASTE=1` it asks for bracketed paste, as Claude and Codex do: a pasted
//! prompt over several lines is one short turn that says it back (`prompt over N lines: …`).
//! | `exit` | SessionEnd, exit 0 |
//!
//! Like Claude, a Stop hook that answers `{"decision":"block"}` keeps the turn going (the fake
//! "works" again and stops again). A verified-done bounce (`[via ptyro: verify]…`) and mail
//! (`<ptyro-message …>`) are prompts: they fire UserPromptSubmit and end a short turn.

use ptyro_core::sync::MutexExt;
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::os::fd::AsFd;
use std::time::Duration;

use serde_json::{Value, json};

struct Fake {
    adapter: String,
    session: String,
    resumed: bool,
    /// Event → shell commands (claude / codex hook config).
    hooks: BTreeMap<String, Vec<String>>,
    notify: Vec<String>,
    statusline: Option<String>,
    psp: bool,
    oc: Option<std::sync::Arc<OcServer>>,
    /// The `--mcp-config` JSON (claude).
    mcp: Option<Value>,
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn uuidish() -> String {
    let n = ptyro_core::now_ms();
    format!(
        "{:08x}-{:04x}-7{:03x}-8{:03x}-{:012x}",
        (n >> 16) as u32,
        n as u16,
        std::process::id() & 0xfff,
        (n >> 4) & 0xfff,
        n
    )
}

impl Fake {
    fn new(args: &[String]) -> Self {
        let adapter = arg_value(args, "--as").unwrap_or_else(|| "claude".into());
        let mut f = Self {
            session: String::new(),
            resumed: false,
            hooks: BTreeMap::new(),
            notify: Vec::new(),
            statusline: None,
            psp: adapter == "claude" || adapter == "opencode" || adapter == "antigravity",
            oc: None,
            mcp: arg_value(args, "--mcp-config").and_then(|m| serde_json::from_str(&m).ok()),
            adapter,
        };
        match f.adapter.as_str() {
            "claude" => {
                if let Some(dir) = arg_value(args, "--plugin-dir")
                    && let Ok(text) =
                        std::fs::read_to_string(std::path::Path::new(&dir).join("hooks/hooks.json"))
                    && let Ok(v) = serde_json::from_str::<Value>(&text)
                {
                    for (ev, groups) in v["hooks"].as_object().cloned().unwrap_or_default() {
                        let cmds = groups
                            .as_array()
                            .into_iter()
                            .flatten()
                            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                            .filter_map(|h| h["command"].as_str().map(str::to_owned))
                            .collect();
                        f.hooks.insert(ev, cmds);
                    }
                }
                if let Some(s) = arg_value(args, "--settings")
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                {
                    f.statusline = s["statusLine"]["command"].as_str().map(str::to_owned);
                }
                match arg_value(args, "--resume") {
                    Some(id) => {
                        f.session = id;
                        f.resumed = true;
                    }
                    None => f.session = arg_value(args, "--session-id").unwrap_or_else(uuidish),
                }
            }
            "codex" => {
                for (i, a) in args.iter().enumerate() {
                    if a != "-c" {
                        continue;
                    }
                    let Some(kv) = args.get(i + 1) else { continue };
                    let Some((k, v)) = kv.split_once('=') else { continue };
                    let Ok(t) = format!("x = {v}").parse::<toml::Table>() else { continue };
                    if let Some(ev) = k.strip_prefix("hooks.").filter(|e| *e != "state") {
                        let cmds = t["x"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .flat_map(|g| {
                                g.get("hooks")
                                    .and_then(|h| h.as_array())
                                    .cloned()
                                    .unwrap_or_default()
                            })
                            .filter_map(|h| {
                                h.get("command").and_then(|c| c.as_str()).map(str::to_owned)
                            })
                            .collect();
                        f.hooks.insert(ev.to_owned(), cmds);
                    } else if k == "notify" {
                        f.notify = t["x"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect();
                    }
                }
                match args.iter().position(|a| a == "resume") {
                    Some(i) => {
                        f.session = args.get(i + 1).cloned().unwrap_or_default();
                        f.resumed = true;
                    }
                    None => f.session = uuidish(),
                }
            }
            "gemini" => match arg_value(args, "--resume") {
                Some(id) => {
                    f.session = id;
                    f.resumed = true;
                }
                None => f.session = arg_value(args, "--session-id").unwrap_or_else(uuidish),
            },
            "opencode" => {
                match arg_value(args, "--session") {
                    Some(id) => {
                        f.session = id;
                        f.resumed = true;
                    }
                    None => f.session = format!("ses_{}", ptyro_core::now_ms()),
                }
                if let Some(port) = arg_value(args, "--port").and_then(|p| p.parse().ok()) {
                    f.oc = OcServer::start(
                        port,
                        std::env::var("OPENCODE_SERVER_PASSWORD").unwrap_or_default(),
                    );
                }
            }
            _ => match arg_value(args, "--resume") {
                Some(id) => {
                    f.session = id;
                    f.resumed = true;
                }
                None => f.session = uuidish(),
            },
        }
        f
    }

    /// Run the hook for `event` with `payload`, as the agent would (command hooks get JSON on
    /// stdin; gemini/antigravity simulate an installed extension). Returns what a hook printed
    /// as JSON (a decision), if anything.
    fn hook(&self, event: &str, payload: Value) {
        let _ = self.hook_out(event, payload);
    }

    fn hook_out(&self, event: &str, payload: Value) -> Option<Value> {
        let (cmds, payload) = self.hook_prep(event, payload);
        run_hooks(&cmds, &payload)
    }

    /// The hook commands for `event` and the payload they get.
    fn hook_prep(&self, event: &str, mut payload: Value) -> (Vec<String>, Value) {
        payload["hook_event_name"] = json!(event);
        payload["cwd"] =
            json!(std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
        let cmds: Vec<String> = match self.adapter.as_str() {
            "claude" | "codex" => self.hooks.get(event).cloned().unwrap_or_default(),
            "gemini" | "antigravity" => {
                let bin = std::env::var("PTYRO_BIN").unwrap_or_else(|_| "ptyro".into());
                vec![format!("{bin} hook {} {event}", self.adapter)]
            }
            _ => Vec::new(),
        };
        if self.adapter == "antigravity" {
            payload["conversationId"] = json!(self.session);
        } else {
            payload["session_id"] = json!(self.session);
        }
        (cmds, payload)
    }

    /// A PreToolUse hook's denial reason, if it denied.
    fn denied(answer: Option<&Value>) -> Option<String> {
        let a = answer?;
        let h = &a["hookSpecificOutput"];
        if h["permissionDecision"] == "deny" {
            return h["permissionDecisionReason"]
                .as_str()
                .map(str::to_owned)
                .or(Some("denied".into()));
        }
        (a["decision"] == "deny").then(|| a["reason"].as_str().unwrap_or("denied").to_owned())
    }

    /// One tool call in a turn, as the agent does it: PreToolUse (which may deny), the work,
    /// PostToolUse. `Err(reason)` when denied.
    fn tool(
        &self,
        name: &str,
        input: Value,
        work: impl FnOnce() -> String,
    ) -> Result<String, String> {
        let tool = json!({ "tool_name": name, "tool_input": input });
        let pre = if self.adapter == "gemini" { "BeforeTool" } else { "PreToolUse" };
        if let Some(why) = Self::denied(self.hook_out(pre, tool.clone()).as_ref()) {
            return Err(why);
        }
        let out = work();
        self.hook(if self.adapter == "gemini" { "AfterTool" } else { "PostToolUse" }, tool);
        Ok(out)
    }

    /// A turn: the prompt hook, working, `body`, then the turn's end (bounced while a Stop hook
    /// blocks, as Claude does).
    fn turn(
        &self,
        out: &mut std::io::Stdout,
        prompt: &str,
        body: impl FnOnce(&mut std::io::Stdout) -> String,
    ) {
        let p = json!({ "prompt": prompt });
        match self.adapter.as_str() {
            "gemini" => self.hook("BeforeAgent", p),
            "antigravity" => self.hook("PreInvocation", p),
            _ => self.hook("UserPromptSubmit", p),
        };
        self.oc(json!({ "type": "session.status", "properties": { "sessionID": self.session, "status": { "type": "busy" } } }));
        self.report(out, "state=working:msg=d29ya2luZw");
        let msg = body(out);
        self.report(out, "state=done");
        self.turn_complete(&msg, out);
    }

    /// Call a tool on the `ptyro` MCP server from `--mcp-config`, the way Claude would.
    fn mcp_call(&self, tool: &str, args: &str) -> String {
        let Some(def) =
            self.mcp.as_ref().map(|m| m["mcpServers"]["ptyro"].clone()).filter(|d| !d.is_null())
        else {
            return "mcp: no ptyro server in --mcp-config".into();
        };
        let mut c = std::process::Command::new(def["command"].as_str().unwrap_or("ptyro"));
        c.args(def["args"].as_array().into_iter().flatten().filter_map(Value::as_str));
        for (k, v) in def["env"].as_object().into_iter().flatten() {
            c.env(k, v.as_str().unwrap_or_default());
        }
        c.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let Ok(mut child) = c.spawn() else { return "mcp: cannot start the server".into() };
        let mut stdin = child.stdin.take().expect("stdin");
        let mut lines = std::io::BufReader::new(child.stdout.take().expect("stdout")).lines();
        let args: Value = serde_json::from_str(args).unwrap_or(json!({}));
        let msgs = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "fake-claude", "version": "0" } } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": args, "_meta": { "progressToken": "p1" } } }),
        ];
        for m in msgs {
            let _ = writeln!(stdin, "{m}");
        }
        let _ = stdin.flush();
        let mut result = String::from("mcp: no answer");
        for line in lines.by_ref() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] == 2 {
                let text =
                    v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_owned();
                let err = v["result"]["isError"] == true || v.get("error").is_some();
                result = format!("mcp {tool} {}:\n{text}", if err { "error" } else { "ok" });
                break;
            }
        }
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    fn report(&self, out: &mut std::io::Stdout, pairs: &str) {
        if self.psp {
            let _ = write!(out, "\x1b]7501;{pairs}:app=fake-{}\x1b\\", self.adapter);
            let _ = out.flush();
        }
    }

    fn oc(&self, ev: Value) {
        if let Some(s) = &self.oc {
            s.emit(ev);
        }
    }

    /// The tour's task (`PTYRO_FAKE_TOUR=1`): look around, ask before running a command, then
    /// fix the typo in `greeting.txt` and say so. Told no, it leaves the file alone.
    fn tour_turn(&self, out: &mut std::io::Stdout, prompt: &str) {
        self.turn(out, prompt, |out| {
            let _ = writeln!(out, "Looking at greeting.txt…");
            std::thread::sleep(Duration::from_millis(800));
            let fix = "sed -i.bak 's/Helo/Hello/' greeting.txt && rm greeting.txt.bak";
            let answer = self.ask_permission(out, fix);
            let tool = json!({ "tool_name": "Bash", "tool_input": { "command": fix } });
            if answer == "no" {
                self.hook("PermissionDenied", tool);
                let msg = "OK, I left greeting.txt as it was.".to_owned();
                let _ = writeln!(out, "{msg}");
                return msg;
            }
            self.report(out, "state=working:msg=Zml4aW5n");
            let text = std::fs::read_to_string("greeting.txt").unwrap_or_default();
            let _ = std::fs::write("greeting.txt", text.replace("Helo", "Hello"));
            self.hook("PostToolUse", tool);
            std::thread::sleep(Duration::from_millis(500));
            let msg = "Fixed the typo in greeting.txt: \"Helo\" is \"Hello\" now.".to_owned();
            let _ = writeln!(out, "{msg}");
            msg
        });
    }

    /// A permission prompt for `command`, answered by key or by ptyro's decision: "yes",
    /// "always" or "no" (printed as `answer: …`).
    fn ask_permission(&self, out: &mut std::io::Stdout, rest: &str) -> &'static str {
        let f = self;
        let tool = json!({ "tool_name": "Bash", "tool_input": { "command": rest } });
        f.hook("PreToolUse", tool.clone());
        // Claude's PermissionRequest runs alongside its dialog (and may answer it).
        let (decided_tx, decided) = std::sync::mpsc::channel();
        match f.adapter.as_str() {
                    "gemini" => f.hook("Notification", json!({ "notification_type": "ToolPermission", "message": format!("Allow Bash: {rest}") })),
                    "opencode" => f.oc(json!({ "type": "permission.asked", "properties": { "sessionID": f.session, "id": "per_1", "title": format!("Bash: {rest}") } })),
                    "claude" => {
                        let (cmds, payload) = f.hook_prep("PermissionRequest", tool.clone());
                        std::thread::spawn(move || {
                            let _ = decided_tx.send(run_hooks(&cmds, &payload));
                        });
                    }
                    _ => f.hook("PermissionRequest", tool.clone()),
                }
        f.report(out, "state=blocked:kind=permission:msg=YXBwcm92ZQ");
        let _ = write!(
            out,
            "\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. Yes, and don't ask again\r\n  3. No (esc)\r\n"
        );
        let _ = out.flush();
        let answer = if f.adapter == "opencode" {
            f.oc.as_ref().map_or("no", |s| s.wait_answer())
        } else if f.adapter == "claude" {
            match key_or_decision(&decided) {
                Err(true) | Ok(b'1' | b'y') => "yes",
                Ok(b'2' | b'a') => "always",
                _ => "no",
            }
        } else {
            match read_key() {
                b'1' | b'y' => "yes",
                b'2' | b'a' => "always",
                _ => "no",
            }
        };
        let _ = writeln!(out, "answer: {answer}");
        answer
    }

    fn turn_complete(&self, msg: &str, out: &mut std::io::Stdout) {
        match self.adapter.as_str() {
            "codex" => {
                self.hook("Stop", json!({}));
                if let Some((prog, args)) = self.notify.split_first() {
                    let j = json!({ "type": "agent-turn-complete", "thread-id": self.session, "last-assistant-message": msg });
                    let _ = std::process::Command::new(prog).args(args).arg(j.to_string()).status();
                }
            }
            "gemini" => self.hook("AfterAgent", json!({})),
            "antigravity" => self.hook("Stop", json!({ "fullyIdle": true })),
            "opencode" => {
                self.oc(json!({ "type": "session.status", "properties": { "sessionID": self.session, "status": { "type": "idle" } } }));
                self.oc(
                    json!({ "type": "session.idle", "properties": { "sessionID": self.session } }),
                );
            }
            _ => {
                // Claude: a Stop hook that blocks keeps the turn going.
                let mut active = false;
                for _ in 0..10 {
                    let stop = json!({ "stop_hook_active": active, "last_assistant_message": msg });
                    let Some(a) = self.hook_out("Stop", stop) else {
                        return;
                    };
                    if a["decision"] != "block" {
                        return;
                    }
                    let reason = a["reason"].as_str().unwrap_or_default();
                    let _ = writeln!(out, "stop hook: {}", reason.lines().next().unwrap_or(""));
                    self.report(out, "state=working:msg=Zml4aW5n");
                    let _ = self.tool("Bash", json!({ "command": "true" }), || {
                        std::thread::sleep(Duration::from_millis(200));
                        String::new()
                    });
                    self.report(out, "state=done");
                    active = true;
                }
            }
        }
    }
}

/// Read one key in raw mode (the permission dialog).
/// Run hook commands with `payload` on stdin; the last JSON line any of them prints.
fn run_hooks(cmds: &[String], payload: &Value) -> Option<Value> {
    let mut answer = None;
    for c in cmds {
        if let Ok(mut child) = std::process::Command::new("sh")
            .args(["-c", c])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(payload.to_string().as_bytes());
            }
            if let Ok(out) = child.wait_with_output() {
                let text = String::from_utf8_lossy(&out.stdout);
                if let Some(v) =
                    text.lines().rev().find_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                {
                    answer = Some(v);
                }
            }
        }
    }
    answer
}

/// A key, or a `PermissionRequest` hook's decision, whichever comes first (Claude shows its
/// dialog while the hook runs; verified on 2.1.305). Polls stdin so no reader is left behind to
/// eat the next prompt.
fn key_or_decision(rx: &std::sync::mpsc::Receiver<Option<Value>>) -> Result<u8, bool> {
    use rustix::termios::{self, LocalModes, OptionalActions};
    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let saved = termios::tcgetattr(fd).ok();
    if let Some(s) = &saved {
        let mut raw = s.clone();
        raw.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO);
        let _ = termios::tcsetattr(fd, OptionalActions::Now, &raw);
    }
    let mut hook_done = false;
    let r = loop {
        if !hook_done {
            match rx.try_recv() {
                Ok(Some(v)) => match v["hookSpecificOutput"]["decision"]["behavior"].as_str() {
                    Some("allow") => break Err(true),
                    Some("deny") => break Err(false),
                    _ => hook_done = true,
                },
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => hook_done = true,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut pfd = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: 50_000_000 };
        if rustix::event::poll(&mut pfd, Some(&timeout)).unwrap_or(0) > 0 {
            let mut b = [0u8; 1];
            let _ = stdin.lock().read(&mut b);
            break Ok(b[0]);
        }
    };
    if let Some(s) = &saved {
        let _ = termios::tcsetattr(fd, OptionalActions::Now, s);
    }
    r
}

fn read_key() -> u8 {
    use rustix::termios::{self, LocalModes, OptionalActions};
    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let saved = termios::tcgetattr(fd).ok();
    if let Some(s) = &saved {
        let mut raw = s.clone();
        raw.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO);
        let _ = termios::tcsetattr(fd, OptionalActions::Now, &raw);
    }
    let mut b = [0u8; 1];
    let _ = stdin.lock().read(&mut b);
    if let Some(s) = &saved {
        let _ = termios::tcsetattr(fd, OptionalActions::Now, s);
    }
    b[0]
}

pub fn main(args: &[String]) -> i32 {
    let f = Fake::new(args);
    let mut out = std::io::stdout();
    let _ = writeln!(out, "fake {} session={} resumed={}", f.adapter, f.session, f.resumed);
    // Claude's SessionStart and every Codex hook name the model the agent runs.
    let model = if f.adapter == "claude" { "claude-opus-5-5" } else { "gpt-5.6-sol" };
    f.hook(
        "SessionStart",
        json!({ "source": if f.resumed { "resume" } else { "startup" }, "model": model }),
    );
    f.oc(json!({ "type": "session.created", "properties": { "info": { "id": f.session } } }));
    f.report(&mut out, "state=idle");
    // `PTYRO_FAKE_PASTE=1`: bracketed paste, as real agents ask for, so a pasted prompt over
    // several lines is one prompt. Off, each line is its own command (the scripted tests).
    if std::env::var("PTYRO_FAKE_PASTE").is_ok() {
        let _ = write!(out, "\x1b[?2004h");
    }
    let stdin = std::io::stdin();
    loop {
        let _ = write!(out, "> ");
        let _ = out.flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return 0;
        }
        // The cooked terminal splits a paste at its new lines: join it up to its end mark (the
        // Enter after it ends that last line).
        if line.contains("\x1b[200~") {
            while !line.contains("\x1b[201~") {
                let mut more = String::new();
                if stdin.lock().read_line(&mut more).unwrap_or(0) == 0 {
                    break;
                }
                line.push_str(&more);
            }
            line = line.replace("\x1b[200~", "").replace("\x1b[201~", "");
            let text = line.trim().to_owned();
            if text.contains('\n') {
                // A prompt over several lines: one short turn, said back whole.
                f.turn(&mut out, &text, |o| {
                    let n = text.lines().count();
                    let said = format!("prompt over {n} lines: {}", text.replace('\n', " ⏎ "));
                    let _ = writeln!(o, "{said}");
                    said
                });
                continue;
            }
            line = text;
        }
        let mut line = line.trim().to_owned();
        // `PTYRO_FAKE_TURNS=1`: act like a real agent on prose. A prompt line `Label: cmd` runs
        // `cmd` (`Implement: edit a.txt hi`), and any other non-empty line is a short turn.
        let prose = std::env::var("PTYRO_FAKE_TURNS").is_ok();
        if prose
            && let Some((_, c)) = line.split_once(": ")
            && ["edit ", "sh ", "work "].iter().any(|p| c.starts_with(p))
        {
            line = c.to_owned();
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match cmd {
            "work" => {
                let (secs, text) = rest.split_once(' ').unwrap_or((rest, "done"));
                let secs: f64 = secs.parse().unwrap_or(1.0);
                let prompt = json!({ "prompt": format!("work {rest}") });
                match f.adapter.as_str() {
                    "gemini" => f.hook("BeforeAgent", prompt),
                    "antigravity" => f.hook("PreInvocation", prompt),
                    _ => f.hook("UserPromptSubmit", prompt),
                }
                f.oc(json!({ "type": "session.status", "properties": { "sessionID": f.session, "status": { "type": "busy" } } }));
                f.report(&mut out, "state=working:msg=d29ya2luZw");
                let tool = json!({ "tool_name": "Bash", "tool_input": { "command": format!("sleep {secs}") } });
                f.hook(
                    if f.adapter == "gemini" { "BeforeTool" } else { "PreToolUse" },
                    tool.clone(),
                );
                std::thread::sleep(Duration::from_secs_f64(secs));
                f.hook(if f.adapter == "gemini" { "AfterTool" } else { "PostToolUse" }, tool);
                let _ = writeln!(out, "worked: {text}");
                f.report(&mut out, "state=done");
                f.turn_complete(text, &mut out);
            }
            "ask" => {
                let tool = json!({ "tool_name": "Bash", "tool_input": { "command": rest } });
                let answer = f.ask_permission(&mut out, rest);
                if answer == "no" {
                    f.hook("PermissionDenied", tool);
                    f.report(&mut out, "state=idle");
                    if f.adapter == "codex" {
                        f.hook("Interrupt", json!({}));
                    }
                    f.oc(json!({ "type": "session.status", "properties": { "sessionID": f.session, "status": { "type": "idle" } } }));
                } else {
                    f.oc(json!({ "type": "permission.replied", "properties": { "sessionID": f.session, "id": "per_1" } }));
                    f.hook("PostToolUse", tool);
                    f.report(&mut out, "state=done");
                    f.turn_complete("ran it", &mut out);
                }
            }
            "fail" => {
                f.hook(
                    "StopFailure",
                    json!({ "error": "rate_limit", "error_details": "5-hour limit reached" }),
                );
                f.report(&mut out, "state=error:msg=cmF0ZSBsaW1pdA");
            }
            "sub" => {
                f.hook("SubagentStart", json!({ "agent_id": "sub-1", "agent_type": "Explore" }));
                std::thread::sleep(Duration::from_millis(300));
                f.hook("SubagentStop", json!({ "agent_id": "sub-1", "agent_type": "Explore" }));
            }
            "team" => {
                for n in ["alice", "bob"] {
                    f.hook("SubagentStart", json!({ "agent_id": n, "agent_type": "teammate" }));
                }
            }
            "notify" => f.hook(
                "Notification",
                json!({ "notification_type": "agent_needs_input", "message": rest }),
            ),
            "usage" => {
                let pct: f64 = rest.parse().unwrap_or(50.0);
                if let Some(cmd) = &f.statusline {
                    let payload = json!({
                        "session_id": f.session,
                        "rate_limits": { "five_hour": { "used_percentage": pct, "resets_at": ptyro_core::now_ms() / 1000 + 3600 }, "seven_day": { "used_percentage": pct / 2.0, "resets_at": ptyro_core::now_ms() / 1000 + 86400 } },
                        "cost": { "total_cost_usd": 0.42 },
                        "context_window": { "used_percentage": 33 }
                    });
                    if let Ok(mut c) = std::process::Command::new("sh")
                        .args(["-c", cmd])
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .spawn()
                    {
                        if let Some(mut stdin) = c.stdin.take() {
                            let _ = stdin.write_all(payload.to_string().as_bytes());
                        }
                        let line = c
                            .wait_with_output()
                            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                            .unwrap_or_default();
                        let _ = writeln!(out, "statusline: {}", line.trim());
                    }
                }
            }
            "exit" => {
                f.hook("SessionEnd", json!({ "reason": "prompt_input_exit" }));
                f.report(&mut out, "state=clear");
                return 0;
            }
            "edit" => {
                let (path, text) = rest.split_once(' ').unwrap_or((rest, ""));
                let abs =
                    std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.into());
                f.turn(&mut out, &line, |out| {
                    let r = f.tool(
                        "Write",
                        json!({ "file_path": abs.to_string_lossy(), "content": text }),
                        || {
                            if let Some(d) = abs.parent() {
                                let _ = std::fs::create_dir_all(d);
                            }
                            let _ = std::fs::write(&abs, format!("{text}\n"));
                            format!("wrote {path}")
                        },
                    );
                    let m = r.unwrap_or_else(|why| format!("denied: {why}"));
                    let _ = writeln!(out, "{m}");
                    m
                });
            }
            "sh" => {
                f.turn(&mut out, &line, |out| {
                    let r = f.tool("Bash", json!({ "command": rest }), || {
                        std::process::Command::new("sh")
                            .args(["-c", rest])
                            .output()
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                            .unwrap_or_default()
                    });
                    let m = match r {
                        Ok(o) => format!("ran: {o}"),
                        Err(why) => format!("denied: {why}"),
                    };
                    let _ = writeln!(out, "{m}");
                    m
                });
            }
            "mcp" => {
                let (tool, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
                f.turn(&mut out, &line, |out| {
                    let r = f
                        .tool(&format!("mcp__ptyro__{tool}"), json!({}), || f.mcp_call(tool, args))
                        .unwrap_or_else(|why| format!("denied: {why}"));
                    let _ = writeln!(out, "{}", r.replace('\n', "\r\n"));
                    r.lines().next().unwrap_or("").to_owned()
                });
            }
            "/exit" | "/quit" => return 0,
            "" => {}
            // A step that wants a typed answer (`output = { schema = … }`): write it, like an
            // agent following the instructions. `PTYRO_FAKE_VERDICT=changes` asks for changes.
            _ if line.starts_with("When you are done, write your answer as JSON to the file ") => {
                let path = line
                    .trim_start_matches("When you are done, write your answer as JSON to the file ")
                    .split(" (and nothing else")
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let v = match std::env::var("PTYRO_FAKE_VERDICT").as_deref() {
                    Ok("changes") => {
                        json!({ "decision": "changes", "issues": ["handle the empty case"], "summary": "almost" })
                    }
                    _ => json!({ "decision": "approve", "issues": [], "summary": "looks right" }),
                };
                let _ = std::fs::write(&path, v.to_string());
                let _ = writeln!(out, "answered in {path}");
                f.turn(&mut out, &line, |_| "answered".into());
            }
            // The judge (M7.10): score every candidate diff in the directory.
            _ if line.starts_with("Write JSON to ") && line.contains(" shaped like ") => {
                let path = line
                    .trim_start_matches("Write JSON to ")
                    .split(" shaped like ")
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let mut scores = serde_json::Map::new();
                let mut notes = serde_json::Map::new();
                for e in std::fs::read_dir(".").into_iter().flatten().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if let Some(id) = name.strip_suffix(".diff") {
                        let lines = std::fs::read_to_string(e.path())
                            .map(|t| t.lines().count())
                            .unwrap_or(0);
                        scores.insert(id.to_owned(), json!(if lines > 0 { 6 } else { 1 }));
                        notes.insert(id.to_owned(), json!(format!("{lines} diff lines")));
                    }
                }
                let _ =
                    std::fs::write(&path, json!({ "scores": scores, "notes": notes }).to_string());
                let _ = writeln!(out, "judged {} candidate(s)", scores.len());
                f.turn(&mut out, &line, |_| "judged".into());
            }
            // The tour's finish asks its agent to commit: it does, with a message saying what.
            _ if std::env::var("PTYRO_FAKE_TOUR").is_ok()
                && line.starts_with("[via ptyro] Please commit") =>
            {
                f.turn(&mut out, &line, |out| {
                    let ok = std::process::Command::new("sh")
                        .args([
                            "-c",
                            "git add -A && git commit -q -m 'Fix the typo in greeting.txt'",
                        ])
                        .status()
                        .is_ok_and(|s| s.success());
                    let msg = if ok {
                        "Committed: Fix the typo in greeting.txt"
                    } else {
                        "Nothing to commit"
                    };
                    let _ = writeln!(out, "{msg}");
                    msg.into()
                });
            }
            // `PTYRO_FAKE_TOUR=1` (`ptyro tour`): any prose is a task, done like an agent would.
            _ if std::env::var("PTYRO_FAKE_TOUR").is_ok()
                && !line.starts_with("[via ptyro: verify]")
                && !line.starts_with("<ptyro-message") =>
            {
                f.tour_turn(&mut out, &line);
            }
            _ if line.starts_with("[via ptyro: verify]")
                || line.starts_with("<ptyro-message")
                || prose =>
            {
                let _ = writeln!(out, "you said: {line}");
                f.turn(&mut out, &line, |_| "read it".into());
            }
            _ => {
                let _ = writeln!(out, "you said: {line}");
            }
        }
    }
}

/// OpenCode's embedded server, the bits ptyro uses.
struct OcServer {
    clients: std::sync::Mutex<Vec<std::net::TcpStream>>,
    answer: std::sync::Mutex<Option<&'static str>>,
    cond: std::sync::Condvar,
}

impl OcServer {
    fn start(port: u16, password: String) -> Option<std::sync::Arc<Self>> {
        let l = std::net::TcpListener::bind(("127.0.0.1", port)).ok()?;
        let me = std::sync::Arc::new(Self {
            clients: std::sync::Mutex::new(Vec::new()),
            answer: std::sync::Mutex::new(None),
            cond: std::sync::Condvar::new(),
        });
        let srv = std::sync::Arc::clone(&me);
        std::thread::spawn(move || {
            for sock in l.incoming().flatten() {
                let srv = std::sync::Arc::clone(&srv);
                let password = password.clone();
                std::thread::spawn(move || srv.handle(sock, &password));
            }
        });
        Some(me)
    }

    fn handle(&self, mut sock: std::net::TcpStream, password: &str) {
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let auth = format!("Basic {}", b64(&format!("opencode:{password}")));
        if !req.lines().map(|l| l.trim_end_matches('\r')).any(|l| {
            l.split_once(':')
                .is_some_and(|(k, v)| k.eq_ignore_ascii_case("authorization") && v.trim() == auth)
        }) {
            let _ = sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        let line = req.lines().next().unwrap_or("");
        if line.starts_with("GET /event") {
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\n");
            let _ = sock.write_all(b"data: {\"type\":\"server.connected\",\"properties\":{}}\n\n");
            self.clients.locked().push(sock);
        } else if line.starts_with("POST /session/") && line.contains("/permissions/") {
            let answer = if req.contains("\"reject\"") {
                "no"
            } else if req.contains("\"always\"") {
                "always"
            } else {
                "yes"
            };
            *self.answer.locked() = Some(answer);
            self.cond.notify_all();
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntrue");
        } else {
            let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }

    fn emit(&self, ev: Value) {
        let msg = format!("data: {ev}\n\n");
        self.clients.locked().retain_mut(|c| c.write_all(msg.as_bytes()).is_ok());
    }

    fn wait_answer(&self) -> &'static str {
        let mut a = self.answer.locked();
        while a.is_none() {
            a = self.cond.wait(a).expect("answer");
        }
        a.take().unwrap_or("no")
    }
}

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = s.as_bytes();
    let mut o = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            o.push(if i <= chunk.len() {
                T[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    o
}
