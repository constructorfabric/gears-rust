#!/usr/bin/env python3
"""
Check that the crates listed in `tools/scripts/pure_crates.toml` stay free of I/O.

A pure crate (for example the BSS Rating evaluation core) must not reach an async
runtime, a database, an HTTP stack or ClientHub through its normal dependency tree,
directly or transitively. For each listed package this runs

    cargo tree -p <package> -e normal --prefix none --format {p}

and fails when any package name in that tree is on the crate's deny list. Dev and
build dependencies are not part of what the crate links, so they are not checked.

Exit codes:
  0 - Every listed crate is clean
  1 - A listed crate reaches a denied crate, or `cargo tree` failed
"""

import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Dict, List, Set

CONFIG = Path(__file__).with_name("pure_crates.toml")


def tree_packages(package: str) -> Set[str]:
    """The package names in `package`'s normal dependency tree, itself excluded."""
    output = subprocess.run(
        [
            "cargo",
            "tree",
            "-p",
            package,
            "-e",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    names = set()
    for line in output.splitlines():
        # `{p}` renders "name vX.Y.Z (path)"; "(*)" marks an already-printed subtree.
        name = line.strip().split(" ", 1)[0]
        if name and name != package:
            names.add(name)
    return names


def check(config: Dict) -> List[str]:
    failures = []
    for entry in config.get("crate", []):
        package = entry["package"]
        denied = set(entry["deny"])
        try:
            found = tree_packages(package) & denied
        except subprocess.CalledProcessError as error:
            failures.append(f"{package}: cargo tree failed: {error.stderr.strip()}")
            continue
        if found:
            failures.append(
                f"{package} must stay pure ({entry.get('reason', 'see pure_crates.toml')}); "
                f"its dependency tree reaches: {', '.join(sorted(found))}"
            )
        else:
            print(f"ok: {package} reaches none of {len(denied)} denied crates")
    return failures


def main() -> int:
    with CONFIG.open("rb") as handle:
        config = tomllib.load(handle)
    failures = check(config)
    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
