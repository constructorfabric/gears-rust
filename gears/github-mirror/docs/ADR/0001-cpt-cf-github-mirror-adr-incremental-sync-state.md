---
status: accepted
date: 2026-08-11
decision-makers: GitHub Mirror design review
---

# ADR-0001: Incremental synchronization uses re-enterable scans with durable watermarks and fingerprints

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. Persist every task in a durable scheduler](#a-persist-every-task-in-a-durable-scheduler)
  - [B. Checkpoint only phase cursors](#b-checkpoint-only-phase-cursors)
  - [C. Always full-rescan without change-detection state](#c-always-full-rescan-without-change-detection-state)
  - [D. Re-enterable rescan with durable watermarks and fingerprints](#d-re-enterable-rescan-with-durable-watermarks-and-fingerprints)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-github-mirror-adr-incremental-sync-state`

## Context and Problem Statement

GitHub Mirror must synchronize large repositories, survive interruption, and make repeat synchronizations cheap. A single repository can contain enough issues, pull requests, comments, reviews, commits, and CI records that a durable per-task queue is tempting, but persisting every crawl task increases schema complexity and creates lease/recovery semantics that compete with GitHub's own mutable list ordering.

The key decision is what state is durable: every task, coarse phase checkpoints, no incremental state, or only the minimal change-detection state required to make a full repository re-run cheap and safe.

## Decision Drivers

- `cpt-cf-github-mirror-fr-session-resume` requires re-enterable synchronization without persisted tasks.
- `cpt-cf-github-mirror-fr-cost-efficiency` requires at least 90% fewer API calls for unchanged incremental runs.
- `cpt-cf-github-mirror-fr-memory-efficiency` requires bounded memory for repositories with 100,000+ entities.
- `cpt-cf-github-mirror-fr-idempotent` requires repeat runs to converge to the same normalized state.
- Mutable GitHub list pages make page-number checkpointing unsafe when new items shift earlier pages.
- The current implementation proves the re-enterable scan approach with watermarks, page-1 ETags, fingerprints, and incomplete-refinement status.

## Considered Options

A. Persist every task in a durable scheduler.
B. Checkpoint only phase cursors.
C. Always full-rescan without change-detection state.
D. Re-enterable rescan with durable watermarks and fingerprints.

## Decision Outcome

Chosen option: "D. Re-enterable rescan with durable watermarks and fingerprints", because it satisfies resumability without a persisted task queue while preserving low-cost incremental behavior. The synchronization task graph remains in memory. On resume, the system re-runs repository extraction and relies on durable HTTP validators, per-family watermarks, staged high-water marks, entity fingerprints, child-count hashes, content-change timestamps, and incomplete-refinement statuses to skip unchanged work and repair interrupted refinements.

### Consequences

- The task queue is intentionally not a recovery artifact; operators resume by re-running a repository or all repositories still marked `in_progress`.
- Watermark promotion must happen only after dependent refinement completes, otherwise interrupted runs can skip unrefined entities.
- Fingerprint completion must be transactionally coupled with normalized upserts so a `complete` fingerprint never points at missing rows.
- Progress reporting is approximate after interruption because tasks are regenerated, but data correctness is durable.
- The design avoids per-task leasing, queue migrations, and stale task cleanup in v1.

### Confirmation

- Unit tests cover watermark staging/promotion, page-1 ETag clean sweeps, forced sweeps, and since-bound sweeps.
- Unit/integration tests cover fingerprint gate reasons: new, fingerprint-changed, child-counts-changed, incomplete, TTL-expired, and forced.
- Resume tests verify incomplete refinements are re-enqueued even when list ETags indicate no new list changes.
- Performance tests compare unchanged incremental runs against full runs and enforce the API-call reduction target.

## Pros and Cons of the Options

### A. Persist every task in a durable scheduler

Every list page, detail fetch, and child-resource fetch is represented as a database task with status, lease owner, and lease expiry.

- Good, because exact task-level resume is straightforward to reason about.
- Good, because progress can be reported from durable task counts.
- Bad, because it adds leasing, heartbeat, stale-lease recovery, per-engine locking behavior, and queue compaction.
- Bad, because mutable GitHub lists can invalidate previously generated page tasks.
- Bad, because the PRD explicitly requires in-memory orchestration and no persisted individual tasks.

### B. Checkpoint only phase cursors

Store coarse checkpoints such as current phase, current page, or current entity offset.

- Good, because it is simpler than persisting every task.
- Good, because it gives more visible resume state than pure re-run.
- Bad, because page offsets are unsafe for mutable GitHub lists where inserts shift page boundaries.
- Bad, because it does not detect detail-level incompleteness unless paired with fingerprints anyway.
- Bad, because checkpoint semantics become endpoint-specific and hard to validate uniformly.

### C. Always full-rescan without change-detection state

On every run, enumerate and refine all configured data from scratch.

- Good, because the algorithm is simple.
- Good, because there is little durable synchronization state to migrate.
- Bad, because it violates the cost-efficiency requirement for unchanged repositories.
- Bad, because it risks exhausting GitHub API quota and triggering secondary limits.
- Bad, because large repositories become impractical for frequent synchronization.

### D. Re-enterable rescan with durable watermarks and fingerprints

Keep tasks in memory, but persist cache validators, staged watermarks, entity fingerprints, refinement status, and content-change timestamps.

- Good, because resume is mechanically simple: re-run and skip unchanged work.
- Good, because watermark scans avoid unsafe page-number checkpoints.
- Good, because fingerprints prevent unnecessary detail fetches.
- Good, because incomplete-refinement status repairs interrupted runs.
- Good, because durable state is compact and domain-oriented rather than task-oriented.
- Bad, because progress/task history is less exact than a persisted task queue.
- Bad, because correctness depends on disciplined promotion and transaction boundaries.

## More Information

Detailed pseudocode is in [ALGORITHMS.md](../ALGORITHMS.md), especially the incremental list sweep, fingerprint refinement gate, and resume-by-rescan sections.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Algorithms**: [ALGORITHMS.md](../ALGORITHMS.md)

This decision directly addresses:

- `cpt-cf-github-mirror-fr-session-resume`
- `cpt-cf-github-mirror-fr-cost-efficiency`
- `cpt-cf-github-mirror-fr-idempotent`
- `cpt-cf-github-mirror-fr-memory-efficiency`
- `cpt-cf-github-mirror-fr-sync-order`
