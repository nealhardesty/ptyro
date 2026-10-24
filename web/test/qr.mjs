// The web app's QR encoder (web/dist/qr.js), checked for what a scanner relies on: the symbol's
// size for the text's length, the three finder patterns, the timing lines, the format
// information (twice, each a valid BCH codeword of a mask) and the version information from
// version 7. (A real decoder agreed on versions 1–10 when this was written; none runs in CI.)
//   node web/test/qr.mjs
import { qrMatrix } from "../dist/qr.js";
import assert from "node:assert/strict";

const bits = (m, cells) => cells.reduce((n, [x, y], i) => n | ((m[y][x] ? 1 : 0) << i), 0);
const bch = (data, poly, deg) => { let r = data; for (let i = 0; i < deg; i++) r = (r << 1) ^ ((r >>> (deg - 1)) * poly); return r; };

// Capacity: 14 bytes fit version 1, 15 need version 2, 213 is the most, 214 returns null.
const sizeOf = (n) => qrMatrix("a".repeat(n))?.length;
assert.equal(sizeOf(14), 21);
assert.equal(sizeOf(15), 25);
assert.equal(sizeOf(213), 57);
assert.equal(sizeOf(214), undefined);
assert.ok(qrMatrix("é".repeat(107)) === null && qrMatrix("é".repeat(100)) !== null, "UTF-8 bytes count, not characters");

for (const text of ["hi", "https://box.tail1234.ts.net/#pair=ptp_" + "x".repeat(43), "y".repeat(150), "z".repeat(213)]) {
  const m = qrMatrix(text);
  const n = m.length;
  const v = (n - 17) / 4;
  assert.ok(Number.isInteger(v) && v >= 1 && v <= 10);
  // Finder patterns: 7x7, dark ring, light ring, 3x3 dark centre.
  for (const [ox, oy] of [[0, 0], [n - 7, 0], [0, n - 7]]) {
    for (let y = 0; y < 7; y++) for (let x = 0; x < 7; x++) {
      const d = Math.max(Math.abs(x - 3), Math.abs(y - 3));
      assert.equal(m[oy + y][ox + x], d !== 2, `finder at ${ox},${oy} (${x},${y})`);
    }
  }
  // Timing lines between the finders, and the always-dark module.
  for (let i = 8; i < n - 8; i++) { assert.equal(m[6][i], i % 2 === 0); assert.equal(m[i][6], i % 2 === 0); }
  assert.equal(m[n - 8][8], true);
  // Format information, both copies: a BCH(15,5) codeword for level M and some mask.
  const first = [...Array(6).keys()].map((i) => [8, i]).concat([[8, 7], [8, 8], [7, 8]], [5, 4, 3, 2, 1, 0].map((x) => [x, 8]));
  const second = [...Array(8).keys()].map((i) => [n - 1 - i, 8]).concat([...Array(7).keys()].map((i) => [8, n - 7 + i]));
  const f1 = bits(m, first), f2 = bits(m, second);
  assert.equal(f1, f2, "the two format copies agree");
  const word = f1 ^ 0x5412;
  const mask = (word >>> 10) & 7;
  assert.equal(word >>> 13, 0, "error correction level M");
  assert.equal(word, (((0 << 3) | mask) << 10) | (bch((0 << 3) | mask, 0x537, 10) & 0x3ff));
  // Version information (versions 7 and up), both copies.
  if (v >= 7) {
    const a = [], b = [];
    for (let i = 0; i < 18; i++) { a.push([n - 11 + (i % 3), Math.floor(i / 3)]); b.push([Math.floor(i / 3), n - 11 + (i % 3)]); }
    const word = bits(m, a);
    assert.equal(word, bits(m, b));
    assert.equal(word, (v << 12) | (bch(v, 0x1f25, 12) & 0xfff));
  }
}
console.log("qr ok");
