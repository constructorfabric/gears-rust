#!/usr/bin/env python3
"""Dependabot sweeper: merge safe + green Dependabot PRs, heal or escalate red ones.

The "Protect main" ruleset has no required status checks, so this script's own
check evaluation is the merge gate and is deliberately strict. Python 3 stdlib
only; talks to GitHub through the `gh` CLI. Dry-run by default: every mutating
call goes through write(), which refuses unless --live was given.
"""
import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

LABEL_MIGRATION = "deps:needs-migration"
LABEL_RECREATED = "deps:recreated"
LABELS = {
    LABEL_MIGRATION: ("B60205", "Dependabot update needs manual migration"),
    LABEL_RECREATED: ("FBCA04", "Dependabot was asked to recreate this PR"),
}
MAX_MERGES = 3
MAX_RECREATES = 3
STALE_HOURS = 72

SAFE_TYPES = {"version-update:semver-patch", "version-update:semver-minor"}
MINOR = "version-update:semver-minor"
WAIT_STATES = {"QUEUED", "IN_PROGRESS", "PENDING", "WAITING", "EXPECTED", "REQUESTED"}
RED_STATES = {"FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE", "ERROR"}
GREEN_STATES = {"SUCCESS", "SKIPPED", "NEUTRAL"}


# ---------------------------------------------------------------- pure logic

def parse_metadata(message):
    """Return [(dependency-name, update-type or None)] from the updated-dependencies block."""
    _, sep, block = message.partition("updated-dependencies:")
    if not sep:
        return []
    block = block.split("\n...", 1)[0]
    entries = []
    for chunk in re.split(r"^\s*-\s+(?=dependency-name:)", block, flags=re.M)[1:]:
        name = re.search(r"dependency-name:\s*(\S+)", chunk)
        utype = re.search(r"update-type:\s*(\S+)", chunk)
        if name:
            entries.append((name.group(1).strip("'\""), utype.group(1) if utype else None))
    return entries


def ecosystem_from_branch(branch):
    m = re.match(r"dependabot/([^/]+)/", branch)
    return m.group(1) if m else None


def parse_from_versions(title, body):
    """Map dependency name -> from-version; key None holds the title's single 'from A to B'."""
    out = {}
    for m in re.finditer(r"^Updates `([^`]+)` from (\S+) to (\S+)", body or "", flags=re.M):
        out[m.group(1)] = m.group(2)
    t = re.search(r"\bfrom (\S+) to (\S+)", title or "")
    if t:
        out[None] = t.group(1)
    return out


def _major(version):
    m = re.match(r"v?(\d+)", version or "")
    return int(m.group(1)) if m else None


def eligibility(ecosystem, entries, from_versions):
    """Return (eligible, reason)."""
    if ecosystem not in ("cargo", "docker", "github_actions"):
        return False, f"unknown ecosystem {ecosystem}"
    if not entries:
        return False, "no updated-dependencies metadata"
    for name, utype in entries:
        if utype is None:
            if ecosystem == "cargo":
                return False, f"{name}: no update-type (cargo)"
            continue
        if utype not in SAFE_TYPES:
            return False, f"{name}: {utype.replace('version-update:semver-', '')}"
        if ecosystem == "cargo" and utype == MINOR:
            frm = from_versions.get(name)
            if frm is None and len(entries) == 1:
                frm = from_versions.get(None)
            major = _major(frm)
            if major is None:
                return False, f"{name}: minor, from-version unknown"
            if major == 0:
                return False, f"{name}: 0.x minor is breaking in cargo"
    return True, "patch/minor" if any(u for _, u in entries) else "digest/sha bump"


def check_token(c):
    if c.get("__typename") == "StatusContext" or "state" in c and "status" not in c:
        return (c.get("state") or "").upper()
    status = (c.get("status") or "").upper()
    if status != "COMPLETED":
        return status or "PENDING"
    return (c.get("conclusion") or "").upper()


def classify_checks(rollup):
    """'wait' | 'red' | 'green'. Anything unrecognised is treated as wait (strict)."""
    if not rollup:
        return "wait"
    tokens = [check_token(c) for c in rollup]
    if any(t in WAIT_STATES or t not in WAIT_STATES | RED_STATES | GREEN_STATES for t in tokens):
        return "wait"
    if any(t in RED_STATES for t in tokens):
        return "red"
    return "green"


def parse_time(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def age_hours(committed_date, now):
    return (now - parse_time(committed_date)).total_seconds() / 3600


class Budget:
    def __init__(self):
        self.merges = 0
        self.recreates = 0


def decide(*, human_commits, eligible, checks, mergeable, labels, age_h, budget):
    """Return an action string; consumes budget for merge/recreate."""
    if human_commits:
        return "skip: human commits"
    if not eligible:
        return "noop (labelled)" if LABEL_MIGRATION in labels else "label needs-migration"
    if checks == "wait":
        return "wait"
    if checks == "red":
        if age_h < STALE_HOURS:
            return "wait (red <72h)"
        if LABEL_RECREATED in labels:
            return "noop (labelled)" if LABEL_MIGRATION in labels else "label needs-migration"
        if budget.recreates >= MAX_RECREATES:
            return "defer: recreate cap"
        budget.recreates += 1
        return "recreate"
    if mergeable == "CONFLICTING":
        return "skip: conflicting"
    if budget.merges >= MAX_MERGES:
        return "defer: merge cap"
    budget.merges += 1
    return "merge"


def has_head_approval(reviews, head_sha):
    return any(
        r.get("state") == "APPROVED" and (r.get("commit") or {}).get("oid") == head_sha
        for r in reviews or []
    )


def is_dependabot(login):
    return (login or "").removesuffix("[bot]") == "dependabot"


BASE_SYNC = re.compile(
    r"^Merge (branch '(main|master)'|remote-tracking branch '[^']+/(main|master)') into "
)


def is_foreign(commit):
    return any(not is_dependabot(a.get("login")) for a in commit.get("authors", []))


def is_human_edit(commit):
    """A non-Dependabot commit, except a plain sync from the base branch."""
    return is_foreign(commit) and not BASE_SYNC.match(commit.get("messageHeadline", ""))


# ----------------------------------------------------------------------- I/O

class Gh:
    def __init__(self, repo, live):
        self.repo = repo
        self.live = live

    def read_json(self, *args):
        res = subprocess.run(["gh", *args], capture_output=True, text=True, check=True)
        return json.loads(res.stdout or "null")

    def write(self, *args):
        """The only mutating entry point; refuses outside --live."""
        if not self.live:
            raise RuntimeError("write() called in dry-run mode")
        subprocess.run(["gh", *args], check=True)


def ensure_labels(gh):
    have = {x["name"] for x in gh.read_json("label", "list", "--repo", gh.repo, "--json", "name", "--limit", "200")}
    for name, (color, desc) in LABELS.items():
        if name not in have and gh.live:
            gh.write("label", "create", name, "--repo", gh.repo, "--color", color, "--description", desc)


def add_label(gh, n, label):
    gh.write("pr", "edit", str(n), "--repo", gh.repo, "--add-label", label)


def process(gh, pr, budget, now):
    n = pr["number"]
    view = gh.read_json(
        "pr", "view", str(n), "--repo", gh.repo, "--json",
        "statusCheckRollup,mergeable,reviews,commits,labels,headRefOid",
    )
    commits = view["commits"]
    human = any(is_human_edit(c) for c in commits)
    # Metadata lives in Dependabot's own commit; a later "Merge branch 'main'"
    # commit from a maintainer (#5220) carries none.
    bot_commits = [c for c in commits if not is_foreign(c)]
    head = bot_commits[-1] if bot_commits else commits[-1]
    message = head["messageHeadline"] + "\n\n" + (head.get("messageBody") or "")
    eco = ecosystem_from_branch(pr["headRefName"])
    ok, reason = eligibility(
        eco, parse_metadata(message), parse_from_versions(pr["title"], pr.get("body"))
    )
    checks = classify_checks(view.get("statusCheckRollup"))
    labels = {x["name"] for x in view.get("labels", [])}
    action = decide(
        human_commits=human, eligible=ok, checks=checks, mergeable=view.get("mergeable"),
        labels=labels, age_h=age_hours(head["committedDate"], now), budget=budget,
    )
    done = action
    if gh.live:
        try:
            if action == "label needs-migration":
                add_label(gh, n, LABEL_MIGRATION)
            elif action == "recreate":
                gh.write("pr", "comment", str(n), "--repo", gh.repo, "--body", "@dependabot recreate")
                add_label(gh, n, LABEL_RECREATED)
            elif action == "merge":
                sha = view["headRefOid"]
                if not has_head_approval(view.get("reviews"), sha):
                    gh.write("pr", "review", str(n), "--repo", gh.repo, "--approve")
                gh.write("pr", "merge", str(n), "--repo", gh.repo, "--squash", "--match-head-commit", sha)
        except subprocess.CalledProcessError as e:
            done = f"{action} FAILED (exit {e.returncode})"
    elif action in ("label needs-migration", "recreate", "merge"):
        done = f"{action} (dry-run)"
    return [f"#{n}", pr["title"].replace("|", "\\|"), eco or "?",
            ("yes: " if ok else "no: ") + reason, checks, done]


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    ap.add_argument("--live", action="store_true", help="perform writes (default: dry-run)")
    args = ap.parse_args()
    if not args.repo:
        ap.error("--repo or $GITHUB_REPOSITORY required")
    gh = Gh(args.repo, args.live)
    ensure_labels(gh)
    prs = gh.read_json(
        "pr", "list", "--repo", args.repo, "--state", "open", "--author", "app/dependabot",
        "--json", "number,title,headRefName,body", "--limit", "100",
    )
    budget, now, rows = Budget(), datetime.now(timezone.utc), []
    for pr in sorted(prs, key=lambda p: p["number"]):
        rows.append(process(gh, pr, budget, now))
    out = [f"### Dependabot sweeper ({'live' if args.live else 'dry-run'})", "",
           "| PR | Title | Ecosystem | Eligibility | Checks | Action |", "|---|---|---|---|---|---|"]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    text = "\n".join(out) + "\n"
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(text)


if __name__ == "__main__":
    sys.exit(main())
