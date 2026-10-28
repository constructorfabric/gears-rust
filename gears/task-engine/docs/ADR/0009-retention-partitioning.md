---
status: accepted
date: 2026-10-25
description: "Keep non-terminal tasks in a small unpartitioned live table, store append-only history in a fixed ring of N list partitions keyed by a time-bucket slot, measure retention from terminal time, and recycle the oldest slot by rescue-then-truncate."
---

# Split Live and History Storage; Keep History in a Fixed Partition Ring (Rescue-then-Truncate)

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The ring](#the-ring)
  - [Recycling a slot](#recycling-a-slot)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Bounded prefix DELETE only](#option-1-bounded-prefix-delete-only)
  - [Option 2: Partition every task table by created_at, drop at the longest policy](#option-2-partition-every-task-table-by-created_at-drop-at-the-longest-policy)
  - [Option 3: Separate database for live tasks](#option-3-separate-database-for-live-tasks)
  - [Option 4: Live/history split with time-range partitions created ahead and dropped](#option-4-livehistory-split-with-time-range-partitions-created-ahead-and-dropped)
  - [Option 5: Live/history split with a fixed partition ring, rescue-then-truncate](#option-5-livehistory-split-with-a-fixed-partition-ring-rescue-then-truncate)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-retention-partitioning`

## Context and Problem Statement

At the target capacity (500 million stored tasks, 100-day default retention) the engine must remove large volumes of old data every day. Removing them row by row causes heavy write load, index churn, and table bloat. Retention policies also vary: a global default, per task type, and per tenant (for example legal hold), and tenant policies take precedence.

Three constraints shape the design. First, the hot dispatch set (claim heads, deadline scans) must not pay for partitioning: PostgreSQL has no global indexes, so a partitioned hot table turns every claim into a fan-out over all partitions. Second, tasks can stay non-terminal for a long time (waits, delayed work, no lifetime limit), so storage that is old enough to remove may still hold data of live tasks. Third, the gear has no dialect SQL and ToolKit has no partition-management API: creating and dropping partitions at runtime would be dialect-specific DDL on the hot maintenance path, and the number of partitions would grow with configuration.

The Event Broker solves the same "bounded history" problem for its event log with a sliding window: a paced retention worker computes the bounds per tick, and the storage backend removes a **prefix** of each `(topic, partition)` log in bounded passes (`maintain()`, at most 4,096 rows per pass), so what survives is always a suffix and a failed pass simply leaves the work for the next tick. The Task Engine adopts the same window semantics and driving model, but at its volume it removes whole physical slots instead of rows.

## Decision Drivers

* Remove old data cheaply at 500 million tasks.
* Keep claims and deadline scans on a small table with global indexes.
* Never remove data of a non-terminal task, or data a longer policy still requires.
* Measure retention from the terminal time, so late completions keep their full retention.
* One tenant's long policy (legal hold) must not block cleanup for everyone else.
* No runtime `CREATE` / `DROP` partition DDL; a fixed, bounded number of partitions.
* Work on PostgreSQL and MySQL in production, and on SQLite for development.

## Considered Options

1. **Bounded prefix DELETE only** — the Event Broker backend pattern: delete the oldest rows in bounded passes.
2. **Partition every task table by created_at, drop at the longest policy** — partition all tables, including live state.
3. **Separate database for live tasks** — move tasks to a history database when terminal.
4. **Live/history split with time-range partitions created ahead and dropped** — unpartitioned live table; history range-partitioned by creation time; future partitions provisioned, old ones rescued and dropped.
5. **Live/history split with a fixed partition ring, rescue-then-truncate** — unpartitioned live table; history list-partitioned into exactly `N` slots keyed by a time bucket; the slot about to be reused is rescued and truncated.

## Decision Outcome

Chosen option: **Option 5 — Live/history split with a fixed partition ring, rescue-then-truncate.**

**Live set.** `te_task_state` holds non-terminal tasks only and is not partitioned. At the terminal transition, in the same transaction, the final state is inserted into `te_task_results` and deleted from `te_task_state` (admin re-enqueue moves it back).

**Live-only side tables.** `te_wait_conditions`, `te_dependencies` (keyed by the successor), and `te_timers` are needed only while their task is non-terminal. They stay unpartitioned and their rows are deleted in the task's terminal transaction, so they are as small as the live set and never need a retention step. Their history survives elsewhere: wait and timer activity in `te_events`, and the predecessor list copied into `te_task_results.predecessors`. The one exception is a `cascade` timer created in the terminal transaction, which is engine work and is deleted when its cascade completes.

**History.** `te_tasks`, `te_task_results`, `te_events`, `te_task_entries`, `te_tags`, and `te_resource_refs` are stored in the ring (below). All rows of one task share one slot, a lookup by task ID prunes to that slot, and truncating a slot removes a task's tags and resource references together with the task.

**Identity tables.** `te_dedup` and `te_supersede_heads` stay unpartitioned, because their primary keys are global uniqueness constraints. Their rows are narrow and deleted in bounded batches: `te_dedup` by `expires_at` (set at the holder's terminal transition; caller keys expire after a short key window, ADR-0012), `te_supersede_heads` by the creation time of their latest task.

**No foreign keys between task tables.** PostgreSQL requires a foreign key into a partitioned table to include the partition key, MySQL/InnoDB supports no foreign keys on partitioned tables, and rescue moves rows to `_retained` tables a foreign key could not follow. Every row that refers to a task is written and removed in the same transaction as that task's transitions, and the maintenance reconciliation reports orphans as a metric.

**Retention clock.** A task's data expires at `terminal_at + effective_retention` (tenant → type → global). Non-terminal tasks never expire by retention; they are bounded by their lifetime timeout and by `hard_retention_days`.

### The ring

* **Geometry, fixed at installation.** `bucket_days` `W` (default 7) and `ring_slots` `N = ⌈H / W⌉ + 2`, where `H` = `drop_horizon_days` (default = global retention, 100). Defaults: `N = 17`, so every history table has exactly 17 partitions (102 in total).
* **Slot of a row.** `bucket = ⌊days_since_epoch(task_created_at) / W⌋` and `ring_slot = bucket mod N`, computed once in Rust from the task's UUID v7 ID and stored as `ring_slot SMALLINT NOT NULL` on every history row (engine-scoped events without a task use their own event ID). It never changes.
* **Physical layout.** PostgreSQL and MySQL: `PARTITION BY LIST (ring_slot)` with one partition per slot, created once by the initial migration; the primary keys include `ring_slot`. SQLite: the same tables unpartitioned, with `ring_slot` indexed.
* **Pruning.** A lookup by task ID filters `ring_slot = slot_of(id)`; a search with a `created_at` window filters `ring_slot IN (slots covering the window)`. No query fans out to more than `N` partitions.
* **Guarantee.** The slot recycled during bucket `c` is slot `(c + 1) mod N`, which holds bucket `c + 1 − N`. Its newest rows are at least `(N − 2) × W ≥ H` days old, so nothing younger than the drop horizon is ever removed, and the slot is empty a full bucket before it receives new rows.

### Recycling a slot

A singleton maintenance loop per domain (advisory lock `te-maintenance`) drives retention the way the Event Broker's retention worker does: a paced tick computes the bounds, runs bounded, idempotent passes, and reports what it did; a failed pass removes nothing and the next tick repeats it. During bucket `c` it recycles slot `s = (c + 1) mod N`, holding bucket `B = c + 1 − N`:

1. **Mark** `s` as `recycling` for bucket `B` in the domain's `te_ring_state` (one row per slot), in a transaction that locks that row. This is a **fence**, not a timer: every write for a task of an old bucket (`b ≤ c + 2 − N`; the bucket being recycled, older ones, and one bucket of margin for clock differences) locks the same row first, writes to the ring only if the row is `active` for its bucket, and otherwise writes to the `_retained` tables. So the mark waits for writers that already hold the row, and every later writer sees `recycling`. When the mark commits, no new row can land in `s`. Newer tasks skip the check, so the hot path pays nothing.
2. **Hard limit.** Force-cancel non-terminal tasks older than `hard_retention_days` through `finalize()` with `forcibly_cancelled`, in bounded batches.
3. **Rescue.** Copy into `te_*_retained` (batched, idempotent) every row in `s` of a task that must outlive it: the task is still **non-terminal**, or it is terminal and `terminal_at + effective_retention > now` (late completions, longer type policies, tenant legal holds).
4. **Truncate** slot `s` in every history table: PostgreSQL `TRUNCATE` of the partition, MySQL `ALTER TABLE … TRUNCATE PARTITION`, SQLite bounded `DELETE … WHERE ring_slot = s`. Through the ToolKit helper only (below), with a short lock timeout and retry.
5. **Free** `s` for bucket `B + N` in `te_ring_state`.
6. **Expire leftovers.** Bounded `DELETE` of expired `_retained` rows, of rows under policies **shorter** than the drop horizon (by `ring_slot` and `terminal_at`), of `te_dedup` rows with `expires_at < now`, and of `te_supersede_heads` rows of removed tasks.

A task is **retained** once its bucket's slot was recycled; its reads and writes then use the `_retained` tables, and history reads union the partitioned table with its retained table.

**ToolKit dependency.** Two small helpers in `toolkit-db` (TODO(toolkit)): (1) a migration helper that declares a table list-partitioned by a slot column with exactly `N` partitions on PostgreSQL/MySQL and unpartitioned on SQLite; (2) `truncate_partition(table, slot)` with the per-backend statements above. There is no runtime `CREATE PARTITION` or `DROP PARTITION`.

**Large long-retention workloads.** A tenant or type whose long policy covers a large share of the data is routed to its own task domain, whose ring is sized for that policy, so rescue sets stay small everywhere.

### Consequences

* Claims and deadline scans run on a small unpartitioned table, independent of history size.
* Daily retention at the capacity target costs one truncation per history table per `W` days plus a small rescue copy and narrow row deletes.
* The number of partitions is fixed (`N` per table) and known at installation: bounded planning cost, no partition provisioning, no runtime DDL except `TRUNCATE`.
* Live tasks and data under longer policies are never lost by a truncation; writes for retained tasks never target a recycled slot.
* A late completion keeps its full retention; one tenant's legal hold does not block cleanup for the domain.
* Changing `W` or `N` (for example a longer drop horizon) is a schema migration that builds a new ring, not a configuration change.
* Integrity between task tables is enforced by the engine's transactions, not by database foreign keys.
* History reads union the partitioned table with its retained table.

### Confirmation

Tests MUST check that:

* the migrations create exactly `N` partitions per history table on PostgreSQL and MySQL, and no foreign key between task tables on any backend;
* no truncation removes data of a non-terminal task or of a task whose retention has not ended;
* rows written for a task of a `recycling` bucket land in the `_retained` tables, and reads by task ID find them;
* a slot is never written after it was marked `recycling`, including by transactions that started before the mark and by nodes whose cache or clock is out of date;
* a task completing late keeps its full retention from `terminal_at`;
* a tenant with a long policy is rescued and does not block truncation for other tenants;
* claim latency is independent of history volume, and a search with a `created_at` window touches only the slots that cover it;
* a terminal transition leaves no `te_wait_conditions`, `te_dependencies`, or `te_timers` rows for the task (except an in-progress `cascade` timer);
* the SQLite fallback removes the same set of data as the partitioned path.

## Pros and Cons of the Options

### Option 1: Bounded prefix DELETE only

The Event Broker storage backend's approach: delete the oldest rows in bounded passes until the bounds hold.

* Good, because it is simple, portable, and needs no partitioning.
* Good, because the paced-worker, bounded-pass, report-per-tick model is proven on the platform.
* Bad, because at 500 million tasks it deletes tens of millions of rows a day, with write amplification, index churn, and bloat.

**Rejected as the main mechanism; kept for SQLite, for policies shorter than the drop horizon, and for `_retained` rows.**

### Option 2: Partition every task table by created_at, drop at the longest policy

* Good, because drops are cheap.
* Bad, because the hot state table becomes partitioned and every claim fans out over all partitions.
* Bad, because partitions holding non-terminal tasks can be dropped.
* Bad, because one long policy (legal hold) moves the drop horizon for the whole domain.

**Rejected.**

### Option 3: Separate database for live tasks

* Good, because hot and cold storage scale independently.
* Bad, because the terminal transition, its events, and its outbox write can no longer commit in one transaction (contradicts ADR-0001).

**Rejected.**

### Option 4: Live/history split with time-range partitions created ahead and dropped

The previous decision.

* Good, because the hot path touches one small table and drops are cheap.
* Bad, because it needs runtime `CREATE PARTITION` / `DROP PARTITION` DDL per backend, which ToolKit does not provide.
* Bad, because the number of partitions depends on configuration and grows with the partition interval and retention.
* Bad, because a write for a long-lived task whose range was dropped has no partition to land in.

**Rejected.**

### Option 5: Live/history split with a fixed partition ring, rescue-then-truncate

* Good, because the hot path touches one small table with global indexes.
* Good, because the partition count is fixed and all partition DDL runs once, in migrations; recycling is a `TRUNCATE`.
* Good, because the drop horizon is the common retention, independent of long policies, and retained tasks have a defined home for new rows.
* Good, because it follows the Event Broker's sliding-window semantics and paced retention worker.
* Neutral, because changing the ring geometry is a migration.
* Bad, because reads of history union a partitioned table with a small retained table.

**Chosen.**

## More Information

```text
live      : te_task_state (non-terminal, unpartitioned)
live-only : te_wait_conditions, te_dependencies, te_timers (unpartitioned, deleted at the terminal transition)
history   : te_tasks, te_task_results, te_events, te_task_entries, te_tags, te_resource_refs
            (LIST-partitioned by ring_slot; exactly N partitions; SQLite unpartitioned)
ring      : bucket = floor(days(task_created_at) / W); ring_slot = bucket mod N; N = ceil(H / W) + 2 (W = 7, H = 100 -> N = 17)
recycle   : during bucket c, slot (c + 1) mod N: mark recycling -> hard limit -> rescue -> TRUNCATE -> free
identity  : te_dedup, te_supersede_heads (unpartitioned, bounded batch deletes)
retained  : te_*_retained (unpartitioned, rescued rows and new rows of retained tasks)
expiry    : terminal_at + effective_retention
FKs       : none between task tables
```

See DESIGN §3.7 Live / history split, §3.2 RetentionService, and §4.1 Configuration (`retention`). Event Broker reference: `infra/workers/retention.rs` (paced retention worker) and the SQLite backend's `maintain()` (bounded prefix removal).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-retention` — terminal-time retention with tenant, type, and global policies.
* `cpt-cf-task-engine-fr-idempotent-create` — uniqueness kept in the non-partitioned `te_dedup`.
* `cpt-cf-task-engine-fr-derived-dedup` — dedup identities removed with their tasks.
* `cpt-cf-task-engine-nfr-latency` — hot path on a small unpartitioned table.
* `cpt-cf-task-engine-nfr-capacity` — cheap removal at 500 million stored tasks with a fixed partition count.
