#!/usr/bin/env python3
"""Release gate: fail when CodeQL has open high/critical alerts that nobody accepted.

Used by .github/workflows/release-plz.yml after the CodeQL run. It lists the open CodeQL
code scanning alerts of the ref being released and removes the ones listed in
`.github/codeql-allowlist.toml`. Any alert left with security severity high or critical
fails the gate, and so blocks publishing crates.

To accept an alert, add an entry to the allowlist in a pull request (rule, path, reason);
the review of that PR is the approval. An entry may carry `expires = "YYYY-MM-DD"`;
after that date it stops suppressing. An entry may carry `alert = N` to accept only that
alert number instead of every alert of the rule in the matching path. Dismissing the alert in the Security tab also
works: dismissed alerts are not open.

The alerts API is keyed by ref (branch), not by commit, so the gate first requires that the
latest CodeQL analysis of the ref is the one for GITHUB_SHA and that every alert's most recent
instance is on that commit; otherwise it fails closed (a later push was analysed first, re-run).

Environment: GH_TOKEN, GITHUB_REPOSITORY, GITHUB_REF, GITHUB_SHA (set by Actions).
Tests call `evaluate()` directly with alerts and allowlist entries; the CLI takes no file paths.
"""
import datetime
import fnmatch
import json
import os
import subprocess
import sys
import tomllib
import urllib.parse

BLOCKING = {"high", "critical"}
ALLOWLIST = ".github/codeql-allowlist.toml"


def gh_api(path: str) -> list[dict]:
    out = subprocess.run(
        ["gh", "api", "--paginate", "--slurp", path],
        check=True, capture_output=True, text=True,
    ).stdout
    return [a for page in json.loads(out) for a in page]


def fetch_alerts(repo: str, ref: str) -> list[dict]:
    q = urllib.parse.quote(ref, safe="")
    return gh_api(f"repos/{repo}/code-scanning/alerts?state=open&tool_name=CodeQL&ref={q}&per_page=100")


def fetch_latest_analysis_sha(repo: str, ref: str) -> str | None:
    """Commit SHA of the most recent CodeQL analysis of `ref` (the API lists newest first)."""
    q = urllib.parse.quote(ref, safe="")
    out = subprocess.run(
        ["gh", "api", f"repos/{repo}/code-scanning/analyses?tool_name=CodeQL&ref={q}&per_page=1"],
        check=True, capture_output=True, text=True,
    ).stdout
    analyses = json.loads(out)
    return analyses[0].get("commit_sha") if analyses else None


def commit_mismatch(latest_sha: str | None, alerts: list[dict], sha: str) -> str | None:
    """Reason the results cannot be tied to `sha`, or None when they are."""
    if not sha:
        return "GITHUB_SHA is not set"
    if latest_sha != sha:
        return f"the latest CodeQL analysis of the ref is for {latest_sha}, not {sha}"
    stale = [a["number"] for a in alerts if a["most_recent_instance"].get("commit_sha") != sha]
    if stale:
        return f"alert(s) {stale} have a most recent instance on another commit than {sha}"
    return None


def load_allowlist(path: str) -> list[dict]:
    if not os.path.exists(path):
        return []
    with open(path, "rb") as f:
        entries = tomllib.load(f).get("ignore", [])
    for e in entries:
        missing = [k for k in ("rule", "path", "reason") if not str(e.get(k, "")).strip()]
        if missing:
            sys.exit(f"::error file={path}::allowlist entry {e} lacks: {', '.join(missing)}")
        if "expires" in e:
            try:
                datetime.date.fromisoformat(str(e["expires"]))
            except ValueError:
                sys.exit(f"::error file={path}::allowlist entry {e}: expires must be YYYY-MM-DD")
        if "alert" in e and not isinstance(e["alert"], int):
            sys.exit(f"::error file={path}::allowlist entry {e}: alert must be an integer alert number")
    return entries


def evaluate(alerts: list[dict], entries: list[dict], today: str) -> int:
    used = set()
    blocking = []
    today_date = datetime.date.fromisoformat(today)
    for a in alerts:
        sev = (a["rule"].get("security_severity_level") or "").lower()
        if sev not in BLOCKING:
            continue
        rule = a["rule"]["id"]
        path = a["most_recent_instance"]["location"]["path"]
        hit = None
        for i, e in enumerate(entries):
            if e["rule"] == rule and fnmatch.fnmatch(path, e["path"]) and e.get("alert", a["number"]) == a["number"]:
                if "expires" in e and datetime.date.fromisoformat(str(e["expires"])) < today_date:
                    print(f"::warning file={ALLOWLIST}::allowlist entry for {rule} {e['path']} expired {e['expires']}")
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
            print(f"::warning file={ALLOWLIST}::allowlist entry for {e['rule']} {e['path']} matches no open alert; remove it")

    if blocking:
        for n, sev, rule, path, url in blocking:
            print(f"::error file={path}::CodeQL alert #{n} ({sev}) {rule} {url}")
        print(f"\n{len(blocking)} open high/critical CodeQL alert(s) block the release. Fix the code, "
              f"dismiss the alert in the Security tab, or add it to {ALLOWLIST} in a PR.")
        return 1
    print("No blocking CodeQL alerts.")
    return 0


def main() -> int:
    repo, ref, sha = os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_REF"], os.environ.get("GITHUB_SHA", "")
    alerts = fetch_alerts(repo, ref)
    reason = commit_mismatch(fetch_latest_analysis_sha(repo, ref), alerts, sha)
    if reason:
        print(f"::error::CodeQL results cannot be tied to the commit being released: {reason}. "
              "Failing closed; re-run once the analysis of this commit is the latest.")
        return 1
    return evaluate(alerts, load_allowlist(ALLOWLIST), datetime.date.today().isoformat())


if __name__ == "__main__":
    sys.exit(main())
