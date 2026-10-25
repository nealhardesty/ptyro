// The settings panel's pure parts (ROADMAP CFG1.3): what a setting is, which ones a search finds,
// which show next to their siblings, how a list is typed, and the name of a key press. No DOM,
// so `web/test/config.mjs` runs them in Node.
export const APPLY = {
    live: "applies at once",
    next: "applies to new sessions",
    restart: "needs a daemon restart",
};
export const ORIGIN = {
    default: "default",
    user: "yours",
    project: "shared (.ptyro.toml)",
    local: "mine (.ptyro.local.toml)",
};
/// The last part of a key, for a label: `ui.sidebar_width` → `sidebar width`.
export const labelOf = (key, member = false) => {
    const parts = key.split(".");
    const tail = member ? parts.slice(-1)[0] : parts.slice(1).join(".") || parts[0];
    return tail.replace(/_/g, " ");
};
/// Whether a setting is relevant next to its siblings: a sink's `url` is for `ntfy`.
export function relevant(e, setting, entries) {
    const w = setting.when;
    if (!w || !e.member)
        return true;
    const sibling = `${e.member[0]}.${e.member[1]}.${w.key}`;
    const v = entries.find((x) => x.key === sibling)?.value;
    return typeof v === "string" && w.any.includes(v);
}
/// Words typed in the search box, against a setting's key, help and shown value. Every word has
/// to be in it (any order); a hit in the key ranks above one in the help.
export function score(query, key, help, shown) {
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length)
        return 0;
    const k = key.toLowerCase().replace(/[_.]/g, " "), hp = help.toLowerCase(), sv = shown.toLowerCase();
    let total = 0;
    for (const w of words) {
        if (k.includes(w))
            total += k.split(" ").includes(w) ? 4 : 3;
        else if (hp.includes(w))
            total += 1;
        else if (sv.includes(w))
            total += 1;
        else
            return -1;
    }
    return total;
}
/// A list as lines in a box, and back: blank lines go, surrounding space too.
export const toLines = (v) => (Array.isArray(v) ? v.map(String).join("\n") : "");
export const fromLines = (t) => JSON.stringify(t.split("\n").map((l) => l.trim()).filter(Boolean));
/// A key press as the config writes it: `C-a`, `M-x`, `S-Tab`, `F5`, `Space`.
export function keyLabel(e) {
    if (["Control", "Alt", "Shift", "Meta", "AltGraph", "CapsLock", "Dead"].includes(e.key))
        return null;
    const names = { " ": "Space", ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", Escape: "Esc", Enter: "Enter", Tab: "Tab", Backspace: "Backspace", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown", Insert: "Insert" };
    let k = names[e.key] ?? e.key;
    if (!k)
        return null;
    const named = k.length > 1;
    // A letter says whether Shift was held; a named key or a control letter needs it spelled out.
    let mods = "";
    if (e.ctrlKey)
        mods += "C-";
    if (e.altKey)
        mods += "M-";
    if (e.shiftKey && (named || e.ctrlKey || e.altKey))
        mods += "S-";
    if (e.ctrlKey || e.altKey)
        k = named ? k : k.toLowerCase();
    return mods + k;
}
/// `#rrggbb` when a colour setting holds one (for a colour picker); otherwise null.
export const hexOf = (v) => (typeof v === "string" && /^#[0-9a-fA-F]{6}$/.test(v) ? v.toLowerCase() : null);
/// The settings of the member `name` of collection `path`, in the order the schema lists them.
export function fieldsOf(view, path, name) {
    return view.entries.filter((e) => e.member && e.member[0] === path && e.member[1] === name);
}
/// A member's one line, from its main field.
export function summary(path, name, fields) {
    const get = (suffix) => {
        const e = fields.find((x) => x.key.endsWith(`.${suffix}`));
        return e && e.shown && !e.shown.startsWith("(") && e.shown !== "—" ? e.shown : undefined;
    };
    switch (path) {
        case "agents": return get("command") ?? get("adapter") ?? "runs as its name";
        case "themes": return get("base") ? `based on ${get("base")}` : "colours of your own";
        case "editors":
        case "scripts.run": return get("command") ?? "";
        case "profiles": return get("label") ?? get("git.email") ?? "";
        case "notifications.sinks": return `${get("type") ?? ""} ${get("url") ?? get("chat_id") ?? get("command") ?? ""}`.trim();
        default: return "";
    }
}
/// The fields a new member asks for, besides its name.
export function needed(path, kind) {
    if (path === "notifications.sinks") {
        return { ntfy: ["url"], pushover: ["token", "user"], slack: ["webhook"], discord: ["webhook"], telegram: ["token", "chat_id"], command: ["command"] }[kind] ?? [];
    }
    return path === "editors" || path === "scripts.run" ? ["command"] : [];
}
export const article = (noun) => (/^[aeiou]/i.test(noun) ? "an" : "a");
