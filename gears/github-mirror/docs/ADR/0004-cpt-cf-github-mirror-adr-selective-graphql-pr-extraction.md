---
status: accepted
date: 2026-08-11
decision-makers: GitHub Mirror design review
---

# ADR-0004: Use REST-first synchronization with selective cost-governed GraphQL for pull-request child extraction

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. REST-only extraction](#a-rest-only-extraction)
  - [B. GraphQL-only extraction](#b-graphql-only-extraction)
  - [C. Static endpoint-by-endpoint selection](#c-static-endpoint-by-endpoint-selection)
  - [D. REST-first with selective cost-governed GraphQL](#d-rest-first-with-selective-cost-governed-graphql)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`

## Context and Problem Statement

GitHub exposes overlapping REST and GraphQL APIs. REST is stable, cacheable with ETags, and aligns with GitHub-compatible API responses. GraphQL can reduce request count for pull-request child data by batching fields and child connections, but it has point costs, node limits, field-level errors, and no HTTP `304 Not Modified` semantics.

The decision is whether synchronization should use REST only, GraphQL only, a static endpoint split, or a REST-first approach with GraphQL used selectively when it reduces request cost.

## Decision Drivers

- `cpt-cf-github-mirror-fr-cost-efficiency` requires minimizing GitHub API calls.
- `cpt-cf-github-mirror-fr-github-compat-api` favors REST-shaped data and response compatibility.
- `cpt-cf-github-mirror-fr-pr-refinement` requires PR reviews, comments, commits, files, statuses/checks, reactions, and mergeability.
- `cpt-cf-github-mirror-fr-raw-storage` requires raw response provenance regardless of API type.
- REST supports ETag/Last-Modified conditional requests; GraphQL does not provide equivalent HTTP cache semantics.
- GraphQL is useful only when the query shape stays under point, request, and node ceilings.

## Considered Options

- A. REST-only extraction.
- B. GraphQL-only extraction.
- C. Static endpoint-by-endpoint selection.
- D. REST-first with selective cost-governed GraphQL.

## Decision Outcome

Chosen option: "D. REST-first with selective cost-governed GraphQL", because REST remains the canonical path for stable resources and GitHub-compatible response shapes, while GraphQL is used for pull-request child extraction only when batching reduces total request cost. The GraphQL extractor uses aliased primary PR chunks, continuation work items for paginated children, greedy first-fit packing by node/request ceilings, node-limit backoff, point-budget pacing, and per-PR tombstone/failure classification.

### Consequences

- Implementations must support both REST and GraphQL clients and telemetry accounting.
- GraphQL extraction must be optional/fallback-capable; REST remains available when GraphQL query shape is too expensive or unsupported.
- Raw response storage must record API type and enough metadata for both REST and GraphQL provenance.
- GraphQL point usage must be reported separately from REST request counts in summaries and telemetry.
- Tests must include field-level GraphQL errors, null nodes, pagination continuations, node-limit backoff, and REST fallback behavior.

### Confirmation

- Unit tests cover GraphQL cost estimation, greedy packing, node-limit backoff, null-node tombstoning, and per-alias failure classification.
- Integration tests compare PR child extraction request/point usage against REST-only extraction for representative repositories.
- Telemetry tests verify REST calls, GraphQL calls, and GraphQL points are accounted separately.
- Compatibility tests verify served GitHub-compatible REST responses continue to come from the normalized store, not from GraphQL response shapes directly.

## Pros and Cons of the Options

### A. REST-only extraction

Fetch all repository and PR data through GitHub REST endpoints.

- Good, because REST supports ETags and Last-Modified validators.
- Good, because raw responses align closely with GitHub-compatible API surfaces.
- Good, because failure modes and pagination are straightforward.
- Bad, because PR child data can require many endpoint calls per PR.
- Bad, because large repositories can burn REST quota even when data could be batched.

### B. GraphQL-only extraction

Use GraphQL for all repository data and derive normalized entities from GraphQL responses.

- Good, because batching and field selection can reduce request count.
- Good, because one query can gather multiple child connections.
- Bad, because GraphQL lacks REST ETag/304 semantics for stable resources.
- Bad, because GitHub-compatible REST response fidelity becomes harder to preserve.
- Bad, because query node limits and point limits create complex failure modes.
- Bad, because GraphQL schema changes can affect broad extraction surfaces at once.

### C. Static endpoint-by-endpoint selection

Predetermine which data families always use REST and which always use GraphQL.

- Good, because behavior is predictable and easy to document.
- Good, because implementation can be optimized per endpoint family.
- Bad, because the cost-effective choice depends on repository shape, child counts, and current point budgets.
- Bad, because static GraphQL use can be worse than REST for small or sparse entities.
- Bad, because static REST use can waste requests for dense PRs.

### D. REST-first with selective cost-governed GraphQL

Prefer REST with conditional requests; use GraphQL for PR child extraction when batching reduces cost and remains within point/node ceilings.

- Good, because stable resources keep REST cache semantics.
- Good, because dense PR child data benefits from GraphQL batching.
- Good, because GraphQL point accounting is explicit and bounded.
- Good, because REST fallback keeps extraction robust when GraphQL is unavailable or too costly.
- Bad, because two API clients and two rate/cost accounting paths must be maintained.
- Bad, because normalization must tolerate source differences between REST and GraphQL payloads.

## More Information

Detailed pseudocode is in [ALGORITHMS.md](../ALGORITHMS.md), especially the cost-governed GraphQL pull-request extraction section.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Algorithms**: [ALGORITHMS.md](../ALGORITHMS.md)

This decision directly addresses:

- `cpt-cf-github-mirror-fr-cost-efficiency`
- `cpt-cf-github-mirror-fr-pr-refinement`
- `cpt-cf-github-mirror-fr-commit-ci-refinement`
- `cpt-cf-github-mirror-fr-github-compat-api`
- `cpt-cf-github-mirror-fr-raw-storage`
- `cpt-cf-github-mirror-fr-telemetry`
