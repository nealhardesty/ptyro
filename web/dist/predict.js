// Predicted echo (ROADMAP WEB2.4; SPEC §16.4 WEB1.8, "Predicted echo"), mosh's speculative local
// echo done carefully, with no DOM in it so web/test/predict.mjs can drive it. On a slow link
// each typed character waits a round trip before the session's echo shows it; this guesses the
// echo and draws it at once, as a layer over the confirmed screen, until the screen says yes or
// no. It never touches what is sent: the terminal sends every key first and only then tells the
// predictor, which has no way to send anything.
//
// The rules (each is tested in web/test/predict.mjs):
// - Only printable, narrow characters (Shift at most) and Backspace are predicted. Anything else
//   (Enter, Tab, arrows, Ctrl and Alt keys, pastes, clicks, focus, a wide or combining character,
//   the right margin) is a *barrier*: the cursor may jump, so a new epoch starts and nothing is
//   predicted until the screen has settled after it (its key typed, then `SETTLE_MS`).
// - A prediction is confirmed when a frame has its character in its cell with the cursor just
//   after it (later ones confirm the ones before, whose cells must match too). It is contradicted
//   when the cursor is anywhere but where the echo so far puts it (it moved by itself, or the
//   echo was different: upper case, `*`), and it expires when its echo hasn't come in time
//   (`ECHO_MS` and the round trip after the daemon said it typed the key; `MAX_MS` at most).
//   Either way every prediction goes at once, and the epoch is over.
// - Predictions are *shown* only when the echo is slow (a smoothed round trip of `ON_MS` or
//   more; off again below `OFF_MS`), and only once `PROOF` of them have been confirmed in this
//   epoch, by a cell that changed and a cursor that moved. A prompt that doesn't echo (a
//   password) never confirms one, so nothing typed there is ever drawn.
// - A character is shown only where it types into a blank cell (the end of the text), a
//   Backspace only when it erases what was typed in this epoch at the end of the text; others
//   are still checked (they prove or disprove), just not drawn.
/// The smoothed round trip at which predictions start to show…
export const ON_MS = 50;
/// …and below which they stop (no point on a LAN: the echo is as quick as a guess).
export const OFF_MS = 30;
/// Confirmed predictions an epoch needs before its predictions show.
export const PROOF = 2;
/// After the daemon says it typed a key, how long its echo may take (plus the round trip).
export const ECHO_MS = 200;
/// No prediction lives longer than this.
export const MAX_MS = 1500;
/// After a key that wasn't predicted is typed, how long until the screen's cursor is trusted.
export const SETTLE_MS = 80;
/// A character that takes exactly one cell in every terminal: ASCII, Latin, Greek, Cyrillic.
export function narrow(c) {
    const cps = Array.from(c);
    if (cps.length !== 1)
        return false;
    const u = cps[0].codePointAt(0);
    return (u >= 0x20 && u <= 0x7e) || (u >= 0xa1 && u <= 0x2ff && u !== 0xad) || (u >= 0x370 && u <= 0x482) || (u >= 0x48a && u <= 0x52f) || (u >= 0x1e00 && u <= 0x1eff);
}
/// What an input event would do to the screen, if it can be guessed.
export function predictable(event) {
    if (event?.t !== "key")
        return null;
    const code = event.key?.code, mods = event.key?.mods ?? 0;
    if (code?.t === "char" && (mods & ~1) === 0 && typeof code.c === "string" && narrow(code.c))
        return { t: "char", c: code.c };
    if (code?.t === "backspace" && mods === 0)
        return { t: "erase" };
    return null;
}
export class Predictor {
    io;
    mode = "auto";
    /// Show predictions whatever the round trip (tests, and someone checking it on a fast link:
    /// `ptyroTerm.predict.force = true` with `localStorage["ptyro.debug"] = "1"`).
    force = false;
    /// The smoothed time from a key to the frame that echoes it, in ms (null: not measured yet).
    srtt = null;
    /// The echo is slow enough to be worth predicting (with hysteresis).
    slow = false;
    epoch = 0;
    /// Confirmed predictions in this epoch.
    proof = 0;
    /// Outstanding, in the order their keys went.
    preds = [];
    stats = { predicted: 0, confirmed: 0, misses: 0, barriers: 0, epochs: 0 };
    /// Columns of `mineRow` typed and echoed in this epoch: what a Backspace may erase.
    mine = new Set();
    mineRow = -1;
    /// The screen's cursor is where keys land again from this time…
    trustAt = 0;
    /// …once this key, which wasn't predicted, has been typed (null: none waited for).
    waitN = null;
    waitAt = 0;
    /// The highest key the daemon said it typed.
    upto = 0;
    /// Letters typed while the screen hadn't caught up with a barrier: placed once it has, after
    /// the ones its frame shows echoed already (`anchor`).
    held = [];
    last = null;
    constructor(io) {
        this.io = io;
    }
    /// The predictions still tied to the cursor (this epoch's).
    chain() { return this.preds.filter((p) => !p.free); }
    /// An input event went to the session (`n`: its number in the outbox). `blocked`: the terminal
    /// can't show predictions now (scrolled back, selecting, dictation held, keys waiting).
    /// True when what is drawn may have changed.
    key(event, n, look, blocked) {
        const now = this.io.now();
        const kind = predictable(event);
        if (this.mode === "off" || blocked || !look || !look.cursor.visible || !kind)
            return this.barrier(n, now);
        let changed = false;
        if (!this.chain().length) {
            // Keys that weren't predicted move the cursor in ways only the screen knows: until it has
            // caught up, letters are held to be placed then, and anything else is another barrier.
            if (this.waitN !== null && now - this.waitAt > 2 * MAX_MS)
                this.waitN = null;
            if (!this.trusted(now)) {
                if (kind.t !== "char" || this.held.length >= 64)
                    return this.barrier(n, now);
                this.held.push({ n, c: kind.c, at: now, typed: null });
                return false;
            }
            changed = this.anchor(look, now);
        }
        return this.place(kind, n, now, null, look) || changed;
    }
    /// Predict `kind` after the chain (or at the cursor). False when it can't be (a barrier then).
    place(kind, n, at, typed, look) {
        const now = this.io.now();
        const chain = this.chain();
        let x, y, ok;
        const tail = chain[chain.length - 1];
        if (tail)
            ({ after: x, y, ok } = tail);
        else {
            ({ x, y } = look.cursor);
            ok = true;
            if (y !== this.mineRow) {
                this.mine.clear();
                this.mineRow = y;
            }
        }
        if (y < 0 || y >= look.rows || x < 0)
            return this.barrier(n, now);
        /// The cell as it will be once what is predicted so far is echoed.
        const ahead = (cx) => {
            for (let i = chain.length - 1; i >= 0; i--)
                if (chain[i].x === cx)
                    return { t: chain[i].ch, pred: chain[i] };
            return { t: look.cell(cx, y), pred: null };
        };
        let p;
        if (kind.t === "char") {
            // The last column: the terminal waits to wrap, and where the cursor goes is its business.
            if (x >= look.cols - 1)
                return this.barrier(n, now);
            // Drawn only where it types into a blank (as it will be: a Backspace predicted before it may
            // have blanked it), never over text that is there (in the middle of a line, the rest moves).
            p = { n, x, y, ch: kind.c, from: x, after: x + 1, was: look.cell(x, y), at, typed, ok: ok && ahead(x).t === " ", late: at !== now, free: null };
        }
        else {
            const tx = x - 1;
            if (tx < 0 || tx >= look.cols)
                return this.barrier(n, now);
            const target = ahead(tx);
            // Only what was typed in this epoch (a prompt isn't erased by Backspace), and only at the
            // end of the text (in the middle the rest of the line moves left).
            const ours = target.pred ? target.pred.ch !== " " : this.mineRow === y && this.mine.has(tx) && target.t !== " ";
            if (!ours || ahead(x).t !== " ")
                return this.barrier(n, now);
            p = { n, x: tx, y, ch: " ", from: x, after: tx, was: look.cell(tx, y), at, typed, ok, late: at !== now, free: null };
        }
        this.preds.push(p);
        this.stats.predicted++;
        this.arm(now);
        return true;
    }
    /// The screen has caught up with the last barrier: where the cursor is now is where keys land.
    trusted(now) { return this.waitN === null && now >= this.trustAt; }
    /// Place the held letters on a screen that has caught up with their barrier: the cells before
    /// its cursor show the first of them echoed already (only ones the daemon said it typed); the
    /// rest are predicted from the cursor. A wrong guess here is only a tentative prediction: it
    /// is checked like any other and never drawn before this epoch proves itself.
    anchor(look, now) {
        const held = this.held;
        if (!held.length)
            return false;
        this.held = [];
        // Too old to place: the screen has them by now, whatever it did with them.
        if (now - held[0].at > MAX_MS)
            return false;
        const { x, y } = look.cursor;
        let typed = 0;
        while (typed < held.length && (held[typed].n === null || held[typed].typed !== null))
            typed++;
        let j = Math.min(typed, x);
        for (; j > 0; j--)
            if (held.slice(0, j).every((h, i) => look.cell(x - j + i, y) === h.c))
                break;
        if (y !== this.mineRow) {
            this.mine.clear();
            this.mineRow = y;
        }
        for (let i = 0; i < j; i++)
            this.mine.add(x - j + i);
        let placed = false;
        for (const h of held.slice(j)) {
            if (!this.place({ t: "char", c: h.c }, h.n, h.at, h.typed, look))
                break;
            placed = true;
        }
        return placed;
    }
    /// The daemon typed keys up to `upto` (`ServerMsg::Typed`).
    acked(upto) {
        const now = this.io.now();
        this.upto = Math.max(this.upto, upto);
        for (const p of this.preds) {
            if (p.n !== null && p.n <= upto && p.typed === null)
                p.typed = now;
            if (p.free && p.free.n !== null && p.free.n <= upto && p.free.strictAt === null)
                p.free.strictAt = now + SETTLE_MS;
        }
        for (const h of this.held)
            if (h.n !== null && h.n <= upto && h.typed === null)
                h.typed = now;
        if (this.waitN !== null && upto >= this.waitN) {
            this.waitN = null;
            this.trustAt = Math.max(this.trustAt, now + SETTLE_MS);
        }
        this.arm(now);
        return false;
    }
    /// A frame was applied to the screen: confirm, contradict, or wait.
    frame(look) {
        const now = this.io.now();
        const c = look.cursor;
        const L = this.last;
        this.last = { x: c.x, y: c.y, visible: c.visible, cols: look.cols, rows: look.rows, alt: look.alt };
        if (!L)
            return false;
        if (!c.visible || !L.visible || L.cols !== look.cols || L.rows !== look.rows || L.alt !== look.alt) {
            // A hidden cursor, a new size, another screen: nothing predicted applies.
            const had = this.preds.length > 0;
            this.preds = [];
            this.held = [];
            this.untrust(now, null);
            this.newEpoch();
            return had;
        }
        let changed = this.judgeFree(look, now);
        // Letters held since a barrier, placed on the first frame after the screen caught up.
        const anchored = this.held.length > 0 && this.trusted(now);
        if (anchored)
            changed = this.anchor(look, now) || changed;
        const chain = this.chain();
        if (anchored) {
            // (Placed on this very frame: nothing to judge yet.)
        }
        else if (chain.length) {
            const y = chain[0].y;
            if (c.y !== y)
                return this.miss(now) || changed;
            let k = -1;
            for (let i = chain.length - 1; i >= 0; i--)
                if (c.x === chain[i].after && look.cell(chain[i].x, y) === chain[i].ch) {
                    k = i;
                    break;
                }
            if (k >= 0) {
                // The echo has come this far: every cell up to here must be as predicted.
                const want = new Map();
                for (let i = 0; i <= k; i++)
                    want.set(chain[i].x, chain[i].ch);
                for (const [x, ch] of want)
                    if (look.cell(x, y) !== ch)
                        return this.miss(now) || changed;
                // Evidence only where a cell now differs from what the screen showed before (typing the
                // letter already there proves nothing, and neither do letters and the Backspaces that
                // took them back: a prompt that doesn't echo shows exactly that).
                const before = new Map();
                for (let i = 0; i <= k; i++)
                    if (!before.has(chain[i].x))
                        before.set(chain[i].x, chain[i].was);
                for (const [x, ch] of want) {
                    if (before.get(x) !== ch)
                        this.proof++;
                    if (ch === " ")
                        this.mine.delete(x);
                    else
                        this.mine.add(x);
                }
                this.mineRow = y;
                if (!chain[k].late)
                    this.sample(now - chain[k].at);
                this.stats.confirmed += k + 1;
                const done = new Set(chain.slice(0, k + 1));
                this.preds = this.preds.filter((p) => !done.has(p));
                changed = true;
            }
            else if (c.x !== chain[0].from)
                return this.miss(now) || changed;
            // (Otherwise the cursor is where it was: the echo hasn't come yet.)
        }
        else if (c.x !== L.x || c.y !== L.y) {
            // The cursor moved by itself (output, a redraw, a new prompt): the proof was about where it was.
            this.newEpoch();
        }
        return this.expire(now) || changed;
    }
    /// Time passed: predictions whose echo is overdue go.
    tick(look) {
        const now = this.io.now();
        const changed = look ? this.judgeFree(look, now) : false;
        return this.expire(now) || changed;
    }
    /// Something the predictions can't survive (keys waiting or refused, scrolled back, dictation,
    /// the option turned off): they all go, and the cursor is learnt again from the screen.
    block() {
        const now = this.io.now();
        const had = this.preds.length > 0;
        this.preds = [];
        this.held = [];
        this.waitN = null;
        this.trustAt = Math.max(this.trustAt, now + SETTLE_MS);
        this.newEpoch();
        return had;
    }
    /// What to draw over the confirmed screen now.
    overlay(blocked = false) {
        const out = { cells: [], cursor: null };
        if (this.mode === "off" || blocked || !(this.slow || this.force))
            return out;
        const cells = new Map();
        for (const p of this.preds)
            if (p.free?.vis)
                cells.set(`${p.x},${p.y}`, { x: p.x, y: p.y, t: p.ch });
        if (this.proof >= PROOF) {
            const chain = this.chain();
            for (const p of chain) {
                if (!p.ok)
                    break;
                cells.set(`${p.x},${p.y}`, { x: p.x, y: p.y, t: p.ch });
            }
            const tail = chain[chain.length - 1];
            if (tail?.ok)
                out.cursor = { x: tail.after, y: tail.y };
        }
        out.cells = [...cells.values()];
        return out;
    }
    /// When a prediction must have been echoed by.
    deadline(p) {
        const rtt = this.srtt ?? 200;
        if (p.typed !== null)
            return Math.min(p.at + MAX_MS, p.typed + ECHO_MS + Math.min(rtt, 1000));
        return p.at + Math.min(MAX_MS, Math.max(600, 2 * rtt + 400));
    }
    // ── inside ──────────────────────────────────────────────────────────────────────────────
    /// A key that wasn't predicted: what is outstanding is judged by its cells alone from now on,
    /// and a new epoch starts once the screen has caught up with this key.
    barrier(n, now) {
        this.stats.barriers++;
        this.held = [];
        const shown = this.proof >= PROOF;
        for (const p of this.preds)
            if (!p.free)
                p.free = { n, strictAt: n === null ? now + SETTLE_MS : null, vis: shown && p.ok };
        this.untrust(now, n);
        this.newEpoch();
        this.arm(now);
        return false;
    }
    /// The screen's cursor isn't where keys land until key `n` is typed (or a while, without numbers).
    untrust(now, n) {
        if (n !== null && n <= this.upto)
            this.trustAt = Math.max(this.trustAt, now + SETTLE_MS);
        else if (n !== null) {
            if (this.waitN === null || n > this.waitN) {
                this.waitN = n;
                this.waitAt = now;
            }
        }
        else
            this.trustAt = Math.max(this.trustAt, now + 1.5 * (this.srtt ?? 150) + SETTLE_MS);
    }
    /// Predictions a barrier freed: confirmed by their cell, gone at once when any is contradicted,
    /// and once their barrier was typed (and settled), confirmed now or gone.
    judgeFree(look, now) {
        const free = this.preds.filter((p) => p.free);
        if (!free.length)
            return false;
        const latest = new Map();
        for (const p of free)
            latest.set(`${p.x},${p.y}`, p);
        const drop = new Set(free.filter((p) => latest.get(`${p.x},${p.y}`) !== p));
        let wrong = false;
        for (const p of latest.values()) {
            const v = look.cell(p.x, p.y);
            if (v === p.ch)
                drop.add(p);
            else if (v !== p.was)
                wrong = true;
            else if (p.free.strictAt !== null && now >= p.free.strictAt)
                drop.add(p);
        }
        if (wrong)
            for (const p of free)
                drop.add(p);
        if (!drop.size)
            return false;
        this.preds = this.preds.filter((p) => !drop.has(p));
        return true;
    }
    /// A prediction was wrong: every one goes, the epoch is over, and keys still on their way may
    /// move the cursor, so it is learnt again from the screen once they are typed.
    miss(now) {
        this.stats.misses++;
        const ns = this.chain().map((p) => p.n).filter((n) => n !== null);
        const had = this.preds.length > 0;
        this.preds = [];
        this.held = [];
        this.untrust(now, ns.length ? Math.max(...ns) : null);
        this.newEpoch();
        return had;
    }
    expire(now) {
        const chain = this.chain();
        if (chain.some((p) => now >= this.deadline(p)))
            return this.miss(now);
        const old = this.preds.filter((p) => p.free && now >= this.deadline(p));
        if (!old.length) {
            this.arm(now);
            return false;
        }
        this.preds = this.preds.filter((p) => !old.includes(p));
        this.arm(now);
        return true;
    }
    newEpoch() {
        this.epoch++;
        this.stats.epochs++;
        this.proof = 0;
        this.mine.clear();
    }
    sample(ms) {
        const m = Math.max(0, ms);
        this.srtt = this.srtt === null ? m : this.srtt + (m - this.srtt) / 4;
        if (this.srtt >= ON_MS)
            this.slow = true;
        else if (this.srtt < OFF_MS)
            this.slow = false;
    }
    /// Ask to be woken for the next deadline (or a freed prediction's strict judgement).
    arm(now) {
        let next = Infinity;
        for (const p of this.preds) {
            next = Math.min(next, this.deadline(p));
            if (p.free?.strictAt != null)
                next = Math.min(next, p.free.strictAt);
        }
        if (next < Infinity)
            this.io.later(Math.max(1, Math.ceil(next - now)));
    }
}
