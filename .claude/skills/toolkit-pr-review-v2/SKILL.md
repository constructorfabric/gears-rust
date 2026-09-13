---
name: toolkit-pr-review-v2
description: "Review Rust changes against idiomatic Rust guidelines and ToolKit framework rules. PR mode posts inline comments on GitHub; local mode reviews a branch against main and writes a markdown report."
user-invocable: true
allowed-tools: Bash, Read, Glob, Grep, Write, Agent
---

# Rust PR Review

Review Rust code changes for quality and ToolKit framework compliance.

**Usage**:
- `/toolkit-pr-review-v2 <PR_NUMBER>` — review a GitHub PR, post findings as inline review comments
- `/toolkit-pr-review-v2 local [<branch-name>]` — review a local branch against `main`, write findings to a markdown file

---

## Table of Contents

- [Inputs](#inputs)
- [Modes](#modes)
- [Resolving the target repository (PR mode)](#resolving-the-target-repository-pr-mode)
- [Review guidelines](#review-guidelines)
- [Coding guidelines reference](#coding-guidelines-reference)
- [Steps](#steps)
- [Comment formatting rules](#comment-formatting-rules)
- [What NOT to do](#what-not-to-do)

---

## Inputs

- `<PR_NUMBER>` — the GitHub PR number (e.g. `123`). Selects **PR mode**.
- `local [<branch-name>]` — selects **local mode**. `<branch-name>` is optional; when omitted the current branch is used.
- `--repo <owner/repo>` — optional, PR mode only (e.g. `constructorfabric/gears-rust`)

## Modes

Resolve the mode from the first argument **before** doing anything else.

### PR mode

First argument is a number. Review target is the GitHub PR; findings are posted as inline review comments.

Set:
- `MODE=pr`
- `REVIEW_ID=<PR_NUMBER>`

### Local mode

First argument is the literal word `local`. Review target is the local branch diffed against `main`; findings are written to a markdown file — **nothing is posted to GitHub**.

Resolve:

```bash
BASE_BRANCH=main
BRANCH_NAME=<branch-name argument, or `git symbolic-ref --quiet --short HEAD` if omitted>
REPO_ROOT=$(git rev-parse --show-toplevel)
HEAD_SHA=$(git rev-parse "$BRANCH_NAME")
```

If no branch argument was given and `git symbolic-ref` fails, HEAD is detached — print the error and stop.
Verify the branch exists (`git rev-parse --verify "$BRANCH_NAME"`) and that `main` exists locally
(`git rev-parse --verify main`; if it does not, fall back to `origin/main` and use that as `BASE_BRANCH`).
If neither resolves, print the error and stop.

Set:
- `MODE=local`
- `BRANCH_SLUG` = `BRANCH_NAME` with `/` replaced by `-`
- `REVIEW_ID=local-$BRANCH_SLUG`

## Resolving the target repository (PR mode)

PR mode only — skip this in local mode. Before fetching PR data, determine which repository to use:

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

Apply **RUST-DEP-001** (dependency and advisory manifest hygiene) only to the manifest and config
files collected in Step 2: `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.cargo/audit.toml`,
`.cargo/config.toml`, `clippy.toml`, `rust-toolchain.toml`. When the diff touches none of them,
the check is skipped.

## Coding guidelines reference

When reviewing, also consult:
- `docs/pr-review/comment-style.md` — comment voice. The only authority on how findings are worded
- `guidelines/SECURITY.md` — security requirements

---

## Steps

### Step 1: Fetch metadata and diff

**PR mode:**

```bash
gh pr view <PR_NUMBER> --repo $REPO --json number,title,body,headRefOid,baseRefName,headRefName
gh pr diff <PR_NUMBER> --repo $REPO
```

Extract the HEAD commit SHA — you need it for posting comments.

**Local mode:**

```bash
git log --oneline "$BASE_BRANCH..$BRANCH_NAME"
git diff --stat "$BASE_BRANCH...$BRANCH_NAME"
git diff "$BASE_BRANCH...$BRANCH_NAME"
```

Use three-dot diff (merge-base) so the review sees only what the branch adds, matching PR semantics.
Record the commit count and the `--stat` totals — they go into the report header.

Save the diff output for analysis.

### Step 2: Identify Rust files in diff

Parse the diff to find all `.rs` files that were added, modified, or deleted.
For added/modified files, note the changed line ranges (added lines only — you can only comment on lines present in the diff).

Also record, per file, a **deletion anchor** for every hunk that removes lines with no added replacement in that hunk (a "pure deletion" hunk in a file that still exists at the review head) — needed so a finding about a partially deleted test (e.g. `TEST-QUALITY-9`) still has a valid line to anchor on. The anchor is the hunk's `newStart` value from its `@@ -oldStart,oldCount +newStart,newCount @@` header — the nearest surviving line in the new file at the deletion point. If `newStart` is `0`, use line `1`; if it falls past the end of the file, use the file's last line.

For a reviewed file **deleted entirely** (present in the base, absent at the review head — its diff hunk targets `/dev/null`), include its path in its own list (`rust_files` or `manifest_files`) **and** track it in `deleted_files`. A deleted file has **no** RIGHT-side line at all, so do not compute `changed_ranges` or a `deletion_anchors` entry for it — see Step 4a for how its content is snapshotted and Step 5 for how findings on it are posted.

This covers manifest and advisory config as well as `.rs`. Deleting `deny.toml` or
`.cargo/audit.toml` outright removes a supply-chain control, which is exactly what
`RUST-DEP-001` exists to catch; if such a file were snapshotted at the head (where it no longer
exists) and filtered against its empty `changed_ranges`, every finding on it would be silently
dropped.

Also collect changed **manifest and lint/advisory config** files into a separate `manifest_files`
list: `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.cargo/audit.toml`, `.cargo/config.toml`,
`clippy.toml`, `rust-toolchain.toml` (at any depth in the repo). Compute `changed_ranges` for them
exactly as for `.rs` files, so findings anchor on real diff lines. These files are **not** added to
`rust_files`; they exist only to feed `RUST-DEP-001`. If the diff touches none of them, leave the
list empty.

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
mkdir -p /tmp/toolkit-pr-review-v2-$REVIEW_ID/files
```

Write `/tmp/toolkit-pr-review-v2-$REVIEW_ID/diff.patch` — the raw diff from Step 1.

Write `/tmp/toolkit-pr-review-v2-$REVIEW_ID/context.json` with the metadata, file lists, and changed line ranges:
```json
{
  "mode": "pr" | "local",
  "pr_number": <PR_NUMBER, or null in local mode>,
  "repo": "<REPO, or null in local mode>",
  "branch": "<BRANCH_NAME, or null in PR mode>",
  "base_branch": "<BASE_BRANCH, or null in PR mode>",
  "head_sha": "<HEAD_SHA>",
  "rust_files": [<list of .rs files from Step 2, including deleted files>],
  "deleted_files": [<any reviewed file deleted entirely, from rust_files or manifest_files — no RIGHT-side content>],
  "manifest_files": [<changed manifest/lint/advisory config files from Step 2; [] if none>],
  "toolkit_owned_files": [<files classified as ToolKit-owned in Step 3>],
  "has_test_code": <boolean: true if any rust_files contains #[test], #[tokio::test], #[cfg(test)], or lives under tests/ — for deleted_files, check their base-side content instead of the (nonexistent) head content>,
  "changed_ranges": {
    "<filepath>": [[<start_line>, <end_line>], ...]
  },
  "deletion_anchors": {
    "<filepath>": [<line>, ...]
  }
}
```

The `changed_ranges` dict maps each file to its list of changed line ranges (derived from parsing diff hunks in Step 2). Agents use this to validate that line numbers are within the diff.

The `deletion_anchors` dict maps each file to the pure-deletion-hunk anchor lines from Step 2. It is a secondary, narrower set of valid line targets — used only for findings about content partially removed from a file that still exists (no added line exists to anchor on), such as `TEST-QUALITY-9`. Omit a file's entry (or use `[]`) when it has no pure-deletion hunks. **Files listed in `deleted_files` never get an entry here** — they have no RIGHT-side line to anchor on at all; see Step 5 for how findings on them are posted instead.

The `manifest_files` list gates `RUST-DEP-001`. Agent B skips that check when the list is empty.

For each file in `rust_files` or `manifest_files` that is **not** in `deleted_files`, read the file
at the review head (the PR head commit in PR mode, `$BRANCH_NAME` in local mode) and write its full
content to:
```text
/tmp/toolkit-pr-review-v2-$REVIEW_ID/files/<escaped-path>
```
For each file **in** `deleted_files`, read it instead at the base commit (the PR's base SHA in PR mode, the merge-base with `$BASE_BRANCH` in local mode) and write that pre-deletion content to the same path — this lets Agent F see which test was removed, and Agent B see which policy a deleted `deny.toml` or `.cargo/audit.toml` used to enforce.

**Advisory counterpart.** `RUST-DEP-001` checks that `.cargo/audit.toml` and `deny.toml` do not
drift apart, which needs both files even when the diff touches only one. So when either is in
`manifest_files`, also snapshot the other into `files/` at the review head (or at the base commit
if it is in `deleted_files`), even though it is unchanged. Do **not** add that counterpart to
`manifest_files` and do **not** give it a `changed_ranges` entry: it is read-only context, and a
finding must still anchor on the file the PR actually changed, or Step 4c drops it. If the
counterpart does not exist in the repo at all, skip it — its absence means there is nothing to
drift from, not that the check failed.

`<escaped-path>` replaces `/` with `__` (e.g., `gears/foo/src/service.rs` → `gears__foo__src__service.rs`).

### Step 4b: Spawn parallel sub-agents

Spawn all applicable sub-agents in parallel using the `Agent` tool. Pass to each agent:
- The review target identity from context (PR number + repo, or branch + base branch)
- Paths to context.json, diff.patch, and files/ directory in /tmp

Spawn these agents (skip Agent E if toolkit_owned_files is empty; skip Agent F if has_test_code is false):

**Agent A — Error Handling & Panic** (`toolkit-pr-review-v2-errors`):
Check IDs: RUST-ERR-001, RUST-PANIC-001, RUST-NO-001, RUST-NO-002, RUST-NO-003, TOOLKIT-ERR-001, TOOLKIT-ERR-002

**Agent B — Security** (`toolkit-pr-review-v2-security`):
Check IDs: RUST-SEC-001, RUST-SEC-002, RUST-NO-006, RUST-DEP-001, TOOLKIT-SEC-001, TOOLKIT-SEC-002
(RUST-DEP-001 is skipped when `manifest_files` is empty; RUST-NO-006 applies only to a crate that
opts out of the workspace `unsafe_code = "forbid"`)

**Agent C — Async, Concurrency, Performance** (`toolkit-pr-review-v2-async`):
Check IDs: RUST-ASYNC-001, RUST-CONC-001, RUST-PERF-001, RUST-NO-004, RUST-NO-005, TOOLKIT-LIFE-001

**Agent D — Design, Types, Architecture** (`toolkit-pr-review-v2-design`):
Check IDs: RUST-API-001, RUST-TYPE-001, RUST-OWN-001, RUST-DATA-001, RUST-OBS-001, RUST-OBS-002, RUST-MOD-001, RUST-LINT-001, RUST-NO-007

**Agent E — ToolKit Framework Compliance** (`toolkit-pr-review-v2-toolkit`):
Check IDs: TOOLKIT-CORE-001, TOOLKIT-CORE-002, TOOLKIT-CORE-003, TOOLKIT-REST-001, TOOLKIT-REST-002, TOOLKIT-REST-003, TOOLKIT-DB-001, TOOLKIT-DB-002, TOOLKIT-CLIENT-001, TOOLKIT-CLIENT-002, TOOLKIT-ODATA-001, TOOLKIT-OOP-001
(Gated: skip if toolkit_owned_files is empty)

**Agent F — Test Quality** (`toolkit-pr-review-v2-tests`):
Check IDs: RUST-TEST-001, TEST-QUALITY-1 through TEST-QUALITY-10
(Gated: skip if has_test_code is false)

Each agent returns a JSON array of findings. See `docs/pr-review/agents/toolkit-pr-review-v2-<name>.md` for detailed prompt structure.

### Step 4c: Collect and merge findings

Wait for all Agent calls to complete. For each result:

1. Extract JSON array from output: find the first `[` and last `]`, parse that substring as JSON.
2. If not valid JSON, log a warning to terminal and treat as `[]`.
3. Append valid findings to a combined list in agent order: A → B → C → D → E → F.

Deduplicate: drop any finding where `(file, line, id)` duplicates an earlier finding.

Apply filter rules:
- For a finding whose `file` is in `deleted_files`: keep it regardless of `line` (it has none — it posts as a file-level comment in Step 5).
- For every other finding: drop it if `line` is not in `changed_ranges[file]` and not in `deletion_anchors[file]` for that file.
- Drop style-only issues that rustfmt or clippy should catch. This includes anything the checklist
  marks `Enforcement: clippy ... (deny)` — `Cargo.toml` `[workspace.lints]` already fails the build
  on it, so posting it costs a slot a real finding needs.
- Drop findings whose `issue` is speculative rather than evidenced: a hypothetical problem, not an
  observed one. Judge this on the `issue` field only. Do **not** filter on `comment` wording —
  hedged phrasing there ("this should probably be X", "looks like this can panic if...") is
  intentional when the finding itself is uncertain, and is required by
  `docs/pr-review/comment-style.md`.

Sort by severity: CRITICAL → HIGH → MEDIUM → LOW.

Cap at 30 findings: if the list exceeds 30, drop from the tail (lowest severity) and log the count dropped to terminal (e.g., "Capped at 30 findings; dropped 5 LOW and 3 MEDIUM findings").

This merged, filtered, sorted, capped list becomes the input to Step 5.

### Step 5 (PR mode): Post inline review comments on GitHub

Local mode skips this step entirely — go to Step 5L.

Split the merged findings into two groups:
- **Line-anchored findings** — `file` not in `deleted_files`. Post together in one review (below).
- **File-level findings** — `file` in `deleted_files`. A deleted file has no RIGHT-side line, so these cannot go in the batch review's `comments` array (which requires `line`+`side`). Post each individually via the single-comment endpoint with `subject_type: "file"` and no `line`/`side`:

```bash
gh api repos/$REPO/pulls/<PR_NUMBER>/comments \
  --method POST \
  -f commit_id="<HEAD_SHA>" \
  -f path="gears/foo/src/tests.rs" \
  -f subject_type="file" \
  -f body=$'**HIGH**\n\n`send_dead_letter_on_timeout` is gone and nothing links a follow-up. If it was failing, we lose both the coverage and the signal.'
```

Note the `$'...'` quoting: inside plain double quotes bash keeps `\n` as two literal characters and
`gh` posts them verbatim, so the comment renders with visible `\n`. ANSI-C quoting turns them into
real newlines, and backticks stay literal inside it. The JSON-payload path below does not need this
(there `\n` is a JSON escape that the parser turns into a newline).

Post these after the batch review below. If there are zero findings in both groups, skip straight to the "zero findings" review call.

Use `gh api` to create a pull request review with the line-anchored inline comments.

Build the review payload:

IMPORTANT: The `gh api` `-f` array syntax is limited. For multiple comments, build a JSON file and POST it.

The review `body` MUST be empty string — no summary in the review itself. The summary goes to the terminal only (Step 6).

Each comment `body` is built from the finding's `comment` field, with the severity header added
only for the two top severities:

```text
CRITICAL or HIGH  ->  "**<SEVERITY>**\n\n<comment>"
MEDIUM or LOW     ->  "<comment>"
```

Never post `issue` or `fix` as comment text — those two fields exist for the summary table and the
local-mode report. The second example below shows a MEDIUM comment, with no header.

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
      "body": "**HIGH**\n\nThe original error gets dropped by `map_err(|_| ...)` here. When this fails in production we only see the generic variant, not what actually went wrong."
    },
    {
      "path": "gears/foo/src/domain/service.rs",
      "line": 77,
      "side": "RIGHT",
      "body": "Why do we need the intermediate `Vec` here? The iterator is consumed once right below."
    }
  ]
}
REVIEW_EOF

gh api repos/$REPO/pulls/<PR_NUMBER>/reviews \
  --method POST \
  --input /tmp/review-payload.json
```

If there are zero line-anchored findings but at least one file-level finding, skip this review call and post only the file-level comments above.

If there are zero findings in both groups, skip the payload above and post a single review whose
`body` carries the message instead of an inline comment:

```bash
gh api repos/$REPO/pulls/<PR_NUMBER>/reviews \
  --method POST \
  -f commit_id="<HEAD_SHA>" \
  -f event="COMMENT" \
  -f body="No issues found."
```

### Step 5L (local mode): Write the markdown report

Write the report to `<REPO_ROOT>/REVIEW_<BRANCH_SLUG>.md`, overwriting any existing file at that path.

Structure:

```markdown
# Branch Review: <BRANCH_NAME>

**Branch:** <BRANCH_NAME> (base: <BASE_BRANCH>)
**Head:** <HEAD_SHA short>
**Date:** <today's date, YYYY-MM-DD>
**Commits:** <commit count from Step 1>
**Files changed:** <count> (+<additions>, -<deletions>)

## Findings

### Critical

- **<the finding's `issue`>** — `<file>:<line>` (`<ID>`)
  - **Why:** <the finding's `comment`>
  - **Fix:** <the finding's `fix`>

### High

...

### Medium

...

### Low

...

## Summary

| # | ID | Sev | Location | Issue | Fix |
|---|----|-----|----------|-------|-----|
| 1 | RUST-ERR-001 | HIGH | service.rs:42 | Error context lost | Preserve source error |
```

Rules for the report:
- One severity section per severity present. Omit empty sections.
- The report uses all three text fields, one per line: `issue` is the bolded headline, `comment`
  fills `**Why:**` (it is the field that carries the reasoning), and `fix` fills `**Fix:**`. This is
  a report rather than a conversation, so the labelled fixed shape is correct here even though
  inline comments drop it. The checklist ID **is** included, since there is no separate
  terminal-only table.
- File paths are repo-relative and include the line number, so they are clickable. Exception: a finding whose `file` is in `deleted_files` has no line — render just the file path (e.g. `` `gears/foo/src/tests.rs` ``).
- If there are zero findings, write the header plus a single line: `No issues found.`

### Step 6: Print summary

After posting (PR mode) or writing the file (local mode), print a compact summary table to the terminal:

**PR mode:**

```text
## Rust PR Review: #<PR_NUMBER>

| # | ID | Sev | Location | Issue | Fix |
|---|----|-----|----------|-------|-----|
| 1 | RUST-ERR-001 | HIGH | service.rs:42 | Error context lost | Preserve source error |
| 2 | TOOLKIT-SEC-001 | CRIT | handler.rs:18 | Raw DB connection | Use SecureConn |
| 3 | TEST-QUALITY-9 | HIGH | tests.rs | Test deleted, no follow-up | Restore or track |

For a finding whose `file` is in `deleted_files`, the Location column shows just the file path (no `:<line>`), since it was posted as a file-level comment.

Posted <N> inline comments on PR #<PR_NUMBER>.
```

**Local mode:** same table, with the header `## Rust Branch Review: <BRANCH_NAME>` and a final line:

```text
Wrote <N> findings to <REPO_ROOT>/REVIEW_<BRANCH_SLUG>.md
```

---

## Comment formatting rules

**Wording is defined in `docs/pr-review/comment-style.md`, and nowhere else.** That file is the
contract: read it rather than relying on a summary here, because a summary is what drifts. The
sub-agents produce the text in the finding's `comment` field. Do not rewrite it here beyond fixing
an outright style violation, and never substitute `issue` + `fix` for it.

The body has exactly two shapes, chosen by severity:

```text
CRITICAL or HIGH   **<SEVERITY>**
                   <blank line>
                   <comment>

MEDIUM or LOW      <comment>
```

The header is dropped for MEDIUM and LOW on purpose: a bold severity tag on every comment is the
clearest signal that a machine wrote it, and severity is still carried by the terminal summary
table (Step 6) and the local-mode report (Step 5L), so nothing is lost for triage.

Do NOT include checklist IDs (e.g. RUST-ERR-001, TOOLKIT-SEC-001) in inline comments. IDs appear
only in the terminal summary table (Step 6) and — in local mode — in the markdown report.

Mechanical rules, which the style file does not cover:
- One issue per comment. If a line has two problems, post two comments.
- Line number must point to an added/modified line that exists in the diff. Do not comment on unchanged lines.
- If you cannot determine the exact line, do not guess — skip that finding. Exception: a finding on a file in `deleted_files` has no line by design — post it as a file-level comment (see Step 5), don't skip it and don't force a line onto it.

---

## What NOT to do

- Do not approve or request changes — use `event: "COMMENT"` only
- Do not post anything to GitHub in local mode — the markdown report is the only output artifact
- Do not commit the generated `REVIEW_*.md` file
- Do not post comments on lines outside the diff
- Do not post generic praise or "LGTM" if there are no issues
- Do not invent issues without evidence in the code
- Do not complain about formatting that rustfmt handles
- Do not post a finding the checklist marks `Enforcement: clippy ... (deny)` — the build already rejects it
- Do not flag code for failing to use an API newer than the pinned toolchain (`rust-toolchain.toml`), which is what the `Requires Rust >= X.Y` markers in the checklist are for
- Do not suggest speculative abstractions or premature generalization
- Do not report more than 30 findings per review (prioritize by severity)
- If there are zero findings, post a single review comment: "No issues found." (PR mode) / write `No issues found.` into the report (local mode)
