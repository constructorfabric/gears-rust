#!/usr/bin/env python3
"""Deterministic structural gate for a work-decomposition manifest.

This is the "strongest gate is free" check from the work-decomposition
proposal: the interpretive work (how to slice, is it feasible) is Claude's
job, but the structural truth is mechanical and has teeth independent of any
LLM. Exit code IS the gate — 0 clean, 1 on any error.

Checks, in order:
  1. JSON-schema shape          (schemas/decomposition.json)
  2. unique brief ids
  3. depends_on references resolve (no dangling id)
  4. dependency graph is acyclic (topological sort)
  5. every scope path is inside the repo and its parent dir exists
     (new-file briefs legitimately don't match an existing file yet, so we
     bound the path rather than require a match)
  6. every brief has non-empty acceptance + a test_plan

Usage:
    validate_decomposition.py <manifest.yaml|.json> [--repo-root DIR] [--json]

The manifest may be YAML or JSON. With --json, emits {"ok": bool,
"errors": [...]} to stdout (the shape the proposal's host.run binds as
decomp_validation); otherwise prints a human-readable report.
"""
import argparse
import json
import os
import sys

try:
    import yaml
except ImportError:
    yaml = None

SCHEMA_PATH = os.path.join(os.path.dirname(__file__), "..", "schemas", "decomposition.json")
GLOB_CHARS = "*?["


def load_manifest(path):
    with open(path, "r", encoding="utf-8") as fh:
        text = fh.read()
    if path.endswith((".yaml", ".yml")):
        if yaml is None:
            raise SystemExit("PyYAML not installed; convert the manifest to JSON or `pip install pyyaml`.")
        return yaml.safe_load(text)
    return json.loads(text)


def schema_errors(manifest):
    try:
        import jsonschema
    except ImportError:
        return ["jsonschema not installed; cannot run shape validation (pip install jsonschema)"]
    with open(SCHEMA_PATH, encoding="utf-8") as fh:
        schema = json.load(fh)
    validator = jsonschema.Draft7Validator(schema)
    out = []
    for err in sorted(validator.iter_errors(manifest), key=lambda e: list(e.path)):
        loc = "/".join(str(p) for p in err.path) or "(root)"
        out.append(f"schema: {loc}: {err.message}")
    return out


def literal_prefix(glob):
    """The path prefix of a glob up to the first wildcard component."""
    parts = glob.split("/")
    kept = []
    for part in parts:
        if any(c in part for c in GLOB_CHARS):
            break
        kept.append(part)
    return "/".join(kept)


def cross_brief_errors(manifest, repo_root):
    errors = []
    briefs = manifest.get("briefs", []) if isinstance(manifest, dict) else []
    ids = [b.get("id") for b in briefs if isinstance(b, dict)]

    # 2. unique ids
    seen = set()
    for bid in ids:
        if bid in seen:
            errors.append(f"duplicate brief id: {bid!r}")
        seen.add(bid)
    idset = set(ids)

    # 3. dangling deps
    for b in briefs:
        for dep in b.get("depends_on", []) or []:
            if dep not in idset:
                errors.append(f"brief {b.get('id')!r} depends_on unknown id {dep!r}")

    # 4. acyclic — Kahn's algorithm over the resolvable edges
    indeg = {bid: 0 for bid in idset}
    adj = {bid: [] for bid in idset}
    for b in briefs:
        bid = b.get("id")
        for dep in b.get("depends_on", []) or []:
            if dep in idset and bid in idset:
                adj[dep].append(bid)
                indeg[bid] += 1
    queue = [n for n, d in indeg.items() if d == 0]
    visited = 0
    while queue:
        n = queue.pop()
        visited += 1
        for m in adj[n]:
            indeg[m] -= 1
            if indeg[m] == 0:
                queue.append(m)
    if visited < len(idset):
        stuck = sorted(n for n, d in indeg.items() if d > 0)
        errors.append(f"dependency cycle among briefs: {', '.join(stuck)}")

    # 5. scope path bounds + 6. acceptance/test_plan
    repo_root = os.path.realpath(repo_root)
    for b in briefs:
        bid = b.get("id")
        for glob in b.get("scope", []) or []:
            prefix = literal_prefix(glob)
            target = os.path.realpath(os.path.join(repo_root, prefix)) if prefix else repo_root
            if os.path.commonpath([repo_root, target]) != repo_root:
                errors.append(f"brief {bid!r} scope {glob!r} escapes the repo root")
                continue
            parent = target if os.path.isdir(target) else os.path.dirname(target)
            if not os.path.isdir(parent):
                errors.append(f"brief {bid!r} scope {glob!r}: parent dir {os.path.relpath(parent, repo_root)!r} does not exist")
        if not (b.get("acceptance") or []):
            errors.append(f"brief {bid!r} has no acceptance criteria")
        if not (b.get("test_plan") or "").strip():
            errors.append(f"brief {bid!r} has no test_plan")

    return errors


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("manifest")
    ap.add_argument("--repo-root", default=".", help="repo root scope paths are resolved against (default: cwd)")
    ap.add_argument("--json", action="store_true", help="emit {ok, errors[]} JSON instead of a report")
    args = ap.parse_args()

    manifest = load_manifest(args.manifest)
    errors = schema_errors(manifest)
    if not any(e.startswith("schema:") for e in errors):
        errors += cross_brief_errors(manifest, args.repo_root)

    ok = len(errors) == 0
    if args.json:
        print(json.dumps({"ok": ok, "errors": errors}, indent=2))
    elif ok:
        n = len(manifest.get("briefs", []))
        print(f"OK — {n} brief(s), unique ids, acyclic deps, scope paths bounded, acceptance + test_plan present.")
    else:
        print(f"FAIL — {len(errors)} error(s):")
        for e in errors:
            print(f"  - {e}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
