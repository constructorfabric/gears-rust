---
status: accepted
date: 2026-10-25
description: "Keep task leases alive with one heartbeat row per worker process and domain (a worker session) instead of one write per task per heartbeat; keep per-task lease tokens for fencing."
---

# Renew Leases per Worker Session, not per Task

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Per-task lease column renewed on every heartbeat](#per-task-lease-column-renewed-on-every-heartbeat)
  - [Per-task lease, renewed lazily](#per-task-lease-renewed-lazily)
  - [Worker-session lease](#worker-session-lease)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-worker-session-leases`

## Context and Problem Statement

Until now every heartbeat wrote `te_task_state.lease_until` for each task. At the capacity profile (100K running tasks, a heartbeat every 10 s) that is about 10K row updates per second on the hottest table. `lease_until` is indexed, so on PostgreSQL none of these updates can be done in place (no HOT update): each one also writes index entries and leaves a dead row version, which drives bloat and vacuum load. Batch heartbeat reduces HTTP requests, but not row writes.

Most job systems (Sidekiq, Celery, Oban, Hatchet) track liveness per worker process, not per job. How do we cut heartbeat writes without weakening fencing?

## Decision Drivers

* Heartbeat writes must grow with the number of worker processes, not with the number of running tasks.
* Fencing stays per task: a stale worker must still get `LEASE_LOST` (ADR-0011).
* A lost worker process must still be detected within one lease duration.
* Losing one process counts as one outage for the blacklist (PRD §5.6).
* No new infrastructure; works on all supported databases.

## Considered Options

* Per-task lease column renewed on every heartbeat (current)
* Per-task lease, renewed lazily (write only when less than half the lease remains)
* Worker-session lease

## Decision Outcome

Chosen option: "Worker-session lease", because heartbeat writes drop from one per task to one per worker process, and per-task fencing does not change.

How it works:

* **Session.** A worker session is one row in `te_worker_sessions` in the task's domain database: (`session_id`, `worker_subject`, `worker_id`, `lease_secs`, `lease_until`, `status`). One worker process has one session per domain. The SDK keeps the `session_id`.
* **Open.** A claim without a `session_id` (or with one that is unknown, expired, or owned by another subject) opens a new session and returns its ID. A restarted process therefore always gets a new session; its old session expires on its own.
* **Claim.** Each claimed task stores its `session_id`. The claim transaction also renews the session and lowers `lease_secs` to the task's effective lease if that is shorter. A session's lease never grows, so it is always as short as the shortest lease of any task it held.
* **Heartbeat.** One guarded update renews the session: `lease_until = now + lease_secs WHERE session_id = ? AND worker_subject = caller AND status = active`. The engine then checks the listed tasks with one read (token, holder, state) and returns `ok` with control signals, or `LEASE_LOST`, per task. Nothing is written to `te_task_state`.
* **Fencing.** Unchanged (ADR-0011): every worker write needs the task's `lease_token` from its holder.
* **Expiry.** The session scan finds `status = active AND lease_until < now`. Expiry is two steps: (1) a guarded update marks the session `expired` (a heartbeat that committed first wins, because the guard repeats `lease_until < now`); (2) the session's tasks are requeued in bounded batches. Each requeue is the normal liveness-failure transition. Step 1 locks only the session row, so it cannot deadlock with a claim.
* **Blacklist.** A session expiry is one outage: the existing coalescing rule records one liveness failure per (worker, queue).
* **Per-task deadlines stay per task.** Assignment and execution timeouts are still columns on `te_task_state`, written only at claim and start.
* **Hand-off.** Reassignment detaches the task from the old session and sets a per-task accept deadline (`handoff_until`). Accept attaches it to the new holder's session.

### Consequences

* Heartbeat writes become about `worker processes × domains ÷ heartbeat interval`. Example: 100K running tasks over 2,000 processes at a 10 s interval is about 200 writes/s instead of 10K. A process that runs one task at a time gains nothing, so worker SDKs should run many tasks per process.
* **Behavior change:** a heartbeat renews every lease of the session. A task stays leased while its worker process is alive, even if its handler hangs. The execution timeout bounds that case, so **every task has one**: when neither the request, the type, nor the queue sets it, the engine default (`default_execution_timeout_secs`) applies, and every value is capped by `trust_limits` (PRD §5.3, §5.12). A worker that drops a task must release it.
* All tasks of one session expire together, which matches how a process actually fails.
* `te_task_state.lease_until` and `heartbeat_at` are removed. New columns: `session_id` and `handoff_until`.
* `te_worker_sessions` is small (one row per process and domain) and is reached only with the caller's subject as a predicate (an `unrestricted` table, like `te_worker_blacklist`).
* Empty sessions expire silently and are deleted by maintenance.

### Confirmation

Tests MUST check that:

* a heartbeat writes one session row and no `te_task_state` row;
* when a worker stops heartbeating, all its tasks are requeued within one lease duration plus one scan interval;
* a worker that loses many tasks with one session gets exactly one blacklist strike per queue;
* after expiry, every call from the old worker returns `LEASE_LOST`, and a late heartbeat does not revive the session;
* a session is never renewed or used by another subject;
* a task with a shorter lease shortens its session's lease;
* a reassigned task is detached from the old session and expires on `handoff_until` if not accepted.

## Pros and Cons of the Options

### Per-task lease column renewed on every heartbeat

* Good, because each task's liveness is tracked on its own.
* Bad, because heartbeat writes grow with running tasks (about 10K/s at the profile), on an indexed column of the hottest table.

### Per-task lease, renewed lazily

* Good, because it is a small change.
* Bad, because it only halves the writes; they still grow with running tasks.

### Worker-session lease

* Good, because writes grow with worker processes, not tasks.
* Good, because one session expiry is naturally one outage.
* Good, because fencing is unchanged.
* Neutral, because a hung handler in a live process keeps its lease until the execution timeout, which every task has and which is capped by the trust limits.
* Bad, because there is one more table and one more scan.

## More Information

```text
te_worker_sessions : session_id PK, worker_subject, worker_id, lease_secs, lease_until, status, bg_slot
heartbeat          : UPDATE te_worker_sessions SET lease_until = now + lease_secs
                     WHERE session_id = ? AND worker_subject = ? AND status = active
expiry             : (1) mark session expired  (2) requeue its tasks in batches (liveness failure)
```

See DESIGN §3.2 TimerService, §3.7 `te_worker_sessions`, and §4.6 Timer Wheel (session scan).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-heartbeat` — a heartbeat renews the leases of the worker session.
* `cpt-cf-task-engine-fr-worker-blacklist` — one session expiry counts as one outage.
* `cpt-cf-task-engine-fr-lease-fencing` — per-task token fencing is unchanged.
* `cpt-cf-task-engine-nfr-timer-accuracy` — lost workers are found by the session scan.
* `cpt-cf-task-engine-nfr-capacity` — heartbeat writes no longer grow with running tasks.
