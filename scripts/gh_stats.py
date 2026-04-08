#!/usr/bin/env python3
"""Fetch GitHub repo activity stats via `gh` CLI and output a Markdown report.

Usage:
    python scripts/gh_stats.py [--since YYYY-MM-DD] [--until YYYY-MM-DD] \
        [--exclude-labels pr-issue,wontfix] \
        [--tracked-team-file PATH] [--tracked-team-login LOGIN1,LOGIN2,...] \
        [--tracked-team-name NAME] [--min-module-net-loc N] \
        [--skip-review-turnaround] [owner/repo]

Default repo: cyberfabric/cyberfabric-core
Requires: gh CLI installed and authenticated.

Tracked team membership:
    Use --tracked-team-file to point to a plain text file with one GitHub login
    per line (blank lines and lines starting with # are ignored).
    Use --tracked-team-login to provide a comma-separated list of logins.
    If both are provided, they are merged. If neither is provided, the tracked
    team set is empty (LOC breakdown will attribute everything to "Others").
    The tracked-team file is intended for local/private use and does not need
    to be committed to version control.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from statistics import median
from typing import Any

DEFAULT_REPO = "cyberfabric/cyberfabric-core"
DEFAULT_EXCLUDE_LABELS = ["pr-issue"]

# Accounts to filter out of human tables
BOTS = {
    "github-actions[bot]",
    "coderabbitai[bot]",
    "qodo-code-review[bot]",
    "mergify[bot]",
    "codecov-commenter",
    "graphite-app[bot]",
    "dependabot[bot]",
    "renovate[bot]",
    "Copilot",
    "github-code-quality[bot]",
}


# ---------------------------------------------------------------------------
# Tracked team loading
# ---------------------------------------------------------------------------

def load_tracked_team_file(path: str) -> set[str]:
    """Load GitHub logins from a text file (one per line, # comments, blank lines ignored)."""
    logins: set[str] = set()
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            logins.add(line)
    return logins


def parse_tracked_team_logins(csv: str) -> set[str]:
    """Parse comma-separated login string into a set."""
    return {login.strip() for login in csv.split(",") if login.strip()}


def build_tracked_team(args: argparse.Namespace) -> set[str]:
    """Merge tracked team logins from --tracked-team-file and --tracked-team-login."""
    team: set[str] = set()
    if args.tracked_team_file:
        team |= load_tracked_team_file(args.tracked_team_file)
    if args.tracked_team_login:
        team |= parse_tracked_team_logins(args.tracked_team_login)
    return team


# ---------------------------------------------------------------------------
# Date handling — half-open interval: since <= dt < until_exclusive
# ---------------------------------------------------------------------------

def parse_since_date(date_str: str | None) -> datetime | None:
    """Parse --since as inclusive start-of-day in UTC."""
    if not date_str:
        return None
    return datetime.strptime(date_str, "%Y-%m-%d").replace(tzinfo=timezone.utc)


def parse_until_exclusive(date_str: str | None) -> datetime | None:
    """Parse --until as exclusive start-of-next-day in UTC.

    This ensures that --until 2026-03-18 includes all events on 2026-03-18.
    """
    if not date_str:
        return None
    day = datetime.strptime(date_str, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    return day + timedelta(days=1)


# ---------------------------------------------------------------------------
# GitHub API helpers
# ---------------------------------------------------------------------------

def gh_api_paginate(endpoint: str, repo: str) -> list[dict[str, Any]]:
    """Call `gh api` with pagination and return parsed JSON list."""
    cmd = [
        "gh", "api",
        f"repos/{repo}/{endpoint}",
        "--paginate",
        "--jq", ".",
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, check=True)
    raw = result.stdout.strip()
    if not raw:
        return []
    raw = raw.replace("]\n[", ",").replace("][", ",")
    return json.loads(raw)


def parse_iso(ts: str) -> datetime:
    """Parse GitHub ISO-8601 timestamp."""
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def filter_by_date(
    items: list[dict],
    since: datetime | None,
    until_exclusive: datetime | None,
    date_field: str = "created_at",
) -> list[dict]:
    """Filter items using half-open interval: since <= dt < until_exclusive."""
    if not since and not until_exclusive:
        return items
    result = []
    for item in items:
        ts = item.get(date_field)
        if not ts:
            continue
        dt = parse_iso(ts)
        if since and dt < since:
            continue
        if until_exclusive and dt >= until_exclusive:
            continue
        result.append(item)
    return result


def is_bot(login: str) -> bool:
    return login in BOTS or login.endswith("[bot]")


def has_excluded_label(item: dict, exclude_labels: set[str]) -> bool:
    labels = {lbl["name"] for lbl in item.get("labels", [])}
    return bool(labels & exclude_labels)


# ---------------------------------------------------------------------------
# Aggregation & formatting
# ---------------------------------------------------------------------------

def aggregate(items: list[dict], user_key: str = "user", body_key: str = "body"):
    """Return {login: [count, total_bytes]} excluding bots."""
    stats: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    for item in items:
        login = item.get(user_key, {}).get("login", "unknown")
        if is_bot(login):
            continue
        body_len = len(item.get(body_key) or "")
        stats[login][0] += 1
        stats[login][1] += body_len
    return stats


def fmt_kb(b: int) -> str:
    if b < 1024:
        return f"{b} B"
    return f"{b / 1024:.1f} KB"


def fmt_duration(td: timedelta) -> str:
    """Format timedelta as human-readable string."""
    total_seconds = int(td.total_seconds())
    if total_seconds < 0:
        return "N/A"
    days = total_seconds // 86400
    hours = (total_seconds % 86400) // 3600
    if days > 0:
        return f"{days}d {hours}h"
    minutes = (total_seconds % 3600) // 60
    if hours > 0:
        return f"{hours}h {minutes}m"
    return f"{minutes}m"


# ---------------------------------------------------------------------------
# Markdown table renderers
# ---------------------------------------------------------------------------

def md_table(title: str, stats: dict[str, list[int]], sort_col: int = 0) -> str:
    rows = sorted(stats.items(), key=lambda x: x[1][sort_col], reverse=True)
    if not rows:
        return f"## {title}\n\nNo data.\n"
    lines = [
        f"## {title}\n",
        "| # | User | Count | Text volume |",
        "|--:|------|------:|------------:|",
    ]
    for i, (login, (count, size)) in enumerate(rows[:20], 1):
        lines.append(f"| {i} | {login} | {count} | {fmt_kb(size)} |")
    lines.append("")
    return "\n".join(lines)


def md_table_prs(title: str, stats: dict[str, int]) -> str:
    """Table with user + PR count, sorted by count."""
    rows = sorted(stats.items(), key=lambda x: x[1], reverse=True)
    if not rows:
        return f"## {title}\n\nNo data.\n"
    lines = [
        f"## {title}\n",
        "| # | User | PRs |",
        "|--:|------|----:|",
    ]
    for i, (login, count) in enumerate(rows[:20], 1):
        lines.append(f"| {i} | {login} | {count} |")
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Time to merge
# ---------------------------------------------------------------------------

def compute_time_to_merge(
    prs: list[dict],
) -> dict[str, dict[str, Any]]:
    """Per-author: median, mean, min, max time to merge. Only merged PRs."""
    durations: dict[str, list[timedelta]] = defaultdict(list)
    for pr in prs:
        if not pr.get("merged_at"):
            continue
        login = pr.get("user", {}).get("login", "unknown")
        if is_bot(login):
            continue
        created = parse_iso(pr["created_at"])
        merged = parse_iso(pr["merged_at"])
        durations[login].append(merged - created)

    result = {}
    for login, durs in durations.items():
        secs = sorted(d.total_seconds() for d in durs)
        result[login] = {
            "count": len(secs),
            "median": timedelta(seconds=median(secs)),
            "mean": timedelta(seconds=sum(secs) / len(secs)),
            "min": timedelta(seconds=secs[0]),
            "max": timedelta(seconds=secs[-1]),
        }
    return result


def md_table_ttm(title: str, stats: dict[str, dict[str, Any]]) -> str:
    """Time-to-merge table sorted by median."""
    rows = sorted(stats.items(), key=lambda x: x[1]["median"])
    if not rows:
        return f"## {title}\n\nNo data.\n"
    lines = [
        f"## {title}\n",
        "| # | User | Merged PRs | Median | Mean | Min | Max |",
        "|--:|------|----------:|---------:|------:|-----:|-----:|",
    ]
    for i, (login, s) in enumerate(rows[:20], 1):
        lines.append(
            f"| {i} | {login} | {s['count']} "
            f"| {fmt_duration(s['median'])} | {fmt_duration(s['mean'])} "
            f"| {fmt_duration(s['min'])} | {fmt_duration(s['max'])} |"
        )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Review turnaround — true "time to first review" using PR review events
# ---------------------------------------------------------------------------

def fetch_pr_reviews(pr_number: int, repo: str) -> list[dict]:
    """Fetch review events for a single PR."""
    return gh_api_paginate(f"pulls/{pr_number}/reviews", repo)


def compute_review_turnaround(
    prs: list[dict],
    repo: str,
    log_fn=None,
) -> dict[str, dict[str, Any]]:
    """Per-reviewer: median/mean/min/max time from PR creation to first review.

    Uses the GitHub PR reviews endpoint (not inline comments) to get true
    review events. This measures how fast a reviewer submits their first
    review (APPROVED, CHANGES_REQUESTED, COMMENTED, or DISMISSED) on a PR.
    Self-reviews (author reviewing their own PR) are excluded.

    Note: this makes one API call per PR, which can be slow and rate-limit
    sensitive for repos with many PRs. Use --skip-review-turnaround to skip.
    """
    # reviewer_login -> list of turnaround durations
    turnarounds: dict[str, list[float]] = defaultdict(list)
    total = len(prs)
    failed_fetches = 0

    for idx, pr in enumerate(prs, 1):
        pr_number = pr.get("number")
        if not pr_number:
            continue
        pr_author = pr.get("user", {}).get("login", "unknown")
        created = parse_iso(pr["created_at"])

        if log_fn and idx % 50 == 0:
            log_fn(f"     reviewing PR {idx}/{total} ...")

        try:
            reviews = fetch_pr_reviews(pr_number, repo)
        except subprocess.CalledProcessError:
            failed_fetches += 1
            continue

        # Find the first review per reviewer (excluding bots and self-reviews)
        first_review_per_reviewer: dict[str, datetime] = {}
        for review in reviews:
            reviewer = review.get("user", {}).get("login", "unknown")
            if is_bot(reviewer):
                continue
            if reviewer == pr_author:
                continue
            submitted_at = review.get("submitted_at")
            if not submitted_at:
                continue
            review_time = parse_iso(submitted_at)
            if reviewer not in first_review_per_reviewer or review_time < first_review_per_reviewer[reviewer]:
                first_review_per_reviewer[reviewer] = review_time

        for reviewer, review_time in first_review_per_reviewer.items():
            delta = (review_time - created).total_seconds()
            if delta >= 0:
                turnarounds[reviewer].append(delta)

    if failed_fetches and log_fn:
        log_fn(f"     WARNING: failed to fetch reviews for {failed_fetches}/{total} PRs")

    result = {}
    for login, durations in turnarounds.items():
        if not durations:
            continue
        durations.sort()
        result[login] = {
            "prs_reviewed": len(durations),
            "median": timedelta(seconds=median(durations)),
            "mean": timedelta(seconds=sum(durations) / len(durations)),
            "min": timedelta(seconds=durations[0]),
            "max": timedelta(seconds=durations[-1]),
        }
    return result


def md_table_turnaround(title: str, stats: dict[str, dict[str, Any]]) -> str:
    rows = sorted(stats.items(), key=lambda x: x[1]["median"])
    if not rows:
        return f"## {title}\n\nNo data.\n"
    lines = [
        f"## {title}\n",
        "| # | User | PRs reviewed | Median | Mean | Min | Max |",
        "|--:|------|------------:|---------:|------:|-----:|-----:|",
    ]
    for i, (login, s) in enumerate(rows[:20], 1):
        lines.append(
            f"| {i} | {login} | {s['prs_reviewed']} "
            f"| {fmt_duration(s['median'])} | {fmt_duration(s['mean'])} "
            f"| {fmt_duration(s['min'])} | {fmt_duration(s['max'])} |"
        )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Issue lifetime
# ---------------------------------------------------------------------------

def compute_issue_lifetime(
    issues: list[dict],
) -> dict[str, Any] | None:
    """Overall issue lifetime stats (only closed issues)."""
    durations: list[float] = []
    for issue in issues:
        if not issue.get("closed_at"):
            continue
        login = issue.get("user", {}).get("login", "unknown")
        if is_bot(login):
            continue
        created = parse_iso(issue["created_at"])
        closed = parse_iso(issue["closed_at"])
        durations.append((closed - created).total_seconds())
    if not durations:
        return None
    durations.sort()
    return {
        "count": len(durations),
        "median": timedelta(seconds=median(durations)),
        "mean": timedelta(seconds=sum(durations) / len(durations)),
        "min": timedelta(seconds=durations[0]),
        "max": timedelta(seconds=durations[-1]),
    }


def md_issue_lifetime(title: str, stats: dict[str, Any] | None) -> str:
    if not stats:
        return f"## {title}\n\nNo data.\n"
    lines = [
        f"## {title}\n",
        f"Based on **{stats['count']}** closed issues.\n",
        "| Metric | Value |",
        "|--------|------:|",
        f"| Median | {fmt_duration(stats['median'])} |",
        f"| Mean | {fmt_duration(stats['mean'])} |",
        f"| Min | {fmt_duration(stats['min'])} |",
        f"| Max | {fmt_duration(stats['max'])} |",
        "",
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Email-to-login mapping
# ---------------------------------------------------------------------------

def build_email_to_login(repo: str) -> dict[str, str]:
    """Build email -> GitHub login mapping from GitHub API.

    Returns a dict mapping git author emails to GitHub logins.
    Unmapped-email detection is handled separately in compute_loc_stats()
    so it is scoped to the same commits used for LOC computation.
    """
    commits = gh_api_paginate("commits?per_page=100", repo)
    mapping: dict[str, str] = {}
    for commit in commits:
        author = commit.get("author")
        commit_data = commit.get("commit", {}).get("author", {})
        email = commit_data.get("email", "")
        if author and email and not is_bot(author.get("login", "")):
            login = author["login"]
            if email not in mapping:
                mapping[email] = login
    return mapping


# ---------------------------------------------------------------------------
# LOC stats — net delta (added lines - deleted lines) per module
# ---------------------------------------------------------------------------

def normalize_rename_path(path: str) -> str:
    """Resolve git rename notation like '{old => new}/file.rs' to the new path.

    Also handles bare renames: old.rs => new.rs

    Limitation: this is a regex-based heuristic over git numstat output.
    Exotic rename patterns (e.g. paths containing literal ' => ' or nested
    braces) may not parse correctly. For typical Rust module paths this works.
    """
    # Pattern: prefix{old => new}suffix
    result = re.sub(r"(.*?)\{[^ ]* => ([^}]*)\}(.*)", lambda m: f"{m.group(1)}{m.group(2)}{m.group(3)}", path)
    # Pattern: bare rename (no braces): old => new
    if " => " in result and "{" not in path:
        result = result.split(" => ", 1)[1]
    return result


def classify_rs_path(path: str) -> str:
    """Classify a .rs file path into a bucket: module name or '_shared'."""
    path = normalize_rename_path(path)
    m = re.match(r"^modules/([^/]+)/", path)
    if m:
        return m.group(1)
    return "_shared"


def compute_loc_stats(
    repo_path: str,
    email_to_login: dict[str, str],
    tracked_team: set[str],
    since: datetime | None,
    until_exclusive: datetime | None,
) -> tuple[dict[str, dict[str, int]] | None, set[str]]:
    """Compute Rust net LOC delta from git log --numstat.

    Net LOC delta = added lines - deleted lines. This is a measure of net
    change, not authored volume.

    Date filtering is done locally in Python using half-open intervals
    (since <= commit_time < until_exclusive) rather than relying on git's
    --since/--until flags, which have ambiguous day-boundary semantics.

    Returns (stats, unmapped_emails) where:
      - stats: {bucket: {"tracked": N, "others": N}} or None
      - unmapped_emails: emails seen in the filtered commits that have no
        GitHub login mapping (scoped to the report period only)
    """
    git_dir = Path(repo_path) / ".git"
    if not git_dir.exists():
        return None, set()

    # Fetch all Rust numstat entries with author email and ISO timestamp.
    # Date filtering is applied in Python below, not by git.
    # -M enables rename detection.
    cmd = [
        "git", "-C", repo_path, "log",
        "--format=COMMIT:%ae|%aI",
        "--numstat", "-M", "--", "*.rs",
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, check=True)

    stats: dict[str, dict[str, int]] = defaultdict(lambda: {"tracked": 0, "others": 0})
    unmapped_emails: set[str] = set()
    current_email = ""
    current_in_range = True  # whether the current commit is within the date range

    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("COMMIT:"):
            payload = line[7:]  # email|iso_timestamp
            sep = payload.rfind("|")
            if sep == -1:
                current_email = payload
                current_in_range = not since and not until_exclusive
                continue
            current_email = payload[:sep]
            ts_str = payload[sep + 1:]
            try:
                commit_time = parse_iso(ts_str)
            except (ValueError, IndexError):
                current_in_range = False
                continue
            # Half-open interval filtering: since <= commit_time < until_exclusive
            current_in_range = True
            if since and commit_time < since:
                current_in_range = False
            if until_exclusive and commit_time >= until_exclusive:
                current_in_range = False
            continue

        if not current_in_range:
            continue

        parts = line.split("\t")
        if len(parts) != 3:
            continue
        added_str, deleted_str, filepath = parts
        if added_str == "-" or deleted_str == "-":
            continue
        added = int(added_str)
        deleted = int(deleted_str)
        # Net LOC delta: added - deleted
        net = added - deleted

        login = email_to_login.get(current_email, current_email)
        if current_email and current_email not in email_to_login:
            unmapped_emails.add(current_email)
        team = "tracked" if login in tracked_team else "others"
        bucket = classify_rs_path(filepath)
        stats[bucket][team] += net

    return (dict(stats) if stats else None), unmapped_emails


def md_table_loc(
    title: str,
    stats: dict[str, dict[str, int]] | None,
    tracked_team_name: str,
    min_module_net_loc: int,
    has_tracked_team: bool = True,
) -> str:
    """Render net LOC delta table. Does not mutate the input stats dict.

    When has_tracked_team is False, renders a simplified table without the
    tracked/others split (just Area and net LOC delta).
    """
    if not stats:
        return f"## {title}\n\nNo data.\n"

    # Work on a shallow copy to avoid mutating the caller's dict
    stats_copy = dict(stats)
    shared = stats_copy.pop("_shared", {"tracked": 0, "others": 0})

    # Filter out modules with small absolute net total (hidden for readability).
    # Use --min-module-net-loc to configure the threshold (default 10).
    modules = [
        (name, m) for name, m in stats_copy.items()
        if abs(m["tracked"] + m["others"]) > min_module_net_loc
    ]
    modules.sort(key=lambda x: x[1]["tracked"] + x[1]["others"], reverse=True)

    total_tracked = shared["tracked"] + sum(m["tracked"] for _, m in modules)
    total_others = shared["others"] + sum(m["others"] for _, m in modules)
    grand_total = total_tracked + total_others

    lines = [
        f"## {title}\n",
        f"Net LOC delta = added lines − deleted lines (net change, not authored volume).\n",
        f"Modules with |net delta| ≤ {min_module_net_loc} lines hidden for readability.\n",
    ]

    if not has_tracked_team:
        # Simplified table: no tracked/others split
        lines.extend([
            "| Area | Net LOC delta |",
            "|------|------------:|",
        ])
        s_total = shared["tracked"] + shared["others"]
        lines.append(f"| **Shared code** | {s_total:,} |")
        for name, m in modules:
            m_total = m["tracked"] + m["others"]
            lines.append(f"| modules/{name} | {m_total:,} |")
        lines.append(f"| **Total** | **{grand_total:,}** |")
        lines.append("")
        return "\n".join(lines)

    others_name = "Others"

    def pct(a: int, t: int) -> str:
        return f"{a * 100 / t:.0f}%" if t > 0 else "—"

    lines.extend([
        f"| Area | {tracked_team_name} | {others_name} | Total | {tracked_team_name} % |",
        "|------|------------:|-----------:|------:|----------:|",
    ])
    s_total = shared["tracked"] + shared["others"]
    lines.append(
        f"| **Shared code** | {shared['tracked']:,} | {shared['others']:,} "
        f"| {s_total:,} | {pct(shared['tracked'], s_total)} |"
    )

    for name, m in modules:
        m_total = m["tracked"] + m["others"]
        lines.append(
            f"| modules/{name} | {m['tracked']:,} | {m['others']:,} "
            f"| {m_total:,} | {pct(m['tracked'], m_total)} |"
        )

    lines.append(
        f"| **Total** | **{total_tracked:,}** | **{total_others:,}** "
        f"| **{grand_total:,}** | **{pct(total_tracked, grand_total)}** |"
    )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="GitHub repo activity stats")
    p.add_argument("repo", nargs="?", default=DEFAULT_REPO, help="owner/repo")
    p.add_argument("--since", type=str, default=None, help="Start date inclusive (YYYY-MM-DD)")
    p.add_argument("--until", type=str, default=None, help="End date inclusive (YYYY-MM-DD)")
    p.add_argument(
        "--exclude-labels",
        type=str,
        default=None,
        help="Comma-separated issue labels to exclude (default: pr-issue)",
    )
    p.add_argument(
        "--repo-path",
        type=str,
        default=".",
        help="Path to local git clone for LOC stats (default: current directory)",
    )
    p.add_argument(
        "--tracked-team-file",
        type=str,
        default=None,
        help=(
            "Path to a text file with tracked team GitHub logins (one per line). "
            "Blank lines and lines starting with # are ignored. "
            "This file is intended for local/private use."
        ),
    )
    p.add_argument(
        "--tracked-team-login",
        type=str,
        default=None,
        help="Comma-separated list of tracked team GitHub logins",
    )
    p.add_argument(
        "--tracked-team-name",
        type=str,
        default="Tracked team",
        help="Display name for the tracked team in report output (default: 'Tracked team')",
    )
    p.add_argument(
        "--min-module-net-loc",
        type=int,
        default=10,
        help=(
            "Hide modules with |net LOC delta| ≤ this threshold "
            "in the LOC table for readability (default: 10)"
        ),
    )
    p.add_argument(
        "--skip-review-turnaround",
        action="store_true",
        default=False,
        help=(
            "Skip the review turnaround metric. This metric makes one API "
            "call per PR and can be slow or hit rate limits on active repos."
        ),
    )
    return p.parse_args()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    args = parse_args()
    repo = args.repo
    since = parse_since_date(args.since)
    until_exclusive = parse_until_exclusive(args.until)

    if since and until_exclusive and since >= until_exclusive:
        print(
            f"ERROR: --since ({args.since}) is after --until ({args.until}). Check the dates.",
            file=sys.stderr,
        )
        raise SystemExit(1)

    tracked_team = build_tracked_team(args)

    exclude_labels = set(
        args.exclude_labels.split(",") if args.exclude_labels else DEFAULT_EXCLUDE_LABELS
    )

    tracked_team_name = args.tracked_team_name
    min_module_net_loc = args.min_module_net_loc

    date_label = ""
    if args.since or args.until:
        parts = []
        if args.since:
            parts.append(f"from {args.since}")
        if args.until:
            parts.append(f"to {args.until}")
        date_label = " (" + " ".join(parts) + ")"

    def log(msg: str):
        print(msg, file=sys.stderr)

    log(f"Fetching stats for **{repo}**{date_label} ...\n")

    if tracked_team:
        log(f"  Tracked team: {len(tracked_team)} members loaded")
    else:
        log("  WARNING: No tracked team members configured. LOC breakdown will attribute everything to 'Others'.")

    # -------------------------------------------------------------------
    # Fetch raw data from GitHub API
    # -------------------------------------------------------------------

    # PR review comments (inline on code) — filtered by created_at
    log("  -> PR review comments ...")
    pr_review_raw = gh_api_paginate("pulls/comments", repo)
    pr_review = filter_by_date(pr_review_raw, since, until_exclusive)
    pr_review_stats = aggregate(pr_review)

    # Issue / PR general comments (discussions) — filtered by created_at
    log("  -> Issue/PR general comments ...")
    general_comments_raw = gh_api_paginate("issues/comments", repo)
    general_comments = filter_by_date(general_comments_raw, since, until_exclusive)
    general_stats = aggregate(general_comments)

    # Issues (excluding PRs and issues with excluded labels)
    log("  -> Issues ...")
    issues_raw = gh_api_paginate("issues?state=all&per_page=100", repo)
    issues_no_prs = [
        i for i in issues_raw
        if i.get("pull_request") is None and not has_excluded_label(i, exclude_labels)
    ]

    # Issues created in period — for "Issues Created" table
    issues_created = filter_by_date(issues_no_prs, since, until_exclusive, date_field="created_at")
    issue_stats = aggregate(issues_created)

    # Issues closed in period — for "Issue Lifetime" metric
    issues_closed = filter_by_date(issues_no_prs, since, until_exclusive, date_field="closed_at")

    # PRs
    log("  -> Pull requests ...")
    prs_raw = gh_api_paginate("pulls?state=all&per_page=100", repo)
    prs_no_excluded = [p for p in prs_raw if not has_excluded_label(p, exclude_labels)]

    # PRs created in period — for "PRs Authored" count
    prs_created = filter_by_date(prs_no_excluded, since, until_exclusive, date_field="created_at")
    pr_author_counts: dict[str, int] = defaultdict(int)
    for pr in prs_created:
        login = pr.get("user", {}).get("login", "unknown")
        if not is_bot(login):
            pr_author_counts[login] += 1

    # PRs merged in period — for "Time to Merge" metric
    prs_merged = filter_by_date(prs_no_excluded, since, until_exclusive, date_field="merged_at")

    # -------------------------------------------------------------------
    # Compute derived metrics
    # -------------------------------------------------------------------

    # Time to merge — based on PRs merged in the requested period
    log("  -> Computing time-to-merge ...")
    ttm_stats = compute_time_to_merge(prs_merged)

    # Review turnaround — true first review using PR review events.
    # Uses PRs created in the period as the basis.
    # Skipped with --skip-review-turnaround (one API call per PR, can be slow).
    turnaround_stats: dict[str, dict[str, Any]] = {}
    if args.skip_review_turnaround:
        log("  -> Skipping review turnaround (--skip-review-turnaround)")
        out.append("## Review Turnaround (time to first review)\n\nSkipped via `--skip-review-turnaround`.\n")
    else:
        log("  -> Computing review turnaround (fetching PR reviews) ...")
        turnaround_stats = compute_review_turnaround(prs_created, repo, log_fn=log)
        out.append(md_table_turnaround("Review Turnaround (time to first review)", turnaround_stats))

    # Issue lifetime — based on issues closed in the requested period
    log("  -> Computing issue lifetime ...")
    issue_lifetime = compute_issue_lifetime(issues_closed)

    # Rust net LOC delta stats (requires local git repo)
    loc_stats = None
    unmapped_emails: set[str] = set()
    repo_path = os.path.abspath(args.repo_path)
    if Path(repo_path, ".git").exists():
        log("  -> Building email->login mapping ...")
        email_to_login = build_email_to_login(repo)
        log(f"     ({len(email_to_login)} unique emails mapped)")
        log("  -> Computing Rust net LOC delta (git log --numstat) ...")
        loc_stats, unmapped_emails = compute_loc_stats(
            repo_path, email_to_login, tracked_team, since, until_exclusive,
        )
        if unmapped_emails:
            log(f"     WARNING: {len(unmapped_emails)} email(s) in the report period could not be mapped to GitHub logins.")
            log(f"     Unmapped emails: {', '.join(sorted(unmapped_emails))}")
            log("     Commits from these emails are attributed to 'Others' in the LOC breakdown.")
    else:
        log(f"  -> Skipping LOC stats (no .git in {repo_path})")

    # -------------------------------------------------------------------
    # Build report
    # -------------------------------------------------------------------
    out: list[str] = []
    out.append(f"# GitHub Activity Report: {repo}{date_label}\n")
    out.append(f"Excluded issue labels: {', '.join(sorted(exclude_labels))}\n")
    out.append("Bots filtered out.\n")

    out.append(md_table_prs("PRs Authored", pr_author_counts))
    out.append(md_table_ttm("Time to Merge (by author, PRs merged in period)", ttm_stats))
    out.append(md_table_turnaround("Review Turnaround (time to first review)", turnaround_stats))
    out.append(md_table("PR Review Comments (inline on code)", pr_review_stats))
    out.append(md_table("Issue / PR General Comments", general_stats))
    out.append(md_table("Issues Created", issue_stats))
    out.append(md_issue_lifetime("Issue Lifetime (issues closed in period)", issue_lifetime))

    loc_title = f"Rust Net LOC Delta by {tracked_team_name}" if tracked_team else "Rust Net LOC Delta"
    out.append(md_table_loc(loc_title, loc_stats, tracked_team_name, min_module_net_loc,
                             has_tracked_team=bool(tracked_team)))

    # Unmapped email notice in report
    if unmapped_emails:
        out.append(f"**Note:** {len(unmapped_emails)} git email(s) in the report period could not be mapped "
                    f"to GitHub logins and are counted as 'Others' in LOC stats. "
                    f"Run with `--tracked-team-login` or fix email mappings to improve accuracy.\n")

    # Summary
    out.append("## Summary\n")
    top_pr_author = max(pr_author_counts.items(), key=lambda x: x[1], default=None)
    top_reviewer = max(pr_review_stats.items(), key=lambda x: x[1][0], default=None)
    top_reviewer_vol = max(pr_review_stats.items(), key=lambda x: x[1][1], default=None)
    top_issue_author = max(issue_stats.items(), key=lambda x: x[1][1], default=None)
    if top_pr_author:
        out.append(f"- **Most PRs authored**: {top_pr_author[0]} ({top_pr_author[1]} PRs)")
    if top_reviewer:
        out.append(f"- **Most inline PR review comments by count**: {top_reviewer[0]} ({top_reviewer[1][0]} comments)")
    if top_reviewer_vol:
        out.append(f"- **Most inline PR review comment text**: {top_reviewer_vol[0]} ({fmt_kb(top_reviewer_vol[1][1])})")
    if top_issue_author:
        out.append(f"- **Largest issue author by volume**: {top_issue_author[0]} ({top_issue_author[1][0]} issues, {fmt_kb(top_issue_author[1][1])})")
    if issue_lifetime:
        out.append(f"- **Median issue lifetime**: {fmt_duration(issue_lifetime['median'])}")
    out.append("")

    print("\n".join(out))


if __name__ == "__main__":
    main()
