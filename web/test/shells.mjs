// A shell's program named as the process list names it. No browser.
//   node web/test/shells.mjs
// The same table as `programs_are_named_as_the_process_list_names_them` in
// crates/ptyro-core/src/session.rs: change both together.
import assert from "node:assert/strict";
import { programName, shellLabel } from "../dist/shellkit.js";

for (const [argv0, name] of [
  ["/bin/zsh", "zsh"],
  ["-zsh", "zsh"],
  ["bash", "bash"],
  ["/opt/My Tools/Fish", "Fish"],
  ["C:\\Program Files\\PowerShell\\7\\pwsh.exe", "pwsh"],
  ["C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\PowerShell.EXE", "powershell"],
  ["C:\\Windows\\System32\\cmd.exe", "cmd"],
  ["C:\\Program Files\\Git\\bin\\bash", "bash"],
  ["pwsh.exe", "pwsh"],
]) assert.equal(programName(argv0), name, argv0);

// A Windows daemon's idle PowerShell reads as where it is; something run in it, as that.
const sh = (argv0, fg, cwd) => ({ kind: "shell", name: "s1", argv: [argv0], foreground: fg, cwd, live_cwd: cwd });
const pwsh = "C:\\Program Files\\PowerShell\\7\\pwsh.exe";
assert.equal(shellLabel(sh(pwsh, "pwsh", "D:\\src\\app")), "…\\app");
assert.equal(shellLabel(sh(pwsh, "htop", "D:\\src\\app")), "htop · …\\app");
assert.equal(shellLabel(sh("/bin/zsh", "zsh", "/home/me/dl")), "…/dl", "as before");
console.log("shells: ok");
