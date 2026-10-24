// The connection to `ptyro serve`: the ptyro protocol (JSON encoding) over one WebSocket, signed
// in with the device token from `ptyro pair`. Requests resolve with the reply or reject with the
// daemon's error (its `code` kept).
export const PROTO = { major: 1, minor: 8 };
export const token = () => localStorage.getItem("ptyro.token");
export class Conn {
    ws = null;
    nextId = 1;
    pending = new Map();
    listeners = new Set();
    state = "closed";
    reason = "";
    /// What this device may do (`read`, `approve`, `control`, `full`), from the welcome.
    scope = "";
    backoff = 500;
    onState = () => { };
    connect() {
        const t = token();
        if (!t)
            return;
        this.state = "connecting";
        this.onState();
        const ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/ws");
        this.ws = ws;
        ws.onopen = () => {
            ws.send(JSON.stringify({ t: "hello", proto: PROTO, client: { kind: "web", version: "web", pid: 0, token: t }, features: [] }));
        };
        ws.onmessage = (ev) => {
            let msg;
            try {
                msg = JSON.parse(typeof ev.data === "string" ? ev.data : "");
            }
            catch {
                return;
            }
            if (msg.t === "welcome") {
                this.state = "open";
                this.scope = msg.scope ?? "full";
                this.backoff = 500;
                this.onState();
                ws.send(JSON.stringify({ t: "watch_events" }));
                for (const l of this.listeners)
                    l({ t: "connected" });
            }
            else if (msg.t === "reject") {
                this.state = "rejected";
                this.reason = msg.reason;
                this.onState();
            }
            else if (msg.t === "response") {
                const p = this.pending.get(msg.id);
                if (p) {
                    this.pending.delete(msg.id);
                    p(msg.outcome);
                }
            }
            else if (msg.t === "goodbye") {
                this.reason = msg.reason;
                this.backoff = 300; // an upgrade: the next image answers in a moment
            }
            else {
                for (const l of this.listeners)
                    l(msg);
            }
        };
        ws.onclose = () => {
            if (this.ws !== ws)
                return; // replaced by a newer connection (a new pairing)
            for (const p of this.pending.values())
                p({ t: "err", error: { code: "daemon_unavailable", message: "connection lost" } });
            this.pending.clear();
            if (this.state === "rejected")
                return;
            this.state = "closed";
            this.onState();
            setTimeout(() => this.connect(), this.backoff);
            this.backoff = Math.min(this.backoff * 2, 10_000);
        };
    }
    send(msg) {
        if (this.ws?.readyState === WebSocket.OPEN)
            this.ws.send(JSON.stringify(msg));
    }
    request(req) {
        return new Promise((ok, err) => {
            if (this.state !== "open")
                return err(new Error("not connected"));
            const id = this.nextId++;
            this.pending.set(id, (o) => (o.t === "ok" ? ok(o.reply) : err(Object.assign(new Error(o.error.message), { code: o.error.code }))));
            this.send({ t: "request", id, req });
        });
    }
}
export const conn = new Conn();
