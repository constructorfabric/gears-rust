Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Base Selection & Eligibility (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: pinned pricing plan documents, subscription version | Downstream: slice 03 | Owners: BSS Rating team -->

# DESIGN — Base Selection & Eligibility (Slice 2)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-selection-eligibility`

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
  - [4.1 Selection Algorithm (normative)](#41-selection-algorithm-normative)
  - [4.2 Eligibility Classes and Cohort Generations (normative)](#42-eligibility-classes-and-cohort-generations-normative)
  - [4.3 Phase Semantics (normative)](#43-phase-semantics-normative)
  - [4.4 Selection Failure Taxonomy (normative)](#44-selection-failure-taxonomy-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Steps 1–2 of the evaluation: resolve the active plan phase (`phase_id`) at the line's instant from
the subscription version, then select **exactly one** price row from the pinned plan document on
the full canonical scope key, per emitted `charge_kind`. Every later step prices the row selected
here, so selection is strictly fail-closed: no fallback row, no nearest window, no selection by
`activatedAt` alone, no matching of phase names.

The slice owns the selection policy — candidate set (including the phase-invariant usage
fallback), the `price_eligibility` class order, cohort selection by the subscription's pinned price
id, the single-match assertion, and its failure values. It does not own the key definition
(pricing, adopted in slice 01 §4.1), overlays (slice 04), or meter mapping (slice 03).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-base-catalog-selection` | Select the row whose window interval contains the instant, on the full 10-axis key; zero or several survivors fail closed (§4.1, §4.4). |
| `cpt-cf-bss-rating-fr-price-eligibility-grandfathering` | Class order `existing_grandfathered > new_subscriptions_only > all_subscriptions`; within grandfathered, the row whose `cohort` equals the cohort of the subscription's pinned price id (§4.2). |
| `cpt-cf-bss-rating-fr-plan-phases` | Active `phase_id` from the subscription version's phase timeline; phase-invariant usage rows cover phases without a phase-specific usage row (§4.3). |
| `cpt-cf-bss-rating-fr-evaluation-order` (steps 1–2) | Called first by `StepPipeline` (slice 01). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | `CandidateSetBuilder` | In-memory filter over the rows of one plan document (typically < 100 rows) | Benchmark |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Purity | Stateless per call | Design |
| `cpt-cf-bss-rating-nfr-resilience` | §4.4 | Every unresolved selection is a typed error | Fixture corpus |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Adopt the pricing scope key verbatim; generation by the pinned price id's cohort. |
| `cpt-cf-bss-pricing-adr-canonical-scope-key` (adopted) | Key definition; pricing is its SoR. |
| `cpt-cf-bss-pricing-adr-grandfathering-cohort-axis` (adopted) | `cohort` = the cutover instant of a generation; N generations may coexist. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-sel`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `SelectionEvaluator` and its value types | module `rating_core::selection` |
| Infrastructure | None — reads the pinned plan document passed in the input | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### The full key or nothing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-full-key-only-sel`

Uniqueness exists only on the full key. Shorter tuples are legitimately non-unique (hybrid
`recurring` + `usage` rows; grandfathered generations beside their successor); the evaluator never
selects, asserts or fails on a partial key.

#### Class order first, then the cohort pin

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-class-then-cohort-sel`

The class order picks the most specific eligible class; inside `existing_grandfathered` the pinned
price id's cohort picks the generation. `activatedAt` decides class membership only.

#### Gaps are judged on the resolved set

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-resolved-set-no-gap-sel`

The no-gap rule is applied after the phase-invariant fallback: a phase covered only by the
phase-invariant usage row is not a gap.

### 2.2 Constraints

#### No silent fallback

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-silent-fallback-sel`

No eligible row on the full key ⇒ `no_eligible_window` for billable usage — never a default row
or a zero rate.

#### `phase` is a `phase_id`

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-phase-id-only-sel`

The phase axis is a uuid; trial/intro/evergreen are display names. Non-phased and one-time rows
carry the plan's terminal `phase_id`.

#### Selection inputs are frozen

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-frozen-selection-inputs-sel`

Phase timeline, `activated_at` and pinned price ids come from the subscription version in the
input; windows and rows come from the pinned plan document. No lookup happens during selection.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-sel`

- **`PhaseTimeline`** — ordered `(phase_id, [from, to))` intervals of the subscription version.
- **`SelectionKey`** — the materialized 10-axis tuple for a line: `plan_id`, `currency` and
  `region` from the subscription's `(currency, region)` binding, `price_overlay = base`, `phase`,
  `price_eligibility`, `charge_kind`, `cohort`, `meter`, `dimension_key`.
- **`CandidateSet`** — rows of the plan document whose window interval (`windows[].intervals[]`
  with state `scheduled`/`active`/`expired`) contains the instant, for the resolved `phase_id` or
  the terminal (phase-invariant) `phase_id`, and the line's `charge_kind`/`meter`/`dimension_key`.
- **`EligibilityClass`** — ordered `existing_grandfathered > new_subscriptions_only > all_subscriptions`.
- **`CohortPin`** — the subscription version's pinned price id(s) and their `cohort`.
- **`SelectionOutcome`** — selected row (`price_id`, `sku_id`, `plan_id`, `scope_key`), window id,
  winning class, matched cohort, `phase_id`, `phase_fallback: bool`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-selection-evaluator-sel`

`SelectionEvaluator` = `PhaseResolver` → `CandidateSetBuilder` → `EligibilityClassFilter` →
`CohortGenerationSelector` → `SelectionGuard` (exactly one survivor).

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-select-base-row-sel`

`select(&LineContext, &PlanDocument, &SubscriptionVersion) -> Result<SelectionOutcome, EvaluationError>`
— internal to `rating-core`; errors are the §4.4 subset of the slice 01 taxonomy.

### 3.4 Internal Dependencies

Called by slice 01's `StepPipeline`; its outcome feeds slice 03 (model), slice 04 (overlays on the
row), slice 07 (currency check).

### 3.5 External Dependencies

| Dependency | What arrives (by value) | Contract |
|------------|------------------------|----------|
| pricing | plan document: `prices[]` with `scopeKey`, `chargeKind`, eligibility, cohort; `windows[]` per scope key; `phases` | SEAMS P-2, P-4 — [DEPENDENCY GAP R-02] |
| subscriptions | phase timeline, `activated_at`, pinned price ids + cohort, `(currency, region)` | SEAMS S-1 — [DEPENDENCY GAP R-03] |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-select-base-row-sel`

**Select the base row** (steps 1–2 of `cpt-cf-bss-rating-seq-evaluate-tariff`): resolve phase →
build candidates → class filter → cohort filter → assert one → emit outcome. Hybrid plans run the
flow once per `charge_kind` and select one `recurring` and one `usage` row.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-phase-fallback-sel`

**Phase-invariant fallback**: a usage line in a phase without a phase-specific usage row selects the
terminal-`phase_id` usage row and sets `phase_fallback = true` in lineage; if a phase-specific usage
row exists for that phase, it wins.

Example: plan P has phases `trial` (uuid T) and `evergreen` (uuid E), one usage row on the terminal
phase and one recurring row per phase. Usage at a trial instant selects the terminal usage row
(`phase_fallback = true`); the recurring line selects the row keyed `phase = T`.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-sel`

None.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-sel`

Part of `rating-core` (slice 01 §3.8).

## 4. Additional Context

### 4.1 Selection Algorithm (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-selection-algorithm-sel`

For a line at instant `t` (the slice start for a usage slice, the period start for a period line,
the record `window_start` for a `usage_event`), per `charge_kind`:

1. Resolve the active `phase_id` at `t` from the phase timeline. None ⇒ `missing_phase_context`.
2. Candidates: rows whose scope key matches `plan_id`, `(currency, region)`, `price_overlay = base`,
   `charge_kind`, `meter`, `dimension_key`, and `phase ∈ {phase_id, terminal}`, with a window
   interval `[effective_from, effective_to)` containing `t`. Phase-specific wins over terminal
   where both exist for the same remaining key (§4.3).
3. Apply the class order and, within `existing_grandfathered`, the cohort pin (§4.2).
4. Assert exactly one survivor: none on billable usage ⇒ `no_eligible_window`; more than one ⇒
   `selection_ambiguity`.
5. Emit `SelectionOutcome`.

The algorithm is a pure function of the plan document and subscription version; re-evaluation under
the same pin yields the same row.

### 4.2 Eligibility Classes and Cohort Generations (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-eligibility-cohort-sel`

- Order (most specific first): `existing_grandfathered > new_subscriptions_only > all_subscriptions`.
- `new_subscriptions_only` excludes subscriptions whose `activated_at` is before the window's
  `effective_from`; `existing_grandfathered` includes only subscriptions activated before the
  generation's cutover (`cohort`).
- Within `existing_grandfathered`, the generation is the row whose `cohort` equals the cohort of the
  subscription's pinned price id. `activated_at` never selects a generation.
- Outside `existing_grandfathered`, `cohort = none`.
- A pin whose cohort matches no live generation while grandfathered candidates exist ⇒
  `cohort_pin_unmatched`. When no grandfathered candidate is live at `t`, the filter proceeds to the
  next class.

Example: generations with cohorts `2026-01-01` (C1) and `2026-06-01` (C2) are both active on the
same key at `t = 2026-09-20`; a subscription pinned to a price id of C1 selects the C1 row; one
activated on 2026-07-10 with no grandfathered pin selects the `all_subscriptions` row.

### 4.3 Phase Semantics (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-phase-semantics-sel`

- `phase` is a uuid `phase_id`; names are display-only.
- Non-phased and one-time rows carry the plan's terminal `phase_id`. One-time rows select for
  order evaluation only; they are not rated while T-D-18 stands (reversal proposed, R-19).
- Distinct phases may have rows effective at the same instant; `phase` is a key axis, so this is not
  an overlap.
- Usage rows are phase-invariant by default: the terminal-`phase_id` usage row is the
  phase-invariant row; a phase-scoped usage row wins for its phase.
- Order-evaluation contexts (`OrderEvaluationV1`, no subscription): terminal `phase_id`,
  `all_subscriptions`, `cohort = none`.
- **Binding cross-check (R-18)**: when the fact carries a `priceId` (CURRENT traceability tuple) or
  an accepted binding (TARGET Atlas term slice), the row selected on the full key must be that row;
  otherwise the child fails closed `binding_mismatch`. Selection is never replaced by the fact's id:
  the fact proves what was sold, selection proves that the pinned catalog prices it.
- A phase conversion inside a usage aggregation window is a split point (slice 03 §4.3); each slice
  resolves its own phase.

### 4.4 Selection Failure Taxonomy (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-failure-taxonomy-sel`

The subset of the slice 01 §4.4 taxonomy raised here; each carries the materialized `SelectionKey`
and candidate provenance in the exception detail.

| Error | Condition |
|-------|-----------|
| `no_eligible_window` | zero survivors on the full key for billable usage |
| `selection_ambiguity` | more than one survivor on the full key (pricing defect; never tie-broken) |
| `missing_phase_context` | no phase active at `t` |
| `torn_cohort_pin` | grandfathered candidates present but the subscription version has no pinned price id / cohort |
| `cohort_pin_unmatched` | pin present, cohort matches no live generation, grandfathered candidates present |

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-base-catalog-selection`, `cpt-cf-bss-rating-fr-price-eligibility-grandfathering`, `cpt-cf-bss-rating-fr-plan-phases`

- **PRD**: §6.3 `fr-base-catalog-selection`, `fr-evaluation-order` (steps 1–2); §6.5; §17.1 steps 1–2.
- **Contracts**: SEAMS P-2, P-4, S-1, §I (K1–K5, F1) — [`../SEAMS.md`](../SEAMS.md).
- **Decisions**: T-D-01, T-D-08, T-D-18 — [`../DECISIONS.md`](../DECISIONS.md).
- **ADR**: [`../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md`](../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md).
- **Related slices**: [`01-foundation.md`](./01-foundation.md), [`03-metering-models.md`](./03-metering-models.md), [`04-overlays-precedence.md`](./04-overlays-precedence.md).
