Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Integration Contracts (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: pricing, subscriptions, usage-collector, IRM | Downstream: Billing, Orders Lifecycle | Owners: BSS Rating team -->

# DESIGN — Integration Contracts (Slice 11)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-consumer-contracts`

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
  - [4.1 Rating Handoff Contract (normative)](#41-rating-handoff-contract-normative)
  - [4.2 Pricing Read-Model Input Contract (normative)](#42-pricing-read-model-input-contract-normative)
  - [4.3 Subscriptions Input Contract (normative)](#43-subscriptions-input-contract-normative)
  - [4.4 Finance FX Input Contract (normative)](#44-finance-fx-input-contract-normative)
  - [4.5 Promotions Coupon Snapshot Contract (normative)](#45-promotions-coupon-snapshot-contract-normative)
  - [4.6 Billing Delivery and Obligation Contract (normative)](#46-billing-delivery-and-obligation-contract-normative)
  - [4.7 pricingSnapshotRef Segment Map (normative)](#47-pricingsnapshotref-segment-map-normative)
  - [4.8 Canonical Naming (normative)](#48-canonical-naming-normative)
  - [4.9 Contracts & Agreements Input Contract (normative)](#49-contracts--agreements-input-contract-normative)
  - [4.10 Order Evaluation Contract (normative)](#410-order-evaluation-contract-normative)
  - [4.11 Inbox and Recovery (normative)](#411-inbox-and-recovery-normative)
  - [4.12 Provider Test Doubles (normative)](#412-provider-test-doubles-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice specifies the **shape of every contract Rating uses across a gear boundary** and the
adapters that turn upstream facts into immutable local copies. Whether each contract exists is
recorded once in [`../SEAMS.md`](../SEAMS.md); where it does not, the shape below is Rating's
**requirement** on that gear (SEAMS §J), aligned with the Seam Atlas v2 baseline where the Atlas is
consistent with this repository (T-D-43; SEAMS §L).

All boundary reads are ClientHub SDK traits (in-process or gRPC OoP with the same trait). Events,
when a broker delivers them, are an additional source for the same inbox (T-D-51).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-contract-rating-handoff` | §4.1 — `rating-core` API (intra-gear). |
| `cpt-cf-bss-rating-contract-pricing-readmodel` | §4.2 — `pin_frontier` + document reads; `PricingDocumentStore`. |
| `cpt-cf-bss-rating-contract-subscriptions-input` | §4.3 — facts, subscription versions, attribution, scopes, recovery reads. |
| `cpt-cf-bss-rating-contract-finance-fx-input` | §4.4 — no source; native currency (R-07). |
| `cpt-cf-bss-rating-contract-promotions-coupon` | §4.5 — no source; fail closed (R-11). |
| `cpt-cf-bss-rating-contract-billing-periodstate` | §4.6 — delivery, recovery reads, period hint. |
| `cpt-cf-bss-rating-contract-contracts-input` | §4.9 — no source; dormant (R-11). |
| `cpt-cf-bss-rating-fr-pre-purchase-evaluation` | §4.10 — `OrderEvaluationV1`. |
| `cpt-cf-bss-rating-fr-snapshot-carry` | §4.7. |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-resilience` | Every upstream read happens outside the rating transaction and is persisted idempotently; `Unavailable` retries; `PermissionDenied` is an exception. |
| `cpt-cf-bss-rating-nfr-throughput-latency` | Pinned documents are immutable per `catalog_version`; caches never need invalidation; attribution is a local projection. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-cc`

Adapters live in `infra/upstream/`, one module per dependency, each wrapping the dependency's SDK
trait resolved from ClientHub at gear `init`.

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-adopt-verbatim-cc`
  Enum values, key axes, band shapes and field names are taken exactly as pricing carries them
  (SEAMS P-9); an unknown value is `unknown_enum_value` / `unsupported_primitive`.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-single-writer-cc`
  Every boundary fact has one producing gear; Rating writes no foreign fact and only copies what it
  read, keyed by the producer's version id; tenant ids in payloads are data, not authority.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-frozen-in-precision-out-cc`
  Inputs enter evaluation as stored immutable copies; outputs leave as exact amounts with lineage.

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-inprocess-boundary-cc`
  Boundary reads are SDK calls through ClientHub; Rating reads no other gear's tables.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-extension-cc`
  Rating never extends an adopted enum or key axis locally.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-rating-contract-draft-cc`
  Upstream shapes in §4.2–§4.3 are Rating's requirements on gears that have not implemented them;
  Rating adapts to the shape the owner finally publishes and records any difference in SEAMS.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-cc`

| Boundary object | Producer | Rating copy |
|---|---|---|
| `PinFrontier { catalog_version, advanced_at }` | pricing | recorded as pin / pin-of-record |
| Plan / overlay document | pricing | `rating_catalog_document` |
| Subscription version | subscriptions | `rating_subscription_version` |
| Commercial fact (version) | subscriptions | `rating_fact` |
| Attribution segment, usage scope | subscriptions | `rating_attribution_segment`, `rating_usage_scope` |
| Usage record / invalidation | usage-collector | `rating_usage_record` |
| Coverage declaration | usage emitter (owner unassigned, overlay T6) | `rating_coverage_declaration` |
| Billing period hint | Billing | `rating_billing_hint` |
| `BillableItemDeliveryV1`, `RatingRunView`, `WindowResultView`, `OrderEvaluation`, `RatingSnapshot` | **rating** | served by the Rating SDK |

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-boundary-surface-cc`

| Component | Responsibility |
|---|---|
| `PricingDocumentStore` | `ensure(tenant, catalog_version, subjects)`: read `rating_catalog_document`; on miss call pricing, verify the hash, insert (`ON CONFLICT DO NOTHING`); immutable in-memory cache. |
| `SubscriptionVersionStore` | Same pattern for subscription versions. |
| `Inbox` + source adapters | Event consumers (TARGET) and pull sweeps feeding one inbox (§4.11). |
| `UsageFeedReader` | Slice 12. |
| `RatingRunClientLocal` | Implements the Rating SDK (§4.6, §4.10). |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-rating-handoff-cc`
  Intra-gear `rating-core` API — DESIGN §3.3 `interface-core-evaluate`.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-pricing-readmodel-cc`
  Pricing reads — §4.2.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-context-inputs-cc`
  Subscriptions reads and events — §4.3; FX, coupons — §4.4/§4.5 (no source).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-contracts-input-cc`
  Contracts — §4.9 (no source).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-order-evaluation-cc`
  `OrderEvaluationV1` — §4.10.

### 3.4 Internal Dependencies

The stores feed the `Rater` (slice 14), which passes stored documents to `rating-core`.

### 3.5 External Dependencies

See [`../SEAMS.md`](../SEAMS.md) §B–§G and §L.

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-evaluate-handoff-cc`
  **Ensure inputs for one evaluation** (outside the rating transaction):

  ```text
  pin  = (admin_rerate or no pin_of_record) ? pricing.pin_frontier(ctx) : child.pin_of_record
                                                              None → pending(pin_unavailable)
  for plan_id in plans referenced by the subscription version in the child's window:
      PricingDocumentStore.ensure(tenant, pin, Plan(plan_id))
  PricingDocumentStore.ensure(tenant, pin, Overlays)
  SubscriptionVersionStore.ensure(tenant, subscription_id, latest)
  → the rater opens its transaction with every input present locally
  ```

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-pricewindow-events-cc`
  **No catalog event consumption.** Window schedules, activations, expiries and cancellations are
  read from the plan document at the pin. Pricing's `CatalogEvent`s are written to `pricing_outbox`
  and never delivered (SEAMS A-3, P-7); `CatalogVersionPublished` is the registry's (D-66(1)) and
  unbuilt. Rating learns new versions only through `pin_frontier`.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-cc`
  Copies are defined in DESIGN §3.7; this slice adds no table.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-cc`
  Adapters resolve SDK clients at `init` and fail gear readiness if a mandatory client is not
  registered (pricing, usage-collector, subscriptions); in test and local-dev profiles the provider
  test doubles of §4.12 are registered instead.

## 4. Additional Context

### 4.1 Rating Handoff Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-rating-handoff-cc`

The core↔pipeline contract is in-process (DESIGN §3.3): the pipeline builds an `EvaluationInput`
only from stored copies and counters, calls `rating_core::evaluate`, and persists the outcome
(slice 15). Incompatible changes bump `engine_version`'s major component.

### 4.2 Pricing Read-Model Input Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-pricing-readmodel-cc`

Trait `bss_pricing_sdk::PricingCatalogClientV1` (exists with one method; the other two are the
proposed additions, J-2 / R-02):

| Method | Status | Contract |
|---|---|---|
| `pin_frontier(ctx) -> Result<Option<PinFrontier>, CanonicalError>` | declared, unimplemented, unregistered | tenant's pin-eligible frontier; `None` ⇒ nothing to pin |
| `plan_document(ctx, plan_id, catalog_version) -> Result<Option<PlanDocument>, CanonicalError>` | **proposed** | projected plan subject at the newest warm version ≤ `catalog_version` (`read_model_repo::delta_at`), plus `document_version`, `content_sha256` |
| `overlay_documents(ctx, catalog_version) -> Result<Vec<OverlayDocument>, CanonicalError>` | **proposed** | all overlay subjects visible to the tenant at that version |

Fields Rating reads (pricing `domain/projection.rs`, keys verbatim): plan `planId, revision, skuId,
planTier, billingCycle, frequency, phases, descriptorSet {invoiceLineTemplate, glCode},
periodFloorCaps, composites, usageCounterOnPlanChange, crossBoundaryChangePolicy,
evaluationPolicyVersion`, `windows[] {scopeKey, intervals[{effectiveFrom, effectiveTo, state}]}`;
price `priceId, scopeKey, lifecycleState, taxCategoryRef, resolvedTaxCategory, billingTiming,
billingAnchorPolicy, anchorDay, prorationBasis, creditOnDowngrade, roundingPolicyRef,
grandfatherUntil, supersedesPriceId`; row `chargeKind, modelKind, amountMinor, unitRateNanoMinor,
bands[{fromQty, toQty, unitPriceNanoMinor}], packageSize, packagePriceMinor, quantitySource,
manualQuantity, meter, dimensionKey, billingGranularity, tierAggregationWindow,
tierQualificationWindow, aggregationFunction, aggregationGranularity, maxHoldGranules,
includedAllowance, reservedRateNanoMinor, reservationFlavor, discountRef`. A required field absent
from the stored document is `missing_model_param`.

Guarantees relied on (SEAMS P-3): future-only window starts, immutable past windows and published
rows, future-only supersession — the basis of the pin rule (DESIGN §4.1).

**Atlas C01 (`PricingReadV1::{resolve, price, hold, acceptance}`, acceptance receipts,
`UsageRatingPolicy`) is not this repository's Pricing model — CONTRACT CONFLICT (R-17).** Its
hourly window semantics are already expressed by `tierAggregationWindow = per_hour`; its
aggregation scope is missing (P-13, R-23).

### 4.3 Subscriptions Input Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-subscriptions-input-cc`

**CURRENT** (documents only, R-03): `BillableItemCreated(kind ∈ {recurring, one_time})` (SEAMS S-2,
S-6); subscription read model (S-1). No attribution, scopes or recovery reads.

**TARGET** (Atlas C04, PROPOSED — NOT YET IMPLEMENTED), all events entering the inbox and all reads
usable as recovery:

| Contract | Rating use |
|---|---|
| `BillableFactPublished { fact: BillableFact { fact_id, fact_version, payload_digest, billing_group { billing_group_id, subscription_id, seller_tenant_id, payer_tenant_id, currency, billing_window }, resource_tenant_id, line_id, item_id, kind, timing, period?, occurrence_id?, quantity?, term_slices[{from, to, accepted_binding}], suspension_ranges[], suspension_billing, collection_paused, scope_ref?, segments_through_seq? } }` | fact intake (slice 14) |
| `BillableSetSealed` | not consumed by Rating (Billing's completeness) |
| `AttributionSegmentChanged { segment, segments_stream_seq }` | attribution projection (slice 13 §4.6) |
| `UsageScopeSealed { scope }` | evidence (slice 13 §4.7) |
| `SubscriptionBillingReadV1::period_facts(ctx, tenant_scope, overlapping_interval, cursor?)` | missing-fact recovery (watchdog) |
| `SubscriptionBillingReadV1::facts(ctx, billing_group_id, composition_version?)` | fact recovery by group |
| `SubscriptionBillingReadV1::segments_since(ctx, resource_tenant_id, after_seq, limit)` | segment gap fill / bootstrap |
| `SubscriptionBillingReadV1::scope_for_window(ctx, fact_id, fact_version, window, inventory_snapshot_id)` | ownership proof per due window |
| `SubscriptionBillingReadV1::bindings(ctx, subscription_id, period)` | price-binding cross-check (R-18) |

Mapping of the Atlas `accepted_binding` onto this repository: the binding's `price_id` must be a
price row of the pinned plan document; Rating's step-2 selection must return the same row
(`binding_mismatch` otherwise, R-18).

**MIGRATION**: the current `BillableItemCreated` maps onto `rating_fact` as in DESIGN Flow C; usage
parents are derived (R-20); attribution has no source (R-25).

Rating consumes but never decides `changeMode`, phases, period cuts, payer or attribution
(T-D-33).

### 4.4 Finance FX Input Contract (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-finance-fx-cc`

No FX contract exists that Rating can pin (SEAMS §F). Only billing currency = price currency is
supported; otherwise `fx_not_supported`. The required future contract is a pinnable rate snapshot
`{ rate_id, base, quote, rate, as_of, provider }` readable by id (J-8).

### 4.5 Promotions Coupon Snapshot Contract (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-promotions-coupons-cc`

No Promotions gear exists (SEAMS G-3). A price row with a non-null `discountRef` fails closed with
`coupon_source_unavailable`. Coupon semantics are in slice 06. External contract: UNKNOWN /
EXTERNAL CONTRACT REQUIRED.

### 4.6 Billing Delivery and Obligation Contract (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-billing-periodstate-cc`

Rating provides (DESIGN §3.3; slice 16): `BillableItemDeliveryV1` (event TARGET, pull
`deliveries_since` always), `RatingRunReadV1::{find_runs, get_run, window_results}`. Billing's
obligations (Atlas C08, J-5):

1. Accept deliveries per fact by `result_revision` (stale ignored, duplicate no-op, same revision
   with a different digest quarantined).
2. Sum exact contributions per `invoice_line_key` across the billing group; round once HALF_EVEN.
3. Before freeze replace the draft; after freeze create ordered credit/debit obligations from the
   rounded aggregate difference against the latest accepted target, including pending postings.
4. Execute `PeriodFloorCapObligation`s (R-22).
5. Post to `bss-ledger` with `pricing_snapshot_ref = snapshot_id`.

Billing may publish `BillingPeriodStateChanged`; Rating treats it as a hint (T-D-45).

### 4.7 pricingSnapshotRef Segment Map (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-snapshot-segments-cc`

The snapshot body and its sources are defined in DESIGN §4.1. The reference Billing passes to the
ledger is `snapshot_id` (`rsnap1:` + 64 hex); `RatingRunReadV1::get_run` and the operator API return
bodies for audit. No other gear writes a segment.

### 4.8 Canonical Naming (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-canonical-naming-cc`

"Rating" names the gear, "`rating-core`" the pure crate, "pipeline" the I/O half. "Tariff" means a
pricing-owned rate definition; in pricing's code and documents "Tariffs" refers to Rating's
evaluation core (historical, ADR-0002). "Fact" is a Subscriptions commercial fact; "child window" is
Rating's evaluation unit beneath it; "result" is an exact absolute outcome; "delivery" is its
publication. Subscriptions' `lineKey` is `sub_line_key`; Rating's `line_key` is a contribution
inside a fact (T-D-53). The Atlas "PriceBook" vocabulary is not used (R-17).

### 4.9 Contracts & Agreements Input Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-contracts-input-cc`

No Contracts implementation exists (SEAMS G-1, G-2; first-draft PRD). At launch step 5 receives no
contract overlays and step 6 no commitment pools; no `CommitmentBalanceEffect` is published. Target
when the gear exists: price-override windows per catalog scope key (contracts PRD `:296-307`) as
the step-5 overlay; pool set `{pool_id, unit, pool_type, balance, balance_version, draw_order,
rollover, overage_rate?}` read at a version and copied; effect protocol per DESIGN Flow F. Every
field of the pool set is UNKNOWN / EXTERNAL CONTRACT REQUIRED until Contracts specifies it.

### 4.10 Order Evaluation Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-order-evaluation-cc`

`OrderEvaluationV1::evaluate(ctx, OrderEvaluationRequest) -> Result<OrderEvaluation,
CanonicalError>` (Atlas C06, T-D-49), Rating-provided, consumed by Orders Lifecycle at submit and
amendment (`orders-lifecycle/docs/PRD.md:114,214`) and by change orders for delta totals.

- Request: `{evaluation_policy_version, tenant_axes, order_id, order_version, currency, lines:
  [{line_id, plan_id, price_ids[], quantity, term: Finite{periods} | Rolling, cycle: monthly |
  annual}]}`. The Atlas `acceptance_ref` has no equivalent in this repository's Pricing (R-17);
  Rating evaluates over the current pin and returns the `catalog_version` it used.
- Response: `{request_digest, catalog_version, currency, lines: [{line_id, recurring_per_cycle
  {gross, discount, net}, one_time {gross, discount, net}, net_pre_tax_tcv, usage_excluded,
  usage_rates[{price_id, unit, model_kind, bands?, window}], promotion_refs: []}], totals,
  tax_included: false, usage_excluded}`; all money `ExactAmount`.
- Rules: rolling monthly TCV = recurring net × 12 + one-time net; annual × 1; finite = periods ×
  recurring + one-time; missing term/cycle ⇒ `InvalidInput`; unsupported models, coupons, phases ⇒
  explicit errors; usage is shown as rates (incl. hourly band tables and window) and excluded from
  TCV — never a predicted consumption (Atlas F01, F10).
- Pure: no persistence, no idempotency key, no reservation, never a billing input.

### 4.11 Inbox and Recovery (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-inbox-recovery-cc`

- Every inbound fact passes `Inbox::accept(tx, source, business_id, version, digest, payload,
  via)`: new key → `accepted`; same key and digest → no-op; same key, different digest →
  `quarantined` + page (Atlas C00).
- Event consumers (TARGET) commit their offset in the same transaction as the inbox row
  (`LocalDbOffsetManager`). Unsupported schema versions are quarantined, never dropped.
- Recovery sweeps call the pull reads (§4.3) per tenant shard on a schedule (default 15 min for
  `period_facts`, on demand for `segments_since`); results enter the same inbox, so a re-read never
  doubles anything.
- Replay retention for any event topic must exceed the largest finalization delay plus the supported
  outage window (Atlas C09); pull reads cover anything older.

### 4.12 Provider Test Doubles (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-test-doubles-cc`

Until providers exist, Rating ships ClientHub-registered fakes behind a `test-providers` feature,
each driven by the shared fixture vectors (Atlas exit criterion: one provider-side and one
consumer-side test per contract with the same fixture ids):

| Fake | Implements | Fixtures |
|---|---|---|
| `FakePricingCatalog` | `PricingCatalogClientV1` + proposed document reads | pricing goldens, F23–F25 band tables |
| `FakeUsageFeed` | the collector's documented `read_usage_feed` (cursor, `Oldest`, `until`, invalidation entries, `CursorBeyondRetention`) | F02, F03, F06, F26, F30 |
| `FakeSubscriptionsBilling` | Atlas C04 events and reads | F04, F17, F18, F28, F33, F35 |
| `FakeCoverage` | coverage declarations / inventory | F05, F06, F30 |
| `RecordingBilling` | consumes `BillableItemDeliveryV1`, applies C08 rules | F14, F15, F27, F32 |

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-snapshot-carry`, `cpt-cf-bss-rating-fr-pre-purchase-evaluation`

- **PRD**: §9.2 (all integration contracts), §4.1.
- **DESIGN**: §3.3, §3.5, §4.1.
- **SEAMS**: §B–§G, §J, §L.
- **Decisions**: T-D-36, T-D-37, T-D-43, T-D-45, T-D-49, T-D-51, R-01…R-05, R-07, R-11, R-17, R-18,
  R-20, R-24, R-25.
