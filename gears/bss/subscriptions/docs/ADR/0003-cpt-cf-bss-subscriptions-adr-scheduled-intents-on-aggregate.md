---
description: "Decision to persist supported scheduled intents on the subscription aggregate for durable execution and recovery."
status: proposed
date: 2026-07-15
decision-makers: "BSS Subscriptions team"
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-08-24 by Virtuozzo International GmbH

# ADR-0003: Scheduled Intents Live on the Aggregate (Not Portal-Side Automation)

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Portal-side automation (cron re-keys the transition)](#portal-side-automation-cron-re-keys-the-transition)
  - [Pending intents on the aggregate (chosen)](#pending-intents-on-the-aggregate-chosen)
  - [A native SubscriptionSchedule aggregate](#a-native-subscriptionschedule-aggregate)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-subscriptions-adr-scheduled-intents-on-aggregate`

## Context and Problem Statement

A customer may request cancellation at period end or another supported future boundary. If only a portal scheduler remembers that commitment, renewal can create a new term before the delayed cancellation reaches Subscriptions. Recovery also cannot distinguish a standing intent from a lost command.

## Decision Drivers

- Renewal and trial conversion must observe pending cancellation before committing.
- The committed intent must survive caller, worker and broker failure.
- Scheduled execution must use current authorization, revision and lifecycle guards.
- Keep commercial intent, charging cutoff, applied transition and physical resource outcome distinct.

## Considered Options

1. Portal-side automation submits a new transition at the due date.
2. Persist supported pending intents on the subscription aggregate.
3. Introduce a separate multi-step schedule aggregate.

## Decision Outcome

Supported cancellation intents live on the aggregate with stable identity, expected revision, effective time, provenance and recoverable disposition. Cancellation supports immediate, end-of-term and explicit future-date requests. A pending or unresolved failed cancellation blocks conflicting renewal and trial conversion until explicitly resolved.

The scheduler assigns each firing the durable identity `intent:{intentId}`. Work due at the same boundary follows deterministic priority: cancellation/end-of-term opt-out first, then applicable lifecycle timers, with intent-ID ordering within a kind. Workers re-read current state and preceding outcomes; a terminal transition disposes of incompatible pending work. Multi-instance worker claims and local aggregate fences enforce this order.

A firing reuses the normal transition path. It records actual application only after the required guards succeed. It never backdates physical stop or commercial state to a delayed worker's scheduled time. Customer-confirmed financial cutoff remains effective independently of later resource completion. Withdrawing or rescheduling a confirmed cancellation requires an explicit versioned confirmation before its cutoff; unfinished execution after cutoff cannot retroactively restore charging.

Renewal and trial expiry/conversion retain their durable timers. Future-start Orders retain their acquisition and attempt-recovery contract; their scheduled date alone starts no service. Scheduled composition, scheduled resume, ramps and base-plan changes refuse before effects. No multi-step schedule aggregate is required.

### Consequences

- Renewal and operation readers see the same durable standing cancellation intent.
- Supported timers share idempotency, ordering and recoverable execution rather than relying on process-local locks.
- Pending intent alone produces no served interval or commercial fact. A confirmed cutoff may require financial replacement evidence without inventing a completed stop.
- Notification delivery and an audit subsystem are not required; operation history and reliable domain events remain mandatory.

### Confirmation

- A pending end-of-term cancellation prevents automatic/manual renewal and paid trial conversion even if their workers arrive first.
- A valid confirmed withdrawal before cutoff restores ordinary eligibility; replay returns the same outcome.
- Policy denial or provider uncertainty cannot fabricate a completed transition. A failed cancellation continues suppressing renewal until explicit resolution.
- A recovering late worker preserves the known financial cutoff and records truthful later application/resource times.

## Pros and Cons of the Options

### Portal-side automation (cron re-keys the transition)

- Requires no aggregate intent record.
- Hides the customer's commitment from renewal and recovery until the scheduler submits it.

### Pending intents on the aggregate (chosen)

- Gives every lifecycle reader the same standing intent and one ordered application path.
- Requires durable intent storage, worker claims and explicit failure recovery.

### A native SubscriptionSchedule aggregate

- Can represent multi-step negotiated schedules.
- Adds another stateful aggregate beyond the supported cancellation and ordinary lifecycle-timer requirements.

## More Information

[Lifecycle Foundation](../features/01-foundation.md) defines ordering and recovery; [renewal and retention](../features/04-renewal-retention.md) defines financial confirmation, cancellation and offboarding.

## Traceability

- **PRD:** [PRD.md](../PRD.md), `fr-scheduled-intents`, renewal and cancellation requirements.
- **Decisions:** [DECISIONS.md](../DECISIONS.md), SUB-D-01 and SUB-D-04.
- **Design:** [DESIGN.md](../DESIGN.md).
