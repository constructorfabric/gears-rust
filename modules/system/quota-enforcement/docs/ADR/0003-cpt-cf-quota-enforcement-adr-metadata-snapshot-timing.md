---
status: accepted
date: 2026-05-07
---

# EvaluationContext metadata snapshot — at applicable-Quotas resolution

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Capture at evaluation entry, before the transaction](#a-capture-at-evaluation-entry-before-the-transaction)
  - [(b) Re-read metadata mid-evaluation on each Engine access](#b-re-read-metadata-mid-evaluation-on-each-engine-access)
  - [(c) Capture at applicable-Quotas resolution (FOR UPDATE step)](#c-capture-at-applicable-quotas-resolution-for-update-step)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-metadata-snapshot-timing`

## Context and Problem Statement

Quota Metadata is mutable and may be referenced by attribute-based Engine evaluation
(`cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`). This raises a design-time question: at what point
during evaluation does the Engine see Quota Metadata? Choices include capturing at evaluation start, re-reading during
evaluation, or capturing at the FOR UPDATE step.

## Decision Drivers

- Determinism (same input → same Decision; required for replay safety, `cpt-cf-quota-enforcement-fr-idempotency`).
- Simplicity of implementation.
- No additional contention with concurrent metadata mutations.
- Compatibility with the storage primitive shape (single-tx read + mutate).

## Considered Options

- (a) Capture metadata at evaluation entry, before the transaction begins.
- (b) Re-read metadata on every Engine access mid-evaluation.
- (c) Capture at the applicable-Quotas resolution step (inside the transaction, with FOR UPDATE).

## Decision Outcome

Chosen option: **(c) — capture at applicable-Quotas resolution**, because it gives deterministic, replay-safe Engine
inputs at zero implementation cost (the metadata is already on the row that the FOR UPDATE step reads), and it aligns
metadata visibility with counter visibility under the same lock.

### Consequences

- `EvaluationOrchestrator` materialises EvaluationContext exactly at the FOR UPDATE step (DESIGN §3.6
  `cpt-cf-quota-enforcement-seq-debit`).
- Engines never see partial or mid-evaluation metadata changes.
- Replay returns the stored `decision_blob` verbatim and does not re-read metadata — satisfies the idempotency-replay
  rule of `cpt-cf-quota-enforcement-fr-idempotency` by construction (the captured `time` binding is never re-bound on
  replay).

### Confirmation

Confirmed by orchestrator unit tests covering concurrent metadata updates against in-flight evaluations, plus replay
equivalence tests verifying byte-identical Decisions under the same idempotency key.

## Pros and Cons of the Options

### (a) Capture at evaluation entry, before the transaction

- Good, because conceptually simple ("snapshot up front").
- Bad, because metadata visibility is decoupled from counter visibility — Engine may see stale metadata alongside fresh
  counters, producing surprising decisions.
- Bad, because requires an extra read step before the transaction.

### (b) Re-read metadata mid-evaluation on each Engine access

- Good, because Engine always sees the freshest metadata.
- Bad, because non-determinism mid-evaluation: the Engine can see different metadata at different call sites within a
  single evaluate.
- Bad, because breaks replay equivalence — replay would re-read and potentially differ.

### (c) Capture at applicable-Quotas resolution (FOR UPDATE step)

- Good, because deterministic — single coherent snapshot for the entire evaluation.
- Good, because zero extra read cost — the metadata is already on the row being locked.
- Good, because replay-safe by construction (replay never re-invokes the Engine).
- Neutral, because metadata updates landing during an in-flight evaluation may be seen by some operations and not others
  depending on lock arrival order — standard transactional semantics, matches caller intuition.

## More Information

Closes the design-time question of when the Engine snapshots Quota Metadata during evaluation
(`cpt-cf-quota-enforcement-fr-quota-metadata`, `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection` — provides deterministic metadata input to the Engine.
- `cpt-cf-quota-enforcement-fr-idempotency` — replay equivalence preserved.
- DESIGN sequence `cpt-cf-quota-enforcement-seq-debit`.
