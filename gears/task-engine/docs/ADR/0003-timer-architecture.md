---
status: accepted
date: 2026-10-24
description: "Store low-frequency deadlines as durable timer rows processed by a sharded timer wheel, and enforce assignment, heartbeat, and execution deadlines with a lease scan over task-state columns."
---

# Enforce Timeouts with a Persistent, Sharded Timer Wheel and a Lease Scan

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: In-memory timers only](#option-1-in-memory-timers-only)
  - [Option 2: External scheduler](#option-2-external-scheduler)
  - [Option 3: Every deadline as a durable timer row](#option-3-every-deadline-as-a-durable-timer-row)
  - [Option 4: Persistent sharded timer wheel plus lease scan](#option-4-persistent-sharded-timer-wheel-plus-lease-scan)
  - [Option 5: Due-time columns polled by index scans](#option-5-due-time-columns-polled-by-index-scans)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-timer-architecture`

## Context and Problem Statement

The Task Engine enforces many deadlines: queue, assignment, execution, heartbeat, and lifetime timeouts, SLA deadlines, delayed enqueue (`run_after`), wait timeouts, and the graceful-cancel grace period. Deadlines must survive process crashes and restarts, fire within 1 second (PRD timer-accuracy NFR), and keep working with up to 100,000 active tasks on several nodes.

Heartbeats recur every few seconds for every running task. If each heartbeat rewrote a timer row, the timer table would become the hottest table in the system.

## Decision Drivers

* No deadline may be lost on crash or restart.
* Timers fire within 1 second under normal load.
* Timer work must scale horizontally, not be pinned to one leader.
* Heartbeats must not cause timer-row write amplification.
* No dependency on an external scheduler or the Cluster gear.

## Considered Options

1. **In-memory timers only** — keep deadlines in process memory.
2. **External scheduler** — delegate deadlines to an external scheduling service.
3. **Every deadline as a durable timer row** — persist every deadline, including heartbeat, as a `te_timers` row.
4. **Persistent sharded timer wheel plus lease scan** — durable `te_timers` rows for low-frequency absolute deadlines; deadline columns on `te_task_state` enforced by a periodic lease scan for assignment, heartbeat, and execution.
5. **Due-time columns polled by index scans** — no timer table: every deadline is an indexed column on the task-state table, found by sharded range scans, as in River or Oban `scheduled_at`.

## Decision Outcome

Chosen option: **Option 4 — Persistent sharded timer wheel plus lease scan.**

Absolute, low-frequency deadlines are stored as `te_timers` rows. A timer wheel per shard sleeps until the nearest expiry, claims due timers with a guarded update and a claim token, and hands them to N worker tasks that apply the timeout action from the state transition table. Abandoned claims are recovered through `claim_until`.

Assignment and execution deadlines (and the accept window of a hand-off) are persisted as columns on `te_task_state` and enforced by a deadline scan that runs every `lease_scan_interval_ms` (default 1 second). The heartbeat deadline is the worker session's `lease_until` (ADR-0017), enforced by a session scan on the same interval. A heartbeat only renews the session; it can never extend the assignment or execution deadline.

All loops are partitioned into `bg_shards` shards. Every row stores a fixed `bg_slot` (0–1023, computed once in Rust from the task or session ID). Shard `i` of `S` owns a contiguous slot range and is guarded by advisory lock `te-bg-{S}-{i}`. Scans filter on the stored slot with plain comparisons, so no SQL hash function is needed, and changing `bg_shards` moves no data. When an instance dies, its locks are released and another instance takes the shard over.

### Consequences

* No deadline is lost on crash or restart; timers are rebuilt from the database.
* Heartbeats cost one `UPDATE` of a small session row and never write timer rows or `te_task_state` rows (ADR-0017).
* Timer and lease processing scale with the number of nodes when `bg_shards ≥ node_count`; changing `bg_shards` needs no data migration.
* The lifetime and SLA timers survive retries, requeues, and waits; state-specific timers are re-armed on each transition.
* `assignment`, `execution`, and `heartbeat` remain reserved `TimerType` discriminants but never appear as `te_timers` rows.
* `lease_scan_limit` must cover the due rows of one shard per scan interval for the scans to keep up.

### Confirmation

We confirm this decision through capacity tests and failure-mode tests.

Tests MUST check that:

* 99% of timers fire within 1 second of expiry with 10,000 active timers;
* timers survive a process restart and fire afterwards;
* a heartbeat never extends the assignment or execution deadline;
* when a shard-holding instance dies, another instance takes over its shard and its timers still fire;
* during a change of `bg_shards`, every slot keeps an owner and no timer fires twice.

## Pros and Cons of the Options

### Option 1: In-memory timers only

Each node keeps its own in-memory timer heap.

* Good, because it is fast and simple.
* Bad, because deadlines are lost on crash or restart.
* Bad, because a task's timers live on whichever node created them.

**Rejected.**

### Option 2: External scheduler

Register each deadline with a separate scheduler that calls back into the Task Engine.

* Good, because scheduling code is not ours to maintain.
* Bad, because it adds a second source of truth that must be reconciled with task state (contradicts ADR-0001).
* Bad, because it adds a deployment dependency.

**Rejected.**

### Option 3: Every deadline as a durable timer row

All deadlines are rows; claim, start, and every heartbeat delete and insert timer rows.

* Good, because one mechanism covers everything.
* Bad, because heartbeats make `te_timers` the hottest table (DELETE+INSERT every few seconds per running task).

**Rejected.**

### Option 4: Persistent sharded timer wheel plus lease scan

`scheduled`, `lifetime`, `sla`, `wait`, `queue`, and `cancel_grace` deadlines are `te_timers` rows processed by a timer wheel (`tickTimers` + N `procTimers` workers). Assignment and execution deadlines are columns (`assignment_deadline`, `execution_deadline`, plus `handoff_until`) checked by a deadline scan; the heartbeat deadline is the worker session's `lease_until` (ADR-0017), checked by a session scan. All loops are split into `bg_shards` shards over a stored `bg_slot`, each shard guarded by its own advisory lock.

* Good, because every deadline is persisted and rebuilt on restart.
* Good, because a heartbeat is one small session-row `UPDATE` with no timer-row or task-row churn.
* Good, because sharding spreads timer and lease work across nodes.
* Neutral, because there are two mechanisms instead of one.
* Bad, because the scan interval bounds accuracy, so it must be sized against the NFR.

**Chosen.**

### Option 5: Due-time columns polled by index scans

Lifetime, SLA, scheduled, queue, cancel-grace, and wait deadlines become columns or child rows with partial indexes, polled like the lease scan.

* Good, because there is no separate timer table and no timer-row inserts or deletes.
* Good, because it reuses the lease-scan pattern already chosen for heartbeat, assignment, and execution.
* Bad, because it needs one partial index per deadline type on the hottest table.
* Bad, because per-wait-condition timeouts do not map to one column per task.
* Neutral, because it could replace the timer table later for some deadline types without changing the API.

**Rejected for now.**

## More Information

```text
te_timers rows : scheduled, lifetime, sla, wait, queue, cancel_grace
te_task_state  : assignment_deadline, execution_deadline, handoff_until, bg_slot
te_worker_sessions : lease_until (heartbeat, ADR-0017), bg_slot
loops          : tickTimers + procTimers (xN), sessionScan, deadlineScan, per shard (te-bg-{S}-{i})
shard i of S   : bg_slot in [floor(i*1024/S), floor((i+1)*1024/S))
```

See DESIGN §3.2 TimerService and §4.6 Timer Wheel algorithm.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-timeouts` — all timeout types are persisted and rebuilt on restart.
* `cpt-cf-task-engine-fr-delayed-enqueue` — `run_after` is a `scheduled` timer.
* `cpt-cf-task-engine-fr-heartbeat` — heartbeat renews the worker session; lost sessions are found by the session scan (ADR-0017).
* `cpt-cf-task-engine-fr-wait-timer` — wait timeouts are `wait` timers.
* `cpt-cf-task-engine-nfr-timer-accuracy` — timers fire within 1 second.
* `cpt-cf-task-engine-nfr-capacity` — sharded loops keep up with 100,000 active tasks.
