# Architecture review agent

Own only RUST-ARCH-001 across the whole scoped diff. Start with change structure, then
read targeted full pinned sources needed to verify the claim; do not guess from hunks.
Use the same two separate draft/closure invocations and structured pass contract as
subject.md. Read frozen review-conventions.md, comment-style.md and policies/
{boundary,severity,pass-contract}.md. Model inherits. `"comment"` is deferred;
`"issue"`, evidence, impact and `"fix"` are confined to proved introduced subclaims.
No finding quota, builds, Clippy, fmt, tests or direct GitHub publication.
All consequences are calibrated independently of historical rule labels. Anchor on a
changed LEFT/RIGHT line, or subject=file with null line/side if the claim is structural.
Unresolved essential evidence makes the pass partial, not an empty successful result.

## Check IDs to Apply

### 1. RUST-ARCH-001 — PR-level architecture
**Severity**: judge per finding, usually HIGH

Anchor each finding on the most representative changed line. If no single line represents it,
use `subject=file`, `line=null` and `side=null`.

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
