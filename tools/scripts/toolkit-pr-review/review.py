#!/usr/bin/env python3
"""toolkit-pr-review: the deterministic half of the review skill.

The invoking agent decides whether a defect is real and how to word it. This code pins
sources, routes modules, validates two source passes and preserves immutable evidence
through collect, history matching, formatting and controlled publication. All repository
entrypoints share it. No command invokes a model.

Usage:
    review.py prepare --pr 4777 [--repo owner/name]
    review.py prepare --local [--branch REF] [--base REF]
    review.py prepare --from-diff F --from-meta F     # offline, no network

Exit codes:
    0  ok
    1  error
    2  the run would exceed --max-total-tokens; context.json is not written
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import budget            # noqa: E402
import classify          # noqa: E402
import diffparse         # noqa: E402
import ghsource          # noqa: E402
import harness           # noqa: E402

SCHEMA_VERSION = 5


def _snapshot_path(work: Path, path: str) -> Path:
    """Mirror the repo tree under files/.

    The scheme this replaces escaped `/` to `__`, which is not injective:
    `gears/mini__chat/src/lib.rs` and `gears/mini/chat/src/lib.rs` produce the same name,
    so one snapshot silently overwrote the other and a shard reviewed the wrong file's
    contents under the right file's name.
    """
    target = (work / "files" / path).resolve()
    root = (work / "files").resolve()
    if root not in target.parents and target != root:
        raise ghsource.SourceError(f"refusing to write a snapshot outside files/: {path}")
    return target


# RUST-DEP-001 compares these two against each other, so when a PR changes one the
# other is snapshotted as read-only context. See the counterpart block in `prepare`.
ADVISORY_PAIR = ("deny.toml", ".cargo/audit.toml")


def cmd_prepare(args: argparse.Namespace) -> int:
    if args.work_dir:
        with harness.lock(args.work_dir):
            return _prepare(args)
    return _prepare(args)


def _prepare(args: argparse.Namespace) -> int:
    work = Path(args.work_dir) if args.work_dir else Path(
        tempfile.mkdtemp(prefix="toolkit-pr-review.")
    )
    work = work.resolve()
    if (work / "context.json").exists() or (work / "manifest.json").exists():
        raise harness.Error("existing work directory is immutable; prepare a new directory (schema 4 is not upgraded)")
    (work / "files").mkdir(parents=True, exist_ok=True)
    (work / "out").mkdir(exist_ok=True)

    # --- resolve the target and get the diff -------------------------------------
    if args.from_diff:
        diff_text = Path(args.from_diff).read_text(encoding="utf-8", errors="replace")
        meta = json.loads(Path(args.from_meta).read_text(encoding="utf-8")) if args.from_meta else {}
        repo = meta.get("repo") or args.repo or "offline/fixture"
        mode = "pr" if meta.get("number") else "local"
        head_sha = meta.get("headRefOid", "")
        base_sha = meta.get("baseRefOid", "")
        pr_number = meta.get("number")
    elif args.pr:
        repo = ghsource.resolve_repo(args.repo)
        meta = ghsource.fetch_pr_meta(args.pr, repo)
        head_sha, base_sha = meta["headRefOid"], meta["baseRefOid"]
        pr_number, mode = args.pr, "pr"
        if not ghsource.ensure_objects(repo, pr_number, [head_sha, base_sha]):
            raise harness.Error("pinned Git graph unavailable; cannot prove merge base")
        base_sha = ghsource.run(["git", "merge-base", base_sha, head_sha]).strip()
        diff_text = ghsource.run(["git", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--find-renames", base_sha, head_sha, "--"])
    else:
        repo = ghsource.resolve_repo(args.repo)
        branch = args.branch or ghsource.run(
            ["git", "symbolic-ref", "--short", "-q", "HEAD"]).strip()
        if not branch:
            print("HEAD is detached; pass --branch", file=sys.stderr)
            return 1
        base_sha = ghsource.local_base(args.base, branch)
        head_sha = ghsource.run(["git", "rev-parse", branch]).strip()
        pr_number, mode = None, "local"
        meta = {"branch": branch, "base": base_sha}
        diff_text = ghsource.run(["git", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--find-renames", base_sha, head_sha, "--"])

    # Every mode writes meta.json: the harnesses read the title, the branch and the SHAs
    # from it for the summary, and a mode-dependent file means each of them needs a
    # fallback path that only runs in local mode and therefore never gets exercised.
    (work / "meta.json").write_text(json.dumps(meta, indent=2), encoding="utf-8")
    (work / "diff.patch").write_text(diff_text, encoding="utf-8")
    parsed = diffparse.parse(diff_text)

    # --- classify -----------------------------------------------------------------
    in_scope = [p for p in sorted(parsed) if classify.in_scope(p)]
    skipped = [p for p in sorted(parsed) if not classify.in_scope(p)]

    offline = bool(args.from_diff)
    blob_source = "fixture" if offline else "git"
    if not offline:
        shas = [s for s in (head_sha, base_sha) if s]
        if not ghsource.ensure_objects(repo, pr_number, shas):
            blob_source = "contents-api"

    files: dict[str, dict] = {}
    for path in in_scope:
        fd = parsed[path]
        ref = base_sha if fd.status == "deleted" else head_sha
        content: bytes | None = None
        if classify.snapshotable(path) and not offline and ref:
            content = ghsource.read_blob_git(ref, path)
            if content is None:
                content = ghsource.read_blob_api(repo, ref, path)
                if content is not None:
                    blob_source = "contents-api"
            if content is None:
                # Both readers failed for a file the contract says to snapshot. Carrying
                # on leaves snapshot=None, which an agent cannot tell apart from a file
                # that is deliberately never snapshotted, so it reviews the diff alone
                # and nothing says the source was missing. Offline runs and files in
                # NEVER_SNAPSHOT do not reach here.
                raise ghsource.SourceError(
                    f"no snapshot for {path} at {ref}: neither the local object database "
                    f"nor the contents API returned it"
                )
        rec: dict = {
            "status": fd.status,
            "old_path": fd.old_path,
            "manifest": classify.is_manifest(path),
            "bytes": 0,
            "est_tokens": 0,
            "snapshot": None,
            "snapshot_ref": "base" if fd.status == "deleted" else "head",
            "ranges": {
                "right": [list(r) for r in fd.right],
                "left": [list(r) for r in fd.left],
            },
        }
        if content is not None:
            target = _snapshot_path(work, path)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
            rec["snapshot"] = f"files/{path}"
            rec["bytes"] = len(content)
        diff_bytes = len(fd.text().encode())
        rec["est_tokens"] = budget.est_tokens(rec["bytes"] + diff_bytes)
        files[path] = rec

    # --- advisory counterpart -------------------------------------------------------
    # RUST-DEP-001 asks whether `.cargo/audit.toml` and `deny.toml` have drifted apart,
    # an advisory accepted in one but not the other. When the PR touches only one of
    # them the other is not in the diff, so the agent had nothing to compare against and
    # the criterion could not fire. Snapshot the counterpart as read-only context: no
    # `context.json` entry, so it stays out of `all_files` and `manifest_files` and has
    # no `ranges.right`, and a finding still anchors on the file the PR actually changed.
    # A counterpart that is absent from the repo leaves nothing under `files/`, which is
    # correct: there is nothing to drift from.
    if not offline and head_sha:
        for present, missing in (ADVISORY_PAIR, ADVISORY_PAIR[::-1]):
            if present in files and missing not in files:
                extra = ghsource.read_blob_git(head_sha, missing)
                if extra is None:
                    extra = ghsource.read_blob_api(repo, head_sha, missing)
                if extra is not None:
                    target = _snapshot_path(work, missing)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(extra)

    # --- agents -------------------------------------------------------------------
    # One agent per rule module, each reading the whole diff. RUST-DEP-001 applies only to
    # manifests, so that list is precomputed here and the security agent is told what it
    # owns instead of re-deriving it. `toolkit` gets every file: nearly all Rust in this
    # repository is gear code built on ToolKit, and a path or symbol filter missed files
    # that use it, such as integration tests that import `toolkit::`.
    #
    # A PR with no `.rs` file spawns no agent at all. Seven agents over a manifest-only or
    # docs-only diff cost a full review's worth of tokens to report next to nothing.
    manifest_files = [p for p in in_scope if files[p]["manifest"]]
    has_rust = any(classify.is_rust(p) for p in in_scope)

    corpus_tokens = sum(files[p]["est_tokens"] for p in in_scope)
    diff_tokens = budget.est_tokens(len(diff_text.encode()))
    costs, post_cost = budget.pipeline_costs(corpus_tokens, diff_tokens) if has_rust else ({}, 0)
    cost = sum(costs.values()) + post_cost
    if cost > args.max_total_tokens:
        heavy = sorted(in_scope, key=lambda p: -files[p]["est_tokens"])[:5]
        print(
            f"This review needs about {cost:,} input tokens across {len(costs)} agents, over the "
            f"--max-total-tokens limit of {args.max_total_tokens:,}.\n"
            f"Every subject agent reads the whole diff, so cost scales with the PR, not the "
            f"agent count.\n"
            f"Heaviest files: {', '.join(heavy)}.\n"
            f"Narrow the review or raise the limit deliberately.",
            file=sys.stderr,
        )
        return 2

    agents = [
        {"name": m,
         "rules": f"docs/toolkit-pr-review/rules/{m}.md",
         "instructions": "docs/toolkit-pr-review/agents/subject.md",
         "files": in_scope,
         "manifest_files": (manifest_files if m == "security" else []),
         "est_tokens": costs[m]}
        for m in budget.MODULES if m in costs
    ]
    if has_rust:
        agents.append({
            "name": "architecture",
            "rules": "docs/toolkit-pr-review/agents/architecture.md",
            "instructions": "docs/toolkit-pr-review/agents/architecture.md",
            "files": in_scope,
            "manifest_files": [],
            "est_tokens": costs["architecture"],
        })

    ctx = {
        "schema_version": SCHEMA_VERSION,
        "mode": mode, "repo": repo, "pr_number": pr_number,
        "branch": meta.get("branch", ""),
        "source_repository": str(Path.cwd().resolve()),
        "base_ref_sha": meta.get("baseRefOid", base_sha),
        "head_sha": head_sha, "base_sha": base_sha,
        "blob_source": blob_source,
        "work_dir": str(work),
        "files": files,
        "all_files": in_scope,
        "skipped_files": skipped,
        "manifest_files": manifest_files,
        "agents": agents,
        "totals": {"files": len(in_scope), "skipped": len(skipped),
                   "agents": len(agents), "source_passes": 2 * len(agents),
                   "post_est_tokens": post_cost, "est_tokens": cost},
    }
    harness.freeze(work, ctx, args.source_root)

    print(f"{len(in_scope)} files in scope, {len(skipped)} skipped (not Rust or a manifest)")
    if not has_rust:
        print("no .rs file changed: no agents to spawn, nothing will be reviewed")
    print(f"{len(agents)} agents, ~{cost:,} input tokens estimated")
    print(f"blob source: {blob_source}")
    for a in agents:
        print(f"  {a['name']:<13} {len(a['files'])} files, ~{a['est_tokens']:,} tokens")
    print(f"WORK_DIR={work}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="review.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("prepare", help="resolve the target, shard it, snapshot sources")
    g = p.add_mutually_exclusive_group()
    g.add_argument("--pr", type=int, help="pull request number")
    g.add_argument("--local", action="store_true", help="review a local branch")
    p.add_argument("--repo", help="owner/name; inferred from the git remote otherwise")
    p.add_argument("--branch", help="local mode: branch under review (default: HEAD)")
    p.add_argument("--base", help="local mode: base ref (default: origin/HEAD, then main, master)")
    p.add_argument("--work-dir", help="reuse a directory instead of creating one")
    p.add_argument("--from-diff", help="offline: read the diff from this file")
    p.add_argument("--from-meta", help="offline: read gh pr view JSON from this file")
    p.add_argument("--source-root", help="offline only: fixture directory containing base/ and head/")
    p.add_argument("--max-total-tokens", type=int, default=budget.DEFAULT_MAX_TOTAL_TOKENS,
                   help="refuse the run above this estimate; context.json is not written")
    p.set_defaults(func=cmd_prepare)
    harness.add_commands(sub)

    args = ap.parse_args(argv)
    try:
        return args.func(args)
    except (ghsource.SourceError, harness.Error, KeyError, TypeError, ValueError, AttributeError, OSError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
