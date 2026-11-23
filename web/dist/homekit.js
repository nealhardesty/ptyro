// Home's logic (SPEC §26), apart from the page (home.ts) so it runs without one: the view's
// types, and the words `ptyro_core::home` says, said the same way. web/test/home.mjs keeps
// them equal to the Rust ones.
export const TITLES = { needs: "Needs you", hanging: "Left hanging", working: "Working", quiet: "Quiet" };
export const SECTIONS = ["needs", "hanging", "working", "quiet"];
/// The word a row leads with (`Next::verb`, word for word).
export const VERBS = {
    answer: "answer", failed: "failed", review: "review", look: "look", asked: "it asked", cut_off: "cut off", planned: "next",
    commit: "commit", push: "push", stale: "idle", working: "working", limit: "limit", open: "your turn", left_off: "left off", nothing: "",
};
export const HANGING = ["asked", "cut_off", "planned", "commit", "push", "stale"];
/// How many rows of a section show before "N more".
export const CAPS = { hanging: 8, quiet: 6 };
/// `2 need you · 1 left hanging · 3 working`, or `all quiet` (`HomeView::headline`).
export function headline(v) {
    let [needs, hanging, working] = [0, 0, 0];
    for (const r of v.rows) {
        working += r.working;
        for (const t of r.things) {
            if (["answer", "failed", "review", "look"].includes(t.next))
                needs++;
            else if (HANGING.includes(t.next))
                hanging++;
        }
    }
    const parts = [needs ? `${needs} need${needs === 1 ? "s" : ""} you` : "", hanging ? `${hanging} left hanging` : "", working ? `${working} working` : ""].filter(Boolean);
    return parts.length ? parts.join(" · ") : "all quiet";
}
/// `45m`, `2h10m`, `3d` (`home::span`).
export function span(ms) {
    const m = Math.floor(Math.max(0, ms) / 60_000);
    if (m < 60)
        return `${m}m`;
    if (m < 1440)
        return m % 60 ? `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m` : `${m / 60}h`;
    return `${Math.floor(m / 1440)}d`;
}
/// `away 2h10m: 3 finished, 1 failed` (`Away::line`).
export function awayLine(a) {
    const say = (n, one, many) => (n ? `${n} ${n === 1 ? one : many}` : "");
    const what = [say(a.finished, "finished", "finished"), say(a.failed, "failed", "failed"), say(a.asked, "asks you", "ask you"), say(a.commits, "commit", "commits")].filter(Boolean);
    return `away ${span(a.back_ms - a.since_ms)}: ${what.length ? what.join(", ") : "nothing happened"}`;
}
/// A week of prompts as marks, a day each (`home::spark`).
export function spark(week) {
    const max = Math.max(0, ...(week ?? []));
    if (!max)
        return "";
    return week.map((n) => (n ? "▁▂▃▄▅▆▇█"[Math.min(8, Math.max(1, Math.ceil((n * 8) / max))) - 1] : "·")).join("");
}
