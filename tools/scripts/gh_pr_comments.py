#!/usr/bin/env python3
"""Print a pull request's review threads in a compact, model-friendly form.

Usage:
    python3 tools/scripts/gh_pr_comments.py <pr> [--repo OWNER/NAME]
        [--author REGEX] [--all] [--include-outdated] [--keep-details]
        [--max-chars N] [--json]

By default this prints only unresolved, current threads. Each thread is shown
as `path:line`, the id of its first comment (for `gh api .../replies`), the
author and a cleaned body. Bot boilerplate is removed: HTML comments, tags and
collapsed `<details>` blocks (suggested patches, walkthroughs, AI prompts).

Requires: gh CLI installed and authenticated.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from typing import Any

DEFAULT_REPO = "constructorfabric/gears-rust"

QUERY = """
query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          isResolved
          isOutdated
          path
          line
          originalLine
          comments(first: 50) {
            nodes { databaseId author { login } body }
          }
        }
      }
    }
  }
}
"""

_DETAILS = re.compile(r"<details\b[^>]*>(?:(?!<details\b).)*?</details>", re.S | re.I)
_HTML_COMMENT = re.compile(r"<!--.*?-->", re.S)
_TAG = re.compile(r"</?[a-zA-Z][^>]*>")
_BLANKS = re.compile(r"\n\s*\n+")
# CodeRabbit boilerplate: the "shout-out" footer and raw-file source links.
_BOT_FOOTER = re.compile(r"\n-{3,}\s*\nIf you found this review helpful.*", re.S)
_RAW_LINK = re.compile(r"\s*\(\[raw\.githubusercontent\.com\]\([^)]*\)\)")


def clean_body(body: str, keep_details: bool = False, max_chars: int = 1500) -> str:
    """Strip HTML noise from a review comment and cap its length."""
    text = _HTML_COMMENT.sub("", body)
    text = _BOT_FOOTER.sub("", text)
    text = _RAW_LINK.sub("", text)
    if not keep_details:
        # Remove innermost blocks first so nested <details> collapse fully.
        while True:
            stripped = _DETAILS.sub("", text)
            if stripped == text:
                break
            text = stripped
    text = _TAG.sub("", text)
    text = _BLANKS.sub("\n", text).strip()
    if max_chars and len(text) > max_chars:
        text = text[:max_chars].rstrip() + " […]"
    return text


def fetch_threads(repo: str, number: int) -> list[dict[str, Any]]:
    owner, name = repo.split("/", 1)
    threads: list[dict[str, Any]] = []
    cursor = None
    while True:
        cmd = [
            "gh", "api", "graphql",
            "-f", f"query={QUERY}",
            "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"number={number}",
        ]
        if cursor:
            cmd += ["-f", f"cursor={cursor}"]
        out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
        page = json.loads(out)["data"]["repository"]["pullRequest"]["reviewThreads"]
        threads.extend(page["nodes"])
        if not page["pageInfo"]["hasNextPage"]:
            return threads
        cursor = page["pageInfo"]["endCursor"]


def select(
    threads: list[dict[str, Any]],
    author: str | None = None,
    include_resolved: bool = False,
    include_outdated: bool = False,
) -> list[dict[str, Any]]:
    """Filter threads; `author` matches the login of the thread's first comment."""
    pattern = re.compile(author) if author else None
    picked = []
    for t in threads:
        comments = t["comments"]["nodes"]
        if not comments:
            continue
        if t["isResolved"] and not include_resolved:
            continue
        if t["isOutdated"] and not include_outdated:
            continue
        login = (comments[0].get("author") or {}).get("login", "ghost")
        if pattern and not pattern.search(login):
            continue
        picked.append(t)
    return picked


def summarize(t: dict[str, Any], keep_details: bool, max_chars: int) -> dict[str, Any]:
    comments = t["comments"]["nodes"]
    first = comments[0]
    replies = [
        {
            "author": (c.get("author") or {}).get("login", "ghost"),
            "body": clean_body(c["body"], keep_details, max_chars // 3),
        }
        for c in comments[1:]
    ]
    return {
        "id": first["databaseId"],
        "path": t["path"],
        "line": t["line"] or t["originalLine"],
        "author": (first.get("author") or {}).get("login", "ghost"),
        "resolved": t["isResolved"],
        "outdated": t["isOutdated"],
        "body": clean_body(first["body"], keep_details, max_chars),
        "replies": replies,
    }


def render(items: list[dict[str, Any]]) -> str:
    out = []
    for it in items:
        flags = [f for f in ("resolved", "outdated") if it[f]]
        head = f"== #{it['id']} {it['path']}:{it['line']} @{it['author']}"
        if flags:
            head += f" ({', '.join(flags)})"
        out.append(head)
        out.append(it["body"])
        for r in it["replies"]:
            out.append(f"  ↳ @{r['author']}: {r['body']}")
    out.append(f"-- {len(items)} thread(s)")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("pr", type=int)
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--author", help="regex on the first comment's login, e.g. coderabbit")
    ap.add_argument("--all", action="store_true", help="include resolved threads")
    ap.add_argument("--include-outdated", action="store_true")
    ap.add_argument("--keep-details", action="store_true", help="keep <details> blocks")
    ap.add_argument("--max-chars", type=int, default=1500, help="cap per comment body; 0 = no cap")
    ap.add_argument("--json", action="store_true", help="print JSON instead of text")
    args = ap.parse_args(argv)

    try:
        threads = fetch_threads(args.repo, args.pr)
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr or str(exc))
        return 1
    picked = select(threads, args.author, args.all, args.include_outdated)
    items = [summarize(t, args.keep_details, args.max_chars) for t in picked]
    print(json.dumps(items, indent=1, ensure_ascii=False) if args.json else render(items))
    return 0


if __name__ == "__main__":
    sys.exit(main())
