---
description: Review a GitHub PR for Rust + ToolKit compliance. Runs 6 specialized agents in parallel, then posts inline comments.
---

# ToolKit PR Review

Review a GitHub PR for Rust + ToolKit compliance and post inline comments.

**Usage**: `/toolkit-pr-review-v2 <PR_NUMBER> [--repo <owner/repo>]`

Agent definitions live in `docs/pr-review/agents/`. Review checklists live in `docs/pr-review/`.

---

## Table of Contents

- [Step 1: Resolve repository](#step-1-resolve-repository)
- [Step 2: Fetch PR metadata and diff](#step-2-fetch-pr-metadata-and-diff)
- [Step 3: Parse diff — identify files, ranges, classify ToolKit-owned](#step-3-parse-diff--identify-files-ranges-classify-toolkit-owned)
- [Step 4: Write context and file snapshots](#step-4-write-context-and-file-snapshots)
- [Step 5: Run review agents in parallel](#step-5-run-review-agents-in-parallel)
- [Step 6: Collect and merge findings](#step-6-collect-and-merge-findings)
- [Step 7: Post inline review comments](#step-7-post-inline-review-comments)
- [Step 8: Print summary table](#step-8-print-summary-table)

---

## Step 1: Resolve repository

```bash
REPO=$(git remote get-url upstream 2>/dev/null \
  | sed 's|.*github.com[:/]\(.*\)\.git|\1|' \
  | sed 's|.*github.com[:/]\(.*\)|\1|') \
  || REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
echo "Repo: $REPO"
```

If `--repo` was passed as an argument, use that value instead. Store as `REPO`.

## Step 2: Fetch PR metadata and diff

// turbo
```bash
PR_NUMBER=<PR_NUMBER>
mkdir -p /tmp/toolkit-pr-review-v2-${PR_NUMBER}/files
gh pr view ${PR_NUMBER} --repo ${REPO} \
  --json number,title,body,headRefOid,baseRefName,headRefName \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json
gh pr diff ${PR_NUMBER} --repo ${REPO} \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/diff.patch
echo "HEAD SHA: $(jq -r .headRefOid /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json)"
```

## Step 3: Parse diff — identify files, ranges, classify ToolKit-owned

Parse `/tmp/toolkit-pr-review-v2-${PR_NUMBER}/diff.patch` to extract:

- **`rust_files`** — all added/modified `.rs` files (strip `a/`/`b/` prefix)
- **`changed_ranges`** — per-file list of `[start, end]` line ranges from diff hunks
- **`toolkit_owned_files`** — subset of `rust_files` where **any** of these signals is present:
  - nearest `Cargo.toml` declares `toolkit` dependency/feature, or crate name starts with `toolkit`
  - file path matches `libs/toolkit*/`, `gears/*/src/`
  - source imports `use toolkit_*` or references `OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, `GearLifecycle`
- **`has_test_code`** — `true` if any file contains `#[test]`, `#[tokio::test]`, or `#[cfg(test)]`

## Step 4: Write context and file snapshots

// turbo
```bash
# Write context.json (fill in values from Step 3)
cat > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json << 'EOF'
{
  "pr_number": <PR_NUMBER>,
  "repo": "<REPO>",
  "head_sha": "<HEAD_SHA>",
  "rust_files": [],
  "toolkit_owned_files": [],
  "has_test_code": false,
  "changed_ranges": {}
}
EOF

# For each rust file, fetch content at HEAD and write to files/ with / replaced by __
# Example:
# gh api repos/${REPO}/contents/path/to/file.rs?ref=<HEAD_SHA> -q .content \
#   | base64 -d > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/files/path__to__file.rs
```

## Step 5: Run review agents in parallel

Spawn all applicable agents as parallel background processes. Each reads its inputs from `/tmp/toolkit-pr-review-v2-${PR_NUMBER}/` and writes a JSON array to its output file.

Skip Agent E if `toolkit_owned_files` is empty. Skip Agent F if `has_test_code` is false.

// turbo
```bash
PR_NUMBER=<PR_NUMBER>
AGENTS_DIR="docs/pr-review/agents"

# Agent A — Error Handling & Panic
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-errors.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-errors.json 2>&1 &

# Agent B — Security
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-security.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-security.json 2>&1 &

# Agent C — Async, Concurrency, Performance
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-async.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-async.json 2>&1 &

# Agent D — Design, Types, Architecture
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-design.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-design.json 2>&1 &

# Agent E — ToolKit Framework Compliance (gated)
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-toolkit.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-toolkit.json 2>&1 &

# Agent F — Test Quality (gated)
claude -p "$(sed '/^---$/,/^---$/d;1{/^---/d}' ${AGENTS_DIR}/toolkit-pr-review-v2-tests.md)

The PR number is ${PR_NUMBER}. Read /tmp/toolkit-pr-review-v2-${PR_NUMBER}/context.json first." \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/out-tests.json 2>&1 &

wait
echo "All agents finished."
```

**Fallback (no `claude` CLI)**: If `claude` CLI is not available, read each agent file from
`docs/pr-review/agents/` directly and execute its checklist yourself, processing agents A–F in
sequence. The agent files are self-contained — follow the Role, Check IDs, Scope Rules, and
Output Contract sections for each.

## Step 6: Collect and merge findings

```bash
export PR_NUMBER=<PR_NUMBER>
python3 - << 'PY'
import json, glob, sys, os

PR_NUMBER = os.environ["PR_NUMBER"]
combined = []
order = ["errors", "security", "async", "design", "toolkit", "tests"]
for name in order:
    path = f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/out-{name}.json"
    try:
        text = open(path).read()
        start, end = text.index("["), text.rindex("]") + 1
        combined.extend(json.loads(text[start:end]))
    except Exception as e:
        print(f"WARNING: {name}: {e}", file=sys.stderr)

# Load changed_ranges for line validation
ctx = json.load(open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/context.json"))
ranges = ctx.get("changed_ranges", {})

def in_range(file, line):
    for start, end in ranges.get(file, []):
        if start <= line <= end:
            return True
    return False

# Deduplicate by (file, line, id), validate line in diff
seen, filtered = set(), []
for f in combined:
    key = (f["file"], f["line"], f["id"])
    if key in seen:
        continue
    if not in_range(f["file"], f["line"]):
        continue
    seen.add(key)
    filtered.append(f)

# Sort CRITICAL > HIGH > MEDIUM > LOW
order_map = {"CRITICAL": 0, "HIGH": 1, "MEDIUM": 2, "LOW": 3}
filtered.sort(key=lambda x: order_map.get(x["severity"], 4))

# Cap at 30
if len(filtered) > 30:
    print(f"Capped at 30; dropped {len(filtered)-30} findings.", file=sys.stderr)
    filtered = filtered[:30]

json.dump(filtered, open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/findings.json", "w"), indent=2)
print(f"Total findings: {len(filtered)}")
PY
```

## Step 7: Post inline review comments

```bash
export PR_NUMBER=<PR_NUMBER>
export HEAD_SHA=$(jq -r .headRefOid /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json)

python3 - << 'PY'
import json, os

PR_NUMBER = os.environ["PR_NUMBER"]
HEAD_SHA = os.environ["HEAD_SHA"]
findings = json.load(open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/findings.json"))
comments = [
    {
        "path": f["file"],
        "line": f["line"],
        "side": "RIGHT",
        "body": f"**{f['severity']}**\n\n{f['issue']}\n\n{f['fix']}"
    }
    for f in findings
]
payload = {
    "commit_id": HEAD_SHA,
    "event": "COMMENT",
    "body": "",
    "comments": comments if comments else []
}
json.dump(payload, open("/tmp/review-payload.json", "w"))
print(f"Prepared {len(comments)} comments")
PY

if [ $(jq '.comments | length' /tmp/review-payload.json) -eq 0 ]; then
  gh api repos/${REPO}/pulls/${PR_NUMBER}/reviews \
    --method POST \
    -f commit_id="${HEAD_SHA}" \
    -f event="COMMENT" \
    -f body="No issues found."
else
  gh api repos/${REPO}/pulls/${PR_NUMBER}/reviews \
    --method POST \
    --input /tmp/review-payload.json
fi
```

## Step 8: Print summary table

```bash
export PR_NUMBER=<PR_NUMBER>
python3 - << 'PY'
import json, os
PR_NUMBER = os.environ["PR_NUMBER"]
findings = json.load(open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/findings.json"))
print(f"\n## Rust PR Review: #{PR_NUMBER}\n")
print("| # | ID | Sev | Location | Issue | Fix |")
print("|---|----|-----|----------|-------|-----|")
for i, f in enumerate(findings, 1):
    loc = f"{f['file'].split('/')[-1]}:{f['line']}"
    print(f"| {i} | {f['id']} | {f['severity']} | {loc} | {f['issue'][:60]} | {f['fix'][:60]} |")
print(f"\nPosted {len(findings)} inline comments on PR #{PR_NUMBER}.")
PY
```
