---
status: accepted
date: 2026-10-24
description: "Count queue, tenant, and task-type concurrency in database slot rows reserved atomically with the claim, with an optional Cluster ready-task cache that is never authoritative."
---

# Enforce Multi-Dimensional Concurrency with Database-Level Atomic Counters

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Count at claim time with COUNT(*)](#option-1-count-at-claim-time-with-count)
  - [Option 2: Distributed in-memory semaphores via Cluster](#option-2-distributed-in-memory-semaphores-via-cluster)
  - [Option 3: Database slot rows with guarded atomic reservation](#option-3-database-slot-rows-with-guarded-atomic-reservation)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-concurrency-enforcement`

## Context and Problem Statement

Concurrency is limited on several dimensions at once: per queue (global and per tenant) and per task type (global and per tenant). A task may be assigned only when every applicable dimension has capacity, and the configured limit must be a hard upper bound across all nodes.

We must decide where these limits are counted and how a reservation stays consistent with the claim.

## Decision Drivers

* The configured limit is a hard upper bound, never exceeded across nodes.
* Reservation and the `queued → claimed` transition happen atomically.
* No dependency on the Cluster gear for correctness (PRD §1.4).
* Hot counters must not become a single-row bottleneck.
* Counters must recover from drift.

## Considered Options

1. **Count at claim time with COUNT(*)** — count `claimed`+`running` tasks per dimension on every claim.
2. **Distributed in-memory semaphores via Cluster** — keep slot counts in a Cluster-gear distributed cache.
3. **Database slot rows with guarded atomic reservation** — one `te_concurrency_slots` row per dimension scope, reserved with `used = used + 1 WHERE used < limit_value` in the claim transaction.

## Decision Outcome

Chosen option: **Option 3 — Database slot rows with guarded atomic reservation.**

Each concurrency dimension scope (`queue_global`, `queue_tenant`, `type_global`, `type_tenant`) has a row in `te_concurrency_slots` holding `used` and `limit_value`. A claim reserves every applicable dimension with a single guarded conditional update per row, in the fixed lock order defined in DESIGN §3.7, inside the claim transaction. If any dimension is full the candidate is skipped, partial reservations roll back, and the claim moves on to tasks governed by other scopes.

Slots are released whenever a task leaves `claimed` or `running` for a state that consumes no capacity, including `running → waiting`. A singleton maintenance loop recomputes `used` from the actual `claimed`+`running` count and fixes drift.

With `concurrency_shards = S > 1`, a hot row is split into S shards whose sub-limits sum to at most N (ADR-0008). The optional Cluster ready-task cache may pre-sort candidates, but the database reservation always decides.

### Consequences

* The configured limit is never exceeded, on any number of nodes.
* The Cluster gear is optional; without it dispatch falls back to database-only and stays correct.
* A claim returns `429 CONCURRENCY_LIMIT` only when nothing could be assigned and capacity was the reason.
* Limits can change at runtime; each task domain applies new limits in its maintenance loop.
* Slot reconciliation must run regularly (`slot_reconcile_interval_secs`).

### Confirmation

Tests MUST check that:

* under concurrent claims, no dimension ever exceeds its limit;
* a full dimension blocks only the tasks it governs;
* slots are released on every exit from `claimed`/`running`;
* sharded limits never admit more than N in total;
* reconciliation corrects an artificially drifted counter;
* the engine stays correct with the ready-task cache unavailable.

## Pros and Cons of the Options

### Option 1: Count at claim time with COUNT(*)

Each claim runs aggregate queries before assigning.

* Good, because there is no extra table.
* Bad, because two concurrent claims can both see free capacity (no atomic reservation).
* Bad, because aggregate queries are expensive on large tables.

**Rejected.**

### Option 2: Distributed in-memory semaphores via Cluster

Nodes coordinate counts through the Cluster gear.

* Good, because reservation can be fast.
* Bad, because correctness depends on an eventually consistent system (contradicts ADR-0001).
* Bad, because it makes the Cluster gear mandatory.

**Rejected.**

### Option 3: Database slot rows with guarded atomic reservation

Optionally hot-row sharded; reconciled periodically from actual `claimed`+`running` counts. An optional Cluster ready-task cache may speed up dispatch but is never authoritative.

* Good, because the limit is enforced in the same transaction as the claim.
* Good, because it works on every supported database through the secure ORM.
* Good, because hot-row sharding keeps per-shard sub-limits summing to ≤ N.
* Neutral, because sharding can leave capacity unused on one shard while another rejects.
* Bad, because slot rows need a reconciliation loop to correct drift.

**Chosen.**

## More Information

See DESIGN §3.2 QueueService, §3.7 `te_concurrency_slots` and Lock ordering, §4.2 Ready-task cache, and ADR-0008 for the claim algorithm and hot-row sharding.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-queue-concurrency` — per-queue global and per-tenant limits.
* `cpt-cf-task-engine-fr-type-concurrency` — per-type global and per-tenant limits.
* `cpt-cf-task-engine-fr-concurrency-overrides` — limits overridable in config by pattern.
* `cpt-cf-task-engine-fr-runtime-config` — limits change at runtime.
* `cpt-cf-task-engine-fr-cluster-cache` — the optional, non-authoritative ready-task cache.
* `cpt-cf-task-engine-nfr-availability` — dispatch stays correct without the Cluster gear.
