#!/usr/bin/env python3
"""Helpers for the retried `release-plz release` in .github/workflows/release-plz.yml.

Subcommands:
  snapshot  Fetch tags from origin and record them in $TAGS_BEFORE: the baseline for
            telling which tags this job created.
  wait      Sleep before a retry: at least --min-delay seconds, longer if the GitHub API
            budget of $API_TOKEN is nearly spent (until it resets, at most --cap
            seconds). Then fetch tags, because release-plz checks for an existing tag
            with a local `git tag -l`.
  collect   Build the job's `releases` / `releases_created` outputs, and tag any
            published crate release-plz left untagged.

Why `collect` exists. A failed release-plz attempt sets no `releases` output, and a
retry leaves out every package whose tag already exists, so the packages a failed
attempt released are recovered from the tags created during the job. Only tags that
name a releasable workspace package at its manifest version count; a concurrent run's
tag for another version, or a hand-pushed old tag, does not.

release-plz publishes a crate, waits for the index, then creates the tag. If it fails
in between (index wait timeout, GitHub API error, expired token), every later run sees
the crate on crates.io and skips it: no tag, no GitHub Release, not in `releases`.
After a successful attempt `collect` finds such crates (publishable, version on
crates.io, no tag), tags each at the commit recorded in its published `.crate`
(`.cargo_vcs_info.json`) and adds them to `releases` so their image and chart are
published. A crate whose commit cannot be determined, and a git-only package left
without a tag after a successful attempt, are reported as errors.

Environment for `collect`: OUTCOME_1..3 and RELEASES_1..3 (step outcomes and
`releases` outputs of the attempts), TAGS_BEFORE, GH_TOKEN, GITHUB_REPOSITORY,
GITHUB_OUTPUT.

Exit codes: 0 on success, 1 on an error that should fail the job.
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RELEASE_CONFIG = REPO_ROOT / "release-plz.toml"
GITHUB_API = "https://api.github.com"
CRATES_INDEX = "https://index.crates.io"
CRATES_DOWNLOAD = "https://static.crates.io/crates"
USER_AGENT = "gears-rust-release-workflow"
TAG_REFSPEC = "+refs/tags/*:refs/tags/*"


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


# --- pure logic (unit-tested) -------------------------------------------------------


def releasable_packages(metadata: dict, git_only: set[str]) -> dict[str, dict]:
    """Map `<name>-v<version>` to the workspace package release-plz releases under it.

    A package is released to crates.io unless its manifest says `publish = false`
    (cargo metadata reports that as `[]`); a `publish = false` package is still
    tagged when release-plz.toml marks it `git_only`.
    """
    members = set(metadata.get("workspace_members", []))
    packages: dict[str, dict] = {}
    for package in metadata["packages"]:
        if members and package["id"] not in members:
            continue
        name, version = package["name"], package["version"]
        to_crates_io = package.get("publish") != []
        if not to_crates_io and name not in git_only:
            continue
        packages[f"{name}-v{version}"] = {
            "package_name": name,
            "version": version,
            "crates_io": to_crates_io and name not in git_only,
        }
    return packages


def git_only_packages(config: dict) -> set[str]:
    return {p["name"] for p in config.get("package", []) if p.get("git_only")}


def merge_releases(*lists: list[dict]) -> list[dict]:
    """Concatenate `releases` lists, keeping the first entry for each tag."""
    seen: set[str] = set()
    merged: list[dict] = []
    for releases in lists:
        for release in releases:
            if release["tag"] not in seen:
                seen.add(release["tag"])
                merged.append(release)
    return merged


def releases_from_tags(tags: set[str], releasable: dict[str, dict]) -> list[dict]:
    return [
        {"package_name": releasable[t]["package_name"], "version": releasable[t]["version"], "tag": t}
        for t in sorted(tags)
        if t in releasable
    ]


def untagged(releasable: dict[str, dict], tags: set[str]) -> list[str]:
    return sorted(t for t in releasable if t not in tags)


def wait_seconds(
    min_delay: int, cap: int, now: int, rate: dict | None, min_remaining: int
) -> int:
    """Seconds to sleep before a retry.

    `rate` is the `resources.core` object of GET /rate_limit, or None when it could not
    be read. A nearly spent budget waits until `reset` (+30s slack), never beyond `cap`.
    """
    delay = min_delay
    if rate is not None and rate.get("remaining", min_remaining) < min_remaining:
        delay = max(delay, int(rate["reset"]) - now + 30)
    return max(0, min(delay, cap))


def parse_releases(raw: str | None) -> list[dict]:
    return json.loads(raw) if raw and raw.strip() else []


# --- I/O --------------------------------------------------------------------------


def run(*args: str) -> str:
    return subprocess.run(args, check=True, capture_output=True, text=True, cwd=REPO_ROOT).stdout


def fetch_tags(attempts: int = 3) -> None:
    """Fetch every tag (and no branches) from origin, retrying transient failures."""
    for attempt in range(1, attempts + 1):
        try:
            run("git", "fetch", "--quiet", "origin", TAG_REFSPEC)
            return
        except subprocess.CalledProcessError as error:
            log(f"git fetch tags failed (attempt {attempt}/{attempts}): {error.stderr.strip()}")
            if attempt == attempts:
                raise
            time.sleep(15 * attempt)


def local_tags() -> set[str]:
    return set(run("git", "tag", "-l").split())


def http_json(url: str, token: str | None = None, body: dict | None = None) -> dict:
    headers = {"User-Agent": USER_AGENT, "Accept": "application/vnd.github+json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read().decode())


def index_path(name: str) -> str:
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def on_crates_io(name: str, version: str) -> bool:
    request = urllib.request.Request(
        f"{CRATES_INDEX}/{index_path(name)}", headers={"User-Agent": USER_AGENT}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            lines = response.read().decode().splitlines()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise
    return any(json.loads(line).get("vers") == version for line in lines if line)


def vcs_commit(crate: bytes, name: str, version: str) -> tuple[str | None, bool]:
    """(commit, dirty) from the `.cargo_vcs_info.json` that `cargo publish` packs into
    a `.crate`; (None, False) when the crate carries no git commit."""
    with tarfile.open(fileobj=io.BytesIO(crate), mode="r:gz") as archive:
        try:
            member = archive.extractfile(f"{name}-{version}/.cargo_vcs_info.json")
        except KeyError:
            return None, False
        if member is None:
            return None, False
        git = json.load(member).get("git") or {}
    return git.get("sha1"), bool(git.get("dirty"))


def published_commit(name: str, version: str) -> tuple[str | None, bool]:
    """The commit a crates.io release was packaged from."""
    request = urllib.request.Request(
        f"{CRATES_DOWNLOAD}/{name}/{name}-{version}.crate", headers={"User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(request, timeout=120) as response:
        return vcs_commit(response.read(), name, version)


def has_commit(sha: str) -> bool:
    try:
        run("git", "cat-file", "-e", f"{sha}^{{commit}}")
        return True
    except subprocess.CalledProcessError:
        return False


def create_tag(repo: str, token: str, tag: str, sha: str) -> bool:
    """Create a lightweight tag; False if it already exists (a concurrent run made it).

    GitHub also answers 422 to invalid or throttled requests, so a 422 counts as
    "already exists" only when the ref can be read back.
    """
    try:
        http_json(
            f"{GITHUB_API}/repos/{repo}/git/refs",
            token,
            {"ref": f"refs/tags/{tag}", "sha": sha},
        )
        return True
    except urllib.error.HTTPError as error:
        if error.code != 422:
            raise
        try:
            http_json(f"{GITHUB_API}/repos/{repo}/git/ref/tags/{tag}", token)
        except urllib.error.HTTPError as lookup:
            if lookup.code == 404:
                raise error from None
            raise
        return False


def write_outputs(outputs: dict[str, str]) -> None:
    path = os.environ.get("GITHUB_OUTPUT")
    lines = "".join(f"{key}={value}\n" for key, value in outputs.items())
    if path:
        with open(path, "a") as handle:
            handle.write(lines)
    else:
        sys.stdout.write(lines)


def load_releasable() -> dict[str, dict]:
    import tomllib  # Python 3.11+, present on the runner

    metadata = json.loads(run("cargo", "metadata", "--no-deps", "--format-version", "1"))
    config = tomllib.loads(RELEASE_CONFIG.read_text())
    return releasable_packages(metadata, git_only_packages(config))


# --- subcommands --------------------------------------------------------------------


def cmd_snapshot() -> int:
    fetch_tags()
    tags = local_tags()
    if not tags:
        log("::error::no tags after fetch; refusing to use an empty baseline")
        return 1
    Path(os.environ["TAGS_BEFORE"]).write_text("\n".join(sorted(tags)) + "\n")
    log(f"recorded {len(tags)} tags")
    return 0


def cmd_wait(min_delay: int, cap: int, min_remaining: int) -> int:
    rate = None
    token = os.environ.get("API_TOKEN")
    try:
        rate = http_json(f"{GITHUB_API}/rate_limit", token)["resources"]["core"]
        log(f"GitHub API budget: remaining={rate['remaining']} limit={rate['limit']} reset={rate['reset']}")
    except (urllib.error.URLError, KeyError, ValueError, TimeoutError) as error:
        log(f"::warning::could not read the GitHub API budget ({error}); waiting the fixed delay")
    delay = wait_seconds(min_delay, cap, int(time.time()), rate, min_remaining)
    log(f"sleeping {delay}s before the retry")
    time.sleep(delay)
    fetch_tags()
    return 0


def cmd_collect() -> int:
    outcomes = [os.environ.get(f"OUTCOME_{i}", "") for i in (1, 2, 3)]
    attempts = [parse_releases(os.environ.get(f"RELEASES_{i}")) for i in (1, 2, 3)]
    succeeded = next((i for i, outcome in enumerate(outcomes) if outcome == "success"), None)

    releasable = load_releasable()
    fetch_tags()
    tags = local_tags()
    errors: list[str] = []

    if succeeded == 0:
        releases = attempts[0]
    else:
        before_path = Path(os.environ["TAGS_BEFORE"])
        before = set(before_path.read_text().split()) if before_path.exists() else set()
        if not before:
            # Without a baseline every historical tag would look new and be republished.
            log("::error::no tag snapshot; cannot tell which releases this job made")
            errors.append("missing tag snapshot")
            from_tags: list[dict] = []
        else:
            from_tags = releases_from_tags(tags - before, releasable)
        last = attempts[succeeded] if succeeded is not None else []
        releases = merge_releases(last, from_tags)
        log(f"released by the successful attempt: {json.dumps(last)}")
        log(f"recovered from tags created during this job: {json.dumps(from_tags)}")

    if succeeded is not None:
        healed, heal_errors = heal_untagged(releasable, tags)
        releases = merge_releases(releases, healed)
        errors.extend(heal_errors)

    releases_json = json.dumps(releases, separators=(",", ":"))
    log(f"releases: {releases_json}")
    write_outputs(
        {"releases": releases_json, "releases_created": "true" if releases else "false"}
    )
    for error in errors:
        log(f"::error::{error}")
    return 1 if errors else 0


def heal_untagged(
    releasable: dict[str, dict],
    tags: set[str],
    is_on_crates_io: Callable[[str, str], bool] = on_crates_io,
    find_commit: Callable[[str, str], tuple[str | None, bool]] = published_commit,
    commit_known: Callable[[str], bool] = has_commit,
) -> tuple[list[dict], list[str]]:
    """Tag published crates that have no tag, at the commit they were published from.

    The tag must point at that commit, not at this job's: the image and chart are
    built from the tag, and a later run may already have moved on.
    """
    repo = os.environ["GITHUB_REPOSITORY"]
    token = os.environ.get("GH_TOKEN", "")
    healed: list[dict] = []
    errors: list[str] = []
    for tag in untagged(releasable, tags):
        package = releasable[tag]
        name, version = package["package_name"], package["version"]
        if not package["crates_io"]:
            errors.append(f"git-only package {name} {version} has no tag {tag} after a successful release")
            continue
        try:
            if not is_on_crates_io(name, version):
                continue  # not published yet; the next release run publishes and tags it
            sha, dirty = find_commit(name, version)
            if not sha or not commit_known(sha):
                errors.append(
                    f"{name} {version} is on crates.io but has no tag {tag}, and the commit it "
                    f"was published from ({sha or 'not recorded in the crate'}) is not in this "
                    "repository; create the tag by hand"
                )
                continue
            created = create_tag(repo, token, tag, sha)
        except (urllib.error.URLError, OSError, tarfile.TarError, ValueError) as error:
            errors.append(f"{name} {version} is on crates.io but has no tag, and creating {tag} failed: {error}")
            continue
        if created:
            note = " The crate was packaged from a dirty worktree." if dirty else ""
            log(
                f"::warning::{name} {version} was on crates.io without a tag; created {tag} at "
                f"{sha[:12]}, the commit it was published from. It has no GitHub Release.{note}"
            )
            healed.append({"package_name": name, "version": version, "tag": tag})
    return healed, errors


def main(argv: list[str]) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("snapshot")
    wait = sub.add_parser("wait")
    wait.add_argument("--min-delay", type=int, required=True, help="seconds")
    wait.add_argument("--cap", type=int, default=3600, help="longest sleep, seconds")
    wait.add_argument("--min-remaining", type=int, default=500, help="API calls a retry needs")
    sub.add_parser("collect")
    args = parser.parse_args(argv)

    if args.command == "snapshot":
        return cmd_snapshot()
    if args.command == "wait":
        return cmd_wait(args.min_delay, args.cap, args.min_remaining)
    return cmd_collect()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
