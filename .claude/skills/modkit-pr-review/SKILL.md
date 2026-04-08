---
name: modkit-pr-review
description: "Review Rust PRs against idiomatic Rust guidelines and ModKit framework rules, post inline comments on GitHub"
user-invocable: true
allowed-tools: Bash, Read, Glob, Grep, Write
---

# Rust PR Review

Review a GitHub pull request for Rust code quality and ModKit framework compliance.
Posts findings as inline review comments directly on the PR.

**Usage**: `/modkit-pr-review <PR_NUMBER>`

---

## Inputs

- `<PR_NUMBER>` — required, the GitHub PR number (e.g. `123`)

## Review guidelines

Apply **Rust idioms and engineering** (`docs/pr-review/modkit-rust-review.md`) to every `.rs` file in the diff.

Apply **ModKit framework compliance** (`docs/pr-review/modkit-framework-compliance-review.md`) **only** to `.rs` files that belong to ModKit-owned code. A file is ModKit-owned when **any** of these signals is present:

1. **Cargo.toml signals** — the nearest `Cargo.toml` (same crate or workspace member) declares a `modkit` dependency/feature, or the crate name starts with `modkit`.
2. **Path heuristics** — the file lives under a path that matches ModKit module conventions (e.g. `modules/*/src/`, `crates/modkit-*/`, or similar namespace).
3. **Source-level symbols** — the file imports from ModKit crates (`use modkit_*`, `use crate::` inside a modkit crate) or references ModKit-specific types/traits such as `OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, or `ModuleLifecycle`.

If none of these signals are detected, skip the framework compliance checklist for that file and apply only the general Rust idioms checklist.

For non-Rust files in the diff (TOML, YAML, migrations, etc.) — apply only general correctness checks, do not force Rust-specific rules.

## Coding guidelines reference

When reviewing, also consult:
- `guidelines/DNA/languages/RUST.md` — project Rust conventions
- `guidelines/SECURITY.md` — security requirements

---

## Steps

### Step 1: Fetch PR metadata and diff

```bash
gh pr view <PR_NUMBER> --json number,title,body,headRefOid,baseRefName,headRefName
gh pr diff <PR_NUMBER>
```

Save the diff output for analysis. Extract the HEAD commit SHA — you need it for posting comments.

### Step 2: Identify Rust files in diff

Parse the diff to find all `.rs` files that were added or modified.
For each file, note the changed line ranges (added lines only — you can only comment on lines present in the diff).

### Step 3: Read review guidelines and classify files

Read `docs/pr-review/modkit-rust-review.md` (always needed).

For each `.rs` file from Step 2, determine whether it is ModKit-owned code:
- Check the nearest `Cargo.toml` for modkit dependencies/features or a `modkit-` crate name.
- Check whether the file path matches ModKit module conventions (`modules/*/src/`, `crates/modkit-*/`).
- Scan the file for ModKit imports (`use modkit_*`) or ModKit types (`OperationBuilder`, `SecureConn`, `SecureORM`, `ClientHub`, `ModuleLifecycle`).

If **any** file is classified as ModKit-owned, also read `docs/pr-review/modkit-framework-compliance-review.md`.

### Step 4: Review each changed file

For each `.rs` file in the diff:

a. Read the full current file from the repo (not just the diff hunk) to understand context.
b. Apply **modkit-rust-review.md** checklist items — idiomatic Rust, error handling, async safety, ownership, testing, etc.
c. **Only if the file was classified as ModKit-owned in Step 3**, also apply **modkit-framework-compliance-review.md** checklist items — SDK pattern, OperationBuilder, SecureConn, module layout, error types, etc.
d. Record each finding with: checklist ID, severity, file path, line number, issue description, fix.

### Step 5: Filter and deduplicate

- Drop findings that are not evidenced in the diff
- Drop style issues that rustfmt/clippy should catch
- Drop speculative or hypothetical issues
- Merge overlapping findings on the same line
- Keep only findings where you have concrete evidence

### Step 6: Post inline review comments on GitHub

Use `gh api` to create a pull request review with inline comments.

Build the review payload:

IMPORTANT: The `gh api` `-f` array syntax is limited. For multiple comments, build a JSON file and POST it.

The review `body` MUST be empty string — no summary in the review itself. The summary goes to the terminal only (Step 7).

```bash
cat > /tmp/review-payload.json << 'REVIEW_EOF'
{
  "commit_id": "<HEAD_SHA>",
  "event": "COMMENT",
  "body": "",
  "comments": [
    {
      "path": "modules/foo/src/domain/service.rs",
      "line": 42,
      "side": "RIGHT",
      "body": "**HIGH**\n\nError context discarded by `map_err(|_| ...)`.\n\nPreserve the source error — wrap with `.context()` or map to a domain error that keeps the cause."
    }
  ]
}
REVIEW_EOF

gh api repos/{owner}/{repo}/pulls/<PR_NUMBER>/reviews \
  --method POST \
  --input /tmp/review-payload.json
```

### Step 7: Print summary

After posting, print a compact summary table to the terminal:

```
## Rust PR Review: #<PR_NUMBER>

| # | ID | Sev | Location | Issue | Fix |
|---|----|-----|----------|-------|-----|
| 1 | RUST-ERR-001 | HIGH | service.rs:42 | Error context lost | Preserve source error |
| 2 | MODKIT-SEC-001 | CRIT | handler.rs:18 | Raw DB connection | Use SecureConn |

Posted <N> inline comments on PR #<PR_NUMBER>.
```

---

## Comment formatting rules

Each inline comment MUST follow this format:

```
**<SEVERITY>**

<One-sentence issue description.>

<One-sentence why it matters.>

<Concrete fix — what to change, not a vague suggestion.>
```

Where `<SEVERITY>` is one of: `CRITICAL`, `HIGH`, `MEDIUM`, `LOW`.

Do NOT include checklist IDs (e.g. RUST-ERR-001, MODKIT-SEC-001) in inline comments. IDs appear only in the terminal summary table (Step 7).

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
