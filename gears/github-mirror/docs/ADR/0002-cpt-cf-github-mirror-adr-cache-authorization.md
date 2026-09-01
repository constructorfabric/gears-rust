---
status: accepted
date: 2026-08-11
decision-makers: GitHub Mirror design review
---

# ADR-0002: Use visibility-aware raw response cache partitioning

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. One cache namespace per token](#a-one-cache-namespace-per-token)
  - [B.Shared cache by canonical request key for every repository](#bshared-cache-by-canonical-request-key-for-every-repository)
  - [C. Disable shared raw cache in multi-tenant deployments](#c-disable-shared-raw-cache-in-multi-tenant-deployments)
  - [D. Visibility-aware cache partitioning](#d-visibility-aware-cache-partitioning)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-github-mirror-adr-cache-authorization`

## Context and Problem Statement

Raw GitHub responses are the source of truth for normalization and re-normalization. Caching those responses is also central to cost-efficient synchronization. Server deployments may synchronize the same GitHub repository for multiple tenants, but repository visibility changes the security boundary. Public repository data can be cached once per canonical org/repo because the GitHub Mirror API layer remains responsible for tenant-specific authorization and presentation. Private repository data must never cross tenant boundaries.

The decision is whether to isolate cache content per token, share cache content for every repository, disable sharing, or partition raw cache entries by repository visibility: shared org/repo cache for public repositories and strict tenant-scoped cache for private repositories.

## Decision Drivers

- `cpt-cf-github-mirror-fr-raw-storage` requires raw response persistence.
- `cpt-cf-github-mirror-fr-cost-efficiency` requires cache reuse and conditional requests.
- `cpt-cf-github-mirror-fr-access-control` and `cpt-cf-github-mirror-fr-multi-tenancy` require preventing cross-tenant exposure of private data.
- `cpt-cf-github-mirror-nfr-security` requires zero token exposure and no raw-cache path that bypasses API authorization.
- Public repository data may legitimately be synchronized once and exposed differently through tenant-specific API authorization decisions.
- Private repository data must be stored and reused only within the tenant that synchronized it.
- Validator reuse (`ETag`, `Last-Modified`) can reduce cost and follows the same visibility-aware cache partition as the body.

## Considered Options

A. One cache namespace per token.
B. Shared cache by canonical request key for every repository.
C. Disable shared raw cache in multi-tenant deployments.
D. Visibility-aware cache partitioning.

## Decision Outcome

Chosen option: "D. Visibility-aware cache partitioning", because it preserves storage/API savings for public repositories while preserving strict tenant isolation for private repositories. Public repository raw responses are stored once under a canonical public cache key derived from host, owner, repository, API family, endpoint, query, headers that affect representation, and API version. Private repository raw responses are stored under a tenant-scoped cache key that includes `tenant_id` before the canonical repository/request dimensions. Unknown repository visibility is treated as private until a successful GitHub response proves the repository is public.

The raw cache is not an authorization surface for API consumers. The GitHub Mirror API layer applies tenant and access-control checks when serving normalized data, so the same public cached data may be visible through different tenant-specific policies. For private repositories, the cache backend must not return bodies or validators across tenant partitions.

### Consequences

- Cache backends must include repository visibility and tenant partitioning in cache-key construction.
- Public repository cache entries can be reused across tenants and tokens.
- Private or visibility-unknown repository cache entries must include `tenant_id` and must never be reused across tenants.
- Raw tokens are never stored or logged; token fingerprints may still be used for telemetry, rate-limit accounting, and credential health, but not as the primary cache-sharing boundary.
- API-level tenant and access checks remain mandatory for all normalized data served from public or private cache-derived records.

### Confirmation

- Tests verify public repository cache entries are reused across tenants only through the public cache partition.
- Tests verify private and visibility-unknown repository cache entries are never read or validated from another tenant partition.
- Tests verify cache keys include representation-affecting request dimensions so public sharing cannot mix incompatible responses.
- Tests verify GitHub Mirror API reads still enforce tenant and access-control checks over normalized data.
- Static checks verify raw tokens and authorization headers are not logged or persisted.

## Pros and Cons of the Options

### A. One cache namespace per token

Each token has an isolated raw-response cache tree or database partition.

- Good, because authorization is simple: a token only reads data it fetched.
- Good, because endpoint-scope differences are naturally isolated.
- Bad, because identical public/repository responses (e.g. shared libs) are duplicated across tokens.
- Bad, because token rotation reduces cache hit ratio.
- Bad, because storage grows with token count instead of repository/content size.

### B.Shared cache by canonical request key for every repository

All tenants and tokens read any body stored under the canonical request key.

- Good, because it maximizes cache hit ratio and deduplication.
- Good, because implementation is simple.
- Bad, because it can expose private repository data across tenants.
- Bad, because it treats public and private repository data as the same security boundary.
- Bad, because it violates multi-tenancy and access-control requirements for private repositories.

### C. Disable shared raw cache in multi-tenant deployments

Only single-token or single-tenant deployments use raw response cache; server deployments avoid shared cache reads.

- Good, because it avoids cross-tenant cache authorization complexity.
- Good, because security review is simpler.
- Bad, because it sacrifices the main cost-efficiency mechanism in the highest-scale deployment mode.
- Bad, because it creates divergent behavior between CLI and server modes.
- Bad, because raw response persistence is still required for provenance and re-normalization.

### D. Visibility-aware cache partitioning

Store public repository raw responses once per canonical org/repo/request key, but store private repository raw responses inside a tenant-scoped partition.

- Good, because it maximizes public repository cache reuse without duplicating public data per tenant or token.
- Good, because private repository bodies and validators cannot cross tenant boundaries.
- Good, because API-level tenancy and access checks remain the only authorization surface for serving normalized data to consumers.
- Good, because token rotation does not destroy cache efficiency for public repositories.
- Bad, because cache implementations must know or persist repository visibility before selecting the partition.
- Bad, because a repository visibility transition requires careful cache invalidation or migration, and unknown visibility must default to the private partition.

## More Information

Detailed pseudocode is in [ALGORITHMS.md](../ALGORITHMS.md), especially the cache-before-network and visibility-aware cache-partition sections.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Algorithms**: [ALGORITHMS.md](../ALGORITHMS.md)

This decision directly addresses:

- `cpt-cf-github-mirror-fr-raw-storage`
- `cpt-cf-github-mirror-fr-cost-efficiency`
- `cpt-cf-github-mirror-fr-access-control`
- `cpt-cf-github-mirror-fr-multi-tenancy`
- `cpt-cf-github-mirror-fr-token-pool`
- `cpt-cf-github-mirror-nfr-security`
