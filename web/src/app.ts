// ptyro web app (SPEC §16.4, M9.3): sessions, a live terminal view drawn from the
// daemon's screen frames, quick replies, and answers to permission prompts. Talks the ptyro
// protocol (JSON encoding) over `ptyro serve`'s WebSocket, signed in with a device token from
// `ptyro pair`. No dependencies, no third-party requests.

import { Screen, modelLabel } from "./screen.js";
import type { Frame, Session } from "./screen.js";
import { conn, store, token } from "./conn.js";
import { Term } from "./term.js";
import { FEATURE, GIVE_UP_MS, Outbox, RESEND_MS, lostTitle } from "./outbox.js";
import { autoFocus, hardKey, isMac, ownKeyboard, shellDigit, strayKey } from "./keys.js";
import type { Focus } from "./keys.js";
import { PressGate, actions, ago, h, patch, sheet, statusWord, tipOf, ui, views } from "./kit.js";
import type { Child } from "./kit.js";
import { can, data, loadAll, machineOf, onEvent, splitMachine } from "./data.js";
import { staleMaybeChanged } from "./projects.js";
import "./settings.js";
import "./panel.js";
import "./library.js";
import { chatsAside, chatsShown, findAllChats, newChat } from "./chats.js";
import { machineChip } from "./machines.js";
import { broadcast, marked, newTask, sessionMenu, useSessions } from "./sessions.js";
import { offerSetup, setupBanner, setupConnected, setupEvent } from "./settings.js";
import { altDigitsOn, goShell, listWorkspace, readAltDigits, shellStrip, shellsGroup, shellsWorkspace } from "./shells.js";
import { apply as showMachine, offlineText } from "./machine.js";

// What this app does, by name: the Web column of docs/parity.md names these, and `make parity`
// checks every name it uses is here (SPEC §1.3, ROADMAP WEB1). Each is its control's tooltip.
const ACTIONS = {
  "sessions": "Every session: what it's doing, and who needs you first",
  "session.open": "A session's live screen",
  "session.compose": "✎ Compose: write a long prompt, or dictate it (the keyboard's mic, macOS dictation, Superwhisper), read it over, and send it as one; Enter sends, Shift+Enter starts a new line, Esc closes and keeps the draft",
  "answer": "Answer what it asks: allow once, always, or deny",
  "host.load": "The load average of the computer ptyro runs on, over 1, 5 and 15 minutes: amber once every CPU is busy, red at twice that ([ui] load)",
  "notifications": "Notifications on this device (a test is sent)",
  "pair": "Pair this device with the link or QR code from `ptyro pair`",
  "unpair": "Forget this device's pairing",
} as const;
const tip = actions(ACTIONS);

// ── storage: the token lives in localStorage (page) and IndexedDB (the service worker) ────────

const db = (): Promise<IDBDatabase> =>
  new Promise((ok, err) => {
    const r = indexedDB.open("ptyro", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => ok(r.result);
    r.onerror = () => err(r.error);
  });
async function idbSet(key: string, value: string | null): Promise<void> {
  const d = await db();
  await new Promise<void>((ok, err) => {
    const tx = d.transaction("kv", "readwrite");
    if (value === null) tx.objectStore("kv").delete(key);
    else tx.objectStore("kv").put(value, key);
    tx.oncomplete = () => ok();
    tx.onerror = () => err(tx.error);
  });
}

function takePairCode(): void {
  const m = location.hash.match(/^#pair=([A-Za-z0-9_\-]+)$/);
  if (m) {
    history.replaceState(null, "", location.pathname + "#/");
    void claim(m[1]);
  }
}
/// Why the last pairing attempt failed (shown on the pairing screen).
let pairNote = "";
/// Trade the one-time code from `ptyro pair` for this device's token (`POST /api/pair`).
async function claim(code: string): Promise<void> {
  pairNote = "Pairing…";
  render();
  try {
    const r = await fetch("/api/pair", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code }),
    });
    if (!r.ok) throw new Error(await r.text());
    const { token } = await r.json();
    pairNote = "";
    useToken(token);
  } catch (e) {
    pairNote = `Pairing failed: ${(e as Error).message || e}`;
    // (With a pairing already in place the pairing screen isn't showing: say it here.)
    if (store.get("ptyro.token")) toast(pairNote);
  }
  render();
}
/// Store a device token and sign in with it (also from a tab that was open, unpaired).
function useToken(t: string): void {
  store.set("ptyro.token", t);
  idbSet("token", t).catch(() => {});
  // A new token may mean new rights (a pairing again for a wider scope): sign in afresh.
  conn.reset();
  conn.connect();
}

const GLYPH: Record<string, string> = { working: "●", blocked: "⚠", idle: "○", done: "✓", errored: "✗", starting: "◌", unknown: "·" };

function statusOf(s: Session): string {
  if (s.state.t === "exited" || s.state.t === "failed") return "exited";
  if (s.state.t === "dormant") return "dormant";
  return s.status?.status ?? (s.kind === "agent" ? "unknown" : "shell");
}

/// What the session list shows (`sidebar.filter`; the same three as the terminal's sidebar):
/// everything, what is running (a live process), or what is active now (an agent working, or
/// waiting for you, a shell with a command running). Quick terminals aren't sessions of their own.
type Show = "all" | "running" | "active";
const SHOWS: Show[] = ["all", "running", "active"];
const isRunning = (s: Session): boolean => !s.quick && (s.state.t === "running" || s.state.t === "starting");
const isActive = (s: Session): boolean => isRunning(s) && (s.status ? ["working", "blocked", "starting"].includes(s.status.status) : !!s.command && s.command.ended_ms == null);
const KEEPS: Record<Show, (s: Session) => boolean> = { all: (s) => !s.quick, running: isRunning, active: isActive };
let show: Show = ((): Show => { const v = store.get("ptyro.show"); return SHOWS.find((x) => x === v) ?? "all"; })();

// ── app state and views ─────────────────────────────────────────────────────────────────────

const app = {
  sessions: new Map<string, Session>(),
  screens: new Map<string, Screen>(),
  /// Unsent replies, per session.
  drafts: new Map<string, string>(),
  viewing: null as string | null,
  toast: "",
};

const root = document.getElementById("app") as HTMLElement;

/// `#/s/<id>` a session, `#/<name>/<rest>` a registered view (kit.ts), else the list.
function route(): { view: string; id?: string } {
  const m = location.hash.match(/^#\/s\/(.+)$/);
  if (m) {
    try { return { view: "session", id: decodeURIComponent(m[1]) }; } catch { return { view: "list" }; }
  }
  const v = location.hash.match(/^#\/([a-z]+)(?:\/(.*))?$/);
  if (v && views.has(v[1])) return { view: v[1], id: v[2] ?? "" };
  return { view: "list" };
}

/// The tabs along the bottom of a top-level page; the active one is the page's.
const TABS: [string, string, string][] = [["list", "#/", "Sessions"], ["chats", "#/chats", "Chats"], ["p", "#/p", "Projects"], ["more", "#/more", "More"]];
function withTabs(view: string, page: HTMLElement): HTMLElement {
  // Chats switched off (`[chat] enabled = false`) have no tab.
  const tabs = TABS.filter(([v]) => v !== "chats" || chatsShown());
  if (!tabs.some(([v]) => v === view)) return page;
  page.classList.add("tabbed");
  page.append(h("nav", { class: "tabs" }, ...tabs.map(([v, href, label]) => h("a", { href, class: v === view ? "on" : "" }, label))));
  return page;
}

async function refresh(): Promise<void> {
  try {
    const r = await conn.request({ t: "list" });
    // A chat's engine belongs to its chat, not to the session list (SPEC §22.2).
    app.sessions = new Map((r.sessions as Session[]).filter((s) => !s.chat).map((s) => [s.id, s]));
  } catch { /* reconnecting */ }
  render();
}

function toast(text: string): void {
  app.toast = text;
  render();
  setTimeout(() => { if (app.toast === text) { app.toast = ""; render(); } }, 3500);
}

async function answer(id: string, decision: string): Promise<void> {
  // The prompt this answers (the blocked status shown): a different one pending now is refused.
  const st = app.sessions.get(id)?.status;
  const expect = st?.status === "blocked" ? st.since_ms : undefined;
  try {
    await conn.request({ t: "answer", addr: id, decision, expect });
    toast(decision === "deny" ? "Denied" : "Allowed");
  } catch (e) {
    if ((e as { code?: string }).code === "conflict") { toast("That prompt changed — have another look"); void refresh(); }
    else toast((e as Error).message);
  }
}

/// The daemon's computer's load average (`[ui] load`): from `status` on connecting, then
/// `host_load` events, which change only this text (every 5 s), never the page.
type HostLoad = { avg: [number, number, number]; cpus: number };
let hostLoad: HostLoad | null = null;
const loadText = (l: HostLoad): string => l.avg.map((a) => (a / 100).toFixed(2)).join(" ");
const loadLevel = (l: HostLoad): string => l.avg[0] >= 200 * Math.max(1, l.cpus) ? "bad" : l.avg[0] >= 100 * Math.max(1, l.cpus) ? "warn" : "";
const machineTitle = (): string => [conn.machine ? `This ptyro runs on ${conn.machine}` : "", hostLoad ? `load average ${loadText(hostLoad)} (1, 5, 15 minutes) on ${hostLoad.cpus} CPUs` : ""].filter(Boolean).join("; ");
const loadSpan = (): HTMLElement | null => hostLoad ? h("span", { class: `load ${loadLevel(hostLoad)}`, title: tip("host.load") }, `${conn.machine ? " · " : ""}${loadText(hostLoad)}`) : null;
function setHostLoad(l: HostLoad | null): void {
  const had = !!hostLoad;
  hostLoad = l;
  // Shown or hidden: the header changes shape. Else only the numbers.
  if (had !== !!l) { render(); return; }
  for (const chip of document.querySelectorAll<HTMLElement>("header .machine")) {
    chip.title = machineTitle();
    chip.querySelector(".load")?.replaceWith(loadSpan() ?? "");
  }
}

/// This device gets push notifications: it has a subscription the daemon knows (`syncPush`, on
/// every connect). Until then a top-level page offers them (More → This device always has them,
/// and their test). Not the browser's permission alone: a device that forgot its pairing keeps the
/// permission but lost its subscription. Until the first sync, the permission says (no 🔔 flashing
/// up on every load of a device that has them).
let pushOn = "Notification" in window && Notification.permission === "granted";
const pushOffered = (): boolean => !pushOn;

/// `back`: true goes to the session list; a string is where it goes instead.
/// What comes and goes after the page is drawn (the machine's chip once connected, its load once
/// known, 🔔) sits next to the title, which gives it room: the page's own controls, to its right,
/// never move under a finger or a pointer that is about to press them.
function header(title: string, back: boolean | string, ...extra: Child[]): HTMLElement {
  const dot = h("span", { class: `dot ${conn.state}`, title: conn.state });
  return h("header", {},
    back ? h("a", { class: "back", href: typeof back === "string" ? back : "#/", "aria-label": "Back" }, "‹") : h("img", { class: "logo", src: "/icon.svg", alt: "" }),
    h("h1", {}, title),
    conn.machine || hostLoad ? h("span", { class: "machine", title: machineTitle() }, conn.machine, loadSpan()) : null,
    !back && pushOffered() ? h("button", { class: "icon", title: tip("notifications"), onclick: () => void enablePush() }, "🔔") : null,
    ...extra,
    dot,
  );
}

function sessionCard(s: Session): HTMLElement {
  const st = statusOf(s);
  const detail = s.status?.status === "blocked" ? s.status.detail ?? s.attention?.text : s.activity ?? s.status?.detail ?? s.title;
  const card = h("a", { class: `card ${st}${selecting && marked.has(s.id) ? " picked" : ""}`, href: selecting ? "#/" : `#/s/${encodeURIComponent(s.id)}`, title: tip("session.open"),
    onclick: selecting ? (e: Event) => { e.preventDefault(); if (!marked.delete(s.id)) marked.add(s.id); render(); } : undefined },
    h("span", { class: "glyph" }, selecting ? (marked.has(s.id) ? "☑" : "☐") : GLYPH[st] ?? "·"),
    h("div", { class: "main" },
      // Another machine's (MM1.B): its name without the machine, and the machine's chip.
      h("div", { class: "name" }, s.machine ? splitMachine(s.name)[1] : s.name, s.agent ? h("small", {}, ` ${s.agent}`, s.model ? ` · ${modelLabel(s.model)}` : "") : null, s.machine ? " " : null, machineChip(s.name)),
      h("div", { class: "sub" }, [statusWord(st), s.project, s.worktree, s.private && "◌ private"].filter(Boolean).join(" · ")),
      detail ? h("div", { class: "detail" }, detail) : null,
    ),
    h("time", {}, s.status ? ago(s.status.since_ms) : ago(s.created_ms)),
  );
  if (s.status?.status === "blocked" && s.state.t === "running") {
    return h("div", { class: "blocked-wrap" }, card,
      h("div", { class: "actions", title: tip("answer") },
        h("button", { class: "allow", onclick: () => void answer(s.id, "allow") }, "Allow"),
        h("button", { class: "deny", onclick: () => void answer(s.id, "deny") }, "Deny"),
      ));
  }
  return card;
}

/// Marking sessions to type into several at once (`broadcast`).
let selecting = false;

/// The search box above the list (`session.find`): one element that stays, so typing keeps its
/// focus while the cards under it are replaced.
let query = "";
const searchBox = h("input", { class: "search", type: "search", placeholder: "Find a session", "aria-label": "Find a session", autocapitalize: "off", autocomplete: "off", title: tipOf("session.find") });
const searchBar = h("div", { class: "searchbar" }, searchBox);
let listCards: () => Node[] = () => [];
searchBox.addEventListener("input", () => {
  query = searchBox.value.trim().toLowerCase();
  const list = document.querySelector<HTMLElement>("main.list .cards");
  if (list) patch(list, listCards());
});
const matches = (s: Session): boolean => !query || [s.name, s.title, s.agent, s.model && modelLabel(s.model), s.project, s.worktree, s.task, s.activity, statusOf(s)].some((x) => x?.toLowerCase().includes(query));

function setShow(m: Show): void {
  show = m;
  store.set("ptyro.show", m);
  render();
}

function listView(): HTMLElement {
  const order = ["blocked", "errored", "working", "starting", "done", "idle", "unknown", "shell", "dormant", "exited"];
  const everything = [...app.sessions.values()].filter(KEEPS[show]).sort((a, b) => order.indexOf(statusOf(a)) - order.indexOf(statusOf(b)) || a.name.localeCompare(b.name));
  const all = [...app.sessions.values()].filter(KEEPS.all);
  // SHELLS have their group (SPEC §23.14), so they aren't among the cards twice.
  listCards = () => { const f = everything.filter((s) => !s.loose && matches(s)); return f.length ? f.map(sessionCard) : query ? [h("p", { class: "empty" }, `Nothing matches “${query}”.`)] : []; };
  const body = everything.length ? listCards() : all.length ? [h("div", { class: "empty" },
    h("p", {}, `Nothing is ${show} right now.`),
    h("button", { class: "small", title: tipOf("sidebar.filter"), onclick: () => setShow("all") }, "Show every session"))] : [h("div", { class: "empty" },
    h("p", {}, "Nothing is running."),
    h("button", { class: "primary", title: tipOf("session.new"), onclick: () => newTask() }, "Start a task"),
    data.projects.size ? null : h("p", {}, h("a", { href: "#/setup", title: tipOf("first.run") }, "Set up ptyro first: your agents, accounts and repositories")))];
  const picked = [...marked].filter((id) => app.sessions.has(id));
  const tools = selecting
    ? h("div", { class: "select-bar" },
        h("span", {}, `${picked.length} selected`),
        h("button", { class: "small primary", disabled: !picked.length, title: tipOf("session.broadcast"), onclick: () => { broadcast(picked); selecting = false; render(); } }, "Type to them"),
        h("button", { class: "small", onclick: () => { selecting = false; marked.clear(); render(); } }, "Cancel"))
    : null;
  const head = header("ptyro", false,
    all.length > 1 ? h("button", { class: "icon", title: tipOf("session.broadcast"), onclick: () => { selecting = true; render(); } }, "☑") : null,
    h("button", { class: "icon plus", title: tipOf("session.new"), "aria-label": "New task", onclick: () => newTask() }, "＋"));
  const counts = SHOWS.map((m) => all.filter(KEEPS[m]).length);
  const filter = all.length ? h("div", { class: "chips filter", title: tipOf("sidebar.filter") },
    ...SHOWS.map((m, i) => h("button", { class: `${m === show ? "on" : ""} ${m}`, "aria-pressed": String(m === show), onclick: () => setShow(m) }, `${m} `, h("span", { class: "n" }, String(counts[i]))))) : null;
  // On a wide screen, the CHATS section beside them (the TUI's sidebar has it under each workspace).
  const aside = chatsShown() ? chatsAside() : null;
  const shells = can("full") || [...app.sessions.values()].some((s) => s.loose) ? shellsGroup(app.sessions.values(), sessionCard) : null;
  return h("div", {}, head, tools, filter, all.length > 6 || query ? searchBar : null, h("main", { class: `list${aside ? " with-chats" : ""}`, title: tip("sessions") }, setupBanner(), shells, h("div", { class: "cards" }, ...body), aside));
}

/// The open session's page. Its compose box and terminal stay in the DOM across renders (updated
/// in place) so the draft, caret and IME composition survive the events that re-render
/// everything around them.
interface Shell {
  id: string;
  el: HTMLElement;
  term: Term;
  /// ✎ compose (WEB2.2): a box for a long prompt or dictation, shown only when asked for.
  compose: HTMLElement;
  draft: HTMLTextAreaElement;
  composing: boolean;
  openCompose(): void;
  /// Closed with the draft kept; the terminal has the keys again (with a keyboard of its own).
  closeCompose(): void;
  /// The terminal should take the focus as soon as it may (the page opened, the window came back).
  want: boolean;
  /// It could be typed to at the last render: becoming typeable (connected, restarted) wants focus.
  could: boolean;
}
let shell: Shell | null = null;

/// This device may type into the session: `control` scope or more, and it is running.
function canType(id: string): boolean {
  return (conn.scope === "control" || conn.scope === "full") && app.sessions.get(id)?.state.t === "running";
}

/// Why the session page has nothing to type with (a viewer, an ended session).
function whyNot(s: Session): string {
  if (conn.state !== "open") return "Connecting…";
  if (conn.scope !== "control" && conn.scope !== "full") return "Watching: this device may look, not type";
  if (s.state.t === "dormant") return "Hibernated: resume it to type";
  if (s.state.t === "starting") return "Starting…";
  return "It has ended: restart it to type";
}

// ── every key exactly once (WEB2.3) ─────────────────────────────────────────────────────────

/// Each session's outbox (`outbox.ts`): its keys numbered and kept until the daemon settles them.
/// They live as long as the page, not the terminal, so keys still waiting when you leave a
/// session's page are sent (or said) all the same.
const outboxes = new Map<string, Outbox>();
/// What each session's page last drew (WEB2.3): its frame's state token, the daemon's clock as
/// it was made (`at_ms`) and when it came here. Keys carry it, so a key is vouched for only by a
/// screen its page drew (never a list's newer token) and the daemon can tell when it arrives late.
const frames = new Map<string, { token: string; at: number | null; rx: number }>();

function outboxOf(id: string): Outbox {
  const had = outboxes.get(id);
  if (had) return had;
  let timer = 0;
  let lost = 0;
  const o: Outbox = new Outbox(id, {
    send: (m) => conn.send(m),
    now: () => performance.now(),
    seen: () => {
      const f = frames.get(id);
      return f ? { token: f.token, clock: f.at === null ? null : f.at + (performance.now() - f.rx) } : { token: null, clock: null };
    },
    changed: () => {
      if (shell?.id === id) shell.term.showOutbox();
      // Lost on another page: said there.
      else if (o.lost.length && o.lost.length !== lost) toast(`${app.sessions.get(id)?.name ?? "A session"}: ${lostTitle(o.lost[o.lost.length - 1])}`);
      lost = o.lost.length;
    },
    check: () => conn.hurry(),
    later: (ms) => { clearTimeout(timer); timer = window.setTimeout(() => o.tick(), ms); },
  });
  outboxes.set(id, o);
  if (conn.state === "open") o.connected(conn.features.includes(FEATURE));
  return o;
}

// ── terminal first (WEB2.2): the terminal is where you type ────────────────────────────────

/// A key was pressed outside any field since the page loaded: this device has a keyboard of its
/// own (a phone with a Bluetooth keyboard, an iPad's), so the terminal may take the focus.
let keySeen = false;
const coarse = (): boolean => matchMedia("(pointer: coarse)").matches;
const focusOf = (el: Element | null): Focus =>
  !el || el === document.body || el === document.documentElement ? "none"
    : (el as HTMLElement).isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) ? "field" : "control";
/// The page (not the terminal: its own selection is drawn, not the document's) has text selected.
const pageSelected = (): boolean => { const s = getSelection(); return !!s && !s.isCollapsed && !!s.toString(); };

/// Give the terminal the focus if the page wants it to and may: a keyboard of the device's own
/// (a phone's would pop up unasked), nothing else busy (a field, a dialog, a selection, compose).
function termFirst(): void {
  const sh = shell;
  if (!sh || sh.id !== app.viewing || !sh.term.el.isConnected) return;
  const can = canType(sh.id);
  if (can && !sh.could) sh.want = true;
  sh.could = can;
  if (!sh.want || !can || sh.composing) return;
  if (document.activeElement === sh.term.input) { sh.want = false; return; }
  if (!ownKeyboard(coarse(), keySeen)) { sh.want = false; return; }
  if (!autoFocus({ canType: can, keyboard: true, focus: focusOf(document.activeElement), dialog: !!document.querySelector(".scrim"), selected: pageSelected() })) return;
  sh.want = false;
  sh.term.focus();
}

/// A key pressed on the page outside the terminal: on a session this device may type to, it goes
/// to the terminal (which takes the focus and the key: `strayKey`). True when it was taken.
function strayToTerm(e: KeyboardEvent): boolean {
  const focus = focusOf(e.target as Element);
  if (hardKey(e, focus)) keySeen = true;
  const sh = shell;
  const r = route();
  if (!sh || r.view !== "session" || r.id !== sh.id || !canType(sh.id) || !sh.term.el.isConnected) return false;
  if (document.querySelector(".scrim") || !strayKey(e, focus, pageSelected())) return false;
  return sh.term.takeKey(e);
}

window.addEventListener("focus", (e) => {
  if (e.target !== window || !shell || shell.id !== app.viewing) return;
  shell.want = true;
  termFirst();
});

function sessionShell(id: string): Shell {
  if (shell?.id === id) return shell;
  // The terminal of the session left behind: its listeners and its canvas go.
  shell?.term.dispose();
  const draft = h("textarea", { rows: 3, enterkeyhint: "send", title: tip("session.compose"), "aria-label": "Compose a message to the session", autocapitalize: "sentences" });
  draft.value = app.drafts.get(id) ?? "";
  // It grows with what is written, up to two fifths of the window, then scrolls.
  const grow = () => { draft.style.height = "auto"; if (draft.scrollHeight) draft.style.height = `${Math.min(draft.scrollHeight + 2, Math.round(innerHeight * 0.4))}px`; };
  draft.addEventListener("input", () => { if (draft.value) app.drafts.set(id, draft.value); else app.drafts.delete(id); grow(); });
  let sending = false;
  // The send's name (WEB2.3, `send_text.once`): the same text sent again after a dropped
  // connection keeps it, so the daemon types it once whatever happened to the first.
  let once = { text: "", name: "" };
  const sendBtn = h("button", { class: "send", title: tip("session.compose"), onclick: () => void send() }, "Send");
  const send = async () => {
    const text = draft.value;
    if (sending || !text.trim() || !canType(id)) return;
    sending = true;
    sendBtn.disabled = true;
    try {
      // Whatever the terminal's keyboard was holding (dictation, a composition) goes first, and
      // every key typed before it is settled before it goes (WEB2.3): it never overtakes keys
      // waiting to be sent, and isn't sent after keys that weren't.
      sh.term.typist.interject();
      const box = outboxOf(id);
      if (box.waiting) {
        const settled = await Promise.race([box.settled(), new Promise<null>((ok) => setTimeout(() => ok(null), GIVE_UP_MS + RESEND_MS))]);
        if (settled !== true) {
          toast(settled === null
            ? "Not sent yet: keys typed before it are still waiting. Send it again once they're through."
            : "Not sent: keys typed before it weren't sent. Look at the session, then send it again.");
          return;
        }
      }
      // The same request as the CLI's `ptyro send`: the daemon pastes a multi-line text (bracketed
      // when the program asked) and then presses Enter, as a key, after a pause.
      if (once.text !== text) once = { text, name: `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}` };
      await conn.request({ t: "send_text", addr: id, text, enter: true, wait_ready_ms: null, once: once.name });
      once = { text: "", name: "" };
      if (draft.value === text) { draft.value = ""; app.drafts.delete(id); grow(); sh.closeCompose(); }
    } catch (e) {
      const code = (e as { code?: string }).code;
      // A daemon that numbers keys (1.52) honours the send's name; an older one types it again.
      toast(code === "daemon_unavailable" || code === "timeout"
        ? conn.features.includes(FEATURE)
          ? "Not sure it was sent: the connection dropped. Send it again: it won't be typed twice."
          : "Not sure it was sent: the connection dropped. Look at the session before sending it again."
        : conn.state !== "open" ? "Not sent: offline. It reconnects by itself; then send it." : (e as Error).message);
    }
    finally { sending = false; sendBtn.disabled = false; }
  };
  draft.addEventListener("keydown", (e) => {
    // An input method's Enter picks its text (Safari says so only with keyCode 229).
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); }
    else if (e.key === "Escape") { e.preventDefault(); sh.closeCompose(); }
  });
  const term = new Term({
    id: () => id,
    screen: () => app.screens.get(id),
    canType: () => canType(id),
    send: (m) => conn.send(m),
    // (Too big for the server, H2.7: the outbox says so, and never keeps it to send again.)
    input: (event) => outboxOf(id).type(event),
    outbox: () => outboxOf(id),
    request: (r) => conn.request(r),
    toast,
    changed: render,
  });
  // Keys still waiting from before, or ones that weren't sent, show on it at once.
  term.showOutbox();
  // What its keys do, as this device has them (a phone's keyboard has no Shift+Enter or Esc).
  const hint = h("p", { class: "compose-hint" }, coarse()
    ? "⏎ sends. Dictate here (the keyboard's mic, Superwhisper), read it over, then send; a dictated new line stays a new line. ✕ keeps the draft."
    : "Enter sends · Shift+Enter: a new line · Esc closes, keeping the draft. Dictate here (macOS dictation, Superwhisper, the keyboard's mic), read it over, then send.");
  const sh: Shell = {
    id, term, draft,
    el: h("div", { class: "session" }),
    compose: h("div", { class: "reply compose" },
      draft,
      h("div", { class: "compose-side" }, sendBtn,
        h("button", { class: "icon", title: "Close: the draft is kept", "aria-label": "Close compose", onclick: () => sh.closeCompose() }, "✕")),
      hint),
    composing: false,
    want: true,
    could: false,
    openCompose: () => {
      sh.composing = true;
      render();
      draft.focus();
      draft.setSelectionRange(draft.value.length, draft.value.length);
      grow();
    },
    closeCompose: () => {
      sh.composing = false;
      sh.want = true;
      render();
    },
  };
  shell = sh;
  return sh;
}

function sessionView(id: string): HTMLElement {
  const s = app.sessions.get(id);
  if (!s) return h("div", {}, header("…", true), h("main", {}, h("p", { class: "empty" }, conn.state === "open" ? "No such session." : "Connecting…")));
  const st = statusOf(s);
  const sh = sessionShell(id);
  sh.draft.placeholder = s.kind === "agent" ? `Write to ${s.name}…` : "Write a command…";
  const blocked = s.status?.status === "blocked" && s.state.t === "running";
  const typeable = canType(id);
  // A key-row button that leaves the focus where it is (the terminal keeps the keyboard up).
  const stay = (e: Event) => e.preventDefault();
  const typing = sh.term.typing;
  const drafted = !!app.drafts.get(id)?.trim();
  patch(sh.el, [
    header(s.name, true, h("button", { class: "icon", title: tipOf("session.menu"), "aria-label": "More", onclick: () => sessionMenu(s) }, "⋯")),
    h("div", { class: `pill ${st}` }, GLYPH[st] ?? "·", " ", statusWord(st), s.model && s.kind === "agent" ? ` · ${modelLabel(s.model)}` : "", s.status?.detail && !blocked ? ` · ${s.status.detail}` : "", s.status ? ` · ${ago(s.status.since_ms)}` : "", s.private ? " · ◌ private" : "", app.screens.get(id)?.title ? ` · ${app.screens.get(id)!.title}` : "", s.machine ? " · " : "", machineChip(s.name)),
    // Its machine is down (MM1.B): this is how it was, and what to do.
    ...(machineOf(s.name) && machineOf(s.name)!.state !== "online" ? [h("p", { class: "banner warn" }, `${machineOf(s.name)!.name} isn't reachable: this is how it was. `, machineOf(s.name)!.fix ?? "")] : []),
    ...(blocked ? [h("section", { class: "prompt" },
      h("div", {}, h("b", {}, s.status?.kind === "permission" ? "Wants permission" : "Waiting for you"), s.held ? h("span", { class: "tag", title: "answers go to the agent as a decision" }, "exact") : null),
      h("pre", {}, s.status?.detail ?? s.attention?.text ?? ""),
      h("div", { class: "actions", title: tip("answer") },
        h("button", { class: "allow", onclick: () => void answer(id, "allow") }, "Allow"),
        h("button", { class: "always", onclick: () => void answer(id, "always") }, "Always"),
        h("button", { class: "deny", onclick: () => void answer(id, "deny") }, "Deny"),
      ))] : []),
    ...(s.state.t === "dormant" ? [h("section", { class: "revive" }, h("span", {}, "Hibernated: its conversation is kept."), h("button", { class: "primary", title: tipOf("session.resume"), onclick: () => void conn.request({ t: "resume", addr: id }).then(() => toast("Resumed"), (e) => toast((e as Error).message)) }, "Resume"))]
      : s.state.t !== "running" ? [h("section", { class: "revive" }, h("span", {}, s.state.t === "failed" ? "It failed to run." : s.state.code != null ? `Exited with ${s.state.code}.` : "Exited."),
          h("button", { class: "primary", title: tipOf("session.restart"), onclick: () => void conn.request({ t: "restart", addr: id }).then(() => toast("Restarted"), (e) => toast((e as Error).message)) }, "Restart"))] : []),
    // A shell of SHELLS: its workspace's shells by number, tmux's status line (SPEC §23.14).
    s.loose ? shellStrip(app.sessions.values(), s) : null,
    sh.term.el,
    typeable ? sh.term.keybar : null,
    h("div", { class: "keys term-more" },
      // The way in to typing, always in sight (a phone's keyboard comes up only when asked for);
      // while typing, the way to put the keyboard away.
      typeable ? h("button", { class: `type${typing ? "" : " primary"}`, title: tipOf("term.type"), "aria-pressed": String(typing), onpointerdown: stay, onmousedown: stay, onclick: () => { if (sh.term.typing) sh.term.input.blur(); else sh.term.focus(); } }, typing ? "⌨ Done" : "⌨ Type") : null,
      typeable ? h("button", { class: `compose-btn${sh.composing ? " on" : ""}`, title: tip("session.compose"), "aria-expanded": String(sh.composing), onclick: () => { if (sh.composing) sh.closeCompose(); else sh.openCompose(); } }, "✎ Compose", drafted && !sh.composing ? h("span", { class: "dot", title: "A draft is waiting" }, " •") : null) : null,
      typeable ? null : h("span", { class: "sub" }, whyNot(s)),
      h("button", { class: "aa", title: tipOf("term.options"), onclick: () => sh.term.options() }, "Aa")),
    typeable && sh.composing ? sh.compose : null,
  ].filter((x): x is HTMLElement => !!x));
  return sh.el;
}

function paint(): void {
  if (!shell || shell.id !== app.viewing || !shell.term.canvas.isConnected) return;
  shell.term.paint();
}

// A press holds back drawing the page (`PressGate`): the click lands on the control pressed.
const presses = new PressGate(() => render());
window.addEventListener("pointerdown", () => presses.pressed(), true);
for (const t of ["pointerup", "pointercancel", "blur"]) window.addEventListener(t, (e) => { if (t !== "blur" || e.target === window) presses.released(); }, true);

function render(): void {
  if (presses.hold()) return;
  if (!token()) {
    root.replaceChildren(pairView());
    return;
  }
  if (conn.state === "rejected") {
    root.replaceChildren(h("div", {}, header("ptyro", false), h("main", { class: "center" },
      h("p", {}, conn.reason),
      h("button", { title: tip("unpair"), onclick: () => void forget() }, "Forget this pairing"),
    )));
    return;
  }
  const r = route();
  if (r.view !== "session" || r.id !== app.viewing) subscribe(r.view === "session" ? r.id! : null);
  const here = r.view === "session" ? r.id! : null;
  const entered = here !== shownSession;
  shownSession = here;
  const view = withTabs(r.view, r.view === "session" ? sessionView(r.id!) : views.has(r.view) ? views.get(r.view)!(r.id ?? "") : listView());
  // A session page opened (or opened again): its terminal has the keys, once it may.
  if (entered && shell && shell.id === here) shell.want = true;
  if (entered && here) void readAltDigits();
  if (conn.state !== "open") view.prepend(h("div", { class: "offline", role: "status" }, offlineText(conn.state, location.hostname)));
  if (root.firstChild !== view || root.childNodes.length !== 1) root.replaceChildren(view);
  // The number of agents waiting on you, in the tab's title and on an installed app's icon.
  const waiting = [...app.sessions.values()].filter((x) => x.status?.status === "blocked" && x.state.t === "running").length;
  showMachine(conn.machine, waiting);
  (navigator as Navigator & { setAppBadge?: (n: number) => Promise<void>; clearAppBadge?: () => Promise<void> })[waiting ? "setAppBadge" : "clearAppBadge"]?.(waiting)?.catch(() => {});
  const t = app.toast ? h("div", { class: "toast" }, app.toast) : null;
  for (const old of view.querySelectorAll(":scope > .toast")) old.remove();
  if (t) view.append(t);
  paint();
  termFirst();
}
/// The session whose page the last render drew (null: another page).
let shownSession: string | null = null;

function subscribe(id: string | null): void {
  if (app.viewing) {
    if (shell?.id === app.viewing) shell.term.release();
    conn.send({ t: "unsubscribe", session: app.viewing });
    // Its screen is no use unsubscribed (a new subscription starts with a keyframe).
    app.screens.delete(app.viewing);
  }
  app.viewing = id;
  if (id) {
    app.screens.set(id, new Screen());
    conn.send({ t: "subscribe", session: id, size: null, control: false });
  }
}

/// Forget this device's pairing (the token here; `ptyro devices revoke` ends it for good).
async function forget(): Promise<void> {
  store.remove("ptyro.token");
  await idbSet("token", null).catch(() => {});
  // This device's push subscription goes too: nothing should be sent to a device that forgot.
  try {
    const reg = await Promise.race([navigator.serviceWorker.ready, new Promise<undefined>((ok) => setTimeout(() => ok(undefined), 1500))]);
    await (await reg?.pushManager.getSubscription())?.unsubscribe();
  } catch { /* no service worker, or none to forget */ }
  location.hash = "#/";
  location.reload();
}

function pairView(): HTMLElement {
  const input = h("input", { placeholder: "Paste the link from `ptyro pair`", autocomplete: "off", title: tip("pair") });
  return h("main", { class: "center pair" },
    h("img", { src: "/icon.svg", width: 72, height: 72, alt: "" }),
    h("h1", {}, "ptyro"),
    h("p", {}, "Run ", h("code", {}, "ptyro pair"), " on your computer and scan the QR code with this device."),
    input,
    h("button", { onclick: () => {
      const m = input.value.match(/pair=([A-Za-z0-9_\-]+)/) ?? input.value.match(/^(ptp_[A-Za-z0-9_\-]+)$/);
      if (m) void claim(m[1]);
    } }, "Sign in"),
    pairNote ? h("p", {}, pairNote) : null,
  );
}

// ── push notifications ──────────────────────────────────────────────────────────────────────

function keyBytes(b64: string): Uint8Array {
  const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(b64.length / 4) * 4, "="));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}
function b64u(buf: ArrayBuffer | null): string {
  if (!buf) return "";
  return btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function enablePush(): Promise<void> {
  if (!("serviceWorker" in navigator) || !("PushManager" in window)) {
    toast(/iPhone|iPad/.test(navigator.userAgent) ? "Add ptyro to your Home Screen first (Share → Add to Home Screen), then open it from there." : "This browser can't receive push notifications.");
    return;
  }
  if (!isSecureContext) { toast("Notifications need HTTPS: open the https:// address `ptyro pair` shows."); return; }
  const perm = await Notification.requestPermission();
  if (perm !== "granted") { toast("Notifications are blocked for this site."); return; }
  try {
    const reg = await navigator.serviceWorker.ready;
    const { key } = await conn.request({ t: "push_key" });
    let sub = await reg.pushManager.getSubscription();
    if (sub) await sub.unsubscribe();
    sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) as BufferSource });
    await conn.request({ t: "push_subscribe", sub: { endpoint: sub.endpoint, p256dh: b64u(sub.getKey("p256dh")), auth: b64u(sub.getKey("auth")) } });
    await conn.request({ t: "push_test" });
    setPushOn(true);
    toast("Notifications on — a test is on its way.");
  } catch (e) { toast(`Notifications: ${(e as Error).message}`); }
}

function setPushOn(on: boolean): void {
  if (pushOn === on) return;
  pushOn = on;
  render();
}

/// On every connect: with notifications already allowed, this device's subscription is made again
/// if it is missing (paired again after Forget) or for another key (the daemon's changed), and
/// told to the daemon (which may have lost it: a device revoked and paired again). Asks nothing:
/// without the permission, the 🔔 offers it.
async function syncPush(): Promise<void> {
  if (!("serviceWorker" in navigator) || !("PushManager" in window) || !isSecureContext || !("Notification" in window) || Notification.permission !== "granted") {
    setPushOn(false);
    return;
  }
  try {
    const reg = await Promise.race([navigator.serviceWorker.ready, new Promise<undefined>((ok) => setTimeout(() => ok(undefined), 5000))]);
    if (!reg) { setPushOn(false); return; }
    const { key } = await conn.request({ t: "push_key" });
    let sub = await reg.pushManager.getSubscription();
    if (sub && b64u(sub.options.applicationServerKey) !== key.replace(/=+$/, "")) { await sub.unsubscribe(); sub = null; }
    sub ??= await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) as BufferSource });
    await conn.request({ t: "push_subscribe", sub: { endpoint: sub.endpoint, p256dh: b64u(sub.getKey("p256dh")), auth: b64u(sub.getKey("auth")) } });
    setPushOn(true);
  } catch { setPushOn(false); }
}

// ── wiring ──────────────────────────────────────────────────────────────────────────────────

/// Option on a Mac types the layout's own characters; Alt elsewhere doesn't (keys.ts `shellDigit`).
const MAC = isMac((navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform || navigator.platform || navigator.userAgent);

conn.listeners.add((msg) => {
  if (msg.t === "disconnected") {
    for (const o of outboxes.values()) o.disconnected();
    return;
  }
  if (msg.t === "typed") {
    outboxes.get(msg.session)?.typed(msg);
    // When each key was typed: its predicted echo is due soon after (WEB2.4).
    const sh = shell;
    if (sh && sh.id === msg.session && msg.stream === outboxes.get(msg.session)?.stream) sh.term.typed(msg.upto);
    return;
  }
  if (msg.t === "connected") {
    // What waited to be typed goes first (WEB2.3), numbered as before.
    for (const o of outboxes.values()) o.connected(conn.features.includes(FEATURE));
    // A fresh connection (start-up, reconnect, live upgrade) has no subscriptions: renew the
    // live view's, from a clean screen.
    if (app.viewing) {
      app.screens.set(app.viewing, new Screen());
      conn.send({ t: "subscribe", session: app.viewing, size: null, control: false });
      if (shell?.id === app.viewing) shell.term.reassert();
    }
    void refresh();
    void loadAll();
    void syncPush();
    void readAltDigits();
    setupConnected();
    conn.request({ t: "status" }).then((r) => setHostLoad(r.status?.load ?? null), () => {});
    return;
  }
  if (msg.t === "screen") {
    const f = msg.frame as Frame;
    const s = app.screens.get(f.session);
    if (s) {
      s.apply(f);
      if (f.state_token) frames.set(f.session, { token: f.state_token, at: typeof f.at_ms === "number" ? f.at_ms : null, rx: performance.now() });
      if (f.session === app.viewing && shell?.id === f.session) shell.term.onFrame();
    }
    // Acked once drawn, so a slow screen gets fewer, bigger deltas; a hidden tab doesn't wait.
    const ack = () => conn.send({ t: "ack", session: f.session, seq: f.seq });
    if (document.hidden) ack(); else requestAnimationFrame(ack);
    return;
  }
  if (msg.t === "event") {
    const e = msg.event;
    if (e.t === "host_load") { setHostLoad(e.load ?? null); return; }
    staleMaybeChanged(e);
    if (onEvent(e)) { render(); return; }
    if (e.info?.chat) return;
    if (e.info && (e.t === "session_created" || e.t === "session_updated" || e.t === "session_exited" || e.t === "session_restarted")) {
      app.sessions.set(e.info.id, e.info);
      if (e.t === "session_restarted" && e.info.id === app.viewing) { app.viewing = null; }
      render();
    } else if (e.t === "session_removed") {
      app.sessions.delete(e.id);
      app.screens.delete(e.id);
      app.drafts.delete(e.id);
      render();
    } else if (e.t === "bell") {
      if (shell && shell.id === e.session) shell.term.bell();
    } else if (e.t === "clipboard") {
      if (shell && shell.id === e.session) shell.term.fromProgram(e.text);
    } else if (e.t === "setup_progress") {
      setupEvent(e);
    }
  }
});
useSessions(() => app.sessions);
conn.onState = render;
// Typing while the connection is down goes nowhere: say so (not for every key).
let droppedAt = 0;
conn.onDropped = (m) => {
  if (m.t !== "input" || Date.now() - droppedAt < 4000) return;
  droppedAt = Date.now();
  toast("Not sent: offline. It reconnects by itself.");
};
conn.onTooBig = (m) => toast(m.t === "input" ? "Not sent: that paste is too large (over 4 MiB)" : "Not sent: too large (over 4 MiB)");
ui.render = render;
ui.toast = toast;
ui.fail = (text) => { if (!offerSetup(text)) toast(text); };
ui.header = header;
ui.push = () => void enablePush();
ui.forget = () => { void forget(); };

// SHELLS' Alt+1…9 and Alt+0 (keys.ts `shellDigit`): shell N, from anywhere, the terminal too:
// taken here, before the terminal's own keydown, so the program never sees it (as in the TUI).
window.addEventListener("keydown", (e) => {
  const t = e.target as Element;
  const n = token() ? shellDigit(e, { focus: focusOf(t), terminal: !!shell && t === shell.term.input, dialog: !!document.querySelector(".scrim"), on: altDigitsOn(), mac: MAC }) : null;
  if (n == null) return;
  e.preventDefault();
  e.stopPropagation();
  const r = route();
  void goShell(app.sessions.values(), n, shellsWorkspace(r.view === "session" ? app.sessions.get(r.id ?? "") : undefined));
}, true);

// Keys, for a desktop: n new task, m new chat, / find, C-g find in every chat, g then s c p m go to a tab, ? this list.
let goKey = 0;
document.addEventListener("keydown", (e) => {
  const t = e.target as HTMLElement;
  // On a session this device may type to, a key pressed anywhere goes to its terminal (WEB2.2).
  if (!e.defaultPrevented && token() && strayToTerm(e)) return;
  // C-g: find across every chat (not in a terminal, whose program has the key).
  if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "g" && token() && chatsShown() && route().view !== "session" && !document.querySelector(".scrim")) { e.preventDefault(); findAllChats(); return; }
  if (e.metaKey || e.ctrlKey || e.altKey || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName) || t.isContentEditable || document.querySelector(".scrim") || !token()) return;
  const tabs: Record<string, string> = { s: "#/", c: "#/chats", p: "#/p", m: "#/more" };
  if (goKey && Date.now() - goKey < 1500 && tabs[e.key]) { goKey = 0; location.hash = tabs[e.key]; e.preventDefault(); return; }
  goKey = 0;
  if (e.key === "g") goKey = Date.now();
  else if (e.key === "n") { e.preventDefault(); newTask(); }
  // SHELLS (SPEC §23.14): a digit shows that shell, on the list and on a shell's page this device
  // only watches (where it may type, every key is the terminal's: Alt+N and the strip switch).
  else if (/^[1-9]$/.test(e.key) && (route().view === "list" || (route().view === "session" && app.sessions.get(route().id ?? "")?.loose))) {
    e.preventDefault();
    const here = app.sessions.get(route().id ?? "");
    void goShell(app.sessions.values(), Number(e.key), here?.loose ? here.workspace ?? "main" : listWorkspace());
  }
  else if (e.key === "m" && chatsShown()) { e.preventDefault(); newChat(); }
  else if (e.key === "/" && route().view === "chats") { e.preventDefault(); document.querySelector<HTMLInputElement>("input.search")?.focus(); }
  // In a chat, `/` starts a slash command in its composer.
  else if (e.key === "/" && route().view === "c") {
    const r = document.querySelector<HTMLTextAreaElement>(".chat-page .reply textarea");
    if (r) { e.preventDefault(); if (!r.value) { r.value = "/"; r.dispatchEvent(new Event("input")); } r.focus(); }
  }
  else if (e.key === "/") { e.preventDefault(); if (route().view !== "list") location.hash = "#/"; setTimeout(() => (searchBox.isConnected ? searchBox : document.querySelector<HTMLInputElement>("input.search"))?.focus(), 50); }
  else if (e.key === "?") sheet("Keys", [h("dl", { class: "facts" }, ...([["n", "New task"], ["m", "New chat"], ["Alt+1…9", "Shell N of SHELLS, from anywhere, while typing in a terminal too (Option on a Mac; Alt+0: shell 10)"], ["1…9", "Shell N of SHELLS (on the list, and a shell's page you only watch)"], ["any key", "On a session you may type to: typed into its terminal (Tab still moves between buttons)"], ["/", "Find a session (in the chats: a chat; in a chat: a command)"], ["Ctrl-G", "Search every chat"], ["Enter", "In a chat: send (Shift+Enter: a new line)"], ["@", "In a chat's box: its folders"], ["g s", "Sessions"], ["g c", "Chats"], ["g p", "Projects"], ["g m", "More"], ["Esc", "Close a dialog"]] as [string, string][]).flatMap(([k, d]) => [h("dt", {}, h("kbd", {}, k)), h("dd", {}, d)]))]);
});

window.addEventListener("hashchange", () => { takePairCode(); render(); });
takePairCode();
if ("serviceWorker" in navigator) void navigator.serviceWorker.register("/sw.js");
render();
conn.connect();
setInterval(() => { if (route().view !== "session") render(); }, 30_000);

