# Claiming and ordering

**Status:** Draft · [All proposals](README.md)

## Problem

A claim can name several queues. Today it merges them by urgency, age and task ID (PRD.md:657, DESIGN.md:2460), which means:

- **Every claim reads every named queue** up to its scan limits (DESIGN.md:2132-2134).
- **Claimers collide.** Workers with overlapping queue sets compete for the same head rows.
- **Locks widen.** One claim transaction holds the counter locks of every queue its picks came from (ADR-0008:73, :98), and coalescing splits by queue set (ADR-0008:101).
- **One backlog wins.** A queue with an old backlog takes every claim until it is empty.
- **Scan limits are applied per queue.** The config comments say `claim_candidate_budget` and `claim_scan_rows_max` are per claim (DESIGN.md:2132, :2134), but the claim loop applies them per queue (DESIGN.md:2443, :2447, :2509), allowing up to 16 × 256 tenant probes in one claim.

Separately, a claim that loses a head row to another claimer waits for the winner's commit while it holds its counter locks (ADR-0008:98). The throughput target is at least 100 workers on one queue (PRD.md:1246), where only skipping locked rows helps.

Nothing needs oldest-first across queues (see [What we give up](#what-we-give-up)), and priority was never part of the merge.

## Proposal

Inside one domain, a claim takes `critical` tasks first from any queue it names, then rotates between its queues for normal work. Order inside a queue does not change. Taking the rows uses `SKIP LOCKED`. Nothing orders work across domains.

- [Proposed] **Claims across queues.** How one claim chooses between the queues it names.
  - **Critical first.** The claim probes each named queue for a queued `critical` task (one indexed read per queue, DESIGN.md:1525) and takes those oldest first.
  - **Then rotation.** Otherwise it starts at the queue after the one the previous claim started at, takes tasks in that queue's own order, and moves on only while the batch is not full. The rotation cursor lives in node memory per waiter group, like the tenant cursor (DESIGN.md:2513). This replaces the merge by urgency, age and ID (PRD.md:657, DESIGN.md:2460).
  - **One domain only.** A claim can only name queues of one domain (PRD.md:655). A worker that consumes several domains sends one claim per domain, and a `critical` task in one domain does not go ahead of `normal` work in another.
  - **Scan limits count per claim.** `claim_candidate_budget` and `claim_scan_rows_max` are shared by all the queues one claim visits. A probe is one "next tenant" read, skipped or not. A claim that uses up its budget returns what it has, and the next claim starts at the next queue. The tenant cursor moves past every tenant a claim probes and keeps its place for the next claim on that queue, so running out of budget pauses the walk instead of restarting it, and no tenant is skipped for good.
- [Proposed] **SKIP LOCKED when taking tasks.** ToolKit (#5478) uses `FOR UPDATE SKIP LOCKED` by default on PostgreSQL and MySQL/MariaDB, only in the step that takes tasks after the counter slots are reserved.
  - **Lock order holds** because a claim plans before it locks:
    - **Plan, then reserve.** The claim first reads its queues in rotation order without locks (free slots and queued tasks) until the batch is covered. Then it reserves the counters it needs in the canonical key order (ADR-0008:73), takes the tasks, and settles. It never locks a counter after its first task write. Locking each queue's counter as the rotation reaches it would deadlock: two claims that start at different queues lock counters in opposite orders.
    - **Critical tasks are in the same plan.** The critical-first step can take tasks from several queues, so its picks are planned and reserved in the same pass.
    - **Why no cycle can form:** a claim waits only on counter rows, in key order, before it holds any task row, and with `SKIP LOCKED` it never waits on a task row. So the deadlock proof stands. ADR-0008 rejected `SKIP LOCKED` at candidate selection for exactly that reason (ADR-0008:151); this uses it only after the counters are held.
  - **Goes:** the "ascending `task_id`" write rule and the "only a greater `task_id`" retry limit (ADR-0008:74).
  - **SQLite** keeps guarded updates. The benchmark measures the result and no longer decides it.

### Benchmark

A local PostgreSQL 18 run compared rotation with the merge plus `SKIP LOCKED` (batch 10, about 1.2 ms per round trip, no concurrency limits):

- **One queue:** equal for `fifo` queues; rotation 1.3–1.5× faster with 200 tenants.
- **4–16 queues, 100 workers:** rotation 1.4–2.9× faster, because every merge claim starts at the queue with the oldest task.
- **With limits** the gap widens, as merge claims then share one counter row; sharded counters should narrow it again.
- **No deadlocks** in any run.

### Unchanged

Order inside a queue (urgency, tenant fairness, tenant-local priority, age, task ID; PRD.md:394-399), and the `fifo` and `round_robin` fairness modes (PRD.md:410-411). A `fifo` queue still serves its tasks in one order. Strict one-at-a-time FIFO is not offered: even with a concurrency limit of 1, `SKIP LOCKED` and retry backoff can let a later task go first.

## What we give up

- **Oldest-first order across queues.** Nothing depends on it for correctness:
  - **Claim order is not run order.** A task claimed first can still finish last: claimed tasks run concurrently, and retries, backoff and lease expiry reorder them. When one task must happen before another, the guarantee is a dependency (the later task names the earlier one as a predecessor), not the order of claims.
  - **Today's order is already partial.** It holds only within one claim's view of its own queues, among rows no other claim is taking (DESIGN.md:2512). Workers that name different queue sets, such as {X, Y} and {Y, Z}, each see a different order, and there is no global one.
  - **Queues exist to separate work** by limits, worker pools or types. Work that needs one order goes in one queue, where order stays exact per tenant.
  - **Other tools do not offer it.** Hatchet has no order across queues, Temporal task queues are independent, BullMQ workers consume one queue each, and Celery consumes its queues round-robin. Sidekiq's strict mode orders queues by precedence, not by age; that is a different feature, and DESIGN already rejected it (DESIGN.md:2520).
  - **The one use case is draining an old backlog first**, for example after an outage. That is a preference about speed, not correctness, and remains possible: mark the urgent tasks `critical` (still first across queues), point more workers at that queue, or hold the other queues for a while ([Queue operations](06-queue-operations.md)). Strict oldest-first has the opposite cost: a fresh queue gets no claims at all until the backlog is gone.
- Strict order under concurrent claims. Order across queues and within a queue becomes best effort, as PRD.md:653 already allows.
- Separate scan budgets per queue. One `round_robin` queue with many full tenants can use a claim's whole budget; the next claim starts at another queue.

## Open questions

- [Open] **Priority aging: drop it, keep it at p3 as designed, or replace it with a different guard against starvation inside a tenant?**
  - **Context:** aging is a p3 SHOULD that raises the priority of tasks queued too long, "so low-priority work is not starved forever by the same tenant's higher-priority work" (PRD.md:535-537). Priority is tenant-local (PRD.md:401): round-robin protects tenants from each other, but inside one tenant the order is strict (PRD.md:394-399), and aging is the PRD's answer to that gap (PRD.md:539, DESIGN.md:2514).
  - **For dropping:** no adopter names a need, none of the queue tools we looked at has it, a tenant can put low-priority work in a separate queue or bound it with a queue timeout, and every pass rewrites the indexed `priority` column on queued rows (DESIGN.md:868). Mutable priority is also one reason `priority` lives in `te_task_state` (DESIGN.md:1444), though bulk reprioritize ([Task mutability](01-task-mutability.md)) needs that too.
  - **For keeping:** without aging, a tenant that mixes priorities in one overloaded queue can wait forever on its low-priority work, and the design has no other guard.
  - **Would settle it:** an adopter that mixes priorities in one queue under sustained load.

## Doc changes

- PRD.md:653 ("best eligible task across all named queues"), :657 (rewrite "Order across queues": critical first across the queues of one claim, then rotation; applies only within a single domain; no order between domains; best effort).
- ADR-0008:46-47, :71, :74, :108, :126, :129, :139, :195-201, :215.
- ADR-0008:144: keep the {1, 10} queues runs in the benchmark.
- DESIGN.md:107, :755, :2133-2134, :2460, :2506, :2508.
- DESIGN.md:2132, :2134, :2443, :2447, :2509 and ADR-0008:71: the scan limits count per claim, and what a probe is.
