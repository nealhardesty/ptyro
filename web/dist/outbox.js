// Every key exactly once (ROADMAP WEB2.3; SPEC §16.4 WEB1.8, "Every key exactly once"), with no
// DOM in it so web/test/outbox.mjs can drive it. A session's keys, pastes and clicks are numbered
// in a stream of this page's own (`ClientMsg::Keys`) and kept until the daemon says they are
// settled (`ServerMsg::Typed`). Each carries what its page last drew when it was typed: that
// frame's state token, and when it was typed on the daemon's clock (the frame's `at_ms` plus
// the time since it came), so the daemon can tell a key that arrives late (a dying connection
// held it) and judge it. A key goes live only behind keys that went ahead of it on the same
// connection: never past one that waits (offline, too much for one message, behind a replay).
// Keys typed while the connection is down, or unanswered for a while, wait here and are shown as
// waiting; on reconnect (or after an unanswered wait) they go again, numbered as before, so a
// copy the daemon already has is dropped. The daemon types them only if the first was typed at
// most `REPLAY_WINDOW_MS` ago and the session still waits for what it did; otherwise none of
// them is typed, and nothing typed after them goes either (half a command is worse than none):
// they are kept as `lost`, to be said, shown and typed again on purpose. A daemon that can't
// number input (older than 1.52) gets plain `Input`, as before, and nothing waits.
/// `ptyro_protocol::REPLAY_WINDOW_MS`: keys older than this when sent again are refused.
export const REPLAY_WINDOW_MS = 5000;
/// `ptyro_protocol::LATE_LIMIT_MS`: keys arriving this long after they were typed are refused.
export const LATE_LIMIT_MS = 10_000;
/// `ptyro_protocol::MAX_SEQ_KEYS`: the most events one message carries.
export const MAX_SEQ_KEYS = 4096;
/// The most one message carries, in bytes (under the server's 4.25 MiB, conn.ts `MAX_SEND`).
export const MAX_BYTES = 4 * 1024 * 1024;
/// What a message takes besides its keys, and each key besides its event (at most).
const ENVELOPE = 512;
const PER_KEY = 128;
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
/// An event's size in a message, in bytes (its JSON in UTF-8).
const sizeOf = (event) => {
    const s = JSON.stringify(event) ?? "";
    let b = PER_KEY;
    for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i);
        if (c < 0x80)
            b += 1;
        else if (c < 0x800)
            b += 2;
        else if (c >= 0xd800 && c < 0xdc00) {
            b += 4;
            i++;
        }
        else
            b += 3;
    }
    return b;
};
/// Clicks, drags, the wheel and focus are aimed at a screen as it was: never kept to send later.
const positional = (event) => event?.t === "mouse" || event?.t === "focus";
export class Outbox {
    session;
    io;
    maxBytes;
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
    /// When this connection's welcome came (a wait for an answer counts from it at the earliest).
    since = 0;
    /// Connections so far: keys typed on an earlier one go only in a replay.
    epoch = 0;
    /// The highest refused number heard of (`Pending.heard`).
    heard = 0;
    /// Losses so far, and who waits for the queue to empty (`settled`).
    losses = 0;
    waiters = [];
    constructor(session, io, stream = randomId(), maxBytes = MAX_BYTES) {
        this.session = session;
        this.io = io;
        this.maxBytes = maxBytes;
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
    /// Once every key typed so far is settled: true if none of them was lost meanwhile (✎ compose
    /// sends behind them, and not at all after keys that weren't sent).
    settled() {
        if (!this.queue.length)
            return Promise.resolve(true);
        return new Promise((done) => this.waiters.push({ losses: this.losses, done }));
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
        const seen = this.io.seen();
        const k = { n: this.next++, event, token: seen.token, clock: seen.clock, heard: this.heard, at: now, epoch: this.epoch, sent: null, went: false, bytes: sizeOf(event) };
        if (k.bytes + ENVELOPE > this.maxBytes) {
            this.lose([k], "other", `it is too large to send from here (over ${Math.floor(this.maxBytes / 1048576)} MiB)`);
            this.changed();
            return null;
        }
        if (this.closed) {
            this.lose([k], "after", "keys before them weren't sent");
            this.changed();
            return null;
        }
        this.queue.push(k);
        this.pump();
        if (!this.online)
            this.io.check();
        this.io.later(this.online ? WAIT_MS : 250);
        this.changed();
        return k.n;
    }
    /// A welcome: `numbered` when it lists the feature. What waits goes again now.
    connected(numbered) {
        this.online = true;
        this.closed = false;
        this.replayTo = 0;
        this.since = this.io.now();
        this.epoch++;
        for (const k of this.queue)
            k.sent = null;
        this.mode = numbered ? "numbered" : "plain";
        if (this.queue.length) {
            if (numbered)
                this.pump();
            else
                this.lose(this.queue.splice(0), "old", "this ptyro can't check keys typed while offline");
        }
        this.io.later(WAIT_MS);
        this.changed();
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
        this.changed();
    }
    /// `ServerMsg::Typed` for this session (another stream's is ignored).
    typed(msg) {
        if (msg.stream !== this.stream)
            return;
        const rs = (Array.isArray(msg.refused) ? msg.refused : msg.refused ? [msg.refused] : []).slice().sort((a, b) => a.from - b.from);
        if (rs.length) {
            for (const r of rs)
                this.heard = Math.max(this.heard, r.to);
            // One notice for them all, named by the first refusal (the rest follow from it).
            const inside = (k) => rs.some((r) => k.n >= r.from && k.n <= r.to);
            const gone = this.queue.filter(inside);
            this.queue = this.queue.filter((k) => !inside(k));
            const why = rs.find((r) => gone.some((k) => k.n >= r.from && k.n <= r.to)) ?? rs[0];
            if (gone.length)
                this.lose(gone, why.why, why.reason);
            // And everything after them that waits (half a command is worse than none): typed before
            // this page heard of the refusal, the daemon refuses them too, if they reach it.
            const after = this.queue.filter((k) => k.n > rs[0].from);
            this.queue = this.queue.filter((k) => k.n < rs[0].from);
            if (after.length)
                this.lose(after, "after", "keys before them weren't sent");
        }
        this.queue = this.queue.filter((k) => k.n > msg.upto);
        if (this.replayTo && msg.upto >= this.replayTo)
            this.replayTo = 0;
        this.pump();
        this.changed();
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
                this.changed();
                return;
            }
            this.io.check();
            this.io.later(500);
            this.changed();
            return;
        }
        if (this.mode !== "numbered")
            return;
        // The oldest send still unanswered (none: nothing could be sent yet).
        const out = this.queue.reduce((m, k) => (k.sent === null ? m : Math.min(m, k.sent)), Infinity);
        // (Counted from this connection's start for keys older than it.)
        if (now - Math.max(head.at, this.since) >= GIVE_UP_MS && (out === Infinity || now - out >= RESEND_MS)) {
            this.lose(this.queue.splice(0), "silent", "no word whether they arrived");
            this.replayTo = 0;
            this.changed();
            return;
        }
        if (now - out >= RESEND_MS)
            this.replay();
        else
            this.pump();
        if (now - head.at >= WAIT_MS)
            this.io.check();
        this.io.later(out === Infinity ? 250 : Math.max(200, RESEND_MS - (now - out)));
        this.changed();
    }
    /// Forget the lost keys (the person dismissed them, or typed them again).
    dismiss() {
        this.lost = [];
        this.changed();
    }
    /// Send what hasn't gone on this connection, if it may go now: typed before the connection, a
    /// replay; typed on it, live, behind keys that went ahead of it on it. Nothing while a replay
    /// waits for its answer (if it is refused, what comes after isn't typed either).
    pump() {
        if (!this.online || this.mode !== "numbered" || this.replayTo)
            return;
        let i = this.queue.findIndex((k) => k.sent === null);
        if (i < 0)
            return;
        if (this.queue[i].epoch < this.epoch) {
            this.replay(i);
            return;
        }
        const now = this.io.now();
        while (i < this.queue.length) {
            const keys = this.batch(i);
            const msg = { t: "keys", session: this.session, stream: this.stream, keys: keys.map((k) => this.wire(k, null)) };
            if (!this.io.send(msg))
                return;
            for (const k of keys) {
                k.sent = now;
                k.went = true;
            }
            i += keys.length;
        }
    }
    /// Keys from `from` (the head: everything unanswered), again, as a replay with what each was
    /// typed against: one message's worth; the rest after its answer.
    replay(from = 0) {
        const now = this.io.now();
        const keys = this.batch(from);
        if (!keys.length)
            return;
        const msg = { t: "keys", session: this.session, stream: this.stream, replay: true, keys: keys.map((k) => this.wire(k, now)) };
        if (!this.io.send(msg))
            return;
        for (const k of keys) {
            k.sent = now;
            k.went = true;
        }
        this.replayTo = keys[keys.length - 1].n;
    }
    /// As many keys from `from` as one message carries (by count and by size; one at least).
    batch(from) {
        const out = [];
        let bytes = ENVELOPE;
        for (let i = from; i < this.queue.length && out.length < MAX_SEQ_KEYS; i++) {
            const k = this.queue[i];
            if (out.length && bytes + k.bytes > this.maxBytes)
                break;
            out.push(k);
            bytes += k.bytes;
        }
        return out;
    }
    /// A key as a message carries it (`SeqKey`); `now` for a replay (its age).
    wire(k, now) {
        return {
            n: k.n, event: k.event,
            ...(k.token ? { token: k.token } : {}),
            ...(now !== null ? { age_ms: Math.max(0, Math.round(now - k.at)) } : {}),
            ...(k.clock !== null ? { typed_ms: Math.max(0, Math.round(k.clock)) } : {}),
            ...(k.heard ? { heard: k.heard } : {}),
        };
    }
    changed() {
        if (!this.queue.length && this.waiters.length) {
            for (const w of this.waiters.splice(0))
                w.done(w.losses === this.losses);
        }
        this.io.changed();
    }
    lose(keys, why, reason) {
        if (!keys.length)
            return;
        this.losses++;
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
        case "overflow": return "the session isn't reading what is typed (it is stopped or stuck)";
        default: return l.reason || "the daemon refused them";
    }
}
/// Keys not sent as the page shows them before Reveal: each character a dot (they may be a
/// password typed at a prompt that doesn't echo), new lines kept.
export function mask(text) {
    return text.replace(/[^\n]/gu, "•");
}
