const base = (p) => p.split("/").filter(Boolean).pop() ?? p;
/// A program's name from its path, as the process list names it (`foreground`): its last part, a
/// login shell's `-` dropped. `program_name` in crates/ptyro-core/src/session.rs, with the same
/// table in web/test/shells.mjs.
export function programName(argv0) {
    return base(argv0).replace(/^-+/, "");
}
/// A shell's label (SPEC §23.4): its title, else what runs in it and where, else where.
export function shellLabel(s) {
    if (s.title && s.title !== s.window_title)
        return s.title;
    if (s.kind !== "shell")
        return s.name;
    const dir = s.live_cwd ?? s.cwd ?? "";
    const where = dir ? `…/${base(dir)}` : "";
    const shell = s.argv?.[0] ? programName(s.argv[0]) : "";
    const fg = s.foreground?.replace(/^-/, "");
    if (fg && fg !== shell)
        return `${fg} · ${where}`;
    return where || s.name;
}
