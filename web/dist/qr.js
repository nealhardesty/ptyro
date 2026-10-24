// A QR code encoder, small and dependency-free: byte mode, error correction level M, versions
// 1–10 (up to 213 bytes: a pairing link is about 90). `qrMatrix` returns the modules, or null
// when the text doesn't fit.
/// Per version (1–10), level M: [error-correction codewords per block, [blocks, data codewords
/// each][]]. Total codewords are 26, 44, 70, 100, 134, 172, 196, 242, 292, 346.
const BLOCKS = [
    [10, [[1, 16]]], [16, [[1, 28]]], [26, [[1, 44]]], [18, [[2, 32]]], [24, [[2, 43]]],
    [16, [[4, 27]]], [18, [[4, 31]]], [22, [[2, 38], [2, 39]]], [22, [[3, 36], [2, 37]]], [26, [[4, 43], [1, 44]]],
];
const ALIGN = [[], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50]];
const dataWords = (v) => BLOCKS[v - 1][1].reduce((n, [b, d]) => n + b * d, 0);
// ── Reed–Solomon over GF(256), polynomial 0x11D ─────────────────────────────────────────────
const EXP = new Uint8Array(512), LOG = new Uint8Array(256);
for (let i = 0, x = 1; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100)
        x ^= 0x11d;
}
for (let i = 255; i < 512; i++)
    EXP[i] = EXP[i - 255];
const mul = (a, b) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);
function ecc(data, n) {
    // The generator polynomial (x - a^0)(x - a^1)…(x - a^(n-1)), highest term first, without its 1.
    let gen = [1];
    for (let i = 0; i < n; i++) {
        const next = new Array(gen.length + 1).fill(0);
        for (let j = 0; j < gen.length; j++) {
            next[j] ^= gen[j];
            next[j + 1] ^= mul(gen[j], EXP[i]);
        }
        gen = next;
    }
    const rem = new Array(n).fill(0);
    for (const d of data) {
        const f = d ^ rem.shift();
        rem.push(0);
        for (let i = 0; i < n; i++)
            rem[i] ^= mul(gen[i + 1], f);
    }
    return rem;
}
// ── the symbol ──────────────────────────────────────────────────────────────────────────────
function codewords(bytes, v) {
    const bits = [];
    const put = (val, len) => { for (let i = len - 1; i >= 0; i--)
        bits.push((val >>> i) & 1); };
    put(0b0100, 4);
    put(bytes.length, v < 10 ? 8 : 16);
    for (const b of bytes)
        put(b, 8);
    const cap = dataWords(v) * 8;
    put(0, Math.min(4, cap - bits.length));
    while (bits.length % 8)
        bits.push(0);
    const data = [];
    for (let i = 0; i < bits.length; i += 8)
        data.push(parseInt(bits.slice(i, i + 8).join(""), 2));
    for (let pad = 0xec; data.length < dataWords(v); pad ^= 0xec ^ 0x11)
        data.push(pad);
    // Split into blocks, add each one's error correction, interleave.
    const [eccLen, groups] = BLOCKS[v - 1];
    const blocks = [];
    let at = 0;
    for (const [count, size] of groups)
        for (let i = 0; i < count; i++) {
            blocks.push(data.slice(at, at + size));
            at += size;
        }
    const eccs = blocks.map((b) => ecc(b, eccLen));
    const out = [];
    const longest = Math.max(...blocks.map((b) => b.length));
    for (let i = 0; i < longest; i++)
        for (const b of blocks)
            if (i < b.length)
                out.push(b[i]);
    for (let i = 0; i < eccLen; i++)
        for (const e of eccs)
            out.push(e[i]);
    return out;
}
class Grid {
    v;
    n;
    m;
    fn;
    constructor(v) {
        this.v = v;
        this.n = 17 + 4 * v;
        this.m = Array.from({ length: this.n }, () => new Array(this.n).fill(false));
        this.fn = Array.from({ length: this.n }, () => new Array(this.n).fill(false));
    }
    set(x, y, dark) { this.m[y][x] = dark; this.fn[y][x] = true; }
    finder(cx, cy) {
        for (let dy = -4; dy <= 4; dy++)
            for (let dx = -4; dx <= 4; dx++) {
                const x = cx + dx, y = cy + dy;
                if (x < 0 || y < 0 || x >= this.n || y >= this.n)
                    continue;
                const d = Math.max(Math.abs(dx), Math.abs(dy));
                this.set(x, y, d !== 2 && d !== 4);
            }
    }
    align(cx, cy) {
        for (let dy = -2; dy <= 2; dy++)
            for (let dx = -2; dx <= 2; dx++)
                this.set(cx + dx, cy + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    }
    functionPatterns() {
        for (let i = 0; i < this.n; i++) {
            this.set(6, i, i % 2 === 0);
            this.set(i, 6, i % 2 === 0);
        }
        this.finder(3, 3);
        this.finder(this.n - 4, 3);
        this.finder(3, this.n - 4);
        const a = ALIGN[this.v - 1];
        for (const y of a)
            for (const x of a) {
                const corner = (x === 6 && y === 6) || (x === 6 && y === a[a.length - 1]) || (x === a[a.length - 1] && y === 6);
                if (!corner)
                    this.align(x, y);
            }
        this.format(0); // reserve
        if (this.v >= 7) {
            let rem = this.v;
            for (let i = 0; i < 12; i++)
                rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
            const bits = (this.v << 12) | rem;
            for (let i = 0; i < 18; i++) {
                const bit = ((bits >>> i) & 1) === 1;
                const x = this.n - 11 + (i % 3), y = Math.floor(i / 3);
                this.set(x, y, bit);
                this.set(y, x, bit);
            }
        }
    }
    /// Format bits: error correction level M (00) and the mask.
    format(mask) {
        const data = (0b00 << 3) | mask;
        let rem = data;
        for (let i = 0; i < 10; i++)
            rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
        const bits = ((data << 10) | rem) ^ 0x5412;
        const bit = (i) => ((bits >>> i) & 1) === 1;
        for (let i = 0; i <= 5; i++)
            this.set(8, i, bit(i));
        this.set(8, 7, bit(6));
        this.set(8, 8, bit(7));
        this.set(7, 8, bit(8));
        for (let i = 9; i < 15; i++)
            this.set(14 - i, 8, bit(i));
        for (let i = 0; i < 8; i++)
            this.set(this.n - 1 - i, 8, bit(i));
        for (let i = 8; i < 15; i++)
            this.set(8, this.n - 15 + i, bit(i));
        this.set(8, this.n - 8, true); // the always-dark module
    }
    place(words) {
        let i = 0;
        for (let right = this.n - 1; right >= 1; right -= 2) {
            if (right === 6)
                right = 5;
            for (let vert = 0; vert < this.n; vert++)
                for (let j = 0; j < 2; j++) {
                    const x = right - j;
                    const up = ((right + 1) & 2) === 0;
                    const y = up ? this.n - 1 - vert : vert;
                    if (!this.fn[y][x] && i < words.length * 8) {
                        this.m[y][x] = ((words[i >>> 3] >>> (7 - (i & 7))) & 1) === 1;
                        i++;
                    }
                }
        }
    }
    mask(k) {
        const f = [
            (x, y) => (x + y) % 2 === 0, (_x, y) => y % 2 === 0, (x) => x % 3 === 0, (x, y) => (x + y) % 3 === 0,
            (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0, (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
            (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0, (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
        ][k];
        for (let y = 0; y < this.n; y++)
            for (let x = 0; x < this.n; x++)
                if (!this.fn[y][x] && f(x, y))
                    this.m[y][x] = !this.m[y][x];
    }
    penalty() {
        const { n, m } = this;
        let p = 0;
        const runs = (line) => {
            let s = 0, run = 1;
            for (let i = 1; i <= line.length; i++) {
                if (i < line.length && line[i] === line[i - 1])
                    run++;
                else {
                    if (run >= 5)
                        s += run - 2;
                    run = 1;
                }
            }
            // finder-like 1:1:3:1:1 patterns with a light run of four on a side
            const str = line.map((b) => (b ? "1" : "0")).join("");
            for (const pat of ["10111010000", "00001011101"])
                for (let i = str.indexOf(pat); i >= 0; i = str.indexOf(pat, i + 1))
                    s += 40;
            return s;
        };
        for (let y = 0; y < n; y++)
            p += runs(m[y]);
        for (let x = 0; x < n; x++)
            p += runs(m.map((r) => r[x]));
        for (let y = 0; y < n - 1; y++)
            for (let x = 0; x < n - 1; x++)
                if (m[y][x] === m[y][x + 1] && m[y][x] === m[y + 1][x] && m[y][x] === m[y + 1][x + 1])
                    p += 3;
        const dark = m.flat().filter(Boolean).length;
        p += Math.floor(Math.abs((dark * 20) / (n * n) - 10)) * 10; // each 5% away from half
        return p;
    }
}
export function qrMatrix(text) {
    const bytes = new TextEncoder().encode(text);
    let v = 1;
    const room = (ver) => dataWords(ver) - (ver < 10 ? 2 : 3);
    while (v <= 10 && bytes.length > room(v))
        v++;
    if (v > 10)
        return null;
    const words = codewords(bytes, v);
    let best = null, bestScore = Infinity;
    for (let k = 0; k < 8; k++) {
        const g = new Grid(v);
        g.functionPatterns();
        g.place(words);
        g.mask(k);
        g.format(k);
        const s = g.penalty();
        if (s < bestScore) {
            best = g;
            bestScore = s;
        }
    }
    return best.m;
}
