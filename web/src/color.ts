// Terminal colours and attribute flags as the page draws them (the canvas, the scrollback, the
// diff).

import type { Color } from "./screen.js";

const BASE16 = [
  "#1b1f24", "#ff7b72", "#3fb950", "#d29922", "#58a6ff", "#bc8cff", "#39c5cf", "#c9d1d9",
  "#6e7681", "#ffa198", "#56d364", "#e3b341", "#79c0ff", "#d2a8ff", "#56d4dd", "#f0f6fc",
];
export function indexed(i: number): string {
  if (i < 16) return BASE16[i];
  if (i < 232) {
    const n = i - 16, l = [0, 95, 135, 175, 215, 255];
    return `rgb(${l[Math.floor(n / 36)]},${l[Math.floor(n / 6) % 6]},${l[n % 6]})`;
  }
  const g = 8 + (i - 232) * 10;
  return `rgb(${g},${g},${g})`;
}
export function css(c: Color | undefined, fallback: string): string {
  if (!c || c.t === "default") return fallback;
  if (c.t === "indexed") return indexed(c.i);
  return `rgb(${c.r},${c.g},${c.b})`;
}
export const F = { BOLD: 1, DIM: 2, ITALIC: 4, UNDERLINE: 8, INVERSE: 256, HIDDEN: 512, STRIKE: 1024 };
