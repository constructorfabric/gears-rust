---
status: accepted
date: 2026-10-24
description: "Support exactly four wait-condition kinds (timer, event, input, children) as engine-owned domain types with ALL/ANY groups and an arm-then-park option, instead of a GTS-extensible condition type."
---

# Model Wait Conditions as Fixed Domain Elements with Four Built-in Kinds

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: GTS-extensible wait-condition type](#option-1-gts-extensible-wait-condition-type)
  - [Option 2: Fixed kinds: timer, event, input, children](#option-2-fixed-kinds-timer-event-input-children)
  - [Option 3: Callback token](#option-3-callback-token)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-wait-condition-model`

## Context and Problem Statement

A running task can park itself until something happens: a point in time, an external event, input from a person or another system, or the end of its child tasks. While parked the task is in `waiting`, holds no concurrency slot, and must resume exactly once, even across crashes and duplicate event delivery.

We must decide whether wait conditions are an open, extensible type (for example a GTS base type that other gears derive from) or a fixed part of the Task Engine domain model.

## Decision Drivers

* Resumption must be exactly-once and transactional with the task state change.
* The engine must understand every condition kind well enough to index, time out, and resolve it.
* Durable suspension must survive restarts.
* Keep the model small and testable; avoid a plugin surface for wait semantics.
* An event that arrives just before the wait is registered must not be missed.

## Considered Options

1. **GTS-extensible wait-condition type** — a GTS base type that gears derive new wait kinds from, with their own matching logic.
2. **Fixed kinds: timer, event, input, children** — four engine-owned kinds stored as a stable enum, combined in ALL/ANY groups.
3. **Callback token** — the task stops and hands out an opaque token; an external system resumes it by calling back with the token, as in AWS Step Functions `waitForTaskToken`.

## Decision Outcome

Chosen option: **Option 2 — Fixed kinds: timer, event, input, children.**

Wait conditions are rows in `te_wait_conditions` with a stable `WaitConditionKind` discriminant (`timer`, `event`, `input`, `children`) and state (`pending`, `satisfied`, `timed_out`, `cancelled`). They are not GTS types.

A task sets one group of conditions with `all` or `any` semantics. In `any` mode the first satisfied condition wakes the task and cancels its siblings; in `all` mode every condition must be satisfied. A group may have a timeout whose `on_timeout` action fails or resumes the task.

Resolution (timer fire, matching event, input resolve, last child reaching a terminal state) marks the condition `satisfied` and, when the group is complete, moves the task `waiting → queued` in the same transaction and writes `WaitConditionSatisfied` and `TaskResumed` to the outbox. For event waits, each task domain records the event in its own inbox in that transaction, and the consumer offset is committed after all domains, so a duplicate event never satisfies a wait twice.

**Arm, then park.** A worker that triggers the event itself (for example, it calls a service whose reply comes back as an event) could miss the reply if the reply arrives before the wait exists. So a worker may first **arm** the group (`park = false`): the conditions start matching as soon as the call commits (the broker subscription is static), while the task keeps running. The worker then starts its action and calls **park**. If the group is already satisfied, the task is not parked and continues under the same lease; otherwise it parks as usual. Wait timeouts start at park.

### Consequences

* Wait resolution is exactly-once on the engine side.
* A waiting task releases its concurrency slot and holds no lease.
* A waiting task counts against its tenant's deferred limit (PRD §5.2 Queue Depth Limits), so parking is an admission that may be refused with `429`; resuming never is. Otherwise parking would free queue capacity without bound.
* Wait condition rows are live-only: they are deleted in the task's terminal transaction, and the wait history stays in the task's event log (ADR-0009).
* Event waits use one static subscription over operator-listed waitable topics. Setting or arming one runs the Event Broker's subscribe checks with the caller's own context (`consume` on the topic, the event type, and the condition's subject type, which is always known); PDP constraints are applied to every candidate event, and constraints the engine cannot apply refuse the wait. Each event condition's timeout is capped, so the set-time checks expire. Only the matched event's header is stored (DESIGN §3.2).
* Pending event conditions are limited per tenant, per tenant and event type, and per tenant and event type without a subject, because one consumer serves every tenant (ADR-0007).
* Each kind has its own validation, indexes, and timeout handling.
* No event is missed when the worker arms the wait before it acts.
* A parent can wait for its children without holding a lease or polling.
* Adding a fifth kind requires a new ADR and a schema change.

### Confirmation

Tests MUST check that:

* each kind resolves and resumes the task exactly once;
* `any` groups cancel remaining siblings and `all` groups wait for every member;
* a group timeout applies `on_timeout` correctly;
* a duplicate event does not satisfy a wait twice, even across task domains;
* a caller without `consume` on the topic, the event type, or the subject type cannot set or arm an event wait; constraints returned by the PDP keep events outside them from matching, and unenforceable constraints refuse the wait;
* an event condition never waits longer than `event_waits.max_wait_secs`;
* pending waits survive a restart;
* an event published after an arm call returns satisfies the armed condition, and park then returns it without parking the task;
* a children condition is satisfied once, after the last child reaches a terminal state, with the per-state counts.

## Pros and Cons of the Options

### Option 1: GTS-extensible wait-condition type

New kinds are registered at runtime with their own schema and resolver.

* Good, because new kinds need no engine release.
* Bad, because the engine cannot index or validate unknown kinds.
* Bad, because resolution logic would run outside engine transactions, breaking exactly-once resume.
* Bad, because it adds a plugin ABI (contradicts ADR-0001).

**Rejected.**

### Option 2: Fixed kinds: timer, event, input, children

`timer` waits until a time; `event` matches an incoming broker event by `event_type`, optional subject filters, and an optional CEL expression (ADR-0007); `input` waits for a resolve call carrying a payload; `children` waits until the task has no non-terminal child tasks. Conditions are grouped with `all` or `any` semantics and an optional group timeout with `on_timeout = fail | resume`.

* Good, because every kind has dedicated columns and indexes.
* Good, because resolution runs in the same transaction as the `waiting → queued` transition.
* Good, because the four kinds cover time, external events, human/system input, and fan-in of child tasks.
* Bad, because a genuinely new kind needs an engine change and a new ADR.

**Chosen.**

### Option 3: Callback token

One generic resume mechanism instead of typed kinds; time and event waits are left to the caller.

* Good, because the engine needs no event subscription and no matching logic.
* Good, because it is a well-known pattern.
* Bad, because every external system must store and return the token, so it cannot react to existing platform events.
* Bad, because time waits and children waits still need engine support.
* Neutral, because the `input` kind already covers the call-back case with a typed payload.

**Rejected.**

## More Information

See DESIGN §3.1 WaitCondition entity, §3.2 WaitConditionService, §3.6 Event Wait sequence, and §3.7 `te_wait_conditions`.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-wait-conditions` — durable suspension with ALL/ANY semantics.
* `cpt-cf-task-engine-fr-wait-timer` — the `timer` kind.
* `cpt-cf-task-engine-fr-wait-event` — the `event` kind.
* `cpt-cf-task-engine-fr-wait-input` — the `input` kind.
* `cpt-cf-task-engine-fr-wait-children` — the `children` kind.
* `cpt-cf-task-engine-usecase-event-wait` — a task parks until an external event arrives.
