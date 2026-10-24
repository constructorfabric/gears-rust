---
status: accepted
date: 2026-10-24
description: "Fence every worker write with a per-lease token and give workers a per-task lease epoch, so a worker that lost its lease can never change the task, and external systems can reject its writes."
---

# Fence Worker Writes with a Lease Token and a Lease Epoch

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Check the worker identity only](#check-the-worker-identity-only)
  - [Check the task state only](#check-the-task-state-only)
  - [Lease token for the engine plus lease epoch for external systems](#lease-token-for-the-engine-plus-lease-epoch-for-external-systems)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-lease-fencing`

## Context and Problem Statement

A worker holds a task through a lease. A worker can stall past its lease (long GC pause, network split) and then wake up. By then the engine may have given the task to another worker. If the old worker can still complete, checkpoint, or report progress, the task runs twice and the second worker's result can be overwritten.

How does the engine make sure that only the current lease holder can change a task, and how can workers protect the external systems they write to?

## Decision Drivers

* At most one completion is accepted per task.
* A stale worker must learn quickly and clearly that it lost the task.
* The same worker may claim the same task again after losing it, so the check cannot rely on worker identity.
* External systems need a value that grows with each new lease, to reject old writes.
* The check must be cheap: it runs on every heartbeat.

## Considered Options

* Check the worker identity only
* Check the task state only
* Lease token for the engine plus lease epoch for external systems

## Decision Outcome

Chosen option: "Lease token for the engine plus lease epoch for external systems", because the token makes stale engine writes impossible with one indexed comparison, and the epoch gives external systems a monotonic fencing value that the engine itself cannot provide.

How it works:

* Every lease grant (claim, reassignment) creates a new **lease token** (opaque, unguessable, unique per lease) and adds one to the task's **lease epoch** (never decreases).
* Every worker call (start, heartbeat, progress, checkpoint, complete, release, set wait, worker entries) carries the token. The engine applies the change only if the token matches the current lease, in the same guarded update as the change.
* A mismatch returns `LEASE_LOST`. It is final: the worker must stop and must not retry the call.
* A lost token never becomes valid again: expiry, requeue, retry, reassignment, park, and forced cancel all clear it.
* A repeated completion with the same token and the same result returns the stored outcome, so a network retry is not reported as lease loss.
* Workers pass the epoch to external systems that support fencing.

### Consequences

* Every worker request and SDK call must carry the lease. The SDK keeps it in a `Lease` handle so it cannot be left out.
* HTTP retry middleware must never retry `LEASE_LOST`, even though its error category is normally retryable.
* Progress updates buffered in memory carry their token and are dropped if the lease changed before they are written.
* Execution is still at-least-once. Workers must make external side effects idempotent, keyed on the task ID, not on the attempt or the epoch.
* The token is a correctness check, not a credential. Authorization still runs first.

### Confirmation

* Tests check that after a lease expires and the task is claimed again, every call from the old worker returns `LEASE_LOST` and the new worker's result is kept.
* Tests check that the epoch increases on every claim and reassignment.
* Tests check that a progress update buffered under a lost lease is never written.

## Pros and Cons of the Options

### Check the worker identity only

Accept a write if it comes from the assigned worker.

* Good, because it needs no extra field.
* Bad, because the same worker can lose a task and claim it again, so an old request from the first claim would still pass.
* Bad, because many processes may share one identity if workers set it badly.

### Check the task state only

Accept a write if the task is still `claimed` or `running`.

* Good, because it is simple.
* Bad, because after a requeue and a new claim the state is `running` again, so a stale worker's write passes.

### Lease token for the engine plus lease epoch for external systems

* Good, because a stale write is rejected by one comparison in the update itself.
* Good, because it matches the receipt-handle or lock-token model of SQS, Azure Service Bus, and Temporal.
* Good, because the epoch lets workers fence external writes.
* Bad, because every worker call must carry the token, which adds a field to the API.

## More Information

The epoch follows the fencing-token pattern described by Kleppmann and used by Kafka member epochs.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-lease-fencing` — token on every worker write, `LEASE_LOST` on mismatch.
* `cpt-cf-task-engine-fr-execution-semantics` — at-least-once execution with fenced task state.
* `cpt-cf-task-engine-fr-assignment` — each claim issues a new token and epoch.
* `cpt-cf-task-engine-fr-reassignment` — reassignment fences the previous holder.
* `cpt-cf-task-engine-fr-heartbeat` — heartbeat is a fenced lease renewal.
* `cpt-cf-task-engine-fr-release` — release is a fenced write.
* `cpt-cf-task-engine-design-sdk-security` — the SDK `Lease` handle.
