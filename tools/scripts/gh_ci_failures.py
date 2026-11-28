#!/usr/bin/env python3
"""Show why a pull request's CI failed, without paging through full logs.

Usage:
    python3 tools/scripts/gh_ci_failures.py <pr> [--repo OWNER/NAME]
        [--wait] [--timeout SECONDS] [--tail N] [--rerun]

Prints one line per failed or cancelled check. For each GitHub Actions job it
also prints, from `gh run view --log-failed` with timestamps and colour codes
removed: the error lines (runner `##[error]`, rustc errors, test panics,
nextest failures) and the N lines leading up to the final `##[error]`. Checks from external apps are
listed with their link only.

--wait   poll until no check is pending, then report (replaces sleep loops)
--rerun  rerun the failed jobs of every affected workflow run once

Exit status: 0 when nothing failed, 1 when something failed, 2 on gh errors or
a --wait timeout.

Requires: gh CLI installed and authenticated.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from typing import Any

DEFAULT_REPO = "constructorfabric/gears-rust"
FAILED_BUCKETS = {"fail", "cancel"}
JOB_LINK = re.compile(r"/actions/runs/(\d+)/job/(\d+)")
# gh prints ESC either raw or as a literal "^[" depending on the runner.
_ANSI = re.compile(r"(?:\x1b|\^\[)\[[0-9;]*[A-Za-z]")
_TIMESTAMP = re.compile(r"^\ufeff?\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z ?")
# Lines worth showing on their own: runner error annotations, rustc errors,
# test panics and nextest/cargo-test failure summaries.
_HIGHLIGHT = re.compile(
    r"##\[error\]|panicked at|^\s*(?:FAIL|TIMEOUT|SIGSEGV|SIGABRT|LEAK-FAIL) \[|"
    r"^error(?:\[E\d+\])?:|test result: FAILED|^\s*Summary \[|^make(?:\[\d+\])?: \*\*\*"
)
MAX_HIGHLIGHTS = 30


def gh(*args: str) -> str:
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def checks(repo: str, pr: int) -> list[dict[str, Any]]:
    # `gh pr checks` exits 8 while checks are pending and 1 when some failed;
    # the JSON on stdout is valid in both cases.
    proc = subprocess.run(
        ["gh", "pr", "checks", str(pr), "--repo", repo,
         "--json", "name,state,bucket,link,workflow"],
        capture_output=True, text=True,
    )
    if not proc.stdout.strip():
        if "no checks reported" in proc.stderr:
            return []
        raise subprocess.CalledProcessError(proc.returncode, proc.args, proc.stdout, proc.stderr)
    return json.loads(proc.stdout)


def job_ids(link: str) -> tuple[str, str] | None:
    m = JOB_LINK.search(link or "")
    return (m.group(1), m.group(2)) if m else None


def clean_log(log: str) -> list[str]:
    """Turn `--log-failed` output (job<TAB>step<TAB>ts line) into plain lines."""
    lines = []
    for raw in log.splitlines():
        parts = raw.split("\t", 2)
        text = parts[2] if len(parts) == 3 else raw
        text = _ANSI.sub("", _TIMESTAMP.sub("", text)).rstrip()
        if text:
            lines.append(text)
    return lines


def summarize_log(log: str, tail: int) -> tuple[list[str], list[str]]:
    """Return (highlights, context).

    On self-hosted runners `--log-failed` returns the whole job log and its
    end is post-job cleanup, so the context window ends at the last
    `##[error]` line instead of the end of the log.
    """
    lines = clean_log(log)
    highlights: list[str] = []
    seen: set[str] = set()
    for line in lines:
        if _HIGHLIGHT.search(line) and line not in seen:
            seen.add(line)
            highlights.append(line)
    if len(highlights) > MAX_HIGHLIGHTS:
        highlights = highlights[:10] + ["…"] + highlights[-(MAX_HIGHLIGHTS - 10):]
    end = len(lines)
    for i in range(len(lines) - 1, -1, -1):
        if "##[error]" in lines[i]:
            end = i + 1
            break
    start = max(0, end - tail) if tail else 0
    return highlights, lines[start:end]


def wait_for(repo: str, pr: int, timeout: int, interval: int = 60) -> list[dict[str, Any]]:
    deadline = time.monotonic() + timeout
    while True:
        current = checks(repo, pr)
        pending = [c for c in current if c["bucket"] == "pending"]
        if not pending:
            return current
        if time.monotonic() > deadline:
            names = ", ".join(c["name"] for c in pending[:5])
            raise TimeoutError(f"{len(pending)} check(s) still pending: {names}")
        time.sleep(interval)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("pr", type=int)
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--wait", action="store_true", help="wait until no check is pending")
    ap.add_argument("--timeout", type=int, default=3600, help="--wait limit in seconds")
    ap.add_argument("--tail", type=int, default=40, help="context lines per failed job; 0 = all")
    ap.add_argument("--rerun", action="store_true", help="rerun failed jobs of affected runs")
    args = ap.parse_args(argv)

    try:
        current = wait_for(args.repo, args.pr, args.timeout) if args.wait else checks(args.repo, args.pr)
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr or str(exc))
        return 2
    except TimeoutError as exc:
        print(exc)
        return 2

    counts: dict[str, int] = {}
    for c in current:
        counts[c["bucket"]] = counts.get(c["bucket"], 0) + 1
    print("checks: " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())))

    failed = [c for c in current if c["bucket"] in FAILED_BUCKETS]
    runs: set[str] = set()
    for c in failed:
        print(f"\n== {c['bucket'].upper()} {c['workflow']} / {c['name']}\n   {c['link']}")
        ids = job_ids(c["link"])
        if not ids:
            continue
        runs.add(ids[0])
        if c["bucket"] != "fail":
            continue
        try:
            log = gh("run", "view", ids[0], "--repo", args.repo, "--job", ids[1], "--log-failed")
        except subprocess.CalledProcessError as exc:
            print(f"   (log unavailable: {(exc.stderr or '').strip()[:200]})")
            continue
        highlights, context = summarize_log(log, args.tail)
        if highlights:
            print("   -- highlights")
            for line in highlights:
                print("   " + line)
        print(f"   -- last {len(context)} lines up to the final error")
        for line in context:
            print("   " + line)

    if args.rerun and runs:
        for run in sorted(runs):
            try:
                gh("run", "rerun", run, "--repo", args.repo, "--failed")
                print(f"\nrerun requested: run {run}")
            except subprocess.CalledProcessError as exc:
                print(f"\nrerun failed for run {run}: {(exc.stderr or '').strip()[:200]}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
