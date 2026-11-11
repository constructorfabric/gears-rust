#!/usr/bin/env python3
"""Backfill git tags that release-plz failed to create after a successful publish.

`release-plz release` publishes to crates.io and then creates `<crate>-v<version>`
through the GitHub REST API. If that call is rate limited, the crate is already
published, every later run reports "already published" and skips it, and the tag is
never created. This script tags the current manifest version of every crates.io
crate that is published but untagged, pushing over git transport (not subject to the
REST API rate limit).

Run with `python3 -I scripts/ci/backfill_release_tags.py [--dry-run]`. Standard library only.
Env for pushing: GITHUB_TOKEN, GITHUB_REPOSITORY. Optional: GITHUB_STEP_SUMMARY.
"""

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BOT_NAME = "github-actions[bot]"
BOT_EMAIL = "41898282+github-actions[bot]@users.noreply.github.com"


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs).stdout


def publishable_packages():
    meta = json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"]))
    # `publish` is null (any registry) or a list of allowed registries; [] means never.
    return [p for p in meta["packages"] if p.get("publish") != []]


def remote_tags():
    tags = set()
    for line in run(["git", "ls-remote", "--tags", "origin"]).splitlines():
        parts = line.split("\t")
        if len(parts) == 2 and parts[1].startswith("refs/tags/"):
            tags.add(parts[1][len("refs/tags/"):].removesuffix("^{}"))
    return tags


def index_path(name):
    n = name.lower()
    if len(n) <= 3:
        return f"{len(n)}/{n}" if len(n) < 3 else f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def on_crates_io(name, version):
    url = f"https://index.crates.io/{index_path(name)}"
    try:
        with urllib.request.urlopen(url, timeout=30) as resp:
            body = resp.read().decode()
    except urllib.error.HTTPError as e:
        if e.code in (404, 410):
            return False
        raise
    return any(json.loads(line).get("vers") == version for line in body.splitlines() if line.strip())


def introducing_commit(version, manifest):
    out = run(["git", "log", "HEAD", "--format=%H", f'-Sversion = "{version}"', "--", manifest])
    shas = out.split()
    return shas[-1] if shas else None


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dry-run", action="store_true", help="print what would be tagged; create and push nothing")
    ap.add_argument("--assume-missing", action="append", default=[], metavar="TAG",
                    help=argparse.SUPPRESS)  # testing: pretend TAG is absent from origin
    args = ap.parse_args()

    existing = remote_tags() - set(args.assume_missing)
    plan = []
    for pkg in sorted(publishable_packages(), key=lambda p: p["name"]):
        name, version = pkg["name"], pkg["version"]
        tag = f"{name}-v{version}"
        if tag in existing:
            continue
        if not on_crates_io(name, version):
            continue  # not published yet: release-plz will publish and tag it
        sha = introducing_commit(version, os.path.relpath(pkg["manifest_path"]))
        if not sha:
            print(f'::warning::{tag}: no commit on HEAD history introduces version = "{version}"; not tagging')
            continue
        plan.append((name, version, tag, sha))

    if not plan:
        print("No missing release tags.")
        return 0

    env = dict(os.environ, GIT_COMMITTER_NAME=BOT_NAME, GIT_COMMITTER_EMAIL=BOT_EMAIL)
    token = os.environ.get("GITHUB_TOKEN", "")
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    url = f"https://x-access-token:{token}@github.com/{repo}.git"
    failed, done = [], []
    for name, version, tag, sha in plan:
        if args.dry_run:
            print(f"[dry-run] would tag {tag} at {sha}")
            continue
        try:
            run(["git", "tag", "-a", tag, sha, "-m", f"{name} v{version}"], env=env)
            run(["git", "push", url, f"refs/tags/{tag}"])
        except subprocess.CalledProcessError as e:
            # A concurrent run (no concurrency group on this workflow) may have pushed it first.
            try:
                if tag in remote_tags():
                    print(f"{tag} already exists on origin (concurrent run); nothing to do")
                    continue
            except subprocess.CalledProcessError:
                pass
            msg = (e.stderr or "").replace(token, "***") if token else (e.stderr or "")
            print(f"::error::failed to backfill {tag} at {sha}: {msg.strip()}")
            failed.append(tag)
            continue
        print(f"::warning::backfilled missing tag {tag} at {sha}: release-plz tag creation failed "
              "(likely GitHub API rate limit) after the crate was published")
        done.append(tag)

    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and not args.dry_run:
        with open(summary, "a") as f:
            f.write(f"Backfilled {len(done)} missing release tag(s): {', '.join(done) or 'none'}"
                    + (f"; FAILED: {', '.join(failed)}" if failed else "") + "\n")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
