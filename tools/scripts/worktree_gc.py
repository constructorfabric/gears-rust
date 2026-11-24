#!/usr/bin/env python3
"""List git worktrees that can be removed, and optionally remove them.

Usage:
    python3 tools/scripts/worktree_gc.py [--base REF] [--repo OWNER/NAME]
        [--fork OWNER] [--no-sizes] [--apply] [--delete-branch] [--clean-target]

For every linked worktree it prints: the branch, its pull request state, the
size of `target/`, and a verdict.

  remove  the tree is clean and its work is merged: a merged PR (whose head is
          HEAD, or HEAD is in --base), a closed PR with HEAD in --base, or a
          detached HEAD in --base
  keep    everything else: open PR, unmerged work, local changes, commits
          not in the PR, or a branch in --base without a merged PR (a branch
          just cut from base looks merged too)

Dry run by default. --apply removes the `remove` worktrees (`git worktree
remove`); --delete-branch also deletes their local branches. --clean-target
deletes `target/` in the kept worktrees; a cargo build running there fails.

PR state comes from the REST API: `gh pr list --head` returns nothing for
some branches in this setup.

Requires: git, gh CLI installed and authenticated.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

DEFAULT_REPO = "constructorfabric/gears-rust"
DEFAULT_FORK = "MikeFalcon77"
DEFAULT_BASE = "upstream/main"


@dataclass
class Worktree:
    path: str
    head: str = ""
    branch: str | None = None
    main: bool = False
    prunable: bool = False
    pr: dict | None = None
    dirty: bool = False
    in_base: bool = False
    target_kb: int | None = None
    verdict: str = "keep"
    reasons: list[str] = field(default_factory=list)


def run(*cmd: str, cwd: str | None = None, check: bool = True) -> str:
    return subprocess.run(cmd, cwd=cwd, check=check, capture_output=True, text=True).stdout


def parse_porcelain(text: str) -> list[Worktree]:
    """Parse `git worktree list --porcelain`; the first entry is the main tree."""
    trees: list[Worktree] = []
    for block in text.strip().split("\n\n"):
        wt: Worktree | None = None
        for line in block.splitlines():
            key, _, value = line.partition(" ")
            if key == "worktree":
                wt = Worktree(path=value)
            elif wt is None:
                continue
            elif key == "HEAD":
                wt.head = value
            elif key == "branch":
                wt.branch = value.removeprefix("refs/heads/")
            elif key == "prunable":
                wt.prunable = True
        if wt:
            trees.append(wt)
    if trees:
        trees[0].main = True
    return trees


def find_pr(repo: str, owners: list[str], branch: str) -> dict | None:
    """Newest PR whose head is `owner:branch` for any of `owners`."""
    found = []
    for owner in owners:
        out = run(
            "gh", "api", f"repos/{repo}/pulls?head={owner}:{branch}&state=all&per_page=5",
            "--jq", "[.[] | {number, state, merged: (.merged_at != null), head: .head.sha}]",
            check=False,
        )
        if out.strip():
            found.extend(json.loads(out))
    return max(found, key=lambda p: p["number"]) if found else None


def decide(wt: Worktree) -> None:
    """Set `verdict` and `reasons` from the facts already collected."""
    if wt.prunable:
        wt.verdict, wt.reasons = "remove", ["directory is gone (prunable)"]
        return
    keep: list[str] = []
    if wt.dirty:
        keep.append("uncommitted changes")
    pr = wt.pr
    if pr and pr["merged"]:
        if not wt.in_base and pr["head"] != wt.head:
            keep.append(f"HEAD differs from merged PR #{pr['number']} head")
        why = f"PR #{pr['number']} merged"
    elif pr and pr["state"] == "closed" and wt.in_base:
        why = f"PR #{pr['number']} closed, HEAD in base"
    elif wt.in_base and not wt.branch:
        why = "detached HEAD in base"
    elif wt.in_base:
        # A branch just cut from base looks merged too; only a PR proves it.
        keep.append("HEAD in base but no merged PR (new branch?)")
    elif pr:
        keep.append(f"PR #{pr['number']} {pr['state']}")
    else:
        keep.append("no PR, not in base")
    if keep:
        wt.verdict, wt.reasons = "keep", keep
    else:
        wt.verdict, wt.reasons = "remove", [why]


def du_kb(path: Path) -> int | None:
    if not path.is_dir():
        return None
    out = run("du", "-sk", str(path), check=False)
    return int(out.split()[0]) if out.strip() else None


def human(kb: int | None) -> str:
    if kb is None:
        return "-"
    for unit in ("K", "M", "G"):
        if kb < 1024:
            return f"{kb:.0f}{unit}"
        kb /= 1024
    return f"{kb:.1f}T"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default=DEFAULT_BASE, help="ref that merged work lands in")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--fork", default=DEFAULT_FORK, help="fork owner that may also hold PR heads")
    ap.add_argument("--no-sizes", action="store_true", help="skip `du` on target/")
    ap.add_argument("--apply", action="store_true", help="remove worktrees marked remove")
    ap.add_argument("--delete-branch", action="store_true", help="with --apply, delete their branches")
    ap.add_argument("--clean-target", action="store_true", help="delete target/ in kept worktrees")
    args = ap.parse_args(argv)

    owners = [args.repo.split("/")[0], args.fork]
    trees = parse_porcelain(run("git", "worktree", "list", "--porcelain"))
    linked = [t for t in trees if not t.main]
    for wt in linked:
        if not wt.prunable:
            wt.dirty = bool(run("git", "-C", wt.path, "status", "--porcelain", check=False).strip())
            wt.in_base = subprocess.run(
                ["git", "merge-base", "--is-ancestor", wt.head, args.base], capture_output=True
            ).returncode == 0
            if not args.no_sizes:
                wt.target_kb = du_kb(Path(wt.path) / "target")
        if wt.branch:
            wt.pr = find_pr(args.repo, owners, wt.branch)
        decide(wt)

    for wt in linked:
        pr = f"#{wt.pr['number']} {'merged' if wt.pr['merged'] else wt.pr['state']}" if wt.pr else "-"
        print(f"{wt.verdict:6} {human(wt.target_kb):>6}  {pr:16} {wt.branch or '(detached)'}")
        print(f"       {wt.path} — {'; '.join(wt.reasons)}")

    if not (args.apply or args.clean_target):
        print("\ndry run; pass --apply to remove, --clean-target to free target/ in kept trees")
        return 0

    for wt in linked:
        if args.apply and wt.verdict == "remove":
            if wt.prunable:
                run("git", "worktree", "prune")
            else:
                run("git", "worktree", "remove", "--force", wt.path)
            print(f"removed {wt.path}")
            if args.delete_branch and wt.branch:
                run("git", "branch", "-D", wt.branch, check=False)
                print(f"deleted branch {wt.branch}")
        elif args.clean_target and wt.verdict == "keep" and wt.target_kb:
            shutil.rmtree(Path(wt.path) / "target", ignore_errors=True)
            print(f"cleaned {wt.path}/target ({human(wt.target_kb)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
