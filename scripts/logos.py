"""Render docs/logos: fifty logo concepts, each as a wide logo and a square icon (SVG, stdlib only).
Run via `make logos`, which also renders the PNGs with rsvg-convert. The wordmark is drawn as
strokes, not text, so it looks the same on every machine. Usage: logos.py OUT_DIR"""
import math, os, sys

FLAME = ("M256 64C270 150 380 190 380 310C380 390 324 448 256 448C188 448 132 390 132 310"
         "C132 250 168 222 190 190C196 220 214 236 228 238C214 170 236 110 256 64Z")
DEFS = """<defs>
<linearGradient id="fg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffd93b"/><stop offset=".45" stop-color="#ff8a1f"/><stop offset="1" stop-color="#f2331d"/></linearGradient>
<linearGradient id="fi" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fffbd0"/><stop offset="1" stop-color="#ffc83a"/></linearGradient>
<linearGradient id="gg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#b6ffcf"/><stop offset="1" stop-color="#18c964"/></linearGradient>
<linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#080c20"/><stop offset="1" stop-color="#26346b"/></linearGradient>
<filter id="glow" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="9"/></filter>
</defs>"""

# the four status colours ptyro uses: working, blocked, idle, done
BLUE, AMBER, GREY, GREEN = "#4da3ff", "#ffb21f", "#9aa4b2", "#3ddc84"
AGENT = ["#ff7a45", "#3ddc84", "#4da3ff", "#b57bff"]


def flame(cx, cy, s, fill="url(#fg)", inner=True):
    """The flame, centred on (cx, cy) at scale s, with a bright core unless inner is False."""
    t = f"translate({cx - 256 * s:.1f} {cy - 256 * s:.1f}) scale({s})"
    core = f'<path d="{FLAME}" fill="url(#fi)" transform="translate(128 224) scale(.5)"/>' if inner else ""
    return f'<g transform="{t}"><path d="{FLAME}" fill="{fill}"/>{core}</g>'


def tile(fill):
    return f'<rect width="512" height="512" rx="112" fill="{fill}"/>'


def pixels(rows, colors, cell, ox, oy, gap=3):
    out = []
    for j, row in enumerate(rows):
        for i, ch in enumerate(row):
            if ch in colors:
                out.append(f'<rect x="{ox + i * cell}" y="{oy + j * cell}" width="{cell - gap}" '
                           f'height="{cell - gap}" rx="5" fill="{colors[ch]}"/>')
    return "".join(out)


# ── the ten icons (each a 512×512 tile) ──────────────────────────────────────────────────────

def prompt_flame():
    return (tile("#15101f") + f'<path d="{FLAME}" fill="url(#fg)" transform="translate(0 4)"/>'
            '<path d="M196 292L262 344L196 396" fill="none" stroke="#15101f" stroke-width="30" '
            'stroke-linecap="round" stroke-linejoin="round"/>'
            '<path d="M290 398H338" stroke="#15101f" stroke-width="30" stroke-linecap="round"/>')


def pixel_flame():
    rows = ["....R....", "....RR...", "...RRO...", "...ROOR.R", "..RROORRR", ".RROOOORR",
            ".RROYYOOR", "RROOYYYOR", "RROYYWYOR", "RROYYWYOR", ".RROYYORR", "..RROORR."]
    cols = {"R": "#f2331d", "O": "#ff8a1f", "Y": "#ffd93b", "W": "#fffbd0"}
    grid = "".join(f'<path d="M{x} 40V472M40 {x}H472" stroke="#ffffff" stroke-opacity=".04" stroke-width="2"/>'
                   for x in range(72, 473, 32))
    return tile("#0d1117") + grid + pixels(rows, cols, 32, 112, 64)


def fork_y():
    node = lambda x, y, c: f'<circle cx="{x}" cy="{y}" r="34" fill="#0e2a33" stroke="{c}" stroke-width="16"/>'
    line = "none"
    paths = ("M256 420V300M256 300C256 236 150 236 150 160M256 300C256 236 362 236 362 160M256 300V190")
    return (tile("#0e2a33") + f'<path d="{paths}" fill="{line}" stroke="#d6f3f5" stroke-width="22" '
            'stroke-linecap="round"/>'
            + node(256, 430, "#d6f3f5") + node(150, 150, GREEN) + node(362, 150, BLUE)
            + flame(256, 124, .34))


def match():
    g = ('<g transform="rotate(-35 256 256)">'
         '<rect x="232" y="196" width="48" height="262" rx="18" fill="#e0a868"/>'
         '<rect x="232" y="196" width="18" height="262" rx="9" fill="#f3c88f"/>'
         '<rect x="216" y="130" width="80" height="96" rx="38" fill="#e5362b"/>'
         '<rect x="226" y="140" width="22" height="50" rx="11" fill="#ff7a68"/>'
         + flame(256, 78, .3) + "</g>")
    sparks = "".join(f'<circle cx="{x}" cy="{y}" r="{r}" fill="#ffd93b"/>' for x, y, r in
                     [(368, 120, 7), (408, 168, 5), (332, 80, 5), (150, 120, 5)])
    return tile("#1c1a22") + g + sparks


def fireworks():
    out = [tile("#0b0f2a")]
    for k in range(12):
        a = math.radians(k * 30 + 15)
        c = AGENT[k % 4]
        x1, y1 = 256 + 76 * math.cos(a), 256 + 76 * math.sin(a)
        x2, y2 = 256 + 150 * math.cos(a), 256 + 150 * math.sin(a)
        x3, y3 = 256 + 196 * math.cos(a), 256 + 196 * math.sin(a)
        out.append(f'<path d="M{x1:.1f} {y1:.1f}L{x2:.1f} {y2:.1f}" stroke="{c}" stroke-width="18" '
                   f'stroke-linecap="round"/><circle cx="{x3:.1f}" cy="{y3:.1f}" r="13" fill="{c}"/>')
    out.append('<circle cx="256" cy="256" r="58" fill="#0b0f2a" stroke="#ffffff" stroke-width="10"/>'
               '<path d="M236 232L264 256L236 280M272 284H290" fill="none" stroke="#ffffff" '
               'stroke-width="14" stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def four_pane():
    pane = lambda x, y, c: (f'<rect x="{x}" y="{y}" width="176" height="176" rx="26" fill="#1b2233" '
                            f'stroke="{c}" stroke-width="12"/><circle cx="{x + 30}" cy="{y + 30}" r="9" fill="{c}"/>')
    bar = lambda x, y, w: f'<path d="M{x} {y}H{x + w}" stroke="#5b6882" stroke-width="12" stroke-linecap="round"/>'
    return (tile("#0a0e17") + pane(72, 72, BLUE) + pane(264, 72, AMBER) + pane(72, 264, GREY)
            + pane(264, 264, GREEN) + flame(160, 168, .3)
            + '<path d="M298 140L322 160L298 180M334 184H356" fill="none" stroke="#e6edf3" stroke-width="11" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
            + '<path d="M312 358L338 384L384 328" fill="none" stroke="#3ddc84" stroke-width="18" '
              'stroke-linecap="round" stroke-linejoin="round"/>')


def crt():
    scan = "".join(f'<path d="M96 {y}H416" stroke="#000" stroke-opacity=".28" stroke-width="3"/>'
                   for y in range(110, 372, 9))
    return (tile("#14161a")
            + '<rect x="60" y="64" width="392" height="332" rx="40" fill="#d9d0b8"/>'
              '<rect x="60" y="64" width="392" height="40" rx="40" fill="#efe8d4"/>'
              '<rect x="88" y="92" width="336" height="276" rx="34" fill="#04180b"/>'
            + f'<g filter="url(#glow)" opacity=".9">{flame(256, 232, .56, "#18c964", False)}</g>'
            + flame(256, 232, .56, "url(#gg)", False)
            + '<path d="M214 288L248 314L214 340M262 340H300" fill="none" stroke="#04180b" stroke-width="16" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
            + scan
            + '<rect x="190" y="396" width="132" height="22" rx="8" fill="#b9b09a"/>'
              '<rect x="150" y="414" width="212" height="26" rx="12" fill="#d9d0b8"/>'
              '<circle cx="410" cy="384" r="7" fill="#3ddc84"/>')


def daemon():
    rows = [".H........H.", ".HH......HH.", "..HHBBBBHH..", "..BBBBBBBB..", ".BBBBBBBBBB.",
            ".BBWWBBWWBB.", ".BBWKBBWKBB.", ".BBBBBBBBBB.", ".BBBBMMBBBB.", ".BBBBBBBBBB.",
            ".BBBBBBBBBB.", ".BB.BBBB.BB."]
    cols = {"H": "#ffd23f", "B": "#ff5a36", "W": "#ffffff", "K": "#1a0f2e", "M": "#1a0f2e"}
    return (tile("#1a0f2e") + pixels(rows, cols, 28, 88, 112, 2) + flame(256, 128, .2))


def campfire():
    stars = "".join(f'<circle cx="{x}" cy="{y}" r="{r}" fill="#fff" opacity=".85"/>' for x, y, r in
                    [(84, 90, 4), (150, 56, 3), (410, 80, 4), (452, 150, 3), (60, 190, 3), (360, 40, 3)])
    log = lambda a: (f'<g transform="rotate({a} 256 420)"><rect x="96" y="394" width="320" height="52" rx="26" '
                     'fill="#7a4a2b"/><circle cx="104" cy="420" r="18" fill="#b07a4f"/>'
                     '<circle cx="104" cy="420" r="8" fill="#7a4a2b"/></g>')
    sparks = "".join(f'<circle cx="{x}" cy="{y}" r="{r}" fill="#ffd93b"/>' for x, y, r in
                     [(150, 210, 6), (372, 170, 5), (340, 250, 4), (180, 120, 4)])
    return (f'<rect width="512" height="512" rx="112" fill="url(#sky)"/>' + stars + log(-14) + log(14)
            + flame(256, 270, .74) + sparks)


def ring():
    c = 2 * math.pi * 172
    seg = c / 4
    arcs = "".join(f'<circle cx="256" cy="256" r="172" fill="none" stroke="{col}" stroke-width="46" '
                   f'stroke-linecap="round" stroke-dasharray="{seg - 70:.1f} {c - seg + 70:.1f}" '
                   f'transform="rotate({-90 + 90 * k + 35} 256 256)"/>'
                   for k, col in enumerate([BLUE, AMBER, GREY, GREEN]))
    return tile("#f6f3ec") + arcs + flame(256, 262, .56, "#1f2430", False)


ICONS = [
    ("prompt-flame", "Prompt Flame", "A flame with a `>_` prompt cut out of it: the terminal, on fire.", prompt_flame, "#15101f", ("#f4eefc", "#ff8a1f")),
    ("pixel-flame", "Pixel Flame", "The flame, drawn in terminal cells.", pixel_flame, "#0d1117", ("#f0f3f6", "#ff8a1f")),
    ("fork-y", "Fork Y", "The `y` is a git fork: one trunk, three worktrees, one of them burning.", fork_y, "#0b2229", ("#d6f3f5", "#3ddc84")),
    ("match", "Strike a Match", "Every agent starts with a spark. The match head is a block cursor.", match, "#1c1a22", ("#f5f0e8", "#ff5a36")),
    ("fireworks", "Fireworks", "Pyrotechnics: a burst of agents (four colours, one prompt in the middle).", fireworks, "#0b0f2a", ("#ffffff", "#ffd93b")),
    ("four-panes", "Four Panes", "Four sessions side by side, each with its status colour.", four_pane, "#0a0e17", ("#e6edf3", "#4da3ff")),
    ("crt", "Phosphor", "A green-phosphor CRT, because it is a terminal after all.", crt, "#14161a", ("#c7f9d8", "#18c964")),
    ("daemon", "The Daemon", "A little pixel imp: the background daemon that keeps your agents alive.", daemon, "#1a0f2e", ("#fff4ea", "#ff5a36")),
    ("campfire", "Campfire", "Agents gather round the fire; you can leave and it keeps burning.", campfire, "#0e1530", ("#f3ecdd", "#ff8a1f")),
    ("status-ring", "Status Ring", "Working, blocked, idle, done: the four states around one flame.", ring, "#f6f3ec", ("#1f2430", "#f2331d")),
]


# ── batch two: no fire. ptyro as pty (pseudo-terminal) and as pterodactyl ─────────────────────

def pty_pair():
    dots = "".join(f'<circle cx="{x}" cy="256" r="11" fill="#fff" opacity="{o}"/>'
                   for x, o in [(222, .35), (256, .65), (290, 1)])
    return (tile("#101826")
            + '<rect x="190" y="232" width="132" height="48" fill="#2a3550"/>'
              '<rect x="48" y="166" width="150" height="180" rx="34" fill="#4da3ff"/>'
              '<rect x="314" y="166" width="150" height="180" rx="34" fill="#b57bff"/>'
              '<path d="M90 224L132 256L90 288" fill="none" stroke="#101826" stroke-width="22" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
              '<path d="M360 296H418" stroke="#101826" stroke-width="22" stroke-linecap="round"/>'
              + dots)


def ptero():
    half = [(256, 52), (272, 112), (282, 176), (448, 124), (408, 236), (372, 212), (346, 304),
            (302, 270), (286, 336), (276, 420), (256, 474)]
    pts = half + [(512 - x, y) for x, y in reversed(half[1:-1])]
    poly = "M" + "L".join(f"{x} {y}" for x, y in pts) + "Z"
    return (f'<rect width="512" height="512" rx="112" fill="#0f3b3f"/>'
            f'<path d="{poly}" fill="#ffd9a0" stroke="#ffd9a0" stroke-width="10" stroke-linejoin="round"/>'
            '<path d="M256 150L256 410" stroke="#e8a85c" stroke-width="14" stroke-linecap="round"/>'
            '<path d="M214 232L150 196M298 232L362 196M228 296L196 330M284 296L316 330" '
            'stroke="#e8a85c" stroke-width="10" stroke-linecap="round"/>'
            '<circle cx="256" cy="108" r="9" fill="#0f3b3f"/>')


def octopus():
    out = ['<rect width="512" height="512" rx="112" fill="#2b1055"/>']
    for i in range(8):
        x0, xe = 178 + i * 22, 74 + i * 52
        out.append(f'<path d="M{x0} 290C{x0} 360 {xe} 330 {xe} 410" fill="none" stroke="#ff7eb6" '
                   'stroke-width="26" stroke-linecap="round"/>'
                   f'<circle cx="{xe}" cy="418" r="21" fill="{AGENT[i % 4]}" stroke="#2b1055" stroke-width="6"/>')
    out.append('<path d="M146 300V210A110 110 0 0 1 366 210V300Z" fill="#ff7eb6"/>'
               '<circle cx="214" cy="226" r="26" fill="#fff"/><circle cx="298" cy="226" r="26" fill="#fff"/>'
               '<circle cx="222" cy="232" r="13" fill="#2b1055"/><circle cx="306" cy="232" r="13" fill="#2b1055"/>'
               '<path d="M236 282Q256 298 276 282" fill="none" stroke="#2b1055" stroke-width="10" '
               'stroke-linecap="round"/><ellipse cx="206" cy="130" rx="30" ry="14" fill="#fff" opacity=".3" '
               'transform="rotate(-25 206 130)"/>')
    return "".join(out)


def lighthouse():
    def width_at(y):
        return 40 + (y - 200) / 12
    stripe = lambda y0, y1: (f'<path d="M{256 - width_at(y0):.1f} {y0}H{256 + width_at(y0):.1f}'
                             f'L{256 + width_at(y1):.1f} {y1}H{256 - width_at(y1):.1f}Z" fill="#e5362b"/>')
    return ('<rect width="512" height="512" rx="112" fill="url(#sky)"/>'
            '<path d="M256 150L0 70V230Z M256 150L512 70V230Z" fill="#ffe27a" opacity=".28"/>'
            '<path d="M216 200H296L316 440H196Z" fill="#f6f3ec"/>' + stripe(262, 302) + stripe(346, 386)
            + '<rect x="200" y="184" width="112" height="18" rx="6" fill="#2a3550"/>'
              '<rect x="226" y="124" width="60" height="62" rx="8" fill="#ffe27a"/>'
              '<path d="M256 78L212 126H300Z" fill="#e5362b"/>'
              '<path d="M0 440Q64 410 128 440T256 440T384 440T512 440V512H0Z" fill="#1d2a52"/>'
              '<path d="M0 470Q64 444 128 470T256 470T384 470T512 470V512H0Z" fill="#2a3a70"/>'
              '<circle cx="90" cy="70" r="3" fill="#fff"/><circle cx="420" cy="60" r="3" fill="#fff"/>')


def constellation():
    nodes = {"a": (170, 110), "d": (296, 110), "e": (356, 190), "f": (296, 270), "b": (170, 270), "c": (170, 410)}
    link = "M170 110H296L356 190L296 270H170M170 110V410"
    cols = {"a": AGENT[1], "e": AGENT[2], "c": AGENT[0], "d": "#fff", "f": "#fff", "b": "#fff"}
    bg = "".join(f'<circle cx="{x}" cy="{y}" r="{r}" fill="#fff" opacity=".6"/>' for x, y, r in
                 [(80, 60, 3), (440, 70, 3), (430, 300, 4), (330, 400, 3), (410, 440, 3), (70, 470, 3), (250, 460, 2)])
    stars = "".join(f'<circle cx="{x}" cy="{y}" r="34" fill="{cols[k]}" opacity=".16"/>'
                    f'<circle cx="{x}" cy="{y}" r="{16 if cols[k] != "#fff" else 11}" fill="{cols[k]}"/>'
                    for k, (x, y) in nodes.items())
    return (tile("#0b1020") + bg + f'<path d="{link}" fill="none" stroke="#6f86c8" stroke-width="6" '
            'stroke-linejoin="round" stroke-linecap="round"/>' + stars)


def honeycomb():
    cells = [((0, 0), "#ffb21f"), ((1, 0), BLUE), ((.5, 1), GREEN), ((-.5, 1), "#3a2c14"),
             ((-1, 0), AMBER), ((-.5, -1), GREY), ((.5, -1), "#3a2c14")]
    out = [tile("#1d1608")]
    for (u, v), col in cells:
        cx, cy = 256 + u * 112, 256 + v * 97
        pts = "".join(("M" if k == 0 else "L") + f"{cx + 54 * math.cos(math.radians(60 * k - 30)):.1f} "
                      f"{cy + 54 * math.sin(math.radians(60 * k - 30)):.1f}" for k in range(6)) + "Z"
        out.append(f'<path d="{pts}" fill="{col}" stroke="{col}" stroke-width="14" stroke-linejoin="round"/>')
    out.append('<path d="M232 218L274 256L232 294" fill="none" stroke="#1d1608" stroke-width="22" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def robot():
    return (tile("#12b5a6")
            + '<path d="M256 150V94" stroke="#f4f7fb" stroke-width="14" stroke-linecap="round"/>'
              '<circle cx="256" cy="80" r="24" fill="#ffb21f"/>'
              '<rect x="84" y="236" width="40" height="92" rx="16" fill="#cdd6e4"/>'
              '<rect x="388" y="236" width="40" height="92" rx="16" fill="#cdd6e4"/>'
              '<rect x="110" y="150" width="292" height="244" rx="64" fill="#f4f7fb"/>'
              '<rect x="142" y="182" width="228" height="180" rx="40" fill="#0b1220"/>'
              '<path d="M184 232L222 258L184 284M328 232L290 258L328 284" fill="none" stroke="#3ddc84" '
              'stroke-width="20" stroke-linecap="round" stroke-linejoin="round"/>'
              '<rect x="226" y="314" width="60" height="18" rx="6" fill="#3ddc84"/>'
              '<rect x="150" y="424" width="212" height="40" rx="20" fill="#f4f7fb"/>')


def radar():
    rings = "".join(f'<circle cx="256" cy="256" r="{r}" fill="none" stroke="#1f8f55" stroke-width="5"/>'
                    for r in (66, 128, 190))
    a1, a2 = math.radians(-100), math.radians(-30)
    p = lambda a: f"{256 + 190 * math.cos(a):.1f} {256 + 190 * math.sin(a):.1f}"
    blip = lambda x, y, c, r=16: f'<circle cx="{x}" cy="{y}" r="{r}" fill="{c}"/>'
    return (tile("#04170f") + rings
            + '<path d="M256 66V446M66 256H446" stroke="#1f8f55" stroke-width="4"/>'
            + f'<path d="M256 256L{p(a1)}A190 190 0 0 1 {p(a2)}Z" fill="#3ddc84" opacity=".3"/>'
            + f'<path d="M256 256L{p(a2)}" stroke="#9dffc4" stroke-width="8" stroke-linecap="round"/>'
            + blip(176, 176, GREY) + blip(342, 330, GREEN)
            + '<circle cx="190" cy="334" r="40" fill="none" stroke="#ffb21f" stroke-width="7" opacity=".6"/>'
            + blip(190, 334, AMBER, 20) + blip(330, 190, BLUE, 18) + blip(256, 256, "#9dffc4", 9))


def orbit():
    def moon(rx, ry, rot, t, col):
        a = math.radians(rot)
        x, y = rx * math.cos(t), ry * math.sin(t)
        return (f'<circle cx="{256 + x * math.cos(a) - y * math.sin(a):.1f}" '
                f'cy="{256 + x * math.sin(a) + y * math.cos(a):.1f}" r="24" fill="{col}" '
                'stroke="#0b0d1c" stroke-width="7"/>')
    rings = "".join(f'<ellipse cx="256" cy="256" rx="200" ry="72" fill="none" stroke="#3b4a7a" '
                    f'stroke-width="7" transform="rotate({r} 256 256)"/>' for r in (-30, 30, 90))
    return (tile("#0b0d1c")
            + '<defs><radialGradient id="pl" cx=".35" cy=".3" r=".8"><stop offset="0" stop-color="#9ee7ff"/>'
              '<stop offset="1" stop-color="#2f6fed"/></radialGradient></defs>'
            + rings + '<circle cx="256" cy="256" r="62" fill="url(#pl)"/>'
            + moon(200, 72, -30, 0.9, AGENT[0]) + moon(200, 72, 30, 3.9, AGENT[1])
            + moon(200, 72, 90, 2.4, AGENT[2]) + moon(200, 72, -30, 4.2, AGENT[3]))


def turtle():
    return (tile("#0f4c5c")
            + '<path d="M0 450Q64 424 128 450T256 450T384 450T512 450V512H0Z" fill="#0b3a47"/>'
              '<path d="M100 340L62 358L104 372Z" fill="#9be564"/>'
              '<rect x="120" y="326" width="276" height="48" rx="24" fill="#9be564"/>'
              '<rect x="148" y="352" width="62" height="70" rx="26" fill="#7fcb4a"/>'
              '<rect x="302" y="352" width="62" height="70" rx="26" fill="#7fcb4a"/>'
              '<rect x="372" y="268" width="38" height="80" rx="19" fill="#9be564"/>'
              '<circle cx="424" cy="268" r="48" fill="#9be564"/>'
              '<circle cx="440" cy="256" r="9" fill="#0f4c5c"/>'
              '<path d="M426 290Q442 300 458 290" fill="none" stroke="#0f4c5c" stroke-width="7" stroke-linecap="round"/>'
              '<path d="M104 340A152 152 0 0 1 408 340Z" fill="#27ae60"/>'
              '<rect x="170" y="222" width="172" height="96" rx="18" fill="#0b1220"/>'
              '<path d="M198 248L224 270L198 292M244 296H290" fill="none" stroke="#9be564" stroke-width="13" '
              'stroke-linecap="round" stroke-linejoin="round"/>')


def clipped(fn):
    """An icon whose artwork is cut to the rounded tile."""
    return lambda: ('<defs><clipPath id="tc"><rect width="512" height="512" rx="112"/></clipPath></defs>'
                    f'<g clip-path="url(#tc)">{fn()}</g>')


ICONS += [
    ("pty-pair", "Pty Pair", "A pty is two ends of one pipe: the master and the slave, a prompt and its cursor.", pty_pair, "#101826", ("#e6edf3", "#b57bff")),
    ("pterodactyl", "Ptero", "p-t-y-ro, say it fast: a pterodactyl, wings spread over your terminal.", ptero, "#0f3b3f", ("#ffe9c7", "#ffd9a0")),
    ("octopus", "Octopus", "Eight arms, eight sessions, one cheerful head keeping track of all of them.", octopus, "#2b1055", ("#ffe3f0", "#ff7eb6")),
    ("lighthouse", "Lighthouse", "One beam sweeping the dark to show which agent needs you.", clipped(lighthouse), "#080c20", ("#f6f3ec", "#ffe27a")),
    ("constellation", "Constellation", "Sessions as stars, joined into a p.", constellation, "#0b1020", ("#e9eeff", "#6f86c8")),
    ("honeycomb", "Honeycomb", "A hive of workers: one cell per agent, one key to the cell that is buzzing.", honeycomb, "#1d1608", ("#fff3d6", "#ffb21f")),
    ("robot", "Friendly Robot", "A robot whose face is a prompt and a smile: `> <`.", robot, "#0c8f84", ("#ffffff", "#ffb21f")),
    ("radar", "Radar", "A sweep, four blips, one of them amber: an agent is blocked on you.", radar, "#04170f", ("#c7f9d8", "#3ddc84")),
    ("orbit", "Orbit", "Four agents in orbit around one planet of code.", orbit, "#0b0d1c", ("#e6e9ff", "#4da3ff")),
    ("turtle", "Turtle", "Slow and steady, and it never loses its home: the shell is a terminal.", clipped(turtle), "#0f4c5c", ("#e9fbe0", "#9be564")),
]


# ── batch three: many agents at once ─────────────────────────────────────────────────────────

def flock():
    def bird(x, y, k, col):
        return (f'<path d="M{x - 44 * k:.1f} {y + 12 * k:.1f}Q{x - 22 * k:.1f} {y - 14 * k:.1f} {x} {y + 10 * k:.1f}'
                f'Q{x + 22 * k:.1f} {y - 14 * k:.1f} {x + 44 * k:.1f} {y + 12 * k:.1f}" fill="none" stroke="{col}" '
                f'stroke-width="{20 * k:.1f}" stroke-linecap="round" stroke-linejoin="round"/>')
    out = ['<rect width="512" height="512" rx="112" fill="url(#sky)"/>', bird(256, 110, 1.5, "#ffffff")]
    for i, (x, y) in enumerate([(196, 190), (316, 190), (136, 270), (376, 270), (76, 350), (436, 350)]):
        out.append(bird(x, y, 1.0, AGENT[i % 4]))
    return "".join(out)


def hub():
    out = [tile("#101426"), '<circle cx="256" cy="256" r="164" fill="none" stroke="#2a3558" stroke-width="5" '
           'stroke-dasharray="4 14" stroke-linecap="round"/>']
    cols = AGENT + [GREEN, AMBER]
    pts = [(256 + 164 * math.cos(math.radians(30 + 60 * k)), 256 + 164 * math.sin(math.radians(30 + 60 * k)))
           for k in range(6)]
    for x, y in pts:
        out.append(f'<path d="M256 256L{x:.1f} {y:.1f}" stroke="#46568c" stroke-width="10" stroke-linecap="round"/>')
    for k, (x, y) in enumerate(pts):
        out.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="38" fill="{cols[k]}" stroke="#101426" stroke-width="8"/>')
    out.append('<circle cx="256" cy="256" r="68" fill="#ffd23f" stroke="#101426" stroke-width="8"/>'
               '<path d="M236 228L272 256L236 284" fill="none" stroke="#101426" stroke-width="20" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def chain():
    out = [tile("#12161f")]
    for k, x in enumerate((70, 160, 250, 340)):
        out.append(f'<path d="M{x} 126L{x + 92} 256L{x} 386" fill="none" stroke="{AGENT[k]}" stroke-width="54" '
                   'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def fan():
    out = [tile("#0d1f2d")]
    cols = AGENT + [AMBER]
    for k, y in enumerate((104, 180, 256, 332, 408)):
        out.append(f'<path d="M72 256C152 256 152 {y} 256 {y}C360 {y} 360 256 440 256" fill="none" '
                   f'stroke="{cols[k]}" stroke-width="14" stroke-linecap="round"/>'
                   f'<circle cx="256" cy="{y}" r="19" fill="{cols[k]}" stroke="#0d1f2d" stroke-width="6"/>')
    out.append('<circle cx="72" cy="256" r="30" fill="#f4f7fb" stroke="#0d1f2d" stroke-width="7"/>'
               '<circle cx="440" cy="256" r="38" fill="#ffd23f" stroke="#0d1f2d" stroke-width="7"/>'
               '<path d="M420 256L434 270L462 240" fill="none" stroke="#0d1f2d" stroke-width="12" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def squad():
    bot = ["...A...", ".HHHHH.", "HHHHHHH", "HKHHHKH", "HHHHHHH", ".H.H.H."]
    out = [tile("#151a2e")]
    for k, (x, y) in enumerate([(90, 104), (268, 104), (90, 276), (268, 276)]):
        out.append(pixels(bot, {"A": "#ffd23f", "H": AGENT[k], "K": "#151a2e"}, 22, x, y, 2))
    return "".join(out)


def merge_train():
    out = [tile("#1a2433"), '<path d="M0 424H512" stroke="#5b6882" stroke-width="8"/>']
    out += [f'<path d="M{x} 416V440" stroke="#3a4560" stroke-width="10" stroke-linecap="round"/>'
            for x in range(20, 512, 40)]
    for k, x in enumerate((30, 138, 246)):
        out.append(f'<rect x="{x}" y="270" width="98" height="102" rx="16" fill="{AGENT[k]}"/>'
                   f'<path d="{"M" + str(x + 36) + " 300L" + str(x + 58) + " 320L" + str(x + 36) + " 340"}" fill="none" '
                   'stroke="#1a2433" stroke-width="12" stroke-linecap="round" stroke-linejoin="round"/>'
                   f'<path d="M{x + 98} 346H{x + 110}" stroke="#9aa4b2" stroke-width="10"/>')
    out.append('<rect x="354" y="296" width="124" height="76" rx="16" fill="#ffd23f"/>'
               '<rect x="414" y="250" width="64" height="62" rx="12" fill="#ffb21f"/>'
               '<rect x="364" y="262" width="30" height="40" rx="8" fill="#ffb21f"/>'
               '<circle cx="470" cy="336" r="9" fill="#fff"/>'
               '<circle cx="372" cy="226" r="14" fill="#fff" opacity=".5"/><circle cx="346" cy="190" r="20" fill="#fff" opacity=".3"/>')
    for x in (70, 178, 286, 380, 446):
        out.append(f'<circle cx="{x}" cy="388" r="22" fill="#0a0e17" stroke="#9aa4b2" stroke-width="7"/>')
    return "".join(out)


def venn():
    pts = [(256, 190, AGENT[0]), (196, 294, AGENT[1]), (316, 294, AGENT[2])]
    out = [tile("#11131c")]
    out += [f'<circle cx="{x}" cy="{y}" r="112" fill="{c}" fill-opacity=".72"/>' for x, y, c in pts]
    out.append('<circle cx="256" cy="260" r="46" fill="#f4f7fb"/>'
               '<path d="M244 240L270 260L244 280" fill="none" stroke="#11131c" stroke-width="14" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def bento():
    pane = lambda x, y, w, h, c, sw=10: (f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="24" fill="#161d2c" '
                                         f'stroke="{c}" stroke-width="{sw}"/>')
    bar = lambda x, y, w: f'<path d="M{x} {y}H{x + w}" stroke="#4a5873" stroke-width="12" stroke-linecap="round"/>'
    return (tile("#0a0e17") + pane(56, 56, 246, 400, BLUE, 14)
            + bar(92, 110, 150) + bar(92, 148, 110) + bar(92, 186, 170) + bar(92, 224, 90)
            + '<path d="M92 290L120 312L92 334" fill="none" stroke="#e6edf3" stroke-width="13" stroke-linecap="round" '
              'stroke-linejoin="round"/><rect x="140" y="304" width="30" height="16" rx="4" fill="#4da3ff"/>'
            + pane(322, 56, 134, 186, AMBER) + '<path d="M389 106V162" stroke="#ffb21f" stroke-width="20" stroke-linecap="round"/>'
              '<circle cx="389" cy="198" r="11" fill="#ffb21f"/>'
            + pane(322, 262, 134, 90, GREEN) + '<path d="M366 308L384 326L416 288" fill="none" stroke="#3ddc84" '
              'stroke-width="14" stroke-linecap="round" stroke-linejoin="round"/>'
            + pane(322, 372, 134, 84, GREY)
            + "".join(f'<circle cx="{x}" cy="414" r="9" fill="#9aa4b2"/>' for x in (361, 389, 417)))


def ducks():
    def duck(x, y, k, col, beak="#ff9f1c"):
        return (f'<path d="M{x - 52 * k:.1f} {y - 10 * k:.1f}L{x - 82 * k:.1f} {y - 34 * k:.1f}L{x - 40 * k:.1f} {y + 14 * k:.1f}Z" fill="{col}"/>'
                f'<ellipse cx="{x}" cy="{y}" rx="{58 * k:.1f}" ry="{38 * k:.1f}" fill="{col}"/>'
                f'<circle cx="{x + 44 * k:.1f}" cy="{y - 42 * k:.1f}" r="{29 * k:.1f}" fill="{col}"/>'
                f'<path d="M{x + 68 * k:.1f} {y - 50 * k:.1f}L{x + 100 * k:.1f} {y - 38 * k:.1f}L{x + 68 * k:.1f} {y - 28 * k:.1f}Z" fill="{beak}"/>'
                f'<circle cx="{x + 50 * k:.1f}" cy="{y - 50 * k:.1f}" r="{5.5 * k:.1f}" fill="#1b2a41"/>'
                f'<path d="M{x - 24 * k:.1f} {y}Q{x - 2 * k:.1f} {y + 20 * k:.1f} {x + 24 * k:.1f} {y - 4 * k:.1f}" fill="none" '
                f'stroke="#00000033" stroke-width="{8 * k:.1f}" stroke-linecap="round"/>')
    waves = "".join(f'<path d="M{x} {y}q20 -14 40 0t40 0" fill="none" stroke="#ffffff55" stroke-width="7" '
                    'stroke-linecap="round"/>' for x, y in [(30, 430), (210, 440), (360, 424), (110, 478), (300, 480)])
    out = ['<defs><clipPath id="tc"><rect width="512" height="512" rx="112"/></clipPath></defs>'
           '<g clip-path="url(#tc)"><rect width="512" height="512" fill="#2a8fd6"/>' + waves]
    out.append(duck(372, 238, 1.4, "#ffd23f"))
    for i, x in enumerate((258, 192, 126, 62)):
        out.append(duck(x, 346, .55, AGENT[i]))
    return "".join(out) + "</g>"


def marionette():
    out = [tile("#1d1530"), '<rect x="110" y="86" width="292" height="24" rx="12" fill="#d9a066"/>'
           '<rect x="238" y="40" width="36" height="60" rx="14" fill="#e0a868"/>']
    for k, (bx, px, py) in enumerate([(130, 124, 290), (206, 222, 372), (306, 320, 316), (382, 418, 388)]):
        out.append(f'<path d="M{bx} 108L{px} {py - 32}" stroke="#e6e0f5" stroke-width="5" stroke-linecap="round"/>'
                   f'<rect x="{px - 40}" y="{py - 36}" width="80" height="80" rx="22" fill="{AGENT[k]}"/>'
                   f'<path d="M{px - 14} {py - 12}L{px + 6} {py + 4}L{px - 14} {py + 20}" fill="none" stroke="#1d1530" '
                   'stroke-width="12" stroke-linecap="round" stroke-linejoin="round"/>'
                   f'<path d="M{px + 14} {py + 22}H{px + 24}" stroke="#1d1530" stroke-width="10" stroke-linecap="round"/>')
    return "".join(out)


ICONS += [
    ("flock", "Flock", "A flock in formation: one lead, six following, all heading the same way.", flock, "#080c20", ("#ffffff", "#4da3ff")),
    ("hub", "Hub and Spoke", "A lead agent in the middle, workers around it: orchestration in one picture.", hub, "#101426", ("#e9ecff", "#ffd23f")),
    ("chain", "Chain", "Four chevrons, four agents: the output of one is the prompt of the next (recipes).", chain, "#12161f", ("#f0f3f6", "#b57bff")),
    ("fan", "Fan Out", "One task fans out to many agents, you pick the best, and it comes back as one.", fan, "#0d1f2d", ("#e9f7ff", "#ffd23f")),
    ("squad", "Squad", "A pixel crew of four, one colour each.", squad, "#151a2e", ("#f0f3f6", "#3ddc84")),
    ("merge-train", "Merge Train", "Each agent's change is a car; the train rolls into main in order.", merge_train, "#1a2433", ("#f0f3f6", "#ffd23f")),
    ("venn", "Overlap", "Many agents, one shared prompt in the middle.", venn, "#11131c", ("#f4f7fb", "#3ddc84")),
    ("bento", "Bento", "A tiled workspace: the big pane you work in, the small ones that report status.", bento, "#0a0e17", ("#e6edf3", "#ffb21f")),
    ("ducks", "Ducks in a Row", "Get your ducks in a row: a lead duck and four followers.", ducks, "#1d6fa8", ("#ffffff", "#ffd23f")),
    ("marionette", "Marionette", "One hand, four strings: you steer all the agents at once.", marionette, "#1d1530", ("#f3effa", "#ff7a45")),
]


# ── batch four: the merge train ──────────────────────────────────────────────────────────────

def loco_front():
    bars = "".join(f'<path d="M{x} 404V450" stroke="#1b2230" stroke-width="10"/>' for x in range(190, 330, 28))
    return (tile("#1b2230")
            + '<rect x="214" y="58" width="84" height="84" fill="#2a3550"/>'
              '<rect x="196" y="44" width="120" height="26" rx="10" fill="#3a4666"/>'
              '<circle cx="256" cy="296" r="150" fill="#e5362b"/>'
              '<circle cx="256" cy="296" r="150" fill="none" stroke="#ffd23f" stroke-width="14"/>'
              '<circle cx="256" cy="304" r="98" fill="#22304a" stroke="#ffd23f" stroke-width="10"/>'
              '<path d="M228 262L286 304L228 346" fill="none" stroke="#ffd23f" stroke-width="26" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
              '<circle cx="256" cy="176" r="24" fill="#fff6c8"/><circle cx="256" cy="176" r="10" fill="#ffd23f"/>'
              '<path d="M126 460L386 460L338 400H174Z" fill="#ffd23f"/>' + bars)


def bullet():
    wins = "".join(f'<rect x="{74 + k * 62}" y="272" width="44" height="36" rx="12" fill="{c}"/>'
                   for k, c in enumerate(AGENT + [AMBER]))
    speed = "".join(f'<path d="M{x1} {y}H{x2}" stroke="#fff" stroke-opacity=".35" stroke-width="8" '
                    'stroke-linecap="round"/>' for x1, x2, y in [(20, 120, 250), (50, 150, 320), (14, 90, 366)])
    return (tile("#0f1a2e") + '<g transform="translate(0 -36)">'
            + '<path d="M44 256H330Q430 256 484 336L484 380H44Z" fill="#f6f3ec"/>'
              '<path d="M44 336H484V352H44Z" fill="#4da3ff"/>' + wins
            + '<path d="M388 272H414Q440 290 456 318H388Z" fill="#1d2a45"/>'
              '<path d="M30 424H482" stroke="#5b6882" stroke-width="8"/>'
              '<circle cx="110" cy="398" r="16" fill="#2a3550"/><circle cx="250" cy="398" r="16" fill="#2a3550"/>'
              '<circle cx="390" cy="398" r="16" fill="#2a3550"/>' + speed + "</g>")


def junction():
    ys = (96, 176, 336, 416)
    out = [tile("#10151f")]
    for c, y in zip(AGENT, ys):
        d = f"M40 {y}C190 {y} 190 256 310 256H410"
        out.append(f'<path d="{d}" fill="none" stroke="{c}" stroke-width="40" stroke-dasharray="5 19"/>'
                   f'<path d="{d}" fill="none" stroke="{c}" stroke-width="26"/>'
                   f'<path d="{d}" fill="none" stroke="#10151f" stroke-width="12"/>'
                   f'<circle cx="40" cy="{y}" r="17" fill="#f4f7fb"/>')
    out.append('<circle cx="440" cy="256" r="40" fill="#ffd23f"/>'
               '<path d="M420 256L434 270L462 238" fill="none" stroke="#10151f" stroke-width="13" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def signal():
    lights = "".join(
        (f'<circle cx="256" cy="{y}" r="40" fill="{c}" filter="url(#glow)"/>' if on else "")
        + f'<circle cx="256" cy="{y}" r="30" fill="{c}" opacity="{1 if on else .22}"/>'
        for y, c, on in [(104, AMBER, 0), (174, BLUE, 0), (244, GREY, 0), (314, GREEN, 1)])
    return (tile("#1a2230") + '<rect x="238" y="360" width="36" height="90" fill="#5b6882"/>'
            '<rect x="196" y="440" width="120" height="24" rx="8" fill="#3a4560"/>'
            '<rect x="186" y="44" width="140" height="330" rx="44" fill="#0d121c" stroke="#3a4560" stroke-width="9"/>'
            + lights)


def board():
    out = [tile("#0c1018"), '<rect x="44" y="64" width="424" height="384" rx="30" fill="#11172a" '
           'stroke="#2a3558" stroke-width="8"/>']
    cols = [GREEN, GREEN, AMBER, BLUE, GREY]
    for r, c in enumerate(cols):
        y = 92 + r * 70
        out.append(f'<circle cx="86" cy="{y + 22}" r="14" fill="{c}"/>')
        for i in range(7):
            x = 122 + i * 38
            h = 8 + ((r * 7 + i * 5) % 3) * 8
            out.append(f'<rect x="{x}" y="{y}" width="32" height="44" rx="7" fill="#1f2940"/>'
                       f'<rect x="{x + 7}" y="{y + 22 - h / 2}" width="18" height="{h}" rx="3" fill="#cdd6e4"/>'
                       f'<path d="M{x} {y + 22}H{x + 32}" stroke="#11172a" stroke-width="3"/>')
        out.append(f'<path d="M426 {y + 8}L446 {y + 22}L426 {y + 36}" fill="none" stroke="{c}" stroke-width="10" '
                   'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def caboose():
    return (tile("#1c1b26")
            + '<path d="M30 424H482" stroke="#5b6882" stroke-width="8"/>'
              '<rect x="190" y="124" width="132" height="90" rx="14" fill="#b8302a"/>'
              '<rect x="206" y="142" width="40" height="46" rx="8" fill="#ffe27a"/>'
              '<rect x="266" y="142" width="40" height="46" rx="8" fill="#ffe27a"/>'
              '<rect x="96" y="200" width="320" height="170" rx="18" fill="#d63b2f"/>'
              '<rect x="80" y="188" width="352" height="20" rx="10" fill="#8f231e"/>'
              '<rect x="122" y="236" width="76" height="70" rx="12" fill="#0b1220"/>'
              '<rect x="314" y="236" width="76" height="70" rx="12" fill="#0b1220"/>'
              '<rect x="218" y="236" width="76" height="70" rx="12" fill="#0b1220"/>'
              '<path d="M238 254L258 270L238 286M266 288H280" fill="none" stroke="#3ddc84" stroke-width="9" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
              '<rect x="60" y="326" width="40" height="12" rx="6" fill="#8f231e"/>'
              '<rect x="412" y="326" width="40" height="12" rx="6" fill="#8f231e"/>'
              '<rect x="100" y="362" width="312" height="18" rx="8" fill="#2a2d3a"/>'
              '<circle cx="64" cy="266" r="20" fill="#ffb21f" filter="url(#glow)"/><circle cx="64" cy="266" r="12" fill="#ffd23f"/>'
              '<circle cx="176" cy="396" r="26" fill="#14151d" stroke="#9aa4b2" stroke-width="8"/>'
              '<circle cx="336" cy="396" r="26" fill="#14151d" stroke="#9aa4b2" stroke-width="8"/>')


def ticket():
    shape = ("M84 136H428Q452 136 452 160V228A28 28 0 0 0 452 284V352Q452 376 428 376H84Q60 376 60 352V284"
             "A28 28 0 0 0 60 228V160Q60 136 84 136Z")
    stripes = "".join(f'<rect x="{84 + k * 56}" y="158" width="46" height="16" rx="8" fill="{c}"/>'
                      for k, c in enumerate(AGENT))
    bars = "".join(f'<rect x="{362 + i * 10}" y="200" width="{4 + (i % 3) * 2}" height="112" fill="#14192b"/>'
                   for i in range(8))
    return (tile("#14192b") + f'<path d="{shape}" fill="#f6f3ec"/>' + stripes
            + '<path d="M334 150V362" stroke="#b9b09a" stroke-width="5" stroke-dasharray="3 12" stroke-linecap="round"/>'
              '<path d="M118 214L206 286L118 358" fill="none" stroke="#14192b" stroke-width="40" '
              'stroke-linecap="round" stroke-linejoin="round"/>'
              '<circle cx="262" cy="322" r="22" fill="#14192b"/>' + bars)


def tunnel():
    out = []
    cols = AGENT + ["#07090f"]
    for i in range(5):
        r, cy, bot = 200 - 38 * i, 270 - 14 * i, 480 - 22 * i
        out.append(f'<path d="M{256 - r} {bot}V{cy}A{r} {r} 0 0 1 {256 + r} {cy}V{bot}Z" fill="{cols[i]}"/>')
    out.append('<path d="M120 512L240 392M392 512L272 392" stroke="#9aa4b2" stroke-width="10" stroke-linecap="round"/>'
               '<circle cx="256" cy="226" r="40" fill="#fff6c8" filter="url(#glow)"/>'
               '<circle cx="256" cy="226" r="14" fill="#fff"/>')
    return '<rect width="512" height="512" rx="112" fill="#0a0d14"/>' + "".join(out)


def turntable():
    out = [tile("#121722"), '<circle cx="256" cy="256" r="196" fill="#1d2535" stroke="#3a4560" stroke-width="10"/>']
    for k in range(6):
        a = -90 + 60 * k
        x, y = 256 + 190 * math.cos(math.radians(a)), 256 + 190 * math.sin(math.radians(a))
        out.append(f'<path d="M256 256L{x:.1f} {y:.1f}" stroke="#5b6882" stroke-width="22" stroke-linecap="round"/>'
                   f'<path d="M256 256L{x:.1f} {y:.1f}" stroke="#1d2535" stroke-width="9" stroke-linecap="round"/>')
    out.append('<rect x="228" y="52" width="56" height="62" rx="14" fill="#ffd23f"/>'
               '<rect x="228" y="120" width="56" height="52" rx="12" fill="#4da3ff"/>'
               '<rect x="228" y="178" width="56" height="44" rx="12" fill="#3ddc84"/>'
               '<circle cx="256" cy="256" r="34" fill="#f6f3ec"/>'
               '<path d="M246 240L268 256L246 272" fill="none" stroke="#121722" stroke-width="10" '
               'stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def pixel_train():
    cells = {}

    def pg(x, y, w, h, c):
        for i in range(w):
            for j in range(h):
                cells[(x + i, y + j)] = c
    for x in range(38):
        pg(x, 15, 1, 1, "#3a4560")
    for k, c in enumerate(AGENT[:3]):
        x0 = k * 10
        pg(x0, 6, 9, 7, c)
        pg(x0 + 3, 8, 1, 1, "#0d1117"); pg(x0 + 4, 9, 1, 1, "#0d1117"); pg(x0 + 3, 10, 1, 1, "#0d1117")
        pg(x0 + 1, 13, 2, 2, "#9aa4b2"); pg(x0 + 6, 13, 2, 2, "#9aa4b2")
        pg(x0 + 9, 10, 1, 1, "#9aa4b2")
    pg(30, 7, 5, 6, "#ffd23f"); pg(35, 4, 3, 9, "#ffb21f"); pg(31, 4, 2, 3, "#9aa4b2")
    pg(36, 6, 1, 2, "#0d1117"); pg(31, 13, 2, 2, "#9aa4b2"); pg(35, 13, 2, 2, "#9aa4b2")
    pg(32, 2, 1, 1, "#f0f3f6"); pg(33, 0, 1, 1, "#9aa4b2")
    grid = "".join(f'<path d="M40 {y}H472" stroke="#fff" stroke-opacity=".04" stroke-width="2"/>'
                   for y in range(72, 473, 32))
    return tile("#0d1117") + grid + "".join(
        f'<rect x="{28 + x * 12}" y="{190 + y * 12}" width="10.5" height="10.5" rx="2" fill="{c}"/>'
        for (x, y), c in cells.items())


ICONS += [
    ("locomotive", "Locomotive", "The engine head-on, with a prompt on its smokebox door.", loco_front, "#1b2230", ("#f6f3ec", "#ffd23f")),
    ("bullet-train", "Bullet Train", "Fast and streamlined: four agents at the windows, no stops.", bullet, "#0f1a2e", ("#f6f3ec", "#4da3ff")),
    ("junction", "Junction", "Four tracks merge into one, ending in a green check: a merge, drawn as rail.", junction, "#10151f", ("#f4f7fb", "#ffd23f")),
    ("signal", "Signal", "The merge queue's signal: only the green light is on.", clipped(signal), "#1a2230", ("#f0f3f6", "#3ddc84")),
    ("departures", "Departures", "The departures board: every change, its status, and when it leaves.", board, "#0c1018", ("#e6edf3", "#3ddc84")),
    ("caboose", "Caboose", "Last car on the train, with a prompt in its window.", caboose, "#1c1b26", ("#f6f3ec", "#d63b2f")),
    ("ticket", "Ticket", "A ticket to main, punched and perforated, one stripe per agent.", ticket, "#14192b", ("#f6f3ec", "#ff7a45")),
    ("tunnel", "Tunnel", "Through the tunnel: colour rings, rails and a headlight.", clipped(tunnel), "#07090f", ("#f4f7fb", "#b57bff")),
    ("turntable", "Turntable", "The roundhouse: six tracks, one train lined up to leave.", turntable, "#121722", ("#f0f3f6", "#ffd23f")),
    ("pixel-train", "Pixel Train", "The merge train in terminal cells, chevron on every car.", pixel_train, "#0d1117", ("#f0f3f6", "#ffd23f")),
]


# ── batch five: pterodactyls ─────────────────────────────────────────────────────────────────

_HALF = [(256, 52), (272, 112), (282, 176), (448, 124), (408, 236), (372, 212), (346, 304),
         (302, 270), (286, 336), (276, 420), (256, 474)]
HEAD_ON = [*_HALF, *[(512 - x, y) for x, y in reversed(_HALF[1:-1])]]
HEAD_ON_D = "M" + "L".join(f"{x} {y}" for x, y in HEAD_ON) + "Z"
SIDE_BODY = ("M474 216L400 192L330 168L366 216L346 246L318 268C300 288 262 296 230 300L150 322L236 332"
             "C300 336 340 310 362 276L384 240L400 228Z")
SIDE_WING = "M285 268L215 170L112 84L150 178L172 168L190 236L214 226L228 280Z"
SIDE_WING2 = "M296 296L286 410L258 380L246 396L240 330Z"


def head_on(cx, cy, k, fill, stroke="none", sw=0):
    return (f'<path d="{HEAD_ON_D}" fill="{fill}" stroke="{stroke}" stroke-width="{sw}" stroke-linejoin="round" '
            f'transform="translate({cx - 256 * k:.1f} {cy - 263 * k:.1f}) scale({k})"/>')


def side(cx, cy, k, fill, back, eye="#fff"):
    return (f'<g transform="translate({cx - 292 * k:.1f} {cy - 247 * k:.1f}) scale({k})">'
            f'<path d="{SIDE_WING2}" fill="{back}"/><path d="{SIDE_BODY}" fill="{fill}"/>'
            f'<path d="{SIDE_WING}" fill="{fill}" stroke="{back}" stroke-width="6" stroke-linejoin="round"/>'
            f'<circle cx="397" cy="207" r="7" fill="{eye}"/></g>')


def ptero_flight():
    clouds = "".join(f'<ellipse cx="{x}" cy="{y}" rx="{w}" ry="{h}" fill="#fff" opacity=".85"/>'
                     for x, y, w, h in [(110, 400, 70, 22), (160, 416, 56, 18), (400, 96, 60, 18), (440, 112, 44, 14)])
    return ('<defs><linearGradient id="dy" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#7cc6ff"/>'
            '<stop offset="1" stop-color="#ffe3b8"/></linearGradient></defs>'
            '<rect width="512" height="512" rx="112" fill="url(#dy)"/>' + clouds
            + side(262, 250, 1.0, "#1c2a4a", "#3b4d7a", "#fff"))


def ptero_pixel():
    rows = ["....B...........", "....BB..........", "....BBB.........", "....BBBB........",
            "....BBBBB...B...", ".....BBBBB.BBBBK", "......BBBBBBEBKK", ".....DDBBBBBBK..",
            ".....DDDBBBB....", "......DDDB......", ".....DD..DD....."]
    cols = {"B": "#3ddc84", "D": "#1f9d5c", "K": "#ffb21f", "E": "#0d1117"}
    grid = "".join(f'<path d="M40 {y}H472" stroke="#fff" stroke-opacity=".04" stroke-width="2"/>'
                   for y in range(72, 473, 32))
    return tile("#0d1117") + grid + pixels(rows, cols, 24, 64, 126, 3)


def egg():
    shell = ("M120 340L168 296L212 340L256 292L300 340L344 296L392 340C392 420 332 468 256 468C180 468 120 420 120 340Z")
    spots = "".join(f'<circle cx="{x}" cy="{y}" r="{r}" fill="#d9d0b8"/>' for x, y, r in
                    [(190, 400, 16), (300, 420, 12), (250, 370, 9), (340, 380, 14)])
    return (tile("#1b2a3a")
            + '<path d="M210 176L134 100L250 150Z" fill="#ff9f80"/>'
              '<path d="M256 150L256 150" />'
              '<circle cx="256" cy="236" r="84" fill="#7bdcb5"/>'
              '<path d="M310 216L398 244L310 270Z" fill="#ffb21f"/>'
              '<circle cx="272" cy="214" r="20" fill="#fff"/><circle cx="278" cy="216" r="10" fill="#1b2a3a"/>'
              '<path d="M166 306L116 250L196 282Z M346 306L396 250L316 282Z" fill="#5cc79c"/>'
              f'<path d="{shell}" fill="#f6f3ec"/>' + spots
              + '<path d="M92 190L132 150L150 200Z M420 180L384 144L362 196Z" fill="#f6f3ec" opacity=".9"/>')


def fossil():
    dark = "#5d503a"
    ribs = "".join(f'<path d="M232 {y}Q256 {y + 12} 280 {y}" fill="none" stroke="{dark}" stroke-width="7" stroke-linecap="round"/>'
                   for y in range(240, 380, 22))
    wing = lambda m: (f'<g transform="{m}" fill="none" stroke="{dark}" stroke-linecap="round" stroke-linejoin="round">'
                      '<path d="M242 214L118 150L66 98" stroke-width="15"/>'
                      '<path d="M66 98L70 250M66 98L112 304M66 98L172 336M66 98L226 296" stroke-width="7"/>'
                      '<circle cx="66" cy="98" r="11" fill="#5d503a" stroke="none"/></g>')
    specks = "".join(f'<circle cx="{x}" cy="{y}" r="3" fill="#9c8d6c"/>' for x, y in
                     [(60, 440), (150, 70), (440, 60), (470, 330), (420, 460), (310, 70), (50, 330)])
    return ('<defs><linearGradient id="st" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#cdbf9f"/>'
            '<stop offset="1" stop-color="#a69676"/></linearGradient></defs>'
            '<rect width="512" height="512" rx="112" fill="url(#st)"/>' + specks
            + f'<rect x="22" y="22" width="468" height="468" rx="92" fill="none" stroke="{dark}" stroke-width="5" opacity=".4"/>'
            + wing("") + wing("translate(512 0) scale(-1 1)") + ribs
            + f'<path d="M256 214V412" stroke="{dark}" stroke-width="12" stroke-linecap="round"/>'
              f'<ellipse cx="256" cy="128" rx="24" ry="38" fill="none" stroke="{dark}" stroke-width="10"/>'
              f'<path d="M256 90V30M256 412V462" stroke="{dark}" stroke-width="9" stroke-linecap="round"/>'
              f'<path d="M256 166V214" stroke="{dark}" stroke-width="12" stroke-linecap="round"/>')


def lowpoly():
    pal = ["#2ec4b6", "#20a39e", "#3a86ff", "#8338ec", "#06d6a0", "#3d5a80"]
    hub = (256, 250)
    tris = "".join(f'<path d="M{hub[0]} {hub[1]}L{a[0]} {a[1]}L{b[0]} {b[1]}Z" fill="{pal[(i * 5 + i // 3) % 6]}" '
                   f'stroke="#0b132b" stroke-width="3" stroke-linejoin="round"/>'
                   for i, (a, b) in enumerate(zip(HEAD_ON, HEAD_ON[1:] + HEAD_ON[:1])))
    return tile("#0b132b") + tris + '<circle cx="256" cy="108" r="9" fill="#0b132b"/>'


def baby():
    return (tile("#ffe8d6")
            + '<path d="M214 130L256 34L298 130Z" fill="#ff9f80"/><path d="M168 150L178 78L226 128Z" fill="#ffb59c"/>'
              '<path d="M344 150L334 78L286 128Z" fill="#ffb59c"/>'
              '<ellipse cx="74" cy="320" rx="52" ry="26" fill="#5cc79c" transform="rotate(-25 74 320)"/>'
              '<ellipse cx="438" cy="320" rx="52" ry="26" fill="#5cc79c" transform="rotate(25 438 320)"/>'
              '<circle cx="256" cy="282" r="162" fill="#7bdcb5"/>'
              '<circle cx="190" cy="262" r="42" fill="#fff"/><circle cx="322" cy="262" r="42" fill="#fff"/>'
              '<circle cx="196" cy="268" r="22" fill="#2b2230"/><circle cx="316" cy="268" r="22" fill="#2b2230"/>'
              '<circle cx="204" cy="258" r="8" fill="#fff"/><circle cx="324" cy="258" r="8" fill="#fff"/>'
              '<circle cx="132" cy="334" r="22" fill="#ff9fb4" opacity=".7"/><circle cx="380" cy="334" r="22" fill="#ff9fb4" opacity=".7"/>'
              '<rect x="212" y="316" width="88" height="62" rx="31" fill="#ffb21f"/>'
              '<circle cx="240" cy="338" r="6" fill="#c97a00"/><circle cx="272" cy="338" r="6" fill="#c97a00"/>')


def chevron_wings():
    out = [tile("#0f1424")]
    for k, x in enumerate((150, 92, 34)):
        out.append(f'<path d="M{x + 66} 156L{x} 256L{x + 66} 356" fill="none" stroke="{AGENT[k]}" stroke-width="34" '
                   'stroke-linecap="round" stroke-linejoin="round"/>'
                   f'<path d="M{512 - x - 66} 156L{512 - x} 256L{512 - x - 66} 356" fill="none" stroke="{AGENT[k]}" '
                   'stroke-width="34" stroke-linecap="round" stroke-linejoin="round"/>')
    out.append('<path d="M256 78L232 164H280Z" fill="#ffb21f"/><circle cx="256" cy="198" r="38" fill="#f6f3ec"/>'
               '<ellipse cx="256" cy="304" rx="34" ry="92" fill="#f6f3ec"/>'
               '<path d="M256 380V450" stroke="#f6f3ec" stroke-width="14" stroke-linecap="round"/>'
               '<circle cx="268" cy="192" r="7" fill="#0f1424"/>')
    return "".join(out)


def sunset():
    stripes = "".join(f'<rect x="120" y="{y}" width="272" height="{h}" fill="#2b0f54"/>'
                      for y, h in [(310, 6), (336, 9), (362, 13), (390, 16)])
    return ('<defs><linearGradient id="sn" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2b0f54"/>'
            '<stop offset=".55" stop-color="#ff5d8f"/><stop offset="1" stop-color="#ffb347"/></linearGradient>'
            '<linearGradient id="sun" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe066"/>'
            '<stop offset="1" stop-color="#ff4d8d"/></linearGradient></defs>'
            '<rect width="512" height="512" fill="url(#sn)"/>'
            '<circle cx="256" cy="320" r="136" fill="url(#sun)"/>' + stripes
            + '<path d="M0 420L90 330L160 400L250 300L350 410L430 340L512 420V512H0Z" fill="#1a0b2e"/>'
            + side(256, 168, .62, "#1a0b2e", "#2b0f54", "#ffe066")
            + '<circle cx="80" cy="70" r="3" fill="#fff"/><circle cx="440" cy="110" r="3" fill="#fff"/>'
              '<circle cx="400" cy="50" r="2" fill="#fff"/>')


def circuit():
    cy = "#22d3ee"
    traces = ('<path d="M298 190H352L384 158H424M292 240H362L396 206M288 292H340L362 268H392M300 340H340L360 360" '
              f'fill="none" stroke="{cy}" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>'
              + "".join(f'<circle cx="{x}" cy="{y}" r="9" fill="#0d1b2a" stroke="{cy}" stroke-width="5"/>'
                        for x, y in [(424, 158), (396, 206), (392, 268), (360, 360)]))
    return (tile("#07121f") + head_on(256, 263, 1.0, "#0d1b2a", cy, 8) + traces
            + f'<g transform="translate(512 0) scale(-1 1)">{traces}</g>'
            + f'<path d="M256 150V440" stroke="{cy}" stroke-width="7" stroke-linecap="round"/>'
              f'<circle cx="256" cy="108" r="10" fill="{cy}"/>')


def pack():
    out = ['<rect width="512" height="512" rx="112" fill="url(#sky)"/>', head_on(256, 164, .6, "#ffd23f")]
    for i, (x, y, k) in enumerate([(120, 300, .36), (392, 300, .36), (62, 410, .24), (450, 410, .24)]):
        out.append(head_on(x, y, k, AGENT[i % 4]))
    return "".join(out)


ICONS += [
    ("ptero-flight", "Ptero in Flight", "A pterodactyl gliding past the clouds, wing raised.", ptero_flight, "#16284d", ("#1c2a4a", "#3a86ff")),
    ("ptero-pixel", "8-Bit Ptero", "A pixel pterodactyl, a nod to the one in the offline-dinosaur game.", ptero_pixel, "#0d1117", ("#f0f3f6", "#3ddc84")),
    ("hatchling", "Hatchling", "A new session hatches: a baby pterodactyl cracking out of its egg.", egg, "#1b2a3a", ("#f6f3ec", "#7bdcb5")),
    ("fossil", "Fossil", "The wing bones in stone: old, but still resumable.", fossil, "#cdbf9f", ("#4a3f2c", "#8a6f3e")),
    ("low-poly", "Low Poly", "Faceted, in teal and violet.", lowpoly, "#0b132b", ("#e9f7ff", "#06d6a0")),
    ("baby-ptero", "Baby Ptero", "The friendly mascot: big eyes, tiny wings, rosy cheeks.", baby, "#ffe8d6", ("#2b2230", "#ff9f80")),
    ("chevron-wings", "Chevron Wings", "Wings made of `<` and `>`: the terminal takes flight.", chevron_wings, "#0f1424", ("#f6f3ec", "#ffb21f")),
    ("sunset-ptero", "Sunset", "A pterodactyl across a retro sunset, mountains below.", clipped(sunset), "#2b0f54", ("#fff1c9", "#ff5d8f")),
    ("circuit-ptero", "Circuit", "Circuit-board wings: part reptile, part machine.", circuit, "#07121f", ("#d6faff", "#22d3ee")),
    ("pack", "The Pack", "Five pterodactyls in formation: a lead and four followers.", pack, "#080c20", ("#ffffff", "#ffd23f")),
]


# ── batch six: riffs on the seven favourites, built to read from across a room ───────────────
# Rules: few shapes, thick strokes, strong contrast. Each riff is numbered from 51 and written to ideas/.

CREAM = "#f6f3ec"
SUN_DEFS = ('<defs><linearGradient id="sn" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2b0f54"/>'
            '<stop offset=".55" stop-color="#ff5d8f"/><stop offset="1" stop-color="#ffb347"/></linearGradient>'
            '<linearGradient id="sun" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe066"/>'
            '<stop offset="1" stop-color="#ff4d8d"/></linearGradient></defs>')


def pol(cx, cy, r, a):
    return cx + r * math.cos(math.radians(a)), cy + r * math.sin(math.radians(a))


def ln(x1, y1, x2, y2, col, w):
    return (f'<path d="M{x1:.1f} {y1:.1f}L{x2:.1f} {y2:.1f}" stroke="{col}" stroke-width="{w}" '
            'stroke-linecap="round"/>')


def chev(x, y, w, h, col, sw):
    return (f'<path d="M{x} {y}L{x + w} {y + h / 2}L{x} {y + h}" fill="none" stroke="{col}" stroke-width="{sw}" '
            'stroke-linecap="round" stroke-linejoin="round"/>')


def dot(x, y, r, fill, stroke="none", sw=0):
    return f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{fill}" stroke="{stroke}" stroke-width="{sw}"/>'


def lerp(c1, c2, t):
    a = [int(c1[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(c2[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(a, b))


# -- riffs on 05 Fireworks ------------------------------------------------------------------

FWB = "#0b0f2a"


def fw_mono():
    out = [tile(FWB)]
    for k in range(8):
        x1, y1 = pol(256, 256, 98, k * 45 + 22.5)
        x2, y2 = pol(256, 256, 216, k * 45 + 22.5)
        out.append(ln(x1, y1, x2, y2, "#fff", 32))
    return "".join(out) + chev(216, 194, 80, 124, "#ffd23f", 40)


def fw_starburst():
    out = [tile(FWB)]
    for k in range(16):
        x1, y1 = pol(256, 256, 80, k * 22.5)
        x2, y2 = pol(256, 256, 214 if k % 2 == 0 else 158, k * 22.5)
        out.append(ln(x1, y1, x2, y2, AGENT[k % 4], 26))
    return "".join(out) + dot(256, 256, 58, "#ffd23f", FWB, 8)


def fw_pinwheel():
    out = [tile(FWB)]
    for k in range(8):
        a = k * 45
        (x0, y0), (x1, y1), (x2, y2) = pol(256, 256, 70, a), pol(256, 256, 150, a + 32), pol(256, 256, 214, a + 6)
        out.append(f'<path d="M{x0:.1f} {y0:.1f}Q{x1:.1f} {y1:.1f} {x2:.1f} {y2:.1f}" fill="none" '
                   f'stroke="{AGENT[k % 4]}" stroke-width="28" stroke-linecap="round"/>')
    return "".join(out) + dot(256, 256, 44, "#fff")


def fw_asterisk():
    out = [tile(FWB)]
    for k in range(4):
        x1, y1 = pol(256, 256, 206, k * 45)
        x2, y2 = pol(256, 256, 206, k * 45 + 180)
        out.append(ln(x1, y1, x2, y2, AGENT[k], 40))
    return ("".join(out) + dot(256, 256, 70, FWB, "#fff", 10) + chev(238, 224, 40, 64, "#fff", 18))


def fw_launch():
    out = [tile(FWB)]
    for k, a in enumerate((-60, -40, -20, 0, 20, 40, 60)):
        x1, y1 = pol(296, 256, 56, a)
        x2, y2 = pol(296, 256, 128, a)
        x3, y3 = pol(296, 256, 168, a)
        out += [ln(x1, y1, x2, y2, AGENT[k % 4], 20), dot(x3, y3, 14, AGENT[k % 4])]
    return "".join(out) + chev(76, 130, 184, 252, "#fff", 66)


def fw_trio():
    def burst(cx, cy, r, col, n=8):
        o = []
        for k in range(n):
            a = k * 360 / n + 11
            x1, y1 = pol(cx, cy, r * .38, a)
            x2, y2 = pol(cx, cy, r, a)
            o.append(ln(x1, y1, x2, y2, col, max(10, r * .17)))
        return "".join(o) + dot(cx, cy, r * .2, col)
    trails = "".join(f'<path d="M{x} 500V{y}" stroke="#fff" stroke-opacity=".25" stroke-width="8" stroke-linecap="round" '
                     'stroke-dasharray="2 18"/>' for x, y in [(140, 240), (380, 210), (262, 440)])
    return (tile(FWB) + trails + burst(140, 160, 82, AGENT[0]) + burst(384, 128, 70, AGENT[1])
            + burst(256, 336, 124, AGENT[2], 10) + dot(256, 336, 26, "#fff"))


def fw_sparkle():
    star = "M256 28C264 168 344 248 484 256C344 264 264 344 256 484C248 344 168 264 28 256C168 248 248 168 256 28Z"
    return (tile(FWB) + f'<path d="{star}" fill="#ffd23f"/>'
            + "".join(dot(x, y, 22, AGENT[i]) for i, (x, y) in enumerate([(112, 112), (400, 112), (400, 400), (112, 400)]))
            + dot(256, 256, 26, FWB))


def fw_shock():
    out = [tile(FWB)]
    for i, r in enumerate((212, 160, 108)):
        out.append(f'<circle cx="256" cy="256" r="{r}" fill="none" stroke="{AGENT[i]}" stroke-width="30" '
                   f'stroke-linecap="round" stroke-dasharray="{r * .6:.0f} {r * .45:.0f}" '
                   f'transform="rotate({i * 40} 256 256)"/>')
    return "".join(out) + dot(256, 256, 48, "#fff")


def fw_confetti():
    out = [tile(FWB)]
    for k in range(10):
        x, y = pol(256, 256, 190, k * 36)
        out.append(f'<rect x="-22" y="-22" width="44" height="44" rx="10" fill="{AGENT[k % 4]}" '
                   f'transform="translate({x:.1f} {y:.1f}) rotate({k * 36})"/>')
    for k in range(10):
        x, y = pol(256, 256, 118, k * 36 + 18)
        out.append(f'<rect x="-13" y="-30" width="26" height="60" rx="9" fill="{AGENT[(k + 2) % 4]}" '
                   f'transform="translate({x:.1f} {y:.1f}) rotate({k * 36 + 108})"/>')
    return "".join(out) + chev(232, 212, 52, 88, "#fff", 24)


def fw_light():
    out = [tile(CREAM)]
    for k in range(8):
        x1, y1 = pol(256, 256, 100, k * 45 + 22.5)
        x2, y2 = pol(256, 256, 188, k * 45 + 22.5)
        x3, y3 = pol(256, 256, 226, k * 45 + 22.5)
        out += [ln(x1, y1, x2, y2, "#1c2040", 30), dot(x3, y3, 17, AGENT[k % 4])]
    return "".join(out) + dot(256, 256, 70, "#1c2040") + chev(238, 222, 44, 68, CREAM, 20)


# -- riffs on 22 Hub ----------------------------------------------------------------------

HB = "#101426"
GOLD = "#ffd23f"


def hub_core(cx, cy, r, bg=HB):
    s = r / 68
    return (dot(cx, cy, r, GOLD, bg, 8) + chev(cx - 20 * s, cy - 28 * s, 36 * s, 56 * s, bg, 20 * s))


def hub_four():
    out = [tile(HB)]
    pts = [pol(256, 256, 156, 45 + 90 * k) for k in range(4)]
    out += [ln(256, 256, x, y, "#46568c", 20) for x, y in pts]
    out += [dot(x, y, 62, AGENT[k], HB, 8) for k, (x, y) in enumerate(pts)]
    return "".join(out) + hub_core(256, 256, 84)


def hub_crown():
    out = [tile(HB)]
    pts = [pol(256, 292, 168, a) for a in (0, 45, 90, 135, 180)]
    out += [ln(256, 292, x, y, "#46568c", 14) for x, y in pts]
    out += [dot(x, y, 36, AGENT[k % 4], HB, 7) for k, (x, y) in enumerate(pts)]
    out.append(dot(256, 292, 62, GOLD, HB, 8) + chev(240, 270, 28, 44, HB, 16))
    out.append(f'<path d="M196 182L208 106L238 142L256 92L274 142L304 106L316 182Z" fill="{GOLD}" '
               'stroke="#101426" stroke-width="8" stroke-linejoin="round"/>')
    return "".join(out)


def hub_tri():
    out = [tile(HB)]
    pts = [pol(256, 268, 160, a) for a in (-90, 30, 150)]
    out += [ln(256, 268, x, y, "#46568c", 24) for x, y in pts]
    out += [dot(x, y, 66, AGENT[k], HB, 8) for k, (x, y) in enumerate(pts)]
    return "".join(out) + hub_core(256, 268, 80)


def hub_hex():
    def hexp(cx, cy, r):
        return "M" + "L".join(f"{cx + r * math.cos(math.radians(60 * k - 30)):.1f} "
                              f"{cy + r * math.sin(math.radians(60 * k - 30)):.1f}" for k in range(6)) + "Z"
    out = [tile(HB)]
    pts = [pol(256, 256, 160, 60 * k) for k in range(6)]
    out += [ln(256, 256, x, y, "#46568c", 16) for x, y in pts]
    out += [f'<path d="{hexp(x, y, 52)}" fill="{AGENT[k % 4] if k < 4 else (GREEN if k == 4 else AMBER)}" '
            f'stroke="{HB}" stroke-width="8" stroke-linejoin="round"/>' for k, (x, y) in enumerate(pts)]
    out.append(f'<path d="{hexp(256, 256, 80)}" fill="{GOLD}" stroke="{HB}" stroke-width="8" stroke-linejoin="round"/>')
    return "".join(out) + chev(238, 226, 36, 60, HB, 18)


def hub_helm():
    out = [tile(HB), '<circle cx="256" cy="256" r="150" fill="none" stroke="#46568c" stroke-width="26"/>']
    for k in range(8):
        x, y = pol(256, 256, 150, k * 45)
        out.append(ln(256, 256, x, y, "#46568c", 20))
    for k in range(4):
        x1, y1 = pol(256, 256, 150, k * 90 + 45)
        x2, y2 = pol(256, 256, 206, k * 90 + 45)
        out += [ln(x1, y1, x2, y2, AGENT[k], 24), dot(x2, y2, 30, AGENT[k], HB, 6)]
    return "".join(out) + hub_core(256, 256, 62)


def hub_bracket():
    out = [tile(HB)]
    ys = (96, 192, 320, 416)
    out.append('<path d="M240 256H322" stroke="#46568c" stroke-width="16" stroke-linecap="round"/>'
               '<path d="M322 96V416" stroke="#46568c" stroke-width="16" stroke-linecap="round"/>')
    out += [ln(322, y, 380, y, "#46568c", 16) + dot(412, y, 38, AGENT[k], HB, 7) for k, y in enumerate(ys)]
    return "".join(out) + hub_core(150, 256, 98)


def hub_star():
    out = [tile(HB)]
    pts = [pol(256, 256, 170 if k % 2 == 0 else 120, k * 45) for k in range(8)]
    out += [ln(256, 256, x, y, "#46568c", 12) for x, y in pts]
    out += [dot(x, y, 38 if k % 2 == 0 else 28, AGENT[k % 4], HB, 7) for k, (x, y) in enumerate(pts)]
    return "".join(out) + dot(256, 256, 58, "#f4f7fb", HB, 8)


def hub_corners():
    out = [tile(HB)]
    pts = [(116, 116), (396, 116), (116, 396), (396, 396)]
    out += [ln(256, 256, x, y, "#46568c", 22) for x, y in pts]
    out += [dot(x, y, 62, AGENT[k], HB, 8) for k, (x, y) in enumerate(pts)]
    out.append(f'<rect x="186" y="186" width="140" height="140" rx="40" fill="{GOLD}" stroke="{HB}" stroke-width="8"/>')
    return "".join(out) + chev(236, 224, 40, 64, HB, 20)


def hub_tree():
    out = [tile(HB), f'<path d="M256 170V270M110 330V270H402V330M256 270V330" fill="none" stroke="#46568c" '
           'stroke-width="18" stroke-linecap="round" stroke-linejoin="round"/>']
    out += [dot(x, 390, 62, AGENT[k], HB, 8) for k, x in enumerate((110, 256, 402))]
    return "".join(out) + hub_core(256, 112, 74)


def hub_ring():
    out = [tile(HB)]
    sizes = [26, 44, 32, 48, 28, 40]
    pts = [pol(256, 256, 178, 60 * k + 30) for k in range(6)]
    out += [ln(256, 256, x, y, "#46568c", 10) for x, y in pts]
    out += [dot(x, y, sizes[k], AGENT[k % 4] if k < 4 else (GREEN if k == 4 else AMBER), HB, 6)
            for k, (x, y) in enumerate(pts)]
    return ("".join(out) + dot(256, 256, 78, HB, "#f4f7fb", 26) + chev(238, 226, 36, 60, "#f4f7fb", 18))


# -- riffs on 23 Chain ---------------------------------------------------------------------

CH = "#12161f"


def chain_3():
    out = [tile(CH)]
    for k, x in enumerate((64, 194, 324)):
        out.append(chev(x, 110, 130, 292, AGENT[k], 84))
    return "".join(out)


def chain_5():
    cols = [AGENT[0], AMBER, GREEN, AGENT[2], AGENT[3]]
    return tile(CH) + "".join(chev(40 + 76 * k, 150, 70, 212, cols[k], 38) for k in range(5))


def chain_ring():
    out = [tile(CH)]
    for k in range(4):
        out.append(f'<g transform="rotate({k * 90} 256 256)">{chev(228, 52, 58, 96, AGENT[k], 44)}</g>')
    return "".join(out) + dot(256, 256, 40, "#f4f7fb")


def chain_cursor():
    out = [tile(CH)]
    out += [chev(40 + 100 * k, 150, 80, 212, AGENT[k], 48) for k in range(3)]
    return "".join(out) + '<rect x="362" y="302" width="96" height="62" rx="12" fill="#f4f7fb"/>'


def chain_nested():
    out = [tile(CH)]
    for i in range(4):
        h = 200 - 36 * i
        tip = 450 - 66 * i
        out.append(chev(tip - 1.1 * h, 256 - h, 1.1 * h, 2 * h, AGENT[i], 40))
    return "".join(out)


def chain_flag():
    out = [tile(CH)]
    for k in range(4):
        x = 56 + 52 * k
        out.append(f'<path d="M{x} 86L{x + 180} 256L{x} 426" fill="none" stroke="{AGENT[k]}" stroke-width="52" '
                   'stroke-linejoin="miter" stroke-miterlimit="10"/>')
    return "".join(out)


def chain_ff():
    out = [tile(CH)]
    for k, x in enumerate((50, 144, 238, 332)):
        out.append(f'<path d="M{x} 150L{x + 86} 256L{x} 362Z" fill="{AGENT[k]}" stroke="{AGENT[k]}" '
                   'stroke-width="28" stroke-linejoin="round"/>')
    return "".join(out)


def chain_queue():
    out = [tile(CH)]
    out += [dot(x, 256, r, AGENT[k], CH, 6) for k, (x, r) in enumerate([(70, 26), (150, 34), (244, 44)])]
    return "".join(out) + chev(312, 126, 130, 260, "#f4f7fb", 70)


def chain_pill():
    out = [tile(CH), '<rect x="36" y="166" width="440" height="180" rx="90" fill="#1f2638"/>']
    out += [chev(86 + 92 * k, 206, 54, 100, AGENT[k], 30) for k in range(4)]
    return "".join(out)


def chain_stair():
    out = [tile(CH)]
    out += [chev(40 + 96 * k, 246 - 56 * k, 76, 160, AGENT[k], 46) for k in range(4)]
    return "".join(out)


# -- riffs on 25 Squad ----------------------------------------------------------------------

SQB = "#151a2e"
CRAB = ["..X.....X..", "...X...X...", "..XXXXXXX..", ".XX.XXX.XX.", "XXXXXXXXXXX", "X.XXXXXXX.X",
        "X.X.....X.X", "...XX.XX..."]
SQUID = ["...XX...", "..XXXX..", ".XXXXXX.", "XX.XX.XX", "XXXXXXXX", "..X..X..", ".X.XX.X.", "X.X..X.X"]
OCTO = ["....XXXX....", ".XXXXXXXXXX.", "XXXXXXXXXXXX", "XXX..XX..XXX", "XXXXXXXXXXXX", "..XXX..XXX..",
        ".XX..XX..XX.", "..XX....XX.."]
BOT = ["...A...", ".HHHHH.", "HHHHHHH", "HKHHHKH", "HHHHHHH", ".H.H.H."]
GHOST = ["...XXXX...", "..XXXXXX..", ".XXXXXXXX.", ".XWWXXWWX.", ".XWKXXWKX.", "XXXXXXXXXX",
         "XXXXXXXXXX", "XXXXXXXXXX", "XX.XXXX.XX", "X...XX...X"]


def spr(rows, col, cell, ox=None, oy=None, gap=3, extra=None):
    ox = (512 - len(rows[0]) * cell) // 2 if ox is None else ox
    oy = (512 - len(rows) * cell) // 2 if oy is None else oy
    cols = {"X": col, "W": "#fff", "K": SQB, **(extra or {})}
    return pixels(rows, cols, cell, ox, oy, gap)


def quad(rows):
    h, w = len(rows), len(rows[0])
    return ["".join("abcd"[(1 if x >= w / 2 else 0) + (2 if y >= h / 2 else 0)] if ch == "X" else ch
                    for x, ch in enumerate(r)) for y, r in enumerate(rows)]


def squad_one():
    return tile(SQB) + spr(CRAB, AGENT[0], 38, gap=4)


def squad_types():
    return (tile(SQB) + spr(SQUID, BLUE, 14, oy=52) + spr(CRAB, GREEN, 14, oy=198) + spr(OCTO, AGENT[0], 14, oy=344))


def squad_grid():
    out = [tile(SQB)]
    for r in range(3):
        for c in range(3):
            col = GOLD if (r, c) == (0, 0) else "#6b82c4"
            out.append(spr(CRAB, col, 12, ox=38 + c * 152, oy=88 + r * 120, gap=2))
    return "".join(out)


def squad_screen():
    return (tile(SQB) + spr(CRAB, AGENT[0], 34, oy=96)
            + f'<rect x="168" y="212" width="176" height="76" rx="14" fill="{SQB}"/>'
            + chev(190, 228, 28, 44, GREEN, 13) + '<path d="M234 272H294" stroke="#3ddc84" stroke-width="13" stroke-linecap="round"/>')


def squad_bot():
    return tile(SQB) + spr(BOT, BLUE, 56, gap=4, extra={"A": GOLD, "H": BLUE})


def squad_laser():
    return (tile(SQB) + spr(CRAB, GREEN, 22, oy=62)
            + '<rect x="250" y="262" width="12" height="150" rx="6" fill="#fff"/>'
              f'<path d="M226 456H286L274 424H238Z" fill="{GOLD}"/><rect x="250" y="404" width="12" height="24" fill="{GOLD}"/>'
            + dot(256, 262, 12, "#fff"))


def squad_v():
    out = [tile(SQB)]
    for k, (x, y) in enumerate([(256, 150), (170, 240), (342, 240), (84, 330), (428, 330)]):
        cell = 14 if k == 0 else 11
        out.append(spr(SQUID, GOLD if k == 0 else AGENT[k % 4], cell, ox=int(x - 4 * cell), oy=int(y - 4 * cell), gap=2))
    return "".join(out)


def squad_quad():
    q = quad(CRAB)
    return tile(SQB) + spr(q, "#fff", 38, gap=4, extra={"a": AGENT[0], "b": AGENT[1], "c": AGENT[2], "d": AGENT[3]})


def squad_window():
    return (tile(SQB) + '<rect x="48" y="64" width="416" height="384" rx="36" fill="#0d1220" stroke="#4a5873" stroke-width="10"/>'
            '<path d="M53 128H459V100Q459 69 428 69H84Q53 69 53 100Z" fill="#2a3550"/>'
            + dot(96, 98, 10, AMBER) + dot(130, 98, 10, GREEN) + dot(164, 98, 10, "#9aa4b2")
            + spr(CRAB, GREEN, 24, oy=198))


def squad_jelly():
    out = [tile(SQB)]
    for k, (x, y) in enumerate([(62, 62), (270, 62), (62, 270), (270, 270)]):
        out.append(spr(GHOST, AGENT[k], 18, ox=x, oy=y, gap=2))
    return "".join(out)


# -- riffs on 28 Bento ---------------------------------------------------------------------

BN = "#0a0e17"
PANE = "#161d2c"


def pane(x, y, w, h, c, sw=12, fill=PANE):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="26" fill="{fill}" stroke="{c}" stroke-width="{sw}"/>'


def bar(x, y, w, col="#4a5873", sw=12):
    return f'<path d="M{x} {y}H{x + w}" stroke="{col}" stroke-width="{sw}" stroke-linecap="round"/>'


def check(cx, cy, k, col):
    return (f'<path d="M{cx - 24 * k} {cy}L{cx - 6 * k} {cy + 20 * k}L{cx + 26 * k} {cy - 22 * k}" fill="none" '
            f'stroke="{col}" stroke-width="{16 * k}" stroke-linecap="round" stroke-linejoin="round"/>')


def bang(cx, cy, k, col):
    return (f'<path d="M{cx} {cy - 34 * k}V{cy + 12 * k}" stroke="{col}" stroke-width="{20 * k}" stroke-linecap="round"/>'
            + dot(cx, cy + 40 * k, 11 * k, col))


def bento_3():
    return (tile(BN) + pane(56, 56, 260, 400, BLUE, 18) + chev(120, 190, 100, 132, "#fff", 30)
            + pane(336, 56, 120, 190, AMBER, 14) + bang(396, 144, 1, AMBER)
            + pane(336, 266, 120, 190, GREEN, 14) + check(396, 362, 1.1, GREEN))


def bento_strip():
    fills = [BLUE, AMBER, GREY, GREEN]
    return (tile(BN) + pane(56, 56, 400, 296, BLUE, 16) + chev(140, 120, 90, 170, "#fff", 28)
            + '<rect x="262" y="260" width="96" height="32" rx="8" fill="#4da3ff"/>'
            + "".join(f'<rect x="{56 + k * 106}" y="376" width="88" height="80" rx="22" fill="{c}"/>'
                      for k, c in enumerate(fills)))


def bento_solid():
    r = lambda x, y, w, h, c: f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="26" fill="{c}"/>'
    return (tile(BN) + r(48, 48, 270, 270, BLUE) + r(334, 48, 130, 130, AMBER) + r(334, 194, 130, 124, GREEN)
            + r(48, 334, 190, 130, AGENT[0]) + r(254, 334, 210, 130, AGENT[3]) + chev(118, 118, 90, 130, "#fff", 30))


def bento_3x3():
    out = [tile(BN)]
    stat = [BLUE, AMBER, GREY, GREEN, GOLD, BLUE, GREY, GREEN, AMBER]
    for i, c in enumerate(stat):
        x, y = 62 + (i % 3) * 134, 62 + (i // 3) * 134
        if i == 4:
            out.append(f'<rect x="{x}" y="{y}" width="120" height="120" rx="26" fill="{GOLD}"/>' + chev(x + 38, y + 30, 30, 60, BN, 16))
        else:
            out.append(pane(x, y, 120, 120, c, 12))
    return "".join(out)


def bento_flip():
    return (tile(BN) + pane(196, 56, 260, 400, BLUE, 18) + chev(260, 190, 100, 132, "#fff", 30)
            + pane(56, 56, 120, 190, AMBER, 14)
            + '<circle cx="116" cy="151" r="30" fill="none" stroke="#4a5873" stroke-width="12"/>'
              '<path d="M116 121A30 30 0 1 1 86 151" fill="none" stroke="#ffb21f" stroke-width="12" stroke-linecap="round"/>'
            + pane(56, 266, 120, 190, GREEN, 14) + bar(86, 330, 60, GREEN) + bar(86, 366, 40, "#4a5873") + bar(86, 402, 52, "#4a5873"))


def bento_bars():
    out = [tile(BN)]
    for k, (x, y, c, f) in enumerate([(56, 56, BLUE, .7), (276, 56, AMBER, .4), (56, 276, GREY, .2), (276, 276, GREEN, 1.0)]):
        out += [pane(x, y, 180, 180, c, 12), dot(x + 38, y + 38, 12, c),
                f'<rect x="{x + 28}" y="{y + 120}" width="124" height="22" rx="11" fill="#2a3550"/>',
                f'<rect x="{x + 28}" y="{y + 120}" width="{124 * f:.0f}" height="22" rx="11" fill="{c}"/>',
                bar(x + 28, y + 80, 90) if k % 2 == 0 else bar(x + 28, y + 80, 60)]
    return "".join(out)


def bento_light():
    f = "#ffffff"
    return (tile(CREAM) + pane(56, 56, 260, 400, "#2d6cdf", 18, f) + chev(120, 190, 100, 132, "#1c2040", 30)
            + pane(336, 56, 120, 190, "#e69500", 14, f) + bang(396, 144, 1, "#e69500")
            + pane(336, 266, 120, 190, "#16a34a", 14, f) + check(396, 362, 1.1, "#16a34a"))


def bento_cascade():
    out = [tile(BN)]
    for i, (x, y, c) in enumerate([(52, 56, GREY), (114, 130, AMBER), (176, 204, BLUE)]):
        w, h = 284, 238
        out.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="28" fill="{PANE}" stroke="{c}" stroke-width="12"/>'
                   f'<path d="M{x + 6} {y + 52}H{x + w - 6}" stroke="{c}" stroke-width="8"/>'
                   + dot(x + 32, y + 28, 8, c) + dot(x + 56, y + 28, 8, c))
    return "".join(out) + chev(248, 306, 56, 90, "#fff", 22)


def bento_tetris():
    grid = ["AAAB", "CABB", "CDDB"[:3] + "B", "CCDD"]
    grid = ["AAAB", "CABB", "CDDB", "CCDD"]
    cols = {"A": BLUE, "B": AMBER, "C": GREEN, "D": AGENT[3]}
    out = [tile(BN)]
    for r, row in enumerate(grid):
        for c, ch in enumerate(row):
            out.append(f'<rect x="{64 + c * 96 + 3}" y="{64 + r * 96 + 3}" width="90" height="90" rx="18" fill="{cols[ch]}"/>')
    return "".join(out) + chev(190, 164 + 96 - 80, 36, 60, BN, 16)


def bento_mondrian():
    blocks = [(46, 46, 260, 260, BLUE), (306, 46, 160, 130, AMBER), (306, 176, 160, 130, CREAM),
              (46, 306, 150, 160, CREAM), (196, 306, 110, 160, GREEN), (306, 306, 160, 160, AGENT[0])]
    out = [tile(CREAM)]
    out += [f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{c}" stroke="#14161c" stroke-width="12"/>'
            for x, y, w, h, c in blocks]
    return "".join(out) + chev(124, 116, 80, 140, "#14161c", 28)


# -- riffs on 48 Sunset ptero --------------------------------------------------------------

def sun_flat():
    return (tile("#2b0f54") + dot(256, 296, 168, GOLD) + side(256, 250, .92, "#1a0b2e", "#2b0f54", GOLD))


def sun_stripes():
    stripes = "".join(f'<rect x="60" y="{y}" width="392" height="{h}" fill="#2b0f54"/>'
                      for y, h in [(288, 8), (320, 12), (354, 17), (392, 22)])
    return (SUN_DEFS + tile("#2b0f54") + '<circle cx="256" cy="286" r="196" fill="url(#sun)"/>' + stripes
            + side(212, 176, .74, "#1a0b2e", "#2b0f54", GOLD))


def sun_headon():
    return (SUN_DEFS + '<rect width="512" height="512" rx="112" fill="url(#sn)"/>'
            + '<circle cx="256" cy="300" r="160" fill="url(#sun)"/>' + head_on(256, 232, .72, "#1a0b2e"))


def sun_moon():
    stars = "".join(dot(x, y, r, "#fff") for x, y, r in [(70, 80, 3), (430, 70, 3), (90, 420, 4), (440, 400, 3), (390, 160, 2)])
    return (tile("#0b1230") + stars + dot(256, 270, 170, CREAM) + dot(200, 220, 26, "#dcd6c6") + dot(320, 330, 38, "#dcd6c6")
            + dot(300, 190, 14, "#dcd6c6") + side(256, 258, .9, "#0b1230", "#1a2550", CREAM))


def sun_mono():
    return ('<defs><linearGradient id="mn" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff7a45"/>'
            '<stop offset="1" stop-color="#ffd23f"/></linearGradient></defs>'
            '<rect width="512" height="512" rx="112" fill="url(#mn)"/>' + side(262, 256, 1.12, "#2b0f54", "#4a2380", GOLD))


def sun_grid():
    lines = "".join(f'<path d="M0 {y}H512" stroke="#ff4d8d" stroke-width="4"/>' for y in (322, 348, 380, 418, 464))
    lines += "".join(f'<path d="M256 300L{x} 512" stroke="#ff4d8d" stroke-width="4"/>' for x in (-260, -60, 100, 256, 412, 572, 772))
    return (SUN_DEFS + '<rect width="512" height="512" fill="url(#sn)"/>'
            + '<clipPath id="hz"><rect width="512" height="300"/></clipPath>'
            + '<circle cx="256" cy="300" r="176" fill="url(#sun)" clip-path="url(#hz)"/>'
            + '<rect y="300" width="512" height="212" fill="#1a0b2e"/>' + lines
            + side(256, 168, .6, "#1a0b2e", "#2b0f54", GOLD))


def sun_dawn():
    rays = "".join(ln(*pol(256, 372, 168, a), *pol(256, 372, 214, a), GOLD, 16) for a in range(190, 351, 20))
    return ('<rect width="512" height="512" rx="112" fill="#ff7a8a"/>' + rays
            + '<clipPath id="hz2"><rect width="512" height="372"/></clipPath>'
            + f'<circle cx="256" cy="372" r="136" fill="{GOLD}" clip-path="url(#hz2)"/>'
            + '<rect y="372" width="512" height="140" fill="#1a0b2e"/>' + side(256, 214, .56, "#1a0b2e", "#2b0f54", GOLD))


def sun_cutout():
    return ('<defs><linearGradient id="sun" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe066"/>'
            '<stop offset="1" stop-color="#ff4d8d"/></linearGradient><mask id="mk"><rect width="512" height="512" fill="#fff"/>'
            + side(262, 262, 1.12, "#000", "#000", "#fff") + '</mask></defs>'
            + tile("#1a0b2e") + '<circle cx="256" cy="256" r="214" fill="url(#sun)" mask="url(#mk)"/>')


def sun_big():
    return (SUN_DEFS + '<rect width="512" height="512" rx="112" fill="url(#sn)"/>' + dot(330, 330, 120, GOLD)
            + side(240, 270, 1.5, "#1a0b2e", "#2b0f54", GOLD))


def sun_duo():
    return (tile("#2b0f54") + dot(256, 300, 150, GOLD) + side(200, 214, .66, "#1a0b2e", "#2b0f54", GOLD)
            + side(344, 340, .4, "#1a0b2e", "#2b0f54", GOLD))


# -- riffs on 45 Low poly ------------------------------------------------------------------

LP = "#0b132b"
SIMPLE_HALF = [(256, 52), (292, 172), (462, 112), (398, 262), (318, 296), (286, 424), (256, 472)]
SIMPLE = SIMPLE_HALF + [(512 - x, y) for x, y in reversed(SIMPLE_HALF[1:-1])]
SIDE_BODY_PTS = [(474, 216), (400, 192), (330, 168), (366, 216), (346, 246), (318, 268), (290, 288), (230, 300),
                 (150, 322), (236, 332), (300, 332), (340, 310), (362, 276), (384, 240), (400, 228)]
SIDE_WING_PTS = [(285, 268), (215, 170), (112, 84), (150, 178), (172, 168), (190, 236), (214, 226), (228, 280)]


def facets(pts, hub, color_of, stroke, sw=3):
    out = []
    for i, (a, b) in enumerate(zip(pts, pts[1:] + pts[:1])):
        cx = (hub[0] + a[0] + b[0]) / 3
        cy = (hub[1] + a[1] + b[1]) / 3
        out.append(f'<path d="M{hub[0]} {hub[1]}L{a[0]} {a[1]}L{b[0]} {b[1]}Z" fill="{color_of(i, cx, cy)}" '
                   f'stroke="{stroke}" stroke-width="{sw}" stroke-linejoin="round"/>')
    return "".join(out)


def pal(colors, step=5):
    return lambda i, x, y: colors[(i * step + i // 3) % len(colors)]


def poly_bold():
    return (tile(LP) + facets(SIMPLE, (256, 240), pal(["#2ec4b6", "#14877f", "#06d6a0"], 1), LP, 5)
            + dot(256, 100, 10, LP))


def poly_rainbow():
    return tile(LP) + facets(HEAD_ON, (256, 250), pal(AGENT + [AMBER], 3), LP) + dot(256, 108, 9, LP)


def poly_warm():
    return (tile("#1a0b2e") + facets(HEAD_ON, (256, 250), pal(["#ff4d6d", "#ff7a45", "#ffb21f", "#ffd23f", "#c9184a"], 3), "#1a0b2e")
            + dot(256, 108, 9, "#1a0b2e"))


def poly_light():
    return (tile(CREAM) + facets(HEAD_ON, (256, 250), pal(["#3a86ff", "#2b59c3", "#8338ec", "#5e60ce", "#4ea8de"], 3), CREAM, 4)
            + dot(256, 108, 9, CREAM))


def poly_closeup():
    k = 1.5
    return ('<defs><clipPath id="tc"><rect width="512" height="512" rx="112"/></clipPath></defs>'
            f'<g clip-path="url(#tc)"><rect width="512" height="512" fill="{LP}"/>'
            f'<g transform="translate({256 - 256 * k:.1f} {400 - 263 * k:.1f}) scale({k})">'
            + facets(HEAD_ON, (256, 250), pal(["#2ec4b6", "#20a39e", "#3a86ff", "#8338ec", "#06d6a0", "#3d5a80"], 5), LP, 3)
            + "</g></g>")


def poly_side():
    t = "translate(36 12) scale(.9)"
    cols = ["#2ec4b6", "#20a39e", "#3a86ff", "#8338ec", "#06d6a0", "#3d5a80"]
    return (tile(LP) + f'<g transform="{t}">'
            + facets(SIDE_WING_PTS, (190, 200), pal(cols, 5), LP, 3) + facets(SIDE_BODY_PTS, (330, 252), pal(cols, 4), LP, 3)
            + '<circle cx="397" cy="207" r="7" fill="#fff"/></g>')


def poly_badge():
    return (tile(LP) + dot(256, 256, 214, "#1d2a4d", "#2ec4b6", 10)
            + f'<g transform="translate({256 - 256 * .8:.1f} {262 - 263 * .8:.1f}) scale(.8)">'
            + facets(HEAD_ON, (256, 250), pal(["#2ec4b6", "#20a39e", "#3a86ff", "#8338ec", "#06d6a0"], 3), "#1d2a4d", 4) + "</g>")


def poly_gradient():
    return (tile(LP) + facets(HEAD_ON, (256, 250), lambda i, x, y: lerp("#7ff5e2", "#0f5c66", abs(x - 256) / 190 * .7 + (y - 52) / 420 * .3),
                              LP, 2) + dot(256, 108, 9, LP))


def poly_wire():
    cy = "#22d3ee"
    out = [tile("#07121f")]
    n = len(HEAD_ON)
    for i, (a, b) in enumerate(zip(HEAD_ON, HEAD_ON[1:] + HEAD_ON[:1])):
        out.append(f'<path d="M256 250L{a[0]} {a[1]}L{b[0]} {b[1]}Z" fill="{cy}" fill-opacity="{.05 + .09 * (i % 3)}" '
                   f'stroke="{cy}" stroke-width="6" stroke-linejoin="round"/>')
    return "".join(out) + "".join(dot(x, y, 8, "#07121f", cy, 5) for x, y in HEAD_ON[::2])


def poly_chevron():
    return (tile(LP) + facets(HEAD_ON, (256, 250), pal(["#2ec4b6", "#20a39e", "#3a86ff", "#8338ec", "#06d6a0", "#3d5a80"], 5), LP, 3)
            + dot(256, 250, 70, LP, "#fff", 9) + chev(238, 220, 34, 60, "#fff", 18))


RIFFS = [
    # (slug, title, blurb, fn, bg, (ink, accent))  grouped by parent; see PARENTS
    ("fw-mono", "Fireworks: Mono", "One colour of ray, one gold chevron: the burst at its simplest.", fw_mono, FWB, ("#ffffff", GOLD)),
    ("fw-starburst", "Fireworks: Starburst", "Sixteen rays, long and short, in agent colours around a gold core.", fw_starburst, FWB, ("#ffffff", AGENT[0])),
    ("fw-pinwheel", "Fireworks: Pinwheel", "The rays curl: a spinning wheel of four colours.", fw_pinwheel, FWB, ("#ffffff", AGENT[2])),
    ("fw-asterisk", "Fireworks: Asterisk", "Four fat strokes crossing: the shape reads at any size.", fw_asterisk, FWB, ("#ffffff", AGENT[1])),
    ("fw-launch", "Fireworks: Launch", "A chevron fires; the burst fans out from its tip.", fw_launch, FWB, ("#ffffff", AGENT[3])),
    ("fw-trio", "Fireworks: Trio", "Three bursts at three sizes, trails rising behind.", fw_trio, FWB, ("#ffffff", AGENT[2])),
    ("fw-sparkle", "Fireworks: Sparkle", "A single gold sparkle with four coloured satellites.", fw_sparkle, FWB, ("#ffffff", GOLD)),
    ("fw-shock", "Fireworks: Shockwave", "Three dashed rings expanding from a white core.", fw_shock, FWB, ("#ffffff", AGENT[2])),
    ("fw-confetti", "Fireworks: Confetti", "Rounded confetti flying outward, a chevron in the middle.", fw_confetti, FWB, ("#ffffff", AGENT[1])),
    ("fw-light", "Fireworks: Daylight", "The burst on a light page, inked in navy.", fw_light, CREAM, ("#1c2040", AGENT[0])),
    ("hub-four", "Hub: Four Corners", "Four fat workers on the diagonals; the lead is a big gold disc.", hub_four, HB, ("#e9ecff", GOLD)),
    ("hub-crown", "Hub: Crown", "The lead wears a crown; five workers fan below.", hub_crown, HB, ("#e9ecff", GOLD)),
    ("hub-tri", "Hub: Triangle", "Three workers and a lead: the minimum that still says orchestra.", hub_tri, HB, ("#e9ecff", AGENT[0])),
    ("hub-hex", "Hub: Hex", "A hexagon lead with six hexagon workers.", hub_hex, HB, ("#e9ecff", GOLD)),
    ("hub-helm", "Hub: Helm", "A ship's wheel: the lead at the hub, an agent on each handle.", hub_helm, HB, ("#e9ecff", GOLD)),
    ("hub-bracket", "Hub: Bracket", "A big lead at left, four workers hanging off a bracket.", hub_bracket, HB, ("#e9ecff", GOLD)),
    ("hub-star", "Hub: Star", "Eight nodes at two distances: a starfish of agents.", hub_star, HB, ("#e9ecff", AGENT[2])),
    ("hub-corners", "Hub: Cross", "A square lead, four corner workers, joined in an X.", hub_corners, HB, ("#e9ecff", GOLD)),
    ("hub-tree", "Hub: Org Chart", "The lead on top, three workers underneath.", hub_tree, HB, ("#e9ecff", GOLD)),
    ("hub-ring", "Hub: Ring", "A white ring lead; workers of every size around it.", hub_ring, HB, ("#e9ecff", AGENT[1])),
    ("chain-3", "Chain: Three", "Three huge chevrons: it fits a favicon.", chain_3, CH, ("#f0f3f6", AGENT[1])),
    ("chain-5", "Chain: Five", "Five slim chevrons, a rainbow in sequence.", chain_5, CH, ("#f0f3f6", AGENT[3])),
    ("chain-ring", "Chain: Cycle", "Four chevrons chasing each other round a circle.", chain_ring, CH, ("#f0f3f6", AGENT[2])),
    ("chain-cursor", "Chain: Cursor", "Three chevrons, then a block cursor: the next prompt waiting.", chain_cursor, CH, ("#f0f3f6", "#f4f7fb")),
    ("chain-nested", "Chain: Nested", "One chevron inside another, four deep, like a tunnel.", chain_nested, CH, ("#f0f3f6", AGENT[0])),
    ("chain-flag", "Chain: Flag", "A single chevron cut from four colour bands.", chain_flag, CH, ("#f0f3f6", AGENT[2])),
    ("chain-ff", "Chain: Fast Forward", "Solid triangles, like a fast-forward button.", chain_ff, CH, ("#f0f3f6", AGENT[0])),
    ("chain-queue", "Chain: Queue", "Three dots waiting in line, one chevron going.", chain_queue, CH, ("#f0f3f6", AGENT[0])),
    ("chain-pill", "Chain: Pill", "Four chevrons on a conveyor track.", chain_pill, CH, ("#f0f3f6", AGENT[1])),
    ("chain-stair", "Chain: Staircase", "Chevrons climbing: every step hands off to the next.", chain_stair, CH, ("#f0f3f6", AGENT[3])),
    ("squad-one", "Squad: One Big Invader", "One crab, huge, in orange: it reads from across the room.", squad_one, SQB, ("#f0f3f6", AGENT[0])),
    ("squad-types", "Squad: Three Types", "A squid, a crab and an octopus stacked.", squad_types, SQB, ("#f0f3f6", GREEN)),
    ("squad-grid", "Squad: Formation", "Nine invaders in rows; the gold one is the lead.", squad_grid, SQB, ("#f0f3f6", GOLD)),
    ("squad-screen", "Squad: Screen Belly", "An invader with a terminal for a belly.", squad_screen, SQB, ("#f0f3f6", AGENT[0])),
    ("squad-bot", "Squad: Big Bot", "The little robot, enlarged to fill the tile.", squad_bot, SQB, ("#f0f3f6", BLUE)),
    ("squad-laser", "Squad: Laser", "A cannon fires up at the lead invader.", squad_laser, SQB, ("#f0f3f6", GREEN)),
    ("squad-v", "Squad: V Formation", "Five squids in a V, the lead in gold.", squad_v, SQB, ("#f0f3f6", GOLD)),
    ("squad-quad", "Squad: Four Colours", "One invader, four colours: the whole squad in one body.", squad_quad, SQB, ("#f0f3f6", AGENT[1])),
    ("squad-window", "Squad: Window", "An invader inside a terminal window.", squad_window, SQB, ("#f0f3f6", GREEN)),
    ("squad-jelly", "Squad: Jellies", "Four wide-eyed ghosts, one colour each.", squad_jelly, SQB, ("#f0f3f6", AGENT[3])),
    ("bento-3", "Bento: Three", "One big pane and two small: the layout in its plainest form.", bento_3, BN, ("#e6edf3", BLUE)),
    ("bento-strip", "Bento: Strip", "A big pane over a strip of four solid status tiles.", bento_strip, BN, ("#e6edf3", BLUE)),
    ("bento-solid", "Bento: Solid", "No borders, just colour blocks and one chevron.", bento_solid, BN, ("#e6edf3", AGENT[0])),
    ("bento-3x3", "Bento: Nine", "Nine panes, one gold: the one that needs you.", bento_3x3, BN, ("#e6edf3", GOLD)),
    ("bento-flip", "Bento: Flipped", "The big pane on the right, a progress ring and bars on the left.", bento_flip, BN, ("#e6edf3", GREEN)),
    ("bento-bars", "Bento: Progress", "Four panes, each with a progress bar.", bento_bars, BN, ("#e6edf3", GREEN)),
    ("bento-light", "Bento: Light", "The three-pane bento on cream.", bento_light, CREAM, ("#1c2040", "#2d6cdf")),
    ("bento-cascade", "Bento: Cascade", "Three overlapping windows, the front one in focus.", bento_cascade, BN, ("#e6edf3", BLUE)),
    ("bento-tetris", "Bento: Tetris", "Four tetrominoes tiling a square, one per agent.", bento_tetris, BN, ("#e6edf3", AMBER)),
    ("bento-mondrian", "Bento: Mondrian", "Primary-colour blocks and heavy black lines.", bento_mondrian, CREAM, ("#14161c", AGENT[0])),
    ("sun-flat", "Sunset: Flat", "Flat gold sun, one dark pterodactyl: three shapes.", clipped(sun_flat), "#2b0f54", ("#fff1c9", GOLD)),
    ("sun-stripes", "Sunset: Stripes", "A huge striped sun, the pterodactyl riding high.", clipped(sun_stripes), "#2b0f54", ("#fff1c9", "#ff5d8f")),
    ("sun-headon", "Sunset: Head-on", "The head-on pterodactyl against the sun.", clipped(sun_headon), "#2b0f54", ("#fff1c9", "#ff5d8f")),
    ("sun-moon", "Sunset: Moon", "Night flight: the silhouette crosses a full moon.", clipped(sun_moon), "#0b1230", ("#f6f3ec", "#7aa2ff")),
    ("sun-mono", "Sunset: Gradient", "Just an orange-to-gold sky and one huge silhouette.", sun_mono, "#ff7a45", ("#2b0f54", "#2b0f54")),
    ("sun-grid", "Sunset: Synthwave", "A neon grid floor under a half-sun.", clipped(sun_grid), "#2b0f54", ("#fff1c9", "#ff4d8d")),
    ("sun-dawn", "Sunset: Dawn", "Sun rising with rays over a dark horizon.", sun_dawn, "#ff7a8a", ("#1a0b2e", "#1a0b2e")),
    ("sun-cutout", "Sunset: Cutout", "The pterodactyl is a hole in the sun: negative space.", sun_cutout, "#1a0b2e", ("#fff1c9", "#ff5d8f")),
    ("sun-big", "Sunset: Big Bird", "A giant silhouette, wing cropped by the edge of the tile.", clipped(sun_big), "#2b0f54", ("#fff1c9", GOLD)),
    ("sun-duo", "Sunset: Duo", "Two pterodactyls crossing one sun.", clipped(sun_duo), "#2b0f54", ("#fff1c9", GOLD)),
    ("poly-bold", "Low Poly: Bold", "Fewer, bigger facets in three teals: the long-distance version.", poly_bold, LP, ("#e9f7ff", "#06d6a0")),
    ("poly-rainbow", "Low Poly: Agents", "Facets in the four agent colours and amber.", poly_rainbow, LP, ("#e9f7ff", AGENT[1])),
    ("poly-warm", "Low Poly: Warm", "Reds, oranges and golds on a deep plum.", poly_warm, "#1a0b2e", ("#fff1c9", "#ff7a45")),
    ("poly-light", "Low Poly: Light", "Blues and violets on cream.", poly_light, CREAM, ("#1c2040", "#5e60ce")),
    ("poly-closeup", "Low Poly: Close-up", "Zoomed in so the wings run off the tile.", poly_closeup, LP, ("#e9f7ff", "#06d6a0")),
    ("poly-side", "Low Poly: Side View", "A faceted pterodactyl in profile.", poly_side, LP, ("#e9f7ff", "#3a86ff")),
    ("poly-badge", "Low Poly: Badge", "The faceted bird inside a round badge.", poly_badge, LP, ("#e9f7ff", "#2ec4b6")),
    ("poly-gradient", "Low Poly: Shaded", "Facets shaded from a bright centre to deep teal edges.", poly_gradient, LP, ("#e9f7ff", "#7ff5e2")),
    ("poly-wire", "Low Poly: Wireframe", "Just the edges and vertices, in cyan.", poly_wire, "#07121f", ("#d6faff", "#22d3ee")),
    ("poly-chevron", "Low Poly: Prompt", "A prompt chevron cut across the faceted wings.", poly_chevron, LP, ("#e9f7ff", "#06d6a0")),
]

# -- tentacle bots: a cute multi-armed robot handling coloured tasks ------------------------

TASKS = [BLUE, AMBER, GREEN, AGENT[0], AGENT[3], "#ff5d8f"]
GLYPHS = ["chev", "bang", "check", "bars", "chev", "check"]


def arm(x0, y0, x1, y1, col, sw=22, bend=0.0):
    """A tentacle from (x0, y0) to (x1, y1): an S-curve, swaying by bend (in pixels)."""
    my = (y0 + y1) / 2
    return (f'<path d="M{x0:.1f} {y0:.1f}C{x0 + bend:.1f} {my:.1f} {x1 - bend:.1f} {my:.1f} {x1:.1f} {y1:.1f}" '
            f'fill="none" stroke="{col}" stroke-width="{sw}" stroke-linecap="round"/>')


def card(cx, cy, s, col, glyph, ink=BN, rx=None):
    """A task: a coloured rounded square, centred on (cx, cy), with a glyph on it."""
    k = s / 80
    g = {"chev": chev(cx - 14 * k, cy - 18 * k, 26 * k, 36 * k, ink, 11 * k),
         "check": check(cx, cy, k * .9, ink), "bang": bang(cx, cy - 7 * k, k * .8, ink),
         "bars": bar(cx - 20 * k, cy - 12 * k, 40 * k, ink, 11 * k) + bar(cx - 20 * k, cy + 12 * k, 26 * k, ink, 11 * k)}[glyph]
    return (f'<rect x="{cx - s / 2:.1f}" y="{cy - s / 2:.1f}" width="{s}" height="{s}" rx="{rx or s * .26:.1f}" '
            f'fill="{col}"/>' + g)


def bothead(cx, cy, w, h, body="#f4f7fb", screen=BN, face=GREEN, ant=GOLD):
    """The robot: a rounded head with a dark screen, a `> <` face and an antenna."""
    x, y, k, ex = cx - w / 2, cy - h / 2, h * .17, w * .2
    st = f'fill="none" stroke="{face}" stroke-width="{h * .1:.1f}" stroke-linecap="round" stroke-linejoin="round"'
    return (f'<path d="M{cx} {y:.1f}v{-h * .2:.1f}" stroke="{body}" stroke-width="{h * .07:.1f}" stroke-linecap="round"/>'
            + dot(cx, y - h * .24, h * .1, ant)
            + f'<rect x="{x:.1f}" y="{y:.1f}" width="{w}" height="{h}" rx="{h * .3:.1f}" fill="{body}"/>'
            + f'<rect x="{x + w * .09:.1f}" y="{y + h * .14:.1f}" width="{w * .82:.1f}" height="{h * .72:.1f}" '
              f'rx="{h * .2:.1f}" fill="{screen}"/>'
            + f'<path d="M{cx - ex - k * .6:.1f} {cy - k:.1f}L{cx - ex + k * .6:.1f} {cy:.1f}L{cx - ex - k * .6:.1f} {cy + k:.1f}'
              f'M{cx + ex + k * .6:.1f} {cy - k:.1f}L{cx + ex - k * .6:.1f} {cy:.1f}L{cx + ex + k * .6:.1f} {cy + k:.1f}" {st}/>'
            + f'<path d="M{cx - w * .08:.1f} {cy + h * .27:.1f}H{cx + w * .08:.1f}" {st}/>')


def tentabot(bg, body, limb, screen, tasks=TASKS):
    """Six tentacles fanning out under the head, each holding a task."""
    out = [tile(bg)]
    for i in range(6):
        x0, x1 = 196 + i * 24, 66 + i * 76
        out.append(arm(x0, 262, x1, 392, limb, 22, 40))
    out.append(bothead(256, 170, 230, 170, body, screen))
    out += [card(66 + i * 76, 424, 62, tasks[i], GLYPHS[i], screen) for i in range(6)]
    return "".join(out)


def tent_1():
    return tentabot("#1b1f3a", "#f4f7fb", "#cdd6e4", BN)


def tent_light():
    return tentabot(CREAM, "#ffffff", "#2d3a5c", "#1c2040", ["#2d6cdf", "#e69500", "#16a34a", "#e8531f", "#8b45e6", "#d6336c"])


def tent_bento():
    return (tile(BN) + pane(56, 56, 260, 400, BLUE, 18)
            + arm(250, 240, 380, 150, "#cdd6e4", 20, -40) + arm(250, 270, 380, 360, "#cdd6e4", 20, -40)
            + pane(336, 56, 120, 190, AMBER, 14) + bang(396, 144, 1, AMBER)
            + pane(336, 266, 120, 190, GREEN, 14) + check(396, 362, 1.1, GREEN)
            + arm(150, 300, 150, 410, "#cdd6e4", 20, 30) + bothead(186, 200, 190, 140) + card(150, 420, 56, AGENT[0], "chev"))


def tent_fireworks():
    out = [tile("#0b0f2a")]
    for k in range(12):
        a = math.radians(k * 30 + 15)
        c, b = AGENT[k % 4], (28 if k % 2 else -28)
        mx, my = 256 + 110 * math.cos(a) - b * math.sin(a), 256 + 110 * math.sin(a) + b * math.cos(a)
        ex, ey = 256 + 190 * math.cos(a), 256 + 190 * math.sin(a)
        out.append(f'<path d="M256 256Q{mx:.1f} {my:.1f} {ex:.1f} {ey:.1f}" fill="none" stroke="{c}" stroke-width="16" '
                   f'stroke-linecap="round"/>' + dot(ex, ey, 17, c, "#0b0f2a", 5))
    return "".join(out) + bothead(256, 262, 150, 112)


TB = ["....A......", "..HHHHHHH..", ".HHHHHHHHH.", ".HHKHHHKHH.", ".HHHHHHHHH.", "..HHHHHHH..",
      ".H.H.H.H.H.", ".H.H.H.H.H.", "H..H.H.H..H", "1..2.3.4..5"]


def tent_squad():
    return tile(SQB) + spr(TB, "#f0f3f6", 40, gap=4, extra={"A": GOLD, "H": "#f0f3f6", "K": SQB, "1": BLUE,
                                                              "2": AMBER, "3": GREEN, "4": AGENT[0], "5": AGENT[3]})


def tent_hub():
    out = [tile(CH)]
    pts = [(256 + 176 * math.cos(math.radians(60 * k - 90)), 256 + 176 * math.sin(math.radians(60 * k - 90))) for k in range(6)]
    out += [arm(256, 256, x, y, "#3a4766", 18, 30 * (1 if k % 2 else -1)) for k, (x, y) in enumerate(pts)]
    out.append(bothead(256, 266, 150, 112))
    out += [dot(x, y, 42, TASKS[k]) + card(x, y, 52, TASKS[k], GLYPHS[k], CH, 26) for k, (x, y) in enumerate(pts)]
    return "".join(out)


def tent_chain():
    out = [tile(CH), '<rect x="36" y="330" width="440" height="130" rx="65" fill="#1f2638"/>']
    for k in range(4):
        x = 100 + k * 104
        out.append(arm(196 + k * 40, 250, x, 360, "#cdd6e4", 20, 30) + card(x, 394, 76, AGENT[k], "chev", CH))
    return "".join(out) + bothead(256, 150, 220, 160)


def tent_juggle():
    out = [tile("#2b1055")]
    for k, (x, y) in enumerate([(70, 170), (150, 76), (256, 110), (362, 76), (442, 170)]):
        out.append(arm(180 + k * 38, 380, x, y + 36, "#ff7eb6", 18, -50 if x < 256 else 50)
                   + card(x, y, 64, TASKS[k], GLYPHS[k], "#2b1055"))
    return "".join(out) + bothead(256, 400, 220, 140, "#ffe3f0", "#2b1055")


def tent_list():
    out = [tile(BN)]
    for k in range(4):
        y = 92 + k * 100
        out.append(arm(150, 256, 232, y, "#cdd6e4", 16, 30)
                   + f'<rect x="232" y="{y - 36}" width="244" height="72" rx="36" fill="{TASKS[k]}"/>'
                   + check(272, y, .8, BN) + bar(310, y, 120, BN, 14))
    return "".join(out) + bothead(104, 256, 150, 124)


def tent_sunset():
    return ('<rect width="512" height="512" rx="112" fill="#2b0f54"/><circle cx="256" cy="290" r="170" fill="#ffd23f"/>'
            + "".join(f'<rect x="80" y="{y}" width="352" height="{h}" fill="#2b0f54"/>' for y, h in [(330, 8), (366, 12), (404, 16)])
            + "".join(arm(196 + i * 30, 330, 90 + i * 83, 200 + (i % 2) * 40, "#1a0b2e", 22, 40) + dot(90 + i * 83, 200 + (i % 2) * 40, 18, AGENT[i % 4])
                      for i in range(5))
            + bothead(256, 360, 220, 150, "#1a0b2e", "#1a0b2e", GOLD, "#ff5d8f"))


def tent_poly():
    T = lambda pts, c: f'<polygon points="{pts}" fill="{c}" stroke="{c}" stroke-width="2" stroke-linejoin="round"/>'
    out = [tile(LP)]
    for i in range(5):
        x, xe = 168 + i * 44, 56 + i * 100
        out.append(T(f"{x - 16},250 {x + 16},250 {xe},400", ["#20a39e", "#2ec4b6"][i % 2])
                   + T(f"{xe - 28},400 {xe + 28},400 {xe},440", TASKS[i]))
    return "".join(out) + (T("256,60 396,130 396,230 256,290 116,230 116,130", "#2ec4b6")
                           + T("256,60 396,130 256,160", "#06d6a0") + T("256,60 116,130 256,160", "#3a86ff")
                           + T("116,130 256,160 256,290 116,230", "#20a39e") + T("396,130 256,160 256,290 396,230", "#3d5a80")
                           + chev(200, 170, 40, 70, "#fff", 16) + chev(312, 170, -40, 70, "#fff", 16))


def tent_closeup():
    out = [tile("#12b5a6")]
    for i in range(4):
        x = 100 + i * 104
        out.append(arm(160 + i * 64, 330, x, 450, "#f4f7fb", 30, 30) + card(x, 484, 92, TASKS[i], GLYPHS[i], "#12b5a6"))
    return ('<defs><clipPath id="tc"><rect width="512" height="512" rx="112"/></clipPath></defs><g clip-path="url(#tc)">'
            + "".join(out) + bothead(256, 210, 340, 250, "#f4f7fb", BN) + "</g>")


def tent_badge():
    ring = "".join(f'<path d="M{256 + 210 * math.cos(math.radians(a)):.1f} {256 + 210 * math.sin(math.radians(a)):.1f}'
                   f'A210 210 0 0 1 {256 + 210 * math.cos(math.radians(a + 82)):.1f} {256 + 210 * math.sin(math.radians(a + 82)):.1f}" '
                   f'fill="none" stroke="{AGENT[k]}" stroke-width="22" stroke-linecap="round"/>' for k, a in enumerate(range(-86, 274, 90)))
    out = [tile("#101826"), ring, '<circle cx="256" cy="256" r="190" fill="#1b2a4a"/>']
    for k in range(4):
        a = math.radians(k * 90 + 45)
        out.append(arm(256, 256, 256 + 150 * math.cos(a), 256 + 150 * math.sin(a), "#9fb4e0", 16, 30)
                   + dot(256 + 150 * math.cos(a), 256 + 150 * math.sin(a), 24, AGENT[k]))
    return "".join(out) + bothead(256, 256, 170, 124)


def tent_kanban():
    out = [tile("#0f2a3a")]
    for c in range(3):
        x = 52 + c * 140
        out.append(f'<rect x="{x}" y="250" width="124" height="214" rx="26" fill="#173a50"/>'
                   f'<rect x="{x + 14}" y="{264 + 0}" width="96" height="48" rx="14" fill="{TASKS[c]}"/>'
                   f'<rect x="{x + 14}" y="326" width="96" height="48" rx="14" fill="{TASKS[(c + 3) % 6]}" opacity=".8"/>'
                   f'<rect x="{x + 14}" y="388" width="96" height="48" rx="14" fill="{TASKS[(c + 1) % 6]}" opacity=".55"/>')
        out.append(arm(216 + c * 40, 190, x + 62, 262, "#e6edf3", 18, 20 - c * 20))
    return "".join(out) + bothead(256, 110, 200, 120, "#e6edf3")


def tent_cross():
    out = [tile("#14192b")]
    for k, (x, y) in enumerate([(100, 100), (412, 100), (100, 412), (412, 412)]):
        out.append(arm(256, 256, x, y, "#cdd6e4", 24, 0) + card(x, y, 116, AGENT[k], GLYPHS[k], "#14192b"))
    return "".join(out) + bothead(256, 270, 180, 136)


def spiral(cx, cy, r0, a0, turns, d):
    n = 40
    return "".join(("M" if i == 0 else "L") + f"{cx + (r0 * (1 - .85 * i / n)) * math.cos(a0 + d * turns * 2 * math.pi * i / n):.1f} "
                   f"{cy + (r0 * (1 - .85 * i / n)) * math.sin(a0 + d * turns * 2 * math.pi * i / n):.1f}" for i in range(n + 1))


def tent_curl():
    out = [tile("#2b1055")]
    for k, (cx, cy, r, d, sx) in enumerate([(92, 380, 60, -1, 190), (420, 380, 60, 1, 322), (190, 440, 36, 1, 232), (322, 440, 36, -1, 280)]):
        tipx, tipy = cx + r * math.cos(math.pi * (1.5 if d < 0 else 1.5)), cy + r * math.sin(math.pi * 1.5)
        out.append(f'<path d="M{sx} 290C{sx} 340 {tipx:.1f} 320 {tipx:.1f} {tipy:.1f}{spiral(cx, cy, r, -math.pi / 2, 1.4, d).replace("M", "L", 1)}" '
                   f'fill="none" stroke="#ff7eb6" stroke-width="20" stroke-linecap="round" stroke-linejoin="round"/>')
        ex, ey = [float(v) for v in spiral(cx, cy, r, -math.pi / 2, 1.4, d).split("L")[-1].split()]
        out.append(dot(ex, ey, 17, AGENT[k], "#2b1055", 5))
    return "".join(out) + bothead(256, 170, 230, 170, "#ffe3f0", "#2b1055")


def tent_cheer():
    out = [tile("#0c8f84")]
    for k in range(5):
        x, y = 56 + k * 100, 130 + abs(k - 2) * 30
        out.append(arm(190 + k * 33, 380, x, y + 20, "#f4f7fb", 20, -40 if x < 256 else 40)
                   + f'<path d="M{x} {y + 20}V{y - 50}" stroke="#f4f7fb" stroke-width="8" stroke-linecap="round"/>'
                   f'<path d="M{x + 4} {y - 50}H{x + 52}L{x + 36} {y - 30}L{x + 52} {y - 10}H{x + 4}Z" fill="{TASKS[k]}" '
                   f'stroke="{TASKS[k]}" stroke-width="6" stroke-linejoin="round"/>')
    return "".join(out) + bothead(256, 410, 220, 130, "#f4f7fb", BN)


def tent_plugs():
    out = [tile("#101826"), '<rect x="40" y="410" width="432" height="62" rx="24" fill="#1f2a40"/>']
    for k in range(4):
        x = 88 + k * 112
        out.append(arm(200 + k * 38, 230, x, 330, "#9fb4e0", 18, 30)
                   + f'<rect x="{x - 30}" y="326" width="60" height="50" rx="14" fill="{AGENT[k]}"/>'
                   f'<path d="M{x - 12} 376V404M{x + 12} 376V404" stroke="#e6edf3" stroke-width="9" stroke-linecap="round"/>'
                   f'<rect x="{x - 22}" y="424" width="44" height="12" rx="6" fill="#0a0e17"/>')
    return "".join(out) + bothead(256, 130, 220, 150)


def tent_orbit():
    out = [tile("#0b0d1c"), '<circle cx="256" cy="256" r="172" fill="none" stroke="#3b4a7a" stroke-width="7" stroke-dasharray="6 18" stroke-linecap="round"/>']
    for k in range(4):
        a = math.radians(k * 90 + 45)
        x, y = 256 + 172 * math.cos(a), 256 + 172 * math.sin(a)
        out.append(arm(256, 256, x, y, "#6f86c8", 14, 26))
    for k in range(4):
        a = math.radians(k * 90 + 45)
        out.append(card(256 + 172 * math.cos(a), 256 + 172 * math.sin(a), 84, AGENT[k], GLYPHS[k], "#0b0d1c"))
    return "".join(out) + bothead(256, 266, 140, 104)


def tent_mono():
    ink, out = "#2b0f54", ['<rect width="512" height="512" rx="112" fill="#ff7a45"/>']
    for i in range(5):
        x, xe = 186 + i * 35, 80 + i * 88
        out.append(arm(x, 270, xe, 410, ink, 24, 40) + dot(xe, 420, 28, "#fff1c9" if i == 2 else ink))
    return "".join(out) + bothead(256, 170, 240, 180, ink, "#ff7a45", ink, ink)


TENT = [
    ("tent-six", "Tentabot: Six Arms", "A cheerful robot, six tentacles, six tasks in six colours: the octopus with a screen for a face.", tent_1, "#1b1f3a", ("#f4f7fb", GOLD)),
    ("tent-light", "Tentabot: Light", "The six-armed robot on cream, in deeper colours for white pages.", tent_light, CREAM, ("#1c2040", "#2d6cdf")),
    ("tent-bento", "Tentabot: Bento", "The three-pane bento, with the robot in the big pane reaching tasks into the small ones.", tent_bento, BN, ("#e6edf3", BLUE)),
    ("tent-fireworks", "Tentabot: Fireworks", "Twelve wavy tentacles burst out of the robot, each ending in a coloured task.", tent_fireworks, "#0b0f2a", ("#ffffff", "#ffd93b")),
    ("tent-squad", "Tentabot: Squad", "The pixel robot with five pixel tentacles, a coloured task at the end of each.", tent_squad, SQB, ("#f0f3f6", GREEN)),
    ("tent-hub", "Tentabot: Hub", "The robot at the centre, six tentacles out to six round tasks.", tent_hub, CH, ("#e6edf3", AGENT[1])),
    ("tent-chain", "Tentabot: Chain", "Four tentacles hold a row of four chevron tiles: the train, hand-delivered.", tent_chain, CH, ("#e6edf3", AGENT[0])),
    ("tent-juggle", "Tentabot: Juggler", "Five tentacles tossing five coloured tasks in the air.", tent_juggle, "#2b1055", ("#ffe3f0", "#ff7eb6")),
    ("tent-list", "Tentabot: Checklist", "Four tentacles, four tasks, one tick each.", tent_list, BN, ("#e6edf3", GREEN)),
    ("tent-sunset", "Tentabot: Sunset", "A silhouetted robot waves coloured tasks in front of a striped sun.", clipped(tent_sunset), "#2b0f54", ("#fff1c9", GOLD)),
    ("tent-poly", "Tentabot: Low Poly", "A faceted head and faceted tentacles with coloured tips.", tent_poly, LP, ("#e9f7ff", "#06d6a0")),
    ("tent-closeup", "Tentabot: Close-up", "A huge head and four fat tentacles, tasks cropped by the edge of the tile.", tent_closeup, "#12b5a6", ("#ffffff", GOLD)),
    ("tent-badge", "Tentabot: Badge", "The robot inside a four-colour ring, one tentacle to each colour.", tent_badge, "#101826", ("#e6edf3", BLUE)),
    ("tent-kanban", "Tentabot: Kanban", "The robot above three columns of tasks, a tentacle dropping into each.", tent_kanban, "#0f2a3a", ("#e6edf3", AMBER)),
    ("tent-cross", "Tentabot: Four Corners", "Four straight tentacles, four big tasks, one in each corner.", tent_cross, "#14192b", ("#e6edf3", AGENT[2])),
    ("tent-curl", "Tentabot: Curls", "Four curled tentacles, a coloured dot at the end of each curl.", tent_curl, "#2b1055", ("#ffe3f0", "#ff7eb6")),
    ("tent-cheer", "Tentabot: Cheer", "Five tentacles wave five coloured flags: the build is green.", tent_cheer, "#0c8f84", ("#ffffff", GOLD)),
    ("tent-plugs", "Tentabot: Plugs", "Four tentacles plug four coloured connectors into a socket strip: one pipe per session.", tent_plugs, "#101826", ("#e6edf3", AGENT[3])),
    ("tent-orbit", "Tentabot: Orbit", "Four tasks on an orbit, each tethered to the robot by a tentacle.", tent_orbit, "#0b0d1c", ("#e6edf3", "#6f86c8")),
    ("tent-mono", "Tentabot: Mono", "One dark silhouette on orange, a single cream task: the long-distance version.", tent_mono, "#ff7a45", ("#2b0f54", "#2b0f54")),
]


PARENTS = ["05 Fireworks", "22 Hub", "23 Chain", "25 Squad", "28 Bento", "48 Sunset ptero", "45 Low poly"]

# ── the wordmark: "ptyro" drawn as strokes on a 20×8 grid (x-height 2..6, descender to 8) ─────

LETTERS = [
    (0.0, "p", '<path d="M0 2V8M0 2H1.5A2 2 0 0 1 1.5 6H0"/>'),
    (4.9, "t", '<path d="M1 0V4.5Q1 6 2.6 6M0 2H2.6"/>'),
    (8.4, "y", '<path d="M0 2L1.8 6M3.6 2L1.4 7.4Q1.1 8 .2 8"/>'),
    (13.0, "r", '<path d="M0 2V6M0 3.6Q.5 2 2.6 2"/>'),
    (16.5, "o", '<rect x="0" y="2" width="3.6" height="4" rx="1.8"/>'),
]


def wordmark(ink, accent, x, y, s):
    g = [f'<g transform="translate({x} {y}) scale({s})" fill="none" stroke-width="1.05" '
         'stroke-linecap="round" stroke-linejoin="round">']
    for dx, ch, body in LETTERS:
        col = accent if ch == "o" else ink
        g.append(f'<g transform="translate({dx} 0)" stroke="{col}">{body}</g>')
    return "".join(g) + "</g>"


def svg(w, h, inner, label):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" '
            f'role="img" aria-label="{label}">{DEFS}{inner}</svg>\n')


def lockup(body, bg, ink, accent):
    """The wide logo: the icon at left, the wordmark at right, on bg (None for a transparent background)."""
    back = f'<rect width="1200" height="400" fill="{bg}"/>' if bg else ""
    return svg(1200, 400, back + f'<g transform="translate(40 40) scale(.625)">{body}</g>'
               + wordmark(ink, accent, 450, 92, 28), "ptyro logo")


def emit(entries, start, imgdir):
    """Write each entry's logo and icon into imgdir; return (name, title, blurb) rows for the README."""
    os.makedirs(imgdir, exist_ok=True)
    rows = []
    for n, (slug, title, blurb, icon, bg, (ink, accent)) in enumerate(entries, start):
        base = f"{n:02d}-{slug}"
        body = icon()
        with open(f"{imgdir}/{base}-icon.svg", "w") as f:
            f.write(svg(512, 512, body, f"ptyro icon: {title}"))
        with open(f"{imgdir}/{base}-logo.svg", "w") as f:
            f.write(lockup(body, bg, ink, accent).replace('aria-label="ptyro logo"', f'aria-label="ptyro logo: {title}"'))
        rows.append((base, title, blurb))
    return rows


def bento_ink():
    """91 Bento Three, inverted for white backgrounds: a light tile, dark ink, deeper status colours."""
    f = "#ffffff"
    return (tile("#eef2f7") + pane(56, 56, 260, 400, "#2d6cdf", 18, f) + chev(120, 190, 100, 132, "#14192b", 30)
            + pane(336, 56, 120, 190, "#e69500", 14, f) + bang(396, 144, 1, "#e69500")
            + pane(336, 266, 120, 190, "#16a34a", 14, f) + check(396, 362, 1.1, "#16a34a"))


def fireworks_ink():
    """05 Fireworks, inverted for white backgrounds: a light tile, deeper colours, navy ink."""
    cols = ["#e8531f", "#16a34a", "#2d6cdf", "#8b45e6"]
    out = [tile("#eef2f7")]
    for k in range(12):
        a = math.radians(k * 30 + 15)
        x1, y1 = 256 + 76 * math.cos(a), 256 + 76 * math.sin(a)
        x2, y2 = 256 + 150 * math.cos(a), 256 + 150 * math.sin(a)
        x3, y3 = 256 + 196 * math.cos(a), 256 + 196 * math.sin(a)
        out.append(f'<path d="M{x1:.1f} {y1:.1f}L{x2:.1f} {y2:.1f}" stroke="{cols[k % 4]}" stroke-width="18" '
                   f'stroke-linecap="round"/><circle cx="{x3:.1f}" cy="{y3:.1f}" r="13" fill="{cols[k % 4]}"/>')
    out.append('<circle cx="256" cy="256" r="58" fill="#ffffff" stroke="#14192b" stroke-width="10"/>'
               '<path d="M236 232L264 256L236 280M272 284H290" fill="none" stroke="#14192b" '
               'stroke-width="14" stroke-linecap="round" stroke-linejoin="round"/>')
    return "".join(out)


def brand(out):
    """The chosen logo (91 Bento Three) and its backup (05 Fireworks) as flat files in out/: each on
    dark, and inverted for white backgrounds (the logo then has a solid white background)."""
    for suffix, body, bg, ink, accent in [
            ("", bento_3(), BN, "#e6edf3", BLUE),
            ("-on-white", bento_ink(), "#ffffff", "#14192b", "#2d6cdf"),
            ("-alt", fireworks(), "#0b0f2a", "#ffffff", "#ffd93b"),
            ("-alt-on-white", fireworks_ink(), "#ffffff", "#14192b", "#e08a00")]:
        with open(f"{out}/ptyro-icon{suffix}.svg", "w") as f:
            f.write(svg(512, 512, body, "ptyro icon"))
        with open(f"{out}/ptyro-logo{suffix}.svg", "w") as f:
            f.write(lockup(body, bg, ink, accent))


THEMES = ["The flame", "No fire", "Many agents at once", "The merge train", "Pterodactyls"]


def main(out):
    """Every image goes in out/ideas/; out/README.md is the one gallery that lists all of them."""
    ideas = f"{out}/ideas"
    rows = emit(ICONS, 1, ideas) + emit(RIFFS, 51, ideas) + emit(TENT, 121, ideas)
    brand(out)
    heads = {i * 10: t for i, t in enumerate(THEMES)}
    heads.update({50 + i * 10: f"Riffs on {p}" for i, p in enumerate(PARENTS)})
    heads[120] = "Tentacle bots: a cute robot handling coloured tasks (riffs on 13 Octopus, 17 Robot, 05, 25, 28 and more)"
    with open(f"{out}/README.md", "w") as f:
        f.write("# ptyro logos\n\n"
                f"{len(rows)} concepts, each as a wide **logo** (1200×400) and a square **icon** (512×512), in SVG "
                "and PNG. Every image file is in [`ideas/`](ideas/). Concepts 01–50 explore themes; 51–120 are ten "
                "riffs on each of seven favourites (05, 22, 23, 25, 28, 48, 45), built from few shapes, thick strokes "
                "and strong contrast so they read from a distance; 121–140 are twenty tentacle bots, a cute robot "
                "handling coloured tasks, drawn in the style of the favourites. Generated by `scripts/logos.py`; run "
                "`make logos` to rebuild (PNGs need `rsvg-convert`). The wordmark is drawn, not typeset, so no fonts "
                "are needed.\n\n"
                "# The chosen logo\n\n"
                "From concept 91 (Bento Three). Use the dark set on dark or colourful pages and the "
                "`-on-white` set on white or light ones (its logo has a solid white background).\n\n"
                '<img src="ptyro-logo.png" alt="ptyro logo" width="480"> <img src="ptyro-icon.png" alt="ptyro icon" width="120">\n\n'
                '<img src="ptyro-logo-on-white.png" alt="ptyro logo for white backgrounds" width="480"> '
                '<img src="ptyro-icon-on-white.png" alt="ptyro icon for white backgrounds" width="120">\n\n'
                "`ptyro-logo` · `ptyro-icon` · `ptyro-logo-on-white` · `ptyro-icon-on-white`, each as `.svg` and `.png`\n\n"
                "# The backup logo\n\n"
                "From concept 05 (Fireworks), the alternate if the chosen logo does not work out. Same two sets, "
                "with `-alt` in the name.\n\n"
                '<img src="ptyro-logo-alt.png" alt="ptyro alternate logo" width="480"> <img src="ptyro-icon-alt.png" alt="ptyro alternate icon" width="120">\n\n'
                '<img src="ptyro-logo-alt-on-white.png" alt="ptyro alternate logo for white backgrounds" width="480"> '
                '<img src="ptyro-icon-alt-on-white.png" alt="ptyro alternate icon for white backgrounds" width="120">\n\n'
                "`ptyro-logo-alt` · `ptyro-icon-alt` · `ptyro-logo-alt-on-white` · `ptyro-icon-alt-on-white`, each as `.svg` and `.png`\n\n")
        for i, (base, title, blurb) in enumerate(rows):
            if i in heads:
                f.write(f"# {heads[i]}\n\n")
            f.write(f"## {base[:base.index('-')]}. {title}\n\n{blurb}\n\n"
                    f'<img src="ideas/{base}-logo.png" alt="{title} logo" width="480"> '
                    f'<img src="ideas/{base}-icon.png" alt="{title} icon" width="120">\n\n'
                    f"`{base}-logo.svg` · `{base}-logo.png` · `{base}-icon.svg` · `{base}-icon.png`\n\n")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "docs/logos")
