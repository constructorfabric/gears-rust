---
status: accepted
date: 2026-10-24
description: "Fence every worker write with a random per-lease token that is bound to the lease holder's subject, and give workers a per-task lease epoch, so neither a worker that lost its lease nor anyone holding a copied token can change the task, and external systems can reject stale writes."
---

# Fence Worker Writes with a Holder-Bound Lease Token and a Lease Epoch

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
  - [Lease token alone](#lease-token-alone)
  - [Holder-bound lease token plus lease epoch](#holder-bound-lease-token-plus-lease-epoch)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-lease-fencing`

## Context and Problem Statement

A worker holds a task through a lease. A worker can stall past its lease (long GC pause, network split) and then wake up. By then the engine may have given the task to another worker. If the old worker can still complete, checkpoint, or report progress, the task runs twice and the second worker's result can be overwritten.

A token alone fixes that, but it turns into a password: anyone with the `claim` action on the queue who learns a live token — from a log line, an administrator's hand-off, or a compromised worker of another principal — could complete or overwrite the task.

How does the engine make sure that only the current lease holder can change a task, and how can workers protect the external systems they write to?

## Decision Drivers

* At most one completion is accepted per task.
* A stale worker must learn quickly and clearly that it lost the task.
* The same worker may claim the same task again after losing it, so the check cannot rely on worker identity alone.
* A token that leaks must not let anyone else act on the task.
* External systems need a value that grows with each new lease, to reject old writes.
* The check must be cheap: it runs on every heartbeat.

## Considered Options

* Check the worker identity only
* Check the task state only
* Lease token alone
* Holder-bound lease token plus lease epoch

## Decision Outcome

Chosen option: "Holder-bound lease token plus lease epoch", because the token makes stale engine writes impossible with one indexed comparison, the subject binding makes a copied token worthless, and the epoch gives external systems a monotonic fencing value that the engine itself cannot provide.

How it works:

* Every claim creates a new **lease token**: a random UUID v4 from a cryptographically secure generator (122 random bits, no timestamp or counter part), unique per lease. It is returned only to the claiming worker, never logged, and never put in an event.
* Every claim and every reassignment adds one to the task's **lease epoch**, which never decreases.
* The lease belongs to the subject that received it (`assignee_subject`). Every worker call (start, heartbeat, progress, checkpoint, complete, release, set wait, worker entries) carries the token, and the engine applies the change only if `lease_token` matches **and** `assignee_subject` equals the caller's subject, in the same guarded update as the change.
* A mismatch of either returns `LEASE_LOST`; the two cases are indistinguishable. It is final: the worker must stop and must not retry the call.
* A lost token never becomes valid again: expiry, requeue, retry, reassignment, park, and forced cancel all clear it.
* **Reassignment hands over no token.** The administrator names the new identity (subject and `worker_id`); the engine records it as the assignee, clears the token, and increases the epoch. The named worker then calls **accept** under its own credentials and receives a fresh token. An accept window that runs out is handled like an expired lease, without a blacklist strike for either worker.
* A repeated completion by the holder with the same token and the same result returns the stored outcome, so a network retry is not reported as lease loss.
* Workers pass the epoch to external systems that support fencing.

### Consequences

* Every worker request and SDK call must carry the lease. The SDK keeps it in a `Lease` handle so it cannot be left out; the handle works only under the `SecurityContext` that obtained it, and its `Debug` output redacts the token.
* A worker fleet that shares one principal can still pass a task between its own processes only through release and claim, not by copying a token to another principal.
* HTTP retry middleware must never retry `LEASE_LOST`, even though its error category is normally retryable.
* Progress updates buffered in memory are accepted only from the holder, carry their token, and are dropped if the lease changed before they are written.
* Execution is still at-least-once. Workers must make external side effects idempotent, keyed on the task ID, not on the attempt or the epoch.
* Authorization still runs first; the fence decides only which authorized caller is the holder.

### Confirmation

* Tests check that after a lease expires and the task is claimed again, every call from the old worker returns `LEASE_LOST` and the new worker's result is kept.
* Tests check that a valid token presented by another subject returns `LEASE_LOST` and changes nothing, also when that subject has `claim` on the task.
* Tests check that a reassignment response carries no token, that only the named identity can accept, and that the old holder gets `LEASE_LOST`.
* Tests check that the epoch increases on every claim and reassignment.
* Tests check that tokens contain no timestamp or counter part (statistical test over generated tokens) and never appear in logs or events.
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

### Lease token alone

Accept a write if it carries the current token, whoever sends it.

* Good, because a stale write is rejected by one comparison in the update itself.
* Good, because it matches the receipt-handle model of SQS.
* Bad, because the token becomes a bearer credential: a leaked token lets any principal with `claim` act as the holder.
* Bad, because reassignment has to hand a live token to an administrator, who passes it on outside the engine.

### Holder-bound lease token plus lease epoch

* Good, because a stale write is rejected by one comparison in the update itself, as with the token alone.
* Good, because a token is useless without the holder's own credentials, so leaks and hand-offs carry no risk.
* Good, because it matches the lock-token model of Azure Service Bus and Temporal, where the lock belongs to the receiving session.
* Good, because the epoch lets workers fence external writes.
* Bad, because every worker call must carry the token, which adds a field to the API.
* Bad, because reassignment needs an extra accept call by the new holder.

## More Information

The epoch follows the fencing-token pattern described by Kleppmann and used by Kafka member epochs. UUID v7 is deliberately not used for tokens: its leading 48 bits are a timestamp and some generators replace random bits with a counter, so it is not unguessable.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-lease-fencing` — holder-bound token on every worker write, `LEASE_LOST` on mismatch.
* `cpt-cf-task-engine-fr-execution-semantics` — at-least-once execution with fenced task state.
* `cpt-cf-task-engine-fr-assignment` — each claim issues a new token and epoch to the claiming subject.
* `cpt-cf-task-engine-fr-reassignment` — reassignment fences the previous holder and hands over no token; the new holder accepts.
* `cpt-cf-task-engine-fr-heartbeat` — heartbeat is a fenced lease renewal.
* `cpt-cf-task-engine-fr-release` — release is a fenced write.
* `cpt-cf-task-engine-design-sdk-security` — the SDK `Lease` handle.
