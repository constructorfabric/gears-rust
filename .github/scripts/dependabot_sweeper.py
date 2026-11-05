#!/usr/bin/env python3
"""Dependabot sweeper: merge safe + green Dependabot PRs, heal or escalate red ones.

The "Protect main" ruleset has no required status checks, so this script's own
check evaluation is the merge gate and is deliberately strict. Python 3 stdlib
only; talks to GitHub through the `gh` CLI. Dry-run by default: every mutating
call goes through write(), which refuses unless --live was given.

A PR is only ever approved or merged when every one of its commits is authored
by Dependabot, signed by GitHub (web-flow) with a valid signature, and has a
single parent. Any other commit (human edit, merge commit, unsigned) is skipped.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone

LABEL_MIGRATION = "deps:needs-migration"
LABEL_RECREATED = "deps:recreated"
LABEL_HOLD = "deps:hold"
LABELS = {
    LABEL_MIGRATION: ("B60205", "Dependabot update needs manual migration"),
    LABEL_RECREATED: ("FBCA04", "Dependabot was asked to recreate this PR"),
    LABEL_HOLD: ("C5DEF5", "Dependabot sweeper: do not touch this PR"),
}
MAX_MERGES = 3
MAX_RECREATES = 3
STALE_HOURS = 72
GH_TIMEOUT = 60  # seconds, per gh call
PR_LIST_LIMIT = 500
RUN_DEADLINE_SECONDS = 600
LIVE_REF = "refs/heads/main"
REPO_RE = r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+"
TITLE_MAX = 100
REQUIRED_CHECKS = ("DCO", "SonarCloud Code Analysis")
MIN_HEAD_AGE_MINUTES = 30
RUN_OK_CONCLUSIONS = {"success", "skipped", "neutral"}
GH_ERRORS = (subprocess.CalledProcessError, subprocess.TimeoutExpired, json.JSONDecodeError)

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


def parse_from_versions(headline, body):
    """Map dependency name -> from-version, read from the signed head commit message.

    The body's "Updates `x` from A to B" lines give one entry per dependency; key None
    holds the single "from A to B" of the headline (or of the body's "Bumps [x](...) from A to B").
    """
    out = {}
    for m in re.finditer(r"^Updates `([^`]+)` from (\S+) to (\S+)", body or "", flags=re.M):
        out[m.group(1)] = m.group(2)
    t = re.search(r"\bfrom (\S+) to (\S+)", headline or "")
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


def _check_name(c):
    return c.get("context") if c.get("__typename") == "StatusContext" else c.get("name")


def ci_verdict(rollup, workflow_runs, total_runs, head_age_min):
    """'wait' | 'red' | 'green': the strict all-green gate for one head SHA.

    Needs the rollup green, every workflow run for the SHA completed and not failed
    (from a complete list), the required external checks present and green, and a
    head commit old enough for late-registering checks to have appeared.
    Wait beats red, as in classify_checks; when in doubt, never green.
    """
    verdicts = [classify_checks(rollup)]
    for name in REQUIRED_CHECKS:
        found = [c for c in rollup or [] if _check_name(c) == name]
        verdicts.append(classify_checks(found))
    if not workflow_runs or total_runs > len(workflow_runs):
        verdicts.append("wait")
    elif any(r.get("status") != "completed" for r in workflow_runs):
        verdicts.append("wait")
    elif any(r.get("conclusion") not in RUN_OK_CONCLUSIONS for r in workflow_runs):
        verdicts.append("red")
    if head_age_min < MIN_HEAD_AGE_MINUTES:
        verdicts.append("wait")
    if "wait" in verdicts:
        return "wait"
    return "red" if "red" in verdicts else "green"


def md_title(title):
    """Make a PR title safe for one markdown table cell (no markup, no mentions)."""
    title = " ".join((title or "").split())
    if len(title) > TITLE_MAX:
        title = title[:TITLE_MAX - 1] + "…"
    title = re.sub(r"([\\|\[\]<>*_`])", r"\\\1", title)
    return title.replace("@", "@\u200b")


def parse_time(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def age_hours(committed_date, now):
    return (now - parse_time(committed_date)).total_seconds() / 3600


class Budget:
    def __init__(self):
        self.merges = 0
        self.recreates = 0


def decide(*, untrusted_commits, eligible, checks, mergeable, labels, age_h, budget,
           draft=False, review_decision=None, unresolved_threads=False):
    """Return an action string; consumes budget for merge/recreate."""
    if untrusted_commits:
        return "skip: non-Dependabot commit"
    if LABEL_HOLD in labels:
        return "skip: hold"
    if draft:
        return "skip: draft"
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
    if LABEL_MIGRATION in labels:
        return "skip: needs-migration label"
    if review_decision == "CHANGES_REQUESTED":
        return "skip: changes requested"
    if unresolved_threads:
        return "skip: unresolved threads"
    if mergeable == "CONFLICTING":
        return "skip: conflicting"
    if mergeable != "MERGEABLE":
        return "wait: mergeability unknown"
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


def is_trusted_commit(c):
    """Fail-closed: Dependabot-authored, single parent, GitHub-signed and valid.

    A maintainer who syncs main into a Dependabot PR now handles that PR by
    hand; `@dependabot rebase` keeps it bot-only.
    """
    nodes = (c.get("authors") or {}).get("nodes") or []
    if not nodes:
        return False
    for node in nodes:
        user = (node or {}).get("user")
        if not user or not is_dependabot(user.get("login")):
            return False
    if (c.get("parents") or {}).get("totalCount") != 1:
        return False
    sig = c.get("signature")
    return bool(sig and sig.get("isValid") is True and sig.get("wasSignedByGitHub") is True)


# ----------------------------------------------------------------------- I/O

def describe(e):
    if isinstance(e, subprocess.TimeoutExpired):
        return "timeout"
    if isinstance(e, subprocess.CalledProcessError):
        return f"exit {e.returncode}"
    return "bad JSON"


COMMITS_QUERY = (
    "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name)"
    "{pullRequest(number:$number){commits(first:100){totalCount nodes{commit{oid messageHeadline "
    "messageBody committedDate parents{totalCount} authors(first:10){nodes{user{login}}} "
    "signature{isValid wasSignedByGitHub}}}}}}}"
)

THREADS_QUERY = (
    "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name)"
    "{pullRequest(number:$number){reviewThreads(first:100){totalCount nodes{isResolved}}}}}"
)


class Gh:
    def __init__(self, repo, live):
        self.repo = repo
        self.live = live

    def read_json(self, *args):
        try:
            res = subprocess.run(
                ["gh", *args], capture_output=True, text=True, check=True, timeout=GH_TIMEOUT
            )
        except subprocess.CalledProcessError as e:
            print(e.stderr, file=sys.stderr)
            raise
        return json.loads(res.stdout or "null")

    def write(self, *args, capture=False):
        """The only mutating entry point; refuses outside --live.

        With capture=True the JSON written to stdout is parsed and returned.
        """
        if not self.live:
            raise RuntimeError("write() called in dry-run mode")
        if not capture:
            subprocess.run(["gh", *args], check=True, timeout=GH_TIMEOUT)
            return None
        res = subprocess.run(
            ["gh", *args], check=True, timeout=GH_TIMEOUT, capture_output=True, text=True
        )
        return json.loads(res.stdout or "null")


def ensure_labels(gh):
    """Return False if labels could not be listed or created."""
    try:
        have = {x["name"] for x in gh.read_json(
            "label", "list", "--repo", gh.repo, "--json", "name", "--limit", "200")}
    except GH_ERRORS as e:
        print(f"warning: listing labels failed ({describe(e)})", file=sys.stderr)
        return False
    ok = True
    for name, (color, desc) in LABELS.items():
        if name not in have and gh.live:
            try:
                gh.write("label", "create", name, "--repo", gh.repo, "--color", color,
                         "--description", desc)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e:
                print(f"warning: creating label {name} failed ({describe(e)})", file=sys.stderr)
                ok = False
    return ok


def fetch_commits(gh, n):
    """Return (total_count, [commit nodes]) for the first 100 commits of PR n."""
    owner, name = gh.repo.split("/", 1)
    data = gh.read_json(
        "api", "graphql", "-f", f"query={COMMITS_QUERY}",
        "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"number={n}",
    )
    conn = data["data"]["repository"]["pullRequest"]["commits"]
    return conn["totalCount"], [x["commit"] for x in conn["nodes"]]


def fetch_unresolved_threads(gh, n):
    """True if PR n has an unresolved review thread (or more than 100 threads)."""
    owner, name = gh.repo.split("/", 1)
    data = gh.read_json(
        "api", "graphql", "-f", f"query={THREADS_QUERY}",
        "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"number={n}",
    )
    conn = data["data"]["repository"]["pullRequest"]["reviewThreads"]
    return conn["totalCount"] > 100 or any(not x["isResolved"] for x in conn["nodes"])


def fetch_runs(gh, sha):
    """Return (total_count, [workflow runs]) for the head SHA (first 100 runs)."""
    data = gh.read_json("api", f"repos/{gh.repo}/actions/runs?head_sha={sha}&per_page=100")
    return data["total_count"], data["workflow_runs"]


def add_label(gh, n, label):
    gh.write("api", "-X", "POST", f"repos/{gh.repo}/issues/{n}/labels", "-f", f"labels[]={label}")


def process(gh, pr, budget, now):
    n = pr["number"]
    eco = ecosystem_from_branch(pr["headRefName"])
    title = md_title(pr["title"])
    try:
        view = gh.read_json(
            "pr", "view", str(n), "--repo", gh.repo, "--json",
            "statusCheckRollup,mergeable,reviews,labels,headRefOid,isDraft,reviewDecision",
        )
        total, commits = fetch_commits(gh, n)
        unresolved = fetch_unresolved_threads(gh, n)
        total_runs, runs = fetch_runs(gh, view.get("headRefOid"))
    except GH_ERRORS as e:
        return [f"#{n}", title, eco or "?", "?", "?", f"read FAILED ({describe(e)})"]
    head = commits[-1] if commits else {}
    # The commit list and the view are two reads; a push in between must not
    # let a commit we never checked become the approved/merged head.
    untrusted = (
        total > len(commits) or not commits or head.get("oid") != view.get("headRefOid")
        or not all(is_trusted_commit(c) for c in commits)
    )
    message = head.get("messageHeadline", "") + "\n\n" + (head.get("messageBody") or "")
    ok, reason = eligibility(
        eco, parse_metadata(message), parse_from_versions(head.get("messageHeadline", ""), head.get("messageBody"))
    )
    age_h = age_hours(head["committedDate"], now) if commits else 0
    checks = ci_verdict(view.get("statusCheckRollup"), runs, total_runs, age_h * 60)
    labels = {x["name"] for x in view.get("labels", [])}
    action = decide(
        untrusted_commits=untrusted, eligible=ok, checks=checks, mergeable=view.get("mergeable"),
        labels=labels, age_h=age_h, budget=budget, draft=bool(view.get("isDraft")),
        review_decision=view.get("reviewDecision"), unresolved_threads=unresolved,
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
                review_id = None
                if not has_head_approval(view.get("reviews"), sha):
                    # The approval is tied to the checked SHA; `--match-head-commit` is the
                    # actual merge gate, and on failure the approval is withdrawn.
                    review = gh.write("api", f"repos/{gh.repo}/pulls/{n}/reviews", "-f",
                                      "event=APPROVE", "-f", f"commit_id={sha}", capture=True)
                    review_id = (review or {}).get("id")
                try:
                    gh.write("pr", "merge", str(n), "--repo", gh.repo, "--squash",
                             "--match-head-commit", sha)
                except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e:
                    done = f"merge FAILED ({describe(e)})"
                    if review_id is not None:
                        try:
                            gh.write("api", "-X", "PUT",
                                     f"repos/{gh.repo}/pulls/{n}/reviews/{review_id}/dismissals",
                                     "-f", "message=Dependabot sweeper: merge failed, "
                                     "withdrawing automatic approval", "-f", "event=DISMISS")
                            done += "; approval dismissed"
                        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e2:
                            done += f"; approval dismiss FAILED ({describe(e2)})"
        except GH_ERRORS as e:
            done = f"{action} FAILED ({describe(e)})"
    elif action in ("label needs-migration", "recreate", "merge"):
        done = f"{action} (dry-run)"
    return [f"#{n}", title, eco or "?",
            ("yes: " if ok else "no: ") + reason, checks, done]


def write_summary(text):
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(text)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    ap.add_argument("--live", action="store_true", help="perform writes (default: dry-run)")
    args = ap.parse_args()
    if not args.repo:
        ap.error("--repo or $GITHUB_REPOSITORY required")
    if not re.fullmatch(REPO_RE, args.repo):
        print(f"error: invalid --repo {args.repo!r}", file=sys.stderr)
        return 2
    if args.live and os.environ.get("GITHUB_REF") != LIVE_REF:
        print(f"error: --live only runs on {LIVE_REF}", file=sys.stderr)
        return 2
    gh = Gh(args.repo, args.live)
    labels_ok = ensure_labels(gh)
    try:
        prs = gh.read_json(
            "pr", "list", "--repo", args.repo, "--state", "open", "--author", "app/dependabot",
            "--json", "number,title,headRefName", "--limit", str(PR_LIST_LIMIT),
        )
    except GH_ERRORS as e:
        write_summary(f"### Dependabot sweeper: listing PRs FAILED ({describe(e)})\n")
        return 1
    budget, now, rows = Budget(), datetime.now(timezone.utc), []
    start = time.monotonic()
    ordered = sorted(prs, key=lambda p: p["number"])
    deferred = 0
    for i, pr in enumerate(ordered):
        if time.monotonic() - start > RUN_DEADLINE_SECONDS:
            deferred = len(ordered) - i
            break
        rows.append(process(gh, pr, budget, now))
    out = [f"### Dependabot sweeper ({'live' if args.live else 'dry-run'})", "",
           "| PR | Title | Ecosystem | Eligibility | Checks | Action |", "|---|---|---|---|---|---|"]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    if deferred:
        out += ["", f"deferred: run deadline reached, {deferred} PRs not processed"]
    truncated = len(prs) >= PR_LIST_LIMIT
    if truncated:
        out += ["", f"**Warning:** PR list hit the limit of {PR_LIST_LIMIT}; "
                    "some PRs were not processed."]
    write_summary("\n".join(out) + "\n")
    failed = any("FAILED" in r[-1] for r in rows)
    return 1 if failed or truncated or not labels_ok else 0


if __name__ == "__main__":
    sys.exit(main())
