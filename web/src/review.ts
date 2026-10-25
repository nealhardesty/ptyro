// Review and ship (ROADMAP WEB1.4): a worktree's diff with line comments for its agent, its
// commits, committing and integrating, its pull request and checks, and the queue of worktrees
// waiting for you.

import { conn } from "./conn.js";
import { data, liveWorktrees, wtTabs } from "./data.js";
import type { Worktree } from "./data.js";
import { createWorktree } from "./flow.js";
import { actions, ago, ask, badge, bar, Cache, confirm, enc, errText, form, go, h, menu, pending, row, section, sheet, ui, views, webHref } from "./kit.js";
import { allSessions } from "./sessions.js";
import { prBadge } from "./projects.js";

const ACTIONS = {
  "review.diff": "The worktree's changes; tap a line to comment on it for the agent",
  "review.against": "What to diff against: the main branch, what's unpushed, the last turn, or uncommitted work",
  "review.add": "Add a comment on this line, for the agent",
  "review.list": "The comments waiting to be sent, shown on their lines",
  "review.rm": "Remove this comment, or all of them",
  "review.send": "Send the comments to the worktree's agent as one prompt",
  "review.queue": "Worktrees ready for review, best first",
  "review.done": "Mark the worktree reviewed",
  "worktree.log": "Commits since the base",
  "worktree.commit": "Commit the changes: choose the files and write the message",
  "worktree.ask_commit": "Ask the agent to commit its work (once its turn is over)",
  "worktree.integrate": "Rebase onto the main branch and fast-forward it, or cherry-pick the commits there",
  "pr.create": "Push the branch and open a pull request",
  "pr.checks": "The pull request and its checks",
  "pr.fix": "Send the failing checks' logs to the agent",
  "pr.automerge": "Merge by itself once the checks pass",
  "pr.watch": "Feed the PR's new failures and review comments to the agent",
  "pr.reply": "Reply on a review thread, or the PR, as \"via ptyro\"",
  "pr.open": "Open a pull request's worktree and session, or make a worktree for it",
} as const;
const tip = actions(ACTIONS);

type Against = "base" | "upstream" | "last_turn" | "head";
const AGAINST: [Against, string][] = [["base", "Base"], ["upstream", "Unpushed"], ["last_turn", "Last turn"], ["head", "Uncommitted"]];
const against = new Map<string, Against>();

// The diff is cached by worktree and comparison; a newer git status refreshes it in place (the
// diff on screen stays until the new one arrives), instead of being a new key that shows "Reading…".
const key = (w: Worktree, extra = ""): string => `${w.id}|0|${extra}`;
const stamped = new Map<string, number>();
const diffs = new Cache<any>((k) => { const [id, , a] = k.split("|"); return conn.request({ t: "worktree_diff", addr: id, against: a }).then((r) => r.doc); });
const comments = new Cache<any[]>((id) => conn.request({ t: "review_list", addr: id }).then((r) => r.comments));
const commits = new Cache<any[]>((k) => conn.request({ t: "worktree_log", addr: k.split("|")[0] }).then((r) => r.commits));
const prs = new Cache<any>((id) => conn.request({ t: "pr_get", addr: id, refresh: true }).then((r) => r.info ?? null));

/// The agent working in a worktree (the newest live one).
const agentIn = (w: Worktree) => [...allSessions().values()].filter((s) => s.worktree === w.id && s.agent && s.state.t === "running").sort((a, b) => b.created_ms - a.created_ms)[0];

// ── the diff ────────────────────────────────────────────────────────────────────────────────

function commentOn(w: Worktree, path: string, line: number, code: string): void {
  form(`${path.split("/").pop()}:${line}`, [{ key: "text", label: "Comment", type: "textarea", required: true, rows: 3, placeholder: "What should change here?" }], "Add", async (v) => {
    await conn.request({ t: "review_add", addr: w.id, path, line, code, text: v.text });
    comments.reload(w.id);
  }, { intro: h("code", {}, code.trim().slice(0, 120)) });
}

function sendComments(w: Worktree, list: any[]): void {
  const agent = agentIn(w);
  form(`Send ${list.length} comment${list.length === 1 ? "" : "s"} to ${agent?.name ?? "the agent"}`, [
    { key: "note", label: "A word to go with them", type: "textarea", rows: 2, placeholder: "optional" },
  ], "Send", async (v) => {
    await conn.request({ t: "review_send", addr: w.id, session: null, note: (v.note as string) || null });
    ui.toast("Sent");
    comments.reload(w.id);
  }, { intro: agent ? undefined : "No agent is running in this worktree: they go to one that is, or fail." });
}

function commentsBar(w: Worktree): HTMLElement | null {
  const list = comments.get(w.id) ?? [];
  if (!list.length) return null;
  return h("div", { class: "comments-bar", title: tip("review.list") },
    h("b", {}, `${list.length} comment${list.length === 1 ? "" : "s"} waiting`),
    h("button", { class: "small primary", title: tip("review.send"), onclick: () => sendComments(w, list) }, "Send to agent"),
    h("button", { class: "small", title: tip("review.rm"), onclick: () => confirm("Remove all comments?", "They haven't been sent.", "Remove", async () => { await ask({ t: "review_remove", addr: w.id, id: null }); comments.reload(w.id); }) }, "Clear"));
}

function diffFile(w: Worktree, f: any, byLine: Map<string, any[]>, open: boolean): HTMLElement {
  const det = h("details", { class: "dfile", open }, h("summary", {},
    h("span", { class: "dstat" }, h("b", { class: "add" }, `+${f.added}`), " ", h("b", { class: "del" }, `−${f.removed}`)), " ",
    h("span", { class: "dname" }, f.path), " ", badge(f.status)));
  if (f.binary) { det.append(h("p", { class: "sub" }, "Binary file")); return det; }
  for (const hk of f.hunks as any[]) {
    det.append(h("div", { class: "hunk-head" }, hk.header));
    const pre = h("div", { class: "dlines" });
    for (const l of hk.lines as any[]) {
      const n = l.new ?? l.old;
      const cls = l.kind === "added" ? "add" : l.kind === "removed" ? "del" : "";
      const line = h("div", { class: `dl ${cls}`, title: tip("review.add"), onclick: () => n != null && commentOn(w, f.path, n, l.text) },
        h("span", { class: "ln" }, String(l.old ?? "")), h("span", { class: "ln" }, String(l.new ?? "")),
        h("span", { class: "sign" }, l.kind === "added" ? "+" : l.kind === "removed" ? "−" : " "), h("span", { class: "tx" }, l.text || " "));
      pre.append(line);
      // A comment is on a line number and the line's text: a removed line 10 and an added line 10
      // are different lines.
      for (const c of (byLine.get(`${f.path}:${n}`) ?? []).filter((c) => c.code == null || c.code === l.text)) {
        pre.append(h("div", { class: "dcomment" }, h("span", {}, c.text),
          h("button", { class: "icon", "aria-label": "Remove the comment", title: tip("review.rm"), onclick: async () => { await ask({ t: "review_remove", addr: w.id, id: c.id }); comments.reload(w.id); } }, "✕")));
      }
    }
    det.append(pre);
  }
  return det;
}

function commitSheet(w: Worktree, doc: any): void {
  const files = (doc?.files ?? []) as any[];
  const picks = new Map<string, HTMLInputElement>();
  const msg = h("textarea", { rows: 3, placeholder: "Commit message", autocapitalize: "sentences" });
  const err = h("p", { class: "form-err" });
  const s = sheet("Commit", [
    h("label", { class: "field" }, h("span", {}, "Message"), msg),
    h("p", { class: "intro" }, files.length ? "Files (untick to leave out):" : "No changes to commit."),
    ...files.map((f) => { const c = h("input", { type: "checkbox" }); c.checked = true; picks.set(f.path, c); return h("label", { class: "field check" }, c, h("span", {}, f.path)); }),
    err,
  ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: async () => {
    if (!msg.value.trim()) { err.textContent = "Write a message."; return; }
    const chosen = [...picks].filter(([, c]) => c.checked).map(([p]) => p);
    try {
      const r = await conn.request({ t: "worktree_commit", addr: w.id, message: msg.value.trim(), paths: chosen.length === picks.size ? [] : chosen, all: chosen.length === picks.size });
      s.close(); ui.toast(`Committed ${String(r.sha).slice(0, 8)}`); diffs.reload(); commits.reload();
    } catch (e) { err.textContent = errText(e); }
  } }, "Commit")]);
}

async function askAgentToCommit(w: Worktree): Promise<void> {
  const a = agentIn(w);
  if (!a) { ui.toast("No agent runs in this worktree"); return; }
  await ask({ t: "send_text", addr: a.id, text: "[via ptyro] Please commit your work now: stage what belongs together, and write a clear commit message (subject line under 72 characters, then why).", enter: true, wait_ready_ms: 300000 }, `Asked ${a.name} to commit (once its turn is over)`);
}

function changesTab(w: Worktree): HTMLElement {
  const a = against.get(w.id) ?? "base";
  const k = key(w, a);
  const doc = diffs.get(k);
  const at = w.status?.at_ms ?? 0;
  if (doc !== undefined && stamped.get(k) !== at) { stamped.set(k, at); diffs.refresh(k); } else if (!stamped.has(k)) stamped.set(k, at);
  const list = comments.get(w.id) ?? [];
  const byLine = new Map<string, any[]>();
  for (const c of list) byLine.set(`${c.path}:${c.line}`, [...(byLine.get(`${c.path}:${c.line}`) ?? []), c]);
  const body = pending(diffs, k, "Reading the diff…") ?? (doc.files.length ? h("div", { class: "diff" },
    h("div", { class: "sub" }, `${doc.files.length} file${doc.files.length === 1 ? "" : "s"}, +${doc.files.reduce((n: number, f: any) => n + f.added, 0)} −${doc.files.reduce((n: number, f: any) => n + f.removed, 0)}${doc.truncated ? " (cut short: too big)" : ""}`),
    ...(doc.files as any[]).map((f, i) => diffFile(w, f, byLine, doc.files.length <= 3 || i === 0)))
    : h("p", { class: "empty" }, a === "base" ? "No changes yet." : "Nothing here: try another comparison."));
  return h("div", { class: "stack-in", title: tip("review.diff") },
    h("div", { class: "chips", title: tip("review.against") }, ...AGAINST.map(([v, l]) => h("button", { class: a === v ? "on" : "", onclick: () => { against.set(w.id, v); ui.render(); } }, l))),
    commentsBar(w),
    bar(
      h("button", { class: "primary", title: tip("worktree.commit"), onclick: () => { const d = diffs.get(key(w, "head")); if (d) commitSheet(w, d); else void conn.request({ t: "worktree_diff", addr: w.id, against: "head" }).then((r) => commitSheet(w, r.doc)); } }, "Commit…"),
      h("button", { title: tip("worktree.ask_commit"), onclick: () => void askAgentToCommit(w) }, "Ask the agent to commit"),
      h("button", { class: "icon", "aria-label": "Refresh", onclick: () => { diffs.reload(); comments.reload(w.id); } }, "↻")),
    body);
}

// ── commits, integrate ──────────────────────────────────────────────────────────────────────

function integrate(w: Worktree): void {
  const base = data.projects.get(w.project)?.default_branch ?? "main";
  form("Merge into the main branch", [
    { key: "into", label: "Into", value: base },
    { key: "mode", label: "How", type: "select", options: [["rebase", "Rebase onto it, then fast-forward"], ["pick", "Cherry-pick the commits there"]] },
    { key: "push", label: "Push it afterwards", type: "check" },
  ], "Merge", async (v) => {
    const r = await conn.request({ t: "worktree_integrate", addr: w.id, into: (v.into as string) || null, cherry_pick: v.mode === "pick", push: !!v.push });
    ui.toast((r.notes ?? ["Merged"])[0]);
    commits.reload();
  });
}

function commitsTab(w: Worktree): HTMLElement {
  const k = key(w);
  const list = commits.get(k);
  return h("div", { class: "stack-in", title: tip("worktree.log") },
    bar(h("button", { class: "primary", title: tip("worktree.integrate"), onclick: () => integrate(w) }, "Merge into main…"), h("button", { class: "icon", "aria-label": "Refresh", onclick: () => commits.reload() }, "↻")),
    pending(commits, k) ?? (list!.length ? h("div", { class: "rows" }, ...list!.map((c) => row(c.subject, `${c.sha.slice(0, 8)} · ${c.author} · ${c.files} file${c.files === 1 ? "" : "s"}`, h("span", {}, h("b", { class: "add" }, `+${c.added}`), " ", h("b", { class: "del" }, `−${c.removed}`), " ", ago(c.at_ms)))))
      : h("p", { class: "empty" }, "No commits on this branch yet.")));
}

// ── the pull request ────────────────────────────────────────────────────────────────────────

function createPr(w: Worktree): void {
  form("Open a pull request", [
    { key: "title", label: "Title", value: w.title ?? "", placeholder: "from the commits" },
    { key: "body", label: "Description", type: "textarea", rows: 4, placeholder: "optional" },
    { key: "base", label: "Into", placeholder: w.base ?? "the main branch" },
    { key: "draft", label: "As a draft", type: "check" },
  ], "Open it", async (v) => {
    const r = await conn.request({ t: "pr_create", addr: w.id, draft: !!v.draft, title: (v.title as string) || null, body: (v.body as string) || null, base: (v.base as string) || null });
    ui.toast(r.info ? `Opened #${r.info.number}` : "Opened");
    prs.reload(w.id);
  });
}

function replyPr(w: Worktree): void {
  form("Reply on the PR", [
    { key: "text", label: "Reply", type: "textarea", required: true, hint: "It's signed “via ptyro”" },
    { key: "comment", label: "Review comment id", type: "number", hint: "Empty replies on the PR itself" },
  ], "Reply", async (v) => { await conn.request({ t: "pr_reply", addr: w.id, comment: v.comment ? Number(v.comment) : null, text: v.text }); ui.toast("Replied"); });
}

const BUCKET: Record<string, string> = { pass: "✓", fail: "✗", cancel: "✗", pending: "◌", skipping: "–" };

function checksTab(w: Worktree): HTMLElement {
  const pr = prs.get(w.id);
  const loading = pending(prs, w.id, "Asking GitHub…");
  if (loading) return h("div", { class: "stack-in" }, loading);
  if (!pr) return h("div", { class: "stack-in", title: tip("pr.checks") },
    h("p", { class: "empty" }, "No pull request yet."),
    bar(h("button", { class: "primary", title: tip("pr.create"), onclick: () => createPr(w) }, "Open a pull request…"), h("button", { class: "icon", "aria-label": "Refresh", onclick: () => prs.reload(w.id) }, "↻")));
  const checks = pr.checks as any[];
  const failing = checks.filter((c) => ["fail", "cancel"].includes(c.bucket));
  return h("div", { class: "stack-in", title: tip("pr.checks") },
    h("div", { class: "pr-head" }, (webHref(pr.url) ? h("a", { href: webHref(pr.url)!, target: "_blank", rel: "noreferrer" }, `#${pr.number} ${pr.title}`) : h("span", {}, `#${pr.number} ${pr.title}`)), " ", prBadge(w) ?? badge(pr.state), pr.review ? badge(pr.review.replace(/_/g, " ").toLowerCase()) : null, pr.automerge ? badge("auto-merge", "info") : null),
    bar(
      failing.length ? h("button", { class: "primary", title: tip("pr.fix"), onclick: async () => { await ask({ t: "pr_fix", addr: w.id }, "Sent the failing checks to the agent"); } }, `Fix ${failing.length} failing`) : null,
      h("button", { title: tip("pr.automerge"), onclick: () => menu("Auto-merge", [
        ...["squash", "merge", "rebase"].map((m) => ({ label: `When the checks pass: ${m}`, run: async () => { await ask({ t: "pr_automerge", addr: w.id, method: m, off: false }, "Will merge when the checks pass"); prs.reload(w.id); } })),
        { label: "Turn off", run: async () => { await ask({ t: "pr_automerge", addr: w.id, method: null, off: true }, "Auto-merge off"); prs.reload(w.id); } },
      ]) }, pr.automerge ? "Auto-merge: on" : "Auto-merge…"),
      h("button", { title: tip("pr.watch"), onclick: () => menu("Watch the PR", [
        { label: "Feed new failures and comments to the agent", run: async () => { await ask({ t: "pr_watch", addr: w.id, off: false }, "Watching"); } },
        { label: "Stop watching", run: async () => { await ask({ t: "pr_watch", addr: w.id, off: true }, "Stopped watching"); } },
      ]) }, "Watch"),
      h("button", { title: tip("pr.reply"), onclick: () => replyPr(w) }, "Reply…"),
      h("button", { class: "icon", "aria-label": "Refresh", onclick: () => prs.reload(w.id) }, "↻")),
    section("Checks", checks.length ? h("div", { class: "rows" }, ...checks.map((c) => row(
      h("span", {}, h("span", { class: `bucket ${c.bucket}` }, BUCKET[c.bucket] ?? "·"), " ", c.name), c.workflow ?? "", webHref(c.link) ? h("a", { href: webHref(c.link)!, target: "_blank", rel: "noreferrer" }, "log") : null)))
      : h("p", { class: "sub" }, "No checks reported.")));
}

wtTabs.push({ id: "changes", label: "Changes", render: changesTab }, { id: "commits", label: "Commits", render: commitsTab }, { id: "checks", label: "Checks", render: checksTab });
// The worktree's page opens on Overview; these sit before Checkpoints.
wtTabs.sort((a, b) => ["changes", "commits", "checks", "checkpoints"].indexOf(a.id) - ["changes", "commits", "checks", "checkpoints"].indexOf(b.id));

// ── the review queue ────────────────────────────────────────────────────────────────────────

export const queue = new Cache<any[]>(() => conn.request({ t: "review_queue" }).then((r) => r.items));

function openPr(): void {
  const projects = [...data.projects.values()];
  form("Open a pull request", [
    { key: "pr", label: "Pull request", required: true, placeholder: "number or URL" },
    { key: "project", label: "In project", type: "select", options: projects.map((p): [string, string] => [p.id, p.slug]), hint: "If it has no worktree yet, one is made on it" },
  ], "Open", async (v) => {
    const n = Number(String(v.pr).match(/(\d+)\s*$/)?.[1]);
    if (!n) throw new Error("Give the pull request's number or its URL.");
    const w = liveWorktrees().find((x) => x.pr?.number === n);
    if (w) {
      const a = agentIn(w);
      go(a ? `#/s/${enc(a.id)}` : `#/wt/${enc(w.id)}/checks`);
      return;
    }
    const prj = data.projects.get(v.project as string);
    if (!prj) throw new Error("Pick a project.");
    void createWorktree({ project: prj.id, pr: n, name: `pr-${n}` });
  });
}

function queueView(): HTMLElement {
  const items = queue.get("");
  const body = pending(queue, "", "Ranking what's waiting…") ?? (items!.length
    ? h("div", { class: "rows", title: tip("review.queue") }, ...items!.map((it) => row(
      h("span", {}, it.worktree.title || it.worktree.name, " ", prBadge(it.worktree)),
      [it.reasons.join(", "), it.last_message].filter(Boolean).join(" — "), ago(Date.now() - it.waiting_ms),
      { onclick: () => menu(it.worktree.title || it.worktree.name, [
        { label: "Review its changes", run: () => go(`#/wt/${enc(it.worktree.id)}/changes`) },
        it.session && { label: "Open its session", run: () => go(`#/s/${enc(it.session)}`) },
        { title: tip("review.done"), label: "Mark reviewed", run: async () => { await ask({ t: "review_done", addr: it.worktree.id }, "Reviewed"); queue.reload(""); } },
      ]) })))
    : h("p", { class: "empty" }, "Nothing is waiting for review."));
  return h("div", {}, ui.header("Review queue", "#/more", h("button", { class: "icon", title: tip("pr.open"), onclick: openPr, "aria-label": "Open a pull request" }, "⎘"), h("button", { class: "icon", onclick: () => queue.reload(""), "aria-label": "Refresh" }, "↻")),
    h("main", { class: "stack" }, h("p", { class: "intro" }, "Worktrees whose agents have finished, the likeliest to be quick and safe first."), body));
}

views.set("review", () => queueView());
