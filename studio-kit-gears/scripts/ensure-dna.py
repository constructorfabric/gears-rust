#!/usr/bin/env python3
"""Ensure guidelines/DNA is available before a gears preset relies on it.

Constructor Studio kit script: lives in studio-kit-gears/scripts/ensure-dna.py.
Resolves the actual on-disk location of `{dna_root}` through `cfs resolve-vars`
rather than a hardcoded path, because where a kit resource lands depends
entirely on how it was installed (in-repo "register" mode keeps it at its
source path; a standalone "copy" install places it under
`.cf-studio/config/kits/<slug>/...` instead) — never on a fixed project-root-
relative path. `{dna_root}` itself is the submodule's root directory, not a
path inside it, deliberately: an uninitialized submodule's mount-point
directory still exists on disk (empty), so `cfs` never fails to load the kit
manifest over this resource the way it would for a resource pointed inside an
unfetched submodule. The canary file this script actually checks is
`languages/RUST.md` under whatever `{dna_root}` resolves to.

`guidelines/DNA` is additionally a git submodule in the `gears-rust` repo
itself, so a checkout can have it registered but not fetched. A kit installed
standalone (outside `gears-rust`) has no such submodule at all; there, the kit
manifest must already have bundled the needed files as plain copies at install
time. This script tells the two cases apart and gives the right remediation
for each, instead of silently reviewing or generating against absent files.
"""

import json
import os
import shutil
import subprocess
import sys

_GITMODULES_MARKER = 'submodule "guidelines/DNA"'


def _find_project_root():
    """Find project root via git rev-parse, then walk up for .gitmodules/.git."""
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except FileNotFoundError:
        pass
    current = os.path.dirname(os.path.abspath(__file__))
    for _ in range(25):
        if os.path.isdir(os.path.join(current, ".git")):
            return current
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent
    return os.getcwd()


def _is_dna_submodule_checkout(root):
    gitmodules = os.path.join(root, ".gitmodules")
    if not os.path.isfile(gitmodules):
        return False
    try:
        with open(gitmodules, encoding="utf-8") as f:
            return _GITMODULES_MARKER in f.read()
    except OSError:
        return False


def _resolve_dna_rust_path():
    """Resolve the actual installed location of `{dna_root}/languages/RUST.md`
    via `cfs resolve-vars` rather than a hardcoded path.

    Returns the resolved absolute path, or None if `cfs` isn't available or
    `{dna_root}` didn't resolve (the caller falls back to a best-effort guess
    in that case).
    """
    cfs_cmd = shutil.which("cfs")
    if cfs_cmd is None:
        return None
    try:
        result = subprocess.run(
            [cfs_cmd, "--json", "resolve-vars", "--kit", "gears", "--flat"],
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            return None
        data = json.loads(result.stdout)
        dna_root = data.get("variables", {}).get("dna_root")
        if not dna_root:
            return None
        return os.path.join(dna_root, "languages", "RUST.md")
    except (OSError, json.JSONDecodeError):
        return None


def main():
    root = _find_project_root()
    canary = _resolve_dna_rust_path()
    best_effort = canary is None
    if best_effort:
        canary = os.path.join(root, "guidelines", "DNA", "languages", "RUST.md")

    if os.path.isfile(canary) and os.path.getsize(canary) > 0:
        print(f"OK: {canary} present")
        return 0

    if _is_dna_submodule_checkout(root):
        submodule_root = os.path.join(root, "guidelines", "DNA")
        submodule_git_marker = os.path.join(submodule_root, ".git")
        if not os.path.exists(submodule_git_marker):
            # Genuinely never fetched here — nothing local to lose, so a plain
            # (non-force) init is the right and sufficient tool.
            print(
                f"{canary} is missing and guidelines/DNA has never been fetched "
                "here; initializing it now..."
            )
            result = subprocess.run(
                ["git", "submodule", "update", "--init", "--recursive", "--", "guidelines/DNA"],
                cwd=root,
            )
        else:
            # The submodule is already checked out — only the canary file
            # itself is missing/modified. Restore just that one file from the
            # submodule's own recorded commit. Deliberately never `--force`
            # the whole submodule here: that discards every local change
            # anywhere inside guidelines/DNA, not just this one file, with no
            # confirmation and no backup.
            relative_canary = os.path.relpath(canary, submodule_root)
            if relative_canary.startswith(".."):
                print(
                    f"ERROR: {canary} does not resolve under {submodule_root}; "
                    "cannot target a single-file restore.",
                    file=sys.stderr,
                )
                return 1
            # The worktree copy can be empty/missing while the index still
            # holds different, staged content (e.g. `git add`ed then the
            # worktree file got truncated some other way) — `git checkout
            # HEAD -- <path>` would silently overwrite both, discarding the
            # staged change. Check for that before restoring, not after.
            staged_diff = subprocess.run(
                ["git", "diff", "--cached", "--quiet", "--", relative_canary],
                cwd=submodule_root,
            )
            if staged_diff.returncode != 0:
                print(
                    f"ERROR: {relative_canary} has staged changes in the index "
                    "that differ from HEAD; restoring it now would silently "
                    "discard them. Resolve manually (`git status`/`git diff "
                    "--cached` in guidelines/DNA) before re-running.",
                    file=sys.stderr,
                )
                return 1
            print(
                f"{canary} is missing but guidelines/DNA is already checked out "
                f"here; restoring just {relative_canary} from its recorded "
                "commit (leaving any other local changes in guidelines/DNA untouched)..."
            )
            result = subprocess.run(
                ["git", "checkout", "HEAD", "--", relative_canary],
                cwd=submodule_root,
            )
        # Re-resolve: a `cfs`-less best-effort guess may have been wrong before
        # the submodule existed at all.
        canary = _resolve_dna_rust_path() or canary
        if result.returncode == 0 and os.path.isfile(canary) and os.path.getsize(canary) > 0:
            print(f"OK: {canary} fetched")
            return 0
        print(
            "ERROR: could not restore guidelines/DNA automatically.\n"
            "Run this manually, then retry:\n"
            "  git submodule update --init --recursive -- guidelines/DNA",
            file=sys.stderr,
        )
        return 1

    print(
        f"ERROR: {canary} is missing and this project has no guidelines/DNA submodule "
        "(standalone kit install). The kit manifest should have bundled this file as a "
        "plain copy at install time — this looks like an incomplete or stale install.\n"
        "Reinstall or refresh the kit, then retry:\n"
        "  cfs kit update\n"
        "  cfs kit install --force",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
