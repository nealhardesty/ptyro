// The connection to `ptyro serve`: the ptyro protocol (JSON encoding) over one WebSocket, signed
// in with the device token from `ptyro pair`. Requests resolve with the reply or reject with the
// daemon's error (its `code` kept).

import type { Outcome } from "./screen.js";

export const PROTO = { major: 1, minor: 8 };
/// `localStorage`, which can throw (blocked site data, a private window): nothing here may depend on it.
export const store = {
  get(k: string): string | null { try { return localStorage.getItem(k); } catch { return null; } },
  set(k: string, v: string): void { try { localStorage.setItem(k, v); } catch { /* private window */ } },
  remove(k: string): void { try { localStorage.removeItem(k); } catch { /* private window */ } },
};
export const token = (): string | null => store.get("ptyro.token");

export type Listener = (msg: any) => void;

export class Conn {
  ws: WebSocket | null = null;
  nextId = 1;
  pending = new Map<number, (o: Outcome) => void>();
  listeners = new Set<Listener>();
  state: "connecting" | "open" | "closed" | "rejected" = "closed";
  reason = "";
  /// What this device may do (`read`, `approve`, `control`, `full`), from the welcome.
  scope = "";
  backoff = 500;
  onState: () => void = () => {};
  /// A message that was meant for the daemon and couldn't be sent (the socket is down).
  onDropped: (msg: any) => void = () => {};
  /// When anything last arrived: a long silence is checked with a question.
  lastRx = Date.now();

  connect(): void {
    const t = token();
    if (!t) return;
    if (this.ws && this.ws.readyState <= WebSocket.OPEN) return; // already on its way, or there
    this.state = "connecting";
    this.onState();
    const ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/ws");
    this.ws = ws;
    ws.onopen = () => {
      ws.send(JSON.stringify({ t: "hello", proto: PROTO, client: { kind: "web", version: "web", pid: 0, token: t }, features: [] }));
    };
    ws.onmessage = (ev) => {
      this.lastRx = Date.now();
      let msg: any;
      try { msg = JSON.parse(typeof ev.data === "string" ? ev.data : ""); } catch { return; }
      if (msg.t === "welcome") {
        this.state = "open";
        this.scope = msg.scope ?? "full";
        this.backoff = 500;
        this.onState();
        ws.send(JSON.stringify({ t: "watch_events" }));
        for (const l of this.listeners) l({ t: "connected" });
      } else if (msg.t === "reject") {
        this.state = "rejected";
        this.reason = msg.reason;
        this.onState();
      } else if (msg.t === "response") {
        const p = this.pending.get(msg.id);
        if (p) { this.pending.delete(msg.id); p(msg.outcome); }
      } else if (msg.t === "goodbye") {
        this.reason = msg.reason;
        this.backoff = 300; // an upgrade: the next image answers in a moment
      } else {
        for (const l of this.listeners) l(msg);
      }
    };
    ws.onclose = () => this.lost(ws);
  }

  /// Drop the connection on purpose (a new pairing): what waits on it fails, and `connect()` starts afresh.
  reset(): void {
    const ws = this.ws;
    this.ws = null;
    try { ws?.close(); } catch { /* already closing */ }
    for (const p of this.pending.values()) p({ t: "err", error: { code: "daemon_unavailable", message: "connection lost" } });
    this.pending.clear();
    this.state = "closed";
    this.backoff = 500;
  }

  /// The socket is gone (closed, or found dead): fail what waits on it and reconnect.
  private lost(ws: WebSocket): void {
    if (this.ws !== ws) return; // replaced by a newer connection (a new pairing), or already handled
    this.ws = null;
    for (const p of this.pending.values()) p({ t: "err", error: { code: "daemon_unavailable", message: "connection lost" } });
    this.pending.clear();
    if (this.state === "rejected") return;
    this.state = "closed";
    this.onState();
    setTimeout(() => this.connect(), this.backoff);
    this.backoff = Math.min(this.backoff * 2, 10_000);
  }

  /// Is the connection alive? A phone that changed network or woke from sleep can hold a socket
  /// that looks open and carries nothing; ask something cheap and, with no answer, start over.
  check(): void {
    if (this.state !== "open" || !this.ws) return;
    const ws = this.ws;
    this.request({ t: "status" }, 4000).catch(() => {
      if (this.ws !== ws) return;
      try { ws.close(); } catch { /* already closing */ }
      this.lost(ws);
    });
  }

  /// Send a message; false (and `onDropped`) when the socket isn't open.
  send(msg: object): boolean {
    if (this.ws?.readyState === WebSocket.OPEN) { this.ws.send(JSON.stringify(msg)); return true; }
    this.onDropped(msg);
    return false;
  }

  /// A request and its reply. No answer within `timeoutMs` (default two minutes; none for a
  /// request that says how long it will wait) rejects, and checks the connection.
  request(req: object, timeoutMs = 120_000): Promise<any> {
    return new Promise((ok, err) => {
      if (this.state !== "open") return err(new Error("not connected"));
      const id = this.nextId++;
      const waits = "wait_ready_ms" in req || "timeout_ms" in req;
      const timer = waits ? 0 : window.setTimeout(() => {
        if (this.pending.delete(id)) { err(Object.assign(new Error("no answer: the connection may be down"), { code: "timeout" })); this.check(); }
      }, timeoutMs);
      this.pending.set(id, (o) => { clearTimeout(timer); o.t === "ok" ? ok(o.reply) : err(Object.assign(new Error(o.error.message), { code: o.error.code })); });
      this.send({ t: "request", id, req });
    });
  }
}

export const conn = new Conn();

// Back from sleep, on a new network, or quiet for a minute: is anyone there?
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") conn.check(); });
window.addEventListener("online", () => conn.check());
setInterval(() => { if (document.visibilityState === "visible" && Date.now() - conn.lastRx > 60_000) conn.check(); }, 20_000);


/// Something the machine does for the app (`POST /api/host`, `crates/ptyro/src/host.rs`): the
/// config file, the doctor, profiles… A device with `full` scope only.
export async function host(op: string, args: Record<string, unknown> = {}): Promise<any> {
  const r = await fetch("/api/host", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token() ?? ""}` },
    body: JSON.stringify({ op, ...args }),
  });
  if (!r.ok) throw new Error(r.status === 401 ? "this device isn't paired any more" : await r.text());
  const o = await r.json();
  if (o.t === "err") throw Object.assign(new Error(o.error.message), { code: o.error.code });
  return o.reply;
}
