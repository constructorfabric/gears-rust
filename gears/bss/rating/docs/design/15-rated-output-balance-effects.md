Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Window Results, Parent Roll-Up & Snapshots (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: 14-unit-synthesis-period-tick | Downstream: 16-billing-handoff-operations | Owners: BSS Rating team -->

# DESIGN — Window Results, Parent Roll-Up & Snapshots (Slice 15, pipeline)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-rated-output`

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
  - [4.1 Outcome → Results (normative)](#41-outcome--results-normative)
  - [4.2 Result Revisions (normative)](#42-result-revisions-normative)
  - [4.3 Line Identities and Idempotency (normative)](#43-line-identities-and-idempotency-normative)
  - [4.4 Exact Amounts (normative)](#44-exact-amounts-normative)
  - [4.5 CommitmentBalanceEffect (normative)](#45-commitmentbalanceeffect-normative)
  - [4.6 Acceptance Vectors](#46-acceptance-vectors)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The durable financial record of Rating. Each **final** evaluation of a child window appends one
**window result** (exact lines, the input manifest, the digest); each complete set of final children
of a parent fact appends one **parent result** at a monotonic `result_revision` — a complete,
absolute, exact replacement of the fact's previous result (T-D-45). Lines reference content-addressed
**snapshots**. Rating stores no deltas and decides nothing about invoices: Billing compares
revisions with its accepted target (slice 16).

Provisional evaluations are estimates on the child row; they are never results and never delivered
(F34).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-fr-snapshot-carry` | `rating_snapshot` per line; `snapshot_id` and provenance on each delivered line (§4.1). |
| `cpt-cf-bss-rating-fr-idempotency` (delta family) | `(child_id, window_revision)`, `(fact_id, result_revision)` and `delivery_id`; Billing derives monetary deltas (§4.3). |
| `cpt-cf-bss-rating-fr-separation` | Insert-only results (§4.2). |
| `cpt-cf-bss-rating-fr-posted-period-protection` | Rating supplies complete revisions with `previous_result_revision`; Billing protects posted invoices (slice 16). |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-resilience` | Results, snapshots, child pointer and roll-up work commit in one transaction; replay is a no-op. |
| `cpt-cf-bss-rating-nfr-audit-segregation` | Manifest + digest reproduce every amount; revisions are never rewritten. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-rob`

`rating` crate: `infra/storage/result_repo.rs`, `app/rollup.rs`, `domain/snapshot.rs` (canonical JSON +
hash, pure), `domain/exact.rs` (re-export of the core's `ExactAmount`).

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-immutable-charges-rob`
  Window results, parent results, snapshots and deliveries are never updated or deleted.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-idempotent-publish-rob`
  A result's identity derives from what caused it; writing it twice is a no-op.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-persist-sealed-rob`
  The store records the core's outcome verbatim; roll-up only sums exact amounts; nothing is rounded.

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-delta-dedup-here-rob`
  Rating owns result idempotency (T-D-11 as amended): a parent revision is published once per
  child-revision vector. Billing deduplicates on `(fact_id, result_revision)` as its own rule.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-record-not-compute-rob`
  No currency rounding, no floor/cap execution, no tax (T-D-46, R-22).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-complete-or-nothing-rob`
  A parent result exists only for a complete expected child set; partial months are never results
  (Atlas D15).

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-rob`

- **`WindowResult`** — `(child_id, window_revision)` + `input_generation`, manifest (`fact_id,
  fact_version, fact_digest, catalog_version, subscription_version, slice q/q_version,
  layout_version, scope_ref, coverage_refs, usage_record_digest, segments_through_seq,
  finalization_policy_ref, evidence_mode, engine_version`), `reason`, `input_digest`,
  `lines: RatedLine[]`, `obligations`.
- **`RatedLine`** — `{ line_key, slice_start, slice_end, sku_id, plan_id, price_id, charge_kind,
  model_kind, quantity, billable_quantity, exact_minor: ExactAmount, currency, gl_code,
  tax_category, invoice_line_template, snapshot_id, lineage }`.
- **`FactResult`** — `(fact_id, result_revision)` + `fact_version`, `child_vector`,
  `window_manifest {expected_keys, results: [(key, window_revision, input_digest)], complete: true}`,
  `lines: ParentLine[]`, `obligations`, `zero_result`, `evidence_mode`.
- **`ParentLine`** — `{ line_key, invoice_line_key, kind, quantity?, unit?, exact_minor, currency,
  gl_code?, tax_category?, invoice_line_template?, rounding_policy = HALF_EVEN, snapshot_id
  (composite over the line's child snapshots, DESIGN §4.1),
  provenance: [{price_id, sku_id, plan_id, catalog_version, slice, window_key?, window_revision?,
  quantity?, exact_minor}] }`.
- **`RatingSnapshot`** — `{ snapshot_id, body }` (DESIGN §4.1).

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-rated-output`

| Component | Responsibility |
|---|---|
| `ResultStore::persist(tx, child, outcome, manifest)` | Called by the rater for a final evaluation: snapshot(s), window result, child pointer, roll-up work. |
| `ParentRollup` | DESIGN §3.6 parent roll-up: completeness check, vector CAS, parent result, delivery row. |
| `SnapshotHasher` | Canonical JSON + SHA-256 (pure). |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-rated-output-rob`
  Internal `persist` and `rollup(fact_id)`; external reads through `RatingRunReadV1` (`get_run`,
  `find_runs`, `window_results`, `deliveries_since`) — DESIGN §3.3.

### 3.4 Internal Dependencies

Called by the rater (slice 14); writes delivery rows consumed by slice 16.

### 3.5 External Dependencies

None.

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-persist-outcome-rob`
  **Persist a final child outcome** (inside the rater transaction):

  ```text
  INSERT rating_snapshot (each distinct line snapshot) ON CONFLICT DO NOTHING
  INSERT rating_window_result (child_id, window_revision = current + 1, input_generation, manifest,
         input_digest, lines, obligations, state = final)
  UPDATE rating_child_window SET current_revision = current + 1, status = final,
         pin_of_record = (reason = admin_rerate or pin_of_record is null)
                         ? manifest.catalog_version : pin_of_record,   -- T-D-37, T-D-24
         current_input_digest = …
  Outbox.enqueue(rating.rollup, fact_id)
  ```

  Parent roll-up: DESIGN §3.6 `seq-parent-rollup`.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-persist-delta-rob`
  **Worked example** (Atlas F32 + F26, hourly volume, bands `[0, 10)` €0.02, `[10, ∞)` €0.015;
  amounts in EUR for readability, stored as exact minor-unit rationals):

  | Event | Child A (10:00) | Child B (11:00) | Parent result | Billing (rounded target, obligation) |
  |---|---|---|---|---|
  | month closes, both final | r1: Q=8 → 0.16 | r1: Q=12 → 0.18 | rev 1 = 0.34 | 0.34, invoice |
  | A corrected (Q 8 → 7.5) | r2: 0.15 | — | rev 2 = 0.33 | 0.33, credit −0.01 (pending) |
  | B corrected (Q 12 → 13.333…) | — | r2: 0.20 | rev 3 = 0.35 | 0.35, debit +0.02 after the credit |

  Rev 3's vector is `[(A, r2), (B, r2)]`; a roll-up that captured `[(A, r1), (B, r2)]` would fail the
  vector CAS and be retried — a stale vector can never overwrite a fresher result.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-rated-output-rob`
  `rating_window_result`, `rating_fact_result`, `rating_fact_head`, `rating_snapshot`,
  `rating_delivery` — DESIGN §3.7.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-rob`
  Persistence runs in the rater transaction; roll-up runs on `rating.rollup` workers.

## 4. Additional Context

### 4.1 Outcome → Results (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-outcome-mapping-rob`

| Outcome element | Persisted as |
|---|---|
| each `RatedLine` | a line of the window result |
| `prepaid_drawdown` in-commit line (dormant, R-11) | a line with amount 0 and notional value in lineage |
| `PeriodFloorCapObligation` | parent result `obligations`, delivered; Billing executes (R-22) |
| `TrueUpObligation` (dormant) | parent result `obligations` |
| snapshot | `rating_snapshot`, referenced by id |
| lineage (pre/post overlay amounts, applied overlay ids, band placement, ASC 606 refs) | `lines[].lineage` |
| provisional outcome | `rating_child_window.provisional_outcome` (estimate, overwritten, never delivered) |

### 4.2 Result Revisions (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-rated-store-rob`

- `window_revision` is dense per child (`1, 2, …`); `result_revision` is dense per fact.
- A window revision is written only when the input digest differs from the current one or evidence
  changed; amounts may still be equal (then the parent vector changes and a new parent revision with
  equal amounts is published — Billing advances its head without a posting, Atlas F15).
- A parent revision is written only when its child vector or fact version differs from the last one.
- A removed source record gives a zero child result, never a missing expected child (Atlas C10).
- Retention ≥ 7 years; results and snapshots referenced by a delivery are never pruned.

### 4.3 Line Identities and Idempotency (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-delta-dedup-rob`

- `line_key` (T-D-53) = `{fact_id}|{meter or charge component}|{dimension_key}|{kind}|{gl/tax
  component}`; stable across revisions and windows of the fact. Hourly lines of the same component
  share one `line_key`; their hours live in provenance.
- `invoice_line_key` = SHA-256 of `{billing_group_id, sub_line_key, meter/item, dimension_key, kind,
  currency, unit, gl_code, tax_category, invoice_line_template, rounding_policy}`; Billing sums all
  facts' contributions with the same key and rounds once (T-D-46).
- Neither key contains an hour, a child revision, or a `price_id` (a price change inside a period
  moves money between provenance entries, not between invoice lines).
- Idempotency: `(child_id, window_revision)`; `(fact_id, result_revision)`; `delivery_id =
  UUIDv5(NS_RATING_DELIVERY, "{fact_id}|{fact_version}|{result_revision}")`; `run_id =
  UUIDv5(NS_RATING_RUN, same string)`.
- The PRD correction key `(unitKey[, slice], prior-rated-version, snapshot)` is carried by
  `(child_id, window_revision → window_revision − 1, snapshot_id)` at child level and
  `(fact_id, result_revision, previous_result_revision)` at delivery level.

### 4.4 Exact Amounts (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-provisional-fx-rob`

- `ExactAmount` = reduced rational in **minor units** of the line currency (T-D-46): `{numerator,
  denominator}`, `denominator > 0`, zero = `0/1`. €0.423 = 42.3 minor = `423/10`.
- Inputs: pricing `MinorAmount` (integer minor) and `RateMinor` (integer 10⁻⁹ minor) are exact;
  quantities are finite decimals; proration fractions are rationals; their products are exact.
- Roll-up sums rationals; no intermediate rounding anywhere. Representation overflow fails closed
  (`precision_overflow`), never truncates.
- Billing converts with `round_half_even(numerator / denominator)` to integer minor units per
  `invoice_line_key` aggregate. Two hourly 0.5-minor contributions roll up to 1 minor (F27).
- Invoice-period FX (dormant, R-07) would be an ordinary new revision.

### 4.5 CommitmentBalanceEffect (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-balance-effect-rob`

Dormant (T-D-10, T-D-27, R-11): no consumer exists. Target: the effect of a final window result on
each observed pool (draw, refill, or zero-draw with overage marker) is appended to
`rating_balance_effect` in the rater transaction, keyed `(child_id, window_revision, pool_id)`, and
served to Contracts through the inbox/outbox pattern; Contracts serializes `balanceVersion`
(UNKNOWN / EXTERNAL CONTRACT REQUIRED).

### 4.6 Acceptance Vectors

| Fixture | Given | Required result |
|---|---|---|
| F02 | 0.047 EUR/VM·h, 10 h | exact 0.470 (47/1 minor); Billing 0.47 |
| F03 | 2.5 VM·h | exact 0.1175 EUR = 11.75 minor = 47/4; Billing 0.12 |
| F19 | €1 prorated 1/3 | exact 100/3 minor; Billing 0.33 |
| F21 | older-input run finishes after a newer generation | superseded; cannot publish a higher revision |
| F27 | two hourly 0.005 contributions, one `invoice_line_key` | parent exact 1/1 minor → 0.01 |
| F32 | corrections to different hours | revisions 0.34 → 0.33 → 0.35; vector CAS |

## 5. Traceability

- **DESIGN**: §3.6, §3.7, §4.2.
- **Decisions**: T-D-11, T-D-39, T-D-45, T-D-46, T-D-53, R-11, R-22.
- **Atlas**: C07, D09, D10, D15, F02, F03, F19, F21, F27, F32.
