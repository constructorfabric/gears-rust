#!/usr/bin/env python3
"""Is there anything for release-plz to publish? Decides whether the release gate runs CodeQL.

A workspace crate counts as pending when its manifest version is absent from crates.io
(sparse index). Crates with `publish = false` are skipped: the manifest alone decides
that, as in release-plz.toml. Fail closed: a crate that cannot be checked, or a cargo
failure, counts as pending, so the worst outcome is an unneeded CodeQL run.

Writes `release=true|false` to $GITHUB_OUTPUT when set, and prints the pending crates.
Needs `cargo` on PATH (set RUSTUP_TOOLCHAIN=stable to keep rustup from installing the
toolchain pinned in rust-toolchain.toml; `cargo metadata --no-deps` does not need it).
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

INDEX = "https://index.crates.io"


def index_path(name: str) -> str:
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def published(name: str, version: str) -> bool | None:
    """True/False, or None when the index could not be read."""
    url = f"{INDEX}/{index_path(name)}"
    for attempt in range(3):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "release-pending-check"}), timeout=20) as r:
                return any(json.loads(line)["vers"] == version for line in r.read().decode().splitlines() if line.strip())
        except urllib.error.HTTPError as e:
            if e.code in (403, 404):  # never published
                return False
        except Exception:
            pass
    return None


def main() -> int:
    try:
        meta = json.loads(subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            check=True, capture_output=True, text=True).stdout)
    except Exception as e:  # fail closed
        print(f"::warning::cargo metadata failed ({e}); assuming a release is pending")
        pending = ["<cargo metadata failed>"]
    else:
        crates = [(p["name"], p["version"]) for p in meta["packages"] if p["publish"] != []]
        with ThreadPoolExecutor(16) as ex:
            results = list(ex.map(lambda c: published(*c), crates))
        pending = []
        for (name, version), ok in zip(crates, results):
            if ok is None:
                print(f"::warning::cannot read the crates.io index for {name}; counting it as pending")
            if ok is not True:
                pending.append(f"{name} {version}")
        print(f"{len(crates)} publishable crates checked")

    print(f"{len(pending)} pending: {', '.join(pending[:20])}{' ...' if len(pending) > 20 else ''}")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a") as f:
            f.write(f"release={'true' if pending else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
