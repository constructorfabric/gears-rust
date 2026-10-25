---
status: accepted
date: 2026-10-24
description: "Keep non-terminal tasks in a small unpartitioned live table, partition only append-only history by task creation time, measure retention from terminal time, and rescue rows that must outlive a partition before dropping it."
---

# Split Live and History Storage; Apply Retention by Rescue-then-Drop of History Partitions

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Batched DELETE only](#option-1-batched-delete-only)
  - [Option 2: Partition every task table by created_at, drop at the longest policy](#option-2-partition-every-task-table-by-created_at-drop-at-the-longest-policy)
  - [Option 3: Separate database for live tasks](#option-3-separate-database-for-live-tasks)
  - [Option 4: Live/history split with rescue-then-drop](#option-4-livehistory-split-with-rescue-then-drop)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-retention-partitioning`

## Context and Problem Statement

At the target capacity (500 million stored tasks, 100-day default retention) the engine must remove large volumes of old data every day. Removing them row by row causes heavy write load, index churn, and table bloat. Retention policies also vary: a global default, per task type, and per tenant (for example legal hold), and tenant policies take precedence.

Two constraints shape the design. First, the hot dispatch set (claim heads, lease scans, heartbeats) must not pay for partitioning: databases such as PostgreSQL have no global indexes, so a partitioned hot table turns every claim and heartbeat into a fan-out over all partitions. Second, tasks can stay non-terminal for a long time (waits, delayed work, no lifetime limit), so a partition that is old enough to drop may still contain data of live tasks.

## Decision Drivers

* Remove old data cheaply at 500 million tasks.
* Keep claim, lease scan, and heartbeat on a small table with global indexes.
* Never remove data of a non-terminal task, or data a longer policy still requires.
* Measure retention from the terminal time, so late completions keep their full retention.
* One tenant's long policy (legal hold) must not block cleanup for everyone else.
* Work on PostgreSQL and MySQL in production, and on SQLite for development.

## Considered Options

1. **Batched DELETE only** — delete expired rows in batches.
2. **Partition every task table by created_at, drop at the longest policy** — partition all tables, including live state, and drop partitions older than the longest effective retention.
3. **Separate database for live tasks** — keep live tasks in another database and move them to the history database when terminal.
4. **Live/history split with rescue-then-drop** — unpartitioned live table for non-terminal tasks; history partitioned by task creation time; retention measured from terminal time; rows that must outlive a partition are copied to small retained tables before the drop.

## Decision Outcome

Chosen option: **Option 4 — Live/history split with rescue-then-drop.**

**Live set.** `te_task_state` holds non-terminal tasks only and is not partitioned. At the terminal transition, in the same transaction, the final state is inserted into `te_task_results` and deleted from `te_task_state` (admin re-enqueue moves it back).

**Live-only side tables.** `te_wait_conditions`, `te_dependencies` (keyed by the successor), and `te_timers` are needed only while their task is non-terminal. They stay unpartitioned and their rows are deleted in the task's terminal transaction, so they are as small as the live set and never need a retention step. Their history survives elsewhere: wait and timer activity in `te_events`, and the predecessor list copied into `te_task_results.predecessors`. The one exception is a `cascade` timer created in the terminal transaction, which is engine work and is deleted when its cascade completes.

**History.** `te_tasks`, `te_task_results`, `te_events`, `te_task_entries`, `te_tags`, and `te_resource_refs` are range-partitioned by **task creation time**, derived from the UUID v7 task id, so all rows of one task share a partition, lookups by id prune to one partition, and one drop removes a task's tags and resource references (up to `max_resource_refs_per_task` rows per task) together with the task.

**Identity tables.** `te_dedup` and `te_supersede_heads` must stay unpartitioned, because their primary keys are global uniqueness constraints and a partitioned key would have to include the partition key. Their rows are narrow and deleted in bounded batches: `te_dedup` rows by their `expires_at` (set at the holder's terminal transition; caller keys expire after a short key window, ADR-0012), `te_supersede_heads` rows by creation-time range.

**No foreign keys between task tables.** PostgreSQL requires a foreign key into a partitioned table to include the partition key, MySQL/InnoDB supports no foreign keys on partitioned tables, and rescue moves rows to `_retained` tables a foreign key could not follow. Every row that refers to a task is written and removed in the same transaction as that task's transitions, and the maintenance reconciliation reports orphans as a metric.

**Retention clock.** A task's data expires at `terminal_at + effective_retention` (tenant → type → global). Non-terminal tasks never expire by retention; they are bounded by their lifetime timeout and by `hard_retention_days`.

**Rescue, then drop.** A singleton maintenance loop per domain (advisory lock `te-maintenance`) provisions future partitions, force-cancels tasks beyond `hard_retention_days`, and for each partition older than `drop_horizon_days` (default = global retention) copies into `te_*_retained` tables every row of a task that is still non-terminal or whose `terminal_at + effective_retention` is in the future. It then drops the partition range in every partitioned table. Retained rows, policies shorter than the drop horizon, and the identity rows of dropped tasks are removed by bounded `DELETE`.

**Large long-retention workloads.** A tenant or type whose long policy covers a large share of the data is routed to its own task domain with a matching `drop_horizon_days`, so rescue sets stay small everywhere.

**SQLite.** No partitioning; the same tables are unpartitioned and retention uses bounded `DELETE` with the same terminal-time clock.

### Consequences

* Claim, lease scan, and heartbeat run on a small unpartitioned table, independent of history size.
* Daily retention at the capacity target costs a few partition drops plus a small rescue copy.
* Live tasks and data under longer policies are never lost by a drop.
* A late completion keeps its full retention.
* One tenant's legal hold no longer blocks cleanup for the domain.
* Dedup windows are at most the retention; `te_dedup` stays unpartitioned, holds only live tasks and identities still in their window, and is cleaned by `expires_at` in bounded batches.
* No table that grows with stored history needs row-by-row deletion except the narrow identity tables.
* The terminal transition writes one extra row (move to `te_task_results`) and deletes the task's live-only rows.
* After a task ends, its wait conditions and dependency rows are no longer listable as rows; they are read from `te_events` and `te_task_results.predecessors`.
* Integrity between task tables is enforced by the engine's transactions, not by database foreign keys.
* History reads union the partitioned table with its retained table.

### Confirmation

Tests MUST check that:

* no partition drop removes data of a non-terminal task;
* a task completing late keeps its full retention from `terminal_at`;
* a tenant with a long policy is rescued and does not block drops for other tenants;
* claim and heartbeat latency are independent of history volume;
* `te_dedup` and `te_supersede_heads` rows are removed with their tasks;
* a partition drop also removes the dropped tasks' tags and resource references, and rescue keeps those of rescued tasks;
* a terminal transition leaves no `te_wait_conditions`, `te_dependencies`, or `te_timers` rows for the task (except an in-progress `cascade` timer);
* the migrations contain no foreign key between task tables on any backend, and reconciliation reports injected orphan rows;
* the SQLite fallback removes the same set of data as the partitioned path.

## Pros and Cons of the Options

### Option 1: Batched DELETE only

A background loop deletes terminal tasks older than their retention.

* Good, because it is simple and portable.
* Bad, because deleting tens of millions of rows a day causes write amplification, index churn, and bloat.

**Rejected.**

### Option 2: Partition every task table by created_at, drop at the longest policy

The original design.

* Good, because drops are cheap.
* Bad, because the hot state table becomes partitioned and every claim and heartbeat fans out over all partitions.
* Bad, because partitions holding non-terminal tasks can be dropped.
* Bad, because one long policy (legal hold) moves the drop horizon for the whole domain, so nothing is ever dropped.

**Rejected.**

### Option 3: Separate database for live tasks

Physical separation of hot and cold data.

* Good, because hot and cold storage scale independently.
* Bad, because the terminal transition, its events, and its outbox write can no longer commit in one transaction (contradicts ADR-0001).

**Rejected.**

### Option 4: Live/history split with rescue-then-drop

Same database, separate tables, so every transition stays atomic.

* Good, because the hot path touches one small table with global indexes.
* Good, because dropping a partition is cheap and never loses needed data.
* Good, because the drop horizon is the common retention, independent of long policies.
* Neutral, because the terminal transition moves one row from the live table to history.
* Bad, because reads of history union a partitioned table with a small retained table.

**Chosen.**

## More Information

```text
live      : te_task_state (non-terminal, unpartitioned)
live-only : te_wait_conditions, te_dependencies, te_timers (unpartitioned, deleted at the terminal transition)
history   : te_tasks, te_task_results, te_events, te_task_entries, te_tags, te_resource_refs
            (partitioned by task creation time)
identity  : te_dedup, te_supersede_heads (unpartitioned, bounded batch deletes)
retained  : te_*_retained (unpartitioned, rescued rows only)
expiry    : terminal_at + effective_retention
drop      : partition range older than drop_horizon_days, after rescue
FKs       : none between task tables
```

See DESIGN §3.7 Live / history split, §3.2 RetentionService, and §4.1 Configuration (`retention`).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-retention` — terminal-time retention with tenant, type, and global policies.
* `cpt-cf-task-engine-fr-idempotent-create` — uniqueness kept in the non-partitioned `te_dedup`.
* `cpt-cf-task-engine-fr-derived-dedup` — dedup identities removed with their tasks.
* `cpt-cf-task-engine-nfr-latency` — hot path on a small unpartitioned table.
* `cpt-cf-task-engine-nfr-capacity` — cheap removal at 500 million stored tasks.
