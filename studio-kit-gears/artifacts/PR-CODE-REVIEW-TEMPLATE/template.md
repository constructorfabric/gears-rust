# Code Review Report Template

This file documents the expected format for generated code review reports.
The `/cf-gears-pr-review` workflow produces reports in `.prs/{ID}/review.md` following this structure.

---

## Format

```markdown
# Code Review: PR #{number}

**PR**: {title}
**Author**: @{author}
**Prompt**: {selected_prompt_name}
**Review Decision**: {existing_review_decision_or_none}

---

## Verdict: {icon} {APPROVE | REQUEST CHANGES | COMMENT}

{One-sentence rationale linking own findings and reviewer feedback.}

---

## Reviewer Comment Analysis

{For each human reviewer who left comments or review threads:}

### @{reviewer}

| # | Concern | Relevance | Addressed? |
|---|---------|-----------|------------|
| 1 | [{summary of concern}]({github_comment_url}) | **Valid** / **Nit** / **False positive** — {brief reason} | ✅ Yes / ❌ No / ✅ N/A |
| … | … | … | … |

{The concern text MUST be a markdown link to the GitHub comment URL.
 Comment URLs are available in `review_threads.json` (each comment node
 has a `url` field) and in `meta.json` `comments[].url`.}

{If no reviewer comments exist:}

No reviewer comments found.

---

## Architecture

{PR-level structural/design assessment from step 4b (ARCH-001 through ARCH-007),
 completed before any per-file review. Findings serious enough to affect the merge
 decision must also appear in the Verdict. Use a single "No issues" line when empty —
 this section is mandatory even with no findings.}

---

## Own Findings

### Correctness {icon}

{Assessment of logic, edge cases, error handling. For Rust changes, informed by the
 `toolkit-pr-review` pass (step 4c3) rather than derived from scratch.}

### Cargo / Clippy / Architecture Lints / Rustfmt Conformance {icon}

{Assessment of tooling conformance, from the `toolkit-pr-review` architecture/toolkit
 rule modules. N/A if no Rust code changed.}

### Code Style & Idiomatic Patterns {icon}

{Assessment of naming, structure, Rust idioms, from the `toolkit-pr-review` design pass.}

### Performance {icon}

{Assessment of performance implications, from the `toolkit-pr-review` async/perf pass.
 N/A if not applicable.}

### Test Coverage {icon}

{Assessment of unit tests and e2e tests in testing/ folder, from the `toolkit-pr-review`
 tests pass. Note missing coverage for new code paths.}

### Security {icon}

{Assessment of vulnerabilities, auth, input validation, from the `toolkit-pr-review`
 security pass.}

### Mistakes & Potential Misbehaviors {icon}

{Any bugs, logic errors, or risky patterns found.}

{Omit sections that are entirely N/A for the change type.
 For example, a Cargo.toml-only PR can omit Code Style, Performance, etc.
 Always keep: Correctness, Test Coverage, Security, and any section with findings.}

---

## Summary

| Area | Rating |
|------|--------|
| Correctness | {icon} {brief} |
| Conformance | {icon} {brief} |
| Style | {icon} {brief} |
| Performance | {icon} {brief} |
| Tests | {icon} {brief} |
| Security | {icon} {brief} |
| Reviewer concerns | {icon} {X of Y valid concerns addressed} |
| Risk | {color_icon} {Very low / Low / Low-medium / Medium / High} |

## Recommendation

{Actionable next steps, numbered. Separate into:}
{1. **Minimum** (blocking) — must fix before merge}
{2. **Recommended** — should fix, not strictly blocking}
{3. **Nice-to-have** — track as follow-up}
```

## Notes

- `{placeholders}` are filled by the LLM during the `/cf-gears-pr-review` workflow.
- Sections with no findings may use a single "No issues" line.
- Omit N/A sections entirely for minimal PRs (e.g. docs-only, config-only).
- The **Reviewer Comment Analysis** section is mandatory — it ensures the
  final verdict accounts for existing human feedback, not just the LLM's
  own code analysis.
- The **Architecture** section is mandatory — it ensures PR-level structural
  findings are captured before per-file review narrows focus.
- Icons: ✅ (pass), ⚠️ (concern), ❌ (fail), 🟢 (low risk), 🟡 (medium), 🔴 (high).
- The verdict must be one of: `✅ APPROVE`, `⚠️ REQUEST CHANGES`, `💬 COMMENT`.