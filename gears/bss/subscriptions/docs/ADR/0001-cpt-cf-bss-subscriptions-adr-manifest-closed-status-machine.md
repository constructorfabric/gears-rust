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

`Subscription.status` has exactly five values: `draft | active | suspended | cancelled | archived`. Trial eligibility, preparation progress, pending cancellation and observed provider delivery describe different concerns. Combining them into subscription statuses would make commercial transitions depend on the progress states of every integration.

## Decision Drivers

- Keep the commercial state machine closed and consistent across consumers.
- Preserve immutable operation outcomes and replay independently of current status.
- Distinguish an accepted request, usable initial readiness, commercial activation and later provider observations.
- Represent trial terms without adding a trial status.

## Considered Options

1. Add separate subscription statuses for trial and pending work.
2. Keep the five statuses and model episodes, operation receipts and supported intents separately.
3. Introduce independently authoritative commercial and billing state machines.

## Decision Outcome

Keep the five-value status enum. An acquisition creates a `draft`; its activation operation remains pending/approved until every required initial capability is usable and the final local guards pass. Subscriptions then records `active`, actual `serviceActivatedAt` T, the applied receipt, commercial revision, facts and outbox atomically. Preparation and readiness do not add statuses or establish service time.

A trial is an explicit episode under accepted Pricing PlanRevision terms: either a free period or monetary credit. It uses ordinary subscription statuses and retains its own immutable start, expiry and conversion outcome. Billing owns promotional grants and accounting; Entitlements owns fulfillment and provider execution. Subscription status alone proves neither available bonus nor observed access.

A supported scheduled cancellation is a durable intent on the aggregate. Its financial confirmation, eventual applied cancellation and resource disposition are distinct records. Pending, failed or unknown work cannot be reported as completed cancellation. Applied cancellation is terminal for service; archive follows the separate retention guards.

`contractEffectiveAt` and `serviceActivatedAt` retain their distinct commercial meanings. Existing customer acceptance evidence remains part of the acquisition; a separate `confirmAcceptance` operation is unavailable. Collection pause and scheduled resume are outside the supported profile.

### Consequences

- Lifecycle consumers handle the same five statuses for paid and trial subscriptions.
- Operation, trial, cancellation and materialization records carry their own identity, version and recoverable outcomes.
- Initial activation consumes held Entitlements readiness through the local activation/non-activation fence. A provider callback never changes commercial status directly.
- A new subscription status requires an explicit domain contract change.

### Confirmation

- The domain enum accepts only the five declared values.
- A preparing acquisition remains draft; successful initial activation records one fresh T and applied outcome.
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
