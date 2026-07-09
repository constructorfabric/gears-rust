---
name: toolkit-pr-review-v2
description: "Review Rust PRs against idiomatic Rust guidelines and ToolKit framework rules, post inline comments on GitHub"
user-invocable: true
allowed-tools: Bash, Read, Glob, Grep, Write, Agent
---

# Rust PR Review

Review a GitHub pull request for Rust code quality and ToolKit framework compliance.
Posts findings as inline review comments directly on the PR.

**Usage**: `/toolkit-pr-review-v2 <PR_NUMBER>`

---

## Table of Contents

- [Inputs](#inputs)
- [Resolving the target repository](#resolving-the-target-repository)
- [Review guidelines](#review-guidelines)
- [Coding guidelines reference](#coding-guidelines-reference)
- [Steps](#steps)
- [Comment formatting rules](#comment-formatting-rules)
- [What NOT to do](#what-not-to-do)

---

## Inputs

- `<PR_NUMBER>` — required, the GitHub PR number (e.g. `123`)
- `--repo <owner/repo>` — optional, the GitHub repository (e.g. `constructorfabric/gears-rust`)

## Resolving the target repository

Before fetching PR data, determine which repository to use:

1. If `--repo` was provided in the arguments, use it.
2. Otherwise, check if an `upstream` remote exists: `git remote get-url upstream 2>/dev/null`. If it returns a URL, extract `owner/repo` from it.
3. Otherwise, fall back to the current repo via `gh repo view --json nameWithOwner -q .nameWithOwner`.

Store the result as `REPO` and pass `--repo $REPO` to all `gh pr` commands, and use it in API paths as `repos/$REPO/pulls/...`.

## Review guidelines

Apply **Rust idioms and engineering** (`docs/pr-review/toolkit-rust-review.md`) to every `.rs` file in the diff.

Apply **ToolKit framework compliance** (`docs/pr-review/toolkit-framework-compliance-review.md`) **only** to `.rs` files that belong to ToolKit-owned code. A file is ToolKit-owned when **any** of these signals is present:

1. **Cargo.toml signals** — the nearest `Cargo.toml` (same crate or workspace member) declares a `toolkit` dependency/feature, or the crate name starts with `toolkit`.
2. **Path heuristics** — the file lives under a path that matches ToolKit gear conventions (e.g. `gears/*/src/`, `crates/toolkit-*/`, or similar namespace).
3. **Source-level symbols** — the file imports from ToolKit crates (`use toolkit_*`, `use crate::` inside a toolkit crate) or references ToolKit-specific types/traits such as `OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, or `GearLifecycle`.

If none of these signals are detected, skip the framework compliance checklist for that file and apply only the general Rust idioms checklist.

Apply **Rust unit test quality review** (`docs/pr-review/toolkit-tests-quality-review.md`) to every changed Rust test you can identify in the diff, including:
- `#[test]` functions
- async tests such as `#[tokio::test]`
- test modules such as `#[cfg(test)] mod tests`
- assertions added or modified inside production files or dedicated test files
- integration tests under `tests/`
- test-only helper code when it materially affects test validity

For non-Rust files in the diff (TOML, YAML, migrations, etc.) — apply only general correctness checks, do not force Rust-specific rules.

## Coding guidelines reference

When reviewing, also consult:
- `guidelines/DNA/languages/RUST.md` — project Rust conventions
- `guidelines/SECURITY.md` — security requirements

---

## Steps

### Step 1: Fetch PR metadata and diff

```bash
gh pr view <PR_NUMBER> --repo $REPO --json number,title,body,headRefOid,baseRefName,headRefName
gh pr diff <PR_NUMBER> --repo $REPO
```

Save the diff output for analysis. Extract the HEAD commit SHA — you need it for posting comments.

### Step 2: Identify Rust files in diff

Parse the diff to find all `.rs` files that were added or modified.
For each file, note the changed line ranges (added lines only — you can only comment on lines present in the diff).

### Step 3: Read review guidelines and classify files

Read `docs/pr-review/toolkit-rust-review.md` (always needed).

For each `.rs` file from Step 2, determine whether it is ToolKit-owned code:
- Check the nearest `Cargo.toml` for toolkit dependencies/features or a `toolkit-` crate name.
- Check whether the file path matches ToolKit gear conventions (`gears/*/src/`, `crates/toolkit-*/`).
- Scan the file for ToolKit imports (`use toolkit_*`) or ToolKit types (`OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, `GearLifecycle`).

If **any** file is classified as ToolKit-owned, also read `docs/pr-review/toolkit-framework-compliance-review.md`.

### Step 4a: Prepare shared context in /tmp

Create the working directory:
```bash
mkdir -p /tmp/toolkit-pr-review-v2-$PR_NUMBER/files
```

Write `/tmp/toolkit-pr-review-v2-$PR_NUMBER/diff.patch` — the raw output of `gh pr diff` from Step 1.

Write `/tmp/toolkit-pr-review-v2-$PR_NUMBER/context.json` with the PR metadata, file lists, and changed line ranges:
```json
{
  "pr_number": <PR_NUMBER>,
  "repo": "<REPO>",
  "head_sha": "<HEAD_SHA>",
  "rust_files": [<list of .rs files from Step 2>],
  "toolkit_owned_files": [<files classified as ToolKit-owned in Step 3>],
  "has_test_code": <boolean: true if any rust_files contains #[test], #[tokio::test], #[cfg(test)], or lives under tests/>],
  "changed_ranges": {
    "<filepath>": [[<start_line>, <end_line>], ...]
  }
}
```

The `changed_ranges` dict maps each file to its list of changed line ranges (derived from parsing diff hunks in Step 2). Agents use this to validate that line numbers are within the diff.

For each file in `rust_files`, read the file from the repo (at the PR head commit) and write its full content to:
```text
/tmp/toolkit-pr-review-v2-$PR_NUMBER/files/<escaped-path>
```
where `<escaped-path>` replaces `/` with `__` (e.g., `gears/foo/src/service.rs` → `gears__foo__src__service.rs`).

### Step 4b: Spawn parallel sub-agents

Spawn all applicable sub-agents in parallel using the `Agent` tool. Pass to each agent:
- PR number and repo from context
- Paths to context.json, diff.patch, and files/ directory in /tmp

Spawn these agents (skip Agent E if toolkit_owned_files is empty; skip Agent F if has_test_code is false):

**Agent A — Error Handling & Panic** (`toolkit-pr-review-v2-errors`):
Check IDs: RUST-ERR-001, RUST-PANIC-001, RUST-NO-001, RUST-NO-002, RUST-NO-003, TOOLKIT-ERR-001, TOOLKIT-ERR-002

**Agent B — Security** (`toolkit-pr-review-v2-security`):
Check IDs: RUST-SEC-001, RUST-NO-006, TOOLKIT-SEC-001, TOOLKIT-SEC-002

**Agent C — Async, Concurrency, Performance** (`toolkit-pr-review-v2-async`):
Check IDs: RUST-ASYNC-001, RUST-CONC-001, RUST-PERF-001, RUST-NO-004, RUST-NO-005, TOOLKIT-LIFE-001

**Agent D — Design, Types, Architecture** (`toolkit-pr-review-v2-design`):
Check IDs: RUST-API-001, RUST-TYPE-001, RUST-OWN-001, RUST-DATA-001, RUST-OBS-001, RUST-MOD-001, RUST-NO-007

**Agent E — ToolKit Framework Compliance** (`toolkit-pr-review-v2-toolkit`):
Check IDs: TOOLKIT-CORE-001, TOOLKIT-CORE-002, TOOLKIT-CORE-003, TOOLKIT-REST-001, TOOLKIT-REST-002, TOOLKIT-REST-003, TOOLKIT-DB-001, TOOLKIT-DB-002, TOOLKIT-CLIENT-001, TOOLKIT-CLIENT-002, TOOLKIT-ODATA-001, TOOLKIT-OOP-001
(Gated: skip if toolkit_owned_files is empty)

**Agent F — Test Quality** (`toolkit-pr-review-v2-tests`):
Check IDs: RUST-TEST-001, TEST-QUALITY-1 through TEST-QUALITY-8
(Gated: skip if has_test_code is false)

Each agent returns a JSON array of findings. See `docs/pr-review/agents/toolkit-pr-review-v2-<name>.md` for detailed prompt structure.

### Step 4c: Collect and merge findings

Wait for all Agent calls to complete. For each result:

1. Extract JSON array from output: find the first `[` and last `]`, parse that substring as JSON.
2. If not valid JSON, log a warning to terminal and treat as `[]`.
3. Append valid findings to a combined list in agent order: A → B → C → D → E → F.

Deduplicate: drop any finding where `(file, line, id)` duplicates an earlier finding.

Apply filter rules:
- Drop findings where `line` is not in `changed_ranges[file]` for that file.
- Drop style-only issues that rustfmt or clippy should catch.
- Drop speculative or hypothetical findings (containing phrases like "might", "could consider", "may want to").

Sort by severity: CRITICAL → HIGH → MEDIUM → LOW.

Cap at 30 findings: if the list exceeds 30, drop from the tail (lowest severity) and log the count dropped to terminal (e.g., "Capped at 30 findings; dropped 5 LOW and 3 MEDIUM findings").

This merged, filtered, sorted, capped list becomes the input to Step 5.

### Step 5: Post inline review comments on GitHub

Use `gh api` to create a pull request review with inline comments.

Build the review payload:

IMPORTANT: The `gh api` `-f` array syntax is limited. For multiple comments, build a JSON file and POST it.

The review `body` MUST be empty string — no summary in the review itself. The summary goes to the terminal only (Step 6).

```bash
cat > /tmp/review-payload.json << 'REVIEW_EOF'
{
  "commit_id": "<HEAD_SHA>",
  "event": "COMMENT",
  "body": "",
  "comments": [
    {
      "path": "gears/foo/src/domain/service.rs",
      "line": 42,
      "side": "RIGHT",
      "body": "**HIGH**\n\nError context discarded by `map_err(|_| ...)`.\n\nPreserve the source error — wrap with `.context()` or map to a domain error that keeps the cause."
    }
  ]
}
REVIEW_EOF

gh api repos/$REPO/pulls/<PR_NUMBER>/reviews \
  --method POST \
  --input /tmp/review-payload.json
```

If there are zero findings, skip the payload above and post a single review whose
`body` carries the message instead of an inline comment:

```bash
gh api repos/$REPO/pulls/<PR_NUMBER>/reviews \
  --method POST \
  -f commit_id="<HEAD_SHA>" \
  -f event="COMMENT" \
  -f body="No issues found."
```

### Step 6: Print summary

After posting, print a compact summary table to the terminal:

```text
## Rust PR Review: #<PR_NUMBER>

| # | ID | Sev | Location | Issue | Fix |
|---|----|-----|----------|-------|-----|
| 1 | RUST-ERR-001 | HIGH | service.rs:42 | Error context lost | Preserve source error |
| 2 | TOOLKIT-SEC-001 | CRIT | handler.rs:18 | Raw DB connection | Use SecureConn |

Posted <N> inline comments on PR #<PR_NUMBER>.
```

---

## Comment formatting rules

Each inline comment MUST follow this format:

```text
**<SEVERITY>**

<One-sentence issue description.>

<One-sentence why it matters.>

<Concrete fix — what to change, not a vague suggestion.>
```

Where `<SEVERITY>` is one of: `CRITICAL`, `HIGH`, `MEDIUM`, `LOW`.

Do NOT include checklist IDs (e.g. RUST-ERR-001, TOOLKIT-SEC-001) in inline comments. IDs appear only in the terminal summary table (Step 6).

Rules:
- Engineering English. No filler, no praise, no hedging.
- No "consider", "you might want to", "it would be nice if". State what is wrong and what to do.
- One issue per comment. If a line has two problems, post two comments.
- Line number must point to an added/modified line that exists in the diff. Do not comment on unchanged lines.
- If you cannot determine the exact line, do not guess — skip that finding.

---

## What NOT to do

- Do not approve or request changes — use `event: "COMMENT"` only
- Do not post comments on lines outside the diff
- Do not post generic praise or "LGTM" if there are no issues
- Do not invent issues without evidence in the code
- Do not complain about formatting that rustfmt handles
- Do not suggest speculative abstractions or premature generalization
- Do not post more than 30 comments per review (prioritize by severity)
- If there are zero findings, post a single review comment: "No issues found."
