// The daemon's screen model on the client (SPEC §6.5; crates/ptyro-core/src/cache.rs in
// TypeScript): apply keyframes and deltas, read the text back. Shared by the web app and its
// tests (web/test), which feed it live frames from a real daemon.
export function runCells(run) {
    const chars = Array.from(run.text);
    if (!run.clusters)
        return chars;
    const out = [];
    let i = 0;
    for (const n of run.clusters) {
        out.push(chars.slice(i, i + n).join(""));
        i += n;
    }
    return out;
}
export class Screen {
    size = { cols: 80, rows: 24 };
    rows = [];
    styles = new Map();
    cursor = { x: 0, y: 0, visible: true };
    seq = 0;
    title = "";
    reshape(size) {
        this.size = size;
        const rows = [];
        for (let y = 0; y < size.rows; y++)
            rows.push(this.rows[y]?.slice(0, size.cols) ?? []);
        this.rows = rows;
    }
    apply(f) {
        if (f.keyframe) {
            this.styles.clear();
            this.rows = [];
            this.title = "";
        }
        if (f.delta.size)
            this.reshape(f.delta.size);
        else if (f.keyframe)
            this.reshape(this.size);
        for (const d of f.delta.styles ?? [])
            this.styles.set(d.id, d.style);
        for (const r of f.delta.rows ?? []) {
            if (r.y >= this.rows.length)
                continue;
            const cells = [];
            for (const run of r.runs) {
                for (const t of runCells(run)) {
                    cells.push({ t, s: run.style, wide: run.wide });
                    if (run.wide)
                        cells.push({ t: "", s: run.style, spacer: true });
                }
            }
            this.rows[r.y] = cells.slice(0, this.size.cols);
        }
        if (f.delta.cursor)
            this.cursor = f.delta.cursor;
        if (f.delta.title !== undefined)
            this.title = f.delta.title;
        this.seq = f.seq;
    }
    text() {
        return this.rows.map((r) => r.map((c) => (c.spacer ? "" : c.t || " ")).join("").replace(/\s+$/, "")).join("\n").replace(/\n+$/, "");
    }
}
