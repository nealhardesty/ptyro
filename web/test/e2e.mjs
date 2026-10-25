// The web app in a real browser, against a real daemon and a real `ptyro serve`: pair, list,
// start a task, answer a permission prompt, edit settings, type into a terminal, survive the
// server going away. Run by crates/ptyro/tests/web_e2e.rs (make web-e2e), which sets the
// scene; no dependencies, headless Chrome over the DevTools protocol (web/test/cdp.mjs).
//   E2E='{"bin":..,"env":{..},"config":..,"repo":..}' node web/test/e2e.mjs CHROME BASE CODE [scenario...]
// A scenario that needs the server restarted prints "@@serve down" / "@@serve up" and waits for
// a line on stdin from the Rust test.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
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

  // An agent asks permission; Allow on the card answers it.
  async permission() {
    ptyro("new", "-d", "-n", "blocked1", "--agent", "claude");
    ptyro("send", "blocked1", "ask rm -rf build", "--wait-ready");
    await tab.waitFor("Allow on the blocked card", `!!document.querySelector(".blocked-wrap .actions .allow")`);
    await tab.click(".blocked-wrap .actions .allow");
    await until("the agent to see the answer", () => ptyro("read", "blocked1").includes("answer: yes"));
    await tab.waitFor("the Allow buttons to go", `!document.querySelector(".blocked-wrap .actions .allow")`);
  },

  // A switch writes the config file (comments kept), Undo restores it, a bad number is refused
  // in the form and never reaches the file.
  async settings() {
    const before = config();
    await fresh("#/settings/interface");
    const sw = `[data-key="ui.clock"] .switch input`;
    await tab.waitFor("the clock switch", `!!document.querySelector(${JSON.stringify(sw)})`);
    const was = await tab.eval(`document.querySelector(${JSON.stringify(sw)}).checked`);
    await tab.click(`[data-key="ui.clock"] .switch`);
    await until("the config to change", () => config() !== before);
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

  // The server goes away: the app says so, then comes back by itself and still works.
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
