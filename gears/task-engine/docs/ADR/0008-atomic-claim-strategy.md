---
status: proposed
date: 2026-10-25
description: "Claim tasks through a ToolKit secure claim primitive that runs a counter-first, guarded-update protocol: plain scoped candidate reads, guarded slot reservation, guarded state transition, settle, commit. No SKIP LOCKED and no dialect SQL in the Task Engine."
---

# Claim Tasks Through a ToolKit Secure Claim Primitive (Counter-First Guarded Updates)

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The claim protocol](#the-claim-protocol)
  - [Why it cannot deadlock](#why-it-cannot-deadlock)
  - [Throughput](#throughput)
  - [ToolKit contract](#toolkit-contract)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Gear-level SELECT ... FOR UPDATE SKIP LOCKED](#option-1-gear-level-select--for-update-skip-locked)
  - [Option 2: Leader-serialized claims](#option-2-leader-serialized-claims)
  - [Option 3: Gear-level optimistic guarded update, task row first](#option-3-gear-level-optimistic-guarded-update-task-row-first)
  - [Option 4: Candidate spreading without row locks](#option-4-candidate-spreading-without-row-locks)
  - [Option 5: ToolKit secure claim primitive, counter-first guarded updates](#option-5-toolkit-secure-claim-primitive-counter-first-guarded-updates)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-atomic-claim-strategy`

## Context and Problem Statement

Many workers on many nodes claim tasks from shared queues at the same time. Two workers must never claim the same task, the best eligible task across all requested queues (PRD §5.2 order) must win, and slot reservation must be atomic with the claim (ADR-0005). The engine runs on PostgreSQL, MariaDB/MySQL, and SQLite through the ToolKit secure ORM, and the gear contains no dialect-specific SQL.

The same problem exists elsewhere on the platform. The `toolkit-db` outbox claims partitions and messages with its own internal, backend-specific statements (`outbox/dialect.rs`), and the Durable Execution gear claims activity instances with CAS, fences, and leases and enforces stage concurrency the same way. Claiming is therefore a platform primitive, not Task-Engine logic.

Where does the claim algorithm live, and which protocol does it run so that it is correct on every backend, deadlock-free, and fast enough for the throughput NFR?

## Decision Drivers

* No double claim, on any backend.
* A concurrency limit is a hard upper bound, never exceeded across nodes.
* Exact claim order among claimable rows (PRD §5.2): per queue urgency, then tenant fairness, then the tenant's `(priority, created_at, task_id)`; across queues `(urgency, created_at, task_id)`.
* No `SKIP LOCKED` and no dialect SQL in the Task Engine; only the secure ORM surface.
* Deadlock-free by construction, not by retry.
* No task-row write for a candidate whose limits are full.
* Claim latency within the PRD targets (p50 50 ms, p99 200 ms); throughput on one queue grows with batch size and nodes, not with parked workers.
* Reusable by other gears (Durable Execution, future job-style gears).

## Considered Options

1. **Gear-level `SELECT ... FOR UPDATE SKIP LOCKED`** — a new `SecureSelect::lock_exclusive_skip_locked()`; the gear locks candidate rows per queue stream, then counters.
2. **Leader-serialized claims** — route all claims through one leader node.
3. **Gear-level optimistic guarded update, task row first** — read candidates, `UPDATE task ... WHERE state = queued`, then reserve counters; roll back the task write when a counter is full.
4. **Candidate spreading without row locks** — like Option 3, but each claimer starts at a random position in the head page.
5. **ToolKit secure claim primitive, counter-first guarded updates** — the protocol of the Task Engine overview deck ("two workers race for T42"): plain candidate read → guarded slot reservation → guarded state transition → settle → commit, owned by `toolkit-db`, with batch claims and node-local coalescing in the gear.

## Decision Outcome

Chosen option: **Option 5 — ToolKit secure claim primitive, counter-first guarded updates.**

Every step is a plain scoped read or a guarded conditional `UPDATE` whose `WHERE` clause is the guard, re-checked by the database on the latest committed row. No step uses `SELECT ... FOR UPDATE` or `SKIP LOCKED`. The protocol lives in `toolkit-db` (`toolkit_db::secure::claim`, TODO(toolkit)); the Task Engine supplies only its candidate streams, counter keys, and the state transition.

### The claim protocol

One transaction per claim (or per coalesced claim of a waiter group), at `READ COMMITTED`: the PostgreSQL default; set explicitly through `TxConfig` on MySQL/MariaDB, whose default `REPEATABLE READ` would make a loser wait instead of moving on; SQLite has one writer.

1. **Pick candidates (no locks).** Each requested queue yields its next candidates in claim order from its dispatch index with a plain, scoped keyset read: a head page for `fifo` queues, one best task per tenant in turn for `round_robin` queues. Every read, including the round-robin "next tenant" probe, carries the caller's compiled `AccessScope`. A k-way merge orders the candidates across queues by `(urgency, created_at, task_id)`. Work is bounded by `claim_scan_rows_max`, `claim_candidate_budget`, and `claim_max_queues`.
2. **Plan capacity (no locks).** A plain read of the counter rows the candidates need (all shards of a sharded scope). Scopes with no room are excluded before any write (node saturation cache plus this read); for each remaining concurrency counter row the claim computes `k` = the slots it wants there, at most `max_tasks`. The picks are the first candidates in merge order that fit the plan.
3. **Reserve slots.** For each counter row, in the canonical key order (concurrency rows, then the queued-backlog rows of the picks): concurrency rows take `UPDATE counter SET used = used + k WHERE <key> AND used + k <= limit_value`; backlog rows take their planned `−k`. One row → applied, and the row lock is held to commit. Zero rows on a concurrency row → re-read it, lower `k` to what is free, or drop the scope (`skipped_for_capacity`). The limit is checked inside the `UPDATE`, under the row lock, so it can never be exceeded.
4. **Win the race.** For the picks in **ascending `task_id`** (which tasks are picked is decided by merge order; only the write order is by id): `UPDATE task SET state = claimed, assignee…, lease_token = <random v4>, lease_epoch = lease_epoch + 1, attempt… WHERE task_id = ? AND state = queued` (scoped). One row → won. Zero rows → another claim (or a cancel, move, timeout) won; the claim takes the next candidate of the same scope if its `task_id` is greater than every row it already wrote, otherwise it leaves the slot for a fresh transaction. The loser never sees an error.
5. **Settle.** On the counter rows it already holds — never a new one — the claim returns unused reservations (`used = used − (k − won)`) and corrects the backlog delta to the tasks it actually won. Then it deletes the won tasks' queue-timeout timers, enqueues `TaskClaimed`, and renews the worker sessions last (ADR-0017).
6. **Commit.** Reservation, state changes, backlog, timers, outbox, and sessions commit together or not at all. A database-reported contention error (`toolkit_db::contention::is_retryable_contention`) retries the whole transaction a bounded number of times; for a claim it counts as a lost race.

Nothing is claimed and at least one candidate was skipped for capacity → `429 CONCURRENCY_LIMIT`; nothing eligible → `204`.

### Why it cannot deadlock

The claim's order — counters first, then task rows — becomes the **one global lock order of the engine** (DESIGN §3.7), for every transaction, not only for claims:

1. `te_dependency_locks` (dependency additions only);
2. **counter rows**, all kinds, each kind by key: concurrency (cross-tenant, then per tenant), queued backlog (cross-tenant, then per tenant), event-wait counters — with the deltas the transaction planned from a plain read;
3. **task rows**, ascending `task_id` (cascades and links: parent or predecessor first, then dependents ascending);
4. settlement of deltas on counter rows the transaction **already holds**;
5. worker sessions.

Every transaction follows **plan → counters → guarded task transitions → settle**. Completion, failure, release, liveness requeue, cancel, park, resume, bulk operations, and cascades all read the task (and its recorded shards) first without a lock, apply their counter deltas, and then run the transition guarded on the expected state. If the guard matches zero rows, the task changed since the plan: the transition is skipped and redone in a fresh transaction (single-task calls roll back and report the lost race as today). A transaction never locks a counter row after it has locked a task row, so no cycle can form between counters and tasks; counters among themselves and task rows among themselves are taken in one sorted order. Reconciliation locks one counter row and then counts with a plain read (DESIGN §3.7).

`READ COMMITTED` is still required for claims: a loser re-checks the guard on the latest committed row and moves on instead of failing with a serialization error (PostgreSQL `REPEATABLE READ`) or taking gap locks (InnoDB `REPEATABLE READ`).

Deadlock retry stays only as a safety net for orders the analysis does not cover.

### Throughput

A counter row with a non-zero limit is held from step 3 to commit, so claims that need the same row serialize on it. Without `SKIP LOCKED`, claims on different nodes also read the same head rows; a claim that loses a head row waits for the winner's commit **while it still holds its counter locks**, so other claims wait behind it. Both waits last one short transaction, but together they can make claims on one queue run one after another. The design keeps that cost low:

* **Batch claims**: one transaction claims up to `max_tasks` tasks with one `+k` per counter row.
* **Node-local coalescing**: parked waiters with the same subject, compiled scope, and queue set share one database claim, so database load grows with nodes, not with workers.
* **No queue-wide depth row**: the queue-wide and domain-wide depth limits are sampled ceilings (DESIGN §3.7 Hot-row rules), so claims and creates of a queue do not all lock one shared backlog row. Only the per-tenant backlog row of each claimed task is touched.
* **Hot-row sharding**: a large limit — global or per tenant — is split into shards whose sub-limits sum to at most N; a claim reserves on one shard with room. A busy tenant therefore does not turn its own row into a hot spot.
* **Unlimited means untouched**: a dimension with limit `0` has no counter row and is never locked.

Estimate to be confirmed by the benchmark: a claim transaction of 2–5 ms on PostgreSQL serializes at 200–500 transactions per second per counter shard; with batches of up to 32 and coalescing this is well above the 500 claims/s NFR on one queue.

**Backend acceleration is a ToolKit strategy, not a gear decision.** The Task Engine asks `toolkit-db` ([#5478](https://github.com/constructorfabric/gears-rust/issues/5478)) to select candidates with `FOR UPDATE SKIP LOCKED` on PostgreSQL/MySQL **by default** (as its outbox already does), under the same contract and the same exact order among free rows, so a losing claim skips a taken head row instead of waiting for it. SQLite keeps plain guarded updates (one writer). An opt-in `Spread` strategy (candidate spreading, Option 4) exists only with PRD sign-off on the relaxed order. The Task Engine never names either.

### ToolKit contract

`toolkit_db::secure::claim` (TODO(toolkit), critical path; also intended for Durable Execution) owns:

* the transaction and its isolation level per backend;
* scoped candidate streams (keyset pages and per-key probes) and the k-way merge, with the caller's `AccessScope` on every statement — there is no unscoped claim path;
* guarded counter reservation (`+k`, shard choice, re-plan on refusal), the guarded state transition, settlement of unused reservations, and the canonical lock order;
* contention retry, and metrics (`claims`, `lost_races`, `capacity_skips`, `contention_retries`).

The gear supplies: the candidate streams (entity, index order, filters), the counter keys of a candidate, the transition (columns and guard), extra writes for won rows (timers, outbox, backlog, sessions) through a callback inside the same transaction, and the batch size. Long-poll, waiter groups, and coalescing stay in the gear.

### Consequences

* The Task Engine contains no `SKIP LOCKED`, no `FOR UPDATE` on claims, and no dialect SQL; only the secure claim API.
* The claim protocol and its correctness proof are reviewed once, in `toolkit-db`, and reused by other gears.
* No double claim and no exceeded limit on any backend; no task-row write for a scope that is full.
* Claim order is exact among claimable rows across queues and independent of the order the worker lists them.
* MySQL/MariaDB claim transactions run at `READ COMMITTED`.
* The engine-wide lock order changes from "task rows, then counters" to "counters, then task rows" (plan → counters → transition → settle) for every transaction; DESIGN §3.7 and the terminal transition (`finalize()`) follow it.
* Per-row serialization is the price of the hard limit; batches, coalescing, and shards bound it, and the benchmark decides whether the ToolKit enables `SKIP LOCKED` internally.
* Until `toolkit_db::secure::claim` exists the Task Engine cannot implement claims; this is a critical-path ToolKit item.
* A cross-domain claim is rejected (`invalid_argument`); all queues of one claim are in one database (ADR-0010).

### Confirmation

Tests MUST check, on PostgreSQL, MySQL/MariaDB, and SQLite, that:

* concurrent claims never assign one task twice;
* no concurrency dimension ever exceeds its limit under concurrent claims, releases, and reconciliation;
* the claimed task is the first eligible task in the PRD §5.2 order across all requested queues, regardless of queue order;
* a full scope causes no task-row write, and the claim moves on to other tenants and types;
* reservation, state changes, backlog, timers, outbox, and sessions commit or roll back together;
* concurrent claims and completions on the same queue never deadlock (no `40P01` / `1213` under the stress test).

Benchmark (exit criterion for `accepted`): PostgreSQL and MySQL 8 × {1, 10, 100, 500} workers × {1, 10} queues × {`fifo`, `round_robin`} × `max_tasks` {1, 16} × queue limit {0, 100, 10 000} × engine nodes {1, 3}, with the per-tenant queue-depth limits **enabled** and creates running at the same time as claims; plus one run where a single tenant holds 100K `queued` tasks at its `per_tenant_concurrency_limit`, and one where a single tenant does all creates and claims (the platform-tenant case). Record p50/p99 claim latency, claims/s, lost races per claim, counter-row wait time, and contention retries. **Pass:** `cpt-cf-task-engine-nfr-latency` and `cpt-cf-task-engine-nfr-throughput` on PostgreSQL with ≥ 100 workers on one queue, and claims/s do not drop from 10 to 100 workers.

## Pros and Cons of the Options

### Option 1: Gear-level SELECT ... FOR UPDATE SKIP LOCKED

* Good, because claimers spread over the head rows (River, Oban, graphile-worker, pgmq).
* Bad, because the gear must lock task rows before counters, so a full counter is found only after rows were locked.
* Bad, because it needs a new, gear-visible locking API and puts backend behavior into the gear.
* Neutral, because the same speed-up remains available as an internal ToolKit strategy under Option 5.

**Rejected as a gear-level mechanism.**

### Option 2: Leader-serialized claims

* Good, because ordering is trivially exact.
* Bad, because throughput is capped by one node and failover adds claim downtime.

**Rejected.**

### Option 3: Gear-level optimistic guarded update, task row first

* Good, because it works on every backend through the secure ORM.
* Bad, because a full counter is discovered after the task row was written, so every capacity skip is a rolled-back write on the hottest table.
* Bad, because the protocol and its proof would be re-implemented by every gear that claims work.

**Rejected.**

### Option 4: Candidate spreading without row locks

* Good, because most guarded updates succeed on the first try.
* Bad, because claim order is no longer exact (up to `k − 1` tasks).

**Rejected as default; opt-in ToolKit strategy only with PRD sign-off.**

### Option 5: ToolKit secure claim primitive, counter-first guarded updates

* Good, because the limit is checked inside one guarded `UPDATE` before any task write.
* Good, because it is deadlock-free by construction and needs no locking reads.
* Good, because it is portable (PostgreSQL, MySQL/MariaDB, SQLite) and the gear stays free of dialect SQL.
* Good, because the protocol is reviewed and tested once and reused by other gears.
* Neutral, because the ToolKit may later accelerate candidate selection internally without changing the contract.
* Bad, because claims that need the same counter row serialize on it for one transaction; batches, coalescing, and shards bound the cost.
* Bad, because it adds a critical-path ToolKit dependency.

**Chosen.**

## More Information

```text
BEGIN (READ COMMITTED)
1. candidates : per queue, plain scoped keyset read in claim order; k-way merge by (urgency, created_at, task_id)
2. plan       : plain read of the candidates' counter rows; drop full scopes; picks in merge order
3. reserve    : UPDATE counter SET used = used + k WHERE key AND used + k <= limit   (sorted keys; lock held)
                backlog rows of the picks: used -= k                                   (sorted keys; lock held)
4. win        : picks in ascending task_id:
                UPDATE task SET state = claimed, lease... WHERE id = ? AND state = queued
                (0 rows -> next candidate with a greater task_id, else leave it to a new transaction)
5. settle     : on held rows only: used -= (k - won); backlog += (k - won); timers; outbox TaskClaimed; renew sessions
COMMIT        (contention error -> bounded retry of the whole transaction)
```

See DESIGN §3.7 Lock ordering and §4.6 Task Assignment.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-assignment` — atomic claim with exact cross-queue order and batch claims.
* `cpt-cf-task-engine-fr-priority-dispatch` — claim order: urgency, tenant fairness, tenant-local priority.
* `cpt-cf-task-engine-fr-tenant-fairness` — scoped round-robin candidate streams.
* `cpt-cf-task-engine-fr-lease-fencing` — lease token and epoch set in the guarded transition.
* `cpt-cf-task-engine-fr-queue-concurrency` — guarded slot reservation before any task write.
* `cpt-cf-task-engine-nfr-latency` — claim latency targets.
* `cpt-cf-task-engine-nfr-throughput` — batch claims, coalescing, sharded counters.
