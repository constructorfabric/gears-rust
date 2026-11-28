# Queue operations

**Status:** Draft · [All proposals](README.md)

## Problem

Operators have no way to stop work from starting without cancelling it.

- **During an incident or maintenance** there is no pause: a depth limit of `0` means unlimited (PRD.md:447), and the consumer's park, which could hold single tasks, goes with [Task mutability](01-task-mutability.md).

## Proposal

An admin can hold a queue. While it is held, creates are still accepted, but no queued task of that queue is claimed, so no new work starts. Tasks that are already claimed or running finish normally; a hold never takes a task from its worker. Releasing the hold, by hand or at a set time, lets claims resume.

- [Proposed] **Admin queue hold.** An administrator can stop claims on one queue without stopping creates.
  - **Hold and release.** `POST /admin/queues/{id}/hold` holds one queue, with an optional `reason` and an optional `until` time for automatic release. `POST /admin/queues/{id}/release` ends it. Both need the queue `configure` action (PRD.md:923). The hold is stored in the queue's domain, so it applies to every engine node within a short refresh interval.
  - **What a held queue does.** Creates are accepted and admitted as usual, including depth limits ([Limits and counters](04-limits-and-counters.md)), so a long hold pushes back on producers with `429` once the queue is full. Claims skip the held queue's tasks and keep serving the other queues they name. Claimed and running tasks keep their leases and finish. Timers keep running: scheduled tasks become queued and resumed waits return to the queue, where they also stay held.
  - **Visible and recorded.** `GET /queues` and `GET /queues/{id}` show whether a queue is held, since when, by whom, why, and until when. Holding and releasing are recorded as queue-scoped events, like `configUpdated` (DESIGN.md:875).

### API changes

- **Add** `POST /admin/queues/{id}/hold` (body: optional `reason`, optional `until`).
- **Add** `POST /admin/queues/{id}/release`.
- **Change** `GET /queues` and `GET /queues/{id}` to include the hold state.
- **Add** two queue-scoped event types, for example `queueHeld` and `queueReleased`.

## What we give up

- Nothing new: the hold is an added operation. It does bring back head-of-line blocking for the held queue, on purpose (PRD.md:427 rules it out for limits only).

## Open questions

- [Open] **Queue timeouts while held: should held time count?** A queue timeout ends a task that "waited in `queued` too long" (PRD.md:517).
  - **Leaning:** no. Queue timeouts do not fire for a held queue; lifetime timeouts still do.

## Doc changes

- PRD §5.2 (new requirement for the queue hold), PRD.md:923 (`configure` covers hold and release).
- DESIGN §3.3 API table (new endpoints), the claim path (skip held queues, around DESIGN.md:2433), the event list (DESIGN.md:875).
- `schemas/types/`: the two new queue event types.
