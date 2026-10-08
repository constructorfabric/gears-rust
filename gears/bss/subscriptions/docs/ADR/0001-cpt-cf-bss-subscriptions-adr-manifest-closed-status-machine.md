---
description: "Decision to keep the subscription status enum closed and represent orthogonal lifecycle concerns as attributes."
status: proposed
date: 2026-07-15
decision-makers: "BSS Subscriptions team"
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-08-24 by Virtuozzo International GmbH

# ADR-0001: Keep the Manifest Status Enum Closed

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Add trial / paused / pending statuses to the enum](#add-trial--paused--pending-statuses-to-the-enum)
  - [Model them as attributes, postures, and pending intents on the closed enum (chosen)](#model-them-as-attributes-postures-and-pending-intents-on-the-closed-enum-chosen)
  - [Split the state across two machines (commercial vs billing)](#split-the-state-across-two-machines-commercial-vs-billing)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-subscriptions-adr-manifest-closed-status-machine`

## Context and Problem Statement

`Subscription.status` has exactly six values: `draft | preparing | active | suspended | cancelled | archived`. Trial eligibility, pending cancellation, asynchronous delivery of resume and active changes, and observed provider delivery describe different concerns and are not statuses. Initial preparation is different: it holds T, accrual and the occupancy conversion, so a customer whose initial preparation has started but who is not yet served must be visible from the status alone. `preparing` is that status; a confirmed purchase whose authorized preparation has not started yet, such as a future-start purchase, stays `draft`; the step-by-step progress of preparation stays in the durable activation operation. Turning every in-flight phase into a status would make commercial transitions depend on the progress states of every integration.

## Decision Drivers

- Keep the commercial state machine closed and consistent across consumers.
- Preserve immutable operation outcomes and replay independently of current status.
- Distinguish an accepted request, usable initial readiness, commercial activation and later provider observations.
- Represent trial terms without adding a trial status.

## Considered Options

1. Add separate subscription statuses for trial and every kind of pending work.
2. Keep five statuses and derive a preparing view from the activation operation.
2a. Add exactly one status, `preparing`, for the only in-flight phase that holds T; model trial episodes, operation receipts and supported intents separately.
3. Introduce independently authoritative commercial and billing state machines.

## Decision Outcome

Keep a closed enum of six values (option 2a). An acquisition creates a `draft`. The authorized start of initial preparation moves it to `preparing` in the same transaction that persists the activation operation; the operation remains pending/approved until every required initial capability is usable and the final local guards pass. Subscriptions then records `active`, actual `serviceActivatedAt` T, the applied receipt, commercial revision, facts and outbox atomically. Closure without activation, including ordinary withdrawal, returns the subscription to `draft` with the reason on the operation; only explicit subscription cancellation during preparation moves it to `cancelled`. Only Subscriptions writes `preparing`; readiness observations never change a status, and preparation does not establish service time (SUB-D-58).

A trial is an explicit episode under accepted Pricing PlanRevision terms: either a free period or monetary credit. It uses ordinary subscription statuses and retains its own immutable start, expiry and conversion outcome. Billing owns promotional grants and accounting; Entitlements owns fulfillment and provider execution. Subscription status alone proves neither available bonus nor observed access.

A supported scheduled cancellation is a durable intent on the aggregate. Its financial confirmation, eventual applied cancellation and resource disposition are distinct records. Pending, failed or unknown work cannot be reported as completed cancellation. Applied cancellation is terminal for service; archive follows the separate retention guards.

`contractEffectiveAt` and `serviceActivatedAt` retain their distinct commercial meanings. Existing customer acceptance evidence remains part of the acquisition; a separate `confirmAcceptance` operation is unavailable. Collection pause and scheduled resume are outside the supported profile.

### Consequences

- Lifecycle consumers handle the same six statuses for paid and trial subscriptions and treat `preparing` as not served and not terminal; for occupancy it counts as a draft.
- Operation, trial, cancellation and materialization records carry their own identity, version and recoverable outcomes.
- Initial activation consumes held Entitlements readiness through the local activation/non-activation fence. A provider callback never changes commercial status directly.
- A new subscription status requires an explicit domain contract change. `preparing` is the only in-flight phase that qualifies, because it alone holds T; resume and active changes do not get a status.

### Confirmation

- The domain enum accepts only the six declared values.
- A preparing acquisition reads `preparing` from the authorized preparation start; successful initial activation records one fresh T and applied outcome; closure without activation returns `draft` with the operation's activation resolution; a `draft` without an activation operation was never prepared.
- An active trial reports active plus its trial episode. Pending cancellation reports the current status plus its intent and any confirmed financial cutoff.
- Reordered delivery observations cannot revive a cancelled subscription or rewrite an applied outcome.

## Pros and Cons of the Options

### Add trial / paused / pending statuses to the enum

- Makes progress visible in one field.
- Couples all consumers to independent episode and provider states and expands the transition matrix.

### Model them as attributes, postures, and pending intents on the closed enum (chosen)

- Preserves a bounded commercial machine and explicit authority for each independent outcome.
- Requires readers to inspect the relevant episode, intent or observation instead of inferring everything from status.

### Split the state across two machines (commercial vs billing)

- Separates commercial and financial concerns.
- Introduces another subscription-state authority without replacing Billing's own financial records or resolving provider progress.

## More Information

[Lifecycle Foundation](../features/01-foundation.md), [commercial posture and readiness](../features/05-entitlements.md) and [trial lifecycle](../features/06-trials.md) define the corresponding records and guards.

## Traceability

- **PRD:** [PRD.md](../PRD.md), `fr-status-enum`, `fr-trials-not-a-status`, `fr-scheduled-intents`, `fr-activation-instants`.
- **Decisions:** [DECISIONS.md](../DECISIONS.md), SUB-D-01, SUB-D-05, SUB-D-55, SUB-D-56, SUB-D-57.
- **Design:** [DESIGN.md](../DESIGN.md).
