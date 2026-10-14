#!/usr/bin/env python3
"""Collect Actions run attempts and compare matched cold/warm job samples.

Uses the authenticated gh CLI for read-only API access. Never launches runs or
deletes caches. Job duration includes every step and post-step; start offset
is deliberately not labelled runner queue time (it includes dependencies).
"""
from __future__ import annotations

import argparse
from datetime import datetime
import json
from pathlib import Path
import statistics
import subprocess


def instant(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def api(endpoint: str, pages=False):
    command = ["gh", "api"]
    if pages:
        command += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(command + [endpoint], text=True))


def sample(run: dict, jobs: list[dict], cache_state: str) -> dict:
    rows = []
    for job in jobs:
        start, end = job.get("started_at"), job.get("completed_at")
        steps = []
        for step in job.get("steps", []):
            a, b = step.get("started_at"), step.get("completed_at")
            steps.append({"name": step["name"], "conclusion": step.get("conclusion"),
                          "seconds": (instant(b)-instant(a)).total_seconds() if a and b else None})
        rows.append({"name": job["name"], "id": job["id"], "conclusion": job.get("conclusion"),
                     "seconds": (instant(end)-instant(start)).total_seconds() if start and end else None,
                     "labels": job.get("labels", []), "runner_name": job.get("runner_name"),
                     "start_offset_seconds": (instant(start)-instant(run["run_started_at"])).total_seconds() if start else None,
                     "steps": steps})
    completed = [instant(j["completed_at"]) for j in jobs if j.get("completed_at")]
    return {"run_id": run["id"], "attempt": run["run_attempt"], "sha": run["head_sha"],
            "url": run["html_url"], "event": run["event"], "conclusion": run.get("conclusion"),
            "workflow": run["name"], "cache_state": cache_state, "jobs": rows,
            "runner_seconds": sum(j["seconds"] or 0 for j in rows if j["conclusion"] != "skipped"),
            "attempt_elapsed_seconds": (max(completed)-instant(run["run_started_at"])).total_seconds() if completed else None}


def collect(args):
    prefix = f"repos/{args.repo}/actions/runs/{args.run}"
    run = api(prefix + (f"/attempts/{args.attempt}" if args.attempt else ""))
    if run["status"] != "completed":
        raise SystemExit("Wait for the run, including post-steps, to complete")
    pages = api(prefix + f"/attempts/{run['run_attempt']}/jobs?per_page=100", pages=True)
    entry = sample(run, [job for page in pages for job in page["jobs"]], args.cache_state)
    data = json.loads(args.output.read_text(encoding="utf-8")) if args.output.exists() else {"schema": 1, "repo": args.repo, "samples": []}
    if data["repo"] != args.repo:
        raise SystemExit("Use separate files for different repositories")
    key = (entry["run_id"], entry["attempt"])
    if any((x["run_id"], x["attempt"]) == key for x in data["samples"]):
        raise SystemExit("This run attempt has already been collected")
    data["samples"].append(entry)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"Saved {entry['url']} attempt {entry['attempt']} ({args.cache_state}) to {args.output}")


def grouped(data: dict) -> dict[tuple, list[float]]:
    values = {}
    for run in data["samples"]:
        if run["conclusion"] != "success":
            raise ValueError(f"Failed/cancelled run cannot establish acceleration: {run['url']}")
        for job in run["jobs"]:
            if job["conclusion"] == "skipped":
                continue
            if job["conclusion"] != "success" or job["seconds"] is None:
                raise ValueError(f"Incomplete or failing job: {job['name']}")
            key = (run["workflow"], run["cache_state"], job["name"], tuple(sorted(job["labels"])))
            values.setdefault(key, []).append(job["seconds"])
    return values


def comparison(before: dict, after: dict) -> str:
    if before["repo"] != after["repo"]:
        raise ValueError("Compare runs in the same repository")
    events_before = {run["event"] for run in before["samples"]}
    events_after = {run["event"] for run in after["samples"]}
    if len(events_before) != 1 or events_before != events_after:
        raise ValueError("Compare one matching event type; do not mix PR and manual runs")
    left, right = grouped(before), grouped(after)
    lines = []
    for key in sorted(left.keys() | right.keys()):
        label = f"{key[0]} / {key[1]} / {key[2]}"
        if key not in left or key not in right:
            lines.append(f"UNMATCHED: {label}; no speed claim")
            continue
        a, b = statistics.median(left[key]), statistics.median(right[key])
        gain = (a-b)/a*100 if a else 0
        lines.append(f"{label}: {a:.1f}s -> {b:.1f}s ({gain:+.1f}%), samples {len(left[key])}/{len(right[key])}; "
                     f"ranges {min(left[key]):.1f}..{max(left[key]):.1f}s / {min(right[key]):.1f}..{max(right[key]):.1f}s")
        if min(len(left[key]), len(right[key])) < 3:
            lines.append("  Preliminary: fewer than three samples per variant")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    gather = sub.add_parser("collect")
    gather.add_argument("--repo", required=True)
    gather.add_argument("--run", required=True, type=int)
    gather.add_argument("--attempt", type=int)
    gather.add_argument("--cache-state", required=True, choices=("cold", "warm"))
    gather.add_argument("--output", required=True, type=Path)
    compare = sub.add_parser("compare")
    compare.add_argument("baseline", type=Path)
    compare.add_argument("optimized", type=Path)
    args = parser.parse_args()
    if args.command == "collect":
        collect(args)
    else:
        try:
            print(comparison(json.loads(args.baseline.read_text(encoding="utf-8")), json.loads(args.optimized.read_text(encoding="utf-8"))))
        except ValueError as error:
            raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
