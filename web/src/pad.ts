// The scratchpad's pure parts (ROADMAP NOTE1.5): a small Markdown reader that produces a tree (never
// HTML), the offline queue that holds quick-adds and edits until the socket is back, and the
// share-target text. No DOM in here, so web/test runs it under Node.

// ── Markdown ───────────────────────────────────────────────────────────────────────────────

export type Inline =
  | { t: "text"; text: string }
  | { t: "b" | "i" | "code"; text: string }
  | { t: "a"; text: string; href: string };

export type Block =
  | { k: "h"; level: number; inl: Inline[] }
  | { k: "li"; line: number; check: boolean | null; indent: number; inl: Inline[] }
  | { k: "p"; inl: Inline[] }
  | { k: "code"; text: string }
  | { k: "quote"; inl: Inline[] }
  | { k: "hr" };

/// Only these links become links; anything else (`javascript:`, `data:`…) stays text.
export function safeHref(href: string): string | null {
  const h = href.trim();
  return /^(https?:\/\/|mailto:|ptyro:\/\/)[^\s<>"']*$/i.test(h) ? h : null;
}

/// `**bold**`, `*italic*` / `_italic_`, `` `code` `` and `[text](url)` / bare URLs.
export function inline(s: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const flush = () => { if (text) { out.push({ t: "text", text }); text = ""; } };
  let i = 0;
  while (i < s.length) {
    const rest = s.slice(i);
    let m: RegExpMatchArray | null;
    if ((m = rest.match(/^`([^`]+)`/))) { flush(); out.push({ t: "code", text: m[1] }); i += m[0].length; continue; }
    if ((m = rest.match(/^\*\*([^*]+)\*\*/))) { flush(); out.push({ t: "b", text: m[1] }); i += m[0].length; continue; }
    if ((m = rest.match(/^(?:\*([^*\s][^*]*)\*|_([^_\s][^_]*)_)/)) && (i === 0 || /\W/.test(s[i - 1]))) {
      flush(); out.push({ t: "i", text: m[1] ?? m[2] }); i += m[0].length; continue;
    }
    if ((m = rest.match(/^\[([^\]]+)\]\(([^)\s]+)\)/))) {
      const href = safeHref(m[2]);
      flush();
      if (href) out.push({ t: "a", text: m[1], href }); else out.push({ t: "text", text: m[0] });
      i += m[0].length;
      continue;
    }
    if ((m = rest.match(/^(?:https?:\/\/|ptyro:\/\/)[^\s<>"')\]]+/i)) && (i === 0 || /\W/.test(s[i - 1]))) {
      const href = safeHref(m[0]);
      flush();
      out.push(href ? { t: "a", text: m[0], href } : { t: "text", text: m[0] });
      i += m[0].length;
      continue;
    }
    text += s[i];
    i++;
  }
  flush();
  return out;
}

export function parseMarkdown(src: string): Block[] {
  const blocks: Block[] = [];
  const lines = src.split("\n");
  let para: string[] = [];
  const endPara = () => { if (para.length) { blocks.push({ k: "p", inl: inline(para.join(" ")) }); para = []; } };
  for (let n = 0; n < lines.length; n++) {
    const l = lines[n];
    const line = n + 1;
    if (l.trimStart().startsWith("```")) {
      endPara();
      const code: string[] = [];
      n++;
      while (n < lines.length && !lines[n].trimStart().startsWith("```")) code.push(lines[n++]);
      blocks.push({ k: "code", text: code.join("\n") });
      continue;
    }
    let m: RegExpMatchArray | null;
    if ((m = l.match(/^(#{1,6}) +(.*)$/))) { endPara(); blocks.push({ k: "h", level: m[1].length, inl: inline(m[2].trim()) }); continue; }
    if ((m = l.match(/^(\s*)[-*] \[([ xX])\] ?(.*)$/))) {
      endPara();
      blocks.push({ k: "li", line, check: m[2] !== " ", indent: Math.floor(m[1].length / 2), inl: inline(m[3]) });
      continue;
    }
    if ((m = l.match(/^(\s*)[-*] +(.*)$/))) { endPara(); blocks.push({ k: "li", line, check: null, indent: Math.floor(m[1].length / 2), inl: inline(m[2]) }); continue; }
    if ((m = l.match(/^> ?(.*)$/))) { endPara(); blocks.push({ k: "quote", inl: inline(m[1]) }); continue; }
    if (/^(-{3,}|\*{3,})\s*$/.test(l)) { endPara(); blocks.push({ k: "hr" }); continue; }
    if (!l.trim()) { endPara(); continue; }
    // A line indented under an item continues it.
    const last = blocks[blocks.length - 1];
    if (/^\s{2,}\S/.test(l) && last?.k === "li" && !para.length) { last.inl.push({ t: "text", text: " " }, ...inline(l.trim())); continue; }
    para.push(l.trim());
  }
  endPara();
  return blocks;
}

// ── the queue ──────────────────────────────────────────────────────────────────────────────

export type Op =
  | { k: "add"; text: string; note?: boolean }
  /// An edit: `text` was typed from the revision `base`.
  | { k: "put"; base: string; text: string };

export interface Store { load(): Promise<string | null>; save(v: string | null): Promise<void> }

/// What waits for a connection, in order. Edits coalesce: the oldest base (the daemon merges
/// from it) with the newest text. Storage failing never breaks the page: it only forgets.
export class Queue {
  ops: Op[] = [];
  constructor(private store: Store) {}

  async load(): Promise<void> {
    try {
      const raw = await this.store.load();
      const v = raw ? JSON.parse(raw) : [];
      if (Array.isArray(v)) this.ops = v.filter((o) => o && (o.k === "add" || o.k === "put"));
    } catch { /* no storage */ }
  }

  private async persist(): Promise<void> {
    try { await this.store.save(this.ops.length ? JSON.stringify(this.ops) : null); } catch { /* no storage */ }
  }

  async push(op: Op): Promise<void> {
    if (op.k === "put") {
      const i = this.ops.findIndex((o) => o.k === "put");
      if (i >= 0) {
        const old = this.ops[i] as Extract<Op, { k: "put" }>;
        this.ops[i] = { k: "put", base: old.base, text: op.text };
        await this.persist();
        return;
      }
    }
    this.ops.push(op);
    await this.persist();
  }

  get length(): number { return this.ops.length; }

  /// Send each in order; the first failure stops it and keeps that one and the rest.
  async flush(send: (op: Op) => Promise<void>): Promise<number> {
    let sent = 0;
    while (this.ops.length) {
      try { await send(this.ops[0]); } catch { break; }
      this.ops.shift();
      sent++;
      await this.persist();
    }
    return sent;
  }
}

// ── the share target ───────────────────────────────────────────────────────────────────────

/// What the phone's share sheet sent (`?title=&text=&url=`), as a line for the pad; null when
/// there is nothing in it.
export function shareLine(q: URLSearchParams): string | null {
  const title = (q.get("title") ?? "").trim();
  const text = (q.get("text") ?? "").trim();
  const url = (q.get("url") ?? "").trim();
  const parts: string[] = [];
  for (const p of [title, text]) if (p && !parts.includes(p)) parts.push(p);
  // Android often puts the link in `text`.
  if (url && !parts.some((p) => p.includes(url))) parts.push(url);
  return parts.length ? parts.join(" — ") : null;
}

/// A line of text for the pad about a session: its task or last words, and where to find it.
export function noteFor(s: { name: string; handle: string; title?: string | null; task?: string | null; last_reply?: string | null }): string {
  const what = (s.task || s.title || s.last_reply || s.name).split("\n")[0].trim();
  return `${what.length > 160 ? what.slice(0, 157) + "…" : what} (ptyro://session/${s.handle})`;
}
