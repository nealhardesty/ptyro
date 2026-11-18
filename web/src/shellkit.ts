// What a SHELLS shell is called (SPEC §23.4), with no DOM, so web/test/shells.mjs runs it in Node.
import type { Session } from "./screen.js";

const base = (p: string): string => p.split("/").filter(Boolean).pop() ?? p;

/// A program's name from its path, as the process list names it (`foreground`): its last part, a
/// login shell's `-` dropped. `program_name` in crates/ptyro-core/src/session.rs, with the same
/// table in web/test/shells.mjs.
export function programName(argv0: string): string {
  return base(argv0).replace(/^-+/, "");
}

/// A shell's label (SPEC §23.4): its title, else what runs in it and where, else where.
export function shellLabel(s: Session): string {
  if (s.title && s.title !== s.window_title) return s.title;
  if (s.kind !== "shell") return s.name;
  const dir = s.live_cwd ?? s.cwd ?? "";
  const where = dir ? `…/${base(dir)}` : "";
  const shell = s.argv?.[0] ? programName(s.argv[0]) : "";
  const fg = s.foreground?.replace(/^-/, "");
  if (fg && fg !== shell) return `${fg} · ${where}`;
  return where || s.name;
}
