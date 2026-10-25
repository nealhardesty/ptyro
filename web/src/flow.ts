// Starting work: a session, or a worktree with an agent in it. The daemon asks questions on the
// way (are the project's scripts trusted? may it run under this profile? are too many worktrees
// waiting for review?); these are the same answers the TUI asks for, as sheets.

import { conn } from "./conn.js";
import { confirm, enc, errText, go, h, sheet, ui } from "./kit.js";

/// Ask for `req`, and when the daemon wants the user to confirm (a profile guard) show its
/// question and send it again with `confirmed`.
export async function withConfirm(req: any, key: string): Promise<any | undefined> {
  const send = (confirmed: boolean) => conn.request({ ...req, [key]: { ...req[key], confirmed } });
  try {
    return await send(false);
  } catch (e) {
    if ((e as { code?: string }).code !== "needs_confirmation") { ui.toast(errText(e)); return undefined; }
    return new Promise((ok) => {
      let said = false;
      const s = confirm("Are you sure?", (e as Error).message, "Go ahead", async () => {
        said = true;
        try { ok(await send(true)); } catch (e2) { ui.toast(errText(e2)); ok(undefined); }
      }, false);
      // Cancel, Esc, ✕ and a tap outside all answer "no" (after the button's own handler had its turn).
      s.onClose(() => setTimeout(() => { if (!said) ok(undefined); }, 0));
    });
  }
}

/// Start a session (`Spawn`); returns it, or `undefined` when it didn't start (a toast says why).
export async function startSession(spec: any, then: "open" | "stay" = "open"): Promise<any | undefined> {
  const full = { cwd: "", argv: [], ...spec };
  let r = await withConfirm({ t: "spawn", spec: full }, "spec");
  if (r?.t === "queued") {
    const again = await new Promise<boolean>((ok) => {
      let said = false;
      const s = confirm("Waiting for review", `${r.unreviewed} worktrees are waiting for you to review them. Start this one anyway?`, "Start anyway", () => { said = true; ok(true); }, false);
      s.onClose(() => setTimeout(() => { if (!said) ok(false); }, 0));
    });
    if (!again) return undefined;
    r = await withConfirm({ t: "spawn", spec: { ...full, unqueued: true } }, "spec");
  }
  const info = r?.info;
  if (info && then === "open") go(`#/s/${enc(info.id)}`);
  return info;
}

/// The project's lifecycle scripts, as they'd run in a new worktree.
async function scriptsFor(project: string): Promise<any | undefined> {
  try { return (await conn.request({ t: "project_scripts", addr: project, worktree: null })).scripts; } catch { return undefined; }
}

/// Create a worktree (and start its agent), asking about the project's scripts first when they
/// aren't trusted yet. Opens its agent's session, or the worktree. Returns the reply.
export async function createWorktree(spec: any): Promise<any | undefined> {
  const scripts = spec.setup === false ? undefined : await scriptsFor(spec.project);
  let trust: string | undefined;
  if (scripts && scripts.text.trim() && !scripts.trusted) {
    const yes = await new Promise<boolean | null>((ok) => {
      const s = sheet("Run this project's scripts?", [
        h("p", { class: "intro" }, `${scripts.project} has scripts that run when a worktree is made (${scripts.names.join(", ")}). They run on your computer, as you. Read them first.`),
        h("pre", { class: "block" }, scripts.text),
      ], [
        h("button", { onclick: () => { ok(false); s.close(); } }, "Not now"),
        h("button", { class: "primary", onclick: () => { ok(true); s.close(); } }, "Trust and run"),
      ]);
      // Esc, ✕ or a tap outside: do nothing at all (an answer already given wins).
      s.onClose(() => ok(null));
    });
    if (yes === null) return undefined;
    if (yes) trust = scripts.fingerprint; else spec = { ...spec, setup: false };
  }
  const r = await withConfirm({ t: "worktree_new", spec: { setup: true, ...spec, trust } }, "spec");
  if (!r) return undefined;
  for (const n of r.notes ?? []) ui.toast(n);
  // Its agent's session; while setup still runs, the worktree's page, which lists what's going on.
  go(r.agent ? `#/s/${enc(r.agent.id)}` : `#/wt/${enc(r.info.id)}`);
  return r;
}
