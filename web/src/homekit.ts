// Home's logic (SPEC §26), apart from the page (home.ts) so it runs without one: the view's
// types, and the words `ptyro_core::home` says, said the same way. web/test/home.mjs keeps
// them equal to the Rust ones.

export type Section = "needs" | "hanging" | "working" | "quiet";
export interface Thing { next: string; text: string; at_ms: number; session?: string; session_name?: string; worktree?: string; worktree_name?: string; item?: string; left?: string; key: string; stamp: string }
export interface Row { key: string; project?: string; name: string; workspace?: string; machine?: string; section: Section; things: Thing[]; live: number; working: number; last_ms: number; week?: number[] }
export interface MeterPart { text: string; level: "quiet" | "unknown" | "old" | "warn" | "limited" }
export interface Meter { label: string; parts: MeterPart[]; level: string }
export interface Away { since_ms: number; back_ms: number; finished: number; failed: number; asked: number; commits: number }
export interface Entry { kind: string; title: string; detail?: string; at_ms: number; addr: string; session?: string }
export interface LeftOff { you?: string; reply?: string; agent?: string; at_ms: number; addr?: string; session?: string }
export interface Trail { key: string; left_off?: LeftOff; entries: Entry[]; hot: { path: string; count: number }[] }
export interface HomeView { at_ms: number; rows: Row[]; meters: Meter[]; away?: Away; memory: "on" | "off" | "withheld"; trail?: Trail; at_start?: boolean }

export const TITLES: Record<Section, string> = { needs: "Needs you", hanging: "Left hanging", working: "Working", quiet: "Quiet" };
export const SECTIONS: Section[] = ["needs", "hanging", "working", "quiet"];
/// The word a row leads with (`Next::verb`, word for word).
export const VERBS: Record<string, string> = {
  answer: "answer", failed: "failed", review: "review", look: "look", asked: "it asked", cut_off: "cut off", planned: "next",
  commit: "commit", push: "push", stale: "idle", working: "working", limit: "limit", open: "your turn", left_off: "left off", nothing: "",
};
export const HANGING = ["asked", "cut_off", "planned", "commit", "push", "stale"];
/// How many rows of a section show before "N more".
export const CAPS: Partial<Record<Section, number>> = { hanging: 8, quiet: 6 };

/// `2 need you · 1 left hanging · 3 working`, or `all quiet` (`HomeView::headline`).
export function headline(v: HomeView): string {
  let [needs, hanging, working] = [0, 0, 0];
  for (const r of v.rows) {
    working += r.working;
    for (const t of r.things) {
      if (["answer", "failed", "review", "look"].includes(t.next)) needs++;
      else if (HANGING.includes(t.next)) hanging++;
    }
  }
  const parts = [needs ? `${needs} need${needs === 1 ? "s" : ""} you` : "", hanging ? `${hanging} left hanging` : "", working ? `${working} working` : ""].filter(Boolean);
  return parts.length ? parts.join(" · ") : "all quiet";
}

/// `45m`, `2h10m`, `3d` (`home::span`).
export function span(ms: number): string {
  const m = Math.floor(Math.max(0, ms) / 60_000);
  if (m < 60) return `${m}m`;
  if (m < 1440) return m % 60 ? `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m` : `${m / 60}h`;
  return `${Math.floor(m / 1440)}d`;
}

/// `away 2h10m: 3 finished, 1 failed` (`Away::line`).
export function awayLine(a: Away): string {
  const say = (n: number, one: string, many: string): string => (n ? `${n} ${n === 1 ? one : many}` : "");
  const what = [say(a.finished, "finished", "finished"), say(a.failed, "failed", "failed"), say(a.asked, "asks you", "ask you"), say(a.commits, "commit", "commits")].filter(Boolean);
  return `away ${span(a.back_ms - a.since_ms)}: ${what.length ? what.join(", ") : "nothing happened"}`;
}

/// A week of prompts as marks, a day each (`home::spark`).
export function spark(week?: number[]): string {
  const max = Math.max(0, ...(week ?? []));
  if (!max) return "";
  return week!.map((n) => (n ? "▁▂▃▄▅▆▇█"[Math.min(8, Math.max(1, Math.ceil((n * 8) / max))) - 1] : "·")).join("");
}

