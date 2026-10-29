// What every view shares: the DOM helper, sheets (the web's dialogs and menus), forms, and the
// hooks the app wires up (re-render, toast, header). No dependencies.
import { conn } from "./conn.js";
export function h(tag, attrs = {}, ...kids) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
        if (k.startsWith("on") && typeof v === "function")
            el.addEventListener(k.slice(2), v);
        else if (k === "class")
            el.className = v;
        else if (v === true)
            el.setAttribute(k, "");
        else if (v !== false && v != null)
            el.setAttribute(k, String(v));
    }
    for (const c of kids)
        if (c)
            el.append(c);
    return el;
}
export function ago(ms) {
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60)
        return `${s}s`;
    if (s < 3600)
        return `${Math.floor(s / 60)}m`;
    if (s < 86400)
        return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
}
/// Make `kids` the children of `parent`, in order, without moving nodes already there in that
/// order (a moved node loses focus and composition).
export function patch(parent, kids) {
    const keep = new Set(kids);
    for (const c of [...parent.childNodes])
        if (!keep.has(c))
            c.remove();
    let at = parent.firstChild;
    for (const k of kids) {
        if (k === at) {
            at = at.nextSibling;
            continue;
        }
        parent.insertBefore(k, at);
    }
}
// ── actions: the controls, by name, and their tooltips ────────────────────────────────────
const tips = {};
/// Register a module's `ACTIONS` and get its typed `tip`. The Web column of docs/parity.md names
/// these, and `make parity` checks every name it uses is registered in some module's `ACTIONS`.
export function actions(a) {
    Object.assign(tips, a);
    return (k) => a[k];
}
/// Another module's action, by name.
export const tipOf = (k) => tips[k] ?? "";
// ── hooks the app sets ──────────────────────────────────────────────────────────────────────
export const ui = {
    render: () => { },
    toast: (_text) => { },
    /// A page's top bar: the title, a back arrow, and extra controls on its right.
    header: (title, _back, ..._extra) => h("header", {}, h("h1", {}, title)),
    /// Turn notifications on for this device.
    push: () => { },
    /// Forget this device's pairing.
    forget: () => { },
};
export const views = new Map();
// ── polling: data that changes without an event ─────────────────────────────────────────────
const polls = new Map();
/// Call `fn` every `ms` while the page is the one that asked (any other page clears it).
export function poll(name, fn, ms = 4000) {
    if (polls.has(name))
        return;
    polls.set(name, window.setInterval(() => { if (document.visibilityState === "visible")
        fn(); }, ms));
}
window.addEventListener("hashchange", () => { for (const t of polls.values())
    clearInterval(t); polls.clear(); });
// ── asking the daemon ───────────────────────────────────────────────────────────────────────
export const errText = (e) => e?.message || String(e);
/// Run a request; on failure toast why and resolve `undefined`.
export async function ask(req, ok) {
    try {
        const r = await conn.request(req);
        if (ok)
            ui.toast(ok);
        return r;
    }
    catch (e) {
        ui.toast(errText(e));
        return undefined;
    }
}
/// A keyed cache of loaded data for views, which render synchronously: `get` returns what's
/// loaded (and starts loading what isn't, re-rendering when it arrives); `reload` forgets it.
export class Cache {
    fetcher;
    data = new Map();
    loading = new Set();
    errors = new Map();
    /// Bumped when a key is forgotten or replaced: a reply to an older fetch is dropped.
    gen = new Map();
    constructor(fetcher) {
        this.fetcher = fetcher;
    }
    start(key, keepErrors) {
        this.loading.add(key);
        const g = (this.gen.get(key) ?? 0) + 1;
        this.gen.set(key, g);
        this.fetcher(key).then((v) => { if (this.gen.get(key) === g) {
            this.data.set(key, v);
            if (keepErrors)
                this.errors.delete(key);
        } }, (e) => { if (this.gen.get(key) === g && !keepErrors)
            this.errors.set(key, errText(e)); })
            .finally(() => { if (this.gen.get(key) === g)
            this.loading.delete(key); ui.render(); });
    }
    get(key) {
        if (!this.data.has(key) && !this.loading.has(key) && !this.errors.has(key) && conn.state === "open")
            this.start(key, false);
        return this.data.get(key);
    }
    error(key) { return this.errors.get(key); }
    set(key, v) { this.data.set(key, v); this.gen.set(key, (this.gen.get(key) ?? 0) + 1); }
    /// Fetch `key` again, keeping what's shown until the new data arrives (no flicker).
    refresh(key) {
        if (this.loading.has(key) || conn.state !== "open")
            return;
        this.start(key, true);
    }
    /// Forget `key` (everything without one) and load again. A fetch still in flight is ignored
    /// when it lands, so it can't put the data from before the change back.
    reload(key) {
        const keys = key === undefined ? [...new Set([...this.data.keys(), ...this.loading, ...this.errors.keys()])] : [key];
        for (const k of keys) {
            this.gen.set(k, (this.gen.get(k) ?? 0) + 1);
            this.loading.delete(k);
            this.data.delete(k);
            this.errors.delete(k);
        }
        ui.render();
    }
}
/// What a cached value shows while it loads, or when it failed.
export function pending(c, key, what = "Loading…") {
    const err = c.error(key);
    if (err)
        return h("p", { class: "empty err" }, err, " ", h("button", { class: "small", onclick: () => c.reload(key) }, "Retry"));
    return c.get(key) === undefined ? h("p", { class: "empty" }, what) : null;
}
let openSheets = [];
const FOCUSABLE = "a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";
/// A dialog that slides up on a phone and sits in the middle on a desktop. It lives outside the
/// page (so re-renders leave it alone) and closes on Esc or a tap outside it. Focus moves into
/// it and comes back to where it was; the page behind is inert while it is open, and Tab stays inside.
export function sheet(title, body, foot = []) {
    const scrim = h("div", { class: "scrim" });
    const el = h("div", { class: "sheet", role: "dialog", "aria-modal": "true", "aria-label": title }, h("div", { class: "sheet-head" }, h("h2", {}, title), h("button", { class: "icon", "aria-label": "Close", onclick: () => s.close() }, "✕")), h("div", { class: "sheet-body" }, ...body), foot.length ? h("div", { class: "sheet-foot" }, ...foot) : null);
    scrim.append(el);
    const before = document.activeElement;
    const closers = [];
    const onKey = (e) => {
        if (openSheets[openSheets.length - 1] !== s)
            return;
        if (e.key === "Escape") {
            s.close();
            return;
        }
        if (e.key === "Tab") {
            const f = [...el.querySelectorAll(FOCUSABLE)].filter((x) => x.offsetParent !== null);
            if (!f.length)
                return;
            const first = f[0], last = f[f.length - 1];
            if (e.shiftKey && (document.activeElement === first || !el.contains(document.activeElement))) {
                e.preventDefault();
                last.focus();
            }
            else if (!e.shiftKey && (document.activeElement === last || !el.contains(document.activeElement))) {
                e.preventDefault();
                first.focus();
            }
        }
    };
    const s = {
        el: scrim,
        close: () => {
            if (!openSheets.includes(s))
                return;
            scrim.remove();
            document.removeEventListener("keydown", onKey);
            openSheets = openSheets.filter((x) => x !== s);
            setInert(openSheets.length > 0);
            if (before?.isConnected)
                before.focus();
            for (const f of closers.splice(0))
                f();
        },
        onClose: (fn) => { closers.push(fn); },
    };
    scrim.addEventListener("mousedown", (e) => { if (e.target === scrim)
        s.close(); });
    document.addEventListener("keydown", onKey);
    document.body.append(scrim);
    openSheets.push(s);
    setInert(true);
    // Into the first field, else its first button (a menu, a confirmation).
    requestAnimationFrame(() => {
        if (!openSheets.includes(s) || el.contains(document.activeElement))
            return;
        (el.querySelector("input:not([type=checkbox]), textarea, select") ?? el.querySelector(".sheet-body button, .sheet-foot .primary, .sheet-foot button") ?? el.querySelector(FOCUSABLE))?.focus();
    });
    return s;
}
/// The page behind a sheet can't be reached while one is open (focus, screen readers).
function setInert(on) {
    const app = document.getElementById("app");
    if (!app)
        return;
    if (on)
        app.setAttribute("inert", "");
    else
        app.removeAttribute("inert");
}
// A sheet belongs to the page it was opened on.
window.addEventListener("hashchange", () => { for (const s of [...openSheets])
    s.close(); });
/// A list of things to do; choosing one closes the sheet and runs it. Falsy entries are skipped,
/// so a menu can say `cond && {…}`.
export function menu(title, items) {
    const list = items.filter(Boolean);
    const s = sheet(title, [h("div", { class: "menu" }, ...(list.length ? list.map((it) => h("button", {
            class: it.danger ? "danger" : "", disabled: it.disabled, title: it.title,
            onclick: () => { s.close(); void it.run(); },
        }, h("span", {}, it.label), it.hint ? h("small", {}, it.hint) : null)) : [h("p", { class: "empty" }, "Nothing to do here.")]))]);
    return s;
}
/// Ask before doing something that can't be undone.
export function confirm(title, message, label, run, danger = true) {
    const s = sheet(title, [h("p", { class: "confirm" }, message)], [
        h("button", { onclick: () => s.close() }, "Cancel"),
        h("button", { class: danger ? "deny" : "primary", onclick: () => { s.close(); void run(); } }, label),
    ]);
    return s;
}
/// Show text (a command's output, a file) in a sheet, with a copy button.
export function textSheet(title, text) {
    return sheet(title, [h("pre", { class: "block" }, text || "(empty)")], [
        h("button", { onclick: () => void navigator.clipboard?.writeText(text).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy"),
    ]);
}
// ── forms ───────────────────────────────────────────────────────────────────────────────────
/// What a name can't hold becomes `_` as it is typed (the same rule as `names::typed` and
/// `names::branch_typed` in ptyro-core): "name" for an address, "branch" for a git branch.
export function slugify(kind, raw) {
    let out = "";
    for (const c of raw) {
        if (kind === "branch") {
            out += /[\s\x00-\x1f\x7f~^:?*[\\]/.test(c) || (out === "" && c === "-") ? "_" : c;
        }
        else {
            const l = c.toLowerCase();
            const bad = !/^[a-z0-9._-]$/.test(l) || (out === "" && (l === "." || l === "-")) || (l === "." && out.endsWith("."));
            out += bad ? "_" : l;
        }
    }
    return out;
}
/// A form in a sheet. `go` gets the values; if it throws, the message shows in the form and the
/// sheet stays; if it returns, the sheet closes. Empty text fields are left out of the values.
export function form(title, fields, label, go, opts = {}) {
    const inputs = new Map();
    const rows = fields.map((f) => {
        let input;
        if (f.type === "textarea") {
            input = h("textarea", { rows: f.rows ?? 4, placeholder: f.placeholder, autocapitalize: "off" });
            input.value = f.value ?? "";
        }
        else if (f.type === "select") {
            input = h("select", {}, ...f.options.map((o) => { const [v, l] = typeof o === "string" ? [o, o] : o; return h("option", { value: v }, l); }));
            input.value = f.value ?? (typeof f.options[0] === "string" ? f.options[0] : f.options[0][0]);
        }
        else if (f.type === "check") {
            input = h("input", { type: "checkbox" });
            input.checked = !!f.value;
        }
        else {
            input = h("input", { type: f.type ?? "text", placeholder: f.placeholder, autocapitalize: "off", autocomplete: "off", spellcheck: "false", inputmode: f.type === "number" ? "numeric" : undefined });
            input.value = f.value == null ? "" : String(f.value);
            const slug = f.slug;
            const box = input;
            if (slug) {
                // The cursor stays where it was: one character in, one out.
                box.addEventListener("input", () => {
                    const at = box.selectionStart;
                    const fixed = slugify(slug, box.value);
                    if (fixed !== box.value) {
                        box.value = fixed;
                        box.setSelectionRange(at, at);
                    }
                });
            }
        }
        inputs.set(f.key, input);
        return f.type === "check"
            ? h("label", { class: "field check" }, input, h("span", {}, f.label), f.hint ? h("small", {}, f.hint) : null)
            : h("label", { class: "field" }, h("span", {}, f.label), input, f.hint ? h("small", {}, f.hint) : null);
    });
    const err = h("p", { class: "form-err" });
    const submit = h("button", { class: opts.danger ? "deny" : "primary" }, label);
    let busy = false;
    const run = async () => {
        // A second Enter or tap while the first is being answered does nothing.
        if (busy)
            return;
        const v = {};
        for (const f of fields) {
            const i = inputs.get(f.key);
            if (f.type === "check")
                v[f.key] = i.checked;
            else if (i.value.trim() !== "" || f.type === "select")
                v[f.key] = i.value.trim();
            else if ("required" in f && f.required) {
                err.textContent = `${f.label} is needed.`;
                i.focus();
                return;
            }
        }
        err.textContent = "";
        busy = true;
        submit.disabled = true;
        try {
            await go(v);
            s.close();
        }
        catch (e) {
            err.textContent = errText(e);
            submit.disabled = false;
        }
        finally {
            busy = false;
        }
    };
    submit.addEventListener("click", () => void run());
    const s = sheet(title, [opts.intro ? h("p", { class: "intro" }, opts.intro) : null, ...rows, err], [h("button", { onclick: () => s.close() }, "Cancel"), submit]);
    // Enter in a single-line field submits.
    s.el.addEventListener("keydown", (e) => {
        const t = e.target;
        if (e.key === "Enter" && t instanceof HTMLInputElement && t.type !== "checkbox" && !e.isComposing) {
            e.preventDefault();
            void run();
        }
    });
    requestAnimationFrame(() => s.el.querySelector("input:not([type=checkbox]), textarea")?.focus());
    return s;
}
// ── small parts ─────────────────────────────────────────────────────────────────────────────
export const badge = (text, kind = "") => h("span", { class: `badge ${kind}` }, text);
/// A titled block on a detail page.
export function section(title, ...kids) {
    return h("section", { class: "block-sec" }, h("h3", {}, title), ...kids);
}
/// A row of buttons.
export const bar = (...kids) => h("div", { class: "bar" }, ...kids);
/// `label: value` lines.
export function facts(rows) {
    return h("dl", { class: "facts" }, ...rows.filter(([, v]) => v != null && v !== false && v !== "").flatMap(([k, v]) => [h("dt", {}, k), h("dd", {}, v)]));
}
/// A tappable row: a title, a line under it, something on the right.
export function row(title, sub, right, opts = {}) {
    const kids = [h("div", { class: "main" }, h("div", { class: "name" }, title), sub ? h("div", { class: "sub" }, sub) : null), right ? h("div", { class: "right" }, right) : null];
    if (opts.href)
        return h("a", { class: `row ${opts.cls ?? ""}`, href: opts.href }, ...kids);
    return h("div", { class: `row ${opts.cls ?? ""} ${opts.onclick ? "tap" : ""}`, onclick: opts.onclick }, ...kids);
}
/// Save text as a file on this device.
export function download(name, text, type = "text/plain") {
    const url = URL.createObjectURL(new Blob([text], { type }));
    const a = h("a", { href: url, download: name });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
/// Go to a route.
export const go = (hash) => { location.hash = hash; };
export const enc = encodeURIComponent;
/// `decodeURIComponent` that never throws: a malformed escape in a route (`%E0%A4%A`) is kept as typed.
export function safeDecode(s) {
    try {
        return decodeURIComponent(s);
    }
    catch {
        return s;
    }
}
/// An address that came from data (a PR link, a check's log, a message): only `http(s)://` is ever
/// followed. Anything else (`javascript:`, `data:`) is `null`, so it can't become a link.
export function webHref(u) {
    if (typeof u !== "string")
        return null;
    try {
        const url = new URL(u);
        return url.protocol === "https:" || url.protocol === "http:" ? url.href : null;
    }
    catch {
        return null;
    }
}
/// Same as `webHref` (the name the rest of the app reaches for).
export const safeHref = webHref;
/// `window.open` for a data-derived address, with the same rule as `webHref`.
export function openWeb(u) {
    const href = webHref(u);
    if (href)
        window.open(href, "_blank", "noopener,noreferrer");
}
/// Run `fn` once at a time: while one call is in flight, others return `undefined` (a double tap,
/// a second Enter). Wrap a form's or a button's handler with it.
export function once(fn) {
    let busy = false;
    return async (...a) => {
        if (busy)
            return undefined;
        busy = true;
        try {
            return await fn(...a);
        }
        finally {
            busy = false;
        }
    };
}
