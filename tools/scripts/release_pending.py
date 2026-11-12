#!/usr/bin/env python3
"""Is there anything for release-plz to publish? Decides whether the release gate runs CodeQL.

A workspace crate counts as pending when its manifest version is absent from crates.io
(sparse index). Crates with `publish = false` are skipped, except those marked
`git_only = true` in release-plz.toml (Docker image + Helm chart artifacts): release-plz
tags them `<name>-v<version>`, so they are pending until that tag exists on origin
(`git ls-remote --tags origin`, independent of local clone depth). Fail closed: a crate
or tag list that cannot be checked, a bad release-plz.toml, or a cargo failure counts as
pending, so the worst outcome is an unneeded CodeQL run.

Writes `release=true|false` to $GITHUB_OUTPUT when set, and prints the pending crates.
Needs `cargo` on PATH (set RUSTUP_TOOLCHAIN=stable to keep rustup from installing the
toolchain pinned in rust-toolchain.toml; `cargo metadata --no-deps` does not need it).
"""
import json
import os
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

INDEX = "https://index.crates.io"
ROOT = Path(__file__).resolve().parents[2]


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


def git_only_names(toml_text: str) -> set[str]:
    """Names of `[[package]]` entries with `git_only = true` in release-plz.toml."""
    return {p["name"] for p in tomllib.loads(toml_text).get("package", []) if p.get("git_only") is True}


def remote_tags(ls_remote_output: str) -> set[str]:
    """Tag names from `git ls-remote --tags` output (peeled `^{}` lines folded in)."""
    tags = set()
    for line in ls_remote_output.splitlines():
        ref = line.split("\t")[-1].strip()
        if ref.startswith("refs/tags/"):
            tags.add(ref.removeprefix("refs/tags/").removesuffix("^{}"))
    return tags


def pending_crates(packages, crates_io, git_only: set[str], tags: set[str] | None) -> list[str]:
    """Pending entries. `crates_io(name, version)` -> True/False/None (unreadable);
    `tags` is the remote tag set, or None when it could not be listed (fail closed)."""
    publishable = [(p["name"], p["version"]) for p in packages if p["publish"] != []]
    tagged = [(p["name"], p["version"]) for p in packages if p["publish"] == [] and p["name"] in git_only]
    with ThreadPoolExecutor(16) as ex:
        results = list(ex.map(lambda c: crates_io(*c), publishable))
    pending = []
    for (name, version), ok in zip(publishable, results):
        if ok is None:
            print(f"::warning::cannot read the crates.io index for {name}; counting it as pending")
        if ok is not True:
            pending.append(f"{name} {version}")
    for name, version in tagged:
        if tags is None or f"{name}-v{version}" not in tags:
            pending.append(f"{name} {version} (no tag)")
    print(f"{len(publishable)} publishable crates and {len(tagged)} git_only packages checked")
    return pending


def main() -> int:
    try:
        meta = json.loads(subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            check=True, capture_output=True, text=True).stdout)
    except Exception as e:  # fail closed
        print(f"::warning::cargo metadata failed ({e}); assuming a release is pending")
        pending = ["<cargo metadata failed>"]
    else:
        try:
            git_only = git_only_names((ROOT / "release-plz.toml").read_text())
        except Exception as e:  # fail closed: treat every publish=false crate as git_only
            print(f"::warning::cannot parse release-plz.toml ({e}); counting publish=false crates as pending")
            git_only = {p["name"] for p in meta["packages"] if p["publish"] == []}
        try:
            tags = remote_tags(subprocess.run(
                ["git", "ls-remote", "--tags", "origin"],
                check=True, capture_output=True, text=True, timeout=60).stdout)
        except Exception as e:  # fail closed
            print(f"::warning::git ls-remote failed ({e}); counting git_only packages as pending")
            tags = None
        pending = pending_crates(meta["packages"], published, git_only, tags)

    print(f"{len(pending)} pending: {', '.join(pending[:20])}{' ...' if len(pending) > 20 else ''}")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a") as f:
            f.write(f"release={'true' if pending else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
