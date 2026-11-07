#!/usr/bin/env python3
"""Record the version table's newest-version replies as test fixtures (ROADMAP BOOT0, SPEC §24.1).

Reads crates/ptyro-agents/src/catalog.toml, fetches each item's `latest` source once, trims the
reply to the fields the parser reads (a GitHub release's assets, Node's older releases), and
writes it to crates/ptyro-agents/tests/fixtures/catalog/<id>-latest.<ext>. Antigravity's
manifest is recorded for two platforms. Prints the newest version of each, so a run is also a
live check that every source still answers. The tests never fetch: review the diff, then update
the versions the tests expect (`newest_versions_from_recorded_replies`).

  python3 scripts/catalog-fixtures.py
"""
import json
import pathlib
import sys
import tomllib
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
TABLE = ROOT / "crates/ptyro-agents/src/catalog.toml"
OUT = ROOT / "crates/ptyro-agents/tests/fixtures/catalog"
AGY_PLATFORMS = ["linux_amd64", "windows_arm64"]


def fetch(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "ptyro-catalog-fixtures"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode()


def trim(source: str, body: str):
    """The reply as the fixture keeps it, and the version it holds."""
    if source == "plain-text":
        return body, body.strip()
    d = json.loads(body)
    if source == "npm":
        keep = {k: d[k] for k in ("name", "version", "description", "engines", "bin", "os", "cpu") if k in d}
        if "dist" in d:
            keep["dist"] = {k: d["dist"][k] for k in ("tarball", "integrity", "shasum") if k in d["dist"]}
        return keep, d["version"]
    if source == "github-release":
        keep = {k: d[k] for k in ("tag_name", "name", "draft", "prerelease", "published_at", "html_url") if k in d}
        keep["assets"] = [{k: a[k] for k in ("name", "browser_download_url") if k in a} for a in d.get("assets", [])[:2]]
        return keep, d["tag_name"]
    if source == "agy-manifest":
        return d, d["version"]
    if source == "node-dist":
        # Down to the newest LTS, so the fixture keeps any newer non-LTS release above it.
        n = next(i for i, r in enumerate(d) if r.get("lts") is not False)
        return d[: n + 1], d[n]["version"]
    raise SystemExit(f"unknown source {source}")


def main() -> int:
    table = tomllib.loads(TABLE.read_text())
    OUT.mkdir(parents=True, exist_ok=True)
    for item in table["item"]:
        latest = item.get("latest")
        if not latest:
            continue
        url, source = latest["url"], latest["source"]
        targets = [(p, url.replace("{platform}", p), f"-{p}") for p in AGY_PLATFORMS] if "{platform}" in url else [(None, url, "")]
        for _, u, suffix in targets:
            try:
                kept, version = trim(source, fetch(u))
            except Exception as e:  # one dead source shouldn't stop the others
                print(f"{item['id']}{suffix}: {u}: {e}", file=sys.stderr)
                continue
            ext = "txt" if source == "plain-text" else "json"
            path = OUT / f"{item['id']}-latest{suffix}.{ext}"
            path.write_text(kept if isinstance(kept, str) else json.dumps(kept, indent=1) + "\n")
            print(f"{item['id']}{suffix}: {version}  ({path.relative_to(ROOT)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
