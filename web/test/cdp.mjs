// A small Chrome DevTools Protocol client for web/test/e2e.mjs: start a headless Chrome of our
// own (a fresh profile directory, never the user's), open tabs, evaluate page code, press real
// keys, take a screenshot. No dependencies: Node's own fetch and WebSocket.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// Poll `fn` until it returns something truthy, or fail with `what` after `ms`.
export async function until(what, fn, ms = 20_000, every = 50) {
  const end = Date.now() + ms;
  let last;
  for (;;) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}${last instanceof Error ? `: ${last.message}` : ""}`);
    await sleep(every);
  }
}

export class Tab {
  constructor(ws, id) {
    this.ws = ws;
    this.id = id;
    this.next = 1;
    this.pending = new Map();
    this.events = [];
    ws.addEventListener("message", (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { ok, fail } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? fail(new Error(`${msg.error.message}`)) : ok(msg.result);
      } else if (msg.method) {
        this.events.push(msg);
        if (this.events.length > 500) this.events.shift();
      }
    });
  }

  send(method, params = {}) {
    const id = this.next++;
    return new Promise((ok, fail) => {
      this.pending.set(id, { ok, fail });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  /// Evaluate an expression in the page; it may be async. Returns its value (JSON-able).
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(`page: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
    return r.result.value;
  }

  /// Wait for a page expression to be truthy.
  waitFor(what, expression, ms = 20_000) {
    return until(what, () => this.eval(expression), ms);
  }

  async goto(url) {
    await this.send("Page.navigate", { url });
  }

  /// Press one key the way a keyboard does: down (with its text), then up.
  async key(key, { text, code, vk } = {}) {
    const t = text ?? (key.length === 1 ? key : undefined);
    const base = { key, code: code ?? (key.length === 1 ? `Key${key.toUpperCase()}` : key), windowsVirtualKeyCode: vk ?? key.toUpperCase().charCodeAt(0) };
    await this.send("Input.dispatchKeyEvent", { type: t ? "keyDown" : "rawKeyDown", ...base, text: t });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  }

  async type(text) {
    for (const c of text) await this.key(c, c === " " ? { code: "Space", vk: 32 } : c.match(/[a-z0-9]/i) ? {} : { code: "Unidentified", vk: c.charCodeAt(0) });
  }

  enter() { return this.key("Enter", { text: "\r", code: "Enter", vk: 13 }); }

  /// A click at the middle of the first element matching `selector` (a real mouse event).
  async click(selector) {
    const box = await this.eval(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return null; e.scrollIntoView({ block: "center" }); const r = e.getBoundingClientRect(); return r.width && r.height ? { x: r.left + r.width / 2, y: r.top + r.height / 2 } : null; })()`);
    if (!box) throw new Error(`nothing to click for ${selector}`);
    for (const type of ["mousePressed", "mouseReleased"]) await this.send("Input.dispatchMouseEvent", { type, ...box, button: "left", clickCount: 1 });
  }

  /// Look like a phone: its width, a touch screen (pointers of type "touch").
  async phone(width = 390, height = 844) {
    await this.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 3, mobile: true });
    await this.send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
  }

  /// Fingers on the screen: each step is a list of points ([x, y]), one per finger, the first
  /// step where they land and the last where they lift. Real touch events, so the page sees
  /// pointer events of type "touch" (and the browser does its own zooming, if the page lets it).
  async fingers(steps, ms = 16) {
    const pts = (s) => s.map(([x, y], id) => ({ x, y, id, radiusX: 4, radiusY: 4, force: 1 }));
    await this.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: pts(steps[0]) });
    for (const s of steps.slice(1)) { await sleep(ms); await this.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: pts(s) }); }
    await sleep(ms);
    await this.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  }

  async screenshot(file) {
    const r = await this.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(file, Buffer.from(r.data, "base64"));
    return file;
  }

  close() { try { this.ws.close(); } catch { /* gone */ } }
}

export class Chrome {
  /// Start Chrome (`exe`) headless with a profile of its own under the temp directory.
  static async start(exe) {
    const dir = mkdtempSync(join(tmpdir(), "ptyro-e2e-chrome-"));
    const child = spawn(exe, [
      "--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run",
      "--no-default-browser-check", "--disable-gpu", "--disable-extensions", "--disable-background-networking",
      "--no-sandbox", "--window-size=1100,900", "about:blank",
    ], { stdio: ["ignore", "ignore", "pipe"] });
    let err = "";
    child.stderr.on("data", (d) => { err += d; });
    const port = await until("Chrome's DevTools port", () => {
      if (child.exitCode !== null) throw new Error(`Chrome exited (${child.exitCode}): ${err.slice(-400)}`);
      try { return Number(readFileSync(join(dir, "DevToolsActivePort"), "utf8").split("\n")[0]); } catch { return 0; }
    }, 30_000);
    return Object.assign(new Chrome(), { child, dir, port });
  }

  /// A new tab, attached.
  async tab(url = "about:blank") {
    const r = await fetch(`http://127.0.0.1:${this.port}/json/new?${encodeURIComponent(url)}`, { method: "PUT" });
    const target = await r.json();
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((ok, fail) => { ws.addEventListener("open", ok); ws.addEventListener("error", () => fail(new Error("DevTools socket"))); });
    const tab = new Tab(ws, target.id);
    await tab.send("Page.enable");
    await tab.send("Runtime.enable");
    await tab.send("Network.enable");
    return tab;
  }

  async closeTab(tab) {
    tab.close();
    await fetch(`http://127.0.0.1:${this.port}/json/close/${tab.id}`).catch(() => {});
  }

  /// Stop the Chrome we started, and remove its profile.
  async stop() {
    this.child.kill("SIGTERM");
    await Promise.race([new Promise((r) => this.child.once("exit", r)), sleep(5000)]);
    if (this.child.exitCode === null) this.child.kill("SIGKILL");
    try { rmSync(this.dir, { recursive: true, force: true }); } catch { /* still busy: the temp dir sweeps it */ }
  }
}
