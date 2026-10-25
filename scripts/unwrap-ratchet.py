#!/usr/bin/env python3
"""A ratchet on `.unwrap()` and `.expect(` in code that is not a test (ROADMAP H2.1).

Counts them per crate, outside `#[cfg(test)]` modules, `tests.rs` files and `tests/`, and fails
when a crate has more than `scripts/unwrap-baseline.txt` says. Fewer is welcome: run with
`--update` to write the new, lower baseline. A new one needs a reason the code can't fail (say
so in the `expect` message) and a raise of the baseline in the same change.

  python3 scripts/unwrap-ratchet.py            check
  python3 scripts/unwrap-ratchet.py --update   rewrite the baseline
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASE = ROOT / "scripts" / "unwrap-baseline.txt"
# Crates that are test or development tooling, not shipped logic.
SKIP = {"ptyro-testkit", "ptyro-drive"}
# Whole files that are test code (their module is `#[cfg(test)]` in main.rs).
TEST_FILES = {"ptyro/src/parity.rs"}
PAT = re.compile(r"\.unwrap\(\)|\.expect\(")


def count(crate: pathlib.Path) -> int:
    n = 0
    for f in sorted((crate / "src").rglob("*.rs")):
        rel = f.relative_to(crate / "src").parts
        if f.name == "tests.rs" or "tests" in rel[:-1] or f"{crate.name}/src/{'/'.join(rel)}" in TEST_FILES:
            continue
        for line in f.read_text().splitlines():
            if line.strip().startswith("#[cfg(test)]"):
                break  # the tests are at the end of the file
            code = line.split("//", 1)[0]
            n += len(PAT.findall(code))
    return n


def main() -> int:
    now = {
        c.name: count(c)
        for c in sorted((ROOT / "crates").iterdir())
        if c.is_dir() and c.name not in SKIP and (c / "src").is_dir()
    }
    if "--update" in sys.argv:
        BASE.write_text("".join(f"{k} {v}\n" for k, v in now.items()))
        print("baseline written:", ", ".join(f"{k} {v}" for k, v in now.items()))
        return 0
    before = {k: int(v) for k, v in (l.split() for l in BASE.read_text().splitlines() if l.strip())}
    bad = [(k, before.get(k, 0), v) for k, v in now.items() if v > before.get(k, 0)]
    for k, was, is_ in bad:
        print(f"{k}: {is_} unwrap/expect in non-test code, baseline {was}", file=sys.stderr)
    if bad:
        print(
            "a new unwrap/expect needs a reason it can't fail (in its message) and a raise of "
            "scripts/unwrap-baseline.txt (`make ratchet-update`); better, handle the error",
            file=sys.stderr,
        )
        return 1
    lower = [(k, before[k], v) for k, v in now.items() if k in before and v < before[k]]
    for k, was, is_ in lower:
        print(f"{k}: down from {was} to {is_}: `make ratchet-update` keeps it there")
    print("unwrap ratchet ok:", ", ".join(f"{k} {v}" for k, v in now.items()))
    return 0


sys.exit(main())
