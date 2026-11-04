# Domains

**Status:** Draft · [All proposals](README.md)

## Problem

Moving a queue to another domain stops both its producers and its consumers.

- **A move needs a drain that cannot happen.** Routing is static config (DESIGN.md:1995), and changing a queue's domain fails startup until the queue is drained. But nothing can drain a queue: there is no pause, a depth limit of `0` means unlimited (PRD.md:447), and bulk move cannot cross domains (PRD.md:1162). A rolling deploy keeps creating in the old domain.
- **After the move, things break.** Claims that mix the moved queue with its old neighbours get `400` (PRD.md:655). Search by queue misses the old domain (PRD.md:1186, DESIGN.md:2835). Dead-lettered tasks left behind cannot be re-enqueued (PRD.md:1180). Dedup and supersession are per domain, so a replay or a stale revision gets through.

Domains themselves are p3, but the API and PRD wording are fixed in p1. If p1 assumes a queue's domain never changes, adding moves later is a breaking change.

## Proposal

A queue move is an operation, not an outage: new work goes to the target domain at once, existing work finishes where it is, and producers and workers keep running throughout.

- [Proposed] **Queue-move model.**
  - **Fixed domain:** a task's domain is set at creation and never changes.
  - **Domain set:** a queue has one primary domain, plus the source domain while a move is in progress.
  - **Placement during a move:** a new task goes to the target, unless it must be written in one transaction with a live task in the source (a child of a source parent, a successor of a source predecessor, a replacement through supersession). This keeps PRD.md:1182 (one domain per atomic operation) true.
  - **Precedent:** the dual-worker approach Temporal documents for moving a namespace (new work starts in the target, existing work finishes in the source, workers poll both). Rows are not copied between domains. The move itself is built with domains in p3.
- [Proposed] **p1 lays the foundation.** p1 commits now to the pieces a queue move needs, so adding moves with domains later is not a breaking change. On one database each piece is constant or trivially true.
  - **PRD wording:** a task's domain is fixed at creation and never changes, and a queue has a domain set (its primary domain, plus the source domain while a move lasts), replacing "moving a live queue needs draining first" (PRD.md:1176).
  - **API fields:** the task's domain on the lease and session in the claim response, so worker calls reach the right domain (sessions are per domain, DESIGN.md:703); each queue's domain set in `GET /queues`, so the SDK knows when to run a second claim loop; an optional `domain` field on the claim request, because a claim must stay in one domain (PRD.md:1182).
  - **Read rules:** search by queue covers the queue's domain set, replacing "uses that one domain" (PRD.md:1186); a lookup by task ID probes the configured domains in a fixed order, since no rule exists today.
  - **Order:** while a queue is moving, the dispatch order of PRD.md:394 holds within each domain only.

### During a move

| Feature | Behaviour |
|---|---|
| Create | Goes to the target, except for tasks linked to a live task in the source |
| Caller keys and derived dedup | Read the source's `te_dedup` before inserting in the target; a duplicate there is answered as today |
| Supersession | Follows the group's live task; otherwise read the source head on first use and copy it, so a stale revision is still rejected |
| Claims | Each domain serves its own tasks. A claim names one domain; `domain` is required when a named queue is in two domains |
| Claim order | Each domain keeps the PRD order (PRD.md:394-399) for its own tasks. Between the two domains there is no order: a newer task in the target can be claimed before an older one in the source, and a `critical` task in the target can wait while the source still has `normal` work. The SDK prefers the source, so older work tends to drain first. Exact order returns when the source is empty |
| Workers | The SDK reads domain sets from `GET /queues`, runs one claim loop per domain, and polls the source first |
| Worker calls | Go to the task's domain, taken from the lease (sessions are per domain, DESIGN.md:703) |
| Reads by ID | Probe the configured domains in a fixed order |
| Search by queue | Covers the queue's domain set and any domain it left that still holds its rows |
| Concurrency and depth limits | Each domain enforces the limit minus the other domain's latest sampled use; can be exceeded by one sampling interval while the move lasts |
| Retries, timers, waits | Stay in the task's domain |
| Re-enqueue | Into the task's own domain while the move lasts; rejected in the source once it is done |
| Events | Unchanged; each task's events come from its own domain, in order |

## What we give up

- While a move lasts, dispatch order holds only within each domain (PRD.md:394).
- A move cannot finish while tasks in other queues of the source keep creating children or successors in the moved queue. Queues that share workflows should move together; the engine reports what keeps the source busy.

## Doc changes

- PRD.md:125, :394 (order holds within each domain during a move), :655, :1176, :1180, :1182, :1186.
- ADR-0010:60, :63-64, :69-71.
- DESIGN.md:703, :1064, :1995, :1999, :2003, :2835; the `GET /queues` response.
