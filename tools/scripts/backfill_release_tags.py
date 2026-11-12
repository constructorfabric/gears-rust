#!/usr/bin/env python3
"""Backfill git tags that release-plz failed to create after a successful publish.

`release-plz release` publishes to crates.io and then creates `<crate>-v<version>`
through the GitHub REST API. If that call is rate limited, the crate is already
published, every later run reports "already published" and skips it, and the tag is
never created. This script tags the current manifest version of every crates.io
crate that is published but untagged, pushing over git transport (not subject to the
REST API rate limit).

Run with `python3 -I tools/scripts/backfill_release_tags.py [--dry-run]`. Python >= 3.11,
standard library only. Env for pushing: GITHUB_TOKEN, GITHUB_REPOSITORY. Optional:
GITHUB_STEP_SUMMARY.
"""

import argparse
import base64
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

BOT_NAME = "github-actions[bot]"
BOT_EMAIL = "41898282+github-actions[bot]@users.noreply.github.com"
MIN_AGE = timedelta(minutes=15)
USER_AGENT = "gears-rust-backfill-release-tags (https://github.com/constructorfabric/gears-rust)"
NETWORK_ERRORS = (OSError, json.JSONDecodeError, subprocess.CalledProcessError)


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
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = resp.read().decode()
    except urllib.error.HTTPError as e:
        if e.code in (404, 410):
            return False
        raise
    return any(json.loads(line).get("vers") == version for line in body.splitlines() if line.strip())


def created_at(name, version):
    """Publication time of `name` `version`, from the crates.io API."""
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}/{version}", headers={"User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        raw = json.load(resp)["version"]["created_at"]
    ts = datetime.fromisoformat(raw)
    return ts if ts.tzinfo else ts.replace(tzinfo=timezone.utc)


def package_version_fallback(text):
    """`[package].version` by scanning the `[package]` table, for manifests tomllib rejects.

    Cargo accepts TOML 1.1 syntax (e.g. multi-line inline tables) that Python's TOML 1.0
    parser does not.
    """
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped.split("#")[0].strip() == "[package]"
        elif in_package:
            m = re.match(r'version\s*=\s*"([^"]+)"', stripped)
            if m:
                return m.group(1)
    return None


def manifest_version(sha, manifest):
    """`[package].version` of `manifest` at `sha`, or None if absent, inherited or unreadable."""
    try:
        text = run(["git", "show", f"{sha}:{manifest}"])
    except subprocess.CalledProcessError:
        return None
    try:
        version = tomllib.loads(text).get("package", {}).get("version")
    except tomllib.TOMLDecodeError:
        version = package_version_fallback(text)
    return version if isinstance(version, str) else None  # `version.workspace = true` is a dict


def introducing_commit(version, manifest):
    """Commit that introduced `version` in `manifest`, or None.

    Looks only at the parsed `[package].version`, so dependency lines that carry the
    same version string cannot match. If the version was set, changed away and set
    again, the answer is the oldest commit of the contiguous run of `version` that
    reaches HEAD (the one the current release belongs to), not an earlier run.
    """
    shas = run(["git", "log", "--format=%H", "HEAD", "--", manifest]).split()
    found = None
    for sha in shas:  # newest first
        if manifest_version(sha, manifest) != version:
            break
        found = sha
    return found


def tag_message(name, version):
    # Same text release-plz uses for its annotated tags.
    return f"chore: Release package {name} version {version}"


def build_plan(packages, existing, *, on_crates_io=on_crates_io, introducing_commit=introducing_commit,
               created_at=created_at, now=None, min_age=MIN_AGE):
    now = now or datetime.now(timezone.utc)
    plan = []
    for pkg in sorted(packages, key=lambda p: p["name"]):
        name, version = pkg["name"], pkg["version"]
        tag = f"{name}-v{version}"
        if tag in existing:
            continue
        if not on_crates_io(name, version):
            continue  # not published yet: release-plz will publish and tag it
        try:
            published = created_at(name, version)
        except (*NETWORK_ERRORS, KeyError, ValueError) as e:
            print(f"::warning::{tag}: cannot determine publication time ({e}); skipping, next run retries")
            continue
        if now - published < min_age:
            # A release run that is still tagging this version would race with us.
            print(f"::notice::{tag} was published less than {min_age} ago; leaving it to the next run")
            continue
        sha = introducing_commit(version, os.path.relpath(pkg["manifest_path"]))
        if not sha:
            print(f'::warning::{tag}: no commit on HEAD history introduces version "{version}"; not tagging')
            continue
        plan.append((name, version, tag, sha))
    return plan


def push_env(token, base=None):
    """Environment that authenticates git over HTTPS without putting the token in argv."""
    b64 = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    print(f"::add-mask::{b64}")  # Actions masks the raw token, not its base64 form
    return dict(
        base if base is not None else os.environ,
        GIT_CONFIG_COUNT="1",
        GIT_CONFIG_KEY_0="http.https://github.com/.extraheader",
        GIT_CONFIG_VALUE_0=f"AUTHORIZATION: basic {b64}",
    )


def apply_plan(plan, dry_run, *, run=run, remote_tags=remote_tags, token="", repo=""):
    """Create and push the planned tags. Returns (done, failed) tag lists."""
    done, failed = [], []
    if dry_run:
        for _, _, tag, sha in plan:
            print(f"[dry-run] would tag {tag} at {sha}")
        return done, failed
    tag_env = dict(os.environ, GIT_COMMITTER_NAME=BOT_NAME, GIT_COMMITTER_EMAIL=BOT_EMAIL)
    url = f"https://github.com/{repo}.git"
    env = push_env(token)
    for name, version, tag, sha in plan:
        try:
            run(["git", "tag", "-a", tag, sha, "-m", tag_message(name, version)], env=tag_env)
            run(["git", "push", url, f"refs/tags/{tag}"], env=env)
        except subprocess.CalledProcessError as e:
            # A concurrent run (no concurrency group on this workflow) may have pushed it first.
            try:
                if tag in remote_tags():
                    print(f"{tag} already exists on origin (concurrent run); nothing to do")
                    continue
            except subprocess.CalledProcessError:
                pass
            print(f"::error::failed to backfill {tag} at {sha}: {(e.stderr or '').strip()}")
            failed.append(tag)
            continue
        print(f"::warning::backfilled missing tag {tag} at {sha}: release-plz tag creation failed "
              "(likely GitHub API rate limit) after the crate was published")
        done.append(tag)
    return done, failed


def write_summary(done, failed):
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as f:
            f.write(f"Backfilled {len(done)} missing release tag(s): {', '.join(done) or 'none'}"
                    + (f"; FAILED: {', '.join(failed)}" if failed else "") + "\n")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dry-run", action="store_true", help="print what would be tagged; create and push nothing")
    args = ap.parse_args(argv)

    try:
        plan = build_plan(publishable_packages(), remote_tags())
    except (*NETWORK_ERRORS, KeyError) as e:
        detail = (getattr(e, "stderr", None) or str(e)).strip()
        print(f"::error::cannot determine missing release tags: {detail}")
        return 1
    if not plan:
        print("No missing release tags.")
        return 0

    done, failed = apply_plan(plan, args.dry_run, token=os.environ.get("GITHUB_TOKEN", ""),
                              repo=os.environ.get("GITHUB_REPOSITORY", ""))
    if not args.dry_run:
        write_summary(done, failed)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
