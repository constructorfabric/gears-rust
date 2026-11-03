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
  - [Option 4: One row per slot](#option-4-one-row-per-slot)
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
3. **Database slot rows with guarded atomic reservation** — one counter row per dimension scope (`te_concurrency_slots` for cross-tenant, `te_tenant_concurrency_slots` for per-tenant dimensions), reserved with `used = used + 1 WHERE used < limit_value` in the claim transaction.
4. **One row per slot** — a fixed set of slot rows per limit; a claim takes a free slot row with a guarded update and records the owning task.

## Decision Outcome

Chosen option: **Option 3 — Database slot rows with guarded atomic reservation.**

Each concurrency dimension scope has one counter row holding `used` and `limit_value`. The rows are split by **scope**, so that the secure ORM can protect the per-tenant ones:

* **Cross-tenant dimensions** (`queue_global`, `type_global`) live in `te_concurrency_slots`, keyed `(dimension_code, scope_id, shard)`. The table has no tenant column and holds no tenant data, so it is `#[secure(unrestricted)]`, reached on request paths only through a narrow `GlobalCounterRepo` (guarded `+1`/`−1` on a key computed from an already-authorized task row) under a justified `allow_all()`.
* **Per-tenant dimensions** (`queue_tenant`, `type_tenant`) live in `te_tenant_concurrency_slots`, keyed `(owner_tenant_id, dimension_code, scope_id, shard)`, with `owner_tenant_id` as a real `tenant_col`. Request paths write them under `scope.tenant_only()` after the task row was authorized with the full scope, so a reservation charged to the wrong tenant fails in the ORM.

Every key column is `NOT NULL` (the dimension says whether `scope_id` is a queue or a task type), so each primary key is a real uniqueness constraint on every backend. A claim reserves every applicable dimension inside the claim transaction, **before it writes any task row**, in the fixed lock order defined in DESIGN §3.7 (counters before task rows; global rows before tenant rows): one guarded `used = used + k WHERE used + k <= limit_value` per counter row, then the guarded `queued → claimed` updates, then the unused part of `k` is returned on the same rows (ADR-0008, ToolKit secure claim primitive). If any dimension is full the candidate is skipped without any task write, and the claim moves on to tasks governed by other scopes. Single-task admissions use one guarded `used = used + 1 WHERE used < limit_value` per row.

Slots are released whenever a task leaves `claimed` or `running` for a state that consumes no capacity, including `running → waiting`. A reservation on a sharded row records its shard on the task, and the release decrements exactly that shard.

A singleton maintenance loop reconciles each counter row from the live tasks charged to it, in either direction. It locks the counter row first and counts in a new statement afterwards, so a claim or release running at the same time is either counted or applied after the correction — never lost or counted twice (DESIGN §3.7 Counter reconciliation). On MySQL/MariaDB the reconciliation transaction runs at `READ COMMITTED`.

With `counter_shards = S > 1`, a row with a large limit is split into S shards whose sub-limits sum to at most N (ADR-0008). This applies to per-tenant rows too: most tenants stay on one shard, but one busy tenant (for example the platform tenant on the `system` queue) must not turn its own row into a hot spot. The optional Cluster ready-task cache may pre-sort candidates, but the database reservation always decides.

### Consequences

* The configured limit is never exceeded, on any number of nodes.
* The Cluster gear is optional; without it dispatch falls back to database-only and stays correct.
* A claim returns `429 CONCURRENCY_LIMIT` only when nothing could be assigned and capacity was the reason.
* Limits can change at runtime; each task domain applies new limits in its maintenance loop.
* Slot reconciliation must run regularly (`slot_reconcile_interval_secs`); it locks only counter rows, never task rows, so it cannot deadlock with claims.
* Each leased task carries the shards it reserved (`queue_slot_shard`, `type_slot_shard`, `tenant_queue_slot_shard`, `tenant_type_slot_shard`).
* Per-tenant counters get the same ORM tenant isolation as task rows; the cross-tenant table is a declared, lint-gated exception to the ToolKit `unrestricted` guidance (DESIGN §2.1), justified because it has no tenant column and is reachable only through `GlobalCounterRepo`.

### Confirmation

Tests MUST check that:

* under concurrent claims, no dimension ever exceeds its limit;
* a full dimension blocks only the tasks it governs;
* slots are released on every exit from `claimed`/`running`;
* sharded limits never admit more than N in total;
* reconciliation corrects an artificially drifted counter, upward and downward;
* reconciliation that runs during concurrent claims and releases never lets a limit be exceeded, on PostgreSQL and MySQL/MariaDB;
* every release decrements the shard recorded at reservation, and a shard-count change never admits more than N;
* a per-tenant reservation attempted with a scope that does not cover the counter's tenant is rejected by the ORM;
* the counter migrations create their primary keys on PostgreSQL, MySQL/MariaDB, and SQLite (no nullable key column);
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

### Option 4: One row per slot

Instead of a `used` counter, a limit of N has N rows; each row is free or held by one task.

* Good, because a held slot names its task, so drift cannot happen and no reconciliation is needed.
* Good, because claims on different free rows do not contend on one counter row.
* Bad, because a limit change must add or delete rows, and shrinking a limit with held slots is complex.
* Bad, because large or unlimited limits (thousands of slots, or 0) need many rows or a special case.
* Bad, because finding a free row adds a read to every claim.

**Rejected.**

## More Information

See DESIGN §2.1 Secure-by-default, §3.2 QueueService, §3.7 `te_concurrency_slots` and `te_tenant_concurrency_slots` and Lock ordering, §4.2 Ready-task cache, and ADR-0008 for the claim algorithm and hot-row sharding.

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
