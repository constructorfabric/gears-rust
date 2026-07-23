#!/usr/bin/env python3
"""
Validate that every publishable crate's packaging metadata files exist.

`cargo publish`/`cargo package` refuse to package a crate if a declared
`readme` or `license-file` path doesn't exist relative to that crate's
Cargo.toml — but that's only caught when actually packaging/publishing,
which is expensive (it compiles the crate) and only runs in the release
job. This script checks the same file-existence rules statically, with no
cargo invocation at all, so it's cheap enough to run on every CI build.

Exit codes:
  0 - All publishable crates have valid readme/license-file paths
  1 - One or more publishable crates reference a missing file
"""

import sys
import tomllib
from pathlib import Path
from typing import List, Tuple

# Directories to skip entirely while walking for Cargo.toml files.
SKIP_DIR_NAMES = {"target", ".git"}

# Relative to the workspace root, directories excluded from the workspace
# (mirrors `exclude` in the root Cargo.toml) — not real workspace members,
# so their packaging metadata is irrelevant here.
EXCLUDED_DIRS = {"tools/fuzz"}


def find_manifests(workspace_root: Path) -> List[Path]:
    """Find all Cargo.toml files in the workspace, skipping excluded dirs."""
    manifests = []
    for path in workspace_root.rglob("Cargo.toml"):
        rel_parts = path.relative_to(workspace_root).parts
        if any(part in SKIP_DIR_NAMES for part in rel_parts):
            continue
        rel_dir = "/".join(rel_parts[:-1])
        if any(rel_dir == excluded or rel_dir.startswith(excluded + "/") for excluded in EXCLUDED_DIRS):
            continue
        manifests.append(path)
    return sorted(manifests)


def is_publishable(package: dict) -> bool:
    """Mirror cargo's `publish` field semantics: only `false`/`[]` disable publishing."""
    publish = package.get("publish")
    if publish is False:
        return False
    if isinstance(publish, list) and len(publish) == 0:
        return False
    return True


def extract_package_table(manifest_text: str) -> dict:
    """Parse just the `[package]` (and `[package.*]`) tables of a manifest.

    Cargo's own TOML parser tolerates multi-line inline tables (commonly used
    for local path dependencies with wrapped formatting), but Python's strict
    `tomllib` rejects them. Since none of that matters for packaging-metadata
    checks, we isolate the `[package]` block — which never uses that pattern
    in this workspace — and parse only that, sidestepping the incompatibility.
    """
    lines = manifest_text.splitlines(keepends=True)
    start = None
    end = len(lines)
    for i, line in enumerate(lines):
        stripped = line.strip()
        if start is None:
            if stripped == "[package]":
                start = i
            continue
        if stripped.startswith("[") and not stripped.startswith("[package."):
            end = i
            break
    if start is None:
        return {}
    return tomllib.loads("".join(lines[start:end]))


def check_manifest(manifest_path: Path, workspace_root: Path) -> Tuple[bool, List[Tuple[str, str, str]]]:
    """Return (is_publishable_crate, violations) for one manifest.

    `violations` is a list of (field, declared_value, expected_path).
    """
    data = extract_package_table(manifest_path.read_text(encoding="utf-8"))

    package = data.get("package")
    if package is None:
        return False, []  # virtual workspace manifest, not a real crate

    if not is_publishable(package):
        return False, []

    crate_dir = manifest_path.parent
    violations = []

    for field in ("readme", "license-file"):
        value = package.get(field)
        if not isinstance(value, str):
            continue  # absent, `false`, or workspace-inherited (never used as such today)
        if not (crate_dir / value).is_file():
            violations.append((field, value, str((crate_dir / value).relative_to(workspace_root))))

    return True, violations


def main() -> int:
    script_dir = Path(__file__).parent
    workspace_root = script_dir.parent.parent

    manifests = find_manifests(workspace_root)
    if not manifests:
        print(f"Error: no Cargo.toml files found under {workspace_root}", file=sys.stderr)
        return 1

    checked = 0
    all_violations = []

    for manifest_path in manifests:
        is_publishable_crate, violations = check_manifest(manifest_path, workspace_root)
        if not is_publishable_crate:
            continue
        checked += 1
        if violations:
            all_violations.append((manifest_path, violations))

    if all_violations:
        print("=" * 80, file=sys.stderr)
        print("PACKAGING METADATA VIOLATIONS DETECTED", file=sys.stderr)
        print("=" * 80, file=sys.stderr)
        print(file=sys.stderr)
        print(
            "The following crates declare a `readme`/`license-file` path "
            "that does not exist. `cargo publish` will fail on these:",
            file=sys.stderr,
        )
        print(file=sys.stderr)

        for manifest_path, violations in all_violations:
            rel_manifest = manifest_path.relative_to(workspace_root)
            print(f"  [X] {rel_manifest}", file=sys.stderr)
            for field, declared, expected_rel in violations:
                print(f"      {field} = \"{declared}\" -> missing {expected_rel}", file=sys.stderr)
            print(file=sys.stderr)

        print("=" * 80, file=sys.stderr)
        print(f"Summary: {checked} valid, {len(all_violations)} invalid", file=sys.stderr)
        print("=" * 80, file=sys.stderr)
        return 1

    print(f"OK: {checked} publishable crates checked")
    return 0


if __name__ == "__main__":
    sys.exit(main())
