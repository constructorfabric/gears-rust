Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Retroactivity & Corrections (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: usage-collector, pricing, subscriptions | Downstream: Billing (via BillableItemDeliveryV1) | Owners: BSS Rating team -->

# DESIGN — Retroactivity & Corrections (Slice 8)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-retroactivity-corrections`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles and Constraints](#2-principles-and-constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions and Sequences](#36-interactions-and-sequences)
  - [3.7 Database Schemas and Tables](#37-database-schemas-and-tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 Replay, Re-evaluation and the Pin of Record (normative)](#41-replay-re-evaluation-and-the-pin-of-record-normative)
  - [4.2 Correction Keys and Idempotency (normative)](#42-correction-keys-and-idempotency-normative)
  - [4.3 Posted Periods (normative)](#43-posted-periods-normative)
  - [4.4 Reversal Math and Emission Guards (normative)](#44-reversal-math-and-emission-guards-normative)
  - [4.5 Acceptance Vectors](#45-acceptance-vectors)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Retroactivity is **not a separate path**. Late usage, an invalidation or replacement, a negative
measurement, a new coverage version, a re-attributed segment, a new fact version, and an
administrative re-rate all do the same thing: they change an input of one or more **child windows**
(`input_generation + 1`), and each affected child is **evaluated again**. A final child gets a new
`window_revision`; its parent gets a new complete `result_revision` once every expected child is
final again (DESIGN §3.6 Flow B; slice 15). Nothing already written is updated.

Rating publishes **absolute** results and never decides whether a revision is invoice content or an
adjustment: Billing compares the new rounded aggregate with its latest accepted target, including
pending postings, and posts the difference (Atlas D09/D10; T-D-45). The math in Rating is identical
before and after an invoice is issued.

**Replay** is different from re-evaluation: it re-evaluates a *recorded* result from the inputs stored
with it, to prove reproducibility. It writes nothing.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-posted-period-protection` | Rating never mutates a delivered revision; every correction is a new revision with `previous_result_revision`; Billing derives the posted-period credit/debit (§4.3). Usage time, acceptance time, rating time and the pin are recorded on every result (§4.4). |
| `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve` | A late record changes its window counter and re-evaluates the whole child (all slices, band continuity) **with the child's pin-of-record** (§4.1). |
| `cpt-cf-bss-rating-fr-usage-corrections` | Invalidation entries subtract the withdrawn quantity; replacements are ordinary records; negative records are ordinary measurements (slice 12 §4.4). |
| `cpt-cf-bss-rating-fr-idempotency` (delta family) | `(child_id, window_revision)`, `(fact_id, result_revision)`, `delivery_id`; unchanged inputs write nothing (§4.2). |
| `cpt-cf-bss-rating-fr-separation` | Usage records, results and deliveries are insert-only. |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Child row lock | A re-evaluation serializes only on its child row and the parent head | Load test |
| `cpt-cf-bss-rating-nfr-resilience` | Input-generation CAS | A stale evaluation can never publish a newer revision (F21) | Chaos test |
| `cpt-cf-bss-rating-nfr-throughput-latency` | Coalesced queue | A burst of late records re-evaluates a child once; `rating_correction_cascade_size` bounds the fan-out | Load test |
| Bitemporal audit (PRD §6.10) | Result manifest | interval and acceptance time of contributing records vs `rated_at` and the pin | Fixture |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Re-evaluation selects on the same scope key from the pinned document, so identical inputs resolve the same row. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-rtr`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain (`rating-core`) | `evaluate` is the only math; `replay(manifest)` re-evaluates a recorded result | Rust, pure |
| Application (`rating`) | Input-generation bumps, `RerateService`, `ReplayVerifier` | Rust gear crate |
| Infrastructure | `rating_window_result`, `rating_fact_result`, `rating_rerate_run` (DESIGN §3.7) | PostgreSQL via SecureORM |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Replay the pin, never the live catalog

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-pinned-replay-rtr`

A correction of a final child re-evaluates with the child's **pin-of-record** and the pricing
documents Rating stored under it; a replay uses the recorded pin of the result being replayed.
Neither ever reads pricing live. Only an administrative re-rate advances the pin-of-record (T-D-21,
T-D-24, T-D-37).

#### Absolute revisions out, never mutation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-delta-only-rtr`

The only output of a correction is a new window revision and, when the parent is complete, a new
parent revision. Prior revisions and deliveries are read, never written. Rating computes no delta.

#### One math, run twice

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-same-math-rtr`

There is no retro-specific formula: a correction is an ordinary evaluation over the new inputs.

### 2.2 Constraints

#### Posted-period immutability is Billing's

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-posted-immutability-rtr`

Rating guarantees that a revision is complete, monotonic and reproducible; Billing guarantees that a
posted invoice changes only through credit/debit notes computed from aggregate targets (Atlas C08;
DESIGN §2.2).

#### Period state is not an input

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-periodstate-required-rtr`

No evaluation reads Billing period state; a missing or unknown state blocks nothing (T-D-45). This
replaces the earlier `periodState`-routing and Rating-fence rules (T-D-28, T-D-42).

#### Correction dedup owner: Rating

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-delta-dedup-owner-rtr`

Rating guarantees at most one window revision per input digest and one parent revision per child
vector (T-D-11 as amended); Billing deduplicates deliveries on `(fact_id, result_revision)`.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-rtr`

- **`InputGeneration`** — monotonic counter on `rating_child_window`, bumped by every input change.
- **`RatingReason`** — `initial | usage_change | fact_change | evidence_change | attribution_change |
  admin_rerate | retry`; recorded on the window result.
- **`RerateRun`** — selector (tenant, subscriptions, fact or period range, `catalog_version < V`,
  `engine_version < E`), requester, reason, enumerated child count, status.
- **`BitemporalStamps`** — per line: interval range and max acceptance time of contributing records,
  `rated_at`, `catalog_version` and its `PinFrontier.advanced_at`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-correction-wrapper-rtr`

- **`Rater`** (slice 14) — performs every re-evaluation.
- **`ParentRollup`** (slice 15) — re-rolls the parent.
- **`RerateService`** — creates a `RerateRun`, enumerates matching children, enqueues them with
  reason `admin_rerate` at a bounded rate; resumable (children already rated after the run start are
  skipped).
- **`ReplayVerifier`** — samples final results, rebuilds `EvaluationInput` from the manifest, calls
  `evaluate` at the recorded `engine_version`, compares `input_digest` and amounts.

### 3.3 API Contracts

The rating contract is `rating_core::evaluate`; corrections add no second entry point.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-delta-envelope-rtr`

**Correction envelope** = the next `BillableItemDeliveryV1` of the fact (DESIGN §3.3) with
`result_revision = n + 1`, `previous_result_revision = n`, and provenance pointing at the changed
`window_revision`s. Administrative re-rate: `RatingRunControlV1::request_rerate` /
`POST /bss-rating/v1/reratings` (`rerate × execute`) with a selector and a mandatory reason; the
receipt returns the run id and the enumerated child count before any child is enqueued.

### 3.4 Internal Dependencies

Slices [`12`](./12-usage-ingestion-normalization.md), [`13`](./13-q-store-attribution.md) (input
changes), [`14`](./14-unit-synthesis-period-tick.md) (rater), [`15`](./15-rated-output-balance-effects.md)
(revisions), [`16`](./16-billing-handoff-operations.md) (delivery). The step math of slices 02–07 and
09 runs unchanged.

### 3.5 External Dependencies

| Dependency | What it provides | Contract |
|------------|------------------|----------|
| usage-collector | late records, invalidations, replacements | SEAMS U-1, U-3 — `[DEPENDENCY GAP R-01]` |
| usage emitter (owner unassigned, overlay T6) | coverage re-declarations | SEAMS U-10 — `[DEPENDENCY GAP R-21]` |
| pricing | pinned documents; future-only dating | SEAMS P-2, P-3 — `[DEPENDENCY GAP R-02]` |
| subscriptions | new fact versions, segment versions | SEAMS S-2, S-3 — `[DEPENDENCY GAP R-03, R-25]` |
| Billing | applies revisions to drafts or posts notes | SEAMS L-1, §L C08 — `[DEPENDENCY GAP R-05]` |

### 3.6 Interactions and Sequences

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-open-period-correction-rtr`

**Late record before the parent is delivered**: the record increments its counter; the child is
re-evaluated provisionally (or, if already final, gets a new window revision with its pin-of-record);
the parent has not been delivered yet, so its first delivery already contains the record.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-posted-period-correction-rtr`

**Correction after delivery** (Billing may have frozen or posted the group): the child gets window
revision `n + 1`; the parent roll-up produces `result_revision m + 1` once all expected children are
final; Billing computes the rounded difference against its accepted target and issues a
credit/debit note. Rating's behaviour is identical whether or not Billing has posted.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-negative-usage-reversal-rtr`

**Invalidation / negative quantity** — DESIGN Flow B. Worked example (Atlas F-A3/F14, VM hours at
0.047 EUR, monthly window, amounts in EUR, stored as exact minor-unit rationals):

```text
Nov 03  October final: 10 VM·h → child r1 0.470 → parent rev 1 = 0.470 → Billing invoice 0.47
Nov 10  invalidation of U-10 (VM stopped 18:10, not 19:10) → Q 9 → child r2 0.423 → parent rev 2
        Billing: target 0.42 vs accepted 0.47 → credit −0.05 (pending posting)
Nov 12  invalidation of U-9 → Q 8 → child r3 0.376 → parent rev 3
        Billing: target 0.38 vs latest accepted 0.42 (incl. the pending credit) → credit −0.04
        final receivable 0.38, never 0.33
```

Worked example (Atlas F26, hourly volume): an issued hour at Q = 9 → 0.18; corrected to Q = 10 → the
whole hour moves to the 0.015 band: 0.15; Billing credits 0.03 at the group aggregate. Pricing only
the extra unit would be wrong. Replaying the same invalidation produces no second credit (its key is
absorbed; the input digest is unchanged).

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-reattribution-rtr`

**Re-attribution** (a new segment version moves an interval to another line or payer): the affected
records are un-counted from the old aggregation key and counted under the new one in one transaction
(slice 12 §4.2); both children are re-evaluated; both parents get new revisions. Corrections to a
prior payer stay in that payer's billing group (Atlas D08).

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-rtr`

No table of its own; uses `rating_window_result`, `rating_fact_result`, `rating_child_window`,
`rating_rerate_run` (DESIGN §3.7). All inputs a replay needs are retained ≥ 7 years, independent of
the collector's retention floor.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-rtr`

No additional deployable; `RerateService` and `ReplayVerifier` run under the gear lifecycle, the
verifier as a lease-guarded singleton per tenant shard.

## 4. Additional Context

### 4.1 Replay, Re-evaluation and the Pin of Record (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-snapshot-replay-rtr`

- **Replay** of `(child_id, r)` rebuilds `EvaluationInput` from the manifest stored with revision `r`
  (catalog documents at its `catalog_version`, fact and subscription versions, per-slice quantities
  with `q_version`s, layout, evidence refs) at its `engine_version`. The result MUST equal the stored
  lines and `input_digest`; any difference is a determinism defect (page).
- **Input correction** of a final child: pin-of-record, latest fact version, current counters and
  evidence. Because pricing's dating is future-only (SEAMS P-3), the pin-of-record equals what any
  later frontier would give for the window — except after a corrective publish, which is exactly what
  the pin protects against until an administrative re-rate is approved.
- **Administrative re-rate** (T-D-21) is the only operation intended to change already-final amounts
  without an input change: after a corrective catalog publish, a new `engine_version`, or a
  retroactive subscription correction. Each selected child is evaluated with the current frontier and
  engine and its pin-of-record advances; **later input corrections use the advanced pin** (T-D-24), so
  an approved repricing is never reverted by replaying an obsolete pin.
- An engine change is never applied implicitly to final children.

### 4.2 Correction Keys and Idempotency (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-correction-key-rtr`

- The PRD correction key `(unitKey[, slice], prior-rated-version, snapshot)` is realized as
  `(child_id, window_revision, previous window_revision, snapshot_id)` at child level and
  `(fact_id, result_revision, previous_result_revision, manifest_digest)` at delivery level
  (slice 15 §4.3).
- The rater writes revision `n + 1` only when the input digest or evidence differs from revision `n`;
  redelivery and concurrent workers never create a second revision for the same inputs.
- The parent writes revision `m + 1` only when its child vector or fact version differs.

### 4.3 Posted Periods (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-periodstate-routing-rtr`

- Rating has no period routing. Every revision is delivered the same way.
- Billing's rule (Atlas C08): stale revision ignored; identical duplicate no-op; same revision with a
  different digest quarantined; a higher revision may skip earlier ones; after freeze, the rounded
  aggregate difference against the latest accepted target (incl. pending postings) becomes ordered
  posting obligations; a rounded no-op still advances the accepted head (F15).
- Rating's obligation to Billing: revisions are monotonic per fact, each is complete for its fact,
  and the previous revision is named.

### 4.4 Reversal Math and Emission Guards (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-reversal-guards-rtr`

- Reversal is re-evaluation over the reduced counter; no refund formula exists.
- Commitment-pool refills are dormant (R-11).
- **Non-negative lines**: the emission guard (slice 01 §4.4) applies to each line of a result; a
  window quantity below zero fails closed (`negative_window_quantity`).
- **Bitemporal audit**: per line, the usage interval range and max acceptance time of contributing
  records, `rated_at`, `catalog_version` and the pin's `advanced_at`.
- **Precision**: exact rationals throughout (T-D-46).

### 4.5 Acceptance Vectors

| Fixture | Required result |
|---|---|
| F14 | v1 0.470 → v2 0.423 → v3 0.376; Billing 0.47 → 0.42 → 0.38; obligations −0.05, −0.04 |
| F15 | v3 before v2 and duplicate v3: v3 accepted, v2 and the duplicate ignored; 0.421 → 0.424 advances the revision with no rounded adjustment |
| F21 | an older-input evaluation finishing late is superseded |
| F26 | whole-hour re-tiering on correction; replay produces no second credit |
| F32 | parent revisions follow child vectors (0.34 → 0.33 → 0.35); vector CAS |

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-posted-period-protection`, `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve`, `cpt-cf-bss-rating-fr-usage-corrections`

- **PRD**: §6.10, §6.1 (`fr-idempotency`, `fr-separation`), §12 AC 9–10.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §3.6 (Flows A′, B, D, admin re-rate), §4.1, §4.2.
- **Contracts**: [`../SEAMS.md`](../SEAMS.md) U-1, U-3, U-10, P-3, S-3, §L C07/C08.
- **Decisions**: T-D-04, T-D-11, T-D-21, T-D-24, T-D-37, T-D-45, T-D-46; open R-01…R-05, R-11,
  R-21, R-25 — [`../DECISIONS.md`](../DECISIONS.md).
- **Slices**: [`12`](./12-usage-ingestion-normalization.md), [`13`](./13-q-store-attribution.md), [`14`](./14-unit-synthesis-period-tick.md), [`15`](./15-rated-output-balance-effects.md), [`16`](./16-billing-handoff-operations.md).
