---
description: Review a GitHub PR for Rust + ToolKit compliance. Runs up to 6 specialized agents in parallel (some are skipped based on repository conditions), then posts inline comments.
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
  --json number,title,body,headRefOid,baseRefOid,baseRefName,headRefName \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json
gh pr diff ${PR_NUMBER} --repo ${REPO} \
  > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/diff.patch
echo "HEAD SHA: $(jq -r .headRefOid /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json)"
```

## Step 3: Parse diff — identify files, ranges, classify ToolKit-owned

Parse `/tmp/toolkit-pr-review-v2-${PR_NUMBER}/diff.patch` to extract:

- **`rust_files`** — all added, modified, or deleted `.rs` files (strip `a/`/`b/` prefix)
- **`deleted_files`** — subset of `rust_files` whose diff hunk targets `/dev/null` on the new side (deleted entirely). These have no RIGHT-side line at all — no `changed_ranges` or `deletion_anchors` entry is computed for them; see Step 4 (base-side snapshot) and Step 7 (file-level comment).
- **`changed_ranges`** — per-file list of `[start, end]` line ranges from diff hunks (added lines only; empty for files in `deleted_files`)
- **`deletion_anchors`** — per-file list of anchor lines for pure-deletion hunks in a file that still exists (hunks that remove lines with no added replacement). The anchor is the hunk's `newStart` from its `@@ -oldStart,oldCount +newStart,newCount @@` header (clamp to line `1` if `0`, or the file's last line if past EOF). Needed so a finding about a partially deleted test (e.g. `TEST-QUALITY-9`) still has a valid line to anchor on.
- **`toolkit_owned_files`** — subset of `rust_files` where **any** of these signals is present:
  - nearest `Cargo.toml` declares `toolkit` dependency/feature, or crate name starts with `toolkit`
  - file path matches `libs/toolkit*/`, `gears/*/src/`
  - source imports `use toolkit_*` or references `OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, `GearLifecycle`
- **`has_test_code`** — `true` if `diff.patch` contains `#[test]`, `#[tokio::test]`, or `#[cfg(test)]` on **any** line (added or removed) — scanning the raw diff, not just head-side file content, so a wholesale-deleted test file still sets this `true`

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
  "deleted_files": [],
  "toolkit_owned_files": [],
  "has_test_code": false,
  "changed_ranges": {},
  "deletion_anchors": {}
}
EOF

# For each rust file NOT in deleted_files, fetch content at HEAD and write to files/ with / replaced by __
# Example:
# gh api repos/${REPO}/contents/path/to/file.rs?ref=<HEAD_SHA> -q .content \
#   | base64 -d > /tmp/toolkit-pr-review-v2-${PR_NUMBER}/files/path__to__file.rs

# For each file IN deleted_files, fetch its pre-deletion content at the base commit instead
# (BASE_SHA = baseRefOid from meta.json) — the file no longer exists at HEAD_SHA:
# BASE_SHA=$(jq -r .baseRefOid /tmp/toolkit-pr-review-v2-${PR_NUMBER}/meta.json)
# gh api repos/${REPO}/contents/path/to/file.rs?ref=${BASE_SHA} -q .content \
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

# Load changed_ranges / deletion_anchors / deleted_files for line validation
ctx = json.load(open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/context.json"))
ranges = ctx.get("changed_ranges", {})
deletion_anchors = ctx.get("deletion_anchors", {})
deleted_files = set(ctx.get("deleted_files", []))

def in_range(file, line):
    for start, end in ranges.get(file, []):
        if start <= line <= end:
            return True
    # TEST-QUALITY-9 findings on partially deleted content may anchor here instead
    return line in deletion_anchors.get(file, [])

# Deduplicate by (file, line, id), validate line in diff.
# A finding on a fully deleted file has no line at all (it posts as a
# file-level comment in Step 7) and skips the line check entirely.
seen, filtered = set(), []
for f in combined:
    key = (f["file"], f.get("line"), f["id"])
    if key in seen:
        continue
    if f["file"] not in deleted_files and not in_range(f["file"], f.get("line")):
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
ctx = json.load(open(f"/tmp/toolkit-pr-review-v2-{PR_NUMBER}/context.json"))
deleted_files = set(ctx.get("deleted_files", []))

# A deleted file has no RIGHT-side line, so its findings can't go in the
# batch review's `comments` array (which requires line+side). Post those
# individually via the single-comment endpoint with subject_type="file".
line_findings = [f for f in findings if f["file"] not in deleted_files]
file_findings = [f for f in findings if f["file"] in deleted_files]

comments = [
    {
        "path": f["file"],
        "line": f["line"],
        "side": "RIGHT",
        "body": f"**{f['severity']}**\n\n{f['issue']}\n\n{f['fix']}"
    }
    for f in line_findings
]
payload = {
    "commit_id": HEAD_SHA,
    "event": "COMMENT",
    "body": "",
    "comments": comments
}
json.dump(payload, open("/tmp/review-payload.json", "w"))
json.dump(file_findings, open("/tmp/file-level-findings.json", "w"))
print(f"Prepared {len(comments)} line comments, {len(file_findings)} file-level comments")
PY

LINE_COUNT=$(jq '.comments | length' /tmp/review-payload.json)
FILE_COUNT=$(jq 'length' /tmp/file-level-findings.json)

if [ "$LINE_COUNT" -eq 0 ] && [ "$FILE_COUNT" -eq 0 ]; then
  gh api repos/${REPO}/pulls/${PR_NUMBER}/reviews \
    --method POST \
    -f commit_id="${HEAD_SHA}" \
    -f event="COMMENT" \
    -f body="No issues found."
else
  if [ "$LINE_COUNT" -gt 0 ]; then
    gh api repos/${REPO}/pulls/${PR_NUMBER}/reviews \
      --method POST \
      --input /tmp/review-payload.json
  fi
  # File-level comments: a deleted file has no RIGHT-side line to anchor on.
  jq -c '.[]' /tmp/file-level-findings.json | while read -r finding; do
    path=$(echo "$finding" | jq -r '.file')
    body=$(echo "$finding" | jq -r '"**" + .severity + "**\n\n" + .issue + "\n\n" + .fix')
    gh api repos/${REPO}/pulls/${PR_NUMBER}/comments \
      --method POST \
      -f commit_id="${HEAD_SHA}" \
      -f path="${path}" \
      -f subject_type="file" \
      -f body="${body}"
  done
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
    name = f['file'].split('/')[-1]
    loc = f"{name}:{f['line']}" if f.get('line') else name
    print(f"| {i} | {f['id']} | {f['severity']} | {loc} | {f['issue'][:60]} | {f['fix'][:60]} |")
print(f"\nPosted {len(findings)} inline comments on PR #{PR_NUMBER}.")
PY
```
