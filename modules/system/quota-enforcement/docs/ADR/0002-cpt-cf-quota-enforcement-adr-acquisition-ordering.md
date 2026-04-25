---
status: accepted
date: 2026-05-07
---

# Multi-Quota acquisition ordering — lexicographic by quota_id UUID

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Lexicographic by quota_id UUID](#lexicographic-by-quota_id-uuid)
  - [Composite (tenant_id, metric, quota_id)](#composite-tenant_id-metric-quota_id)
  - [Per-metric serialisation queue](#per-metric-serialisation-queue)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-acquisition-ordering`

## Context and Problem Statement

Atomic multi-Quota acquisition (`cpt-cf-quota-enforcement-fr-lease-acquire`,
`cpt-cf-quota-enforcement-fr-multi-quota-evaluation`) requires a deterministic ordering under which all workers acquire
row locks across the applicable Quota set. Without a shared discipline, concurrent multi-Quota operations can deadlock
pairwise.

## Decision Drivers

- Deadlock-free concurrent acquisition (PRD `cpt-cf-quota-enforcement-fr-lease-acquire` normative).
- Transaction-stable ordering (must not change due to field renames or schema evolution).
- Globally applicable across operations (debit, credit, rollback, lease 3-phase, batch).
- Predictable latency (no quadratic acquisition attempts).

## Considered Options

- Lexicographic by `quota_id` (UUID).
- Composite ordering by `(tenant_id, metric, quota_id)`.
- Queue-based serialisation per-metric (single writer per hot Quota).

## Decision Outcome

Chosen option: **Lexicographic by `quota_id` UUID**, because it is global, transactional- stable, immune to renames or
schema evolution, and yields a one-line deadlock-freedom proof that applies uniformly to every mutation primitive
(debit, credit, rollback, lease acquire / commit / release, batch debit).

### Consequences

- Every mutation primitive in the storage plugin sorts the applicable Quota set ascending by `quota_id` before issuing
  `SELECT ... FOR UPDATE`.
- **`quota_id` MUST be UUIDv7** (`Uuid::now_v7()` per the Rust `uuid` crate), consistent with the platform convention
  for production domain-entity IDs in Postgres-backed modules (`mini-chat` `message_id` / `attachment_id` /
  `reaction_id`, `file-parser` IR entity IDs, chat-engine ADR-0012). v7 gives time-ordered lex-comparison, which
  converts the deterministic acquisition ordering into near-monotonic creation ordering and yields better B-tree index
  locality on hot-path lookups (`quotas` PK, `quotas_subject_metric_active`). Deadlock-freedom is preserved regardless
  of UUID flavor — the property requires only that every worker uses the same ordering function. v4 was rejected because
  it sacrifices index locality for randomness that QE does not need (per-`(tenant, metric)` Quota cardinality is ~10, so
  contention-distribution benefit of randomness is negligible).
- Sharded counters (P2 hook) extend the ordering to `(quota_id, shard_id)` — additive change, no breaking impact on P1
  callers.
- Plugin tests must verify ordering discipline; an ArchUnit-style or lint check on the storage-plugin code is the
  canonical confirmation.

### Confirmation

Confirmed by storage-plugin unit tests that exercise concurrent multi-Quota mutations and verify no deadlock under
sustained contention; supplemented by code review against the ordering rule in every mutation method.

## Pros and Cons of the Options

### Lexicographic by quota_id UUID

- Good, because globally unique, schema-stable, rename-immune.
- Good, because the deadlock-freedom proof is one line ("all workers acquire in same order").
- Good, because the same ordering applies to every mutation primitive — no per-operation exception handling.
- Neutral, because adding sharded counters in P2 requires extending ordering to `(quota_id, shard_id)` — additive, no
  breaking impact.

### Composite (tenant_id, metric, quota_id)

- Good, because it groups locks by tenant in plan output, which can aid log reading.
- Bad, because it adds key cardinality with no stronger guarantee — `quota_id` alone is already globally unique.
- Bad, because composite keys couple to schema (rename of `metric` field would touch ordering).

### Per-metric serialisation queue

- Good, because it eliminates contention by single-writer-per-metric.
- Bad, because it adds latency at the queue boundary even under low contention.
- Bad, because multi-metric mutations (e.g., a debit touching tenant-scoped and user-scoped Quotas across different
  metrics) require N queue interactions — high complexity for a marginal benefit.

## More Information

DESIGN §3.6 — `cpt-cf-quota-enforcement-seq-debit`, `-lease-acquire`, `-batch-debit` illustrate the sorted-FOR-UPDATE
pattern in their flow diagrams.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-quota-enforcement-fr-multi-quota-evaluation` — atomic acquisition.
- `cpt-cf-quota-enforcement-fr-lease-acquire` — deadlock freedom under concurrent leases.
