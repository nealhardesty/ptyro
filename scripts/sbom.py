#!/usr/bin/env python3
"""A software bill of materials for a release (ROADMAP H2.2): CycloneDX 1.5 JSON from Cargo.lock.

Every crate the binary can be built from, with its version, where it comes from and the SHA-256
Cargo recorded for it, so a user (or their scanner) can tell what is inside a ptyro release and
match it against advisories. Deterministic: the same lockfile gives the same file (the timestamp
is SOURCE_DATE_EPOCH, else the last commit's time).

  python3 scripts/sbom.py [-o FILE]       default: dist/ptyro-<version>.cdx.json
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib
import uuid
from datetime import datetime, timezone

ROOT = pathlib.Path(__file__).resolve().parent.parent


def version() -> str:
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def timestamp() -> str:
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if not epoch:
        out = subprocess.run(["git", "log", "-1", "--format=%ct"], cwd=ROOT, capture_output=True, text=True)
        epoch = out.stdout.strip() or "0"
    return datetime.fromtimestamp(int(epoch), timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def main() -> int:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    ver = version()
    members = {p["name"] for p in lock["package"] if "source" not in p}
    components = []
    for p in sorted(lock["package"], key=lambda p: (p["name"], p["version"])):
        if p["name"] in members:
            continue  # ptyro's own crates are the application, described below
        c = {
            "type": "library",
            "bom-ref": f"pkg:cargo/{p['name']}@{p['version']}",
            "name": p["name"],
            "version": p["version"],
            "purl": f"pkg:cargo/{p['name']}@{p['version']}",
        }
        if p.get("checksum"):
            c["hashes"] = [{"alg": "SHA-256", "content": p["checksum"]}]
        src = p.get("source", "")
        if src.startswith("git+"):
            c["externalReferences"] = [{"type": "vcs", "url": re.sub(r"^git\+|#.*$", "", src)}]
        components.append(c)
    digest = subprocess.run(["sha256sum" if sys.platform != "darwin" else "shasum", *([] if sys.platform != "darwin" else ["-a", "256"]), str(ROOT / "Cargo.lock")], capture_output=True, text=True).stdout.split()[0]
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, f'ptyro/{ver}/{digest}')}",
        "version": 1,
        "metadata": {
            "timestamp": timestamp(),
            "component": {
                "type": "application",
                "bom-ref": f"pkg:cargo/ptyro@{ver}",
                "name": "ptyro",
                "version": ver,
                "purl": f"pkg:cargo/ptyro@{ver}",
                "licenses": [{"license": {"id": "MIT"}}],
                "externalReferences": [{"type": "vcs", "url": "https://github.com/nealhardesty/ptyro"}],
            },
            "properties": [{"name": "ptyro:source", "value": "Cargo.lock"}, {"name": "ptyro:lock-sha256", "value": digest}],
        },
        "components": components,
    }
    out = pathlib.Path(sys.argv[sys.argv.index("-o") + 1]) if "-o" in sys.argv else ROOT / "dist" / f"ptyro-{ver}.cdx.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(bom, indent=2, sort_keys=True) + "\n")
    print(f"{out} ({len(components)} components)")
    return 0


sys.exit(main())
