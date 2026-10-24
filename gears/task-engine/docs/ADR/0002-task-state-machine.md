---
status: accepted
date: 2026-10-24
description: "Model every task as one of 10 explicit states with a fixed transition matrix, four terminal states, and one guarded update per transition."
---

# Model the Task Lifecycle as an Explicit 10-State Machine

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Free-form status string](#option-1-free-form-status-string)
  - [Option 2: Minimal state set with flags](#option-2-minimal-state-set-with-flags)
  - [Option 3: Explicit 10-state machine with a transition matrix](#option-3-explicit-10-state-machine-with-a-transition-matrix)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-state-machine`

## Context and Problem Statement

The Task Engine must track every task from creation to its final outcome. Tasks can be delayed (`run_after`), blocked on predecessors, claimed by a worker, running, parked on a wait condition, retried, cancelled, timed out, or dead-lettered. Workers, timers, the event consumer, and administrators all change task state concurrently, on several nodes.

We must decide how task state is represented and changed, so that every actor sees one consistent lifecycle, illegal transitions cannot happen, and every transition can drive slot release, timers, and lifecycle events in the same transaction.

## Decision Drivers

* One authoritative lifecycle shared by the REST API, SDK, timers, and event consumer.
* Every transition is atomic with its side effects (slots, queue-depth counters, timers, outbox events).
* Illegal transitions are impossible, not merely discouraged.
* Concurrent actors (worker, lease scan, cancel, timer) must race safely without row locks.
* States must be cheap to index and filter (claim dispatch, search, retention).

## Considered Options

1. **Free-form status string** — store a status text column and let each code path decide what to write.
2. **Minimal state set with flags** — a few coarse states (pending/active/done) plus boolean flags for waiting, blocked, scheduled, dead-lettered.
3. **Explicit 10-state machine with a transition matrix** — ten named states, a closed transition matrix, and one guarded conditional update per transition.

## Decision Outcome

Chosen option: **Option 3 — Explicit 10-state machine with a transition matrix.**

Every task is in exactly one of ten states, stored as a stable `TaskState` enum discriminant (`te_task_state.state_code`). Four states are terminal: `completed`, `failed`, `cancelled`, and `dead_lettered`. A terminal task never changes state again, with one exception: an administrator may re-enqueue a `dead_lettered` task back to `queued`.

Every transition is listed in the DESIGN state transition table and is executed as one guarded conditional update that names the expected source state. The same transaction applies the transition's side effects: concurrency slot release, queue-depth counter changes, timer creation or deletion, and the lifecycle event written to the outbox. If the guard matches zero rows, another actor won the race and the caller re-reads the task.

### Consequences

* There is one lifecycle for all actors; the REST API, SDK, timers, and event consumer cannot disagree about a task's state.
* Illegal transitions are rejected by construction (`failed_precondition`), not by convention.
* Retryable failures go `running → scheduled`, never through `failed`; `failed` is always terminal.
* Cancellation is a first-class terminal state; `cancelled`, `forciblyCancelled`, and `superseded` result codes record how it happened.
* Claim dispatch, search, and retention all filter on one indexed column.
* Adding or removing a state requires a new ADR, an enum migration, and an update of the transition table.

### Confirmation

We confirm this decision through design review of the transition table and through tests.

Tests MUST check that:

* every transition in the table succeeds from its source state and is rejected from any other state;
* terminal states accept no transition except admin re-enqueue of `dead_lettered`;
* two concurrent transitions on one task produce exactly one winner;
* each transition performs its side effects (slots, counters, timers, events) atomically.

## Pros and Cons of the Options

### Option 1: Free-form status string

Each component writes the status it believes is correct; validation is spread across handlers.

* Good, because it is quick to start.
* Bad, because nothing prevents illegal transitions or two components disagreeing.
* Bad, because side effects (slots, timers, events) are re-implemented per code path.
* Bad, because strings are expensive to index and easy to misspell.

**Rejected.**

### Option 2: Minimal state set with flags

Sub-states are expressed through flags on top of a coarse status.

* Good, because there are fewer states to document.
* Bad, because flag combinations create implicit, untested states.
* Bad, because claim dispatch and search must evaluate several columns instead of one indexed discriminant.
* Bad, because terminal vs non-terminal becomes ambiguous.

**Rejected.**

### Option 3: Explicit 10-state machine with a transition matrix

States: `scheduled`, `blocked`, `queued`, `claimed`, `running`, `waiting` (non-terminal) and `completed`, `failed`, `cancelled`, `dead_lettered` (terminal). Each transition is a guarded `UPDATE ... WHERE state_code = <expected>` that also performs its side effects in the same transaction.

* Good, because every legal path is enumerated and reviewable.
* Good, because a single indexed `state_code` drives dispatch, search, and retention.
* Good, because guarded updates make concurrent transitions race-safe without row locks.
* Good, because side effects are attached to transitions in one place.
* Bad, because adding a state later requires a new ADR and a migration of the enum.

**Chosen.**

## More Information

```text
non-terminal: scheduled, blocked, queued, claimed, running, waiting
terminal:     completed, failed, cancelled, dead_lettered
exception:    dead_lettered --> queued (admin re-enqueue)
```

The full diagram and the per-transition side effects are in DESIGN §3.1 Task State Machine and the State Transition Table.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-state-result-enums` — stable `TaskState` and `TaskResultCode` enums.
* `cpt-cf-task-engine-fr-task-create` — initial state is `queued`, `scheduled`, or `blocked`.
* `cpt-cf-task-engine-fr-task-cancel` — cancellation is a terminal lifecycle state.
* `cpt-cf-task-engine-fr-retry` — retryable failures go to `scheduled`, never `failed`.
* `cpt-cf-task-engine-fr-dead-letter` — `dead_lettered` is terminal with admin re-enqueue.
* `cpt-cf-task-engine-fr-wait-conditions` — `waiting` is a non-terminal state.
* `cpt-cf-task-engine-fr-dependencies` — `blocked` holds tasks with unmet predecessors.
