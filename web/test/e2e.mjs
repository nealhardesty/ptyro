// The web app in a real browser, against a real daemon and a real `ptyro serve`: pair, list,
// start a task, answer a permission prompt, edit settings, type into a terminal, survive the
// server going away. Run by crates/ptyro/tests/web_e2e.rs (make web-e2e), which sets the
// scene; no dependencies, headless Chrome over the DevTools protocol (web/test/cdp.mjs).
//   E2E='{"bin":..,"env":{..},"config":..,"repo":..}' node web/test/e2e.mjs CHROME BASE CODE [scenario...]
// A scenario that needs the server restarted prints "@@serve down" / "@@serve up" and waits for
// a line on stdin from the Rust test.
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { Chrome, sleep, until } from "./cdp.mjs";

const [exe, base, code, ...only] = process.argv.slice(2);
const E = JSON.parse(process.env.E2E);
const say = (...a) => console.log(...a);

const ptyro = (...args) => execFileSync(E.bin, args, { env: { ...process.env, ...E.env }, cwd: E.repo, encoding: "utf8", timeout: 60_000 });
const sessions = () => {
  const v = JSON.parse(ptyro("list", "--json"));
  return Array.isArray(v) ? v : v.sessions;
};
const find = (name) => sessions().find((s) => s.name === name);
const config = () => readFileSync(E.config, "utf8");

const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const hostOp = async (what) => { say(`@@serve ${what}`); const r = await lines.next(); if (r.value !== "done") throw new Error(`the test host said ${r.value}`); };

const chrome = await Chrome.start(exe);
let tab;
const fresh = async (hash = "#/") => {
  if (tab) await chrome.closeTab(tab);
  tab = await chrome.tab(`${base}/${hash}`);
  await tab.waitFor("the app to draw", `!!document.querySelector("header, .list, main")`);
  return tab;
};
const card = (name) => `[...document.querySelectorAll("a.card")].some(c => c.querySelector(".name")?.firstChild?.textContent === ${JSON.stringify(name)})`;
const clickCard = (name) => tab.eval(`[...document.querySelectorAll("a.card")].find(c => c.querySelector(".name")?.firstChild?.textContent === ${JSON.stringify(name)}).click()`);

const scenarios = {
  // The app trades the one-time code for a token, then lists what the CLI started.
  async list() {
    ptyro("new", "-d", "-n", "cli-shell", "--", "sh", "-c", "echo up; cat");
    await tab.waitFor("the CLI's session as a card", card("cli-shell"));
  },

  // ＋, a prompt, Start: a session in a new worktree, which then shows as a card.
  async newTask() {
    // (Projects arrive a moment after the first cards; a tap before they do offers "Add a project"
    // instead of the sheet. A person would tap again, so a fresh page does.)
    for (let tries = 0; ; tries++) {
      await sleep(700 * (tries + 1));
      await tab.click("button.icon.plus");
      try { await tab.waitFor("the sheet", `!!document.querySelector(".sheet-foot .primary")`, 2500); break; }
      catch (e) { if (tries >= 4) throw e; await fresh("#/"); await sleep(500); }
    }
    await tab.eval(`(() => { const t = document.querySelector("textarea"); t.value = "e2e task"; t.dispatchEvent(new Event("input", { bubbles: true })); })()`);
    await tab.click(".sheet-foot .primary");
    const made = await until("a worktree session", () => sessions().find((s) => s.name !== "cli-shell" && s.name !== "blocked1"));
    // (The app opens the new session; the list is a tap back.)
    await tab.waitFor("the new session to open", `location.hash.startsWith("#/s/")`);
    await tab.eval(`location.hash = "#/"`);
    await tab.waitFor("its card", card(made.name));
  },

  // ＋, "A command…", a command line, Start: the daemon runs it through its own shell (the
  // web sends the line, not an argv: WIN4), in a worktree that exists.
  async command() {
    for (let tries = 0; ; tries++) {
      await sleep(700 * (tries + 1));
      await tab.click("button.icon.plus");
      try { await tab.waitFor("the sheet", `!!document.querySelector(".sheet-foot .primary")`, 2500); break; }
      catch (e) { if (tries >= 4) throw e; await fresh("#/"); await sleep(500); }
    }
    const line = "echo e2e-$((6 * 7)); cat";
    await tab.eval(`(() => {
      const [agent, where] = document.querySelectorAll(".sheet select");
      agent.value = "@command"; agent.dispatchEvent(new Event("change", { bubbles: true }));
      const wt = [...where.options].find((o) => o.value.startsWith("w:"));
      if (!wt) throw new Error("no worktree to run it in");
      where.value = wt.value; where.dispatchEvent(new Event("change", { bubbles: true }));
      const cmd = document.querySelector(".sheet label.field:not([hidden]) input");
      cmd.value = ${JSON.stringify(line)}; cmd.dispatchEvent(new Event("input", { bubbles: true }));
    })()`);
    await tab.click(".sheet-foot .primary");
    const ran = await until("the command's session", () => sessions().find((s) => s.argv?.[s.argv.length - 1] === line));
    if (ran.kind !== "command" || ran.argv.length !== 3 || ran.argv[1] !== "-c") throw new Error(`ran as ${JSON.stringify(ran.argv)} (${ran.kind})`);
    await until("the shell's arithmetic in its output", () => ptyro("read", ran.name).includes("e2e-42"));
  },

  // An agent asks permission; Allow on the card answers it.
  async permission() {
    ptyro("new", "-d", "-n", "blocked1", "--agent", "claude");
    ptyro("send", "blocked1", "ask rm -rf build", "--wait-ready");
    await tab.waitFor("Allow on the blocked card", `!!document.querySelector(".blocked-wrap .actions .allow")`);
    await tab.click(".blocked-wrap .actions .allow");
    await until("the agent to see the answer", () => ptyro("read", "blocked1").includes("answer: yes"));
    await tab.waitFor("the Allow buttons to go", `!document.querySelector(".blocked-wrap .actions .allow")`);
  },

  // Home (SPEC §26.10): an agent that asks is the first row under "Needs you", with what it
  // asks; the headline counts it; the row unfolds into its trail (with what to do there) and
  // fits a phone; a tap goes to the session; answered, it leaves "Needs you" without a reload;
  // and a page opened with no address at all opens on home.
  async home() {
    ptyro("new", "-d", "-n", "homeask", "--agent", "claude");
    ptyro("send", "homeask", "ask rm -rf dist", "--wait-ready");
    await fresh("#/home");
    const asker = `[...document.querySelectorAll("main.home .home-sec.needs .home-row")].find((r) => r.textContent.includes("homeask"))`;
    await tab.waitFor("the asker under Needs you, with what it asks", `(() => { const r = ${asker}; return !!r && r.textContent.includes("answer") && r.textContent.includes("rm -rf dist"); })()`, 20_000);
    await tab.waitFor("the headline to count it", `/need(s)? you/.test(document.querySelector(".home-headline")?.textContent ?? "")`);
    if (!(await tab.eval(`document.querySelector("nav.tabs a.on")?.textContent`) === "Home")) throw new Error("Home isn't the tab shown");
    await tab.phone();
    await tab.eval(`${asker}.querySelector(".unfold").click()`);
    await tab.waitFor("its trail, with what to do there", `[...document.querySelectorAll(".home-row.open .home-trail .bar button")].some((b) => b.textContent === "New task")`);
    if (!(await tab.eval(`document.documentElement.scrollWidth <= innerWidth`))) throw new Error("home is wider than a phone");
    await tab.eval(`${asker}.querySelector(".home-head").click()`);
    await tab.waitFor("its session", `location.hash === "#/s/" + ${JSON.stringify(encodeURIComponent(find("homeask").id))}`);
    await tab.eval(`location.hash = "#/home"`);
    await tab.waitFor("home again", `!!${asker}`);
    ptyro("answer", "homeask", "deny");
    await tab.waitFor("it to leave Needs you by itself", `!${asker}`, 20_000);
    ptyro("rm", "--stop", "-y", "homeask");
    // No address at all: home (`[ui] home_at_start`, on unless switched off). `#/` is the sessions.
    await fresh("");
    await tab.waitFor("a bare address to open on home", `location.hash === "#/home" && !!document.querySelector("main.home")`, 15_000);
  },

  // The Library (KB1.9): a session's turn is kept and the page lists it under its day, another
  // one's shows as it is kept (no reload); the search finds it with the match marked; a tap
  // opens the reader at that piece, All of it reads the rest, the find box looks in it, Its
  // session goes to the session; and Forget…, asked in the daemon's words, deletes it for good.
  async library() {
    const recalled = () => JSON.parse(ptyro("recall", "--json", "zqlibrary"));
    const rowWith = (word) => `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes(${JSON.stringify(word)}))`;
    const type = (sel, text) => tab.eval(`(() => { const b = document.querySelector(${JSON.stringify(sel)}); b.value = ${JSON.stringify(text)}; b.dispatchEvent(new Event("input", { bubbles: true })); })()`);
    const press = (label) => tab.eval(`[...document.querySelectorAll("main.library button, .sheet button")].find((b) => b.textContent.startsWith(${JSON.stringify(label)})).click()`);
    ptyro("new", "-d", "-n", "lib1", "--agent", "claude");
    ptyro("send", "lib1", "edit lib.txt zqlibrary", "--wait-ready");
    await until("the turn to be kept", () => recalled().length > 0, 20_000);
    await fresh("#/library");
    // On a phone: nothing wider than the screen, in the list or in the reader.
    await tab.phone();
    const fits = async (what) => { if (!(await tab.eval(`document.documentElement.scrollWidth <= innerWidth`))) throw new Error(`${what} is wider than a phone`); };
    await tab.waitFor("the search box", `!!document.querySelector("input.search")`);
    await tab.waitFor("the history, under its day", `${rowWith("zqlibrary")} && document.querySelector("main.library h3.day")?.textContent === "Today"`);
    await tab.waitFor("the filters: the kinds that are kept", `[...(document.querySelector('.kb-filters select[aria-label="Kind"]')?.options ?? [])].map((o) => o.value).join() === ",session,chat" && document.querySelectorAll(".kb-filters select").length === 4`);
    // Kept while the page is open: shown without a reload (the daemon's `kb_updated`).
    ptyro("new", "-d", "-n", "lib2", "--agent", "claude");
    ptyro("send", "lib2", "edit other.txt zqsecond", "--wait-ready");
    await tab.waitFor("the second session's turn, as it is kept", rowWith("zqsecond"), 20_000);
    // A filter that keeps nothing says so; back to every kind.
    const kind = (v) => tab.eval(`(() => { const k = document.querySelector('.kb-filters select[aria-label="Kind"]'); k.value = ${JSON.stringify(v)}; k.dispatchEvent(new Event("change", { bubbles: true })); })()`);
    await kind("chat");
    await tab.waitFor("chats have no history here", `document.querySelector("main.library .rows")?.textContent.includes("Chats are on the Chats page")`);
    await kind("");
    await type("input.search", "zqlibrary");
    await tab.waitFor("a hit with the match marked", `[...document.querySelectorAll("main.library .rows .row mark")].some((m) => m.textContent === "zqlibrary")`);
    if (await tab.eval(rowWith("zqsecond"))) throw new Error("the search lists what doesn't match");
    await fits("the list");
    await tab.eval(`[...document.querySelectorAll("main.library .rows .row")].find((r) => r.querySelector("mark")).click()`);
    await tab.waitFor("the reader, at the piece that matched", `location.hash.startsWith("#/library/k/") && [...document.querySelectorAll("main.reader .kb-piece.at pre mark")].some((m) => m.textContent === "zqlibrary")`);
    // All of it, then find in it.
    if (await tab.eval(`[...document.querySelectorAll("main.reader button")].some((b) => b.textContent.startsWith("All of it"))`)) await press("All of it");
    await tab.waitFor("all of it", `document.querySelectorAll("main.reader .kb-piece").length >= 3 && ![...document.querySelectorAll("main.reader button")].some((b) => b.textContent.startsWith("All of it"))`);
    await type('input[aria-label="Find in it"]', "WROTE");
    await tab.waitFor("what was looked for, marked and counted", `/^1\\/\\d+$/.test(document.querySelector("main.reader .bar .sub")?.textContent ?? "") && document.querySelector("main.reader .kb-piece.at mark")?.textContent === "wrote"`);
    await fits("the reader");
    // Its session, and back to the reader.
    await press("Its session");
    await tab.waitFor("the session it happened in", `location.hash === "#/s/" + ${JSON.stringify(encodeURIComponent(find("lib1").id))}`);
    await tab.eval(`history.back()`);
    await tab.waitFor("the reader again", `!!document.querySelector("main.reader .kb-piece")`);
    // ⋯ → Forget…: the daemon says what will go, in the words every client uses.
    await tab.click(`header button[aria-label="More"]`);
    await tab.waitFor("the reader's menu", `[...document.querySelectorAll(".sheet .menu button")].some((b) => b.textContent.startsWith("Copy its address"))`);
    await press("Forget…");
    await tab.waitFor("the question, in the daemon's words", `document.querySelector(".sheet h2")?.textContent.startsWith("Forget 1 item: session") && [...document.querySelectorAll(".sheet p.confirm")].some((p) => p.textContent.startsWith("The text is deleted from the knowledge base for good"))`);
    await tab.eval(`[...document.querySelectorAll(".sheet-foot .deny")].find((b) => b.textContent === "Forget").click()`);
    await until("it to be forgotten", () => recalled().length === 0);
    await tab.waitFor("the list again, without it", `location.hash === "#/library" && !document.querySelector("main.library .rows .row mark") && document.querySelector("main.library .rows")?.textContent.includes("Nothing matches.")`);
    if (!ptyro("kb").includes("1 forgotten")) throw new Error(`the status doesn't count it: ${ptyro("kb")}`);
    // A session's ⋯ → History: the Library with only what was kept of it.
    await fresh(`#/s/${encodeURIComponent(find("lib2").id)}`);
    await tab.waitFor("the session's menu button", `!!document.querySelector('header button[aria-label="More"], header button.icon')`);
    await tab.eval(`[...document.querySelectorAll("header button")].find((b) => b.textContent === "⋯").click()`);
    await tab.waitFor("History in its menu", `[...document.querySelectorAll(".sheet .menu button")].some((b) => b.textContent.startsWith("History"))`);
    await press("History");
    await tab.waitFor("only that session's", `location.hash === "#/library" && ${rowWith("zqsecond")} && [...document.querySelectorAll(".kb-filters button")].some((b) => b.textContent.startsWith("Only session lib2"))`);
    ptyro("kill", "lib1");
    ptyro("kill", "lib2");
  },

  // The Library's notes (KB2): a note the command wrote is listed under what it is about; New
  // note (every option) writes one; a tap opens it to edit in the page, Save writes its file
  // and the change is found; ⋯ → Delete…, asked in the daemon's words, deletes the file.
  async notes() {
    const recalled = (word) => JSON.parse(ptyro("recall", "--json", "--here", word));
    const rowWith = (word) => `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes(${JSON.stringify(word)}))`;
    const press = (label) => tab.eval(`[...document.querySelectorAll("main.library button, .sheet button")].find((b) => b.textContent.startsWith(${JSON.stringify(label)})).click()`);
    const fill = (sel, text) => tab.eval(`(() => { const b = document.querySelector(${JSON.stringify(sel)}); b.value = ${JSON.stringify(text)}; b.dispatchEvent(new Event("input", { bubbles: true })); })()`);
    ptyro("kb", "note", "zqwebnote from the command", "--title", "Web one");
    await fresh("#/library");
    await tab.phone();
    const fits = async (what) => { if (!(await tab.eval(`document.documentElement.scrollWidth <= innerWidth`))) throw new Error(`${what} is wider than a phone`); };
    await tab.waitFor("the tabs", `document.querySelectorAll(".kb-tabs button").length === 3`);
    await press("Notes");
    await tab.waitFor("the note, under what it is about", `location.hash === "#/library/notes" && ${rowWith("Web one")} && document.querySelector("main.library h3.day")?.textContent === "Global (about no project)"`);
    await tab.waitFor("who wrote it", `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes("you ·"))`);
    await fits("the notes");
    // New note: every option the command takes, as a pick where the answers are known.
    await press("New note");
    await tab.waitFor("the form", `document.querySelector(".sheet h2")?.textContent === "New note" && document.querySelectorAll(".sheet select").length === 2 && !!document.querySelector(".sheet textarea")`);
    const labels = await tab.eval(`[...document.querySelectorAll(".sheet label.field > span")].map((s) => s.textContent).join()`);
    if (labels !== "Note,Title,About,Seen by,Tags") throw new Error(`the form's fields: ${labels}`);
    await fill(".sheet textarea", "zqformnote we chose the page because it is here");
    await tab.eval(`(() => { const [title, tags] = document.querySelectorAll(".sheet input:not([type=checkbox])"); title.value = "From the page"; tags.value = "web, decision"; })()`);
    await tab.click(".sheet-foot .primary");
    await until("the note to be kept", () => recalled("zqformnote").length > 0);
    const made = recalled("zqformnote")[0];
    if (made.kind !== "note" || made.writer !== "user" || made.title !== "From the page") throw new Error(`kept as ${JSON.stringify(made)}`);
    if (recalled("decision").length !== 1) throw new Error("its tags are not found");
    await tab.waitFor("it in the list", rowWith("From the page"));
    // A tap: its file, in the page. Save writes it, and the change is found.
    await tab.eval(`[...document.querySelectorAll("main.library .rows .row")].find((r) => r.textContent.includes("From the page")).click()`);
    await tab.waitFor("the note's file", `location.hash.startsWith("#/library/note/") && (document.querySelector("textarea.kb-note")?.value ?? "").includes("by: you") && document.querySelector("textarea.kb-note").value.includes("zqformnote")`);
    await fits("the note");
    const text = await tab.eval(`document.querySelector("textarea.kb-note").value`);
    await fill("textarea.kb-note", text.replace("zqformnote", "zqpageedit"));
    await press("Save");
    // The page says it saved and asks for the file again (for a moment it shows no note, and
    // no ⋯): what it then shows is the file as it was saved.
    await tab.waitFor("the page to say it saved, and show the note as it is now", `document.querySelector(".toast")?.textContent === "Saved" && !!document.querySelector('header button[aria-label="More"]') && (document.querySelector("textarea.kb-note")?.value ?? "").includes("zqpageedit")`);
    await until("the edit to be found", () => recalled("zqpageedit").length > 0);
    if (recalled("zqformnote").length) throw new Error("the old words are still found");
    // ⋯ → Delete…: the daemon says the file goes.
    await tab.click(`header button[aria-label="More"]`);
    await tab.waitFor("the note's menu", `[...document.querySelectorAll(".sheet .menu button")].some((b) => b.textContent.startsWith("Delete…"))`);
    await press("Delete…");
    await tab.waitFor("the question, in the daemon's words", `document.querySelector(".sheet h2")?.textContent.startsWith("Forget 1 item: note") && [...document.querySelectorAll(".sheet p.confirm")].some((p) => p.textContent.startsWith("A note's file is deleted"))`);
    await tab.eval(`[...document.querySelectorAll(".sheet-foot .deny")].find((b) => b.textContent === "Forget").click()`);
    await until("it to be gone", () => recalled("zqpageedit").length === 0);
    await tab.waitFor("the notes again, without it", `location.hash === "#/library/notes" && ${rowWith("Web one")} && !${rowWith("From the page")}`);
    // Who wrote it, as a filter of the search too.
    await press("Search");
    await tab.waitFor("the search's filters", `location.hash === "#/library" && !!document.querySelector('.kb-filters select[aria-label="Wrote"]')`);
    ptyro("kb", "forget", recalled("zqwebnote")[0].addr.split("/")[0], "--yes");
  },

  // The Library's sources (KB3.7): the project is read by itself and listed; Read a folder…
  // (every option the command takes) reads one, and what it holds is found; a tap shows it in
  // full, Look at it again reads what changed; Stop reading it…, asked in the daemon's words,
  // drops it; Agents' history… counts before it reads.
  async sources() {
    const recalled = (word) => JSON.parse(ptyro("recall", "--json", "--here", word));
    const rowWith = (word) => `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes(${JSON.stringify(word)}))`;
    const press = (label) => tab.eval(`[...document.querySelectorAll("main.library button, .sheet button")].find((b) => b.textContent.startsWith(${JSON.stringify(label)})).click()`);
    const tapRow = (word) => tab.eval(`[...document.querySelectorAll("main.library .rows .row")].find((r) => r.textContent.includes(${JSON.stringify(word)})).click()`);
    const labels = () => tab.eval(`[...document.querySelectorAll(".sheet label.field > span")].map((s) => s.textContent).join()`);
    const book = join(E.repo, "..", "handbook");
    mkdirSync(book, { recursive: true });
    writeFileSync(join(book, "guide.md"), "# Belt tension\n\nzqsourceword: tighten the belt a quarter turn.\n");
    await fresh("#/library");
    await tab.phone();
    const fits = async (what) => { if (!(await tab.eval(`document.documentElement.scrollWidth <= innerWidth`))) throw new Error(`${what} is wider than a phone`); };
    await tab.waitFor("the tabs", `document.querySelectorAll(".kb-tabs button").length === 3`);
    await press("Sources");
    // A project is read by itself: nothing was asked for it.
    await tab.waitFor("the project, as a root", `location.hash === "#/library/sources" && [...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes("project") && r.textContent.includes("repo"))`, 20_000);
    await fits("the sources");
    // What is kept of shells (KB4): the commands, as it comes; what they print only once the
    // page has said what that stores, and switched back with one press.
    const shellsSay = (text) => `!!document.querySelector("main.library .kb-shells")?.textContent.includes(${JSON.stringify(text)})`;
    const shellNow = () => JSON.parse(ptyro("kb", "status", "--json")).shell;
    await tab.waitFor("what is kept of shells", shellsSay("Shell commands are kept, not what they print."));
    await press("Keep what they print too…");
    await tab.waitFor("what that stores, asked first", `document.querySelector(".sheet h2")?.textContent === "Keep what your commands print?" && !!document.querySelector(".sheet p.confirm")?.textContent.includes("the first 2 KiB and the last 6 KiB")`);
    if (shellNow() !== "commands") throw new Error("kb.shell was switched before the question was answered");
    await fits("the question about what commands print");
    await tab.eval(`[...document.querySelectorAll(".sheet-foot .deny")].find((b) => b.textContent === "Keep it").click()`);
    await tab.waitFor("what they print, kept", shellsSay("Shell commands are kept, and what they print."), 20_000);
    await until("the daemon to keep what commands print", () => shellNow() === "output", 20_000);
    await press("Stop keeping what they print");
    await tab.waitFor("commands only again", shellsSay("Shell commands are kept, not what they print."), 20_000);
    await until("the daemon to keep commands only", () => shellNow() === "commands", 20_000);
    await fits("the sources, with the shells' switches");
    // Read a folder…: every option the command takes, as a pick where the answers are known.
    await press("Read a folder…");
    await tab.waitFor("the form", `document.querySelector(".sheet h2")?.textContent === "Read a folder" && document.querySelectorAll(".sheet select").length === 2`);
    const fields = await labels();
    if (fields !== "Folder or file,Name,Project,Seen by,Only,Not,Source comments,Source code too (it makes the knowledge base large),Commit messages,Only now: never look at it again") throw new Error(`the form's fields: ${fields}`);
    await fits("the form");
    await tab.eval(`(() => { const [path, name] = document.querySelectorAll(".sheet input:not([type=checkbox])"); path.value = ${JSON.stringify(book)}; name.value = "handbook"; })()`);
    await tab.click(".sheet-foot .primary");
    await until("the folder's document to be found", () => recalled("zqsourceword").length > 0, 20_000);
    const hit = recalled("zqsourceword")[0];
    if (hit.kind !== "doc" || !String(hit.title).includes("guide.md")) throw new Error(`found as ${JSON.stringify(hit)}`);
    // Its row, without a reload: what it holds, and that it was read.
    await tab.waitFor("the folder's row, read", `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes("handbook") && r.textContent.includes("1 file") && /read [0-9]+[smh] ago/.test(r.textContent))`, 20_000);
    await fits("the sources, with a folder");
    // A tap: all of it, and Look at it again, which finds what changed.
    await tapRow("handbook");
    await tab.waitFor("the root, in full", `document.querySelector(".sheet h2")?.textContent === "handbook" && document.querySelector(".sheet .facts")?.textContent.includes("A folder you gave") && document.querySelector(".sheet .facts").textContent.includes(${JSON.stringify(book)})`);
    await fits("a root's sheet");
    writeFileSync(join(book, "more.md"), "# More\n\nzqsecondsource: oil the hopper.\n");
    await press("Look at it again");
    await until("the new file to be found", () => recalled("zqsecondsource").length > 0, 20_000);
    await tab.waitFor("two files in its row", `[...document.querySelectorAll("main.library .rows .row")].some((r) => r.textContent.includes("handbook") && r.textContent.includes("2 files"))`, 20_000);
    // The project's own root is not removed here; the folder's is, asked in the daemon's words.
    await tapRow("repo");
    await tab.waitFor("the project's sheet", `document.querySelector(".sheet h2")?.textContent === "repo" && ![...document.querySelectorAll(".sheet button")].some((b) => b.textContent.startsWith("Stop reading it"))`);
    await tab.click(`.sheet button[aria-label="Close"]`);
    await tapRow("handbook");
    await tab.waitFor("the folder's sheet", `[...document.querySelectorAll(".sheet button")].some((b) => b.textContent.startsWith("Stop reading it"))`);
    await press("Stop reading it…");
    await tab.waitFor("the question, in the daemon's words", `document.querySelector(".sheet h2")?.textContent.startsWith("Stop reading handbook") && [...document.querySelectorAll(".sheet p.confirm")].some((p) => p.textContent.startsWith("Its files and its repository aren't touched"))`);
    await tab.eval(`[...document.querySelectorAll(".sheet-foot .deny")].find((b) => b.textContent === "Stop reading it").click()`);
    await until("what was kept of it to go", () => recalled("zqsourceword").length === 0, 20_000);
    await tab.waitFor("the sources, without it", `!${rowWith("handbook")} && ${rowWith("repo")}`, 20_000);
    if (!existsSync(join(book, "guide.md"))) throw new Error("the folder's own files were touched");
    // Agents' history…: every option, and the count before anything is read.
    await press("Agents' history…");
    await tab.waitFor("its form", `document.querySelector(".sheet h2")?.textContent === "Agents' history" && document.querySelectorAll(".sheet select").length === 3`);
    const asked = await labels();
    if (asked !== "Whose,Profile,Since,Only count: read nothing") throw new Error(`the history's fields: ${asked}`);
    await tab.eval(`document.querySelector(".sheet input[type=checkbox]").click()`);
    await tab.click(".sheet-foot .primary");
    await tab.waitFor("the count", `/conversations? in your agents' folders/i.test(document.body.innerText)`);
    rmSync(book, { recursive: true, force: true });
  },

  // A switch writes the config file (comments kept), Undo restores it, a bad number is refused
  // in the form and never reaches the file.
  async settings() {
    const before = config();
    await fresh("#/settings/interface");
    const sw = `[data-key="ui.clock"] .switch input`;
    await tab.waitFor("the clock switch", `!!document.querySelector(${JSON.stringify(sw)})`);
    const was = await tab.eval(`document.querySelector(${JSON.stringify(sw)}).checked`);
    // (The panel repaints once its data arrives, and a click during that repaint is lost: click
    // again only while the switch still shows the old value, so it never flips twice.)
    await until("the config to change", async () => {
      if (config() !== before) return true;
      if (await tab.eval(`document.querySelector(${JSON.stringify(sw)})?.checked`) === was) {
        await tab.click(`[data-key="ui.clock"] .switch`);
      }
      await sleep(700);
      return config() !== before;
    }, 20_000, 0);
    const after = config();
    if (!after.includes(`clock = ${!was}`)) throw new Error(`clock = ${!was} not written:\n${after}`);
    if (!after.includes("# e2e: keep this comment")) throw new Error(`a comment was lost:\n${after}`);
    await tab.waitFor("the Undo snackbar", `!!document.querySelector(".snack button")`);
    await tab.click(".snack button");
    await until("Undo to restore the file", () => config() === before);

    const num = `[data-key="ui.sidebar_width"] input`;
    // (The panel repaints after an Undo, which would drop a value typed meanwhile: type until it shows.)
    await until("the range error", async () => {
      await tab.eval(`(() => { const i = document.querySelector(${JSON.stringify(num)}); i.value = "3"; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
      await sleep(500);
      return tab.eval(`document.querySelector("[data-key='ui.sidebar_width'] .form-err")?.textContent.includes("outside 12 to 120")`);
    }, 30_000, 0);
    if (config() !== before) throw new Error("a refused value reached the file");
    await tab.eval(`(() => { const i = document.querySelector(${JSON.stringify(num)}); i.value = "44"; i.dispatchEvent(new Event("input", { bubbles: true })); i.dispatchEvent(new Event("change", { bubbles: true })); })()`);
    await until("sidebar_width = 44", () => /sidebar_width\s*=\s*44/.test(config()));
  },

  // Real key events into the terminal reach the shell.
  async terminal() {
    const s = find("cli-shell");
    await fresh(`#/s/${s.id}`);
    await tab.waitFor("the terminal", `!!document.querySelector("textarea.term-input")`);
    await tab.eval(`document.querySelector("textarea.term-input").focus()`);
    await sleep(300);
    await tab.type("hello-e2e");
    await tab.enter();
    await until("the line to arrive", () => ptyro("read", "cli-shell").split("\n").some((l) => l.trim() === "hello-e2e"));
  },

  // A phone's keyboard (WEB2.1): a word's letters arrive while it is still composed, an
  // autocorrect arrives as a correction, and dictation (a many-word composition, a big insert
  // with a newline in it) is held, shown, and arrives once as text, its newline pressing nothing.
  // Real IME operations through the DevTools protocol, into `cat` on a real tty (which echoes
  // the line as it is edited, and prints it again only when Enter is pressed).
  async typing() {
    ptyro("new", "-d", "-n", "typing", "--", "sh", "-c", "cat");
    const s = await until("the typing session", () => find("typing"));
    await fresh(`#/s/${s.id}`);
    await tab.eval(`localStorage.setItem("ptyro.debug", "1")`);
    await tab.phone();
    await tab.send("Page.reload");
    // `E2E_THROTTLE=6`: a slow machine (CI), to reproduce timing bugs here.
    if (process.env.E2E_THROTTLE) await tab.send("Emulation.setCPUThrottlingRate", { rate: Number(process.env.E2E_THROTTLE) });
    await tab.waitFor("the terminal", `!!window.ptyroTerm && document.querySelector("canvas.term")?.width > 0`);
    if (!(await tab.eval(`matchMedia("(pointer: coarse)").matches`))) throw new Error("the phone emulation has no coarse pointer: the page would take this for a computer");
    await tab.eval(`document.querySelector(".term-input").focus()`);
    await sleep(300);
    const line = () => ptyro("read", "typing").split("\n").map((l) => l.trimEnd()).filter(Boolean);
    const last = () => line().at(-1) ?? "";
    const ime = (text) => tab.send("Input.imeSetComposition", { text, selectionStart: [...text].length, selectionEnd: [...text].length });
    for (const t of ["h", "he", "hel"]) await ime(t);
    await until("the letters while the word is still composed", () => last() === "hel", 10_000);
    if (!(await tab.eval(`ptyroTerm.typist.composing`))) throw new Error("the composition ended early");
    await ime("help");
    await ime("helpp");
    await ime("help");
    await until("the correction", () => last() === "help", 10_000);
    await tab.send("Input.insertText", { text: "help" });
    await tab.send("Input.insertText", { text: " " });
    await until("the space", () => last() === "help", 10_000);
    // Dictation as a composition: held and shown, nothing sent while it is spoken.
    await ime("one");
    await ime("one two");
    await ime("one two three");
    await tab.waitFor("the dictation preview", `!document.querySelector(".term-pending").hidden && document.querySelector(".term-pending").textContent.includes("two three")`, 5000);
    await sleep(400);
    const mid = last();
    if (mid !== "help one") throw new Error(`dictation was streamed (only its first word should have gone): ${JSON.stringify(mid)}`);
    await tab.send("Input.insertText", { text: "one two three" });
    await until("the dictation, once", () => last() === "help one two three", 10_000);
    if (!(await tab.eval(`document.querySelector(".term-pending").hidden`))) throw new Error("the preview stayed after the text was sent");
    // A keyboard extension's insert (Superwhisper's keyboard, a clipboard chip): one paste once it
    // settles; without bracketed paste, its newline is a space and presses nothing.
    await tab.send("Input.insertText", { text: " four five\nsix" });
    await until("the inserted text", () => last() === "help one two three four five six", 10_000);
    const before = line().filter((l) => l.includes("four five six")).length;
    if (before !== 1) throw new Error(`a dictated newline pressed Enter: ${JSON.stringify(line().slice(-4))}`);
    await tab.enter();
    await until("cat to print the line when Enter is pressed", () => line().filter((l) => l === "help one two three four five six").length === 2, 10_000);
    if (await tab.eval(`document.querySelector(".term-input").value`) !== "") throw new Error("text was left in the field");
    // Plain keys: a password field (no predictions, no mic), kept for this device, typing still works.
    await tab.eval(`ptyroTerm.setPlain(true)`);
    if (!(await tab.eval(`document.querySelector(".term-input").type === "password" && document.activeElement === document.querySelector(".term-input") && localStorage.getItem("ptyro.term.plain") === "1"`))) throw new Error("plain keys didn't switch the field");
    await tab.send("Input.insertText", { text: "p" });
    await tab.type("lain");
    await until("plain keys' letters", () => last() === "plain", 10_000);
    await tab.eval(`ptyroTerm.setPlain(false)`);
    if (!(await tab.eval(`document.querySelector(".term-input").tagName === "TEXTAREA" && localStorage.getItem("ptyro.term.plain") === "0"`))) throw new Error("plain keys didn't switch back");
    await tab.eval(`localStorage.removeItem("ptyro.debug")`);
    ptyro("kill", "typing");
  },

  // Terminal first (WEB2.2), on a computer: the session page opens with the terminal taking the
  // keys (no click), a key pressed while nothing has the focus goes to it (the first letter too),
  // ✎ compose sends a multi-line prompt once and gives the keys back, Esc keeps a draft, and an
  // ended session offers nothing to type with and says why.
  async terminalFirst() {
    ptyro("new", "-d", "-n", "first", "--", "sh", "-c", "cat");
    const s = await until("the session", () => find("first"));
    await fresh(`#/s/${s.id}`);
    const lines = () => ptyro("read", "first").split("\n").map((l) => l.trimEnd()).filter(Boolean);
    // (A tty echoes a line as it is typed and `cat` prints it again at Enter: twice is once.)
    const twice = (t) => () => lines().filter((l) => l === t).length === 2;
    const focused = `document.activeElement === document.querySelector(".term-input")`;
    if (await tab.eval(`matchMedia("(pointer: coarse)").matches`)) throw new Error("the desktop tab has a coarse pointer");
    await tab.waitFor("the terminal to take the keys by itself", focused, 10_000);
    if (await tab.eval(`!!document.querySelector(".compose")`)) throw new Error("the compose box is open by default");
    await tab.type("first-keys");
    await tab.enter();
    await until("keys typed with no click", twice("first-keys"), 10_000);
    await tab.eval(`document.activeElement.blur()`);
    await tab.waitFor("the terminal to let the focus go", `!(${focused}) && document.activeElement === document.body`);
    await tab.type("stray");
    await tab.enter();
    await until("keys typed on the page, the first one too", twice("stray"), 10_000);
    if (!(await tab.eval(focused))) throw new Error("the terminal didn't keep the focus it took");

    // ✎ compose: a multi-line prompt, read over, sent once with Enter (Shift+Enter is a new line).
    await tab.click(".term-more .compose-btn");
    await tab.waitFor("the compose box, focused", `document.activeElement === document.querySelector(".compose textarea")`);
    await tab.send("Input.insertText", { text: "line one" });
    const shiftEnter = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, modifiers: 8 };
    await tab.send("Input.dispatchKeyEvent", { type: "keyDown", ...shiftEnter, text: "\r" });
    await tab.send("Input.dispatchKeyEvent", { type: "keyUp", ...shiftEnter });
    await tab.send("Input.insertText", { text: "line two" });
    const v = await tab.eval(`document.querySelector(".compose textarea").value`);
    if (v !== "line one\nline two") throw new Error(`the compose box has ${JSON.stringify(v)}`);
    await sleep(300);
    if (lines().some((l) => l.includes("line one"))) throw new Error("the compose box typed into the session before it was sent");
    await tab.enter();
    await tab.enter(); // a second Enter on the emptied box sends nothing
    // (cat's copy of the first line may land in the middle of the tty's echo of the second.)
    const times = (t) => ptyro("read", "first").split(t).length - 1;
    await until("the prompt's two lines", () => times("line one") === 2 && times("line two") === 2, 10_000);
    await tab.waitFor("compose to close and the terminal to have the keys again", `!document.querySelector(".compose") && ${focused}`);
    await sleep(700);
    if (times("line one") !== 2 || times("line two") !== 2) throw new Error(`the prompt went more than once: ${JSON.stringify(lines().slice(-8))}`);

    // Esc closes it with the draft kept (marked on the button), and the terminal has the keys.
    await tab.click(".term-more .compose-btn");
    await tab.waitFor("the compose box, again", `document.activeElement === document.querySelector(".compose textarea")`);
    if (await tab.eval(`document.querySelector(".compose textarea").value`) !== "") throw new Error("a sent prompt stayed as a draft");
    await tab.send("Input.insertText", { text: "kept draft" });
    await tab.key("Escape", { code: "Escape", vk: 27 });
    await tab.waitFor("Esc to close it, marked as a draft", `!document.querySelector(".compose") && !!document.querySelector(".compose-btn .dot") && ${focused}`);
    await tab.click(".term-more .compose-btn");
    await tab.waitFor("the draft back", `document.querySelector(".compose textarea")?.value === "kept draft"`);
    if (lines().some((l) => l.includes("kept draft"))) throw new Error("a draft was sent");
    await tab.key("Escape", { code: "Escape", vk: 27 });
    await tab.waitFor("the terminal to have the keys", `!document.querySelector(".compose") && ${focused}`);

    // A click on the page while the terminal has the keys isn't lost to the focus moving (⋯ opens
    // its menu at the first click), and once the menu is closed, keys go to the terminal again.
    await tab.click(`header button[aria-label="More"]`);
    await tab.waitFor("the session's menu at the first click", `!!document.querySelector(".menu button")`, 5000);
    await tab.key("Escape", { code: "Escape", vk: 27 });
    await tab.waitFor("the menu to close", `!document.querySelector(".scrim")`);
    await tab.type("after-menu");
    await tab.enter();
    await until("keys after the menu", twice("after-menu"), 10_000);

    // Ended: nothing to type with, and the page says why.
    ptyro("kill", "first");
    await tab.waitFor("an ended session to offer no typing", `!document.querySelector(".term-more .type, .term-more .compose-btn, .compose, .term-keys") && /ended/.test(document.querySelector(".term-more .sub")?.textContent ?? "")`, 15_000);
  },

  // Terminal first on a phone: the page never takes the focus by itself (that would pop the
  // keyboard up unasked), compose takes no room, and ⌨ Type is the way in (⌨ Done the way out).
  async typePhone() {
    ptyro("new", "-d", "-n", "phone-type", "--", "sh", "-c", "cat");
    const s = await until("the session", () => find("phone-type"));
    await fresh(`#/s/${s.id}`);
    await tab.phone();
    await tab.send("Page.reload");
    await tab.waitFor("the terminal and ⌨ Type", `document.querySelector("canvas.term")?.width > 0 && !!document.querySelector(".term-more .type")`);
    if (!(await tab.eval(`matchMedia("(pointer: coarse)").matches`))) throw new Error("the phone emulation has no coarse pointer");
    const focused = `document.activeElement === document.querySelector(".term-input")`;
    await sleep(1000);
    if (await tab.eval(focused)) throw new Error("the terminal took the focus on a phone: its keyboard would pop up unasked");
    if (await tab.eval(`!!document.querySelector(".compose")`)) throw new Error("the compose box takes room on a phone");
    const tap = async (sel) => {
      const p = await tab.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.scrollIntoView({ block: "center" }); const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`);
      await tab.fingers([[p], [p]]);
    };
    await tap(".term-more .type");
    await tab.waitFor("⌨ Type to give the terminal the keys", `${focused} && document.querySelector(".term-more .type").textContent.includes("Done")`);
    await tab.send("Input.insertText", { text: "tapped" });
    await tab.enter();
    await until("what was typed after ⌨ Type", () => ptyro("read", "phone-type").split("\n").filter((l) => l.trimEnd() === "tapped").length === 2, 10_000);
    await tap(".term-more .type");
    await tab.waitFor("⌨ Done to put the keyboard away", `!(${focused}) && document.querySelector(".term-more .type").textContent.includes("Type")`);
    ptyro("kill", "phone-type");
  },

  // On a phone: two fingers zoom the terminal (and never the page), two fingers tapped switch
  // between fitted and full size, one finger sideways pans, and the choice is kept.
  async zoom() {
    const s = find("cli-shell");
    await fresh(`#/s/${s.id}`);
    await tab.eval(`(() => { for (const k of Object.keys(localStorage)) if (k.startsWith("ptyro.term.")) localStorage.removeItem(k); localStorage.setItem("ptyro.debug", "1"); })()`);
    await tab.phone();
    await tab.send("Page.reload");
    await tab.waitFor("the terminal", `!!window.ptyroTerm && document.querySelector("canvas.term")?.width > 0`);
    await sleep(300);
    const state = () => tab.eval(`(() => { const t = ptyroTerm, w = t.wrap; return { zoom: t.zoom, fit: t.fit, px: t.geo.px, size: t.size, left: w.scrollLeft, scale: visualViewport.scale, kept: localStorage.getItem("ptyro.term.zoom") }; })()`);
    const box = await tab.eval(`(() => { const r = ptyroTerm.wrap.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + Math.min(r.height / 2, 120) }; })()`);
    const pinch = (from, to, n = 10) => tab.fingers(Array.from({ length: n + 1 }, (_, i) => {
      const d = from + ((to - from) * i) / n;
      return [[box.x - d / 2, box.y], [box.x + d / 2, box.y]];
    }));
    const first = await state();
    if (first.zoom !== "auto") throw new Error(`a new device starts automatic: ${JSON.stringify(first)}`);
    if (!first.fit && first.px < 9) throw new Error(`automatic chose unreadable text: ${JSON.stringify(first)}`);

    await pinch(40, 300);
    let st = await state();
    if (st.zoom !== "size" || !(st.px > first.px) || st.kept !== "size") throw new Error(`pinching out didn't zoom in: ${JSON.stringify(first)} → ${JSON.stringify(st)}`);
    if (st.scale !== 1) throw new Error(`the page zoomed, not the terminal (scale ${st.scale})`);
    const big = st;

    // One finger sideways pans the bigger screen.
    await tab.fingers(Array.from({ length: 9 }, (_, i) => [[box.x + 60 - i * 15, box.y]]));
    await sleep(600);
    st = await state();
    if (!(st.left > big.left)) throw new Error(`a sideways drag didn't pan: ${big.left} → ${st.left}`);
    // Up and down on a screen taller than its box moves the box first (not the history).
    const top0 = await tab.eval(`ptyroTerm.wrap.scrollTop`);
    await tab.fingers(Array.from({ length: 9 }, (_, i) => [[box.x, box.y + 80 - i * 15]]));
    await sleep(600);
    const top1 = await tab.eval(`({ top: ptyroTerm.wrap.scrollTop, back: !!ptyroTerm.doc })`);
    if (!(top1.top > top0) || top1.back) throw new Error(`an upward drag didn't scroll the tall screen: ${top0} → ${JSON.stringify(top1)}`);

    await pinch(300, 30);
    st = await state();
    if (st.zoom !== "fit" || !st.fit || st.kept !== "fit") throw new Error(`pinching all the way in didn't fit the width: ${JSON.stringify(st)}`);

    // Two fingers tapped: fitted ⇄ full size.
    await tab.fingers([[[box.x - 30, box.y], [box.x + 30, box.y]]], 60);
    st = await state();
    if (st.zoom !== "size") throw new Error(`a two-finger tap didn't go to full size: ${JSON.stringify(st)}`);
    await tab.fingers([[[box.x - 30, box.y], [box.x + 30, box.y]]], 60);
    st = await state();
    if (st.zoom !== "fit") throw new Error(`a second two-finger tap didn't fit again: ${JSON.stringify(st)}`);

    // Kept for next time.
    await tab.send("Page.reload");
    await tab.waitFor("the terminal again", `!!window.ptyroTerm && ptyroTerm.zoom === "fit"`);
    if ((await state()).scale !== 1) throw new Error("the page is zoomed after all that");
    await tab.eval(`localStorage.removeItem("ptyro.debug")`);
  },

  // The server goes away: the app says so, then comes back by itself and still works.
  // Every key exactly once (WEB2.3): the server goes away mid-typing; keys typed meanwhile wait,
  // shown as waiting, and land once when it is back. Then the session changes while the server
  // is away (a command starts in its shell): the keys typed meanwhile aren't sent, the page says
  // why, shows them, and "Type them now" types them on purpose.
  async exactlyOnce() {
    ptyro("new", "-d", "-n", "once", "--", "env", "PS1=$ ", "sh");
    const s = await until("the shell", () => find("once"));
    await fresh(`#/s/${s.id}`);
    const lines = () => ptyro("read", "once").split("\n").map((l) => l.trimEnd());
    const has = (t) => lines().filter((l) => l.includes(t)).length;
    const focused = `document.activeElement === document.querySelector(".term-input")`;
    await tab.waitFor("the terminal to take the keys", focused, 10_000);
    await until("the shell's prompt", () => lines().some((l) => /[$#]$/.test(l)), 10_000);
    await tab.type("echo on");
    await until("keys typed live", () => has("echo on") === 1, 10_000);
    await hostOp("down");
    await tab.waitFor("the page to know it is offline", `!!document.querySelector(".offline")`, 30_000);
    await tab.type("e-");
    await tab.enter();
    await tab.waitFor("the keys shown as waiting", `!document.querySelector(".term-wait").hidden && /3 keys waiting/.test(document.querySelector(".term-wait").textContent)`, 5000);
    await hostOp("up");
    // ("echo on", then "e-⏎" while away: `echo one-`, echoed once and printed once.)
    await until("the offline keys, typed once", () => has("one-") === 2, 20_000);
    await tab.waitFor("nothing left waiting", `document.querySelector(".term-wait").hidden && document.querySelector(".term-lost").hidden`, 10_000);
    await sleep(800);
    if (has("echo one-") !== 1 || has("one-") !== 2) throw new Error(`keys typed more than once: ${JSON.stringify(lines().filter(Boolean).slice(-6))}`);

    // The session changes while the server is away: what is typed meanwhile isn't sent.
    await hostOp("down");
    await tab.waitFor("offline again", `!!document.querySelector(".offline")`, 30_000);
    ptyro("send", "once", "sleep 2");
    await sleep(300);
    await tab.type("echo late");
    await tab.enter();
    await hostOp("up");
    await tab.waitFor("the page to say the keys weren't sent, and why", `!document.querySelector(".term-lost").hidden && /10 keys weren't sent: the session changed/.test(document.querySelector(".term-lost").textContent)`, 20_000);
    await sleep(2500); // sleep 2 is over
    if (has("echo late")) throw new Error(`refused keys were typed: ${JSON.stringify(lines().filter(Boolean).slice(-6))}`);
    await tab.eval(`[...document.querySelectorAll(".term-lost button")].find(b => b.textContent === "Show").click()`);
    // Hidden at first (they may be a password typed where nothing echoes); Reveal shows them.
    await tab.waitFor("the keys, hidden", `document.querySelector("pre.lost-keys")?.textContent === ${JSON.stringify("••••••••••\n")}`);
    await tab.eval(`[...document.querySelectorAll(".sheet button")].find(b => b.textContent === "Reveal").click()`);
    await tab.waitFor("the keys, readable", `document.querySelector("pre.lost-keys")?.textContent === ${JSON.stringify("echo late⏎\n")}`);
    await tab.eval(`[...document.querySelectorAll(".sheet button")].find(b => b.textContent === "Type them now").click()`);
    await until("the keys typed on purpose", () => has("late") >= 2 && lines().some((l) => l.trim() === "late"), 10_000);
    await tab.waitFor("the notice gone", `document.querySelector(".term-lost").hidden && !document.querySelector(".sheet")`);
    ptyro("kill", "once");
  },

  // SHELLS' Alt+1…9 (SPEC §23.6), as in the TUI: typing in shell 1's terminal, Alt+2 (a Mac's
  // Option+2, which says ™: the physical key decides) shows shell 2, whose terminal then has the
  // keys; Alt+1 (as Windows sends it) goes back. Neither program ever saw an Alt digit.
  async altDigits() {
    for (const n of [1, 2]) ptyro("shells", "new", "-d", "--slot", String(n), "-n", `alt-${n}`, "--", "sh", "-c", "cat -v");
    const one = await until("shell 1", () => find("alt-1"));
    const two = await until("shell 2", () => find("alt-2"));
    const read = (name) => ptyro("read", name).split("\n").map((l) => l.trimEnd()).filter(Boolean);
    // (The tty echoes a line as it is typed and `cat -v` prints it again at Enter: twice is once.
    // An Alt digit that reached it would show as ^[2, ™ as M-bM-^DM-" or ™.)
    const twice = (name, t) => () => read(name).filter((l) => l === t).length === 2;
    const focused = `document.activeElement === document.querySelector(".term-input")`;
    const alt = async (key, code, vk) => {
      const k = { key, code, windowsVirtualKeyCode: vk, modifiers: 1 };
      await tab.send("Input.dispatchKeyEvent", { type: "keyDown", ...k, text: key });
      await tab.send("Input.dispatchKeyEvent", { type: "keyUp", ...k });
    };
    await fresh(`#/s/${one.id}`);
    await tab.waitFor("shell 1's terminal to take the keys", focused, 10_000);
    await tab.type("in-one");
    await tab.enter();
    await until("keys typed in shell 1", twice("alt-1", "in-one"), 10_000);
    await alt("™", "Digit2", 50);
    await tab.waitFor("Alt+2 to show shell 2", `location.hash === ${JSON.stringify(`#/s/${two.id}`)}`);
    await tab.type("in-two");
    await tab.enter();
    // (Typed at once: not one key may still go to shell 1, the page left.)
    try { await until("keys typed in shell 2", twice("alt-2", "in-two"), 10_000); } catch (e) { throw new Error(`${e.message}: shell 1 has ${JSON.stringify(read("alt-1"))}, shell 2 ${JSON.stringify(read("alt-2"))}`); }
    await alt("1", "Digit1", 49);
    await tab.waitFor("Alt+1 to show shell 1 again", `location.hash === ${JSON.stringify(`#/s/${one.id}`)}`);
    await sleep(500);
    for (const [name, typed] of [["alt-1", "in-one"], ["alt-2", "in-two"]]) {
      const got = read(name);
      if (got.join("\n") !== `${typed}\n${typed}`) throw new Error(`${name}'s program saw more than was typed: ${JSON.stringify(got)}`);
    }
    for (const n of [1, 2]) ptyro("kill", `alt-${n}`);
  },

  // Predicted echo (WEB2.4), in a real page against a real shell on a real tty. Here the echo is
  // quicker than any guess, so auto never draws one (checked); `force` then stands in for a slow
  // link. Each key and what is drawn after it are read in one synchronous step, before any frame
  // can arrive. A prompt that doesn't echo (`stty -echo; read`) never shows a letter, and every
  // key still reaches the session exactly as typed.
  async predict() {
    ptyro("new", "-d", "-n", "predict", "--", "env", "PS1=$ ", "sh");
    const s = await until("the shell", () => find("predict"));
    await fresh(`#/s/${s.id}`);
    await tab.eval(`localStorage.setItem("ptyro.debug", "1")`);
    await tab.send("Page.reload");
    await tab.waitFor("the terminal", `!!window.ptyroTerm && document.querySelector("canvas.term")?.width > 0`);
    // Connected again after the reload and the screen drawn: until then a key is held (the
    // predictor rightly guesses nothing while offline), so the round trip couldn't be measured.
    await tab.waitFor("the page connected, its screen drawn", `ptyroTerm.host.outbox().online && !!ptyroTerm.host.screen() && ptyroTerm.host.canType()`, 15_000);
    const lines = () => ptyro("read", "predict").split("\n").map((l) => l.trimEnd()).filter(Boolean);
    const last = () => lines().at(-1) ?? "";
    await until("the prompt", () => last() === "$", 10_000);
    const key = (c) => tab.eval(`(() => {
      const t = ptyroTerm;
      t.key(${JSON.stringify({ code: { t: "char", c }, mods: 0 })}, false);
      const o = t.predict.overlay();
      return { cells: o.cells.map((x) => x.t).join(""), cursor: o.cursor, slow: t.predict.slow, srtt: t.predict.srtt };
    })()`);
    const settled = () => tab.waitFor("the predictions confirmed", `ptyroTerm.predict.preds.length === 0`, 5000);
    // The cursor trusted again: anything sent before (the terminal taking focus sends a focus
    // event) is a barrier until the daemon said it typed it, plus a moment. Letters typed before
    // that are held and placed late, never measured, so the test waits as the predictor does.
    const trusted = () => tab.waitFor("the predictor to trust the cursor", `(() => { const p = ptyroTerm.predict; return p.waitN === null && p.io.now() >= p.trustAt && p.held.length === 0; })()`, 10_000);
    for (const c of "ec") { await trusted(); await key(c); await settled(); await sleep(150); }
    await trusted();
    const auto = await key("h");
    if (auto.srtt === null) throw new Error(`the echo's round trip wasn't measured: ${JSON.stringify(await tab.eval(`(() => { const p = ptyroTerm.predict; return { now: p.io.now(), trustAt: p.trustAt, waitN: p.waitN, upto: p.upto, held: p.held, preds: p.preds, stats: p.stats, mode: p.mode, slow: p.slow, epoch: p.epoch, proof: p.proof, outbox: (() => { const o = ptyroTerm.host.outbox(); return { online: o.online, mode: o.mode, shown: o.shown }; })(), canType: ptyroTerm.host.canType(), cursor: ptyroTerm.host.screen()?.cursor }; })()`))}`);
    if (!auto.slow && auto.cells) throw new Error(`a prediction was drawn on a fast link: ${JSON.stringify(auto)}`);
    await settled();
    await tab.eval(`ptyroTerm.predict.force = true`);
    const forced = await key("o");
    if (forced.cells !== "o" || forced.cursor?.x !== 6) throw new Error(`the guess wasn't drawn at the cursor: ${JSON.stringify(forced)}`);
    await settled();
    await until("the letters, typed once", () => last() === "$ echo", 10_000);
    await tab.type(" hi");
    await tab.enter();
    await until("the command ran", () => lines().includes("hi"), 10_000);
    // A password: nothing typed there is drawn, ever.
    await tab.type("stty -echo; read x; stty echo; echo got-$x");
    await tab.enter();
    await sleep(1200);
    for (const c of "secret") {
      const r = await key(c);
      if (r.cells.trim()) throw new Error(`a letter typed at a prompt that doesn't echo was drawn: ${JSON.stringify(r)}`);
      await sleep(120);
    }
    await sleep(2000);
    if (!(await tab.eval(`ptyroTerm.predict.preds.length === 0 && ptyroTerm.predict.stats.misses >= 1`))) throw new Error("the unechoed guesses weren't dropped as misses");
    await tab.enter();
    await until("every key reached the session as typed", () => lines().includes("got-secret"), 10_000);
    if (lines().some((l) => l.includes("secret") && !l.includes("got-secret"))) throw new Error(`the password was echoed: ${JSON.stringify(lines().slice(-4))}`);
    // The option is in Aa, and off draws nothing.
    await tab.eval(`ptyroTerm.options()`);
    await tab.waitFor("Predict typing in the terminal's options", `document.body.innerText.includes("Predict typing: auto")`);
    await tab.eval(`[...document.querySelectorAll(".menu button, .sheet button, button")].find(b => b.textContent.includes("Predict typing: auto")).click()`);
    if (!(await tab.eval(`ptyroTerm.predict.mode === "off" && localStorage.getItem("ptyro.term.predict") === "off"`))) throw new Error("the option didn't switch off and stay off");
    const off = await key("x");
    if (off.cells) throw new Error(`drawn while off: ${JSON.stringify(off)}`);
    await tab.eval(`(() => { localStorage.removeItem("ptyro.term.predict"); localStorage.removeItem("ptyro.debug"); })()`);
    ptyro("kill", "predict");
  },

  async offline() {
    await fresh("#/");
    await tab.waitFor("a card", `!!document.querySelector("a.card")`);
    await hostOp("down");
    await tab.waitFor("the offline banner", `!!document.querySelector(".offline")`, 30_000);
    await hostOp("up");
    await tab.waitFor("the banner to go", `!document.querySelector(".offline")`, 60_000);
    ptyro("new", "-d", "-n", "after-offline", "--", "sh", "-c", "cat");
    await tab.waitFor("a session started while away", card("after-offline"));
  },

  // The first-run banner (BOOT3): no agent on the computer (the fake machine has none yet), so
  // the Sessions tab offers the checklist, and its button opens it.
  async banner() {
    await fresh("#/");
    const text = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;
    await tab.waitFor("the banner", text("No coding agent is installed on this computer yet"), 30_000);
    await tab.eval(`[...document.querySelectorAll(".setup-banner a")].find(a => a.textContent === "Set up agents and tools").click()`);
    await tab.waitFor("the checklist", `location.hash === "#/setup" && document.body.innerText.includes("OpenCode")`, 30_000);
  },

  // Settings → Agents and tools: the daemon's checklist (a fake machine) in the TUI's shape,
  // Agents and Tools with each one's line; the administrator steps (apt) shown with the command
  // to run on the computer and the TUI's way, never run from here; Install runs the rest, with
  // its lines and report (BOOT1, BOOT3).
  async setup() {
    await fresh("#/settings");
    const text = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;
    await tab.waitFor("the Settings entry", `!!document.querySelector('a[href="#/setup/settings"]')`, 30_000);
    await tab.eval(`document.querySelector('a.setup-link').click()`);
    await tab.waitFor("the checklist", text("OpenCode"), 30_000);
    for (const t of ["AGENTS", "TOOLS", "Anthropic", "then log in", "asks for admin"]) await tab.waitFor(t, `document.body.innerText.toUpperCase().includes(${JSON.stringify(t.toUpperCase())})`);
    await tab.waitFor("the admin note", text("Administrator rights are asked for only on the computer itself"));
    await tab.waitFor("the admin command", `[...document.querySelectorAll(".setup-admin code")].some(c => c.textContent === "ptyro install git gh")`);
    await tab.waitFor("the TUI's way", text("then U"));
    await tab.waitFor("the back arrow to Settings", `!!document.querySelector('header a[href="#/settings"]')`);
    await tab.eval(`[...document.querySelectorAll("button")].find(b => b.textContent === "Install 2").click()`);
    await tab.waitFor("the run's report", text("2 installed. git and GitHub CLI are left for the computer (administrator rights): run ptyro install git gh there."), 30_000);
    await tab.waitFor("the admin skips' lines", text("git: left for the computer"));
    await tab.waitFor("its lines", text("fake vendor: install claude"));
    await tab.waitFor("Already here", `[...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("Claude Code") && e.innerText.includes("your default agent"))`, 30_000);
  },

  // One setup at a time, on the web too (BOOT3): a run started here (OpenCode, whose fake
  // installer waits) is still going when the page opens again; the new page says so, shows its
  // lines and offers no second Install; once it ends, the report shows there.
  async setupRunning() {
    const world = join(E.fake, "world.json");
    const go = join(E.fake, "go");
    rmSync(go, { force: true });
    writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"], outcomes: { opencode: "wait" } }));
    const text = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;
    const install = async (name) => {
      await tab.waitFor("the checklist", `!!document.querySelector('input[aria-label=${JSON.stringify(name)}]')`, 30_000);
      await tab.eval(`document.querySelector('input[aria-label=${JSON.stringify(name)}]').click()`);
      await tab.waitFor(`${name} ticked`, `[...document.querySelectorAll("button")].some(b => b.textContent === "Install 1" && !b.disabled)`, 30_000);
      await tab.eval(`[...document.querySelectorAll("button")].find(b => b.textContent === "Install 1").click()`);
      await tab.waitFor("its installer waiting", text("fake vendor: waiting for go"), 30_000);
    };
    try {
      await fresh("#/setup");
      await install("OpenCode");
      // A page that didn't start it (this one, opened again).
      await fresh("#/setup");
      await tab.waitFor("the run going already", text("Setup is already running on the computer"), 30_000);
      await tab.waitFor("its lines so far", text("fake vendor: waiting for go"));
      await tab.waitFor("no second install", `[...document.querySelectorAll("button")].some(b => b.textContent === "Installing…" && b.disabled)`);
      writeFileSync(go, "");
      await tab.waitFor("its report", text("1 installed. git and GitHub CLI are left for the computer"), 30_000);
      await tab.waitFor("the checklist after it", `[...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("OpenCode"))`, 30_000);
      // (The fake machine forgets it, for the next step.)
      rmSync(join(E.fake, "installed", "opencode"), { force: true });

      // Setup running from a terminal (the machine's lock, not the daemon's run): the page says
      // who instead of offering Install, and offers it again on its own once that ends.
      rmSync(go, { force: true });
      const cli = spawn(E.bin, ["install", "opencode", "--yes"], { env: { ...process.env, ...E.env, PTYRO_TEST_SETUP: E.fake }, cwd: E.repo, stdio: "ignore" });
      await until("the terminal's install to start", () => existsSync(join(E.env.PTYRO_HOME, "state", "setup.who")));
      await fresh("#/setup");
      await tab.waitFor("who is installing", text("Setup is already running on the computer (from `ptyro install`"), 30_000);
      await tab.waitFor("no Install meanwhile", `[...document.querySelectorAll("button")].some(b => b.textContent === "Waiting for the other setup" && b.disabled)`);
      writeFileSync(go, "");
      await new Promise((ok) => cli.on("exit", ok));
      await tab.waitFor("Install offered again", `!document.body.innerText.includes("Setup is already running") && [...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("OpenCode"))`, 30_000);
      // (The fake machine forgets it, for the next step.)
      rmSync(join(E.fake, "installed", "opencode"), { force: true });

      // Away and back: the page's connection drops mid-run (the server goes), the run ends
      // meanwhile, and the page, back, says so and shows the checklist as it is now.
      rmSync(go, { force: true });
      await fresh("#/setup");
      await install("OpenCode");
      await hostOp("down");
      await tab.waitFor("the offline banner", `!!document.querySelector(".offline")`, 30_000);
      writeFileSync(go, "");
      // The run has let go of the machine's lock (after its last event): nothing is running.
      await until("the run to end in the daemon", () => existsSync(join(E.fake, "installed", "opencode")) && !existsSync(join(E.env.PTYRO_HOME, "state", "setup.who")));
      await hostOp("up");
      await tab.waitFor("the note that it finished away", text("It finished while this page was away"), 60_000);
      await tab.waitFor("the checklist as it is now", `[...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("OpenCode"))`, 30_000);
      await tab.waitFor("Install free again", `![...document.querySelectorAll("button")].some(b => b.textContent === "Installing…")`);
    } finally {
      // Never leave a fake installer holding the run for the scenarios after this one.
      writeFileSync(go, "");
      writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"] }));
    }
  },

  // Out of date, offered (BOOT5): on Health, an `updates` finding's "Look for newer versions
  // now" finds Claude Code's newer version (a test registry, never the network); its Update says
  // what runs, then runs it through the daemon, the setup page following it to "1 updated.";
  // Health then offers nothing for it.
  async update() {
    const world = join(E.fake, "world.json");
    const reg = E.env.PTYRO_TEST_LATEST;
    mkdirSync(reg, { recursive: true });
    mkdirSync(join(E.fake, "installed"), { recursive: true });
    writeFileSync(join(reg, "claude"), "2.1.400");
    writeFileSync(join(E.fake, "installed", "claude"), "2.1.300");
    writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"], versions: { claude: "2.1.400" } }));
    const text = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;
    const sec = `[...document.querySelectorAll("section.block-sec")].find(s => s.querySelector("h3")?.textContent === "updates")`;
    const tapIn = (what) => tab.eval(`[...${sec}.querySelectorAll(".row")].find(r => r.innerText.includes(${JSON.stringify(what)})).click()`);
    const choose = (label) => tab.eval(`[...document.querySelectorAll(".menu button")].find(b => b.innerText.startsWith(${JSON.stringify(label)})).click()`);
    try {
      await fresh("#/health");
      await tab.waitFor("the updates section", `!!${sec}`, 60_000);
      await tab.eval(`${sec}.querySelector(".row").click()`);
      await tab.waitFor("its menu", `!!document.querySelector(".menu button")`);
      await choose("Look for newer versions now");
      await tab.waitFor("Claude Code's newer version", `!!${sec} && ${sec}.innerText.includes("Claude Code 2.1.400 is out (you have 2.1.300)")`, 60_000);
      await tab.waitFor("its command", text("it runs `claude update`"));
      await tapIn("Claude Code 2.1.400 is out");
      await tab.waitFor("the Update choice", `!!document.querySelector(".menu button")`);
      await choose("Update Claude Code to 2.1.400");
      await tab.waitFor("what it runs, asked first", `!!document.querySelector(".sheet") && document.querySelector(".sheet").innerText.includes("Claude Code 2.1.300 → 2.1.400")`);
      await tab.eval(`[...document.querySelectorAll(".sheet-foot button")].find(b => b.textContent === "Update").click()`);
      await tab.waitFor("the setup page following it", `location.hash === "#/setup"`, 30_000);
      await tab.waitFor("its report", text("1 updated."), 60_000);
      await tab.waitFor("its line", text("fake vendor: update claude"));
      await until("Claude Code updated on the computer", () => readFileSync(join(E.fake, "installed", "claude"), "utf8").trim() === "2.1.400");
      await fresh("#/health");
      await tab.waitFor("the updates section again", `!!${sec}`, 60_000);
      await tab.waitFor("nothing offered for it", `!${sec}.innerText.includes("Claude Code 2.1.400 is out")`, 30_000);
    } finally {
      rmSync(join(reg, "claude"), { force: true });
      writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"] }));
    }
  },

  // Moves to the recommended installer (BOOT7): Claude Code and Codex installed with npm are rows
  // offering the move; Keep npm keeps Claude Code (it goes to Already here, not offered again);
  // Codex ticked moves: the vendor's installer, then npm's copy removed, "1 moved…".
  async move() {
    const world = join(E.fake, "world.json");
    mkdirSync(join(E.fake, "installed"), { recursive: true });
    writeFileSync(join(E.fake, "installed", "codex"), "0.162.1");
    writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"], other: { claude: "npm", codex: "npm" } }));
    const text = (t) => `document.body.innerText.includes(${JSON.stringify(t)})`;
    try {
      await fresh("#/setup");
      await tab.waitFor("Codex offered the move", `!!document.querySelector('input[aria-label="Codex"]') && ${text("installed with npm · move to the recommended installer?")}`, 30_000);
      await tab.waitFor("Claude Code offered it too", `!!document.querySelector('input[aria-label="Claude Code"]')`);
      await tab.eval(`[...document.querySelectorAll(".setup-item-row")].find(r => r.innerText.includes("Claude Code")).querySelector("button.setup-keep").click()`);
      await tab.waitFor("Claude Code kept", `!document.querySelector('input[aria-label="Claude Code"]') && [...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("Claude Code"))`, 30_000);
      await tab.eval(`document.querySelector('input[aria-label="Codex"]').click()`);
      await tab.waitFor("Codex ticked", `[...document.querySelectorAll("button")].some(b => b.textContent === "Install 1" && !b.disabled)`, 30_000);
      await tab.eval(`[...document.querySelectorAll("button")].find(b => b.textContent === "Install 1").click()`);
      await tab.waitFor("its report", text("1 moved to the recommended installer."), 60_000);
      await tab.waitFor("the new one first", text("fake vendor: install codex"));
      await tab.waitFor("then npm's copy removed", text("fake npm: removed codex"));
      await tab.waitFor("Codex here, the recommended way", `[...document.querySelectorAll(".setup-here")].some(e => e.innerText.includes("Codex"))`, 30_000);
    } finally {
      for (const f of ["installed/codex", "new/codex", "removed/codex"]) rmSync(join(E.fake, f), { force: true });
      writeFileSync(world, JSON.stringify({ platform: "linux", programs: ["apt-get", "sudo"] }));
    }
  },

  // A worktree's ⋯ → Rename…, then its project's (SPEC §2.3): the address changes, the branch
  // and the directory stay. The project gets its name back afterwards.
  async rename() {
    const wts = () => JSON.parse(ptyro("wt", "ls", "--json"));
    ptyro("wt", "new", "e2e-name", "-p", "repo", "--no-fetch", "--no-setup");
    const before = await until("the new worktree", () => wts().find((w) => w.name === "e2e-name"));
    const rename = async (title, name) => {
      await tab.waitFor("its ⋯", `!!document.querySelector('header button[aria-label="More"]')`);
      await tab.click('header button[aria-label="More"]');
      await tab.waitFor("its menu", `!!document.querySelector(".menu button")`);
      await tab.eval(`[...document.querySelectorAll(".menu button")].find(b => b.innerText.startsWith("Rename…")).click()`);
      await tab.waitFor(title, `!!document.querySelector(".sheet") && document.querySelector(".sheet").innerText.includes(${JSON.stringify(title)})`);
      await tab.eval(`(() => { const i = document.querySelector(".sheet input"); i.value = ${JSON.stringify(name)}; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
      await tab.click(".sheet-foot .primary");
    };
    await fresh(`#/wt/${encodeURIComponent(before.id)}`);
    await rename("Rename e2e-name", "e2e-renamed");
    const after = await until("the rename", () => wts().find((w) => w.id === before.id && w.name === "e2e-renamed"));
    if (after.branch !== before.branch || after.path !== before.path) throw new Error(`moved: ${JSON.stringify(after)}`);
    await fresh(`#/proj/${encodeURIComponent(before.project)}`);
    try {
      await rename("Rename repo", "e2e-repo");
      await until("the project's rename", () => wts().some((w) => w.id === before.id && w.project_slug === "e2e-repo"));
    } finally {
      ptyro("project", "rename", before.project, "repo");
    }
  },

  // Clean up (SPEC §10.8): a worktree squash-merged into main is listed; Keep it keeps it (the
  // daemon remembers), so it leaves the list and `ptyro gc` no longer offers it.
  async keep() {
    const wts = () => JSON.parse(ptyro("wt", "ls", "--json"));
    const git = (cwd, ...args) => execFileSync("git", args, { cwd, env: { ...process.env, ...E.env }, encoding: "utf8" });
    ptyro("wt", "new", "e2e-keep", "-p", "repo", "--no-fetch", "--no-setup");
    const w = await until("the new worktree", () => wts().find((x) => x.name === "e2e-keep" && x.state === "ready"));
    writeFileSync(join(w.path, "kept.txt"), "kept\n");
    git(w.path, "add", "kept.txt");
    git(w.path, "commit", "-qm", "kept");
    git(E.repo, "merge", "-q", "--squash", w.branch);
    git(E.repo, "commit", "-qm", "e2e-keep (squashed)");
    const listed = (name) => `[...document.querySelectorAll(".rows .row")].some(r => r.innerText.includes(${JSON.stringify(name)}))`;
    await fresh("#/gc");
    await tab.waitFor("the squash-merged worktree", `${listed("e2e-keep")} && document.body.innerText.includes("squash-merged")`, 30_000);
    await tab.eval(`[...document.querySelectorAll(".rows .row")].find(r => r.innerText.includes("e2e-keep")).click()`);
    await tab.waitFor("its menu", `!!document.querySelector(".menu button")`);
    await tab.eval(`[...document.querySelectorAll(".menu button")].find(b => b.innerText.startsWith("Keep it")).click()`);
    await tab.waitFor("it to leave the list", `!${listed("e2e-keep")}`, 15_000);
    const stale = JSON.parse(ptyro("gc", "--json"));
    if (stale.some((e) => e.worktree.name === "e2e-keep")) throw new Error(`still offered: ${JSON.stringify(stale)}`);
  },

  // A repository moved (SPEC §10.1): its project's page says its directory is gone, and ⋯ →
  // Move… (the place ptyro found filled in, else typed) points it at the new place; the banner
  // goes. The project keeps its id.
  async move() {
    const git = (cwd, ...args) => execFileSync("git", args, { cwd, env: { ...process.env, ...E.env }, encoding: "utf8" });
    const home = E.env.HOME;
    const from = join(home, "e2e-move", "was", "mvapp");
    const to = join(home, "e2e-move", "now", "mvapp");
    rmSync(join(home, "e2e-move"), { recursive: true, force: true });
    mkdirSync(from, { recursive: true });
    mkdirSync(join(home, "e2e-move", "now"), { recursive: true });
    git(from, "init", "-q", "-b", "main");
    git(from, "remote", "add", "origin", "https://example.com/e2e/mvapp.git");
    git(from, "commit", "-q", "--allow-empty", "-m", "init");
    const show = () => JSON.parse(ptyro("project", "show", "mvapp", "--json")).project;
    ptyro("project", "add", from);
    const id = show().id;
    try {
      execFileSync("mv", [from, to]);
      // Adding the new place spots the move (and refuses: it asks, and there's no terminal).
      try { ptyro("project", "add", to); } catch { /* refused, offering the move */ }
      await fresh(`#/proj/${encodeURIComponent(id)}`);
      await tab.waitFor("the banner saying it's gone", `!!document.querySelector(".banner.moved") && document.querySelector(".banner.moved").innerText.includes("is gone")`, 30_000);
      await tab.click('header button[aria-label="More"]');
      await tab.waitFor("its menu", `!!document.querySelector(".menu button")`);
      // Missing: the menu leads with Move…, then Forget.
      const first = await tab.eval(`[...document.querySelectorAll(".menu button")].map(b => b.innerText.split(String.fromCharCode(10))[0]).slice(0, 2).join("|")`);
      if (first !== "Move…|Forget this project") throw new Error(`the menu leads with ${first}`);
      await tab.eval(`[...document.querySelectorAll(".menu button")].find(b => b.innerText.startsWith("Move…")).click()`);
      await tab.waitFor("the move form", `!!document.querySelector(".sheet") && document.querySelector(".sheet").innerText.includes("Move mvapp")`);
      // A relative path: the web has no folder for it to start from, so it says how to write it.
      await tab.eval(`(() => { const i = document.querySelector(".sheet input"); i.value = "now/mvapp"; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
      await tab.click(".sheet-foot .primary");
      await tab.waitFor("the relative path refused", `!!document.querySelector(".sheet .form-err") && document.querySelector(".sheet .form-err").innerText.includes("A full path is needed")`);
      if (!show().root.endsWith("/was/mvapp")) throw new Error(`moved by a relative path: ${JSON.stringify(show())}`);
      await tab.eval(`(() => { const i = document.querySelector(".sheet input"); i.value = ${JSON.stringify(to)}; i.dispatchEvent(new Event("input", { bubbles: true })); })()`);
      await tab.click(".sheet-foot .primary");
      const p = await until("the move", () => { const p = show(); return p.root.endsWith("/now/mvapp") ? p : null; });
      if (p.id !== id) throw new Error(`another project: ${JSON.stringify(p)}`);
      await tab.waitFor("the banner to go", `!document.querySelector(".banner.moved")`, 15_000);
    } finally {
      try { ptyro("project", "rm", "mvapp", "--yes", "--release"); } catch { /* gone already */ }
    }
  },

  // Machine and profile tabs (TABS.3, SPEC §11.2k): with one profile there is no strip; with
  // `me` and `work` in use it shows `all`, `[default]`, `[me]`, `[work]` with what needs you on
  // each; a tab lists only the sessions that run as its profile (a `me` session in the `main`
  // workspace, whose profile is `default`, is in `[me]` and not in `[default]`); the pick
  // survives a reload; opening a session outside the tab switches to its tab; a new workspace
  // starts on the tab's profile; a new shell or task under a tab lands in it; it fits a phone.
  async tabs() {
    const strip = `document.querySelector(".chips.scope")`;
    const tabs = `[...(${strip}?.querySelectorAll("button") ?? [])].map((b) => b.textContent)`;
    const pick = (label) => tab.eval(`[...${strip}.querySelectorAll("button")].find((b) => b.firstChild.textContent === ${JSON.stringify(label)}).click()`);
    const on = `${strip}?.querySelector("button.on")?.firstChild.textContent`;
    const kept = (profile) => `localStorage.getItem("ptyro.tab") === ${JSON.stringify(JSON.stringify({ profile }))}`;
    const made = [];
    ptyro("new", "-d", "-n", "tabs-def", "--", "sh", "-c", "cat");
    try {
      await tab.waitFor("its card", card("tabs-def"));
      await sleep(500);
      if (await tab.eval(`!!${strip}`)) throw new Error(`a strip with one profile: ${await tab.eval(`JSON.stringify(${tabs})`)}`);
      ptyro("profile", "new", "me");
      ptyro("profile", "new", "work");
      ptyro("ws", "new", "tabsjob", "--profile", "work");
      ptyro("new", "-d", "-n", "tabs-me", "--agent", "claude", "--profile", "me", "-y");
      ptyro("send", "tabs-me", "ask rm -rf build", "--wait-ready");
      ptyro("shells", "new", "-d", "-n", "tabs-job", "-w", "tabsjob", "-C", E.repo, "--", "sh", "-c", "cat");
      // `me` is in use through its session alone, `work` through its workspace (and its shell).
      await tab.waitFor("the strip, with what waits on `[me]`", `(() => { const t = ${tabs}; return t[0]?.startsWith("all") && t.some((x) => x.startsWith("[default]")) && t.some((x) => x === "[me] ⚠1") && t.some((x) => x.startsWith("[work]")); })()`, 20_000);
      if (await tab.eval(on) !== "all") throw new Error("`all` isn't picked at first");
      await tab.waitFor("every session under `all`", `${card("tabs-def")} && ${card("tabs-me")}`);
      await pick("[me]");
      await tab.waitFor("only `me`'s sessions", `${card("tabs-me")} && !${card("tabs-def")} && !document.body.innerText.includes("tabs-job")`);
      await pick("[default]");
      await tab.waitFor("no `me` session under `[default]`", `${card("tabs-def")} && !${card("tabs-me")}`);
      await pick("[work]");
      await tab.waitFor("`work`'s shell, in its workspace", `!${card("tabs-def")} && !${card("tabs-me")} && document.body.innerText.includes("tabs-job")`);
      // Kept in the browser: a reload shows `[work]` again.
      await fresh("#/");
      await tab.waitFor("`[work]` after a reload", `${on} === "[work]" && !${card("tabs-def")}`, 15_000);
      // A new workspace starts on the tab's profile.
      await fresh("#/ws");
      await tab.waitFor("the workspaces", `document.body.innerText.includes("tabsjob")`, 15_000);
      await sleep(500);
      await tab.click("button.icon.plus");
      await tab.waitFor("the form, on `work`", `document.querySelectorAll(".sheet select").length > 0 && [...document.querySelectorAll(".sheet select")].some((s) => s.value === "work")`, 15_000);
      // Going to a session outside the tab (a link, a notification, Home) switches to its tab.
      await fresh(`#/s/${encodeURIComponent(find("tabs-me").id)}`);
      await tab.waitFor("its page", `!!document.querySelector(".term-box, canvas")`, 15_000);
      // The page can draw before the list arrives: the strip follows once the session is known.
      await tab.waitFor("the tab switched to `[me]`", kept("me"), 15_000);
      await tab.eval(`location.hash = "#/"`);
      await tab.waitFor("`[me]` picked by going to its session", `${on} === "[me]" && ${card("tabs-me")}`, 15_000);
      // Under `[me]` (no workspace of its own), SHELLS' `＋ new shell` starts one running as `me`:
      // its page opens and the tab stays `[me]`.
      await tab.waitFor("SHELLS' ＋ new shell", `!!document.querySelector(".shells .shell-new")`);
      await tab.click(".shells .shell-new");
      const shell = await until("a shell running as `me`", () => sessions().find((s) => s.loose && s.profile === "me"));
      made.push(shell.name);
      await tab.waitFor("its page", `location.hash === ${JSON.stringify(`#/s/${encodeURIComponent(shell.id)}`)}`, 15_000);
      await sleep(300);
      if (!(await tab.eval(kept("me")))) throw new Error(`the tab moved: ${await tab.eval(`localStorage.getItem("ptyro.tab")`)}`);
      // A new task under `[me]` runs as `me`.
      await tab.eval(`location.hash = "#/"`);
      await tab.waitFor("`[me]` still", `${on} === "[me]"`, 15_000);
      await tab.click("header button.icon.plus");
      await tab.waitFor("the task sheet, on `me`", `[...document.querySelectorAll(".sheet select")].some((s) => s.value === "me")`, 15_000);
      await tab.click(".sheet-foot button:not(.primary)");
      // A phone: the strip stays one line that scrolls sideways, and the page doesn't.
      await tab.phone(320, 640);
      await tab.waitFor("a phone-sized page", `innerWidth <= 320`);
      const fit = await tab.eval(`(() => { const s = ${strip}; const b = [...s.querySelectorAll("button")]; return { page: document.documentElement.scrollWidth <= innerWidth, line: b.every((x) => x.offsetTop === b[0].offsetTop), scrolls: getComputedStyle(s).overflowX }; })()`);
      if (!fit.page || !fit.line || fit.scrolls !== "auto") throw new Error(`the strip on a phone: ${JSON.stringify(fit)}`);
    } finally {
      for (const n of ["tabs-me", "tabs-job", "tabs-def", ...made]) { try { ptyro("rm", "--stop", "-y", n); } catch { /* gone */ } }
      // Its sessions go a moment after `rm`: until then the workspace isn't empty.
      await until("the sessions gone", () => !sessions().some((s) => ["tabs-me", "tabs-job", "tabs-def", ...made].includes(s.name)), 20_000, 250);
      const wsGone = () => { const v = JSON.parse(ptyro("ws", "ls", "--json")); return !(Array.isArray(v) ? v : v.workspaces ?? []).some((w) => w.name === "tabsjob"); };
      await until("`ws rm tabsjob`", () => { if (!wsGone()) { try { ptyro("ws", "rm", "tabsjob", "--yes"); } catch { /* again */ } } return wsGone(); }, 20_000, 250);
      for (const p of ["me", "work"]) { try { ptyro("profile", "rm", p, "--yes", "--force"); } catch { /* gone */ } }
      await tab.eval(`localStorage.removeItem("ptyro.tab")`).catch(() => {});
    }
    // One profile again: no strip, and the list shows everything.
    await fresh("#/");
    await tab.waitFor("no strip with one profile again", `!!document.querySelector("main.list") && !${strip}`, 15_000);
  },
};

let failed = 0;
try {
  tab = await chrome.tab(`${base}/#pair=${code}`);
  await tab.waitFor("pairing to finish", `!!document.querySelector("header, .list, main") && !location.hash.includes("pair=")`, 30_000);
  for (const [name, run] of Object.entries(scenarios)) {
    if (only.length && !only.includes(name)) continue;
    const t0 = Date.now();
    try {
      if (name !== "list") await fresh("#/");
      await run();
      say(`ok   ${name} (${Date.now() - t0} ms)`);
    } catch (e) {
      failed++;
      const shot = await tab.screenshot(join(tmpdir(), `ptyro-e2e-${name}.png`)).catch(() => "no screenshot");
      const page = await tab.eval(`location.href + "\\n" + document.body.innerText.slice(0, 600) + "\\n" + (document.querySelector(".sheet")?.outerHTML ?? "(no sheet)").slice(0, 600)`).catch(() => "(page unreadable)");
      say(`FAIL ${name}: ${e.message}\n     screenshot: ${shot}\n     page: ${String(page).replace(/\n/g, "\n       ")}`);
    }
  }
} finally {
  await chrome.stop();
}
process.exit(failed ? 1 : 0);
