//! `ptyro fake-agent --as <adapter>`: the fake agent behaving like Claude Code, Codex or OpenCode
//! towards ptyro (M4.15), so CI covers every adapter without real agents.
//!
//! It reads the integration ptyro generated for it, exactly as the real agent would:
//! - **claude**: the `--plugin-dir`'s `hooks/hooks.json`, the `--settings` statusLine command,
//!   `--session-id` / `--resume`, and the `ptyro` server of `--mcp-config` with the tools
//!   `--allowedTools` names (a tool it doesn't name is refused, as Claude refuses it);
//! - **codex**: `-c hooks.<Event>=[…]` and `-c notify=[…]`, `resume <id>`, and
//!   `-c mcp_servers.ptyro.*` (without `default_tools_approval_mode="approve"` a call is
//!   refused, as `codex exec` fails it);
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
//!
//! With `PTYRO_FAKE_TURNS=1` it treats prose like a real agent: a line `Label: <command>` runs
//! the command, and any other line is a short turn (prompts and follow-ups).
//! With `PTYRO_FAKE_TASK=1` any prose is a task: it asks before running a command, then fixes the
//! typo in `greeting.txt` (or, told no, leaves it).
//! With `PTYRO_FAKE_PASTE=1` it asks for bracketed paste, as Claude and Codex do: a pasted
//! prompt over several lines is one short turn that says it back (`prompt over N lines: …`).
//! | `mcp <file> <tool> <json>` | calls a tool of the `ptyro` MCP server the launch gave it, as the real agent would, and writes the answer to `<file>` (`tools/list`: the tools' names and the server's instructions) |
//! | `exit` | SessionEnd, exit 0 |
//! With `PTYRO_FAKE_HOOK_DELAY_MS=N` every hook takes N ms to start (a loaded machine).
//! With `PTYRO_FAKE_TRANSCRIPT=1` the fake Claude and Codex keep a transcript as the real ones
//! do (`fake_transcript`), name it in every hook (`transcript_path`), and take two more
//! commands: `compact <summary>` (Claude's `/compact`: no hook fires) and `rewind` (the file
//! goes back to the end of the first turn).
//! The fake Claude's terminal doesn't echo: as Claude does, it draws a prompt into the
//! conversation once it takes it, and empties its box. So it never shows a typed-but-unsent
//! draft: a dropped Enter on Claude's box isn't modelled (Codex's is,
//! `PTYRO_FAKE_DROP_FIRST_ENTER`). With `PTYRO_FAKE_LAGGY_BOX=N` it is a Claude whose redraw
//! lags: a prompt it took stays in its box, and `ask` draws its permission dialog N ms later,
//! before its hooks (what ptyro must never press Enter on); `PTYRO_FAKE_PLAIN_DIALOG=1` words
//! that dialog so no adapter recognises it. `PTYRO_FAKE_START_GATE=<path>` keeps it loading
//! (output every 200 ms) until the file exists, then it starts. These three and
//! `PTYRO_FAKE_HOOK_DELAY_MS` are test knobs (`ptyro_core::test_knob`).
//!
//! Like Claude, a Stop hook that answers `{"decision":"block"}` keeps the turn going (the fake
//! "works" again and stops again). Mail (`<ptyro-message …>`) is a prompt: it fires
//! UserPromptSubmit and ends a short turn.

use ptyro_core::sync::MutexExt;
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
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
    /// `PTYRO_FAKE_LATE_SESSION_START=1`: SessionStart only with the first prompt, after the
    /// turn's OSC 7501 working, as Codex 0.166 sends it. Set until it has been sent.
    late_start: std::cell::Cell<bool>,
    /// `PTYRO_FAKE_LAGGY_BOX=N` (claude): a taken prompt stays in the box, and `ask`'s dialog
    /// comes N ms after it is taken, before its hooks.
    laggy: Option<u64>,
    /// `PTYRO_FAKE_PLAIN_DIALOG=1`: `ask`'s dialog in words no adapter recognises.
    plain_dialog: bool,
    /// `PTYRO_FAKE_TRANSCRIPT=1`: the conversation's file, as the real agent keeps it.
    script: Option<crate::fake_transcript::Script>,
    /// The `ptyro` MCP server this launch was given.
    mcp: Option<Mcp>,
}

/// An MCP server as a launch names it, and which of its tools run unasked.
#[derive(Default)]
struct Mcp {
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// Claude's `--allowedTools` (`mcp__ptyro__recall`, …).
    allowed: Vec<String>,
    /// Codex's `default_tools_approval_mode = "approve"`.
    approve: bool,
}

impl Mcp {
    /// One call, as an agent makes it: start the server, `initialize`, then the request.
    /// Answers the tool's text (`tools/list`: the names, `[always]` where one is always
    /// loaded, and the server's instructions).
    fn call(&self, tool: &str, args: &Value) -> String {
        let mut c = std::process::Command::new(&self.command);
        c.args(&self.args)
            .envs(self.env.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = match c.spawn() {
            Ok(c) => c,
            Err(e) => return format!("mcp: the server did not start: {e}"),
        };
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return "mcp: no pipes".into();
        };
        let mut lines = std::io::BufReader::new(stdout).lines();
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            if writeln!(stdin, "{msg}").is_err() {
                return Value::Null;
            }
            for line in lines.by_ref().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && v["id"] == id
                {
                    return v;
                }
            }
            Value::Null
        };
        let init = ask(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "fake-agent", "version": "0" } }),
        );
        let said = if tool == "tools/list" {
            let listed = ask(2, "tools/list", json!({}));
            let names: Vec<String> = listed["result"]["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    let always = t["_meta"]["anthropic/alwaysLoad"] == true;
                    format!(
                        "{}{}",
                        t["name"].as_str().unwrap_or(""),
                        if always { " [always]" } else { "" }
                    )
                })
                .collect();
            format!(
                "tools: {}\ninstructions: {}",
                names.join(", "),
                init["result"]["instructions"].as_str().unwrap_or("")
            )
        } else {
            let r = ask(2, "tools/call", json!({ "name": tool, "arguments": args }));
            match r["result"]["content"][0]["text"].as_str() {
                Some(text) if r["result"]["isError"] == true => format!("error: {text}"),
                Some(text) => text.to_owned(),
                None => format!("mcp: {}", r["error"]["message"].as_str().unwrap_or("no answer")),
            }
        };
        drop(stdin);
        let _ = child.wait();
        said
    }
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
            psp: adapter == "claude" || adapter == "opencode",
            oc: None,
            late_start: std::cell::Cell::new(
                std::env::var_os("PTYRO_FAKE_LATE_SESSION_START").is_some(),
            ),
            laggy: ptyro_core::test_knob("PTYRO_FAKE_LAGGY_BOX")
                .filter(|_| adapter == "claude")
                .map(|v| v.parse().unwrap_or(0)),
            plain_dialog: ptyro_core::test_knob("PTYRO_FAKE_PLAIN_DIALOG").is_some(),
            script: None,
            mcp: None,
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
                if let Some(s) = arg_value(args, "--mcp-config")
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .map(|v| v["mcpServers"]["ptyro"].clone())
                    .filter(|s| s["command"].is_string())
                {
                    let strs = |v: &Value| -> Vec<String> {
                        v.as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    };
                    f.mcp = Some(Mcp {
                        command: s["command"].as_str().unwrap_or_default().to_owned(),
                        args: strs(&s["args"]),
                        env: s["env"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                            .collect(),
                        allowed: arg_value(args, "--allowedTools")
                            .map(|a| a.split([',', ' ']).map(str::to_owned).collect())
                            .unwrap_or_default(),
                        approve: false,
                    });
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
                    } else if let Some(key) = k.strip_prefix("mcp_servers.ptyro.") {
                        let m = f.mcp.get_or_insert_with(Mcp::default);
                        match key {
                            "command" => {
                                m.command = t["x"].as_str().unwrap_or_default().to_owned();
                            }
                            "args" => {
                                m.args = t["x"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|s| s.as_str().map(str::to_owned))
                                    .collect();
                            }
                            "env" => {
                                m.env = t["x"]
                                    .as_table()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                                    .collect();
                            }
                            "default_tools_approval_mode" => {
                                m.approve = t["x"].as_str() == Some("approve");
                            }
                            _ => {}
                        }
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
        f.script = crate::fake_transcript::Script::open(&f.adapter, &f.session);
        f
    }

    /// Run the hook for `event` with `payload`, as the agent would (command hooks get JSON on
    /// stdin). Returns what a hook printed
    /// as JSON (a decision), if anything.
    fn hook(&self, event: &str, payload: Value) {
        let _ = self.hook_out(event, payload);
    }

    fn hook_out(&self, event: &str, payload: Value) -> Option<Value> {
        let (cmds, payload) = self.hook_prep(event, payload);
        run_hooks(&cmds, &payload, HOOK_TIMEOUT)
    }

    /// The hook commands for `event` and the payload they get.
    fn hook_prep(&self, event: &str, mut payload: Value) -> (Vec<String>, Value) {
        payload["hook_event_name"] = json!(event);
        payload["cwd"] =
            json!(std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
        let cmds: Vec<String> = match self.adapter.as_str() {
            "claude" | "codex" => self.hooks.get(event).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        payload["session_id"] = json!(self.session);
        if let Some(s) = &self.script {
            payload["transcript_path"] = json!(s.path);
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
        if let Some(why) = Self::denied(self.hook_out("PreToolUse", tool.clone()).as_ref()) {
            return Err(why);
        }
        let out = work();
        if let Some(s) = &self.script {
            s.tool(name, &tool["tool_input"], &out);
        }
        self.hook("PostToolUse", tool);
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
        self.late_session_start(out);
        if let Some(s) = &self.script {
            s.user(prompt);
        }
        self.hook("UserPromptSubmit", p);
        self.oc(json!({ "type": "session.status", "properties": { "sessionID": self.session, "status": { "type": "busy" } } }));
        self.report(out, "state=working:msg=d29ya2luZw");
        let msg = body(out);
        self.report(out, "state=done");
        self.turn_complete(&msg, out);
    }

    /// `PTYRO_FAKE_LATE_SESSION_START=1`, at the first prompt: Codex 0.166's order, OSC 7501's
    /// working (which it sends), then SessionStart, then the prompt's own hooks.
    fn late_session_start(&self, out: &mut std::io::Stdout) {
        if self.late_start.replace(false) {
            let _ = write!(out, "\x1b]7501;state=working:app=fake-{}\x1b\\", self.adapter);
            let _ = out.flush();
            self.hook("SessionStart", json!({ "source": "startup" }));
        }
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

    /// The task (`PTYRO_FAKE_TASK=1`): look around, ask before running a command, then fix the
    /// typo in `greeting.txt` and say so. Told no, it leaves the file alone.
    fn task_turn(&self, out: &mut std::io::Stdout, prompt: &str) {
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
        let dialog = |out: &mut std::io::Stdout| {
            if f.plain_dialog {
                let _ = write!(out, "\r\nRun `{rest}`? Enter runs it, Esc refuses.\r\n");
            } else {
                let _ = write!(
                    out,
                    "\r\nDo you want to proceed?\r\n❯ 1. Yes\r\n  2. Yes, and don't ask again\r\n  3. No (esc)\r\n"
                );
            }
            let _ = out.flush();
        };
        if let Some(ms) = f.laggy {
            std::thread::sleep(Duration::from_millis(ms));
            dialog(out);
        }
        f.hook("PreToolUse", tool.clone());
        // Claude's PermissionRequest runs alongside its dialog (and may answer it).
        let (decided_tx, decided) = std::sync::mpsc::channel();
        match f.adapter.as_str() {
                    "opencode" => f.oc(json!({ "type": "permission.asked", "properties": { "sessionID": f.session, "id": "per_1", "title": format!("Bash: {rest}") } })),
                    "claude" => {
                        let (cmds, payload) = f.hook_prep("PermissionRequest", tool.clone());
                        std::thread::spawn(move || {
                            let _ = decided_tx.send(run_hooks(&cmds, &payload, HELD_TIMEOUT));
                        });
                    }
                    _ => f.hook("PermissionRequest", tool.clone()),
                }
        f.report(out, "state=blocked:kind=permission:msg=YXBwcm92ZQ");
        if f.laggy.is_none() {
            dialog(out);
        }
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
        // Codex has the reply in its file before `Stop`; Claude writes it just after.
        let late = self.script.as_ref().and_then(|s| {
            if self.adapter == "claude" {
                return Some(s.reply_late(msg));
            }
            s.reply(msg);
            None
        });
        match self.adapter.as_str() {
            "codex" => {
                self.hook("Stop", json!({}));
                if let Some((prog, args)) = self.notify.split_first() {
                    let j = json!({ "type": "agent-turn-complete", "thread-id": self.session, "last-assistant-message": msg });
                    let _ = std::process::Command::new(prog).args(args).arg(j.to_string()).status();
                }
            }
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
                        break;
                    };
                    if a["decision"] != "block" {
                        break;
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
        if let (Some(late), Some(s)) = (late, &self.script) {
            let _ = late.join();
            s.turn_ended();
        }
    }
}

/// A command line run as the real agents run theirs (hooks, status lines, the Bash tool): `sh -c`.
fn agent_shell(line: &str) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.args(["-c", line]);
    c
}

/// Claude empties its input box as it takes a prompt, which moves up into the conversation.
/// The fake draws what it took itself (its terminal doesn't echo, as Claude's raw one doesn't),
/// so the cursor is still in the box, right after its `> `: the box's rule and line go, and the
/// prompt is written as the conversation's line. Left in the box, a taken prompt looked unsent,
/// and ptyro pressed Enter again while a slow hook still left the status idle; the dialog that
/// came next took that Enter as "no".
fn take_from_box(out: &mut std::io::Stdout, typed: &str) {
    let _ = write!(out, "\r\x1b[1A\x1b[J> {typed}\r\n");
    let _ = out.flush();
}

/// The rule Claude's input box sits under.
const RULE: &str = "────────────────────────────────────────";

/// Read one key in raw mode (the permission dialog).
/// Run hook commands with `payload` on stdin; the last JSON line any of them prints.
/// How long a hook may run before it is killed and the agent goes on, as real agents do
/// (Codex's `timeout`, Claude's): a shell that hangs as it starts (seen once on a loaded
/// Windows runner, WIN13) costs a turn's hooks, not the session.
const HOOK_TIMEOUT: Duration = Duration::from_secs(30);
/// A `PermissionRequest` hook ptyro holds for a phone's answer (the relay's own bound).
const HELD_TIMEOUT: Duration = Duration::from_secs(3600);

fn run_hooks(cmds: &[String], payload: &Value, limit: Duration) -> Option<Value> {
    let mut answer = None;
    // `PTYRO_FAKE_HOOK_DELAY_MS=N`: each hook takes N ms to start, as on a loaded machine (a
    // macOS runner took over 500 ms), so what ptyro does before a hook arrives is testable.
    // A test knob: a release build never reads it.
    let delay = ptyro_core::test_knob("PTYRO_FAKE_HOOK_DELAY_MS").and_then(|v| v.parse().ok());
    for c in cmds {
        if let Some(ms) = delay {
            std::thread::sleep(Duration::from_millis(ms));
        }
        let Ok(mut child) = agent_shell(c)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.to_string().as_bytes());
        }
        // Its output on a thread, so a hook past its time can be killed.
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(mut stdout) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stdout.read_to_string(&mut text);
                let _ = tx.send(text);
            });
        }
        let deadline = std::time::Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        let text = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
        if let Some(v) =
            text.lines().rev().find_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        {
            answer = Some(v);
        }
    }
    answer
}

/// A key, or a `PermissionRequest` hook's decision, whichever comes first (Claude shows its
/// dialog while the hook runs; verified on 2.1.305). Polls stdin so no reader is left behind to
/// eat the next prompt.
fn key_or_decision(rx: &std::sync::mpsc::Receiver<Option<Value>>) -> Result<u8, bool> {
    let stdin = std::io::stdin();
    let raw = ptyro_pty::console::RawInput::new();
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
        if ptyro_pty::console::input_ready(std::time::Duration::from_millis(50)) {
            let mut b = [0u8; 1];
            let _ = stdin.lock().read(&mut b);
            break Ok(b[0]);
        }
    };
    drop(raw);
    r
}

fn read_key() -> u8 {
    let stdin = std::io::stdin();
    let _raw = ptyro_pty::console::RawInput::new();
    let mut b = [0u8; 1];
    let _ = stdin.lock().read(&mut b);
    b[0]
}

pub fn main(args: &[String]) -> i32 {
    let f = Fake::new(args);
    let mut out = std::io::stdout();
    let _ = writeln!(out, "fake {} session={} resumed={}", f.adapter, f.session, f.resumed);
    // Claude's SessionStart and every Codex hook name the model the agent runs.
    let model = if f.adapter == "claude" { "claude-opus-5-5" } else { "gpt-5.6-sol" };
    // `PTYRO_FAKE_START_GATE=<path>`: it loads (output, every 200 ms) until the file exists, then
    // starts: a test holds its first hook back for as long as it needs.
    if let Some(gate) = ptyro_core::test_knob("PTYRO_FAKE_START_GATE") {
        let mut n = 0u32;
        while !std::path::Path::new(&gate).exists() {
            let _ = write!(out, "\r\x1b[2Kloading {n} {}", "·".repeat(70));
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(200));
            n += 1;
        }
        let _ = write!(out, "\r\x1b[2K");
    }
    if !f.late_start.get() {
        f.hook(
            "SessionStart",
            json!({ "source": if f.resumed { "resume" } else { "startup" }, "model": model }),
        );
    }
    f.oc(json!({ "type": "session.created", "properties": { "info": { "id": f.session } } }));
    f.report(&mut out, "state=idle");
    // Codex 0.166 says idle over OSC 7501 as it starts (and nothing by hook until a prompt).
    if f.adapter == "codex" && f.late_start.get() {
        let _ = write!(out, "\x1b]7501;state=idle:app=fake-codex\x1b\\");
    }
    // `PTYRO_FAKE_PASTE=1`: bracketed paste, as real agents ask for, so a pasted prompt over
    // several lines is one prompt. Off, each line is its own command (the scripted tests).
    if std::env::var("PTYRO_FAKE_PASTE").is_ok() {
        let _ = write!(out, "\x1b[?2004h");
    }
    let stdin = std::io::stdin();
    // Claude draws what is typed into its box itself: the fake does too (`take_from_box`), so a
    // line typed ahead while it works can't land where a taken prompt is redrawn.
    let unechoed =
        (f.adapter == "claude" && f.laggy.is_none()).then(ptyro_pty::console::RawInput::unechoed);
    // `PTYRO_FAKE_DROP_FIRST_ENTER=1`: the first prompt's Enter is lost, its text left in the
    // composer until the next Enter, as Codex 0.166 drops one while busy just after it says idle.
    let mut drop_first = std::env::var_os("PTYRO_FAKE_DROP_FIRST_ENTER").is_some();
    let mut held: Option<String> = None;
    loop {
        // Codex 0.166 says nothing until the first prompt: its composer is how ptyro sees it's
        // ready (`›`).
        let late_codex =
            f.adapter == "codex" && std::env::var_os("PTYRO_FAKE_LATE_SESSION_START").is_some();
        if held.is_none() {
            // Claude's input box sits under a rule, which is how ptyro tells it from the `❯`
            // lines of the conversation above it.
            if f.adapter == "claude" {
                let _ = write!(out, "{RULE}\r\n");
            }
            let _ = write!(out, "{}", if late_codex { "› " } else { "> " });
        }
        let _ = out.flush();
        let mut line = String::new();
        let got = stdin.lock().read_line(&mut line).unwrap_or(0);
        if got == 0 {
            return 0;
        }
        if let Some(h) = held.take() {
            if !line.trim().is_empty() {
                let _ = writeln!(out, "fake: a second prompt while one waits: {}", line.trim());
            }
            line = h;
        } else if drop_first && !line.trim().is_empty() {
            drop_first = false;
            let _ = write!(out, "{}{}", if late_codex { "› " } else { "> " }, line.trim());
            held = Some(line);
            continue;
        } else if unechoed.is_some() && !line.contains("\x1b[200~") {
            take_from_box(&mut out, line.trim_end_matches(['\r', '\n']));
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
                f.late_session_start(&mut out);
                if let Some(s) = &f.script {
                    s.user(&format!("work {rest}"));
                }
                f.hook("UserPromptSubmit", prompt);
                f.oc(json!({ "type": "session.status", "properties": { "sessionID": f.session, "status": { "type": "busy" } } }));
                f.report(&mut out, "state=working:msg=d29ya2luZw");
                let tool = json!({ "tool_name": "Bash", "tool_input": { "command": format!("sleep {secs}") } });
                f.hook("PreToolUse", tool.clone());
                std::thread::sleep(Duration::from_secs_f64(secs));
                f.hook("PostToolUse", tool);
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
                    if let Ok(mut c) = agent_shell(cmd)
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
                        agent_shell(rest)
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
            "/exit" | "/quit" => return 0,
            // `mcp <file> <tool> <json>`: a tool of the `ptyro` server, as the agent calls it.
            "mcp" => {
                let mut parts = rest.splitn(3, ' ');
                let (file, tool) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let args: Value =
                    parts.next().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(json!({}));
                let said = match &f.mcp {
                    None => "no mcp server: this launch was given none".to_owned(),
                    Some(m) if tool == "tools/list" => m.call(tool, &args),
                    // Claude refuses a tool its rules don't allow; Codex one nobody approved.
                    Some(m)
                        if f.adapter == "claude"
                            && !m.allowed.contains(&format!("mcp__ptyro__{tool}")) =>
                    {
                        format!("refused: mcp__ptyro__{tool} is not allowed")
                    }
                    Some(m) if f.adapter == "codex" && !m.approve => {
                        "refused: the call needs an approval".to_owned()
                    }
                    Some(m) => m.call(tool, &args),
                };
                // (Written whole or not at all: whoever waits for it reads it once.)
                let tmp = format!("{file}.tmp");
                let _ = std::fs::write(&tmp, said).and_then(|()| std::fs::rename(&tmp, file));
                let _ = writeln!(out, "mcp done {file}");
            }
            "compact" if f.script.is_some() => {
                if let Some(s) = &f.script {
                    s.compact(rest);
                }
                let _ = writeln!(out, "compacted");
            }
            "rewind" if f.script.is_some() => {
                if let Some(s) = &f.script {
                    s.rewind();
                }
                let _ = writeln!(out, "rewound");
            }
            "" => {}
            // `PTYRO_FAKE_TASK=1`: any prose is a task, done like an agent would.
            _ if std::env::var("PTYRO_FAKE_TASK").is_ok()
                && !line.starts_with("<ptyro-message") =>
            {
                f.task_turn(&mut out, &line);
            }
            _ if line.starts_with("<ptyro-message") || prose => {
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
