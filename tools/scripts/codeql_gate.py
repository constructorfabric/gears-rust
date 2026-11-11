#!/usr/bin/env python3
"""Release gate: fail when CodeQL has open high/critical alerts that nobody accepted.

Used by .github/workflows/release-plz.yml after the CodeQL run. It lists the open CodeQL
code scanning alerts of the ref being released and removes the ones listed in
`.github/codeql-allowlist.toml`. Any alert left with security severity high or critical
fails the gate, and so blocks publishing crates.

To accept an alert, add an entry to the allowlist in a pull request (rule, path, reason);
the review of that PR is the approval. An entry may carry `expires = "YYYY-MM-DD"`;
after that date it stops suppressing. Dismissing the alert in the Security tab also
works: dismissed alerts are not open.

Environment: GH_TOKEN, GITHUB_REPOSITORY, GITHUB_REF (set by Actions).
`--alerts-file FILE` reads the alerts from a JSON file instead of the API (for tests).
"""
import argparse
import datetime
import fnmatch
import json
import os
import subprocess
import sys
import tomllib

BLOCKING = {"high", "critical"}
ALLOWLIST = ".github/codeql-allowlist.toml"


def fetch_alerts(repo: str, ref: str) -> list[dict]:
    out = subprocess.run(
        ["gh", "api", "--paginate", "--slurp",
         f"repos/{repo}/code-scanning/alerts?state=open&tool_name=CodeQL&ref={ref}&per_page=100"],
        check=True, capture_output=True, text=True,
    ).stdout
    return [a for page in json.loads(out) for a in page]


def load_allowlist(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, "rb") as f:
        entries = tomllib.load(f).get("ignore", [])
    for e in entries:
        missing = [k for k in ("rule", "path", "reason") if not str(e.get(k, "")).strip()]
        if missing:
            sys.exit(f"::error file={path}::allowlist entry {e} lacks: {', '.join(missing)}")
    return entries


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--alerts-file")
    ap.add_argument("--allowlist", default=ALLOWLIST)
    ap.add_argument("--today", default=datetime.date.today().isoformat())
    args = ap.parse_args()

    if args.alerts_file:
        alerts = json.load(open(args.alerts_file))
    else:
        alerts = fetch_alerts(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_REF"])

    entries = load_allowlist(args.allowlist)
    used = set()
    blocking = []
    for a in alerts:
        sev = (a["rule"].get("security_severity_level") or "").lower()
        if sev not in BLOCKING:
            continue
        rule = a["rule"]["id"]
        path = a["most_recent_instance"]["location"]["path"]
        hit = None
        for i, e in enumerate(entries):
            if e["rule"] == rule and fnmatch.fnmatch(path, e["path"]):
                if str(e.get("expires", "9999-12-31")) < args.today:
                    print(f"::warning file={args.allowlist}::allowlist entry for {rule} {e['path']} expired {e['expires']}")
                    continue
                hit = i
                break
        if hit is None:
            blocking.append((a["number"], sev, rule, path, a.get("html_url", "")))
        else:
            used.add(hit)
            print(f"allowed: #{a['number']} {sev} {rule} {path} ({entries[hit]['reason']})")

    for i, e in enumerate(entries):
        if i not in used:
            print(f"::warning file={args.allowlist}::allowlist entry for {e['rule']} {e['path']} matches no open alert; remove it")

    if blocking:
        for n, sev, rule, path, url in blocking:
            print(f"::error file={path}::CodeQL alert #{n} ({sev}) {rule} {url}")
        print(f"\n{len(blocking)} open high/critical CodeQL alert(s) block the release. Fix the code, "
              f"dismiss the alert in the Security tab, or add it to {ALLOWLIST} in a PR.")
        return 1
    print("No blocking CodeQL alerts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
