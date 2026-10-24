---
status: proposed
date: 2026-10-24
description: "Claim tasks with a plain scoped read plus a guarded conditional update through the ToolKit secure ORM, reserve slots with single-statement conditional updates, and shard hot rows and background loops instead of using row locks or dialect SQL."
---

# Claim Tasks with a DB-Agnostic Optimistic Guarded Update

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: SELECT ... FOR UPDATE SKIP LOCKED](#option-1-select--for-update-skip-locked)
  - [Option 2: Leader-serialized claims](#option-2-leader-serialized-claims)
  - [Option 3: Optimistic guarded update with k-way merge](#option-3-optimistic-guarded-update-with-k-way-merge)
  - [Option 4: Candidate spreading without row locks](#option-4-candidate-spreading-without-row-locks)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-atomic-claim-strategy`

> **TODO (revisit before implementation)**: this decision does not yet account for **head-row contention**. All concurrent claimers target the same global-minimum candidate, so on a busy queue N−1 workers block on one row lock and then lose the race; claims effectively serialize per queue. The rejection of `SKIP LOCKED` rests on a ToolKit limitation, not on its merits. Explore (a) exposing `FOR UPDATE SKIP LOCKED` for PostgreSQL / MySQL through the secure ORM, keeping the optimistic path for SQLite; (b) randomized or worker-offset candidate selection within the head page, quantifying the ordering relaxation; (c) batch claims. Benchmark with ≥ 100 workers on one queue against the claim latency and throughput NFRs, then update this ADR and DESIGN §4.6 Task Assignment.

## Context and Problem Statement

Many workers on many nodes claim tasks from shared queues at the same time. Two workers must never claim the same task, the best eligible task across all requested queues (PRD §5.2 order) must win, and slot reservation must be atomic with the claim (ADR-0005). The engine runs on PostgreSQL, MariaDB/MySQL, and SQLite through the ToolKit secure ORM.

Scoped `SecureSelect` offers `lock_shared()` / `lock_exclusive()` (`FOR UPDATE`), but no `SKIP LOCKED`, which exists only inside the toolkit-db outbox. Plain `FOR UPDATE` does not fix head-row contention.

> **TODO(toolkit)**: request a scoped `lock_exclusive_skip_locked()` on `SecureSelect`. sea-orm already has `lock_with_behavior(…, SkipLocked)`, so the change is small. Target PG, MySQL 8, MariaDB 10.6+; no-op on SQLite.
>
> **Options** (details in DESIGN §4.6):
> - (A) `SKIP LOCKED` per queue stream — preferred;
> - (B) candidate spreading (Option 4) — needs PRD sign-off on relaxed order; fallback;
> - (C) node-local claim coalescing;
> - (D) batch claim;
> - (E) virtual sub-queues — last resort;
> - (F) ready-task cache — optimization only.
>
> **Suggested:** A + D + C. Benchmark ≥ 100 workers on one queue, then accept this ADR.

## Decision Drivers

* No double claim, on any backend.
* Exact claim order (PRD §5.2): per queue urgency, then tenant fairness, then the tenant's `(priority, created_at, task_id)`; across queues `(urgency, created_at, task_id)`.
* No dialect-specific SQL; must work through the secure ORM and on SQLite.
* Claim latency within the PRD targets (p50 50 ms, p99 200 ms).
* Claim throughput on one queue must grow with the number of workers, not collapse.
* Hot rows (counters) and background loops must not become single bottlenecks.

## Considered Options

1. **SELECT ... FOR UPDATE SKIP LOCKED** — lock candidate rows and skip locked ones.
2. **Leader-serialized claims** — route all claims through one leader node.
3. **Optimistic guarded update with k-way merge** — read per-queue heads without locks, merge them by claim order, then claim with `UPDATE ... WHERE state_code = queued`; exactly one racing worker sees `rows_affected == 1`.
4. **Candidate spreading without row locks** — keep the optimistic guarded update, but each claimer starts at a random (or `hash(worker_id)`-based) position within the first `claim_scan_limit` candidates instead of the head.

## Decision Outcome

Chosen option: **Option 3 — Optimistic guarded update with k-way merge**, as a provisional choice that works with today's ToolKit. It is expected to be replaced by Option 1 (where ToolKit allows) or combined with Option 4 once the TODO above is resolved.

Claims use lock-free optimistic concurrency. The engine reads candidate heads with plain scoped `SELECT`s, merges them in claim order, and attempts each candidate with one guarded update that requires `state_code = queued`, reserves concurrency slots with `used = used + 1 WHERE used < limit_value`, sets the lease (a random `lease_token` bound to `assignee_subject`, `lease_epoch + 1`, `lease_until`; ADR-0011), and increments `attempt`, all in one transaction and in the fixed lock order of DESIGN §3.7. A guard matching zero rows means another worker won; the engine tries the next candidate.

All queues in one claim must resolve to one task domain, so the claim runs inside one database. The number of queues per claim is capped by `claim_max_queues`.

Hot rows are sharded: with `counter_shards = S > 1`, a cross-tenant slot or backlog row (`te_concurrency_slots`, `te_queue_backlog`) is split into S shards whose sub-limits sum to at most N, and a claim reserves against one shard. Background loops (timer wheel, lease scan) are split into `bg_shards` shards guarded by advisory locks; low-frequency maintenance runs under a single advisory-lock leader.

### Consequences

* There is no double claim and no dialect branching anywhere in the Task Engine.
* SQLite is supported for development, test, and low-volume single-node use.
* Claim order is exact across queues.
* Concurrency limits stay hard upper bounds even when sharded; sharding may leave some capacity unused on one shard.
* Lease fencing (`lease_token`, `lease_epoch`) is set by the same guarded update, so stale workers are rejected with `LEASE_LOST`.
* A cross-domain claim is rejected with `invalid_argument`.

### Confirmation

Tests MUST check that:

* concurrent claims never assign one task twice, on every backend;
* the claimed task is the first eligible task in the PRD §5.2 order across all requested queues, regardless of queue order;
* slot reservation and the state change commit or roll back together;
* sharded limits never admit more than N;
* claim latency meets the PRD targets under the reference load.

## Pros and Cons of the Options

### Option 1: SELECT ... FOR UPDATE SKIP LOCKED

The classic PostgreSQL/MySQL queue pattern.

* Good, because it is the standard PostgreSQL queue pattern (River, Oban, graphile-worker, pgmq).
* Good, because claimers skip rows that others hold, so workers spread over the head and claims do not serialize on one row.
* Good, because it keeps exact head order for the rows that are free.
* Bad, because the secure ORM does not expose it to gears today (see TODO(toolkit) above).
* Bad, because SQLite has no equivalent, so SQLite keeps the optimistic path (one writer, so no contention there).

**Preferred if ToolKit can expose it — see the TODO above.**

### Option 2: Leader-serialized claims

A single node assigns tasks in order.

* Good, because ordering is trivially exact.
* Bad, because throughput is capped by one node.
* Bad, because leader failover adds claim downtime.

**Rejected.**

### Option 3: Optimistic guarded update with k-way merge

Each requested queue's next candidates (index-ordered, no DB sort: a head page for `fifo` queues, one next-tenant probe for `round_robin` queues) are read through the secure ORM; a k-way merge yields candidates in global order; the claim and slot reservation run in one transaction with guarded conditional updates. Losers move to the next candidate within `claim_candidate_budget`.

* Good, because it behaves identically on every backend, including SQLite.
* Good, because it uses only the secure ORM surface.
* Good, because ordering is exact across queues and independent of the order queues are listed.
* Bad, because every claimer targets the same head row: on a busy queue N−1 workers wait for one row lock and then lose, so claims serialize per queue (head-row contention).
* Bad, because the cost is bounded by `claim_max_queues × claim_scan_limit`, so those limits must exist.

**Chosen for now (works today); not final.**

### Option 4: Candidate spreading without row locks

Workers aim at different rows, so most guarded updates succeed on the first try. Strict head order becomes "one of the first k".

* Good, because it removes most head-row contention with no ToolKit change and works on every backend.
* Good, because it combines well with batch claims.
* Bad, because claim order is no longer exact: a task may be claimed before up to k−1 older or higher-priority tasks; this must be checked against PRD §5.2 / §5.6.
* Neutral, because SQLite (single writer) gains nothing and can keep head order.

**Candidate — see the TODO above.**

## More Information

```text
1. per queue: fifo     -> SELECT head page ORDER BY (urgency, priority, created_at, task_id) LIMIT claim_scan_limit
               round_robin -> SELECT next tenant's best task ORDER BY (owner_tenant_id, priority, created_at, task_id)
                              WHERE urgency = u AND owner_tenant_id > cursor LIMIT 1
2. k-way merge across queues by (urgency, created_at, task_id)
3. per candidate (within claim_candidate_budget):
     UPDATE te_task_state SET state=claimed, lease... WHERE task_id=? AND state=queued
     UPDATE te_concurrency_slots SET used=used+1 WHERE ... AND used < limit_value          (cross-tenant)
     UPDATE te_tenant_concurrency_slots SET used=used+1 WHERE ... AND used < limit_value   (per tenant)
     rows_affected == 1 on all -> commit; else rollback and try next
```

See DESIGN §3.8 multi-instance coordination and §4.6 Task Assignment.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-assignment` — atomic claim with exact cross-queue order.
* `cpt-cf-task-engine-fr-priority-dispatch` — claim order: urgency, tenant fairness, tenant-local priority.
* `cpt-cf-task-engine-fr-lease-fencing` — lease token and epoch set in the claim update.
* `cpt-cf-task-engine-fr-queue-concurrency` — slot reservation in the claim transaction.
* `cpt-cf-task-engine-nfr-latency` — claim latency targets.
* `cpt-cf-task-engine-nfr-throughput` — no leader bottleneck on claims.
