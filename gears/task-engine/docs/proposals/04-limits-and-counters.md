# Limits and counters

**Status:** Draft · [All proposals](README.md)

## Problem

Per-tenant depth limits (`tenant_max_queue_depth`, `tenant_max_deferred_tasks`; PRD.md:443, :445) are enforced exactly: every create and every claim locks a row of `te_tenant_queue_backlog`. To keep that row from becoming a hot spot, the design shards it (`backlog_shard`) and reconciles the shards (DESIGN.md:1602, :1647-1677). That is a lock on the hottest paths, plus sharding and reconciliation machinery, to hold a limit whose purpose is back-pressure, not accounting. The queue-wide depth limits already use a periodic count instead.

## Proposal

Per-tenant depth limits are checked against counts taken every few seconds, like the queue-wide limits, so creates and claims stop locking a backlog row.

- [Proposed] **Per-tenant depth from periodic counts.** Per-tenant depth limits are checked against a count taken every few seconds, not a locked backlog row.
  - **Kept:** `tenant_max_queue_depth` and `tenant_max_deferred_tasks` (PRD.md:443, :445), the `429` `QUEUE_DEPTH_LIMIT` with `Retry-After`, and the admission rules.
  - **Change:** the maintenance leader counts each tenant's tasks every `queue_depth.sample_secs` (default 5 s, DESIGN.md:1669); admission is refused while the latest count is at or above the limit.
  - **Removed:** `te_tenant_queue_backlog`, the `backlog_shard` column, and their sharding and reconciliation (DESIGN.md:1602, :1647-1677).
  - **Overshoot:** at most one interval of the tenant's own admissions: at 1,000 creates/s and 5 s, up to 5,000 over a 100,000 limit. Per-tenant rate limits are p2 (PRD.md:1206).

### Unchanged

- **Concurrency limits.** Global and per-tenant concurrency stay exact, with counter rows locked to commit, as designed (PRD.md:427). Revisit only if a benchmark shows the counter rows are a bottleneck.

## What we give up

- Exact per-tenant depth enforcement, a MUST today (PRD.md:447) with an acceptance criterion (PRD.md:1492).

## Doc changes

- PRD.md:447, :1492.
- DESIGN.md:1597-1611 (lock order step 3), :1627-1677 (reconciliation, backlog table, hot-row rules, admission), :2606-2621 (create).
- ADR-0008:73, :102.
