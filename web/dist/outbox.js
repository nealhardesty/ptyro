// Every key exactly once (ROADMAP WEB2.3; SPEC §16.4 WEB1.8, "Every key exactly once"), with no
// DOM in it so web/test/outbox.mjs can drive it. A session's keys, pastes and clicks are numbered
// in a stream of this page's own (`ClientMsg::Keys`) and kept until the daemon says they are
// settled (`ServerMsg::Typed`). Keys typed while the connection is down, or unanswered for a
// while, wait here and are shown as waiting; on reconnect (or after an unanswered wait) they go
// again, numbered as before, so a copy the daemon already has is dropped. The daemon types them
// only if the first was typed at most `REPLAY_WINDOW_MS` ago and the session still waits for what
// it did (its `state_token`, carried by each key); otherwise none of them is typed, and nothing
// typed after them goes either (half a command is worse than none): they are kept as `lost`, to
// be said, shown and typed again on purpose. A daemon that can't number input (older than 1.52)
// gets plain `Input`, as before, and nothing waits.
/// `ptyro_protocol::REPLAY_WINDOW_MS`: keys older than this when sent again are refused.
export const REPLAY_WINDOW_MS = 5000;
/// `ptyro_protocol::MAX_SEQ_KEYS`: the most events one message carries.
export const MAX_SEQ_KEYS = 4096;
/// Unanswered this long: shown as waiting, and the connection asked whether it is alive.
export const WAIT_MS = 1200;
/// Unanswered this long on an open connection: sent again (a copy the daemon has is dropped).
export const RESEND_MS = 2500;
/// No answer at all this long on an open connection: given up on, and said.
export const GIVE_UP_MS = 15_000;
/// What a daemon calls the feature (`ptyro_protocol::FEATURE_INPUT_SEQ`).
export const FEATURE = "input_seq";
const randomId = () => {
    const a = new Uint8Array(12);
    try {
        crypto.getRandomValues(a);
    }
    catch {
        for (let i = 0; i < a.length; i++)
            a[i] = Math.floor(Math.random() * 256);
    }
    return [...a].map((b) => b.toString(16).padStart(2, "0")).join("");
};
/// Clicks, drags, the wheel and focus are aimed at a screen as it was: never kept to send later.
const positional = (event) => event?.t === "mouse" || event?.t === "focus";
export class Outbox {
    session;
    io;
    stream;
    next = 1;
    /// Not settled yet, in order.
    queue = [];
    /// Not sent, newest last (a few kept).
    lost = [];
    online = false;
    mode = "unknown";
    /// A replay is out on this connection, covering numbers up to this; what comes after waits for
    /// its answer (if it is refused, they go too).
    replayTo = 0;
    /// Keys were lost while offline: everything typed until the connection is back is lost too.
    closed = false;
    /// When this connection's welcome came: keys typed before it go only in a replay.
    since = 0;
    constructor(session, io, stream = randomId()) {
        this.session = session;
        this.io = io;
        this.stream = stream;
    }
    /// Keys waiting to be sent or answered.
    get waiting() { return this.queue.length; }
    /// Should the page say they wait? (Offline, or unanswered for a moment: a key on its way isn't news.)
    get shown() {
        if (!this.queue.length)
            return false;
        return !this.online || this.io.now() - this.queue[0].at >= WAIT_MS;
    }
    /// An input event typed into the session. Its number, when it has one (WEB2.4's predictions
    /// learn from `Typed` when it was typed); null for an older daemon's plain input, or when dropped.
    type(event) {
        if (this.mode === "plain") {
            // An older daemon: as before (dropped while offline, and the page says so).
            this.io.send({ t: "input", session: this.session, event });
            return null;
        }
        if (positional(event) && (!this.online || this.mode !== "numbered"))
            return null;
        const now = this.io.now();
        const k = { n: this.next++, event, token: this.io.token(), at: now, sent: null, went: false };
        if (this.closed) {
            this.lose([k], "after", "keys before them weren't sent");
            return null;
        }
        this.queue.push(k);
        if (this.online && this.mode === "numbered" && !this.replayTo) {
            if (this.io.send({ t: "keys", session: this.session, stream: this.stream, keys: [{ n: k.n, event }] })) {
                k.sent = now;
                k.went = true;
            }
        }
        if (!this.online)
            this.io.check();
        this.io.later(this.online ? WAIT_MS : 250);
        this.io.changed();
        return k.n;
    }
    /// A welcome: `numbered` when it lists the feature. What waits goes again now.
    connected(numbered) {
        this.online = true;
        this.closed = false;
        this.replayTo = 0;
        this.since = this.io.now();
        for (const k of this.queue)
            k.sent = null;
        this.mode = numbered ? "numbered" : "plain";
        if (this.queue.length) {
            if (numbered)
                this.replay();
            else
                this.lose(this.queue.splice(0), "old", "this ptyro can't check keys typed while offline");
        }
        this.io.later(WAIT_MS);
        this.io.changed();
    }
    /// The connection went: what waits goes again on the next one.
    disconnected() {
        if (!this.online)
            return;
        this.online = false;
        this.replayTo = 0;
        for (const k of this.queue)
            k.sent = null;
        if (this.queue.length)
            this.io.check();
        this.io.later(250);
        this.io.changed();
    }
    /// `ServerMsg::Typed` for this session (another stream's is ignored).
    typed(msg) {
        if (msg.stream !== this.stream)
            return;
        const rs = (Array.isArray(msg.refused) ? msg.refused : msg.refused ? [msg.refused] : []).slice().sort((a, b) => a.from - b.from);
        if (rs.length) {
            // One notice for them all, named by the first refusal (the rest follow from it).
            const inside = (k) => rs.some((r) => k.n >= r.from && k.n <= r.to);
            const gone = this.queue.filter(inside);
            this.queue = this.queue.filter((k) => !inside(k));
            const why = rs.find((r) => gone.some((k) => k.n >= r.from && k.n <= r.to)) ?? rs[0];
            if (gone.length)
                this.lose(gone, why.why, why.reason);
            // And everything after them that waits (half a command is worse than none).
            const after = this.queue.filter((k) => k.n > rs[0].from);
            this.queue = this.queue.filter((k) => k.n < rs[0].from);
            if (after.length)
                this.lose(after, "after", "keys before them weren't sent");
        }
        this.queue = this.queue.filter((k) => k.n > msg.upto);
        if (this.replayTo && msg.upto >= this.replayTo) {
            this.replayTo = 0;
            this.flush();
        }
        this.io.changed();
    }
    /// Time passed: show what waits, check the connection, send again, give up.
    tick() {
        const now = this.io.now();
        if (!this.queue.length)
            return;
        const head = this.queue[0];
        if (!this.online) {
            // Too old to be typed: the daemon would refuse them. Never sent, so never typed: say so
            // now. Ones that went may have been typed, so they wait to be asked about.
            if (now - head.at > REPLAY_WINDOW_MS && !this.queue.some((k) => k.went)) {
                this.lose(this.queue.splice(0), "stale", "you were offline too long");
                this.closed = true;
                this.io.changed();
                return;
            }
            this.io.check();
            this.io.later(500);
            this.io.changed();
            return;
        }
        if (this.mode !== "numbered")
            return;
        const out = Math.min(...this.queue.map((k) => (k.sent ?? now)));
        // (Counted from this connection's start for keys older than it.)
        if (now - Math.max(head.at, this.since) >= GIVE_UP_MS && now - out >= RESEND_MS) {
            this.lose(this.queue.splice(0), "silent", "no word whether they arrived");
            this.replayTo = 0;
            this.io.changed();
            return;
        }
        if (now - out >= RESEND_MS)
            this.replay();
        else if (now - head.at >= WAIT_MS)
            this.io.check();
        this.io.later(Math.max(200, RESEND_MS - (now - out)));
        this.io.changed();
    }
    /// Forget the lost keys (the person dismissed them, or typed them again).
    dismiss() {
        this.lost = [];
        this.io.changed();
    }
    /// Everything waiting, again, as one replay with what each key was typed against.
    replay() {
        const now = this.io.now();
        const keys = this.queue.slice(0, MAX_SEQ_KEYS);
        const msg = {
            t: "keys", session: this.session, stream: this.stream, replay: true,
            keys: keys.map((k) => ({ n: k.n, event: k.event, ...(k.token ? { token: k.token } : {}), age_ms: Math.max(0, Math.round(now - k.at)) })),
        };
        if (!this.io.send(msg))
            return;
        for (const k of keys) {
            k.sent = now;
            k.went = true;
        }
        this.replayTo = keys[keys.length - 1].n;
    }
    /// After a replay was answered: what waited behind it goes, live (typed on this connection,
    /// with its screen in sight), or as another replay (more than one message's worth, from before).
    flush() {
        const now = this.io.now();
        const rest = this.queue.filter((k) => k.sent === null);
        if (!rest.length || !this.online)
            return;
        if (rest.some((k) => k.at < this.since)) {
            this.replay();
            return;
        }
        const msg = { t: "keys", session: this.session, stream: this.stream, keys: rest.map((k) => ({ n: k.n, event: k.event })) };
        if (this.io.send(msg))
            for (const k of rest) {
                k.sent = now;
                k.went = true;
            }
    }
    lose(keys, why, reason) {
        const events = keys.filter((k) => !positional(k.event)).map((k) => k.event);
        if (!events.length)
            return;
        const last = this.lost[this.lost.length - 1];
        // Typed after a loss, while still offline: the same loss.
        if (last && why === "after" && this.io.now() - last.at < 60_000)
            last.events.push(...events);
        else
            this.lost.push({ events, why, reason, at: this.io.now() });
        if (this.lost.length > 5)
            this.lost.splice(0, this.lost.length - 5);
    }
}
const NAMES = {
    enter: "⏎\n", tab: "⇥", back_tab: "⇤", backspace: "⌫", esc: "⎋", up: "↑", down: "↓", left: "←", right: "→",
    home: "⇱", end: "⇲", page_up: "⇞", page_down: "⇟", insert: "⎀", delete: "⌦", menu: "☰",
};
/// Input events as a person reads them: text as typed, ⏎ ⌫ ⎋ ← for keys, ^C and M-x with Ctrl
/// and Alt, a paste as its text.
export function describe(events) {
    let out = "";
    for (const e of events) {
        if (e?.t === "paste") {
            out += e.text ?? "";
            continue;
        }
        if (e?.t !== "key")
            continue;
        const { code, mods = 0 } = e.key ?? {};
        const name = code?.t === "char" ? code.c : code?.t === "f" ? `F${code.n}` : NAMES[code?.t] ?? "";
        out += (mods & 4 ? "^" : "") + (mods & 2 ? "M-" : "") + (mods & 4 && code?.t === "char" ? name.toUpperCase() : name);
    }
    return out;
}
/// What the page says about keys that weren't sent: "4 keys weren't sent: …".
export function lostTitle(l) {
    const n = l.events.length;
    const what = n === 1 ? (l.events[0]?.t === "paste" ? "A paste" : "A key") : `${n} ${l.events.every((e) => e?.t === "paste") ? "pastes" : "keys"}`;
    const verb = l.why === "silent" ? "may not have been sent" : n === 1 ? "wasn't sent" : "weren't sent";
    return `${what} ${verb}: ${whyText(l)}.`;
}
export function whyText(l) {
    switch (l.why) {
        case "changed": return "the session changed while you were away (it asked something new, or started or finished something)";
        case "stale": return "you were offline too long";
        case "unknown": return l.reason || "ptyro couldn't tell whether they were typed already";
        case "scope": return "this device may only watch";
        case "ended": return "the session isn't running";
        case "old": return "this ptyro is older and can't check keys typed while offline";
        case "after": return "they came after keys that weren't sent";
        case "silent": return "no word came back whether they arrived";
        default: return l.reason || "the daemon refused them";
    }
}
