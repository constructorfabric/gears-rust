---
status: accepted
date: 2026-08-11
decision-makers: GitHub Mirror design review
---

# ADR-0003: Rate-limit admission happens per token before shared request capacity is consumed

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Single global semaphore only](#a-single-global-semaphore-only)
  - [B. Acquire global permit then check token quota](#b-acquire-global-permit-then-check-token-quota)
  - [C. Fixed per-token concurrency limits](#c-fixed-per-token-concurrency-limits)
  - [D. Per-token admission before shared capacity with adaptive soft caps](#d-per-token-admission-before-shared-capacity-with-adaptive-soft-caps)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-github-mirror-adr-rate-limit-admission`

## Context and Problem Statement

GitHub Mirror must maximize throughput while avoiding primary and secondary GitHub rate limits. Multiple repositories and sessions may run concurrently, and token pools may contain tokens with independent budgets. If rate-limited work occupies shared request capacity while sleeping, healthy tokens can starve and synchronization throughput collapses.

The decision is how to order global concurrency and per-token rate-limit admission, and whether concurrency should be fixed or adaptive.

## Decision Drivers

- `cpt-cf-github-mirror-fr-rate-limit` requires respecting GitHub rate limits and `Retry-After`.
- `cpt-cf-github-mirror-fr-parallel-fetch` requires parallel fetching while maximizing throughput.
- `cpt-cf-github-mirror-fr-token-pool` requires independent per-token budgets and rotation.
- `cpt-cf-github-mirror-nfr-rate-compliance` requires avoiding secondary rate-limit bans.
- Inline `X-RateLimit-Remaining` headers can appear exhausted under concurrency before authoritative `/rate_limit` confirms exhaustion.
- Secondary rate limits require reducing request pressure, not merely sleeping every worker equally.

## Considered Options

- A. Single global semaphore only.
- B. Acquire global permit then check token quota.
- C. Fixed per-token concurrency limits.
- D. Per-token admission before shared capacity with adaptive soft caps.

## Decision Outcome

Chosen option: "D. Per-token admission before shared capacity with adaptive soft caps", because it prevents exhausted tokens from occupying shared request capacity and allows healthy tokens to continue. Each token has an independent controller. Admission first evaluates backoff, quota reserve, authoritative `/rate_limit` reconciliation, and the token's current in-flight count. Only admitted requests proceed to consume shared request capacity. Response observation updates budget headers and applies AIMD: secondary-limit or `Retry-After` responses halve the soft cap; healthy successful responses increase it toward the configured maximum.

### Consequences

- Every request path must call per-token admission before consuming shared request capacity.
- Every response/error path must release the token in-flight slot.
- Controllers must distinguish core REST quota from search and GraphQL quota pools.
- `/rate_limit` probing must be serialized or cached to avoid a probe thundering herd.
- Operators get better utilization under token pools, but behavior is more complex than fixed limits.

### Confirmation

- Unit tests verify admission/release changes in-flight counts correctly.
- Unit tests verify 429 or `Retry-After` halves the soft cap and sets backoff.
- Unit tests verify successful healthy responses increase the soft cap.
- Integration tests run concurrent sessions and confirm rate-limited tokens do not block healthy tokens.
- E2E tests verify synchronization with parallelism does not trigger secondary rate-limit bans under normal operating bounds.

## Pros and Cons of the Options

### A. Single global semaphore only

All requests acquire from one process-wide concurrency limit with no per-token admission gate.

- Good, because it is simple to implement and easy to reason about.
- Good, because it caps total process pressure.
- Bad, because it does not model independent token budgets.
- Bad, because a single exhausted token can keep retrying and consuming global slots.
- Bad, because it cannot exploit token pools effectively.

### B. Acquire global permit then check token quota

Requests first acquire shared capacity, then wait on per-token rate-limit gates while holding that capacity.

- Good, because request concurrency is globally bounded before any other work.
- Neutral, because it can work under one token and light load.
- Bad, because rate-limited tokens can hold permits while sleeping until reset.
- Bad, because healthy tokens starve behind sleeping tasks.
- Bad, because long GitHub reset windows make the system appear hung.

### C. Fixed per-token concurrency limits

Each token gets a static concurrency cap, with no AIMD adaptation.

- Good, because it isolates tokens better than a single global semaphore.
- Good, because implementation is simpler than adaptive control.
- Bad, because the right cap depends on repository shape, endpoint mix, GitHub secondary-limit behavior, and time of day.
- Bad, because a conservative cap leaves throughput unused.
- Bad, because an aggressive cap keeps triggering secondary limits.

### D. Per-token admission before shared capacity with adaptive soft caps

Each token controller gates backoff, quota reserve, authoritative quota, and in-flight count before the request consumes shared capacity.

- Good, because exhausted tokens do not occupy shared request capacity.
- Good, because token pools remain productive when one token is exhausted or backing off.
- Good, because AIMD converges toward safe throughput for current GitHub behavior.
- Good, because authoritative `/rate_limit` reconciliation avoids unnecessary hour-long sleeps on stale inline headers.
- Bad, because the controller is stateful and needs careful release/observe discipline.
- Bad, because tests must cover concurrency and retry edge cases.

## More Information

Detailed pseudocode is in [ALGORITHMS.md](../ALGORITHMS.md), especially the rate-limit admission and adaptive soft-cap sections.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Algorithms**: [ALGORITHMS.md](../ALGORITHMS.md)

This decision directly addresses:

- `cpt-cf-github-mirror-fr-rate-limit`
- `cpt-cf-github-mirror-fr-parallel-fetch`
- `cpt-cf-github-mirror-fr-token-pool`
- `cpt-cf-github-mirror-nfr-rate-compliance`
- `cpt-cf-github-mirror-nfr-parallel-sync`
