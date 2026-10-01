Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Multi-Currency & FX (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: Pricing (per-market rows), Subscriptions ((currency, region) binding), FX source (none yet) | Downstream: step 9, Billing | Owners: BSS Rating team -->

# DESIGN — Multi-Currency & FX (Slice 7)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-currency-fx`

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
  - [4.1 Currency Role Separation (normative)](#41-currency-role-separation-normative)
  - [4.2 FX Policy Semantics (normative)](#42-fx-policy-semantics-normative)
  - [4.3 FX-Lock Snapshot Segment (normative)](#43-fx-lock-snapshot-segment-normative)
  - [4.4 Ordering and Precision at the FX Boundary (normative)](#44-ordering-and-precision-at-the-fx-boundary-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice is the **step-8 evaluator** in `rating-core`. It keeps three currency roles apart —
**price currency** (the selected row's currency), **billing currency** (the subscription's
`(currency, region)` binding), **presentment currency** (display only, outside Rating) — and is the
only place a conversion could occur.

No gear provides versioned, pinnable FX rates to Rating (SEAMS F-1…F-3): `rate-provider` is a
latest-only plugin of the ledger, and the ledger mints its rate snapshots only when it posts.
**Launch behaviour is native currency only** (DESIGN §2.2, R-07): pricing publishes per-market rows
per `(currency, region)`, the subscription binding selects the row, and billing currency must equal
price currency. A mismatch fails the child closed with `fx_not_supported`. The FX policies in §4.2
are the specified target behaviour and activate when a pinnable rate-snapshot contract exists
(SEAMS J-8).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-multi-currency` | Roles separated (§4.1); native currency at launch; per-market rows are never FX-derived. |
| `cpt-cf-bss-rating-fr-fx-policy` | Two policies specified (§4.2); inactive until R-07; no implicit or provider-default rate ever. |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-8 slot between the two coupon passes. |
| `cpt-cf-bss-rating-fr-coupon-application-order` | Billing-currency coupons after step 8 (§4.4). |
| `cpt-cf-bss-rating-fr-snapshot-carry` | FX segment of the snapshot (empty at launch) (§4.3). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Native check | One equality check at launch | — |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guard | Mismatch or missing rate record ⇒ typed error | Fixture |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Pinned inputs | Rates (when added) are values of the input | Design |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | `currency` is a scope-key axis: each market is its own row, never derived by FX. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-fx`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `CurrencyRoleResolver`, `FxPolicyApplier` (inactive), billing-currency coupon pass hook | `rating-core` module |
| Infrastructure | None | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Three roles, one conversion point

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-currency-roles-fx`

Price, billing and presentment currency are distinct; the only authoritative conversion is step 8,
and it runs only when billing ≠ price.

#### No unrecorded FX

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-no-implicit-fx`

A converted amount always carries its rate record identity; without a record, evaluation fails
closed.

#### Convert at full precision, never round

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-full-precision-conversion-fx`

Conversion is exact (rational rate × exact amount); Billing rounds to minor units (T-D-46).

### 2.2 Constraints

#### FX tables and policy are Finance's

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-finance-sor-fx`

Rating never sources, derives, inverts or triangulates a rate. `[DEPENDENCY GAP R-07]` No FX owner
exposes pinnable rates today; J-8 names the required change (ledger or a Finance gear exposing
`lock_rate` / `read_snapshot`).

#### The (currency, region) binding is consumed, never re-derived

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-binding-consumed-fx`

Billing currency is read from the stored subscription version (SEAMS S-1); Rating never infers it.

#### Presentment is outside rating-core

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-presentment-outside-fx`

Display conversion is non-authoritative and not computed here.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-fx`

- **`CurrencyRoles`** — `price_currency` (selected row), `billing_currency` (subscription binding).
- **`FxRateRecord`** (target) — `rate_ref` (pinnable id from the FX owner), pair `(price →
  billing)`, rate, policy kind (`per_window_rate_lock | invoice_period`). No source at launch.
- **`FxApplication`** (target) — pre/post amounts, `rate_ref`, provisional flag.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-conversion-fx`

- **`CurrencyRoleResolver`** — binds roles; equal ⇒ skip conversion; different ⇒ `fx_not_supported`
  at launch, `FxPolicyApplier` once R-07 is resolved.
- **`FxPolicyApplier`** (target) — §4.2.
- **Billing-currency coupon pass** — invokes slice 06 with `Pass::Billing`.

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-convert-fx`

Internal: `apply_fx(&CouponLine, &CurrencyRoles, Option<&FxRateRecord>) -> Result<FxLine,
EvaluationError>`. Errors: `fx_not_supported` (launch), `fx_rate_missing`, `fx_pair_missing`
(exact pair direction required; no inversion).

### 3.4 Internal Dependencies

Upstream: slice 02 (row currency), slice 06 (price-currency pass). Downstream: slice 06
(billing-currency pass), slice 01 step-9 guards, slice 09 (floor/cap comparison currency).

### 3.5 External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | Per-`(currency, region)` rows, minor/nano-minor amounts | Fields exist (SEAMS P-2, P-8); read API MISSING (R-02) |
| Subscriptions | `(currency, region)` binding | ASSUMED (SEAMS S-1) |
| FX owner (ledger / Finance) | Pinnable rate snapshots | MISSING (SEAMS F-1…F-3, R-07) |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-step8-convert-fx`

1. Resolve `CurrencyRoles` from the selected row and the subscription version.
2. Equal ⇒ no conversion; FX segment empty.
3. Different ⇒ `fx_not_supported` (launch). Target: load the pinned `FxRateRecord`, convert at full
   precision, record `rate_ref`.
4. Apply billing-currency coupons (slice 06); hand to step 9.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-close-delta-fx`

Target (R-07) — invoice-period FX: the child is evaluated with a provisional rate; once the
close-time rate record exists the child is re-evaluated with it, producing a new window revision and
a new parent revision like any other correction (DESIGN §3.6 Flow B). Finalization waits for the
close-time rate (pending reason `fx_rate_pending`), so Billing receives one complete revision rather
than a provisional one. Not active at launch.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-fx`

None. When FX is enabled, the rate record used is stored with the window result so replay does not
read the FX owner.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-fx`

Part of `rating-core`.

## 4. Additional Context

### 4.1 Currency Role Separation (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-currency-roles-fx`

- **Price currency**: currency of the selected row; per-market rows are first-class; a missing market
  row is `no_eligible_window`, never FX-derived.
- **Billing currency**: the subscription's `(currency, region)` binding.
- **Presentment currency**: outside Rating.
- Conversion iff billing ≠ price; at launch that case fails closed (`fx_not_supported`).
- One currency per line; lines of one invoice share the billing currency.

### 4.2 FX Policy Semantics (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-fx-policy-fx`

*Activates with R-07.*

- **Per-window rate lock**: the locked rate is final when rated; `rate_ref` recorded.
- **Invoice-period FX**: provisional rate on provisional evaluations; the child finalizes only with
  the close-time rate record; a later rate correction is a new revision (Billing derives any
  difference).
- The rate record must carry the exact pair direction (price → billing); no inversion or
  triangulation.
- Missing record with billing ≠ price ⇒ fail closed.

### 4.3 FX-Lock Snapshot Segment (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-fx-lock-segment-fx`

- The FX segment of the `rating_snapshot` body holds the `rate_ref`(s) used; empty for native
  currency (all launch lines).
- A later re-evaluation with a different rate produces a new snapshot and revision; recorded snapshots
  never change.

### 4.4 Ordering and Precision at the FX Boundary (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-fx-ordering-fx`

- Order: price-currency coupons (step 7) → conversion (step 8, skipped when native) →
  billing-currency coupons → step-9 guards. The billing-currency pass runs even when conversion is
  skipped, keeping coupon placement invariant.
- Conversion computes at full precision and never rounds; Billing rounds to minor units.
- Period floor/cap comparison in billing currency uses the same rate record as step 8 (slice 09).

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-multi-currency`, `cpt-cf-bss-rating-fr-fx-policy`

- **PRD**: §6.9, §17.1 steps 2 and 8, §12 AC 8.
- **Seams / contracts**: [`../SEAMS.md`](../SEAMS.md) F-1…F-3, P-8, S-1, J-8.
- **Decisions**: R-07, T-D-46 — [`../DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`06-coupons.md`](./06-coupons.md), [`08-retroactivity-corrections.md`](./08-retroactivity-corrections.md), [`09-period-plan-change.md`](./09-period-plan-change.md).
