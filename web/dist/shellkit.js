// Either separator: a Windows daemon's paths read with `\` (WIN7).
const base = (p) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
/// A program's name from its path, as the process list names it (`foreground`): its last part, a
/// login shell's `-` dropped; for a Windows path (with `\`, or a runnable extension) also without
/// the extension and lowercase (`C:\…\pwsh.exe` is `pwsh`). `program_name` in
/// crates/ptyro-core/src/session.rs, with the same table in web/test/shells.mjs.
export function programName(argv0) {
    const last = base(argv0).replace(/^-+/, "");
    const stem = /\.(exe|cmd|bat|com)$/i.exec(last);
    if (stem)
        return last.slice(0, stem.index).toLowerCase();
    return argv0.includes("\\") ? last.toLowerCase() : last;
}
/// A shell's label (SPEC §23.4): its title, else what runs in it and where, else where.
export function shellLabel(s) {
    if (s.title && s.title !== s.window_title)
        return s.title;
    if (s.kind !== "shell")
        return s.name;
    const dir = s.live_cwd ?? s.cwd ?? "";
    const where = dir ? `…${dir.includes("\\") ? "\\" : "/"}${base(dir)}` : "";
    const shell = s.argv?.[0] ? programName(s.argv[0]) : "";
    const fg = s.foreground?.replace(/^-/, "");
    if (fg && fg !== shell)
        return `${fg} · ${where}`;
    return where || s.name;
}
