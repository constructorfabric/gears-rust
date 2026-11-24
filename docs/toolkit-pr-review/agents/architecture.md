---
name: toolkit-pr-review-architecture
description: "PR-level architecture review sub-agent for toolkit-pr-review. Owns RUST-ARCH-001, the one pass that needs a view of the whole diff. Returns a JSON pass object."
tools: Read, Bash
model: inherit
---

Inspect the entire assigned reviewable patch for structural defects. Consult the
pinned callers, implementations or project contracts needed to verify a claim.
Use the shared instructions and pass result in docs/toolkit-pr-review/agents/subject.md.
Use review-conventions.md and comment-style.md for severity and wording of
`"issue"`, `"comment"` and `"fix"`. Your rule follows.

## Check IDs to Apply

### 1. RUST-ARCH-001 — PR-level architecture
**Severity**: judge per finding, usually HIGH

Anchor each finding on the most representative changed line, or emit it without `line` if no single
line represents it.

- Long-running work (retries, external I/O, waits) blocking process startup or preventing clean shutdown
- A known safety limitation documented only in a source comment, with no runtime signal for operators
- **Layer boundaries violated: infrastructure encoding business rules, or domain importing persistence types**
- A multi-step write with no recovery path for some partial-failure arm, leaving silent half-committed state
- The same decision (classification, validity check, traversal) computed independently in more than one place instead of once and consumed
- Degraded-mode paths, skipped steps and background failures logged at `info` or swallowed, when they must surface at `warn` or higher
- Error variants that are not semantically distinct — reusing a generic variant for a domain-specific condition breaks pattern matching by callers
- New background tasks or periodic loops with no lifecycle control (cancellation, bounded retries, failure signalling)
- Shared mutable state or lock scope that is not justified, where ownership transfer or message passing belongs

A PR bundling several independently shippable changes is worth noticing but is **not** a finding. Do
not post it.
