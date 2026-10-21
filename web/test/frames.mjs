// The web app's screen model against a real daemon (run by crates/ptyro/tests/m9.rs when Node is
// available): sign in through `ptyro serve`, subscribe to a session, apply its frames with
// web/dist/screen.js, and print the text once MARKER shows up. The Rust test compares it with
// `ptyro read`, so the TypeScript and Rust screen caches can't drift apart unnoticed.
//   node web/test/frames.mjs ws://127.0.0.1:PORT/ws TOKEN SESSION MARKER
import { Screen } from "../dist/screen.js";

const [url, token, name, marker] = process.argv.slice(2);
const ws = new WebSocket(url);
const screen = new Screen();
let id = 0;
const timer = setTimeout(() => { console.error("timed out; screen:\n" + screen.text()); process.exit(2); }, 20000);
ws.onopen = () => ws.send(JSON.stringify({ t: "hello", proto: { major: 1, minor: 8 }, client: { kind: "web", version: "test", pid: 0, token }, features: [] }));
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.t === "welcome") ws.send(JSON.stringify({ t: "request", id: ++id, req: { t: "list" } }));
  else if (m.t === "reject") { console.error(m.reason); process.exit(3); }
  else if (m.t === "response") {
    const s = m.outcome.reply.sessions.find((x) => x.name === name);
    ws.send(JSON.stringify({ t: "subscribe", session: s.id, size: null, control: false }));
  } else if (m.t === "screen") {
    screen.apply(m.frame);
    ws.send(JSON.stringify({ t: "ack", session: m.frame.session, seq: m.frame.seq }));
    if (screen.text().includes(marker)) {
      clearTimeout(timer);
      process.stdout.write(screen.text() + "\n");
      ws.close();
      process.exit(0);
    }
  }
};
