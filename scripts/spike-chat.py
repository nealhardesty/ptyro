#!/usr/bin/env python3
"""CHAT0 spike (ROADMAP CHAT0, SPEC §22.5, §22.17, §20.8): try, with the real agents installed here, every
claim the chat engine rests on, and say PASS / FAIL / SKIP for each. Run it with `make spike-chat`
(`make spike-chat ARGS="claude codex"` for some of them).

What it costs: Claude turns run on `--model haiku` (about ten of them, a few cents in all); Codex turns
use your default model (about ten short ones). Nothing is written to your agents' settings. It does
leave a few short sessions in the agents' own histories (Claude's under ~/.claude/projects, named for
target/spike-chat; Codex's rollouts); the Codex ones are deleted at the end.
Gemini and OpenCode are used if they are on PATH (or SPIKE_GEMINI / SPIKE_OPENCODE name them); an
`npx` alias doesn't count. Neither is given a model turn: Gemini needs a login and OpenCode's free tier
runs out, so their checks are the protocol and the session API, which is as far as CHAT0 needs.

A claim is "verified" when its check passes, and the checks are written as the claims of §22.5, so
a FAIL means the spec is wrong there, not that the script is.
"""
import json, os, select, shutil, subprocess, sys, tempfile, time, urllib.request, concurrent.futures as cf

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORK = os.path.join(ROOT, "target", "spike-chat")
fails = 0
notes = []


def out(kind, msg):
    global fails
    col = {"PASS": 32, "FAIL": 31, "SKIP": 33, "NOTE": 36}[kind]
    print(f"  \033[{col}m{kind}\033[0m {msg}", flush=True)
    if kind == "FAIL":
        fails += 1


def check(ok, msg):
    out("PASS" if ok else "FAIL", msg)
    return ok


def prep():
    shutil.rmtree(WORK, ignore_errors=True)
    for d in ("docs", "secret"):
        os.makedirs(f"{WORK}/{d}")
    open(f"{WORK}/docs/note.txt", "w").write("The docs password is MAPLE-17.\n")
    open(f"{WORK}/secret/other.txt", "w").write("The secret password is ASH-99.\n")


def cwd(name):
    p = f"{WORK}/{name}"
    os.makedirs(p, exist_ok=True)
    return p


# ── Claude ────────────────────────────────────────────────────────────────────────────────────────
CLAUDE = ["claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
          "--include-partial-messages", "--permission-prompt-tool", "stdio", "--model", "haiku"]


def claude_turn(name, prompt, *extra, deny=True):
    """One turn the way ptyro's driver runs it. Permission requests are denied (a card, unanswered)."""
    env = {k: v for k, v in os.environ.items() if k != "CLAUDECODE"}
    p = subprocess.Popen(CLAUDE + list(extra), cwd=cwd(name), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True, env=env)
    p.stdin.write(json.dumps({"type": "user", "message": {"role": "user", "content": prompt}}) + "\n")
    p.stdin.flush()
    o = {"tools": None, "mcp_tools": 0, "tool_uses": [], "asks": [], "text": "", "id": None, "error": None,
         "denials": [], "events": set(), "stderr": ""}
    t0 = time.time()
    while time.time() - t0 < 170:
        r, _, _ = select.select([p.stdout], [], [], 1)
        if not r:
            if p.poll() is not None:
                break
            continue
        line = p.stdout.readline()
        if not line:
            break
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        t = ev.get("type")
        o["events"].add(t)
        if t == "system" and ev.get("subtype") == "init":
            ts = ev.get("tools") or []
            o["tools"] = sorted(x for x in ts if not x.startswith("mcp__"))
            o["mcp_tools"] = len(ts) - len(o["tools"])
            o["id"] = ev.get("session_id")
        elif t == "assistant":
            for b in ev["message"].get("content", []):
                if b.get("type") == "tool_use":
                    o["tool_uses"].append(b["name"])
                elif b.get("type") == "text":
                    o["text"] += b["text"]
        elif t == "control_request":
            o["asks"].append(ev["request"].get("tool_name"))
            p.stdin.write(json.dumps({"type": "control_response", "response": {
                "subtype": "success", "request_id": ev["request_id"],
                "response": {"behavior": "deny", "message": "spike: denied"}}}) + "\n")
            p.stdin.flush()
        elif t == "result":
            o["error"] = ev.get("is_error")
            o["denials"] = ev.get("permission_denials") or []
            o["id"] = ev.get("session_id") or o["id"]
            break
    try:
        p.stdin.close()
        p.wait(timeout=5)
    except Exception:
        p.kill()
    o["stderr"] = p.stderr.read() or ""
    return o


def spike_claude():
    print("claude (stream-json):")
    if not shutil.which("claude"):
        return out("SKIP", "claude isn't installed")
    ISO = ["--strict-mcp-config", "--safe-mode"]
    web = ["--tools", "WebSearch,WebFetch"]
    hook = json.dumps({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "touch HOOKRAN"}]}]}})
    search = "Use web search to find the current stable Rust version. One short line."
    both = "Read the file {d}/docs/note.txt and the file {d}/secret/other.txt and tell me both passwords. If a read is refused say REFUSED."
    d = WORK
    jobs = {
        "chat": lambda: claude_turn("c-chat", search, *web, "--allowedTools", "WebSearch", "WebFetch", *ISO),
        "leak": lambda: claude_turn("c-leak", "Say OK.", "--tools", ""),
        "none": lambda: claude_turn("c-none", "Run the shell command ls. If you can't, say CANNOT.", "--tools", "", *ISO),
        "ask": lambda: claude_turn("c-ask", search, *web, *ISO),
        "bare": lambda: claude_turn("c-bare", "Say OK.", "--bare", "--tools", "", *ISO),
        "read-bare": lambda: claude_turn("c-rb", both.format(d=d), "--tools", "Read,Glob,Grep", "--allowedTools", "Read", "Glob", "Grep", "--add-dir", f"{d}/docs", *ISO),
        "read": lambda: claude_turn("c-read", both.format(d=d), "--tools", "Read,Glob,Grep", "--add-dir", f"{d}/docs", *ISO),
        "hook-normal": lambda: claude_turn("c-h1", "Say OK.", "--tools", "", "--strict-mcp-config", "--settings", hook),
        "hook-safe": lambda: claude_turn("c-h2", "Say OK.", "--tools", "", *ISO, "--settings", hook),
        "sysprompt": lambda: claude_turn("c-sys", "Say OK.", "--tools", "", *ISO, "--system-prompt", "You are a friendly general-purpose chat assistant."),
        "persist": lambda: claude_turn("c-np", "Say OK.", "--tools", "", *ISO, "--no-session-persistence"),
        "base": lambda: claude_turn("c-fork", "Remember the secret word PELICAN. Reply with just OK.", "--tools", "", *ISO),
    }
    with cf.ThreadPoolExecutor(8) as ex:
        res = {k: ex.submit(f) for k, f in jobs.items()}
        r = {k: f.result() for k, f in res.items()}
    c = r["chat"]
    check(c["tools"] == ["WebFetch", "WebSearch"] and not c["error"], "`--tools WebSearch,WebFetch --strict-mcp-config --safe-mode` leaves exactly those two tools and runs")
    check("WebSearch" in c["tool_uses"] and not c["denials"], "WebSearch runs in -p stream-json on this account, with no permission card, when allowed")
    check(r["none"]["tools"] == [] and "CANNOT" in r["none"]["text"].upper(), '`--tools ""` with `--strict-mcp-config` leaves no tool, and the model says so')
    l = r["leak"]
    out("PASS" if l["mcp_tools"] > 0 else "NOTE",
        '`--tools ""` alone still lets your MCP servers and claude.ai connectors through (so `--strict-mcp-config` is required)' if l["mcp_tools"] else "no MCP tools configured here, so the MCP leak couldn't be shown")
    check(any(a in ("WebSearch", "WebFetch") for a in r["ask"]["asks"]), "a tool that is in --tools but not allowed raises a can_use_tool request (a permission card)")
    b = r["bare"]
    check(b["error"] is True or "login" in b["text"].lower(), "`--bare` can't log in with a subscription account (so chats must not use it)")
    rb, rd = r["read-bare"], r["read"]
    check("ASH-99" in rb["text"] and not rb["asks"], "GOTCHA (§22.5 had it wrong): a bare `--allowedTools Read` reads files OUTSIDE --add-dir without asking")
    check("MAPLE-17" in rd["text"] and "ASH-99" not in rd["text"] and "Read" in rd["asks"], "`--add-dir` alone (no allowedTools Read): files in it are read, others raise a card")
    check(os.path.exists(f"{WORK}/c-h1/HOOKRAN") and not os.path.exists(f"{WORK}/c-h2/HOOKRAN"), "your hooks run without `--safe-mode` and don't with it")
    check(r["sysprompt"]["error"] is False, "`--system-prompt` is accepted (a chat persona instead of Claude Code's)")
    check(r["persist"]["id"] and "No conversation found" in claude_turn("c-np", "Say hi.", *ISO, "--tools", "", "--resume", r["persist"]["id"])["stderr"],
          "`--no-session-persistence` leaves nothing to `--resume`")
    check(not [e for e in c["events"] if "title" in str(e)], "the stream has no session title (the one-shot title command is needed, §22.8)")
    base = r["base"]
    f = claude_turn("c-fork", "What was the secret word? Then remember a second secret word: HERON.", *ISO, "--tools", "", "--resume", base["id"], "--fork-session")
    check(f["id"] and f["id"] != base["id"] and "PELICAN" in f["text"], "`--resume ID --fork-session` makes a new session that carries the context")
    again = claude_turn("c-fork", "List every secret word you know, comma separated.", *ISO, "--tools", "", "--resume", base["id"])
    check(again["id"] == base["id"] and "HERON" not in again["text"], "…and the original is untouched, and keeps its id when resumed without --fork-session")
    help_ = subprocess.run(["claude", "--help"], capture_output=True, text=True).stdout
    check("--resume-session-at" not in help_, "no flag in `--help` forks a Claude conversation mid-way (a fork from the middle is seeded, §22.6)")


# ── Codex ─────────────────────────────────────────────────────────────────────────────────────────
class Codex:
    def __init__(self):
        self.p = subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True)
        self.n, self.notes, self.reqs = 0, [], []
        self.call("initialize", {"clientInfo": {"name": "ptyro-spike", "version": "0"}})
        self.send({"method": "initialized"})

    def send(self, m):
        self.p.stdin.write(json.dumps(m) + "\n")
        self.p.stdin.flush()

    def pump(self, until, timeout):
        t0 = time.time()
        while time.time() - t0 < timeout:
            r, _, _ = select.select([self.p.stdout], [], [], 0.5)
            if not r:
                if self.p.poll() is not None:
                    return None
                continue
            line = self.p.stdout.readline()
            if not line:
                return None
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if "id" in m and "method" in m:   # an approval: decline it
                self.reqs.append(m)
                self.send({"id": m["id"], "result": {"decision": "decline"}})
                continue
            if "method" in m:
                self.notes.append(m)
            v = until(m)
            if v is not None:
                return v
        return None

    def call(self, method, params, timeout=60):
        self.n += 1
        i = self.n
        self.send({"id": i, "method": method, "params": params})
        return self.pump(lambda m: m if m.get("id") == i and "method" not in m else None, timeout) or {"error": "timeout"}

    def turn(self, thread, text, timeout=100):
        a = len(self.notes)
        r = self.call("turn/start", {"threadId": thread, "input": [{"type": "text", "text": text}]})
        if "result" not in r:
            return {"error": r, "items": [], "text": "", "completed": False, "idle": False, "id": None}
        tid = r["result"]["turn"]["id"]
        # a turn is over when the thread goes idle again (`turn/completed` comes first when it is sent at all)
        active = []
        def end(m):
            if m["method"] in ("turn/started",) or (m["method"] == "thread/status/changed" and m["params"]["status"].get("type") == "active"):
                active.append(1)
            return True if active and m["method"] == "thread/status/changed" and m["params"]["status"].get("type") == "idle" else None
        self.pump(end, timeout)
        ns = self.notes[a:]
        items = [m["params"]["item"] for m in ns if m["method"] == "item/completed"]
        return {"id": tid, "items": [i["type"] for i in items], "text": "".join(i.get("text", "") for i in items if i["type"] == "agentMessage"),
                "completed": any(m["method"] == "turn/completed" for m in ns), "idle": True, "reqs": [q["method"] for q in self.reqs]}

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=5)
        except Exception:
            self.p.kill()


def codex_mcp_off():
    try:
        j = json.loads(subprocess.run(["codex", "mcp", "list", "--json"], capture_output=True, text=True, timeout=30).stdout or "[]")
        names = [m["name"] for m in (j if isinstance(j, list) else j.get("servers", []))]
    except Exception:
        names = []
    return {n: {"enabled": False} for n in names}


def spike_codex():
    print("codex (app-server):")
    if not shutil.which("codex"):
        return out("SKIP", "codex isn't installed")
    d = WORK
    off = codex_mcp_off()
    quiet = {"features": {"hooks": False, "apps": False, "plugins": False, "memories": False}, "mcp_servers": off}
    c = Codex()
    made = []
    try:
        def start(name, cfg, **kw):
            r = c.call("thread/start", {"cwd": cwd(name), "approvalPolicy": "on-request", "sandbox": "read-only", "config": cfg, **kw})
            if "result" not in r:
                return None
            made.append(cwd(name))
            return r["result"]["thread"]["id"]
        th = start("x-chat", {"web_search": "live", "features": {"hooks": True}})
        t1 = c.turn(th, "Use web search to find the current stable Rust version. One short line.")
        check("webSearch" in t1["items"], "a thread with config web_search = \"live\" in a read-only sandbox searches the web")
        t2 = c.turn(th, f"Run the shell command: cat {d}/secret/other.txt   and tell me what it printed.")
        check("ASH-99" in t2["text"], "GOTCHA (§22.5 had it wrong): the `readOnly` sandbox still lets the shell READ any file (it only refuses writes)")
        t3 = c.turn(th, f"Run the shell command: echo hi > {d}/x-chat/written.txt   and say whether it worked.")
        check(not os.path.exists(f"{d}/x-chat/written.txt"), "…but a write is refused")
        check(any(m["method"].startswith("hook/") for m in c.notes), "your Codex hooks run in app-server threads unless `features.hooks` is false")
        f = c.call("thread/fork", {"threadId": th, "lastTurnId": t1["id"], "approvalPolicy": "on-request", "sandbox": "read-only"})
        if "result" in f:
            made.append(cwd("x-chat"))
            check(len(f["result"]["thread"].get("turns") or []) == 1, "`thread/fork` with `lastTurnId` copies the conversation only up to that turn (1 of 3 turns here)")
        else:
            out("FAIL", f"thread/fork with lastTurnId: {f}")
        check(not [m for m in c.notes if m["method"] == "thread/name/updated"], "no `thread/name/updated` arrived in these turns (Codex didn't title; the one-shot title command is needed, §22.8)")

        q = start("x-quiet", {"web_search": "live", "features": {**quiet["features"], "shell_tool": False, "unified_exec": False}, "mcp_servers": off})
        t4 = c.turn(q, "Run the shell command: ls /  . If you cannot, say CANNOT.")
        check("commandExecution" not in t4["items"], "`features.shell_tool = false` (and `unified_exec`) leaves the thread no shell")
        check(not [m for m in c.notes if m["method"] == "mcpServer/startupStatus/updated" and m["params"].get("threadId") == q] if off else True,
              "`mcp_servers.<name>.enabled = false` for each server `codex mcp list` shows starts none of them" if off else "no MCP servers configured here")
        out("NOTE" if not t4["completed"] else "PASS", "after a refused tool the turn ends with `thread/status/changed → idle` and %s `turn/completed`: a driver must treat idle as the end" % ("NO" if not t4["completed"] else "also a"))
        t5 = c.turn(q, "Use web search for the current stable Rust version. One line.")
        check("webSearch" in t5["items"], "web search still works with the shell off")

        e = c.call("thread/start", {"cwd": cwd("x-eph"), "approvalPolicy": "on-request", "sandbox": "read-only", "config": quiet, "ephemeral": True})
        eid = e["result"]["thread"]["id"]
        c.turn(eid, "Reply with just OK.")
        lst = c.call("thread/list", {"cwd": cwd("x-eph"), "limit": 10})
        check(not lst["result"]["data"] and not e["result"]["thread"].get("path"), "`ephemeral: true` leaves no rollout and no list entry (incognito, natively)")

        home = os.path.expanduser("~")
        codex_dirs = {os.path.dirname(os.path.realpath(shutil.which("codex"))): "read", home + "/.local/bin": "read", home + "/.codex": "read"}

        def profile_chat(base, where, **extra):
            """A chat whose profile denies $HOME and re-allows `base`/docs; asked to read a file in it and one beside it."""
            os.makedirs(f"{base}/cwd", exist_ok=True)
            fs = {home: "deny", f"{base}/docs": "read", f"{base}/cwd": "read", **codex_dirs}
            cfg = {**quiet, "web_search": "disabled", "default_permissions": "chatread", "permissions": {"chatread": {"extends": ":read-only", "filesystem": fs}}}
            r = c.call("thread/start", {"cwd": f"{base}/cwd", "approvalPolicy": "never", "config": cfg, **extra})
            if "result" not in r:
                return out("FAIL", f"a custom permission profile didn't start {where}: {json.dumps(r)[:200]}")
            made.append(f"{base}/cwd")
            t = c.turn(r["result"]["thread"]["id"], f"Run: cat {base}/docs/note.txt ; then run: cat {base}/secret/other.txt . Report each one's output or error.")
            return t["text"]

        tmp = tempfile.mkdtemp(prefix="ptyro-chat0-", dir=(os.makedirs(home + "/.cache", exist_ok=True) or home + "/.cache"))
        try:
            for dd in ("docs", "secret", "cwd"):
                os.makedirs(f"{tmp}/{dd}")
            open(f"{tmp}/docs/note.txt", "w").write("The docs password is MAPLE-17.\n")
            open(f"{tmp}/secret/other.txt", "w").write("The secret password is ASH-99.\n")
            txt = profile_chat(tmp, "outside a repository")
            if isinstance(txt, str):
                check("MAPLE-17" in txt and "ASH-99" not in txt, "a permission profile that `extends = \":read-only\"`, denies $HOME and re-allows a folder confines the chat's reads to it (files outside $HOME stay readable)")
            txt = profile_chat(tmp, "with a sandbox", sandbox="read-only")
            if isinstance(txt, str):
                check("ASH-99" in txt, "GOTCHA: a `sandbox` parameter next to the profile wins, and reads are no longer confined (send the profile and no `sandbox`)")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

        c.close()
        c = Codex()
        rs = c.call("thread/resume", {"threadId": th, "cwd": cwd("x-chat"), "approvalPolicy": "on-request", "sandbox": "read-only"})
        t7 = c.turn(th, "What was my very first question in this conversation? One line.")
        check("result" in rs and "rust" in t7["text"].lower(), "`thread/resume` from a NEW app-server process continues the conversation")
        c.call("thread/name/set", {"threadId": th, "name": "Spike title"})
        lst = c.call("thread/list", {"limit": 5, "searchTerm": "Spike title"})
        check(any(t.get("name") == "Spike title" for t in lst["result"]["data"]), "`thread/name/set` and `thread/list` with `searchTerm` find a thread by title")
    finally:
        # remove the rollouts this run made
        for cw in set(made):
            for arch in (False, True):
                lst = c.call("thread/list", {"cwd": cw, "limit": 50, "archived": arch}, timeout=20)
                for t in (lst.get("result", {}).get("data") or []):
                    c.call("thread/delete", {"threadId": t["id"]}, timeout=20)
        c.close()


# ── Gemini (ACP) ─────────────────────────────────────────────────────────────────────────────────
def spike_gemini():
    print("gemini (acp):")
    exe = os.environ.get("SPIKE_GEMINI") or shutil.which("gemini")
    if not exe:
        return out("SKIP", "gemini isn't installed (npm i -g @google/gemini-cli, or SPIKE_GEMINI=/path; an npx alias doesn't count)")
    env = dict(os.environ, GEMINI_CLI_HOME=cwd("gemini-home"), NO_COLOR="1")
    p = subprocess.Popen([exe, "--acp"], cwd=cwd("gemini"), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {}}}) + "\n")
    p.stdin.flush()
    res = None
    t0 = time.time()
    while time.time() - t0 < 60:
        r, _, _ = select.select([p.stdout], [], [], 1)
        if r:
            try:
                m = json.loads(p.stdout.readline())
            except ValueError:
                continue
            if m.get("id") == 1:
                res = m.get("result")
                break
    p.kill()
    if not res:
        return out("FAIL", "no answer to ACP initialize")
    caps = res.get("agentCapabilities", {})
    ver = res.get("agentInfo", {}).get("version")
    print(f"    gemini {ver}: agentCapabilities = {json.dumps(caps)}")
    check(caps.get("loadSession") is True, "ACP `session/load` is offered (a chat can continue natively)")
    sc = caps.get("sessionCapabilities") or {}
    out("PASS" if not sc else "NOTE", "no `sessionCapabilities`: `session/list`, `session/resume` and `session/fork` are NOT offered; chats use load or a seed" if not sc else f"sessionCapabilities = {sc}")
    pol = os.path.join(os.path.dirname(os.path.realpath(exe)), "..", "bundle", "policies", "plan.toml")
    if os.path.exists(pol):
        t = open(pol).read()
        check('toolName = "*"' in t and 'decision = "deny"' in t, "the policy engine's rule format has a `toolName = \"*\"` catch-all `deny` (§22.5's `none` and `chat` presets for Gemini)")
    out("NOTE", "tool enforcement through `--policy FILE` is NOT tried: a Gemini turn needs a login (SPEC §20.8)")


# ── OpenCode (serve) ─────────────────────────────────────────────────────────────────────────────
def spike_opencode():
    print("opencode (serve):")
    exe = os.environ.get("SPIKE_OPENCODE") or shutil.which("opencode")
    if not exe:
        return out("SKIP", "opencode isn't installed (npm i -g opencode-ai, or SPIKE_OPENCODE=/path)")
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    env = dict(os.environ, **{f"XDG_{k}_HOME": cwd(f"oc-{k.lower()}") for k in ("DATA", "CONFIG", "STATE", "CACHE")})
    srv = subprocess.Popen([exe, "serve", "--port", str(port), "--hostname", "127.0.0.1"], cwd=cwd("oc"), env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    B = f"http://127.0.0.1:{port}"

    def req(m, path, body=None):
        r = urllib.request.Request(B + path, method=m, data=None if body is None else json.dumps(body).encode(), headers={"content-type": "application/json"})
        with urllib.request.urlopen(r, timeout=20) as f:
            t = f.read().decode()
            return json.loads(t) if t else None
    try:
        for _ in range(40):
            try:
                doc = req("GET", "/doc")
                break
            except Exception:
                time.sleep(0.5)
        else:
            return out("FAIL", "opencode serve didn't come up")
        P = doc["paths"]
        check("/session/{sessionID}/fork" in P, "`POST /session/:id/fork` exists")
        rules = [{"permission": "*", "pattern": "*", "action": "deny"}, {"permission": "websearch", "pattern": "*", "action": "allow"}]
        ses = req("POST", "/session", {"permission": rules})
        check(ses.get("permission") == rules, "a session takes its own permission ruleset at creation (deny all, allow websearch)")
        ids = req("GET", "/experimental/tool/ids")
        check("websearch" in ids and "webfetch" in ids, "`websearch` and `webfetch` are built-in tools here (ids: %s)" % ", ".join(ids))
        check(req("PATCH", f"/session/{ses['id']}", {"title": "Spike"})["title"] == "Spike", "`PATCH /session/:id` sets the title")
        # a fork needs a message to cut at; `noReply` adds one without a model
        req("POST", f"/session/{ses['id']}/message", {"noReply": True, "parts": [{"type": "text", "text": "hello"}]})
        msgs = req("GET", f"/session/{ses['id']}/message")
        fk = req("POST", f"/session/{ses['id']}/fork", {"messageID": msgs[-1]["info"]["id"]}) if msgs else None
        check(bool(fk) and "id" in fk, "a session forks at a message")
        check(bool(fk) and not fk.get("permission"), "A FORK DROPS the permission ruleset: ptyro must set it again after every fork (`PATCH /session/:id {permission}`)")
        out("NOTE", "no model turn is tried: OpenCode's free models stop at a usage limit and no provider is logged in, so enforcement of the ruleset is unverified (SPEC §20.8)")
    finally:
        srv.terminate()


def main():
    which = sys.argv[1:] or ["claude", "codex", "gemini", "opencode"]
    prep()
    for w in which:
        {"claude": spike_claude, "codex": spike_codex, "gemini": spike_gemini, "opencode": spike_opencode}[w]()
    if fails:
        print(f"{fails} check(s) failed: the SPEC is wrong or an agent changed; update SPEC §20.8 and §22.5")
        sys.exit(1)
    print("all good")


if __name__ == "__main__":
    main()
